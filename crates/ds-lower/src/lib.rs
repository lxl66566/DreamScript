//! # ds-lower: checked programs to executable dsmod modules
//!
//! The lowering pass: one checked package (parse -> `ds_check::analyze`)
//! becomes one ds-mod module — frame layouts per the locked rules,
//! uop bodies per the locked uop set — that the tier-1 VM runs through the
//! ds-mod writer/loader path.
//!
//! # Architecture
//!
//! Three stages per package:
//!
//! 1. **Planning** collects the lowerable top-level functions: const-arrow lambdas — monomorphic or
//!    free-generic (plus the synthesized functions lambda values register mid-lowering). A free
//!    generic fn lowers its ONE body once: call sites stage the instantiated evidence words its
//!    body's runtime demands read, in the outgoing-argument area after the true arguments. Comptime
//!    items stay exempt, and generic lambdas as values and comptime parameters refuse at their own
//!    doors. A `main` entry must exist.
//! 2. **Shapes** map every solved type onto its slot representation ([`shape` module docs]):
//!    scalars are one word, bare structs are field spans, unions are tag+payload spans with
//!    per-member payload offsets. Tag ids are module-wide, assigned in first-use order.
//! 3. **Per-function lowering** (`[func]`) walks each body once, reading the checker's recorded
//!    tables: per-expression solved types (`expr_types`, flow-refined), per-binder declared types
//!    (`binders`), and per-use name resolutions. Slots bump-allocate (params first, then
//!    locals/temps); the outgoing-argument area sizes itself as the max over call sites of staged
//!    words and result spans, and any overlay word that carries more than one word type over the
//!    frame's lifetime is `SlotType::Raw`.
//!
//! Early exits lower exactly as the golden ds-vm fixtures do: `?` and the
//! extraction handlers emit TAG_TEST + JMPIF, then either construct the
//! alternative or re-span the propagated member into the enclosing return
//! union with RETSPAN. Interpolation lowers to `str_*` builtin calls
//! chained with STRCAT. Tail positions lower to CALLT (the stack never
//! grows) and final expressions to RET/RETSPAN.
//!
//! # The Perceus placement pass (rides the walk)
//!
//! The walk in `[func]` is also the forward pass of the dup/drop placement
//! (`[place]`): every Ref-typed word owns a reference count, and both the
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
mod types;

use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
};

use ds_abi::{RcOp, Uop};
use ds_check::{Analyzed, Condition, TraitId, render_type};
use ds_diag::Span;
use ds_mod::{Dep, Hash128, Module, TargetClass};
use ds_parse::{Ast, Expr, Ident, ImplDecl, Item, ItemKind, Param, Pattern, StructMember};
pub use error::LowerError;
use func::{BodyId, EvidenceEnv, FnLower, GenericFn, Plan};
use shape::Shapes;

/// One op of a function body: a runnable micro-operation or a placed
/// reference-counting op. The module's executable code carries the
/// same sequence, so this type is the placement contract the VM stream
/// agrees with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlacedOp {
    /// A runnable micro-operation.
    Uop(Uop),
    /// A placed reference-counting op.
    Rc(RcOp),
}

/// One function's lowering yield: the validated record, its placed op
/// sequence, the resumed-frame facts, and the span marks (final word pc
/// plus the source span each region runs under).
pub type FinalizedFn = (ds_mod::Function, Vec<PlacedOp>, FnFacts, Vec<(u32, Span)>);

/// The served ambient splice: the pre-lowered function records plus
/// the member-tag rows their bodies minted, both read off the checked-in
/// dsmod artifact. The rows seed the merged lowering's tag table so the
/// bodies the plan lowers here mint after the spliced prefix with the
/// ids the fresh lowering's ambient-first walk would assign them —
/// byte-identical to the fresh module, which the splice's equivalence
/// gate pins.
pub struct Served {
    /// The pre-lowered bodies, the plan's function-table prefix.
    pub functions: Vec<ds_mod::Function>,
    /// The member-tag rows the bodies mint, in id order.
    pub tags: Vec<ds_mod::MemberTag>,
    /// The served bodies' pc-to-span rows, re-installed verbatim so the
    /// merged module's table equals the fresh lowering's; `None` when
    /// the served artifact carries no table (stripped).
    pub spans: Option<ds_mod::SpanTable>,
}

/// The lowered module plus the tables callers need: the function table's
/// names in index order, the module-wide tag ids, and each function's
/// placed op sequence.
pub struct Lowered {
    module: Module,
    functions: Vec<String>,
    carried_defaults: HashMap<String, u32>,
    /// The instances whose evidence words the module bakes statically:
    /// the internal-exercise fact a consuming link reads before it
    /// shadows the declaring impl under a link winner.
    exercised: HashSet<u32>,
    tags: Vec<ds_mod::MemberTag>,
    placed: Vec<Vec<PlacedOp>>,
    facts: Vec<FnFacts>,
    /// How many function records a served splice installed in front of
    /// the plan's own table; a fresh lowering answers zero.
    spliced: usize,
}

