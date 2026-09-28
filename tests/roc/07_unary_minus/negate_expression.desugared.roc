# Syntax: unary minus over expressions — desugared, explicit types.
#
# The operand is the whole postfix chain, so `-r.v` negates the FIELD, and
# I64.to_str(`-n)` would try to negate a Str — an error, not a syntax quirk.
app [main!] {}

scale : I64 -> I64
scale = |x| x * 10

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    r : { v: I64 }
    r = { v: 4 }
    sum : I64
    sum = 1 + 2
    echo!("${I64.to_str(r.v.negate())},${I64.to_str(scale(2).negate())},${I64.to_str(sum.negate())}")
    Ok({})
}
