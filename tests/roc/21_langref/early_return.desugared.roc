# Syntax: early return — desugared, explicit types.
#
# It unwinds to the lambda boundary, so the statements after it are skipped. Every
# `if` still needs an else, which is why the non-returning arm is `{}`.
#
# Distinct from `break`, which leaves the nearest loop.
app [main!] {}

sign : I64 -> Str
sign = |n| {
    if n < 0 { return "neg" } else { {} }
    if n == 0 { return "zero" } else { {} }
    "pos"
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${sign(0 - 1)},${sign(0)},${sign(1)}")
    Ok({})
}
