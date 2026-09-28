# Learning.md

How this interpreter works, what its words mean, what you have to know before you change
it, and what has already been measured. `README.md` is the tour for a visitor; this is
the whole map for whoever maintains it next. Sections 1–10 are live; 11–14 are the
record of how it got here, kept because every one of them stops someone repeating work.

---

## 1. The one rule

**`roc` is the arbiter.** When rocflight and the real compiler disagree, rocflight is
wrong — always, including when rocflight's answer is more sensible. Every gate in this
repository is a comparison against `roc`'s own output, and nothing in it decides on its
own what correct means.

Two corollaries that have caused more bugs here than anything else:

- **Probe before implementing.** The language reference is partly aspirational
  (`list[i]` and `continue` are documented and rejected; `dictionaries-and-sets.md` and
  `iterators.md` are TODO stubs; `{ r & x: 5 }` appears only inside a commented-out
  block and is rejected — the real spelling is `{ ..r, x: 5 }`), and the reverse bites
  too: a form that looks broken is often just misused. `.?` sat in these docs as
  "segfaults roc, blocked upstream" for several phases; it segfaults only on an ordinary
  field of a plain record, and works correctly on the optional field it is meant for.
- **No quiet fallback.** Anything the compiler cannot lower is an `Err` naming the
  construct. There is no second engine to take over, because a program that silently
  runs through a different path is a program whose parity you no longer know.

The pinned compiler is `nightly-2026-09-27-a3ce7f1`, checked out at `roc-compiler/`
(untracked — a working copy of roc's own repo, not part of this one).

---

## 2. The pipeline

`rocflight file.roc` is one function: `run::run_file` (`src/run.rs`). Set
`ROCFLIGHT_TIME=1` to see each step's wall time; `ROCFLIGHT_CODE=1` dumps the bytecode.

### Step 1 — desugar (`src/desugaring/`)

A **text-level** rewrite before parsing. Its output must be a real Roc program that
passes `roc check` on its own, because that is exactly what a golden pair's
`.desugared.roc` file is. Type annotations are *preserved*, not stripped. Section 8 has
the per-rule detail.

### Step 2 — parse (`src/parser/`, ~6k lines, the largest piece)

Hand-written recursive descent producing `ast::Expr`. It also collects the side-tables
later phases need and cannot recover from the tree: nominal declarations, nominal
parameters, `where` methods, literal suffixes, overflowed literals, field defaults,
imports, ingests, the app's entry point.

Statement chains are walked in a **loop**, not a Rust frame per statement — here, in the
checker, and in the compiler alike. 6,000 statements in one block used to overflow the
stack.

`?` and `??` are desugared **here**, not in step 1: `??` needs its operand's extent, and
`?` has to move the rest of the block into the `Ok` arm, which raw-text substitution
cannot locate reliably.

### Step 2b — platforms and modules

- `platform::real::verify_app` resolves any real platform the app names, *before*
  checking, so a bad import is reported against the platform's sources.
- `builtin::needed_by(source)` reads which members of the vendored `Builtin.roc` this
  file actually needs, off the raw text. Only those load. Never a command-line choice:
  `Builtin.roc` is the runtime, not an option.
- Local modules (`import Hello exposing [hello]`) are ordinary `.roc` files beside the
  importer, desugared and parsed the same way.
- Platform modules (`Stdout.roc` etc.) are loaded as the Roc they are. A member with a
  body is Roc; a member with only an annotation dispatches to the compiled host.

### Step 3 — type check (`src/types/checker.rs`)

Bidirectional checking — `synth` (infer) and `check` (verify against an expectation) —
with Hindley-Milner unification, let-polymorphism, and a substitution resolved at the end.

The checker's *second* job is as important as the first: it hands the compiler facts that
let it emit better code and correct code. `integer_binops` (both operands proved integer
→ `BinInt`), `dispatch_modules` (which module a method call resolves to), `dec_literals`
/ `f32_literals` / `u128_literals`, `literal_conversions` (a literal standing for a
nominal, via `from_numeral` / `from_quote` / `from_interpolation`), `missing_fields`,
`default_sites`, `match_types`, `for_iter_calls`. The construction of `compile::Unit` at
the end of `run.rs` *is* the checker→compiler contract.

**Numeral polymorphism** is the subtlety that recurs. An unconstrained number literal is
fractional in Roc: `a = 7` prints `7.0`. So a literal synthesises to a *variable*, the
variable is recorded in `numeral_vars`, and whatever pins it decides the width; whatever
nothing pins defaults at the end. Most "wrong number type" bugs are a literal's variable
getting tied to the wrong thing.

### Step 4 — compile (`src/vm/compile.rs`)

AST → bytecode. Everything this pass does is work a tree-walker would redo on every
execution: which register a name lives in, which chunk a call goes to, which values a
closure captures, where a branch lands.

**Names are resolved here and never again.** A local is a register index, a captured
variable a capture index, a top-level value a slot, a top-level function a chunk id.
Nothing compares a string at run time — that is the whole reason this is faster than
walking the tree.

Then `vm::liveness` proves which reads are the last read of a value (so a move can *take*
rather than clone), and `vm::peephole` folds shapes like a jump-to-`IterNext` into a
single back-edge opcode.

The `expr` match is exhaustive over `Expr` **on purpose**: add an AST variant and this
file stops compiling until you lower it.

### Step 5 — run (`src/vm/mod.rs`)

A register VM. Four load-bearing properties:

1. **No `unsafe`** — the crate is `#![forbid(unsafe_code)]`. Register access is
   bounds-checked indexing, dispatch is a `match`, frames are indices. A wrong opcode is
   a panic with a message, never memory corruption.
2. **Names are compile-time** (above).
3. **Roc calls do not recurse in Rust.** `frames` is a `Vec`, so a Roc call costs ~32
   heap bytes, and "too deep" is a Roc error rather than a stack overflow. *The
   exception:* a builtin's callback (`xs.sort_by(f)`) re-enters through `call_closure`,
   which does nest a Rust frame.
4. **A tail call reuses its frame**, so tail-recursive Roc runs in constant memory.

Builtins live in `src/eval/`. They take and return `Value`s and hold no interpreter
state, which is why they survived the tree-walker's removal unchanged.

---

## 3. Glossary

| Term | What it means here |
|---|---|
| **golden pair** | Two files per syntax feature: `x.roc` (sugared, *no* annotations) and `x.desugared.roc` (explicit types, no sugar). Four outputs must be byte-identical: `roc` and `rocflight`, on both files. The definition of done. |
| **eval backend** | rocflight wired into roc's *own* eval test runner as a fifth backend (alongside roc's interpreter, dev backend and wasm). `rocflight eval FILE` is its entry point. 1,953/1,953. |
| **intrinsic** | A `Builtin.roc` member with an annotation and **no body**. It must be implemented in Rust (`src/eval/`). The real compiler makes the same split in `BuiltinLowLevel.zig`. |
| **defined member** | A `Builtin.roc` member *with* a body. Ordinary Roc, compiled into the program ahead of the app; dispatch finds it like any `Type.method`. |
| **artifact** | `src/roc/Builtin.artifact` — `Builtin.roc` already parsed AND compiled, written by `gen-artifact` and `include_bytes!`d back in. Names in it are borrowed from the blob rather than interned. |
| **prefix / group** | The precompiled block of chunks and globals that `Builtin.roc` occupies at the front of every program. Same block for every program, so it can be compiled once. `artifact::Prefix`, `compile::Group`. |
| **chunk** | One compiled function: code, constants, register count, arity, name tables, pattern table, and a `spans` array mapping each instruction back to an AST node (that is how runtime errors get a line). |
| **slot / global** | A top-level binding that is not a function. Addressed by index. |
| **capture** | A value a closure copied out of the enclosing frame when it was made. A captured `var` is a shared `Value::Cell` instead, so assignment is seen by everyone. |
| **nominal** | `Name := backing` / `Name :: backing`. Distinct from every other nominal even with identical backing. **Erased at run time** — the VM tells them apart by *shape* (`NominalShape`), which is why `Set` and `Dict` look the same to the runtime. |
| **opaque nominal** | Declared with `::`. Inspects as `<opaque>`. Within one file roc does not otherwise distinguish it from `:=` — opacity needs module boundaries — so the two are synonyms here. |
| **dispatch** | `a.method(b)`. Resolved at compile time when the checker names the receiver's module; otherwise at run time via `Program::methods_by_name`, ranked by shape fit and nominal depth. |
| **operator method** | `a + b` *is* `a.plus(b)`, `==` is `is_eq`, `//` is `div_trunc_by`. A program that defines one gets `BinDispatch`; everything else gets plain `Bin`. |
| **hosted** | A platform function declared with a type and no body. Marshalled out to the platform's compiled host (`src/platform/hosted.rs`, `host/`). |
| **host library** | `host/` — the interpreter as a static library that a platform's `main` links and calls `roc_main` on. The *only* crate with `unsafe`, by definition. |
| **numeral taint** | A type variable that came from a numeric literal (`numeral_vars`). Polymorphic until something pins it; unpinned, it defaults fractional. |
| **PEND** | A golden pair that is correctly written but that rocflight does not yet match. Non-fatal without `--strict`. |

