# Syntax: and / or / ! — desugared, explicit types.
# Prefix ! canonicalizes to Bool.not upstream; it is NOT the effectful-name !.
app [main!] {}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    r : { both: Bool, either: Bool, negated: Bool }
    r = { both: Bool.True and Bool.False, either: Bool.True or Bool.False, negated: Bool.not(Bool.True) }
    echo!(Str.inspect(r))
    Ok({})
}
