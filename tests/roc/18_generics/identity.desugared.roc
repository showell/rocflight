# Syntax: type variables — desugared, explicit types.
#
# A lowercase name in a signature is universally quantified: the caller picks the type.
# Nothing expands here — generics are a typing feature, invisible at run time.
#
# The same function is used at TWO different types below. That only works because each
# use gets its own instance of `a`; otherwise the first use would pin it.
app [main!] {}

identity : a -> a
identity = |x| x

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    s : Str
    s = identity("hi")
    n : I64
    n = identity(5)
    echo!("${s},${I64.to_str(n)}")
    Ok({})
}
