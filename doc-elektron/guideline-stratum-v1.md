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
- A connection receives a new unit every `2^(32 + rolling bits) / hashrate * 0.8` seconds, clamped to a shortest interval (`min_job_interval_ms`, default 200 ms, and never so short that all connections together exceed `max_jobs_per_sec`, default 50,000: 15,000 connections get 300 ms) and `max_job_interval_ms` (default 30,000), so the miner is not left without fresh search space. Measured on a Bitaxe (BM1370, expected 1.07 to 1.2 TH/s): with 200 ms and more between jobs it hashed at 0.6 to 0.8 TH/s; with a lower minimum (jobs every 90 to 120 ms, 25 ms floor) the device-reported rate fell to 0.28 to 0.4 TH/s and kept falling. Each fresh job is a clean job for ESP-Miner devices and makes the chip drop its work, so more jobs are not better. The optimum is not known; do not go below 200 ms without measuring on the device. The rate used is the measured one but at least 60 % of the typical rate of the device type, because a miner that is starved of work reports less than it can do.
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

### Steady-state load with Bitaxe-style miners

A Bitaxe (ESP-Miner) is given a clean job every 200 ms (see `guideline-release-and-bitaxe-test.md`, section 8a), five jobs per second and connection. `test/bench_bitaxe_profile.py`
measures that load with synthetic miners on one 4-core machine that also runs the load generator, the node and the daemon (loopback, regtest, 64 candidates, `refresh_secs = 20`, 20 s window):

| Connections | Daemon memory (before / after) | Jobs per second | To the miners | Daemon CPU | Clean job after a new tip (p50 / max) |
|---|---|---|---|---|---|
| 1,000 | 19 / 21 MB | 4,950 | 21 Mbit/s | 5 % of one core | 28 / 30 ms |
| 5,000 | 58 / 68 MB | 24,749 | 105 Mbit/s | 25 % of one core | 52 / 62 ms |
| 15,000 | 153 / 183 MB | 74,435 | 316 Mbit/s | 85 % of one core | 74 / 132 ms |

The network is the limit before the CPU is. The work units are used up much faster than with the earlier one job per 15 to 30 s: one candidate offers 16 slices x 60 ntime steps = 960 units per epoch,
so with five jobs per second per miner and a 20 s epoch one candidate serves about ten Bitaxes, and the 64 candidates of the benchmark about 600 (the allocator reported 38,560 overlapping units at 1,000 and 438,560 at 5,000 connections;
the reading at 15,000 came right after an epoch change and says nothing). Overlapping units mean two miners get the same header, which shows up as duplicates between miners. 15,000 Bitaxes need about 1,600 candidates.

### Self-adjusting pool (per connection and work space)

Two things adapt to the load now:

- **Job interval per connection** (`[stratum] adaptive_jobs`): the pool counts the duplicate shares and the shares for jobs it no longer knows (not the ones after a new block) of each connection. More than a tenth of them: the
  interval shrinks by 40 % (down to `adaptive_min_interval_ms`, default 50 ms, and not below the budget limit above) and, if that does not help, jobs become clean for that connection. Fewer than two in a hundred: it grows by 15 % per round again, never above the interval that the hash rate
  estimate calls for. The chosen interval is shown per miner (column JOB ms) and in `/api/v1/miners` as `jobMs`.
- **What the pool knows per device type** (`src/devices.rs` `profile_key`, table `device_learned` in the ledger, operator API `/api/v1/learned`): the key is the chip for ESP-Miner devices (`bitaxe/bm1370`), else the software name. A new connection starts with what the pool learned from earlier connections of the same type once it has at least three observations (typical hash rate, learned job interval, whether the type needs clean jobs; where the chip has a typical rate, the higher of the learned and the typical rate is used, because a measured rate can be low when a miner was starved or lost shares), else with a typical hash rate of the chip (BM1370 1.2 TH/s, BM1368 0.7, BM1366 0.5, BM1397 0.4; only starting values), else with the configured defaults. That sets the first share difficulty (about one share per `share_secs`), the job pace and the clean flag; the measurement of the connection takes over from the first shares on. Once a minute the median of the connections of a type (with at least 20 shares each) is folded into the stored values with a slow moving average, so one odd connection changes little. Rows not updated for `ledger.stats_keep_days` are removed.
- **Candidates in use** (`[pool] autoscale_candidates`, `min_candidates`): the pool measures how fast the work units of the last epoch were handed out (one unit per job) and builds the next epoch with
  `ceil(units per second x epoch seconds x 1.25 / 960)` candidates, between `min_candidates` (default 16) and the number of configured addresses. It grows at once and shrinks slowly. The node builds and attests only the candidates in use.

`test/bench_bitaxe_profile.py` with `AUTOSCALE=true ADDRS=2000 CONNS=...` (batch node, 20 s epochs, one 4-core machine for everything):

| Connections | Candidates in use after the first epochs | Overlapping units | Node time to build all candidates of an epoch | Daemon memory | Daemon CPU |
|---|---|---|---|---|---|
| 5,000 | 651 (of 2,000) | 0 | not sampled | 77 MB | 30 % of one core |
| 15,000 | 1,940 (of 2,000) | 0 | 90 to 109 ms | 205 MB | 85 % of one core |

Without autoscaling the same load with 64 candidates overlapped by 438,560 units at 5,000 connections. With a node without the batch RPC every candidate costs one `getblocktemplate` call (about 0.7 ms),
1,900 candidates then take over a second per epoch: use a node with the `getattestationbatch` RPC (branch `photonpool`) for pools of this size.

### Dynamic load: miners joining and leaving

`test/bench_dynamic_growth.py` (batch node, 2,000 addresses, 20 s epochs, one 4-core machine for the daemon, the load generator and the node): 1,000 simulated Bitaxes at t = 0, 4,000 more after 40 s, 10,000 more after 80 s, then the groups leave one by one.

| Time | Miners | Candidates in use | Demand (units/s) | Overlapping units | Node time per epoch | Daemon memory | Daemon CPU |
|---|---|---|---|---|---|---|---|
| 6 s | 1,000 | 115 | 4,395 | 0 | 6 ms | 21 MB | 7 % of one core |
| 54 s | 5,000 | 657 | 25,224 | 0 | 35 ms | 70 MB | 29 % |
| 104 s | 15,000 | 1,950 | 74,873 | 0 | 117 ms | 191 MB | 84 % |
| 134 s | 14,000 | 1,946 | 74,707 | 0 | 111 ms | 192 MB | 92 % |
| 209 s | 222 | 1,854 | 71,193 | 0 | 97 ms | 209 MB | 63 % |
| 269 s | 0 | 954 | 36,611 | 0 | 47 ms | 192 MB | 1 % |
| 299 s | 0 | 239 | 9,153 | 0 | 12 ms | 188 MB | 1 % |
| 359 s | 0 | 30 | 1,144 | 0 | 2 ms | 188 MB | 0 % |

The candidates grow within one to two epochs and an exhausted epoch is rebuilt at once, so no unit is handed out twice. When the miners have left, the demand halves per epoch and the candidates follow it down to `min_candidates`.
Observation: the memory of the process stays at its peak after the miners left (the allocator keeps it); it did not grow in a second wave.

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
