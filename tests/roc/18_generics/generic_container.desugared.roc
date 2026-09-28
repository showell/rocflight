# Syntax: generic containers — desugared, explicit types.
#
# The variable is bound by the whole signature, so `List(a) -> List(a)` returns a list
# of the SAME element type it was given — the two positions are not independent.
#
# `count` returns U64 because that is what `List.len` gives; annotating it I64 makes
# roc reject the definition.
app [main!] {}

echo_list : List(a) -> List(a)
echo_list = |xs| xs

count : List(a) -> U64
count = |xs| List.len(xs)

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    nums : List(I64)
    nums = echo_list([1, 2])
    words : List(Str)
    words = echo_list(["a", "b", "c"])
    echo!("${Str.inspect(nums)},${Str.inspect(words)},${U64.to_str(count(words))},${I64.to_str(nums.fold(0, |a, x| a + x))}")
    Ok({})
}
