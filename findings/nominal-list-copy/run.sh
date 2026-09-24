#!/bin/bash
# Time main.roc at three sizes. In place, the time is flat in SIZE;
# copying, it grows with SIZE.
set -u
cd "$(dirname "$0")"
ROCFLIGHT="${ROCFLIGHT:-../../target/release/rocflight}"
mkdir -p gen
cp Text.roc gen/
for size in 1024 4096 16384; do
    sed "s/SIZE/$size/" main.roc > gen/main.roc
    printf '%6s units: ' "$size"
    /usr/bin/time -f '%U s' "$ROCFLIGHT" gen/main.roc 2>&1 | tr '\n' ' '
    echo
done