/// One function's lowering facts beyond the artifact itself: the
/// per-statement value ends of `main` (the resumed-frame seam's resume
/// targets — the code length after each direct body statement's value
/// code, the last entry the tail's exit boundary), and every binder's
/// registered body slots (the `:drop` seam's word discovery).
#[derive(Clone, Debug, Default)]
pub struct FnFacts {
    /// Code positions, one per direct body statement plus the tail's
    /// exit boundary; empty for every function that is not `main`.
    pub(crate) value_ends: Vec<u32>,
    /// The binder span to body-slot words map (`main`'s is the `:drop`
    /// seam's discovery surface).
    pub(crate) registered: HashMap<Span, Vec<u8>>,
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

    /// The trait default bodies a link surface may carry, keyed by
    /// mangled name over this module's own table: exactly the rows
    /// staging no runtime evidence. A consumer's dispatch whose solving
    /// impl spells no member addresses the linked row.
    #[must_use]
    pub fn carried_default_bodies(&self) -> &HashMap<String, u32> {
        &self.carried_defaults
    }

    /// The instances whose evidence words the module bakes statically:
    /// a dispatch site's solved instance word or a chain route naming
    /// the impl's body. A consuming link cannot shadow such an impl —
    /// the baked words would keep the loser's behavior under the
    /// winner — so the surface stamps the fact on its impl rows.
    #[must_use]
    pub fn exercised(&self) -> &HashSet<u32> {
        &self.exercised
    }

    /// The function-table index of a lowered function name.
    #[must_use]
    pub fn function_index(&self, name: &str) -> Option<u32> {
        self.functions
            .iter()
            .position(|f| f == name)
            .map(|index| u32::try_from(index).expect("function index fits u32"))
    }

    /// How many function records a served splice installed in front of
    /// the plan's own table; a fresh lowering answers zero. The count is
    /// the observable difference between a served splice and a silent
    /// fallback: the two answer byte-identical modules by contract, so
    /// a gate that must tell them apart reads this.
    #[must_use]
    pub fn spliced(&self) -> usize {
        self.spliced
    }

    /// The module's tag-table rows, one per minted atom in id order,
    /// each carrying the declaring declaration's name beside the label
    /// (two files of one package may share a label, so a name-keyed
    /// lookup is ambiguous — the rows and the ids are the truth).
    #[must_use]
    pub fn tags(&self) -> &[ds_mod::MemberTag] {
        &self.tags
    }

    /// The module-wide tag id of a declared atom label's first row, as
    /// a host building `Value::Tag` arguments over a label-unique module
    /// needs. A module with two rows of one label answers the
    /// lowest-id row — the first-use order's first mint.
    #[must_use]
    pub fn tag(&self, name: &str) -> Option<u8> {
        self.tags
            .iter()
            .find(|row| row.name == name)
            .map(|row| row.id)
    }

    /// Records the module's dependency list into the header (link
    /// table), so a built artifact names its linked deps and a load walk
    /// over it resolves without re-deriving the list from source.
    /// Lowering itself produces no cross-module links (natives are named,
    /// the ambient's impl bodies inline), so the compile seam owns the
    /// list: it collects the source's import declarations.
    #[must_use]
    pub fn with_deps(mut self, deps: Vec<Dep>) -> Self {
        self.module = deps.into_iter().fold(self.module, |module, dep| {
            module.with_dep(dep.name, dep.content_hash)
        });
        self
    }

    /// One function's placed op sequence: the runnable uops plus the
    /// placed RC ops (dup/drop) at their exact positions — the artifact
    /// the placement contract pins and the VM seam will splice in. A
    /// body served from the checked-in artifact answers an empty stream:
    /// its code words already ride the module verbatim.
    #[must_use]
    pub fn placed(&self, name: &str) -> Option<&[PlacedOp]> {
        self.function_index(name)
            .and_then(|index| self.placed.get(index as usize).map(Vec::as_slice))
    }

    /// `main`'s per-statement value ends, in statement order: the
    /// resumed-frame seam's resume targets, the last entry the tail's exit
    /// boundary.
    #[must_use]
    pub fn value_ends(&self, name: &str) -> Option<&[u32]> {
        self.function_index(name)
            .and_then(|index| self.facts.get(index as usize))
            .map(|facts| facts.value_ends.as_slice())
    }

    /// The named function's whole registration map: binder span to body
    /// slots, owning registrations only.
    #[must_use]
    pub fn registered(&self, name: &str) -> Option<&HashMap<Span, Vec<u8>>> {
        self.function_index(name)
            .and_then(|index| self.facts.get(index as usize))
            .map(|facts| &facts.registered)
    }
}

/// One embedded comptime answer, as the runtime module materializes it
/// at the call site: the evaluator's recorded constant. The
/// embeddable results are the scalar atoms, string, a list over
/// embeddable elements, and a tuple over embeddable scalars (each the
/// literal a user would have written) — a comptime function answering
/// anything else refuses at the evaluator, never here.
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
    /// A list answer over embeddable elements, in order.
    List(Vec<ComptimeConst>),
    /// A tuple answer over embeddable scalar elements, in order.
    Tuple(Vec<ComptimeConst>),
}

