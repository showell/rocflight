# Testing policy for our rocflight work (draft, 2026-09-28)

Nobody runs rocflight in production, so a regression costs us a fix-forward, not an outage. The gates exist to keep three promises, in this order:

1. roc and rocflight agree on the golden pairs and the examples.
2. Fast Track on the interpreter produces the same output, in about the same time.
3. ~~roc2rust and roc2codex keep working.~~ Deprecated; see "Codex emission" below.

Running every gate on every fix takes 20 minutes or more, and that's what slows us down. The policy below spends that time once per **batch**, not once per fix.

## The tiers

| Tier | What | Wall time | When |
|---|---|---|---|
| 0 | The new test, plus that test file's suite (`cargo test --test types_test`), in debug | seconds | Every edit |
| 1 | `cargo test`, and `check_roc.sh` on the affected directory | 1–3 min | Every commit |
| 2 | The all-prs gates: cargo test, check_roc, check_examples, check_artifact, the ported corpus (581), and Fast Track on the interpreter with timings | ~7 min | Once per batch, before anything goes upstream |
| 3 | ~~The codex-emit gates~~ | | **Deprecated: not run** |

- **A batch** is several fixes stacked on their PR branches and merged into `all-prs`. Tier 2 runs once over the whole batch. If it's red, bisect within the batch. The fixes in a batch are small and on separate commits, so that's cheap.
- **Tier 2 replaces the per-PR-branch gates.** `all-prs` is a superset of each PR branch, so gating each branch separately repeats the same suites. Gate a single PR branch on its own only when its merge into `all-prs` needed a conflict resolution, because then the two versions differ in more than the merge.

## Codex emission: deprecated (Steve, 2026-09-29)

roc2rust and roc2codex are deprecated for now. Their code stays in place on `codex-emit`; nothing is removed. But their gates don't run: Tier 3 is off, and a batch is done when Tier 2 is green. `codex-emit` stops following `all-prs`, and it isn't merged or pushed with a batch. Its last gated state is `631816f` (2026-09-28): roc2rust 581/581, and Fast Track's generated Rust matching roc 09-27. If the translators come back, start by merging `all-prs` into `codex-emit` and running the old Tier 3 there.

## What we don't cut

- **Test first:** every fix lands with a test that failed before it.
- **Fast Track timings** (Tier 2) run for any batch that touches the VM, the runtime, or the checker's cost, since performance is objective 1. A timing more than 10% off is re-timed on a quiet box before anyone believes it.
- **Nothing reaches B-Teague** (PR pushes, PR bodies, issue comments) without Tier 2 green on the commits it carries.
- **The pin:** gates run under `nightly-2026-09-27-a3ce7f1`. A second nightly is used only to tell whether a change in roc explains a result.
- **Moving the pin, or re-syncing `Builtin.roc`, gets Tiers 0–2 in full,** whatever the batch's usual scope. The change reaches every program.

## Tracking what's outbound

Cutting corners is safe only while we know which gates each outbound commit passed. `OUTBOUND.md` on the fork's `main` gets one line per PR or issue: the head commit, the gate tier it passed, and the date. Update the line whenever the PR's head moves. Anything pushed at Tier 1 only is marked **ungated** there until a batch's Tier 2 covers it.

## The Cloud Claude

The Cloud Claude runs Tiers 0–1, plus check_roc and check_examples under the pin, on its `cloud/<topic>` branches. The corpus and Fast Track need this box, so its work gets them when it's integrated into a batch here.
