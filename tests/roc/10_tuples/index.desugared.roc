# Syntax: tuple indexing — desugared, explicit types.
#
# .0 is postfix like record field access, so it applies to any receiver, including a
# call result: mk(5).1. The index is a number, not a name.
app [main!] {}

pair : (Str, I64)
pair = ("Roc", 1)

mk : I64 -> (I64, I64)
mk = |n| (n, n + 1)

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${pair.0},${I64.to_str(pair.1)},${I64.to_str(mk(5).1)}")
    Ok({})
}
