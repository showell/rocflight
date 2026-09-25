# `==` inside a nominal's `is_eq` calls `is_eq` again

`Text :: List(U8)` defines `is_eq` by unwrapping both sides and comparing
the lists: `|Text.(a), Text.(b)| a == b`. `a` and `b` are `List(U8)`, so
their `==` is the list's.

    $ roc run main.roc          # nightly-2026-09-22-e494788
    equal
    $ rocflight main.roc
    Error: Runtime error: Recursion went deeper than 1000000 calls in `Text.is_eq` at Text.roc:6:31

rocflight dispatches the inner `==` to `Text.is_eq` again, presumably by
the value's shape, since a nominal is erased at run time.
