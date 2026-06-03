# Translation: lean (deployed) ↔ readable reference

Two builds implement the **same** contract — identical wire format, identical
PDA derivation, identical on-chain state layout — so one client and one test
suite drive both. The lean build is what runs on-chain (minimal compute); the
readable build exists so the logic is easy to audit.

| Concern | `program/readable/` (solana-program) | `program/lean/` (Pinocchio, deployed) |
|---|---|---|
| Stack | `solana-program` + `borsh` | `pinocchio` (no_std) + `solana-nostd-sha256` |
| Entrypoint | `entrypoint!` (std panic handler) | `program_entrypoint!` + `default_allocator!` + `nostd_panic_handler!` |
| Account type | `AccountInfo` | `AccountView` |
| Pubkey type | `Pubkey` | `Address` |
| Instruction decode | `borsh` enum `SettlementInstruction` | manual: first byte = tag, fixed-offset payload |
| State (de)serialize | `borsh` `Settlement` struct | manual reads/writes at byte offsets `O_*` |
| Hash | `solana_program::hash::hashv` (SHA-256) | `solana_nostd_sha256::hashv` |
| Create account (CPI) | `system_instruction::create_account` + `invoke_signed` | `pinocchio_system::CreateAccount.invoke_signed` |
| PDA seed | `b"escrow"` (`SEED`) | `b"escrow"` (`SEED`) — same |
| Logging | `msg!` (`XETE_SETTLE_*`) | none (lean: no logs) |

## State layout (both, 81 bytes)

```
[ 0..32 ] depositor               Pubkey
[32..40 ] amount                  u64 LE
[40..72 ] beneficiary_commitment  [u8;32]   = SHA256(beneficiary_pubkey ‖ salt)
[72..80 ] unlock_time             i64 LE
[80     ] bump                    u8
```

The readable build encodes this with borsh; the lean build writes the same
bytes by hand at the offsets `O_DEPOSITOR=0`, `O_AMOUNT=32`, `O_COMMIT=40`,
`O_UNLOCK=72`, `O_BUMP=80`. The results are byte-identical, which is why the
same `settlement_id`, commitment, and salt produce the same PDA and the same
claim check on either build.

## Instruction wire format (both)

```
deposit : [0] ‖ settlement_id[32] ‖ amount(u64 LE) ‖ commitment[32] ‖ unlock(i64 LE)
claim   : [1] ‖ settlement_id[32] ‖ salt_len(u32 LE) ‖ salt[salt_len]
reclaim : [2] ‖ settlement_id[32]
```

See [SPEC.md](SPEC.md) for accounts and semantics.
