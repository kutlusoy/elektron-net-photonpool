#!/bin/sh
# usage: ELEKTROND=/path/to/elektrond [ELEKTROND_WALLET=/path/to/wallet-enabled/elektrond] PHOTON_DATADIR=/tmp/photon ./run_all.sh
# step11 needs a node built with the wallet; it is skipped when ELEKTROND_WALLET is not set.
cd "$(dirname "$0")" || exit 1
rc=0
for s in step0*.py step10*.py step12*.py; do python3 "$s" || rc=1; done
if [ -n "$ELEKTROND_WALLET" ]; then
  ELEKTROND="$ELEKTROND_WALLET" python3 step11_ledger_and_payouts.py || rc=1
else
  echo "[step11] skipped (set ELEKTROND_WALLET to a wallet-enabled elektrond)"
fi
exit $rc
