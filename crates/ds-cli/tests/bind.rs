//! End-to-end `ds bind` runs through the `ds` binary: the host's rustc
//! builds the fixture cdylib, a package pins it under `[artifacts]`,
//! `ds save` installs the bundle, and the store's artifact segment holds
//! the library bytes — `ds bind` then binds the installed library and
//! reports its exported C symbols, while the extern consumption seam
//! keeps its honest lowering refusal (the marshaling round's own).

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use ds_mod::Hash128;
use ds_pkg::Store;

/// The fixture library's file name per platform.
fn library_name() -> &'static str {
    if cfg!(windows) {
        "dscli_bind_fixture.dll"
    } else if cfg!(target_os = "macos") {
        "libdscli_bind_fixture.dylib"
    } else {
        "libdscli_bind_fixture.so"
    }
}

/// The host's target triple, the spelling the fixture's manifest pins.
fn host_triple() -> &'static str {
    if cfg!(all(target_arch = "x86_64", target_os = "windows")) {
        "x86_64-pc-windows-msvc"
    } else if cfg!(all(target_arch = "aarch64", target_os = "windows")) {
        "aarch64-pc-windows-msvc"
    } else if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_arch = "aarch64", target_os = "linux")) {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(all(target_arch = "x86_64", target_os = "macos")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_arch = "aarch64", target_os = "macos")) {
        "aarch64-apple-darwin"
    } else {
        "unknown-unknown"
    }
}

/// The fixture source the host's rustc compiles as a cdylib: a real
/// C-ABI shared object, no C toolchain involved.
const FIXTURE_SOURCE: &str = r#"
#[no_mangle]
pub extern "C" fn ds_add(left: i64, right: i64) -> i64 {
    left.wrapping_add(right)
}

#[no_mangle]
pub extern "C" fn ds_answer() -> i64 {
    42
}
"#;

/// Compiles the fixture cdylib into `dir`, `None` when the host's rustc
/// cannot run (the gated legs skip).
fn build_fixture(dir: &Path) -> Option<Vec<u8>> {
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
    let source = dir.join("fixture.rs");
    fs::write(&source, FIXTURE_SOURCE).expect("the fixture source writes");
    let out = dir.join(library_name());
    let built = Command::new(rustc)
        .arg("--edition=2024")
        .arg("--crate-type=cdylib")
        .arg("--crate-name=dscli_bind_fixture")
        .arg(&source)
        .arg("-o")
        .arg(&out)
        .output()
        .ok()?;
    if !built.status.success() {
        return None;
    }
    fs::read(&out).ok()
}

/// The fixture root's source: it declares the extern the artifact
/// exports, the spelling the marshaling round will route.
const ROOT_SOURCE: &str = concat!(
    "extern fn ds_add (left: int, right: int) -> int\n",
    "\n",
    "fn main() -> int {\n",
    "ds_add(20, 22)\n",
    "}\n",
);

/// The root's manifest, pinning the fixture library under [artifacts].
fn root_manifest(artifact: Hash128) -> String {
    format!(
        "name = \"app\"\nversion = \"0.1.0\"\n\n[artifacts]\nmath = {{ ver = \"1.0.0\", target = \
         \"{}\", hash = \"blake3:{artifact}\" }}\n",
        host_triple()
    )
}

