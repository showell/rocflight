#!/bin/bash
# roc-apps' ported Codex tests (tests/ported), run by rocflight as they are:
# each app over the `codex` package, its output against expected/<name>.txt.
#
#   codex/ported.sh              every test; the tally, and the failures by message
#   codex/ported.sh NAME...      those (codex_arithmetic, ...)
#
# Writes one line per test to $OUT/results.txt: PASS/FAIL/TIMEOUT, name, ms,
# and a failure's first error line.
set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROCFLIGHT="${ROCFLIGHT:-$HOME/build/rust-target/release/rocflight}"
PORTED="${PORTED:-$HOME/showell_repos/roc-apps/tests/ported}"
OUT="${OUT:-$HOME/build/rocflight/ported}"
mkdir -p "$OUT"
if [ $# -gt 0 ]; then names=("$@"); else mapfile -t names < <(cd "$PORTED" && ls codex_*.roc | sed 's/\.roc$//'); fi
: > "$OUT/results.txt"
for n in "${names[@]}"; do
    t0=$(date +%s%N)
    out="$(cd "$PORTED" && timeout 30 "$ROCFLIGHT" "$n.roc" 2>&1)"; rc=$?
    ms=$(( ($(date +%s%N) - t0) / 1000000 ))
    if [ "$out" = "$(cat "$PORTED/expected/$n.txt")" ]; then echo "PASS $n $ms"
    elif [ $rc = 124 ]; then echo "TIMEOUT $n $ms"
    else echo "FAIL $n $ms | $(echo "$out" | grep -m1 -iE 'error|panic' | cut -c1-160)"; fi >> "$OUT/results.txt"
done
cut -d' ' -f1 "$OUT/results.txt" | sort | uniq -c
echo "--- failures by message:"
grep -v '^PASS' "$OUT/results.txt" | cut -d'|' -f2 | sed 's/at [A-Za-z_@]*\.roc:[0-9:]*//; s/[0-9]\+/N/g' | sort | uniq -c | sort -rn | head -20
