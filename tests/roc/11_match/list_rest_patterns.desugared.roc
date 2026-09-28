# Syntax: list rest patterns — desugared, explicit types.
#
# `..` matches any number of elements, including none. It may appear at the end, in
# the middle, or at the start, and `.. as name` binds the skipped elements as a list.
# Only one `..` is allowed per pattern.
#
# Returns U64 because List.len does, and every arm of a match must agree on type.
app [main!] {}

classify : List(I64) -> U64
classify = |xs| match xs {
    [1, 2, ..] => 66
    [2, .., 1] => 88
    [9, .. as tail] => 77 + List.len(tail)
    [.., 5] => 55
    _ => 100
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${U64.to_str(classify([1, 2, 9]))},${U64.to_str(classify([2, 8, 8, 1]))},${U64.to_str(classify([9, 4, 4]))},${U64.to_str(classify([3, 5]))},${U64.to_str(classify([7]))}")
    Ok({})
}
