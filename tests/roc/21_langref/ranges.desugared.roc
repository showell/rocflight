# Syntax: ranges — desugared, explicit types.
#
# A range is NOT a list: `Str.inspect` renders it `<opaque>`, and it cannot be
# passed where a `List` is wanted. It exists to be iterated.
#
# `..<` and `..=` bind looser than `+` and `*`, so `1 + 1..<2 * 3` is `2..<6`.
app [main!] {}

sum_over : Range(I64) -> I64
sum_over = |range| {
    var $total = 0
    for n in range {
        $total = $total + n
    }
    $total
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    exclusive : I64
    exclusive = sum_over(0..<5)
    inclusive : I64
    inclusive = sum_over(1..=5)
    lo : I64
    lo = 2
    hi : I64
    hi = 5
    from_vars : I64
    from_vars = sum_over(lo..<hi)
    from_exprs : I64
    from_exprs = sum_over(1 + 1..<2 * 3)
    echo!("${I64.to_str(exclusive)},${I64.to_str(inclusive)},${I64.to_str(from_vars)},${I64.to_str(from_exprs)}")
    echo!(Str.inspect(0..<3))
    Ok({})
}
