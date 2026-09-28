# Syntax: escape sequences — desugared, explicit types.
# Escapes are resolved by the tokenizer, so the desugared form is unchanged.
app [main!] {}

line : Str
line = "tab:\there\nquote:\"q\"\nbackslash:\\"

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!(line)
    Ok({})
}
