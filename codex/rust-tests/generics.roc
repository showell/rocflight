# Type variables: a lambda inside a generic function has the function's own
# (`without`), a local function Roc generalizes is a closure at the one type it
# is used at (`ascending`), and a nested `if` builds tags of the whole union
# (`sign`). `Try.map_ok`'s lambda has its declared type.
app [main!] {}

without : List(a), a -> List(a) where [a.is_eq : a, a -> Bool]
without = |xs, item| List.drop_if(xs, |other| other == item)

sign : I64 -> [Above, Below, Equal]
sign = |n| if n > 0 { Above } else if n < 0 { Below } else { Equal }

half : I64 -> Try(I64, [Odd])
half = |n| if n % 2 == 0 { Ok(n // 2) } else { Err(Odd) }

main! = |_args| {
	ascending = |xs| List.sort_with(xs, |a, b| if a < b { Before } else if a > b { After } else { Same })
	sorted = ascending([3.U64, 1, 2])
	echo!("${Str.join_with(without(["a", "b", "a"], "a"), " ")} ${Str.join_with(List.map(sorted, |n| n.to_str()), " ")}\n")
	signs = List.map([5, -2, 0, 7], |n| sign(n))
	echo!("${List.count_if(signs, |s| s == Above).to_str()} ${List.count_if(signs, |s| s == Below).to_str()}\n")
	levels = List.map([3, 1, 0], |n| if n > 2 { High } else if n > 0 { Low } else { Zero })
	echo!("${List.count_if(levels, |l| l == Low).to_str()}\n")
	echo!(match Try.map_ok(half(10), |h| { h, big: h > 3 }) {
		Ok(r) => "${r.h.to_str()} ${if r.big { "big" } else { "small" }}\n"
		Err(Odd) => "odd\n"
	})
	Ok({})
}
