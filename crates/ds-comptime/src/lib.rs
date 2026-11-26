//! # ds-comptime: the comptime evaluator
//!
//! Comptime-marked calls evaluate at compile time as ordinary code on the
//! same uop VM: the evaluator lowers the package's comptime functions into
//! one slice module ([`ds_lower::lower_comptime_slice`]), installs it into
//! a fresh fenced VM, and drives each call site's arguments through it,
//! recording the answers the runtime lowering embeds at the call sites
//! ([`ds_lower::ComptimeValues`]).
//!
//! The fence is the admission plus the install seam: an ordinary runtime
//! callee admits by its own solved effect row — an empty row and a row of
//! `trap` alone admit the callee's body into the slice (trap is
//! deterministic and never the fence's business, a hit trap surfacing as
//! the comptime call's own compile error from the fenced run), every other
//! row refusing with `ComptimeCalleeRefusal` naming the comptime function,
//! the callee, and the call — and the registry still serves the package's
//! platform name with an empty provides list and the comptime reflection
//! natives under their reserved module, so a world-touching call the rows
//! cannot attribute refuses at the slice's install — a compile error naming
//! the capability, never a runtime trap (`ComptimeFenceRefusal`). The pure
//! builtin core serves everything else the lowered subset reaches.
//!
//! The dep-side const evaluation rides the same fence: a linked package's
//! exported computed consts (a `pub` data const whose initializer is no
//! plain literal) plan their initializers into the evaluation slice as
//! zero-parameter bodies, the fenced run answers each one's value, and the
//! embedded answer becomes the const's link fact — the consumer reads the
//! const as its value exactly like a plain one, and a reach the fence
//! refuses, a trap, or a runaway refuses the producer's artifact build
//! naming the const.
//!
//! Reflection is static over tags-as-data: the evaluator projects the
//! checker's declaration tables into the VM's reflection store (one field
//! record per declared field, one case record per declared enum case —
//! name, type index, tag name), the `fields`/`type_name` natives answer
//! from it, and nothing of it reaches a runtime module.

use std::collections::{HashMap, HashSet};

use ds_check::{Analyzed, DefId, DotRes, Effect, EffectRow, Res, Ty, TypeExpr, TypeId};
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

/// The fenced run's total step budget, one step per decoded instruction:
/// 2^28. Measured on the debug CLI, a 10^7-iteration comptime build loop
/// (~7e7 steps) evaluates in ~3 s, so the budget holds ~3.8x that step
/// count while a runaway fenced run answers in ~14 s instead of hanging.
const COMPTIME_FUEL: u64 = 1 << 28;

/// Evaluates every comptime call in the package: the answers keyed by
/// each call's span, or the diagnostics that refuse the evaluation. The
/// `module_name` and `source_hash` are the runtime module's own — the
/// slice is the same package's comptime half. `effects` is the
/// package's solved effect rows, the same computation the caller's
/// platform check read (ambient externs carry their declared rows
/// there), so the fence's admission reads the one row source of truth.
///
/// # Errors
///
/// One diagnostic per honest refusal: a runtime callee the fence
/// refuses (a row beyond pure and `trap`, a generic body, a member
/// call), a world-touching call the rows cannot attribute (the install
/// seam), an argument shape the round does not stage, a result it
/// cannot embed, or a trap the fenced run raised.
pub fn evaluate<T>(
    module_name: &str,
    source_hash: Hash128,
    asts: &[T],
    analyzed: &Analyzed,
    effects: &ds_check::Effects,
) -> Result<ComptimeValues, Vec<Diagnostic>>
where
    T: std::borrow::Borrow<Ast>,
{
    let sites = collect_sites(asts, analyzed);
    if sites.is_empty() {
        return Ok(ComptimeValues::default());
    }
    let fns = comptime_functions(asts, analyzed);
    let mut admission = Admission::new(asts, analyzed, effects, &[]);
    if let Some(diagnostic) = admission.check(&fns) {
        return Err(vec![diagnostic]);
    }
    let mut reflection = Reflection::new(analyzed);
    let slice = ds_lower::lower_comptime_slice(
        module_name,
        source_hash,
        asts,
        analyzed,
        admission.admitted(),
        &[],
    )
    .map_err(|error| {
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
    })?;
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
    // the fence's builtin row: the world-touching builtin core rows
    // drop out of the fenced VM's table, so the slice's load refuses
    // them as compile errors
    vm.install_native_module(
        ds_vm::native::BUILTIN_MODULE,
        ds_vm::comptime::fenced_builtin_module(),
    );
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
        match vm.run_with_budget(module_name, &function.name, &staged, COMPTIME_FUEL) {
            Ok(answer) => {
                // the call's own checked type gates the embed (the runtime
                // lowering stages each element literal from it); the host
                // seam answers the span whole, so every word reaches here
                let call_ty = analyzed.checked.expr_types.get(&site.span).map(|v| v.ty);
                match embed(answer.as_slice(), call_ty, analyzed, &vm) {
                    Ok(constant) => values.insert(site.span, constant),
                    Err(message) => diags.push(
                        Diagnostic::error(ErrorCode::ComptimeEvalRefusal, message)
                            .with_primary(site.span, "the comptime call's answer"),
                    ),
                }
            },
            Err(trap) => {
                // the budget's refusal names the runaway shape, so the
                // message reads the same on every host
                if matches!(trap, ds_vm::Trap::OutOfFuel) {
                    diags.push(
                        Diagnostic::error(
                            ErrorCode::ComptimeEvalRefusal,
                            format!(
                                "the comptime evaluation ran past its {COMPTIME_FUEL}-step \
                                 budget: a pure fn's non-terminating loop or recursion never \
                                 answers on its own"
                            ),
                        )
                        .with_primary(site.span, "the comptime call"),
                    );
                } else {
                    diags.push(
                        Diagnostic::error(
                            ErrorCode::ComptimeEvalRefusal,
                            format!("the comptime evaluation trapped: {trap:?}"),
                        )
                        .with_primary(site.span, "the comptime call"),
                    );
                }
            },
        }
    }
    drop(guard);
    if diags.is_empty() { Ok(values) } else { Err(diags) }
}

