# Syntax: List.map — desugared, explicit types. Argument order is (list, fn).
app [main!] {}

doubled : List(I64) -> List(I64)
doubled = |xs| List.map(xs, |x| x * 2)

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    xs = [1, 2, 3]
    echo!("${Str.inspect(doubled(xs))},${Str.inspect(doubled([]))},${I64.to_str(xs.fold(0, |a, x| a + x))}")
    Ok({})
}
