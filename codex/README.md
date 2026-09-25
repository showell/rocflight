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

## Roc to Rust

`src/rust/` writes the same checked program as one Rust file, which `rustc`
compiles; `codex/rust.sh` compiles each ported test (a debug build, whose
overflow checks panic where Roc's arithmetic crashes), runs it and diffs its
output against the same verdict.

    roc2rust app.roc out.rs
    codex/rust.sh                    # all of them; rust.sh NAME... for some

Unlike roc2codex it translates every module, rocemit's `CceText`, `CceChar`
and `Prelude` included; `runtime.rs` is Roc's own builtins by hand, and heads
every program. It writes only what `main!` reaches.

- **Structural records and unions** (`{ p : I64 }`, `[Just(a), None]`) are one
  generic Rust type per set of field or tag names (`Rec_p<T0>`,
  `Tags_Just_None<T0>`); `[Ok(a), Err(e)]` is `Result`. An open union (a lone
  tag's type) is the declared union holding its tags.
- **A nominal** is a named type, with `Rc` where a nominal field holds another
  (only a nominal can be recursive); one over a list or a scalar (`CceText`) is
  its backing, as Roc erases it.
- **A list** is an `Rc<Vec<T>>` that copies on write, so a uniquely held list is
  written in place, as in Roc. Values are cloned where read.
- **A closure** is `Rc<dyn Fn(..)>`, its captures cloned in. A `match` is a
  labeled block of nested `if let`s. `main!` runs on a 1 GB stack: Rust does no
  tail calls.
- **Types rustc can infer are left to it**: a variable in a body that is not
  the enclosing definition's own is `_`; one nothing constrains is `()`.

It relies on rocemit writing plain-typed locals with an annotation: an
unannotated local is let-generalised, and its string or number literal stays
a `Str` or a fraction where its uses want a `CceText` or an `I64`.

506 of 521 compile and print their verdict. 7 of the rest are the programs
over rocemit's `Mem`; 3 are rocflight dropping a type's arguments where it is
written before its declaration (B-Teague/rocflight#11); 5 are single cases not
yet looked at.

## Where it stands (2026-09-24)

514 of 521 round-trip, none wrong. The other 7 are refused on purpose: a
program that pokes raw memory is emitted over rocemit's `Mem`, threaded through
every function, which is a whole-program rewrite rather than an idiom.

Record types come back by name because rocemit writes each one (without type
parameters) as a nominal, `Byte := { val : I64 }`: two Codex records of one
shape are otherwise one Roc type.

## This branch, and what is not upstream

`codex-emit` is `all-prs` plus the commits below. `all-prs` is Brian's `main`
with every open PR branch merged as sent, and no more: new rocflight fixes
start there, so anything a fix depends on is something already sent. The
corpus (`codex/ported.sh`) and all five Fast Track experiments run on
`all-prs` alone; this list is what the tools here need beyond it.

| Commit | For | Upstream | Remove when |
|---|---|---|---|
| fork: record each node's type | roc2codex, roc2rust read the typed tree | not yet proposed (changes no behavior) | a PR of it merges |
| fork: roc2codex and roc2rust | the tools themselves | not for upstream | never |
| fork (#13, partial): an if/match against an unsolved type joins its branches | roc2rust's types for a lambda whose branches return different tags | issue #13 | #13 is fixed, or nothing needs it (Fast Track's `Tables` is annotated) |
| fork (#14): Try's functions have their declared types | roc2rust's types for `Try.map_ok(t, f)` | issue #14 | #14 is fixed |

Before writing a fork-only fix, ask whether the Roc should just be clearer (an
annotation, a distinct name): Fast Track's `exp_cards` renamed a helper rather
than depend on #17's name resolution.
