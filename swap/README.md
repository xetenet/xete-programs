# xete-swap (skunkworks)

Rung 1 of the agent marketplace: **atomic SPL-token ⇄ token settlement** — e.g. an
agent sells excess on-chain compute tokens for USDC, trustlessly, both legs or neither.
Separate from `xete-tab` (the SOL settlement contract); its own private repo.

Lean **Pinocchio** (`no_std`, zero heap, hand-laid byte layout), same house style and
release profile as `xete-escrow-pin`. **Single file by design** (`src/lib.rs`) so it
audits top-to-bottom.

## Status — all seven instructions live, validator-verified
- [x] `open_swap` — escrow the give-leg into a program-owned vault + post terms
- [x] `fill` — atomic two-leg settle at the taker's pinned price (slippage-guarded)
- [x] `cancel_swap` — maker reclaims before any fill
- [x] `expire` — permissionless refund after the deadline
- [x] `make_offer` / `accept_offer` / `withdraw_offer` / `expire_offer` — the bid/offer queue
- [x] `S_TAKER` private/targeted swaps (named-counterparty OTC)
- [x] validator suites: **happy 48/48 + adversarial 51/51**
- [x] **P0 PDA-reuse rug found & fixed** (terms-pinned offers + slippage) — see `THREAT_MODEL.md`

Open backlog (see `THREAT_MODEL.md` → Known gaps): `expire_offer`, CU optimization,
`S_TAKER` private swaps, the protocol-fee hook, native-SOL legs.

## Security
- `THREAT_MODEL.md` / `THREAT_MODEL.html` — attack→mitigation matrix (A–G) + honest gaps.
- `TEST_COVERAGE.md` — every matrix row mapped to its must-reject assertion.
- Standing defenses: owner check · PDA re-derivation · recipient binding · `TransferChecked`.

## Instruction tags
`0 open_swap · 1 fill · 2 cancel_swap · 3 expire · 4 make_offer · 5 accept_offer · 6 withdraw_offer · 7 expire_offer`

## State layouts
- **Swap (186 B):** `maker[32] · give_mint[32] · give_amount:u64 · want_mint[32] ·
  want_amount:u64 · taker[32] (0=open) · terms_hash[32] · expiry:i64 · status:u8 · bump:u8`
- **Offer (154 B):** `offerer[32] · swap[32] · want_mint[32] · want_amount:u64 ·
  give_mint[32] · give_amount:u64 · expiry:i64 · status:u8 · bump:u8`
  — `give_mint`+`give_amount` pin *what the bid is for* (the rug fix).

## Build & test
```
cargo build-sbf          # build the program
bash run_swap_test.sh    # deploy to a local validator + run both suites
```

## Gate / scope
Skunkworks + for-profit marketplace horizon. **Validator only** — do not deploy; not
grant scope. Designs route through the lead / John before any mainnet.