/// Evaluates the package's exported computed consts — the dep-side half
/// of the const link facts (G4): every `pub` data const of a member file
/// past the ambient band whose initializer is no plain literal plans its
/// initializer into the same evaluation slice the comptime path lowers,
/// the fenced VM answers each site's value, and the embedded answer
/// becomes the const's [`ConstFact`](ds_check::ConstFact) — the consumer
/// reads the const as its value exactly like a plain one. The answers
/// keep the plain fact's own vocabulary (the scalar words and a
/// homogeneous list over one scalar word); everything else — a tuple, a
/// nested answer — refuses here, honestly, the same vocabulary line the
/// literal fact holds. `ambient_files` is the compile's ambient band:
/// the ambient's own definitions never export.
///
/// # Errors
///
/// One diagnostic per honest refusal, each naming the const: a reach
/// the fence refuses (an impure row, a generic or member or unmanageable
/// callee, a deinit-carrying construction), a slice the initializer does
/// not lower into, a trap or runaway the fenced run raised, or an answer
/// outside the vocabulary. The refusals land at the producer's artifact
/// build, never a silent drop or a consumer-side mystery.
pub fn evaluate_consts<T>(
    module_name: &str,
    source_hash: Hash128,
    asts: &[T],
    analyzed: &Analyzed,
    effects: &ds_check::Effects,
    ambient_files: usize,
) -> Result<HashMap<DefId, ds_check::ConstFact>, Vec<Diagnostic>>
where
    T: std::borrow::Borrow<Ast>,
{
    let sites = collect_const_sites(asts, analyzed, ambient_files);
    if sites.is_empty() {
        return Ok(HashMap::default());
    }
    let mut admission = Admission::new(asts, analyzed, effects, &sites);
    if let Some(diagnostic) = admission.check(&comptime_functions(asts, analyzed)) {
        return Err(vec![diagnostic]);
    }
    let consts: Vec<DefId> = sites.iter().map(|site| site.def).collect();
    let slice = ds_lower::lower_comptime_slice(
        module_name,
        source_hash,
        asts,
        analyzed,
        admission.admitted(),
        &consts,
    )
    .map_err(|error| vec![slice_refusal(&error, &sites)])?;
    // the fenced VM the comptime path drives: the platform name serves
    // nothing (a world call in a slice refuses at its own install) and
    // the comptime reflection natives ride their reserved module
    let mut registry = ModuleRegistry::new();
    registry.install_platform(Platform::new(ds_lower::platform_name(asts), &[]));
    registry.install_native(COMPTIME_MODULE, ds_vm::comptime::comptime_module());
    let mut vm = Vm::new(registry);
    vm.install_native_module(
        ds_vm::native::BUILTIN_MODULE,
        ds_vm::comptime::fenced_builtin_module(),
    );
    let mut reflection = Reflection::new(analyzed);
    let guard = ds_vm::comptime::set_reflection(reflection.table_mut());
    let install = vm.install_dsmod(slice.into_module());
    if let Err(error) = install {
        drop(guard);
        return Err(vec![fence_refusal(&error, &sites)]);
    }
    let mut values = HashMap::default();
    let mut diags = Vec::new();
    for site in &sites {
        let ty = analyzed.checked.def_types.get(&site.def).copied();
        match vm.run_with_budget(module_name, &site.name, &[], COMPTIME_FUEL) {
            Ok(answer) if answer.as_slice().is_empty() => diags.push(site_refusal(
                site,
                "the initializer answers no word: a unit-typed const carries no value fact, so \
                 nothing embeds here",
            )),
            Ok(answer) => match embed(answer.as_slice(), ty, analyzed, &vm) {
                Ok(constant) => match const_fact(&constant) {
                    Some(fact) => {
                        values.insert(site.def, fact);
                    },
                    None => diags.push(site_refusal(
                        site,
                        "the initializer's answer is outside the const fact's vocabulary: the \
                         crossing values are int, float, bool, string, and a list over one scalar \
                         word of those; a tuple or nested answer refuses here",
                    )),
                },
                Err(message) => diags.push(site_refusal(site, &message)),
            },
            Err(ds_vm::Trap::OutOfFuel) => diags.push(site_refusal(
                site,
                &format!(
                    "the initializer's evaluation ran past its {COMPTIME_FUEL}-step budget: a \
                     pure initializer's non-terminating loop or recursion never answers on its own"
                ),
            )),
            Err(trap) => diags.push(site_refusal(
                site,
                &format!(
                    "the initializer trapped: {trap:?} — the value a dependent would read does \
                     not exist, so the artifact build refuses"
                ),
            )),
        }
    }
    drop(guard);
    if diags.is_empty() { Ok(values) } else { Err(diags) }
}

/// One dep-side evaluation site: a `pub` data const of a member file
/// whose initializer is no plain literal and no callable binding.
struct ConstSite {
    def: DefId,
    /// The binding's name — the slice's planned body runs under it.
    name: String,
    /// The binding name's span, the refusals' secondary.
    decl_span: Span,
    /// The initializer's span, the refusals' primary.
    span: Span,
}

