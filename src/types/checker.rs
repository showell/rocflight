//! Type checking and inference
//!
//! Bidirectional type checking: synthesis (infer) + checking (verify)
//! Phase 3: Lambdas, calls, let bindings

use crate::memory::string_pool::intern;
use crate::ast::{Expr, BinOp, Pattern};
use crate::error::TypeError;
use super::{Type, Substitution};

/// Type checker with Hindley-Milner inference
pub struct TypeChecker {
    /// Every type the program declares, by name — `Position : { x : I64, y : I64 }`,
    /// `IOErr := […]` — so a name used BEFORE its declaration (or declared in another
    /// module) can be resolved when it is met. The parser leaves such a use as
    /// `Nominal { name, backing: TypeVar(u32::MAX) }`, a placeholder, and unifying
    /// through that shared sentinel tied unrelated types to each other: every numeral
    /// in a record whose type was declared two lines further down defaulted to `Dec`.
    declared_types: std::collections::HashMap<String, Type>,
    /// `Module.member` types a platform's modules declare — `Host.stdin_bytes!` has an
    /// annotation and no body, exactly like a `Builtin.roc` intrinsic, and this is
    /// where its type comes from. Consulted after the program's own bindings.
    declared_signatures: std::collections::HashMap<String, Type>,
    subst: Substitution,
    /// Each `Dispatch` node and its RECEIVER's type, before the substitution is
    /// finished. Resolved afterwards into the module whose method block owns the call.
    dispatches: Vec<(crate::ast::NodeId, Type)>,
    /// `iter.collect()` nodes whose OUTPUT type is a nominal with its own `from_iter`.
    /// roc's `collect` is `Output.from_iter(iterator)` — the expectation picks the
    /// implementation — so the compiler needs the name the annotation settled on.
    collect_targets: std::collections::HashMap<crate::ast::NodeId, String>,
    /// Numeric LITERAL nodes and the type they were checked against.
    ///
    /// `Dec` is a 128-bit fixed-point value, not a float, so `0.0` in a `List(Dec)` has
    /// to reach the evaluator as one. Only the checker knows which; resolved through
    /// the finished substitution, like `integer_binops`.
    literals: Vec<(crate::ast::NodeId, Type)>,
    /// Type variables that came from a numeric LITERAL.
    ///
    /// A numeral is polymorphic — `15` is an I64 in one place and a Dec in another — so
    /// it synthesises to a variable rather than to I64. These are the variables that
    /// stand for one, which is what lets a dispatch on an unresolved numeral still
    /// resolve, and what `fractional_literals` defaults at the end.
    numeral_vars: std::collections::HashSet<u32>,
    /// Field-value variables of a record UPDATE whose base is still unknown. Such a
    /// field is written unconditionally, so the slot it lands in must be a plain one:
    /// roc rejects `|r| { ..r, a: 5 }` applied to a `{ a ?: U64 }`. Everywhere else an
    /// optional slot happily accepts a value of its inner type, so this set is the
    /// only thing that tells the two apart.
    committed_vars: std::collections::HashSet<u32>,
    /// Record literals written as `Name.{ … }`, from `Parser::nominal_literals`.
    /// The nominal is erased in the AST, so this is how the fields keep their types.
    nominal_literals: std::collections::HashMap<crate::ast::NodeId, Type>,
    /// Literal nodes with an explicit type suffix, from `Parser::suffixed_literals`.
    suffixed: std::collections::HashMap<crate::ast::NodeId, Type>,
    /// Every numeric literal's VALUE (for a fractional one, its `Dec` attos) and
    /// whether it was written with a point — what `literal_problems` checks against
    /// the width each one ended up with.
    literal_values: std::collections::HashMap<crate::ast::NodeId, (i128, bool)>,
    /// Literal nodes the parser could not hold exactly, from
    /// `Parser::overflowed_literals`: a `Dec` past its bounds or with too many places.
    overflowed_literals: std::collections::HashSet<crate::ast::NodeId>,
    /// Record literal nodes checked against a type with optional fields they left
    /// out, and which fields: the compiler fills each with `Value::Missing`, which is
    /// what `{ a: <missing>, b: 2 }` inspects from.
    missing_fields: std::collections::HashMap<crate::ast::NodeId, Vec<String>>,
    /// Nominal -> its DEFAULTED field names (fields declared `name : T ?? default`),
    /// so a `{}` or partial record checked against the nominal may omit them.
    defaulted: std::collections::HashMap<String, Vec<String>>,
    /// Record/unit literal nodes checked against a nominal with defaulted or optional
    /// fields, and the nominal name: the compiler fills the omitted ones there.
    default_sites: std::collections::HashMap<crate::ast::NodeId, String>,
    /// See `declare_nominal_params`.
    nominal_params: std::collections::HashMap<String, Vec<u32>>,
    /// `List.with_capacity` calls whose element type is zero-sized; see `zero_sized`.
    zero_sized_capacity: std::collections::HashSet<crate::ast::NodeId>,
    /// See `note_polymorphic_use`.
    poly_problems: Vec<String>,
    /// Annotation result variables that the BODY may not pin; see `unify`.
    rigid_vars: std::collections::HashSet<u32>,
    /// Literal nodes standing for a nominal, and which conversion builds it:
    /// `x : Tag = "Roc"` is `Tag.from_quote("Roc")`, `n : Big = 42` is
    /// `Big.from_numeral(42)`, `u : Url = "a${b}"` is `Url.from_interpolation(..)`.
    /// The ones decided while checking; `literal_conversions` adds the ones inference
    /// decided later.
    converted: std::collections::HashMap<crate::ast::NodeId, (String, &'static str)>,
    /// Does the program declare a `from_quote` or `from_interpolation` anywhere? Only
    /// then is a string literal polymorphic; otherwise it is a `Str`, as cheaply as
    /// before.
    quotable: bool,
    /// The nominals declaring any literal conversion, so asking is a set lookup and
    /// not a walk of the environment per checked expression.
    conversion_nominals: std::collections::HashSet<String>,
    /// Variables standing for a string literal, which may still become a nominal
    /// with `from_quote`; see `numeral_vars`.
    quote_vars: std::collections::HashSet<u32>,
    /// The ids that are tag unions' ROWS. A row stands for more tags, or for the
    /// nominal its union turned out to be, so binding one to anything else is a
    /// type error -- `f : [A, ..x], x -> _` may not take a `Str` for `x`.
    row_vars: std::collections::HashSet<u32>,
    /// String literal nodes and their variables, plain and interpolated.
    str_literals: Vec<(crate::ast::NodeId, Type)>,
    interp_literals: Vec<(crate::ast::NodeId, Type)>,
    /// Literals with a nominal suffix, from `Parser::nominal_suffixes`.
    suffixed_nominals: std::collections::HashMap<crate::ast::NodeId, String>,
    /// Expressions checked against a nominal with a literal conversion, which a raw
    /// literal may reach through a generic body; the compiler converts at run time.
    coerce_values: std::collections::HashMap<crate::ast::NodeId, String>,
    /// Lambdas whose declared parameters are such nominals: `(param index, nominal)`.
    coerce_params: std::collections::HashMap<crate::ast::NodeId, Vec<(usize, String)>>,
    /// Every `match` and its scrutinee's type, for literal patterns on a nominal.
    matches: Vec<(crate::ast::NodeId, Type)>,
    /// `for` loops whose iterable is a nominal with an `iter` method: the compiler
    /// inserts the `.iter()` call.
    for_iter_calls: std::collections::HashSet<crate::ast::NodeId>,
    /// The result type of each lambda currently being checked, innermost last.
    ///
    /// A `return` leaves the FUNCTION, so what it hands back is the function's result
    /// — `|arg| { if …  { return 99 } else { … }; Str.count_utf8_bytes(first) }` is
    /// what tells `99` it is a byte count.
    returns: Vec<Type>,
    /// `Json.parse` call sites and the type each is expected to produce.
    ///
    /// Reading JSON back into a type needs to KNOW that type — `[1,2,3]` is a
    /// `List(ItemKind)` only because the annotation says so, and `ItemKind`'s own
    /// `parser_for` is what turns each number into a tag. Nothing at run time can
    /// recover that, so the checker has to say.
    parse_targets: std::collections::HashMap<crate::ast::NodeId, Type>,
    /// The nominal whose method block is being checked, if any.
    ///
    /// Inside `Graph :: … .{ … }` a sibling method is in scope UNQUALIFIED — roc lets
    /// `from_list` call `from_dict(…)` — but rocflight binds it as `Graph.from_dict`.
    /// Without this the bare name is unknown, its result is a fresh variable, and every
    /// use of the method it belongs to loses its type.
    enclosing_type: Vec<String>,
    /// A nested nominal's enclosing owner — see `Parser::enclosing_owners`.
    enclosing_owners: std::collections::HashMap<String, String>,

    /// Every type `synth` found and every type `check` was told, in the order they
    /// were pushed, when `record_types` asks; `node_types` resolves them. Off by
    /// default: it costs a type clone per call, and only a tool that reads the whole
    /// typed tree needs it.
    recorded_types: Option<Vec<(crate::ast::NodeId, Type)>>,
    /// Each `BinOp` node and the type its operands unified to, before the
    /// substitution is finished.
    ///
    /// The compiler asks for this so it can emit an integer-only opcode where both
    /// operands are known to be integers. Recorded rather than answered on the spot
    /// because inference is not finished yet: the type here may still be a variable
    /// that later unifies with `I64`.
    binops: Vec<(crate::ast::NodeId, Type)>,
    /// Numeral variables a `let` GENERALISED, so each call site gets its own copy.
    /// A literal typed by one of these — the `1` of `add_one = |x| x + 1` — has no
    /// single type: it is a `Dec` where the caller's argument is and a `U8` where
    /// that is, and rocflight compiles one body. Left as a plain integer, it takes
    /// the other operand's type at run time, which is the same answer either way.
    generalized_numerals: std::collections::HashSet<u32>,
    /// The copies `instantiate` made of each of those, one per call site: what the
    /// uses settled on is what says whether the body's literal has a single type.
    numeral_copies: std::collections::HashMap<u32, Vec<u32>>,
    next_var: u32,
    /// Methods a `where` clause promised, which may be dispatched on a type variable
    /// inference has not resolved. Set from the parser before checking.
    where_methods: Vec<String>,
    /// How deep synthesis is inside an UNANNOTATED lambda body.
    ///
    /// Such a body is checked before any call site is seen, so its parameters are still
    /// bare type variables and a dispatch on one cannot be resolved yet. roc infers
    /// these from the call sites; this checker synthesises once, so inside a lambda it
    /// falls back to the builtin table rather than refusing.
    // ponytail: a real fix propagates argument types back into the body at each call
    // site. Deferring costs the dispatch check inside lambdas only.
    lambda_depth: u32,
    /// Types of names in scope, innermost scope last, each with the type-variable ids
    /// that are universally quantified for it (empty for a monomorphic binding).
    ///
    /// A polymorphic binding is INSTANTIATED at every use: `identity : a -> a` used at
    /// Str and then at I64 needs a fresh variable each time, or the first use pins `a`
    /// and the second fails.
    ///
    /// Before this existed every identifier synthesised to a fresh variable, which
    /// meant the checker could not know a value's declared type — and so could not
    /// reject a tag outside a closed union, check a `match` for exhaustiveness, or
    /// give `x.field` a real type. Annotations reaching the AST are what fill it.
    env: Vec<Vec<(String, Type, Vec<u32>)>>,
}

impl TypeChecker {
    /// Create new type checker
    /// Record the methods a `where` clause promised, before checking begins.
    /// Bind every annotated top-level name BEFORE any body is checked.
    ///
    /// Declarations come in any order: `update_game` may call `prepend`, declared
    /// forty lines further down with its type written out. Checking bodies in file
    /// order gave such a call an unknown result, and every numeral that met it —
    /// `full.rest.drop_last(1)` — defaulted to a fraction. `check_let` binds the same
    /// annotation again when it reaches the definition, which changes nothing.
    pub fn predeclare(&mut self, ast: &Expr) {
        let mut cursor = ast;
        while let Expr::Let { name, annotation, value, body, .. } = cursor {
            // A `_ = <chain>` is the parser's shape for declarations followed by
            // top-level `expect`s; its chain is the program's top level too.
            if *name == "_" && matches!(**value, Expr::Let { .. }) {
                self.predeclare(value);
            }
            if let Some(declared) = annotation {
                let declared = self.with_rows(declared);
                let mut generics = Vec::new();
                Self::type_vars_in(&declared, &mut generics);
                generics.sort_unstable();
                generics.dedup();
                self.bind_poly(name, declared, generics);
            }
            cursor = body;
        }
        for (name, ..) in self.env.iter().flatten() {
            for method in [".from_quote", ".from_numeral", ".from_interpolation"] {
                if let Some(owner) = name.strip_suffix(method) {
                    self.conversion_nominals.insert(owner.to_string());
                    self.quotable |= method != ".from_numeral";
                }
            }
        }
    }

    /// Does the nominal `name` declare `method`? By the declared name, without
    /// instantiating it — asked from `unify` and from the exporters alike.
    fn has_method(&self, name: &str, method: &str) -> bool {
        let bare = name.rsplit('.').next().unwrap_or(name);
        self.env.iter().flatten().any(|(n, ..)| {
            n.strip_suffix(method).and_then(|n| n.strip_suffix('.')).is_some_and(|n| n == name || n == bare)
        })
    }

    /// A nominal that builds itself from a literal, by any of the three conversions.
    fn converts_literals(&self, ty: &Type) -> bool {
        !self.conversion_nominals.is_empty()
            && matches!(ty, Type::Nominal { name, .. }
                if self.conversion_nominals.contains(*name)
                    || self.conversion_nominals.contains(name.rsplit('.').next().unwrap_or(name)))
    }

    /// The nominal a suffix names, as the program declared it.
    fn nominal_named(&self, name: &str) -> Type {
        self.apply(&Type::Nominal { name: intern(name), backing: Box::new(Type::TypeVar(u32::MAX)), args: Vec::new() })
    }

    /// What `Name.from_interpolation` gives back, where it is declared.
    fn interpolation_result(&mut self, name: &str) -> Option<Type> {
        let signature = self.declared(name, "from_interpolation")?;
        Self::peel_params(&signature, 2).map(|(_, result)| result)
    }

    pub fn declare_suffixed_nominals(&mut self, suffixes: &[(crate::ast::NodeId, &'static str)]) {
        self.suffixed_nominals.extend(suffixes.iter().map(|(id, name)| (*id, name.to_string())));
    }

    fn leak(name: &str) -> &'static str {
        crate::memory::string_pool::intern(name)
    }

    /// Make the program's type declarations known by name; see `declared_types`.
    pub fn declare_types<'a>(&mut self, declared: impl IntoIterator<Item = (&'a str, Type)>) {
        for (name, ty) in declared {
            // A name the app merely IMPORTS is registered by its own parser as a
            // placeholder — a nominal whose backing is the sentinel variable — because
            // the app's file never declared it. The module's real declaration comes
            // second and must win, or an imported nominal's tags have no payload types.
            // A placeholder carries no information: it stands for the name itself with
            // an unparsed backing. `ThingAlias : ThingMod.Thing` in another module is
            // NOT one — it names `Thing`, which resolves — so it must win over the
            // app's own `Nominal { ThingAlias, ? }` stand-in.
            let placeholder = |t: &Type| {
                matches!(t, Type::Nominal { name: n, backing, .. }
                    if *n == name && matches!(**backing, Type::TypeVar(u32::MAX)))
            };
            // A module is an empty namespace, `Maybe :: [].{ ... }`, and types are
            // registered by their last segment, so the module and a type it declares
            // under its own name (`Maybe(a) : [Just(a), None]`, written `Maybe.Maybe`
            // elsewhere) are both `Maybe`. The namespace has no values to type, so
            // the declared type wins whichever arrives first.
            let namespace = |t: &Type| {
                let t = match t {
                    Type::Nominal { backing, .. } => &**backing,
                    other => other,
                };
                matches!(t, Type::TagUnion { tags, open: false, .. } if tags.is_empty())
            };
            match self.declared_types.get(name) {
                Some(existing) if placeholder(existing) && !placeholder(&ty) => {
                    self.declared_types.insert(name.to_string(), ty);
                }
                Some(existing) if namespace(existing) && !namespace(&ty) && !placeholder(&ty) => {
                    self.declared_types.insert(name.to_string(), ty);
                }
                Some(_) => {}
                None => {
                    self.declared_types.insert(name.to_string(), ty);
                }
            }
        }
    }

    /// The substitution applied, and every placeholder for a declared name replaced
    /// by its declaration, however deep. A placeholder is a `Nominal` whose backing is
    /// a bare variable: the parser's sentinel, or — once an annotation has been
    /// instantiated — a fresh variable standing in for it, which is why the name and
    /// not the sentinel is what identifies one. A name already being expanded stays
    /// as it is, which keeps a recursive declaration finite.
    fn apply(&self, ty: &Type) -> Type {
        let applied = self.subst.apply(ty);
        if self.declared_types.is_empty() {
            return applied;
        }
        self.expand(&applied, &mut Vec::new())
    }

    /// What a placeholder named `name` stands for, if a declaration says.
    ///
    /// A `:=` nominal whose own backing is a bare variable — `Wrapper(a) := a` — is
    /// not a placeholder, and is left alone.
    /// Does a record pattern name a field the scrutinee declares OPTIONAL?
    fn reads_optional_fields(&self, pattern: &Pattern, scrutinee: &Type) -> bool {
        let Pattern::Record { fields, .. } = pattern else { return false };
        let resolved = self.apply(scrutinee);
        let known = match &resolved {
            Type::Record { fields, .. } => fields,
            Type::Nominal { backing, .. } => match &**backing {
                Type::Record { fields, .. } => fields,
                _ => return false,
            },
            _ => return false,
        };
        fields.iter().any(|(name, _)| {
            known.iter().any(|(field, ty)| field == name && matches!(ty, Type::Optional(_)))
        })
    }

    fn placeholder_target(&self, name: &str) -> Option<&Type> {
        // `OsStr.OsStr` is `OsStr`'s own declaration seen from outside.
        let bare = name.rsplit('.').next().unwrap_or(name);
        let declared = self.declared_types.get(name).or_else(|| self.declared_types.get(bare))?;
        match declared {
            // `Wrapper(a) := a` — a nominal whose OWN backing is a variable is a real
            // declaration, not a stand-in. A cross-module alias to another nominal,
            // `ThingAlias : ThingMod.Thing`, has a different name and does resolve:
            // one more expansion reaches `Thing`'s declaration.
            Type::Nominal { name: n, backing, .. }
                if matches!(**backing, Type::TypeVar(_)) && (*n == name || *n == bare) => None,
            _ => Some(declared),
        }
    }

    /// A parameterised nominal's declared parameters, by its name or its bare name.
    fn params_of(&self, name: &str) -> Option<&Vec<u32>> {
        let bare = name.rsplit('.').next().unwrap_or(name);
        self.nominal_params.get(name).or_else(|| self.nominal_params.get(bare))
    }

    fn expand(&self, ty: &Type, seen: &mut Vec<String>) -> Type {
        match ty {
            Type::Nominal { name, backing, args } => {
                let args: Vec<Type> = args.iter().map(|t| self.expand(t, seen)).collect();
                if matches!(**backing, Type::TypeVar(_)) && !seen.iter().any(|s| s == *name) {
                    if let Some(target) = self.placeholder_target(name) {
                        // The declaration at THIS reference's arguments: `Step(a)`
                        // named inside `Iter_(a)` is `Step` over `Iter_`'s `a`, not
                        // over a type of its own.
                        let target = match self.params_of(name) {
                            Some(params) if !args.is_empty() => {
                                let mapping: Vec<(u32, Type)> = params.iter().copied().zip(args.iter().cloned()).collect();
                                Self::substitute_vars(target, &mapping)
                            }
                            _ => target.clone(),
                        };
                        seen.push((*name).to_string());
                        let expanded = self.expand(&target, seen);
                        seen.pop();
                        // The arguments stay on the nominal they name; an alias to
                        // another nominal (`Foo(a) : Bar`) is that one's.
                        let bare = |n: &str| n.rsplit('.').next().unwrap_or(n).to_string();
                        return match expanded {
                            Type::Nominal { name: n, backing, args: none } if none.is_empty() && bare(n) == bare(name) => {
                                Type::Nominal { name: n, backing, args }
                            }
                            other => other,
                        };
                    }
                }
                seen.push((*name).to_string());
                let backing = self.expand(backing, seen);
                seen.pop();
                Type::Nominal { name: *name, backing: Box::new(backing), args }
            }
            Type::List(inner) => Type::List(Box::new(self.expand(inner, seen))),
            Type::Range(inner) => Type::Range(Box::new(self.expand(inner, seen))),
            Type::Optional(inner) => Type::Optional(Box::new(self.expand(inner, seen))),
            Type::Function(a, b) => Type::Function(Box::new(self.expand(a, seen)), Box::new(self.expand(b, seen))),
            Type::Tuple(items) => Type::Tuple(items.iter().map(|t| self.expand(t, seen)).collect()),
            Type::Record { fields, open } => Type::Record {
                fields: fields.iter().map(|(n, t)| (*n, self.expand(t, seen))).collect(),
                open: *open,
            },
            Type::TagUnion { tags, open, row } => Type::TagUnion {
                tags: tags
                    .iter()
                    .map(|(n, args)| (*n, args.iter().map(|t| self.expand(t, seen)).collect()))
                    .collect(),
                open: *open,
                row: *row,
            },
            _ => ty.clone(),
        }
    }

    /// A type with no bits at run time: `{}`, or a record or tuple of only such.
    fn zero_sized(ty: &Type) -> bool {
        match ty {
            Type::Unit => true,
            Type::Record { fields, open: false } => fields.iter().all(|(_, t)| Self::zero_sized(t)),
            Type::Tuple(items) => items.iter().all(Self::zero_sized),
            Type::TagUnion { tags, open: false, .. } => tags.len() == 1 && tags[0].1.iter().all(Self::zero_sized),
            _ => false,
        }
    }

    /// The `List.with_capacity` calls that get a capacity of 0 — see `zero_sized`.
    pub fn zero_sized_capacity(&self) -> std::collections::HashSet<crate::ast::NodeId> {
        self.zero_sized_capacity.clone()
    }

    /// The parameters of each parameterised nominal — see `expand`.
    pub fn declare_nominal_params(&mut self, params: impl IntoIterator<Item = (String, Vec<u32>)>) {
        self.nominal_params.extend(params);
    }

    /// Make `Module.member : Type` annotations from a platform's modules known.
    pub fn declare_signatures(&mut self, signatures: impl IntoIterator<Item = (String, Type)>) {
        self.declared_signatures.extend(signatures);
    }

    pub fn allow_dispatch(&mut self, methods: Vec<String>) {
        // EXTENDS: the app's `where` clauses and each imported module's both grant
        // permission, and assigning dropped whichever came first.
        self.where_methods.extend(methods);
    }

    pub fn new() -> Self {
        TypeChecker {
            declared_types: std::collections::HashMap::new(),
            declared_signatures: std::collections::HashMap::new(),
            where_methods: Vec::new(),
            lambda_depth: 0,
            subst: Substitution::new(),
            binops: Vec::new(),
            dispatches: Vec::new(),
            collect_targets: std::collections::HashMap::new(),
            enclosing_type: Vec::new(),
            enclosing_owners: std::collections::HashMap::new(),

            recorded_types: None,
            literals: Vec::new(),
            numeral_vars: std::collections::HashSet::new(),
            generalized_numerals: std::collections::HashSet::new(),
            numeral_copies: std::collections::HashMap::new(),
            committed_vars: std::collections::HashSet::new(),
            nominal_literals: std::collections::HashMap::new(),
            suffixed: std::collections::HashMap::new(),
            literal_values: std::collections::HashMap::new(),
            overflowed_literals: std::collections::HashSet::new(),
            converted: std::collections::HashMap::new(),
            quotable: false,
            conversion_nominals: std::collections::HashSet::new(),
            quote_vars: std::collections::HashSet::new(),
            row_vars: std::collections::HashSet::new(),
            str_literals: Vec::new(),
            interp_literals: Vec::new(),
            suffixed_nominals: std::collections::HashMap::new(),
            coerce_values: std::collections::HashMap::new(),
            coerce_params: std::collections::HashMap::new(),
            matches: Vec::new(),
            for_iter_calls: std::collections::HashSet::new(),
            missing_fields: std::collections::HashMap::new(),
            defaulted: std::collections::HashMap::new(),
            default_sites: std::collections::HashMap::new(),
            nominal_params: std::collections::HashMap::new(),
            zero_sized_capacity: std::collections::HashSet::new(),
            poly_problems: Vec::new(),
            rigid_vars: std::collections::HashSet::new(),
            returns: Vec::new(),
            parse_targets: std::collections::HashMap::new(),
            next_var: 0,
            env: vec![Vec::new()],
        }
    }

