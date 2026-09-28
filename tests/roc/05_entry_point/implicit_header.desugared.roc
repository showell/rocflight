# Syntax: headerless platformless app — desugared.
# Desugaring makes the implied header explicit and annotates the entry point.
# The default host requires exactly: List(Str) => Try(_a, [Exit(I8), ..])
app [main!] {}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("hello")
    Ok({})
}
