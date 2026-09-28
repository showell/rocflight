# Syntax: operator precedence — desugared, explicit types.
# Desugaring makes the implicit grouping explicit with parens.
app [main!] {}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    a : I64
    a = 2 + (3 * 4)
    b : I64
    b = (2 + 3) * 4
    echo!("${I64.to_str(a)} ${I64.to_str(b)}")
    Ok({})
}
