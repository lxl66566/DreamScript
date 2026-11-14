//! # ds-meta: the comptime evaluator [M6, G1, G2]
//!
//! Comptime-marked calls evaluate at compile time as ordinary code on
//! the same uop VM: the evaluator lowers the package's comptime
//! functions into one slice module ([`ds_lower::lower_comptime_slice`]),
//! installs it into a fresh fenced VM, and drives each call site's
//! arguments through it, recording the answers the runtime lowering
//! embeds at the call sites ([`ds_lower::ComptimeValues`]).
//!
//! The fence is the install seam: the evaluator's registry serves the
//! package's platform name with an empty provides list and the comptime
//! reflection natives under their reserved module, so a world-touching
//! call inside a comptime function refuses at the slice's install — a
//! compile error naming the capability, never a runtime trap
//! (`ComptimeFenceRefusal`). The pure builtin core serves everything
//! else the lowered subset reaches.
//!
//! Reflection is static over tags-as-data: the evaluator projects the
//! checker's declaration tables into the VM's reflection store (one
//! field record per declared field — name, type index, tag name), the
//! `fields`/`type_name` natives answer from it, and nothing of it
//! reaches a runtime module (G2).

use std::collections::HashMap;

use ds_check::{Analyzed, DefId, DotRes, Res, TypeExpr, TypeId};
use ds_diag::{Diagnostic, ErrorCode, Span};
use ds_lower::{ComptimeConst, ComptimeValues};
use ds_mod::Hash128;
use ds_parse::{
    Ast, BlockExpr, CallArg, Expr, FnDecl, Item, ItemKind, ParamMode, Path, Stmt, StmtKind,
    StrPart, TraitMember,
};
use ds_vm::{
    ModuleRegistry, Value, Vm,
    comptime::{COMPTIME_MODULE, ComptimeField, ComptimeType},
    loader::LoadError,
    platform::Platform,
};

/// Evaluates every comptime call in the package: the answers keyed by
/// each call's span, or the diagnostics that refuse the evaluation. The
/// `module_name` and `source_hash` are the runtime module's own — the
/// slice is the same package's comptime half.
///
/// # Errors
///
/// One diagnostic per honest refusal: a world-touching call inside a
/// comptime function (the fence), a call into the runtime half of the
/// package, an argument shape the round does not stage, a result it
/// cannot embed, or a trap the fenced run raised.
pub fn evaluate(
    module_name: &str,
    source_hash: Hash128,
    asts: &[Ast],
    analyzed: &Analyzed,
) -> Result<ComptimeValues, Vec<Diagnostic>> {
    let sites = collect_sites(asts, analyzed);
    if sites.is_empty() {
        return Ok(ComptimeValues::default());
    }
    let fns = comptime_functions(asts, analyzed);
    let mut reflection = Reflection::new(analyzed);
    let slice = ds_lower::lower_comptime_slice(module_name, source_hash, asts, analyzed).map_err(
        |error| {
            let diagnostic = Diagnostic::error(
                ErrorCode::ComptimeEvalRefusal,
                format!("the comptime functions do not lower: {error}"),
            );
            match error.span() {
                Some(span) => {
                    vec![diagnostic.with_primary(span, "outside the comptime slice's contract")]
                },
                None => vec![diagnostic],
            }
        },
    )?;
    // Phase one registers every site's arguments (and their field
    // closures) into the reflection table BEFORE the table reaches the
    // VM — the natives answer from the installed snapshot, so a later
    // registration would be invisible to the runs.
    let mut staged_sites = Vec::with_capacity(sites.len());
    let mut diags = Vec::new();
    for site in &sites {
        let mut staged = Vec::with_capacity(site.args.len());
        let mut broken = None;
        for arg in &site.args {
            match reflection.stage(arg) {
                Ok(index) => staged.push(Value::Tag(index)),
                Err(message) => {
                    broken = Some(message);
                    break;
                },
            }
        }
        if let Some(message) = broken {
            diags.push(
                Diagnostic::error(ErrorCode::ComptimeEvalRefusal, message)
                    .with_primary(site.span, "the comptime call's argument"),
            );
            continue;
        }
        staged_sites.push((site, staged));
    }
    if !diags.is_empty() {
        return Err(diags);
    }
    // the fence: the platform name serves nothing, so a world call in
    // the slice refuses at the install below
    let mut registry = ModuleRegistry::new();
    registry.install_platform(Platform::new(ds_lower::platform_name(asts), &[]));
    registry.install_native(COMPTIME_MODULE, ds_vm::comptime::comptime_module());
    let mut vm = Vm::new(registry);
    let guard = ds_vm::comptime::set_reflection(reflection.table_mut());
    let install = vm.install_dsmod(slice.into_module());
    if let Err(error) = install {
        drop(guard);
        return Err(vec![fence_diagnostic(&error, &fns, &sites)]);
    }
    let mut values = ComptimeValues::default();
    for (site, staged) in staged_sites {
        let Some(function) = fns.iter().find(|f| f.def == site.def) else {
            continue;
        };
        match vm.run(module_name, &function.name, &staged) {
            Ok(value) => match embed(value, &vm) {
                Ok(constant) => values.insert(site.span, constant),
                Err(message) => diags.push(
                    Diagnostic::error(ErrorCode::ComptimeEvalRefusal, message)
                        .with_primary(site.span, "the comptime call's answer"),
                ),
            },
            Err(trap) => diags.push(
                Diagnostic::error(
                    ErrorCode::ComptimeEvalRefusal,
                    format!("the comptime evaluation trapped: {trap:?}"),
                )
                .with_primary(site.span, "the comptime call"),
            ),
        }
    }
    drop(guard);
    if diags.is_empty() { Ok(values) } else { Err(diags) }
}

