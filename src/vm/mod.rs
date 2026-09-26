//! A register VM for Roc, in safe Rust. The engine.
//!
//! It ran alongside a tree-walking interpreter for six phases, gated against it at
//! every step; `Learning.md` §11 has the measurements and the order they landed
//! in. The tree-walker is gone, and this runs every program.
//!
//! Four properties are load-bearing and worth stating before the code:
//!
//! 1. **No `unsafe`.** The crate is `#![forbid(unsafe_code)]`. Register access is
//!    bounds-checked indexing, dispatch is a `match`, and frames are indices rather
//!    than pointers. A wrong opcode is a panic with a message, not memory corruption.
//! 2. **Names are resolved at compile time.** A local is a register index, a captured
//!    variable is a capture index, a top-level value is a slot, a top-level function is
//!    a chunk id. Nothing compares a string at run time, which is the whole reason this
//!    is faster than walking the tree.
//! 3. **Calls do not recurse in Rust.** `frames` is an ordinary `Vec`, so Roc recursion
//!    costs ~32 bytes a level on the heap instead of a Rust stack frame, and going too
//!    deep is a Roc-level error rather than a stack overflow with no diagnostic. The
//!    exception is a builtin's callback, which re-enters through `call_closure` — see
//!    there.
//! 4. **A tail call reuses its frame.** Tail-recursive Roc runs in constant memory, so
//!    the idiomatic functional loop stops being a depth risk at all.

pub mod compile;
mod liveness;
mod peephole;

pub use compile::{compile, compile_unit};

use crate::ast::BinOp;
use crate::error::EvalError;
use crate::eval::Value;
use std::cell::RefCell;
use std::rc::Rc;

/// Top-level values, by slot.
///
/// Shared rather than owned: a builtin's callback (`xs.map(f)`) re-enters the VM
/// through `call_closure`, and that second machine has to see the same top level.
type Globals = Rc<RefCell<Vec<Option<Value>>>>;

thread_local! {
    /// The program and globals of the VM currently running, so `eval::apply` can call
    /// a `Value::Closure` from inside a builtin. A stack, because a callback can
    /// itself call a builtin that takes a callback.
    static RUNNING: RefCell<Vec<(Rc<Program>, Globals)>> = const { RefCell::new(Vec::new()) };
}

/// Run `f` against the program currently running, if there is one.
///
/// A borrow rather than a clone of the `Rc`: these lookups happen inside `Str.inspect`
/// and `==`, and the program is not going anywhere while an instruction of its own is
/// executing.
fn with_running<R>(f: impl FnOnce(&Program) -> R) -> Option<R> {
    RUNNING.with(|r| r.borrow().last().map(|(program, _)| f(program)))
}

/// Every `Type.method` function in the running program, for a given method name.
///
/// A nominal's method block compiles to top-level functions whose names carry a dot,
/// and the compiler indexed those by bare method name in `Program::methods_by_name` —
/// so this is a table lookup, not a scan of every chunk with a formatted suffix.
///
/// Used by the two builtins that dispatch on a user's own method: `Str.inspect` looking
/// for a `to_inspect`, and operator dispatch looking for `plus`/`is_eq`/…
pub fn methods_named(method: &str, receiver: &Value) -> Vec<(&'static str, Value)> {
    ranked_methods(method, receiver).into_iter().map(|(_, name, value)| (name, value)).collect()
}

/// The one method meant for `receiver`, when the shapes can say: the only candidate,
/// or the most specific of those that fit the value EXACTLY. `None` when several
/// merely admit it — the tree of `Try.is_eq` answering every `==` is what that avoids.
pub fn best_method(method: &str, receiver: &Value) -> Option<(&'static str, Value)> {
    let ranked = ranked_methods(method, receiver);
    match ranked.len() {
        0 => None,
        1 => ranked.into_iter().next().map(|(_, name, value)| (name, value)),
        _ => ranked.into_iter().next().filter(|((exact, _), ..)| *exact).map(|(_, name, value)| (name, value)),
    }
}

/// `methods_named` with each candidate's rank: an exact shape fit, and the nominal's
/// depth. Most specific first.
fn ranked_methods(method: &str, receiver: &Value) -> Vec<((bool, u8), &'static str, Value)> {
    with_running(|program| {
        let Some(defined) = program.methods_by_name.get(method) else { return Vec::new() };
        defined
            .iter()
            // Only the nominals whose shape does not RULE the receiver out. Without
            // this a lone `Try.is_eq` in scope answered every `==`, tuples and unrelated
            // tags included, and `(1, "x") == (1, "x")` failed with "No match arm
            // matched".
            .filter(|(qualified, _)| match qualified.rsplit_once('.') {
                Some((owner, _)) => program
                    .nominal_shapes
                    .get(owner)
                    .is_none_or(|shape| shape.admits(receiver)),
                None => true,
            })
            .map(|(qualified, chunk)| {
                // The most specific first: an EXACT shape fit before a mere
                // admission, and a nominal declared over another before that other.
                let owner = qualified.split('.').next().unwrap_or(qualified);
                let exact = program.nominal_shapes.get(owner).is_some_and(|s| s.is_exactly(receiver));
                let depth = program.nominal_depth.get(owner).copied().unwrap_or(0);
                ((exact, depth), *qualified, Value::Closure(Rc::clone(&program.chunks[*chunk as usize].bare)))
            })
            .collect::<Vec<_>>()
    })
    .unwrap_or_default()
    .into_iter()
    .fold(Vec::new(), |mut sorted: Vec<((bool, u8), &'static str, Value)>, item| {
        let at = sorted.iter().position(|(rank, ..)| *rank < item.0).unwrap_or(sorted.len());
        sorted.insert(at, item);
        sorted
    })
}

/// Call a VM closure from outside the VM — from a builtin's callback.
///
/// `List.map` and friends are written against `eval::call_function`, so a callback
/// re-enters the VM here. The cost is that a callback nests a Rust frame: "calls do not
/// recurse in Rust" holds for Roc calls but NOT across a builtin's callback boundary,
/// so deeply nested `map`-inside-`map` is bounded by the Rust stack again. Lowering the
/// callback-taking builtins into bytecode is what removes the last such bound.
/// A literal pattern against a value that is a nominal built from literals — which
/// nominal, its shape says — is the nominal's conversion of the literal, compared
/// with its `is_eq` where it has one and structurally where that is derived.
/// A nominal iterable's `iter` method applied to it: the list or iterator to loop.
fn call_iter_method(program: &Rc<Program>, value: &Value) -> Result<Option<Value>, EvalError> {
    let owner = program.methods.iter().find(|((module, m), _)| {
        *m == "iter" && program.nominal_shapes.get(module).is_some_and(|shape| shape.is_exactly(value))
    });
    match owner {
        Some((_, chunk)) => Ok(Some(call_closure(&program.chunks[*chunk as usize].bare, vec![value.clone()])?)),
        None => Ok(None),
    }
}

fn nominal_literal_matches(program: &Rc<Program>, pattern: &crate::ast::Pattern, value: &Value) -> Result<bool, EvalError> {
    use crate::ast::Pattern;
    let (method, literal) = match pattern {
        Pattern::Int(n) => ("from_numeral", crate::eval::numeral::numeral_from_value(&Value::Int(*n))),
        Pattern::Float(f, _) => ("from_numeral", crate::eval::numeral::numeral_from_value(&Value::Float(*f))),
        Pattern::Str(s) => ("from_quote", Some(Value::Str(Rc::from(*s)))),
        _ => return Ok(false),
    };
    let Some(literal) = literal else { return Ok(false) };
    let owner = program.methods.iter().find(|((module, m), _)| {
        *m == method && program.nominal_shapes.get(module).is_some_and(|shape| shape.is_exactly(value))
    });
    let Some(((module, _), chunk)) = owner else { return Ok(false) };
    let converted = match call_closure(&program.chunks[*chunk as usize].bare, vec![literal])? {
        Value::Tag("Ok", payload) if payload.len() == 1 => payload[0].clone(),
        other => return Err(EvalError { message: format!("No match arm matched {}", other) }),
    };
    match program.methods.get(&(module, "is_eq")) {
        Some(is_eq) => Ok(matches!(
            call_closure(&program.chunks[*is_eq as usize].bare, vec![value.clone(), converted])?,
            Value::Bool(true)
        )),
        None => Ok(crate::eval::values_equal(value, &converted)),
    }
}

pub fn call_closure(closure: &Rc<Closure>, args: Vec<Value>) -> Result<Value, EvalError> {
    let context = RUNNING.with(|r| r.borrow().last().cloned());
    let (program, globals) = context.ok_or_else(|| EvalError {
        message: "vm: a closure was called with no VM running".to_string(),
    })?;
    // A fresh register file per callback. Pooling them across callbacks was measured
    // on `iter_range` (two million of these) and saved nothing: the allocation is not
    // where a callback's time goes. Lowering `fold` and `map` into bytecode is.
    let mut vm = Vm { program, regs: Vec::new(), frames: Vec::new(), globals, entry_args: None };
    vm.call(closure, args)
}

/// A register, relative to the frame's base.
pub type Reg = u16;

/// An index into `Program::chunks`.
pub type ChunkId = u32;

