//! **Roc to Codex.** A checked Roc program, written out as Codex: one chapter per
//! module, and the app's chapter holding its `opening`.
//!
//! The subject is Roc that `rocemit` (rust-codex-compiler) wrote from Codex, so the
//! round trip can be judged by running the Codex this writes with `codexrun` against
//! the output the original program was captured with. Two things make that Roc
//! readable as Codex:
//!
//! - **A Codex `Text` is a Roc `Text :: List(U8)`**, a type of its own, so the
//!   checker's types say where a Codex `Text` was. The `Text` module is Codex's own
//!   text, not a chapter: `Text.concat` is `&`, `Text.show_int` is `show`, and a
//!   string literal typed `Text` is a Codex text literal.
//! - **rocemit writes a Codex builtin as a Roc idiom**, and those are read back as
//!   the builtin: `U64.to_i64_wrap(List.len(xs))` is `list-length xs`. The idioms
//!   are rocemit's builtin table (`roc_emit.rs`, `fn builtin`) run backwards.
//!
//! What it does not know how to write it refuses, naming the form, the way rocemit
//! refuses: a refusal is a line in the ledger, never a guess.
//!
//! The output is a RESOLVED unit, which `codexrun` runs from anywhere: the Foreword
//! chapters every program cites (`ListUtils`, `Tuple`) and `Console`, copied from a
//! Cobblestone checkout, then this program's chapters as `Roc--Name`.

use std::collections::HashMap;

use crate::ast::{BinOp, Expr, MatchArm, NodeId, Pattern};
use crate::types::Type;

/// One Roc module: its name, its top level, and the types it declared.
pub struct Module<'a> {
    pub name: String,
    pub ast: &'a Expr,
    pub types: Vec<(&'static str, Type)>,
}

/// Everything the emitter reads: the app and its modules, with the checker's types.
pub struct Input<'a> {
    /// The app's chapter name.
    pub app_name: String,
    pub app: &'a Expr,
    pub app_types: Vec<(&'static str, Type)>,
    pub modules: Vec<Module<'a>>,
    /// Each node's type, from `TypeChecker::node_types`.
    pub types: HashMap<NodeId, Type>,
    /// A Cobblestone checkout's `codex/foreword/core`, for the carried chapters.
    pub foreword: std::path::PathBuf,
}

/// The module that is Codex's own text, and the Foreword chapters every unit carries.
const TEXT_MODULE: &str = "Text";
const CARRIED: [&str; 3] = ["ListUtils", "Tuple", "Console"];

/// The whole program as one resolved Codex unit.
pub fn emit(input: &Input) -> Result<String, String> {
    let mut records = Vec::new();
    for (name, ty) in input.modules.iter().flat_map(|m| m.types.iter()).chain(input.app_types.iter()) {
        if let Type::Record { fields, .. } = ty {
            records.push((*name, field_names(fields)));
        }
    }
    let cx = Cx { types: &input.types, records };

    let mut out = String::new();
    for chapter in CARRIED {
        let path = input.foreword.join(format!("{}.codex", chapter));
        let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
        let text = text.replacen(&format!("Chapter: {}", chapter), &format!("Chapter: Foreword--{}", chapter), 1);
        out.push_str(text.trim_end());
        out.push_str("\n\n");
    }
    let names: Vec<&str> = input.modules.iter().map(|m| m.name.as_str()).filter(|n| *n != TEXT_MODULE).collect();
    for module in &input.modules {
        if module.name == TEXT_MODULE {
            continue;
        }
        out.push_str(&cx.chapter(&module.name, module.ast, &module.types, &names)?);
    }
    out.push_str(&cx.chapter(&input.app_name, input.app, &input.app_types, &names)?);
    Ok(out)
}