/// One comptime call site: the callee's definition, the call's span, and
/// the argument expressions in parameter order.
struct CallSite {
    def: DefId,
    span: Span,
    args: Vec<Expr>,
}

/// One declared comptime function: its definition, its top-level name
/// (the slice's function-table entry), and its own span for diagnostics.
struct ComptimeFn {
    def: DefId,
    name: String,
    span: Span,
}

/// Every call whose callee is a comptime function, at the positions the
/// runtime module lowers: direct name calls and the UFCS dot form (the
/// receiver stages as the first argument). A comptime function's own
/// body is the evaluator's, not a lowered position — its nested comptime
/// calls are ordinary intra-module calls inside the fenced run, so the
/// walk skips those trees.
fn collect_sites(asts: &[Ast], analyzed: &Analyzed) -> Vec<CallSite> {
    let mut sites = Vec::new();
    for (index, ast) in asts.iter().enumerate() {
        let res = &analyzed.resolutions[index];
        for item in items_of(ast) {
            if is_comptime_declaration(item) {
                continue;
            }
            walk_item(item, &mut |expr: &Expr| {
                let Expr::Call {
                    callee, args, span, ..
                } = expr
                else {
                    return;
                };
                let def = match callee.as_ref() {
                    Expr::Path { path, .. } => match path_target(path, res) {
                        Some(Res::Def(def)) => *def,
                        _ => return,
                    },
                    Expr::Field { name, .. } => match res.get(name.span) {
                        Some(Res::Dot(DotRes::Free { func, fed_param })) if *fed_param == 0 => {
                            *func
                        },
                        _ => return,
                    },
                    _ => return,
                };
                let is_comptime = analyzed.decls.fns.get(&def).is_some_and(|sig| {
                    sig.params
                        .iter()
                        .any(|p| matches!(p.mode, ParamMode::Comptime(_)))
                });
                if is_comptime {
                    sites.push(CallSite {
                        def,
                        span: *span,
                        args: args.iter().map(|arg| arg.value.clone()).collect(),
                    });
                }
            });
        }
    }
    sites
}

/// Whether a top-level item is a comptime fn declaration (its body is
/// the evaluator's, never a lowered position).
fn is_comptime_declaration(item: &Item) -> bool {
    let ItemKind::Local(local) = &item.kind else {
        return false;
    };
    let Expr::Lambda { params, .. } = &local.init else {
        return false;
    };
    params
        .iter()
        .any(|p| matches!(p.mode, ParamMode::Comptime(_)))
}

/// The package's declared comptime functions, in declaration order.
fn comptime_functions(asts: &[Ast], analyzed: &Analyzed) -> Vec<ComptimeFn> {
    let mut fns = Vec::new();
    for item in asts.iter().flat_map(items_of) {
        let ItemKind::Local(local) = &item.kind else {
            continue;
        };
        let ds_parse::Pattern::Binding { name, .. } = &local.pat else {
            continue;
        };
        let Expr::Lambda { params, .. } = &local.init else {
            continue;
        };
        if !params
            .iter()
            .any(|p| matches!(p.mode, ParamMode::Comptime(_)))
        {
            continue;
        }
        let Some(def) = analyzed.package.lookup(&name.name) else {
            continue;
        };
        fns.push(ComptimeFn {
            def,
            name: name.name.clone(),
            span: name.span,
        });
    }
    fns
}

