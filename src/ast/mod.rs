//! Abstract Syntax Tree definitions for Roc
//!
//! Phase 1: String literals
//! Phase 2: Numbers, identifiers
//! Phase 3: Lambdas, calls, let bindings
//! Phase 4: Builtins, lambdas, qualified names

use std::fmt;

/// Binary operators
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    // Arithmetic
    Add,
    Sub,
    Mul,
    Div,
    /// Truncating integer division: `//`
    IntDiv,
    /// Remainder: `%`
    Rem,
    // Comparison
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    // Logical
    And,
    Or,
}

impl fmt::Display for BinOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BinOp::Add => write!(f, "+"),
            BinOp::Sub => write!(f, "-"),
            BinOp::Mul => write!(f, "*"),
            BinOp::Div => write!(f, "/"),
            BinOp::IntDiv => write!(f, "//"),
            BinOp::Rem => write!(f, "%"),
            BinOp::Eq => write!(f, "=="),
            BinOp::Ne => write!(f, "!="),
            BinOp::Lt => write!(f, "<"),
            BinOp::Le => write!(f, "<="),
            BinOp::Gt => write!(f, ">"),
            BinOp::Ge => write!(f, ">="),
            BinOp::And => write!(f, "&&"),
            BinOp::Or => write!(f, "||"),
        }
    }
}

use std::cell::RefCell;

/// Which node this is, for asking questions about it later.
///
/// The AST itself carries no source positions and no types: both are side tables the
/// parser and the checker fill in, indexed by this. That keeps `Expr` the shape it was
/// — a `match` on it still reads as the grammar — while making a node something other
/// passes can *refer to*.
///
/// Two things were blocked on having this at all:
///
/// * **Error locations.** A runtime error could say what went wrong but not where, so
///   the VM's per-instruction span table had nothing to point at.
/// * **Specialised arithmetic.** `AddInt` can only be emitted where both operands are
///   known to be `Int`, and the checker knew that but had nowhere to write it down.
///
/// Dense and assigned in parse order, so a side table is a `Vec` rather than a map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

impl NodeId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Where a node came from, once a source has been registered for it.
#[derive(Debug, Clone)]
pub struct Location {
    pub file: String,
    pub line: u32,
    pub column: u32,
}

impl std::fmt::Display for Location {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}:{}", self.file, self.line, self.column)
    }
}

/// One parse's worth of nodes, and the text they came from.
struct Source {
    /// The first id this parse handed out, and the last. A nested parse — a string
    /// interpolation makes its own parser mid-expression — sits INSIDE its parent's
    /// range, so resolving a node picks the narrowest range containing it.
    first: u32,
    last: u32,
    file: String,
    text: String,
}

struct NodeTable {
    /// Byte offset into the owning source, by node id. `u32::MAX` for a node built
    /// somewhere that did not know its position.
    offsets: Vec<u32>,
    sources: Vec<Source>,
}

thread_local! {
    /// Node offsets and the sources they belong to.
    ///
    /// Global rather than a field on `Parser` because not every node is built by a
    /// parser method: a few literals come from free functions that take a `&str` and
    /// have no access to parser state. Threading a counter through them would have
    /// meant changing their signatures for nothing.
    static NODES: RefCell<NodeTable> =
        RefCell::new(NodeTable { offsets: Vec::new(), sources: Vec::new() });
}

/// An offset that means "this node does not know where it came from".
const UNKNOWN: u32 = u32::MAX;

/// Hand out the next node id, remembering where in the source it was.
pub fn fresh_node(offset: usize) -> NodeId {
    NODES.with(|n| {
        let mut n = n.borrow_mut();
        let id = n.offsets.len() as u32;
        n.offsets.push(u32::try_from(offset).unwrap_or(UNKNOWN));
        NodeId(id)
    })
}

/// Hand out the next node id for a node whose position is not known.
pub fn fresh_node_unlocated() -> NodeId {
    fresh_node(UNKNOWN as usize)
}

