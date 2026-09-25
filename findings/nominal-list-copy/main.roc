app [main!] {}

import Text

# 2000 writes into a list of SIZE units, the list wrapped in a nominal type.
main! = |_args| {
	var $t = Text.from_units(List.repeat(1, SIZE))
	var $i = 0
	while $i < 2000 {
		$t = Text.set_unit($t, 0, 3)
		$i = $i + 1
	}
	echo!(Str.inspect(Text.len($t)))
	echo!("\n")
	Ok({})
}
