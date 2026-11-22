//! The git-remote registry round trip: the index's row files and
//! content-addressed bundles served out of a git repository, fetched
//! through the client's snapshot clone, verified and installed through
//! the store like a local-index fetch — the same taxonomy answering
//! over the remote's offering, clone failures refusing hard in git's
//! own words. The remote is a temp bare repository spelled `file://`,
//! so no live network serves the suite.

use std::{fmt::Write as _, path::Path, process::Command};

use ds_diag::FileId;
use ds_mod::Hash128;
use ds_pkg::{
    Demand, Fetched, GitRegistry, INDEX_SEGMENT, Manifest, Package, RegistryClient, Remote,
    Resolved, Store, StoreProvider, Version, resolve,
};

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

/// Builds a manifest from parts, mirroring the resolution fixtures.
fn manifest_of(name: &str, version: &str, deps: &[(&str, &str, Hash128)]) -> Manifest {
    let mut deps_line = String::new();
    for (dep, ver, hash) in deps {
        // one write per dep line: String formatting never fails
        let _ = writeln!(
            deps_line,
            "{dep} = {{ ver = \"{ver}\", hash = \"blake3:{hash}\" }}"
        );
    }
    let text = if deps.is_empty() {
        format!("name = \"{name}\"\nversion = \"{version}\"\n")
    } else {
        format!("name = \"{name}\"\nversion = \"{version}\"\n\n[deps]\n{deps_line}")
    };
    Manifest::parse(&text, FileId::new(0)).expect("fixture manifest parses")
}

/// Builds one package over a manifest with one source member.
fn package_of(name: &str, version: &str) -> Package {
    let manifest = manifest_of(name, version, &[]);
    Package::new(&manifest, [(
        format!("{}.ds", name.replace('/', "_")),
        b"fn helper() { 1 }".to_vec(),
    )])
    .expect("fixture package builds")
}

/// One bare registry repository over the given offering: each entry's
/// row line and content-addressed bundle written into a work tree, one
/// commit, and a bare clone answering the `file://` URL the client
/// clones. Entries must arrive in the offering's ascending version
/// order, the row file's own rule. The temp root outlives the caller's
/// use of the URL.
fn registry(entries: &[(&str, &Package)]) -> (tempfile::TempDir, String) {
    let work = tempfile::tempdir().expect("temp scratch root");
    let tree = work.path().join("tree");
    std::fs::create_dir_all(tree.join(INDEX_SEGMENT)).expect("the index segment creates");
    std::fs::create_dir_all(tree.join("pkg")).expect("the storage creates");
    for (name, package) in entries {
        let manifest = package.manifest(FileId::new(0)).expect("fixture parses");
        let row = tree.join(INDEX_SEGMENT).join(name);
        let rows = std::fs::read_to_string(&row).unwrap_or_default()
            + &format!("{} blake3:{}\n", manifest.version, package.content_hash());
        std::fs::write(&row, rows).expect("the row writes");
        std::fs::write(
            tree.join("pkg")
                .join(format!("{}.pkg", package.content_hash())),
            package.to_bytes(),
        )
        .expect("the bundle writes");
    }
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
    let bare = work.path().join("registry.git");
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
    (work, file_url(&bare))
}

/// Resolves one root over a cold store with the git registry filling
/// its misses, counting the demands that reached the remote.
fn resolve_over(
    root: &Manifest,
    url: &str,
    cache: &Path,
) -> (Result<Resolved, ds_pkg::ResolveError>, usize) {
    let store = Store::open(cache);
    let registry = GitRegistry::open(url);
    let mut fetches = 0;
    let mut provider = StoreProvider::with_remote(&store, |name, version| {
        fetches += 1;
        registry.fetch(name, version)
    });
    let outcome = resolve(root, Path::new("."), |name, demand| match demand {
        Demand::Registry { ver, pin } => provider.fetch(name, ver, pin),
        Demand::Path { .. } => Fetched::Refused("the fixture serves no path".to_owned()),
    });
    (outcome, fetches)
}

/// A cold store resolves over the git remote exactly like a local
/// index: the row names the bundle identity, the snapshot serves bytes
/// that recompute to it, the verified bundle installs through the
/// store's atomic path, and a second cold resolution over a second
/// client never fetches again — the installed row answers, the lock
/// bytes staying byte-identical.
#[test]
fn a_cold_store_fetches_verifies_and_installs_over_the_git_remote() {
    if !git_present() {
        eprintln!("skipping: the system git is absent");
        return;
    }
    let lib = package_of("lib", "1.2.0");
    let root = manifest_of("app", "1.0.0", &[("lib", "1.2.0", lib.content_hash())]);
    let (work, url) = registry(&[("lib", &lib)]);
    let first = tempfile::tempdir().expect("first cache root");
    let (outcome, fetches) = resolve_over(&root, &url, first.path());
    assert_eq!(fetches, 1, "the miss reaches the remote once");
    let resolved = outcome.expect("the remote resolves the fixture");
    assert_eq!(resolved.deps()["lib"].content_hash, lib.content_hash());
    assert_eq!(
        Store::open(first.path()).load(lib.content_hash()).unwrap(),
        Some(lib.clone()),
        "the verified bundle installs through the store's atomic path"
    );
    // a second resolution over the same warm store: the installed row
    // answers, so the remote is never fetched again
    let (again, fetches) = resolve_over(&root, &url, first.path());
    assert_eq!(fetches, 0, "the installed row answers without the remote");
    let again = again.expect("the store alone resolves");
    assert_eq!(again.lock_bytes(), resolved.lock_bytes());
    drop(work);
}

