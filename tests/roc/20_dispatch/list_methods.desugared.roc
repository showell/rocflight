# Syntax: List dispatch — desugared, explicit types.
#
# `xs.map(f)` is `List.map(xs, f)` and `xs.fold(0, f)` is `List.fold(xs, 0, f)`: the
# receiver is prepended, the written arguments follow. Chaining works because the
# result of one dispatch is itself a receiver — `xs.len().to_str()`.
app [main!] {}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    seed : I64
    seed = I64.from_str("0") ?? 0
    xs : List(I64)
    xs = [1, 2, 3]
    echo!("${xs.len().to_str()},${Str.inspect(xs.map(|x| x * 2))},${xs.fold(seed, |a, x| a + x).to_str()}")
    Ok({})
}
