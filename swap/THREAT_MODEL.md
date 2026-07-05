# xete-swap — threat model

How this contract can be attacked, and exactly what stops each attack. Written
against `src/lib.rs` (open/fill/cancel/expire + make/accept/withdraw_offer).

## Trust model

- **No party is trusted.** Maker, taker, offerer, and any random caller are all
  treated as adversarial. The program is the only trusted actor.
- **Funds at rest live in program-owned PDA vaults** (`vault` for a swap's give-leg,
  `ovault` for an offer's bid). Only the program, signing with the PDA seeds, can
  move them.
- **Atomic or nothing.** Every settlement moves both legs and closes the escrow in
  one instruction. There is no "settled but still open" state — we *close* the
  account rather than flip a status flag, so there is no stale-state window.

## The standing defenses (applied everywhere)

Three checks recur and shut down most Solana attacks:

1. **Owner check** — `account.owner() == program_id` before trusting any state
   account. A closed/foreign account fails this.
2. **PDA re-derivation** — every swap/vault/offer/ovault address is recomputed with
   `find_program_address(seeds)` and compared to the passed account. You cannot
   substitute a look-alike account.
3. **Recipient binding** — before paying out, the destination token account's mint
   *and owner* bytes are read and checked against the expected party. You cannot
   redirect a payout to yourself.

Plus `TransferChecked` (not `Transfer`) on every token move, so the SPL Token program
itself enforces the mint and decimals of each account.

## Token-2022 (Token Extensions) — out of scope, rejected by construction

**This contract supports classic SPL Token mints only.** Every token CPI targets the
hard-coded classic Token program (`pinocchio_token::ID` = `Tokenkeg…`) and derives ATAs
under it. A Token-2022 mint's accounts are owned by the Token-2022 program (`TokenzQd…`),
so any `open`/`fill`/`offer` referencing one fails the CPI and the transaction reverts:
**no escrow is ever created against a 2022 mint, and no funds can be stranded.** Verified
on a local validator (a 2022 give-mint `open` is rejected).

This is a deliberate v1 decision, not an oversight. Token-2022 extensions carry
escrow-hostile behaviours — **transfer fees** (silent shortfall vs the stated amount),
**transfer hooks** (arbitrary CPI on every move), **permanent delegate** (a third party
can drain a vault), **non-transferable**, **default-frozen**, **confidential** amounts.
Admitting 2022 safely requires an on-chain extension allow-list plus net-of-fee
accounting; that is reserved for a future version. Until then 2022 is refused at the
program boundary, by construction. Clients SHOULD filter token pickers to classic-Token
mints so a user sees a clean "unsupported" message instead of a failed transaction.

## Attacks and preventions

### A. Account substitution
| # | Attack | Prevention |
|---|--------|-----------|
| A1 | Pass a fake program-owned "swap"/"offer" to fake a settlement | Owner check (`owner() == program_id`) + PDA re-derivation |
| A2 | Pass a vault/ovault you control to drain or escrow elsewhere | Vault/ovault PDA re-derived from the *same* `(party, nonce)` that derived the verified state account |
| A3 | Pass a wrong mint to spoof decimals | Mint address compared to stored `S_GIVE_MINT/S_WANT_MINT/O_WANT_MINT`; `TransferChecked` also enforces mint+decimals on-chain |
| A4 | Pass a malicious "token program" / "system program" | CPIs target the hard-coded `pinocchio_token::ID` / system ID; the passed program account is only loaded, never trusted — a wrong one just fails the tx (self-DoS). **This is also why Token-2022 mints are refused** — their accounts are owned by `TokenzQd…`, not the hard-coded classic program (see *Token-2022 scope* below). |

### B. Payment / refund redirection
| # | Attack | Prevention |
|---|--------|-----------|
| B1 | `fill`: send the taker's payment somewhere other than the maker | `maker_want` verified: mint == want_mint **and** owner == state maker |
| B2 | `accept_offer`: redirect the bid (not to maker) or the goods (not to offerer) | `maker_want` owner == maker **and** `offerer_give` owner == offerer |
| B3 | `expire` (permissionless): redirect the refund away from the maker | `refund_and_close` verifies `maker_give` owner == maker |
| B4 | `withdraw_offer`: redirect the refund | `offerer_want` owner == offerer (+ offerer must sign) |

### C. Authorization bypass
| # | Attack | Prevention |
|---|--------|-----------|
| C1 | Cancel someone else's swap | Maker must **sign** and `state.maker == signer` |
| C2 | Accept an offer on a swap you don't own | Maker must **sign** and `state.maker == signer`; only the listing's maker can accept |
| C3 | Withdraw someone else's bid | Offerer must **sign** and `offer.offerer == signer` |
| C4 | `fill` / `expire` are intentionally permissionless | By design — `fill` pays the maker no matter who calls; `expire` always refunds the maker. Neither can divert funds (see B1/B3) |

### D. Cross-object confusion
| # | Attack | Prevention |
|---|--------|-----------|
| D1 | `accept_offer` pairing an offer with an *unrelated* swap (pay from one, release from another) | `offer.O_SWAP == swap.address()` binds the bid to that exact listing |
| D2 | Mismatched mints between swap and offer | `offer.want_mint == swap.want_mint == want_mint account`; give side checked against `swap.give_mint` |
| D3 | Pair swap-A state with vault-B | Both derived from the same `(maker, nonce)`; the nonce that re-derives the verified swap must also re-derive the vault |

### E. Double-spend / replay / re-entrancy
| # | Attack | Prevention |
|---|--------|-----------|
| E1 | Fill twice, fill-after-cancel, double-withdraw, etc. | Settlement **closes** the state account (data zeroed, lamports swept). A second call fails the owner/length/status check |
| E2 | Re-init the same swap/offer (same nonce) | `CreateAccount` reverts if the account already exists |
| E3 | Re-entrancy via a callback | Only CPIs are to the SPL Token program, which never calls back. No re-entrancy surface |
| E4 | Settle an expired listing | `fill`/`accept_offer` reject once `now >= expiry`; only `expire` (refund) works after |

### F. Economic / arithmetic
| # | Attack | Prevention |
|---|--------|-----------|
| F1 | Overflow when crediting swept rent | `checked_add`, errors on overflow |
| F2 | Accept an offer at terms worse than the offerer agreed to | `give_amount` is the swap's, **immutable** after `open`; the offer is bound to that swap. The offerer gets exactly what they bid on, so no maker can move the goalposts |
| F3 | Price manipulation on accept | The maker signs the acceptance and the price paid is the offer's own escrowed bid — the maker consents to it |

### G. Griefing / DoS
| # | Attack | Prevention / note |
|---|--------|-----------|
| G1 | Front-run a fill / race cancel-vs-fill / accept-vs-withdraw | Atomic; first to land wins, the loser's tx reverts. Only tx fees lost |
| G2 | Spam offers against a maker's listing | Each offerer pays their own rent; the maker is never forced to interact. No maker-side DoS |
| G3 | `accept_offer` compute cost (4 PDA derivations + 4 token CPIs) | Works within a raised CU limit (test uses 600k). Not a vuln; optimization noted below |

## Known gaps & planned hardening (honest list)

1. **No permissionless offer-expiry sweep.** Swaps have `expire` (anyone can refund the
   maker after the deadline); offers do **not** — only the offerer can `withdraw`. Funds
   are never lost (the offerer can always reclaim), but an abandoned bid locks the
   offerer's own tokens + rent until *they* act. **Plan:** add `expire_offer` mirroring
   `expire`, refunding the offerer permissionlessly after `O_EXPIRY`.
2. **Zero-amount / already-expired orders** — *FIXED (2026-06-07):* `open_swap` and
   `make_offer` now reject `amount == 0`, and `open_swap` rejects an already-past expiry.
3. **CU cost of `accept_offer`.** Four `find_program_address` calls dominate. **Plan:**
   accept the bumps in instruction data and use `create_program_address` (cheap verify)
   instead of re-finding them.
4. **The terms hash `H` is recorded, not enforced.** `H = hash(terms‖item)` is stored as
   an auditable commitment; on-chain settlement does **not** gate on it. For on-chain
   assets the give-leg *is* the asset, so this is fine. Binding an **off-chain** item to a
   transfer is the "rung 2 / hard" problem and is explicitly out of scope here.
5. **Targeted (private) swaps reserved, not active.** `S_TAKER` is laid out but unused;
   today every swap is fillable by any taker. **Plan:** enforce `S_TAKER` when non-zero.

## The PDA-reuse rug — found and fixed (2026-06-07)

A swap PDA is reusable after close, so a maker could `cancel` a listing and `open` a new
one at the **same slot** with worse goods, then `accept` a standing bid — address-binding
was not terms-binding (this falsified the original D1 / F2 claims). **Fixed:** the offer
now records the `give_mint` + `give_amount` it is bidding for, and `accept_offer` rejects
if the listing's current goods differ; `fill` now takes an `expect_give` + `max_want`
slippage guard. The adversarial suite reproduces the attack (the close+reopen genuinely
succeeds) and asserts the `accept` is rejected.

## Test coverage

Two validator suites, both green — run with `bash run_swap_test.sh`:

- **`tests/swap_test.py` — 41/41.** Positive paths for all seven instructions, the rug
  regression, fill-slippage rejects, and the input guards.
- **`tests/adversarial.py` — 42/42.** Every client-triggerable row asserted to **reject**:
  A1–A4, B1–B4, C1–C3, D1, E1–E2, E4, plus the guards. The non-triggerable rows (C4
  permissionless-by-design, E3 re-entrancy, F1 overflow, G1–G3 races/spam/CU) are
  documented in that file, not asserted.

Row-by-row mapping in **`TEST_COVERAGE.md`**.
