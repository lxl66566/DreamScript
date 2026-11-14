//! # ds-lower: checked programs to executable dsmod modules [M3]
//!
//! The lowering pass: one checked package (parse -> `ds_check::analyze`)
//! becomes one ds-mod module — frame layouts per the locked BL-6 rules,
//! uop bodies per the BL-5 contract — that the tier-1 VM runs through the
//! ds-mod writer/loader path.
//!
//! # Architecture
//!
//! Three stages per package:
//!
//! 1. **Planning** collects the lowerable top-level functions: const-arrow lambdas — monomorphic or
//!    free-generic (plus the synthesized functions lambda values register mid-lowering, F2). A free
//!    generic fn lowers its ONE body once [H3]: call sites stage the instantiated evidence words
//!    its body's runtime demands read, in the outgoing-argument area after the true arguments.
//!    Comptime items stay exempt, and generic lambdas as values and comptime parameters refuse at
//!    their own doors. A `main` entry must exist.
//! 2. **Shapes** map every solved type onto its slot representation ([`shape` module docs]):
//!    scalars are one word, bare structs are field spans, unions are tag+payload spans with
//!    per-member payload offsets. Tag ids are module-wide, assigned in first-use order.
//! 3. **Per-function lowering** ([`func`]) walks each body once, reading the checker's recorded
//!    tables: per-expression solved types (`expr_types`, flow-refined), per-binder declared types
//!    (`binders`), and per-use name resolutions. Slots bump-allocate (params first, then
//!    locals/temps); the outgoing-argument area sizes itself as the max over call sites of staged
//!    words and result spans, per BL-6, and any overlay word that carries more than one word type
//!    over the frame's lifetime is `SlotType::Raw`.
//!
//! Early exits lower exactly as the golden ds-vm fixtures do: `?` and the
//! extraction handlers emit TAG_TEST + JMPIF, then either construct the
//! alternative or re-span the propagated member into the enclosing return
//! union with RETSPAN. Interpolation lowers to `str_*` builtin calls
//! chained with STRCAT. Tail positions lower to CALLT (the stack never
//! grows [H2]) and final expressions to RET/RETSPAN.
//!
//! # The Perceus placement pass (C4, rides the walk)
//!
//! The walk in [`func`] is also the forward pass of the dup/drop placement
//! ([`place`]): every Ref-typed word owns a reference count, and both the
//! placed sequence returned per function ([`Lowered::placed`]) and the
//! module's executable code interleave `RcOp::Inc`/`Dec` with the runnable
//! uops — the 0xE words ride beside the uops, and the tier-1 VM executes
//! them over its counted heap blocks.
//!
//! # ds-check seam (additive, consumed here)
//!
//! The lowerer does not re-derive types: `ds_check::analyze` returns the
//! declaration tables, the resolutions, and a `Checked` whose `Ty` handles
//! were minted by those tables' universe, plus the per-expression type
//! side table this pass requested. Passing the bundle around as one unit
//! is load-bearing: a second pass would mint an unrelated universe.

mod error;
mod func;
mod native;
mod place;
mod shape;

use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
};

use ds_abi::{RcOp, Uop};
use ds_check::{Analyzed, Condition, TraitId, render_type};
use ds_diag::Span;
use ds_mod::{EntryPlan, Hash128, Module, TargetClass};
use ds_parse::{Ast, Expr, Ident, ImplDecl, Item, ItemKind, Param, Pattern, StructMember};
pub use error::LowerError;
use func::{BodyId, EvidenceEnv, FnLower, Plan};
use shape::Shapes;

/// One op of a function body: a runnable micro-operation or a placed
/// reference-counting op (C4). The module's executable code carries the
/// same sequence, so this type is the placement contract the VM stream
/// agrees with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlacedOp {
    /// A runnable micro-operation.
    Uop(Uop),
    /// A placed reference-counting op.
    Rc(RcOp),
}

/// The lowered module plus the tables callers need: the function table's
/// names in index order, the module-wide tag ids, and each function's
/// placed op sequence.
pub struct Lowered {
    module: Module,
    functions: Vec<String>,
    tags: HashMap<String, u8>,
    placed: Vec<Vec<PlacedOp>>,
}

impl Lowered {
    /// The finished ds-mod module.
    #[must_use]
    pub fn module(&self) -> &Module {
        &self.module
    }

    /// The finished module, consumed: the owning install path takes the
    /// function bodies over without copying them. The name, tag, and
    /// placement tables drop with this record, so callers read what they
    /// still need first.
    #[must_use]
    pub fn into_module(self) -> Module {
        self.module
    }

