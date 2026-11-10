//! The `ds check` subcommand: the M2 static story over one package. The
//! given sources register in a query database and every check runs as a
//! memoized query — parse (syntax errors are data), resolve, the
//! bidirectional type check with refinement (B2), the affine pass
//! [C1, C5], and the effect rows (E1) (computed, informational until the
//! M7 platform check decides what they forbid). Diagnostics print through
//! the ds-diag renderers; the exit code is 0 when nothing fatal was
//! reported.
//!
//! The package is the stdlib ambient plus the files given (G3): the M3
//! ambient (ds-stdlib) registers as a package member before the user's
//! files, so corpus sources check clean as-is. `--no-ambient` checks the
//! files alone — for self-contained packages whose declarations would
//! collide with the ambient's single namespace.
//!
//! Fatality scoping mirrors the conformance gate
//! ([`ds_check::corpus`]): in a file carrying the corpus `// items:`
//! header, diagnostics confined to items staged away from `typecheck`
//! (run leftovers, intended rejections, later-milestone constructs) are
//! corpus-owned noise — printed, never fatal. Plain files are fully
//! fatal: nothing is staged.

use std::{path::PathBuf, process::ExitCode};

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use ds_check::{DefKind, corpus};
use ds_diag::{Diagnostic, LabelStyle, render};
use ds_query::{
    BlockIndex, Db, DefId, EffectsOf, LinearityCheck, PackageId, ParseFile, QueryError, Resolve,
    TypeOf,
};

/// The `ds check` surface: the package's source files and the diagnostic
/// rendering.
#[derive(Debug, Args)]
pub struct CheckArgs {
    /// One .ds source file of the package; the files share one namespace (G3)
    files: Vec<PathBuf>,
    /// Check the files alone, without the stdlib ambient package member
    #[arg(long)]
    no_ambient: bool,
    /// How diagnostics render
    #[arg(long, value_enum, default_value = "human")]
    error_format: ErrorFormat,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ErrorFormat {
    /// Plain text with source excerpts (the ds-diag human renderer)
    Human,
    /// One JSON object per line (the ds-diag json renderer)
    Json,
}

/// One collected diagnostic plus its fatality.
struct Report {
    diag: Diagnostic,
    fatal: bool,
}

/// Checks one package: registers the sources, runs every M2 query over
/// them, prints the diagnostics, and reports fatality as the exit code.
pub fn check(args: &CheckArgs) -> Result<ExitCode> {
    let mut db = if args.no_ambient {
        Db::new()
    } else {
        // The ambient's effect rows [E1] ride the database's table; a
        // package without the ambient gets the pure default.
        Db::with_declared_rows(ds_stdlib::declared_rows())
    };
    let mut files = Vec::with_capacity(args.files.len() + 1);
    if !args.no_ambient {
        // The ambient is the package's first member: one namespace [G3],
        // its declarations before the user's in collection order.
        files.push(db.add_file(ds_stdlib::AMBIENT_FILE, ds_stdlib::AMBIENT_SOURCE));
    }
    for path in &args.files {
        let source =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        files.push(db.add_file(path.to_string_lossy().into_owned(), &source));
    }

    let mut reports = Vec::new();
    // Per-file item stages: the corpus scoping, `None` for plain files.
    let mut scopes: Vec<Option<Vec<corpus::ItemStage>>> = Vec::with_capacity(files.len());
    for &file in &files {
        let parse = query(db.get::<ParseFile>(file), "parse")?;
        reports.extend(parse.diagnostics.iter().map(|diag| Report {
            diag: diag.clone(),
            // Syntax errors always gate: no stage owns broken syntax.
            fatal: true,
        }));
        scopes.push(
            parse
                .ast
                .as_deref()
                .and_then(|ast| corpus::item_stages(&parse.source, ast)),
        );
    }

    let resolved = query(db.get::<Resolve>(PackageId::new(0)), "resolve")?;
    for diag in &resolved.diags {
        reports.push(Report {
            diag: diag.clone(),
            fatal: !exempt(&scopes, diag),
        });
    }

    for &file in &files {
        let index = query(db.get::<BlockIndex>(file), "block index")?;
        for block in &index.blocks {
            let def = DefId::new((file.as_u32() << 8) | block.id.index);
            let typed = query(db.get::<TypeOf>(def), "type_of")?;
            for diag in &typed.diags {
                reports.push(Report {
                    diag: diag.clone(),
                    fatal: !exempt(&scopes, diag),
                });
            }
            let linear = query(db.get::<LinearityCheck>(def), "linearity_check")?;
            for diag in &linear.diags {
                reports.push(Report {
                    diag: diag.clone(),
                    fatal: !exempt(&scopes, diag),
                });
            }
        }
    }

    // Effect rows [E1]: computed for information. Value-kind definitions
    // only — type-level declarations carry no row.
    for def in 0..resolved.package.len() {
        let raw = u32::try_from(def).expect("definition count fits u32");
        if matches!(
            resolved.package.def(ds_check::DefId::from_raw(raw)).kind,
            DefKind::Fn | DefKind::Const | DefKind::Var
        ) {
            query(db.get::<EffectsOf>(DefId::new(raw)), "effects_of")?;
        }
    }

    // Source order per file: the focus span of each diagnostic, pass order
    // among equals.
    reports.sort_by_key(|report| focus(&report.diag));
    for report in &reports {
        match args.error_format {
            ErrorFormat::Human => eprint!("{}", render::human(&report.diag, db.files())),
            ErrorFormat::Json => eprintln!("{}", render::to_json(&report.diag, db.files())),
        }
    }

    if reports.iter().any(|report| report.fatal) {
        Ok(ExitCode::FAILURE)
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

/// Sort key of one diagnostic: the focus span (primary label, else
/// first), grouping by file then source position; a labelless diagnostic
/// sorts last.
fn focus(diag: &Diagnostic) -> (u32, u32, u32) {
    diag.labels
        .iter()
        .find(|label| label.style == LabelStyle::Primary)
        .or_else(|| diag.labels.first())
        .map_or((u32::MAX, u32::MAX, u32::MAX), |label| {
            (label.span.file.as_u32(), label.span.start, label.span.end)
        })
}

/// Whether every label of the diagnostic lies inside an item the corpus
/// stages away from `typecheck` — run leftovers, intended rejections,
/// later-milestone constructs. Only `typecheck` items gate M2 checking;
/// a file with no `// items:` header stages nothing, so its diagnostics
/// are never exempt.
fn exempt(scopes: &[Option<Vec<corpus::ItemStage>>], diag: &Diagnostic) -> bool {
    diag.labels.iter().all(|label| {
        scopes
            .get(label.span.file.as_u32() as usize)
            .and_then(Option::as_deref)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    label.span.file == item.span.file
                        && label.span.start >= item.span.start
                        && label.span.end <= item.span.end
                        && item.stage != "typecheck"
                })
            })
    })
}

/// One query failure, self-describing with the pass that failed.
fn query<T>(result: Result<T, QueryError>, pass: &str) -> Result<T> {
    result.with_context(|| format!("{pass} query failed"))
}
