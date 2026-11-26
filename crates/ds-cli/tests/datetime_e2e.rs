//! The datetime face end to end through the `ds` binary (todo item
//! 101's datetime clause): the pure calendar math and the fixed-shape
//! ISO spelling as @std ds source over unix seconds, the one world
//! touch the ambient's `utc_offset` op answers (the host's local UTC
//! offset, seconds east), and the local composition adding the word
//! before the same pure walk. The refusals keep the world-op class
//! honest: a host without the time capability refuses the offset call
//! site at install like every world op. A std-declaring graph keeps
//! the spellings alive too: the ambient core declares the extern and
//! the datetime members ride the bundle's own member file, so the
//! package compile formats over the same seam.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use ds_vm::platform::OP_UTC_OFFSET;

mod support;

/// The binary with the manifest root's cwd and a private script-cache
/// root: spawned runs never read or write a real user cache directory.
fn ds() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command.current_dir(env!("CARGO_MANIFEST_DIR"));
    command.env("DS_CACHE", test_cache());
    command
}

/// A private per-thread script-cache root: the thread's exit removes it.
fn test_cache() -> PathBuf {
    support::shared_dir("ds-cli-datetime-cache-")
}

/// Runs the command and flattens the outcome.
fn run(command: &mut Command) -> Output {
    command.output().expect("the binary runs")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
        .to_str()
        .expect("fixture path is UTF-8")
        .to_owned()
}

/// One package root (a `ds.pkg` manifest beside the members) under a
/// fresh temp dir, answering the root's path.
fn write_root(tag: &str, manifest: &str, members: &[(&str, &str)]) -> PathBuf {
    let root = support::guarded_dir(&format!("ds-cli-datetime-{tag}-"));
    fs::write(root.join("ds.pkg"), manifest).expect("the manifest writes");
    for (path, body) in members {
        fs::write(root.join(path), body).expect("the member writes");
    }
    root
}

/// The std-declaring root's manifest: the default @std bundle at its
/// own content-identity.
fn std_manifest() -> String {
    let package = ds_stdlib::std_package();
    let identity = package.content_hash();
    let version = package
        .manifest(ds_diag::FileId::new(0))
        .expect("the default bundle's manifest parses")
        .version
        .to_string();
    format!(
        "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@std = {{ ver = \"{version}\", hash = \
         \"blake3:{identity}\" }}\n"
    )
}

/// The acceptance run's expected output: the calendar walk over the
/// known days (the leap rules across the whole 400-year cycle, both
/// i64 edges) and the bounded offset word.
const FACE_OUT: &str = concat!(
    "1970-01-01T00:00:00\n",
    "1969-12-31T23:59:59\n",
    "1970-01-01T23:59:59\n",
    "1970-01-02T00:00:00\n",
    "1969-12-31T00:00:00\n",
    "2000-02-29T00:00:00\n",
    "2024-02-29T00:00:00\n",
    "1900-03-01T00:00:00\n",
    "2100-03-01T00:00:00\n",
    "9999-12-31T23:59:59\n",
    "10000-01-01T00:00:00\n",
    "0001-01-01T00:00:00\n",
    "-292277022657-01-27T08:29:52\n",
    "292277026596-12-04T15:30:07\n",
    "fields 2026 11 24 0 0 0 2\n",
    "offset bounded\n",
    "",
);

/// The acceptance: the pure calendar and the fixed-shape spelling run
/// through the served ambient, cold and warm under the same answer,
/// and the offset word stays inside the day.
#[test]
fn the_datetime_face_runs_through_the_bare_script() {
    let entry = fixture("datetime_face.ds");
    for pass in ["cold", "warm"] {
        let out = run(ds().arg("run").arg(&entry));
        assert_eq!(
            out.status.code(),
            Some(0),
            "the {pass} run exits 0: {}",
            text(&out.stderr)
        );
        assert_eq!(text(&out.stdout), FACE_OUT, "the {pass} run answers");
    }
}

/// The local face under a forced POSIX TZ: the child process reads the
/// zone at its first offset call, so `TZ=XXX-5` (five hours east, no
/// DST rules) pins the offset word and the shifted spelling. Linux
/// alone: the string's meaning is the glibc/musl contract.
#[cfg(target_os = "linux")]
#[test]
fn the_local_face_reads_the_forced_zone() {
    let out = run(ds()
        .arg("run")
        .arg(fixture("datetime_local_face.ds"))
        .env("TZ", "XXX-5"));
    assert_eq!(
        out.status.code(),
        Some(0),
        "the forced-zone run exits 0: {}",
        text(&out.stderr)
    );
    assert_eq!(
        text(&out.stdout),
        concat!(
            "offset 18000\n",
            "1970-01-01T05:00:00\n",
            "2000-02-29T05:00:00\n",
            "local 5 2\n",
            "",
        ),
        "the forced-zone run answers"
    );
}

/// The std-declaring graph keeps the face alive: the ambient core
/// declares the extern and the datetime members ride the bundle's own
/// member file, so the package compile formats over the import
/// selection from the linked surface.
#[test]
fn a_std_declared_root_formats_over_the_datetime_member() {
    let app = concat!(
        "import @std (utc_datetime, datetime_str)\n",
        "\n",
        "fn utc (secs: int) -> string {\n",
        "  datetime_str(utc_datetime(secs))\n",
        "}\n",
        "\n",
        "fn main () -> int {\n",
        "  println(utc(0))\n",
        "  println(utc(951782400))\n",
        "  println(utc(9223372036854775807))\n",
        "  0\n",
        "}\n",
    );
    let root = write_root("member", &std_manifest(), &[("app.ds", app)]);
    let entry = root.join("app.ds");
    let out = run(ds()
        .arg("run")
        .arg(entry.to_str().expect("temp path is UTF-8")));
    assert_eq!(
        out.status.code(),
        Some(0),
        "the std-declared run exits 0: {}",
        text(&out.stderr)
    );
    assert_eq!(
        text(&out.stdout),
        concat!(
            "1970-01-01T00:00:00\n",
            "2000-02-29T00:00:00\n",
            "292277026596-12-04T15:30:07\n",
            "",
        ),
        "the std-declared run answers"
    );
    fs::remove_dir_all(&root).expect("the temp tree removes");
}

/// A time-capability claim under the console-only embed handler
/// refuses at install: the offset op's call site names the unprovided
/// capability, the world-op class's structured refusal.
#[test]
fn an_embed_source_refuses_the_offset_op_at_install() {
    let out = run(ds()
        .arg("run")
        .arg("--no-ambient")
        .arg(fixture("embed_time_refused.ds")));
    assert!(
        !out.status.success(),
        "the offset call site under embed exits nonzero"
    );
    assert_eq!(
        text(&out.stdout),
        "",
        "nothing runs before the install refuses"
    );
    assert!(
        text(&out.stderr).contains(&format!(
            "calls world op {OP_UTC_OFFSET} (`time`), which the platform `embed` does not provide"
        )),
        "the install names the unprovided capability: {}",
        text(&out.stderr)
    );
}
