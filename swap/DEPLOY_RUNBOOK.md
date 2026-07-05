# AXTSWAP mainnet upgrade — deploy runbook

**Program:** `AXTSWAPVuUPivum39Fh2N6AS6SsktpABDpBLePknyvUD` (LIVE, BPF Upgradeable)
**Upgrade authority:** `CmraiWB8rTfR4td7iC7TmvrjMGbJv1nqkvJsbz2MJaDq` — keypair at `<upgrade-authority-keypair.json>` (gitignored)
**Source:** branch `feat/escrowless-nft-otc`, modular tree `src/{lib,state,cpi,config,swap,offer,basket,pnft,mcore,escrowless,order}.rs`, tags 0–40 (refactored from the single-file `362aadc`; behaviour proven identical by full regression below — pending commit)
**Toolchain (must match the build):** solana-cli 4.0.0 (Agave) · cargo 1.96.0

## Artifact
- New build (royalty v1, committed `8e3bc60`): `target/deploy/xete_swap.so` — **315,688 bytes**, sha256 **`3a709e8195ba23be1d24b66f39a30f6c731d974a51e43b19dcdf185e6fb9eb37`** (voluntary royalty protocol v1: signature-bound royalty_mode+maker_pct, royalty.rs TM parser, pay_want_and_royalty, list_pnft carries+stores the term). Full `cargo clean` rebuild reproduces this exact hash bit-for-bit.
- Superseded: modular-only build 305,104 / `847c6cab…` ; original monolith 311,184 / `17c2d595…`.
- Current on-chain bytes: 135,984, sha256 `ad46fc50bd12d3be1c6647800676c6f7fa1a9ffb2736ee0634e2087c4d0133b4` (backup: `solana program dump … pre-upgrade-backup.so`)
- Program ~doubled (added baskets/bearer + escrowless ×4 + sealed orders).

## Pre-flight — DONE, fundless (do not need to repeat unless source changes)
- [x] Full 41-tag regression GREEN on the FINAL (modular + 5-improvements) build, each suite on a fresh validator: core 0–11 (fee_edge 12/12, shard 30/30, concurrency 10/10, alias 20/20) · baskets 12–17 (23/0) · pNFT 18–21 (13/13) · Core 22–25 (11/11) · escrowless SPL/T22/pNFT/Core 26–37 (18+9+8+8) · sealed orders 38–40 (6+7+6).
- [x] Clean deterministic rebuild (cargo clean → build-sbf from committed 8e3bc60) → 315,688 bytes / sha256 above, REPRODUCED bit-identical. ZERO compiler warnings.
- [x] Mainnet live-state audit: only ONE account under the program — the Config PDA `9qHGNBB4zpTSfRSyUkbVhEtJFfQsur7nkuqfnNyxBdfb` (108 bytes). No live swaps/offers/listings.
- [x] Migration safety: live Config parses correctly under the new layout (admin=CmraiWB8, fee_wallet=xtFEE, fee_bps=30, delist=0.001 SOL, shards=0, alias-gate OFF).
- [x] **Upgrade rehearsal on the REAL mainnet bytes (localnet): 4/4 — config survived, a pre-existing OLD swap filled post-upgrade, new tags 26/27 work post-upgrade.**
- [x] **CLI 4.0.0 auto-extends** programdata on `program deploy --program-id` — NO manual `solana program extend` needed.

## Cost / funding (PAID — gated on John's ~$250)
- Net permanent cost ≈ **1.22 SOL** (programdata rent grows 0.948 → 2.167 SOL) + ~0.003–0.01 SOL tx fees (~600 write chunks).
- Temporary buffer ≈ 2.17 SOL (refunded on success).
- Deploy wallet CmraiWB8 balance at audit: **0.613 SOL → INSUFFICIENT.** Top up to **~3 SOL** before deploying.

## Deploy steps (PAID)
1. Confirm wallet funded (≥ ~3 SOL): `solana balance CmraiWB8… --url mainnet-beta`
2. Backup current bytes: `solana program dump AXTSWAPVuUP… pre-upgrade-backup.so --url mainnet-beta` (expect sha256 `ad46fc50…`)
3. Build + verify hash: `cargo build-sbf` → `sha256sum target/deploy/xete_swap.so` MUST equal `3a709e81…9eb37`
4. **Upgrade (auto-extends, retains authority — do NOT pass `--final`):**
   ```
   solana program deploy --program-id AXTSWAPVuUPivum39Fh2N6AS6SsktpABDpBLePknyvUD \
     target/deploy/xete_swap.so \
     --upgrade-authority <upgrade-authority-keypair.json> \
     --url mainnet-beta
   ```
5. Verify: `solana program show AXTSWAPVuUP… --url mainnet-beta` → Data Length 315688, Authority CmraiWB8, new Last Deployed slot.
6. (optional) `solana-verify` for a reproducible-build attestation.

## After the program upgrade (PAID, separate)
- **pNFT/Core sealed orders need a PER-ORDER v0 ALT** (NOT a shared one): 24 accounts overflow a legacy tx, and a shared static ALT (~8 program/sysvar/config accounts) saves too little to fit. The taker creates a per-order ALT (~20 accounts) at settle time and pays a tiny refundable rent — a client-flow feature, no special deploy step. SPL/T22 sealed orders + all escrow/escrowless paths need NO ALT.
- **Per-standard canary:** small real-asset SPL → pNFT → Core round-trips via the Python SDK.

## Rollback
Authority is retained, so a bad upgrade can be re-upgraded to `pre-upgrade-backup.so` (re-run step 4 with the backup .so). Migration is one-way only if account layouts change — they did not here.

## Verified (reproducible) build — public verification
CONFIRMED byte-identical to the deploy artifact. The DEFAULT solana-verify image is too old (Cargo 1.84 cannot
parse our `edition2024` transitive deps) — you MUST pin the base image to our toolchain (solana 4.0.0):

    solana-verify build --library-name xete_swap \
      --base-image solanafoundation/solana-verifiable-build:4.0.0

Reproduces exactly: 315,688 B · sha256 `3a709e8195ba23be1d24b66f39a30f6c731d974a51e43b19dcdf185e6fb9eb37`
prog-hash `f25e946165bbd4ee2b8c02381ee577c586e1abe839199c659482dc724837a70f`. Requires Docker.

After deploy, mint the on-chain verified badge from the PUBLIC repo:

    solana-verify verify-from-repo <github-url> \
      --program-id AXTSWAPVuUPivum39Fh2N6AS6SsktpABDpBLePknyvUD \
      --library-name xete_swap --base-image solanafoundation/solana-verifiable-build:4.0.0

## Post-deploy smoke test (run BEFORE any real users)
Instant, read-only deploy verification (zero SOL):
    bash ops/mainnet_smoke_verify.sh
  -> PASS when Data Length=315688, on-chain hash=f25e9461…, authority=CmraiWB8 retained, config(108B) survives.
Live functional canary (throwaway wallets + junk tokens + ~0.1 SOL on a funded payer):
    bash ops/mainnet_canary.sh <FUNDED_PAYER_KEYPAIR.json>
  Exercises open_swap -> fill -> cancel on the LIVE program, confirms the XetePostCreated event actually
  emits on mainnet, and DUMPS the tx logs on any failure for instant diagnosis. Reclaims what it can.
Only after BOTH pass: proceed to the real-asset (junk pNFT -> real pNFT) canary.
