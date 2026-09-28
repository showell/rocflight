# Syntax: `??` — desugared, explicit types.
#
# `expr ?? d` becomes a match on the Try. Unlike `?` this is a purely local rewrite:
# the default replaces the Err case in place, so no continuation has to move.
app [main!] {}

parse_or : Str, I64 -> I64
parse_or = |s, d| match I64.from_str(s) {
    Ok(v) => v
    Err(_) => d
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${I64.to_str(parse_or("42", 0))},${I64.to_str(parse_or("nope", 7))}")
    Ok({})
}
