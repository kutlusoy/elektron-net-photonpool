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

Target load: 1,000 to 10,000 ASIC miners (no hobby devices). The pool cannot raise hashrate; efficiency means minimising stale, duplicate and idle hashing.

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

## 6. Protocols

- **Stratum V1:** required, most ASIC firmware speaks only V1. Version rolling with a mask reserved partly for pool work partitioning, as in `elektron-net-pool`. Behaviour with `extranonce2_size = 0` on stock firmware MUST be tested on real devices.
- **Stratum V2:** standard channels only at first (fixed, prevalidated candidate). Extended channels and job negotiation stay disabled, see the SV2 specification for the gating rules.
- **Photon edge protocol (optional):** compact binary protocol between a farm-side proxy and the pool. Job messages carry only a candidate id, nTime and version slice. It is a profile of the same semantics, not a replacement for V1 or V2 on the miner side, because no ASIC firmware would speak a new protocol.

## 7. Scalability

- Async I/O with few threads, preferably a compiled language without garbage collection pauses.
- Connection and message limits per source, bounded queues, no unbounded caches.
- Tip change to first job on the wire is the primary latency metric (each second of delay wastes roughly 1.6 % of work at 60 s blocks).
- Expected load at 10,000 miners with one share per 5 s is about 2,000 shares per second, negligible for validation; job fan-out dominates.

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
- [ ] Regtest: full block mined through the pool is accepted by an unmodified node.
- [ ] Negative tests: altered coinbase, payout, transaction set, stale tip are rejected.

## 11. Open Questions

1. Real cost of the batch RPC per address, and the achievable number of templates per block.
2. How many jobs per second stock ASIC firmware accepts without dropping work.
3. Number and management of pool payout addresses, and sweeping of matured coinbase outputs.
4. Implementation language (Rust or Go recommended).