---

## 4. The four data structures worth knowing cold

**`Value`** (`src/eval/value.rs`) — **48 bytes**, with a guard test. It is moved on every
binding, argument, list element and return, so its width is a tax on the whole
interpreter. Anything variable-length is behind an `Rc`: `Str(Rc<str>)`,
`List(Rc<Vec<..>>)`, `Record(Rc<..>)`, `Tuple(Rc<..>)`, `Tag(name, Rc<[Value]>)`,
`Closure(Rc<..>)`. That is not a micro-optimisation — it is why passing a container to a
function is a refcount bump instead of a copy. **If you add a variant, box it and check
the size test still passes.** Three variants force the 48 independently (`Simd`, `Range`,
`Tag`), so it cannot be shrunk to 32 without undoing work that paid — see §12.

Numbers are not one type: `Int(i128)`, `U128`, `Float(f64)`, `F32(f32)`, `Dec(i128)`
(fixed-point, 18 places, *not* a float — that is the whole point of `Dec`), plus `Simd`.

**`Type`** (`src/types/mod.rs`) — every name in it is an interned `&'static str`
(`memory::string_pool`), because a `Type` is cloned constantly and the checker has 135
clone sites. `Record` and `TagUnion` carry an `open` flag: open means "at least these",
which decides both unification and match exhaustiveness.

**`Op`** (`src/vm/mod.rs`) — the opcode set, grouped: loads/moves, arithmetic (`Bin` /
`BinInt` / `BinK` / `BinIntK` — the specialised forms exist because the checker proved
something), control flow and calls (`CallFn` known callee, `Call` value callee, `TailCall`
frame-reusing), aggregate construction, pattern **tests** (each jumps away on *non*-match)
then destructuring, loops (`IterNext` / `IterNextBack`), builtins and dispatch,
statements. Each variant's doc comment says why it exists; several exist because of a
specific measured cost, and deleting one to "simplify" reintroduces it.

**`Program`** (`src/vm/mod.rs`) — chunks, globals count, the top-level chunk, the optional
builtin `prelude` chunk, the entry point, the method tables, and the nominal shape/depth
tables that make run-time dispatch correct.

---

## 5. Repo map

```
src/desugaring/   text-level sugar, before parsing
src/parser/       recursive descent → AST, plus the side-tables later phases need
src/ast/          Expr, Pattern, node identity (NodeId → source offset → line)
src/types/        bidirectional checker, unification, the checker→compiler facts
src/vm/compile.rs AST → bytecode; liveness.rs and peephole.rs are its passes
src/vm/mod.rs     the opcodes, the machine, run-time dispatch
src/eval/         builtins, operators, Str.inspect, lazy iterators, Dec/F32 math, crypto
src/platform/     platform resolution, module loading, layout, ABI, marshalling, driver
src/builtin.rs    reading the vendored Builtin.roc: which members, bodied vs intrinsic
src/artifact.rs   the parsed-and-compiled-at-build-time Builtin.roc blob (read + write)
src/run.rs        the pipeline, as a library call
src/main.rs       the CLI
host/             the interpreter as a platform host library (the only unsafe)
build.rs          builtin member index, artifact staleness check, host lib embedding
tests/roc/        golden pairs (20 phases) + the 28 vendored examples
tests/bench/      benchmark programs + baseline.tsv
```

---

## 6. The gates

```bash
cargo test --quiet              # the Rust side
tests/check_eval.sh --strict    # roc's own eval tests, rocflight as a fifth backend
tests/check_roc.sh --strict     # the golden pairs — the definition of done
tests/check_examples.sh         # roc-lang.org's own examples, outside-in
tests/check_builtin.sh --strict # the vendored Builtin.roc still parses
tests/check_artifact.sh         # the artifact still regenerates byte-identically
tests/check_host.sh             # linked into basic-cli's real host, calling its effects
tests/bench.sh                  # performance against the saved baseline
tests/bench_compare.sh          # the same programs under roc's interpreter and dev backend
```

`check_eval.sh --strict` and `check_roc.sh --strict` define parity. The rest catch
specific classes of rot.

**The eval suite is the one that catches parser changes.** Two refusals in §12 were found
by it while `check_roc.sh` and `check_examples.sh` both stayed green: guarding
`skip_trivia` on start-of-line read 1881 of 1953, and narrowing `needed_by` read 1949.
`check_roc.sh` on the **debug** build is what catches a span pointing outside its node
range, which in release is silently a nonsense error location.

`check_eval.sh` detail: `--report` prints the family and problem tables; `--keep` leaves
each failure's source, stderr and exit code under `target/eval-fails/`; `--filter X`
scopes to tests whose name contains `X` (several union); `-- --timeout 5000` reaches the
runner for a hang. `rocflight eval FILE` prints `Str.inspect` of the module's value and
exits 0, or prints an error NAME and exits 2 (`Crash`, `CompileError`).

### Known red gates

Both predate any current work and are written down so a later change is not blamed for
them:

- **`cargo test`**: `vm_test` fails `a_var_a_closure_captures_is_refused_rather_than_going_stale`
  and `what_the_vm_refuses_is_refused_deliberately`. Both assert the compiler REFUSES a
  `var` that a closure captures; it now compiles one through a shared cell (`MakeCell` /
  `CellGet`), so the tests outlived the refusal they were written for. Stale assertions —
  but someone has to decide what they should assert instead.
