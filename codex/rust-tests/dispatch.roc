# Method calls: a builtin's (`xs.concat(ys)`, `n.to_str()`) and a nominal's
# own (`c.bump()`), plus the builtins whose `Err` is a tag of the program's.
app [main!] {}

Counter := { n : I64 }.{
	bump : Counter -> Counter
	bump = |c| Counter.{ n: c.n + 1 }
}

find_big : List(I64) -> Try(I64, [NotFound])
find_big = |xs| List.find_first(xs, |x| x > 10)

main! = |_args| {
	xs = [1, 2].concat([3])
	c = Counter.{ n: 41 }.bump()
	echo!("${xs.len().to_str()} ${c.n.to_str()}\n")
	echo!(match find_big([3, 30]) {
		Ok(x) => "big ${x.to_str()}\n"
		Err(NotFound) => "none\n"
	})
	echo!(match find_big([3]) {
		Ok(x) => "big ${x.to_str()}\n"
		Err(NotFound) => "none\n"
	})
	sorted : List(I64)
	sorted = List.sort_with([3, 1, 2], |a, b| if a < b { Before } else if a > b { After } else { Same })
	echo!("${Str.join_with(List.map(sorted, |n| n.to_str()), " ")}\n")
	Ok({})
}
