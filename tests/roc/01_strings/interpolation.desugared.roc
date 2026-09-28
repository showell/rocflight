# Syntax: string interpolation — desugared, explicit types.
# Interpolation is NOT desugared to concat: it is a primitive string form.
app [main!] {}

name : Str
name = "Roc"

count : I64
count = 3

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${name} has ${I64.to_str(count)} letters")
    Ok({})
}
