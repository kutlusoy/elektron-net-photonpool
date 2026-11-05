# Elektron Net: `elektron-net-photonpool` Docker Stack Guideline

- **Version:** 0.1 (draft)
- **Date:** November 3, 2026
- **Audience:** Pool operators
- **Reference implementation:** [`elektron-net`](https://github.com/kutlusoy/elektron-net) - `src/common/args.cpp` (config file handling), `src/wallet/` (encrypted wallet, descriptor export) - treat as ground truth for anything the doc references
- **See also:** [`../Quick-Install.md`](../Quick-Install.md), [`guideline-payout-modes.md`](./guideline-payout-modes.md), [`guideline-public-page.md`](./guideline-public-page.md)

---

## 1. What the stack is

`stack/` is a self-contained Docker setup of this repository: the Elektron Net node, the pool daemon `photond` and Caddy for HTTPS. It does not depend on any other repository of the project except the node source that is built into the node image. `./install.sh` generates all configuration, sets up the wallet and starts everything.

The repository root additionally has a smaller `docker-compose.yml` for a manual setup without the installer.

## 2. Layout

| File | Purpose |
|---|---|
| `stack/stack.conf.example` | all settings with defaults; copy to `stack.conf` |
| `stack/install.sh` | reads `stack.conf` (plain `KEY=VALUE`, nothing is executed), validates it, generates the files below, builds, sets up the wallet, starts the stack |
| `stack/docker-compose.yml` | services `node`, `photond`, `caddy` (profile `tls`, active when `DOMAIN` is set) |
| generated, not committed | `.env`, `photond.env` (wallet passphrase, chmod 600), `node/elektron.conf`, `photond.toml`, `Caddyfile`, `docker-compose.ports.yml`, `CREDENTIALS.txt`, `data/` |

## 3. What `install.sh` does

1. Reads and validates `stack.conf`. Every value that ends up in a generated file is checked (numbers, booleans, host name, URLs, wallet name).
2. Generates a wallet passphrase, or reuses the one of a previous run. A rerun never rotates secrets.
3. Writes the node configuration (wallet enabled, RPC reachable only inside the Docker network, no RPC password) and `photond.toml` (RPC through the node's cookie file, wallet addresses provisioned from the pool wallet, public page behind Caddy).
4. Builds the images (the node is compiled from `ELEKTRON_REPO` at `ELEKTRON_REF`) and starts the node.
5. Waits for the node's RPC, creates the wallet if missing, encrypts it with the passphrase and exports the private descriptors once to `data/backup/` (chmod 600).
6. Starts `photond` (and Caddy when `DOMAIN` is set). `photond` waits for the node to finish syncing and then provisions its payout addresses.
7. Optionally opens the ports with ufw (`FIREWALL=true`) and writes `CREDENTIALS.txt`.

`./install.sh --update` rebuilds with fresh base images (`docker compose build --pull`) and restarts. `./install.sh --config FILE` uses another configuration file.

## 4. Settings

See `stack.conf.example`. The pool settings correspond to `photond.toml` (modes, fees, PPLNS window, PPS reserve, minimum payout in leptons, payout interval, fee rate, extra Stratum ports), `DOMAIN` and `CADDY_EMAIL` control HTTPS, `EXPLORER_URL` the links to blocks and transactions, `ELEKTRON_REPO` and `ELEKTRON_REF` the node that is built (`NODE_PATCHES=true` applies the Photon node patch with the batch attestation RPC, `false` builds the unmodified node), `WALLET_NAME`, `WALLET_ADDRESSES` (default 512; the pool uses only as many as the work demand needs, see `guideline-stratum-v1.md`) and `WALLET_PASSPHRASE` the pool wallet. `DRY_RUN=true` (default) only logs payouts.

## 5. Security notes

- The node RPC port is never published. `photond` authenticates with the RPC cookie in `data/node`, which is mounted read-only; after a node restart `photond` reads the new cookie by itself.
- The admin API (8081) and the pool page port (8082) are published on `127.0.0.1` only; the public entry is Caddy. With a domain set, `public.trust_proxy` is enabled so that every visitor gets their own rate limit.
- The wallet passphrase exists only in `photond.env` and `CREDENTIALS.txt`. It is passed to the node through the command line during `install.sh` (`encryptwallet`, `walletpassphrase`), where it is briefly visible in the process list of the host and the container. Run the installer on a host you control.
- Back up `data/backup/`, `photond.env` and `data/photon/` (the SQLite ledger) offline. The passphrase alone does not recover funds, the descriptor backup alone is useless without the passphrase of a restored wallet file.
- Payouts stay in dry-run mode until you set `DRY_RUN=false` and run `./install.sh` again.

## 6. Tested and not tested

Tested in this repository:

- The generated files for the cases with and without a domain and with extra Stratum ports: `install.sh` was run against a stubbed `docker`, the result validated with `docker compose config` and parsed by `photond`. Reruns keep the passphrase.
- The wallet flow against a real wallet-enabled node (`test/step13_encrypted_wallet.py`): a node started with `-wallet=<name>` for a wallet that does not exist yet comes up, `encryptwallet` keeps it running, the private-descriptor export works, addresses are provisioned from the encrypted wallet, a payout is sent with the passphrase from the environment, and the wallet is locked again afterwards.

Not tested: the Docker image builds and a complete run of `install.sh` on a real server (no Docker daemon was available while writing it), and Caddy certificate issuance.

## 7. Checklist

- [x] Generated configuration validated.
- [x] Encrypted wallet flow tested against a real node.
- [ ] First complete run on a test server (image builds, sync, Caddy certificate).
- [ ] Restore test of the wallet from the backup files.

## 8. Open Questions

1. Whether the node image should pin a release tag instead of `main` by default.
2. Whether the wallet passphrase should be passed to the node through a file instead of the command line.
