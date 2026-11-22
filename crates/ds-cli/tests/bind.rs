//! End-to-end `ds bind` runs through the `ds` binary: the host's rustc
//! builds the fixture cdylib, a package pins it under `[artifacts]`,
//! `ds save` installs the bundle, and the store's artifact segment holds
//! the library bytes — `ds bind` then binds the installed library and
//! reports its exported C symbols, while the extern consumption seam
//! serves the marshaling slice's shapes end to end — the scalar words,
//! a string/bytes parameter's call-scoped pair, a unit return's absent
//! word — its out-of-slice shapes keeping the honest lowering
//! refusals.
//! The host's rustc (cargo sets `RUSTC` for the test binary) builds the
//! fixtures, so a build failure is a test failure, never a silent pass.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use ds_mod::Hash128;
use ds_pkg::Store;

mod support;

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
/// C-ABI shared object, no C toolchain involved. The exports cover the
/// marshaling slice's register classes the run pins: the i64 GP pair,
/// the f64 FP pair, and the u8 GP word.
const FIXTURE_SOURCE: &str = r#"
#[unsafe(no_mangle)]
pub extern "C" fn ds_add(left: i64, right: i64) -> i64 {
    left.wrapping_add(right)
}

#[unsafe(no_mangle)]
pub extern "C" fn ds_answer() -> i64 {
    42
}

#[unsafe(no_mangle)]
pub extern "C" fn ds_scale(value: f64, by: f64) -> f64 {
    value * by
}

#[unsafe(no_mangle)]
pub extern "C" fn ds_not(flag: u8) -> u8 {
    u8::from(flag == 0)
}
"#;

/// The marshaling-round fixture source: the tally family — the
/// borrowed (pointer, length) pair's pointer dereferenced and summed
/// (a mis-staged word answers garbage, never a pass), the pair followed
/// by an FP word (the mixed placement), two pairs (the four-GP-word
/// envelope), and the unit-returning records whose effect the scalar
/// face reads back.
const MARSHAL_FIXTURE_SOURCE: &str = r#"
use std::sync::atomic::{AtomicUsize, Ordering};

static RECORDED: AtomicUsize = AtomicUsize::new(0);

#[unsafe(no_mangle)]
pub extern "C" fn ds_tally(data: *const u8, len: usize) -> i64 {
    unsafe { read_buffer(data, len) }.iter().map(|&byte| i64::from(byte)).sum()
}

#[unsafe(no_mangle)]
pub extern "C" fn ds_weighed(data: *const u8, len: usize, scale: f64) -> f64 {
    let tally: f64 = unsafe { read_buffer(data, len) }
        .iter()
        .map(|&byte| f64::from(byte))
        .sum();
    tally * scale
}

#[unsafe(no_mangle)]
pub extern "C" fn ds_total(
    first: *const u8,
    first_len: usize,
    second: *const u8,
    second_len: usize,
) -> i64 {
    ds_tally(first, first_len).wrapping_add(ds_tally(second, second_len))
}

#[unsafe(no_mangle)]
pub extern "C" fn ds_record(data: *const u8, len: usize) {
    let _ = data;
    RECORDED.store(len, Ordering::SeqCst);
}

#[unsafe(no_mangle)]
pub extern "C" fn ds_recorded() -> i64 {
    RECORDED.load(Ordering::SeqCst) as i64
}

#[unsafe(no_mangle)]
pub extern "C" fn ds_clear() {
    RECORDED.store(0, Ordering::SeqCst);
}

unsafe fn read_buffer<'a>(data: *const u8, len: usize) -> &'a [u8] {
    if data.is_null() {
        return &[];
    }
    unsafe { std::slice::from_raw_parts(data, len) }
}
"#;

