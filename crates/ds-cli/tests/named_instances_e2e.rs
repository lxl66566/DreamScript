//! The named-instance vocabulary end to end through the `ds` binary (the
//! B6 two-impls escape): two impls of one trait over one head — the
//! unnamed row answering every plain spelling as before, the named rows
//! answering only their selected call sites — the qualified spelling
//! selecting the same instance, a generic impl's selected dispatch riding
//! the runtime-evidence machinery, and the unselected call on a
//! named-only head refusing with the note that names the selection the
//! vocabulary offers.

use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Output},
};

mod support;

/// The binary with a private working directory and script-cache root.
fn ds_in(dir: &PathBuf) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command.current_dir(dir);
    command.env("DS_CACHE", dir.join("cache"));
    command
}

fn workdir(name: &str) -> PathBuf {
    support::guarded_dir(&format!("ds-cli-named-instances-{name}-"))
}

fn run(command: &mut Command) -> Output {
    command.output().expect("the binary runs")
}

/// Writes one source into the working directory and runs it.
fn run_source(name: &str, source: &str) -> Output {
    let dir = workdir(name);
    let _ = fs::create_dir_all(&dir);
    fs::write(dir.join("app.ds"), source).expect("the source writes");
    let out = run(ds_in(&dir).arg("run").arg("app.ds"));
    let _ = fs::remove_dir_all(&dir);
    out
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The B6 escape itself: one unnamed and two named impls of one trait
/// over one head — the plain spelling keeps the unnamed row's answer,
/// each selection answers its own named body, and the qualified
/// spelling selects the same instance.
#[test]
fn two_impls_of_one_head_select_at_call_sites() {
    let out = run_source(
        "same-head",
        concat!(
            "trait tone {\n",
            "  fn hum(self) -> string\n",
            "}\n",
            "struct widget { n: int }\n",
            "impl tone for widget {\n",
            "  fn hum(self) -> string { \"plain\" }\n",
            "}\n",
            "impl tone for widget as fast {\n",
            "  fn hum(self) -> string { \"fast\" }\n",
            "}\n",
            "impl tone for widget as slow {\n",
            "  fn hum(self) -> string { \"slow\" }\n",
            "}\n",
            "fn main() {\n",
            "  const w = widget { n: 1 }\n",
            "  const a = w.hum()\n",
            "  const b = w.hum() as fast\n",
            "  const c = tone::hum(w) as slow\n",
            "  println(\"${a} ${b} ${c}\")\n",
            "}\n",
        ),
    );
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(text(&out.stdout), "plain fast slow\n0\n");
}

/// A selected call into a generic named impl dispatches through the
/// same runtime-evidence machinery the unnamed spelling rides: the
/// instance resolves in its slot, the body's own nested bound resolves
/// in the default vocabulary, and the answer composes.
#[test]
fn a_generic_named_impl_selects_and_composes() {
    let out = run_source(
        "generic-named",
        concat!(
            "trait label {\n",
            "  fn label(self) -> string\n",
            "}\n",
            "struct wrap[t] { v: t }\n",
            "impl label for int {\n",
            "  fn label(self) -> string { \"int\" }\n",
            "}\n",
            "impl[t: label] label for wrap[t] as deep {\n",
            "  fn label(self) -> string { \"<\" + self.v.label() + \">\" }\n",
            "}\n",
            "fn main() {\n",
            "  const w: wrap[int] = wrap { v: 3 }\n",
            "  println(w.label() as deep)\n",
            "}\n",
        ),
    );
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(text(&out.stdout), "<int>\n0\n");
}

/// An unselected call over a named-only head keeps the ordinary missing
/// instance refusal, with the note naming the selection the vocabulary
/// offers — the default vocabulary never sees the named row.
#[test]
fn a_named_only_head_refuses_the_plain_spelling_with_the_selection_note() {
    let out = run_source(
        "named-only",
        concat!(
            "trait tone {\n",
            "  fn hum(self) -> string\n",
            "}\n",
            "struct widget { n: int }\n",
            "impl tone for widget as fast {\n",
            "  fn hum(self) -> string { \"fast\" }\n",
            "}\n",
            "fn main() {\n",
            "  const w = widget { n: 1 }\n",
            "  println(w.hum())\n",
            "}\n",
        ),
    );
    assert_eq!(out.status.code(), Some(1), "the run must refuse: {out:?}");
    let err = text(&out.stderr);
    assert!(err.contains("DS2013"), "the refusal keeps its code: {err}");
    assert!(
        err.contains("this receiver's impls are named; a call site selects one with `as fast`"),
        "the note names the selection: {err}"
    );
}

/// The status quo pin: a package whose impls are all unnamed resolves
/// every spelling exactly as before the vocabulary existed — the plain
/// dot call, the free-form UFCS reading, and the qualified spelling all
/// answer the one row.
#[test]
fn all_unnamed_packages_keep_the_plain_resolution() {
    let out = run_source(
        "all-unnamed",
        concat!(
            "trait tone {\n",
            "  fn hum(self) -> string\n",
            "}\n",
            "struct widget { n: int }\n",
            "impl tone for widget {\n",
            "  fn hum(self) -> string { \"plain\" }\n",
            "}\n",
            "fn main() {\n",
            "  const w = widget { n: 1 }\n",
            "  println(\"${w.hum()} ${tone::hum(w)}\")\n",
            "}\n",
        ),
    );
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(text(&out.stdout), "plain plain\n0\n");
}
