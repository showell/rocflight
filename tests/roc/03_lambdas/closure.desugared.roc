# Syntax: closure — desugared, explicit types.
# The returned lambda's captured environment is part of its type, not its syntax.
app [main!] {}

make_adder : I64 -> (I64 -> I64)
make_adder = |n| |x| x + n

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    add5 : I64 -> I64
    add5 = make_adder(5)
    echo!(I64.to_str(add5(37)))
    Ok({})
}
