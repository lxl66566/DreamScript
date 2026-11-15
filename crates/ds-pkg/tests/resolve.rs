//! MVS resolution pinned: the diamond's minimal version set, demand
//! raising with one fetch per `(name, version)`, the refusal taxonomy
//! (missing package, unsatisfiable bound, hash pins, cycles), and lock
//! byte-stability across two resolutions of one graph.

use std::{collections::BTreeMap, fmt::Write};

use ds_diag::FileId;
use ds_mod::Hash128;
use ds_pkg::{Fetched, Manifest, Package, ProvidedPackage, ResolveError, Version, resolve};

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
    let resolved = resolve(&fixtures.root, |name, ver| registry.fetch(name, ver))
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
    let resolved = resolve(&fixtures.root, |name, ver| registry.fetch(name, ver))
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
    let resolved = resolve(&fixtures.root, |name, ver| registry.fetch(name, ver))
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
    let error = resolve(&missing_root, |name, ver| registry.fetch(name, ver))
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
    let error = resolve(&unsat.root, |name, ver| registry.fetch(name, ver))
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
    let error =
        resolve(&drifted, |name, ver| registry.fetch(name, ver)).expect_err("a wrong pin refuses");
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
    let error =
        resolve(&root, |name, ver| registry.fetch(name, ver)).expect_err("conflicting pins refuse");
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
    let error = resolve(&root, |name, _ver| match name {
        "ca" => Fetched::Found(ProvidedPackage {
            manifest: ca.clone(),
            content_hash: hash_a,
        }),
        "cb" => Fetched::Found(ProvidedPackage {
            manifest: cb.clone(),
            content_hash: hash_b,
        }),
        _ => Fetched::UnknownName,
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
    let error = resolve(&self_root, |name, ver| registry.fetch(name, ver))
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
        });
        rows.insert("swap".to_owned(), versions);
        rows
    };
    let root = manifest_of("app", "1.0.0", &[("swap", "1.0.0", other.content_hash())]);
    let mut fetches = Vec::new();
    let error = resolve(&root, |name, ver| {
        fetches.push((name.to_owned(), *ver));
        match rows.get(name) {
            None => Fetched::UnknownName,
            Some(versions) => match versions.get(ver) {
                Some(row) => Fetched::Found(row.clone()),
                None => Fetched::Unoffered { best: None },
            },
        }
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
    let resolved = resolve(&fixtures.root, |name, ver| registry.fetch(name, ver))
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
    let resolved = resolve(&fixtures.root, |name, ver| registry.fetch(name, ver))
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
    };
    let root = manifest_of("app", "1.0.0", &[("swap", "1.0.0", other.content_hash())]);
    let error = resolve(&root, |name, _ver| match name {
        "swap" => Fetched::Found(row.clone()),
        _ => Fetched::UnknownName,
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
    let error =
        resolve(&root, |name, ver| registry.fetch(name, ver)).expect_err("the back edge refuses");
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
        resolve(&fixtures.root, |name, ver| registry.fetch(name, ver))
            .expect("resolves")
            .lock_bytes()
    };
    let second = {
        let mut registry = Registry::new(&fixtures.packages);
        resolve(&fixtures.root, |name, ver| registry.fetch(name, ver))
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
