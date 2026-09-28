# Syntax: alternative patterns — desugared, explicit types.
#
# `A | B => body` matches either and shares one body. Alternatives cannot bind
# different variables, since the body has to work for every branch.
app [main!] {}

warm_or_cool : [Red, Orange, Blue, Cyan] -> Str
warm_or_cool = |c| match c {
    Red | Orange => "warm"
    Blue | Cyan => "cool"
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${warm_or_cool(Red)},${warm_or_cool(Orange)},${warm_or_cool(Cyan)}")
    Ok({})
}
