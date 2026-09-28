# Syntax: nested tag patterns — desugared, explicit types.
#
# Patterns nest to any depth, and a binding anywhere inside is available in the body.
# `Try(a, b)` being `[Ok(a), Err(b)]` is why `Err(StdoutErr(e))` works the same way.
app [main!] {}

unwrap : [Wrap([Inner(Str), Empty]), Bare] -> Str
unwrap = |t| match t {
    Wrap(Inner(s)) => s
    Wrap(Empty) => "empty"
    Bare => "bare"
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${unwrap(Wrap(Inner("deep")))},${unwrap(Wrap(Empty))},${unwrap(Bare)}")
    Ok({})
}
