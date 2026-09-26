//! The type checker: annotations, generics, and nominal types.

mod common;
use crate::common::*;

// ==========================================================================
// Type annotations in the AST, and the three checks they unlock.
//
// Annotations used to be skipped by the parser and thrown away. That single gap was
// the root cause of three separate documented ceilings:
//
//   1. identifiers had no type — every `Expr::Ident` synthesised a fresh variable
//   2. a closed tag union could not be enforced — `c : [Red, Green]` accepted `Blue`
//   3. `match` exhaustiveness could not be checked — nothing knew the full union
//
// Verified against `roc` nightly-2026-09-03: a GUARDED arm does not count towards
// coverage, which roc also rejects with "This match expression doesn't cover all
// possible cases."

// --- the annotation reaches the AST at all ---------------------------------

#[test]
fn an_annotation_gives_the_binding_its_declared_type() {
    assert_eq!(type_of("x : U8\nx = 255\nx"), "U8");
    assert_eq!(type_of("s : Str\ns = \"hi\"\ns"), "Str");
    assert_eq!(type_of("xs : List(I64)\nxs = [1]\nxs"), "List(I64)");
}

#[test]
fn annotations_survive_desugaring() {
    let desugared = Desugarer::new("x : U8\nx = 255\n".to_string()).desugar().unwrap();
    assert!(desugared.contains("x : U8"), "annotation was stripped");
}

#[test]
fn every_annotation_form_in_the_corpus_parses() {
    // Records, tuples, tag unions, functions, applied types, and `Try`.
    for src in [
        "x : { a: Bool, b: I64 }\nx = { a: Bool.True, b: 1 }\nx",
        "x : (Str, I64)\nx = (\"a\", 1)\nx",
        "x : [Foo(I64, Str), Bar]\nx = Bar\nx",
        "f : I64, I64 -> I64\nf = |a, b| a + b\nf",
        "f : (I64, I64) -> I64\nf = |p| p.0\nf",
        "x : List(List(I64))\nx = [[1]]\nx",
        "x : {}\nx = {}\nx",
    ] {
        assert!(accepts(src), "should type check: {}", src);
    }
}

#[test]
fn try_is_the_ok_err_union() {
    // `Try(a, b)` IS `[Ok(a), Err(b)]` in roc, so the annotation is checkable.
    assert_eq!(
        type_of("x : Try(I64, Str)\nx = Ok(1)\nx"),
        "[Err(Str), Ok(I64)]"
    );
}

#[test]
fn a_multi_param_annotation_curries() {
    assert_eq!(type_of("f : Str, I64 -> Bool\nf = |a, b| Bool.True\nf"), "(Str -> (I64 -> Bool))");
    // Parenthesised, it is ONE tuple parameter instead of two.
    assert_eq!(type_of("f : (Str, I64) -> Bool\nf = |p| Bool.True\nf"), "((Str, I64) -> Bool)");
}

// --- feature 1: identifiers carry a type ----------------------------------

#[test]
fn a_named_non_function_cannot_be_called() {
    // Previously accepted: `x` synthesised a fresh var, which unified with anything.
    assert!(!accepts("x = 42\nx(1)"), "calling an I64 should fail");
}

#[test]
fn inference_flows_through_names_without_annotations() {
    // An unconstrained numeral has no width until something gives it one, and roc
    // then DEFAULTS it — `x = 42` prints `42.0`, a `Dec`. Nothing here constrains it,
    // so what comes back is the default rather than the old unconditional I64.
    assert_eq!(defaulted_type_of("x = 42\nx"), "Dec");
    assert_eq!(type_of("n : I64\nn = 42\nn"), "I64");
    assert_eq!(type_of("r = { a: Bool.True }\nr.a"), "Bool");
}

// --- feature 2: closed tag unions -----------------------------------------

#[test]
fn a_closed_union_rejects_a_tag_it_does_not_list() {
    let err = type_error("c : [Red, Green]\nc = Blue\nc");
    assert!(err.contains("Blue"), "error should name the tag, got {}", err);
}

#[test]
fn a_closed_union_accepts_its_own_tags() {
    assert!(accepts("c : [Red, Green]\nc = Green\nc"));
}

#[test]
fn an_inferred_union_stays_open() {
    // A tag expression means "at least this tag", so unification may add more. Only an
    // annotation closes a union.
    assert_eq!(type_of("Red"), "[Red, ..]");
    assert!(accepts("c : [Red, Green, ..]\nc = Blue\nc"));
}

#[test]
fn annotations_also_catch_non_tag_mismatches() {
    // All of these were invisible before, for the same reason.
    assert!(!accepts("r : { x: Bool }\nr = { x: 1 }\nr"), "record field type");
    assert!(!accepts("xs : List(I64)\nxs = [\"a\"]\nxs"), "list element type");
    assert!(!accepts("t : (I64, I64)\nt = (1, 2, 3)\nt"), "tuple arity");
    assert!(!accepts("v : [Foo(I64)]\nv = Foo(\"s\")\nv"), "payload type");
}

