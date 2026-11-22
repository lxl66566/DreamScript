//! MVS resolution pinned: the diamond's minimal version set, demand
//! raising with one fetch per `(name, version)`, the refusal taxonomy
//! (missing package, unsatisfiable bound, hash pins, cycles), and lock
//! byte-stability across two resolutions of one graph.

use std::{collections::BTreeMap, fmt::Write, path::Path};

use ds_diag::FileId;
use ds_mod::Hash128;
use ds_pkg::{
    Demand, Fetched, Manifest, Package, ProvidedPackage, ResolveError, Store, Version, fetch_path,
    resolve,
};

/// The registry-shaped closure over one static row set: every demand in
/// these fixtures is registry-shaped (the path-demand tests build their
/// own closures over real directories), and a stray path demand refuses
/// with the fixture's own words.
fn registry_only(
    mut rows: impl FnMut(&str, &Version) -> Fetched,
) -> impl FnMut(&str, Demand<'_>) -> Fetched {
    move |name, demand| match demand {
        Demand::Registry { ver, .. } => rows(name, ver),
        Demand::Path { .. } => Fetched::Refused("the fixture registry serves no path".to_owned()),
    }
}

/// Builds a manifest from parts; the canonical text round-trips through
/// [`Manifest::parse`] in the assertions that need spans.
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

/// Builds a package whose content hash pins travel: the manifest member
/// plus one source member, hashed per the bundle cascade.
fn package_of(manifest: &Manifest) -> Package {
    Package::new(manifest, [(
        format!("{}.ds", manifest.name.replace('/', "_")),
        manifest.to_text().into_bytes(),
    )])
    .expect("fixture package builds")
}

/// The provider seam over a static registry: exact `(name, version)`
/// rows, with the fetch log proving what resolution asked for.
struct Registry {
    rows: BTreeMap<String, BTreeMap<Version, ProvidedPackage>>,
    fetches: Vec<(String, Version)>,
}

impl Registry {
    fn new(packages: &[Package]) -> Self {
        let mut rows: BTreeMap<String, BTreeMap<Version, ProvidedPackage>> = BTreeMap::new();
        for package in packages {
            let manifest = package.manifest(FileId::new(0)).expect("fixture parses");
            rows.entry(manifest.name.clone()).or_default().insert(
                manifest.version,
                ProvidedPackage {
                    manifest,
                    content_hash: package.content_hash(),
                    member_identity: package
                        .member_identity()
                        .expect("fixture bundle folds its members"),
                },
            );
        }
        Self {
            rows,
            fetches: Vec::new(),
        }
    }

    fn fetch(&mut self, name: &str, version: &Version) -> Fetched {
        self.fetches.push((name.to_owned(), *version));
        match self.rows.get(name) {
            None => Fetched::UnknownName,
            Some(versions) => match versions.get(version) {
                Some(row) => Fetched::Found(row.clone()),
                None => Fetched::Unoffered {
                    best: versions.keys().next_back().copied(),
                },
            },
        }
    }

    /// The packages keyed by name, for building pins into manifests.
    fn hash_of(packages: &[Package], name: &str) -> Hash128 {
        packages
            .iter()
            .find(|package| {
                package
                    .manifest(FileId::new(0))
                    .is_ok_and(|manifest| manifest.name == name)
            })
            .map(Package::content_hash)
            .expect("fixture package exists")
    }
}

/// One graph's fixtures: every package plus the root manifest built
/// against the packages' real content hashes.
struct Graph {
    packages: Vec<Package>,
    root: Manifest,
}

fn graph(packages: Vec<Package>, root_deps: Vec<(&str, &str)>) -> Graph {
    let mut deps = Vec::new();
    for (name, ver) in root_deps {
        let hash = Registry::hash_of(&packages, name);
        deps.push((name, ver, hash));
    }
    Graph {
        root: manifest_of("app", "1.0.0", &deps),
        packages,
    }
}

#[test]
fn diamond_picks_the_max_of_declared_minima() {
    // base 1.2.0 and 1.1.0 exist; left demands 1.2.0, right demands 1.1.0
    let base_12 = package_of(&manifest_of("base", "1.2.0", &[]));
    let base_11 = package_of(&manifest_of("base", "1.1.0", &[]));
    let left = package_of(&manifest_of("left", "1.0.0", &[(
        "base",
        "1.2.0",
        base_12.content_hash(),
    )]));
    let right = package_of(&manifest_of("right", "1.0.0", &[(
        "base",
        "1.1.0",
        base_11.content_hash(),
    )]));
    let fixtures = graph(vec![base_12, base_11, left, right], vec![
        ("left", "1.0.0"),
        ("right", "1.0.0"),
    ]);
    let mut registry = Registry::new(&fixtures.packages);
    let resolved = resolve(
        &fixtures.root,
        Path::new("."),
        registry_only(|name, ver| registry.fetch(name, ver)),
    )
    .expect("the diamond resolves");
    // max of declared minima: 1.2.0 wins, 1.1.0 never fetched
    assert_eq!(resolved.deps()["base"].version, Version::new(1, 2, 0));
    assert_eq!(resolved.deps()["base"].content_hash, {
        let base = fixtures
            .packages
            .iter()
            .find(|package| {
                package
                    .manifest(FileId::new(0))
                    .is_ok_and(|manifest| manifest.version == Version::new(1, 2, 0))
            })
            .unwrap();
        base.content_hash()
    });
    assert_eq!(registry.fetches, vec![
        ("left".to_owned(), Version::new(1, 0, 0)),
        ("right".to_owned(), Version::new(1, 0, 0)),
        ("base".to_owned(), Version::new(1, 2, 0)),
    ]);
}

