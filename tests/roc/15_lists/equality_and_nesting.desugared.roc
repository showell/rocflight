# Syntax: list equality and nesting — desugared, explicit types.
# Equality is element-wise; Str.inspect recurses into nested lists.
app [main!] {}

same : List(I64), List(I64) -> Bool
same = |a, b| a == b

nested : List(List(I64))
nested = [[1], [2, 3]]

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${Str.inspect(same([1, 2], [1, 2]))},${Str.inspect(same([1], [2]))},${Str.inspect(nested)},${I64.to_str(nested.fold(0, |a, xs| a + xs.fold(0, |b, x| b + x)))}")
    Ok({})
}
