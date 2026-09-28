# Syntax: comparison operators — desugared, explicit types.
app [main!] {}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    r : { eq: Bool, ne: Bool, lt: Bool, le: Bool, gt: Bool, ge: Bool }
    r = { eq: 1 == 1, ne: 1 != 2, lt: 1 < 2, le: 2 <= 2, gt: 3 > 2, ge: 3 >= 3 }
    echo!(Str.inspect(r))
    Ok({})
}
