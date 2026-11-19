//! The member walk `ds save` and `ds info` run over one worktree: a
//! link never moves the member set — a directory link (junction or
//! symlink) never expands, a .ds link bundles only when its target
//! resolves inside the root — so the saved identity is a deterministic
//! function of the tree and a link-free tree keeps its canonical bytes.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use ds_diag::FileId;
use ds_mod::Hash128;
use ds_pkg::{Manifest, Package};

/// The fixture member.
const MEMBER: &str = "pub fn a() -> int {\n1\n}\n";

/// The fixture manifest.
const MANIFEST: &str = "name = \"@walk\"\nversion = \"1.0.0\"\n";

/// Writes one package tree: the manifest plus its sources.
fn write_pkg(dir: &Path, manifest: &str, sources: &[(&str, &str)]) {
    fs::create_dir_all(dir).expect("the package dir creates");
    fs::write(dir.join("ds.pkg"), manifest).expect("the manifest writes");
    for (path, text) in sources {
        fs::write(dir.join(path), text).expect("the member writes");
    }
}

/// A unique temp dir per tag, removed first; the test removes it after.
fn temp_dir(tag: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("ds-cli-save-{tag}-{}", std::process::id()));
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
            let dir = env::temp_dir().join(format!("ds-cli-save-cache-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            dir
        })
        .clone()
}

/// Runs the built binary against the shared test store and flattens the
/// outcome.
fn ds(args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_ds"))
        .env("DS_CACHE", test_cache())
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

/// The canonical bundle identity a member set frames to: the same bytes
/// `ds save` writes, computed without the walk.
fn bundle_identity(sources: &[(&str, &str)]) -> Hash128 {
    let manifest = Manifest::parse(MANIFEST, FileId::new(0)).expect("fixture manifest parses");
    let bundle = Package::new(
        &manifest,
        sources
            .iter()
            .map(|(path, text)| ((*path).to_owned(), text.as_bytes().to_vec())),
    )
    .expect("fixture bundle builds");
    bundle.content_hash()
}

/// The identity a successful `ds save` reports, as its `blake3:` token.
fn saved_hash(stdout: &str) -> &str {
    stdout
        .trim()
        .rsplit(' ')
        .next()
        .expect("the install reports its identity")
}

/// Creates a directory link at `link` naming `target`: a junction on
/// Windows (no privilege), a symlink on Unix. The answer is whether the
/// platform created it; a caller whose link the platform refuses skips
/// the link's assertions.
fn link_dir(target: &Path, link: &Path) -> bool {
    #[cfg(windows)]
    {
        Command::new("cmd")
            .arg("/c")
            .arg("mklink")
            .arg("/J")
            .arg(link.as_os_str())
            .arg(target.as_os_str())
            .output()
            .is_ok_and(|output| output.status.success())
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
}

/// Creates a file link at `link` naming `target`, skipping (a false
/// answer) when the platform refuses the privilege.
fn link_file(target: &Path, link: &Path) -> bool {
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link).is_ok()
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
}

/// Removes a link the tests created, the platform's own spelling: a
/// junction removes as a directory, a Unix symlink as a file.
fn remove_link(link: &Path) {
    let _ = if cfg!(windows) { fs::remove_dir(link) } else { fs::remove_file(link) };
}

/// A link-free tree saves its canonical identity: the walk collects
/// exactly the members the bundle grammar frames, so the install's hash
/// is the one a dependent's manifest pins.
#[test]
fn a_link_free_tree_saves_its_canonical_identity() {
    let work = temp_dir("plain");
    let pkg = work.join("pkg");
    write_pkg(&pkg, MANIFEST, &[("a.ds", MEMBER)]);

    let (ok, out, err) = ds(&["save", pkg.to_str().expect("temp path is UTF-8")]);
    assert!(ok, "the link-free save exits 0 (stderr: {err})");
    assert_eq!(
        saved_hash(&out),
        format!("blake3:{}", bundle_identity(&[("a.ds", MEMBER)])),
        "the identity is the canonical framing of the members"
    );

    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A directory link never expands, whatever it names: a link looping
/// back into the root cannot multiply the tree into a member chain, a
/// link naming a directory outside the root pulls nothing in, and `ds
/// info` reads the same member set off the walked tree.
#[test]
fn a_directory_link_never_moves_the_member_set() {
    let work = temp_dir("dir-link");
    let pkg = work.join("pkg");
    write_pkg(&pkg, MANIFEST, &[("a.ds", MEMBER)]);
    let identity = format!("blake3:{}", bundle_identity(&[("a.ds", MEMBER)]));
    let outside = work.join("outside");
    fs::create_dir_all(&outside).expect("the outside dir creates");
    fs::write(outside.join("x.ds"), "pub fn x() -> int {\n9\n}\n")
        .expect("the outside member writes");
    let looped = pkg.join("loop");
    let escaped = pkg.join("out");
    assert!(
        link_dir(&pkg, &looped) && link_dir(&outside, &escaped),
        "the platform creates directory links"
    );

    let (ok, out, err) = ds(&["save", pkg.to_str().expect("temp path is UTF-8")]);
    assert!(ok, "the linked save exits 0 (stderr: {err})");
    assert_eq!(
        saved_hash(&out),
        identity,
        "both links contribute no member: the identity is the link-free one"
    );
    let (ok, out, err) = ds(&["info", pkg.to_str().expect("temp path is UTF-8")]);
    assert!(ok, "the linked info exits 0 (stderr: {err})");
    assert_eq!(
        out, "pub fn a() -> int\n",
        "the surface renders the one member"
    );

    remove_link(&looped);
    remove_link(&escaped);
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A .ds link bundles only inside the root: a link naming the tree's
/// own member joins under the link's path, a link escaping the root
/// stays out of the bundle.
#[test]
fn a_ds_link_bundles_only_inside_the_root() {
    let work = temp_dir("file-link");
    let pkg = work.join("pkg");
    write_pkg(&pkg, MANIFEST, &[("a.ds", MEMBER)]);
    let outside = work.join("outside");
    fs::create_dir_all(&outside).expect("the outside dir creates");
    fs::write(outside.join("x.ds"), "pub fn x() -> int {\n9\n}\n")
        .expect("the outside member writes");
    let escaped = pkg.join("escape.ds");
    let aliased = pkg.join("alias.ds");
    if !link_file(outside.join("x.ds").as_path(), &escaped) {
        // the platform refuses file links (no privilege); the walk's
        // directory-link rule is the platform-independent half
        return;
    }
    assert!(
        link_file(pkg.join("a.ds").as_path(), &aliased),
        "the platform that linked one file links two"
    );

    let (ok, out, err) = ds(&["save", pkg.to_str().expect("temp path is UTF-8")]);
    assert!(ok, "the linked save exits 0 (stderr: {err})");
    assert_eq!(
        saved_hash(&out),
        format!(
            "blake3:{}",
            bundle_identity(&[("a.ds", MEMBER), ("alias.ds", MEMBER)])
        ),
        "the inside link bundles, the escaping link stays out"
    );

    remove_link(&escaped);
    remove_link(&aliased);
    fs::remove_dir_all(&work).expect("the temp tree removes");
}
