//! The builtins: what `Builtin.roc` is allowed to say, and how a call reaches one.

mod common;
use crate::common::*;

// ==========================================================================
// The constructs `src/roc/Builtin.roc` needs that nothing else in `tests/roc` reached.
//
// Every expectation here was produced by running the same program under
// `roc nightly-2026-09-03` on 2026-09-16 and copying what it printed. They are NOT
// golden pairs: `?` now lifts its Try out under a generated `#tryN` name, so no
// hand-written desugared twin can produce an identical AST, which is what a pair in
// `tests/roc/` has to do.
//
// `tests/check_builtin.sh` is the gate these serve — it reports how much of
// Builtin.roc parses. This file says what each construct MEANS once it does.

#[test]
fn for_destructures_a_tuple() {
    // `for (key, value) in Dict.iter(dict)` is how Builtin.roc walks a Dict.
    let src = "f = |ps| {\n\tvar $t = 0\n\tfor (a, b) in ps {\n\t\t$t = $t + a * b\n\t}\n\t$t\n}\n\nf([(1, 2), (3, 4)])";
    assert_eq!(value(src), "14");
}

#[test]
fn for_takes_a_guard() {
    // `for item in list if predicate(item) { … }` runs the body only for the items
    // that pass, which is the body wrapped in an else-less `if`.
    let src = "f = |xs| {\n\tvar $t = 0\n\tfor x in xs if x > 10 {\n\t\t$t = $t + x\n\t}\n\t$t\n}\n\nf([5, 20, 30])";
    assert_eq!(value(src), "50");
}

#[test]
fn question_works_inside_an_expression() {
    // Not just `x = expr?`: the `?` here is an operand of `+`, so it has to be lifted
    // out of the expression it sits in before the match can wrap the block.
    let half = "half = |n| if n % 2 == 0 { Ok(n / 2) } else { Err(Odd) }\n\n";
    let f = "f = |xs| {\n\tvar $t = 0\n\tfor x in xs {\n\t\t$t = $t + half(x)?\n\t}\n\tOk($t)\n}\n\n";
    assert_eq!(value(&format!("{}{}f([2, 4, 6])", half, f)), "Ok(6)");
    // And it short-circuits out of the LOOP, not just out of the iteration.
    assert_eq!(value(&format!("{}{}f([2, 3, 6])", half, f)), "Err(Odd)");
}

#[test]
fn question_may_be_a_loop_bodys_last_statement() {
    // An assignment evaluates to `{}` whatever its right-hand side does, so there is a
    // continuation to propagate into — unlike a block ending in a bare `expr?`.
    let src = "step = |a, b| Ok(a + b)\n\nf = |xs| {\n\tvar $s = 0\n\tfor x in xs {\n\t\t$s = step($s, x)?\n\t}\n\tOk($s)\n}\n\nf([1, 2])";
    assert_eq!(value(src), "Ok(3)");
}

#[test]
fn a_bare_rest_in_a_record_pattern() {
    // `Found({ data, entry_index, .. })`. The `..` names nothing; it says only that
    // the record has other fields, which is what keeps its type open.
    let src = "f = |p| match p {\n\t{ x, y, .. } => x + y\n}\n\nf({ x: 1, y: 2, label: \"p\" })";
    assert_eq!(value(src), "3");
}

#[test]
fn a_tag_can_be_destructured_by_a_binding() {
    // `Ok(encoded) = encode_shape(…)`. Refutable in general — roc rejects the Try case
    // with "non exhaustive destructure", and so does the checker here — but exhaustive
    // for a single-tag union.
    let src = "f = |b| {\n\tWrap(v) = b\n\tv\n}\n\nf(Wrap(7))";
    assert_eq!(value(src), "7");
}

#[test]
fn the_smallest_integer_is_a_literal() {
    // `-9223372036854775808` has a magnitude one past `i64::MAX`, so reading the digits
    // before applying the sign rejects the one literal that names `I64.lowest`.
    assert_eq!(value("x = -9223372036854775808\n\nx"), "-9223372036854775808");
}

