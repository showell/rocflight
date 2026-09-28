# Syntax: chained dispatch — desugared, explicit types.
#
# Chaining needs each dispatch to have a KNOWN result type, since the result is the
# next receiver. `fold` returns its accumulator, so its type comes from the first
# written argument rather than from the method name alone.
#
# `xs.fold(seed, |a, x| a - x)` is `List.fold(xs, seed, f)` — subtraction is used here
# precisely because it would give a different answer if the arguments were swapped.
app [main!] {}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    seed : I64
    seed = I64.from_str("10") ?? 0
    xs : List(I64)
    xs = [1, 2]
    r : { s: Str }
    r = { s: "" }
    echo!("${xs.fold(seed, |a, x| a - x).to_str()},${Str.inspect(r.s.is_empty())},${xs.map(|x| x).len().to_str()}")
    Ok({})
}
