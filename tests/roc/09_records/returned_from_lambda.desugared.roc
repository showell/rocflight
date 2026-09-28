# Syntax: a lambda returning a record — desugared, explicit types.
#
# A braced lambda body is a record literal when it starts `name: value` and a block
# when it does not. Every site accepting braces shares that decision, or `|n| { v: n }`
# gets parsed as a block and fails on `v: n`.
app [main!] {}

flags : Bool -> { enabled: Bool, disabled: Bool }
flags = |on| { enabled: on, disabled: Bool.not(on) }

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!(Str.inspect(flags(Bool.True)))
    Ok({})
}
