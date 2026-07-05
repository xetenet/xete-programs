# AXTSWAP voluntary-royalty protocol — build spec (bulletproof, custody-grade)

Status: DESIGN LOCKED (John, 2026-06-29). Ships in the FIRST contract deploy. Build loop is live.
This is the authoritative reference; the contract source + tests must conform to it exactly.

## 0. Principle
The royalty ("r") is a **negotiated, optional** term that, *when a party chooses to pay it*, is paid **exactly as
the asset's on-chain metadata stipulates** — destination, shares, bps all read from chain, enforced atomically by
the contract at settle, or the trade reverts. The chain is the sole source of truth for WHERE money goes; the
parties negotiate only WHO pays. Custody code: **fail-closed** everywhere — any parse ambiguity, account mismatch,
or dust discrepancy ⇒ revert; never pay the wrong amount or the wrong wallet.

## 1. Decisions (John, final)
- CREATORS: pay **exactly as written on chain** (every creator with share>0, verified or not). Chain = truth.
  Clients show a **prominent notice to BOTH maker + taker** when an UNVERIFIED creator wallet will receive funds
  (show address + amount). Copy: "this wallet hasn't verified itself as a creator on-chain" — NOT "scam"
  (unverified = creator didn't co-sign; many legit creators never verify). Voluntary ⇒ uneasy party declines/counters.
- PAYER negotiation: mode ∈ {NONE, MAKER (from proceeds), TAKER (on top), SPLIT(maker_pct 0..100)}. Maker can
  offer / require / be hands-off; taker can COUNTER. All options at all points (counters live in the bid/sealed
  flows; a public listing is take-it-or-leave-it on its stated mode).
- COUNTER-OFFER = ATOMIC REJECT: a counter rejects the prior terms in the SAME action ⇒ never two live offers.
- ENFORCED pNFTs: treat the asset's "enforced" royalty as negotiable like everything else (nobody pays unless a
  party chooses). TECH: program-allowlist rulesets block transfer by non-allowlisted programs ⇒ if AXTSWAP isn't
  allowlisted we CANNOT move the asset ⇒ grey out "unsupported" (a permission wall, separate from royalty).
- LAUNCH BAR: my adversarial suite + full regression IS the gate (no external audit required; audit recommended
  later for real value).

## 2. On-chain royalty term (the negotiated stance; NOT the destination)
Stored in the listing/swap state and in the sealed-order signed message. Destination/shares/bps are NOT stored —
they are read from the asset at settle.
- `royalty_mode: u8`  0=NONE, 1=MAKER, 2=TAKER, 3=SPLIT
- `maker_pct: u8`     0..100, only meaningful for SPLIT (maker funds this % of the royalty, taker funds the rest)
Placement:
- listing/swap state: extend SWAP_LEN (218) by 2 bytes → 220 (royalty_mode @218, maker_pct @219). Update every
  `len != SWAP_LEN` check + every writer (open_swap/open_pnft/open_core/list/list_pnft/list_core write 0,0 default).
- sealed-order signed message: append royalty_mode + maker_pct (167 → 169). Update verify_ed25519_order message
  reconstruction in settle_signed_order{,_pnft,_core} + the SDK/Kotlin order builders + goldens + maker signing.

