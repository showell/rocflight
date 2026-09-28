//! A field read out of a record (or a payload out of a tag) at the point nothing reads
//! that field again is MOVED, not cloned, so a list inside a record stays unique and
//! `List.set`/`append` change it in place. Without that, `Builtin.roc`'s `Dict.insert`
//! (`list_set_unsafe(next_data.entries, ...)`) copied the whole table on every insert.
//!
//! Soundness first: a moved-out field must never be observable. Then growth: 8x the
//! input under 24x the time, as in `method_dispatch_test.rs`.

use std::process::Command;

fn run(test: &str, program: &str) -> (String, f64) {
    let dir = std::env::temp_dir().join(format!("rocflight-record-moves-{}-{}", test, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.roc");
    std::fs::write(&file, program).unwrap();
    let started = std::time::Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_rocflight")).arg(&file).output().unwrap();
    let elapsed = started.elapsed().as_secs_f64();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{} failed:\n{}", test, String::from_utf8_lossy(&out.stderr));
    (String::from_utf8(out.stdout).unwrap(), elapsed)
}

/// A field read twice; read and then the whole record used; read on one branch with
/// the whole record on the other; read in a loop with the record live round the back
/// edge; a record captured by a closure; a record reached through a `var`; a
/// destructure followed by the whole record; a tag payload read and then the whole tag
/// inspected, on both arms. The expected output is roc 09-27's.
#[test]
fn a_moved_field_is_never_observable() {
    let program = r####"app [main!] {}

Pair : { xs : List(U64), n : U64 }

twice : Pair -> (List(U64), List(U64))
twice = |r| (r.xs.append(1), r.xs.append(2))

then_whole : Pair -> (List(U64), Pair)
then_whole = |r| {
	a = r.xs.append(9)
	(a, r)
}

branchy : Pair, Bool -> Str
branchy = |r, flag|
	if flag {
		Str.inspect(r.xs.append(5))
	} else {
		Str.inspect(r)
	}

looped : Pair -> List(U64)
looped = |r| {
	var $acc = []
	for i in 0..<3.U64 {
		$acc = $acc.concat(r.xs.append(i))
	}
	$acc
}

captured : Pair -> (List(U64), List(U64))
captured = |r| {
	f = |extra| r.xs.append(extra)
	first = r.xs.append(7)
	(first, f(8))
}

through_var : Pair -> (List(U64), Pair)
through_var = |r| {
	var $p = r
	grown = $p.xs.append(3)
	$p = { xs: grown, n: $p.n + 1 }
	(grown, $p)
}

destructured : Pair -> (List(U64), U64, Pair)
destructured = |r| {
	{ xs, n } = r
	(xs.append(4), n, r)
}

Shape : [Found({ data : Pair, at : U64 }), Missing(U64)]

payload_then_whole : Shape -> (List(U64), Str)
payload_then_whole = |s|
	match s {
		Found({ data, at }) => (data.xs.append(at), Str.inspect(s))
		Missing(k) => ([k], Str.inspect(s))
	}

payload_only : Shape -> List(U64)
payload_only = |s|
	match s {
		Found({ data, .. }) => data.xs.append(data.n)
		Missing(k) => [k]
	}

main! = |_args| {
	r = { xs: [1, 2], n: 5 }
	echo!("${Str.inspect(twice(r))}\n")
	echo!("${Str.inspect(then_whole(r))}\n")
	echo!("${branchy(r, True)} ${branchy(r, False)}\n")
	echo!("${Str.inspect(looped(r))}\n")
	echo!("${Str.inspect(captured(r))}\n")
	echo!("${Str.inspect(through_var(r))}\n")
	echo!("${Str.inspect(destructured(r))}\n")
	echo!("${Str.inspect(payload_then_whole(Found({ data: r, at: 3 })))}\n")
	echo!("${Str.inspect(payload_then_whole(Missing(4)))}\n")
	echo!("${Str.inspect(payload_only(Found({ data: r, at: 3 })))} ${Str.inspect(payload_only(Missing(6)))}\n")
	echo!("${Str.inspect(r)}\n")
	Ok({})
}
"####;
    let expected = r####"([1, 2, 1], [1, 2, 2])
([1, 2, 9], { n: 5, xs: [1, 2] })
[1, 2, 5] { n: 5, xs: [1, 2] }
[1, 2, 0, 1, 2, 1, 1, 2, 2]
([1, 2, 7], [1, 2, 8])
([1, 2, 3], { n: 6, xs: [1, 2, 3] })
([1, 2, 4], 5, { n: 5, xs: [1, 2] })
([1, 2, 3], "Found({ at: 3, data: { n: 5, xs: [1, 2] } })")
([4], "Missing(4)")
[1, 2, 5] [6]
{ n: 5, xs: [1, 2] }
"####;
    assert_eq!(run("soundness", program).0, expected);
}

fn assert_linear(test: &str, program: &str, n: usize) {
    let at = |n: usize| program.replace("NN", &n.to_string());
    run(test, &at(n));
    let (out, small) = run(test, &at(n));
    assert_eq!(out.trim(), n.to_string(), "{}", test);
    let (out, large) = run(test, &at(8 * n));
    assert_eq!(out.trim(), (8 * n).to_string(), "{}", test);
    assert!(
        large < small * 24.0,
        "{}: 8x the input took {:.1}x the time ({:.3}s against {:.3}s)",
        test,
        large / small,
        large,
        small
    );
}

const RECORD_FIELD: &str = "app [main!] {}\n\nmain! = |_args| {\n\tvar $r = { xs: List.repeat(0.U64, NN), n: 0.U64 }\n\tfor i in 0..<NN.U64 {\n\t\t$r = { xs: List.set($r.xs, i, i) ?? [], n: $r.n + 1 }\n\t}\n\techo!(\"${Str.inspect($r.n)}\\n\")\n\tOk({})\n}\n";

const RECORD_DESTRUCTURED: &str = "app [main!] {}\n\nmain! = |_args| {\n\tvar $r = { xs: List.repeat(0.U64, NN), n: 0.U64 }\n\tfor i in 0..<NN.U64 {\n\t\t{ xs, n } = $r\n\t\t$r = { xs: List.set(xs, i, i) ?? [], n: n + 1 }\n\t}\n\techo!(\"${Str.inspect($r.n)}\\n\")\n\tOk({})\n}\n";

/// `$r.xs` is read before `$r.n`, so the move has to be per field: the record is
/// still live when `xs` leaves it.
#[test]
fn updating_a_list_inside_a_record_is_linear() {
    assert_linear("record_field", RECORD_FIELD, 2_000);
    assert_linear("record_destructured", RECORD_DESTRUCTURED, 2_000);
}

fn inserting(container: &str, call: &str) -> String {
    format!(
        "app [main!] {{}}\n\nmain! = |_args| {{\n\tvar $c = {}.empty()\n\tfor i in 0..<NN.U64 {{\n\t\t$c = {}\n\t}}\n\techo!(\"${{Str.inspect({}.len($c))}}\\n\")\n\tOk({{}})\n}}\n",
        container, call, container
    )
}

#[test]
fn dict_insert_is_linear() {
    assert_linear("dict_qualified", &inserting("Dict", "Dict.insert($c, i, i)"), 1_000);
    assert_linear("dict_method", &inserting("Dict", "$c.insert(i, i)"), 1_000);
}

#[test]
fn set_insert_is_linear() {
    assert_linear("set_qualified", &inserting("Set", "Set.insert($c, i)"), 1_000);
    assert_linear("set_method", &inserting("Set", "$c.insert(i)"), 1_000);
}
