# Syntax: inferred union — desugared, explicit types.
#
# The two branches yield different tags, so the if's type is the UNION of them.
# Unifying two tag unions merges their tags rather than demanding they be equal;
# payloads of a shared tag must still agree.
app [main!] {}

pick : Bool -> [Red, Green]
pick = |b| {
    if b {
        Red
    } else {
        Green
    }
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${Str.inspect(pick(Bool.True))},${Str.inspect(pick(Bool.False))}")
    Ok({})
}
