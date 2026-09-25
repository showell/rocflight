//! `Builtin.roc`, parsed at BUILD time and read back at run time.
//!
//! A four-line program that names a `Dict` parses 1,358 lines of `Builtin.roc` — source
//! the user did not write and that cannot change without rebuilding the binary. That
//! was 1.8ms of a 2.6ms run. This is the same trees, written out once by
//! `cargo run --bin gen-artifact` and `include_bytes!`d back in.
//!
//! **Every name is borrowed from the blob, not interned.** The strings live in the
//! binary's read-only data, so reading one is a slice and not an allocation — which is
//! sound because every comparison of a name in the interpreter is by content, as
//! `memory::string_pool` already documents. That is the single biggest reason this is
//! faster than parsing rather than merely different.
//!
//! **Staleness is a BUILD error, not a run-time check.** The artifact records a hash of
//! the source it was made from and `build.rs` compares it against `src/roc/Builtin.roc`,
//! so an artifact that no longer matches fails `cargo build` with the command to
//! regenerate it. Checking at run time would mean hashing 700kB on every startup, which
//! is most of what this phase saves.

use crate::ast::{Expr, MatchArm, NodeId, Pattern, StrPart};
use crate::types::Type;

/// Bumped whenever the FORMAT changes, so an artifact from an older tree is rejected by
/// `build.rs` rather than decoded as nonsense.
pub const MAGIC: &[u8; 8] = b"ROCFLT06";

/// FNV-1a of the source an artifact was built from. `build.rs` computes the same thing
/// over `src/roc/Builtin.roc` and refuses to build if they differ.
pub fn source_hash(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

// ---------------------------------------------------------------- writing

/// Builds the blob. Used by `gen-artifact` only; the interpreter never writes one.
pub struct Writer {
    out: Vec<u8>,
    strings: Vec<String>,
    index: std::collections::HashMap<String, u32>,
}

impl Default for Writer {
    fn default() -> Self {
        Writer { out: Vec::new(), strings: Vec::new(), index: std::collections::HashMap::new() }
    }
}

impl Writer {
    /// A bare count, for the section boundary the reader expects.
    pub fn count(&mut self, n: usize) {
        self.u(n as u64);
    }

    fn u(&mut self, mut n: u64) {
        // LEB128: a length, a node id or a small tag is one byte almost always.
        while n >= 0x80 {
            self.out.push((n as u8) | 0x80);
            n >>= 7;
        }
        self.out.push(n as u8);
    }

    fn tag(&mut self, t: u8) {
        self.out.push(t);
    }

    /// Fixed width, for BYTECODE. A varint costs a loop and a branch per byte to
    /// decode, and an opcode field is read once per instruction in the chunk — the
    /// artifact is bigger this way and that is the trade this section wants.
    fn w16(&mut self, n: u16) {
        self.out.extend_from_slice(&n.to_le_bytes());
    }

    fn w32(&mut self, n: u32) {
        self.out.extend_from_slice(&n.to_le_bytes());
    }

    fn i128(&mut self, n: i128) {
        self.out.extend_from_slice(&n.to_le_bytes());
    }

    fn f64(&mut self, n: f64) {
        self.out.extend_from_slice(&n.to_bits().to_le_bytes());
    }

    fn bool(&mut self, b: bool) {
        self.out.push(u8::from(b));
    }

    /// A name, as an index into the shared table. `Builtin.roc` repeats its names
    /// heavily, so this is both smaller and one less thing to read back.
    fn s(&mut self, text: &str) {
        let next = self.strings.len() as u32;
        let at = *self.index.entry(text.to_string()).or_insert_with(|| {
            self.strings.push(text.to_string());
            next
        });
        self.u(u64::from(at));
    }

    fn node(&mut self, id: NodeId, base: u32) {
        // Relative to the member's own first node, so loading rebases by addition.
        self.u(u64::from(id.index() as u32 - base));
    }

    fn seq<T>(&mut self, items: &[T], mut each: impl FnMut(&mut Self, &T)) {
        self.u(items.len() as u64);
        for item in items {
            each(self, item);
        }
    }

    /// The finished blob: magic, source hash, string table, then the members.
    pub fn finish(self, hash: u64, body_count: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.out.len() + 64 * self.strings.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&hash.to_le_bytes());
        out.extend_from_slice(&(body_count as u32).to_le_bytes());
        // The table: every distinct name, length-prefixed, in one run.
        let mut header = Writer::default();
        header.u(self.strings.len() as u64);
        for text in &self.strings {
            header.u(text.len() as u64);
        }
        out.extend_from_slice(&header.out);
        for text in &self.strings {
            out.extend_from_slice(text.as_bytes());
        }
        out.extend_from_slice(&self.out);
        out
    }
}

// ---------------------------------------------------------------- reading

pub struct Reader<'a> {
    blob: &'static [u8],
    at: usize,
    strings: &'a [&'static str],
}

impl<'a> Reader<'a> {
    fn u(&mut self) -> u64 {
        let mut n = 0u64;
        let mut shift = 0;
        loop {
            let byte = self.blob[self.at];
            self.at += 1;
            n |= u64::from(byte & 0x7f) << shift;
            if byte < 0x80 {
                return n;
            }
            shift += 7;
        }
    }

    fn tag(&mut self) -> u8 {
        let t = self.blob[self.at];
        self.at += 1;
        t
    }

    fn r16(&mut self) -> u16 {
        let n = u16::from_le_bytes([self.blob[self.at], self.blob[self.at + 1]]);
        self.at += 2;
        n
    }

    fn r32(&mut self) -> u32 {
        let n = u32::from_le_bytes([
            self.blob[self.at],
            self.blob[self.at + 1],
            self.blob[self.at + 2],
            self.blob[self.at + 3],
        ]);
        self.at += 4;
        n
    }

    fn i128(&mut self) -> i128 {
        let mut buf = [0u8; 16];
        buf.copy_from_slice(&self.blob[self.at..self.at + 16]);
        self.at += 16;
        i128::from_le_bytes(buf)
    }

    fn f64(&mut self) -> f64 {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&self.blob[self.at..self.at + 8]);
        self.at += 8;
        f64::from_bits(u64::from_le_bytes(buf))
    }

    fn bool(&mut self) -> bool {
        self.tag() != 0
    }

    /// A name, borrowed from the blob. No allocation and no interning: the bytes are in
    /// the binary and outlive everything.
    fn s(&mut self) -> &'static str {
        let at = self.u() as usize;
        self.strings[at]
    }

    fn node(&mut self, base: u32) -> NodeId {
        NodeId(base + self.u() as u32)
    }

    fn seq<T>(&mut self, base: u32, mut each: impl FnMut(&mut Self, u32) -> T) -> Vec<T> {
        let n = self.u() as usize;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(each(self, base));
        }
        out
    }
}

// ---------------------------------------------------------------- types

