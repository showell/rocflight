# Syntax: one-line if/else — desugared, explicit types.
#
# if/else is an EXPRESSION, so it has a value and both branches must agree on type.
# There is no statement form and no if-without-else: a missing else is reported as
# "The second branch of this if does not match the previous branch".
# The condition must be a Bool, not a number — roc says so explicitly.
app [main!] {}

classify : I64 -> Str
classify = |n| if n == 1 "One" else "NotOne"

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${classify(1)},${classify(2)}")
    Ok({})
}