/// The reflection projection: declared atoms to reflection indices, with
/// composite field types rendered as nameable non-traversable entries.
struct Reflection<'a> {
    analyzed: &'a Analyzed,
    /// The VM's reflection table under construction.
    table: Vec<ComptimeType>,
    /// Declared atom to its reflection index.
    atoms: HashMap<TypeId, u8>,
    /// Rendered composite spelling to its synthetic index.
    composites: HashMap<String, u8>,
}

impl<'a> Reflection<'a> {
    fn new(analyzed: &'a Analyzed) -> Self {
        Self {
            analyzed,
            table: Vec::new(),
            atoms: HashMap::default(),
            composites: HashMap::default(),
        }
    }

    fn table_mut(&mut self) -> Vec<ComptimeType> {
        std::mem::take(&mut self.table)
    }

    /// The reflection index of a declared atom, registering it (and its
    /// field closure) on first reach.
    fn register_atom(&mut self, atom: TypeId) -> Option<u8> {
        if let Some(&index) = self.atoms.get(&atom) {
            return Some(index);
        }
        let decl = self.analyzed.decls.universe.atom_decl(atom).ok()?;
        let fields = match &decl.kind {
            ds_check::AtomKind::Struct(fields) => {
                let mut records = Vec::with_capacity(fields.len());
                for field in fields {
                    let type_index = self.register_field_type(&field.ty)?;
                    let tag = match &field.ty {
                        TypeExpr::Atom(id) => self
                            .analyzed
                            .decls
                            .universe
                            .atom_decl(*id)
                            .map(|decl| decl.name.clone())
                            .unwrap_or_default(),
                        _ => String::new(),
                    };
                    records.push(ComptimeField {
                        name: field.name.clone().unwrap_or_default(),
                        type_index,
                        tag,
                    });
                }
                records
            },
            // enums, tags, and structural atoms answer no fields this
            // round — enum case traversal is the reflection residual
            _ => Vec::new(),
        };
        let index = self.push(ComptimeType {
            name: decl.name.clone(),
            fields,
        })?;
        self.atoms.insert(atom, index);
        Some(index)
    }

    /// One field's declared type to a reflection index: a bare atom
    /// registers traversably, anything else renders to a nameable
    /// non-traversable entry.
    fn register_field_type(&mut self, ty: &TypeExpr) -> Option<u8> {
        if let TypeExpr::Atom(id) = ty {
            self.register_atom(*id)
        } else {
            let rendered = render_type_expr(ty, self.analyzed);
            if let Some(&index) = self.composites.get(&rendered) {
                return Some(index);
            }
            let index = self.push(ComptimeType {
                name: rendered.clone(),
                fields: Vec::new(),
            })?;
            self.composites.insert(rendered, index);
            Some(index)
        }
    }

    /// One staged argument expression to its reflection index: the
    /// round-one argument shape is a single type name.
    fn stage(&mut self, arg: &Expr) -> Result<u8, String> {
        let Expr::Path { path, .. } = arg else {
            return Err(
                "a comptime argument names a declared type this round; anything else (a \
                 parenthesized name, a local, an application like list[int]) is the evaluator's \
                 residual"
                    .to_owned(),
            );
        };
        let [only] = path.segments.as_slice() else {
            return Err("a comptime argument is one segment this round".to_owned());
        };
        let file = only.span.file.as_u32() as usize;
        let Some(def) = self
            .analyzed
            .resolutions
            .get(file)
            .and_then(|res| res.get(only.span))
            .and_then(|res| match res {
                Res::Def(def) => Some(*def),
                _ => None,
            })
        else {
            return Err(format!("`{}` does not name a declared type", only.name));
        };
        let atom = self
            .analyzed
            .decls
            .atoms
            .get(&def)
            .copied()
            .ok_or_else(|| format!("`{}` does not name a declared type", only.name))?;
        self.register_atom(atom)
            .ok_or_else(|| format!("`{}` has no reflection entry", only.name))
    }

    fn push(&mut self, entry: ComptimeType) -> Option<u8> {
        let index = u8::try_from(self.table.len()).ok()?;
        self.table.push(entry);
        Some(index)
    }
}