// --- feature 3: match exhaustiveness --------------------------------------

#[test]
fn a_match_missing_a_case_is_rejected() {
    let err = type_error(
        "f : [Red, Green, Blue] -> Str\nf = |c| match c { Red => \"r\" Green => \"g\" }\nf",
    );
    assert!(err.contains("Blue"), "error should name the missing tag, got {}", err);
}

#[test]
fn full_coverage_is_accepted() {
    assert!(accepts(
        "f : [Red, Green, Blue] -> Str\nf = |c| match c { Red => \"r\" Green => \"g\" Blue => \"b\" }\nf"
    ));
}

#[test]
fn a_wildcard_or_binding_completes_a_match() {
    assert!(accepts("f : [Red, Green, Blue] -> Str\nf = |c| match c { Red => \"r\" _ => \"o\" }\nf"));
    assert!(accepts("f : [Red, Green, Blue] -> Str\nf = |c| match c { Red => \"r\" x => \"o\" }\nf"));
}

#[test]
fn alternatives_count_towards_coverage() {
    assert!(accepts(
        "f : [Red, Green, Blue] -> Str\nf = |c| match c { Red | Green => \"rg\" Blue => \"b\" }\nf"
    ));
}

#[test]
fn a_guarded_arm_does_not_count_as_coverage() {
    // It may not run, so it cannot complete a match. roc agrees: the same program is
    // rejected with "This match expression doesn't cover all possible cases."
    let err = type_error(
        "f : [Red, Green], I64 -> Str\nf = |c, n| match c { Red => \"r\" _ if n > 0 => \"p\" }\nf",
    );
    assert!(err.contains("Green"), "error should name the missing tag, got {}", err);
}

#[test]
fn an_open_or_unannotated_scrutinee_is_not_checked() {
    // There is no list of "all the cases" to check against, so nothing is claimed.
    assert!(accepts("f : [Red, Green, ..] -> Str\nf = |c| match c { Red => \"r\" _ => \"o\" }\nf"));
    assert!(accepts("f = |c| match c { Red => \"r\" }\nf"));
}

// --- pattern bindings get real types --------------------------------------

#[test]
fn payload_bindings_take_their_declared_types() {
    assert!(accepts(
        "f : [Foo(I64, Str)] -> Str\nf = |t| match t { Foo(n, s) => \"${s}${I64.to_str(n)}\" }\nf"
    ));
    // ...and a wrong use of one is caught.
    assert!(
        !accepts("f : [Foo(I64)] -> Str\nf = |t| match t { Foo(n) => n }\nf"),
        "an I64 payload should not satisfy a Str result"
    );
}

#[test]
fn a_rest_binding_is_a_list_of_the_element_type() {
    assert!(accepts(
        "f : List(I64) -> I64\nf = |xs| match xs { [9, .. as tail] => List.len(tail) _ => 0 }\nf"
    ));
}

// ==========================================================================
// Type variables and generics — phase 18.
//
// Verified against `roc` nightly-2026-09-03 before implementing:
//   * `identity : a -> a` may be used at two DIFFERENT types in one program
//   * within one signature a repeated name is ONE variable: `pair : a, a -> a`
//     rejects `pair(1, "s")`
//   * across signatures the names are unrelated
//   * `List(a) -> List(a)` ties the element types of argument and result together

const ID: &str = "identity : a -> a\nidentity = |x| x\n";
const PAIR: &str = "pair : a, a -> a\npair = |x, _y| x\n";
const FIRST: &str = "first : a, b -> a\nfirst = |x, _y| x\n";

// --- generalisation -------------------------------------------------------

#[test]
fn a_generic_function_works_at_one_type() {
    assert_eq!(defaulted_type_of(&format!("{}identity(\"hi\")", ID)), "Str");
    assert_eq!(defaulted_type_of(&format!("{}identity(5)", ID)), "Dec");
}

#[test]
fn a_generic_function_works_at_two_types_in_one_program() {
    // The whole point. Without instantiating per use, the first call pins `a` and the
    // second fails with "Cannot unify Str with I64".
    assert!(accepts(&format!("{}s = identity(\"hi\")\nn = identity(5)\nn", ID)));
}

#[test]
fn each_use_gets_its_own_instance() {
    assert_eq!(
        defaulted_type_of(&format!("{}s = identity(\"hi\")\nn = identity(5)\nn", ID)),
        "Dec"
    );
}

#[test]
fn a_generic_function_survives_three_uses() {
    assert!(accepts(&format!(
        "{}a = identity(\"s\")\nb = identity(1)\nc = identity(Bool.True)\nc",
        ID
    )));
}

// --- repeated variables ---------------------------------------------------

