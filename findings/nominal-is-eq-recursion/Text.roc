Text :: List(U8).{
	from_units : List(U8) -> Text
	from_units = |us| Text.(us)

	is_eq : Text, Text -> Bool
	is_eq = |Text.(a), Text.(b)| a == b
}
