# Syntax: `??` precedence — desugared, explicit types.
#
# `x ?? 1 + 2` is `x ?? (1 + 2)`, so the Err case is 3 — not `(x ?? 1) + 2`.
# Desugaring makes the grouping explicit.
app [main!] {}

parse_or_sum : Str -> I64
parse_or_sum = |s| match I64.from_str(s) {
    Ok(v) => v
    Err(_) => (1 + 2)
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${I64.to_str(parse_or_sum("nope"))},${I64.to_str(parse_or_sum("9"))}")
    Ok({})
}
