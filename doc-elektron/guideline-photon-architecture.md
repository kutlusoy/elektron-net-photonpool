# Elektron Net: `elektron-net-photonpool` Architecture Guideline

- **Version:** 0.1 (draft)
- **Date:** November 3, 2026
- **Audience:** Pool developers and operators, node developers
- **Reference implementation:** [`elektron-net`](https://github.com/kutlusoy/elektron-net) - `src/validation.cpp` (`ComputeBlockUTXOAttestationHash`, `ValidateUTXOCheckpoint`), `src/node/miner.cpp` (`BlockAssembler::CreateNewBlock`), `src/rpc/mining.cpp` (`getblocktemplate`), `src/kernel/utxo_muhash.h`, `src/crypto/muhash.cpp` - treat as ground truth for anything the doc references
- **Consumer:** [`elektron-net-pool`](https://github.com/kutlusoy/elektron-net-pool) (existing Stratum V1 pool, source of hardware quirks)
- **See also:** [`doc-elektron/mining-pool-integration.md`](https://github.com/kutlusoy/elektron-net/blob/main/doc-elektron/mining-pool-integration.md), [`WHITEPAPER.md`](https://github.com/kutlusoy/elektron-net/blob/main/WHITEPAPER.md) section 4.3

---

## 1. Scope and non-negotiables

The pool MUST NOT require, assume or propose any change to Elektron Net consensus. Every block MUST carry the attestation the node computed for that exact candidate, and `submitblock` on an unmodified node is the only acceptance test. The pool MUST NOT trust a share as a valid block, and MUST NOT reuse an attestation after any change to the coinbase, the transaction set or the tip.

Target load: 1,000 miners of all sizes (small to large ASICs, no hobby devices); the design must not preclude growth. The pool cannot raise hashrate; efficiency means minimising stale, duplicate and idle hashing.

## 2. Verified consensus facts

These were read from the node source and MUST be re-checked against the pinned node commit before implementation.

1. The attestation is the UTXO set commitment after the block, computed with the attestation output stripped from the coinbase (`StripAttestationOutput`). It is HASH_SERIALIZED below height 137000 and incremental MuHash from 137000 on; the intra-block dependent-transaction fix applies from 170000 (`kernel/chainparams.cpp`, mainnet).
2. The coinbase txid is an outpoint in the attested set. Payout script, amount, scriptSig, nLockTime and the witness commitment therefore change the attestation, as does the transaction set.
3. The coinbase scriptSig MUST be exactly `coinbase_script_sig_prefix`. There is no extranonce space.
4. Header rules are the standard Bitcoin ones: time above median time past and below now plus the future limit, nVersion at least 4 once CLTV is active (`validation.cpp`, contextual header checks). Version rolling and nTime rolling are consensus-safe.
5. `getblocktemplate` accepts `coinbaseaddress`, but keeps one cached template (one tip and one payout script). Alternating addresses rebuilds the full template on every call under `cs_main`.
6. `getblocktemplate` with `mode=proposal` runs `TestBlockValidity` on a complete block without checking proof of work. It validates a candidate but cannot compute an attestation.

## 3. The search space problem

With a fixed coinbase, one (template, nTime) pair offers at most 2^32 nonces times the rollable version bits. A 100 TH/s ASIC exhausts 2^44 headers in roughly 0.2 s. Without further measures, many miners hash identical headers and the pool-wide useful hashrate is capped.

Estimates (NOT measured; to be validated in section 9):

| Total hashrate | Distinct templates per block interval (60 s, 2^48 headers per template and nTime, 60 nTime values) |
|---|---|
| 100 PH/s | about 350 |
| 1 EH/s | about 3,500 |

Mitigation without consensus change: one template per pool-controlled payout address. Each address yields a different coinbase txid and therefore an independent search space.

### 3.1 Sizing for 1,000 miners

Model (estimates, NOT measured): the pool reserves the top 4 rollable version bits for work partitioning and miners roll the remaining 12 (mask `1ffe0000`, as in `elektron-net-pool`). A unit of work is one (candidate, nTime, pool slice) tuple and gives a miner 2^44 headers. A (candidate, nTime) pair holds 16 units. nTime advances once per second, 60 values per 60 s block.

| Scenario | Total hashrate | Candidates (pool addresses) per block | Units per second | Job messages per miner and second (average) |
|---|---|---|---|---|
| Small | 10 PH/s | about 36 | about 570 | about 0.6 |
| Medium | 100 PH/s | about 350 | about 5,700 | about 6 |
| Large | 300 PH/s | about 1,100 | about 17,000 | about 17 |

Derivation: candidates = hashrate / (2^48 * 1 pair per second) per second of nTime, i.e. hashrate * 60 / (2^48 * 60). Units per second = hashrate / 2^44.

Consequences:

- Share validation (about 100 shares per second at one share per 10 s per miner) and socket fan-out (about 3 MB/s in the large scenario) are not the bottleneck.
- Candidate production at the node is the bottleneck: plain `getblocktemplate` rebuilds the full template for every address, so it is only viable up to a few dozen addresses. Above roughly 10 PH/s the batch RPC of section 4 is required.
- A 1 TH/s miner needs a new unit about every 17 s, a 100 TH/s miner every 0.18 s, a 300 TH/s miner every 0.06 s. Whether stock firmware accepts that job rate is unknown and MUST be tested (section 9).
- Using nTime values far in the future is consensus-valid (limit `MAX_FUTURE_BLOCK_TIME` = 2 h) but SHALL NOT be used, since it distorts block timestamps.

## 4. Node side: batch attestation RPC

A new, authenticated RPC (working name `getattestationbatch`) MUST be added to the node. It is an RPC only, not a consensus change.

- Input: list of payout scripts for the current tip.
- Work done once: transaction selection, accumulator state for tip plus block transactions.
- Work per script: copy accumulator, add the single coinbase coin, finalize (the modular inversion in `MuHash3072::Finalize` is the expected dominant cost).
- Output: attestation per script, bound to the tip hash and a template id.
- The accumulator work SHOULD run outside `cs_main` on a copy.
- Pool builds each coinbase itself from the template fields and spot-checks candidates with `mode=proposal`.
- No flag may skip or relax attestation. Fail closed on any computation error.

Until the RPC exists, the pool falls back to plain `getblocktemplate` for a small number of addresses.

## 5. Pool architecture

```text
elektrond --ZMQ hashblock--> Template service --> Job generator --> Protocol frontends
    ^                              |                    |              (V1, V2, edge)
    | getattestationbatch          v                    v
    +------------------------ Candidate store     Share validator --> Accounting (batched)
                                                        |                  |
                                                        v                  v
                                                  Block submitter     Payout worker
```

- **Template service:** reacts to new tips via ZMQ, requests attestations for all pool addresses, stores one immutable candidate per (tip, address).
- **Job generator:** assigns every miner a unique (candidate, nTime, version slice) tuple, so no two miners hash the same header. Serialises each job once and broadcasts the bytes.
- **Share validator:** in-memory, one double SHA-256 per share, pool-wide duplicate detection on the header, no database access in the hot path.
- **Block submitter:** rebuilds the full block from the immutable candidate, calls `submitblock`, records the result.
- **Accounting and payout:** batched writes, separate process. Pool fees are paid outside the coinbase.
- **Frontends:** separate listeners sharing the same core.

### 5.1 Job allocation rules

1. The allocation unit is (candidate, nTime, pool slice). The generator MUST NOT hand the same unit to two miners within one tip epoch.
2. Each miner receives a new unit every 2^44 / h seconds, where h is its estimated hashrate (from accepted shares and vardiff). Small miners keep a unit until it is exhausted.
3. `clean_jobs` is sent only on a tip change. Unit changes within an epoch are non-clean jobs.
4. Pool-wide duplicate detection keys on (candidate id, nTime, version, nonce) in a bounded set that is cleared on each tip change.
5. If all units of an epoch are in use, the pool MUST flag the overlap instead of silently reusing units. Sizing in section 3.1 is meant to prevent this.
6. Candidates are immutable. A tip change invalidates all of them atomically.

### 5.2 Pool payout addresses

- Payout addresses are pool-owned `be1q...` addresses (mainnet bech32 prefix `be`, native segwit), derived from one pool HD wallet. The node and RPC work with the resulting scripts.
- The number of addresses follows section 3.1 plus about 25 % headroom.
- Coinbase outputs mature after `COINBASE_MATURITY` = 100 blocks (`consensus/consensus.h`). Rewards from found blocks are swept to a main address after maturity.
- Miner payouts are made by the payout worker from pool funds. The coinbase keeps its single payout output, pool fees stay outside the coinbase.
- Signing keys SHOULD stay off the pool host. The pool only needs the public derivation to build coinbase scripts.

## 6. Protocols

- **Stratum V1:** required, most ASIC firmware speaks only V1. Version rolling with a mask reserved partly for pool work partitioning, as in `elektron-net-pool`. Behaviour with `extranonce2_size = 0` on stock firmware MUST be tested on real devices.
- **Stratum V2:** standard channels only at first (fixed, prevalidated candidate). Extended channels and job negotiation stay disabled, see the SV2 specification for the gating rules.
- **Photon edge protocol (optional):** compact binary protocol between a farm-side proxy and the pool. Job messages carry only a candidate id, nTime and version slice. It is a profile of the same semantics, not a replacement for V1 or V2 on the miner side, because no ASIC firmware would speak a new protocol.

## 7. Scalability

- Async I/O with few threads, preferably a compiled language without garbage collection pauses.
- Connection and message limits per source, bounded queues, no unbounded caches.
- Tip change to first job on the wire is the primary latency metric (each second of delay wastes roughly 1.6 % of work at 60 s blocks).
- Expected load at 1,000 miners with one share per 10 s is about 100 shares per second, negligible for validation; job fan-out and candidate production dominate (section 3.1).
- A single process on a few cores is sufficient for 1,000 miners. The design SHOULD NOT block a later move to more miners.

## 8. Operations and UI

- Headless daemon: config file, signal handling, admin API, Prometheus metrics, systemd and Docker.
- The web UI is optional, served by the daemon as static files over the admin API. The pool MUST run without it.
- Alert on any `bad-utxo-attestation` and disable the affected job producer, never the node check.

## 9. Measurements required before implementation

1. Cost per address of add plus finalize in `MuHash3072` on production-sized UTXO sets.
2. Job rate and nTime rolling behaviour of the target ASIC firmwares, and their handling of `extranonce2_size = 0`.
3. Template build cost with a realistic mempool.
4. Tip change to job latency end to end.

## 10. Checklist

- [ ] Node commit pinned and attestation path re-verified.
- [ ] Batch attestation RPC designed, reviewed and benchmarked.
- [ ] Hardware job-rate and ntime tests done on real ASICs.
- [ ] Pool-wide duplicate share detection specified.
- [ ] Unit allocator tested for no overlap across miners of mixed hashrate.
- [ ] Pool HD wallet, address count and sweep procedure defined.
- [ ] Regtest: full block mined through the pool is accepted by an unmodified node.
- [ ] Negative tests: altered coinbase, payout, transaction set, stale tip are rejected.

## 11. Open Questions

1. Real cost of the batch RPC per address, and the achievable number of templates per block.
2. How many jobs per second stock ASIC firmware accepts without dropping work.
3. Expected total hashrate and distribution of miner sizes (selects the sizing scenario).
4. Target firmware families (Bitmain, MicroBT, Canaan, Braiins OS) and their version rolling and ntime rolling behaviour.
5. Payout scheme (PPS, PPLNS or other) and fee policy.
6. Implementation language (Rust or Go recommended).
