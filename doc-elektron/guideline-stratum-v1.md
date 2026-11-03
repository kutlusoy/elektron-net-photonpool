# Elektron Net: `elektron-net-photonpool` Stratum V1 Server Guideline

- **Version:** 0.1 (draft)
- **Date:** November 3, 2026
- **Audience:** Pool developers and operators, firmware integrators
- **Reference implementation:** [`elektron-net`](https://github.com/kutlusoy/elektron-net) - `src/validation.cpp` (`ValidateUTXOCheckpoint`), `src/rpc/mining.cpp` (`getblocktemplate`), and [`elektron-net-pool`](https://github.com/kutlusoy/elektron-net-pool) `src/models/MiningJob.ts`, `StratumV1Client.ts` for the proven wire layout - treat as ground truth for anything the doc references
- **See also:** [`guideline-daemon-core.md`](./guideline-daemon-core.md), [`guideline-photon-architecture.md`](./guideline-photon-architecture.md), [`mining-pool-integration.md`](https://github.com/kutlusoy/elektron-net/blob/main/doc-elektron/mining-pool-integration.md)

---

## 1. Wire layout

The coinbase is fixed by the attestation, so there is no extranonce space.

| Message | Content |
|---|---|
| `mining.subscribe` result | `[[["mining.notify","<session>"]], "", 0]`: empty `extranonce1`, `extranonce2_size = 0` |
| `mining.notify` | `coinb1` is the non-witness coinbase up to the end of the scriptSig, `coinb2` the rest (sequence, outputs, locktime). `coinb1 + coinb2` is exactly the coinbase, nothing is spliced in between. |
| `prevhash` | internal byte order with every 4-byte word reversed (Stratum convention) |
| `merkle_branch` | siblings of the coinbase, internal byte order |
| `version` | candidate version with the pool slice in bits 13 to 16 |
| `ntime` | candidate time plus the unit's nTime offset |

Pool work partitioning: every job carries a unit (candidate, nTime offset, version slice) that is handed to exactly one connection per epoch. Version bits 13 to 16 (`0x0001e000`) are the pool slice, bits 17 to 28 (`0x1ffe0000`) are negotiated to the miner with `mining.configure` (BIP 310), which is the only search space a miner has besides nonce and nTime.

## 2. Supported methods

| Method | Behaviour |
|---|---|
| `mining.configure` | Grants `version-rolling` with the mask intersected with `1ffe0000`; refuses it when no usable bits remain or `min-bit-count` cannot be met; answers every other extension with `false` |
| `mining.subscribe` | As above |
| `mining.authorize` | Account is `<bech32 address>.<worker>`. The address is checked for the configured network (`require_address`). Unknown formats get error 24. |
| `mining.suggest_difficulty` | Clamped to `min_difficulty` and `max_difficulty` |
| `mining.extranonce.subscribe` | Answered with `false` |
| `mining.submit` | `[worker, job_id, extranonce2, ntime, nonce, version_bits]`; header version is `(job_version & ~mask) | (version_bits & mask)` |
| `mining.set_difficulty`, `mining.notify` | Sent by the server |

Error codes: 20 other or invalid share, 21 job not found or stale, 22 duplicate, 23 low difficulty, 24 unauthorized.

## 3. Share handling

1. The job id (`epoch << 32 | unit number`) must be among the connection's last 16 jobs, otherwise `Job not found`.
2. A job from an older tip is `Stale share`. A job from an earlier epoch on the same tip is still valid.
3. Version bits outside the negotiated mask, nTime out of range and unknown slices are invalid.
4. A hash that meets the network target is a block even if it is below the share target. Otherwise it must meet the share target.
5. Duplicates are detected pool-wide per epoch by header hash.
6. A found block is assembled from the original candidate bytes and submitted asynchronously; success is logged as an event and listed in `/api/v1/blocks`.

## 4. Difficulty, jobs and efficiency

- Vardiff aims at one share per `share_secs` (default 10 s) per connection and adjusts after 30 s or 50 shares by at most a factor of 8 per step. After a change the previous target stays acceptable for 60 s.
- The hashrate estimate is derived from accepted share difficulty over a sliding window.
- A connection receives a new unit every `2^(32 + rolling bits) / hashrate * 0.8` seconds, clamped to `min_job_interval_ms` (default 200) and `max_job_interval_ms` (default 30,000), so the miner is not left without fresh search space.
- On a new tip every connection receives a `clean_jobs` notify immediately. All other job refreshes are non-clean.
- All candidate-specific parts of the notify message are rendered once per candidate; per job only the job id, version and nTime are formatted.

## 5. Protection against abuse

Line length is limited to 8,192 bytes, malformed messages (more than 10), failed authorizations (more than 10) and invalid shares (more than 100) close the connection. Idle connections are dropped after `idle_secs`, slow readers after a write timeout of 10 s. `max_connections` bounds the number of sockets. Not yet implemented: per-IP limits, banning, TLS.

## 6. Tests and measurements

`test/step09_stratum_v1.py` drives the server with a Python miner against an unmodified `elektrond` on regtest:

- Capability negotiation, extranonce advertisement, authorization of valid and invalid accounts.
- A rolled-version share that meets the network target becomes a block that the node accepts and that pays a pool payout script.
- Low-difficulty, stale, unknown-job, unauthorized-submit and oversized-line cases are rejected as specified.
- 60 concurrent connections receive 60 distinct units; all receive a clean job after a new tip.

`test/bench_stratum_fanout.py` with the synthetic load generator (`target/release/loadtest`) measured, on one 4-core machine that also runs the load generator, the node and the daemon (loopback, regtest, 64 candidates, a tiny mempool):

| Connections | Daemon memory | Clean job after a new tip (p50 / p99 / max) | Daemon CPU for the fan-out |
|---|---|---|---|
| 1,000 | 13 MB | 60 / 62 / 62 ms | 0.03 s |
| 5,000 | 42 MB | 101 / 113 / 113 ms | 0.12 s |
| 10,000, each sending 0.5 invalid shares per second | 79 MB | 90 / 116 / 116 ms | 0.16 s |
| 15,000 | 115 MB | 137 / 185 / 185 ms | 0.29 s |

All connections received the job in every run. The times include block generation in the node and the rebuild of all candidates through `getblocktemplate` (about 45 ms of it for 64 addresses), not only the fan-out itself. Memory is about 7 to 8 KB per connection. Limits of the test setup: the file descriptor limit of the container (20,000) and a single client address, hardware behaviour is NOT covered.

## 7. Known gaps

- No accounting, payout or persistence yet; shares are only counted.
- Not tested on real ASIC or firmware: acceptance of `extranonce2_size = 0` with an empty `extranonce1`, the job rate for fast devices, nTime rolling behaviour.
- Hobby firmware that rejects an empty `extranonce1` is not supported (the legacy pool has a special mode for it).
- Vardiff has only been exercised with synthetic traffic.
- No TLS, no per-IP limits.

## 8. Checklist

- [x] Notify layout verified by a block accepted by an unmodified node.
- [x] Version rolling with the negotiated mask.
- [x] Pool-wide unit uniqueness across connections.
- [x] Fan-out measured up to 15,000 connections.
- [ ] Hardware test with real ASICs of the target families.
- [ ] Share ledger, payouts and per-miner statistics.
- [ ] Per-IP limits, banning, TLS.
- [ ] Larger load test on separate hosts.

## 9. Open Questions

1. Which firmware families reject `extranonce2_size = 0` or an empty `extranonce1`.
2. Maximum job rate per device that stock firmware accepts without dropping work.
3. Whether a compatibility mode for hobby firmware is wanted.
