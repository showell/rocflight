# Syntax: digit separators — desugared, explicit types.
#
# Separators are lexical only: they are removed before the value is read, in every
# radix. Stopping at the `_` silently truncated the number.
#
# A separator must sit BETWEEN digits, so it never starts or ends a run.
app [main!] {}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    big : I64
    big = 1_000_000
    hex : I64
    hex = 0xFF_FF
    bin : I64
    bin = 0b1010_1010
    frac : F64
    frac = 1_0.2_5
    echo!("${I64.to_str(big)},${I64.to_str(hex)},${I64.to_str(bin)},${F64.to_str(frac)}")
    Ok({})
}
