# Syntax: explicit platformless app header — desugared, explicit types.
app [main!] {}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_| {
    echo!("hello")
    Ok({})
}
