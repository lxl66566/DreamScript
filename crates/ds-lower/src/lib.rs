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
//! 1. **Planning** collects the lowerable top-level functions: monomorphic const-arrow lambdas
//!    only. Generic functions and comptime items are exempt — nothing is emitted for them, and a
//!    call into one is a [`LowerError::Unsupported`] naming the M6 `CALL_GENERIC` seam. A `main`
//!    entry must exist.
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

use std::collections::HashMap;

use ds_abi::{RcOp, Uop};
use ds_check::Analyzed;
use ds_mod::{Hash128, Module, TargetClass};
use ds_parse::{Ast, Expr, Item, ItemKind, Pattern};
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
    for ast in asts {
        for block in &ast.blocks {
            for item in &block.items {
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
    let (functions, placed, shapes) = lower_all(&plan, analyzed, &lin)?;
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

/// Collects one top-level item into the plan when it is a lowerable
/// monomorphic const-arrow function. Everything else is skipped; generic
/// functions and comptime items are the documented M6 exemption.
fn plan_item<'a>(item: &'a Item, plan: &mut Plan<'a>, analyzed: &Analyzed) {
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

/// What lowering every planned function yields: the module's functions,
/// each function's placed op sequence, and the shared shape tables.
type LowerAll<'b> = (Vec<ds_mod::Function>, Vec<Vec<PlacedOp>>, Shapes<'b>);

/// Lowers every planned function; each one's resolutions come from its
/// own file (the bundle is keyed by file id).
fn lower_all<'b>(
    plan: &Plan<'_>,
    analyzed: &'b Analyzed,
    lin: &ds_check::Linearity,
) -> Result<LowerAll<'b>, LowerError> {
    let mut shapes = Shapes::new(analyzed);
    let mut functions = Vec::with_capacity(plan.functions().len());
    let mut placed = Vec::with_capacity(plan.functions().len());
    for planned in plan.functions() {
        let res = &analyzed.resolutions[planned.local.span.file.as_u32() as usize];
        let (function, ops) = FnLower::lower(analyzed, res, plan, &mut shapes, lin, planned)?;
        functions.push(function);
        placed.push(ops);
    }
    Ok((functions, placed, shapes))
}
