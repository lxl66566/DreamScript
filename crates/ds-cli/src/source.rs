//! The source pipeline behind `ds run <file.ds>` and `ds build`: one .ds
//! file through parse, the static story ([`ds_check::analyze`], the stdlib
//! ambient registered first) with its affine walk gate, the platform check
//! ([`ds_check::check_platform`]), and lowering ([`ds_lower::lower`]) to a
//! ds-mod module — stopping where each subcommand needs it. The module
//! carries the source's blake3 hash, so [`ds_mod::read_checked`] verifies
//! what this pipeline records.
//!
//! Lowering serves a subset: monomorphic const-arrow
//! functions, rebindable `var` locals with assignment and `while` loops,
//! UFCS dot-calls onto resolved free functions, the covered builtin
//! natives, early exits, interpolation, and tail positions. Constructs
//! outside it fail with a structured diagnostic naming the construct and
//! the seam it needs.
//!
//! Package mode runs the same pipeline with the caller's framing: a
//! resolved package's member (or a package-shaped root) names its module
//! directly, and the resolved graph's dep identities ride in, so the
//! dep-list writer records real content for the names the source text
//! alone cannot determine.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
    sync::LazyLock,
};

use anyhow::{Context, Result, anyhow};
use ds_check::{Analyzed, DeclaredRows, Effect, EffectRow};
use ds_diag::{Diagnostic, ErrorCode, Files, render};
use ds_dsi::ServedPrefix;
use ds_lower::{ComptimeValues, LowerError, Lowered, lower_with_comptime, lower_with_served};
use ds_mod::Hash128;
use ds_parse::{ItemKind, parse, parse_script};

/// The reserved native module stems: a source file with either stem
/// would shadow pool FuncRefs the lowered bodies emit (the VM's `builtin`
/// core and the default `std` platform handler's module name).
const RESERVED_MODULES: [&str; 2] = ["builtin", "std"];

/// The host provision's member file: the CLI-owned declaration member an
/// entry session registers beside the ambient — the convenience spelling
/// is host code, never ambient or `@std` source).
/// The `$` stem keeps it outside every plausible user stem.
pub(crate) const HOST_FILE: &str = "$host.ds";

/// The host provision's text: the `die` spelling's declaration, `pub`
/// like every ambient face because the member's name is the
/// host's surface to the entry files, not a private one. `die` routes to
/// the builtin core's bare trap primitive — what
/// it prints and which exit code follows is this host's own policy,
/// which is why it stays here while the policy-free `panic` and the
/// deliberate-status `exit` are ambient declarations; an un-provisioned
/// compile keeps the name unknown, the die split's discipline.
pub(crate) const HOST_SOURCE: &str = "pub extern fn die[t](msg: string) -> t\n";

/// The entry session's declared rows: the ambient's table plus the die
/// spelling's trap row — the host's own declaration, so the row rides
/// with it (a declared row is a lower bound; trap alone never fences or
/// refuses the pooled bodies).
static SESSION_ROWS: LazyLock<DeclaredRows> = LazyLock::new(|| {
    ds_stdlib::declared_rows()
        .clone()
        .with("die", EffectRow::of(Effect::Trap))
});

/// The entry session's rows when the ambient serves: the host member's
/// rows beside the ambient's table.
pub(crate) fn session_rows() -> &'static DeclaredRows {
    &SESSION_ROWS
}

/// How many ambient files a package compile under `link_name` registers
/// ahead of its members: one unless the ambient is off — or the package
/// is the reserved `@std` itself, whose member IS the ambient source
/// seeded verbatim, so registering the ambient beside it would duplicate
/// every declaration. The one rule the compile and the surface builder
/// share: the rebase map a consumer builds reads the linked package's
/// own file space through it.
pub(crate) fn package_ambient_files(no_ambient: bool, link_name: &str) -> usize {
    usize::from(!no_ambient && link_name != ds_stdlib::STD_PACKAGE_NAME)
}

/// Extracts every member file's exported top-level definitions:
/// one [`Export`] per package def whose defining span names a member
/// source — the module's own ambient excluded — carrying the facts an
/// import surface serves (the written signature of a callable, the
/// solved row, the function-table index a cross-module call addresses,
/// the value a `const` binding carries as its own link fact — the
/// literal a plain initializer is, the dep-side evaluation's answer a
/// computed one — and the word layout a struct, enum, or enum-case
/// declaration carries as its own). A `const` binding whose value
/// is a function exports as callable when its signature registered;
/// a data const exports as a binding, its fact its evaluated value. A
/// `pub import` row's bound selections export too
/// — the transitive surface closure — under the names the rows bound,
/// each carrying its origin package's link name (the position fact the
/// entry rides translates through this module's own dep list) so a
/// dependent's FuncRef addresses the origin, never this package's
/// table; a private import's bound defs stay excluded exactly as
/// before.
#[must_use]
fn exports_of(
    analyzed: &Analyzed,
    ambient_files: usize,
    effects: &ds_check::Effects,
    functions: &[String],
    carried: &HashMap<String, u32>,
    deps: &[ds_mod::Dep],
    const_values: &HashMap<ds_check::DefId, ds_check::ConstFact>,
) -> Vec<Export> {
    let caps_of = |def: ds_check::DefId| {
        effects
            .defs()
            .iter()
            .find(|entry| entry.def == def)
            .map_or(0, |entry| entry.caps)
    };
    let mut exports = Vec::new();
    // The `pub import` rows' bindings, keyed by def: the bound names and
    // row spans the transitive exports below carry (a def two rows alias
    // rides twice, one export per name).
    let mut reexports: HashMap<ds_check::DefId, Vec<(&str, ds_diag::Span)>> = HashMap::new();
    for (def, name, span) in analyzed.package.reexports() {
        reexports
            .entry(*def)
            .or_default()
            .push((name.as_str(), *span));
    }
    // The type declarations' link facts as one co-derivation fixpoint:
    // a field may name another declared type of the same member, so the
    // whole member's facts derive together before any export row
    // carries one.
    let facts = ds_check::TypeFact::member_facts(analyzed, ambient_files, carried);
    for (index, def) in analyzed.package.defs().enumerate() {
        if (def.span.file.as_u32() as usize) < ambient_files {
            continue; // the ambient's own definitions never export
        }
        let id = ds_check::DefId::from_raw(u32::try_from(index).expect("def count fits u32"));
        if let Some(entry) = analyzed.package.foreign().get(&id) {
            // an import-bound definition is not this package's own: only
            // a `pub import` row's binding re-exports (the transitive
            // closure), the row's own span the definition site a
            // dependent's diagnostics name — the origin's declaring span
            // lives in the origin package's file space and never rebases
            // into a dependent's registry. The origin's link name rides
            // from the entry's position fact through this module's own
            // dep list, so a dependent's FuncRef addresses the package
            // whose table holds the body.
            for (name, span) in reexports.get(&id).into_iter().flatten() {
                let origin = deps
                    .get(usize::try_from(entry.link).expect("link position fits usize"))
                    .map(|dep| dep.name.clone());
                exports.push(Export {
                    name: (*name).to_owned(),
                    vis: entry.vis,
                    kind: match entry.kind {
                        ds_check::ForeignKind::Fn => ExportKind::Fn,
                        ds_check::ForeignKind::Binding => ExportKind::Binding,
                        ds_check::ForeignKind::Type => ExportKind::Type,
                    },
                    generic: entry.generic,
                    defaulted: entry.defaulted,
                    decl_span: *span,
                    sig: entry.sig.clone(),
                    value: entry.value.clone(),
                    ty: entry.ty.clone(),
                    func: entry.func,
                    origin,
                    row: entry.row,
                    caps: entry.caps,
                });
            }
            continue;
        }
        let sig = analyzed.decls.fns.get(&id).cloned();
        let kind = match (def.kind, sig.is_some()) {
            (ds_check::DefKind::Const, true) | (ds_check::DefKind::Fn, _) => ExportKind::Fn,
            (ds_check::DefKind::Const, false) | (ds_check::DefKind::Var, _) => ExportKind::Binding,
            _ => ExportKind::Type,
        };
        let (generic, defaulted, func) = match &sig {
            Some(sig) => (
                !sig.generics.is_empty()
                    || sig
                        .params
                        .iter()
                        .any(|param| matches!(param.mode, ds_parse::ParamMode::Comptime(_))),
                sig.params.iter().any(|param| param.default.is_some()),
                functions.iter().position(|name| *name == def.name),
            ),
            None => (false, false, None),
        };
        // A data const's per-target link fact is its value: a plain
        // literal initializer carries the literal itself, and a computed
        // one carries the value the dep-side evaluation answered on this
        // compile — both embed at a dependent's use sites the same way.
        let value = if kind == ExportKind::Binding {
            analyzed
                .decls
                .consts
                .get(&id)
                .filter(|local| !local.mutable)
                .and_then(|local| {
                    ds_check::ConstFact::of_expr(&local.init)
                        .or_else(|| const_values.get(&id).cloned())
                })
        } else {
            None
        };
        // A type declaration's per-target link fact is its word layout
        // over the surface's vocabulary — the fixpoint computed ahead of
        // the walk (the deinit gate, the generic heads, and every field
        // shape included); every type-namespace shape it refuses stays
        // refused downstream.
        let ty = facts.get(&id).cloned();
        exports.push(Export {
            name: def.name.clone(),
            vis: def.vis,
            kind,
            generic,
            defaulted,
            decl_span: def.span,
            sig,
            value,
            ty,
            func: func.map(|func| u32::try_from(func).expect("function index fits u32")),
            origin: None,
            row: effects.of(id).unwrap_or_default(),
            caps: caps_of(id),
        });
    }
    exports
}

