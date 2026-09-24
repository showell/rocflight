//! **Roc to Codex.** A checked Roc program, written out as Codex: one chapter per
//! module, and the app's chapter holding its `opening`.
//!
//! The subject is Roc that `rocemit` (rust-codex-compiler) wrote from Codex, so the
//! round trip can be judged by running the Codex this writes with `codexrun` against
//! the output the original program was captured with. Two things make that Roc
//! readable as Codex:
//!
//! - **A Codex `Text` is a Roc `CceText :: List(U8)`, and a Codex `Char` a
//!   `CceChar :: I64`**, types of their own, so the checker's types say where each
//!   was. Their modules are Codex's own text, not chapters: `CceText.concat` is `&`,
//!   `CceText.show_int` is `show`, `CceChar.of_code(15)` is `'a'`, and a string
//!   literal is a Codex text literal.
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
const TEXT_MODULE: &str = "CceText";
const CHAR_MODULE: &str = "CceChar";
/// rocemit's own helpers: each stands for a Codex builtin or operator.
const PRELUDE_MODULE: &str = "Prelude";
const CARRIED: [&str; 3] = ["ListUtils", "Tuple", "Console"];

/// Roc's wrapping arithmetic in Codex. A plain `Integer` passes for an
/// `Integer wrapping`, and arithmetic on one wraps where a plain one traps
/// (codex/test/ops/int-wrapping-spelling).
const WRAP_CHAPTER: &str = "Chapter: Roc--Wrap

Section: Wrapping arithmetic

  roc-plus-wrap : Integer wrapping, Integer -> Integer
  roc-plus-wrap (a) (b) = a + b

  roc-minus-wrap : Integer wrapping, Integer -> Integer
  roc-minus-wrap (a) (b) = a - b

  roc-times-wrap : Integer wrapping, Integer -> Integer
  roc-times-wrap (a) (b) = a * b

Page 1

";

/// Roc's own modules. A call into one is either an idiom this reads back, or a
/// refusal: its bare name means nothing in Codex.
const ROC_BUILTIN_MODULES: [&str; 17] = [
    "I8", "I16", "I32", "I64", "I128", "U8", "U16", "U32", "U64", "U128", "F32", "F64", "Dec", "List", "Str", "Bool", "Prelude",
];

