//! The `ds` binary: DreamScript's command-line interface.
//!
//! `ds check` runs the static story over .ds sources, `ds run` executes
//! one module — a dsmod file, or a .ds source compiled in-process —, `ds
//! build` compiles a .ds source to a dsmod module, and `ds fmt` rewrites
//! .ds sources in place. Every further subcommand (info, dev, save, bind,
//! link) lands when the backend it drives lands — no stub commands.

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod build;
mod check;
mod fmt;
mod run;
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
    /// Format .ds sources in place (zero style options)
    Fmt(fmt::FmtArgs),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Check(args) => check::check(&args),
        Command::Run(args) => run::run(&args).map(|()| ExitCode::SUCCESS),
        Command::Build(args) => build::build(&args).map(|()| ExitCode::SUCCESS),
        Command::Fmt(args) => Ok(fmt::fmt(&args)),
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
