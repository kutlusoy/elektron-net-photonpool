# Elektron Net: `elektron-net-photonpool` Photon Protocol Guideline

- **Version:** 0.1 (design draft, not implemented)
- **Date:** November 3, 2026
- **Audience:** Protocol, node and pool developers
- **Reference implementation:** [`elektron-net`](https://github.com/kutlusoy/elektron-net) - `src/validation.cpp` (`ComputeBlockUTXOAttestationHash`, `StripAttestationOutput`, `ExtractCoinbaseUTXOAttestation`), `src/node/miner.cpp` (`BlockAssembler::CreateNewBlock`), `src/rpc/mining.cpp` (`getblocktemplate`) - treat as ground truth for anything the doc references
- **See also:** [`guideline-photon-architecture.md`](./guideline-photon-architecture.md), [`guideline-batch-attestation-rpc.md`](./guideline-batch-attestation-rpc.md), [`guideline-payout-modes.md`](./guideline-payout-modes.md), [`guideline-stratum-v1.md`](./guideline-stratum-v1.md)

---

## 1. Goal

The Photon protocol lets miners keep control of what they mine and of their money, while a pool coordinates work and accounting. It is meant to be as efficient as a header-only protocol can be and to be an on-ramp for decentralization:

1. **Miner-built templates.** A farm runs its own node and chooses its own transactions. The pool cannot censor.
2. **Non-custodial payouts.** The block reward is split in the coinbase between the miners. The pool holds no miner funds.
3. **Verifiable accounting.** The pool commits to its share accounting on-chain in every block, so a miner can prove what it was owed.
4. **Portability.** A miner that distrusts a pool changes the pool without changing hardware or node.
5. **Efficiency.** Binary, encrypted, header-only mining with a few messages per block.

Non-goals: changes to Elektron Net consensus (the attestation stays untouched), a new ASIC protocol (ASICs keep speaking Stratum V1 or V2 to a local gateway), and replacing the existing pool-template mode (Photon coexists with it).

## 2. Facts from the node that shape the design

These were read from the source and checked on regtest (see the tests referenced in the other guidelines).

| Fact | Consequence |
|---|---|
| The UTXO attestation is computed over the exact candidate block with the attestation output stripped. The coinbase txid is part of the attested set, so every coinbase byte (payout, scriptSig, extra outputs, witness commitment) and the transaction set change it. | Whoever builds the coinbase must have the node compute the attestation for exactly that coinbase. |
| Extra coinbase outputs are valid when the node's attestation includes them (tested: an attested third OP_RETURN and attested extra spendable outputs are accepted, unattested ones give `bad-utxo-attestation`). | A coinbase paying many miners plus an accounting commitment is consensus-valid. |
| The scriptSig may only deviate from `coinbase_script_sig_prefix` if the node attested that exact scriptSig. | Extranonce space exists, but every variant needs its own attestation. |
| With MuHash, all candidates of one tip share the same spent and added non-coinbase coins; only the coinbase coins differ. | Many coinbase variants cost little per variant on a node (see the batch RPC guideline; Python model 0.57 ms per variant, native expected to be far lower). |
| `getblocktemplate mode=proposal` validates a fully assembled candidate including its attestation (about 1.2 ms). | A pool can spot-check announced candidates cheaply. |
| Pruning is mandatory and automatic (197,280 blocks). | Running an own node is cheap, which makes miner-built templates realistic for small farms. |

## 3. Roles

| Role | Runs | Does |
|---|---|---|
| **Gateway** | on the miner's side, next to an own Elektron Net node | builds templates, creates coinbase variants with attestations through the node, serves local Stratum (V1 or V2) to the ASICs, submits found blocks through its own node, sends shares and template announcements to the pool |
| **Pool** | the coordinator (`photond`) | verifies shares, keeps the accounting, publishes the payout plan and the accounting commitment, optionally also serves legacy miners in pool-template mode |
| **Node** | one per gateway, one for the pool | consensus, attestation computation, block relay |

A miner that does not want to run a node keeps using the existing pool-template mode (Stratum V1 today). Photon is additive.

## 4. Transport and framing

- One TCP connection per gateway, **Noise** secured (`Noise_NX_25519_ChaChaPoly_SHA256` as a starting point; the pool's static key is certified by a pool authority key that gateways pin). Alternatives (Noise with secp256k1 as in Stratum V2) are an open question.
- Binary frames: `ext_type u16 | msg_type u8 | length u24 | payload`. Little-endian integers. Frame sizes are capped (default 64 KiB). The framing is deliberately close to Stratum V2 so that tooling and libraries can be reused.
- Channels multiplex several mining groups over one connection. Everything on a channel is fixed to one payout identity (the miner's payout script).
- All state-changing messages carry the **tip hash** they refer to; anything for a tip that is no longer current is dropped, never reinterpreted.

## 5. Messages (first draft)

| Message | Direction | Content |
|---|---|---|
| `Setup` / `SetupAck` | both | version, features (`extranonce_variants`, `batch_shares`, `legacy_stratum_proxy`), limits |
| `OpenChannel` / `OpenChannelAck` | gateway to pool, back | payout script of the miner, requested share target, channel id; the pool answers with the accepted target and the current payout plan |
| `NewTip` | pool to gateway | tip hash and height as early as the pool knows them (a hint; gateways follow their own node) |
| `PayoutPlan` | pool to gateway | for height `h+1`: plan hash, accounting root, list of `(script, ppm)` outputs, fee output, rules version (section 6) |
| `TemplateAnnounce` | gateway to pool | tip hash, template id, coinbase (non-witness, so the pool can check the outputs), merkle branch, version, bits, attestation commitment; sent once per variant batch, not per share |
| `Shares` | gateway to pool | batch of `(channel, template id, variant, nonce, ntime delta, version bits)`, about 16 bytes per share; the pool recomputes the header hash |
| `ShareAck` | pool to gateway | batched accept counts and per-share rejects with reason codes (stale, duplicate, low difficulty, unknown template, bad coinbase) |
| `SetTarget` | pool to gateway | new share target (vardiff) |
| `BlockFound` | gateway to pool | header hash of a found block, informational: the gateway submits the block itself |
| `EpochCommit` | pool to gateway | accounting root and size of the epoch that ended with a block, plus Merkle proofs on request |
| `Reject` / `Disconnect` | both | error code, reason |

Share targets are per channel; hashrate is estimated from accepted share difficulty exactly as in the Stratum V1 server.

## 6. Payout plan and coinbase

### 6.1 Plan

For every block the pool publishes a `PayoutPlan` for height `h+1` computed **only from shares up to block `h-1`**. It is therefore known one block ahead, so gateways need no round trip after a new tip: they only wait for their own node.

The plan is a deterministic function of the accounting state:

1. Take the credits (PPLNS-style window or another rule fixed by `rules_version`) of the window ending at block `h-1`.
2. Remove outputs below the dust limit; their credit is carried to the next plan.
3. Keep at most `N_max` outputs (default 128), largest credits first; the rest is carried.
4. Express each output as parts per million of the block reward; the pool fee is an explicit output with a published rate.
5. Compute `plan_hash` over the canonical serialization and `accounting_root` over all credits (section 7).

### 6.2 Coinbase layout

```text
vout[0..k-1]   payouts, in plan order, value = floor(reward * ppm / 1_000_000)
vout[k]        pool fee output, value = remainder of the reward (so the sum is exact)
vout[k+1]      attestation OP_RETURN         (from the gateway's node)
vout[k+2]      witness commitment            (from the gateway's node)
vout[k+3]      OP_RETURN <"PHOT" || accounting_root>   single push, 36 bytes
```

- The reward is `subsidy + fees` of the **gateway's own** template; the gateway computes the amounts, the pool verifies them from the announced coinbase.
- The commitment is a single data push after `OP_RETURN` with the tag `PHOT`. It cannot be mistaken for the attestation output, which the node recognizes by the shape `<height><32 bytes>` (two pushes), and it does not start with the witness commitment magic.
- The scriptSig carries the BIP 34 prefix from the node, optionally followed by an extranonce (section 8). Every variant must be attested by the node.

## 7. Verifiable accounting

- Credits are leaves `(script, credit)` of a binary Merkle tree. `accounting_root` is its root, committed on-chain in every block (section 6.2).
- A miner requests a Merkle proof for its leaf (`EpochCommit`) and checks it against the root found in the confirmed block. A pool that reports other credits than it committed is caught by the first miner that checks.
- The plan of the next block must follow deterministically from the committed state (same rules, same data). A gateway recomputes the plan from the previous root and the share log hash chain it received and refuses a plan that does not follow.
- Shares are logged per channel in a hash chain, so a miner can prove its submitted work.

This does not make the pool trustless (it still decides which shares count), but every deviation becomes provable.

## 8. Search space without extranonce limits

Today the fixed coinbase leaves about 2^44 header values per candidate and nTime step, which does not suffice for large farms. With an own node a gateway creates **coinbase variants**:

- Variant id `v` is placed in the scriptSig after the BIP 34 prefix (a few bytes). Each variant changes the coinbase txid and therefore needs its own attestation.
- The gateway asks its node for a batch of variants at once (batch attestation RPC with `script_sig_suffix`, `payout_script` and `extra_outputs` per candidate) and gets one attestation per variant.
- Each variant is an independent search space for the local ASICs (distributed over Stratum jobs as in the pool-template mode: unit = variant, nTime offset, version slice).
- Sizing: a farm needs about one variant per 2.8e14 H/s and second of nTime use. A 1 PH/s farm needs a handful of variants per second, a 1 EH/s farm a few thousand, which a node can attest in a fraction of a core once the batch RPC is native.

## 9. Share verification

The pool never needs the transactions of a gateway's template. For every `TemplateAnnounce` it checks:

1. The tip hash is the current tip.
2. The coinbase outputs equal the plan: same scripts in plan order, amounts consistent with `reward = coinbase value`, fee output present, accounting commitment equal to the plan's root.
3. The coinbase value does not exceed `subsidy + fees` plausibility bounds (subsidy from the schedule, fees bounded by the block weight limit).
4. The merkle root of each share follows from the announced coinbase txid and branch.
5. **Attestation spot check:** with a probability that adapts to the gateway's history the pool assembles the candidate (needs the transactions only for this sample, requested with `GetTransactions`) and validates it with `getblocktemplate mode=proposal`. A failed check invalidates all shares of that variant and lowers the gateway's trust, repeated failures end the session.

Shares that meet the share target but whose block would be invalid cost the pool nothing in block revenue because the gateway submits blocks itself; they only risk unearned credit, which the spot checks limit.

A gateway submits any block it finds through its own node first, then reports `BlockFound`. The pool cannot withhold or steal a block.

## 10. Timeline of one block

1. Tip `h` arrives at the gateway's node (and at the pool).
2. The gateway already holds `PayoutPlan(h+1)`. It builds the template, asks the node for the batch of variants with the coinbase from the plan (one local call).
3. It sends jobs to the ASICs through its local Stratum server and announces the variants to the pool.
4. ASICs hash; shares are sent in batches (a few per second per channel).
5. A found block is submitted by the gateway's node and reported.
6. The pool closes the epoch, commits the new accounting root, publishes `PayoutPlan(h+2)`.

The time from the new tip to the first job is dominated by the node's attestation call, not by the network.

## 11. Compatibility and migration

| Phase | What | Needs |
|---|---|---|
| 0 | Pool-template mode today (Stratum V1), ledger payouts | done |
| 1 | Batch attestation RPC in the node (as a patch shipped with the pool) | done, see the batch RPC guideline |
| 2 | Photon wire protocol, gateway with own node, shares and announcements, spot checks, payout plan | this document |
| 3 | Non-custodial coinbase payouts and the accounting commitment | needs phase 1 with `extra_outputs` |
| 4 | Stratum V2 on the gateway side (standard channels towards ASICs) | optional |
| 5 | Optional share chain for a pool-less mode (shares as linked mini-blocks, plan derived from the chain) | research |

Legacy miners keep working on the same pool: they remain in the ledger and are paid by the existing worker; only Photon channels take part in the coinbase split and the commitment. The coinbase of a block mined by a gateway pays the plan, so the plan has to include the legacy miners' pooled share (as one pool output holding their funds) until they migrate.

## 12. Threat model

| Threat | Defense |
|---|---|
| Pool reports false credits | Committed accounting root, Merkle proofs, deterministic plan |
| Pool withholds or steals a found block | The gateway's node submits the block itself |
| Pool changes the plan to favour itself | Plan recomputed by gateways from the committed state; deviating plans are refused |
| Gateway sends invalid attestations to farm credit | Spot checks with adaptive probability, invalidation of the variant, trust score, ban |
| Gateway mines on a stale tip | All messages carry the tip hash; stale shares are rejected |
| Duplicate or replayed shares | Share key is the header hash, per-epoch dedupe |
| Fake hashrate | Share proof of work is checked by the pool |
| Dust and Sybil accounts | Dust limit, `N_max`, carry-over of small credits |
| Connection flooding | Noise authentication, per-IP and per-channel limits, bounded frames |
| Reorgs | Plans and announcements are tied to a tip hash; credits are settled only for blocks that stay in the chain |

## 13. Comparison

| | Stratum V1 (today) | Stratum V2 | DATUM | P2Pool | Photon |
|---|---|---|---|---|---|
| Miner-built templates | no | with job declaration | yes (own node) | yes | yes |
| Non-custodial payout in coinbase | no | pool dependent | yes | yes | yes |
| Verifiable accounting | no | no | pool dependent | by share chain | Merkle root on-chain |
| Wire format | text | binary, encrypted | Stratum plus own | own | binary, encrypted |
| Needs an own node | no | optional | yes | yes | for miner-built mode |
| Extranonce limitation on Elektron Net | blocks scaling | blocks scaling | n/a | n/a | solved through attested variants |

Photon follows the goals of DATUM and borrows the framing of Stratum V2. What is specific to Elektron Net is the attestation: the coinbase must be attested by the node that builds it, which is why the batch attestation RPC is the key building block and why a light pruned node is an advantage.

## 14. Implementation plan in this repository

1. **Node patch** (`docker/node/patches/`): `getattestationbatch` with `outputs`, `tail_outputs` and `script_sig_suffix` per candidate, applied when the node image is built. It does not change consensus code paths. Done and tested against the Python oracle, including the multi-output coinbase with commitment and extranonce of this protocol.
2. **`photond` batch backend**: done (`pool.backend = auto`), replaces the one-`getblocktemplate`-call-per-address bootstrap backend.
3. **`src/photon/`**: frame codec and Noise session, message types, channel state, plan derivation and Merkle commitment, share verifier with spot checks.
4. **Gateway** (`photon-gateway` binary): local node client, variant generation, local Stratum V1 server reusing `stratum.rs`, Photon client.
5. **Tests:** codec round trips and fuzzing, plan determinism, Merkle proofs, regtest with gateway, pool and two nodes (a block built by a gateway's node is accepted and pays the plan), simulation of many gateways.

## 15. Checklist

- [x] Batch attestation RPC as a node patch, equivalence against the oracle.
- [x] Coinbase with plan-style outputs, commitment and extranonce accepted by an unmodified validating node on regtest (`test/step14_batch_attestation.py`).
- [ ] Message definitions and test vectors frozen.
- [ ] Plan derivation specified and implemented deterministically.
- [ ] Spot check cost and adaptive probability measured.
- [ ] Gateway with local Stratum V1 for ASICs.
- [ ] Independent review of the Noise handshake and the key certification.

## 16. Open Questions

1. Noise suite and certificate scheme (25519 or secp256k1, who signs the pool key).
2. Whether the reward fraction rules need the exact fee amount of the gateway's template or a bound only.
3. Maximum number of coinbase outputs under block weight and attestation cost on mainnet.
4. How legacy ledger miners and Photon channels share one block reward in the plan.
5. Whether the accounting commitment should cover the share log as well as the credits.
6. Share chain design for a later pool-less mode.
