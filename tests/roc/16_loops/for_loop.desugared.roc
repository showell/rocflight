# Syntax: for loop — desugared, explicit types.
#
# A loop is a PRIMITIVE, not sugar: there is nothing to expand it into here. Its value
# is `{}`, so it is a statement rather than something you bind.
#
# `var` declares a rebindable name; a plain binding cannot be reassigned, roc reports
# it as a redeclaration. The `$` is part of the identifier, like the `!` on an
# effectful name — `$sum` and `sum` are different names.
app [main!] {}

total : List(I64) -> I64
total = |xs| {
    var $sum = 0
    for n in xs {
        $sum = $sum + n
    }
    $sum
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${I64.to_str(total([1, 2, 3, 4]))},${I64.to_str(total([]))}")
    Ok({})
}
