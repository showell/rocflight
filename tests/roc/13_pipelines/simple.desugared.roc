# Syntax: pipeline — desugared, explicit types.
#
# `x |> f` becomes the ordinary call `f(x)`. Nothing survives of the operator itself:
# it is pure sugar for reordering a call.
app [main!] {}

double : I64 -> I64
double = |n| n * 2

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    n : I64
    n = 21
    echo!(I64.to_str(n |> double))
    Ok({})
}
