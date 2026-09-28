#!/usr/bin/env bash
# Parse gate for the vendored builtin module.
#
# `src/roc/Builtin.roc` is a VERBATIM copy of the roc compiler's own
# `src/build/roc/Builtin.roc` — the 24,743 lines of Roc that define Str, List, Dict,
# Set, Num, Iter and the Json encoding. rocflight loads it and binds the
# annotation-only members to Rust, the way `canonicalize/BuiltinLowLevel.zig` binds
# them to low-level ops. That only works if rocflight can parse the file.
#
# The reading is `rocflight --builtins`, in `src/builtin.rs`, so the slicing lives in
# one place — the same code the loader will use. This script is the gate around it.
#
# Re-sync when the pinned nightly moves:
#     cp roc-compiler/src/build/roc/Builtin.roc src/roc/Builtin.roc && rm src/roc/Builtin.artifact \
#       && cargo run --release --bin gen-artifact && tests/check_builtin.sh
#
# `--builtins` is a development flag, so this needs the DEBUG binary: `cargo build`.
#
# Usage: tests/check_builtin.sh [--strict] [--names]
set -uo pipefail
cd "$(dirname "$0")/.."
ROCFLIGHT=${ROCFLIGHT:-./target/debug/rocflight}

strict=0
flag=--builtins
for arg in "$@"; do
  case "$arg" in
    --strict) strict=1 ;;
    --names) flag=--builtins=names ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

out=$("$ROCFLIGHT" "$flag") || { printf '%s\n' "$out"; exit 1; }
# Colour the verdict column without teaching Rust about terminals.
printf '%s\n' "$out" \
  | sed -e $'s/^  ok  /  \033[32mok\033[0m  /' -e $'s/^  FAIL/  \033[31mFAIL\033[0m/'

if [ "$strict" = 1 ] && printf '%s\n' "$out" | grep -q '^  FAIL'; then
  exit 1
fi
exit 0
