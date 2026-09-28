# Syntax: method block — desugared, explicit types.
#
# A method is a plain function namespaced under the type. Written out explicitly it is
# `Name.method(receiver, ...)`; the dispatch form `receiver.method(...)` is the same
# call with the receiver moved in front.
#
# The block holds ordinary functions, reached as `Name.method(...)`.
app [main!] {}

Secret :: { key: Str }.{
    new : Str -> Secret
    new = |k| { key: k }

    reveal : Secret -> Str
    reveal = |s| s.key
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    s : Secret
    s = Secret.new("hunter2")
    echo!(Secret.reveal(s))
    Ok({})
}