/// Compiles one fixture source as the cdylib into `dir` and answers its
/// bytes. Cargo sets `RUSTC` for the test binary, so a build failure is
/// a test failure carrying rustc's own stderr — never a silent pass.
fn build_fixture_source(dir: &Path, source: &str) -> Vec<u8> {
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
    let source_path = dir.join("fixture.rs");
    fs::write(&source_path, source).expect("the fixture source writes");
    let out = dir.join(library_name());
    let built = Command::new(rustc)
        .arg("--edition=2024")
        .arg("--crate-type=cdylib")
        .arg("--crate-name=dscli_bind_fixture")
        .arg(&source_path)
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap_or_else(|error| panic!("the fixture's rustc runs: {error}"));
    assert!(
        built.status.success(),
        "the fixture cdylib builds (rustc stderr: {})",
        String::from_utf8_lossy(&built.stderr)
    );
    fs::read(&out).expect("the built fixture reads back")
}

/// The marshaling-slice fixture over its own source.
fn build_fixture(dir: &Path) -> Vec<u8> {
    build_fixture_source(dir, FIXTURE_SOURCE)
}

/// The fixture root's source: it declares the bound extern the artifact
/// exports, the spelling the consumption seam routes.
const ROOT_SOURCE: &str = concat!(
    "extern \"math\" fn ds_add (left: int, right: int) -> int\n",
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

/// A unique guarded temp dir per tag, removed at the test thread's
/// exit.
fn temp_dir(tag: &str) -> PathBuf {
    support::guarded_dir(&format!("ds-cli-bind-{tag}-"))
}

/// A private per-thread store: spawned runs never touch a real user
/// cache directory, and the thread's exit removes the root.
fn test_cache() -> PathBuf {
    support::shared_dir("ds-cli-bind-cache-")
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
fn fixture(tag: &str, install_artifact: bool) -> (PathBuf, PathBuf, Hash128) {
    fixture_source(tag, install_artifact, ROOT_SOURCE)
}

/// The same fixture with a custom root source: the source writes before
/// the save, so the installed bundle's identity covers the exact bytes
/// the run compiles.
fn fixture_source(tag: &str, install_artifact: bool, source: &str) -> (PathBuf, PathBuf, Hash128) {
    let work = temp_dir(tag);
    let dylib = build_fixture(&work);
    let identity = ds_pkg::artifact_content_hash(&dylib);
    let root = work.join("root");
    fs::create_dir_all(&root).expect("the root dir creates");
    fs::write(root.join("ds.pkg"), root_manifest(identity)).expect("the manifest writes");
    fs::write(root.join("app.ds"), source).expect("the source writes");
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
    (root, cache, identity)
}

/// The fixture package over both custom sides: the dylib source and the
/// root source, saved and its library installed under its own identity.
fn fixture_both(
    tag: &str,
    install_artifact: bool,
    dylib: &str,
    source: &str,
) -> (PathBuf, PathBuf, Hash128) {
    let work = temp_dir(tag);
    let dylib_bytes = build_fixture_source(&work, dylib);
    let identity = ds_pkg::artifact_content_hash(&dylib_bytes);
    let root = work.join("root");
    fs::create_dir_all(&root).expect("the root dir creates");
    fs::write(root.join("ds.pkg"), root_manifest(identity)).expect("the manifest writes");
    fs::write(root.join("app.ds"), source).expect("the source writes");
    let cache = test_cache();
    let (ok, out, err) = ds(&cache, &["save", root.to_str().expect("the path is UTF-8")]);
    assert!(ok, "ds save exits 0 (stderr: {err})");
    assert!(
        out.starts_with("saved "),
        "the install reports (stdout: {out})"
    );
    if install_artifact {
        Store::open(&cache)
            .install_artifact(identity, &dylib_bytes)
            .expect("the fixture library installs into the store");
    }
    (root, cache, identity)
}

/// A package with its artifact installed binds: the report names the
/// artifact's identity and every exported C symbol.
#[test]
fn a_bound_artifact_reports_its_exported_c_symbols() {
    let (root, cache, identity) = fixture("report", true);
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
    let (root, cache, identity) = fixture("select", true);
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
        err.contains("declares no artifact `nope`") && err.contains("declared: math"),
        "the refusal names the demand and the declared spellings (stdout: {out}, stderr: {err})"
    );
}

/// A package whose artifact never installed refuses to bind: the
/// missing library is an unsatisfied demand, the same refusal a package
/// run answers before any compile.
#[test]
fn a_missing_artifact_refuses_the_bind() {
    let (root, cache, _) = fixture("missing", false);
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

/// The externs' consumption seam is live (H5): a scalar-signature bound
/// extern runs the installed artifact's C symbol through the run's bound
/// install, the answers pinning every register-class shape the slice
/// rules — the i64 GP pair, the f64 FP pair, the u8 GP word, and the
/// empty call — and a warm run (the script-mode cache serving the root's
/// dsmod) answering identically.
#[test]
fn a_bound_extern_call_answers_over_the_installed_artifact() {
    let (root, cache, _) = fixture("consume", true);
    let entry = root.join("app.ds");
    let (ok, out, err) = ds(&cache, &["run", entry.to_str().expect("the path is UTF-8")]);
    assert!(ok, "the bound extern call runs (stderr: {err})");
    assert_eq!(
        out, "42\n",
        "the answer prints one word per line (stdout: {out})"
    );
    // the warm run: the script-mode cache serves the root's dsmod and
    // the same bound install answers identically
    let (ok, warm, err) = ds(&cache, &["run", entry.to_str().expect("the path is UTF-8")]);
    assert!(ok, "the warm run answers (stderr: {err})");
    assert_eq!(warm, out, "the warm run answers identically");
}

/// The root's source pinning the slice's remaining register-class
/// shapes: the FP pair, the u8 GP word, and the empty call, answered in
/// one run through the entry's whole word span.
const SHAPES_SOURCE: &str = concat!(
    "extern \"math\" fn ds_scale (value: float, by: float) -> float\n",
    "extern \"math\" fn ds_not (flag: bool) -> bool\n",
    "extern \"math\" fn ds_answer () -> int\n",
    "\n",
    "fn main () -> (float, bool, int) {\n",
    "(ds_scale(1.5, 4.0), ds_not(true), ds_answer())\n",
    "}\n",
);

/// The run pins the slice's remaining register-class shapes over the
/// same installed artifact: the FP pair, the u8 GP word, and the empty
/// call — the entry's whole word span answering one word per line.
#[test]
fn the_register_class_shapes_answer_over_the_installed_artifact() {
    let (root, cache, _) = fixture_source("shapes", true, SHAPES_SOURCE);
    let entry = root.join("app.ds");
    let (ok, out, err) = ds(&cache, &["run", entry.to_str().expect("the path is UTF-8")]);
    assert!(ok, "the shapes run (stderr: {err})");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "three answers print (stdout: {out})");
    assert_eq!(lines[0], "6.0", "the FP pair marshals");
    assert_eq!(
        lines[1], "false",
        "the u8 word marshals ds_not(true) flipped"
    );
    assert_eq!(lines[2], "42", "the empty call marshals");
}

/// The marshaling round's language-side consumption (H5): a `string`
/// or `bytes` parameter rides its call-scoped (pointer, length) pair
/// and a unit return rides the absent word, both live through `ds run`
/// over the installed artifact — the pair legs' values answered
/// directly (the fixture dereferences the pointer, so a mis-staged word
/// answers garbage, never a pass) and the unit calls' effects read back
/// through the scalar face.
#[test]
fn the_marshaled_pair_and_unit_shapes_answer_over_the_installed_artifact() {
    let source = concat!(
        "extern \"math\" fn ds_tally (data: string) -> int\n",
        "extern \"math\" fn ds_weighed (data: bytes, scale: float) -> float\n",
        "extern \"math\" fn ds_total (first: string, second: bytes) -> int\n",
        "extern \"math\" fn ds_record (data: string)\n",
        "extern \"math\" fn ds_recorded () -> int\n",
        "extern \"math\" fn ds_clear ()\n",
        "\n",
        "fn main () -> (int, float, int, int) {\n",
        "const text = \"abc\"\n",
        "const buf = utf8_encode(\"ab\")\n",
        "const a = ds_tally(text)\n",
        "const w = ds_weighed(buf, 10.0)\n",
        "const t = ds_total(text, buf)\n",
        "ds_record(\"xyz\")\n",
        "const recorded = ds_recorded()\n",
        "ds_clear()\n",
        "(a, w, t, recorded * 100 + ds_recorded())\n",
        "}\n",
    );
    let (root, cache, _) = fixture_both("marshal", true, MARSHAL_FIXTURE_SOURCE, source);
    let entry = root.join("app.ds");
    let (ok, out, err) = ds(&cache, &["run", entry.to_str().expect("the path is UTF-8")]);
    assert!(ok, "the marshaled shapes run (stderr: {err})");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 4, "four answers print (stdout: {out})");
    assert_eq!(
        lines[0], "294",
        "the string pair's borrowed bytes tally (97+98+99) (stdout: {out})"
    );
    assert_eq!(
        lines[1], "1950.0",
        "the bytes pair followed by the FP word, the mixed placement (stdout: {out})"
    );
    assert_eq!(
        lines[2], "489",
        "two pairs fill the four-C-word envelope (294+195) (stdout: {out})"
    );
    assert_eq!(
        lines[3], "300",
        "the unit calls' effects read back: 3 recorded, then cleared to 0 (stdout: {out})"
    );
}