#[test]
fn the_languages_own_numbers_fit() {
    // `U64.highest` and `U128.highest` are written out in `Builtin.roc`, and an i64
    // could not hold either — which is what stopped `Iter` and `Num` parsing.
    assert_eq!(value("x = 18446744073709551615\n\nx"), "18446744073709551615");
    // `I128.lowest`, whose magnitude is one past `i128::MAX` — the trap `I64.lowest`
    // sprang, one width up.
    assert_eq!(
        value("x = -170141183460469231731687303715884105728\n\nx"),
        "-170141183460469231731687303715884105728"
    );
    assert_eq!(value("x = -9223372036854775808\n\nx"), "-9223372036854775808");

    // `U128.highest` is past `i128::MAX`, so it is held as the same BIT PATTERN and
    // reads back as -1. Every width operation is correct on it; only printing one that
    // large is not. `ponytail: the ceiling is now 128 bits rather than 64.`
    assert_eq!(value("x = 340282366920938463463374607431768211455\n\nx"), "-1");

    // Wider than an i128 is still refused, by name.
    let err = parse("x = 9999999999999999999999999999999999999999999\n\nx")
        .expect_err("should not parse");
    assert!(err.message.contains("does not fit in I128"), "got {:?}", err);
}

#[test]
fn a_tag_payload_may_end_with_a_comma() {
    assert_eq!(value("x = Pair(\n\t1,\n\t2,\n)\n\nx"), "Pair(1, 2)");
}

#[test]
fn a_record_field_may_hold_a_multi_parameter_function() {
    // `{ next_payload : _state, U64, U64 -> Try(…) }` is ONE field taking three
    // parameters, not three fields. The commas only mean parameters if an arrow
    // follows, which is what leaves `{ x : Bool, y : Bool }` alone.
    assert!(parse("N :: { f : I64, Str -> Bool }.{\n\tg : N -> N\n\tg = |n| n\n}\n\n42").is_ok());
    let both = parse("N :: { x : I64, y : Str }.{\n\tg : N -> N\n\tg = |n| n\n}\n\n42");
    assert!(both.is_ok(), "a plain two-field record still parses");
}

#[test]
fn a_file_may_be_nothing_but_declarations() {
    // Builtin.roc has no trailing expression, and neither does a module of pure
    // annotations. Its value is `{}`.
    assert_eq!(value("N :: [A].{\n\tf : N -> N\n\tf = |n| n\n}"), "{}");
}

#[test]
fn a_qualified_type_name_is_read() {
    // `dict_find_for_insert_from : List(Dict.DictBucket), … -> …`. An uppercase dotted
    // path parses as a nominal-qualified TAG in expression position, which the type
    // parser rejected — and because a rejected annotation is only skipped, the
    // signature was dropped in silence rather than reported.
    let annotated = "f : Dict.DictBucket -> U64\nf = |_x| \"nope\"\n\n42";
    let ast = parse(annotated).expect("a qualified type name parses");
    TypeChecker::new()
        .synth(&ast)
        .expect_err("the annotation is applied, so the Str body is a type error");
}

#[test]
fn a_grapheme_literal_is_a_pattern() {
    // roc has no character type, so `'"' => …` matches the number 34. Builtin.roc's
    // JSON scanner matches bytes that way.
    let src = "f = |b| match b {\n\t'\\\"' => 1\n\t'a' => 2\n\t_ => 0\n}\n\nf(97)";
    assert_eq!(value(src), "2");
}

#[test]
fn the_builtin_boundary_is_read_off_the_source() {
    // The contract P1 rests on: a member with a body is Roc, a member with only an
    // annotation is an intrinsic Rust must supply. Asserted on names rather than
    // counts, which move whenever the pinned nightly does.
    let read = rocflight::builtin::read();
    let find = |name: &str| read.iter().find(|m| m.name == name).expect("member is present");

    let dict = find("Dict");
    assert!(dict.error.is_none(), "Dict parses: {:?}", dict.error);
    // `insert` is written in Roc, so rocflight gets it for free once it loads.
    assert!(dict.defined.contains(&"Dict.insert"), "got {:?}", dict.defined);
    // `Str.concat` is annotation-only — roc lowers it to a low-level op, and so must we.
    assert!(find("Str").intrinsics.contains(&"Str.concat"));
    assert!(!find("Str").intrinsics.contains(&"Str.is_empty"), "is_empty has a body");

    // The low-level section sits OUTSIDE the `Builtin` nominal and is where the ops
    // Builtin.roc calls but never defines are declared.
    let low = find("(low level)");
    assert!(low.intrinsics.contains(&"list_get_unsafe"), "got {:?}", &low.intrinsics[..5]);
    assert!(low.intrinsics.contains(&"hasher_finish"));
}

// --- P2: loading the vendored module ------------------------------------------------

#[test]
fn a_member_loads_and_an_unknown_one_is_refused() {
    let loaded = rocflight::builtin::load(&["Hasher"]).expect("Hasher loads");
    assert_eq!(loaded.len(), 1);
    // `Hasher` is 16 annotation-only members, so what it contributes is the DECLARATION
    // of intrinsics, not definitions. They are qualified, so none are bare low-level.
    assert!(loaded[0].intrinsics.is_empty(), "got {:?}", loaded[0].intrinsics);

    let err = match rocflight::builtin::load(&["Nope"]) {
        Err(e) => e,
        Ok(_) => panic!("an unknown member should be refused"),
    };
    assert!(err.contains("no builtin member named"), "got {}", err);
}