/// A node with the same position as an existing one.
///
/// What a composite node uses: `a + b` starts where `a` starts, and `f(x)` where `f`
/// does, so inheriting the first child's offset gives the construct's real start
/// without every parse function having to remember where it began.
pub fn fresh_node_like(other: &Expr) -> NodeId {
    let offset = NODES.with(|n| {
        n.borrow().offsets.get(other.id().index()).copied().unwrap_or(UNKNOWN)
    });
    fresh_node(offset as usize)
}

/// Install a run of nodes whose offsets are already known, answering the id of the
/// first. What `artifact::Artifact::member` uses: the ids inside a pre-parsed tree are
/// relative to its own first node, so loading pushes the offsets and adds this base.
pub fn push_nodes(offsets: &[u32]) -> u32 {
    NODES.with(|n| {
        let mut n = n.borrow_mut();
        let base = n.offsets.len() as u32;
        n.offsets.extend_from_slice(offsets);
        base
    })
}

/// The offsets of the nodes in `first..last`, for writing an artifact.
pub fn offsets_between(first: u32, last: u32) -> Vec<u32> {
    NODES.with(|n| n.borrow().offsets[first as usize..last as usize].to_vec())
}

/// How many nodes exist so far: a watermark, so nodes made after a point — another
/// file's — can be told from the ones before it.
pub fn node_count() -> usize {
    NODES.with(|n| n.borrow().offsets.len())
}

/// Where a node started in its source, if that was recorded.
pub fn offset_of(id: NodeId) -> Option<usize> {
    NODES.with(|n| {
        let offset = *n.borrow().offsets.get(id.index())?;
        (offset != UNKNOWN).then_some(offset as usize)
    })
}

/// Move a node to a different offset.
///
/// For a node built somewhere that did not know where it was: the free functions that
/// parse a literal out of a `&str` have no parser state, so the caller — which does
/// know where the literal started — says so afterwards.
pub fn relocate(id: NodeId, offset: usize) {
    NODES.with(|n| {
        let mut n = n.borrow_mut();
        if let Some(slot) = n.offsets.get_mut(id.index()) {
            *slot = u32::try_from(offset).unwrap_or(UNKNOWN);
        }
    })
}

/// The id the next node will get, for opening a source range.
pub fn next_node_id() -> NodeId {
    NODES.with(|n| NodeId(n.borrow().offsets.len() as u32))
}

/// Open a source range: every node from here on belongs to `file` until it is closed.
pub fn open_source(first: NodeId, file: &str, text: &str) -> usize {
    NODES.with(|n| {
        let mut n = n.borrow_mut();
        n.sources.push(Source {
            first: first.0,
            last: u32::MAX,
            file: file.to_string(),
            text: text.to_string(),
        });
        n.sources.len() - 1
    })
}

/// Close a source range at the last node handed out.
pub fn close_source(handle: usize) {
    NODES.with(|n| {
        let mut n = n.borrow_mut();
        let last = n.offsets.len().saturating_sub(1) as u32;
        if let Some(source) = n.sources.get_mut(handle) {
            source.last = last;
        }
    })
}

/// Where a node came from: file, line and column, one-based.
///
/// `None` when the node was built somewhere that did not know its position, or before
/// any source was registered — a test that parses a string directly, for instance.
pub fn locate(id: NodeId) -> Option<Location> {
    NODES.with(|n| {
        let n = n.borrow();
        let offset = *n.offsets.get(id.index())?;
        if offset == UNKNOWN {
            return None;
        }
        // The narrowest range containing the node, so a string interpolation's own
        // parse wins over the file it sits in.
        let source = n
            .sources
            .iter()
            .filter(|s| s.first <= id.0 && id.0 <= s.last)
            .min_by_key(|s| s.last.saturating_sub(s.first))?;
        let upto = source.text.get(..offset as usize)?;
        Some(Location {
            file: source.file.clone(),
            line: upto.matches('\n').count() as u32 + 1,
            column: (upto.len() - upto.rfind('\n').map_or(0, |i| i + 1)) as u32 + 1,
        })
    })
}

