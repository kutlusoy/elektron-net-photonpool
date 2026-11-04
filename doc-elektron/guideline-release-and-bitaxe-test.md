# Elektron Net: `elektron-net-photonpool` Release and Bitaxe Mainnet Test Guideline

- **Version:** 0.1
- **Date:** November 4, 2026
- **Audience:** Operators running the first live test
- **Reference implementation:** [`elektron-net`](https://github.com/kutlusoy/elektron-net) (node), [ESP-Miner](https://github.com/bitaxeorg/ESP-Miner) (Bitaxe firmware) - treat as ground truth for anything the doc references
- **See also:** [`Quick-Install.md`](../Quick-Install.md), [`guideline-stratum-v1.md`](./guideline-stratum-v1.md), [`guideline-payout-modes.md`](./guideline-payout-modes.md)

---

## 1. What this release contains

Release candidate `0.2.0-rc1`: Stratum V1 pool with solo, PPLNS and PPS, ledger, payouts (dry-run by default), public and miner pages, operator UI. The Photon protocol modules exist in `src/photon/` but are **not part of the running daemon yet**; a Bitaxe uses plain Stratum V1. The node patch (`getattestationbatch`) is **off by default**: one miner needs no batch RPC, and the patch is still in review.

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
- The batch RPC patch is opt-in; the differential test passes but the review is not finished. For pools with more than a handful of payout addresses build the node with `NODE_PATCHES=true` after the review.
- Docker images are not built in CI; the first image build happens on your server.
- Mainnet-scale benchmarks are missing (a regtest simulation of 11,000 transactions per block and 10,000 miners is documented in [`guideline-daemon-core.md`](./guideline-daemon-core.md)).

## 8a. First hardware result (one Bitaxe, BM1370)

The first run showed 98.8 % duplicate shares (326 duplicates against 4 accepted) and a hash rate estimate of 1.4 GH/s for a device that reports 6 TH/s itself.
The log shows the same result (job, ntime, version bits, nonce) submitted again about every 163 ms, so the chip sweeps the same space of one job again and again.
What the code of ESP-Miner says: with `extranonce2_size = 0` the job counts as not rollable (`miner_job_is_rollable`), the firmware then sends each job once and does not create new headers
by itself, whereas a normal pool gets a new header every 500 ms through `extranonce_2++`. One header seems to cover only about 2^37 hashes of this chip (a pass of about 163 ms at roughly 1 TH/s), not the 2^44 that the
pool assumed for 12 granted version bits. The conclusion is not proven (no chip documentation was used): it is the explanation that fits the log and the firmware source.

Change made: `[stratum] version_space_bits` (default 5) tells the pool how much space one job gives a miner, so it sends fresh work about that often (at least every `min_job_interval_ms`),
and a duplicate share now triggers fresh work at once (for 30 s at the shortest interval). To confirm: duplicates should drop sharply and the pool's hash rate estimate should approach what the device reports.
Open question for the Photon design: a miner that needs a new header every few hundred milliseconds needs a new unit (candidate, nTime step or slice) at that rate; the work space and traffic figures of the protocol guideline have to be re-checked against this.

## 9. Checklist

- [ ] Pre-flight (section 3) done.
- [ ] Bitaxe connected, shares accepted.
- [ ] At least one tip change survived without a reject burst.
- [ ] Report data (section 7) collected.