fn put_type(w: &mut Writer, ty: &Type) {
    match ty {
        Type::Str => w.tag(0),
        Type::U8 => w.tag(1),
        Type::U16 => w.tag(2),
        Type::U32 => w.tag(3),
        Type::U64 => w.tag(4),
        Type::U128 => w.tag(5),
        Type::I8 => w.tag(6),
        Type::I16 => w.tag(7),
        Type::I32 => w.tag(8),
        Type::I64 => w.tag(9),
        Type::I128 => w.tag(10),
        Type::F32 => w.tag(11),
        Type::F64 => w.tag(12),
        Type::Dec => w.tag(13),
        Type::Bool => w.tag(14),
        Type::Unit => w.tag(15),
        Type::TypeVar(v) => {
            w.tag(16);
            w.u(u64::from(*v));
        }
        Type::List(inner) => {
            w.tag(17);
            put_type(w, inner);
        }
        Type::Function(a, b) => {
            w.tag(18);
            put_type(w, a);
            put_type(w, b);
        }
        Type::Optional(inner) => {
            w.tag(19);
            put_type(w, inner);
        }
        Type::Range(inner) => {
            w.tag(20);
            put_type(w, inner);
        }
        Type::Tuple(items) => {
            w.tag(21);
            w.seq(items, |w, t| put_type(w, t));
        }
        Type::Record { fields, open } => {
            w.tag(22);
            w.seq(fields, |w, (n, t)| {
                w.s(n);
                put_type(w, t);
            });
            w.bool(*open);
        }
        // A nominal's arguments ride in tag 25; tag 23 is one without, as it
        // always was, so an artifact with none reads the same.
        Type::Nominal { name, backing, args } if args.is_empty() => {
            w.tag(23);
            w.s(name);
            put_type(w, backing);
        }
        Type::Nominal { name, backing, args } => {
            w.tag(25);
            w.s(name);
            put_type(w, backing);
            w.seq(args, |w, t| put_type(w, t));
        }
        Type::TagUnion { tags, open } => {
            w.tag(24);
            w.seq(tags, |w, (n, payload)| {
                w.s(n);
                w.seq(payload, |w, t| put_type(w, t));
            });
            w.bool(*open);
        }
    }
}

fn get_type(r: &mut Reader) -> Type {
    match r.tag() {
        0 => Type::Str,
        1 => Type::U8,
        2 => Type::U16,
        3 => Type::U32,
        4 => Type::U64,
        5 => Type::U128,
        6 => Type::I8,
        7 => Type::I16,
        8 => Type::I32,
        9 => Type::I64,
        10 => Type::I128,
        11 => Type::F32,
        12 => Type::F64,
        13 => Type::Dec,
        14 => Type::Bool,
        15 => Type::Unit,
        16 => Type::TypeVar(r.u() as u32),
        17 => Type::List(Box::new(get_type(r))),
        18 => {
            let a = get_type(r);
            let b = get_type(r);
            Type::Function(Box::new(a), Box::new(b))
        }
        19 => Type::Optional(Box::new(get_type(r))),
        20 => Type::Range(Box::new(get_type(r))),
        21 => Type::Tuple(r.seq(0, |r, _| get_type(r))),
        22 => {
            let fields = r.seq(0, |r, _| (r.s(), get_type(r)));
            Type::Record { fields, open: r.bool() }
        }
        23 => {
            let name = r.s();
            Type::Nominal { name, backing: Box::new(get_type(r)), args: Vec::new() }
        }
        25 => {
            let name = r.s();
            let backing = Box::new(get_type(r));
            Type::Nominal { name, backing, args: r.seq(0, |r, _| get_type(r)) }
        }
        24 => {
            let tags = r.seq(0, |r, _| (r.s(), r.seq(0, |r, _| get_type(r))));
            Type::TagUnion { tags, open: r.bool() }
        }
        other => unreachable_tag("Type", other),
    }
}

// ---------------------------------------------------------------- patterns

fn put_pattern(w: &mut Writer, p: &Pattern) {
    match p {
        Pattern::Wildcard => w.tag(0),
        Pattern::Binding(n) => {
            w.tag(1);
            w.s(n);
        }
        Pattern::Int(n) => {
            w.tag(2);
            w.i128(*n);
        }
        Pattern::Float(f, raw) => {
            w.tag(3);
            w.f64(*f);
            w.i128(*raw);
        }
        Pattern::Str(s) => {
            w.tag(4);
            w.s(s);
        }
        Pattern::StrInterp { prefix, segments } => {
            w.tag(5);
            w.s(prefix);
            w.seq(segments, |w, (a, b)| {
                w.s(a);
                w.s(b);
            });
        }
        Pattern::As { name, inner } => {
            w.tag(6);
            w.s(name);
            put_pattern(w, inner);
        }
        Pattern::Tag { name, args } => {
            w.tag(7);
            w.s(name);
            w.seq(args, |w, p| put_pattern(w, p));
        }
        Pattern::Tuple(items) => {
            w.tag(8);
            w.seq(items, |w, p| put_pattern(w, p));
        }
        Pattern::Record { fields, rest } => {
            w.tag(9);
            w.seq(fields, |w, (n, p)| {
                w.s(n);
                put_pattern(w, p);
            });
            put_opt_str(w, rest.as_ref());
        }
        Pattern::List { before, rest, after } => {
            w.tag(10);
            w.seq(before, |w, p| put_pattern(w, p));
            match rest {
                None => w.tag(0),
                Some(None) => w.tag(1),
                Some(Some(name)) => {
                    w.tag(2);
                    w.s(name);
                }
            }
            w.seq(after, |w, p| put_pattern(w, p));
        }
        Pattern::Nominal { name, inner } => {
            w.tag(11);
            w.s(name);
            put_pattern(w, inner);
        }
    }
}

fn get_pattern(r: &mut Reader) -> Pattern {
    match r.tag() {
        0 => Pattern::Wildcard,
        1 => Pattern::Binding(r.s()),
        2 => Pattern::Int(r.i128()),
        3 => {
            let f = r.f64();
            Pattern::Float(f, r.i128())
        }
        4 => Pattern::Str(r.s()),
        5 => {
            let prefix = r.s();
            Pattern::StrInterp { prefix, segments: r.seq(0, |r, _| (r.s(), r.s())) }
        }
        6 => {
            let name = r.s();
            Pattern::As { name, inner: Box::new(get_pattern(r)) }
        }
        7 => {
            let name = r.s();
            Pattern::Tag { name, args: r.seq(0, |r, _| get_pattern(r)) }
        }
        8 => Pattern::Tuple(r.seq(0, |r, _| get_pattern(r))),
        9 => {
            let fields = r.seq(0, |r, _| (r.s(), get_pattern(r)));
            Pattern::Record { fields, rest: get_opt_str(r) }
        }
        10 => {
            let before = r.seq(0, |r, _| get_pattern(r));
            let rest = match r.tag() {
                0 => None,
                1 => Some(None),
                2 => Some(Some(r.s())),
                other => unreachable_tag("Pattern::List rest", other),
            };
            Pattern::List { before, rest, after: r.seq(0, |r, _| get_pattern(r)) }
        }
        11 => {
            let name = r.s();
            Pattern::Nominal { name, inner: Box::new(get_pattern(r)) }
        }
        other => unreachable_tag("Pattern", other),
    }
}

fn put_opt_str(w: &mut Writer, s: Option<&&'static str>) {
    match s {
        None => w.tag(0),
        Some(text) => {
            w.tag(1);
            w.s(text);
        }
    }
}

fn get_opt_str(r: &mut Reader) -> Option<&'static str> {
    if r.tag() == 0 {
        None
    } else {
        Some(r.s())
    }
}

// ---------------------------------------------------------------- expressions