/// One instruction. Three-address: operands in, one destination out.
///
/// `Bin` carries its `BinOp` rather than there being one opcode per operator. Splitting
/// it into `AddInt`/`AddFrac`/`AddAny` using what the type checker proved is a
/// *measured* step for later; doing it now would mean two implementations of Roc's
/// arithmetic semantics before there is a single benchmark to prove it pays.
#[derive(Debug, Clone, Copy)]
pub enum Op {
    /// `dst = consts[k]`
    LoadK { dst: Reg, k: u32 },
    /// `dst = src`
    Move { dst: Reg, src: Reg },
    /// `dst = src`, and `src` is left empty — the last read of a value takes it
    /// instead of cloning it, so a record or a list nothing else holds can then be
    /// changed in place. `vm::liveness` is what proves the read is the last one, and
    /// a separate opcode rather than a flag keeps the branch out of `Move`, which is
    /// the most executed instruction there is.
    MoveTake { dst: Reg, src: Reg },
    /// `dst = globals[idx]`
    LoadGlob { dst: Reg, idx: u32 },
    /// `globals[idx] = src`
    StoreGlob { idx: u32, src: Reg },
    /// `dst = this closure's captures[idx]`
    LoadCap { dst: Reg, idx: u16 },
    /// `dst = Cell(src)`: a `var` a closure captures lives in a shared cell.
    MakeCell { dst: Reg, src: Reg },
    /// `dst = *cell`
    CellGet { dst: Reg, cell: Reg },
    /// `*cell = src`
    CellSet { cell: Reg, src: Reg },
    /// `dst = the closure that is running`.
    ///
    /// How a block-local function calls itself. The tree-walker rebound a rebuilt
    /// closure into every call frame to tie that knot; the running closure is already
    /// in the frame, so this is a refcount bump and no analysis.
    LoadSelf { dst: Reg },
    /// `dst = a op b`
    Bin { dst: Reg, a: Reg, b: Reg, op: BinOp },
    /// `dst = a op b`, where the checker proved both operands are integers.
    ///
    /// Skips the dispatch on operand shapes that `Bin` pays on every execution: the
    /// answer to "what are these?" was known once, at compile time. Emitted only where
    /// `TypeChecker::integer_binops` says so, and it falls back to the generic path if
    /// a value turns out not to be an integer after all — being right matters more
    /// than the assumption being kept.
    /// `width` is the operands' integer width as `bits | (signed << 7)`, or 0 when the
    /// checker did not name one; a result outside it is roc's overflow crash.
    BinInt { dst: Reg, a: Reg, b: Reg, op: BinOp, width: u8 },
    /// `Bin`/`BinInt` whose RIGHT operand is a literal, read straight from `consts`
    /// rather than loaded into a register first. `x + 1` was two instructions and is
    /// one; `vm::compile` fuses them where the literal's register is a temporary it
    /// allocated for exactly that operand.
    BinK { dst: Reg, a: Reg, k: u32, op: BinOp },
    BinIntK { dst: Reg, a: Reg, k: u32, op: BinOp, width: u8 },
    /// `ip = to`
    Jump { to: u32 },
    /// `if cond == False { ip = to }`. The condition must be a `Bool`, and `kind`
    /// decides only what a non-`Bool` is told — roc words the three cases differently
    /// and the tree-walker followed it, so the VM has to as well.
    JumpFalse { cond: Reg, to: u32, kind: CondKind },
    /// `dst = closure(chunk, regs[base..base + n])` — the captures are already in
    /// consecutive registers, put there by the enclosing function.
    MakeClosure { dst: Reg, chunk: ChunkId, base: Reg, n: u16 },
    /// `dst = chunk(regs[base..base + argc])`, the callee known at compile time.
    ///
    /// The arguments are already in consecutive registers, and the callee's frame
    /// starts at `base` — so a call passes no argument list at all, which is the
    /// per-call `Vec<Value>` the tree-walker allocated. A top-level function needs no
    /// closure value either, so a direct call allocates nothing whatsoever.
    CallFn { dst: Reg, chunk: ChunkId, base: Reg, argc: u16 },
    /// `dst = regs[func](regs[base..base + argc])`, the callee a value in a register.
    Call { dst: Reg, func: Reg, base: Reg, argc: u16 },
    /// A call in tail position: reuse this frame instead of pushing one.
    ///
    /// This is what makes a tail-recursive Roc function a loop. The callee is `chunk`
    /// when it is known at compile time, and the value in `func` otherwise.
    TailCall { func: Reg, chunk: Option<ChunkId>, base: Reg, argc: u16 },
    /// Return `src` to the caller.
    Ret { src: Reg },

    // ---- aggregates: built from values already in consecutive registers ----
    /// `dst = [regs[base .. base + n]]`
    MakeList { dst: Reg, base: Reg, n: u16 },
    /// `dst = (regs[base .. base + n])`
    MakeTuple { dst: Reg, base: Reg, n: u16 },
    /// `regs[list].push(regs[src])`, in place when nothing else holds the list —
    /// which is so for the list a compiled `map` is building. `src` is left `Unit`.
    ListPush { list: Reg, src: Reg },
    /// `dst = Name(regs[base .. base + n])`, the name from this chunk's `names`.
    MakeTag { dst: Reg, name: u16, base: Reg, n: u16 },
    /// `dst = { names[name .. name + n]: regs[base .. base + n] }`, in that order.
    MakeRecord { dst: Reg, name: u16, base: Reg, n: u16 },
    /// `dst = { ..regs[obj], names[name .. name + n]: regs[base .. base + n] }`.
    ///
    /// A new record; roc has no mutation. Updating a field the record does not have is
    /// an error, which is why this is not just a sequence of stores.
    /// `take` says `obj`'s register is dead after this instruction, so the record is
    /// moved out of it rather than cloned and `Rc::make_mut` mutates it in place.
    UpdateRecord { dst: Reg, obj: Reg, name: u16, base: Reg, n: u16, take: bool },
    /// `dst = regs[obj].field`, the field name from `names`.
    GetField { dst: Reg, obj: Reg, name: u16 },
    /// `dst = Ok(regs[obj].field)`, or `Err(MissingField)` when it is absent.
    GetOptField { dst: Reg, obj: Reg, name: u16 },
    /// `dst = regs[obj].i` — a tuple's positional element.
    GetIndex { dst: Reg, obj: Reg, i: u16 },

    // ---- pattern tests: each jumps to `to` when the value does NOT match ----
    /// A literal pattern from this chunk's `pats`.
    TestLit { obj: Reg, pat: u16, to: u32 },
    /// `TestLit`, unless the value is a nominal built from literals: then the
    /// literal is converted through that nominal and compared with its `is_eq`.
    TestLitDyn { obj: Reg, pat: u16, to: u32 },
    /// Match `obj` against the string pattern `pat`, writing its captures into
    /// `base`, `base+1`, …; jump to `to` if it does not match.
    TestStr { obj: Reg, pat: u16, base: Reg, to: u32 },
    /// A tag with this name and this many payload elements.
    TestTag { obj: Reg, name: u16, n: u16, to: u32 },
    /// A tuple of exactly this length.
    TestTuple { obj: Reg, n: u16, to: u32 },
    /// A record — which fields it needs is checked by `GetFieldOr`.
    TestRecord { obj: Reg, to: u32 },
    /// A list, of exactly `n` elements, or at least `n` when `exact` is false.
    TestList { obj: Reg, n: u16, exact: bool, to: u32 },
    /// Jump to `to` unless `cond` holds exactly `Bool(want)`.
    ///
    /// Deliberately NOT `JumpFalse`, which errors on anything that is not a `Bool`.
    /// The predicate of a compiled `keep_if`, `any` or `all` has to answer the way the
    /// builtin does, and the builtin asks `matches!(value, Bool(b) if b == want)` — so a
    /// non-`Bool` is simply not a match there, never a message. Compiling the predicate
    /// to `JumpFalse` would have invented an error the interpreter does not have.
    TestBool { cond: Reg, want: bool, to: u32 },
    /// No arm matched `obj`. Always an error; roc's own exhaustiveness check is what
    /// normally makes this unreachable.
    NoMatch { obj: Reg },

    // ---- destructuring: valid once the matching test above has passed ----
    /// `dst = regs[obj]`'s tag payload element `i`.
    GetPayload { dst: Reg, obj: Reg, i: u16 },
    /// `dst = regs[obj].field`, or jump to `to` when the record has no such field.
    GetFieldOr { dst: Reg, obj: Reg, name: u16, to: u32 },
    /// `dst =` the record's fields that `names[name .. name + n]` does not name.
    GetRest { dst: Reg, obj: Reg, name: u16, n: u16 },
    /// `dst = regs[obj][i]`, counting from the END when `from_end`.
    GetElem { dst: Reg, obj: Reg, i: u16, from_end: bool },
    /// `dst =` the list minus `front` elements at the start and `back` at the end —
    /// what `[a, .. as middle, b]` binds.
    GetSlice { dst: Reg, obj: Reg, front: u16, back: u16 },

    // ---- loops ----
    /// `dst = start..end`, inclusive or not. A range is NOT a list: roc keeps it
    /// opaque, and `IterNext` walks it without ever building one.
    MakeRange { dst: Reg, start: Reg, end: Reg, inclusive: bool },
    // ---- builtins, dispatch, interpolation ----
    /// `dst = Module.name(regs[base .. base + argc])`, a builtin of the interpreter's.
    ///
    /// The implementations live in `crate::eval`, take and return `Value`s, and hold
    /// no interpreter state — which is why they outlived the tree-walker they were
    /// written for.
    CallBuiltin { dst: Reg, name: u16, base: Reg, argc: u16 },
    /// `dst = name(regs[base .. base + argc])`, an effect of the default host.
    CallHost { dst: Reg, name: u16, base: Reg, argc: u16 },
    /// `dst = Module.name` as a VALUE — `xs.map(Str.inspect)`.
    MakeBuiltin { dst: Reg, name: u16 },
    /// `dst = regs[base].method(regs[base + 1 .. base + argc])`.
    ///
    /// Which builtin that is depends on the receiver's type at RUN time, so unlike a
    /// nominal's own method — which the compiler resolves to a chunk — this one cannot
    /// be settled earlier.
    DispatchMethod { dst: Reg, name: u16, base: Reg, argc: u16 },
    /// `dst =` the literals `names[name .. name + n + 1]` with `regs[base .. base + n]`
    /// interleaved between them: `"a${x}b"` is two literals and one value.
    Interp { dst: Reg, name: u16, base: Reg, n: u16 },
    /// `dst = a op b`, but a nominal's own operator method first.
    ///
    /// Only emitted for a program that actually defines one (`plus`, `is_eq`, …), so
    /// the ordinary `Bin` stays a jump into the operator table and nothing else.
    BinDispatch { dst: Reg, a: Reg, b: Reg, op: BinOp },

    // ---- statements ----
    /// `expect cond` inside a function body: a runtime assertion. Reports a failure to
    /// stderr and carries on — roc does not abort on a failed expectation — and makes
    /// the run exit non-zero. It is not a test, so it is never tallied.
    Expect { cond: Reg },
    /// A TOP-LEVEL `expect`, which is a test. Only emitted under `--test`, and tallied
    /// there; a normal run does not compile it at all.
    TestExpect { cond: Reg },
    /// `dbg value`, to stderr.
    /// `shape`: the value's type as `eval::inspect_as` reads it, `{}` if unknown.
    Dbg { src: Reg, shape: Reg },
    /// `crash message`. Always an error.
    Crash { src: Reg },