    /// The function-table names, in index order (FuncRef indexes).
    #[must_use]
    pub fn functions(&self) -> &[String] {
        &self.functions
    }

    /// The function-table index of a lowered function name.
    #[must_use]
    pub fn function_index(&self, name: &str) -> Option<u32> {
        self.functions
            .iter()
            .position(|f| f == name)
            .map(|index| u32::try_from(index).expect("function index fits u32"))
    }

    /// The module-wide tag id of a declared atom name, as the run entry
    /// path needs to build `Value::Tag` arguments.
    #[must_use]
    pub fn tag(&self, name: &str) -> Option<u8> {
        self.tags.get(name).copied()
    }

    /// Records the run surface's entry staging facts into the module, so
    /// the artifact carries what a warm run needs to stage arguments
    /// without the front end: one row per top-level lambda (a candidate
    /// entry name) with one plan per parameter. Lowering owns the tag
    /// table (recorded at construction); the caller owns which names are
    /// entries and how their parameters stage.
    #[must_use]
    pub fn with_entry_plans(mut self, entries: Vec<EntryPlan>) -> Self {
        self.module = entries.into_iter().fold(self.module, |module, plan| {
            module.with_entry(plan.name, plan.args)
        });
        self
    }

    /// One function's placed op sequence: the runnable uops plus the
    /// placed RC ops (dup/drop) at their exact positions — the artifact
    /// the M4 placement contract pins and the VM seam will splice in. A
    /// body served from the checked-in artifact answers an empty stream:
    /// its code words already ride the module verbatim.
    #[must_use]
    pub fn placed(&self, name: &str) -> Option<&[PlacedOp]> {
        self.function_index(name)
            .and_then(|index| self.placed.get(index as usize).map(Vec::as_slice))
    }
}

/// One embedded comptime answer, as the runtime module materializes it
/// at the call site (G1): the evaluator's recorded constant. The
/// round-one embeddable results are the scalar atoms and string — a
/// comptime function answering anything else refuses at the evaluator,
/// never here.
#[derive(Clone, Debug)]
pub enum ComptimeConst {
    /// A string answer.
    Str(String),
    /// An int answer.
    Int(i64),
    /// A float answer.
    Float(f64),
    /// A bool answer.
    Bool(bool),
}

/// The evaluator's recorded answers, keyed by each comptime call's span:
/// the runtime lowering embeds exactly these and refuses a comptime call
/// whose span is missing (the evaluator's contract).
#[derive(Clone, Debug, Default)]
pub struct ComptimeValues {
    answers: HashMap<Span, ComptimeConst>,
}

impl ComptimeValues {
    /// Records one answer.
    pub fn insert(&mut self, span: Span, constant: ComptimeConst) {
        self.answers.insert(span, constant);
    }

    /// One call span's answer.
    #[must_use]
    pub fn get(&self, span: Span) -> Option<&ComptimeConst> {
        self.answers.get(&span)
    }

    /// Whether any answer was recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.answers.is_empty()
    }
}

/// The shared lowering core behind the entries: `main_required` waives
/// the run contract's entry check for the ambient's declaration-only
/// generation and the comptime slice, `served` splices pre-lowered
/// function bodies over the planned prefix instead of walking them, and
/// `comptime` selects the evaluator's slice or embeds its recorded
/// answers at the call sites.
fn lower_module(
    module_name: &str,
    source_hash: Hash128,
    asts: &[Ast],
    analyzed: &Analyzed,
    main_required: bool,
    served: Option<Vec<ds_mod::Function>>,
    comptime: func::ComptimeRun<'_>,
) -> Result<Lowered, LowerError> {
    let slice = matches!(comptime, func::ComptimeRun::Slice);
    if !analyzed.checked.diags.is_empty() {
        return Err(LowerError::NotChecked {
            count: analyzed.checked.diags.len(),
        });
    }
    let mut plan = Plan::new();
    plan.set_demands(collect_demands(asts, analyzed));
    if !slice {
        plan_deinits(asts, &mut plan, analyzed);
    }
    for ast in asts {
        for block in &ast.blocks {
            for item in &block.items {
                refuse_derive(item)?;
                plan_item(item, &mut plan, analyzed, slice);
            }
        }
    }
    if main_required && !slice && !plan.has_main() {
        return Err(LowerError::NoMain);
    }
    if let Some(served) = &served {
        let names = plan.names();
        let detail = if names.len() < served.len() {
            Some(format!(
                "the plan holds {} functions, the serve holds {}",
                names.len(),
                served.len()
            ))
        } else {
            names
                .iter()
                .zip(served.iter())
                .position(|(planned, body)| planned != body.name())
                .map(|at| {
                    format!(
                        "planned function {at} is `{}` but the served body names `{}`",
                        names[at],
                        served[at].name()
                    )
                })
        };
        if let Some(detail) = detail {
            return Err(LowerError::ServedDrift { detail });
        }
    }
    // the affine facts the placement pass consumes: last uses and moves
    // come from the checker (C1), never re-derived here
    let lin = ds_check::linearity_check(
        asts,
        &analyzed.package,
        &analyzed.resolutions,
        &analyzed.decls,
        &analyzed.checked,
    );
    let (functions, placed, shapes) =
        lower_all(&mut plan, analyzed, &lin, &platform_name(asts), served, comptime)?;

    let mut module = Module::new(module_name, TargetClass::Bytecode, source_hash);
    for function in functions {
        module = module.with_function(function);
    }
    for chain in plan.take_chains() {
        module = module.with_chain(chain);
    }
    let tags = shapes.tag_names();
    // the tag table rides the artifact in first-use order: ids are dense
    // from zero, so sorting by id reconstructs the assignment order
    let mut rows: Vec<(&str, u8)> = tags.iter().map(|(name, id)| (name.as_str(), *id)).collect();
    rows.sort_unstable_by_key(|&(_, id)| id);
    for (name, id) in rows {
        module = module.with_tag(name, id);
    }
    Ok(Lowered {
        module,
        functions: plan.names(),
        tags,
        placed,
    })
}

