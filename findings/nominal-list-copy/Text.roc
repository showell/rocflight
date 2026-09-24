Text :: List(U8).{
	# Without this, the loop in main.roc writes in place.
	from_quote : Str -> Try(Text, [BadQuotedBytes(Str)])
	from_quote = |s| Ok(Text.(Str.to_utf8(s)))

	from_units : List(U8) -> Text
	from_units = |us| Text.(us)

	set_unit : Text, U64, U8 -> Text
	set_unit = |Text.(t), i, u| Text.(List.set(t, i, u) ?? crash("set"))

	len : Text -> U64
	len = |Text.(t)| List.len(t)
}
