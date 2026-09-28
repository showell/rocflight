# Syntax: Str dispatch — desugared, explicit types.
#
# A zero-argument method still needs its parens: `s.is_empty()`, not `s.is_empty`,
# which would be a field access.
app [main!] {}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    empty : Str
    empty = ""
    filled : Str
    filled = "x"
    echo!("${Str.inspect(empty.is_empty())},${Str.inspect(filled.is_empty())}")
    Ok({})
}
