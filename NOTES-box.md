# Notes from Steve's Claude on the box: standing state, policy and method

This file holds what Steve's Claude on the box (the droplet where the gates run) knows about
this fork, so a new session, or Claude in the Cloud, can pick the work up. It replaces that
Claude's private memory of the project (moved here 2026-10-01). Day-to-day conversation with
the Cloud Claude stays in `NOTES-<date>.md`; `OUTBOUND.md` tracks every PR and issue head;
`TESTING.md` defines the tiers.

**State as of 2026-10-01: parked.** Steve has switched to other work. Everything is pushed and
nothing is held (see `OUTBOUND.md`). The next step, when work resumes, is under "Open".

## The project

- rocflight is Brian Teague's Roc interpreter in Rust (`B-Teague/rocflight`). This fork is
  `showell/rocflight`. Remotes on the box: `fork` = showell, `origin` = B-Teague.
- Brian's Claude reads our PRs and issues. Each opens with "Hi Brian. This is Steve's Claude
  writing to (presumably) your Claude." and ends with "Reported by Claude (Anthropic's Claude
  Code), working with @showell" plus the Claude Code line.
- **Before anything goes upstream, a cold review:** a fresh agent, told to act as Brian's
  reviewing agent, reads the PR. Every cold review on 09-26 found a real bug, including a
  regression. Send a fix we are only ~75% sure of hedged, or as an issue instead.
- Stack dependent fixes on one PR. Checker fixes stack on #25 (Steve's instruction).
- `gh pr edit` fails on GitHub's Projects-classic deprecation. Update a PR body with
  `gh api -X PATCH repos/B-Teague/rocflight/pulls/N --input body.json`, where `body.json` is
  `{"body": ...}`.

## Collaboration with Claude in the Cloud

- The Cloud Claude (CC) is a collaborator. It works on `cloud/<topic>` branches; the box Claude
  owns `src/types/` and the parser, integrates, runs the gates and sends PRs.
- The channel is `NOTES-<date>.md` on fork main, numbered replies appended. When a reply holds
  action items for the other side, Steve relays "tell Claude there are action items".
- CC's commits keep their authorship; integrate with `cherry-pick -x`.
- **Never rewrite `395da5f`** (the 09-27 pin commit): CC's branches are based on it.
- CC pushes to Steve's repos, so always fetch before building on a branch.

## Branches and checkouts

- `all-prs` = Brian's main + every open PR branch merged exactly as sent. Rebuild it when a PR
  changes. New fixes start from it.
