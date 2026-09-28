# Syntax: braced if/else — desugared, explicit types.
#
# A braced branch is a block, so it may bind names and ends in an expression whose
# value is the branch's value. Braces here are never a record literal: a record
# field is `name: value`, a block statement is `name = value`.
app [main!] {}

describe : I64 -> Str
describe = |n| if n == 5 {
    label : Str
    label = "Five"
    label
} else {
    "NotFive"
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${describe(5)},${describe(6)}")
    Ok({})
}
