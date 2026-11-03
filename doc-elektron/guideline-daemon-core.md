# Elektron Net: `elektron-net-photonpool` Daemon Core Guideline

- **Version:** 0.1 (draft)
- **Date:** November 3, 2026
- **Audience:** Pool developers and operators
- **Reference implementation:** [`elektron-net`](https://github.com/kutlusoy/elektron-net) - `src/rpc/mining.cpp` (`getblocktemplate`), `src/validation.cpp` (`ValidateUTXOCheckpoint`), `src/node/miner.cpp` - treat as ground truth for anything the doc references
- **See also:** [`guideline-photon-architecture.md`](./guideline-photon-architecture.md), [`guideline-batch-attestation-rpc.md`](./guideline-batch-attestation-rpc.md), [`guideline-admin-api-and-ui.md`](./guideline-admin-api-and-ui.md)

---

## 1. Scope of this version

`photond` is written in Rust (no garbage collector, predictable latency, native SHA-256 with midstate). This first core connects to an unmodified node and implements everything up to the miner-facing protocols:

| Module | Role | Status |
|---|---|---|
| `rpc.rs` | Async JSON-RPC client with keep-alive, cookie or user:password auth | done |
| `addr.rs` | bech32 payout address to script, prefix checked per network | done |
| `tx.rs` | Coinbase serialisation, coinbase merkle branch and root | done, unit tested |
| `template.rs` | Immutable candidate from `getblocktemplate`, header, block assembly, nonce search with midstate | done, unit tested |
| `pool.rs` | Epoch per tip, lock-free unit allocator, share validation with duplicate filter, tip loop | done, unit tested |
| `api.rs` | Read-only `/api/v1` (summary, miners, blocks, events) and static UI | done |
| `stratum.rs`, `diff.rs` | Stratum V1 server: subscribe, authorize, notify, submit, version rolling, vardiff, pre-rendered notify, session statistics | done, see [`guideline-stratum-v1.md`](./guideline-stratum-v1.md) |
| Stratum V2 frontend | Noise, standard channels | not yet |
| Batch attestation backend | Candidates for all payout scripts in one node call (Photon node patch, `pool.backend = auto`, falls back to `getblocktemplate`) | done, see [`guideline-batch-attestation-rpc.md`](./guideline-batch-attestation-rpc.md) |
| `ledger.rs`, `accounting.rs` | SQLite ledger, PPLNS and PPS accounting, block maturity, payout worker, wallet address provisioning | done, see [`guideline-payout-modes.md`](./guideline-payout-modes.md) |
| `public.rs` | Public pool page and read-only API with rate limiting | done, see [`guideline-public-page.md`](./guideline-public-page.md) |

Mainnet is far beyond height 290000, so only the MuHash attestation path matters. The daemon refuses to start on a node below the intra-block attestation fix height (`170000` on mainnet).

## 2. Behaviour

1. At start the daemon checks node chain and height and decodes all payout addresses for the configured network. Wrong prefix or wrong chain is a hard error.
2. The tip loop polls `getbestblockhash` (default every 100 ms). On a new tip, or after `refresh_secs`, it builds an epoch: one `getblocktemplate` call per payout address, one immutable candidate each. The coinbase is built locally from the template fields and the candidate carries the node-provided attestation.
3. A configurable number of candidates per epoch (`spot_check`) is validated with `getblocktemplate mode=proposal`. A candidate the node rejects is dropped and counted, fail closed. If none is left no epoch is published.
4. An epoch is published atomically. Each unit (candidate, nTime offset, version slice) is handed out once per epoch; wrapping the space is counted as overlap and shown in the allocator pane.
5. Version bits 13 to 16 (`0x0001e000`) carry the pool slice, bits 17 to 28 (`0x1ffe0000`) are left to the miner. Shares with other version bits changed are invalid.
6. A share is checked for candidate, slice, version bits, nTime range, share target, duplicate (hash prefix per epoch) and network target.
7. A block is assembled from the original candidate bytes and submitted with `submitblock`. Acceptance is verified against the new best block hash.

## 3. CLI and configuration

```text
photond -c photond.toml run              daemon, admin API and UI
photond -c photond.toml check            build and fully validate every candidate once
photond -c photond.toml check --mine     additionally solve and submit one block (regtest only use)
```

```toml
[node]
rpc_url = "http://127.0.0.1:8332"
auth = "user:password"        # or "cookie:/path/to/.cookie"
poll_ms = 100

[pool]
network = "mainnet"           # mainnet, testnet, regtest
payout_addresses = ["be1q..."]
refresh_secs = 20             # 0 disables periodic rebuilds on an unchanged tip
spot_check = 4                # candidates validated via proposal per epoch

[stratum]
listen = "0.0.0.0:3333"
initial_difficulty = 1024
share_secs = 10
max_connections = 200000

[api]
listen = "127.0.0.1:8081"
ui_dir = "ui"
```

The `[stratum]` options are described in [`guideline-stratum-v1.md`](./guideline-stratum-v1.md).

## 4. Tests and measurements

`test/step08_photond_core.py` runs the daemon against an unmodified `elektrond` on regtest (build with `cargo build --release`, point `PHOTOND` at the binary):

- All candidates are validated by the node; three consecutive blocks solved from pool candidates are accepted and pay pool payout scripts.
- Wrong address prefix and wrong chain are refused.
- The running daemon serves the UI, follows a new tip, rebuilds the epoch, records the tip-to-ready latency and streams events.

Measured on regtest with an empty mempool and a tiny UTXO set (lower bound, NOT representative of mainnet):

| Payout addresses | Candidate fetch via `getblocktemplate` |
|---|---|
| 8 | 8 ms |
| 50 | 44 ms |
| 200 | 162 ms |
| 430 | 318 ms (all 430 validated by the node) |

The cost per address grows with mempool size and UTXO set on mainnet. This backend is therefore the bootstrap path; the batch interface of the batch RPC guideline replaces it. Unit tests cover merkle branch against a full tree, varint boundaries, target comparison, allocator uniqueness and share checks.

## 5. Next steps

1. Hardware tests of the Stratum V1 server and payouts on real devices and mainnet.
2. Photon protocol (see [`guideline-photon-protocol.md`](./guideline-photon-protocol.md)).
3. Tip notifications through the node mining IPC interface instead of polling.
4. Stratum V2 standard channels.

## 5.1 Planned tests

- **Load simulation** (`test/sim_full_blocks.py`, done on one machine over loopback, regtest): 11,000 transactions per block (3.12 MWU), 25,000 extra UTXOs, 10,000 simulated miners, 430 payout addresses, two rounds.

  | Measurement | Result |
  |---|---|
  | `getblocktemplate` | 629 ms |
  | `getattestationbatch`, 430 candidates | 828 ms |
  | Epoch build, `gbt` backend | 190 s |
  | Epoch build, batch backend | 576 ms |
  | Block request to clean job at miner | p50 838-870 ms, max 894 ms (includes ~770 ms node block generation) |
  | Miners receiving the job | 10,000 / 10,000 |
  | Daemon RSS | 97-104 MB |

  The `gbt` backend is unusable with full blocks; the batch backend is required. Not covered: real network, ASIC firmware, mainnet UTXO set size, ledger write rate under share load.

## 6. Checklist

- [x] Candidates built and validated against an unmodified node.
- [x] Blocks solved from candidates accepted on regtest.
- [x] Unit allocator uniqueness test.
- [ ] Mainnet-sized benchmark of candidate production.
- [x] Stratum V1 frontend.
- [ ] Hardware tests.

## 7. Open Questions

1. Mainnet cost of one `getblocktemplate` call per address with a realistic mempool.
2. Number of payout addresses the first production deployment needs (see the sizing table of the architecture guideline).
