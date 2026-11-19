//! End-to-end `ds fmt` through the binary: library files rewrite in place
//! and the second run is a no-op, script entry files fold to the written
//! `fn main` shape and round-trip (their `##` separators surviving), syntax
//! errors render structured diagnostics without touching the file, and
//! `--check` gates CI without writing.

use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Output},
};

fn ds_fmt(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ds"))
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("the ds binary builds with the test")
}

/// One scratch directory per test, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = env::temp_dir().join(format!("ds-cli-fmt-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("the temp dir creates");
        Self(dir)
    }

    fn write(&self, name: &str, source: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, source).expect("the scratch source writes");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A messy library file rewrites in place; the second run reports
/// unchanged and writes nothing (the content pins idempotence).
#[test]
fn a_library_file_formats_in_place_and_stays_there() {
    let scratch = Scratch::new("library");
    let path = scratch.write("lib.ds", "const one=1\nconst   two=2\n");
    let out = ds_fmt(&["fmt", path.to_str().expect("temp path is UTF-8")]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("formatted {}\n", path.display())
    );
    assert_eq!(
        fs::read_to_string(&path).expect("the file rewrote"),
        "const one = 1\nconst two = 2\n"
    );
    let out = ds_fmt(&["fmt", path.to_str().expect("temp path is UTF-8")]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("unchanged {}\n", path.display())
    );
    assert_eq!(
        fs::read_to_string(&path).expect("the file stays"),
        "const one = 1\nconst two = 2\n"
    );
}

/// One invocation over a changing and an already-formatted file reports
/// each file's own action and exits 0 when nothing failed.
#[test]
fn mixed_files_report_per_file_actions() {
    let scratch = Scratch::new("mixed");
    let messy = scratch.write("messy.ds", "const   x=1\n");
    let clean = scratch.write("clean.ds", "const x = 1\n");
    let out = ds_fmt(&[
        "fmt",
        messy.to_str().expect("temp path is UTF-8"),
        clean.to_str().expect("temp path is UTF-8"),
    ]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!(
            "formatted {}\nunchanged {}\n",
            messy.display(),
            clean.display()
        )
    );
    assert_eq!(
        fs::read_to_string(&messy).expect("the messy file rewrote"),
        "const x = 1\n"
    );
}

/// A script entry file folds into the written `fn main` (the R20 synthetic
/// path); the formatted output re-parses and re-formats identically, and
/// its `##` separator survives on its own line.
#[test]
fn a_script_entry_folds_to_written_main_and_round_trips() {
    let scratch = Scratch::new("entry");
    let path = scratch.write("entry.ds", "const a = 1\n## section\nprintln(a + 1)\n");
    let out = ds_fmt(&["fmt", path.to_str().expect("temp path is UTF-8")]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("formatted {}\n", path.display())
    );
    assert_eq!(
        fs::read_to_string(&path).expect("the file rewrote"),
        "## section\nfn main() -> () {\n  const a = 1\n\n  println(a + 1)\n}\n"
    );
    // The written form is fixed point: library-mode parse, no rewrite.
    let out = ds_fmt(&["fmt", path.to_str().expect("temp path is UTF-8")]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("unchanged {}\n", path.display())
    );
}

/// Syntax errors render through the structured diagnostic path (one
/// `error[DSxxxx]` block), the file is untouched, and the exit is 1.
#[test]
fn syntax_errors_report_structured_diagnostics_and_touch_nothing() {
    let scratch = Scratch::new("broken");
    let path = scratch.write("broken.ds", "const x = \"open\n");
    let out = ds_fmt(&["fmt", path.to_str().expect("temp path is UTF-8")]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(out.stdout, [] as [u8; 0]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("error[DS"),
        "structured diagnostics expected: {stderr}"
    );
    assert!(stderr.contains("left unformatted"));
    assert_eq!(
        fs::read_to_string(&path).expect("the file is untouched"),
        "const x = \"open\n"
    );
}

/// `--check` writes nothing: a file that would change exits 1 naming the
/// file, an already-formatted one exits 0.
#[test]
fn check_mode_gates_without_writing() {
    let scratch = Scratch::new("check");
    let messy = scratch.write("messy.ds", "const   x=1\n");
    let messy_arg = messy.to_str().expect("temp path is UTF-8");
    let out = ds_fmt(&["fmt", "--check", messy_arg]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("would reformat {}\n", messy.display())
    );
    assert_eq!(
        fs::read_to_string(&messy).expect("nothing was written"),
        "const   x=1\n"
    );
    fs::write(&messy, "const x = 1\n").expect("the scratch source writes");
    let out = ds_fmt(&["fmt", "--check", messy_arg]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("unchanged {}\n", messy.display())
    );
}

/// Files that are not .ds sources are refused up front, like `ds run`.
#[test]
fn a_non_ds_extension_is_refused() {
    let scratch = Scratch::new("ext");
    let path = scratch.write("mod.dsmod", "const x = 1\n");
    let out = ds_fmt(&["fmt", path.to_str().expect("temp path is UTF-8")]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("ds fmt formats .ds sources"));
}

/// A CRLF file keeps its line endings: the messy one formats to the
/// canonical CRLF spelling (never an LF rewrite), and the canonical CRLF
/// file is byte-stable — plain `fmt` and `--check` both report it
/// unchanged, so a `core.autocrlf` workspace passes the CI gate.
#[test]
fn crlf_files_format_to_crlf_and_stay_there() {
    let scratch = Scratch::new("crlf");
    let messy = scratch.write("crlf.ds", "const   x=1\r\nconst  y=2\r\n");
    let arg = messy.to_str().expect("temp path is UTF-8");
    let out = ds_fmt(&["fmt", arg]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("formatted {}\n", messy.display())
    );
    assert_eq!(
        fs::read_to_string(&messy).expect("the messy file rewrote"),
        "const x = 1\r\nconst y = 2\r\n"
    );
    let out = ds_fmt(&["fmt", arg]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("unchanged {}\n", messy.display())
    );
    assert_eq!(
        fs::read_to_string(&messy).expect("the file stays CRLF"),
        "const x = 1\r\nconst y = 2\r\n"
    );
    let out = ds_fmt(&["fmt", "--check", arg]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    // A bare-LF file beside the CRLF one stays LF: the dominant rule is
    // per file, not per invocation.
    let lf = scratch.write("lf.ds", "const x = 1\n");
    let out = ds_fmt(&["fmt", "--check", lf.to_str().expect("temp path is UTF-8")]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("unchanged {}\n", lf.display())
    );
}
