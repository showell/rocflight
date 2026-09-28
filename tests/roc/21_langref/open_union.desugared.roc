# Syntax: a named open tag union — desugared, explicit types.
#
# `[Red, ..]` says "Red and possibly more". Naming the rest as `..u` ties two
# positions to the same leftovers, so a function can take a union and hand back
# whatever extra tags came with it.
#
# Because the union is open, a `match` on it needs a wildcard: the compiler cannot
# know the tags it has not been shown.
app [main!] {}

describe : [Red, ..u] -> Str
describe = |c| match c {
    Red => "red"
    _ => "other"
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${describe(Red)},${describe(Green)},${describe(Blue)}")
    Ok({})
}