#[test]
fn minimal_versions_not_provider_latest() {
    // the provider offers 2.0.0; nothing demands above 1.5.0, so 1.5.0
    // resolves and 2.0.0 is never asked for
    let foo_15 = package_of(&manifest_of("foo", "1.5.0", &[]));
    let foo_20 = package_of(&manifest_of("foo", "2.0.0", &[]));
    let fixtures = graph(vec![foo_15, foo_20], vec![("foo", "1.5.0")]);
    let mut registry = Registry::new(&fixtures.packages);
    let resolved = resolve(
        &fixtures.root,
        Path::new("."),
        registry_only(|name, ver| registry.fetch(name, ver)),
    )
    .expect("resolves at the declared minimum");
    assert_eq!(resolved.deps()["foo"].version, Version::new(1, 5, 0));
    assert!(
        !registry
            .fetches
            .contains(&("foo".to_owned(), Version::new(2, 0, 0)))
    );
}

#[test]
fn a_risen_demand_refetches_the_name_once_per_version() {
    // root demands b 1.0.0; z@1.0.0 (processed after b) raises b to 1.1.0,
    // so b is fetched at both versions and resolves at the raised one
    let b_10 = package_of(&manifest_of("b", "1.0.0", &[]));
    let b_11 = package_of(&manifest_of("b", "1.1.0", &[]));
    let z = package_of(&manifest_of("z", "1.0.0", &[(
        "b",
        "1.1.0",
        b_11.content_hash(),
    )]));
    let fixtures = graph(vec![b_10, b_11, z], vec![("b", "1.0.0"), ("z", "1.0.0")]);
    let mut registry = Registry::new(&fixtures.packages);
    let resolved = resolve(
        &fixtures.root,
        Path::new("."),
        registry_only(|name, ver| registry.fetch(name, ver)),
    )
    .expect("the raised demand resolves");
    assert_eq!(resolved.deps()["b"].version, Version::new(1, 1, 0));
    assert_eq!(registry.fetches, vec![
        ("b".to_owned(), Version::new(1, 0, 0)),
        ("z".to_owned(), Version::new(1, 0, 0)),
        ("b".to_owned(), Version::new(1, 1, 0)),
    ]);
}

