# Syntax: tags with payloads — desugared, explicit types.
#
# A payload tag is written Tag(T1, T2) in the type and Tag(v1, v2) as a value.
# Str.inspect renders it Tag(v1, v2). Payload arity and types are both checked.
app [main!] {}

pair : [Foo(I64, Str), Bar]
pair = Foo(42, "hi")

single : [Wrap(Str)]
single = Wrap("x")

first_of = |t| match t {
    Foo(n, _) => n
    _ => 0
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${Str.inspect(pair)},${Str.inspect(single)},${Str.inspect(Bar)},${I64.to_str(first_of(pair))}")
    Ok({})
}