/// The remote's own taxonomy answers when the store cannot: a name the
/// remote's rows never carry is unknown, a version above its offering
/// unoffered with its best, and one clone serves every demand.
#[test]
fn the_taxonomy_answers_over_the_remote_offering() {
    if !git_present() {
        eprintln!("skipping: the system git is absent");
        return;
    }
    let lib = package_of("lib", "1.2.0");
    let (_work, url) = registry(&[("lib", &lib)]);
    let remote = GitRegistry::open(&url);
    let served = Version::parse("1.2.0").expect("fixture version parses");
    assert!(
        matches!(remote.fetch("ghost", &served), Remote::UnknownName),
        "the unknown name reports through the remote"
    );
    let Remote::Unoffered { best } = remote.fetch("lib", &Version::new(2, 0, 0)) else {
        panic!("the unoffered version reports its taxonomy");
    };
    assert_eq!(best, Some(served));
    assert!(
        matches!(remote.fetch("lib", &served), Remote::Found(_)),
        "the offered version serves from the same snapshot"
    );
}

/// A remote the client cannot clone refuses hard in git's own words,
/// never a taxonomy answer: an unreachable host, a missing repository,
/// and an absent git binary all land here.
#[test]
fn an_uncloneable_remote_refuses_hard() {
    if !git_present() {
        eprintln!("skipping: the system git is absent");
        return;
    }
    let work = tempfile::tempdir().expect("temp root");
    let url = file_url(&work.path().join("no-such-registry.git"));
    let remote = GitRegistry::open(&url);
    let Remote::Refused(message) = remote.fetch("lib", &Version::new(1, 0, 0)) else {
        panic!("a remote nothing clones refuses hard");
    };
    assert!(
        message.contains("cloning the registry snapshot"),
        "the refusal names the clone it could not land: {message}"
    );
    assert!(
        message.contains(&url),
        "the refusal names the remote it could not reach: {message}"
    );
}

/// Pre-release and build metadata ride the remote's rows: a demand for
/// the pre-release spelling resolves exactly it, the build metadata
/// riding the identity, and a plain demand takes the release exactly,
/// never the pre-release.
#[test]
fn pre_release_and_build_metadata_ride_the_remote() {
    if !git_present() {
        eprintln!("skipping: the system git is absent");
        return;
    }
    let rc = package_of("lib", "1.2.0-rc.1+build.7");
    let release = package_of("lib", "1.2.0");
    // rows ascend: the pre-release precedes its own release
    let (_work, url) = registry(&[("lib", &rc), ("lib", &release)]);
    let demanded = Version::parse("1.2.0-rc.1+build.7").expect("fixture version parses");
    let root = manifest_of("app", "1.0.0", &[(
        "lib",
        "1.2.0-rc.1+build.7",
        rc.content_hash(),
    )]);
    let cache = tempfile::tempdir().expect("temp cache root");
    let (outcome, _) = resolve_over(&root, &url, cache.path());
    let resolved = outcome.expect("the pre-release offering resolves");
    assert_eq!(resolved.deps()["lib"].version, demanded);
    assert_eq!(resolved.deps()["lib"].content_hash, rc.content_hash());
    let root = manifest_of("app", "1.0.0", &[("lib", "1.2.0", release.content_hash())]);
    let cache = tempfile::tempdir().expect("second cache root");
    let (outcome, _) = resolve_over(&root, &url, cache.path());
    let resolved = outcome.expect("the release offering resolves");
    assert_eq!(resolved.deps()["lib"].version, Version::new(1, 2, 0));
    assert_eq!(resolved.deps()["lib"].content_hash, release.content_hash());
}

/// The `$DS_REGISTRY` spelling rule: a `scheme://` value opens the git
/// client, anything else a local index root.
#[test]
fn the_env_spelling_picks_the_client() {
    assert!(matches!(
        RegistryClient::parse("https://example.com/ds-registry.git"),
        RegistryClient::Git(_)
    ));
    assert!(matches!(
        RegistryClient::parse("file:///tmp/ds-registry.git"),
        RegistryClient::Git(_)
    ));
    assert!(matches!(
        RegistryClient::parse("/tmp/ds-registry"),
        RegistryClient::Index(_)
    ));
    assert!(matches!(
        RegistryClient::parse("Z:/ds-registry"),
        RegistryClient::Index(_)
    ));
}