/// The member files' trait-impl link rows: one per plain trait
/// impl whose trait crossed as a fact, the head and member bodies
/// addressed over the same word vocabulary and module table the export
/// rows serve. Everything the walk refuses (a generic or conditional
/// impl, an unplanned member) simply rides no row, exactly like a type
/// shape the vocabulary cannot spell.
#[must_use]
fn impl_facts_of(
    analyzed: &Analyzed,
    ambient_files: usize,
    functions: &[String],
    carried: &HashMap<String, u32>,
    exercised: &HashSet<u32>,
) -> Vec<ds_check::import::ImplFact> {
    let facts = ds_check::TypeFact::member_facts(analyzed, ambient_files, carried);
    let eligible: HashSet<ds_check::DefId> = facts.keys().copied().collect();
    ds_check::import::ImplFact::member_facts(
        analyzed,
        ambient_files,
        functions,
        &eligible,
        &facts,
        exercised,
    )
}

/// The member files' inherent-impl link rows (the type-keyed receiver
/// seam): one per plain inherent impl over an exported type of this
/// package, the owner's name and each member body's index addressed
/// over the same module table the export rows serve. Everything the
/// walk refuses (a generic impl, a generic or defaulted member, an
/// unplanned body, an owner whose structural fact stayed home) simply
/// rides no row, exactly like a type shape the vocabulary cannot
/// spell.
#[must_use]
fn inherent_facts_of(
    analyzed: &Analyzed,
    ambient_files: usize,
    functions: &[String],
    carried: &HashMap<String, u32>,
) -> Vec<ds_check::import::InherentFact> {
    let facts = ds_check::TypeFact::member_facts(analyzed, ambient_files, carried);
    ds_check::import::InherentFact::member_facts(analyzed, ambient_files, functions, &facts)
}

/// The producer's default-body gate: an exported trait member
/// whose declared default body cannot cross — the body never planned,
/// stages runtime evidence, or carries an effect row — refuses this
/// compile here, naming the member at its own declaration, exactly
/// like the dep-side const fence refuses a reach it cannot answer. A
/// shape the surface cannot honestly serve fails before any consumer
/// sees it; `Ok` when every exported default body carries.
fn default_body_gate(
    analyzed: &Analyzed,
    ambient_files: usize,
    effects: &ds_check::Effects,
    functions: &[String],
    carried: &HashMap<String, u32>,
) -> Result<(), Vec<Diagnostic>> {
    let diags = ds_check::import::refused_default_bodies(
        analyzed,
        ambient_files,
        effects,
        functions,
        carried,
    )
    .into_iter()
    .map(|refusal| {
        Diagnostic::error(
            ErrorCode::LinkedDefaultBodyRefusal,
            format!(
                "trait `{}`'s default body for `{}` cannot cross packages this round: {}",
                refusal.trait_name, refusal.member, refusal.seam
            ),
        )
        .with_primary(refusal.span, "the default body refuses the link walk")
    })
    .collect::<Vec<_>>();
    if diags.is_empty() { Ok(()) } else { Err(diags) }
}

/// Rebases one signature's span set across file registries: every
/// span and written type names the consuming compile's own registry, so
/// the diagnostics a link pass mints render there. A signature whose
/// file ids all map is untouched in shape.
#[must_use]
pub(crate) fn rebase_sig(
    sig: &ds_check::FnSig,
    map: &HashMap<ds_diag::FileId, ds_diag::FileId>,
) -> std::sync::Arc<ds_check::FnSig> {
    let mut sig = ds_check::FnSig {
        generics: sig.generics.clone(),
        params: sig.params.clone(),
        ret: sig.ret.clone(),
        bind: sig.bind.clone(),
    };
    for generic in &mut sig.generics {
        generic.span = rebase_span(generic.span, map);
        rebase_type(&mut generic.bound, map);
    }
    for param in &mut sig.params {
        param.span = rebase_span(param.span, map);
        rebase_type(&mut param.ty, map);
    }
    rebase_type(&mut sig.ret, map);
    std::sync::Arc::new(sig)
}

/// Rebases one written type's spans through the registry map.
fn rebase_type(ty: &mut Option<ds_parse::Type>, map: &HashMap<ds_diag::FileId, ds_diag::FileId>) {
    let Some(ty) = ty else { return };
    rebase_type_node(ty, map);
}

/// Rebases one type node's spans, recursively.
fn rebase_type_node(ty: &mut ds_parse::Type, map: &HashMap<ds_diag::FileId, ds_diag::FileId>) {
    use ds_parse::Type;
    match ty {
        Type::Path { span, args, .. } => {
            *span = rebase_span(*span, map);
            for arg in args {
                rebase_type_node(arg, map);
            }
        },
        Type::Optional { span, inner }
        | Type::Negation { span, inner }
        | Type::Intersect {
            span, left: inner, ..
        }
        | Type::Difference {
            span, left: inner, ..
        }
        | Type::Paren { span, inner }
        | Type::Ptr { span, inner, .. } => {
            *span = rebase_span(*span, map);
            rebase_type_node(inner, map);
        },
        Type::Union { span, left, right } => {
            *span = rebase_span(*span, map);
            rebase_type_node(left, map);
            rebase_type_node(right, map);
        },
        Type::Tuple { span, items } => {
            *span = rebase_span(*span, map);
            for item in items {
                rebase_type_node(item, map);
            }
        },
        Type::Unit(span) | Type::TypeKw(span) | Type::Foreign { span, .. } => {
            *span = rebase_span(*span, map);
        },
        Type::Fn { span, params, ret } => {
            *span = rebase_span(*span, map);
            for param in params {
                param.name.span = rebase_span(param.name.span, map);
                rebase_type(&mut param.ty, map);
            }
            if let Some(ret) = ret {
                rebase_type_node(ret, map);
            }
        },
    }
}

/// Maps one span's file id through the registry map; a span whose file
/// does not map keeps its id (a sig fact the surface never reads back).
pub(crate) fn rebase_span(
    span: ds_diag::Span,
    map: &HashMap<ds_diag::FileId, ds_diag::FileId>,
) -> ds_diag::Span {
    match map.get(&span.file) {
        Some(file) => ds_diag::Span::new(*file, span.start, span.end),
        None => span,
    }
}

/// One compiled source: the lowered module plus what the run entry path
/// needs — the module's name and the export surface a dependent's
/// imports bind through.
pub struct CompiledSource {
    /// The lowered module, its name derived from the file's stem, with
    /// the tag table recorded.
    pub lowered: Lowered,
    /// The module name.
    pub name: String,
    /// Every member file's exported top-level definitions, in
    /// declaration order: the surface a dependent's import selections
    /// validate against, carrying the written signatures and effect rows
    /// the dependent's own checks read. Empty for a script compile's
    /// own view of itself.
    pub exports: Vec<Export>,
    /// The member files' declared trait impls: the owner-keyed
    /// member link rows a dependent's trait call sites dispatch
    /// through, each member body's index addressing this module's own
    /// table. Empty for a script compile's own view of itself.
    pub impls: Vec<ds_check::import::ImplFact>,
    /// The member files' declared inherent impls (the type-keyed
    /// receiver seam): the member link rows a dependent's static and
    /// value call sites dispatch through, each member body's index
    /// addressing this module's own table. Empty for a script compile's
    /// own view of itself.
    pub inherent: Vec<ds_check::import::InherentFact>,
}

