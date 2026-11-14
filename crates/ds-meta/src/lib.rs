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
//! field record per declared field, one case record per declared enum
//! case — name, type index, tag name), the `fields`/`type_name` natives
//! answer from it, and nothing of it reaches a runtime module (G2).

use std::collections::HashMap;

use ds_check::{Analyzed, DefId, DotRes, Res, Ty, TypeExpr, TypeId};
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
    // the fenced VM exists before the staging: a scalar comptime
    // parameter stages a plain literal word, and a string literal
    // argument interns into the VM's root heap (the host-facing staging
    // discipline). The fence is still the install seam below — the
    // platform name serves nothing, so a world call in the slice refuses
    // at the slice's own install.
    let mut registry = ModuleRegistry::new();
    registry.install_platform(Platform::new(ds_lower::platform_name(asts), &[]));
    registry.install_native(COMPTIME_MODULE, ds_vm::comptime::comptime_module());
    let mut vm = Vm::new(registry);
    // Phase one registers every site's arguments (and their field
    // closures) into the reflection table BEFORE the table reaches the
    // VM — the natives answer from the installed snapshot, so a later
    // registration would be invisible to the runs.
    let mut staged_sites = Vec::with_capacity(sites.len());
    let mut diags = Vec::new();
    for site in &sites {
        let params = analyzed
            .decls
            .fns
            .get(&site.def)
            .map(|sig| sig.params.clone())
            .unwrap_or_default();
        let mut staged = Vec::with_capacity(site.args.len());
        let mut broken = None;
        if site.args.len() != params.len() {
            broken = Some(
                "a comptime call stages one plain argument per parameter this round; an omitted \
                 or defaulted parameter refuses here"
                    .to_owned(),
            );
        }
        for (param, arg) in params.iter().zip(&site.args) {
            match reflection.stage(&mut vm, param.ty.as_ref(), arg) {
                Ok(value) => staged.push(value),
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
            Ok(value) => {
                // the call's own checked type gates the list embed (the
                // runtime lowering stages each element literal from it)
                let call_ty = analyzed.checked.expr_types.get(&site.span).map(|v| v.ty);
                match embed(value, call_ty, analyzed, &vm) {
                    Ok(constant) => values.insert(site.span, constant),
                    Err(message) => diags.push(
                        Diagnostic::error(ErrorCode::ComptimeEvalRefusal, message)
                            .with_primary(site.span, "the comptime call's answer"),
                    ),
                }
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

/// The reflection projection: declared atoms and enums to reflection
/// indices, with composite field types rendered as nameable
/// non-traversable entries.
struct Reflection<'a> {
    analyzed: &'a Analyzed,
    /// The VM's reflection table under construction.
    table: Vec<ComptimeType>,
    /// Declared atom to its reflection index.
    atoms: HashMap<TypeId, u8>,
    /// Declared non-generic enum to its reflection index.
    enums: HashMap<DefId, u8>,
    /// Rendered composite spelling to its synthetic index.
    composites: HashMap<String, u8>,
}

impl<'a> Reflection<'a> {
    fn new(analyzed: &'a Analyzed) -> Self {
        Self {
            analyzed,
            table: Vec::new(),
            atoms: HashMap::default(),
            enums: HashMap::default(),
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
            // generic enums (opaque atoms), tags, and structural atoms
            // answer no fields — reflection over generic declarations
            // waits on substitution (a non-generic enum reaches
            // `register_enum` instead: it owns no atom of its own, its
            // name naming the union of its minted cases)
            _ => Vec::new(),
        };
        let index = self.push(ComptimeType {
            name: decl.name.clone(),
            fields,
        })?;
        self.atoms.insert(atom, index);
        Some(index)
    }

    /// The reflection index of a declared non-generic enum, registering
    /// one case record per case on first reach: `name` the case's
    /// declared name, `type` its payload — the case's own minted atom
    /// (a minted payload case IS a struct atom, traversable like any
    /// struct; the predeclared `none` atom when the case constructs
    /// bare), `tag` the constructor tag its values carry at runtime
    /// (the case's own name).
    fn register_enum(&mut self, def: DefId) -> Option<u8> {
        if let Some(&index) = self.enums.get(&def) {
            return Some(index);
        }
        let cases = self.analyzed.decls.enums.get(&def)?.cases.clone();
        let mut records = Vec::with_capacity(cases.len());
        for (name, span, payload) in &cases {
            let type_index = match payload {
                ds_parse::EnumPayload::None => self.register_atom(TypeId::NONE)?,
                ds_parse::EnumPayload::Tuple(_) | ds_parse::EnumPayload::Struct(_) => {
                    let case_def = self
                        .analyzed
                        .package
                        .lookup_type(name)
                        .filter(|&d| self.analyzed.package.def(d).span == *span)?;
                    let atom = self.analyzed.decls.atoms.get(&case_def).copied()?;
                    self.register_atom(atom)?
                },
            };
            records.push(ComptimeField {
                name: name.clone(),
                type_index,
                tag: name.clone(),
            });
        }
        let name = self.analyzed.package.def(def).name.clone();
        let index = self.push(ComptimeType {
            name,
            fields: records,
        })?;
        self.enums.insert(def, index);
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

    /// One staged argument word: the parameter's annotation decides the
    /// shape — `type` stages a declared type's reflection index (the
    /// argument a single type name), one of the four scalar atoms stages
    /// a plain literal of its kind, and anything else has no comptime
    /// staging this round.
    fn stage(
        &mut self,
        vm: &mut Vm,
        annotation: Option<&ds_parse::Type>,
        arg: &Expr,
    ) -> Result<Value, String> {
        match (annotation, scalar_annotation(annotation)) {
            (Some(ds_parse::Type::TypeKw(_)), _) => self.stage_type(arg).map(Value::Tag),
            (_, Some(kind)) => stage_literal(vm, kind, arg),
            _ => Err(
                "a comptime parameter is annotated `type` or one of the scalar atoms int, float, \
                 bool, string; anything else refuses here"
                    .to_owned(),
            ),
        }
    }

    /// One staged type argument: the reflection index of the single type
    /// name the argument spells.
    fn stage_type(&mut self, arg: &Expr) -> Result<u8, String> {
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
        // A declared type's reflection entry: an atom (structs, tags,
        // generic enums' opaque atoms) or a non-generic enum, which owns
        // no atom of its own — its name names the union of its minted
        // cases, so its entry carries one case record per case.
        let index = if let Some(&atom) = self.analyzed.decls.atoms.get(&def) {
            self.register_atom(atom)
        } else if self.analyzed.decls.enums.get(&def).is_some() {
            self.register_enum(def)
        } else {
            None
        };
        index.ok_or_else(|| format!("`{}` has no reflection entry", only.name))
    }

    fn push(&mut self, entry: ComptimeType) -> Option<u8> {
        let index = u8::try_from(self.table.len()).ok()?;
        self.table.push(entry);
        Some(index)
    }
}

/// The scalar kind one comptime parameter's annotation names, when it
/// names one of the four atoms the evaluator stages as a literal word.
enum ScalarKind {
    Int,
    Float,
    Bool,
    Str,
}

/// The scalar annotation of a comptime parameter: a bare one-segment
/// path naming one of the four scalar atoms (the checker's declaration
/// gate admits exactly these beside `type`).
fn scalar_annotation(annotation: Option<&ds_parse::Type>) -> Option<ScalarKind> {
    let ds_parse::Type::Path { path, args, .. } = annotation? else {
        return None;
    };
    if !args.is_empty() || path.segments.len() != 1 {
        return None;
    }
    match path.segments[0].name.as_str() {
        "int" => Some(ScalarKind::Int),
        "float" => Some(ScalarKind::Float),
        "bool" => Some(ScalarKind::Bool),
        "string" => Some(ScalarKind::Str),
        _ => None,
    }
}

/// One plain scalar literal staged as its word, a string interning into
/// the VM's root heap (the host-facing staging discipline).
fn stage_literal(vm: &mut Vm, kind: ScalarKind, arg: &Expr) -> Result<Value, String> {
    const SHAPE: &str = "a scalar comptime parameter takes a plain literal of its annotation; \
                         anything else (a parenthesized literal, a name, an interpolated or \
                         suffixed literal) refuses here";
    match (kind, arg) {
        (
            ScalarKind::Int,
            Expr::Literal {
                lit: ds_parse::Literal::Int { suffix, text },
                ..
            },
        ) if suffix.is_empty() => parse_int_word(text)
            .map(Value::I64)
            .ok_or_else(|| SHAPE.to_owned()),
        (
            ScalarKind::Float,
            Expr::Literal {
                lit: ds_parse::Literal::Float { suffix, text },
                ..
            },
        ) if suffix.is_empty() => text
            .parse::<f64>()
            .map(Value::F64)
            .map_err(|_| SHAPE.to_owned()),
        (
            ScalarKind::Bool,
            Expr::Literal {
                lit: ds_parse::Literal::Bool(word),
                ..
            },
        ) => Ok(Value::Bool(*word)),
        (ScalarKind::Str, Expr::Str { parts, .. }) => {
            let mut text = String::new();
            for part in parts {
                match part {
                    StrPart::Text { text: chunk, .. } => text.push_str(chunk),
                    _ => return Err(SHAPE.to_owned()),
                }
            }
            Ok(vm.intern_str(&text))
        },
        _ => Err(SHAPE.to_owned()),
    }
}

/// One integer literal's raw digit text (radix prefix kept, separators
/// already stripped by the parser) to its word [B7].
fn parse_int_word(text: &str) -> Option<i64> {
    let (radix, digits) = match text.as_bytes() {
        [b'0', b'x', ..] => (16, &text[2..]),
        [b'0', b'b', ..] => (2, &text[2..]),
        [b'0', b'o', ..] => (8, &text[2..]),
        _ => (10, text),
    };
    i64::from_str_radix(digits, radix).ok()
}

/// One embedded answer: the embeddable results are the scalar atoms,
/// string, and a list over embeddable elements (staged at the call site
/// as the list literal a user would have written).
fn embed(
    value: Value,
    call_ty: Option<Ty>,
    analyzed: &Analyzed,
    vm: &Vm,
) -> Result<ComptimeConst, String> {
    match value {
        Value::I64(word) => Ok(ComptimeConst::Int(word)),
        Value::F64(word) => Ok(ComptimeConst::Float(word)),
        Value::Bool(word) => Ok(ComptimeConst::Bool(word)),
        Value::Ref(word) => {
            let objects = vm.objects();
            if let Ok((elems, stride)) = objects.list_parts(word.id()) {
                return embed_list(elems, stride, call_ty, analyzed, objects);
            }
            let text = objects
                .str_at(word.id())
                .map_err(|_| "the answer is not a string".to_owned())?;
            Ok(ComptimeConst::Str(text.to_owned()))
        },
        Value::Tag(_) | Value::Fn(_) | Value::Arena(_) => Err("the embeddable answers are int, \
                                                               float, bool, string, and a list \
                                                               over embeddable elements; a \
                                                               comptime function answering \
                                                               anything else refuses here"
            .to_owned()),
    }
}

/// One list answer: embeddable when every element is one embeddable word
/// and the call's own checked type is the list application (the runtime
/// lowering stages each element literal from that type's argument, so
/// the gate refusing here is what keeps the embed inside the literal
/// vocabulary).
fn embed_list(
    elems: &[Value],
    stride: usize,
    call_ty: Option<Ty>,
    analyzed: &Analyzed,
    objects: &ds_vm::object::Objects,
) -> Result<ComptimeConst, String> {
    if stride > 1 {
        return Err(
            "a list answer embeds one embeddable word per element; a multi-word element (a \
             record, a tuple) refuses here"
                .to_owned(),
        );
    }
    let is_list_application = call_ty.is_some_and(|ty| {
        analyzed
            .decls
            .universe
            .members(ty)
            .first()
            .is_some_and(|&atom| {
                matches!(
                    analyzed.decls.universe.atom_decl(atom).map(|decl| &decl.kind),
                    Ok(ds_check::AtomKind::Apply(_, args)) if args.len() == 1
                )
            })
    });
    if !is_list_application {
        return Err(
            "a list answer embeds over the call's own list type; a comptime function answering a \
             list through a wider return type refuses here"
                .to_owned(),
        );
    }
    let mut items = Vec::with_capacity(elems.len());
    for elem in elems {
        items.push(match elem {
            Value::I64(word) => ComptimeConst::Int(*word),
            Value::F64(word) => ComptimeConst::Float(*word),
            Value::Bool(word) => ComptimeConst::Bool(*word),
            Value::Ref(word) => ComptimeConst::Str(
                objects
                    .str_at(word.id())
                    .map_err(|_| {
                        "the embeddable list elements are int, float, bool, and string".to_owned()
                    })?
                    .to_owned(),
            ),
            Value::Tag(_) | Value::Fn(_) | Value::Arena(_) => {
                return Err(
                    "the embeddable list elements are int, float, bool, and string; anything else \
                     refuses here"
                        .to_owned(),
                );
            },
        });
    }
    Ok(ComptimeConst::List(items))
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