/// Lowers one checked package to a ds-mod module named `module_name`.
///
/// # Errors
///
/// [`LowerError::NotChecked`] when the package still carries checker
/// diagnostics; [`LowerError::Unsupported`] for constructs outside the M3
/// runtime contract (each error names the seam it needs); [`LowerError::NoMain`]
/// when no lowerable `main` entry exists.
pub fn lower(
    module_name: &str,
    source_hash: Hash128,
    asts: &[Ast],
    analyzed: &Analyzed,
) -> Result<Lowered, LowerError> {
    lower_with_comptime(
        module_name,
        source_hash,
        asts,
        analyzed,
        &ComptimeValues::default(),
    )
}

/// Lowers the ambient's declaration package for the checked-in dsmod
/// artifact: the run contract's `main` requirement is waived — a
/// declaration-only member has no entry — and everything else runs
/// exactly as [`lower`]. The artifact's consumer splices the returned
/// functions in front of a user package's own table (see
/// [`lower_with_served`]), so the ambient's items must keep planning
/// first in the combined package for the indexes to line up.
///
/// # Errors
///
/// [`LowerError::NotChecked`] and [`LowerError::Unsupported`] as
/// [`lower`]; never [`LowerError::NoMain`].
pub fn lower_ambient(
    module_name: &str,
    source_hash: Hash128,
    asts: &[Ast],
    analyzed: &Analyzed,
) -> Result<Lowered, LowerError> {
    let empty = ComptimeValues::default();
    lower_module(
        module_name,
        source_hash,
        asts,
        analyzed,
        false,
        None,
        func::ComptimeRun::Runtime(&empty),
    )
}

/// The runtime-module lowering with the comptime evaluator's answers
/// embedded: comptime functions plan nothing (their call sites emit the
/// recorded constants), and every other surface lowers exactly like
/// [`lower`].
///
/// # Errors
///
/// As [`lower`], plus [`LowerError::Unsupported`] for a comptime call
/// whose span the evaluator never recorded.
pub fn lower_with_comptime(
    module_name: &str,
    source_hash: Hash128,
    asts: &[Ast],
    analyzed: &Analyzed,
    values: &ComptimeValues,
) -> Result<Lowered, LowerError> {
    lower_module(
        module_name,
        source_hash,
        asts,
        analyzed,
        true,
        None,
        func::ComptimeRun::Runtime(values),
    )
}

/// The comptime slice the evaluator compiles for its fenced run: exactly
/// the package's comptime functions, lowered as ordinary locals — a
/// comptime function calling another is a plain intra-module call there,
/// and calls into runtime functions refuse (the slice's table never
/// holds them). No `main` is required and no deinit hooks plan (binding
/// a deinit-carrying struct inside a comptime function refuses at the
/// hook's claim).
///
/// # Errors
///
/// [`LowerError::NotChecked`] when the package still carries checker
/// diagnostics; [`LowerError::Unsupported`] for comptime bodies outside
/// the covered runtime contract.
pub fn lower_comptime_slice(
    module_name: &str,
    source_hash: Hash128,
    asts: &[Ast],
    analyzed: &Analyzed,
) -> Result<Lowered, LowerError> {
    lower_module(
        module_name,
        source_hash,
        asts,
        analyzed,
        false,
        None,
        func::ComptimeRun::Slice,
    )
}

