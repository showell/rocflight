# Syntax: tuple destructuring — desugared, explicit types.
#
# `(name, n) = pair` binds both elements at once. The left side is a PATTERN in a
# binding position, which is why the parser reuses the match pattern parser here.
app [main!] {}

pair : (Str, I64)
pair = ("Roc", 1)

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    (name, n) = pair
    echo!("${name},${I64.to_str(n)}")
    Ok({})
}