#[test]
fn the_low_level_section_declares_bare_ops() {
    // These have no module — Builtin.roc calls them by a bare name — so the compiler
    // has to be told they are calls into Rust rather than undefined variables.
    let loaded = rocflight::builtin::load(&["(low level)"]).expect("the low-level section loads");
    assert!(loaded[0].intrinsics.contains(&"list_get_unsafe"), "got a list of {} names", loaded[0].intrinsics.len());
    assert!(loaded[0].intrinsics.iter().all(|n| !n.contains('.')), "bare names only");
}

#[test]
fn the_low_level_section_is_cut_to_what_the_other_members_reach() {
    // Alone, it is loaded whole: 153 bare ops. Beside `Dict` and `Set` it keeps only
    // the declarations their text reaches — a handful of ops and the `dict_*` helpers
    // — and the other 269 are never parsed.
    let whole = rocflight::builtin::load(&["(low level)"]).expect("loads");
    let cut = rocflight::builtin::load(&["(low level)", "Dict", "Set"]).expect("loads");
    let (whole, cut) = (&whole[0], &cut[0]);
    assert!(whole.intrinsics.len() > 100, "got {}", whole.intrinsics.len());
    assert!(cut.intrinsics.len() < 20, "got {:?}", cut.intrinsics);
    for op in ["list_get_unsafe", "list_set_unsafe", "hasher_finish"] {
        assert!(cut.intrinsics.contains(&op), "`Dict` reaches {op}: {:?}", cut.intrinsics);
    }
}

#[test]
fn low_level_ops_run() {
    use rocflight::eval::{call_builtin_values, Value};
    let call = |name: &str, mut args: Vec<Value>| {
        call_builtin_values("LowLevel", name, &mut args)
            .unwrap_or_else(|e| panic!("{}: {}", name, e))
    };
    let list = |ns: &[i128]| Value::list(ns.iter().map(|n| Value::Int(*n)).collect());

    assert_eq!(call("list_get_unsafe", vec![list(&[7, 8, 9]), Value::Int(1)]).to_string(), "8");
    assert_eq!(
        call("list_set_unsafe", vec![list(&[1, 2, 3]), Value::Int(0), Value::Int(9)]).to_string(),
        "[9, 2, 3]"
    );
    assert_eq!(
        call("list_swap_unsafe", vec![list(&[1, 2, 3]), Value::Int(0), Value::Int(2)]).to_string(),
        "[3, 2, 1]"
    );
    assert_eq!(
        call("list_append_unsafe", vec![list(&[1]), Value::Int(2)]).to_string(),
        "[1, 2]"
    );
    // `Hasher :: { state : U64 }`, and the digest IS the state.
    assert_eq!(
        call("hasher_finish", vec![Value::record(vec![("state", Value::Int(42))])]).to_string(),
        "42"
    );

    // "unsafe" means the caller proved the index; rocflight cannot elide Rust's bounds
    // check, so a bad index is a message and never a panic.
    let err = match call_builtin_values("LowLevel", "list_get_unsafe", &mut [list(&[1]), Value::Int(5)]) {
        Err(e) => e.message,
        Ok(_) => panic!("reading past the end should fail"),
    };
    assert!(err.contains("past the end"), "got {}", err);

    // An op Builtin.roc declares but Rust has not written yet names itself.
    let err = match call_builtin_values("LowLevel", "u8_from_str", &mut []) {
        Err(e) => e.message,
        Ok(_) => panic!("an unimplemented op should fail"),
    };
    assert!(err.contains("not implemented"), "got {}", err);
}

// --- P3: dispatch resolves by the receiver's type ------------------------------------

