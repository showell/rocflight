#!/bin/bash
# ROC TO RUST: roc-apps' ported tests (Codex -> Roc by rocemit), written as Rust
# by roc2rust, compiled by rustc, run, and compared with the output Cobblestone
# captured for the original Codex.
#
#   codex/rust.sh            every test; the tally, and refusals by reason
#   codex/rust.sh NAME...    those (codex_arithmetic, ...)
#
# One line per test in $OUT/ledger.txt: PASS, FAIL (wrong output, or it
# panicked), NO-COMPILE (rustc rejected it; its first error), REFUSED
# (roc2rust said no, and why), TIMEOUT. The Rust is kept at $OUT/<name>.rs.
# A debug build: its overflow checks panic where Roc's arithmetic crashes.
set -u
BIN="${BIN:-$HOME/build/rust-target/release}"
PORTED="${PORTED:-$HOME/showell_repos/roc-apps/tests/ported}"
OUT="${OUT:-$HOME/build/rocflight/rust}"
mkdir -p "$OUT"
export PATH=$HOME/.cargo/bin:$PATH
if [ $# -gt 0 ]; then names=("$@"); else mapfile -t names < <(cd "$PORTED" && ls codex_*.roc | sed 's/\.roc$//'); fi
: > "$OUT/ledger.txt"
trim() { awk 'BEGIN{n=0} /^$/{n++; next} {while (n-- > 0) print ""; n=0; print}'; }
one() {
    n="$1"
    if ! (cd "$PORTED" && timeout 60 "$BIN/roc2rust" "$n.roc" "$OUT/$n.rs") 2> "$OUT/$n.refused"; then
        echo "REFUSED $n | $(sed 's/^REFUSED: //' "$OUT/$n.refused" | head -1 | cut -c1-140)"; return
    fi
    if ! rustc --edition 2021 -A warnings -o "$OUT/$n.bin" "$OUT/$n.rs" 2> "$OUT/$n.rustc"; then
        echo "NO-COMPILE $n | $(grep -m1 '^error' "$OUT/$n.rustc" | cut -c1-140)"; return
    fi
    timeout 60 "$OUT/$n.bin" 2> "$OUT/$n.err" | trim > "$OUT/$n.out"; rc=${PIPESTATUS[0]}
    rm -f "$OUT/$n.bin"
    if [ $rc = 124 ]; then echo "TIMEOUT $n"
    elif cmp -s "$OUT/$n.out" "$PORTED/expected/$n.txt"; then echo "PASS $n"
    else echo "FAIL $n | rc=$rc $(grep -m1 'panicked' "$OUT/$n.err" | cut -c1-100) $(diff "$OUT/$n.out" "$PORTED/expected/$n.txt" | grep -m1 '^[<>]' | cut -c1-80)"; fi
}
export -f one trim; export BIN PORTED OUT
printf '%s\n' "${names[@]}" | xargs -P "${JOBS:-2}" -I{} bash -c 'one {}' | sort -k2 > "$OUT/ledger.txt"
cut -d' ' -f1 "$OUT/ledger.txt" | sort | uniq -c | sort -rn
echo "--- refusals and compile errors by reason:"
grep -E '^(REFUSED|NO-COMPILE)' "$OUT/ledger.txt" | cut -d'|' -f2 | sed 's/`[^`]*`/`_`/g; s/"[^"]*"/"_"/g; s/[0-9]\+/N/g' | cut -c1-80 | sort | uniq -c | sort -rn | head -30
