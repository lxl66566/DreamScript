//! The F9.5 cross-file tag shadow end to end through the `ds` binary: a
//! two-member package where the second member re-declares the first's
//! tag label. The directed probe resolves each bare label use against
//! the expected union's members, so the value the shadowing member
//! constructs and the arm the first member matches must agree on the
//! same declaration's atom at run — the checker's per-use record is
//! the lowering's truth, never the first namespace holder.

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
