//! End-to-end package runs through the `ds` binary: `ds save` installs
//! fixture packages into the test store, and a root whose `ds.pkg` sits
//! beside it resolves over the store, compiles against the graph, and
//! installs through the deps-first walk.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use ds_diag::FileId;
use ds_mod::{Dep, Hash128};
use ds_pkg::{Manifest, Package};

/// The deepest fixture package's member.
const DEEP_MEMBER: &str = "fn base() -> int {\n21\n}\n";

/// The middle fixture package's member: it imports the deepest package,
/// so the walk installs a two-deep chain.
const LIB_MEMBER: &str =
    "import @deep\n\nfn twice(n: int) -> int {\nn + n\n}\n\nfn main() -> int {\ntwice(21)\n}\n";

/// The fixture root's source: imports the ambient and the middle
/// package, prints, and returns.
const ROOT_SOURCE: &str = concat!(
    "import @std/io\n",
    "import @lib/helper (twice)\n",
    "\n",
    "fn main() -> int {\n",
    "println(\"hi\")\n",
    "7\n",
    "}\n",
);

/// The deepest package's manifest, dependency-free.
fn deep_manifest() -> String {
    "name = \"@deep\"\nversion = \"1.0.0\"\n".to_owned()
}

/// The middle package's manifest, pinning the deepest package.
fn lib_manifest(deep: Hash128) -> String {
    format!(
        "name = \"@lib\"\nversion = \"1.2.0\"\n\n[deps]\n@deep = {{ ver = \"1.0.0\", hash = \
         \"blake3:{deep}\" }}\n"
    )
}

/// The root's manifest, pinning the middle package (and optionally a
/// second, never-installed package the refusal tests name).
fn root_manifest(lib: Hash128, ghost: bool) -> String {
    let ghost = if ghost {
        format!(
            "\n@ghost = {{ ver = \"0.9.0\", hash = \"blake3:{}\" }}\n",
            Hash128::ZERO
        )
    } else {
        String::new()
    };
    format!(
        "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@lib = {{ ver = \"1.2.0\", hash = \
         \"blake3:{lib}\" }}{ghost}"
    )
}

/// The bundle identity a saved package installs under: the same canonical
/// bytes `ds save` frames from the tree.
fn bundle_identity(manifest: &str, sources: &[(&str, &str)]) -> Hash128 {
    let manifest = Manifest::parse(manifest, FileId::new(0)).expect("fixture manifest parses");
    let bundle = Package::new(
        &manifest,
        sources
            .iter()
            .map(|(path, text)| ((*path).to_owned(), text.as_bytes().to_vec())),
    )
    .expect("fixture bundle builds");
    bundle.content_hash()
}

/// Writes one package tree: the manifest plus its sources.
fn write_pkg(dir: &Path, manifest: &str, sources: &[(&str, &str)]) {
    fs::create_dir_all(dir).expect("the package dir creates");
    fs::write(dir.join("ds.pkg"), manifest).expect("the manifest writes");
    for (path, text) in sources {
        let file = dir.join(path);
        fs::create_dir_all(file.parent().expect("the member has a parent"))
            .expect("the member dir creates");
        fs::write(file, text).expect("the member writes");
    }
}

/// A unique temp dir per tag, removed first; the test removes it after.
fn temp_dir(tag: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("ds-cli-pkg-{tag}-{}", std::process::id()));
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
            let dir = env::temp_dir().join(format!("ds-cli-pkg-cache-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            dir
        })
        .clone()
}

