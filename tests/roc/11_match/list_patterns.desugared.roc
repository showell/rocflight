# Syntax: exact-length list patterns — desugared, explicit types.
#
# A list pattern matches on LENGTH first, then element-wise. Literal elements must
# be equal; lowercase names bind. Arms are tried in order, so [1, 2] beats [a, b].
#
# The match goes through a function so the scrutinee is not a compile-time constant:
# roc warns "this match value is known at compile time" otherwise.
app [main!] {}

describe : List(I64) -> Str
describe = |xs| match xs {
    [] => "empty"
    [x] => "one:${I64.to_str(x)}"
    [1, 2] => "exactly one-two"
    [a, b] => "two:${I64.to_str(a + b)}"
    _ => "many"
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${describe([])},${describe([7])},${describe([1, 2])},${describe([3, 4])},${describe([1, 2, 3])}")
    Ok({})
}