#[test]
fn the_taxonomy_refuses() {
    // a missing package: the provider knows no such name (a ghost has no
    // content to pin, so any pin serves — the miss fires first)
    let missing_root = manifest_of("app", "1.0.0", &[(
        "ghost",
        "1.0.0",
        Hash128::new([0x11; 16]),
    )]);
    let mut registry = Registry::new(&[]);
    let error = resolve(
        &missing_root,
        Path::new("."),
        registry_only(|name, ver| registry.fetch(name, ver)),
    )
    .expect_err("unknown name refuses");
    assert_eq!(error, ResolveError::MissingPackage {
        name: "ghost".to_owned(),
        demanded: Version::new(1, 0, 0),
        dependent: "app".to_owned(),
    });

    // an unsatisfiable bound: the name exists, the demanded minimum does not
    let old = package_of(&manifest_of("old", "0.9.0", &[]));
    let unsat = graph(vec![old], vec![("old", "2.0.0")]);
    let mut registry = Registry::new(&unsat.packages);
    let error = resolve(
        &unsat.root,
        Path::new("."),
        registry_only(|name, ver| registry.fetch(name, ver)),
    )
    .expect_err("unoffered version refuses");
    assert_eq!(error, ResolveError::UnsatisfiableBound {
        name: "old".to_owned(),
        demanded: Version::new(2, 0, 0),
        best: Some(Version::new(0, 9, 0)),
        dependent: "app".to_owned(),
    });

    // a drifted pin: the root pins content that is not the package's
    let real = package_of(&manifest_of("real", "1.0.0", &[]));
    let drifted = Manifest::parse(
        &format!(
            "name = \"app\"\nversion = \"1.0.0\"\n\n[deps]\nreal = {{ ver = \"1.0.0\", hash = \
             \"blake3:{}\" }}\n",
            Hash128::new([0xaa; 16])
        ),
        FileId::new(0),
    )
    .unwrap();
    let mut registry = Registry::new(&[real]);
    let error = resolve(
        &drifted,
        Path::new("."),
        registry_only(|name, ver| registry.fetch(name, ver)),
    )
    .expect_err("a wrong pin refuses");
    let ResolveError::HashMismatch { pinned, found, .. } = &error else {
        panic!("expected HashMismatch, got {error:?}")
    };
    assert_eq!(*pinned, Hash128::new([0xaa; 16]));
    assert_ne!(*found, *pinned);

    // two declarers pinning one (name, ver) at two hashes
    let shared = package_of(&manifest_of("shared", "1.0.0", &[]));
    let first = manifest_of("x1", "1.0.0", &[("shared", "1.0.0", shared.content_hash())]);
    let second = manifest_of("x2", "1.0.0", &[(
        "shared",
        "1.0.0",
        Hash128::new([0xbb; 16]),
    )]);
    let x1 = package_of(&first);
    let x2 = package_of(&second);
    let root = manifest_of("app", "1.0.0", &[
        ("x1", "1.0.0", x1.content_hash()),
        ("x2", "1.0.0", x2.content_hash()),
    ]);
    let mut registry = Registry::new(&[shared, x1, x2]);
    let error = resolve(
        &root,
        Path::new("."),
        registry_only(|name, ver| registry.fetch(name, ver)),
    )
    .expect_err("conflicting pins refuse");
    let ResolveError::HashMismatch { version, .. } = &error else {
        panic!("expected HashMismatch, got {error:?}")
    };
    assert_eq!(*version, Version::new(1, 0, 0));

    // a cross-package cycle through fetched manifests: cyclic content is
    // unconstructible in real packages (the hash cascade has no fixed
    // point over a cycle), so a lying provider supplies rows whose pins
    // agree — the fetches validate and the final graph's cycle refuses
    let hash_a = Hash128::new([0x01; 16]);
    let hash_b = Hash128::new([0x02; 16]);
    let ca = manifest_of("ca", "1.0.0", &[("cb", "1.0.0", hash_b)]);
    let cb = manifest_of("cb", "1.0.0", &[("ca", "1.0.0", hash_a)]);
    let root = manifest_of("app", "1.0.0", &[("ca", "1.0.0", hash_a)]);
    let error = resolve(&root, Path::new("."), |name, demand| match demand {
        Demand::Registry { .. } => match name {
            "ca" => Fetched::Found(ProvidedPackage {
                manifest: ca.clone(),
                content_hash: hash_a,
                member_identity: Hash128::ZERO,
            }),
            "cb" => Fetched::Found(ProvidedPackage {
                manifest: cb.clone(),
                content_hash: hash_b,
                member_identity: Hash128::ZERO,
            }),
            _ => Fetched::UnknownName,
        },
        Demand::Path { .. } => Fetched::Refused("the fixture serves no path".to_owned()),
    })
    .expect_err("the cycle refuses");
    assert_eq!(error, ResolveError::Cycle {
        cycle: vec!["ca".to_owned(), "cb".to_owned(), "ca".to_owned()]
    });

    // a dep naming the root package itself: a cycle, never a fetch
    let dep = package_of(&manifest_of("selfdep", "1.0.0", &[]));
    let self_root = manifest_of("app", "1.0.0", &[
        ("selfdep", "1.0.0", dep.content_hash()),
        ("app", "1.0.0", Hash128::ZERO),
    ]);
    let mut registry = Registry::new(&[dep]);
    let error = resolve(
        &self_root,
        Path::new("."),
        registry_only(|name, ver| registry.fetch(name, ver)),
    )
    .expect_err("the root self-dep refuses");
    assert_eq!(error, ResolveError::Cycle {
        cycle: vec!["app".to_owned(), "app".to_owned()]
    });
    assert!(
        !registry.fetches.iter().any(|(name, _)| name == "app"),
        "the root is never fetched"
    );
}

#[test]
fn a_served_mismatch_is_the_demand_unserved() {
    // the provider answers the demanded pair with another version's
    // content: the load walk's same-miss rule
    let other = package_of(&manifest_of("swap", "1.0.1", &[]));
    let rows = {
        let mut rows: BTreeMap<String, BTreeMap<Version, ProvidedPackage>> = BTreeMap::new();
        let mut versions = BTreeMap::new();
        versions.insert(Version::new(1, 0, 0), ProvidedPackage {
            manifest: other.manifest(FileId::new(0)).unwrap(),
            content_hash: other.content_hash(),
            member_identity: other.member_identity().unwrap(),
        });
        rows.insert("swap".to_owned(), versions);
        rows
    };
    let root = manifest_of("app", "1.0.0", &[("swap", "1.0.0", other.content_hash())]);
    let mut fetches = Vec::new();
    let error = resolve(&root, Path::new("."), |name, demand| match demand {
        Demand::Registry { ver, .. } => {
            fetches.push((name.to_owned(), *ver));
            match rows.get(name) {
                None => Fetched::UnknownName,
                Some(versions) => match versions.get(ver) {
                    Some(row) => Fetched::Found(row.clone()),
                    None => Fetched::Unoffered { best: None },
                },
            }
        },
        Demand::Path { .. } => Fetched::Refused("the fixture serves no path".to_owned()),
    })
    .expect_err("the version disagreement refuses");
    assert_eq!(error, ResolveError::MissingPackage {
        name: "swap".to_owned(),
        demanded: Version::new(1, 0, 0),
        dependent: "app".to_owned(),
    });
}