/// Runs the built binary and flattens the outcome.
fn ds(args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_ds"))
        .env("DS_CACHE", test_cache())
        .args(args)
        .output()
        .expect("the ds binary runs");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// The fixture packages saved into the test store, and the identity the
/// root pins the middle package at.
fn fixtures(tag: &str) -> (PathBuf, Hash128) {
    let work = temp_dir(tag);
    let deep = work.join("deep");
    write_pkg(&deep, &deep_manifest(), &[("deep.ds", DEEP_MEMBER)]);
    let lib_manifest = lib_manifest(bundle_identity(&deep_manifest(), &[(
        "deep.ds",
        DEEP_MEMBER,
    )]));
    let lib = work.join("lib");
    write_pkg(&lib, &lib_manifest, &[("lib.ds", LIB_MEMBER)]);
    for dir in [&deep, &lib] {
        let (ok, out, err) = ds(&["save", dir.to_str().expect("temp path is UTF-8")]);
        assert!(ok, "ds save {} exits 0 (stderr: {err})", dir.display());
        assert!(
            out.starts_with("saved "),
            "the install reports the identity it pinned (stdout: {out})"
        );
    }
    let lib_id = bundle_identity(&lib_manifest, &[("lib.ds", LIB_MEMBER)]);
    (work, lib_id)
}

/// The root fixture tree, beside whose entry file the manifest sits.
fn write_root(work: &Path, manifest: &str, source: &str) -> PathBuf {
    let root = work.join("root");
    write_pkg(&root, manifest, &[("app.ds", source)]);
    root
}

/// The happy-path pair the fixture assertions share: the run prints and
/// exits clean over the resolved graph, and the built artifact's link
/// table pins the ambient and the middle package at the identities the
/// walk validates.
#[test]
fn a_saved_dep_serves_a_package_run_and_build() {
    let (work, lib_id) = fixtures("saved");
    let root = write_root(&work, &root_manifest(lib_id, false), ROOT_SOURCE);
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");

    let (ok, out, err) = ds(&["run", entry]);
    assert!(ok, "the package run exits 0 (stderr: {err})");
    assert_eq!(out, "hi\n7\n");

    let out_path = work.join("app.dsmod");
    let (ok, _, err) = ds(&[
        "build",
        entry,
        "--out",
        out_path.to_str().expect("temp path is UTF-8"),
    ]);
    assert!(ok, "the package build exits 0 (stderr: {err})");
    let bytes = fs::read(&out_path).expect("the artifact writes");
    let module = ds_mod::read(&bytes).expect("the built artifact reads");
    assert_eq!(
        module.deps(),
        &[
            Dep {
                name: "@std".to_owned(),
                content_hash: Hash128::compute(ds_stdlib::AMBIENT_SOURCE.as_bytes()),
            },
            Dep {
                name: "@lib".to_owned(),
                content_hash: Hash128::compute(LIB_MEMBER.as_bytes()),
            },
        ],
        "the writer records the ambient and the resolved package's identities"
    );
    assert_eq!(module.name(), "app");

    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// The missing dep the store never installed: resolution refuses naming
/// the demand and the registry round, for both run and build.
#[test]
fn a_missing_dep_refuses_naming_the_registry_round() {
    let (work, lib_id) = fixtures("missing");
    let root = write_root(&work, &root_manifest(lib_id, true), ROOT_SOURCE);
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");

    let (ok, out, err) = ds(&["run", entry]);
    assert!(!ok, "a package run with a missing dep exits nonzero");
    assert_eq!(out, "");
    assert!(err.contains("`@ghost`"), "the refusal names the dep: {err}");
    assert!(
        err.contains("registry round"),
        "the refusal names the fill: {err}"
    );

    let out_path = work.join("app.dsmod");
    let (ok, _, err) = ds(&[
        "build",
        entry,
        "--out",
        out_path.to_str().expect("temp path is UTF-8"),
    ]);
    assert!(!ok, "a package build with a missing dep exits nonzero");
    assert!(err.contains("`@ghost`"), "the refusal names the dep: {err}");

    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// An import the manifest never declares refuses: the recorded link
/// names no package the graph resolves, and the refusal says so.
#[test]
fn an_undeclared_import_refuses() {
    let (work, lib_id) = fixtures("undeclared");
    let source = concat!(
        "import @std/io\n",
        "import @other (helper)\n",
        "\n",
        "fn main() -> int {\n",
        "7\n",
        "}\n",
    );
    let root = write_root(&work, &root_manifest(lib_id, false), source);
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(!ok, "an undeclared import exits nonzero");
    assert_eq!(out, "");
    assert!(
        err.contains("import `@other` links no package the manifest graph resolves"),
        "the refusal names the link: {err}"
    );
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A package whose bundle carries more than one source member cannot
/// serve one module this round: the run refuses naming the member count.
#[test]
fn a_multi_member_package_refuses_to_serve() {
    let (work, _lib_id) = fixtures("multi");
    let wide = work.join("wide");
    let manifest = "name = \"@wide\"\nversion = \"1.0.0\"\n".to_owned();
    write_pkg(&wide, &manifest, &[
        ("a.ds", "fn a() -> int {\n1\n}\n"),
        ("b.ds", "fn b() -> int {\n2\n}\n"),
    ]);
    let (ok, _, err) = ds(&["save", wide.to_str().expect("temp path is UTF-8")]);
    assert!(ok, "the multi-member bundle installs (stderr: {err})");
    let wide_id = bundle_identity(&manifest, &[
        ("a.ds", "fn a() -> int {\n1\n}\n"),
        ("b.ds", "fn b() -> int {\n2\n}\n"),
    ]);
    let root_manifest = format!(
        "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@wide = {{ ver = \"1.0.0\", hash = \
         \"blake3:{wide_id}\" }}\n"
    );
    let root = write_root(
        &work,
        &root_manifest,
        "import @wide (a)\n\nfn main() -> int {\n7\n}\n",
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(!ok, "a multi-member package run exits nonzero");
    assert_eq!(out, "");
    assert!(
        err.contains("carries 2 source members"),
        "the refusal names the member count: {err}"
    );
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// `ds save` refuses a root with no .ds source and a root with no
/// manifest, each with one clean line.
#[test]
fn save_refuses_empty_roots() {
    let work = temp_dir("save-empty");
    let bare = work.join("bare");
    fs::create_dir_all(&bare).expect("the bare dir creates");
    fs::write(
        bare.join("ds.pkg"),
        "name = \"bare\"\nversion = \"1.0.0\"\n",
    )
    .expect("the manifest writes");
    let (ok, out, err) = ds(&["save", bare.to_str().expect("temp path is UTF-8")]);
    assert!(!ok, "a root with no source exits nonzero");
    assert_eq!(out, "");
    assert!(err.contains("no .ds source member"), "stderr: {err}");

    let empty = work.join("empty");
    fs::create_dir_all(&empty).expect("the empty dir creates");
    let (ok, _, err) = ds(&["save", empty.to_str().expect("temp path is UTF-8")]);
    assert!(!ok, "a root with no manifest exits nonzero");
    assert!(err.contains("reading"), "stderr: {err}");
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A source with no manifest beside it keeps the plain script path: the
/// run succeeds with the package machinery never engaging.
#[test]
fn a_plain_script_beside_no_manifest_runs_as_today() {
    let work = temp_dir("plain");
    let dir = work.join("plain");
    fs::create_dir_all(&dir).expect("the dir creates");
    let source = dir.join("main.ds");
    fs::write(&source, "fn main() -> int {\n7\n}\n").expect("the source writes");
    let (ok, out, err) = ds(&["run", source.to_str().expect("temp path is UTF-8")]);
    assert!(ok, "the plain script runs (stderr: {err})");
    assert_eq!(out, "7\n");
    fs::remove_dir_all(&work).expect("the temp tree removes");
}
