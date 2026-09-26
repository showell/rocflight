//! Hand-written recursive descent, desugared source in and `ast::Expr` out.
//!
//! It also collects the side-tables later phases need and cannot recover from the tree
//! (nominals, `where` methods, literal suffixes, field defaults, imports, the entry
//! point), and it is where `?`, `??` and `.?` are expanded — each needs an expression's
//! extent, which the text-level desugarer cannot find. Learning.md §2 has the pipeline.

use crate::ast::{Expr, StrPart, MatchArm, Pattern};
use crate::types::Type;
use crate::error::ParseError;
use crate::memory::string_pool;

pub struct Parser {
    input: String,
    pos: usize,
    /// App entry point (e.g., "main!") if app declaration found
    entry_point: Option<String>,
    /// The registered source this parser's nodes belong to, when it was given a name.
    source: Option<usize>,
    /// Counter for type variables introduced by annotations (`a -> a`, and any type
    /// name the interpreter does not model).
    next_type_var: u32,
    /// Annotations read but not yet claimed by a binding of the same name.
    pending_annotations: Vec<(&'static str, Type)>,
    /// Nominal types declared with `Name := backing`, so an annotation naming one
    /// resolves to it rather than to an anonymous fresh variable.
    nominals: Vec<(&'static str, Type)>,
    /// Dependencies from the app header: `(alias, spec, is_platform)`.
    ///
    /// `app [main!] { cli: platform "URL", pkg: "URL", roc: "nightly-..." }` — the
    /// `roc:` entry pins the compiler and is not a fetchable dependency.
    dependencies: Vec<(String, String, bool)>,
    /// Modules brought in by `import`, as `(dependency alias, module)`.
    imports: Vec<(String, String)>,
    /// Files ingested by `import "path" as name : Str`, as `(name, path)`. The file's
    /// CONTENTS become the binding, so the caller reads it relative to the source.
    ingests: Vec<(String, String)>,
    /// Top-level `expect`s, held back until every declaration is bound.
    ///
    /// roc loads a module completely and only then runs its tests, so an `expect` may
    /// use a function declared below it. Running them in source order called functions
    /// that did not exist yet.
    deferred_expects: Vec<Expr>,
    /// Local modules brought in by `import Hello exposing [hello]`, as
    /// `(module path, exposed names)`. The path is relative to the importing file and
    /// names a `.roc` beside it — `Dir/Hello` is `Dir/Hello.roc`.
    local_modules: Vec<(String, Vec<String>)>,
    /// The method blocks open around the current position, innermost last.
    open_blocks: Vec<String>,
    /// Each nominal declared inside another's method block, with that owner:
    /// `Shape :: [].{ Box := [B(I64)].{ ... } }` gives `("Box", "Shape")`. See
    /// `enclosing_owners`.
    enclosing_owners: Vec<(String, String)>,
    /// How deep `parse_expr` is nested. Only the outermost call wraps the program in
    /// its nominal method bindings.
    expr_depth: u32,
    /// Members of a nominal's method block that carry an ANNOTATION BUT NO BODY, as
    /// `(Type.name, its declared type)`.
    ///
    /// In `Builtin.roc` that is precisely the set of intrinsics: the real compiler's
    /// `canonicalize/BuiltinLowLevel.zig` rewrites exactly these into lambdas running a
    /// `LowLevel` op, and rocflight has to supply each one from Rust. They fall out of
    /// the annotation bookkeeping for free — an annotation a binding claims is a
    /// definition, and whatever is left over when the block closes is an intrinsic.
    /// Names only: nothing reads an intrinsic's declared type, `signatures` does.
    intrinsics: Vec<&'static str>,
    /// EVERY method-block annotation, `Type.method` to its declared type, whether or
    /// not a body claimed it. This is the type table `Builtin.roc` is really for: the
    /// signatures are already written, module-qualified and arity-correct, so the
    /// checker has no business guessing them from a method name.
    signatures: Vec<(&'static str, Type)>,
    /// Record literals written as a NOMINAL construction, `Point.{ x: 3 }`, and the
    /// nominal's type.
    ///
    /// The AST erases the nominal — roc does too — so by the time the checker sees the
    /// literal it is a plain record and its fields have no declared types. That is what
    /// left `Point := { x: I64 }` unable to tell `3` it is an integer.
    nominal_literals: Vec<(crate::ast::NodeId, Type)>,
    /// Nominals declared with `::`, the OPAQUE form.
    opaque_nominals: Vec<&'static str>,
    /// How many blocks enclose the cursor. `?` is only meaningful inside one, because
    /// the match it expands to has to wrap the rest of the block.
    block_depth: u32,
    /// The next primary expression begins a block's STATEMENT (or its tail).
    ///
    /// roc reads `{ x }` as a record pun only there; as a binding's right-hand side, a
    /// call argument or a list element it is a block whose value is `x`. Verified
    /// against `roc check`: `y : { x : U64 }` accepts `y = { { x } }` and rejects
    /// `y = { x }`, `y = f({ x })` and `y = [{ x }]`.
    stmt_head: bool,
    /// Set while a pipe's target is parsed: a whitespace-separated `.postfix` after it
    /// belongs to the completed pipe (`2 |> bar() .blah()(3)`), not to the target.
    pipe_target: bool,
    /// Aliases with an EXTENSION parameter — `R(x) : { a : I64, ..x }`,
    /// `T(x) : [A, ..x]` — as `(alias, the parameter's variable id, is a record)`.
    /// Applying one is only legal with a matching kind and no duplicate member.
    extension_aliases: Vec<(String, u32, bool)>,
    /// The extension parameter of the alias currently being parsed, if it has one.
    pending_extension: Option<(u32, bool)>,
    /// Declarations roc refuses that only the type parser can see. `parse_type`'s
    /// errors are swallowed on purpose — an annotation is documentation here — so a
    /// real problem needs a channel of its own.
    type_problems: Vec<String>,
    /// `expr?` sites lifted out of the statement being parsed, as
    /// `(fresh name, the Try expression, an optional error mapper)`. Drained by
    /// `parse_block` once the statement's value is complete.
    pending_tries: Vec<(&'static str, Expr, Option<Expr>)>,
    /// Fresh-name counter for the `.?`-chain desugaring, so `o.?b.c` maps through Ok.
    opt_chain_counter: usize,
    /// Defaults collected while parsing the record type of the current declaration,
    /// as `(field, default expression)`. Moved into `nominal_defaults` when the
    /// declaration completes.
    field_defaults: Vec<(String, Expr)>,
    /// Optional field names collected the same way.
    optional_fields: Vec<String>,
    /// Per-nominal field defaults, so `Name.{ ... }` can fill the omitted ones.
    nominal_defaults: Vec<(String, Vec<(String, Expr)>)>,
    /// See `nominal_suffixes()`.
    nominal_suffixes: Vec<(crate::ast::NodeId, &'static str)>,
    /// Method names promised by a `where` clause, so the checker may dispatch them on
    /// a type variable that inference has not resolved.
    where_methods: Vec<String>,
    /// Type parameters of each parameterised nominal, as `(name, var ids in order)`.
    /// `Wrapper(a) := { item: a }` records the id that `a` was given, so `Wrapper(Str)`
    /// can substitute `Str` for it.
    nominal_params: Vec<(String, Vec<u32>)>,
    /// Types the file's imports declare, with their parameters: `Tup2(I64, I64)`
    /// written in another module is that module's `Tup2` with the arguments put
    /// in, as a local declaration's would be. See `declare_imported`.
    imported_types: Vec<(&'static str, Type)>,
    imported_params: Vec<(String, Vec<u32>)>,
    /// Names introduced by `var`, which are reassignable.
    ///
    /// Flat rather than scoped: a `var x` in one function also makes a later `x = e`
    /// in another read as a reassignment.
    // ponytail: flat set, make it scoped if a test ever shadows a sibling's `var` name.
    mutable_names: Vec<String>,
    /// Methods from nominal `.{ ... }` blocks, as `("Type.method", body)`.
    ///
    /// Wrapped around the program so they are ordinary bindings: `Secret.reveal` is
    /// then a plain lookup, and `s.reveal()` a dispatch that finds it.
    methods: Vec<(&'static str, Option<Type>, Expr)>,
    /// Methods of a nominal declared INSIDE a block, with the block depth they belong
    /// to. They stay where they were written rather than being hoisted, because a
    /// block-local nominal's method may capture the enclosing scope —
    /// `make = |offset| { Local := [...].{ get = |Local(n)| n + offset } … }` — and a
    /// top-level chunk has nothing to capture from.
    local_methods: Vec<(u32, &'static str, Option<Type>, Expr)>,
    /// Type variables seen so far in the annotation being parsed.
    ///
    /// A repeated name must mean the SAME variable: `pair : a, a -> a` constrains both
    /// parameters to one type, and without this map each `a` became a separate fresh
    /// variable, so `pair(1, "s")` was wrongly accepted.
    annotation_vars: Vec<(String, u32)>,
}

impl Parser {
    /// A parser whose nodes know which file they came from, so a runtime error can
    /// say where.
    ///
    /// `new` leaves that out, which is what the tests and the nested parse of a string
    /// interpolation want: without a registered source a node simply has no location,
    /// and an error reads exactly as it did before.
    pub fn named(file: &str, input: &str) -> Self {
        let mut parser = Parser::new(input);
        parser.source = Some(crate::ast::open_source(crate::ast::next_node_id(), file, input));
        parser
    }

    pub fn new(input: &str) -> Self {
        Parser {
            input: input.to_string(),
            pos: 0,
            next_type_var: 0,
            pending_annotations: Vec::new(),
            nominals: Vec::new(),
            annotation_vars: Vec::new(),
            dependencies: Vec::new(),
            imports: Vec::new(),
            ingests: Vec::new(),
            local_modules: Vec::new(),
            open_blocks: Vec::new(),
            enclosing_owners: Vec::new(),
            deferred_expects: Vec::new(),
            field_defaults: Vec::new(),
            optional_fields: Vec::new(),
            nominal_defaults: Vec::new(),
            nominal_suffixes: Vec::new(),
            where_methods: Vec::new(),
            nominal_params: Vec::new(),
            imported_types: Vec::new(),
            imported_params: Vec::new(),
            mutable_names: Vec::new(),
            expr_depth: 0,
            block_depth: 0,
            stmt_head: false,
            pipe_target: false,
            extension_aliases: Vec::new(),
            pending_extension: None,
            type_problems: Vec::new(),
            intrinsics: Vec::new(),
            signatures: Vec::new(),
            nominal_literals: Vec::new(),
            opaque_nominals: Vec::new(),
            pending_tries: Vec::new(),
            opt_chain_counter: 0,
            methods: Vec::new(),
            local_methods: Vec::new(),
            entry_point: None,
            source: None,
        }
    }

    pub fn app_entry_point(&self) -> Option<String> {
        self.entry_point.clone()
    }

    /// A nominal suffix on a literal PATTERN — `123.MyNum =>` — is dropped: the
    /// scrutinee's type already says which conversion the literal goes through.
    fn skip_type_suffix(&mut self) {
        let rest = &self.input[self.pos..];
        if rest.starts_with('.') && rest[1..].starts_with(char::is_uppercase) {
            let len = rest[1..].chars().take_while(|c| is_ident_char(*c)).count();
            self.pos += 1 + len;
        }
    }

    /// A fresh node id, remembering where the parser currently is. Composite nodes
    /// should prefer `crate::ast::fresh_node_like(child)`, which takes the construct's
    /// START from its first child rather than its end from here.
    fn node(&self) -> crate::ast::NodeId {
        crate::ast::fresh_node(self.pos)
    }

    pub fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        let parsed = self.parse_expr_outer();
        // Closing the range bounds this file's nodes, so a node made LATER by an
        // enclosing parse does not resolve to this file.
        if let Some(handle) = self.source {
            crate::ast::close_source(handle);
        }
        parsed
    }

    fn parse_expr_outer(&mut self) -> Result<Expr, ParseError> {
        // Counted from the very start: a nominal's method block is parsed by
        // `skip_trivia` in the loop below, so a lambda inside it re-enters this
        // function before the main expression is reached.
        self.expr_depth += 1;
        let outcome = self.parse_expr_inner();
        self.expr_depth -= 1;
        let mut program = outcome?;

        // Only the outermost call brings nominal methods into scope; wrapping at every
        // level nested them inside the first method's own body.
        if self.expr_depth == 0 {
            // Whatever annotation is still pending at the end of the file was never
            // claimed by a binding, so it declares a type and no body. At the top level
            // of `Builtin.roc` those are the low-level ops — `list_get_unsafe`,
            // `hasher_finish`, `dict_seed` — which the real compiler injects and which
            // Rust has to supply here. `claim_intrinsics` with an empty prefix keeps
            // their bare names.
            self.claim_intrinsics("", 0);
            for (name, annotation, value) in
                std::mem::take(&mut self.methods).into_iter().rev()
            {
                program = Expr::Let { id: self.node(),
                    name,
                    annotation,
                    value: Box::new(value),
                    body: Box::new(program),
                };
            }
            // The tests go last, after every declaration is in scope.
            //
            // `parse_top_level` defers the ones it parses at the outermost level, but
            // not those inside a `_ = <chain>` — the shape this parser produces for a
            // file whose declarations are followed by expects — because it is not at
            // that level when it meets them. `compile_unit` reorders them anyway when
            // it flattens, so the VM was already running them last; the CHECKER walks
            // the tree as written, and saw `graph.dfs(…)` before `graph` was bound.
            let mut expects = std::mem::take(&mut self.deferred_expects);
            Self::lift_expects(&mut program, &mut expects);
            if !expects.is_empty() {
                Self::append_to_body(&mut program, expects);
            }
        }
        Ok(program)
    }

    /// Sequence `extra` at the innermost point of a `Let` spine, before its value.
    ///
    /// The file's value — usually `main!` — stays last, so what the program evaluates
    /// to is unchanged.
    /// Take every `expect` statement out of the top-level spine, in order.
    ///
    /// The spine is the chain of `Let`s the file's declarations build, and a statement
    /// in it is a binding to `_`. A `_` bound to another chain is the flattened group
    /// `compile_unit` also looks through, so this descends into one.
    fn lift_expects(program: &mut Expr, out: &mut Vec<Expr>) {
        // A loop down the spine, recursing only into a nested group: a file of ten
        // thousand declarations must not cost ten thousand Rust frames here.
        let mut cursor = program;
        loop {
            // Take the expects off the front of this spine, then walk what is left.
            while matches!(cursor, Expr::Let { name: "_", value, .. } if matches!(**value, Expr::Expect(..)))
            {
                let Expr::Let { value, body, .. } =
                    std::mem::replace(cursor, Expr::Unit(crate::ast::fresh_node_unlocated()))
                else {
                    unreachable!("matched a Let")
                };
                out.push(*value);
                *cursor = *body;
            }
            // A `_` bound to another chain is the flattened group `compile_unit` also
            // looks through. Decided before the mutable match: a guard that binds
            // fields mutably is a second borrow the checker refuses.
            let nested = matches!(&*cursor, Expr::Let { name: "_", value, .. } if matches!(**value, Expr::Let { .. }));
            match cursor {
                Expr::Let { value, body, .. } => {
                    if nested {
                        Self::lift_expects(value, out);
                    }
                    cursor = body;
                }
                // A chain can END in an expect rather than binding one to `_`, which
                // is the shape a group of declarations followed by a test produces.
                Expr::Expect(..) => {
                    let lifted =
                        std::mem::replace(cursor, Expr::Unit(crate::ast::fresh_node_unlocated()));
                    out.push(lifted);
                    return;
                }
                _ => return,
            }
        }
    }

    fn append_to_body(program: &mut Expr, extra: Vec<Expr>) {
        let mut cursor = program;
        while let Expr::Let { body, .. } = cursor {
            cursor = body;
        }
        let tail = std::mem::replace(cursor, Expr::Unit(crate::ast::fresh_node_unlocated()));
        let mut rebuilt = tail;
        for value in extra.into_iter().rev() {
            rebuilt = Expr::Let { id: crate::ast::fresh_node_unlocated(),
                name: "_",
                annotation: None,
                value: Box::new(value),
                body: Box::new(rebuilt),
            };
        }
        *cursor = rebuilt;
    }

    /// The body of `parse_expr`, without the method-block wrapping.
    fn parse_expr_inner(&mut self) -> Result<Expr, ParseError> {
        self.skip_whitespace();

        // Skip app and import declarations at the top level
        loop {
            let rest = &self.input[self.pos..];

            if rest.starts_with("app ") {
                // Extract entry point from app declaration: app [entry!] { ... }
                if !self.extract_app_entry_point() {
                    self.skip_to_next_declaration();
                }
                self.skip_whitespace();
            } else if rest.starts_with("import ") {
                self.record_import();
                self.skip_to_line_end();
                self.skip_whitespace();
            } else {
                let before = self.pos;
                self.skip_trivia();
                if self.pos == before {
                    break;
                }
            }
        }

        // A platformless app may omit the header entirely: `main! = |_args| ...`
        // implies `app [main!] {}`. Verified against `roc run` on
        // nightly-2026-09-03, and against roc-compiler/test/echo/hello.roc.
        //
        // Only the OUTERMOST parse asks: the scan reads every line of the file, and
        // every braceless lambda body re-enters here, so asking at each depth made
        // parsing a member of Builtin.roc quadratic — 11ms of the low-level section's
        // 13ms, and a third of a second for `Num`.
        if self.expr_depth == 1
            && self.entry_point.is_none()
            && self.has_top_level_binding("main!")
        {
            self.entry_point = Some("main!".to_string());
        }

        // Parse the main expression.
        self.parse_top_level()
    }

    /// Parse a type expression, e.g. `List(Str) => Try({}, [Exit(I8), ..])`.
    ///
    /// Grammar, in decreasing precedence:
    /// ```text
    /// type  := atom (',' atom)* ('->' | '=>') type    -- function
    ///        | atom
    /// atom  := Name ['(' type (',' type)* ')']        -- I64, List(T), Try(a, b)
    ///        | name                                   -- lowercase: a type variable
    ///        | '{' [field (',' field)*] '}'           -- record, {} is unit
    ///        | '(' type (',' type)* ')'               -- 1 elem groups, 2+ is a tuple
    ///        | '[' [tag (',' tag)*] [',' '..'] ']'    -- tag union, `..` means open
    /// ```
    ///
    /// `=>` (effectful) types the same as `->` here: the interpreter does not model
    /// effects, only their signatures.
    ///
    /// Parenthesisation is load-bearing in the argument list: `A, B -> C` takes two
    /// parameters, while `(A, B) -> C` takes one tuple.
    fn parse_type(&mut self) -> Result<Type, ParseError> {
        let mut params = vec![self.parse_type_atom()?];

        loop {
            self.skip_inline_whitespace();
            if !self.input[self.pos..].starts_with(',') {
                break;
            }
            // A comma here separates parameters, but it could also belong to an
            // enclosing list; only commit if another atom follows.
            let saved = self.pos;
            self.pos += 1;
            // Across the newline: a parameter list may be written one per line, which
            // is how Builtin.roc writes the big record-shaped ones. Committing is safe
            // because a failed atom rewinds to `saved`.
            self.skip_whitespace();
            match self.parse_type_atom() {
                Ok(atom) => params.push(atom),
                Err(_) => {
                    self.pos = saved;
                    break;
                }
            }
        }

        self.skip_inline_whitespace();
        let rest = &self.input[self.pos..];
        let is_arrow = rest.starts_with("->") || rest.starts_with("=>");
        if !is_arrow {
            if params.len() == 1 {
                return Ok(params.pop().expect("checked length"));
            }
            // A bare comma list with no arrow is not a type on its own.
            return Err(ParseError {
                message: "Expected '->' or '=>' after a type parameter list".to_string(),
                position: self.pos,
            });
        }
        // Past here an arrow follows, so the comma list really was parameters.
        self.pos += 2;
        self.skip_inline_whitespace();

        // Curried, matching how lambdas and calls are typed: `A, B -> C` becomes
        // `A -> (B -> C)`.
        let mut result = self.parse_type()?;
        for param in params.into_iter().rev() {
            result = Type::Function(Box::new(param), Box::new(result));
        }
        Ok(result)
    }

    /// Parse a type that does NOT treat a top-level comma as a parameter separator.
    ///
    /// Used everywhere a comma already means something else: record field values,
    /// tuple elements, tag payloads and applied-type arguments. Without this level,
    /// `{ x: Bool, y: Bool }` parses the first field's value as the parameter list
    /// `Bool, y` and then fails looking for an arrow.
    fn parse_type_operand(&mut self) -> Result<Type, ParseError> {
        let atom = self.parse_type_atom()?;
        self.skip_inline_whitespace();
        let rest = &self.input[self.pos..];
        if rest.starts_with("->") || rest.starts_with("=>") {
            self.pos += 2;
            let result = self.parse_type_operand()?;
            return Ok(Type::Function(Box::new(atom), Box::new(result)));
        }

        // A MULTI-parameter function still reads as one operand when an arrow follows
        // the comma list: `{ next_payload : _state, U64, U64 -> Try(…) }` is one field
        // whose type takes three parameters, not three fields. The commas are only
        // parameters if an arrow turns up, so this collects them speculatively and
        // rewinds when it does not — which is what leaves an ordinary
        // `{ x: Bool, y: Bool }` alone.
        let saved = self.pos;
        let mut params = vec![atom];
        while self.input[self.pos..].starts_with(',') {
            self.pos += 1;
            self.skip_whitespace();
            match self.parse_type_atom() {
                Ok(next) => params.push(next),
                Err(_) => break,
            }
            self.skip_inline_whitespace();
        }
        let rest = &self.input[self.pos..];
        if params.len() > 1 && (rest.starts_with("->") || rest.starts_with("=>")) {
            self.pos += 2;
            self.skip_inline_whitespace();
            let mut result = self.parse_type_operand()?;
            for param in params.into_iter().rev() {
                result = Type::Function(Box::new(param), Box::new(result));
            }
            return Ok(result);
        }
        self.pos = saved;
        Ok(params.into_iter().next().expect("the atom is always there"))
    }

    /// Parse one type atom. See `parse_type` for the grammar.
    fn parse_type_atom(&mut self) -> Result<Type, ParseError> {
        self.skip_inline_whitespace();
        let rest = &self.input[self.pos..];

        if rest.starts_with('{') {
            return self.parse_record_type();
        }
        if rest.starts_with('[') {
            return self.parse_tag_union_type();
        }
        if rest.starts_with('(') {
            self.pos += 1;
            // `()` is Roc's EMPTY PARAMETER LIST, which is how Builtin.roc spells a
            // function that takes nothing: `step! : () => [Done]`, `empty : () ->
            // Dict(k, v)`. It is also the unit type, and `{}` is the same type, so
            // both land on `Type::Unit`. Without this the tuple branch below asks for
            // a type atom, finds `)`, and fails the whole annotation.
            self.skip_inline_whitespace();
            if self.input[self.pos..].starts_with(')') {
                self.pos += 1;
                return Ok(Type::Unit);
            }
            let mut items = vec![self.parse_type_operand()?];
            loop {
                self.skip_inline_whitespace();
                if !self.input[self.pos..].starts_with(',') {
                    break;
                }
                self.pos += 1;
                items.push(self.parse_type_operand()?);
            }
            self.skip_inline_whitespace();
            if !self.input[self.pos..].starts_with(')') {
                return Err(ParseError {
                    message: "Expected ')' in type".to_string(),
                    position: self.pos,
                });
            }
            self.pos += 1;
            // One element is grouping, like `(I64 -> I64)`; more is a tuple.
            return Ok(if items.len() == 1 {
                items.pop().expect("checked length")
            } else {
                Type::Tuple(items)
            });
        }

        // A QUALIFIED type name — `Dict.DictBucket`, `Str.Utf8Problem` — is read here
        // rather than through `parse_identifier`, which sees an uppercase dotted path
        // as a nominal-qualified TAG (`Animal.Dog`) and hands back something this match
        // then rejected, silently dropping the whole annotation.
        //
        // The last segment is the name: nested nominals are registered flat, so
        // `Str :: […].{ Utf8Problem := […] }` puts `Utf8Problem` in scope, not
        // `Str.Utf8Problem`.
        // A WINDOW, not a `String`. Every type atom took this path and allocated its
        // own name on the heap just to ask whether it contains a dot — and `Builtin.roc`
        // is a file of annotations, each with several atoms in it. Nothing below needs
        // it owned: `contains`, `starts_with`, `ends_with`, `rsplit` and `len` all read
        // a borrowed slice.
        let end = rest
            .char_indices()
            .find(|(_, c)| !(c.is_alphanumeric() || *c == '_' || *c == '.'))
            .map_or(rest.len(), |(i, _)| i);
        let qualified: &str = &rest[..end];
        // `Kinds.Thing` written in a module that is itself `Thing :: [].{ .. }`: the
        // qualifier names no type of this file, so the name is the IMPORT's, and the
        // file's own namespace of the same last segment must not answer for it.
        let foreign = qualified.contains('.')
            && self.nominal(qualified.split('.').next().expect("split yields one part")).is_none();
        let name = if qualified.contains('.')
            && qualified.starts_with(char::is_uppercase)
            && !qualified.ends_with('.')
        {
            self.pos += qualified.len();
            let last = qualified.rsplit('.').next().expect("split yields one part");
            if !last.starts_with(char::is_uppercase) {
                return Err(ParseError {
                    message: format!("Expected a type name, got {}", qualified),
                    position: self.pos,
                });
            }
            &*Box::leak(last.to_string().into_boxed_str())
        } else {
            let (remaining, ident) = parse_identifier(rest).map_err(|_| ParseError {
                message: "Expected a type".to_string(),
                position: self.pos,
            })?;
            self.pos += rest.len() - remaining.len();
            match ident {
                Expr::Ident(n, _) => n,
                other => {
                    return Err(ParseError {
                        message: format!("Expected a type name, got {}", other),
                        position: self.pos,
                    })
                }
            }
        };

        // A lowercase name is a type variable (`a -> a`), universally quantified.
        // The same name within one annotation is the same variable.
        if !name.starts_with(|c: char| c.is_uppercase()) {
            return Ok(Type::TypeVar(self.annotation_var(name)));
        }

        // Applied type: `List(I64)`, `Try(a, b)`.
        let mut args = Vec::new();
        if self.input[self.pos..].starts_with('(') {
            self.pos += 1;
            loop {
                self.skip_inline_whitespace();
                if self.input[self.pos..].starts_with(')') {
                    self.pos += 1;
                    break;
                }
                args.push(self.parse_type_operand()?);
                self.skip_inline_whitespace();
                let rest = &self.input[self.pos..];
                if rest.starts_with(',') {
                    self.pos += 1;
                    // A trailing comma before `)` is legal here too.
                    self.skip_whitespace();
                    if self.input[self.pos..].starts_with(')') {
                        self.pos += 1;
                        break;
                    }
                } else if rest.starts_with(')') {
                    self.pos += 1;
                    break;
                } else {
                    return Err(ParseError {
                        message: format!("Expected ',' or ')' in {}(...)", name),
                        position: self.pos,
                    });
                }
            }
        }

        // A BUILTIN name keeps its builtin meaning even where the file declares it.
        // `Builtin.roc` writes `Str :: [ProvidedByCompiler]` and
        // `List(_item) :: [ProvidedByCompiler]`, and reading `Str` through those makes
        // it a nominal over an opaque tag and throws a list's element type away — so
        // every signature the file declares would arrive less precise than the types
        // rocflight already has.
        //
        // `Iter` is the exception: rocflight maps `Iter(a)` to `List(a)` for the
        // builtin iterator, but a program may declare its OWN `Iter` record type, and
        // then that record — with its `next` field and method block — is what the
        // annotation means, not a list.
        if !(name == "Iter" && self.nominal("Iter").is_some()) {
            if let Some(builtin) = builtin_type(name, &mut args, || Type::TypeVar(u32::MAX)) {
                return Ok(builtin);
            }
        }

        // A declared nominal wins over the fallback: `Point` is the nominal, not an
        // anonymous variable.
        let declared = if foreign {
            self.imported_type(name).or_else(|| self.nominal(name))
        } else {
            self.nominal(name).or_else(|| self.imported_type(name))
        };
        if let Some(nominal) = declared {
            // A nominal named but not yet declared here — the recursive `ConsList(a)`
            // inside `ConsList`'s own body, or an imported name — stands in with a
            // variable for its backing. Each OCCURRENCE gets its own: the shared
            // sentinel is one variable, so the first unification that bound it made
            // every other placeholder in the program mean that same type. Nested
            // `ConsList(ConsList(I64))` is where that showed: the inner list's union
            // became the outer's tail.
            if args.is_empty() {
                return Ok(nominal);
            }
            // `Wrapper(Str)` — put the arguments in place of the declared parameters.
            let found = if foreign {
                self.imported_params.iter().chain(self.nominal_params.iter()).find(|(n, _)| n == name)
            } else {
                self.nominal_params.iter().chain(self.imported_params.iter()).find(|(n, _)| n == name)
            };
            if let Some((_, params)) = found {
                let pairs: Vec<(u32, Type)> =
                    params.iter().copied().zip(args.iter().cloned()).collect();
                self.check_extension(name, &nominal, &pairs);

                // The recursive reference inside the body — the `ConsList(a)` of
                // `ConsList(a) := [Nil, Cons(a, ConsList(a))]` — is a placeholder,
                // with no backing to put the arguments in, so it keeps them: the
                // checker's `expand` puts them in place of the declaration's
                // parameters. Kept on a declared nominal too, as written -- but only
                // on the nominal NAMED: an alias's body (`Swap(a, b) : P(b, a)`)
                // already carries its own arguments, substituted.
                return Ok(match substitute_type_vars(&nominal, &pairs) {
                    Type::Nominal { name: n, backing, args: own } if own.is_empty() && same_name(n, name) => {
                        Type::Nominal { name: n, backing, args }
                    }
                    other => other,
                });
            }
        }

        Ok(named_type(name, args, || Type::TypeVar(self.next_type_var_unchecked())))
    }

    /// Parse `{ x: I64, y: Str }`, or `{}` for unit.
    ///
    /// A record type may span LINES and carry comments between its fields, which is how
    /// roc writes anything wider than a couple of fields. The braces bound it, so
    /// crossing a newline here cannot run into the next declaration.
    fn parse_record_type(&mut self) -> Result<Type, ParseError> {
        self.pos += 1; // Skip '{'
        let mut fields: Vec<(&'static str, Type)> = Vec::new();
        let mut open = false;

        loop {
            self.skip_whitespace();
            let rest = &self.input[self.pos..];
            if rest.starts_with('}') {
                self.pos += 1;
                break;
            }
            // `{ name: Str, .. }` or `{ name: Str, ..r }` — either spelling opens the
            // record. The name in `..r` ties two positions to the same leftovers,
            // which nothing here needs: an open record is open either way.
            if rest.starts_with("..") {
                open = true;
                self.pos += 2;
                let rest = &self.input[self.pos..];
                if let Ok((remaining, ident)) = parse_identifier(rest) {
                    self.pos += rest.len() - remaining.len();
                    // `..x` names the EXTENSION: whatever is substituted for `x` has to
                    // be a record too, and may not repeat a field written here.
                    if let Expr::Ident(name, _) = ident {
                        if name.starts_with(|c: char| c.is_lowercase()) {
                            let id = self.annotation_var(name);
                            self.pending_extension = Some((id, true));
                        }
                    }
                }
                continue;
            }
            // `_ : U8` is padding: a slot with no name and no value.
            if rest.starts_with('_') && !rest[1..].starts_with(is_ident_char) {
                self.pos += 1;
                self.skip_whitespace();
                if self.input[self.pos..].starts_with(':') {
                    self.pos += 1;
                }
                let _ = self.parse_type_operand()?;
                self.skip_whitespace();
                if self.input[self.pos..].starts_with(',') {
                    self.pos += 1;
                }
                continue;
            }
            let (remaining, ident) = parse_identifier(rest)?;
            self.pos += rest.len() - remaining.len();
            let field = match ident {
                Expr::Ident(n, _) => n,
                other => {
                    return Err(ParseError {
                        message: format!("Expected a field name in a record type, got {}", other),
                        position: self.pos,
                    })
                }
            };
            self.skip_whitespace();

            // `name ?: Type` declares an OPTIONAL field: it may be absent, and is read
            // with `.?name`, which yields a Try. Only allowed on a nominal's backing
            // record.
            let optional = self.input[self.pos..].starts_with("?:");
            if optional {
                self.pos += 2;
            } else if self.input[self.pos..].starts_with(':') {
                self.pos += 1;
            } else {
                return Err(ParseError {
                    message: format!("Expected ':' after record field '{}'", field),
                    position: self.pos,
                });
            }

            let field_type = self.parse_type_operand()?;
            self.skip_whitespace();

            // `name : Type ?? default` declares a DEFAULTED field: omitting it at
            // construction substitutes the default, so it is always present when read
            // and needs no unwrapping.
            if self.input[self.pos..].starts_with("??") {
                self.pos += 2;
                self.skip_whitespace();
                let default = self.parse_or_expr()?;
                self.field_defaults.push((field.to_string(), default));
            }
            if optional {
                self.optional_fields.push(field.to_string());
                fields.push((field, Type::Optional(Box::new(field_type))));
            } else {
                fields.push((field, field_type));
            }

            self.skip_whitespace();
            let rest = &self.input[self.pos..];
            if rest.starts_with(',') {
                self.pos += 1;
            } else if !rest.starts_with('}') {
                return Err(ParseError {
                    message: "Expected ',' or '}' in a record type".to_string(),
                    position: self.pos,
                });
            }
        }

        // `{}` is unit, not an empty record type. `{ .. }` is a record of anything.
        if fields.is_empty() && !open {
            return Ok(Type::Unit);
        }
        // Sorted, so field order in the annotation does not affect unification.
        fields.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(Type::Record { fields, open })
    }

    /// Parse `[Red, Green]`, `[Foo(I64, Str), Bar]`, `[Exit(I8), ..]`.
    ///
    /// May span LINES — roc writes a union of more than a few tags one per line. The
    /// brackets bound it, so crossing a newline cannot run into the next declaration.
    ///
    /// A trailing `..` marks the union OPEN: more tags may be added, and a `match` on
    /// it needs a wildcard. Without it the union is closed.
    fn parse_tag_union_type(&mut self) -> Result<Type, ParseError> {
        self.pos += 1; // Skip '['
        let mut tags: Vec<(&'static str, Vec<Type>)> = Vec::new();
        let mut open = false;
        // A NAMED extension, `..others`, is the union's row: every `..others` in the
        // signature is the same one.
        let mut row = None;

        loop {
            self.skip_whitespace();
            let rest = &self.input[self.pos..];
            if rest.starts_with(']') {
                self.pos += 1;
                break;
            }
            if rest.starts_with("..") {
                self.pos += 2;
                open = true;
                self.skip_whitespace();
                if self.input[self.pos..].starts_with(']') {
                    self.pos += 1;
                    break;
                }
                // `..x` names the EXTENSION, and is a type VARIABLE rather than
                // another tag: consume it, or it was read as a tag called `x`.
                let rest = &self.input[self.pos..];
                if rest.starts_with(char::is_lowercase) {
                    if let Ok((remaining, Expr::Ident(name, _))) = parse_identifier(rest) {
                        self.pos += rest.len() - remaining.len();
                        let id = self.annotation_var(name);
                        self.pending_extension = Some((id, false));
                        row = Some(id);
                        self.skip_whitespace();
                        if self.input[self.pos..].starts_with(',') {
                            self.pos += 1;
                        }
                    }
                }
                continue;
            }

            let (remaining, ident) = parse_identifier(rest)?;
            self.pos += rest.len() - remaining.len();
            let tag = match ident {
                Expr::Ident(n, _) => n,
                other => {
                    return Err(ParseError {
                        message: format!("Expected a tag name, got {}", other),
                        position: self.pos,
                    })
                }
            };

            let mut payload = Vec::new();
            if self.input[self.pos..].starts_with('(') {
                self.pos += 1;
                loop {
                    self.skip_whitespace();
                    if self.input[self.pos..].starts_with(')') {
                        self.pos += 1;
                        break;
                    }
                    payload.push(self.parse_type_operand()?);
                    self.skip_whitespace();
                    let rest = &self.input[self.pos..];
                    if rest.starts_with(',') {
                        self.pos += 1;
                    } else if rest.starts_with(')') {
                        self.pos += 1;
                        break;
                    } else {
                        return Err(ParseError {
                            message: format!("Expected ',' or ')' in tag {}", tag),
                            position: self.pos,
                        });
                    }
                }
            }
            tags.push((tag, payload));

            self.skip_whitespace();
            let rest = &self.input[self.pos..];
            if rest.starts_with(',') {
                self.pos += 1;
            } else if !rest.starts_with(']') {
                return Err(ParseError {
                    message: "Expected ',' or ']' in a tag union type".to_string(),
                    position: self.pos,
                });
            }
        }

        tags.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(Type::TagUnion { tags, open, row })
    }

    /// Skip spaces and tabs but NOT newlines.
    ///
    /// An annotation ends at its line break, so a type parser that skipped newlines
    /// would run on into the binding below it.
    fn skip_inline_whitespace(&mut self) {
        while let Some(c) = self.input[self.pos..].chars().next() {
            if c == ' ' || c == '\t' {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
    }

    /// The variable id for a named type variable in the annotation being parsed.
    ///
    /// Reused for a repeated name, so `a, a -> a` ties all three positions together.
    fn annotation_var(&mut self, name: &str) -> u32 {
        if let Some((_, id)) = self.annotation_vars.iter().find(|(n, _)| n == name) {
            return *id;
        }
        let id = self.fresh_type_var();
        self.annotation_vars.push((name.to_string(), id));
        id
    }

    /// Allocate a type variable id for an annotation's generic parameter.
    fn fresh_type_var(&mut self) -> u32 {
        self.next_type_var += 1;
        self.next_type_var
    }

    /// Same, for use where `&mut self` is already borrowed.
    fn next_type_var_unchecked(&mut self) -> u32 {
        self.fresh_type_var()
    }

    /// Skip whitespace, comments and type-annotation lines.
    ///
    /// Called wherever a declaration may appear: the top-level header loop and the
    /// top-level binding chain both need it, and a missed annotation there silently
    /// truncates the chain (the binding after it never gets parsed).
    /// Whitespace, comments, nominal declarations and standalone type annotations.
    ///
    /// The two captures below run between EVERY token, and each answers by scanning to
    /// the end of the line and searching it for `:=`, `::` or `:`. Guarding them on
    /// "the cursor is at the start of a line" was tried, for -4.2% on a `Dict` program,
    /// -3.2% on `strings` and -2.7% on a 2,000-declaration file — and it reads **1881
    /// of 1953**. Both constructs read to the next `\n`, so the guard looked safe and is
    /// not: annotations reach here mid-line often enough to break 72 eval tests, and
    /// `tests/check_roc.sh` and `tests/check_examples.sh` both stayed green while they
    /// did. Anything cheaper than this has to understand WHICH mid-line positions
    /// matter first.
    fn skip_trivia(&mut self) {
        loop {
            self.skip_whitespace();
            let rest = &self.input[self.pos..];
            if rest.starts_with('#') {
                self.skip_to_line_end();
                continue;
            }
            if self.capture_nominal_declaration() {
                continue;
            }
            if self.skip_type_annotation() {
                continue;
            }
            break;
        }
    }

    /// Skip a standalone type annotation line such as `main! : List(Str) => Try(...)`.
    ///
    /// Annotations survive desugaring so the emitted .roc stays valid Roc, but the
    /// interpreter infers types itself, so they are not parsed. Returns whether one
    /// was consumed.
    fn skip_type_annotation(&mut self) -> bool {
        self.capture_type_annotation()
    }

    /// Add a nominal's defaulted fields to a construction that omitted them.
    ///
    /// `Cfg.{ host: "a" }` for `Cfg := { host: Str, port: U16 ?? 8080 }` becomes
    /// `{ host: "a", port: 8080 }`. Fields the construction supplied are left alone.
    fn fill_defaults(&self, type_name: &str, built: Expr) -> Expr {
        let Some((_, defaults)) = self.nominal_defaults.iter().find(|(n, _)| n == type_name)
        else {
            return built;
        };
        let Expr::Record(mut fields, _) = built else {
            // `Name.{}` parses as unit; a nominal with defaults still gets them.
            if matches!(built, Expr::Unit(_)) {
                return Expr::Record(
                    defaults
                        .iter()
                        .map(|(f, d)| (leak_field(f), d.clone()))
                        .collect(),
                    self.node(),
                );
            }
            return built;
        };

        for (field, default) in defaults {
            if !fields.iter().any(|(name, _)| name == field) {
                fields.push((leak_field(field), default.clone()));
            }
        }
        Expr::Record(fields, self.node())
    }

    /// Finish a tag expression whose name has been consumed, reading any payload.
    ///
    /// Shared by bare tags (`Ok(x)`) and nominal-qualified ones (`Animal.Dog(x)`),
    /// which build the same value.
    fn finish_tag(&mut self, name: &'static str) -> Result<Expr, ParseError> {
        let mut args = Vec::new();
        if self.input[self.pos..].starts_with('(') {
            self.pos += 1; // Skip '('
            self.skip_whitespace();
            if self.input[self.pos..].starts_with(')') {
                self.pos += 1;
            } else {
                loop {
                    args.push(self.parse_or_expr()?);
                    self.skip_whitespace();
                    let rest = &self.input[self.pos..];
                    if rest.starts_with(',') {
                        self.pos += 1;
                        self.skip_whitespace();
                        // A trailing comma before `)` is legal, as it is in a call.
                        // Builtin.roc writes multi-line tag payloads that way.
                        if self.input[self.pos..].starts_with(')') {
                            self.pos += 1;
                            break;
                        }
                    } else if rest.starts_with(')') {
                        self.pos += 1;
                        break;
                    } else {
                        return Err(ParseError {
                            message: "Expected ',' or ')' in tag arguments".to_string(),
                            position: self.pos,
                        });
                    }
                }
            }
        }
        self.skip_whitespace();

        // `Bool : [True, False]`, so a bare `True` IS the boolean — roc runs
        // `x = True` then `if x { … }` quite happily, and rocflight refused it with
        // "Cannot unify [True, ..] with Bool". The qualified `Bool.True` was already
        // a boolean; this makes the unqualified spelling agree. `Builtin.roc` writes
        // every `return False` that way.
        if args.is_empty() && matches!(name, "True" | "False") {
            return Ok(Expr::Bool(name == "True", self.node()));
        }
        Ok(Expr::Tag { id: self.node(), name, args })
    }

    /// Read a nominal type declaration: `Name := backing`.
    ///
    /// `Name :: backing` (opaque) is accepted as the same thing. Opacity only matters
    /// across module boundaries, which the interpreter does not have, so treating the
    /// two alike is honest rather than lazy — within one file roc does not distinguish
    /// them either (both allow field access and both accept the plain backing value).
    ///
    /// A trailing `.{ ... }` method block is parsed by `parse_method_block`: its
    /// members become ordinary `Type.method` bindings.
    fn capture_nominal_declaration(&mut self) -> bool {
        let rest = &self.input[self.pos..];
        let line_end = rest.find('\n').unwrap_or(rest.len());
        let line = &rest[..line_end];

        // `Name :=` / `Name ::` — the name must be capitalised, like every type.
        let assign = match line.find(":=").or_else(|| line.find("::")) {
            Some(i) => i,
            None => return false,
        };
        // `::` is the OPAQUE form. roc shows one as `<opaque>` rather than as its
        // backing value, which is the only place the difference is observable here.
        let opaque = line[assign..].starts_with("::");
        // `Wrapper(a) := ...` parameterises the nominal. The parameters need no
        // record of their own: the backing type's `a` becomes a fresh type variable
        // the same way a lowercase name in any annotation does.
        let declared = line[..assign].trim();
        let name = match declared.find('(') {
            Some(i) if declared.ends_with(')') => declared[..i].trim(),
            _ => declared,
        };
        if name.is_empty()
            || !name.starts_with(|c: char| c.is_uppercase())
            || !name.chars().all(|c| c.is_alphanumeric() || c == '_')
        {
            return false;
        }

        // Copied out before the mutable parse calls below end `rest`'s borrow.
        let name_owned: &'static str = Box::leak(name.to_string().into_boxed_str());
        let name = name.to_string();
        let line_start = self.pos;
        self.pos += assign + 2;

        // The backing type may span lines (a record written out field per line), so
        // parse it rather than assuming it ends at the newline.
        self.annotation_vars.clear();
        self.field_defaults.clear();
        self.optional_fields.clear();
        // Give each parameter its id BEFORE the backing type is parsed, so the `a` in
        // `{ item: a }` resolves to the same variable the parameter list declared.
        let param_names: Vec<String> = match declared.find('(') {
            Some(i) if declared.ends_with(')') => declared[i + 1..declared.len() - 1]
                .split(',')
                .map(|p| p.trim().to_string())
                .collect(),
            _ => Vec::new(),
        };
        let params: Vec<u32> =
            param_names.iter().map(|p| self.annotation_var(p)).collect();
        if !params.is_empty() {
            self.nominal_params.push((name.clone(), params));
        }
        // Register a placeholder for the name BEFORE parsing its backing, so a
        // recursive reference inside — `rest : Iter(item)` in `Iter`'s own body —
        // resolves to this nominal rather than to the builtin `Iter` (which is `List`)
        // or a bare variable. Overwritten with the real backing once parsed.
        let slot = self.nominals.len();
        self.nominals.push((
            name_owned,
            Type::Nominal { name: name_owned, backing: Box::new(Type::TypeVar(u32::MAX)), args: Vec::new() },
        ));
        match self.parse_type_operand() {
            Ok(backing) => {
                if opaque {
                    self.opaque_nominals.push(name_owned);
                }
                self.nominals[slot] = (
                    name_owned,
                    Type::Nominal { name: name_owned, backing: Box::new(backing), args: Vec::new() },
                );
                // Defaults belong to THIS nominal; clear the scratch list so the next
                // declaration starts empty.
                let defaults = std::mem::take(&mut self.field_defaults);
                self.optional_fields.clear();
                if !defaults.is_empty() {
                    self.nominal_defaults.push((name.clone(), defaults));
                }
                self.parse_method_block(&name);
                true
            }
            Err(_) => {
                // Not something the type parser understands; drop the placeholder and
                // leave the line to be skipped.
                self.nominals.truncate(slot);
                self.pos = line_start + line_end;
                true
            }
        }
    }

    /// Expand `{ a: pa, b: pb, c: pc }.Combiner` into nested `Combiner.map2` calls.
    ///
    /// The fields are folded right, each level pairing its value with the rest:
    ///
    /// ```text
    /// Combiner.map2(pa, Combiner.map2(pb, pc, |x, y| (x, y)), |x, y| { a: x, b: y.0, c: y.1 })
    /// ```
    ///
    /// The inner levels build plain pairs and the outermost builds the record, reading
    /// each field out of the nesting. roc writes the combiners as patterns —
    /// `|month, (day, year)|` — which this cannot, because a lambda's parameters are
    /// names here; indexing the pair is the same thing said differently.
    fn record_builder(
        &mut self,
        combiner: &'static str,
        fields: &[(&'static str, Expr)],
    ) -> Result<Expr, ParseError> {
        if fields.len() < 2 {
            return Err(ParseError {
                message: format!(
                    "a record builder needs at least two fields to combine, got {}",
                    fields.len()
                ),
                position: self.pos,
            });
        }
        let (head, tail) = ("#rb_head", "#rb_rest");

        // The nesting is right-associated pairs, so field `j` is reached by taking the
        // second element `j - 1` times and then the first — except the last, which IS
        // the second element all the way down.
        let reach = |j: usize| {
            let mut path = Expr::Ident(tail, self.node());
            for _ in 0..j.saturating_sub(1) {
                path = Expr::TupleIndex { id: self.node(), tuple: Box::new(path), index: 1 };
            }
            if j < fields.len() - 1 {
                path = Expr::TupleIndex { id: self.node(), tuple: Box::new(path), index: 0 };
            }
            path
        };

        let mut built = fields[fields.len() - 1].1.clone();
        for index in (0..fields.len() - 1).rev() {
            let outermost = index == 0;
            let body = if outermost {
                let mut assembled = vec![(fields[0].0, Expr::Ident(head, self.node()))];
                for (j, (name, _)) in fields.iter().enumerate().skip(1) {
                    assembled.push((*name, reach(j)));
                }
                Expr::Record(assembled, self.node())
            } else {
                Expr::Tuple(
                    vec![Expr::Ident(head, self.node()), Expr::Ident(tail, self.node())],
                    self.node(),
                )
            };
            built = Expr::Call { id: self.node(),
                func: Box::new(Expr::Qualified { id: self.node(), module: combiner, name: "map2" }),
                args: vec![
                    fields[index].1.clone(),
                    built,
                    Expr::Lambda { id: self.node(),
                        params: std::rc::Rc::from([head, tail]),
                        body: std::rc::Rc::new(body),
                    },
                ],
            };
        }
        Ok(built)
    }

    /// Does `expr` use the name `bare` anywhere inside it?
    fn mentions(expr: &Expr, bare: &str) -> bool {
        if matches!(expr, Expr::Ident(n, _) if *n == bare) {
            return true;
        }
        expr.children().into_iter().any(|child| Self::mentions(child, bare))
    }

    /// Does `expr` use `bare` as a FREE name — one it does not bind itself?
    ///
    /// `|value| value == …` mentions `value`, but it is the lambda's own parameter and
    /// has nothing to do with a `value` bound later in the block.
    pub(crate) fn mentions_free(expr: &Expr, bare: &str) -> bool {
        fn binds(pattern: &Pattern, bare: &str) -> bool {
            match pattern {
                Pattern::Binding(n) => *n == bare,
                Pattern::As { name, inner } => *name == bare || binds(inner, bare),
                Pattern::Tag { args, .. } => args.iter().any(|p| binds(p, bare)),
                Pattern::Tuple(items) => items.iter().any(|p| binds(p, bare)),
                Pattern::Record { fields, rest } => {
                    *rest == Some(bare) || fields.iter().any(|(_, p)| binds(p, bare))
                }
                Pattern::List { before, rest, after } => {
                    matches!(rest, Some(Some(n)) if *n == bare)
                        || before.iter().chain(after.iter()).any(|p| binds(p, bare))
                }
                _ => false,
            }
        }
        match expr {
            Expr::Ident(n, _) => *n == bare,
            Expr::Lambda { params, body, .. } => {
                !params.iter().any(|p| *p == bare) && Self::mentions_free(body, bare)
            }
            Expr::Let { name, value, body, .. } | Expr::VarDecl { name, value, body, .. } => {
                Self::mentions_free(value, bare)
                    || (*name != bare && Self::mentions_free(body, bare))
            }
            Expr::For { name, iterable, body, .. } => {
                Self::mentions_free(iterable, bare)
                    || (*name != bare && Self::mentions_free(body, bare))
            }
            Expr::Match { scrutinee, arms, .. } => {
                Self::mentions_free(scrutinee, bare)
                    || arms.iter().any(|arm| {
                        !arm.patterns.iter().any(|p| binds(p, bare))
                            && (arm.guard.as_ref().is_some_and(|g| Self::mentions_free(g, bare))
                                || Self::mentions_free(&arm.body, bare))
                    })
            }
            other => other.children().into_iter().any(|c| Self::mentions_free(c, bare)),
        }
    }

    /// Order a nominal's block-local members so each comes after the siblings it
    /// names. roc's method block is a recursive group; these are sequential bindings,
    /// so `first = second` has to follow `second`. A cycle keeps its source order —
    /// there is no ordering that works, and the compiler reports the undefined name.
    fn order_by_dependency(
        members: Vec<(u32, &'static str, Option<Type>, Expr)>,
    ) -> Vec<(u32, &'static str, Option<Type>, Expr)> {
        let bare: Vec<&str> =
            members.iter().map(|(_, n, ..)| n.rsplit('.').next().unwrap_or(n)).collect();
        let mut done = vec![false; members.len()];
        let mut open = vec![false; members.len()];
        let mut order: Vec<usize> = Vec::with_capacity(members.len());
        fn visit(
            i: usize,
            members: &[(u32, &'static str, Option<Type>, Expr)],
            bare: &[&str],
            done: &mut Vec<bool>,
            open: &mut Vec<bool>,
            order: &mut Vec<usize>,
            mentions: &dyn Fn(&Expr, &str) -> bool,
        ) {
            if done[i] || open[i] {
                return;
            }
            open[i] = true;
            for (j, name) in bare.iter().enumerate() {
                if j != i && mentions(&members[i].3, name) {
                    visit(j, members, bare, done, open, order, mentions);
                }
            }
            open[i] = false;
            done[i] = true;
            order.push(i);
        }
        for i in 0..members.len() {
            visit(i, &members, &bare, &mut done, &mut open, &mut order, &Self::mentions);
        }
        let mut slots: Vec<Option<(u32, &'static str, Option<Type>, Expr)>> =
            members.into_iter().map(Some).collect();
        order.into_iter().filter_map(|i| slots[i].take()).collect()
    }

    /// Parse a `.{ ... }` method block after a nominal declaration.
    ///
    /// The block holds ordinary bindings — `reveal = |s| s.key` — which become
    /// functions namespaced under the type. They are recorded as `Type.method` and
    /// wrapped around the program by `parse_expr`, so `Secret.reveal` is an ordinary
    /// name lookup and `s.reveal()` is a dispatch that finds it.
    ///
    /// A method's annotation sits inside the block (`show : Counter -> Str`), and is
    /// claimed along with the binding — without it the method's parameter has no type,
    /// so `|c| c.n.to_str()` cannot dispatch on `c.n`.
    fn parse_method_block(&mut self, type_name: &str) {
        self.skip_inline_whitespace();
        if !self.input[self.pos..].starts_with(".{") {
            return;
        }
        // A block inside another's: its methods see the outer block's members too.
        if let Some(outer) = self.open_blocks.last() {
            if outer != type_name {
                self.enclosing_owners.push((type_name.to_string(), outer.clone()));
            }
        }
        self.open_blocks.push(type_name.to_string());
        self.parse_method_block_members(type_name);
        self.open_blocks.pop();
    }

    fn parse_method_block_members(&mut self, type_name: &str) {
        self.pos += 2;

        // Whatever annotations are pending when this block closes, and were pushed
        // while it was open, are its members WITHOUT a body — its intrinsics.
        let outer_annotations = self.pending_annotations.len();

        loop {
            self.skip_trivia();
            let rest = &self.input[self.pos..];
            if rest.is_empty() {
                self.claim_intrinsics(type_name, outer_annotations);
                return;
            }
            if rest.starts_with('}') {
                self.pos += 1;
                self.claim_intrinsics(type_name, outer_annotations);
                return;
            }

            // `name = value`
            let Ok((remaining, ident)) = parse_identifier(rest) else {
                // Not a binding; step over it rather than spinning.
                self.pos += 1;
                continue;
            };
            let consumed = rest.len() - remaining.len();
            let after = rest[consumed..].trim_start();
            if !after.starts_with('=') || after.starts_with("==") || after.starts_with("=>") {
                self.pos += consumed.max(1);
                continue;
            }
            let Expr::Ident(method, _) = ident else {
                self.pos += consumed.max(1);
                continue;
            };

            self.pos += consumed;
            self.skip_whitespace();
            self.pos += 1; // Skip '='
            self.skip_whitespace();

            // Claimed before parsing the value: `skip_trivia` above already read the
            // annotation line into `pending_annotations`.
            let annotation = self.claim_annotation(method);
            // ONE `Type.method` string: it was built, formatted and leaked twice, once
            // for the signature and once for the method, and they are the same text.
            let qualified: &'static str =
                Box::leak(format!("{}.{}", type_name, method).into_boxed_str());
            if let Some(ty) = &annotation {
                self.signatures.push((qualified, ty.clone()));
            }

            match self.parse_or_expr() {
                Ok(value) => {
                    if self.block_depth > 0 {
                        self.local_methods.push((self.block_depth, qualified, annotation, value));
                    } else {
                        self.methods.push((qualified, annotation, value));
                    }
                }
                Err(_) => {
                    self.claim_intrinsics(type_name, outer_annotations);
                    return;
                }
            }
        }
    }

    /// Move the annotations a closing method block left unclaimed into `intrinsics`.
    ///
    /// `outer` is how many annotations were pending when the block opened; anything
    /// past that was declared inside it. A nested block runs first and takes its own,
    /// so each name lands under the type that declared it.
    fn claim_intrinsics(&mut self, type_name: &str, outer: usize) {
        let from = outer.min(self.pending_annotations.len());
        for (name, ty) in self.pending_annotations.split_off(from) {
            let qualified: &'static str = if type_name.is_empty() {
                name
            } else {
                Box::leak(format!("{}.{}", type_name, name).into_boxed_str())
            };
            // The type goes to `signatures`, which is the only place anything reads
            // it. `intrinsics` is a list of NAMES — every reader takes `.0` and throws
            // the type away — so it used to cost a deep `Type` clone apiece, and the
            // low-level section is 454 lines of nothing else.
            self.intrinsics.push(qualified);
            self.signatures.push((qualified, ty));
        }
    }

    /// Members declared with a type but no body: what Rust has to supply.
    pub fn intrinsics(&self) -> &[&'static str] {
        &self.intrinsics
    }

    /// Literal nodes that could not be held exactly — a `Dec` out of range or with too
    /// many places — which the checker refuses, as roc does. Drained.
    pub fn overflowed_literals(&self) -> Vec<crate::ast::NodeId> {
        OVERFLOWED_NODES.with(|o| std::mem::take(&mut *o.borrow_mut()))
    }

    /// Literal nodes with an explicit type suffix, and the type each named.
    pub fn suffixed_literals(&self) -> Vec<(crate::ast::NodeId, Type)> {
        SUFFIXED.with(|s| {
            s.borrow()
                .iter()
                .filter_map(|(id, name)| {
                    Some((*id, builtin_type(name, &mut Vec::new(), || Type::TypeVar(u32::MAX))?))
                })
                .collect()
        })
    }

    /// Every numeric literal's text, by node; see `NUMERAL_TEXT`.
    pub fn numeral_texts(&self) -> std::collections::HashMap<crate::ast::NodeId, String> {
        NUMERAL_TEXT.with(|t| t.borrow().iter().cloned().collect())
    }

    /// Literals with a NOMINAL suffix — `123.MyNum`, `"Roc".Tag`, `"a${b}".Url` — and
    /// which nominal: the literal is that nominal's `from_numeral` / `from_quote` /
    /// `from_interpolation` of itself.
    pub fn nominal_suffixes(&self) -> &[(crate::ast::NodeId, &'static str)] {
        &self.nominal_suffixes
    }

    /// Record literals written as `Name.{ … }`, with the nominal's type.
    /// Per-nominal defaulted fields and their default expressions; see
    /// `nominal_defaults`.
    pub fn field_default_exprs(&self) -> &[(String, Vec<(String, Expr)>)] {
        &self.nominal_defaults
    }

    pub fn nominal_literals(&self) -> &[(crate::ast::NodeId, Type)] {
        &self.nominal_literals
    }

    /// The nominals declared with `::` — the opaque form.
    pub fn opaque_nominals(&self) -> &[&'static str] {
        &self.opaque_nominals
    }

    /// The nominal types the file declared, as `(name, backing)`.
    pub fn nominals(&self) -> &[(&'static str, Type)] {
        &self.nominals
    }

    /// Every `Type.method` annotation the file declared, with or without a body.
    pub fn signatures(&self) -> &[(&'static str, Type)] {
        &self.signatures
    }

    /// Method names any `where` clause in the file promised.
    pub fn where_methods(&self) -> Vec<String> {
        self.where_methods.clone()
    }

    /// Look up a nominal type by name.
    /// An extension alias applied to something it cannot extend.
    ///
    /// `R(x) : { a : I64, ..x }` extends a RECORD and `T(x) : [A, ..x]` a tag union,
    /// and neither may bring a member the base already names. roc refuses both; here
    /// they are recorded as problems, since `parse_type`'s own errors are swallowed.
    fn check_extension(&mut self, alias: &str, declared: &Type, pairs: &[(u32, Type)]) {
        let Some((_, id, is_record)) =
            self.extension_aliases.iter().find(|(n, ..)| n == alias).cloned()
        else {
            return;
        };
        let Some((_, argument)) = pairs.iter().find(|(p, _)| *p == id) else { return };
        let base: Vec<&'static str> = match declared {
            Type::Record { fields, .. } => fields.iter().map(|(f, _)| *f).collect(),
            Type::TagUnion { tags, .. } => tags.iter().map(|(t, _)| *t).collect(),
            _ => return,
        };
        let brought: Vec<&'static str> = match (is_record, argument) {
            (true, Type::Record { fields, .. }) => fields.iter().map(|(f, _)| *f).collect(),
            (false, Type::TagUnion { tags, .. }) => tags.iter().map(|(t, _)| *t).collect(),
            // A type variable is still unknown — the alias may yet be applied to a
            // fitting one — so only a CONCRETE mismatch is a problem.
            (_, Type::TypeVar(_)) => return,
            _ => {
                self.type_problems.push(format!(
                    "`{}` extends a {}, so `{}` cannot be its extension",
                    alias,
                    if is_record { "record" } else { "tag union" },
                    argument
                ));
                return;
            }
        };
        if let Some(duplicate) = brought.iter().find(|m| base.contains(m)) {
            self.type_problems.push(format!(
                "`{}`'s extension names `{}`, which it already has",
                alias, duplicate
            ));
        }
    }

    /// See `type_problems`.
    pub fn type_problems(&self) -> &[String] {
        &self.type_problems
    }

    /// Make the types a file's imports declare known to its annotations, so an
    /// imported `Tup2(I64, I64)` keeps its arguments. A module's own empty
    /// namespace (`Maybe :: []`) is not one of them: a type it declares under its
    /// own name (`Maybe(a)`) is what `Maybe.Maybe` means.
    pub fn declare_imported(&mut self, types: &[(&'static str, Type)], params: &[(String, Vec<u32>)]) {
        for (name, ty) in types {
            let empty = matches!(ty, Type::TagUnion { tags, open: false, .. } if tags.is_empty())
                || matches!(ty, Type::Nominal { backing, .. } if matches!(**backing, Type::TypeVar(_)) || matches!(&**backing, Type::TagUnion { tags, open: false, .. } if tags.is_empty()));
            if !empty {
                self.imported_types.push((name, ty.clone()));
            }
        }
        self.imported_params.extend(params.iter().cloned());
    }

    fn imported_type(&self, name: &str) -> Option<Type> {
        self.imported_types.iter().rev().find(|(n, _)| *n == name).map(|(_, t)| t.clone())
    }

    fn nominal(&self, name: &str) -> Option<Type> {
        // The MOST RECENT declaration of the name. Two blocks may each declare a
        // `Local` of their own — roc scopes a block-local nominal to its block — and
        // the parser walks the file in order, so the last one seen is the one in
        // scope. Taking the first made `Local.Second(8)` a tag of the other block's
        // `[First(U64)]`.
        self.nominals.iter().rev().find(|(n, _)| *n == name).map(|(_, t)| t.clone())
    }

    /// Read a standalone annotation line, remembering its parsed type.
    ///
    /// Replaces the old skip-and-forget. The type is stashed in `pending_annotations`
    /// and claimed by the next binding of the same name; an annotation whose binding
    /// never appears is simply dropped, which is what roc allows too.
    ///
    /// A type the parser cannot make sense of is still skipped rather than raising —
    /// an annotation is documentation to the interpreter, and refusing the file over
    /// one would be worse than ignoring it. `roc check` is the authority on validity.
    fn capture_type_annotation(&mut self) -> bool {
        let rest = &self.input[self.pos..];
        let line_end = rest.find('\n').unwrap_or(rest.len());
        let line = rest[..line_end].trim();

        let colon = match line.find(':') {
            Some(c) => c,
            None => return false,
        };
        // `Module.name` and a trailing `!` are both legal in the name position.
        //
        // A PARAMETERISED alias — `Parser(a) : List(Str) -> Try(a, ...)` — puts its
        // type variables in parentheses after the name, exactly as the nominal form
        // does. The parameters need no record of their own: a lowercase name in the
        // aliased type already becomes a type variable.
        let declared = line[..colon].trim();
        let (name, alias_params): (String, Vec<String>) = match declared.find('(') {
            Some(i) if declared.ends_with(')') => (
                declared[..i].trim().to_string(),
                declared[i + 1..declared.len() - 1]
                    .split(',')
                    .map(|p| p.trim().to_string())
                    .collect(),
            ),
            _ => (declared.to_string(), Vec::new()),
        };
        let name = name.as_str();
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '!')
        {
            return false;
        }
        // `x :: y` is not an annotation.
        if line[colon + 1..].starts_with(':') {
            return false;
        }
        // A record field is `name: value` with no space before the colon; an
        // annotation is `name : Type`. Requiring the space keeps record literals
        // out of this path.
        if !line[..colon].ends_with(char::is_whitespace) {
            return false;
        }

        // `show : a -> Str where [a.to_str : a -> Str]` constrains the type variable.
        // The constraint is the compiler's to verify — all the interpreter takes from
        // it is permission to dispatch those methods on an unresolved variable.
        // The clause may sit on the signature's own line or on the next one.
        // A window, not a copy. This used to `collect()` 400 chars into a `String` for
        // every annotation line in the file, and `Builtin.roc` is mostly annotations.
        //
        // The window is THIS LINE AND THE NEXT, which is what the sentence above says a
        // clause may occupy. It was "the next 400 characters", and finding where 400
        // characters end means decoding 400 characters — on every annotation, to look
        // for something almost none of them have. That one `char_indices().nth(400)`
        // was **15.8%** of the time to parse a file of annotations, and `Builtin.roc`
        // is a file of annotations. Two `find('\n')` instead, which are memchr.
        let region = &self.input[self.pos..];
        let first = region.find('\n').map_or(region.len(), |i| i + 1);
        let window = region[first..].find('\n').map_or(region.len(), |i| first + i);
        let clause_region = &region[..window];
        let promised: Vec<String> = match clause_region.find("where [").map(|i| {
            let tail = &clause_region[i..];
            tail.find(']').map(|end| &tail[..end]).unwrap_or(tail)
        }) {
            Some(clause) => clause
                .split(',')
                .filter_map(|c| {
                    let method = c.split(':').next()?.trim().trim_start_matches('[');
                    let (_, method) = method.rsplit_once('.')?;
                    let method = method.trim_end_matches("()");
                    (!method.is_empty()).then(|| method.to_string())
                })
                .collect(),
            None => Vec::new(),
        };

        // Consume `name :` then parse the type, stopping at the line break.
        let name_owned: &'static str = Box::leak(name.to_string().into_boxed_str());
        let line_start = self.pos;
        self.pos += colon + 1;
        // Each annotation has its own type variables: the `a` in one signature is
        // unrelated to the `a` in the next.
        self.annotation_vars.clear();
        self.pending_extension = None;
        // Give the parameters their ids first, so the aliased type's `a` is the same
        // variable the parameter list declared.
        let params: Vec<u32> =
            alias_params.iter().map(|p| self.annotation_var(p)).collect();
        self.where_methods.extend(promised);
        match self.parse_type() {
            Ok(ty) => {
                // `Bytes : List(U8)` is a TYPE ALIAS, not a value annotation — a
                // capitalised name has no binding to claim it. An alias is
                // transparent, so it is stored as the aliased type itself rather than
                // wrapped in `Nominal`: `Bytes` and `List(U8)` are the same type.
                if name_owned.starts_with(|c: char| c.is_uppercase()) {
                    self.nominals.push((name_owned, ty));
                    if !params.is_empty() {
                        self.nominal_params.push((name.to_string(), params));
                    }
                    if let Some((id, is_record)) = self.pending_extension.take() {
                        self.extension_aliases.push((name.to_string(), id, is_record));
                    }
                } else {
                    self.pending_annotations.push((name_owned, ty));
                }
                // The annotation ends at EOL — unless the type itself spanned lines,
                // as a record type written one field per line does. Whichever ran
                // further is the real end.
                self.pos = self.pos.max(line_start + line_end);
                // A `where` clause may sit on its own continuation line, after the
                // signature. Its constraints were already read off the text; what is
                // left is to step over them so they are not parsed as code.
                self.consume_where_clause();
            }
            Err(_) => self.pos = line_start + line_end,
        }
        true
    }

    /// Step over a `where [...]` clause that follows a signature, wherever it sits.
    ///
    /// The clause is the compiler's to verify; the interpreter has already taken the
    /// method names it promises. Written on its own line it would otherwise be parsed
    /// as an expression, and `where` is not one.
    fn consume_where_clause(&mut self) {
        let saved = self.pos;
        self.skip_whitespace();
        if !starts_with_keyword(&self.input[self.pos..], "where") {
            self.pos = saved;
            return;
        }
        self.pos += "where".len();
        self.skip_whitespace();
        if !self.input[self.pos..].starts_with('[') {
            self.pos = saved;
            return;
        }
        // Bracket-counted, since a constraint's own type may hold `[...]`.
        let mut depth = 0usize;
        while self.pos < self.input.len() {
            match self.input[self.pos..].chars().next() {
                Some('[') => depth += 1,
                Some(']') => {
                    depth -= 1;
                    if depth == 0 {
                        self.pos += 1;
                        return;
                    }
                }
                _ => {}
            }
            self.pos += self.input[self.pos..].chars().next().map_or(1, |c| c.len_utf8());
        }
    }

    /// Take the pending annotation for `name`, if one was declared.
    fn claim_annotation(&mut self, name: &str) -> Option<Type> {
        let index = self.pending_annotations.iter().position(|(n, _)| *n == name)?;
        Some(self.pending_annotations.remove(index).1)
    }

    /// Is `name` bound at the start of a line (column 0)? Used to detect the
    /// implicit entry point of a headerless platformless app.
    fn has_top_level_binding(&self, name: &str) -> bool {
        self.input.lines().any(|line| {
            line.strip_prefix(name)
                .map(|after| after.trim_start().starts_with('='))
                .unwrap_or(false)
        })
    }

    /// Extract app entry point from declaration like: app [main!] { ... }
    /// Returns whether the header was consumed in full (dependency map included).
    ///
    /// When it was, the caller must NOT also skip to the next line: the cursor already
    /// sits on the line after the header, and skipping again swallowed the first
    /// `import`.
    fn extract_app_entry_point(&mut self) -> bool {
        self.pos += 4; // Skip "app "
        self.skip_whitespace();

        let rest = &self.input[self.pos..];
        if rest.starts_with('[') {
            self.pos += 1; // Skip '['
            self.skip_whitespace();

            let rest = &self.input[self.pos..];
            // Find the identifier (entry point), including trailing '!'
            let mut end = 0;
            for (i, ch) in rest.chars().enumerate() {
                if ch == ']' || ch.is_whitespace() {
                    end = i;
                    break;
                }
                if i == rest.len() - 1 {
                    end = rest.len();
                }
            }

            if end > 0 {
                let entry_point = rest[..end].to_string();
                self.entry_point = Some(entry_point);
                self.pos += end;
            }
        }

        // `app [main!] { cli: platform "URL", pkg: "URL", roc: "version" }`
        self.skip_whitespace();
        if self.input[self.pos..].starts_with(']') {
            self.pos += 1;
            self.skip_whitespace();
        }
        if self.input[self.pos..].starts_with('{') {
            self.parse_dependencies();
            return true;
        }
        false
    }

    /// Parse the app header's dependency map.
    ///
    /// Entries look like `alias: platform "URL"` or `alias: "URL"`. The `roc:` entry
    /// pins the compiler version rather than naming something to fetch, so it is
    /// recorded like any other and filtered out by the loader (its spec is not an
    /// archive URL, so it resolves to nothing).
    fn parse_dependencies(&mut self) {
        self.pos += 1; // Skip '{'

        loop {
            self.skip_whitespace();
            let rest = &self.input[self.pos..];
            if rest.is_empty() || rest.starts_with('}') {
                if !rest.is_empty() {
                    self.pos += 1;
                }
                self.skip_whitespace();
                return;
            }
            if rest.starts_with(',') || rest.starts_with('#') {
                if rest.starts_with('#') {
                    self.skip_to_line_end();
                } else {
                    self.pos += 1;
                }
                continue;
            }

            // alias
            let Ok((remaining, ident)) = parse_identifier(rest) else {
                // Something unexpected; skip a character rather than spinning.
                self.pos += 1;
                continue;
            };
            self.pos += rest.len() - remaining.len();
            let alias = match ident {
                Expr::Ident(name, _) => name.to_string(),
                _ => continue,
            };

            self.skip_whitespace();
            if !self.input[self.pos..].starts_with(':') {
                continue;
            }
            self.pos += 1;
            self.skip_whitespace();

            // optional `platform` keyword
            let is_platform = starts_with_keyword(&self.input[self.pos..], "platform");
            if is_platform {
                self.pos += "platform".len();
                self.skip_whitespace();
            }

            // the quoted spec
            let rest = &self.input[self.pos..];
            if !rest.starts_with('"') {
                continue;
            }
            let Some(close) = rest[1..].find('"') else {
                return;
            };
            let spec = rest[1..1 + close].to_string();
            self.pos += close + 2;

            self.dependencies.push((alias, spec, is_platform));
        }
    }

    /// Dependencies declared in the app header.
    pub fn dependencies(&self) -> &[(String, String, bool)] {
        &self.dependencies
    }

    /// Modules brought in by `import`, as `(dependency alias, module)`.
    ///
    /// `import cli.Stdout` gives `("cli", "Stdout")`; a bare `import Module` gives
    /// `("", "Module")`, meaning a module beside the app rather than one from a
    /// dependency.
    pub fn imports(&self) -> &[(String, String)] {
        &self.imports
    }

    /// Local modules imported, as `(module path, names it exposes)`.
    pub fn local_modules(&self) -> &[(String, Vec<String>)] {
        &self.local_modules
    }

    /// Each nominal declared inside another's method block, and that owner. A method
    /// of the inner one sees the outer one's members by their bare names, as its own
    /// siblings: `Box.is_eq` may call `Shape`'s `same_box` as `same_box`.
    pub fn enclosing_owners(&self) -> &[(String, String)] {
        &self.enclosing_owners
    }

    /// Files ingested by `import "path" as name`, as `(binding name, path)`.
    pub fn ingests(&self) -> &[(String, String)] {
        &self.ingests
    }

    /// Note an `import` line. The cursor stays put; the caller skips the line.
    fn record_import(&mut self) {
        let rest = &self.input["import ".len() + self.pos..];
        let line = rest.lines().next().unwrap_or("");
        // `import cli.Stdout exposing [x]` — only the module path is needed here.
        let path = line.split_whitespace().next().unwrap_or("").trim();
        if path.is_empty() {
            return;
        }

        // `import "sample.txt" as sample : Str` INGESTS a file: the binding is the
        // file's contents, not a module. The quotes are what say so.
        if let Some(file) = path.strip_prefix('"').and_then(|p| p.strip_suffix('"')) {
            let name = line
                .split_whitespace()
                .skip_while(|w| *w != "as")
                .nth(1)
                .unwrap_or("")
                .trim_end_matches(':');
            if !name.is_empty() {
                self.ingests.push((name.to_string(), file.to_string()));
            }
            return;
        }
        match path.split_once('.') {
            Some((alias, module)) => {
                self.imports.push((alias.to_string(), module.to_string()))
            }
            None => {
                // A module beside the app. `exposing [a, b]` says which of its names
                // become usable unqualified; the rest stay behind `Module.name`.
                let exposed = line
                    .split_once('[')
                    .and_then(|(_, rest)| rest.split_once(']'))
                    .map(|(names, _)| {
                        names
                            .split(',')
                            .map(|n| n.trim().to_string())
                            .filter(|n| !n.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
                self.local_modules.push((path.to_string(), exposed));
                self.imports.push((String::new(), path.to_string()));
            }
        }
    }

    /// Skip until next declaration or expression
    fn skip_to_next_declaration(&mut self) {
        while self.pos < self.input.len() {
            let rest = &self.input[self.pos..];
            if rest.starts_with('\n') {
                self.pos += 1;
                self.skip_whitespace();
                return;
            }
            self.pos += 1;
        }
    }

    /// Skip to end of line
    fn skip_to_line_end(&mut self) {
        while self.pos < self.input.len() && self.input.as_bytes()[self.pos] != b'\n' {
            self.pos += 1;
        }
    }

    /// Parse let binding or regular expression.
    ///
    /// A loop over the file's chain of bindings rather than a call per binding, so a
    /// module of ten thousand declarations is bounded by memory rather than by the
    /// Rust stack — 20,000 of them overflowed it. Each step parses one statement; the
    /// bindings are folded into the chain from the back once its end is reached.
    fn parse_let_or_expr(&mut self) -> Result<Expr, ParseError> {
        let mut bindings = Vec::new();
        let mut tail = loop {
            match self.parse_statement()? {
                Step::Binding(binding) => bindings.push(binding),
                Step::Done(expr) => break expr,
            }
        };
        for mut binding in bindings.into_iter().rev() {
            let Expr::Let { body, .. } = &mut binding else { unreachable!("a binding is a Let") };
            *body = Box::new(tail);
            tail = binding;
        }
        Ok(tail)
    }

    /// One statement of a chain: a binding that carries on, or the expression the
    /// chain ends in.
    fn parse_statement(&mut self) -> Result<Step, ParseError> {
        self.skip_trivia();

        let rest = &self.input[self.pos..];
        if rest.is_empty() {
            return Err(ParseError {
                message: "Unexpected end of input".to_string(),
                position: self.pos,
            });
        }

        // Check for "let" keyword
        if rest.starts_with("let ") {
            self.pos += 4; // Skip "let "
            self.skip_whitespace();

            // Parse variable name
            let rest = &self.input[self.pos..];
            let (remaining, name_expr) = parse_identifier(rest)?;
            self.pos += rest.len() - remaining.len();

            let name = match name_expr {
                Expr::Ident(n, _) => n,
                _ => unreachable!(),
            };

            self.skip_whitespace();

            // Expect "="
            let rest = &self.input[self.pos..];
            if !rest.starts_with('=') {
                return Err(ParseError {
                    message: "Expected '=' after variable name in let binding".to_string(),
                    position: self.pos,
                });
            }
            self.pos += 1;
            self.skip_whitespace();

            // Parse value expression
            let value = Box::new(self.parse_primary_expr()?);

            self.skip_whitespace();

            // Expect "in"
            let rest = &self.input[self.pos..];
            if !rest.starts_with("in") {
                return Err(ParseError {
                    message: "Expected 'in' after value in let binding".to_string(),
                    position: self.pos,
                });
            }
            // Make sure "in" is followed by whitespace or end of input
            let after_in = &rest[2..];
            if !after_in.is_empty() && !after_in.starts_with(|c: char| c.is_whitespace()) {
                return Err(ParseError {
                    message: "Expected whitespace or end of input after 'in'".to_string(),
                    position: self.pos + 2,
                });
            }
            self.pos += 2; // Skip "in"
            self.skip_whitespace();

            // Parse body expression
            let body = Box::new(self.parse_let_or_expr()?);

            Ok(Step::Done(Expr::Let { id: self.node(), name, annotation: None, value, body }))
        } else {
            // Top-level destructuring: `(a, b) = value`, then the rest of the file.
            // Same shape as inside a block, and likewise a one-arm match.
            if rest.starts_with('(') || rest.starts_with('{') {
                let saved = self.pos;
                let opens_paren = rest.starts_with('(');
                let parsed = if opens_paren {
                    self.parse_tuple_pattern()
                } else {
                    self.parse_record_pattern()
                };
                let destructured = match parsed {
                    Ok(pattern) => {
                        self.skip_whitespace();
                        let after = &self.input[self.pos..];
                        if after.starts_with('=')
                            && !after.starts_with("==")
                            && !after.starts_with("=>")
                        {
                            Some(pattern)
                        } else {
                            self.pos = saved;
                            None
                        }
                    }
                    Err(_) => {
                        self.pos = saved;
                        None
                    }
                };

                if let Some(pattern) = destructured {
                    self.pos += 1; // Skip '='
                    self.skip_whitespace();
                    let value = self.parse_or_expr()?;
                    self.skip_trivia();

                    if self.input[self.pos..].is_empty() {
                        return Err(ParseError {
                            message: "A destructuring binding needs something after it: \
                                      it binds names rather than producing a value"
                                .to_string(),
                            position: self.pos,
                        });
                    }
                    let body = self.parse_let_or_expr()?;
                    return self.destructure_at_top_level(pattern, value, body).map(Step::Done);
                }
            }

            // Check for top-level binding: name = expr
            // This is similar to let but at file level
            let lookahead_rest = &self.input[self.pos..];
            if let Ok((remaining, expr)) = parse_identifier(lookahead_rest) {
                let lookahead_pos = lookahead_rest.len() - remaining.len();
                let after_ident = &lookahead_rest[lookahead_pos..].trim_start();

                if after_ident.starts_with('=') && !after_ident.starts_with("==") {
                    // This is a binding!
                    if let Expr::Ident(name, _) = expr {
                        self.pos += lookahead_pos;
                        self.skip_whitespace();
                        self.pos += 1; // Skip '='
                        self.skip_whitespace();
                        let annotation = self.claim_annotation(name);

                        // Parse value. This must go through the full operator
                        // precedence chain, not just `parse_call_expr`: a top-level
                        // binding can be any expression (`a = 2 + (3 * 4)`), exactly
                        // like a binding inside a block.
                        let value = Box::new(self.parse_or_expr()?);

                        self.skip_whitespace();

                        // Check if there's more content
                        let rest3 = &self.input[self.pos..];
                        if rest3.is_empty() {
                            // Nothing follows, so the file's value is this binding.
                            // The body refers to the name rather than cloning the
                            // value: cloning doubled the AST and made the evaluator
                            // build the value twice, discarding the second copy.
                            return Ok(Step::Done(Expr::Let { id: self.node(),
                                name,
                                annotation,
                                value,
                                body: Box::new(Expr::Ident(name, self.node())),
                            }));
                        } else {
                            // Continue parsing the rest of the chain.
                            self.skip_trivia();
                            if self.input[self.pos..].is_empty() {
                                // Trailing annotations/comments only: this binding is
                                // the last one, so its value is the file's value.
                                return Ok(Step::Done(Expr::Let { id: self.node(),
                                    name,
                                    annotation,
                                    value,
                                    body: Box::new(Expr::Ident(name, self.node())),
                                }));
                            }
                            // The rest of the chain is the body; `parse_let_or_expr`
                            // fills it in once the chain ends.
                            return Ok(Step::Binding(Expr::Let { id: self.node(),
                                name,
                                annotation,
                                value,
                                body: Box::new(Expr::Unit(crate::ast::fresh_node_unlocated())),
                            }));
                        }
                    }
                }
            }

            self.parse_or_expr().map(Step::Done)
        }
    }

    /// Parse the file's statements, in sequence.
    ///
    /// `parse_let_or_expr` stops at the first statement that is not a binding, because
    /// a binding carries the rest of the file as its body while a bare expression has
    /// nowhere to put it. At FILE scope there is always more that might follow — a
    /// module of `expect`s is nothing but bare expressions — so anything left over is
    /// parsed and sequenced after. Not done inside `parse_let_or_expr` itself: a
    /// lambda body without braces goes through it too, and would swallow the rest of
    /// the file.
    fn parse_top_level(&mut self) -> Result<Expr, ParseError> {
        // A file can be nothing but declarations. `Builtin.roc` is 23,555 lines of
        // them, and so is any module whose types and methods are all claimed by
        // `skip_trivia`; there is no trailing expression left to be the file's value,
        // so its value is `{}`. Only the outermost parse may decide this — a lambda
        // body that runs out of input is a truncated file, not an empty one.
        if self.expr_depth == 1 {
            self.skip_trivia();
            if self.input[self.pos..].is_empty() {
                return Ok(Expr::Unit(self.node()));
            }
        }
        let value = self.parse_let_or_expr()?;
        // A top-level test waits for the whole file, so it is set aside here and put
        // back at the end by `parse_expr`.
        if self.expr_depth == 1 && matches!(value, Expr::Expect(_, _)) {
            self.deferred_expects.push(value);
            self.skip_trivia();
            if self.input[self.pos..].is_empty() {
                return Ok(Expr::Unit(self.node()));
            }
            return self.parse_top_level();
        }
        // Only the OUTERMOST parse owns the rest of the file. A braceless lambda body
        // re-enters `parse_expr`, and sequencing there would make `|n| n + 1` swallow
        // every declaration after it.
        if self.expr_depth > 1 {
            return Ok(value);
        }
        self.skip_trivia();
        if self.input[self.pos..].is_empty() {
            return Ok(value);
        }
        Ok(Expr::Let { id: self.node(),
            name: "_",
            annotation: None,
            value: Box::new(value),
            body: Box::new(self.parse_top_level()?),
        })
    }

    /// Parse `expr ?? default`, the loosest operator.
    ///
    /// Desugars to `match expr { Ok(v) => v, Err(_) => default }`. Purely local:
    /// unlike `?`, nothing has to move into the arm, so the rewrite happens right
    /// here rather than in the block fold.
    ///
    /// `??` binds looser than arithmetic — `x ?? 1 + 2` is `x ?? (1 + 2)`, verified
    /// against roc — which is why the right side is parsed at the or-level below.
    fn parse_or_expr(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_or_inner()?;

        loop {
            self.skip_whitespace();
            if !self.input[self.pos..].starts_with("??") {
                break;
            }
            self.pos += 2;
            self.skip_whitespace();
            let default = self.parse_or_inner()?;

            left = Expr::Match { id: self.node(),
                scrutinee: Box::new(left),
                arms: vec![
                    MatchArm {
                        patterns: vec![Pattern::Tag {
                            name: "Ok",
                            args: vec![Pattern::Binding("v")],
                        }],
                        guard: None,
                        body: Expr::Ident("v", self.node()),
                    },
                    MatchArm {
                        patterns: vec![Pattern::Tag {
                            name: "Err",
                            args: vec![Pattern::Wildcard],
                        }],
                        guard: None,
                        body: default,
                    },
                ],
            };
        }

        Ok(left)
    }

    /// Parse logical OR: a || b
    fn parse_or_inner(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_and_expr()?;

        loop {
            self.skip_whitespace();
            let rest = &self.input[self.pos..];

            // Roc spells these `or` / `and`; `||` and `&&` are also accepted here.
            // Both spellings happen to be two characters wide.
            let matched = (rest.starts_with("||") && !rest.starts_with("|||"))
                || starts_with_keyword(rest, "or");
            if matched {
                self.pos += 2;
                self.skip_whitespace();
                let right = self.parse_and_expr()?;
                left = Expr::BinOp { id: crate::ast::fresh_node_like(&left),
                    left: Box::new(left),
                    op: crate::ast::BinOp::Or,
                    right: Box::new(right),
                };
            } else {
                break;
            }
        }

        Ok(left)
    }

    /// Parse logical AND: a && b
    fn parse_and_expr(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_comparison_expr()?;

        loop {
            self.skip_whitespace();
            let rest = &self.input[self.pos..];

            let matched = if rest.starts_with("&&") && !rest.starts_with("&&&") {
                self.pos += 2;
                true
            } else if starts_with_keyword(rest, "and") {
                self.pos += 3;
                true
            } else {
                false
            };
            if matched {
                self.skip_whitespace();
                let right = self.parse_comparison_expr()?;
                left = Expr::BinOp { id: crate::ast::fresh_node_like(&left),
                    left: Box::new(left),
                    op: crate::ast::BinOp::And,
                    right: Box::new(right),
                };
            } else {
                break;
            }
        }

        Ok(left)
    }

    /// Parse comparison: a == b, a < b, etc.
    fn parse_comparison_expr(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_range_expr()?;

        loop {
            self.skip_whitespace();
            let rest = &self.input[self.pos..];

            let op = if rest.starts_with("==") {
                self.pos += 2;
                crate::ast::BinOp::Eq
            } else if rest.starts_with("!=") {
                self.pos += 2;
                crate::ast::BinOp::Ne
            } else if rest.starts_with("<=") {
                self.pos += 2;
                crate::ast::BinOp::Le
            } else if rest.starts_with(">=") {
                self.pos += 2;
                crate::ast::BinOp::Ge
            } else if rest.starts_with('<') && !rest.starts_with("<<") {
                self.pos += 1;
                crate::ast::BinOp::Lt
            } else if rest.starts_with('>') && !rest.starts_with(">>") {
                self.pos += 1;
                crate::ast::BinOp::Gt
            } else {
                break;
            };

            self.skip_whitespace();
            let right = self.parse_range_expr()?;
            left = Expr::BinOp { id: crate::ast::fresh_node_like(&left),
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }

        Ok(left)
    }

    /// Parse a range: `0..<5` (end excluded) or `1..=5` (end included).
    ///
    /// Non-associative — `a..<b..<c` is not a range of ranges — so this reads at most
    /// one operator and does not loop.
    fn parse_range_expr(&mut self) -> Result<Expr, ParseError> {
        let left = self.parse_additive_expr()?;
        self.skip_whitespace();

        let rest = &self.input[self.pos..];
        let inclusive = if rest.starts_with("..<") {
            false
        } else if rest.starts_with("..=") {
            true
        } else {
            return Ok(left);
        };
        self.pos += 3;
        self.skip_whitespace();

        Ok(Expr::Range { id: self.node(),
            start: Box::new(left),
            end: Box::new(self.parse_additive_expr()?),
            inclusive,
        })
    }

    /// Parse addition/subtraction: a + b, a - b
    fn parse_additive_expr(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_multiplicative_expr()?;

        loop {
            self.skip_whitespace();
            let rest = &self.input[self.pos..];

            let op = if rest.starts_with('+') {
                self.pos += 1;
                crate::ast::BinOp::Add
            } else if rest.starts_with('-') && !rest.starts_with("->") && !is_next_digit(rest) {
                // A `-` with space before it and NONE after starts a unary negation,
                // not a subtraction. roc rejects `m -n` outright for this reason.
                //
                // It is not a nicety: without it, a line ending in a value followed by
                // a line starting with `-x` reads as subtraction ACROSS the newline —
                // `n = 5` then `-n` became `5 - n`.
                //
                // The gap has to be found by looking BEHIND: `parse_primary_expr`
                // consumes trailing whitespace, so by the time this loop runs the
                // newline is already gone. Same reason the call parser needs it.
                let tight_right = !rest[1..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_whitespace());
                if tight_right && self.preceded_by_whitespace() {
                    break;
                }
                self.pos += 1;
                crate::ast::BinOp::Sub
            } else {
                break;
            };

            self.skip_whitespace();
            let right = self.parse_multiplicative_expr()?;
            left = Expr::BinOp { id: crate::ast::fresh_node_like(&left),
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }

        Ok(left)
    }

    /// Parse multiplication/division: a * b, a / b
    fn parse_multiplicative_expr(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_unary_expr()?;

        loop {
            self.skip_whitespace();
            let rest = &self.input[self.pos..];

            // `//` must be tested before `/`, or it reads as two divisions.
            let op = if rest.starts_with("//") {
                self.pos += 2;
                crate::ast::BinOp::IntDiv
            } else if rest.starts_with('*') {
                self.pos += 1;
                crate::ast::BinOp::Mul
            } else if rest.starts_with('/') {
                self.pos += 1;
                crate::ast::BinOp::Div
            } else if rest.starts_with('%') {
                self.pos += 1;
                crate::ast::BinOp::Rem
            } else {
                break;
            };

            self.skip_whitespace();
            let right = self.parse_unary_expr()?;
            left = Expr::BinOp { id: crate::ast::fresh_node_like(&left),
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }

        Ok(left)
    }

    /// Parse unary minus: `-x`.
    ///
    /// `-x` IS `x.negate()` — roc lowers it to that method, which is why negating a
    /// Str fails with "This negate method is being called on a value whose type
    /// doesn't have that method" rather than a syntax error. Building a `Dispatch`
    /// here reuses phase 20 wholesale and gives the same diagnostic for free.
    ///
    /// Looser than `|>`, so `-n |> inc` is `-(inc(n))` = -6, not `inc(-n)` = -4.
    /// Verified against roc; the two readings differ, so the numbers matter.
    ///
    /// A negative literal (`-5`) is handled by the number lexer instead, so it stays
    /// one token rather than becoming a negate call on 5.
    fn parse_unary_expr(&mut self) -> Result<Expr, ParseError> {
        self.skip_whitespace();
        let rest = &self.input[self.pos..];

        // `-` followed by a digit is a negative literal, not a negation.
        if rest.starts_with('-') && !rest[1..].starts_with(|c: char| c.is_ascii_digit()) {
            self.pos += 1;
            self.skip_whitespace();
            let operand = self.parse_unary_expr()?;
            return Ok(Expr::Dispatch { id: self.node(),
                receiver: Box::new(operand),
                method: "negate",
                args: Vec::new(),
            });
        }

        self.parse_pipe_expr()
    }

    /// Parse `x |> f`, the pipeline operator.
    ///
    /// `x |> f` is `f(x)`, and `x |> f(a)` is `f(x, a)` — the piped value is
    /// PREPENDED to whatever arguments were written, the same convention static
    /// dispatch uses. Left-associative, so `x |> f |> g` is `g(f(x))`.
    ///
    /// It binds TIGHTER than every binary operator, which is the opposite of most
    /// languages. Verified against roc: `1 + 2 |> inc` is `1 + inc(2)` = 4, and
    /// `2 * 3 |> inc` is `2 * inc(3)` = 8. That is why this level sits between the
    /// multiplicative operators and the call level rather than at the top.
    fn parse_pipe_expr(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_call_expr()?;

        loop {
            self.skip_whitespace();
            if !self.input[self.pos..].starts_with("|>") {
                break;
            }
            self.pos += 2;
            self.skip_whitespace();

            // `x |> (expr)` GROUPS when the parenthesized value is the WHOLE target:
            // it is computed, then `x` is applied to it — `2 |> (bar(3).blah())` calls
            // the returned function with 2. But if a call or method follows the closing
            // paren — `1 |> (|v| v + 1)()`, `x |> (f)(a)` — that is an ordinary call and
            // `x` inserts as its first argument. Detect by parsing the group alone and
            // seeing whether a `(` or `.` follows it.
            let mut grouped = false;
            if self.input[self.pos..].starts_with('(') {
                let before = self.pos;
                let _ = self.parse_primary_expr()?;
                self.skip_whitespace();
                let after = &self.input[self.pos..];
                grouped = !(after.starts_with('(') || after.starts_with('.'));
                self.pos = before;
            }
            self.pipe_target = true;
            let target = self.parse_call_expr();
            self.pipe_target = false;
            let target = target?;
            if grouped {
                left = Expr::Call { id: self.node(), func: Box::new(target), args: vec![left] };
                left = self.parse_postfix(left, false)?;
                continue;
            }
            left = match target {
                // Already a call: the piped value joins its arguments, in front.
                Expr::Call { func, args, .. } => {
                    let mut all = Vec::with_capacity(args.len() + 1);
                    all.push(left);
                    all.extend(args);
                    Expr::Call { id: self.node(), func, args: all }
                }
                // A method call: the piped value becomes the first EXPLICIT argument,
                // after the receiver — `xs |> r.concat()` is `r.concat(xs)`, and
                // `2 |> h.sum(4)` is `h.sum(2, 4)`.
                Expr::Dispatch { receiver, method, mut args, id } => {
                    args.insert(0, left);
                    Expr::Dispatch { receiver, method, args, id }
                }
                // A bare tag is a constructor: `2 |> Ok` is `Ok(2)`.
                Expr::Tag { id, name, mut args } => {
                    args.insert(0, left);
                    Expr::Tag { id, name, args }
                }
                // A bare function — a name, a lambda, `Module.fn` — is applied to it.
                func => Expr::Call { id: self.node(), func: Box::new(func), args: vec![left] },
            };
            // `2 |> bar() .blah()(3)`: the whitespace-separated postfix applies to the
            // completed pipe.
            left = self.parse_postfix(left, false)?;
        }

        Ok(left)
    }

    /// `match base { Ok(#x) => proj(#x), Err(#e) => Err(#e) }` — the desugaring that
    /// lets a `.?` chain continue: whatever follows reads off the `Ok` payload, and a
    /// missing slot anywhere short-circuits to `Err`.
    fn map_through_ok(&mut self, base: Expr, proj: impl FnOnce(Expr, &mut Self) -> Expr) -> Expr {
        self.opt_chain_counter += 1;
        let ok_name: &'static str = Box::leak(format!("#opt{}", self.opt_chain_counter).into_boxed_str());
        let err_name: &'static str = Box::leak(format!("#optErr{}", self.opt_chain_counter).into_boxed_str());
        let body = proj(Expr::Ident(ok_name, self.node()), self);
        let arms = vec![
            crate::ast::MatchArm {
                patterns: vec![Pattern::Tag { name: "Ok", args: vec![Pattern::Binding(ok_name)] }],
                guard: None,
                body,
            },
            crate::ast::MatchArm {
                patterns: vec![Pattern::Tag { name: "Err", args: vec![Pattern::Binding(err_name)] }],
                guard: None,
                body: Expr::Tag { id: self.node(), name: "Err", args: vec![Expr::Ident(err_name, self.node())] },
            },
        ];
        Expr::Match { id: self.node(), scrutinee: Box::new(base), arms }
    }

    /// Parse function call or primary expression
    fn parse_call_expr(&mut self) -> Result<Expr, ParseError> {
        let stop_ws_dot = std::mem::take(&mut self.pipe_target);
        let expr = self.parse_primary_expr()?;
        self.parse_postfix(expr, stop_ws_dot)
    }

    /// The postfix operators after `expr`: calls, field reads, method calls, `?`.
    /// With `stop_ws_dot`, a `.name` that whitespace separates from `expr` is left
    /// for the caller (see `pipe_target`).
    fn parse_postfix(&mut self, mut expr: Expr, stop_ws_dot: bool) -> Result<Expr, ParseError> {
        // True once `expr` is a `Try` produced by `.?` access: a following `.field`
        // or `.?field` then maps through the `Ok`, so `o.?b.c` and `o.?b.?c` work.
        let mut optional_chain = false;

        loop {
            self.skip_whitespace();
            let rest = &self.input[self.pos..];
            if stop_ws_dot
                && rest.starts_with('.')
                && rest[1..].starts_with(is_ident_start)
                && self.preceded_by_whitespace()
            {
                break;
            }

            // Postfix `?`. It cannot be unwrapped where it stands — the whole rest of
            // the block has to move into the `Ok` arm — so the Try is lifted out under
            // a fresh name here and `parse_block` folds the match around the statement.
            // That covers `f(g(x)?)` and `if predicate(item)? { … }` as well as
            // `$t + double(x)?`, where the `?` binds to an operand rather than to the
            // statement's value.
            //
            // The case where the `?` DOES cover the whole value is handed straight
            // back to the statement parser, which needs no extra binding for it; see
            // `parse_block`. `??` is the default operator and belongs to
            // `parse_or_expr`, and `?:` is an optional record field.
            if rest.starts_with('?')
                && !rest.starts_with("??")
                && self.block_depth > 0
                && !rest[1..].starts_with(':')
            {
                self.pos += 1;
                // Inline only: a mapper sits on the SAME line, and crossing the
                // newline would swallow whatever declaration comes next.
                self.skip_inline_whitespace();
                // `expr ? |e| MyError(e)` replaces the error before propagating it,
                // and `expr ? MyError` is the short spelling of the same thing — a tag
                // name used as the constructor it is.
                let rest = &self.input[self.pos..];
                let mapper = if rest.starts_with('|') {
                    Some(self.parse_lambda()?)
                } else if rest.starts_with(char::is_uppercase) {
                    let name: String = rest
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    self.pos += name.len();
                    let name: &'static str = Box::leak(name.into_boxed_str());
                    Some(Expr::Tag { id: self.node(), name, args: Vec::new() })
                } else {
                    None
                };
                let fresh: &'static str =
                    Box::leak(format!("#try{}", self.pending_tries.len()).into_boxed_str());
                self.pending_tries.push((fresh, expr, mapper));
                expr = Expr::Ident(fresh, self.node());
                self.skip_whitespace();
                continue;
            }

            // Postfix field access: `r.x`, and chains like `r.a.b`. Applies to any
            // receiver — `f(x).field` and `{ a: 1 }.a` included — which is why it
            // lives here rather than in the identifier branch of parse_primary_expr.
            // Optional field access: `.?name`. Yields a Try rather than the value,
            // because the field may be absent.
            if rest.starts_with(".?") && rest[2..].starts_with(is_ident_start) {
                self.pos += 2;
                let rest = &self.input[self.pos..];
                let (remaining, field_expr) = parse_identifier(rest)?;
                self.pos += rest.len() - remaining.len();
                if let Expr::Ident(field, _) = field_expr {
                    expr = if optional_chain {
                        // `o.?b.?c`: read `.?c` off the Ok payload, which is itself a Try.
                        self.map_through_ok(expr, |slot, this| Expr::OptionalField {
                            id: this.node(),
                            record: Box::new(slot),
                            field,
                        })
                    } else {
                        Expr::OptionalField { id: self.node(), record: Box::new(expr), field }
                    };
                    optional_chain = true;
                    self.skip_whitespace();
                    continue;
                }
                return Err(ParseError {
                    message: "Expected a field name after `.?`".to_string(),
                    position: self.pos,
                });
            }

            // `"hello".Str`: a type suffix that says what the literal already is.
            if rest.starts_with(".Str") && !rest[4..].starts_with(is_ident_char)
                && matches!(expr, Expr::Str(..) | Expr::StrInterp(..))
            {
                self.pos += 4;
                self.skip_whitespace();
                continue;
            }
            // `"Roc".Tag`, `123.MyNum`, `'a'.Code`: a NOMINAL suffix, which is that
            // nominal's literal conversion applied to the literal. A numeric width
            // suffix never reaches here — the number reader consumed it.
            if rest.starts_with('.') && rest[1..].starts_with(char::is_uppercase)
                && matches!(expr, Expr::Str(..) | Expr::StrInterp(..) | Expr::Int(..) | Expr::Float(..))
            {
                let name: String = rest[1..].chars().take_while(|c| is_ident_char(*c)).collect();
                self.pos += 1 + name.len();
                let name: &'static str = Box::leak(name.into_boxed_str());
                self.nominal_suffixes.push((expr.id(), name));
                self.skip_whitespace();
                continue;
            }

            // RECORD BUILDER: `{ a: pa, b: pb }.Combiner`.
            //
            // roc combines the fields with the named type's `map2`, so a record of
            // parsers becomes a parser of records. `DateParser.roc` writes the
            // expansion out by hand in its second test, which is what this matches.
            if rest.starts_with('.') && rest[1..].starts_with(char::is_uppercase) {
                if let Expr::Record(fields, _) = &expr {
                    let fields = fields.clone();
                    self.pos += 1;
                    let rest = &self.input[self.pos..];
                    let name: String = rest
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    self.pos += name.len();
                    self.skip_whitespace();
                    let combiner: &'static str = Box::leak(name.into_boxed_str());
                    expr = self.record_builder(combiner, &fields)?;
                    continue;
                }
            }

            // Positional tuple access: `.0`, `.1`. Same postfix slot as `.field`,
            // distinguished by the index being digits rather than an identifier.
            if rest.starts_with('.') && rest[1..].starts_with(|c: char| c.is_ascii_digit()) {
                self.pos += 1; // Skip '.'
                let digits: String = self.input[self.pos..]
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                self.pos += digits.len();
                let index = digits.parse::<usize>().map_err(|_| ParseError {
                    message: format!("Invalid tuple index .{}", digits),
                    position: self.pos,
                })?;
                expr = Expr::TupleIndex { id: self.node(), tuple: Box::new(expr), index };
                self.skip_whitespace();
                continue;
            }

            if rest.starts_with('.') && rest[1..].starts_with(is_ident_start) {
                self.pos += 1; // Skip '.'
                let rest = &self.input[self.pos..];
                let (remaining, field_expr) = parse_identifier(rest)?;

                // `.name(` is a method call, `.name` a field read. The parens are the
                // only difference, so the check has to happen before committing to a
                // FieldAccess. No whitespace is allowed before the `(`, same as any
                // other call.
                let after_name = &remaining[..];
                if after_name.starts_with('(') {
                    if let Expr::Ident(method, _) = field_expr {
                        self.pos += rest.len() - remaining.len();
                        let args = self.parse_call_arguments()?;
                        expr = Expr::Dispatch { id: self.node(), receiver: Box::new(expr), method, args };
                        self.skip_whitespace();
                        continue;
                    }
                }
                self.pos += rest.len() - remaining.len();
                match field_expr {
                    Expr::Ident(field, _) => {
                        expr = if optional_chain {
                            // `o.?b.c`: read `.c` off the Ok payload and re-wrap in Ok,
                            // so the whole chain stays a `Try` the `??` can unwrap.
                            self.map_through_ok(expr, |slot, this| Expr::Tag {
                                id: this.node(),
                                name: "Ok",
                                args: vec![Expr::FieldAccess { id: this.node(), record: Box::new(slot), field }],
                            })
                        } else {
                            Expr::FieldAccess { id: self.node(), record: Box::new(expr), field }
                        };
                        self.skip_whitespace();
                        continue;
                    }
                    _ => {
                        return Err(ParseError {
                            message: "Expected a field name after '.'".to_string(),
                            position: self.pos,
                        })
                    }
                }
            }

            // Function application requires NO whitespace before the `(`: roc rejects
            // `f (1)` ("this token cannot start a statement here"). That rule is what
            // makes `if n > 0 (if n > 10 "big" else "small") else "neg"` unambiguous —
            // without it, the condition `n > 0` swallows the parenthesised
            // then-branch as a call on `0`.
            //
            // Field access does NOT share this rule: `r .x` is accepted by roc.
            if rest.starts_with('(') && !self.preceded_by_whitespace() {
                self.pos += 1; // Skip '('
                self.skip_whitespace();

                let mut args = Vec::new();

                // Parse arguments. Each argument is a full expression, not a single
                // atom: `I64.to_str(inc(41))` nests a call, and `f(a + 1)` nests an
                // operator. `parse_primary_expr` handles neither, and stops after
                // `inc`, leaving the `(` to fail the closing-paren check.
                let rest = &self.input[self.pos..];
                if !rest.starts_with(')') {
                    loop {
                        args.push(self.parse_or_expr()?);
                        self.skip_whitespace();

                        let rest = &self.input[self.pos..];
                        if rest.starts_with(',') {
                            self.pos += 1;
                            self.skip_whitespace();
                            // A trailing comma before `)` is legal.
                            if self.input[self.pos..].starts_with(')') {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }

                self.skip_whitespace();
                let rest = &self.input[self.pos..];
                if !rest.starts_with(')') {
                    return Err(ParseError {
                        message: "Expected ')' after function arguments".to_string(),
                        position: self.pos,
                    });
                }
                self.pos += 1; // Skip ')'

                expr = Expr::Call { id: self.node(),
                    func: Box::new(expr),
                    args,
                };
            } else if rest.starts_with("->") {
                // `x->f(a)` is `f(x, a)` and `x->Module.f()` is `Module.f(x)`: the
                // value becomes the first argument of what follows.
                self.pos += 2;
                self.skip_whitespace();
                let callee = self.parse_primary_expr()?;
                let mut args = vec![expr];
                if self.input[self.pos..].starts_with('(') {
                    args.extend(self.parse_call_arguments()?);
                }
                expr = Expr::Call { id: self.node(), func: Box::new(callee), args };
                self.skip_whitespace();
            } else {
                break;
            }
        }

        Ok(expr)
    }

    /// Parse primary expression: number, string, identifier, lambda
    /// Parse a multiline string: consecutive lines each beginning with `\\`.
    ///
    /// The value is the text after each `\\`, joined with newlines and with NO trailing
    /// newline. Content is RAW — `\\t` inside one is a backslash and a `t`, not a tab —
    /// but `${...}` interpolation still works, which is why the parts are built rather
    /// than the text interned whole.
    fn parse_multiline_string(&mut self) -> Result<Expr, ParseError> {
        let mut content = String::new();
        let mut first = true;

        loop {
            if !self.input[self.pos..].starts_with("\\\\") {
                break;
            }
            self.pos += 2;

            let rest = &self.input[self.pos..];
            let end = rest.find('\n').unwrap_or(rest.len());
            if !first {
                content.push('\n');
            }
            first = false;
            content.push_str(&rest[..end]);
            self.pos += end;

            // Another `\\` after the newline continues the same string; anything else
            // ends it, and the cursor stays where the string stopped.
            let resume = self.pos;
            self.pos += self.input[self.pos..].len() - self.input[self.pos..].trim_start().len();
            if !self.input[self.pos..].starts_with("\\\\") {
                self.pos = resume;
                break;
            }
        }

        self.skip_whitespace();
        if content.contains("${") {
            // Named rather than cloned, for the reason `parse_string` gives.
            let parts = {
                let Parser { nominals, nominal_defaults, nominal_literals, .. } = self;
                parse_interpolation_parts(&content, nominals, nominal_defaults, nominal_literals)?
            };
            return Ok(Expr::StrInterp(parts, self.node()));
        }
        Ok(Expr::Str(string_pool::intern(&content), self.node()))
    }

    /// Parse `'a'` into the code point it denotes.
    ///
    /// The result is an `Int` because that is what roc makes of it: `'a' + 1` is 98,
    /// and a grapheme literal unifies with any number type.
    fn parse_grapheme_literal(&mut self) -> Result<Expr, ParseError> {
        let open = self.pos;
        self.pos += 1; // opening quote

        let rest = &self.input[self.pos..];
        let (ch, width) = match rest.chars().next() {
            Some('\\') => {
                let escape = rest[1..].chars().next().ok_or_else(|| ParseError {
                    message: "Unclosed grapheme literal".to_string(),
                    position: open,
                })?;
                match escape {
                    'n' => ('\n', 2),
                    't' => ('\t', 2),
                    'r' => ('\r', 2),
                    '\\' => ('\\', 2),
                    '\'' => ('\'', 2),
                    '"' => ('"', 2),
                    // `'\u(e9)'`, the same escape strings use.
                    'u' if rest[2..].starts_with('(') => {
                        let close = rest.find(')').ok_or_else(|| ParseError {
                            message: "Unclosed \\u( escape".to_string(),
                            position: open,
                        })?;
                        let hex = &rest[3..close];
                        let c = u32::from_str_radix(hex, 16)
                            .ok()
                            .and_then(char::from_u32)
                            .ok_or_else(|| ParseError {
                                message: format!("Invalid unicode escape \\u({})", hex),
                                position: open,
                            })?;
                        (c, close + 1)
                    }
                    other => {
                        return Err(ParseError {
                            message: format!("Unknown escape \\{} in a grapheme literal", other),
                            position: open,
                        })
                    }
                }
            }
            Some(c) => (c, c.len_utf8()),
            None => {
                return Err(ParseError {
                    message: "Unclosed grapheme literal".to_string(),
                    position: open,
                })
            }
        };

        self.pos += width;
        if !self.input[self.pos..].starts_with('\'') {
            return Err(ParseError {
                message: "A grapheme literal holds exactly one character".to_string(),
                position: open,
            });
        }
        self.pos += 1;

        Ok(Expr::Int(ch as i128, self.node()))
    }

    fn parse_primary_expr(&mut self) -> Result<Expr, ParseError> {
        self.skip_whitespace();
        // `...` stands where code is not written yet, and crashes if reached.
        if self.input[self.pos..].starts_with("...") {
            self.pos += 3;
            return Ok(Expr::Crash(Box::new(Expr::Str("not implemented", self.node())), self.node()));
        }
        // Where this expression starts. Most nodes are built after their text has been
        // consumed, so `self.pos` by then points PAST them; a literal parsed by one of
        // the free functions does not know its position at all. Stamping the start here
        // fixes both, and every composite node inherits it from its first child.
        let start = self.pos;
        let parsed = self.parse_primary_inner();
        if let Ok(expr) = &parsed {
            crate::ast::relocate(expr.id(), start);
        }
        parsed
    }

    fn parse_primary_inner(&mut self) -> Result<Expr, ParseError> {
        // Consumed by the FIRST primary of a statement, whatever it turns out to be:
        // in `f({ x })` the statement's first primary is `f`, so the argument's brace
        // is no longer at a statement head.
        let stmt_head = std::mem::take(&mut self.stmt_head);
        self.skip_whitespace();

        let rest = &self.input[self.pos..];
        if rest.is_empty() {
            return Err(ParseError {
                message: "Unexpected end of input".to_string(),
                position: self.pos,
            });
        }

        // A multiline string: `\\line` continued over consecutive lines.
        if rest.starts_with("\\\\") {
            return self.parse_multiline_string();
        }

        // Grapheme literal: `'a'` is the number 97, not a one-character Str. roc has
        // no character type — the literal is a number literal spelled visually.
        if rest.starts_with('\'') {
            return self.parse_grapheme_literal();
        }

        // `match scrutinee { ... }`. Before the identifier branch, like `if`.
        if starts_with_keyword(rest, "match") {
            return self.parse_match();
        }

        // Statement keywords. Each takes one operand, so they parse like a prefix
        // operator rather than needing a statement position of their own.
        if starts_with_keyword(rest, "return") {
            self.pos += 6;
            self.skip_whitespace();
            return Ok(Expr::Return(Box::new(self.parse_or_expr()?), self.node()));
        }
        if starts_with_keyword(rest, "crash") {
            self.pos += 5;
            self.skip_whitespace();
            return Ok(Expr::Crash(Box::new(self.parse_or_expr()?), self.node()));
        }
        if starts_with_keyword(rest, "expect") {
            self.pos += 6;
            self.skip_whitespace();
            return Ok(Expr::Expect(Box::new(self.parse_or_expr()?), self.node()));
        }
        if starts_with_keyword(rest, "dbg") {
            self.pos += 3;
            self.skip_whitespace();
            return Ok(Expr::Dbg(Box::new(self.parse_or_expr()?), self.node()));
        }

        // Loops are EXPRESSIONS whose value is `{}`, so they may be bound
        // (`y = for n in xs { ... }`) as well as used as statements. roc allows both.
        if starts_with_keyword(rest, "for") {
            return self.parse_for();
        }
        if starts_with_keyword(rest, "while") {
            return self.parse_while();
        }
        if starts_with_keyword(rest, "break") {
            self.pos += 5;
            self.skip_whitespace();
            return Ok(Expr::Break(self.node()));
        }

        // `if cond then else otherwise`. Checked before the identifier branch, or
        // `if` would parse as a variable named "if".
        if starts_with_keyword(rest, "if") {
            return self.parse_if();
        }

        // Prefix `!` is logical not. Unrelated to the `!` that ends an effectful
        // name: that one is part of the identifier and never appears in front.
        // Canonicalises to `Bool.not(x)`, matching the upstream compiler.
        if rest.starts_with('!') && !rest.starts_with("!=") {
            self.pos += 1;
            self.skip_whitespace();
            let operand = self.parse_call_expr()?;
            return Ok(Expr::Call { id: self.node(),
                func: Box::new(Expr::Qualified { id: self.node(), module: "Bool", name: "not" }),
                args: vec![operand],
            });
        }

        // List literal: `[1, 2, 3]`, `[]`.
        if rest.starts_with('[') {
            return self.parse_list();
        }

        // `{` starts either a record literal or a block.
        if rest.starts_with('{') {
            if stmt_head && self.looks_like_record(true) {
                return self.parse_record();
            }
            return self.parse_braced();
        }

        // `(` opens either a grouped expression or a tuple. A comma decides:
        // `(1)` is grouping, `(1, 2)` is a tuple. roc has no one-tuple.
        if rest.starts_with('(') {
            self.pos += 1;
            self.skip_whitespace();
            let first = self.parse_or_expr()?;
            self.skip_whitespace();

            if self.input[self.pos..].starts_with(',') {
                let mut items = vec![first];
                while self.input[self.pos..].starts_with(',') {
                    self.pos += 1;
                    self.skip_whitespace();
                    // A trailing comma before `)` is allowed.
                    if self.input[self.pos..].starts_with(')') {
                        break;
                    }
                    items.push(self.parse_or_expr()?);
                    self.skip_whitespace();
                }
                if !self.input[self.pos..].starts_with(')') {
                    return Err(ParseError {
                        message: "Expected ')' to close tuple".to_string(),
                        position: self.pos,
                    });
                }
                self.pos += 1;
                self.skip_whitespace();
                return Ok(Expr::Tuple(items, self.node()));
            }

            if !self.input[self.pos..].starts_with(')') {
                return Err(ParseError {
                    message: "Expected ')' to close parenthesised expression".to_string(),
                    position: self.pos,
                });
            }
            self.pos += 1;
            self.skip_whitespace();
            return Ok(first);
        }

        // Try lambda first: |x| body or |x, y| body
        if rest.starts_with('|') {
            return self.parse_lambda();
        }

        // Try number
        match parse_number_literal(rest) {
            Ok((remaining, expr)) => {
                self.pos += rest.len() - remaining.len();
                self.skip_whitespace();
                return Ok(expr);
            }
            // A literal that IS a number but does not fit says so. Falling through to
            // the string branch reported `Expected '"'` three tokens later, which named
            // neither the literal nor the reason.
            Err(e) if e.message.starts_with("Integer literal") => {
                return Err(ParseError { message: e.message, position: self.pos })
            }
            Err(_) => {}
        }

        // Try string
        if rest.starts_with('"') {
            return self.parse_string();
        }

        // Try identifier or qualified name
        if let Some(first_char) = rest.chars().next() {
            if is_ident_start(first_char) {
                if let Ok((remaining, expr)) = parse_identifier(rest) {
                    self.pos += rest.len() - remaining.len();

                    // `x.y` is either a module member or a record field access.
                    // Roc capitalises modules and types, so the receiver's case
                    // decides: `Str.inspect` is a module member, `point.x` a field.
                    if let Expr::Ident(base, _) = expr {
                        if base == "Crypto" {
                            if let Some(call) = self.parse_crypto_chain() {
                                return Ok(call);
                            }
                        }
                        if base.starts_with(|c: char| c.is_uppercase()) {
                            // `Name.{ ... }` builds a nominal from its backing record.
                            // The nominal name is erased in the value, matching roc:
                            // `Str.inspect` on one shows the bare record.
                            if self.input[self.pos..].starts_with(".{") {
                                self.pos += 1; // Skip '.', leaving the '{'
                                let built = self.parse_nominal_braced()?;
                                // Omitting a defaulted field substitutes its default,
                                // so the record always has it — which is why a
                                // defaulted field needs no unwrapping to read.
                                let built = self.fill_defaults(base, built);
                                if let Some(declared) = self.nominal(base) {
                                    self.nominal_literals.push((built.id(), declared));
                                }
                                return Ok(built);
                            }
                            // `Name.(payload)` builds a nominal over a non-record
                            // — recorded for the same reason `.{ … }` is: the nominal
                            // is erased, so this is what keeps the payload's type.
                            // payload: `UserId.(7)` is 7, `Pair.(1, "two")` is the
                            // tuple. Like `.{ }`, the nominal name is erased — roc
                            // inspects both as the bare payload. Skipping the `.`
                            // leaves the `(`, which already parses as grouping or a
                            // tuple depending on the comma.
                            if self.input[self.pos..].starts_with(".(") {
                                self.pos += 1;
                                let built = self.parse_primary_expr()?;
                                if let Some(declared) = self.nominal(base) {
                                    self.nominal_literals.push((built.id(), declared));
                                }
                                return Ok(built);
                            }
                            if let Some(qualified) = self.try_parse_qualified(base) {
                                // `Animal.Dog(x)` is the tag `Dog(x)`: the
                                // qualification says which nominal it belongs to, and
                                // carries no runtime weight.
                                if let Expr::Qualified { module, name, .. } = qualified {
                                    // A capitalised name after a module is a TAG,
                                    // whether or not this file declared the nominal:
                                    // `Try.Ok(x)` is `Ok(x)`, and `Try` is declared in
                                    // `Builtin.roc`, not here. Requiring a local
                                    // declaration made every qualified builtin tag an
                                    // "Unknown function". `Bool.True` lands on the
                                    // boolean through `finish_tag`, as the bare
                                    // spelling does.
                                    // `Cfg.Cfg.{ … }` builds the nominal `Cfg` through
                                    // its module: the same as `Cfg.{ … }` here.
                                    if name.starts_with(|c: char| c.is_uppercase())
                                        && self.input[self.pos..].starts_with(".{")
                                    {
                                        self.pos += 1;
                                        let built = self.parse_nominal_braced()?;
                                        // Through another module, the nominal is that
                                        // module's, as for a qualified type, and so are
                                        // its defaults: this file's own `Dim` must not
                                        // fill in fields of the import's.
                                        let foreign = self.nominal(module).is_none()
                                            && self.imported_type(name).is_some();
                                        let built = if foreign { built } else { self.fill_defaults(name, built) };
                                        let declared = if foreign { self.imported_type(name) } else { self.nominal(name) };
                                        if let Some(declared) = declared {
                                            self.nominal_literals.push((built.id(), declared));
                                        }
                                        return Ok(built);
                                    }
                                    // `ThingMod.Thing.Make(7)` — a module, then the
                                    // nominal it declares, then the tag. The middle
                                    // segment names the nominal the tag belongs to, so
                                    // step past the module and read the rest as
                                    // `Thing.Make`; without it `Thing` became a bare
                                    // tag and `.Make(…)` a method call on it.
                                    // A nominal declared inside another's method block
                                    // — `One := [A].{ Two := [B].{ value = 1 } }` — is
                                    // reached as `One.Two.value`, however deep, so the
                                    // capitalised segments are walked to the last one.
                                    let mut owner = name;
                                    while owner.starts_with(|c: char| c.is_uppercase())
                                        && self.input[self.pos..].starts_with('.')
                                        && self.input[self.pos + 1..].starts_with(|c: char| c.is_uppercase())
                                    {
                                        let Some(inner) = self.try_parse_qualified(owner) else { break };
                                        let Expr::Qualified { name: next, .. } = inner else { return Ok(inner) };
                                        if !next.starts_with(|c: char| c.is_uppercase()) {
                                            return Ok(inner);
                                        }
                                        let after = &self.input[self.pos..];
                                        if after.starts_with('.') && after[1..].starts_with(|c: char| c.is_uppercase()) {
                                            owner = next;
                                            continue;
                                        }
                                        if after.starts_with('.')
                                            && after[1..].starts_with(is_ident_start)
                                            && self.nominal(next).is_some()
                                        {
                                            if let Some(member) = self.try_parse_qualified(next) {
                                                return Ok(member);
                                            }
                                        }
                                        let tag = self.finish_tag(next)?;
                                        if let Some(declared) = self.nominal(owner) {
                                            self.nominal_literals.push((tag.id(), declared));
                                        }
                                        return Ok(tag);
                                    }
                                    // `One.Two.value`: a nested nominal's member.
                                    if name.starts_with(|c: char| c.is_uppercase())
                                        && self.input[self.pos..].starts_with('.')
                                        && self.input[self.pos + 1..].starts_with(is_ident_start)
                                        && !self.input[self.pos + 1..].starts_with(|c: char| c.is_uppercase())
                                        && (self.nominal(name).is_some() || self.imported(module))
                                    {
                                        if let Some(member) = self.try_parse_qualified(name) {
                                            return Ok(member);
                                        }
                                    }
                                    if name.starts_with(|c: char| c.is_uppercase()) {
                                        let tag = self.finish_tag(name)?;
                                        // `Logic.True` is a tag of `Logic`, not the
                                        // boolean `finish_tag` makes of a bare `True`.
                                        let tag = match tag {
                                            Expr::Bool(_, id) if module != "Bool" && self.nominal(module).is_some() => {
                                                Expr::Tag { id, name, args: Vec::new() }
                                            }
                                            other => other,
                                        };
                                        // A tag of a nominal declared HERE is checked
                                        // against that nominal's union, the way a
                                        // `Name.{ … }` record is: `Maybe.Some(42)`
                                        // is how the `42` learns it is an `I64`.
                                        if let Some(declared) = self.nominal(module) {
                                            self.nominal_literals.push((tag.id(), declared));
                                        }
                                        return Ok(tag);
                                    }
                                }
                                return Ok(qualified);
                            }
                        }
                    }
                    // Lowercase `x.y` is a field access, handled as a postfix
                    // operator in `parse_call_expr`.

                    // A capitalised identifier that is not `Module.name` is a tag:
                    // `Ok(x)`, `Err(e)`, or a bare tag like `Red`.
                    if let Expr::Ident(name, _) = expr {
                        if name.starts_with(|c: char| c.is_uppercase()) {
                            return self.finish_tag(name);
                        }
                    }

                    self.skip_whitespace();
                    return Ok(expr);
                }
            }
        }

        // Fallback to string parsing for error message
        self.parse_string()
    }

    /// `Crypto.SHA256.hash`, `Crypto.SHA256.Hasher.empty`, `Crypto.SHA256.Digest.to_hex`
    /// and the BLAKE3 forms: a nested-module API the checker cannot resolve from a bare
    /// tag chain, so it is collapsed into a qualified builtin call whose module names
    /// the algorithm and the sub-namespace — `CryptoSha256`, `CryptoSha256Hasher`,
    /// `CryptoSha256Digest`. The cursor sits just past `Crypto`.
    fn parse_crypto_chain(&mut self) -> Option<Expr> {
        let start = self.pos;
        let seg = |this: &mut Self| -> Option<&'static str> {
            let rest = &this.input[this.pos..];
            if !rest.starts_with('.') { return None; }
            let name: String = rest[1..].chars().take_while(|c| is_ident_char(*c)).collect();
            if name.is_empty() { return None; }
            this.pos += 1 + name.len();
            Some(Box::leak(name.into_boxed_str()))
        };
        let algo = match seg(self) {
            Some("SHA256") => "Sha256",
            Some("BLAKE3") => "Blake3",
            _ => { self.pos = start; return None; }
        };
        let next = match seg(self) {
            Some(n) => n,
            None => { self.pos = start; return None; }
        };
        let (module, method): (String, &'static str) = match next {
            "Hasher" => (format!("{}Hasher", algo), seg(self).unwrap_or("")),
            "Digest" => (format!("{}Digest", algo), seg(self).unwrap_or("")),
            // A lowercase member of the algorithm itself — `hash`, `hash_chunks`.
            lower => (algo.to_string(), lower),
        };
        if method.is_empty() {
            self.pos = start;
            return None;
        }
        let module: &'static str = Box::leak(module.into_boxed_str());
        self.skip_whitespace();
        Some(Expr::Qualified { id: self.node(), module, name: method })
    }

    /// Parse `Module.name` after an uppercase identifier, if a `.name` follows.
    /// Is `name` a module this file imports? `KeyMod.Key.parse` then names the
    /// nominal `Key` of that module and its method.
    fn imported(&self, name: &str) -> bool {
        self.imports.iter().any(|(alias, module)| {
            alias == name || module == name || module.rsplit(['.', '/']).next() == Some(name)
        })
    }

    /// The parameters of each parameterised nominal, for the checker's recursive
    /// expansion: `ConsList(a)` is `[Nil, Cons(a, ConsList(a))]`, whatever `a` is.
    pub fn nominal_params(&self) -> &[(String, Vec<u32>)] {
        &self.nominal_params
    }

    fn try_parse_qualified(&mut self, module: &'static str) -> Option<Expr> {
        let rest = &self.input[self.pos..];
        if !rest.starts_with('.') || !rest[1..].starts_with(is_ident_start) {
            return None;
        }
        self.pos += 1; // Skip '.'
        let rest = &self.input[self.pos..];
        let (remaining, name_expr) = parse_identifier(rest).ok()?;
        self.pos += rest.len() - remaining.len();
        match name_expr {
            Expr::Ident(name, _) => {
                self.skip_whitespace();
                Some(Expr::Qualified { id: self.node(), module, name })
            }
            _ => None,
        }
    }

    /// Is the character immediately before the cursor whitespace?
    ///
    /// Used to tell function application (`f(1)`) from a grouped expression that
    /// merely follows something (`n > 0 (…)`). `parse_primary_expr` consumes
    /// trailing whitespace, so the distinction has to be recovered by looking back.
    ///
    /// ponytail: a look-behind, not a token stream. A real lexer would carry
    /// adjacency on each token and this would be a field check. Worth doing if more
    /// constructs come to depend on spacing.
    fn preceded_by_whitespace(&self) -> bool {
        self.input[..self.pos]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_whitespace())
    }

    /// Build the `match` that `name = value?` desugars to.
    ///
    /// ```text
    /// match value {
    ///     Ok(name) => continuation
    ///     Err(e) => Err(e)
    /// }
    /// ```
    ///
    /// ponytail: the error is bound to the fixed name `e`, so a `?` whose
    /// continuation refers to an outer `e` would see the error instead. A gensym
    /// would fix it; needs the parser to carry a counter, and no test has hit it.
    /// Build the `match` that `?` desugars to.
    ///
    /// `ok` is the pattern the unwrapped value is bound by — a plain name for
    /// `x = e?`, or a whole destructuring pattern for `{ a, b } = e?`.
    ///
    /// `mapper`, when given, is the lambda from `e ? |err| Replacement(err)`: the error
    /// is rewritten before it propagates, which is how a caller turns a builtin's
    /// error into one of its own.
    fn propagate_error_pattern(
        ok: Pattern,
        value: Expr,
        continuation: Expr,
        mapper: Option<Expr>,
    ) -> Expr {
        let propagated = match mapper {
            // `? MyError` — the mapper is the tag CONSTRUCTOR, so the error goes
            // inside it rather than being handed to it as a function.
            Some(Expr::Tag { name, args, .. }) if args.is_empty() => {
                Expr::Tag { id: crate::ast::fresh_node_unlocated(),
                    name: "Err",
                    args: vec![Expr::Tag { id: crate::ast::fresh_node_unlocated(),
                        name,
                        args: vec![Expr::Ident("e", crate::ast::fresh_node_unlocated())],
                    }],
                }
            }
            Some(map) => Expr::Tag { id: crate::ast::fresh_node_unlocated(),
                name: "Err",
                args: vec![Expr::Call { id: crate::ast::fresh_node_unlocated(),
                    func: Box::new(map),
                    args: vec![Expr::Ident("e", crate::ast::fresh_node_unlocated())],
                }],
            },
            None => Expr::Tag { id: crate::ast::fresh_node_unlocated(), name: "Err", args: vec![Expr::Ident("e", crate::ast::fresh_node_unlocated())] },
        };

        // `return`, not a bare `Err(e)`: `?` leaves the enclosing FUNCTION, and the
        // continuation it wraps is not always the function's body. Inside a `for` or
        // `while` the block's value is `{}`, so yielding `Err(e)` there is both a type
        // error and the wrong control flow — the loop would carry on. Builtin.roc
        // propagates from inside a loop (`$out = append($out, transform(item)?)`), and
        // so does `tests/roc/17_error_handling`.
        let propagated = Expr::Return(
            Box::new(propagated),
            crate::ast::fresh_node_unlocated(),
        );

        Expr::Match { id: crate::ast::fresh_node_unlocated(),
            scrutinee: Box::new(value),
            arms: vec![
                MatchArm { patterns: vec![ok], guard: None, body: continuation },
                MatchArm {
                    patterns: vec![Pattern::Tag {
                        name: "Err",
                        args: vec![Pattern::Binding("e")],
                    }],
                    guard: None,
                    body: propagated,
                },
            ],
        }
    }

    /// Parse `match scrutinee { pattern => body ... }`.
    ///
    /// Arms are separated by newlines; a comma between them is allowed but not
    /// required. Arms are kept in source order because matching stops at the first
    /// one that succeeds.
    fn parse_match(&mut self) -> Result<Expr, ParseError> {
        self.pos += 5; // Skip "match"
        self.skip_whitespace();

        // The scrutinee stops on its own at the `{`, since nothing in an expression
        // can continue into a brace.
        let scrutinee = Box::new(self.parse_or_expr()?);
        self.skip_whitespace();

        if !self.input[self.pos..].starts_with('{') {
            return Err(ParseError {
                message: "Expected '{' after the match scrutinee".to_string(),
                position: self.pos,
            });
        }
        self.pos += 1; // Skip '{'

        let mut arms = Vec::new();
        loop {
            self.skip_trivia();
            let rest = &self.input[self.pos..];
            if rest.is_empty() {
                return Err(ParseError {
                    message: "Unexpected end of input inside match; expected '}'".to_string(),
                    position: self.pos,
                });
            }
            if rest.starts_with('}') {
                self.pos += 1;
                self.skip_whitespace();
                break;
            }

            arms.push(self.parse_match_arm()?);

            // A comma between arms is optional.
            self.skip_whitespace();
            if self.input[self.pos..].starts_with(',') {
                self.pos += 1;
            }
        }

        if arms.is_empty() {
            return Err(ParseError {
                message: "A match needs at least one arm".to_string(),
                position: self.pos,
            });
        }

        Ok(Expr::Match { id: self.node(), scrutinee, arms })
    }

    /// Parse one arm: `A | B if guard => body`.
    fn parse_match_arm(&mut self) -> Result<MatchArm, ParseError> {
        let mut patterns = vec![self.parse_pattern()?];

        // Alternatives: `A | B | C`. Careful not to eat `||`.
        loop {
            self.skip_whitespace();
            let rest = &self.input[self.pos..];
            // `|>` is the pipeline operator, not an alternative separator.
            if rest.starts_with('|') && !rest.starts_with("||") && !rest.starts_with("|>") {
                self.pos += 1;
                self.skip_whitespace();
                patterns.push(self.parse_pattern()?);
            } else {
                break;
            }
        }

        // `pattern as name` binds the whole value as well.
        self.skip_whitespace();
        if starts_with_keyword(&self.input[self.pos..], "as") {
            self.pos += 2;
            self.skip_whitespace();
            let rest = &self.input[self.pos..];
            let (remaining, ident) = parse_identifier(rest)?;
            self.pos += rest.len() - remaining.len();
            let Expr::Ident(name, _) = ident else {
                return Err(ParseError { message: "Expected a name after `as`".to_string(), position: self.pos });
            };
            let inner = patterns.pop().expect("at least one pattern");
            patterns.push(Pattern::As { name, inner: Box::new(inner) });
        }

        // Optional guard, evaluated with the pattern's bindings in scope.
        self.skip_whitespace();
        let guard = if starts_with_keyword(&self.input[self.pos..], "if") {
            self.pos += 2;
            self.skip_whitespace();
            Some(self.parse_or_expr()?)
        } else {
            None
        };

        self.skip_whitespace();
        if !self.input[self.pos..].starts_with("=>") {
            return Err(ParseError {
                message: "Expected '=>' after a match pattern".to_string(),
                position: self.pos,
            });
        }
        self.pos += 2; // Skip "=>"
        self.skip_whitespace();

        // The arm's body is a block of one statement, so a `?` in it returns from
        // here — where the pattern's names are in scope — rather than from the
        // statement the whole `match` sits in.
        let tries_before = self.pending_tries.len();
        self.block_depth += 1;
        let parsed = self.parse_or_expr();
        self.block_depth -= 1;
        let body = parsed?;
        let tries = self.pending_tries.split_off(tries_before);
        let body = if tries.is_empty() { body } else { Self::lift_tries(tries, body) };
        Ok(MatchArm { patterns, guard, body })
    }

    /// Parse a single pattern.
    fn parse_pattern(&mut self) -> Result<Pattern, ParseError> {
        self.skip_whitespace();
        let rest = &self.input[self.pos..];

        // `_` is the wildcard. `_name` is an ordinary binding that documents being
        // unused, so only a bare `_` counts.
        if rest.starts_with('_') && !rest[1..].starts_with(is_ident_char) {
            self.pos += 1;
            self.skip_whitespace();
            return Ok(Pattern::Wildcard);
        }

        if rest.starts_with('(') {
            return self.parse_tuple_pattern();
        }

        if rest.starts_with('[') {
            return self.parse_list_pattern();
        }

        // A grapheme literal matches the NUMBER it denotes — roc has no character type,
        // so `'"' => …` is the pattern `34`. Builtin.roc's JSON scanner matches bytes
        // that way.
        if rest.starts_with('\'') {
            let literal = self.parse_grapheme_literal()?;
            self.skip_type_suffix();
            return match literal {
                Expr::Int(n, _) => Ok(Pattern::Int(n)),
                other => Err(ParseError {
                    message: format!("Expected a grapheme literal pattern, got {}", other),
                    position: self.pos,
                }),
            };
        }

        // A bare record pattern — `|{ x, y }|`, or a `match` arm. Previously only the
        // nominal spelling `Name.{ ... }` reached the record-pattern parser, so
        // destructuring a plain record in a parameter was a parse error.
        if rest.starts_with('{') {
            return self.parse_record_pattern();
        }

        if rest.starts_with('"') {
            let parsed = self.parse_string()?;
            self.skip_type_suffix();
            return match parsed {
                Expr::Str(s, _) => Ok(Pattern::Str(s)),
                // `"foo${name}bar"`: literal text matches itself, each `${name}`
                // captures up to the next literal, and `${_}` captures and drops.
                Expr::StrInterp(parts, _) => {
                    let mut prefix: &'static str = "";
                    let mut segments: Vec<(&'static str, &'static str)> = Vec::new();
                    for part in &parts {
                        match part {
                            StrPart::Literal(text) => match segments.last_mut() {
                                Some(last) => last.1 = text,
                                None => prefix = text,
                            },
                            StrPart::Expr(Expr::Ident(name, _)) => segments.push((name, "")),
                            StrPart::Expr(other) => {
                                return Err(ParseError {
                                    message: format!("A string pattern can only capture a name, not {}", other),
                                    position: self.pos,
                                })
                            }
                        }
                    }
                    Ok(Pattern::StrInterp { prefix, segments })
                }
                other => Err(ParseError {
                    message: format!("Only plain strings may be patterns, got {}", other),
                    position: self.pos,
                }),
            };
        }

        if let Ok((remaining, expr)) = parse_number_literal(rest) {
            self.pos += rest.len() - remaining.len();
            self.skip_type_suffix();
            self.skip_whitespace();
            return match expr {
                Expr::Int(n, _) => Ok(Pattern::Int(n)),
                Expr::Float(n, exact, _) => Ok(Pattern::Float(n, exact)),
                other => Err(ParseError {
                    message: format!("Unsupported numeric pattern: {}", other),
                    position: self.pos,
                }),
            };
        }

        if let Ok((remaining, ident)) = parse_identifier(rest) {
            self.pos += rest.len() - remaining.len();
            let name = match ident {
                Expr::Ident(n, _) => n,
                other => {
                    return Err(ParseError {
                        message: format!("Unsupported pattern: {}", other),
                        position: self.pos,
                    })
                }
            };

            // `Name.{ x, y }` destructures a nominal's backing record, and
            // `Name.Tag(p)` matches a nominal union's tag.
            if name.starts_with(|c: char| c.is_uppercase()) {
                if self.input[self.pos..].starts_with(".{") {
                    self.pos += 1; // Skip '.', leaving the '{'
                    return self.parse_record_pattern();
                }
                // `Name.(payload)` unwraps a nominal over a NON-record backing, the
                // pattern counterpart of the `Name.(x)` constructor. The nominal is
                // erased at runtime, so it matches as its payload does; it is kept in
                // the tree because the payload's TYPE is the backing, not the nominal.
                if self.input[self.pos..].starts_with(".(") {
                    self.pos += 2; // Skip '.('
                    let inner = self.parse_pattern()?;
                    self.skip_whitespace();
                    // `Pair.(a, b)` unwraps a tuple backing.
                    if self.input[self.pos..].starts_with(',') {
                        let mut items = vec![inner];
                        while self.input[self.pos..].starts_with(',') {
                            self.pos += 1;
                            self.skip_whitespace();
                            if self.input[self.pos..].starts_with(')') {
                                break;
                            }
                            items.push(self.parse_pattern()?);
                            self.skip_whitespace();
                        }
                        if self.input[self.pos..].starts_with(')') {
                            self.pos += 1;
                            self.skip_whitespace();
                        }
                        return Ok(Pattern::Nominal { name, inner: Box::new(Pattern::Tuple(items)) });
                    }
                    if !self.input[self.pos..].starts_with(')') {
                        return Err(ParseError {
                            message: format!("Expected ')' to close `{}.(`", name),
                            position: self.pos,
                        });
                    }
                    self.pos += 1;
                    self.skip_whitespace();
                    return Ok(Pattern::Nominal { name, inner: Box::new(inner) });
                }
                if self.input[self.pos..].starts_with('.')
                    && self.input[self.pos + 1..].starts_with(|c: char| c.is_uppercase())
                {
                    self.pos += 1; // Skip '.'
                    let rest = &self.input[self.pos..];
                    let (leftover, tag_ident) = parse_identifier(rest)?;
                    self.pos += rest.len() - leftover.len();
                    if let Expr::Ident(tag, _) = tag_ident {
                        return self.finish_tag_pattern(tag);
                    }
                }
            }

            // Capitalised means a tag, lowercase means a binding — the same rule the
            // expression parser uses to tell `Red` from a variable.
            if name.starts_with(|c: char| c.is_uppercase()) {
                return self.finish_tag_pattern(name);
            }

            self.skip_whitespace();
            return Ok(Pattern::Binding(name));
        }

        Err(ParseError {
            message: "Expected a pattern".to_string(),
            position: self.pos,
        })
    }

    /// Parse `if cond a else b`.
    ///
    /// The condition needs no parentheses and the branches no braces. There is no
    /// ambiguity about where the condition ends because Roc applies functions with
    /// `f(x)`, never by juxtaposition — so the expression parser stops on its own
    /// once the then-branch begins.
    ///
    /// `else` is OPTIONAL, and a missing one means `{}`. roc parses `if cond { … }`
    /// happily and gives it the type `{}` — using the result is then a type error
    /// ("The value's type, which does not have a method named from_numeral, is: {}"),
    /// not a parse error. That is what makes an else-less `if` usable as a statement,
    /// which is how `Builtin.roc` writes a guarded assignment inside a loop.
    ///
    /// `else if` is parsed by recursing, which builds an `If` whose `otherwise` is
    /// another `If` — there is no separate else-if node.
    fn parse_if(&mut self) -> Result<Expr, ParseError> {
        self.pos += 2; // Skip "if"
        self.skip_whitespace();

        let condition = Box::new(self.parse_or_expr()?);
        self.skip_whitespace();

        let then_branch = Box::new(self.parse_or_expr()?);
        self.skip_whitespace();

        if !starts_with_keyword(&self.input[self.pos..], "else") {
            let otherwise = Box::new(Expr::Unit(self.node()));
            return Ok(Expr::If { id: self.node(), condition, then_branch, otherwise });
        }
        self.pos += 4; // Skip "else"
        self.skip_whitespace();

        // `else if ...` recurses; anything else is an ordinary expression.
        let otherwise = if starts_with_keyword(&self.input[self.pos..], "if") {
            Box::new(self.parse_if()?)
        } else {
            Box::new(self.parse_or_expr()?)
        };

        Ok(Expr::If { id: self.node(), condition, then_branch, otherwise })
    }

    /// Parse a list literal: `[1, 2, 3]`, `[]`. A trailing comma is allowed.
    fn parse_list(&mut self) -> Result<Expr, ParseError> {
        self.pos += 1; // Skip '['
        let mut items = Vec::new();

        loop {
            self.skip_whitespace();
            let rest = &self.input[self.pos..];
            if rest.is_empty() {
                return Err(ParseError {
                    message: "Unexpected end of input in list literal".to_string(),
                    position: self.pos,
                });
            }
            if rest.starts_with(']') {
                self.pos += 1;
                self.skip_whitespace();
                break;
            }

            items.push(self.parse_or_expr()?);
            self.skip_whitespace();
            let rest = &self.input[self.pos..];
            if rest.starts_with(',') {
                self.pos += 1;
            } else if !rest.starts_with(']') {
                return Err(ParseError {
                    message: "Expected ',' or ']' in list literal".to_string(),
                    position: self.pos,
                });
            }
        }

        Ok(Expr::List(items, self.node()))
    }

    /// Build a top-level `(a, b) = value` binding.
    ///
    /// Uses indexing rather than a `match`, because a top-level binding must NOT be
    /// scoped: the continuation is the rest of the file, which defines things the
    /// runner looks up afterwards (`main!` among them), and a match arm pops its
    /// scope on the way out. Inside a block a match is correct and is what is used.
    ///
    /// ```text
    /// (a, b) = pair      ->   __destructure = pair
    /// <rest>                  a = __destructure.0
    ///                         b = __destructure.1
    ///                         <rest>
    /// ```
    ///
    /// Ceiling: only bindings and wildcards are accepted here. roc also allows a
    /// refutable element (`(1, b) = pair`); that needs a match, so it is rejected
    /// with a pointer to the block form rather than silently skipping the check.
    fn destructure_at_top_level(
        &mut self,
        pattern: Pattern,
        value: Expr,
        body: Expr,
    ) -> Result<Expr, ParseError> {
        // A record destructuring binds by FIELD NAME rather than position, but is
        // otherwise the same shape, so both lower to indexing into one temporary.
        let items: Vec<(Accessor, Pattern)> = match pattern {
            Pattern::Tuple(items) => items
                .into_iter()
                .enumerate()
                .map(|(index, p)| (Accessor::Index(index), p))
                .collect(),
            Pattern::Record { fields, rest } => {
                // A bare `..` names nothing, so there is nothing to build.
                if rest.is_some_and(|name| name != "_") {
                    return Err(ParseError {
                        message: "`..rest` is not supported in a TOP-LEVEL destructuring: \
                                  building the remaining record needs a match, which a \
                                  top-level binding cannot use. Destructure inside a block."
                            .to_string(),
                        position: self.pos,
                    });
                }
                fields
                    .into_iter()
                    .map(|(name, p)| (Accessor::Field(name), p))
                    .collect()
            }
            other => {
                return Err(ParseError {
                    message: format!("Unsupported top-level destructuring pattern: {}", other),
                    position: self.pos,
                })
            }
        };

        // Bind the value once; the element bindings then index into it.
        const TEMP: &str = "__destructure";
        let mut chain = body;
        for (accessor, item) in items.iter().rev() {
            let name = match item {
                Pattern::Binding(name) => *name,
                // Nothing to bind, so the element can be skipped entirely.
                Pattern::Wildcard => continue,
                other => {
                    return Err(ParseError {
                        message: format!(
                            "Only names and `_` are supported in a top-level \
                             destructuring; `{}` needs a match inside a block",
                            other
                        ),
                        position: self.pos,
                    })
                }
            };
            let value = match accessor {
                Accessor::Index(index) => Expr::TupleIndex { id: crate::ast::fresh_node_unlocated(),
                    tuple: Box::new(Expr::Ident(TEMP, crate::ast::fresh_node_unlocated())),
                    index: *index,
                },
                Accessor::Field(field) => Expr::FieldAccess { id: crate::ast::fresh_node_unlocated(),
                    record: Box::new(Expr::Ident(TEMP, crate::ast::fresh_node_unlocated())),
                    field,
                },
            };
            chain = Expr::Let { id: crate::ast::fresh_node_unlocated(),
                name,
                annotation: None,
                value: Box::new(value),
                body: Box::new(chain),
            };
        }

        Ok(Expr::Let { id: crate::ast::fresh_node_unlocated(),
            name: TEMP,
            annotation: None,
            value: Box::new(value),
            body: Box::new(chain),
        })
    }

    /// Finish a tag pattern whose name has been consumed, reading any payload.
    ///
    /// Shared by bare tags (`Foo(a, b)`) and nominal-qualified ones
    /// (`Animal.Dog(name)`), which match the same value.
    fn finish_tag_pattern(&mut self, name: &'static str) -> Result<Pattern, ParseError> {
        let mut args = Vec::new();
        // Payload patterns nest: `Wrap(Inner(s))`.
        if self.input[self.pos..].starts_with('(') {
            self.pos += 1;
            loop {
                self.skip_whitespace();
                if self.input[self.pos..].starts_with(')') {
                    self.pos += 1;
                    break;
                }
                args.push(self.parse_pattern()?);
                self.skip_whitespace();
                let rest = &self.input[self.pos..];
                if rest.starts_with(',') {
                    self.pos += 1;
                } else if rest.starts_with(')') {
                    self.pos += 1;
                    break;
                } else {
                    return Err(ParseError {
                        message: "Expected ',' or ')' in a tag pattern".to_string(),
                        position: self.pos,
                    });
                }
            }
        }
        self.skip_whitespace();
        Ok(Pattern::Tag { name, args })
    }

    /// Parse a record pattern: `{ x, y }` or `{ x: 0, y }`.
    ///
    /// Reached through a nominal destructuring (`Point.{ x }`). A bare field name is
    /// shorthand for binding it to itself.
    fn parse_record_pattern(&mut self) -> Result<Pattern, ParseError> {
        self.pos += 1; // Skip '{'
        let mut fields = Vec::new();
        let mut rest_binding: Option<&'static str> = None;

        loop {
            self.skip_whitespace();
            let rest = &self.input[self.pos..];
            if rest.is_empty() {
                return Err(ParseError {
                    message: "Unexpected end of input in record pattern".to_string(),
                    position: self.pos,
                });
            }
            if rest.starts_with('}') {
                self.pos += 1;
                self.skip_whitespace();
                break;
            }

            // `..rest` binds every field the pattern does not name, which is how a
            // field gets removed: name it `_`, capture the rest.
            //
            // A BARE `..` says only "and whatever else is in there", which is how
            // Builtin.roc writes most of its record patterns
            // (`Found({ data, entry_index, .. })`). It binds the remainder to the
            // throwaway name rather than to nothing, because `rest` is also what tells
            // the checker the record is open — see `pattern_type`.
            if rest.starts_with("..") {
                self.pos += 2;
                self.skip_whitespace();
                let rest = &self.input[self.pos..];
                match parse_identifier(rest) {
                    Ok((leftover, Expr::Ident(name, _))) => {
                        self.pos += rest.len() - leftover.len();
                        rest_binding = Some(name);
                    }
                    _ => rest_binding = Some("_"),
                }
                self.skip_whitespace();
                if self.input[self.pos..].starts_with(',') {
                    self.pos += 1;
                }
                continue;
            }

            let (leftover, ident) = parse_identifier(rest)?;
            self.pos += rest.len() - leftover.len();
            let field = match ident {
                Expr::Ident(n, _) => n,
                other => {
                    return Err(ParseError {
                        message: format!("Expected a field name in a record pattern, got {}", other),
                        position: self.pos,
                    })
                }
            };

            self.skip_whitespace();
            let pattern = if self.input[self.pos..].starts_with(':') {
                self.pos += 1;
                self.parse_pattern()?
            } else {
                // `{ x }` means `{ x: x }`.
                Pattern::Binding(field)
            };
            fields.push((field, pattern));

            self.skip_whitespace();
            let rest = &self.input[self.pos..];
            if rest.starts_with(',') {
                self.pos += 1;
            } else if !rest.starts_with('}') {
                return Err(ParseError {
                    message: "Expected ',' or '}' in a record pattern".to_string(),
                    position: self.pos,
                });
            }
        }

        Ok(Pattern::Record { fields, rest: rest_binding })
    }

    /// Parse a tuple pattern: `(0, 0)`, `(x, 0)`. Fixed arity, element-wise.
    fn parse_tuple_pattern(&mut self) -> Result<Pattern, ParseError> {
        self.pos += 1; // Skip '('
        let mut items = Vec::new();

        loop {
            self.skip_whitespace();
            let rest = &self.input[self.pos..];
            if rest.is_empty() {
                return Err(ParseError {
                    message: "Unexpected end of input in tuple pattern".to_string(),
                    position: self.pos,
                });
            }
            if rest.starts_with(')') {
                self.pos += 1;
                self.skip_whitespace();
                break;
            }

            items.push(self.parse_pattern()?);
            self.skip_whitespace();
            let rest = &self.input[self.pos..];
            if rest.starts_with(',') {
                self.pos += 1;
            } else if !rest.starts_with(')') {
                return Err(ParseError {
                    message: "Expected ',' or ')' in tuple pattern".to_string(),
                    position: self.pos,
                });
            }
        }

        Ok(Pattern::Tuple(items))
    }

    /// Parse a list pattern: `[]`, `[a, b]`, `[1, 2, ..]`, `[2, .., 1]`,
    /// `[9, .. as tail]`.
    ///
    /// At most one `..`. Patterns before it match from the front, patterns after it
    /// from the back, and `.. as name` binds the skipped middle as a list.
    fn parse_list_pattern(&mut self) -> Result<Pattern, ParseError> {
        self.pos += 1; // Skip '['
        let mut before = Vec::new();
        let mut after = Vec::new();
        let mut rest_binding: Option<Option<&'static str>> = None;

        loop {
            self.skip_whitespace();
            let remaining = &self.input[self.pos..];
            if remaining.is_empty() {
                return Err(ParseError {
                    message: "Unexpected end of input in list pattern".to_string(),
                    position: self.pos,
                });
            }
            if remaining.starts_with(']') {
                self.pos += 1;
                self.skip_whitespace();
                break;
            }

            // `..` or `.. as name`. Checked before the element parser, since `.` is
            // not the start of any pattern.
            if remaining.starts_with("..") {
                if rest_binding.is_some() {
                    return Err(ParseError {
                        message: "A list pattern may contain at most one `..`".to_string(),
                        position: self.pos,
                    });
                }
                self.pos += 2;
                self.skip_whitespace();

                let mut name = None;
                if starts_with_keyword(&self.input[self.pos..], "as") {
                    self.pos += 2;
                    self.skip_whitespace();
                    let rest = &self.input[self.pos..];
                    let (leftover, ident) = parse_identifier(rest)?;
                    self.pos += rest.len() - leftover.len();
                    match ident {
                        Expr::Ident(n, _) => name = Some(n),
                        other => {
                            return Err(ParseError {
                                message: format!("Expected a name after `.. as`, got {}", other),
                                position: self.pos,
                            })
                        }
                    }
                }
                rest_binding = Some(name);
            } else {
                let pattern = self.parse_pattern()?;
                if rest_binding.is_some() {
                    after.push(pattern);
                } else {
                    before.push(pattern);
                }
            }

            self.skip_whitespace();
            let remaining = &self.input[self.pos..];
            if remaining.starts_with(',') {
                self.pos += 1;
            } else if !remaining.starts_with(']') {
                return Err(ParseError {
                    message: "Expected ',' or ']' in list pattern".to_string(),
                    position: self.pos,
                });
            }
        }

        Ok(Pattern::List { before, rest: rest_binding, after })
    }

    /// Parse a parenthesised argument list, cursor on the `(`.
    ///
    /// Shared by ordinary calls and by static dispatch, which differ only in what they
    /// do with the result.
    fn parse_call_arguments(&mut self) -> Result<Vec<Expr>, ParseError> {
        self.pos += 1; // Skip '('
        self.skip_whitespace();
        let mut args = Vec::new();

        if self.input[self.pos..].starts_with(')') {
            self.pos += 1;
            return Ok(args);
        }

        loop {
            args.push(self.parse_or_expr()?);
            self.skip_whitespace();
            let rest = &self.input[self.pos..];
            if rest.starts_with(',') {
                self.pos += 1;
                self.skip_whitespace();
                // A trailing comma before `)` is legal, and is how roc writes a call
                // whose arguments are spread over several lines.
                if self.input[self.pos..].starts_with(')') {
                    self.pos += 1;
                    break;
                }
            } else if rest.starts_with(')') {
                self.pos += 1;
                break;
            } else {
                return Err(ParseError {
                    message: "Expected ',' or ')' after function arguments".to_string(),
                    position: self.pos,
                });
            }
        }
        Ok(args)
    }

    /// Parse `for name in iterable { body }`.
    ///
    /// The loop's value is `{}` — it is a statement, not something you bind.
    fn parse_for(&mut self) -> Result<Expr, ParseError> {
        self.pos += 3; // Skip "for"
        self.skip_whitespace();

        // The loop variable is a PATTERN, not just a name: `for (key, value) in dict`
        // is how Builtin.roc walks a Dict's entries. Only a plain binding can be the
        // loop variable itself, so anything else iterates over a fresh name and
        // destructures it in the body — the same shape the desugarer already gives
        // `(a, b) = pair`.
        let pattern = self.parse_pattern()?;
        let (name, destructure) = match pattern {
            Pattern::Binding(n) => (n, None),
            other => {
                let fresh: &'static str =
                    Box::leak(format!("#for{}", self.pos).into_boxed_str());
                (fresh, Some(other))
            }
        };

        self.skip_whitespace();
        if !starts_with_keyword(&self.input[self.pos..], "in") {
            return Err(ParseError {
                message: "Expected `in` after the `for` loop variable".to_string(),
                position: self.pos,
            });
        }
        self.pos += 2;
        self.skip_whitespace();

        // The iterable stops at the `{`, since nothing in an expression continues
        // into a brace.
        let iterable = Box::new(self.parse_or_expr()?);
        self.skip_whitespace();

        // `for x in xs if cond { … }` — a filtered loop. The body runs only for the
        // items that satisfy the guard, so it is exactly the body wrapped in an
        // else-less `if`. Verified against the real compiler: `for x in [1,2,3] if
        // x > 1 { $t = $t + x }` leaves `$t` at 5.
        let mut guard = None;
        if starts_with_keyword(&self.input[self.pos..], "if") {
            self.pos += 2;
            self.skip_whitespace();
            guard = Some(self.parse_or_expr()?);
            self.skip_whitespace();
        }

        if !self.input[self.pos..].starts_with('{') {
            return Err(ParseError {
                message: "Expected '{' to open the `for` loop body".to_string(),
                position: self.pos,
            });
        }
        let mut body = Box::new(self.parse_block()?);
        if let Some(condition) = guard {
            body = Box::new(Expr::If { id: self.node(),
                condition: Box::new(condition),
                then_branch: body,
                otherwise: Box::new(Expr::Unit(self.node())),
            });
        }
        if let Some(pattern) = destructure {
            body = Box::new(Expr::Match { id: self.node(),
                scrutinee: Box::new(Expr::Ident(name, self.node())),
                arms: vec![crate::ast::MatchArm {
                    patterns: vec![pattern],
                    guard: None,
                    body: *body,
                }],
            });
        }

        Ok(Expr::For { id: self.node(), name, iterable, body })
    }

    /// Parse `while condition { body }`. Its value is `{}`.
    fn parse_while(&mut self) -> Result<Expr, ParseError> {
        self.pos += 5; // Skip "while"
        self.skip_whitespace();

        let condition = Box::new(self.parse_or_expr()?);
        self.skip_whitespace();
        if !self.input[self.pos..].starts_with('{') {
            return Err(ParseError {
                message: "Expected '{' to open the `while` loop body".to_string(),
                position: self.pos,
            });
        }
        let body = Box::new(self.parse_block()?);

        Ok(Expr::While { id: self.node(), condition, body })
    }

    /// Parse whatever the `{` at the cursor opens: a record literal or a block.
    ///
    /// Every site that accepts a braced construct must go through here. A lambda
    /// body used to call `parse_block` directly, so `|n| { v: n }` parsed the record
    /// as a block and failed on `v: n`.
    /// The braces of `Name.{ … }`: a record however they read, so `Key.{ raw }` is
    /// the pun `{ raw: raw }` and not a block whose value is `raw`.
    fn parse_nominal_braced(&mut self) -> Result<Expr, ParseError> {
        if self.looks_like_record(true) {
            self.parse_record()
        } else {
            self.parse_block()
        }
    }

    fn parse_braced(&mut self) -> Result<Expr, ParseError> {
        if self.looks_like_record(false) {
            self.parse_record()
        } else {
            self.parse_block()
        }
    }

    /// Does the `{` at the cursor open a record literal rather than a block?
    ///
    /// The discriminator is the same one the annotation skipper uses: a record field
    /// is `name: value` with **no** space before the colon, while a block statement
    /// that happens to be annotated is `name : Type`. A block statement otherwise
    /// starts with `name =`, or with something that is not an identifier at all.
    fn looks_like_record(&self, stmt_head: bool) -> bool {
        let rest = &self.input[self.pos + 1..]; // past the '{'
        // A record written one field per line often opens with a comment, so the first
        // thing after the `{` is not necessarily the first field.
        let mut trimmed = rest.trim_start();
        while trimmed.starts_with('#') {
            trimmed = match trimmed.find('\n') {
                Some(i) => trimmed[i..].trim_start(),
                None => "",
            };
        }

        // `{}` is the unit value, handled by parse_block.
        if trimmed.starts_with('}') {
            return false;
        }
        // `{ ..base, field: value }` is a record UPDATE, not a block.
        if trimmed.starts_with("..") {
            return true;
        }
        let ident_len = trimmed
            .char_indices()
            .take_while(|(i, c)| {
                if *i == 0 { is_ident_start(*c) } else { is_ident_char(*c) }
            })
            .count();
        if ident_len == 0 {
            return false;
        }
        let after = &trimmed[ident_len..];
        // No whitespace allowed between the field name and its colon.
        if after.starts_with(':') && !after.starts_with("::") {
            return true;
        }
        // `{ id : 7, balance : 99 }` — roc allows a space before the colon, and both
        // `Builtin.roc` and the eval tests write it. A BLOCK also opens `x : T`, so
        // the two are told apart by two things: an ANNOTATION is followed by the
        // binding it annotates (`f : … <newline> f = …`), and a RECORD's first field
        // ends at a comma on the same line. A function-type annotation has top-level
        // commas of its own — `f : List(a), b -> c` — so the rebind test comes first.
        let spaced = after.trim_start();
        if after.len() != spaced.len() && spaced.starts_with(':') && !spaced.starts_with("::") {
            let field = &trimmed[..ident_len];
            if let Some(nl) = spaced.find('\n') {
                let mut next = spaced[nl + 1..].trim_start();
                while next.starts_with('#') {
                    next = match next.find('\n') {
                        Some(i) => next[i..].trim_start(),
                        None => "",
                    };
                }
                if let Some(rest) = next.strip_prefix(field) {
                    let rest = rest.trim_start();
                    if rest.starts_with('=') && !rest.starts_with("==") && !rest.starts_with("=>") {
                        return false;
                    }
                }
            }
            let mut depth = 0i32;
            for c in spaced[1..].chars() {
                match c {
                    '(' | '[' | '{' => depth += 1,
                    ')' | ']' => depth -= 1,
                    '}' if depth == 0 => return false,
                    '}' => depth -= 1,
                    ',' if depth == 0 => return true,
                    '\n' => return false,
                    _ => {}
                }
            }
            return false;
        }
        // Field punning: `{ name, age }` is `{ name: name, age: age }`. A COMMA is
        // what marks it, EXCEPT at a block's statement or tail position, where roc
        // reads a lone `{ x }` as a one-field record — `|fun| { { fun } }` is how
        // `Builtin.roc`-style code builds a single-field wrapper. Anywhere else a lone
        // `{ x }` is a block whose value is `x`.
        let rest_after = after.trim_start();
        rest_after.starts_with(',') || (stmt_head && rest_after.starts_with('}'))
    }

    /// Parse a record literal: `{ x: 1, y: f(2) }`.
    fn parse_record(&mut self) -> Result<Expr, ParseError> {
        self.pos += 1; // Skip '{'
        let mut fields = Vec::new();
        // `{ ..base, field: value }` is an UPDATE. roc rejects `{ base & field: v }`,
        // so this spelling is the only one.
        let mut base: Option<Expr> = None;

        loop {
            self.skip_whitespace();
            let rest = &self.input[self.pos..];
            if rest.starts_with("..") {
                self.pos += 2;
                self.skip_whitespace();
                base = Some(self.parse_or_expr()?);
                self.skip_whitespace();
                if self.input[self.pos..].starts_with(',') {
                    self.pos += 1;
                }
                continue;
            }
            if rest.starts_with('}') {
                self.pos += 1;
                self.skip_whitespace();
                break;
            }

            let (remaining, ident) = parse_identifier(rest)?;
            let name = match ident {
                Expr::Ident(n, _) => n,
                _ => {
                    return Err(ParseError {
                        message: "Expected a field name in record literal".to_string(),
                        position: self.pos,
                    })
                }
            };
            self.pos += rest.len() - remaining.len();
            self.skip_inline_whitespace();

            // Field punning: `{ name, birth_year }` is `{ name: name, ... }`. The
            // field takes the value of the binding that shares its name.
            if !self.input[self.pos..].starts_with(':') {
                let next = self.input[self.pos..].chars().next();
                if matches!(next, Some(',') | Some('}')) || next.is_none() {
                    fields.push((name, Expr::Ident(name, self.node())));
                    self.skip_whitespace();
                    if self.input[self.pos..].starts_with(',') {
                        self.pos += 1;
                        continue;
                    }
                    if self.input[self.pos..].starts_with('}') {
                        self.pos += 1;
                        self.skip_whitespace();
                        break;
                    }
                    continue;
                }
                return Err(ParseError {
                    message: format!("Expected ':' after record field '{}'", name),
                    position: self.pos,
                });
            }
            self.pos += 1; // Skip ':'
            self.skip_whitespace();

            // Field values are full expressions.
            fields.push((name, self.parse_or_expr()?));

            self.skip_whitespace();
            let rest = &self.input[self.pos..];
            if rest.starts_with(',') {
                self.pos += 1; // A trailing comma before `}` is legal.
            } else if !rest.starts_with('}') {
                return Err(ParseError {
                    message: "Expected ',' or '}' in record literal".to_string(),
                    position: self.pos,
                });
            }
        }

        Ok(match base {
            Some(base) => Expr::RecordUpdate { id: self.node(), base: Box::new(base), fields },
            None => Expr::Record(fields, self.node()),
        })
    }

    /// Parse a block `{ stmt \n stmt \n expr }` into nested `Let`s.
    ///
    /// Roc blocks are a sequence of statements ending in an expression. Rather than
    /// add a `Block` AST node, each statement becomes a `Let`: `name = value`
    /// binds `name`, and a bare statement (typically an effect call like
    /// `echo!("hi")`) binds the throwaway name `_`. The final expression is the
    /// innermost body, so the block's value is the last expression's value.
    ///
    /// `{}` is the empty record, Roc's unit value.
    fn parse_block(&mut self) -> Result<Expr, ParseError> {
        self.pos += 1; // Skip '{'
        self.skip_whitespace();

        // A block starts a fresh statement context: this one may be nested inside a
        // statement of an enclosing block — a lambda body in a call argument — and a
        // `?` in ITS first statement must be lifted to ITS statement, not the caller's.
        let enclosing_tries = std::mem::take(&mut self.pending_tries);
        self.block_depth += 1;
        let parsed = self.parse_block_inner();
        self.block_depth -= 1;
        self.pending_tries = enclosing_tries;
        parsed
    }

    fn parse_block_inner(&mut self) -> Result<Expr, ParseError> {

        if self.input[self.pos..].starts_with('}') {
            self.pos += 1;
            self.skip_whitespace();
            return Ok(Expr::Unit(self.node()));
        }

        // One per statement; the last is the block's result.
        // `(target, annotation, value, propagates, error mapper, lifted `?` sites)`.
        type Stmt =
            (BindTarget, Option<Type>, Expr, bool, Option<Expr>, Vec<(&'static str, Expr, Option<Expr>)>);
        let mut stmts: Vec<Stmt> = Vec::new();

        loop {
            // Blocks carry annotations and comments too (`add5 : I64 -> I64`), so the
            // same trivia rules apply here as at the top level.
            self.skip_trivia();

            // A nominal declared in THIS block brings its methods with it, bound where
            // it stands so they can capture what is in scope. Ordered by dependency:
            // roc lets a member name a later sibling (`first = second`), and these are
            // ordinary sequential bindings.
            if self.local_methods.iter().any(|(d, ..)| *d == self.block_depth) {
                let depth = self.block_depth;
                let mut mine: Vec<(u32, &'static str, Option<Type>, Expr)> = Vec::new();
                self.local_methods.retain(|entry| {
                    if entry.0 == depth { mine.push(entry.clone()); false } else { true }
                });
                for (_, name, annotation, value) in Self::order_by_dependency(mine) {
                    stmts.push((BindTarget::Name(name), annotation, value, false, None, Vec::new()));
                }
            }

            let rest = &self.input[self.pos..];

            if rest.is_empty() {
                return Err(ParseError {
                    message: "Unexpected end of input inside block; expected '}'".to_string(),
                    position: self.pos,
                });
            }
            if rest.starts_with('}') {
                self.pos += 1;
                self.skip_whitespace();
                break;
            }

            // `var` is a binding form, so it has to be handled before the ordinary
            // binding check — otherwise `var x` reads as the name `var`. Loops and
            // `break` need no special case here: they are expressions, handled by
            // `parse_primary_expr`, and a statement position accepts any expression.
            if starts_with_keyword(rest, "var") {
                self.pos += 3;
                self.skip_whitespace();
                let rest = &self.input[self.pos..];
                let (remaining, ident) = parse_identifier(rest)?;
                self.pos += rest.len() - remaining.len();
                let name = match ident {
                    Expr::Ident(n, _) => n,
                    other => {
                        return Err(ParseError {
                            message: format!("Expected a name after `var`, got {}", other),
                            position: self.pos,
                        })
                    }
                };
                if !self.mutable_names.iter().any(|n| n == name) {
                    self.mutable_names.push(name.to_string());
                }
                self.skip_whitespace();
                if !self.input[self.pos..].starts_with('=') {
                    return Err(ParseError {
                        message: format!("Expected '=' after `var {}`", name),
                        position: self.pos,
                    });
                }
                self.pos += 1;
                self.skip_whitespace();
                let value = self.parse_or_expr()?;
                stmts.push((
                    BindTarget::Var(name),
                    None,
                    value,
                    false,
                    None,
                    std::mem::take(&mut self.pending_tries),
                ));
                continue;
            }

            // `name = value`, or a destructuring `(a, b) = value`.
            let mut bound: Option<BindTarget> = None;
            let mut stmt_annotation: Option<Type> = None;

            // Destructuring: try to read a pattern followed by `=`. A statement may
            // legitimately START with `(` as a grouped expression, so this backtracks
            // rather than committing.
            // Copied out so `rest`'s borrow ends before the mutable parse calls below.
            // A record destructuring `{ x, y } = r` is the same shape as a tuple one.
            let opens_paren = rest.starts_with('(');
            let opens_brace = rest.starts_with('{');
            // `Ok(encoded) = expr` destructures a TAG, which is how Builtin.roc unwraps
            // a Try it has already proved cannot fail. It starts with a capital, and so
            // does an ordinary statement like `Str.inspect(x)` — the backtracking below
            // is what tells them apart, on whether an `=` follows.
            let opens_tag = rest.starts_with(char::is_uppercase);
            if opens_paren || opens_brace || opens_tag {
                let saved = self.pos;
                let parsed = if opens_paren {
                    self.parse_tuple_pattern()
                } else if opens_brace {
                    self.parse_record_pattern()
                } else {
                    self.parse_pattern()
                };
                if let Ok(pattern) = parsed {
                    self.skip_whitespace();
                    let after = &self.input[self.pos..];
                    if after.starts_with('=') && !after.starts_with("==") && !after.starts_with("=>")
                    {
                        self.pos += 1; // Skip '='
                        self.skip_whitespace();
                        bound = Some(BindTarget::Destructure(pattern));
                    } else {
                        self.pos = saved;
                    }
                } else {
                    self.pos = saved;
                }
            }

            if bound.is_none() {
                let rest = &self.input[self.pos..];
                if let Ok((remaining, ident)) = parse_identifier(rest) {
                    let consumed = rest.len() - remaining.len();
                    let after = rest[consumed..].trim_start();
                    if after.starts_with('=')
                        && !after.starts_with("==")
                        && !after.starts_with("=>")
                    {
                        if let Expr::Ident(name, _) = ident {
                            self.pos += consumed;
                            self.skip_whitespace();
                            self.pos += 1; // Skip '='
                            self.skip_whitespace();
                            // A name declared by `var` is mutable, so `x = e`
                            // REASSIGNS rather than shadows. Shadowing inside a loop
                            // body would be discarded when its scope pops. `$` carries
                            // no meaning of its own — it is an ordinary identifier
                            // character — but `var $sum` lands in the same set.
                            bound = Some(if self.mutable_names.iter().any(|n| n == name) {
                                BindTarget::Assign(name)
                            } else {
                                BindTarget::Name(name)
                            });
                            stmt_annotation = self.claim_annotation(name);
                        }
                    }
                }
            }

            // Only a BARE expression statement (or the block's tail) is where roc
            // reads a lone `{ x }` as a record; a binding's value is not.
            self.stmt_head = bound.is_none();
            let mut value = self.parse_or_expr()?;
            self.stmt_head = false;

            // `parse_call_expr` lifts every `?` it sees, including one that covers the
            // statement's whole value — which shows up here as the value being nothing
            // but the fresh name of the try just pushed. Take that one back: the
            // statement form binds the unwrapped value directly, with no extra `let`,
            // and it is the form that must still reject `?` on a block's final
            // expression. Any remaining tries were nested inside the value and stay
            // lifted.
            let mut propagates = false;
            let mut mapper = None;
            if let Expr::Ident(name, _) = &value {
                if self.pending_tries.last().is_some_and(|(fresh, ..)| fresh == name) {
                    let (_, scrutinee, lifted) =
                        self.pending_tries.pop().expect("checked above");
                    value = scrutinee;
                    mapper = lifted;
                    propagates = true;
                }
            }

            stmts.push((
                bound.unwrap_or(BindTarget::Name("_")),
                stmt_annotation,
                value,
                propagates,
                mapper,
                std::mem::take(&mut self.pending_tries),
            ));
        }

        // Mutual recursion between block-local bindings: roc supports it only at the
        // TOP level, and a block's bindings run in order, so naming a later sibling
        // has nothing to name. A nominal's method block is a recursive group by roc's
        // own rules, and `order_by_dependency` has already put its members in order,
        // so a qualified `Type.member` is left alone.
        for (i, (target, _, value, ..)) in stmts.iter().enumerate() {
            if !matches!(target, BindTarget::Name(n) if !n.contains('.')) {
                continue;
            }
            for (later, ..) in stmts[i + 1..].iter() {
                let BindTarget::Name(name) = later else { continue };
                if name.contains('.') || *name == "_" {
                    continue;
                }
                if Self::mentions_free(value, name) {
                    self.type_problems.push(format!(
                        "`{}` is used before it is defined; roc supports mutual \
                         recursion only between top-level definitions",
                        name
                    ));
                }
            }
        }

        // Fold from the end: every statement wraps the one after it, so the
        // "rest of the block" is whatever has been folded so far.
        let (
            result_target,
            _result_annotation,
            result,
            result_propagates,
            result_mapper,
            result_tries,
        ) = stmts.pop().ok_or_else(|| ParseError {
            message: "Empty block body".to_string(),
            position: self.pos,
        })?;
        // Only when the last statement IS the block's value. An assignment or a `var`
        // still evaluates to `{}` whatever its right-hand side does, so
        // `for x in xs { $s = step($s, x)? }` has a perfectly good continuation — the
        // implicit `{}` — and roc accepts it. Builtin.roc's `fold_try` is exactly that.
        if result_propagates && matches!(result_target, BindTarget::Name(_)) {
            // `?` unwraps, so a block ending in `expr?` evaluates to the unwrapped
            // value rather than a Try. roc rejects that against a Try return type,
            // and there is no continuation to put in the Ok arm.
            return Err(ParseError {
                message: "`?` cannot be used on the final expression of a block: it                           unwraps the value, leaving nothing to propagate into"
                    .to_string(),
                position: self.pos,
            });
        }
        // A propagating assignment assigns the UNWRAPPED value, so it is lifted like
        // any nested `?`: bind the `Ok` to a fresh name and assign that. Appending it
        // last puts it innermost, inside any `?` that its own right-hand side used.
        let (result, result_tries) = if result_propagates {
            let mut tries = result_tries;
            let fresh: &'static str =
                Box::leak(format!("#try{}", tries.len()).into_boxed_str());
            tries.push((fresh, result, result_mapper));
            (Expr::Ident(fresh, self.node()), tries)
        } else {
            (result, result_tries)
        };

        // A block's value is its LAST expression — but if that last statement is an
        // assignment or a `var`, it still has to run, and the block's value is unit.
        // Dropping the target here silently discarded the mutation, so
        // `for n in xs { $sum = $sum + n }` became `for n in xs { $sum + n }` and the
        // loop did nothing.
        let mut body = match result_target {
            BindTarget::Assign(name) => Expr::Assign { id: self.node(),
                name,
                value: Box::new(result),
                body: Box::new(Expr::Unit(self.node())),
            },
            BindTarget::Var(name) => Expr::VarDecl { id: self.node(),
                name,
                value: Box::new(result),
                body: Box::new(Expr::Unit(self.node())),
            },
            BindTarget::Destructure(pattern) => Expr::Match { id: self.node(),
                scrutinee: Box::new(result),
                arms: vec![MatchArm {
                    patterns: vec![pattern],
                    guard: None,
                    body: Expr::Unit(self.node()),
                }],
            },
            BindTarget::Name(_) => result,
        };
        body = Self::lift_tries(result_tries, body);
        while let Some((target, annotation, value, propagates, mapper, tries)) = stmts.pop() {
            // A destructuring binding is a one-arm match: `(a, b) = v` then the rest
            // is exactly `match v { (a, b) => rest }`. No new AST node needed.
            if let BindTarget::Destructure(pattern) = target {
                if propagates {
                    // `{ before: a } = expr ? |e| ...` — unwrap the Ok, then
                    // destructure what was inside it.
                    body = Self::propagate_error_pattern(
                        Pattern::Tag { name: "Ok", args: vec![pattern] },
                        value,
                        body,
                        mapper,
                    );
                    body = Self::lift_tries(tries, body);
                    continue;
                }
                body = Expr::Match { id: self.node(),
                    scrutinee: Box::new(value),
                    arms: vec![MatchArm { patterns: vec![pattern], guard: None, body }],
                };
                body = Self::lift_tries(tries, body);
                continue;
            }
            let name = match target {
                BindTarget::Name(name) => name,
                // `var $x = expr?` and `$x = expr?`: the `Ok` payload lands under a
                // fresh name first, and the declaration or reassignment reads that.
                BindTarget::Var(name) | BindTarget::Assign(name) => {
                    let (value, unwrap) = if propagates {
                        let fresh: &'static str =
                            Box::leak(format!("#try_assign{}", self.pos).into_boxed_str());
                        (Expr::Ident(fresh, self.node()), Some((fresh, value)))
                    } else {
                        (value, None)
                    };
                    body = if matches!(target, BindTarget::Var(_)) {
                        Expr::VarDecl { id: self.node(), name, value: Box::new(value), body: Box::new(body) }
                    } else {
                        Expr::Assign { id: self.node(), name, value: Box::new(value), body: Box::new(body) }
                    };
                    if let Some((fresh, scrutinee)) = unwrap {
                        body = Self::propagate_error_pattern(
                            Pattern::Tag { name: "Ok", args: vec![Pattern::Binding(fresh)] },
                            scrutinee,
                            body,
                            mapper,
                        );
                    }
                    body = Self::lift_tries(tries, body);
                    continue;
                }
                BindTarget::Destructure(_) => unreachable!("handled above"),
            };
            body = if propagates {
                // This is the whole point of `?`: the continuation moves INSIDE the
                // Ok arm, so an Err skips it entirely.
                //
                //     x = expr?        match expr {
                //     <rest>      =>       Ok(x) => <rest>
                //                          Err(e) => Err(e)
                //                      }
                Self::propagate_error_pattern(
                    Pattern::Tag {
                        name: "Ok",
                        args: vec![if name == "_" {
                            Pattern::Wildcard
                        } else {
                            Pattern::Binding(name)
                        }],
                    },
                    value,
                    body,
                    mapper,
                )
            } else {
                Expr::Let { id: self.node(),
                    name,
                    annotation,
                    value: Box::new(value),
                    body: Box::new(body),
                }
            };
            body = Self::lift_tries(tries, body);
        }
        Ok(body)
    }

    /// Wrap `body` in the `match` each lifted `?` needs.
    ///
    /// The tries were collected left to right, so they are applied in reverse: the
    /// last one lands innermost, and `f(g(x)?, h(y)?)` unwraps `g(x)` before `h(y)`,
    /// which is the order the arguments are evaluated in.
    fn lift_tries(tries: Vec<(&'static str, Expr, Option<Expr>)>, body: Expr) -> Expr {
        tries.into_iter().rev().fold(body, |body, (name, value, mapper)| {
            Self::propagate_error_pattern(
                Pattern::Tag { name: "Ok", args: vec![Pattern::Binding(name)] },
                value,
                body,
                mapper,
            )
        })
    }

    /// Parse lambda expression: |params| body
    fn parse_lambda(&mut self) -> Result<Expr, ParseError> {
        self.skip_whitespace();

        let rest = &self.input[self.pos..];
        if !rest.starts_with('|') {
            return Err(ParseError {
                message: "Expected '|' to start lambda".to_string(),
                position: self.pos,
            });
        }
        self.pos += 1; // Skip '|'
        self.skip_whitespace();

        let mut params: Vec<&'static str> = Vec::new();
        // Parameters that are patterns rather than plain names, paired with the
        // generated parameter they destructure.
        let mut destructured: Vec<(&'static str, Pattern)> = Vec::new();

        // Parse parameters. Each is a PATTERN: `|Point.{ x, y }|` and `|(a, b)|` are
        // both legal, not just `|name|`.
        // `|var $x|`: a parameter the body may reassign. It becomes a plain parameter
        // whose value seeds a `var` of the written name.
        let mut var_params: Vec<(&'static str, &'static str)> = Vec::new();
        let has_params = !self.input[self.pos..].starts_with('|');
        if has_params {
            loop {
                let is_var = starts_with_keyword(&self.input[self.pos..], "var");
                if is_var {
                    self.pos += 3;
                    self.skip_whitespace();
                }
                let pattern = match self.parse_pattern() {
                    Ok(pattern) => pattern,
                    Err(_) => break,
                };
                if is_var {
                    let Pattern::Binding(name) = pattern else {
                        return Err(ParseError { message: "Expected a name after `var`".to_string(), position: self.pos });
                    };
                    let generated: &'static str = Box::leak(format!("__var{}", params.len()).into_boxed_str());
                    params.push(generated);
                    var_params.push((name, generated));
                    if !self.mutable_names.iter().any(|n| n == name) {
                        self.mutable_names.push(name.to_string());
                    }
                    self.skip_whitespace();
                    if self.input[self.pos..].starts_with(',') {
                        self.pos += 1;
                        self.skip_whitespace();
                        continue;
                    }
                    break;
                }

                match pattern {
                    // A plain name is used directly, which keeps the common case's
                    // AST unchanged.
                    Pattern::Binding(name) => params.push(name),
                    other => {
                        let generated: &'static str = Box::leak(
                            format!("__param{}", params.len()).into_boxed_str(),
                        );
                        params.push(generated);
                        destructured.push((generated, other));
                    }
                }

                self.skip_whitespace();
                if self.input[self.pos..].starts_with(',') {
                    self.pos += 1;
                    self.skip_whitespace();
                } else {
                    break;
                }
            }
        }

        self.skip_whitespace();
        let rest = &self.input[self.pos..];
        if !rest.starts_with('|') {
            return Err(ParseError {
                message: "Expected '|' to end lambda parameters".to_string(),
                position: self.pos,
            });
        }
        self.pos += 1; // Skip '|'
        self.skip_whitespace();

        // Parse body. A `{ ... }` block is a primary expression; anything else
        // falls through to the normal expression parser — as a block of one
        // statement, so a `?` in it has somewhere to return from. A RECORD is not a
        // block: `|b| { val: b.val + 1 }.val` reads the field inside the lambda, so
        // it takes the normal path, where postfix and operators apply to it.
        let mut body = if self.input[self.pos..].starts_with('{') && !self.looks_like_record(false) {
            self.parse_braced()?
        } else {
            let tries_before = self.pending_tries.len();
            self.block_depth += 1;
            let parsed = self.parse_expr();
            self.block_depth -= 1;
            let parsed = parsed?;
            let tries = self.pending_tries.split_off(tries_before);
            if tries.is_empty() { parsed } else { Self::lift_tries(tries, parsed) }
        };

        for (name, generated) in var_params.into_iter().rev() {
            body = Expr::VarDecl {
                name,
                value: Box::new(Expr::Ident(generated, self.node())),
                body: Box::new(body),
                id: self.node(),
            };
        }

        // A pattern parameter is a one-arm match on the generated name, the same shape
        // a destructuring binding uses inside a block.
        // A parameter pattern the argument does not fit is a CRASH at run time
        // (`(|[a]| a)([])`), where a `match` with no arm for it is a compile problem;
        // the fallback arm is what tells the two apart.
        for (generated, pattern) in destructured.into_iter().rev() {
            let fallback = MatchArm {
                patterns: vec![Pattern::Wildcard],
                guard: None,
                body: Expr::Crash(
                    Box::new(Expr::Str("This pattern does not match the argument", self.node())),
                    self.node(),
                ),
            };
            body = Expr::Match { id: self.node(),
                scrutinee: Box::new(Expr::Ident(generated, self.node())),
                arms: vec![MatchArm { patterns: vec![pattern], guard: None, body }, fallback],
            };
        }

        Ok(Expr::Lambda { id: self.node(), params: params.into(), body: std::rc::Rc::new(body) })
    }

    /// Skip whitespace
    fn skip_whitespace(&mut self) {
        loop {
            let rest = &self.input[self.pos..];
            let trimmed = rest.trim_start();
            self.pos += rest.len() - trimmed.len();

            // A `#` comment runs to the end of the line and is trivia EVERYWHERE — a
            // comment can sit between a binding's `=` and its value, which is where
            // skipping only whitespace used to leave the parser looking at the `#`.
            if self.input[self.pos..].starts_with('#') {
                self.skip_to_line_end();
                continue;
            }
            break;
        }
    }

    /// Parse string literal: "..."
    fn parse_string(&mut self) -> Result<Expr, ParseError> {
        // The sub-parser for each `${...}` needs the nominal declarations too, or
        // `Animal.Dog(x)` inside an interpolation parses as a qualified CALL instead of
        // a tag. Any parser state a nested expression depends on has to be passed down.
        //
        // Destructured rather than cloned. `nominals` and `nominal_defaults` were copied
        // for EVERY string literal in the file, only because `nominal_literals` is
        // borrowed mutably alongside them and `&self` cannot do both at once — and a
        // `Type` is a tree, so each copy walked every declaration. 800 string literals
        // cost 0.97ms with no nominals in scope and 5.82ms with twenty of them:
        // quadratic in (literals x declarations). Naming the fields splits the borrow.
        let Parser { input, pos, nominals, nominal_defaults, nominal_literals, .. } = self;
        let rest = &input[*pos..];
        match parse_string_literal(rest, nominals, nominal_defaults, nominal_literals) {
            Ok((remaining, expr)) => {
                *pos += rest.len() - remaining.len();
                Ok(expr)
            }
            Err(e) => Err(ParseError {
                message: e.message,
                position: *pos + e.position,
            }),
        }
    }
}

/// Parse string literal with interpolation
fn parse_string_literal<'input>(
    input: &'input str,
    nominals: &[(&'static str, Type)],
    nominal_defaults: &[(String, Vec<(String, Expr)>)],
    nominal_literals: &mut Vec<(crate::ast::NodeId, Type)>,
) -> Result<(&'input str, Expr), ParseError> {
    // Check for opening quote
    if !input.starts_with('"') {
        return Err(ParseError {
            message: "Expected '\"'".to_string(),
            position: 0,
        });
    }

    let input = &input[1..]; // Skip opening quote
    let (content, remaining) = parse_string_content(input)?;

    if !remaining.starts_with('"') {
        return Err(ParseError {
            message: "Expected closing '\"'".to_string(),
            position: input.len() - remaining.len(),
        });
    }

    let remaining = &remaining[1..]; // Skip closing quote

    if content.is_empty() {
        Ok((remaining, Expr::Str(string_pool::intern(""), crate::ast::fresh_node_unlocated())))
    } else if content.contains("${") {
        // Parse interpolation expressions
        let parts = parse_interpolation_parts(
            &content,
            nominals,
            nominal_defaults,
            nominal_literals,
        )?;
        Ok((remaining, Expr::StrInterp(parts, crate::ast::fresh_node_unlocated())))
    } else {
        // Plain string
        Ok((remaining, Expr::Str(string_pool::intern(&content), crate::ast::fresh_node_unlocated())))
    }
}

/// Parse string content (everything between quotes, handling escapes)
fn parse_string_content(input: &str) -> Result<(String, &str), ParseError> {
    let mut result = String::new();
    let mut pos = 0;
    let input_bytes = input.as_bytes();

    while pos < input_bytes.len() {
        // An interpolation may contain string literals of its own, as in
        //     "${render(Foo(42, "answer"))}"
        // so `${ ... }` is copied across verbatim, tracking brace depth and skipping
        // over nested strings. Without this the inner quote ends the outer string.
        if input_bytes[pos] == b'$' && pos + 1 < input_bytes.len() && input_bytes[pos + 1] == b'{' {
            result.push_str("${");
            pos += 2;

            let mut brace_depth = 1;
            while pos < input_bytes.len() && brace_depth > 0 {
                match input_bytes[pos] {
                    b'{' => {
                        brace_depth += 1;
                        result.push('{');
                        pos += 1;
                    }
                    b'}' => {
                        brace_depth -= 1;
                        result.push('}');
                        pos += 1;
                    }
                    b'"' => {
                        // Copy a nested string literal whole, honouring escapes so a
                        // `\"` inside it does not look like its terminator.
                        result.push('"');
                        pos += 1;
                        while pos < input_bytes.len() && input_bytes[pos] != b'"' {
                            if input_bytes[pos] == b'\\' && pos + 1 < input_bytes.len() {
                                result.push(input_bytes[pos] as char);
                                pos += 1;
                            }
                            let ch = input[pos..].chars().next().expect("char boundary");
                            result.push(ch);
                            pos += ch.len_utf8();
                        }
                        if pos < input_bytes.len() {
                            result.push('"');
                            pos += 1;
                        }
                    }
                    _ => {
                        let ch = input[pos..].chars().next().expect("char boundary");
                        result.push(ch);
                        pos += ch.len_utf8();
                    }
                }
            }

            if brace_depth != 0 {
                return Err(ParseError {
                    message: "Unclosed ${ in string interpolation".to_string(),
                    position: pos,
                });
            }
            continue;
        }

        match input_bytes[pos] {
            b'"' => break,
            b'\\' => {
                pos += 1;
                if pos < input_bytes.len() {
                    match input_bytes[pos] {
                        b'n' => result.push('\n'),
                        b't' => result.push('\t'),
                        b'r' => result.push('\r'),
                        b'\\' => result.push('\\'),
                        b'"' => result.push('"'),
                        // `\u(e9)` inserts a code point by its hex value.
                        b'u' if input_bytes.get(pos + 1) == Some(&b'(') => {
                            let start = pos + 2;
                            match input[start..].find(')') {
                                Some(offset) => {
                                    let hex = &input[start..start + offset];
                                    match u32::from_str_radix(hex, 16)
                                        .ok()
                                        .and_then(char::from_u32)
                                    {
                                        Some(c) => result.push(c),
                                        None => {
                                            return Err(ParseError {
                                                message: format!(
                                                    "Invalid unicode escape \\u({})",
                                                    hex
                                                ),
                                                position: pos,
                                            })
                                        }
                                    }
                                    // Past the closing paren; the loop's own `pos += 1`
                                    // steps over the final character.
                                    pos = start + offset;
                                }
                                None => {
                                    return Err(ParseError {
                                        message: "Unclosed \\u( escape".to_string(),
                                        position: pos,
                                    })
                                }
                            }
                        }
                        c => {
                            result.push('\\');
                            result.push(c as char);
                        }
                    }
                    pos += 1;
                }
            }
            // A multi-byte character must be pushed WHOLE: `b as char` reinterprets
            // one UTF-8 byte as a code point, which turned `σ` into mojibake.
            _ => {
                let ch = input[pos..].chars().next().expect("pos is a char boundary");
                result.push(ch);
                pos += ch.len_utf8();
            }
        }
    }

    Ok((result, &input[pos..]))
}

/// Check if character can start an identifier
/// Allows both lowercase and uppercase for module names
fn is_ident_start(c: char) -> bool {
    // `$` starts a mutable name. `$sum` and `sum` are DIFFERENT names in roc — the
    // sigil is part of the identifier, like the trailing `!` on an effectful one.
    c.is_ascii_alphabetic() || c == '_' || c == '$'
}

/// Check if character can be in an identifier
/// Allows both lowercase and uppercase
fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Parse number literal: decimal, hex (0xFF), octal (0o77), binary (0b1010)
/// With optional type suffixes: .U8, .I32, .F32, .Dec
/// Scan a run of digits, allowing `_` separators, and return the digits without them.
///
/// Roc permits `_` anywhere inside a number for readability — `1_000_000`,
/// `0xFF_FF`, `1_0.2_5`. Stopping at the `_` silently truncated the value: `1_000_000`
/// parsed as `1`.
fn scan_digits(bytes: &[u8], mut pos: usize, accept: impl Fn(u8) -> bool) -> (usize, String) {
    let mut digits = String::new();
    let mut last_was_digit = false;
    while pos < bytes.len() {
        let c = bytes[pos];
        if accept(c) {
            digits.push(c as char);
            last_was_digit = true;
            pos += 1;
        } else if c == b'_' && last_was_digit {
            // A separator must sit between digits, so `_1` and a trailing `_` end the
            // run rather than being absorbed.
            if pos + 1 < bytes.len() && accept(bytes[pos + 1]) {
                pos += 1;
            } else {
                break;
            }
        } else {
            break;
        }
    }
    (pos, digits)
}

/// Parse a numeric literal: decimal, hex (`0xFF`), octal (`0o77`), binary (`0b1010`),
/// fractional, scientific (`1.5e3`), and type-suffixed (`255.U8`).
///
/// Digit separators are allowed throughout. Verified against roc: `1_000_000` is
/// 1000000, `1e3` is 1000, `1.5e-3` is 0.0015, `0xFF_FF` is 65535.
/// A decimal literal's digits as a `Dec`: the value times 10^18.
///
/// Read from the TEXT, because the double it also parses to has already lost the last
/// places — which is the whole reason `Dec` exists. An exponent falls back to the
/// double, since `1e30` has no exact fixed-point form anyway.
fn scale_decimal(number: &str, is_negative: bool) -> i128 {
    // The exact reader first: it knows exponents and that `Dec.lowest` is one past
    // `-i128::MAX`. What it refuses (digits past the eighteenth place, a magnitude
    // past the bound) is scaled the old way below, truncated or saturated — and
    // remembered, because roc refuses such a literal outright.
    let signed = if is_negative { format!("-{}", number) } else { number.to_string() };
    if let Some(exact) = crate::eval::dec_from_str(&signed) {
        return exact;
    }
    OVERFLOWED.with(|o| o.set(true));
    let scale = crate::eval::DEC_SCALE;
    if number.contains(['e', 'E']) {
        return crate::eval::dec_from_f64(
            number.parse::<f64>().unwrap_or(0.0) * if is_negative { -1.0 } else { 1.0 },
        );
    }
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    // Eighteen places is where a `Dec` stops; anything past them is dropped, as it is
    // by every other operation on one.
    let mut digits: String = fraction.chars().take(18).collect();
    while digits.len() < 18 {
        digits.push('0');
    }
    let whole: i128 = whole.parse().unwrap_or(0);
    let fraction: i128 = digits.parse().unwrap_or(0);
    let magnitude = whole.saturating_mul(scale).saturating_add(fraction);
    if is_negative { -magnitude } else { magnitude }
}

thread_local! {
    /// Set by `scale_decimal` when a literal could not be held exactly; the caller
    /// that makes the node reads and clears it, and records the node.
    static OVERFLOWED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Literal nodes `scale_decimal` could not hold exactly. Drained like `SUFFIXED`.
    static OVERFLOWED_NODES: std::cell::RefCell<Vec<crate::ast::NodeId>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Every numeric literal's TEXT, by node, for a custom `from_numeral`: it receives
    /// the digits as written, which the value has already rounded or narrowed.
    static NUMERAL_TEXT: std::cell::RefCell<Vec<(crate::ast::NodeId, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Literal nodes that carried an explicit type SUFFIX — `255.U8` — and which type.
    ///
    /// A thread-local because the number literal is read by a free function with no
    /// parser to hand, and the alternative is threading an out-parameter through every
    /// call site of it. `Parser::take_suffixed` drains it.
    static SUFFIXED: std::cell::RefCell<Vec<(crate::ast::NodeId, &'static str)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn parse_number_literal(input: &str) -> Result<(&str, Expr), ParseError> {
    let bytes = input.as_bytes();
    let mut pos = 0;

    let is_negative = if pos < bytes.len() && bytes[pos] == b'-' {
        pos += 1;
        true
    } else {
        false
    };

    if pos >= bytes.len() || !bytes[pos].is_ascii_digit() {
        return Err(ParseError { message: "Expected digit".to_string(), position: 0 });
    }

    // Radix prefixes. Each yields an integer, so no fraction or exponent follows.
    if bytes[pos] == b'0' && pos + 1 < bytes.len() {
        let radix = match bytes[pos + 1] {
            b'x' | b'X' => Some((16u32, "hex")),
            b'o' | b'O' => Some((8, "octal")),
            b'b' | b'B' => Some((2, "binary")),
            _ => None,
        };
        if let Some((radix, label)) = radix {
            pos += 2;
            let (next, digits) = scan_digits(bytes, pos, |c| {
                c == b'_'
                    || match radix {
                        16 => c.is_ascii_hexdigit(),
                        8 => (b'0'..=b'7').contains(&c),
                        _ => c == b'0' || c == b'1',
                    }
            });
            let digits: String = digits.chars().filter(|c| *c != '_').collect();
            if digits.is_empty() {
                return Err(ParseError {
                    message: format!("Expected a {} digit", label),
                    position: next,
                });
            }
            // Read as 128 UNSIGNED bits, then reinterpreted: `0xFFFF…` past `i128::MAX`
            // is a `U128`, held as the same bit pattern the decimal reader keeps.
            let value = u128::from_str_radix(&digits, radix).map_err(|_| ParseError {
                message: format!("Invalid {} number: {}", label, digits),
                position: 0,
            })? as i128;
            let value = if is_negative { -value } else { value };
            return Ok((&input[next..], Expr::Int(value, crate::ast::fresh_node_unlocated())));
        }
    }

    // Integer part.
    let (next, mut number) = scan_digits(bytes, pos, |c| c.is_ascii_digit());
    pos = next;
    if number.is_empty() {
        return Err(ParseError { message: "Expected digit".to_string(), position: pos });
    }

    // A `.` followed by a digit is a fraction; followed by a letter it is a type
    // suffix (`255.U8`), which belongs to the integer.
    let mut is_fractional = false;
    if pos < bytes.len() && bytes[pos] == b'.' && pos + 1 < bytes.len() && bytes[pos + 1].is_ascii_digit()
    {
        is_fractional = true;
        number.push('.');
        let (next, fraction) = scan_digits(bytes, pos + 1, |c| c.is_ascii_digit());
        number.push_str(&fraction);
        pos = next;
    }

    // Exponent: `e`/`E`, an optional sign, then digits. Valid with or without a
    // fraction — `1e3` is 1000.
    if pos < bytes.len() && (bytes[pos] == b'e' || bytes[pos] == b'E') {
        let mut probe = pos + 1;
        let mut sign = String::new();
        if probe < bytes.len() && (bytes[probe] == b'+' || bytes[probe] == b'-') {
            sign.push(bytes[probe] as char);
            probe += 1;
        }
        let (next, exponent) = scan_digits(bytes, probe, |c| c.is_ascii_digit());
        // Only an exponent with digits counts; otherwise the `e` starts something else.
        if !exponent.is_empty() {
            is_fractional = true;
            number.push('e');
            number.push_str(&sign);
            number.push_str(&exponent);
            pos = next;
        }
    }

    if is_fractional {
        let value = number.parse::<f64>().map_err(|_| ParseError {
            message: format!("Invalid number: {}", number),
            position: 0,
        })?;
        let value = if is_negative { -value } else { value };
        // The SAME literal as a fixed-point value, read off the digits rather than off
        // the double, so `147.666666666666666666` keeps all eighteen places.
        let scaled = scale_decimal(&number, is_negative);
        let overflowed = OVERFLOWED.with(|o| o.replace(false));
        let node = crate::ast::fresh_node_unlocated();
        if overflowed {
            OVERFLOWED_NODES.with(|o| o.borrow_mut().push(node));
        }
        NUMERAL_TEXT.with(|t| t.borrow_mut().push((node, signed_text(is_negative, &number))));

        // A fractional type suffix, e.g. `3.14.F64` — recorded like an integer's, so
        // the checker types it and the compiler builds the right representation. It
        // used to be dropped, which left `2.0.F64` a numeral that defaulted.
        let remaining = &input[pos..];
        for suffix in [".F32", ".F64", ".Dec"] {
            if let Some(rest) = remaining.strip_prefix(suffix) {
                SUFFIXED.with(|s| s.borrow_mut().push((node, &suffix[1..])));
                return Ok((rest, Expr::Float(value, scaled, node)));
            }
        }
        return Ok((remaining, Expr::Float(value, scaled, node)));
    }

    // Parsed WIDE, then narrowed. `-9223372036854775808` is `i64::MIN`, and its
    // magnitude is one past `i64::MAX` — reading the digits as an `i64` before applying
    // the sign rejects the one literal that names the smallest integer there is.
    // `Builtin.roc` writes it out for `I64.lowest`.
    // Read UNSIGNED, then apply the sign. `I128.lowest` is
    // -170141183460469231731687303715884105728, and its magnitude is one past
    // `i128::MAX` — the same trap `I64.lowest` sprang, one width up.
    let magnitude = number.parse::<u128>().map_err(|_| ParseError {
        message: format!("Integer literal {} does not fit in I128", number),
        position: 0,
    })?;
    // KEPT WIDE. `U64.highest` is 18446744073709551615 and `U128.highest` is
    // 340282366920938463463374607431768211455 — both are written out in `Builtin.roc`,
    // and narrowing the literal to an i64 is what stopped `Iter` and `Num` parsing.
    const I128_MIN_MAGNITUDE: u128 = 1u128 << 127;
    let value = if is_negative {
        if magnitude == I128_MIN_MAGNITUDE {
            i128::MIN
        } else {
            -i128::try_from(magnitude).map_err(|_| ParseError {
                message: format!("Integer literal -{} does not fit in I128", number),
                position: 0,
            })?
        }
    } else {
        // `U128.highest` is past `i128::MAX`, so it is held as the same BIT PATTERN.
        // Every width operation reads it back correctly; only a comparison or a
        // `to_str` on one that large sees it as negative.
        magnitude as i128
    };

    let node = crate::ast::fresh_node_unlocated();
    NUMERAL_TEXT.with(|t| t.borrow_mut().push((node, signed_text(is_negative, &number))));

    // An integer type suffix, e.g. `255.U8`. The VALUE is unchanged — one integer
    // representation — but the TYPE is not: a suffixed literal is not a numeral waiting
    // to be defaulted, it has already been told what it is.
    let remaining = &input[pos..];
    if remaining.starts_with('.') {
        let after = &remaining[1..];
        for suffix in [
            "U128", "I128", "U64", "I64", "U32", "I32", "U16", "I16", "U8", "I8", "Dec",
            "F64", "F32",
        ] {
            if let Some(rest) = after.strip_prefix(suffix) {
                // Only if the suffix ends there — `255.U8x` is not a suffix.
                if !rest.starts_with(is_ident_char) {
                    SUFFIXED.with(|s| s.borrow_mut().push((node, suffix)));
                    // `170141183460469231732.Dec` is past what a `Dec` holds.
                    if suffix == "Dec" && value.checked_mul(crate::eval::DEC_SCALE).is_none() {
                        OVERFLOWED_NODES.with(|o| o.borrow_mut().push(node));
                    }
                    return if suffix.starts_with('F') || suffix == "Dec" {
                        Ok((
                            rest,
                            Expr::Float(
                                value as f64,
                                value.saturating_mul(crate::eval::DEC_SCALE),
                                node,
                            ),
                        ))
                    } else {
                        Ok((rest, Expr::Int(value, node)))
                    };
                }
            }
        }
    }

    Ok((remaining, Expr::Int(value, node)))
}

fn signed_text(is_negative: bool, number: &str) -> String {
    if is_negative { format!("-{}", number) } else { number.to_string() }
}

/// Parse identifier: x, main, birds
fn parse_identifier(input: &str) -> Result<(&str, Expr), ParseError> {
    let mut pos = 0;
    let mut chars = input.chars();

    // First character must be lowercase letter or underscore
    match chars.next() {
        Some(c) if is_ident_start(c) => pos += c.len_utf8(),
        _ => {
            return Err(ParseError {
                message: "Expected identifier start".to_string(),
                position: 0,
            })
        }
    }

    // Rest can be alphanumeric or underscore
    for c in chars {
        if is_ident_char(c) {
            pos += c.len_utf8();
        } else {
            break;
        }
    }

    // A trailing `!` is part of the identifier, not an operator: upstream's
    // `chompIdentGeneral` (roc-compiler/src/parse/tokenize.zig) chomps it, and an
    // effectful binding whose name lacks it is a warning. So `echo!` is one name.
    // Guard against `!=`, which is its own operator.
    if input[pos..].starts_with('!') && !input[pos..].starts_with("!=") {
        pos += 1;
    }

    let ident = &input[..pos];
    let remaining = &input[pos..];

    Ok((remaining, Expr::Ident(string_pool::intern(ident), crate::ast::fresh_node_unlocated())))
}

/// Parse string interpolation: "text ${expr} more"
/// Returns vector of literal strings and expressions
fn parse_interpolation_parts(
    content: &str,
    nominals: &[(&'static str, Type)],
    nominal_defaults: &[(String, Vec<(String, Expr)>)],
    nominal_literals: &mut Vec<(crate::ast::NodeId, Type)>,
) -> Result<Vec<StrPart>, ParseError> {
    let mut parts = Vec::new();
    let mut current_literal = String::new();
    let mut pos = 0;
    let bytes = content.as_bytes();

    while pos < bytes.len() {
        // Look for ${
        if pos + 1 < bytes.len() && bytes[pos] == b'$' && bytes[pos + 1] == b'{' {
            // Save current literal if any
            if !current_literal.is_empty() {
                parts.push(StrPart::Literal(string_pool::intern(&current_literal)));
                current_literal.clear();
            }

            // Find matching }
            pos += 2; // Skip ${
            let expr_start = pos;
            let mut brace_depth = 1;

            while pos < bytes.len() && brace_depth > 0 {
                match bytes[pos] {
                    b'{' => brace_depth += 1,
                    b'}' => brace_depth -= 1,
                    // A `}` inside a nested string is not our closing brace.
                    b'"' => {
                        pos += 1;
                        while pos < bytes.len() && bytes[pos] != b'"' {
                            if bytes[pos] == b'\\' {
                                pos += 1;
                            }
                            pos += 1;
                        }
                    }
                    _ => {}
                }
                pos += 1;
            }

            if brace_depth != 0 {
                return Err(ParseError {
                    message: "Unclosed ${ in string interpolation".to_string(),
                    position: 0,
                });
            }

            // Parse expression
            let expr_str = &content[expr_start..pos - 1];
            let mut expr_parser = Parser::new(expr_str);
            // Every piece of parser state a nested expression depends on has to be
            // passed down. `nominals` alone was not enough: a construction inside an
            // interpolation also needs the field DEFAULTS, or its omitted fields are
            // silently left out.
            expr_parser.nominals = nominals.to_vec();
            expr_parser.nominal_defaults = nominal_defaults.to_vec();
            let expr = expr_parser.parse_expr()?;
            // And every piece it LEARNS has to be passed back up: a `Point.{ … }`
            // inside an interpolation is a nominal construction like any other, and
            // the checker only knows that from this record.
            nominal_literals.extend(expr_parser.nominal_literals);
            parts.push(StrPart::Expr(Box::leak(Box::new(expr))));
        } else {
            // Push the whole character, not one byte of it: `as char` on a byte
            // splits multi-byte UTF-8 and turns `é` into mojibake. Advancing must use
            // the character's own width for the same reason, or the next read lands
            // mid-character.
            let ch = content[pos..].chars().next().expect("pos is a char boundary");
            current_literal.push(ch);
            pos += ch.len_utf8();
        }
    }

    // Add final literal if any
    if !current_literal.is_empty() {
        parts.push(StrPart::Literal(string_pool::intern(&current_literal)));
    }

    Ok(parts)
}

/// Check if '-' is followed by a digit (negative literal) vs subtraction operator
fn is_next_digit(rest: &str) -> bool {
    if rest.len() < 2 {
        return false;
    }
    let after_minus = &rest[1..];
    after_minus.chars().next().map_or(false, |c| c.is_ascii_digit())
}

/// Does `rest` start with the bare keyword `kw`, not merely a word beginning with it?
///
/// Without the boundary check, `android` would parse as `and` followed by `roid`.
fn starts_with_keyword(rest: &str, kw: &str) -> bool {
    rest.strip_prefix(kw)
        .map(|after| after.chars().next().is_none_or(|c| !is_ident_char(c)))
        .unwrap_or(false)
}

/// What a block statement binds: a plain name, or a pattern to destructure.
///
/// Destructuring is folded into a one-arm `match`, so it needs no AST node of its own.
enum BindTarget {
    Name(&'static str),
    /// `var x = value` — a rebindable binding.
    Var(&'static str),
    /// `x = value` where `x` is an existing `var` — updates it in place.
    Assign(&'static str),
    Destructure(Pattern),
}

/// Map a capitalised type name plus arguments to a `Type`.
///
/// `Try(a, b)` becomes the tag union `[Ok(a), Err(b)]`, because in roc that is
/// literally what it is — which is what makes `main!`'s annotation checkable.
///
/// ponytail: an unrecognised name yields a fresh type variable rather than an error,
/// so an annotation mentioning a type the interpreter does not model stays harmless
/// instead of rejecting the file. Nominal types (phase 14) will need real entries.
/// Replace type variables by id, used to instantiate a parameterised nominal.
fn substitute_type_vars(ty: &Type, pairs: &[(u32, Type)]) -> Type {
    match ty {
        Type::TypeVar(id) => pairs
            .iter()
            .find(|(p, _)| p == id)
            .map(|(_, t)| t.clone())
            .unwrap_or_else(|| ty.clone()),
        Type::List(inner) => Type::List(Box::new(substitute_type_vars(inner, pairs))),
        Type::Optional(inner) => Type::Optional(Box::new(substitute_type_vars(inner, pairs))),
        Type::Nominal { name, backing, args } => Type::Nominal {
            name: *name,
            backing: Box::new(substitute_type_vars(backing, pairs)),
            args: args.iter().map(|t| substitute_type_vars(t, pairs)).collect(),
        },
        // `open` is carried: `R(x) : { a : I64, ..x }` applied to anything produced a
        // CLOSED `{ a : I64 }`, so the extension's own fields were then rejected.
        Type::Record { fields, open } => Type::Record {
            fields: fields
                .iter()
                .map(|(n, t)| (*n, substitute_type_vars(t, pairs)))
                .collect(),
            open: *open,
        },
        Type::Tuple(items) => {
            Type::Tuple(items.iter().map(|t| substitute_type_vars(t, pairs)).collect())
        }
        Type::Function(a, b) => Type::Function(
            Box::new(substitute_type_vars(a, pairs)),
            Box::new(substitute_type_vars(b, pairs)),
        ),
        Type::TagUnion { tags, open, row } => {
            let mut tags: Vec<(&'static str, Vec<Type>)> = tags
                .iter()
                .map(|(n, ts)| {
                    (*n, ts.iter().map(|t| substitute_type_vars(t, pairs)).collect())
                })
                .collect();
            // An extension parameter, `T(x) : [A, ..x]`, applied: its argument's tags
            // join the union, and its row or its closedness is the union's.
            match row.and_then(|r| pairs.iter().find(|(p, _)| *p == r)).map(|(_, t)| t) {
                Some(Type::TagUnion { tags: more, open, row }) => {
                    tags.extend(more.iter().filter(|(n, _)| !tags.iter().any(|(m, _)| m == n)).cloned().collect::<Vec<_>>());
                    tags.sort_by(|a, b| a.0.cmp(&b.0));
                    Type::TagUnion { tags, open: *open, row: *row }
                }
                Some(Type::TypeVar(v)) => Type::TagUnion { tags, open: true, row: Some(*v) },
                _ => Type::TagUnion { tags, open: *open, row: *row },
            }
        }
        other => other.clone(),
    }
}

/// Two spellings of one nominal: `Mod.Name` and `Name`.
fn same_name(a: &str, b: &str) -> bool {
    a == b || a.rsplit('.').next() == b.rsplit('.').next()
}

fn named_type(name: &str, args: Vec<Type>, fresh: impl FnMut() -> Type) -> Type {
    let mut args = args;
    if let Some(builtin) = builtin_type(name, &mut args, fresh) {
        return builtin;
    }
    // Every other name is a TYPE, not an unknown: a user's own nominal has already
    // been resolved by its declaration before this is reached. Answering a fresh
    // variable threw the name away, which is what left `fruit_dict : Dict(Str, U64)`
    // with no type at all and `fruit_dict.get(k)` with nothing to dispatch on.
    //
    // The arguments are kept: `Step(a)` named before `Step` is declared is `Step` at
    // `a`, which the checker's `expand` puts in place of the declaration's
    // parameters once it knows them.
    Type::Nominal { name: string_pool::intern(name), backing: Box::new(Type::TypeVar(u32::MAX)), args }
}

/// The types Roc names and rocflight models directly. `None` for anything else.
/// `args` is borrowed and only emptied when a builtin actually MATCHES.
///
/// It used to be taken by value, so the one caller cloned — deep-copying every argument
/// type, `String` field names and all, for every type atom in every annotation, and
/// throwing the copy away whenever the name was not a builtin. A file of annotations is
/// what `Builtin.roc` is: `Try(List(U64), [Bad(Str)])` cost 38 heap allocations and
/// this was most of them.
fn builtin_type(name: &str, args: &mut Vec<Type>, mut fresh: impl FnMut() -> Type) -> Option<Type> {
    let _ = &mut fresh;
    Some(match (name, args.len()) {
        ("Str", 0) => Type::Str,
        ("Bool", 0) => Type::Bool,
        ("U8", 0) => Type::U8,
        ("U16", 0) => Type::U16,
        ("U32", 0) => Type::U32,
        ("U64", 0) => Type::U64,
        ("U128", 0) => Type::U128,
        ("I8", 0) => Type::I8,
        ("I16", 0) => Type::I16,
        ("I32", 0) => Type::I32,
        ("I64", 0) => Type::I64,
        ("I128", 0) => Type::I128,
        ("F32", 0) => Type::F32,
        ("F64", 0) => Type::F64,
        ("Dec", 0) => Type::Dec,
        ("List", 1) => Type::List(Box::new(args.pop().expect("arity 1"))),
        // A boxed value is the value here — `Box.box` and `Box.unbox` are the identity
        // at run time — so `Box(I64 -> I64)` types as the function it holds.
        ("Box", 1) => args.pop().expect("arity 1"),
        // An iterator is walked with the List methods, so it IS the list it behaves
        // like here. As a nameless nominal its element was dropped, and a lambda
        // handed to `.iter().map(..)` was checked against nothing — `x * 2` never
        // learnt it was an I64 and printed `4.0`.
        ("Iter", 1) => Type::List(Box::new(args.pop().expect("arity 1"))),
        // `Range(num)` over a third-party numeric type keeps its element in the backing,
        // so `range : Range(Distance)` pins the numbers inside a `Range.custom` config
        // to `Distance`. rocflight's own integer ranges never write the name.
        ("Range", 1) => Type::Nominal {
            name: "Range",
            backing: Box::new(args.pop().expect("arity 1")),
            args: Vec::new(),
        },
        ("Try", 2) => {
            // `pop` takes from the END, so the error type comes off first.
            let err = args.pop().expect("arity 2");
            let ok = args.pop().expect("arity 2");
            // Sorted by tag name, like every other union.
            Type::TagUnion {
                tags: vec![("Err", vec![err]), ("Ok", vec![ok])],
                open: false,
                row: None,
            }
        }
        _ => return None,
    })
}

/// How a top-level destructuring reaches one element of its temporary.
///
/// A tuple pattern indexes by position, a record pattern by field name; both otherwise
/// lower to the same chain of bindings.
enum Accessor {
    Index(usize),
    Field(&'static str),
}

/// Leak a field name so it lives as long as the AST.
/// A field name as a `&'static str`.
///
/// Interned rather than leaked outright — which is what this did — so the same field
/// name written in twenty record literals is one allocation instead of twenty.
fn leak_field(name: &str) -> &'static str {
    string_pool::intern(name)
}

/// One step of a file's statement chain, for `Parser::parse_let_or_expr`.
enum Step {
    /// A binding whose body is a placeholder until the chain's end is known.
    Binding(Expr),
    /// The expression the chain ends in.
    Done(Expr),
}
