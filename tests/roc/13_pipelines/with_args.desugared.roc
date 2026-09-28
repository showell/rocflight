# Syntax: pipeline with arguments — desugared, explicit types.
#
# The piped value becomes the FIRST argument, so `n |> subtract(4)` is
# `subtract(n, 4)` — 6, not -6.
#
# `x |> f(a)` is `f(x, a)` — the piped value is PREPENDED, the same convention static
# dispatch uses. Subtraction is used below because it would give a different answer if
# the order were reversed.
app [main!] {}

subtract : I64, I64 -> I64
subtract = |a, b| a - b

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    n : I64
    n = 10
    echo!(I64.to_str(n |> subtract(4)))
    Ok({})
}
