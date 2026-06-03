# xete-tab settlement contract — specification

A non-custodial confidential-settlement program for Solana. This document is
enough to build a compatible client without reading the source.

- **Program (mainnet, immutable):** `GPCsJ6kvrQ61wDG8bpP8315ge7AHfmsUHdxTD7LQ6CoJ`
- **Deployed build:** `program/lean/` (Pinocchio, no_std). `program/readable/`
  is a byte-for-wire-compatible reference.

---

## PDA derivation

Every settlement lives at a program-derived address:

```
PDA = find_program_address([ SEED, settlement_id ], program_id)
SEED = b"escrow"            # 6 bytes, fixed, load-bearing — see note below
settlement_id = 32 random bytes chosen by the depositor
```

`settlement_id` is a caller-chosen random 32-byte value — **not** the
beneficiary — so the address itself never reveals who the funds are for.

> **Note on `SEED`.** The on-chain seed is the literal byte-string `escrow`, a
> fixed internal constant from this contract's first deployment. The program is
> immutable, so it cannot change; clients **must** use it exactly. The protocol
> and product are "settlement / non-custodial" — this seed is the one place an
> earlier internal name persists, and it carries no functional meaning beyond
> being the derivation constant.

---

## Account state (81 bytes)

Fixed-size. The readable build borsh-encodes this struct; the lean build writes
the same layout by hand at these offsets:

| Offset | Field | Type | Meaning |
|-------:|-------|------|---------|
| 0  | `depositor`              | `[u8;32]` (Pubkey) | funded it; the only key that may reclaim |
| 32 | `amount`                 | `u64` LE           | lamports deposited |
| 40 | `beneficiary_commitment` | `[u8;32]`          | `SHA256(beneficiary_pubkey ‖ salt)` |
| 72 | `unlock_time`            | `i64` LE           | unix seconds; `0` = claimable immediately |
| 80 | `bump`                   | `u8`               | PDA bump |

---

## Instructions

Instruction data is `[tag: u8] ‖ payload`. Tags: `0` deposit, `1` claim,
`2` reclaim.

### 0 — Deposit
**Accounts:** `[signer, writable] depositor`, `[writable] settlement_pda`,
`[] system_program`
**Payload (80 bytes):** `settlement_id [32] ‖ amount u64 LE ‖ commitment [32] ‖ unlock_time i64 LE`

Creates the PDA via the system program, funded with `amount`. `amount` **must**
clear rent-exemption for an 81-byte account — that floor is the minimum send
(the rent is borrowed from the send and returned on close). Fails if the PDA
already exists (`AccountAlreadyInitialized`) or the address doesn't match the
seeds (`InvalidSeeds`).

### 1 — Claim
**Accounts:** `[signer, writable] beneficiary`, `[writable] settlement_pda`
**Payload:** `settlement_id [32] ‖ salt_len u32 LE ‖ salt [salt_len]`

The signer proves they are the hidden beneficiary:
`SHA256(signer_pubkey ‖ salt) == beneficiary_commitment`. If `unlock_time` is in
the future, claim is rejected until then. On success the account is **closed**:
its entire balance (principal + rent) is swept to the beneficiary and its data
zeroed.

### 2 — Reclaim
**Accounts:** `[signer, writable] depositor`, `[writable] settlement_pda`
**Payload:** `settlement_id [32]`

Only the original `depositor` may reclaim, and only while the settlement is
still open (no claim has closed it). Closes the account back to the depositor.
There is **no time lock on reclaim** in this version — the depositor keeps
control until the beneficiary claims.

---

## Confidentiality model

- **Hidden until claim:** the chain stores only `SHA256(beneficiary ‖ salt)`.
  The salt is delivered to the beneficiary off-chain (over xete's
  end-to-end-encrypted channel). The beneficiary's identity is revealed on-chain
  only when they sign the claim.
- **Public always:** the settlement's existence, its `amount`, and the
  `depositor` are visible from the moment of deposit. The deposit↔claim link is
  permanent and public. This is **not** a mixer — there is no pooling and no
  broken link, only a concealed-until-accepted recipient.

## Errors (selected)

`InvalidInstructionData`, `NotEnoughAccountKeys`, `MissingRequiredSignature`,
`InvalidSeeds`, `IllegalOwner`, `AccountAlreadyInitialized`, `InsufficientFunds`
(sub-rent deposit), `InvalidArgument` (commitment mismatch or still locked),
`ArithmeticOverflow`.
