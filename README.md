# rocflight

An interpreter for [Roc](https://www.roc-lang.org), written in Rust: a parser, a
bidirectional type checker, and a register VM in safe Rust.

Two goals, in this order:

1. **Full feature parity with the Roc compiler.** Not "mostly works" — every syntax
   feature is verified against the real `roc` binary, and a divergence is a bug.
2. **As close to bare metal as safe Rust reaches.** Every optimization is measured, and
   none of them is allowed to cost goal 1. No `unsafe` in the interpreter; the one
   exception is `host/`, the crate that hands values to a platform's compiled host,
   which is foreign code by definition.

```bash
cargo build -p rocflight-host --release --target x86_64-unknown-linux-musl  # once; needs `rustup target add x86_64-unknown-linux-musl`
cargo build --release
./target/release/rocflight path/to/main.roc
```

The first line builds the interpreter as a platform's `app`; the second embeds it, so
the release binary is self-contained. Running an app on a real platform links the two
together with `zig` (on the path), once per platform.

`Learning.md` is the maintainer's map: the pipeline, the gates, how to add a feature,
and the measured history of everything that was tried.

---

## First, the credit

**Roc is not ours.** This project interprets a language designed, built, argued over and
documented by other people, over years, for free.

Roc was created by [**Richard Feldman**](https://github.com/rtfeldman) and is built by a
community — the `authors` file in the compiler repo lists **172 names**, and the work is
stewarded by the [Roc Programming Language Foundation](https://www.roc-lang.org), a US
501(c)(3) nonprofit. It is released under the Universal Permissive License, © 2019
Richard Feldman and subsequent Roc authors.

Everything this interpreter knows about Roc, it learned from their work:

- The [**language reference**](https://github.com/roc-lang/roc/tree/main/docs/langref),
  which phase 21 was written against.
- The [**examples**](https://github.com/roc-lang/examples), all 28 of which are vendored
  here under `tests/roc/examples/` and used as the outside-in correctness gate. They
  found bugs no test of ours would have.
- The **compiler itself**, which is the arbiter for every test in this repo. When
  rocflight and `roc` disagree, `roc` is right.

If you find this project useful, the people to thank are theirs, not ours.
[Sponsor Roc](https://github.com/sponsors/roc-lang), or come say hello in
[their Zulip](https://roc.zulipchat.com) — they are, genuinely, very friendly about it.

> Roc is still pre-1.0. Its own README opens with *"Work in progress! Roc is not ready
> for a 0.1 release yet."* This interpreter chases a moving target on purpose, and is
> pinned to `nightly-2026-09-27-a3ce7f1`.

---

## Cool facts about Roc

Some from their docs, some learned the hard way while making this thing agree with the
compiler.

**It's named after a mythical bird.** The logo is an origami bird — an homage to Elm's
tangram logo. Per the FAQ, the name also won because it gives a three-letter file
extension and has "incredible potential for puns."

**Every operator is a method.** `a + b` *is* `a.plus(b)`. `-x` is `x.negate()`, `==` is
`is_eq`, `//` is `div_trunc_by`. Define `plus` on your type and it gets `+` — there is no
separate operator-overloading mechanism, because operators were never separate.

**The pipe binds tighter than everything.** `|>` binds *more* tightly than `+` and `*`,
which is the opposite of most languages that have it.

**There is no character type.** `'a'` is the number `97`. It is a number literal with a
different spelling, so `'a' + 1` is `98` — and, being a bare literal, it inspects as
`98.0` until something annotates it.

**An unconstrained number literal is fractional.** `a = 7` then `a.to_str()` prints
`7.0`, not `7`. Annotate it and you get `7`. Type annotations are not always decoration.

**A range is not a list.** `Str.inspect(0..<3)` gives `<opaque>`, and you cannot pass one
where a `List` is wanted. It exists to be iterated.

**There is no null.** The FAQ quotes null's inventor calling it his "billion dollar
mistake." Roc has no null and no `Maybe` — errors are `Try` with explicit tags, so the
set of things that can go wrong is written in the type.

**An application has exactly one platform.** Not a framework, not a library set — the
platform author controls which primitives exist and how they are implemented, so a
platform can give a coherent experience for its whole domain. It is the most distinctive
idea in the language.

**The whole compiler runs in a browser.** No backend server. It compiles to machine code
or WebAssembly.

**The compiler is written in Zig now.** It used to be Rust. They rewrote it.

---

## Status

| | |
|---|---|
| roc's eval tests | **1,953 / 1,953** pass with rocflight as a fifth backend of roc's own eval harness, and all 72 of its problem tests are refused: `tests/check_eval.sh --strict` is green |
| Golden pairs | **99 / 99** across 20 phases |
| Rust tests | **574** pass; 2 `vm_test` assertions fail, and are stale rather than broken (see `Learning.md`, "Known red gates") |
| Language examples | **20** of the 28 vendored examples match `roc` byte for byte — Snake among them, on basic-cli's real host, including a whole game played key by key. 2 fail, 6 are pending on language gaps, 1 `roc` itself rejects |
| `Builtin.roc` | **12 of 12** members parse; 1,443 definitions in Roc, 1,109 intrinsics in Rust |

```bash
tests/check_eval.sh --strict    # roc's own eval tests, rocflight as a fifth backend
tests/check_roc.sh --strict     # the 99 golden pairs — the definition of done
tests/check_examples.sh         # roc-lang.org's own examples, outside-in
tests/check_builtin.sh --strict # the vendored Builtin.roc still parses
tests/check_artifact.sh         # the parsed-at-build-time blob regenerates identically
tests/check_host.sh             # linked into basic-cli's real host, calling its effects
cargo test --quiet              # the Rust side
tests/bench.sh                  # performance, against a saved baseline
tests/bench_compare.sh          # the same programs under roc's interpreter and dev backend
```

Eight of the 28 examples are apps on basic-cli's compiled host. rocflight does not
re-implement that host: `rocflight main.roc` links the interpreter INTO it — the
platform's own recipe, `zig`'s lld, once per platform, about 100 ms — and runs the
result, so `Stdout.line!` is the platform's Roc calling the platform's C. The pending
ones are blocked on language gaps their platform modules expose (a string literal
standing for a `Path`, package imports, two inference cases); `--strict` makes those
fatal. `Dict` and `Set` run `Builtin.roc`'s own open-addressing table rather than a Rust
stand-in.

### The command line

It is `roc`'s, minus everything an interpreter has no business doing — there is no
`build`, `bundle`, `install`, `glue`, `fmt`, `docs` or `repl`, and none of `roc`'s
options, which all configure codegen, caching or parallelism that rocflight does not
have. What is left is the part that runs a program:

```
rocflight [ROC_FILE] [ARGS]...   run it (default: main.roc, as `roc` does); the rest is the app's
rocflight test [ROC_FILE]        run the file's top-level `expect`s, like `roc test`
rocflight version                print the version
rocflight help                   print the above
```

An app on a real platform is run by linking the interpreter into that platform's host.
The release binary carries what it needs for that (`build.rs` embeds
`librocflight_host.a`; the driver extracts it into `~/.cache/rocflight` on first use),
and looks beside itself or at `ROCFLIGHT_LIB` first for development. `zig` must be on
the path: it is the linker, as it is for basic-cli's own builds.

There are no `--` options in the release binary, on purpose: everything that was one was
a development aid, and a switch that changes how a program runs is a way to run it
against something other than the real interpreter. `Builtin.roc` is the clearest case —
it used to be selectable with `--load-builtins`, and it is not a choice, it is the
runtime. The pipeline can still be inspected in **debug builds**, where `rocflight help`
lists `--show-desugared`, `--show-ast`, `--ast-only`, `--show-platforms` and
`--builtins`; the release binary rejects all of them as unknown arguments.

---

## Performance

`tests/bench.sh` reports medians against a saved baseline and checks each benchmark's
output, so a change that is fast and wrong fails instead of looking like a win.

It started as a tree-walker. Two rounds of representation fixes took it a long way, and
then a **register VM in safe Rust** replaced it — built alongside it for six phases,
differentially gated against it at every step, and switched over only once both engines
passed every gate on every file.

Against roc's own interpreter (`roc --opt=interpreter`, the LIR interpreter in
`roc-compiler/src/eval`) and its dev backend, on the same programs, measured by
`tests/bench_compare.sh` (medians, nightly-2026-09-03, wall time including each engine's
parse and compile; roc's build cache is warm):

```
benchmark        rocflight   roc-interp   roc-dev   interp/rocflight
calls                  5ms         71ms      31ms         14.2x
closure_capture        2ms        151ms      74ms         75.5x
closure_in_loop        9ms        752ms      69ms         83.6x
iter_range           174ms      35640ms     485ms        204.8x
list_ops               2ms        154ms      75ms         77.0x
list_pass              3ms         38ms      34ms         12.7x
loop                   8ms       3196ms      89ms        399.5x
matching              19ms       1258ms      71ms         66.2x
matching_tail         21ms        309ms      30ms         14.7x
records                7ms        796ms      70ms        113.7x
records_tail           6ms         83ms      30ms         13.8x
strings                6ms        197ms     130ms         32.8x
```

Every benchmark's output is checked against roc's on every run. Two things had to change
to get there: each program's work depends on `args.len()`, because roc evaluates a pure
call with literal arguments at compile time; and the programs use roc's own names
(`Try.ok_or`, not an invented `with_default`).

Eight ceilings are gone rather than merely improved:

- List work is **linear**, not quadratic, and passing a list to a function is a
  **refcount bump**, not a copy. A register move used to deep-copy a `Vec`, so a loop
  that handed 8,000 elements to each iteration took four seconds; it takes six
  milliseconds, and `tests/bench/list_pass.roc` keeps it that way.
- Type-checking a block is **linear in its bindings**: 3,000 bindings cost 1.5 seconds,
  and now cost 11 milliseconds.
- A chain of statements is walked in a **loop**, not a Rust frame per statement, in the
  parser, the checker and the compiler alike. 6,000 statements in one block overflowed
  the stack; 20,000 run in 48 milliseconds.
- `fold` and `map` and six more callback methods are **compiled into the frame** when the
  checker has proved the receiver is a list. A literal lambda callback is inlined into
  the loop, so there is no call at all.
- Building a 40,000-character string peaks at 6 MB where it used to reach **710 MB**.
- Recursion is heap-allocated frames, so 500,000 levels run in 4 MB — the tree-walker
  exhausted a 256 MB reserved stack at 200,000 — and a **tail call reuses its frame**,
  so five million tail calls run in 2.8 MB.
- A `for` over a range never builds one, so `0..<10_000_000` allocates nothing.
- Compiling 8,000 top-level declarations was quadratic (94ms); it is linear (2.2ms).

Names are resolved once, at compile time: a local is a register, a captured variable an
index, a top-level name a slot, a top-level function a chunk id. Nothing compares a
string at run time. `Value` is **48 bytes**, with a guard test to keep it there, and the
crate is `#![forbid(unsafe_code)]`.

On a short program the VM is 1–4% of the time and the rest is the front end, which is
why `Builtin.roc` is parsed *and* compiled at build time and read back as a blob. A
four-line `Dict` program is **1.14ms** in process, against 3.5ms before that work.
`ROCFLIGHT_TIME=1` on any run prints each phase; `ROCFLIGHT_CODE=1` dumps the bytecode.
The measured history, including everything tried and rejected, is in `Learning.md`.

---

## Layout

```
src/desugaring/  text-level sugar, before parsing
src/parser/      recursive descent → AST; the largest piece, and where most syntax lives
src/types/       bidirectional checker with let-polymorphism
src/vm/          the register VM: compiler, liveness, peephole, opcodes, machine
src/eval/        builtins, operators, inspect, lazy iterators, Dec/F32 math, crypto
src/platform/    platform resolution, module loading, ABI, marshalling, the link driver
src/builtin.rs   reading the vendored Builtin.roc
src/artifact.rs  the parsed-and-compiled-at-build-time Builtin.roc blob
host/            the interpreter as a platform host library (the only unsafe)
tests/roc/       99 golden pairs across 20 phases, plus the 28 vendored examples
tests/bench/     benchmark programs and the saved baseline
```

---

## Known ceilings

Written down rather than hidden, because an interpreter that quietly disagrees with its
compiler is worse than one that says where it doesn't:

- A nominal's type ARGUMENTS are dropped: `Dict(Str, U64)` and `Dict(I64, Bool)` are one
  type here, so an element's type is still a variable.
- Nominals are erased, so the runtime tells them apart by SHAPE. A record with exactly an
  opaque nominal's fields inspects as `<opaque>` too, and a `Set` and a `Dict` are the
  same shape — only the checker separates those.
- The `Encoding` protocol's own members are Rust rather than roc's. JSON round-trips and
  a type's `encoder_for` runs, but another format would need the real thing.
- `.iter()` on a list **is** that list, so nothing after the checker can tell
  `xs.keep_if(p)` from `xs.iter().keep_if(p)` — and `map` produces its output list
  rather than fusing into its consumer. A range that stays a range is lazy and allocates
  nothing; an `Iter` that is its own value is the fix, and it is a representation change.
- `where` constraints are read for the names they promise, not verified.
- A frame has at most 65,535 registers, and every `let` in a block takes one.
- A cyclic record type (`{ ..p, next: p }`) is refused by `roc` as anonymous recursion;
  the checker here reports nothing and the program runs.
- `.1` on a nominal over a tuple works here; roc keeps the nominal opaque to tuple access.
- `var` names are tracked in one flat set, so a `var x` in one function makes a later
  `x = e` in another read as a reassignment rather than a shadow.
- A refutable top-level pattern (`(1, b) = pair`) and `..rest` in a top-level
  destructuring are refused: both need a match, and a top-level binding cannot be scoped.
- `m-n` with no spaces is subtraction here; `roc` rejects it. More permissive, which is
  the safe direction, and no golden pair can depend on it.

---

## License

This interpreter is an independent project and is not affiliated with or endorsed by the
Roc Programming Language Foundation. The Roc language, its compiler, its documentation
and its examples are the work of the Roc authors, under the Universal Permissive License
1.0; the vendored examples under `tests/roc/examples/` retain that license and their
original copyright.