#[test]
fn a_repeated_variable_ties_positions_together() {
    assert!(accepts(&format!("{}pair(1, 2)", PAIR)));
}

#[test]
fn a_repeated_variable_rejects_mismatched_types() {
    // `pair : a, a -> a` means one type for both. This was wrongly accepted while each
    // occurrence of `a` became a separate fresh variable.
    assert!(
        !accepts(&format!("{}pair(1, \"s\")", PAIR)),
        "a, a -> a should reject mixed argument types"
    );
}

#[test]
fn distinct_variables_may_differ() {
    assert!(accepts(&format!("{}first(1, \"s\")", FIRST)));
    assert_eq!(defaulted_type_of(&format!("{}first(1, \"s\")", FIRST)), "Dec");
}

#[test]
fn variables_are_scoped_to_their_own_signature() {
    // The `a` in `pair` is unrelated to the `a` in `first`.
    assert!(accepts(&format!("{}{}x = pair(1, 2)\ny = first(\"s\", 1)\ny", PAIR, FIRST)));
}

// --- generic containers ---------------------------------------------------

#[test]
fn a_variable_inside_a_container_is_still_generic() {
    let src = "echo_list : List(a) -> List(a)\necho_list = |xs| xs\n";
    assert_eq!(defaulted_type_of(&format!("{}echo_list([1])", src)), "List(Dec)");
    assert!(accepts(&format!("{}n = echo_list([1])\ns = echo_list([\"a\"])\ns", src)));
}

#[test]
fn argument_and_result_element_types_are_tied() {
    let src = "echo_list : List(a) -> List(a)\necho_list = |xs| xs\n";
    // The result is a list of the SAME element type, not an independent one.
    assert_eq!(defaulted_type_of(&format!("{}echo_list([\"a\"])", src)), "List(Str)");
}

#[test]
fn a_generic_argument_with_a_concrete_result() {
    let src = "count : List(a) -> U64\ncount = |xs| List.len(xs)\n";
    assert!(accepts(&format!("{}n = count([1])\ns = count([\"a\"])\ns", src)));
}

// --- the id-space bug -----------------------------------------------------

#[test]
fn annotation_variables_do_not_collide_with_inferred_ones() {
    // The parser and the checker allocate type-variable ids from the same number
    // space, so an annotation's `$1` would otherwise BE the checker's first fresh
    // variable and unify with something unrelated. Instantiating on ingest keeps the
    // parser's ids out of unification entirely.
    //
    // This showed up as `List(a) -> List(a)` failing with "Cannot unify List(I64)
    // with I64" — two unrelated types meeting through a shared id.
    let src = "echo_list : List(a) -> List(a)\necho_list = |xs| xs\n";
    assert_eq!(defaulted_type_of(&format!("{}echo_list([1])", src)), "List(Dec)");
}

#[test]
fn a_monomorphic_binding_is_not_generalised() {
    // Only annotation variables are quantified. An inferred type stays put, so a
    // plain binding cannot be used at two types.
    assert!(!accepts("x = 42\ns : Str\ns = x\ns"), "an I64 binding is not generic");
}

// ==========================================================================
// Nominal types — phase 14.
//
// Verified against `roc` nightly-2026-09-03 before implementing:
//   * `Name := backing` is NOT opaque: roc accepts the plain backing value where the
//     nominal is expected (`f({ x: 1 })` for `f : Point -> _`)
//   * but two DIFFERENT nominals with identical backing do not interchange
//   * the nominal name is erased in values — `Str.inspect` on one shows the bare
//     backing record
//   * a nominal over a tag union is still exhaustiveness-checked
//
// `::` opaque types are accepted as a SYNONYM for `:=`, because within one file roc
// does not distinguish them — both allow field access and both accept the plain
// backing — and opacity only matters across module boundaries. Method blocks, field
// defaults and optional fields all landed later in this phase; the notes that used to
// sit here calling them unimplemented, and `.?` a compiler segfault, were both wrong.

// --- declaration and construction -----------------------------------------

// A nominal named inside a declaration -- its own recursive `IList(a)`, or a
// `Step(a)` declared after it -- is the declaration's argument, not a fresh type.
// roc rejects each `Str` below ("This string literal is being used where a
// non-string type is needed"). Whole programs, because only `run_file` gives the
// checker the declarations a placeholder expands to.
fn app(decls: &str, value: &str) -> String {
    format!("app [main!] {{}}\n\n{decls}\nmain! = |_args| {{\n\t_ = {value}\n\tOk({{}})\n}}\n")
}
const ILIST: &str = "IList(a) := [INil, ICons(a, IList(a))]\n";
const ITER: &str = "Iter_(a) := { next : (I64 -> Step(a)) }\nStep(a) := [One(a, Iter_(a)), Done]\n\nempty : Iter_(I64)\nempty = Iter_.{ next: |_| Done }\n";

