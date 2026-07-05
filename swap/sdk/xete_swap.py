"""xete-swap SDK — Python client for the agent-to-agent swap program.

Wraps all instructions (open/fill/cancel/expire/make_offer/accept_offer/withdraw_offer/
expire_offer + init_config/update_config) with PDA/ATA derivation, send+confirm, state
decoders, and the protocol-fee plumbing. Built on solders + solana-py (xete-mcp stack).

  from xete_swap import XeteSwap
  xs = XeteSwap(PROGRAM_ID, rpc="http://127.0.0.1:8899")
  xs.init_config(admin_kp, fee_wallet_pubkey, fee_bps=30, delist_fee=1_000_000)   # once
  nonce, swap, vault = xs.open_swap(maker_kp, GIVE, 100_000_000, WANT, 250_000_000)
  xs.fill(taker_kp, maker_pubkey, nonce, GIVE, WANT, expect_give=100_000_000, max_want=250_000_000)

fill/accept/cancel auto-attach the Config + fee accounts (the fee wallet is read from the
on-chain Config and cached). The maker nets want_amount - fee; the fee lands in the fee
wallet's want-mint ATA.
"""
from __future__ import annotations
import struct, time
from dataclasses import dataclass
from solders.keypair import Keypair
from solders.pubkey import Pubkey
from solders.instruction import AccountMeta, Instruction
from solders.message import Message
from solders.transaction import Transaction
from solana.rpc.api import Client
from solana.rpc.commitment import Confirmed
from solana.rpc.types import TxOpts

SYS = Pubkey.from_string("11111111111111111111111111111111")
TOKEN = Pubkey.from_string("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA")
ATA_PROG = Pubkey.from_string("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL")
T2022 = Pubkey.from_string("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb")
MPL_TM = Pubkey.from_string("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s")
SYSVAR_IX = Pubkey.from_string("Sysvar1nstructions1111111111111111111111111")
OPEN_PNFT = 18
FILL_PNFT = 19
CANCEL_PNFT = 20
EXPIRE_PNFT = 21
MPL_CORE = Pubkey.from_string("CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d")
OPEN_CORE = 22
FILL_CORE = 23
CANCEL_CORE = 24
EXPIRE_CORE = 25
LIST = 26
FILL_LISTING = 27
CANCEL_LISTING = 28
EXPIRE_LISTING = 29
LIST_PNFT = 30
FILL_LISTING_PNFT = 31
CANCEL_LISTING_PNFT = 32
EXPIRE_LISTING_PNFT = 33
CB = Pubkey.from_string("ComputeBudget111111111111111111111111111111")

OPEN, FILL, CANCEL, EXPIRE, MAKE_OFFER, ACCEPT_OFFER, WITHDRAW_OFFER, EXPIRE_OFFER, INIT_CONFIG, UPDATE_CONFIG = range(10)
INIT_FEE_VAULT, SWEEP_FEES = 10, 11
STATUS_OPEN = 0
SWAP_LEN, OFFER_LEN, CONFIG_LEN = 220, 186, 108
BPS_DENOM = 10_000

S_MAKER, S_GIVE_MINT, S_GIVE_AMT, S_WANT_MINT, S_WANT_AMT = 0, 32, 64, 72, 104
S_TAKER, S_TERMS, S_EXPIRY, S_STATUS, S_BUMP, S_NONCE = 112, 144, 176, 184, 185, 186
O_OFFERER, O_SWAP, O_WANT_MINT, O_WANT_AMT, O_GIVE_MINT, O_GIVE_AMT, O_EXPIRY, O_STATUS, O_BUMP, O_NONCE = \
    0, 32, 64, 96, 104, 136, 144, 152, 153, 154
C_ADMIN, C_FEE_WALLET, C_FEE_BPS, C_DELIST_FEE, C_ALIAS_PROGRAM, C_FEE_SHARDS, C_BUMP = 0, 32, 64, 66, 74, 106, 107

def _u64(n): return struct.pack("<Q", n)
def _i64(n): return struct.pack("<q", n)
def _u16(n): return struct.pack("<H", n)
def _pk(b): return Pubkey.from_bytes(bytes(b))
def _amt(d, off): return int.from_bytes(d[off:off + 8], "little")
def cu_ix(units): return Instruction(CB, bytes([2]) + struct.pack("<I", units), [])
def new_nonce(): return bytes(Keypair().pubkey())
def ata(owner: Pubkey, mint: Pubkey, prog: Pubkey = TOKEN) -> Pubkey:
    return Pubkey.find_program_address([bytes(owner), bytes(prog), bytes(mint)], ATA_PROG)[0]
def M(pk, signer, writable): return AccountMeta(pk, signer, writable)

@dataclass
class Swap:
    maker: Pubkey; give_mint: Pubkey; give_amount: int; want_mint: Pubkey; want_amount: int
    taker: Pubkey; terms: bytes; expiry: int; status: int; bump: int; nonce: bytes

@dataclass
class Offer:
    offerer: Pubkey; swap: Pubkey; want_mint: Pubkey; want_amount: int
    give_mint: Pubkey; give_amount: int; expiry: int; status: int; bump: int; nonce: bytes

@dataclass
class Config:
    admin: Pubkey; fee_wallet: Pubkey; fee_bps: int; delist_fee: int; alias_program: Pubkey; fee_shards: int


