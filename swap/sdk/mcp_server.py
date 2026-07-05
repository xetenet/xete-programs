"""xete-swap MCP server — gives any MCP-enabled agent the ability to trade tokens
atomically on-chain through the xete-swap program.

Exposes swap as runtime-discoverable tools so an agent can: see its wallet + the fee
policy, list a token for another, fill a listing, place/accept/withdraw price bids, and
inspect any swap or bid. Built on the xete-swap SDK; mirrors the xete-mcp conventions.

Transport: stdio (local). Run: `python sdk/mcp_server.py` (needs the `mcp` package).

Config (env):
  XETE_SWAP_PROGRAM   the deployed xete-swap program id (required)
  XETE_RPC_URL        Solana RPC (default http://127.0.0.1:8899 — set to devnet/mainnet)
  XETE_SWAP_KEYPAIR   path to this agent's funded Solana keypair (JSON array) — required
                      to sign trades. Without it, only read-only tools work.

Listings are coordinated peer-to-peer: opening a swap returns its (maker, nonce); share
those with a counterparty (e.g. over xete messaging) so they can fill or bid. On-chain
browse-discovery is the next enhancement.
"""
from __future__ import annotations
import json, os
from pathlib import Path

from mcp.server.fastmcp import FastMCP
from solders.keypair import Keypair
from solders.pubkey import Pubkey

import sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from xete_swap import XeteSwap, ata

PROGRAM = os.environ.get("XETE_SWAP_PROGRAM", "")
RPC_URL = os.environ.get("XETE_RPC_URL", "http://127.0.0.1:8899")
KEYPAIR_PATH = os.environ.get("XETE_SWAP_KEYPAIR", "")

mcp = FastMCP("xete-swap")
_xs: XeteSwap | None = None
_kp: Keypair | None = None


def _client() -> XeteSwap:
    global _xs
    if _xs is None:
        if not PROGRAM:
            raise RuntimeError("set XETE_SWAP_PROGRAM to the deployed program id")
        _xs = XeteSwap(PROGRAM, RPC_URL)
    return _xs


def _signer() -> Keypair:
    global _kp
    if _kp is None:
        if not KEYPAIR_PATH or not Path(KEYPAIR_PATH).exists():
            raise RuntimeError("set XETE_SWAP_KEYPAIR to a funded Solana keypair file to trade")
        _kp = Keypair.from_bytes(bytes(json.loads(Path(KEYPAIR_PATH).read_text())))
    return _kp


def _pk(s) -> Pubkey:
    return Pubkey.from_string(s) if isinstance(s, str) else s


def _ok(d):
    return json.dumps(d, indent=2, default=str)


def _err(e):
    return json.dumps({"status": "failed", "error": str(e)[:300]})


@mcp.tool()
def swap_identity() -> str:
    """This agent's swap identity: wallet address, SOL balance, the swap program in use,
    and the live fee policy (settlement fee in basis points + delist fee). Other agents
    fill or bid on your listings using your wallet address + the listing nonce."""
    try:
        xs = _client()
        out = {"program": str(xs.program), "rpc": RPC_URL}
        try:
            kp = _signer()
            out["wallet"] = str(kp.pubkey())
            out["sol_balance"] = xs.rpc.get_balance(kp.pubkey()).value / 1e9
            out["can_trade"] = True
        except Exception as e:
            out["can_trade"] = False
            out["note"] = str(e)[:160]
        c = xs.get_config()
        out["fee"] = None if c is None else {"settlement_bps": c.fee_bps, "delist_fee_lamports": c.delist_fee,
                                             "fee_wallet": str(c.fee_wallet)}
        return _ok(out)
    except Exception as e:
        return _err(e)


@mcp.tool()
def swap_open_listing(give_mint: str, give_amount: int, want_mint: str, want_amount: int,
                      ttl_secs: int = 86400) -> str:
    """List tokens for sale: escrow `give_amount` (base units) of `give_mint` and ask
    `want_amount` of `want_mint`, atomically swappable until `ttl_secs` from now. Returns
    the listing's swap address + the (maker, nonce) a buyer needs to fill it. Amounts are
    in the token's smallest units (respect its decimals)."""
    try:
        xs, kp = _client(), _signer()
        nonce, sp, vp = xs.open_swap(kp, _pk(give_mint), give_amount, _pk(want_mint), want_amount, ttl_secs=ttl_secs)
        return _ok({"status": "listed", "swap": str(sp), "maker": str(kp.pubkey()),
                    "nonce": nonce.hex(), "give_mint": give_mint, "give_amount": give_amount,
                    "want_mint": want_mint, "want_amount": want_amount})
    except Exception as e:
        return _err(e)


@mcp.tool()
def swap_fill(maker: str, nonce_hex: str, give_mint: str, want_mint: str,
              expect_give: int, max_want: int) -> str:
    """Fill someone's listing: pay up to `max_want` of `want_mint` for `expect_give` of
    `give_mint`, atomically. `maker`+`nonce_hex` identify the listing (from the lister).
    The pin (`expect_give`, `max_want`) protects you if the listing changed — the trade
    reverts rather than overpay. You receive the goods, the maker receives your payment
    minus the protocol fee."""
    try:
        xs, kp = _client(), _signer()
        sig = xs.fill(kp, _pk(maker), bytes.fromhex(nonce_hex), _pk(give_mint), _pk(want_mint), expect_give, max_want)
        return _ok({"status": "filled", "signature": str(sig)})
    except Exception as e:
        return _err(e)