#[test]
fn a_recursive_nominal_keeps_its_argument() {
    let bad = format!("{ILIST}\nl : IList(I64)\nl = ICons(1, ICons(\"x\", INil))\n");
    let err = run_program(&app(&bad, "l")).expect_err("a Str inside an IList(I64)");
    assert!(err.contains("Str") && err.contains("I64"), "got {}", err);
    let good = format!("{ILIST}\nl : IList(I64)\nl = ICons(1, ICons(2, INil))\n");
    assert_eq!(run_program(&app(&good, "l")), Ok(()));
}

#[test]
fn a_nominal_named_before_its_declaration_keeps_its_argument() {
    let bad = format!("{ITER}\nworse : Iter_(I64)\nworse = Iter_.{{ next: |_| One(\"oops\", empty) }}\n");
    let err = run_program(&app(&bad, "worse")).expect_err("a Str inside an Iter_(I64)");
    assert!(err.contains("Str") && err.contains("I64"), "got {}", err);
    // And a well-typed one still checks: the same nominal at the same arguments is
    // the same type, without unfolding `Iter_` inside `Step` inside `Iter_` for ever.
    let good = format!("{ITER}\nfine : Iter_(I64)\nfine = Iter_.{{ next: |_| One(7, empty) }}\n");
    assert_eq!(run_program(&app(&good, "fine")), Ok(()));
}

#[test]
fn an_alias_to_a_nominal_keeps_the_nominals_own_arguments() {
    // `Swap(I64, Str)` is `P(Str, I64)`: the alias's arguments are its own, not P's.
    // roc rejects `t : P(I64, Str)` and accepts `u : P(Str, I64)`.
    let decls = "P(a, b) := { x : a, y : b }\nSwap(a, b) : P(b, a)\n\ns : Swap(I64, Str)\ns = P.{ x: \"hi\", y: 1 }\n";
    let err = run_program(&app(&format!("{decls}\nt : P(I64, Str)\nt = s\n"), "t")).expect_err("P(I64, Str) is not Swap(I64, Str)");
    assert!(err.contains("Str") && err.contains("I64"), "got {}", err);
    assert_eq!(run_program(&app(&format!("{decls}\nu : P(Str, I64)\nu = s\n"), "u")), Ok(()));
}

#[test]
fn a_tag_pattern_on_a_nominal_binds_its_payload_types() {
    // `B(n)` against a `Crate := [B(I64), Empty]` binds `n : I64`, so returning it
    // where a Str is declared is rejected, as roc rejects it ("The first branch of
    // this match does not match the previous branch").
    let src = "Crate := [B(I64), Empty]\nf : Crate -> Str\nf = |c| match c {\n    B(n) => n\n    Empty => \"empty\"\n}\nf";
    let err = type_error(src);
    assert!(err.contains("Str") && err.contains("I64"), "got {}", err);
    assert!(accepts("Crate := [B(I64), Empty]\nf : Crate -> I64\nf = |c| match c {\n    B(n) => n\n    Empty => 0\n}\nf"));
}

#[test]
fn an_inferred_union_grows_where_it_is_used() {
    // `List.repeat(Red, 2)` and `Blue` are one union once `List.append` joins them,
    // `[Blue, Red, ..]`, and `show` takes only `[Green, Red]`. roc rejects it ("This
    // argument has the type: [Green, Red] -> Str"); without row variables the
    // union never grew, and the program crashed at `match` on `Blue` instead.
    let show = "show : [Red, Green] -> Str\nshow = |c| match c {\n    Red => \"red\"\n    Green => \"green\"\n}\n";
    let err = type_error(&format!("{}List.map(List.append(List.repeat(Red, 2), Blue), show)", show));
    assert!(err.contains("Blue"), "got {}", err);
    assert!(accepts(&format!("{}List.map(List.append(List.repeat(Red, 2), Green), show)", show)));
}

#[test]
fn an_inferred_union_keeps_growing_past_a_builtins_open_union() {
    // `U8.from_str`'s error is an open union with no row of its own. The list's
    // union meets it and stays open under a fresh row, so the `Blue` appended next
    // still reaches `show`, which roc rejects.
    let show = "show : Try(U8, [BadNumStr]) -> Str\nshow = |t| match t {\n    Ok(n) => U8.to_str(n)\n    Err(BadNumStr) => \"bad\"\n}\n";
    let err = type_error(&format!("{}List.map(List.append(List.append(List.repeat(Ok(1), 1), U8.from_str(\"5\")), Blue), show)", show));
    assert!(err.contains("Blue"), "got {}", err);
    assert!(accepts(&format!("{}List.map(List.append(List.repeat(Ok(1), 1), U8.from_str(\"5\")), show)", show)));
}

