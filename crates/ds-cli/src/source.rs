//! The source pipeline behind `ds run <file.ds>` and `ds build`: one .ds
//! file through parse, the static story ([`ds_check::analyze`], the stdlib
//! ambient registered first) with its affine walk gate, the platform check
//! ([`ds_check::check_platform`]), and lowering ([`ds_lower::lower`]) to a
//! ds-mod module — stopping where each subcommand needs it. The module
//! carries the source's blake3 hash, so [`ds_mod::read_checked`] verifies
//! what this pipeline records.
//!
//! Lowering's contract is the M3 subset: monomorphic const-arrow
//! functions, rebindable `var` locals with assignment and `while` loops,
//! UFCS dot-calls onto resolved free functions (F3), the covered builtin
//! natives, early exits, interpolation, and tail positions. Constructs
//! outside it fail with a structured diagnostic naming the construct and
//! the seam it needs.
//!
//! Package mode runs the same pipeline with the caller's framing: a
//! resolved package's member (or a package-shaped root) names its module
//! directly, and the resolved graph's dep identities ride in, so the
//! dep-list writer records real content for the names the source text
//! alone cannot determine.

use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result, anyhow};
use ds_check::{Analyzed, Ty};
use ds_diag::{Diagnostic, ErrorCode, Files, render};
use ds_dsi::ServedPrefix;
use ds_lower::{ComptimeValues, LowerError, Lowered, lower_with_comptime, lower_with_served};
use ds_mod::{EntryArg, EntryPlan, Hash128};
use ds_parse::{ItemKind, Pattern, parse, parse_script};

/// The reserved native module stems: a source file with either stem
/// would shadow pool FuncRefs the lowered bodies emit (the VM's `builtin`
/// core and the default `std` platform handler's module name).
const RESERVED_MODULES: [&str; 2] = ["builtin", "std"];

/// One compiled source: the lowered module plus what the run entry path
/// needs — the module's name and the entry's staging plan.
pub struct CompiledSource {
    /// The lowered module, its name derived from the file's stem, with
    /// the tag table and every candidate entry's staging plan recorded.
    pub lowered: Lowered,
    /// The module name.
    pub name: String,
    /// The entry's staging plan, when a lowerable function of the entry
    /// name exists.
    pub entry_args: Option<Vec<EntryArg>>,
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
    entry: &str,
    no_ambient: bool,
    prefix_cache: Option<&crate::dsi_cache::PrefixCache>,
) -> Result<CompiledSource> {
    let source =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let inputs = Inputs {
        name: script_module_name(path)?,
        display: path.display().to_string(),
        main_required: true,
        dep_content: &NO_DEPS,
    };
    compile_text(&inputs, &source, entry, no_ambient, true, prefix_cache)
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
    dep_content: &'a BTreeMap<String, Hash128>,
}

/// Compiles one package-shaped root over source text. `name` is the
/// module's name (the script rule, its file stem), `display` the path
/// diagnostics render, and `dep_content` the resolved graph's
/// identities, which the writer records for names the source text alone
/// cannot determine. The run contract's `main` entry is required, the
/// script rule.
///
/// # Errors
///
/// A reserved or empty module name, and every pipeline failure
/// [`compile`] reports.
pub fn compile_package(
    name: &str,
    display: &str,
    source: &str,
    entry: &str,
    no_ambient: bool,
    dep_content: &BTreeMap<String, Hash128>,
) -> Result<CompiledSource> {
    compile_framed(name, display, source, entry, no_ambient, true, dep_content)
}

/// Compiles one resolved package's single source member: the module
/// serves under the package's link name, its own recorded imports
/// validate against the same graph through the caller's `dep_content`,
/// and the run contract's `main` entry check is waived — an installed
/// member serves its members and never runs as an entry.
///
/// # Errors
///
/// A reserved or empty module name, and every pipeline failure
/// [`compile`] reports.
pub fn compile_member(
    name: &str,
    display: &str,
    source: &str,
    no_ambient: bool,
    dep_content: &BTreeMap<String, Hash128>,
) -> Result<CompiledSource> {
    compile_framed(
        name,
        display,
        source,
        "main",
        no_ambient,
        false,
        dep_content,
    )
}

