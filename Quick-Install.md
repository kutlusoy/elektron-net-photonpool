# Elektron Net: `elektron-net-photonpool` Quick Install

- **Version:** 0.1 (draft)
- **Date:** November 3, 2026
- **Audience:** Pool operators who want a running pool quickly (Docker or bare metal)
- **Reference implementation:** [`elektron-net`](https://github.com/kutlusoy/elektron-net) - node, RPC and `elektron.conf` options - treat as ground truth for anything the doc references
- **See also:** [`doc-elektron/guideline-daemon-core.md`](doc-elektron/guideline-daemon-core.md), [`doc-elektron/guideline-stratum-v1.md`](doc-elektron/guideline-stratum-v1.md), [`doc-elektron/guideline-admin-api-and-ui.md`](doc-elektron/guideline-admin-api-and-ui.md), [`doc-elektron/guideline-payout-modes.md`](doc-elektron/guideline-payout-modes.md)

---

## 1. Status and what you get

`photond` is a headless pool daemon: it follows your Elektron Net node, builds attested block candidates, serves Stratum V1 to miners and exposes a read-only admin API with a terminal-style web UI.

**Status:** the pool offers solo (direct coinbase payout, no fee), PPLNS and PPS side by side, keeps a SQLite ledger, tracks block maturity and pays out through a node wallet, and serves a public pool page. All of this is tested on regtest against an unmodified node (see the `test/` directory). **Payouts start in dry-run mode** (`payout.dry_run = true`), so nothing is sent until you switch it off. Not tested yet: real ASIC hardware and firmware (the first test, a Bitaxe on mainnet, is described in [`doc-elektron/guideline-release-and-bitaxe-test.md`](doc-elektron/guideline-release-and-bitaxe-test.md)) and mainnet-scale load. The node is built unmodified by default; the optional Photon node patch (`NODE_PATCHES=true`) is only needed for pools with many payout addresses. Details: [`doc-elektron/guideline-payout-modes.md`](doc-elektron/guideline-payout-modes.md).

## 2. Requirements

- Linux x86_64, 2 CPU cores, 2 GB RAM for the pool plus the resources of an Elektron Net node.
- A synchronized Elektron Net mainnet node (the Docker setup below builds and runs one for you).
- At least one pool payout address (`be1q...`) from a wallet whose keys you control. The pool only needs the addresses, never the keys. Use the desktop wallet or `elektron-net-electrum`.
- Open TCP ports: `3333` (miners), `8333` (node P2P). Never expose the node RPC (`8332`). Keep the admin API (`8081`) on localhost.

## 3. Option A: Docker (recommended)

```bash
git clone https://github.com/kutlusoy/elektron-net-photonpool.git
cd elektron-net-photonpool
cp docker/photond.docker.toml photond.toml
nano photond.toml            # set payout_addresses to your own be1q... addresses
docker compose up -d --build
```

The first build compiles the node from source and takes a while. The node then has to synchronize; `photond` waits for it and starts automatically.

Check progress:

```bash
docker compose logs -f photond
docker compose exec node gosu elektron elektron-cli -conf=/data/elektron.conf -datadir=/data getblockchaininfo
```

`photond` reports `waiting for the node` until `initialblockdownload` is `false`.

What the compose file does:

| Service | Role | Ports |
|---|---|---|
| `node` | Elektron Net node, config in `docker/elektron.conf`, data in the volume `node-data` | `8333` public. RPC `8332` stays inside the Docker network. |
| `photond` | Pool daemon, config from `./photond.toml` | `3333` public, `8081` on `127.0.0.1` only |

The pool authenticates to the node with the RPC cookie in the shared volume, so no password has to be configured. To pin a node version, set `ELEKTRON_REF` (tag or branch) in `docker-compose.yml`.

Update:

```bash
git pull
docker compose up -d --build
```

Stop: `docker compose down` (keeps the blockchain volume). Remove everything including the chain: `docker compose down -v`.

## 4. Option B: bare metal

### 4.1 Node

Install and sync an Elektron Net node as described in the [`elektron-net`](https://github.com/kutlusoy/elektron-net) repository. A minimal `~/.elektron/elektron.conf` for the pool:

```ini
server=1
rpcbind=127.0.0.1
rpcallowip=127.0.0.1
rpcuser=photon
rpcpassword=CHANGE_ME_TO_A_LONG_RANDOM_PASSWORD
```

**Important:** on its first start the node writes a default `elektron.conf` with `rpcuser=elek` and `rpcpassword=pass` if none exists. Replace these values before starting the pool, and never expose the RPC port. As long as `rpcpassword` is set, the RPC cookie is disabled; use `auth = "user:password"` in that case. Without `rpcuser`/`rpcpassword` the node creates `~/.elektron/.cookie` and you can use `auth = "cookie:/home/<user>/.elektron/.cookie"`.

Pruning is mandatory and automatic on Elektron Net; there is nothing to configure.

### 4.2 Pool

```bash
curl https://sh.rustup.rs -sSf | sh        # Rust toolchain, once
git clone https://github.com/kutlusoy/elektron-net-photonpool.git
cd elektron-net-photonpool
cargo build --release
cp photond.example.toml photond.toml
nano photond.toml                          # node auth, network, payout_addresses
target/release/photond -c photond.toml check
target/release/photond -c photond.toml run
```

`check` builds all candidates once, validates every one against the node (`getblocktemplate` proposal) and prints a JSON report. All configured addresses must show up as valid candidates.

### 4.3 systemd

`systemd/` contains hardened units for the pool and the gateway, an example node unit and an installer:

```bash
sudo systemd/install.sh pool                 # or: gateway, both
sudoedit /etc/photond/photond.toml
sudo systemctl enable --now photond
```

Files go to `/opt/photond`, `/etc/photond` and `/var/lib/photond`; the pool stops with `SIGINT` so that pending share totals reach the ledger. Details: [`doc-elektron/guideline-systemd.md`](doc-elektron/guideline-systemd.md).

## 5. Connect miners

| Setting | Value |
|---|---|
| URL | `stratum+tcp://<your-host>:3333` |
| User | `<be1q address of the miner>.<worker name>` |
| Password | `x`, or `m=solo`, `m=pplns`, `m=pps` to choose the payout mode (on ports that allow it) |

**Payout modes:** solo pays the block reward directly to the miner's address in the coinbase (no fee). PPLNS and PPS are paid from the pool wallet. A port has a default mode; with `allow_override` a miner can pick another with `m=...`. PPS is paused until the pool wallet holds the configured reserve. Enable version rolling (BIP 310) in the miner if it has the option. The pool uses a fixed coinbase, so it advertises an empty `extranonce1` and `extranonce2_size = 0`; the search space comes from the negotiated version bits, nonce and nTime. Reading the ESP-Miner (Bitaxe) source shows that it accepts this; firmware that cannot work with it is a known open risk, see the Bitaxe test guideline.

## 6. Web UI, public page and API

- UI: `http://127.0.0.1:8081/` (for a remote host use an SSH tunnel: `ssh -L 8081:127.0.0.1:8081 user@host`).
- API (read-only): `/api/v1/summary`, `/miners`, `/blocks`, `/events`.
- **Public page for miners:** set `[public] enabled = true` to serve a public terminal page (pool statistics, connection details with ports and modes, blocks, payouts, address lookup) and a personal **miner page** (`/miner.html#<address>`: balances, pending PPLNS credits with the blocks until maturity, payouts, workers, 24 h hashrate, blocks found) on its own listener, default `127.0.0.1:8082`. Put a reverse proxy with TLS in front of it and set `trust_proxy = true`. The operator UI and API are never served there. See [`doc-elektron/guideline-public-page.md`](doc-elektron/guideline-public-page.md).
- The UI shows a `DEMO DATA` tag and simulated numbers when it cannot reach the API.

## 7. Configuration reference

All options with comments are in [`photond.example.toml`](photond.example.toml) (bare metal) and [`docker/photond.docker.toml`](docker/photond.docker.toml) (Docker). The most important ones:

| Option | Meaning |
|---|---|
| `node.rpc_url`, `node.auth` | Node JSON-RPC and credentials (`user:password` or `cookie:/path`) |
| `pool.network` | `mainnet`, `testnet` or `regtest`, must match the node |
| `payout.wallet`, `payout.wallet_addresses` | Node wallet for payouts; the pool provisions its own payout addresses from it (then `pool.payout_addresses` can stay empty) |
| `payout.dry_run`, `payout.min_payout_leptons`, `payout.fee_pct.*`, `payout.pplns.window_factor` | Payout switch (default dry run) and the mode parameters, all with standard defaults |
| `pool.payout_addresses` | Pool-owned addresses; more addresses give miners more independent search space (see the sizing table in [`doc-elektron/guideline-photon-architecture.md`](doc-elektron/guideline-photon-architecture.md)) |
| `public.*` | Public pool page and its rate limit (see the next section) |
| `pool.spot_check` | Candidates per epoch validated against the node |
| `pool.backend` | `auto` (default): batch attestation when the node has the Photon patch, else one `getblocktemplate` call per address; `batch`, `gbt` force one |
| `stratum.listen`, `stratum.initial_difficulty`, `stratum.share_secs` | Miner port, starting difficulty, target seconds per share |
| `stratum.max_connections` | Upper bound for sockets |
| `api.listen`, `api.ui_dir` | Admin API and UI location |
| `api.explorer_url` | Optional base URL of a mempool-style block explorer (for example `https://mempool.elektron-net.org`). Found blocks and the chain tip then link to `<explorer_url>/block/<hash>` in the UI and in `/api/v1/blocks`. Must start with `http://` or `https://`. |

## 8. Troubleshooting

| Symptom | Cause and fix |
|---|---|
| `waiting for the node: node is still syncing` | The node is still in initial block download. Wait. |
| `node refused the credentials` (HTTP 401) | Wrong `node.auth`. Check `rpcuser`/`rpcpassword` or the cookie path. |
| `address ... has prefix X, expected be` | An address of another network in `payout_addresses` or as miner account. |
| `Unauthorized worker` in the miner | The user name is not `<be1q address>.<worker>` for the configured network. |
| `candidate N rejected by node` in the events | The node refused a candidate; the pool drops it. If every candidate is rejected, check the node version and report `bad-utxo-attestation` or other reasons from the log. |
| Miners connect but get no job | No epoch yet: the node is syncing or unreachable. Check `docker compose logs photond`. |
| Too many open files | Raise `LimitNOFILE` (systemd) or `ulimit -n`. |

## 9. Checklist

- [ ] Node synchronized, RPC not exposed.
- [ ] Own payout addresses set, keys stored safely offline.
- [ ] `photond check` shows all candidates valid.
- [ ] Port `3333` reachable from the miners, `8081` only local.
- [ ] Payout handling decided (see the payout modes guideline) before real miners connect.

## 10. Standalone or complete Docker stack

The pool is fully usable on its own (sections 3 and 4). For a public page with automatic HTTPS in the standalone Docker setup, point a DNS record at the server, open ports 80 and 443 and start the optional Caddy profile:

```bash
POOL_DOMAIN=pool.example.org docker compose --profile tls up -d
```

The public page is then served at `https://pool.example.org`; the admin API and the operator UI stay on `127.0.0.1:8081`.

### 10.1 Complete Docker stack (node, pool, HTTPS)

`stack/` in this repository sets up everything with one script: the Elektron Net node (built from source, with an encrypted pool wallet), the pool daemon and Caddy with automatic HTTPS, all pre-configured:

```bash
cd stack
cp stack.conf.example stack.conf      # set at least DOMAIN, optionally POOL_NAME, EXPLORER_URL, fees ...
./install.sh
```

The script generates the node and pool configuration, creates and encrypts the wallet (with a generated passphrase and a private-descriptor backup), starts the stack and writes `stack/CREDENTIALS.txt`. Payouts start in dry-run mode. Re-run `./install.sh` after changing `stack.conf`, or `./install.sh --update` to rebuild with fresh base images. Details: [`doc-elektron/guideline-docker-stack.md`](doc-elektron/guideline-docker-stack.md).

## 11. Photon gateway for farms with their own node (optional)

Instead of mining on the pool's Stratum port, a farm can run its own node (built with the Photon node patch, `NODE_PATCHES=true`) and a gateway next to it. The block reward is then split in the coinbase between all Photon miners, nobody holds anyone's funds, and the pool commits its accounting on chain. The pool enables the listener with `[photon] enabled = true` (see `photond.example.toml`); the farm copies `gateway.example.toml`, sets the pool's address and public key and its own payout address, and points the ASICs at the gateway's Stratum port:

```bash
photond gateway --gateway-config gateway.toml
```

The gateway reconnects to the pool by itself after an outage (see `gateway.example.toml`); run it under systemd (`systemd/photon-gateway.service`) so that it also comes back after a crash or reboot. Status: tested end to end on regtest with a simulated ASIC, not yet with real hardware. Details: [`doc-elektron/guideline-photon-protocol.md`](doc-elektron/guideline-photon-protocol.md).
