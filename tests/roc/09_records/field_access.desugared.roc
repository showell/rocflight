# Syntax: record field access — desugared, explicit types.
#
# `a.b` is field access only when the receiver is lowercase; Roc capitalises
# modules and types, so `Str.inspect` is a module member and `nested.top` a field.
app [main!] {}

nested : { inner: { depth: I64 }, top: I64 }
nested = { inner: { depth: 2 }, top: 1 }

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${I64.to_str(nested.top)},${I64.to_str(nested.inner.depth)}")
    Ok({})
}
