# Syntax: method dispatch — desugared, explicit types.
#
# `c.show()` is `Counter.show(c)`, and `c.bump(2)` is `Counter.bump(c, 2)` — the same
# receiver-first rule static dispatch uses for builtins.
#
# Same call as `Type.method(value, args)`, with the receiver moved in front.
app [main!] {}

Counter :: { n: I64 }.{
    start : Counter
    start = { n: 0 }

    bump : Counter, I64 -> Counter
    bump = |c, by| { ..c, n: c.n + by }

    show : Counter -> Str
    show = |c| c.n.to_str()
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    c : Counter
    c = Counter.bump(Counter.start, 5)
    echo!("${c.show()},${Counter.show(c)},${c.bump(2).show()}")
    Ok({})
}
