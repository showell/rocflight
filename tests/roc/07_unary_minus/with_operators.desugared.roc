# Syntax: unary minus with operators — desugared, explicit types.
#
# Precedence, from tightest: postfix (call, field, dispatch), then `|>`, then unary
# minus, then the binary operators. So `-n |> inc` negates the PIPED result, giving
# -6 rather than inc(-5) = -4.
app [main!] {}

inc : I64 -> I64
inc = |x| x + 1

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    n : I64
    n = 5
    a : I64
    a = 2 * n.negate()
    b : I64
    b = 10 - n.negate()
    c : I64
    c = (n |> inc).negate()
    echo!("${I64.to_str(a)},${I64.to_str(b)},${I64.to_str(c)}")
    Ok({})
}