/// Top-level expression
///
/// Every variant carries a `NodeId`. It is deliberately last in each variant, and
/// deliberately not something a `match` has to mention: existing patterns keep working
/// with `..`, and the ones that read a node's identity ask for it by name.
#[derive(Debug, Clone)]
pub enum Expr {
    /// String literal: "hello"
    Str(&'static str, NodeId),
    /// String interpolation: "x=${expr}"
    StrInterp(Vec<StrPart>, NodeId),
    /// Integer literal: 42, -3
    Int(i128, NodeId),
    /// Float literal: 3.14, -2.5
    /// A fractional literal: the double it parses to, and the SAME literal scaled by
    /// `Dec::SCALE`.
    ///
    /// Both, because `Dec` is not a float: `147.666666666666666666` is exact as a
    /// fixed-point value and is not as a double, so reconstructing the one from the
    /// other loses the digits the type exists to keep. Which is used depends on the
    /// type the checker gives the node.
    Float(f64, i128, NodeId),
    /// Identifier: x, main
    Ident(&'static str, NodeId),
    /// Qualified name: Module.function
    Qualified {
        module: &'static str,
        name: &'static str,
        id: NodeId,
    },
    /// Binary operation: left op right
    BinOp {
        left: Box<Expr>,
        op: BinOp,
        right: Box<Expr>,
        id: NodeId,
    },
    /// Lambda function: |x| body or |x, y| x + y
    ///
    /// `Rc`, not `Box` or `Vec`: evaluating this node builds a closure that has to own
    /// its body and parameter list, and a `Box` made that a DEEP clone of every AST
    /// node in the body — once per closure created, which inside a loop is once per
    /// iteration. It also forced a lifetime transmute, since the clone was not
    /// `'static`. Sharing is sound because the AST is immutable after parsing.
    Lambda {
        params: std::rc::Rc<[&'static str]>,
        body: std::rc::Rc<Expr>,
        id: NodeId,
    },
    /// Function call: f(x) or add(1, 2)
    Call {
        func: Box<Expr>,
        args: Vec<Expr>,
        id: NodeId,
    },
    /// Let binding: `x = value` followed by `body`.
    ///
    /// `annotation` is the declared type from a preceding `x : Type` line, when there
    /// was one. Carrying it here is what lets the checker know an identifier's type,
    /// reject a tag outside a closed union, and check a `match` for exhaustiveness.
    Let {
        name: &'static str,
        annotation: Option<crate::types::Type>,
        value: Box<Expr>,
        body: Box<Expr>,
        id: NodeId,
    },
    /// Empty record `{}` — Roc's unit value.
    Unit(NodeId),
    /// Record literal: `{ x: 1, y: 2 }`. Fields keep source order here; only
    /// `Str.inspect` sorts them.
    Record(Vec<(&'static str, Expr)>, NodeId),
    /// Boolean literal, from `Bool.True` / `Bool.False`.
    Bool(bool, NodeId),
    /// List literal: `[1, 2, 3]`, `[]`.
    List(Vec<Expr>, NodeId),
    /// Record update: `{ ..base, field: value }`.
    ///
    /// Builds a NEW record from `base` with the named fields replaced; records are not
    /// mutated. Note the spelling — `{ base & field: value }` is rejected by roc.
    RecordUpdate {
        base: Box<Expr>,
        fields: Vec<(&'static str, Expr)>,
        id: NodeId,
    },
    /// Range: `0..<3` (exclusive) or `1..=3` (inclusive).
    ///
    /// NOT a list — roc inspects a range as `<opaque>` and rejects passing one where a
    /// `List` is wanted. It is iterable by `for`.
    Range {
        start: Box<Expr>,
        end: Box<Expr>,
        inclusive: bool,
        id: NodeId,
    },
    /// Tuple literal: `("Roc", 1)`.
    ///
    /// Heterogeneous and fixed-length, unlike a list. `(1)` is NOT a one-tuple — it
    /// is a parenthesised expression — so this always holds two or more elements.
    Tuple(Vec<Expr>, NodeId),
    /// Positional tuple access: `pair.0`. Zero-based.
    TupleIndex {
        tuple: Box<Expr>,
        index: usize,
        id: NodeId,
    },
    /// Pattern match: `match scrutinee { pattern => body ... }`.
    ///
    /// An expression, like `if`: every arm's body has the same type. Arms are tried
    /// in order and the first whose pattern matches (and whose guard holds) wins, so
    /// order is significant.
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
        id: NodeId,
    },
    /// Conditional expression: `if cond a else b`.
    ///
    /// An expression, not a statement, so `else` is mandatory — roc rejects a bare
    /// `if`. `else if` is not a separate form: it is an `If` whose `otherwise` is
    /// another `If`.
    If {
        condition: Box<Expr>,
        then_branch: Box<Expr>,
        otherwise: Box<Expr>,
        id: NodeId,
    },
    /// `var x = value` then `body` — a REBINDABLE binding.
    ///
    /// Distinct from `Let`: a plain binding cannot be reassigned (roc reports it as a
    /// redeclaration), and only a `var` may appear on the left of an assignment.
    VarDecl {
        name: &'static str,
        value: Box<Expr>,
        body: Box<Expr>,
        id: NodeId,
    },
    /// `x = value` where `x` is an existing `var` — updates it IN PLACE.
    ///
    /// Shadowing would be wrong: a loop body runs in its own scope, so a new binding
    /// there would be discarded and the value read after the loop unchanged.
    Assign {
        name: &'static str,
        value: Box<Expr>,
        body: Box<Expr>,
        id: NodeId,
    },
    /// `for name in iterable { body }`. Evaluates to `{}`.
    For {
        name: &'static str,
        iterable: Box<Expr>,
        body: Box<Expr>,
        id: NodeId,
    },
    /// `while condition { body }`. Evaluates to `{}`.
    While {
        condition: Box<Expr>,
        body: Box<Expr>,
        id: NodeId,
    },
    /// `return value` — leaves the enclosing FUNCTION immediately.
    ///
    /// Unlike `break`, which leaves a loop, this unwinds to the lambda boundary.
    Return(Box<Expr>, NodeId),
    /// `crash "message"` — aborts the program.
    Crash(Box<Expr>, NodeId),
    /// `expect condition` — checks an assertion.
    ///
    /// A failure is REPORTED, not fatal: roc prints to stderr and carries on.
    Expect(Box<Expr>, NodeId),
    /// `dbg value` — prints the value to stderr and carries on.
    Dbg(Box<Expr>, NodeId),
    /// `break` — leaves the nearest enclosing loop.
    ///
    /// There is no `continue`: it crashes the roc compiler on nightly-2026-09-03, so
    /// no golden pair can be written against it.
    Break(NodeId),
    /// Static dispatch: `receiver.method(args)`.
    ///
    /// Resolved through the RECEIVER'S TYPE — `n.to_str()` is `I64.to_str(n)` when
    /// `n : I64` — with the receiver becoming the first argument. That ordering is why
    /// roc's builtins take their subject first (`List.map(list, fn)`).
    ///
    /// Distinct from `FieldAccess`: `s.is_empty` reads a field, `s.is_empty()` calls a
    /// method, so the parens are what separate them.
    Dispatch {
        receiver: Box<Expr>,
        method: &'static str,
        args: Vec<Expr>,
        id: NodeId,
    },
    /// Optional field access: `config.?timeout`.
    ///
    /// Yields `Ok(value)` when the field is present and `Err(MissingField)` when it is
    /// not. Only meaningful for a field declared `name ?: Type` on a nominal's backing
    /// record — using it on an ordinary field segfaults the roc compiler.
    OptionalField {
        record: Box<Expr>,
        field: &'static str,
        id: NodeId,
    },
    /// Record field access: `point.x`.
    ///
    /// Distinct from `Expr::Qualified`: Roc capitalises modules and types, so a
    /// lowercase receiver (`point.x`) is a field access while an uppercase one
    /// (`Str.inspect`) is a module member.
    FieldAccess {
        record: Box<Expr>,
        field: &'static str,
        id: NodeId,
    },
    /// Tag application: `Ok(x)`, `Err(e)`, or a bare tag like `Red` (no args).
    Tag {
        name: &'static str,
        args: Vec<Expr>,
        id: NodeId,
    },
}


impl Expr {
    /// This node's direct sub-expressions, in source order.
    ///
    /// Exhaustive over `Expr` on purpose, like the compiler's own match: a variant
    /// added to the AST has to say what it contains, or a walk that relies on this
    /// would silently stop short.
    pub fn children(&self) -> Vec<&Expr> {
        match self {
            Expr::Str(..) | Expr::Int(..) | Expr::Float(..) | Expr::Ident(..)
            | Expr::Qualified { .. } | Expr::Unit(_) | Expr::Bool(..) | Expr::Break(_) => Vec::new(),
            Expr::StrInterp(parts, _) => parts
                .iter()
                .filter_map(|part| match part {
                    StrPart::Expr(e) => Some(*e),
                    StrPart::Literal(_) => None,
                })
                .collect(),
            Expr::BinOp { left, right, .. } => vec![left, right],
            Expr::Lambda { body, .. } => vec![body],
            Expr::Call { func, args, .. } => {
                std::iter::once(&**func).chain(args.iter()).collect()
            }
            Expr::Let { value, body, .. }
            | Expr::VarDecl { value, body, .. }
            | Expr::Assign { value, body, .. } => vec![value, body],
            Expr::Record(fields, _) => fields.iter().map(|(_, v)| v).collect(),
            Expr::RecordUpdate { base, fields, .. } => {
                std::iter::once(&**base).chain(fields.iter().map(|(_, v)| v)).collect()
            }
            Expr::List(items, _) | Expr::Tuple(items, _) => items.iter().collect(),
            Expr::Tag { args, .. } => args.iter().collect(),
            Expr::Range { start, end, .. } => vec![start, end],
            Expr::TupleIndex { tuple, .. } => vec![tuple],
            Expr::Match { scrutinee, arms, .. } => std::iter::once(&**scrutinee)
                .chain(arms.iter().flat_map(|arm| arm.guard.iter().chain(std::iter::once(&arm.body))))
                .collect(),
            Expr::If { condition, then_branch, otherwise, .. } => {
                vec![condition, then_branch, otherwise]
            }
            Expr::For { iterable, body, .. } => vec![iterable, body],
            Expr::While { condition, body, .. } => vec![condition, body],
            Expr::Return(inner, _) | Expr::Crash(inner, _) | Expr::Expect(inner, _)
            | Expr::Dbg(inner, _) => vec![inner],
            Expr::Dispatch { receiver, args, .. } => {
                std::iter::once(&**receiver).chain(args.iter()).collect()
            }
            Expr::OptionalField { record, .. } | Expr::FieldAccess { record, .. } => vec![record],
        }
    }

    /// This node's identity, for looking it up in a side table.
    pub fn id(&self) -> NodeId {
        match self {
            Expr::Qualified { id, .. } => *id,
            Expr::BinOp { id, .. } => *id,
            Expr::Lambda { id, .. } => *id,
            Expr::Call { id, .. } => *id,
            Expr::Let { id, .. } => *id,
            Expr::RecordUpdate { id, .. } => *id,
            Expr::Range { id, .. } => *id,
            Expr::TupleIndex { id, .. } => *id,
            Expr::Match { id, .. } => *id,
            Expr::If { id, .. } => *id,
            Expr::VarDecl { id, .. } => *id,
            Expr::Assign { id, .. } => *id,
            Expr::For { id, .. } => *id,
            Expr::While { id, .. } => *id,
            Expr::Dispatch { id, .. } => *id,
            Expr::OptionalField { id, .. } => *id,
            Expr::FieldAccess { id, .. } => *id,
            Expr::Tag { id, .. } => *id,
            Expr::Str(.., id) => *id,
            Expr::StrInterp(.., id) => *id,
            Expr::Int(.., id) => *id,
            Expr::Float(.., id) => *id,
            Expr::Ident(.., id) => *id,
            Expr::Unit(.., id) => *id,
            Expr::Record(.., id) => *id,
            Expr::Bool(.., id) => *id,
            Expr::List(.., id) => *id,
            Expr::Tuple(.., id) => *id,
            Expr::Return(.., id) => *id,
            Expr::Crash(.., id) => *id,
            Expr::Expect(.., id) => *id,
            Expr::Dbg(.., id) => *id,
            Expr::Break(.., id) => *id,
        }
    }
}

// A block `{ a = 1 \n f(a) \n expr }` is NOT its own variant: the parser lowers
// it to nested `Let`s, with non-binding statements bound to `_`.

/// One arm of a `match`: alternatives, an optional guard, and a body.
#[derive(Debug, Clone)]
pub struct MatchArm {
    /// `A | B => body` — the arm matches if ANY of these patterns match.
    pub patterns: Vec<Pattern>,
    /// `pattern if cond => body`. Checked only after the pattern matches, with the
    /// pattern's bindings in scope.
    pub guard: Option<Expr>,
    pub body: Expr,
}

/// A `match` pattern.
///
/// Learning.md §9 has the verified semantics of each form — arm order, guards falling
/// through, and where `..` may sit in a list pattern.
#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    /// `_` — matches anything, binds nothing.
    Wildcard,
    /// `x` — matches anything and binds it to `x`.
    Binding(&'static str),
    /// `1`, `3.5`, `"hello"` — matches an equal value.
    Int(i128),
    /// The float value and the exact scaled `Dec` (value * 10^18) it was written as,
    /// so a long-form `Dec` pattern like `1.000000000000000001` matches exactly.
    Float(f64, i128),
    Str(&'static str),
    /// `"foo${name}bar"` — matches a string that starts with `prefix`, then for each
    /// segment captures text up to the next literal and binds it (`_` binds nothing).
    /// A last segment with an empty literal captures the rest of the string.
    StrInterp {
        prefix: &'static str,
        segments: Vec<(&'static str, &'static str)>,
    },
    /// `Ok(n) as whole` — matches `inner` and also binds the whole value to `name`.
    As {
        name: &'static str,
        inner: Box<Pattern>,
    },
    /// `Red`, `Foo(a, b)`, `Wrap(Inner(s))` — patterns nest to any depth.
    Tag {
        name: &'static str,
        args: Vec<Pattern>,
    },
    /// Tuple pattern: `(0, 0)`, `(x, 0)`. Fixed arity, matched element-wise.
    Tuple(Vec<Pattern>),
    /// `Name.(payload)` — unwraps a nominal over a non-record backing. The nominal
    /// is erased at run time, so it matches as `inner` does; the checker types
    /// `inner` against the BACKING, so `|Text.(units)|` binds a `List(U8)`.
    Nominal {
        name: &'static str,
        inner: Box<Pattern>,
    },
    /// Record pattern: `{ x, y }`, `Point.{ x }`, `{ email: _, ..rest }`.
    ///
    /// Each entry is a field name and the pattern matched against it; in a PATTERN a
    /// bare `x` is shorthand for `x: x` (in a record LITERAL it is not — `{ x }` there
    /// is a block).
    ///
    /// `rest` is `Some(name)` for `..name`, which binds every field NOT named into a
    /// new record — so naming a field `_` and capturing the rest removes it.
    Record {
        fields: Vec<(&'static str, Pattern)>,
        rest: Option<&'static str>,
    },
    /// List pattern: `[]`, `[a, b]`, `[1, 2, ..]`, `[2, .., 1]`, `[9, .. as tail]`.
    ///
    /// `rest` is `None` for an exact-length pattern. When present it holds the
    /// position of `..` within `before`+`after` and an optional name to bind the
    /// skipped elements to. At most one `..` per pattern.
    List {
        /// Elements matched from the front.
        before: Vec<Pattern>,
        /// `Some(name)` for `.. as name`, `Some(None)` shape handled by `rest`.
        rest: Option<Option<&'static str>>,
        /// Elements matched from the back, after the `..`.
        after: Vec<Pattern>,
    },
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Pattern::Wildcard => write!(f, "_"),
            Pattern::Binding(name) => write!(f, "{}", name),
            Pattern::Int(n) => write!(f, "{}", n),
            Pattern::Float(n, _) => write!(f, "{}", n),
            Pattern::Str(s) => write!(f, "\"{}\"", s),
            Pattern::StrInterp { prefix, segments } => {
                write!(f, "\"{}", prefix)?;
                for (name, literal) in segments {
                    write!(f, "${{{}}}{}", name, literal)?;
                }
                write!(f, "\"")
            }
            Pattern::As { name, inner } => write!(f, "{} as {}", inner, name),
            Pattern::Nominal { name, inner } => write!(f, "{}.({})", name, inner),
            Pattern::Tag { name, args } => {
                if args.is_empty() {
                    write!(f, "{}", name)
                } else {
                    let rendered: Vec<String> = args.iter().map(|a| a.to_string()).collect();
                    write!(f, "{}({})", name, rendered.join(", "))
                }
            }
            Pattern::Tuple(items) => {
                let rendered: Vec<String> = items.iter().map(|p| p.to_string()).collect();
                write!(f, "({})", rendered.join(", "))
            }
            Pattern::Record { fields, rest } => {
                let mut rendered: Vec<String> = fields
                    .iter()
                    .map(|(name, p)| {
                        // `{ x }` round-trips as itself when the pattern is just the
                        // field's own name.
                        if matches!(p, Pattern::Binding(b) if b == name) {
                            name.to_string()
                        } else {
                            format!("{}: {}", name, p)
                        }
                    })
                    .collect();
                if let Some(name) = rest {
                    rendered.push(format!("..{}", name));
                }
                write!(f, "{{ {} }}", rendered.join(", "))
            }
            Pattern::List { before, rest, after } => {
                let mut parts: Vec<String> = before.iter().map(|p| p.to_string()).collect();
                if let Some(binding) = rest {
                    parts.push(match binding {
                        Some(name) => format!(".. as {}", name),
                        None => "..".to_string(),
                    });
                }
                parts.extend(after.iter().map(|p| p.to_string()));
                write!(f, "[{}]", parts.join(", "))
            }
        }
    }
}

/// Part of a string interpolation
#[derive(Debug, Clone)]
pub enum StrPart {
    /// Literal part of string
    Literal(&'static str),
    /// Expression to interpolate: ${...}
    Expr(&'static Expr),
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expr::Str(s, _) => write!(f, "\"{}\"", s),
            Expr::StrInterp(parts, _) => {
                write!(f, "\"")?;
                for part in parts {
                    match part {
                        StrPart::Literal(s) => write!(f, "{}", s)?,
                        StrPart::Expr(e) => write!(f, "${{{}}}", e)?,
                    }
                }
                write!(f, "\"")
            }
            Expr::Int(n, _) => write!(f, "{}", n),
            Expr::Float(n, ..) => write!(f, "{}", n),
            Expr::Ident(name, _) => write!(f, "{}", name),
            Expr::Unit(_) => write!(f, "{{}}"),
            Expr::Bool(b, _) => write!(f, "Bool.{}", if *b { "True" } else { "False" }),
            Expr::List(items, _) => {
                let rendered: Vec<String> = items.iter().map(|i| i.to_string()).collect();
                write!(f, "[{}]", rendered.join(", "))
            }
            Expr::RecordUpdate { base, fields, .. } => {
                let rendered: Vec<String> =
                    fields.iter().map(|(k, v)| format!("{}: {}", k, v)).collect();
                write!(f, "{{ ..{}, {} }}", base, rendered.join(", "))
            }
            Expr::Range { start, end, inclusive, .. } => {
                write!(f, "{}..{}{}", start, if *inclusive { "=" } else { "<" }, end)
            }
            Expr::Tuple(items, _) => {
                let rendered: Vec<String> = items.iter().map(|i| i.to_string()).collect();
                write!(f, "({})", rendered.join(", "))
            }
            Expr::TupleIndex { tuple, index, .. } => write!(f, "{}.{}", tuple, index),
            Expr::FieldAccess { record, field, .. } => write!(f, "{}.{}", record, field),
            Expr::OptionalField { record, field, .. } => write!(f, "{}.?{}", record, field),
            Expr::VarDecl { name, value, body, .. } => {
                write!(f, "var {} = {} in {}", name, value, body)
            }
            Expr::Assign { name, value, body, .. } => {
                write!(f, "{} := {} in {}", name, value, body)
            }
            Expr::For { name, iterable, body, .. } => {
                write!(f, "for {} in {} {{ {} }}", name, iterable, body)
            }
            Expr::While { condition, body, .. } => {
                write!(f, "while {} {{ {} }}", condition, body)
            }
            Expr::Return(value, _) => write!(f, "return {}", value),
            Expr::Crash(message, _) => write!(f, "crash {}", message),
            Expr::Expect(condition, _) => write!(f, "expect {}", condition),
            Expr::Dbg(value, _) => write!(f, "dbg {}", value),
            Expr::Break(_) => write!(f, "break"),
            Expr::Dispatch { receiver, method, args, .. } => {
                let rendered: Vec<String> = args.iter().map(|a| a.to_string()).collect();
                write!(f, "{}.{}({})", receiver, method, rendered.join(", "))
            }
            Expr::If { condition, then_branch, otherwise, .. } => {
                write!(f, "if {} {} else {}", condition, then_branch, otherwise)
            }
            Expr::Match { scrutinee, arms, .. } => {
                write!(f, "match {} {{", scrutinee)?;
                for arm in arms {
                    let pats: Vec<String> = arm.patterns.iter().map(|p| p.to_string()).collect();
                    write!(f, " {}", pats.join(" | "))?;
                    if let Some(guard) = &arm.guard {
                        write!(f, " if {}", guard)?;
                    }
                    write!(f, " => {}", arm.body)?;
                }
                write!(f, " }}")
            }
            Expr::Record(fields, _) => {
                let rendered: Vec<String> =
                    fields.iter().map(|(k, v)| format!("{}: {}", k, v)).collect();
                write!(f, "{{ {} }}", rendered.join(", "))
            }
            Expr::Tag { name, args, .. } => {
                if args.is_empty() {
                    write!(f, "{}", name)
                } else {
                    let rendered: Vec<String> = args.iter().map(|a| a.to_string()).collect();
                    write!(f, "{}({})", name, rendered.join(", "))
                }
            }
            Expr::Qualified { module, name, .. } => {
                write!(f, "{}.{}", module, name)
            }
            Expr::BinOp { left, op, right, .. } => {
                write!(f, "({} {} {})", left, op, right)
            }
            Expr::Lambda { params, body, .. } => {
                write!(f, "|{}| {}", params.join(", "), body)
            }
            Expr::Call { func, args, .. } => {
                write!(f, "{}(", func)?;
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", arg)?;
                }
                write!(f, ")")
            }
            Expr::Let { name, value, body, .. } => {
                write!(f, "let {} = {} in {}", name, value, body)
            }
        }
    }
}

