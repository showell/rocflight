# A write through a nominal with `from_quote` copies the list

`Text :: List(U8)` wraps a list. `main.roc` writes one unit of it 2000
times, through `Text.set_unit`, which unwraps, `List.set`s and rewraps.

`./run.sh` times it at three sizes (`ROCFLIGHT=` names the binary):

    1024 units:  0.29 s
    4096 units:  1.14 s
   16384 units:  4.18 s

The time grows with the size, so every write copies the list. `roc run`
(nightly-2026-09-22-e494788) takes 0.06 s at 16384.

Without `from_quote` in `Text.roc`, rocflight takes 0.00 s at every size.
`from_quote` is never called; declaring it is enough. `List.concat`
through the same nominal copies the same way.

A nominal over a list with `from_quote` is how a program gets string
literals of its own text type (`classify : I64 -> Text` answering
`"zero"`), so any loop that builds such a text is quadratic.
