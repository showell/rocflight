# Syntax: real platform — desugared, explicit types.
#
# `Stdout.line! : Str => Try({}, [StdoutErr(IOErr), ..])`, read from the platform's own
# cached source rather than assumed.
#
# The entry point's type comes from the platform's `requires` clause, not the default
# host's: the argument is `List(OsStr)`, not `List(Str)`. The error type is OPEN
# (`[Exit(I32), ..]`), so writing it out means the UNION of what the platform requires
# and what the body can raise — `[Exit(I32), StdoutErr(IOErr), ..]`. Neither half alone
# is accepted.
app [main!] {
	cli: platform "https://github.com/roc-lang/basic-cli/releases/download/0.22.0/F1JVZPYfWP71s8vk6tHcV1Qx1Ef6CZkwswGoCn8VHZmL.tar.zst",
	roc: "nightly-2026-09-03-62fcb65",
}

import cli.Stdout
import cli.IOErr exposing [IOErr]
import cli.OsStr exposing [OsStr]

main! : List(OsStr) => Try({}, [Exit(I32), StdoutErr(IOErr)])
main! = |_args| {
	Stdout.line!("hello from a real platform")
}