    /// The next element of `iter`, with `idx` holding the position reached so far.
    ///
    /// `dst` gets the element and `idx` advances; when there is nothing left, jump to
    /// `to`. Over a range this computes the element instead of reading one, so
    /// `for i in 0..<10_000_000` allocates nothing — the tree-walker's own fix for that
    /// was to special-case ranges in `for`, and this is the same idea as an opcode.
    IterNext { dst: Reg, iter: Reg, idx: Reg, to: u32 },
    /// The same step, run as a loop's BACK EDGE: `to` is the body rather than the exit,
    /// so an element jumps back into the loop and exhaustion falls through. It replaces
    /// a `Jump` that only existed to reach an `IterNext`, which is a third of everything
    /// a `for` loop over a range executes. `vm::peephole` is what proves the shape.
    IterNextBack { dst: Reg, iter: Reg, idx: Reg, to: u32 },
}

/// Which construct a conditional jump came from, so its error can say so.
#[derive(Debug, Clone, Copy)]
pub enum CondKind {
    If,
    Guard,
    While,
    /// The left side of `and` / `or`, which decides whether the right side runs.
    Operand,
}

impl CondKind {
    fn expected_bool(self, got: &Value) -> EvalError {
        EvalError {
            message: match self {
                CondKind::If => format!("An if condition must be a Bool, got {}", got),
                CondKind::Guard => format!("A match guard must be a Bool, got {}", got),
                CondKind::While => format!("A `while` condition must be a Bool, got {}", got),
                CondKind::Operand => format!("`and` and `or` need Bool operands, got {}", got),
            },
        }
    }
}

/// One compiled function: flat code, its own constants, a known frame size.
#[derive(Debug)]
pub struct Chunk {
    pub code: Vec<Op>,
    pub consts: Vec<Value>,
    /// Frame size. Known at compile time, so a call grows the register file once.
    pub n_regs: u16,
    pub arity: u16,
    /// Parameter names. Shared with the AST node, and used only to render a function
    /// value as `<lambda |x, y|>` — the same text the tree-walker produced.
    pub params: Rc<[&'static str]>,
    /// Field and tag names, by index. Names are compared by content at run time, which
    /// is the one exception to "no string comparison at run time" and is deliberate:
    /// resolving a field to a SLOT was measured twice and bought nothing, because
    /// `GetField`'s cost is the dispatch and the `Value` clone, not reaching the name
    /// (Learning.md §12).
    pub names: Vec<&'static str>,
    /// Literal patterns, by index. Only `Int`, `Float` and `Str` ever land here: every
    /// other pattern is compiled into tests and destructuring ops.
    pub pats: Vec<crate::ast::Pattern>,
    /// For errors and `dbg` only — never looked up by.
    pub name: &'static str,
    /// The AST node each instruction came from, parallel to `code`.
    ///
    /// This is what a runtime error is located by: the instruction that failed knows
    /// its node, the node knows its offset, and the offset knows its line. It was
    /// impossible until the AST had node identity — there was nothing for an
    /// instruction to point at.
    pub spans: Vec<crate::ast::NodeId>,
    /// This chunk as a closure over nothing, made once at compile time.
    ///
    /// A top-level function captures nothing, so the closure it runs under is the same
    /// object every time. Building it per call — which is what the first draft did —
    /// put a heap allocation on the hot path and cost 4% of `fib`.
    pub bare: Rc<Closure>,
}

/// A function value: which chunk to run, and what it captured.
///
/// Captures are by VALUE and decided at compile time, so there is no environment, no
/// `RefCell` chain and no scope vector — the three things that made the tree-walker's
/// closures expensive. A `var` that is captured *and* assigned needs a shared cell
/// instead; `var` is V3, and so is that.
#[derive(Debug)]
pub struct Closure {
    pub chunk: ChunkId,
    /// Copied from the chunk, so rendering a function value needs no program.
    pub params: Rc<[&'static str]>,
    pub captures: Vec<Value>,
}

/// A whole compiled program.
#[derive(Debug)]
pub struct Program {
    pub chunks: Vec<Chunk>,
    /// Numeric literals a nominal's `from_numeral` converts, with the nominal: roc
    /// folds each at compile time, so one the conversion rejects is a compile problem
    /// even in a function nothing calls. Checked before the program runs.
    pub literal_coercions: Vec<(&'static str, Value)>,
    /// Top-level bindings whose value is not a function.
    pub n_globals: usize,
    /// The chunk holding the top level itself.
    pub top: ChunkId,
    /// The BUILTINS' top level, when they came in already compiled: it binds their
    /// globals and must run before this program's own. See `artifact::Prefix`.
    pub prelude: Option<ChunkId>,
    /// The app's entry point, if it declared one, and its arity.
    pub entry: Option<(ChunkId, u16)>,
    /// Top-level methods by `(module, method)` — `("Dict", "insert")`.
    ///
    /// The compiler resolves a dispatch itself when the checker names the receiver's
    /// module. When it cannot, only the running value knows, and this is what it looks
    /// the method up in.
    pub methods: std::collections::HashMap<(&'static str, &'static str), ChunkId>,
    /// The same methods by bare name. A user nominal is a plain record at run time —
    /// roc erases nominals — so a receiver whose module cannot be named falls back to
    /// a method of that name, provided only one type defines it.
    pub methods_by_name: std::collections::HashMap<&'static str, Vec<(&'static str, ChunkId)>>,
    /// What each nominal's backing type LOOKS like, by name.
    ///
    /// roc erases nominals, so a value cannot say which one it is. Its shape can rule
    /// one out, though, and that is enough: a tuple is not a `Try`, so `Try.is_eq` must
    /// not answer `(1, "x") == (1, "x")`. This is the runtime half of nominal identity
    /// — the compiler resolves it from the type wherever the checker knows one.
    pub nominal_shapes: std::collections::HashMap<&'static str, NominalShape>,
    /// How many nominals each nominal is declared over: `Set :: Dict(…)` is 1, `Dict`
    /// is 0. Two candidates of one shape are told apart by it — the wrapper is meant.
    pub nominal_depth: std::collections::HashMap<&'static str, u8>,
    /// The shapes of the nominals declared with `::`, the OPAQUE form.
    ///
    /// roc shows one as `<opaque>` instead of its backing value, and that is the only
    /// place opacity is observable inside a single file.
    pub opaque_shapes: Vec<NominalShape>,
}

/// The shape of a nominal's backing type, as far as a runtime value can be compared.
#[derive(Debug, Clone)]
pub enum NominalShape {
    Tags(Vec<String>),
    /// The field names, each with the coarse kind its declared type implies — what
    /// tells `{ value : F32 }` from `{ value : F64 }` when the names alone cannot.
    Fields(Vec<(String, FieldKind)>),
    Tuple(usize),
    /// A SIMD backing — `Vector := U64x2`. Holds the element-kind byte so a
    /// `Value::Simd` of the right width is recognised as this nominal.
    Simd(u8),
    /// A backing that is a list or a scalar — `Text :: List(U8)`, `Code :: I64`. It
    /// rules out every value of another kind, and is never an exact fit, since a
    /// plain list or integer is indistinguishable from it at run time: a list keeps
    /// answering to `List`'s own methods.
    Kind(FieldKind),
    /// A backing this cannot rule anything out from — a type variable, or another
    /// nominal whose own shape is unknown. Never filters.
    Unknown,
}

/// What a declared field type says about the value it holds, as coarsely as a
/// `Value` variant: enough to break a tie between two nominals of one field list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Any,
    Int,
    Float,
    F32,
    Dec,
    Str,
    Bool,
    List,
    Record,
    Tag,
    Tuple,
}

impl FieldKind {
    pub(crate) fn of(ty: &crate::types::Type) -> FieldKind {
        use crate::types::Type;
        match ty {
            Type::U8 | Type::U16 | Type::U32 | Type::U64 | Type::U128
            | Type::I8 | Type::I16 | Type::I32 | Type::I64 | Type::I128 => FieldKind::Int,
            Type::F64 => FieldKind::Float,
            Type::F32 => FieldKind::F32,
            Type::Dec => FieldKind::Dec,
            Type::Str => FieldKind::Str,
            Type::Bool => FieldKind::Bool,
            Type::List(_) => FieldKind::List,
            Type::Record { .. } => FieldKind::Record,
            Type::TagUnion { .. } => FieldKind::Tag,
            Type::Tuple(_) => FieldKind::Tuple,
            Type::Nominal { backing, .. } => FieldKind::of(backing),
            _ => FieldKind::Any,
        }
    }

    fn holds(self, value: &Value) -> bool {
        match (self, value) {
            (FieldKind::Any, _) => true,
            (FieldKind::Int, Value::Int(_)) => true,
            (FieldKind::Float, Value::Float(_)) => true,
            (FieldKind::F32, Value::F32(_)) => true,
            (FieldKind::Dec, Value::Dec(_)) => true,
            (FieldKind::Str, Value::Str(_)) => true,
            (FieldKind::Bool, Value::Bool(_)) => true,
            (FieldKind::List, Value::List(_) | Value::Range { .. }) => true,
            (FieldKind::Record, Value::Record(_) | Value::Unit) => true,
            (FieldKind::Tag, Value::Tag(..) | Value::Bool(_)) => true,
            (FieldKind::Tuple, Value::Tuple(_)) => true,
            _ => false,
        }
    }
}

/// A top-level method by its qualified name, as a callable value.
///
/// Unlike `methods_named` this needs no receiver: `Json.parse` looks up a nominal's
/// `parser_for` knowing only the type it must produce.
pub fn method_by_name(module: &str, method: &str) -> Option<Value> {
    with_running(|program| {
        let chunk = *program.methods.get(&(module, method))?;
        Some(Value::Closure(Rc::clone(&program.chunks[chunk as usize].bare)))
    })
    .flatten()
}

/// A nominal's own method by its owner's name, `Color.to_inspect`: for a caller that
/// knows from the TYPE which nominal a value is, where `methods_named` guesses from
/// its shape.
/// Answers the method's qualified name with it.
pub fn method_of(owner: &str, method: &str) -> Option<(&'static str, Value)> {
    with_running(|program| {
        program.methods_by_name.get(method)?.iter().find_map(|(qualified, chunk)| {
            let of = qualified.strip_suffix(method)?.strip_suffix('.')?;
            (of == owner).then(|| (*qualified, Value::Closure(Rc::clone(&program.chunks[*chunk as usize].bare))))
        })
    })
    .flatten()
}

/// Is `value` exactly the shape of a nominal the running program declared opaque?
///
/// Asked for every record, tag and tuple `Str.inspect` renders, nested ones included,
/// so it borrows the shapes rather than copying them out each time.
pub fn is_opaque(value: &Value) -> bool {
    with_running(|program| program.opaque_shapes.iter().any(|shape| shape.is_exactly(value)))
        .unwrap_or(false)
}

impl NominalShape {
    /// Is `value` EXACTLY this shape — the same fields, no more?
    ///
    /// Stricter than `admits`, which only rules a value out. Opacity is decided from
    /// the shape because roc erases nominals and the value cannot say which one it is,
    /// so a record that happens to have the same fields as an opaque nominal reads as
    /// opaque too. `ponytail: the alternative is a `Value` that carries its nominal,
    /// which every other operation would then have to see through.`
    pub fn is_exactly(&self, value: &Value) -> bool {
        match (self, value) {
            (NominalShape::Fields(declared), Value::Record(fields)) => {
                declared.len() == fields.len()
                    && declared.iter().all(|(want, _)| fields.iter().any(|(have, _)| have == want))
            }
            (NominalShape::Tags(names), Value::Tag(tag, _)) => names.iter().any(|n| n == tag),
            (NominalShape::Tuple(n), Value::Tuple(items)) => items.len() == *n,
            (NominalShape::Simd(kind), Value::Simd { kind: k, .. }) => k == kind,
            _ => false,
        }
    }