## 3. Metadata sourcing — VERIFY LAYOUTS BEFORE CODING (do NOT parse from memory)
NEXT BUILD STEP. Dump a real account from localnet/mainnet + cross-check metaplex source; record exact offsets here.
- Token-Metadata `Metadata` acct (PDA ["metadata", TM_program_id, mint]) — LAYOUT CONFIRMED (mpl-token-metadata
  source; strings are borsh `String` = u32-LE len + bytes, NOT fixed-offset — old accts puff to max, new ones
  don't, so the parser MUST WALK them, never hardcode offset 319). Bulletproof parse algorithm (bounds-check EVERY
  read against data.len(); any overrun/bad-tag ⇒ REVERT):
    [0]      key u8        (should be 4 = MetadataV1; sanity-check)
    [1..33]  update_authority Pubkey
    [33..65] mint Pubkey
    o = 65
    name:   name_len   = u32le(data[o..o+4]); o += 4 + name_len
    symbol: sym_len    = u32le(data[o..o+4]); o += 4 + sym_len
    uri:    uri_len    = u32le(data[o..o+4]); o += 4 + uri_len
    sfbp:   u16le(data[o..o+2]); o += 2            (require sfbp <= 10000)
    creators: opt = data[o]; o += 1               (opt MUST be 0 or 1, else REVERT)
      if opt==1: vlen = u32le(data[o..o+4]); o += 4   (require vlen <= CAP=8 and o + vlen*34 <= len)
        each Creator (34 bytes): address[32] | verified u8(0/1) | share u8   (o += 34)
  Then: require Σ share == 100 (for the split); pay per share. verified flag drives the CLIENT's unverified-wallet
  notice (chain still pays per John's decision). CONTRACT GUARDS before parsing: passed acct == canonical metadata
  PDA for the give mint/asset AND acct.owner == TM_program_id (no spoofed/substituted metadata). Empirical
  confirmation: dump real mainnet Metadata accounts (single-creator + multi-creator + puffed + non-puffed) as
  FIXTURES in the adversarial suite — the parser must reproduce them byte-for-byte.
- Metaplex Core (NEEDS SOURCE/EMPIRICAL — docs are conceptual, NOT byte-precise): asset account holds plugins
  inline via PluginHeader -> PluginRegistry (RegistryRecords with per-plugin offsets) -> Plugin data. Royalties
  plugin = basis_points(u16?) + creators(Vec<{address:Pubkey, percentage:u8}>) + RuleSet enum (None/ProgramAllowList
  /ProgramDenyList). DO NOT parse from the conceptual docs — get the layout from the mpl-core source OR dump a real
  Core asset w/ a Royalties plugin and parse empirically, then mirror royalty.rs's TM parser (fail-closed, bounds-
  checked). STATUS: TM parser DONE+validated (covers the large majority of NFTs); Core parser = remaining, build it
  the same bulletproof way (separate parse_core_royalty fn + a Core arm in pay_want_and_royalty). Until then, Core
  list/open/order handlers should REJECT royalty_mode != NONE (client greys out royalty for Core assets).
- pNFT enforcement (NUANCED — VERIFY EMPIRICALLY, do not assume): pNFT token accts are frozen; the ONLY move is
  TM `Transfer`, which runs the RuleSet every time (owner OR delegate) — delegation does NOT skip it; our escrowless
  pNFT path already IS a TM delegate-transfer that hits the ruleset. BUT pass/block is ruleset-specific, and our
  wallet→wallet escrowless model (NFT never enters a program-owned escrow) PASSES rulesets that gate on
  program-owned-escrow destinations, while program-ALLOWLIST rulesets (check the calling program in-stack) BLOCK us
  regardless until AXTSWAP is allowlisted. ACTION: clone a real enforced-pNFT ruleset (e.g. Mad Lads-style) to
  localnet and actually run our escrowless TransferV1 through it + read the standard Metaplex royalty ruleset source;
  record the verdict here. Grey out ONLY assets we provably can't move. STRATEGIC: honoring royalties (this feature)
  is the qualification to get AXTSWAP onto those allowlists — the grey-out is "unsupported until listed", not permanent.