#[test]
fn an_annotations_open_union_is_a_row_of_its_own() {
    // `keep`'s `..` is a row, fresh at each use, so what `keep` returns is the
    // list's union and the `Blue` appended to it reaches `show`. roc rejects it.
    let src = "keep : [Red, ..] -> [Red, ..]\nkeep = |c| c\nshow : [Red, Green] -> Str\nshow = |c| match c {\n    Red => \"red\"\n    Green => \"green\"\n}\n";
    let err = type_error(&format!("{}List.map(List.append(List.map(List.repeat(Red, 2), keep), Blue), show)", src));
    assert!(err.contains("Blue"), "got {}", err);
    assert!(accepts(&format!("{}List.map(List.append(List.map(List.repeat(Red, 2), keep), Green), show)", src)));
}

#[test]
fn a_lone_tag_passed_as_a_nominal_is_that_nominal() {
    // Each `Empty` meets `Node` through `show`, and from then on it IS a `Node`:
    // the `Blue` appended afterwards is not one of its tags, as roc says.
    let node = "Node := [Empty, Leaf(U8)]\nshow : Node -> Str\nshow = |n| match n {\n    Empty => \"empty\"\n    Leaf(b) => U8.to_str(b)\n}\n";
    let err = type_error(&format!("{}List.append(List.map(List.repeat(Empty, 2), |n| {{\n    _s = show(n)\n    n\n}}), Blue)", node));
    assert!(err.contains("Blue"), "got {}", err);
    assert!(accepts(&format!("{}List.append(List.map(List.repeat(Empty, 2), |n| {{\n    _s = show(n)\n    n\n}}), Leaf(7))", node)));

    // And its recorded type says so: no node is left typed `[Empty, ..]`.
    let ast = build(&format!("{}List.map(List.repeat(Empty, 2), show)", node));
    let mut checker = TypeChecker::new();
    checker.record_types();
    checker.synth(&ast).unwrap();
    let types: Vec<String> = checker.node_types().values().map(|t| t.to_string()).collect();
    assert!(!types.iter().any(|t| t.starts_with("[Empty")), "{:?}", types);
    assert!(types.iter().any(|t| t == "List(Node)"), "{:?}", types);
}

#[test]
fn a_nominal_annotation_resolves_to_the_nominal() {
    assert_eq!(type_of("Point := { x: I64 }\np : Point\np = Point.{ x: 1 }\np"), "Point");
}

#[test]
fn construction_yields_the_backing_value() {
    // The nominal name is erased: `Str.inspect` shows the bare record, as roc does.
    assert_eq!(
        as_str("Point := { x: I64, y: I64 }\np = Point.{ x: 3, y: 4 }\nStr.inspect(p)"),
        "{ x: 3, y: 4 }"
    );
}

#[test]
fn fields_are_read_with_plain_dot() {
    assert_eq!(
        as_str("Point := { x: I64 }\np = Point.{ x: 7 }\nI64.to_str(p.x)"),
        "7"
    );
}

#[test]
fn a_nominal_union_tag_is_just_the_tag() {
    // `Animal.Dog(x)` builds the same value a bare `Dog(x)` would.
    assert_eq!(
        as_str("Animal := [Dog(Str)]\nStr.inspect(Animal.Dog(\"rex\"))"),
        "Dog(\"rex\")"
    );
}

// --- distinctness, which is the point -------------------------------------

#[test]
fn different_nominals_do_not_interchange() {
    let err = type_error(
        "A := { x: I64 }\nB := { x: I64 }\nf : A -> I64\nf = |p| p.x\nb : B\nb = B.{ x: 1 }\nf(b)",
    );
    assert!(err.contains("different nominal"), "got {}", err);
}

#[test]
fn the_same_nominal_interchanges() {
    assert!(accepts("A := { x: I64 }\nf : A -> I64\nf = |p| p.x\na : A\na = A.{ x: 1 }\nf(a)"));
}

#[test]
fn the_backing_type_is_accepted_where_a_nominal_is_expected() {
    // `:=` is nominal but not opaque. Verified against roc, which accepts this.
    assert!(accepts("Point := { x: I64 }\nf : Point -> I64\nf = |p| p.x\nf({ x: 1 })"));
}

// --- patterns -------------------------------------------------------------

#[test]
fn a_nominal_destructuring_pattern_binds_fields() {
    assert_eq!(
        as_str("Point := { x: I64, y: I64 }\nf = |Point.{ x, y }| I64.to_str(x + y)\nf(Point.{ x: 9, y: 1 })"),
        "10"
    );
}

#[test]
fn a_field_pattern_may_rename_or_match() {
    // `{ x }` is shorthand for `{ x: x }`; an explicit pattern also works.
    assert_eq!(
        as_str("P := { x: I64 }\nf = |p| match p { P.{ x: 0 } => \"zero\" P.{ x } => I64.to_str(x) }\nf(P.{ x: 0 })"),
        "zero"
    );
}

