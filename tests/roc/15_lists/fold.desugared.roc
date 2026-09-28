# Syntax: List.fold — desugared, explicit types. Order is (list, initial, fn).
app [main!] {}

total : List(I64) -> I64
total = |xs| List.fold(xs, 0, |acc, x| acc + x)

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${I64.to_str(total([1, 2, 3, 4]))},${I64.to_str(total([]))}")
    Ok({})
}
