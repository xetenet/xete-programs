# xete-swap — on-chain events

The program emits a **public discovery event every time a public order is created**, so any indexer can pick up
new posts in real time with zero bespoke work. It uses the **standard Anchor `Program data:` log format**
(`sol_log_data` = 8-byte discriminator + Borsh fields), so `@coral-xyz/anchor`, Helius, Solscan, and Dune
decoders consume it directly from the IDL at [`idl/xete_swap_events.json`](idl/xete_swap_events.json).

## Scope — public posts only
Events fire **only** on the six public, rested order-create instructions. The private, off-chain-signed,
taker-targeted settlement path (`settle_signed_order` / `_pnft` / `_core`, tags 38–40) creates no rested account
and **emits nothing** — it stays confidential. Every field in the event is already public in the created order
account; the log is a convenience/notification signal, never a confidentiality boundary.

## `XetePostCreated`
- **discriminator** = `sha256("event:XetePostCreated")[..8]` = `[81,85,208,198,141,253,103,253]` (`0x5155d0c68dfd67fd`)
- **brand stamp:** field 0 is always ASCII `xete`; the event name namespaces the discriminator to xete; the
  emitting program id `AXTSWAPVuUP…` is the implicit origin.

Wire layout (198 bytes, all little-endian; Borsh of fixed-size fields = plain concatenation):

| offset | size | field | notes |
|-------:|-----:|-------|-------|
| 0   | 8  | discriminator | `5155d0c68dfd67fd` |
| 8   | 4  | magic         | ASCII `xete` |
| 12  | 1  | version       | schema version = 1 |
| 13  | 1  | kind          | see table below |
| 14  | 32 | maker         | order maker |
| 46  | 32 | post          | created order PDA — fetch for full state |
| 78  | 32 | give          | given asset mint (for Core: the asset address) |
| 110 | 32 | want          | requested asset mint |
| 142 | 8  | give_amount   | u64 |
| 150 | 8  | want_amount   | u64 |
| 158 | 8  | expiry        | i64 unix seconds |
| 166 | 32 | nonce         | order nonce |

### `kind`
| value | meaning | instruction |
|------:|---------|-------------|
| 0 | fungible escrow swap      | `open_swap` (0) |
| 1 | escrowless SPL listing    | `list` (26) |
| 2 | escrowless pNFT listing   | `list_pnft` (30) |
| 3 | escrowless Core listing   | `list_core` (34) |
| 4 | pNFT escrow swap          | `open_pnft` (18) |
| 5 | Core escrow swap          | `open_core` (22) |

## Consuming it
- **Helius webhook / Enhanced Transactions:** point a webhook at program `AXTSWAPVuUP…`; each matching tx carries
  the `Program data:` log. Match the 8-byte discriminator, then decode per the table.
- **Anchor:** `new BorshEventCoder(idl).decode(base64LogData)` → `{ name: "XetePostCreated", data: {...} }`.
- **Raw `logsSubscribe` / Geyser:** filter log lines starting `Program data:`, base64-decode, check bytes[0..8]
  == discriminator and bytes[8..12] == `xete`, then read fields at the offsets above.
