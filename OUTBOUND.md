# Outbound

What we have out upstream, and which gates each item's current head passed (tiers as in `TESTING.md`). Update a line whenever a PR's head moves. A line marked **ungated** has no Tier 2 run covering its head yet.

`all-prs` is `main` plus every PR below, plus the cloud branches not yet sent. `codex-emit` is `all-prs` plus our translators.

## PRs to B-Teague/rocflight

| PR | Branch | Head | Gates | Date | Merge after |
|---|---|---|---|---|---|
| #1 | `module-imports` | `0dfe7fb` | Tier 2 in `all-prs` `46ecb76` | 2026-09-28 | |
| #22 | `fast-track-runs` | `484cf04` | Tier 2 in `all-prs` `46ecb76` | 2026-09-28 | |
| #23 | `smaller-fixes` | `d5c4fee` | Tier 2 in `all-prs` `46ecb76` | 2026-09-28 | |
| #25 | `node-types` | `9172c59` | branch gates under 09-07; Tier 2 in `all-prs` `2c46f6d` | 2026-09-29 | #23 |
| #27 | `nightly-09-27` | `92dfe53` | branch gates under 09-27; Tier 2 in `all-prs` `46ecb76` | 2026-09-28 | |
| #28 | `builtin-09-27` | `222afb5` | branch gates and `check_builtin --strict` under 09-27; Tier 2 in `all-prs` `46ecb76` | 2026-09-29 | #27 |
| #29 | `json-numbers` | `a3f7d09` | branch gates under 09-27; Tier 2 in `all-prs` `46ecb76` | 2026-09-29 | #28 |
| #30 | `float-display` | `9ae8115` | branch gates under 09-27; Tier 2 in `all-prs` `46ecb76` | 2026-09-29 | #27 |
| #31 | `list-copies` | `a824e7b` | branch gates under 09-27; Tier 2 in `all-prs` `46ecb76` | 2026-09-29 | #27 |
| #32 | `issue-6-list-copy` | `42c53ff` | branch gates under 09-27; Tier 2 in `all-prs` `46ecb76` | 2026-09-29 | #27 |
| #33 | `num-runtime-sweep` | `314fb60` | branch gates under 09-27; Tier 2 in `all-prs` `8f475fd` | 2026-09-29 | #28, #25 (and #23) |
| #34 | `record-tuple-moves` | `1a8cdcd` | branch gates under 09-27; Tier 2 in `all-prs` `2c46f6d` | 2026-09-29 | #31 |

The work in #28 to #34 is by the cloud Claude, cherry-picked with `-x`.

## Held here, not yet sent

Nothing. The cloud Claude's next work, slice-backed lists (NOTES (18), approved in (20)), isn't started.

## Issues filed

| Where | # | What | State |
|---|---|---|---|
| B-Teague/rocflight | 4, 6, 9, 11, 13, 14, 16, 17, 24, 26 | as titled | open; #9 and #24 are fixed in #25, and #6 in #32 |
| roc-lang/roc | 11662 | `roc glue` fails on 09-23 | open |
| roc-lang/roc | 11801 | an unannotated generic with method calls costs O(body²) in inference | open |
| roc-lang/roc | 11806 | segfault: an effectful call with an unresolved argument inside `for` | fixed by roc-lang/roc#11813, closed 2026-09-29 |
| roc-lang/roc | 11807 | hex floats with more than 16 significant digits round wrongly | open |
| roc-lang/roc | 11808 | an unannotated `to_inspect` is silently ignored | open |
| roc-lang/roc | 11845 | `List.prepend` is O(n) and undocumented as such; after `drop_first` it reallocates every time (drafted by the cloud Claude) | open |
| roc-lang/examples | 296 | annotate `to_inspect` in EncodeDecode and CustomInspect | merged 2026-09-28 |