- `node-types` = PR #25.
- `codex-emit` = `all-prs` + a listed stack of fork-only commits (node-type recording,
  roc2codex/roc2rust, #13 partial, #14 Try typing; the list is in `codex/README.md`).
  **Frozen at `631816f`** (see "Codex emission" below). `codex-emit-2026-09-25` is the
  pre-restructure branch.
- On the box: `~/showell_repos/rocflight` is the only checkout. Worktrees are made per job
  (`git worktree add`) and removed once pushed. Build targets live in
  `~/build/rust-target-<name>`, one per worktree, so incremental builds don't collide.
- **Merging PR branches:** test files conflict on every append (keep both sides; a script
  that resolves union conflicts must also handle `|||||||` diff3 markers). `Builtin.artifact`
  is regenerated with `cargo run --release --bin gen-artifact`, never taken from a side.
  Artifact conflicts in `build.rs`/`artifact.rs`: resolve only the conflicting hunks.
  `git checkout --theirs` takes the whole file and drops the other side's tables.
- **Artifact magic per branch** (as of 09-29): main and the 09-27 stack 05, list-copies 06,
  node-types 08, codex-emit and cloud/num-runtime-sweep 09, all-prs / cloud/h2 / #34 10.

## Policy (Steve)

- **No regressions, ever.** A fix that turns any right case wrong does not ship, however much
  else it fixes (09-28, on the withdrawn lead-1 fix below).
- **Types survive past the parser.** When a later stage meets a type rocflight got wrong or
  lost, fix the checker or parser here (a test that fails without the fix, then the fix,
  stacked on #25). Never accept approximate types and patch around them downstream; remove
  downstream workarounds once the checker fix lands. Don't be shy sending checker fixes
  upstream.
- **Fork-only changes each get a decision:** sidestep them in our own Roc when the workaround
  is also cleaner Roc (annotations especially); otherwise send them upstream if sound; keep
  them only when neither works, with the reason written down. Measure reliance by running
  everything on `all-prs` alone.
- **rocflight may be stricter than roc.** On dubious practice (shadowing, a name that is both
  a namespace member and a top-level name) it should complain rather than mimic semantics
  still in flux (roc is pre-0.10).
- Do not close open tag unions by value in the checker (`[Solo, ..]` is equal across unrelated
  contexts).
- Be bold with core fixes; be conservative only when a change touches 100+ sites.

## The roc pin

- **The pin is nightly-2026-09-27-a3ce7f1** (moved 09-28, PR #27). Verify "roc prints X" with
  09-27 and say so. Branch gates for PRs based on Brian's main still run 09-07.
- 09-27 breaks 09-07 for SafeMath (implicit opening came after 09-07). PR #27's body corrects
  `395da5f`'s "19/0 on both" claim.
- The vendored `Builtin.roc` was re-synced to a3ce7f1 (PR #28). A pin or Builtin move gets
  every tier.
- **`to_inspect` since nightly 09-23** (roc #11573/#11588): an override counts only if its type
  is exactly `T -> Str`. Unannotated, it is silently ignored. Filed roc-lang/roc#11808 and
  roc-lang/examples#296; rocflight still applies every `to_inspect`, and the vendored examples
  are annotated.

## Gates

- Tiers are in `TESTING.md`. Tier 1 is `cargo test --release --no-fail-fast` (plain
  `cargo test` stops at the first failing test binary, which is how a regression was missed on
  09-29). Also run `cargo test` in debug: release once hid an overflow.
- Then `cargo build --release` (the test build leaves a stale binary), `check_roc`,
  `check_examples`, `check_artifact`, and on `all-prs` the ported corpus (`codex/ported.sh`) and
  Fast Track.
- **Baselines (09-29):** `all-prs` `2c46f6d` under 09-27: check_roc 98/1 (the 1 is basic-cli's
  own warnings), check_examples 19/0, ported 581/581, Fast Track identical to roc.
  `node-types` under 09-07: 97/2 (basic_cli.roc and ranges.roc fail at roc's own check) and
  19/0. Roc-apps' ported set grows, so a new failing test is not automatically a regression:
  check `git log --diff-filter=A`.
- **A fix gets a test proven to fail without it** (swap the old file in with
  `git checkout <rev> -- f`, never stash/pop).
- **Performance is gated** (Steve, 09-26). The gate script appends each Fast Track run's
  seconds and peak KB to `~/build/rocflight/ft_timings.tsv`. A single gate run is noise on the
  box (rf_table has read 20.8 to 24.8 s at the same speed); judge a change by interleaved A/B
  runs of two binaries, min and median. Baseline 09-26: rf_table ~22 s, rf_roll ~10 s, rf_dup
  ~6 s, rf_search ~2 s, rf_cards5 ~1.5 s, 15-23 MB peak. Brian's `tests/bench.sh` programs run
  4-30 ms (±20% noise); only `iter_range` (~225 ms) is long enough to read.
- Fast Track method: always try one game first (a scratch copy with the count lowered).

## Codex emission (deprecated 2026-09-29)

roc2codex and roc2rust stay on `codex-emit`, frozen at `631816f` (the last gated commit). Tier 3
does not run and `codex-emit` is not merged with batches. Do not delete the code.

What it established, for the record:
- roc2rust translated all 581 ported programs, and Fast Track's five experiments print what roc
  prints. On musl (roc's allocator) the Rust ran 25-35% faster than roc, with less peak memory
  in four of five experiments. It still made ~1.8× roc's allocation count (a list is `Rc` +
  `Vec`), which no longer cost time. Essay: `notes/roc2rust-on-par.md` on the essay server.
- Single-allocation lists were tried three ways and rejected (hand-written unsafe; `ecow`, whose
  atomic count was slower; a safe `Rc<[T]>` hybrid, slower and bigger). Steve: no unsafe, no
  dependency, speed is a dealbreaker; competitive, not roc's exact layout.
- roc2rust's runtime (`src/rust/runtime.rs`) is embedded as text, so building roc2rust never
  compiles it. After any runtime edit run `codex/rust.sh --tests` first.
- Memory is judged by peak live bytes, not RSS.

## Open, in order

1. **Slices (CC).** The design (NOTES 18) is approved in NOTES (20) with one change: on a unique
   list, `drop_last`/`take_first` release the dropped suffix at once, as roc's `listSublist`
   does (`src/builtins/list.zig` ~1593, roc main `b3c0f02`). Invariant: on a unique list,
   nothing outside the window is alive. CC does steps 1-2 on `cloud/slices`, then stops; the box
   judges step 2 on Fast Track with an interleaved A/B before steps 3-5.
2. **Lead 1: double literal conversion** (`from_numeral` runs 10× per literal: 6000 calls vs
   roc's 60). Marking producers was withdrawn: it regressed `List.map(xs, seven)` (kept as local
   branch `lead1-producer-marks`, `4277e05`). The real fix is specialization: compile a generic
   function whose literal reaches a converting nominal once per nominal, transitively through
   the call graph, keyed on instantiation sites including bare references. About a day. Steve
   deferred it; don't start unasked.
3. **Let-generalization leniency:** a block's `red = Red` is generalized (rows too), so it
   passes as both `[Red, Green]` and `[Red, Blue]`; roc rejects. Candidate PR.
4. **Anonymous `..` is flexible in its own body** (roc: rigid): `f : Str -> [Red, ..]` with
   `f = |_| Blue` is accepted. Candidate PR.
5. **Typed-inspect leftovers** (listed in #25): `rocflight eval`'s top-level print,
   `Str.inspect` as a function value, builtins' own `to_inspect` bodies (Dict shows via Set's
   `to_list`: a dispatch-by-shape bug), imported modules' opaque nominals.
6. **Checker gaps, low priority:** value annotations are not opened (rocflight is stricter than
   roc here, and on nominal-argument variance); `where` per variable (static dispatch step 2);
   `Dict(Str, I64)` annotations unchecked; #13 re-check; `with_rows` renumbering is redundant.
7. `check_roc`'s two 09-07 failures are Brian's test files (basic_cli.roc pins nightly-09-03;
   ranges.roc writes `var total`, roc wants `$total`). One-line fixes; the `$total` one is in
   PR #27.
8. The interpreter on the real cli platform needs Rust's musl `libunwind.a` linked after the
   host lib (ship decision pending).

## Reference: how roc's checker does static dispatch

Researched 2026-09-26 from roc source @ `d6312ed7` (2026-09-16); probes on nightly 14d9829
agree. This is the spec for rocflight's version.

- **A method call on an unknown receiver is a constraint, never a name-based guess:**
  `StaticDispatchConstraint {fn_name, fn_var, origin}` lives on the flex var
  (types/types.zig:1066, 303). `fn_var = fn(receiver, args..) -> result` (Check.zig:24085,
  `mkReceiverDispatchConstraint` 24107). Origins: method_call, desugared_binop (`x * 2` is
  `times`), unaryop, where_clause, from_literal (numerals are constraints too).
- **Order:** receiver already concrete: resolve the method first and check args against its
  params (lambda params seeded). Otherwise check args with fresh vars, then build the
  constraint (Check.zig:20029/20094).
- **Unify:** flex+flex merges constraint lists (same name: unify the fn_vars, unify.zig:3488);
  flex+concrete defers the check to a queue (unify.zig:555), drained by
  `checkStaticDispatchConstraints` (Check.zig:31296). Nominal: look up its method block,
  instantiate, unify with fn_var (typing the lambda arg and result); absent = missing method.
  Rigid (annotation): must be in its `where` list. Record/tuple/tag union: only derived methods
  (is_eq, hash, codecs, `map` on a tag union). Still flex: requeue.
- **Generalize:** constraints ride on the generalized var, which is the inferred `where`
  clause; instantiate copies them with fresh fn_vars (instantiate.zig:636).
  `double_all = |it| it.map(|x| x * 2)` :
  `a -> b where [a.map : a, (c -> c) -> b, c.times : c, d -> c, d.from_numeral : ..]`.
- **Binops:** `times` returns the receiver type; operands are not unified (a user
  `times : Duration, I64 -> Duration` works), except eagerly when the left is a builtin number.
- **Unpinned method constraints are errors, never defaulted** (`unresolved_dispatcher`,
  Check.zig:11206, judged at generalization 10930 if not reachable from the scheme's args,
  lambda params or literals). Only literal constraints default (Dec/Str).
- **Errors point at the constraint's origin inside the generic body**, not the caller.
- **Codegen:** a per-site `StaticDispatchCallPlan` (direct / evidence_dependent / structural),
  monomorphized, so roc never dispatches at run time. rocflight dispatches on the runtime
  value instead, so it needs the types right, not a new codegen.
