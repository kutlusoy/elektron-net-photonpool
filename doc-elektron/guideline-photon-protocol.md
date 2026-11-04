# Elektron Net: `elektron-net-photonpool` Photon Protocol Guideline

- **Version:** 0.7 (several channels per gateway; trust memory and proportional spot check selection; spot check budget, measured under load; implemented in `src/photon/`: codec, messages, Noise transport, plan, accounting tree, coinbase check, share verifier, pool session, gateway with Stratum V1 front end for ASICs and a CPU miner for tests)
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

## 14a. As built (v0.2)

Code: `src/photon/` (`wire.rs`, `msg.rs`, `noise.rs`, `merkle.rs`, `plan.rs`, `coinbase.rs`, `verify.rs`, `cli.rs`). Tests: 37 unit tests (`cargo test photon`) and `test/step16_photon_plan_coinbase.py` (Rust plan and checker against the patched node).

- **Framing:** `ext u16 | type u8 | length u24 | payload`, payload cap 64 KiB, the cap is enforced when the header is complete (before buffering). Messages 0x00-0x0f in extension 0 (`Setup`, `SetupAck`, `OpenChannel`, `OpenChannelAck`, `NewTip`, `PayoutPlan`, `TemplateAnnounce`, `Shares`, `ShareAck`, `SetTarget`, `BlockFound`, `EpochCommit`, `GetProof`, `Proof`, `Reject`, `Disconnect`). Decoding is strict: every collection is capped, truncated or trailing bytes are errors. Three wire vectors are frozen in `msg.rs`; a change there is a protocol change.
- **Transport:** `Noise_NX_25519_ChaChaPoly_SHA256`, prologue `photon/1`. The gateway pins the pool's static key (decision for v1: key pinning instead of a certificate authority, see open question 1). Noise messages carry a u16 length prefix; frames are cut into 65,519-byte chunks and reassembled.
- **`TemplateAnnounce`:** carries the non-witness coinbases of a batch of variants (at most 64 per message) plus one merkle branch (the transaction set is the same for all variants). Shares address a variant by number; the pool recomputes `merkle root = coinbase txid + branch` and the header (BIP 320 version rolling mask `0x1fffe000`, ntime advance at most 7200 s).
- **Plan rules v1:** ppm = floor(credit * (1e6 - fee_ppm) / total); entries below `min_ppm` are dropped (credit carries over); at most `n_max` entries (largest credit first, ties by script bytes); ppm recomputed over the kept entries; outputs ordered by ppm descending then script. `plan_hash = SHA256("PHOTON-PLAN-V1" || rules || height || tip || root || outputs || fee)`. A gateway refuses a plan whose hash, bounds, ordering or fee terms deviate (`check_plan`).
- **Accounting tree:** leaf `SHA256(0x00 || u16 len || script || u64 credit)`, node `SHA256(0x01 || l || r)`, odd nodes are promoted unchanged, root `SHA256(0x02 || u32 count || top)`; leaves sorted by script, scripts unique. The frozen root vector was computed independently in Python.
- **Coinbase:** payouts in plan order (value = floor(reward * ppm / 1e6)), fee output with the remainder (omitted when 0), attestation, witness commitment, `OP_RETURN "PHOT" || root`. The checker (`check_announced`) verifies layout, values, locktime, BIP 34 height, the shape of the attestation and witness commitment outputs and a reward bound. It cannot verify the attestation digest itself; that is the node's spot check (`Trust`: probability 0.5 decaying to 0.02 with clean checks, multiplied by 4 per failure, ban after 3 failures).
- **Measured end to end** (`step16`): 4 extranonce variants of a 40-miner plan are attested in one node call, accepted by the Rust checker and by the node's validator, one block is accepted by `submitblock` and pays exactly `floor(reward * ppm / 1e6)` per miner with the root on chain.

## 14b. Pool session and gateway (v0.3)

Pool: `[photon] enabled = true` in `photond.toml` (see `photond.example.toml`). The Noise key is created in `photon.key` on first start (owner-only); the public key is logged and shown in `/api/v1/summary` (`photon.publicKey`) and is what gateways pin.

