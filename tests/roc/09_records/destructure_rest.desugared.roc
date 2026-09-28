# Syntax: record rest pattern — desugared, explicit types.
#
# `..rest` collects the unnamed fields into a NEW record, so the result type has fewer
# fields than the input. That is what makes it a way to remove one.
#
# Naming a field with `_` and capturing the rest is how you drop a field.
app [main!] {}

Full : { name: Str, age: I64, email: Str }
Trimmed : { name: Str, age: I64 }

drop_email : Full -> Trimmed
drop_email = |p| {
    { email: _, ..rest } = p
    rest
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    trimmed = drop_email({ name: "ada", age: 30, email: "a@b.c" })
    echo!("${Str.inspect(trimmed)},${I64.to_str(trimmed.age)}")
    Ok({})
}
