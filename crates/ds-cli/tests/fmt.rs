//! End-to-end `ds fmt` through the binary: library files rewrite in place
//! and the second run is a no-op, plain script entry files fold to the
//! written `fn main` shape and round-trip, `##`-separated script entries
//! keep their statement groups with the separators in place, syntax
//! errors render structured diagnostics without touching the file, and
//! `--check` gates CI without writing.

use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Output},
};

mod support;

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
        let dir = support::guarded_dir(&format!("ds-cli-fmt-{name}-"));
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

/// A plain script entry file folds into the written `fn main` (the R20
/// synthetic path); the formatted output re-parses and re-formats
/// identically.
#[test]
fn a_script_entry_folds_to_written_main_and_round_trips() {
    let scratch = Scratch::new("entry");
    let path = scratch.write("entry.ds", "const   a=1\nprintln(a + 1)\n");
    let out = ds_fmt(&["fmt", path.to_str().expect("temp path is UTF-8")]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("formatted {}\n", path.display())
    );
    assert_eq!(
        fs::read_to_string(&path).expect("the file rewrote"),
        "fn main() -> () {\n  const a = 1\n  println(a + 1)\n}\n"
    );
    // The written form is fixed point: library-mode parse, no rewrite.
    let out = ds_fmt(&["fmt", path.to_str().expect("temp path is UTF-8")]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("unchanged {}\n", path.display())
    );
}

/// A `##`-separated script entry keeps its script shape: the separator
/// stays between the statement groups it separates (the grammar admits
/// `##` only between top-level blocks, so the fold's one-`fn main`
/// spelling cannot carry it), and the written form is a fixed point
/// under both `fmt` and `--check`.
#[test]
fn a_separated_script_entry_keeps_its_statement_groups() {
    let scratch = Scratch::new("groups");
    let path = scratch.write(
        "groups.ds",
        "const   a=1\nprintln(\"hi\")\n## part two\nconst   b=2\nprintln(\"bye\")\n",
    );
    let arg = path.to_str().expect("temp path is UTF-8");
    let out = ds_fmt(&["fmt", arg]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        fs::read_to_string(&path).expect("the file rewrote"),
        "const a = 1\nprintln(\"hi\")\n## part two\nconst b = 2\nprintln(\"bye\")\n"
    );
    let out = ds_fmt(&["fmt", arg]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("unchanged {}\n", path.display())
    );
    let out = ds_fmt(&["fmt", "--check", arg]);
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

/// A mixed-EOL file folds its layout to the dominant CRLF while a raw
/// string's bare-LF line survives byte for byte — line-ending
/// normalization is layout-only, never program data. The result is a
/// fixed point under both `fmt` and `--check`.
#[test]
fn mixed_eol_files_fold_layout_only() {
    let scratch = Scratch::new("eol");
    let src = "fn a() -> int {\r\n1\r\n}\r\n\r\nfn b() -> string \
               {\r\nr\"one\ntwo\"\r\n}\r\n\r\nfn c() -> int {\r\n2\r\n}\r\n";
    let path = scratch.write("eol.ds", src);
    let arg = path.to_str().expect("temp path is UTF-8");
    let out = ds_fmt(&["fmt", arg]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("formatted {}\n", path.display())
    );
    assert_eq!(
        fs::read_to_string(&path).expect("the file rewrote"),
        "fn a() -> int {\r\n  1\r\n}\r\n\r\nfn b() -> string {\r\n  r\"one\ntwo\"\r\n}\r\n\r\nfn \
         c() -> int {\r\n  2\r\n}\r\n"
    );
    let out = ds_fmt(&["fmt", arg]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("unchanged {}\n", path.display())
    );
    let out = ds_fmt(&["fmt", "--check", arg]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("unchanged {}\n", path.display())
    );
    // The minority spelling keeps its own bytes too: a bare-LF file with
    // a CRLF raw string stays LF around the untouched literal.
    let src = "const s =   r\"x\r\ny\"\n";
    let path = scratch.write("eol2.ds", src);
    let arg = path.to_str().expect("temp path is UTF-8");
    let out = ds_fmt(&["fmt", arg]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        fs::read_to_string(&path).expect("the file rewrote"),
        "const s = r\"x\r\ny\"\n"
    );
    let out = ds_fmt(&["fmt", "--check", arg]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
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