/// Lowers one checked package with the ambient's function bodies served
/// from a checked-in artifact instead of lowered here. The served bodies
/// must be exactly what lowering the ambient member alone produces —
/// [`lower_ambient`] over the same trees — and planning here must
/// reproduce their function-table prefix: the served list installs at
/// indexes `0..n`, every body-walking seam (dispatch staging, call
/// FuncRefs, evidence demands) reads the planned entries as ever, and
/// only the per-body lowering walk is skipped. The ambient member must
/// sit first in `asts` (the run path's registration order); a served
/// list whose length or names disagree with the plan is
/// [`LowerError::ServedDrift`] — the caller falls back to [`lower`].
///
/// # Errors
///
/// As [`lower`], plus [`LowerError::ServedDrift`] when the served
/// prefix disagrees with the plan.
pub fn lower_with_served(
    module_name: &str,
    source_hash: Hash128,
    asts: &[Ast],
    analyzed: &Analyzed,
    served: Vec<ds_mod::Function>,
    values: &ComptimeValues,
) -> Result<Lowered, LowerError> {
    lower_module(
        module_name,
        source_hash,
        asts,
        analyzed,
        true,
        Some(served),
        func::ComptimeRun::Runtime(values),
    )
}

/// Plans every struct's declared `deinit` member as a synthesized module
/// function before any body lowers (C5): binding a value of a
/// deinit-carrying struct claims the hook by naming this plan's function,
/// so the table index must exist before the first binder mints its pool
/// constant. The synthesized function is one borrowed `self` parameter —
/// the dying instance — whose body is the declared block, named after
/// the struct. A generic struct's deinit registers without a plan:
/// binding its values refuses with the CALL_GENERIC seam instead of
/// silently skipping the hook.
fn plan_deinits(asts: &[Ast], plan: &mut Plan, analyzed: &Analyzed) {
    for ast in asts {
        for block in &ast.blocks {
            for item in &block.items {
                let ItemKind::Struct(s) = &item.kind else {
                    continue;
                };
                let Some((span, body)) = s.members.iter().find_map(|member| match member {
                    StructMember::Deinit { span, body } => Some((*span, body.clone())),
                    StructMember::Field(_) => None,
                }) else {
                    continue;
                };
                let Some(def) = analyzed.package.lookup_type(&s.name.name) else {
                    continue;
                };
                if !s.generics.is_empty() {
                    plan.declare_deinit(def);
                    continue;
                }
                let lambda = Expr::Lambda {
                    span,
                    generics: Vec::new(),
                    params: vec![Param {
                        span,
                        mode: ds_parse::ParamMode::Borrow,
                        name: Ident {
                            span,
                            name: "self".to_owned(),
                        },
                        ty: None,
                        default: None,
                    }],
                    ret: None,
                    arrow: true,
                    body: Box::new(Expr::Block(body)),
                };
                let body_span_value = body_span(&lambda);
                plan.push_deinit(def, &s.name.name, lambda, body_span_value);
            }
        }
    }
}

/// The deinit function's recorded body-block span: the checker types the
/// declared block in place, so its span keys the unit return the
/// synthesized function reads.
fn body_span(lambda: &Expr) -> Span {
    let Expr::Lambda { body, .. } = lambda else {
        unreachable!("the synthesized deinit is a lambda");
    };
    let Expr::Block(block) = &**body else {
        unreachable!("the synthesized deinit body is a block");
    };
    block.span
}

/// A `#[derive(...)]` attribute refuses at lowering: the checker's
/// derive expansion consumes the attribute there (G1), so an attribute
/// reaching this pass means the trees were not the analyzer's own —
/// `lower` must run over `Analyzed::asts`, where consumed attributes
/// are stripped and their impls are ordinary items.
fn refuse_derive(item: &Item) -> Result<(), LowerError> {
    for attr in &item.attrs {
        if attr.name.name == "derive" {
            return Err(LowerError::unsupported(
                attr.span,
                "a derive attribute",
                "the checker's derive expansion owns it; lower over the analyzer's expanded trees",
            ));
        }
    }
    Ok(())
}