/// Run `src` the way `main.rs` does: checker first, and what it learned handed to the
/// compiler. `builtins` names the `Builtin.roc` members to load ahead of it.
fn run_with(src: &str, builtins: &[&str]) -> Result<String, String> {
    let desugared = Desugarer::new(src.to_string()).desugar().map_err(|e| e.to_string())?;
    let mut app = Parser::new(&desugared);
    let ast = app.parse_expr().map_err(|e| e.to_string())?;
    let loaded = rocflight::builtin::load(builtins)?;
    let mut checker = TypeChecker::new();
    checker.declare_nominal_literals(app.nominal_literals());
    checker.synth(&ast).map_err(|e| e.message)?;
    let unit = rocflight::vm::compile::Unit {
        prefix_modules: 0,
        modules: loaded
            .iter()
            .map(|l| rocflight::vm::compile::Module {
                ast: &l.ast,
                type_name: l.name,
                exposed: Vec::new(),
            })
            .collect(),
        app: &ast,
        entry: None,
        ingested: Vec::new(),
        integer_binops: checker.integer_binops(),
        dispatch_modules: checker.dispatch_modules(),
        binop_modules: checker.binop_modules(),
        dec_literals: checker.dec_literals(),
        fractional_literals: checker.fractional_literals(),
        parse_targets: checker.json_parse_targets(),
        collect_targets: checker.collect_targets(),
        f32_literals: checker.f32_literals(),
        u128_literals: checker.u128_literals(),
        default_sites: checker.default_sites(),
        nominal_defaults: Default::default(),
        missing_fields: checker.missing_fields(),
        run_expects: false,
        conversions: checker.literal_conversions(),
        numeral_texts: Default::default(),
        coerce_values: checker.coerce_values(),
        coerce_params: checker.coerce_params(),
        zero_sized_capacity: checker.zero_sized_capacity(),
        match_types: checker.match_types(),
        for_iter_calls: checker.for_iter_calls(),
        nominals: loaded
            .iter()
            .flat_map(|l| l.nominals.iter().cloned())
            .chain(app.nominals().iter().cloned())
            .collect(),
        opaque_nominals: app.opaque_nominals().to_vec(),
        intrinsics: loaded.iter().flat_map(|l| l.intrinsics.iter().copied()).collect(),
        test_mode: true,
        enclosing_owners: std::collections::HashMap::new(),
    };
    let program = std::rc::Rc::new(rocflight::vm::compile_unit(&unit)?);
    rocflight::vm::run(&program).map(|v| v.to_string()).map_err(|e| e.message)
}

#[test]
fn a_loaded_member_does_not_steal_a_method_name() {
    // `Stream` defines `map`. So does `List` — in Rust. Dispatch used to pick the only
    // top-level `map` it could see by NAME, so loading Stream made every `xs.map(f)`
    // call `Stream.map` and fail on a list. The receiver's type decides.
    let src = "xs : List(I64)\nxs = [1, 2, 3]\n\nxs.map(|n| n * 2)";
    assert_eq!(run_with(src, &[]).expect("plain"), "[2, 4, 6]");
    assert_eq!(run_with(src, &["Stream"]).expect("with Stream loaded"), "[2, 4, 6]");
}

#[test]
fn an_operator_does_not_reach_an_unrelated_is_eq() {
    // Every operator is a method — `a == b` is `a.is_eq(b)` — and a program that
    // defines ANY of them used to send every operator through a method search. One
    // `is_eq` in scope then answered for tuples and tags as well as its own type.
    let user = "Money :: { cents: I64 }.{\n    is_eq = |a, b| a.cents == b.cents\n}\n\n";

    // Its own type still dispatches: 5 and 5 are equal, 5 and 7 are not.
    assert_eq!(
        run_with(&format!("{}Money.{{ cents: 5 }} == Money.{{ cents: 5 }}", user), &[]).unwrap(),
        "True"
    );
    // A tuple is not a nominal and cannot own that method, so it compares structurally.
    assert_eq!(run_with(&format!("{}(1, \"x\") == (1, \"x\")", user), &[]).unwrap(), "True");
    assert_eq!(run_with(&format!("{}(1, \"x\") == (2, \"x\")", user), &[]).unwrap(), "False");
    // And neither do the types the checker can name.
    assert_eq!(run_with(&format!("{}1 + 2", user), &[]).unwrap(), "3.0");
    assert_eq!(run_with(&format!("{}\"a\" == \"a\"", user), &[]).unwrap(), "True");
}

#[test]
fn an_ambiguous_method_is_still_named_not_guessed() {
    // The receiver is a bare record, so only the checker could have known which type
    // was meant, and it does not. Both candidates are named rather than one picked.
    let src = "A :: { v: I64 }.{\n    show = |a| a.v.to_str()\n}\n\nB :: { v: I64 }.{\n    show = |b| b.v.to_str()\n}\n\nx = { v: 1 }\nx.show()";
    let err = run_with(src, &[]).expect_err("ambiguous");
    assert!(err.contains("ambiguous"), "got {}", err);
    assert!(err.contains("A.show") && err.contains("B.show"), "it should name both: {}", err);
}

// --- P3b: Builtin.roc's signatures are the type table --------------------------------



