# Syntax: effectful call `echo!` — desugared, explicit types.
# The `!` is part of the identifier, not a postfix operator: nothing is rewritten.
# `=>` (not `->`) is the arrow for an effectful function.
app [main!] {}

shout : Str -> Str
shout = |s| "${s}!"

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!(shout("one"))
    echo!(shout("two"))
    Ok({})
}
