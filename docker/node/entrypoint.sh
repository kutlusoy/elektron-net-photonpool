#!/bin/sh
set -e
cp /config/elektron.conf /data/elektron.conf
chown -R elektron:elektron /data
exec gosu elektron "$@"
