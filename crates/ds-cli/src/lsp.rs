//! The `ds lsp` subcommand: the language-server protocol on stdin and
//! stdout, the editor embedding of the same query surface the REPL and
//! `ds check` drive. The engine runs on a dedicated worker thread; this
//! subcommand only builds the runtime the protocol loop needs.

use std::process::ExitCode;

use anyhow::Result;
use clap::Args;

/// The `ds lsp` surface: no options in the first slice (the client's
/// `initialize` carries the protocol configuration).
#[derive(Debug, Args)]
pub struct LspArgs {}

/// Serves the protocol until the client shuts the server down or
/// disconnects.
///
/// # Errors
///
/// Runtime-construction and stream failures only; language-level
/// problems travel as published diagnostics, never as exits.
pub fn lsp(_args: &LspArgs) -> Result<ExitCode> {
    // The protocol loop is async while the CLI stays sync, so the
    // runtime lives and dies inside the subcommand.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(ds_lsp::serve(tokio::io::stdin(), tokio::io::stdout()))?;
    Ok(ExitCode::SUCCESS)
}
