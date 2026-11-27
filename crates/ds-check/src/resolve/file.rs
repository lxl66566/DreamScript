//! The per-file walk: one resolver over the complete package tables, recording
//! every name use the file reaches and the candidates a miss answers.
//!
//! # Namespace split
//!
//! Constructor positions resolve against the TYPE namespace only: brace
//! construction (`user { ... }`), bare tag references, pattern paths, and
//! generic-application constructors (`list[page]()`, whose head and type
//! arguments are type uses). A call-form name resolves against the VALUE
//! namespace (a function call), else the TYPE namespace (a constructor
//! call — tag or payload construction); present in BOTH it is an
//! ambiguity error (DS2014), never a priority search. A bare (non-call)
//! use resolves VALUE-first, then TYPE — a bare tag is a
//! value-producing constructor, while a struct/enum/trait name reached at
//! a bare use resolves so the checker can word its "type used as a value"
//! error. `struct map` and `fn map` therefore coexist, each resolving in
//! its own positions.
//!
//! # Local scoping
//!
//! Params, lets, lambda parameters, match-arm binders, and for-patterns
//! bind into lexical scopes. Same-scope redefinition is an error (both
//! spans named); nested scopes may shadow outer bindings and package
//! names. An initializer is resolved BEFORE its own binders bind
//! (`const x = x` uses the outer `x`); the defined name of a const-arrow
//! function is in scope inside its own body via the package table
//! (recursion). Generic parameters bind in a separate type-variable scope
//! stack for type positions; a trait member's signatures additionally bind
//! `Self` as the self type. Or-pattern alternatives rebind
//! identical names into one arm scope without error; whether alternatives
//! must bind the SAME set is the checker's.

use super::*;

/// One file's resolution pass: walks items with lexical scopes and records
/// every name use.
pub(super) struct FileResolver<'a> {
    pub(super) package: &'a Package,
    pub(super) uses: Vec<(Span, Res)>,
    pub(super) diags: Vec<Diagnostic>,
    /// The file being resolved: the shadowing tag lookup is
    /// per file, a file's own tag declaration beating the first's.
    pub(super) file: ds_diag::FileId,
    /// The file's import bindings, when the link pass produced any.
    pub(super) imports: Option<&'a FileImports>,
    /// Value scopes, innermost last: binder name to binder span.
    pub(super) scopes: Vec<FxHashMap<String, Span>>,
    /// Type-variable scopes, innermost last.
    pub(super) type_scopes: Vec<FxHashMap<String, Span>>,
    /// Depth of `fields` projection arguments being resolved: a bare
    /// name no namespace binds stays quiet there (the fn's own generic
    /// parameter the checker's variable scopes own), the checker's
    /// projection owning the diagnostic for a name that resolves to
    /// nothing.
    pub(super) quiet_projection_args: usize,
}

/// What a bare identifier in a pattern names.
pub(super) enum ArmTag {
    /// No constructor entry: the identifier binds.
    Binding,
    /// The universe's predeclared `none` atom, which has no def.
    BuiltinNone,
    /// A constructor type's definition.
    Def(DefId),
}

