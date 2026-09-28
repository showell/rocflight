# Syntax: nested if — desugared, explicit types.
# Desugaring replaces the grouping parens with braces, which is a block.
app [main!] {}

size : I64 -> Str
size = |n| {
    if n > 0 {
        if n > 10 {
            "big"
        } else {
            "small"
        }
    } else {
        "neg"
    }
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${size(20)},${size(5)},${size(0 - 1)}")
    Ok({})
}
