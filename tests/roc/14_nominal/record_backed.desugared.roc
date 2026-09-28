# Syntax: nominal type over a record — desugared, explicit types.
#
# The nominal name is erased at the value level: Str.inspect shows the BACKING
# record, not the name. What the name buys is TYPE distinctness — two nominals with
# identical backing types do not interchange.
#
# Constructed with `Name.{ ... }`; fields are read with plain `.field`.
app [main!] {}

Point := { x: I64, y: I64 }

origin_distance : Point -> I64
origin_distance = |p| p.x + p.y

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    here : Point
    here = Point.{ x: 3, y: 4 }
    echo!("${I64.to_str(origin_distance(here))},${Str.inspect(here)}")
    Ok({})
}
