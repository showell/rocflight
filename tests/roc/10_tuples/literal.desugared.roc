# Syntax: tuple literals — desugared, explicit types.
#
# A tuple is heterogeneous and fixed-length; a list is homogeneous and variable. Its
# type is written (T1, T2). Str.inspect renders it (v1, v2) and recurses.
#
# `(1)` is NOT a one-tuple — it is just a parenthesised expression.
app [main!] {}

pair : (Str, I64)
pair = ("Roc", 1)

tri : (I64, I64, I64)
tri = (1, 2, 3)

nested : (I64, (Str, I64))
nested = (1, ("a", 2))

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${Str.inspect(pair)},${Str.inspect(tri)},${Str.inspect(nested)},${I64.to_str(pair.1)},${I64.to_str(tri.0 + tri.1 + tri.2)},${I64.to_str(nested.0)},${I64.to_str(nested.1.1)}")
    Ok({})
}
