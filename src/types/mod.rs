//! Type system for Roc
//!
//! Hindley-Milner type inference with unification
//! Phase 1: Str type

use std::fmt;
use std::collections::HashMap;

pub mod checker;

pub use checker::TypeChecker;

/// Type representation
///
/// Every NAME in here — a record's fields, a union's tags, a nominal — is an interned
/// `&'static str` from `memory::string_pool`, not a `String`. Two reasons, both
/// measured: building a record type allocated one `String` per field while parsing, and
/// `Builtin.roc` is a file of type annotations; and a `Type` is CLONED constantly —
/// every nominal lookup in the parser deep-copies one, which cost 0.69µs per reference
/// to a nominal with a moderate backing type, and the checker has 135 clone sites.
/// Interned, a clone copies the spine and none of the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    /// String type
    Str,
    /// Unsigned integers
    U8, U16, U32, U64, U128,
    /// Signed integers
    I8, I16, I32, I64, I128,
    /// Floating point
    F32, F64,
    /// Arbitrary precision decimal
    Dec,
    /// Boolean type
    Bool,
    /// Type variable: $0, $1, etc.
    TypeVar(u32),
    /// List type: List(T)
    List(Box<Type>),
    /// Function type: (A -> B)
    Function(Box<Type>, Box<Type>),
    /// Empty record `{}` — Roc's unit type.
    Unit,
    /// Record type: `{ x: I64, y: Str }`. Sorted by field name so two records with
    /// the same fields in different source order unify.
    /// Record type. `open` means "at least these fields" — `{ name: Str, .. }` — so
    /// a record carrying extras still fits. A closed record is exactly its fields.
    Record { fields: Vec<(&'static str, Type)>, open: bool },
    /// An OPTIONAL record field, declared `name ?: Type`.
    ///
    /// The field may genuinely be absent, so a record without it still matches. Read
    /// with `.?name`, which yields `Ok(value)` or `Err(MissingField)`.
    ///
    /// Distinct from a DEFAULTED field (`name : Type ?? default`), which is filled in
    /// at construction and is therefore always present.
    Optional(Box<Type>),
    /// A nominal type declared with `Name := backing`.
    ///
    /// Distinct from every OTHER nominal, even one with an identical backing type —
    /// that distinctness is the whole point. It is not opaque, though: roc accepts the
    /// backing type where the nominal is expected (`f({ x: 1 })` for `f : Point -> _`),
    /// so unification falls through to the backing when only one side is nominal.
    ///
    /// `args` are the type arguments as written, `IList(I64)`'s `[I64]`, and two
    /// uses of one nominal unify at them. A placeholder (see `is_placeholder`) has
    /// nothing else: the recursive `IList(a)` inside
    /// `IList(a) := [INil, ICons(a, IList(a))]`, or a `Step(a)` named before its
    /// declaration, has no backing yet, and its declaration's parameters are these
    /// once it is expanded. Empty for a nominal without parameters, and for one
    /// rocflight builds itself.
    Nominal { name: &'static str, backing: Box<Type>, args: Vec<Type> },
    /// The type of a range expression. Opaque, like roc's.
    /// A numeric range, `1..=n`; the element is what iterating it yields.
    Range(Box<Type>),
    /// Tuple type: `(Str, I64)`. Positional, so element order is part of the type.
    Tuple(Vec<Type>),
    /// Tag union type: `[Red, Green]`, `[Foo(I64, Str), Bar]`, `[Exit(I8), ..]`.
    ///
    /// Each entry is a tag name and its payload types (empty for a bare tag).
    /// Sorted by tag name so declaration order does not affect unification.
    ///
    /// `open` distinguishes the two kinds that matter:
    /// * **closed** (`open: false`) — written out in an annotation. A value may only
    ///   use tags from the list, and a `match` must cover all of them.
    /// * **open** (`open: true`) — inferred from a tag expression, or written with a
    ///   trailing `..`. More tags may be added by unification, and a `match` needs a
    ///   wildcard to be exhaustive.
    ///
    /// `row` is an inferred open union's ROW VARIABLE: what the union turns out to
    /// be beyond the tags it lists. A tag expression or a tag pattern gets a fresh
    /// one, and unification binds it -- to the other side's extra tags, or, against
    /// a nominal, to the nominal itself -- so every copy of the union learns the
    /// same thing. `List.append(List.repeat(Red, 2), Blue)` is one union, grown to
    /// `[Blue, Red, ..]`, and a lone `Empty` passed where a `Node` is expected IS a
    /// `Node` once its row says so. An annotation's `..` gets one when the checker
    /// takes the signature in. `None` on a closed union, and on an open one the
    /// checker builds for a builtin's result, which unifies permissively.
    TagUnion { tags: Vec<(&'static str, Vec<Type>)>, open: bool, row: Option<u32> },
}

impl Type {
    /// Is this a nominal named but not declared here — an import, or the recursive
    /// reference inside a declaration's own body? Its backing is a stand-in variable:
    /// the parser's sentinel, or a fresh variable once an annotation was instantiated.
    pub fn is_placeholder(&self) -> bool {
        matches!(self, Type::Nominal { backing, .. } if matches!(**backing, Type::TypeVar(_)))
    }

    /// A record whose fields are exactly these. The common case — an open record only
    /// comes from an annotation that writes `..`.
    pub fn closed_record(fields: Vec<(&'static str, Type)>) -> Type {
        Type::Record { fields, open: false }
    }

    /// Is this one of the integer types?
    ///
    /// Used for numeric-literal polymorphism: `255` may be a U8, an I64, or any other
    /// integer, and only its context decides which.
    pub fn is_integer(&self) -> bool {
        matches!(
            self,
            Type::U8 | Type::U16 | Type::U32 | Type::U64 | Type::U128
                | Type::I8 | Type::I16 | Type::I32 | Type::I64 | Type::I128
        )
    }

    /// Is this one of the fractional types? `Dec` counts.
    pub fn is_fractional(&self) -> bool {
        matches!(self, Type::F32 | Type::F64 | Type::Dec)
    }

    /// Is this any numeric type at all?
    pub fn is_numeric(&self) -> bool {
        self.is_integer() || self.is_fractional()
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Str => write!(f, "Str"),
            Type::Unit => write!(f, "{{}}"),
            Type::Range(elem) => write!(f, "Range({})", elem),
            Type::Optional(inner) => write!(f, "{}?", inner),
            Type::Nominal { name, .. } => write!(f, "{}", name),
            Type::Tuple(items) => {
                let rendered: Vec<String> = items.iter().map(|t| t.to_string()).collect();
                write!(f, "({})", rendered.join(", "))
            }
            Type::TagUnion { tags, open, .. } => {
                let rendered: Vec<String> = tags
                    .iter()
                    .map(|(name, payload)| {
                        if payload.is_empty() {
                            (*name).to_string()
                        } else {
                            let args: Vec<String> =
                                payload.iter().map(|t| t.to_string()).collect();
                            format!("{}({})", name, args.join(", "))
                        }
                    })
                    .collect();
                if *open {
                    write!(f, "[{}, ..]", rendered.join(", "))
                } else {
                    write!(f, "[{}]", rendered.join(", "))
                }
            }
            Type::Record { fields, open } => {
                if fields.is_empty() {
                    return write!(f, "{}", if *open { "{ .. }" } else { "{}" });
                }
                let mut rendered: Vec<String> =
                    fields.iter().map(|(k, t)| format!("{}: {}", k, t)).collect();
                if *open {
                    rendered.push("..".to_string());
                }
                write!(f, "{{ {} }}", rendered.join(", "))
            }
            Type::U8 => write!(f, "U8"),
            Type::U16 => write!(f, "U16"),
            Type::U32 => write!(f, "U32"),
            Type::U64 => write!(f, "U64"),
            Type::U128 => write!(f, "U128"),
            Type::I8 => write!(f, "I8"),
            Type::I16 => write!(f, "I16"),
            Type::I32 => write!(f, "I32"),
            Type::I64 => write!(f, "I64"),
            Type::I128 => write!(f, "I128"),
            Type::F32 => write!(f, "F32"),
            Type::F64 => write!(f, "F64"),
            Type::Dec => write!(f, "Dec"),
            Type::Bool => write!(f, "Bool"),
            Type::TypeVar(n) => write!(f, "${}", n),
            Type::List(t) => write!(f, "List({})", t),
            Type::Function(a, b) => write!(f, "({} -> {})", a, b),
        }
    }
}

/// Type substitution map: maps TypeVar to concrete Type
pub struct Substitution {
    bindings: HashMap<u32, Type>,
}

impl Substitution {
    /// Point every variable currently bound to `old` at `new` instead.
    ///
    /// An OPEN record grows as more of its fields are read, and by then the variable it
    /// stands for has already been resolved away — `synth` applies the substitution
    /// before returning a name's type. This is how the growth gets back to the
    /// variable: nothing else is bound to that exact record, because the field types
    /// inside it are freshly made.
    pub fn rebind(&mut self, old: &Type, new: Type) {
        for bound in self.bindings.values_mut() {
            if bound == old {
                *bound = new.clone();
            }
        }
    }

    /// Rebind every variable whose type, APPLIED, is `old`: `rebind` for a type
    /// that has been applied already, whose variables' own bindings are in it.
    /// Only a record is rebound; a variable bound to another variable follows it.
    pub fn rebind_applied(&mut self, old: &Type, new: Type) {
        let hits: Vec<u32> = self
            .bindings
            .iter()
            .filter(|(_, bound)| matches!(bound, Type::Record { .. }) && self.apply(bound) == *old)
            .map(|(v, _)| *v)
            .collect();
        for v in hits {
            self.bindings.insert(v, new.clone());
        }
    }

    pub fn new() -> Self {
        Substitution {
            bindings: HashMap::new(),
        }
    }

    /// Insert a type binding
    pub fn insert(&mut self, var: u32, ty: Type) {
        self.bindings.insert(var, ty);
    }

    pub fn get(&self, var: u32) -> Option<&Type> {
        self.bindings.get(&var)
    }

    /// Apply substitution to a type (follow chains)
    pub fn apply(&self, ty: &Type) -> Type {
        match ty {
            Type::TypeVar(v) => {
                if let Some(bound) = self.get(*v) {
                    self.apply(bound)
                } else {
                    ty.clone()
                }
            }
            Type::List(inner) => Type::List(Box::new(self.apply(inner))),
            Type::Range(inner) => Type::Range(Box::new(self.apply(inner))),
            Type::Function(a, b) => {
                Type::Function(Box::new(self.apply(a)), Box::new(self.apply(b)))
            }
            // Every compound type, not just these two. A variable inside a tag's
            // payload, a record's field or a tuple's slot was never substituted, so
            // anything learned about it was learned and then thrown away — which is
            // what kept `render(Foo(42, "answer"))` from telling `42` that
            // `I64.to_str(n)` had already made it an I64.
            Type::Tuple(items) => Type::Tuple(items.iter().map(|t| self.apply(t)).collect()),
            Type::Optional(inner) => Type::Optional(Box::new(self.apply(inner))),
            Type::Nominal { name, backing, args } => Type::Nominal {
                name: *name,
                backing: Box::new(self.apply(backing)),
                args: args.iter().map(|t| self.apply(t)).collect(),
            },
            Type::Record { fields, open } => Type::Record {
                fields: fields.iter().map(|(n, t)| (*n, self.apply(t))).collect(),
                open: *open,
            },
            Type::TagUnion { tags, open, row } => {
                let tags = tags
                    .iter()
                    .map(|(n, args)| (*n, args.iter().map(|t| self.apply(t)).collect()))
                    .collect();
                match row {
                    Some(r) => self.extend(tags, *r),
                    None => Type::TagUnion { tags, open: *open, row: None },
                }
            }
            other => other.clone(),
        }
    }
}

impl Substitution {
    /// An open union's tags followed by what its row has been bound to: more tags
    /// (the row's own, then ITS row), a nominal the union turned out to be, or
    /// nothing yet.
    fn extend(&self, mut tags: Vec<(&'static str, Vec<Type>)>, row: u32) -> Type {
        match self.apply(&Type::TypeVar(row)) {
            Type::TypeVar(r) => {
                tags.sort_by(|x, y| x.0.cmp(&y.0));
                Type::TagUnion { tags, open: true, row: Some(r) }
            }
            Type::TagUnion { tags: more, open, row } => {
                tags.extend(more);
                tags.sort_by(|x, y| x.0.cmp(&y.0));
                Type::TagUnion { tags, open, row }
            }
            nominal @ Type::Nominal { .. } => nominal,
            other => unreachable!("a row is bound to tags or a nominal, not {}", other),
        }
    }
}

impl Default for Substitution {
    fn default() -> Self {
        Self::new()
    }
}