/// Collects one top-level item into the plan when it is a lowerable
/// const-arrow function: a monomorphic one lowers its own body, a free
/// generic one its single ONE body with the runtime-evidence demands its
/// bounds collect [H3]. Everything else is skipped; comptime functions
/// are the evaluator's slice — the runtime module plans none of them and
/// the slice plans nothing else.
fn plan_item(item: &Item, plan: &mut Plan, analyzed: &Analyzed, slice: bool) {
    if let ItemKind::Impl(imp) = &item.kind {
        if !slice {
            plan_impl(item, imp, plan, analyzed);
        }
        return;
    }
    if let ItemKind::Trait(t) = &item.kind {
        if !slice {
            plan_trait_defaults(t, plan, analyzed);
        }
        return;
    }
    let ItemKind::Local(local) = &item.kind else {
        return;
    };
    if local.mutable {
        return;
    }
    let Pattern::Binding { name, .. } = &local.pat else {
        return;
    };
    let Expr::Lambda {
        generics, params, ..
    } = &local.init
    else {
        return; // a non-function const: no module function for it
    };
    let Some(def) = analyzed.package.lookup(&name.name) else {
        return;
    };
    if !analyzed.checked.def_types.contains_key(&def) {
        return; // the checker has no typed value for it (extern or later stage)
    }
    let comptime_fn = params
        .iter()
        .any(|p| matches!(p.mode, ds_parse::ParamMode::Comptime(_)));
    if comptime_fn != slice {
        return; // the runtime module and the comptime slice split here
    }
    if !generics.is_empty() {
        // The ONE generic body [H3]: a free generic fn lowers once,
        // never per instantiation; its declared bounds collect the
        // runtime-evidence demands its body's nested bound calls read,
        // and the entering call sites stage the words. A bare `[t]`
        // parameter bounds nothing, so its bodies demand nothing.
        let bounds = Rc::new(fn_bounds(def, analyzed));
        let pairs = plan.demand(&BodyId::Free(def));
        plan.push_free_generic(name.name.clone(), def, local, EvidenceEnv { pairs, bounds });
        return;
    }
    plan.push(name.name.clone(), def, local);
}

/// The declared bounds of one free generic fn's type parameters: the
/// written name of each `[t: trait]` clause to the traits it names,
/// resolved through the package's type namespace. Keyed by the written
/// name (not a minted atom) because the declaration and each call
/// site's checking mint their own variables; a bound naming no trait
/// (an affine marker, a non-single-segment path) bounds nothing.
fn fn_bounds(def: ds_check::DefId, analyzed: &Analyzed) -> HashMap<String, HashSet<TraitId>> {
    let mut out: HashMap<String, HashSet<TraitId>> = HashMap::new();
    let Some(sig) = analyzed.decls.fns.get(&def) else {
        return out;
    };
    for param in &sig.generics {
        let Some(ds_parse::Type::Path { path, .. }) = &param.bound else {
            continue;
        };
        let [only] = path.segments.as_slice() else {
            continue;
        };
        let Some(trait_def) = analyzed.package.lookup_type(&only.name) else {
            continue;
        };
        let Some(&trait_id) = analyzed.decls.traits.get(&trait_def) else {
            continue;
        };
        out.entry(param.name.clone()).or_default().insert(trait_id);
    }
    out
}

/// Collects one trait impl's member bodies into the plan (the trait
/// dispatch seam): each member fn enters the function table under its
/// mangled identity `<trait>#<target>#<member>` with the checker-solved
/// return its FnDecl names. A generic impl's bodies lower once per impl
/// with one runtime-evidence slot per nested bound demand, staged by the
/// entering call sites from the checker-recorded bounds. Inherent impls
/// plan through their own mangle below.
fn plan_impl(item: &Item, imp: &ImplDecl, plan: &mut Plan, analyzed: &Analyzed) {
    if imp.trait_ref.is_none() {
        plan_inherent_impl(item, imp, plan, analyzed);
        return;
    }
    let trait_ref = imp
        .trait_ref
        .as_ref()
        .expect("the trait arm sees the trait ref it checked for");
    let [only] = trait_ref.path.segments.as_slice() else {
        return;
    };
    let bounds = Rc::new(impl_bounds(item.span, analyzed));
    for f in &imp.fns {
        let Some(body) = &f.body else {
            continue;
        };
        let Some(ret) = analyzed.decls.impl_fn_rets.get(&f.span).copied() else {
            continue;
        };
        let body_expr = Rc::new(Expr::Block(body.clone()));
        let pairs = plan.demand(&BodyId::Member {
            impl_site: item.span,
            member: f.name.name.clone(),
        });
        let evidence = EvidenceEnv {
            pairs,
            bounds: bounds.clone(),
        };
        let name = format!("{}#{}#{}", only.name, render_type(&imp.target), f.name.name);
        plan.push_declared(
            name,
            item.span,
            f.name.name.clone(),
            Rc::new(f.params.clone()),
            body_expr,
            ret,
            evidence,
        );
    }
}

