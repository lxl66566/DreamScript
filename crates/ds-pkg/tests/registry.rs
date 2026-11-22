//! The registry index round trip: a store miss falls to the registry's
//! fetch hook, the index's row names the bundle identity, the storage
//! serves bytes that must recompute to it, the verified bundle installs
//! through the store's atomic path, and the taxonomy still answers for
//! a name or version the readable index does not offer — while anything
//! the index cannot verify refuses hard naming what failed.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    path::{Path, PathBuf},
};

use ds_diag::FileId;
use ds_mod::Hash128;
use ds_pkg::{
    ArtifactFetch, Demand, Fetched, INDEX_SEGMENT, Manifest, Package, Registry, Remote, Resolved,
    Store, StoreProvider, Version, resolve,
};

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

/// Builds a package over a manifest with one source member.
fn package_of(manifest: &Manifest, source: &[u8]) -> Package {
    Package::new(manifest, [(
        format!("{}.ds", manifest.name.replace('/', "_")),
        source.to_vec(),
    )])
    .expect("fixture package builds")
}

/// The one-graph fixture: an app package pinning a lib package, a fresh
/// store the misses fall into, and an index root offering packages.
struct Fixture {
    // the temp roots outlive the files written below them
    _scratch: tempfile::TempDir,
    _cache: tempfile::TempDir,
    index: PathBuf,
    store: Store,
    root: Manifest,
    lib: Package,
}

fn fixture() -> Fixture {
    let scratch = tempfile::tempdir().expect("temp scratch root");
    let cache = tempfile::tempdir().expect("temp cache root");
    let index = scratch.path().join("registry");
    std::fs::create_dir_all(index.join(INDEX_SEGMENT)).expect("the index segment creates");
    let store = Store::open(cache.path());
    let lib = package_of(
        &manifest_of("lib", "1.0.0", &[]),
        b"fn inc(x: int) { x + 37 }",
    );
    let root = manifest_of("app", "1.0.0", &[("lib", "1.0.0", lib.content_hash())]);
    Fixture {
        _scratch: scratch,
        _cache: cache,
        index,
        store,
        root,
        lib,
    }
}

/// Writes one package into the index: the row line appended to the
/// name's row file and the bundle below the content-addressed storage.
fn index_row(index: &Path, package: &Package) {
    let manifest = package.manifest(FileId::new(0)).expect("fixture parses");
    let row = index.join(INDEX_SEGMENT).join(&manifest.name);
    if let Some(parent) = row.parent() {
        std::fs::create_dir_all(parent).expect("the index row's parent creates");
    }
    let mut rows = std::fs::read_to_string(&row).unwrap_or_default();
    // one write per row: String formatting never fails
    let _ = writeln!(
        rows,
        "{} blake3:{}",
        manifest.version,
        package.content_hash()
    );
    std::fs::write(&row, rows).expect("the index row writes");
    let bundle = index.join("pkg");
    std::fs::create_dir_all(&bundle).expect("the index storage creates");
    std::fs::write(
        bundle.join(format!("{}.pkg", package.content_hash())),
        package.to_bytes(),
    )
    .expect("the index bundle writes");
}

/// Resolves one root over the store with the index filling its misses,
/// counting the fetches that reached the registry.
fn resolve_over(
    root: &Manifest,
    store: &Store,
    index: &Path,
) -> (Result<Resolved, ds_pkg::ResolveError>, usize) {
    let registry = Registry::open(index);
    let mut fetches = 0;
    let mut provider = StoreProvider::with_remote(store, |name, version| {
        fetches += 1;
        registry.fetch(name, version)
    });
    let outcome = resolve(root, Path::new("."), |name, demand| match demand {
        Demand::Registry { ver, pin } => provider.fetch(name, ver, pin),
        Demand::Path { .. } => Fetched::Refused("the fixture serves no path".to_owned()),
    });
    (outcome, fetches)
}

