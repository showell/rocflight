# Syntax: comparing tags — desugared, explicit types.
#
# Bare tags compare by name; payload tags compare payloads too. The record stays
# inline exactly as the sugared file has it, so the two build the same AST.
app [main!] {}

chosen : [Red, Green, Blue]
chosen = Green

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!(Str.inspect({ same: chosen == Green, other: chosen == Red }))
    Ok({})
}
