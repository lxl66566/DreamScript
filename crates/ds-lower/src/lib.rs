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
//! Early exits lower exactly as the golden ds-vm fixtures do: `or return`
//! and `?` emit TAG_TEST + JMPIF, then either construct the alternative
//! or re-span the propagated member into the enclosing return union with
//! RETSPAN. Interpolation lowers to `str_*` builtin calls chained with
//! STRCAT. Tail positions lower to CALLT (the stack never grows [H2]) and
//! final expressions to RET/RETSPAN.
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
mod shape;

use std::collections::HashMap;

use ds_check::Analyzed;
use ds_mod::{Hash128, Module, TargetClass};
use ds_parse::{Ast, Expr, Item, ItemKind, Pattern};
pub use error::LowerError;
use func::{FnLower, Plan};
use shape::Shapes;

/// The lowered module plus the tables callers need: the function table's
/// names in index order, and the module-wide tag ids.
pub struct Lowered {
    module: Module,
    functions: Vec<String>,
    tags: HashMap<String, u8>,
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
    let (functions, shapes) = lower_all(&plan, analyzed)?;
    let mut module = Module::new(module_name, TargetClass::Bytecode, source_hash);
    for function in functions {
        module = module.with_function(function);
    }
    Ok(Lowered {
        module,
        functions: plan.names(),
        tags: shapes.tag_names(),
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

/// Lowers every planned function; each one's resolutions come from its
/// own file (the bundle is keyed by file id).
fn lower_all<'b>(
    plan: &Plan<'_>,
    analyzed: &'b Analyzed,
) -> Result<(Vec<ds_mod::Function>, Shapes<'b>), LowerError> {
    let mut shapes = Shapes::new(analyzed);
    let mut functions = Vec::with_capacity(plan.functions().len());
    for planned in plan.functions() {
        let res = &analyzed.resolutions[planned.local.span.file.as_u32() as usize];
        functions.push(FnLower::lower(analyzed, res, plan, &mut shapes, planned)?);
    }
    Ok((functions, shapes))
}