/// A symbol the artifact does not export refuses at lowering, the
/// refusal naming the symbol and the artifact.
#[test]
fn a_missing_symbol_refuses_the_run() {
    let (root, cache, _) = fixture_source(
        "missing-symbol",
        true,
        concat!(
            "extern \"math\" fn ds_missing () -> int\n",
            "\n",
            "fn main() -> int {\n",
            "ds_missing()\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let (ok, out, err) = ds(&cache, &["run", entry.to_str().expect("the path is UTF-8")]);
    assert!(!ok, "the missing symbol refuses");
    assert!(
        err.contains("the bound extern `ds_missing` refuses")
            && err.contains("exports no `ds_missing`"),
        "the refusal names the symbol (stdout: {out}, stderr: {err})"
    );
}

/// A signature outside the marshaling slice refuses at lowering, the
/// refusal naming the slice: a string return here (the returning
/// direction is the ownership round's own).
#[test]
fn a_wrong_signature_refuses_the_run() {
    let (root, cache, _) = fixture_source(
        "wrong-signature",
        true,
        concat!(
            "extern \"math\" fn ds_add (left: string, right: string) -> string\n",
            "\n",
            "fn main() -> string {\n",
            "ds_add(\"a\", \"b\")\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let (ok, out, err) = ds(&cache, &["run", entry.to_str().expect("the path is UTF-8")]);
    assert!(!ok, "the out-of-slice signature refuses");
    assert!(
        err.contains("the bound extern `ds_add` refuses") && err.contains("C-ABI marshaling slice"),
        "the refusal names the slice (stdout: {out}, stderr: {err})"
    );
}

/// Runs the binary with the run-cache debug observable on, so the
/// repin round can pin which path each run took.
fn ds_traced(cache: &Path, args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_ds"))
        .env("DS_CACHE", cache)
        .env_remove("DS_REGISTRY")
        .env("DS_RUN_CACHE_DEBUG", "1")
        .args(args)
        .output()
        .expect("the ds binary runs");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Fixture A's exports: ds_answer alone, so the sorted enumeration gives
/// it index 0.
const PIN_A_SOURCE: &str = r#"
#[unsafe(no_mangle)]
pub extern "C" fn ds_answer() -> i64 {
    42
}
"#;

/// Fixture B's exports: ds_aaa sorts ahead of ds_answer, so the
/// enumeration moves ds_answer to index 1 — the library a stale module
/// compiled against A must never be served over.
const PIN_B_SOURCE: &str = r#"
#[unsafe(no_mangle)]
pub extern "C" fn ds_aaa() -> i64 {
    999
}

#[unsafe(no_mangle)]
pub extern "C" fn ds_answer() -> i64 {
    42
}
"#;

/// A repinned artifact re-keys the warm module: the same source over a
/// moved `[artifacts]` pin takes a fresh compile that addresses the new
/// library's own enumeration, and the run still answers 42 — a stale
/// module compiled against fixture A would call B's index-0 export and
/// answer 999.
#[test]
fn a_repinned_artifact_never_serves_the_warm_module() {
    let work = temp_dir("repin");
    let a = build_fixture_source(&work, PIN_A_SOURCE);
    let b = build_fixture_source(&work, PIN_B_SOURCE);
    let pin_a = ds_pkg::artifact_content_hash(&a);
    let pin_b = ds_pkg::artifact_content_hash(&b);
    let root = work.join("root");
    fs::create_dir_all(&root).expect("the root dir creates");
    let entry = root.join("app.ds");
    fs::write(
        &entry,
        "extern \"math\" fn ds_answer () -> int\n\nfn main() -> int {\nds_answer()\n}\n",
    )
    .expect("the source writes");
    fs::write(root.join("ds.pkg"), root_manifest(pin_a)).expect("the manifest writes");
    let cache = test_cache();
    let store = Store::open(&cache);
    store
        .install_artifact(pin_a, &a)
        .expect("fixture A installs");
    store
        .install_artifact(pin_b, &b)
        .expect("fixture B installs");
    let run = || {
        let (ok, out, err) =
            ds_traced(&cache, &["run", entry.to_str().expect("the path is UTF-8")]);
        assert!(
            ok,
            "the run over the pinned artifact exits 0 (stderr: {err})"
        );
        assert_eq!(out, "42\n", "the answer prints (stderr: {err})");
        err
    };
    // cold then warm over pin A, both answering 42
    let cold = run();
    assert!(
        cold.contains("ds run cache: miss"),
        "the first run compiles: {cold}"
    );
    let warm = run();
    assert!(
        warm.contains("ds run cache: hit"),
        "the second run serves the cold run's artifact: {warm}"
    );
    // repin to B: the moved pin re-keys, the warm artifact compiled
    // against A never serves, and the fresh module addresses B's own
    // enumeration — still 42
    fs::write(root.join("ds.pkg"), root_manifest(pin_b)).expect("the manifest rewrites");
    let repinned = run();
    assert!(
        repinned.contains("ds run cache: miss"),
        "the repinned run recompiles: {repinned}"
    );
    let warm_b = run();
    assert!(
        warm_b.contains("ds run cache: hit"),
        "the repin's own artifact serves: {warm_b}"
    );
}

/// A script with no manifest beside it keeps the graphless refusal: a
/// bound extern addresses an artifact entry only a package graph
/// resolves.
#[test]
fn a_graphless_bound_extern_refuses() {
    let work = temp_dir("plain-bound");
    let source = work.join("plain.ds");
    fs::write(
        &source,
        concat!(
            "extern \"math\" fn ds_add (left: int, right: int) -> int\n",
            "\n",
            "fn main() -> int {\n",
            "ds_add(20, 22)\n",
            "}\n",
        ),
    )
    .expect("the source writes");
    let cache = test_cache();
    let (ok, out, err) = ds(&cache, &[
        "run",
        source.to_str().expect("the path is UTF-8"),
    ]);
    assert!(!ok, "a graphless bound extern refuses");
    assert!(
        err.contains("declares no [artifacts] entry `math`"),
        "the refusal names the missing fold (stdout: {out}, stderr: {err})"
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