/// The shared framing behind the two package shapes.
fn compile_framed(
    name: &str,
    display: &str,
    source: &str,
    entry: &str,
    no_ambient: bool,
    main_required: bool,
    dep_content: &BTreeMap<String, Hash128>,
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
        display: display.to_owned(),
        main_required,
        dep_content,
    };
    compile_text(&inputs, source, entry, no_ambient, true, None)
}

/// The compile pipeline over source text (`compile` after its read):
/// `allow_serve` switches the ambient dsmod splice off, the fresh
/// lowering baseline the artifact gates compare against.
fn compile_text(
    inputs: &Inputs<'_>,
    source: &str,
    entry: &str,
    no_ambient: bool,
    allow_serve: bool,
    prefix_cache: Option<&crate::dsi_cache::PrefixCache>,
) -> Result<CompiledSource> {
    let mut files = Files::new();
    // The trees move into the analyzer, one pre-expansion mark per tree:
    // the blob-served ambient's is expanded at artifact build time, every
    // other tree expands at run time.
    let mut asts = Vec::new();
    let mut preexpanded = Vec::new();
    // The registration the prefix pins: the ambient first, the entry
    // file after it.
    let mut ids = Vec::new();
    if !no_ambient {
        let id = files.add_source(ds_stdlib::AMBIENT_FILE, ds_stdlib::AMBIENT_SOURCE);
        let (ast, expanded) = ambient_tree(&files, id);
        asts.push(ast);
        preexpanded.push(expanded);
        ids.push(id);
    }
    // The given file is the entry: script mode folds its top-level
    // statements into a synthesized `main` (the ambient stays
    // pure-declaration, like every imported file).
    let id = files.add_source(inputs.display.clone(), source);
    asts.push(match parse_script(&files, id) {
        Ok(ast) => ast,
        Err(diags) => return Err(reject(&files, &diags, "the source has syntax errors")),
    });
    preexpanded.push(false);
    ids.push(id);
    let name = inputs.name.clone();
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
        served = cache.lookup(&name, source, &ids);
    }
    if served.is_none() && !no_ambient {
        served = ds_stdlib::ambient_dsi(ids[0]).ok();
    }
    let analyzed = match served {
        Some(prefix) => {
            let ServedPrefix {
                package,
                resolutions,
                decls,
                effect_defs,
                effect_methods,
            } = prefix;
            served_rows = Some((resolutions.len(), effect_defs, effect_methods));
            ds_meta::analyze_served(&files, asts, &preexpanded, package, resolutions, decls)
        },
        None => ds_meta::analyze_owned(&files, asts, &preexpanded),
    };
    if !analyzed.checked.diags.is_empty() {
        return Err(reject(
            &files,
            &analyzed.checked.diags,
            "the source does not check clean",
        ));
    }
    // The affine walk (C1, C5): the same pass `ds check` and `ds repl`
    // consult before committing, refusing here before anything lowers
    // or runs. Lowering derives its move and deinit placement from this
    // walk's facts, which hold only over a clean one — a moved-then-used
    // value would lower code whose deinit fires twice.
    let linear = ds_check::linearity_check(
        &analyzed.asts,
        &analyzed.package,
        &analyzed.resolutions,
        &analyzed.decls,
        &analyzed.checked,
    );
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
    // The platform check [H5]: the package's world usage verified inside
    // what its platform declaration provides. Without the ambient there
    // are no declared rows to attribute, but the declaration itself is
    // still validated.
    let empty_rows = ds_check::DeclaredRows::new();
    let empty_caps = ds_check::WorldCaps::new();
    let (rows, caps): (&ds_check::DeclaredRows, &ds_check::WorldCaps) = if no_ambient {
        (&empty_rows, &empty_caps)
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
    // The pooled-body refusal [E3]: a fork-join combinator's element
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
    let source_hash = Hash128::compute(source.as_bytes());
    // The comptime evaluator (G1): comptime calls run on the fenced VM
    // here, their answers embedding at the call sites below — a package
    // with no comptime call evaluates nothing.
    let comptime = ds_meta::evaluate(&name, source_hash, asts, &analyzed, &effects)
        .map_err(|diags| reject(&files, &diags, "the source fails the comptime fence"))?;
    // The ambient's impl bodies splice in from the checked-in dsmod when
    // it verifies against this build; every miss (identity, format,
    // name, source hash, splice boundary, plan disagreement) falls back
    // to lowering them here, whose answer is the canonical one.
    let served = if !no_ambient && allow_serve { crate::ambient_dsmod::serve() } else { None };
    let lowered = lower_via_serve(
        inputs.main_required,
        &name,
        source_hash,
        asts,
        &analyzed,
        served,
        &comptime,
    )
    .map_err(|error| lower_error(&files, &error))?;
    // every top-level lambda is a candidate `--entry` name, so the whole
    // table rides the artifact and any of them serves a warm run; this
    // run's entry takes its own slice. The dep list rides the header the
    // same way: the artifact names the source's imports, so a load walk
    // over it resolves its deps without re-deriving them from source.
    let entries = entry_plans(asts, &analyzed);
    let entry_args = entries
        .iter()
        .find(|plan| plan.name == entry)
        .map(|plan| plan.args.clone());
    let deps = dep_list(&asts[usize::from(!no_ambient)..], inputs.dep_content);
    // A clean, fully lowered compile persists its own prefix for the
    // next run of the same source; every failure inside is silent (the
    // compile already succeeded, the next run falls back).
    if let Some(cache) = prefix_cache {
        cache.store(&name, source, &ids, &analyzed, &effects, rows, caps);
    }
    Ok(CompiledSource {
        lowered: lowered.with_deps(deps).with_entry_plans(entries),
        name,
        entry_args,
    })
}

