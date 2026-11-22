//! The store-backed provider seam pinned: resolution over the store
//! fetches a missing bundle once and never refetches a valid one, a
//! corrupt cached bundle refetches through the remote (and refuses
//! without one), the taxonomy of unserved demands derives from the
//! cache's own knowledge when the remote cannot speak, and the resolved
//! graph composes with the deps-first load walk — the store's bundle
//! serving the linked module's source identity end to end.

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
};

use ds_diag::FileId;
use ds_mod::{Constant, FuncRefKey, Function, Hash128, Module, TargetClass};
use ds_pkg::{
    Demand, Fetched, Manifest, Package, Remote, Resolved, Store, StoreProvider, Version, resolve,
};
use ds_vm::{
    FrameAnswer, ModuleRegistry, Value, Vm,
    abi::{ConstIdx, FrameLayout, SlotType::I64, uop::Uop},
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

/// Builds a package over a manifest with one source member under the
/// name's slashed spelling.
fn package_of(manifest: &Manifest, source: &[u8]) -> Package {
    Package::new(manifest, [(
        format!("{}.ds", manifest.name.replace('/', "_")),
        source.to_vec(),
    )])
    .expect("fixture package builds")
}

/// A remote registry fixture: bundles written to a scratch directory
/// beside the cache root, the hook reading the demanded pair's file —
/// the fetch count proving what resolution asked the remote for.
struct RemoteRegistry {
    dir: PathBuf,
    fetches: Vec<(String, Version)>,
}

impl RemoteRegistry {
    /// Writes every bundle to the registry directory, creating it.
    fn new(dir: PathBuf, packages: &[Package]) -> Self {
        std::fs::create_dir_all(&dir).expect("the registry directory creates");
        for package in packages {
            let manifest = package.manifest(FileId::new(0)).expect("fixture parses");
            std::fs::write(
                dir.join(format!("{}-{}.pkg", manifest.name, manifest.version)),
                package.to_bytes(),
            )
            .expect("the registry row writes");
        }
        Self {
            dir,
            fetches: Vec::new(),
        }
    }

    /// The hook: one counted fetch per demand, the file's bundle decoded
    /// or the name unknown.
    fn fetch(&mut self, name: &str, version: &Version) -> Remote {
        self.fetches.push((name.to_owned(), *version));
        match std::fs::read(self.dir.join(format!("{name}-{version}.pkg"))) {
            Ok(bytes) => match Package::from_bytes(&bytes) {
                Ok(package) => Remote::Found(package),
                Err(_) => Remote::UnknownName,
            },
            Err(_) => Remote::UnknownName,
        }
    }
}

/// A scratch directory beside the store's cache root, so the remote
/// registry never writes inside the cache.
fn remote_dir(store: &Store) -> PathBuf {
    store
        .dir()
        .parent()
        .map(|root| root.join("remote"))
        .expect("the store's dir always has a parent")
}

/// The two-package fixture: an app package pinning a lib package, over
/// a fresh store under a temp cache root.
struct Fixture {
    // the temp cache root outlives the store's files (its removal is the teardown)
    _cache: tempfile::TempDir,
    store: Store,
    root: Manifest,
    lib: Package,
    app: Package,
}

fn fixture() -> Fixture {
    let cache = tempfile::tempdir().expect("temp cache root");
    let store = Store::open(cache.path());
    let lib = package_of(
        &manifest_of("lib", "1.0.0", &[]),
        b"fn inc(x: int) { x + 37 }",
    );
    let lib_hash = lib.content_hash();
    let root = manifest_of("app", "1.0.0", &[("lib", "1.0.0", lib_hash)]);
    let app = package_of(&root, b"fn main() { lib(5).inc() }");
    Fixture {
        _cache: cache,
        store,
        root,
        lib,
        app,
    }
}

/// Resolves one root over the fixture's store through a remote registry
/// holding `packages`, answering the resolution and the fetch log.
fn resolve_with(fixture: &Fixture, packages: &[Package]) -> (Resolved, RemoteRegistry) {
    let mut remote = RemoteRegistry::new(remote_dir(&fixture.store), packages);
    let mut provider =
        StoreProvider::with_remote(&fixture.store, |name, version| remote.fetch(name, version));
    let resolved = resolve(&fixture.root, Path::new("."), |name, demand| match demand {
        Demand::Registry { ver, pin } => provider.fetch(name, ver, pin),
        Demand::Path { .. } => Fetched::Refused("the fixture serves no path".to_owned()),
    })
    .expect("the fixture resolves");
    (resolved, remote)
}

#[test]
fn a_valid_bundle_is_fetched_once_and_never_again() {
    let fixture = fixture();
    let (resolved, remote) = resolve_with(&fixture, std::slice::from_ref(&fixture.lib));
    // the remote served the one demand, the store installed the bundle
    assert_eq!(remote.fetches, [("lib".to_owned(), Version::new(1, 0, 0))]);
    assert_eq!(
        fixture.store.load(fixture.lib.content_hash()).unwrap(),
        Some(fixture.lib.clone())
    );
    // the resolution validated the winning pin against the served content
    assert_eq!(
        resolved.deps()["lib"].content_hash,
        fixture.lib.content_hash()
    );
    // a second resolution over the same store never touches the remote:
    // the registry serves nothing at all
    let mut empty = RemoteRegistry::new(remote_dir(&fixture.store), &[]);
    let mut provider =
        StoreProvider::with_remote(&fixture.store, |name, version| empty.fetch(name, version));
    let again = resolve(&fixture.root, Path::new("."), |name, demand| match demand {
        Demand::Registry { ver, pin } => provider.fetch(name, ver, pin),
        Demand::Path { .. } => Fetched::Refused("the fixture serves no path".to_owned()),
    })
    .expect("the store alone resolves");
    assert_eq!(empty.fetches, [] as [(String, Version); 0]);
    assert_eq!(again.lock_bytes(), resolved.lock_bytes());
}

#[test]
fn the_seam_carries_each_dep_s_member_identity() {
    let fixture = fixture();
    // the fixture's lib folds one member, so the seam's identity is the
    // raw member hash — the same value a cache probe keys with, served
    // without loading the bundle again
    let expected = Hash128::compute(b"fn inc(x: int) { x + 37 }");
    let (resolved, _remote) = resolve_with(&fixture, std::slice::from_ref(&fixture.lib));
    assert_eq!(resolved.deps()["lib"].member_identity, expected);
    // a second, cache-only provider over the installed rows serves the
    // same identity (the fold landed with the row)
    let mut provider = StoreProvider::cached(&fixture.store);
    let again = resolve(&fixture.root, Path::new("."), |name, demand| match demand {
        Demand::Registry { ver, pin } => provider.fetch(name, ver, pin),
        Demand::Path { .. } => Fetched::Refused("the fixture serves no path".to_owned()),
    })
    .expect("the store alone resolves");
    assert_eq!(again.deps()["lib"].member_identity, expected);
    // a multi-member dep composes the ascending-path fold through the
    // seam, fetched and installed alike
    let multi = Package::new(&manifest_of("multi", "1.0.0", &[]), vec![
        ("b.ds".to_owned(), b"two".to_vec()),
        ("a.ds".to_owned(), b"one".to_vec()),
    ])
    .expect("fixture bundle builds");
    let root = manifest_of("app", "1.0.0", &[("multi", "1.0.0", multi.content_hash())]);
    let mut remote = RemoteRegistry::new(remote_dir(&fixture.store), std::slice::from_ref(&multi));
    let mut provider =
        StoreProvider::with_remote(&fixture.store, |name, version| remote.fetch(name, version));
    let resolved = resolve(&root, Path::new("."), |name, demand| match demand {
        Demand::Registry { ver, pin } => provider.fetch(name, ver, pin),
        Demand::Path { .. } => Fetched::Refused("the fixture serves no path".to_owned()),
    })
    .expect("the multi-member fixture resolves");
    assert_eq!(
        resolved.deps()["multi"].member_identity,
        multi.member_identity().unwrap()
    );
}

#[test]
fn a_fetched_bundle_without_a_servable_member_set_refuses() {
    let fixture = fixture();
    // a hook serving a bundle whose member is not UTF-8 text: the
    // member fold refuses and the seam carries the hard refusal
    let unservable = Package::new(&manifest_of("lib", "1.0.0", &[]), [(
        "lib.ds".to_owned(),
        vec![0xff, 0xfe],
    )])
    .expect("the bundle builds over raw bytes");
    let root = manifest_of("app", "1.0.0", &[(
        "lib",
        "1.0.0",
        unservable.content_hash(),
    )]);
    let refused = unservable.clone();
    let mut provider =
        StoreProvider::with_remote(&fixture.store, move |_, _| Remote::Found(refused.clone()));
    let error = resolve(&root, Path::new("."), |name, demand| match demand {
        Demand::Registry { ver, pin } => provider.fetch(name, ver, pin),
        Demand::Path { .. } => Fetched::Refused("the fixture serves no path".to_owned()),
    })
    .expect_err("the unservable member set refuses");
    assert_eq!(
        error.to_string(),
        "`lib` at 1.0.0: the fetched bundle for `lib` does not serve a member set: member \
         `lib.ds` is not UTF-8 text"
    );
    // the refused bundle never installed
    assert_eq!(fixture.store.load(unservable.content_hash()).unwrap(), None);
}

#[test]
fn a_corrupt_cached_bundle_refetches_and_repairs() {
    let fixture = fixture();
    let (_, remote) = resolve_with(&fixture, std::slice::from_ref(&fixture.lib));
    assert_eq!(remote.fetches.len(), 1);
    // corrupt the installed bytes: the file's own hash no longer answers
    // its identity's claim
    std::fs::write(
        fixture.store.path_of(fixture.lib.content_hash()),
        b"corrupt bytes",
    )
    .expect("the corruption lands");
    // the next resolution refetches and the repair lands in the store
    let (_, remote) = resolve_with(&fixture, std::slice::from_ref(&fixture.lib));
    assert_eq!(remote.fetches.len(), 1);
    assert_eq!(
        fixture.store.load(fixture.lib.content_hash()).unwrap(),
        Some(fixture.lib)
    );
}

#[test]
fn a_corrupt_cached_bundle_without_a_remote_refuses() {
    let fixture = fixture();
    let _ = resolve_with(&fixture, std::slice::from_ref(&fixture.lib));
    std::fs::write(
        fixture.store.path_of(fixture.lib.content_hash()),
        b"corrupt bytes",
    )
    .expect("the corruption lands");
    // a cache-only provider serves nothing for the drifted file: the
    // demand refuses through the resolution taxonomy
    let mut provider = StoreProvider::cached(&fixture.store);
    let error = resolve(&fixture.root, Path::new("."), |name, demand| match demand {
        Demand::Registry { ver, pin } => provider.fetch(name, ver, pin),
        Demand::Path { .. } => Fetched::Refused("the fixture serves no path".to_owned()),
    })
    .expect_err("the corrupted bundle refuses");
    assert_eq!(
        error.to_string(),
        "`app` demands `lib` at 1.0.0, which the provider does not serve"
    );
}

#[test]
fn the_cache_s_own_knowledge_sets_the_unserved_taxonomy() {
    let fixture = fixture();
    fixture
        .store
        .install(&fixture.lib)
        .expect("the lib installs");
    let mut provider = StoreProvider::cached(&fixture.store);
    // a name the cache has seen at another version is unoffered, its
    // highest carried for the diagnostic
    let high = manifest_of("app", "1.0.0", &[("lib", "2.0.0", Hash128::new([1; 16]))]);
    let error = resolve(&high, Path::new("."), |name, demand| match demand {
        Demand::Registry { ver, pin } => provider.fetch(name, ver, pin),
        Demand::Path { .. } => Fetched::Refused("the fixture serves no path".to_owned()),
    })
    .expect_err("the unoffered version refuses");
    assert_eq!(
        error.to_string(),
        "`app` demands `lib` at 2.0.0, above every offered version (1.0.0)"
    );
    // a name the cache has never seen is unknown
    let ghost = manifest_of("app", "1.0.0", &[("ghost", "1.0.0", Hash128::new([2; 16]))]);
    let error = resolve(&ghost, Path::new("."), |name, demand| match demand {
        Demand::Registry { ver, pin } => provider.fetch(name, ver, pin),
        Demand::Path { .. } => Fetched::Refused("the fixture serves no path".to_owned()),
    })
    .expect_err("the unknown name refuses");
    assert_eq!(
        error.to_string(),
        "`app` demands `ghost` at 1.0.0, which the provider does not serve"
    );
}

#[test]
fn the_remote_taxonomy_and_content_pass_through_the_seam() {
    let fixture = fixture();
    // the remote's own taxonomy wins when it answers
    let mut provider = StoreProvider::with_remote(&fixture.store, |name, _version| match name {
        "lib" => Remote::Unoffered {
            best: Some(Version::new(3, 0, 0)),
        },
        _ => Remote::UnknownName,
    });
    let root = manifest_of("app", "1.0.0", &[("lib", "2.0.0", Hash128::new([1; 16]))]);
    let error = resolve(&root, Path::new("."), |name, demand| match demand {
        Demand::Registry { ver, pin } => provider.fetch(name, ver, pin),
        Demand::Path { .. } => Fetched::Refused("the fixture serves no path".to_owned()),
    })
    .expect_err("the remote's unoffered version refuses");
    assert_eq!(
        error.to_string(),
        "`app` demands `lib` at 2.0.0, above every offered version (3.0.0)"
    );
    let ghost = manifest_of("app", "1.0.0", &[("ghost", "1.0.0", Hash128::new([2; 16]))]);
    let error = resolve(&ghost, Path::new("."), |name, demand| match demand {
        Demand::Registry { ver, pin } => provider.fetch(name, ver, pin),
        Demand::Path { .. } => Fetched::Refused("the fixture serves no path".to_owned()),
    })
    .expect_err("the remote's unknown name refuses");
    assert_eq!(
        error.to_string(),
        "`app` demands `ghost` at 1.0.0, which the provider does not serve"
    );
    // a remote serving content under another name hits the load walk's
    // same-miss rule through the landed resolution validation
    let other = package_of(&manifest_of("other", "1.0.0", &[]), b"fn f() {}");
    let mut provider =
        StoreProvider::with_remote(&fixture.store, |_, _| Remote::Found(other.clone()));
    let swap = manifest_of("app", "1.0.0", &[("lib", "1.0.0", other.content_hash())]);
    let error = resolve(&swap, Path::new("."), |name, demand| match demand {
        Demand::Registry { ver, pin } => provider.fetch(name, ver, pin),
        Demand::Path { .. } => Fetched::Refused("the fixture serves no path".to_owned()),
    })
    .expect_err("content under another name refuses");
    assert_eq!(
        error.to_string(),
        "`app` demands `lib` at 1.0.0, which the provider does not serve"
    );
}

/// A demand serves from its pin alone: a store holding many unrelated
/// installed bundles — one of them corrupt under its own identity —
/// resolves by reading exactly the pinned files, so the unrelated
/// population (corrupt or not) prices nothing and refuses nothing.
#[test]
fn a_demand_serves_from_its_pin_among_many_installed_bundles() {
    let fixture = fixture();
    let mut fillers = Vec::new();
    for index in 0..8 {
        let filler = package_of(
            &manifest_of(&format!("filler{index}"), "1.0.0", &[]),
            b"fn f() {}",
        );
        fixture.store.install(&filler).expect("the filler installs");
        fillers.push(filler);
    }
    fixture
        .store
        .install(&fixture.lib)
        .expect("the lib installs");
    // corrupt one unrelated row: bytes under the identity that hash
    // elsewhere serve nothing, and the demand never reads them anyway
    std::fs::write(
        fixture.store.path_of(fillers[3].content_hash()),
        b"corrupt bytes",
    )
    .expect("the corruption lands");
    let mut provider = StoreProvider::cached(&fixture.store);
    let resolved = resolve(&fixture.root, Path::new("."), |name, demand| match demand {
        Demand::Registry { ver, pin } => provider.fetch(name, ver, pin),
        Demand::Path { .. } => Fetched::Refused("the fixture serves no path".to_owned()),
    })
    .expect("the pinned demand resolves");
    assert_eq!(
        resolved.deps()["lib"].content_hash,
        fixture.lib.content_hash()
    );
    assert_eq!(
        resolved.deps()["lib"].member_identity,
        fixture.lib.member_identity().expect("the fixture folds")
    );
}

/// A provider holds no snapshot of the store: an install landing after
/// the provider's first demand — another process's `ds save`, another
/// provider's fetch — serves that provider's next demand, where a
/// snapshot built at the first demand would have missed it forever.
#[test]
fn an_install_after_the_first_demand_serves_the_same_provider() {
    let fixture = fixture();
    let mut provider = StoreProvider::cached(&fixture.store);
    let error = resolve(&fixture.root, Path::new("."), |name, demand| match demand {
        Demand::Registry { ver, pin } => provider.fetch(name, ver, pin),
        Demand::Path { .. } => Fetched::Refused("the fixture serves no path".to_owned()),
    })
    .expect_err("the empty store refuses");
    assert_eq!(
        error.to_string(),
        "`app` demands `lib` at 1.0.0, which the provider does not serve"
    );
    // the demanded content lands from outside this provider
    fixture
        .store
        .install(&fixture.lib)
        .expect("the lib installs");
    let resolved = resolve(&fixture.root, Path::new("."), |name, demand| match demand {
        Demand::Registry { ver, pin } => provider.fetch(name, ver, pin),
        Demand::Path { .. } => Fetched::Refused("the fixture serves no path".to_owned()),
    })
    .expect("the same provider serves the landed install");
    assert_eq!(
        resolved.deps()["lib"].content_hash,
        fixture.lib.content_hash()
    );
}

/// A `(name, version)` self-described at two identities serves the
/// demand's own pin: the pin directs the read, so registry drift under
/// one pair never picks between the identities — the pin validation
/// that still refuses is a pin whose own content disagrees.
#[test]
fn two_identities_under_one_pair_serve_the_demand_s_own_pin() {
    let fixture = fixture();
    let one = package_of(&manifest_of("lib", "1.0.0", &[]), b"fn one() { 1 }");
    let two = package_of(&manifest_of("lib", "1.0.0", &[]), b"fn two() { 2 }");
    fixture.store.install(&one).expect("the first installs");
    fixture.store.install(&two).expect("the second installs");
    let root = manifest_of("app", "1.0.0", &[("lib", "1.0.0", two.content_hash())]);
    let mut provider = StoreProvider::cached(&fixture.store);
    let resolved = resolve(&root, Path::new("."), |name, demand| match demand {
        Demand::Registry { ver, pin } => provider.fetch(name, ver, pin),
        Demand::Path { .. } => Fetched::Refused("the fixture serves no path".to_owned()),
    })
    .expect("the pinned identity serves");
    assert_eq!(resolved.deps()["lib"].content_hash, two.content_hash());
    assert_eq!(
        resolved.deps()["lib"].member_identity,
        two.member_identity().expect("the fixture folds")
    );
}

/// The lib module the fixture's source member describes: `inc(x) = x + 37`.
fn lib_module(source: &[u8]) -> Module {
    let inc = Function::new(
        "inc",
        FrameLayout::new(vec![I64, I64], 1, 0).expect("valid fixture layout"),
        vec![Constant::I64(37)],
        vec![
            Uop::Funcf { frame_slots: 2 },
            Uop::Kimm {
                dst: 1,
                kidx: ConstIdx(0),
            },
            Uop::AddI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            },
            Uop::Ret { src: 0 },
        ],
    )
    .expect("valid function");
    Module::new("lib", TargetClass::Bytecode, Hash128::compute(source)).with_function(inc)
}

