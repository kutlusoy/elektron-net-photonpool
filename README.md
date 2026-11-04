# elektron-net-photonpool

Headless, high-throughput mining pool for [Elektron Net](https://elektron-net.org), built for 1,000 to 10,000 ASIC miners.

Status: release candidate `0.2.0-rc1` for the first mainnet test. The Rust daemon (`photond`) offers solo (direct coinbase payout), PPLNS and PPS with a Stratum V1 server, a SQLite ledger, payouts (dry-run by default), a public pool page and a personal miner page. Tested on regtest against unmodified nodes with an independent attestation oracle; not yet tested on real ASIC hardware (first test: a Bitaxe on mainnet, see [`doc-elektron/guideline-release-and-bitaxe-test.md`](doc-elektron/guideline-release-and-bitaxe-test.md)). Stratum V2 is not implemented. Architecture: [`doc-elektron/guideline-photon-architecture.md`](doc-elektron/guideline-photon-architecture.md). Changes: [`CHANGELOG.md`](CHANGELOG.md).

## Goals

- Run as a single headless daemon (config file, signals, admin API, Prometheus metrics).
- Serve Stratum V1 and Stratum V2 from one core, plus an optional compact edge protocol for farm proxies.
- Keep the mandatory per-block UTXO attestation untouched. No consensus changes.
- Minimise wasted hashes: instant tip-change propagation, unique work per miner, no duplicate shares.
- Optional web UI served by the daemon; the pool works fully without it.

## Photon protocol

A binary, Noise-encrypted protocol for miner-built templates, non-custodial coinbase payouts and an on-chain accounting commitment. The protocol core (framing, messages, transport, payout plan, accounting tree, coinbase and share checks) is implemented and tested in `src/photon/`; the pool session and the gateway are next. See [`doc-elektron/guideline-photon-protocol.md`](doc-elektron/guideline-photon-protocol.md). The optional node patch `getattestationbatch` (`docker/node/patches/`, off by default) attests many coinbase candidates in one call, see [`doc-elektron/guideline-batch-attestation-rpc.md`](doc-elektron/guideline-batch-attestation-rpc.md).

## Tests

```text
cargo test                                  # 61 unit tests
ELEKTROND=... test/run_all.sh               # integration steps 1-16 against regtest nodes (see the header of run_all.sh)
```

The integration tests recompute every attestation independently in Python and validate candidates with the node itself. Load figures (10,000 miners, full blocks) are in [`doc-elektron/guideline-daemon-core.md`](doc-elektron/guideline-daemon-core.md).

## Quick start

See [`Quick-Install.md`](Quick-Install.md) for Docker and bare-metal setup, node configuration and connecting miners. `stack/` installs the complete stack (node, pool, HTTPS) with one script, see [`doc-elektron/guideline-docker-stack.md`](doc-elektron/guideline-docker-stack.md).

## Build and run

```text
cargo build --release
target/release/photond -c photond.toml check
target/release/photond -c photond.toml run
```

See [`doc-elektron/guideline-daemon-core.md`](doc-elektron/guideline-daemon-core.md) for configuration and status.

## Operator UI

`ui/` contains a dependency-free terminal-style dashboard. Open `ui/index.html` in a browser for the simulated demo (marked `DEMO DATA`), or serve the folder next to the pool admin API at `/api/v1`. See [`doc-elektron/guideline-admin-api-and-ui.md`](doc-elektron/guideline-admin-api-and-ui.md).

## Links

- Website: https://elektron-net.org
- X: @elektronnet, @kutlusoy
- Telegram: @elektronnet
- Main repository: https://github.com/kutlusoy/elektron-net