#[test]
fn a_store_miss_fetches_verifies_and_installs() {
    let fixture = fixture();
    index_row(&fixture.index, &fixture.lib);
    // a fresh store: the demand misses and the registry serves it
    let (resolved, fetches) = resolve_over(&fixture.root, &fixture.store, &fixture.index);
    let resolved = resolved.expect("the fixture resolves over the index");
    assert_eq!(fetches, 1);
    assert_eq!(
        fixture.store.load(fixture.lib.content_hash()).unwrap(),
        Some(fixture.lib.clone()),
        "the verified bundle installs through the store's atomic path"
    );
    assert_eq!(
        resolved.deps()["lib"].content_hash,
        fixture.lib.content_hash()
    );
    // a second resolution over a second, empty index never fetches: the
    // store's installed row answers, and the lock bytes stay stable
    let empty = fixture.index.parent().unwrap().join("empty");
    std::fs::create_dir_all(empty.join(INDEX_SEGMENT)).expect("the empty index creates");
    let (again, fetches) = resolve_over(&fixture.root, &fixture.store, &empty);
    let again = again.expect("the store alone resolves");
    assert_eq!(fetches, 0);
    assert_eq!(again.lock_bytes(), resolved.lock_bytes());
    // the same graph fetched into a second cold store serializes
    // byte-identically: the lock is a pure function of the graph
    let second = tempfile::tempdir().expect("second cache root");
    let store = Store::open(second.path());
    let registry = Registry::open(&fixture.index);
    let mut provider =
        StoreProvider::with_remote(&store, |name, version| registry.fetch(name, version));
    let cold = resolve(&fixture.root, Path::new("."), |name, demand| match demand {
        Demand::Registry { ver, pin } => provider.fetch(name, ver, pin),
        Demand::Path { .. } => Fetched::Refused("the fixture serves no path".to_owned()),
    })
    .expect("the second cold store resolves");
    assert_eq!(cold.lock_bytes(), resolved.lock_bytes());
}

#[test]
fn a_row_hash_mismatch_refuses_hard_naming_both_hashes() {
    let fixture = fixture();
    let other = package_of(&manifest_of("other", "1.0.0", &[]), b"fn f() {}");
    // the row pins the lib identity, the storage serves another package's
    // bytes under that name: the fetched bytes must recompute to the pin
    index_row(&fixture.index, &fixture.lib);
    std::fs::write(
        Registry::open(&fixture.index).bundle_path(fixture.lib.content_hash()),
        other.to_bytes(),
    )
    .expect("the drifted bundle writes");
    let (outcome, _) = resolve_over(&fixture.root, &fixture.store, &fixture.index);
    let message = outcome.expect_err("the mismatch refuses").to_string();
    assert!(
        message.contains(&format!("blake3:{}", fixture.lib.content_hash())),
        "the refusal names the row's pin: {message}"
    );
    assert!(
        message.contains(&format!("blake3:{}", other.content_hash())),
        "the refusal names the fetched bytes' own hash: {message}"
    );
    assert!(
        fixture
            .store
            .load(fixture.lib.content_hash())
            .unwrap()
            .is_none(),
        "unverified content never installs"
    );
}

#[test]
fn the_index_taxonomy_answers_when_the_store_cannot() {
    let fixture = fixture();
    index_row(&fixture.index, &fixture.lib);
    // the readable index carries the taxonomy: a name it never heard of
    // is unknown, a version above its offering unoffered with its best
    let ghost = manifest_of("app", "1.0.0", &[("ghost", "1.0.0", Hash128::ZERO)]);
    let (outcome, _) = resolve_over(&ghost, &fixture.store, &fixture.index);
    assert_eq!(
        outcome.expect_err("the unknown name refuses").to_string(),
        "`app` demands `ghost` at 1.0.0, which the provider does not serve"
    );
    let high = manifest_of("app", "1.0.0", &[("lib", "2.0.0", Hash128::ZERO)]);
    let (outcome, _) = resolve_over(&high, &fixture.store, &fixture.index);
    assert_eq!(
        outcome
            .expect_err("the unoffered version refuses")
            .to_string(),
        "`app` demands `lib` at 2.0.0, above every offered version (1.0.0)"
    );
}

#[test]
fn an_unreadable_index_refuses_hard_not_as_a_taxonomy() {
    let fixture = fixture();
    let missing = fixture.index.parent().unwrap().join("no-such-index");
    let (outcome, _) = resolve_over(&fixture.root, &fixture.store, &missing);
    let message = outcome.expect_err("the missing index refuses").to_string();
    assert!(
        message.contains("is not a readable directory"),
        "the refusal names the unusable index: {message}"
    );
}