    /// Enter a new scope (a lambda body, or a match arm).
    fn push_scope(&mut self) {
        self.env.push(Vec::new());
    }

    /// Leave the innermost scope. The outermost is never popped.
    fn pop_scope(&mut self) {
        if self.env.len() > 1 {
            self.env.pop();
        }
    }

    /// Record a monomorphic name's type in the innermost scope.
    fn bind(&mut self, name: &str, ty: Type) {
        if let Some(scope) = self.env.last_mut() {
            scope.push((name.to_string(), ty, Vec::new()));
        }
    }

    /// `import Foo exposing [bar]`: the bare name is the module's `Foo.bar`, with the
    /// same type and the same quantified variables. Without it the bare spelling is an
    /// unknown name, so `bar(0).baz({})` dispatched on an unresolved type.
    pub fn expose(&mut self, type_name: &str, names: &[String]) {
        for name in names {
            let qualified = format!("{}.{}", type_name, name);
            let found = self.env.iter().rev().find_map(|scope| {
                scope.iter().rev().find(|(n, _, _)| *n == qualified).cloned()
            });
            if let Some((_, ty, generics)) = found {
                self.bind_poly(name, ty, generics);
            }
        }
    }

    /// Record a name whose type variables are universally quantified.
    fn bind_poly(&mut self, name: &str, ty: Type, generics: Vec<u32>) {
        if let Some(scope) = self.env.last_mut() {
            scope.push((name.to_string(), ty, generics));
        }
    }

    /// Type variables still free in the environment.
    ///
    /// These may NOT be generalised at a let-binding: they belong to something still
    /// being inferred — an enclosing lambda's parameter, say — so quantifying one here
    /// would let two uses disagree about a type that is in fact fixed.
    fn env_type_vars(&self) -> Vec<u32> {
        let mut out = Vec::new();
        for scope in &self.env {
            for (_, ty, generics) in scope {
                let mut vars = Vec::new();
                Self::type_vars_in(&self.apply(ty), &mut vars);
                out.extend(vars.into_iter().filter(|v| !generics.contains(v)));
            }
        }
        out
    }

    /// Look a name up, innermost scope first so shadowing works.
    ///
    /// A polymorphic binding comes back INSTANTIATED, with fresh variables standing in
    /// for its quantified ones, so separate uses cannot constrain each other.
    fn lookup(&mut self, name: &str) -> Option<Type> {
        let found = self.env.iter().rev().find_map(|scope| {
            scope
                .iter()
                .rev()
                .find(|(n, _, _)| n == name)
                .map(|(_, t, g)| (t.clone(), g.clone()))
        })?;
        let (ty, generics) = found;
        Some(if generics.is_empty() { ty } else { self.instantiate(&ty, &generics) })
    }

    /// Replace each quantified variable with a fresh one, consistently.
    ///
    /// `a -> a` instantiates to `$7 -> $7`, not `$7 -> $8`: the two positions are the
    /// same variable, they are just a *new* same variable at this use site.
    fn instantiate(&mut self, ty: &Type, generics: &[u32]) -> Type {
        let mapping: Vec<(u32, Type)> =
            generics.iter().map(|id| (*id, self.fresh_var())).collect();
        // A generalised NUMERAL stays one in each copy: `add_one = |x| x + 1` is
        // generalised over a variable the body's `1` made a numeral, and a copy
        // nothing pins has to default to `Dec` as roc's does, not stay open. Only the
        // variables `check_let` generalised are carried — marking every numeral
        // variable's copy made `List.append`'s element a number, because an
        // annotation's variable ids and these share a number space.
        for (id, fresh) in &mapping {
            if let (Type::TypeVar(v), true) = (fresh, self.generalized_numerals.contains(id)) {
                self.numeral_vars.insert(*v);
                self.numeral_copies.entry(*id).or_default().push(*v);
                // A field value COMMITTED by a record update stays committed in every
                // copy: `set_a = |r| { ..r, a: 5 }` may not then write that `5` into a
                // `{ a ?: U64 }`, and roc refuses the program at the call site.
                if self.committed_vars.contains(id) {
                    self.committed_vars.insert(*v);
                }
            }
        }
        let instance = Self::substitute_vars(ty, &mapping);
        self.note_rows(&instance);
        instance
    }

    /// Structural substitution of type variables by id.
    fn substitute_vars(ty: &Type, mapping: &[(u32, Type)]) -> Type {
        match ty {
            Type::TypeVar(id) => mapping
                .iter()
                .find(|(from, _)| from == id)
                .map(|(_, to)| to.clone())
                .unwrap_or_else(|| ty.clone()),
            Type::List(inner) => {
                Type::List(Box::new(Self::substitute_vars(inner, mapping)))
            }
            Type::Range(inner) => Type::Range(Box::new(Self::substitute_vars(inner, mapping))),
            Type::Function(param, result) => Type::Function(
                Box::new(Self::substitute_vars(param, mapping)),
                Box::new(Self::substitute_vars(result, mapping)),
            ),
            Type::Tuple(items) => Type::Tuple(
                items.iter().map(|t| Self::substitute_vars(t, mapping)).collect(),
            ),
            // `open` is carried: closing it here made every instantiation of a
            // generalised `|c| c.help` demand a record with EXACTLY `help`, so
            // `get_help({ help, value })` failed with "unexpected field `value`".
            Type::Record { fields, open } => Type::Record {
                fields: fields
                    .iter()
                    .map(|(n, t)| (*n, Self::substitute_vars(t, mapping)))
                    .collect(),
                open: *open,
            },
            Type::Optional(inner) => {
                Type::Optional(Box::new(Self::substitute_vars(inner, mapping)))
            }
            // The row is a variable like any other, so a generalised union's copies
            // grow apart.
            Type::TagUnion { tags, open, row } => Type::TagUnion {
                tags: tags
                    .iter()
                    .map(|(n, payload)| {
                        (
                            *n,
                            payload.iter().map(|t| Self::substitute_vars(t, mapping)).collect(),
                        )
                    })
                    .collect(),
                open: *open,
                row: row.map(|r| match mapping.iter().find(|(from, _)| *from == r) {
                    Some((_, Type::TypeVar(to))) => *to,
                    _ => r,
                }),
            },
            Type::Nominal { name, backing, args } => Type::Nominal {
                name: *name,
                backing: Box::new(Self::substitute_vars(backing, mapping)),
                args: args.iter().map(|t| Self::substitute_vars(t, mapping)).collect(),
            },
            other => other.clone(),
        }
    }

    /// Collect every type-variable id appearing in a type.
    ///
    /// Applied to an ANNOTATION, these are the universally quantified variables: a
    /// lowercase name in a signature means "any type", so each use may pick its own.
    /// It also keeps the parser's ids out of unification entirely — the two allocate
    /// from the same number space, so an annotation's `$1` would otherwise collide
    /// with the checker's first fresh variable.
    fn type_vars_in(ty: &Type, out: &mut Vec<u32>) {
        match ty {
            Type::TypeVar(id) => {
                if !out.contains(id) {
                    out.push(*id);
                }
            }
            Type::List(inner) | Type::Range(inner) => Self::type_vars_in(inner, out),
            Type::Nominal { backing, args, .. } => {
                Self::type_vars_in(backing, out);
                args.iter().for_each(|t| Self::type_vars_in(t, out));
            }
            Type::Function(param, result) => {
                Self::type_vars_in(param, out);
                Self::type_vars_in(result, out);
            }
            Type::Tuple(items) => items.iter().for_each(|t| Self::type_vars_in(t, out)),
            Type::Record { fields, .. } => {
                fields.iter().for_each(|(_, t)| Self::type_vars_in(t, out))
            }
            Type::TagUnion { tags, row, .. } => {
                tags.iter()
                    .for_each(|(_, payload)| payload.iter().for_each(|t| Self::type_vars_in(t, out)));
                if let Some(r) = row {
                    Self::type_vars_in(&Type::TypeVar(*r), out);
                }
            }
            _ => {}
        }
    }

    /// Check `expr` against an expected type, rather than inferring it.
    ///
    /// The two cases that need checking rather than synthesis:
    ///
    /// * a **lambda** — the expected type supplies its parameter types, which is how a
    ///   `match` inside the body learns the parameter's declared tag union and can be
    ///   checked for exhaustiveness;
    /// * a **tag** — the expected union decides whether the tag is a member at all.
    ///
    /// Everything else falls back to synthesising and unifying, which is equivalent.
    pub fn check(&mut self, expr: &Expr, expected: &Type) -> Result<(), TypeError> {
        self.check_node(expr, expected)?;
        if let Some(types) = self.recorded_types.as_mut() {
            types.push((expr.id(), expected.clone()));
        }
        Ok(())
    }

    fn check_node(&mut self, expr: &Expr, expected: &Type) -> Result<(), TypeError> {
        let resolved = self.apply(expected);

        // A string literal where a nominal with `from_quote` is expected IS that
        // nominal: roc builds it by calling `from_quote`, and so does the compiler.
        if let (Expr::Str(_, id), Type::Nominal { name, .. }) = (expr, &resolved) {
            if self.has_method(name, "from_quote") {
                self.converted.insert(*id, ((*name).to_string(), "from_quote"));
                return Ok(());
            }
        }
        // An interpolated string where a nominal with `from_interpolation` is expected
        // — or a `Try` of one, when that is what the method returns — is that call.
        if let Expr::StrInterp(parts, id) = expr {
            let target = match &resolved {
                Type::Nominal { name, .. } => Some(*name),
                Type::TagUnion { tags, .. } => tags.iter().find(|(t, _)| *t == "Ok").and_then(|(_, p)| match p.as_slice() {
                    [Type::Nominal { name, .. }] => Some(*name),
                    _ => None,
                }),
                _ => None,
            };
            if let Some(name) = target.filter(|n| self.has_method(n, "from_interpolation")) {
                for part in parts {
                    if let crate::ast::StrPart::Expr(inner) = part {
                        self.synth(inner)?;
                    }
                }
                let result = self.interpolation_result(&name).unwrap_or_else(|| resolved.clone());
                self.unify(&result, &resolved)?;
                self.converted.insert(*id, ((*name).to_string(), "from_interpolation"));
                return Ok(());
            }
        }

        match expr {
            // Numeric literals are POLYMORPHIC: `255` is a U8 in `x : U8`, an I64 in
            // `x : I64`. Synthesising them as I64 and unifying would reject every
            // annotation that is not I64.
            Expr::Int(n, id) if resolved.is_numeric() => {
                self.literal_values.insert(*id, (*n, false));
                self.literals.push((*id, resolved.clone()));
                Ok(())
            }
            Expr::Float(_, exact, id) if resolved.is_fractional() => {
                self.literal_values.insert(*id, (*exact, true));
                self.literals.push((*id, resolved.clone()));
                Ok(())
            }

            // Arithmetic inherits the expected type, so `x : U8 = 1 + 2` works for the
            // same reason a bare literal does.
            Expr::BinOp { left, op, right, id }
                if resolved.is_numeric()
                    && matches!(
                        op,
                        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div
                            | BinOp::IntDiv | BinOp::Rem
                    ) =>
            {
                self.binops.push((*id, resolved.clone()));
                self.check(left, &resolved)?;
                self.check(right, &resolved)
            }

            // `Json.parse(text)` checked against a type is the only place that type is
            // ever stated; remember it for the compiler.
            Expr::Call { func, id, .. }
                if matches!(&**func, Expr::Qualified { module: "Json", name: "parse", .. }) =>
            {
                self.parse_targets.insert(*id, resolved.clone());
                let actual = self.synth(expr)?;
                self.unify(&actual, &resolved)
            }

            // `iter.collect()` builds whatever it is CHECKED against, through that
            // type's own `from_iter` — roc declares it
            // `collect : Iter(item) -> output where [output.from_iter : ...]`.
            // Synthesising alone gives a list, and `Set.to_list` on a list then has no
            // nominal to match. Whether the nominal really HAS a `from_iter` is left to
            // the compiler, which has the definitions; a name with none falls back to
            // the builtin `collect` exactly as before.
            Expr::Dispatch { method: "collect", id, .. }
                if matches!(&resolved, Type::Nominal { .. }) =>
            {
                let Type::Nominal { name, .. } = &resolved else { unreachable!("matched") };
                self.collect_targets.insert(*id, (*name).to_string());
                let actual = self.synth(expr)?;
                let _ = self.unify(&actual, &resolved);
                Ok(())
            }

            // `{}` against a record (or nominal over one) whose every field is
            // optional or defaulted: the unit is that record with all fields omitted,
            // filled at construction. The nominal name lets the compiler find the
            // defaults; an anonymous record fills its optionals with `<missing>`.
            Expr::Unit(id)
                if self.omittable_record(&resolved).is_some() =>
            {
                let (nominal, fields) = self.omittable_record(&resolved).expect("guarded");
                if let Some(name) = nominal {
                    self.default_sites.insert(*id, name.to_string());
                } else {
                    self.missing_fields.insert(*id, fields);
                }
                Ok(())
            }

            // A call's expectation belongs to its RESULT, and pushing it there BEFORE
            // the arguments are checked is what lets `{}` reach a nominal's defaults
            // through a generic function: `value : Config = id({})` resolves `id`'s
            // type variable to `Config` first, so the unit is checked against the
            // nominal rather than against a bare variable and left as a bare `{}`.
            // Qualified calls keep their own synth path, which types builtins.
            Expr::Call { func, args, .. } if !args.is_empty() => {
                // A builtin or method is typed by its DECLARED signature rather than
                // by synthesising the qualified name, which has no type of its own.
                let callee = match &**func {
                    Expr::Qualified { module, name, .. } => {
                        match self.declared(module, name) {
                            Some(signature) => signature,
                            // No signature: the builtin table types this one, and it
                            // does so only from the call as a whole.
                            None => {
                                let actual = self.synth(expr)?;
                                return self.unify(&actual, &resolved);
                            }
                        }
                    }
                    _ => self.synth(func)?,
                };
                let peeled = Self::peel_params(&self.apply(&callee), args.len());
                match peeled {
                    Some((params, result)) => {
                        self.unify(&result, &resolved)?;
                        // `List.with_capacity(n)` of a zero-sized element never
                        // allocates, so roc reports its capacity as 0.
                        if matches!(&**func, Expr::Qualified { module: "List", name: "with_capacity", .. }) {
                            if let Type::List(elem) = self.apply(&resolved) {
                                if Self::zero_sized(&elem) {
                                    self.zero_sized_capacity.insert(expr.id());
                                }
                            }
                        }
                        for (arg, ty) in args.iter().zip(params.iter()) {
                            let ty = self.apply(ty);
                            self.check(arg, &ty)?;
                        }
                        // As the fallback arm does: a generic call whose result is a
                        // nominal that converts literals hands back a raw literal at
                        // run time, and the conversion happens at this node.
                        if let Type::Nominal { name, .. } = &resolved {
                            if self.converts_literals(&resolved) {
                                self.coerce_values.insert(expr.id(), (*name).to_string());
                            }
                        }
                        Ok(())
                    }
                    None => {
                        let actual = self.synth(expr)?;
                        self.unify(&actual, &resolved)
                    }
                }
            }

            // A nominal construction knows its own field types better than whatever it
            // is being checked against — an open record grown from field reads names
            // only the fields that were read. Synthesising routes it through the
            // declaration; the expectation is then checked against the result.
            _ if expr_id_has_nominal(self, expr) => {
                let actual = self.synth(expr)?;
                self.unify(&actual, &resolved)
            }

            // An OPTIONAL field holds an ordinary value of its inner type; the option
            // is about whether the field is there, not about what it holds.
            _ if matches!(resolved, Type::Optional(_)) => {
                let Type::Optional(inner) = &resolved else { unreachable!("matched") };
                let inner = (**inner).clone();
                self.check(expr, &inner)
            }

            // Every element against the element type, so a list of numeric literals
            // becomes what its annotation says: `[46, 69]` in a `List(Dec)` is a list
            // of fixed-point values, not of integers.
            Expr::List(items, _) if matches!(resolved, Type::List(_)) => {
                let Type::List(element) = &resolved else { unreachable!("matched") };
                let element = (**element).clone();
                for item in items {
                    self.check(item, &element)?;
                }
                Ok(())
            }
            // Both branches of an `if` are checked against the expected type rather
            // than joined and then unified: `if c { a: 5 } else {}` against
            // `{ a ?: U8 }` checks `{ a: 5 }` and `{}` each against the optional-field
            // record, where joining them (a record with `{}`) would fail.
            Expr::If { condition, then_branch, otherwise, .. } => {
                let cond = self.synth(condition)?;
                self.unify(&cond, &Type::Bool)?;
                self.check(then_branch, &resolved)?;
                self.check(otherwise, &resolved)
            }
            // Each arm body checked against the expected type — so a `|_| {}` lambda
            // (whose `_` param wraps the body in a match) checks its `{}` against the
            // annotated result rather than synthesising it to a bare unit. Still
            // exhaustiveness-checked, via the shared helper.
            Expr::Match { scrutinee, arms, id } => {
                let scrutinee_type = self.synth(scrutinee)?;
                // Recorded as `synth` records it: a literal pattern against a nominal
                // with a conversion needs the scrutinee's type (`match_types`).
                self.matches.push((*id, scrutinee_type.clone()));
                for arm in arms {
                    for pattern in &arm.patterns {
                        // `{ age: Ok(v) }` against `{ age ?: U8 }`: the sub-pattern
                        // reads the optional field as `Ok`/`Err`, which `bind_pattern`
                        // types; a structural unify would demand a `U8` there.
                        if self.reads_optional_fields(pattern, &scrutinee_type) {
                            continue;
                        }
                        let pat = self.pattern_type(pattern)?;
                        self.unify(&scrutinee_type, &pat)?;
                    }
                    self.push_scope();
                    let resolved_scrut = self.apply(&scrutinee_type);
                    for pattern in &arm.patterns {
                        self.bind_pattern(pattern, &resolved_scrut);
                    }
                    let outcome = (|c: &mut Self| -> Result<(), TypeError> {
                        if let Some(guard) = &arm.guard {
                            let g = c.synth(guard)?;
                            c.unify(&g, &Type::Bool)?;
                        }
                        c.check(&arm.body, &resolved)
                    })(self);
                    self.pop_scope();
                    outcome?;
                }
                self.check_exhaustive(arms, &scrutinee_type)
            }


            Expr::Record(written, id)
                if matches!(self.record_backing(&resolved), Some(_)) =>
            {
                let (declared, open) = self.record_backing(&resolved).expect("guarded");
                // A nominal whose omitted fields are all optional/defaulted: the
                // compiler fills them from the nominal's defaults.
                if let Type::Nominal { name, .. } = &resolved {
                    let omitted_ok = declared.iter().all(|(field, ty)| {
                        written.iter().any(|(w, _)| w == field)
                            || matches!(ty, Type::Optional(_))
                            || self.defaulted.get(*name).is_some_and(|d| d.iter().any(|x| x == field))
                    });
                    if omitted_ok {
                        self.default_sites.insert(*id, (*name).to_string());
                    }
                }
                let left_out: Vec<String> = declared
                    .iter()
                    .filter(|(name, ty)| {
                        matches!(ty, Type::Optional(_)) && !written.iter().any(|(w, _)| *w == *name)
                    })
                    .map(|(name, _)| (*name).to_string())
                    .collect();
                if !left_out.is_empty() {
                    self.missing_fields.insert(*id, left_out);
                }
                for (name, value) in written {
                    match declared.iter().find(|(field, _)| field == name) {
                        Some((_, ty)) => {
                            let ty = ty.clone();
                            self.check(value, &ty)?;
                        }
                        // An OPEN row promises only the fields it lists, so a field
                        // the literal adds GROWS it — the same rule field access
                        // applies. Without this `set = |r| { ..r, a: 2 }` kept a row
                        // of just `a`, and `set({ a: 1, b: "x" }).b` had no `b`.
                        None if open => {
                            let extra = self.synth(value)?;
                            let mut grown = declared.clone();
                            grown.push((intern(name), extra));
                            grown.sort_by(|a, b| a.0.cmp(&b.0));
                            let grown = Type::Record { fields: grown, open: true };
                            self.subst.rebind(&resolved, grown);
                        }
                        None => {
                            self.synth(value)?;
                            return Err(TypeError {
                                message: format!("Record has an unexpected field `{}`", name),
                                expected: resolved.to_string(),
                                actual: format!("a record with `{}`", name),
                                line: 0,
                                col: 0,
                            });
                        }
                    }
                }
                let nominal_name = match &resolved {
                    Type::Nominal { name, .. } => Some(*name),
                    _ => None,
                };
                for (field, ty) in &declared {
                    if written.iter().any(|(w, _)| w == field) || matches!(ty, Type::Optional(_)) {
                        continue;
                    }
                    // A DEFAULTED field of a nominal may be omitted — it materializes
                    // at construction; only a genuinely required field is an error.
                    if nominal_name.as_ref().is_some_and(|n| self.defaulted.get(*n).is_some_and(|d| d.iter().any(|x| x == field))) {
                        continue;
                    }
                    return Err(TypeError {
                        message: format!("Record is missing field `{}`", field),
                        expected: resolved.to_string(),
                        actual: "a record without it".to_string(),
                        line: 0,
                        col: 0,
                    });
                }
                Ok(())
            }

            // A nominal wraps its backing, so checking against one checks against that.
            Expr::Record(..) | Expr::Tag { .. } | Expr::List(..)
                if matches!(resolved, Type::Nominal { .. }) =>
            {
                let Type::Nominal { backing, .. } = &resolved else { unreachable!("matched") };
                let backing = (**backing).clone();
                self.check(expr, &backing)
            }

            // A tuple against a tuple, element by element.
            Expr::Tuple(items, _) if matches!(&resolved, Type::Tuple(t) if t.len() == items.len()) =>
            {
                let Type::Tuple(declared) = &resolved else { unreachable!("matched") };
                let declared = declared.clone();
                for (item, ty) in items.iter().zip(declared.iter()) {
                    self.check(item, ty)?;
                }
                Ok(())
            }

            // A tag's payload against what the union declares for that tag, so a
            // literal inside one takes its type from the union: `Ok(147.66…)` against
            // `Try(Dec, …)` is a fixed-point value, not a float.
            Expr::Tag { name, args, .. } if matches!(resolved, Type::TagUnion { .. }) => {
                // An IMPORTED nominal's constructor checked against a row the caller
                // merely inferred — `lookup(MyTag.Foo({ x: 42 }))`, where `lookup`'s
                // parameter is known only to be `[Foo(a), ..]` — takes its payload
                // types from the DECLARATION. Without this the `42` is checked against
                // a bare variable and defaults to `Dec`. Only when the row says nothing
                // about the payload, so this can add information but never contradict.
                // An OPEN row is one the checker inferred from uses, not one the
                // program declared, so the declaration is the better authority. A
                // CLOSED union came from an annotation and keeps its own types.
                let row_is_inferred = matches!(&resolved, Type::TagUnion { tags, open: true, .. }
                    if tags.iter().any(|(t, payload)| t == name && payload.len() == args.len()));
                if row_is_inferred && !args.is_empty() && !matches!(*name, "Ok" | "Err" | "True" | "False") {
                    if let Some(declared) = self.nominal_declaring_tag(name, args.len()) {
                        let backing = match &declared {
                            Type::Nominal { backing, .. } => (**backing).clone(),
                            other => other.clone(),
                        };
                        // Guarded against re-entering this arm forever: the recursive
                        // check arrives with the declaration as its expectation, and
                        // that row is no longer blank.
                        if backing != resolved {
                            self.check(expr, &backing)?;
                            return self.unify(&declared, &resolved);
                        }
                    }
                }
                let Type::TagUnion { tags, .. } = &resolved else { unreachable!("matched") };
                match tags.iter().find(|(tag, _)| tag == name) {
                    Some((_, declared)) if declared.len() == args.len() => {
                        for (arg, ty) in args.iter().zip(declared.iter()) {
                            self.check(arg, ty)?;
                        }
                        Ok(())
                    }
                    // Not a tag this union declares, or a different arity: let
                    // unification report it rather than passing silently.
                    _ => {
                        let actual = self.synth(expr)?;
                        self.unify(&actual, &resolved)
                    }
                }
            }

            Expr::Lambda { params, body, .. } => {
                // `f! : () => {}` with `f! = || …`: the annotation's `()` is one unit
                // parameter and the lambda declares none, so peel that one arrow —
                // as a call `f!()` does — or the body is checked against the whole
                // function type.
                let arity = if params.is_empty()
                    && matches!(&resolved, Type::Function(param, _) if matches!(**param, Type::Unit))
                {
                    1
                } else {
                    params.len()
                };
                match Self::peel_params(&resolved, arity) {
                    Some((param_types, result_type)) => {
                        // A parameter declared as a nominal with a literal conversion
                        // may receive a raw literal from a generic caller.
                        let coerce: Vec<(usize, String)> = param_types
                            .iter()
                            .enumerate()
                            .filter(|(_, t)| self.converts_literals(&self.apply(t)))
                            .map(|(i, t)| match self.apply(t) {
                                Type::Nominal { name, .. } => (i, name.to_string()),
                                _ => unreachable!("filtered"),
                            })
                            .collect();
                        if !coerce.is_empty() {
                            self.coerce_params.insert(expr.id(), coerce);
                        }
                        self.push_scope();
                        for (param, ty) in params.iter().zip(param_types.iter()) {
                            self.bind(param, ty.clone());
                        }
                        // Deferred here as well as in the inferred case: a body may
                        // dispatch on the result of a builtin this interpreter does
                        // not model, which comes back as a bare variable through no
                        // fault of the program.
                        self.lambda_depth += 1;
                        self.returns.push(result_type.clone());
                        let outcome = self.check(body, &result_type);
                        self.returns.pop();
                        self.lambda_depth -= 1;
                        self.pop_scope();
                        outcome
                    }
                    // The annotation is not a function of this arity. Fall back so the
                    // mismatch is reported by unification rather than silently ignored.
                    None => {
                        let actual = self.synth(expr)?;
                        self.unify(&actual, &resolved)
                    }
                }
            }
            // A BLOCK's value is its last expression, so the expectation belongs there:
            // `f : a -> a` with `f = |x| { unused = 0  … }` must check the tail against
            // `a`, not synthesise the block and compare afterwards — by then the arms
            // of a `match` in that tail have already agreed among themselves.
            Expr::Let { name, annotation, value, body, .. } => {
                self.check_let(name, annotation, value)?;
                let bound = self.lookup(name);
                self.push_scope();
                if let Some(ty) = bound {
                    self.bind(name, ty);
                }
                let outcome = self.check(body, &resolved);
                self.pop_scope();
                outcome
            }
            _ => {
                let actual = self.synth(expr)?;
                self.unify(&actual, &resolved)?;
                // Not a literal, so nothing converts it here; a raw literal can still
                // arrive at run time through a generic body, and is converted then.
                if let Type::Nominal { name, .. } = &resolved {
                    if self.converts_literals(&resolved)
                        && !matches!(expr, Expr::Int(..) | Expr::Float(..) | Expr::Str(..) | Expr::StrInterp(..))
                    {
                        self.coerce_values.insert(expr.id(), (*name).to_string());
                    }
                }
                Ok(())
            }
        }
    }

