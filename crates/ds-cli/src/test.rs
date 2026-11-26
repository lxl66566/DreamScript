//! The `ds test` subcommand: one package root's `tests/` directory, each
//! .ds file an independent script entry composed with the package's own
//! members (the member walk never lists `tests/`), run in ascending
//! order through one fresh `ds test --run-one` child per test. A test
//! passes when its run exits 0 — a trap report, a refused compile, or a
//! deliberate nonzero `exit(code)` fails it with its output shown under
//! a fence — the trailing summary line closes the run, and the exit is
//! 0 when all passed and 1 otherwise. A root without `tests/` answers a
//! bare `no tests`, exit 0. The conformance gate's per-file `// items:`
//! stage grammar stays the richer opt-in format for checked
//! expectations; this surface keeps the exit-code contract only.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    time::Instant,
};

use anyhow::{Context, Result, bail};
use clap::Args;

use crate::{package, run};

/// The `ds test` surface: one package root directory, discovery and
/// listing flags, and the runner's own one-test re-entry.
#[derive(Debug, Args)]
pub struct TestArgs {
    /// Package root directory holding the `tests/` scripts
    #[arg(default_value = ".")]
    root: PathBuf,
    /// Print the discovered tests, run nothing
    #[arg(long)]
    list: bool,
    /// Run exactly this one test file against the root's package (the
    /// runner spawns one per discovered test; not a hand surface)
    #[arg(long, hide = true)]
    run_one: Option<PathBuf>,
}

/// Runs the root's tests.
///
/// # Errors
///
/// A root that is not a directory, an unreadable `tests/` directory, a
/// failed spawn, and the re-entry's compile and run refusals.
pub fn test(args: &TestArgs) -> Result<ExitCode> {
    if let Some(one) = &args.run_one {
        return run_one(&args.root, one);
    }
    if !args.root.is_dir() {
        bail!(
            "`{}` is not a directory; ds test runs a package root's tests/ scripts",
            args.root.display()
        );
    }
    let tests = discover(&args.root)?;
    if args.list {
        for path in &tests {
            println!("{}", display_name(&args.root, path));
        }
        return Ok(ExitCode::SUCCESS);
    }
    if tests.is_empty() {
        println!("no tests");
        return Ok(ExitCode::SUCCESS);
    }
    let exe = std::env::current_exe().context("resolving the ds binary")?;
    let root = args
        .root
        .canonicalize()
        .with_context(|| format!("resolving {}", args.root.display()))?;
    let mut failed = 0usize;
    let mut failures: Vec<(String, Vec<u8>, Vec<u8>)> = Vec::new();
    for path in &tests {
        let name = display_name(&args.root, path);
        let file = path
            .canonicalize()
            .with_context(|| format!("resolving {}", path.display()))?;
        let started = Instant::now();
        let out = Command::new(&exe)
            .arg("test")
            .arg(&root)
            .arg("--run-one")
            .arg(&file)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("spawning the runner for {name}"))?;
        let elapsed = started.elapsed();
        let passed = out.status.success();
        if !passed {
            failed += 1;
            failures.push((name.clone(), out.stdout, out.stderr));
        }
        println!(
            "test {name} ... {} ({:.3}s)",
            if passed { "ok" } else { "FAILED" },
            elapsed.as_secs_f64()
        );
    }
    for (name, stdout, stderr) in &failures {
        println!("---- {name} ----");
        print_captured(stdout);
        print_captured(stderr);
    }
    let passed = tests.len() - failed;
    println!(
        "test result: {}. {passed} passed; {failed} failed; {} total",
        if failed == 0 { "ok" } else { "failed" },
        tests.len()
    );
    if failed == 0 { Ok(ExitCode::SUCCESS) } else { Ok(ExitCode::FAILURE) }
}

/// One test's run: the root's package graph (a manifest root resolves
/// exactly like `ds run` beside it, a manifest-free root the implicit
/// `@std` graph), the composed members, the package compile, and the
/// run tail. All output prints live; the exit code is the test's whole
/// verdict.
///
/// # Errors
///
/// An unreadable root, test file, or member, and every compile and run
/// refusal the composed run reports.
fn run_one(root: &Path, file: &Path) -> Result<ExitCode> {
    let graph = match package::open_root(root, false)? {
        Some(graph) => graph,
        None => package::implicit_std_graph(),
    };
    let members = package::test_members(root, file)?;
    let compiled = package::compile_test(file, &members, &graph)?;
    run::run_test_entry(compiled, &graph)
}

/// The root's `tests/*.ds` entries, ascending path order — the run's
/// deterministic order. The flat directory is the convention; deeper
/// nesting stays future surface.
fn discover(root: &Path) -> Result<Vec<PathBuf>> {
    let dir = root.join("tests");
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut tests = Vec::new();
    for entry in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry.with_context(|| format!("reading {}", dir.display()))?;
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("ds") {
            tests.push(path);
        }
    }
    tests.sort();
    Ok(tests)
}

/// The test's display name: the `/`-separated path below the root.
fn display_name(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// One captured stream under a failure's fence, verbatim, the missing
/// trailing newline restored.
fn print_captured(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    let text = String::from_utf8_lossy(bytes);
    if text.ends_with('\n') {
        print!("{text}");
    } else {
        println!("{text}");
    }
}
