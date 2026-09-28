# Syntax: List.len — desugared, explicit types. Returns U64.
app [main!] {}

size : List(I64) -> U64
size = |xs| List.len(xs)

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${U64.to_str(size([1, 2, 3]))},${U64.to_str(size([]))}")
    Ok({})
}