/// One member file's exported definition: the import surface's facts,
/// in this compile's own file space — a dependent rebases the
/// spans into its own registry when it builds its import surfaces.
#[derive(Clone, Debug)]
pub struct Export {
    /// The declared name.
    pub name: String,
    /// The declared tier.
    pub vis: ds_check::Vis,
    /// What the declaration defines.
    pub kind: ExportKind,
    /// A generic declaration (generic parameters or a comptime
    /// parameter).
    pub generic: bool,
    /// A defaulted parameter.
    pub defaulted: bool,
    /// The defining name occurrence, this compile's file space.
    pub decl_span: ds_diag::Span,
    /// The written signature: callable definitions only.
    pub sig: Option<std::sync::Arc<ds_check::FnSig>>,
    /// The literal fact a non-callable `const` carries: the value itself,
    /// which a dependent's use sites embed. Computed initializers and
    /// `var` bindings carry none.
    pub value: Option<ds_check::ConstFact>,
    /// The structural fact a type declaration carries: the word layout
    /// a dependent's link walk mints its own def from — a struct's
    /// fields, an enum's tag rows, a case's own row. Every other
    /// type-namespace shape carries none.
    pub ty: Option<std::sync::Arc<ds_check::TypeFact>>,
    /// The function's index in this module's table, callable definitions
    /// only. A re-exported entry's index names the ORIGIN module's table.
    pub func: Option<u32>,
    /// The origin package's link name when this export re-exports
    /// transitively (a `pub import` row's binding): a dependent's dep
    /// list records the name and the entry's FuncRef addresses the
    /// origin's position, so the body's own module serves the call.
    /// `None` for the package's own declarations.
    pub origin: Option<String>,
    /// The definition's solved effect row.
    pub row: EffectRow,
    /// The definition's world-capability bits.
    pub caps: u8,
}

/// What one exported definition declares.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExportKind {
    /// A function (a `fn` or a callable const-arrow binding).
    Fn,
    /// A data binding.
    Binding,
    /// A type-namespace declaration.
    Type,
}

/// What happens to the compile bundle — the analyzer's checked tables
/// and trees — once the module lowers. The bundle's teardown is
/// dominated by the ambient's tables and trees, so the one-compile-per-
/// process subcommands (`ds run`, `ds build`) keep it: the memory stays
/// resident until process exit and the teardown never runs. A host that
/// compiles repeatedly in one process (the package graph's member
/// compiles, the test suites) drops it, so the memory returns per
/// compile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BundleAfter {
    /// The bundle drops with the compile.
    Drop,
    /// The bundle stays until process exit reclaims it.
    Keep,
}

/// Parses, checks, and lowers one source file. Diagnostics render to
/// stderr as they do under `ds check`; the returned error summarizes the
/// report (the subcommand exits 1 either way). The stdlib ambient, when
/// registered, serves from the checked-ambient artifacts: its
/// pre-expanded tree from the AST blob, its resolve half and declaration
/// prefix from the `.dsi` tables, and its lowered impl bodies from the
/// ambient dsmod (spliced in front of the user's own function table),
/// every artifact miss falling back to the fresh compute.
///
/// # Errors
///
/// File reads, syntax and checker diagnostics, and lowering failures
/// (each named and located).
pub fn compile(
    path: &Path,
    no_ambient: bool,
    prefix_cache: Option<&crate::dsi_cache::PrefixCache>,
    after: BundleAfter,
) -> Result<CompiledSource> {
    let source =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let inputs = Inputs {
        name: script_module_name(path)?,
        display: path.display().to_string(),
        main_required: true,
        identity: Hash128::compute(source.as_bytes()),
        dep_content: &NO_DEPS,
    };
    compile_text(
        &inputs,
        &[(inputs.display.clone(), source)],
        true,
        no_ambient,
        true,
        prefix_cache,
        None,
        None,
        after,
    )
}

/// The dep identities a plain script supplies: none — every name but the
/// ambient records the zero marker, the link the load walk's validation
/// refuses until package resolution supplies the content.
static NO_DEPS: BTreeMap<String, Hash128> = BTreeMap::new();

/// One compile's framing: the module name and the path diagnostics render
/// (a script's file stem and path, a package module's link name and
/// member path), the dep identities package resolution supplies for the
/// writer, and whether the run contract's `main` entry is required (a
/// script or package root requires one; a package member serves its
/// members and waives the check).
struct Inputs<'a> {
    name: String,
    display: String,
    main_required: bool,
    /// The module's content identity: a script's or single-member
    /// package's source hash, a multi-member package's composed fold.
    /// Every cache tier keys on it — the dsmod artifact's header, the
    /// persisted `.dsi` prefix — so a member edit re-keys each one.
    identity: Hash128,
    dep_content: &'a BTreeMap<String, Hash128>,
}

/// Compiles one package module over its member sources (one
/// namespace per package): the members share one compile unit after the
/// ambient, the module serves under `name` at the caller-supplied
/// `identity` (a single member's source hash, a composed fold over
/// every member otherwise), and its own recorded imports validate
/// against the same graph through the caller's `dep_content` and bind
/// through `graph`. `script` parses the first member as a script — its
/// top-level statements fold into the synthesized `main`, the manifest-
/// free run path's rule over the implicit `@std` graph — while `false`
/// keeps the member grammar a package root or linked member parses
/// under. The run contract's `main` entry check is waived
/// when `main_required` is false — an installed member serves its
/// members and never runs as an entry. `dep_dsi` is the linked-member
/// `.dsi` tier: a compile the script-mode run path serves as a dep
/// caches persists its own checked prefix beside its dsmod.
///
/// # Errors
///
/// A reserved or empty module name, and every pipeline failure
/// [`compile`] reports.
#[allow(clippy::fn_params_excessive_bools, clippy::too_many_arguments)]
pub fn compile_linked(
    name: &str,
    members: &[(String, String)],
    identity: Hash128,
    script: bool,
    main_required: bool,
    no_ambient: bool,
    dep_content: &BTreeMap<String, Hash128>,
    graph: Option<&crate::package::Graph>,
    prefix_cache: Option<&crate::dsi_cache::PrefixCache>,
    dep_dsi: Option<&crate::run_cache::RunCache>,
    after: BundleAfter,
) -> Result<CompiledSource> {
    if name.is_empty() {
        return Err(anyhow!("the package module has no name"));
    }
    if RESERVED_MODULES.contains(&name) {
        return Err(anyhow!(
            "`{name}` is a reserved native module name (`builtin`, `std`); the package cannot \
             serve under it"
        ));
    }
    let inputs = Inputs {
        name: name.to_owned(),
        display: members
            .first()
            .map_or_else(String::new, |(display, _)| display.clone()),
        main_required,
        identity,
        dep_content,
    };
    compile_text(
        &inputs,
        members,
        script,
        no_ambient,
        true,
        prefix_cache,
        dep_dsi,
        graph,
        after,
    )
}