#[test]
fn two_raises_before_the_fetch_read_one_demand() {
    // root demands a then x; a raises b to 1.0.0, x to 1.1.0 — both raises
    // land while b's first entry is still queued, so b is fetched once, at
    // the final demand 1.1.0 (never at 1.0.0)
    let b_10 = package_of(&manifest_of("b", "1.0.0", &[]));
    let b_11 = package_of(&manifest_of("b", "1.1.0", &[]));
    let a = package_of(&manifest_of("a", "1.0.0", &[(
        "b",
        "1.0.0",
        b_10.content_hash(),
    )]));
    let x = package_of(&manifest_of("x", "1.0.0", &[(
        "b",
        "1.1.0",
        b_11.content_hash(),
    )]));
    let fixtures = graph(vec![b_10, b_11, a, x], vec![("a", "1.0.0"), ("x", "1.0.0")]);
    let mut registry = Registry::new(&fixtures.packages);
    let resolved = resolve(
        &fixtures.root,
        Path::new("."),
        registry_only(|name, ver| registry.fetch(name, ver)),
    )
    .expect("the co-raised demand resolves");
    assert_eq!(resolved.deps()["b"].version, Version::new(1, 1, 0));
    assert_eq!(registry.fetches, vec![
        ("a".to_owned(), Version::new(1, 0, 0)),
        ("x".to_owned(), Version::new(1, 0, 0)),
        ("b".to_owned(), Version::new(1, 1, 0)),
    ]);
}

#[test]
fn agreeing_pins_at_one_version_resolve() {
    // two declarers pin the same (name, ver) at the same content: no
    // disagreement, one resolution
    let shared = package_of(&manifest_of("shared", "1.0.0", &[]));
    let shared_hash = shared.content_hash();
    let left = package_of(&manifest_of("left", "1.0.0", &[(
        "shared",
        "1.0.0",
        shared_hash,
    )]));
    let right = package_of(&manifest_of("right", "1.0.0", &[(
        "shared",
        "1.0.0",
        shared_hash,
    )]));
    let fixtures = graph(vec![shared, left, right], vec![
        ("left", "1.0.0"),
        ("right", "1.0.0"),
    ]);
    let mut registry = Registry::new(&fixtures.packages);
    let resolved = resolve(
        &fixtures.root,
        Path::new("."),
        registry_only(|name, ver| registry.fetch(name, ver)),
    )
    .expect("agreeing pins resolve");
    assert_eq!(resolved.deps()["shared"].version, Version::new(1, 0, 0));
    assert_eq!(resolved.deps()["shared"].content_hash, shared_hash);
    // one fetch for the name both declarers demanded
    assert_eq!(registry.fetches, vec![
        ("left".to_owned(), Version::new(1, 0, 0)),
        ("right".to_owned(), Version::new(1, 0, 0)),
        ("shared".to_owned(), Version::new(1, 0, 0)),
    ]);
}

#[test]
fn content_under_another_name_is_the_demand_unserved() {
    // the provider answers the demanded pair with a manifest naming
    // another package: the load walk's same-miss rule
    let other = package_of(&manifest_of("other", "1.0.0", &[]));
    let row = ProvidedPackage {
        manifest: other.manifest(FileId::new(0)).unwrap(),
        content_hash: other.content_hash(),
        member_identity: other.member_identity().unwrap(),
    };
    let root = manifest_of("app", "1.0.0", &[("swap", "1.0.0", other.content_hash())]);
    let error = resolve(&root, Path::new("."), |name, demand| match demand {
        Demand::Registry { .. } => match name {
            "swap" => Fetched::Found(row.clone()),
            _ => Fetched::UnknownName,
        },
        Demand::Path { .. } => Fetched::Refused("the fixture serves no path".to_owned()),
    })
    .expect_err("the name disagreement refuses");
    assert_eq!(error, ResolveError::MissingPackage {
        name: "swap".to_owned(),
        demanded: Version::new(1, 0, 0),
        dependent: "app".to_owned(),
    });
}

#[test]
fn a_resolved_dep_on_the_root_closes_a_cycle() {
    // a resolved package depending on the root closes the cycle through
    // the root node: the root is never fetched, never resolvable
    let back = package_of(&manifest_of("back", "1.0.0", &[(
        "app",
        "1.0.0",
        Hash128::ZERO,
    )]));
    let root = manifest_of("app", "1.0.0", &[("back", "1.0.0", back.content_hash())]);
    let mut registry = Registry::new(&[back]);
    let error = resolve(
        &root,
        Path::new("."),
        registry_only(|name, ver| registry.fetch(name, ver)),
    )
    .expect_err("the back edge refuses");
    assert_eq!(error, ResolveError::Cycle {
        cycle: vec!["app".to_owned(), "back".to_owned(), "app".to_owned()]
    });
}

