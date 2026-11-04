# Elektron Net: `elektron-net-photonpool` systemd Guideline

- **Version:** 0.1
- **Date:** November 4, 2026
- **Audience:** Operators of pools and gateways on Linux servers
- **Reference implementation:** `systemd/` in this repository (units, installer); [`systemd.exec(5)`](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html) for the directives - treat as ground truth for anything the doc references
- **See also:** [`Quick-Install.md`](../Quick-Install.md), [`guideline-docker-stack.md`](./guideline-docker-stack.md), [`guideline-photon-review.md`](./guideline-photon-review.md)

---

## 1. What is provided

| File | Purpose |
|---|---|
| `systemd/photond.service` | the pool (`photond -c /etc/photond/photond.toml run`) |
| `systemd/photon-gateway.service` | a farm's gateway (`photond gateway --gateway-config /etc/photond/gateway.toml`) |
| `systemd/elektrond.service` | example for the node; adapt the paths |
| `systemd/photond.sysusers.conf` | creates the account `photon` |
| `systemd/install.sh` | installs the binary, the UI, the units and example configurations |

## 2. Installation

```bash
cargo build --release
sudo systemd/install.sh pool        # or: gateway, both
sudoedit /etc/photond/photond.toml  # node auth, network, payout addresses
sudo -u photon /opt/photond/photond -c /etc/photond/photond.toml check
sudo systemctl enable --now photond
journalctl -u photond -f
```

The installer never overwrites a configuration file, does not enable or start anything, and accepts `DESTDIR=/some/dir` for packaging or a dry run. The files end up in `/opt/photond` (binary and `ui/`), `/etc/photond` (configuration, `photond.env` for secrets) and `/usr/lib/systemd/system`. State (ledger, Photon key) lives in `/var/lib/photond`: the units use `WorkingDirectory=/var/lib/photond`, so relative paths such as `ledger.db` and `photon.key` in the example configuration land there.

## 3. Behaviour worth knowing

- **Graceful stop:** `KillSignal=SIGINT` and `TimeoutStopSec=30`. The pool writes the pending share totals and the remembered gateway trust to the ledger when it receives `SIGINT`; with `SIGTERM` or `SIGKILL` the last seconds of Photon shares are lost.
- **Start order:** `After=elektrond.service` but not `Requires=`: the pool and the gateway wait for a synchronized node and retry by themselves.
- **Restart:** the pool restarts after 5 s, the gateway after 15 s. A gateway that ends because of a wrong pool key, a ban or a failed proof should not hammer the pool, which the longer delay prevents; check `journalctl -u photon-gateway` when it keeps restarting.
- **Secrets:** `PHOTON_WALLET_PASSPHRASE` and similar variables go into `/etc/photond/photond.env` (mode 0600, owned by root; systemd reads it before dropping privileges).
- **File descriptors:** `LimitNOFILE=1048576` for the pool (one per miner), 65,536 for the gateway.
- **Hardening:** no new privileges, read-only system, no capabilities, restricted address families and system calls, private `/tmp` and devices, write access only to `/var/lib/photond`. `ProtectHome=read-only` lets the pool read the node's cookie file under a home directory; set `ProtectHome=true` and use `auth = "user:password"` or `"env:VAR"` instead to close that. The units were checked with `systemd-analyze verify` (only the missing binary is reported on a machine without the installation).
- **Firewall:** the pool's Stratum (default 3333), Photon (3340) and public page ports are meant to be reachable; the admin API (default `127.0.0.1:8081`) is not. The gateway's Stratum port belongs on the farm network only, because it has no authentication.

## 4. Node unit

`systemd/elektrond.service` is an example only: the node's own documentation is authoritative. Points that matter for the pool: `TimeoutStopSec=600` (a clean shutdown protects the chainstate), `allowignoredconf=1` in the node configuration when `-conf` is used, and RPC bound to loopback.

## 5. Checklist

- [ ] `photon` account exists (`systemd-sysusers`), `/etc/photond/*.toml` readable by group `photon`.
- [ ] `check` prints all candidates valid before the service is enabled.
- [ ] Ports opened as described in section 3, admin API not exposed.
- [ ] `journalctl -u photond` shows `photon protocol listening` (if Photon is enabled) and the pool key that gateways pin.
