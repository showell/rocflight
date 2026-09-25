# Integer conversions that wrap: each `to_<int>_wrap` roc defines on each
# number type (roc's own list; 128-bit targets aside), at a value that does not
# fit the target, so the answer is the low bits and the sign they give. From
# F32 and F64, the whole part, in range and out of it.
app [main!] {}

main! = |_args| {
	v1 : I64
	v1 = 1_099_511_627_781
	echo!("I64 1_099_511_627_781: ${v1.to_i8_wrap().to_str()} ${v1.to_i16_wrap().to_str()} ${v1.to_i32_wrap().to_str()} ${v1.to_i64_wrap().to_str()} ${v1.to_u8_wrap().to_str()} ${v1.to_u16_wrap().to_str()} ${v1.to_u32_wrap().to_str()} ${v1.to_u64_wrap().to_str()}\n")
	v2 : I64
	v2 = -1
	echo!("I64 -1: ${v2.to_i8_wrap().to_str()} ${v2.to_i16_wrap().to_str()} ${v2.to_i32_wrap().to_str()} ${v2.to_i64_wrap().to_str()} ${v2.to_u8_wrap().to_str()} ${v2.to_u16_wrap().to_str()} ${v2.to_u32_wrap().to_str()} ${v2.to_u64_wrap().to_str()}\n")
	v3 : U64
	v3 = 18_446_744_073_709_551_615
	echo!("U64 18_446_744_073_709_551_615: ${v3.to_i8_wrap().to_str()} ${v3.to_i16_wrap().to_str()} ${v3.to_i32_wrap().to_str()} ${v3.to_i64_wrap().to_str()} ${v3.to_u8_wrap().to_str()} ${v3.to_u16_wrap().to_str()} ${v3.to_u32_wrap().to_str()} ${v3.to_u64_wrap().to_str()}\n")
	v4 : I32
	v4 = -70_000
	echo!("I32 -70_000: ${v4.to_i8_wrap().to_str()} ${v4.to_i16_wrap().to_str()} ${v4.to_i32_wrap().to_str()} ${v4.to_u8_wrap().to_str()} ${v4.to_u16_wrap().to_str()} ${v4.to_u32_wrap().to_str()} ${v4.to_u64_wrap().to_str()}\n")
	v5 : U32
	v5 = 4_000_000_000
	echo!("U32 4_000_000_000: ${v5.to_i8_wrap().to_str()} ${v5.to_i16_wrap().to_str()} ${v5.to_i32_wrap().to_str()} ${v5.to_u8_wrap().to_str()} ${v5.to_u16_wrap().to_str()} ${v5.to_u32_wrap().to_str()}\n")
	v6 : I16
	v6 = -300
	echo!("I16 -300: ${v6.to_i8_wrap().to_str()} ${v6.to_i16_wrap().to_str()} ${v6.to_u8_wrap().to_str()} ${v6.to_u16_wrap().to_str()} ${v6.to_u32_wrap().to_str()} ${v6.to_u64_wrap().to_str()}\n")
	v7 : U16
	v7 = 65_000
	echo!("U16 65_000: ${v7.to_i8_wrap().to_str()} ${v7.to_i16_wrap().to_str()} ${v7.to_u8_wrap().to_str()} ${v7.to_u16_wrap().to_str()}\n")
	v8 : I8
	v8 = -100
	echo!("I8 -100: ${v8.to_i8_wrap().to_str()} ${v8.to_u8_wrap().to_str()} ${v8.to_u16_wrap().to_str()} ${v8.to_u32_wrap().to_str()} ${v8.to_u64_wrap().to_str()}\n")
	v9 : U8
	v9 = 200
	echo!("U8 200: ${v9.to_i8_wrap().to_str()} ${v9.to_u8_wrap().to_str()}\n")
	v10 : F64
	v10 = -123.75
	echo!("F64 -123.75: ${v10.to_i8_wrap().to_str()} ${v10.to_i16_wrap().to_str()} ${v10.to_i32_wrap().to_str()} ${v10.to_i64_wrap().to_str()} ${v10.to_u8_wrap().to_str()} ${v10.to_u16_wrap().to_str()} ${v10.to_u32_wrap().to_str()} ${v10.to_u64_wrap().to_str()}\n")
	v11 : F64
	v11 = 5_000_000_000.5
	echo!("F64 5_000_000_000.5: ${v11.to_i8_wrap().to_str()} ${v11.to_i16_wrap().to_str()} ${v11.to_i32_wrap().to_str()} ${v11.to_i64_wrap().to_str()} ${v11.to_u8_wrap().to_str()} ${v11.to_u16_wrap().to_str()} ${v11.to_u32_wrap().to_str()} ${v11.to_u64_wrap().to_str()}\n")
	v12 : F32
	v12 = -123.75
	echo!("F32 -123.75: ${v12.to_i8_wrap().to_str()} ${v12.to_i16_wrap().to_str()} ${v12.to_i32_wrap().to_str()} ${v12.to_i64_wrap().to_str()} ${v12.to_u8_wrap().to_str()} ${v12.to_u16_wrap().to_str()} ${v12.to_u32_wrap().to_str()} ${v12.to_u64_wrap().to_str()}\n")
	v13 : F32
	v13 = 5_000_000_000.5
	echo!("F32 5_000_000_000.5: ${v13.to_i8_wrap().to_str()} ${v13.to_i16_wrap().to_str()} ${v13.to_i32_wrap().to_str()} ${v13.to_i64_wrap().to_str()} ${v13.to_u8_wrap().to_str()} ${v13.to_u16_wrap().to_str()} ${v13.to_u32_wrap().to_str()} ${v13.to_u64_wrap().to_str()}\n")
	f : F64
	f = 1.0e40
	echo!("F64 to F32: ${f.to_f32_wrap().to_f64().to_str()} ${v10.to_f32_wrap().to_f64().to_str()}\n")
	Ok({})
}
