# Syntax: nominal destructuring pattern — desugared, explicit types.
#
# `Name.{ x, y }` in a parameter position unwraps the nominal and binds its fields.
# Shorthand: `{ x }` binds the field `x` to the name `x`.
app [main!] {}

Point := { x: I64, y: I64 }

get_x : Point -> I64
get_x = |Point.{ x, y }| x + y

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!(I64.to_str(get_x(Point.{ x: 9, y: 1 })))
    Ok({})
}