#[test]
fn two_resolutions_produce_byte_identical_locks() {
    let base = package_of(&manifest_of("base", "1.3.0", &[]));
    let left = package_of(&manifest_of("left", "1.1.0", &[(
        "base",
        "1.2.0",
        base.content_hash(),
    )]));
    let right = package_of(&manifest_of("right", "2.0.0", &[(
        "base",
        "1.3.0",
        base.content_hash(),
    )]));
    let fixtures = graph(vec![base, left, right], vec![
        ("left", "1.1.0"),
        ("right", "2.0.0"),
    ]);
    let first = {
        let mut registry = Registry::new(&fixtures.packages);
        resolve(
            &fixtures.root,
            Path::new("."),
            registry_only(|name, ver| registry.fetch(name, ver)),
        )
        .expect("resolves")
        .lock_bytes()
    };
    let second = {
        let mut registry = Registry::new(&fixtures.packages);
        resolve(
            &fixtures.root,
            Path::new("."),
            registry_only(|name, ver| registry.fetch(name, ver)),
        )
        .expect("resolves again")
        .lock_bytes()
    };
    assert_eq!(first, second);
    // the lock carries every manifest of the graph: app, base, left, right
    let count = u32::from_le_bytes(first[0..4].try_into().unwrap());
    assert_eq!(count, 4);
    for name in ["app", "base", "left", "right"] {
        assert!(
            first
                .windows(name.len())
                .any(|window| window == name.as_bytes()),
            "the lock names {name}"
        );
    }
    // the lock's manifest texts round-trip through the model
    let text = String::from_utf8_lossy(&first).into_owned();
    assert!(text.contains("name = \"base\"\nversion = \"1.3.0\"\n"));
}

/// Builds a manifest with deps plus one artifact declaration.
fn manifest_with_artifact(
    name: &str,
    version: &str,
    deps: &[(&str, &str, Hash128)],
    artifact: &str,
    art_ver: &str,
    target: &str,
    hash: Hash128,
) -> Manifest {
    let base = manifest_of(name, version, deps);
    let text = format!(
        "{}\n[artifacts]\n{artifact} = {{ ver = \"{art_ver}\", target = \"{target}\", hash = \
         \"blake3:{hash}\" }}\n",
        base.to_text()
    );
    Manifest::parse(&text, FileId::new(0)).expect("fixture manifest parses")
}

#[test]
fn artifacts_fold_from_the_root_and_deps_under_one_name() {
    // root and lib both declare `mymath` at the same identity: the graph
    // carries one artifact, every declarer agreeing
    let art = Hash128::new([0x44; 16]);
    let lib = package_of(&manifest_with_artifact(
        "lib",
        "1.0.0",
        &[],
        "mymath",
        "1.2.0",
        "x86_64-pc-windows-msvc",
        art,
    ));
    let root = manifest_with_artifact(
        "app",
        "1.0.0",
        &[("lib", "1.0.0", lib.content_hash())],
        "mymath",
        "1.2.0",
        "x86_64-pc-windows-msvc",
        art,
    );
    let mut registry = Registry::new(&[lib]);
    let resolved = resolve(
        &root,
        Path::new("."),
        registry_only(|name, ver| registry.fetch(name, ver)),
    )
    .expect("resolves");
    let folded = resolved.artifacts();
    assert_eq!(folded.len(), 1);
    assert_eq!(folded["mymath"].ver, Version::new(1, 2, 0));
    assert_eq!(folded["mymath"].target, "x86_64-pc-windows-msvc");
    assert_eq!(folded["mymath"].hash, art);
    // and the artifact's pin rides the lock: the manifest texts carry it
    let lock = String::from_utf8_lossy(&resolved.lock_bytes()).into_owned();
    assert!(lock.contains("[artifacts]"));
    assert!(lock.contains("mymath"));
}

#[test]
fn a_second_artifact_identity_under_one_name_refuses() {
    // lib pins `mymath` at one hash, the root at another: one version
    // per name this round, the refusal naming both declarers
    let pinned = Hash128::new([0x44; 16]);
    let found = Hash128::new([0x55; 16]);
    let lib = package_of(&manifest_with_artifact(
        "lib",
        "1.0.0",
        &[],
        "mymath",
        "1.2.0",
        "x86_64-pc-windows-msvc",
        pinned,
    ));
    let root = manifest_with_artifact(
        "app",
        "1.0.0",
        &[("lib", "1.0.0", lib.content_hash())],
        "mymath",
        "2.0.0",
        "aarch64-unknown-linux-gnu",
        found,
    );
    let mut registry = Registry::new(&[lib]);
    let error = resolve(
        &root,
        Path::new("."),
        registry_only(|name, ver| registry.fetch(name, ver)),
    )
    .expect_err("the conflict refuses");
    let ResolveError::ArtifactConflict {
        name,
        pinned: pin,
        found: other,
        dependent,
        declarer,
    } = &error
    else {
        panic!("expected ArtifactConflict, got {error:?}")
    };
    assert_eq!(name, "mymath");
    // the root records first, so its pin is the recorded one and lib's
    // the disagreeing bound
    assert_eq!(pin.hash, found);
    assert_eq!(other.hash, pinned);
    assert_eq!(pin.ver, Version::new(2, 0, 0));
    assert_eq!(other.target, "x86_64-pc-windows-msvc");
    assert_eq!(dependent, "app");
    assert_eq!(declarer, "lib");
    // the message names both identities and the one-slot rule
    let message = error.to_string();
    assert!(message.contains("blake3:44"), "{message}");
    assert!(message.contains("blake3:55"), "{message}");
    assert!(message.contains("one version per name"), "{message}");
}

