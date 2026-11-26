//! The cross-file tag shadow end to end through the `ds` binary: a
//! two-member package where the second member re-declares the first's
//! tag label. The directed probe resolves each bare label use against
//! the expected union's members, so the value the shadowing member
//! constructs and the arm the first member matches must agree on the
//! same declaration's atom at run — the checker's per-use record is
//! the lowering's truth, never the first namespace holder. A user fn
//! sharing an ambient tag's label joins the same discipline: the call
//! form resolves to the fn, and the lowering enters the fn's own body.

use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Output},
};

mod support;

fn test_cache() -> PathBuf {
    support::shared_dir("ds-cli-tag-shadow-cache-")
}

/// Stages one two-member package: `a.ds` declares the first holder and
/// the consuming match, `b.ds` (the entry) its own shadow and the
/// constructions.
fn package(entry_body: &str) -> PathBuf {
    let dir = support::guarded_dir("ds-cli-tag-shadow-src-");
    fs::write(
        dir.join("ds.pkg"),
        "name = \"shadow\"\nversion = \"0.1.0\"\n",
    )
    .expect("the manifest writes");
    fs::write(
        dir.join("a.ds"),
        concat!(
            "pub tag refused\n",
            "pub type u = refused | io_err\n",
            "\n",
            "pub fn consume(v: u) -> () {\n",
            "  match v {\n",
            "    refused => println(\"a: refused\")\n",
            "    rest => println(\"a: other\")\n",
            "  }\n",
            "}\n",
        ),
    )
    .expect("the first member writes");
    fs::write(dir.join("b.ds"), entry_body).expect("the entry writes");
    dir.join("b.ds")
}

fn ds_run(entry: &PathBuf) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command.env("DS_CACHE", test_cache()).arg("run").arg(entry);
    command.output().expect("the ds binary runs")
}

/// Writes `source` to a scratch file and runs it as a bare script: the
/// file sits outside any package, so the ambient is the only other
/// declarer in the compile.
fn run_script(source: &str) -> Output {
    let dir = support::guarded_dir("ds-cli-tag-shadow-src-");
    let path = dir.join("shadow_fn.ds");
    fs::write(&path, source).expect("the script writes");
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command.env("DS_CACHE", test_cache()).arg("run").arg(&path);
    command.output().expect("the ds binary runs")
}

/// A value the shadowing file constructs under the first holder's union
/// carries the union member's atom, so the first file's own arm — the
/// remaining scrutinee's one refused — matches it.
#[test]
fn the_directed_value_crosses_to_the_first_members_arm() {
    let entry = package(concat!(
        "tag refused\n",
        "\n",
        "fn mk_b() -> u {\n",
        "  refused\n",
        "}\n",
        "\n",
        "fn main() -> () {\n",
        "  consume(mk_b())\n",
        "}\n",
    ));
    let out = ds_run(&entry);
    assert!(out.status.success(), "stderr: {:?}", out.stderr);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a: refused\n0\n");
}

/// The shadowing file's own alias carries its own atom: a value
/// constructed and matched inside it stays on the shadow's declaration.
#[test]
fn the_shadowing_file_reads_its_own_atom() {
    let entry = package(concat!(
        "tag refused\n",
        "type v = refused | int\n",
        "\n",
        "fn mk_v() -> v {\n",
        "  refused\n",
        "}\n",
        "\n",
        "fn main() -> () {\n",
        "  match mk_v() {\n",
        "    refused => println(\"b: refused\")\n",
        "    rest => println(\"b: other\")\n",
        "  }\n",
        "}\n",
    ));
    let out = ds_run(&entry);
    assert!(out.status.success(), "stderr: {:?}", out.stderr);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "b: refused\n0\n");
}

/// The qualified escape hatch at run: the second member constructs the
/// FIRST member's declaration through the head's qualified spelling,
/// so the consuming match arms see that member's atom.
#[test]
fn the_qualified_escape_constructs_the_first_members_atom() {
    let entry = package(concat!(
        "tag refused\n",
        "\n",
        "fn main() -> () {\n",
        "  consume(u::refused)\n",
        "  consume(refused)\n",
        "}\n",
    ));
    let out = ds_run(&entry);
    assert!(out.status.success(), "stderr: {:?}", out.stderr);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "a: refused\na: refused\n0\n"
    );
}

