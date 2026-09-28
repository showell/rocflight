# Syntax: literal patterns — desugared, explicit types.
#
# `_` is the wildcard: it matches anything and binds nothing. Integer and string
# literal patterns need it (or full coverage) to satisfy exhaustiveness, since there
# is no way to list every I64 or Str.
app [main!] {}

count_name : I64 -> Str
count_name = |n| match n {
    1 => "one"
    2 => "two"
    _ => "many"
}

initial : Str -> Str
initial = |s| match s {
    "alice" => "A"
    "bob" => "B"
    _ => "?"
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${count_name(1)},${count_name(9)},${initial("bob")},${initial("zed")}")
    Ok({})
}