#[test]
fn a_lifted_dep_retires_its_superseded_artifact_pin() {
    // c@1.0.0 and c@2.0.0 each declare `libfoo` at different hashes; d
    // lifts c to 2.0.0 after the walk fetched 1.0.0 — the retired
    // version's declaration never pins: the fold reads the settled
    // graph, so the diamond resolves with c@2.0.0's pin instead of
    // fabricating a conflict with c@1.0.0's
    let pin_1 = Hash128::new([0x11; 16]);
    let pin_2 = Hash128::new([0x22; 16]);
    let c_10 = package_of(&manifest_with_artifact(
        "c",
        "1.0.0",
        &[],
        "libfoo",
        "1.0.0",
        "x86_64-pc-windows-msvc",
        pin_1,
    ));
    let c_20 = package_of(&manifest_with_artifact(
        "c",
        "2.0.0",
        &[],
        "libfoo",
        "1.0.0",
        "x86_64-pc-windows-msvc",
        pin_2,
    ));
    let d = package_of(&manifest_of("d", "1.0.0", &[(
        "c",
        "2.0.0",
        c_20.content_hash(),
    )]));
    let root = manifest_of("app", "1.0.0", &[
        ("c", "1.0.0", c_10.content_hash()),
        ("d", "1.0.0", d.content_hash()),
    ]);
    let mut registry = Registry::new(&[c_10, c_20, d]);
    let resolved = resolve(
        &root,
        Path::new("."),
        registry_only(|name, ver| registry.fetch(name, ver)),
    )
    .expect("the lifted diamond resolves");
    assert_eq!(resolved.deps()["c"].version, Version::new(2, 0, 0));
    assert_eq!(resolved.artifacts()["libfoo"].hash, pin_2);
}

#[test]
fn an_artifact_edit_rekeys_the_lock() {
    // two roots differing only in the artifact's pin produce different
    // locks: the artifact identity flows into the lock bytes
    let one = Hash128::new([0x44; 16]);
    let two = Hash128::new([0x55; 16]);
    let lock_of = |hash| {
        let root = manifest_with_artifact(
            "app",
            "1.0.0",
            &[],
            "mymath",
            "1.2.0",
            "x86_64-pc-windows-msvc",
            hash,
        );
        let mut registry = Registry::new(&[]);
        resolve(
            &root,
            Path::new("."),
            registry_only(|name, ver| registry.fetch(name, ver)),
        )
        .expect("an artifact-only root resolves")
        .lock_bytes()
    };
    assert_ne!(lock_of(one), lock_of(two));
    assert_eq!(lock_of(one), lock_of(one));
}

/// Writes one directory-shaped package: the manifest plus its members.
fn write_dir_package(dir: &Path, manifest: &str, members: &[(&str, &str)]) {
    std::fs::create_dir_all(dir).expect("the package dir creates");
    std::fs::write(dir.join("ds.pkg"), manifest).expect("the manifest writes");
    for (path, text) in members {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().expect("the member has a parent"))
            .expect("the member dir creates");
        std::fs::write(file, text).expect("the member writes");
    }
}

/// A manifest whose deps are all path-shaped.
fn path_manifest(name: &str, version: &str, deps: &[(&str, &str)]) -> Manifest {
    let mut text = format!("name = \"{name}\"\nversion = \"{version}\"\n");
    for (dep, path) in deps {
        let _ = writeln!(text, "\n[deps.{dep}]\npath = \"{path}\"");
    }
    Manifest::parse(&text, FileId::new(0)).expect("fixture manifest parses")
}