    /// Peel a curried function type into its parameter types and final result.
    ///
    /// `A -> (B -> C)` gives `([A, B], C)`. Used to push an annotation's parameter
    /// types into a lambda's scope, which is how a `match` on a parameter learns the
    /// parameter's declared union — and therefore whether the arms are exhaustive.
    /// `module_of`, but a numeral variable answers as the fractional type it will
    /// default to — an unresolved numeral still has a method block.
    fn module_named(&self, ty: &Type) -> Option<&'static str> {
        if let Type::TypeVar(v) = ty {
            // A numeral nothing pinned down defaults to `Dec`, a string literal to `Str`.
            if self.quote_vars.contains(v) {
                return Some("Str");
            }
            return self.numeral_vars.contains(v).then_some("Dec");
        }
        module_of(ty)
    }

    /// What a numeric module's method gives back, read off its name.
    ///
    /// `Builtin.roc` declares every one of these per width, and loading the `Num`
    /// member for its signatures costs sixteen milliseconds a run; the names carry
    /// enough. `F64.pow(2.0, 3.0)` is an F64, `U8.is_even(n)` a Bool, `U8.to_i64(n)`
    /// an I64. A `_try`, `order_relative_to` or anything unrecognised stays
    /// unconstrained, exactly as before.
    /// The one nominal whose backing tag union declares `tag` with `arity` payload
    /// slots, if exactly one does. `None` when none or several do — a name two types
    /// share says nothing about which was meant, and only roc's own module resolution
    /// could tell them apart.
    fn nominal_declaring_tag(&mut self, tag: &str, arity: usize) -> Option<Type> {
        let candidates: Vec<Type> = self
            .declared_types
            .values()
            .filter(|decl| matches!(decl, Type::Nominal { .. }))
            .cloned()
            .collect();
        let mut found: Option<Type> = None;
        for decl in candidates {
            let expanded = self.expand(&decl, &mut Vec::new());
            let Type::Nominal { backing, .. } = &expanded else { continue };
            let Type::TagUnion { tags, .. } = &**backing else { continue };
            if !tags.iter().any(|(n, payload)| *n == tag && payload.len() == arity) {
                continue;
            }
            if found.is_some() {
                return None;
            }
            found = Some(expanded);
        }
        let declared = found?;
        // FRESH variables per construction, as a `Name.{ … }` literal gets.
        let mut generics = Vec::new();
        Self::type_vars_in(&declared, &mut generics);
        generics.sort_unstable();
        generics.dedup();
        Some(if generics.is_empty() { declared } else { self.instantiate(&declared, &generics) })
    }

    /// A nominal whose backing is a tag union containing every tag in `ty` and which
    /// declares `method`: the nominal an imported or bare-tag constructor stands for.
    fn nominal_for_tags(&mut self, ty: &Type, method: &str) -> Option<Type> {
        let Type::TagUnion { tags, .. } = ty else { return None };
        let names: Vec<&str> = tags.iter().map(|(n, _)| *n).collect();
        let candidates: Vec<Type> = self
            .declared_types
            .values()
            .filter(|decl| matches!(decl, Type::Nominal { .. }))
            .cloned()
            .collect();
        for decl in candidates {
            let Type::Nominal { ref name, .. } = decl else { continue };
            let expanded = self.expand(&decl, &mut Vec::new());
            let backing_tags = match &expanded {
                Type::Nominal { backing, .. } => match &**backing {
                    Type::TagUnion { tags, .. } => tags.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
                    _ => continue,
                },
                _ => continue,
            };
            if names.iter().all(|n| backing_tags.iter().any(|b| b == n)) && self.declared(name, method).is_some() {
                return Some(decl);
            }
        }
        None
    }

    fn numeric_result(&mut self, declared: &Type, name: &str) -> Option<Type> {
        if name.starts_with("is_") || name.ends_with("_overflows") {
            return Some(Type::Bool);
        }
        // `n.range_exclusive_to(m)` and its three siblings build a range of the
        // receiver's numeric type.
        if matches!(name, "range_exclusive_to" | "range_inclusive_to" | "range_exclusive_from" | "range_inclusive_from") {
            return Some(Type::Range(Box::new(declared.clone())));
        }
        if name == "from_bits" || name == "from_attos" {
            return Some(declared.clone());
        }
        // `to_i8_try`, `round_to_i32_try`, `sqrt_try`, `plus_try`: a `Try` whose `Ok`
        // is the conversion's target, or the module's own type.
        if let Some(rest) = name.strip_suffix("_try") {
            let rest = rest
                .strip_prefix("round_")
                .or_else(|| rest.strip_prefix("floor_"))
                .or_else(|| rest.strip_prefix("ceiling_"))
                .unwrap_or(rest);
            let ok = match Self::conversion_type(rest) {
                Some(target) => target,
                None if rest.starts_with("to_") || rest.starts_with("from_") => return None,
                None => declared.clone(),
            };
            return Some(Type::TagUnion {
                tags: vec![("Err", vec![self.fresh_var()]), ("Ok", vec![ok])],
                open: true,
                row: None,
            });
        }
        if name.starts_with("from_") || name.starts_with("range_") {
            return None;
        }
        if name == "to_bits" {
            return Some(if matches!(declared, Type::F32) { Type::U32 } else { Type::U64 });
        }
        if name == "to_attos" {
            return Some(Type::I128);
        }
        // `round_to_i128`, `floor_to_i32`: a conversion after a rounding.
        let bare = name
            .strip_prefix("round_")
            .or_else(|| name.strip_prefix("floor_"))
            .or_else(|| name.strip_prefix("ceiling_"))
            .unwrap_or(name);
        if bare.starts_with("to_") {
            return Self::conversion_type(bare);
        }
        match name {
            "order_relative_to" | "to_hash" | "highest" | "lowest" => None,
            _ => Some(declared.clone()),
        }
    }

    /// The target type of a `to_…` conversion name, `_wrap` or not: `to_u8` is a U8.
    fn conversion_type(name: &str) -> Option<Type> {
        let target = name.strip_prefix("to_")?;
        let target = target.strip_suffix("_wrap").unwrap_or(target);
        let module = match target {
            "u8" => "U8", "u16" => "U16", "u32" => "U32", "u64" => "U64", "u128" => "U128",
            "i8" => "I8", "i16" => "I16", "i32" => "I32", "i64" => "I64", "i128" => "I128",
            "f32" => "F32", "f64" => "F64", "dec" => "Dec",
            _ => return None,
        };
        type_named(module)
    }

    /// Give every argument that is still an unpinned numeral the type `ty`.
    ///
    /// For a numeric method with no declared signature. An argument that anything else
    /// already typed is left alone, so a declared `U8` shift count on a `U32` receiver
    /// stays a `U8`.
    fn pin_numerals_to(&mut self, ty: &Type, args: &[Type]) {
        for arg in args {
            if let Type::TypeVar(v) = self.apply(arg) {
                if self.numeral_vars.contains(&v) {
                    let _ = self.unify(ty, arg);
                }
            }
        }
    }

    /// The type of `name` read as a sibling of the method being checked.
    /// A nested nominal's methods see its enclosing owner's members as well, so the
    /// lookup walks outward: `Box.name`, then `Shape.name`.
    fn sibling(&mut self, name: &str) -> Option<Type> {
        let mut owner = self.enclosing_type.last()?.clone();
        loop {
            if let Some(ty) = self.lookup(&format!("{}.{}", owner, name)) {
                return Some(ty);
            }
            owner = self.enclosing_owners.get(&owner)?.clone();
        }
    }

    /// See `Parser::enclosing_owners`.
    pub fn declare_enclosing_owners(&mut self, owners: impl IntoIterator<Item = (String, String)>) {
        self.enclosing_owners.extend(owners);
    }

    /// A type as the program will actually see it: the substitution applied, and any
    /// numeral left unpinned resolved to the `Dec` it defaults to.
    pub fn defaulted(&self, ty: &Type) -> Type {
        self.default_numerals(&self.apply(ty), &self.numeral_vars)
    }

    fn default_numerals(&self, ty: &Type, numerals: &std::collections::HashSet<u32>) -> Type {
        match ty {
            Type::TypeVar(v) if numerals.contains(v) => Type::Dec,
            Type::TypeVar(v) if self.quote_vars.contains(v) => Type::Str,
            Type::List(inner) => Type::List(Box::new(self.default_numerals(inner, numerals))),
            Type::Range(inner) => Type::Range(Box::new(self.default_numerals(inner, numerals))),
            Type::Optional(inner) => {
                Type::Optional(Box::new(self.default_numerals(inner, numerals)))
            }
            Type::Tuple(items) => {
                Type::Tuple(items.iter().map(|t| self.default_numerals(t, numerals)).collect())
            }
            Type::Function(a, b) => Type::Function(
                Box::new(self.default_numerals(a, numerals)),
                Box::new(self.default_numerals(b, numerals)),
            ),
            Type::Nominal { name, backing, args } => Type::Nominal {
                name: *name,
                backing: Box::new(self.default_numerals(backing, numerals)),
                args: args.iter().map(|t| self.default_numerals(t, numerals)).collect(),
            },
            Type::Record { fields, open } => Type::Record {
                fields: fields
                    .iter()
                    .map(|(n, t)| (*n, self.default_numerals(t, numerals)))
                    .collect(),
                open: *open,
            },
            Type::TagUnion { tags, open, row } => Type::TagUnion {
                tags: tags
                    .iter()
                    .map(|(n, args)| {
                        (*n, args.iter().map(|t| self.default_numerals(t, numerals)).collect())
                    })
                    .collect(),
                open: *open,
                row: *row,
            },
            other => other.clone(),
        }
    }

    /// Pin the literals that carried an explicit type suffix: `255.U8` is a U8, not a
    /// numeral waiting to be defaulted.
    pub fn declare_suffixed_literals(&mut self, literals: &[(crate::ast::NodeId, Type)]) {
        self.suffixed.extend(literals.iter().cloned());
    }

    /// Tell the checker which record literals were written as a nominal construction.
    /// A record type, or a nominal over one, as `(fields, open)`.
    fn record_backing(&self, ty: &Type) -> Option<(Vec<(&'static str, Type)>, bool)> {
        match ty {
            Type::Record { fields, open } => Some((fields.clone(), *open)),
            Type::Nominal { backing, .. } => match &**backing {
                Type::Record { fields, open } => Some((fields.clone(), *open)),
                _ => None,
            },
            _ => None,
        }
    }

    /// If `ty` is a record (or nominal over one) that a `{}` may build — every field
    /// optional or, for a nominal, defaulted — the nominal name (if any) and the
    /// optional field names to fill with `<missing>` for the anonymous case.
    fn omittable_record(&self, ty: &Type) -> Option<(Option<&'static str>, Vec<String>)> {
        let (fields, _) = self.record_backing(ty)?;
        if fields.is_empty() {
            return None;
        }
        match ty {
            Type::Nominal { name, .. } => {
                let ok = fields.iter().all(|(field, t)| {
                    matches!(t, Type::Optional(_))
                        || self.defaulted.get(*name).is_some_and(|d| d.iter().any(|x| x == field))
                });
                ok.then(|| (Some(crate::memory::string_pool::intern(name)), Vec::new()))
            }
            _ => {
                let all_optional = fields.iter().all(|(_, t)| matches!(t, Type::Optional(_)));
                all_optional.then(|| (None, fields.iter().map(|(n, _)| (*n).to_string()).collect()))
            }
        }
    }

    /// Tell the checker which fields of each nominal are defaulted, so a `{}` or
    /// partial record against it may omit them (they materialize at construction).
    pub fn declare_defaults(&mut self, defaults: &[(String, Vec<(String, crate::ast::Expr)>)]) {
        for (name, fields) in defaults {
            self.defaulted
                .entry(name.clone())
                .or_default()
                .extend(fields.iter().map(|(f, _)| f.clone()));
        }
    }

    /// Record/unit literal nodes that build a nominal with omitted defaulted/optional
    /// fields; see `default_sites`.
    pub fn default_sites(&self) -> std::collections::HashMap<crate::ast::NodeId, &'static str> {
        self.default_sites
            .iter()
            .map(|(id, name)| (*id, crate::memory::string_pool::intern(name)))
            .collect()
    }

    pub fn declare_nominal_literals(&mut self, literals: &[(crate::ast::NodeId, Type)]) {
        self.nominal_literals.extend(literals.iter().cloned());
    }

    /// The declared type of `Module.method`, from the program or from `Builtin.roc`.
    ///
    /// A name the program binds wins: a user's own `Counter.show` is theirs. Otherwise
    /// the answer comes from the source roc itself compiles — `Dict.get : Dict(k, v),
    /// k -> Try(v, [KeyNotFound])` is written down there, so there is no reason to
    /// guess a result type from a method's name.
    ///
    /// Instantiated fresh at every use, so two calls to `Dict.empty()` do not unify
    /// with each other.
    fn declared(&mut self, module: &str, method: &str) -> Option<Type> {
        // `Range`'s own constructor and iterator over a THIRD-PARTY numeric type. Given
        // ahead of the `Range` → `List` alias below so a `Range.custom` config pins its
        // numbers to the range's element (`Range(Distance)` → `Distance`), which the
        // `List` alias would leave as an unconstrained default. `iter`/`fold`/`map` for
        // an integer range still fall through to `List`.
        if module == "Range" && !self.declared_types.contains_key("Range") {
            let num = self.fresh_var();
            let range = Type::Nominal { name: "Range", backing: Box::new(num.clone()), args: Vec::new() };
            let closed = |tags: Vec<(&'static str, Vec<Type>)>| Type::TagUnion { tags, open: false, row: None };
            let len_hint = closed(vec![
                ("Known", vec![Type::U64]),
                ("Unknown", vec![]),
            ]);
            match method {
                "custom" => {
                    let config = Type::Record {
                        fields: vec![
                            ("lower", num.clone()),
                            ("upper", num.clone()),
                            ("step", num.clone()),
                            ("upper_bound", closed(vec![
                                ("Exclusive", vec![]),
                                ("Inclusive", vec![]),
                            ])),
                            ("direction", closed(vec![
                                ("From", vec![]),
                                ("To", vec![]),
                            ])),
                            ("len_if_known", len_hint.clone()),
                        ],
                        open: false,
                    };
                    return Some(Type::Function(Box::new(config), Box::new(range)));
                }
                "iter" | "iter_rev" => {
                    return Some(Type::Function(Box::new(range), Box::new(Type::List(Box::new(num)))));
                }
                "size_hint" => {
                    return Some(Type::Function(Box::new(range), Box::new(len_hint)));
                }
                _ => {}
            }
        }
        // An `Iter` is the list it walks, so `Iter.fold(it, 0, f)` is checked against
        // `List.fold`'s signature — which is what tells `f` its element type. Unless
        // the program declares an `Iter` of its own, whose methods are its own.
        let module = if matches!(module, "Iter" | "Range") && !self.declared_types.contains_key(module) {
            "List"
        } else {
            module
        };
        // `Numeral`, as `Builtin.roc` declares it for a custom `from_numeral`.
        if module == "Numeral" {
            let numeral = self.nominal_named("Numeral");
            let result = match method {
                "is_negative" => Type::Bool,
                "digits_before_pt" | "digits_after_pt" => Type::List(Box::new(Type::U8)),
                "digits_after_pt_count" => Type::U64,
                _ => return None,
            };
            return Some(Type::Function(Box::new(numeral), Box::new(result)));
        }
        let qualified = format!("{}.{}", module, method);
        if let Some(ty) = self.lookup(&qualified) {
            return Some(ty);
        }
        let ty = match self.declared_signatures.get(&qualified) {
            Some(ty) => ty.clone(),
            None => crate::builtin::signatures_for(module)
                .iter()
                .find(|(name, _)| *name == qualified)
                .map(|(_, ty)| ty.clone())?,
        };
        let ty = self.with_rows(&ty);
        let mut generics = Vec::new();
        Self::type_vars_in(&ty, &mut generics);
        generics.sort_unstable();
        generics.dedup();
        Some(if generics.is_empty() { ty } else { self.instantiate(&ty, &generics) })
    }

    fn peel_params(ty: &Type, count: usize) -> Option<(Vec<Type>, Type)> {
        let mut params = Vec::with_capacity(count);
        let mut current = ty.clone();
        for _ in 0..count {
            match current {
                Type::Function(param, result) => {
                    params.push(*param);
                    current = *result;
                }
                _ => return None,
            }
        }
        Some((params, current))
    }

    /// Generate a fresh type variable
    pub fn fresh_var(&mut self) -> Type {
        let var = Type::TypeVar(self.next_var);
        self.next_var += 1;
        var
    }

    /// A fresh row variable, for a union inferred from one tag. Rows and type
    /// variables share one number space and one substitution.
    fn fresh_row(&mut self) -> u32 {
        self.next_var += 1;
        self.row_vars.insert(self.next_var - 1);
        self.next_var - 1
    }

    /// Note every row in `ty` as one: a signature's, or a copy of one.
    fn note_rows(&mut self, ty: &Type) {
        match ty {
            Type::TagUnion { tags, row, .. } => {
                if let Some(r) = row {
                    self.row_vars.insert(*r);
                }
                tags.iter().flat_map(|(_, payload)| payload).for_each(|t| self.note_rows(t));
            }
            Type::List(inner) | Type::Range(inner) | Type::Optional(inner) => self.note_rows(inner),
            Type::Function(a, b) => {
                self.note_rows(a);
                self.note_rows(b);
            }
            Type::Tuple(items) => items.iter().for_each(|t| self.note_rows(t)),
            Type::Record { fields, .. } => fields.iter().for_each(|(_, t)| self.note_rows(t)),
            Type::Nominal { args, .. } => args.iter().for_each(|t| self.note_rows(t)),
            _ => {}
        }
    }

    /// A signature as the checker uses it: each `..` in it, `[Red, ..]`, is a row
    /// of its own, which a use instantiates like any of the signature's variables.
    ///
    /// The rows are numbered above every variable the signature already has. The
    /// parser numbers an annotation's variables from the same space as the
    /// checker's, so a row that happened to share one's id would be instantiated
    /// as that variable, and a row became an `I64`.
    fn with_rows(&mut self, ty: &Type) -> Type {
        let mut vars = Vec::new();
        Self::type_vars_in(ty, &mut vars);
        // Not the parser's placeholder backing, `$u32::MAX`, which is no variable.
        if let Some(highest) = vars.iter().filter(|v| **v != u32::MAX).max() {
            self.next_var = self.next_var.max(highest + 1);
        }
        let with = self.add_rows(ty);
        self.note_rows(&with);
        with
    }

    fn add_rows(&mut self, ty: &Type) -> Type {
        match ty {
            Type::TagUnion { tags, open, row } => {
                let tags = tags
                    .iter()
                    .map(|(n, payload)| (*n, payload.iter().map(|t| self.add_rows(t)).collect()))
                    .collect();
                let row = if *open && row.is_none() { Some(self.fresh_row()) } else { *row };
                Type::TagUnion { tags, open: *open, row }
            }
            Type::List(inner) => Type::List(Box::new(self.add_rows(inner))),
            Type::Range(inner) => Type::Range(Box::new(self.add_rows(inner))),
            Type::Optional(inner) => Type::Optional(Box::new(self.add_rows(inner))),
            Type::Function(a, b) => Type::Function(Box::new(self.add_rows(a)), Box::new(self.add_rows(b))),
            Type::Tuple(items) => Type::Tuple(items.iter().map(|t| self.add_rows(t)).collect()),
            Type::Record { fields, open } => Type::Record {
                fields: fields.iter().map(|(n, t)| (*n, self.add_rows(t))).collect(),
                open: *open,
            },
            Type::Nominal { name, backing, args } => Type::Nominal {
                name: *name,
                backing: backing.clone(),
                args: args.iter().map(|t| self.add_rows(t)).collect(),
            },
            other => other.clone(),
        }
    }

    /// Synthesize (infer) type of expression
    /// The `BinOp` nodes whose operands are both integers, once inference is done.
    ///
    /// Resolved through the finished substitution, so a node whose operands were only
    /// a type variable while it was being checked is answered correctly here. What the
    /// compiler does with it is emit `BinInt`.
    /// Which MODULE each `Dispatch` node's receiver belongs to, once inference is done.
    ///
    /// This is what makes `xs.map(f)` reach `List.map` rather than whichever loaded type
    /// happens to define a `map`. Resolved through the finished substitution, like
    /// `integer_binops`, so a receiver that was still a variable when it was checked is
    /// answered correctly here.
    ///
    /// A receiver with no module — a bare record, a tag union, a variable a `where`
    /// clause covers — is simply absent, and the compiler falls back to what it did
    /// before: the single candidate by name, or a runtime dispatch on the value.
    /// See the `collect_targets` field.
    pub fn collect_targets(&self) -> std::collections::HashMap<crate::ast::NodeId, String> {
        self.collect_targets.clone()
    }

    pub fn dispatch_modules(
        &self,
    ) -> std::collections::HashMap<crate::ast::NodeId, &'static str> {
        self.dispatches
            .iter()
            .filter_map(|(id, ty)| Some((*id, module_of(&self.apply(ty))?)))
            .collect()
    }

    /// Which module each `BinOp` node's operands belong to, once inference is done.
    ///
    /// An operator is a method — `a == b` is `a.is_eq(b)` — so it needs the same
    /// answer as `dispatch_modules`, and for the same reason: the operand's TYPE says
    /// whose `is_eq` runs. Without it a lone user `is_eq` anywhere in the program
    /// captures every comparison, tuples and tags included.
    pub fn binop_modules(&self) -> std::collections::HashMap<crate::ast::NodeId, &'static str> {
        self.binops
            .iter()
            .filter_map(|(id, ty)| Some((*id, operator_module(&self.apply(ty))?)))
            .collect()
    }

    /// The literal nodes whose type is `Dec`, once inference is done.
    ///
    /// What the compiler does with it is emit a fixed-point value rather than an
    /// integer or a float: `Dec` carries eighteen decimal places exactly, which is why
    /// roc prints `147.666666666666666666` where an f64 gives `147.66666666666666`.
    /// What each `Json.parse` call was expected to produce, resolved.
    pub fn json_parse_targets(
        &self,
    ) -> std::collections::HashMap<crate::ast::NodeId, Type> {
        self.parse_targets.iter().map(|(id, ty)| (*id, self.apply(ty))).collect()
    }

    pub fn dec_literals(&self) -> std::collections::HashSet<crate::ast::NodeId> {
        self.literals_typed(&Type::Dec)
    }

    /// See `overflowed_literals`.
    pub fn declare_overflowed_literals(&mut self, ids: impl IntoIterator<Item = crate::ast::NodeId>) {
        self.overflowed_literals.extend(ids);
    }

    /// A top-level CONSTANT that cannot be built without crashing.
    ///
    /// roc folds every top-level constant at compile time, so a `crash` in one is
    /// reported then — used or not. A FUNCTION is not folded (its body runs only when
    /// called), and the PROGRAM's own value is what the run is for, so its crash is a
    /// crash. Only an unconditional crash counts: one behind an `if` or a `match` may
    /// never be reached.
    pub fn comptime_crash_problems(&self, ast: &Expr, is_program: bool) -> Option<String> {
        fn unconditional(expr: &Expr) -> bool {
            match expr {
                Expr::Crash(..) => true,
                Expr::Let { value, body, .. } | Expr::VarDecl { value, body, .. } => {
                    unconditional(value) || unconditional(body)
                }
                _ => false,
            }
        }
        // The program's value: the name the trailing expression reads, if any.
        let mut cursor = ast;
        while let Expr::Let { body, .. } | Expr::VarDecl { body, .. } = cursor {
            cursor = body;
        }
        // A MODULE's trailing expression is just its last binding — nothing runs it —
        // so only the program's own answer is exempt.
        let answer = match cursor {
            Expr::Ident(name, _) if is_program => Some(*name),
            _ => None,
        };
        let mut cursor = ast;
        while let Expr::Let { name, value, body, .. } = cursor {
            // `_` is a bare STATEMENT, not a constant: `{ crash "…"  identity }` runs
            // the crash, it does not fail to build a definition.
            if *name != "_"
                && Some(*name) != answer
                && !matches!(&**value, Expr::Lambda { .. })
                && unconditional(value)
            {
                return Some(format!(
                    "`{}` cannot be built: it crashes, and roc evaluates a top-level                      constant at compile time",
                    name
                ));
            }
            cursor = body;
        }
        // The program's ANSWER decided by a constant condition. roc folds it at
        // compile time and reports the branch that was never taken; with the answer
        // itself settled that way, the branch is dead code in the finished program
        // rather than a path some input could reach. Only the answer's own binding
        // counts: a constant condition anywhere else still runs, and the suite expects
        // the value it produces.
        // Only a module's `main`: that is the top-level constant roc folds, so its
        // untaken branch is dead in the finished program. A binding inside an
        // expression is an ordinary local — `x = if True { … } else { … }` then `x`
        // still runs, and the suite expects its value.
        if let Some(answer) = answer.filter(|name| *name == "main") {
            let mut cursor = ast;
            while let Expr::Let { name, value, body, .. } = cursor {
                if *name == answer {
                    let dead = match &**value {
                        Expr::If { condition, .. } => matches!(&**condition, Expr::Bool(..)),
                        Expr::Match { scrutinee, arms, .. } => {
                            matches!(&**scrutinee, Expr::Bool(..)) && arms.len() > 1
                        }
                        _ => false,
                    };
                    if dead {
                        return Some(format!(
                            "`{}` is decided at compile time, and the branch not taken is unreachable",
                            answer
                        ));
                    }
                }
                cursor = body;
            }
        }
        None
    }

    /// A builtin that needs to know what it is LOOKING at, handed something whose type
    /// is still open — `Str.inspect([])`, `List.sum([])`. roc calls that an unresolved
    /// polymorphic value and refuses it: there is no element type to render or to add,
    /// and nothing later can supply one because the call is the whole use.
    ///
    /// Only at the TOP LEVEL. Inside a lambda the same open type is an ordinary
    /// parameter — `show = |x| Str.inspect(x)` is a generic function, not a mistake —
    /// and only the call site says what it holds.
    fn note_polymorphic_use(&mut self, method: &str, receiver: Option<&Type>, args: &[Type]) {
        if self.lambda_depth > 0 || !matches!(method, "inspect" | "sum" | "product") {
            return;
        }
        let subject = receiver.cloned().or_else(|| args.first().cloned());
        let Some(subject) = subject else { return };
        // A NUMERAL is not open: nothing pinned it, so it is a `Dec`.
        let open = |ty: &Type, numerals: &std::collections::HashSet<u32>| {
            matches!(ty, Type::TypeVar(v) if !numerals.contains(v))
        };
        let resolved = self.apply(&subject);
        let unresolved = match (&resolved, method) {
            (Type::List(element), _) => open(&self.apply(element), &self.numeral_vars),
            (other, "inspect") => open(other, &self.numeral_vars),
            _ => false,
        };
        if unresolved {
            self.poly_problems.push(format!(
                "`{}` needs to know what it is given, and this value's type is still open",
                method
            ));
        }
    }

    /// The variables of an annotated function's RESULT that its body may not pin.
    ///
    /// Only a bare variable, and only one that also appears among the PARAMETERS: a
    /// result the caller can reach no other way — `Dict.empty() -> Dict(k, v)` — is
    /// the function's own to build.
    fn rigid_result(&self, instance: &Type) -> Vec<u32> {
        if !self.where_methods.is_empty() {
            return Vec::new();
        }
        let mut params = Vec::new();
        let mut cursor = instance;
        while let Type::Function(param, result) = cursor {
            params.push((**param).clone());
            cursor = result;
        }
        let Type::TypeVar(v) = cursor else { return Vec::new() };
        let mut in_params = Vec::new();
        for param in &params {
            Self::type_vars_in(param, &mut in_params);
        }
        if in_params.contains(v) { vec![*v] } else { Vec::new() }
    }

    /// See `note_polymorphic_use`.
    pub fn polymorphic_problems(&self) -> Option<String> {
        self.poly_problems.first().cloned()
    }

    /// A declaration roc refuses: a union naming a tag twice, a record naming a
    /// field twice.
    pub fn declaration_problems(&self) -> Option<String> {
        for (name, ty) in &self.declared_types {
            let backing = match ty {
                Type::Nominal { backing, .. } => &**backing,
                other => other,
            };
            let names: Vec<&'static str> = match backing {
                Type::TagUnion { tags, .. } => tags.iter().map(|(t, _)| *t).collect(),
                Type::Record { fields, .. } => fields.iter().map(|(f, _)| *f).collect(),
                _ => continue,
            };
            let mut seen = std::collections::HashSet::new();
            for member in names {
                if !seen.insert(member) {
                    return Some(format!("`{}` names `{}` twice", name, member));
                }
            }
        }
        None
    }

    /// Record literals with optional fields left out, and which; see `missing_fields`.
    pub fn missing_fields(&self) -> std::collections::HashMap<crate::ast::NodeId, Vec<&'static str>> {
        self.missing_fields
            .iter()
            .map(|(id, names)| {
                (*id, names.iter().map(|n| &*Box::leak(n.clone().into_boxed_str()) as &'static str).collect())
            })
            .collect()
    }

    /// Every literal that builds a nominal, with the nominal and the conversion:
    /// `(node, (nominal, "from_quote" | "from_numeral" | "from_interpolation"))`. The
    /// ones decided while checking, plus every literal inference later resolved to
    /// such a nominal.
    pub fn literal_conversions(
        &self,
    ) -> std::collections::HashMap<crate::ast::NodeId, (&'static str, &'static str)> {
        let mut out: std::collections::HashMap<crate::ast::NodeId, (&'static str, &'static str)> = self
            .converted
            .iter()
            .map(|(id, (name, method))| (*id, (Self::leak(name), *method)))
            .collect();
        let resolved = |ty: &Type| match self.apply(ty) {
            Type::Nominal { name, .. } => Some(name),
            _ => None,
        };
        for (id, ty) in &self.literals {
            if let Some(name) = resolved(ty).filter(|n| self.has_method(n, "from_numeral")) {
                out.entry(*id).or_insert((Self::leak(&name), "from_numeral"));
            }
        }
        for (literals, first, second) in [
            (&self.str_literals, "from_quote", "from_interpolation"),
            (&self.interp_literals, "from_interpolation", "from_quote"),
        ] {
            for (id, ty) in literals {
                let Some(name) = resolved(ty) else { continue };
                let method = [first, second].into_iter().find(|m| self.has_method(&name, m));
                if let Some(method) = method {
                    out.entry(*id).or_insert((Self::leak(&name), method));
                }
            }
        }
        out
    }

    /// Expressions the compiler converts at run time if a raw literal reaches them;
    /// see `coerce_values`.
    pub fn coerce_values(&self) -> std::collections::HashMap<crate::ast::NodeId, &'static str> {
        self.coerce_values.iter().map(|(id, name)| (*id, Self::leak(name))).collect()
    }

    /// Lambda parameters the compiler converts at entry; see `coerce_params`.
    pub fn coerce_params(
        &self,
    ) -> std::collections::HashMap<crate::ast::NodeId, Vec<(usize, &'static str)>> {
        self.coerce_params
            .iter()
            .map(|(id, params)| (*id, params.iter().map(|(i, n)| (*i, Self::leak(n))).collect()))
            .collect()
    }

    /// Each `match` whose scrutinee's type mentions a nominal with a literal
    /// conversion, resolved: a literal pattern there is that conversion and the
    /// nominal's `is_eq`.
    /// `for` loop nodes whose iterable is a nominal iterated via its `iter` method.
    pub fn for_iter_calls(&self) -> std::collections::HashSet<crate::ast::NodeId> {
        self.for_iter_calls.clone()
    }

    pub fn match_types(&self) -> std::collections::HashMap<crate::ast::NodeId, Type> {
        self.matches
            .iter()
            .map(|(id, ty)| (*id, self.apply(ty)))
            .filter(|(_, ty)| self.mentions_conversion(ty))
            .collect()
    }

    fn mentions_conversion(&self, ty: &Type) -> bool {
        match ty {
            Type::Nominal { backing, .. } => self.converts_literals(ty) || self.mentions_conversion(backing),
            Type::List(inner) | Type::Optional(inner) => self.mentions_conversion(inner),
            Type::Tuple(items) => items.iter().any(|t| self.mentions_conversion(t)),
            Type::Record { fields, .. } => fields.iter().any(|(_, t)| self.mentions_conversion(t)),
            Type::TagUnion { tags, .. } => tags.iter().any(|(_, p)| p.iter().any(|t| self.mentions_conversion(t))),
            _ => false,
        }
    }

    /// A literal that does not fit the type inference gave it: `256` as a `U8`, `1.5`
    /// as an `I64`, a `Dec` past its bounds. roc refuses these at compile time, and
    /// so does this, once every type is known.
    /// A `to_inspect` method must return `Str` — roc refuses one that does not. Run
    /// after the program is synthesised, so its methods are in scope.
    pub fn method_problems(&self) -> Option<String> {
        for scope in &self.env {
            for (name, ty, _) in scope {
                if !name.ends_with(".to_inspect") {
                    continue;
                }
                // Peel to the function's result type; a `to_inspect` that gives back
                // anything but `Str` is a compile problem.
                let mut current = self.apply(ty);
                while let Type::Function(_, result) = current {
                    current = self.apply(&result);
                }
                if !matches!(current, Type::Str | Type::TypeVar(_)) {
                    return Some(format!("`{}` must return Str", name));
                }
            }
        }
        // A record builder's `map2` COMBINES: `map2 : B(a), B(b), (a, b -> c) -> B(c)`.
        // Giving back `B(a)` throws the combined value away, and roc refuses the
        // declaration rather than the use. The test is that the result mentions the
        // combining function's own result — nothing else can stand for `c`.
        for (qualified, signature) in self
            .env
            .iter()
            .flatten()
            .map(|(n, t, _)| (n, t))
            .chain(self.declared_signatures.iter())
        {
            if !qualified.ends_with(".map2") {
                continue;
            }
            let Some((params, result)) = Self::peel_params(signature, 3) else { continue };
            let mut combined = params[2].clone();
            while let Type::Function(_, next) = combined {
                combined = *next;
            }
            let Type::TypeVar(produced) = combined else { continue };
            let mut in_result = Vec::new();
            Self::type_vars_in(&result, &mut in_result);
            if !in_result.contains(&produced) {
                return Some(format!(
                    "`{}` must give back the builder over what its function produced",
                    qualified
                ));
            }
        }
        None
    }

    pub fn literal_problems(&self) -> Option<String> {
        let mut typed: std::collections::HashMap<crate::ast::NodeId, Type> = std::collections::HashMap::new();
        for (id, ty) in &self.literals {
            typed.insert(*id, self.apply(ty));
        }
        for (id, ty) in &self.suffixed {
            typed.insert(*id, ty.clone());
        }
        for (id, (value, fractional)) in &self.literal_values {
            let Some(ty) = typed.get(id) else { continue };
            if ty.is_integer() {
                if *fractional {
                    return Some(format!("a fractional literal cannot be a {}", ty));
                }
                if self.overflowed_literals.contains(id) || !Self::width_fits(ty, *value) {
                    return Some(format!("{} does not fit in a {}", value, ty));
                }
            } else if self.overflowed_literals.contains(id)
                && matches!(self.defaulted(ty), Type::Dec)
            {
                // An unpinned fractional literal defaults to `Dec`; if it overflowed
                // what a `Dec` holds — `999…999.0`, 39 nines — roc refuses it, so it is
                // a compile problem here too, even before its type is pinned.
                return Some("this literal is outside what a Dec can hold".to_string());
            }
        }
        None
    }

    fn width_fits(ty: &Type, value: i128) -> bool {
        let (lo, hi): (i128, i128) = match ty {
            Type::U8 => (0, u8::MAX.into()),
            Type::U16 => (0, u16::MAX.into()),
            Type::U32 => (0, u32::MAX.into()),
            Type::U64 => (0, u64::MAX.into()),
            // A `U128` past `i128::MAX` is held as the same bit pattern, which reads as
            // negative here: the parser already checked its magnitude.
            Type::U128 => return true,
            Type::I8 => (i8::MIN.into(), i8::MAX.into()),
            Type::I16 => (i16::MIN.into(), i16::MAX.into()),
            Type::I32 => (i32::MIN.into(), i32::MAX.into()),
            Type::I64 => (i64::MIN.into(), i64::MAX.into()),
            _ => return true,
        };
        value >= lo && value <= hi
    }

    /// The literal nodes typed `F32`, which the compiler rounds to one.
    pub fn f32_literals(&self) -> std::collections::HashSet<crate::ast::NodeId> {
        self.literals_typed(&Type::F32)
    }

    /// The literal nodes typed `U128`, which the compiler emits as an unsigned 128-bit
    /// value — so a `U128` past `i128::MAX` prints its true magnitude, not a bit
    /// pattern read back negative.
    pub fn u128_literals(&self) -> std::collections::HashSet<crate::ast::NodeId> {
        self.literals_typed(&Type::U128)
    }

    /// The literal nodes that ended up as `ty`: inferred, or written with the suffix.
    fn literals_typed(&self, ty: &Type) -> std::collections::HashSet<crate::ast::NodeId> {
        self.literals
            .iter()
            .filter(|(_, t)| self.apply(t) == *ty)
            .map(|(id, _)| *id)
            .chain(self.suffixed.iter().filter(|(_, t)| *t == ty).map(|(id, _)| *id))
            .collect()
    }

    /// The literal nodes nothing ever pinned down.
    ///
    /// roc DEFAULTS an unconstrained numeral to `Dec` — `x = 15` prints `15.0`, and
    /// `[1, 2, 3]` prints `[1.0, 2.0, 3.0]`. A literal that any annotation, parameter
    /// or operation reached is concrete by now and is not here.
    pub fn fractional_literals(&self) -> std::collections::HashSet<crate::ast::NodeId> {
        let mut pinned = std::collections::HashSet::new();
        let mut floating = std::collections::HashSet::new();
        for (id, ty) in &self.literals {
            match self.apply(ty) {
                // A literal in a GENERALISED body follows its call sites: where one
                // of them pinned a width, the literal has no single type, so it stays
                // an integer and takes its operand's at run time (see
                // `generalized_numerals`). Where none did, every use defaults to `Dec`
                // and so does it — `rec = |n| … rec(n - 1) + 1` prints `2.0`.
                Type::TypeVar(v)
                    if self.generalized_numerals.contains(&v)
                        && self.numeral_copies.get(&v).is_some_and(|copies| {
                            copies.iter().any(|c| !matches!(self.apply(&Type::TypeVar(*c)), Type::TypeVar(_)))
                        }) => {}
                Type::TypeVar(_) => {
                    floating.insert(*id);
                }
                _ => {
                    pinned.insert(*id);
                }
            }
        }
        floating.retain(|id| !pinned.contains(id));
        floating
    }

    pub fn integer_binops(&self) -> std::collections::HashSet<crate::ast::NodeId> {
        self.binops
            .iter()
            .filter(|(_, ty)| self.apply(ty).is_integer())
            .map(|(id, _)| *id)
            .collect()
    }

    /// A `match`'s arms cover every case its scrutinee can be — enforced only when
    /// the scrutinee's type says what the cases are (a closed union, a `Bool`, or a
    /// literal type with no catch-all). Shared by the synthesis and checking paths.
    fn check_exhaustive(&mut self, arms: &[crate::ast::MatchArm], scrutinee_type: &Type) -> Result<(), TypeError> {
        let resolved = match self.apply(scrutinee_type) {
            Type::Nominal { backing, .. } => *backing,
            other => other,
        };
        // An `as` or a nominal pattern covers whatever its inner pattern does.
        fn base(p: &Pattern) -> &Pattern {
            match p { Pattern::As { inner, .. } | Pattern::Nominal { inner, .. } => base(inner), other => other }
        }
        let covers_everything = arms.iter().any(|arm| {
            arm.guard.is_none()
                && arm.patterns.iter().any(|p| matches!(base(p), Pattern::Wildcard | Pattern::Binding(_)))
        });
        let literal_scrutinee = resolved.is_numeric() || matches!(resolved, Type::Str);
        let bool_covered = matches!(resolved, Type::Bool)
            && ["True", "False"].iter().all(|wanted| {
                arms.iter().any(|arm| {
                    arm.guard.is_none()
                        && arm.patterns.iter().any(|p| matches!(p, Pattern::Tag { name, .. } if name == wanted))
                })
            });
        if !covers_everything && (literal_scrutinee || (matches!(resolved, Type::Bool) && !bool_covered)) {
            return Err(TypeError {
                message: "This match does not cover all cases".to_string(),
                expected: resolved.to_string(),
                actual: format!("{} arm(s)", arms.len()),
                line: 0, col: 0,
            });
        }
        if let Type::TagUnion { tags, open: false, .. } = &resolved {
            if !covers_everything {
                let uncovered: Vec<&str> = tags
                    .iter()
                    .filter(|(tag, _)| {
                        !arms.iter().any(|arm| {
                            arm.guard.is_none()
                                && arm.patterns.iter().any(|p| matches!(base(p), Pattern::Tag { name, .. } if name == tag))
                        })
                    })
                    .map(|(tag, _)| *tag)
                    .collect();
                if !uncovered.is_empty() && self.lambda_depth > 0 {
                    return Err(TypeError {
                        message: format!("This match does not cover all cases; missing: {}", uncovered.join(", ")),
                        expected: resolved.to_string(),
                        actual: format!("{} arm(s)", arms.len()),
                        line: 0, col: 0,
                    });
                }
            }
        }
        Ok(())
    }

    pub fn synth(&mut self, expr: &Expr) -> Result<Type, TypeError> {
        let ty = self.synth_node(expr)?;
        if let Some(types) = self.recorded_types.as_mut() {
            types.push((expr.id(), ty.clone()));
        }
        Ok(ty)
    }

    /// Record every node's type from here on; read them with `node_types`.
    pub fn record_types(&mut self) {
        self.recorded_types.get_or_insert_with(Vec::new);
    }

    /// Each recorded node's type, once inference is done: the substitution applied
    /// and an unpinned numeral defaulted, as the program will see it. Meaningful only
    /// after checking succeeded: a check that fails leaves a partial record. It builds
    /// the map on each call, so a caller asks once.
    ///
    /// A node typed more than once keeps the last type recorded for it, and a node
    /// that is checked records the type it was checked AGAINST after its own
    /// synthesised type, so that is the one kept. Unification is lenient in places,
    /// so the two can differ: a nominal against its backing, a range where a list is
    /// expected, one integer width against another. For a translator the expected
    /// type is usually the one wanted (a `5` checked against `U8` is a `U8`).
    pub fn node_types(&self) -> std::collections::HashMap<crate::ast::NodeId, Type> {
        self.recorded_types.iter().flatten().map(|(id, ty)| (*id, self.defaulted(ty))).collect()
    }

    fn synth_node(&mut self, expr: &Expr) -> Result<Type, TypeError> {
        match expr {
            _ if expr_id_has_nominal(self, expr) => {
                // Taken OUT while it is checked: `check` falls back to `synth` for a
                // shape it has no rule for, and that would arrive back here.
                let declared = self
                    .nominal_literals
                    .remove(&expr.id())
                    .expect("checked by the guard");
                // An IMPORTED nominal named in `exposing` — `import MyTag exposing
                // [MyTag]` — is attached by the parser with a PLACEHOLDER backing,
                // because the app's own file never declared it. The checker was given
                // the real declaration with the module, so use that: without it
                // `MyTag.Foo({ x: 42 })` checked its payload against a bare variable
                // and the literal defaulted to `Dec`.
                let declared = match &declared {
                    Type::Nominal { name, backing, .. } if matches!(**backing, Type::TypeVar(u32::MAX)) => {
                        self.declared_types.get(*name).cloned().unwrap_or_else(|| declared.clone())
                    }
                    _ => declared,
                };
                // FRESH variables per construction: `Wrapper(a)` is one declaration and
                // `Wrapper.{ item: "roc" }` and `Wrapper.{ item: 42 }` are two uses of
                // it, so they must not share the `a`.
                let declared = {
                    let mut generics = Vec::new();
                    Self::type_vars_in(&declared, &mut generics);
                    generics.sort_unstable();
                    generics.dedup();
                    if generics.is_empty() {
                        declared
                    } else {
                        self.instantiate(&declared, &generics)
                    }
                };
                let backing = match &declared {
                    Type::Nominal { backing, .. } => (**backing).clone(),
                    other => other.clone(),
                };
                let outcome = self.check(expr, &backing);
                self.nominal_literals.insert(expr.id(), declared.clone());
                outcome?;
                Ok(declared)
            }
            // `123.MyNum`, `"Roc".Tag`, `"a${b}".Url`: the nominal's conversion of
            // the literal, typed as what that conversion gives.
            Expr::Int(n, id) if self.suffixed_nominals.contains_key(id) => {
                self.literal_values.insert(*id, (*n, false));
                let ty = self.nominal_named(&self.suffixed_nominals[id]);
                self.literals.push((*id, ty.clone()));
                Ok(ty)
            }
            Expr::Float(_, exact, id) if self.suffixed_nominals.contains_key(id) => {
                self.literal_values.insert(*id, (*exact, true));
                let ty = self.nominal_named(&self.suffixed_nominals[id]);
                self.literals.push((*id, ty.clone()));
                Ok(ty)
            }
            Expr::Str(_, id) if self.suffixed_nominals.contains_key(id) => {
                let name = self.suffixed_nominals[id].clone();
                let ty = self.nominal_named(&name);
                self.converted.insert(*id, (name, "from_quote"));
                Ok(ty)
            }
            Expr::StrInterp(parts, id) if self.suffixed_nominals.contains_key(id) => {
                for part in parts {
                    if let crate::ast::StrPart::Expr(inner) = part {
                        self.synth(inner)?;
                    }
                }
                let name = self.suffixed_nominals[id].clone();
                let ty = self.interpolation_result(&name).unwrap_or_else(|| self.nominal_named(&name));
                self.converted.insert(*id, ((*name).to_string(), "from_interpolation"));
                Ok(ty)
            }
            // A string literal is polymorphic where the program declares a
            // `from_quote`: it may be a `Str` or that nominal, and only its use says.
            Expr::Str(_, id) if self.quotable => {
                let var = self.fresh_var();
                if let Type::TypeVar(v) = var {
                    self.quote_vars.insert(v);
                }
                self.str_literals.push((*id, var.clone()));
                Ok(var)
            }
            Expr::Str(_, _) => Ok(Type::Str),
            Expr::Unit(_) => Ok(Type::Unit),
            Expr::Bool(_, _) => Ok(Type::Bool),
            Expr::Range { start, end, .. } => {
                // The bounds share a type — `1..=n` and `1.0..<x` — but it is NOT
                // forced to `I64`: an unpinned range of literals is a numeral, so
                // `(1..=3)` iterates `Dec`s as roc does, and `(1.0..<4.0)` a `Dec`
                // range, and `(1.U8..=3.U8)` a `U8` range.
                let start_type = self.synth(start)?;
                let end_type = self.synth(end)?;
                self.unify(&start_type, &end_type)?;
                Ok(Type::Range(Box::new(self.apply(&start_type))))
            }
            Expr::Tuple(items, _) => {
                // Positional, so each element keeps its own type — no joining.
                let mut types = Vec::with_capacity(items.len());
                for item in items {
                    types.push(self.synth(item)?);
                }
                Ok(Type::Tuple(types))
            }
            Expr::TupleIndex { tuple, index, .. } => {
                let tuple_type = self.synth(tuple)?;
                // Likewise for a nominal over a tuple.
                let resolved = match self.apply(&tuple_type) {
                    Type::Nominal { backing, .. } => *backing,
                    other => other,
                };
                match resolved {
                    Type::Tuple(items) => items.get(*index).cloned().ok_or_else(|| TypeError {
                        message: format!(
                            "Tuple has {} element(s), so .{} is out of range",
                            items.len(),
                            index
                        ),
                        expected: format!("a tuple with at least {} element(s)", index + 1),
                        actual: Type::Tuple(items.clone()).to_string(),
                        line: 0,
                        col: 0,
                    }),
                    // Receiver type not resolved (no type environment for
                    // identifiers); eval will catch a genuine mistake.
                    _ => Ok(self.fresh_var()),
                }
            }
            Expr::RecordUpdate { base, fields, .. } => {
                let base_type = self.synth(base)?;
                let known = match self.apply(&base_type) {
                    Type::Record { fields, .. } => fields,
                    Type::Nominal { backing, .. } => match *backing {
                        Type::Record { fields, .. } => fields,
                        _ => Vec::new(),
                    },
                    _ => Vec::new(),
                };
                let mut updated = Vec::new();
                for (name, value) in fields {
                    // CHECKED against the field it replaces, where the base knows it.
                    // `{ ..p, age: 31 }` on a `{ age: I64 }` makes `31` an I64 rather
                    // than leaving it to default.
                    match known.iter().find(|(field, _)| field == name) {
                        Some((_, declared)) => {
                            let declared = declared.clone();
                            self.check(value, &declared)?;
                            updated.push((intern(name), declared));
                        }
                        None => updated.push((intern(name), self.synth(value)?)),
                    }
                }

                // The base is still unknown — an unannotated parameter. It is at least
                // a record WITH these fields, and saying so links their types to
                // whatever the caller passes: `birthday = |p| { ..p, age: 31 }` then
                // `birthday(start)` is what tells `31` it is an I64.
                let base_was_open = known.is_empty() && matches!(self.apply(&base_type), Type::TypeVar(_));
                if base_was_open {
                    let shape = Type::Record {
                        fields: updated.iter().map(|(n, t)| (*n, t.clone())).collect(),
                        open: true,
                    };
                    for (_, ty) in &updated {
                        if let Type::TypeVar(v) = self.apply(ty) {
                            self.committed_vars.insert(v);
                        }
                    }
                    let _ = self.unify(&base_type, &shape);
                    // The result IS the base's row — `{ ..r, a: 2 }` has exactly `r`'s
                    // fields, with `a` retyped, and the update already unified that in.
                    // Returning a snapshot instead froze the row at the fields seen so
                    // far, so a caller's extra field never reached the result and
                    // `set({ a: 1, b: "x" }).b` had no `b`.
                    return Ok(base_type);
                }

                match self.apply(&base_type) {
                    Type::Record { fields: known, open } => {
                        // The result keeps the base's shape, with named fields replaced.
                        // A field the base does not have is an error: an update cannot
                        // add one — unless the base is OPEN, where "the fields seen so
                        // far" is all that is known and an update names another of them.
                        let mut result = known.clone();
                        for (name, ty) in updated {
                            match result.iter_mut().find(|(field, _)| *field == name) {
                                Some(slot) => slot.1 = ty,
                                None if open => result.push((intern(&name), ty)),
                                None => {
                                    return Err(TypeError {
                                        message: format!(
                                            "Record has no field `{}` to update",
                                            name
                                        ),
                                        expected: Type::closed_record(known).to_string(),
                                        actual: name.to_string(),
                                        line: 0,
                                        col: 0,
                                    })
                                }
                            }
                        }
                        Ok(Type::closed_record(result))
                    }
                    // A nominal record updated is that nominal: `{ ..p, x: 1 }` on a
                    // `P := { x : I64, .. }` is a `P`. The fields were checked against
                    // the backing above; one it does not have is an error, as for a
                    // plain record. A fresh variable here made the next update's base
                    // an open record holding only the fields that update named.
                    resolved @ Type::Nominal { .. } if !known.is_empty() => {
                        for (name, _) in &updated {
                            if !known.iter().any(|(field, _)| field == name) {
                                return Err(TypeError {
                                    message: format!("Record has no field `{}` to update", name),
                                    expected: Type::closed_record(known.clone()).to_string(),
                                    actual: name.to_string(),
                                    line: 0,
                                    col: 0,
                                });
                            }
                        }
                        Ok(resolved)
                    }
                    // Base type not resolved yet; eval catches a real mistake.
                    _ => Ok(self.fresh_var()),
                }
            }
            Expr::List(items, _) => {
                // Every element must share one type. An empty list's element type is
                // unconstrained, so it gets a fresh variable.
                let mut element = self.fresh_var();
                for item in items {
                    let item_type = self.synth(item)?;
                    element = self.join(&element, &item_type)?;
                }
                Ok(Type::List(Box::new(element)))
            }
            Expr::Match { scrutinee, arms, id } => {
                let scrutinee_type = self.synth(scrutinee)?;
                self.matches.push((*id, scrutinee_type.clone()));

                // Every pattern must be able to match the scrutinee, and every arm
                // body must agree on a type. Tag patterns join into a union the same
                // way tag expressions do, so `Red`/`Green` arms accept a [Green, Red]
                // scrutinee.
                let mut result: Option<Type> = None;
                for arm in arms {
                    for pattern in &arm.patterns {
                        // `{ age: Ok(v) }` against `{ age ?: U8 }`: the sub-pattern
                        // reads the optional field as `Ok`/`Err`, which `bind_pattern`
                        // types; a structural unify would demand a `U8` there.
                        if self.reads_optional_fields(pattern, &scrutinee_type) {
                            continue;
                        }
                        let pattern_type = self.pattern_type(pattern)?;
                        self.unify(&scrutinee_type, &pattern_type)?;
                    }

                    // The arm's bindings are in scope for its guard and body, with the
                    // types their positions imply.
                    self.push_scope();
                    let resolved_scrutinee = self.apply(&scrutinee_type);
                    for pattern in &arm.patterns {
                        self.bind_pattern(pattern, &resolved_scrutinee);
                    }

                    let outcome = (|checker: &mut Self| -> Result<Type, TypeError> {
                        // A guard is a Bool, checked after its pattern matches.
                        if let Some(guard) = &arm.guard {
                            let guard_type = checker.synth(guard)?;
                            checker.unify(&guard_type, &Type::Bool)?;
                        }
                        checker.synth(&arm.body)
                    })(self);
                    self.pop_scope();

                    let body_type = outcome?;
                    result = Some(match result {
                        None => body_type,
                        Some(previous) => self.join(&previous, &body_type)?,
                    });
                }

                self.check_exhaustive(arms, &scrutinee_type)?;

                result.ok_or_else(|| TypeError {
                    message: "A match needs at least one arm".to_string(),
                    expected: "one or more arms".to_string(),
                    actual: "none".to_string(),
                    line: 0,
                    col: 0,
                })
            }
            Expr::If { condition, then_branch, otherwise, .. } => {
                // The condition must be a Bool — roc is explicit that a number will
                // not do: "This if condition must evaluate to a Bool".
                let condition_type = self.synth(condition)?;
                self.unify(&condition_type, &Type::Bool)?;

                // Both branches must agree, since the `if` has a single type.
                let then_type = self.synth(then_branch)?;
                let else_type = self.synth(otherwise)?;
                // `join`, not `unify`: branches yielding different tags produce the
                // union of them, not just the first branch's type.
                self.join(&then_type, &else_type)
            }
            Expr::For { name, iterable, body, id } => {
                let iterable_type = self.synth(iterable)?;
                let element = match self.apply(&iterable_type) {
                    // A range yields its element type without being a list.
                    Type::Range(elem) => *elem,
                    // A nominal with an `iter` method — a custom iterable — is looped
                    // over its `iter()`, whose element is what the loop binds. The
                    // compiler inserts the `.iter()` for these nodes.
                    Type::Nominal { ref name, .. } if self.declared(name, "iter").is_some() => {
                        self.for_iter_calls.insert(*id);
                        let iter = self.declared(name, "iter").expect("just checked");
                        match Self::peel_params(&iter, 1).map(|(_, result)| self.apply(&result)) {
                            Some(Type::Range(elem)) | Some(Type::List(elem)) => *elem,
                            _ => self.fresh_var(),
                        }
                    }
                    // Still unknown — a parameter whose call site has not been seen.
                    // It is still a LIST of the element, which is what carries a type
                    // from the loop body out to the caller's argument: without the
                    // link, `total([1, 2, 3])` could never tell its literals that
                    // `$sum` is an I64. A range satisfies `List` in `unify`, so this
                    // does not shut one out.
                    Type::TypeVar(_) => {
                        let element = self.fresh_var();
                        self.unify(&iterable_type, &Type::List(Box::new(element.clone())))?;
                        element
                    }
                    _ => {
                        let element = self.fresh_var();
                        self.unify(&iterable_type, &Type::List(Box::new(element.clone())))?;
                        element
                    }
                };

                self.push_scope();
                self.bind(name, element);
                let outcome = self.synth(body);
                self.pop_scope();
                outcome?;
                Ok(Type::Unit)
            }
            Expr::While { condition, body, .. } => {
                let condition_type = self.synth(condition)?;
                self.unify(&condition_type, &Type::Bool)?;
                self.synth(body)?;
                // `while True { … }` never falls through, so it constrains nothing —
                // its value is whatever the surrounding code needs, and a `return`
                // inside is the only exit. A conditional loop yields `{}`.
                if matches!(&**condition, Expr::Bool(true, _)) {
                    Ok(self.fresh_var())
                } else {
                    Ok(Type::Unit)
                }
            }
            // `break` never produces a value; it leaves the loop.
            Expr::Break(_) => Ok(self.fresh_var()),
            // `return` leaves the function, so it fits wherever it appears.
            Expr::Return(value, _) => {
                let returned = self.synth(value)?;
                if let Some(expected) = self.returns.last().cloned() {
                    self.unify(&returned, &expected)?;
                }
                // A `return` never falls through, so it fits wherever it is written.
                Ok(self.fresh_var())
            }
            // `crash` never returns, so it too fits anywhere.
            Expr::Crash(message, _) => {
                self.synth(message)?;
                Ok(self.fresh_var())
            }
            Expr::Expect(condition, _) => {
                let condition_type = self.synth(condition)?;
                self.unify(&condition_type, &Type::Bool)?;
                Ok(Type::Unit)
            }
            Expr::Dbg(value, _) => {
                self.synth(value)?;
                Ok(Type::Unit)
            }
            Expr::Dispatch { receiver, method, args, id } => {
                let receiver_type = self.synth(receiver)?;

                // Dispatch is STATIC: the receiver's type picks the module. If the type
                // is still an unresolved variable there is nothing to dispatch on, and
                // roc says so too — "trying to dispatch a method named to_str on an
                // unresolved type variable".
                let mut resolved = self.apply(&receiver_type);
                // A bare tag-union receiver whose tags belong to a nominal that
                // declares this method IS that nominal: an imported constructor,
                // `CounterMod.Counter(41)`, types as `[Counter(U64)]` here, but its
                // `Counter` nominal owns `.get`. roc erases the nominal at run time and
                // the value's shape finds the method; the checker needs the nominal so
                // the call type-checks and returns the right type.
                if let Type::TagUnion { .. } = &resolved {
                    if let Some(nominal) = self.nominal_for_tags(&resolved, method) {
                        let _ = self.unify(&receiver_type, &nominal);
                        resolved = self.apply(&nominal);
                    }
                }
                self.dispatches.push((*id, resolved.clone()));
                // A `where` clause promised this method exists on whatever the caller
                // supplies, so an unresolved receiver is fine here — roc has already
                // checked the constraint is satisfied at each call site.
                let promised = self.where_methods.iter().any(|m| m == method);
                // A numeral variable is not "unresolved" in the sense that matters: it
                // is a number whose width is not yet fixed, and every numeric width
                // answers the same method block here.
                let numeral =
                    matches!(&resolved, Type::TypeVar(v) if self.numeral_vars.contains(v) || self.quote_vars.contains(v));
                if matches!(resolved, Type::TypeVar(_)) && !numeral && !promised && self.lambda_depth == 0 {
                    return Err(TypeError {
                        message: format!(
                            "Cannot dispatch `{}` on an unresolved type; annotate the receiver",
                            method
                        ),
                        expected: "a receiver with a known type".to_string(),
                        actual: resolved.to_string(),
                        line: 0,
                        col: 0,
                    });
                }
                // A method a `where` clause promised, on a receiver still generic: its
                // result is whatever the caller's monomorphization makes it, which the
                // enclosing function's declared return type pins. Answering with the
                // builtin table instead would guess — `value.get()` would be a `Try`
                // from `List.get`, not the `U64` the `where item.get : item -> U64`
                // signature promises. A fresh variable lets the annotation decide.
                if promised && matches!(resolved, Type::TypeVar(_)) {
                    // Still synthesise the arguments, so their own literals get typed.
                    for arg in args {
                        self.synth(arg)?;
                    }
                    return Ok(self.fresh_var());
                }

                // A TAG UNION has no method block of its own, so only the handful this
                // interpreter implements for a `Try` will run. roc reports the rest as
                // "This <name> method is being called on a value whose type doesn't have
                // that method", and so should this — otherwise `x.first().to_str()`
                // quietly answers `Str` for a `Try` that has no `to_str`.
                if matches!(resolved, Type::TagUnion { .. })
                    && !promised
                    && !matches!(
                        *method,
                        "is_ok" | "is_err" | "map_ok" | "map_err" | "ok_or" | "on_err"
                    )
                    && self.declared("Tags", method).is_none()
                {
                    return Err(TypeError {
                        message: format!(
                            "This `{}` method is being called on a value whose type does not have it",
                            method
                        ),
                        expected: format!("a type with a `{}` method", method),
                        actual: resolved.to_string(),
                        line: 0,
                        col: 0,
                    });
                }

                // A result type is needed for CHAINING: `xs.len().to_str()` dispatches
                // on what `len` returned, so an unconstrained variable there makes the
                // second dispatch impossible.
                // A nominal receiver dispatches to its own method block, and here the
                // TYPE says which: `c : Counter` means `Counter.method`. This is real
                // static dispatch — the evaluator has to fall back to a name search,
                // because values carry no nominal tag.
                // The receiver's type names its module, and the module's method block
                // declares the signature — for a user nominal and for `Builtin.roc`'s
                // own types alike, once `declare_builtins` has seeded them. Applying it
                // consumes the receiver plus the written arguments.
                if let Some(module) = self.module_named(&resolved) {
                    if let Some(signature) = self.declared(module, method) {
                        if let Some((params, result)) =
                            Self::peel_params(&signature, args.len() + 1)
                        {
                            // CHECKED against the declared parameters, before they are
                            // synthesised: a lambda argument needs its parameter types
                            // BEFORE its body is looked at, or the body infers them from
                            // nothing. `xs.fold(0, |b, x| b + x)` nested inside another
                            // fold is where that shows: without it `x` is a free
                            // variable and the elements never learn what they are.
                            // The RECEIVER FIRST, which is what carries a type into the
                            // arguments: `List.fold : List(a), state, (state, a ->
                            // state) -> state` only tells the lambda what `a` is once
                            // the list has said so.
                            //
                            // Except a range, which answers the List methods without
                            // being a List — `(1..=100).iter()` reaches `List.iter`.
                            if let Some(first) = params.first() {
                                match &resolved {
                                    // A range answers the List methods as the `List` of
                                    // its element, so unify the first parameter against
                                    // that — otherwise the element type is thrown away
                                    // and a range of literals defaults to `Dec` even
                                    // when the method's own signature would pin it.
                                    Type::Range(elem) => {
                                        let as_list = Type::List(elem.clone());
                                        let _ = self.unify(first, &as_list);
                                    }
                                    _ => self.unify(first, &resolved)?,
                                }
                            }
                            for (arg, declared) in args.iter().zip(params.iter().skip(1)) {
                                let declared = self.apply(declared);
                                self.check(arg, &declared)?;
                            }
                            return Ok(self.apply(&result));
                        }
                    }
                }

                // `negate` keeps its receiver's type; everything else comes from the
                // shared builtin table.
                if *method == "negate" {
                    return Ok(resolved);
                }
                // No signature to go by: synthesise the arguments and fall back to the
                // checker's own table.
                let mut arg_types = Vec::with_capacity(args.len());
                for arg in args {
                    arg_types.push(self.synth(arg)?);
                }
                // `x.shl_wrap(1)` on a `U8`: the `1` is the receiver's width, as above.
                if resolved.is_integer() || resolved.is_fractional() {
                    self.pin_numerals_to(&resolved, &arg_types);
                }
                // A count is a `U64`: `xs.step_by(2)`, `xs.take_first(3)`.
                if matches!(*method, "step_by" | "take_first" | "take_last" | "drop_first" | "drop_last") {
                    // `step_by` on a fractional range steps by the element:
                    // `5.Dec.range_inclusive_from(1).step_by(1.5)`.
                    let element = self.element_of(Some(&resolved));
                    let count = if *method == "step_by" && self.apply(&element).is_fractional() {
                        element
                    } else {
                        Type::U64
                    };
                    self.pin_numerals_to(&count, &arg_types);
                }
                // `value.encode(format)` is `format.encode_<kind>(value)`, which is
                // literally how `Builtin.roc` defines `encode` on every scalar. Those
                // members are not loaded, so the format's own signature is what says
                // what the call gives back — and a chain off it needs that.
                if *method == "encode" && arg_types.len() == 1 {
                    if let Some(kind) = self.module_named(&resolved).map(|m| m.to_ascii_lowercase()) {
                        let fmt = self.apply(&arg_types[0]);
                        if let Some(owner) = self.module_named(&fmt) {
                            let named = format!("encode_{}", kind);
                            if let Some(signature) = self.declared(owner, &named) {
                                if let Some((params, result)) = Self::peel_params(&signature, 2) {
                                    let _ = self.unify(&params[0], &fmt);
                                    let _ = self.unify(&params[1], &resolved);
                                    return Ok(self.apply(&result));
                                }
                            }
                        }
                    }
                }
                // A method roc has NO declaration for, on a builtin container: not a
                // gap in this interpreter, a name that does not exist. roc reports
                // `list.reverse()` (it is `rev`) rather than running it.
                if self
                    .module_named(&resolved)
                    .is_some_and(|m| matches!(m, "List" | "Str" | "Dict" | "Set")
                        || crate::eval::is_numeric_module(m))
                    && !crate::builtin::declared_names().contains(*method)
                    && !self.env.iter().flatten().any(|(n, ..)| {
                        n.rsplit_once('.').is_some_and(|(_, m)| m == *method)
                    })
                {
                    return Err(TypeError {
                        message: format!("There is no `{}` method", method),
                        expected: format!("a method roc declares"),
                        actual: method.to_string(),
                        line: 0,
                        col: 0,
                    });
                }
                // `n.to_u64()` says its own result in its name. The qualified spelling
                // `Dec.to_u64(n)` reaches `numeric_result`, but method syntax only ever
                // reached the shared builtin table, which left it unconstrained — so
                // `0 + n.to_u64()` defaulted the `0` to `Dec` and summed fixed-point.
                let numeric_receiver = resolved.is_integer()
                    || resolved.is_fractional()
                    || matches!(&resolved, Type::TypeVar(v) if self.numeral_vars.contains(v));
                if numeric_receiver {
                    if let Some(target) = Self::conversion_type(method) {
                        return Ok(target);
                    }
                }
                Ok(self.builtin_result(method, Some(&resolved), &arg_types))
            }
            Expr::OptionalField { record, field, .. } => {
                let record_type = self.synth(record)?;
                let resolved = match self.apply(&record_type) {
                    Type::Nominal { backing, .. } => *backing,
                    other => other,
                };
                // `Ok(value)` or `Err(MissingField)`. The union is CLOSED: those are
                // the only two outcomes.
                let value = match &resolved {
                    Type::Record { fields: known, .. } => known
                        .iter()
                        .find(|(name, _)| name == field)
                        // The Ok payload is the field's own type, not the Optional
                        // wrapper around it.
                        .map(|(_, t)| match t {
                            Type::Optional(inner) => (**inner).clone(),
                            other => other.clone(),
                        })
                        .unwrap_or_else(|| self.fresh_var()),
                    _ => self.fresh_var(),
                };
                Ok(Type::TagUnion {
                    tags: vec![
                        ("Err", vec![Type::TagUnion {
                            tags: vec![("MissingField", Vec::new())],
                            open: false,
                            row: None,
                        }]),
                        ("Ok", vec![value]),
                    ],
                    open: false,
                    row: None,
                })
            }
            Expr::FieldAccess { record, field, .. } => {
                let record_type = self.synth(record)?;
                // A nominal over a record supports field access on its backing — roc
                // allows `p.x` for `p : Point`, and a method body relies on it.
                let resolved = match self.apply(&record_type) {
                    Type::Nominal { backing, .. } => *backing,
                    other => other,
                };
                match resolved {
                    // An OPEN record promises only the fields it lists, so reading
                    // another one GROWS it. `|p| p.x + p.y` reaches here twice, and
                    // without this the second read failed against the record the first
                    // one had just invented.
                    Type::Record { ref fields, open: true }
                        if !fields.iter().any(|(name, _)| name == field) =>
                    {
                        let field_type = self.fresh_var();
                        let mut grown = fields.clone();
                        grown.push((intern(field), field_type.clone()));
                        let grown = Type::Record { fields: grown, open: true };
                        match record_type {
                            Type::TypeVar(v) => self.subst.insert(v, grown),
                            ref already => self.subst.rebind(already, grown),
                        }
                        Ok(field_type)
                    }
                    Type::Record { fields, .. } => fields
                        .iter()
                        .find(|(name, _)| name == field)
                        .map(|(_, ty)| ty.clone())
                        .ok_or_else(|| TypeError {
                            message: format!("Record has no field '{}'", field),
                            expected: format!("a record with field '{}'", field),
                            actual: Type::closed_record(fields.clone()).to_string(),
                            line: 0,
                            col: 0,
                        }),
                    // Not resolved yet — an unannotated parameter. It is at least a
                    // record WITH this field, and saying so links the field's type to
                    // whatever the caller passes: `|a| 0 - a.cents` learns that `cents`
                    // is an I64 from the `Money` it is called with, so the `0` beside
                    // it does not default.
                    Type::TypeVar(_) => {
                        let field_type = self.fresh_var();
                        let shape = Type::Record {
                            fields: vec![(intern(field), field_type.clone())],
                            open: true,
                        };
                        let _ = self.unify(&record_type, &shape);
                        Ok(field_type)
                    }
                    _ => Ok(self.fresh_var()),
                }
            }
            // `Point.{ x: 3 }` is a nominal construction: its fields have the types the
            // declaration gave them, even though the AST no longer says which nominal
            // it was.

            Expr::Record(fields, _) => {
                // Sorted by field name, so `{ x: 1, y: 2 }` and `{ y: 2, x: 1 }`
                // produce the same type and therefore unify.
                let mut typed = Vec::with_capacity(fields.len());
                for (name, value) in fields {
                    typed.push((intern(name), self.synth(value)?));
                }
                typed.sort_by(|a, b| a.0.cmp(&b.0));
                Ok(Type::closed_record(typed))
            }
            // A tag literal is a one-tag union. Unifying it with another union
            // merges them, so `if b Red else Green` comes out as [Green, Red].
            Expr::Tag { name, args, .. } => {
                // An IMPORTED nominal's constructor — `CrateMod.Crate(5)` — reaches
                // here as a bare tag: the app's parser never saw
                // `Crate := [Crate(U64)]`, so it could not attach the declaration the
                // way it does for a nominal declared in this file. The CHECKER has the
                // imported declarations, and a tag that exactly one nominal declares at
                // this arity is that nominal's constructor — which is what types the
                // payload, so `5` is a `U64` and not a defaulted `Dec`.
                if !args.is_empty() && !matches!(*name, "Ok" | "Err" | "True" | "False") {
                    if let Some(declared) = self.nominal_declaring_tag(name, args.len()) {
                        let backing = match &declared {
                            Type::Nominal { backing, .. } => (**backing).clone(),
                            other => other.clone(),
                        };
                        self.check(expr, &backing)?;
                        return Ok(declared);
                    }
                }
                let mut payload = Vec::with_capacity(args.len());
                for arg in args {
                    payload.push(self.synth(arg)?);
                }
                Ok(Type::TagUnion {
                    tags: vec![(intern(name), payload)],
                    open: true,
                    row: Some(self.fresh_row()),
                })
            }
            // Interpolation always produces a Str, but its embedded expressions still
            // have to be checked — skipping them meant a whole class of error inside
            // `${...}` went unreported, including the one that hid broken chained
            // dispatch in this project's own test files.
            Expr::StrInterp(parts, id) => {
                for part in parts {
                    if let crate::ast::StrPart::Expr(inner) = part {
                        self.synth(inner)?;
                    }
                }
                if !self.quotable {
                    return Ok(Type::Str);
                }
                let var = self.fresh_var();
                if let Type::TypeVar(v) = var {
                    self.quote_vars.insert(v);
                }
                self.interp_literals.push((*id, var.clone()));
                Ok(var)
            }
            // A numeral is POLYMORPHIC. `15` is an I64 in `x : I64`, a `Dec` in a
            // `List(Dec)`, and — where nothing says — a FRACTIONAL value: roc prints
            // `15.0` for a bare `x = 15`. Synthesising I64 here made the interpreter
            // disagree with the compiler about its own examples.
            // A SUFFIXED literal already said what it is.
            Expr::Int(n, id) if self.suffixed.contains_key(id) => {
                self.literal_values.insert(*id, (*n, false));
                Ok(self.suffixed[id].clone())
            }
            Expr::Float(_, exact, id) if self.suffixed.contains_key(id) => {
                self.literal_values.insert(*id, (*exact, true));
                Ok(self.suffixed[id].clone())
            }
            Expr::Int(n, id) => {
                self.literal_values.insert(*id, (*n, false));
                let var = self.fresh_var();
                if let Type::TypeVar(v) = var {
                    self.numeral_vars.insert(v);
                }
                self.literals.push((*id, var.clone()));
                Ok(var)
            }
            // A fractional literal is a numeral too: roc types `0.1 + 0.2` as `Dec`
            // and prints `0.3`, and only an annotation, a suffix or an `F64` beside
            // it makes one a float.
            Expr::Float(_, exact, id) => {
                self.literal_values.insert(*id, (*exact, true));
                let var = self.fresh_var();
                if let Type::TypeVar(v) = var {
                    self.numeral_vars.insert(v);
                }
                self.literals.push((*id, var.clone()));
                Ok(var)
            }
            Expr::Ident(name, _) => {
                // `{ ..r, gone: _ }` unsets an optional field: `_` fits any slot.
                if *name == "_" {
                    return Ok(self.fresh_var());
                }
                // A name in scope has a known type now.
                if let Some(ty) = self.lookup(name) {
                    return Ok(self.apply(&ty));
                }
                // A SIBLING method, called by its bare name from inside the same
                // method block.
                if let Some(ty) = self.sibling(name) {
                    return Ok(self.apply(&ty));
                }
                // Effects the default host provides (`echo!`) have known signatures.
                if let Some((params, ret)) = crate::platform::host::lookup(name) {
                    let ret_type = if ret == "{}" { Type::Unit } else { Type::Str };
                    let mut ty = ret_type;
                    for param in params.iter().rev() {
                        let param_type = if *param == "Str" { Type::Str } else { self.fresh_var() };
                        ty = Type::Function(Box::new(param_type), Box::new(ty));
                    }
                    return Ok(ty);
                }
                Ok(self.fresh_var())
            }
            // `Bool.True` / `Bool.False` are VALUES. Treating every qualified name as a
            // function meant the checker and the evaluator disagreed about these, which
            // stayed invisible until annotations were actually checked against.
            Expr::Qualified { module: "Bool", name: "True" | "False", .. } => Ok(Type::Bool),
            // A nominal's method block binds `Type.method` as an ordinary name, so a
            // qualified reference resolves from the environment before falling back to
            // "some function" — `Counter.start` is a Counter, not a function.
            Expr::Qualified { module, name, .. } => {
                if let Some(declared) = self.declared(module, name) {
                    return Ok(declared);
                }
                // `U8.highest`, `F64.pi`, `Dec.lowest`: a VALUE of the module's type,
                // not a function waiting for arguments.
                if crate::eval::is_numeric_constant(module, name) {
                    if let Some(declared) = type_named(module) {
                        return Ok(declared);
                    }
                }
                // Qualified names are typically functions from builtins
                // Create a function type that can accept arguments
                // Input type: fresh var, Output type: fresh var
                let input_type = self.fresh_var();
                let output_type = self.fresh_var();
                Ok(Type::Function(
                    Box::new(input_type),
                    Box::new(output_type),
                ))
            }
            Expr::BinOp { left, op, right, id } => {
                let left_type = self.synth(left)?;
                let right_type = self.synth(right)?;
                self.binops.push((*id, left_type.clone()));

                match op {
                    // Arithmetic returns the type of its operands, not always I64.
                    // Returning I64 unconditionally made `quot : F64` reject
                    // `quot = 7.0 / 2.0`, which only showed up once annotations were
                    // actually checked.
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => {
                        // A nominal's own operator method says what its right operand
                        // is: `Duration.times : Duration, I64 -> Duration`.
                        if let Type::Nominal { name, .. } = self.apply(&left_type) {
                            let method = match op {
                                BinOp::Add => "plus",
                                BinOp::Sub => "minus",
                                BinOp::Mul => "times",
                                _ => "div_by",
                            };
                            if let Some(signature) = self.declared(&name, method) {
                                if let Some((params, result)) = Self::peel_params(&signature, 2) {
                                    self.unify(&params[0], &left_type)?;
                                    self.unify(&params[1], &right_type)?;
                                    return Ok(self.apply(&result));
                                }
                            }
                        }
                        self.unify(&left_type, &right_type)?;
                        Ok(self.apply(&left_type))
                    }
                    // `//` and `%` are integer-only in Roc, which is a constraint on the
                    // OPERANDS as much as a promise about the result: `7 // 2` makes
                    // both literals integers rather than leaving them to default.
                    // `//` and `%` keep their operands' type too: roc's `10 // 3` on
                    // unconstrained numerals is a `Dec`, printed `3.0`.
                    BinOp::IntDiv | BinOp::Rem => {
                        self.unify(&left_type, &right_type)?;
                        Ok(self.apply(&left_type))
                    }
                    // Comparison yields Bool, not an integer — but the two sides are
                    // still the same type, and a numeric literal on the right has no
                    // type of its own until something says so. Without this
                    // `variance == Ok(147.666666666666666666)` compares a fixed-point
                    // value against the nearest double to it, and they differ.
                    //
                    // The outcome is DISCARDED: roc rejects a mismatched comparison and
                    // this does not yet, and turning that on here would be a separate
                    // change with its own fallout.
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                        let left_type = self.apply(&left_type);
                        let _ = self.check(right, &left_type);
                        Ok(Type::Bool)
                    }
                    // `and` / `or` take and return Bool.
                    BinOp::And | BinOp::Or => Ok(Type::Bool),
                }
            }
            Expr::Lambda { params, body, .. } => {
                // For each parameter, allocate a fresh type variable
                let mut param_types = vec![];
                for _ in params.iter() {
                    param_types.push(self.fresh_var());
                }

                // The parameters have to be IN SCOPE while the body is synthesised, or
                // every mention of one is a brand new variable and nothing the body
                // learns about a parameter reaches the function's type.
                self.push_scope();
                for (param, ty) in params.iter().zip(param_types.iter()) {
                    self.bind(param, ty.clone());
                }
                self.lambda_depth += 1;
                let result = self.fresh_var();
                self.returns.push(result.clone());
                let body_type = self.synth(body);
                self.returns.pop();
                self.lambda_depth -= 1;
                self.pop_scope();
                let body_type = body_type?;
                // The body falls through to the same place a `return` jumps to.
                self.unify(&body_type, &result)?;
                let body_type = self.apply(&result);

                // Build function type: (T1 -> T2 -> ... -> Tn)
                //
                // `|| body` takes no parameters and is still a function: Roc spells its
                // empty parameter list `()`, the unit type, so it is `{} -> body` — the
                // same shape `enable_raw_mode! : () => {}` declares. Typing it as its
                // body made every zero-argument effect a value instead of a call.
                let mut result_type = body_type;
                if param_types.is_empty() {
                    result_type = Type::Function(Box::new(Type::Unit), Box::new(result_type));
                }
                for param_type in param_types.into_iter().rev() {
                    result_type = Type::Function(Box::new(param_type), Box::new(result_type));
                }

                Ok(result_type)
            }
            // A qualified builtin call — `List.len(xs)` — resolves its result the same
            // way `xs.len()` does, so the two spellings behave alike when chained.
            Expr::Call { func, args, .. } if matches!(**func, Expr::Qualified { .. }) => {
                if let Expr::Qualified { module, name, .. } = &**func {
                    // The synthetic crypto modules the parser produces for
                    // `Crypto.SHA256.*` / `Crypto.BLAKE3.*` — typed here, since they
                    // are not in any signature table.
                    if let Some(result) = Self::crypto_result(module, name) {
                        for arg in args {
                            self.synth(arg)?;
                        }
                        return Ok(result);
                    }
                    if crate::eval::simd_kind(module).is_some() {
                        for arg in args {
                            self.synth(arg)?;
                        }
                        return Ok(self.simd_result(module, name));
                    }
                    // A nominal's method (or a `Builtin.roc` member) has a declared
                    // signature, so its own types decide the result — not the builtin
                    // table, which would hand back an unconstrained variable.
                    if let Some(signature) = self.declared(module, name) {
                        // `Dict.empty()` takes no arguments and has type `{} -> Dict`:
                        // Roc spells an empty parameter list `()`, which IS the unit
                        // type, so applying it still peels one arrow.
                        let takes_unit = args.is_empty()
                            && matches!(&signature, Type::Function(param, _) if matches!(**param, Type::Unit));
                        let arity = if takes_unit { 1 } else { args.len() };
                        if let Some((params, result)) = Self::peel_params(&signature, arity) {
                            // CHECK each argument against its declared parameter, in
                            // order, so an earlier argument that pins a type variable
                            // (a list's element) is known before a later one is looked
                            // at (a lambda over that element). Synthesising every
                            // argument first — as this used to — typed the lambda's
                            // parameter as a bare variable and its body defaulted, so
                            // `List.map(xs, |r| r.?a ?? 10)` folded `Dec`s.
                            if !takes_unit {
                                for (declared, arg) in params.iter().zip(args.iter()) {
                                    let declared = self.apply(declared);
                                    self.check(arg, &declared)?;
                                }
                            }
                            let applied: Vec<Type> = params.iter().map(|p| self.apply(p)).collect();
                            self.note_polymorphic_use(name, None, &applied);
                            return Ok(self.apply(&result));
                        }
                    }
                    let mut arg_types = Vec::with_capacity(args.len());
                    for arg in args {
                        arg_types.push(self.synth(arg)?);
                    }
                    // A qualified call NAMES its receiver's type: `I64.to_str(birds)`
                    // says `birds` is an I64, and `Str.concat(a, b)` says `a` is a Str.
                    // Without this an unannotated numeral stayed a numeral and
                    // defaulted to `Dec`, so `I64.to_str(3)` printed `3.0`.
                    //
                    // A CONSTRUCTOR is the exception: `I64.from_str(s)` takes a Str and
                    // returns an I64, so its first argument is not the receiver. They
                    // are spelled `from_…` throughout `Builtin.roc`.
                    // `Box.box(x)` is `x` and `Box.unbox(b)` is `b`: the box is erased.
                    if *module == "Box" && matches!(*name, "box" | "unbox") {
                        if let Some(inner) = arg_types.first() {
                            return Ok(self.apply(inner));
                        }
                    }
                    // `F64.from_bits(n)` takes the bits as a `U64` (`U32` for `F32`),
                    // and `Dec.from_attos(n)` an `I128`: a bare numeral there is that,
                    // not the module's type and not a `Dec`.
                    if let Some(declared) = type_named(module) {
                        let bits = match (*name, &declared) {
                            ("from_bits", Type::F32) => Some(Type::U32),
                            ("from_bits", _) => Some(Type::U64),
                            ("from_attos", _) => Some(Type::I128),
                            _ => None,
                        };
                        if let Some(bits) = bits {
                            self.pin_numerals_to(&bits, &arg_types);
                        }
                    }
                    if let Some(receiver) = arg_types.first() {
                        if !name.starts_with("from_") {
                            if let Some(declared) = type_named(module) {
                                let _ = self.unify(&declared, receiver);
                                // A numeric method's OTHER numerals are the receiver's
                                // width too: `U8.shl_wrap(200, 1)` shifts by a U8, and
                                // without this the `1` defaulted to `Dec` and the call
                                // fell off the width-aware path as unknown.
                                if declared.is_integer() || declared.is_fractional() {
                                    self.pin_numerals_to(&declared, &arg_types[1..]);
                                }
                            }
                        }
                    }
                    // And a constructor RETURNS the module's type: `I64.from_str(s)` is
                    // a `Try(I64, …)`, which is what tells everything downstream of it
                    // that it is working with integers.
                    if let Some(declared) = type_named(module) {
                        // roc has no `==` on floats: `is_float_eq` says what you mean.
                        if matches!(declared, Type::F32 | Type::F64) && *name == "is_eq" {
                            return Err(TypeError {
                                message: format!("`{}.is_eq` is intentionally unavailable; use `is_float_eq`", module),
                                expected: "is_float_eq".to_string(),
                                actual: name.to_string(),
                                line: 0,
                                col: 0,
                            });
                        }
                        match *name {
                            "from_numeral" => {
                                let numeral = self.nominal_named("Numeral");
                                self.pin_numerals_to(&numeral, &arg_types);
                                if let Some(arg) = arg_types.first() {
                                    self.unify(arg, &numeral)?;
                                }
                                return Ok(Type::TagUnion {
                                    tags: vec![
                                        ("Err", vec![Type::TagUnion {
                                            tags: vec![("InvalidNumeral", vec![Type::Str])],
                                            open: false,
                                            row: None,
                                        }]),
                                        ("Ok", vec![declared]),
                                    ],
                                    open: false,
                                    row: None,
                                });
                            }
                            "from_str" => {
                                return Ok(Type::TagUnion {
                                    tags: vec![
                                        ("Err", vec![self.fresh_var()]),
                                        ("Ok", vec![declared]),
                                    ],
                                    open: true,
                                    row: None,
                                })
                            }
                            // `I64.to_str` and the `to_…` conversions say what they
                            // give back in their names.
                            "to_str" => return Ok(Type::Str),
                            _ => {}
                        }
                        if let Some(result) = self.numeric_result(&declared, name) {
                            return Ok(result);
                        }
                    }

                    // Drop the receiver: `List.fold(xs, 0, f)` has its accumulator
                    // second, matching `xs.fold(0, f)`.
                    let rest = if arg_types.is_empty() { &arg_types[..] } else { &arg_types[1..] };
                    let rest = rest.to_vec();
                    // `List.concat(xs, ys)` passes its subject first, exactly as
                    // `xs.concat(ys)` does, so the receiver is the first argument.
                    let receiver = arg_types.first().map(|t| self.apply(t));
                    return Ok(self.builtin_result(name, receiver.as_ref(), &rest));
                }
                unreachable!("guarded by the match arm")
            }
            Expr::Call { func, args, .. } => {
                // Unify the callee with `arg -> fresh` for each argument, rather than
                // requiring it to already BE a `Type::Function`.
                //
                // Identifiers still synth to a fresh type variable (there is no type
                // environment yet), so insisting on a concrete function type here
                // rejected every call to a user-defined function — `add5(37)` failed
                // with "Cannot call non-function type: $5". Unification handles both
                // cases: a variable gets bound to the function type, and a concrete
                // function type is checked as before.
                //
                // Ceiling: a non-function callee is caught only when its type is
                // already concrete — `42(1)` and `"hi"(1)` fail in `unify`, but
                // `x = 42` then `x(1)` does not, because `x` synths to a fresh var.
                // That needs a type environment for identifiers; until then this
                // errs toward accepting, which is the right trade — the previous
                // behaviour rejected every call to a user-defined function.
                // ponytail: fix by giving `synth` an environment, alongside phase 18.
                let mut current_type = self.synth(func)?;

                // `f()` is a call with NO arguments, and a zero-parameter lambda's
                // type is `{} -> result`. Without peeling that arrow here the call
                // synthesises to the function itself, so `force_strings(empty())`
                // unified `{} -> List(a)` with `List(Str)`.
                if args.is_empty() {
                    if let Type::Function(param, result) = self.apply(&current_type) {
                        if matches!(*param, Type::Unit) {
                            current_type = *result;
                        }
                    }
                }

                for arg in args {
                    // When the callee's type is already known, CHECK the argument
                    // against the parameter rather than synthesising it. A numeric
                    // literal is polymorphic and only the parameter says which type it
                    // is — that is how `safe_variance([46, 69])` makes `Dec`s.
                    let known = self.apply(&current_type);
                    if let Type::Function(param, result) = known {
                        self.check(arg, &param)?;
                        current_type = *result;
                        continue;
                    }
                    let arg_type = self.synth(arg)?;
                    let return_type = self.fresh_var();
                    let expected = Type::Function(
                        Box::new(arg_type),
                        Box::new(return_type.clone()),
                    );
                    self.unify(&current_type, &expected)?;
                    current_type = return_type;
                }

                Ok(self.apply(&current_type))
            }
            // The statement spine — a block's `let`s, `var`s and assignments — is
            // walked in a loop rather than by recursing once per statement, so a
            // block's depth is bounded by memory rather than by the Rust stack, which
            // 6,000 statements overflowed.
            Expr::Let { .. } | Expr::VarDecl { .. } | Expr::Assign { .. } => {
                let mut cursor = expr;
                loop {
                    match cursor {
                        Expr::Let { name, annotation, value, body, .. } => {
                            // `Graph.from_dict = …` is a METHOD, and its siblings are
                            // in scope unqualified while its value is checked.
                            let owns = name.rsplit_once('.').map(|(owner, _)| owner.to_string());
                            if let Some(owner) = owns.clone() {
                                self.enclosing_type.push(owner);
                            }
                            let checked = self.check_let(name, annotation, value);
                            if owns.is_some() {
                                self.enclosing_type.pop();
                            }
                            checked?;
                            cursor = body;
                        }
                        Expr::VarDecl { name, value, body, .. } => {
                            let bound = self.synth(value)?;
                            self.bind(name, bound);
                            cursor = body;
                        }
                        Expr::Assign { name, value, body, .. } => {
                            // A reassignment must agree with what the `var` already holds.
                            let assigned = self.synth(value)?;
                            if let Some(existing) = self.lookup(name) {
                                self.unify(&existing, &assigned)?;
                            }
                            cursor = body;
                        }
                        other => return self.synth(other),
                    }
                }
            }
        }
    }

    /// The binding half of a `Let`: everything but its body.
    fn check_let(
        &mut self,
        name: &&'static str,
        annotation: &Option<Type>,
        value: &Expr,
    ) -> Result<(), TypeError> {
        {
            {
                match annotation {
                    // Declared: the annotation is authoritative, and the value is
                    // CHECKED against it rather than merely inferred. That is what
                    // rejects `c : [Red, Green]` with `c = Blue`.
                    Some(declared) => {
                        let declared = &self.with_rows(declared);
                        let mut generics = Vec::new();
                        Self::type_vars_in(declared, &mut generics);

                        // The definition is checked against its OWN instance, so
                        // checking the body cannot pin the quantified variables for
                        // everyone else.
                        let instance = if generics.is_empty() {
                            declared.clone()
                        } else {
                            self.instantiate(declared, &generics)
                        };
                        // Bound BEFORE the value is checked, so a recursive call
                        // inside the body resolves through the annotation. Without
                        // this, `hanoi` calling itself produced an unresolved type and
                        // anything dispatched on the result failed.
                        self.bind_poly(name, declared.clone(), generics.clone());
                        // The declared RESULT, when it is one of the annotation's own
                        // variables, belongs to the caller: the body must produce it
                        // from its arguments, not decide what it is. Only while this
                        // body is checked, and only when no `where` clause is in play —
                        // `make : Str -> a where [a.from_quote : ...]` really does
                        // return a `Str`, and `where` is tracked by method name here,
                        // not per variable.
                        let rigid = self.rigid_result(&instance);
                        self.rigid_vars.extend(rigid.iter().copied());
                        let outcome = self.check(value, &instance);
                        for v in &rigid {
                            self.rigid_vars.remove(v);
                        }
                        outcome?;
                        self.bind_poly(name, declared.clone(), generics);
                    }
                    // Inferred: generalise, exactly as an annotated binding is. Without
                    // this, `describe = |c| ...` is monomorphic — the first call site
                    // pins its parameter and the second is a type error, even though
                    // roc accepts both. A variable is quantified only if it is free in
                    // the inferred type and NOT free in the enclosing environment,
                    // which is what keeps an enclosing lambda's parameter fixed.
                    None => {
                        let inferred = self.synth(value)?;
                        let inferred = self.apply(&inferred);

                        let mut generics = Vec::new();
                        Self::type_vars_in(&inferred, &mut generics);
                        // A numeral's type must not be quantified — `birds = 3` has
                        // ONE type, and `I64.to_str(birds)` is what fixes it.
                        // Generalising would give every use a fresh copy, so nothing
                        // could reach the literal and it would default to `Dec`.
                        // `unify` marks every variable a numeral's is bound to, either
                        // way round, so membership here is the whole test.
                        // A FUNCTION is the exception: roc monomorphises per call
                        // site, so `add_one = |x| x + 1` is a `Dec` where nothing
                        // constrains it and a `U8` where an annotation does, in the
                        // same block. Its copies stay numerals (see `instantiate`).
                        if !matches!(value, Expr::Lambda { .. }) {
                            generics.retain(|v| !self.numeral_vars.contains(v));
                        }
                        // A body that asks a parameter whether an operation OVERFLOWS
                        // is asking about a WIDTH: `a.plus_overflows(b)` is a different
                        // question at `U8` than at `I64`. Generalising leaves each use
                        // its own copy, and a copy nothing pins defaults to `Dec`,
                        // which has no such method. roc monomorphises per call site;
                        // keeping one type makes every use agree instead, which is the
                        // same answer whenever they share a width — and they must, for
                        // the program to have a single meaning here.
                        if width_sensitive(value) {
                            generics.clear();
                        }
                        // Walking the environment costs every binding in scope, so
                        // only a binding that still has something to quantify pays it.
                        // Most do not: a monomorphic `let` is the common case, and a
                        // block of thousands of them was quadratic.
                        if !generics.is_empty() {
                            let outer = self.env_type_vars();
                            generics.retain(|v| !outer.contains(v));
                        }
                        generics.dedup();

                        self.generalized_numerals
                            .extend(generics.iter().filter(|v| self.numeral_vars.contains(v)));
                        self.bind_poly(name, inferred, generics);
                    }
                }
                Ok(())
            }
        }
    }

    /// Result type of a builtin, for both `xs.len()` and `List.len(xs)`.
    ///
    /// Needed for CHAINING: the result is the next receiver, so an unconstrained
    /// variable makes a following dispatch impossible. `args` are the types written
    /// AFTER the receiver, which is what `fold` needs.
    ///
    /// `len` is I64 rather than roc's U64 because integer widths are not
    /// distinguished here — see the note on numeric unification.
    ///
    /// ponytail: a partial table, covering the builtins that exist. Anything else is
    /// unconstrained, which costs only a chain off it.
    /// The result of a SIMD call, given the vector type name and the method. Most
    /// methods give back the same vector; the lane and bit views give integers.
    fn simd_result(&mut self, module: &str, method: &str) -> Type {
        let elem = match module {
            "U8x16" => Type::U8, "I8x16" => Type::I8,
            "U16x8" => Type::U16, "I16x8" => Type::I16,
            "U32x4" => Type::U32, "I32x4" => Type::I32,
            "U64x2" => Type::U64, "I64x2" => Type::I64,
            _ => Type::I64,
        };
        let vector = Type::Nominal { name: intern(module), backing: Box::new(Type::U128), args: Vec::new() };
        match method {
            "get_lane" => elem,
            "to_u128_bits" => Type::U128,
            "to_list" => Type::List(Box::new(elem)),
            "to_inspect" => Type::Str,
            "is_eq" => Type::Bool,
            _ => vector,
        }
    }

    /// A crypto `Digest`/`Hasher` as a nominal, so `.to_hex()`/`.finish()` dispatch on
    /// it reaches `builtin_result`.
    fn crypto_nominal(name: &str) -> Type {
        Type::Nominal {
            name: intern(name),
            backing: Box::new(Type::closed_record(vec![("bytes", Type::List(Box::new(Type::U8)))])),
            args: Vec::new(),
        }
    }

    /// The result type of a synthetic crypto qualified call, or `None` if `module` is
    /// not one of them.
    fn crypto_result(module: &str, method: &str) -> Option<Type> {
        if !(module.starts_with("Sha256") || module.starts_with("Blake3")) {
            return None;
        }
        let digest = Self::crypto_nominal("CryptoDigest");
        let try_digest = Type::TagUnion {
            tags: vec![
                ("Err", vec![Type::TypeVar(u32::MAX)]),
                ("Ok", vec![digest.clone()]),
            ],
            open: true,
            row: None,
        };
        Some(match method {
            "hash" | "hash_chunks" | "finish" => digest,
            "empty" | "write" => Self::crypto_nominal("CryptoHasher"),
            "to_hex" => Type::Str,
            "to_bytes" => Type::List(Box::new(Type::U8)),
            "is_eq" => Type::Bool,
            "from_hex" | "from_bytes" => try_digest,
            _ => return None,
        })
    }

    fn builtin_result(
        &mut self,
        method: &str,
        receiver: Option<&Type>,
        args: &[Type],
    ) -> Type {
        self.note_polymorphic_use(method, receiver, args);
        // A SIMD vector answers its methods in method syntax too.
        if let Some(Type::Nominal { name, .. }) = receiver {
            if crate::eval::simd_kind(name).is_some() {
                return self.simd_result(&*name, method);
            }
        }
        // A crypto digest/hasher answers its methods in method syntax too.
        if let Some(Type::Nominal { name, .. }) = receiver {
            if *name == "CryptoDigest" || *name == "CryptoHasher" {
                if let Some(result) = Self::crypto_result(name, method) {
                    return result;
                }
                // to_hex/to_bytes/is_eq/finish are not keyed on the module here, so map
                // them directly.
                match method {
                    "to_hex" => return Type::Str,
                    "to_bytes" => return Type::List(Box::new(Type::U8)),
                    "is_eq" => return Type::Bool,
                    "finish" => return Self::crypto_nominal("CryptoDigest"),
                    "write" => return Self::crypto_nominal("CryptoHasher"),
                    _ => {}
                }
            }
        }
        match method {
            "to_str" | "inspect" => Type::Str,
            // `concat` keeps the RECEIVER's type: `Str.concat` gives a Str, and
            // `List.concat` a List. Naming only the method cannot tell them apart.
            "concat" => match receiver {
                Some(Type::List(inner)) => Type::List(inner.clone()),
                Some(Type::Str) | None => Type::Str,
                Some(other) => other.clone(),
            },
            "is_empty" | "not" => Type::Bool,
            "len" => Type::I64,
            // `Str.to_utf8` and `Str.iter_utf8` give bytes, so their element is `U8` —
            // without this a `line.to_utf8()` on an unresolved parameter came back as a
            // list of an unknown element, and a literal beside it defaulted to `Dec`.
            "to_utf8" => Type::List(Box::new(Type::U8)),
            // `fold` returns its accumulator — the first argument after the receiver,
            // which a name-only table cannot express.
            "fold" => args.first().cloned().unwrap_or_else(|| self.fresh_var()),
            // `map` keeps the receiver's shape with a new element type.
            "map" => Type::List(Box::new(self.fresh_var())),
            // An iterator is walked with the List methods here, so it is typed as the
            // List it behaves like — KEEPING the element type, so `(1..=n).iter()`
            // carries the range's element to whatever consumes it, and a later
            // `.map(f)` or annotation can still pin it rather than letting it default.
            "iter" | "iter_rev" | "rev" | "clear" => Type::List(Box::new(self.element_of(receiver))),
            // These give back what they were handed.
            "reverse" | "sort_with" | "drop_first" | "drop_last" | "append" | "prepend" => {
                receiver.cloned().unwrap_or_else(|| self.fresh_var())
            }
            // `haystack.split_first(needle)` gives `Ok({ before, after })` on a hit and
            // `Err(NotFound)` on a miss. The field types matter: destructuring the Ok
            // is how a caller gets two more `Str`s to go on splitting.
            "split_first" | "split_last" => Type::TagUnion {
                tags: vec![
                    (
                        "Ok",
                        vec![Type::closed_record(vec![
                            ("after", Type::Str),
                            ("before", Type::Str),
                        ])],
                    ),
                    ("Err", vec![self.fresh_var()]),
                ],
                open: true,
                row: None,
            },
            "join_with" | "with_ascii_uppercased" | "with_ascii_lowercased" | "trim" => Type::Str,
            // The iterator API, over a list or a range: an `Iter` is the list it walks.
            "step_by" | "keep_if" | "drop_if" | "take_first" | "take_last"
            | "sublist" | "sort" | "sort_by" | "sort_reversed" | "sort_by_reversed"
            | "sort_with_reversed" | "drop_at" | "drop_swap" | "append_sublist"
            | "collect" | "prepended" => self.list_like(receiver),
            "single" => Type::List(Box::new(receiver.cloned().unwrap_or_else(|| self.fresh_var()))),
            "with_index" => {
                let item = self.element_of(receiver);
                Type::List(Box::new(Type::Tuple(vec![Type::U64, item])))
            }
            "sum" => self.element_of(receiver),
            "any" | "all" | "contains" => Type::Bool,
            "capacity" => Type::U64,
            // `a.min(b)` is the NUMERIC two-argument form and gives back a number;
            // `xs.min()` is the list one and gives back a `Try`. Only the arity tells
            // them apart, and an unresolved receiver took the list reading — so
            // `c.x > a.x.min(b.x)` compared a number with a `Result`.
            "min" | "max" if !args.is_empty() => {
                let ty = receiver.cloned().unwrap_or_else(|| self.fresh_var());
                let _ = self.unify(&ty, &args[0]);
                self.apply(&ty)
            }
            // The INDEX of a match is a `U64`, not the element.
            "find_first_index" | "find_last_index" => {
                let index = Type::U64;
                self.try_of(index)
            }
            "find_first" | "first" | "last" | "get" | "min" | "max" | "product" => {
                let item = self.element_of(receiver);
                self.try_of(item)
            }
            "set" | "swap" | "update" | "insert" => {
                let list = self.list_like(receiver);
                self.try_of(list)
            }
            "split_at" => {
                let list = self.list_like(receiver);
                Type::closed_record(vec![("before", list.clone()), ("others", list)])
            }
            "next" if self.is_list_like(receiver) => {
                let item = self.element_of(receiver);
                let rest = self.list_like(receiver);
                Type::TagUnion {
                    tags: vec![
                        ("Done", vec![]),
                        (
                            "One",
                            vec![Type::closed_record(vec![("item", item), ("rest", rest.clone())])],
                        ),
                        ("Skip", vec![Type::closed_record(vec![("rest", rest)])]),
                    ],
                    open: true,
                    row: None,
                }
            }
            "size_hint" if self.is_list_like(receiver) => Type::TagUnion {
                tags: vec![("Known", vec![Type::U64]), ("Unknown", vec![])],
                open: true,
                row: None,
            },
            // `n.range_exclusive_to(m)` in method syntax builds a range of the
            // receiver's numeric type.
            "range_exclusive_to" | "range_inclusive_to" | "range_exclusive_from" | "range_inclusive_from" => {
                Type::Range(Box::new(receiver.cloned().unwrap_or_else(|| self.fresh_var())))
            }
            // Bit shifts, wrapping/saturating arithmetic and negate keep the receiver's
            // width, so `a.shl_wrap(2).shr_wrap(1)` on a `U32` stays a `U32` and the
            // second shift has a known receiver.
            "shl_wrap" | "shr_wrap" | "shr_zf_wrap" | "negate"
            | "plus_wrap" | "minus_wrap" | "times_wrap"
            | "plus_saturated" | "minus_saturated" | "times_saturated"
            | "bitwise_and" | "bitwise_or" | "bitwise_xor" | "bitwise_not"
                if receiver.is_some_and(|t| self.apply(t).is_integer()) =>
            {
                receiver.cloned().unwrap_or_else(|| self.fresh_var())
            }
            "fold_with_index" => args.first().cloned().unwrap_or_else(|| self.fresh_var()),
            _ => self.fresh_var(),
        }
    }

    /// Could the receiver be a list? A record or a nominal cannot — those answer
    /// `next` from their own fields and methods.
    fn is_list_like(&self, receiver: Option<&Type>) -> bool {
        !matches!(
            receiver.map(|t| self.apply(t)),
            Some(Type::Record { .. } | Type::Nominal { .. } | Type::Str | Type::Bool)
        )
    }

    /// The receiver as a list: itself if it is one, a list of something for a range.
    fn list_like(&mut self, receiver: Option<&Type>) -> Type {
        match receiver.map(|t| self.apply(t)) {
            Some(list @ Type::List(_)) => list,
            Some(Type::Range(elem)) => Type::List(elem),
            _ => Type::List(Box::new(self.fresh_var())),
        }
    }

    /// The receiver's element type, where it is known to be a list.
    fn element_of(&mut self, receiver: Option<&Type>) -> Type {
        match receiver.map(|t| self.apply(t)) {
            Some(Type::List(inner)) | Some(Type::Range(inner)) => *inner,
            _ => self.fresh_var(),
        }
    }

    /// `Try(ok, _)`: an open union of `Ok(ok)` and an `Err` of anything.
    fn try_of(&mut self, ok: Type) -> Type {
        Type::TagUnion {
            tags: vec![("Err", vec![self.fresh_var()]), ("Ok", vec![ok])],
            open: true,
            row: None,
        }
    }

    /// Bind the names a pattern introduces, with the types their positions imply.
    ///
    /// Walks the pattern against the scrutinee's type, so `Foo(n, s)` against
    /// `[Foo(I64, Str)]` gives `n: I64` and `s: Str`. Where the type is not concrete
    /// enough to say, a fresh variable is used rather than nothing — the name is still
    /// in scope, just unconstrained.
    fn bind_pattern(&mut self, pattern: &Pattern, scrutinee: &Type) {
        match pattern {
            Pattern::Wildcard => {}
            Pattern::Binding(name) => self.bind(name, scrutinee.clone()),
            Pattern::Int(_) | Pattern::Float(..) | Pattern::Str(_) => {}
            Pattern::StrInterp { segments, .. } => {
                for (name, _) in segments {
                    if *name != "_" {
                        self.bind(name, Type::Str);
                    }
                }
            }
            Pattern::As { name, inner } => {
                self.bind(name, scrutinee.clone());
                self.bind_pattern(inner, scrutinee);
            }
            // `Text.(units)` against a `Text` binds `units` to the backing.
            Pattern::Nominal { name, inner } => {
                let backing = match self.apply(scrutinee) {
                    Type::Nominal { name: n, backing, .. } if n == *name => *backing,
                    other => other,
                };
                self.bind_pattern(inner, &backing);
            }
            Pattern::Tag { name, args } => {
                // The scrutinee as known so far, and a nominal's tags are its
                // backing's: `B(n)` against a `Crate := [B(I64), ..]` binds `n : I64`.
                // Taking only a bare union left every nominal's payload unconstrained.
                let resolved = match self.apply(scrutinee) {
                    Type::Nominal { backing, .. } => *backing,
                    other => other,
                };
                let payload = match resolved {
                    Type::TagUnion { tags, .. } => tags
                        .iter()
                        .find(|(tag, _)| tag == name)
                        .map(|(_, payload)| payload.clone()),
                    _ => None,
                };
                for (index, arg) in args.iter().enumerate() {
                    let ty = payload
                        .as_ref()
                        .and_then(|p| p.get(index).cloned())
                        .unwrap_or_else(|| self.fresh_var());
                    self.bind_pattern(arg, &ty);
                }
            }
            Pattern::Record { fields, rest } => {
                // A nominal's backing record. Only the named fields are constrained,
                // so the record's other fields are free.
                let record = match scrutinee {
                    Type::Nominal { backing, .. } => (**backing).clone(),
                    other => other.clone(),
                };
                for (name, pattern) in fields {
                    let ty = match &record {
                        Type::Record { fields: known, .. } => known
                            .iter()
                            .find(|(field, _)| field == name)
                            .map(|(_, t)| t.clone()),
                        _ => None,
                    }
                    .unwrap_or_else(|| self.fresh_var());
                    // Destructuring an OPTIONAL field gives `Ok(value)` or
                    // `Err(MissingField)`, never the bare value.
                    let ty = match ty {
                        Type::Optional(inner) => Type::TagUnion {
                            tags: vec![
                                ("Err", vec![Type::TagUnion {
                                    tags: vec![("MissingField", vec![])],
                                    open: true,
                                    row: None,
                                }]),
                                ("Ok", vec![*inner]),
                            ],
                            open: true,
                            row: None,
                        },
                        other => other,
                    };
                    self.bind_pattern(pattern, &ty);
                }

                // `..rest` is a record of the fields NOT named, so its type has fewer
                // fields than the scrutinee — that is what makes it remove one.
                if let Some(rest_name) = rest {
                    let remaining = match &record {
                        Type::Record { fields: known, .. } => Type::closed_record(
                            known
                                .iter()
                                .filter(|(field, _)| {
                                    !fields.iter().any(|(named, _)| named == field)
                                })
                                .cloned()
                                .collect(),
                        ),
                        // The scrutinee is not known yet — an unannotated parameter. It
                        // is still the SAME record apart from the named fields, so
                        // sharing its type keeps the remaining fields connected to
                        // whatever the caller passes: `drop_email(p)` then
                        // `trimmed.age` is what tells `age` it is an I64.
                        //
                        // ponytail: this says "the whole record" where the truth is
                        // "the record minus `email`", which `Type` cannot spell — it
                        // has no row variable. The field IS removed at run time; the
                        // cost is that reading it back type-checks when roc would
                        // refuse. A row-polymorphic record closes the gap.
                        other => other.clone(),
                    };
                    self.bind(rest_name, remaining);
                }
            }
            Pattern::Tuple(items) => {
                for (index, item) in items.iter().enumerate() {
                    let ty = match scrutinee {
                        Type::Tuple(types) => types.get(index).cloned(),
                        _ => None,
                    }
                    .unwrap_or_else(|| self.fresh_var());
                    self.bind_pattern(item, &ty);
                }
            }
            Pattern::List { before, rest, after } => {
                let element = match scrutinee {
                    Type::List(inner) => (**inner).clone(),
                    _ => self.fresh_var(),
                };
                for item in before.iter().chain(after.iter()) {
                    self.bind_pattern(item, &element);
                }
                // `.. as name` binds the skipped middle, which is a list of the same
                // element type.
                if let Some(Some(name)) = rest {
                    self.bind(name, Type::List(Box::new(element)));
                }
            }
        }
    }

    /// The type a pattern can match.
    ///
    /// A wildcard or binding matches anything, so it gets a fresh variable. A literal
    /// matches its own type. A tag pattern is a one-tag union, exactly like a tag
    /// expression, so `unify` merges arms into the scrutinee's union.
    ///
    /// Bindings a pattern introduces are recorded by the `Match` arm, which pushes a
    /// scope and binds each one to the type its position implies.
    fn pattern_type(&mut self, pattern: &Pattern) -> Result<Type, TypeError> {
        Ok(match pattern {
            Pattern::Wildcard | Pattern::Binding(_) => self.fresh_var(),
            // A literal pattern is as polymorphic as the literal: a numeral, or a
            // string that may be a nominal with `from_quote`.
            Pattern::Int(_) | Pattern::Float(..) => {
                let var = self.fresh_var();
                if let Type::TypeVar(v) = var {
                    self.numeral_vars.insert(v);
                }
                var
            }
            Pattern::Str(_) if self.quotable => {
                let var = self.fresh_var();
                if let Type::TypeVar(v) = var {
                    self.quote_vars.insert(v);
                }
                var
            }
            Pattern::Str(_) | Pattern::StrInterp { .. } => Type::Str,
            Pattern::As { inner, .. } => self.pattern_type(inner)?,
            Pattern::Nominal { name, .. } => self.nominal_named(name),
            Pattern::Tag { name, args } => {
                let mut payload = Vec::with_capacity(args.len());
                for arg in args {
                    payload.push(self.pattern_type(arg)?);
                }
                Type::TagUnion { tags: vec![(intern(name), payload)], open: true, row: Some(self.fresh_row()) }
            }
            Pattern::Tuple(items) => {
                let mut types = Vec::with_capacity(items.len());
                for item in items {
                    types.push(self.pattern_type(item)?);
                }
                Type::Tuple(types)
            }
            Pattern::Record { fields, rest } => {
                let mut types = Vec::with_capacity(fields.len());
                for (name, pattern) in fields {
                    types.push((intern(name), self.pattern_type(pattern)?));
                }
                types.sort_by(|a, b| a.0.cmp(&b.0));
                // With `..rest` the pattern matches a record with MORE fields than it
                // names, so a closed record type would be wrong. A fresh variable lets
                // it accept any wider record; `bind_pattern` still gives `rest` the
                // precise remainder when the scrutinee's type is known.
                if rest.is_some() {
                    self.fresh_var()
                } else if types.is_empty() {
                    // `Ok({}) =>` matches the unit value, and `{}` the expression IS
                    // `Type::Unit`; an empty closed record here failed to unify with
                    // it, and `main_for_host!` matches exactly that.
                    Type::Unit
                } else {
                    Type::closed_record(types)
                }
            }
            Pattern::List { before, rest, after } => {
                // Every element pattern constrains the same element type. A `.. as
                // name` binding is a List of that element type, but bindings do not
                // reach a type environment yet, so nothing records it.
                let mut element = self.fresh_var();
                for pattern in before.iter().chain(after.iter()) {
                    let pattern_type = self.pattern_type(pattern)?;
                    element = self.join(&element, &pattern_type)?;
                }
                let _ = rest;
                Type::List(Box::new(element))
            }
        })
    }

    /// Unify two types and return the type that covers both.
    ///
    /// For most types that is just the unified type, but two tag unions join to the
    /// UNION of their tags: the branches of `if b Red else Green` have types [Red]
    /// and [Green], and the `if` as a whole is [Green, Red]. `unify` alone cannot
    /// express that, because it reports success or failure rather than a type.
    pub fn join(&mut self, t1: &Type, t2: &Type) -> Result<Type, TypeError> {
        self.unify(t1, t2)?;
        let (a, b) = (self.apply(t1), self.apply(t2));

        // Unions with rows have grown into one union already; only those without
        // (an annotation's `[A, ..]`, a builtin's) still need their tags merged.
        if a == b {
            return Ok(a);
        }
        if let (
            Type::TagUnion { tags: a_tags, open: a_open, .. },
            Type::TagUnion { tags: b_tags, open: b_open, .. },
        ) = (&a, &b)
        {
            let mut merged = a_tags.clone();
            for (name, payload) in b_tags {
                if !merged.iter().any(|(n, _)| n == name) {
                    merged.push((*name, payload.clone()));
                }
            }
            merged.sort_by(|x, y| x.0.cmp(&y.0));
            // The join is closed only if both sides were: a closed union joined with
            // an open one can still grow.
            return Ok(Type::TagUnion { tags: merged, open: *a_open || *b_open, row: None });
        }
        Ok(a)
    }

    pub fn unify(&mut self, t1: &Type, t2: &Type) -> Result<(), TypeError> {
        let t1 = self.apply(t1);
        let t2 = self.apply(t2);

        if t1 == t2 {
            return Ok(());
        }

        match (&t1, &t2) {
            // TypeVar cases
            (Type::TypeVar(v1), Type::TypeVar(v2)) if v1 == v2 => Ok(()),
            // `{}` is the unit type however it was built.
            (Type::Unit, Type::Record { fields, open: false })
            | (Type::Record { fields, open: false }, Type::Unit)
                if fields.is_empty() =>
            {
                Ok(())
            }
            // An optional field HOLDS a value of its inner type — `{ a ?: U8 }` accepts
            // `{ a: 5 }`. Peeled ahead of the TypeVar arms so an inferred field
            // variable binds to `U8` rather than to `U8?`, which the numeral guard
            // below rejects outright ("A number cannot be used as U8?").
            (Type::Optional(_), Type::TypeVar(v)) | (Type::TypeVar(v), Type::Optional(_))
                if !self.committed_vars.contains(v) =>
            {
                let inner = match (&t1, &t2) {
                    (Type::Optional(i), _) | (_, Type::Optional(i)) => (**i).clone(),
                    _ => unreachable!("matched"),
                };
                let other = Type::TypeVar(*v);
                self.unify(&inner, &other)
            }
            (Type::TypeVar(v), t) | (t, Type::TypeVar(v)) => {
                // A parameterised nominal's OWN parameter, as its declaration wrote it.
                // `expand` splices the declaration in wherever a placeholder is met, so
                // this one variable is SHARED by every occurrence of the type in the
                // program — binding it made `ConsList(I64)`'s tail become whatever the
                // last use said, and a `ConsList(ConsList(I64))` in the same function
                // then met an `I64` where a list belonged. Every real use instantiates
                // (fresh ids) before it constrains anything, so what reaches here is a
                // spliced declaration: let it through, bind nothing.
                if self.nominal_params.values().any(|params| params.contains(v)) {
                    if let Type::TypeVar(w) = t {
                        if !self.nominal_params.values().any(|params| params.contains(w)) {
                            self.subst.insert(*w, Type::TypeVar(*v));
                        }
                    }
                    return Ok(());
                }
                // A NUMERAL variable stands for a number whose width is not yet fixed,
                // not for anything at all. Letting it become a `Bool` or a function is
                // what made `x = 42` then `x(1)` type-check, and `r : { x: Bool }` accept
                // `{ x: 1 }`.
                // The constraint TRAVELS: binding a numeral's variable to another
                // makes that one a numeral too, whichever way round the binding went.
                // Without this `pair : a, a -> a` accepted `pair(1, "s")` — the `1`
                // reached `a`, and `a` was then free to become a Str.
                if let Type::TypeVar(w) = t {
                    if self.numeral_vars.contains(v) {
                        self.numeral_vars.insert(*w);
                    } else if self.numeral_vars.contains(w) {
                        self.numeral_vars.insert(*v);
                    }
                    if self.quote_vars.contains(v) {
                        self.quote_vars.insert(*w);
                    } else if self.quote_vars.contains(w) {
                        self.quote_vars.insert(*v);
                    }
                    // So does being a ROW, and a row is never a number or a string.
                    if self.row_vars.contains(v) || self.row_vars.contains(w) {
                        self.row_vars.insert(*v);
                        self.row_vars.insert(*w);
                        if [v, w].iter().any(|x| self.numeral_vars.contains(x) || self.quote_vars.contains(x)) {
                            return Err(TypeError {
                                message: "A number or a string cannot extend a tag union".to_string(),
                                expected: "a tag union".to_string(),
                                actual: t1.to_string(),
                                line: 0,
                                col: 0,
                            });
                        }
                    }
                }
                // A RIGID variable is the CALLER's choice, not the body's: in
                // `get_err : [Ok(a), Err(e)] -> e` the result is whatever `e` the
                // caller supplied, so an arm answering `""` is wrong however well a
                // `Str` would fit this one call. Binding to another variable is fine —
                // that is still "whatever the caller picks".
                if self.rigid_vars.contains(v) && !matches!(t, Type::TypeVar(_)) {
                    return Err(TypeError {
                        message: format!("{} is the caller's to choose, so this cannot answer {}", t1, t),
                        expected: t1.to_string(),
                        actual: t2.to_string(),
                        line: 0,
                        col: 0,
                    });
                }
                // A string literal is a `Str` or a nominal with `from_quote` (an
                // interpolated one, `from_interpolation`), and nothing else.
                if self.quote_vars.contains(v)
                    && !matches!(t, Type::TypeVar(_) | Type::Str)
                    && !matches!(t, Type::Nominal { name, .. }
                        if self.has_method(name, "from_quote") || self.has_method(name, "from_interpolation"))
                {
                    return Err(TypeError {
                        message: format!("A string literal cannot be used as {}", t),
                        expected: t.to_string(),
                        actual: "a string".to_string(),
                        line: 0,
                        col: 0,
                    });
                }
                // A numeral may also be a nominal with `from_numeral`.
                if self.numeral_vars.contains(v)
                    && !matches!(t, Type::TypeVar(_))
                    && !t.is_numeric()
                    && !matches!(t, Type::Nominal { name, .. } if self.has_method(name, "from_numeral"))
                {
                    return Err(TypeError {
                        message: format!("A number cannot be used as {}", t),
                        expected: t.to_string(),
                        actual: "a number".to_string(),
                        line: 0,
                        col: 0,
                    });
                }
                // A ROW takes tags, or the nominal its union turned out to be.
                if self.row_vars.contains(v)
                    && !matches!(t, Type::TypeVar(_) | Type::TagUnion { .. })
                    && !matches!(t, Type::Nominal { backing, .. } if matches!(**backing, Type::TagUnion { .. }))
                {
                    return Err(TypeError {
                        message: format!("{} is not a tag union, so it cannot extend one", t),
                        expected: "a tag union".to_string(),
                        actual: t.to_string(),
                        line: 0,
                        col: 0,
                    });
                }
                if self.occurs_check(*v, t) {
                    Err(TypeError {
                        message: format!("Infinite type: ${} = {}", v, t),
                        expected: t1.to_string(),
                        actual: t2.to_string(),
                        line: 0,
                        col: 0,
                    })
                } else {
                    self.subst.insert(*v, t.clone());
                    Ok(())
                }
            }
            // Two nominals interchange only if they are the SAME nominal — or if one
            // is BACKED BY the other. `Graph(a) :: Dict(a, List(a))` is a Dict wearing
            // a name, and within a module roc lets the backing type through, so a
            // `Graph` satisfies a `Dict` exactly as a plain record satisfies a nominal
            // over one. Refusing it made `GraphTraversal` fail with "Dict and Graph are
            // different nominal types".
            (Type::Nominal { name: a, backing: a_backing, args: a_args },
             Type::Nominal { name: b, backing: b_backing, args: b_args }) => {
                if a == b {
                    // The same nominal is the same type exactly when its ARGUMENTS
                    // are: a backing is the declaration at those arguments. It is
                    // also what ends a recursive type -- `Iter_(a)` holds a `Step(a)`
                    // holding an `Iter_(a)` -- which unifying backings would unfold
                    // for ever. Without arguments on both sides (a construction, or
                    // a nominal rocflight builds itself) the backings are compared.
                    if !a_args.is_empty() && a_args.len() == b_args.len() {
                        for (x, y) in a_args.clone().iter().zip(b_args.clone().iter()) {
                            self.unify(x, y)?;
                        }
                        return Ok(());
                    }
                    if a_backing == b_backing {
                        return Ok(());
                    }
                    return self.unify(a_backing, b_backing);
                }
                if matches!(&**a_backing, Type::Nominal { name, .. } if name == b) {
                    let other = t2.clone(); return self.unify(a_backing, &other);
                }
                if matches!(&**b_backing, Type::Nominal { name, .. } if name == a) {
                    let other = t1.clone(); return self.unify(&other, b_backing);
                }
                Err(TypeError {
                    message: format!("{} and {} are different nominal types", a, b),
                    expected: t1.to_string(),
                    actual: t2.to_string(),
                    line: 0,
                    col: 0,
                })
            }
            // A RANGE satisfies a `List`: it answers the List methods, `for` walks
            // either, and `eval::module_for` calls a range a List too. Keeping them
            // apart would make a function that loops over its argument reject a range.
            (Type::Range(a), Type::List(b)) | (Type::List(b), Type::Range(a)) => self.unify(a, b),

            // A `Range(num)` nominal iterates as a list of its element, so it satisfies a
            // `List` or a `..` range by matching its BACKING against the element — not the
            // whole list, which the generic nominal-vs-other arm below would wrongly try.
            // This is what lets `mk : U64 -> Range(U64)` accept `0..<n` and a `for` loop
            // bind the element, while the nominal identity still routes `Range.custom`.
            (Type::Nominal { name, backing, .. }, Type::List(elem) | Type::Range(elem))
            | (Type::List(elem) | Type::Range(elem), Type::Nominal { name, backing, .. })
                if *name == "Range" =>
            {
                let (backing, elem) = ((**backing).clone(), (**elem).clone());
                self.unify(&backing, &elem)
            }

            // A union inferred from tags where a nominal over tags is expected: its
            // tags are checked against the nominal's, and then its row is bound to
            // the nominal, because the union IS that nominal. A lone `Empty` passed
            // as a `Node` is a `Node` from then on, wherever a copy of it went.
            (nominal @ Type::Nominal { backing, .. }, Type::TagUnion { tags, open: true, row: Some(r) })
            | (Type::TagUnion { tags, open: true, row: Some(r) }, nominal @ Type::Nominal { backing, .. })
                if matches!(**backing, Type::TagUnion { .. }) =>
            {
                let (nominal, backing, r) = (nominal.clone(), (**backing).clone(), *r);
                self.unify(&backing, &Type::TagUnion { tags: tags.clone(), open: true, row: None })?;
                self.bind_row(r, nominal)
            }
            // Nominal against anything else: compare the backing type. roc accepts a
            // plain record where a `:=` nominal is expected, so this is deliberate
            // rather than lax — verified against the compiler.
            (Type::Nominal { backing, .. }, other) | (other, Type::Nominal { backing, .. }) => {
                let backing = (**backing).clone();
                let other = other.clone();
                self.unify(&backing, &other)
            }
            // Numeric widths are not distinguished: the evaluator has ONE integer
            // representation (i64) and one fractional one (f64), so enforcing widths
            // in the checker would be theatre — it would reject programs the
            // interpreter runs correctly. `roc check` is the authority on widths, and
            // every golden pair goes through it.
            //
            // ponytail: proper numeric literals need a constrained type variable
            // ("some integer"), which is real Hindley-Milner work. Revisit if the
            // evaluator ever grows per-width arithmetic.
            (a, b) if a.is_integer() && b.is_integer() => Ok(()),
            (a, b) if a.is_fractional() && b.is_fractional() => Ok(()),
            // A `Dec` beside an `I64` is a type error in roc (`1.0.Dec + 2.I64`), and
            // it is one here: a numeral that could be either is a variable, handled
            // above, so what reaches this line is two concrete types on opposite
            // sides of the integer/fractional divide.
            // List unification
            (Type::List(a), Type::List(b)) => self.unify(a, b),
            // Two tag unions: the shared tags must agree on payload arity and types,
            // and a closed union may not gain tags. An open union's extra tags go to
            // the other side's ROW, when it has one: `if b Red else Green` unifies
            // `[Red, ..]` with `[Green, ..]`, and both are `[Green, Red, ..]` from
            // then on. An open union without a row (an annotation's `[A, ..]`, a
            // builtin's) learns nothing itself, and `join` merges the tags it lists.
            (
                Type::TagUnion { tags: a, open: a_open, row: a_row },
                Type::TagUnion { tags: b, open: b_open, row: b_row },
            ) => {
                // Shared tags must agree on payload arity and types.
                for (name, a_payload) in a.iter() {
                    if let Some((_, b_payload)) = b.iter().find(|(n, _)| n == name) {
                        if a_payload.len() != b_payload.len() {
                            return Err(TypeError {
                                message: format!(
                                    "Tag {} used with {} payload(s) and {} payload(s)",
                                    name,
                                    a_payload.len(),
                                    b_payload.len()
                                ),
                                expected: t1.to_string(),
                                actual: t2.to_string(),
                                line: 0,
                                col: 0,
                            });
                        }
                        for (x, y) in a_payload.iter().zip(b_payload.iter()) {
                            self.unify(x, y)?;
                        }
                    }
                }

                // A CLOSED union may not gain tags. This is what makes
                // `c : [Red, Green]` reject `c = Blue` — the check that was impossible
                // while annotations never reached the AST.
                let missing = |closed: &[(&'static str, Vec<Type>)],
                               other: &[(&'static str, Vec<Type>)]|
                 -> Option<String> {
                    other
                        .iter()
                        .find(|(n, _)| !closed.iter().any(|(c, _)| c == n))
                        .map(|(n, _)| (*n).to_string())
                };

                if !a_open {
                    if let Some(extra) = missing(a, b) {
                        return Err(TypeError {
                            message: format!(
                                "Tag {} is not a member of the tag union {}",
                                extra, t1
                            ),
                            expected: t1.to_string(),
                            actual: t2.to_string(),
                            line: 0,
                            col: 0,
                        });
                    }
                }
                if !b_open {
                    if let Some(extra) = missing(b, a) {
                        return Err(TypeError {
                            message: format!(
                                "Tag {} is not a member of the tag union {}",
                                extra, t2
                            ),
                            expected: t2.to_string(),
                            actual: t1.to_string(),
                            line: 0,
                            col: 0,
                        });
                    }
                }
                // The tags each side lacks, which the other's row takes on.
                let only = |these: &[(&'static str, Vec<Type>)], those: &[(&'static str, Vec<Type>)]| -> Vec<(&'static str, Vec<Type>)> {
                    these.iter().filter(|(n, _)| !those.iter().any(|(m, _)| m == n)).cloned().collect()
                };
                let (only_a, only_b) = (only(a, b), only(b, a));
                let grown = |tags: Vec<(&'static str, Vec<Type>)>, open: bool, row: Option<u32>| Type::TagUnion { tags, open, row };
                match (*a_row, *b_row) {
                    (Some(r), Some(s)) if r == s => {
                        if let Some((extra, _)) = only_a.first().or(only_b.first()) {
                            return Err(TypeError {
                                message: format!("Tag {} is not a member of the tag union {}", extra, t1),
                                expected: t1.to_string(),
                                actual: t2.to_string(),
                                line: 0,
                                col: 0,
                            });
                        }
                        Ok(())
                    }
                    (Some(r), Some(s)) => match (only_a.is_empty(), only_b.is_empty()) {
                        (true, true) => self.bind_row(r, Type::TypeVar(s)),
                        (false, true) => self.bind_row(s, grown(only_a, true, Some(r))),
                        (true, false) => self.bind_row(r, grown(only_b, true, Some(s))),
                        (false, false) => {
                            let rest = self.fresh_row();
                            self.bind_row(r, grown(only_b, true, Some(rest)))?;
                            self.bind_row(s, grown(only_a, true, Some(rest)))
                        }
                    },
                    // Against a union without a row, a closed one ends this one's row,
                    // and an open one leaves it open under a fresh row, to go on
                    // learning.
                    (Some(r), None) => {
                        let rest = if *b_open { Some(self.fresh_row()) } else { None };
                        self.bind_row(r, grown(only_b, *b_open, rest))
                    }
                    (None, Some(s)) => {
                        let rest = if *a_open { Some(self.fresh_row()) } else { None };
                        self.bind_row(s, grown(only_a, *a_open, rest))
                    }
                    (None, None) => Ok(()),
                }
            }
            // Tuples unify positionally and only at the same arity.
            (Type::Tuple(a), Type::Tuple(b)) if a.len() == b.len() => {
                for (x, y) in a.iter().zip(b.iter()) {
                    self.unify(x, y)?;
                }
                Ok(())
            }
            // An optional field unifies with the value's type whether it is present
            // or not — that is what `?:` means.
            (Type::Optional(inner), other) | (other, Type::Optional(inner)) => {
                let inner = (**inner).clone();
                let other = other.clone();
                self.unify(&inner, &other)
            }
            // `{}` where a record of only optional fields is expected: the unit IS the
            // empty record, and every field it lacks may be absent.
            (Type::Unit, Type::Record { fields, open }) | (Type::Record { fields, open }, Type::Unit)
                if *open || fields.iter().all(|(_, t)| matches!(t, Type::Optional(_))) =>
            {
                Ok(())
            }
            // `True` and `False` are the booleans spelled as tags: a `True` pattern
            // against a `Bool` scrutinee, or a `Bool` where `[True, False]` is wanted.
            (Type::Bool, Type::TagUnion { tags, .. }) | (Type::TagUnion { tags, .. }, Type::Bool)
                if tags.iter().all(|(t, p)| matches!(*t, "True" | "False") && p.is_empty()) =>
            {
                Ok(())
            }
            // Record unification, walked by NAME rather than position, because an
            // optional field may be absent on one side and so the two lists can differ
            // in length. A field missing from one side is only acceptable when the
            // other declares it optional.
            (
                Type::Record { fields: a, open: a_open },
                Type::Record { fields: b, open: b_open },
            ) => {
                let missing_ok = |ty: &Type| matches!(ty, Type::Optional(_));

                for (name, ta) in a.iter() {
                    match b.iter().find(|(n, _)| n == name) {
                        Some((_, tb)) => self.unify(ta, tb)?,
                        // An OPEN record promises only the fields it lists, so the
                        // other side may lack the rest. A CLOSED one lists them all,
                        // and roc refuses a wider value against it — even when the
                        // extra field is optional, because the layouts differ.
                        None if *b_open => {}
                        None => {
                            return Err(TypeError {
                                message: format!("Record is missing field `{}`", name),
                                expected: t1.to_string(),
                                actual: t2.to_string(),
                                line: 0,
                                col: 0,
                            })
                        }
                    }
                }
                for (name, tb) in b.iter() {
                    if a.iter().any(|(n, _)| n == name) {
                        continue;
                    }
                    if !*a_open && !missing_ok(tb) {
                        return Err(TypeError {
                            message: format!("Record has an unexpected field `{}`", name),
                            expected: t1.to_string(),
                            actual: t2.to_string(),
                            line: 0,
                            col: 0,
                        });
                    }
                }
                // An open record that meets a closed one IS that record: the
                // parameter of `|acc, x| .. k.key == x.key ..`, folded over a list
                // of `{ key, i }`, has an `i` too. Rebound by value, as field access
                // grows one: the types arrive here already applied, so the variable
                // that held it is not known.
                if *a_open && !*b_open {
                    self.subst.rebind_applied(&t1, t2.clone());
                } else if *b_open && !*a_open {
                    self.subst.rebind_applied(&t2, t1.clone());
                }
                Ok(())
            }
            // Function unification
            (Type::Function(a1, b1), Type::Function(a2, b2)) => {
                self.unify(a1, a2)?;
                self.unify(b1, b2)
            }
            // Type mismatch
            _ => Err(TypeError {
                message: format!("Cannot unify {} with {}", t1, t2),
                expected: t1.to_string(),
                actual: t2.to_string(),
                line: 0,
                col: 0,
            }),
        }
    }

    /// Bind an open union's row, refusing a union that would contain itself.
    fn bind_row(&mut self, row: u32, to: Type) -> Result<(), TypeError> {
        // `unify` applies both sides first, so a row reaching here is unbound.
        debug_assert!(self.subst.get(row).is_none(), "row ${} bound twice", row);
        if self.occurs_check(row, &to) {
            return Err(TypeError {
                message: format!("Infinite type: a tag union contains itself through {}", to),
                expected: to.to_string(),
                actual: format!("${}", row),
                line: 0,
                col: 0,
            });
        }
        self.subst.insert(row, to);
        Ok(())
    }

    /// Occurs check: prevent infinite types.
    ///
    /// Every compound type is looked inside, not just lists and functions. A variable
    /// bound to a record containing itself — `{ ..p, next: p }` — made `apply` recurse
    /// for ever, and the checker went down with a stack overflow instead of a message.
    fn occurs_check(&self, var: u32, ty: &Type) -> bool {
        let ty = self.apply(ty);
        let mut vars = Vec::new();
        Self::type_vars_in(&ty, &mut vars);
        vars.contains(&var)
    }
}

impl Default for TypeChecker {
    fn default() -> Self {
        Self::new()
    }
}

/// `module_of`, widened for OPERATORS only.
///
/// A tuple and a bare tag union name no method block, and saying so is what keeps a
/// lone `is_eq` from answering `(1, "x") == (1, "x")` or `Green == Green`. They are not
/// answered for ordinary dispatch, where "no module" has to stay "unresolved" — a
/// `Try` really has no `to_str`, and reporting that is the point.
fn operator_module(ty: &Type) -> Option<&'static str> {
    Some(match ty {
        Type::Tuple(_) => "Tuple",
        // A nominal OVER a tag union arrives as `Type::Nominal` and is answered by
        // `module_of`; this is the case where the value is tags and nothing more.
        Type::TagUnion { .. } => "Tags",
        other => return module_of(other),
    })
}

/// The module whose method block owns a value of this type.
///
/// It agrees with `eval::module_for`, which answers the same question from a runtime
/// value: a method has to resolve the same way whether the compiler picks it or the VM
/// does. `None` means the type does not name a module — a bare record or tag has no
/// nominal to dispatch through, because roc erases nominals and the value carries no
/// tag to recover one from.
fn module_of(ty: &Type) -> Option<&'static str> {
    Some(match ty {
        Type::Str => "Str",
        Type::Bool => "Bool",
        Type::List(_) => "List",
        // A range answers the List methods — `(1..=n).iter().fold(..)` is the idiom —
        // and `eval::module_for` says the same about the runtime value.
        Type::Range(_) => "List",
        // Every integer width is its OWN module: `U8.shl_wrap(200, 1)` is 144 and
        // `I64.shl_wrap(200, 1)` is 400, and the width is what the module name carries
        // to `eval::call_numeric`. The runtime value is one `i128` for all of them.
        Type::U8 => "U8",
        Type::U16 => "U16",
        Type::U32 => "U32",
        Type::U64 => "U64",
        Type::U128 => "U128",
        Type::I8 => "I8",
        Type::I16 => "I16",
        Type::I32 => "I32",
        Type::I64 => "I64",
        Type::I128 => "I128",
        Type::F32 => "F32",
        Type::F64 => "F64",
        Type::Dec => "Dec",
        // A tuple has no method block and cannot be a nominal's backing, so naming it
        // here is what says "no user method owns this operator". A RECORD is left
        // unnamed on purpose: roc erases nominals, so an unannotated `Money.{ cents: 5 }`
        // is only a record to the checker and may still own the method.

        // `c : Counter` dispatches into `Counter`'s own method block. This is the case
        // the runtime cannot reconstruct, which is why the compiler has to.
        // Interned because a method name is `&'static str` everywhere else in the
        // compiler, and this is asked once per dispatch node.
        Type::Nominal { name, .. } => crate::memory::string_pool::intern(name),
        _ => return None,
    })
}

/// The type a module name stands for, where it stands for one.
///
/// `I64` is a type; `List` needs an argument and `Dict` is not modelled with its own,
/// so only the ones a bare name fully determines are answered here.
fn type_named(module: &str) -> Option<Type> {
    Some(match module {
        "Str" => Type::Str,
        "Bool" => Type::Bool,
        "U8" => Type::U8, "U16" => Type::U16, "U32" => Type::U32,
        "U64" => Type::U64, "U128" => Type::U128,
        "I8" => Type::I8, "I16" => Type::I16, "I32" => Type::I32,
        "I64" => Type::I64, "I128" => Type::I128,
        "F32" => Type::F32, "F64" => Type::F64, "Dec" => Type::Dec,
        _ => return None,
    })
}

/// Was this expression written as a nominal construction — `Point.{ x: 3 }` or
/// `UserId.(7)`?
fn expr_id_has_nominal(checker: &TypeChecker, expr: &Expr) -> bool {
    checker.nominal_literals.contains_key(&expr.id())
}


/// Does `expr` ask about a WIDTH — `a.plus_overflows(b)` and its siblings, whose answer
/// differs per integer width? See the call site in `check_let`.
fn width_sensitive(expr: &Expr) -> bool {
    if matches!(expr, Expr::Dispatch { method, .. } if method.ends_with("_overflows")) {
        return true;
    }
    expr.children().into_iter().any(width_sensitive)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signatures_rows_are_numbered_above_its_own_variables() {
        // The parser numbers `a` from the checker's own space, so a fresh checker
        // would otherwise mint the `..`'s row as `$0` too, and instantiating the
        // signature would make the row and `a` one variable.
        let mut checker = TypeChecker::new();
        let signature = Type::Function(
            Box::new(Type::TypeVar(0)),
            Box::new(Type::TagUnion { tags: vec![("X", vec![])], open: true, row: None }),
        );
        let Type::Function(_, result) = checker.with_rows(&signature) else { panic!("a function") };
        let Type::TagUnion { row: Some(row), .. } = *result else { panic!("a row: {}", result) };
        assert_ne!(row, 0);
    }

    #[test]
    fn a_nominals_placeholder_is_not_a_variable_to_number_above() {
        // `Node`'s placeholder backing is the parser's `$u32::MAX`; counting it
        // overflowed, and in a release build wrapped, leaving the row at `$0`.
        let mut checker = TypeChecker::new();
        let node = Type::Nominal { name: "Node", backing: Box::new(Type::TypeVar(u32::MAX)), args: Vec::new() };
        let signature = Type::Function(
            Box::new(Type::Tuple(vec![node, Type::TypeVar(0)])),
            Box::new(Type::TagUnion { tags: vec![("X", vec![])], open: true, row: None }),
        );
        let Type::Function(_, result) = checker.with_rows(&signature) else { panic!("a function") };
        let Type::TagUnion { row: Some(row), .. } = *result else { panic!("a row: {}", result) };
        assert_eq!(row, 1);
    }
}
