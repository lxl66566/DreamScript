//! End-to-end package runs against a git-remote registry: `$DS_REGISTRY`
//! spelled as a `file://` URL serves the registry's index rows and
//! bundles from a temp bare repository, the store's misses fill from
//! the snapshot's verified content, and the run compiles and executes
//! over the fetched graph — a pre-release version riding the remote's
//! rows. The remote is local; no live network serves the suite.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use ds_diag::FileId;
use ds_mod::Hash128;
use ds_pkg::{INDEX_SEGMENT, Manifest, Package};

/// The dependency's member: the run proves the fetched graph links by
/// calling through it.
const LIB_MEMBER: &str = "pub fn twice(n: int) -> int {\nn + n\n}\n";

/// The root's source: imports the dependency's member and calls it.
const ROOT_SOURCE: &str = concat!(
    "import @lib (twice)\n",
    "\n",
    "fn main() -> int {\n",
    "twice(21)\n",
    "}\n",
);

/// The dependency's manifest, its version a pre-release spelling.
const LIB_MANIFEST: &str = "name = \"@lib\"\nversion = \"1.2.0-rc.1\"\n";

/// Whether the system git serves this suite: the fixtures build
/// repositories with it and the client shells out to it, so a machine
/// without git reports and skips rather than failing.
fn git_present() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

/// One `git -C dir` invocation, refusing with git's own stderr.
fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The `file://` spelling of one local path: a unix path spells
/// `file:///abs`, a Windows drive path `file:///Z:/...` with forward
/// slashes — the three-slash form both platforms honor.
fn file_url(path: &Path) -> String {
    #[cfg(unix)]
    {
        format!("file://{}", path.display())
    }
    #[cfg(windows)]
    {
        format!("file:///{}", path.display().to_string().replace('\\', "/"))
    }
}

/// The canonical bundle bytes a package's identity names.
fn bundle(manifest: &str, sources: &[(&str, &str)]) -> (Vec<u8>, Hash128) {
    let manifest = Manifest::parse(manifest, FileId::new(0)).expect("fixture manifest parses");
    let package = Package::new(
        &manifest,
        sources
            .iter()
            .map(|(path, text)| ((*path).to_owned(), text.as_bytes().to_vec())),
    )
    .expect("fixture bundle builds");
    (package.to_bytes(), package.content_hash())
}

/// Writes one package tree: the manifest plus its sources.
fn write_pkg(dir: &Path, manifest: &str, sources: &[(&str, &str)]) {
    fs::create_dir_all(dir).expect("the package dir creates");
    fs::write(dir.join("ds.pkg"), manifest).expect("the manifest writes");
    for (path, text) in sources {
        fs::write(dir.join(path), text).expect("the member writes");
    }
}

/// One bare registry repository offering the lib package at its
/// pre-release version, answering the `file://` URL the client clones.
fn registry_url(work: &Path, version: &str, bytes: &[u8], hash: Hash128) -> String {
    let tree = work.join("tree");
    let row = tree.join(INDEX_SEGMENT).join("@lib");
    fs::create_dir_all(row.parent().expect("the row file has a parent"))
        .expect("the index segment creates");
    fs::write(&row, format!("{version} blake3:{hash}\n")).expect("the row writes");
    fs::create_dir_all(tree.join("pkg")).expect("the storage creates");
    fs::write(tree.join("pkg").join(format!("{hash}.pkg")), bytes).expect("the bundle writes");
    git(&tree, &["init", "--quiet"]);
    git(&tree, &["add", INDEX_SEGMENT, "pkg"]);
    git(&tree, &[
        "-c",
        "user.name=ds",
        "-c",
        "user.email=ds@localhost",
        "commit",
        "--quiet",
        "--message",
        "registry",
    ]);
    let bare = work.join("registry.git");
    let output = Command::new("git")
        .arg("clone")
        .arg("--quiet")
        .arg("--bare")
        .arg(&tree)
        .arg(&bare)
        .output()
        .expect("the bare clone runs");
    assert!(
        output.status.success(),
        "the bare clone failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    file_url(&bare)
}

/// Runs the built binary against one cache directory and one
/// `$DS_REGISTRY` spelling, flattening the outcome.
fn ds(cache: &Path, registry: &str, args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_ds"))
        .env("DS_CACHE", cache)
        .env("DS_REGISTRY", registry)
        .args(args)
        .output()
        .expect("the ds binary runs");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// The root fixture tree, beside whose entry file the manifest sits.
fn write_root(work: &Path, lib_id: Hash128) -> PathBuf {
    let root = work.join("root");
    write_pkg(
        &root,
        &format!(
            "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@lib = {{ ver = \"1.2.0-rc.1\", hash \
             = \"blake3:{lib_id}\" }}\n"
        ),
        &[("app.ds", ROOT_SOURCE)],
    );
    root
}

/// A cold store plus a git-remote registry: `$DS_REGISTRY` spelled as a
/// `file://` URL, the store's misses fill from the snapshot's verified
/// content, and the package run compiles and runs over the fetched
/// graph with the pre-release version riding the rows.
#[test]
fn a_git_remote_registry_fills_a_cold_store_run() {
    if !git_present() {
        eprintln!("skipping: the system git is absent");
        return;
    }
    let work = tempfile::tempdir().expect("temp work root");
    let cache = tempfile::tempdir().expect("temp cache root");
    let (bytes, lib_id) = bundle(LIB_MANIFEST, &[("lib.ds", LIB_MEMBER)]);
    let url = registry_url(work.path(), "1.2.0-rc.1", &bytes, lib_id);
    let root = write_root(work.path(), lib_id);
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");

    let (ok, out, err) = ds(cache.path(), &url, &["run", "--print-result", entry]);
    assert!(ok, "the remote-filled run exits 0 (stderr: {err})");
    assert_eq!(out, "42\n", "the run calls through the fetched graph");
    assert!(
        cache
            .path()
            .join("pkg")
            .join(format!("{lib_id}.pkg"))
            .is_file(),
        "the verified bundle installs from the snapshot"
    );
}

/// A `$DS_REGISTRY` URL the client cannot clone refuses with git's own
/// words, and a registry that answered carries no local-fill hint.
#[test]
fn an_uncloneable_remote_refuses_with_git_own_words() {
    if !git_present() {
        eprintln!("skipping: the system git is absent");
        return;
    }
    let work = tempfile::tempdir().expect("temp work root");
    let cache = tempfile::tempdir().expect("temp cache root");
    let url = file_url(&work.path().join("no-such-registry.git"));
    let (_, lib_id) = bundle(LIB_MANIFEST, &[("lib.ds", LIB_MEMBER)]);
    let root = write_root(work.path(), lib_id);
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");

    let (ok, out, err) = ds(cache.path(), &url, &["run", entry]);
    assert!(!ok, "a remote nothing clones exits nonzero");
    assert_eq!(out, "");
    assert!(
        err.contains("cloning the registry snapshot"),
        "the refusal names the clone it could not land: {err}"
    );
    assert!(
        err.contains(&url),
        "the refusal names the remote it could not reach: {err}"
    );
    assert!(
        !err.contains("nothing fetches remotely"),
        "a registry that answered carries no local-fill hint: {err}"
    );
    assert!(
        !cache.path().join("pkg").exists(),
        "nothing installs from a remote that never served"
    );
}