#[test]
fn a_type_roc_names_is_a_type_here() {
    // `Dict`, `Set`, `Iter`, `Hasher` are Roc's own, declared in `Builtin.roc`. They
    // used to come out of the type parser as a FRESH VARIABLE — the name thrown away —
    // which is why `fruit_dict : Dict(Str, U64)` had no type and `fruit_dict.get(k)`
    // reported "Cannot dispatch `get` on an unresolved type".
    assert_eq!(type_of("f : Dict(Str, U64) -> Dict(Str, U64)\nf = |d| d\n\nf"), "(Dict -> Dict)");
    assert_eq!(type_of("f : Set(Str) -> Set(Str)\nf = |s| s\n\nf"), "(Set -> Set)");
}

#[test]
fn a_builtin_method_has_the_type_roc_wrote_for_it() {
    // Not guessed from the method's name: `Dict.get : Dict(k, v), k -> Try(v,
    // [KeyNotFound])` is written down in the source roc itself compiles.
    let got = type_of("d : Dict(Str, U64)\nd = Dict.empty()\n\nd.get(\"a\")");
    assert!(got.contains("KeyNotFound"), "the declared error tag survives: {}", got);
    assert!(got.contains("Ok"), "and so does the Ok arm: {}", got);

    // And a plain one: `Dict.len : Dict(k, v) -> U64`, not a fresh variable.
    assert_eq!(type_of("d : Dict(Str, U64)\nd = Dict.empty()\n\nd.len()"), "U64");
}

#[test]
fn a_zero_argument_builtin_applies_its_unit_parameter() {
    // `Dict.empty : () -> Dict(k, v)`. Roc spells an empty parameter list `()`, which
    // IS the unit type, so calling it peels one arrow. Peeling none handed back the
    // function itself, and `Dict.empty().insert(k, v)` had nothing to dispatch on.
    assert_eq!(type_of("Dict.empty()"), "Dict");
    assert_eq!(type_of("Dict.empty().insert(\"a\", 1)"), "Dict");
}

#[test]
fn a_builtin_checks_its_arguments_too() {
    // Reading the result type off the end is not type checking. `Dict.with_capacity`
    // is declared `U64 -> Dict(k, v)`, so a Str is rejected here rather than surfacing
    // as nonsense at run time.
    assert_eq!(type_of("Dict.with_capacity(8)"), "Dict");
    let err = type_error("Dict.with_capacity(\"nope\")");
    assert!(
        err.contains("Str") && err.contains("U64"),
        "the mismatch should name both sides: {}",
        err
    );

    // What it does NOT do yet: a nominal's type ARGUMENTS are dropped, so `Dict(Str,
    // U64)` and `Dict(I64, Bool)` are one type here and an element's type is still a
    // variable. Inserting a Str into a `Set(U64)` is therefore accepted. Carrying the
    // arguments needs a parameterised nominal in `Type`; this records the gap so that
    // closing it has a test to flip.
    assert_eq!(type_of("s : Set(U64)\ns = Set.empty()\n\ns.insert(\"not a number\")"), "Set");
}

#[test]
fn a_users_own_method_still_wins() {
    // The table is a fallback, not an override: a name the program binds is the
    // program's. `Counter.show` here, not anything `Builtin.roc` says about `show`.
    let src = "Counter :: { n: I64 }.{\n    show : Counter -> Str\n    show = |c| c.n.to_str()\n}\n\nf : Counter -> Str\nf = |c| c.show()\n\nf";
    assert_eq!(type_of(src), "(Counter -> Str)");
}

// --- P4: Dict and Set run roc's own implementation ------------------------------------

#[test]
fn a_bare_true_is_a_boolean() {
    // `Bool : [True, False]`, so a bare `True` IS the boolean. roc runs `x = True` then
    // `if x { … }`; rocflight refused it with "Cannot unify [True, ..] with Bool", and
    // `Builtin.roc` writes every `return False` that way.
    // `1.0` and not `1`: nothing says what these numerals are, and roc defaults an
    // unconstrained one to `Dec`.
    assert_eq!(run_with("x = True\n\nif x { 1 } else { 2 }", &[]).unwrap(), "1.0");
    assert_eq!(run_with("x = False\n\nif x { 1 } else { 2 }", &[]).unwrap(), "2.0");
    // The qualified spelling still agrees.
    assert_eq!(run_with("if Bool.True { 1 } else { 2 }", &[]).unwrap(), "1.0");
}

#[test]
fn a_qualified_tag_needs_no_local_nominal() {
    // `Try.Ok(x)` is `Ok(x)`. Requiring the nominal to be declared in THIS file made
    // every tag `Builtin.roc` qualifies an "Unknown function".
    assert_eq!(run_with("Try.Ok(1)", &[]).unwrap(), "Ok(1.0)");
    assert_eq!(run_with("Undeclared.Dog(1)", &[]).unwrap(), "Dog(1.0)");
}

