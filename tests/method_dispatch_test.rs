//! A method call must hand its receiver over, not share it. `xs.append(x)` in a loop
//! was quadratic where `List.append(xs, x)` was linear: the dispatch cloned the
//! receiver before calling the builtin, so the list's `Rc` was never unique and every
//! in-place operation (`append`, `concat`, …) copied it.
//!
//! Each case runs at `n` and at `8n`: linear work costs about eight times as long, a
//! quadratic one sixty-four. 24 leaves room for a noisy machine.

use std::process::Command;

fn seconds(test: &str, program: &str) -> f64 {
    let dir = std::env::temp_dir().join(format!("rocflight-method-dispatch-{}-{}", test, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.roc");
    std::fs::write(&file, program).unwrap();
    let started = std::time::Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_rocflight")).arg(&file).output().unwrap();
    let elapsed = started.elapsed().as_secs_f64();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{} failed:\n{}", test, String::from_utf8_lossy(&out.stderr));
    elapsed
}

/// `body` builds `$xs` in a loop of `NN` steps; `len` reads its size back.
fn assert_linear(test: &str, body: &str, len: &str, n: usize) {
    let program = |n: usize| {
        format!(
            "app [main!] {{}}\n\n{}\n\nmain! = |_args| {{\n\techo!(\"${{Str.inspect({})}}\\n\")\n\tOk({{}})\n}}\n",
            body.replace("NN", &n.to_string()),
            len
        )
    };
    // The smaller run first and twice, so neither pays for a cold start.
    seconds(test, &program(n));
    let small = seconds(test, &program(n));
    let large = seconds(test, &program(8 * n));
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
fn append_through_a_method_call_is_linear() {
    assert_linear(
        "append",
        "build = |{}| {\n\tvar $xs = []\n\tfor i in 0..<NN.U64 {\n\t\t$xs = $xs.append(i)\n\t}\n\t$xs\n}",
        "List.len(build({}))",
        2_000,
    );
}

#[test]
fn append_on_a_typed_list_through_a_method_call_is_linear() {
    assert_linear(
        "typed_append",
        "build : U64 -> List(U64)\nbuild = |n| {\n\tvar $xs = List.with_capacity(0)\n\tfor i in 0..<n {\n\t\t$xs = $xs.append(i)\n\t}\n\t$xs\n}\n\nsize = NN",
        "List.len(build(size))",
        2_000,
    );
}

/// `concat` mutates in place through the method form too. (`prepend` is left out on
/// purpose: it moves every element on a contiguous list, in roc as well.)
#[test]
fn concat_through_a_method_call_is_linear() {
    assert_linear(
        "concat",
        "build = |{}| {\n\tvar $xs = []\n\tfor i in 0..<NN.U64 {\n\t\t$xs = $xs.concat([i])\n\t}\n\t$xs\n}",
        "List.len(build({}))",
        2_000,
    );
}

/// `drop_last` on a list nothing else holds cuts it where it stands, method form or
/// not, instead of copying what it keeps.
#[test]
fn drop_last_in_a_loop_is_linear() {
    assert_linear(
        "drop_last",
        "build = |{}| {\n\tvar $xs = List.repeat(0.U64, NN)\n\tfor _i in 0..<NN.U64 {\n\t\t$xs = $xs.drop_last(1)\n\t}\n\t$xs\n}",
        "List.len(build({}))",
        2_000,
    );
}