/// The compile pipeline over source text (`compile` after its read):
/// `allow_serve` switches the ambient dsmod splice off, the fresh
/// lowering baseline the artifact gates compare against.
#[allow(clippy::fn_params_excessive_bools, clippy::too_many_arguments)]
fn compile_text(
    inputs: &Inputs<'_>,
    sources: &[(String, String)],
    script: bool,
    no_ambient: bool,
    allow_serve: bool,
    prefix_cache: Option<&crate::dsi_cache::PrefixCache>,
    dep_dsi: Option<&crate::run_cache::RunCache>,
    graph: Option<&crate::package::Graph>,
    after: BundleAfter,
) -> Result<CompiledSource> {
    let mut files = Files::new();
    // The trees move into the analyzer, one pre-expansion mark per tree:
    // the blob-served ambient's is expanded at artifact build time, every
    // other tree expands at run time.
    let mut asts = Vec::new();
    let mut preexpanded = Vec::new();
    // The registration the prefix pins: the ambient first, the sources
    // after it, in order. The reserved `@std` package's own member
    // compile registers none — the package carries the ambient core as
    // its first member, so the ambient beside it would duplicate every
    // face. The ambient's shape follows the session before the graph: a
    // package compile whose graph resolved `@std` registers the shrunk
    // core (the migrated members bind through the linked surface, so a
    // selection's name stays free), while an entry script session — the
    // manifest-free run over the implicit `@std` graph — composes the
    // @std bodied members back beside the core (the run path's implicit
    // resolution: the bare load-bearing migrated names resolve and
    // lower, the data-shaped members staying bound through the link),
    // and every undeclaring registration serves the merged source as
    // ever — the un-merge's reading switch, the undeclaring side
    // byte-stable.
    let mut ids = Vec::new();
    let std_pkg = inputs.name == ds_stdlib::STD_PACKAGE_NAME;
    let std_declared = graph.is_some_and(crate::package::Graph::declares_std);
    let ambient_source = if !std_declared {
        ds_stdlib::AMBIENT_SOURCE
    } else if script {
        ds_stdlib::AMBIENT_SESSION_SOURCE
    } else {
        ds_stdlib::AMBIENT_CORE_SOURCE
    };
    let ambient_files = usize::from(!no_ambient && !std_pkg);
    // The die provision: an entry session carries
    // the host member as its last registered source — a plain member, so
    // the ambient band, the serve tiers' file counts, and the export
    // rules all see one more member file and nothing else moves. The
    // session is one the host provisions when the run contract's `main`
    // entry is required (a script or a package root, never an installed
    // member), the ambient registers (the merged reading the provision
    // rides), and the package is not `@std` itself (its member is the
    // ambient verbatim).
    let host_member = usize::from(inputs.main_required && !no_ambient && !std_pkg);
    if ambient_files > 0 {
        let id = files.add_source(ds_stdlib::AMBIENT_FILE, ambient_source);
        let (ast, expanded) = ambient_tree(&files, id, !std_declared);
        asts.push(ast);
        preexpanded.push(expanded);
        ids.push(id);
    }
    let mut members: Vec<(String, String)> = sources.to_vec();
    if host_member > 0 {
        members.push((HOST_FILE.to_owned(), HOST_SOURCE.to_owned()));
    }
    for (at, (display, text)) in members.iter().enumerate() {
        // The first source is the entry: script mode folds its top-level
        // statements into a synthesized `main` (the ambient stays
        // pure-declaration, like every member file).
        let id = files.add_source(display, text);
        asts.push(std::sync::Arc::new(if script && at == 0 {
            match parse_script(&files, id) {
                Ok(ast) => ast,
                Err(diags) => return Err(reject(&files, &diags, "the source has syntax errors")),
            }
        } else {
            match parse(&files, id) {
                Ok(ast) => ast,
                Err(diags) => return Err(reject(&files, &diags, "the source has syntax errors")),
            }
        }));
        preexpanded.push(false);
        ids.push(id);
    }
    let name = inputs.name.clone();
    // The import surfaces: a resolved graph's linked packages
    // validate and bind this compile's own imports (the ambient's merged
    // member served as `@std` when it registers). A graph absent keeps
    // every import declarative. The builder registers the selected
    // linked members' texts after the sources, so the surfaces' spans
    // name this compile's own registry.
    let imports = graph
        .map(|graph| graph.surfaces(&mut files, &asts, ambient_files, dep_dsi))
        .transpose()?;
    // Two serve tiers, each a pure cache with the fresh compute as the
    // canonical fallback: the user cache's composed prefix first — the
    // whole program's front-end half (the ambient member plus this
    // member), one artifact per source under `$DS_CACHE` — then the
    // ambient's own checked-in artifact — the ambient's half alone, the
    // user file collecting, resolving, and declaring fresh. Every miss
    // falls back to the tier below and finally to the fully fresh
    // analyze.
    let mut served = None;
    let mut served_rows = None;
    if let Some(cache) = prefix_cache {
        // the identity — a single source's hash, a multi-member package's
        // composed fold — keys the tier, so a sibling edit re-keys it
        served = cache.lookup(&name, inputs.identity, &ids);
    }
    // The ambient tier alone parks its affine walk: the prefix is one
    // file shared by every compile of the process, so the first walk
    // over it fills the park and later member compiles install it. A
    // composed user prefix serves a different (per-source) file set and
    // keeps the plain full walk. The `@std` package's own member compile
    // takes the fresh analyze — its member parsed and expanded here, not
    // the blob-served ambient tree the prefix assumes — and a core-shaped
    // registration (a graph that resolved `@std`) takes it too: the
    // checked-in `.dsi` carries the merged ambient's tables, a pure cache
    // whose miss falls to this fresh analyze.
    let mut ambient_served = false;
    if served.is_none() && ambient_files > 0 && !std_declared {
        served = ds_stdlib::ambient_dsi(ids[0]).ok();
        ambient_served = served.is_some();
    }
    let analyzed = match (served, &imports) {
        (Some(prefix), imports) => {
            let ServedPrefix {
                package,
                resolutions,
                decls,
                effect_defs,
                effect_methods,
            } = prefix;
            served_rows = Some((resolutions.len(), effect_defs, effect_methods));
            match imports {
                Some(imports) => ds_meta::analyze_served_linked(
                    &files,
                    asts,
                    &preexpanded,
                    package,
                    resolutions,
                    decls,
                    imports,
                ),
                None => {
                    ds_meta::analyze_served(&files, asts, &preexpanded, package, resolutions, decls)
                },
            }
        },
        (None, Some(imports)) => ds_meta::analyze_owned_linked(&files, asts, &preexpanded, imports),
        (None, None) => ds_meta::analyze_owned(&files, asts, &preexpanded),
    };
    let has_errors = analyzed
        .checked
        .diags
        .iter()
        .any(|diag| diag.severity == ds_diag::Severity::Error);
    if has_errors {
        let errors: Vec<Diagnostic> = analyzed
            .checked
            .diags
            .iter()
            .filter(|diag| diag.severity == ds_diag::Severity::Error)
            .cloned()
            .collect();
        return Err(reject(&files, &errors, "the source does not check clean"));
    }
    // A link-time warning reports and the compile stands: the shadowing
    // it names resolved by the link layering, never silently.
    for warning in analyzed
        .checked
        .diags
        .iter()
        .filter(|diag| diag.severity == ds_diag::Severity::Warning)
    {
        eprint!("{}", render::human(warning, &files));
    }
    // The affine walk: the same pass `ds check` and `ds repl`
    // consult before committing, refusing here before anything lowers
    // or runs. Lowering derives its move and deinit placement from this
    // walk's facts, which hold only over a clean one — a moved-then-used
    // value would lower code whose deinit fires twice. The ambient
    // serve's prefix parks after the first member compile walks it, so
    // later compiles walk only their own trees and read the parked rows.
    let linear = if ambient_served {
        let Some((served_files, ..)) = &served_rows else {
            unreachable!("an ambient serve records its file count")
        };
        ds_check::linearity_check_served(
            &analyzed.asts,
            &analyzed.package,
            &analyzed.resolutions,
            &analyzed.decls,
            &analyzed.checked,
            *served_files,
            ds_stdlib::ambient_linearity(),
        )
    } else {
        ds_check::linearity_check(
            &analyzed.asts,
            &analyzed.package,
            &analyzed.resolutions,
            &analyzed.decls,
            &analyzed.checked,
        )
    };
    if !linear.diags.is_empty() {
        return Err(reject(
            &files,
            &linear.diags,
            "the source fails the linearity check",
        ));
    }
    // Every later pass walks the analyzer's own trees: derive attributes
    // expanded into impl items there, so the original parse trees would
    // misalign with the resolutions and tables below.
    let asts = &analyzed.asts;
    // The platform check: the package's world usage verified inside
    // what its platform declaration provides. Without the ambient there
    // are no declared rows to attribute, but the declaration itself is
    // still validated — the `@std` package's own member compile the
    // exception: its source is the ambient's, so its declared rows apply
    // (they are name-keyed over exactly the externs it declares).
    let empty_rows = DeclaredRows::new();
    let empty_caps = ds_check::WorldCaps::new();
    let (rows, caps): (&DeclaredRows, &ds_check::WorldCaps) = if no_ambient && !std_pkg {
        (&empty_rows, &empty_caps)
    } else if host_member > 0 {
        (&SESSION_ROWS, ds_stdlib::world_caps())
    } else {
        (ds_stdlib::declared_rows(), ds_stdlib::world_caps())
    };
    // The rows come from the analyzer's own collect-and-resolve result,
    // never a second pass over the same trees; the served path seeds the
    // ambient's solved rows and walks only the user tree (the ambient's
    // subgraph cannot reach a user definition, so the answers agree).
    let effects = match served_rows {
        Some((served_files, effect_defs, effect_methods)) => {
            ds_check::effects::effects_of_resolved_served(
                &asts[served_files..],
                &analyzed.package,
                &analyzed.resolutions[served_files..],
                &analyzed.checked.hole_show_dispatches,
                &effect_defs,
                &effect_methods,
                rows,
                caps,
                ds_stdlib::ambient_effect_names(),
            )
        },
        None => ds_check::effects::effects_of_resolved(
            asts,
            &analyzed.package,
            &analyzed.resolutions,
            &analyzed.checked.hole_show_dispatches,
            rows,
            caps,
        ),
    };
    let platform = ds_check::check_platform(asts, &effects);
    if !platform.is_empty() {
        return Err(reject(
            &files,
            &platform,
            "the source fails the platform check",
        ));
    }
    // The pooled-body refusal: a fork-join combinator's element
    // body refuses the members a pooled drive cannot serve — the
    // sibling-addressing fiber control natives and the world surface —
    // unconditionally, so legality never depends on the host's pool
    // opt-in.
    let pooled = ds_check::check_pooled_bodies(&analyzed, rows);
    if !pooled.is_empty() {
        return Err(reject(
            &files,
            &pooled,
            "the source fails the pooled-body check",
        ));
    }
    let name = inputs.name.clone();
    let source_hash = inputs.identity;
    // The comptime evaluator: comptime calls run on the fenced VM
    // here, their answers embedding at the call sites below — a package
    // with no comptime call evaluates nothing.
    let comptime = ds_comptime::evaluate(&name, source_hash, asts, &analyzed, &effects)
        .map_err(|diags| reject(&files, &diags, "the source fails the comptime fence"))?;
    // The dep-side const evaluation: a linked member's exported
    // computed consts evaluate here, on the producer's own compile, so
    // their values ride the export surface as link facts — the run path
    // never pays for a compile nothing imports (a root or script's own
    // consts stay shape-refused downstream exactly as before).
    let const_values = if inputs.main_required {
        HashMap::new()
    } else {
        ds_comptime::evaluate_consts(&name, source_hash, asts, &analyzed, &effects, ambient_files)
            .map_err(|diags| reject(&files, &diags, "the exported consts refuse to evaluate"))?
    };
    // The ambient's impl bodies splice in from the checked-in dsmod when
    // it verifies against this build; every miss (identity, format,
    // name, source hash, splice boundary, plan disagreement) falls back
    // to lowering them here, whose answer is the canonical one. The
    // `@std` package's own member compile never splices: its items ARE
    // the ambient's, planned in this module's own table. A core-shaped
    // registration never splices either — the artifact is keyed on the
    // merged source, and the fresh lowering is the canonical fallback a
    // core-keyed splice artifact would later serve.
    let served = if !no_ambient && !std_pkg && allow_serve && !std_declared {
        crate::ambient_dsmod::serve()
    } else {
        None
    };
    let lowered = lower_via_serve(
        inputs.main_required,
        &name,
        source_hash,
        asts,
        &analyzed,
        served,
        &linear,
        &comptime,
    )
    .map_err(|error| lower_error(&files, &error))?;
    // The dep list rides the header: the artifact names the source's
    // imports, so a load walk over it resolves its deps without
    // re-deriving them from source.
    let deps = dep_list(&asts[ambient_files..], imports.as_ref(), inputs.dep_content);
    // A clean, fully lowered compile persists its own prefix for the
    // next run of the same source; every failure inside is silent (the
    // compile already succeeded, the next run falls back).
    if let Some(cache) = prefix_cache {
        cache.store(
            &name,
            inputs.identity,
            &ids,
            &analyzed,
            &effects,
            rows,
            caps,
        );
    }
    // A linked dep member's compile (the entry contract waived, so
    // never the root itself) persists its own checked prefix beside the
    // dsmod the walk's caller caches — the surface `ds info` and the LSP
    // hover serve — over the same graph fold the dsmod key applies;
    // every failure inside is silent the same way.
    if let (Some(cache), false) = (dep_dsi, inputs.main_required) {
        cache.store_dep_dsi(&name, source_hash, &analyzed, &effects, rows, caps);
    }
    if !inputs.main_required
        && let Err(diags) = default_body_gate(
            &analyzed,
            ambient_files,
            &effects,
            lowered.functions(),
            lowered.carried_default_bodies(),
        )
    {
        return Err(reject(
            &files,
            &diags,
            "the exported trait default bodies refuse the link walk",
        ));
    }
    let exports = exports_of(
        &analyzed,
        ambient_files,
        &effects,
        lowered.functions(),
        lowered.carried_default_bodies(),
        &deps,
        &const_values,
    );
    let impls = impl_facts_of(
        &analyzed,
        ambient_files,
        lowered.functions(),
        lowered.carried_default_bodies(),
        lowered.exercised(),
    );
    let inherent = inherent_facts_of(
        &analyzed,
        ambient_files,
        lowered.functions(),
        lowered.carried_default_bodies(),
    );
    match after {
        BundleAfter::Keep => {
            // The bundle's teardown is pure overhead on the one-shot
            // subcommands: nothing reads the tables again, and process
            // exit reclaims the memory, so the ambient-dominated drop
            // never runs.
            std::mem::forget(analyzed);
        },
        BundleAfter::Drop => {},
    }
    Ok(CompiledSource {
        lowered: lowered.with_deps(deps),
        name,
        exports,
        impls,
        inherent,
    })
}