#[test]
fn a_nominal_backed_by_a_nominal_unifies_with_it() {
    // `Graph(a) :: Dict(a, List(a))` is a Dict wearing a name, and within a module roc
    // lets the backing through. Refusing it failed with "Dict and Graph are different
    // nominal types".
    let src = "G :: Dict(Str, U64).{\n    unwrap : G -> Dict(Str, U64)\n    unwrap = |g| g\n}\n\nG.unwrap";
    assert!(run_with(src, &[]).is_ok(), "a nominal satisfies its backing");
}

#[test]
fn the_numeric_width_operations_respect_their_width() {
    use rocflight::eval::{call_builtin_values, Value};
    let call = |module: &str, name: &str, args: Vec<i128>| {
        let mut args: Vec<Value> = args.into_iter().map(Value::Int).collect();
        call_builtin_values(module, name, &mut args)
            .unwrap_or_else(|e| panic!("{}.{}: {}", module, name, e))
            .to_string()
    };
    // The width comes from the MODULE: the same shift truncates differently.
    assert_eq!(call("U32", "shl_wrap", vec![1, 8]), "256");
    assert_eq!(call("U8", "shl_wrap", vec![1, 8]), "1"); // the count wraps too
    assert_eq!(call("U8", "shl_wrap", vec![200, 1]), "144"); // 400 truncated to 8 bits
    // Arithmetic shift carries the sign down; zero-fill does not.
    assert_eq!(call("I8", "shr_wrap", vec![-1, 7]), "-1");
    assert_eq!(call("U8", "shr_zf_wrap", vec![-1, 7]), "1");
    // A conversion reads its target width off the NAME, not the receiver.
    assert_eq!(call("U64", "to_u8_wrap", vec![257]), "1");
    assert_eq!(call("U8", "to_i8_wrap", vec![255]), "-1");
    assert_eq!(call("U32", "bitwise_and", vec![0xF0, 0x3C]), "48");

    // `highest` is the width's own bound: the value is an i128, so `U64.highest` is
    // held in full. (It used to saturate to i64::MAX, when the value was an i64.)
    assert_eq!(call("U32", "highest", vec![]), "4294967295");
    assert_eq!(call("U64", "highest", vec![]), u64::MAX.to_string());
    assert_eq!(call("I8", "lowest", vec![]), "-128");
    // The bit counts and the checked conversions read the width the same way.
    assert_eq!(call("U8", "count_leading_zero_bits", vec![1]), "7");
    assert_eq!(call("I8", "mod_by", vec![-7, 3]), "2");
    assert_eq!(call("U8", "to_i8_try", vec![200]), "Err(OutOfRange)");
    assert_eq!(call("U8", "plus_wrap", vec![255, 1]), "0");
}

#[test]
fn equal_values_hash_alike() {
    use rocflight::eval::{call_builtin_values, Value};
    let hash = |value: Value| {
        let empty = Value::record(vec![("state", Value::Int(0))]);
        let written =
            call_builtin_values("Str", "to_hash", &mut [value, empty]).expect("hashed");
        call_builtin_values("LowLevel", "hasher_finish", &mut [written])
            .expect("finished")
            .to_string()
    };
    // The contract a Dict rests on: `==` implies the same hash.
    assert_eq!(hash(rocflight::eval::str_value("ab")), hash(rocflight::eval::str_value("ab")));
    assert_ne!(hash(rocflight::eval::str_value("ab")), hash(rocflight::eval::str_value("ba")));
    assert_eq!(hash(Value::Int(7)), hash(Value::Int(7)));
    assert_ne!(hash(Value::Int(7)), hash(Value::Int(8)));
}

#[test]
fn a_dict_is_rocs_own_dict() {
    // Not a Rust HashMap behind a Roc-shaped facade: this is `Builtin.roc`'s open
    // addressing table, running here. The value shows its buckets.
    // A Str value, because a nominal's type ARGUMENTS are still dropped — see
    // `a_builtin_checks_its_arguments_too` — so `Dict(Str, I64)` would not reach the
    // numeral and it would default.
    let built = run_with("Dict.empty().insert(\"a\", \"b\")", &["(low level)", "Dict", "Set"])
        .expect("insert runs");
    assert!(built.starts_with("HashMap("), "got {}", built);
    assert!(built.contains("dist_and_fingerprint"), "with a real bucket table: {}", built);
    assert!(built.contains("entries: [(\"a\", \"b\")]"), "and the entry: {}", built);
}

