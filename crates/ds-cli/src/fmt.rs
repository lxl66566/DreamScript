//! The `ds fmt` subcommand: rewrite .ds sources in place through the
//! zero-option formatter. Each file parses exactly as `ds check` parses it
//! (library mode); a file library mode rejects as top-level statements
//! parses as the script entry — the fold whose output spells a written
//! `fn main() -> () { ... }`, so both shapes format with no flag. Syntax
//! failures render through the ds-diag human renderer and the file is left
//! untouched; the exit code is 1 when any file failed or, under `--check`,
//! when any file would change.

use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{Context, Result, anyhow};
use clap::Args;
use ds_diag::{Diagnostic, Files, render};
use ds_fmt::format_source;
use ds_parse::{parse, parse_script};

/// The `ds fmt` surface: the files and the CI gate. No style options — the
/// formatter's contract is zero of them.
#[derive(Debug, Args)]
pub struct FmtArgs {
    /// The .ds source files to format in place
    #[arg(required = true)]
    files: Vec<PathBuf>,
    /// Report the files that would change without writing; exit 1 when any would
    #[arg(long)]
    check: bool,
}

/// What one invocation did to one file.
enum Action {
    /// Rewritten in place.
    Formatted,
    /// Already canonical; nothing written.
    Unchanged,
    /// Would change; `--check` wrote nothing.
    WouldReformat,
}

impl Action {
    fn label(self) -> &'static str {
        match self {
            Self::Formatted => "formatted",
            Self::Unchanged => "unchanged",
            Self::WouldReformat => "would reformat",
        }
    }
}

/// Formats every given file, reporting one action line per file (stdout);
/// a failed file renders its diagnostics (stderr) but never stops the
/// others. Exit 1 when any file failed or, under `--check`, would change.
pub fn fmt(args: &FmtArgs) -> ExitCode {
    let mut clean = true;
    for path in &args.files {
        match format_file(path, args.check) {
            Ok(action) => {
                if matches!(action, Action::WouldReformat) {
                    clean = false;
                }
                println!("{} {}", action.label(), path.display());
            },
            // The diagnostics behind the summary are already on stderr.
            Err(error) => {
                clean = false;
                eprintln!("{error:#}");
            },
        }
    }
    if clean { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// Formats one file: parse (library mode, the script entry as the
/// fallback), format, and write back only when the output differs — an
/// already-formatted file is never touched.
fn format_file(path: &Path, check_only: bool) -> Result<Action> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    if ext != "ds" {
        return Err(anyhow!(
            "`{}` has extension `{ext}`; ds fmt formats .ds sources",
            path.display()
        ));
    }
    let source =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut files = Files::new();
    let id = files.add_source(path.to_string_lossy().into_owned(), &source);
    // Library mode is the default (the `ds check` shape); only a file it
    // rejects as top-level statements formats as the entry, so a plain
    // top-level `const` stays the declaration it is in a package.
    let ast = match parse(&files, id) {
        Ok(ast) => ast,
        Err(_) => parse_script(&files, id).map_err(|diags| reject(&files, &diags, path))?,
    };
    let formatted = format_source(&files, id, &ast);
    // The comparison and the write-back both speak the source's dominant
    // line ending: the canonical form is about layout, not EOL style, and
    // a CRLF workspace file is byte-stable under fmt instead of being
    // rewritten to LF wholesale.
    let written = with_source_newlines(&source, formatted);
    if written == source {
        return Ok(Action::Unchanged);
    }
    if check_only {
        return Ok(Action::WouldReformat);
    }
    std::fs::write(path, &written).with_context(|| format!("writing {}", path.display()))?;
    Ok(Action::Formatted)
}

/// Re-spells `text` with the source's dominant line ending: the count of
/// CRLF breaks vs bare-LF ones picks the winner (ties and an all-LF file
/// stay LF, the formatter's native spelling). Verbatim string literals
/// can carry either spelling, so the text folds to LF before re-emitting
/// the winner — CRLF content round-trips byte for byte.
fn with_source_newlines(source: &str, text: String) -> String {
    let bytes = source.as_bytes();
    let (mut crlf, mut lf) = (0usize, 0usize);
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\r' if bytes.get(i + 1) == Some(&b'\n') => {
                crlf += 1;
                i += 2;
            },
            b'\n' => {
                lf += 1;
                i += 1;
            },
            _ => i += 1,
        }
    }
    if crlf > lf { text.replace("\r\n", "\n").replace('\n', "\r\n") } else { text }
}

/// Renders every syntax diagnostic to stderr and summarizes the report as
/// one error; the rendered diagnostics are the message.
fn reject(files: &Files, diags: &[Diagnostic], path: &Path) -> anyhow::Error {
    for diag in diags {
        eprint!("{}", render::human(diag, files));
    }
    anyhow!(
        "{}: {} syntax diagnostic(s); left unformatted",
        path.display(),
        diags.len()
    )
}
