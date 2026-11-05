# Elektron Net: `elektron-net-photonpool` Batch Attestation RPC Guideline

- **Version:** 0.2 (draft; implemented as a node patch and tested, see section 3.5)
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

### 3.5 As built: the `photonpool` branch of the node (and the patch file)

The RPC is part of the branch `photonpool` of `elektron-net` (commits "rpc: add getattestationbatch for mining pools" and the test and guideline commit, see `doc-elektron/guideline-attestation-batch-rpc.md` there). It is always available and does no work unless it is called; the Docker setups of this repository build the node from that branch (`ELEKTRON_REF=photonpool`). When the code is merged to `main`, change `ELEKTRON_REF` back to `main`. `docker/node/patches/0001-photon-getattestationbatch.patch` is the same code and is kept for the tests of the patch file (`check_patch_scope.py`); it does not apply to the `photonpool` branch.

The text below describes the implementation.



The interface exists as a patch that is applied when the node image is built (`docker/node/patches/0001-photon-getattestationbatch.patch`, `PHOTON_NODE_PATCHES=false` by default until the review is complete, set it to `true` to build the patched node; `NODE_PATCHES` in the stack). It adds three functions and one RPC and changes no existing code path of the node:

- `ComputeAttestationBaseState` (`validation.cpp`): the tip's MuHash accumulator with all non-coinbase transactions applied, with the same rules as the incremental path of `ComputeBlockUTXOAttestationHash`, including the intra-block fix gate.
- `AttestationForCoinbase`: the attestation of one stripped coinbase on top of that base state.
- RPC `getattestationbatch` (`rpc/mining.cpp`, category `mining`): one block template per call, one attestation per requested coinbase variant.

The patch was developed against a copy of the node source, applies cleanly to `elektron-net` at `596278b` and leaves that repository untouched.

Request (JSON-RPC): `{"candidates": [ {"outputs": [{"scriptPubKey": hex, "value": leptons?}], "tail_outputs": [{"scriptPubKey": hex, "value": leptons?}], "script_sig_suffix": hex?} ]}` with 1 to 4096 candidates.

- `outputs` are the payout outputs. At most one may omit its value and then receives the remainder of the reward; the sum of the explicit values must not exceed the reward.
- `tail_outputs` follow the witness commitment (value defaults to 0), for example an accounting commitment as an `OP_RETURN`.
- `script_sig_suffix` extends the BIP 34 prefix (extranonce); the scriptSig must stay between 2 and 100 bytes.
- The final coinbase is `outputs`, attestation, witness commitment, `tail_outputs`. An output shaped like an attestation (`OP_RETURN <height> <32 bytes>`) is refused.

Response: the template (`tip`, `height`, `version`, `bits`, `target`, `curtime`, `mintime`, `coinbasevalue`, the coinbase fields, `witness_commitment_script`, `transactions` with `data`, `txid`, `hash`, `fee`, `weight`) and `candidates[]` with `attestation_script`, `coinbase_txid` (of the stripped coinbase) and `coinbase_hex`. The call fails before the MuHash activation height, during initial block download and for malformed requests. It holds `cs_main` while it works, which is acceptable at the measured speed; computing outside the lock and the shared-inverse optimisation of section 2 are possible refinements.

Tests (`test/step14_batch_attestation.py`, patched node on regtest, mempool with a dependent transaction chain):

- Every attestation equals the independent oracle; `coinbase_txid` is the stripped-coinbase txid.
- Assembled blocks are accepted by `getblocktemplate` proposal and by `submitblock`; the block pays the requested script.
- A multi-output coinbase with an `OP_RETURN` accounting commitment and an extranonce suffix is accepted (this is the coinbase layout of the Photon protocol).
- Variants that differ only in the extranonce get different attestations; oversized requests, outputs above the reward, two remainder outputs, attestation-shaped outputs and oversized scriptSigs are refused.

