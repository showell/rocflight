# `var`, `while`, `for`, `break` inside a `match`, an early `return`, and
# string interpolation.
app [main!] {}

first_over : List(I64), I64 -> Str
first_over = |xs, limit| {
	for x in xs {
		if x > limit {
			return "first over ${I64.to_str(limit)}: ${I64.to_str(x)}"
		}
	}
	"none over ${I64.to_str(limit)}"
}

sum_until_negative : List(I64) -> I64
sum_until_negative = |xs| {
	var $total = 0
	for x in xs {
		match x < 0 {
			True => break
			False => {
				$total = $total + x
			}
		}
	}
	$total
}

main! = |_args| {
	var $i = 0
	var $squares = []
	while $i < 5 {
		$squares = List.append($squares, $i * $i)
		$i = $i + 1
	}
	echo!("${Str.join_with(List.map($squares, |n| I64.to_str(n)), ",")}\n")
	echo!("${first_over([1, 5, 9], 4)}\n")
	echo!("${first_over([1, 2], 4)}\n")
	echo!("${I64.to_str(sum_until_negative([3, 4, -1, 10]))}\n")
	Ok({})
}
