# Elektron Net: `elektron-net-photonpool` Release and Bitaxe Mainnet Test Guideline

- **Version:** 0.1
- **Date:** November 4, 2026
- **Audience:** Operators running the first live test
- **Reference implementation:** [`elektron-net`](https://github.com/kutlusoy/elektron-net) (node), [ESP-Miner](https://github.com/bitaxeorg/ESP-Miner) (Bitaxe firmware) - treat as ground truth for anything the doc references
- **See also:** [`Quick-Install.md`](../Quick-Install.md), [`guideline-stratum-v1.md`](./guideline-stratum-v1.md), [`guideline-payout-modes.md`](./guideline-payout-modes.md)

---

## 1. What this release contains

Release candidate `0.2.0-rc1`: Stratum V1 pool with solo, PPLNS and PPS, ledger, payouts (dry-run by default), public and miner pages, operator UI. The Photon protocol modules exist in `src/photon/` but are **not part of the running daemon yet**; a Bitaxe uses plain Stratum V1. The node RPC `getattestationbatch` (branch `photonpool` of `elektron-net`) is used by the Docker setups; one miner needs no batch RPC, and the code is still in review.

## 2. Why a Bitaxe can work with this pool

The pool serves a fixed coinbase per candidate (the node attests every coinbase byte), so it sends an empty `extranonce1` and `extranonce2_size = 0`. Read in the ESP-Miner source (master, November 2026):

- The firmware accepts an empty extranonce1 (hex length 0, even, at most 64) and `extranonce_2_len` 0 to 8 (`components/stratum/stratum_api.c`).
- `miner_job_is_rollable()` requires `extranonce2_len > 0`; with 0 the firmware does not roll extranonce2 (`components/stratum/include/miner_job.h`), so it does not resend identical work.
- Search space then comes from the nonce, hardware version rolling (the pool grants the BIP 320 mask) and the pool's nTime offsets and candidates.

This is a reading of the source, not a test. The live test is what proves it.

## 3. Pre-flight (mainnet)

1. Node synchronized (`getblockchaininfo`: `initialblockdownload` false), RPC reachable only locally.
2. `pool.network = "mainnet"`; payout addresses start with `be1` and belong to keys you hold.
3. `photond -c photond.toml check` prints all candidates valid. Mainnet blocks are small, so the `gbt` backend is fast enough for one miner.
4. Keep `payout.dry_run = true` and PPS disabled for the first test. Use **solo mode** on the test port: the reward goes straight to the miner address in the coinbase, no wallet and no ledger money involved.
5. Port 3333 (or your chosen port) reachable from the Bitaxe, admin API only on loopback.
6. Check the node version is `main` and that the pool and node agree on the chain (`pool.network`).

## 4. Bitaxe settings

| Setting | Value |
|---|---|
| Stratum URL | host name or IP of the pool, no scheme |
| Port | a port whose `[[stratum.ports]]` mode is `solo` (3335 in `photond.example.toml`; the single `listen` port uses `default_mode`) |
| Stratum user | `be1q...<your address>.bitaxe1` |
| Stratum password | `m=solo` (if the port does not already default to solo) |
| Fallback pool | optional, but keep one configured so the device keeps hashing if the test aborts |

A Bitaxe reaches roughly 0.5 to 1.2 TH/s. Initial difficulty: the pool starts low and vardiff settles within minutes; set `suggest_difficulty` in the firmware if you want to start higher.

## 5. What to check, in order

| Step | Expected | If not |
|---|---|---|
| 1. Connect | Operator UI shows 1 miner, the Bitaxe log shows `Subscribe result: extranonce=, extranonce2_len=0` | Wrong port, firewall, address prefix `be` |
| 2. Authorize | No `Unauthorized worker` | User must be `<be1 address>.<worker>` |
| 3. First job | `New Work Activated` in the Bitaxe log within a second | Pool still waiting for the node, see `/api/v1/summary` |
| 4. Shares | Accepted shares climb, vardiff converges to a share every few seconds | Many rejects: read the reject reasons in the UI (stale, low difficulty, duplicate) |
| 5. Tip change | After each new block a clean job arrives and shares continue without a reject burst | Persistent stale shares: check node latency |
| 6. Hashrate | Pool estimate near the device's own display | Large deviation points to wrong difficulty accounting, keep the raw numbers |
| 7. Block (very unlikely at this hashrate) | A found block is validated by the node and shows in the blocks list | If `submitblock` is refused, keep the full log line and the block hex |

With this hashrate a block is not expected. The goals of the test are steps 1 to 6: the device accepts the extranonce settings, receives jobs, produces valid shares and survives tip changes.

## 6. Abort and rollback

- Point the Bitaxe back to its normal pool (change the URL, nothing else is needed).
- `docker compose down` or `systemctl stop photond` stops the pool; the ledger is a single SQLite file and is safe to keep.
- With `dry_run = true` nothing was ever sent from the wallet.

## 7. What to record for the report

Firmware version, pool settings, the first 30 lines of the Bitaxe log after connecting, the UI summary after 30 minutes (shares accepted/rejected by reason, hashrate estimate, vardiff), and the pool log. This decides whether the empty extranonce1 and `extranonce2_size = 0` are acceptable for all firmware or need a documented exception.

## 8. Known limits of this release

- No ASIC test before this one; Stratum V2 is not implemented.
- The batch RPC is in the node branch `photonpool`; the differential tests and the sanitizer run pass, but the independent review is not finished. A pool with only a few payout addresses can use `pool.backend = "gbt"`.
- Docker images are not built in CI; the first image build happens on your server.
- Mainnet-scale benchmarks are missing (a regtest simulation of 11,000 transactions per block and 10,000 miners is documented in [`guideline-daemon-core.md`](./guideline-daemon-core.md)).

## 8a. First hardware result (one Bitaxe Gamma, BM1370, firmware 2.9.0)

First run: 98.8 % duplicate shares (326 duplicates against 4 accepted) and a hash rate estimate of 1.4 GH/s for a device that reports 6 TH/s. After the first change
(fresh jobs every 200 ms): the hash rate shown by the device was correct (1.01 TH/s), but 83 % of the shares were rejected as "Job not found" and 14 % as duplicates.

Cause, from the log and the firmware source (tag v2.9.0, `create_jobs_task.c`): for every notification ESP-Miner keeps its ASIC job queue filled with up to 10 jobs, each with
`extranonce2 + 1`. With `extranonce2_size = 0` (which the pool must send, because the attestation sits inside the coinbase) all of them are identical. The ASIC task takes one job
per 500 ms, so the chip sweeps the same header again and again (hits repeat about every 163 ms, which I verified by recomputing the logged results from the job), and a new notification waits behind the queued jobs, which
is up to 5 s of latency. Shares then arrive for jobs that the pool has already dropped. The firmware only throws the queue away on a notification marked clean.

Changes:
- `[stratum] clean_work_agents` (default `["bitaxe", "esp-miner"]`): every fresh job for these miners is marked clean, so the chip always gets the newest job at once and no queue builds up.
  Large ASICs do not get it, a clean job makes them flush their pipeline. `test/step24_clean_work_for_esp_miner.py` checks it.
- `[stratum] version_space_bits` (default 5): job cadence from the space a miner really sweeps (a pass of one job takes the chip about 114 ms at 1.2 TH/s, so about 9 jobs per second; the shortest interval is `min_job_interval_ms`, default 25 ms, raised for big farms by `max_jobs_per_sec`). The default used to be 200 ms and held a Bitaxe of 1.2 TH/s at about 0.7 TH/s.
- A duplicate share triggers fresh work at once.
- The pool keeps the jobs of a connection for 10 s (at least 16, at most 128), so that late shares are still found.

To confirm on the next run: stale and duplicate shares should fall to a few percent, and the pool's hash rate estimate should come close to the 1 TH/s of the device.
Consequence for the Photon design: a Bitaxe needs a new header about every 160 ms; the work space and traffic figures of the protocol guideline assumed the miner covers 2^44 hashes per header and have to be re-checked.

## 9. Checklist

- [x] Pre-flight (section 3) done.
- [x] Bitaxe connected, shares accepted (one Bitaxe Gamma, 37 shares and one duplicate in the longer run, one block found).
- [x] At least one tip change survived without a reject burst.
- [x] Report data (section 7) collected: logs and UI screenshots, see 8a.
- [ ] Longer run (hours) with the hash rate estimate of the pool next to the device's own figure.
- [ ] A larger ASIC (Antminer, Whatsminer or similar) and other firmware: start-up log and 30 minutes of statistics.