/// Every dep-side evaluation site, in declaration order: the exported
/// (`pub`, G4's dependent-facing tier) immutable data consts defined
/// past the ambient band whose initializer carries no plain literal
/// fact. Callable bindings (a lambda or fn-typed initializer) ride the
/// callable clause; a plain literal's fact stands on its own; private
/// and `pub(pkg)` consts never cross to a dependent, so nothing
/// evaluates for them.
fn collect_const_sites<T>(asts: &[T], analyzed: &Analyzed, ambient_files: usize) -> Vec<ConstSite>
where
    T: std::borrow::Borrow<Ast>,
{
    let mut sites = Vec::new();
    for ast in asts.iter().skip(ambient_files) {
        for item in items_of(std::borrow::Borrow::borrow(ast)) {
            let ItemKind::Local(local) = &item.kind else {
                continue;
            };
            if local.mutable {
                continue;
            }
            let ds_parse::Pattern::Binding { name, .. } = &local.pat else {
                continue;
            };
            if matches!(local.init, Expr::Lambda { .. }) {
                continue; // a callable const rides the callable clause
            }
            if ds_check::ConstFact::of_expr(&local.init).is_some() {
                continue; // plain: the literal is the fact
            }
            let Some(def) = analyzed.package.lookup(&name.name) else {
                continue;
            };
            if analyzed.package.def(def).vis != ds_check::Vis::Pub {
                continue; // never dependent-facing, nothing evaluates
            }
            if analyzed
                .checked
                .def_types
                .get(&def)
                .copied()
                .is_some_and(|ty| fn_typed(ty, analyzed))
            {
                continue; // a fn-typed const is a callable binding
            }
            sites.push(ConstSite {
                def,
                name: name.name.clone(),
                decl_span: name.span,
                span: local.init.span(),
            });
        }
    }
    sites
}

/// Whether a checked type names exactly one fn-signature atom.
fn fn_typed(ty: Ty, analyzed: &Analyzed) -> bool {
    matches!(analyzed.decls.universe.members(ty), [only] if analyzed
        .decls
        .universe
        .atom_decl(*only)
        .is_ok_and(|decl| matches!(decl.kind, ds_check::AtomKind::FnSig(..))))
}

/// One embedded answer as the const link fact's own vocabulary: the
/// scalar words and a non-empty homogeneous list over one scalar word —
/// the same vocabulary a plain initializer's literal fact carries. A
/// tuple and an empty list answer `None` (the fact spells no tuple, and
/// an empty list has no element evidence — the literal spelling's own
/// line).
fn const_fact(constant: &ComptimeConst) -> Option<ds_check::ConstFact> {
    let scalar = |constant: &ComptimeConst| -> Option<ds_check::ConstFact> {
        match constant {
            ComptimeConst::Int(value) => Some(ds_check::ConstFact::Int(*value)),
            ComptimeConst::Float(value) => Some(ds_check::ConstFact::Float(*value)),
            ComptimeConst::Bool(value) => Some(ds_check::ConstFact::Bool(*value)),
            ComptimeConst::Str(text) => Some(ds_check::ConstFact::Str(text.clone())),
            ComptimeConst::List(_) | ComptimeConst::Tuple(_) => None,
        }
    };
    match constant {
        ComptimeConst::List(items) => {
            let [first, rest @ ..] = &items[..] else {
                return None;
            };
            let word = std::mem::discriminant(first);
            let mut facts = Vec::with_capacity(items.len());
            facts.push(scalar(first)?);
            for item in rest {
                if std::mem::discriminant(item) != word {
                    return None; // a mixed list spells no single element word
                }
                facts.push(scalar(item)?);
            }
            Some(ds_check::ConstFact::List(facts))
        },
        other => scalar(other),
    }
}

/// One evaluation site's refusal: the initializer primary, the binding
/// secondary, the const's own error code.
fn site_refusal(site: &ConstSite, message: &str) -> Diagnostic {
    Diagnostic::error(ErrorCode::LinkedConstEvalRefusal, message.to_owned())
        .with_primary(site.span, "the const's initializer")
        .with_secondary(site.decl_span, "the exported const")
}

/// A slice an initializer does not lower into: the lowering's own span
/// primary when it names one, the first site's initializer otherwise.
fn slice_refusal(error: &ds_lower::LowerError, sites: &[ConstSite]) -> Diagnostic {
    let diagnostic = Diagnostic::error(
        ErrorCode::LinkedConstEvalRefusal,
        format!(
            "an exported const's initializer does not lower inside the evaluation fence: {error}"
        ),
    );
    match error.span() {
        Some(span) => diagnostic.with_primary(span, "outside the evaluation slice's contract"),
        None => diagnostic.with_primary(sites[0].span, "the const's initializer"),
    }
}

/// The world-touching builtin rows the fenced table drops: each parks
/// the calling fiber against the clock — the timer race, the channel
/// deadline twins. Answers the row's ambient name for the fence
/// refusal, `None` for a row the fenced table serves.
fn dropped_clock_row(index: u32) -> Option<&'static str> {
    match index {
        ds_vm::native::BUILTIN_TIMEOUT => Some("timeout"),
        ds_vm::native::BUILTIN_CHAN_SEND_DEADLINE => Some("chan_send_deadline"),
        ds_vm::native::BUILTIN_CHAN_RECV_DEADLINE => Some("chan_recv_deadline"),
        _ => None,
    }
}

