# Syntax: type-suffixed numeric literal — desugared, explicit types.
# The suffix IS the annotation, so desugaring lifts it to a binding annotation.
app [main!] {}

small : U8
small = 255

wide : I32
wide = 42

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${U8.to_str(small)} ${I32.to_str(wide)}")
    Ok({})
}