/// One embedded answer: the round-one embeddable results are the scalar
/// atoms and string.
fn embed(value: Value, vm: &Vm) -> Result<ComptimeConst, String> {
    match value {
        Value::I64(word) => Ok(ComptimeConst::Int(word)),
        Value::F64(word) => Ok(ComptimeConst::Float(word)),
        Value::Bool(word) => Ok(ComptimeConst::Bool(word)),
        Value::Ref(word) => {
            let text = vm
                .objects()
                .str_at(word.id())
                .map_err(|_| "the answer is not a string".to_owned())?;
            Ok(ComptimeConst::Str(text.to_owned()))
        },
        Value::Tag(_) | Value::Fn(_) | Value::Arena(_) => Err("the round-one embeddable answers \
                                                               are int, float, bool, and string; \
                                                               a comptime function answering \
                                                               anything else refuses here"
            .to_owned()),
    }
}

/// The fence and slice-install refusals as diagnostics: a world call
/// names the capability and the function whose pool held it; anything
/// else names the call the slice could not resolve.
fn fence_diagnostic(error: &LoadError, fns: &[ComptimeFn], sites: &[CallSite]) -> Diagnostic {
    match error {
        LoadError::UnprovidedPlatformOp {
            func, capability, ..
        } => {
            let primary = fns
                .iter()
                .find(|f| f.name == *func)
                .map_or(sites[0].span, |f| f.span);
            Diagnostic::error(
                ErrorCode::ComptimeFenceRefusal,
                format!(
                    "this comptime function calls the world surface (`{capability}`); the \
                     comptime fence refuses world-touching calls as compile errors [G1]"
                ),
            )
            .with_primary(primary, "the comptime function")
            .with_secondary(sites[0].span, "evaluated at this comptime call")
        },
        LoadError::UnresolvedCallee { func, module, .. } => Diagnostic::error(
            ErrorCode::ComptimeFenceRefusal,
            format!(
                "the comptime function `{func}` calls `{module}`, which the comptime slice does \
                 not carry; a comptime function may call the pure builtins and other comptime \
                 functions this round"
            ),
        )
        .with_primary(
            fns.first().map_or(sites[0].span, |f| f.span),
            "the comptime function",
        ),
        other => Diagnostic::error(
            ErrorCode::ComptimeEvalRefusal,
            format!("the comptime slice does not install: {other:?}"),
        ),
    }
}

/// A checker type expression rendered to its source-shaped spelling (the
/// `type_name` answer for composite field types).
fn render_type_expr(ty: &TypeExpr, analyzed: &Analyzed) -> String {
    let atom = |id: &TypeId| {
        analyzed
            .decls
            .universe
            .atom_decl(*id)
            .map_or_else(|_| "?".to_owned(), |decl| decl.name.clone())
    };
    match ty {
        TypeExpr::Atom(id) => atom(id),
        TypeExpr::Trait(id) => analyzed
            .decls
            .universe
            .trait_decl(*id)
            .map_or_else(|_| "?".to_owned(), |decl| decl.name.clone()),
        TypeExpr::Not(id) => format!(
            "~{}",
            analyzed
                .decls
                .universe
                .trait_decl(*id)
                .map_or_else(|_| "?".to_owned(), |decl| decl.name.clone())
        ),
        TypeExpr::Union(a, b) => {
            format!(
                "{} | {}",
                render_type_expr(a, analyzed),
                render_type_expr(b, analyzed)
            )
        },
        TypeExpr::Inter(a, b) => {
            format!(
                "{} & {}",
                render_type_expr(a, analyzed),
                render_type_expr(b, analyzed)
            )
        },
        TypeExpr::Diff(a, b) => {
            format!(
                "{} - {}",
                render_type_expr(a, analyzed),
                render_type_expr(b, analyzed)
            )
        },
        TypeExpr::Tuple(elements) => {
            let rendered: Vec<_> = elements
                .iter()
                .map(|element| render_type_expr(element, analyzed))
                .collect();
            format!("({})", rendered.join(", "))
        },
    }
}

/// One block's statements and tail, then each expression node in
/// evaluation-relevant position: the walk reaches every place a call can
/// appear, so the evaluator's site collection and any future comptime
/// marking share one traversal.
fn walk_block(block: &BlockExpr, go: &mut dyn FnMut(&Expr)) {
    for stmt in &block.stmts {
        walk_stmt(stmt, go);
    }
}

fn walk_stmt(stmt: &Stmt, go: &mut dyn FnMut(&Expr)) {
    match &stmt.kind {
        StmtKind::Local(local) => walk_expr(&local.init, go),
        StmtKind::Expr(expr) => walk_expr(expr, go),
    }
}

