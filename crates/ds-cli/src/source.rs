//! The source pipeline behind `ds run <file.ds>` and `ds build`: one .ds
//! file through parse, the static story ([`ds_check::analyze`], the stdlib
//! ambient registered first), and lowering ([`ds_lower::lower`]) to a
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
use ds_lower::{LowerError, Lowered, lower};
use ds_mod::Hash128;
use ds_parse::{ItemKind, Pattern, parse, parse_script};
use ds_vm::abi::{
    SlotType,
    SlotType::{Bool, F64, I64},
};

/// The reserved native module stems: a source file with either stem would
/// shadow pool FuncRefs the lowered bodies emit (the VM's `builtin` core
/// and the `std` platform module).
const RESERVED_MODULES: [&str; 2] = ["builtin", "std"];

/// How one `--args` value stages onto the entry's parameter words.
#[derive(Clone, Copy, Debug)]
pub enum ArgPlan {
    /// One parameter word of this kind.
    Word(SlotType),
    /// An `int | none` parameter: a staged value presents the int member,
    /// an absent value presents none (optional program arguments).
    OptionalInt,
    /// A parameter shape `--args` cannot stage; the run reports it.
    Foreign,
}

/// One compiled source: the lowered module plus what the run entry path
/// needs — the module's name and the entry's staging plan.
pub struct CompiledSource {
    /// The lowered module, its name derived from the file's stem.
    pub lowered: Lowered,
    /// The module name.
    pub name: String,
    /// The entry's staging plan, when a lowerable function of the entry
    /// name exists.
    pub entry_args: Option<Vec<ArgPlan>>,
}

/// Parses, checks, and lowers one source file. Diagnostics render to
/// stderr as they do under `ds check`; the returned error summarizes the
/// report (the subcommand exits 1 either way).
///
/// # Errors
///
/// File reads, syntax and checker diagnostics, and lowering failures
/// (each named and located).
pub fn compile(path: &Path, entry: &str, no_ambient: bool) -> Result<CompiledSource> {
    let source =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut files = Files::new();
    let mut asts = Vec::new();
    if !no_ambient {
        let id = files.add_source(ds_stdlib::AMBIENT_FILE, ds_stdlib::AMBIENT_SOURCE);
        asts.push(parse(&files, id).expect("the ambient parses"));
    }
    // The given file is the entry: script mode folds its top-level
    // statements into a synthesized `main` (the ambient stays
    // pure-declaration, like every imported file).
    let id = files.add_source(path.to_string_lossy().into_owned(), &source);
    asts.push(match parse_script(&files, id) {
        Ok(ast) => ast,
        Err(diags) => return Err(reject(&files, &diags, "the source has syntax errors")),
    });
    let analyzed = ds_check::analyze(&files, &asts);
    if !analyzed.checked.diags.is_empty() {
        return Err(reject(
            &files,
            &analyzed.checked.diags,
            "the source does not check clean",
        ));
    }
    let name = module_name(path)?;
    let lowered = lower(&name, Hash128::compute(source.as_bytes()), &asts, &analyzed)
        .map_err(|error| lower_error(&files, &error))?;
    let entry_args =
        entry_params(&asts, &analyzed, entry).map(|params| arg_plans(&analyzed, &params));
    Ok(CompiledSource {
        lowered,
        name,
        entry_args,
    })
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

/// The entry's parameter types, from the top-level `const <entry>`
/// binding's checked binder facts; `None` when no such function exists.
fn entry_params(asts: &[ds_parse::Ast], analyzed: &Analyzed, entry: &str) -> Option<Vec<Ty>> {
    asts.iter().find_map(|ast| {
        ast.blocks.iter().find_map(|block| {
            block.items.iter().find_map(|item| {
                let ItemKind::Local(local) = &item.kind else {
                    return None;
                };
                let Pattern::Binding { name, .. } = &local.pat else {
                    return None;
                };
                if name.name != entry {
                    return None;
                }
                let ds_parse::Expr::Lambda { params, .. } = &local.init else {
                    return None;
                };
                params
                    .iter()
                    .map(|param| {
                        analyzed
                            .checked
                            .binders
                            .get(&param.name.span)
                            .map(|fact| fact.ty)
                    })
                    .collect::<Option<Vec<_>>>()
            })
        })
    })
}

/// How the entry's parameters stage, by their checked types' member
/// names: the scalar atoms stage their word, `int | none` stages its
/// two-word union span.
fn arg_plans(analyzed: &Analyzed, params: &[Ty]) -> Vec<ArgPlan> {
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
                ["bool"] => ArgPlan::Word(Bool),
                ["float"] => ArgPlan::Word(F64),
                ["int"] => ArgPlan::Word(I64),
                ["int", "none"] => ArgPlan::OptionalInt,
                _ => ArgPlan::Foreign,
            }
        })
        .collect()
}
