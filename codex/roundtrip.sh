#!/bin/bash
# THE ROUND TRIP: roc-apps' ported tests (Codex -> Roc by rocemit), back to Codex
# by roc2codex, run by codexrun, against the output Cobblestone captured.
#
#   codex/roundtrip.sh            every test; the tally, and refusals by reason
#   codex/roundtrip.sh NAME...    those (codex_arithmetic, ...)
#
# One line per test in $OUT/ledger.txt: PASS (output equals expected), FAIL
# (it does not; codexrun's first line), ORACLE-FAIL (codexrun fails the
# original Codex program the same way), REFUSED (roc2codex said no, and why),
# TIMEOUT. The Codex is kept at $OUT/<name>.codex.
set -u
BIN="${BIN:-$HOME/build/rust-target/release}"
PORTED="${PORTED:-$HOME/showell_repos/roc-apps/tests/ported}"
OUT="${OUT:-$HOME/build/rocflight/roundtrip}"
COBBLESTONE="${COBBLESTONE:-$HOME/showell_repos/cobblestone-u62}"
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
    else
        # Does codexrun run the ORIGINAL? If it fails that too, the gap is the
        # oracle's, not the round trip's.
        src="$(grep -m1 '^#   from' "$PORTED/$n.roc" | sed 's|.*/blob/master/||')"
        if [ -n "$src" ] && ! timeout 60 "$BIN/codexrun" "$COBBLESTONE/$src" 2>/dev/null | cmp -s - "$PORTED/expected/$n.txt"; then
            echo "ORACLE-FAIL $n | codexrun fails the original $src too"
        else
            echo "FAIL $n | $(diff "$OUT/$n.out" "$PORTED/expected/$n.txt" | grep -m1 '^[<>]' | cut -c1-140)"
        fi
    fi
}
export -f one; export BIN PORTED OUT COBBLESTONE
printf '%s\n' "${names[@]}" | xargs -P "${JOBS:-2}" -I{} bash -c 'one {}' | sort -k2 > "$OUT/ledger.txt"
cut -d' ' -f1 "$OUT/ledger.txt" | sort | uniq -c | sort -rn
echo "--- refusals by reason:"
grep '^REFUSED' "$OUT/ledger.txt" | cut -d'|' -f2 | sed 's/`[^`]*`/`_`/g; s/"[^"]*"/"_"/g' | cut -c1-60 | sort | uniq -c | sort -rn | head -25
