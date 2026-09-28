# Syntax: `?` error-propagation operator — desugared, explicit types.
# `?` expands to a match on the Try, propagating Err unchanged.
#
# The error tag is BadNumStr, not InvalidNumStr: found by annotating the wrong
# one and reading the expected type back out of `roc check`.
app [main!] {}

parse_both : Str, Str => Try(I64, [BadNumStr])
parse_both = |a, b| {
    match I64.from_str(a) {
        Ok(x) =>
            match I64.from_str(b) {
                Ok(y) => Ok(x + y)
                Err(e) => return Err(e)
            }
        Err(e) => return Err(e)
    }
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    match parse_both("1", "2") {
        Ok(n) => echo!(I64.to_str(n))
        Err(_) => echo!("err")
    }
    Ok({})
}