@mcp.tool()
def swap_cancel(nonce_hex: str, give_mint: str) -> str:
    """Cancel your own listing and reclaim the escrowed tokens. A small flat delist fee
    (SOL) applies. `nonce_hex` is the listing nonce from when you opened it."""
    try:
        xs, kp = _client(), _signer()
        sig = xs.cancel(kp, bytes.fromhex(nonce_hex), _pk(give_mint))
        return _ok({"status": "cancelled", "signature": str(sig)})
    except Exception as e:
        return _err(e)


@mcp.tool()
def swap_make_offer(swap_address: str, want_mint: str, bid_amount: int, ttl_secs: int = 86400) -> str:
    """Place a price bid on a listing: escrow `bid_amount` (base units) of `want_mint` as
    an offer on the swap at `swap_address`. The maker can accept your bid at your price.
    Returns the offer's (offerer, nonce) the maker needs to accept it. Reclaimable any
    time with swap_withdraw_offer."""
    try:
        xs, kp = _client(), _signer()
        nonce, offer, ovault = xs.make_offer(kp, _pk(swap_address), _pk(want_mint), bid_amount, ttl_secs=ttl_secs)
        return _ok({"status": "bid_placed", "offer": str(offer), "offerer": str(kp.pubkey()),
                    "nonce": nonce.hex(), "swap": swap_address, "bid_amount": bid_amount})
    except Exception as e:
        return _err(e)


@mcp.tool()
def swap_accept_offer(my_nonce_hex: str, offerer: str, offer_nonce_hex: str,
                      give_mint: str, want_mint: str) -> str:
    """Accept a bid on YOUR listing: settle your swap (identified by your `my_nonce_hex`)
    against the bid from `offerer`+`offer_nonce_hex`, atomically. The bidder gets your
    goods, you get their bid minus the protocol fee. Only the listing's maker can do this."""
    try:
        xs, kp = _client(), _signer()
        sig = xs.accept_offer(kp, bytes.fromhex(my_nonce_hex), _pk(offerer), bytes.fromhex(offer_nonce_hex),
                              _pk(give_mint), _pk(want_mint))
        return _ok({"status": "accepted", "signature": str(sig)})
    except Exception as e:
        return _err(e)


@mcp.tool()
def swap_withdraw_offer(nonce_hex: str, want_mint: str) -> str:
    """Withdraw your own bid and reclaim the escrowed funds. `nonce_hex` is the offer
    nonce from when you placed the bid."""
    try:
        xs, kp = _client(), _signer()
        sig = xs.withdraw_offer(kp, bytes.fromhex(nonce_hex), _pk(want_mint))
        return _ok({"status": "withdrawn", "signature": str(sig)})
    except Exception as e:
        return _err(e)


@mcp.tool()
def swap_get(swap_address: str) -> str:
    """Inspect a listing by its swap address: maker, the give/want mints + amounts, the
    private-taker (if any), expiry, and status. Returns null if it doesn't exist (closed)."""
    try:
        s = _client().get_swap(_pk(swap_address))
        if s is None:
            return _ok({"exists": False})
        return _ok({"exists": True, "maker": str(s.maker), "give_mint": str(s.give_mint),
                    "give_amount": s.give_amount, "want_mint": str(s.want_mint), "want_amount": s.want_amount,
                    "private_taker": (None if s.taker == Pubkey.default() else str(s.taker)),
                    "expiry": s.expiry})
    except Exception as e:
        return _err(e)


@mcp.tool()
def swap_get_offer(offer_address: str) -> str:
    """Inspect a bid by its offer address: offerer, which swap it targets, the bid amount,
    the goods it's pinned to buy, and expiry. Returns null if it doesn't exist."""
    try:
        o = _client().get_offer(_pk(offer_address))
        if o is None:
            return _ok({"exists": False})
        return _ok({"exists": True, "offerer": str(o.offerer), "swap": str(o.swap),
                    "bid_amount": o.want_amount, "buying_give_mint": str(o.give_mint),
                    "buying_give_amount": o.give_amount, "expiry": o.expiry})
    except Exception as e:
        return _err(e)


@mcp.tool()
def swap_browse(limit: int = 50) -> str:
    """Browse all OPEN listings on-chain. Returns each listing's swap address, maker, the
    give/want mints + amounts, and the (maker, nonce) needed to act on it — so you can find
    and trade listings without being handed them. Feed a listing into swap_fill or
    swap_make_offer next."""
    try:
        rows = [{"swap": str(a), "maker": str(s.maker), "nonce": s.nonce.hex(),
                 "give_mint": str(s.give_mint), "give_amount": s.give_amount,
                 "want_mint": str(s.want_mint), "want_amount": s.want_amount, "expiry": s.expiry}
                for a, s in _client().list_open_swaps(limit=limit)]
        return _ok({"count": len(rows), "listings": rows})
    except Exception as e:
        return _err(e)


@mcp.tool()
def swap_browse_bids(swap_address: str = "", limit: int = 50) -> str:
    """Browse OPEN bids. With a swap_address, only bids on that listing (useful for a maker
    choosing which bid to accept). Returns each bid's (offerer, nonce) and amount — pass
    those to swap_accept_offer."""
    try:
        rows = [{"offer": str(a), "offerer": str(o.offerer), "nonce": o.nonce.hex(),
                 "bid_amount": o.want_amount, "on_swap": str(o.swap), "expiry": o.expiry}
                for a, o in _client().list_open_offers(swap=(swap_address or None), limit=limit)]
        return _ok({"count": len(rows), "bids": rows})
    except Exception as e:
        return _err(e)


def main():
    mcp.run()


if __name__ == "__main__":
    main()
