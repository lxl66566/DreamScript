//! The spellings @std declares reach a non-entry library package: a
//! path dep's member compiles against the ambient alone (the host
//! provision registers for an entry session only), so the fsys path
//! round lowers and runs from library source and `ds check` over the
//! same member checks clean where the names stayed unknown under the
//! host-only placement.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

mod support;

/// The library member: the fsys path round's states over one directory,
/// each answer mapped to its class or its success word.
const PATH_MEMBER: &str = concat!(
    "fn class(e: io_err) -> string {\n",
    "  match e {\n",
    "    not_found => \"not_found\"\n",
    "    permission_denied => \"permission_denied\"\n",
    "    already_exists => \"already_exists\"\n",
    "    timed_out => \"timed_out\"\n",
    "    unexpected_eof => \"unexpected_eof\"\n",
    "    io_other { msg } => \"io_other\"\n",
    "  }\n",
    "}\n",
    "\n",
    "pub fn states(dir: string) -> string {\n",
    "  const fresh = mkdir(dir).ok((_: ()) => \"made\").err((e: io_err) => class(e))\n",
    "  const again = mkdir(dir).ok((_: ()) => \"made\").err((e: io_err) => class(e))\n",
    "  const inner = mkdir(\"${dir}/inner\").ok((_: ()) => \"made\").err((e: io_err) => \
     class(e))\n",
    "  const full = remove(dir).ok((_: ()) => \"removed\").err((e: io_err) => class(e))\n",
    "  const emptied = remove(\"${dir}/inner\").ok((_: ()) => \"removed\").err((e: io_err) => \
     class(e))\n",
    "  const swept = remove(dir).ok((_: ()) => \"removed\").err((e: io_err) => class(e))\n",
    "  const back = fs_size(dir).ok((n: int) => \"size ${n}\").err((e: io_err) => class(e))\n",
    "  \"${fresh} ${again} ${inner} ${full} ${emptied} ${swept} ${back}\"\n",
    "}\n",
);

/// One guarded work tree holding a path-dep library package beside a
/// root package whose entry selects the member's `states`.
fn tree(tag: &str, member: &str) -> (PathBuf, PathBuf) {
    let work = support::guarded_dir(&format!("ds-cli-lib-provision-{tag}-"));
    let lib = work.join("lib");
    fs::create_dir_all(&lib).expect("the library dir creates");
    fs::write(lib.join("ds.pkg"), "name = \"@lib\"\nversion = \"1.0.0\"\n")
        .expect("the library manifest writes");
    fs::write(lib.join("helper.ds"), member).expect("the library member writes");
    let root = work.join("root");
    fs::create_dir_all(&root).expect("the root dir creates");
    fs::write(
        root.join("ds.pkg"),
        "name = \"app\"\nversion = \"0.1.0\"\n\n[deps.@lib]\npath = \"../lib\"\n",
    )
    .expect("the root manifest writes");
    fs::write(
        root.join("app.ds"),
        concat!(
            "import @lib/helper (states)\n",
            "\n",
            "fn main() -> () {\n",
            "  println(states(\"lib_states.d\"))\n",
            "}\n",
        ),
    )
    .expect("the root member writes");
    (work, root)
}

/// Runs the binary with the root package as the working directory and a
/// private store.
fn run(root: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command
        .current_dir(root)
        .env("DS_CACHE", root.join("cache"))
        .args(args);
    command.output().expect("the binary runs")
}

/// The library member's fsys path round compiles against the ambient
/// alone and answers end to end through the root's call: the create,
/// its already_exists branch, the non-empty directory's io_other floor,
/// both removals, and the missing path's not_found read.
#[test]
fn the_fsys_path_round_reaches_a_library_package() {
    let (work, root) = tree("path", PATH_MEMBER);
    let out = run(&root, &["run", "app.ds"]);
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        concat!(
            "made already_exists made io_other removed removed not_found\n",
            "",
        ),
        "the library member's path round answers through the root's call"
    );

    // the member alone checks clean: the names resolve from the ambient
    // with no entry session and no host provision
    let out = run(&root, &["check", "../lib/helper.ds"]);
    assert!(out.status.success(), "the member checks: {out:?}");
    assert_eq!(out.stderr, b"", "the check is silent");

    fs::remove_dir_all(&work).ok();
}

/// The ambient `panic` spelling reaches the library member: the dep
/// compiles against the ambient with no entry session, the deliberate
/// bug halts the run through the trap report naming the library frame,
/// and the member alone checks clean — the die split's core half on
/// the library surface, where the host's `die` never reaches.
#[test]
fn the_ambient_panic_reaches_a_library_package() {
    let (work, root) = tree(
        "panic",
        "pub fn boom(msg: string) -> () {\n  panic(msg)\n  return ()\n}\n",
    );
    fs::write(
        root.join("app.ds"),
        concat!(
            "import @lib/helper (boom)\n",
            "\n",
            "fn main() -> () {\n",
            "  println(\"before\")\n",
            "  boom(\"lib boom\")\n",
            "  println(\"unreachable\")\n",
            "}\n",
        ),
    )
    .expect("the root member writes");
    let out = run(&root, &["run", "app.ds"]);
    assert_eq!(out.status.code(), Some(1), "the panic halts the run");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "before\n",
        "nothing past the panic runs"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("trap: die: lib boom") && err.contains("@lib::boom"),
        "the trap report names the library frame: {err}"
    );

    // the member alone checks clean: the spelling resolves from the
    // ambient with no entry session and no host provision
    let out = run(&root, &["check", "../lib/helper.ds"]);
    assert!(out.status.success(), "the member checks: {out:?}");
    assert_eq!(out.stderr, b"", "the check is silent");

    fs::remove_dir_all(&work).ok();
}
