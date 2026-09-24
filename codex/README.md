# roc2codex

A Roc program, checked by rocflight, written out as Codex (Cobblestone's
language): one Codex chapter per Roc module, and the app's `main!` as the
chapter's `opening`. The code is `src/codex/`; the binary is
`src/bin/roc2codex.rs`.

    cargo build --release
    roc2codex app.roc out.codex      # exit 2 and a reason on a refusal
    codexrun out.codex               # rust-codex-compiler's Codex interpreter

This branch (`codex-emit`) is rocflight's `main` with our PRs to it applied, and
this work on top. The per-node types it reads come from the checker's
`record_types` / `node_types`, which are off unless asked for.

## The round trip

The subjects are roc-apps' ported tests (`tests/ported`): Codex programs from
Cobblestone's test suite, written as Roc by rocemit (rust-codex-compiler). Each
comes back to Codex here and runs under `codexrun`, and its output must equal the
output Cobblestone captured for the original.

    codex/roundtrip.sh               # all 526; the tally and refusals by reason
    codex/roundtrip.sh NAME...       # codex_arithmetic, ...
    codex/ported.sh                  # the same tests run by rocflight itself

`roundtrip.sh` outcomes: PASS; FAIL (wrong output); ORACLE-FAIL (codexrun fails
the original Codex program too, so the round trip cannot be judged); REFUSED
(roc2codex said no, and why); TIMEOUT. Its ledger and the Codex it wrote are
under `~/build/rocflight/roundtrip/`.

## What it relies on

The subject is rocemit's Roc, and roc2codex reads rocemit's conventions back:

- **`CceText :: List(U8)` is a Codex `Text`, `CceChar :: I64` a Codex `Char`.**
  Their modules, and rocemit's `Prelude`, are Codex's own builtins, not
  chapters: `CceText.concat` is `&`, `CceChar.of_code(15)` is `'a'`,
  `Prelude.approx_eq` is `~`. A CCE code is a position in Cobblestone's
  alphabet, not Unicode.
- **rocemit's idioms are read back as the builtins they were written from**
  (`U64.to_i64_wrap(List.len(xs))` is `list-length xs`; a `List.get(..) ??
  crash(..)` is `list-at`). The table is rocemit's `fn builtin` run backwards.
- **A match on a Char matches its code** (`match CceChar.code(c) { 15 => }`), and
  comes back as `when c is 'a'`.
- **Every string literal is a Codex `Text`**: Codex has one string type.
- **Wrapping arithmetic** (`I64.plus_wrap`) goes through the unit's `Roc--Wrap`
  chapter, whose helpers take an `Integer wrapping`: rocemit's Roc no longer
  says which Codex integers wrapped.

Where a Roc program could mean two things, roc2codex refuses rather than
picks; Roc from anywhere but rocemit will meet refusals first.

## The unit

`codexrun` runs a RESOLVED unit from anywhere: the Foreword chapters every Codex
program cites (`ListUtils`, `Tuple`) and `Console`, copied from a Cobblestone
checkout (`$COBBLESTONE`, default `~/showell_repos/cobblestone-u62`), then this
program's chapters as `Roc--Name`.

## Where it stands (2026-09-24)

506 of 526 round-trip, none wrong. 10 are refused on purpose: a program that
pokes raw memory is emitted over rocemit's `Mem`, threaded through every
function, which is a whole-program rewrite rather than an idiom. The rest are
single cases: record literals of no declared record type, and reals written
as their bits.
