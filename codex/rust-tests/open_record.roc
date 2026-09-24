# An open record (`k.key`) that meets the closed record it is: the lambda
# sorting `kept` reads `a.i`, which only the closed record has.
app [main!] {}

firsts : List(Str) -> List(Str)
firsts = |lines| {
	keyed = List.map_with_index(lines, |line, i| { key: line, i, line })
	kept = List.fold(
		keyed,
		[],
		|acc, x|
			match List.last(acc) {
				Ok(k) if k.key == x.key => acc
				_ => List.append(acc, x)
			},
	)
	List.map(List.sort_with(kept, |a, b| if a.i < b.i { Before } else if a.i > b.i { After } else { Same }), |x| x.line)
}

main! = |_args| {
	echo!("${Str.join_with(firsts(["b", "a", "a"]), ",")}\n")
	Ok({})
}
