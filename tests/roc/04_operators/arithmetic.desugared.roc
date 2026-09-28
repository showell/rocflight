# Syntax: + - * / — desugared, explicit types.
# / is fractional division: both operands are F64 here, not I64.
#
# Bindings stay inside main!'s block, exactly where the sugared file has them, so
# the two files differ ONLY in sugar and must build the same AST.
app [main!] {}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    sum : I64
    sum = 5 + 3
    diff : I64
    diff = 10 - 4
    prod : I64
    prod = 6 * 7
    quot : F64
    quot = 7.0 / 2.0
    echo!("${I64.to_str(sum)} ${I64.to_str(diff)} ${I64.to_str(prod)} ${F64.to_str(quot)}")
    Ok({})
}