/// How the round lowers: the ordinary program mode, or the REPL's
/// resumed-frame session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LowerMode {
    /// The ordinary run path: a binder's words die where the placement
    /// proves their last use, and `main`'s scope exit runs at completion.
    Program,
    /// The REPL's resumed frame: `main`'s binders pin (their words stay
    /// claimed to the frame's teardown — a session-live value must never
    /// die at a text-relative last use, since a later submission may
    /// still read it), and the per-statement value ends record.
    ResumedFrame,
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
/// function bodies over the planned prefix instead of walking them,
/// `facts` carries a caller's already-run affine walk (the walk is
/// deterministic over the same tables, so the same inputs answer the same
/// rows; `None` walks here — the backstop for embedders that run no gate
/// of their own), `comptime` selects the evaluator's slice or embeds its
/// recorded answers at the call sites, and `mode` carries the REPL
/// session's resumed-frame facts.
// the slice's admitted callee set and const-evaluation site set are a
// ninth and tenth lowering input beside the run's own eight; bundling
// them would invent a config struct for one call path
#[allow(clippy::too_many_arguments)]
fn lower_module<T>(
    module_name: &str,
    source_hash: Hash128,
    asts: &[T],
    analyzed: &Analyzed,
    main_required: bool,
    served: Option<Served>,
    admitted: &[ds_check::DefId],
    consts: &[ds_check::DefId],
    facts: Option<&ds_check::Linearity>,
    comptime: func::ComptimeRun<'_>,
    mode: LowerMode,
) -> Result<Lowered, LowerError>
where
    T: std::borrow::Borrow<Ast>,
{
    let slice = matches!(comptime, func::ComptimeRun::Slice);
    // a warning reports and the package lowers: the link layering's
    // shadowing warnings ride the diagnostic list beside errors, and
    // only an error leaves the tables unlowerable
    let errors = analyzed
        .checked
        .diags
        .iter()
        .filter(|diag| diag.severity == ds_diag::Severity::Error)
        .count();
    if errors > 0 {
        return Err(LowerError::NotChecked { count: errors });
    }
    let mut plan = Plan::new();
    plan.set_demands(collect_demands(asts, analyzed));
    // the type lens: the plan's discovery substitutes the recorded
    // types of every instantiated body under it, and the lowering reads
    // the results through the shape layer — one registry, the
    // analyzed package's own until a substitution mints a type
    let mut types = types::Types::new(
        &analyzed.decls.universe,
        &analyzed.decls.apply_structs,
        &analyzed.decls.apply_enums,
    );
    // every struct declaring a `deinit`: the instance keys read the set
    // before the plan's own deinit table completes (it fills in only
    // after all items plan), and a deinit-carrying struct keys an
    // instance by its own atom, unshared
    let deinits = deinit_structs(asts, analyzed);
    let mut found = func::Discovery {
        analyzed,
        types: &mut types,
        deinits: &deinits,
        comptime_path: Vec::new(),
        comptime_instance: None,
    };
    for tree in asts {
        let ast = tree.borrow();
        for block in &ast.blocks {
            for item in &block.items {
                refuse_derive(item)?;
                let first = plan.functions().len();
                plan_item(item, &mut plan, analyzed, slice, admitted, consts);
                // each struct's deinit plans right after its own item, so
                // one file's entries stay contiguous and append-stable:
                // the ambient's records (items, deinits, and their minted
                // records) remain the plan's prefix the splice serves,
                // while a growing file's new declaration appends after
                // every earlier entry — item, deinit, and record alike —
                // instead of a shifted deinit re-keying the indices a
                // live frame's retiring row still names, the reorder a
                // swap refuses over
                if !slice {
                    plan_deinits(item, &mut plan, analyzed);
                }
                // the item's lambda-value and spawn sites mint their
                // records, and its calls entering a free generic fn plan
                // the body they enter — an instance over the solved shape,
                // the ONE body where the site still names a variable —
                // before any body lowers: every record and instance
                // lands inside the item's contiguous block, and the walk
                // that reaches a site reads the plan's answer instead of
                // planning — the discipline the served splice's
                // plan-time prefix needs over lowering-time records
                func::discover_item(&mut plan, &mut found, first)?;
            }
        }
    }
    if main_required && !slice && !plan.has_main() {
        return Err(LowerError::NoMain);
    }
    if let Some(served) = &served {
        let names = plan.names();
        let functions = &served.functions;
        let detail = if names.len() < functions.len() {
            Some(format!(
                "the plan holds {} functions, the serve holds {}",
                names.len(),
                functions.len()
            ))
        } else {
            names
                .iter()
                .zip(functions.iter())
                .position(|(planned, body)| planned != body.name())
                .map(|at| {
                    format!(
                        "planned function {at} is `{}` but the served body names `{}`",
                        names[at],
                        functions[at].name()
                    )
                })
        };
        if let Some(detail) = detail {
            return Err(LowerError::ServedDrift { detail });
        }
    }
    // the affine facts the placement pass consumes: last uses and moves
    // come from the checker, never re-derived here — the caller's
    // own walk serves when it already ran over the same tables
    let walked;
    let lin = if let Some(lin) = facts {
        lin
    } else {
        walked = ds_check::linearity_check(
            asts,
            &analyzed.package,
            &analyzed.resolutions,
            &analyzed.decls,
            &analyzed.checked,
        );
        &walked
    };
    // the walk's own refusals gate the facts: placement over a violating
    // walk is unsound (a moved-then-used value's deinit would fire twice),
    // so a package the walk reports refuses here the same way an unchecked
    // one does — the embedder's backstop when no CLI gate ran first
    if !lin.diags.is_empty() {
        return Err(LowerError::NotLinear {
            count: lin.diags.len(),
        });
    }
    let served_spans = served.as_ref().and_then(|served| served.spans.clone());
    let (functions, placed, facts, shapes, spliced, marks) = lower_all(
        &mut plan,
        analyzed,
        &types,
        lin,
        &platform_name(asts),
        served,
        comptime,
        mode,
    )?;

    let mut module = Module::new(module_name, TargetClass::Bytecode, source_hash);
    for function in functions {
        module = module.with_function(function);
    }
    for chain in plan.take_chains() {
        module = module.with_chain(chain);
    }
    // the tag table rides the artifact in first-use order, one row per
    // minted atom carrying its declaring declaration's name beside the
    // label (a defless structural atom an empty declaring name)
    let tags = shapes.tag_rows();
    for row in &tags {
        module = module.with_tag(row.decl.clone(), row.name.clone(), row.id);
    }
    module = module.with_pc_spans(span_table(analyzed, served_spans, spliced, &marks));
    Ok(Lowered {
        module,
        functions: plan.names(),
        carried_defaults: plan.carried_defaults().clone(),
        exercised: plan.exercised().clone(),
        tags,
        placed,
        facts,
        spliced,
    })
}