fn walk_item(item: &Item, go: &mut dyn FnMut(&Expr)) {
    match &item.kind {
        ItemKind::Local(local) => walk_expr(&local.init, go),
        ItemKind::Impl(imp) => {
            for f in &imp.fns {
                walk_fn(f, go);
            }
        },
        ItemKind::Trait(t) => {
            for member in &t.members {
                if let TraitMember::Fn(f) = member {
                    walk_fn(f, go);
                }
            }
        },
        _ => {},
    }
}

fn walk_fn(f: &FnDecl, go: &mut dyn FnMut(&Expr)) {
    if let Some(body) = &f.body {
        walk_block(body, go);
    }
}

fn items_of(ast: &Ast) -> impl Iterator<Item = &Item> {
    ast.blocks.iter().flat_map(|block| block.items.iter())
}

fn walk_expr(expr: &Expr, go: &mut dyn FnMut(&Expr)) {
    go(expr);
    match expr {
        Expr::Literal { .. }
        | Expr::RawStr { .. }
        | Expr::Path { .. }
        | Expr::FnType { .. }
        | Expr::Break(_)
        | Expr::Continue(_) => {},
        Expr::Str { parts, .. } => {
            for part in parts {
                match part {
                    StrPart::Text { .. } | StrPart::Ident { .. } => {},
                    StrPart::Expr { expr, .. } => walk_expr(expr, go),
                }
            }
        },
        Expr::Lambda { params, body, .. } => {
            for param in params {
                if let Some(default) = &param.default {
                    walk_expr(default, go);
                }
            }
            walk_expr(body, go);
        },
        Expr::Struct { fields, .. } => {
            for (_, value) in fields {
                if let Some(value) = value {
                    walk_expr(value, go);
                }
            }
        },
        Expr::Tuple { items, .. } | Expr::List { items, .. } => {
            for item in items {
                walk_expr(item, go);
            }
        },
        Expr::Paren { inner, .. } | Expr::Try { expr: inner, .. } => walk_expr(inner, go),
        Expr::Call {
            callee,
            args,
            trailing,
            ..
        } => {
            walk_expr(callee, go);
            walk_args(args, go);
            if let Some(block) = trailing {
                walk_expr(block, go);
            }
        },
        Expr::Field { recv, .. } => walk_expr(recv, go),
        Expr::Index { recv, index, .. } => {
            walk_expr(recv, go);
            walk_expr(index, go);
        },
        Expr::MapApply { key, value, .. } => {
            walk_expr(key, go);
            walk_expr(value, go);
        },
        Expr::Unary { expr, .. } | Expr::Is { expr, .. } => walk_expr(expr, go),
        Expr::Binary { left, right, .. } | Expr::Assign { left, right, .. } => {
            walk_expr(left, go);
            walk_expr(right, go);
        },
        Expr::Range { start, end, .. } => {
            walk_expr(start, go);
            walk_expr(end, go);
        },
        Expr::If {
            cond, then, else_, ..
        } => {
            walk_expr(cond, go);
            walk_block(then, go);
            if let Some(else_) = else_ {
                walk_expr(else_, go);
            }
        },
        Expr::Match {
            scrutinee, arms, ..
        } => {
            walk_expr(scrutinee, go);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    walk_expr(guard, go);
                }
                walk_expr(&arm.body, go);
            }
        },
        Expr::While { cond, body, .. } => {
            walk_expr(cond, go);
            walk_block(body, go);
        },
        Expr::For { iter, body, .. } => {
            walk_expr(iter, go);
            walk_expr(body, go);
        },
        Expr::Return { expr, .. } => {
            if let Some(expr) = expr {
                walk_expr(expr, go);
            }
        },
        Expr::Nursery { body, .. } | Expr::Spawn { body, .. } | Expr::Defer { body, .. } => {
            walk_block(body, go);
        },
        Expr::Block(block) => walk_block(block, go),
    }
}

fn walk_args(args: &[CallArg], go: &mut dyn FnMut(&Expr)) {
    for arg in args {
        walk_expr(&arg.value, go);
    }
}

/// A path's target resolution, when the path is a single name.
fn path_target<'r>(path: &Path, res: &'r ds_check::Resolutions) -> Option<&'r Res> {
    let [only] = path.segments.as_slice() else {
        return None;
    };
    res.get(only.span)
}