class XeteSwap:
    def __init__(self, program_id, rpc="http://127.0.0.1:8899"):
        self.program = Pubkey.from_string(program_id) if isinstance(program_id, str) else program_id
        self.rpc = Client(rpc)
        self._fee_wallet = None
        self._fee_shards = None

    # ── PDAs ──
    def swap_pda(self, maker, nonce):    return Pubkey.find_program_address([b"swap",   bytes(maker), nonce], self.program)[0]
    def vault_pda(self, maker, nonce):   return Pubkey.find_program_address([b"vault",  bytes(maker), nonce], self.program)[0]
    def offer_pda(self, offerer, nonce): return Pubkey.find_program_address([b"offer",  bytes(offerer), nonce], self.program)[0]
    def ovault_pda(self, offerer, nonce):return Pubkey.find_program_address([b"ovault", bytes(offerer), nonce], self.program)[0]
    def config_pda(self):                return Pubkey.find_program_address([b"config"], self.program)[0]
    def fee_vault_pda(self, want_mint, idx): return Pubkey.find_program_address([b"fee_vault", bytes(want_mint), bytes([idx])], self.program)[0]
    # Token-Metadata PDAs (for pNFT escrow)
    def metadata_pda(self, mint):     return Pubkey.find_program_address([b"metadata", bytes(MPL_TM), bytes(mint)], MPL_TM)[0]
    def edition_pda(self, mint):      return Pubkey.find_program_address([b"metadata", bytes(MPL_TM), bytes(mint), b"edition"], MPL_TM)[0]
    def token_record_pda(self, mint, ta): return Pubkey.find_program_address([b"metadata", bytes(MPL_TM), bytes(mint), b"token_record", bytes(ta)], MPL_TM)[0]

    # ── send + confirm ──
    def send(self, signers, ixs, payer):
        bh = self.rpc.get_latest_blockhash().value.blockhash
        tx = Transaction(signers, Message.new_with_blockhash(ixs, payer.pubkey(), bh), bh)
        sig = self.rpc.send_transaction(tx, opts=TxOpts(skip_preflight=False, preflight_commitment=Confirmed)).value
        for _ in range(80):
            time.sleep(0.3)
            st = self.rpc.get_signature_statuses([sig]).value[0]
            if not st:
                continue
            if st.err:
                raise RuntimeError(f"tx rejected: {st.err}")
            if str(st.confirmation_status).split(".")[-1].lower() in ("confirmed", "finalized"):
                return sig
        raise TimeoutError("confirmation timed out")

    # ── config (fee policy) ──
    def init_config_ix(self, admin, fee_wallet, fee_bps, delist_fee, alias_program, fee_shards=0):
        data = bytes([INIT_CONFIG]) + bytes(fee_wallet) + _u16(fee_bps) + _u64(delist_fee) + bytes(alias_program) + bytes([fee_shards])
        return Instruction(self.program, data, [M(admin, True, True), M(self.config_pda(), False, True), M(SYS, False, False)])

    def update_config_ix(self, admin, fee_wallet, fee_bps, delist_fee, alias_program, fee_shards=0):
        data = bytes([UPDATE_CONFIG]) + bytes(fee_wallet) + _u16(fee_bps) + _u64(delist_fee) + bytes(alias_program) + bytes([fee_shards])
        return Instruction(self.program, data, [M(admin, True, True), M(self.config_pda(), False, True)])

    def init_config(self, admin, fee_wallet, fee_bps, delist_fee, alias_program=SYS, fee_shards=0):
        self._fee_wallet = self._fee_shards = None
        return self.send([admin], [self.init_config_ix(admin.pubkey(), fee_wallet, fee_bps, delist_fee, alias_program, fee_shards)], admin)

    def update_config(self, admin, fee_wallet, fee_bps, delist_fee, alias_program=SYS, fee_shards=0):
        self._fee_wallet = self._fee_shards = None
        return self.send([admin], [self.update_config_ix(admin.pubkey(), fee_wallet, fee_bps, delist_fee, alias_program, fee_shards)], admin)

    def fee_wallet(self):
        if self._fee_wallet is None:
            c = self.get_config()
            self._fee_wallet = c.fee_wallet if c else None
        return self._fee_wallet

    def fee_shards(self):
        if self._fee_shards is None:
            c = self.get_config()
            self._fee_shards = c.fee_shards if c else 0
        return self._fee_shards

    def fee_dest(self, want_mint, nonce, fee_wallet, fee_shards):
        """Where the protocol fee lands: the fee wallet's want-ATA (sharding off) or the
        derived shard vault keyed by nonce[0] % fee_shards (sharding on)."""
        if fee_shards and fee_shards > 0:
            return self.fee_vault_pda(want_mint, nonce[0] % fee_shards)
        return ata(fee_wallet, want_mint)

    # ── fee-vault sharding (dormant unless config.fee_shards > 0) ──
    def init_fee_vault_ix(self, payer, want_mint, idx):
        return Instruction(self.program, bytes([INIT_FEE_VAULT, idx]), [
            M(payer, True, True), M(self.fee_vault_pda(want_mint, idx), False, True), M(want_mint, False, False),
            M(TOKEN, False, False), M(SYS, False, False)])

    def sweep_fees_ix(self, caller, want_mint, fee_wallet, idx):
        return Instruction(self.program, bytes([SWEEP_FEES, idx]), [
            M(caller, True, True), M(self.fee_vault_pda(want_mint, idx), False, True), M(want_mint, False, False),
            M(self.config_pda(), False, False), M(ata(fee_wallet, want_mint), False, True), M(TOKEN, False, False)])

    def init_fee_vault(self, payer, want_mint, idx):
        return self.send([payer], [self.init_fee_vault_ix(payer.pubkey(), want_mint, idx)], payer)

    def sweep_fees(self, caller, want_mint, idx, fee_wallet=None):
        fee_wallet = fee_wallet or self.fee_wallet()
        return self.send([caller], [self.sweep_fees_ix(caller.pubkey(), want_mint, fee_wallet, idx)], caller)

    # ── instruction builders (faithful to the deployed program) ──
    def open_ix(self, maker, nonce, give_mint, give_amount, want_mint, want_amount, expiry,
                terms=bytes(32), taker=bytes(32), alias=SYS, token_prog=TOKEN):
        sp, vp = self.swap_pda(maker, nonce), self.vault_pda(maker, nonce)
        data = bytes([OPEN]) + nonce + _u64(give_amount) + _u64(want_amount) + terms + _i64(expiry) + taker
        ix = Instruction(self.program, data, [
            M(maker, True, True), M(sp, False, True), M(vp, False, True), M(give_mint, False, False),
            M(want_mint, False, False), M(ata(maker, give_mint, token_prog), False, True), M(token_prog, False, False), M(SYS, False, False),
            M(self.config_pda(), False, False), M(alias, False, False)])
        return sp, vp, ix

    def open_pnft_ix(self, maker, nonce, give_mint, want_mint, want_amount, expiry,
                     terms=bytes(32), taker=bytes(32), alias=SYS, rules_prog=MPL_TM, rules=MPL_TM):
        """Escrow a programmable NFT (classic-Token mint) into the swap PDA's ATA via Token-Metadata Transfer."""
        sp = self.swap_pda(maker, nonce)
        src = ata(maker, give_mint)         # maker's ATA (classic SPL Token)
        vault = ata(sp, give_mint)          # swap PDA's ATA = the escrow vault (created by the CPI)
        md, ed = self.metadata_pda(give_mint), self.edition_pda(give_mint)
        orec, drec = self.token_record_pda(give_mint, src), self.token_record_pda(give_mint, vault)
        data = bytes([OPEN_PNFT]) + nonce + _u64(1) + _u64(want_amount) + terms + _i64(expiry) + taker
        ix = Instruction(self.program, data, [
            M(maker, True, True), M(sp, False, True), M(give_mint, False, False), M(want_mint, False, False),
            M(src, False, True), M(vault, False, True), M(md, False, True), M(ed, False, False),
            M(orec, False, True), M(drec, False, True), M(MPL_TM, False, False), M(TOKEN, False, False),
            M(ATA_PROG, False, False), M(SYSVAR_IX, False, False), M(SYS, False, False),
            M(rules_prog, False, False), M(rules, False, False), M(self.config_pda(), False, False), M(alias, False, False)])
        return sp, vault, ix

    def open_pnft(self, maker, give_mint, want_mint, want_amount, ttl_secs=3600,
                  terms=bytes(32), taker=bytes(32), nonce=None, alias=SYS, rules_prog=MPL_TM, rules=MPL_TM):
        nonce = nonce or new_nonce()
        sp, vault, ix = self.open_pnft_ix(maker.pubkey(), nonce, give_mint, want_mint, want_amount,
                                          int(time.time()) + ttl_secs, terms, taker, alias, rules_prog, rules)
        # Token-Metadata Transfer is CPI-heavy (creates dest token-record); lift the compute budget
        self.send([maker], [cu_ix(600_000), ix], maker)
        return nonce, sp, vault

    def fill_pnft_ix(self, taker, maker, nonce, give_mint, want_mint, expect_give, max_want,
                     fee_wallet, fee_shards=0, want_token_prog=TOKEN, rules_prog=MPL_TM, rules=MPL_TM):
        sp = self.swap_pda(maker, nonce)
        vault = ata(sp, give_mint); tgive = ata(taker, give_mint)
        md, ed = self.metadata_pda(give_mint), self.edition_pda(give_mint)
        orec, drec = self.token_record_pda(give_mint, vault), self.token_record_pda(give_mint, tgive)
        data = bytes([FILL_PNFT]) + nonce + _u64(expect_give) + _u64(max_want)
        return Instruction(self.program, data, [
            M(taker, True, True), M(sp, False, True), M(maker, False, True), M(give_mint, False, False),
            M(want_mint, False, False), M(ata(taker, want_mint), False, True), M(ata(maker, want_mint), False, True),
            M(want_token_prog, False, False), M(self.config_pda(), False, False),
            M(self.fee_dest(want_mint, nonce, fee_wallet, fee_shards), False, True),
            M(vault, False, True), M(tgive, False, True), M(md, False, True), M(ed, False, False),
            M(orec, False, True), M(drec, False, True), M(MPL_TM, False, False), M(TOKEN, False, False),
            M(ATA_PROG, False, False), M(SYSVAR_IX, False, False), M(SYS, False, False),
            M(rules_prog, False, False), M(rules, False, False)])

    def cancel_pnft_ix(self, maker, nonce, give_mint, fee_wallet, rules_prog=MPL_TM, rules=MPL_TM):
        sp = self.swap_pda(maker, nonce)
        vault = ata(sp, give_mint); mgive = ata(maker, give_mint)
        md, ed = self.metadata_pda(give_mint), self.edition_pda(give_mint)
        orec, drec = self.token_record_pda(give_mint, vault), self.token_record_pda(give_mint, mgive)
        return Instruction(self.program, bytes([CANCEL_PNFT]) + nonce, [
            M(maker, True, True), M(sp, False, True), M(give_mint, False, False), M(mgive, False, True),
            M(vault, False, True), M(self.config_pda(), False, False), M(fee_wallet, False, True), M(SYS, False, False),
            M(md, False, True), M(ed, False, False), M(orec, False, True), M(drec, False, True),
            M(MPL_TM, False, False), M(TOKEN, False, False), M(ATA_PROG, False, False), M(SYSVAR_IX, False, False),
            M(rules_prog, False, False), M(rules, False, False)])

    def expire_pnft_ix(self, caller, maker, nonce, give_mint, rules_prog=MPL_TM, rules=MPL_TM):
        sp = self.swap_pda(maker, nonce)
        vault = ata(sp, give_mint); mgive = ata(maker, give_mint)
        md, ed = self.metadata_pda(give_mint), self.edition_pda(give_mint)
        orec, drec = self.token_record_pda(give_mint, vault), self.token_record_pda(give_mint, mgive)
        return Instruction(self.program, bytes([EXPIRE_PNFT]) + nonce, [
            M(caller, True, True), M(sp, False, True), M(maker, False, True), M(give_mint, False, False),
            M(mgive, False, True), M(vault, False, True), M(md, False, True), M(ed, False, False),
            M(orec, False, True), M(drec, False, True), M(MPL_TM, False, False), M(TOKEN, False, False),
            M(ATA_PROG, False, False), M(SYSVAR_IX, False, False), M(SYS, False, False),
            M(rules_prog, False, False), M(rules, False, False)])

    def fill_pnft(self, taker, maker, nonce, give_mint, want_mint, expect_give, max_want, fee_wallet=None, fee_shards=None):
        fee_wallet = fee_wallet or self.fee_wallet(); fee_shards = self.fee_shards() if fee_shards is None else fee_shards
        ix = self.fill_pnft_ix(taker.pubkey(), maker, nonce, give_mint, want_mint, expect_give, max_want, fee_wallet, fee_shards)
        return self.send([taker], [cu_ix(600_000), ix], taker)

    def cancel_pnft(self, maker, nonce, give_mint, fee_wallet=None):
        fee_wallet = fee_wallet or self.fee_wallet()
        return self.send([maker], [cu_ix(600_000), self.cancel_pnft_ix(maker.pubkey(), nonce, give_mint, fee_wallet)], maker)

    def expire_pnft(self, caller, maker, nonce, give_mint):
        return self.send([caller], [cu_ix(600_000), self.expire_pnft_ix(caller.pubkey(), maker, nonce, give_mint)], caller)

    def open_core_ix(self, maker, nonce, asset, want_mint, want_amount, expiry,
                     terms=bytes(32), taker=bytes(32), alias=SYS, collection=None):
        """Escrow a Metaplex Core asset by transferring its ownership to the swap PDA (no vault account)."""
        sp = self.swap_pda(maker, nonce)
        coll = collection or MPL_CORE  # None -> sentinel (bare asset)
        data = bytes([OPEN_CORE]) + nonce + _u64(1) + _u64(want_amount) + terms + _i64(expiry) + taker
        ix = Instruction(self.program, data, [
            M(maker, True, True), M(sp, False, True), M(asset, False, True), M(coll, False, False),
            M(want_mint, False, False), M(MPL_CORE, False, False), M(SYS, False, False),
            M(self.config_pda(), False, False), M(alias, False, False)])
        return sp, ix

    def fill_core_ix(self, taker, maker, nonce, asset, want_mint, expect_give, max_want,
                     fee_wallet, fee_shards=0, collection=None, want_token_prog=TOKEN):
        sp = self.swap_pda(maker, nonce); coll = collection or MPL_CORE
        data = bytes([FILL_CORE]) + nonce + _u64(expect_give) + _u64(max_want)
        return Instruction(self.program, data, [
            M(taker, True, True), M(sp, False, True), M(maker, False, True), M(asset, False, True),
            M(coll, False, False), M(want_mint, False, False), M(ata(taker, want_mint), False, True),
            M(ata(maker, want_mint), False, True), M(want_token_prog, False, False), M(self.config_pda(), False, False),
            M(self.fee_dest(want_mint, nonce, fee_wallet, fee_shards), False, True), M(MPL_CORE, False, False)])

    def cancel_core_ix(self, maker, nonce, asset, fee_wallet, collection=None):
        sp = self.swap_pda(maker, nonce); coll = collection or MPL_CORE
        return Instruction(self.program, bytes([CANCEL_CORE]) + nonce, [
            M(maker, True, True), M(sp, False, True), M(asset, False, True), M(coll, False, False),
            M(self.config_pda(), False, False), M(fee_wallet, False, True), M(SYS, False, False), M(MPL_CORE, False, False)])

    def expire_core_ix(self, caller, maker, nonce, asset, collection=None):
        sp = self.swap_pda(maker, nonce); coll = collection or MPL_CORE
        return Instruction(self.program, bytes([EXPIRE_CORE]) + nonce, [
            M(caller, True, True), M(sp, False, True), M(maker, False, True), M(asset, False, True),
            M(coll, False, False), M(MPL_CORE, False, False)])

    def open_core(self, maker, asset, want_mint, want_amount, ttl_secs=3600,
                  terms=bytes(32), taker=bytes(32), nonce=None, alias=SYS, collection=None):
        nonce = nonce or new_nonce()
        sp, ix = self.open_core_ix(maker.pubkey(), nonce, asset, want_mint, want_amount,
                                   int(time.time()) + ttl_secs, terms, taker, alias, collection)
        self.send([maker], [cu_ix(400_000), ix], maker)
        return nonce, sp

    def fill_core(self, taker, maker, nonce, asset, want_mint, expect_give, max_want, fee_wallet=None, fee_shards=None, collection=None):
        fee_wallet = fee_wallet or self.fee_wallet(); fee_shards = self.fee_shards() if fee_shards is None else fee_shards
        ix = self.fill_core_ix(taker.pubkey(), maker, nonce, asset, want_mint, expect_give, max_want, fee_wallet, fee_shards, collection)
        return self.send([taker], [cu_ix(400_000), ix], taker)

    def cancel_core(self, maker, nonce, asset, fee_wallet=None, collection=None):
        fee_wallet = fee_wallet or self.fee_wallet()
        return self.send([maker], [cu_ix(400_000), self.cancel_core_ix(maker.pubkey(), nonce, asset, fee_wallet, collection)], maker)

    def expire_core(self, caller, maker, nonce, asset, collection=None):
        return self.send([caller], [cu_ix(400_000), self.expire_core_ix(caller.pubkey(), maker, nonce, asset, collection)], caller)

    # -- escrowless / delegate-based listings (the NFT stays in the maker's wallet) --
    def listing_pda(self, maker, nonce): return Pubkey.find_program_address([b"listing", bytes(maker), nonce], self.program)[0]

    def list_ix(self, maker, nonce, give_mint, give_amount, want_mint, want_amount, expiry,
                terms=bytes(32), taker=bytes(32), alias=SYS, token_prog=TOKEN, royalty_mode=0, maker_pct=0):
        lp = self.listing_pda(maker, nonce)
        data = bytes([LIST]) + nonce + _u64(give_amount) + _u64(want_amount) + terms + _i64(expiry) + taker + bytes([royalty_mode, maker_pct])
        ix = Instruction(self.program, data, [
            M(maker, True, True), M(lp, False, True), M(give_mint, False, False), M(want_mint, False, False),
            M(ata(maker, give_mint, token_prog), False, True), M(token_prog, False, False), M(SYS, False, False),
            M(self.config_pda(), False, False), M(alias, False, False)])
        return lp, ix

    def fill_listing_ix(self, taker, maker, nonce, give_mint, want_mint, expect_give, max_want,
                        fee_wallet, fee_shards=0, give_token_prog=TOKEN, want_token_prog=TOKEN,
                        metadata=None, creator_atas=None):
        lp = self.listing_pda(maker, nonce)
        data = bytes([FILL_LISTING]) + nonce + _u64(expect_give) + _u64(max_want)
        accts = [
            M(taker, True, True), M(lp, False, True), M(maker, False, True), M(give_mint, False, False),
            M(want_mint, False, False), M(ata(maker, give_mint, give_token_prog), False, True),
            M(ata(taker, give_mint, give_token_prog), False, True), M(ata(taker, want_mint), False, True),
            M(ata(maker, want_mint), False, True), M(want_token_prog, False, False), M(self.config_pda(), False, False),
            M(self.fee_dest(want_mint, nonce, fee_wallet, fee_shards), False, True), M(give_token_prog, False, False)]
        if metadata is not None:  # royalty fill: metadata(13) + creator want-ATAs(14..)
            accts.append(M(metadata, False, False))
            for ca in (creator_atas or []):
                accts.append(M(ca, False, True))
        return Instruction(self.program, data, accts)

    def cancel_listing_ix(self, maker, nonce, give_mint, fee_wallet, token_prog=TOKEN):
        lp = self.listing_pda(maker, nonce)
        return Instruction(self.program, bytes([CANCEL_LISTING]) + nonce, [
            M(maker, True, True), M(lp, False, True), M(give_mint, False, False),
            M(ata(maker, give_mint, token_prog), False, True), M(token_prog, False, False),
            M(self.config_pda(), False, False), M(fee_wallet, False, True), M(SYS, False, False)])

    def expire_listing_ix(self, caller, maker, nonce, give_mint):
        lp = self.listing_pda(maker, nonce)
        return Instruction(self.program, bytes([EXPIRE_LISTING]) + nonce, [
            M(caller, True, True), M(lp, False, True), M(maker, False, True)])

    def do_list(self, maker, give_mint, give_amount, want_mint, want_amount, ttl_secs=3600,
                terms=bytes(32), taker=bytes(32), nonce=None, alias=SYS, token_prog=TOKEN):
        nonce = nonce or new_nonce()
        lp, ix = self.list_ix(maker.pubkey(), nonce, give_mint, give_amount, want_mint, want_amount,
                              int(time.time()) + ttl_secs, terms, taker, alias, token_prog)
        self.send([maker], [ix], maker)
        return nonce, lp

    def ata_create_idem_ix(self, payer, owner, mint, token_prog=TOKEN):
        """Associated-token-account CreateIdempotent (data=[1]) — the taker makes their own destination ATA."""
        return Instruction(ATA_PROG, bytes([1]), [
            M(payer, True, True), M(ata(owner, mint, token_prog), False, True), M(owner, False, False),
            M(mint, False, False), M(SYS, False, False), M(token_prog, False, False)])

    def take_listing(self, taker, maker, nonce, give_mint, want_mint, expect_give, max_want,
                     fee_wallet=None, fee_shards=None, give_token_prog=TOKEN):
        fee_wallet = fee_wallet or self.fee_wallet(); fee_shards = self.fee_shards() if fee_shards is None else fee_shards
        mk_ata = self.ata_create_idem_ix(taker.pubkey(), taker.pubkey(), give_mint, give_token_prog)
        ix = self.fill_listing_ix(taker.pubkey(), maker, nonce, give_mint, want_mint, expect_give, max_want, fee_wallet, fee_shards, give_token_prog)
        return self.send([taker], [cu_ix(400_000), mk_ata, ix], taker)

    def cancel_listing(self, maker, nonce, give_mint, fee_wallet=None, token_prog=TOKEN):
        fee_wallet = fee_wallet or self.fee_wallet()
        return self.send([maker], [self.cancel_listing_ix(maker.pubkey(), nonce, give_mint, fee_wallet, token_prog)], maker)

    def expire_listing(self, caller, maker, nonce, give_mint):
        return self.send([caller], [self.expire_listing_ix(caller.pubkey(), maker, nonce, give_mint)], caller)

    def list_pnft_ix(self, maker, nonce, give_mint, want_mint, want_amount, expiry,
                     terms=bytes(32), taker=bytes(32), alias=SYS, rules_prog=MPL_TM, rules=MPL_TM,
                     royalty_mode=0, maker_pct=0):
        lp = self.listing_pda(maker, nonce); oa = ata(maker, give_mint)
        tr = self.token_record_pda(give_mint, oa); md, ed = self.metadata_pda(give_mint), self.edition_pda(give_mint)
        data = bytes([LIST_PNFT]) + nonce + _u64(1) + _u64(want_amount) + terms + _i64(expiry) + taker + bytes([royalty_mode, maker_pct])
        ix = Instruction(self.program, data, [
            M(maker, True, True), M(lp, False, True), M(give_mint, False, False), M(want_mint, False, False),
            M(oa, False, True), M(tr, False, True), M(md, False, True), M(ed, False, False), M(MPL_TM, False, False),
            M(TOKEN, False, False), M(SYSVAR_IX, False, False), M(SYS, False, False),
            M(rules_prog, False, False), M(rules, False, False), M(self.config_pda(), False, False), M(alias, False, False)])
        return lp, ix

    def fill_listing_pnft_ix(self, taker, maker, nonce, give_mint, want_mint, expect_give, max_want,
                             fee_wallet, fee_shards=0, rules_prog=MPL_TM, rules=MPL_TM, want_token_prog=TOKEN,
                             creator_atas=None):
        lp = self.listing_pda(maker, nonce); ma = ata(maker, give_mint); tg = ata(taker, give_mint)
        md, ed = self.metadata_pda(give_mint), self.edition_pda(give_mint)
        orec, drec = self.token_record_pda(give_mint, ma), self.token_record_pda(give_mint, tg)
        data = bytes([FILL_LISTING_PNFT]) + nonce + _u64(expect_give) + _u64(max_want)
        # 23 fixed accounts; the metadata (acct 12) is reused for royalty parsing. Royalty fills append the
        # creator want-ATAs after the 23 fixed accounts (the contract reads them at accounts[23..]).
        accts = [
            M(taker, True, True), M(lp, False, True), M(maker, False, True), M(give_mint, False, False),
            M(want_mint, False, False), M(ata(taker, want_mint), False, True), M(ata(maker, want_mint), False, True),
            M(want_token_prog, False, False), M(self.config_pda(), False, False),
            M(self.fee_dest(want_mint, nonce, fee_wallet, fee_shards), False, True),
            M(ma, False, True), M(tg, False, True), M(md, False, True), M(ed, False, False),
            M(orec, False, True), M(drec, False, True), M(MPL_TM, False, False), M(TOKEN, False, False),
            M(ATA_PROG, False, False), M(SYSVAR_IX, False, False), M(SYS, False, False),
            M(rules_prog, False, False), M(rules, False, False)]
        for ca in (creator_atas or []):
            accts.append(M(ca, False, True))
        return Instruction(self.program, data, accts)

    def cancel_listing_pnft_ix(self, maker, nonce, give_mint, fee_wallet, rules_prog=MPL_TM, rules=MPL_TM):
        lp = self.listing_pda(maker, nonce); oa = ata(maker, give_mint)
        tr = self.token_record_pda(give_mint, oa); md, ed = self.metadata_pda(give_mint), self.edition_pda(give_mint)
        return Instruction(self.program, bytes([CANCEL_LISTING_PNFT]) + nonce, [
            M(maker, True, True), M(lp, False, True), M(give_mint, False, False), M(oa, False, True),
            M(tr, False, True), M(md, False, True), M(ed, False, False), M(MPL_TM, False, False),
            M(TOKEN, False, False), M(SYSVAR_IX, False, False), M(SYS, False, False),
            M(rules_prog, False, False), M(rules, False, False), M(self.config_pda(), False, False), M(fee_wallet, False, True)])

    def expire_listing_pnft_ix(self, caller, maker, nonce, give_mint):
        lp = self.listing_pda(maker, nonce)
        return Instruction(self.program, bytes([EXPIRE_LISTING_PNFT]) + nonce, [
            M(caller, True, True), M(lp, False, True), M(maker, False, True)])

    def do_list_pnft(self, maker, give_mint, want_mint, want_amount, ttl_secs=3600,
                     terms=bytes(32), taker=bytes(32), nonce=None, alias=SYS, rules_prog=MPL_TM, rules=MPL_TM,
                     royalty_mode=0, maker_pct=0):
        nonce = nonce or new_nonce()
        lp, ix = self.list_pnft_ix(maker.pubkey(), nonce, give_mint, want_mint, want_amount,
                                   int(time.time()) + ttl_secs, terms, taker, alias, rules_prog, rules,
                                   royalty_mode, maker_pct)
        self.send([maker], [cu_ix(400_000), ix], maker)
        return nonce, lp

    def take_listing_pnft(self, taker, maker, nonce, give_mint, want_mint, expect_give, max_want, fee_wallet=None, fee_shards=None, rules_prog=MPL_TM, rules=MPL_TM):
        fee_wallet = fee_wallet or self.fee_wallet(); fee_shards = self.fee_shards() if fee_shards is None else fee_shards
        ix = self.fill_listing_pnft_ix(taker.pubkey(), maker, nonce, give_mint, want_mint, expect_give, max_want, fee_wallet, fee_shards, rules_prog, rules)
        return self.send([taker], [cu_ix(600_000), ix], taker)

    def cancel_listing_pnft(self, maker, nonce, give_mint, fee_wallet=None, rules_prog=MPL_TM, rules=MPL_TM):
        fee_wallet = fee_wallet or self.fee_wallet()
        return self.send([maker], [cu_ix(400_000), self.cancel_listing_pnft_ix(maker.pubkey(), nonce, give_mint, fee_wallet, rules_prog, rules)], maker)

    def expire_listing_pnft(self, caller, maker, nonce, give_mint):
        return self.send([caller], [self.expire_listing_pnft_ix(caller.pubkey(), maker, nonce, give_mint)], caller)

    def fill_ix(self, taker, maker, nonce, give_mint, want_mint, expect_give, max_want, fee_wallet, fee_shards=0, give_token_prog=TOKEN):
        sp, vp = self.swap_pda(maker, nonce), self.vault_pda(maker, nonce)
        data = bytes([FILL]) + nonce + _u64(expect_give) + _u64(max_want)
        return Instruction(self.program, data, [
            M(taker, True, True), M(sp, False, True), M(vp, False, True), M(maker, False, True),
            M(give_mint, False, False), M(want_mint, False, False), M(ata(taker, want_mint), False, True),
            M(ata(taker, give_mint, give_token_prog), False, True), M(ata(maker, want_mint), False, True), M(TOKEN, False, False),
            M(self.config_pda(), False, False), M(self.fee_dest(want_mint, nonce, fee_wallet, fee_shards), False, True), M(give_token_prog, False, False)])

    def cancel_ix(self, maker, nonce, give_mint, fee_wallet, token_prog=TOKEN):
        sp, vp = self.swap_pda(maker, nonce), self.vault_pda(maker, nonce)
        return Instruction(self.program, bytes([CANCEL]) + nonce, [
            M(maker, True, True), M(sp, False, True), M(vp, False, True), M(give_mint, False, False),
            M(ata(maker, give_mint, token_prog), False, True), M(token_prog, False, False),
            M(self.config_pda(), False, False), M(fee_wallet, False, True), M(SYS, False, False)])

    def expire_ix(self, caller, maker, nonce, give_mint):
        sp, vp = self.swap_pda(maker, nonce), self.vault_pda(maker, nonce)
        return Instruction(self.program, bytes([EXPIRE]) + nonce, [
            M(caller, True, True), M(sp, False, True), M(vp, False, True), M(maker, False, True),
            M(give_mint, False, False), M(ata(maker, give_mint), False, True), M(TOKEN, False, False)])

    def make_offer_ix(self, offerer, nonce, swap_pk, want_mint, want_amount, expiry, alias=SYS):
        offer, ovault = self.offer_pda(offerer, nonce), self.ovault_pda(offerer, nonce)
        data = bytes([MAKE_OFFER]) + nonce + _u64(want_amount) + _i64(expiry)
        return offer, ovault, Instruction(self.program, data, [
            M(offerer, True, True), M(offer, False, True), M(ovault, False, True), M(want_mint, False, False),
            M(swap_pk, False, False), M(ata(offerer, want_mint), False, True), M(TOKEN, False, False), M(SYS, False, False),
            M(self.config_pda(), False, False), M(alias, False, False)])

    def accept_offer_ix(self, maker, swap_nonce, offerer, offer_nonce, give_mint, want_mint, fee_wallet, fee_shards=0):
        sp, vp = self.swap_pda(maker, swap_nonce), self.vault_pda(maker, swap_nonce)
        offer, ovault = self.offer_pda(offerer, offer_nonce), self.ovault_pda(offerer, offer_nonce)
        data = bytes([ACCEPT_OFFER]) + swap_nonce + offer_nonce
        return Instruction(self.program, data, [
            M(maker, True, True), M(sp, False, True), M(vp, False, True), M(offer, False, True), M(ovault, False, True),
            M(offerer, False, True), M(give_mint, False, False), M(want_mint, False, False),
            M(ata(offerer, give_mint), False, True), M(ata(maker, want_mint), False, True), M(TOKEN, False, False),
            M(self.config_pda(), False, False), M(self.fee_dest(want_mint, swap_nonce, fee_wallet, fee_shards), False, True)])

    def withdraw_offer_ix(self, offerer, nonce, want_mint):
        offer, ovault = self.offer_pda(offerer, nonce), self.ovault_pda(offerer, nonce)
        return Instruction(self.program, bytes([WITHDRAW_OFFER]) + nonce, [
            M(offerer, True, True), M(offer, False, True), M(ovault, False, True), M(want_mint, False, False),
            M(ata(offerer, want_mint), False, True), M(TOKEN, False, False)])

    def expire_offer_ix(self, caller, offerer, nonce, want_mint):
        offer, ovault = self.offer_pda(offerer, nonce), self.ovault_pda(offerer, nonce)
        return Instruction(self.program, bytes([EXPIRE_OFFER]) + nonce, [
            M(caller, True, True), M(offer, False, True), M(ovault, False, True), M(offerer, False, True),
            M(want_mint, False, False), M(ata(offerer, want_mint), False, True), M(TOKEN, False, False)])

    # ── high-level (build + send + confirm) ──
    def open_swap(self, maker, give_mint, give_amount, want_mint, want_amount, ttl_secs=3600,
                  terms=bytes(32), taker=bytes(32), nonce=None, alias=SYS, token_prog=TOKEN):
        nonce = nonce or new_nonce()
        sp, vp, ix = self.open_ix(maker.pubkey(), nonce, give_mint, give_amount, want_mint, want_amount,
                                  int(time.time()) + ttl_secs, terms, taker, alias, token_prog)
        self.send([maker], [ix], maker)
        return nonce, sp, vp

    def fill(self, taker, maker, nonce, give_mint, want_mint, expect_give, max_want, give_token_prog=TOKEN):
        ix = self.fill_ix(taker.pubkey(), maker, nonce, give_mint, want_mint, expect_give, max_want,
                          self.fee_wallet(), self.fee_shards(), give_token_prog)
        return self.send([taker], [ix], taker)

    def cancel(self, maker, nonce, give_mint, token_prog=TOKEN):
        return self.send([maker], [self.cancel_ix(maker.pubkey(), nonce, give_mint, self.fee_wallet(), token_prog)], maker)

    def expire(self, caller, maker, nonce, give_mint):
        return self.send([caller], [self.expire_ix(caller.pubkey(), maker, nonce, give_mint)], caller)

    def make_offer(self, offerer, swap_pk, want_mint, want_amount, ttl_secs=3600, nonce=None, alias=SYS):
        nonce = nonce or new_nonce()
        offer, ovault, ix = self.make_offer_ix(offerer.pubkey(), nonce, swap_pk, want_mint, want_amount, int(time.time()) + ttl_secs, alias)
        self.send([offerer], [ix], offerer)
        return nonce, offer, ovault

    def accept_offer(self, maker, swap_nonce, offerer, offer_nonce, give_mint, want_mint, cu=600_000):
        ix = self.accept_offer_ix(maker.pubkey(), swap_nonce, offerer, offer_nonce, give_mint, want_mint,
                                  self.fee_wallet(), self.fee_shards())
        return self.send([maker], [cu_ix(cu), ix], maker)

    def withdraw_offer(self, offerer, nonce, want_mint):
        return self.send([offerer], [self.withdraw_offer_ix(offerer.pubkey(), nonce, want_mint)], offerer)

    def expire_offer(self, caller, offerer, nonce, want_mint):
        return self.send([caller], [self.expire_offer_ix(caller.pubkey(), offerer, nonce, want_mint)], caller)

    # ── state reads ──
    def _data(self, pda):
        info = self.rpc.get_account_info(pda, commitment=Confirmed).value
        return bytes(info.data) if info else None

    @staticmethod
    def _swap_from(d):
        return Swap(_pk(d[S_MAKER:S_MAKER+32]), _pk(d[S_GIVE_MINT:S_GIVE_MINT+32]), _amt(d, S_GIVE_AMT),
                    _pk(d[S_WANT_MINT:S_WANT_MINT+32]), _amt(d, S_WANT_AMT), _pk(d[S_TAKER:S_TAKER+32]),
                    d[S_TERMS:S_TERMS+32], int.from_bytes(d[S_EXPIRY:S_EXPIRY+8], "little", signed=True),
                    d[S_STATUS], d[S_BUMP], bytes(d[S_NONCE:S_NONCE+32]))

    @staticmethod
    def _offer_from(d):
        return Offer(_pk(d[O_OFFERER:O_OFFERER+32]), _pk(d[O_SWAP:O_SWAP+32]), _pk(d[O_WANT_MINT:O_WANT_MINT+32]),
                     _amt(d, O_WANT_AMT), _pk(d[O_GIVE_MINT:O_GIVE_MINT+32]), _amt(d, O_GIVE_AMT),
                     int.from_bytes(d[O_EXPIRY:O_EXPIRY+8], "little", signed=True), d[O_STATUS], d[O_BUMP],
                     bytes(d[O_NONCE:O_NONCE+32]))

    def get_swap(self, pda):
        d = self._data(pda)
        return self._swap_from(d) if d and len(d) == SWAP_LEN else None

    def get_offer(self, pda):
        d = self._data(pda)
        return self._offer_from(d) if d and len(d) == OFFER_LEN else None

    def list_open_swaps(self, limit=200):
        """Browse all open listings on-chain (getProgramAccounts). Returns [(address, Swap)]."""
        resp = self.rpc.get_program_accounts(self.program, commitment=Confirmed, encoding="base64", filters=[SWAP_LEN])
        out = [(it.pubkey, self._swap_from(bytes(it.account.data)))
               for it in (resp.value or [])
               if len(bytes(it.account.data)) == SWAP_LEN and bytes(it.account.data)[S_STATUS] == STATUS_OPEN]
        return out[:limit]

    def list_open_offers(self, swap=None, limit=200):
        """Browse open bids; if `swap` given, only bids targeting that swap. Returns [(address, Offer)]."""
        target = swap if (swap is None or isinstance(swap, Pubkey)) else Pubkey.from_string(swap)
        resp = self.rpc.get_program_accounts(self.program, commitment=Confirmed, encoding="base64", filters=[OFFER_LEN])
        out = []
        for it in (resp.value or []):
            d = bytes(it.account.data)
            if len(d) == OFFER_LEN and d[O_STATUS] == STATUS_OPEN:
                o = self._offer_from(d)
                if target is None or o.swap == target:
                    out.append((it.pubkey, o))
        return out[:limit]

    def fill_listing(self, taker, sw):
        """Fill a discovered Swap object (from list_open_swaps) at its own published terms."""
        return self.fill(taker, sw.maker, sw.nonce, sw.give_mint, sw.want_mint, sw.give_amount, sw.want_amount)

    def get_config(self):
        d = self._data(self.config_pda())
        if not d or len(d) != CONFIG_LEN:
            return None
        return Config(_pk(d[C_ADMIN:C_ADMIN+32]), _pk(d[C_FEE_WALLET:C_FEE_WALLET+32]),
                      int.from_bytes(d[C_FEE_BPS:C_FEE_BPS+2], "little"), _amt(d, C_DELIST_FEE),
                      _pk(d[C_ALIAS_PROGRAM:C_ALIAS_PROGRAM+32]), d[C_FEE_SHARDS])

    def token_balance(self, token_account):
        try:
            return int(self.rpc.get_token_account_balance(token_account, commitment=Confirmed).value.amount)
        except Exception:
            return None

    def is_closed(self, pda):
        return self.rpc.get_account_info(pda, commitment=Confirmed).value is None
