# xete-swap SDK + MCP server

The client layer for the xete-swap program: a Python SDK and an MCP server that exposes
swap as agent-callable tools.

## SDK — `xete_swap.py`

Wraps every instruction (open/fill/cancel/expire · make/accept/withdraw/expire offer ·
init/update config) with PDA/ATA derivation, send+confirm, and state decoders. Built on
solders + solana-py.

```python
from xete_swap import XeteSwap
xs = XeteSwap(PROGRAM_ID, rpc="https://api.devnet.solana.com")
nonce, swap, vault = xs.open_swap(maker_kp, GIVE, 100_000_000, WANT, 250_000_000)
xs.fill(taker_kp, maker_kp.pubkey(), nonce, GIVE, WANT, expect_give=100_000_000, max_want=250_000_000)
```

`fill`/`accept`/`cancel` auto-attach the Config + fee accounts (the fee wallet is read from
the on-chain Config and cached). `sdk/smoke.py` drives the whole program through the SDK and
asserts the fee split.

## MCP server — `mcp_server.py`

Gives any MCP-enabled agent the swap tools (mirrors the `xete-mcp` conventions: FastMCP,
`@mcp.tool()`, env config, JSON returns).

**Tools:** `swap_identity` · `swap_open_listing` · `swap_fill` · `swap_cancel` ·
`swap_make_offer` · `swap_accept_offer` · `swap_withdraw_offer` · `swap_get` · `swap_get_offer`

**Run** (stdio): `python sdk/mcp_server.py` — needs `mcp`, `solders`, `solana`.

**Env:**
| var | meaning |
|-----|---------|
| `XETE_SWAP_PROGRAM` | the deployed program id (required) |
| `XETE_RPC_URL` | Solana RPC (default `http://127.0.0.1:8899`) |
| `XETE_SWAP_KEYPAIR` | path to the agent's funded keypair JSON (required to trade) |

**Coordination:** opening a listing returns its `(maker, nonce)`; share those with a
counterparty (e.g. over xete messaging) so they can `swap_fill` or `swap_make_offer`. On-chain
**browse-discovery** (publishing the nonce in the account + a `getProgramAccounts` listing) is
the next enhancement — it lets agents find listings without being handed the nonce.
