# Syntax: lambda — desugared, explicit types.
# Lambdas are already explicit; desugaring only adds the annotation.
app [main!] {}

inc : I64 -> I64
inc = |x| x + 1

add : I64, I64 -> I64
add = |x, y| x + y

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${I64.to_str(inc(41))} ${I64.to_str(add(20, 22))}")
    Ok({})
}
