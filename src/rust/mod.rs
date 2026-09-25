//! **Roc to Rust.** A checked Roc program, written out as one Rust file: every
//! module's definitions at the top level (`Module__name`), its types, and a
//! `main` that runs `main!` on a thread with a deep stack.
//!
//! Every Roc module is translated, rocemit's runtime modules (`CceText`,
//! `CceChar`, `Prelude`) included; what is written by hand is only Roc's own
//! builtins (`List.*`, `Str.*`, `I64.*`, ...), in `runtime.rs`, which heads every
//! program.
//!
//! **Types.** A structural record or tag union (`{ p : I64 }`, `[Just(a), None]`)
//! is the same Roc type wherever it is written, alias or not, so it is one Rust
//! struct or enum per set of field or tag names, generic over every field or
//! payload (`Rec_p<T0>`, `Tags_Just_None<T0>`). `[Ok(a), Err(e)]` is `Result`. A
//! nominal (`:=`) is a named type; only a nominal can be recursive, so a field
//! holding one is an `Rc`. A nominal over a list or a scalar (`CceText ::
//! List(U8)`) is a type alias: Roc erases it at run time, and so does this.
//!
//! **Values.** Every non-copy value is cloned where it is read; a `List` is an
//! `Rc` that copies on write, so a clone is a count. A closure is an
//! `Rc<dyn Fn(..)>` with its captures cloned in. A `match` is a labeled block of
//! nested `if let`s, one per arm, which takes nested patterns, guards and
//! literals alike.
//!
//! What it does not know how to write it refuses, naming the form.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::ast::{BinOp, Expr, MatchArm, NodeId, Pattern, StrPart};
use crate::codex::Input;
use crate::types::Type;

const RUNTIME: &str = include_str!("runtime.rs");

/// Roc's own modules, which `runtime.rs` answers.
const BUILTIN_MODULES: [&str; 12] = ["List", "Str", "I64", "U64", "U8", "I32", "U32", "U16", "F64", "Num", "Bool", "Dict"];

pub fn emit(input: &Input) -> Result<String, String> {
    let mut cx = Cx::new(input);
    cx.collect();
    // Only what `main!` reaches: writing a definition records every top-level
    // name it refers to, and those are written next, until nothing new appears.
    let mut all: HashMap<String, (Option<&str>, Option<&Type>, &Expr)> = HashMap::new();
    for (module, ast) in input.modules.iter().map(|m| (Some(m.name.as_str()), m.ast)).chain(std::iter::once((None, input.app))) {
        let mut cursor = ast;
        while let Expr::Let { name, annotation, value, body, .. } = cursor {
            if *name != "_" {
                all.insert(name.to_string(), (module, annotation.as_ref(), &**value));
            }
            cursor = body;
        }
    }
    cx.wanted.borrow_mut().insert("main!".to_string());
    let mut done: BTreeSet<String> = BTreeSet::new();
    let mut defs = String::new();
    loop {
        let next = cx.wanted.borrow().iter().find(|n| !done.contains(*n)).cloned();
        let Some(name) = next else { break };
        done.insert(name.clone());
        let Some((module, annotation, value)) = all.get(&name) else {
            return Err(format!("`{}` is referred to and not defined", name));
        };
        let def = cx.def(*module, &name, *annotation, value);
        cx.renames.borrow_mut().clear();
        defs.push_str(&def?);
        defs.push('\n');
    }
    let types = cx.type_defs()?;
    let mut out = String::new();
    out.push_str(RUNTIME);
    out.push_str("\n// ---- the program's types ----\n\n");
    out.push_str(&types);
    out.push_str("\n// ---- the program ----\n\n");
    out.push_str(&defs);
    out.push_str(
        "fn main() {\n    let t = std::thread::Builder::new().stack_size(1 << 30).spawn(|| { main__e(List::<String>::of(vec![])); }).unwrap();\n    if t.join().is_err() { std::process::exit(1); }\n}\n",
    );
    Ok(out)
}

/// A declared nominal: its Rust name, its type parameters in order, and what it is
/// built over.
#[derive(Clone)]
struct Nominal {
    rust: String,
    params: Vec<u32>,
    backing: Type,
}

struct Cx<'a> {
    input: &'a Input<'a>,
    types: &'a HashMap<NodeId, Type>,
    /// Every top-level definition by its Roc name (`CceText.len`, `main!`), and
    /// whether it is a function (a lambda) or a value (a thunk here).
    tops: HashMap<String, bool>,
    /// The nominals, by their bare Roc name.
    nominals: HashMap<String, Nominal>,
    /// Structural records and unions met, by their sorted field or tag names:
    /// each is one generic Rust type.
    records: RefCell<BTreeSet<Vec<String>>>,
    unions: RefCell<BTreeMap<Vec<String>, Vec<usize>>>,
    /// The top-level definitions referred to so far; see `emit`.
    wanted: RefCell<BTreeSet<String>>,
    /// Loops written so far, for their labels: a `break` inside a `match`'s
    /// labeled block must name its loop.
    loops: RefCell<usize>,
    /// The definition being written: its body's type variables, as its
    /// signature names them. The checker checks a body against a fresh copy of
    /// the annotation, so a lambda inside `set_insert : List(a), a -> ..` has
    /// the copy's variable, not `a`.
    renames: RefCell<HashMap<u32, Type>>,
    /// The one use of a variable that hands it over rather than cloning it: `$x`
    /// in `$x = List.append($x, y)`. The list then has one owner, and is changed
    /// in place instead of copied, as roc does.
    moving: RefCell<Option<NodeId>>,
}

fn sanitize(name: &str) -> String {
    let base = name.trim_end_matches('!');
    let bang = name.ends_with('!');
    // `$deck`, a `var`: `v_deck`.
    let s = base.replace('.', "__").replace('$', "v_");
    let s = match s.as_str() {
        "as" | "break" | "const" | "continue" | "crate" | "else" | "enum" | "extern" | "false" | "fn" | "for" | "if" | "impl" | "in"
        | "let" | "loop" | "match" | "mod" | "move" | "mut" | "pub" | "ref" | "return" | "self" | "Self" | "static" | "struct"
        | "super" | "trait" | "true" | "type" | "unsafe" | "use" | "where" | "while" | "async" | "await" | "dyn" | "abstract"
        | "become" | "box" | "do" | "final" | "macro" | "override" | "priv" | "typeof" | "unsized" | "virtual" | "yield" | "try"
        | "gen" | "main" => format!("{}_", s),
        _ => s,
    };
    if bang { format!("{}_e", s) } else { s }
}

fn bare(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// The type variables of a type, in the order they first appear.
fn vars_of(t: &Type, out: &mut Vec<u32>) {
    match t {
        // The parser's stand-in for a name declared elsewhere: not a variable.
        Type::TypeVar(u32::MAX) => {}
        Type::TypeVar(v) => {
            if !out.contains(v) {
                out.push(*v)
            }
        }
        Type::List(e) | Type::Optional(e) | Type::Range(e) => vars_of(e, out),
        Type::Function(a, b) => {
            vars_of(a, out);
            vars_of(b, out)
        }
        Type::Tuple(xs) => xs.iter().for_each(|x| vars_of(x, out)),
        Type::Record { fields, .. } => fields.iter().for_each(|(_, x)| vars_of(x, out)),
        Type::TagUnion { tags, .. } => tags.iter().flat_map(|(_, p)| p).for_each(|x| vars_of(x, out)),
        Type::Nominal { backing, .. } if !matches!(**backing, Type::TypeVar(_)) => vars_of(backing, out),
        _ => {}
    }
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
        (Type::Nominal { backing: b1, .. }, Type::Nominal { backing: b2, .. }) => bind(b1, b2, out),
        _ => {}
    }
}

fn copy_type(t: &Type) -> bool {
    matches!(
        t,
        Type::I8 | Type::I16 | Type::I32 | Type::I64 | Type::I128 | Type::U8 | Type::U16 | Type::U32 | Type::U64 | Type::U128 | Type::F32 | Type::F64 | Type::Bool | Type::Unit
    )
}

/// The scope a body is written in: the locals, and the owner its bare names are
/// looked up under first.
#[derive(Clone)]
struct Scope {
    locals: Vec<String>,
    owner: String,
    module: Option<String>,
    /// The enclosing definition's type parameters. A type written inside its
    /// body names only these; any other variable (the checker's own, for an
    /// instantiation) is `_`, and rustc infers it.
    generics: Vec<u32>,
    /// The label of the loop a `break` leaves.
    in_loop: Option<String>,
}

impl Scope {
    fn local(&self, n: &str) -> bool {
        self.locals.iter().any(|l| l == n)
    }
}