- **Plans:** on every new tip `h` the pool derives the plan for `h+2` from the ledger window of Photon shares (`MODE_PHOTON`, window = `window_factor` x network difficulty) and keeps the plan for `h+1` that it published one block earlier; at startup or after a reorg it derives the missing one late. Both are pushed to every connection (`NewTip`, `PayoutPlan`, `EpochCommit`). `GetProof` returns a Merkle proof for a script; the reference gateway checks its own credit against the committed root.
- **Channels:** `OpenChannel` accepts witness v0/v1 payout scripts only. The share target is the requested one clamped to the pool limits and then adjusted by vardiff (one share per `share_secs`).
- **Announces:** `Announced::new` validates every variant against the plan of its height (layout, values, BIP 34 height, locktime, reward bound = subsidy plus `max_reward_over_subsidy_pct`); curtime must be within two hours of the pool's clock. Chunks of one template must agree and not overlap.
- **Shares:** header rebuilt from the announced coinbase and branch; share target of the channel, network target flags a block; duplicates are filtered per connection and pool-wide by header hash. Shares are credited to the account `hex(payout script)` in the Photon window.
- **Spot check:** with the probability of the gateway's `Trust` score (0.5 decaying to 0.02) the pool sends `GetTransactions` for a random variant of an announce, assembles the block (zero nonce) and asks the node for `getblocktemplate mode=proposal`, which validates the attestation. While a check runs, shares of that variant are held; they are credited on a pass and dropped on a failure. A failure lowers the trust, three failures end the session. `duplicate` (the node already has this exact block) counts as a pass; `inconclusive-*` (a new tip arrived during the check) is neither pass nor failure.
- **Gateway:** `photond gateway --gateway-config gateway.toml` (fields in `src/photon/gateway.rs`, `GatewayCfg`). It needs the patched node (`getattestationbatch`). It refuses plans whose fee terms differ from its configuration, builds `variants` coinbases per template through the node, announces them in frames under 48 KB, answers spot checks, submits found blocks to its own node and reports them. `software_miner = true` is a CPU miner for tests and regtest.
- **Test:** `test/step17_photon_session.py` (regtest, two nodes' worth of roles on one node): an honest gateway mines six blocks that pay the miner directly in the coinbase, carry the commitment and a proof the gateway verifies; spot checks of an honest gateway pass; a gateway that corrupts the attestation is caught by the spot check and disconnected. Passed three times in a row.

### Gateway Stratum V1 front end (v0.4)

`[stratum] listen` in the gateway config (`gateway.example.toml`) starts a Stratum V1 server for ASICs and farm proxies, built on the same work scheme as the pool's Stratum server: empty `extranonce1`, `extranonce2_size = 0`, version rolling with the pool's slice bits, units = (variant, nTime offset, version slice) of the gateway's own template, vardiff per ASIC (`share_secs`). The gateway's variants are the candidates of its `Epoch`, so no two ASICs get the same work.

- **Shares:** every accepted ASIC share that also meets the pool channel's target (updated by `SetTarget`) is forwarded as a Photon share; the pool credits it with the channel's difficulty. ASIC and channel difficulty are independent, so a farm can run low ASIC difficulties for monitoring.
- **Blocks:** a share that meets the network target is submitted to the gateway's own node first, then reported with `BlockFound`.
- **Routing:** shares are sent under the id of the template they were mined on. The pool keeps templates of the previous tip for 10 seconds, so the final share of a block that moved the tip is still credited. If the gateway's node is ahead of the pool's, the pool waits up to 1.5 s for its own tip before rejecting an announce.
- **Tip changes:** on a new tip the epoch is cleared until the gateway holds the plan for the new height and has announced the new template; ASICs get a clean job then.
- **Test:** `test/step18_gateway_stratum.py` mines four blocks through the Stratum port with the shared test client; blocks are accepted by the gateway's node, shares and block reports arrive at the pool, and the latest block pays the ASIC's address 99 % of the reward and the pool fee at least 1 %. Passed 8 of 8 runs.

Limits of this version: Photon channels form their own window, legacy Stratum miners are not part of the plan (open question 4); a cheater is only caught when a check hits one of its variants and does not finish during a tip change; shares credited before a failed check are not clawed back; the Stratum front end has only been run against the test client, not against ASIC firmware; one gateway uses one channel (one payout address).

## 14c. Load of the spot checks (v0.5)

Measured with `test/load_spotchecks.py` and `photond photon-load`: many simulated gateways (own Noise connection, channel, announces, shares, real transaction sets for the spot checks) against one pool and one node, everything on one machine over loopback (regtest, blocks of 250 to 1,000 transactions because the node was limited by `-blockmaxweight`; 100 s per scenario, a tip every 10 s, 2 variants per gateway, 0.2 shares per second per gateway).

| Gateways | Budget / concurrency | Checks per tip | Check time total / node | Pool CPU | Node CPU | Pool RSS | Failures, drops |
|---|---|---|---|---|---|---|---|
| 100 | 8 / 2 | 8 | 35 / 13 ms | 1 % | 4 % | 35 MB | 0, 0 |
| 500 | 8 / 2 | 8 | 49 / 15 ms | 4 % | 17 % | 92 MB | 0, 0 |
| 2000 | 8 / 2 | 6.2 | 118 / 36 ms | 10 % | 38 % | 275 MB | 0, 0 |
| 500 | 64 / 4 | 64 | 227 / 26 ms | 4 % | 18 % | 114 MB | 0, 0 |

Node CPU includes the load generator's own batch attestation calls (up to 4,000 candidates per tip), which a real pool does not have. Without a budget (first run: every selected gateway is checked, 36 to 182 checks per tip) the node queued the validations: 213 ms average at 100 gateways, 953 ms at 500, and at 2,000 gateways its RPC queue overflowed, the answers were not JSON, and 343 honest checks were counted as failures. That exposed two design errors, both fixed:

- **Infrastructure errors are not gateway failures.** Only a definitive rejection by the node, a malformed or oversized transaction stream, or a missing delivery counts against a gateway. A node that cannot answer makes the check inconclusive.
- **Checks have a budget.** `max_checks_per_tip` (default 8, over all gateways, first come first served among the gateways selected by their trust probability) and `max_concurrent_checks` (default 2). Selected checks beyond the budget are skipped (`checksSkipped`), which costs nothing but coverage.

Cost model: validating a block costs the node about 15 ms at 250 transactions, 36 to 53 ms at 500 to 1,000, and about 0.75 s at 11,000 (the full-block measurement of `getattestationbatch` in `guideline-daemon-core.md`; proposal validation is the same attestation work). With the default budget of 8 per tip and blocks every 60 s, full blocks cost the node about 6 s per minute (10 % of one core); a budget of 64 would need most of a core. The data transferred for a check is the transaction set (0.02 to 0.08 MB here, a few MB for full blocks) and the pool memory per connection is about 100 KB.

What the budget means for security: with N gateways and a steady trust probability of 0.02 the chance that one gateway is checked in a given tip is about budget / (0.02 N), capped by its own probability. At 2,000 gateways and a budget of 8 that is far below one check per hour for an established gateway, so an attacker that behaved correctly for a long time is hard to catch; new connections (probability 0.5) are checked far more often until the budget is used, and a pool restart makes everyone new at once. The spot check therefore limits fraud to what a gateway earns before its first checks; it is not a continuous guarantee. Mitigations: the proportional selection and the remembered trust of section 14d; not implemented: checking shares instead of templates.

Another limit found by the generator: the node's RPC body limit is 32 MB. A candidate with a 128-output plan is about 20 KB of JSON, so about 1,500 variants fit into one `getattestationbatch` call. The gateway and the load generator now split larger requests (`attest_all`).

## 14d. Trust memory and spot check selection (v0.6)

**Remembered trust.** The pool keeps a `TrustBook` (`src/photon/trustbook.rs`) with one entry per network address (`ip:<address>`) and per payout script (`script:<hex>`): passed and failed spot checks, time of the last failure, ban time. A connection uses all keys it knows and takes the highest check probability among them, so a reconnect, a new address with the same payout script, or a new script from a known address does not start from scratch. Entries are written to the ledger (table `photon_trust`) every 30 seconds and on shutdown, loaded at start, and dropped after 30 days without activity.

- **Forgiveness:** one failure is forgiven per 24 hours without a new one.
- **Ban:** three failures ban a payout script for one hour, also on other addresses and across restarts; after the ban a single further failure bans it again. An address is never banned (many gateways can sit behind one address, and a farm behind a NAT must not be locked out by one cheater); its failures only keep the check probability of everything behind it high (up to 1.0 after two failures). In a loopback test all gateways share one address, so all of them share one entry; with real addresses trust is per farm.

**Selection.** Instead of first come, first served, the probability of each announce is its own trust probability divided by `max(1, sum of the probabilities of all connected gateways / max_checks_per_tip)`. The expected number of checks per tip is then the budget, and the share of the budget a gateway receives is proportional to its own probability: new or failed gateways are checked much more often than established ones (0.5 against 0.02 is 25 times), and an established gateway never drops to zero, which a strict "least trusted first" order would allow (a cheater would only have to behave until it is trusted). The hard cap `max_checks_per_tip` stays.

Status: `/api/v1/summary` reports `photon.trustEntries` and `photon.sumP`. Tests: 8 unit tests of the book (shared keys, least trusted key decides, ban and re-ban, forgiveness, pruning, address without ban) and `test/step17_photon_session.py` (trust is remembered, the banned script is refused on reconnect and after a restart of the pool, the honest gateway reconnects).

## 14e. Several channels per gateway (v0.7)

A farm that serves several customers or sub-farms runs one gateway and gives each of them its own payout address as the Stratum user name: `<payout address>.<worker>`. The gateway opens one pool channel per distinct address on demand (the first worker of an address triggers `OpenChannel`, later workers reuse it; the authorize reply waits for the pool, at most 5 seconds, and carries the pool's reason when it refuses). Users without an address use the default channel (`payout_address`); with `[stratum] require_address = true` they are refused.

- **Credit:** every ASIC share is forwarded on its worker's channel and credited with that channel's difficulty to the account of that payout script. The block reward is not affected: the coinbase pays the plan to everybody, whichever worker finds the block.
- **Channel targets:** the pool adjusts the share target per channel (`SetTarget`); the gateway forwards a share when it meets the target of its worker's channel.
- **Limits:** `max_channels_per_connection` at the pool (default 64, a refused open reaches the worker as `channel limit reached`); all channels of a gateway share one connection, one set of announces and one spot check budget, and their payout scripts are all keys of the gateway's remembered trust (a banned script blocks only itself, but a failed check counts against every script of the connection).
- **Verification:** after every `EpochCommit` the gateway requests a proof for each channel's script and checks it against the committed root; the pool answers each request with a `Proof` or a `Reject(8)` (no credit yet), in order.
- **Test:** `test/step19_multi_channel.py`: workers of two addresses mine alternately on one gateway, a worker without an address uses the default channel, a fourth address is refused because the pool allows three channels, the latest coinbase pays each worker address 49.5 % of the reward and the pool fee 1 %, and both owners' credits verify. Passed 5 of 5 runs.

## 15. Checklist

- [x] Batch attestation RPC as a node patch, equivalence against the oracle.
- [x] Coinbase with plan-style outputs, commitment and extranonce accepted by an unmodified validating node on regtest (`test/step14_batch_attestation.py`).
- [x] Message definitions and first test vectors frozen (`msg.rs`, `merkle.rs`).
- [x] Plan derivation specified and implemented deterministically (rules v1).
- [x] Pool session: listener, channel state, plan publication from the ledger, `TemplateAnnounce` handling, spot-check calls to the node (`step17`).
- [x] Spot check cost measured under load (14c); [ ] mainnet-sized blocks with several hundred real gateways on separate machines.
- [x] Gateway core with CPU miner and local Stratum V1 front end ([ ] tested with real ASIC firmware).
- [ ] Independent review of the Noise handshake and the key certification.

## 16. Open Questions

1. Noise suite and certificate scheme (25519 or secp256k1, who signs the pool key).
2. Whether the reward fraction rules need the exact fee amount of the gateway's template or a bound only.
3. Maximum number of coinbase outputs under block weight and attestation cost on mainnet.
4. How legacy ledger miners and Photon channels share one block reward in the plan.
5. Whether the accounting commitment should cover the share log as well as the credits.
6. Share chain design for a later pool-less mode.
