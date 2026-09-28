# Syntax: optional field — desugared, explicit types.
#
# `.?field` is the only way to read one, and it yields a Try. `.?` on an ORDINARY field
# is not meaningful — doing it segfaults the roc compiler on nightly-2026-09-03.
#
# Unlike a defaulted field, an optional one may genuinely be absent, so reading it
# gives a `Try`: `Ok(value)` or `Err(MissingField)`.
app [main!] {}

Cfg := { host: Str, timeout ?: U64 }

describe : Cfg -> Str
describe = |c| match c.?timeout {
    Ok(t) => "${t.to_str()}ms"
    Err(MissingField) => "no timeout"
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${describe(Cfg.{ host: "a" })},${describe(Cfg.{ host: "b", timeout: 30 })}")
    Ok({})
}