/// The shadow package's written artifact carries BOTH same-label rows —
/// one per declaring file, ids dense in first-use order — where a
/// name-keyed fold would drop one row at random. The warm run serving
/// the cached bytes back is the byte-determinism pin: a second process
/// reads the artifact the first wrote and answers identically.
#[test]
fn the_shadow_artifact_carries_both_rows_and_serves_warm() {
    let entry = package(concat!(
        "tag refused\n",
        "\n",
        "fn mk_b() -> u {\n",
        "  refused\n",
        "}\n",
        "\n",
        "fn main() -> () {\n",
        "  consume(mk_b())\n",
        "}\n",
    ));
    let first = ds_run(&entry);
    assert!(first.status.success(), "stderr: {:?}", first.stderr);
    assert_eq!(String::from_utf8_lossy(&first.stdout), "a: refused\n0\n");

    // the written module's tag table: two `refused` rows, distinct ids,
    // the declaring declaration's name recorded beside each label
    let cache = test_cache();
    let mut rows = Vec::new();
    let mut stack = vec![cache.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "dsmod") {
                let bytes = fs::read(&path).expect("the artifact reads");
                if let Ok(module) = ds_mod::read(&bytes) {
                    rows.extend(module.tags().iter().cloned());
                }
            }
        }
    }
    let refused: Vec<ds_mod::MemberTag> = rows
        .iter()
        .filter(|row| row.name == "refused")
        .cloned()
        .collect();
    assert_eq!(
        refused.len(),
        2,
        "both declaring files' rows ride the table: {rows:?}"
    );
    assert_ne!(refused[0].id, refused[1].id, "the atoms keep distinct ids");
    assert!(
        refused.iter().all(|row| row.decl == "refused"),
        "each row records its declaring declaration: {refused:?}"
    );

    // the warm serve: a second process reads the artifact back and the
    // run answers identically
    let second = ds_run(&entry);
    assert!(second.status.success(), "stderr: {:?}", second.stderr);
    assert_eq!(String::from_utf8_lossy(&second.stdout), "a: refused\n0\n");
    let _ = fs::remove_dir_all(&cache);
}

/// A user fn sharing the ambient `refused` tag's label checks clean and
/// lowers as the fn: the call form resolves to the value namespace's
/// fn (a tag family yields the call form to the value), and the plan
/// registers the fn under its own def, so the call enters the fn's body
/// and answers through it.
#[test]
fn a_fn_sharing_the_ambient_tags_label_calls_through_the_fn() {
    let out = run_script(concat!(
        "fn refused() -> int {\n",
        "  42\n",
        "}\n",
        "\n",
        "println(\"answer=${refused()}\")\n",
    ));
    assert!(out.status.success(), "stderr: {:?}", out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("answer=42"), "the fn answered: {stdout}");
}

/// A bounded generic fn sharing the label plans the same way: the body's
/// bounds collect under the fn's own def, so the entering site stages
/// the serving impl's evidence and the dispatch reaches the impl body.
#[test]
fn a_bounded_generic_fn_sharing_the_label_dispatches_its_bound() {
    let out = run_script(concat!(
        "trait shows {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "\n",
        "impl shows for int {\n",
        "  fn show(self) -> string { \"n=$self\" }\n",
        "}\n",
        "\n",
        "fn refused[t: shows](x: t) -> string {\n",
        "  x.show()\n",
        "}\n",
        "\n",
        "println(\"r=${refused(7)}\")\n",
    ));
    assert!(out.status.success(), "stderr: {:?}", out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("r=n=7"), "the dispatch answered: {stdout}");
}

/// A comptime fn sharing the label admits through its own def as well:
/// the evaluator collects the fn body under the value namespace's def,
/// the recorded answer embeds at the call site, and the lowering never
/// hears the collision.
#[test]
fn a_comptime_fn_sharing_the_label_embeds_its_answer() {
    let out = run_script(concat!(
        "fn refused(comptime t: type) -> string {\n",
        "  type_name(t)\n",
        "}\n",
        "\n",
        "struct pt { x: int }\n",
        "\n",
        "println(\"k=${refused(pt)}\")\n",
    ));
    assert!(out.status.success(), "stderr: {:?}", out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("k=pt"),
        "the comptime answer embedded: {stdout}"
    );
}

/// The cross-file package form: the entry's fn wins the call form over
/// the first member's same-label tag, the tag stays reachable through
/// the union's qualified spelling, and the first member's own match arm
/// still sees the tag's atom.
#[test]
fn the_entry_fn_wins_the_call_form_and_the_tag_stays_reachable() {
    let entry = package(concat!(
        "fn refused() -> int {\n",
        "  42\n",
        "}\n",
        "\n",
        "fn mk() -> u {\n",
        "  u::refused\n",
        "}\n",
        "\n",
        "fn main() -> () {\n",
        "  println(\"answer=${refused()}\")\n",
        "  consume(mk())\n",
        "}\n",
    ));
    let out = ds_run(&entry);
    assert!(out.status.success(), "stderr: {:?}", out.stderr);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "answer=42\na: refused\n0\n"
    );
}