/// The ambient member's tree and whether it arrived pre-expanded: the
/// embedded blob when it verifies against this registration (its trees
/// carry the derive expansion from artifact build time), a fresh parse
/// otherwise — the blob is a pure cache, never correctness-load-bearing.
/// The blob serves the merged ambient alone: a core-shaped registration
/// (a graph that resolved `@std`) parses fresh, since the artifact's tree
/// carries the migrated members' declarations back.
fn ambient_tree(
    files: &Files,
    id: ds_diag::FileId,
    merged: bool,
) -> (std::sync::Arc<ds_parse::Ast>, bool) {
    let served = merged
        .then(|| ds_stdlib::ambient_ast(id))
        .and_then(Result::ok);
    match served {
        Some(ast) => (ast, true),
        None => (
            std::sync::Arc::new(parse(files, id).expect("the ambient parses")),
            false,
        ),
    }
}

/// The lowering seam the ambient dsmod rides: the served bodies and
/// their tag rows splice over the planned ambient prefix, and any
/// failure of theirs — the splice disagreement included — falls back to
/// the fresh lowering, whose result (success or refusal) is the run's
/// canonical answer: the serve is a pure cache, never
/// correctness-load-bearing. Both paths embed the comptime evaluator's
/// answers at the call sites and read the affine walk the gate already
/// ran (`linear`), so the walk answers once per compile.
fn lower_via_serve<T>(
    main_required: bool,
    name: &str,
    source_hash: Hash128,
    asts: &[T],
    analyzed: &Analyzed,
    served: Option<ds_lower::Served>,
    linear: &ds_check::Linearity,
    comptime: &ComptimeValues,
) -> Result<Lowered, LowerError>
where
    T: std::borrow::Borrow<ds_parse::Ast>,
{
    if main_required {
        if let Some(served) = served
            && let Ok(lowered) = lower_with_served(
                name,
                source_hash,
                asts,
                analyzed,
                served,
                Some(linear),
                comptime,
            )
        {
            return Ok(lowered);
        }
        return lower_with_comptime(name, source_hash, asts, analyzed, Some(linear), comptime);
    }
    // the member shape: the library lowering, the same served splice
    if let Some(served) = served
        && let Ok(lowered) = ds_lower::lower_library(
            name,
            source_hash,
            asts,
            analyzed,
            Some(served),
            Some(linear),
            comptime,
        )
    {
        return Ok(lowered);
    }
    ds_lower::lower_library(
        name,
        source_hash,
        asts,
        analyzed,
        None,
        Some(linear),
        comptime,
    )
}

/// Renders every diagnostic to stderr and summarizes the report as one
/// error; the rendered diagnostics are the message.
fn reject(files: &Files, diags: &[Diagnostic], summary: &str) -> anyhow::Error {
    for diag in diags {
        eprint!("{}", render::human(diag, files));
    }
    anyhow!("{summary} ({} diagnostic(s))", diags.len())
}

