# Syntax: chained pipeline — desugared, explicit types.
#
# Left-associative, so `n |> double |> inc` is `inc(double(n))` — 11, not 12. Writing
# it out reverses the reading order, which is the whole reason the operator exists.
app [main!] {}

double : I64 -> I64
double = |n| n * 2

inc : I64 -> I64
inc = |n| n + 1

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    n : I64
    n = 5
    echo!(I64.to_str(n |> double |> inc))
    Ok({})
}
