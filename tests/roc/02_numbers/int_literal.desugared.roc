# Syntax: decimal integer literal — desugared, explicit types.
# Bare int literals default to I64 when nothing constrains them.
app [main!] {}

birds : I64
birds = 3

debt : I64
debt = -7

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${I64.to_str(birds)} ${I64.to_str(debt)}")
    Ok({})
}
