# elektron-net-photonpool

Headless, high-throughput mining pool for [Elektron Net](https://elektron-net.org), built for 1,000 to 10,000 ASIC miners.

Status: early development. The Rust daemon (`photond`) with a Stratum V1 server and the operator UI prototype exist. Accounting, payouts, hardware tests and Stratum V2 are next. See [`doc-elektron/guideline-photon-architecture.md`](doc-elektron/guideline-photon-architecture.md).

## Goals

- Run as a single headless daemon (config file, signals, admin API, Prometheus metrics).
- Serve Stratum V1 and Stratum V2 from one core, plus an optional compact edge protocol for farm proxies.
- Keep the mandatory per-block UTXO attestation untouched. No consensus changes.
- Minimise wasted hashes: instant tip-change propagation, unique work per miner, no duplicate shares.
- Optional web UI served by the daemon; the pool works fully without it.

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