impl FileResolver<'_> {
    pub(super) fn item(&mut self, item: &Item) {
        match &item.kind {
            ItemKind::Import(imp) => self.import(imp, item.vis),
            ItemKind::Tag(_) | ItemKind::Platform(_) => {},
            ItemKind::Local(local) => {
                // A top-level binding declares a PACKAGE def, not a
                // local: the initializer and annotation resolve here, the
                // binders bind nowhere, and uses reach the name through
                // the package table.
                self.expr(&local.init);
                if let Some(ty) = &local.ty {
                    self.type_(ty);
                }
            },
            ItemKind::Fn(f) | ItemKind::ExternFn(f) => self.fn_decl(f, false),
            ItemKind::Struct(s) => {
                self.type_params(&s.generics);
                for ty in &s.supers {
                    self.type_(ty);
                }
                for member in &s.members {
                    match member {
                        StructMember::Field(field) => self.type_(&field.ty),
                        // `self` inside a deinit body is the dying
                        // instance, borrowed; the walk never types it,
                        // so binding the name here is all resolution owes.
                        StructMember::Deinit { span, body } => {
                            self.scopes.push(FxHashMap::default());
                            self.bind(
                                &Ident {
                                    span: *span,
                                    name: "self".to_owned(),
                                },
                                false,
                            );
                            self.block(body);
                            self.scopes.pop();
                        },
                    }
                }
                self.type_scopes.pop();
            },
            ItemKind::Enum(e) => {
                self.type_params(&e.generics);
                for ty in &e.supers {
                    self.type_(ty);
                }
                for case in &e.cases {
                    match &case.payload {
                        EnumPayload::None => {},
                        EnumPayload::Tuple(types) => {
                            for ty in types {
                                self.type_(ty);
                            }
                        },
                        EnumPayload::Struct(fields) => {
                            for field in fields {
                                self.type_(&field.ty);
                            }
                        },
                    }
                }
                self.type_scopes.pop();
            },
            ItemKind::TypeAlias(a) => {
                self.type_params(&a.generics);
                self.type_(&a.ty);
                self.type_scopes.pop();
            },
            ItemKind::Trait(t) => {
                self.type_params(&t.generics);
                for ty in &t.supers {
                    self.type_(ty);
                }
                // A trait member's signatures spell the self type `Self`.
                self.type_scopes
                    .push(FxHashMap::from_iter([("Self".to_owned(), t.name.span)]));
                for member in &t.members {
                    match member {
                        TraitMember::AssocType { .. } => {},
                        TraitMember::Fn(f) => self.fn_decl(f, true),
                    }
                }
                self.type_scopes.pop();
                self.type_scopes.pop();
            },
            ItemKind::Impl(imp) => {
                self.type_params(&imp.generics);
                if let Some(trait_ref) = &imp.trait_ref {
                    for arg in &trait_ref.args {
                        self.type_(arg);
                    }
                    // The trait reference is a type use: the type
                    // namespace owns it, so a same-named value (a free
                    // `fn show` beside `trait show`) never claims the
                    // head.
                    match trait_ref.path.segments.as_slice() {
                        [head] => self.type_name_use(head),
                        _ => self.path_use(&trait_ref.path),
                    }
                }
                // An impl's members spell the self type `Self`, trait and
                // inherent alike.
                self.type_scopes
                    .push(FxHashMap::from_iter([("Self".to_owned(), imp.span)]));
                self.type_(&imp.target);
                for f in &imp.fns {
                    self.fn_decl(f, true);
                }
                self.type_scopes.pop();
                self.type_scopes.pop();
            },
        }
    }

    /// Binds generic type parameters in a fresh type scope.
    fn type_params(&mut self, params: &[TypeParam]) {
        let mut scope = FxHashMap::default();
        for param in params {
            self.record_dup(&mut scope, &param.name);
            if let Some(bound) = &param.bound {
                self.type_(bound);
            }
        }
        self.type_scopes.push(scope);
    }

    fn fn_decl(&mut self, f: &FnDecl, member: bool) {
        self.type_params(&f.generics);
        self.scopes.push(FxHashMap::default());
        for (index, param) in f.params.iter().enumerate() {
            self.param(param, member && index == 0);
        }
        if let Some(ret) = &f.ret {
            self.type_(ret);
        }
        if let Some(body) = &f.body {
            self.block(body);
        }
        self.scopes.pop();
        self.type_scopes.pop();
    }

    fn param(&mut self, param: &Param, receiver: bool) {
        if let Some(ty) = &param.ty {
            // The annotated receiver's `self` annotation is the
            // checker's to report; a plain `self` path here would
            // double-report.
            let bare_self = matches!(
                ty,
                Type::Path { path, args, .. }
                    if args.is_empty()
                        && path.segments.len() == 1
                        && path.segments[0].name == "self"
            );
            if !(receiver && bare_self) {
                self.type_(ty);
            }
        }
        self.bind(&param.name, false);
    }

    fn local(&mut self, local: &ds_parse::LocalDecl) {
        // The initializer resolves before the binders bind.
        self.expr(&local.init);
        if let Some(ty) = &local.ty {
            self.type_(ty);
        }
        self.bind_pattern(&local.pat, false);
    }

    fn block(&mut self, block: &ds_parse::BlockExpr) {
        self.scopes.push(FxHashMap::default());
        for stmt in &block.stmts {
            self.stmt(stmt);
        }
        self.scopes.pop();
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Local(local) => self.local(local),
            StmtKind::Fn(f) => self.block_fn(f),
            StmtKind::Expr(expr) => self.expr(expr),
        }
    }

    /// One block-local `fn` declaration. The body resolves under a frame
    /// holding the declaration's own name beside the parameters — the
    /// recursion door, with the parameters shadowing it like a top-level
    /// fn's do — and then the name binds in the enclosing block like any
    /// local binding: a sibling statement from the declaration on
    /// resolves it, a use before the declaration, in a sibling block, or
    /// in an enclosing scope resolves nothing.
    fn block_fn(&mut self, f: &FnDecl) {
        self.type_params(&f.generics);
        self.scopes.push(FxHashMap::default());
        self.bind(&f.name, false);
        self.scopes.push(FxHashMap::default());
        for param in &f.params {
            self.param(param, false);
        }
        if let Some(ret) = &f.ret {
            self.type_(ret);
        }
        if let Some(body) = &f.body {
            self.block(body);
        }
        self.scopes.pop();
        self.scopes.pop();
        self.bind(&f.name, false);
        self.type_scopes.pop();
    }

    // ---- value positions ---------------------------------------------------

    fn expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Literal { .. }
            | Expr::RawStr { .. }
            | Expr::Break(_)
            | Expr::Continue(_)
            // A conversion placeholder uses no name.
            | Expr::Invalid(_) => {},
            Expr::Str { parts, .. } => {
                for part in parts {
                    match part {
                        ds_parse::StrPart::Text { .. } => {},
                        ds_parse::StrPart::Expr { expr, .. } => self.expr(expr),
                        // `$x` bare holes are a value use of the name.
                        ds_parse::StrPart::Ident { name, .. } => self.name_use(name),
                    }
                }
            },
            Expr::Lambda {
                generics,
                params,
                ret,
                body,
                ..
            } => {
                self.type_params(generics);
                self.scopes.push(FxHashMap::default());
                for param in params {
                    self.param(param, false);
                }
                if let Some(ret) = ret {
                    self.type_(ret);
                }
                self.expr(body);
                self.scopes.pop();
                self.type_scopes.pop();
            },
            Expr::Path { path, .. } => self.path_use(path),
            Expr::Struct { path, fields, .. } => {
                self.ctor_path_use(path);
                for (name, value) in fields {
                    match value {
                        // Punning: the field name is also a value use.
                        None => self.name_use(name),
                        Some(expr) => self.expr(expr),
                    }
                }
            },
            Expr::List { items, .. } | Expr::Tuple { items, .. } => {
                for item in items {
                    self.expr(item);
                }
            },
            Expr::Paren { inner, .. } => self.expr(inner),
            Expr::Call {
                callee,
                args,
                trailing,
                ..
            } => {
                match callee.as_ref() {
                    // UFCS: a dot-callee gets tiered resolution;
                    // a bare type name as the receiver takes the
                    // type-keyed tier instead.
                    Expr::Field { recv, name, .. } => {
                        self.dot_recv_use(recv);
                        self.dot_use(recv, name);
                    },
                    // A call-form name resolves per the namespace split;
                    // a generic-application constructor head is a type use.
                    Expr::Path { path, .. } if path.segments.len() == 1 => {
                        self.call_name_use(&path.segments[0]);
                    },
                    Expr::Index { recv, index, .. } => self.ctor_apply_use(recv, index),
                    Expr::MapApply {
                        head, key, value, ..
                    } => self.map_apply_use(head, key, value),
                    _ => self.expr(callee),
                }
                let quiet_arg = self.quiet_projection_argument(callee, args.len());
                for arg in args {
                    if quiet_arg {
                        self.quiet_projection_args += 1;
                        self.expr(&arg.value);
                        self.quiet_projection_args -= 1;
                    } else {
                        self.expr(&arg.value);
                    }
                }
                if let Some(tail) = trailing {
                    self.expr(tail);
                }
            },
            Expr::Field { recv, name, .. } => {
                // Bare field access: struct-owned, type-directed later.
                // A module-alias receiver takes the qualified member's
                // value seam instead.
                if let Some(module) = self.module_ref_receiver(recv) {
                    self.module_member_value_use(recv, module, name);
                } else {
                    self.expr(recv);
                }
            },
            Expr::FieldKey { recv, name, .. } => {
                // The keyed access resolves like a field read: the
                // receiver as a value, the bracketed constant as an
                // ordinary expression (the comptime record's member
                // projection).
                self.expr(recv);
                self.expr(name);
            },
            Expr::Index { recv, index, .. } => {
                self.expr(recv);
                self.expr(index);
            },
            Expr::MapApply {
                head, key, value, ..
            } => self.map_apply_use(head, key, value),
            // A fn type in the constructor brackets resolves its param and
            // return types as type uses.
            Expr::FnType { ty, .. } => self.type_(ty),
            Expr::Try { expr, .. } | Expr::Unary { expr, .. } => self.expr(expr),
            Expr::Is { expr, ty, .. } => {
                self.expr(expr);
                self.type_(ty);
            },
            Expr::Binary { left, right, .. }
            | Expr::Range {
                start: left,
                end: right,
                ..
            }
            | Expr::Assign { left, right, .. } => {
                self.expr(left);
                self.expr(right);
            },
            Expr::If {
                cond, then, else_, ..
            } => {
                self.expr(cond);
                self.block(then);
                if let Some(else_expr) = else_ {
                    self.expr(else_expr);
                }
            },
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.expr(scrutinee);
                for arm in arms {
                    // Each arm is its own scope nested in the enclosing one.
                    self.scopes.push(FxHashMap::default());
                    self.arm_pattern(&arm.pat);
                    if let Some(ann) = &arm.ann {
                        self.type_(ann);
                    }
                    if let Some(guard) = &arm.guard {
                        self.expr(guard);
                    }
                    self.expr(&arm.body);
                    self.scopes.pop();
                }
            },
            Expr::While { cond, body, .. } => {
                self.expr(cond);
                self.block(body);
            },
            Expr::For {
                pat, iter, body, ..
            } => {
                self.expr(iter);
                self.scopes.push(FxHashMap::default());
                self.bind_pattern(pat, false);
                self.expr(body);
                self.scopes.pop();
            },
            Expr::Return { expr, .. } => {
                if let Some(expr) = expr {
                    self.expr(expr);
                }
            },
            Expr::Nursery { body, .. } | Expr::Spawn { body, .. } | Expr::Defer { body, .. } => {
                self.block(body);
            },
            Expr::Block(block) => self.block(block),
        }
    }

    /// Refuses a use crossing the private tier's file boundary: a
    /// private name is its defining file's alone. The resolution still
    /// records, keeping the refusal the single diagnostic the later
    /// passes see (the collision precedent).
    fn enforce_vis(&mut self, span: Span, def: DefId) {
        let def_decl = self.package.def(def);
        if def_decl.vis == Vis::Private && def_decl.span.file != span.file {
            // The message names the member spelling for these collection
            // names; every other private name keeps the plain boundary
            // refusal
            let hint = match def_decl.name.as_str() {
                "list_get" => "; the member spelling is `xs.get(i)`",
                "list_push" => "; the member spelling is `xs.push(x)`",
                "map_get" => "; the member spelling is `m.get(k)`",
                "map_set" => "; the member spelling is `m.set(k, v)`",
                "map_keys" => "; the member spelling is `m.keys()`",
                "list_len" | "len" => "; the member spelling is `xs.len()`",
                "list_concat" => "; the member spelling is `xs.concat(ys)`",
                "map_len" => "; the member spelling is `m.len()`",
                // the member-tier migration's retired seams: the free
                // spelling is gone, the member one receiver-qualified
                "cons" => "; the member spelling is `xs.cons(x)`",
                "tail" => "; the member spelling is `xs.tail()`",
                "collect" => "; the member spelling is `xs.collect()`",
                "first" => "; the member spelling is `xs.first()`",
                "filter" => "; the member spelling is `xs.filter(f)`",
                "partition" => "; the member spelling is `xs.partition(f)`",
                "reduce" => "; the member spelling is `xs.reduce(init, f)`",
                "sort" => "; the member spelling is `xs.sort()`",
                "sort_by" => "; the member spelling is `xs.sort_by(cmp)`",
                "concat" => "; the member spelling is `s.concat(t)`",
                // the float math family and the string face's retired
                // seams
                "sqrt" | "sin" | "cos" | "tan" | "floor" | "ceil" | "round" | "trunc" | "ln"
                | "exp" => "; the member spelling is `x.<op>()`",
                "atan2" => "; the member spelling is `y.atan2(x)`",
                "pow" => "; the member spelling is `base.pow(exp)`",
                "abs" => "; the member spelling is `x.abs()`",
                "str_fixed" => "; the member spelling is `x.str_fixed(digits)`",
                "trim" => "; the member spelling is `s.trim()`",
                "parse_int" => "; the member spelling is `s.parse_int()`",
                "parse_float" => "; the member spelling is `s.parse_float()`",
                "split" => "; the member spelling is `s.split(sep)`",
                "substring" => "; the member spelling is `s.substring(start, end)`",
                "to_upper" => "; the member spelling is `s.to_upper()`",
                "to_lower" => "; the member spelling is `s.to_lower()`",
                "char_at" => "; the member spelling is `s.char_at(i)`",
                "contains" => "; the member spelling is `s.contains(needle)`",
                "index_of" => "; the member spelling is `s.index_of(needle)`",
                "replace" => "; the member spelling is `s.replace(needle, with)`",
                _ => "",
            };
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::PrivateUse,
                    format!(
                        "`{}` is private to its defining file [G4]{hint}",
                        def_decl.name
                    ),
                )
                .with_primary(span, "used outside the defining file")
                .with_secondary(def_decl.span, "defined private here"),
            );
        }
    }

    /// Records one use of a package definition behind the private tier's
    /// file boundary.
    fn def_use(&mut self, span: Span, def: DefId) {
        self.enforce_vis(span, def);
        self.uses.push((span, Res::Def(def)));
    }

    /// One import declaration: an `@std` import's selections validate
    /// against the exported surface — the compile unit's merged namespace
    /// serving the undeclared reading — while a graph that resolved the
    /// reserved package serves the same span through its linked surface
    /// (the member surface's own validation, recorded by the link pass)
    /// and every other path (bare or sigiled) stays declarative for the
    /// link walk (the dep-list records it and the graph validates the
    /// link). A condition names the `os` or `arch` key against one
    /// lowercase target-triple word, evaluated against the compiler's own
    /// target here: a false condition excludes the import (its selections
    /// validate on the targets that hold it), and a condition outside the
    /// vocabulary refuses. A `pub import` of `@std` refuses here: the
    /// merged-ambient reading binds the compile's own names, never
    /// import-bound defs, so no selection re-exports until the un-merge.
    fn import(&mut self, imp: &ds_parse::ImportDecl, vis: Option<ds_parse::Vis>) {
        match ds_parse::import_cond(imp) {
            ds_parse::ImportCond::Absent | ds_parse::ImportCond::Held(true) => {},
            ds_parse::ImportCond::Held(false) => return,
            ds_parse::ImportCond::Malformed { span, label } => {
                self.diags.push(
                    Diagnostic::error(
                        ErrorCode::ImportConditionMalformed,
                        "an import's condition names the `os` or `arch` key against one lowercase \
                         target-triple word"
                            .to_owned(),
                    )
                    .with_primary(span, label),
                );
                return;
            },
        }
        if !imp.path.at_sign || imp.path.segments.first().is_none_or(|s| s.name != "std") {
            return;
        }
        if let Some(ds_parse::Vis::Pub(span)) = vis {
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::VisNoTarget,
                    "`pub import` of `@std` re-exports nothing: the merged-ambient reading binds \
                     this compile's own names [G4]"
                        .to_owned(),
                )
                .with_primary(span, "the visibility modifier"),
            );
        }
        if self
            .imports
            .is_some_and(|imports| imports.linked.contains(&imp.span))
        {
            return;
        }
        for sel in &imp.select {
            let Some(def) = self.package.lookup(&sel.name.name) else {
                self.diags.push(
                    Diagnostic::error(
                        ErrorCode::ImportNameMissing,
                        format!(
                            "`{}` names no definition the imported surface exports [G4]",
                            sel.name.name
                        ),
                    )
                    .with_primary(sel.span, "no such exported name"),
                );
                continue;
            };
            if !self.package.def(def).vis.exported() {
                let def_span = self.package.def(def).span;
                self.diags.push(
                    Diagnostic::error(
                        ErrorCode::ImportNotExported,
                        format!(
                            "`{}` is `pub(pkg)` or private in its package; only `pub` names \
                             export                              [G4]",
                            sel.name.name
                        ),
                    )
                    .with_primary(sel.span, "not exported to dependents")
                    .with_secondary(def_span, "declared here"),
                );
            }
        }
    }

    /// A plain identifier use: locals shadow package names, the value
    /// namespace comes next, and the type namespace last — a bare tag is a
    /// value-producing constructor, and a struct/enum/trait name reached
    /// here still resolves so the checker words its "type used as a value"
    /// error. Bare `none` is the universe's distinguished unit atom, not a
    /// package name; the checker types it directly, so it records nothing
    /// here.
    fn name_use(&mut self, name: &Ident) {
        if let Some(bind) = self.lookup_local(&name.name) {
            self.uses.push((name.span, Res::Local { bind }));
            return;
        }
        if let Some(def) = self
            .package
            .lookup_value(&name.name)
            .or_else(|| self.package.lookup_type_in(self.file, &name.name))
            .or_else(|| self.package.lookup_case_in(self.file, &name.name))
        {
            self.def_use(name.span, def);
        } else if let Some(def) = self.import_binding(&name.name) {
            self.uses.push((name.span, Res::Def(def)));
        } else if let Some(module) = self.module_alias(&name.name) {
            self.uses.push((name.span, Res::Module(module)));
        } else if name.name != "none" && self.quiet_projection_args == 0 {
            self.unknown_name(name, self.shadowing_candidates());
        }
    }

    /// Whether this call is the comptime loop's `fields` projection with
    /// exactly one argument: the callee resolving to the builtin def.
    fn quiet_projection_argument(&mut self, callee: &Expr, arity: usize) -> bool {
        if arity != 1 {
            return false;
        }
        let Expr::Path { path, .. } = callee else {
            return false;
        };
        let [only] = path.segments.as_slice() else {
            return false;
        };
        self.uses.iter().rev().any(|(span, res)| {
            *span == only.span
                && matches!(res, Res::Def(def) if self.package.def(*def).name == "fields")
        })
    }

    /// The import-bound function a name selects in the resolving file:
    /// locals and package names shadow imported names, the resolver's own
    /// priority.
    fn import_binding(&self, name: &str) -> Option<DefId> {
        self.imports
            .and_then(|imports| imports.names.get(name))
            .map(|&(def, _)| def)
    }

    /// The module alias a name binds in the resolving file, when no
    /// local, package name, or imported name shadows it.
    fn module_alias(&self, name: &str) -> Option<ModuleId> {
        self.imports
            .and_then(|imports| imports.modules.get(name))
            .map(|&(module, _)| module)
    }

    /// The import-bound type a name selects in the resolving file:
    /// a selection of a linked type's def, when the package's own type
    /// namespace does not declare the name — the same shadowing priority
    /// the value positions follow.
    fn import_type_binding(&self, name: &str) -> Option<DefId> {
        let def = self.import_binding(name)?;
        // a linked enum's cases cross as selections beside it: the kind
        // reads the case namespace, not the shared type namespace
        (self.package.def(def).kind.in_type_namespace()
            || self.package.def(def).kind == DefKind::Case)
            .then_some(def)
    }

    /// The callee of a call form: locals first (an indirect call of a
    /// bound function value), then the value namespace, then the type
    /// namespace (a constructor call — tag or payload construction).
    /// Present in BOTH namespaces the call is ambiguous — an error naming
    /// both declarations, never a priority search.
    fn call_name_use(&mut self, name: &Ident) {
        if let Some(bind) = self.lookup_local(&name.name) {
            self.uses.push((name.span, Res::Local { bind }));
            return;
        }
        // the construction door keeps its case reading: a payloaded
        // case constructs by call form (the bare-value spellings went
        // qualified with the namespace exit, the cross-spelling
        // refusals riding this door)
        let type_def = self
            .package
            .lookup_type_in(self.file, &name.name)
            .or_else(|| self.package.lookup_case_in(self.file, &name.name));
        match (self.package.lookup_value(&name.name), type_def) {
            (Some(value), Some(ty)) => {
                // A tag family declaration yields to the value:
                // tag names left the value namespace, so the call form
                // always means the fn or const. Every other family is
                // a constructor collision — DS2014, naming both
                // declarations.
                if matches!(self.package.def(ty).kind, DefKind::Tag | DefKind::Case) {
                    self.def_use(name.span, value);
                } else {
                    self.namespace_collision(name, value, ty);
                    // Still record the value declaration so the checker
                    // types the call through and the ambiguity stays the
                    // single diagnostic; the error is fatal.
                    self.def_use(name.span, value);
                }
            },
            (Some(value), None) => self.def_use(name.span, value),
            (None, Some(ty)) => self.def_use(name.span, ty),
            (None, None) => {
                if let Some(def) = self.import_binding(&name.name) {
                    self.uses.push((name.span, Res::Def(def)));
                } else if let Some(module) = self.module_alias(&name.name) {
                    self.uses.push((name.span, Res::Module(module)));
                } else if name.name != "none" {
                    self.unknown_name(name, self.package.callable_candidates());
                }
            },
        }
    }

    /// A call-form name present in both namespaces: DS2014, naming both
    /// declarations.
    fn namespace_collision(&mut self, name: &Ident, value: DefId, ty: DefId) {
        let value_def = self.package.def(value);
        let ty_def = self.package.def(ty);
        self.diags.push(
            Diagnostic::error(
                ErrorCode::AmbiguousCall,
                format!(
                    "call-form `{}` is ambiguous between the value `{}` and the {} `{}` [F9]",
                    name.name,
                    value_def.name,
                    ty_def.kind.word(),
                    ty_def.name
                ),
            )
            .with_primary(name.span, "ambiguous call form")
            .with_secondary(value_def.span, "the value declaration")
            .with_secondary(ty_def.span, "the type declaration"),
        );
    }

    /// A generic-application constructor `list[page]()`: the head
    /// resolves in the type namespace and the bracketed arguments are type
    /// uses. A head that names no type is an ordinary index expression
    /// (`xs[i]`), whose typing is the checker's.
    fn ctor_apply_use(&mut self, recv: &Expr, index: &Expr) {
        let head = single_path_ident(recv);
        if let Some(head) = head.filter(|id| self.package.lookup_type(&id.name).is_some()) {
            self.type_name_use(head);
            self.ctor_type_arg(index);
        } else {
            self.expr(recv);
            self.expr(index);
        }
    }

    /// The map constructor's type application `map[k, v]`: the
    /// head is a type use and the two bracketed arguments are type uses,
    /// the list application's own convention.
    fn map_apply_use(&mut self, head: &Ident, key: &Expr, value: &Expr) {
        self.type_name_use(head);
        self.ctor_type_arg(key);
        self.ctor_type_arg(value);
    }

    /// One bracketed constructor argument: a type spelled in expression
    /// position — a path is a type use, a tuple walks its items, a fn type
    /// walks its param and return types, anything else falls back to the
    /// expression walk.
    fn ctor_type_arg(&mut self, arg: &Expr) {
        match arg {
            Expr::Path { path, .. } if path.segments.len() == 1 => {
                self.type_name_use(&path.segments[0]);
            },
            Expr::Paren { inner, .. } => self.ctor_type_arg(inner),
            Expr::Tuple { items, .. } => {
                for item in items {
                    self.ctor_type_arg(item);
                }
            },
            Expr::FnType { ty, .. } => self.type_(ty),
            _ => self.expr(arg),
        }
    }

    /// A path expression: one segment is a name use; `Trait::f` qualifies
    /// directly; longer paths are module paths.
    fn path_use(&mut self, path: &Path) {
        match path.segments.as_slice() {
            [only] => self.name_use(only),
            [head, method] => {
                let Some(def) = self
                    .package
                    .lookup_type_in(self.file, &head.name)
                    .or_else(|| {
                        // a linked package's registered type (its trait, its
                        // structs) mints outside the type namespace: the
                        // qualified head still resolves over its foreign row,
                        // so the spelling dispatches through the same
                        // per-site record the dot spellings take
                        self.package.foreign_type_def(&head.name)
                    })
                else {
                    // A value name at the qualifier position still gets
                    // the sharper "not a trait" word.
                    if self.package.lookup_value(&head.name).is_some() {
                        self.diags.push(
                            Diagnostic::error(
                                ErrorCode::UnresolvedName,
                                format!(
                                    "`{}` is not a trait; `A::f` is the trait-method escape hatch \
                                     [F3]",
                                    head.name
                                ),
                            )
                            .with_primary(head.span, "not a trait"),
                        );
                        return;
                    }
                    self.unknown_name(head, self.shadowing_candidates());
                    return;
                };
                if self.package.def(def).kind == DefKind::Trait {
                    let candidate = self
                        .package
                        .method_candidates(&method.name)
                        .find(|&id| self.package.method(id).owner == MethodOwner::Trait(def));
                    let Some(id) = candidate else {
                        let others = self
                            .package
                            .method_candidates(&method.name)
                            .filter(|&id| self.package.method(id).owner != MethodOwner::Trait(def))
                            .map(|id| self.package.method(id).owner_name(self.package))
                            .collect::<Vec<_>>();
                        let mut diag = Diagnostic::error(
                            ErrorCode::UnresolvedName,
                            format!("trait `{}` has no method `{}`", head.name, method.name),
                        )
                        .with_primary(method.span, "no such member");
                        if !others.is_empty() {
                            diag = diag.with_note(format!(
                                "method `{}` exists on: {}",
                                method.name,
                                others.join(", ")
                            ));
                        }
                        self.diags.push(diag);
                        return;
                    };
                    self.def_use(head.span, def);
                    self.uses.push((method.span, Res::TraitMethod(id)));
                } else if !self.qualified_label_use(def, head, method) {
                    self.diags.push(
                        Diagnostic::error(
                            ErrorCode::UnresolvedName,
                            format!(
                                "`{}` is not a trait; `A::f` is the trait-method escape hatch [F3]",
                                head.name
                            ),
                        )
                        .with_primary(head.span, "not a trait"),
                    );
                }
            },
            _ => {
                self.diags.push(
                    Diagnostic::error(
                        ErrorCode::UnresolvedName,
                        "multi-segment paths resolve through imports, which land with modules [G4]",
                    )
                    .with_primary(path.span, "module path"),
                );
            },
        }
    }

    /// A brace-construction head `user { ... }`: the type namespace
    /// only; longer paths are module paths.
    fn ctor_path_use(&mut self, path: &Path) {
        match path.segments.as_slice() {
            [only] => {
                if let Some(def) = self
                    .package
                    .lookup_type(&only.name)
                    .or_else(|| self.package.lookup_case(&only.name))
                    .or_else(|| self.import_type_binding(&only.name))
                {
                    self.def_use(only.span, def);
                } else if only.name != "none" {
                    let candidates = self.type_candidates();
                    self.unknown_name_at(only, candidates, "struct");
                }
            },
            _ => {
                self.diags.push(
                    Diagnostic::error(
                        ErrorCode::UnresolvedName,
                        "qualified struct literals resolve through imports, which land with \
                         modules [G4]",
                    )
                    .with_primary(path.span, "qualified constructor path"),
                );
            },
        }
    }

    /// A UFCS dot-call's method position: free functions first, then
    /// registered methods, then the receiver's fn-typed field (the
    /// checker's decision — a type-directed tier); the `.err`/`.ok`
    /// handler forms are surface
    /// syntax, not names. The dot is a function position, so the free
    /// tier searches the VALUE namespace only. A receiver that is
    /// a bare name resolving through the type namespace takes the
    /// type-keyed tier instead: the
    /// value tiers never apply behind a type name.
    fn dot_use(&mut self, recv: &Expr, name: &Ident) {
        if let Some(module) = self.module_ref_receiver(recv) {
            self.module_member_use(module, name);
            return;
        }
        if let Some(target) = self.type_keyed_receiver(recv) {
            self.static_use(recv, target, name);
            return;
        }
        self.dot_use_value(recv, name);
    }

    /// Whether the receiver is a bare name whose use the walk recorded
    /// nothing for: no local, no namespace def, no import binding — the
    /// receiver has no value reading for the field tier to decide on,
    /// and the unknown-name refusal at the receiver is the whole
    /// answer (the checker's silent no-record path takes over). The
    /// receiver's own use walk ran already (`dot_recv_use` precedes
    /// `dot_use`), so the table's answer at the name's span is the
    /// truth.
    fn unresolved_receiver(&self, recv: &Expr) -> bool {
        single_path_ident(recv)
            .is_some_and(|only| !self.uses.iter().rev().any(|(span, _)| *span == only.span))
    }

    /// Whether a dot-call's receiver is a bare name this file's import
    /// aliases bound: no local and no package name shadows it —
    /// the module reference is the weakest reading, exactly like the
    /// type-keyed tier's discipline.
    fn module_ref_receiver(&self, recv: &Expr) -> Option<ModuleId> {
        let only = single_path_ident(recv)?;
        if self.lookup_local(&only.name).is_some()
            || self.package.lookup_value(&only.name).is_some()
            || self.package.lookup_type(&only.name).is_some()
            || self.import_binding(&only.name).is_some()
        {
            return None;
        }
        self.module_alias(&only.name)
    }

    /// The member position behind a module alias: the alias's surface
    /// resolves the name — an `@std` alias through the merged member's
    /// own declarations, a linked alias through its member's exported
    /// entries — with the same tier refusals a selection meets.
    fn module_member_use(&mut self, module: ModuleId, name: &Ident) {
        let module_ref = self.package.module_ref(module);
        match &module_ref.surface {
            ModuleSurface::Merged { member } => {
                let found = self.package.lookup_value(&name.name).filter(|&def| {
                    let def_decl = self.package.def(def);
                    def_decl.span.file == *member && def_decl.vis.exported()
                });
                match found {
                    Some(def) => {
                        self.uses
                            .push((name.span, Res::Dot(DotRes::Module { func: def })));
                    },
                    None => {
                        self.diags.push(
                            Diagnostic::error(
                                ErrorCode::ImportNameMissing,
                                format!(
                                    "`{}` names no `pub` definition the module `{}` exports [G4]",
                                    name.name, module_ref.alias
                                ),
                            )
                            .with_primary(name.span, "no such exported name"),
                        );
                    },
                }
            },
            ModuleSurface::Linked { surface, defs } => {
                let Some(entry) = surface.entry(&name.name) else {
                    self.diags.push(
                        Diagnostic::error(
                            ErrorCode::ImportNameMissing,
                            format!(
                                "`{}` names no definition the module `{}` exports [G4]",
                                name.name, module_ref.alias
                            ),
                        )
                        .with_primary(name.span, "no such exported name"),
                    );
                    return;
                };
                if !entry.vis.exported() {
                    self.diags.push(
                        Diagnostic::error(
                            ErrorCode::ImportNotExported,
                            format!(
                                "`{}` is `pub(pkg)` or private in its package; only `pub` names \
                                 export                              [G4]",
                                name.name
                            ),
                        )
                        .with_primary(name.span, "not exported to dependents")
                        .with_secondary(entry.decl_span, "declared here"),
                    );
                    return;
                }
                let Some(&func) = defs.get(&name.name) else {
                    // a shape the qualified call cannot serve: the alias
                    // resolves call sites, so a literal const (a value
                    // member since the value seam) and a refused shape
                    // alike name the same boundary here
                    let what = if entry.kind == ForeignKind::Binding && entry.value.is_some() {
                        format!(
                            "the module alias serves callables at call sites; a linked const \
                             reads in value position (`{}.{}`) or binds through its selection \
                             import",
                            module_ref.alias, name.name
                        )
                    } else {
                        "it cannot bind across packages this round".to_owned()
                    };
                    self.diags.push(
                        Diagnostic::error(
                            ErrorCode::ImportUnsupportedShape,
                            format!(
                                "`{}` names no callable the module serves: {what} [G4]",
                                name.name
                            ),
                        )
                        .with_primary(name.span, "not bindable through the alias")
                        .with_secondary(entry.decl_span, "declared here"),
                    );
                    return;
                };
                self.uses
                    .push((name.span, Res::Dot(DotRes::Module { func })));
            },
        }
    }

    /// A module alias's qualified member in value position: a
    /// linked package's fact-carrying const binds through the alias
    /// exactly as its selection spelling does — the same def, so every
    /// later pass embeds the literal — while every other member keeps
    /// its refusal at this seam. The merged-ambient alias keeps the
    /// call-only reading: its member file's names are the merged
    /// namespace's own, never import-bound defs.
    fn module_member_value_use(&mut self, recv: &Expr, module: ModuleId, name: &Ident) {
        let linked = match &self.package.module_ref(module).surface {
            ModuleSurface::Linked { surface, .. } => Some(surface.clone()),
            ModuleSurface::Merged { .. } => None,
        };
        self.expr(recv);
        let Some(surface) = linked else {
            return;
        };
        let module_alias = self.package.module_ref(module).alias().to_owned();
        let Some(entry) = surface.entry(&name.name) else {
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::ImportNameMissing,
                    format!(
                        "`{}` names no definition the module `{module_alias}` exports [G4]",
                        name.name
                    ),
                )
                .with_primary(name.span, "no such exported name"),
            );
            return;
        };
        if !entry.vis.exported() {
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::ImportNotExported,
                    format!(
                        "`{}` is `pub(pkg)` or private in its package; only `pub` names \
                         export                              [G4]",
                        name.name
                    ),
                )
                .with_primary(name.span, "not exported to dependents")
                .with_secondary(entry.decl_span, "declared here"),
            );
            return;
        }
        let Some(&def) = self
            .package
            .foreign_const_defs
            .get(&(entry.link, entry.name.clone()))
        else {
            let what = match entry.kind {
                ForeignKind::Binding => "its value is no const fact — a `var` binding evaluates \
                                         in the linked module and no per-target value exists for \
                                         it; its selection import keeps the same refusal"
                    .to_owned(),
                ForeignKind::Fn => "a linked function serves through the alias at its call sites \
                                    alone; a function value has no callable word this round"
                    .to_owned(),
                ForeignKind::Type => "a linked type is a type-namespace name; its selection \
                                      import binds the name in type position"
                    .to_owned(),
            };
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::ImportUnsupportedShape,
                    format!(
                        "`{}` names no value member the module serves: {what} [G4]",
                        name.name
                    ),
                )
                .with_primary(name.span, "not bindable through the alias")
                .with_secondary(entry.decl_span, "declared here"),
            );
            return;
        };
        self.uses.push((name.span, Res::Def(def)));
    }

    /// Whether a dot-call's receiver is a bare name resolving through the
    /// type namespace: no local and no value-namespace def shadows it
    /// (the ordinary name-use priority), and a type-namespace def names
    /// it — the package's own declarations first, the file's import
    /// bindings below, the same priority a type position spells.
    /// Such a receiver is a type, not a value — the type-keyed call tier
    /// owns it, so a linked type name refuses its member calls at the
    /// check instead of falling to the value tiers. An instantiated
    /// application takes the same tier: the receiver's head (a
    /// constructor-bracket application or a carried application type)
    /// names the type whose own impl members the call dispatches.
    fn type_keyed_receiver(&self, recv: &Expr) -> Option<DefId> {
        if let Some(only) = single_path_ident(recv) {
            if self.lookup_local(&only.name).is_some() {
                return None;
            }
            let def = self
                .package
                .lookup_type(&only.name)
                .or_else(|| self.import_type_binding(&only.name))?;
            return self
                .package
                .def(def)
                .kind
                .in_type_namespace()
                .then_some(def);
        }
        self.applied_receiver_head(recv)
    }

    /// The head's def of an instantiated type-keyed receiver — a type
    /// application in the receiver position: the one-argument bracket
    /// (`stack[int]`), the two-argument bracket (the grammar spells it
    /// `map[k, v]`; a synthesized tree carries any head), or a written
    /// application type carried by the fn-type node (the spelling the
    /// brackets cannot express: three or more arguments, an optional
    /// argument). The head names a type with no local shadow, the
    /// package's namespace first and the file's import bindings below —
    /// exactly the bare form's discipline.
    fn applied_receiver_head(&self, recv: &Expr) -> Option<DefId> {
        let head = match recv {
            Expr::Index { recv, .. } => single_path_ident(recv)?,
            Expr::MapApply { head, .. } => head,
            Expr::FnType { ty, .. } => {
                let Type::Path { path, args, .. } = ty.as_ref() else {
                    return None;
                };
                if path.segments.len() != 1 || args.is_empty() {
                    return None;
                }
                &path.segments[0]
            },
            _ => return None,
        };
        if self.lookup_local(&head.name).is_some() {
            return None;
        }
        let def = self
            .package
            .lookup_type(&head.name)
            .or_else(|| self.import_type_binding(&head.name))?;
        self.package
            .def(def)
            .kind
            .in_type_namespace()
            .then_some(def)
    }

    /// The receiver walk of a dot-call: an instantiated type-keyed
    /// receiver whose head names a type — the package's declarations or
    /// the file's import bindings — walks as type uses (the
    /// constructor-bracket convention), while every other shape — the
    /// map application and a carried fn type included, whose own arms
    /// already read as type uses — keeps the expression walk.
    fn dot_recv_use(&mut self, recv: &Expr) {
        let Expr::Index {
            recv: head, index, ..
        } = recv
        else {
            self.expr(recv);
            return;
        };
        let Some(head) = single_path_ident(head).filter(|id| {
            self.lookup_local(&id.name).is_none()
                && (self.package.lookup_type(&id.name).is_some()
                    || self.import_type_binding(&id.name).is_some())
        }) else {
            self.expr(recv);
            return;
        };
        self.type_name_use(head);
        self.ctor_type_arg(index);
    }

    /// The method position behind a type-name receiver: the type's own
    /// inherent impl members with that name claim the spelling — the
    /// type-keyed tier, resolved at check time with no instance word. A
    /// tag or case name that names no member of its own type keeps its
    /// other reading (the constructor value: `tone.show()` dispatches
    /// the trait impl over the tag's value), so the ordinary value
    /// tiers serve it; any other type names its members alone.
    fn static_use(&mut self, recv: &Expr, target: DefId, name: &Ident) {
        let type_name = self.package.def(target).name.clone();
        // one pass serves both halves: the owner's own members claim the
        // call, and the rest feed the "exists on" note when none do
        let (candidates, others): (Vec<MethodId>, Vec<MethodId>) =
            self.package.method_candidates(&name.name).partition(|&id| {
                matches!(
                    &self.package.method(id).owner,
                    MethodOwner::Inherent(owner) if *owner == type_name
                )
            });
        if candidates.is_empty() {
            if matches!(self.package.def(target).kind, DefKind::Tag | DefKind::Case) {
                // The type name's constructor value is the receiver: a
                // resolved name, so the field tier's unresolved-receiver
                // exemption never fires here.
                self.dot_use_value(recv, name);
                return;
            }
            let others = others
                .iter()
                .map(|&id| self.package.method(id).owner_name(self.package))
                .collect::<Vec<_>>();
            let mut diag = Diagnostic::error(
                ErrorCode::UnresolvedName,
                format!("type `{type_name}` carries no member `{}`", name.name),
            )
            .with_primary(name.span, "not a member of this type")
            .with_note(
                "a bare type name as the receiver calls that type's own receiverless impl \
                 members; other spellings need a value receiver",
            );
            if !others.is_empty() {
                diag = diag.with_note(format!(
                    "method `{}` exists on: {}",
                    name.name,
                    others.join(", ")
                ));
            }
            self.diags.push(diag);
            return;
        }
        self.uses
            .push((name.span, Res::Dot(DotRes::Static { candidates })));
    }

    /// The ordinary value tiers behind the dot: the handler
    /// forms, then free functions, then every registered method
    /// candidate, then the field tier's marker (the checker decides).
    /// A receiver that reaches here carries a value reading
    /// (a local, a value def, or a tag/case constructor value) — a bare
    /// name resolving to nothing excepted, where the field tier has no
    /// type to decide on and records nothing.
    fn dot_use_value(&mut self, recv: &Expr, name: &Ident) {
        if name.name == "err" || name.name == "ok" {
            self.uses.push((name.span, Res::Handler));
            return;
        }
        if let Some(func) = self.package.lookup_value(&name.name) {
            // A free fn private to another file cannot claim the dot
            // position: the receiver fallback would never reach the
            // members it shadows. The private def falls through to the
            // method tier here (the plain free call keeps its boundary
            // refusal at the value-name door), and the free reading
            // records only when no member answers the name, so the
            // refusal below names the def the privacy gate reports — a
            // dependency's own bodies resolve their own members against
            // a dependent's same-named private helper.
            let func_decl = self.package.def(func);
            let privatized = func_decl.vis == Vis::Private && func_decl.span.file != self.file;
            if !privatized {
                // Capability parameters have no syntax yet, so the receiver
                // feeds parameter 0 — the first data parameter.
                self.enforce_vis(name.span, func);
                self.uses
                    .push((name.span, Res::Dot(DotRes::Free { func, fed_param: 0 })));
                return;
            }
        }
        let candidates: Vec<MethodId> = self.package.method_candidates(&name.name).collect();
        if candidates.is_empty() {
            // No member answers the name: a privatized free fn keeps
            // its free reading so the privacy gate's refusal names it;
            // with no def at all the field tier decides — the receiver's
            // fn-typed field called through the dot, else the checker's
            // refused reading (the diagnostic lives with that decision,
            // which needs the receiver's type).
            if let Some(func) = self.package.lookup_value(&name.name) {
                self.enforce_vis(name.span, func);
                self.uses
                    .push((name.span, Res::Dot(DotRes::Free { func, fed_param: 0 })));
                return;
            }
            if self.unresolved_receiver(recv) {
                return;
            }
            self.uses.push((name.span, Res::Dot(DotRes::Field)));
            return;
        }
        self.uses
            .push((name.span, Res::Dot(DotRes::Methods { candidates })));
    }

    // ---- type positions ----------------------------------------------------

    fn type_(&mut self, ty: &Type) {
        match ty {
            Type::Path { path, args, .. } => {
                match path.segments.as_slice() {
                    [only] => self.type_name_use(only),
                    [head, tail] => {
                        let routed = self
                            .package
                            .lookup_type_in(self.file, &head.name)
                            .is_some_and(|def| self.qualified_label_use(def, head, tail));
                        if !routed {
                            self.diags.push(
                                Diagnostic::error(
                                    ErrorCode::UnresolvedName,
                                    "qualified type paths resolve through imports, which land \
                                     with modules [G4]",
                                )
                                .with_primary(path.span, "qualified type path"),
                            );
                        }
                    },
                    _ => {
                        self.diags.push(
                            Diagnostic::error(
                                ErrorCode::UnresolvedName,
                                "qualified type paths resolve through imports, which land with \
                                 modules [G4]",
                            )
                            .with_primary(path.span, "qualified type path"),
                        );
                    },
                }
                for arg in args {
                    self.type_(arg);
                }
            },
            Type::Optional { inner, .. }
            | Type::Negation { inner, .. }
            | Type::Paren { inner, .. }
            | Type::Ptr { inner, .. } => self.type_(inner),
            Type::Union { left, right, .. }
            | Type::Intersect { left, right, .. }
            | Type::Difference { left, right, .. } => {
                self.type_(left);
                self.type_(right);
            },
            Type::Tuple { items, .. } => {
                for item in items {
                    self.type_(item);
                }
            },
            Type::Fn { params, ret, .. } => {
                for param in params {
                    if let Some(ty) = &param.ty {
                        self.type_(ty);
                    }
                }
                if let Some(ret) = ret {
                    self.type_(ret);
                }
            },
            Type::Unit(_) | Type::TypeKw(_) | Type::Foreign { .. } => {},
        }
    }

    fn type_name_use(&mut self, name: &Ident) {
        if let Some(bind) = self.lookup_type_var(&name.name) {
            self.uses.push((name.span, Res::Local { bind }));
        } else if let Some(def) = self.package.lookup_type_in(self.file, &name.name) {
            self.def_use(name.span, def);
        } else if name.name == "none" {
            // `none` is the universe's distinguished unit atom, not a
            // package declaration; the checker solves it directly.
        } else if name.name == "self" {
            // `self` in a type position refuses; the receiver type is
            // spelled `Self`.
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::LowercaseSelfType,
                    "`self` is retired as a type name; the receiver type is spelled `Self`",
                )
                .with_primary(name.span, "type name `self`")
                .with_suggestion(Suggestion::Replace {
                    span: name.span,
                    with: "Self".to_owned(),
                }),
            );
        } else if let Some(def) = self.package.lookup_case_in(self.file, &name.name) {
            // a case label keeps its type-side reading (the directed
            // annotations, unions, and `is` tests ride it); it claims no
            // entry of the shared type namespace, so a declaration of
            // the same label coexists beside it
            self.def_use(name.span, def);
        } else if let Some(def) = self.import_type_binding(&name.name) {
            // a linked type's selection: the sentinels above keep
            // their readings before any imported spelling
            self.def_use(name.span, def);
        } else {
            let candidates = self.type_candidates();
            self.unknown_name_at(name, candidates, "type");
        }
    }

    // ---- patterns ----------------------------------------------------------

    /// Binds a pattern's binders into the current scope and resolves its
    /// case paths. `or_rebind` allows identical rebinding across
    /// or-pattern alternatives.
    fn bind_pattern(&mut self, pat: &Pattern, or_rebind: bool) {
        self.pattern_at(pat, or_rebind, false);
    }

    /// A match-arm pattern: sub-patterns are member tests first, bindings
    /// second.
    fn arm_pattern(&mut self, pat: &Pattern) {
        self.pattern_at(pat, false, true);
    }

    /// One pattern walk shared by binding declarations (`arm == false`)
    /// and match-arm tests (`arm == true`): in a test position a single
    /// identifier that names a constructor type (or the predeclared
    /// `none`) is a member test that binds nothing — bare tags are
    /// constructor values, so the type namespace wins over any
    /// same-named local; identifiers with no constructor entry bind.
    fn pattern_at(&mut self, pat: &Pattern, or_rebind: bool, arm: bool) {
        match pat {
            Pattern::Wildcard(_)
            | Pattern::Literal { .. }
            | Pattern::Str { .. }
            | Pattern::RawStr { .. } => {},
            Pattern::Binding { name, .. } => match self.arm_tag(name, arm) {
                ArmTag::Binding => self.bind(name, or_rebind),
                ArmTag::BuiltinNone => {},
                ArmTag::Def(def) => {
                    self.def_use(name.span, def);
                },
            },
            // Pattern paths are item references, not locals.
            Pattern::Path { path, .. } => self.pattern_path(path),
            Pattern::Call { path, args, .. } => {
                self.pattern_path(path);
                for arg in args {
                    self.pattern_at(arg, or_rebind, arm);
                }
            },
            Pattern::Struct { path, fields, .. } => {
                self.pattern_path(path);
                for (name, field) in fields {
                    match field {
                        // Punning binds the field name.
                        None => self.bind(name, or_rebind),
                        Some(sub) => self.pattern_at(sub, or_rebind, arm),
                    }
                }
            },
            Pattern::Tuple { pats, .. } => {
                for sub in pats {
                    self.pattern_at(sub, or_rebind, arm);
                }
            },
            Pattern::Or { pats, .. } => {
                for sub in pats {
                    self.pattern_at(sub, true, arm);
                }
            },
        }
    }

    /// The type-namespace entry a bare arm identifier names: the
    /// predeclared `none`, a constructor type's def, or nothing (a
    /// binding). Traits and aliases are sets, not constructors.
    fn arm_tag(&self, name: &Ident, arm: bool) -> ArmTag {
        if arm && name.name == "none" && !self.package.declares("none") {
            return ArmTag::BuiltinNone;
        }
        if !arm {
            return ArmTag::Binding;
        }
        self.package
            .lookup_type_in(self.file, &name.name)
            .or_else(|| self.package.lookup_case_in(self.file, &name.name))
            .or_else(|| self.import_type_binding(&name.name))
            .filter(|&def| self.package.def(def).kind.is_constructor())
            .map_or(ArmTag::Binding, ArmTag::Def)
    }

    /// Pattern paths are constructor references: the type namespace
    /// only.
    fn pattern_path(&mut self, path: &Path) {
        let candidates = || {
            self.package
                .defs
                .iter()
                .filter(|def| def.kind.in_type_namespace())
                .map(|def| def.name.clone())
                .collect::<Vec<_>>()
        };
        match path.segments.as_slice() {
            [only] => {
                if let Some(def) = self
                    .package
                    .lookup_type_in(self.file, &only.name)
                    .or_else(|| self.package.lookup_case_in(self.file, &only.name))
                    .or_else(|| self.import_type_binding(&only.name))
                {
                    self.def_use(only.span, def);
                } else {
                    let candidates = candidates();
                    self.unknown_name_at(only, candidates, "pattern");
                }
            },
            // The tail is a case name; case tables are the checker's.
            // A tag-family head routes its label through the qualified
            // spelling instead.
            [head, tail] => {
                if let Some(def) = self
                    .package
                    .lookup_type_in(self.file, &head.name)
                    .or_else(|| self.import_type_binding(&head.name))
                {
                    if self.package.def(def).kind == DefKind::Enum {
                        self.def_use(head.span, def);
                    } else if !self.qualified_label_use(def, head, tail) {
                        let candidates = candidates();
                        self.unknown_name_at(head, candidates, "pattern");
                    }
                } else {
                    let candidates = candidates();
                    self.unknown_name_at(head, candidates, "pattern");
                }
            },
            _ => {
                self.diags.push(
                    Diagnostic::error(
                        ErrorCode::UnresolvedName,
                        "multi-segment pattern paths are not part of the surface [F4]",
                    )
                    .with_primary(path.span, "too many segments"),
                );
            },
        }
    }

    // ---- scopes ----------------------------------------------------------

    fn bind(&mut self, name: &Ident, rebind_ok: bool) {
        let scope = self.scopes.last_mut().expect("a scope is always open");
        match scope.get(&name.name) {
            Some(&first) if !rebind_ok => {
                self.diags.push(
                    Diagnostic::error(
                        ErrorCode::DuplicateDefinition,
                        format!("`{}` is already bound in this scope", name.name),
                    )
                    .with_secondary(first, "first bound here")
                    .with_primary(name.span, "bound again here"),
                );
            },
            _ => {
                scope.insert(name.name.clone(), name.span);
            },
        }
    }

    /// Records a binder in `scope`, reporting same-scope duplicates.
    fn record_dup(&mut self, scope: &mut FxHashMap<String, Span>, name: &Ident) {
        if let Some(&first) = scope.get(&name.name) {
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::DuplicateDefinition,
                    format!("`{}` is already bound in this scope", name.name),
                )
                .with_secondary(first, "first bound here")
                .with_primary(name.span, "bound again here"),
            );
            return;
        }
        scope.insert(name.name.clone(), name.span);
    }

    fn lookup_local(&self, name: &str) -> Option<Span> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    fn lookup_type_var(&self, name: &str) -> Option<Span> {
        self.type_scopes
            .iter()
            .rev()
            .find_map(|s| s.get(name).copied())
    }

    // ---- diagnostics -------------------------------------------------------

    /// The qualified `type::label` routing: a type-family head
    /// (tag, case, enum, alias) routes its label to the declaring
    /// declaration the HEAD's own file names — the escape hatch the bare
    /// label's ambiguity refusal points at. Membership in the head's set
    /// is the checker's universe work, never the resolver's. Returns
    /// whether the pair took the route (the caller keeps its own
    /// refusal otherwise).
    fn qualified_label_use(&mut self, head_def: DefId, head: &Ident, tail: &Ident) -> bool {
        if !matches!(
            self.package.def(head_def).kind,
            DefKind::Tag | DefKind::Case | DefKind::Enum | DefKind::TypeAlias
        ) {
            return false;
        }
        let head_file = self.package.def(head_def).span.file;
        self.def_use(head.span, head_def);
        // the tail reads the head's declaring file's case namespace
        // first, a tag-family label's type entry second: cases claim no
        // entry of the shared type namespace
        if let Some(label_def) = self
            .package
            .lookup_case_in(head_file, &tail.name)
            .or_else(|| self.package.lookup_type_in(head_file, &tail.name))
            .filter(|&d| matches!(self.package.def(d).kind, DefKind::Tag | DefKind::Case))
        {
            self.def_use(tail.span, label_def);
        } else {
            self.unknown_name(tail, self.shadowing_candidates());
        }
        true
    }

    fn unknown_name(&mut self, name: &Ident, candidates: Vec<String>) {
        self.unknown_name_at(name, candidates, "name");
    }

    fn unknown_name_at(&mut self, name: &Ident, candidates: Vec<String>, what: &str) {
        let mut diag = Diagnostic::error(
            ErrorCode::UnresolvedName,
            format!("unknown {} `{}`", what, name.name),
        )
        .with_primary(name.span, "not found");
        if let Some(near) = nearest(&name.name, candidates) {
            diag = diag
                .with_note(format!("did you mean `{near}`?"))
                .with_suggestion(Suggestion::Replace {
                    span: name.span,
                    with: near,
                });
        }
        self.diags.push(diag);
    }

    /// Names visible for suggestions at a value use: package names plus
    /// every local in scope.
    fn shadowing_candidates(&self) -> Vec<String> {
        let mut names: Vec<String> = self.package.defs().map(|def| def.name.clone()).collect();
        for scope in &self.scopes {
            names.extend(scope.keys().cloned());
        }
        names
    }

    /// Names visible at a type use: package names plus type variables.
    fn type_candidates(&self) -> Vec<String> {
        let mut names: Vec<String> = self.package.defs().map(|def| def.name.clone()).collect();
        for scope in &self.type_scopes {
            names.extend(scope.keys().cloned());
        }
        names
    }
}
/// The identifier of a single-segment path expression, if it is one.
fn single_path_ident(expr: &Expr) -> Option<&Ident> {
    let Expr::Path { path, .. } = expr else {
        return None;
    };
    match path.segments.as_slice() {
        [only] => Some(only),
        _ => None,
    }
}

impl MethodDecl {
    /// The human-readable owner name for diagnostics: the trait's name or
    /// the inherent impl's target type.
    fn owner_name(&self, package: &Package) -> String {
        match &self.owner {
            MethodOwner::Trait(id) => package.def(*id).name.clone(),
            MethodOwner::Inherent(target) => format!("inherent impl `{target}`"),
        }
    }
}
