# Syntax: crash — desugared, explicit types.
#
# `crash` never returns a value, so it fits in a branch whose other arm is a Str.
#
# The branch here is never taken — a crash that IS taken has no output to compare,
# and with a constant condition roc evaluates it at compile time and refuses the file.
app [main!] {}

check : List(Str) -> Str
check = |args| if List.len(args) > 99 { crash "impossible" } else { "ok" }

main! : List(Str) => Try({}, [Exit(I8)])
main! = |args| {
    echo!(check(args))
    Ok({})
}
