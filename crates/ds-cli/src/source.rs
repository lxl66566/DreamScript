//! The source pipeline behind `ds run <file.ds>` and `ds build`: one .ds
//! file through parse, the static story ([`ds_check::analyze`], the stdlib
//! ambient registered first), the platform check ([`ds_check::check_platform`]),
//! and lowering ([`ds_lower::lower`]) to a
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

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use ds_check::{Analyzed, Ty};
use ds_diag::{Diagnostic, ErrorCode, Files, render};
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
pub fn compile(path: &Path, entry: &str, no_ambient: bool) -> Result<CompiledSource> {
    let source =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    compile_text(path, &source, entry, no_ambient, true)
}

/// The compile pipeline over source text (`compile` after its read):
/// `allow_serve` switches the ambient dsmod splice off, the fresh
/// lowering baseline the artifact gates compare against.
fn compile_text(
    path: &Path,
    source: &str,
    entry: &str,
    no_ambient: bool,
    allow_serve: bool,
) -> Result<CompiledSource> {
    let mut files = Files::new();
    // The trees move into the analyzer, one pre-expansion mark per tree:
    // the blob-served ambient's is expanded at artifact build time, every
    // other tree expands at run time.
    let mut asts = Vec::new();
    let mut preexpanded = Vec::new();
    // The checked-ambient artifact's serve, when it verifies against this
    // registration (source hash plus file id): the ambient's resolve half
    // and declaration prefix arrive pre-built, so the analyze collects,
    // resolves, and declares only the user file. Every miss falls back to
    // the fresh compute — the artifact is a pure cache.
    let mut served = None;
    if !no_ambient {
        let id = files.add_source(ds_stdlib::AMBIENT_FILE, ds_stdlib::AMBIENT_SOURCE);
        let (ast, expanded) = ambient_tree(&files, id);
        asts.push(ast);
        preexpanded.push(expanded);
        served = ds_stdlib::ambient_dsi(id).ok();
    }
    // The given file is the entry: script mode folds its top-level
    // statements into a synthesized `main` (the ambient stays
    // pure-declaration, like every imported file).
    let id = files.add_source(path.to_string_lossy().into_owned(), source);
    asts.push(match parse_script(&files, id) {
        Ok(ast) => ast,
        Err(diags) => return Err(reject(&files, &diags, "the source has syntax errors")),
    });
    preexpanded.push(false);
    // The ambient's solved effect rows ride beside the served analyze:
    // the effects pass seeds them and walks only the user tree.
    let mut served_rows = None;
    let analyzed = match served {
        Some(served) => {
            let ds_stdlib::ServedAmbient {
                package,
                resolutions,
                decls,
                effect_defs,
                effect_methods,
            } = served;
            served_rows = Some((effect_defs, effect_methods));
            ds_check::analyze_served(&files, asts, &preexpanded, package, resolutions, decls)
        },
        None => ds_check::analyze_owned(&files, asts, &preexpanded),
    };
    if !analyzed.checked.diags.is_empty() {
        return Err(reject(
            &files,
            &analyzed.checked.diags,
            "the source does not check clean",
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
        Some((effect_defs, effect_methods)) => ds_check::effects::effects_of_resolved_served(
            &asts[1..],
            &analyzed.package,
            &analyzed.resolutions[1..],
            &effect_defs,
            &effect_methods,
            rows,
            caps,
        ),
        None => ds_check::effects::effects_of_resolved(
            asts,
            &analyzed.package,
            &analyzed.resolutions,
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
    let name = module_name(path)?;
    let source_hash = Hash128::compute(source.as_bytes());
    // The comptime evaluator (G1): comptime calls run on the fenced VM
    // here, their answers embedding at the call sites below — a package
    // with no comptime call evaluates nothing.
    let comptime = ds_meta::evaluate(&name, source_hash, asts, &analyzed)
        .map_err(|diags| reject(&files, &diags, "the source fails the comptime fence"))?;
    // The ambient's impl bodies splice in from the checked-in dsmod when
    // it verifies against this build; every miss (identity, format,
    // name, source hash, splice boundary, plan disagreement) falls back
    // to lowering them here, whose answer is the canonical one.
    let served = if !no_ambient && allow_serve { crate::ambient_dsmod::serve() } else { None };
    let lowered = lower_via_serve(&name, source_hash, asts, &analyzed, served, &comptime)
        .map_err(|error| lower_error(&files, &error))?;
    // every top-level lambda is a candidate `--entry` name, so the whole
    // table rides the artifact and any of them serves a warm run; this
    // run's entry takes its own slice
    let entries = entry_plans(asts, &analyzed);
    let entry_args = entries
        .iter()
        .find(|plan| plan.name == entry)
        .map(|plan| plan.args.clone());
    Ok(CompiledSource {
        lowered: lowered.with_entry_plans(entries),
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
    name: &str,
    source_hash: Hash128,
    asts: &[ds_parse::Ast],
    analyzed: &Analyzed,
    served: Option<Vec<ds_mod::Function>>,
    comptime: &ComptimeValues,
) -> Result<Lowered, LowerError> {
    if let Some(served) = served
        && let Ok(lowered) = lower_with_served(name, source_hash, asts, analyzed, served, comptime)
    {
        return Ok(lowered);
    }
    lower_with_comptime(name, source_hash, asts, analyzed, comptime)
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

/// The module name: the file's stem. `builtin` and `std` are reserved
/// for the native modules.
fn module_name(path: &Path) -> Result<String> {
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::compile_text;

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
        let compiled = compile_text(Path::new("gate.ds"), GATE, "main", false, allow_serve)
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
        let compiled = compile_text(Path::new("gate.ds"), GATE, "main", false, true)
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
}
