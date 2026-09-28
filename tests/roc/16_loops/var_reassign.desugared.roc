# Syntax: var reassignment — desugared, explicit types.
#
# Reassignment UPDATES the existing binding rather than shadowing it, which is what
# makes a loop body able to affect the value read after the loop.
app [main!] {}

stepped : I64 -> I64
stepped = |start| {
    var $acc = start
    $acc = $acc * 2
    $acc = $acc + 1
    $acc
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${I64.to_str(stepped(5))},${I64.to_str(stepped(0))}")
    Ok({})
}
