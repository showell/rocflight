# Syntax: // and % — desugared, explicit types.
app [main!] {}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    d : I64
    d = 7 // 2
    m : I64
    m = 7 % 2
    echo!("${I64.to_str(d)} ${I64.to_str(m)}")
    Ok({})
}
