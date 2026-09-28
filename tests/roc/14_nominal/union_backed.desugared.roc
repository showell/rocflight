# Syntax: nominal type over a tag union — desugared, explicit types.
#
# `Name.Tag(v)` builds the same value a bare `Tag(v)` would; the qualification names
# which nominal type it belongs to. Patterns qualify the same way.
#
# Tags are reached as `Name.Tag`, in both construction and patterns.
app [main!] {}

Animal := [Dog(Str), Cat(Str)]

speak : Animal -> Str
speak = |a| match a {
    Animal.Dog(name) => "${name} says woof"
    Animal.Cat(name) => "${name} says meow"
}

main! : List(Str) => Try({}, [Exit(I8)])
main! = |_args| {
    echo!("${speak(Animal.Dog("rex"))},${speak(Animal.Cat("tom"))}")
    Ok({})
}