#[test]
fn only_a_program_that_needs_a_dict_loads_one() {
    // Loading `Dict` means parsing it, `Set`, and the 2,305-line low-level section —
    // 17ms against a 3ms baseline. The only way to make a Dict is to name one.
    assert!(rocflight::builtin::needed_by("main! = |_| echo!(\"hi\")").is_empty());
    assert!(rocflight::builtin::needed_by("d = Dict.empty()").contains(&"Dict"));
    assert!(rocflight::builtin::needed_by("s = Set.empty()").contains(&"(low level)"));
    // `Box` carries `Try`, so a program using `Ok`/`Err` may reach it.
    assert!(rocflight::builtin::needed_by("f : Try(I64, [Bad])").contains(&"Box"));
}

// --- P3c: nominal identity ------------------------------------------------------------

#[test]
fn a_sibling_method_is_in_scope_unqualified() {
    // Inside `G :: … .{ … }`, roc lets one method call another by its bare name.
    // rocflight binds them as `G.wrap`, so `wrap(n)` was an undefined variable — and
    // before that a fresh type variable, which cost every caller its type.
    let src = "G :: { n: I64 }.{\n    wrap : I64 -> G\n    wrap = |n| G.{ n: n }\n\n    twice : I64 -> G\n    twice = |n| wrap(n * 2)\n\n    size : G -> I64\n    size = |g| g.n\n}\n\nG.twice(21).size()";
    assert_eq!(run_with(src, &[]).expect("sibling call"), "42");
}

#[test]
fn a_test_runs_after_the_whole_file_is_in_scope() {
    // An `expect` written ABOVE the binding it uses. `compile_unit` already ran these
    // last; the CHECKER walked the tree as written and saw the use before the binding,
    // so `later.size()` dispatched on an unresolved type.
    let src = "G :: { n: I64 }.{\n    size : G -> I64\n    size = |g| g.n\n}\n\nexpect {\n    actual = later.size()\n    actual == 7\n}\n\nlater : G\nlater = G.{ n: 7 }\n";
    assert!(run_with(src, &[]).is_ok(), "{:?}", run_with(src, &[]));
}

#[test]
fn a_nominals_shape_says_whose_method_a_value_can_have_meant() {
    // roc erases nominals, so a value cannot say which one it is — but its SHAPE can
    // rule one out, and that is enough. `same = |a, b| a == b` is polymorphic, so the
    // checker has only a variable and the choice falls to run time.
    let user = "Wrapped :: [Ok(I64), Err(Str)].{\n    is_eq = |a, b| Bool.False\n}\n\nsame = |a, b| a == b\n\n";

    // A tuple is not a `Wrapped`, so its `is_eq` must not answer — it compares
    // structurally, which is what roc prints.
    assert_eq!(run_with(&format!("{}same((1, \"x\"), (1, \"x\"))", user), &[]).unwrap(), "True");
    assert_eq!(run_with(&format!("{}same((1, \"x\"), (2, \"x\"))", user), &[]).unwrap(), "False");
    // An unrelated tag is ruled out the same way.
    assert_eq!(run_with(&format!("{}same(Green, Green)", user), &[]).unwrap(), "True");
    // A tag the nominal DOES declare reaches it, and that `is_eq` always says False.
    assert_eq!(run_with(&format!("{}same(Ok(1), Ok(1))", user), &[]).unwrap(), "False");
}

#[test]
fn a_graph_is_a_dict_wearing_a_name() {
    // The shape of P4's second example: a nominal over a Dict, destructured back to the
    // raw Dict inside its own methods.
    let src = "G(a) :: Dict(a, List(a)).{\n    from_list : List((a, List(a))) -> G(a)\n    from_list = |l| G.(Dict.from_list(l))\n\n    size : G(a) -> U64\n    size = |G.(d)| Dict.len(d)\n}\n\ng : G(Str)\ng = G.from_list([(\"A\", [\"B\"]), (\"B\", [])])\n\ng.size()";
    assert_eq!(run_with(src, &["(low level)", "Dict", "Set"]).expect("graph"), "2");
}

// ==========================================================================
// Static dispatch — phase 20.
//
// Verified against `roc` nightly-2026-09-03 before implementing:
//   * `receiver.method(args)` resolves through the receiver's TYPE, and the receiver
//     becomes the FIRST argument — which is why roc's builtins take their subject
//     first (`List.map(list, fn)`)
//   * dispatching on an unresolved type is an error in roc too: "trying to dispatch a
//     method named to_str on an unresolved type variable"
//   * `s.is_empty` is a field read; `s.is_empty()` is a method call. The parens are
//     the only difference.

// --- the basic form -------------------------------------------------------

#[test]
fn a_method_call_resolves_through_the_receivers_type() {
    assert_eq!(as_str("n : I64\nn = 42\nn.to_str()"), "42");
    assert_eq!(as_str("s : Str\ns = \"hi\"\nStr.inspect(s.is_empty())"), "False");
}