#[test]
fn malformed_rows_refuse_hard_naming_the_row_file() {
    let fixture = fixture();
    let registry = Registry::open(&fixture.index);
    let row = registry.row_path("lib");
    for text in [
        // no trailing newline
        "1.0.0 blake3:00000000000000000000000000000000",
        // not a row shape
        "\n1.0.0\n",
        // non-canonical version spelling
        "1.0 blake3:00000000000000000000000000000000\n",
        // uppercase hex drift
        "1.0.0 blake3:0000000000000000000000000000000F\n",
        // descending rows
        "2.0.0 blake3:00000000000000000000000000000000\n1.0.0 \
         blake3:00000000000000000000000000000000\n",
        // a duplicate version
        "1.0.0 blake3:00000000000000000000000000000000\n1.0.0 \
         blake3:00000000000000000000000000000001\n",
    ] {
        std::fs::write(&row, text).expect("the drifted row writes");
        assert!(
            matches!(
                registry.fetch("lib", &Version::new(1, 0, 0)),
                Remote::Refused(_)
            ),
            "the drifted row refuses: {text:?}"
        );
    }
    // an empty row file is an honest empty listing: the name is known,
    // nothing offered
    std::fs::write(&row, "").expect("the empty row writes");
    assert!(matches!(
        registry.fetch("lib", &Version::new(1, 0, 0)),
        Remote::Unoffered { best: None }
    ));
    // a missing row file is the unknown-name taxonomy
    std::fs::remove_file(&row).expect("the row removes");
    assert!(
        matches!(
            registry.fetch("lib", &Version::new(1, 0, 0)),
            Remote::UnknownName
        ),
        "the missing row reports the unknown name"
    );
}

/// A offered-but-unstored row names an identity the storage holds no
/// bundle for: a hard refusal, never a taxonomy answer.
#[test]
fn a_row_without_a_stored_bundle_refuses_hard() {
    let fixture = fixture();
    let registry = Registry::open(&fixture.index);
    let row = registry.row_path("lib");
    std::fs::create_dir_all(row.parent().expect("the index segment exists"))
        .expect("the index segment creates");
    std::fs::write(
        &row,
        format!("1.0.0 blake3:{}\n", fixture.lib.content_hash()),
    )
    .expect("the row writes");
    let Remote::Refused(message) = registry.fetch("lib", &Version::new(1, 0, 0)) else {
        panic!("an unstored bundle refuses");
    };
    assert!(
        message.contains(&format!("blake3:{}", fixture.lib.content_hash())),
        "the refusal names the identity nothing serves: {message}"
    );
}

/// A row pinning a hash whose stored bytes decode nowhere refuses hard
/// too: verification precedes every taxonomy answer.
#[test]
fn an_undecodable_bundle_refuses_hard() {
    let fixture = fixture();
    let registry = Registry::open(&fixture.index);
    let bytes = b"not a bundle";
    let hash = Hash128::compute(bytes);
    let row = registry.row_path("lib");
    std::fs::create_dir_all(row.parent().expect("the index segment exists"))
        .expect("the index segment creates");
    std::fs::write(&row, format!("1.0.0 blake3:{hash}\n")).expect("the row writes");
    let bundle = registry.bundle_path(hash);
    std::fs::create_dir_all(bundle.parent().expect("the storage has a parent"))
        .expect("the storage creates");
    std::fs::write(&bundle, bytes).expect("the bundle writes");
    let outcome = registry.fetch("lib", &Version::new(1, 0, 0));
    assert!(
        matches!(outcome, Remote::Refused(_)),
        "bytes that hash to the pin but decode nowhere refuse: {outcome:?}"
    );
}

/// The manifest pin validates the served content independently of the
/// index's own verification: a row serving other content than the
/// declarer's pin refuses through the resolution's hash disagreement.
#[test]
fn a_pin_disagreement_with_the_index_refuses_independently() {
    let fixture = fixture();
    let other = package_of(
        &manifest_of("lib", "1.0.0", &[]),
        b"fn inc(x: int) { x + 1 }",
    );
    index_row(&fixture.index, &other);
    // the root pins the real lib's identity, the index serves another
    // package's bytes under the same name and version
    let (outcome, _) = resolve_over(&fixture.root, &fixture.store, &fixture.index);
    let message = outcome
        .expect_err("the pin disagreement refuses")
        .to_string();
    assert!(
        message.contains("pinned content"),
        "the refusal names the disagreement: {message}"
    );
}

