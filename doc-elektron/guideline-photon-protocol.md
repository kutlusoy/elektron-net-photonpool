# Elektron Net: `elektron-net-photonpool` Photon Protocol Guideline

- **Version:** 0.16 (first hardware test with a Bitaxe: a header covers about 2^37 hashes, not 2^44, so units are used up far faster; the pool adapts job interval and work space by itself; attestation cost measured against the set size; UTXO growth explained; full 16-bit version roll for ASICs, automatic variant count, measured Stratum traffic; everybody is paid, no minimum, parts per billion, turn-taking above the output limit, capacity limits; prior art, placement of the commitment; automatic reconnect of the gateway; several channels per gateway; trust memory and proportional spot check selection; spot check budget, measured under load; implemented in `src/photon/`: codec, messages, Noise transport, plan, accounting tree, coinbase check, share verifier, pool session, gateway with Stratum V1 front end for ASICs and a CPU miner for tests)
- **Date:** November 5, 2026
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
| `PayoutPlan` | pool to gateway | for height `h+1`: plan hash, accounting root, list of `(script, ppb)` outputs, fee output, rules version (section 6) |
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

1. Take the credits (PPLNS-style window or another rule fixed by `rules_version`) of the window ending at block `h-2`.
2. **Everybody with a credit is paid.** There is no minimum amount and no minimum part; the plan holds nothing back. A credit becomes a weight, the weights are normalised so that payouts plus the pool fee are exactly the whole reward, and each output is expressed in **parts per billion (ppb)** of the block reward (at the current subsidy of 5 coins one part is half a lepton, so only a credit worth less than one lepton per block is rounded away; that remainder goes to the fee output).
3. Up to `N_max` credits (default 200) are all paid in every block.
4. With more credits than `N_max` the plan keeps the **largest `N_max - N_max/4` credits in every block** and lets the rest take turns: the remaining credits, ordered by script, are split into `g = ceil(rest / (N_max/4))` groups, the group `h mod g` is in the plan of block `h`, and its credit is multiplied by `g`. Every credit is therefore paid at its full value once per cycle of `g` blocks (about every `g` minutes with 60-second blocks), nobody is dropped, and a plan never has more than `N_max` outputs. The multiplication keeps the long-run share equal to the credit's share; the share of a single block fluctuates slightly with the size of the group.
5. Order the outputs by part descending, then script; compute `plan_hash` over the canonical serialization and `accounting_root` over **all** credits (section 7).

**Who is paid, and custody.** The unit of payment is the **payout address of a gateway**: one output per address, paid directly to the gateway (or to the address of one of its channels, for a farm that pays its customers directly). How a farm splits its payout among its own machines or customers is its own business, so the number of outputs follows the number of gateways and addresses, not the number of ASICs. Custody is excluded only for **solo** mining (the coinbase pays the miner directly). The ledger modes (PPLNS, PPS) hold balances by design. For Photon the current design pays every address on chain without accumulating balances; letting the pool collect small Photon amounts and pay them together would be possible later, at the cost of some of the verifiability of the accounting, and is not needed unless the number of outputs becomes a problem.

Why a limit at all: `N_max` is not a dust rule, it protects what is on the other side of the coinbase. ESP32 based miners (Bitaxe and others running ESP-Miner) accept at most 16,384 bytes per Stratum line (`STRATUM_V1_MAX_JSON_LINE_SIZE`), and a job carries the whole coinbase in hex, so a job with more than about 230 p2wpkh outputs cannot be read by them; 200 leaves room for a merkle branch of 14 hashes. See 6.3 for the other limits.

### 6.2 Coinbase layout

