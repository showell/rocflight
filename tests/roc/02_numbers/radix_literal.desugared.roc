# Syntax: hex / octal / binary literals — desugared, explicit types.
# Radix is a lexical form only; the desugared value is the same I64.
app [main!] {}

hex : I64
hex = 0xFF

oct : I64
oct = 0o77

bin : I64
bin = 0b1010

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${I64.to_str(hex)} ${I64.to_str(oct)} ${I64.to_str(bin)}")
    Ok({})
}
