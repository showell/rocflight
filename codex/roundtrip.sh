#!/bin/bash
# THE ROUND TRIP: roc-apps' ported tests (Codex -> Roc by rocemit), back to Codex
# by roc2codex, run by codexrun, against the output Cobblestone captured.
#
#   codex/roundtrip.sh            every test; the tally, and refusals by reason
#   codex/roundtrip.sh NAME...    those (codex_arithmetic, ...)
#
# One line per test in $OUT/ledger.txt: PASS (output equals expected), FAIL
# (it does not; codexrun's first line), REFUSED (roc2codex said no, and why),
# TIMEOUT. The Codex is kept at $OUT/<name>.codex.
set -u
BIN="${BIN:-$HOME/build/rust-target/release}"
PORTED="${PORTED:-$HOME/showell_repos/roc-apps/tests/ported}"
OUT="${OUT:-$HOME/build/rocflight/roundtrip}"
mkdir -p "$OUT"
if [ $# -gt 0 ]; then names=("$@"); else mapfile -t names < <(cd "$PORTED" && ls codex_*.roc | sed 's/\.roc$//'); fi
: > "$OUT/ledger.txt"
one() {
    n="$1"
    if ! (cd "$PORTED" && timeout 60 "$BIN/roc2codex" "$n.roc" "$OUT/$n.codex") 2> "$OUT/$n.refused"; then
        echo "REFUSED $n | $(sed 's/^REFUSED: //' "$OUT/$n.refused" | head -1 | cut -c1-140)"; return
    fi
    timeout 60 "$BIN/codexrun" "$OUT/$n.codex" > "$OUT/$n.out" 2>&1; rc=$?
    if [ $rc = 124 ]; then echo "TIMEOUT $n"
    elif cmp -s "$OUT/$n.out" "$PORTED/expected/$n.txt"; then echo "PASS $n"
    else echo "FAIL $n | $(diff "$OUT/$n.out" "$PORTED/expected/$n.txt" | grep -m1 '^[<>]' | cut -c1-140)"; fi
}
export -f one; export BIN PORTED OUT
printf '%s\n' "${names[@]}" | xargs -P "${JOBS:-2}" -I{} bash -c 'one {}' | sort -k2 > "$OUT/ledger.txt"
cut -d' ' -f1 "$OUT/ledger.txt" | sort | uniq -c | sort -rn
echo "--- refusals by reason:"
grep '^REFUSED' "$OUT/ledger.txt" | cut -d'|' -f2 | sed 's/`[^`]*`/`_`/g; s/"[^"]*"/"_"/g' | cut -c1-60 | sort | uniq -c | sort -rn | head -25