    /// `is_exactly`, and each field holds what its declared type says: the tie-break
    /// between `{ value : F32 }` and `{ value : F64 }`, which share a field list.
    pub fn holds_kinds(&self, value: &Value) -> bool {
        match (self, value) {
            (NominalShape::Fields(declared), Value::Record(fields)) => declared.iter().all(|(want, kind)| {
                fields.iter().any(|(have, held)| have == want && kind.holds(held))
            }),
            _ => true,
        }
    }

    /// Could `value` be of this nominal? `false` only when the shape RULES IT OUT.
    pub fn admits(&self, value: &Value) -> bool {
        match (self, value) {
            (NominalShape::Tags(names), Value::Tag(tag, _)) => names.iter().any(|n| n == tag),
            (NominalShape::Tags(_), _) => false,
            (NominalShape::Fields(declared), Value::Record(fields)) => {
                declared.iter().all(|(want, _)| fields.iter().any(|(have, _)| have == want))
            }
            (NominalShape::Fields(_), _) => false,
            (NominalShape::Tuple(n), Value::Tuple(items)) => items.len() == *n,
            (NominalShape::Tuple(_), _) => false,
            (NominalShape::Simd(kind), Value::Simd { kind: k, .. }) => k == kind,
            (NominalShape::Simd(_), _) => false,
            (NominalShape::Kind(kind), value) => kind.holds(value),
            (NominalShape::Unknown, _) => true,
        }
    }
}

/// Could `value` be the nominal that owns `qualified`? A shape that is not known
/// admits anything, so this only ever rules a method OUT.
fn admits_receiver(program: &Program, qualified: &str, value: &Value) -> bool {
    let owner = qualified.split('.').next().unwrap_or(qualified);
    program.nominal_shapes.get(owner).is_none_or(|shape| shape.admits(value))
}

/// The shape of a type, for `NominalShape`.
pub fn shape_of(ty: &crate::types::Type) -> NominalShape {
    use crate::types::Type;
    match ty {
        Type::TagUnion { tags, .. } => {
            NominalShape::Tags(tags.iter().map(|(name, _)| (*name).to_string()).collect())
        }
        Type::Record { fields, .. } => {
            NominalShape::Fields(fields.iter().map(|(name, ty)| ((*name).to_string(), FieldKind::of(ty))).collect())
        }
        Type::Tuple(items) => NominalShape::Tuple(items.len()),
        // A U128 may be held as `Value::U128`, which `FieldKind::Int` does not
        // hold, so it rules nothing out.
        Type::List(_) | Type::Str | Type::Bool | Type::F32 | Type::F64 | Type::Dec
        | Type::U8 | Type::U16 | Type::U32 | Type::U64
        | Type::I8 | Type::I16 | Type::I32 | Type::I64 | Type::I128 => NominalShape::Kind(FieldKind::of(ty)),
        Type::Nominal { name, backing, .. } => match crate::eval::simd_kind(name) {
            Some(kind) => NominalShape::Simd(kind),
            None => shape_of(backing),
        },
        _ => NominalShape::Unknown,
    }
}

/// A suspended caller: where to resume, and where to put the result.
///
/// The tree-walker spent many Rust frames per Roc call, which is why it needed a 256 MB
/// stack to recurse a few hundred levels; this is a `Vec` push.
#[derive(Debug)]
struct Frame {
    chunk: ChunkId,
    ip: u32,
    base: u32,
    dst: Reg,
    /// The closure that was running, for `LoadCap` and `LoadSelf` after the return.
    closure: Rc<Closure>,
}

/// How deep Roc recursion may go before it is an error rather than an ambition.
///
/// This exists so that runaway recursion produces a message. It is not a stack limit —
/// frames are heap-allocated — so it can be generous: a million frames is ~32 MB. A
/// tail-recursive function does not count against it at all.
const MAX_FRAMES: usize = 1_000_000;

/// Compile an AST and run it, in one call.
///
/// The ordinary way to run a single-file program: the compile step is not separately
/// interesting to a caller who just wants the answer.
pub fn eval(ast: &crate::ast::Expr) -> Result<Value, EvalError> {
    let program = Rc::new(compile(ast, None).map_err(|message| EvalError { message })?);
    run(&program)
}

/// Run a compiled program's top level, then its entry point if it has one.
///
/// The return value is the entry point's, or the top level's if there is none — which
/// is what a module is.
/// Run each `from_numeral` a literal needs, the way roc's compile-time evaluation
/// does; see `Program::literal_coercions`. The message is what `main` reports as a
/// compile problem.
fn fold_literal_coercions(program: &Program) -> Result<(), EvalError> {
    for (nominal, literal) in &program.literal_coercions {
        let Some(chunk) = program.methods.get(&(*nominal, "from_numeral")) else { continue };
        let Some(numeral) = crate::eval::numeral::numeral_from_value(literal) else { continue };
        if let Value::Tag("Err", payload) = call_closure(&program.chunks[*chunk as usize].bare, vec![numeral])? {
            return Err(EvalError {
                message: format!(
                    "`{}.from_numeral` rejects the literal {}: {}",
                    nominal,
                    literal,
                    payload.first().map(|p| p.to_string()).unwrap_or_default()
                ),
            });
        }
    }
    Ok(())
}

pub fn run(program: &Rc<Program>) -> Result<Value, EvalError> {
    run_with_args(program, None)
}

/// Run, handing the entry point `args` — what a platform's host passes to `main!`.
/// `None` is an empty argument list, which is all a platformless run can offer.
pub fn run_with_args(program: &Rc<Program>, args: Option<Value>) -> Result<Value, EvalError> {
    run_scoped(program, args, |_| ()).map(|(value, _)| value)
}

/// Run, and while the program is still installed render `Str.inspect` of the result —
/// which a nominal's own `to_inspect` can steer, and that dispatch needs the running
/// program and its globals, both gone the moment the run returns.
pub fn run_and_inspect(program: &Rc<Program>, args: Option<Value>) -> Result<(Value, String), EvalError> {
    run_scoped(program, args, crate::eval::inspect)
}

/// Run with `program` installed as the running program, then — before uninstalling it,
/// so a callback or a `to_inspect` can still reach it — hand the result to `after`.
fn run_scoped<R>(
    program: &Rc<Program>,
    args: Option<Value>,
    after: impl FnOnce(&Value) -> R,
) -> Result<(Value, R), EvalError> {
    let mut vm = Vm::new(program);
    vm.entry_args = args;
    // Installed for as long as this program runs, so a builtin's callback can find the
    // machine to run a closure on.
    RUNNING.with(|r| r.borrow_mut().push((Rc::clone(program), Rc::clone(&vm.globals))));
    let result = fold_literal_coercions(program).and_then(|()| {
        vm.run_program().map(|value| {
            let extra = after(&value);
            (value, extra)
        })
    });
    RUNNING.with(|r| {
        r.borrow_mut().pop();
    });
    result
}

impl Vm {
    fn run_program(&mut self) -> Result<Value, EvalError> {
        let (top, entry) = (self.program.top, self.program.entry);
        // The builtins' own top level, binding their globals before anything reads one.
        if let Some(prelude) = self.program.prelude {
            self.call_chunk(prelude, Vec::new())?;
        }
        let value = self.call_chunk(top, Vec::new())?;
        match entry {
            None => Ok(value),
            // `main! : List(Str) => ...`, and the arity-0 spelling is also accepted.
            // The arguments are the host's to give; without one there are none.
            Some((chunk, arity)) => {
                let args = if arity == 0 {
                    Vec::new()
                } else {
                    vec![self.entry_args.take().unwrap_or_else(|| Value::list(Vec::new()))]
                };
                self.call_chunk(chunk, args)
            }
        }
    }
}

/// The machine. Registers, frames and globals.
pub struct Vm {
    program: Rc<Program>,
    /// One contiguous register file. A frame is the window `[base, base + n_regs)`.
    regs: Vec<Value>,
    frames: Vec<Frame>,
    /// Top-level values, by slot. `None` until the top level assigns it, which is how
    /// a use-before-definition becomes a message instead of a wrong answer.
    globals: Globals,
    /// What the entry point is called with, when a host supplied it. Taken on use.
    entry_args: Option<Value>,
}

impl Vm {
    pub fn new(program: &Rc<Program>) -> Self {
        Vm {
            program: Rc::clone(program),
            regs: Vec::new(),
            frames: Vec::new(),
            globals: Rc::new(RefCell::new(vec![None; program.n_globals])),
            entry_args: None,
        }
    }

    /// Call a chunk that captures nothing — the top level, or a top-level function.
    pub fn call_chunk(&mut self, chunk: ChunkId, args: Vec<Value>) -> Result<Value, EvalError> {
        let closure = self.chunk(chunk)?.bare.clone();
        self.call(&closure, args)
    }

    /// Call `closure` with `args` and run until it returns.
    pub fn call(&mut self, closure: &Rc<Closure>, args: Vec<Value>) -> Result<Value, EvalError> {
        let base = self.regs.len();
        let n_regs = self.chunk(closure.chunk)?.n_regs as usize;
        self.regs.resize(base + n_regs.max(args.len()), Value::Unit);
        for (i, arg) in args.into_iter().enumerate() {
            self.regs[base + i] = arg;
        }
        let result = self.exec(closure.clone(), base);
        // Leave the register file as it was found, whether or not the call succeeded:
        // an error propagating out of here must not leave a frame's worth of registers
        // behind for the next call to inherit.
        self.regs.truncate(base);
        self.frames.clear();
        result
    }

    fn chunk(&self, id: ChunkId) -> Result<&Chunk, EvalError> {
        self.program.chunks.get(id as usize).ok_or_else(|| EvalError {
            message: format!("vm: no chunk {}", id),
        })
    }