/// A unique temp dir per tag, removed first.
fn temp_dir(tag: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("ds-cli-bind-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("the temp dir creates");
    dir
}

/// A private per-process store: spawned runs never touch a real user
/// cache directory.
fn test_cache() -> PathBuf {
    static CACHE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            let dir = env::temp_dir().join(format!("ds-cli-bind-cache-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            dir
        })
        .clone()
}

/// Runs the built binary against one cache directory.
fn ds(cache: &Path, args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_ds"))
        .env("DS_CACHE", cache)
        .env_remove("DS_REGISTRY")
        .args(args)
        .output()
        .expect("the ds binary runs");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// The fixture package: the cdylib bytes, their artifact identity, and
/// the root tree saved into the cache's store (the bundle only — the
/// artifact bytes install separately, the registry fetch the run path
/// composes).
fn fixture(tag: &str, install_artifact: bool) -> Option<(PathBuf, PathBuf, Hash128)> {
    let work = temp_dir(tag);
    let Some(dylib) = build_fixture(&work) else {
        eprintln!("skipping: the host's rustc cannot build the fixture cdylib");
        return None;
    };
    let identity = ds_pkg::artifact_content_hash(&dylib);
    let root = work.join("root");
    fs::create_dir_all(&root).expect("the root dir creates");
    fs::write(root.join("ds.pkg"), root_manifest(identity)).expect("the manifest writes");
    fs::write(root.join("app.ds"), ROOT_SOURCE).expect("the source writes");
    let cache = test_cache();
    let (ok, out, err) = ds(&cache, &["save", root.to_str().expect("the path is UTF-8")]);
    assert!(ok, "ds save exits 0 (stderr: {err})");
    assert!(
        out.starts_with("saved "),
        "the install reports (stdout: {out})"
    );
    if install_artifact {
        Store::open(&cache)
            .install_artifact(identity, &dylib)
            .expect("the fixture library installs into the store");
    }
    Some((root, cache, identity))
}

/// A package with its artifact installed binds: the report names the
/// artifact's identity and every exported C symbol.
#[test]
fn a_bound_artifact_reports_its_exported_c_symbols() {
    let Some((root, cache, identity)) = fixture("report", true) else {
        return;
    };
    let entry = root.join("app.ds");
    let (ok, out, err) = ds(&cache, &[
        "bind",
        entry.to_str().expect("the path is UTF-8"),
    ]);
    assert!(ok, "ds bind exits 0 (stderr: {err})");
    assert!(
        out.contains(&format!(
            "bound `math` 1.0.0 for {} (blake3:{identity})",
            host_triple()
        )),
        "the report names the artifact's bound identity (stdout: {out})"
    );
    for symbol in ["ds_add", "ds_answer"] {
        assert!(
            out.contains(symbol),
            "the report lists `{symbol}` (stdout: {out})"
        );
    }
    // the directory target answers the same report: the package root
    // names the manifest as well as a child file does
    let (ok, dir_out, err) = ds(&cache, &["bind", root.to_str().expect("the path is UTF-8")]);
    assert!(ok, "ds bind <root> exits 0 (stderr: {err})");
    assert_eq!(
        out, dir_out,
        "the file and directory targets report identically"
    );
}

/// The `--artifact` flag selects one entry, an unknown name refusing
/// with the declared spellings.
#[test]
fn the_artifact_flag_selects_one_bound_library() {
    let Some((root, cache, identity)) = fixture("select", true) else {
        return;
    };
    let entry = root.join("app.ds");
    let (ok, out, err) = ds(&cache, &[
        "bind",
        entry.to_str().expect("the path is UTF-8"),
        "--artifact",
        "math",
    ]);
    assert!(ok, "the named artifact binds (stderr: {err})");
    assert!(
        out.contains(&format!(
            "bound `math` 1.0.0 for {} (blake3:{identity})",
            host_triple()
        )),
        "the selected artifact reports (stdout: {out})"
    );
    let (ok, out, err) = ds(&cache, &[
        "bind",
        entry.to_str().expect("the path is UTF-8"),
        "--artifact",
        "nope",
    ]);
    assert!(!ok, "an unknown artifact name refuses");
    assert!(
        out.contains("declares no artifact `nope`") && err.contains("declared: math"),
        "the refusal names the demand and the declared spellings (stdout: {out}, stderr: {err})"
    );
}

/// A package whose artifact never installed refuses to bind: the
/// missing library is an unsatisfied demand, the same refusal a package
/// run answers before any compile.
#[test]
fn a_missing_artifact_refuses_the_bind() {
    let Some((root, cache, _)) = fixture("missing", false) else {
        return;
    };
    let entry = root.join("app.ds");
    let (ok, out, err) = ds(&cache, &[
        "bind",
        entry.to_str().expect("the path is UTF-8"),
    ]);
    assert!(!ok, "a missing artifact refuses");
    assert!(
        err.contains("artifact `math` 1.0.0") && err.contains("is not installed"),
        "the refusal names the artifact and its demand (stdout: {out}, stderr: {err})"
    );
}

/// The consumption seam stays honest: a source whose extern names the
/// bound library's symbol still refuses at lowering — binding reports,
/// and the C-ABI marshaling round owns the call.
#[test]
fn an_extern_call_into_a_bound_artifact_keeps_its_lowering_refusal() {
    let Some((root, cache, _)) = fixture("consume", true) else {
        return;
    };
    let entry = root.join("app.ds");
    let (ok, out, err) = ds(&cache, &["run", entry.to_str().expect("the path is UTF-8")]);
    assert!(!ok, "the extern call refuses at lowering");
    assert!(
        err.contains("extern non-builtin"),
        "the refusal names the extern seam (stdout: {out}, stderr: {err})"
    );
}

/// A plain script with no manifest beside it refuses: nothing declares
/// an artifact to bind.
#[test]
fn a_manifest_free_target_refuses() {
    let work = temp_dir("plain");
    let source = work.join("plain.ds");
    fs::write(&source, "fn main() -> int { 1 }\n").expect("the source writes");
    let cache = test_cache();
    let (ok, out, err) = ds(&cache, &[
        "bind",
        source.to_str().expect("the path is UTF-8"),
    ]);
    assert!(!ok, "a manifest-free target refuses");
    assert!(
        err.contains("declares no package"),
        "the refusal names the missing manifest (stdout: {out}, stderr: {err})"
    );
}

/// A package whose manifest declares no artifact refuses with the empty
/// `[artifacts]` bound named.
#[test]
fn an_artifact_free_package_refuses() {
    let work = temp_dir("empty");
    let root = work.join("root");
    fs::create_dir_all(&root).expect("the root dir creates");
    fs::write(root.join("ds.pkg"), "name = \"app\"\nversion = \"0.1.0\"\n")
        .expect("the manifest writes");
    fs::write(root.join("app.ds"), "fn main() -> int { 1 }\n").expect("the source writes");
    let cache = test_cache();
    let (ok, out, err) = ds(&cache, &["save", root.to_str().expect("the path is UTF-8")]);
    assert!(ok, "ds save exits 0 (stderr: {err})");
    assert!(
        out.starts_with("saved "),
        "the install reports (stdout: {out})"
    );
    let entry = root.join("app.ds");
    let (ok, out, err) = ds(&cache, &[
        "bind",
        entry.to_str().expect("the path is UTF-8"),
    ]);
    assert!(!ok, "an artifact-free package refuses");
    assert!(
        err.contains("declares no [artifacts] entry"),
        "the refusal names the empty artifact bound (stdout: {out}, stderr: {err})"
    );
}
