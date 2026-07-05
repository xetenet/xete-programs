# xete-tab — non-custodial confidential settlement for Solana

A small, non-custodial Solana program that settles value from one party to a
**hidden beneficiary**: confidential while the transfer is pending, then fully
on the record and auditable the moment it is claimed. It is a **reusable
settlement primitive** other developers can build on — not an app, not a
service, and not a custodian.

It is the value-transfer half of [xete](https://xete.net), the open protocol
that gives AI agents a Solana wallet they can use as both an encrypted inbox
and a custody-free way to pay.

---

## Status: live and immutable on mainnet

| | |
|---|---|
| **Program** | [`GPCsJ6kvrQ61wDG8bpP8315ge7AHfmsUHdxTD7LQ6CoJ`](https://explorer.solana.com/address/GPCsJ6kvrQ61wDG8bpP8315ge7AHfmsUHdxTD7LQ6CoJ) |
| **Upgrade authority** | **relinquished** — the program is immutable; no one (including us) can change, seize, freeze, or redirect it |
| **First settlement — deposit** | [`4zAVuxHQ…PSyqig`](https://explorer.solana.com/tx/4zAVuxHQ3ve3NkzTbr1Nvb4AAUEXoKo5ZXkX45VegGy5cXmhoWVR724aMtZrimhqErU9SA4Eq2GxxJrrcAPSyqig) |
| **First settlement — claim** | [`5fwM657…c4MM7`](https://explorer.solana.com/tx/5fwM657mN3n3LXbMeGSttmUG3N147sHcmn775i3kZ92Afrx3iVGStXMnSyVzpD39t6H3L7e3mz8Sb4zP4iTc4MM7) |
| **License** | MIT |

---

## How it works

Three instructions: **deposit**, **claim**, **reclaim**.

1. **Deposit.** A depositor funds a program-owned account (a PDA) with the
   amount to settle. The account stores a **commitment hash** —
   `H(beneficiary_pubkey ‖ salt)` — *not* the beneficiary's public key. So the
   account exists publicly, with a visible depositor and amount, but **who the
   funds are for is hidden on-chain.** The salt is shared with the beneficiary
   off-chain (over xete's end-to-end-encrypted channel).

2. **Claim.** The beneficiary proves they are the intended recipient by
   submitting the salt: the program checks `H(claimant ‖ salt) == commitment`.
   On success the account is closed and its entire balance — **principal plus
   the rent reserve** — is swept to the beneficiary.

3. **Reclaim.** Until a claim happens, the original depositor can take the
   funds back. The money never leaves the depositor's control until the
   recipient accepts it.

### Rent control

The account that briefly holds the transfer is **closed the instant it is
claimed (or reclaimed)**, returning its rent deposit along with the funds. No
SOL is ever stranded on-chain. Net of the reclaimed rent, a settlement costs
only a sliver of network fee. And because the rent keeps the account
rent-exempt while it waits, a pending settlement can sit on-chain as long as it
needs to — minutes or months — with no expiry and no decay.

---

## What this is — and isn't

- **A primitive, not a service.** This program is a neutral building block. It
  provides a non-custodial transfer that settles on acceptance; what two
  parties choose to do with it is up to them.
- **Non-custodial.** Only the depositor's and the beneficiary's own keys move
  funds. The program can never release on its own, and with the upgrade
  authority relinquished, no operator can intervene.
- **Not a mixer.** Each settlement is a discrete, on-chain `A → PDA → B`
  account. The deposit↔claim link is permanent and public; only the
  beneficiary is concealed, and only until claim. Existence, amount, and
  depositor are always visible.
- **Confidential, then auditable.** This mirrors how high-value commerce
  already works — a deal is arranged privately and disclosed once it closes.

---

## Repository layout

```
program/
  lean/        Pinocchio (no_std) build — this is the build deployed on-chain
  readable/    solana-program build — a human-readable reference of the same logic
tests/
  test_settlement.py   adversarial + positive-control suite (18 checks)
SPEC.md        wire format, account layout, instruction-by-instruction spec
TRANSLATION.md lean ↔ readable, side by side
SECURITY.md    security contact + disclosure
```

Two builds, identical wire format and PDA derivation, so the same client and
the same test suite drive both. The **lean** build is what runs on-chain
(minimal compute); the **readable** build exists so the logic is easy to audit.
A reproducible `solana-verify` build is on the roadmap.

## Build

```bash
# lean (deployed) build
cd program/lean && cargo build-sbf

# readable reference build
cd program/readable && cargo build-sbf
```

## Test

Deploy the program to a local validator, then:

```bash
python tests/test_settlement.py <PROGRAM_ID> http://127.0.0.1:8899
```

---

## A note on the PDA seed

The on-chain PDA seed is the literal byte-string `escrow` — a fixed internal
constant from this contract's first deployment. It is load-bearing: the live,
immutable program derives every account from it, so it cannot change. The
protocol and product are **settlement / non-custodial**; this 6-byte seed is
the one internal place an older name survives, and any client integrating with
the live program must use it. See [`SPEC.md`](SPEC.md).

---

Part of **xete** · [xete.net](https://xete.net) · `uvx xete-mcp`
