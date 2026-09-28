# Syntax: pipeline precedence — desugared, explicit types.
#
# Desugaring makes the grouping explicit: the pipe applies to the operand next to it,
# not to the whole surrounding expression.
#
# That is the opposite of most languages, where a pipe is the loosest thing in an
# expression. Here `1 + 2 |> inc` is `1 + inc(2)`, giving 4 — not `inc(1 + 2)`.
app [main!] {}

inc : I64 -> I64
inc = |n| n + 1

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    a : I64
    a = 1 + 2 |> inc
    b : I64
    b = 2 * 3 |> inc
    echo!("${I64.to_str(a)},${I64.to_str(b)}")
    Ok({})
}
