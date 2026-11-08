# Elektron Net: `elektron-net-photonpool` Quick Install

- **Version:** 0.1 (draft)
- **Date:** November 3, 2026
- **Audience:** Pool operators who want a running pool quickly (Docker or bare metal)
- **Reference implementation:** [`elektron-net`](https://github.com/kutlusoy/elektron-net) - node, RPC and `elektron.conf` options - treat as ground truth for anything the doc references
- **See also:** [`doc-elektron/guideline-daemon-core.md`](doc-elektron/guideline-daemon-core.md), [`doc-elektron/guideline-stratum-v1.md`](doc-elektron/guideline-stratum-v1.md), [`doc-elektron/guideline-admin-api-and-ui.md`](doc-elektron/guideline-admin-api-and-ui.md), [`doc-elektron/guideline-payout-modes.md`](doc-elektron/guideline-payout-modes.md)

---

## 1. Status and what you get

`photond` is a headless pool daemon: it follows your Elektron Net node, builds attested block candidates, serves Stratum V1 to miners and exposes a read-only admin API with a terminal-style web UI.

**Status:** the pool offers solo (direct coinbase payout, no fee), PPLNS and PPS side by side, keeps a SQLite ledger, tracks block maturity and pays out through a node wallet, and serves a public pool page. All of this is tested on regtest against an unmodified node (see the `test/` directory). **Payouts start in dry-run mode** (`payout.dry_run = true`), so nothing is sent until you switch it off. Not tested yet: real ASIC hardware and firmware (the first test, a Bitaxe on mainnet, is described in [`doc-elektron/guideline-release-and-bitaxe-test.md`](doc-elektron/guideline-release-and-bitaxe-test.md)) and mainnet-scale load. The Docker setups build the node from the branch `photonpool` of `elektron-net`, which adds the `getattestationbatch` RPC (one call for all payout addresses, no consensus change); with a node that lacks it the pool asks once per payout address. Details: [`doc-elektron/guideline-payout-modes.md`](doc-elektron/guideline-payout-modes.md).

## 1a. Which way to install?

Three ways lead to a running pool. Pick by what you already have:

| You want | Use | Section |
|---|---|---|
| Everything from one script: node, pool, HTTPS with your domain, and a **new encrypted pool wallet** (generated passphrase, backup of the private descriptors, many payout addresses already provisioned) | **Complete stack** `stack/install.sh` | [10.1](#101-complete-docker-stack-node-pool-https) |
| Node and pool in Docker, but with **your own payout addresses or your own wallet** (desktop wallet, `elektron-net-electrum`), or you already run a reverse proxy | **Docker compose** | [3](#3-option-a-docker-recommended) |
| No Docker, everything installed and managed by yourself (systemd) | **Bare metal** | [4](#4-option-b-bare-metal) |

Short version: **new pool with PPLNS or PPS and a public domain: take the complete stack.** It is the least work and the only way that sets up the encrypted wallet for you. **Own keys or own infrastructure: Docker compose or bare metal.**

What changes between the ways:

| | Complete stack | Docker compose | Bare metal |
|---|---|---|---|
| Node built from source | yes | yes | you |
| Pool wallet created and encrypted | yes (passphrase in `stack/CREDENTIALS.txt`) | you (see `payout.wallet`) | you |
| Payout addresses | provisioned from the wallet (default 512) | your list in `photond.toml`, or provisioned if you set `payout.wallet` | same |
| HTTPS and domain | Caddy, automatic | optional (`--profile tls`) | your reverse proxy |
| Payouts at the start | dry run | dry run | dry run |

In every case the payouts start in **dry run** (`payout.dry_run = true`): nothing is sent until you switch it off. Solo miners are paid directly in the coinbase and need no wallet; PPLNS and PPS are paid from the node wallet, see [`doc-elektron/guideline-payout-modes.md`](doc-elektron/guideline-payout-modes.md). The pool only needs addresses, the keys stay in the node wallet.

## 2. Requirements

- Linux x86_64, 2 CPU cores, 2 GB RAM for the pool plus the resources of an Elektron Net node.
- A synchronized Elektron Net mainnet node (the Docker setup below builds and runs one for you).
- At least one pool payout address (`be1q...`) from a wallet whose keys you control. The pool only needs the addresses, never the keys. Use the desktop wallet or `elektron-net-electrum`.
- Open TCP ports: `3333` (miners), `8333` (node P2P). Never expose the node RPC (`8332`). Keep the admin API (`8081`) on localhost.

## 3. Option A: Docker (recommended)

### 3.0 Prepare the server (once, for Docker and the complete stack)

On a fresh Debian or Ubuntu server, as root (or with `sudo`):

```bash
apt-get update && apt-get install -y git curl openssl     # tools used by the steps below
curl -fsSL https://get.docker.com | sh                    # Docker Engine and the compose plugin
docker --version && docker compose version                # both must print a version
mkdir -p /opt && cd /opt                                  # the install directory used in this guide
```

The firewall must allow the miner port `3333`, the node port `8333` and, for the complete stack, `80` and `443`. The complete stack script can set this up itself (`FIREWALL` in `stack.conf`).

This way needs your own payout addresses (or a wallet you set up yourself). For a complete setup including the encrypted wallet use the [complete stack](#101-complete-docker-stack-node-pool-https) instead, see [section 1a](#1a-which-way-to-install).

```bash
cd /opt
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

The pool authenticates to the node with the RPC cookie in the shared volume, so no password has to be configured. The node is built from the branch `photonpool` of `elektron-net` (it contains the `getattestationbatch` RPC); to pin a node version or to switch to `main` later, set `ELEKTRON_REF` (tag or branch) in `docker-compose.yml`.

Update:

```bash
cd /opt/elektron-net-photonpool
git pull
docker compose up -d --build
```

Stop: `docker compose down` (keeps the blockchain volume). Remove everything including the chain: `docker compose down -v`.

## 4. Option B: bare metal

### 4.1 Node

Install and sync an Elektron Net node as described in the [`elektron-net`](https://github.com/kutlusoy/elektron-net) repository. Until the `getattestationbatch` RPC is on its `main` branch, build the node from the branch `photonpool` (`git clone --branch photonpool https://github.com/kutlusoy/elektron-net.git`); with it the pool asks the node once per tip for all payout addresses, with an older node once per payout address. A minimal `~/.elektron/elektron.conf` for the pool:

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

Files go to `/opt/photond`, `/etc/photond` and `/var/lib/photond`; the pool stops with `SIGINT` so that pending share totals reach the ledger. Details: [`doc-elektron/guideline-systemd.md`](doc-elektron/guideline-systemd.md). For the public pool page over HTTPS without Docker, use `systemd/Caddyfile.example` (section 4 of that guideline).

## 5. Connect miners

| Setting | Value |
|---|---|
| URL | `stratum+tcp://<your-host>:3333` |
| User | `<be1q address of the miner>.<worker name>` |
| Password | `x`, or `m=solo`, `m=pplns`, `m=pps` to choose the payout mode (on ports that allow it) |

**Payout modes:** solo pays the block reward directly to the miner's address in the coinbase (no fee). PPLNS and PPS are paid from the pool wallet. A port has a default mode; with `allow_override` a miner can pick another with `m=...`. PPS is paused until the pool wallet holds the configured reserve. Enable version rolling (BIP 310) in the miner if it has the option. The pool uses a fixed coinbase, so it advertises an empty `extranonce1` and `extranonce2_size = 0`; the search space comes from the negotiated version bits, nonce and nTime. Reading the ESP-Miner (Bitaxe) source shows that it accepts this; firmware that cannot work with it is a known open risk, see the Bitaxe test guideline.

## 6. Web UI, public page and API

- UI: `http://127.0.0.1:8081/` (the same pages as the public port plus the OPS view with worker names, ledger and console) (for a remote host use an SSH tunnel: `ssh -L 8081:127.0.0.1:8081 user@host`).
- API (read-only): `/api/v1/summary`, `/miners`, `/blocks`, `/events`.
- **Public page for miners:** set `[public] enabled = true` to serve a public terminal page (pool statistics, connection details with ports and modes, blocks, payouts, address lookup) and a personal **miner page** (`/miner/<address>`, also `/miner.html#<address>`: balances, pending PPLNS credits with the blocks until maturity, payouts, workers, 24 h hashrate, blocks found) on its own listener, default `127.0.0.1:8082`. Put a reverse proxy with TLS in front of it and set `trust_proxy = true`. The admin API and the OPS view are never served there; the ENGINE view (chain tip, allocator, share quality, sanitized events) is. See [`doc-elektron/guideline-public-page.md`](doc-elektron/guideline-public-page.md).
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
| `ledger.stats_keep_days` | Days of pool statistics (one sample per minute, about 6 MB for 60 days) that the public page draws its history from; older samples are deleted (default 60) |
| `ledger.device_history_minutes` | The history by device type (Bitaxe, NerdMiner, ...) is stored once per this many minutes (default 5, about 70 bytes per device type and sample) |
| `pool.spot_check` | Candidates per epoch validated against the node |
| `pool.autoscale_candidates`, `pool.min_candidates` | The pool uses only as many of its payout addresses (candidates) as the work demand needs and grows or shrinks by itself (default on, at least 16). Every candidate offers 960 work units per epoch; a Bitaxe uses about 100 per epoch. With many miners provision enough addresses (`payout.wallet_addresses`, the Docker stack default is 512) and use a node with the `getattestationbatch` RPC (branch `photonpool`, the Docker default) |
| `pool.backend` | `auto` (default): batch attestation when the node has the `getattestationbatch` RPC, else one `getblocktemplate` call per address; `batch`, `gbt` force one |
| `stratum.listen`, `stratum.initial_difficulty`, `stratum.share_secs` | Miner port, starting difficulty, target seconds per share |
| `stratum.max_connections` | Upper bound for sockets |
| `stratum.adaptive_jobs`, `stratum.adaptive_min_interval_ms` | The job interval of every connection is learned from its duplicate and stale shares (default on, shortest 50 ms). The pool also remembers what it learned per device type (hash rate, job interval, clean jobs) and starts the next connection of that type with it; `stratum.min_job_interval_ms` (default 200, do not lower it without measuring on the device) and `stratum.max_jobs_per_sec` (default 50000) set the shortest job interval, which grows with the number of connections |
| `stratum.clean_work_agents`, `stratum.version_space_bits` | Miners whose name contains one of these words (default `bitaxe`, `esp-miner`) get every fresh job marked clean; how many version bits a miner really sweeps per job (default 5), which sets how often fresh work is sent |
| `api.listen`, `api.ui_dir` | Admin API and UI location |
| `api.explorer_url` | Optional base URL of a mempool-style block explorer (for example `https://mempool.elektron-net.org`). Found blocks and the chain tip then link to `<explorer_url>/block/<hash>` in the UI and in `/api/v1/blocks`. Must start with `http://` or `https://`. |

### 7a. Optional: restrict the pool's RPC user

The pool talks to the node with one RPC user. Everything the pool calls is listed here, so a dedicated user can be limited to it with `rpcauth` and `rpcwhitelist` in `elektron.conf` (generate the line with the `rpcauth.py` script of the node sources; the pool then uses that user and password in `[node] auth`). The methods are: `getblockchaininfo`, `getblockcount`, `getbestblockhash`, `getblockheader`, `getblocktemplate`, `getattestationbatch`, `submitblock` and, for payouts, `createwallet`, `loadwallet`, `listwallets`, `getnewaddress`, `getaddressesbylabel`, `getbalance`, `listtransactions`, `sendmany`, `walletpassphrase`, `walletlock`. This is a suggestion that has not been tested with the pool; the RPC port is not reachable from outside in the Docker setups in any case.

## 8. Troubleshooting

| Symptom | Cause and fix |
|---|---|
| `waiting for the node: node is still syncing` | The node is still in initial block download. Wait. |
| `waiting for the node: reading cookie /node/.cookie: No such file or directory` | The node has not started (or not yet written its cookie). With Docker: `docker compose ps` and `docker compose logs node`; the pool keeps retrying every 5 s and starts by itself once the cookie exists. |
| `waiting for the node: reading cookie /home/<user>/.elektron/.cookie ...` while the node runs in Docker | `photond.toml` is the bare-metal example. The compose file now overrides `rpc_url` and `auth` for the container (env `PHOTOND_NODE_RPC_URL`, `PHOTOND_NODE_AUTH`), so `git pull` and `docker compose up -d --build` is enough; or start from `docker/photond.docker.toml` (`rpc_url = "http://node:8332"`, `auth = "cookie:/node/.cookie"`): `cp docker/photond.docker.toml photond.toml`, set the payout addresses, `docker compose up -d --force-recreate photond`. |
| `exec /usr/local/bin/entrypoint.sh: no such file or directory` (node container) | The script has Windows line endings (`\r`) after a checkout on Windows. The repository now contains a `.gitattributes` and the image strips them; rebuild with `docker compose build --no-cache node`. If it still happens: `git config core.autocrlf false`, delete the clone, clone again. |
| `node refused the credentials` (HTTP 401) | Wrong `node.auth`. Check `rpcuser`/`rpcpassword` or the cookie path. |
| `address ... has prefix X, expected be` | An address of another network in `payout_addresses` or as miner account. |
| `Unauthorized worker` in the miner | The user name is not `<be1q address>.<worker>` for the configured network. |
| `candidate N rejected by node` in the events | The node refused a candidate; the pool drops it. If every candidate is rejected, check the node version and report `bad-utxo-attestation` or other reasons from the log. |
| Miners connect but get no job | No epoch yet: the node is syncing or unreachable. Check `docker compose logs photond`. |
| Too many open files | Raise `LimitNOFILE` (systemd) or `ulimit -n`. |

### 8a. Reading the pool log

```bash
docker compose logs -f photond                      # live, Ctrl+C to leave
docker compose logs --since 30m photond | grep -i "error\|warn\|refused\|ended without"   # problems of the last half hour
RUST_LOG=photond=debug                               # more detail: set it under environment: of photond in docker-compose.yml, then docker compose up -d
```

Lines worth knowing: `ERROR` (a payout or a block failed, the text says why), `WARN` (something was skipped, for example the registry could not be read), `session N <ip> [<user agent>] ended without a share after Ns: <reason>` (a miner connected and never delivered a share: wrong firmware handshake, wrong settings or a firewall), `authorization refused, the user name must be <address>.<worker>` (the miner has no valid payout address as user name), `BLOCK FOUND`, `payout N: M accounts, tx ...` and `registry: ...`. A miner that works shows up on the pool page under Devices and on the miner page of its address within a minute.

## 9. Checklist

- [ ] Node synchronized, RPC not exposed.
- [ ] Own payout addresses set, keys stored safely offline.
- [ ] `photond check` shows all candidates valid.
- [ ] Port `3333` reachable from the miners, `8081` only local.
- [ ] Payout handling decided (see the payout modes guideline) before real miners connect.

## 9a. Background music (optional)

The web pages can play background music. Put audio files (mp3, ogg, opus, m4a, wav, flac) into the `music/` folder next to `docker-compose.yml` (for bare metal: set `music_dir` in `[api]`). The player in the title bar plays them as a looping playlist with play, pause, stop, previous, next and volume; the browser streams the files, so nothing is held in memory. Copy `music/playlist.example.json` to `music/playlist.json` to set titles and the credit that a license asks for; it is shown in a tooltip on the track title. Audio files and `playlist.json` are ignored by git on purpose, because most free licenses allow using a track on your site but not passing the file on: copy them to the server yourself. See [`music/README.md`](music/README.md).

## 10. Standalone or complete Docker stack

The pool is fully usable on its own (sections 3 and 4). For a public page with automatic HTTPS in the standalone Docker setup, point a DNS record at the server, open ports 80 and 443 and start the optional Caddy profile:

```bash
POOL_DOMAIN=pool.example.org docker compose --profile tls up -d
```

The public page is then served at `https://pool.example.org`; the admin API and the operator UI stay on `127.0.0.1:8081`.

### 10.1 Complete Docker stack (node, pool, HTTPS)

`stack/` in this repository sets up everything with one script: the Elektron Net node (built from source, with an encrypted pool wallet), the pool daemon and Caddy with automatic HTTPS, all pre-configured:

```bash
cd /opt
git clone https://github.com/kutlusoy/elektron-net-photonpool.git     # skip if already cloned
cd elektron-net-photonpool/stack
cp stack.conf.example stack.conf      # set at least DOMAIN, optionally POOL_NAME, EXPLORER_URL, fees ...
./install.sh
```

The script generates the node and pool configuration, creates and encrypts the wallet (with a generated passphrase and a private-descriptor backup), starts the stack and writes `stack/CREDENTIALS.txt`. Payouts start in dry-run mode.

By default all three modes are active (`DEFAULT_MODE=solo`, `STRATUM_PORT=3333`, `PORT_PPLNS=3334`, `PORT_PPS=3335`, `PPS_ENABLED=true`): port 3333 pays solo, 3334 PPLNS and 3335 PPS. Without a `stack.conf` the script also uses `DOMAIN=phtn.space`, `CADDY_EMAIL=ali@kutlusoy.com` and `EXPLORER_URL=https://mempool.elektron-net.org`; change them in `stack.conf` for another pool. Only the main port lets a miner choose another mode with `m=solo|pplns|pps` in the password. Re-run `./install.sh` after changing `stack.conf`. Updating: see [10.2](#102-update-the-complete-stack). Make the DNS record of `DOMAIN` point to the server before the first run so HTTPS can be issued. Details: [`doc-elektron/guideline-docker-stack.md`](doc-elektron/guideline-docker-stack.md).

### 10.1a Show your pool as the block finder (registry)

Explorers name the pool that found a block through the [Elektron Net registry](https://github.com/kutlusoy/elektron-net-registry). The stack does the pool's part by itself (`REGISTRY_REPORTING=true` is the default): it reports each found block to the listed explorers and answers their confirmation call at `https://<DOMAIN>/api/pool/identity/confirm`. For the explorers to accept it, add a line for your pool to `pools.txt` of the registry by pull request, with the Name exactly as `POOL_NAME` and the URL exactly `https://<DOMAIN>`, for example `"PPLNS", "Photon Pool", "https://phtn.space";`. Details: [`doc-elektron/guideline-pool-registry-reporting.md`](doc-elektron/guideline-pool-registry-reporting.md).

### 10.2 Update the complete stack

An update keeps the wallet, the passphrase, the wallet backup, the pool ledger (`photond.db`) and the blockchain. Only `photond.toml`, `Caddyfile`, `.env` and `docker-compose.ports.yml` are generated again from `stack.conf`, so keep your settings in `stack.conf` and do not edit those files by hand.

1. Back up the wallet files. `./install.sh --update` also saves a copy automatically to `stack/data/update-backup-<date>/` (wallet descriptors, passphrase, settings), but that stays on the same server: take a copy off the server as well, and do it before each update:

```bash
cd /opt/elektron-net-photonpool/stack
mkdir -p /root/photon-backup-$(date +%F)
cp -a data/backup photond.env stack.conf /root/photon-backup-$(date +%F)/
```

2. Fetch the new version and rebuild:

```bash
cd /opt/elektron-net-photonpool
git pull
cd stack
./install.sh --update
```

3. Check:

```bash
docker compose ps
docker compose logs -f photond
```

The node is compiled again from the branch `photonpool` (a few minutes) and restarts briefly; miners reconnect by themselves. The log says `Wallet 'pool' is loaded` / `already encrypted` and `Reusing the wallet passphrase of the previous run`; if it says `Generated a wallet passphrase` or `Creating wallet` instead, stop and check that you are in the right directory.

Never run `docker compose down -v`: `-v` deletes the volumes including the blockchain. `git pull` does not touch `stack.conf`, `photond.env`, `data/` or the generated files, they are ignored by git.

## 11. Photon gateway for farms with their own node (optional)

Instead of mining on the pool's Stratum port, a farm can run its own node (built from the branch `photonpool` of `elektron-net`, which has the `getattestationbatch` RPC) and a gateway next to it. The block reward is then split in the coinbase between all Photon miners, nobody holds anyone's funds, and the pool commits its accounting on chain. The pool enables the listener with `[photon] enabled = true` (see `photond.example.toml`); the farm copies `gateway.example.toml`, sets the pool's address and public key and its own payout address, and points the ASICs at the gateway's Stratum port:

```bash
photond gateway --gateway-config gateway.toml
```

The gateway reconnects to the pool by itself after an outage (see `gateway.example.toml`); run it under systemd (`systemd/photon-gateway.service`) so that it also comes back after a crash or reboot. Status: tested end to end on regtest with a simulated ASIC, not yet with real hardware. Details: [`doc-elektron/guideline-photon-protocol.md`](doc-elektron/guideline-photon-protocol.md).