/// The app module: `main` stages 5, calls the first dep entry's `inc`,
/// halts on the overlaid result.
fn app_module(source: &[u8], linked: Hash128) -> Module {
    let main = Function::new(
        "main",
        FrameLayout::new(vec![I64], 0, 1).expect("valid fixture layout"),
        vec![Constant::I64(5), Constant::FuncRef {
            key: FuncRefKey::Dep(1),
            func: 0,
        }],
        vec![
            Uop::Funcf { frame_slots: 1 },
            Uop::Kimm {
                dst: 0,
                kidx: ConstIdx(0),
            },
            Uop::Call {
                callee: ConstIdx(1),
                nargs: 1,
            },
            Uop::Halt { slot: 0 },
        ],
    )
    .expect("valid function");
    Module::new("app", TargetClass::Bytecode, Hash128::compute(source))
        .with_dep("lib", linked)
        .with_function(main)
}

#[test]
fn the_resolved_graph_composes_with_the_load_walk() {
    let fixture = fixture();
    let (resolved, remote) = resolve_with(&fixture, std::slice::from_ref(&fixture.lib));
    assert_eq!(remote.fetches.len(), 1);
    // the linked module identities derive from the resolved bundles'
    // source members: the load walk's link hash rule
    let lib_source = fixture.lib.members()["lib.ds"].clone();
    let lib_linked = Hash128::compute(&lib_source);
    let app_source = fixture.app.members()["app.ds"].clone();
    // the walk fetches each uninstalled link through a provider that
    // validates the demanded source hash against the resolved bundle's
    // installed bytes
    let mut vm = Vm::new(ModuleRegistry::new());
    let module = lib_module(&lib_source);
    vm.load(app_module(&app_source, lib_linked), |name, linked| {
        if name != "lib" {
            return None;
        }
        let dep = resolved.deps().get("lib")?;
        let package = fixture.store.load(dep.content_hash).ok()??;
        let source = package.members().get("lib.ds")?;
        (Hash128::compute(source) == linked).then(|| module.clone())
    })
    .expect("the resolved graph loads");
    // deps-first: lib installs before app
    assert_eq!(vm.registry().module_id("lib"), Some(0));
    assert_eq!(vm.registry().module_id("app"), Some(1));
    // and the call runs over the resolved, installed content
    assert_eq!(
        vm.run("app", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(42)))
    );
}
