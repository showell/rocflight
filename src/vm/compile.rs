//! AST → bytecode.
//!
//! Everything this pass does is work a tree-walker would redo on every execution:
//! deciding which register a name lives in, which chunk a call goes to, which of an
//! enclosing function's values a closure needs, where a branch lands. Doing it once is
//! the entire point.
//!
//! **Refusals are explicit.** Anything this cannot lower is an `Err` naming it. There
//! is no fallback interpreter to quietly take over, which is the point of there being
//! one engine: a program either compiles or says why. The `expr` match is exhaustive
//! over `Expr` on purpose, so a variant added to the AST is a compile error here.

use super::{Chunk, ChunkId, CondKind, Op, Program, Reg};
use crate::ast::{Expr, MatchArm, Pattern, StrPart};
use crate::eval::Value;
use std::rc::Rc;

/// The top level, resolved: what is a function, what is a value, and where each lives.
struct Tops {
    /// `(name, chunk, arity)` for every top-level binding whose value is a lambda.
    fns: Vec<(&'static str, ChunkId, u16)>,
    /// Every other top-level binding, in declaration order. The index is the slot.
    globals: Vec<&'static str>,
    /// `exposing [hello]` — a bare name standing for a module's qualified one.
    aliases: Vec<(&'static str, &'static str)>,
    /// Does this program define an operator method (`plus`, `is_eq`, …) on a nominal?
    ///
    /// Almost none do, so the ordinary `Bin` opcode skips the check entirely and only
    /// a program that overloads an operator pays for the lookup.
    operator_methods: bool,
    /// The three above, by name.
    ///
    /// They were linear scans, and the top level looks EVERY binding up as it compiles
    /// it, so compiling was quadratic in the number of top-level names: 1,000
    /// declarations took 1.8ms, 8,000 took 94ms. First wins on a duplicate, which is
    /// what `position`/`find` did.
    fn_at: std::collections::HashMap<&'static str, (ChunkId, u16)>,
    global_at: std::collections::HashMap<&'static str, u32>,
    alias_at: std::collections::HashMap<&'static str, &'static str>,
}

/// The method names roc maps its operators onto. `a + b` IS `a.plus(b)`.
const OPERATOR_METHODS: &[&str] = &[
    "plus", "minus", "times", "div_by", "div_trunc_by", "rem_by", "is_lt", "is_gt",
    "is_lte", "is_gte", "is_eq",
];

impl Tops {
    /// Every top-level function whose name ends in `.method` — a nominal's method
    /// block compiles to exactly that, so this is `methods_named` done at compile time.
    fn methods(&self, method: &str) -> Vec<(&'static str, ChunkId, u16)> {
        let suffix = format!(".{}", method);
        self.fns
            .iter()
            .filter(|(name, ..)| name.ends_with(&suffix))
            .copied()
            .collect()
    }

    fn func(&self, name: &str) -> Option<(ChunkId, u16)> {
        self.fn_at.get(name).copied()
    }

    fn global(&self, name: &str) -> Option<u32> {
        self.global_at.get(name).copied()
    }

    /// The qualified name a bare one was exposed as, if any.
    fn alias(&self, name: &str) -> Option<&'static str> {
        self.alias_at.get(name).copied()
    }

    /// Build the three indexes, once, after every name is known. `or_insert` and not
    /// `insert`: a duplicate name resolved to the FIRST one when these were scans.
    fn index(&mut self) {
        for (name, chunk, arity) in &self.fns {
            self.fn_at.entry(name).or_insert((*chunk, *arity));
        }
        for (i, name) in self.globals.iter().enumerate() {
            self.global_at.entry(name).or_insert(i as u32);
        }
        for (bare, full) in &self.aliases {
            self.alias_at.entry(bare).or_insert(full);
        }
    }
}

/// Where a capture's value comes from, as seen by the ENCLOSING function.
///
/// A closure copies its captures out of the frame that creates it, so each one is
/// either one of that frame's registers, one of *its* captures (the grandparent's
/// value, threaded down), or the enclosing function itself.
#[derive(Debug, Clone, Copy)]
enum CapSource {
    Local(Reg),
    Capture(u16),
    /// The enclosing function's own closure — a nested lambda that calls the named
    /// function it is written inside.
    Enclosing,
}

/// How a name resolved.
enum Found {
    /// A register in this frame: a parameter or a `let`.
    Local(Reg),
    /// A register holding a `Value::Cell`: a captured `var`, read through it.
    LocalCell(Reg),
    /// A captured `var`: the capture is the cell.
    CaptureCell(u16),
    /// The function this name belongs to is the one running.
    SelfRef,
    /// An enclosing function's value, copied in when the closure was made.
    Capture(u16),
    /// A top-level binding that is not a function.
    Global(u32),
    /// Resolved, but the VM will not compile this use of it.
    Refused(String),
    /// A top-level function, which is a chunk rather than a value.
    Func(ChunkId, u16),
}

/// One local module of a multi-file program: its AST, the type its names hang on, and
/// the names it exposes bare.
pub struct Module<'a> {
    pub ast: &'a Expr,
    /// `Dir/Hello` exposes its names as `Hello.name`.
    pub type_name: &'static str,
    pub exposed: Vec<&'static str>,
}