/// The ambient member's tree and whether it arrived pre-expanded: the
/// embedded blob when it verifies against this registration (its trees
/// carry the derive expansion from artifact build time), a fresh parse
/// otherwise — the blob is a pure cache, never correctness-load-bearing.
fn ambient_tree(files: &Files, id: ds_diag::FileId) -> (ds_parse::Ast, bool) {
    match ds_stdlib::ambient_ast(id) {
        Ok(ast) => (ast, true),
        Err(_) => (parse(files, id).expect("the ambient parses"), false),
    }
}

/// The lowering seam the ambient dsmod rides: the served bodies splice
/// over the planned ambient prefix, and any failure of theirs — the
/// splice disagreement included — falls back to the fresh lowering,
/// whose result (success or refusal) is the run's canonical answer: the
/// serve is a pure cache, never correctness-load-bearing. Both paths
/// embed the comptime evaluator's answers at the call sites.
fn lower_via_serve(
    main_required: bool,
    name: &str,
    source_hash: Hash128,
    asts: &[ds_parse::Ast],
    analyzed: &Analyzed,
    served: Option<Vec<ds_mod::Function>>,
    comptime: &ComptimeValues,
) -> Result<Lowered, LowerError> {
    if main_required {
        if let Some(served) = served
            && let Ok(lowered) =
                lower_with_served(name, source_hash, asts, analyzed, served, comptime)
        {
            return Ok(lowered);
        }
        return lower_with_comptime(name, source_hash, asts, analyzed, comptime);
    }
    // the member shape: the library lowering, the same served splice
    if let Some(served) = served
        && let Ok(lowered) =
            ds_lower::lower_library(name, source_hash, asts, analyzed, Some(served), comptime)
    {
        return Ok(lowered);
    }
    ds_lower::lower_library(name, source_hash, asts, analyzed, None, comptime)
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
fn lower_error(files: &Files, error: &LowerError) -> anyhow::Error {
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
    Ok(name)
}

/// Every top-level lambda binding's staging plan, in declaration order
/// with the first occurrence per name (the entry lookup's first-match
/// rule): each binding is a candidate `--entry` name, and the table
/// rides the artifact so any of them serves a warm run.
fn entry_plans(asts: &[ds_parse::Ast], analyzed: &Analyzed) -> Vec<EntryPlan> {
    let mut entries: Vec<EntryPlan> = Vec::new();
    for ast in asts {
        for block in &ast.blocks {
            for item in &block.items {
                let ItemKind::Local(local) = &item.kind else {
                    continue;
                };
                let Pattern::Binding { name, .. } = &local.pat else {
                    continue;
                };
                let ds_parse::Expr::Lambda { params, .. } = &local.init else {
                    continue;
                };
                if entries.iter().any(|plan| plan.name == name.name) {
                    continue;
                }
                let Some(types) = params
                    .iter()
                    .map(|param| {
                        analyzed
                            .checked
                            .binders
                            .get(&param.name.span)
                            .map(|fact| fact.ty)
                    })
                    .collect::<Option<Vec<_>>>()
                else {
                    continue;
                };
                entries.push(EntryPlan {
                    name: name.name.clone(),
                    args: arg_plans(analyzed, &types),
                });
            }
        }
    }
    entries
}

/// How one parameter stages, by its checked type's member names: the
/// scalar atoms stage their word, `int | none` stages its two-word union
/// span.
fn arg_plans(analyzed: &Analyzed, params: &[Ty]) -> Vec<EntryArg> {
    let universe = &analyzed.decls.universe;
    params
        .iter()
        .map(|ty| {
            let members = universe.members(*ty);
            let mut names: Vec<&str> = members
                .iter()
                .filter_map(|atom| {
                    universe
                        .atom_decl(*atom)
                        .ok()
                        .map(|decl| decl.name.as_str())
                })
                .collect();
            names.sort_unstable();
            match names.as_slice() {
                ["bool"] => EntryArg::Bool,
                ["float"] => EntryArg::Float,
                ["int"] => EntryArg::Int,
                ["int", "none"] => EntryArg::OptionalInt,
                _ => EntryArg::Foreign,
            }
        })
        .collect()
}

/// The module's dep list, recorded from the source's import declarations
/// into the header's link table (G3.5): one entry per linked name in
/// declaration order — the plain list the load walk consumes, so a built
/// artifact names its deps and a walk over it never re-derives them from
/// source.
///
/// An `@`-sigil path links its package stem with the sigil kept
/// (`@std/csv` links `@std`): the sigil space is the registry's package
/// space, a package is one module this round (G3.5's single-member
/// ruling), and the member path stays a within-package selector the load
/// link does not carry. A bare path links the path as written. Aliases
/// and selective lists are spellings inside the source; a conditional
/// import records unconditionally (the artifact may run under the
/// condition's target). The content hash is the linked module's source
/// hash: the ambient member's for `@std` — the checked-in @std dsmod
/// serves under exactly that identity — the caller-supplied identity for
/// a name package resolution pinned, and the zero marker for every name
/// whose content nothing supplies; a zero link names a dep at no pinned
/// content, which the load walk's validation refuses.
fn dep_list(asts: &[ds_parse::Ast], dep_content: &BTreeMap<String, Hash128>) -> Vec<ds_mod::Dep> {
    let mut deps: Vec<ds_mod::Dep> = Vec::new();
    for ast in asts {
        for block in &ast.blocks {
            for item in &block.items {
                let ItemKind::Import(import) = &item.kind else {
                    continue;
                };
                let mut name = String::new();
                if import.path.at_sign {
                    // the package stem carries the sigil; later segments
                    // are member selectors, not link names
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
                if deps.iter().any(|dep| dep.name == name) {
                    continue;
                }
                let content_hash = if name == "@std" {
                    Hash128::compute(ds_stdlib::AMBIENT_SOURCE.as_bytes())
                } else {
                    dep_content.get(&name).copied().unwrap_or(Hash128::ZERO)
                };
                deps.push(ds_mod::Dep { name, content_hash });
            }
        }
    }
    deps
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ds_vm::abi::{ConstIdx, FrameLayout, SlotType, SlotType::I64, uop::Uop};

    use super::{Inputs, NO_DEPS, compile_package, compile_text};

    /// One script's compile framing: the module name and display path the
    /// script rule derives.
    fn script_inputs(name: &str) -> Inputs<'static> {
        Inputs {
            name: name.to_owned(),
            display: format!("{name}.ds"),
            main_required: true,
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

    /// One compile over the gate program: the module bytes plus the run
    /// output of `main` on a fresh VM (the ambient's world natives need
    /// the std platform installed first, the run path's own order).
    fn compile_and_run(allow_serve: bool) -> (Vec<u8>, Vec<u8>) {
        let compiled = compile_text(
            &script_inputs("gate"),
            GATE,
            "main",
            false,
            allow_serve,
            None,
        )
        .expect("the gate program compiles");
        let bytes = ds_mod::write(compiled.lowered.module());
        let mut registry = ds_vm::ModuleRegistry::new();
        registry.install_platform(ds_vm::Platform::std());
        let mut vm = ds_vm::Vm::new(registry);
        vm.install_dsmod(compiled.lowered.into_module())
            .expect("the module installs");
        vm.capture_output();
        vm.run("gate", "main", &[]).expect("the gate program runs");
        (bytes, vm.captured_output().to_vec())
    }

    /// The served splice answers the fresh lowering's module byte for
    /// byte (function table, pools, code words, chains, tags, entry
    /// facts — the writer's canonical bytes pin them all) and the two
    /// modules run to the same output.
    #[test]
    fn the_served_module_matches_a_fresh_one() {
        let (served_bytes, served_out) = compile_and_run(true);
        let (fresh_bytes, fresh_out) = compile_and_run(false);
        assert!(!fresh_out.is_empty(), "the gate program prints");
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
        let compiled = compile_text(&script_inputs("gate"), GATE, "main", false, true, None)
            .expect("the gate program compiles");
        let functions = compiled.lowered.functions();
        assert!(
            functions.len() > served.len(),
            "the user's own bodies sit after the ambient's"
        );
        for (planned, body) in functions.iter().zip(served.iter()) {
            assert_eq!(planned, body.name());
        }
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
        served.swap(0, 1);
        let spliced = super::lower_via_serve(
            true,
            "simple",
            ds_mod::Hash128::compute(SIMPLE.as_bytes()),
            &analyzed.asts,
            &analyzed,
            Some(served),
            &ds_lower::ComptimeValues::default(),
        )
        .expect("the disagreeing serve still compiles");
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

    // ---- the dep-list writer (G3.5's link table) ----

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
    /// sigil package stem, single-member packages this round — at the
    /// ambient's content hash; write -> read keeps the list, and a walk
    /// over the built artifact installs @std first and the module last,
    /// the same ids a hand-built root carrying the same dep assigns.
    #[test]
    fn a_built_module_s_recorded_dep_list_round_trips_the_walk() {
        const APP: &str = "import @std/csv (split)\nimport @std/io\n\nfn main() -> int {\n7\n}\n";
        let compiled =
            compile_text(&script_inputs("app"), APP, "main", false, true, None).expect("compiles");
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
        let compiled =
            compile_text(&script_inputs("app"), APP, "main", false, true, None).expect("compiles");
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
        let compiled =
            compile_text(&script_inputs("app"), APP, "main", false, true, None).expect("compiles");
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
    /// it as a provider miss this round: no package resolution exists to
    /// serve or validate it.
    #[test]
    fn an_unresolved_package_import_records_a_zero_link_the_walk_refuses() {
        const APP: &str =
            "import acme/pipeline/schema (row, parse_row)\n\nfn main() -> int {\n7\n}\n";
        let compiled =
            compile_text(&script_inputs("app"), APP, "main", false, true, None).expect("compiles");
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
        let compiled = compile_package("@app", "@app/main.ds", APP, "main", false, &dep_content)
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
        let compiled =
            compile_text(&script_inputs("app"), APP, "main", false, true, None).expect("compiles");
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
