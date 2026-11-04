#!/bin/sh
# Installs photond, the systemd units and example configuration files.
#   sudo systemd/install.sh [pool|gateway|both]      (default: both)
# Environment: DESTDIR (for packaging or a dry run), PHOTOND_BIN (default target/release/photond), NO_SYSTEMCTL=1 (do not touch systemd).
# Nothing is enabled or started; the script prints what to do next. Existing configuration files are never overwritten.
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
what=${1:-both}
bin=${PHOTOND_BIN:-$here/target/release/photond}
d=${DESTDIR:-}

[ -x "$bin" ] || { echo "ERROR: $bin not found; run 'cargo build --release' first or set PHOTOND_BIN" >&2; exit 1; }
case "$what" in pool|gateway|both) ;; *) echo "usage: $0 [pool|gateway|both]" >&2; exit 1 ;; esac

install -d -m 0755 "$d/opt/photond" "$d/etc/photond" "$d/usr/lib/systemd/system" "$d/usr/lib/sysusers.d"
install -m 0755 "$bin" "$d/opt/photond/photond"
cp -R "$here/ui" "$d/opt/photond/"
install -m 0644 "$here/systemd/photond.sysusers.conf" "$d/usr/lib/sysusers.d/photond.conf"

if [ "$what" = pool ] || [ "$what" = both ]; then
  install -m 0644 "$here/systemd/photond.service" "$d/usr/lib/systemd/system/photond.service"
  if [ ! -e "$d/etc/photond/photond.toml" ]; then
    install -m 0640 "$here/photond.example.toml" "$d/etc/photond/photond.toml"
    sed -i 's|^ui_dir = "ui"|ui_dir = "/opt/photond/ui"|' "$d/etc/photond/photond.toml"
  fi
  if [ ! -e "$d/etc/photond/photond.env" ]; then
    ( umask 077; cat > "$d/etc/photond/photond.env" <<'ENV'
# Environment of the photond service (systemd EnvironmentFile; one NAME=value per line, no quotes needed, mode 0600).
#
# Passphrase of the node wallet, if it is encrypted. The variable name is payout.wallet_passphrase_env in photond.toml.
# PHOTON_WALLET_PASSPHRASE=
#
# Node login for `auth = "env:PHOTON_NODE_AUTH"` in photond.toml: the value is user:password.
# PHOTON_NODE_AUTH=elek:secret
#
# Log level. Options: error | warn | info (default) | debug | trace; per module such as photond::photon=debug,info.
# RUST_LOG=info
ENV
    )
  fi
  install -m 0644 "$here/systemd/Caddyfile.example" "$d/etc/photond/Caddyfile.example"
fi
if [ "$what" = gateway ] || [ "$what" = both ]; then
  install -m 0644 "$here/systemd/photon-gateway.service" "$d/usr/lib/systemd/system/photon-gateway.service"
  if [ ! -e "$d/etc/photond/gateway.env" ]; then
    ( umask 077; cat > "$d/etc/photond/gateway.env" <<'ENV'
# Environment of the photon-gateway service (systemd EnvironmentFile; one NAME=value per line, mode 0600).
#
# Node login for `node_auth = "env:GATEWAY_NODE_AUTH"` in gateway.toml: the value is user:password.
# GATEWAY_NODE_AUTH=elek:secret
#
# Log level. Options: error | warn | info (default) | debug | trace.
# RUST_LOG=info
ENV
    )
  fi
  [ -e "$d/etc/photond/gateway.toml" ] || install -m 0640 "$here/gateway.example.toml" "$d/etc/photond/gateway.toml"
fi
[ -e "$d/usr/lib/systemd/system/elektrond.service" ] || install -m 0644 "$here/systemd/elektrond.service" "$d/usr/lib/systemd/system/elektrond.service.example"

if [ -z "$d" ] && [ "${NO_SYSTEMCTL:-0}" != 1 ]; then
  command -v systemd-sysusers >/dev/null 2>&1 && systemd-sysusers /usr/lib/sysusers.d/photond.conf || true
  chgrp photon /etc/photond/*.toml 2>/dev/null || true
  command -v systemctl >/dev/null 2>&1 && systemctl daemon-reload || true
fi

cat <<MSG
Installed to $d/opt/photond and $d/etc/photond.
Next:
  1. edit /etc/photond/photond.toml (pool) and/or /etc/photond/gateway.toml (gateway):
     node auth, network, payout addresses (relative paths such as ledger.db and photon.key land in /var/lib/photond)
  2. check it:    sudo -u photon /opt/photond/photond -c /etc/photond/photond.toml check
  3. start it:    systemctl enable --now photond            (pool)
                  systemctl enable --now photon-gateway     (gateway)
  4. watch it:    journalctl -u photond -f
  5. public page: see /etc/photond/Caddyfile.example (Caddy with automatic HTTPS in front of the pool page, pool only)
MSG
