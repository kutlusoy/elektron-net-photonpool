# Changelog

## 0.2.0-rc1 (release candidate for the first mainnet test)

Pool (`photond`)
- Stratum V1 server with vardiff, version rolling, empty `extranonce1` and `extranonce2_size = 0`, lock-free work allocation; 15,000 simulated connections on one 4-core box in 115 MB RSS.
- Payout modes solo (direct coinbase payout, no fee), PPLNS and PPS in one pool; mode per port or `m=` in the password; PPS paused until the wallet reserve is funded.
- SQLite ledger, maturity tracking (101 confirmations), batched `sendmany` payouts with crash recovery, dry-run by default, encrypted wallet support.
- Public pool page and personal miner page on a separate listener with rate limiting; terminal-style operator UI and read-only admin API.
- Candidate backends: `gbt` (one `getblocktemplate` per payout address, unmodified node) and `batch` (`getattestationbatch`, Photon node patch), `pool.backend = auto`.
- Photon protocol (`src/photon/`): framing, messages, Noise transport, payout plan, accounting tree, coinbase check, share verifier, pool session (`[photon]` in `photond.toml`, off by default) and `photond gateway` with a Stratum V1 front end for ASICs (`gateway.example.toml`) and a CPU miner for tests. The first Bitaxe test uses the normal pool Stratum port.

Node patch (opt-in, `docker/node/patches/`)
- `getattestationbatch`: one call attests many coinbase candidates (about 0.07 ms per candidate). No consensus code path is changed. Not applied by default.

Docker
- `docker-compose.yml` (node and pool) and `stack/` (node, pool, Caddy with HTTPS, encrypted wallet). The node is built from `elektron-net` `main`.

Tests (`test/`)
- 19 integration steps, a spot check load test with simulated gateways (`test/load_spotchecks.py`, `photond photon-load`) against unmodified regtest nodes with an independent Python attestation oracle, 61 Rust unit tests, a differential test of the batch RPC and a full-block load simulation (11,000 transactions per block, 10,000 miners).

Photon spot checks
- Per-tip budget (`max_checks_per_tip`, default 8) and concurrency limit (`max_concurrent_checks`, default 2); a node that cannot answer makes a check inconclusive instead of failing the gateway; the gateway splits large attestation requests below the node's 32 MB RPC limit. Load figures in `doc-elektron/guideline-photon-protocol.md` section 14c.

- Gateway trust is remembered per address and per payout script (kept in the ledger), spot checks are selected proportionally to each gateway's own probability, a payout script is banned for an hour after three failed checks.

- One gateway serves several payout addresses: Stratum users `<payout address>.<worker>` get their own Photon channel (`max_channels_per_connection`, default 64), each owner verifies its own credit.

Known limits
- No real ASIC hardware test yet (first one: Bitaxe, see `doc-elektron/guideline-release-and-bitaxe-test.md`), Stratum V2 not implemented, no mainnet-scale benchmark, Docker images not yet built in CI.
