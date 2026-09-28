# Syntax: match on bare tags — desugared, explicit types.
#
# match is an EXPRESSION: every arm's body must have the same type, and the match
# has that type. roc requires the arms to be EXHAUSTIVE — a match missing a case is
# rejected with "This match expression doesn't cover all possible cases."
app [main!] {}

describe : [Red, Green, Blue] -> Str
describe = |c| match c {
    Red => "red"
    Green => "green"
    Blue => "blue"
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${describe(Red)},${describe(Green)},${describe(Blue)}")
    Ok({})
}
