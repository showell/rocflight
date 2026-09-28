# Syntax: open records — desugared, explicit types.
#
# An open record type says "at least these fields". A closed one — no `..` — says
# exactly these. Opening the type is what lets one function serve records that carry
# extra fields.
#
# The named form `..r` binds the rest, so two positions can be tied to the same
# leftovers. Anonymous `..` is the common case.
app [main!] {}

name_of : { name: Str, .. } -> Str
name_of = |r| r.name

same_extras : { id: I64, ..r }, { id: I64, ..r } -> I64
same_extras = |a, b| a.id + b.id

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    person = { name: "Ada", age: 36 }
    bare = { name: "Bob" }
    left = { id: 1, tag: "x" }
    right = { id: 2, tag: "y" }
    echo!("${name_of(person)},${name_of(bare)},${I64.to_str(same_extras(left, right))}")
    Ok({})
}