/// Collects one inherent impl's member bodies into the plan: each member
/// fn enters the function table under the mangled identity
/// `<target>#<member>` — one `#`-joined pair against the trait impls'
/// three-part mangle, and `#` sits outside the identifier charset, so
/// neither a user-declared name nor a trait-impl body collides. A
/// monomorphic impl's body is a plain local function its call sites
/// reach through the checker-recorded method id. A generic impl's bodies
/// lower once per impl with the runtime-evidence machinery the trait form
/// uses (one evidence slot per nested bound demand); the entering call
/// sites stage the words through the checker's call-site bounds record —
/// no instance word exists for an inherent impl.
fn plan_inherent_impl(item: &Item, imp: &ImplDecl, plan: &mut Plan, analyzed: &Analyzed) {
    let bounds = Rc::new(impl_bounds(item.span, analyzed));
    for f in &imp.fns {
        let Some(body) = &f.body else {
            continue;
        };
        let Some(ret) = analyzed.decls.impl_fn_rets.get(&f.span).copied() else {
            continue;
        };
        let body_expr = Rc::new(Expr::Block(body.clone()));
        let pairs = plan.demand(&BodyId::Member {
            impl_site: item.span,
            member: f.name.name.clone(),
        });
        let name = format!("{}#{}", render_type(&imp.target), f.name.name);
        plan.push_declared(
            name,
            item.span,
            f.name.name.clone(),
            Rc::new(f.params.clone()),
            body_expr,
            ret,
            EvidenceEnv {
                pairs,
                bounds: bounds.clone(),
            },
        );
    }
}

/// Collects one trait's declared default bodies into the plan (the
/// dispatch seam's omitted-member half): each member fn carrying a
/// body lowers once under its mangled identity `<trait>#<member>`, the
/// dispatch target a call site reaches when its solving impl spells no
/// member — a defaulted member is omittable exactly because this body
/// exists. The body is the ONE generic shape over the trait's own
/// variables: `self` types the `Self` variable (one Raw word, the
/// runtime kind the caller's fact), and the body's nested calls on
/// `self` ride the runtime-evidence machinery — the checker satisfies
/// them through the trait-scoped `Self` assumption, so the entering
/// dispatch sites stage the serving member bodies as the body's
/// evidence demands, exactly like a free generic fn's entering sites.
fn plan_trait_defaults(t: &ds_parse::TraitDecl, plan: &mut Plan, analyzed: &Analyzed) {
    let Some(def) = analyzed.package.lookup_type(&t.name.name) else {
        return;
    };
    let Some(&trait_id) = analyzed.decls.traits.get(&def) else {
        return;
    };
    let bounds = Rc::new(trait_bounds(t, trait_id, analyzed));
    for member in &t.members {
        let ds_parse::TraitMember::Fn(f) = member else {
            continue;
        };
        let Some(body) = &f.body else {
            continue;
        };
        let Some(ret) = analyzed.decls.impl_fn_rets.get(&f.span).copied() else {
            continue;
        };
        let body_expr = Rc::new(Expr::Block(body.clone()));
        let pairs = plan.demand(&BodyId::Default {
            trait_def: def,
            member: f.name.name.clone(),
        });
        let name = format!("{}#{}", t.name.name, f.name.name);
        plan.push_default_body(
            name,
            def,
            f.name.name.clone(),
            Rc::new(f.params.clone()),
            body_expr,
            Some(ret),
            EvidenceEnv {
                pairs,
                bounds: bounds.clone(),
            },
        );
    }
}

/// The runtime-evidence bounds a trait's default bodies demand under:
/// `Self` satisfies the trait itself (the scoped assumption the
/// checker activates inside the member bodies, so default bodies call
/// sibling members on `self` with no instance), plus the trait's own
/// declared generic bounds.
fn trait_bounds(
    t: &ds_parse::TraitDecl,
    trait_id: TraitId,
    analyzed: &Analyzed,
) -> HashMap<String, HashSet<TraitId>> {
    let mut out: HashMap<String, HashSet<TraitId>> = HashMap::new();
    out.entry("Self".to_owned()).or_default().insert(trait_id);
    for param in &t.generics {
        let Some(ds_parse::Type::Path { path, .. }) = &param.bound else {
            continue;
        };
        let [only] = path.segments.as_slice() else {
            continue;
        };
        let Some(trait_def) = analyzed.package.lookup_type(&only.name) else {
            continue;
        };
        let Some(&bound) = analyzed.decls.traits.get(&trait_def) else {
            continue;
        };
        out.entry(param.name.name.clone())
            .or_default()
            .insert(bound);
    }
    out
}

