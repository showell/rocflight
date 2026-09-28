# Syntax: a parameterised nominal — desugared, explicit types.
#
# The parameters after the name become type variables in the backing type, so one
# declaration gives a whole family: `Wrapper(Str)`, `Wrapper(I64)`, and so on.
#
# A function over the family stays generic by naming the parameter itself —
# `unwrap : Wrapper(a) -> a` works for every instantiation.
app [main!] {}

Wrapper(a) := { item: a }
Pairing(a, b) := { left: a, right: b }

unwrap : Wrapper(a) -> a
unwrap = |w| w.item

swap : Pairing(a, b) -> Pairing(b, a)
swap = |p| Pairing.{ left: p.right, right: p.left }

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    word : Wrapper(Str)
    word = Wrapper.{ item: "roc" }
    count : Wrapper(I64)
    count = Wrapper.{ item: 42 }
    both : Pairing(Str, I64)
    both = Pairing.{ left: "a", right: 1 }
    flipped : Pairing(I64, Str)
    flipped = swap(both)
    echo!("${unwrap(word)},${I64.to_str(unwrap(count))},${I64.to_str(flipped.left)},${flipped.right}")
    Ok({})
}