- **`tests/check_examples.sh`**: `CustomInspect` (a `dbg` of a nominal renders its
  backing) and `GraphTraversal` (`List.concat needs a List, got <opaque>`) fail. The
  script exits 0 because it is not `--strict`, which is how they went unnoticed.

---

## 7. Adding a language feature

**Every syntax feature gets its own golden pair**, and both files must compile cleanly
under `roc`:

```
tests/roc/<NN>_<phase>/<syntax>.roc              # sugared — how a person writes it
tests/roc/<NN>_<phase>/<syntax>.desugared.roc    # explicit types, no sugar
```

```
        roc run <sugared>   ═══   roc run <desugared>
              ║                          ║
     rocflight <sugared>   ═══   rocflight <desugared>
```

Four programs, one output. Each edge catches a different class of bug: the top edge that
the desugaring changed the program's meaning; the left and right that the interpreter is
wrong about the surface or the explicit form. **Running the interpreter against the
desugared file is not redundant** — it is a distinct path through the parser, and it found
a real bug: top-level bindings parsed their value with `parse_call_expr`, so
`a = 2 + (3 * 4)` failed while the sugared file passed.

The requirement list:

1. `roc check` reports **no errors** on *both* files. Note `roc check` **exits non-zero on
   warnings**, so "compiles cleanly" means warning-free — a constant scrutinee earns
   "this match value is known at compile time", so keep test inputs non-constant.
2. All four outputs are **byte-identical**.
3. Both files build the **same AST** (`cargo test --test desugar_test`, which needs
   neither `roc` nor a built binary). This catches what output comparison cannot: five
   pairs once had bindings inside `main!` in one file and at the top level in the other —
   same output, different programs. `desugar_test` compares the AST only, not the
   inferred type, because a desugared file's declared type is legitimately more specific.
4. The `.desugared.roc` carries **explicit type annotations**; the sugared file carries
   **none at all**. That is what proves inference works — an annotation on every binding
   lets a checker coast.
5. One **feature** per pair, not one phase. `+` and `//` are separate files.

Because an unannotated integer literal is fractional in roc, deleting an annotation can
change a value's printed form. Pin the type through a **use** instead:
`I64.to_str(x)`, `I64.from_str("42") ?? 0` (which propagates — seeding a fold with it
pins the whole list), `xs.len()` for `U64`, or passing it to an annotated function.
Prefer a pin that shows up in the output.

**Twelve files keep an annotation**, each saying why in a comment, for two reasons only:
the annotation *is* the syntax under test (`where [a.to_str : a -> Str]`,
`{ name: Str, .. }`, `[Red, ..u]`, `Bytes : List(U8)`, `Wrapper(a) -> a`, `a -> a`), or
`roc` refuses the file without it (a top-level `empty = []` is an unresolved polymorphic
value; a method block needs annotations before roc attaches its methods; an unannotated
function with a constant result makes roc warn). The gate rejects any other annotation in
a sugared file, so this cannot quietly rot.

Two gate modes, because a pair can be correctly written while the interpreter is still
catching up: **PAIR** (both files check, the two `roc run` outputs agree, annotations
present) is always fatal — failing it means the test files are wrong. **INTERP**
(rocflight matches roc on both files) is reported as `PEND` and is fatal only under
`--strict`, which is the definition of done.

### The order of work