#[test]
fn a_slashed_name_nests_as_its_own_segments() {
    let fixture = fixture();
    let nested = package_of(
        &manifest_of("acme/pipeline", "1.0.0", &[]),
        b"fn step(x: int) { x + 1 }",
    );
    index_row(&fixture.index, &nested);
    let root = manifest_of("app", "1.0.0", &[(
        "acme/pipeline",
        "1.0.0",
        nested.content_hash(),
    )]);
    let registry = Registry::open(&fixture.index);
    let mut provider = StoreProvider::with_remote(&fixture.store, |name, version| {
        registry.fetch(name, version)
    });
    let resolved = resolve(&root, Path::new("."), |name, demand| match demand {
        Demand::Registry { ver, pin } => provider.fetch(name, ver, pin),
        Demand::Path { .. } => Fetched::Refused("the fixture serves no path".to_owned()),
    })
    .expect("the slashed name resolves");
    assert_eq!(
        resolved.deps()["acme/pipeline"].content_hash,
        nested.content_hash()
    );
}

/// The index's rows answer a multi-version offering: the demand takes
/// exactly the version it names, MVS having raised it beforehand.
#[test]
fn a_multi_version_row_file_serves_the_demanded_version() {
    let fixture = fixture();
    let one = package_of(&manifest_of("lib", "1.0.0", &[]), b"fn a() { 1 }");
    let two = package_of(&manifest_of("lib", "1.2.0", &[]), b"fn b() { 2 }");
    index_row(&fixture.index, &one);
    index_row(&fixture.index, &two);
    let root = manifest_of("app", "1.0.0", &[("lib", "1.2.0", two.content_hash())]);
    let registry = Registry::open(&fixture.index);
    let mut provider = StoreProvider::with_remote(&fixture.store, |name, version| {
        registry.fetch(name, version)
    });
    let resolved = resolve(&root, Path::new("."), |name, demand| match demand {
        Demand::Registry { ver, pin } => provider.fetch(name, ver, pin),
        Demand::Path { .. } => Fetched::Refused("the fixture serves no path".to_owned()),
    })
    .expect("the demanded version resolves");
    assert_eq!(resolved.deps()["lib"].version, Version::new(1, 2, 0));
    assert_eq!(resolved.deps()["lib"].content_hash, two.content_hash());
    // the other offering is never fetched: one demand, one fetch
    let mut fetches: BTreeMap<String, Version> = BTreeMap::new();
    let fresh = tempfile::tempdir().expect("fresh cache root");
    let store = Store::open(fresh.path());
    let registry = Registry::open(&fixture.index);
    let mut provider = StoreProvider::with_remote(&store, |name, version| {
        fetches.insert(name.to_owned(), *version);
        registry.fetch(name, version)
    });
    resolve(&root, Path::new("."), |name, demand| match demand {
        Demand::Registry { ver, pin } => provider.fetch(name, ver, pin),
        Demand::Path { .. } => Fetched::Refused("the fixture serves no path".to_owned()),
    })
    .expect("the second cold store resolves");
    assert_eq!(
        fetches,
        BTreeMap::from([("lib".to_owned(), Version::new(1, 2, 0))])
    );
}

/// The artifact storage serves by content hash: found bytes verify
/// against the demanded identity, an unknown identity answers absent,
/// and bytes that do not hash to the identity they sit under refuse
/// hard — the store's own discipline over the registry's files.
#[test]
fn artifacts_serve_by_content_hash_under_the_artifact_segment() {
    let fixture = fixture();
    let registry = Registry::open(&fixture.index);
    let bytes = b"library bytes".to_vec();
    let identity = ds_pkg::artifact_content_hash(&bytes);
    // nothing sits under the identity yet
    assert!(matches!(
        registry.fetch_artifact(identity),
        ArtifactFetch::Absent
    ));
    // land the library and the demand serves, verified
    let path = registry.artifact_path(identity);
    std::fs::create_dir_all(path.parent().expect("the segment dir has a parent"))
        .expect("the artifact segment creates");
    std::fs::write(&path, &bytes).expect("the library lands");
    assert_eq!(
        registry.fetch_artifact(identity),
        ArtifactFetch::Found(bytes.clone())
    );
    // drifted bytes under the identity refuse hard, both hashes named
    std::fs::write(&path, b"drifted").expect("the drift lands");
    let ArtifactFetch::Refused(message) = registry.fetch_artifact(identity) else {
        panic!("drifted storage refuses")
    };
    assert!(message.contains("blake3:"), "{message}");
}