#[test]
fn any_numeric_width_dispatches_to_the_same_place() {
    // The interpreter keeps one integer representation, so `U8` and `I64` land in the
    // same builtin.
    assert_eq!(as_str("small : U8\nsmall = 7\nsmall.to_str()"), "7");
}

#[test]
fn the_receiver_becomes_the_first_argument() {
    // `xs.fold(0, f)` is `List.fold(xs, 0, f)`. Subtraction catches an argument swap.
    assert_eq!(as_str("xs : List(I64)\nxs = [1, 2]\nxs.fold(10, |a, x| a - x).to_str()"), "7");
}

#[test]
fn extra_arguments_follow_the_receiver() {
    assert_eq!(
        as_str("xs : List(I64)\nxs = [1, 2, 3]\nStr.inspect(xs.map(|x| x * 2))"),
        "[2, 4, 6]"
    );
}

#[test]
fn a_zero_argument_method_still_needs_parens() {
    assert_eq!(as_str("xs : List(I64)\nxs = [1, 2, 3]\nxs.len().to_str()"), "3");
}

// --- chaining -------------------------------------------------------------

#[test]
fn dispatch_chains() {
    // The result of one dispatch is itself a receiver, so its type has to be known.
    assert_eq!(as_str("xs : List(I64)\nxs = [1, 2, 3]\nxs.len().to_str()"), "3");
    assert_eq!(as_str("xs : List(I64)\nxs = [1, 2]\nxs.fold(0, |a, x| a + x).to_str()"), "3");
    assert_eq!(as_str("xs : List(I64)\nxs = [1, 2]\nxs.map(|x| x).len().to_str()"), "2");
}

#[test]
fn a_method_works_on_any_receiver_expression() {
    // Postfix, like field access: a call result or a field is a fine receiver.
    assert_eq!(as_str("mk = |n| [n]\nmk(1).len().to_str()"), "1");
    assert_eq!(as_str("r : { s: Str }\nr = { s: \"\" }\nStr.inspect(r.s.is_empty())"), "True");
}

// --- field versus method --------------------------------------------------

#[test]
fn parens_separate_a_method_call_from_a_field_read() {
    // `.f` reads a field; `.f()` calls a method. Only the parens differ.
    assert_eq!(as_str("r = { f: 1 }\nI64.to_str(r.f)"), "1");
    assert_eq!(as_str("xs : List(I64)\nxs = [1]\nxs.len().to_str()"), "1");
}

// --- errors ---------------------------------------------------------------

#[test]
fn dispatching_on_an_unresolved_type_is_rejected() {
    // roc rejects this too. It now says WHY in roc's own terms: `first` gives a
    // `Try(item, [ListWasEmpty])` — read off `Builtin.roc`'s signature rather than
    // guessed — and a Try has no `to_str`. The older, vaguer "unresolved receiver" was
    // all this could say before the result type was known.
    let err = type_error("x = []\nx.first().to_str()");
    assert!(
        err.contains("unresolved") || err.contains("does not have it"),
        "error should explain why the dispatch cannot work, got {}",
        err
    );
}

#[test]
fn dispatch_inside_an_unannotated_lambda_is_deferred() {
    // A known ceiling. roc infers a lambda's parameter types from its CALL SITES; this
    // checker synthesises the body once, before any call site is seen, so a dispatch on
    // a parameter has nothing to resolve yet and falls back to the builtin table
    // instead of refusing. The call still evaluates correctly.
    assert_eq!(as_str("show = |x| x.to_str()\nshow(7)"), "7");
}

#[test]
fn an_unknown_method_names_the_module_it_looked_in() {
    let err = eval_error("n : I64\nn = 1\nn.nope()");
    assert!(err.contains("I64.nope"), "got {}", err);
}

#[test]
fn a_record_receiver_has_no_module_to_dispatch_on() {
    // Values carry no nominal wrapper, so there is nothing to look a method up in.
    let err = eval_error("r : { x: I64 }\nr = { x: 1 }\nr.nope()");
    assert!(err.contains("Cannot dispatch"), "got {}", err);
}

// --- the hole this work exposed -------------------------------------------

#[test]
fn expressions_inside_interpolation_are_type_checked() {
    // `StrInterp` used to synthesise as Str WITHOUT checking its parts, so any error
    // inside `${...}` went unreported — which is what hid broken chained dispatch in
    // this project's own golden pair.
    let err = type_error(r#""v=${1 + "s"}""#);
    assert!(
        err.contains("Cannot unify") || err.contains("A number cannot be used as Str"),
        "got {}",
        err
    );
}
