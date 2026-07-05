"""SDK smoke test — drives the deployed program entirely through the SDK, and proves the fee split.
    python sdk/smoke.py <PROGRAM_ID> <GIVE> <WANT> <maker.json> <taker.json> <fee_wallet> [rpc]"""
import sys, os, json
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from solders.keypair import Keypair
from solders.pubkey import Pubkey
from solana.rpc.commitment import Confirmed
from xete_swap import XeteSwap, ata

PROG, G, W, MK, TK, FW = sys.argv[1:7]
RPC = sys.argv[7] if len(sys.argv) > 7 else "http://127.0.0.1:8899"
load = lambda p: Keypair.from_bytes(bytes(json.load(open(p))))
maker, taker = load(MK), load(TK)
GIVE, WANT, FEE_WALLET = Pubkey.from_string(G), Pubkey.from_string(W), Pubkey.from_string(FW)
xs = XeteSwap(PROG, RPC)
GA, WA, BID = 100_000_000, 250_000_000, 300_000_000
bps = xs.get_config().fee_bps
fee_of = lambda amt: amt * bps // 10_000
fee_acct = ata(FEE_WALLET, WANT)

res = []
def ck(label, cond):
    res.append(bool(cond)); print(f"[{'PASS' if cond else 'FAIL'}] {label}")

print(f"(config fee = {bps} bps)")
# open -> fill, all via the SDK; fee skimmed to the fee wallet, maker gets the rest
nonce, sp, vp = xs.open_swap(maker, GIVE, GA, WANT, WA, ttl_secs=3600)
ck("open_swap + decode", (s := xs.get_swap(sp)) and s.give_amount == GA and s.maker == maker.pubkey())
mw0 = xs.token_balance(ata(maker.pubkey(), WANT)) or 0
tg0 = xs.token_balance(ata(taker.pubkey(), GIVE)) or 0
fee0 = xs.token_balance(fee_acct) or 0
xs.fill(taker, maker.pubkey(), nonce, GIVE, WANT, expect_give=GA, max_want=WA)
ck("fill -> maker nets want minus fee", (xs.token_balance(ata(maker.pubkey(), WANT)) or 0) - mw0 == WA - fee_of(WA))
ck("fill -> fee wallet collected the fee", (xs.token_balance(fee_acct) or 0) - fee0 == fee_of(WA))
ck("fill -> taker received full give", (xs.token_balance(ata(taker.pubkey(), GIVE)) or 0) - tg0 == GA)
ck("fill -> swap + vault closed", xs.is_closed(sp) and xs.is_closed(vp))

# open -> make_offer -> accept_offer, fee skimmed from the bid
n2, sp2, vp2 = xs.open_swap(maker, GIVE, GA, WANT, WA, ttl_secs=3600)
on, offer, ovault = xs.make_offer(taker, sp2, WANT, BID, ttl_secs=3600)
ck("make_offer escrow + pinned give-terms", xs.token_balance(ovault) == BID and (o := xs.get_offer(offer)) and o.give_amount == GA)
mw1 = xs.token_balance(ata(maker.pubkey(), WANT)) or 0
fee1 = xs.token_balance(fee_acct) or 0
xs.accept_offer(maker, n2, taker.pubkey(), on, GIVE, WANT)
ck("accept_offer -> maker nets bid minus fee", (xs.token_balance(ata(maker.pubkey(), WANT)) or 0) - mw1 == BID - fee_of(BID))
ck("accept_offer -> fee wallet collected the fee", (xs.token_balance(fee_acct) or 0) - fee1 == fee_of(BID))
ck("accept_offer -> swap + offer + vaults closed", xs.is_closed(sp2) and xs.is_closed(offer) and xs.is_closed(ovault))

# cancel charges the flat delist fee (SOL) to the fee wallet
n3, sp3, vp3 = xs.open_swap(maker, GIVE, GA, WANT, WA, ttl_secs=3600)
fsol0 = xs.rpc.get_balance(FEE_WALLET, commitment=Confirmed).value
xs.cancel(maker, n3, GIVE)
ck("cancel -> delist fee paid to fee wallet", xs.rpc.get_balance(FEE_WALLET, commitment=Confirmed).value - fsol0 == xs.get_config().delist_fee)
ck("cancel -> swap + vault closed", xs.is_closed(sp3) and xs.is_closed(vp3))

# discovery: browse open listings, then fill one using ONLY the discovered data (no nonce handed over)
n4, sp4, vp4 = xs.open_swap(maker, GIVE, GA, WANT, WA, ttl_secs=3600)
found = [(a, s) for (a, s) in xs.list_open_swaps() if a == sp4]
ck("discovery: list_open_swaps finds the listing (with its nonce)", len(found) == 1 and found[0][1].nonce == n4)
mw3 = xs.token_balance(ata(maker.pubkey(), WANT)) or 0
xs.fill_listing(taker, found[0][1])
ck("discovery: fill_listing settles from discovered data", xs.is_closed(sp4) and (xs.token_balance(ata(maker.pubkey(), WANT)) or 0) - mw3 == WA - fee_of(WA))

p = sum(res)
print(f"\n{p}/{len(res)} SDK smoke checks passed " + ("*** ALL GREEN ***" if p == len(res) else "*** FAIL ***"))
sys.exit(0 if p == len(res) else 1)