    /// The interpreter loop.
    ///
    /// `ip`, `base`, `code` and `cur` are locals, not fields: they change on every
    /// instruction, and reading them through `self.frames.last()` each time would be
    /// both slower and harder to read. They are written into a `Frame` only on a call.
    fn exec(&mut self, start: Rc<Closure>, start_base: usize) -> Result<Value, EvalError> {
        // Split the borrows up front: `code` comes out of `program`, and the register
        // file is mutated all the way through. Without this the two would conflict.
        let program = Rc::clone(&self.program);
        let globals = Rc::clone(&self.globals);
        let Vm { regs, frames, .. } = self;

        let mut cur = start;
        let mut chunk_id = cur.chunk;
        let mut code: &[Op] = &program.chunks[chunk_id as usize].code;
        let mut base = start_base;
        let mut ip = 0usize;

        loop {
            let op = *code.get(ip).ok_or_else(|| EvalError {
                message: format!(
                    "vm: ran off the end of `{}`",
                    program.chunks[chunk_id as usize].name
                ),
            })?;
            ip += 1;

            match op {
                Op::LoadK { dst, k } => {
                    regs[base + dst as usize] =
                        program.chunks[chunk_id as usize].consts[k as usize].clone();
                }
                Op::Move { dst, src } => {
                    regs[base + dst as usize] = regs[base + src as usize].clone();
                }
                Op::MoveTake { dst, src } => {
                    let value = std::mem::replace(&mut regs[base + src as usize], Value::Unit);
                    regs[base + dst as usize] = value;
                }
                Op::LoadGlob { dst, idx } => {
                    let value = globals.borrow()[idx as usize].clone().ok_or_else(|| EvalError {
                        message: "Used before it was defined".to_string(),
                    })?;
                    regs[base + dst as usize] = value;
                }
                Op::StoreGlob { idx, src } => {
                    globals.borrow_mut()[idx as usize] = Some(regs[base + src as usize].clone());
                }
                Op::LoadCap { dst, idx } => {
                    regs[base + dst as usize] = cur.captures[idx as usize].clone();
                }
                Op::MakeCell { dst, src } => {
                    let value = regs[base + src as usize].clone();
                    regs[base + dst as usize] = Value::Cell(Rc::new(std::cell::RefCell::new(value)));
                }
                Op::CellGet { dst, cell } => {
                    let Value::Cell(shared) = &regs[base + cell as usize] else {
                        return Err(EvalError { message: "vm: CellGet on a value that is not a cell".to_string() });
                    };
                    let value = shared.borrow().clone();
                    regs[base + dst as usize] = value;
                }
                Op::CellSet { cell, src } => {
                    let value = regs[base + src as usize].clone();
                    let Value::Cell(shared) = &regs[base + cell as usize] else {
                        return Err(EvalError { message: "vm: CellSet on a value that is not a cell".to_string() });
                    };
                    *shared.borrow_mut() = value;
                }
                Op::LoadSelf { dst } => {
                    regs[base + dst as usize] = Value::Closure(cur.clone());
                }
                Op::Bin { dst, a, b, op } => {
                    // The one implementation of Roc's operators. The operands are
                    // borrowed, not cloned: cloning two 32-byte `Value`s to add two
                    // integers was 40% of `fib`.
                    //
                    // A nominal's own operator method goes to `BinDispatch`, which the
                    // compiler emits whenever the program defines one, so nothing
                    // reaching this arm can have one.
                    let value = crate::eval::apply_binop(
                        op,
                        &regs[base + a as usize],
                        &regs[base + b as usize],
                    )
                    .map_err(|e| locate_error(&program, chunk_id, ip, e))?;
                    regs[base + dst as usize] = value;
                }
                // The right operand is a LITERAL, read straight out of the constant
                // table. The `LoadK` that used to put it in a register first was a
                // whole dispatch, a 48-byte copy and the drop glue on whatever the
                // register held — a quarter of everything `calls` executes and nearly
                // a third of `records_tail`.
                Op::BinK { dst, a, k, op } => {
                    let value = crate::eval::apply_binop(
                        op,
                        &regs[base + a as usize],
                        &program.chunks[chunk_id as usize].consts[k as usize],
                    )
                    .map_err(|e| locate_error(&program, chunk_id, ip, e))?;
                    regs[base + dst as usize] = value;
                }
                Op::BinInt { dst, a, b, op, width } => {
                    let value = bin_int(op, width, &regs[base + a as usize], &regs[base + b as usize])
                        .map_err(|e| locate_error(&program, chunk_id, ip, e))?;
                    regs[base + dst as usize] = value;
                }
                Op::BinIntK { dst, a, k, op, width } => {
                    let value = bin_int(
                        op,
                        width,
                        &regs[base + a as usize],
                        &program.chunks[chunk_id as usize].consts[k as usize],
                    )
                    .map_err(|e| locate_error(&program, chunk_id, ip, e))?;
                    regs[base + dst as usize] = value;
                }
                Op::Jump { to } => ip = to as usize,
                Op::JumpFalse { cond, to, kind } => {
                    // Strictly a `Bool`, and the tree-walker's message for
                    // anything else: roc has no truthiness and neither does this.
                    match &regs[base + cond as usize] {
                        Value::Bool(true) => {}
                        Value::Bool(false) => ip = to as usize,
                        other => return Err(locate_error(&program, chunk_id, ip, kind.expected_bool(other))),
                    }
                }
                Op::MakeClosure { dst, chunk, base: cap_base, n } => {
                    let mut captures = Vec::with_capacity(n as usize);
                    for i in 0..n as usize {
                        captures.push(regs[base + cap_base as usize + i].clone());
                    }
                    let callee = &program.chunks[chunk as usize];
                    regs[base + dst as usize] = if n == 0 {
                        // Captures nothing, so the closure made at compile time will do.
                        Value::Closure(callee.bare.clone())
                    } else {
                        Value::Closure(Rc::new(Closure {
                            chunk,
                            params: callee.params.clone(),
                            captures,
                        }))
                    };
                }
                Op::CallFn { dst, chunk, base: arg_base, argc } => {
                    let callee = &program.chunks[chunk as usize];
                    check_arity(callee.arity, argc).map_err(|e| locate_error(&program, chunk_id, ip, e))?;
                    if frames.len() >= MAX_FRAMES {
                        return Err(locate_error(&program, chunk_id, ip, too_deep(callee.name)));
                    }
                    let new_base = base + arg_base as usize;
                    grow(regs, new_base + callee.n_regs as usize);
                    frames.push(Frame {
                        chunk: chunk_id,
                        ip: ip as u32,
                        base: base as u32,
                        dst,
                        closure: cur,
                    });
                    // A top-level function captures nothing, so the closure it runs
                    // under is the one made at compile time: a refcount bump, not an
                    // allocation, on every call.
                    cur = callee.bare.clone();
                    chunk_id = chunk;
                    code = &callee.code;
                    base = new_base;
                    ip = 0;
                }
                Op::Call { dst, func, base: arg_base, argc } => {
                    // A BUILTIN held as a value and then called: `parse_with(U64.from_str)`
                    // stores it, and the parser it returns calls it. `eval::call_function`
                    // already knew how; the opcode insisted on a closure.
                    if let Value::Builtin(qualified, _) = &regs[base + func as usize] {
                        let qualified = *qualified;
                        let args = collect(regs, base + arg_base as usize, argc);
                        let value = crate::eval::call_function(
                            Value::Builtin(qualified, args.len()),
                            args,
                        )
                        .map_err(|e| locate_error(&program, chunk_id, ip, e))?;
                        regs[base + dst as usize] = value;
                        continue;
                    }
                    let callee = as_closure(&regs[base + func as usize])
                            .map_err(|e| locate_error(&program, chunk_id, ip, e))?;
                    let target = &program.chunks[callee.chunk as usize];
                    check_arity(target.arity, argc).map_err(|e| locate_error(&program, chunk_id, ip, e))?;
                    if frames.len() >= MAX_FRAMES {
                        return Err(locate_error(&program, chunk_id, ip, too_deep(target.name)));
                    }
                    let new_base = base + arg_base as usize;
                    grow(regs, new_base + target.n_regs as usize);
                    frames.push(Frame {
                        chunk: chunk_id,
                        ip: ip as u32,
                        base: base as u32,
                        dst,
                        closure: cur,
                    });
                    cur = callee;
                    chunk_id = cur.chunk;
                    code = &target.code;
                    base = new_base;
                    ip = 0;
                }
                Op::TailCall { func, chunk, base: arg_base, argc } => {
                    // A BUILTIN held as a value, called in tail position: there is no
                    // chunk to jump to, so compute it here and return as this frame's
                    // result, exactly as `Op::Ret` does.
                    if chunk.is_none() {
                        if let Value::Builtin(qualified, _) = &regs[base + func as usize] {
                            let qualified = *qualified;
                            let args = collect(regs, base + arg_base as usize, argc);
                            let value = crate::eval::call_function(
                                Value::Builtin(qualified, args.len()),
                                args,
                            )
                            .map_err(|e| locate_error(&program, chunk_id, ip, e))?;
                            match frames.pop() {
                                None => return Ok(value),
                                Some(caller) => {
                                    chunk_id = caller.chunk;
                                    code = &program.chunks[chunk_id as usize].code;
                                    base = caller.base as usize;
                                    ip = caller.ip as usize;
                                    cur = caller.closure;
                                    regs[base + caller.dst as usize] = value;
                                    continue;
                                }
                            }
                        }
                    }
                    // No frame is pushed and none is popped: the arguments move down
                    // to where this frame's own parameters are, and execution starts
                    // again at the top of the callee. A tail-recursive function is
                    // therefore a loop, in constant memory.
                    let callee = match chunk {
                        Some(id) => program.chunks[id as usize].bare.clone(),
                        None => as_closure(&regs[base + func as usize])
                            .map_err(|e| locate_error(&program, chunk_id, ip, e))?,
                    };
                    let target = &program.chunks[callee.chunk as usize];
                    check_arity(target.arity, argc).map_err(|e| locate_error(&program, chunk_id, ip, e))?;
                    if arg_base != 0 {
                        // Increasing order, and every destination is below its source,
                        // so nothing is overwritten before it has been read.
                        for i in 0..argc as usize {
                            let arg = std::mem::replace(
                                &mut regs[base + arg_base as usize + i],
                                Value::Unit,
                            );
                            regs[base + i] = arg;
                        }
                    }
                    grow(regs, base + target.n_regs as usize);
                    cur = callee;
                    chunk_id = cur.chunk;
                    code = &target.code;
                    ip = 0;
                }
                // ---- aggregates ----
                Op::MakeList { dst, base: b, n } => {
                    let items = collect(regs, base + b as usize, n);
                    regs[base + dst as usize] = Value::list(items);
                }
                Op::MakeTuple { dst, base: b, n } => {
                    let items = collect(regs, base + b as usize, n);
                    regs[base + dst as usize] = Value::tuple(items);
                }
                Op::ListPush { list, src } => {
                    let item = std::mem::replace(&mut regs[base + src as usize], Value::Unit);
                    match &mut regs[base + list as usize] {
                        Value::List(items) => Rc::make_mut(items).push(item),
                        other => {
                            return Err(locate_error(&program, chunk_id, ip, EvalError {
                                message: format!("vm: pushed onto {}, which is not a list", other),
                            }))
                        }
                    }
                }
                Op::MakeTag { dst, name, base: b, n } => {
                    let tag = program.chunks[chunk_id as usize].names[name as usize];
                    // Straight into the `Rc` from an exact-size iterator, which
                    // allocates ONCE; going through a `Vec` allocated twice. A tag
                    // with no payload shares one and allocates nothing.
                    let value = if n == 0 {
                        Value::bare(tag)
                    } else {
                        Value::Tag(
                            tag,
                            (0..n as usize)
                                .map(|i| {
                                    std::mem::replace(
                                        &mut regs[base + b as usize + i],
                                        Value::Unit,
                                    )
                                })
                                .collect(),
                        )
                    };
                    regs[base + dst as usize] = value;
                }
                Op::MakeRecord { dst, name, base: b, n } => {
                    let names = &program.chunks[chunk_id as usize].names;
                    let mut fields = Vec::with_capacity(n as usize);
                    for i in 0..n as usize {
                        // TAKE the field out of its register, as `MakeTuple` and
                        // `MakeTag` beside it do. The compiler resets `next_reg` to
                        // `base` before allocating the destination, so these registers
                        // are dead the moment this op runs; cloning them was copying a
                        // nested record or bumping a refcount for nothing.
                        fields.push((
                            names[name as usize + i],
                            std::mem::replace(&mut regs[base + b as usize + i], Value::Unit),
                        ));
                    }
                    regs[base + dst as usize] = Value::record(fields);
                }
                Op::UpdateRecord { dst, obj, name, base: b, n, take } => {
                    // `take` is `vm::liveness` saying nothing reads `obj` after this,
                    // so the record moves out of its register and `Rc::make_mut` below
                    // mutates it IN PLACE — no allocation, which is what roc's own
                    // uniqueness buys and what `{ ..p, x: 1 }` over a `var` needs.
                    let mut record = if take {
                        std::mem::replace(&mut regs[base + obj as usize], Value::Unit)
                    } else {
                        regs[base + obj as usize].clone()
                    };
                    let Value::Record(shared) = &mut record else {
                        return Err(locate_error(&program, chunk_id, ip, EvalError {
                            message: format!("Cannot update `{}`: it is not a record", record),
                        }))
                    };
                    let fields = Rc::make_mut(shared);
                    let names = &program.chunks[chunk_id as usize].names;
                    for i in 0..n as usize {
                        let field = names[name as usize + i];
                        match fields.iter_mut().find(|(f, _)| *f == field) {
                            Some(slot) => slot.1 = regs[base + b as usize + i].clone(),
                            None => {
                                return Err(locate_error(&program, chunk_id, ip, EvalError {
                                    message: format!("Record has no field `{}` to update", field),
                                }))
                            }
                        }
                    }
                    regs[base + dst as usize] = record;
                }
                Op::GetField { dst, obj, name } => {
                    let field = program.chunks[chunk_id as usize].names[name as usize];
                    let value = match &regs[base + obj as usize] {
                        // A field the record does not have is an optional one that was
                        // left out — the checker refuses a missing required field.
                        Value::Record(fields) => fields
                            .iter()
                            .find(|(f, _)| *f == field)
                            .map(|(_, v)| v.clone())
                            .unwrap_or(Value::Missing),
                        Value::Unit => Value::Missing,
                        // `Builtin.roc`'s `Set.from_iter`/`Dict.from_iter` READ
                        // `iterator.len_if_known` off roc's `Iter` record. rocflight's
                        // iterator is the list, range or lazy value it walks, so the
                        // field is answered from its size hint rather than stored.
                        other if field == "len_if_known" && crate::eval::module_for(other) == Some("List") => {
                            crate::eval::size_hint_of(other)
                                .map_err(|e| locate_error(&program, chunk_id, ip, e))?
                        }
                        other => {
                            return Err(locate_error(&program, chunk_id, ip, EvalError {
                                message: format!("Cannot access field '{}' on {}", field, other),
                            }))
                        }
                    };
                    regs[base + dst as usize] = value;
                }
                Op::GetOptField { dst, obj, name } => {
                    let field = program.chunks[chunk_id as usize].names[name as usize];
                    let value = match &regs[base + obj as usize] {
                        Value::Record(fields) => match fields.iter().find(|(f, _)| *f == field) {
                            Some((_, Value::Missing)) | None => {
                                // Absent, which is the point of an optional field.
                                Value::tag("Err", [Value::bare("MissingField")])
                            }
                            Some((_, v)) => Value::tag("Ok", [v.clone()]),
                        },
                        // `{}` is the empty record: every optional field is absent.
                        Value::Unit => Value::tag("Err", [Value::bare("MissingField")]),
                        other => {
                            return Err(locate_error(&program, chunk_id, ip, EvalError {
                                message: format!(
                                    "Cannot read optional field `{}` on {}",
                                    field, other
                                ),
                            }))
                        }
                    };
                    regs[base + dst as usize] = value;
                }
                Op::GetIndex { dst, obj, i } => {
                    let value = match &regs[base + obj as usize] {
                        Value::Tuple(items) => {
                            items.get(i as usize).cloned().ok_or_else(|| EvalError {
                                message: format!(
                                    "Tuple has {} element(s), so .{} is out of range",
                                    items.len(),
                                    i
                                ),
                            })?
                        }
                        other => {
                            return Err(locate_error(&program, chunk_id, ip, EvalError {
                                message: format!("Cannot index .{} on {}", i, other),
                            }))
                        }
                    };
                    regs[base + dst as usize] = value;
                }

                // ---- pattern tests ----
                Op::TestLit { obj, pat, to } => {
                    let pattern = &program.chunks[chunk_id as usize].pats[pat as usize];
                    if !crate::eval::literal_pattern_matches(pattern, &regs[base + obj as usize]) {
                        ip = to as usize;
                    }
                }
                Op::TestLitDyn { obj, pat, to } => {
                    let pattern = &program.chunks[chunk_id as usize].pats[pat as usize];
                    let value = &regs[base + obj as usize];
                    let matched = match value {
                        Value::Record(_) | Value::Tag(..) | Value::Tuple(_) => {
                            nominal_literal_matches(&program, pattern, value)
                                .map_err(|e| locate_error(&program, chunk_id, ip, e))?
                        }
                        _ => crate::eval::literal_pattern_matches(pattern, value),
                    };
                    if !matched {
                        ip = to as usize;
                    }
                }
                Op::TestStr { obj, pat, base: captures, to } => {
                    let pattern = &program.chunks[chunk_id as usize].pats[pat as usize];
                    let found = match (pattern, &regs[base + obj as usize]) {
                        (crate::ast::Pattern::StrInterp { prefix, segments }, Value::Str(text)) => {
                            crate::eval::interp_captures(prefix, segments, text)
                        }
                        _ => None,
                    };
                    match found {
                        Some(values) => {
                            for (i, value) in values.into_iter().enumerate() {
                                regs[base + captures as usize + i] = value;
                            }
                        }
                        None => ip = to as usize,
                    }
                }
                Op::TestTag { obj, name, n, to } => {
                    let want = program.chunks[chunk_id as usize].names[name as usize];
                    match &regs[base + obj as usize] {
                        Value::Tag(tag, payload)
                            if *tag == want && payload.len() == n as usize => {}
                        // `True` and `False` are the booleans spelled as tags.
                        Value::Bool(b) if n == 0 && want == if *b { "True" } else { "False" } => {}
                        // An optional field read as a `Try`: a present value IS `Ok(value)`,
                        // and a missing slot IS `Err(MissingField)`.
                        Value::Missing if want == "Err" && n == 1 => {}
                        other if want == "Ok" && n == 1 && !matches!(other, Value::Tag(..) | Value::Missing) => {}
                        _ => ip = to as usize,
                    }
                }
                Op::TestTuple { obj, n, to } => match &regs[base + obj as usize] {
                    Value::Tuple(items) if items.len() == n as usize => {}
                    _ => ip = to as usize,
                },
                Op::TestRecord { obj, to } => {
                    // `{}` the value is `Unit`, and the pattern `{}` names no fields,
                    // so it matches; a pattern with fields then fails on the read.
                    if !matches!(regs[base + obj as usize], Value::Record(_) | Value::Unit) {
                        ip = to as usize;
                    }
                }
                Op::TestList { obj, n, exact, to } => match &regs[base + obj as usize] {
                    Value::List(items)
                        if (exact && items.len() == n as usize)
                            || (!exact && items.len() >= n as usize) => {}
                    _ => ip = to as usize,
                },
                Op::TestBool { cond, want, to } => {
                    if !matches!(&regs[base + cond as usize], Value::Bool(b) if *b == want) {
                        ip = to as usize;
                    }
                }
                Op::NoMatch { obj } => {
                    return Err(locate_error(&program, chunk_id, ip, EvalError {
                        message: format!("No match arm matched {}", regs[base + obj as usize]),
                    }))
                }

                // ---- destructuring ----
                Op::GetPayload { dst, obj, i } => {
                    let value = match &regs[base + obj as usize] {
                        Value::Tag(_, payload) => payload[i as usize].clone(),
                        // See `TestTag`: an optional field's `Ok` payload is the value,
                        // and a missing one's `Err` payload is `MissingField`.
                        Value::Missing => Value::bare("MissingField"),
                        other => other.clone(),
                    };
                    regs[base + dst as usize] = value;
                }
                Op::GetFieldOr { dst, obj, name, to } => {
                    let field = program.chunks[chunk_id as usize].names[name as usize];
                    let found = match &regs[base + obj as usize] {
                        Value::Record(fields) => {
                            fields.iter().find(|(f, _)| *f == field).map(|(_, v)| v.clone())
                        }
                        other => unreachable_shape("a record", other)?,
                    };
                    match found {
                        Some(value) => regs[base + dst as usize] = value,
                        // A record pattern naming a field the value does not have is
                        // simply not a match, not an error.
                        None => ip = to as usize,
                    }
                }
                Op::GetRest { dst, obj, name, n } => {
                    let names = &program.chunks[chunk_id as usize].names;
                    let named = &names[name as usize..name as usize + n as usize];
                    let value = match &regs[base + obj as usize] {
                        Value::Record(fields) => Value::record(
                            fields
                                .iter()
                                .filter(|(f, _)| !named.contains(f))
                                .cloned()
                                .collect(),
                        ),
                        other => unreachable_shape("a record", other)?,
                    };
                    regs[base + dst as usize] = value;
                }
                Op::GetElem { dst, obj, i, from_end } => {
                    let value = match &regs[base + obj as usize] {
                        Value::List(items) => {
                            let at = if from_end { items.len() - 1 - i as usize } else { i as usize };
                            items[at].clone()
                        }
                        other => unreachable_shape("a list", other)?,
                    };
                    regs[base + dst as usize] = value;
                }
                Op::GetSlice { dst, obj, front, back } => {
                    let value = match &regs[base + obj as usize] {
                        Value::List(items) => {
                            Value::list(items[front as usize..items.len() - back as usize].to_vec())
                        }
                        other => unreachable_shape("a list", other)?,
                    };
                    regs[base + dst as usize] = value;
                }

                // ---- loops ----
                Op::MakeRange { dst, start, end, inclusive } => {
                    let (lo, hi) = (&regs[base + start as usize], &regs[base + end as usize]);
                    // Two integers stay the fast `Range`; anything else (a `Dec`, a
                    // float) becomes a lazy iterator walked element by element.
                    regs[base + dst as usize] = match (lo, hi) {
                        (Value::Int(a), Value::Int(b)) => Value::Range { start: *a, end: *b, inclusive, step: 1 },
                        (a, b) => {
                            let step = match a {
                                Value::Dec(_) => Value::Dec(crate::eval::DEC_SCALE),
                                Value::Float(_) => Value::Float(1.0),
                                Value::F32(_) => Value::F32(1.0),
                                _ => Value::Int(1),
                            };
                            Value::Iter(std::rc::Rc::new(crate::eval::lazy::Lazy::Range {
                                at: a.clone(),
                                end: b.clone(),
                                step,
                                inclusive,
                            }))
                        }
                    };
                }
                Op::IterNext { dst, iter, idx, to } => {
                    ip = iter_step(&program, regs, base, dst, iter, idx, to as usize, ip)
                        .map_err(|e| locate_error(&program, chunk_id, ip - 1, e))?;
                }
                // The same step as a back edge: an element jumps to the BODY and
                // exhaustion falls through, which is the other way round.
                Op::IterNextBack { dst, iter, idx, to } => {
                    ip = iter_step(&program, regs, base, dst, iter, idx, ip, to as usize)
                        .map_err(|e| locate_error(&program, chunk_id, ip - 1, e))?;
                }

                // ---- builtins, dispatch, interpolation ----
                Op::CallBuiltin { dst, name, base: b, argc } => {
                    let names = &program.chunks[chunk_id as usize].names;
                    let (module, func) = (names[name as usize], names[name as usize + 1]);
                    // The register window IS the argument list. Collecting it into a
                    // `Vec` first was a `malloc` per builtin call and bought nothing:
                    // the arguments are already contiguous, they are already dead after
                    // the call, and `collect` took them out with the same
                    // `mem::replace` the builtins do themselves.
                    let from = base + b as usize;
                    let value = crate::eval::call_builtin_values(
                        module,
                        func,
                        &mut regs[from..from + argc as usize],
                    )
                    .map_err(|e| locate_error(&program, chunk_id, ip, e))?;
                    regs[base + dst as usize] = value;
                }
                Op::CallHost { dst, name, base: b, argc } => {
                    let effect = program.chunks[chunk_id as usize].names[name as usize];
                    let args = collect(regs, base + b as usize, argc);
                    regs[base + dst as usize] = crate::eval::host_effect(effect, args).map_err(|e| locate_error(&program, chunk_id, ip, e))?;
                }
                Op::MakeBuiltin { dst, name } => {
                    // The qualified name is built once, at compile time.
                    let qualified = program.chunks[chunk_id as usize].names[name as usize];
                    // Arity 1: every builtin used as a value takes its subject and
                    // nothing else.
                    regs[base + dst as usize] = Value::Builtin(qualified, 1);
                }
                Op::DispatchMethod { dst, name, base: b, argc } => {
                    let method = program.chunks[chunk_id as usize].names[name as usize];
                    // A method the PROGRAM defines for this value's own module wins over
                    // the interpreter's own: once `Builtin.roc` is loaded, `List.map` is
                    // Roc, and the Rust one is only there for what Roc has not defined.
                    let target = match crate::eval::module_for(&regs[base + b as usize]) {
                        // The value names its module, so the answer is that module's
                        // method or nothing. Falling through to another type's method of
                        // the same name is what made a loaded `Stream` answer a List.
                        Some(module) => program.methods.get(&(module, method)).copied(),
                        // No module: a record or a tag, which is all a nominal is at run
                        // time. If exactly one type defines the method, that is the one
                        // meant; if several do, only the checker could have known.
                        None => match program.methods_by_name.get(method) {
                            // ... unless the value's SHAPE rules that one out. A record
                            // matches the pattern `Ok(_)` at run time, so a lone
                            // `Try.is_eq` answered `==` on a record by calling ITSELF on
                            // the same record a million frames deep before failing —
                            // 130ms per comparison, thrown away for the structural answer.
                            Some(defined) if defined.len() == 1 => {
                                admits_receiver(&program, defined[0].0, &regs[base + b as usize])
                                    .then_some(defined[0].1)
                            }
                            // Several types define it. The value's SHAPE can still say
                            // which nominal it is — `Key.{ value: n }` is a record of
                            // exactly `Key`'s fields — so the candidates whose nominal
                            // fits are the ones meant. Two nominals of one shape (`Set`
                            // over `Dict`) are told apart by depth: the wrapper is the
                            // more specific.
                            Some(defined)
                                if defined.iter().any(|(owner, _)| {
                                    let module = owner.split('.').next().unwrap_or(owner);
                                    program.nominal_shapes.get(module).is_some_and(|shape| shape.is_exactly(&regs[base + b as usize]))
                                }) =>
                            {
                                let depth_of = |owner: &str| {
                                    let module = owner.split('.').next().unwrap_or(owner);
                                    program.nominal_depth.get(module).copied().unwrap_or(0)
                                };
                                let fitting: Vec<&(&'static str, ChunkId)> = defined
                                    .iter()
                                    .filter(|(owner, _)| {
                                        let module = owner.split('.').next().unwrap_or(owner);
                                        program.nominal_shapes.get(module).is_some_and(|shape| shape.is_exactly(&regs[base + b as usize]))
                                    })
                                    .collect();
                                let best = fitting.iter().map(|(owner, _)| depth_of(owner)).max().unwrap_or(0);
                                let deepest: Vec<&&(&'static str, ChunkId)> =
                                    fitting.iter().filter(|(owner, _)| depth_of(owner) == best).collect();
                                // Two nominals of one field list at one depth: the
                                // fields' KINDS may still tell them apart (`{ value :
                                // F32 }` from `{ value : F64 }`). If not, only the
                                // checker could have known, so name them both.
                                let held: Vec<&&(&'static str, ChunkId)> = deepest
                                    .iter()
                                    .copied()
                                    .filter(|(owner, _)| {
                                        let module = owner.split('.').next().unwrap_or(owner);
                                        program.nominal_shapes.get(module).is_some_and(|shape| shape.holds_kinds(&regs[base + b as usize]))
                                    })
                                    .collect();
                                let mut winners = if held.len() == 1 { held.into_iter() } else { deepest.into_iter() };
                                match (winners.next(), winners.next()) {
                                    (Some((_, chunk)), None) => Some(*chunk),
                                    _ => {
                                        let names: Vec<&str> = fitting.iter().map(|(name, _)| *name).collect();
                                        return Err(locate_error(&program, chunk_id, ip, EvalError {
                                            message: format!(
                                                "`{}` is ambiguous: {} all define it. Call it explicitly.",
                                                method,
                                                names.join(", ")
                                            ),
                                        }));
                                    }
                                }
                            }
                            // Several types define it but the value is none of them —
                            // `Dict.to_hash`/`Set.to_hash` against a record, tuple or
                            // tag KEY, which `Dict` hashes structurally. A method the
                            // interpreter provides for any value (`to_hash`) falls back
                            // to that builtin rather than being called ambiguous.
                            Some(_) if crate::eval::has_structural_builtin(method) => None,
                            Some(defined) => {
                                let names: Vec<&str> =
                                    defined.iter().map(|(name, _)| *name).collect();
                                return Err(locate_error(&program, chunk_id, ip, EvalError {
                                    message: format!(
                                        "`{}` is ambiguous: {} all define it. Call it explicitly.",
                                        method,
                                        names.join(", ")
                                    ),
                                }));
                            }
                            None => None,
                        },
                    };
                    match target {
                        Some(chunk) => {
                            let callee = &program.chunks[chunk as usize];
                            check_arity(callee.arity, argc)
                                .map_err(|e| locate_error(&program, chunk_id, ip, e))?;
                            if frames.len() >= MAX_FRAMES {
                                return Err(locate_error(&program, chunk_id, ip, too_deep(callee.name)));
                            }
                            let new_base = base + b as usize;
                            grow(regs, new_base + callee.n_regs as usize);
                            frames.push(Frame {
                                chunk: chunk_id,
                                ip: ip as u32,
                                base: base as u32,
                                dst,
                                closure: cur,
                            });
                            cur = callee.bare.clone();
                            chunk_id = chunk;
                            code = &callee.code;
                            base = new_base;
                            ip = 0;
                        }
                        None => {
                            // Receiver at slot 0, arguments after it: the register
                            // window is already the argument list every builtin wants.
                            let from = base + b as usize;
                            let value = crate::eval::dispatch_builtin(
                                method,
                                &mut regs[from..from + argc as usize],
                            )
                            .map_err(|e| locate_error(&program, chunk_id, ip, e))?;
                            regs[base + dst as usize] = value;
                        }
                    }
                }
                Op::Interp { dst, name, base: b, n } => {
                    let names = &program.chunks[chunk_id as usize].names;
                    let mut out = String::new();
                    for i in 0..n as usize {
                        out.push_str(names[name as usize + i]);
                        out.push_str(&crate::eval::interpolated(&regs[base + b as usize + i]));
                    }
                    out.push_str(names[name as usize + n as usize]);
                    regs[base + dst as usize] = Value::Str(Rc::from(out));
                }
                Op::BinDispatch { dst, a, b, op } => {
                    let left = regs[base + a as usize].clone();
                    let right = regs[base + b as usize].clone();
                    let value = match crate::eval::dispatch_operator(op, &left, &right)
                        .map_err(|e| locate_error(&program, chunk_id, ip, e))?
                    {
                        Some(from_method) => from_method,
                        None => crate::eval::apply_binop(op, &left, &right)
                            .map_err(|e| locate_error(&program, chunk_id, ip, e))?,
                    };
                    regs[base + dst as usize] = value;
                }

                // ---- statements ----
                Op::Expect { cond } => {
                    crate::eval::run_expect(&regs[base + cond as usize]).map_err(|e| locate_error(&program, chunk_id, ip, e))?;
                }
                Op::TestExpect { cond } => {
                    crate::eval::run_test_expect(&regs[base + cond as usize]).map_err(|e| locate_error(&program, chunk_id, ip, e))?;
                }
                Op::Dbg { src, shape } => crate::eval::run_dbg(&regs[base + src as usize], &regs[base + shape as usize]),
                Op::Crash { src } => {
                    return Err(locate_error(
                        &program,
                        chunk_id,
                        ip,
                        crate::eval::crash_error(&regs[base + src as usize]),
                    ))
                }

                Op::Ret { src } => {
                    // Take the value rather than cloning it: this register is dead.
                    let value = std::mem::replace(&mut regs[base + src as usize], Value::Unit);
                    match frames.pop() {
                        None => return Ok(value),
                        Some(caller) => {
                            chunk_id = caller.chunk;
                            code = &program.chunks[chunk_id as usize].code;
                            base = caller.base as usize;
                            ip = caller.ip as usize;
                            cur = caller.closure;
                            regs[base + caller.dst as usize] = value;
                        }
                    }
                }
            }
        }
    }
}

/// Take `n` values out of consecutive registers, leaving `Unit` behind.
///
/// A move rather than a clone: these registers are the aggregate's own arguments and
/// are dead the instant it is built.
/// Integer arithmetic the checker proved is integer arithmetic.
///
/// Shared by `BinInt` and `BinIntK`, which differ only in where the right operand
/// comes from — a register or the constant table. `inline(always)` for the reason
/// `iter_step` has it: left out of line this is a call on the hottest opcode there is.
/// Errors come back UNLOCATED; the call site attaches the instruction.
#[inline(always)]
fn bin_int(
    op: crate::ast::BinOp,
    width: u8,
    lhs: &Value,
    rhs: &Value,
) -> Result<Value, EvalError> {
    // `I128` cannot be range-checked after the fact — its arithmetic wraps at the same
    // width the value lives in — so its overflow is caught with checked i128 math here,
    // before `int_binop` wraps it.
    if width == crate::eval::I128_WIDTH {
        if let (Value::Int(x), Value::Int(y)) = (lhs, rhs) {
            let checked = match op {
                crate::ast::BinOp::Add => x.checked_add(*y),
                crate::ast::BinOp::Sub => x.checked_sub(*y),
                crate::ast::BinOp::Mul => x.checked_mul(*y),
                crate::ast::BinOp::IntDiv | crate::ast::BinOp::Div => x.checked_div(*y),
                _ => Some(0),
            };
            match checked {
                Some(_)
                    if !matches!(
                        op,
                        crate::ast::BinOp::Add
                            | crate::ast::BinOp::Sub
                            | crate::ast::BinOp::Mul
                            | crate::ast::BinOp::IntDiv
                            | crate::ast::BinOp::Div
                    ) => {}
                Some(n) => return Ok(Value::Int(n)),
                None => {
                    return Err(EvalError {
                        message: "crash: integer overflow: I128 arithmetic overflowed".to_string(),
                    })
                }
            }
        }
    }
    match (lhs, rhs) {
        (Value::Int(x), Value::Int(y)) => match crate::eval::int_binop(op, *x, *y) {
            Some(Ok(Value::Int(n))) if width != 0 && !crate::eval::fits_width(n, width) => {
                Err(EvalError { message: format!("crash: integer overflow: {} does not fit", n) })
            }
            Some(result) => result,
            // `and`/`or` are the only ops `int_binop` declines, and the compiler never
            // specialises those.
            None => crate::eval::apply_binop(op, lhs, rhs),
        },
        (left, right) => crate::eval::apply_binop(op, left, right),
    }
}

/// One step of a `for` loop, shared by `IterNext` and `IterNextBack`.
///
/// They differ only in which way round the two answers go: `on_item` is where to
/// continue when the iterator yielded something and `on_done` where to go when it did
/// not. `IterNext` falls through with an element and jumps out when exhausted;
/// `IterNextBack` — the fused back edge — jumps back into the body with an element and
/// falls through when exhausted. One implementation, so there is one place to be right.
///
/// Errors come back UNLOCATED; the call site attaches the instruction.
///
/// `inline(always)`, and it is load-bearing: left to LLVM this stayed out of line and
/// `iter_range` — which is the lazy branch below, two million times — cost 10.8% more.
/// The same trap the `Lazy::advance` split hit, for the same reason.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn iter_step(
    program: &Rc<Program>,
    regs: &mut [Value],
    base: usize,
    dst: Reg,
    iter: Reg,
    idx: Reg,
    on_done: usize,
    on_item: usize,
) -> Result<usize, EvalError> {
    let at = match &regs[base + idx as usize] {
        Value::Int(n) => *n,
        other => {
            return Err(EvalError { message: format!("vm: loop counter held {}", other) })
        }
    };
    // A nominal iterable — a record or tag with an `iter` method — is turned into its
    // iterator once, in place, then looped. First, because what `iter` gives may be
    // the lazy iterator below.
    if matches!(&regs[base + iter as usize], Value::Record(_) | Value::Tag(..)) {
        if let Some(iterated) = call_iter_method(program, &regs[base + iter as usize])? {
            regs[base + iter as usize] = iterated;
        }
    }
    // A lazy iterator carries its own state, not an index: step it, skipping past
    // `Skip`s, and write the rest back for next time.
    //
    // The iterator is TAKEN out of its register rather than cloned, so that
    // `Lazy::step` is its only owner and can advance it in place — otherwise a loop
    // over a non-integer range allocates a rest per element. The register is rewritten
    // on both paths below, so it never stays `Unit`.
    if matches!(&regs[base + iter as usize], Value::Iter(_)) {
        let taken = std::mem::replace(&mut regs[base + iter as usize], Value::Unit);
        let Value::Iter(mut current) = taken else {
            return Err(EvalError { message: "vm: iterator vanished".to_string() });
        };
        let stepped = loop {
            match current.step()? {
                crate::eval::lazy::Step::Done => break None,
                crate::eval::lazy::Step::Skip(rest) => current = rest,
                crate::eval::lazy::Step::One(item, rest) => break Some((item, rest)),
            }
        };
        return Ok(match stepped {
            None => {
                // `Done` consumed the iterator, and an iterator that is done is an
                // empty one.
                regs[base + iter as usize] = Value::Iter(crate::eval::lazy::exhausted());
                on_done
            }
            Some((item, rest)) => {
                regs[base + dst as usize] = item;
                regs[base + iter as usize] = Value::Iter(rest);
                on_item
            }
        });
    }
    let next = match &regs[base + iter as usize] {
        Value::Range { start, end, inclusive, step } => {
            let last = if *inclusive { *end } else { *end - 1 };
            let current = start + at * i128::from(*step);
            (*step > 0 && current <= last).then_some(Value::Int(current))
        }
        Value::List(items) => items.get(at as usize).cloned(),
        other => {
            return Err(EvalError {
                message: format!("`for` needs a List or a range to iterate, got {}", other),
            })
        }
    };
    Ok(match next {
        None => on_done,
        Some(value) => {
            regs[base + dst as usize] = value;
            regs[base + idx as usize] = Value::Int(at + 1);
            on_item
        }
    })
}

fn collect(regs: &mut [Value], from: usize, n: u16) -> Vec<Value> {
    (0..n as usize)
        .map(|i| std::mem::replace(&mut regs[from + i], Value::Unit))
        .collect()
}

/// A destructuring op ran on a value whose shape a test should already have rejected.
///
/// Reachable only through a compiler bug, so it reports one rather than panicking: a
/// wrong opcode in safe Rust is a message, which is the whole argument for this being
/// safe Rust.
fn unreachable_shape<T>(expected: &str, got: &Value) -> Result<T, EvalError> {
    Err(EvalError {
        message: format!("vm: destructured {} as {}", got, expected),
    })
}

/// Say where a failing instruction was, if its node knows.
///
/// The message gains " at file:line:column". A node with no registered source — a
/// string parsed straight into the parser, as the tests do — adds nothing, so an error
/// from one reads exactly as it did before locations existed.
fn locate_error(program: &Program, chunk: ChunkId, ip: usize, mut error: EvalError) -> EvalError {
    // `ip` has already advanced past the instruction that failed.
    let Some(node) = program
        .chunks
        .get(chunk as usize)
        .and_then(|c| c.spans.get(ip.saturating_sub(1)))
    else {
        return error;
    };
    if let Some(at) = crate::ast::locate(*node) {
        error.message = format!("{} at {}", error.message, at);
    }
    error
}

/// Make room for a callee's frame. The register file only ever grows to the deepest
/// call the program actually makes.
fn grow(regs: &mut Vec<Value>, need: usize) {
    if regs.len() < need {
        regs.resize(need, Value::Unit);
    }
}

/// The tree-walker's wording, kept so the message did not change when it went.
fn check_arity(expected: u16, got: u16) -> Result<(), EvalError> {
    if expected == got {
        return Ok(());
    }
    Err(EvalError {
        message: format!("Lambda expects {} argument(s), got {}", expected, got),
    })
}

fn too_deep(name: &str) -> EvalError {
    EvalError {
        message: format!("Recursion went deeper than {} calls in `{}`", MAX_FRAMES, name),
    }
}

/// A value being called has to be a function; the tree-walker's message, kept.
fn as_closure(value: &Value) -> Result<Rc<Closure>, EvalError> {
    match value {
        Value::Closure(c) => Ok(c.clone()),
        other => Err(EvalError {
            message: format!("Attempted to call a non-function value: {}", other),
        }),
    }
}
