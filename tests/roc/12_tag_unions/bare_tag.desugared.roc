# Syntax: bare tags — desugared, explicit types.
#
# A tag union type is written [Red, Green, Blue]. Str.inspect renders a bare tag as
# just its name. Tags are capitalised, which is how the parser tells `Green` from a
# variable and `Str.inspect` from a field access.
app [main!] {}

favourite : [Red, Green, Blue]
favourite = Green

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${Str.inspect(favourite)},${Str.inspect(Red)}")
    Ok({})
}
