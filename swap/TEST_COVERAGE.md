# xete-swap — test coverage

Two suites run against a local `solana-test-validator`. One command does deploy + mint +
fund + run both:

```
bash run_swap_test.sh
```

- **`tests/swap_test.py` — 41/41** — happy paths + the rug regression + slippage + guards.
- **`tests/adversarial.py` — 42/42** — every client-triggerable threat-matrix row, asserted
  to **reject**.

## Happy path (`swap_test.py`)

| Flow | What it proves |
|------|----------------|
| `open → fill` | atomic two-leg settle; maker gets want, taker gets give; swap+vault closed |
| `open → cancel` | maker reclaims the give-leg; accounts closed; rent returned |
| `open → expire` | permissionless refund after the deadline; fill-after-expiry rejected |
| `open → make_offer → accept` | bid escrowed; maker takes the bid, offerer gets the goods; all 4 accounts closed |
| `open → make_offer → withdraw` | bidder reclaims the escrowed bid |
| **rug regression** | close + reopen-same-slot with worse goods, then `accept` → **rejected** |
| **slippage** | `fill` with `max_want` too low, or wrong `expect_give` → **rejected** |
| **guards** | zero-give open, already-expired open → **rejected** |

## Adversarial must-reject (`adversarial.py`)

| # | Attack | Asserted |
|---|--------|----------|
| A1 | swap account not program-owned | reject |
| A2 | wrong vault account | reject |
| A3 | wrong give-mint | reject |
| A4 | bogus token program | reject |
| B1 | `fill` pays a non-maker account | reject |
| B2 | `accept` routes goods to non-offerer | reject |
| B2 | `accept` pays a non-maker account | reject |
| B3 | `expire` refund to non-maker | reject |
| B4 | `withdraw` refund to non-offerer | reject |
| C1 | cancel by non-maker | reject |
| C2 | accept by non-maker | reject |
| C3 | withdraw by non-offerer | reject |
| D1 | accept an offer against the wrong swap | reject |
| E1 | fill after cancel | reject |
| E1 | double-withdraw | reject |
| E2 | re-init same nonce while open | reject |
| E4 | accept after the swap expired | reject |
| — | zero-give / zero-want / already-expired open | reject |

## Documented, not asserted (not client-triggerable)

| # | Why it can't be tested from a client |
|---|--------------------------------------|
| C4 | `fill` / `expire` are permissionless **by design** — they can only ever pay the maker |
| E3 | re-entrancy is structurally impossible — the only CPI is to SPL Token, which never calls back |
| F1 | rent-credit overflow is not reachable with real lamport values (`checked_add` guards it anyway) |
| G1 | transaction races — non-deterministic; atomicity makes the loser revert with only fees lost |
| G2 | spam bids — allowed, each bidder pays their own rent; not a correctness rejection |
| G3 | compute cost — a budget/optimization concern, not a rejection |
