# Syntax: grapheme literals — desugared, explicit types.
#
# roc has no character type. `'a'` is a number literal spelled visually, so it
# unifies with any number type and takes part in arithmetic.
app [main!] {}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    a : I64
    a = 'a'
    next : I64
    next = 'a' + 1
    accented : I64
    accented = 'é'
    newline : I64
    newline = '\n'
    escaped : I64
    escaped = '\u(e9)'
    echo!("${I64.to_str(a)},${I64.to_str(next)},${I64.to_str(accented)},${I64.to_str(newline)},${I64.to_str(escaped)}")
    Ok({})
}