fn put_expr(w: &mut Writer, e: &Expr, base: u32) {
    use crate::ast::BinOp;
    match e {
        Expr::Str(s, id) => {
            w.tag(0);
            w.s(s);
            w.node(*id, base);
        }
        Expr::StrInterp(parts, id) => {
            w.tag(1);
            w.seq(parts, |w, part| match part {
                StrPart::Literal(s) => {
                    w.tag(0);
                    w.s(s);
                }
                StrPart::Expr(inner) => {
                    w.tag(1);
                    put_expr(w, inner, base);
                }
            });
            w.node(*id, base);
        }
        Expr::Int(n, id) => {
            w.tag(2);
            w.i128(*n);
            w.node(*id, base);
        }
        Expr::Float(f, raw, id) => {
            w.tag(3);
            w.f64(*f);
            w.i128(*raw);
            w.node(*id, base);
        }
        Expr::Ident(s, id) => {
            w.tag(4);
            w.s(s);
            w.node(*id, base);
        }
        Expr::Qualified { module, name, id } => {
            w.tag(5);
            w.s(module);
            w.s(name);
            w.node(*id, base);
        }
        Expr::BinOp { left, op, right, id } => {
            w.tag(6);
            put_expr(w, left, base);
            w.tag(binop_tag(*op));
            put_expr(w, right, base);
            w.node(*id, base);
        }
        Expr::Lambda { params, body, id } => {
            w.tag(7);
            w.seq(params, |w, p| w.s(p));
            put_expr(w, body, base);
            w.node(*id, base);
        }
        Expr::Call { func, args, id } => {
            w.tag(8);
            put_expr(w, func, base);
            w.seq(args, |w, a| put_expr(w, a, base));
            w.node(*id, base);
        }
        Expr::Let { name, annotation, value, body, id } => {
            w.tag(9);
            w.s(name);
            match annotation {
                None => w.tag(0),
                Some(ty) => {
                    w.tag(1);
                    put_type(w, ty);
                }
            }
            put_expr(w, value, base);
            put_expr(w, body, base);
            w.node(*id, base);
        }
        Expr::Unit(id) => {
            w.tag(10);
            w.node(*id, base);
        }
        Expr::Record(fields, id) => {
            w.tag(11);
            w.seq(fields, |w, (n, v)| {
                w.s(n);
                put_expr(w, v, base);
            });
            w.node(*id, base);
        }
        Expr::Bool(b, id) => {
            w.tag(12);
            w.bool(*b);
            w.node(*id, base);
        }
        Expr::List(items, id) => {
            w.tag(13);
            w.seq(items, |w, i| put_expr(w, i, base));
            w.node(*id, base);
        }
        Expr::RecordUpdate { base: record, fields, id } => {
            w.tag(14);
            put_expr(w, record, base);
            w.seq(fields, |w, (n, v)| {
                w.s(n);
                put_expr(w, v, base);
            });
            w.node(*id, base);
        }
        Expr::Range { start, end, inclusive, id } => {
            w.tag(15);
            put_expr(w, start, base);
            put_expr(w, end, base);
            w.bool(*inclusive);
            w.node(*id, base);
        }
        Expr::Tuple(items, id) => {
            w.tag(16);
            w.seq(items, |w, i| put_expr(w, i, base));
            w.node(*id, base);
        }
        Expr::TupleIndex { tuple, index, id } => {
            w.tag(17);
            put_expr(w, tuple, base);
            w.u(*index as u64);
            w.node(*id, base);
        }
        Expr::Match { scrutinee, arms, id } => {
            w.tag(18);
            put_expr(w, scrutinee, base);
            w.seq(arms, |w, arm| {
                w.seq(&arm.patterns, |w, p| put_pattern(w, p));
                match &arm.guard {
                    None => w.tag(0),
                    Some(g) => {
                        w.tag(1);
                        put_expr(w, g, base);
                    }
                }
                put_expr(w, &arm.body, base);
            });
            w.node(*id, base);
        }
        Expr::If { condition, then_branch, otherwise, id } => {
            w.tag(19);
            put_expr(w, condition, base);
            put_expr(w, then_branch, base);
            put_expr(w, otherwise, base);
            w.node(*id, base);
        }
        Expr::VarDecl { name, value, body, id } => {
            w.tag(20);
            w.s(name);
            put_expr(w, value, base);
            put_expr(w, body, base);
            w.node(*id, base);
        }
        Expr::Assign { name, value, body, id } => {
            w.tag(21);
            w.s(name);
            put_expr(w, value, base);
            put_expr(w, body, base);
            w.node(*id, base);
        }
        Expr::For { name, iterable, body, id } => {
            w.tag(22);
            w.s(name);
            put_expr(w, iterable, base);
            put_expr(w, body, base);
            w.node(*id, base);
        }
        Expr::While { condition, body, id } => {
            w.tag(23);
            put_expr(w, condition, base);
            put_expr(w, body, base);
            w.node(*id, base);
        }
        Expr::Return(inner, id) => {
            w.tag(24);
            put_expr(w, inner, base);
            w.node(*id, base);
        }
        Expr::Crash(inner, id) => {
            w.tag(25);
            put_expr(w, inner, base);
            w.node(*id, base);
        }
        Expr::Expect(inner, id) => {
            w.tag(26);
            put_expr(w, inner, base);
            w.node(*id, base);
        }
        Expr::Dbg(inner, id) => {
            w.tag(27);
            put_expr(w, inner, base);
            w.node(*id, base);
        }
        Expr::Break(id) => {
            w.tag(28);
            w.node(*id, base);
        }
        Expr::Dispatch { receiver, method, args, id } => {
            w.tag(29);
            put_expr(w, receiver, base);
            w.s(method);
            w.seq(args, |w, a| put_expr(w, a, base));
            w.node(*id, base);
        }
        Expr::OptionalField { record, field, id } => {
            w.tag(30);
            put_expr(w, record, base);
            w.s(field);
            w.node(*id, base);
        }
        Expr::FieldAccess { record, field, id } => {
            w.tag(31);
            put_expr(w, record, base);
            w.s(field);
            w.node(*id, base);
        }
        Expr::Tag { name, args, id } => {
            w.tag(32);
            w.s(name);
            w.seq(args, |w, a| put_expr(w, a, base));
            w.node(*id, base);
        }
    }
    // `BinOp` is a plain C-like enum; keeping its mapping beside the writer rather than
    // deriving a number from the variant order means reordering it cannot silently
    // change what an old artifact decodes to — `build.rs` rejects it instead.
    fn binop_tag(op: BinOp) -> u8 {
        match op {
            BinOp::Add => 0,
            BinOp::Sub => 1,
            BinOp::Mul => 2,
            BinOp::Div => 3,
            BinOp::IntDiv => 4,
            BinOp::Rem => 5,
            BinOp::Eq => 6,
            BinOp::Ne => 7,
            BinOp::Lt => 8,
            BinOp::Le => 9,
            BinOp::Gt => 10,
            BinOp::Ge => 11,
            BinOp::And => 12,
            BinOp::Or => 13,
        }
    }
}

