# Syntax: record update — desugared, explicit types.
#
# A new record is built from the base, with the named fields replaced. The base is not
# mutated — there is no mutation of records in Roc.
#
# NOT `{ base & field: value }`: roc rejects that outright. Named fields override the
# base's; every other field is carried over.
app [main!] {}

Person : { name: Str, age: I64 }

birthday : Person -> Person
birthday = |p| { ..p, age: 31 }

rename : Person -> Person
rename = |p| { ..p, name: "renamed" }

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    start : Person
    start = { name: "ada", age: 30 }
    echo!("${Str.inspect(birthday(start))},${Str.inspect(rename(start))},${I64.to_str(start.age)}")
    Ok({})
}
