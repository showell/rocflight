app [main!] {}

import Text

main! = |_args| {
	same = Text.from_units([1, 2]) == Text.from_units([1, 2])
	echo!(if same { "equal\n" } else { "different\n" })
	Ok({})
}
