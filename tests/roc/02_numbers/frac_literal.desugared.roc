# Syntax: fractional literal — desugared, explicit types.
app [main!] {}

pi : F64
pi = 3.14

neg : F64
neg = -2.5

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${F64.to_str(pi)} ${F64.to_str(neg)}")
    Ok({})
}
