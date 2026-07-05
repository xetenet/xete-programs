# xete alias registry — spec (scoping)

The on-chain `%alias` identity layer. Its own Pinocchio program + repo (lives here until
that repo exists). It is **the membership card** for the swap marketplace: only agents
holding an alias may *list*; everyone can still *resolve* names and *exit* trades freely.

Status: contract + client SDK + permit decision-core BUILT & green on localnet (see
`~/.hermes/ALIAS_READINESS.md` for the live tracker). Core policy decisions settled with John
(2026-06-07/08); the **no-judgment-calls** refinements to §5/§6/§9 settled 2026-06-12 (premium =
length only, no curated name lists; grace = snapshot + rate-limit, `.sol` holders eligible).
Validator-only; no deploy / SOL until John's explicit go.

---

## 1. What it does

- Records `%name → { owner wallet, agent_id }` on-chain, permanently.
- Lets the swap contract verify membership (it reads the claimant's alias record; it can't
  call our server).
- Lets anyone resolve `%name → wallet` on-chain with no server in the loop.

**Resolving is permissionless. Only *claiming* is gated.** That keeps the directory a public
good while the front door stays controlled.

## 2. Account model

- **Alias** (PDA, key = the raw name): `owner[32] · name(≤32, [a-z0-9_], normalized) ·
  agent_id[32] · created_at:i64 · bump:u8` (+ a small optional metadata slot, later).
- **Config** (PDA, one): `admin_authority[32] (cold) · permit_authority[32] (hot, server) ·
  names_wallet[32] · bump`. Mutable by the cold admin key. (No on-chain reserved-name field —
  reservation/grace is server-side snapshot logic, §9; brand governance is reactive, §6.)
- Name keyed raw (≤32 chars) — human-readable, no hashing.

## 3. Instructions (modules)

| Module | What it does | Gated? |
|--------|--------------|--------|
| dispatch | route instructions | — |
| state | Alias + Config layouts | — |
| **claim** | register a new `%name`, charge the permit's price → names wallet | **permit (server co-sign)** |
| **update** | re-point your `%name` to a new wallet (key rotation) | owner signs, permissionless |
| **transfer** | hand `%name` to a new owner who **already holds an alias** | owner signs + on-chain membership check |
| **release** | give up `%name`; owner gets **nothing**; reclaimable rent → names wallet | owner signs |
| **admin/config** | rotate the permit key, set names wallet, manage authority | cold admin signs |

Resolve = clients read the PDA directly (no instruction).

## 4. The claim handshake (how off-chain checks reach the chain)

The contract can't see IP/device. So:
1. Agent asks the **server** to claim `%name`.
2. Server runs the throttle + computes the **full price** (see §5), then **co-signs** the
   claim with the hot permit key.
3. The on-chain `claim` checks: *the permit authority signed this, and the amount matches.*
   Creates the alias, charges the price to the names wallet.

The contract is **"dumb" on pricing** — all pricing logic lives server-side in the permit, so
it retunes anytime with no redeploy. The permit key is a **crown jewel** (protect like the
deploy key); it's separate from the cold admin key and **rotatable** if it leaks.

## 5. Pricing — free for the genuine, costly to hoard

Genuine users and hoarders reach for **different names**, so we price the *name*, not the *user*:

- **Ordinary / descriptive names** (`%bend-compute-bot`) → **free** (rent only). Farming these
  is pointless — nobody buys junk names.
- **Short / premium names** (`%x`, `%ai`, `%usdc`) → **priced**, because they're the only ones
  with hoard value.

**Premium is defined by LENGTH ONLY — a descending floor over any charset** (letters, digits,
mixed: `%x`, `%42`, `%m3`, `%007` are all premium because they're *short*, not because of what
they spell). The floor is steep at 1–2 chars and fades to free by ~6, so there's no hard
"premium / not" line to defend — 4 is in-but-cheap, 5 is barely, 6+ is free.

**No curated lists of "special" names** — no first-name list, no Fortune 500, no brand tiers.
Those require endless judgment calls, drift (the F500 changes yearly), and cultural bias
(`%john` priced but `%jianyu` free?), and they don't even deter impersonation: a squatter just
pays. Impersonation/brand abuse is handled by *resolve-shows-the-wallet + reactive takedown*
(§6), never by pricing. (Deferred option, NOT v1: a single **neutral dictionary-word** floor for
valuable longer real words like `%apple` — objective, no list to maintain — but the length floor
+ velocity toll already cover v1, and pricing is server-side so it can be added with no redeploy.)