/// Lowers one checked package to a ds-mod module named `module_name`.
///
/// # Errors
///
/// [`LowerError::NotChecked`] when the package still carries checker
/// diagnostics; [`LowerError::Unsupported`] for constructs outside the
/// runtime contract (each error names the seam it needs); [`LowerError::NoMain`]
/// when no lowerable `main` entry exists.
pub fn lower<T>(
    module_name: &str,
    source_hash: Hash128,
    asts: &[T],
    analyzed: &Analyzed,
) -> Result<Lowered, LowerError>
where
    T: std::borrow::Borrow<Ast>,
{
    lower_in_mode(
        module_name,
        source_hash,
        asts,
        analyzed,
        None,
        LowerMode::Program,
    )
}

/// [`lower`] under an explicit [`LowerMode`] and a caller's already-run
/// affine walk — the REPL's resumed-frame session is the caller, handing
/// the rows its check gate paid for so a submission never pays the
/// lowerer's backstop re-walk (the same facts seam `lower_with_comptime`
/// gives the run and reload paths).
///
/// # Errors
///
/// [`lower`]'s own.
pub fn lower_in_mode<T>(
    module_name: &str,
    source_hash: Hash128,
    asts: &[T],
    analyzed: &Analyzed,
    facts: Option<&ds_check::Linearity>,
    mode: LowerMode,
) -> Result<Lowered, LowerError>
where
    T: std::borrow::Borrow<Ast>,
{
    let empty = ComptimeValues::default();
    lower_module(
        module_name,
        source_hash,
        asts,
        analyzed,
        true,
        None,
        &[],
        &[],
        facts,
        func::ComptimeRun::Runtime(&empty),
        mode,
    )
}

/// Lowers the ambient's declaration package for the checked-in dsmod
/// artifact: the run contract's `main` requirement is waived — a
/// declaration-only member has no entry — and everything else runs
/// exactly as [`lower`]. The artifact's consumer splices the returned
/// functions in front of a user package's own table (see
/// [`lower_with_served`]), so the ambient's items must keep planning
/// first in the combined package for the indexes to line up, and the
/// returned tag table rides beside them (the splice seeds the merged
/// module's ids with it, so the artifact's bodies and tags travel
/// together).
///
/// # Errors
///
/// [`LowerError::NotChecked`] and [`LowerError::Unsupported`] as
/// [`lower`]; never [`LowerError::NoMain`].
pub fn lower_ambient<T>(
    module_name: &str,
    source_hash: Hash128,
    asts: &[T],
    analyzed: &Analyzed,
) -> Result<Lowered, LowerError>
where
    T: std::borrow::Borrow<Ast>,
{
    let empty = ComptimeValues::default();
    lower_module(
        module_name,
        source_hash,
        asts,
        analyzed,
        false,
        None,
        &[],
        &[],
        None,
        func::ComptimeRun::Runtime(&empty),
        LowerMode::Program,
    )
}