#[test]
fn nominal_union_patterns_match() {
    let f = |v: &str| format!(
        "Animal := [Dog(Str), Cat(Str)]\nf = |a| match a {{ Animal.Dog(n) => \"woof:${{n}}\" Animal.Cat(n) => \"meow:${{n}}\" }}\nf({})",
        v);
    assert_eq!(as_str(&f("Animal.Dog(\"rex\")")), "woof:rex");
    assert_eq!(as_str(&f("Animal.Cat(\"tom\")")), "meow:tom");
}

#[test]
fn lambda_parameters_may_be_patterns() {
    // Needed by `|Point.{ x }|`, and it generalises: a tuple pattern works too.
    assert_eq!(as_str("f = |(a, b)| I64.to_str(a + b)\nf((2, 3))"), "5");
}

// --- exhaustiveness reaches through the nominal ---------------------------

#[test]
fn a_nominal_union_is_exhaustiveness_checked() {
    // The union is the nominal's backing type, so the check applies. roc rejects the
    // same program with "This match expression doesn't cover all possible cases."
    let err = type_error(
        "Animal := [Dog(Str), Cat(Str)]\nf : Animal -> Str\nf = |a| match a { Animal.Dog(n) => n }\nf",
    );
    assert!(err.contains("Cat"), "error should name the missing tag, got {}", err);
}

// --- deferred forms still parse ------------------------------------------

#[test]
fn opaque_declarations_are_accepted_like_nominal_ones() {
    assert!(accepts("Secret :: { key: Str }\nf : Secret -> Str\nf = |s| s.key\nf"));
}

#[test]
fn a_method_block_is_skipped_not_rejected() {
    // Methods need static dispatch; the block is consumed so the rest of the file is
    // still usable.
    assert!(accepts(
        "Animal := [Dog(Str)].{\n    speak = |a| \"woof\"\n}\nf : Animal -> Str\nf = |a| match a { Animal.Dog(n) => n }\nf"
    ));
}

#[test]
fn nominals_are_visible_inside_string_interpolation() {
    // Each `${...}` gets its own sub-parser, which has to inherit the declarations or
    // `Animal.Dog(x)` parses as a qualified call instead of a tag.
    assert_eq!(
        as_str("Animal := [Dog(Str)]\nname = |a| match a { Animal.Dog(n) => n }\n\"got ${name(Animal.Dog(\"rex\"))}\""),
        "got rex"
    );
}

// ==========================================================================
// Nominal method blocks — the rest of phase 14.
//
// Verified against `roc` nightly-2026-09-03:
//   * `Name :: backing.{ ... }` holds ordinary functions, reached as `Name.method(...)`
//   * `value.method(args)` is the same call with the receiver moved in front, exactly
//     as static dispatch does for builtins
//   * `::` is NOT opaque within one file: a raw record is accepted where the nominal
//     is expected, just as with `:=`. Opacity needs module boundaries this interpreter
//     does not have, so the two are treated alike.

const COUNTER: &str = "Counter :: { n: I64 }.{\n    start : Counter\n    start = { n: 0 }\n\n    bump : Counter, I64 -> Counter\n    bump = |c, by| { ..c, n: c.n + by }\n\n    show : Counter -> Str\n    show = |c| c.n.to_str()\n}\n";

// --- the explicit form ----------------------------------------------------

#[test]
fn a_method_is_reached_as_type_dot_method() {
    assert_eq!(as_str(&format!("{}Counter.show(Counter.start)", COUNTER)), "0");
}

#[test]
fn a_zero_argument_method_is_a_plain_value() {
    // `start : Counter` has no parameters, so `Counter.start` is a value not a call.
    assert_eq!(
        as_str(&format!("{}Str.inspect(Counter.start)", COUNTER)),
        "{ n: 0 }"
    );
}

#[test]
fn methods_take_their_arguments_in_order() {
    assert_eq!(
        as_str(&format!("{}Counter.show(Counter.bump(Counter.start, 5))", COUNTER)),
        "5"
    );
}

// --- dispatch -------------------------------------------------------------

#[test]
fn a_method_may_be_dispatched_on_its_receiver() {
    assert_eq!(as_str(&format!("{}c = Counter.start\nc.show()", COUNTER)), "0");
}

#[test]
fn dispatch_and_the_explicit_form_agree() {
    let src = format!("{}c = Counter.bump(Counter.start, 5)\n", COUNTER);
    assert_eq!(as_str(&format!("{}c.show()", src)), as_str(&format!("{}Counter.show(c)", src)));
}

#[test]
fn dispatch_passes_extra_arguments_after_the_receiver() {
    // `c.bump(2)` is `Counter.bump(c, 2)`.
    assert_eq!(
        as_str(&format!("{}c = Counter.start\nc.bump(2).show()", COUNTER)),
        "2"
    );
}