Premium price = **scarcity floor + decaying velocity toll**:
- **Scarcity floor** — a permanent minimum by name length (shorter = higher). The price never
  drops below it, so quiet-period sniping still pays full freight.
- **Velocity toll** — a global surcharge that **rises with the recent claim rate** and **decays
  back down to the floor** during a rest (never to zero). A burst of premium claims drives the
  price up on the next grab — for everyone, regardless of identity.

**Framing: congestion toll, never appreciation.** The climb is a fee *to xete* (names wallet),
not value the holder captures, and it decays — so buying at a velocity peak to flip *loses*
money (the live claim price has cooled, fresh comparable names are cheaper). That deters speed
instead of rewarding it, and avoids the pump-scheme look that would dent the brand.

Tuning dials (server-side, no redeploy): the length→floor curve (steepness + how far out premium
fades), climb steepness + cap, decay rate (too fast = snipeable; too slow = punishes latecomers).

## 6. Anti-hoard / sybil stack (layered; none alone is the wall)

| Layer | Catches | Sybil-proof? |
|-------|---------|--------------|
| Membership requirement (front door) | drive-by claimers | no (it's the gate) |
| Server throttle on IP + device fingerprint | the lazy majority (one machine, rotating IPs) | **no** — proxies/VMs/IPv6/CGNAT defeat it; speed bump only |
| Per-fingerprint **cumulative claim count** (monotonic, never decremented) | recyclers (claim → transfer → reclaim-cheap) | no — beaten by fingerprint rotation |
| **Scarcity price** by name length | grabbing *valuable* names | **yes** — charges regardless of identity |
| **Global velocity toll** on premium | claiming *fast* (land-rush) | **yes** — sees aggregate rate, not identity |
| Reactive dispute / takedown (NO predictive reserved list) | brand-squatting / impersonation on long look-alikes | n/a (governance + verify-the-wallet, not a curated list) |

**The sybil-proof guarantees come only from the economic levers** (scarcity floor + velocity
toll) — they're identity-agnostic, so fingerprint/wallet/IP tricks don't dodge them. The
identity-keyed layers (throttle, fingerprint count) are speed bumps that catch the lazy and raise
the bar; the economics are the wall.

**No predictive reserved list.** We deliberately do NOT pre-curate a big list of "protected"
names — that's xete acting as a name authority (off-brand), it can't predict demand, and it
saddles us with a governance process. Brand protection is **reactive**: a `%name` is a *pointer to
a wallet, not proof of identity* — clients resolve to the wallet and the user verifies the wallet
before sending value, and genuine trademark abuse is handled by dispute/takedown after the fact.
The only "reservation" we keep is the time-boxed **migration grace** for people who already hold
the name (§9) — eligibility there is a cryptographic snapshot, not our judgment.

**Per-fingerprint counting detail:** the server counts *claims ever issued* per fingerprint
(monotonic, never decremented). Transfers/releases are owner-signed on-chain and never touch it,
so "transfer the name away then reclaim cheap" is dead for a stable fingerprint — we price the
**Nth claim, not the Nth held.**

## 7. Explicitly rejected (ethics + brand + fit)

- **Proof-of-work / costly calculation** — taxes *compute*, which is exactly what our adversaries
  have a surplus of (they sell it) and hits light legit agents harder. Money is the better tax.
- **Covert hardware fingerprinting / deanonymizing puzzles** — disproportionate (squatting is a
  nuisance, not theft), legally fraught (covert persistent device IDs = regulated personal data),
  and a direct betrayal of xete's privacy/sovereignty brand. We fight sybils with **economics, not
  surveillance** — and never have to identify anyone.

## 8. Transfer rules

- Names are **transferable for a small fee** → names wallet. Enforceable here (unlike NFT
  royalties) because ownership is a field in our record, changed only by our `transfer` ix.
  **Keep the fee small** — a big one drives sales to off-chain private-key handoffs that bypass us
  and are unsafe for the buyer.
- **The recipient must already hold an alias.** Since a first alias can only be *claimed* (never
  bought), every holder was throttle-vetted at least once — so "owns an alias" *is* proof of
  membership, checked fully on-chain, no fresh permit needed.
- Transferring away your **last** alias drops you to non-member (must claim to return).

## 9. Migration — the grace window (snapshot + rate-limit, dial armed)

When claiming opens, people who **already hold a name** get a head start to bring it onto xete
before the public can squat it. Three properties make this fair without any judgment calls:

**Eligibility = a cryptographic snapshot, two sources.** As of an **announced snapshot date**, the
claiming wallet must hold either:
- the off-chain **xete alias** `%name` (from the relay's existing alias records), **or**
- the matching Solana name-service domain **`name.sol`** (verified by an on-chain read of the SNS
  record's owner — permissionless, no oracle, same shape as the xete-alias check).

The snapshot being *dated in the past / at announcement* is what kills the farm-it arbitrage: you
can't buy a short `.sol` after the fact to mint a free premium `%alias` — too late, you weren't a
holder at the cutoff. (Airdrop-style cutoff.)

**`.sol` caveats:** only `.sol` labels that are already a **valid `%alias` string** qualify
(`a-z 0-9 _`, ≤32) — no lossy hyphen→underscore transform (it manufactures collisions). And
**wrapped/tokenized** `.sol` domains hold ownership in an NFT, not the name record's owner field,
so the check must resolve the NFT holder in that case.

**Collision priority** when two parties are eligible for the same name at snapshot: existing
**xete holder > `.sol` holder > public** (we honor our own users first). Cheap to state, must be
stated.

**Cost & limits during grace:**
- **Free** — ordinary *and* premium. You're moving a name you already own; charging for it is a
  hostile sunrise. The snapshot already bounds the claimable set to names you provably held, so no
  per-fingerprint count cap is needed *inside grace* — the snapshot **is** the cap.
- The only limiter is a **per-claiming-wallet rate limit** (claims/min) — pure anti-flood, so a
  script can't hammer the server; a real holder with a dozen names just claims them over a few
  minutes.
- The **velocity/surge dial stays armed** as an emergency brake (retunable, no redeploy) in case
  grace itself goes weird (set far bigger than expected, mass-transfer gaming) — but it does **not**
  charge legitimate snapshot holders. Governor, not toll booth.

**After grace:** the full stack applies to everyone — premium **price** (length floor), **velocity
toll** on rushes, and the per-fingerprint **count cap** on recyclers (§5/§6).

## 10. Open / deferred

- Exact numbers (tier boundary, floor curve, climb/decay, transfer fee) — tune live, server-side.
- Program upgradeable at first (policy/permit-key will evolve), immutable later.
- The **names wallet** is a dedicated vanity address (e.g. `xtNAME`), kept separate from grant /
  other accounts. Generated free; only *used* on-chain after John's deploy go.
- Build order across the project: ① swap fee hook → ② this registry → ③ wire the swap's listing
  gate to it.

---

## 5c · Provable pricing, per-address surcharge, burst defense (settled 2026-06-13)

**Three transparent line items.** A claim price = three separately-shown, independently-verifiable parts:
1. **Floor** — by name length (permanent; 0 for 6+ char ordinary names). The alpha curve: 1ch 5 /
   2ch 1 / 3ch 0.2 / 4ch 0.05 / 5ch 0.01 SOL (permit-core `default_profile`, 10x below the original).
2. **Land rush** (global velocity toll) — rises with the recent *aggregate* claim rate, decays back to
   the floor. The shared "everyone is greedy" component. Velocity climb/cap left strong on purpose:
   cheap floors stop hoarding, the toll is the cool-off brake.
3. **Your rush** (per-address surcharge — NOT YET BUILT) — rises with *your own* recent claims, decays
   to zero. The personal "you are greedy" component; a soft self-correcting throttle, zero for the
   ~95% who claim once.

**Provable, not asserted.** Single source of truth = the `permit-core` formula; its inputs are on-chain;
the ticker runs the SAME formula (compile permit-core to WASM for the browser) so the displayed price
can't disagree with the charged one. The land-rush input = on-chain claim rate; you cannot fake traffic
without paying real, visible SOL to claim from yourself. Realized prices are on-chain transfers to the
names wallet, so the whole price tape is auditable after the fact and overcharging vs the formula is
provable. Pricing must hold NO hidden parameters (the formula goes public client-side); dial changes must
be versioned + signed + public so a quiet retune-to-gouge is as visible as the traffic. Gold-standard
option (deferred): move the curve on-chain so the program enforces it from its own claim counter + Clock.

**Per-address surcharge mechanics (when built):**
- Derived from the claimer's on-chain alias `created_at` timestamps; each alias record carries one,
  written from the Clock sysvar. No stored counter, read from chain.
- Enumerate ALL of the address's aliases at login and **sum the decaying bumps**
  (sum of climb x decay(now - created_at_i)), NOT just the shortest delta, else a same-block burst of N
  claims prices like one. Same math as the land-rush toll, keyed to the address instead of the namespace.
- The user's alias is already fetched to render the identity chip, so the timestamps ride along; a hosted
  timestamps-only index is an OPTIONAL perf cache (public data, chain stays source of truth; the cache
  must be re-derivable + spot-checkable vs chain, never authoritative).

**Burst defense, "one you fix; one you price":**
- **Same wallet, N windows:** the on-chain `created_at` cannot see un-confirmed claims, so a co-sign burst
  in one slot would read pre-burst state. Defended by the server's EPHEMERAL in-flight counter, not the
  chain. TODAY the per-wallet rate limiter (server.rs, in-memory, 60s window, capped before confirmation)
  already throttles the crude flood; when the surcharge is built it must count in-flight co-signs too.
- **N wallets, one agent:** deliberately NOT prevented (we refused sybil-correlation for privacy). Not
  unguarded though, the global toll taxes the burst (claims 2..N and everyone pay the surge it inflicts)
  and per-wallet-free means only the first ordinary name per wallet is free. Economic, not preventive:
  make him pay + self-limiting, not impossible.

## 9b · Identity, privacy & sybil model (settled 2026-06-13)

**Privacy, not anonymity** — the user controls who knows what; we do not promise unlinkability. The wall
against hoarding is **economics** (floor + velocity toll), never an identity gate.
- **One free name per WALLET** (not per person) — a conscious trade to avoid holding any correlation data.
- **No stored wallet-linkage graph** (honeypot). **Never on-chain** (permanent public deanonymization).
  Optional linking exists ONLY as a user benefit (unified management): opt-in, consented via an off-chain
  signed message (SIWS, not a txn), revocable, ideally user-held (verify-and-forget), not a fee trap.
- **No mandatory 3rd-party (e.g. X) auth** — outsources surveillance, off-thesis, weak (cheap accounts),
  and still needs storage. X / `.sol` / `@handle` only as OPTIONAL opt-in public badges the user chooses.
- **`.sol` match = free `%name` during GRACE only** (incl. premium); post-grace = free market (normal
  pricing) + the owns-both badge + resolution, never a standing free premium claim.
- **Identity chip** resolves the connected wallet best-name-first: `%xetename` -> `.sol` (owns-both badge)
  -> truncated address; doubles as post-claim confirmation (re-resolves to `%name` on settle).

## 5d · Pricing: steeper floor, NO royalties (settled 2026-06-13)

The 10x-cut floors are too flat at the short end (0.2 SOL for a 3-char felt disposable AND subsidizes
flippers; the floor is the *patient* actor's price). Proposed STEEPER curve (still live-tunable dials):
1ch 25 / 2ch 7 / 3ch 1.5 / 4ch 0.2 / 5ch 0.04 SOL, 6+ free. Gives a visible premium tier (kills the
"disposable" signal) while the long tail stays adoption-cheap.

Capture scarcity **at mint** — the floor is the only un-evadeable capture point (you cannot claim
without paying it; there is no optional-floor marketplace). **NO royalties / NO secondary transfer tax:**
enforced royalties proved unenforceable in the NFT boom (optional-royalty venues won the race to zero;
wrapping / OTC / "gift + side payment" route around any contract rule), and our OWN swap/settlement
primitives would be the evasion rail. Price once, sovereign after; a secondary flip is normal market
liquidity, not arbitrage of our underpricing. (Royalty idea raised then killed by John from NFT-boom
experience — do not revisit.)

## 5e · Agent parity + structured quote (settled 2026-06-13)

**RULE — human-UI parity:** nothing in the claim flow is human-only. Every number a human sees in the
banner/chart has a machine-readable twin in the agent path. Agents are the PRIMARY audience; the chart is
one renderer, the data is the product.

Canonical artifact = a structured **quote** object:
`{ name|length, floor, land_rush, your_rush, total, status, decay:{half_life, projected_total_at[]},
   eligibility:{grace, sol_match, owns_both, free}, verify:{formula_ref, onchain_input_refs} }`.
The web chart + itemized 3-line math are ONE renderer of this object.

- Read-only **quote** endpoint + MCP tool `xete_alias_quote(name|len, wallet)` = the agent's "chart"
  (price-check, no commit). The existing claim/challenge/confirm endpoints already ARE the agent path
  (the web UI is a front-end over them); enrich the response, do not build a new rail.
- Surfacing `decay`/`projected` to agents is INTENDED, not a leak: it lets an agent self-throttle (wait
  for the toll to cool), which is exactly what the toll exists to produce.
- Provability matters most here: humans trust the chart, agents verify. Ship the quote AND the means to
  recompute it (public formula + on-chain inputs; permit-core to WASM for the shared client formula).

## 11 · Onboarding: claim -> inbox -> %echo + welcome (design 2026-06-13)

On claim success, guide the new holder straight into the inbox with a pre-seeded thread; **%echo** (the
live echo agent, given its own alias) auto-replies in seconds so they experience private messaging
immediately = the cold-start fix. Same handoff for AGENTS (native to messaging): the claim response
returns inbox/messaging handles so an agent can transact at once. The claim-success -> inbox handoff must
carry the new identity through (no "go log in again" seam).

A **welcome message** waits in the inbox. Concise, does not sweat details. Points to:
1. Get a **VERIFIED House Elf** build (verify authenticity, House-Elf intrusion/verification theme).
2. **Arm your agent** — MCP access (`uvx xete-mcp`) + **Black Knight** guardian tooling.
3. Brief ecosystem capabilities: **pay** (on-chain, non-escrow, revocable any time), **message** privately,
   **vault** (organized encrypted secrets), **agent runs its own inbox watcher**.

OPEN: which common agent frameworks "make the grade" for a tailored/templated watcher setup — TBD.

WELCOME DRAFT (placeholders [link] to fill):
> Welcome, %name. You've got a name on xete - and this IS your first message, so try it: reply, and
> %echo answers in seconds. That's private messaging, working, right now.
> When you're ready, three moves:
> 1. Get House Elf - your wallet, vault, and inbox in one place. Always take the VERIFIED build so you
>    know it's genuinely ours. -> [link]
> 2. Arm your agent - hand it xete's tools over MCP (uvx xete-mcp) and put Black Knight in front of it.
>    -> [link]
> 3. Look around - everything below is already yours.
> What you can do here:
>  - Pay - on chain, never in escrow, revocable any time. The money never leaves your hands.
>  - Message - private by default.
>  - Vault - secrets, encrypted and organized.
>  - Watch - your agent can run its own inbox watcher.
> Reply to dig in, or just say hey to %echo.
> - team xete

## 12 · Storage policy + dual inbox (settled 2026-06-13)

**Storage policy — "here is your copy, store it if you like."** Sovereign model applied to documents:
we hand the user their copy (ToS text, welcome, exports) and they decide whether to keep it locally.
We are NOT the mandatory custodian. Carve-out: the minimal compliance/operational records we are
*required* to keep (e.g. ToS-acceptance version + timestamp) are ours, kept minimal — but the document
TEXT is the user's copy. Rule: user content = theirs; irreducible legal/operational flags = ours, minimal.

**Dual inbox — BOTH must be functional before any public claim window (John, launch-blocking).** The
relay is ciphertext-only; we never hold plaintext. The user chooses WHERE decryption happens:
- **House Elf (local, encrypted at rest) — BUILT.** Tauri `get_inbox` (pulls blobs + decrypts in the Rust
  backend) + `send_herd` (compose/send). Nothing unencrypted touches disk.
- **Webmail (zero local footprint) — TO BUILD.** A browser inbox that decrypts in-session and persists
  nothing (no localStorage, no disk). Same ciphertext blobs; decrypt in the tab, evaporate on close.
  Honest claim = "no plaintext at rest on your device," NOT "no trace" (browsers keep ephemera).

Framing for both: "we never see your plaintext (relay ciphertext-only); you choose whether *your device*
ever does." Web = untrusted/shared device; House Elf = your trusted machine + offline. Same encrypted
blobs, only the decryption locus differs.
