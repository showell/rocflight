# Syntax: payload binding — desugared, explicit types.
#
# `Foo(n, label)` binds each payload positionally. The bindings are scoped to that
# arm only. Pattern arity must match the tag's payload count.
app [main!] {}

render : [Foo(I64, Str), Bar] -> Str
render = |t| match t {
    Foo(n, label) => "${label}=${I64.to_str(n)}"
    Bar => "bar"
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${render(Foo(42, "answer"))},${render(Bar)}")
    Ok({})
}