/// One lowering failure: spanned constructs render as diagnostics; the
/// package-level errors stay one line.
pub(crate) fn lower_error(files: &Files, error: &LowerError) -> anyhow::Error {
    match error.span() {
        Some(span) => {
            let diagnostic = Diagnostic::error(ErrorCode::LowerUnsupported, error.to_string())
                .with_primary(span, "outside the M3 lowering contract");
            eprint!("{}", render::human(&diagnostic, files));
            anyhow!("the source does not lower under the M3 contract")
        },
        None => anyhow!("{error}"),
    }
}

/// The script rule's module name: the file's stem. `builtin` and `std`
/// are reserved for the native modules.
pub(crate) fn script_module_name(path: &Path) -> Result<String> {
    let name = path
        .file_stem()
        .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
    if name.is_empty() {
        return Err(anyhow!("`{}` has no module name to derive", path.display()));
    }
    if RESERVED_MODULES.contains(&name.as_str()) {
        return Err(anyhow!(
            "`{stem}` is a reserved native module name (`builtin`, `std`); rename the source file",
            stem = path.display()
        ));
    }
    if name == ds_stdlib::STD_PACKAGE_NAME {
        return Err(anyhow!(
            "`{stem}` is the reserved standard-library package name; a script or package module \
             cannot serve under it",
            stem = path.display()
        ));
    }
    Ok(name)
}

/// The module's dep list, recorded from the source's import declarations
/// into the header's link table: one entry per linked name in
/// declaration order — the plain list the load walk consumes, so a built
/// artifact names its deps and a walk over it never re-derives them from
/// source. A compile with import surfaces records the surfaces' own
/// link-name walk instead — the source's names in first-occurrence order
/// plus the re-export origins the surfaces introduce (`pub import`), so a
/// transitive entry's FuncRef position names a dep this list carries.
///
/// An `@`-sigil path links its package stem with the sigil kept
/// (`@std/csv` links `@std`): the sigil space is the registry's package
/// space, a package is one module (a single member), and the member path
/// stays a within-package selector the load
/// link does not carry. A bare path links the path as written. Aliases
/// and selective lists are spellings inside the source; a conditional
/// import records unconditionally (the artifact may run under the
/// condition's target). The content hash is the linked module's source
/// hash: the ambient member's for `@std` — the checked-in @std dsmod
/// serves under exactly that identity — the caller-supplied identity for
/// a name package resolution pinned, and the zero marker for every name
/// whose content nothing supplies; a zero link names a dep at no pinned
/// content, which the load walk's validation refuses.
/// The link name one import declaration records and links under:
/// an at-sigil path's package stem with the sigil (later segments are
/// member selectors), a bare path whole. The dep-list writer, the link
/// positions, and the surface builder all derive from this one rule.
pub(crate) fn import_link_name(import: &ds_parse::ImportDecl) -> String {
    let mut name = String::new();
    if import.path.at_sign {
        name.push('@');
        name.push_str(&import.path.segments[0].name);
    } else {
        for (at, segment) in import.path.segments.iter().enumerate() {
            if at > 0 {
                name.push('/');
            }
            name.push_str(&segment.name);
        }
    }
    name
}

