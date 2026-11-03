# Elektron Net: `elektron-net-photonpool` Mining Landscape Review

- **Version:** 0.1 (draft)
- **Date:** November 3, 2026
- **Audience:** Pool developers and operators
- **Reference implementation:** [`elektron-net`](https://github.com/kutlusoy/elektron-net) - `src/interfaces/mining.h`, `src/ipc/`, `CMakeLists.txt` (`ENABLE_IPC`) - treat as ground truth for anything the doc references
- **See also:** [`guideline-photon-architecture.md`](./guideline-photon-architecture.md), [`guideline-batch-attestation-rpc.md`](./guideline-batch-attestation-rpc.md)

---

## 1. Evidence quality

The sources below are mostly secondary (blogs, news aggregators, vendor pages) and partly contradict each other. Everything here is a lead to verify against primary sources (Stratum V2 specification site, SRI repository and releases, Bitcoin Core release notes, vendor changelogs) before it influences a design decision. Nothing in this document is a verified fact about Elektron Net.

## 2. Findings

1. **Stratum V2 adoption is moving, mostly at the mining-protocol level.** Reports state that seven large pools joined the Stratum V2 Working Group on May 7, 2026, and that production use today is mostly the mining protocol (encryption, binary framing) with the pool still building templates. Native V2 pools reported: Braiins Pool and DEMAND (DMND). A job declaration block by a miner-built template is reported, with conflicting dates. Sources: [spark.money](https://www.spark.money/research/bitcoin-stratum-v2-mining-decentralization), [forklog](https://forklog.com/en/major-bitcoin-pools-back-stratum-v2-adoption/), [D-Central matrix](https://d-central.tech/stratum-protocol-matrix/).
2. **Firmware.** Braiins OS+ is reported as the main Antminer firmware with a native V2 client, and Bitaxe AxeOS as shipping V2 from a 2026 release. Most ASIC firmware still speaks V1 with BIP 310 version rolling (default mask `0x1FFFE000`). Treat as unverified. Source: [D-Central matrix](https://d-central.tech/stratum-protocol-matrix/).
3. **Reference implementation.** The Stratum V2 Reference Implementation (SRI) has a published roadmap for 2026 (prebuilt binaries, pool and job declarator web UIs). Source: [SRI roadmap](https://stratumprotocol.org/blog/sri-roadmap-2026/).
4. **Node-side mining interface.** Bitcoin Core exposes a Cap'n Proto based mining IPC interface, built by default in release builds, and a Rust crate implements a Stratum V2 Template Provider on top of it. Sources: [Bitcoin Core PR 31802](https://github.com/bitcoin/bitcoin/pull/31802), [bitcoin_core_sv2](https://docs.rs/bitcoin_core_sv2).
5. **ckpool V2 support** is mentioned for July 31, 2026 in one matrix, unverified.
6. **Not found:** reliable sources on ntime rolling behaviour of current ASIC firmware or on low-latency block propagation practice in pools.

## 3. Checked in this repository

`elektron-net` already contains the Bitcoin Core mining interface: `Mining` and `BlockTemplate` in `src/interfaces/mining.h` (`createNewBlock`, `waitTipChanged`, `waitNext`, `getCoinbaseTx`, `getCoinbaseMerklePath`, `submitSolution`, `checkBlock`), the IPC layer in `src/ipc/` and the `ENABLE_IPC` build option. The interface was designed for Stratum V2 template providers and fits this project directly.

## 4. Consequences for the pool design

1. **Use IPC for the hot path.** Push-based tip changes and binary calls replace JSON-RPC polling. Add the batch call to this interface (see the batch RPC guideline).
2. **V1 stays mandatory, V2 follows.** Most hardware speaks V1. Build the V2 mining protocol (Noise encrypted, binary, standard channels) as the second listener. Job declaration and extended channels stay out of scope, because the coinbase is fixed by the attestation.
3. **Template provider logic is not reusable as is.** Existing SV2 template providers assume a free coinbase. Elektron Net needs the attested coinbase from the node.
4. **Engineering levers for speed** (design suggestions, to be benchmarked): pre-serialised job frames written once to all sockets, `TCP_NODELAY`, socket sharding per core (`SO_REUSEPORT`), batched syscalls (`writev` or io_uring), no database access in the hot path, bounded queues, per-core share validators.
5. **Photon edge protocol.** A farm-side proxy speaking a compact binary profile to the pool is the one place a custom protocol helps, because no firmware change is needed. It SHOULD reuse V2 framing and security (Noise) instead of inventing new primitives.

## 5. Checklist

- [ ] Verify the V2 findings against the specification site and SRI releases.
- [ ] Verify firmware V2 support per target device family.
- [ ] Confirm the IPC build is available in Elektron Net release artifacts.
- [ ] Measure ntime and version rolling behaviour on real devices.

## 6. Open Questions

1. Which firmware families will actually be connected, and which of them speak V2?
2. Does the target hardware accept very frequent non-clean jobs without dropping work?
