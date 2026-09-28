# Syntax: expect and dbg — desugared, explicit types.
#
# Neither aborts. A passing `expect` is silent; a failing one prints to stderr and
# execution continues, which is why "still running" appears after it.
#
# A failing `expect` is reported, not fatal — the program continues.
app [main!] {}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    dbg "marker"
    expect 1 + 1 == 2
    expect 1 + 1 == 3
    echo!("still running")
    Ok({})
}