/// Every generic body's runtime-evidence demands, propagated to a
/// fixpoint before planning: each body's walk collects its own
/// dot-calls plus, at every call entering another generic body (a free
/// generic fn or a generic inherent member), the callee's own demands
/// whose per-call record answers in the runtime form — read through the
/// table and re-keyed onto the calling frame's variables. A callee's
/// demands may themselves carry callee demands, so the pass iterates
/// until no body's list grows; the final table feeds the planning
/// pushes here and the lambda/spawn walks at lowering time.
fn collect_demands(asts: &[Ast], analyzed: &Analyzed) -> func::Demands {
    struct Body<'a> {
        id: BodyId,
        bounds: Rc<HashMap<String, HashSet<TraitId>>>,
        expr: Rc<Expr>,
        res: &'a ds_check::Resolutions,
    }
    let mut bodies: Vec<Body> = Vec::new();
    for ast in asts {
        for block in &ast.blocks {
            for item in &block.items {
                match &item.kind {
                    ItemKind::Local(local) => {
                        let Pattern::Binding { name, .. } = &local.pat else {
                            continue;
                        };
                        let Expr::Lambda {
                            generics,
                            span,
                            body,
                            ..
                        } = &local.init
                        else {
                            continue;
                        };
                        if generics.is_empty() || local.mutable {
                            continue;
                        }
                        let Some(def) = analyzed.package.lookup(&name.name) else {
                            continue;
                        };
                        if !analyzed.checked.def_types.contains_key(&def) {
                            continue;
                        }
                        let bounds = Rc::new(fn_bounds(def, analyzed));
                        if bounds.is_empty() {
                            continue;
                        }
                        bodies.push(Body {
                            id: BodyId::Free(def),
                            bounds,
                            expr: Rc::new((**body).clone()),
                            res: &analyzed.resolutions[span.file.as_u32() as usize],
                        });
                    },
                    ItemKind::Impl(imp) => {
                        if imp.generics.is_empty() {
                            continue;
                        }
                        let bounds = Rc::new(impl_bounds(item.span, analyzed));
                        if bounds.is_empty() {
                            continue;
                        }
                        for f in &imp.fns {
                            let Some(body) = &f.body else {
                                continue;
                            };
                            if !analyzed.decls.impl_fn_rets.contains_key(&f.span) {
                                continue;
                            }
                            bodies.push(Body {
                                id: BodyId::Member {
                                    impl_site: item.span,
                                    member: f.name.name.clone(),
                                },
                                bounds: bounds.clone(),
                                expr: Rc::new(Expr::Block(body.clone())),
                                res: &analyzed.resolutions[f.span.file.as_u32() as usize],
                            });
                        }
                    },
                    ItemKind::Trait(t) => {
                        let Some(def) = analyzed.package.lookup_type(&t.name.name) else {
                            continue;
                        };
                        let Some(&trait_id) = analyzed.decls.traits.get(&def) else {
                            continue;
                        };
                        let bounds = Rc::new(trait_bounds(t, trait_id, analyzed));
                        for member in &t.members {
                            let ds_parse::TraitMember::Fn(f) = member else {
                                continue;
                            };
                            let Some(body) = &f.body else {
                                continue;
                            };
                            if !analyzed.decls.impl_fn_rets.contains_key(&f.span) {
                                continue;
                            }
                            bodies.push(Body {
                                id: BodyId::Default {
                                    trait_def: def,
                                    member: f.name.name.clone(),
                                },
                                bounds: bounds.clone(),
                                expr: Rc::new(Expr::Block(body.clone())),
                                res: &analyzed.resolutions[f.span.file.as_u32() as usize],
                            });
                        }
                    },
                    _ => {},
                }
            }
        }
    }
    // the direct pass over an empty table, then iterate: each round
    // reads every callee's complete list from the previous one, and the
    // collected sets only grow, so the pass reaches the fixpoint
    let mut table = func::Demands::default();
    for body in &bodies {
        let pairs = Rc::new(func::runtime_pairs(
            analyzed,
            body.res,
            &body.bounds,
            &body.expr,
            &table,
        ));
        table.insert(body.id.clone(), pairs);
    }
    loop {
        let current = table.clone();
        let mut changed = false;
        for body in &bodies {
            let pairs = Rc::new(func::runtime_pairs(
                analyzed,
                body.res,
                &body.bounds,
                &body.expr,
                &current,
            ));
            changed |= current.get(&body.id) != Some(pairs.as_slice());
            table.insert(body.id.clone(), pairs);
        }
        if !changed {
            return table;
        }
    }
}

