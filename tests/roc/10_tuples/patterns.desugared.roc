# Syntax: tuple patterns — desugared, explicit types.
#
# A tuple pattern matches element-wise at a fixed arity. Arms are tried in order, so
# (0, 0) is tested before (x, 0).
app [main!] {}

locate : (I64, I64) -> Str
locate = |p| match p {
    (0, 0) => "origin"
    (x, 0) => "x-axis:${I64.to_str(x)}"
    (0, y) => "y-axis:${I64.to_str(y)}"
    _ => "elsewhere"
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${locate((0, 0))},${locate((3, 0))},${locate((0, 4))},${locate((1, 1))}")
    Ok({})
}
