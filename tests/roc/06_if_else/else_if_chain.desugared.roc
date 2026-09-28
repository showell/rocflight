# Syntax: else-if chain — desugared, explicit types.
#
# `else if` is not a separate construct: it is an if/else whose else branch is
# another if/else. Desugaring makes that nesting explicit with braces.
app [main!] {}

name_of : I64 -> Str
name_of = |n| {
    if n == 3 {
        "Three"
    } else {
        if n == 4 {
            "Four"
        } else {
            "Other"
        }
    }
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${name_of(3)},${name_of(4)},${name_of(9)}")
    Ok({})
}
