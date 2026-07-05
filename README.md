# xete-programs

The on-chain Solana programs behind [xete](https://xete.net). Each ships a
**reproducible, verifiable build** — what runs on mainnet is byte-identical to the source
in this repo. Verify any of them yourself with
[`solana-verify`](https://github.com/Ellipsis-Labs/solana-verifiable-build); no trust required.

## Programs

| Program | Mainnet program ID | Source | Crate |
|---|---|---|---|
| **Swap / OTC** | `AXTSWAPVuUPivum39Fh2N6AS6SsktpABDpBLePknyvUD` | [`swap/`](swap/) | `xete-swap` |
| **Settlement** | `GPCsJ6kvrQ61wDG8bpP8315ge7AHfmsUHdxTD7LQ6CoJ` | [`program/lean/`](program/lean/) | `xete-tab` |

## Verify it yourself

```sh
# Swap / OTC program
solana-verify verify-from-repo -um \
  --program-id AXTSWAPVuUPivum39Fh2N6AS6SsktpABDpBLePknyvUD \
  --mount-path swap --library-name xete_swap \
  https://github.com/xetenet/xete-programs

# Settlement program
solana-verify verify-from-repo -um \
  --program-id GPCsJ6kvrQ61wDG8bpP8315ge7AHfmsUHdxTD7LQ6CoJ \
  --mount-path program/lean --library-name xete_tab \
  https://github.com/xetenet/xete-programs
```

Add `--remote` to also submit the result to the public
[OtterSec verified-builds](https://verify.osec.io) registry.

**Toolchain note.** These crates pull an `edition2024` dependency, so the build needs
Rust ≥ 1.85. If your `solana-verify` default image is older, pass a matching `--base-image`.
Every program commits its `Cargo.lock`, so the dependency graph is deterministic.

## Layout

- **`swap/`** — the OTC / sealed-order program (upgradeable; cold upgrade authority).
  Specs: [`swap/THREAT_MODEL.md`](swap/THREAT_MODEL.md), [`swap/EVENTS.md`](swap/EVENTS.md),
  [`swap/ROYALTY_SPEC.md`](swap/ROYALTY_SPEC.md).
- **`program/`** — the settlement program: `lean/` is the deployed build, `readable/` an
  annotated reference. See [`SPEC.md`](SPEC.md) and [`SETTLEMENT.md`](SETTLEMENT.md).

## Scope

This repo is the **on-chain program source** — the part `solana-verify` can prove
byte-for-byte. The relay / message server and the client apps are separate systems and are
not required to verify these programs.

## License

See [LICENSE](LICENSE).