/// The runtime-module lowering with the comptime evaluator's answers
/// embedded: comptime functions plan nothing (their call sites emit the
/// recorded constants), and every other surface lowers exactly like
/// [`lower`]. `facts` carries a caller's already-run affine walk, `None`
/// walking here.
///
/// # Errors
///
/// As [`lower`], plus [`LowerError::Unsupported`] for a comptime call
/// whose span the evaluator never recorded.
pub fn lower_with_comptime<T>(
    module_name: &str,
    source_hash: Hash128,
    asts: &[T],
    analyzed: &Analyzed,
    facts: Option<&ds_check::Linearity>,
    values: &ComptimeValues,
) -> Result<Lowered, LowerError>
where
    T: std::borrow::Borrow<Ast>,
{
    lower_module(
        module_name,
        source_hash,
        asts,
        analyzed,
        true,
        None,
        &[],
        &[],
        facts,
        func::ComptimeRun::Runtime(values),
        LowerMode::Program,
    )
}

/// The library lowering for a module that serves members instead of
/// running: [`lower_with_comptime`]'s lowering with the run contract's
/// `main` entry check waived, `served` splicing pre-lowered ambient
/// bodies over the planned prefix exactly as [`lower_with_served`] does —
/// an installed module's callers reach its members and never its entry.
/// `facts` carries a caller's already-run affine walk, `None` walking
/// here.
///
/// # Errors
///
/// [`lower_with_served`]'s and [`lower_with_comptime`]'s; never
/// [`LowerError::NoMain`].
pub fn lower_library<T>(
    module_name: &str,
    source_hash: Hash128,
    asts: &[T],
    analyzed: &Analyzed,
    served: Option<Served>,
    facts: Option<&ds_check::Linearity>,
    values: &ComptimeValues,
) -> Result<Lowered, LowerError>
where
    T: std::borrow::Borrow<Ast>,
{
    lower_module(
        module_name,
        source_hash,
        asts,
        analyzed,
        false,
        served,
        &[],
        &[],
        facts,
        func::ComptimeRun::Runtime(values),
        LowerMode::Program,
    )
}

/// The comptime slice the evaluator compiles for its fenced run: the
/// package's comptime functions plus the ordinary helpers whose defs
/// `admitted` names (the evaluator's row-based admission pass), all
/// lowered as ordinary locals — a comptime function calling another, or
/// an admitted helper, is a plain intra-module call there, and calls
/// into runtime functions the set does not name refuse (the slice's
/// table never holds them). The exported consts whose defs `consts`
/// names (the dep-side evaluation's sites) plan as zero-parameter
/// bodies answering their initializers' values. No `main` is required
/// and no deinit hooks plan (the evaluator's admission refuses a
/// deinit-carrying construction before the slice lowers, so no value
/// inside the fence ever expects a hook).
///
/// # Errors
///
/// [`LowerError::NotChecked`] when the package still carries checker
/// diagnostics; [`LowerError::Unsupported`] for comptime bodies outside
/// the covered runtime contract.
pub fn lower_comptime_slice<T>(
    module_name: &str,
    source_hash: Hash128,
    asts: &[T],
    analyzed: &Analyzed,
    admitted: &[ds_check::DefId],
    consts: &[ds_check::DefId],
) -> Result<Lowered, LowerError>
where
    T: std::borrow::Borrow<Ast>,
{
    lower_module(
        module_name,
        source_hash,
        asts,
        analyzed,
        false,
        None,
        admitted,
        consts,
        None,
        func::ComptimeRun::Slice,
        LowerMode::Program,
    )
}

/// Lowers one checked package with the ambient's function bodies served
/// from a checked-in artifact instead of lowered here. The served bodies
/// must be exactly what lowering the ambient member alone produces —
/// [`lower_ambient`] over the same trees — and planning here must
/// reproduce their function-table prefix: the served list installs at
/// indexes `0..n`, every body-walking seam (dispatch staging, call
/// FuncRefs, evidence demands) reads the planned entries as ever, and
/// only the per-body lowering walk is skipped. The served tag rows seed
/// the mint table so the bodies lowered here continue the ambient's
/// ids — the fresh lowering's ambient-first walk mints the same rows in
/// the same order, which the splice's equivalence gate pins. The
/// ambient member must sit first in `asts` (the run path's registration
/// order); a served list whose length, names, or tag rows disagree with
/// the plan is [`LowerError::ServedDrift`] — the caller falls back to
/// [`lower`]. `facts` carries a caller's already-run affine walk, `None`
/// walking here.
///
/// # Errors
///
/// As [`lower`], plus [`LowerError::ServedDrift`] when the served
/// prefix disagrees with the plan.
pub fn lower_with_served<T>(
    module_name: &str,
    source_hash: Hash128,
    asts: &[T],
    analyzed: &Analyzed,
    served: Served,
    facts: Option<&ds_check::Linearity>,
    values: &ComptimeValues,
) -> Result<Lowered, LowerError>
where
    T: std::borrow::Borrow<Ast>,
{
    lower_module(
        module_name,
        source_hash,
        asts,
        analyzed,
        true,
        Some(served),
        &[],
        &[],
        facts,
        func::ComptimeRun::Runtime(values),
        LowerMode::Program,
    )
}

