# Security

## Contact

Report security issues privately to **security@xete.net**. Please do not open a
public issue for a suspected vulnerability.

## Scope and current state

- **Program:** `GPCsJ6kvrQ61wDG8bpP8315ge7AHfmsUHdxTD7LQ6CoJ` (Solana mainnet).
- **The program is immutable.** Its upgrade authority has been relinquished, so
  no one — including the authors — can change, upgrade, seize, freeze, or
  redirect it. Any fix necessarily ships as a *new* program at a new address;
  this one cannot be patched in place.
- **Non-custodial.** Only the depositor's and the beneficiary's own keys move
  funds. There is no operator or admin key with any privilege over a settlement.

## Testing

The logic has been exercised by an adversarial suite (`tests/test_settlement.py`,
18 checks) covering every must-reject path and the positive controls. A
reproducible `solana-verify` build, so the on-chain bytes provably match this
source, is on the roadmap.

## What this contract is not

It is a neutral, non-custodial settlement primitive — not a custodial service,
not an escrow service, and not a mixer. It holds no one's funds on anyone's
behalf and exercises no discretion over release. See [SPEC.md](SPEC.md) and the
[README](README.md).