/// The evaluation slice's install refusal: a world call names the
/// capability, anything else the load error.
fn fence_refusal(error: &LoadError, sites: &[ConstSite]) -> Diagnostic {
    match error {
        LoadError::UnprovidedPlatformOp { capability, .. } => site_refusal(
            &sites[0],
            &format!(
                "the initializer calls the world surface (`{capability}`); the evaluation fence \
                 refuses world-touching calls as compile errors"
            ),
        ),
        // the world-touching builtin rows the fenced table drops: the
        // call parks the calling fiber against the clock, the same
        // world refusal the unserved platform ops take
        LoadError::UnresolvedCallee { module, index, .. }
            if *module == ds_vm::native::BUILTIN_MODULE
                && let Some(row) = dropped_clock_row(*index) =>
        {
            site_refusal(
                &sites[0],
                &format!(
                    "the initializer calls the world surface (`{row}`: the call parks the calling \
                     fiber against the clock); the evaluation fence refuses world-touching calls \
                     as compile errors"
                ),
            )
        },
        other => Diagnostic::error(
            ErrorCode::LinkedConstEvalRefusal,
            format!("the evaluation slice does not install: {other:?}"),
        )
        .with_primary(sites[0].span, "the const's initializer"),
    }
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
fn collect_sites<T>(asts: &[T], analyzed: &Analyzed) -> Vec<CallSite>
where
    T: std::borrow::Borrow<Ast>,
{
    let mut sites = Vec::new();
    for (index, ast) in asts.iter().enumerate() {
        let res = &analyzed.resolutions[index];
        for item in items_of(std::borrow::Borrow::borrow(ast)) {
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
fn comptime_functions<T>(asts: &[T], analyzed: &Analyzed) -> Vec<ComptimeFn>
where
    T: std::borrow::Borrow<Ast>,
{
    let mut fns = Vec::new();
    for item in asts
        .iter()
        .map(std::borrow::Borrow::<Ast>::borrow)
        .flat_map(items_of)
    {
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

/// The fence's row predicate (the 2026-11-17 amendment): a pure row
/// admits, and a row of `trap` alone admits — trap is deterministic and
/// never the fence's business, a trap the fenced run hits reporting as
/// the comptime call's own compile error. Every other row (a world row,
/// an error row, a nursery internal, or any of them beside `trap`)
/// refuses exactly as the 2026-11-15 ruling held.
fn row_admits(row: EffectRow) -> bool {
    row.minus(EffectRow::of(Effect::Trap)).is_pure()
}

/// The fence's callee admission (the 2026-11-15 ruling, the 2026-11-17
/// amendment): an ordinary runtime callee admits by its own solved row —
/// an empty (pure) row and a row of `trap` alone admit the callee's
/// body into the slice ([`row_admits`]), every other row refusing at
/// the fence naming the comptime fn, the callee, and the call. The rows
/// are the effects pass's solved fixpoint, so a direct callee's row
/// already carries its transitive closure: one refusing reach anywhere
/// behind a callee refuses that callee here. Trap never refuses here —
/// a trap the fenced run hits reports as the comptime call's own
/// compile error — so the pass's unsolved-type over-approximation (every
/// trapping operator rows `trap`, B7) fences nothing. An admitted
/// callee still refuses when the slice cannot plan what it reaches — a
/// generic body's evidence seam, a member call the slice plans no body
/// for, a binding shape no module function lowers — each an honest
/// residual of this round. The slice's lowering keeps its belt refusal
/// for the shapes this walk does not match (a fn-value indirection),
/// and the install seam keeps the world refusal (DS2033's capability
/// diagnostic) behind the admission.
/// One fence refusal's owner: the declaration the diagnostic names — a
/// comptime function or an exported const — with the error code its own
/// seam refuses under.
struct Owner {
    name: String,
    span: Span,
    kind: &'static str,
    code: ErrorCode,
}

struct Admission<'a> {
    analyzed: &'a Analyzed,
    effects: &'a ds_check::Effects,
    /// One plannable ordinary fn per def: its file and its lambda body.
    bodies: HashMap<DefId, (usize, &'a Expr)>,
    /// The comptime declarations the walk starts from: def, file, body.
    roots: Vec<(DefId, usize, &'a Expr)>,
    /// The dep-side evaluation sites the walk starts from: the const's
    /// name, its binding span, its file, and its initializer.
    const_roots: Vec<(String, Span, usize, &'a Expr)>,
    /// The admitted defs, in first-admit order — the slice's plan set.
    admitted: Vec<DefId>,
    /// Per-def admission state: present while a def is walked (a cycle
    /// admits, the fenced VM's stack bounding it), the value marking a
    /// finished admission.
    states: HashMap<DefId, bool>,
    /// Every struct whose declaration carries a `deinit` member (C5):
    /// the slice plans no hooks, so a literal of one refuses at the
    /// fence — the hook's side effects cannot silently skip at comptime.
    deinits: HashSet<DefId>,
}

impl<'a> Admission<'a> {
    fn new<T>(
        asts: &'a [T],
        analyzed: &'a Analyzed,
        effects: &'a ds_check::Effects,
        sites: &[ConstSite],
    ) -> Self
    where
        T: std::borrow::Borrow<Ast>,
    {
        let mut bodies = HashMap::default();
        let mut roots = Vec::new();
        let mut const_roots = Vec::new();
        let mut deinits = HashSet::default();
        for ast in asts.iter().map(std::borrow::Borrow::<Ast>::borrow) {
            for item in items_of(ast) {
                let ItemKind::Struct(decl) = &item.kind else {
                    continue;
                };
                if !decl
                    .members
                    .iter()
                    .any(|member| matches!(member, ds_parse::StructMember::Deinit { .. }))
                {
                    continue;
                }
                if let Some(def) = analyzed.package.lookup_type(&decl.name.name) {
                    deinits.insert(def);
                }
            }
        }
        for (index, ast) in asts
            .iter()
            .enumerate()
            .map(|(index, ast)| (index, std::borrow::Borrow::borrow(ast)))
        {
            for item in items_of(ast) {
                let ItemKind::Local(local) = &item.kind else {
                    continue;
                };
                let ds_parse::Pattern::Binding { name, .. } = &local.pat else {
                    continue;
                };
                let Some(def) = analyzed.package.lookup(&name.name) else {
                    continue;
                };
                let Expr::Lambda { params, .. } = &local.init else {
                    // a dep-side evaluation site's root: the initializer
                    // the fence walks the same way a comptime fn's body
                    if sites.iter().any(|site| site.def == def) {
                        const_roots.push((name.name.clone(), name.span, index, &local.init));
                    }
                    continue;
                };
                if params
                    .iter()
                    .any(|p| matches!(p.mode, ParamMode::Comptime(_)))
                {
                    roots.push((def, index, &local.init));
                } else if !local.mutable && is_runtime_fn(def, analyzed) {
                    bodies.insert(def, (index, &local.init));
                }
            }
        }
        Self {
            analyzed,
            effects,
            bodies,
            roots,
            const_roots,
            admitted: Vec::new(),
            states: HashMap::default(),
            deinits,
        }
    }

    /// The slice's plan set: every row-admitted callee the walk admitted.
    fn admitted(&self) -> &[DefId] {
        &self.admitted
    }

    /// Walks every comptime fn's and evaluation site's reaches; the
    /// first refusal wins.
    fn check(&mut self, fns: &[ComptimeFn]) -> Option<Diagnostic> {
        let analyzed = self.analyzed;
        let roots = self.roots.clone();
        for (def, file, body) in roots {
            let Some(f) = fns.iter().find(|f| f.def == def) else {
                continue;
            };
            let owner = Owner {
                name: f.name.clone(),
                span: f.span,
                kind: "comptime function",
                code: ErrorCode::ComptimeCalleeRefusal,
            };
            if let Some(diagnostic) = self.walk_root(&owner, file, body, analyzed) {
                return Some(diagnostic);
            }
        }
        let roots = self.const_roots.clone();
        for (name, span, file, body) in roots {
            let owner = Owner {
                name,
                span,
                kind: "exported const",
                code: ErrorCode::LinkedConstEvalRefusal,
            };
            if let Some(diagnostic) = self.walk_root(&owner, file, body, analyzed) {
                return Some(diagnostic);
            }
        }
        None
    }

    /// One root body's reach walk: the deinit-literal gate plus the
    /// callee admissions, the first refusal winning.
    fn walk_root(
        &mut self,
        owner: &Owner,
        file: usize,
        body: &Expr,
        analyzed: &Analyzed,
    ) -> Option<Diagnostic> {
        let res = &analyzed.resolutions[file];
        let mut found: Option<Diagnostic> = None;
        walk_expr(body, &mut |expr: &Expr| {
            if found.is_some() {
                return;
            }
            if let Some(diagnostic) = self.deinit_literal(expr, res, owner, None) {
                found = Some(diagnostic);
                return;
            }
            for (span, _, target) in reaches(res, expr) {
                if let Target::Fn(callee) = target
                    && is_runtime_fn(callee, analyzed)
                    && let Err(diagnostic) = self.admit(owner, callee, span)
                {
                    found = Some(diagnostic);
                    return;
                }
            }
        });
        found
    }

    /// Admits one ordinary callee: row-pure and slice-plannable, its own
    /// reaches admitting the same way (the rows close the loop, so a
    /// cycle only re-enters a def already proven pure).
    fn admit(&mut self, owner: &Owner, def: DefId, span: Span) -> Result<(), Diagnostic> {
        if self.states.contains_key(&def) {
            return Ok(()); // admitted, or a cycle re-entering a walk in progress
        }
        self.states.insert(def, false);
        let analyzed = self.analyzed;
        let name = analyzed.package.def(def).name.clone();
        if analyzed
            .decls
            .fns
            .get(&def)
            .is_some_and(|sig| !sig.generics.is_empty())
        {
            return Err(self.refusal(
                owner,
                span,
                def,
                format!(
                    "the {} `{}` reaches the generic function `{name}`, whose one-body evidence \
                     seam is the runtime module's this round",
                    owner.kind, owner.name
                ),
            ));
        }
        let Some(row) = self.effects.of(def) else {
            return Err(self.refusal(
                owner,
                span,
                def,
                format!(
                    "the {} `{}` reaches `{name}`, whose effect row the fence cannot read; the \
                     callee refuses here",
                    owner.kind, owner.name
                ),
            ));
        };
        if !row_admits(row) {
            let origin = self.origin(def);
            let origin_clause = (origin != def).then(|| {
                let origin_name = analyzed.package.def(origin).name.clone();
                format!(" — the row turns refusing at `{origin_name}` first")
            });
            let message = format!(
                "the {} `{}` reaches `{name}`, whose effect row is `{}`; the evaluation fence \
                 admits only callees with a pure or trap-only row{}",
                owner.kind,
                owner.name,
                row.render(),
                origin_clause.as_deref().unwrap_or("")
            );
            let mut diagnostic = self.refusal(owner, span, def, message);
            if origin != def {
                diagnostic = diagnostic.with_secondary(
                    analyzed.package.def(origin).span,
                    "the row's first refusing hop",
                );
            }
            return Err(diagnostic);
        }
        let Some((file, body)) = self.bodies.get(&def).copied() else {
            return Err(self.refusal(
                owner,
                span,
                def,
                format!(
                    "the {} `{}` reaches `{name}`, whose body the evaluation slice cannot plan (a \
                     `var`-bound or import-bound callee refuses here)",
                    owner.kind, owner.name
                ),
            ));
        };
        let res = &analyzed.resolutions[file];
        let mut found: Option<Diagnostic> = None;
        walk_expr(body, &mut |expr: &Expr| {
            if found.is_some() {
                return;
            }
            if let Some(diagnostic) = self.deinit_literal(expr, res, owner, Some(&name)) {
                found = Some(diagnostic);
                return;
            }
            for (use_span, member, target) in reaches(res, expr) {
                match target {
                    Target::Fn(callee) => {
                        if is_runtime_fn(callee, analyzed)
                            && let Err(diagnostic) = self.admit(owner, callee, use_span)
                        {
                            found = Some(diagnostic);
                            return;
                        }
                    },
                    Target::Member => {
                        found = Some(self.refusal(
                            owner,
                            use_span,
                            def,
                            format!(
                                "the {} `{}` reaches the member call `{member}` inside `{name}`, \
                                 and the evaluation slice plans no impl bodies this round",
                                owner.kind, owner.name
                            ),
                        ));
                        return;
                    },
                }
            }
        });
        if let Some(diagnostic) = found {
            return Err(diagnostic);
        }
        self.states.insert(def, true);
        self.admitted.push(def);
        Ok(())
    }

    /// One struct literal of a deinit-carrying struct (C5): the slice
    /// plans no deinit hooks, so the value's death would silently skip
    /// the hook inside the fence — the literal refuses here, inside the
    /// comptime fn's own body or inside the helper `via` names.
    fn deinit_literal(
        &self,
        expr: &Expr,
        res: &ds_check::Resolutions,
        owner: &Owner,
        via: Option<&str>,
    ) -> Option<Diagnostic> {
        let Expr::Struct { path, span, .. } = expr else {
            return None;
        };
        let head = path.segments.first()?;
        let def = match res.get(head.span) {
            Some(Res::Def(def)) => *def,
            _ => return None,
        };
        if !self.deinits.contains(&def) {
            return None;
        }
        let name = self.analyzed.package.def(def).name.clone();
        let inside = match via {
            Some(helper) => format!(" inside `{helper}`"),
            None => String::new(),
        };
        Some(
            Diagnostic::error(
                ErrorCode::ComptimeEvalRefusal,
                format!(
                    "the {} `{}` reaches a deinit-carrying `{name}` construction{inside}, and the \
                     evaluation slice plans no deinit hooks; deinit-carrying parts refuse inside \
                     the fence",
                    owner.kind, owner.name
                ),
            )
            .with_primary(*span, "this construction")
            .with_secondary(owner.span, owner.kind),
        )
    }

    /// One refusal over the shared code: the call's span primary, the
    /// comptime fn and the callee secondary.
    fn refusal(&self, owner: &Owner, span: Span, callee: DefId, message: String) -> Diagnostic {
        Diagnostic::error(owner.code, message)
            .with_primary(span, "this call")
            .with_secondary(owner.span, owner.kind)
            .with_secondary(self.analyzed.package.def(callee).span, "the callee")
    }

    /// The row's first refusing hop: drilling through body order, the
    /// first non-admitting use of each admitting caller names the next
    /// hop, and a definition whose own body holds no refusing use (its
    /// refusing effect is intrinsic) or a revisited definition is the
    /// hop.
    fn origin(&self, def: DefId) -> DefId {
        let analyzed = self.analyzed;
        let mut visited = vec![def];
        let mut def = def;
        loop {
            let Some((file, body)) = self.bodies.get(&def).copied() else {
                return def;
            };
            let res = &analyzed.resolutions[file];
            let mut next: Option<DefId> = None;
            walk_expr(body, &mut |expr: &Expr| {
                if next.is_some() {
                    return;
                }
                for (_, _, target) in reaches(res, expr) {
                    if let Target::Fn(callee) = target
                        && is_runtime_fn(callee, analyzed)
                        && self.effects.of(callee).is_some_and(|row| !row_admits(row))
                    {
                        next = Some(callee);
                        return;
                    }
                }
            });
            match next {
                None => return def,
                Some(callee) => {
                    if visited.contains(&callee) {
                        return def;
                    }
                    visited.push(callee);
                    def = callee;
                },
            }
        }
    }
}

/// One named reach of an expression node: a path segment's or dot-name's
/// resolution. A free fn (or a const binding of one) is a def reach; a
/// registered member call is a member reach; everything else (a local, a
/// handler, a bare name) reaches nothing.
#[derive(Clone, Copy)]
enum Target {
    /// The resolved free-fn def.
    Fn(DefId),
    /// A registered member call: trait or inherent candidates, or the
    /// qualified `Trait::f` escape hatch.
    Member,
}

/// The reaches of one expression node, in source order.
fn reaches<'n>(res: &ds_check::Resolutions, expr: &'n Expr) -> Vec<(Span, &'n str, Target)> {
    let mut out = Vec::new();
    match expr {
        Expr::Path { path, .. } => {
            for seg in &path.segments {
                match res.get(seg.span) {
                    Some(Res::Def(def)) => {
                        out.push((seg.span, seg.name.as_str(), Target::Fn(*def)));
                    },
                    Some(Res::TraitMethod(_)) => {
                        out.push((seg.span, seg.name.as_str(), Target::Member));
                    },
                    _ => {},
                }
            }
        },
        Expr::Field { name, .. } => match res.get(name.span) {
            Some(Res::Def(def)) => out.push((name.span, name.name.as_str(), Target::Fn(*def))),
            Some(Res::Dot(DotRes::Free { func, .. })) => {
                out.push((name.span, name.name.as_str(), Target::Fn(*func)));
            },
            Some(
                Res::Dot(DotRes::Methods { .. } | DotRes::Static { .. }) | Res::TraitMethod(_),
            ) => {
                out.push((name.span, name.name.as_str(), Target::Member));
            },
            _ => {},
        },
        _ => {},
    }
    out
}

/// Whether a resolved callee is the package's runtime half: a declared
/// free fn with a typed value (an extern carries none, so the pure
/// builtin core and the world surface stay the install seam's own
/// classes) that no parameter marks comptime — exactly the functions
/// the slice plans only when the admission names them. A case
/// constructor is a value construction, not a call into the runtime
/// half.
fn is_runtime_fn(def: DefId, analyzed: &Analyzed) -> bool {
    if analyzed.decls.cases.get(&def).is_some() {
        return false;
    }
    let Some(sig) = analyzed.decls.fns.get(&def) else {
        return false;
    };
    analyzed.checked.def_types.contains_key(&def)
        && !sig
            .params
            .iter()
            .any(|p| matches!(p.mode, ParamMode::Comptime(_)))
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
                    // a linked enum's minted cases answer through the
                    // foreign name index the install filled — the
                    // package namespace never carries them
                    let case_def = self
                        .analyzed
                        .package
                        .lookup_type(name)
                        .filter(|&d| self.analyzed.package.def(d).span == *span)
                        .or_else(|| self.analyzed.decls.foreign_type(name))?;
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
    /// name the argument spells (parentheses read through, a pure
    /// spelling of the same form).
    fn stage_type(&mut self, arg: &Expr) -> Result<u8, String> {
        let Expr::Path { path, .. } = unparen(arg) else {
            return Err(
                "a comptime argument names a declared type this round; anything else (a local, an \
                 application like list[int], a type-typed value) is the evaluator's residual"
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

/// The expression inside any parentheses around it: a parenthesized
/// spelling of a staged form is the same form, so both staging doors read
/// through it. Nested parens flatten; the wrapped shape keeps its own
/// staging rule.
fn unparen(mut expr: &Expr) -> &Expr {
    while let Expr::Paren { inner, .. } = expr {
        expr = inner;
    }
    expr
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
/// the VM's root heap (the host-facing staging discipline). Parentheses
/// read through: a plain literal in parens stages as the same literal.
fn stage_literal(vm: &mut Vm, kind: ScalarKind, arg: &Expr) -> Result<Value, String> {
    const SHAPE: &str = "a scalar comptime parameter takes a plain literal of its annotation; \
                         anything else (a name, an interpolated or suffixed literal) refuses here";
    let arg = unparen(arg);
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
/// already stripped by the parser) to its word `[B7]`.
fn parse_int_word(text: &str) -> Option<i64> {
    let (radix, digits) = match text.as_bytes() {
        [b'0', b'x', ..] => (16, &text[2..]),
        [b'0', b'b', ..] => (2, &text[2..]),
        [b'0', b'o', ..] => (8, &text[2..]),
        _ => (10, text),
    };
    i64::from_str_radix(digits, radix).ok()
}

/// The embeddable answer vocabulary, one clause for every embed refusal.
const EMBEDDABLE: &str = "the embeddable answers are int, float, bool, string, a list over \
                          embeddable elements, and a tuple over embeddable scalars; a comptime \
                          function answering anything else refuses here";

/// One embedded answer: the embeddable results are the scalar atoms,
/// string, a list over embeddable elements, and a tuple over embeddable
/// scalars (each staged at the call site as the literal a user would
/// have written). Every answer embeds over the call's own checked type —
/// a scalar answer whose call site checks as anything wider (a tuple, a
/// unit, a wider union) refuses here, never the lowering, and a
/// multi-word answer embeds only as a tuple whose arity and element
/// types match the call's checked tuple element for element.
fn embed(
    words: &[Value],
    call_ty: Option<Ty>,
    analyzed: &Analyzed,
    vm: &Vm,
) -> Result<ComptimeConst, String> {
    if words.len() > 1 {
        return embed_tuple(words, call_ty, analyzed, vm);
    }
    let value = words
        .first()
        .copied()
        .expect("a drive answer carries a word");
    match value {
        Value::I64(word) => {
            gate_scalar_answer(call_ty, "int", analyzed)?;
            Ok(ComptimeConst::Int(word))
        },
        Value::F64(word) => {
            gate_scalar_answer(call_ty, "float", analyzed)?;
            Ok(ComptimeConst::Float(word))
        },
        Value::Bool(word) => {
            gate_scalar_answer(call_ty, "bool", analyzed)?;
            Ok(ComptimeConst::Bool(word))
        },
        Value::Ref(word) => {
            let objects = vm.objects();
            if let Ok((elems, stride)) = objects.list_parts(word.id()) {
                return embed_list(elems, stride, call_ty, analyzed, objects);
            }
            let text = objects
                .str_at(word.id())
                .map_err(|_| "the answer is not a string".to_owned())?;
            gate_scalar_answer(call_ty, "string", analyzed)?;
            Ok(ComptimeConst::Str(text.to_owned()))
        },
        Value::Tag(_) | Value::Fn(_) | Value::Arena(_) => Err(EMBEDDABLE.to_owned()),
    }
}

/// One scalar answer's gate: the call's checked type must be exactly the
/// scalar atom the answer is, so a wider return (a tuple's head word, a
/// unit word) refuses at the embed instead of recording a truncated
/// answer the runtime lowering cannot stage.
fn gate_scalar_answer(call_ty: Option<Ty>, atom: &str, analyzed: &Analyzed) -> Result<(), String> {
    let names = |ty: Ty| -> Option<String> {
        let members = analyzed.decls.universe.members(ty);
        match members {
            [only] => analyzed
                .decls
                .universe
                .atom_decl(*only)
                .ok()
                .map(|decl| decl.name.clone()),
            _ => None,
        }
    };
    let spelled = call_ty.and_then(names);
    match spelled.as_deref() {
        Some(name) if name == atom => Ok(()),
        _ => {
            let rendered = call_ty.map(|ty| analyzed.decls.universe.render(ty));
            let where_ = rendered.map_or_else(
                || "the call's own type".to_owned(),
                |ty| format!("the call's checked type `{ty}`"),
            );
            Err(format!(
                "a scalar answer embeds only over its own scalar type at the call; {where_} is \
                 wider, so the scalar's word does not embed there — {EMBEDDABLE}"
            ))
        },
    }
}

/// One tuple element's own atom name: the elements of a solved tuple type
/// are single atoms (solving distributes products over unions
/// element-wise), so the per-element gate reads the atom's declaration
/// directly.
fn tuple_elem_name(elem: TypeId, analyzed: &Analyzed) -> Option<String> {
    analyzed
        .decls
        .universe
        .atom_decl(elem)
        .ok()
        .map(|decl| decl.name.clone())
}

/// One multi-word answer: embeddable only as a tuple over the four
/// scalar atoms, the call's own checked type a tuple of the same arity
/// with each element gated against its position's element type word for
/// word — the runtime lowering stages the tuple literal a user would
/// have written from exactly these facts.
fn embed_tuple(
    words: &[Value],
    call_ty: Option<Ty>,
    analyzed: &Analyzed,
    vm: &Vm,
) -> Result<ComptimeConst, String> {
    let rendered = call_ty.map(|ty| analyzed.decls.universe.render(ty));
    let elems = call_ty.and_then(|ty| {
        let [only] = analyzed.decls.universe.members(ty) else {
            return None;
        };
        match analyzed
            .decls
            .universe
            .atom_decl(*only)
            .map(|decl| decl.kind.clone())
        {
            Ok(ds_check::AtomKind::Tuple(elems)) => Some(elems),
            _ => None,
        }
    });
    let Some(elems) = elems else {
        let where_ = rendered.map_or_else(
            || "the call's own type".to_owned(),
            |ty| format!("the call's checked type `{ty}`"),
        );
        return Err(format!(
            "a multi-word answer embeds only as a tuple over scalars at the call; {where_} is not \
             a tuple type — {EMBEDDABLE}"
        ));
    };
    if elems.len() != words.len() {
        let where_ = rendered.map_or_else(
            || "the call's own type".to_owned(),
            |ty| format!("the call's checked type `{ty}`"),
        );
        return Err(format!(
            "a multi-word answer embeds only as a tuple of the same arity at the call; {where_} \
             takes {} element(s), the answer carries {words_len} — {EMBEDDABLE}",
            elems.len(),
            words_len = words.len(),
        ));
    }
    let mut items = Vec::with_capacity(words.len());
    for (word, elem) in words.iter().zip(&elems) {
        let name = tuple_elem_name(*elem, analyzed);
        let item = match (word, name.as_deref()) {
            (Value::I64(word), Some("int")) => ComptimeConst::Int(*word),
            (Value::F64(word), Some("float")) => ComptimeConst::Float(*word),
            (Value::Bool(word), Some("bool")) => ComptimeConst::Bool(*word),
            (Value::Ref(word), Some("string")) => ComptimeConst::Str(
                vm.objects()
                    .str_at(word.id())
                    .map_err(|_| "the answer is not a string".to_owned())?
                    .to_owned(),
            ),
            _ => {
                let where_ = rendered.map_or_else(
                    || "the call's own type".to_owned(),
                    |ty| format!("the call's checked type `{ty}`"),
                );
                return Err(format!(
                    "a tuple answer embeds one scalar per element over its own element type at \
                     the call; {where_} names an element the answer's word does not match — \
                     {EMBEDDABLE}"
                ));
            },
        };
        items.push(item);
    }
    Ok(ComptimeConst::Tuple(items))
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
        // the world-touching builtin rows the fenced table drops: the
        // call parks the calling fiber against the clock, the same
        // world refusal the unserved platform ops take
        LoadError::UnresolvedCallee {
            func,
            module,
            index,
        } if *module == ds_vm::native::BUILTIN_MODULE
            && let Some(row) = dropped_clock_row(*index) =>
        {
            Diagnostic::error(
                ErrorCode::ComptimeFenceRefusal,
                format!(
                    "this comptime function calls the world surface (`{row}`: the call parks the \
                     calling fiber against the clock); the comptime fence refuses world-touching \
                     calls as compile errors [G1]"
                ),
            )
            .with_primary(
                fns.iter()
                    .find(|f| f.name == *func)
                    .map_or(sites[0].span, |f| f.span),
                "the comptime function",
            )
            .with_secondary(sites[0].span, "evaluated at this comptime call")
        },
        LoadError::UnresolvedCallee { func, module, .. } => Diagnostic::error(
            ErrorCode::ComptimeFenceRefusal,
            format!(
                "the comptime function `{func}` calls `{module}`, which the comptime slice does \
                 not carry; a comptime function may call the pure builtins, other comptime \
                 functions, and the helpers the fence admits this round (a pure or trap-only row)"
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
        TypeExpr::Empty => "empty".to_owned(),
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
        | Expr::Continue(_)
        // a conversion placeholder holds nothing to visit
        | Expr::Invalid(_) => {},
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
        Expr::FieldKey { recv, name, .. } => {
            walk_expr(recv, go);
            walk_expr(name, go);
        },
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

#[cfg(test)]
mod tests {
    use ds_check::{Effect, EffectRow};

    use super::row_admits;

    /// The fence's row predicate (the 2026-11-17 amendment): a pure row
    /// and a row of `trap` alone admit — the over-approximation the
    /// effects pass rows onto unsolved-type trapping operators (string
    /// concat included) fences nothing — while every other row refuses:
    /// each effect alone, and each beside `trap` (a world row keeps its
    /// refusal whatever `trap` rides with it).
    #[test]
    fn the_fence_admits_pure_and_trap_only_rows() {
        assert!(row_admits(EffectRow::PURE));
        assert!(row_admits(EffectRow::of(Effect::Trap)));
        assert!(!row_admits(EffectRow::of(Effect::World)));
        assert!(!row_admits(EffectRow::of(Effect::Error)));
        assert!(!row_admits(EffectRow::of(Effect::Cancel)));
        assert!(!row_admits(EffectRow::of(Effect::Discontinue)));
        assert!(!row_admits(
            EffectRow::of(Effect::Trap) | EffectRow::of(Effect::World)
        ));
        assert!(!row_admits(
            EffectRow::of(Effect::Trap) | EffectRow::of(Effect::Error)
        ));
        assert!(!row_admits(
            EffectRow::of(Effect::World) | EffectRow::of(Effect::Cancel)
        ));
    }
}
