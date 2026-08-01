# AXTSWAP — build, test and verification record

**Program:** `AXTSWAPVuUPivum39Fh2N6AS6SsktpABDpBLePknyvUD` (Solana mainnet, BPF Upgradeable)
**Toolchain (must match the build):** Agave `solana-cli 4.0.0` · `cargo 1.96.0` · `platform-tools v1.53`

This file is the public build-and-test record referenced by the program's on-chain
`security.txt` `auditors` field. Deployment procedure, key handling and operational
runbooks are maintained privately and are deliberately not published here.

## Test evidence

Full 41-tag regression, each suite run against a fresh local validator:

| Suite | Tags | Result |
|---|---|---|
| Core swap / offer | 0–11 | fee_edge 12/12 · shard 30/30 · concurrency 10/10 · alias 20/20 |
| Baskets | 12–17 | 23/23 |
| pNFT | 18–21 | 13/13 |
| Metaplex Core | 22–25 | 11/11 |
| Escrowless SPL / T22 / pNFT / Core | 26–37 | 18 · 9 · 8 · 8 |
| Sealed orders | 38–40 | 6 · 7 · 6 |

Host-level fuzzing: ed25519 instruction-layout parser (75M+ execs) and the Token-Metadata
royalty parser (38M+ execs), plus a stateful sequence fuzzer asserting a value-conservation
invariant. Zero compiler warnings on a clean `cargo build-sbf`.

Row-by-row threat-matrix mapping is in [`TEST_COVERAGE.md`](TEST_COVERAGE.md); the attack
model and the honest list of known gaps are in [`THREAT_MODEL.md`](THREAT_MODEL.md).

## Reproducible build

A clean `cargo clean && cargo build-sbf` reproduces the release artifact bit-for-bit.

The **default** `solana-verify` image is too old — its Cargo 1.84 cannot parse our
`edition2024` transitive dependencies — so the base image must be pinned to the toolchain
above:

```sh
solana-verify build --library-name xete_swap \
  --base-image solanafoundation/solana-verifiable-build:4.0.0
```

To verify the deployed program against this public repository:

```sh
solana-verify verify-from-repo -um \
  --program-id AXTSWAPVuUPivum39Fh2N6AS6SsktpABDpBLePknyvUD \
  --mount-path swap --library-name xete_swap \
  --base-image solanafoundation/solana-verifiable-build:4.0.0 \
  https://github.com/xetenet/xete-programs
```

Requires Docker. Add `--remote` to submit the result to the public
[OtterSec verified-builds](https://verify.osec.io) registry.

## Client note — address lookup tables

pNFT and Metaplex Core **sealed orders** need a **per-order v0 ALT**: 24 accounts overflow a
legacy transaction, and a shared static ALT saves too little to fit. The taker creates a
per-order ALT (~20 accounts) at settle time and pays a small refundable rent. This is a
client-flow concern only. SPL / Token-2022 sealed orders and every escrowed and escrowless
path need no ALT.
