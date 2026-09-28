# Syntax: `Name.(payload)` — desugared, explicit types.
#
# The record form is `Name.{ ... }`; this is its sibling for everything else. The
# payload may be a single value or, with commas, a tuple.
#
# The nominal name is erased at runtime: `Str.inspect` shows the bare payload. The
# type is still distinct, which is what a nominal is for — a `UserId` does not pass
# where a plain `U64` is wanted.
app [main!] {}

UserId := U64
Pair := (I64, Str)

show_id : UserId -> Str
show_id = |u| Str.inspect(u)

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    id = UserId.(7)
    computed = UserId.(3 + 4)
    p = Pair.(1, "two")
    echo!("${show_id(id)},${show_id(computed)},${Str.inspect(p)}")
    Ok({})
}
