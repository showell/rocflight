# Syntax: list literals — desugared, explicit types.
#
# Elements are annotated because an unconstrained integer literal inspects as "1.0"
# in roc — it defaults to a fractional type.
app [main!] {}

xs : List(I64)
xs = [1, 2, 3]

empty : List(I64)
empty = []

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${Str.inspect(xs)},${Str.inspect(empty)},${I64.to_str(xs.fold(0, |a, x| a + x))}")
    Ok({})
}