Measured on regtest (small UTXO set, JSON included, one call): 100 candidates in 8 ms, 1,000 in 69 ms, 4,000 in 276 ms, about 0.07 ms per candidate. In the pool (`test/bench_candidates.py`, including local coinbase rebuild and comparison with the node's coinbase, all candidates validated by the node):

| Payout addresses | One `getblocktemplate` call per address | Batch backend |
|---|---|---|
| 100 | 72 ms | 12 ms |
| 430 | 349 ms | 47 ms |
| 2,000 | 1,786 ms | 136 ms |

Mainnet-sized UTXO sets and mempools are not covered by these numbers.

## 4. Attested coinbase form

`ComputeBlockUTXOAttestationHash` strips only the attestation output (`StripAttestationOutput`) and keys the coinbase coin by the txid of what remains. The attested form is therefore:

```text
[ payout, witness commitment, extra_outputs... ]
```

The batch call MUST compute the coinbase txid over exactly this form (version, input with `script_sig_prefix`, sequence `0xFFFFFFFE`, outputs as above, `lock_time = height - 1`) and MUST insert one coin per spendable output at that txid, with the block height and the coinbase flag, as the consensus code does. Unspendable outputs are skipped.

## 5. Confirmed: extra coinbase outputs change the attestation

`guideline-coinbase-third-op-return.md` in `elektron-net` states that a pool-side informational OP_RETURN needs no node involvement. The code and a regtest experiment show otherwise:

- `StripAttestationOutput` removes only an output matching `<height><32 bytes>`. A third single-push OP_RETURN stays in the stripped coinbase.
- The coinbase txid, which is part of the attested coin outpoint, then differs from the txid the node used when it built the template.

Experiment (`test/regtest_attestation_extra_output.py`, unmodified node built from `elektron-net` commit `596278b`, regtest):

| Height | Case | `submitblock` result |
|---|---|---|
| 21, 131 | Coinbase exactly as `getblocktemplate` describes | accepted |
| 22, 132 | One extra byte appended to the scriptSig | `bad-utxo-attestation` |
| 22, 132 | Third OP_RETURN (single push) appended as the last output | `bad-utxo-attestation` |
| 22, 132 | Control again after the rejected attempts | accepted |

Heights 21/22 are below and 131/132 above `MuhashAttestationActivationHeight` (50 on regtest) and `IntraBlockAttestationFixActivationHeight` (110 on regtest), so both the legacy and the incremental attestation path reject the extra output.

Consequence: a pool MUST NOT add any coinbase output beyond the node-provided set unless the attestation was computed for exactly that coinbase. The batch interface supports `extra_outputs` for this purpose. The pool-side tag described in the `elektron-net` document would currently produce invalid blocks and MUST NOT be implemented as written.

## 6. Consistency and locking

1. Take the tip, the mempool selection and a copy of the tip accumulator under the usual locks, then release them.
2. Perform all per-candidate arithmetic on copies without holding `cs_main`.
3. Before returning, compare the tip against the one used. If it changed, return `stale-tip`.
4. The response carries `tip_hash` and `template_id`. The pool MUST discard results whose tip is not the current one and MUST never submit a block built from them.
5. The call MUST NOT mutate chainstate or the live accumulator.

## 6a. As built: locking

The JSON-RPC mirror takes `cs_main` only for one consistent snapshot (tip, template, witness commitment, attestation base state, minimum time). The per-candidate work (coinbase assembly, MuHash of the outputs, hex encoding) runs on copies without the lock, so a large request cannot stall block validation. The hold time of the snapshot is logged at debug level (`-debug=rpc`). A change of the tip while a request runs is not detected by the RPC itself; the pool discards stale work (new epoch) and validates every candidate with a `getblocktemplate` proposal before use.

## 6b. Hardening after the external review (November 5, 2026)

An external static review of the patch (no build, no run) raised the points below. Verdicts and what was done:

| Finding | Verdict | Done |
|---|---|---|
| Overflow of `fixed += value` | Not reachable with the current caps (at most 2000 values of `MAX_MONEY` = 2.1e15 leptons is 4.2e18, below the `int64` limit of 9.2e18), but fragile when a cap is raised | Every value is checked against `reward - fixed` before it is added |
| Request size and cost | Partly valid. The HTTP body limit of the node (32 MiB) bounds the request, but the cost is per output (MuHash), so a small JSON body with very many small outputs was still expensive | Budget over the whole request: at most 400,000 outputs and 8 MiB of scripts. The suggested defaults of 64 candidates and 128 outputs were NOT adopted: the pool uses hundreds of candidates and coinbases with up to 1000 outputs, they would break it |
| `cs_main` held for the whole call | Partly valid. Input parsing and validation already happened before the lock, but the per-candidate loop ran under it although it needs only copies | The loop runs without the lock (section 6a) |
| Attestation correctness | Valid as a requirement, already covered: `step14` and `step15` compare every candidate with a Python oracle, with the node's own validator and with the unpatched `getblocktemplate` path, including intra-block spends, reorgs, activation heights and no side effects | `step29` adds the two-node test: blocks built from the batch are accepted by an UNPATCHED node, a block with a wrong attestation is rejected by both |
| No authentication of the RPC | The RPC uses the node's normal authentication (cookie or `rpcauth`) and is only reachable inside the Docker network or on localhost | No code change. Optional hardening: a separate `rpcauth` user for the pool with `rpcwhitelist` (see the Quick-Install); the exact method list is in the Quick-Install and was not tested |
| Parallel heavy calls | Valid | At most two calls run at the same time, further ones fail at once with "busy" (the pool calls it once per tip) |
| (own finding) coinbase of more than half a block | The reserved weight was clamped silently, which could produce an overweight block | Such a request is refused |

`step30` is the end-to-end version: a real `photond` with the batch backend on the patched node, Stratum miners in pool mode and in solo mode find blocks, and a second UNPATCHED node accepts every one of them (pool and solo, six blocks in a row, the solo block pays the miner directly).

`step28` tests the budgets, the overflow case, the coinbase size limit, the parallel limit and that the node stays responsive and keeps validating blocks during heavy calls. Sanitizer run (November 5, 2026): the patched node was built with `-DSANITIZERS=address,undefined` (RelWithDebInfo, `-O1`, wallet off) and `step14`, `step15` (seeds 1 and 2), `step16` to `step20`, `step22`, `step23`, `step28`, `step29` and `step30` ran against it, with `ASAN_OPTIONS=detect_leaks=0` and the reports written to files: no AddressSanitizer or UndefinedBehaviorSanitizer report. `step23` is a timing check and failed once by a hair under the slow sanitizer build (83 ms against a limit of 83 ms) and passed on the second run. Leak detection was off, and the fuzz-style input tests are random, not coverage guided. Not done: a coverage-guided fuzzer.

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

## 11. Test plan and results

Tests live in `test/` and run against an unmodified `elektrond` on regtest (built from `elektron-net` commit `596278b`, no wallet, no IPC). They use an independent oracle written in Python: it rebuilds the post-block UTXO set from node data (`scantxoutset`, block template) with the reference MuHash implementation of the node's test framework and derives the attestation without any node help. Run all steps with `ELEKTROND=/path/to/elektrond PHOTON_DATADIR=/tmp/photon test/run_all.sh`.

| Step | Script | What it proves | Result |
|---|---|---|---|
| 1 | `step01_oracle_matches_node.py` | Oracle MuHash of the scanned set equals `gettxoutsetinfo muhash`; oracle attestation equals the node's for an empty block | passed |
| 2 | `step02_extra_output_counter_test.py` | `getblocktemplate mode=proposal` validates the attestation; a third OP_RETURN is rejected with the node attestation and accepted (proposal and `submitblock`) with the oracle attestation | passed |
| 3 | `step03_multi_payout_candidates.py` | Six payout scripts (OP_TRUE and P2WPKH) give six different attestations, all accepted by proposal; a candidate with another script's attestation is rejected; mined blocks are accepted, including on the next height | passed |
| 4 | `step04_transactions_and_tx_sets.py` | With a mempool parent, child and independent transaction: oracle equals node attestation, several payouts accepted, block with transactions mined; dropping a transaction, or reordering, is rejected with the old attestation and accepted once witness commitment and attestation are recomputed | passed |
| 5 | `step05_stale_and_reorg.py` | A candidate for an old parent is `inconclusive-not-best-prevblk` in proposal and `inconclusive` on submit, never best; after invalidating the competing block the node reorgs onto it; fresh candidates work afterwards | passed |
| 6 | `step06_mutation_matrix.py` | Wrong value, scriptSig suffix, wrong height, wrong digest, unattested extra spendable output are rejected; the same extra output is accepted when its coin is attested | passed |
| 7 | `step07_many_payouts_timing.py` | 500 payout scripts: 500 distinct attestations, a sample of 25 accepted by proposal, one mined | passed |

Findings:

1. The shared-base model of section 2 holds: after the shared work, each payout script costs one coin hash and one 3072-bit multiplication. In Python this is 0.57 ms per candidate (500 candidates in 285 ms). A native implementation will be considerably faster. The Python base (110 ms for 130 coins) scales with the set and is not representative, the node uses its incremental accumulator.
2. `getblocktemplate mode=proposal` checks the full attestation. It is a valid consensus check for assembled candidates (about 1.2 ms each on regtest) and closes the open question about the prevalidation path.
3. Any change to the transaction set or its order changes the witness commitment and therefore the coinbase txid and the attestation. The batch interface MUST return a fresh commitment together with the attestations and the pool MUST NOT reuse either after changing the set. Reordering independent transactions is not free.
4. A coinbase may claim less than the full reward, with a recomputed attestation it is valid. Payout value changes are therefore consensus-valid but not useful for the pool.
5. Dropping or reordering transactions requires adjusting the coinbase value to subsidy plus the fees of the included transactions (`bad-cb-amount` otherwise).
6. The legacy attestation path (below `MuhashAttestationActivationHeight`) is irrelevant for production: mainnet is far beyond height 290000. The batch interface returns `unsupported-height` below the activation height, the oracle implements the MuHash path.

Still to do: native benchmark of the batch call on production-sized UTXO sets, pruned and snapshot-restored nodes, fuzzing of the request parser, and an end-to-end test of the batch interface itself once it exists (compare against this oracle).

## 12. Checklist

- [ ] Node commit pinned and attestation path re-verified.
- [x] Oracle equivalence on regtest (steps 1 to 7) for the MuHash path.
- [ ] Equivalence tests pass for all activation boundaries.
- [x] Section 5 finding confirmed on regtest (both attestation paths).
- [ ] Correct `guideline-coinbase-third-op-return.md` in `elektron-net`.
- [ ] Benchmark confirms the per-candidate cost model.
- [ ] Authentication, limits and error handling reviewed.
- [ ] No consensus file touched.

## 13. Open Questions

1. Exact per-candidate cost on production UTXO sets (section 2 is a model).
2. Whether `ApplyCoinHash` multiplies strictly into the numerator in all cases (to be checked in `kernel/coinstats.cpp`).
3. Whether the IPC build is part of the standard Elektron release artifacts for all target platforms.
4. Whether `BlockWaitOptions` provides a fee threshold usable for refresh decisions.
5. Native cost per candidate and per set size (the Python figures are an upper bound).
