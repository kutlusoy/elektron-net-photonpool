#!/bin/sh
# usage: ELEKTROND=/path/to/elektrond [ELEKTROND_WALLET=/path/to/wallet-enabled/elektrond] PHOTON_DATADIR=/tmp/photon ./run_all.sh
# step14 and step15 (differential test, SEED/ROUNDS/MAXCAND tunable) need a node built with docker/node/patches/*.patch (ELEKTROND_BATCH); step11 and step13 need a node built with the wallet; it is skipped when ELEKTROND_WALLET is not set.
cd "$(dirname "$0")" || exit 1
rc=0
for s in step0*.py step10*.py step12*.py step24*.py step25*.py; do python3 "$s" || rc=1; done
if [ -n "$ELEKTROND_WALLET" ]; then
  ELEKTROND="$ELEKTROND_WALLET" python3 step11_ledger_and_payouts.py || rc=1
  ELEKTROND="$ELEKTROND_WALLET" python3 step13_encrypted_wallet.py || rc=1
else
  echo "[step11, step13] skipped (set ELEKTROND_WALLET to a wallet-enabled elektrond)"
fi
if [ -n "$ELEKTROND_BATCH" ]; then
  ELEKTROND="$ELEKTROND_BATCH" python3 step14_batch_attestation.py || rc=1
  for seed in 1 2 3; do SEED=$seed ELEKTROND="$ELEKTROND_BATCH" python3 step15_differential.py || rc=1; done
  ELEKTROND="$ELEKTROND_BATCH" python3 step16_photon_plan_coinbase.py || rc=1   # needs target/release/photond or PHOTOND
  ELEKTROND="$ELEKTROND_BATCH" python3 step17_photon_session.py || rc=1       # pool session, gateway, spot checks
  ELEKTROND="$ELEKTROND_BATCH" python3 step18_gateway_stratum.py || rc=1     # gateway Stratum front end
  ELEKTROND="$ELEKTROND_BATCH" python3 step19_multi_channel.py || rc=1        # several channels per gateway
  ELEKTROND="$ELEKTROND_BATCH" python3 step20_gateway_reconnect.py || rc=1     # pool outage and reconnect
  ELEKTROND="$ELEKTROND_BATCH" python3 step22_coinbase_weight.py || rc=1       # block weight with large coinbases
  ELEKTROND="$ELEKTROND_BATCH" python3 step23_attestation_scaling.py || rc=1    # attestation cost against the set size
else
  echo "[step14] skipped (set ELEKTROND_BATCH to an elektrond built with the Photon node patch)"
fi
exit $rc