impl<'a> Cx<'a> {
    fn new(input: &'a Input<'a>) -> Cx<'a> {
        Cx {
            input,
            types: &input.types,
            tops: HashMap::new(),
            nominals: HashMap::new(),
            records: RefCell::new(BTreeSet::new()),
            unions: RefCell::new(BTreeMap::new()),
            wanted: RefCell::new(BTreeSet::new()),
            loops: RefCell::new(0),
            renames: RefCell::new(HashMap::new()),
            moving: RefCell::new(None),
        }
    }

    fn collect(&mut self) {
        let params: HashMap<&str, &Vec<u32>> = self.input.nominal_params.iter().map(|(n, p)| (n.as_str(), p)).collect();
        for (name, ty) in self.input.modules.iter().flat_map(|m| m.types.iter()).chain(self.input.app_types.iter()) {
            if let Type::Nominal { backing, .. } = ty {
                // A placeholder, or a module's namespace (`Maybe :: []`), which no
                // value has: not a type to write.
                if matches!(**backing, Type::TypeVar(_)) || matches!(&**backing, Type::TagUnion { tags, .. } if tags.is_empty()) {
                    continue;
                }
                let b = bare(name).to_string();
                let ps = match params.get(*name).or_else(|| params.get(b.as_str())) {
                    Some(p) => (*p).clone(),
                    None => {
                        let mut v = Vec::new();
                        vars_of(backing, &mut v);
                        v
                    }
                };
                self.nominals.insert(b.clone(), Nominal { rust: b.trim_end_matches('_').to_string() + "_T", params: ps, backing: (**backing).clone() });
            }
        }
        for ast in self.input.modules.iter().map(|m| m.ast).chain(std::iter::once(self.input.app)) {
            let mut cursor = ast;
            while let Expr::Let { name, value, body, .. } = cursor {
                if *name != "_" {
                    self.tops.insert(name.to_string(), matches!(**value, Expr::Lambda { .. }));
                }
                cursor = body;
            }
        }
    }

    fn ty_of(&self, e: &Expr) -> Result<Type, String> {
        let t = self.types.get(&e.id()).ok_or_else(|| format!("no type for {}", short(e)))?;
        let renames = self.renames.borrow();
        Ok(if renames.is_empty() { t.clone() } else { subst(t, &renames) })
    }

    // ---- types ----

    fn ty(&self, t: &Type) -> Result<String, String> {
        Ok(match t {
            Type::I8 => "i8".into(),
            Type::I16 => "i16".into(),
            Type::I32 => "i32".into(),
            Type::I64 => "i64".into(),
            Type::I128 => "i128".into(),
            Type::U8 => "u8".into(),
            Type::U16 => "u16".into(),
            Type::U32 => "u32".into(),
            Type::U64 => "u64".into(),
            Type::U128 => "u128".into(),
            Type::F32 => "f32".into(),
            Type::F64 | Type::Dec => "f64".into(),
            Type::Bool => "bool".into(),
            Type::Str => "String".into(),
            Type::Unit => "()".into(),
            Type::TypeVar(v) => format!("T{}", v),
            Type::List(e) => format!("List<{}>", self.ty(e)?),
            Type::Tuple(xs) => {
                let xs: Result<Vec<String>, String> = xs.iter().map(|x| self.ty(x)).collect();
                format!("({},)", xs?.join(", "))
            }
            Type::Function(..) => {
                let mut ps = Vec::new();
                let mut cur = t;
                while let Type::Function(a, b) = cur {
                    if !matches!(**a, Type::Unit) || ps.is_empty() && !matches!(**b, Type::Function(..)) {
                        ps.push(self.ty(a)?);
                    }
                    cur = b;
                }
                format!("Rc<dyn Fn({}) -> {}>", ps.join(", "), self.ty(cur)?)
            }
            Type::Record { fields, .. } => {
                let names: Vec<String> = sorted_names(fields.iter().map(|(n, _)| *n));
                self.records.borrow_mut().insert(names.clone());
                let mut args = Vec::new();
                for n in &names {
                    let (_, ft) = fields.iter().find(|(f, _)| f == n).expect("named");
                    args.push(self.ty(ft)?);
                }
                format!("{}<{}>", record_name(&names), args.join(", "))
            }
            Type::TagUnion { tags, open: true } if self.declared_union(tags).is_some() => {
                return self.ty(&self.declared_union(tags).expect("guarded"));
            }
            Type::TagUnion { tags, .. } => {
                let names: Vec<String> = sorted_names(tags.iter().map(|(n, _)| *n));
                if names == ["Err", "Ok"] || names == ["Ok"] || names == ["Err"] {
                    let get = |n: &str| tags.iter().find(|(t, _)| *t == n).map(|(_, p)| p.clone()).unwrap_or_default();
                    let ok = get("Ok");
                    let err = get("Err");
                    return Ok(format!("Result<{}, {}>", self.payload(&ok)?, self.payload(&err)?));
                }
                let arity: Vec<usize> = names.iter().map(|n| tags.iter().find(|(t, _)| *t == n).map_or(0, |(_, p)| p.len())).collect();
                self.unions.borrow_mut().insert(names.clone(), arity);
                let mut args = Vec::new();
                for n in &names {
                    let (_, p) = tags.iter().find(|(t, _)| *t == n).expect("named");
                    for x in p {
                        args.push(self.ty(x)?);
                    }
                }
                if args.is_empty() { union_name(&names) } else { format!("{}<{}>", union_name(&names), args.join(", ")) }
            }
            Type::Nominal { name, backing } => {
                let Some(n) = self.nominals.get(bare(name)) else {
                    return Err(format!("the nominal {} is not declared", name));
                };
                if erased(&n.backing) {
                    // A nominal over a list or a scalar is its backing, as at run time.
                    return self.ty(&self.instantiate(n, backing));
                }
                let mut bound = HashMap::new();
                bind(&n.backing, backing, &mut bound);
                if n.params.is_empty() {
                    n.rust.clone()
                } else {
                    let args: Result<Vec<String>, String> = n.params.iter().map(|p| self.ty(bound.get(p).unwrap_or(&Type::Unit))).collect();
                    format!("{}<{}>", n.rust, args?.join(", "))
                }
            }
            other => return Err(format!("the type {}", other)),
        })
    }

    /// An open union (`[MBReady, ..]`, a lone tag's type) as the one declared
    /// structural union holding all its tags, with the declaration's other tags:
    /// a Rust enum is its whole set of tags.
    fn declared_union(&self, tags: &[(&'static str, Vec<Type>)]) -> Option<Type> {
        let mut found = self
            .input
            .modules
            .iter()
            .flat_map(|m| m.types.iter())
            .chain(self.input.app_types.iter())
            .filter_map(|(_, t)| match t {
                Type::TagUnion { tags: d, .. } if tags.iter().all(|(n, _)| d.iter().any(|(m, _)| m == n)) && d.len() > tags.len() => Some(t.clone()),
                _ => None,
            });
        let decl = found.next()?;
        if found.any(|other| sorted_names(tag_names(&other).into_iter()) != sorted_names(tag_names(&decl).into_iter())) {
            return None;
        }
        let mut bound = HashMap::new();
        bind(&decl, &Type::TagUnion { tags: tags.to_vec(), open: true }, &mut bound);
        let decl = subst(&decl, &bound);
        Some(match decl {
            Type::TagUnion { tags, .. } => Type::TagUnion { tags, open: false },
            other => other,
        })
    }

    /// A nominal's backing with the use's arguments put in.
    fn instantiate(&self, n: &Nominal, used_backing: &Type) -> Type {
        if matches!(used_backing, Type::TypeVar(_)) { n.backing.clone() } else { used_backing.clone() }
    }

    /// A tag's payload as one Rust type: nothing, the one, or a tuple.
    fn payload(&self, p: &[Type]) -> Result<String, String> {
        Ok(match p {
            [] => "()".into(),
            [one] => self.ty(one)?,
            many => {
                let xs: Result<Vec<String>, String> = many.iter().map(|x| self.ty(x)).collect();
                format!("({})", xs?.join(", "))
            }
        })
    }

    /// Is this field's type a nominal struct or enum, held behind an `Rc`?
    fn boxed(&self, t: &Type) -> bool {
        match t {
            Type::Nominal { name, .. } => self.nominals.get(bare(name)).is_some_and(|n| !erased(&n.backing)),
            _ => false,
        }
    }

    fn field_ty(&self, t: &Type) -> Result<String, String> {
        let s = self.ty(t)?;
        Ok(if self.boxed(t) { format!("Rc<{}>", s) } else { s })
    }

    /// The Rust definitions of every type the program named.
    fn type_defs(&self) -> Result<String, String> {
        let mut out = String::new();
        let mut noms: Vec<&Nominal> = self.nominals.values().collect();
        noms.sort_by(|a, b| a.rust.cmp(&b.rust));
        for n in noms {
            if erased(&n.backing) {
                continue;
            }
            let generics = if n.params.is_empty() {
                String::new()
            } else {
                format!("<{}>", n.params.iter().map(|p| format!("T{}", p)).collect::<Vec<_>>().join(", "))
            };
            let derive = if holds_fn(&n.backing) { "#[derive(Clone)]" } else { "#[derive(Clone, PartialEq, Debug)]" };
            match &n.backing {
                Type::Record { fields, .. } => {
                    out.push_str(&format!("{}\npub struct {}{} {{\n", derive, n.rust, generics));
                    for (f, t) in fields {
                        out.push_str(&format!("    pub {}: {},\n", sanitize(f), self.field_ty(t)?));
                    }
                    out.push_str("}\n\n");
                }
                Type::TagUnion { tags, .. } => {
                    out.push_str(&format!("{}\npub enum {}{} {{\n", derive, n.rust, generics));
                    for (tag, p) in tags {
                        if p.is_empty() {
                            out.push_str(&format!("    {},\n", tag));
                        } else {
                            let ps: Result<Vec<String>, String> = p.iter().map(|x| self.field_ty(x)).collect();
                            out.push_str(&format!("    {}({}),\n", tag, ps?.join(", ")));
                        }
                    }
                    out.push_str("}\n\n");
                }
                other => return Err(format!("a nominal over {}", other)),
            }
        }
        // Types reached only while writing these definitions are written too; the
        // structural ones are generic, so each is written once.
        for names in self.records.borrow().iter() {
            if names == &["len".to_string(), "start".to_string()] {
                continue; // runtime.rs has it
            }
            let ps: Vec<String> = (0..names.len()).map(|i| format!("T{}", i)).collect();
            out.push_str(&format!("#[derive(Clone, PartialEq, Debug)]\npub struct {}<{}> {{\n", record_name(names), ps.join(", ")));
            for (i, n) in names.iter().enumerate() {
                out.push_str(&format!("    pub {}: T{},\n", sanitize(n), i));
            }
            out.push_str("}\n\n");
        }
        for (names, arity) in self.unions.borrow().iter() {
            let total: usize = arity.iter().sum();
            let ps: Vec<String> = (0..total).map(|i| format!("T{}", i)).collect();
            let generics = if total == 0 { String::new() } else { format!("<{}>", ps.join(", ")) };
            out.push_str(&format!("#[derive(Clone, PartialEq, Debug)]\npub enum {}{} {{\n", union_name(names), generics));
            let mut k = 0;
            for (n, a) in names.iter().zip(arity) {
                if *a == 0 {
                    out.push_str(&format!("    {},\n", n));
                } else {
                    let xs: Vec<String> = (k..k + a).map(|i| format!("T{}", i)).collect();
                    k += a;
                    out.push_str(&format!("    {}({}),\n", n, xs.join(", ")));
                }
            }
            out.push_str("}\n\n");
            if names == &["After", "Before", "Same"] {
                out.push_str("impl RocOrder for Tags_After_Before_Same {\n    fn order(&self) -> std::cmp::Ordering {\n        match self {\n            Tags_After_Before_Same::Before => std::cmp::Ordering::Less,\n            Tags_After_Before_Same::Same => std::cmp::Ordering::Equal,\n            Tags_After_Before_Same::After => std::cmp::Ordering::Greater,\n        }\n    }\n}\n\n");
            }
        }
        Ok(out)
    }

    // ---- definitions ----

    fn def(&self, module: Option<&str>, name: &str, annotation: Option<&Type>, value: &Expr) -> Result<String, String> {
        let rust = sanitize(name);
        let owner = name.rsplit_once('.').map(|(o, _)| o.to_string()).unwrap_or_else(|| module.unwrap_or("").to_string());
        let mut scope = Scope { locals: Vec::new(), owner, module: module.map(|m| m.to_string()), generics: Vec::new(), in_loop: None };
        let Some(sig) = annotation.cloned().or_else(|| self.types.get(&value.id()).cloned()) else {
            return Err(format!("`{}` has no type", name));
        };
        let mut renames = HashMap::new();
        if let Some(body) = self.types.get(&value.id()) {
            bind(body, &sig, &mut renames);
            renames.retain(|v, t| *t != Type::TypeVar(*v));
        }
        *self.renames.borrow_mut() = renames;
        match value {
            Expr::Lambda { params, body, .. } => {
                let Some((ps, result)) = peel(&sig, params.len()) else {
                    return Err(format!("`{}`'s type has fewer parameters than its lambda", name));
                };
                let mut vars = Vec::new();
                vars_of(&sig, &mut vars);
                // `main!`'s arguments are the platform's strings, whatever the
                // checker left their type as, so it has no type parameters.
                if name == "main!" {
                    vars.clear();
                }
                let generics = generic_list(&vars);
                scope.generics = vars;
                let mut args = Vec::new();
                for (p, t) in params.iter().zip(&ps) {
                    let t = if name == "main!" { "List<String>".to_string() } else { self.ty(t)? };
                    args.push(format!("{}: {}", sanitize(p), t));
                    scope.locals.push(p.to_string());
                }
                // `|_args|` on `main!` is the platform's; it is unused here.
                let result_ty = self.ty(&result)?;
                let body = self.expr(body, &scope)?;
                Ok(format!("pub fn {}{}({}) -> {} {{\n    {}\n}}\n", rust, generics, args.join(", "), result_ty, indent(&body, 4)))
            }
            _ => {
                let mut vars = Vec::new();
                vars_of(&sig, &mut vars);
                let generics = generic_list(&vars);
                scope.generics = vars;
                let t = self.ty(&sig)?;
                let body = self.expr(value, &scope)?;
                if !generics.is_empty() {
                    return Ok(format!("pub fn {}{}() -> {} {{\n    {}\n}}\n", rust, generics, t, indent(&body, 4)));
                }
                // A constant is computed once, as roc does: `Routes.forward`, a table of
                // every walk, was rebuilt at each lookup.
                Ok(format!(
                    "pub fn {}() -> {} {{\n    thread_local! {{ static ONCE: std::cell::OnceCell<{}> = std::cell::OnceCell::new(); }}\n    ONCE.with(|once| once.get_or_init(|| {}).clone())\n}}\n",
                    rust, t, t, indent(&body, 4)
                ))
            }
        }
    }

    // ---- names ----

    /// A bare name in scope: a local, or a top-level definition looked up under
    /// the owner, the module, and then the app.
    fn resolve(&self, n: &str, scope: &Scope) -> Option<String> {
        let candidates = [
            format!("{}.{}", scope.owner, n),
            scope.module.as_ref().map(|m| format!("{}.{}", m, n)).unwrap_or_default(),
            n.to_string(),
        ];
        let found = candidates.into_iter().find(|c| !c.is_empty() && self.tops.contains_key(c));
        if let Some(f) = &found {
            self.wanted.borrow_mut().insert(f.clone());
        }
        found
    }

    fn qualified(&self, module: &str, name: &str) -> Option<String> {
        let q = format!("{}.{}", module, name);
        let found = self.tops.contains_key(&q).then_some(q);
        if let Some(f) = &found {
            self.wanted.borrow_mut().insert(f.clone());
        }
        found
    }

    /// A reference to a top-level definition as a value.
    fn top_value(&self, roc: &str, e: &Expr, scope: &Scope) -> Result<String, String> {
        let rust = sanitize(roc);
        if self.tops[roc] {
            // A function used as a value: a closure over it, typed by the use.
            let t = self.ty_of(e)?;
            let Some((ps, _)) = peel_all(&t) else {
                return Err(format!("`{}` used as a value of type {}", roc, t));
            };
            let names: Vec<String> = (0..ps.len()).map(|i| format!("a{}", i)).collect();
            let typed: Result<Vec<String>, String> = names.iter().zip(&ps).map(|(n, t)| Ok(format!("{}: {}", n, erase_free(&self.ty(t)?, &scope.generics)))).collect();
            Ok(format!("(Rc::new(move |{}| {}({})) as {})", typed?.join(", "), rust, names.join(", "), erase_free(&self.ty(&t)?, &scope.generics)))
        } else {
            Ok(format!("{}()", rust))
        }
    }

    // ---- expressions ----

    fn expr(&self, e: &Expr, scope: &Scope) -> Result<String, String> {
        Ok(match e {
            Expr::Int(n, _) => self.int_lit(*n, e),
            Expr::Float(f, _, _) => format!("{:?}f64", f),
            Expr::Bool(b, _) => b.to_string(),
            Expr::Unit(_) => "()".into(),
            Expr::Str(s, _) => {
                let lit = format!("String::from({:?})", s);
                match self.ty_of(e)? {
                    Type::Nominal { name, .. } => {
                        let from = self.qualified(bare(name), "from_quote").ok_or_else(|| format!("a string literal as a {} with no from_quote", name))?;
                        format!("{}({}).unwrap()", sanitize(&from), lit)
                    }
                    _ => lit,
                }
            }
            Expr::Ident(n, _) => {
                if scope.local(n) {
                    self.read_local(n, e)?
                } else if let Some(top) = self.resolve(n, scope) {
                    self.top_value(&top, e, scope)?
                } else {
                    return Err(format!("the name `{}`", n));
                }
            }
            Expr::Qualified { module, name, .. } => match self.qualified(module, name) {
                Some(top) => self.top_value(&top, e, scope)?,
                None => return Err(format!("`{}.{}` as a value", module, name)),
            },
            Expr::BinOp { left, op, right, .. } => self.binop(left, *op, right, scope)?,
            Expr::Call { func, args, .. } => self.call(func, args, e, scope)?,
            Expr::If { condition, then_branch, otherwise, .. } => self.branching(condition, then_branch, otherwise, &self.ty_of(e).ok(), scope)?,
            Expr::Let { .. } | Expr::VarDecl { .. } | Expr::Assign { .. } => self.block(e, scope)?,
            Expr::While { condition, body, .. } => {
                let (label, inner) = self.loop_scope(scope);
                format!("{{ {}: while {} {{ {}; }} }}", label, self.expr(condition, scope)?, self.expr(body, &inner)?)
            }
            Expr::For { name, iterable, body, .. } => {
                if let Expr::Range { start, end, inclusive, .. } = &**iterable {
                    let (label, mut inner) = self.loop_scope(scope);
                    inner.locals.push(name.to_string());
                    return Ok(format!(
                        "{{ {}: for {} in ({}){}({}) {{ {}; }} }}",
                        label,
                        sanitize(name),
                        self.expr(start, scope)?,
                        if *inclusive { "..=" } else { ".." },
                        self.expr(end, scope)?,
                        self.expr(body, &inner)?
                    ));
                }
                if !matches!(self.ty_of(iterable)?, Type::List(_)) {
                    return Err(format!("a for over {}", short(iterable)));
                }
                let (label, mut inner) = self.loop_scope(scope);
                inner.locals.push(name.to_string());
                format!(
                    "{{ let __it = {}; {}: for {} in __it.0.iter().cloned() {{ {}; }} }}",
                    self.expr(iterable, scope)?,
                    label,
                    sanitize(name),
                    self.expr(body, &inner)?
                )
            }
            Expr::Break(_) => match &scope.in_loop {
                Some(label) => format!("break {}", label),
                None => return Err("a break outside a loop".into()),
            },
            // Inside a lambda, `return` leaves the lambda, as a Rust closure's does.
            Expr::Return(value, _) => format!("return {}", self.expr(value, scope)?),
            Expr::Expect(cond, _) => format!(
                "{{ if !({}) {{ eprintln!(\"expect failed: {}\"); }} }}",
                self.expr(cond, scope)?,
                short(cond).replace('\\', "\\\\").replace('"', "\\\"").replace('{', "{{").replace('}', "}}")
            ),
            Expr::Dbg(value, _) => format!("{{ let __d = {}; eprintln!(\"{{:?}}\", __d); __d }}", self.expr(value, scope)?),
            Expr::StrInterp(parts, _) => {
                let mut out = String::from("{ let mut __t = String::new();");
                for part in parts {
                    match part {
                        StrPart::Literal(l) => out.push_str(&format!(" __t.push_str({:?});", l)),
                        StrPart::Expr(x) => {
                            if !matches!(self.ty_of(x)?, Type::Str) {
                                return Err(format!("an interpolation of {}", short(x)));
                            }
                            out.push_str(&format!(" __t.push_str(&{});", self.expr(x, scope)?))
                        }
                    }
                }
                out.push_str(" __t }");
                out
            }
            Expr::Match { scrutinee, arms, .. } => self.matching(scrutinee, arms, &self.ty_of(e).ok(), scope)?,
            Expr::List(items, _) => {
                let xs: Result<Vec<String>, String> = items.iter().map(|i| self.expr(i, scope)).collect();
                format!("List::of(vec![{}])", xs?.join(", "))
            }
            Expr::Tuple(items, _) => {
                let xs: Result<Vec<String>, String> = items.iter().map(|i| self.expr(i, scope)).collect();
                format!("({},)", xs?.join(", "))
            }
            Expr::TupleIndex { tuple, index, .. } => format!("({}).{}", self.expr(tuple, scope)?, index),
            Expr::Record(fields, _) => self.record(fields, e, scope)?,
            Expr::RecordUpdate { base, fields, .. } => {
                let t = self.ty_of(e)?;
                let mut out = format!("{{ let mut __r = {};", self.expr(base, scope)?);
                for (f, v) in fields {
                    let (_, boxed) = self.field_type(&t, f)?;
                    let v = self.expr(v, scope)?;
                    let v = if boxed { format!("Rc::new({})", v) } else { v };
                    out.push_str(&format!(" __r.{} = {};", sanitize(f), v));
                }
                out.push_str(" __r }");
                out
            }
            Expr::FieldAccess { record, field, .. } => {
                let rt = self.ty_of(record)?;
                let (_, boxed) = self.field_type(&rt, field)?;
                let r = self.expr(record, scope)?;
                if boxed {
                    format!("(*({}).{}).clone()", r, sanitize(field))
                } else {
                    format!("({}).{}", r, sanitize(field))
                }
            }
            Expr::Tag { name, args, .. } => self.tag(name, args, e, scope)?,
            Expr::Lambda { params, body, .. } => self.lambda(params, body, e, scope)?,
            Expr::Crash(msg, _) => format!("panic!(\"{{}}\", {})", self.expr(msg, scope)?),
            Expr::Dispatch { receiver, method: "negate", args, .. } if args.is_empty() => format!("(-({}))", self.expr(receiver, scope)?),
            Expr::Dispatch { receiver, method: "not", args, .. } if args.is_empty() => format!("(!({}))", self.expr(receiver, scope)?),
            Expr::Dispatch { receiver, method, args, .. } => {
                // `xs.concat(ys)` is `List.concat(xs, ys)`: the receiver's type names
                // the module, and the receiver is the first argument.
                let rt = self.ty_of(receiver)?;
                let module: &'static str = match &rt {
                    Type::List(_) => "List",
                    Type::Str => "Str",
                    Type::I64 => "I64",
                    Type::U64 => "U64",
                    Type::U8 => "U8",
                    Type::I32 => "I32",
                    Type::U32 => "U32",
                    Type::U16 => "U16",
                    Type::F64 => "F64",
                    Type::Nominal { name, .. } => crate::memory::string_pool::intern(bare(name)),
                    other => return Err(format!("a method {} of {}", method, other)),
                };
                let func = Expr::Qualified { module, name: method, id: crate::ast::fresh_node_unlocated() };
                let mut all = vec![(**receiver).clone()];
                all.extend(args.iter().cloned());
                self.call(&func, &all, e, scope)?
            }
            other => return Err(format!("an expression {}", short(other))),
        })
    }

    /// A new loop's label, and the scope its body is written in.
    fn loop_scope(&self, scope: &Scope) -> (String, Scope) {
        let mut n = self.loops.borrow_mut();
        *n += 1;
        let label = format!("'l{}", n);
        let mut inner = scope.clone();
        inner.in_loop = Some(label.clone());
        (label, inner)
    }

    fn read_local(&self, n: &str, e: &Expr) -> Result<String, String> {
        let t = self.ty_of(e)?;
        let moved = *self.moving.borrow() == Some(e.id());
        Ok(if copy_type(&t) || moved { sanitize(n) } else { format!("{}.clone()", sanitize(n)) })
    }

    fn int_lit(&self, n: i128, e: &Expr) -> String {
        self.int_for(n, &self.ty_of(e).unwrap_or(Type::I64))
    }

    /// A statement chain: `let`s, `var`s and assignments, as a Rust block.
    fn block(&self, e: &Expr, scope: &Scope) -> Result<String, String> {
        let mut scope = scope.clone();
        let mut out = String::from("{\n");
        let mut cursor = e;
        loop {
            match cursor {
                Expr::Let { name, value, body, .. } => {
                    let at_use = self.at_use(name, value, body, &scope)?;
                    let saved = self.renames.borrow().clone();
                    self.renames.borrow_mut().extend(at_use);
                    let v = self.expr(value, &scope);
                    *self.renames.borrow_mut() = saved;
                    let v = v?;
                    if *name == "_" {
                        out.push_str(&format!("    let _ = {};\n", indent(&v, 4)));
                    } else {
                        out.push_str(&format!("    let {} = {};\n", sanitize(name), indent(&v, 4)));
                        scope.locals.push(name.to_string());
                    }
                    cursor = body;
                }
                Expr::VarDecl { name, value, body, .. } => {
                    let v = self.expr(value, &scope)?;
                    out.push_str(&format!("    let mut {} = {};\n", sanitize(name), indent(&v, 4)));
                    scope.locals.push(name.to_string());
                    cursor = body;
                }
                Expr::Assign { name, value, body, .. } => {
                    *self.moving.borrow_mut() = self_update(name, value);
                    let v = self.expr(value, &scope);
                    *self.moving.borrow_mut() = None;
                    let v = v?;
                    out.push_str(&format!("    {} = {};\n", sanitize(name), indent(&v, 4)));
                    cursor = body;
                }
                other => {
                    out.push_str(&format!("    {}\n}}", indent(&self.expr(other, &scope)?, 4)));
                    return Ok(out);
                }
            }
        }
    }

    /// A local function's type variables, as its uses bind them. Roc generalizes
    /// `ascending = |xs| List.sort_with(xs, ..)` over its element; a Rust closure
    /// has one type, which is the one it is used at. Used at two, it is refused.
    fn at_use(&self, name: &str, value: &Expr, body: &Expr, scope: &Scope) -> Result<HashMap<u32, Type>, String> {
        let mut out = HashMap::new();
        if !matches!(value, Expr::Lambda { .. }) {
            return Ok(out);
        }
        let t = self.ty_of(value)?;
        let mut free = Vec::new();
        vars_of(&t, &mut free);
        free.retain(|v| !scope.generics.contains(v));
        if free.is_empty() {
            return Ok(out);
        }
        let mut uses = Vec::new();
        each_ident(body, &mut |n, node| if n == name { uses.push(self.ty_of(node)) });
        for used in uses {
            let mut bound = HashMap::new();
            bind(&t, &used?, &mut bound);
            for v in &free {
                let Some(b) = bound.remove(v) else { continue };
                match out.get(v) {
                    Some(earlier) if *earlier != b => {
                        return Err(format!("the local function `{}` is used at two types, {} and {}; a Rust closure has one", name, earlier, b));
                    }
                    _ => {
                        out.insert(*v, b);
                    }
                }
            }
        }
        Ok(out)
    }

    fn binop(&self, l: &Expr, op: BinOp, r: &Expr, scope: &Scope) -> Result<String, String> {
        let a = self.expr(l, scope)?;
        let b = self.expr(r, scope)?;
        let sym = match op {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div | BinOp::IntDiv => "/",
            BinOp::Rem => "%",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::And => "&&",
            BinOp::Or => "||",
            BinOp::Eq | BinOp::Ne => {
                // A nominal's own equality where it has one; derived otherwise.
                let t = self.ty_of(l)?;
                if let Type::Nominal { name, .. } = &t {
                    if let Some(eq) = self.qualified(bare(name), "is_eq") {
                        let call = format!("{}({}, {})", sanitize(&eq), a, b);
                        return Ok(if matches!(op, BinOp::Ne) { format!("(!{})", call) } else { call });
                    }
                }
                if matches!(op, BinOp::Eq) { "==" } else { "!=" }
            }
        };
        Ok(format!("({} {} {})", a, sym, b))
    }

    fn call(&self, func: &Expr, args: &[Expr], e: &Expr, scope: &Scope) -> Result<String, String> {
        let xs: Result<Vec<String>, String> = args.iter().map(|a| self.expr(a, scope)).collect();
        let xs = xs?;
        let callee = match func {
            Expr::Ident(n, _) if !scope.local(n) => {
                if *n == "echo!" {
                    return Ok(format!("echo({})", xs.join(", ")));
                }
                match self.resolve(n, scope) {
                    Some(top) if self.tops[&top] => format!("{}{}", sanitize(&top), self.turbofish(&top, func, scope)?),
                    Some(top) => format!("({})", self.top_value(&top, func, scope)?),
                    None => return Err(format!("a call to `{}`", n)),
                }
            }
            Expr::Qualified { module, name, .. } => match self.qualified(module, name) {
                Some(top) if self.tops[&top] => format!("{}{}", sanitize(&top), self.turbofish(&top, func, scope)?),
                Some(top) => format!("({})", self.top_value(&top, func, scope)?),
                // The platform's one effect: a line out, its newline written for it.
                None if *module == "Echo" && *name == "line!" => return Ok(format!("echo_line({})", xs.join(", "))),
                None if *module == "Try" && *name == "is_ok" && xs.len() == 1 => return Ok(format!("({}).is_ok()", xs[0])),
                None if *module == "Try" && *name == "map_ok" && xs.len() == 2 => return Ok(format!("({}).map(|__x| (*{})(__x))", xs[0], xs[1])),
                None if BUILTIN_MODULES.contains(module) => {
                    if matches!(*module, "List" | "Str") {
                        // A builtin's `Err` is `()` in runtime.rs; the program's is the
                        // tag it names, in the program's own union.
                        if let Some(tag) = builtin_err_tag(module, name) {
                            return Ok(format!("{}__{}({}){}", module, name, xs.join(", "), self.err_as(e, tag)?));
                        }
                        format!("{}__{}", module, name)
                    } else {
                        format!("{}::{}", module, name)
                    }
                }
                None => return Err(format!("a call to `{}.{}`", module, name)),
            },
            other => format!("(*{})", self.expr(other, scope)?),
        };
        let _ = e;
        Ok(format!("{}({})", callee, xs.join(", ")))
    }

    /// `.map_err(..)` turning a builtin's `Err(())` into this call's `Err(tag)`.
    fn err_as(&self, e: &Expr, tag: &str) -> Result<String, String> {
        let t = self.ty_of(e)?;
        let Type::TagUnion { tags, .. } = &t else { return Err(format!("a builtin answering {}", t)) };
        let err = tags.iter().find(|(n, _)| *n == "Err").map(|(_, p)| p.clone()).unwrap_or_default();
        match err.as_slice() {
            [] => Ok(String::new()),
            [et] if matches!(et, Type::Unit) || matches!(et, Type::TypeVar(_)) => Ok(String::new()),
            [et] => {
                let (enum_name, _) = self.variant(et, tag)?;
                Ok(format!(".map_err(|_| {}::{})", enum_name, tag))
            }
            _ => Err(format!("a builtin's Err of {}", t)),
        }
    }

    /// A field's type at this use, and whether the definition holds it behind an
    /// `Rc`: a nominal field of a nominal record is; a structural record's field is
    /// a type parameter, never.
    /// A generic function's type arguments at a call, from the use's type: Roc
    /// lets a variable stay unconstrained (`List.len(make_empty(0))`) and Rust
    /// does not, so one nothing binds is `()`.
    fn turbofish(&self, top: &str, func: &Expr, scope: &Scope) -> Result<String, String> {
        let Some(sig) = self.sig_of(top) else { return Ok(String::new()) };
        let mut vars = Vec::new();
        vars_of(&sig, &mut vars);
        if vars.is_empty() {
            return Ok(String::new());
        }
        // A qualified callee has no type of its own recorded; rustc infers there.
        let Some(used) = self.types.get(&func.id()).cloned() else { return Ok(String::new()) };
        let mut bound = HashMap::new();
        bind(&sig, &used, &mut bound);
        let mut args = Vec::new();
        for v in &vars {
            let t = bound.get(v).cloned().unwrap_or(Type::Unit);
            // A numeral the checker defaulted to a fraction: rustc types it with
            // the unsuffixed literal it is written as.
            if matches!(t, Type::Dec) {
                args.push("_".into());
                continue;
            }
            let mut free = Vec::new();
            vars_of(&t, &mut free);
            // In a function with no type parameters a leftover variable is
            // unconstrained, so `()`; inside a generic one it may be one of that
            // function's own, under the checker's id, so rustc infers it.
            if free.iter().any(|f| !scope.generics.contains(f)) && !scope.generics.is_empty() {
                args.push(erase_free(&self.ty(&t)?, &scope.generics));
            } else {
                let t = if free.iter().any(|f| !scope.generics.contains(f)) { subst_free(&t, &scope.generics) } else { t };
                args.push(self.ty(&t)?);
            }
        }
        Ok(format!("::<{}>", args.join(", ")))
    }

    /// A top-level definition's declared type: its annotation, or its value's type.
    fn sig_of(&self, top: &str) -> Option<Type> {
        for ast in self.input.modules.iter().map(|m| m.ast).chain(std::iter::once(self.input.app)) {
            let mut cursor = ast;
            while let Expr::Let { name, annotation, value, body, .. } = cursor {
                if *name == top {
                    return annotation.clone().or_else(|| self.types.get(&value.id()).cloned());
                }
                cursor = body;
            }
        }
        None
    }

    fn field_type(&self, t: &Type, field: &str) -> Result<(Type, bool), String> {
        match t {
            Type::Record { fields, .. } => fields.iter().find(|(f, _)| *f == field).map(|(_, t)| (t.clone(), false)).ok_or_else(|| format!("no field {} in {}", field, t)),
            Type::Nominal { name, backing } => {
                let n = self.nominals.get(bare(name)).ok_or_else(|| format!("the nominal {}", name))?;
                let decl = match &n.backing {
                    Type::Record { fields, .. } => fields.iter().find(|(f, _)| *f == field).map(|(_, t)| t.clone()),
                    _ => None,
                }
                .ok_or_else(|| format!("no field {} in {}", field, name))?;
                let mut bound = HashMap::new();
                bind(&n.backing, backing, &mut bound);
                Ok((subst(&decl, &bound), self.boxed(&decl)))
            }
            other => Err(format!("a field {} of {}", field, other)),
        }
    }

    fn record(&self, fields: &[(&'static str, Expr)], e: &Expr, scope: &Scope) -> Result<String, String> {
        let t = self.ty_of(e)?;
        let name = match &t {
            Type::Nominal { name, .. } => self.nominals.get(bare(name)).map(|n| n.rust.clone()).ok_or_else(|| format!("the nominal {}", name))?,
            // Typed only by its shape (a let-bound literal can be): the one nominal
            // record with exactly these fields, where there is one, since rocemit
            // writes every record type without parameters as a nominal.
            Type::Record { fields: tf, .. } => match self.nominal_record(tf) {
                Some((rust, nt)) => return self.record_as(&rust, &nt, fields, scope),
                None => {
                    let s = self.ty(&t)?;
                    s.split('<').next().unwrap_or(&s).to_string()
                }
            },
            other => return Err(format!("a record literal typed {}", other)),
        };
        let mut parts = Vec::new();
        for (f, v) in fields {
            let (_, boxed) = self.field_type(&t, f)?;
            let v = self.expr(v, scope)?;
            parts.push(format!("{}: {}", sanitize(f), if boxed { format!("Rc::new({})", v) } else { v }));
        }
        Ok(format!("{} {{ {} }}", name, parts.join(", ")))
    }

    /// The one nominal record whose fields are exactly these.
    fn nominal_record(&self, fields: &[(&'static str, Type)]) -> Option<(String, Type)> {
        let want = sorted_names(fields.iter().map(|(n, _)| *n));
        let mut found = self.nominals.iter().filter(|(_, n)| {
            matches!(&n.backing, Type::Record { fields: f, .. } if sorted_names(f.iter().map(|(x, _)| *x)) == want) && n.params.is_empty()
        });
        let (name, n) = found.next()?;
        if found.next().is_some() {
            return None;
        }
        let nt = Type::Nominal { name: crate::memory::string_pool::intern(name), backing: Box::new(n.backing.clone()) };
        Some((n.rust.clone(), nt))
    }

    fn record_as(&self, rust: &str, t: &Type, fields: &[(&'static str, Expr)], scope: &Scope) -> Result<String, String> {
        let mut parts = Vec::new();
        for (f, v) in fields {
            let (_, boxed) = self.field_type(t, f)?;
            let v = self.expr(v, scope)?;
            parts.push(format!("{}: {}", sanitize(f), if boxed { format!("Rc::new({})", v) } else { v }));
        }
        Ok(format!("{} {{ {} }}", rust, parts.join(", ")))
    }

    fn branching(&self, condition: &Expr, then_branch: &Expr, otherwise: &Expr, whole: &Option<Type>, scope: &Scope) -> Result<String, String> {
        Ok(format!(
            "(if {} {{\n    {}\n}} else {{\n    {}\n}})",
            self.expr(condition, scope)?,
            indent(&self.result(then_branch, whole, scope)?, 4),
            indent(&self.result(otherwise, whole, scope)?, 4)
        ))
    }

    /// An arm's or a branch's value. A lone tag there (`Solo =>` beside
    /// `Anytime => Partner(p)`) knows only itself, an open `[Solo, ..]`, and a
    /// nested `if` or `match` knows only its own tags; each is a value of the
    /// whole `match` or `if`, whose type holds every arm's tags.
    fn result(&self, body: &Expr, whole: &Option<Type>, scope: &Scope) -> Result<String, String> {
        match body {
            Expr::Tag { name, args, .. } => {
                if let (Type::TagUnion { open: true, .. }, Some(w @ Type::TagUnion { tags, .. })) = (self.ty_of(body)?, whole) {
                    if tags.iter().any(|(n, _)| n == name) {
                        return self.tag_typed(name, args, w, scope);
                    }
                }
                self.expr(body, scope)
            }
            Expr::If { condition, then_branch, otherwise, .. } => self.branching(condition, then_branch, otherwise, whole, scope),
            Expr::Match { scrutinee, arms, .. } => self.matching(scrutinee, arms, whole, scope),
            _ => self.expr(body, scope),
        }
    }

    fn tag(&self, name: &str, args: &[Expr], e: &Expr, scope: &Scope) -> Result<String, String> {
        let t = self.ty_of(e)?;
        self.tag_typed(name, args, &t, scope)
    }

    fn tag_typed(&self, name: &str, args: &[Expr], t: &Type, scope: &Scope) -> Result<String, String> {
        let t = t.clone();
        let xs: Result<Vec<String>, String> = args.iter().map(|a| self.expr(a, scope)).collect();
        let xs = xs?;
        match &t {
            Type::Bool => return Ok((name == "True").to_string()),
            // `[Ok, Err]` is `Result`; so is a lone `Ok(..)` or `Err(..)`, whose
            // union knows only its own tag.
            Type::TagUnion { tags, .. }
                if sorted_names(tags.iter().map(|(n, _)| *n)) == ["Err", "Ok"] || (matches!(name, "Ok" | "Err") && tags.len() == 1) =>
            {
                let payload = match xs.as_slice() {
                    [] => "()".to_string(),
                    [one] => one.clone(),
                    many => format!("({})", many.join(", ")),
                };
                return Ok(format!("{}({})", name, payload));
            }
            _ => {}
        }
        let (enum_name, payload_types) = self.variant(&t, name)?;
        if xs.is_empty() {
            return Ok(format!("{}::{}", enum_name, name));
        }
        let boxed: Vec<String> = xs
            .into_iter()
            .zip(payload_types.iter())
            .map(|(x, (_, boxed))| if *boxed { format!("Rc::new({})", x) } else { x })
            .collect();
        Ok(format!("{}::{}({})", enum_name, name, boxed.join(", ")))
    }

    /// The Rust enum a tag belongs to, and that tag's payload types as declared.
    fn variant(&self, t: &Type, tag: &str) -> Result<(String, Vec<(Type, bool)>), String> {
        match t {
            Type::TagUnion { tags, .. } => {
                let s = self.ty(t)?;
                let p = tags.iter().find(|(n, _)| *n == tag).map(|(_, p)| p.iter().map(|x| (x.clone(), false)).collect()).unwrap_or_default();
                Ok((s.split('<').next().unwrap_or(&s).to_string(), p))
            }
            Type::Nominal { name, backing } => {
                let n = self.nominals.get(bare(name)).ok_or_else(|| format!("the nominal {}", name))?;
                let Type::TagUnion { tags, .. } = &n.backing else {
                    return Err(format!("a tag {} of the nominal {}", tag, name));
                };
                let mut bound = HashMap::new();
                bind(&n.backing, backing, &mut bound);
                let p = tags.iter().find(|(x, _)| *x == tag).map(|(_, p)| p.iter().map(|x| (subst(x, &bound), self.boxed(x))).collect()).unwrap_or_default();
                Ok((n.rust.clone(), p))
            }
            other => Err(format!("a tag {} of {}", tag, other)),
        }
    }

    fn lambda(&self, params: &[&'static str], body: &Expr, e: &Expr, scope: &Scope) -> Result<String, String> {
        let t = self.ty_of(e)?;
        let (ps, result) = peel(&t, params.len()).ok_or("a lambda whose type is not a function")?;
        let mut inner = scope.clone();
        inner.in_loop = None;
        let mut typed = Vec::new();
        for (p, pt) in params.iter().zip(&ps) {
            typed.push(format!("{}: {}", sanitize(p), erase_free(&self.ty(pt)?, &scope.generics)));
            inner.locals.push(p.to_string());
        }
        let b = self.expr(body, &inner)?;
        // Captures are cloned in, so the closure owns them and the caller keeps its own.
        let mut used = BTreeSet::new();
        names_in(body, &mut used);
        let captures: Vec<String> = scope.locals.iter().filter(|l| used.contains(l.as_str()) && !params.contains(&l.as_str())).map(|l| sanitize(l)).collect();
        let lets: String = captures.iter().map(|c| format!("let {c} = {c}.clone(); ")).collect();
        Ok(format!("{{ {}(Rc::new(move |{}| -> {} {{ {} }}) as {}) }}", lets, typed.join(", "), erase_free(&self.ty(&result)?, &scope.generics), b, erase_free(&self.ty(&t)?, &scope.generics)))
    }

    // ---- matching ----

    fn matching(&self, scrutinee: &Expr, arms: &[MatchArm], whole: &Option<Type>, scope: &Scope) -> Result<String, String> {
        let st = self.ty_of(scrutinee)?;
        let mut out = format!("{{ let __s = {}; 'm: {{\n", self.expr(scrutinee, scope)?);
        for arm in arms {
            for p in &arm.patterns {
                let mut inner = scope.clone();
                let mut binds = Vec::new();
                let tail = {
                    let mut probe = inner.clone();
                    self.pat_names(p, &mut probe.locals);
                    let body = self.result(&arm.body, whole, &probe)?;
                    match &arm.guard {
                        Some(g) => format!("if {} {{ break 'm ({}); }}", self.expr(g, &probe)?, body),
                        None => format!("break 'm ({});", body),
                    }
                };
                let code = self.pat(p, "(&__s)", &st, &mut binds, &mut inner, tail)?;
                out.push_str(&format!("    {}\n", indent(&code, 4)));
            }
        }
        out.push_str("    panic!(\"no match arm applied\")\n} }");
        Ok(out)
    }

    fn pat_names(&self, p: &Pattern, out: &mut Vec<String>) {
        match p {
            Pattern::Binding(n) => out.push(n.to_string()),
            Pattern::As { name, inner } => {
                out.push(name.to_string());
                self.pat_names(inner, out)
            }
            Pattern::Tag { args, .. } | Pattern::Tuple(args) => args.iter().for_each(|a| self.pat_names(a, out)),
            Pattern::Record { fields, rest } => {
                fields.iter().for_each(|(_, a)| self.pat_names(a, out));
                if let Some(r) = rest {
                    out.push(r.to_string())
                }
            }
            Pattern::List { before, rest, after } => {
                before.iter().chain(after).for_each(|a| self.pat_names(a, out));
                if let Some(Some(r)) = rest {
                    out.push(r.to_string())
                }
            }
            Pattern::Nominal { inner, .. } => self.pat_names(inner, out),
            _ => {}
        }
    }

    /// `v` is an expression of type `&T`, `t` is `T`. Wraps `inner` in what must
    /// hold for `p` to match, with its bindings made.
    fn pat(&self, p: &Pattern, v: &str, t: &Type, binds: &mut Vec<String>, scope: &mut Scope, inner: String) -> Result<String, String> {
        let k = binds.len();
        Ok(match p {
            Pattern::Wildcard => inner,
            Pattern::Binding(n) => format!("let {} = ({}).clone(); {}", sanitize(n), v, inner),
            Pattern::As { name, inner: sub } => {
                let rest = self.pat(sub, v, t, binds, scope, inner)?;
                format!("let {} = ({}).clone(); {}", sanitize(name), v, rest)
            }
            Pattern::Int(n) => format!("if *{} == {} {{ {} }}", v, self.int_for(*n, t), inner),
            Pattern::Str(s) => {
                let lit = match t {
                    Type::Nominal { name, .. } => {
                        let from = self.qualified(bare(name), "from_quote").ok_or("a string pattern with no from_quote")?;
                        format!("{}(String::from({:?})).unwrap()", sanitize(&from), s)
                    }
                    _ => format!("String::from({:?})", s),
                };
                format!("if *{} == {} {{ {} }}", v, lit, inner)
            }
            Pattern::Nominal { inner: sub, .. } => {
                let backing = match t {
                    Type::Nominal { name, backing } => {
                        let n = self.nominals.get(bare(name)).ok_or("an unknown nominal pattern")?;
                        self.instantiate(n, backing)
                    }
                    other => other.clone(),
                };
                self.pat(sub, v, &backing, binds, scope, inner)?
            }
            Pattern::Tuple(items) => {
                let Type::Tuple(ts) = t else { return Err(format!("a tuple pattern on {}", t)) };
                let mut code = inner;
                for (i, (item, it)) in items.iter().zip(ts).enumerate().rev() {
                    code = self.pat(item, &format!("(&({}).{})", v, i), it, binds, scope, code)?;
                }
                code
            }
            Pattern::Tag { name, args } => {
                if matches!(t, Type::Bool) {
                    return Ok(format!("if *{} == {} {{ {} }}", v, *name == "True", inner));
                }
                let names: Vec<String> = (0..args.len()).map(|i| format!("__p{}_{}", k, i)).collect();
                for n in &names {
                    binds.push(n.clone());
                }
                let is_result = matches!(t, Type::TagUnion { tags, .. } if sorted_names(tags.iter().map(|(n, _)| *n)) == ["Err", "Ok"]);
                let (head, pts): (String, Vec<(Type, bool)>) = if is_result {
                    let Type::TagUnion { tags, .. } = t else { unreachable!() };
                    (name.to_string(), tags.iter().find(|(n, _)| n == name).map(|(_, p)| p.iter().map(|x| (x.clone(), false)).collect()).unwrap_or_default())
                } else {
                    let (e, pts) = self.variant(t, name)?;
                    (format!("{}::{}", e, name), pts)
                };
                let mut code = inner;
                if is_result && args.len() > 1 {
                    return Err("a Result tag with several payloads".into());
                }
                for (i, (a, (pt, boxed))) in args.iter().zip(&pts).enumerate().rev() {
                    let access = if *boxed { format!("(&**{})", names[i]) } else { names[i].clone() };
                    code = self.pat(a, &access, pt, binds, scope, code)?;
                }
                let bind = if args.is_empty() { String::new() } else { format!("({})", names.join(", ")) };
                let head = if is_result && args.is_empty() { format!("{}(_)", head) } else { format!("{}{}", head, bind) };
                format!("if let {} = {} {{ {} }}", head, v, code)
            }
            Pattern::Record { fields, rest } => {
                if rest.is_some() {
                    return Err("a record pattern with a rest".into());
                }
                let mut code = inner;
                for (f, sub) in fields.iter().rev() {
                    let (ft, boxed) = self.field_type(t, f)?;
                    let access = if boxed { format!("(&*({}).{})", v, sanitize(f)) } else { format!("(&({}).{})", v, sanitize(f)) };
                    code = self.pat(sub, &access, &ft, binds, scope, code)?;
                }
                code
            }
            Pattern::List { before, rest, after } => {
                let Type::List(et) = t else { return Err(format!("a list pattern on {}", t)) };
                let n = before.len() + after.len();
                let cond = match rest {
                    None => format!("({}).0.len() == {}", v, n),
                    Some(_) => format!("({}).0.len() >= {}", v, n),
                };
                let mut code = inner;
                if let Some(Some(r)) = rest {
                    if !after.is_empty() {
                        return Err("a list pattern with elements after its rest".into());
                    }
                    code = format!("let {} = ({}).rest({}); {}", sanitize(r), v, before.len(), code);
                }
                for (i, b) in before.iter().enumerate().rev() {
                    code = self.pat(b, &format!("({}).at({})", v, i), et, binds, scope, code)?;
                }
                for (i, a) in after.iter().enumerate().rev() {
                    code = self.pat(a, &format!("({v}).at(({v}).0.len() - {})", after.len() - i), et, binds, scope, code)?;
                }
                format!("if {} {{ {} }}", cond, code)
            }
            other => return Err(format!("a pattern {}", other)),
        })
    }

    /// A whole-number literal of type `t`; a fraction (`Dec`) is written as the
    /// `f64` its type is.
    fn int_for(&self, n: i128, t: &Type) -> String {
        let suffix = match t {
            Type::U8 => "u8",
            Type::U16 => "u16",
            Type::U32 => "u32",
            Type::U64 => "u64",
            Type::U128 => "u128",
            Type::I8 => "i8",
            Type::I16 => "i16",
            Type::I32 => "i32",
            Type::I128 => "i128",
            Type::F64 | Type::Dec => "f64",
            Type::F32 => "f32",
            _ => "i64",
        };
        if n < 0 { format!("({}{})", n, suffix) } else { format!("{}{}", n, suffix) }
    }
}

/// The tag a builtin's `Err` carries.
fn builtin_err_tag(module: &str, name: &str) -> Option<&'static str> {
    Some(match (module, name) {
        ("List", "get") => "OutOfBounds",
        ("List", "first" | "last") => "ListWasEmpty",
        ("List", "find_first" | "find_last" | "find_first_index") => "NotFound",
        _ => return None,
    })
}

fn generic_list(vars: &[u32]) -> String {
    if vars.is_empty() {
        String::new()
    } else {
        format!("<{}>", vars.iter().map(|v| format!("T{}: Clone + PartialEq + std::fmt::Debug + 'static", v)).collect::<Vec<_>>().join(", "))
    }
}

/// A type written inside a body: a variable that is not the definition's own
/// is `_`, for rustc to infer.
fn erase_free(s: &str, generics: &[u32]) -> String {
    let b = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        let boundary = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
        if b[i] == b'T' && boundary {
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let ends = j == b.len() || !(b[j].is_ascii_alphanumeric() || b[j] == b'_');
            if j > i + 1 && ends {
                let v: u32 = s[i + 1..j].parse().unwrap_or(u32::MAX);
                out.push_str(if generics.contains(&v) { &s[i..j] } else { "_" });
                i = j;
                continue;
            }
        }
        out.push(b[i] as char);
        i += 1;
    }
    out
}

fn sorted_names<'x>(names: impl Iterator<Item = &'x str>) -> Vec<String> {
    let mut v: Vec<String> = names.map(|s| s.to_string()).collect();
    v.sort();
    v
}

fn tag_names(t: &Type) -> Vec<&'static str> {
    match t {
        Type::TagUnion { tags, .. } => tags.iter().map(|(n, _)| *n).collect(),
        _ => Vec::new(),
    }
}

fn record_name(names: &[String]) -> String {
    format!("Rec_{}", names.join("_"))
}

fn union_name(names: &[String]) -> String {
    format!("Tags_{}", names.join("_"))
}

/// A nominal Roc erases at run time and Rust can too: one over a list or a scalar.
fn erased(backing: &Type) -> bool {
    !matches!(backing, Type::Record { .. } | Type::TagUnion { .. })
}

fn holds_fn(t: &Type) -> bool {
    match t {
        Type::Function(..) => true,
        Type::List(e) => holds_fn(e),
        Type::Tuple(xs) => xs.iter().any(holds_fn),
        Type::Record { fields, .. } => fields.iter().any(|(_, x)| holds_fn(x)),
        Type::TagUnion { tags, .. } => tags.iter().flat_map(|(_, p)| p).any(holds_fn),
        _ => false,
    }
}

/// A type with every variable not among `keep` made `()`: nothing constrains it.
fn subst_free(t: &Type, keep: &[u32]) -> Type {
    let mut vars = Vec::new();
    vars_of(t, &mut vars);
    let bound: HashMap<u32, Type> = vars.into_iter().filter(|v| !keep.contains(v)).map(|v| (v, Type::Unit)).collect();
    subst(t, &bound)
}

fn subst(t: &Type, bound: &HashMap<u32, Type>) -> Type {
    match t {
        Type::TypeVar(v) => bound.get(v).cloned().unwrap_or_else(|| t.clone()),
        Type::List(e) => Type::List(Box::new(subst(e, bound))),
        Type::Function(a, b) => Type::Function(Box::new(subst(a, bound)), Box::new(subst(b, bound))),
        Type::Tuple(xs) => Type::Tuple(xs.iter().map(|x| subst(x, bound)).collect()),
        Type::Record { fields, open } => Type::Record { fields: fields.iter().map(|(n, x)| (*n, subst(x, bound))).collect(), open: *open },
        Type::TagUnion { tags, open } => Type::TagUnion { tags: tags.iter().map(|(n, p)| (*n, p.iter().map(|x| subst(x, bound)).collect())).collect(), open: *open },
        Type::Nominal { name, backing } => Type::Nominal { name, backing: Box::new(subst(backing, bound)) },
        other => other.clone(),
    }
}

/// The first `n` parameters of a curried function type, and what is left.
fn peel(t: &Type, n: usize) -> Option<(Vec<Type>, Type)> {
    let mut params = Vec::new();
    let mut cur = t;
    while params.len() < n {
        let Type::Function(a, b) = cur else { return None };
        params.push((**a).clone());
        cur = b;
    }
    Some((params, cur.clone()))
}

fn peel_all(t: &Type) -> Option<(Vec<Type>, Type)> {
    let mut params = Vec::new();
    let mut cur = t;
    while let Type::Function(a, b) = cur {
        params.push((**a).clone());
        cur = b;
    }
    (!params.is_empty()).then(|| (params, cur.clone()))
}

/// Every bare name an expression reads, for a closure's captures.
/// In `$x = f($x, ..)`, the `$x` passed on, when it is the value's only use of
/// `$x`: the assignment replaces it, so nothing reads the old one again.
fn self_update(name: &str, value: &Expr) -> Option<NodeId> {
    let first = match value {
        Expr::Call { args, .. } => args.first()?,
        Expr::Dispatch { receiver, .. } => &**receiver,
        _ => return None,
    };
    let Expr::Ident(n, id) = first else { return None };
    let mut uses = 0;
    each_ident(value, &mut |m, _| uses += (m == name) as usize);
    (*n == name && uses == 1).then_some(*id)
}

/// The names a body refers to.
fn names_in(e: &Expr, out: &mut BTreeSet<String>) {
    each_ident(e, &mut |n, _| {
        out.insert(n.to_string());
    })
}

/// Every identifier in an expression, with its node.
fn each_ident(e: &Expr, f: &mut dyn FnMut(&str, &Expr)) {
    match e {
        Expr::Ident(n, _) => f(n, e),
        Expr::BinOp { left, right, .. } => {
            each_ident(left, f);
            each_ident(right, f)
        }
        Expr::Call { func, args, .. } => {
            each_ident(func, f);
            args.iter().for_each(|a| each_ident(a, f))
        }
        Expr::Lambda { body, .. } => each_ident(body, f),
        Expr::Let { value, body, .. } | Expr::VarDecl { value, body, .. } | Expr::Assign { value, body, .. } => {
            each_ident(value, f);
            each_ident(body, f)
        }
        Expr::If { condition, then_branch, otherwise, .. } => {
            each_ident(condition, f);
            each_ident(then_branch, f);
            each_ident(otherwise, f)
        }
        Expr::Match { scrutinee, arms, .. } => {
            each_ident(scrutinee, f);
            for a in arms {
                each_ident(&a.body, f);
                if let Some(g) = &a.guard {
                    each_ident(g, f)
                }
            }
        }
        Expr::List(xs, _) | Expr::Tuple(xs, _) => xs.iter().for_each(|x| each_ident(x, f)),
        Expr::Record(fs, _) => fs.iter().for_each(|(_, x)| each_ident(x, f)),
        Expr::RecordUpdate { base, fields, .. } => {
            each_ident(base, f);
            fields.iter().for_each(|(_, x)| each_ident(x, f))
        }
        Expr::FieldAccess { record, .. } => each_ident(record, f),
        Expr::TupleIndex { tuple, .. } => each_ident(tuple, f),
        Expr::Tag { args, .. } => args.iter().for_each(|x| each_ident(x, f)),
        Expr::Crash(x, _) | Expr::Return(x, _) | Expr::Dbg(x, _) | Expr::Expect(x, _) => each_ident(x, f),
        Expr::Dispatch { receiver, args, .. } => {
            each_ident(receiver, f);
            args.iter().for_each(|x| each_ident(x, f))
        }
        Expr::For { iterable, body, .. } => {
            each_ident(iterable, f);
            each_ident(body, f)
        }
        Expr::StrInterp(parts, _) => parts.iter().for_each(|p| if let StrPart::Expr(x) = p { each_ident(x, f) }),
        Expr::While { condition, body, .. } => {
            each_ident(condition, f);
            each_ident(body, f)
        }
        _ => {}
    }
}

fn indent(s: &str, n: usize) -> String {
    s.replace('\n', &format!("\n{}", " ".repeat(n)))
}

fn short(e: &Expr) -> String {
    let s = e.to_string();
    if s.len() > 80 { format!("{}...", &s[..s.char_indices().nth(77).map_or(s.len(), |(i, _)| i)]) } else { s }
}
