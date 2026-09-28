# Syntax: Str.inspect on a record — desugared, explicit types.
#
# Str.inspect sorts fields alphabetically and does so recursively. Booleans render
# as True / False. Strings would render quoted.
#
# Deliberately all-Bool: an integer literal that nothing constrains inspects as
# "42.0" in Roc, because it defaults to a fractional type, while an I64-annotated
# one inspects as "42". Mixing an unannotated number into an inspected record would
# make the sugared and desugared outputs differ — which the pair gate would catch.
app [main!] {}

r : { zebra: Bool, apple: Bool, mango: Bool }
r = { zebra: Bool.True, apple: Bool.False, mango: Bool.True }

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!(Str.inspect(r))
    Ok({})
}
