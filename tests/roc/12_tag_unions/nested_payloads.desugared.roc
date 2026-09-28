# Syntax: nested tag payloads — desugared, explicit types.
#
# Str.inspect recurses into payloads, so strings stay quoted and a record payload
# still gets its fields sorted alphabetically.
app [main!] {}

nested : [Outer([Inner(Str)])]
nested = Outer(Inner("y"))

mixed : [Wrap({ b: Bool, a: [Red] })]
mixed = Wrap({ b: Bool.True, a: Red })

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${Str.inspect(nested)}|${Str.inspect(mixed)}")
    Ok({})
}
