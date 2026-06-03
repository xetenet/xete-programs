"""Adversarial suite for the xete-tab settlement contract — every must-reject
plus the positive controls. Run against a validator with the program deployed:

    python tests/test_settlement.py <PROGRAM_ID> [rpc]

18 checks: valid deposit/claim/reclaim, timelock both ways, rent-follows-funds,
and the full set of rejections (wrong salt, wrong beneficiary, non-depositor
reclaim, double-claim, reclaim-after-claim, claim-after-reclaim, claim-before-
unlock, sub-rent deposit, wrong PDA, claim-non-existent, re-init).
"""
import hashlib, struct, sys, time
from solders.keypair import Keypair
from solders.pubkey import Pubkey
from solders.instruction import AccountMeta, Instruction
from solders.message import Message
from solders.transaction import Transaction
from solana.rpc.api import Client
from solana.rpc.commitment import Confirmed
from solana.rpc.types import TxOpts

SYS = Pubkey.from_string("11111111111111111111111111111111")
# PDA seed — the load-bearing on-chain constant (see SPEC.md / the "note on the
# PDA seed" in README.md). Clients MUST use this exact byte-string.
SEED = b"escrow"
PROG = None
c = None

def deposit_data(sid, amt, cm, unlock): return bytes([0]) + sid + struct.pack("<Q", amt) + cm + struct.pack("<q", unlock)
def claim_data(sid, salt): return bytes([1]) + sid + struct.pack("<I", len(salt)) + salt
def reclaim_data(sid): return bytes([2]) + sid
def pda(sid): return Pubkey.find_program_address([SEED, sid], PROG)[0]
def commit(b, salt): return hashlib.sha256(bytes(b) + salt).digest()

def dep_ix(A, sid, amt, cm, unlock, pda_override=None):
    p = pda_override or pda(sid)
    return Instruction(program_id=PROG, data=deposit_data(sid, amt, cm, unlock),
        accounts=[AccountMeta(A.pubkey(), True, True), AccountMeta(p, False, True), AccountMeta(SYS, False, False)])
def claim_ix(B, sid, salt, pda_override=None):
    p = pda_override or pda(sid)
    return Instruction(program_id=PROG, data=claim_data(sid, salt),
        accounts=[AccountMeta(B.pubkey(), True, True), AccountMeta(p, False, True)])
def reclaim_ix(A, sid, pda_override=None):
    p = pda_override or pda(sid)
    return Instruction(program_id=PROG, data=reclaim_data(sid),
        accounts=[AccountMeta(A.pubkey(), True, True), AccountMeta(p, False, True)])

def attempt(signers, ixs, payer):
    bh = c.get_latest_blockhash().value.blockhash
    tx = Transaction(signers, Message.new_with_blockhash(ixs, payer.pubkey(), bh), bh)
    try:
        sig = c.send_transaction(tx, opts=TxOpts(skip_preflight=False, preflight_commitment=Confirmed)).value
    except Exception as e:
        return ('rejected', str(e).replace("\n", " ")[:80])
    for _ in range(50):
        time.sleep(0.3)
        st = c.get_signature_statuses([sig]).value[0]
        if st and st.confirmation_status:
            return ('rejected', str(st.err)[:80]) if st.err else ('ok', str(sig)[:14])
    return ('timeout', '')

results = []
def check(label, got, want):
    ok = got[0] == want
    results.append(ok)
    extra = '' if got[0] == 'ok' else f"  ({got[1]})"
    print(f"[{'PASS' if ok else 'FAIL'}] {label} -> {got[0]}{extra}")

def airdrop(pk, sol):
    c.request_airdrop(pk, int(sol * 1e9))
    for _ in range(60):
        time.sleep(0.3)
        if c.get_balance(pk, commitment=Confirmed).value > 0:
            return
def bal(pk): return c.get_balance(pk, commitment=Confirmed).value
def rid(): return bytes(Keypair().pubkey())

def main():
    global PROG, c
    PROG = Pubkey.from_string(sys.argv[1])
    c = Client(sys.argv[2] if len(sys.argv) > 2 else "http://127.0.0.1:8899")
    A, B, C = Keypair(), Keypair(), Keypair()
    for kp in (A, B, C): airdrop(kp.pubkey(), 3.0)
    salt = b"the-correct-salt"; AMT = 100_000_000

    e1 = rid()
    check("1 valid deposit", attempt([A], [dep_ix(A, e1, AMT, commit(B.pubkey(), salt), 0)], A), 'ok')
    check("2 claim WRONG salt", attempt([B], [claim_ix(B, e1, b"wrong")], B), 'rejected')
    check("3 claim WRONG beneficiary (C, right salt)", attempt([C], [claim_ix(C, e1, salt)], C), 'rejected')
    check("4 reclaim by NON-depositor (C)", attempt([C], [reclaim_ix(C, e1)], C), 'rejected')
    b0 = bal(B.pubkey())
    check("5 valid claim", attempt([B], [claim_ix(B, e1, salt)], B), 'ok')
    print(f"     receiver delta = {(bal(B.pubkey())-b0)/1e9:+.6f} SOL (expect ~+0.0999)")
    check("6 DOUBLE claim", attempt([B], [claim_ix(B, e1, salt)], B), 'rejected')
    check("7 reclaim AFTER claim", attempt([A], [reclaim_ix(A, e1)], A), 'rejected')

    e2 = rid()
    check("8 deposit e2", attempt([A], [dep_ix(A, e2, AMT, commit(B.pubkey(), salt), 0)], A), 'ok')
    check("9 valid reclaim (depositor)", attempt([A], [reclaim_ix(A, e2)], A), 'ok')
    check("10 claim AFTER reclaim", attempt([B], [claim_ix(B, e2, salt)], B), 'rejected')

    e3 = rid(); unlock = int(time.time()) + 6
    check("11 deposit e3 (timelock +6s)", attempt([A], [dep_ix(A, e3, AMT, commit(B.pubkey(), salt), unlock)], A), 'ok')
    check("12 claim BEFORE unlock", attempt([B], [claim_ix(B, e3, salt)], B), 'rejected')
    print("     waiting out timelock..."); time.sleep(9)
    check("13 claim AFTER unlock", attempt([B], [claim_ix(B, e3, salt)], B), 'ok')

    check("14 SUB-RENT deposit (1000 lamports)", attempt([A], [dep_ix(A, rid(), 1000, commit(B.pubkey(), salt), 0)], A), 'rejected')

    e5, e6 = rid(), rid()
    check("15 WRONG PDA deposit", attempt([A], [dep_ix(A, e5, AMT, commit(B.pubkey(), salt), 0, pda_override=pda(e6))], A), 'rejected')
    check("16 claim NON-EXISTENT settlement", attempt([B], [claim_ix(B, rid(), salt)], B), 'rejected')

    e8 = rid()
    check("17 deposit e8", attempt([A], [dep_ix(A, e8, AMT, commit(B.pubkey(), salt), 0)], A), 'ok')
    check("18 RE-INIT same settlement", attempt([A], [dep_ix(A, e8, AMT, commit(B.pubkey(), salt), 0)], A), 'rejected')

    p = sum(results)
    print(f"\n{p}/{len(results)} checks passed", "— all green" if p == len(results) else "— *** FAILURES ABOVE ***")
    sys.exit(0 if p == len(results) else 1)

main()