/// The declared bounds of one generic impl's type variables: parameter
/// name to the traits its `[t: trait]` clauses name. The trait form reads
/// its declared instances' conditions; the inherent form reads the
/// checker's declared-bounds record (no instance exists). The name (not
/// the minted atom) is the key because the declaration and the body's
/// checking mint their own variables; a bare `[t]` parameter bounds
/// nothing, so calls on it never check clean and no demand collects for
/// it.
fn impl_bounds(impl_site: Span, analyzed: &Analyzed) -> HashMap<String, HashSet<TraitId>> {
    let mut out: HashMap<String, HashSet<TraitId>> = HashMap::new();
    for (instance, site) in &analyzed.decls.instance_impls {
        if *site != impl_site {
            continue;
        }
        let Ok(conditions) = analyzed.decls.instances.conditions(*instance) else {
            continue;
        };
        collect_bounds(&analyzed.decls.universe, conditions, &mut out);
    }
    if let Some(record) = analyzed.decls.inherent_impl_bounds.get(&impl_site) {
        collect_bounds(&analyzed.decls.universe, &record.conditions, &mut out);
    }
    out
}

/// One conditions list into the parameter-name bounds map: each
/// condition's single member is the declaration's minted variable atom,
/// bridged to the written parameter name.
fn collect_bounds(
    universe: &ds_check::Universe,
    conditions: &[Condition<ds_check::Ty>],
    out: &mut HashMap<String, HashSet<TraitId>>,
) {
    for cond in conditions {
        let [var] = universe.members(cond.ty) else {
            continue;
        };
        let Some(param) = shape::var_param_name(universe, *var) else {
            continue;
        };
        out.entry(param).or_default().insert(cond.trait_id);
    }
}

/// What lowering every planned function yields: the module's functions,
/// each function's placed op sequence, and the shared shape tables.
type LowerAll<'b> = (Vec<ds_mod::Function>, Vec<Vec<PlacedOp>>, Shapes<'b>);

/// The platform name the world natives' pool FuncRefs address (H5, E1):
/// the package's `platform "..."` declaration's name, the default
/// platform `std` when no declaration names one — the host must install
/// a platform under this name or the module's install refuses the world
/// call sites. The comptime evaluator serves the same name with an
/// empty platform, which is the fence.
#[must_use]
pub fn platform_name(asts: &[Ast]) -> String {
    for ast in asts {
        for block in &ast.blocks {
            for item in &block.items {
                if let ItemKind::Platform(decl) = &item.kind {
                    return decl.name.name.clone();
                }
            }
        }
    }
    native::DEFAULT_PLATFORM_NAME.to_owned()
}

fn lower_all<'b>(
    plan: &mut Plan,
    analyzed: &'b Analyzed,
    lin: &ds_check::Linearity,
    platform: &str,
    served: Option<Vec<ds_mod::Function>>,
    comptime: func::ComptimeRun<'b>,
) -> Result<LowerAll<'b>, LowerError> {
    let mut shapes = Shapes::new(analyzed, plan.declared_deinits());
    let mut functions = Vec::with_capacity(plan.functions().len());
    let mut placed = Vec::with_capacity(plan.functions().len());
    // The served prefix installs verbatim (names already validated
    // against the plan): its bodies carry their own placed streams as
    // code words, so their placed rows here stay empty and the walk
    // starts at the splice's end.
    if let Some(served) = served {
        for body in served {
            functions.push(body);
            placed.push(Vec::new());
        }
    }
    let mut index = functions.len();
    while let Some(planned) = plan.function(index).cloned() {
        let res = &analyzed.resolutions[planned.lambda.span().file.as_u32() as usize];
        // Any sentinel-spanned refusal that reached here without a
        // node-observing re-point lands on the function being lowered:
        // the sentinel renders against the first file's first line,
        // which is the stdlib ambient in the CLI pipeline.
        let (function, ops) = FnLower::lower(
            analyzed,
            res,
            plan,
            &mut shapes,
            lin,
            &planned,
            u32::try_from(index).expect("function index fits u32"),
            func::RunContext {
                platform: platform.to_owned(),
                comptime,
            },
        )
        .map_err(|err| err.respanned(planned.lambda.span()))?;
        functions.push(function);
        placed.push(ops);
        index += 1;
    }
    Ok((functions, placed, shapes))
}