fn field_names(fields: &[(&'static str, Type)]) -> Vec<&'static str> {
    let mut names: Vec<&'static str> = fields.iter().map(|(n, _)| *n).collect();
    names.sort_unstable();
    names
}

/// A Roc name as Codex spells it: rocemit wrote `is-even` as `is_even`.
fn kebab(name: &str) -> String {
    name.trim_end_matches('!').replace('_', "-")
}

/// The last segment of a qualified name: Codex names a cited chapter's definitions
/// bare.
fn bare(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

struct Cx<'a> {
    types: &'a HashMap<NodeId, Type>,
    /// Each declared record type, by its sorted field names: a record literal is
    /// anonymous in Roc and named in Codex.
    records: Vec<(&'static str, Vec<&'static str>)>,
}

/// The expression's precedence as an operand: an atom needs no parentheses.
fn atom(s: &str) -> bool {
    let s = s.trim();
    let bracketed = |open: char, close: char| {
        s.starts_with(open) && s.ends_with(close) && {
            let mut depth = 0i32;
            let mut closed_early = false;
            for (i, c) in s.char_indices() {
                if c == open { depth += 1 }
                if c == close { depth -= 1 }
                if depth == 0 && i + c.len_utf8() < s.len() { closed_early = true; break }
            }
            !closed_early
        }
    };
    !s.contains(' ') || bracketed('(', ')') || bracketed('[', ']') || (s.starts_with('"') && s.ends_with('"') && !s[1..s.len() - 1].contains('"'))
}

fn paren(s: String) -> String {
    if atom(&s) { s } else { format!("({})", s) }
}

impl Cx<'_> {
    fn ty_of(&self, e: &Expr) -> Option<&Type> {
        self.types.get(&e.id())
    }

    fn is_text(&self, e: &Expr) -> bool {
        matches!(self.ty_of(e), Some(Type::Nominal { name, .. }) if bare(name) == TEXT_MODULE)
    }

    fn chapter(&self, name: &str, ast: &Expr, types: &[(&'static str, Type)], all: &[&str]) -> Result<String, String> {
        let mut out = format!("Chapter: Roc--{}\n  cites Foreword chapter Console\n", name);
        for other in all {
            if *other != name {
                out.push_str(&format!("  cites Roc chapter {}\n", other));
            }
        }
        out.push_str("\nSection: Types\n\n");
        for (tname, ty) in types {
            if let Some(decl) = self.type_decl(tname, ty)? {
                out.push_str(&decl);
                out.push('\n');
            }
        }
        out.push_str("Section: Definitions\n\n");
        let mut cursor = ast;
        while let Expr::Let { name, annotation, value, body, .. } = cursor {
            if *name != "_" && *name != "line!" {
                out.push_str(&self.def(name, annotation.as_ref(), value)?);
                out.push('\n');
            }
            cursor = body;
        }
        out.push_str("Page 1\n\n");
        Ok(out)
    }

    /// A type declaration: a record, or a union as a sum type. A module's own
    /// namespace (`Maybe :: []`) is no type.
    fn type_decl(&self, name: &str, ty: &Type) -> Result<Option<String>, String> {
        let name = bare(name);
        Ok(match ty {
            Type::Record { fields, .. } => {
                let mut out = format!("  {} = record {{\n", name);
                let n = fields.len();
                for (i, (f, t)) in fields.iter().enumerate() {
                    out.push_str(&format!("    {} : {}{}\n", kebab(f), self.ty(t)?, if i + 1 < n { "," } else { "" }));
                }
                out.push_str("  }\n");
                Some(out)
            }
            Type::TagUnion { tags, .. } if tags.is_empty() => None,
            Type::TagUnion { tags, .. } => {
                let mut out = format!("  {} =\n", name);
                for (tag, payload) in tags {
                    out.push_str(&format!("    | {}", tag));
                    for p in payload {
                        out.push_str(&format!(" ({})", self.ty(p)?));
                    }
                    out.push('\n');
                }
                Some(out)
            }
            Type::Nominal { backing, .. } => return self.type_decl(name, backing),
            other => return Err(format!("type declaration `{}` of {}", name, other)),
        })
    }

    fn ty(&self, t: &Type) -> Result<String, String> {
        Ok(match t {
            Type::I64 => "Integer".into(),
            Type::F64 => "Number".into(),
            Type::Bool => "Boolean".into(),
            Type::Unit => "Nothing".into(),
            Type::List(e) => format!("List {}", paren(self.ty(e)?)),
            Type::Nominal { name, .. } if bare(name) == TEXT_MODULE => "Text".into(),
            Type::Nominal { name, .. } => bare(name).to_string(),
            Type::Record { fields, .. } => match self.record_name(fields) {
                Some(n) => n.to_string(),
                None => return Err(format!("an anonymous record type {}", t)),
            },
            Type::TypeVar(v) => format!("t{}", v),
            Type::Tuple(items) => {
                let xs: Result<Vec<String>, String> = items.iter().map(|i| self.ty(i)).collect();
                format!("({})", xs?.join(", "))
            }
            Type::Function(..) => {
                let mut params = Vec::new();
                let mut cur = t;
                while let Type::Function(a, b) = cur {
                    params.push(self.ty(a)?);
                    cur = b;
                }
                format!("({} -> {})", params.join(", "), self.ty(cur)?)
            }
            other => return Err(format!("type {}", other)),
        })
    }

    fn record_name(&self, fields: &[(&'static str, Type)]) -> Option<&'static str> {
        let want = field_names(fields);
        let mut found = self.records.iter().filter(|(_, fs)| *fs == want).map(|(n, _)| *n);
        let first = found.next()?;
        found.next().is_none().then(|| bare(first)).map(|s| crate::memory::string_pool::intern(s))
    }

    /// A top-level definition: its signature, then its equation.
    fn def(&self, name: &str, annotation: Option<&Type>, value: &Expr) -> Result<String, String> {
        if name == "main!" {
            return self.opening(value);
        }
        let cname = kebab(bare(name));
        let Some(sig) = annotation else {
            return Err(format!("`{}` has no annotation", name));
        };
        match value {
            Expr::Lambda { params, body, .. } => {
                let (ps, result) = peel(sig, params.len()).ok_or_else(|| format!("`{}`'s annotation has fewer parameters than its lambda", name))?;
                let ps: Result<Vec<String>, String> = ps.iter().map(|p| self.ty(p)).collect();
                let heads: Vec<String> = params.iter().map(|p| format!("({})", kebab(p))).collect();
                Ok(format!(
                    "  {} : {} -> {}\n  {} {} =\n    {}\n",
                    cname,
                    ps?.join(", "),
                    self.ty(result)?,
                    cname,
                    heads.join(" "),
                    indent(&self.expr(body)?, 4)
                ))
            }
            _ => Ok(format!("  {} : {}\n  {} =\n    {}\n", cname, self.ty(sig)?, cname, indent(&self.expr(value)?, 4))),
        }
    }

    /// `main!` as the chapter's opening: its statements in an `act` block.
    fn opening(&self, value: &Expr) -> Result<String, String> {
        let Expr::Lambda { body, .. } = value else {
            return Err("`main!` is not a function".into());
        };
        Ok(format!("  opening : [Console] Nothing = act\n{}  end\n", self.statements(body, 4)?))
    }

    /// An effectful body as `act` lines. `let _ = s in rest` is the statement `s`;
    /// `let x = v in rest` scopes `x` over a nested `act`, as Codex does.
    fn statements(&self, e: &Expr, ind: usize) -> Result<String, String> {
        let pad = " ".repeat(ind);
        let mut out = String::new();
        let mut cursor = e;
        loop {
            match cursor {
                Expr::Let { name, value, body, .. } if *name == "_" => {
                    out.push_str(&self.statement(value, ind)?);
                    cursor = body;
                }
                Expr::Let { name, value, body, .. } => {
                    out.push_str(&format!("{}let {} = {}\n{}in act\n", pad, kebab(name), indent(&self.expr(value)?, ind + 2), pad));
                    out.push_str(&self.statements(body, ind + 2)?);
                    out.push_str(&format!("{}end\n", pad));
                    return Ok(out);
                }
                // The program's answer: nothing to say in Codex.
                Expr::Tag { name, .. } if *name == "Ok" => return Ok(out),
                other => {
                    out.push_str(&self.statement(other, ind)?);
                    return Ok(out);
                }
            }
        }
    }

    fn statement(&self, e: &Expr, ind: usize) -> Result<String, String> {
        let pad = " ".repeat(ind);
        // `line!(Text.printed(t))` is Codex's print-line-uni.
        if let Some([arg]) = call_of(e, "line!") {
            if let Some([t]) = call_of(arg, "Text.printed") {
                return Ok(format!("{}print-line-uni {}\n", pad, paren(self.expr(t)?)));
            }
        }
        match e {
            // A block of statements.
            Expr::Let { .. } => self.statements(e, ind),
            other => Err(format!("a statement {}", short(other))),
        }
    }

    fn expr(&self, e: &Expr) -> Result<String, String> {
        if let Some(s) = self.idiom(e)? {
            return Ok(s);
        }
        Ok(match e {
            Expr::Int(n, _) if *n < 0 => format!("({})", n),
            Expr::Int(n, _) => n.to_string(),
            Expr::Str(s, _) if self.is_text(e) => text_literal(s),
            Expr::Str(s, _) => return Err(format!("a Str literal {:?} that is not a Text", s)),
            Expr::Bool(b, _) => if *b { "True".into() } else { "False".into() },
            Expr::Ident(n, _) => kebab(n),
            Expr::Qualified { name, .. } => kebab(name),
            Expr::BinOp { left, op, right, .. } => self.binop(left, *op, right)?,
            Expr::Call { func, args, .. } => {
                let f = match &**func {
                    Expr::Ident(n, _) => kebab(n),
                    Expr::Qualified { module, name, .. } if *module != TEXT_MODULE => kebab(name),
                    other => return Err(format!("a call to {}", short(other))),
                };
                let mut out = f;
                for a in args {
                    out.push(' ');
                    out.push_str(&paren(self.expr(a)?));
                }
                out
            }
            Expr::If { condition, then_branch, otherwise, .. } => format!(
                "if {} then {} else {}",
                self.expr(condition)?,
                paren(self.expr(then_branch)?),
                paren(self.expr(otherwise)?)
            ),
            Expr::Let { name, value, body, .. } if *name != "_" => format!(
                "let {} = {}\nin {}",
                kebab(name),
                self.expr(value)?,
                self.expr(body)?
            ),
            Expr::Match { scrutinee, arms, .. } => self.when(scrutinee, arms)?,
            Expr::List(items, _) => {
                let xs: Result<Vec<String>, String> = items.iter().map(|i| self.expr(i)).collect();
                format!("[{}]", xs?.join(", "))
            }
            Expr::Record(fields, _) => {
                let ty = self.ty_of(e).cloned();
                let Some(Type::Record { fields: tf, .. }) = ty.as_ref().map(strip) else {
                    return Err("a record literal of no record type".into());
                };
                let rname = self.record_name(tf).ok_or("a record literal of no declared record type")?;
                let fs: Result<Vec<String>, String> = fields.iter().map(|(f, v)| Ok(format!("{} = {}", kebab(f), self.expr(v)?))).collect();
                format!("{} {{ {} }}", rname, fs?.join(", "))
            }
            Expr::FieldAccess { record, field, .. } => format!("{}.{}", paren(self.expr(record)?), kebab(field)),
            Expr::Tag { name, args, .. } => {
                let mut out = name.to_string();
                for a in args {
                    out.push(' ');
                    out.push_str(&paren(self.expr(a)?));
                }
                out
            }
            Expr::Lambda { params, body, .. } => {
                let ps: Vec<String> = params.iter().map(|p| kebab(p)).collect();
                format!("(\\{} -> {})", ps.join(" "), self.expr(body)?)
            }
            other => return Err(format!("an expression {}", short(other))),
        })
    }

    fn binop(&self, l: &Expr, op: BinOp, r: &Expr) -> Result<String, String> {
        let sym = match op {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div | BinOp::IntDiv => "/",
            BinOp::Eq => "==",
            BinOp::Ne => "/=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::And => "and",
            BinOp::Or => "or",
            BinOp::Rem => return Ok(format!("int-rem {} {}", paren(self.expr(l)?), paren(self.expr(r)?))),
        };
        Ok(format!("{} {} {}", paren(self.expr(l)?), sym, paren(self.expr(r)?)))
    }

    fn when(&self, scrutinee: &Expr, arms: &[MatchArm]) -> Result<String, String> {
        let mut out = format!("when {}", self.expr(scrutinee)?);
        for arm in arms {
            if arm.guard.is_some() {
                return Err("a match arm with a guard".into());
            }
            for p in &arm.patterns {
                out.push_str(&format!("\n  is {} -> {}", self.pattern(p, scrutinee)?, indent(&self.expr(&arm.body)?, 4)));
            }
        }
        Ok(out)
    }

    fn pattern(&self, p: &Pattern, scrutinee: &Expr) -> Result<String, String> {
        Ok(match p {
            Pattern::Wildcard => "otherwise".into(),
            Pattern::Binding(n) => kebab(n),
            Pattern::Int(n) if *n < 0 => format!("({})", n),
            Pattern::Int(n) => n.to_string(),
            Pattern::Str(s) if self.is_text(scrutinee) => text_literal(s),
            Pattern::Tag { name, args } => {
                let mut out = name.to_string();
                for a in args {
                    out.push_str(&format!(" ({})", self.pattern(a, scrutinee)?));
                }
                out
            }
            Pattern::Tuple(items) => {
                let xs: Result<Vec<String>, String> = items.iter().map(|i| self.pattern(i, scrutinee)).collect();
                format!("({})", xs?.join(", "))
            }
            other => return Err(format!("a pattern {}", other)),
        })
    }

    /// rocemit's idioms, read back as the Codex they were written from.
    fn idiom(&self, e: &Expr) -> Result<Option<String>, String> {
        // `Text.concat(a, b)` is `&`.
        if let Some([a, b]) = call_of(e, "Text.concat") {
            return Ok(Some(format!("{} & {}", self.expr(a)?, paren(self.expr(b)?))));
        }
        if let Some([n]) = call_of(e, "Text.show_int") {
            return Ok(Some(format!("show {}", paren(self.expr(n)?))));
        }
        if let Some([t]) = call_of(e, "Text.len") {
            return Ok(Some(format!("text-length {}", paren(self.expr(t)?))));
        }
        // `U64.to_i64_wrap(List.len(xs))` is `list-length xs`.
        if let Some([inner]) = call_of(e, "U64.to_i64_wrap") {
            if let Some([xs]) = call_of(inner, "List.len") {
                return Ok(Some(format!("list-length {}", paren(self.expr(xs)?))));
            }
        }
        if let Some([a, b]) = call_of(e, "List.concat") {
            return Ok(Some(format!("{} & {}", self.expr(a)?, paren(self.expr(b)?))));
        }
        if let Some([xs, x]) = call_of(e, "List.append") {
            return Ok(Some(format!("list-snoc {} {}", paren(self.expr(xs)?), paren(self.expr(x)?))));
        }
        for (roc, codex) in [("I64.min", "min"), ("I64.max", "max"), ("I64.rem_by", "int-rem"), ("Prelude.int_mod", "int-mod")] {
            if let Some([a, b]) = call_of(e, roc) {
                return Ok(Some(format!("{} {} {}", codex, paren(self.expr(a)?), paren(self.expr(b)?))));
            }
        }
        // `-x` arrives as `x.negate()`.
        if let Expr::Dispatch { receiver, method: "negate", args, .. } = e {
            if args.is_empty() {
                return Ok(Some(format!("(-{})", paren(self.expr(receiver)?))));
            }
        }
        // `-x` is `0 - x` in Roc's AST and in Codex's output alike, but a literal
        // reads better negated.
        if let Expr::BinOp { left, op: BinOp::Sub, right, .. } = e {
            if matches!(**left, Expr::Int(0, _)) {
                if let Expr::Int(n, _) = **right {
                    return Ok(Some(format!("(-{})", n)));
                }
            }
        }
        Ok(None)
    }
}

/// The arguments of a call to `name` (`Module.fn` or a bare name), if `e` is one.
fn call_of<'e>(e: &'e Expr, name: &str) -> Option<&'e [Expr]> {
    let Expr::Call { func, args, .. } = e else { return None };
    let matches = match &**func {
        Expr::Ident(n, _) => *n == name,
        Expr::Qualified { module, name: n, .. } => name.split_once('.') == Some((module, n)),
        _ => false,
    };
    matches.then_some(args.as_slice())
}

/// The first `n` parameters of a curried function type, and what is left.
fn peel(t: &Type, n: usize) -> Option<(Vec<&Type>, &Type)> {
    let mut params = Vec::new();
    let mut cur = t;
    while params.len() < n {
        let Type::Function(a, b) = cur else { return None };
        params.push(&**a);
        cur = b;
    }
    Some((params, cur))
}

fn strip(t: &Type) -> &Type {
    match t {
        Type::Nominal { backing, .. } if !matches!(**backing, Type::TypeVar(_)) => strip(backing),
        other => other,
    }
}

/// A Codex text literal. Codex escapes as Roc does for the characters rocemit
/// escaped: backslash, quote, newline, tab.
fn text_literal(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn indent(s: &str, n: usize) -> String {
    s.replace('\n', &format!("\n{}", " ".repeat(n)))
}

/// A short rendering of an expression, for a refusal.
fn short(e: &Expr) -> String {
    let s = e.to_string();
    if s.len() > 80 { format!("{}...", &s[..s.char_indices().nth(77).map_or(s.len(), |(i, _)| i)]) } else { s }
}
