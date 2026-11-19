//! The `ds` binary: DreamScript's command-line interface.
//!
//! `ds check` runs the static story over .ds sources, `ds run` executes
//! one module — a dsmod file, or a .ds source compiled in-process —, `ds
//! build` compiles a .ds source to a dsmod module, `ds save` installs a
//! working-tree package into the content-addressed store, `ds fmt`
//! rewrites .ds sources in place, `ds info` renders a package's `.dsi`
//! API surface, `ds bind` binds an installed package artifact's native
//! library and reports its exported C symbols (H5), `ds repl`
//! reads-eval-prints against the in-process pipeline with a persistent
//! session, `ds lsp` serves the language-server protocol on
//! stdin/stdout, and `ds dev` drives the hot-reload loop (compile,
//! watch, swap). Every further subcommand (link) lands when the backend
//! it drives lands — no stub commands.

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod ambient_dsmod;
mod bind;
mod build;
mod check;
mod dev;
mod dsi_cache;
mod fmt;
mod info;
mod lsp;
mod package;
mod repl;
mod run;
mod run_cache;
mod save;
mod source;

#[derive(Debug, Parser)]
#[command(
    name = "ds",
    version,
    about = "DreamScript command-line interface",
    allow_negative_numbers = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Check .ds sources with the M2 static story
    Check(check::CheckArgs),
    /// Execute a dsmod module or a .ds source
    Run(run::RunArgs),
    /// Compile a .ds source to a dsmod module
    Build(build::BuildArgs),
    /// Install a working-tree package into the content-addressed store
    Save(save::SaveArgs),
    /// Render a package's `.dsi` API surface (G4)
    Info(info::InfoArgs),
    /// Bind an installed package artifact's native library and report
    /// its exported C symbols (H5)
    Bind(bind::BindArgs),
    /// Format .ds sources in place (zero style options)
    Fmt(fmt::FmtArgs),
    /// Run one .ds source with hot reload: watch, recompile, swap
    Dev(dev::DevArgs),
    /// Interactive read-eval-print loop over the in-process pipeline
    Repl(repl::ReplArgs),
    /// Serve the language-server protocol on stdin/stdout
    Lsp(lsp::LspArgs),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Check(args) => check::check(&args),
        Command::Run(args) => run::run(&args).map(|()| ExitCode::SUCCESS),
        Command::Build(args) => build::build(&args).map(|()| ExitCode::SUCCESS),
        Command::Save(args) => save::save(&args).map(|()| ExitCode::SUCCESS),
        Command::Info(args) => info::info(&args).map(|()| ExitCode::SUCCESS),
        Command::Bind(args) => bind::bind(&args).map(|()| ExitCode::SUCCESS),
        Command::Fmt(args) => Ok(fmt::fmt(&args)),
        Command::Dev(args) => dev::dev(&args),
        Command::Repl(args) => repl::repl(&args),
        Command::Lsp(args) => lsp::lsp(&args),
    };
    match result {
        // the subcommand reports its own exit code (diagnostics vs clean)
        Ok(code) => code,
        // every failure path is self-describing (missing files, query
        // engine errors, structured diagnostics already open with
        // `error[DSxxxx]`), so no extra prefix
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        },
    }
}
