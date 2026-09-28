# Syntax: while loop — desugared, explicit types.
#
# The condition is re-evaluated each pass, so it has to read a `var` the body updates.
app [main!] {}

sum_below : I64 -> I64
sum_below = |limit| {
    var $i = 0
    var $sum = 0
    while $i < limit {
        $sum = $sum + $i
        $i = $i + 1
    }
    $sum
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${I64.to_str(sum_below(5))},${I64.to_str(sum_below(0))}")
    Ok({})
}
