# Elektron Net: `elektron-net-photonpool` Photon Code Review Guideline

- **Version:** 0.1
- **Date:** November 4, 2026
- **Audience:** Maintainers and reviewers of the Photon code
- **Reference implementation:** [`elektron-net`](https://github.com/kutlusoy/elektron-net) - `src/validation.cpp`, `src/rpc/mining.cpp` (node side); in this repository `src/photon/` (about 4,400 lines) - treat as ground truth for anything the doc references
- **See also:** [`guideline-photon-protocol.md`](./guideline-photon-protocol.md), [`guideline-batch-attestation-rpc.md`](./guideline-batch-attestation-rpc.md), [`guideline-systemd.md`](./guideline-systemd.md)

---

## 1. Scope and method

A single-reviewer pass over all of `src/photon/` (wire, messages, Noise transport, plan, accounting tree, coinbase checks, share verifier, trust book, pool service, gateway, gateway Stratum front end, load generator) with the question "what can a hostile gateway, a hostile ASIC or a hostile pool do, and what happens on a panic or an overload". It was done by the author of the code with the help of the test suite; it is **not** the independent review that the protocol checklist asks for (Noise handshake, key handling, the node patch). Treat the results as a first pass.

Method: read every file against the threat model of the protocol guideline, list every `unwrap`, index and arithmetic on external data, try to find ways to earn credit without work, to exhaust memory, CPU or the node, and to make honest parties fail. Each finding was fixed with a test where a test is possible.

## 2. Findings that were fixed

| # | Severity | Finding | Fix | Test |
|---|---|---|---|---|
| 1 | **Critical** | The announced `bits` were taken from the gateway. A hash that meets the network target is a block and bypassed the share target, so a gateway announcing the easiest target (`0x207fffff`) would be credited for every hash it computed without any work. | The pool compares the announced `bits` with its own node's value for the tip (`check_announce_terms`); a mismatch is refused and counts as a strike. | unit test, `step17` (gateway with a wrong difficulty earns nothing) |
| 2 | High | A gateway could send unlimited chunks for one `template_id` (distinct `first_variant`) and keep up to 16 templates: unbounded memory per connection. | At most 4,096 variants per template and 48 MiB of announced coinbases per connection; the oldest templates are dropped first. | by construction (limits checked in `announce`) |
| 3 | High | `panic = "abort"` in the release profile: any panic in one session task would have taken down the whole pool. | The release profile uses unwinding; a panicking task ends only its connection. | robustness tests below |
| 4 | High | Spot checks were counted as failures when the node could not answer (queue overflow, timeout); honest gateways would have been banned under load. | Only a definitive rejection by the node, a malformed transaction stream or a missing delivery counts; node trouble is "inconclusive". Per-tip budget and concurrency limit. | `test/load_spotchecks.py` |
| 5 | Medium | Connections were only limited globally; one address could use all slots. | `max_connections_per_ip` (default 32; IPv6 counted per /64). | by construction |
| 6 | Medium | The per-connection duplicate filter was never cleared: slow growth over weeks. | Cleared when the shares of the previous tip can no longer arrive; the pool-wide filter (two generations) stays. | `step18`, `step19` |
| 7 | Medium | `GetProof` could be requested without limit. | At most 8 requests per channel and tip, then strikes and disconnect. | by construction |
| 8 | Low | The trust book removed old entries in quadratic time and the counters could overflow. | Hash set for the removal; saturating counters; failures capped at 16 in the probability. | unit tests |
| 9 | Low | Trust per IPv6 address was trivially evaded by rotating addresses inside a /64. | IPv6 trust keys use the /64 prefix. | unit test |
| 10 | Low | Test-only gateway options (`corrupt_attestation`, `announce_bits_override`, `software_miner`) could be left on in production unnoticed. | A warning is logged at startup when one of them is set. | - |
| 11 | High | The batch RPC left only the default 8,000 weight units for the coinbase. A coinbase with 128 or 200 outputs weighs 16.5 k and 25.4 k, so a full block built with it would have exceeded the block weight limit. | The RPC reserves room for the largest coinbase of the request. | `step22` |
| 12 | Medium | ESP-Miner (Bitaxe) reads at most 16,384 bytes per Stratum line; a job with more than about 230 payout outputs is invisible to it. | Default `max_outputs` is 200, the gateway warns about larger jobs, and the plan lets the smaller credits take turns above the limit. | unit test of the size estimate |

Robustness tests added: `random_and_mutated_frames_never_panic` (50,000 random frames and 2,000 mutations of every message type) and `random_and_mutated_coinbases_never_panic` (60,000 random and mutated coinbases through the parser and the plan check). They run in `cargo test`.

## 3. What was checked and found sound

- **Wire and messages:** every length is bounded before allocation (`count`, `bytes16/32`), oversized frames are refused at the header, trailing and missing bytes are errors for every message (tested for all types), the Noise receive path is cancel-safe and bounded.
- **Coinbase check:** all output values are summed with overflow check, the layout is compared byte by byte with the plan, the BIP 34 height, the locktime and the shape of the attestation and witness-commitment outputs are verified; the attestation digest itself is deliberately left to the node (spot check).
- **Plan:** deterministic for any order of the input, integer math in `u128`, no division by zero (guarded), ordering and bounds re-checked by the gateway (`check_plan`), fee terms compared with its configuration.
- **Accounting tree:** domain-separated leaf, node and root hashes, odd nodes promoted (no duplicate-leaf ambiguity), proof verification rejects wrong lengths, extra and missing siblings.
- **Accounting of shares:** pool-wide duplicate filter on the header hash, credit only for hashes that meet the channel target (or the verified network target), held until the spot check of the variant passes.
- **Reconnect and channel table:** the gateway never reuses pool channel ids across connections, and fatal conditions (wrong key, ban, version, failed proof) are not retried.

## 4. Open risks (not fixed)

| Risk | Why it stays | Mitigation today |
|---|---|---|
| The spot check is probabilistic and budgeted. A gateway that cheats only on variants that are not checked earns credit for them. | Checking every variant costs the node a full block validation each time. | Trust-weighted selection, ban per payout script, held shares of checked variants, per-tip budget (guideline 14c, 14d). Shares credited before a later failed check are not clawed back. |
| The gateway's Stratum port has no authentication. Anyone who can reach it can mine for any payout address or exhaust the pool's channel limit with fake addresses. | A farm network is normally closed; authentication would need a convention the ASIC firmware supports. | Listen on the farm LAN only, firewall it, `require_address = true`. |
| Gateways are anonymous (the Noise `NX` pattern authenticates only the pool). A hostile party can open many connections from many addresses. | Identity would need registration, which is a policy question. | Per-address limits, trust per address and per script, budget, channel limit. |
| The pool key is pinned; there is no certificate or rotation scheme. | Open question 1 of the protocol guideline. | Distribute the key out of band; a rotation means all gateways change one setting. |
| The Noise handshake, key handling and the node patch were not reviewed by anyone else. | Needs a second person. | Listed in the checklist of the protocol guideline. |
| Photon shares credited between two accounting flushes are lost if the pool is killed instead of stopped with `SIGINT`. | The ledger is written periodically for speed. | The systemd unit uses `KillSignal=SIGINT`; the flush interval is a few seconds. |
| The gateway needs a node with the Photon patch (not reviewed). | See the patch review plan in `guideline-batch-attestation-rpc.md`. | Differential test against three references. |
| Real ASIC firmware has not been tested against the gateway front end. | Needs hardware. | `step18`/`step19` use a simulated Stratum client. |
| Memory and CPU limits were measured on one machine with regtest-sized blocks. | Needs a larger setup. | `test/load_spotchecks.py` can be run with larger blocks. |

## 5. Checklist

- [x] First review pass over `src/photon/` by the author, findings fixed and tested.
- [ ] Independent review of the Noise handshake, key handling and certificate scheme.
- [ ] Independent review of the node patch (`guideline-batch-attestation-rpc.md`).
- [ ] Fuzzing of the decoders with a coverage-guided fuzzer (the current tests are random and mutation based).
- [ ] Test with real ASIC firmware.
