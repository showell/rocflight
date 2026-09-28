# Syntax: scientific notation — desugared, explicit types.
#
# A whole float prints WITHOUT a trailing `.0`: 1e3 shows as 1000, not 1000.0.
#
# Valid with or without a fraction: `1e3` is 1000.
app [main!] {}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    a : F64
    a = 1e3
    b : F64
    b = 1.5e3
    c : F64
    c = 1.5e-3
    d : F64
    d = 1.5E3
    echo!("${F64.to_str(a)},${F64.to_str(b)},${F64.to_str(c)},${F64.to_str(d)}")
    Ok({})
}
