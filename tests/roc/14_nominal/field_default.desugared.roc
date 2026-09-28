# Syntax: field default — desugared, explicit types.
#
# Omitting the field substitutes the declared default at construction, so the record
# always has it. That is why no unwrapping is needed to read it.
#
# Only allowed on a nominal's backing record, which is why every construction that
# omits one is an explicit `Name.{ ... }`. A defaulted field is ALWAYS there when read,
# so it is read with plain `.field` — no unwrapping.
app [main!] {}

Cfg := { host: Str, port: U16 ?? 8080 }

show : Cfg -> Str
show = |c| "${c.host}:${c.port.to_str()}"

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${show(Cfg.{ host: "a" })},${show(Cfg.{ host: "b", port: 99 })}")
    Ok({})
}
