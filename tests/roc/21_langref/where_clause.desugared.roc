# Syntax: a `where` clause — desugared, explicit types.
#
# The list is bracketed, and each constraint names a method the variable must have,
# with its signature. It is what lets a generic function dispatch: without the
# clause, `x.to_str()` has no type to dispatch on and the compiler says so.
#
# Every call site is checked against the constraint, so the promise is the compiler's
# to keep, not the caller's to remember.
app [main!] {}

label : a -> Str where [a.to_str : a -> Str]
label = |x| "<${x.to_str()}>"

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    n : I64
    n = I64.from_str("7") ?? 0
    f : F64
    f = 1.5
    echo!("${label(n)},${label(f)}")
    Ok({})
}