- ENFORCED-pNFT SETTLEMENT PATH (John's escape hatch — "to collect the offer funds, the maker must PAY with the NFT"):
  flip the authority. Instead of our program/delegate moving the NFT (gated), structure a MAKER-DELIVERS offer:
  the TAKER escrows funds naming the NFT; the MAKER claims them by OWNER-authorized TM Transfer of the NFT straight
  to the taker, atomically gated by the contract (funds release IFF NFT delivered, same tx). Owner-scenario TM
  transfers are permissive in the standard royalty ruleset (program-allowlist bites delegate/sale scenarios, not
  Owner) -> this moves enforced pNFTs WITHOUT AXTSWAP being allowlisted. Royalty layer rides the same atomic tx
  (honor royalties AND dodge the program-block). TRADE-OFF: maker must be online to sign the settle (accept-style,
  not a passive escrowless listing) -> design split: escrowless listing for normal NFTs (set-and-forget), maker-
  delivers offer for enforced pNFTs. Reuses the make_offer/accept_offer shape (maker-signed settle), NFT variant.
  VERIFY: that a given ruleset permits owner-auth transfer inside a program CPI w/ concurrent payment (some
  introspect the instructions sysvar to catch program-orchestrated owner sales) -- prove on localnet vs a real
  enforced ruleset before relying on it.
Fail-closed: if the metadata acct isn't the canonical PDA for the mint, or parse runs past the buffer, or
creators absent while a royalty term is set → REVERT.

## 4. Settle-time algorithm (in the NFT settle paths, when royalty_mode != NONE)
1. Verify the passed metadata acct is the canonical PDA for the give mint/asset (no spoofed metadata).
2. Parse sfbp (bps) + creators[] (addr, verified, share). Require Σ share == 100 (else revert).
3. royalty = want_amount * bps / 10_000  (u128 math; 0 ⇒ nothing to do).
4. Per mode: maker_share = (mode==MAKER)?royalty : (mode==SPLIT)?royalty*maker_pct/100 : 0 ;
   taker_share = royalty - maker_share. maker_share is skimmed from the want proceeds (maker nets less);
   taker_share is charged on top (taker pays want + taker_share). taker's max_want guard must include it.
5. Split EACH funded share across creators by share%, dust-safe: creator_i gets floor(share_i * src / 100); the
   LAST creator gets the remainder so Σ == src exactly. (Up to creators×{sources} transfers — cap creators (e.g.
   ≤5); if asset has more share>0 creators than the cap → unsupported/grey out, never partial-pay.)
6. All transfers atomic with the give+want legs. Missing/invalid creator ATA ⇒ whole settle reverts.

## 5. Scope (instructions touched)
Term written by: open_pnft, open_core, list, list_pnft, list_core (and open_swap for layout consistency, writes NONE).
Enforced at: fill_pnft, fill_core, fill_listing, fill_listing_pnft, fill_listing_core, settle_signed_order{,_pnft,_core}.
Fungible / no-royalty assets: term forced NONE; UI greys out.

## 6. Adversarial test matrix (the launch gate — must all pass)
- Parser: truncated/oversized metadata, lying string lengths, creators absent, Σshare ≠ 100, share>100, 0 creators,
  >cap creators, non-canonical metadata PDA (spoof), wrong mint, unverified creators present.
- Split math: 1..5 creators, indivisible amounts (dust), bps=0, bps=10000, want tiny/huge (u128 no overflow),
  maker_pct 0/50/100, every mode. Σ paid == royalty exactly; maker net + taker paid reconcile.
- Modes: NONE pays nothing; MAKER/TAKER/SPLIT fund correctly; required term with missing creator acct → revert.
- Negotiation: counter atomically rejects prior; no double-live offer; taker max_want includes taker_share.
- pNFT: allowlist-ruleset asset → unsupported (revert clean), not a silent skip.
- Regression: full 41-tag suite still green (royalty=NONE path == today's behaviour byte-for-byte).

## 7. Build order
(1) Verify metadata layouts (§3) → fill exact offsets here. (2) Term encoding (§2) + regen affected goldens.
(3) Parser module + dust-safe split (§3–4), heavily unit-tested in isolation first. (4) Wire into settle paths (§5).
(5) Adversarial suite (§6) + full regression. (6) Python SDK + Kotlin builders (variable creator accts) + goldens.
(7) Negotiation UI + the unverified-wallet notice. (8) Localnet e2e. Then deploy-ready (gated on John's funds).
