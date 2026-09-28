# Syntax: unary minus — desugared, explicit types.
#
# `-x` IS `x.negate()`. roc lowers it to that method, which is why negating a Str
# fails with "This negate method is being called on a value whose type doesn't have
# that method" rather than a syntax error. Writing it out is valid roc.
app [main!] {}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    n : I64
    n = 5
    echo!(I64.to_str(n.negate()))
    Ok({})
}
