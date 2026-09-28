# Syntax: `?` short-circuits — desugared, explicit types.
#
# `x = expr?` becomes a match whose Ok arm holds THE REST OF THE BLOCK. That is why
# `?` is the first genuinely non-trivial desugaring: the continuation has to be moved
# inside the arm, not merely rewritten in place.
app [main!] {}

add_one : Str => Try(I64, [BadNumStr])
add_one = |s| {
    match I64.from_str(s) {
        Ok(n) => Ok(n + 1)
        Err(e) => return Err(e)
    }
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    good = match add_one("41") { Ok(v) => I64.to_str(v) Err(_) => "err" }
    bad = match add_one("nope") { Ok(v) => I64.to_str(v) Err(_) => "err" }
    echo!("${good},${bad}")
    Ok({})
}