1. **Establish ground truth with `roc`.** Never from memory. Two tools: the LSP
   (`roc experimental-lsp --stdio`, driven over JSON-RPC — `textDocument/hover` gives the
   type at a position, `completion` a `detail` per symbol, `inlayHint` a whole file; this
   is how `echo!`'s signature was found), and **deliberately wrong annotations**, because
   `roc check` names the type it expected:

   ```bash
   printf 'app [main!] {}\n\nmain! : Str => Str\nmain! = |_| ""\n' > /tmp/t.roc
   roc check /tmp/t.roc
   #   But the platform requires:
   #       List(Str) => Try(_a, [Exit(I8), ..])
   ```

   `roc repl` is fine for scratch work but cannot show what a *platform* requires.
   The reference file for syntax is `roc-compiler/test/echo/all_syntax_test.roc` — its
   live code, not its comments.
2. **Write the golden pair and make it green under `roc` alone**, before touching Rust.
   The pair is the specification; if it will not compile, the feature is not yet
   understood well enough to implement.
3. **Implement in pipeline order**: desugarer → parser/AST → checker → compiler → VM or
   builtin. Skipping a layer is how you get a feature that parses and mis-runs. **Prefer
   composing existing AST variants over adding one** — blocks are the worked example:
   `{ a = 1 \n f(a) \n expr }` needs no `Block` node, because the parser lowers it to
   nested `Expr::Let` with non-binding statements bound to `_`.
4. **Verify**: `tests/check_roc.sh --strict`, then `tests/check_eval.sh --strict`, then
   `cargo test`. Also check the interpreter's own emitted desugaring is valid Roc:

   ```bash
   ./target/debug/rocflight --show-desugared F.roc 2>&1 \
     | sed -n '/=== DESUGARED CODE ===/,/=== END DESUGARED CODE ===/p' \
     | sed '1d;$d' > /tmp/desugared.roc
   roc check /tmp/desugared.roc
   ```

Debug-only flags for working on a pair: `--ast-only` (AST and inferred type, no run),
`--show-ast` (both, then run; also lists every numeral the checker defaulted to a
fraction, by line:col — a `Dec` where an `I64` was meant starts as one of these),
`--show-desugared`, `--show-platforms`, `--builtins`.

---

## 8. Desugaring rules

What the text-level pass does, and what only *looks* like sugar. Its output must pass
`roc check` on its own.

| Form | Where | What happens |
|---|---|---|
| Type annotations | — | **Preserved verbatim.** The parser parses them (`capture_type_annotation`) rather than skipping; `Expr::Let` carries the declared type. That is what lets the checker reject a tag outside a closed union, check a `match` for exhaustiveness, and give every identifier a real type. |
| `=>` | — | **Never rewritten.** It is the effectful-function arrow *and* the `match` arm separator; a blind replacement corrupts every match in the file. |
| `foo!` | — | The `!` is **part of the identifier** (upstream `chompIdentGeneral` chomps it into the token). Nothing to rewrite, and it says nothing about whether a call returns `Try`. |
| `!foo` | parser | Unary logical not → a `Bool.not` call. |
| `a != b` | parser | Its own token; asks for `is_eq` and negates it. |
| `"${x}"` | parser | A primitive string form, not sugar for concatenation. |
| `255.U8` | desugarer | The suffix is lifted to an annotation: `small : U8` / `small = 255`. |
| `expr?` | parser | `match expr { Ok(v) => <rest of block>, Err(e) => Err(e) }`. Rejected on a block's final expression (it unwraps, so there is no continuation and roc calls it a type error). The error binds to the fixed name `e`. `ponytail: needs a gensym if a continuation ever refers to an outer e.` |
| `expr ?? d` | parser | `match expr { Ok(v) => v, Err(_) => d }`. The **loosest** operator: `x ?? 1 + 2` is `x ?? (1 + 2)`. |
| `r.?field` | parser | `Expr::OptionalField`, yielding `Ok(v)` or `Err(MissingField)`. NOT sugar for a match — the presence test happens at run time. Only for a nominal's `?:` field. |
| `field ?: T` | parser | `Type::Optional` on a nominal's backing record; a record without the field still unifies. Read with `.?`. |
| `field : T ?? d` | parser | **Defaulted**, not optional: filled at construction, so the record really has the field and plain `.field` reads it. |

Two rules used to transform and were wrong, which is why they are listed as explicit
no-ops: deleting annotations made the emitted file un-compilable and threw away the
types, and rewriting `=>` to `->` discarded effectfulness and broke every `match`. An
even earlier pass scanned for `!` character by character and emitted Rust-shaped
nonsense into `.roc` output (`match echo("hello") { Ok(v) => v, ... }`), collapsed
`a != b` into `a = b`, and deleted unary `!`. **When desugaring a construct, check what
the Zig compiler actually does with it first** — the target is always Roc source or a Roc
builtin.

Text-level work must not process inside string literals or comments, and must not match
`???` or `??!`.

---

## 9. Verified language facts

Each of these was checked by running `roc`, and each one was a bug here first.

**Entry point.** A platformless app is what `roc run` links the built-in default host
for. The default host requires **exactly** `main! : List(Str) => Try(_a, [Exit(I8), ..])`
— `Try`, not `Result` (`Result` is not in scope at all); `=>`, not `->`; and
`[Ok({}), ..]` is not accepted in place of the nominal `Try`. A headerless file implies
`app [main!] {}`. `main!` is also the export name for apps with a real platform.

**`echo! : Str => {}`**, provided by the **default host**, not the compiler — from a type
module it fails with "Nothing is named echo! in this scope" (`src/platform/host.rs`
models that). It writes with **no trailing newline**.

**`module [...]` headers are deprecated** upstream in favour of type modules. Do not add
support; do not write new ones.

**Numbers.** `to_str` dispatches on the numeric type: a bare `n.to_str()` on an
unannotated binding fails ("trying to dispatch a method named to_str on an unresolved
type variable"). `List.len` returns **U64**, which forces the whole `match` around it.
`1500.0` prints as `1500`: roc drops a whole float's `.0`. Digit separators must sit
between digits; `1.5e3` is `1500`; `'a'` is `97`.

**Precedence, tightest first:** postfix (`.field`, `.0`, `()`), `|>`, unary minus, then
the binary operators, then `??`. So `1 + 2 |> double` is `1 + double(2)` = 5, and
`-n |> inc` is `-(inc(n))`. Ranges bind **looser** than `+` and `*`: `1 + 1..<2 * 3` is
`2..<6`. Watch the test numbers — `2 * 3 |> double` gives 12 under both readings and
proves nothing.

**Whitespace is significant in two places.** `f(1)` is a call and `f (1)` is an error in
roc; without that rule the condition in `if n > 0 (…) else …` swallows the parenthesised
branch. And a `-` with space before and none after is unary negation, not subtraction —
otherwise a line starting `-x` reads as subtraction across the newline. Both need looking
*behind*, since the primary parser has already consumed the whitespace. Whitespace before
`.` is not significant.

**Braces.** A record field is `name: value`; a block annotation is `name : Type`. The
space before the colon is the discriminator, and every site accepting braces must share
that decision (`Parser::parse_braced`). A bare name **puns in a pattern but not in a
literal**: `{ name } = r` binds `name`, while `{ name }` as an expression is a block whose
value is `name`. `(1)` is grouping — roc has no one-tuple.

**`match`.** Arms are newline-separated (a comma is optional) and **order matters**. `_`
is the wildcard; `_unused` is an ordinary binding. A **false guard falls through** to the
next arm rather than failing the match, and a guarded arm does not complete an
exhaustiveness check. Pattern bindings are collected and bound only once the whole
pattern matches, or a nested pattern that fails on its last element leaks bindings.
`..` in a list pattern may sit at the end, middle or start, matches zero or more, and
`.. as name` binds the middle; at most one per pattern.

**`if` is an expression and `else` is mandatory** (roc: "The second branch of this if does
not match the previous branch"). The condition must be a `Bool`. `else if` is not a
separate construct — it is an `If` whose else branch is another `If`.

**Tags.** A tag literal is a one-tag union (`Red : [Red]`). Unifying two unions **merges**
their tags. Members sort by name, so declaration order cannot affect unification. Payload
arity and types are checked. `Try(a, b)` *is* `[Ok(a), Err(b)]`, so `Ok`/`Err` need no
special case. `Bool : [True, False]`, and a bare `True` is a boolean — not only the
qualified `Bool.True`. Openness (`[Red, ..]`) applies to parameter positions.

**Records.** `Str.inspect` sorts fields **alphabetically**, recursively. An update
`{ ..base, x: v }` builds a new record and cannot ADD a field. `..rest` binds every field
not named, producing a record with fewer fields — which is what makes `{ email: _, ..rest }`
a way to remove one — and such a pattern must not type as closed.

**Nominals.** `Name := backing` is nominal but **not opaque**: the plain backing value is
accepted where the nominal is expected, yet two different nominals with identical backing
do not interchange. Values carry **no nominal wrapper**, so `Str.inspect` shows the bare
backing and `Animal.Dog(x)` builds what a bare `Dog(x)` would. Exhaustiveness reaches
through a nominal to its backing union. Lambda parameters are **patterns**
(`|Point.{ x, y }|`, `|(a, b)|`). A sibling method is in scope unqualified inside a method
block. `Name.(payload)` constructs a non-record backing.

**Dispatch.** `receiver.method(args)` is `Module.method(receiver, args)` — receiver first,
which is why roc's builtins take their subject first. **Parens separate a method call from
a field read**: `s.is_empty` reads a field, `s.is_empty()` calls. `x.y` is field access
only when the receiver is lowercase. Chaining needs result types, so `xs.len()`,
`List.len(xs)` and `xs |> List.len()` all share one `builtin_result`.

**Pipelines.** `x |> f(a)` is `f(x, a)` — the piped value is **prepended**. Left
associative. `|`, `||` and `|>` all start alike, so the match-alternative separator has to
exclude `|>` explicitly.

**`var` and loops.** `$` is an ordinary identifier character, not a sigil with meaning —
mutability comes from `var`, and a plain binding cannot be reassigned. Assignment updates
in place; binding would shadow, and a loop body's scope pops. Loops are **expressions**
whose value is `{}`. `break` rides the error channel, so a stray one surfaces as an error
rather than vanishing.

**Top-level order.** roc's top level is order-independent, and `expect`s run after the
whole module is in scope. Comments are trivia **everywhere**, including between a
binding's `=` and its value.

**A type alias is transparent** (`Bytes : List(U8)` unifies freely with `List(U8)`); a
capital letter is what separates an alias from a value annotation. `where` constraints are
read for the names they promise, which the checker then permits on an unresolved variable.

**Still not in the compiler** (no pair can be written): `list[i]` subscript, `continue`
(crashes `roc` on the pinned nightly).

---

## 10. Maintaining it

### Re-syncing the vendored `Builtin.roc`

```bash
cp roc-compiler/src/build/roc/Builtin.roc src/roc/Builtin.roc
rm src/roc/Builtin.artifact              # a stale one fails the build, gen-artifact's included
cargo run --release --bin gen-artifact   # rebuild the parsed+compiled blob
tests/check_builtin.sh --strict
tests/check_artifact.sh
```

`build.rs` records an FNV-1a hash of the source in the artifact and **fails the build** if
they drift, with the regeneration command in the message. It also computes the member
byte-offset index so finding a member is a table lookup rather than a scan of 24,743
lines; a skewed offset shows up as a member failing to parse, which is what
`check_builtin.sh --strict` catches. A *missing* artifact is not an error: `build.rs`
leaves an empty one and `load` falls back to parsing, correct and slow.

Remember the contract: **bodied member = Roc, annotation-only member = your problem in
Rust.** The parser computes the split for free — an annotation a binding claims is a
definition, and one left unclaimed when its block closes is an intrinsic, which is
exactly the rule `BuiltinLowLevel.zig` applies.

### Performance work

```bash
cargo build --release
cp target/release/rocflight /tmp/before
# ... change, rebuild ...
ROCFLIGHT=/tmp/before tests/bench.sh --runs 9 --save
tests/bench.sh --runs 9
```

Always the release build, and always **A/B against a copied pre-change binary,
interleaved** — this machine drifts ~7% over a few minutes, which is larger than most
effects worth having. Four things to know before measuring:

- **`tests/bench/baseline.tsv` in the repo is not this machine's baseline.** It reads
  ±10–50% on an untouched tree.
- **Anything under a millisecond is a median of 15–25 runs, never one.** Two projections
  in §11 came from single readings of a cold page cache and were out by 3–4×.
- **Confirm the mechanism before writing the fix.** Three entries in §11 were wrong about
  *why* while being right that something was slow; a bytecode dump or an opcode histogram
  caught each one.
- **roc folds pure calls with literal arguments at compile time**, so a benchmark whose
  work does not depend on `args.len()` measures nothing. And **the eval suite is not a
  benchmark** — its per-test times are roc's harness plus fork/exec.

**The `exec` match is layout-sensitive.** Adding one branch to the `DispatchMethod` arm
moved `records` — which executes no `DispatchMethod` at all — by 6.5%. So a change to one
opcode's arm can cost more elsewhere than it saves, and the whole suite has to be A/B'd
for any VM change. An opcode histogram (ten throwaway lines counting in `exec`) is what
picked every item in optimization phase 8 over what the plan had queued; rebuild it
whenever this area is reopened, and never keep it, because counting in the dispatch loop
costs what it measures.

### Invariants not to break

- `#![forbid(unsafe_code)]` in the interpreter crate. `host/` is the sole exception and
  documents every unsafe operation it performs.
- `size_of::<Value>() == 48`, guarded by a test.
- No run-time string comparison for *name resolution*. (Field and tag names are still
  compared by content at access sites — documented at `Chunk::names`.)
- The compiler's `expr` match stays exhaustive; `liveness`'s `reads`/`kills`/`successors`
  match every opcode with **no wildcard arm**.
- Nothing is cached between runs: editing a `.roc` file always takes effect.
- No `--` options in the release binary. Debug builds get the inspection flags; release
  rejects them as typos.
- Stderr is free (the harnesses discard it); stdout is the program's answer. That is why
  `ROCFLIGHT_TIME` and `dbg` go to stderr.
- One reader of the artifact, `builtin::artifact()`, behind `builtin::generating` — a
  reader added anywhere else makes the artifact non-reproducible (it happened twice).

### Traps that have bitten before

- **A lone method answering everything.** A single `Try.is_eq` in scope used to answer
  every `==`, tuples included. Run-time dispatch filters candidates by nominal shape and
  only accepts an ambiguous set when one fits *exactly* — see `ranked_methods`.
- **Recursive `to_inspect`.** roc's unwrap changes the type so the inner `Str.inspect`
  finds no custom method; rocflight erases nominals so it finds the same one forever. The
  `INSPECTING` stack is what ends it.
- **A numeral defaulting to `Dec`** because its variable got tied to a placeholder. The
  checker's `declared_types` exists to stop a use-before-declaration from unifying through
  a shared sentinel.
- **A builtin callback re-entering the VM** nests a Rust frame, so deep `sort_by` inside
  `sort_by` is bounded by the Rust stack with no Roc-level diagnostic.
- **`Range` is not a `List`.** Building one as a list would print `[0, 1, 2]` where roc
  prints `<opaque>` and would wrongly satisfy a `List` parameter.
- **Anything the parser accumulates has to be threaded into sub-parsers.** Each `${...}`
  gets its own `Parser`; three separate bugs came from one inheriting `nominals` but not
  the defaults, or neither.
- **`#[inline(always)]` is load-bearing in two places** (`iter_step`, and the
  leaf/wrapper split in `Lazy::advance`). Left to LLVM, both stayed out of line and cost
  10%.

---

## 11. History, measured

Four plans, all finished. Their full text is in git; this is what each one established.
The point of keeping it is §12.

### Feature parity, phases 1–22 (`tests/check_roc.sh`, `tests/check_examples.sh`)

99 golden pairs across 20 phases: strings, numbers, lambdas, operators, entry points,
`if`, unary minus, records, tuples, `match`, tag unions, pipelines, nominals, lists,
loops, error sugar, generics, real platforms, dispatch, and the langref sweep (phase 21,
15 pairs, `tests/lang_test.rs`). Phase 21 found three forms that were **silently
wrong** rather than missing — digit separators stopped at the `_`, `1.5e3` stopped at the
`e`, `\u(e9)` was literal — and six that were accepted but unchecked, where an unknown
type name became a fresh variable that unified with anything.

Phase 22 ran the language's own 28 examples, which found what no test here would: comments
as trivia everywhere, a top-level expression statement ending the parse, `expect` order,
block-local recursion, an order-dependent top level, an 8 MB stack reservation the
tree-walker still blew at ~200 levels, `concat` hardcoded to `Str`, `byte as char`
mojibake, unescaped `Str.inspect`, and multi-line record/union types stopping at the first
newline.

### Eval parity, phases 0–25 (`tests/check_eval.sh`)

957 → **1,953 of 1,953**, and 72 of 72 problem tests refused. Phases 0–7 took it to 1,736
(integer widths by dispatch module, floats and a real `Dec`, ~60 `Str`/`List`/`Iter`/`Box`
builtins, numerals through nominals, the checker in both directions, parser gaps, dispatch
ranked by shape). Phases 8–17 cleared the self-contained families: `Numeral` and
`from_numeral`/`from_quote`/`from_interpolation`, a lazy `Iter` and ranges over any
numeric type, cross-module dispatch, optional and defaulted fields, `U128` as a real
`u128` with overflow crashes, `Set`/`Dict` structural keys, SHA-256 and BLAKE3, SIMD
vector types.

Phases 18–25 were not features but **shared mechanisms**: nominal identity at run time
(shape-based dispatch already did it, given first-class builtins in tail position, a SIMD
arm on `NominalShape`, and rendering `to_inspect` *inside* the running scope);
parameterized nominals (a nominal's backing already carries its parameters — `Set`'s
signatures just had to be parsed with `Dict`'s declaration in scope); value-level
monomorphization, which turned out to be five independent checker holes rather than one
mechanism; capturing nominal methods; an `Iter` `Builtin.roc` can consume; the JSON codec
protocol; the last checker refusals; and libm bit-exactness.

Rules that held for every phase: a phase is done when its `--filter` is green, not when
its builtins exist; every phase reruns the other gates; and **nothing gets wider than the
tests ask** — `Builtin.roc` declares 1,109 intrinsics and the eval tests reach a few
hundred.

### `Builtin.roc`, phases P0–P6 (`tests/check_builtin.sh`)

`src/roc/Builtin.roc` is a **verbatim copy** of `roc-compiler/src/build/roc/Builtin.roc`:
23,555 lines of Roc, one nominal holding eleven members plus a twelfth slice — the 2,305
trailing top-level declarations (`list_get_unsafe`, `hasher_finish`, `dict_seed`) that the
real compiler injects, and which are **153 of the 226 intrinsics**. A plan that counted
only the members would have under-counted the Rust work by two thirds.

**12 of 12 parse**, after sixteen parser gaps that each verified against the real compiler
first: `()` as an empty parameter list, a file that is nothing but declarations, an
else-less `if` (the absent branch is `{}`, so a bare `if` is a statement), `for (k, v) in`
over a pattern, a guarded `for`, `?` in expression position and as a loop body's last
statement and propagating out of a loop, a bare `..` in a record pattern,
`Ok(encoded) = expr`, trailing commas in a tag payload, a multi-line parameter list as a
record field, `-9223372036854775808` (parsed wide then narrowed), a qualified type name
(`List(Dict.DictBucket)` — and a rejected annotation was only *skipped*, so the signature
was dropped in silence), and a grapheme literal as a pattern.

`Dict` and `Set` are **34 and 30 lines of Roc** with two intrinsics each: implementing
`Dict` was never writing a hash table, it was supplying the `List` and `Hasher` ops
underneath roc's own open-addressing table, which is what actually runs.
`Dict.empty().insert("a", 1)` yields
`HashMap({ entries: [("a", 1)], buckets: [{ dist_and_fingerprint: 467, … }], … })`.

What that cost in Rust: the numeric width family (`shl_wrap`, `bitwise_*`, the
`to_uN_wrap`/`to_iN` conversions, `highest`/`lowest` — the width comes from the MODULE,
so `U32.shl_wrap(1, 8)` is 256 and `U8.shl_wrap(200, 1)` is 144), sixteen `Hasher`
intrinsics over FNV-1a (roc declares the algorithm to be the compiler's business and
requires only that `==` implies the same hash), `Dec` as `i128` scaled by 10^18 — with
multiplication split into whole and fractional parts, because `a * b / SCALE` overflows
past about 13, and the literal kept as WRITTEN because `147.666666666666666666` is exact
as a `Dec` and not as an f64.

`Builtin.roc`'s **annotations are the checker's source of truth** (`signatures_for`), so
`Dict.get` is `Try(v, [KeyNotFound])` because line 5557 says so, not because a match on
the method name guessed it. Declared parameters are unified against the arguments —
reading a result type off the end is not type checking.

Also here: a **numeral defaults to `Dec`**, which alone broke 52 of the 98 pairs and
putting them back was most of the phase; and record-builder syntax (`{ a: pa }.Combiner`
folds the fields through the named type's `map2`).

`Builtin.roc`'s doc comments hold 1,142 ` ```roc ` blocks with 1,926 `expect`s, written by
the Roc authors against the real semantics, and this plan proposed harvesting them into a
corpus. **That never landed**, and roc's own eval suite does the same job better — it is
roc's harness, on roc's tests, with roc's comparison. If anyone revives the idea: one file
per fenced block, not per `expect`, because 18 of them depend on a line three above.

### The platform host, P0–P5 (`tests/check_host.sh`)

`src/platform/real.rs` used to re-implement four of basic-cli's 60 effects in Rust and
call the other 56 an architectural limit. It is not one: `roc --opt=interpreter` calls that
exact host. So **rocflight links itself into the platform's own host** instead —
`host/` is a `staticlib` exporting `roc_main`, the driver writes the platform's `hosted`
table as a C file, `zig`'s lld links it per the platform's own `targets:` recipe, and the
result is exec'd with `ROCFLIGHT_APP` pointing at the source. Cold link 106 ms, warm run
8 ms; the executable is cached **per platform**, so editing a `.roc` file never re-links.

`libhost.a` cannot be called from a running glibc process — it is a static musl archive,
an archive cannot be `dlopen`ed, and re-linking it into a `.so` would put musl's std
inside a glibc process. `roc` does not try either. The contract is small: the host defines
`main`, `roc_alloc`/`roc_dealloc`/`roc_realloc`, `roc_dbg`, `roc_expect_failed`,
`roc_crashed` and the 60 `hosted_*` functions, and imports **exactly one** symbol,
`roc_main(args: RocList<OsStr>) -> i32`. `dbg`, a failed inline `expect` and `crash` route
to the host, which owns the process.

No assembly trampoline was needed, unlike roc's: on x86-64 SysV anything over 16 bytes is
passed by pointer and returned through a hidden `sret` pointer, and every `Str`, `List`,
record and payload-carrying union is over 16 bytes, so all 60 signatures collapse onto a
handful of `extern "C"` shapes. `tests/platform_test.rs` classifies all 60 and fails the
build if one does not fit. `ponytail:` x86-64 SysV only; aarch64 needs a second
classifier, and floats in SSE registers are rejected explicitly rather than mis-passed.

**Snake runs on basic-cli's real host and matches `roc` byte for byte**, and the examples
gate replays a whole game key by key — which is how two checker bugs were found (a type
named before its declaration resolving through a shared placeholder, and annotated
top-level names not bound before bodies are checked).

### Optimization, phases 0–8

Every phase done, gated on 1,953 of 1,953 throughout. A four-line `Dict` program went
**3.5ms → 1.14ms** in process; the benchmark suite fell by 2.7–27.5% in phase 7 alone and
1.4–20.8% in phase 8, with nothing regressing in either.

- **Phase 0 — measure.** No sampler works on this machine (`perf` absent, `gprofng`
  recorded ~10% of samples, no valgrind), so `ROCFLIGHT_TIME=1` and `ROCFLIGHT_CODE=1`
  are the instruments. They corrected three of this plan's own claims, which is why they
  landed before any change.
- **Phase 1 — stop re-deriving constants.** The member index and the low-level
  reachability closure are functions of a constant and moved to `build.rs` (−0.76ms,
  −0.67ms). `signatures_for` is seeded from what `load` already parsed, for every member
  but `Set` — `Set(item) :: Dict(item, {})`, so its signatures only carry the element type
  if `Dict`'s declaration was in scope. That also turned one PENDING example into a pass,
  because a full parse infers what an annotations-only one cannot.
- **Phase 2 — `Builtin.roc` parsed AND compiled at build time.** `builtin::load`
  1.821ms → 0.235ms, `compile` 0.604ms → 0.088ms. What made it fast is not the binary
  format: **every name is borrowed from the blob**, so reading one is a slice. Opening the
  artifact decodes no trees (each body carries a fixed-width length the header steps
  over), and node identity is a *rebase* — a member stores offsets relative to its own
  first node. The bytecode is provably program-independent: four programs' builtin chunks
  are byte-identical once chunk numbers are normalised, so `compile_unit` numbers the
  precompiled group first and everything else after it. The `Op` codec is generated from
  one table by a macro, because a codec is exactly where a writer and a reader drift, and
  a drift there decodes as a *different program* rather than as an error.
- **Phase 3 — the parser.** Parse time is linear in input size, and the wins were not the
  lexer. An annotation with a real type costs **six times** a bare one
  (`f0 : I64, Str -> Try(List(U64), [Bad(Str)])` at 3.00µs/line against 0.48µs), and
  `Builtin.roc` is a file of annotations, so the cost was `parse_type`'s allocations — 48
  heap allocations for one type. Removed: a deep copy of the nominal table **per string
  literal** (−86% where it bites, quadratic in literals × nominals, invisible in any
  typical profile), a global-mutex SipHash string pool (→ thread-local FNV-1a, −4% to −6%
  everywhere), a 400-character `String` per annotation line, `builtin_type` taking
  arguments by value, `claim_intrinsics` deep-cloning a `Type` into a list nothing reads
  it from, and `types::Type`'s names becoming `&'static str` (189 sites, −39% on
  annotation-heavy parsing). The tokenizer is still unwritten and is no longer obviously
  next; measure it against the annotation-heavy case first.
- **Phase 4 — VM per-op cost**, ~15ns. The stated 3× was never a missing `BinInt`: both
  loops emit it, and the difference was a `Dec` range walked as a **lazy iterator** —
  six operator dispatches and a 168-byte `Rc` per element. `Lazy::step` takes its `Rc` by
  value and advances in place through `Rc::make_mut` (copy-on-write, so a caller holding
  its own handle still observes nothing), and the wrapping iterators advance through one
  mutable borrow instead of rebuilding themselves; `iter_range` 316ms → 222ms, chains
  −20%. Then `Move` turned out to be the **most executed opcode** (25% of `matching` and
  `records`): `Compiler::discard` stops materialising a discarded `{}`, and
  `wrote_directly` patches the producing instruction to write the destination — sound
  only on a straight-line run (a branch's other arm would be left writing nothing) and
  only for a temporary above the `next_reg` watermark. `loop` −26%, `matching` −20%. And
  the compiler's top level was **quadratic** in declarations (8,000 = 32 million string
  comparisons, 94ms → 2.2ms with three `HashMap`s built once).
- **Phase 5 — lower the callback builtins.** `any`, `all`, `count_if`, `find_first`,
  `find_first_index`, `find_last_index`, `fold_with_index` and `fold_try` compile into
  in-frame loops (−36% to −54% each); `count_if` had to be *implemented* first, because a
  method should not exist only in the compiler. What makes them safe to lower is that each
  is declared on `List` alone and answers a plain **value**, so the loop can stand in
  whatever the checker believes about the receiver. The new opcode is `TestBool`,
  deliberately not `JumpFalse`, which errors on a non-`Bool` — compiling to it would have
  invented an error the interpreter does not have.
- **Phase 6 — startup, which was never rocflight's.** ~200µs of it is the dynamic loader;
  `-C target-feature=+crt-static` (x86_64 Linux only) plus `strip = true` took
  `--version` to two microseconds over `/bin/true`, and every short benchmark −4% to −13%.
  `thiserror` had to go first: a proc macro cannot be built for a statically linked
  target, and it was buying three `Display` impls. Fat LTO measured and kept —
  `lto = "thin"` builds in 8s instead of 21s and loses on ten of twelve benchmarks.
- **Phase 7 — the allocations.** `Record` and `Tuple` went behind an `Rc` (31 compile
  errors, all *construction* sites, because `Rc` derefs through for reads): a record and a
  tuple are now **flat in their width**, 16 fields costing what 2 do, where each field used
  to add ~10ns per pass. Tag payloads are `Rc<[Value]>` — one allocation, and none for a
  bare tag — but `Rc::from(a_vec)` does *not* save it, so 113 call sites became arrays.
  `DispatchMethod` was collecting the register window into a `Vec`, `remove(0)`-ing the
  receiver off the front and building a second `Vec` to put it back; the dispatch layer
  takes `&mut [Value]` now, which **is** the window. And `src/vm/liveness.rs` proves the
  last read of a value so `Move` becomes `MoveTake` and `UpdateRecord` mutates in place:
  `records` 80,720 allocations → 828. Its soundness rule is one-directional — reads may be
  over-approximated and kills under-approximated, never the reverse — because a *missed*
  read empties a register something still needs and answers wrongly with no crash.
  `TailCall` is the one range kill, and without it `records_tail` got no takes and read
  +6.9% from code layout alone.
- **Phase 8 — instructions.** A `for` loop's back edge does the step itself
  (`IterNextBack`, rewritten in place so nothing is renumbered, and only when all three
  facts — back edge, target is an `IterNext`, `exit == jump + 1` — hold). A literal operand
  is read from the constant table (`BinK`/`BinIntK`), folded only when the previous
  instruction is a `LoadK` into a **temporary**, or `y = 5` then `x + y` would delete the
  binding. And phase 4.3's destination hint reached calls (safe exactly when
  `dst < arg_base`) and the lowered loops. `loop.roc` and `iter_range` are two instructions
  per iteration now; there is no third to remove.

**Where the time goes now:** `parse 0.077ms | builtin::load 0.339ms | type check 0.058ms
| compile 0.655ms | run 0.052ms`. `compile` is the largest number left and it is the same
question in a new place — every definition `Builtin.roc` declares is compiled whether the
program calls it or not.

---

## 12. Measured and rejected

Do not re-open these without new evidence. Each was built or priced, and the number is why
it is not here.

| Idea | Verdict |
|---|---|
| `BinDec`/`BinDecK` opcodes | Works, net loss: `iter_range` −5.6% but `records` +3.3% and `records_tail` +2.7%, and those execute **not one** `Dec` operation. Two more arms move the dispatch loop's layout. Ask again only if `exec` stops being layout-sensitive. |
| An inline cache for `DispatchMethod` | The lookup is not the cost. Removing it *outright* for the programs where it is 20% of instructions won nothing (`records` 6.5% **worse**), and an inline cache cannot beat zero. |
| `GetField` by slot | Making the name lookup free changed nothing, including on `records`. The 20% is an instruction *count*; what it costs is dispatch, bounds checks and a `Value` clone. Retired twice. |
| Shrinking `Value` to 32 bytes | Three variants force 48 independently. Getting to 32 means boxing `Range` (an allocation per loop, which phase 4.1 spent its effort removing) and undoing the `Rc<[Value]>` tag payload. No version of this wins. |
| `Parser::input: String` → `&'a str` | The copy is one ~20kB memcpy, ~0.7µs, against 646µs of parsing the same file. 0.1%, for a lifetime through 134 call sites. |
| `Box<Expr>` → `Rc<Expr>` | Nothing clones the AST — one site in the whole crate. `Rc` pays only where something is shared. The AST's real cost is the *number* of small allocations, which is an arena question. |
| `Box<Type>` → `Rc<Type>` in the checker | Looks compelling (135 clone sites), but the whole type check is now 0.048ms on a trivial program. A 4,277-line refactor for tidiness. |
| An arena for AST nodes | Not attempted, and the indirect evidence is weak. Stubbing `fresh_node` out to bound it fails, because node identity is load-bearing for annotations and nominal literals. Measure properly first. |
| An inline small-string for `Value::Str` | `Rc<str>` already makes the clone free; only construction allocates. It is a second representation threaded through every `Str.*` builtin. Measure `strings.roc` against a counting allocator first. |
| Guarding `skip_trivia` on start-of-line | −4.2% on a `Dict` program, and **1881 of 1953** eval tests. Annotations reach it mid-line. `check_roc.sh` and `check_examples.sh` both stayed green. |
| Narrowing `needed_by` to whole words | Changes the answer for **one** file in the repo, worth 0.35ms once, and 1949 of 1953. Precise narrowing needs the parsed AST. |
| Stripping comments before the reachability closure | Changes nothing: every name mentioned in a `Builtin.roc` comment is also used in code. |
| Lowering `keep_if`/`drop_if` into loops | 414ms → 153ms, and reverted. `Builtin.roc` declares both `List.keep_if -> List(a)` and `Iter.keep_if -> Iter(a)`; a compiled loop may only stand in for the List one, and there is no sound guard (`dispatch_modules` calls `(1..=5).iter()` a `List`). Trading a correct `Iter` case for a correct `List` case is not progress. |
| Loading `Builtin.roc`'s members wholesale | 923 eval tests instead of 957 — their bodies shadow working Rust builtins and reach intrinsics that do not exist yet. |
| Testing eval cases through a module's `main` | roc folds it at compile time before any backend runs. The runner's expression tests are what the backends execute. |
| Varint for the artifact's bytecode section | 0.146ms → 0.136ms. The cost is materialising 99 chunks' `Vec`s, not decoding them, and without `unsafe` that is the floor. |
| A `dst == obj` fast path for `UpdateRecord` | Dead code: across the whole suite and all 99 pairs the compiler never emits one. Making `make_mut` fire is ownership analysis, not a fast path. |

**Deliberately not doing**, for reasons that are not measurements:

- **A JIT.** The measured problem was a front end re-parsing a constant, not a slow inner
  loop.
- **`unsafe`.** `#![forbid(unsafe_code)]` is load-bearing: a wrong opcode is a message,
  not memory corruption.
- **Threads.** A run is a pipeline with a shared string pool, a thread-local node table
  and `RUNNING`. The eval harness already runs 12 processes.
- **A daemon or server mode for the eval harness.** It would be the single largest number
  available, because it amortizes the front end across 1,953 tests — and it would be a
  lie: the harness is the gate precisely because it runs rocflight the way a user does.
- **Caching the user's own parse between runs.** Editing a `.roc` file must take effect
  immediately.
- **Other platform targets.** `x64mac` links against `libSystem`; `x64win` needs a COFF
  linker and 15 import libraries. Same plan, different recipe from the `targets:` block.
- **Embedding the app in the linked executable**, as `roc` does. The app path travels in
  an environment variable, which is what keeps the edit-run loop free.

---

## 13. The platform ABI

Reference for `src/platform/layout.rs` and `marshal.rs`. Sizes are 64-bit. Sources:
`roc-compiler/src/builtins/{str,list}.zig`, `src/layout/{store,field_order}.zig`, and
basic-cli's generated `src/roc_platform_abi.rs`. The **declared** signature in `Host.roc`
drives every layout — never the runtime shape of a `Value`.

| Type | Layout |
|---|---|
| `Str` | 24 B: `bytes: *u8`, `capacity_or_alloc_ptr: usize`, `length: usize` — **in that order**. Small string: the high bit of the *last byte* is set, its low 7 bits are the length, the 23 preceding bytes hold the text. Big string: `capacity` stored shifted left by one; low bit set means seamless slice. |
| `List(a)` | 24 B: `bytes: *a`, `length: usize`, `capacity_or_alloc_ptr: usize` — **a different order from `Str`**. |
| Refcount | An `isize` immediately **before** the allocation `bytes` points at. `1` = uniquely owned; `0` = static, never freed. Allocate with the host's `roc_alloc(len, align)`, free with `roc_dealloc(ptr, align)`. |
| Records | Fields sorted by **descending alignment, then alphabetical name**. Source order never reaches memory. |
| Tag unions | Tags sorted **alphabetically**; the discriminant index is the sorted position. Payload first (a union of the sorted payloads), discriminant **after** it, width 0/1/2/4/8 bytes by tag count, whole thing padded to its alignment. |
| `Try(ok, err)` | Is `[Ok(ok), Err(err)]`, so **`Err` = 0, `Ok` = 1**. |
| `OsStr` | `[Utf8(Str), UnixBytes(List(U8)), WindowsU16s(List(U16))]` → sorted `UnixBytes`=0, `Utf8`=1, `WindowsU16s`=2; 24 B payload + discriminant, padded to **32 B**. |
| `Box(a)` | A pointer (8 B), refcounted like a list. `FileReader`, `SqliteStmt`, `TcpStream` are `Box(U64)` — opaque handles, passed through untouched. |
| Numbers | `I32`/`U64`/`I128`/`F64`/`Dec` at their C sizes; `Bool` is a byte; `{}` is zero-sized. |

**Ownership:** hosted functions **take** their arguments and **give** their results.
rocflight allocates fresh args through `roc_alloc`, never reuses one, converts each result
to a `Value`, then `roc_dealloc`s what the result owned. The gate is a round-trip that
leaves the host's allocator balanced (`FakeHeap` counts, in process).

A platform ships: public modules as ordinary Roc; `Host.roc`, annotation-only members with
ABI-safe types; a `hosted { "hosted_stdin_bytes": Host.stdin_bytes!, … }` symbol map in
dispatch order; `provides { "roc_main": main_for_host! }`; a per-target link recipe
(`targets: { x64musl: { inputs: ["crt1.o", "libhost.a", "libunwind.a", app, "libc.a"] } }`,
in link order, where `app` is the slot rocflight fills); and `targets/x64musl/libhost.a`.
Its `requires` signature contains `{}`, so reading it needs brace matching. An exposed
module declares its members inside `Name :: [].{ ... }` and **anything after that block is
private** — `Stdout.roc` defines `widen_stdout_err` there.

`roc` already downloads, verifies and extracts dependencies, so none of that is
reimplemented: rocflight reads `~/.cache/roc/packages/<HASH>/`, where `<HASH>` is the
URL's filename minus its archive extension. Both cache layouts and both `.tar.zst`/older
extensions are recognised. In the app header, `alias: platform "URL"` differs from
`alias: "URL"`, and `roc: "nightly-..."` pins the compiler rather than naming an archive.

---

## 14. Known ceilings

The current list, with the reasoning, is in `README.md`. When you hit a new one, add it
there in the same breath as the workaround. A ceiling in the README is a maintenance note;
a ceiling only in someone's head is a bug report waiting to be filed three phases later.

Eleven of basic-cli's 18 modules load. The rest are **language gaps**, not host gaps, and
they are what the pending examples are blocked on: `Path` (and `Cmd`, `Env`, `File`,
`Sqlite` through it) needs a string literal to become a nominal via `from_str`, `Locale` a
`{ raw: Str }` nominal literal, `Sleep` `seconds * 1000` on an `F64`, `Http` a package
import. Each shows as PENDING with its real error.
