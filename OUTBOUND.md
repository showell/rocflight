# Outbound

What we have out upstream, and which gates each item's current head passed (tiers as in `TESTING.md`). Update a line whenever a PR's head moves. A line marked **ungated** has no Tier 2 run covering its head yet.

`all-prs` is `main` plus every PR below, plus the cloud branches not yet sent. `codex-emit` is `all-prs` plus our translators.

## PRs to B-Teague/rocflight

| PR | Branch | Head | Gates | Date | Merge after |
|---|---|---|---|---|---|
| #1 | `module-imports` | `0dfe7fb` | Tier 2 in `all-prs` `46ecb76` | 2026-09-28 | |
| #22 | `fast-track-runs` | `484cf04` | Tier 2 in `all-prs` `46ecb76` | 2026-09-28 | |
| #23 | `smaller-fixes` | `d5c4fee` | Tier 2 in `all-prs` `46ecb76` | 2026-09-28 | |
| #25 | `node-types` | `44a2403` | Tier 2 in `all-prs` `46ecb76`; Tier 3 in `codex-emit` `631816f` | 2026-09-28 | #23 |
| #27 | `nightly-09-27` | `92dfe53` | branch gates under 09-27; Tier 2 in `all-prs` `46ecb76` | 2026-09-28 | |
| #28 | `builtin-09-27` | `222afb5` | branch gates and `check_builtin --strict` under 09-27; Tier 2 in `all-prs` `46ecb76` | 2026-09-29 | #27 |
| #29 | `json-numbers` | `a3f7d09` | branch gates under 09-27; Tier 2 in `all-prs` `46ecb76` | 2026-09-29 | #28 |
| #30 | `float-display` | `9ae8115` | branch gates under 09-27; Tier 2 in `all-prs` `46ecb76` | 2026-09-29 | #27 |
| #31 | `list-copies` | `a824e7b` | branch gates under 09-27; Tier 2 in `all-prs` `46ecb76` | 2026-09-29 | #27 |
| #32 | `issue-6-list-copy` | `42c53ff` | branch gates under 09-27; Tier 2 in `all-prs` `46ecb76` | 2026-09-29 | #27 |

The work in #28 to #32 is by the cloud Claude, cherry-picked with `-x`.

## Held here, not yet sent

- **`node-types` `15a89d3`:** a hex, octal or binary literal takes its type suffix. Tier 1 only, so it's **ungated**. It goes to #25 with the next batch's Tier 2.
- **`cloud/num-runtime-sweep` `219250b`:** the numeric runtime sweep. It's for the next batch, then a PR stacked on #28.

## Issues filed

| Where | # | What | State |
|---|---|---|---|
| B-Teague/rocflight | 4, 6, 9, 11, 13, 14, 16, 17, 24, 26 | as titled | open; #9 and #24 are fixed in #25, and #6 in #32 |
| roc-lang/roc | 11662 | `roc glue` fails on 09-23 | open |
| roc-lang/roc | 11801 | an unannotated generic with method calls costs O(body²) in inference | open |
| roc-lang/roc | 11806 | segfault: an effectful call with an unresolved argument inside `for` | fixed by roc-lang/roc#11813, closed 2026-09-29 |
| roc-lang/roc | 11807 | hex floats with more than 16 significant digits round wrongly | open |
| roc-lang/roc | 11808 | an unannotated `to_inspect` is silently ignored | open |
| roc-lang/examples | 296 | annotate `to_inspect` in EncodeDecode and CustomInspect | merged 2026-09-28 |