/// The whole program as one resolved Codex unit.
pub fn emit(input: &Input) -> Result<String, String> {
    let mut records = Vec::new();
    let mut unions = Vec::new();
    for (name, ty) in input.modules.iter().flat_map(|m| m.types.iter()).chain(input.app_types.iter()) {
        match strip(ty) {
            Type::Record { fields, .. } => records.push((*name, field_names(fields))),
            Type::TagUnion { tags, .. } if !tags.is_empty() => unions.push((*name, tags.clone(), params_of(tags))),
            _ => {}
        }
    }
    // What each code 0..127 names, from the program's own `CceText.points`: a
    // `CceChar.of_code(15)` is the literal `'a'`.
    let points = input
        .modules
        .iter()
        .find(|m| m.name == TEXT_MODULE)
        .and_then(|m| list_def(m.ast, "CceText.points"))
        .unwrap_or_default();
    let cx = Cx { types: &input.types, records, unions, points };

    let mut out = String::new();
    for chapter in CARRIED {
        let path = input.foreword.join(format!("{}.codex", chapter));
        let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
        let text = text.replacen(&format!("Chapter: {}", chapter), &format!("Chapter: Foreword--{}", chapter), 1);
        out.push_str(text.trim_end());
        out.push_str("\n\n");
    }
    // A program that pokes raw memory is emitted over rocemit's `Mem`, threaded
    // through every function that reaches it: a whole-program rewrite, not an
    // idiom, and not undone here.
    if input.modules.iter().any(|m| m.name == "Mem") {
        return Err("the program threads rocemit's machine memory (Mem)".into());
    }
    out.push_str(WRAP_CHAPTER);
    let mut names: Vec<&str> = input.modules.iter().map(|m| m.name.as_str()).filter(|n| ![TEXT_MODULE, CHAR_MODULE, PRELUDE_MODULE].contains(n)).collect();
    names.push("Wrap");
    for module in &input.modules {
        if [TEXT_MODULE, CHAR_MODULE, PRELUDE_MODULE].contains(&module.name.as_str()) {
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
/// Roc marks an unused binding with a leading `_`; Codex has no such mark.
fn kebab(name: &str) -> String {
    let name = name.trim_end_matches('!');
    let trimmed = name.trim_start_matches('_');
    let name = if trimmed.is_empty() { "unused" } else { trimmed };
    name.replace('_', "-")
}

/// A type name as Codex spells it: rocemit suffixed one that collides with a Roc
/// builtin (`Box_` for Codex's `Box`).
fn type_name(name: &str) -> String {
    bare(name).trim_end_matches('_').to_string()
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
    /// Each declared union: its tags as declared, and its type parameters in order.
    /// A union in a signature arrives structural, since an alias is transparent,
    /// and is named in Codex: `[Just(a), None]` is `Maybe a`.
    unions: Vec<(&'static str, Vec<(&'static str, Vec<Type>)>, Vec<u32>)>,
    /// The code point each CCE code 0..127 names.
    points: Vec<i128>,
}

/// The integers of a top-level list definition, `name = [..]`.
fn list_def(ast: &Expr, name: &str) -> Option<Vec<i128>> {
    let mut cursor = ast;
    while let Expr::Let { name: n, value, body, .. } = cursor {
        if *n == name {
            let Expr::List(items, _) = &**value else { return None };
            return items.iter().map(|i| if let Expr::Int(v, _) = i { Some(*v) } else { None }).collect();
        }
        cursor = body;
    }
    None
}

/// A declaration's type parameters: the variables of its payloads, in the order
/// they first appear, which is the order `Tup2(a, b) : [MkTup2(a, b)]` declares them.
fn params_of(tags: &[(&'static str, Vec<Type>)]) -> Vec<u32> {
    fn walk(t: &Type, out: &mut Vec<u32>) {
        match t {
            Type::TypeVar(v) => {
                if !out.contains(v) {
                    out.push(*v)
                }
            }
            Type::List(e) => walk(e, out),
            Type::Function(a, b) => {
                walk(a, out);
                walk(b, out)
            }
            Type::Tuple(xs) => xs.iter().for_each(|x| walk(x, out)),
            Type::Record { fields, .. } => fields.iter().for_each(|(_, x)| walk(x, out)),
            Type::TagUnion { tags, .. } => tags.iter().flat_map(|(_, p)| p).for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    tags.iter().flat_map(|(_, p)| p).for_each(|t| walk(t, &mut out));
    out
}

/// Bind a declaration's variables by walking it beside a use of it.
fn bind(decl: &Type, used: &Type, out: &mut HashMap<u32, Type>) {
    match (decl, used) {
        (Type::TypeVar(v), t) => {
            out.entry(*v).or_insert_with(|| t.clone());
        }
        (Type::List(a), Type::List(b)) => bind(a, b, out),
        (Type::Function(a1, b1), Type::Function(a2, b2)) => {
            bind(a1, a2, out);
            bind(b1, b2, out)
        }
        (Type::Tuple(xs), Type::Tuple(ys)) => xs.iter().zip(ys).for_each(|(x, y)| bind(x, y, out)),
        (Type::Record { fields: f1, .. }, Type::Record { fields: f2, .. }) => {
            for (n, x) in f1 {
                if let Some((_, y)) = f2.iter().find(|(m, _)| m == n) {
                    bind(x, y, out)
                }
            }
        }
        (Type::TagUnion { tags: t1, .. }, Type::TagUnion { tags: t2, .. }) => {
            for (n, p1) in t1 {
                if let Some((_, p2)) = t2.iter().find(|(m, _)| m == n) {
                    p1.iter().zip(p2).for_each(|(x, y)| bind(x, y, out))
                }
            }
        }
        _ => {}
    }
}

/// The expression's precedence as an operand: an atom needs no parentheses.
fn atom(s: &str) -> bool {
    let s = s.trim();
    // `-5` is an operand only in parentheses: `f (-5)`, but `x = -5`.
    if s.starts_with('-') {
        return false;
    }
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
        let name = &type_name(name);
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
                let heads: Vec<String> = params_of(tags).iter().map(|v| format!(" (t{})", v)).collect();
                let mut out = format!("  {}{} =\n", name, heads.concat());
                for (tag, payload) in tags {
                    out.push_str(&format!("    | {}", tag));
                    for p in payload {
                        out.push_str(&format!(" ({})", self.ty(p)?));
                    }
                    out.push('\n');
                }
                Some(out)
            }
            Type::Nominal { backing, .. } if !matches!(**backing, Type::TypeVar(_)) => return self.type_decl(name, backing),
            // An alias of anything else is transparent: its uses already say the type.
            _ => None,
        })
    }

    fn ty(&self, t: &Type) -> Result<String, String> {
        Ok(match t {
            Type::I64 => "Integer".into(),
            Type::Str => "Text".into(),
            Type::F64 => "Real".into(),
            Type::Bool => "Boolean".into(),
            Type::Unit => "Nothing".into(),
            Type::List(e) => format!("List {}", paren(self.ty(e)?)),
            Type::Nominal { name, .. } if bare(name) == TEXT_MODULE => "Text".into(),
            Type::Nominal { name, .. } if bare(name) == CHAR_MODULE => "Char".into(),
            Type::Nominal { name, .. } => type_name(name),
            Type::Record { fields, .. } => type_name(self.record_names(fields)?),
            Type::TypeVar(v) => format!("t{}", v),
            Type::TagUnion { tags, .. } => {
                let names: Vec<&str> = {
                    let mut n: Vec<&str> = tags.iter().map(|(t, _)| *t).collect();
                    n.sort_unstable();
                    n
                };
                let found = self.unions.iter().find(|(_, decl, _)| {
                    let mut d: Vec<&str> = decl.iter().map(|(t, _)| *t).collect();
                    d.sort_unstable();
                    d == names
                });
                let Some((name, decl, params)) = found else {
                    return Err(format!("an anonymous union {}", t));
                };
                let mut bound = HashMap::new();
                bind(&Type::TagUnion { tags: decl.clone(), open: false }, t, &mut bound);
                let mut out = type_name(name);
                for p in params {
                    let arg = bound.get(p).cloned().unwrap_or(Type::TypeVar(*p));
                    out.push(' ');
                    out.push_str(&paren(self.ty(&arg)?));
                }
                out
            }
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
        self.record_names(fields).ok()
    }

    /// The one declared record type with these fields; or, where there is none or
    /// more than one, why not. Two aliases of one shape (`Byte : { val : I64 }`,
    /// `Wide : { val : I64 }`) are one Roc type, and which Codex type a literal
    /// was is not in the Roc.
    fn record_names(&self, fields: &[(&'static str, Type)]) -> Result<&'static str, String> {
        let want = field_names(fields);
        let found: Vec<&str> = self.records.iter().filter(|(_, fs)| *fs == want).map(|(n, _)| bare(n)).collect();
        match found.as_slice() {
            [one] => Ok(crate::memory::string_pool::intern(one)),
            [] => Err(format!("a record {{ {} }} of no declared record type", want.join(", "))),
            many => Err(format!("a record {{ {} }} that could be any of {}", want.join(", "), many.join(", "))),
        }
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
            // An effectful function is an `act` whose last line is its value.
            Expr::Lambda { params, body, .. } if name.ends_with('!') => {
                let (ps, result) = peel(sig, params.len()).ok_or_else(|| format!("`{}`'s annotation has fewer parameters than its lambda", name))?;
                let ps: Result<Vec<String>, String> = ps.iter().map(|p| self.ty(p)).collect();
                let heads: Vec<String> = params.iter().map(|p| format!(" ({})", kebab(p))).collect();
                Ok(format!(
                    "  {} : {} -> [Console] {}\n  {}{} = act\n{}  end\n",
                    cname,
                    ps?.join(", "),
                    self.ty(result)?,
                    cname,
                    heads.concat(),
                    self.statements(body, 4)?
                ))
            }
            Expr::Lambda { params, body, .. } => {
                let (ps, result) = peel(sig, params.len()).ok_or_else(|| format!("`{}`'s annotation has fewer parameters than its lambda", name))?;
                let ps: Result<Vec<String>, String> = ps.iter().map(|p| self.ty(p)).collect();
                let heads: Vec<String> = params.iter().map(|p| format!("({})", kebab(p))).collect();
                Ok(format!("  {} : {} -> {}\n  {} {} ={}\n", cname, ps?.join(", "), self.ty(result)?, cname, heads.join(" "), body_text(&self.expr(body)?)))
            }
            _ => Ok(format!("  {} : {}\n  {} ={}\n", cname, self.ty(sig)?, cname, body_text(&self.expr(value)?))),
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
                // `w = noisy!(1)`: an effect's answer is bound for the rest of the act.
                Expr::Let { name, value, body, .. } if effect_call(value) => {
                    out.push_str(&format!("{}{} <- {}\n", pad, kebab(name), self.effect(value)?));
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
                // An effectful function's value: the act's last line.
                other if !is_statement(other) => {
                    out.push_str(&format!("{}{}\n", pad, indent(&self.expr(other)?, ind + 2)));
                    return Ok(out);
                }
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
            if let Some([t]) = call_of(arg, "CceText.printed") {
                return Ok(format!("{}print-line-uni {}\n", pad, paren(self.expr(t)?)));
            }
            // An opening's Integer value, which the driver prints.
            if let Some([n]) = call_of(arg, "I64.to_str") {
                return Ok(format!("{}print-line-uni (show {})\n", pad, paren(self.expr(n)?)));
            }
        }
        if effect_call(e) {
            return Ok(format!("{}{}\n", pad, self.effect(e)?));
        }
        match e {
            // A block of statements.
            Expr::Let { .. } => self.statements(e, ind),
            // A choice between statements: an `if` whose branches are acts.
            Expr::If { condition, then_branch, otherwise, .. } if is_statement(e) => Ok(format!(
                "{pad}if {} then act\n{}{pad}end else act\n{}{pad}end\n",
                self.expr(condition)?,
                self.statements(then_branch, ind + 2)?,
                self.statements(otherwise, ind + 2)?,
            )),
            // A choice among statements: a `when` whose arms are acts.
            Expr::Match { scrutinee, arms, .. } if is_statement(e) => {
                let (scrutinee, on_char) = match call_of(scrutinee, "CceChar.code") {
                    Some([c]) => (c, true),
                    _ => (&**scrutinee, false),
                };
                let mut out = format!("{}when {}\n", pad, self.expr(scrutinee)?);
                for arm in arms {
                    let guard = match &arm.guard {
                        Some(g) => format!(" when {}", self.expr(g)?),
                        None => String::new(),
                    };
                    for p in &arm.patterns {
                        let pat = match p {
                            Pattern::Int(n) if on_char => self.char_literal(*n).ok_or_else(|| format!("a Char pattern {} with no printable literal", n))?,
                            Pattern::Wildcard => "otherwise".into(),
                            other => self.pattern(other, scrutinee)?,
                        };
                        out.push_str(&format!("{}  is {}{} -> act\n{}{}  end\n", pad, pat, guard, self.statements(&arm.body, ind + 4)?, pad));
                    }
                }
                Ok(out)
            }
            other => Err(format!("a statement {}", short(other))),
        }
    }

    /// A call of an effectful function: `noisy!(1)` is `noisy 1`.
    fn effect(&self, e: &Expr) -> Result<String, String> {
        let Expr::Call { func, args, .. } = e else { return Err("an effect that is not a call".into()) };
        let mut out = match &**func {
            Expr::Ident(n, _) | Expr::Qualified { name: n, .. } => kebab(n),
            other => return Err(format!("an effect through {}", short(other))),
        };
        for a in args {
            out.push(' ');
            out.push_str(&paren(self.expr(a)?));
        }
        Ok(out)
    }

    fn expr(&self, e: &Expr) -> Result<String, String> {
        if let Some(s) = self.idiom(e)? {
            return Ok(s);
        }
        Ok(match e {
            // Codex reads `-9223372036854775808` as the negation of a literal one past
            // the largest Integer; its own programs write the minimum in hex.
            Expr::Int(n, _) if *n == i64::MIN as i128 => "#8000000000000000".into(),
            Expr::Int(n, _) => n.to_string(),
            // Codex has one real type: a fractional literal the checker left as a
            // `Dec` (an unannotated record's field) is a Codex real as well.
            Expr::Float(f, _, _) => real_literal(*f).ok_or_else(|| format!("a real {} Codex cannot spell", f))?,
            // Codex has one string type, `Text`. A literal the checker left as `Str`
            // (a let-generalised local's) is still one; what a `Str` could do that a
            // `Text` cannot is a `Str` builtin, and those are refused.
            Expr::Str(s, _) => text_literal(s),
            Expr::Bool(b, _) => if *b { "True".into() } else { "False".into() },
            Expr::Ident(n, _) => kebab(n),
            Expr::Qualified { name, .. } => kebab(name),
            Expr::BinOp { left, op, right, .. } => self.binop(left, *op, right)?,
            Expr::Call { func, args, .. } => {
                let f = match &**func {
                    Expr::Ident(n, _) => kebab(n),
                    Expr::Qualified { module, name, .. } if *module != TEXT_MODULE && !ROC_BUILTIN_MODULES.contains(module) => kebab(name),
                    // A function value: a record's field, `(bx.get)(i)`, or a call's
                    // answer, `mk(4)(20, 22)`.
                    other @ (Expr::FieldAccess { .. } | Expr::Call { .. }) => format!("({})", self.expr(other)?),
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
                // Named by its own fields, which a literal lists in full: its type
                // can be an unresolved variable (a payload of a generic tag).
                let shape: Vec<(&'static str, Type)> = fields.iter().map(|(f, _)| (*f, Type::Unit)).collect();
                let rname = self.record_names(&shape)?;
                let fs: Result<Vec<String>, String> = fields.iter().map(|(f, v)| Ok(format!("{} = {}", kebab(f), self.expr(v)?))).collect();
                format!("{} {{ {} }}", type_name(rname), fs?.join(", "))
            }
            // `{ ..r, f: v }` is Codex's `__record-set r "f" v`, a field at a time.
            Expr::RecordUpdate { base, fields, .. } => {
                let mut out = self.expr(base)?;
                for (f, v) in fields {
                    out = format!("__record-set {} \"{}\" {}", paren(out), kebab(f), paren(self.expr(v)?));
                }
                out
            }
            // rocemit's clamp for a bounded integer: `{ val: e }.val` is `e`, when
            // no declared record has that one field.
            Expr::FieldAccess { record, field, .. }
                if matches!(&**record, Expr::Record(fs, _) if fs.len() == 1 && fs[0].0 == *field)
                    && self.record_name(&[(*field, Type::I64)]).is_none() =>
            {
                let Expr::Record(fs, _) = &**record else { unreachable!() };
                self.expr(&fs[0].1)?
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
        // rocemit matches a Codex Char on its code: `match CceChar.code(c) { 15 => }`
        // is `when c is 'a' ->`.
        let (scrutinee, on_char) = match call_of(scrutinee, "CceChar.code") {
            Some([c]) => (c, true),
            _ => (scrutinee, false),
        };
        let mut out = format!("when {}", self.expr(scrutinee)?);
        for arm in arms {
            let guard = match &arm.guard {
                Some(g) => format!(" when {}", self.expr(g)?),
                None => String::new(),
            };
            for p in &arm.patterns {
                let pat = match p {
                    Pattern::Int(n) if on_char => self.char_literal(*n).ok_or_else(|| format!("a Char pattern {} with no printable literal", n))?,
                    // A whole arm's wildcard; inside a constructor it is `_`.
                    Pattern::Wildcard => "otherwise".into(),
                    other => self.pattern(other, scrutinee)?,
                };
                out.push_str(&format!("\n  is {}{} -> {}", pat, guard, indent(&self.expr(&arm.body)?, 4)));
            }
        }
        Ok(out)
    }

    fn pattern(&self, p: &Pattern, scrutinee: &Expr) -> Result<String, String> {
        Ok(match p {
            Pattern::Wildcard => "_".into(),
            Pattern::Binding(n) => kebab(n),
            // A Codex list is matched with its constructors: `Nil`, `Cons (h) (t)`.
            Pattern::List { before, rest: None, after } if before.is_empty() && after.is_empty() => "Nil".into(),
            Pattern::List { before, rest: Some(tail), after } if before.len() == 1 && after.is_empty() => format!(
                "Cons ({}) ({})",
                self.pattern(&before[0], scrutinee)?,
                tail.map(kebab).unwrap_or_else(|| "_".into())
            ),
            Pattern::Int(n) if matches!(self.ty_of(scrutinee), Some(Type::Nominal { name, .. }) if bare(name) == CHAR_MODULE) => {
                self.char_literal(*n).ok_or_else(|| format!("a Char pattern {} with no printable literal", n))?
            }
            Pattern::Int(n) if *n < 0 => format!("({})", n),
            Pattern::Int(n) => n.to_string(),
            Pattern::Str(s) => text_literal(s),
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

    /// A CCE code as a Codex char literal, where the code names a plain printable
    /// character.
    fn char_literal(&self, code: i128) -> Option<String> {
        let point = *self.points.get(usize::try_from(code).ok()?)?;
        let c = char::from_u32(u32::try_from(point).ok()?)?;
        (c.is_ascii_graphic() || c == ' ').then(|| match c {
            '\'' => "'\\''".to_string(),
            '\\' => "'\\\\'".to_string(),
            c => format!("'{}'", c),
        })
    }

    fn concat_left(&self, a: &Expr) -> Result<String, String> {
        let s = self.expr(a)?;
        Ok(if call_of(a, "CceText.concat").is_some() || call_of(a, "List.concat").is_some() { s } else { paren(s) })
    }

    /// rocemit's idioms, read back as the Codex they were written from.
    fn idiom(&self, e: &Expr) -> Result<Option<String>, String> {
        // A Codex Char: `CceChar.of_code(15)` is `'a'`; the conversions and the
        // classifiers are Codex's builtins.
        if let Some([c]) = call_of(e, "CceChar.of_code") {
            if let Expr::Int(n, _) = c {
                if let Some(lit) = self.char_literal(*n) {
                    return Ok(Some(lit));
                }
            }
            return Ok(Some(format!("code-to-char {}", paren(self.expr(c)?))));
        }
        for (roc, codex) in [
            ("CceChar.code", "char-code"),
            ("CceChar.is_letter", "is-letter"),
            ("CceChar.is_digit", "is-digit"),
            ("CceChar.is_whitespace", "is-whitespace"),
        ] {
            if let Some([c]) = call_of(e, roc) {
                return Ok(Some(format!("{} {}", codex, paren(self.expr(c)?))));
            }
        }
        // `List.get(xs, I64.to_u64_wrap(i)) ?? crash(..)`, which the parser has
        // made a match, is `list-at xs i`; `List.set` is `list-set-at`.
        if let Expr::Match { scrutinee, arms, .. } = e {
            if arms.len() == 2 && matches!(arms[1].body, Expr::Crash(..)) {
                if let [Pattern::Tag { name: "Ok", args }] = arms[0].patterns.as_slice() {
                    if let ([Pattern::Binding(v)], Expr::Ident(b, _)) = (args.as_slice(), &arms[0].body) {
                        if v == b {
                            for (roc, codex) in [("List.get", "list-at"), ("List.set", "list-set-at"), ("List.insert", "list-insert-at")] {
                                if let Some(xs) = call_of(scrutinee, roc) {
                                    let mut out = codex.to_string();
                                    for (k, x) in xs.iter().enumerate() {
                                        let x = match (k, call_of(x, "I64.to_u64_wrap")) {
                                            (1, Some([i])) => i,
                                            _ => x,
                                        };
                                        out.push(' ');
                                        out.push_str(&paren(self.expr(x)?));
                                    }
                                    return Ok(Some(out));
                                }
                            }
                        }
                    }
                }
            }
        }
        // `Text.concat(a, b)` is `&`.
        // `&` is left-associative, so a left operand that is itself an `&` needs no
        // parentheses; anything else might reach past it (`if .. else "a" & "b"`).
        if let Some([a, b]) = call_of(e, "CceText.concat") {
            return Ok(Some(format!("{} & {}", self.concat_left(a)?, paren(self.expr(b)?))));
        }
        if let Some([n]) = call_of(e, "CceText.show_int") {
            return Ok(Some(format!("show {}", paren(self.expr(n)?))));
        }
        // The rest of `Text`: each is the Codex builtin rocemit wrote it for.
        for (roc, codex) in [
            ("CceText.len", "text-length"),
            ("CceText.char_at", "char-at"),
            ("CceText.char_code_at", "char-code-at"),
            ("CceText.substring", "substring"),
            ("CceText.split", "text-split"),
            ("CceText.char_to_text", "char-to-text"),
            ("CceText.char_encode", "char-encode"),
            ("CceText.compare", "text-compare"),
            ("CceText.to_integer", "text-to-integer"),
            ("CceText.contains", "text-contains"),
            ("CceText.starts_with", "text-starts-with"),
            ("CceText.ends_with", "text-ends-with"),
            ("CceText.replace", "text-replace"),
            ("CceText.concat_list", "text-concat-list"),
            ("CceText.of_bytes", "raw-bytes-to-text"),
        ] {
            if let Some(args) = call_of(e, roc) {
                let mut out = codex.to_string();
                for a in args {
                    out.push(' ');
                    out.push_str(&paren(self.expr(a)?));
                }
                return Ok(Some(out));
            }
        }
        // `U64.to_i64_wrap(List.len(xs))` is `list-length xs`.
        if let Some([inner]) = call_of(e, "U64.to_i64_wrap") {
            if let Some([xs]) = call_of(inner, "List.len") {
                return Ok(Some(format!("list-length {}", paren(self.expr(xs)?))));
            }
        }
        if let Some([a, b]) = call_of(e, "List.concat") {
            return Ok(Some(format!("{} & {}", self.concat_left(a)?, paren(self.expr(b)?))));
        }
        if let Some([xs, x]) = call_of(e, "List.append") {
            return Ok(Some(format!("list-snoc {} {}", paren(self.expr(xs)?), paren(self.expr(x)?))));
        }
        for (roc, codex) in [
            ("I64.min", "min"),
            ("I64.max", "max"),
            ("I64.rem_by", "int-rem"),
            ("Prelude.int_mod", "int-mod"),
            ("I64.bitwise_and", "bit-and"),
            ("I64.bitwise_or", "bit-or"),
            ("I64.bitwise_xor", "bit-xor"),
        ] {
            if let Some([a, b]) = call_of(e, roc) {
                return Ok(Some(format!("{} {} {}", codex, paren(self.expr(a)?), paren(self.expr(b)?))));
            }
        }
        if let Some([x]) = call_of(e, "I64.bitwise_not") {
            return Ok(Some(format!("bit-not {}", paren(self.expr(x)?))));
        }
        // Reals, and rocemit's Prelude: `~` is approximate equality, `~0` exact.
        if let Some([a, b]) = call_of(e, "Prelude.approx_eq") {
            return Ok(Some(format!("{} ~ {}", paren(self.expr(a)?), paren(self.expr(b)?))));
        }
        if let Expr::BinOp { left, op: BinOp::Eq, right, .. } = e {
            if let (Some([a]), Some([b])) = (call_of(left, "F64.to_bits"), call_of(right, "F64.to_bits")) {
                return Ok(Some(format!("{} ~0 {}", paren(self.expr(a)?), paren(self.expr(b)?))));
            }
        }
        if let Some([t]) = call_of(e, "CceText.of_str") {
            if let Some([x]) = call_of(t, "Prelude.real_to_str") {
                return Ok(Some(format!("show {}", paren(self.expr(x)?))));
            }
        }
        if let Some([x]) = call_of(e, "U64.to_i64_wrap") {
            if let Some([r]) = call_of(x, "F64.to_bits") {
                return Ok(Some(format!("real-to-bits {}", paren(self.expr(r)?))));
            }
        }
        // Roc's `mod_by` takes the divisor's sign and Codex's `int-mod` is
        // Euclidean: the same for a positive divisor, which is the only one read.
        if let Some([a, b]) = call_of(e, "I64.mod_by") {
            if matches!(b, Expr::Int(n, _) if *n > 0) {
                return Ok(Some(format!("int-mod {} {}", paren(self.expr(a)?), paren(self.expr(b)?))));
            }
        }
        // A real rocemit wrote as its bits: the shortest decimal that reads back as
        // those bits, where Codex can spell it.
        if let Some([Expr::Int(bits, _)]) = call_of(e, "F64.from_bits") {
            if let Some(text) = real_literal(f64::from_bits(*bits as u64)) {
                return Ok(Some(text));
            }
        }
        if let Some([x]) = call_of(e, "F64.from_bits") {
            if let Some([b]) = call_of(x, "I64.to_u64_wrap") {
                return Ok(Some(format!("bits-to-real {}", paren(self.expr(b)?))));
            }
        }
        for (roc, codex) in [
            ("Prelude.int_abs", "abs"),
            ("I64.to_f64", "real-from-int"),
            ("F64.to_i64_wrap", "real-to-int"),
            ("F64.abs", "real-abs"),
            ("F64.sqrt", "real-sqrt"),
        ] {
            if let Some([x]) = call_of(e, roc) {
                return Ok(Some(format!("{} {}", codex, paren(self.expr(x)?))));
            }
        }
        for (roc, codex) in [("F64.max", "real-max"), ("F64.min", "real-min")] {
            if let Some([a, b]) = call_of(e, roc) {
                return Ok(Some(format!("{} {} {}", codex, paren(self.expr(a)?), paren(self.expr(b)?))));
            }
        }
        // rocemit writes arithmetic on a Codex `Integer wrapping` as Roc's
        // wrapping operations; the Codex has lost that type, so the wrapping
        // happens in `Roc--Wrap`'s helpers, whose first operand is `Integer wrapping`.
        for (roc, helper) in [("I64.plus_wrap", "roc-plus-wrap"), ("I64.minus_wrap", "roc-minus-wrap"), ("I64.times_wrap", "roc-times-wrap")] {
            if let Some([a, b]) = call_of(e, roc) {
                return Ok(Some(format!("{} {} {}", helper, paren(self.expr(a)?), paren(self.expr(b)?))));
            }
        }
        for (roc, op) in [("I64.div_trunc_by", "/"), ("Prelude.int_pow", "^")] {
            if let Some([a, b]) = call_of(e, roc) {
                return Ok(Some(format!("{} {} {}", paren(self.expr(a)?), op, paren(self.expr(b)?))));
            }
        }
        // A shift's count is narrowed to a byte for Roc: `bit-shl x n`.
        for (roc, codex) in [("I64.shl_wrap", "bit-shl"), ("I64.shr_wrap", "bit-shr"), ("I64.shr_zf_wrap", "bit-shru")] {
            if let Some([x, n]) = call_of(e, roc) {
                let n = match call_of(n, "I64.to_u8_wrap") {
                    Some([n]) => n,
                    _ => n,
                };
                return Ok(Some(format!("{} {} {}", codex, paren(self.expr(x)?), paren(self.expr(n)?))));
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
                    return Ok(Some(format!("-{}", n)));
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

/// A call of a function whose name ends in `!`, other than the print helper.
fn effect_call(e: &Expr) -> bool {
    let Expr::Call { func, .. } = e else { return false };
    matches!(&**func, Expr::Ident(n, _) | Expr::Qualified { name: n, .. } if n.ends_with('!') && *n != "line!")
}

/// Is `e` something an act says rather than answers: printing, an effect, or a
/// choice among them?
fn is_statement(e: &Expr) -> bool {
    effect_call(e)
        || call_of(e, "line!").is_some()
        || matches!(e, Expr::Let { .. })
        || matches!(e, Expr::Match { arms, .. } if arms.iter().any(|a| is_statement(&a.body)))
        || matches!(e, Expr::If { then_branch, otherwise, .. } if is_statement(then_branch) || is_statement(otherwise))
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

/// A definition's body: on the `=` line when it is one line, as Codex writes it
/// (and as a negative literal must be), else below it.
fn body_text(s: &str) -> String {
    if s.contains('\n') { format!("\n    {}", indent(s, 4)) } else { format!(" {}", s) }
}

/// A real as a Codex literal that reads back as the same double: Rust's `Display`
/// writes the shortest such decimal and never an exponent, which Codex writes
/// the same way (`sq-tol = 0.000000001`); a whole number gets its `.0`.
fn real_literal(f: f64) -> Option<String> {
    if !f.is_finite() {
        return None;
    }
    let text = f.to_string();
    Some(if text.contains('.') { text } else { format!("{}.0", text) })
}

fn indent(s: &str, n: usize) -> String {
    s.replace('\n', &format!("\n{}", " ".repeat(n)))
}

/// A short rendering of an expression, for a refusal.
fn short(e: &Expr) -> String {
    let s = e.to_string();
    if s.len() > 80 { format!("{}...", &s[..s.char_indices().nth(77).map_or(s.len(), |(i, _)| i)]) } else { s }
}