fn dep_list<T>(
    asts: &[T],
    imports: Option<&ds_check::ImportGraph>,
    dep_content: &BTreeMap<String, Hash128>,
) -> Vec<ds_mod::Dep>
where
    T: std::borrow::Borrow<ds_parse::Ast>,
{
    // the surfaces' walk: the source's own link names plus the re-export
    // origins the surface builder assigned positions for, exactly the
    // order the entries' link facts address; a graph-absent compile
    // (the script path) walks its own imports in first-occurrence order
    let names: Vec<String> = if let Some(imports) = imports {
        imports.link_order().to_vec()
    } else {
        let mut names: Vec<String> = Vec::new();
        for ast in asts
            .iter()
            .map(std::borrow::Borrow::<ds_parse::Ast>::borrow)
        {
            for block in &ast.blocks {
                for item in &block.items {
                    let ItemKind::Import(import) = &item.kind else {
                        continue;
                    };
                    let name = import_link_name(import);
                    if !names.contains(&name) {
                        names.push(name);
                    }
                }
            }
        }
        names
    };
    names
        .into_iter()
        .map(|name| {
            // The reserved `@std` links at the identity the walk serves for
            // it: the graph's own bundle identity when the manifest declared
            // the dep, the ambient source hash otherwise (the merged-ambient
            // reading, whose walk serves the checked-in ambient dsmod under
            // exactly that hash).
            let content_hash = if name == "@std" {
                dep_content
                    .get(&name)
                    .copied()
                    .unwrap_or_else(|| Hash128::compute(ds_stdlib::AMBIENT_SOURCE.as_bytes()))
            } else {
                dep_content.get(&name).copied().unwrap_or(Hash128::ZERO)
            };
            ds_mod::Dep { name, content_hash }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ds_vm::abi::{ConstIdx, FrameLayout, SlotType, SlotType::I64, uop::Uop};

    use super::{BundleAfter, CompiledSource, Inputs, NO_DEPS, compile_linked, compile_text};

    /// The script-shaped test compile: one source, script parsing, no
    /// resolved graph.
    fn compile_script(
        inputs: &Inputs<'_>,
        source: &str,
        no_ambient: bool,
        allow_serve: bool,
    ) -> anyhow::Result<CompiledSource> {
        compile_text(
            inputs,
            &[(inputs.display.clone(), source.to_owned())],
            true,
            no_ambient,
            allow_serve,
            None,
            None,
            None,
            BundleAfter::Drop,
        )
    }

    /// One script's compile framing: the module name and display path the
    /// script rule derives.
    fn script_inputs(name: &str) -> Inputs<'static> {
        Inputs {
            name: name.to_owned(),
            display: format!("{name}.ds"),
            main_required: true,
            identity: ds_mod::Hash128::ZERO,
            dep_content: &NO_DEPS,
        }
    }

    /// The user program the served-vs-fresh gate runs: every seam where
    /// the ambient's bodies participate — the derive families
    /// dispatching into ambient impl members (`show`/`hash`/`ord`/
    /// `clone` and the type-keyed `default`), a free generic's bound
    /// dispatch (the CALLG whose evidence chain names an ambient body),
    /// a generic impl body dispatching into the ambient over its
    /// elements (the runtime-resolved site), and an `int | none` entry
    /// parameter (the user-minted tag table).
    const GATE: &str = "#[derive(show, hash, ord, clone, default)]\nstruct cell { n: int, label: \
                        string }\n\ntrait echo { fn echo(self) -> string }\n\nimpl[t: show] echo \
                        for list[t] {\nfn echo(self) -> string {\nvar acc = \"!\"\nfor x in self \
                        {\nacc = \"${acc}${x.show()}\"\n}\nacc\n}\n}\n\nfn render[t: show](v: t) \
                        -> string { v.show() }\n\nfn probe(port: int?) -> int { unwrap_or(port, \
                        7) }\n\nfn main() -> () {\nconst c = cell { n: 3, label: \"x\" }\nconst d \
                        = cell.default()\nprintln(\"${c.show()} ${c.label.hash()} ${c.compare(d)} \
                        ${render(5)} ${[1, 2].echo()} ${probe(none)} ${max(1, 2)}\")\n}\n";

    /// One compile over the gate program: the module bytes, the run
    /// output of `main` on a fresh VM (the ambient's world natives need
    /// the std platform installed first, the run path's own order), and
    /// how many function records the served splice installed.
    fn compile_and_run(allow_serve: bool) -> (Vec<u8>, Vec<u8>, usize) {
        let compiled = compile_script(&script_inputs("gate"), GATE, false, allow_serve)
            .expect("the gate program compiles");
        let spliced = compiled.lowered.spliced();
        let bytes = ds_mod::write(compiled.lowered.module());
        let mut registry = ds_vm::ModuleRegistry::new();
        registry.install_platform(ds_vm::Platform::std());
        let mut vm = ds_vm::Vm::new(registry);
        vm.install_dsmod(compiled.lowered.into_module())
            .expect("the module installs");
        vm.capture_output();
        vm.run("gate", "main", &[]).expect("the gate program runs");
        (bytes, vm.captured_output().to_vec(), spliced)
    }

    /// The served splice answers the fresh lowering's module byte for
    /// byte (function table, pools, code words, chains, tags, entry
    /// facts — the writer's canonical bytes pin them all) and the two
    /// modules run to the same output. The splice itself must have
    /// installed the artifact's whole record set on the served side:
    /// the two modules are byte-identical by contract, so a silent
    /// fallback would pass this comparison without ever serving.
    #[test]
    fn the_served_module_matches_a_fresh_one() {
        let records = crate::ambient_dsmod::serve()
            .expect("the artifact serves")
            .functions
            .len();
        let (served_bytes, served_out, served_spliced) = compile_and_run(true);
        let (fresh_bytes, fresh_out, fresh_spliced) = compile_and_run(false);
        assert!(!fresh_out.is_empty(), "the gate program prints");
        assert_eq!(fresh_spliced, 0, "the fresh baseline installs nothing");
        assert_eq!(
            served_spliced, records,
            "the splice must serve, not fall back"
        );
        assert_eq!(served_bytes, fresh_bytes, "the modules must be identical");
        assert_eq!(served_out, fresh_out, "the runs must print identically");
    }

    /// The splice really serves: the compile with the artifact enabled
    /// answers the artifact's function records verbatim — the names the
    /// served table carries are the planned table's own prefix, and the
    /// user's bodies sit after them.
    #[test]
    fn the_serve_installs_the_artifacts_bodies() {
        let served = crate::ambient_dsmod::serve().expect("the artifact serves");
        let compiled = compile_script(&script_inputs("gate"), GATE, false, true)
            .expect("the gate program compiles");
        assert_eq!(
            compiled.lowered.spliced(),
            served.functions.len(),
            "the artifact's whole record set installs"
        );
        let functions = compiled.lowered.functions();
        assert!(
            functions.len() > served.functions.len(),
            "the user's own bodies sit after the ambient's"
        );
        for (planned, body) in functions.iter().zip(served.functions.iter()) {
            assert_eq!(planned, body.name());
        }
    }

    /// The splice serves a compile whose own analysis mints none of the
    /// ambient's structural atoms: the artifact's tag rows include rows
    /// only the ambient's body walk mints (the json face's answer-union
    /// members under the list/map constructors), which a trivial
    /// program's universe never holds, so the seed must accept rows that
    /// resolve to no atom and the splice must still install the
    /// artifact's records.
    #[test]
    fn the_splice_serves_a_compile_without_the_ambient_s_structural_atoms() {
        const TRIVIAL: &str = "fn main() -> int {\n7\n}\n";
        let records = crate::ambient_dsmod::serve()
            .expect("the artifact serves")
            .functions
            .len();
        let compiled = compile_script(&script_inputs("trivial"), TRIVIAL, false, true)
            .expect("the trivial program compiles");
        assert_eq!(
            compiled.lowered.spliced(),
            records,
            "the splice serves a universe that never minted the ambient's structural atoms"
        );
        // the json readers' answer unions ride as structural rows a
        // trivial program never mints (the written enum's member atoms
        // under the `list[...]`/`map[...]` constructors)
        assert!(
            compiled
                .lowered
                .module()
                .tags()
                .iter()
                .any(|row| row.name.starts_with("list[json_null")),
            "the unminted row rides the module's table"
        );
    }

    /// `exit(code)` rides the proc capability's op row: the compiled
    /// module installs under a platform serving `proc` and answers the
    /// deliberate status out of the engine, while a platform refusing
    /// the capability refuses the module at install. The entry links
    /// the whole ambient face, so the walk names the first frame the
    /// refused capability leaves unserved — the pin is the capability
    /// the refusal reports, the exit op the served install runs.
    #[test]
    fn exit_rides_the_proc_capability_op_row() {
        const EXITS: &str = "fn main() -> () {\n  exit(3)\n  println(\"unreachable\")\n}\n";
        let compiled = compile_script(&script_inputs("exits"), EXITS, false, true)
            .expect("the exit program compiles");
        let bytes = ds_mod::write(compiled.lowered.module());

        let without_proc = || {
            let mut registry = ds_vm::ModuleRegistry::new();
            registry.install_platform(ds_vm::Platform::new("std", &[
                ds_vm::Capability::Console,
                ds_vm::Capability::Fsys,
                ds_vm::Capability::Net,
                ds_vm::Capability::Time,
                ds_vm::Capability::Tls,
            ]));
            ds_vm::Vm::new(registry)
        };
        let module = ds_mod::read(&bytes).expect("the module reads back");
        let err = without_proc().install_dsmod(module).unwrap_err();
        let ds_vm::LoadError::UnprovidedPlatformOp {
            platform,
            capability,
            ..
        } = err
        else {
            panic!("the install names the platform op: {err:?}");
        };
        assert_eq!(platform, "std", "the refusal names the platform");
        assert_eq!(
            capability, "proc",
            "a host refusing the proc capability refuses the module at install"
        );

        let mut vm = fresh_vm();
        let module = ds_mod::read(&bytes).expect("the module reads back");
        vm.install_dsmod(module)
            .expect("a proc-serving host installs the module");
        assert_eq!(
            vm.run("exits", "main", &[]).unwrap_err(),
            ds_vm::Trap::Exit { code: 3 },
            "the engine hands the deliberate status out and decides nothing"
        );
    }

    /// The fallback path: a served list that disagrees with the plan
    /// (here, two artifact bodies swapped, so the prefix names part) is
    /// answered by the fresh lowering — the splice's refusal never
    /// surfaces as the run's. The disagreement is package-independent,
    /// so the probe runs over a plain ambient-free program.
    #[test]
    fn a_disagreeing_serve_falls_back_to_the_fresh_module() {
        const SIMPLE: &str = "tag int\n\ntrait twice {\nfn twice(self) -> int\n}\n\nimpl twice \
                              for int {\nfn twice(self) -> int { self + self }\n}\n\nfn main() -> \
                              int {\nconst x = 21\nx.twice()\n}\n";
        let mut files = ds_diag::Files::new();
        let id = files.add_source("simple.ds", SIMPLE);
        let asts = [ds_parse::parse(&files, id).expect("the probe parses")];
        let analyzed = ds_check::analyze(&files, &asts);
        assert!(analyzed.checked.diags.is_empty(), "the probe checks clean");

        let mut served = crate::ambient_dsmod::serve().expect("the artifact serves");
        served.functions.swap(0, 1);
        let linear = ds_check::linearity_check(
            &analyzed.asts,
            &analyzed.package,
            &analyzed.resolutions,
            &analyzed.decls,
            &analyzed.checked,
        );
        let spliced = super::lower_via_serve(
            true,
            "simple",
            ds_mod::Hash128::compute(SIMPLE.as_bytes()),
            &analyzed.asts,
            &analyzed,
            Some(served),
            &linear,
            &ds_lower::ComptimeValues::default(),
        )
        .expect("the disagreeing serve still compiles");
        assert_eq!(
            spliced.spliced(),
            0,
            "the disagreeing serve installs nothing"
        );
        let fresh = ds_lower::lower(
            "simple",
            ds_mod::Hash128::compute(SIMPLE.as_bytes()),
            &analyzed.asts,
            &analyzed,
        )
        .expect("the fresh lowering compiles");
        assert_eq!(
            ds_mod::write(spliced.module()),
            ds_mod::write(fresh.module()),
            "the fallback answers the fresh module"
        );
    }

    // ---- the dep-list writer (the header's link table) ----

    /// The ambient member's source hash: the @std module's content
    /// identity the writer records and the checked-in artifact carries.
    fn std_hash() -> ds_mod::Hash128 {
        ds_mod::Hash128::compute(ds_stdlib::AMBIENT_SOURCE.as_bytes())
    }

    /// The @std module the walk serves: the checked-in artifact's dsmod
    /// payload (past its 16-byte build-identity header), carrying the
    /// linked name and the ambient source hash.
    fn std_artifact() -> ds_mod::Module {
        ds_mod::read(&ds_stdlib::AMBIENT_DSMOD[16..]).expect("the embedded @std artifact reads")
    }

    fn layout(slots: &[SlotType], params: usize, args: usize) -> FrameLayout {
        FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout")
    }

    /// A one-function module whose `main` returns a constant: bodies are
    /// irrelevant to walk order, so the hand-built root is this shape
    /// (the load-walk tests' own leaf).
    fn leaf(name: &str, source: &[u8], answer: i64) -> ds_mod::Module {
        let main = ds_mod::Function::new(
            "main",
            layout(&[I64], 0, 0),
            vec![ds_mod::Constant::I64(answer)],
            vec![
                Uop::Funcf { frame_slots: 1 },
                Uop::Kimm {
                    dst: 0,
                    kidx: ConstIdx(0),
                },
                Uop::Ret { src: 0 },
            ],
        )
        .expect("valid function");
        ds_mod::Module::new(
            name,
            ds_mod::TargetClass::Bytecode,
            ds_mod::Hash128::compute(source),
        )
        .with_function(main)
    }

    /// A fresh VM with the default std platform, the run path's own order.
    fn fresh_vm() -> ds_vm::Vm {
        let mut registry = ds_vm::ModuleRegistry::new();
        registry.install_platform(ds_vm::Platform::std());
        ds_vm::Vm::new(registry)
    }

    /// A source importing two @std members builds one dep entry — the
    /// sigil package stem, one module per package — at the
    /// ambient's content hash; write -> read keeps the list, and a walk
    /// over the built artifact installs @std first and the module last,
    /// the same ids a hand-built root carrying the same dep assigns.
    #[test]
    fn a_built_module_s_recorded_dep_list_round_trips_the_walk() {
        const APP: &str = "import @std/csv (split)\nimport @std/io\n\nfn main() -> int {\n7\n}\n";
        let compiled = compile_script(&script_inputs("app"), APP, false, true).expect("compiles");
        let recorded = compiled.lowered.module().deps().to_vec();
        assert_eq!(
            recorded,
            vec![ds_mod::Dep {
                name: "@std".to_owned(),
                content_hash: std_hash(),
            }],
            "both @std member imports link the package stem once"
        );

        // write -> read: the recorded list is the artifact's own fact
        let bytes = ds_mod::write(compiled.lowered.module());
        let built = ds_mod::read(&bytes).expect("the written module reads back");
        assert_eq!(built.deps(), recorded.as_slice());

        // the walk over the built artifact, the provider serving the
        // checked-in @std dsmod
        let mut vm = fresh_vm();
        let served = std_artifact();
        vm.load(built, |name, _| (name == "@std").then(|| served.clone()))
            .expect("the built module loads over the served @std");
        assert_eq!(vm.registry().module_id("@std"), Some(0));
        assert_eq!(vm.registry().module_id("app"), Some(1));

        // the hand-built root: the same list a test assembles by hand
        // assigns the same ids
        let mut vm = fresh_vm();
        let served = std_artifact();
        vm.load(
            leaf("app", APP.as_bytes(), 7).with_dep("@std", std_hash()),
            |name, _| (name == "@std").then(|| served.clone()),
        )
        .expect("the hand-built root loads");
        assert_eq!(vm.registry().module_id("@std"), Some(0));
        assert_eq!(vm.registry().module_id("app"), Some(1));
    }

    /// A recorded link whose content disagrees refuses the walk as today:
    /// the provider serves @std at a drifted hash and the refusal names
    /// the hash the writer recorded.
    #[test]
    fn a_stale_hash_in_the_recorded_list_refuses_as_today() {
        const APP: &str = "import @std/io\n\nfn main() -> int {\n7\n}\n";
        let compiled = compile_script(&script_inputs("app"), APP, false, true).expect("compiles");
        let bytes = ds_mod::write(compiled.lowered.module());
        let built = ds_mod::read(&bytes).expect("the written module reads back");
        let mut vm = ds_vm::Vm::new(ds_vm::ModuleRegistry::new());
        let served = leaf("@std", b"drifted-std", 1);
        assert_eq!(
            vm.load(built, |name, _| (name == "@std").then(|| served.clone()))
                .unwrap_err(),
            ds_vm::LoadError::HashMismatch {
                dep: "@std".to_owned(),
                dependent: "app".to_owned(),
                linked: std_hash(),
                found: Some(ds_mod::Hash128::compute(b"drifted-std")),
            }
        );
        assert_eq!(vm.registry().module_id("app"), None);
    }

    /// An import naming the enclosing module's own name records a
    /// self-named entry, which the walk resolves against the module's own
    /// install and never fetches: a provider answering nothing still
    /// loads, and the entry runs.
    #[test]
    fn a_self_named_recorded_entry_stays_local() {
        const APP: &str = "import app\n\nfn main() -> int {\n7\n}\n";
        let compiled = compile_script(&script_inputs("app"), APP, false, true).expect("compiles");
        assert_eq!(compiled.lowered.module().deps(), [ds_mod::Dep {
            name: "app".to_owned(),
            content_hash: ds_mod::Hash128::ZERO,
        }]);
        let bytes = ds_mod::write(compiled.lowered.module());
        let built = ds_mod::read(&bytes).expect("the written module reads back");
        let mut vm = fresh_vm();
        vm.load(built, |_, _| None)
            .expect("the self-named entry never reaches the provider");
        assert_eq!(vm.registry().module_id("app"), Some(0));
        assert_eq!(
            vm.run("app", "main", &[]),
            Ok(ds_vm::FrameAnswer::one(ds_vm::Value::I64(7)))
        );
    }

    /// A user-package import the compile cannot resolve records the zero
    /// marker — a name linked at no pinned content — and the walk refuses
    /// it as a provider miss: no package resolution exists to
    /// serve or validate it.
    #[test]
    fn an_unresolved_package_import_records_a_zero_link_the_walk_refuses() {
        const APP: &str =
            "import acme/pipeline/schema (row, parse_row)\n\nfn main() -> int {\n7\n}\n";
        let compiled = compile_script(&script_inputs("app"), APP, false, true).expect("compiles");
        assert_eq!(compiled.lowered.module().deps(), [ds_mod::Dep {
            name: "acme/pipeline/schema".to_owned(),
            content_hash: ds_mod::Hash128::ZERO,
        }]);
        let bytes = ds_mod::write(compiled.lowered.module());
        let built = ds_mod::read(&bytes).expect("the written module reads back");
        let mut vm = ds_vm::Vm::new(ds_vm::ModuleRegistry::new());
        assert_eq!(
            vm.load(built, |_, _| None).unwrap_err(),
            ds_vm::LoadError::ProviderMiss {
                dep: "acme/pipeline/schema".to_owned(),
                dependent: "app".to_owned(),
            }
        );
    }

    /// The identities package resolution supplies enter the writer: a
    /// linked name the map pins records that identity, and a name outside
    /// the map still records the zero marker.
    #[test]
    fn supplied_identities_enter_the_link_table() {
        const APP: &str = "import @lib/helper (twice)\nimport @other\n\nfn main() -> int {\n7\n}\n";
        let identity = ds_mod::Hash128::compute(b"the lib member's text");
        let dep_content = BTreeMap::from([("@lib".to_owned(), identity)]);
        let compiled = compile_linked(
            "@app",
            &[("@app/main.ds".to_owned(), APP.to_owned())],
            ds_mod::Hash128::compute(APP.as_bytes()),
            false,
            true,
            false,
            &dep_content,
            None,
            None,
            None,
            BundleAfter::Drop,
        )
        .expect("compiles");
        assert_eq!(compiled.lowered.module().deps(), [
            ds_mod::Dep {
                name: "@lib".to_owned(),
                content_hash: identity,
            },
            ds_mod::Dep {
                name: "@other".to_owned(),
                content_hash: ds_mod::Hash128::ZERO,
            },
        ]);
    }

    /// The wider import shapes record their plain linked name: an alias
    /// and a condition are spellings inside the source (the packages
    /// round owns their semantics), so the list carries the name each
    /// declaration links and nothing else.
    #[test]
    fn aliased_and_conditional_imports_record_their_plain_linked_name() {
        const APP: &str = "import acme/pipeline/fs as pio\nimport acme/pipeline/io if os == \
                           \"windows\"\n\nfn main() -> int {\n7\n}\n";
        let compiled = compile_script(&script_inputs("app"), APP, false, true).expect("compiles");
        assert_eq!(compiled.lowered.module().deps(), [
            ds_mod::Dep {
                name: "acme/pipeline/fs".to_owned(),
                content_hash: ds_mod::Hash128::ZERO,
            },
            ds_mod::Dep {
                name: "acme/pipeline/io".to_owned(),
                content_hash: ds_mod::Hash128::ZERO,
            },
        ]);
    }
}
