# Syntax: record destructuring — desugared, explicit types.
#
# `{ name, age } = person` binds both fields at once. Inside a PATTERN a bare name is
# shorthand for `name: name`; inside a record LITERAL it is not — `{ name }` there is a
# block whose value is `name`.
app [main!] {}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    person : { name: Str, age: I64 }
    person = { name: "ada", age: 30 }
    { name, age } = person
    echo!("${name},${I64.to_str(age)}")
    Ok({})
}
