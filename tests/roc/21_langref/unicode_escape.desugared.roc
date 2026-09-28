# Syntax: unicode escape — desugared, explicit types.
#
# The escape is resolved by the tokenizer, so the desugared form is unchanged. Both
# spellings below render as "café", one precomposed and one with a combining accent.
app [main!] {}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    accented : Str
    accented = "caf\u(e9)"
    combining : Str
    combining = "cafe\u(301)"
    echo!("${accented},${combining}")
    Ok({})
}
