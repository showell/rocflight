//! H's field moves, extended to the two shapes it missed: a field read and then
//! replaced by record update syntax, `{ ..r, a: r.a.append(x) }`, and a tuple element,
//! `(t.0.append(x), t.1 + 1)`. Both were quadratic in a loop; both are how Roc state is
//! usually threaded through one.
//!
//! Soundness first: a moved-out part must never be observable. Then growth: 8x the
//! input under 24x the time, as in `record_moves_test.rs`.

use std::process::Command;

fn run(test: &str, program: &str) -> (String, f64) {
    let dir = std::env::temp_dir().join(format!("rocflight-update-tuple-{}-{}", test, std::process::id()));
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

/// Update syntax: the field read and then replaced; the old record used after the
/// update; a read field the update does NOT replace; an update replacing other fields;
/// a loop. Tuples: an element read twice; read, then the whole tuple; a step function;
/// a destructure, then the whole tuple; a tuple captured by a closure. Expected
/// output: roc 09-27's.
#[test]
fn a_moved_field_or_element_is_never_observable() {
    let program = r####"app [main!] {}

State : { log : List(U64), n : U64, tag : Str }

grow : State, U64 -> State
grow = |s, x| { ..s, log: s.log.append(x) }

grow_keep_old : State, U64 -> (State, State)
grow_keep_old = |s, x| {
	next = { ..s, log: s.log.append(x) }
	(next, s)
}

other_field : State, U64 -> (List(U64), State)
other_field = |s, x| {
	a = s.log.append(x)
	(a, { ..s, n: s.n + 1 })
}

replace_other : State -> State
replace_other = |s| { ..s, tag: Str.concat(s.tag, "!"), n: List.len(s.log) }

looped : State -> State
looped = |s| {
	var $st = s
	for i in 0..<3.U64 {
		$st = { ..$st, log: $st.log.append(i), n: $st.n + 1 }
	}
	$st
}

twice : (List(U64), U64) -> (List(U64), List(U64))
twice = |t| (t.0.append(1), t.0.append(2))

then_whole : (List(U64), U64) -> (List(U64), (List(U64), U64))
then_whole = |t| {
	a = t.0.append(9)
	(a, t)
}

step : (List(U64), U64) -> (List(U64), U64)
step = |t| (t.0.append(t.1), t.1 + 1)

destructured : (List(U64), U64) -> (List(U64), (List(U64), U64))
destructured = |t| {
	(xs, k) = t
	(xs.append(k), t)
}

captured : (List(U64), U64) -> (List(U64), List(U64))
captured = |t| {
	f = |x| t.0.append(x)
	first = t.0.append(7)
	(first, f(8))
}

main! = |_args| {
	s : State
	s = { log: [1, 2], n: 5, tag: "a" }
	echo!("${Str.inspect(grow(s, 3))}\n")
	echo!("${Str.inspect(grow_keep_old(s, 3))}\n")
	echo!("${Str.inspect(other_field(s, 3))}\n")
	echo!("${Str.inspect(replace_other(s))}\n")
	echo!("${Str.inspect(looped(s))}\n")
	t = ([1.U64, 2], 5.U64)
	echo!("${Str.inspect(twice(t))}\n")
	echo!("${Str.inspect(then_whole(t))}\n")
	echo!("${Str.inspect(step(step(t)))}\n")
	echo!("${Str.inspect(destructured(t))}\n")
	echo!("${Str.inspect(captured(t))}\n")
	echo!("${Str.inspect((s, t))}\n")
	Ok({})
}
"####;
    let expected = r####"{ log: [1, 2, 3], n: 5, tag: "a" }
({ log: [1, 2, 3], n: 5, tag: "a" }, { log: [1, 2], n: 5, tag: "a" })
([1, 2, 3], { log: [1, 2], n: 6, tag: "a" })
{ log: [1, 2], n: 2, tag: "a!" }
{ log: [1, 2, 0, 1, 2], n: 8, tag: "a" }
([1, 2, 1], [1, 2, 2])
([1, 2, 9], ([1, 2], 5))
([1, 2, 5, 6], 7)
([1, 2, 5], ([1, 2], 5))
([1, 2, 7], [1, 2, 8])
({ log: [1, 2], n: 5, tag: "a" }, ([1, 2], 5))
"####;
    assert_eq!(run("soundness", program).0, expected);
}

/// The rest of the battery: a replaced field read twice inside the update; a read
/// in one branch and the whole value in the other; a read round a loop with the
/// value live across the back edge; a record captured by a closure. Expected
/// output: roc 09-27's.
#[test]
fn a_moved_part_is_never_observable_across_branches_loops_and_closures() {
    let program = r####"app [main!] {}

State : { log : List(U64), n : U64, tag : Str }

read_twice : State, U64 -> State
read_twice = |s, x| { ..s, log: s.log.append(x), n: List.len(s.log) }

branchy : State, Bool -> (List(U64), State)
branchy = |s, which| if which (s.log.append(1), { ..s, n: 0 }) else ([], s)

branchy_update : State, Bool -> (State, State)
branchy_update = |s, which| if which { next = { ..s, log: s.log.append(2) }
	(next, next) } else ({ ..s, n: 9 }, s)

loop_live : State -> List(U64)
loop_live = |s| {
	var $acc = []
	for i in 0..<3.U64 {
		$acc = $acc.concat(s.log.append(i))
	}
	$acc
}

record_captured : State -> (State, List(U64))
record_captured = |s| {
	f = |x| s.log.append(x)
	next = { ..s, log: s.log.append(7) }
	(next, f(8))
}

tuple_branch : (List(U64), U64), Bool -> (List(U64), (List(U64), U64))
tuple_branch = |t, which| if which (t.0.append(3), ([], 0)) else ([], t)

tuple_loop_live : (List(U64), U64) -> List(U64)
tuple_loop_live = |t| {
	var $acc = []
	for i in 0..<3.U64 {
		$acc = $acc.concat(t.0.append(i))
	}
	$acc
}

tuple_read_twice : (List(U64), U64) -> (List(U64), U64)
tuple_read_twice = |t| (t.0.append(t.1), List.len(t.0))

main! = |_args| {
	s = { log: [1, 2], n: 5, tag: "a" }
	echo!("${Str.inspect(read_twice(s, 3))}\n")
	echo!("${Str.inspect(branchy(s, Bool.True))}\n")
	echo!("${Str.inspect(branchy(s, Bool.False))}\n")
	echo!("${Str.inspect(branchy_update(s, Bool.True))}\n")
	echo!("${Str.inspect(branchy_update(s, Bool.False))}\n")
	echo!("${Str.inspect(loop_live(s))}\n")
	echo!("${Str.inspect(record_captured(s))}\n")
	t = ([1.U64, 2], 5.U64)
	echo!("${Str.inspect(tuple_branch(t, Bool.True))}\n")
	echo!("${Str.inspect(tuple_branch(t, Bool.False))}\n")
	echo!("${Str.inspect(tuple_loop_live(t))}\n")
	echo!("${Str.inspect(tuple_read_twice(t))}\n")
	echo!("${Str.inspect((s, t))}\n")
	Ok({})
}
"####;
    let expected = r####"{ log: [1, 2, 3], n: 2, tag: "a" }
([1, 2, 1], { log: [1, 2], n: 0, tag: "a" })
([], { log: [1, 2], n: 5, tag: "a" })
({ log: [1, 2, 2], n: 5, tag: "a" }, { log: [1, 2, 2], n: 5, tag: "a" })
({ log: [1, 2], n: 9, tag: "a" }, { log: [1, 2], n: 5, tag: "a" })
[1, 2, 0, 1, 2, 1, 1, 2, 2]
({ log: [1, 2, 7], n: 5, tag: "a" }, [1, 2, 8])
([1, 2, 3], ([], 0))
([], ([1, 2], 5))
[1, 2, 0, 1, 2, 1, 1, 2, 2]
([1, 2, 5], 2)
({ log: [1, 2], n: 5, tag: "a" }, ([1, 2], 5))
"####;
    assert_eq!(run("soundness_more", program).0, expected);
}

fn assert_linear(test: &str, body: &str, n: usize) {
    let at = |n: usize| {
        format!(
            "app [main!] {{}}\n\nmain! = |_args| {{\n{}\n\tOk({{}})\n}}\n",
            body.replace("NN", &n.to_string())
        )
    };
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

#[test]
fn record_update_syntax_is_linear() {
    let method = "\tvar $r = { a: [], b: 0.U64 }\n\tfor i in 0..<NN.U64 {\n\t\t$r = { ..$r, a: $r.a.append(i) }\n\t}\n\techo!(\"${Str.inspect(List.len($r.a))}\\n\")";
    let qualified = "\tvar $r = { a: [], b: 0.U64 }\n\tfor i in 0..<NN.U64 {\n\t\t$r = { ..$r, a: List.append($r.a, i), b: $r.b + 1 }\n\t}\n\techo!(\"${Str.inspect(List.len($r.a))}\\n\")";
    assert_linear("update_method", method, 2_000);
    assert_linear("update_qualified", qualified, 2_000);
}

#[test]
fn a_list_inside_a_tuple_is_linear() {
    let method = "\tvar $t = ([], 0.U64)\n\tfor i in 0..<NN.U64 {\n\t\t$t = ($t.0.append(i), $t.1 + 1)\n\t}\n\techo!(\"${Str.inspect(List.len($t.0))}\\n\")";
    let destructured = "\tvar $t = ([], 0.U64)\n\tfor i in 0..<NN.U64 {\n\t\t(xs, k) = $t\n\t\t$t = (List.append(xs, i), k + 1)\n\t}\n\techo!(\"${Str.inspect(List.len($t.0))}\\n\")";
    assert_linear("tuple_method", method, 2_000);
    assert_linear("tuple_destructured", destructured, 2_000);
}