fn get_expr(r: &mut Reader, base: u32) -> Expr {
    use crate::ast::BinOp;
    fn binop(t: u8) -> BinOp {
        match t {
            0 => BinOp::Add,
            1 => BinOp::Sub,
            2 => BinOp::Mul,
            3 => BinOp::Div,
            4 => BinOp::IntDiv,
            5 => BinOp::Rem,
            6 => BinOp::Eq,
            7 => BinOp::Ne,
            8 => BinOp::Lt,
            9 => BinOp::Le,
            10 => BinOp::Gt,
            11 => BinOp::Ge,
            12 => BinOp::And,
            13 => BinOp::Or,
            other => unreachable_tag("BinOp", other),
        }
    }
    match r.tag() {
        0 => {
            let s = r.s();
            Expr::Str(s, r.node(base))
        }
        1 => {
            let parts = r.seq(base, |r, base| match r.tag() {
                0 => StrPart::Literal(r.s()),
                // A leaked `&'static Expr`, as the parser makes one.
                1 => StrPart::Expr(Box::leak(Box::new(get_expr(r, base)))),
                other => unreachable_tag("StrPart", other),
            });
            Expr::StrInterp(parts, r.node(base))
        }
        2 => {
            let n = r.i128();
            Expr::Int(n, r.node(base))
        }
        3 => {
            let f = r.f64();
            let raw = r.i128();
            Expr::Float(f, raw, r.node(base))
        }
        4 => {
            let s = r.s();
            Expr::Ident(s, r.node(base))
        }
        5 => {
            let module = r.s();
            let name = r.s();
            Expr::Qualified { module, name, id: r.node(base) }
        }
        6 => {
            let left = Box::new(get_expr(r, base));
            let op = binop(r.tag());
            let right = Box::new(get_expr(r, base));
            Expr::BinOp { left, op, right, id: r.node(base) }
        }
        7 => {
            let params: std::rc::Rc<[&'static str]> = r.seq(base, |r, _| r.s()).into();
            let body = std::rc::Rc::new(get_expr(r, base));
            Expr::Lambda { params, body, id: r.node(base) }
        }
        8 => {
            let func = Box::new(get_expr(r, base));
            let args = r.seq(base, get_expr);
            Expr::Call { func, args, id: r.node(base) }
        }
        9 => {
            let name = r.s();
            let annotation = if r.tag() == 0 { None } else { Some(get_type(r)) };
            let value = Box::new(get_expr(r, base));
            let body = Box::new(get_expr(r, base));
            Expr::Let { name, annotation, value, body, id: r.node(base) }
        }
        10 => Expr::Unit(r.node(base)),
        11 => {
            let fields = r.seq(base, |r, base| (r.s(), get_expr(r, base)));
            Expr::Record(fields, r.node(base))
        }
        12 => {
            let b = r.bool();
            Expr::Bool(b, r.node(base))
        }
        13 => {
            let items = r.seq(base, get_expr);
            Expr::List(items, r.node(base))
        }
        14 => {
            let record = Box::new(get_expr(r, base));
            let fields = r.seq(base, |r, base| (r.s(), get_expr(r, base)));
            Expr::RecordUpdate { base: record, fields, id: r.node(base) }
        }
        15 => {
            let start = Box::new(get_expr(r, base));
            let end = Box::new(get_expr(r, base));
            let inclusive = r.bool();
            Expr::Range { start, end, inclusive, id: r.node(base) }
        }
        16 => {
            let items = r.seq(base, get_expr);
            Expr::Tuple(items, r.node(base))
        }
        17 => {
            let tuple = Box::new(get_expr(r, base));
            let index = r.u() as usize;
            Expr::TupleIndex { tuple, index, id: r.node(base) }
        }
        18 => {
            let scrutinee = Box::new(get_expr(r, base));
            let arms = r.seq(base, |r, base| {
                let patterns = r.seq(base, |r, _| get_pattern(r));
                let guard = if r.tag() == 0 { None } else { Some(get_expr(r, base)) };
                MatchArm { patterns, guard, body: get_expr(r, base) }
            });
            Expr::Match { scrutinee, arms, id: r.node(base) }
        }
        19 => {
            let condition = Box::new(get_expr(r, base));
            let then_branch = Box::new(get_expr(r, base));
            let otherwise = Box::new(get_expr(r, base));
            Expr::If { condition, then_branch, otherwise, id: r.node(base) }
        }
        20 => {
            let name = r.s();
            let value = Box::new(get_expr(r, base));
            let body = Box::new(get_expr(r, base));
            Expr::VarDecl { name, value, body, id: r.node(base) }
        }
        21 => {
            let name = r.s();
            let value = Box::new(get_expr(r, base));
            let body = Box::new(get_expr(r, base));
            Expr::Assign { name, value, body, id: r.node(base) }
        }
        22 => {
            let name = r.s();
            let iterable = Box::new(get_expr(r, base));
            let body = Box::new(get_expr(r, base));
            Expr::For { name, iterable, body, id: r.node(base) }
        }
        23 => {
            let condition = Box::new(get_expr(r, base));
            let body = Box::new(get_expr(r, base));
            Expr::While { condition, body, id: r.node(base) }
        }
        24 => {
            let inner = Box::new(get_expr(r, base));
            Expr::Return(inner, r.node(base))
        }
        25 => {
            let inner = Box::new(get_expr(r, base));
            Expr::Crash(inner, r.node(base))
        }
        26 => {
            let inner = Box::new(get_expr(r, base));
            Expr::Expect(inner, r.node(base))
        }
        27 => {
            let inner = Box::new(get_expr(r, base));
            Expr::Dbg(inner, r.node(base))
        }
        28 => Expr::Break(r.node(base)),
        29 => {
            let receiver = Box::new(get_expr(r, base));
            let method = r.s();
            let args = r.seq(base, get_expr);
            Expr::Dispatch { receiver, method, args, id: r.node(base) }
        }
        30 => {
            let record = Box::new(get_expr(r, base));
            let field = r.s();
            Expr::OptionalField { record, field, id: r.node(base) }
        }
        31 => {
            let record = Box::new(get_expr(r, base));
            let field = r.s();
            Expr::FieldAccess { record, field, id: r.node(base) }
        }
        32 => {
            let name = r.s();
            let args = r.seq(base, get_expr);
            Expr::Tag { name, args, id: r.node(base) }
        }
        other => unreachable_tag("Expr", other),
    }

}

/// A tag the writer cannot produce. Only a corrupt or mismatched artifact reaches this,
/// and `build.rs` rejects a mismatched one before the binary exists.
fn unreachable_tag(what: &str, tag: u8) -> ! {
    panic!("builtin artifact: {} has no variant {} — regenerate it", what, tag)
}


// ---------------------------------------------------------------- bytecode

use crate::vm::{Chunk, Op};
use crate::eval::Value;

/// Reg, u16, u32 and u8 all go out as one varint; the rest have their own byte.
macro_rules! put_scalar {
    ($w:expr, r, $v:expr) => { $w.w16($v) };
    ($w:expr, u16, $v:expr) => { $w.w16($v) };
    ($w:expr, u32, $v:expr) => { $w.w32($v) };
    ($w:expr, u8, $v:expr) => { $w.tag($v) };
    ($w:expr, bool, $v:expr) => { $w.bool($v) };
    ($w:expr, binop, $v:expr) => { $w.tag(binop_tag($v)) };
    ($w:expr, cond, $v:expr) => { $w.tag(cond_tag($v)) };
    ($w:expr, optchunk, $v:expr) => {
        match $v {
            None => $w.tag(0),
            Some(id) => { $w.tag(1); $w.w32(id); }
        }
    };
}

macro_rules! get_scalar {
    ($r:expr, r) => { $r.r16() };
    ($r:expr, u16) => { $r.r16() };
    ($r:expr, u32) => { $r.r32() };
    ($r:expr, u8) => { $r.tag() };
    ($r:expr, bool) => { $r.bool() };
    ($r:expr, binop) => { binop_of($r.tag()) };
    ($r:expr, cond) => { cond_of($r.tag()) };
    ($r:expr, optchunk) => { if $r.tag() == 0 { None } else { Some($r.r32()) } };
}

/// One table, both directions.
///
/// An opcode codec is exactly the place a writer and a reader drift apart, and a drift
/// here decodes as a DIFFERENT PROGRAM rather than as an error. So neither is written
/// by hand: this macro generates both from one list, and adding an `Op` variant without
/// listing it is a non-exhaustive-match error.
macro_rules! op_codec {
    ($( $tag:literal $name:ident { $( $field:ident : $kind:ident, )* } , )*) => {
        fn put_op(w: &mut Writer, op: &Op) {
            match *op {
                $( Op::$name { $( $field, )* } => {
                    w.tag($tag);
                    $( put_scalar!(w, $kind, $field); )*
                } )*
            }
        }
        fn get_op(r: &mut Reader) -> Op {
            match r.tag() {
                $( $tag => Op::$name { $( $field: get_scalar!(r, $kind), )* }, )*
                other => unreachable_tag("Op", other),
            }
        }
    };
}

op_codec! {
    0 LoadK { dst: r, k: u32, },
    1 Move { dst: r, src: r, },
    2 MoveTake { dst: r, src: r, },
    3 LoadGlob { dst: r, idx: u32, },
    4 StoreGlob { idx: u32, src: r, },
    5 LoadCap { dst: r, idx: u16, },
    6 MakeCell { dst: r, src: r, },
    7 CellGet { dst: r, cell: r, },
    8 CellSet { cell: r, src: r, },
    9 LoadSelf { dst: r, },
    10 Bin { dst: r, a: r, b: r, op: binop, },
    11 BinInt { dst: r, a: r, b: r, op: binop, width: u8, },
    12 BinK { dst: r, a: r, k: u32, op: binop, },
    13 BinIntK { dst: r, a: r, k: u32, op: binop, width: u8, },
    14 Jump { to: u32, },
    15 JumpFalse { cond: r, to: u32, kind: cond, },
    16 MakeClosure { dst: r, chunk: u32, base: r, n: u16, },
    17 CallFn { dst: r, chunk: u32, base: r, argc: u16, },
    18 Call { dst: r, func: r, base: r, argc: u16, },
    19 TailCall { func: r, chunk: optchunk, base: r, argc: u16, },
    20 Ret { src: r, },
    21 MakeList { dst: r, base: r, n: u16, },
    22 MakeTuple { dst: r, base: r, n: u16, },
    23 ListPush { list: r, src: r, },
    24 MakeTag { dst: r, name: u16, base: r, n: u16, },
    25 MakeRecord { dst: r, name: u16, base: r, n: u16, },
    26 UpdateRecord { dst: r, obj: r, name: u16, base: r, n: u16, take: bool, },
    27 GetField { dst: r, obj: r, name: u16, },
    28 GetOptField { dst: r, obj: r, name: u16, },
    29 GetIndex { dst: r, obj: r, i: u16, },
    30 TestLit { obj: r, pat: u16, to: u32, },
    31 TestLitDyn { obj: r, pat: u16, to: u32, },
    32 TestStr { obj: r, pat: u16, base: r, to: u32, },
    33 TestTag { obj: r, name: u16, n: u16, to: u32, },
    34 TestTuple { obj: r, n: u16, to: u32, },
    35 TestRecord { obj: r, to: u32, },
    36 TestList { obj: r, n: u16, exact: bool, to: u32, },
    37 TestBool { cond: r, want: bool, to: u32, },
    38 NoMatch { obj: r, },
    39 GetPayload { dst: r, obj: r, i: u16, },
    40 GetFieldOr { dst: r, obj: r, name: u16, to: u32, },
    41 GetRest { dst: r, obj: r, name: u16, n: u16, },
    42 GetElem { dst: r, obj: r, i: u16, from_end: bool, },
    43 GetSlice { dst: r, obj: r, front: u16, back: u16, },
    44 MakeRange { dst: r, start: r, end: r, inclusive: bool, },
    45 CallBuiltin { dst: r, name: u16, base: r, argc: u16, },
    46 CallHost { dst: r, name: u16, base: r, argc: u16, },
    47 MakeBuiltin { dst: r, name: u16, },
    48 DispatchMethod { dst: r, name: u16, base: r, argc: u16, },
    49 Interp { dst: r, name: u16, base: r, n: u16, },
    50 BinDispatch { dst: r, a: r, b: r, op: binop, },
    51 Expect { cond: r, },
    52 TestExpect { cond: r, },
    53 Dbg { src: r, },
    54 Crash { src: r, },
    55 IterNext { dst: r, iter: r, idx: r, to: u32, },
    56 IterNextBack { dst: r, iter: r, idx: r, to: u32, },
}

fn binop_tag(op: crate::ast::BinOp) -> u8 {
    use crate::ast::BinOp::*;
    match op {
        Add => 0, Sub => 1, Mul => 2, Div => 3, IntDiv => 4, Rem => 5, Eq => 6,
        Ne => 7, Lt => 8, Le => 9, Gt => 10, Ge => 11, And => 12, Or => 13,
    }
}

fn binop_of(t: u8) -> crate::ast::BinOp {
    use crate::ast::BinOp::*;
    match t {
        0 => Add, 1 => Sub, 2 => Mul, 3 => Div, 4 => IntDiv, 5 => Rem, 6 => Eq,
        7 => Ne, 8 => Lt, 9 => Le, 10 => Gt, 11 => Ge, 12 => And, 13 => Or,
        other => unreachable_tag("BinOp", other),
    }
}

fn cond_tag(k: crate::vm::CondKind) -> u8 {
    use crate::vm::CondKind::*;
    match k { If => 0, Guard => 1, While => 2, Operand => 3 }
}

fn cond_of(t: u8) -> crate::vm::CondKind {
    use crate::vm::CondKind::*;
    match t {
        0 => If, 1 => Guard, 2 => While, 3 => Operand,
        other => unreachable_tag("CondKind", other),
    }
}

/// A chunk constant. Only these can be one: the compiler puts literals in `consts`, and
/// a closure, an iterator or a cell is made at run time and cannot be written down.
fn put_value(w: &mut Writer, v: &Value) {
    match v {
        Value::Unit => w.tag(0),
        Value::Missing => w.tag(1),
        Value::Bool(b) => { w.tag(2); w.bool(*b); }
        Value::Int(n) => { w.tag(3); w.i128(*n); }
        Value::Dec(n) => { w.tag(4); w.i128(*n); }
        Value::U128(n) => { w.tag(5); w.i128(*n as i128); }
        Value::Float(f) => { w.tag(6); w.f64(*f); }
        Value::F32(f) => { w.tag(7); w.f64(f64::from(*f)); }
        Value::Str(s) => { w.tag(8); w.s(s); }
        Value::Builtin(name, arity) => { w.tag(9); w.s(name); w.u(*arity as u64); }
        Value::List(items) => { w.tag(10); w.seq(items, |w, v| put_value(w, v)); }
        Value::Tuple(items) => { w.tag(11); w.seq(items, |w, v| put_value(w, v)); }
        Value::Record(fields) => {
            w.tag(12);
            w.seq(fields, |w, (n, v)| { w.s(n); put_value(w, v); });
        }
        Value::Tag(name, payload) => {
            w.tag(13);
            w.s(name);
            w.seq(payload, |w, v| put_value(w, v));
        }
        Value::Range { start, end, inclusive, step } => {
            w.tag(14);
            w.i128(*start);
            w.i128(*end);
            w.bool(*inclusive);
            w.i128(i128::from(*step));
        }
        Value::Simd { kind, bits } => { w.tag(15); w.u(u64::from(*kind)); w.i128(*bits as i128); }
        other => panic!("a {:?} cannot be a chunk constant", other),
    }
}

fn get_value(r: &mut Reader) -> Value {
    match r.tag() {
        0 => Value::Unit,
        1 => Value::Missing,
        2 => Value::Bool(r.bool()),
        3 => Value::Int(r.i128()),
        4 => Value::Dec(r.i128()),
        5 => Value::U128(r.i128() as u128),
        6 => Value::Float(r.f64()),
        7 => Value::F32(r.f64() as f32),
        8 => Value::Str(std::rc::Rc::from(r.s())),
        9 => { let name = r.s(); Value::Builtin(name, r.u() as usize) }
        10 => Value::list(r.seq(0, |r, _| get_value(r))),
        11 => Value::tuple(r.seq(0, |r, _| get_value(r))),
        12 => Value::record(r.seq(0, |r, _| (r.s(), get_value(r)))),
        13 => { let name = r.s(); Value::tag(name, r.seq(0, |r, _| get_value(r))) }
        14 => {
            let start = r.i128();
            let end = r.i128();
            let inclusive = r.bool();
            Value::Range { start, end, inclusive, step: r.i128() as i64 }
        }
        15 => { let kind = r.u() as u8; Value::Simd { kind, bits: r.i128() as u128 } }
        other => unreachable_tag("Value", other),
    }
}

fn put_shape(w: &mut Writer, shape: &crate::vm::NominalShape) {
    use crate::vm::NominalShape as S;
    match shape {
        S::Unknown => w.tag(0),
        S::Tuple(n) => { w.tag(1); w.u(*n as u64); }
        S::Simd(k) => { w.tag(2); w.u(u64::from(*k)); }
        S::Tags(names) => { w.tag(3); w.seq(names, |w, n| w.s(n)); }
        S::Fields(fields) => {
            w.tag(4);
            w.seq(fields, |w, (n, kind)| { w.s(n); w.tag(field_kind_tag(*kind)); });
        }
        S::Kind(kind) => { w.tag(5); w.tag(field_kind_tag(*kind)); }
    }
}

fn get_shape(r: &mut Reader) -> crate::vm::NominalShape {
    use crate::vm::NominalShape as S;
    match r.tag() {
        0 => S::Unknown,
        1 => S::Tuple(r.u() as usize),
        2 => S::Simd(r.u() as u8),
        3 => S::Tags(r.seq(0, |r, _| r.s().to_string())),
        4 => S::Fields(r.seq(0, |r, _| (r.s().to_string(), field_kind_of(r.tag())))),
        5 => S::Kind(field_kind_of(r.tag())),
        other => unreachable_tag("NominalShape", other),
    }
}

fn field_kind_tag(k: crate::vm::FieldKind) -> u8 {
    use crate::vm::FieldKind::*;
    match k {
        Any => 0, Int => 1, Float => 2, F32 => 3, Dec => 4, Str => 5,
        Bool => 6, List => 7, Record => 8, Tag => 9, Tuple => 10,
    }
}

fn field_kind_of(t: u8) -> crate::vm::FieldKind {
    use crate::vm::FieldKind::*;
    match t {
        0 => Any, 1 => Int, 2 => Float, 3 => F32, 4 => Dec, 5 => Str,
        6 => Bool, 7 => List, 8 => Record, 9 => Tag, 10 => Tuple,
        other => unreachable_tag("FieldKind", other),
    }
}

fn put_chunk(w: &mut Writer, chunk: &Chunk, node_base: u32, node_end: u32) {
    w.s(chunk.name);
    w.w16(chunk.n_regs);
    w.w16(chunk.arity);
    w.seq(&chunk.params, |w, p| w.s(p));
    w.seq(&chunk.names, |w, n| w.s(n));
    w.seq(&chunk.consts, |w, v| put_value(w, v));
    w.seq(&chunk.pats, |w, p| put_pattern(w, p));
    w.seq(&chunk.code, |w, op| put_op(w, op));
    // Spans are node ids, rebased on load exactly as the AST's are — but not every one
    // of them is a builtin's. A group's top level begins with the compiler pointing at
    // the APP's node, and that node means nothing in a prefix reused by another
    // program. Those are written as 0, "no location", which is the truth; everything
    // else is `1 + offset`. Getting this wrong is invisible in release — the
    // subtraction wraps and the span is quietly nonsense — and `tests/check_roc.sh` on
    // the DEBUG build is what caught it.
    w.seq(&chunk.spans, |w, id| {
        let at = id.index() as u32;
        if at < node_base || at >= node_end {
            w.w32(0);
        } else {
            w.w32(at - node_base + 1);
        }
    });
}

fn get_chunk(r: &mut Reader, id: u32, node_base: u32) -> Chunk {
    let name = r.s();
    let n_regs = r.r16();
    let arity = r.r16();
    let params: std::rc::Rc<[&'static str]> = r.seq(0, |r, _| r.s()).into();
    Chunk {
        name,
        n_regs,
        arity,
        bare: std::rc::Rc::new(crate::vm::Closure {
            chunk: id,
            params: std::rc::Rc::clone(&params),
            captures: Vec::new(),
        }),
        params,
        names: r.seq(0, |r, _| r.s()),
        consts: r.seq(0, |r, _| get_value(r)),
        pats: r.seq(0, |r, _| get_pattern(r)),
        code: r.seq(0, |r, _| get_op(r)),
        spans: r.seq(node_base, |r, base| match r.r32() {
            0 => crate::ast::fresh_node_unlocated(),
            at => NodeId(base + at - 1),
        }),
    }
}

// ---------------------------------------------------------------- members

/// One member, as `builtin::load` wants it back.
pub struct Member {
    pub name: &'static str,
    /// `None` when read by `member_tables`, which does not decode the tree.
    pub ast: Option<Expr>,
    pub intrinsics: Vec<&'static str>,
    pub signatures: Vec<(&'static str, Type)>,
    pub nominals: Vec<(&'static str, Type)>,
}

/// Write one member. `base` is the id of its first node; `offsets` is every node's
/// source offset, in id order, so loading can rebuild the table.
pub fn put_member(
    w: &mut Writer,
    name: &str,
    base: u32,
    offsets: &[u32],
    ast: &Expr,
    intrinsics: &[&'static str],
    signatures: &[(&'static str, Type)],
    nominals: &[(&'static str, Type)],
) {
    w.s(name);
    // A fixed-width length, patched once the body is written, so opening the artifact
    // can step over a member it was not asked for instead of decoding it. Without this
    // the header decoded all eight trees and threw them away — 0.7ms, most of what the
    // artifact saves.
    let length_at = w.out.len();
    w.out.extend_from_slice(&0u32.to_le_bytes());
    let body_at = w.out.len();
    // Ordered by who wants what, cheapest first, each expensive run behind a length so
    // it can be stepped over rather than decoded:
    //
    //   intrinsics | signatures | nominals | node offsets | tree
    //
    // Only `Dict` is ever asked for its SIGNATURES — `signatures_for` serves four
    // modules and `Set`'s must be re-parsed beside `Dict`'s — so the low-level
    // section's 153 and `Set`'s 32 were decoded into nothing. The TREE and the node
    // offsets are wanted only when there is no compiled prefix. What is left, and what
    // every run does read, is the intrinsics and the nominals.
    w.seq(intrinsics, |w, s| w.s(s));
    let sig_at = w.out.len();
    w.out.extend_from_slice(&0u32.to_le_bytes());
    let sig_from = w.out.len();
    w.seq(signatures, |w, (n, t)| {
        w.s(n);
        put_type(w, t);
    });
    let sig_len = (w.out.len() - sig_from) as u32;
    w.out[sig_at..sig_at + 4].copy_from_slice(&sig_len.to_le_bytes());
    w.seq(nominals, |w, (n, t)| {
        w.s(n);
        put_type(w, t);
    });
    w.u(offsets.len() as u64);
    for offset in offsets {
        w.u(u64::from(*offset));
    }
    put_expr(w, ast, base);
    let length = (w.out.len() - body_at) as u32;
    w.out[length_at..length_at + 4].copy_from_slice(&length.to_le_bytes());
}

/// The artifact, decoded far enough to find each member. Strings are resolved once,
/// members are decoded only when asked for.
pub struct Artifact {
    strings: Vec<&'static str>,
    /// Where each member's body starts, by name.
    members: Vec<(&'static str, usize)>,
    /// Where each compiled prefix's body starts, by selection key.
    prefixes: Vec<(&'static str, usize)>,
    blob: &'static [u8],
}

impl Artifact {
    /// Read the header: the string table, then each member's name and offset.
    ///
    /// The strings are slices of the blob, which lives in the binary — so this
    /// allocates one `Vec` of pointers and copies no text at all.
    pub fn open(blob: &'static [u8]) -> Option<Artifact> {
        if blob.len() < 20 || &blob[..8] != MAGIC {
            return None;
        }
        let count = u32::from_le_bytes(blob[16..20].try_into().ok()?) as usize;
        let mut head = Reader { blob, at: 20, strings: &[] };
        let n_strings = head.u() as usize;
        let mut lengths = Vec::with_capacity(n_strings);
        for _ in 0..n_strings {
            lengths.push(head.u() as usize);
        }
        // One validation of the whole run, then a slice per name.
        let total: usize = lengths.iter().sum();
        let text = std::str::from_utf8(blob.get(head.at..head.at + total)?).ok()?;
        let mut strings = Vec::with_capacity(n_strings);
        let mut at = 0;
        for len in lengths {
            strings.push(text.get(at..at + len)?);
            at += len;
        }
        let mut r = Reader { blob, at: head.at + total, strings: &strings };
        let mut members = Vec::with_capacity(count);
        for _ in 0..count {
            let name = r.s();
            let length = u32::from_le_bytes(blob.get(r.at..r.at + 4)?.try_into().ok()?) as usize;
            r.at += 4;
            // Where the member's own body begins. Opening the artifact reads NO tree:
            // it steps over each body by its length and decodes only what is asked for.
            members.push((name, r.at));
            r.at += length;
        }
        let n_prefixes = r.u() as usize;
        let mut prefixes = Vec::with_capacity(n_prefixes);
        for _ in 0..n_prefixes {
            let key = r.s();
            let length = u32::from_le_bytes(blob.get(r.at..r.at + 4)?.try_into().ok()?) as usize;
            r.at += 4;
            prefixes.push((key, r.at));
            r.at += length;
        }
        Some(Artifact { strings, members, prefixes, blob })
    }

    pub fn has(&self, name: &str) -> bool {
        self.members.iter().any(|(n, _)| *n == name)
    }

    /// Decode one member, installing its nodes in the table first so the ids in the
    /// tree can be rebased onto them.
    pub fn member(&self, name: &str) -> Option<Member> {
        let (found, at) = self.members.iter().find(|(n, _)| *n == name)?;
        let mut r = Reader { blob: self.blob, at: *at, strings: &self.strings };
        let intrinsics = r.seq(0, |r, _| r.s());
        let signatures = self.read_signatures(&mut r)?;
        let nominals = r.seq(0, |r, _| (r.s(), get_type(r)));
        let n_offsets = r.u() as usize;
        let mut offsets = Vec::with_capacity(n_offsets);
        for _ in 0..n_offsets {
            offsets.push(r.u() as u32);
        }
        let base = crate::ast::push_nodes(&offsets);
        Some(Member {
            name: found,
            intrinsics,
            signatures,
            nominals,
            ast: Some(get_expr(&mut r, base)),
        })
    }

    /// The tables every run needs, and only those: the intrinsic names and the declared
    /// nominals. Not the signatures, not the node offsets, not the tree.
    ///
    /// Signatures are asked for by module and only for four of them, so decoding all of
    /// a member's is decoding into nothing — see `signatures_of`, which reads one
    /// member's on demand.
    pub fn member_tables(&self, name: &str) -> Option<Member> {
        let (found, at) = self.members.iter().find(|(n, _)| *n == name)?;
        let mut r = Reader { blob: self.blob, at: *at, strings: &self.strings };
        let intrinsics = r.seq(0, |r, _| r.s());
        self.skip_signatures(&mut r)?;
        Some(Member {
            name: found,
            intrinsics,
            signatures: Vec::new(),
            nominals: r.seq(0, |r, _| (r.s(), get_type(r))),
            ast: None,
        })
    }

    /// One member's declared signatures, decoded on demand.
    pub fn signatures_of(&self, name: &str) -> Option<Vec<(&'static str, Type)>> {
        let (_, at) = self.members.iter().find(|(n, _)| *n == name)?;
        let mut r = Reader { blob: self.blob, at: *at, strings: &self.strings };
        let _ = r.seq(0, |r, _| r.s());
        self.read_signatures(&mut r)
    }

    fn read_signatures(&self, r: &mut Reader) -> Option<Vec<(&'static str, Type)>> {
        r.at += 4;
        Some(r.seq(0, |r, _| (r.s(), get_type(r))))
    }

    fn skip_signatures(&self, r: &mut Reader) -> Option<()> {
        let len = u32::from_le_bytes(self.blob.get(r.at..r.at + 4)?.try_into().ok()?) as usize;
        r.at += 4 + len;
        Some(())
    }
}


// ---------------------------------------------------------------- the compiled prefix

/// `Builtin.roc` COMPILED, for one selection of members.
///
/// The measurement that made this worth writing: with chunk numbers normalised, every
/// named builtin chunk is byte-identical across four different programs — including one
/// defining nominal operator methods, which flips a program-wide compiler switch. The
/// builtin bytecode does not depend on the program that loads it. Only the NUMBERING
/// did, because nested lambdas inside builtin members were numbered after every
/// top-level binding, and a user program contributes some.
///
/// So the builtins take a stable id PREFIX — chunk 0 is their top level, then their
/// functions, then their nested lambdas — and a program's own chunks are numbered after
/// it. Nothing is renumbered on load; the prefix keeps the ids it was compiled with.
pub struct Prefix {
    pub chunks: Vec<Chunk>,
    /// Global slot names, in slot order. A program's own globals follow them.
    pub globals: Vec<&'static str>,
    /// Top-level functions: name, chunk, arity.
    pub fns: Vec<(&'static str, u32, u16)>,
    pub methods: Vec<((&'static str, &'static str), u32)>,
    pub methods_by_name: Vec<(&'static str, Vec<(&'static str, u32)>)>,
    pub nominal_shapes: Vec<(&'static str, crate::vm::NominalShape)>,
    pub literal_coercions: Vec<(&'static str, Value)>,
}

#[allow(clippy::too_many_arguments)]
pub fn put_prefix(
    w: &mut Writer,
    key: &str,
    node_base: u32,
    node_end: u32,
    offsets: &[u32],
    program: &crate::vm::Program,
    group_chunks: usize,
    globals: &[&'static str],
    fns: &[(&'static str, u32, u16)],
) {
    w.s(key);
    let length_at = w.out.len();
    w.out.extend_from_slice(&0u32.to_le_bytes());
    let body_at = w.out.len();
    w.u(offsets.len() as u64);
    for offset in offsets {
        w.u(u64::from(*offset));
    }
    w.seq(&program.chunks[..group_chunks], |w, chunk| put_chunk(w, chunk, node_base, node_end));
    w.seq(globals, |w, g| w.s(g));
    w.seq(fns, |w, (n, c, a)| {
        w.s(n);
        w.u(u64::from(*c));
        w.u(u64::from(*a));
    });
    // SORTED, all three: they are `HashMap`s, and iteration order varies per process —
    // which made the artifact differ byte for byte between two runs that produced the
    // same program. `tests/check_artifact.sh` is the gate that caught it.
    let mut methods: Vec<_> = program.methods.iter().collect();
    methods.sort_by_key(|(k, _)| *k);
    w.seq(&methods, |w, ((module, method), chunk)| {
        w.s(module);
        w.s(method);
        w.u(u64::from(**chunk));
    });
    let mut by_name: Vec<_> = program.methods_by_name.iter().collect();
    by_name.sort_by_key(|(k, _)| *k);
    w.seq(&by_name, |w, (method, defined)| {
        w.s(method);
        w.seq(defined, |w, (name, chunk)| {
            w.s(name);
            w.u(u64::from(*chunk));
        });
    });
    let mut shapes: Vec<_> = program.nominal_shapes.iter().collect();
    shapes.sort_by_key(|(k, _)| *k);
    w.seq(&shapes, |w, (name, shape)| {
        w.s(name);
        put_shape(w, shape);
    });
    w.seq(&program.literal_coercions, |w, (name, value)| {
        w.s(name);
        put_value(w, value);
    });
    let length = (w.out.len() - body_at) as u32;
    w.out[length_at..length_at + 4].copy_from_slice(&length.to_le_bytes());
}

impl Artifact {
    /// The compiled prefix for a selection, if the artifact carries one.
    pub fn prefix(&self, key: &str) -> Option<Prefix> {
        let (_, at) = self.prefixes.iter().find(|(n, _)| *n == key)?;
        let mut r = Reader { blob: self.blob, at: *at, strings: &self.strings };
        let n_offsets = r.u() as usize;
        let mut offsets = Vec::with_capacity(n_offsets);
        for _ in 0..n_offsets {
            offsets.push(r.u() as u32);
        }
        // The spans in these chunks point at builtin source, so the nodes go in first
        // and the ids are rebased onto them — the same move the trees make.
        let base = crate::ast::push_nodes(&offsets);
        let mut id = 0u32;
        let chunks = {
            let n = r.u() as usize;
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                out.push(get_chunk(&mut r, id, base));
                id += 1;
            }
            out
        };
        Some(Prefix {
            chunks,
            globals: r.seq(0, |r, _| r.s()),
            fns: r.seq(0, |r, _| (r.s(), r.u() as u32, r.u() as u16)),
            methods: r.seq(0, |r, _| ((r.s(), r.s()), r.u() as u32)),
            methods_by_name: r
                .seq(0, |r, _| (r.s(), r.seq(0, |r, _| (r.s(), r.u() as u32)))),
            nominal_shapes: r.seq(0, |r, _| (r.s(), get_shape(r))),
            literal_coercions: r.seq(0, |r, _| (r.s(), get_value(r))),
        })
    }

    pub fn has_prefix(&self, key: &str) -> bool {
        self.prefixes.iter().any(|(n, _)| *n == key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one property that matters: what comes out is what went in. `Debug` is the
    /// comparison because `Expr` has no `PartialEq`, and it prints every field.
    #[test]
    fn a_member_round_trips() {
        let members = crate::builtin::parse_members(&["Box"]).expect("parse Box");
        let (loaded, base, offsets) = &members[0];
        let mut w = Writer::default();
        put_member(
            &mut w,
            loaded.name,
            *base,
            offsets,
            &loaded.ast,
            &loaded.intrinsics,
            &loaded.signatures,
            &loaded.nominals,
        );
        // No compiled prefixes in this one, but the section still has to be there.
        w.count(0);
        let blob: &'static [u8] = Box::leak(w.finish(0, 1).into_boxed_slice());
        let artifact = Artifact::open(blob).expect("open");
        let back = artifact.member("Box").expect("member");

        assert_eq!(back.intrinsics, loaded.intrinsics);
        assert_eq!(format!("{:?}", back.signatures), format!("{:?}", loaded.signatures));
        assert_eq!(format!("{:?}", back.nominals), format!("{:?}", loaded.nominals));

        // Every id moved by the same amount — that IS the rebase — so the trees are
        // compared with the numbers taken out, and the ids are checked separately by
        // what they point at.
        let before = blank_ids(&format!("{:?}", loaded.ast));
        let after = blank_ids(&format!("{:?}", back.ast.expect("the tree")));
        assert_eq!(before, after, "round trip changed the tree");

        // The tables come back without the tree — and without the signatures, which
        // are read by module on demand and are the bulk of what a member declares.
        let tables = artifact.member_tables("Box").expect("tables");
        assert!(tables.ast.is_none(), "member_tables decoded the tree");
        assert!(tables.signatures.is_empty(), "member_tables decoded the signatures");
        assert_eq!(tables.intrinsics, loaded.intrinsics);
        assert_eq!(format!("{:?}", tables.nominals), format!("{:?}", loaded.nominals));
        // And asking for them by name gets exactly what went in.
        let signatures = artifact.signatures_of("Box").expect("signatures");
        assert_eq!(format!("{:?}", signatures), format!("{:?}", loaded.signatures));
    }

    /// `NodeId(17)` -> `NodeId(_)`, so a rebased tree prints the same as its original.
    fn blank_ids(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(at) = rest.find("NodeId(") {
            out.push_str(&rest[..at + "NodeId(".len()]);
            rest = &rest[at + "NodeId(".len()..];
            let end = rest.find(')').unwrap_or(rest.len());
            out.push('_');
            rest = &rest[end..];
        }
        out.push_str(rest);
        out
    }

    /// The ids are only worth anything if they still point at the right offsets.
    #[test]
    fn a_members_node_offsets_survive() {
        let members = crate::builtin::parse_members(&["Box"]).expect("parse Box");
        let (loaded, base, offsets) = &members[0];
        let mut w = Writer::default();
        put_member(
            &mut w,
            loaded.name,
            *base,
            offsets,
            &loaded.ast,
            &loaded.intrinsics,
            &loaded.signatures,
            &loaded.nominals,
        );
        w.count(0);
        let blob: &'static [u8] = Box::leak(w.finish(0, 1).into_boxed_slice());
        let artifact = Artifact::open(blob).expect("open");
        let before = crate::ast::node_count() as u32;
        let _ = artifact.member("Box").expect("member");
        let after = crate::ast::node_count() as u32;
        assert_eq!((after - before) as usize, offsets.len(), "wrong number of nodes installed");
        assert_eq!(&crate::ast::offsets_between(before, after), offsets, "offsets differ");
    }
}