/// The path-demand closure over a real store: every registry demand
/// reports unknown, every path demand reads the directory through the
/// provider's own fetch.
fn path_only(store: &Store) -> impl FnMut(&str, Demand<'_>) -> Fetched + '_ {
    move |name, demand| match demand {
        Demand::Registry { .. } => {
            let _ = name;
            Fetched::UnknownName
        },
        Demand::Path { dir } => fetch_path(store, dir),
    }
}

/// A path dep resolves from source: the directory's members frame the
/// same canonical bundle `ds save` lands, the content identity keys the
/// store (a cache fill the resolution can serve again), and the
/// resolved row carries the directory and the directory's own version.
#[test]
fn a_path_dep_resolves_from_source_through_the_store_keys() {
    let work = tempfile::tempdir().expect("a temp dir");
    let cache = work.path().join("cache");
    let store = Store::open(&cache);
    let root = work.path().join("root");
    let lib = work.path().join("lib");
    write_dir_package(&lib, "name = \"@lib\"\nversion = \"1.4.0\"\n", &[(
        "helper.ds",
        "pub fn base() -> int {\n21\n}\n",
    )]);
    write_dir_package(
        &root,
        "name = \"app\"\nversion = \"0.1.0\"\n\n[deps.@lib]\npath = \"../lib\"\n",
        &[("main.ds", "fn main() {}\n")],
    );
    let manifest = path_manifest("app", "0.1.0", &[("@lib", "../lib")]);
    let resolved = resolve(&manifest, &root, path_only(&store)).expect("the path dep resolves");
    let package = resolved.deps()["@lib"].clone();
    assert_eq!(package.version, Version::new(1, 4, 0));
    assert_eq!(package.dir.as_deref(), Some(lib.as_path()));
    // the identity is the ds-save twin's: the same members, the same
    // canonical bundle
    let twin = Package::new(
        &Manifest::parse("name = \"@lib\"\nversion = \"1.4.0\"\n", FileId::new(0)).unwrap(),
        [(
            "helper.ds".to_owned(),
            b"pub fn base() -> int {\n21\n}\n".to_vec(),
        )],
    )
    .unwrap();
    assert_eq!(package.content_hash, twin.content_hash());
    assert_eq!(
        package.member_identity,
        twin.member_identity().expect("the twin folds")
    );
    // the store holds the cache fill under the identity
    assert!(store.load(package.content_hash).unwrap().is_some());
    // and a second resolution serves the same row from the source again
    let again = resolve(&manifest, &root, path_only(&store)).unwrap();
    assert_eq!(again.deps()["@lib"].content_hash, package.content_hash);
}

/// Path deps nest: a path package's own path deps resolve against ITS
/// directory, and the resolved graph's path set answers the transitive
/// watch roots in ascending order.
#[test]
fn transitive_path_deps_resolve_against_their_own_directories() {
    let work = tempfile::tempdir().expect("a temp dir");
    let store = Store::open(&work.path().join("cache"));
    let root = work.path().join("root");
    let lib = work.path().join("lib");
    let deep = work.path().join("deep");
    write_dir_package(&deep, "name = \"@deep\"\nversion = \"1.0.0\"\n", &[(
        "deep.ds",
        "pub fn base() -> int {\n21\n}\n",
    )]);
    write_dir_package(
        &lib,
        "name = \"@lib\"\nversion = \"1.0.0\"\n\n[deps.@deep]\npath = \"../deep\"\n",
        &[("lib.ds", "import @deep (base)\n")],
    );
    write_dir_package(&root, "name = \"app\"\nversion = \"0.1.0\"\n", &[(
        "main.ds",
        "fn main() {}\n",
    )]);
    let manifest = path_manifest("app", "0.1.0", &[("@lib", "../lib")]);
    let resolved = resolve(&manifest, &root, path_only(&store)).expect("the chain resolves");
    assert_eq!(resolved.deps().len(), 2);
    assert_eq!(
        resolved.path_dirs(),
        vec![
            deep.canonicalize().expect("the deep dir resolves"),
            lib.canonicalize().expect("the lib dir resolves")
        ],
        "the watch set is the transitive path closure, ascending"
    );
}

/// A path dep naming the root (a sibling spelling of the package under
/// development) closes the cycle the final graph's edges read.
#[test]
fn a_path_dep_cycle_refuses() {
    let work = tempfile::tempdir().expect("a temp dir");
    let store = Store::open(&work.path().join("cache"));
    let root = work.path().join("root");
    let side = work.path().join("side");
    write_dir_package(
        &side,
        "name = \"side\"\nversion = \"1.0.0\"\n\n[deps.app]\npath = \"../root\"\n",
        &[("side.ds", "fn member() {}\n")],
    );
    write_dir_package(&root, "name = \"app\"\nversion = \"0.1.0\"\n", &[(
        "main.ds",
        "fn main() {}\n",
    )]);
    let manifest = path_manifest("app", "0.1.0", &[("side", "../side")]);
    let error = resolve(&manifest, &root, path_only(&store)).expect_err("the cycle refuses");
    assert!(matches!(error, ResolveError::Cycle { .. }), "{error}");
}

/// One name at both shapes refuses, one name at two directories refuses,
/// and a served (directory-less) package declaring a path dep refuses.
#[test]
fn path_shape_conflicts_refuse() {
    let work = tempfile::tempdir().expect("a temp dir");
    let store = Store::open(&work.path().join("cache"));
    let root = work.path().join("root");
    let x1 = work.path().join("x1");
    let x2 = work.path().join("x2");
    let y = work.path().join("y");
    write_dir_package(&x1, "name = \"@x\"\nversion = \"1.0.0\"\n", &[(
        "x.ds",
        "fn member() {}\n",
    )]);
    write_dir_package(&x2, "name = \"@x\"\nversion = \"1.0.0\"\n", &[(
        "x.ds",
        "fn other() {}\n",
    )]);
    // y path-deps @x at ../x1 (a different directory than the root's x2)
    write_dir_package(
        &y,
        "name = \"@y\"\nversion = \"1.0.0\"\n\n[deps.@x]\npath = \"../x1\"\n",
        &[("y.ds", "fn member() {}\n")],
    );
    write_dir_package(&root, "name = \"app\"\nversion = \"0.1.0\"\n", &[(
        "main.ds",
        "fn main() {}\n",
    )]);

    // the root path-deps @x at ../x2 while @y demands it at ../x1
    let manifest = path_manifest("app", "0.1.0", &[("@x", "../x2"), ("@y", "../y")]);
    let error =
        resolve(&manifest, &root, path_only(&store)).expect_err("the two directories refuse");
    assert!(
        matches!(error, ResolveError::PathConflict { .. }),
        "{error}"
    );

    // one name at both shapes across declarers: the root path-demands
    // @x while a served registry package pins it
    let served = ProvidedPackage {
        manifest: Manifest::parse(
            &format!(
                "name = \"@reg\"\nversion = \"1.0.0\"\n\n[deps]\nx = {{ ver = \"1.0.0\", hash = \
                 \"blake3:{}\" }}\n",
                Hash128::ZERO
            ),
            FileId::new(0),
        )
        .unwrap(),
        content_hash: Hash128::new([0x77; 16]),
        member_identity: Hash128::ZERO,
    };
    let mut text = String::from("name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n");
    let _ = writeln!(
        text,
        "@reg = {{ ver = \"1.0.0\", hash = \"blake3:{}\" }}",
        Hash128::new([0x77; 16])
    );
    text.push_str("\n[deps.x]\npath = \"../x1\"\n");
    let manifest = Manifest::parse(&text, FileId::new(0)).unwrap();
    let error = resolve(&manifest, &root, |name, demand| match (name, demand) {
        ("@reg", Demand::Registry { .. }) => Fetched::Found(served.clone()),
        (_, Demand::Registry { .. }) => Fetched::UnknownName,
        (_, Demand::Path { dir }) => fetch_path(&store, dir),
    })
    .expect_err("the mixed shapes refuse");
    assert!(matches!(error, ResolveError::MixedDemand { .. }), "{error}");

    // a served package declaring a path dep of its own (nothing else
    // demanding the name): the orphan refusal, never a fetch
    let orphan = ProvidedPackage {
        manifest: Manifest::parse(
            "name = \"@reg\"\nversion = \"1.0.0\"\n\n[deps.@z]\npath = \"../nowhere\"\n",
            FileId::new(0),
        )
        .unwrap(),
        content_hash: Hash128::new([0x77; 16]),
        member_identity: Hash128::ZERO,
    };
    let manifest = Manifest::parse(
        &format!(
            "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@reg = {{ ver = \"1.0.0\", hash = \
             \"blake3:{}\" }}\n",
            Hash128::new([0x77; 16])
        ),
        FileId::new(0),
    )
    .unwrap();
    let error = resolve(&manifest, &root, |name, demand| match (name, demand) {
        ("@reg", Demand::Registry { .. }) => Fetched::Found(orphan.clone()),
        (_, Demand::Registry { .. }) => Fetched::UnknownName,
        (_, Demand::Path { dir }) => fetch_path(&store, dir),
    })
    .expect_err("the orphan path refuses");
    assert!(matches!(error, ResolveError::OrphanPath { .. }), "{error}");
}

/// A path target that shapes no package refuses with the directory's
/// own words (the fetch's refusal rides verbatim).
#[test]
fn a_broken_path_target_refuses_with_the_fetches_words() {
    let work = tempfile::tempdir().expect("a temp dir");
    let store = Store::open(&work.path().join("cache"));
    let root = work.path().join("root");
    write_dir_package(&root, "name = \"app\"\nversion = \"0.1.0\"\n", &[(
        "main.ds",
        "fn main() {}\n",
    )]);
    let manifest = path_manifest("app", "0.1.0", &[("@ghost", "../ghost")]);
    let error = resolve(&manifest, &root, path_only(&store)).expect_err("the missing dir refuses");
    assert!(matches!(error, ResolveError::PathRefused { .. }), "{error}");
    assert!(error.to_string().contains("ds.pkg"), "{error}");

    // a present directory whose manifest names another package refuses
    // the name mismatch
    let wrong = work.path().join("wrong");
    write_dir_package(&wrong, "name = \"@other\"\nversion = \"1.0.0\"\n", &[(
        "wrong.ds",
        "fn member() {}\n",
    )]);
    let manifest = path_manifest("app", "0.1.0", &[("@right", "../wrong")]);
    let error =
        resolve(&manifest, &root, path_only(&store)).expect_err("the name mismatch refuses");
    assert!(matches!(error, ResolveError::PathRefused { .. }), "{error}");
    assert!(error.to_string().contains("names itself"), "{error}");
}
