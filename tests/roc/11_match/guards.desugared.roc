# Syntax: guards — desugared, explicit types.
#
# A guard runs only after its pattern matches, and the pattern's bindings are in
# scope for it. If the guard is false the arm is skipped and matching continues,
# so arm ORDER matters: `0` is tested before the guarded `x`.
app [main!] {}

sign_of : I64 -> Str
sign_of = |n| match n {
    0 => "zero"
    x if x < 0 => "negative"
    _ => "positive"
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${sign_of(0)},${sign_of(0 - 5)},${sign_of(5)}")
    Ok({})
}