```text
vout[0..k-1]   payouts, in plan order, value = floor(reward * ppb / 1_000_000_000); an output worth 0 is left out
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

### 7.1 Where the commitment lives, and what pruning means

The commitment is an output of the **coinbase transaction**, therefore part of the block body. It is not in the block header and cannot be: the header has fixed fields (version, previous hash, merkle root, time, bits, nonce) and adding one would be a consensus change, which this project excludes. The header still commits to it indirectly, because the coinbase txid is a leaf of the merkle root.

Sizes (60-second blocks, 525,600 blocks per year, pruning window 197,280 blocks = 137 days):

| | per block | per year | kept by a pruned node |
|---|---|---|---|
| Commitment output (8 B value, 1 B length, 38 B script) | 47 B | 24.7 MB | 9.3 MB |
| For comparison: 128 payout outputs (31 B each, p2wpkh) | 4.0 KB | 2.1 GB | 783 MB (worst case) |

- **If it were in the header:** 80 B would become 116 B (36 B: 32 B root plus the 4 B tag), a factor of **1.45** (1.40 without the tag). The header chain, which is never pruned, would grow from 42 MB to 61 MB per year, 19 MB more every year, forever. That is why the body is the better place even apart from consensus.
- **Pruning:** a pruned node deletes the block bodies, and with them the commitment, after 137 days. That is much longer than anything the commitment is needed for: a miner compares the root with the pool's `EpochCommit` and its own proof when the block is found and paid (maturity is 100 blocks, about 100 minutes). Whoever wants to prove a credit later keeps a receipt (the coinbase transaction, its merkle branch to the header, and the proof of its leaf; a few hundred bytes), because from the header chain alone the commitment cannot be reconstructed after pruning; unpruned nodes and block explorers keep it.
- **UTXO set and attestation:** the commitment is an `OP_RETURN` output. It is unspendable, never enters the UTXO set and is not part of the attested set. It costs block space only.
- **The real storage cost is the payout outputs**, not the commitment. Everybody is paid, so a block carries up to `N_max` (200) spendable payout outputs: 4 to 6 KB per block, up to 105 million new outputs per year in the worst case (all 200 slots used in every block), about 1.2 GB of block data in a pruned node (197,280 blocks), and the same number of entries in the UTXO set that every node holds and the attestation covers. This is the price of the current design, in which the plan pays every payout address on chain without accumulating balances (see "Who is paid, and custody" in 6.1); section 7.2 explains how the set grows and shrinks. It is bounded by `N_max`, it grows only with the number of distinct payout addresses, and miners can keep it low by using one address for a whole farm and by spending (consolidating) their payouts. The plan has no minimum amount.

### 6.3 Capacity: what limits the number of outputs

Measured or read from the sources, 60-second blocks, regtest node with the `getattestationbatch` RPC:

| Limit | Value | What was done |
|---|---|---|
| ESP-Miner line size | 16,384 bytes per Stratum line; a job with 200 outputs is about 13 KB, with 1 output about 0.7 KB | `N_max` default 200; the gateway warns when a job exceeds 16,000 bytes |
| Block weight reserved for the coinbase | the node reserves 8,000 weight units by default; a coinbase with 128 outputs weighs 16.5 k, with 200 outputs 25.4 k. On a full block this would exceed the block limit | **fixed in the node RPC**: `getattestationbatch` reserves room for the largest coinbase of the request (`test/step22_coinbase_weight.py`: blocks of 324, 288 and 255 transactions for 1, 128 and 200 outputs, 92.8 k, 98.3 k and 97.9 k of a 100 k limit) |
| Attestation time per candidate | 0.21 ms with 1 output, 2.2 ms with 128, 7.2 ms with 512 (about 0.014 ms per output) | a farm needing 100 variants spends about 0.34 s of node time per tip at 200 outputs |
| RPC request size | one candidate with 128 outputs is about 19 KB of JSON; the node accepts 32 MB per request (about 1,500 variants) | `attest_all` splits larger requests |
| Wire frames | plan with 200 outputs about 6 KB, an announced coinbase 6.4 KB, announces are cut at 48 KB (7 variants) | within the 64 KB frame limit |
| Stratum traffic to the ASICs | every job carries the whole coinbase (13,033 B with 200 outputs against about 700 B with one). With the 12 version bits per unit that the pool-style 16 slices leave to the miner, an ASIC needs a new job after 2^44 hashes. **Measured** with 1,000 simulated ASICs of 200 TH/s each (200 PH/s, `test/measure_stratum_traffic.py`, the hash rate is a test knob, no hardware): slices 4.97 jobs/s per ASIC (the 200 ms job interval floor caps it; unlimited it would be about 14), 64.7 KB/s per ASIC, **64.7 MB/s = 518 Mbit/s** in total. | **fixed in the gateway:** every unit of work gets all 16 BIP 320 version bits (`full_version_roll`, default). 0.90 jobs/s, 11.7 KB/s per ASIC, **11.7 MB/s = 94 Mbit/s**, 5.5 times less (16 times less without the floor). The ASIC negotiates the mask `1fffe000`. |
| Work space and variants | **Corrected by the first hardware test (14g):** a Bitaxe uses up five units per second, so one candidate (960 units per 20 s epoch) serves about ten of them. The pool now sizes its candidates by itself from the measured unit demand (`autoscale_candidates`, 1,940 candidates for 15,000 simulated Bitaxes, no overlapping units). The figures in this row assume miners that sweep the whole 2^48 of a unit: one variant offers 2^32 x 2^16 x 60 = 1.65e16 hashes per epoch, so the farm needs about `1.5 x hash rate x 60 s / 1.65e16` variants (16 variants cover 4.5 PH/s, 200 PH/s need about 1,100) | **automatic:** the gateway adds up the hash rate of its ASICs, raises the number of variants of a template (between `variants` and `max_variants`, default 16 and 2,048) and rebuilds the template for the same tip when ASICs join later (no clean job). It warns if the work space of a template is used up. At 1,066 variants the node attests in about 3.6 s per tip (3.4 ms per variant at 200 outputs) and the announce is about 7 MB per tip. |
| UTXO set and block data | see 7.1 | accepted |

### 7.2 UTXO set growth

**What the number is.** The UTXO set is the set of all outputs that exist and have not been spent. Every block changes it in two ways: it **adds** the spendable outputs it creates (here: the payout outputs of the coinbase) and it **removes** the outputs that its transactions spend. The size after a block is

```text
size(h) = size(h-1) + outputs created in h - outputs spent in h
```

The 105 million per year in 7.1 are only the "created" side, in the worst case (200 payout outputs in every block, 525,600 blocks per year). Whether the set grows by that much depends on the "spent" side, and the pool does not control it.

**It can shrink.** Spending several outputs in one transaction that creates fewer outputs removes entries. A miner who merges 50 small payouts into one output removes 50 and adds 1, a net change of -49. If more outputs are merged or spent than new payouts arrive, the set gets smaller overall. This is the normal way a UTXO set shrinks, and it is the reason to consolidate.

**It does not shrink by itself.** An output that nobody spends stays in the set forever; maturity (100 blocks, about 100 minutes) only delays when it can be spent. Pruning does not help either: it deletes old block data after 197,280 blocks (137 days), not the UTXO set. Only spending shrinks it, and every merge costs the miner a fee, so it happens only if miners choose to do it.

**Worked example.** A farm with one payout address is in the plan of every block (it is among the largest credits) and receives one output per block.

| Behaviour of the farm | Outputs of this farm in the set after one day (1,440 blocks) |
|---|---|
| never spends | 1,440 |
| merges all its payouts once a day into one output | 1 after the merge (1,440 outputs were added during the day and 1,439 of them removed again by the merge) |
| spends the payouts as they arrive, leaving a change output | about 1 at any time |

If every miner consolidates about as fast as payouts arrive, the set does not grow with time. It then stays near **one output per active payout address**, plus the payouts of the last few blocks that are not merged yet. The growth is therefore bounded by the number of distinct payout addresses, not by the number of blocks.

**What the plan does to the number of outputs.** Payouts to the same script are separate outputs (one per block and script), so one address per farm creates far fewer outputs than one address per machine, and a farm with several addresses (several channels of a gateway) creates one set of outputs per address. For credits beyond `N_max` the turn-taking of 6.1 pays a small miner one output per cycle instead of one per block: with 22 groups that is 22 times fewer outputs for the same sum.

**Cost of a large set.** Every node has to keep the set (chainstate on disk and the cache in memory); a rough figure is on the order of 100 bytes per entry, which would put the worst case at around 10 GB of additional chainstate per year if nothing were ever spent (an estimate, not measured). The attestation is an incremental accumulator over the set, so each block costs time in proportion to the outputs it creates and spends, not to the size of the set. This was **measured at the node** (`test/step23_attestation_scaling.py`, regtest, node with the `getattestationbatch` RPC, medians of 5 blocks of 200 transactions that each spend one output and create two):

| Outputs in the set | `getattestationbatch` (template and attestation) | `submitblock` (validation including the attestation) | `gettxoutsetinfo muhash` (a full pass over the set) |
|---|---|---|---|
| 2,346 | 17.5 ms | 13.3 ms | 30.2 ms |
| 23,152 | 17.2 ms | 13.2 ms | 155.4 ms |
| 73,658 (31 times more) | 14.2 ms | 13.2 ms | 512.4 ms |

The cost of a block of the same shape stays the same while the set grows 31-fold; a pass over the whole set grows 17-fold. A block that recomputed the accumulator over the set would have cost about as much as the last column. The cost does follow what the block does: blocks of 50, 200 and 800 transactions took 4.8, 15.1 and 42.4 ms at the largest size (about 0.05 to 0.1 ms per transaction), and a coinbase costs about 0.014 ms per payout output (section 6.3). Limits of the test: the whole set fits in the cache (a real chainstate also pays for disk reads when it looks up the outputs a block spends, which is a cost of validation in general and not of the attestation), the time of `submitblock` contains more than the attestation (script checks, writing the block), and the sets are small compared to a mature mainnet. The size of the set therefore matters for disk, memory and the time to sync, not for the per-block attestation.

**What we can and cannot do.**

- We cannot make miners consolidate. We can tell them to: use one address per farm, merge payouts from time to time (preferably when fees are low), and do not leave many small payouts unspent.
- `N_max` caps the creation side at 200 per block. Lowering it lowers the worst case but, for more credits than slots, makes the cycles longer.
- A pool that wants a hard growth bound would have to collect small amounts and pay them in larger, rarer outputs. Custody is allowed for the ledger modes and could be allowed for Photon, but it is not used there at present, so this growth is accepted.
- To see what really happens, an operator can count the outputs created by the plan per day (`N_max` times blocks found by gateways) and compare with the growth of the node's UTXO set (`gettxoutsetinfo`); the first is an upper bound of the pool's contribution, the second includes everything on the chain.

## 8. Search space without extranonce limits

Today the fixed coinbase leaves about 2^44 header values per candidate and nTime step, which does not suffice for large farms. **Correction after the first hardware test (14g):** this number assumes that a miner sweeps all granted version bits for one header. A Bitaxe (BM1370, ESP-Miner 2.9.0) sweeps only about 2^37 hashes per header (a pass of about 160 ms at 1 TH/s), so a unit lasts 128 times shorter than assumed, and the figures below that are derived from 2^44 are lower bounds on the work space that is needed. With an own node a gateway creates **coinbase variants**:

- Variant id `v` is placed in the scriptSig after the BIP 34 prefix (a few bytes). Each variant changes the coinbase txid and therefore needs its own attestation.
- The gateway asks its node for a batch of variants at once (batch attestation RPC with `script_sig_suffix`, `payout_script` and `extra_outputs` per candidate) and gets one attestation per variant.
- Each variant is an independent search space for the local ASICs (distributed over Stratum jobs as in the pool-template mode: unit = variant, nTime offset, version slice).
- Sizing: a farm needs about one variant per 2.8e14 H/s and second of nTime use. The exact figure is in 6.3 (variants = 1.5 x hash rate x 60 s / 1.65e16): about 6 for 1 PH/s, 1,100 for 200 PH/s, and the gateway computes it from the hash rate of the attached ASICs.

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
| Many tiny accounts (Sybil) | `N_max` bounds the outputs per block and everybody is still paid in turns; the cost is paid by the attacker's own tiny credits and by the UTXO set (6.3, 7.1) |
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

## 13a. Prior art and what is specific here

The concept is **not new**. Miner-built block templates with payouts written into the coinbase exist for Bitcoin:

- **DATUM** (OCEAN, announced September 2024, still beta according to its release notes): a gateway runs next to the miner's own node, builds the template, talks Stratum V1 to the ASICs and an encrypted link to the pool; the pool supplies the payout split and the payouts are coinbase outputs, so the pool holds no miner funds. Sources: [NO BS Bitcoin](https://nobsbitcoin.com/ocean-launches-datum), [D-Central](https://d-central.tech/datum-protocol/), [Gateway releases](https://github.com/OCEAN-xyz/datum_gateway/releases/tag/v0.4.0beta), [Spark analysis](https://www.spark.money/research/bitcoin-decentralized-mining-pool-ocean-demand). The Photon gateway model is the same architecture.
- **Stratum V2 Job Declaration**: miners declare their own templates, the pool does accounting and payouts. Reports and forum threads state that stock SV2 gives the pool no way to make the payout split checkable before a block is found and discuss extensions (a pool-signed commitment to the split, multi-output or CTV based payouts). Sources: [specification](https://stratumprotocol.org/specification/06-job-declaration-protocol), [Delving Bitcoin: non-custodial payouts](https://delvingbitcoin.org/t/scaling-noncustodial-mining-payouts-with-ctv/1753/14), [Delving Bitcoin: SV2 extensions for payout verification](https://mirror.b10c.me/delvingbitcoin-org/2026-08-14-sv2-extensions-for-payout-verification-and-job-size-at-scale-id2805.html).
- **P2Pool and P2Pool v2**: no central pool; a share chain defines the payouts, which are written into the coinbase of each found block.

What is specific to Elektron Net and this design:

| Aspect | Prior art | Photon |
|---|---|---|
| Coinbase freedom | free, extranonce rotation is cheap | every coinbase byte is bound to the node's UTXO attestation, so each variant needs its own attestation: `getattestationbatch` and variants per template (sections 8, 9) |
| Checking the pool's accounting | share chain (P2Pool), unclear for DATUM, proposals for SV2 | merkle root of all credits in every block plus a proof per payout script (section 7) |
| Checking the gateway | not needed for payouts | the attestation can only be validated by a node, so the pool spot-checks announced variants with a budget and a trust score (guidelines 14c, 14d) |
| Plan timing | not known | plan for height `h+2` is published when `h` arrives, so gateways need no round trip after a new tip |

Limits of this comparison: the sources are secondary (press, blogs, forum threads) and were found with two web searches; a source for the merkle-root-of-credits commitment in the coinbase was not found, but that is not proof that none exists. The ecosystem differs too: DATUM and Stratum V2 have deployed pools and firmware clients, while the only Photon gateway is the one in this repository, tested on regtest with a simulated ASIC and not reviewed by a second person. "Protocol" is generous: this is a profile built from known parts (Noise transport, SV2-like framing, a DATUM-like gateway model) plus the Elektron-specific pieces in the table. Open decision: whether to pursue interoperability with Stratum V2 (see the landscape review) or stay with this profile.

## 14. Implementation plan in this repository

1. **Node RPC** (branch `photonpool` of `elektron-net`): `getattestationbatch` with `outputs`, `tail_outputs` and `script_sig_suffix` per candidate, built into the node. It does not change consensus code paths. Done and tested against the Python oracle, including the multi-output coinbase with commitment and extranonce of this protocol.
2. **`photond` batch backend**: done (`pool.backend = auto`), replaces the one-`getblocktemplate`-call-per-address bootstrap backend.
3. **`src/photon/`**: frame codec and Noise session, message types, channel state, plan derivation and Merkle commitment, share verifier with spot checks.
4. **Gateway** (`photon-gateway` binary): local node client, variant generation, local Stratum V1 server reusing `stratum.rs`, Photon client.
5. **Tests:** codec round trips and fuzzing, plan determinism, Merkle proofs, regtest with gateway, pool and two nodes (a block built by a gateway's node is accepted and pays the plan), simulation of many gateways.

## 14a. As built (v0.2)

Code: `src/photon/` (`wire.rs`, `msg.rs`, `noise.rs`, `merkle.rs`, `plan.rs`, `coinbase.rs`, `verify.rs`, `cli.rs`). Tests: 37 unit tests (`cargo test photon`) and `test/step16_photon_plan_coinbase.py` (Rust plan and checker against the patched node).

- **Framing:** `ext u16 | type u8 | length u24 | payload`, payload cap 64 KiB, the cap is enforced when the header is complete (before buffering). Messages 0x00-0x0f in extension 0 (`Setup`, `SetupAck`, `OpenChannel`, `OpenChannelAck`, `NewTip`, `PayoutPlan`, `TemplateAnnounce`, `Shares`, `ShareAck`, `SetTarget`, `BlockFound`, `EpochCommit`, `GetProof`, `Proof`, `Reject`, `Disconnect`). Decoding is strict: every collection is capped, truncated or trailing bytes are errors. Three wire vectors are frozen in `msg.rs`; a change there is a protocol change.
- **Transport:** `Noise_NX_25519_ChaChaPoly_SHA256`, prologue `photon/1`. The gateway pins the pool's static key (decision for v1: key pinning instead of a certificate authority, see open question 1). Noise messages carry a u16 length prefix; frames are cut into 65,519-byte chunks and reassembled.
- **`TemplateAnnounce`:** carries the non-witness coinbases of a batch of variants (at most 64 per message) plus one merkle branch (the transaction set is the same for all variants). Shares address a variant by number; the pool recomputes `merkle root = coinbase txid + branch` and the header (BIP 320 version rolling mask `0x1fffe000`, ntime advance at most 7200 s).
- **Plan rules (version 2, see 6.1):** ppb = floor(weight * (1e9 - fee_ppb) / total weight); all credits are paid, with turn-taking above `n_max`; outputs ordered by ppb descending then script. `plan_hash = SHA256("PHOTON-PLAN-V2" || rules || height || tip || root || outputs || fee)`. A gateway refuses a plan whose hash, bounds, ordering or fee terms deviate (`check_plan`).
- **Accounting tree:** leaf `SHA256(0x00 || u16 len || script || u64 credit)`, node `SHA256(0x01 || l || r)`, odd nodes are promoted unchanged, root `SHA256(0x02 || u32 count || top)`; leaves sorted by script, scripts unique. The frozen root vector was computed independently in Python.
- **Coinbase:** payouts in plan order (value = floor(reward * ppb / 1e9), outputs worth 0 are left out), fee output with the remainder (omitted when 0), attestation, witness commitment, `OP_RETURN "PHOT" || root`. The checker (`check_announced`) verifies layout, values, locktime, BIP 34 height, the shape of the attestation and witness commitment outputs and a reward bound. It cannot verify the attestation digest itself; that is the node's spot check (`Trust`: probability 0.5 decaying to 0.02 with clean checks, multiplied by 4 per failure, ban after 3 failures).
- **Measured end to end** (`step16`): 4 extranonce variants of a 40-miner plan are attested in one node call, accepted by the Rust checker and by the node's validator, one block is accepted by `submitblock` and pays exactly `floor(reward * ppb / 1e9)` per miner with the root on chain.

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

## 14f. Reconnect (v0.8)

The gateway keeps its state outside the pool connection (`Gw`): node link, plans, current template, the Stratum server and a channel table with gateway-local ids. A lost connection (pool restart, network, idle timeout) ends only the connection:

- **Backoff:** 1 s, 2 s, 4 s ... up to `max_reconnect_secs` (default 30) plus a small jitter, reset when a connection held for 30 s. `reconnect = false` makes the first failure an error.
- **Never retried:** a pool key that does not match the pinned one, `Disconnect` with code 21 (banned) or 23 (protocol version), a refused default channel, a plan commit that differs from its plan, an accounting proof that does not verify. These end the process with an error, because retrying cannot help and a proof failure should be seen by the operator.
- **During the outage:** the node link and the Stratum server stay up, the template follows the node's tip as long as the plan for the new height is known (the gateway holds the plans of the current and the next block, so ASICs lose nothing for about one block); found blocks go to the own node and pay the last known plan; shares are lost because the pool is not there to credit them. Workers of known addresses can connect, a new address is refused with `the pool connection is down, try again shortly`.
- **After the reconnect:** the gateway re-opens every channel (the request number of `OpenChannel` is the local channel id, so ASIC sessions keep their ids although the pool assigns new ones), re-announces the template of the current tip, requests new plans and proofs, and the pool continues with the credits it kept in the ledger. The pool writes pending share aggregates and trust to the ledger on a graceful shutdown (`SIGINT`).
- **Test:** `test/step20_gateway_reconnect.py`: a worker mines three blocks, the pool is stopped, the gateway reports the lost connection, a worker of the known address connects, a new address is refused, a block mined during the outage is accepted and pays the last plan, the pool is started again, the gateway is back in about 4 s with both channels, two more blocks are credited, the coinbase pays the old credit, and A's credit is verified again; a wrong key ends the gateway at once, an unreachable pool with `reconnect = false` is an error. Passed 4 of 4 runs.

Limits: an outage longer than the known plans (about one block) stops the work, because the gateway cannot build a coinbase without a plan; a restart of the gateway itself is not covered (run it under a supervisor); the pool does not yet tell the gateway how long it will be away.

## 14g. First hardware test and self-adjusting pool (v0.9)

The first run against real hardware (one Bitaxe Gamma, BM1370, ESP-Miner 2.9.0, through the Stratum V1 front end of the pool, solo mode) found three things that the simulated tests could not show:

1. **`extranonce2_size = 0` makes ESP-Miner queue identical jobs.** The firmware keeps up to ten jobs ahead, each with `extranonce2 + 1`, which is the same job when there is no extranonce space. The chip sweeps the same header again and again
   (the same hit every 163 ms; recomputed from the job, the logged hits are real), new work waits behind the queue for up to 5 s, and results arrive for jobs the pool no longer knows. First result: 98.8 % duplicates, later 83 % "job not found".
2. **The space of a header is about 2^37 hashes**, not 2^44 (section 8).
3. **Fix and result:** every fresh job is marked clean for ESP-Miner (`clean_work_agents`), jobs are sent about every 200 ms, jobs are kept 10 s for late shares. Result with one Bitaxe: 37 shares and one duplicate, one block found, stable.

Consequences for the pool, all measured with simulated Bitaxes (`test/bench_bitaxe_profile.py`, `test/bench_dynamic_growth.py`, see `guideline-stratum-v1.md`):
- 15,000 connections at five jobs per second: 74,000 jobs/s, 316 Mbit/s, 85 % of one core, 205 MB.
- **Work space adapts by itself.** Candidates in use follow the unit demand (`autoscale_candidates`): 115 for 1,000 miners, 657 for 5,000, 1,950 for 15,000, back to the minimum when the miners leave; an exhausted epoch is rebuilt at once. No overlapping units during growth or shrinking.
- **Job interval adapts per connection** (`adaptive_jobs`) from the duplicate and stale shares.

What this does not settle: the behaviour of other firmware and of larger ASICs (open question 7), and whether the Photon gateway, which creates its headers locally, can serve a Bitaxe-like miner with the same cadence over its Stratum front end.

## 15. Checklist

- [x] Batch attestation RPC in the node (branch `photonpool`), equivalence against the oracle.
- [x] Coinbase with plan-style outputs, commitment and extranonce accepted by an unmodified validating node on regtest (`test/step14_batch_attestation.py`).
- [x] Message definitions and first test vectors frozen (`msg.rs`, `merkle.rs`).
- [x] Plan derivation specified and implemented deterministically (rules v1).
- [x] Pool session: listener, channel state, plan publication from the ledger, `TemplateAnnounce` handling, spot-check calls to the node (`step17`).
- [x] Spot check cost measured under load (14c); [ ] mainnet-sized blocks with several hundred real gateways on separate machines.
- [x] Gateway core with CPU miner and local Stratum V1 front end ([ ] tested with real ASIC firmware: the pool's own Stratum port was tested with one Bitaxe, see 14g; the gateway was not).
- [x] First hardware test (one Bitaxe, pool Stratum port): shares accepted, block found; [ ] larger ASICs, [ ] other firmware, [ ] Photon channel with real hardware.
- [x] Dynamic load (miners joining and leaving in waves) with self-adjusting work space, simulated.
- [ ] Independent review of the Noise handshake and the key certification.

## 16. Open Questions

1. Noise suite and certificate scheme (25519 or secp256k1, who signs the pool key).
2. Whether the reward fraction rules need the exact fee amount of the gateway's template or a bound only.
3. How to carry many outputs to ASICs without the Stratum V1 traffic of a full coinbase in every job (section 6.3).
4. How legacy ledger miners and Photon channels share one block reward in the plan.
5. Whether the accounting commitment should cover the share log as well as the credits.
6. Share chain design for a later pool-less mode.
7. How many header values a miner of each firmware really sweeps per job, and how often it needs a new header (Bitaxe: about 2^37, new job about every 160 ms at 1 TH/s; larger ASICs unknown). The adaptive pool measures it per connection; a profile per firmware could set it from the start.
8. Whether a miner without extranonce space can be served at a lower job rate, for example by an own extranonce2 range that stays inside a pre-attested set of coinbases (the txid of the coinbase is attested, so every extranonce value needs its own attestation).
