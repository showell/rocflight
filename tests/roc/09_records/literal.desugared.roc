# Syntax: record literal — desugared, explicit types.
# A trailing comma before `}` is legal. Fields keep source order in the type;
# only Str.inspect sorts them.
app [main!] {}

point : { x: Bool, y: Bool }
point = { x: Bool.True, y: Bool.False }

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!(Str.inspect(point))
    Ok({})
}
