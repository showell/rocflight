# Syntax: tuple equality — desugared, explicit types. Element-wise, like records.
app [main!] {}

same : (I64, Str), (I64, Str) -> Bool
same = |a, b| a == b

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${Str.inspect(same((1, "x"), (1, "x")))},${Str.inspect(same((1, "x"), (2, "x")))}")
    Ok({})
}
