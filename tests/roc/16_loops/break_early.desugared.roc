# Syntax: break — desugared, explicit types.
#
# `break` leaves the nearest enclosing loop. `continue` is NOT supported: it crashes
# the roc compiler on nightly-2026-09-03, so no pair can be written against it.
#
# Every `if` needs an else branch, so the non-breaking arm is `{}`.
app [main!] {}

first_negative : List(I64) -> I64
first_negative = |xs| {
    var $found = 0
    for n in xs {
        if n < 0 {
            $found = n
            break
        } else {
            {}
        }
    }
    $found
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${I64.to_str(first_negative([1, 2, 0 - 7, 3]))},${I64.to_str(first_negative([1, 2]))}")
    Ok({})
}
