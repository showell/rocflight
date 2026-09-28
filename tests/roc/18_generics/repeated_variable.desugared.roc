# Syntax: repeated type variables — desugared, explicit types.
#
# Within ONE signature a repeated name is one variable, so `pair(1, "s")` is rejected.
# Across signatures the names are unrelated: the `a` in `pair` is not the `a` in
# `first`.
#
# `pair : a, a -> a` requires both arguments to have one type; `first : a, b -> a`
# lets them differ.
app [main!] {}

pair : a, a -> a
pair = |x, _y| x

first : a, b -> a
first = |x, _y| x

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    both : I64
    both = pair(1, 2)
    mixed : I64
    mixed = first(3, "ignored")
    echo!("${I64.to_str(both)},${I64.to_str(mixed)}")
    Ok({})
}
