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
//! 1. **Planning** collects the lowerable top-level functions: monomorphic const-arrow lambdas only
//!    (plus the synthesized functions lambda values register mid-lowering, F2). Generic functions
//!    and comptime items are exempt — nothing is emitted for their bodies, and a call into one
//!    outside the covered generic builtins is a [`LowerError::Unsupported`] naming the M6
//!    `CALL_GENERIC` seam (the ambient `len` routes through CALLG, carrying its instantiated type
//!    as the evidence word the one builtin body dispatches on). A `main` entry must exist.
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
use ds_check::{Analyzed, TraitId, render_type};
use ds_diag::Span;
use ds_mod::{Hash128, Module, TargetClass};
use ds_parse::{Ast, Expr, Ident, ImplDecl, Item, ItemKind, Param, Pattern, StructMember};
pub use error::LowerError;
use func::{FnLower, Plan};
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

    /// One function's placed op sequence: the runnable uops plus the
    /// placed RC ops (dup/drop) at their exact positions — the artifact
    /// the M4 placement contract pins and the VM seam will splice in.
    #[must_use]
    pub fn placed(&self, name: &str) -> Option<&[PlacedOp]> {
        self.function_index(name)
            .and_then(|index| self.placed.get(index as usize).map(Vec::as_slice))
    }
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
    if !analyzed.checked.diags.is_empty() {
        return Err(LowerError::NotChecked {
            count: analyzed.checked.diags.len(),
        });
    }
    let mut plan = Plan::new();
    plan_deinits(asts, &mut plan, analyzed);
    for ast in asts {
        for block in &ast.blocks {
            for item in &block.items {
                refuse_derive(item)?;
                plan_item(item, &mut plan, analyzed);
            }
        }
    }
    if !plan.has_main() {
        return Err(LowerError::NoMain);
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
    let (functions, placed, shapes) = lower_all(&mut plan, analyzed, &lin)?;
    let mut module = Module::new(module_name, TargetClass::Bytecode, source_hash);
    for function in functions {
        module = module.with_function(function);
    }
    Ok(Lowered {
        module,
        functions: plan.names(),
        tags: shapes.tag_names(),
        placed,
    })
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

/// A `#[derive(...)]` attribute refuses at lowering: the derive engine
/// is M6's (G1) work, so the attribute generates no behavior yet, and
/// the refusal reports that at the attribute's own span. Parse, check,
/// and fmt still accept attributed declarations; only lowering stops.
fn refuse_derive(item: &Item) -> Result<(), LowerError> {
    for attr in &item.attrs {
        if attr.name.name == "derive" {
            return Err(LowerError::unsupported(
                attr.span,
                "a derive attribute",
                "the M6 derive engine owns it; until it lands the attribute generates no behavior",
            ));
        }
    }
    Ok(())
}

/// Collects one top-level item into the plan when it is a lowerable
/// monomorphic const-arrow function. Everything else is skipped; generic
/// functions and comptime items are the documented M6 exemption.
fn plan_item(item: &Item, plan: &mut Plan, analyzed: &Analyzed) {
    if let ItemKind::Impl(imp) = &item.kind {
        plan_impl(item, imp, plan, analyzed);
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
    let Expr::Lambda { generics, .. } = &local.init else {
        return; // a non-function const: no module function for it
    };
    if !generics.is_empty() {
        return; // the generic exemption: M6's CALL_GENERIC owns these
    }
    let Some(def) = analyzed.package.lookup(&name.name) else {
        return;
    };
    if !analyzed.checked.def_types.contains_key(&def) {
        return; // the checker has no typed value for it (extern or later stage)
    }
    plan.push(name.name.clone(), def, local);
}

/// Collects one trait impl's member bodies into the plan (the trait
/// dispatch seam): each member fn enters the function table under its
/// mangled identity `<trait>#<target>#<member>` with the checker-solved
/// return its FnDecl names. A generic impl's bodies lower per resolved
/// receiver type, which no static plan can name: the item is marked
/// runtime-evidence territory and call sites dispatching into it refuse
/// naming the seam. Inherent impls plan through their own mangle below.
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
    let bounds = impl_bounds(item.span, analyzed);
    if !imp.generics.is_empty() {
        // the runtime-evidence territory [H3]: a generic impl's member
        // bodies lower once per impl (never per instantiation), the call
        // sites entering them stage the per-bound evidence words the
        // body's nested dispatches read, and the marker serves the
        // transitive refusal — a nested dispatch whose serving body is
        // itself generic has no staging yet
        plan.note_runtime_impl(item.span);
    }
    for f in &imp.fns {
        let Some(body) = &f.body else {
            continue;
        };
        let Some(ret) = analyzed.decls.impl_fn_rets.get(&f.span).copied() else {
            continue;
        };
        let body_expr = Rc::new(Expr::Block(body.clone()));
        let evidence = if bounds.is_empty() {
            Rc::new(Vec::new())
        } else {
            let res = &analyzed.resolutions[f.span.file.as_u32() as usize];
            Rc::new(func::runtime_pairs(analyzed, res, &bounds, &body_expr))
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
/// need runtime-resolved evidence no inherent call site can stage, so
/// the item is marked runtime-evidence territory and call sites refuse
/// naming the seam.
fn plan_inherent_impl(item: &Item, imp: &ImplDecl, plan: &mut Plan, analyzed: &Analyzed) {
    if !imp.generics.is_empty() {
        plan.note_runtime_impl(item.span);
        return;
    }
    for f in &imp.fns {
        let Some(body) = &f.body else {
            continue;
        };
        let Some(ret) = analyzed.decls.impl_fn_rets.get(&f.span).copied() else {
            continue;
        };
        let name = format!("{}#{}", render_type(&imp.target), f.name.name);
        plan.push_declared(
            name,
            item.span,
            f.name.name.clone(),
            Rc::new(f.params.clone()),
            Rc::new(Expr::Block(body.clone())),
            ret,
            Rc::new(Vec::new()),
        );
    }
}

/// The declared bounds of one generic impl's type variables: parameter
/// name to the traits its `[t: trait]` clauses name, from the instances
/// the impl declared. The name (not the minted atom) is the key because
/// the instance declaration and the body's checking mint their own
/// variables; a bare `[t]` parameter bounds nothing, so calls on it
/// never check clean and no demand collects for it.
fn impl_bounds(impl_site: Span, analyzed: &Analyzed) -> HashMap<String, HashSet<TraitId>> {
    let mut out: HashMap<String, HashSet<TraitId>> = HashMap::new();
    for (instance, site) in &analyzed.decls.instance_impls {
        if *site != impl_site {
            continue;
        }
        let Ok(conditions) = analyzed.decls.instances.conditions(*instance) else {
            continue;
        };
        for cond in conditions {
            let [var] = analyzed.decls.universe.members(cond.ty) else {
                continue;
            };
            let Some(param) = shape::var_param_name(&analyzed.decls.universe, *var) else {
                continue;
            };
            out.entry(param).or_default().insert(cond.trait_id);
        }
    }
    out
}

/// What lowering every planned function yields: the module's functions,
/// each function's placed op sequence, and the shared shape tables.
type LowerAll<'b> = (Vec<ds_mod::Function>, Vec<Vec<PlacedOp>>, Shapes<'b>);

/// Lowers the planned functions as a worklist: a body may register
/// synthesized lambda functions as it lowers (F2), and every registered
/// function lowers in turn. Each one's resolutions come from its own
/// file (the bundle is keyed by file id).
fn lower_all<'b>(
    plan: &mut Plan,
    analyzed: &'b Analyzed,
    lin: &ds_check::Linearity,
) -> Result<LowerAll<'b>, LowerError> {
    let mut shapes = Shapes::new(analyzed, plan.declared_deinits());
    let mut functions = Vec::with_capacity(plan.functions().len());
    let mut placed = Vec::with_capacity(plan.functions().len());
    let mut index = 0;
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
        )
        .map_err(|err| err.respanned(planned.lambda.span()))?;
        functions.push(function);
        placed.push(ops);
        index += 1;
    }
    Ok((functions, placed, shapes))
}
