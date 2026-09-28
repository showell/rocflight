# Syntax: static dispatch — desugared, explicit types.
#
# `n.to_str()` is `I64.to_str(n)`. The receiver becomes the first argument, which is
# why roc's builtins take their subject first.
#
# The binding is annotated on purpose: an unconstrained integer literal defaults to a
# fractional type, so `42.to_str()` would print "42.0".
app [main!] {}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    n : I64
    n = I64.from_str("42") ?? 0
    small : I64
    small = I64.from_str("7") ?? 0
    echo!("${n.to_str()},${small.to_str()}")
    Ok({})
}