#[test]
fn dispatch_chains_through_methods() {
    assert_eq!(
        as_str(&format!("{}Counter.start.bump(1).bump(2).show()", COUNTER)),
        "3"
    );
}

// --- a method body sees its parameter's type ------------------------------

#[test]
fn a_methods_annotation_gives_its_parameter_a_type() {
    // `show = |c| c.n.to_str()` only works because `show : Counter -> Str` is claimed
    // from inside the block — otherwise `c` is untyped and `c.n` cannot dispatch.
    assert_eq!(as_str(&format!("{}Counter.start.show()", COUNTER)), "0");
}

#[test]
fn field_access_reaches_through_a_nominal() {
    // `p.x` for `p : Point` reads the backing record's field.
    assert_eq!(
        as_str("Point := { x: I64 }\np : Point\np = Point.{ x: 7 }\nI64.to_str(p.x)"),
        "7"
    );
}

// --- ambiguity ------------------------------------------------------------

#[test]
fn a_method_name_two_types_share_is_reported_not_guessed() {
    // Values carry no nominal tag, so dispatch searches by method name. Two matches
    // cannot be told apart, and guessing would silently call the wrong one.
    let src = "A :: { v: I64 }.{\n    show : A -> Str\n    show = |a| a.v.to_str()\n}\n\nB :: { v: I64 }.{\n    show : B -> Str\n    show = |b| b.v.to_str()\n}\n\nx = { v: 1 }\nx.show()";
    let err = eval_error(src);
    assert!(err.contains("ambiguous"), "got {}", err);
    assert!(err.contains("A.show") && err.contains("B.show"), "it should name both: {}", err);
}

#[test]
fn the_explicit_form_is_never_ambiguous() {
    let src = "A :: { v: I64 }.{\n    show : A -> Str\n    show = |a| a.v.to_str()\n}\n\nB :: { v: I64 }.{\n    show : B -> Str\n    show = |b| b.v.to_str()\n}\n\nA.show({ v: 1 })";
    assert_eq!(as_str(src), "1");
}

// --- opacity --------------------------------------------------------------

#[test]
fn opaque_and_nominal_declarations_behave_alike() {
    // roc accepts a raw record where either is expected, within one file.
    assert_eq!(
        as_str("Secret :: { key: Str }.{\n    reveal : Secret -> Str\n    reveal = |s| s.key\n}\nSecret.reveal({ key: \"raw\" })"),
        "raw"
    );
}

#[test]
fn a_method_block_does_not_disturb_the_program_around_it() {
    // The methods are wrapped around the whole program, and only by the OUTERMOST
    // parse — wrapping at every level nested them inside the first method's own body.
    assert_eq!(
        as_str(&format!("{}other = \"kept\"\nother", COUNTER)),
        "kept"
    );
}

// ==========================================================================
// Record field defaults and optional fields — the last of phase 14.
//
// Verified against `roc` nightly-2026-09-03:
//   * `name : Type ?? default` is a DEFAULTED field. Omitting it at construction
//     substitutes the default, so it is always present and read with plain `.name`.
//   * `name ?: Type` is an OPTIONAL field. It may genuinely be absent, so it is read
//     with `.?name`, which yields `Ok(value)` or `Err(MissingField)`.
//   * both are only allowed on a nominal's backing record.
//
// Correction to an earlier note: `.?` does NOT segfault the compiler in general. It
// segfaults when MISUSED on an ordinary field of a plain record — which is not what it
// is for. On a nominal's `?:` field it works correctly.

const CFG: &str = "Cfg := { host: Str, port: U16 ?? 8080 }\n";
const OPT: &str = "Cfg := { host: Str, timeout ?: U64 }\n";

// --- defaults -------------------------------------------------------------

#[test]
fn an_omitted_defaulted_field_gets_its_default() {
    assert_eq!(
        as_str(&format!("{}c = Cfg.{{ host: \"a\" }}\nU16.to_str(c.port)", CFG)),
        "8080"
    );
}

#[test]
fn a_supplied_value_wins_over_the_default() {
    assert_eq!(
        as_str(&format!("{}c = Cfg.{{ host: \"a\", port: 99 }}\nU16.to_str(c.port)", CFG)),
        "99"
    );
}

#[test]
fn a_defaulted_field_needs_no_unwrapping() {
    // It is always present, so it reads like any other field.
    assert_eq!(
        as_str(&format!("{}c = Cfg.{{ host: \"a\" }}\n\"${{c.host}}:${{U16.to_str(c.port)}}\"", CFG)),
        "a:8080"
    );
}

#[test]
fn the_default_is_part_of_the_constructed_record() {
    assert_eq!(
        as_str(&format!("{}Str.inspect(Cfg.{{ host: \"a\" }})", CFG)),
        "{ host: \"a\", port: 8080 }"
    );
}

