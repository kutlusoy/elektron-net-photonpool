# Elektron Net: `elektron-net-photonpool` Batch Attestation RPC Guideline

- **Version:** 0.1 (draft)
- **Date:** November 3, 2026
- **Audience:** Node developers, pool developers
- **Reference implementation:** [`elektron-net`](https://github.com/kutlusoy/elektron-net) - `src/validation.cpp` (`ComputeBlockUTXOAttestationHash`, `StripAttestationOutput`, `ValidateUTXOCheckpoint`), `src/kernel/utxo_muhash.h`, `src/crypto/muhash.cpp`, `src/interfaces/mining.h`, `src/node/miner.cpp`, `src/node/types.h`, `src/rpc/mining.cpp` - treat as ground truth for anything the doc references
- **Consumer:** [`elektron-net-photonpool`](https://github.com/kutlusoy/elektron-net-photonpool)
- **See also:** [`guideline-photon-architecture.md`](./guideline-photon-architecture.md), [`guideline-mining-landscape.md`](./guideline-mining-landscape.md), [`guideline-coinbase-third-op-return.md`](https://github.com/kutlusoy/elektron-net/blob/main/doc-elektron/guideline-coinbase-third-op-return.md), [`mining-pool-integration.md`](https://github.com/kutlusoy/elektron-net/blob/main/doc-elektron/mining-pool-integration.md)

---

## 1. Purpose and non-negotiables

The pool needs one UTXO attestation per payout script and tip, for hundreds to a thousand scripts, within milliseconds of a tip change. `getblocktemplate` cannot deliver this: it keeps one cached template (one tip, one payout script) and rebuilds everything for every new script under `cs_main` (`src/rpc/mining.cpp`).

This document specifies a node-side batch interface. Rules:

1. It MUST NOT change consensus, validation, pruning or snapshot logic.
2. Its result MUST be bit-identical to what `ComputeBlockUTXOAttestationHash` computes for the same full block. Reuse of the consensus accumulator code is mandatory, no second implementation.
3. It MUST fail closed: any error, stale tip or inconsistency returns an error, never a partial or approximate result.
4. It MUST NOT offer any flag that skips, trusts or relaxes attestation.
5. It is an authorisation-gated mining interface, never a public RPC.
6. `submitblock` on an unmodified node remains the only acceptance test.

## 2. Why the batch is cheap

Verified in the source:

- The MuHash state keeps a numerator (added coins) and a denominator (removed coins) (`UTXOMuHashState`, `crypto/muhash.h`). `AddCoin` multiplies into the numerator, `RemoveCoin` into the denominator.
- Finalisation divides numerator by denominator, which needs a modular inversion of the denominator (`Num3072::Divide`, `GetInverse`), then SHA-256 over the result (`MuHash3072::Finalize`).
- For all candidates of one tip, the spent coins and the added non-coinbase coins are identical. Only the coinbase coin differs per payout script.

Therefore (design, to be confirmed by benchmark):

1. Apply the block's non-coinbase transactions once to a copy of the tip accumulator. This yields a shared numerator `N0` and a shared denominator `D0`.
2. Compute `D0^-1` once.
3. Per candidate: hash the coinbase coin into a 3072-bit number, multiply it into `N0`, multiply by `D0^-1`, run SHA-256. No inversion per candidate.

Expected cost per candidate is a small number of 3072-bit multiplications plus one coin hash, i.e. tens of microseconds on a modern core. This is an estimate and MUST be measured (section 11).

The batch path applies only from `MuhashAttestationActivationHeight` on. Below it, or when the incremental state is unavailable, the call returns `unsupported-height`.

## 3. Interface

### 3.1 Primary: Mining IPC interface

The node already ships the Cap'n Proto based mining interface (`src/interfaces/mining.h`, `src/ipc/`, `ENABLE_IPC` in `CMakeLists.txt`) with `createNewBlock`, `waitTipChanged`, `getCoinbaseTx`, `getCoinbaseMerklePath`, `submitSolution`, `checkBlock` and `BlockTemplate::waitNext`. The batch call SHOULD be added to this interface:

```text
Mining::createAttestedBatch(options: BlockCreateOptions, requests: [CandidateRequest]) -> BatchTemplate
```

Reasons: binary transport, no JSON encoding, long-lived connection, tip changes through `waitTipChanged` instead of polling, same trust boundary (local unix socket with file permissions).

### 3.2 Secondary: JSON-RPC mirror

For tooling and tests, `getattestationbatch` mirrors the same semantics. It MUST require authentication and MUST NOT be reachable without it.

### 3.3 Request

```text
CandidateRequest {
  payout_script : bytes        // scriptPubKey of coinbase vout[0] (for be1q addresses: P2WPKH)
  extra_outputs : [TxOut]      // optional, appended after the required outputs, usually empty
}
BlockCreateOptions            // as in src/node/types.h (use_mempool, reserved weight, sigops)
```

Limits (configurable, enforced before any work): maximum requests per call (default 4096), maximum total extra output bytes, maximum script size, maximum calls per second per client.

### 3.4 Response

Shared part (identical for every candidate):

```text
tip_hash, height, template_id, bits, version, min_time, cur_time
coinbase: version, sequence, script_sig_prefix, witness (32 bytes), lock_time, coinbasevalue
witness_commitment_output        // identical for all payout scripts (coinbase wtxid is zeroed)
transactions[]: raw, txid, wtxid, fee, sigops (in block order)
coinbase_merkle_path[]           // same for all candidates
```

Per candidate:

```text
attestation        : 32 bytes
coinbase_txid      : 32 bytes   // for cross-checking the pool's own coinbase construction
```

`template_id` is a domain-separated SHA-256 over tip hash, height, ordered txids, coinbase value, bits and version. It is an internal identity, not a consensus value.

The pool builds each coinbase itself. For a candidate the final layout is:

```text
vout[0]   payout
vout[1]   attestation OP_RETURN   (from the response)
vout[2]   witness commitment      (shared)
vout[3..] extra_outputs, verbatim, last
```

## 4. Attested coinbase form

`ComputeBlockUTXOAttestationHash` strips only the attestation output (`StripAttestationOutput`) and keys the coinbase coin by the txid of what remains. The attested form is therefore:

```text
[ payout, witness commitment, extra_outputs... ]
```

The batch call MUST compute the coinbase txid over exactly this form (version, input with `script_sig_prefix`, sequence `0xFFFFFFFE`, outputs as above, `lock_time = height - 1`) and MUST insert one coin per spendable output at that txid, with the block height and the coinbase flag, as the consensus code does. Unspendable outputs are skipped.

## 5. Finding: extra coinbase outputs change the attestation

`guideline-coinbase-third-op-return.md` states that a pool-side informational OP_RETURN needs no node involvement. From the code this does not hold:

- `StripAttestationOutput` removes only an output matching `<height><32 bytes>`. A third single-push OP_RETURN stays in the stripped coinbase.
- The coinbase txid, which is part of the attested coin outpoint, then differs from the txid the node used when it built the template.

Consequence: a pool that appends an output after the attestation was computed without it produces a block with `bad-utxo-attestation`. This MUST be confirmed on regtest before relying on either document. The batch interface supports `extra_outputs` so that such an output can be attested correctly. Until confirmed, the pool MUST NOT add any output beyond the node-provided set.

## 6. Consistency and locking

1. Take the tip, the mempool selection and a copy of the tip accumulator under the usual locks, then release them.
2. Perform all per-candidate arithmetic on copies without holding `cs_main`.
3. Before returning, compare the tip against the one used. If it changed, return `stale-tip`.
4. The response carries `tip_hash` and `template_id`. The pool MUST discard results whose tip is not the current one and MUST never submit a block built from them.
5. The call MUST NOT mutate chainstate or the live accumulator.

## 7. Security

- Local unix socket with restrictive permissions for IPC, cookie or credential authentication for JSON-RPC.
- Bounded input, bounded output, bounded concurrency, cancellation on disconnect.
- Deterministic results. No randomness, no time dependence besides `cur_time`.
- No debugging flag that bypasses checks. A test-only `verify_count` MAY recompute the first K candidates through the legacy full-block path and compare; a mismatch is a hard error.
- Never log payout scripts of third parties in clear beyond debug level.

## 8. Pool usage

```text
waitTipChanged  ->  createAttestedBatch(N scripts)  ->  build N coinbases  ->  merkle roots
      -> pre-serialised jobs  ->  broadcast  (clean_jobs)
```

Between tips, fee refreshes use `waitNext` on the previous template, followed by a new batch call only if the transaction set changed. Spot-check one candidate per tip through `getblocktemplate mode=proposal` or `checkBlock` on a fully assembled test block. Latency target (to be measured, not promised): batch call for 1,000 scripts in well under 100 ms.

## 9. Error codes

`unsupported-height`, `stale-tip`, `too-many-requests`, `invalid-script`, `extra-outputs-too-large`, `template-failed`, `attestation-compute-failed`, `internal-mismatch`. Every error is final for that call; the pool retries with a fresh call.

## 10. Alternatives considered

- **Many `getblocktemplate` calls:** viable only for a few dozen scripts because of the single cache slot and full rebuild per call.
- **Pool-side MuHash:** would need the live UTXO accumulator and ties the pool to consensus internals. Rejected.
- **ZMQ plus JSON-RPC polling:** works but costs latency and encoding overhead; the IPC interface already provides push semantics.

## 11. Test plan

1. Equivalence: for random templates on regtest, across heights below and above `MuhashAttestationActivationHeight` and `IntraBlockAttestationFixActivationHeight`, every batch attestation equals `ComputeBlockUTXOAttestationHash` on the assembled full block.
2. Mined blocks: blocks built from batch results are accepted by an unmodified node (`submitblock`).
3. Mutation: altered payout, extra output, order or transaction set invalidates the attestation.
4. Extra outputs: confirm the finding of section 5 in both directions.
5. Race: tip change during the call returns `stale-tip`; reorg tests.
6. Pruned, snapshot-restored and fully synced nodes return identical results.
7. Dependent transaction chains inside the block.
8. Fuzz the request parser and the script/outputs handling.
9. Benchmark: time per candidate and per batch for 100, 1,000 and 4,096 scripts on production-sized UTXO sets.

## 12. Checklist

- [ ] Node commit pinned and attestation path re-verified.
- [ ] Equivalence tests pass for all activation boundaries.
- [ ] Finding of section 5 confirmed or refuted on regtest.
- [ ] Benchmark confirms the per-candidate cost model.
- [ ] Authentication, limits and error handling reviewed.
- [ ] No consensus file touched.

## 13. Open Questions

1. Exact per-candidate cost on production UTXO sets (section 2 is a model).
2. Whether `ApplyCoinHash` multiplies strictly into the numerator in all cases (to be checked in `kernel/coinstats.cpp`).
3. Whether the IPC build is part of the standard Elektron release artifacts for all target platforms.
4. Whether `BlockWaitOptions` provides a fee threshold usable for refresh decisions.