/// Everything a program is made of.
pub struct Unit<'a> {
    /// How many of the leading `modules` are the PRECOMPILED GROUP — `Builtin.roc`.
    ///
    /// They get their own top-level chunk and their own block of chunk ids, ahead of
    /// everything else, so that block is the same for every program and can be compiled
    /// once and reused. See `artifact::Prefix`.
    pub prefix_modules: usize,
    /// Compiled into the SAME program as the app, ahead of it, so a module's top level
    /// is part of this one's. The tree-walker got the same effect by evaluating each
    /// module into the shared global scope first.
    pub modules: Vec<Module<'a>>,
    pub app: &'a Expr,
    /// The name the app header declared, if it is an app rather than a module.
    pub entry: Option<&'a str>,
    /// `import "data.txt" as text : Str` — file contents, read before compiling.
    pub ingested: Vec<(&'static str, String)>,
    /// The `BinOp` nodes the checker proved have integer operands, from
    /// `TypeChecker::integer_binops`. Empty is always safe: it just means every
    /// operator goes through the generic opcode.
    pub integer_binops: std::collections::HashSet<crate::ast::NodeId>,
    /// Which module each `Dispatch` node's receiver belongs to, from
    /// `TypeChecker::dispatch_modules`. Empty is always safe: it just means every
    /// dispatch resolves the way it did before the checker was consulted.
    pub dispatch_modules: std::collections::HashMap<crate::ast::NodeId, &'static str>,
    /// Which module each `BinOp` node's operands belong to, from
    /// `TypeChecker::binop_modules`. Empty means no operator is dispatched.
    pub binop_modules: std::collections::HashMap<crate::ast::NodeId, &'static str>,
    /// The nominals in scope, as `(name, backing)`, from `Parser::nominals`. What the
    /// VM makes of them is `Program::nominal_shapes`.
    pub nominals: Vec<(&'static str, crate::types::Type)>,
    /// The names among `nominals` that were declared with `::`, the opaque form.
    pub opaque_nominals: Vec<&'static str>,
    /// Literal nodes the checker typed as `Dec`, from `TypeChecker::dec_literals`.
    /// They are lowered as fixed-point values rather than integers or floats.
    pub dec_literals: std::collections::HashSet<crate::ast::NodeId>,
    /// Literal nodes the checker typed as `F32`, from `TypeChecker::f32_literals`.
    pub f32_literals: std::collections::HashSet<crate::ast::NodeId>,
    /// Literal nodes typed `U128`; emitted as `Value::U128`.
    pub u128_literals: std::collections::HashSet<crate::ast::NodeId>,
    /// Record literals with optional fields left out, and which, from
    /// `TypeChecker::missing_fields`: each is filled with `Value::Missing`.
    pub missing_fields: std::collections::HashMap<crate::ast::NodeId, Vec<&'static str>>,
    /// Literals that are a nominal built through one of its conversions, and which
    /// nominal and conversion, from `TypeChecker::literal_conversions`.
    pub conversions: std::collections::HashMap<crate::ast::NodeId, (&'static str, &'static str)>,
    /// Every numeric literal's text, for `from_numeral`; from `Parser::numeral_texts`.
    pub numeral_texts: std::collections::HashMap<crate::ast::NodeId, String>,
    /// From `TypeChecker::coerce_values` and `coerce_params`: where a raw literal may
    /// arrive at run time and which nominal it should be.
    pub coerce_values: std::collections::HashMap<crate::ast::NodeId, &'static str>,
    pub coerce_params: std::collections::HashMap<crate::ast::NodeId, Vec<(usize, &'static str)>>,
    /// From `TypeChecker::zero_sized_capacity`: `List.with_capacity` calls compiled
    /// with a capacity of 0, as roc never allocates for a zero-sized element.
    pub zero_sized_capacity: std::collections::HashSet<crate::ast::NodeId>,
    /// From `TypeChecker::match_types`: scrutinee types that mention such a nominal.
    pub match_types: std::collections::HashMap<crate::ast::NodeId, crate::types::Type>,
    /// See `TypeChecker::for_iter_calls`.
    pub for_iter_calls: std::collections::HashSet<crate::ast::NodeId>,
    /// See `TypeChecker::default_sites`: a record/unit literal node -> the nominal it
    /// builds, whose omitted defaulted/optional fields the compiler materializes.
    pub default_sites: std::collections::HashMap<crate::ast::NodeId, &'static str>,
    /// Per-nominal defaulted fields and their default expressions.
    pub nominal_defaults: Vec<(String, Vec<(String, crate::ast::Expr)>)>,
    /// Run the top-level `expect`s as well as the program: `rocflight eval` does, and
    /// reports a failing one as roc does, at compile time.
    pub run_expects: bool,
    /// Literal nodes nothing pinned down, from `TypeChecker::fractional_literals`.
    /// roc defaults an unconstrained numeral to a fractional type.
    pub fractional_literals: std::collections::HashSet<crate::ast::NodeId>,
    /// `Json.parse` call sites and the type each must produce, from
    /// `TypeChecker::parse_targets`. Passed to the builtin as an extra argument.
    pub parse_targets: std::collections::HashMap<crate::ast::NodeId, crate::types::Type>,
    /// `collect()` call sites and the nominal whose `from_iter` builds the result,
    /// from `TypeChecker::collect_targets`.
    pub collect_targets: std::collections::HashMap<crate::ast::NodeId, String>,
    /// Bare names the builtin module DECLARES but does not define — its low-level ops.
    ///
    /// They are calls into Rust, so a missing one is a runtime message naming the op
    /// rather than a compile error on a name that is, after all, declared. That is how
    /// a qualified builtin like `Str.repeat` already behaves.
    pub intrinsics: std::collections::HashSet<&'static str>,
    /// A nested nominal's enclosing owner, from `Parser::enclosing_owners`: the
    /// owner a sibling lookup tries next.
    pub enclosing_owners: std::collections::HashMap<&'static str, &'static str>,
    /// `roc test` semantics: run the top-level `expect`s and tally them. A normal run
    /// SKIPS them — roc only treats a top-level `expect` as a test — while an `expect`
    /// inside a function body runs either way.
    pub test_mode: bool,
}

/// Compile a single-file program.
pub fn compile(ast: &Expr, entry: Option<&str>) -> Result<Program, String> {
    compile_unit(&Unit {
        prefix_modules: 0,
        modules: Vec::new(),
        app: ast,
        entry,
        ingested: Vec::new(),
        integer_binops: std::collections::HashSet::new(),
        dispatch_modules: std::collections::HashMap::new(),
        binop_modules: std::collections::HashMap::new(),
        nominals: Vec::new(),
        opaque_nominals: Vec::new(),
        dec_literals: std::collections::HashSet::new(),
        f32_literals: std::collections::HashSet::new(),
        u128_literals: std::collections::HashSet::new(),
        conversions: std::collections::HashMap::new(),
        numeral_texts: std::collections::HashMap::new(),
        coerce_values: std::collections::HashMap::new(),
        coerce_params: std::collections::HashMap::new(),
        zero_sized_capacity: std::collections::HashSet::new(),
        match_types: std::collections::HashMap::new(),
        for_iter_calls: std::collections::HashSet::new(),
        default_sites: std::collections::HashMap::new(),
        nominal_defaults: Vec::new(),
        missing_fields: std::collections::HashMap::new(),
        run_expects: false,
        fractional_literals: std::collections::HashSet::new(),
        parse_targets: std::collections::HashMap::new(),
        collect_targets: std::collections::HashMap::new(),
        intrinsics: std::collections::HashSet::new(),
        enclosing_owners: std::collections::HashMap::new(),
        // The bare helper is what the unit tests and `vm::eval` use: run everything.
        test_mode: true,
    })
}

/// Compile a whole program, local modules and ingested files included.
pub fn compile_unit(unit: &Unit) -> Result<Program, String> {
    compile_unit_with(unit, None)
}

/// Compile, on top of `Builtin.roc` already compiled.
///
/// The prefix owns chunks `0..P` — chunk 0 being the builtins' own top level, which
/// becomes this program's `prelude` — and this program's chunks are numbered after
/// them. Nothing is renumbered: the prefix keeps the ids it was compiled with, which is
/// sound because its bytecode does not depend on the program that loads it. See
/// `artifact::Prefix` for the measurement that establishes that.
pub fn compile_unit_with(
    unit: &Unit,
    prefix_owned: Option<crate::artifact::Prefix>,
) -> Result<Program, String> {
    compile_reporting(unit, prefix_owned).map(|(program, _)| program)
}

/// What a compile made of its precompiled GROUP — phase A — so `gen-artifact` can write
/// it out. `None` when there was no group, or when one was loaded rather than compiled.
pub struct Group {
    /// How many chunks the group owns: ids `0..chunks` of the finished program.
    pub chunks: usize,
    pub fns: Vec<(&'static str, ChunkId, u16)>,
    pub globals: Vec<&'static str>,
}

pub fn compile_reporting(
    unit: &Unit,
    prefix_owned: Option<crate::artifact::Prefix>,
) -> Result<(Program, Option<Group>), String> {
    let (ast, entry) = (unit.app, unit.entry);
    // The top level is a chain of `let`s ending in an expression.
    //
    // A binding to `_` whose value is itself a chain is flattened INTO the top level:
    // the parser produces that shape for a file whose declarations are followed by
    // top-level `expect`s, and its `main!` and helpers are the program's top level
    // however the tree came out. Without this they are block-locals, and a helper
    // declared below `main!` cannot be referenced from inside it.
    let mut bindings: Vec<(&'static str, &Expr)> = Vec::new();
    let mut statements: Vec<&Expr> = Vec::new();

    // Each module's top level, ahead of the app's. Its trailing expression keeps its
    // place as a statement — that is where a module's `expect`s live.
    let mut aliases: Vec<(&'static str, &'static str)> = Vec::new();
    // Where the precompiled group's bindings and statements end.
    let (mut split_bindings, mut split_statements) = (0usize, 0usize);
    for (at, module) in unit.modules.iter().enumerate() {
        if at == unit.prefix_modules {
            split_bindings = bindings.len();
            split_statements = statements.len();
        }
        let mut cursor = module.ast;
        while let Expr::Let { name, value, body, .. } = cursor {
            bindings.push((name, value.as_ref()));
            cursor = body;
        }
        statements.push(cursor);
        // `exposing [hello]` makes `Hello.hello` reachable as plain `hello`.
        for name in &module.exposed {
            aliases.push((name, qualify(module.type_name, name)));
        }
    }
    if unit.prefix_modules >= unit.modules.len() {
        split_bindings = bindings.len();
        split_statements = statements.len();
    }

    let mut cursor = ast;
    let tail = loop {
        match cursor {
            Expr::Let { name: "_", value, body, .. } if matches!(**value, Expr::Let { .. }) => {
                let mut inner = value.as_ref();
                while let Expr::Let { name, value, body, .. } = inner {
                    bindings.push((name, value.as_ref()));
                    inner = body;
                }
                // Whatever the inner chain ended in was bound to `_` and discarded, so
                // it stays a statement: run for its effects, value thrown away.
                statements.push(inner);
                cursor = body;
            }
            // `_ = <expr>` binds nothing: it is a statement run for its effect, and
            // that is the shape a top-level `expect` arrives in. As a binding it
            // would take a global slot under the name `_` and, worse, be compiled
            // as an ordinary in-function `expect` rather than as a test.
            Expr::Let { name: "_", value, body, .. } => {
                statements.push(value.as_ref());
                cursor = body;
            }
            Expr::Let { name, value, body, .. } => {
                bindings.push((name, value.as_ref()));
                cursor = body;
            }
            other => break other,
        }
    };

    // Chunk 0 is the top level itself, so top-level functions start at 1. Ids are
    // handed out before any body is compiled — that is what lets two functions call
    // each other, and a function call one declared further down the file.
    // The compiled prefix, if there is one, owns chunks 0..P — chunk 0 being the
    // builtins' own top level, which runs before this program's.
    let prefix = prefix_owned.as_ref();
    let prefix_chunks = prefix.map_or(0, |p| p.chunks.len());
    let prefix_fns = prefix.map_or(0, |p| p.fns.len());
    // Chunk 0 is the group's top level whenever there IS a group — compiled here as
    // phase A, or loaded as a prefix — and it runs before this program's own.
    let mut prelude: Option<ChunkId> = prefix.map(|_| 0);
    // The first id this program may use. Its top level takes it, then its functions.
    // Where THIS program's own top level lands. Without phase A it is chunk 0 (or the
    // first free id after a loaded prefix); with phase A it is after the group's chunks,
    // which is only known once the group is compiled.
    let mut top_slot = prefix_chunks as ChunkId;
    let mut tops = Tops {
        fns: prefix.map_or_else(Vec::new, |p| p.fns.clone()),
        globals: prefix.map_or_else(Vec::new, |p| p.globals.clone()),
        aliases,
        operator_methods: false,
        fn_at: std::collections::HashMap::new(),
        global_at: std::collections::HashMap::new(),
        alias_at: std::collections::HashMap::new(),
    };
    // An ingested file is a top-level Str, known before the program starts.
    for (name, _) in &unit.ingested {
        tops.globals.push(name);
    }
    // PHASE A is the precompiled group — `Builtin.roc` — and it gets its own top-level
    // chunk (id 0) and its own block of function ids, ahead of everything else. That is
    // what makes the block the same for every program and reusable. When a compiled
    // prefix was handed in there is no phase A: its chunks ARE that block.
    let phase_a = if prefix.is_some() { 0 } else { split_bindings };
    let two_phase = phase_a > 0;
    // Chunk 0 of the whole program: the group's top level when there is one, this
    // program's own otherwise.
    if two_phase {
        prelude = Some(0);
    }
    let is_lambda = |v: &&Expr| matches!(v, Expr::Lambda { .. });
    let a_lambdas = bindings.iter().take(phase_a).filter(|(_, v)| is_lambda(v)).count();
    let b_lambdas = bindings.iter().skip(phase_a).filter(|(_, v)| is_lambda(v)).count();
    let mut next_chunk = top_slot + 1;
    for (name, value) in bindings.iter().take(phase_a) {
        match value {
            Expr::Lambda { params, .. } => {
                tops.fns.push((name, next_chunk, params.len() as u16));
                next_chunk += 1;
            }
            _ => tops.globals.push(name),
        }
    }
    let group_globals = tops.globals.len();
    // The GLOBAL slots can all be handed out now — a slot index does not depend on a
    // chunk id. The remaining functions' chunk ids cannot, when there is a phase A:
    // they come after its nested lambdas, and how many there are is only known once it
    // has been compiled. Without a phase A there is nothing to wait for.
    for (name, value) in bindings.iter().skip(phase_a) {
        match value {
            Expr::Lambda { params, .. } if !two_phase => {
                tops.fns.push((name, next_chunk, params.len() as u16));
                next_chunk += 1;
            }
            Expr::Lambda { .. } => {}
            _ => tops.globals.push(name),
        }
    }
    tops.operator_methods = tops.fns.iter().any(|(name, ..)| {
        OPERATOR_METHODS.iter().any(|m| name.ends_with(&format!(".{}", m)))
    });
    // Every name is known now and none is added after this point.
    tops.index();

    let mut c = Compiler {
        tops,
        // One reserved slot per chunk whose id is already known. Nested lambdas
        // reserve theirs as they are found.
        // One slot per chunk id, so an index into this IS a chunk id whichever path we
        // took. A loaded prefix's slots stay `None` and its real chunks are put back in
        // front at the end; nothing reads them while compiling.
        chunks: (0..prefix_chunks)
            .map(|_| None)
            .chain((0..=if two_phase { a_lambdas } else { b_lambdas }).map(|_| None))
            .collect(),
        states: Vec::new(),
        node: ast.id(),
        integer_binops: &unit.integer_binops,
        dispatch_modules: &unit.dispatch_modules,
        binop_modules: &unit.binop_modules,
        dec_literals: &unit.dec_literals,
        f32_literals: &unit.f32_literals,
        u128_literals: &unit.u128_literals,
        conversions: &unit.conversions,
        numeral_texts: &unit.numeral_texts,
        coerce_values: unit.coerce_values.clone(),
        coerce_params: &unit.coerce_params,
        zero_sized_capacity: unit.zero_sized_capacity.clone(),
        match_types: &unit.match_types,
        default_sites: &unit.default_sites,
        nominal_defaults: &unit.nominal_defaults,
        nominal_records: unit
            .nominals
            .iter()
            .filter_map(|(name, backing)| {
                // A SLICE of the unit's own type, not a copy of its fields.
                let fields: &[(&'static str, crate::types::Type)] = match backing {
                    crate::types::Type::Record { fields, .. } => Some(fields.as_slice()),
                    crate::types::Type::Nominal { backing, .. } => match &**backing {
                        crate::types::Type::Record { fields, .. } => Some(fields.as_slice()),
                        _ => None,
                    },
                    _ => None,
                }?;
                Some((*name, fields))
            })
            .collect(),
        pending_coerce: None,
        literal_coercions: Vec::new(),
        missing_fields: &unit.missing_fields,
        fractional_literals: &unit.fractional_literals,
        parse_targets: &unit.parse_targets,
        collect_targets: &unit.collect_targets,
        capture_free_methods: Vec::new(),
        global_owner: None,
        intrinsics: &unit.intrinsics,
        enclosing_owners: &unit.enclosing_owners,
    };

    for (name, value) in bindings.iter().take(phase_a) {
        if let Expr::Lambda { params, body, id } = value {
            let (chunk, _) = c.tops.func(name).expect("collected above");
            c.pending_coerce = c.coerce_params.get(id).cloned();
            let captures = c.function(chunk, name, params, body, None)?;
            debug_assert!(captures.is_empty(), "a top-level function captured something");
        }
    }
    if two_phase {
        // The group's own top level, holding only ITS globals and statements.
        let group = c.top_level(unit, &bindings[..phase_a], &statements[..split_statements], None)?;
        c.chunks[0] = Some(group.finish(0, 0));
        // Everything after the group's chunks — its functions AND their nested lambdas
        // — belongs to this program. Its top level takes the next id, then its
        // functions, and its own nested lambdas follow.
        top_slot = c.chunks.len() as ChunkId;
        c.chunks.push(None);
        let mut next = top_slot + 1;
        for (name, value) in bindings.iter().skip(phase_a) {
            if let Expr::Lambda { params, .. } = value {
                c.tops.fns.push((name, next, params.len() as u16));
                c.chunks.push(None);
                next += 1;
            }
        }
        c.tops.index();
    }
    for (name, value) in bindings.iter().skip(phase_a) {
        if let Expr::Lambda { params, body, id } = value {
            let (chunk, _) = c.tops.func(name).expect("collected above");
            c.pending_coerce = c.coerce_params.get(id).cloned();
            // A top-level function is at the outermost level, so it has nothing to
            // capture: every free name in it is a global or another top-level function.
            let captures = c.function(chunk, name, params, body, None)?;
            debug_assert!(captures.is_empty(), "a top-level function captured something");
        }
    }

    let own = c.top_level(unit, &bindings[phase_a..], &statements[split_statements..], Some(tail))?;
    c.chunks[top_slot as usize] = Some(own.finish(top_slot, 0));

    let entry = match entry {
        None => None,
        Some(name) => Some(c.tops.func(name).ok_or_else(|| {
            format!("vm: the entry point `{}` is not a top-level function", name)
        })?),
    };

    // The dispatch tables. Only the compiler knows which top-level functions are
    // methods — they are the ones whose name is `Type.method` — and only the running
    // value can resolve a dispatch the checker could not type.
    let mut methods = std::collections::HashMap::new();
    let mut methods_by_name: std::collections::HashMap<&'static str, Vec<(&'static str, ChunkId)>> =
        std::collections::HashMap::new();
    let block_local = c.capture_free_methods.clone();
    for (qualified, chunk) in c.tops.fns.iter().skip(prefix_fns).map(|(q, c, _)| (*q, *c)).chain(block_local) {
        if let Some((module, method)) = qualified.rsplit_once('.') {
            let module: &'static str = Box::leak(module.to_string().into_boxed_str());
            let method: &'static str = Box::leak(method.to_string().into_boxed_str());
            methods.entry((module, method)).or_insert(chunk);
            methods_by_name.entry(method).or_default().push((qualified, chunk));
        }
    }

    // The prefix's own dispatch tables, which were built when IT was compiled — that
    // is where a builtin's block-local methods live, and nothing here recompiles them.
    let mut literal_coercions = Vec::new();
    let prefix_chunks_owned = match prefix_owned {
        None => Vec::new(),
        Some(p) => {
            for ((module, method), chunk) in p.methods {
                methods.entry((module, method)).or_insert(chunk);
            }
            for (method, defined) in p.methods_by_name {
                methods_by_name.entry(method).or_default().extend(defined);
            }
            literal_coercions = p.literal_coercions;
            p.chunks
        }
    };
    literal_coercions.append(&mut c.literal_coercions);
    let chunks: Vec<Chunk> = prefix_chunks_owned
        .into_iter()
        .chain(
            // The first `prefix_chunks` slots are placeholders that kept an index equal
            // to a chunk id while compiling; the real chunks go in front of them.
            c.chunks
                .into_iter()
                .skip(prefix_chunks)
                .map(|slot| slot.expect("every reserved chunk id was compiled")),
        )
        .collect();

    let group = two_phase.then(|| Group {
        chunks: top_slot as usize,
        fns: c.tops.fns[..a_lambdas].to_vec(),
        globals: c.tops.globals[..group_globals].to_vec(),
    });
    Ok((Program {
        chunks,
        literal_coercions,
        n_globals: c.tops.globals.len(),
        top: top_slot,
        // The builtins' top level, which binds their globals. It runs first.
        prelude,
        entry,
        methods,
        methods_by_name,
        nominal_shapes: unit
            .nominals
            .iter()
            .map(|(name, backing)| (*name, resolved_shape(&unit.nominals, backing)))
            .collect(),
        nominal_depth: unit
            .nominals
            .iter()
            .map(|(name, backing)| (*name, resolved_shape_depth(&unit.nominals, backing).1))
            .collect(),
        opaque_shapes: unit
            .nominals
            .iter()
            .filter(|(name, _)| unit.opaque_nominals.contains(name))
            .map(|(_, backing)| resolved_shape(&unit.nominals, backing))
            .collect(),
    }, group))
}

/// One function being compiled. `Compiler::states` is a stack of these, so a nested
/// lambda can see the frames it is written inside.
struct FnState {
    code: Vec<Op>,
    spans: Vec<crate::ast::NodeId>,
    consts: Vec<Value>,
    /// Name → register, innermost last. A linear scan, but at COMPILE time and over a
    /// single function's names — the run-time scan this replaces walked every scope of
    /// every enclosing call, comparing strings, on every identifier.
    locals: Vec<Local>,
    /// One entry per enclosing loop, holding the `break` jumps still to be pointed at
    /// that loop's exit. Per FUNCTION, so a `break` inside a lambda inside a loop is
    /// refused rather than jumping out of a frame that is no longer running.
    loops: Vec<Vec<u32>>,
    /// Where each capture comes from, in declaration order. The index is what
    /// `LoadCap` uses at run time.
    captures: Vec<CapSource>,
    capture_names: Vec<&'static str>,
    /// Which captures are cells (a captured `var`), read through `CellGet`.
    capture_boxed: Vec<bool>,
    /// Field and tag names, by index. A record literal's names are appended as a
    /// consecutive run, which is what `MakeRecord` reads.
    names: Vec<&'static str>,
    /// Literal patterns, by index — see `Op::TestLit`.
    pats: Vec<Pattern>,
    /// The next free register. Temporaries are freed by restoring this, so a register
    /// is reused by the next expression instead of the frame growing with the AST.
    next_reg: Reg,
    /// The high-water mark, which is the frame size.
    max_reg: Reg,
    /// The name this function was bound to, if it was bound by a `let`. Resolving it
    /// inside the body yields the running closure, which is how a block-local function
    /// calls itself without capturing a binding that does not exist yet.
    self_name: Option<&'static str>,
    params: Rc<[&'static str]>,
    name: &'static str,
    /// The top level is not a function, so `return` there is an error rather than a
    /// silent disagreement with the tree-walker about what it means.
    in_function: bool,
}

impl FnState {
    fn new(
        name: &'static str,
        params: Rc<[&'static str]>,
        self_name: Option<&'static str>,
        in_function: bool,
    ) -> Self {
        FnState {
            code: Vec::new(),
            spans: Vec::new(),
            consts: Vec::new(),
            locals: Vec::new(),
            loops: Vec::new(),
            captures: Vec::new(),
            capture_names: Vec::new(),
            capture_boxed: Vec::new(),
            names: Vec::new(),
            pats: Vec::new(),
            next_reg: 0,
            max_reg: 0,
            self_name,
            params,
            name,
            in_function,
        }
    }

    fn finish(mut self, chunk: ChunkId, arity: u16) -> Chunk {
        // One span per instruction, or an error's location is somebody else's. A
        // `code.push` that skipped `emit` is exactly how that goes wrong, and it did.
        debug_assert_eq!(
            self.code.len(),
            self.spans.len(),
            "chunk `{}` has {} instructions and {} spans",
            self.name,
            self.code.len(),
            self.spans.len()
        );
        // Pairs the machine can run as one instruction, then the last read of a
        // register taking its value instead of cloning it. Both want finished code —
        // they reason about the whole chunk's control flow — and `fuse` runs first so
        // that `liveness` sees the opcodes that will actually run.
        super::peephole::fuse(&mut self.code);
        super::liveness::mark_takes(&mut self.code, self.max_reg);
        Chunk {
            code: self.code,
            spans: self.spans,
            consts: self.consts,
            n_regs: self.max_reg,
            arity,
            names: self.names,
            pats: self.pats,
            bare: Rc::new(super::Closure {
                chunk,
                params: self.params.clone(),
                captures: Vec::new(),
            }),
            params: self.params,
            name: self.name,
        }
    }

    fn local(&self, name: &str) -> Option<&Local> {
        self.locals.iter().rev().find(|l| l.name == name)
    }

    fn local_mut(&mut self, name: &str) -> Option<&mut Local> {
        self.locals.iter_mut().rev().find(|l| l.name == name)
    }
}

/// A name bound in a function's own frame.
struct Local {
    name: &'static str,
    reg: Reg,
    /// Declared with `var`, so it may be assigned.
    is_var: bool,
    /// A closure has copied this value. Assigning it afterwards would leave that copy
    /// stale, so the assignment is refused — see `Compiler::upvalue`.
    captured: bool,
    /// A `var` some lambda in its scope mentions: the register holds a `Value::Cell`
    /// and every read and write goes through it, so the closures share the variable.
    boxed: bool,
}

struct Compiler<'u> {
    tops: Tops,
    chunks: Vec<Option<Chunk>>,
    states: Vec<FnState>,
    /// The node being compiled, stamped onto every instruction it emits.
    node: crate::ast::NodeId,
    /// Which `BinOp` nodes may use the integer-only opcode.
    integer_binops: &'u std::collections::HashSet<crate::ast::NodeId>,
    /// See `Unit::f32_literals`.
    f32_literals: &'u std::collections::HashSet<crate::ast::NodeId>,
    u128_literals: &'u std::collections::HashSet<crate::ast::NodeId>,
    /// See `Unit::conversions` and the fields after it.
    conversions: &'u std::collections::HashMap<crate::ast::NodeId, (&'static str, &'static str)>,
    numeral_texts: &'u std::collections::HashMap<crate::ast::NodeId, String>,
    coerce_values: std::collections::HashMap<crate::ast::NodeId, &'static str>,
    coerce_params: &'u std::collections::HashMap<crate::ast::NodeId, Vec<(usize, &'static str)>>,
    zero_sized_capacity: std::collections::HashSet<crate::ast::NodeId>,
    match_types: &'u std::collections::HashMap<crate::ast::NodeId, crate::types::Type>,
    default_sites: &'u std::collections::HashMap<crate::ast::NodeId, &'static str>,
    nominal_defaults: &'u Vec<(String, Vec<(String, crate::ast::Expr)>)>,
    /// Each default-site nominal's backing fields, so the compiler knows which omitted
    /// fields are optional (fill `<missing>`) versus defaulted (fill the default).
    nominal_records:
        std::collections::HashMap<&'static str, &'u [(&'static str, crate::types::Type)]>,
    /// The parameter conversions of the lambda about to be compiled; `function` takes
    /// them.
    pending_coerce: Option<Vec<(usize, &'static str)>>,
    /// See `Program::literal_coercions`.
    literal_coercions: Vec<(&'static str, Value)>,
    /// See `Unit::missing_fields`.
    missing_fields: &'u std::collections::HashMap<crate::ast::NodeId, Vec<&'static str>>,
    /// See `Unit::dispatch_modules`.
    dispatch_modules: &'u std::collections::HashMap<crate::ast::NodeId, &'static str>,
    /// See `Unit::binop_modules`.
    binop_modules: &'u std::collections::HashMap<crate::ast::NodeId, &'static str>,
    /// See `Unit::dec_literals`.
    dec_literals: &'u std::collections::HashSet<crate::ast::NodeId>,
    /// See `Unit::fractional_literals`.
    fractional_literals: &'u std::collections::HashSet<crate::ast::NodeId>,
    /// See `Unit::parse_targets`.
    parse_targets: &'u std::collections::HashMap<crate::ast::NodeId, crate::types::Type>,
    /// See `Unit::collect_targets`.
    collect_targets: &'u std::collections::HashMap<crate::ast::NodeId, String>,
    /// Block-local nominal methods that captured nothing, added to the runtime
    /// dispatch tables. See `closure`.
    capture_free_methods: Vec<(&'static str, ChunkId)>,
    /// The nominal owning the VALUE being compiled, when it is a method-block member
    /// that is not a function — a top-level `effects = { send: send }`, or a
    /// block-local one. See `enclosing_type`.
    global_owner: Option<&'static str>,
    /// Bare low-level names the builtin module declares; see `Unit::intrinsics`.
    intrinsics: &'u std::collections::HashSet<&'static str>,
    /// See `Unit::enclosing_owners`.
    enclosing_owners: &'u std::collections::HashMap<&'static str, &'static str>,
}

/// Does this instruction move control, or leave the block?
///
/// Used by `Compiler::wrote_directly` to tell a straight-line run of instructions from one
/// with more than one path through it. Listed explicitly rather than by looking for a `to`
/// field, so that a new jumping opcode has to be classified here on purpose.
fn branches(op: &Op) -> bool {
    matches!(
        op,
        Op::Jump { .. }
            | Op::JumpFalse { .. }
            | Op::TestLit { .. }
            | Op::TestLitDyn { .. }
            | Op::TestStr { .. }
            | Op::TestTag { .. }
            | Op::TestTuple { .. }
            | Op::TestRecord { .. }
            | Op::TestList { .. }
            | Op::TestBool { .. }
            | Op::GetFieldOr { .. }
            | Op::IterNext { .. }
            | Op::NoMatch { .. }
            | Op::Ret { .. }
            | Op::TailCall { .. }
            | Op::Crash { .. }
    )
}

/// Which one-callback list method a compiled loop is.
///
/// They share a skeleton — take the next element, run the callback on it, do something
/// with the answer — and differ only in that last step, which is `finish_element`.
#[derive(Debug, Clone, Copy)]
enum Shape {
    /// `xs.fold(init, f)`: the answer is the accumulator.
    Fold,
    /// `xs.map(f)`: the answer is a list of what the callback returned.
    Map,
    /// `xs.any(p)` (true) or `xs.all(p)` (false): the first element the predicate
    /// answers `want` for settles it.
    Decide(bool),
    /// `xs.count_if(p)`: how many elements the predicate agreed with.
    Count,
    /// `xs.find_first(p)`: `Ok(item)` for the first agreement, else `Err(NotFound)`.
    Find,
    /// `xs.find_first_index(p)` (true) or `xs.find_last_index(p)` (false): the POSITION
    /// rather than the item, as `Ok(i)`, else `Err(NotFound)`.
    FindIndex(bool),
    /// `xs.fold_with_index(init, f)`: a fold whose callback also gets the position.
    FoldIndex,
    /// `xs.fold_try(init, f)`: a fold that stops at the first `Err` and hands it back,
    /// and wraps the accumulator in `Ok` if it reaches the end.
    FoldTry,
}

impl Shape {
    /// Does the method take an accumulator before its callback?
    fn takes_init(self) -> bool {
        matches!(self, Shape::Fold | Shape::FoldIndex | Shape::FoldTry)
    }

    /// How many parameters the callback has.
    fn arity(self) -> usize {
        match self {
            Shape::Fold | Shape::FoldTry => 2,
            Shape::FoldIndex => 3,
            _ => 1,
        }
    }

    /// Does the LOOP need to track the current element's position?
    ///
    /// Not the same question as whether the callback is handed it: `find_first_index`
    /// reports a position but its predicate takes only the element, which is what
    /// `arity` says. Conflating the two passed the index as a second argument and made
    /// `xs.find_first_index(big)` fail with "Lambda expects 1 argument(s), got 2".
    fn needs_index(self) -> bool {
        matches!(self, Shape::FoldIndex | Shape::FindIndex(_))
    }

    /// Is the position one of the callback's arguments?
    fn passes_index(self) -> bool {
        matches!(self, Shape::FoldIndex)
    }

    /// Does it build a tag after the loop, needing a register for the payload?
    fn needs_slot(self) -> bool {
        matches!(self, Shape::Find | Shape::FoldTry)
    }
}

/// The registers a shape needs beyond the loop's own `dst`, `iter`, `idx` and `item`.
#[derive(Default, Clone, Copy)]
struct Spares {
    /// The constant `1`, for the shapes that count.
    one: Reg,
    /// The position the NEXT element will have.
    pos: Reg,
    /// The position of the element in hand.
    cur: Reg,
    /// Somewhere to put a tag's payload before building it.
    slot: Reg,
}

impl<'u> Compiler<'u> {
    /// One top-level chunk: bind each global in order, then the statements, then the
    /// trailing expression if this is the program's own rather than a group's.
    ///
    /// Two callers, which is why it is a method: the precompiled group gets one of
    /// these and so does the program that loads it. Order matters within each — a
    /// global that reads one declared below it gets "Used before it was defined".
    fn top_level(
        &mut self,
        unit: &Unit,
        bindings: &[(&'static str, &Expr)],
        statements: &[&Expr],
        tail: Option<&Expr>,
    ) -> Result<FnState, String> {
        self.states.push(FnState::new("top level", Rc::from([]), None, false));
        if tail.is_some() {
            for (name, text) in &unit.ingested {
                let reg = self.alloc()?;
                self.constant(reg, crate::eval::str_value(text.clone()))?;
                let idx = self.tops.global(name).expect("reserved above");
                self.emit(Op::StoreGlob { idx, src: reg });
                self.st().next_reg = reg;
            }
        }
        for (name, value) in bindings {
            if matches!(value, Expr::Lambda { .. }) {
                continue;
            }
            let save = self.st().next_reg;
            self.global_owner = name.rsplit_once('.').map(|(owner, _)| owner);
            let src = self.expr(value)?;
            self.global_owner = None;
            let idx = self.tops.global(name).expect("collected above");
            self.emit(Op::StoreGlob { idx, src });
            self.st().next_reg = save;
        }
        // Statements the flattening lifted out of a `_` binding, run for their effects.
        //
        // ponytail: after the globals rather than interleaved with them in source order.
        // The only thing this shape holds today is top-level `expect`s, which run after
        // the declarations anyway; interleaving matters once those are compiled (V5).
        for statement in statements {
            let save = self.st().next_reg;
            self.top_statement(statement, unit.test_mode || unit.run_expects)?;
            self.st().next_reg = save;
        }
        match tail {
            // A group's top level answers nothing: it exists to bind its globals.
            None => {
                let src = self.literal(Value::Unit)?;
                self.emit(Op::Ret { src });
            }
            // A file whose last declaration is a top-level `expect` has it as the
            // trailing expression rather than a statement. It is still a test, so it
            // gets the same treatment and the top level's own value is `{}` either way.
            Some(tail) if matches!(tail, Expr::Expect(..)) => {
                self.top_statement(tail, unit.test_mode || unit.run_expects)?;
                let src = self.literal(Value::Unit)?;
                self.emit(Op::Ret { src });
            }
            Some(tail) => self.tail(tail)?,
        }
        Ok(self.states.pop().expect("pushed above"))
    }

    /// The function being compiled.
    fn st(&mut self) -> &mut FnState {
        self.states.last_mut().expect("a function is always being compiled")
    }

    fn emit(&mut self, op: Op) {
        let node = self.node;
        let st = self.st();
        st.code.push(op);
        // Parallel to `code`: whichever node the compiler is working on owns the
        // instructions it emits, which is how a runtime error finds its line.
        st.spans.push(node);
    }

    /// `x + 1`: fold the literal straight into the operator, if that is what this is.
    ///
    /// The right operand is a literal exactly when the LAST instruction emitted is a
    /// `LoadK` into `b`, and `b` is a temporary this expression allocated — `b >= save`,
    /// the `next_reg` watermark from before the operands compiled. That second test is
    /// not decoration: without it `y = 5` followed by `x + y` would match, because `y`'s
    /// own `LoadK` is the previous instruction and `y`'s register is the operand. Fusing
    /// there would delete the binding and leave every later read of `y` empty.
    ///
    /// The `LoadK` is REPLACED rather than removed, so the instruction count is what it
    /// was when any jump target was recorded — a `while` loop's head is the first
    /// instruction of its condition, which for `while i < 10` is exactly this `LoadK`.
    /// Its span becomes the operator's, so an overflow still reports at the operator.
    ///
    /// `fused` arrives with `k: 0`; the real constant index is filled in here.
    fn fuse_literal_operand(&mut self, a: Reg, b: Reg, save: Reg, fused: Op) -> bool {
        if b == a || b < save {
            return false;
        }
        let node = self.node;
        let st = self.st();
        let Some(&Op::LoadK { dst, k }) = st.code.last() else { return false };
        if dst != b {
            return false;
        }
        let with_k = match fused {
            Op::BinK { dst, a, op, .. } => Op::BinK { dst, a, k, op },
            Op::BinIntK { dst, a, op, width, .. } => Op::BinIntK { dst, a, k, op, width },
            _ => return false,
        };
        let last = st.code.len() - 1;
        st.code[last] = with_k;
        st.spans[last] = node;
        true
    }

    fn alloc(&mut self) -> Result<Reg, String> {
        let st = self.st();
        let reg = st.next_reg;
        st.next_reg = reg
            .checked_add(1)
            .ok_or_else(|| "vm: a function needs more than 65535 registers".to_string())?;
        st.max_reg = st.max_reg.max(st.next_reg);
        Ok(reg)
    }

    /// Reserve registers up to and including `reg`, so later temporaries land above it.
    fn reserve(&mut self, reg: Reg) -> Result<(), String> {
        while self.st().next_reg <= reg {
            self.alloc()?;
        }
        Ok(())
    }

    fn constant(&mut self, dst: Reg, value: Value) -> Result<(), String> {
        let st = self.st();
        let k = u32::try_from(st.consts.len())
            .map_err(|_| "vm: too many constants".to_string())?;
        st.consts.push(value);
        // Through `emit`, so this instruction gets a span like every other. Pushing
        // straight onto `code` left the span table one short, and every error after
        // the first constant in a chunk lost its location.
        self.emit(Op::LoadK { dst, k });
        Ok(())
    }

    /// The index of the next instruction, for patching a jump once its target is known.
    fn here(&mut self) -> u32 {
        self.st().code.len() as u32
    }

    fn patch_to_here(&mut self, at: u32) {
        let target = self.here();
        match &mut self.st().code[at as usize] {
            Op::Jump { to }
            | Op::JumpFalse { to, .. }
            | Op::TestLit { to, .. }
            | Op::TestLitDyn { to, .. }
            | Op::TestStr { to, .. }
            | Op::TestTag { to, .. }
            | Op::TestTuple { to, .. }
            | Op::TestRecord { to, .. }
            | Op::TestList { to, .. }
            | Op::TestBool { to, .. }
            | Op::GetFieldOr { to, .. }
            | Op::IterNext { to, .. } => *to = target,
            other => unreachable!("patched a {:?}, which is not a jump", other),
        }
    }

    /// The index of `name` in this chunk's name table, adding it if it is new.
    fn name_idx(&mut self, name: &'static str) -> Result<u16, String> {
        let st = self.st();
        if let Some(i) = st.names.iter().position(|n| *n == name) {
            return Ok(i as u16);
        }
        let idx = u16::try_from(st.names.len())
            .map_err(|_| "vm: more than 65535 names in one function".to_string())?;
        st.names.push(name);
        Ok(idx)
    }

    /// Append names as a CONSECUTIVE run and answer where it starts. A record's fields
    /// are read as one span, so they cannot be deduplicated individually.
    fn names_run(&mut self, names: &[&'static str]) -> Result<u16, String> {
        let st = self.st();
        let start = u16::try_from(st.names.len())
            .map_err(|_| "vm: more than 65535 names in one function".to_string())?;
        st.names.extend_from_slice(names);
        u16::try_from(st.names.len())
            .map_err(|_| "vm: more than 65535 names in one function".to_string())?;
        Ok(start)
    }

    fn pat_idx(&mut self, pattern: Pattern) -> Result<u16, String> {
        let st = self.st();
        let idx = u16::try_from(st.pats.len())
            .map_err(|_| "vm: more than 65535 literal patterns in one function".to_string())?;
        st.pats.push(pattern);
        Ok(idx)
    }

    /// Resolve a name innermost-first, as the tree-walker's `lookup` did.
    /// The nominal whose method block is being compiled, from the function's own name.
    ///
    /// Inside `Graph :: … .{ … }` a sibling method is in scope UNQUALIFIED — roc lets
    /// `from_list` call `from_dict(…)` — but it is bound here as `Graph.from_dict`.
    /// The one binding in scope named `Type.method`, if exactly one is. A block-local
    /// nominal's methods are ordinary locals (they capture), so this is how a dispatch
    /// the checker could not resolve statically still finds one.
    fn unique_scoped_method(&self, method: &str) -> Option<&'static str> {
        let suffix = format!(".{}", method);
        let mut found: Option<&'static str> = None;
        for state in &self.states {
            for local in &state.locals {
                if !local.name.ends_with(&suffix) {
                    continue;
                }
                match found {
                    Some(seen) if seen == local.name => {}
                    Some(_) => return None,
                    None => found = Some(local.name),
                }
            }
        }
        found
    }

    /// The block the code being compiled belongs to, then each block around it: the
    /// owners a bare sibling name is tried under, innermost first.
    fn enclosing_owners_of(&self) -> Vec<&'static str> {
        let mut owners = Vec::new();
        let mut owner = self.enclosing_type();
        while let Some(o) = owner {
            if owners.contains(&o) {
                break;
            }
            owners.push(o);
            owner = self.enclosing_owners.get(o).copied();
        }
        owners
    }

    fn enclosing_type(&self) -> Option<&'static str> {
        self.states
            .iter()
            .rev()
            .find_map(|state| state.name.rsplit_once('.').map(|(owner, _)| owner))
            // A method-block member that is a VALUE rather than a function —
            // `effects = { send: send }` — is compiled as a global at the top level,
            // whose frame is called "top level". Without the owner its siblings are
            // undefined names.
            .or(self.global_owner)
    }

    fn resolve(&mut self, name: &'static str) -> Option<Found> {
        let level = self.states.len() - 1;
        if let Some(l) = self.states[level].local(name) {
            return Some(if l.boxed { Found::LocalCell(l.reg) } else { Found::Local(l.reg) });
        }
        if self.states[level].self_name == Some(name) {
            return Some(Found::SelfRef);
        }
        match self.upvalue(level, name) {
            Err(e) => return Some(Found::Refused(e)),
            Ok(Some(idx)) if self.states[level].capture_boxed[idx as usize] => {
                return Some(Found::CaptureCell(idx))
            }
            Ok(Some(idx)) => return Some(Found::Capture(idx)),
            Ok(None) => {}
        }
        if let Some(idx) = self.tops.global(name) {
            return Some(Found::Global(idx));
        }
        if let Some((c, a)) = self.tops.func(name) {
            return Some(Found::Func(c, a));
        }
        // Last: a name another module exposed. Checked after everything else so a
        // local binding of the same name wins, as it did in the tree-walker.
        let full = self.tops.alias(name)?;
        match self.resolve(full) {
            None => Some(Found::Refused(format!(
                "module does not expose `{}`",
                name
            ))),
            found => found,
        }
    }

    /// Find `name` in a function enclosing `level`, threading a capture down each
    /// level in between, and answer its capture index in `level`.
    ///
    /// This is the whole of closure conversion: after it, a captured variable is an
    /// index into a `Vec`, and nothing at run time knows it ever had a name.
    fn upvalue(&mut self, level: usize, name: &'static str) -> Result<Option<u16>, String> {
        if let Some(i) = self.states[level].capture_names.iter().position(|n| *n == name) {
            return Ok(Some(i as u16));
        }
        // Level 0 is a top-level function or the top level itself: there is no
        // enclosing frame, so an unresolved name there is a global or an error.
        let Some(parent) = level.checked_sub(1) else { return Ok(None) };

        if let Some(l) = self.states[parent].local_mut(name) {
            if l.is_var && !l.boxed {
                // A `var` a closure captures is compiled through a shared cell
                // (`MakeCell` / `CellGet`) so both sides see the assignments — but only
                // when the binding was boxed as it was created. This arm is the one that
                // was not, and capturing it by value would silently go stale, so it is
                // refused instead. ponytail: boxing retroactively means re-emitting the
                // binding; do it only if a real program hits this.
                return Err(format!(
                    "vm: `{}` is a `var` captured by a closure, which needs a shared cell (V3 refuses it rather than copying it and going stale)",
                    name
                ));
            }
            l.captured = true;
            let (reg, boxed) = (l.reg, l.boxed);
            return Ok(self.add_capture(level, name, CapSource::Local(reg), boxed));
        }
        if self.states[parent].self_name == Some(name) {
            return Ok(self.add_capture(level, name, CapSource::Enclosing, false));
        }
        match self.upvalue(parent, name)? {
            None => Ok(None),
            Some(in_parent) => {
                let boxed = self.states[parent].capture_boxed[in_parent as usize];
                Ok(self.add_capture(level, name, CapSource::Capture(in_parent), boxed))
            }
        }
    }

    fn add_capture(&mut self, level: usize, name: &'static str, src: CapSource, boxed: bool) -> Option<u16> {
        let st = &mut self.states[level];
        let idx = u16::try_from(st.captures.len()).ok()?;
        st.captures.push(src);
        st.capture_names.push(name);
        st.capture_boxed.push(boxed);
        Some(idx)
    }

    /// Compile a lambda into `chunk`, and answer what it captured.
    fn function(
        &mut self,
        chunk: ChunkId,
        name: &'static str,
        params: &Rc<[&'static str]>,
        body: &Expr,
        self_name: Option<&'static str>,
    ) -> Result<Vec<CapSource>, String> {
        self.states.push(FnState::new(name, params.clone(), self_name, true));
        for param in params.iter() {
            let reg = self.alloc()?;
            self.st().locals.push(Local { name: param, reg, is_var: false, captured: false, boxed: false });
        }
        // A parameter declared as a nominal with a literal conversion: a raw literal
        // from a generic caller is converted on entry, anything else passes through.
        if let Some(coerce) = self.pending_coerce.take() {
            for (i, nominal) in coerce {
                let reg = i as Reg;
                let save = self.st().next_reg;
                let got = self.coerce(reg, nominal)?;
                self.emit(Op::Move { dst: reg, src: got });
                self.st().next_reg = save;
            }
        }
        // The body is in tail position by definition, which is what turns a
        // tail-recursive function into a loop.
        let result = self.tail(body);
        let st = self.states.pop().expect("pushed above");
        result?;
        let captures = st.captures.clone();
        let arity = u16::try_from(params.len()).map_err(|_| "vm: too many parameters".to_string())?;
        self.chunks[chunk as usize] = Some(st.finish(chunk, arity));
        Ok(captures)
    }

    /// Compile a lambda in an expression position: a new chunk, plus the code that
    /// gathers its captures and builds the closure.
    fn closure(
        &mut self,
        name: &'static str,
        params: &Rc<[&'static str]>,
        body: &Expr,
        self_name: Option<&'static str>,
    ) -> Result<Reg, String> {
        self.chunks.push(None);
        let chunk = (self.chunks.len() - 1) as ChunkId;
        let captures = self.function(chunk, name, params, body, self_name)?;
        // A block-local nominal's method that captures NOTHING can also answer a
        // runtime dispatch, which is how an imported generic helper — compiled long
        // before this block — reaches it. One that does capture cannot: its chunk
        // needs the closure's values, and only the binding in scope has them.
        if captures.is_empty() && name.contains('.') {
            self.capture_free_methods.push((name, chunk));
        }

        // The captured values go in consecutive registers, which is where
        // `MakeClosure` reads them from.
        let cap_base = self.st().next_reg;
        for (i, src) in captures.iter().enumerate() {
            let target = cap_base + i as Reg;
            self.reserve(target)?;
            match *src {
                CapSource::Local(reg) => self.emit(Op::Move { dst: target, src: reg }),
                CapSource::Capture(idx) => self.emit(Op::LoadCap { dst: target, idx }),
                CapSource::Enclosing => self.emit(Op::LoadSelf { dst: target }),
            }
        }
        let n = u16::try_from(captures.len()).map_err(|_| "vm: too many captures".to_string())?;
        self.st().next_reg = cap_base;
        let dst = self.alloc()?;
        self.emit(Op::MakeClosure { dst, chunk, base: cap_base, n });
        Ok(dst)
    }

    /// Put a call's arguments in consecutive registers and answer where they start.
    fn arguments(&mut self, args: &[Expr]) -> Result<(Reg, u16), String> {
        let refs: Vec<&Expr> = args.iter().collect();
        self.values(&refs)
    }

    /// Evaluate expressions into consecutive registers: a call's arguments, a list's
    /// elements, a record's field values. Answers where the run starts and how long it
    /// is, which is the shape every aggregate and call opcode expects.
    fn values(&mut self, args: &[&Expr]) -> Result<(Reg, u16), String> {
        let arg_base = self.st().next_reg;
        for (i, arg) in args.iter().enumerate() {
            let target = arg_base + i as Reg;
            self.reserve(target)?;
            let save = self.st().next_reg;
            let before = self.here();
            let reg = self.expr(arg)?;
            if reg != target && !self.wrote_directly(before, reg, target, save) {
                self.emit(Op::Move { dst: target, src: reg });
            }
            // Free the argument's own temporaries but keep the argument itself.
            self.st().next_reg = save;
        }
        let argc = u16::try_from(args.len()).map_err(|_| "vm: too many arguments".to_string())?;
        Ok((arg_base, argc))
    }

    /// A call whose callee is a top-level function, if this one is.
    fn direct_callee(&mut self, func: &Expr) -> Option<(ChunkId, u16)> {
        let name = match func {
            Expr::Ident(name, _) => *name,
            _ => return None,
        };
        // A local, a capture or a self-reference shadows a top-level name, so those
        // have to be checked first — the callee is then a value, not a chunk.
        match self.resolve(name) {
            Some(Found::Func(chunk, arity)) => Some((chunk, arity)),
            _ => None,
        }
    }

    /// The `Numeral` a literal node stands for: from its text where the parser kept
    /// it, otherwise from the value.
    fn numeral(&self, id: &crate::ast::NodeId, value: Value) -> Result<Value, String> {
        self.numeral_texts
            .get(id)
            .and_then(|text| crate::eval::numeral::numeral_from_text(text))
            .or_else(|| crate::eval::numeral::numeral_from_value(&value))
            .ok_or_else(|| format!("vm: {} cannot be read as a numeral", value))
    }

    /// `"Roc"` where a `Tag` is expected: `Tag.from_quote("Roc")`, unwrapped; `42`
    /// where a `Big` is: `Big.from_numeral(numeral)`. roc refuses a literal the
    /// conversion rejects at compile time; here that is a runtime crash, since the
    /// conversion only runs then.
    fn convert_literal(&mut self, module: &str, method: &str, arg: Value) -> Result<Reg, String> {
        let base = self.st().next_reg;
        let got = self.literal(arg)?;
        if got != base {
            self.emit(Op::Move { dst: base, src: got });
        }
        self.call_conversion(module, method, base, 1)
    }

    /// `"a${b}"` where a `Url` is expected: `Url.from_interpolation(first, rest)`,
    /// with `rest` the `(value, following text)` pairs — a list, which is what an
    /// `Iter` is here.
    fn interpolated(&mut self, module: &str, method: &str, parts: &[StrPart]) -> Result<Reg, String> {
        let mut literals: Vec<&'static str> = vec![""];
        let mut exprs: Vec<&Expr> = Vec::new();
        for part in parts {
            match part {
                StrPart::Literal(text) => *literals.last_mut().expect("seeded") = text,
                StrPart::Expr(e) => {
                    exprs.push(e);
                    literals.push("");
                }
            }
        }
        // `first` at `base`, the list of pairs at `base + 1`, each pair built from
        // the two registers after it.
        let base = self.st().next_reg;
        self.place(base, |c| c.literal(Value::Str(Rc::from(literals[0]))))?;
        let list = base + 1;
        self.reserve(list)?;
        let pairs = self.st().next_reg;
        for (i, e) in exprs.iter().enumerate() {
            let target = pairs + i as Reg;
            self.reserve(target)?;
            let save = self.st().next_reg;
            let pair = save;
            self.place(pair, |c| c.expr(e))?;
            self.place(pair + 1, |c| c.literal(Value::Str(Rc::from(literals[i + 1]))))?;
            self.emit(Op::MakeTuple { dst: target, base: pair, n: 2 });
            self.st().next_reg = save;
        }
        let n = u16::try_from(exprs.len()).map_err(|_| "vm: too many interpolations".to_string())?;
        self.emit(Op::MakeList { dst: list, base: pairs, n });
        self.call_conversion(module, method, base, 2)
    }

    /// Compute a value into exactly `target`, freeing whatever temporaries it used.
    fn place(&mut self, target: Reg, value: impl FnOnce(&mut Self) -> Result<Reg, String>) -> Result<(), String> {
        self.reserve(target)?;
        let save = self.st().next_reg;
        let got = value(self)?;
        if got != target {
            self.emit(Op::Move { dst: target, src: got });
        }
        self.st().next_reg = save;
        Ok(())
    }

    /// Call `Module.method` on the `argc` arguments at `base`; a `from_quote` or
    /// `from_numeral` answers a `Try`, and the literal is its `Ok`.
    fn call_conversion(&mut self, module: &str, method: &str, base: Reg, argc: u16) -> Result<Reg, String> {
        let owner = qualify(module, method);
        let (chunk, arity) = self
            .tops
            .func(owner)
            .ok_or_else(|| format!("vm: `{}` has no `{}` for a literal", module, method))?;
        check_arity(owner, arity, argc)?;
        self.st().next_reg = base;
        let dst = self.alloc()?;
        self.emit(Op::CallFn { dst, chunk, base, argc });
        if method == "from_interpolation" {
            return Ok(dst);
        }
        let ok = self.name_idx("Ok")?;
        let fail = self.here();
        self.emit(Op::TestTag { obj: dst, name: ok, n: 1, to: u32::MAX });
        let out = self.alloc()?;
        self.emit(Op::GetPayload { dst: out, obj: dst, i: 0 });
        let done = self.here();
        self.emit(Op::Jump { to: u32::MAX });
        self.patch_to_here(fail);
        self.emit(Op::NoMatch { obj: dst });
        self.patch_to_here(done);
        Ok(out)
    }

    /// The `Ok` payload of a `Try` in `src`, crashing on an `Err` — what a
    /// `from_numeral`/`from_quote` conversion answers with.
    fn unwrap_ok(&mut self, src: Reg) -> Result<Reg, String> {
        let ok = self.name_idx("Ok")?;
        let fail = self.here();
        self.emit(Op::TestTag { obj: src, name: ok, n: 1, to: u32::MAX });
        let out = self.alloc()?;
        self.emit(Op::GetPayload { dst: out, obj: src, i: 0 });
        let done = self.here();
        self.emit(Op::Jump { to: u32::MAX });
        self.patch_to_here(fail);
        self.emit(Op::NoMatch { obj: src });
        self.patch_to_here(done);
        Ok(out)
    }

    /// `Lit.coerce(value, from_quote, from_numeral, from_interpolation)` on `src`: a raw
    /// literal that reached a nominal's slot through a generic body is converted, and
    /// anything else comes back as it was. Each converter the nominal lacks is `{}`.
    fn coerce(&mut self, src: Reg, nominal: &'static str) -> Result<Reg, String> {
        let base = self.st().next_reg;
        self.reserve(base)?;
        self.emit(Op::Move { dst: base, src });
        for (i, method) in ["from_quote", "from_numeral", "from_interpolation"].into_iter().enumerate() {
            let target = base + 1 + i as Reg;
            self.reserve(target)?;
            let save = self.st().next_reg;
            let owner = qualify(nominal, method);
            let got = if self.tops.func(owner).is_some() {
                self.use_name(owner)?
            } else {
                self.literal(Value::Unit)?
            };
            if got != target {
                self.emit(Op::Move { dst: target, src: got });
            }
            self.st().next_reg = save;
        }
        self.st().next_reg = base;
        let dst = self.alloc()?;
        let name = self.names_run(&["Lit", "coerce"])?;
        self.emit(Op::CallBuiltin { dst, name, base, argc: 4 });
        Ok(dst)
    }

    /// Does the program declare a nominal that builds itself from a literal? Then a
    /// literal pattern against a value of unknown type must ask the value.
    fn converts_literals(&self) -> bool {
        !self.tops.methods("from_numeral").is_empty() || !self.tops.methods("from_quote").is_empty()
    }

    /// A statement at the top level, where `expect` means something different.
    ///
    /// A top-level `expect` is a TEST: `roc test` runs it and tallies it, and a normal
    /// `roc run` skips it entirely — the condition is never evaluated, so a call it
    /// makes has no effects either. Every other statement runs both ways, and so does
    /// an `expect` inside a function body, which is a runtime assertion rather than a
    /// test and is never tallied.
    fn top_statement(&mut self, statement: &Expr, test_mode: bool) -> Result<(), String> {
        let Expr::Expect(condition, _) = statement else {
            self.expr(statement)?;
            return Ok(());
        };
        if !test_mode {
            return Ok(());
        }
        let cond = self.expr(condition)?;
        self.emit(Op::TestExpect { cond });
        Ok(())
    }

    /// Compile `e` in TAIL position: the code emitted ends the function, either by
    /// returning or by handing the frame to a tail call.
    fn tail(&mut self, e: &Expr) -> Result<(), String> {
        let enclosing = std::mem::replace(&mut self.node, e.id());
        let result = self.tail_inner(e);
        self.node = enclosing;
        result
    }

    fn tail_inner(&mut self, e: &Expr) -> Result<(), String> {
        match e {
            Expr::If { condition, then_branch, otherwise, .. } => {
                let save = self.st().next_reg;
                let cond = self.expr(condition)?;
                self.st().next_reg = save;
                let jump_to_else = self.here();
                self.emit(Op::JumpFalse { cond, to: u32::MAX, kind: CondKind::If });
                // Each branch ends the function itself, so there is no jump over the
                // else branch and no destination register to agree on.
                self.tail(then_branch)?;
                self.st().next_reg = save;
                self.patch_to_here(jump_to_else);
                self.tail(otherwise)?;
                self.st().next_reg = save;
                Ok(())
            }

            Expr::Let { .. } | Expr::VarDecl { .. } | Expr::Assign { .. } => {
                let (rest, pushed) = self.statements(e)?;
                let result = self.tail(rest);
                self.pop_locals(pushed);
                result
            }

            // Every arm's body is in tail position too, so a function that is one
            // `match` returns straight out of the arm that matched.
            Expr::Match { scrutinee, arms, id } => {
                self.compile_match(scrutinee, arms, true, self.match_types.get(id).cloned())?;
                Ok(())
            }

            // `return f(x)` is still a tail call.
            Expr::Return(inner, _) if self.st().in_function => self.tail(inner),

            Expr::Call { func, args, .. } => {
                if let Some((chunk, arity)) = self.direct_callee(func) {
                    let (arg_base, argc) = self.arguments(args)?;
                    check_arity(func_name(func), arity, argc)?;
                    self.emit(Op::TailCall { func: 0, chunk: Some(chunk), base: arg_base, argc });
                    return Ok(());
                }
                // A builtin is not a tail call: it returns a value, which this
                // function then returns. Missing this arm made every builtin call in
                // tail position compile as a call to a `Value::Builtin` instead —
                // "Attempted to call a non-function value", on 24 golden pairs.
                if let Some(src) = self.builtin_call(func, args)? {
                    self.emit(Op::Ret { src });
                    return Ok(());
                }
                // The callee is a value. It is loaded BEFORE the arguments, so its
                // register is below them and survives the arguments moving down.
                let save = self.st().next_reg;
                let callee = self.expr(func)?;
                let (arg_base, argc) = self.arguments(args)?;
                self.st().next_reg = save;
                self.emit(Op::TailCall { func: callee, chunk: None, base: arg_base, argc });
                Ok(())
            }

            other => {
                let src = self.expr(other)?;
                self.emit(Op::Ret { src });
                Ok(())
            }
        }
    }

    /// Compile the statements at the front of `e` — its `let`s, `var`s and
    /// assignments — and answer the expression they lead to and how many locals they
    /// pushed. A loop, not a recursion per statement: a block's depth is then bounded
    /// by memory rather than by the Rust stack, which 6,000 statements overflowed.
    ///
    /// Each statement's instructions carry its own node, as they would have through
    /// `expr`. The caller compiles the tail and pops the locals afterwards.
    fn statements<'e>(&mut self, e: &'e Expr) -> Result<(&'e Expr, usize), String> {
        let mut cursor = e;
        let mut pushed = 0;
        loop {
            let enclosing = std::mem::replace(&mut self.node, cursor.id());
            let next = match cursor {
                Expr::Let { name, value, body, .. } => {
                    pushed += 1;
                    self.bind(name, value, false).map(|()| &**body)
                }
                // `var x = value` then the rest of the block. Like a `let`, except the
                // binding may be assigned — which, compiled, is a write to its register.
                Expr::VarDecl { name, value, body, .. } => {
                    pushed += 1;
                    let boxed = lambda_mentions(body, name);
                    self.bind_var(name, value, boxed).map(|()| &**body)
                }
                Expr::Assign { name, value, body, .. } => self.assign(name, value).map(|()| &**body),
                other => {
                    self.node = enclosing;
                    return Ok((other, pushed));
                }
            };
            self.node = enclosing;
            cursor = next?;
        }
    }

    /// Compile `e` for its EFFECT: its value is not read, so it need not exist.
    ///
    /// A loop body is the case that matters. `for i in .. { total = total + i }` is a
    /// block whose statements do the work and whose tail is `{}`, and compiling that tail
    /// as an expression emitted a `LoadK` of `Unit` into a register nothing ever read —
    /// one wasted instruction per iteration, which was 20% of the `loop` benchmark's
    /// opcodes. Anything that is not a bare `{}` still has to run: only the
    /// materialization is skipped, never the evaluation.
    fn discard(&mut self, e: &Expr) -> Result<(), String> {
        let (tail, pushed) = match e {
            Expr::Let { .. } | Expr::VarDecl { .. } | Expr::Assign { .. } => self.statements(e)?,
            other => (other, 0),
        };
        let result = match tail {
            Expr::Unit(_) => Ok(()),
            other => self.expr(other).map(|_| ()),
        };
        self.pop_locals(pushed);
        result
    }

    fn pop_locals(&mut self, n: usize) {
        let locals = &mut self.st().locals;
        let keep = locals.len() - n;
        locals.truncate(keep);
    }

    /// `xs.fold(init, f)`, `xs.map(f)`, `xs.keep_if(p)` and friends as a loop in THIS
    /// frame.
    ///
    /// The builtin versions re-enter the VM from Rust once per element — a fresh
    /// machine, an argument `Vec` and a Rust frame each time — which was the whole of
    /// `iter_range`'s 199ms. Compiled, each element is an `IterNext`, a move or two
    /// and an ordinary `Call` on the same frame stack.
    ///
    /// Only for a receiver the checker proved is a list (or an iterator over one), and
    /// only when no roc-defined method of that name is loaded, which the caller checks:
    /// anything else still dispatches at run time. `None` when the method is not one
    /// of these two.
    fn list_loop(
        &mut self,
        method: &str,
        receiver: &Expr,
        args: &[Expr],
        module: &str,
    ) -> Result<Option<Reg>, String> {
        let shape = match (method, args.len()) {
            ("fold", 2) => Shape::Fold,
            ("map", 1) => Shape::Map,
            // `keep` vs `drop`, and `any` vs `all`, differ only in which answer from the
            // predicate is the interesting one.
            //
            ("any", 1) => Shape::Decide(true),
            ("all", 1) => Shape::Decide(false),
            ("count_if", 1) => Shape::Count,
            ("find_first", 1) => Shape::Find,
            ("find_first_index", 1) => Shape::FindIndex(true),
            ("find_last_index", 1) => Shape::FindIndex(false),
            ("fold_with_index", 2) => Shape::FoldIndex,
            ("fold_try", 2) => Shape::FoldTry,
            _ => return Ok(None),
        };
        // Every shape above answers a plain VALUE — a list of results, an accumulator, a
        // `Bool`, a count, a `Try`. That is what makes them safe to compile whatever the
        // checker thinks the receiver's module is.
        //
        // `keep_if`/`drop_if` are NOT here, and were tried: `Builtin.roc` declares both
        // `List.keep_if -> List(a)` and `Iter.keep_if -> Iter(a)`, the second lazy, so a
        // compiled loop may only stand in for the List one. There is no sound way to tell
        // them apart here. `dispatch_modules` is not it — the checker calls
        // `(1..=5).iter()` a `List`, so lowering on that made
        // `Str.inspect((1..=5).iter().keep_if(p))` answer `[4.0, 5.0]` where roc answers
        // `<opaque>`; and nothing syntactic is it either, because `xs = (1..=n).iter()`
        // then `xs.keep_if(p)` has a bare name as its receiver. See `dispatch_builtin`,
        // which answers the lazy one for both and says what is still divergent.
        let _ = module;

        // The accumulator, the list being built, or the answer. Allocated first, so it
        // sits below everything the loop uses and survives the temporaries being freed.
        let dst = self.alloc()?;
        // The answer if the loop runs out, for every shape that has one: `any` of nothing
        // is `False` and `all` of nothing is `True`, nothing matches nothing, and a count
        // of nothing is zero. An element that settles it overwrites this and leaves.
        match shape {
            Shape::Map => self.emit(Op::MakeList { dst, base: dst, n: 0 }),
            Shape::Decide(want) => self.constant(dst, Value::Bool(!want))?,
            Shape::Count => self.constant(dst, Value::Int(0))?,
            Shape::Find | Shape::FindIndex(_) => self.constant(
                dst,
                Value::tag("Err", [Value::bare("NotFound")]),
            )?,
            // A fold's answer starts as its `init`, loaded below.
            Shape::Fold | Shape::FoldIndex | Shape::FoldTry => {}
        }
        let iter = self.expr(receiver)?;
        self.reserve(iter)?;
        let mark = self.st().next_reg;
        let mut rest = args;
        if shape.takes_init() {
            let init = self.expr(&rest[0])?;
            if init != dst {
                self.emit(Op::Move { dst, src: init });
            }
            self.st().next_reg = mark;
            rest = &rest[1..];
        }
        // A LITERAL lambda is compiled into the loop itself — no closure, no call —
        // with its parameters bound to the loop's own registers. Its body sees the
        // same names it would have captured, and they cannot have changed since:
        // a captured `var` is refused outright. Only a body that stays put qualifies:
        // a `return` would leave the enclosing function, a `break` its loop, and an
        // assignment could reach a `var` the lambda would not have been allowed to.
        let inline = match &rest[0] {
            Expr::Lambda { params, body, .. }
                if params.len() == shape.arity() && !escapes(body) =>
            {
                Some((params.clone(), body.clone()))
            }
            _ => None,
        };
        let func = if inline.is_some() {
            0
        } else {
            let func = self.alloc()?;
            let f = self.expr(&rest[0])?;
            if f != func {
                self.emit(Op::Move { dst: func, src: f });
            }
            self.st().next_reg = func + 1;
            func
        };
        let idx = self.alloc()?;
        self.constant(idx, Value::Int(0))?;
        let item = self.alloc()?;
        // Allocated before the loop, so a constant is loaded once rather than per
        // element. A shape that counts needs the `1`; one that reports a POSITION needs a
        // running counter and a copy of the current one; one that builds a tag needs
        // somewhere to put the payload.
        let mut spares = Spares::default();
        if matches!(shape, Shape::Count) || shape.needs_index() {
            spares.one = self.alloc()?;
            self.constant(spares.one, Value::Int(1))?;
        }
        if shape.needs_index() {
            spares.pos = self.alloc()?;
            self.constant(spares.pos, Value::Int(0))?;
            spares.cur = self.alloc()?;
        }
        if shape.needs_slot() {
            spares.slot = self.alloc()?;
        }

        let top = self.here();
        self.emit(Op::IterNext { dst: item, iter, idx, to: u32::MAX });
        // The position of the element just taken, and the counter moved on for the next
        // one. `IterNext`'s own `idx` cannot serve: it is the position only for a list or
        // a range, and a lazy iterator carries its state instead and never touches it.
        if shape.needs_index() {
            self.emit(Op::Move { dst: spares.cur, src: spares.pos });
            self.emit(Op::BinInt {
                dst: spares.pos,
                a: spares.pos,
                b: spares.one,
                op: crate::ast::BinOp::Add,
                width: 0,
            });
        }
        // Where a shape that can stop early goes once an element settles the answer:
        // patched to the loop's exit when that is known.
        let decided: Option<u32>;
        // The callback's arguments go in consecutive registers, where `Call` expects
        // them; its frame then starts there.
        let base = self.st().next_reg;
        // What the callback is handed, in the order roc declares it: the accumulator if
        // there is one, then the element, then its position if the method reports one.
        let mut args_for = Vec::with_capacity(3);
        if shape.takes_init() {
            args_for.push(dst);
        }
        args_for.push(item);
        if shape.passes_index() {
            args_for.push(spares.cur);
        }
        if let Some((params, body)) = inline {
            let locals_before = self.st().locals.len();
            for (name, reg) in params.iter().zip(&args_for) {
                self.st().locals.push(Local { name, reg: *reg, is_var: false, captured: false, boxed: false });
            }
            let body_at = self.here();
            let out = self.expr(&body)?;
            self.st().locals.truncate(locals_before);
            decided = self.finish_element(shape, dst, item, out, base, body_at, top, spares)?;
        } else {
            let argc = args_for.len() as u16;
            self.reserve(base + argc - 1)?;
            let body_at = self.here();
            for (i, src) in args_for.iter().enumerate() {
                self.emit(Op::Move { dst: base + i as Reg, src: *src });
            }
            // The result lands where the first argument was: the callee's frame is dead
            // by then.
            self.emit(Op::Call { dst: base, func, base, argc });
            decided = self.finish_element(shape, dst, item, base, base, body_at, top, spares)?;
        }
        self.emit(Op::Jump { to: top });
        self.patch_to_here(top);
        // Reaching the end without an `Err` means the accumulator is the answer, wrapped:
        // `fold_try` gives `Try(state, err)`. An element that stopped early already holds
        // its `Err` and jumps PAST this.
        if matches!(shape, Shape::FoldTry) {
            let ok = self.name_idx("Ok")?;
            self.emit(Op::Move { dst: spares.slot, src: dst });
            self.emit(Op::MakeTag { dst, name: ok, base: spares.slot, n: 1 });
        }
        if let Some(at) = decided {
            self.patch_to_here(at);
        }
        self.st().next_reg = dst + 1;
        Ok(Some(dst))
    }

    /// What one element does with `out`, the value the callback answered.
    ///
    /// `Some(at)` is a jump out of the loop that still needs its target: only an
    /// `any`/`all` has one, and only the caller knows where the loop ends.
    fn finish_element(
        &mut self,
        shape: Shape,
        dst: Reg,
        item: Reg,
        out: Reg,
        base: Reg,
        body_at: u32,
        top: u32,
        spares: Spares,
    ) -> Result<Option<u32>, String> {
        match shape {
            Shape::Fold | Shape::FoldIndex => {
                // The accumulator is where the body's answer has to end up, so let the
                // body's last instruction write it there — the same destination hint
                // Phase 4.3 gave an assignment, which the lowered loops never got. It
                // was a third of everything `iter_range` executed.
                if out != dst && !self.wrote_directly(body_at, out, dst, base) {
                    self.emit(Op::Move { dst, src: out });
                }
                Ok(None)
            }
            Shape::Map => {
                // `ListPush` takes its element out of the register. A body that is just
                // a name answers with that name's own register, which must stay.
                let src = if out < base {
                    let copy = self.alloc()?;
                    self.emit(Op::Move { dst: copy, src: out });
                    copy
                } else {
                    out
                };
                self.emit(Op::ListPush { list: dst, src });
                Ok(None)
            }
            // The first element the predicate agrees with settles it: write the answer
            // and leave. Anything else — including a value that is not a `Bool` — goes
            // round again, which is what the builtin does.
            Shape::Decide(want) => {
                self.emit(Op::TestBool { cond: out, want, to: top });
                self.constant(dst, Value::Bool(want))?;
                let at = self.here();
                self.emit(Op::Jump { to: u32::MAX });
                Ok(Some(at))
            }
            // Every agreement adds one; nothing leaves early. `width: 0` because the
            // count has no declared integer width to overflow.
            Shape::Count => {
                self.emit(Op::TestBool { cond: out, want: true, to: top });
                self.emit(Op::BinInt { dst, a: dst, b: spares.one, op: crate::ast::BinOp::Add, width: 0 });
                Ok(None)
            }
            // `Ok(item)` and out. `MakeTag` wants its payload in consecutive registers,
            // so the item is copied into the spare slot first — copied and not moved,
            // because `dst` is built from it and the loop would otherwise push `Unit`.
            Shape::Find => {
                self.emit(Op::TestBool { cond: out, want: true, to: top });
                let name = self.name_idx("Ok")?;
                self.emit(Op::Move { dst: spares.slot, src: item });
                self.emit(Op::MakeTag { dst, name, base: spares.slot, n: 1 });
                let at = self.here();
                self.emit(Op::Jump { to: u32::MAX });
                Ok(Some(at))
            }
            // The POSITION, which `cur` already holds. `find_last_index` keeps looking
            // and lets a later element overwrite the answer; `find_first_index` leaves.
            Shape::FindIndex(first) => {
                self.emit(Op::TestBool { cond: out, want: true, to: top });
                let name = self.name_idx("Ok")?;
                self.emit(Op::MakeTag { dst, name, base: spares.cur, n: 1 });
                if !first {
                    return Ok(None);
                }
                let at = self.here();
                self.emit(Op::Jump { to: u32::MAX });
                Ok(Some(at))
            }
            // An `Err` is the answer and stops the fold; an `Ok` unwraps into the
            // accumulator. Anything else becomes the accumulator as it stands, which is
            // what the builtin's `other => acc = other` does — and `GetPayload` already
            // answers a non-tag with itself, so `TestTag "Ok"` covers both. Testing for
            // `Ok` rather than trusting the `Err` test to be exhaustive is what keeps a
            // payload-less tag from indexing an empty payload.
            Shape::FoldTry => {
                let err = self.name_idx("Err")?;
                let not_err = self.here();
                self.emit(Op::TestTag { obj: out, name: err, n: 1, to: u32::MAX });
                self.emit(Op::Move { dst, src: out });
                let at = self.here();
                self.emit(Op::Jump { to: u32::MAX });
                self.patch_to_here(not_err);
                let ok = self.name_idx("Ok")?;
                let not_ok = self.here();
                self.emit(Op::TestTag { obj: out, name: ok, n: 1, to: u32::MAX });
                self.emit(Op::GetPayload { dst, obj: out, i: 0 });
                let done = self.here();
                self.emit(Op::Jump { to: u32::MAX });
                self.patch_to_here(not_ok);
                self.emit(Op::Move { dst, src: out });
                self.patch_to_here(done);
                Ok(Some(at))
            }
        }
    }

    /// Bind `name` to `value` in a fresh register, and push it as a local.
    ///
    /// The caller pops the local when the binding goes out of scope.
    fn bind(&mut self, name: &'static str, value: &Expr, is_var: bool) -> Result<(), String> {
        self.bind_in(name, value, is_var, false)
    }

    /// `var name = value`; `boxed` when a lambda in scope mentions it, so it lives in
    /// a shared cell — see `Local::boxed`.
    fn bind_var(&mut self, name: &'static str, value: &Expr, boxed: bool) -> Result<(), String> {
        self.bind_in(name, value, true, boxed)
    }

    fn bind_in(&mut self, name: &'static str, value: &Expr, is_var: bool, boxed: bool) -> Result<(), String> {
        let save = self.st().next_reg;
        // A block-local nominal's member — `Local.get` bound where its nominal was
        // declared — has its siblings in scope UNQUALIFIED inside its body, the same
        // way a top-level method block's do.
        let outer_owner = self.global_owner;
        if let Some((owner, _)) = name.rsplit_once('.') {
            self.global_owner = Some(owner);
        }
        // A lambda bound by a `let` may call itself by name: record the name so the
        // body resolves it to the running closure rather than to a binding that does
        // not exist yet.
        let src = match value {
            Expr::Lambda { params, body, id } => {
                self.pending_coerce = self.coerce_params.get(id).cloned();
                self.closure(name, params, body, Some(name))
            }
            other => self.expr(other),
        };
        let src = match src {
            Ok(reg) => reg,
            Err(e) => {
                self.global_owner = outer_owner;
                return Err(e);
            }
        };
        self.st().next_reg = save;
        // The binding gets a register of its own. Aliasing `src` would break as soon
        // as `src` was a temporary the next expression reuses.
        let slot = self.alloc()?;
        if boxed {
            self.emit(Op::MakeCell { dst: slot, src });
        } else if slot != src {
            self.emit(Op::Move { dst: slot, src });
        }
        self.global_owner = outer_owner;
        self.st().locals.push(Local { name, reg: slot, is_var, captured: false, boxed });
        Ok(())
    }

    /// Compile `e`, and answer which register holds its value.
    ///
    /// The register may be a local's, so nothing may write to a returned register
    /// except through an op that reads its inputs first (`Bin` does) or a fresh
    /// destination (everything else).
    fn expr(&mut self, e: &Expr) -> Result<Reg, String> {
        // See `Unit::zero_sized_capacity`: the same call, asking for nothing.
        if self.zero_sized_capacity.remove(&e.id()) {
            if let Expr::Call { func, id, .. } = e {
                let zero = Expr::Call { id: *id, func: func.clone(), args: vec![Expr::Int(0, *id)] };
                return self.expr(&zero);
            }
        }
        // Checked against a nominal with a literal conversion: convert at run time if
        // a raw literal arrives. Taken out while the expression itself is compiled.
        if let Some(nominal) = self.coerce_values.remove(&e.id()) {
            // A LITERAL a nominal's `from_numeral` converts is folded before the
            // program runs, as roc folds it at compile time — see `Program::literal_coercions`.
            match e {
                Expr::Int(n, _) => self.literal_coercions.push((nominal, Value::Int(*n))),
                Expr::Float(f, ..) => self.literal_coercions.push((nominal, Value::Float(*f))),
                _ => {}
            }
            let compiled = self.expr(e);
            self.coerce_values.insert(e.id(), nominal);
            let src = compiled?;
            return self.coerce(src, nominal);
        }
        // Whatever this node emits is attributed to it. Restored afterwards so a
        // parent's own instructions are not blamed on its last child.
        let enclosing = std::mem::replace(&mut self.node, e.id());
        let result = self.expr_inner(e);
        self.node = enclosing;
        result
    }

    fn expr_inner(&mut self, e: &Expr) -> Result<Reg, String> {
        match e {
            // A literal that IS a nominal: the nominal's `from_numeral` of it.
            Expr::Int(n, id) if self.conversions.contains_key(id) => {
                let (module, method) = self.conversions[id];
                if method == "from_numeral" {
                    self.literal_coercions.push((module, Value::Int(*n)));
                }
                let numeral = self.numeral(id, Value::Int(*n))?;
                self.convert_literal(module, method, numeral)
            }
            Expr::Float(f, _, id) if self.conversions.contains_key(id) => {
                let (module, method) = self.conversions[id];
                if method == "from_numeral" {
                    self.literal_coercions.push((module, Value::Float(*f)));
                }
                let numeral = self.numeral(id, Value::Float(*f))?;
                self.convert_literal(module, method, numeral)
            }
            // A literal the checker typed as `Dec` is a FIXED-POINT value: `Dec` keeps
            // eighteen decimal places exactly, which an f64 cannot.
            Expr::Int(n, id) if self.dec_literals.contains(id) => {
                self.literal(Value::Dec(n * crate::eval::DEC_SCALE))
            }
            // Nothing ever said what this numeral is, so it defaults — and roc's
            // default is `Dec`, not a float. Checked against the compiler: a whole
            // `F64` prints `1500`, a whole `Dec` prints `1500.0`, and `x = 1500` with
            // no annotation prints `1500.0`.
            Expr::Int(n, id) if self.fractional_literals.contains(id) => {
                self.literal(Value::Dec(n.saturating_mul(crate::eval::DEC_SCALE)))
            }
            Expr::Int(n, id) if self.f32_literals.contains(id) => {
                self.literal(Value::F32(*n as f32))
            }
            Expr::Int(n, id) if self.u128_literals.contains(id) => {
                self.literal(Value::U128(*n as u128))
            }
            Expr::Int(n, _) => self.literal(Value::Int(*n)),
            // The literal as it was WRITTEN, not as the nearest double to it — and an
            // unpinned one is a `Dec` too, which is roc's default for `0.1 + 0.2`.
            Expr::Float(_, exact, id)
                if self.dec_literals.contains(id) || self.fractional_literals.contains(id) =>
            {
                self.literal(Value::Dec(*exact))
            }
            Expr::Float(f, _, id) if self.f32_literals.contains(id) => {
                self.literal(Value::F32(*f as f32))
            }
            Expr::Float(f, ..) => self.literal(Value::Float(*f)),
            Expr::Bool(b, _) => self.literal(Value::Bool(*b)),
            Expr::Str(s, id) if self.conversions.contains_key(id) => {
                let (module, method) = self.conversions[id];
                self.convert_literal(module, method, Value::Str(Rc::from(*s)))
            }
            Expr::StrInterp(parts, id) if self.conversions.contains_key(id) => {
                let (module, method) = self.conversions[id];
                self.interpolated(module, method, parts)
            }
            Expr::Str(s, _) => self.literal(Value::Str(Rc::from(*s))),
            // `{}` where a record of optional fields was expected: every slot missing.
            Expr::Unit(id) if self.default_sites.contains_key(id) => {
                let name = self.default_sites[id];
                self.build_defaulted_record(name, &[])
            }
            Expr::Unit(id) if self.missing_fields.contains_key(id) => {
                let left_out = self.missing_fields[id].clone();
                let name = self.names_run(&left_out)?;
                let base = self.st().next_reg;
                for _ in &left_out {
                    self.literal(Value::Missing)?;
                }
                self.st().next_reg = base;
                let dst = self.alloc()?;
                self.emit(Op::MakeRecord { dst, name, base, n: left_out.len() as u16 });
                Ok(dst)
            }
            Expr::Unit(_) => self.literal(Value::Unit),

            // `_` as a value is an unset optional field.
            Expr::Ident("_", _) => self.literal(Value::Missing),
            Expr::Ident(name, _) => self.use_name(name),

            Expr::Lambda { params, body, id } => {
                self.pending_coerce = self.coerce_params.get(id).cloned();
                self.closure("<lambda>", params, body, None)
            }

            // `and` and `or` short-circuit, as roc's do: `False and crash "x"` is `False`.
            Expr::BinOp { left, op: op @ (crate::ast::BinOp::And | crate::ast::BinOp::Or), right, .. } => {
                let save = self.st().next_reg;
                let dst = self.alloc()?;
                let a = self.expr(left)?;
                self.emit(Op::Move { dst, src: a });
                let skip = self.here();
                // `and`: a false left side IS the answer. `or`: a false left side means
                // the right side decides, so jump INTO it; a true one jumps past.
                self.emit(Op::JumpFalse { cond: dst, to: u32::MAX, kind: CondKind::Operand });
                if matches!(op, crate::ast::BinOp::And) {
                    let b = self.expr(right)?;
                    self.emit(Op::Move { dst, src: b });
                    self.patch_to_here(skip);
                } else {
                    let past = self.here();
                    self.emit(Op::Jump { to: u32::MAX });
                    self.patch_to_here(skip);
                    let b = self.expr(right)?;
                    self.emit(Op::Move { dst, src: b });
                    self.patch_to_here(past);
                }
                self.st().next_reg = save + 1;
                Ok(dst)
            }

            Expr::BinOp { left, op, right, id } => {
                let save = self.st().next_reg;
                let a = self.expr(left)?;
                let b = self.expr(right)?;
                self.st().next_reg = save;
                let dst = self.alloc()?;
                // An operator is dispatched only when the CHECKER says its operands are
                // a nominal that defines the matching method. `operator_methods` alone
                // is a program-wide switch: it sent every `==` through a method search,
                // so one `Try.is_eq` in scope answered for tuples and tags too.
                // The checker named the operands' nominal and it defines the method:
                // call that method, rather than a runtime search that cannot tell a
                // `Set` from the `Dict` it is built on.
                let direct = self.binop_modules.get(id).copied().and_then(|module| {
                    let method = operator_method_name(*op)?;
                    self.tops.func(qualify(module, method)).filter(|(_, arity)| *arity == 2).map(|(chunk, _)| chunk)
                });
                if let Some(chunk) = direct {
                    // The arguments go ABOVE both operands: `b` may sit right after
                    // `a`, and copying `a` into the first slot must not overwrite it.
                    self.st().next_reg = a.max(b).max(dst) + 1;
                    let arg_base = self.st().next_reg;
                    let first = self.alloc()?;
                    let second = self.alloc()?;
                    self.emit(Op::Move { dst: first, src: a });
                    self.emit(Op::Move { dst: second, src: b });
                    self.emit(Op::CallFn { dst, chunk, base: arg_base, argc: 2 });
                    if matches!(op, crate::ast::BinOp::Ne) {
                        let not = self.names_run(&["Bool", "not"])?;
                        self.emit(Op::CallBuiltin { dst, name: not, base: dst, argc: 1 });
                    }
                    self.st().next_reg = dst + 1;
                    return Ok(dst);
                }
                if self.tops.operator_methods && self.operator_dispatches(id, *op) {
                    self.emit(Op::BinDispatch { dst, a, b, op: *op });
                } else if self.integer_binops.contains(id)
                    && !matches!(op, crate::ast::BinOp::And | crate::ast::BinOp::Or)
                {
                    // The checker says both sides are integers, so the shapes need not
                    // be examined again at run time — and which width, so the result
                    // is checked against it.
                    let width = match op {
                        // `//` overflows only at the signed minimum divided by -1,
                        // which roc crashes on — so it carries the width too.
                        crate::ast::BinOp::Add | crate::ast::BinOp::Sub | crate::ast::BinOp::Mul
                        | crate::ast::BinOp::IntDiv => {
                            self.binop_modules.get(id).map_or(0, |m| crate::eval::width_code(m))
                        }
                        _ => 0,
                    };
                    let fused = Op::BinIntK { dst, a, k: 0, op: *op, width };
                    if !self.fuse_literal_operand(a, b, save, fused) {
                        self.emit(Op::BinInt { dst, a, b, op: *op, width });
                    }
                } else {
                    let fused = Op::BinK { dst, a, k: 0, op: *op };
                    if !self.fuse_literal_operand(a, b, save, fused) {
                        self.emit(Op::Bin { dst, a, b, op: *op });
                    }
                }
                Ok(dst)
            }

            Expr::If { condition, then_branch, otherwise, .. } => {
                let save = self.st().next_reg;
                let cond = self.expr(condition)?;
                self.st().next_reg = save;
                // The destination is allocated before either branch, so both can land
                // their result in the same place whichever way the jump goes.
                let dst = self.alloc()?;
                let jump_to_else = self.here();
                self.emit(Op::JumpFalse { cond, to: u32::MAX, kind: CondKind::If });

                let after_cond = self.st().next_reg;
                let then_reg = self.expr(then_branch)?;
                if then_reg != dst {
                    self.emit(Op::Move { dst, src: then_reg });
                }
                self.st().next_reg = after_cond;
                let jump_to_end = self.here();
                self.emit(Op::Jump { to: u32::MAX });

                self.patch_to_here(jump_to_else);
                let else_reg = self.expr(otherwise)?;
                if else_reg != dst {
                    self.emit(Op::Move { dst, src: else_reg });
                }
                self.st().next_reg = after_cond;
                self.patch_to_here(jump_to_end);
                Ok(dst)
            }

            Expr::Let { .. } | Expr::VarDecl { .. } | Expr::Assign { .. } => {
                let (rest, pushed) = self.statements(e)?;
                let result = self.expr(rest);
                self.pop_locals(pushed);
                result
            }

            // `return` in the middle of a block: the code after it is unreachable,
            // which is exactly what a jump to the function's exit means.
            Expr::Return(inner, _) if self.st().in_function => {
                let src = self.expr(inner)?;
                self.emit(Op::Ret { src });
                Ok(src)
            }
            Expr::Return(_, _) => Err("vm: `return` outside a function".to_string()),

            // `Json.parse(text)` is given the TYPE it must produce as a second
            // argument: nothing at run time can recover it, and reading `[1,2,3]` back
            // into a `List(ItemKind)` is only possible knowing it.
            Expr::Call { func, args, id }
                if matches!(&**func, Expr::Qualified { module: "Json", name: "parse", .. })
                    && self.parse_targets.contains_key(id) =>
            {
                let target = type_descriptor(&self.parse_targets[id]);
                let name = self.names_run(&["Json", "parse"])?;
                let arg_base = self.st().next_reg;
                let (_, argc) = self.arguments(args)?;
                self.st().next_reg = arg_base + argc as u16;
                let slot = self.alloc()?;
                self.constant(slot, target)?;
                self.st().next_reg = arg_base;
                let dst = self.alloc()?;
                self.emit(Op::CallBuiltin { dst, name, base: arg_base, argc: argc + 1 });
                Ok(dst)
            }

            Expr::Call { func, args, .. } => {
                if let Some((chunk, arity)) = self.direct_callee(func) {
                    let (arg_base, argc) = self.arguments(args)?;
                    check_arity(func_name(func), arity, argc)?;
                    self.st().next_reg = arg_base;
                    let dst = self.alloc()?;
                    self.emit(Op::CallFn { dst, chunk, base: arg_base, argc });
                    return Ok(dst);
                }
                if let Some(dst) = self.builtin_call(func, args)? {
                    return Ok(dst);
                }
                // The callee is a value in a register: a parameter, a capture, a
                // `let`-bound closure, or the result of another call.
                let save = self.st().next_reg;
                let callee = self.expr(func)?;
                let (arg_base, argc) = self.arguments(args)?;
                self.st().next_reg = save;
                let dst = self.alloc()?;
                self.emit(Op::Call { dst, func: callee, base: arg_base, argc });
                Ok(dst)
            }

            Expr::List(items, _) => {
                let (base, n) = self.arguments(items)?;
                self.st().next_reg = base;
                let dst = self.alloc()?;
                self.emit(Op::MakeList { dst, base, n });
                Ok(dst)
            }

            Expr::Tuple(items, _) => {
                let (base, n) = self.arguments(items)?;
                self.st().next_reg = base;
                let dst = self.alloc()?;
                self.emit(Op::MakeTuple { dst, base, n });
                Ok(dst)
            }

            Expr::Tag { name, args, .. } => {
                let name = self.name_idx(name)?;
                let (base, n) = self.arguments(args)?;
                self.st().next_reg = base;
                let dst = self.alloc()?;
                self.emit(Op::MakeTag { dst, name, base, n });
                Ok(dst)
            }

            Expr::Record(fields, id) if self.default_sites.contains_key(id) => {
                let name = self.default_sites[id];
                self.build_defaulted_record(name, fields)
            }
            Expr::Record(fields, id) => {
                // An optional field the literal left out is still a slot of the
                // record, holding `<missing>`.
                let left_out: Vec<&'static str> = self.missing_fields.get(id).cloned().unwrap_or_default();
                let mut field_names: Vec<&'static str> = fields.iter().map(|(n, _)| *n).collect();
                field_names.extend(left_out.iter().copied());
                let name = self.names_run(&field_names)?;
                let values: Vec<&Expr> = fields.iter().map(|(_, v)| v).collect();
                let (base, mut n) = self.values(&values)?;
                for _ in &left_out {
                    self.literal(Value::Missing)?;
                    n += 1;
                }
                self.st().next_reg = base;
                let dst = self.alloc()?;
                self.emit(Op::MakeRecord { dst, name, base, n });
                Ok(dst)
            }

            Expr::RecordUpdate { base: record, fields, .. } => {
                let field_names: Vec<&'static str> = fields.iter().map(|(n, _)| *n).collect();
                let name = self.names_run(&field_names)?;
                let save = self.st().next_reg;
                let obj = self.expr(record)?;
                self.reserve(obj)?;
                let values: Vec<&Expr> = fields.iter().map(|(_, v)| v).collect();
                let (base, n) = self.values(&values)?;
                self.st().next_reg = save;
                let dst = self.alloc()?;
                self.emit(Op::UpdateRecord { dst, obj, name, base, n, take: false });
                Ok(dst)
            }

            Expr::FieldAccess { record, field, .. } => {
                let name = self.name_idx(field)?;
                let save = self.st().next_reg;
                let obj = self.expr(record)?;
                self.st().next_reg = save;
                let dst = self.alloc()?;
                self.emit(Op::GetField { dst, obj, name });
                Ok(dst)
            }

            Expr::OptionalField { record, field, .. } => {
                let name = self.name_idx(field)?;
                let save = self.st().next_reg;
                let obj = self.expr(record)?;
                self.st().next_reg = save;
                let dst = self.alloc()?;
                self.emit(Op::GetOptField { dst, obj, name });
                Ok(dst)
            }

            Expr::TupleIndex { tuple, index, .. } => {
                let i = u16::try_from(*index)
                    .map_err(|_| "vm: tuple index out of range".to_string())?;
                let save = self.st().next_reg;
                let obj = self.expr(tuple)?;
                self.st().next_reg = save;
                let dst = self.alloc()?;
                self.emit(Op::GetIndex { dst, obj, i });
                Ok(dst)
            }

            Expr::Match { scrutinee, arms, id } => {
                let dst = self.compile_match(scrutinee, arms, false, self.match_types.get(id).cloned())?;
                Ok(dst.expect("a non-tail match has a destination"))
            }

            // `Module.name` as a VALUE — `xs.map(Str.inspect)`.
            Expr::Qualified { module, name, .. } => {
                // A nominal's method block binds `Type.method` as an ordinary
                // top-level name, so it resolves exactly like any other name — and it
                // need not be a function: `Counter.start = { n: 0 }` is a global.
                let qualified = qualify(module, name);
                if self.resolve(qualified).is_some() {
                    return self.use_name(qualified);
                }
                // `Bool.True` and `Bool.False` are VALUES, not nullary builtins.
                if *module == "Bool" {
                    match *name {
                        "True" => return self.literal(Value::Bool(true)),
                        "False" => return self.literal(Value::Bool(false)),
                        _ => {}
                    }
                }
                // `U64.highest` is a value too, so it is CALLED here rather than left
                // as a function to be called later — there is no later, a constant is
                // used where it stands.
                if crate::eval::is_numeric_constant(module, name) {
                    let idx = self.names_run(&[module, name])?;
                    let base = self.st().next_reg;
                    self.reserve(base)?;
                    self.st().next_reg = base;
                    let dst = self.alloc()?;
                    self.emit(Op::CallBuiltin { dst, name: idx, base, argc: 0 });
                    return Ok(dst);
                }
                let name = self.name_idx(qualified)?;
                let dst = self.alloc()?;
                self.emit(Op::MakeBuiltin { dst, name });
                Ok(dst)
            }

            Expr::StrInterp(parts, _) => self.interpolation(parts),

            Expr::Dispatch { receiver, method, args, id } => {
                self.dispatch(receiver, method, args, *id)
            }

            // The three statement forms. Each yields `{}`.
            Expr::Expect(condition, _) => {
                let save = self.st().next_reg;
                let cond = self.expr(condition)?;
                self.st().next_reg = save;
                self.emit(Op::Expect { cond });
                self.literal(Value::Unit)
            }

            Expr::Dbg(value, _) => {
                let save = self.st().next_reg;
                let src = self.expr(value)?;
                self.st().next_reg = save;
                self.emit(Op::Dbg { src });
                self.literal(Value::Unit)
            }

            Expr::Crash(message, _) => {
                let save = self.st().next_reg;
                let src = self.expr(message)?;
                self.st().next_reg = save;
                self.emit(Op::Crash { src });
                // Never reached: `Crash` leaves the program. Every expression still has
                // to answer with a register.
                self.literal(Value::Unit)
            }

            Expr::Range { start, end, inclusive, .. } => {
                let save = self.st().next_reg;
                let a = self.expr(start)?;
                let b = self.expr(end)?;
                self.st().next_reg = save;
                let dst = self.alloc()?;
                self.emit(Op::MakeRange { dst, start: a, end: b, inclusive: *inclusive });
                Ok(dst)
            }

            Expr::For { name, iterable, body, .. } => {
                self.for_loop(name, iterable, body)?;
                // `for` is a statement: its value is `{}`.
                self.literal(Value::Unit)
            }

            Expr::While { condition, body, .. } => {
                self.while_loop(condition, body)?;
                self.literal(Value::Unit)
            }

            Expr::Break(_) => {
                if self.st().loops.is_empty() {
                    // The tree-walker raised a break signal that nothing caught, so
                    // what a bare `break` does there is not worth copying.
                    return Err("vm: `break` outside a loop".to_string());
                }
                let at = self.here();
                self.emit(Op::Jump { to: u32::MAX });
                self.st().loops.last_mut().expect("checked above").push(at);
                // `break` leaves the loop, so nothing reads this register — but every
                // expression has to answer with one.
                self.literal(Value::Unit)
            }

            // No catch-all. Every `Expr` variant is compiled, and an exhaustive match
            // is what keeps it that way: a new one added to the AST is a compile error
            // here rather than a program the VM quietly refuses at run time.
        }
    }

    /// A `match`, as a chain of compare-and-branch.
    ///
    /// Every arm's tests jump to the next alternative on failure, so a match costs the
    /// tests it actually runs and nothing else. The tree-walker instead allocated a
    /// `Vec` of bindings per pattern attempt, pushes a scope, and binds each name into
    /// it — for every arm it tries, not just the one that wins.
    ///
    /// In tail position each arm's body ends the function itself, so there is no result
    /// register and no jump to a common exit: `area = |s| match s { ... }` returns
    /// straight out of the arm that matched.
    fn compile_match(
        &mut self,
        scrutinee: &Expr,
        arms: &[MatchArm],
        tail: bool,
        scrutinee_type: Option<crate::types::Type>,
    ) -> Result<Option<Reg>, String> {
        let v = self.expr(scrutinee)?;
        // The scrutinee is read by every arm, so its register stays allocated for the
        // whole match rather than being freed as an ordinary temporary.
        self.reserve(v)?;
        let dst = if tail { None } else { Some(self.alloc()?) };
        let arm_base = self.st().next_reg;

        let mut ends: Vec<u32> = Vec::new();
        for arm in arms {
            // `A | B => body` compiles the body once per alternative. Sharing it would
            // mean the alternatives had to agree on which register each binding lives
            // in, which they do not.
            for pattern in &arm.patterns {
                let locals_before = self.st().locals.len();
                let mut fails: Vec<u32> = Vec::new();
                self.pattern(pattern, v, &mut fails, scrutinee_type.as_ref())?;

                if let Some(guard) = &arm.guard {
                    // The guard sees the pattern's bindings, and a false guard skips
                    // the arm rather than failing the match.
                    let save = self.st().next_reg;
                    let cond = self.expr(guard)?;
                    self.st().next_reg = save;
                    fails.push(self.here());
                    self.emit(Op::JumpFalse { cond, to: u32::MAX, kind: CondKind::Guard });
                }

                match dst {
                    None => self.tail(&arm.body)?,
                    Some(dst) => {
                        let save = self.st().next_reg;
                        let body = self.expr(&arm.body)?;
                        if body != dst {
                            self.emit(Op::Move { dst, src: body });
                        }
                        self.st().next_reg = save;
                        ends.push(self.here());
                        self.emit(Op::Jump { to: u32::MAX });
                    }
                }

                for at in fails {
                    self.patch_to_here(at);
                }
                // The bindings and registers of a failed arm are dead: the next arm
                // reuses the registers, and no name it did not bind is in scope.
                self.st().locals.truncate(locals_before);
                self.st().next_reg = arm_base;
            }
        }

        // roc requires the arms to be exhaustive and this interpreter cannot verify
        // that, so falling off the end reports it rather than inventing a value.
        self.emit(Op::NoMatch { obj: v });
        for at in ends {
            self.patch_to_here(at);
        }
        Ok(dst)
    }

    /// Emit the tests that `pattern` implies, and the destructuring its sub-patterns
    /// need. Every test's jump is collected in `fails`, for the caller to point at the
    /// next alternative.
    ///
    /// Bindings become locals. A binding written before a LATER test fails is simply
    /// dead — the register is reused and the local is popped — which is the compiled
    /// equivalent of the tree-walker throwing away a half-filled bindings vector.
    fn pattern(
        &mut self,
        pattern: &Pattern,
        v: Reg,
        fails: &mut Vec<u32>,
        ty: Option<&crate::types::Type>,
    ) -> Result<(), String> {
        use crate::types::Type;
        // What the value under this pattern is, through a nominal that is not itself
        // built from literals; `None` where the checker did not say.
        let shape = |ty: Option<&Type>| -> Option<Type> {
            match ty? {
                Type::Nominal { backing, .. } => Some((**backing).clone()),
                other => Some(other.clone()),
            }
        };
        match pattern {
            Pattern::Wildcard => Ok(()),

            // `(word, index) = pair` where `index` is a `var` in scope assigns it, as
            // roc does, rather than binding a new name over it — which is what left the
            // `while` loop spinning, its counter reset to the shadowed original.
            Pattern::Binding(name) if self.st().local(name).is_some_and(|l| l.is_var) => {
                let (reg, boxed) = self.st().local(name).map(|l| (l.reg, l.boxed)).expect("checked");
                if boxed {
                    self.emit(Op::CellSet { cell: reg, src: v });
                } else {
                    self.emit(Op::Move { dst: reg, src: v });
                }
                Ok(())
            }
            Pattern::Binding(name) => {
                let slot = self.alloc()?;
                self.emit(Op::Move { dst: slot, src: v });
                self.st().locals.push(Local { name, reg: slot, is_var: false, captured: false, boxed: false });
                Ok(())
            }

            // A nominal is erased at run time: its payload is the value itself.
            Pattern::Nominal { inner, .. } => self.pattern(inner, v, fails, ty),

            // The whole value under `name`, and then the inner pattern against it.
            Pattern::As { name, inner } => {
                self.pattern(&Pattern::Binding(name), v, fails, None)?;
                self.pattern(inner, v, fails, ty)
            }

            // `TestStr` writes each capture into a register of its own, and the
            // captures then bind like plain names.
            Pattern::StrInterp { segments, .. } => {
                let pat = self.pat_idx(pattern.clone())?;
                let base = self.st().next_reg;
                for _ in segments {
                    self.alloc()?;
                }
                fails.push(self.here());
                self.emit(Op::TestStr { obj: v, pat, base, to: u32::MAX });
                for (i, (name, _)) in segments.iter().enumerate() {
                    if *name != "_" {
                        self.pattern(&Pattern::Binding(name), base + i as u16, fails, None)?;
                    }
                }
                Ok(())
            }

            // A literal against a nominal built from literals: the nominal's conversion
            // of the literal, compared with its `is_eq` — or structurally, where the
            // equality is derived.
            Pattern::Int(_) | Pattern::Float(..) | Pattern::Str(_)
                if matches!(ty, Some(Type::Nominal { name, .. })
                    if self.tops.func(qualify(name, "from_numeral")).is_some()
                        || self.tops.func(qualify(name, "from_quote")).is_some()) =>
            {
                let Some(Type::Nominal { name, .. }) = ty else { unreachable!("matched") };
                let nominal: &'static str = crate::memory::string_pool::intern(name);
                let save = self.st().next_reg;
                let (method, arg) = match pattern {
                    Pattern::Int(n) => ("from_numeral", crate::eval::numeral::numeral_from_value(&Value::Int(*n))),
                    Pattern::Float(f, _) => ("from_numeral", crate::eval::numeral::numeral_from_value(&Value::Float(*f))),
                    Pattern::Str(s) => ("from_quote", Some(Value::Str(Rc::from(*s)))),
                    _ => unreachable!("matched"),
                };
                let arg = arg.ok_or_else(|| format!("vm: {:?} cannot be read as a numeral", pattern))?;
                let converted = self.convert_literal(nominal, method, arg)?;
                let cond = match self.tops.func(qualify(nominal, "is_eq")).filter(|(_, arity)| *arity == 2) {
                    Some((chunk, _)) => {
                        let base = self.st().next_reg;
                        let a = self.alloc()?;
                        let b = self.alloc()?;
                        self.emit(Op::Move { dst: a, src: v });
                        self.emit(Op::Move { dst: b, src: converted });
                        let dst = self.alloc()?;
                        self.emit(Op::CallFn { dst, chunk, base, argc: 2 });
                        dst
                    }
                    None => {
                        let dst = self.alloc()?;
                        self.emit(Op::Bin { dst, a: v, b: converted, op: crate::ast::BinOp::Eq });
                        dst
                    }
                };
                fails.push(self.here());
                self.emit(Op::JumpFalse { cond, to: u32::MAX, kind: CondKind::Guard });
                self.st().next_reg = save;
                Ok(())
            }
            // The value's type is not known HERE — a `where`-constrained parameter —
            // but a nominal declared in this block supplies both the conversion and
            // the equality as ordinary locals, because a block-local method may
            // capture. Exactly one such pair in scope is the one meant; the runtime
            // test below cannot see them, since they are closures in registers rather
            // than entries in the program's method table.
            Pattern::Int(_) | Pattern::Float(..) | Pattern::Str(_)
                if !matches!(ty, Some(t) if !matches!(t, Type::TypeVar(_)))
                    && self.unique_scoped_method(match pattern {
                        Pattern::Str(_) => "from_quote",
                        _ => "from_numeral",
                    }).is_some()
                    && self.unique_scoped_method("is_eq").is_some() =>
            {
                let (method, arg) = match pattern {
                    Pattern::Int(n) => ("from_numeral", crate::eval::numeral::numeral_from_value(&Value::Int(*n))),
                    Pattern::Float(f, _) => ("from_numeral", crate::eval::numeral::numeral_from_value(&Value::Float(*f))),
                    Pattern::Str(s) => ("from_quote", Some(Value::Str(Rc::from(*s)))),
                    _ => unreachable!("matched"),
                };
                let arg = arg.ok_or_else(|| format!("vm: {:?} cannot be read as a numeral", pattern))?;
                let convert = self.unique_scoped_method(method).expect("guarded");
                let equals = self.unique_scoped_method("is_eq").expect("guarded");
                let save = self.st().next_reg;
                let func = self.use_name(convert)?;
                let base = self.st().next_reg;
                self.place(base, |c| c.literal(arg))?;
                self.st().next_reg = base + 1;
                let built = self.alloc()?;
                self.emit(Op::Call { dst: built, func, base, argc: 1 });
                let converted = self.unwrap_ok(built)?;
                let eq = self.use_name(equals)?;
                let pair = self.st().next_reg;
                let a = self.alloc()?;
                let b = self.alloc()?;
                self.emit(Op::Move { dst: a, src: v });
                self.emit(Op::Move { dst: b, src: converted });
                let cond = self.alloc()?;
                self.emit(Op::Call { dst: cond, func: eq, base: pair, argc: 2 });
                fails.push(self.here());
                self.emit(Op::JumpFalse { cond, to: u32::MAX, kind: CondKind::Guard });
                self.st().next_reg = save;
                Ok(())
            }

            // The value's type is not known here and the program has such nominals:
            // the value says at run time whether it is one.
            Pattern::Int(_) | Pattern::Float(..) | Pattern::Str(_)
                if !matches!(ty, Some(t) if !matches!(t, Type::TypeVar(_))) && self.converts_literals() =>
            {
                let pat = self.pat_idx(pattern.clone())?;
                fails.push(self.here());
                self.emit(Op::TestLitDyn { obj: v, pat, to: u32::MAX });
                Ok(())
            }
            Pattern::Int(_) | Pattern::Float(..) | Pattern::Str(_) => {
                let pat = self.pat_idx(pattern.clone())?;
                fails.push(self.here());
                self.emit(Op::TestLit { obj: v, pat, to: u32::MAX });
                Ok(())
            }

            Pattern::Tag { name, args } => {
                let name_idx = self.name_idx(name)?;
                let n = u16::try_from(args.len())
                    .map_err(|_| "vm: too many tag arguments".to_string())?;
                fails.push(self.here());
                self.emit(Op::TestTag { obj: v, name: name_idx, n, to: u32::MAX });
                let payload: Option<Vec<Type>> = match shape(ty) {
                    Some(Type::TagUnion { tags, .. }) => tags.into_iter().find(|(t, _)| t == name).map(|(_, p)| p),
                    _ => None,
                };
                for (i, arg) in args.iter().enumerate() {
                    if matches!(arg, Pattern::Wildcard) {
                        continue;
                    }
                    let elem = self.alloc()?;
                    self.emit(Op::GetPayload { dst: elem, obj: v, i: i as u16 });
                    self.pattern(arg, elem, fails, payload.as_ref().and_then(|p| p.get(i)))?;
                }
                Ok(())
            }

            Pattern::Tuple(items) => {
                let n = u16::try_from(items.len())
                    .map_err(|_| "vm: too many tuple elements".to_string())?;
                fails.push(self.here());
                self.emit(Op::TestTuple { obj: v, n, to: u32::MAX });
                let item_types: Option<Vec<Type>> = match shape(ty) {
                    Some(Type::Tuple(types)) => Some(types),
                    _ => None,
                };
                for (i, item) in items.iter().enumerate() {
                    if matches!(item, Pattern::Wildcard) {
                        continue;
                    }
                    let elem = self.alloc()?;
                    self.emit(Op::GetIndex { dst: elem, obj: v, i: i as u16 });
                    self.pattern(item, elem, fails, item_types.as_ref().and_then(|t| t.get(i)))?;
                }
                Ok(())
            }

            Pattern::Record { fields, rest } => {
                fails.push(self.here());
                self.emit(Op::TestRecord { obj: v, to: u32::MAX });
                for (field, sub) in fields {
                    let name = self.name_idx(field)?;
                    let slot = self.alloc()?;
                    // A record that lacks a named field is not a match, not an error,
                    // so this read is itself one of the tests.
                    fails.push(self.here());
                    self.emit(Op::GetFieldOr { dst: slot, obj: v, name, to: u32::MAX });
                    let field_type = match shape(ty) {
                        Some(Type::Record { fields, .. }) => fields.into_iter().find(|(f, _)| f == field).map(|(_, t)| match t {
                            Type::Optional(inner) => *inner,
                            other => other,
                        }),
                        _ => None,
                    };
                    self.pattern(sub, slot, fails, field_type.as_ref())?;
                }
                if let Some(rest_name) = rest {
                    // `..rest` binds every field the pattern did NOT name.
                    let named: Vec<&'static str> = fields.iter().map(|(f, _)| *f).collect();
                    let name = self.names_run(&named)?;
                    let n = u16::try_from(named.len())
                        .map_err(|_| "vm: too many record fields".to_string())?;
                    let slot = self.alloc()?;
                    self.emit(Op::GetRest { dst: slot, obj: v, name, n });
                    self.st()
                        .locals
                        .push(Local { name: rest_name, reg: slot, is_var: false, captured: false, boxed: false });
                }
                Ok(())
            }

            Pattern::List { before, rest, after } => {
                let fixed = before.len() + after.len();
                let n = u16::try_from(fixed).map_err(|_| "vm: list pattern too long".to_string())?;
                fails.push(self.here());
                // Without a `..` the length has to be exact; with one the list only has
                // to be long enough to cover the fixed patterns.
                self.emit(Op::TestList { obj: v, n, exact: rest.is_none(), to: u32::MAX });
                let element: Option<Type> = match shape(ty) {
                    Some(Type::List(inner)) => Some(*inner),
                    _ => None,
                };

                for (i, item) in before.iter().enumerate() {
                    if matches!(item, Pattern::Wildcard) {
                        continue;
                    }
                    let elem = self.alloc()?;
                    self.emit(Op::GetElem { dst: elem, obj: v, i: i as u16, from_end: false });
                    self.pattern(item, elem, fails, element.as_ref())?;
                }
                // The trailing patterns are positioned from the END, since what `..`
                // absorbed is only known at run time.
                for (j, item) in after.iter().enumerate() {
                    if matches!(item, Pattern::Wildcard) {
                        continue;
                    }
                    let from_end = (after.len() - 1 - j) as u16;
                    let elem = self.alloc()?;
                    self.emit(Op::GetElem { dst: elem, obj: v, i: from_end, from_end: true });
                    self.pattern(item, elem, fails, element.as_ref())?;
                }
                if let Some(Some(rest_name)) = rest {
                    let slot = self.alloc()?;
                    self.emit(Op::GetSlice {
                        dst: slot,
                        obj: v,
                        front: before.len() as u16,
                        back: after.len() as u16,
                    });
                    self.st()
                        .locals
                        .push(Local { name: rest_name, reg: slot, is_var: false, captured: false, boxed: false });
                }
                Ok(())
            }
        }
    }

    /// Emit the code that reads `name`, wherever the compiler decided it lives.
    fn use_name(&mut self, name: &'static str) -> Result<Reg, String> {
        match self.resolve(name) {
            Some(Found::Local(reg)) => Ok(reg),
            Some(Found::LocalCell(cell)) => {
                let dst = self.alloc()?;
                self.emit(Op::CellGet { dst, cell });
                Ok(dst)
            }
            Some(Found::CaptureCell(idx)) => {
                let dst = self.alloc()?;
                self.emit(Op::LoadCap { dst, idx });
                self.emit(Op::CellGet { dst, cell: dst });
                Ok(dst)
            }
            Some(Found::SelfRef) => {
                let dst = self.alloc()?;
                self.emit(Op::LoadSelf { dst });
                Ok(dst)
            }
            Some(Found::Capture(idx)) => {
                let dst = self.alloc()?;
                self.emit(Op::LoadCap { dst, idx });
                Ok(dst)
            }
            Some(Found::Global(idx)) => {
                let dst = self.alloc()?;
                self.emit(Op::LoadGlob { dst, idx });
                Ok(dst)
            }
            // A top-level function used as a value: a closure over nothing.
            Some(Found::Func(chunk, _)) => {
                let dst = self.alloc()?;
                self.emit(Op::MakeClosure { dst, chunk, base: dst, n: 0 });
                Ok(dst)
            }
            Some(Found::Refused(why)) => Err(why),
            None => {
                // The same sibling rule, for a method used as a VALUE rather than
                // called: `map(xs, helper)` inside the block `helper` belongs to.
                for owner in self.enclosing_owners_of() {
                    let qualified = qualify(owner, name);
                    if self.resolve(qualified).is_some() {
                        return self.use_name(qualified);
                    }
                }
                Err(format!("Undefined variable: {}", name))
            }
        }
    }

    /// A call to a builtin, a host effect, or a nominal's method by qualified name.
    ///
    /// `None` when the callee is none of those, and the caller falls back to calling a
    /// value in a register.
    fn builtin_call(&mut self, func: &Expr, args: &[Expr]) -> Result<Option<Reg>, String> {
        match func {
            // `Str.concat(a, b)`, or `Point.show(p)` for a nominal's own method.
            Expr::Qualified { module, name, .. } => {
                let qualified = qualify(module, name);
                // A nominal name that is not a function — `Counter.start = { n: 0 }` —
                // is a value being called, which the caller compiles as such.
                if self.tops.global(qualified).is_some() {
                    return Ok(None);
                }
                if let Some((chunk, arity)) = self.tops.func(qualified) {
                    let (arg_base, argc) = self.arguments(args)?;
                    check_arity(qualified, arity, argc)?;
                    self.st().next_reg = arg_base;
                    let dst = self.alloc()?;
                    self.emit(Op::CallFn { dst, chunk, base: arg_base, argc });
                    return Ok(Some(dst));
                }
                // `List.fold(xs, init, f)` is `xs.fold(init, f)` with the receiver
                // written first, and compiles to the same loop.
                if *module == "List" {
                    if let Some((receiver, rest)) = args.split_first() {
                        if let Some(dst) = self.list_loop(name, receiver, rest, module)? {
                            return Ok(Some(dst));
                        }
                    }
                }
                let name = self.names_run(&[module, name])?;
                let (arg_base, argc) = self.arguments(args)?;
                self.st().next_reg = arg_base;
                let dst = self.alloc()?;
                self.emit(Op::CallBuiltin { dst, name, base: arg_base, argc });
                Ok(Some(dst))
            }

            Expr::Ident(bare, _) => {
                // A local, a capture or a global shadows all of this: a name bound in
                // the program is called as a value, not as a builtin.
                if self.resolve(bare).is_some() {
                    return Ok(None);
                }
                // A SIBLING method, called by its bare name from inside the same
                // method block: `from_list = |l| from_dict(…)` inside `Graph`.
                for owner in self.enclosing_owners_of() {
                    if let Some((chunk, arity)) = self.tops.func(qualify(owner, bare)) {
                        let (arg_base, argc) = self.arguments(args)?;
                        check_arity(bare, arity, argc)?;
                        self.st().next_reg = arg_base;
                        let dst = self.alloc()?;
                        self.emit(Op::CallFn { dst, chunk, base: arg_base, argc });
                        return Ok(Some(dst));
                    }
                }
                // An effect of the default host. `!` is part of the name.
                if crate::platform::host::lookup(bare).is_some() {
                    let name = self.name_idx(bare)?;
                    let (arg_base, argc) = self.arguments(args)?;
                    self.st().next_reg = arg_base;
                    let dst = self.alloc()?;
                    self.emit(Op::CallHost { dst, name, base: arg_base, argc });
                    return Ok(Some(dst));
                }
                // A LOW-LEVEL op: a bare name `Builtin.roc` calls but never defines,
                // which the real compiler injects and rocflight answers from Rust.
                // Sent under a module of its own, because Roc has no module for these.
                if crate::eval::low_level_arity(bare).is_some() || self.intrinsics.contains(bare) {
                    if let Some(arity) = crate::eval::low_level_arity(bare) {
                        check_arity(bare, arity as u16, args.len() as u16)?;
                    }
                    let name = self.names_run(&["LowLevel", bare])?;
                    let (arg_base, argc) = self.arguments(args)?;
                    self.st().next_reg = arg_base;
                    let dst = self.alloc()?;
                    self.emit(Op::CallBuiltin { dst, name, base: arg_base, argc });
                    return Ok(Some(dst));
                }
                // Bare `to_str(x)`. The tree-walker stringified any value here, which
                // is what `Num.to_str` does, so it goes to the same place.
                if *bare == "to_str" {
                    let name = self.names_run(&["Num", "to_str"])?;
                    let (arg_base, argc) = self.arguments(args)?;
                    self.st().next_reg = arg_base;
                    let dst = self.alloc()?;
                    self.emit(Op::CallBuiltin { dst, name, base: arg_base, argc });
                    return Ok(Some(dst));
                }
                Ok(None)
            }

            _ => Ok(None),
        }
    }

    /// Does this operator go through a method?
    ///
    /// Yes when the checker named a module that defines one. Yes ALSO when it named no
    /// module at all: roc erases nominals, so an unannotated `Money.{ cents: 5 }` is
    /// just a record here and its `plus` has to stay reachable. No when the module is
    /// named and has no such method — which is what keeps `1 + 2`, `"a" == "b"` and a
    /// tuple comparison away from whatever `is_eq` happens to be in scope.
    fn operator_dispatches(&self, node: &crate::ast::NodeId, op: crate::ast::BinOp) -> bool {
        let Some(method) = operator_method_name(op) else { return false };
        match self.binop_modules.get(node) {
            Some(module) => self.tops.func(qualify(module, method)).is_some(),
            None => true,
        }
    }

    /// `receiver.method(args)`.
    ///
    /// A nominal's own method block wins, and the compiler can resolve it: it is a
    /// top-level function whose name ends in `.method`. Everything else depends on the
    /// receiver's type at run time and becomes `DispatchMethod`.
    fn dispatch(
        &mut self,
        receiver: &Expr,
        method: &'static str,
        args: &[Expr],
        node: crate::ast::NodeId,
    ) -> Result<Reg, String> {
        let all = self.tops.methods(method);

        // `iter.collect()` is `Output.from_iter(iterator)`: the checker settled which
        // `Output` the annotation asked for, and only that type's `from_iter` builds
        // the right value. Taking the receiver's own module here — `List`/`Iter` — got
        // the builtin `collect`, which materialized a plain list and left
        // `Set.to_list` with no nominal to match.
        // A nominal declared INSIDE a block binds its methods as ordinary local
        // names — they may capture the enclosing scope, so they are closures rather
        // than top-level chunks. The checker says which nominal the receiver is; the
        // method is then just a value in scope, called with the receiver first.
        let named = self.dispatch_modules.get(&node).copied().map(|module| qualify(module, method));
        // With no module named — a generic parameter, whose type only the call site
        // knows — a single in-scope `Type.method` binding is the one meant, the same
        // rule `DispatchMethod` applies to the global table at run time.
        let named = named.or_else(|| {
            self.tops.methods(method).is_empty().then(|| self.unique_scoped_method(method)).flatten()
        });
        if let Some(qualified) = named {
            if self.st().local(qualified).is_some() || matches!(self.resolve(qualified), Some(Found::Capture(_))) {
                let save = self.st().next_reg;
                let callee = self.use_name(qualified)?;
                let arg_base = self.st().next_reg;
                self.reserve(arg_base)?;
                let inner = self.st().next_reg;
                let got = self.expr(receiver)?;
                if got != arg_base {
                    self.emit(Op::Move { dst: arg_base, src: got });
                }
                self.st().next_reg = inner;
                let (_, rest) = self.arguments(args)?;
                self.st().next_reg = save;
                let dst = self.alloc()?;
                self.emit(Op::Call { dst, func: callee, base: arg_base, argc: rest + 1 });
                return Ok(dst);
            }
        }

        let forced: Option<(&'static str, ChunkId, u16)> = (method == "collect")
            .then(|| self.collect_targets.get(&node).cloned())
            .flatten()
            .and_then(|nominal| {
                let owner = qualify(&nominal, "from_iter");
                self.tops.methods("from_iter").iter().copied().find(|(name, ..)| *name == owner)
            });

        // What the CHECKER says the receiver is. A method name alone cannot pick a
        // definition once more than one type defines it, and `Builtin.roc` has every
        // type defining `map`, `len`, `is_eq` and `to_hash`. With the receiver's module
        // in hand the choice is exact: `Type.method` if that type defines one, and
        // otherwise the builtin for that module, whatever else is in scope.
        let candidates: Vec<(&'static str, ChunkId, u16)> =
            match self.dispatch_modules.get(&node) {
                Some(module) => {
                    let owner = qualify(module, method);
                    all.iter().copied().filter(|(name, ..)| *name == owner).collect()
                }
                // No module: a bare record or tag, or a variable a `where` clause
                // covers. Unchanged from before — the single candidate by name, or a
                // runtime dispatch on the value.
                None => all,
            };

        // With no module named, the choice belongs to the running value: `DispatchMethod`
        // looks the method up by what the receiver turns out to be, and falls back to a
        // uniquely-named one for a nominal, which is a bare record at run time. Picking
        // the only candidate HERE is what made a loaded `Stream` answer `xs.map(f)`.
        let candidates: Vec<(&'static str, ChunkId, u16)> = match forced {
            Some(one) => vec![one],
            None if self.dispatch_modules.contains_key(&node) => candidates,
            None => Vec::new(),
        };

        // A list's `fold` or `map`, with nothing roc-defined answering to it: a loop
        // in this frame rather than a builtin that re-enters the VM per element.
        // `Iter` is what `.iter()` is declared to give; at run time it is the list.
        if let Some(module) = self
            .dispatch_modules
            .get(&node)
            .copied()
            .filter(|m| candidates.is_empty() && matches!(*m, "List" | "Iter"))
        {
            if let Some(dst) = self.list_loop(method, receiver, args, module)? {
                return Ok(dst);
            }
        }

        // The receiver is the first argument either way — which is why roc's builtins
        // take their subject first: `xs.map(f)` is `List.map(xs, f)`.
        let arg_base = self.st().next_reg;
        self.reserve(arg_base)?;
        let save = self.st().next_reg;
        let got = self.expr(receiver)?;
        if got != arg_base {
            self.emit(Op::Move { dst: arg_base, src: got });
        }
        self.st().next_reg = save;
        let (_, rest) = self.arguments(args)?;
        let argc = rest + 1;
        self.st().next_reg = arg_base;
        let dst = self.alloc()?;

        match candidates.first() {
            Some(&(name, chunk, arity)) => {
                check_arity(name, arity, argc)?;
                self.emit(Op::CallFn { dst, chunk, base: arg_base, argc });
            }
            // A numeric receiver whose WIDTH the checker knows: call the builtin for
            // that width. A runtime dispatch would read the value's module instead,
            // and every integer value says `I64` — so `x.shl_wrap(1)` on a `U8` was
            // shifted at 64 bits.
            None if self.dispatch_modules.get(&node).is_some_and(|m| crate::eval::is_numeric_module(m) || *m == "Numeral") => {
                let module = self.dispatch_modules[&node];
                let name = self.names_run(&[module, method])?;
                self.emit(Op::CallBuiltin { dst, name, base: arg_base, argc });
            }
            None => {
                let name = self.name_idx(method)?;
                self.emit(Op::DispatchMethod { dst, name, base: arg_base, argc });
            }
        }
        Ok(dst)
    }

    /// Build a nominal's record with its omitted fields materialized: the fields the
    /// literal wrote, plus each defaulted field's default expression, plus `<missing>`
    /// for each optional field left out. This is what makes a bare `{}` or a partial
    /// `{ bar: n }` checked against a nominal carry that nominal's defaults, even when
    /// the parser could not tell the type at the literal.
    fn build_defaulted_record(&mut self, nominal: &'static str, written: &[(&'static str, Expr)]) -> Result<Reg, String> {
        let defaults = self.nominal_defaults.iter().find(|(n, _)| n == nominal).map(|(_, d)| d.clone()).unwrap_or_default();
        let all_fields: &[(&'static str, crate::types::Type)] =
            self.nominal_records.get(nominal).copied().unwrap_or(&[]);
        // Collect the pieces: (field_name, source) where source is a written expr, a
        // default expr, or `<missing>`.
        enum Src<'a> { Expr(&'a Expr), Default(Expr), Missing }
        let mut pieces: Vec<(&'static str, Src)> = Vec::new();
        for (name, value) in written {
            pieces.push((*name, Src::Expr(value)));
        }
        for (field, ty) in all_fields {
            if written.iter().any(|(w, _)| w == field) {
                continue;
            }
            // Already interned — it came out of the unit's own type.
            let field_name: &'static str = field;
            if let Some((_, default)) = defaults.iter().find(|(f, _)| f == field) {
                pieces.push((field_name, Src::Default(default.clone())));
            } else if matches!(ty, crate::types::Type::Optional(_)) {
                pieces.push((field_name, Src::Missing));
            }
        }
        // A field with a default that the nominal_records did not list (e.g. an
        // imported nominal whose type is not in scope) is still filled.
        for (field, default) in &defaults {
            let field_name = crate::memory::string_pool::intern(field);
            if !pieces.iter().any(|(n, _)| *n == field_name) {
                pieces.push((field_name, Src::Default(default.clone())));
            }
        }
        let names: Vec<&'static str> = pieces.iter().map(|(n, _)| *n).collect();
        let name_idx = self.names_run(&names)?;
        let base = self.st().next_reg;
        let n = u16::try_from(pieces.len()).map_err(|_| "vm: too many record fields".to_string())?;
        for (i, (_, src)) in pieces.iter().enumerate() {
            let target = base + i as Reg;
            self.reserve(target)?;
            let save = self.st().next_reg;
            let got = match src {
                Src::Expr(e) => self.expr(e)?,
                Src::Default(e) => self.expr(e)?,
                Src::Missing => self.literal(Value::Missing)?,
            };
            if got != target {
                self.emit(Op::Move { dst: target, src: got });
            }
            self.st().next_reg = save;
        }
        self.st().next_reg = base;
        let dst = self.alloc()?;
        self.emit(Op::MakeRecord { dst, name: name_idx, base, n });
        Ok(dst)
    }

    /// `"a${x}b"` — the literal segments and the values between them.
    ///
    /// Normalised to strict alternation at compile time: n values and n+1 literals,
    /// with empty literals inserted where the source has two expressions in a row. The
    /// opcode then needs no structure of its own.
    fn interpolation(&mut self, parts: &[StrPart]) -> Result<Reg, String> {
        let mut literals: Vec<&'static str> = vec![""];
        let mut exprs: Vec<&Expr> = Vec::new();
        for part in parts {
            match part {
                StrPart::Literal(text) => {
                    // Two literals in a row would need joining, which the parser does
                    // not produce; a literal after a value starts a new segment.
                    let last = literals.last_mut().expect("seeded with one");
                    if last.is_empty() {
                        *last = text;
                    } else {
                        return Err("vm: two string literals in a row".to_string());
                    }
                }
                StrPart::Expr(e) => {
                    exprs.push(e);
                    literals.push("");
                }
            }
        }
        let name = self.names_run(&literals)?;
        let (base, n) = self.values(&exprs)?;
        self.st().next_reg = base;
        let dst = self.alloc()?;
        self.emit(Op::Interp { dst, name, base, n });
        Ok(dst)
    }

    /// `x = value` where `x` already exists: a write to wherever it lives.
    ///
    /// The tree-walker's `assign` walked the scopes and updated the binding in place;
    /// this resolves it once, at compile time, to a register or a global slot.
    fn assign(&mut self, name: &'static str, value: &Expr) -> Result<(), String> {
        let save = self.st().next_reg;
        let before = self.here();
        let src = self.expr(value)?;
        self.st().next_reg = save;

        if let Some(l) = self.st().local(name) {
            let (reg, captured, boxed) = (l.reg, l.captured, l.boxed);
            if boxed {
                self.emit(Op::CellSet { cell: reg, src });
                return Ok(());
            }
            if captured {
                // A closure has already copied this value, so assigning it now would
                // leave that copy stale. The same shared cell that a captured `var`
                // would need fixes this too.
                return Err(format!(
                    "vm: `{}` is assigned after a closure captured it, which needs a shared cell",
                    name
                ));
            }
            if reg != src && !self.wrote_directly(before, src, reg, save) {
                self.emit(Op::Move { dst: reg, src });
            }
            return Ok(());
        }
        if let Some(idx) = self.tops.global(name) {
            self.emit(Op::StoreGlob { idx, src });
            return Ok(());
        }
        // The tree-walker's wording: an assignment to a name that does not exist is
        // almost always a missing `var`.
        Err(format!("Cannot assign to `{}`: it is not declared with `var`", name))
    }

    /// Make the instruction that produced `src` write to `dst` instead, if it is safe to.
    ///
    /// `total = total + i` compiled to a `BinInt` into a temporary and then a `Move` into
    /// `total` — and `Move` was the most executed opcode in the interpreter, a quarter of
    /// `matching` and `records` and a fifth of `loop`. The arithmetic can just as well
    /// land on the target.
    ///
    /// Only when the value compiled to a STRAIGHT LINE. In a run with no branch in it the
    /// last write to a register is the only one that reaches the end, so redirecting it is
    /// the whole story. With a branch it is not: `total = if c { 1 } else { 2 }` ends with
    /// one arm's write, and redirecting only that one would leave the other arm writing a
    /// register nobody reads any more. The operands are read before the destination is
    /// written, so `dst` may alias one of them.
    /// `temps` is `next_reg` from before the value was compiled: anything at or above it
    /// is a temporary this expression made, and anything below is a live local or an
    /// argument already in place. Only a temporary may be redirected — patching a local's
    /// write would leave the local unwritten.
    fn wrote_directly(&mut self, before: u32, src: Reg, dst: Reg, temps: Reg) -> bool {
        if src < temps {
            return false;
        }
        let st = self.st();
        let Some(last) = st.code.len().checked_sub(1).filter(|at| *at >= before as usize) else {
            return false;
        };
        if st.code[before as usize..].iter().any(branches) {
            return false;
        }
        match &mut st.code[last] {
            Op::LoadK { dst: d, .. }
            | Op::Move { dst: d, .. }
            | Op::LoadGlob { dst: d, .. }
            | Op::LoadCap { dst: d, .. }
            | Op::CellGet { dst: d, .. }
            | Op::LoadSelf { dst: d }
            | Op::Bin { dst: d, .. }
            | Op::BinInt { dst: d, .. }
            | Op::BinK { dst: d, .. }
            | Op::BinIntK { dst: d, .. }
            | Op::BinDispatch { dst: d, .. }
            | Op::MakeClosure { dst: d, .. }
            | Op::MakeList { dst: d, .. }
            | Op::MakeTuple { dst: d, .. }
            | Op::MakeTag { dst: d, .. }
            | Op::MakeRecord { dst: d, .. }
            | Op::UpdateRecord { dst: d, .. }
            | Op::GetField { dst: d, .. }
            | Op::GetOptField { dst: d, .. }
            | Op::GetIndex { dst: d, .. }
            | Op::GetPayload { dst: d, .. }
            | Op::GetRest { dst: d, .. }
            | Op::GetElem { dst: d, .. }
            | Op::GetSlice { dst: d, .. }
            | Op::MakeRange { dst: d, .. }
            | Op::CallBuiltin { dst: d, .. }
            | Op::CallHost { dst: d, .. }
            | Op::MakeBuiltin { dst: d, .. }
            | Op::Interp { dst: d, .. }
                if *d == src =>
            {
                *d = dst;
                true
            }
            // A call writes its `dst` AFTER the frame starting at `base` has been torn
            // down, so redirecting it onto a local is safe exactly when the local sits
            // BELOW that frame and the call cannot have scribbled on it. Arguments are
            // always allocated above every live local, so it always does — but the
            // overlap is what made these three unsafe to touch before, so it is checked
            // rather than argued. `p = step(p)` is this case, and it was a `Move` of a
            // whole record per iteration.
            Op::CallFn { dst: d, base: arg_base, .. }
            | Op::Call { dst: d, base: arg_base, .. }
            | Op::DispatchMethod { dst: d, base: arg_base, .. }
                if *d == src && dst < *arg_base =>
            {
                *d = dst;
                true
            }
            _ => false,
        }
    }

    /// `for x in iterable { body }` — one `IterNext` per iteration, and for a range
    /// no list is ever built.
    fn for_loop(&mut self, name: &'static str, iterable: &Expr, body: &Expr) -> Result<(), String> {
        let save = self.st().next_reg;
        let iter = self.expr(iterable)?;
        self.reserve(iter)?;
        // The position reached so far. A register, so the loop needs no state outside
        // the frame and nothing to allocate.
        let idx = self.alloc()?;
        self.constant(idx, Value::Int(0))?;
        let item = self.alloc()?;

        // The `IterNext` is both the top of the loop and the test that leaves it:
        // the body jumps back to it, and its own `to` points past the loop.
        let top = self.here();
        self.emit(Op::IterNext { dst: item, iter, idx, to: u32::MAX });

        self.st().locals.push(Local { name, reg: item, is_var: false, captured: false, boxed: false });
        self.st().loops.push(Vec::new());
        let body_base = self.st().next_reg;
        let result = self.discard(body);
        self.st().next_reg = body_base;
        let breaks = self.st().loops.pop().expect("pushed above");
        self.st().locals.pop();
        result?;

        self.emit(Op::Jump { to: top });
        self.patch_to_here(top);
        for at in breaks {
            self.patch_to_here(at);
        }
        self.st().next_reg = save;
        Ok(())
    }

    fn while_loop(&mut self, condition: &Expr, body: &Expr) -> Result<(), String> {
        let save = self.st().next_reg;
        let top = self.here();
        let cond = self.expr(condition)?;
        self.st().next_reg = save;
        let failed = self.here();
        self.emit(Op::JumpFalse { cond, to: u32::MAX, kind: CondKind::While });

        self.st().loops.push(Vec::new());
        let result = self.discard(body);
        self.st().next_reg = save;
        let breaks = self.st().loops.pop().expect("pushed above");
        result?;

        self.emit(Op::Jump { to: top });
        self.patch_to_here(failed);
        for at in breaks {
            self.patch_to_here(at);
        }
        Ok(())
    }

    fn literal(&mut self, value: Value) -> Result<Reg, String> {
        let dst = self.alloc()?;
        self.constant(dst, value)?;
        Ok(dst)
    }
}

/// Would compiling this lambda body in place change where control goes?
///
/// Conservative: a `return` or `break` inside a NESTED lambda would be its own, but
/// telling the two apart is not worth a mistake, and the fallback is only a call.
fn escapes(e: &Expr) -> bool {
    matches!(e, Expr::Return(..) | Expr::Break(_) | Expr::Assign { .. })
        || e.children().into_iter().any(escapes)
}

/// A direct call's arity is known at compile time, so a wrong one need not wait for
/// run time. The name makes it a better message than the tree-walker's was.
fn check_arity(name: &str, arity: u16, argc: u16) -> Result<(), String> {
    if arity == argc {
        return Ok(());
    }
    Err(format!("`{}` expects {} argument(s), got {}", name, arity, argc))
}

/// `Module.name`, as one `&'static str` the name tables and lookups can share.
///
/// Interned, like the identifiers the parser produces: this is asked for every
/// qualified name the compiler meets, and the same `Str.concat` should not leak a
/// fresh copy per call site.
fn qualify(module: &str, name: &str) -> &'static str {
    crate::memory::string_pool::intern(&format!("{}.{}", module, name))
}

fn func_name(func: &Expr) -> &'static str {
    match func {
        Expr::Ident(name, _) => name,
        _ => "a function",
    }
}


/// The method an operator is sugar for. Agrees with `eval::dispatch_operator`, which
/// makes the same mapping when the call actually runs.
/// A nominal's shape, following a nominal declared OVER another one by name:
/// `Set(item) :: Dict(item, {})` has `Dict`'s fields, which its own backing — a
/// placeholder with the arguments dropped — cannot say.
fn resolved_shape(nominals: &[(&'static str, crate::types::Type)], backing: &crate::types::Type) -> crate::vm::NominalShape {
    resolved_shape_depth(nominals, backing).0
}

/// The shape, and how many nominals were followed to reach it: `Set` over `Dict` is
/// one deeper than `Dict`, which is what makes it the more specific of two candidates
/// that fit the same value.
fn resolved_shape_depth(nominals: &[(&'static str, crate::types::Type)], backing: &crate::types::Type) -> (crate::vm::NominalShape, u8) {
    use crate::types::Type;
    let mut ty = backing;
    let mut depth = 0u8;
    for _ in 0..8 {
        // A placeholder — a nominal named but not declared here — is resolved by its
        // name, whether it stands alone or is what a declared nominal is built over.
        let placeholder = match ty {
            Type::Nominal { name, backing } if matches!(**backing, Type::TypeVar(_)) => Some(name),
            Type::Nominal { backing, .. } => match &**backing {
                Type::Nominal { name, backing: inner } if matches!(**inner, Type::TypeVar(_)) => Some(name),
                _ => None,
            },
            _ => None,
        };
        let Some(name) = placeholder else { return (crate::vm::shape_of(ty), depth) };
        let bare = name.rsplit('.').next().unwrap_or(name);
        match nominals.iter().find(|(n, _)| *n == bare) {
            Some((_, declared)) => {
                ty = declared;
                depth += 1;
            }
            None => break,
        }
    }
    (crate::vm::shape_of(ty), depth)
}

fn operator_method_name(op: crate::ast::BinOp) -> Option<&'static str> {
    use crate::ast::BinOp;
    Some(match op {
        BinOp::Add => "plus",
        BinOp::Sub => "minus",
        BinOp::Mul => "times",
        BinOp::Div => "div_by",
        BinOp::IntDiv => "div_trunc_by",
        BinOp::Rem => "rem_by",
        BinOp::Lt => "is_lt",
        BinOp::Gt => "is_gt",
        BinOp::Le => "is_lte",
        BinOp::Ge => "is_gte",
        BinOp::Eq | BinOp::Ne => "is_eq",
        _ => return None,
    })
}

/// A type as a runtime value, for the builtins that must read one.
///
/// Only the shape a JSON reader needs: a list of what, a nominal by name, and a
/// stopping point for everything else — where the reader falls back to reading the
/// document as it stands.
fn type_descriptor(ty: &crate::types::Type) -> Value {
    use crate::types::Type;
    match ty {
        Type::List(inner) => Value::tag("List", [type_descriptor(inner)]),
        // A record's FIELDS carry the shape down: without them a field holding a
        // nominal with its own `parser_for` was read as whatever the document said.
        Type::Record { fields, .. } => Value::tag(
            "Record",
            vec![Value::list(
                fields
                    .iter()
                    .map(|(name, field)| {
                        Value::tuple(vec![
                            crate::eval::str_value(*name),
                            type_descriptor(field),
                        ])
                    })
                    .collect(),
            )],
        ),
        // The second payload is what the nominal WRAPS — `Opt(Inner)`'s `Inner` — so a
        // `parser_for` that delegates through its type parameter has something to
        // delegate to.
        Type::Nominal { name, backing } => Value::tag(
            "Nominal",
            vec![crate::eval::str_value(*name), wrapped_descriptor(backing)],
        ),
        // `Try(a, e)` is how a parse result is written, and the `a` is what to read.
        Type::TagUnion { tags, .. } => match tags.iter().find(|(tag, _)| *tag == "Ok") {
            Some((_, payload)) if payload.len() == 1 => type_descriptor(&payload[0]),
            _ => Value::Unit,
        },
        _ => Value::Unit,
    }
}

/// The type a nominal wraps, when exactly one of its tags carries a single payload:
/// `Opt(a) := [None, Has(a)]` wraps `a`. Anything else has no single element.
fn wrapped_descriptor(backing: &crate::types::Type) -> Value {
    use crate::types::Type;
    let Type::TagUnion { tags, .. } = backing else { return Value::Unit };
    let mut carrying = tags.iter().filter(|(_, payload)| payload.len() == 1);
    match (carrying.next(), carrying.next()) {
        (Some((_, payload)), None) => type_descriptor(&payload[0]),
        _ => Value::Unit,
    }
}

/// Does a lambda inside `expr` mention `name` as a free variable? A `var` such a
/// lambda captures has to live in a shared cell (`Local::boxed`).
fn lambda_mentions(expr: &Expr, name: &str) -> bool {
    if matches!(expr, Expr::Lambda { .. }) && crate::parser::Parser::mentions_free(expr, name) {
        return true;
    }
    expr.children().into_iter().any(|child| lambda_mentions(child, name))
}
