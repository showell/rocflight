# Findings

Programs where rocflight and `roc` disagree, each reduced and each with a
README saying what `roc` does and what rocflight does. They are the cases
behind PRs to rocflight; a finding leaves when its fix is merged.

| finding | |
|---|---|
| `nominal-is-eq-recursion/` | `==` on the unwrapped values inside `is_eq` recurses into `is_eq` |
| `nominal-list-copy/` | a write through a nominal that declares `from_quote` copies the list |
