# Syntax: string literal — desugared, explicit types
app [main!] {}

greeting : Str
greeting = "hello world"

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!(greeting)
    Ok({})
}