#[test]
fn defaults_belong_to_their_own_nominal() {
    // Two declarations must not share a scratch list of defaults.
    let src = "A := { x: I64 ?? 1 }\nB := { y: I64 ?? 2 }\n";
    assert_eq!(as_str(&format!("{}Str.inspect(A.{{}})", src)), "{ x: 1 }");
    assert_eq!(as_str(&format!("{}Str.inspect(B.{{}})", src)), "{ y: 2 }");
}

#[test]
fn defaults_are_filled_inside_string_interpolation() {
    // Each `${...}` gets its own sub-parser, which has to inherit the defaults or the
    // omitted field is silently left out.
    assert_eq!(
        as_str(&format!("{}\"v=${{U16.to_str(Cfg.{{ host: \"a\" }}.port)}}\"", CFG)),
        "v=8080"
    );
}

// --- optional fields ------------------------------------------------------

#[test]
fn an_absent_optional_field_reads_as_missing() {
    assert_eq!(
        as_str(&format!("{}f = |c| match c.?timeout {{ Ok(t) => U64.to_str(t) Err(MissingField) => \"none\" }}\nf(Cfg.{{ host: \"a\" }})", OPT)),
        "none"
    );
}

#[test]
fn a_present_optional_field_reads_as_ok() {
    assert_eq!(
        as_str(&format!("{}f = |c| match c.?timeout {{ Ok(t) => U64.to_str(t) Err(MissingField) => \"none\" }}\nf(Cfg.{{ host: \"a\", timeout: 30 }})", OPT)),
        "30"
    );
}

#[test]
fn a_record_without_its_optional_field_still_type_checks() {
    // `?:` means the field may genuinely be absent, so record unification cannot
    // require it.
    assert!(accepts(&format!("{}c : Cfg\nc = Cfg.{{ host: \"a\" }}\nc", OPT)));
}

#[test]
fn a_record_with_its_optional_field_also_type_checks() {
    assert!(accepts(&format!("{}c : Cfg\nc = Cfg.{{ host: \"a\", timeout: 30 }}\nc", OPT)));
}

#[test]
fn a_missing_required_field_is_still_rejected() {
    // Relaxing unification for OPTIONAL fields must not relax it for required ones.
    assert!(
        !accepts("r : { a: I64, b: I64 }\nr = { a: 1 }\nr"),
        "a missing required field should fail"
    );
}

#[test]
fn an_unexpected_field_is_still_rejected() {
    assert!(
        !accepts("r : { a: I64 }\nr = { a: 1, extra: 2 }\nr"),
        "an unexpected field should fail"
    );
}

// --- defaults and optional fields are different ---------------------------

#[test]
fn a_defaulted_field_is_present_while_an_optional_one_may_not_be() {
    let both = "Cfg := { d: I64 ?? 7, o ?: I64 }\n";
    // The default is there...
    assert_eq!(as_str(&format!("{}Str.inspect(Cfg.{{}})", both)), "{ d: 7 }");
    // ...and the optional one is not, which `.?` reports.
    assert_eq!(
        as_str(&format!("{}f = |c| match c.?o {{ Ok(v) => I64.to_str(v) Err(MissingField) => \"absent\" }}\nf(Cfg.{{}})", both)),
        "absent"
    );
}

#[test]
fn node_types_are_recorded_only_when_asked() {
    // Off by default. Asked, every node's type is kept, defaulted as the program will
    // see it: the whole program's, and the list literal's `List(Dec)`.
    let ast = build("xs = [1, 2]\nList.len(xs)");
    let mut quiet = TypeChecker::new();
    quiet.synth(&ast).unwrap();
    assert!(quiet.node_types().is_empty());

    let mut checker = TypeChecker::new();
    checker.record_types();
    let ty = checker.synth(&ast).unwrap();
    let types = checker.node_types();
    assert_eq!(types.get(&ast.id()).map(|t| t.to_string()), Some(checker.defaulted(&ty).to_string()));
    assert!(types.values().any(|t| t.to_string() == "List(Dec)"), "{:?}", types.values().map(|t| t.to_string()).collect::<Vec<_>>());
}

#[test]
fn a_checked_node_keeps_the_type_it_was_checked_against() {
    // `y` is an `I64`; `x : U8` checks it against `U8`, which the checker accepts
    // between integer widths. The use of `y` keeps `U8`, the type it was checked
    // against, not the `I64` it synthesises.
    let ast = build("y : I64\ny = 3\n\nx : U8\nx = y\n\nx");
    let mut checker = TypeChecker::new();
    checker.record_types();
    checker.synth(&ast).unwrap();
    let types = checker.node_types();
    let rocflight::ast::Expr::Let { body, .. } = &ast else { panic!("a let: {}", ast) };
    let rocflight::ast::Expr::Let { name: "x", value, .. } = &**body else { panic!("x: {}", body) };
    assert_eq!(types.get(&value.id()).map(|t| t.to_string()).as_deref(), Some("U8"));
}
