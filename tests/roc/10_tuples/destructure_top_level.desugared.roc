# Syntax: top-level destructuring — desugared, explicit types.
#
# A top-level binding must not be scoped: the continuation is the rest of the file,
# which defines main!. So this desugars to indexing rather than the one-arm match
# used inside a block, where scoping an arm is correct. `_` binds nothing.
app [main!] {}

pair : (Str, I64)
pair = ("Roc", 1)

(name, n) = pair

(_, second) = pair

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${name},${I64.to_str(n)},${I64.to_str(second)}")
    Ok({})
}
