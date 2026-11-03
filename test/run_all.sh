#!/bin/sh
# usage: ELEKTROND=/path/to/elektrond PHOTON_DATADIR=/tmp/photon ./run_all.sh
cd "$(dirname "$0")" || exit 1
rc=0
for s in step0*.py; do python3 "$s" || rc=1; done
exit $rc
