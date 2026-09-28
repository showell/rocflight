# Syntax: nested string in interpolation — desugared, explicit types.
#
# Interpolation is a primitive string form, not sugar for concatenation, so nothing
# is rewritten here. What matters is the scanner: inside ${...} it tracks brace depth
# AND skips over nested string literals, honouring escapes so a \" cannot end them
# early.
app [main!] {}

shout : Str -> Str
shout = |s| s

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("a=${shout("one")},b=${shout("two")}")
    Ok({})
}
