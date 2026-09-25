//! The language itself: literals, operators, and every form built on them.
//!
//! One file per feature was one CRATE per feature. The sections below are those
//! files, verbatim apart from the helpers they each redefined — see `common/mod.rs`.

mod common;
use crate::common::*;

// ==========================================================================
// `if` / `else` — phase 6.
//
// Verified against `roc` nightly-2026-09-03 before implementing:
//   * `if` is an EXPRESSION, and `else` is OPTIONAL: `if cond { … }` parses and has
//     the type `{}`. Re-checked against nightly-2026-09-03 on 2026-09-16 — using the
//     result is a type error against `{}`, never a parse error, which is what lets a
//     bare `if` stand as a statement.
//   * the condition must be a Bool: "This if condition must evaluate to a Bool".
//   * `else if` is not a separate form — it nests.
//   * braced branches are real blocks, so they may bind names.

#[test]
fn one_line_if_picks_a_branch() {
    assert_eq!(as_str(r#"if 1 == 1 "yes" else "no""#), "yes");
    assert_eq!(as_str(r#"if 1 == 2 "yes" else "no""#), "no");
}

#[test]
fn condition_needs_no_parens_but_accepts_them() {
    assert_eq!(as_str(r#"if (1 == 1) "yes" else "no""#), "yes");
}

#[test]
fn branches_may_be_blocks() {
    // A braced branch is a block, not a record: it can bind names and its value is
    // the final expression.
    assert_eq!(
        as_str("if 1 == 1 {\n    label = \"five\"\n    label\n} else {\n    \"other\"\n}"),
        "five"
    );
}

#[test]
fn else_if_chains_nest() {
    let src = r#"if 1 == 3 "three" else if 1 == 1 "one" else "other""#;
    assert_eq!(as_str(src), "one");

    // `else if` is an If whose otherwise is another If — no separate node.
    let ast = parse(src).unwrap();
    match ast {
        rocflight::ast::Expr::If { otherwise, .. } => assert!(
            matches!(*otherwise, rocflight::ast::Expr::If { .. }),
            "else-if should nest an If, got {}",
            otherwise
        ),
        other => panic!("expected If, got {}", other),
    }
}

#[test]
fn else_if_falls_through_to_the_last_branch() {
    assert_eq!(
        as_str(r#"if 1 == 3 "three" else if 1 == 4 "four" else "other""#),
        "other"
    );
}

#[test]
fn only_the_taken_branch_is_evaluated() {
    // The untaken branch divides by zero. If both branches were evaluated this
    // would error instead of returning a value.
    assert_eq!(as_str(r#"if 1 == 1 "safe" else I64.to_str(1 // 0)"#), "safe");
    assert_eq!(as_str(r#"if 1 == 2 I64.to_str(1 // 0) else "safe""#), "safe");
}

#[test]
fn a_missing_else_branch_is_unit() {
    // Not a parse error: roc accepts `if cond { … }` and gives the absent branch the
    // type `{}`.
    let ast = parse(r#"if 1 == 1 { 2 } "#).expect("a bare if parses");
    assert_eq!(ast.to_string(), "if (1 == 1) 2 else {}");

    // Using its value is still rejected, because `{}` does not unify with the taken
    // branch — a TYPE error, which is exactly where roc reports it too ("The value's
    // type, which does not have a method named from_numeral, is: {}").
    let err = TypeChecker::new()
        .synth(&parse(r#"if 1 == 1 { 2 } "#).unwrap())
        .expect_err("a bare if with a value does not check");
    assert!(err.message.contains("{}"), "got {:?}", err);

    // It earns its keep as a STATEMENT, where both branches are `{}`. This is the
    // shape `Builtin.roc` uses for a guarded assignment inside a loop.
    let src = "f = |n| {\n\tvar $t = 0\n\tif n > 0 {\n\t\t$t = 1\n\t}\n\t$t\n}\n\nf(5)";
    assert_eq!(eval(src).to_string(), "1");
}

#[test]
fn non_bool_condition_is_a_type_error() {
    let ast = parse(r#"if 1 "yes" else "no""#).unwrap();
    assert!(
        TypeChecker::new().synth(&ast).is_err(),
        "a numeric condition should not type check"
    );
}

#[test]
fn mismatched_branches_are_a_type_error() {
    // Both branches must agree: the if has one type.
    let ast = parse(r#"if 1 == 1 "str" else 42"#).unwrap();
    assert!(
        TypeChecker::new().synth(&ast).is_err(),
        "Str vs I64 branches should not type check"
    );
}

#[test]
fn if_works_as_a_call_argument() {
    assert_eq!(as_str(r#"Str.inspect(if 1 == 1 "a" else "b")"#), "\"a\"");
}

#[test]
fn if_nests_inside_a_branch() {
    assert_eq!(
        as_str(r#"if 20 > 0 (if 20 > 10 "big" else "small") else "neg""#),
        "big"
    );
}

#[test]
fn keyword_boundaries_are_respected() {
    // `iffy` and `elsewhere` are ordinary identifiers, not `if` / `else`.
    assert_eq!(as_str("iffy = \"name\"\niffy"), "name");
    assert_eq!(as_str("elsewhere = \"name\"\nelsewhere"), "name");
}

#[test]
fn call_requires_no_space_before_the_paren() {
    // roc rejects `f (1)`, and that rule is load-bearing: it lets a parenthesised
    // expression follow an operand without being read as a call on it.
    // `n > 0 (…)` must parse as a comparison followed by a separate group.
    let ast = parse("f = |x| x\nn = 1\nif n > 0 (if n > 10 \"big\" else \"small\") else \"neg\"")
        .expect("grouped expression after an operand should parse");
    assert!(format!("{}", ast).contains("if"), "expected an if, got {}", ast);
}

// ==========================================================================
// `match` — phase 11.
//
// Verified against `roc` nightly-2026-09-03 before implementing:
//   * arms are newline-separated; a comma between them is allowed but optional
//   * roc REQUIRES the arms to be exhaustive
//   * `_` is the wildcard; patterns nest to any depth; `A | B` shares one body
//   * a guard runs after its pattern matches, with the pattern's bindings in scope
//
// List patterns (`[]`, `[x, ..]`, `[1, .. as tail]`) are not covered: they need
// lists, which are a later phase.

// --- patterns -------------------------------------------------------------

#[test]
fn tag_patterns_select_an_arm() {
    let m = |c: &str| format!("match {} {{ Red => \"r\" Green => \"g\" Blue => \"b\" }}", c);
    assert_eq!(as_str(&m("Red")), "r");
    assert_eq!(as_str(&m("Green")), "g");
    assert_eq!(as_str(&m("Blue")), "b");
}

#[test]
fn literal_patterns_match_values() {
    let m = |n: &str| format!("match {} {{ 1 => \"one\" 2 => \"two\" _ => \"many\" }}", n);
    assert_eq!(as_str(&m("1")), "one");
    assert_eq!(as_str(&m("2")), "two");
    assert_eq!(as_str(&m("9")), "many");

    let s = |v: &str| format!("match {} {{ \"a\" => \"A\" _ => \"?\" }}", v);
    assert_eq!(as_str(&s("\"a\"")), "A");
    assert_eq!(as_str(&s("\"z\"")), "?");
}

#[test]
fn wildcard_matches_anything_and_binds_nothing() {
    assert_eq!(as_str(r#"match Red { _ => "anything" }"#), "anything");
}

#[test]
fn underscore_prefixed_name_is_a_binding_not_a_wildcard() {
    // `_` alone is the wildcard; `_unused` is an ordinary binding that documents
    // being unused, so it still binds.
    assert_eq!(as_str(r#"match "v" { _unused => _unused }"#), "v");
}

#[test]
fn binding_pattern_captures_the_value() {
    assert_eq!(as_str(r#"match "captured" { x => x }"#), "captured");
}

#[test]
fn alternatives_share_one_body() {
    let m = |c: &str| format!("match {} {{ Red | Orange => \"warm\" Blue => \"cool\" }}", c);
    assert_eq!(as_str(&m("Red")), "warm");
    assert_eq!(as_str(&m("Orange")), "warm");
    assert_eq!(as_str(&m("Blue")), "cool");
}

#[test]
fn payload_patterns_bind_positionally() {
    assert_eq!(
        as_str(r#"match Foo(42, "label") { Foo(n, s) => "${s}=${I64.to_str(n)}" Bar => "bar" }"#),
        "label=42"
    );
}

#[test]
fn patterns_nest() {
    let m = |v: &str| format!("match {} {{ Wrap(Inner(s)) => s Wrap(Empty) => \"empty\" Bare => \"bare\" }}", v);
    assert_eq!(as_str(&m(r#"Wrap(Inner("deep"))"#)), "deep");
    assert_eq!(as_str(&m("Wrap(Empty)")), "empty");
    assert_eq!(as_str(&m("Bare")), "bare");
}

#[test]
fn payload_arity_must_match_the_pattern() {
    // `Foo(a)` must not match a two-payload `Foo`.
    assert!(
        eval_error(r#"match Foo(1, 2) { Foo(a) => "one" }"#).contains("No match arm"),
        "a one-arg pattern should not match a two-payload tag"
    );
}

// --- order and guards -----------------------------------------------------

#[test]
fn arms_are_tried_in_order() {
    // The wildcard is last, so the specific arm wins. Reversed, the wildcard would.
    assert_eq!(as_str(r#"match 1 { 1 => "specific" _ => "general" }"#), "specific");
    assert_eq!(as_str(r#"match 1 { _ => "general" 1 => "specific" }"#), "general");
}

#[test]
fn a_false_guard_falls_through_to_the_next_arm() {
    let m = |n: &str| format!("match {} {{ 0 => \"zero\" x if x < 0 => \"neg\" _ => \"pos\" }}", n);
    assert_eq!(as_str(&m("0")), "zero");
    assert_eq!(as_str(&m("0 - 5")), "neg");
    assert_eq!(as_str(&m("5")), "pos");
}

#[test]
fn a_guard_sees_the_patterns_bindings() {
    assert_eq!(as_str(r#"match 7 { x if x > 5 => "big" _ => "small" }"#), "big");
}

#[test]
fn a_non_bool_guard_is_an_error() {
    assert!(
        eval_error(r#"match 1 { x if x => "yes" _ => "no" }"#).contains("Bool"),
        "a non-Bool guard should say so"
    );
}

// --- scoping --------------------------------------------------------------

#[test]
fn arm_bindings_do_not_leak_out_of_the_match() {
    // `inner` exists only inside its arm.
    let src = "outer = \"kept\"\nfirst = match Wrap(\"x\") { Wrap(inner) => inner }\nouter";
    assert_eq!(as_str(src), "kept");
}

#[test]
fn a_partial_nested_match_leaves_no_bindings_behind() {
    // `Pair(a, Inner(b))` binds `a`, then fails on the second element. `a` must not
    // leak into the next arm, or the fallback would see a stale value.
    let src = r#"match Pair("first", Other) { Pair(a, Inner(b)) => b _ => "fallback" }"#;
    assert_eq!(as_str(src), "fallback");
}

// --- exhaustiveness -------------------------------------------------------

#[test]
fn no_matching_arm_is_a_runtime_error() {
    // roc rejects a non-exhaustive match at compile time. The interpreter cannot:
    // it never sees the type annotations, so it has no idea what the full union is.
    // It reports the failure at run time rather than returning something wrong.
    // ponytail: becomes a compile-time check once annotations reach the AST.
    assert!(
        eval_error(r#"match Blue { Red => "r" Green => "g" }"#).contains("No match arm"),
        "an unmatched value should be reported"
    );
}

// --- as an expression -----------------------------------------------------

#[test]
fn match_is_an_expression() {
    // It has a value, so it can sit anywhere an expression can.
    assert_eq!(
        as_str(r#"Str.inspect(match Red { Red => "r" _ => "o" })"#),
        "\"r\""
    );
}

#[test]
fn arm_bodies_may_be_blocks() {
    assert_eq!(
        as_str("match Red {\n    Red => {\n        label = \"block\"\n        label\n    }\n    _ => \"other\"\n}"),
        "block"
    );
}

#[test]
fn commas_between_arms_are_optional() {
    assert_eq!(as_str(r#"match Red { Red => "r", Green => "g" }"#), "r");
    assert_eq!(as_str(r#"match Red { Red => "r" Green => "g" }"#), "r");
}

#[test]
fn match_keyword_boundary_is_respected() {
    assert_eq!(as_str("matcher = \"name\"\nmatcher"), "name");
}

// ==========================================================================
// Record literals, Bool, field access, and the operators they unblocked.
//
// Behaviour here was verified against `roc` nightly-2026-09-03 before being
// implemented, not inferred.

// --- record literals -------------------------------------------------------

#[test]
fn record_literal_keeps_source_order() {
    match eval("{ zebra: 1, apple: 2 }") {
        Value::Record(fields) => {
            let names: Vec<&str> = fields.iter().map(|(n, _)| *n).collect();
            // Only Str.inspect sorts; the value itself keeps source order.
            assert_eq!(names, vec!["zebra", "apple"]);
        }
        other => panic!("expected Record, got {:?}", other),
    }
}

#[test]
fn record_allows_trailing_comma() {
    match eval("{ x: 1, y: 2, }") {
        Value::Record(fields) => assert_eq!(fields.len(), 2),
        other => panic!("expected Record, got {:?}", other),
    }
}

#[test]
fn record_field_values_are_full_expressions() {
    assert_eq!(as_str("r = { sum: 2 + 3 * 4 }\nI64.to_str(r.sum)"), "14");
}

#[test]
fn empty_braces_are_unit_not_a_record() {
    assert!(matches!(eval("{}"), Value::Unit));
}

// --- record vs block disambiguation ---------------------------------------

#[test]
fn braces_with_annotated_binding_are_a_block_not_a_record() {
    // `n : I64` has a space before the colon, so it is an annotation inside a
    // block. A record field is `n: value`, with no space. Conflating the two would
    // silently turn a block into a one-field record.
    assert_eq!(as_str("f = |_| {\n    n : I64\n    n = 21\n    I64.to_str(n * 2)\n}\nf(0)"), "42");
}

#[test]
fn braces_with_a_field_are_a_record_not_a_block() {
    match eval("{ n: 21 }") {
        Value::Record(fields) => assert_eq!(fields.len(), 1),
        other => panic!("expected Record, got {:?}", other),
    }
}

// --- field access ---------------------------------------------------------

#[test]
fn field_access_chains() {
    assert_eq!(as_str("r = { a: { b: 7 } }\nI64.to_str(r.a.b)"), "7");
}

#[test]
fn uppercase_receiver_is_a_module_not_a_field() {
    // `Str.inspect` must stay a module member; only lowercase receivers are fields.
    assert_eq!(as_str("Str.inspect(Bool.True)"), "True");
}

#[test]
fn missing_field_is_an_error() {
    // Caught by the type checker: `r` is bound to a closed record without `nope`.
    // The VM no longer checks — an absent field there is an optional one left out.
    let desugared = Desugarer::new("r = { a: 1 }\nr.nope".to_string()).desugar().unwrap();
    let mut parser = Parser::new(&desugared);
    let ast = parser.parse_expr().unwrap();
    let mut checker = rocflight::types::checker::TypeChecker::new();
    assert!(checker.synth(&ast).is_err(), "missing field should be a type error");
}

#[test]
fn missing_field_on_a_literal_is_caught_by_the_type_checker() {
    // When the receiver is the literal itself, its type IS known, so the checker
    // rejects it before evaluation.
    let desugared = Desugarer::new("{ a: 1 }.nope".to_string()).desugar().unwrap();
    let mut parser = Parser::new(&desugared);
    let ast = parser.parse_expr().unwrap();
    assert!(TypeChecker::new().synth(&ast).is_err(), "missing field should fail");
}

// --- Str.inspect ----------------------------------------------------------

#[test]
fn inspect_sorts_record_fields_alphabetically() {
    // Verified against roc: `{ zebra: .., apple: .., mango: .. }` inspects sorted.
    assert_eq!(
        as_str("Str.inspect({ zebra: Bool.True, apple: Bool.False, mango: Bool.True })"),
        "{ apple: False, mango: True, zebra: True }"
    );
}

#[test]
fn inspect_sorts_nested_records_too() {
    assert_eq!(
        as_str("Str.inspect({ outer: Bool.True, inner: { y: Bool.False, x: Bool.True } })"),
        "{ inner: { x: True, y: False }, outer: True }"
    );
}

#[test]
fn inspect_quotes_strings_and_names_bools() {
    assert_eq!(as_str("Str.inspect(\"hi\")"), "\"hi\"");
    assert_eq!(as_str("Str.inspect(Bool.False)"), "False");
    assert_eq!(as_str("Str.inspect({})"), "{}");
}

// --- operators unblocked by the above -------------------------------------

#[test]
fn comparisons_yield_bool() {
    assert!(as_bool("1 < 2"));
    assert!(!as_bool("2 < 1"));
    assert!(as_bool("2 <= 2"));
    assert!(as_bool("\"a\" == \"a\""));
}

#[test]
fn and_or_keywords_work_like_the_symbols() {
    assert!(!as_bool("Bool.True and Bool.False"));
    assert!(as_bool("Bool.True or Bool.False"));
}

#[test]
fn keyword_boundary_is_respected() {
    // Without a boundary check, `android` would parse as `and` + `roid`.
    assert_eq!(as_str("android = \"phone\"\nandroid"), "phone");
    assert_eq!(as_str("organ = \"pipe\"\norgan"), "pipe");
}

#[test]
fn prefix_bang_is_logical_not() {
    // Unrelated to the `!` that ends an effectful name.
    assert!(!as_bool("!Bool.True"));
    assert!(as_bool("Bool.not(Bool.False)"));
}

#[test]
fn int_div_and_rem() {
    assert_eq!(as_str("I64.to_str(7 // 2)"), "3");
    assert_eq!(as_str("I64.to_str(7 % 2)"), "1");
    // `//` must not be read as two `/` operators.
    assert_eq!(as_str("I64.to_str(100 // 10 // 2)"), "5");
}

#[test]
fn int_div_by_zero_is_an_error() {
    let desugared = Desugarer::new("7 // 0".to_string()).desugar().unwrap();
    let mut parser = Parser::new(&desugared);
    let ast = parser.parse_expr().unwrap();
    assert!(rocflight::vm::eval(&ast).is_err(), "division by zero should fail");
}

#[test]
fn field_access_works_on_any_receiver() {
    // Field access is postfix, so the receiver can be a literal or a call result,
    // not only a bare identifier.
    assert_eq!(as_str("I64.to_str({ a: 7 }.a)"), "7");
    assert_eq!(as_str("mk = |n| { v: n }\nI64.to_str(mk(7).v)"), "7");
}

// ==========================================================================
// Record update and destructuring — the rest of phase 09.
//
// Verified against `roc` nightly-2026-09-03:
//   * the update spelling is `{ ..base, field: value }`. `{ base & field: value }` is
//     REJECTED — it appears in the reference file only inside a commented-out TODO
//   * `{ x, y } = r` destructures; inside a PATTERN a bare name means `x: x`
//   * in a record LITERAL a bare name is NOT punning: `{ name }` is a block whose
//     value is `name`
//   * `..rest` in a pattern binds every field not named, so naming one `_` and
//     capturing the rest removes it

// --- update ---------------------------------------------------------------

#[test]
fn an_update_replaces_the_named_fields() {
    assert_eq!(
        as_str("p = { name: \"ada\", age: 30 }\nStr.inspect({ ..p, age: 31 })"),
        "{ age: 31, name: \"ada\" }"
    );
}

#[test]
fn unnamed_fields_are_carried_over() {
    assert_eq!(
        as_str("p = { a: 1, b: 2, c: 3 }\nStr.inspect({ ..p, b: 9 })"),
        "{ a: 1, b: 9, c: 3 }"
    );
}

#[test]
fn the_base_is_not_mutated() {
    // Records are values; an update builds a new one.
    assert_eq!(
        as_str("p = { a: 1 }\nq = { ..p, a: 2 }\nStr.inspect(p)"),
        "{ a: 1 }"
    );
}

#[test]
fn several_fields_may_be_updated_at_once() {
    assert_eq!(
        as_str("p = { a: 1, b: 2 }\nStr.inspect({ ..p, a: 9, b: 8 })"),
        "{ a: 9, b: 8 }"
    );
}

#[test]
fn an_update_of_a_nominal_record_is_the_nominal() {
    // Each update answers a `P`, so the next one sees every field: three deep, the
    // result is still a whole `P`, not a record holding the last update's fields.
    let src = "P := { x : I64, y : I64, z : I64 }\n\
               bump : P, I64 -> P\nbump = |p, a| { ..{ ..{ ..p, x: a }, y: a }, z: a }\n\
               start : P\nstart = P.{ x: 1, y: 2, z: 3 }\nq = bump(start, 10)\nq.x + q.y + q.z";
    assert_eq!(value(src), "30");
}

#[test]
fn updating_a_nominal_record_by_a_field_it_lacks_is_an_error() {
    assert!(
        !accepts("P := { a : I64 }\np : P\np = P.{ a: 1 }\n{ ..p, nope: 2 }"),
        "adding a field to a nominal record via update should fail"
    );
}

#[test]
fn updating_a_field_the_record_lacks_is_an_error() {
    // An update cannot ADD a field.
    assert!(
        !accepts("p : { a: I64 }\np = { a: 1 }\n{ ..p, nope: 2 }"),
        "adding a field via update should fail"
    );
}

#[test]
fn updating_a_non_record_is_an_error() {
    let err = eval_error("x = 5\n{ ..x, a: 1 }");
    assert!(err.contains("not a record"), "got {}", err);
}

#[test]
fn an_update_is_still_distinguished_from_a_block() {
    // `{ ..base, ... }` starts with `..`, which no block statement does.
    assert_eq!(as_str("p = { a: 1 }\nStr.inspect({ ..p, a: 2 })"), "{ a: 2 }");
    // A real block still works.
    assert_eq!(as_str("f = |_| {\n    x = 1\n    x\n}\nI64.to_str(f(0))"), "1");
}

// --- destructuring --------------------------------------------------------

#[test]
fn a_record_destructuring_binds_its_fields() {
    assert_eq!(
        as_str("f = |_| {\n    { name, age } = { name: \"ada\", age: 30 }\n    \"${name}${I64.to_str(age)}\"\n}\nf(0)"),
        "ada30"
    );
}

#[test]
fn a_field_may_be_renamed_while_destructuring() {
    assert_eq!(
        as_str("f = |_| {\n    { name: who } = { name: \"ada\" }\n    who\n}\nf(0)"),
        "ada"
    );
}

#[test]
fn destructuring_works_at_the_top_level() {
    assert_eq!(as_str("p = { a: 1, b: 2 }\n{ a, b } = p\nI64.to_str(a + b)"), "3");
}

// --- ..rest ---------------------------------------------------------------

#[test]
fn rest_collects_the_fields_not_named() {
    assert_eq!(
        as_str("f = |p| {\n    { email: _, ..rest } = p\n    Str.inspect(rest)\n}\nf({ name: \"ada\", age: 30, email: \"e\" })"),
        "{ age: 30, name: \"ada\" }"
    );
}

#[test]
fn rest_may_be_empty() {
    assert_eq!(
        as_str("f = |p| {\n    { a: _, ..rest } = p\n    Str.inspect(rest)\n}\nf({ a: 1 })"),
        "{}"
    );
}

#[test]
fn named_fields_still_bind_alongside_rest() {
    assert_eq!(
        as_str("f = |p| {\n    { a, ..rest } = p\n    \"${I64.to_str(a)}${Str.inspect(rest)}\"\n}\nf({ a: 1, b: 2 })"),
        "1{ b: 2 }"
    );
}

// --- the literal/pattern asymmetry ----------------------------------------

#[test]
fn a_bare_name_puns_in_a_pattern_but_not_in_a_literal() {
    // In a PATTERN, `{ name }` means `{ name: name }`.
    assert_eq!(
        as_str("f = |_| {\n    { name } = { name: \"ada\" }\n    name\n}\nf(0)"),
        "ada"
    );
    // In an EXPRESSION, `{ name }` is a block whose value is `name` — verified against
    // roc, which prints the string rather than a record.
    assert_eq!(as_str("name = \"ada\"\nStr.inspect({ name })"), "\"ada\"");
}

// ==========================================================================
// Tuples — phase 10.
//
// Verified against `roc` nightly-2026-09-03 before implementing:
//   * `(1)` is a parenthesised expression, NOT a one-tuple
//   * `.0` is zero-based and works on any receiver, including a call result
//   * `Str.inspect` renders `("Roc", 1)` and recurses
//   * destructuring works both inside a block and at the top level

// --- literals -------------------------------------------------------------

#[test]
fn tuple_literals_hold_mixed_types() {
    assert_eq!(as_str(r#"Str.inspect(("Roc", 1))"#), r#"("Roc", 1)"#);
    assert_eq!(as_str("Str.inspect((1, 2, 3))"), "(1, 2, 3)");
    // A trailing comma is allowed.
    assert_eq!(as_str("Str.inspect((1, 2,))"), "(1, 2)");
}

#[test]
fn a_single_paren_is_grouping_not_a_one_tuple() {
    // roc has no one-tuple: `(1)` is just `1`.
    assert_eq!(as_str("I64.to_str((1))"), "1");
    assert_eq!(as_str("I64.to_str((2 + 3) * 4)"), "20");
}

#[test]
fn tuples_nest_and_inspect_recurses() {
    assert_eq!(as_str(r#"Str.inspect((1, ("a", 2)))"#), r#"(1, ("a", 2))"#);
    assert_eq!(as_str(r#"Str.inspect([(1, "a"), (2, "b")])"#), r#"[(1, "a"), (2, "b")]"#);
}

#[test]
fn tuple_equality_is_elementwise() {
    assert_eq!(as_str(r#"Str.inspect((1, "x") == (1, "x"))"#), "True");
    assert_eq!(as_str(r#"Str.inspect((1, "x") == (2, "x"))"#), "False");
}

// --- indexing -------------------------------------------------------------

#[test]
fn index_is_zero_based() {
    assert_eq!(as_str(r#"p = ("Roc", 1)
p.0"#), "Roc");
    assert_eq!(as_str(r#"p = ("Roc", 1)
I64.to_str(p.1)"#), "1");
}

#[test]
fn index_works_on_any_receiver() {
    // Postfix, like record field access.
    assert_eq!(as_str("I64.to_str((7, 8).1)"), "8");
    assert_eq!(as_str("mk = |n| (n, n + 1)\nI64.to_str(mk(5).1)"), "6");
}

#[test]
fn out_of_range_index_is_an_error() {
    // Caught by the type checker when the tuple's type is known here.
    let ast = parse("(1, 2).5").unwrap();
    assert!(
        TypeChecker::new().synth(&ast).is_err(),
        ".5 on a 2-tuple should not type check"
    );
}

#[test]
fn indexing_a_non_tuple_is_an_error() {
    let ast = parse("x = Red\nx.0").unwrap();
    let _ = TypeChecker::new().synth(&ast);
    assert!(rocflight::vm::eval(&ast).is_err(), "indexing a tag should fail");
}

// --- patterns -------------------------------------------------------------

#[test]
fn tuple_patterns_match_elementwise() {
    let f = |p: &str| format!(r#"f = |p| match p {{ (0, 0) => "origin" (x, 0) => "x-axis" _ => "other" }}
f({})"#, p);
    assert_eq!(as_str(&f("(0, 0)")), "origin");
    assert_eq!(as_str(&f("(3, 0)")), "x-axis");
    assert_eq!(as_str(&f("(1, 1)")), "other");
}

#[test]
fn tuple_patterns_require_matching_arity() {
    // A tuple pattern CONSTRAINS the scrutinee to that arity, so a 3-tuple cannot
    // reach it. roc agrees, from the other end: it infers `p : (a, b)` from the first
    // pattern and reports the wildcard as redundant.
    let err = type_error(r#"f = |p| match p { (a, b) => "two" _ => "other" }
f((1, 2, 3))"#);
    assert!(err.contains("unify"), "arity mismatch should be a type error, got {}", err);
}

// --- destructuring --------------------------------------------------------

#[test]
fn destructuring_inside_a_block() {
    assert_eq!(
        as_str(r#"f = |_| {
    (name, n) = ("Roc", 1)
    "${name}${I64.to_str(n)}"
}
f(0)"#),
        "Roc1"
    );
}

#[test]
fn destructuring_at_the_top_level() {
    // Must NOT be scoped: the continuation is the rest of the file.
    assert_eq!(as_str(r#"pair = ("Roc", 1)
(name, n) = pair
"${name}${I64.to_str(n)}""#), "Roc1");
}

#[test]
fn a_wildcard_element_binds_nothing() {
    assert_eq!(as_str(r#"pair = ("Roc", 1)
(_, n) = pair
I64.to_str(n)"#), "1");
}

#[test]
fn a_grouped_expression_statement_is_not_destructuring() {
    // A statement may legitimately start with `(`. The destructuring check has to
    // backtrack when no `=` follows.
    assert_eq!(as_str(r#"f = |_| {
    (1 + 2)
}
I64.to_str(f(0))"#), "3");
}

#[test]
fn refutable_top_level_pattern_is_rejected_clearly() {
    // roc allows `(1, b) = pair`; the interpreter does not, because a top-level
    // binding cannot be a match. The error should point at the block form.
    let err = parse("pair = (1, 2)\n(1, b) = pair\nb").expect_err("should be rejected");
    assert!(
        err.message.contains("match") || err.message.contains("top-level"),
        "error should explain the restriction, got {:?}",
        err.message
    );
}

// ==========================================================================
// Tag unions — phase 12.
//
// Verified against `roc` nightly-2026-09-03 before implementing:
//   * `Str.inspect` renders a bare tag as `Red` and a payload tag as `Foo(42, "hi")`
//   * tags compare by name, and by payload when they have one
//   * `Try(a, b)` is `[Ok(a), Err(b)]` under the hood
//   * `[Red, Green, ..]` openness applies to parameter positions, not to a value
//     binding: `c : [Red, Green, ..]` still rejects `c = Blue`

// --- construction ---------------------------------------------------------

#[test]
fn bare_tag_is_a_value_not_a_call() {
    match eval("Red") {
        Value::Tag(name, payload) => {
            assert_eq!(name, "Red");
            assert!(payload.is_empty(), "bare tag should have no payload");
        }
        other => panic!("expected Tag, got {:?}", other),
    }
}

#[test]
fn tag_can_carry_payloads() {
    match eval(r#"Foo(42, "hi")"#) {
        Value::Tag(name, payload) => {
            assert_eq!(name, "Foo");
            assert_eq!(payload.len(), 2);
        }
        other => panic!("expected Tag, got {:?}", other),
    }
}

// --- typing ---------------------------------------------------------------

#[test]
fn a_tag_literal_is_an_open_one_tag_union() {
    // Open — rendered with a trailing `..`. A tag expression says "at least Red", so
    // unification may add more tags. Only an annotation produces a CLOSED union.
    assert_eq!(defaulted_type_of("Red"), "[Red, ..]");
    assert_eq!(defaulted_type_of(r#"Foo(1, "a")"#), "[Foo(Dec, Str), ..]");
}

#[test]
fn branches_with_different_tags_join_into_a_union() {
    // The if's type is the union of its branches, not just the first one.
    assert_eq!(defaulted_type_of("if 1 == 1 Red else Green"), "[Green, Red, ..]");
}

#[test]
fn union_members_are_sorted_so_order_does_not_matter() {
    assert_eq!(
        defaulted_type_of("if 1 == 1 Red else if 1 == 2 Green else Blue"),
        "[Blue, Green, Red, ..]"
    );
    // Same tags written the other way round give the same type.
    assert_eq!(
        defaulted_type_of("if 1 == 1 Blue else if 1 == 2 Green else Red"),
        "[Blue, Green, Red, ..]"
    );
}

#[test]
fn the_same_tag_in_both_branches_does_not_duplicate() {
    assert_eq!(defaulted_type_of("if 1 == 1 Foo(1) else Foo(2)"), "[Foo(Dec), ..]");
}

#[test]
fn payload_arity_mismatch_is_a_type_error() {
    let err = TypeChecker::new()
        .synth(&build("if 1 == 1 Foo(1) else Foo(1, 2)"))
        .expect_err("differing payload arity should fail");
    assert!(
        err.message.contains("payload"),
        "error should mention payloads, got {:?}",
        err.message
    );
}

#[test]
fn payload_type_mismatch_is_a_type_error() {
    assert!(
        TypeChecker::new().synth(&build(r#"if 1 == 1 Foo(1) else Foo("s")"#)).is_err(),
        "I64 vs Str payload should fail"
    );
}

#[test]
fn ok_and_err_are_ordinary_tags() {
    // `Try(a, b)` is `[Ok(a), Err(b)]`, so nothing special is needed for them.
    assert_eq!(defaulted_type_of("Ok({})"), "[Ok({}), ..]");
    assert_eq!(defaulted_type_of(r#"Err("boom")"#), "[Err(Str), ..]");
}

// --- equality -------------------------------------------------------------

#[test]
fn bare_tags_compare_by_name() {
    assert_eq!(as_str("Str.inspect(Red == Red)"), "True");
    assert_eq!(as_str("Str.inspect(Red == Green)"), "False");
}

#[test]
fn payload_tags_compare_payloads_too() {
    assert_eq!(as_str("Str.inspect(Foo(1) == Foo(1))"), "True");
    assert_eq!(as_str("Str.inspect(Foo(1) == Foo(2))"), "False");
    assert_eq!(as_str(r#"Str.inspect(Foo(1, "a") == Foo(1, "a"))"#), "True");
}

#[test]
fn a_tag_never_equals_a_non_tag() {
    assert_eq!(as_str("Str.inspect(Red == 1)"), "False");
}

// --- rendering ------------------------------------------------------------

#[test]
fn inspect_matches_roc_for_tags() {
    assert_eq!(as_str("Str.inspect(Red)"), "Red");
    assert_eq!(as_str(r#"Str.inspect(Foo(1, "hi"))"#), "Foo(1, \"hi\")");
    assert_eq!(as_str(r#"Str.inspect(Wrap("x"))"#), "Wrap(\"x\")");
    // Nested: payloads are inspected recursively, so strings stay quoted.
    assert_eq!(as_str(r#"Str.inspect(Outer(Inner("y")))"#), "Outer(Inner(\"y\"))");
}

#[test]
fn tags_carry_records_and_records_carry_tags() {
    assert_eq!(
        as_str("Str.inspect(Wrap({ b: Bool.True, a: Red }))"),
        "Wrap({ a: Red, b: True })"
    );
}

// ==========================================================================
// Lists and their builtins — phase 15, plus the list patterns it unblocks in 11.
//
// Verified against `roc` nightly-2026-09-03 before implementing:
//   * `List.len` returns U64 — mixing it with I64 match arms is a type error
//   * argument order is `List.map(list, fn)` and `List.fold(list, initial, fn)`,
//     with the accumulator as the callback's FIRST parameter
//   * `Str.inspect` renders `[1, 2, 3]` and recurses: `[[1], [2, 3]]`
//   * `..` may sit at the end, middle, or start of a list pattern

// --- literals -------------------------------------------------------------

#[test]
fn list_literals_and_empty() {
    assert_eq!(as_str("Str.inspect([1, 2, 3])"), "[1, 2, 3]");
    assert_eq!(as_str("Str.inspect([])"), "[]");
    // A trailing comma is allowed.
    assert_eq!(as_str("Str.inspect([1, 2,])"), "[1, 2]");
}

#[test]
fn inspect_recurses_into_nested_lists() {
    assert_eq!(as_str("Str.inspect([[1], [2, 3]])"), "[[1], [2, 3]]");
    // Strings inside a list keep their quotes.
    assert_eq!(as_str(r#"Str.inspect(["a", "b"])"#), r#"["a", "b"]"#);
}

#[test]
fn lists_hold_any_value_kind() {
    assert_eq!(as_str("Str.inspect([Red, Green])"), "[Red, Green]");
    assert_eq!(as_str("Str.inspect([{ b: 1, a: 2 }])"), "[{ a: 2, b: 1 }]");
}

#[test]
fn list_equality_is_elementwise_and_length_sensitive() {
    assert_eq!(as_str("Str.inspect([1, 2] == [1, 2])"), "True");
    assert_eq!(as_str("Str.inspect([1, 2] == [2, 1])"), "False");
    assert_eq!(as_str("Str.inspect([1] == [1, 2])"), "False");
    assert_eq!(as_str("Str.inspect([] == [])"), "True");
}

// --- builtins -------------------------------------------------------------

#[test]
fn list_len_and_is_empty() {
    assert_eq!(as_str("I64.to_str(List.len([1, 2, 3]))"), "3");
    assert_eq!(as_str("I64.to_str(List.len([]))"), "0");
    assert_eq!(as_str("Str.inspect(List.is_empty([]))"), "True");
    assert_eq!(as_str("Str.inspect(List.is_empty([1]))"), "False");
}

#[test]
fn list_map_takes_the_list_first() {
    assert_eq!(as_str("Str.inspect(List.map([1, 2, 3], |x| x * 2))"), "[2, 4, 6]");
    assert_eq!(as_str("Str.inspect(List.map([], |x| x * 2))"), "[]");
}

#[test]
fn list_fold_accumulates_with_acc_first() {
    assert_eq!(as_str("I64.to_str(List.fold([1, 2, 3, 4], 0, |acc, x| acc + x))"), "10");
    // The initial value is returned untouched for an empty list.
    assert_eq!(as_str("I64.to_str(List.fold([], 99, |acc, x| acc + x))"), "99");
    // Order matters: subtraction would differ if acc and x were swapped.
    assert_eq!(as_str("I64.to_str(List.fold([1, 2], 10, |acc, x| acc - x))"), "7");
}

#[test]
fn unknown_list_builtin_is_reported() {
    let ast = parse("List.nope([1])").unwrap();
    let err = rocflight::vm::eval(&ast).expect_err("should fail");
    assert!(err.message.contains("List.nope"), "got {}", err.message);
}

// --- list patterns --------------------------------------------------------

#[test]
fn exact_length_list_patterns() {
    let f = |xs: &str| format!(r#"f = |xs| match xs {{ [] => "none" [x] => "one" [a, b] => "two" _ => "many" }}
f({})"#, xs);
    assert_eq!(as_str(&f("[]")), "none");
    assert_eq!(as_str(&f("[1]")), "one");
    assert_eq!(as_str(&f("[1, 2]")), "two");
    assert_eq!(as_str(&f("[1, 2, 3]")), "many");
}

#[test]
fn list_patterns_bind_elements() {
    assert_eq!(
        as_str(r#"f = |xs| match xs { [a, b] => I64.to_str(a + b) _ => "no" }
f([3, 4])"#),
        "7"
    );
}

#[test]
fn literal_elements_must_be_equal() {
    let f = |xs: &str| format!(r#"f = |xs| match xs {{ [1, 2] => "onetwo" [a, b] => "other" _ => "no" }}
f({})"#, xs);
    assert_eq!(as_str(&f("[1, 2]")), "onetwo");
    assert_eq!(as_str(&f("[9, 9]")), "other");
}

#[test]
fn rest_pattern_at_the_end() {
    let f = |xs: &str| format!(r#"f = |xs| match xs {{ [1, 2, ..] => "yes" _ => "no" }}
f({})"#, xs);
    assert_eq!(as_str(&f("[1, 2, 3, 4]")), "yes");
    // `..` matches zero elements too.
    assert_eq!(as_str(&f("[1, 2]")), "yes");
    assert_eq!(as_str(&f("[1]")), "no");
}

#[test]
fn rest_pattern_in_the_middle() {
    let f = |xs: &str| format!(r#"f = |xs| match xs {{ [2, .., 1] => "yes" _ => "no" }}
f({})"#, xs);
    assert_eq!(as_str(&f("[2, 8, 8, 1]")), "yes");
    assert_eq!(as_str(&f("[2, 1]")), "yes");
    assert_eq!(as_str(&f("[2, 8, 8, 9]")), "no");
}

#[test]
fn rest_pattern_at_the_start() {
    let f = |xs: &str| format!(r#"f = |xs| match xs {{ [.., 5] => "yes" _ => "no" }}
f({})"#, xs);
    assert_eq!(as_str(&f("[1, 2, 5]")), "yes");
    assert_eq!(as_str(&f("[5]")), "yes");
    assert_eq!(as_str(&f("[5, 1]")), "no");
}

#[test]
fn rest_can_bind_the_skipped_elements() {
    assert_eq!(
        as_str(r#"f = |xs| match xs { [9, .. as tail] => Str.inspect(tail) _ => "no" }
f([9, 4, 5])"#),
        "[4, 5]"
    );
    // Binding an empty middle gives an empty list, not a failure.
    assert_eq!(
        as_str(r#"f = |xs| match xs { [9, .. as tail] => Str.inspect(tail) _ => "no" }
f([9])"#),
        "[]"
    );
}

#[test]
fn only_one_rest_per_pattern() {
    let err = parse(r#"f = |xs| match xs { [.., 1, ..] => "x" _ => "y" }
f([1])"#)
        .expect_err("two `..` should be rejected");
    assert!(err.message.contains(".."), "got {:?}", err.message);
}

#[test]
fn a_list_pattern_does_not_match_a_non_list() {
    // A list pattern CONSTRAINS the scrutinee, so a tag cannot reach it — roc rejects
    // this the same way: "This argument has the type [Red, ..] but f needs ...".
    let err = type_error(r#"f = |v| match v { [a] => "list" _ => "other" }
f(Red)"#);
    assert!(err.contains("List"), "error should mention List, got {}", err);
}

// --- the apply refactor ---------------------------------------------------

#[test]
fn lambdas_still_work_in_both_call_positions() {
    // Both `eval` call sites now share one `apply`. A named binding...
    assert_eq!(as_str("f = |x| x\nf(\"direct\")"), "direct");
    // ...and a chained call, which goes through the other site.
    assert_eq!(as_str("mk = |_| |x| x\nmk(0)(\"chained\")"), "chained");
}

#[test]
fn calling_a_non_function_reports_the_value() {
    let ast = parse("f = |x| x\nList.map([1], 5)").unwrap();
    let err = rocflight::vm::eval(&ast).expect_err("should fail");
    assert!(err.message.contains("non-function"), "got {}", err.message);
}

// ==========================================================================
// Loops and mutable bindings — phase 16.
//
// Verified against `roc` nightly-2026-09-03 before implementing:
//   * `$` is PART of the identifier — `$sum` and `sum` are different names, the same
//     way a trailing `!` distinguishes an effectful name
//   * a plain binding cannot be reassigned; roc reports it as a redeclaration
//   * a loop's value is `{}` — it is a statement, not something you bind
//   * `break` leaves the nearest enclosing loop
//
// NOT implemented: `continue`. It CRASHES the roc compiler on nightly-2026-09-03
// ("Please report this issue at github.com/roc-lang/roc/issues"), so no golden pair
// can be written against it.

// --- var and assignment ---------------------------------------------------

#[test]
fn a_var_can_be_reassigned() {
    assert_eq!(
        value("f = |_| {\n    var $a = 5\n    $a = $a * 2\n    $a\n}\nf(0)"),
        "10"
    );
}

#[test]
fn the_dollar_is_part_of_the_name() {
    // `$x` and `x` are different names, so this reads the outer `x`, not the var.
    assert_eq!(
        value("f = |_| {\n    x = 1\n    var $x = 99\n    x\n}\nf(0)"),
        "1"
    );
}

#[test]
fn a_dollar_name_without_var_is_an_ordinary_binding() {
    // `$` carries no meaning of its own — roc accepts `$nope = 1` as a plain binding
    // and prints 1. Mutability comes from `var`, not from the sigil.
    assert_eq!(value("f = |_| {\n    $nope = 1\n    $nope\n}\nf(0)"), "1");
}

#[test]
fn a_var_without_a_dollar_is_still_reassignable() {
    assert_eq!(
        value("f = |xs| {\n    var sum = 0\n    for n in xs {\n        sum = sum + n\n    }\n    sum\n}\nf([1, 2, 3])"),
        "6"
    );
}

// --- for ------------------------------------------------------------------

#[test]
fn a_for_loop_accumulates_into_a_var() {
    // The body runs in its own scope, so the assignment has to UPDATE the outer var
    // rather than bind a new one — otherwise the sum is lost when the scope pops.
    assert_eq!(
        value("f = |xs| {\n    var $sum = 0\n    for n in xs {\n        $sum = $sum + n\n    }\n    $sum\n}\nf([1, 2, 3, 4])"),
        "10"
    );
}

#[test]
fn an_empty_list_leaves_the_var_untouched() {
    assert_eq!(
        value("f = |xs| {\n    var $sum = 7\n    for n in xs {\n        $sum = $sum + n\n    }\n    $sum\n}\nf([])"),
        "7"
    );
}

#[test]
fn a_for_loop_evaluates_to_unit() {
    assert_eq!(
        value("f = |xs| {\n    var $s = 0\n    y = for n in xs {\n        $s = $s + n\n    }\n    y\n}\nf([1])"),
        "{}"
    );
}

#[test]
fn the_loop_variable_is_scoped_to_the_body() {
    let err = eval_error("f = |xs| {\n    for n in xs {\n        n\n    }\n    n\n}\nf([1])");
    assert!(err.contains("Undefined"), "got {}", err);
}

#[test]
fn iterating_a_non_list_is_an_error() {
    let err = eval_error("f = |_| {\n    for n in 5 {\n        n\n    }\n    1\n}\nf(0)");
    assert!(err.contains("List"), "got {}", err);
}

// --- while ----------------------------------------------------------------

#[test]
fn a_while_loop_runs_until_its_condition_is_false() {
    assert_eq!(
        value("f = |limit| {\n    var $i = 0\n    var $sum = 0\n    while $i < limit {\n        $sum = $sum + $i\n        $i = $i + 1\n    }\n    $sum\n}\nf(5)"),
        "10"
    );
}

#[test]
fn a_while_loop_may_never_run() {
    assert_eq!(
        value("f = |limit| {\n    var $i = 0\n    while $i < limit {\n        $i = $i + 1\n    }\n    $i\n}\nf(0)"),
        "0"
    );
}

#[test]
fn a_non_bool_while_condition_is_an_error() {
    let err = eval_error("f = |_| {\n    var $i = 0\n    while $i {\n        $i = 1\n    }\n    $i\n}\nf(0)");
    assert!(err.contains("Bool"), "got {}", err);
}

// --- break ----------------------------------------------------------------

#[test]
fn break_exits_a_for_loop_early() {
    assert_eq!(
        value("f = |xs| {\n    var $found = 0\n    for n in xs {\n        if n < 0 {\n            $found = n\n            break\n        } else {\n            {}\n        }\n    }\n    $found\n}\nf([1, 2, 0 - 7, 3])"),
        "-7"
    );
}

#[test]
fn break_exits_a_while_loop_early() {
    assert_eq!(
        value("f = |_| {\n    var $i = 0\n    while Bool.True {\n        $i = $i + 1\n        if $i > 3 {\n            break\n        } else {\n            {}\n        }\n    }\n    $i\n}\nf(0)"),
        "4"
    );
}

#[test]
fn break_outside_a_loop_surfaces_as_an_error() {
    // It rides the error channel, so it must not vanish silently when there is no
    // loop to catch it.
    let err = eval_error("f = |_| {\n    break\n}\nf(0)");
    assert!(!err.is_empty(), "a stray break should not be swallowed");
}

// --- the bug this exposed -------------------------------------------------

#[test]
fn an_assignment_as_the_last_statement_of_a_block_still_runs() {
    // The fold used to discard the final statement's binding target, so a loop body
    // whose only statement was an assignment did nothing at all.
    assert_eq!(
        value("f = |_| {\n    var $a = 1\n    for n in [10] {\n        $a = n\n    }\n    $a\n}\nf(0)"),
        "10"
    );
}

// ==========================================================================
// Error-handling sugar — phase 17: `?` and `??`.
//
// Verified against `roc` nightly-2026-09-03 before implementing:
//   * `?` unwraps Ok and skips the rest of the block on Err
//   * `?` on a block's FINAL expression is a type error — it unwraps, so the body
//     yields the payload rather than a Try
//   * `??` binds looser than arithmetic: `x ?? 1 + 2` is `x ?? (1 + 2)`, giving 3
//
// `.?` and `?:` landed later, in phase 14 — see `tests/field_defaults_test.rs`. The
// note that used to sit here, that `.?` segfaults `roc`, was wrong: it segfaults only
// when misused on an ordinary field of a plain record.

// --- ?? -------------------------------------------------------------------

#[test]
fn default_operator_takes_the_ok_value() {
    assert_eq!(as_str(r#"f = |s| I64.from_str(s) ?? 0
I64.to_str(f("42"))"#), "42");
}

#[test]
fn default_operator_falls_back_on_err() {
    assert_eq!(as_str(r#"f = |s| I64.from_str(s) ?? 7
I64.to_str(f("nope"))"#), "7");
}

#[test]
fn default_operator_binds_looser_than_arithmetic() {
    // `x ?? 1 + 2` is `x ?? (1 + 2)` = 3, not `(x ?? 1) + 2`.
    assert_eq!(as_str(r#"f = |s| I64.from_str(s) ?? 1 + 2
I64.to_str(f("nope"))"#), "3");
    // ...and the Ok path is unaffected by the default's shape.
    assert_eq!(as_str(r#"f = |s| I64.from_str(s) ?? 1 + 2
I64.to_str(f("9"))"#), "9");
}

#[test]
fn default_operator_desugars_to_a_match() {
    let ast = parse(r#"f = |s| I64.from_str(s) ?? 0
f"#).unwrap();
    let rendered = format!("{}", ast);
    assert!(
        rendered.contains("match") && rendered.contains("Ok(v)") && rendered.contains("Err(_)"),
        "?? should desugar to a match, got {}",
        rendered
    );
}

// --- ? --------------------------------------------------------------------

#[test]
fn question_unwraps_on_the_ok_path() {
    let src = r#"f = |s| {
    n = I64.from_str(s)?
    Ok(n + 1)
}
match f("41") { Ok(v) => I64.to_str(v) Err(_) => "err" }"#;
    assert_eq!(as_str(src), "42");
}

#[test]
fn question_short_circuits_the_rest_of_the_block() {
    // The `Ok(n + 1)` after the `?` must not run when from_str fails.
    let src = r#"f = |s| {
    n = I64.from_str(s)?
    Ok(n + 1)
}
match f("nope") { Ok(v) => I64.to_str(v) Err(_) => "err" }"#;
    assert_eq!(as_str(src), "err");
}

#[test]
fn question_chains() {
    let src = r#"add = |a, b| {
    x = I64.from_str(a)?
    y = I64.from_str(b)?
    Ok(x + y)
}
match add("1", "2") { Ok(v) => I64.to_str(v) Err(_) => "err" }"#;
    assert_eq!(as_str(src), "3");
    let bad = src.replace(r#"add("1", "2")"#, r#"add("1", "zz")"#);
    assert_eq!(as_str(&bad), "err");
}

#[test]
fn question_moves_the_continuation_into_the_ok_arm() {
    // This is what makes `?` non-trivial: the statements AFTER it end up inside the
    // Ok arm, so an Err skips them.
    let ast = parse(r#"f = |s| {
    n = I64.from_str(s)?
    Ok(n + 1)
}
f"#).unwrap();
    let rendered = format!("{}", ast);
    assert!(
        rendered.contains("Ok(n) => Ok((n + 1))"),
        "the continuation should sit inside the Ok arm, got {}",
        rendered
    );
    // `return`, not a bare `Err(e)`: `?` leaves the enclosing FUNCTION. Where the
    // match is the function's own body the two are the same, but inside a `for` or
    // `while` only the `return` propagates — the block's value there is `{}`.
    assert!(
        rendered.contains("Err(e) => return Err(e)"),
        "the Err arm should return the error, got {}",
        rendered
    );
}

#[test]
fn question_without_a_binding_still_propagates() {
    // `_ = expr?` discards the value but keeps the short-circuit.
    let src = r#"f = |s| {
    _ = I64.from_str(s)?
    Ok("survived")
}
match f("1") { Ok(v) => v Err(_) => "err" }"#;
    assert_eq!(as_str(src), "survived");
    let bad = src.replace(r#"f("1")"#, r#"f("zz")"#);
    assert_eq!(as_str(&bad), "err");
}

#[test]
fn question_on_the_final_expression_is_rejected() {
    // roc rejects it too, as a type error: `?` unwraps, so the block yields the
    // payload rather than a Try. Here there is also no continuation to move.
    let err = parse("f = |s| {\n    I64.from_str(s)?\n}\nf")
        .expect_err("`?` as the final expression should not parse");
    assert!(
        err.message.contains('?'),
        "error should mention the operator, got {:?}",
        err.message
    );
}

#[test]
fn double_question_is_not_read_as_two_singles() {
    // `??` must be consumed by the expression parser before the block looks for `?`.
    assert_eq!(as_str(r#"f = |s| I64.from_str(s) ?? 5
I64.to_str(f("nope"))"#), "5");
}

// ==========================================================================
// Pipelines — phase 13.
//
// Verified against `roc` nightly-2026-09-03 before implementing:
//   * `x |> f` is `f(x)`; `x |> f(a)` is `f(x, a)` — the piped value is PREPENDED,
//     the same convention static dispatch uses
//   * left-associative: `x |> f |> g` is `g(f(x))`
//   * it binds TIGHTER than every binary operator, which is the opposite of most
//     languages: `1 + 2 |> inc` is `1 + inc(2)` = 4, and `2 * 3 |> inc` is
//     `2 * inc(3)` = 8

const PIPE_DEFS: &str = "double : I64 -> I64\ndouble = |n| n * 2\ninc : I64 -> I64\ninc = |n| n + 1\nsub : I64, I64 -> I64\nsub = |a, b| a - b\n";

fn piped(expr: &str) -> String {
    as_str(&format!("{}({}).to_str()", PIPE_DEFS, expr))
}

// --- the basic form -------------------------------------------------------

#[test]
fn a_pipe_applies_the_function_to_the_value() {
    assert_eq!(piped("21 |> double"), "42");
}

#[test]
fn the_piped_value_is_prepended_to_written_arguments() {
    // `10 |> sub(4)` is `sub(10, 4)` = 6, not `sub(4, 10)` = -6. Subtraction is the
    // test that catches a swap.
    assert_eq!(piped("10 |> sub(4)"), "6");
}

#[test]
fn pipes_are_left_associative() {
    // `g(f(x))`, so the written order is the reading order.
    assert_eq!(piped("5 |> double |> inc"), "11");
    // Reversed, the arithmetic differs — proving the order is real.
    assert_eq!(piped("5 |> inc |> double"), "12");
}

#[test]
fn a_lambda_may_be_the_target() {
    assert_eq!(piped("20 |> (|n| n + 1)"), "21");
}

#[test]
fn a_qualified_function_may_be_the_target() {
    assert_eq!(as_str("xs : List(I64)\nxs = [1, 2]\n(xs |> List.len()).to_str()"), "2");
}

// --- precedence, which is the surprising part -----------------------------

#[test]
fn a_pipe_binds_tighter_than_addition() {
    // `1 + inc(2)` = 4, NOT `inc(1 + 2)` = 4... which happens to collide, so use
    // numbers where the two readings differ.
    assert_eq!(piped("1 + 2 |> double"), "5"); // 1 + double(2), not double(3)=6
}

#[test]
fn a_pipe_binds_tighter_than_multiplication() {
    assert_eq!(piped("2 * 3 |> inc"), "8"); // 2 * inc(3), not inc(6)=7
}

#[test]
fn a_pipe_binds_tighter_than_integer_division() {
    assert_eq!(piped("6 // 2 |> inc"), "2"); // 6 // inc(2), not inc(3)=4
}

#[test]
fn a_pipe_binds_tighter_than_comparison() {
    assert_eq!(
        as_str(&format!("{}Str.inspect(4 == 2 |> double)", PIPE_DEFS)),
        "True" // 4 == double(2), not double(4 == 2) which would not type check
    );
}

#[test]
fn parens_still_override_precedence() {
    assert_eq!(piped("(1 + 2) |> double"), "6");
}

// --- interaction with other `|` syntax ------------------------------------

#[test]
fn a_match_alternative_is_not_read_as_a_pipe() {
    // `A | B` in a pattern and `|>` in an expression both start with `|`.
    assert_eq!(
        as_str("f = |c| match c { Red | Green => \"rg\" Blue => \"b\" }\nf(Green)"),
        "rg"
    );
}

#[test]
fn a_lambda_boundary_is_not_read_as_a_pipe() {
    assert_eq!(piped("7 |> (|n| n * 3)"), "21");
}

#[test]
fn a_lambda_whose_body_is_a_record_reads_its_field_inside() {
    // `|b| { val: b.val + 1 }.val` is a lambda answering the field, not the field of
    // a lambda: a record body takes postfix like any other expression.
    let src = "Byte : { val : I64 }\nbump : Byte -> I64\nbump = |b| { val: (b.val + 1) }.val\nbump({ val: 41 })";
    assert_eq!(value(src), "42");
}

#[test]
fn or_is_not_read_as_a_pipe() {
    assert_eq!(
        as_str("Str.inspect(Bool.True or Bool.False)"),
        "True"
    );
}

// --- composes with the rest ----------------------------------------------

#[test]
fn a_pipe_target_can_be_dispatched_on() {
    assert_eq!(as_str("xs : List(I64)\nxs = [1, 2, 3]\n(xs |> List.len()).to_str()"), "3");
}

#[test]
fn a_pipe_works_inside_interpolation() {
    assert_eq!(piped("(3 |> double)"), "6");
    assert_eq!(as_str(&format!("{}\"v={{}}\"", PIPE_DEFS).replace("{}", "${(3 |> double).to_str()}")), "v=6");
}

// ==========================================================================
// Unary minus — phase 07.
//
// Verified against `roc` nightly-2026-09-03 before implementing:
//   * `-x` IS `x.negate()` — roc lowers it to that method, which is why `-s` on a Str
//     fails with "This negate method is being called on a value whose type doesn't
//     have that method" rather than a syntax error
//   * the operand is the whole POSTFIX chain: `-r.v` is `-(r.v)`, and `-n.to_str()`
//     tries to negate a Str
//   * it is LOOSER than `|>`: `-n |> inc` is `-(inc(n))` = -6, not `inc(-n)` = -4
//
// Known divergence: roc's tokenizer rejects a `-` bound tightly to an identifier
// (`m-n` and `m -n` are parse errors there, while `m - n` and `10-3` are fine). This
// interpreter accepts all of them as subtraction — more permissive, which is the safe
// direction, and no golden pair can rely on it because every pair passes `roc check`.

const NEG_DEFS: &str = "n : I64\nn = 5\nm : I64\nm = 3\ninc : I64 -> I64\ninc = |x| x + 1\nscale : I64 -> I64\nscale = |x| x * 10\n";

fn expr(e: &str) -> String {
    value(&format!("{}{}", NEG_DEFS, e))
}

// --- the lowering ---------------------------------------------------------

#[test]
fn unary_minus_lowers_to_a_negate_call() {
    // `-x` and `x.negate()` must build the SAME AST — that is what the golden pair
    // demonstrates, and it is why the diagnostic for `-"s"` mentions a method.
    let sugared = format!("{}", build("n = 5\n-n"));
    let explicit = format!("{}", build("n = 5\nn.negate()"));
    assert_eq!(sugared, explicit);
    assert!(sugared.contains("negate"), "expected a negate call, got {}", sugared);
}

#[test]
fn negating_a_variable() {
    assert_eq!(expr("-n"), "-5");
}

#[test]
fn a_negative_literal_stays_one_token() {
    // `-5` is lexed as a literal, not a negate call on 5.
    assert_eq!(value("-5"), "-5");
    assert!(!format!("{}", build("-5")).contains("negate"));
}

// --- the operand is the whole postfix chain -------------------------------

#[test]
fn the_operand_includes_field_access() {
    assert_eq!(expr("r = { v: 4 }\n-r.v"), "-4");
}

#[test]
fn the_operand_includes_a_call() {
    assert_eq!(expr("-scale(2)"), "-20");
}

#[test]
fn the_operand_includes_a_parenthesised_expression() {
    assert_eq!(expr("-(1 + 2)"), "-3");
}

#[test]
fn negating_a_non_number_is_an_error() {
    // roc reports this as a missing `negate` method, not a syntax error.
    let ast = build("s : Str\ns = \"x\"\n-s");
    let _ = TypeChecker::new().synth(&ast);
    assert!(rocflight::vm::eval(&ast).is_err(), "negating a Str should fail");
}

// --- precedence -----------------------------------------------------------

#[test]
fn unary_minus_is_tighter_than_the_binary_operators() {
    assert_eq!(expr("2 * -n"), "-10");
    assert_eq!(expr("10 - -n"), "15");
    assert_eq!(expr("-n + 1"), "-4");
}

#[test]
fn unary_minus_is_looser_than_a_pipeline() {
    // `-(inc(n))` = -6, not `inc(-n)` = -4. The two readings differ, so this is a real
    // test rather than a coincidence of the numbers.
    assert_eq!(expr("-n |> inc"), "-6");
}

#[test]
fn binary_minus_still_works() {
    assert_eq!(expr("m - n"), "-2");
    assert_eq!(expr("m - -n"), "8");
}

#[test]
fn negation_nests() {
    assert_eq!(expr("-(-n)"), "5");
}

#[test]
fn negation_composes_with_dispatch() {
    // `(-n).to_str()` negates first; `-n.to_str()` would negate a Str, which fails.
    assert_eq!(expr("(-n).to_str()"), "\"-5\"");
}

// ==========================================================================
// Phase 21 — the features the langref documents that earlier phases missed.
//
// Each has a golden pair under `tests/roc/21_langref/`; these pin the pieces that a
// pair exercises only indirectly, and the ones where roc's behaviour is surprising
// enough to be worth stating outright.

// --- grapheme literals ----------------------------------------------------

#[test]
fn a_grapheme_literal_is_a_number() {
    assert_eq!(value("'a'"), "97");
}

#[test]
fn a_grapheme_literal_takes_part_in_arithmetic() {
    assert_eq!(value("'a' + 1"), "98");
}

#[test]
fn a_grapheme_literal_holds_a_code_point_not_a_byte() {
    // 'é' is two bytes in UTF-8 but ONE code point, and the code point is the value.
    assert_eq!(value("'é'"), "233");
}

#[test]
fn a_grapheme_literal_understands_escapes() {
    assert_eq!(value("'\\n'"), "10");
    assert_eq!(value("'\\u(e9)'"), "233");
}

// --- ranges ---------------------------------------------------------------

#[test]
fn a_range_is_opaque_not_a_list() {
    // roc renders a range as `<opaque>`. Building one as a list would show
    // `[0, 1, 2]` here and would wrongly satisfy a `List` parameter.
    assert_eq!(value("0..<3"), "<opaque>");
}

#[test]
fn an_exclusive_range_stops_before_its_end() {
    assert_eq!(
        value("f = |r| {\n    var total = 0\n    for n in r {\n        total = total + n\n    }\n    total\n}\nf(0..<5)"),
        "10"
    );
}

#[test]
fn an_inclusive_range_reaches_its_end() {
    assert_eq!(
        value("f = |r| {\n    var total = 0\n    for n in r {\n        total = total + n\n    }\n    total\n}\nf(1..=5)"),
        "15"
    );
}

#[test]
fn a_range_binds_looser_than_arithmetic() {
    // `1 + 1..<2 * 3` is `2..<6`, not `1 + (1..<2) * 3`.
    assert_eq!(
        value("f = |r| {\n    var total = 0\n    for n in r {\n        total = total + n\n    }\n    total\n}\nf(1 + 1..<2 * 3)"),
        "14"
    );
}

// --- nominal construction over a non-record payload -----------------------

#[test]
fn a_nominal_over_a_single_value_is_its_payload() {
    assert_eq!(value("UserId := U64\nUserId.(7)"), "7");
}

#[test]
fn a_nominal_over_several_values_is_a_tuple() {
    assert_eq!(value("Pair := (I64, Str)\nPair.(1, \"two\")"), "(1, \"two\")");
}

// --- operators dispatch to methods ----------------------------------------

#[test]
fn a_type_that_defines_plus_gets_the_plus_operator() {
    let src = "Money :: { cents: I64 }.{\n    plus : Money, Money -> Money\n    plus = |a, b| { cents: a.cents + b.cents }\n}\n\
               a : Money\na = Money.{ cents: 5 }\nb : Money\nb = Money.{ cents: 7 }\n(a + b).cents";
    assert_eq!(value(src), "12");
}

#[test]
fn a_user_defined_is_eq_decides_equality_both_ways() {
    // `!=` asks for `is_eq` too, then negates it — roc names no separate method.
    let src = "Money :: { cents: I64 }.{\n    is_eq : Money, Money -> Bool\n    is_eq = |a, b| a.cents == b.cents\n}\n\
               a : Money\na = Money.{ cents: 5 }\nb : Money\nb = Money.{ cents: 9 }\n[a == b, a != b]";
    assert_eq!(value(src), "[False, True]");
}

#[test]
fn a_list_backed_nominals_is_eq_does_not_claim_a_record() {
    // A record compared with `==` looks for an `is_eq` by the value's shape. `T` is
    // over a list, so its `is_eq` is no candidate for the record; before, a list
    // backing had no shape, ruled nothing out, and `T.is_eq` compared the records
    // with `==` again, in Rust, until the stack ran out.
    let file = std::env::temp_dir().join("rocflight_list_shape.roc");
    std::fs::write(
        &file,
        "app [main!] {}\n\nT :: List(U8).{\n\tis_eq : T, T -> Bool\n\tis_eq = |T.(a), T.(b)| a == b\n\n\tof : List(U8) -> T\n\tof = |u| T.(u)\n}\n\n\
         main! = |_args| Ok({ n: T.of([1]) } == { n: T.of([1]) })\n",
    )
    .unwrap();
    let options = rocflight::run::Options { inspect_result: true, ..Default::default() };
    let ran = rocflight::run::run_file(file.to_str().unwrap(), options)
        .unwrap_or_else(|e| panic!("{}", e))
        .expect("an app runs");
    let _ = std::fs::remove_file(&file);
    assert_eq!(ran.inspected.expect("inspected"), "Ok(True)");
}

#[test]
fn a_scalar_backed_nominals_is_eq_does_not_claim_a_record() {
    // The same for a nominal over an integer: `Code.is_eq` is no candidate for a
    // record, so the record is compared field by field.
    let file = std::env::temp_dir().join("rocflight_scalar_shape.roc");
    std::fs::write(
        &file,
        "app [main!] {}\n\nCode :: I64.{\n\tis_eq : Code, Code -> Bool\n\tis_eq = |Code.(a), Code.(b)| a == b\n\n\tof : I64 -> Code\n\tof = |c| Code.(c)\n}\n\n\
         main! = |_args| Ok({ c: Code.of(15) } == { c: Code.of(15) })\n",
    )
    .unwrap();
    let options = rocflight::run::Options { inspect_result: true, ..Default::default() };
    let ran = rocflight::run::run_file(file.to_str().unwrap(), options)
        .unwrap_or_else(|e| panic!("{}", e))
        .expect("an app runs");
    let _ = std::fs::remove_file(&file);
    assert_eq!(ran.inspected.expect("inspected"), "Ok(True)");
}

#[test]
fn a_user_defined_operator_does_not_capture_the_primitives() {
    // A type defining `plus` must not hijack `1 + 2`.
    let src = "Money :: { cents: I64 }.{\n    plus : Money, Money -> Money\n    plus = |a, b| { cents: a.cents + b.cents }\n}\n1 + 2";
    assert_eq!(value(src), "3");
}

#[test]
fn a_nominal_unwrapped_by_a_pattern_is_its_backing_type() {
    // `|Units.(a), Units.(b)| a == b` compares the two LISTS: `a` is a `List(U8)`, so
    // its `==` is the list's, not `Units.is_eq` again. Typed as the nominal, the
    // comparison called itself until the recursion limit. The whole pipeline, since
    // the checker's operand types reach the compiler only through `run_file`.
    let file = std::env::temp_dir().join("rocflight_nominal_unwrap.roc");
    std::fs::write(
        &file,
        "app [main!] {}\n\nUnits :: List(U8).{\n\tis_eq : Units, Units -> Bool\n\tis_eq = |Units.(a), Units.(b)| a == b\n}\n\n\
         main! = |_args| Ok(Units.([1, 2]) == Units.([1, 2]))\n",
    )
    .unwrap();
    let options = rocflight::run::Options { inspect_result: true, ..Default::default() };
    let ran = rocflight::run::run_file(file.to_str().unwrap(), options)
        .unwrap_or_else(|e| panic!("{}", e))
        .expect("an app runs");
    let _ = std::fs::remove_file(&file);
    assert_eq!(ran.inspected.expect("inspected"), "Ok(True)");
}

#[test]
fn a_string_pattern_matches_a_nominal_through_from_quote() {
    // `"zero"` against a `Text` is `Text.from_quote("zero")`, compared by `is_eq`.
    // Inside an annotated function the match is checked, not synthesised, and must
    // still hand its scrutinee's type to the compiler.
    let file = std::env::temp_dir().join("rocflight_quote_pattern.roc");
    std::fs::write(
        &file,
        "app [main!] {}\n\nText :: List(U8).{\n\tfrom_quote : Str -> Try(Text, [BadQuotedBytes(Str)])\n\tfrom_quote = |s| Ok(Text.(Str.to_utf8(s)))\n\n\
         \tis_eq : Text, Text -> Bool\n\tis_eq = |Text.(a), Text.(b)| a == b\n}\n\n\
         name : Text -> Str\nname = |t| match t {\n\t\"zero\" => \"matched\"\n\t_ => \"fell through\"\n}\n\n\
         main! = |_args| Ok(name(\"zero\"))\n",
    )
    .unwrap();
    let options = rocflight::run::Options { inspect_result: true, ..Default::default() };
    let ran = rocflight::run::run_file(file.to_str().unwrap(), options)
        .unwrap_or_else(|e| panic!("{}", e))
        .expect("an app runs");
    let _ = std::fs::remove_file(&file);
    assert_eq!(ran.inspected.expect("inspected"), "Ok(\"matched\")");
}

#[test]
fn a_nested_nominals_methods_see_the_enclosing_blocks_members() {
    // `Box.is_eq` calls `same_box`, which `Shape`'s block declares: roc resolves a bare
    // name through every block around the method, not only the method's own.
    let file = std::env::temp_dir().join("rocflight_enclosing_owner.roc");
    std::fs::write(
        &file,
        "app [main!] {}\n\nShape :: [].{\n\tBox := [B(I64)].{\n\t\tis_eq : Shape.Box, Shape.Box -> Bool\n\t\tis_eq = |a, b| same_box(a, b)\n\t}\n\n\
         \tsame_box : Shape.Box, Shape.Box -> Bool\n\tsame_box = |a, b| match (a, b) {\n\t\t(B(x), B(y)) => x == y\n\t}\n}\n\n\
         main! = |_args| Ok(B(3) == B(3))\n",
    )
    .unwrap();
    let options = rocflight::run::Options { inspect_result: true, ..Default::default() };
    let ran = rocflight::run::run_file(file.to_str().unwrap(), options)
        .unwrap_or_else(|e| panic!("{}", e))
        .expect("an app runs");
    let _ = std::fs::remove_file(&file);
    assert_eq!(ran.inspected.expect("inspected"), "Ok(True)");
}

// --- type-level features --------------------------------------------------

#[test]
fn a_type_alias_is_transparent() {
    // `Bytes` and `List(U8)` are the same type, so a plain list passes for `Bytes`.
    let src = "Bytes : List(U8)\nsize : Bytes -> U64\nsize = |b| b.len()\nsize([1, 2, 3])";
    assert_eq!(value(src), "3");
}

#[test]
fn an_open_record_accepts_extra_fields() {
    let src = "name_of : { name: Str, .. } -> Str\nname_of = |r| r.name\n\
               [name_of({ name: \"a\", age: 1 }), name_of({ name: \"b\" })]";
    assert_eq!(value(src), "[\"a\", \"b\"]");
}

#[test]
fn a_where_clause_permits_dispatch_on_a_type_variable() {
    // Without the clause this is "Cannot dispatch `to_str` on an unresolved type".
    let src = "label : a -> Str where [a.to_str : a -> Str]\nlabel = |x| x.to_str()\n\
               n : I64\nn = 7\nlabel(n)";
    assert_eq!(value(src), "\"7\"");
}

#[test]
fn a_parameterised_nominal_instantiates_its_backing_type() {
    let src = "Wrapper(a) := { item: a }\nunwrap : Wrapper(a) -> a\nunwrap = |w| w.item\n\
               n : Wrapper(I64)\nn = Wrapper.{ item: 42 }\nunwrap(n) + 1";
    assert_eq!(value(src), "43");
}

// ==========================================================================
// Modules and packages.
//
// Verified against `roc` nightly-2026-09-07 and nightly-2026-09-22: the program
// below prints `hi!` under both.
//   * a module's own `import Sibling` is a file beside that module, and is loaded
//     whether or not the app imports it too.
//   * `pkg: "./pkg/main.roc"` in an app header is a package on disk: `import
//     pkg.Words` is `pkg/Words.roc`, and Words' own `import Letters` is beside it.

/// Write `files` under a fresh directory and run its `main.roc`, answering
/// `Str.inspect` of what `main!` returned.
fn run_files(dir: &str, files: &[(&str, &str)]) -> String {
    let root = std::env::temp_dir().join(dir);
    let _ = std::fs::remove_dir_all(&root);
    for (path, text) in files {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, text).unwrap();
    }
    let options = rocflight::run::Options { inspect_result: true, ..Default::default() };
    let main = root.join("main.roc");
    let ran = rocflight::run::run_file(main.to_str().unwrap(), options)
        .unwrap_or_else(|e| panic!("{}", e))
        .expect("an app runs");
    let _ = std::fs::remove_dir_all(&root);
    ran.inspected.expect("inspected")
}

#[test]
fn a_module_loads_the_modules_it_imports() {
    let out = run_files(
        "rocflight_module_imports",
        &[
            ("Exclaim.roc", "Exclaim :: [].{\n\tbang : Str -> Str\n\tbang = |s| Str.concat(s, \"!\")\n}\n"),
            ("Shout.roc", "import Exclaim\n\nShout :: [].{\n\tshout : Str -> Str\n\tshout = |s| Exclaim.bang(s)\n}\n"),
            ("main.roc", "app [main!] {}\n\nimport Shout\n\nmain! = |_args| Ok(Shout.shout(\"hi\"))\n"),
        ],
    );
    assert_eq!(out, "Ok(\"hi!\")");
}

#[test]
fn a_package_on_disk_is_imported_through_its_alias() {
    let out = run_files(
        "rocflight_local_package",
        &[
            ("pkg/main.roc", "package [Words] {}\n"),
            ("pkg/Letters.roc", "Letters :: [].{\n\th : Str\n\th = \"h\"\n}\n"),
            ("pkg/Words.roc", "import Letters\n\nWords :: [].{\n\tgreeting : Str\n\tgreeting = Str.concat(Letters.h, \"i\")\n}\n"),
            ("main.roc", "app [main!] { pkg: \"./pkg/main.roc\" }\n\nimport pkg.Words\n\nmain! = |_args| Ok(Words.greeting)\n"),
        ],
    );
    assert_eq!(out, "Ok(\"hi\")");
}

#[test]
fn a_module_type_named_like_its_module_is_the_type_elsewhere() {
    // `Maybe.Maybe(Str)` in another module is the tag union, not the empty namespace
    // `Maybe :: []` that shares its last segment.
    let out = run_files(
        "rocflight_module_same_name",
        &[
            ("Maybe.roc", "Maybe :: [].{\n\tMaybe(a) : [Just(a), None]\n\n\twith_default : Maybe.Maybe(a), a -> a\n\twith_default = |m, d| match m {\n\t\tJust(x) => x\n\t\tNone => d\n\t}\n}\n"),
            ("Find.roc", "import Maybe\n\nFind :: [].{\n\tfirst : List(Str) -> Maybe.Maybe(Str)\n\tfirst = |xs| match List.first(xs) {\n\t\tOk(x) => Just(x)\n\t\tErr(_) => None\n\t}\n}\n"),
            ("main.roc", "app [main!] {}\n\nimport Maybe\nimport Find\n\nmain! = |_args| Ok(Maybe.with_default(Find.first([\"given\"]), \"fallback\"))\n"),
        ],
    );
    assert_eq!(out, "Ok(\"given\")");
}

#[test]
fn a_qualified_type_is_the_imports_even_where_the_module_shares_its_name() {
    // In `Thing.roc`, itself the namespace `Thing :: [].{ .. }`, the annotation
    // `Kinds.Thing` is the record `Kinds` declares, not the file's own namespace.
    let out = run_files(
        "rocflight_qualified_type_import",
        &[
            ("Kinds.roc", "Kinds :: [].{\n\tThing : { a : I64 }\n}\n"),
            ("Thing.roc", "import Kinds\n\nThing :: [].{\n\tmake : I64 -> Kinds.Thing\n\tmake = |n| { a: n }\n}\n"),
            ("main.roc", "app [main!] {}\n\nimport Thing\n\nmain! = |_args| Ok(Thing.make(5).a)\n"),
        ],
    );
    assert_eq!(out, "Ok(5)");
}

#[test]
fn a_nominal_built_through_its_module_has_its_declared_fields() {
    // `Shapes.Dim.{ w: 3 }` in another file: the literal is `Dim`'s, so `3` is the
    // `I64` its field says, not a fraction.
    let out = run_files(
        "rocflight_qualified_nominal_literal",
        &[
            ("Shapes.roc", "Shapes :: [].{\n\tDim := { w : I64 }\n}\n"),
            ("main.roc", "app [main!] {}\n\nimport Shapes\n\nmain! = |_args| {\n\td = Shapes.Dim.{ w: 3 }\n\tOk(Str.inspect(d.w))\n}\n"),
        ],
    );
    assert_eq!(out, "Ok(\"3\")");
}

#[test]
fn a_nominal_built_through_its_module_is_not_the_files_own_of_that_name() {
    // This file's `Dim` has a defaulted `h`; `Shapes.Dim` has no `h` at all. The
    // literal is the import's, so the local default is not filled in.
    let out = run_files(
        "rocflight_qualified_nominal_literal_shadowed",
        &[
            ("Shapes.roc", "Shapes :: [].{\n\tDim := { w : I64 }\n}\n"),
            ("main.roc", "app [main!] {}\n\nimport Shapes\n\nDim := { w : I64, h : I64 ?? 0 }\n\nmain! = |_args| {\n\td = Shapes.Dim.{ w: 3 }\n\tl = Dim.{ w: 4 }\n\tOk(Str.inspect((d.w, l.h)))\n}\n"),
        ],
    );
    assert_eq!(out, "Ok(\"(3, 0)\")");
}

#[test]
fn an_imported_modules_expects_do_not_run_with_the_app() {
    // `roc` runs a module's top-level `expect`s under `roc test` only. A module's
    // top level went straight into the globals, so its `_ = expect` became a global
    // evaluated on load: this one crashed the app, and in Fast Track one naming a
    // helper outside the namespace block failed it with "Undefined variable".
    let out = run_files(
        "rocflight_module_expects_not_run",
        &[
            ("Rng.roc", "Rng :: [].{\n\tnext : U64 -> U64\n\tnext = |s| s + 1\n}\n\nboom : U64 -> U64\nboom = |_| crash \"a module's expect ran\"\n\nexpect boom(1) == 1\n"),
            ("main.roc", "app [main!] {}\n\nimport Rng\n\nmain! = |_args| Ok(Rng.next(1))\n"),
        ],
    );
    assert_eq!(out, "Ok(2)");
}