/// The deinit structs the plan's instance keys read: every struct whose
/// declaration carries a `deinit` member. The plan's own deinit table
/// completes only after all items plan, and the discovery keys
/// instances between an item's planning and the plan's completion, so
/// the keys read this pre-pass set — the same defs the plan's own
/// planning collects, taken from the trees directly.
fn deinit_structs<T>(asts: &[T], analyzed: &Analyzed) -> HashSet<ds_check::DefId>
where
    T: std::borrow::Borrow<Ast>,
{
    let mut out = HashSet::new();
    for ast in asts.iter().map(std::borrow::Borrow::<Ast>::borrow) {
        for block in &ast.blocks {
            for item in &block.items {
                let ItemKind::Struct(s) = &item.kind else {
                    continue;
                };
                if !s
                    .members
                    .iter()
                    .any(|member| matches!(member, StructMember::Deinit { .. }))
                {
                    continue;
                }
                if let Some(def) = analyzed.package.lookup_type(&s.name.name) {
                    out.insert(def);
                }
            }
        }
    }
    out
}

/// Plans one struct item's declared `deinit` member as a synthesized
/// module function before any body lowers: binding a value of a
/// deinit-carrying struct claims the hook by naming this plan's function,
/// so the table index must exist before the first binder mints its pool
/// constant. The synthesized function is one borrowed `self` parameter —
/// the dying instance — whose body is the declared block, named after the
/// struct. A generic struct's deinit registers without a plan:
/// binding its values refuses with the CALL_GENERIC seam instead of
/// silently skipping the hook.
fn plan_deinits(item: &Item, plan: &mut Plan, analyzed: &Analyzed) {
    let ItemKind::Struct(s) = &item.kind else {
        return;
    };
    let Some((span, body)) = s.members.iter().find_map(|member| match member {
        StructMember::Deinit { span, body } => Some((*span, body.clone())),
        StructMember::Field(_) => None,
    }) else {
        return;
    };
    let Some(def) = analyzed.package.lookup_type(&s.name.name) else {
        return;
    };
    if !s.generics.is_empty() {
        plan.declare_deinit(def);
        return;
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
/// derive expansion consumes the attribute there, so an attribute
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
/// bounds collect. Everything else is skipped; comptime functions
/// are the evaluator's slice — the runtime module plans none of them and
/// the slice plans nothing else beyond the row-admitted helpers
/// `admitted` names (the evaluator's own admission pass).
fn plan_item(
    item: &Item,
    plan: &mut Plan,
    analyzed: &Analyzed,
    slice: bool,
    admitted: &[ds_check::DefId],
    consts: &[ds_check::DefId],
) {
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
    // The planned item is a value binding, so its def reads the value
    // namespace: a fn sharing an ambient tag's label plans under its own
    // def, and a call the checker resolved to the fn lowers as the fn
    // call.
    let Some(def) = analyzed.package.lookup_value(&name.name) else {
        return;
    };
    if !analyzed.checked.def_types.contains_key(&def) {
        return; // the checker has no typed value for it (extern or later stage)
    }
    // A dep-side evaluation site (slice alone): the exported const's
    // initializer plans as its own zero-parameter body, the answer the
    // fenced run drives for the link fact's value. The runtime module
    // never plans it — a non-function const stays without a module
    // function there, exactly as before.
    if slice && consts.contains(&def) && !matches!(local.init, Expr::Lambda { .. }) {
        plan.push_const(name.name.clone(), def, local, analyzed);
        return;
    }
    let Expr::Lambda {
        generics, params, ..
    } = &local.init
    else {
        return; // a non-function const: no module function for it
    };
    let comptime_fn = params
        .iter()
        .any(|p| matches!(p.mode, ds_parse::ParamMode::Comptime(_)));
    if comptime_fn != slice && !(slice && admitted.contains(&def)) {
        return; // the runtime module and the comptime slice split here; an
        // admitted helper is the slice's one exception, a row-admitted
        // callee the evaluator's admission pass named for this slice
    }
    if !generics.is_empty() {
        // A free generic fn registers without a table entry: a body that
        // lowers once per shape an entering site solves (the discovery
        // planning each instance), the shape a site still naming a
        // variable enters being the ONE Raw-word body. Its declared
        // bounds collect the runtime-evidence demands its body's nested
        // bound calls read, and the entering call sites stage the words.
        // A bare `[t]` parameter bounds nothing, so its bodies demand
        // nothing.
        let bounds = Rc::new(fn_bounds(def, analyzed));
        let pairs = plan.demand(&BodyId::Free(def));
        plan.register_generic(GenericFn {
            name: name.name.clone(),
            def,
            lambda: local.init.clone(),
            pairs,
            bounds,
        });
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
        let name = mangled_name(
            &only.name,
            &render_type(&imp.target),
            &f.name.name,
            imp.name.as_ref().map(|name| name.name.as_str()),
        );
        // A generic impl's member bodies hold no table entry of their
        // own: every entering site plans the body it needs (an instance
        // over the receiver's solved shape, or the ONE Raw-word body a
        // site still naming a variable enters), and a member any body's
        // runtime evidence demands plans its ONE body here — the
        // demand's chain names the serving body under some instance of
        // this trait, and the site baking the chain may sit in any item.
        if !imp.generics.is_empty() {
            plan.register_generic_member(func::GenericMember {
                name,
                impl_site: item.span,
                member: f.name.name.clone(),
                generics: imp
                    .generics
                    .iter()
                    .map(|param| param.name.name.clone())
                    .collect(),
                clause: f
                    .generics
                    .iter()
                    .map(|param| param.name.name.clone())
                    .collect(),
                params: Rc::new(f.params.clone()),
                body: body_expr,
                decl_span: f.span,
                pairs: evidence.pairs,
                bounds: evidence.bounds,
                default_of: None,
            });
            if plan.member_is_demanded(&only.name, &f.name.name, analyzed) {
                plan.member_one_body_named(analyzed, item.span, &f.name.name);
            }
            continue;
        }
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

/// The mangled table name of one impl member body:
/// `<trait>#<target>#<member>`, plus `#name` for a named impl's
/// vocabulary — `#` sits outside the identifier charset, so no
/// user-declared name collides.
fn mangled_name(trait_name: &str, target: &str, member: &str, impl_name: Option<&str>) -> String {
    let mut name = format!("{trait_name}#{target}#{member}");
    if let Some(impl_name) = impl_name {
        name.push('#');
        name.push_str(impl_name);
    }
    name
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
        // A generic impl's member bodies hold no table entry of their
        // own, exactly the trait form's: every entering site plans the
        // body it needs (an instance over the receiver's solved shape,
        // or the ONE Raw-word body a site still naming a variable
        // enters). An inherent member never serves a runtime-evidence
        // demand (a demand names a trait member alone), so no
        // declaration-side ONE body plans here.
        if !imp.generics.is_empty() {
            plan.register_generic_member(func::GenericMember {
                name,
                impl_site: item.span,
                member: f.name.name.clone(),
                generics: imp
                    .generics
                    .iter()
                    .map(|param| param.name.name.clone())
                    .collect(),
                clause: f
                    .generics
                    .iter()
                    .map(|param| param.name.name.clone())
                    .collect(),
                params: Rc::new(f.params.clone()),
                body: body_expr,
                decl_span: f.span,
                pairs,
                bounds: bounds.clone(),
                default_of: None,
            });
            continue;
        }
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
        let index = plan.push_default_body(
            name.clone(),
            def,
            f.name.name.clone(),
            Rc::new(f.params.clone()),
            Rc::clone(&body_expr),
            Some(ret),
            EvidenceEnv {
                pairs: Rc::clone(&pairs),
                bounds: bounds.clone(),
            },
        );
        // the default body runs the members' per-shape story over the
        // trait's `Self` variable: the dispatch site's receiver shape
        // instantiates it, the trait's own span keying the shared
        // instance tables, and the ONE Raw-word body this planning
        // pushed serving the sites a variable-written receiver enters
        plan.register_generic_default(
            def,
            t.name.span,
            func::GenericMember {
                name,
                impl_site: t.name.span,
                member: f.name.name.clone(),
                generics: std::iter::once("Self".to_owned())
                    .chain(
                        analyzed
                            .decls
                            .trait_generics
                            .get(&def)
                            .map(|names| names.iter().cloned())
                            .into_iter()
                            .flatten(),
                    )
                    .collect(),
                clause: Vec::new(),
                params: Rc::new(f.params.clone()),
                body: body_expr,
                decl_span: f.span,
                pairs,
                bounds: bounds.clone(),
                default_of: Some(def),
            },
            index,
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
fn collect_demands<T>(asts: &[T], analyzed: &Analyzed) -> func::Demands
where
    T: std::borrow::Borrow<Ast>,
{
    struct Body<'a> {
        id: BodyId,
        bounds: Rc<HashMap<String, HashSet<TraitId>>>,
        expr: Rc<Expr>,
        res: &'a ds_check::Resolutions,
    }
    let mut bodies: Vec<Body> = Vec::new();
    for ast in asts.iter().map(std::borrow::Borrow::<Ast>::borrow) {
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
                        // The body's def reads the value namespace, the
                        // plan's own registration reading: a generic fn
                        // sharing an ambient tag's label keeps its bounds
                        // under its own def.
                        let Some(def) = analyzed.package.lookup_value(&name.name) else {
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

/// The module's pc-to-span section: the served table (the spliced
/// prefix's rows, installed verbatim) plus, per freshly lowered
/// function, its region starts resolved against the bundle's span facts.
/// Adjacent regions naming the same position merge, so one expression's
/// ops share one region, and the rows stay ascending by function index —
/// the writer's canonical order, byte-identical between the served and
/// fresh lowerings of the same package.
fn span_table(
    analyzed: &Analyzed,
    served: Option<ds_mod::SpanTable>,
    spliced: usize,
    marks: &[Vec<(u32, Span)>],
) -> ds_mod::SpanTable {
    let mut table = served.unwrap_or_default();
    // the belt: the served table names the spliced prefix only, so a
    // table carrying rows past the splice drops them here — the fresh
    // rows own every function the walk lowered
    let spliced = u32::try_from(spliced).expect("function index fits u32");
    table.funcs.retain(|row| row.func < spliced);
    for (index, fn_marks) in marks.iter().enumerate() {
        if index < spliced as usize {
            continue;
        }
        let func = u32::try_from(index).expect("function index fits u32");
        let mut regions = Vec::with_capacity(fn_marks.len());
        for (pc, span) in fn_marks {
            let facts = &analyzed.span_files[span.file.as_u32() as usize];
            let at = facts.location_clamped(span.start);
            let name = facts.name();
            let file = if let Some(at) = table.files.iter().position(|known| known == name) {
                at
            } else {
                table.files.push(name.to_owned());
                table.files.len() - 1
            };
            let at = ds_mod::SpanLocation {
                file: u16::try_from(file).expect("the file table fits u16"),
                line: at.line,
                col: at.col,
            };
            if regions
                .last()
                .is_some_and(|last: &ds_mod::SpanRegion| last.at == at)
            {
                continue;
            }
            regions.push(ds_mod::SpanRegion { pc: *pc, at });
        }
        if !regions.is_empty() {
            table.funcs.push(ds_mod::FuncSpans { func, regions });
        }
    }
    table
}

/// What lowering every planned function yields: the module's functions,
/// each function's placed op sequence, and the shared shape tables.
type LowerAll<'b> = (
    Vec<ds_mod::Function>,
    Vec<Vec<PlacedOp>>,
    Vec<FnFacts>,
    Shapes<'b>,
    usize,
    Vec<Vec<(u32, Span)>>,
);

/// The platform name the world natives' pool FuncRefs address:
/// the package's `platform "..."` declaration's name, the default
/// platform `std` when no declaration names one — the host must install
/// a platform under this name or the module's install refuses the world
/// call sites. The comptime evaluator serves the same name with an
/// empty platform, which is the fence.
#[must_use]
pub fn platform_name<T>(asts: &[T]) -> String
where
    T: std::borrow::Borrow<Ast>,
{
    for ast in asts.iter().map(std::borrow::Borrow::<Ast>::borrow) {
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
    types: &'b types::Types<'b>,
    lin: &ds_check::Linearity,
    platform: &str,
    served: Option<Served>,
    comptime: func::ComptimeRun<'b>,
    mode: LowerMode,
) -> Result<LowerAll<'b>, LowerError> {
    let mut shapes = Shapes::new(analyzed, types, plan.declared_deinits());
    let mut functions = Vec::with_capacity(plan.functions().len());
    let mut placed = Vec::with_capacity(plan.functions().len());
    let mut facts = Vec::with_capacity(plan.functions().len());
    let mut marks = Vec::with_capacity(plan.functions().len());
    let mut spliced = 0;
    // The served prefix installs verbatim (names already validated
    // against the plan): its bodies carry their own placed streams as
    // code words, so their placed rows here stay empty and the walk
    // starts at the splice's end. Its tag rows seed the mint table
    // first — the spliced bodies reference those ids as immediates, so
    // every body lowered here must mint after them. The linked
    // modules' rows seed beside them (the shared tag mapping): a
    // union-typed word crossing a module boundary carries the producer's
    // ids as immediates, so this module's own table reserves them before
    // any body mints.
    if let Some(served) = served {
        shapes.seed_served(&served.tags)?;
        for body in served.functions {
            functions.push(body);
            placed.push(Vec::new());
            facts.push(FnFacts::default());
            marks.push(Vec::new());
        }
        spliced = placed.len();
    }
    shapes.seed_linked(analyzed.package.linked_tag_rows())?;
    // the placement pass reads the checker's affine rows through one
    // shared sorted table: every function's walk queries it, so the
    // lowering never copies the package's rows per function
    let affine = Rc::new(place::AffineFacts::from_linearity(lin));
    let mut index = functions.len();
    while let Some(planned) = plan.function(index).cloned() {
        let res = &analyzed.resolutions[planned.lambda.span().file.as_u32() as usize];
        // this entry lowers under its own instantiation's substitution
        // (a free generic fn's instance, or the enclosing instance a
        // lambda or spawned child was minted inside); every other
        // entry lowers over the analyzed registry as read
        shapes.enter(planned.subst.clone());
        // Any sentinel-spanned refusal that reached here without a
        // node-observing re-point lands on the function being lowered:
        // the sentinel renders against the first file's first line,
        // which is the stdlib ambient in the CLI pipeline.
        let (function, ops, fact, fn_marks) = FnLower::lower(
            analyzed,
            res,
            plan,
            &mut shapes,
            &affine,
            &planned,
            u32::try_from(index).expect("function index fits u32"),
            func::RunContext {
                platform: platform.to_owned(),
                comptime,
                mode,
            },
        )
        .map_err(|err| err.respanned(planned.lambda.span()))?;
        functions.push(function);
        placed.push(ops);
        facts.push(fact);
        marks.push(fn_marks);
        index += 1;
    }
    Ok((functions, placed, facts, shapes, spliced, marks))
}
