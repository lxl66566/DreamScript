//! End-to-end package runs through the `ds` binary: `ds save` installs
//! fixture packages into the test store, and a root whose `ds.pkg` sits
//! beside it resolves over the store, compiles against the graph, and
//! installs through the deps-first walk — the script-mode dsmod cache
//! serving the root across runs under a key that folds the graph's dep
//! identities.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use ds_diag::FileId;
use ds_mod::{Dep, Hash128};
use ds_pkg::{Manifest, Package};

/// The deepest fixture package's member.
const DEEP_MEMBER: &str = "pub fn base() -> int {\n21\n}\n";

/// The middle fixture package's member: it imports the deepest package
/// and calls through, so the walk installs a two-deep chain and the
/// cross-module call lowers through the link position.
const LIB_MEMBER: &str = concat!(
    "import @deep (base)\n",
    "\n",
    "pub fn twice(n: int) -> int {\n",
    "base() + n\n",
    "}\n",
    "\n",
    "fn main() -> int {\n",
    "twice(21)\n",
    "}\n",
);

/// The fixture root's source: imports the ambient and the middle
/// package's helper member, calls the selected function, prints, and
/// returns.
const ROOT_SOURCE: &str = concat!(
    "import @std/io\n",
    "import @lib/helper (twice)\n",
    "\n",
    "fn main() -> int {\n",
    "println(\"hi\")\n",
    "twice(21)\n",
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

/// Runs the built binary against the shared test store and flattens the
/// outcome.
fn ds(args: &[&str]) -> (bool, String, String) {
    ds_env(&test_cache(), None, args)
}

/// Runs the built binary against one cache directory and an optional
/// registry index root ($DS_REGISTRY), flattening the outcome.
fn ds_env(cache: &Path, registry: Option<&Path>, args: &[&str]) -> (bool, String, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command.env("DS_CACHE", cache).env_remove("DS_REGISTRY");
    if let Some(registry) = registry {
        command.env("DS_REGISTRY", registry);
    }
    let output = command.args(args).output().expect("the ds binary runs");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Runs the binary with the debug observable on against one cache
/// directory: the `ds run cache:` and `ds dep cache:` lines on stderr
/// name the path every lookup took — the observable that proves a warm
/// run compiled nothing in-process, where byte-identical output alone
/// cannot (a fresh compile answers the same bytes).
fn ds_debug(cache: &Path, args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_ds"))
        .env("DS_CACHE", cache)
        .env("DS_RUN_CACHE_DEBUG", "1")
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

/// Every `.dsmod` below the cache root: the run-cache artifact count
/// (the store's bundles and the `.dsi` prefix tier answer other
/// extensions).
fn run_artifacts(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "dsmod") {
                found.push(path);
            }
        }
    }
    found
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
    write_pkg(&lib, &lib_manifest, &[("helper.ds", LIB_MEMBER)]);
    for dir in [&deep, &lib] {
        let (ok, out, err) = ds(&["save", dir.to_str().expect("temp path is UTF-8")]);
        assert!(ok, "ds save {} exits 0 (stderr: {err})", dir.display());
        assert!(
            out.starts_with("saved "),
            "the install reports the identity it pinned (stdout: {out})"
        );
    }
    let lib_id = bundle_identity(&lib_manifest, &[("helper.ds", LIB_MEMBER)]);
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
    assert_eq!(out, "hi\n42\n", "the call crosses two linked packages");

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

/// The dep-member cache over package runs: the cold run compiles the
/// linked members in-process and writes one artifact per member beside
/// the root's own; the warm run serves the root artifact and every
/// linked member from disk — the debug markers proving the walk never
/// compiled, byte-identity alone unable to — and prints the cold run's
/// output; a repinned dep (new content in the store as a version bump,
/// new manifest pins) re-keys both artifact families, so a stale dep
/// artifact never serves the new resolution while the old pins keep
/// serving their own.
#[test]
fn a_warm_package_run_serves_the_dep_members_from_the_cache() {
    let (work, lib_id) = fixtures("depcache");
    let root = write_root(&work, &root_manifest(lib_id, false), ROOT_SOURCE);
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let cache = work.join("cache");
    // the private cache root is the store too, so the fixtures install
    // into it ahead of the runs
    for dir in [work.join("deep"), work.join("lib")] {
        let (ok, out, err) = ds_env(&cache, None, &[
            "save",
            dir.to_str().expect("temp path is UTF-8"),
        ]);
        assert!(ok, "ds save {} exits 0 (stderr: {err})", dir.display());
        assert!(
            out.starts_with("saved "),
            "the install reports its identity: {out}"
        );
    }

    // the cold run: the root misses, both linked members miss and
    // compile in-process, and one artifact lands per compiled module
    let (ok, cold, err) = ds_debug(&cache, &["run", entry]);
    assert!(ok, "the cold package run succeeds (stderr: {err})");
    assert_eq!(cold, "hi\n42\n");
    assert!(err.contains("ds run cache: miss"), "the root misses: {err}");
    assert_eq!(
        err.matches("ds dep cache: miss").count(),
        2,
        "both linked members miss cold: {err}"
    );
    assert_eq!(
        run_artifacts(&cache).len(),
        3,
        "the root and both members land one artifact each"
    );

    // the warm run: the root serves and both members serve from disk —
    // the dep hit markers are the walk's own lookup decisions, so
    // nothing compiled in-process — and the output is byte-identical
    let (ok, warm, err) = ds_debug(&cache, &["run", entry]);
    assert!(ok, "the warm package run succeeds (stderr: {err})");
    assert_eq!(warm, cold, "the warm run prints the cold output");
    assert!(err.contains("ds run cache: hit"), "the root serves: {err}");
    assert_eq!(
        err.matches("ds dep cache: hit").count(),
        2,
        "both linked members serve from disk: {err}"
    );
    assert_eq!(
        err.matches("ds dep cache: miss").count(),
        0,
        "no member recompiles on the warm run: {err}"
    );
    assert_eq!(
        run_artifacts(&cache).len(),
        3,
        "the warm run writes nothing"
    );

    // the repin: new dep content ships as a version bump (the store
    // serves the lowest identity for a pair), so both graph identities
    // move and both artifact families re-key — the run over the new
    // pins answers the new content, never the stale member artifact
    let deep_two = DEEP_MEMBER.replace("21", "22");
    let deep_two_manifest = "name = \"@deep\"\nversion = \"1.1.0\"\n".to_owned();
    let deep_two_dir = work.join("deep-two");
    write_pkg(&deep_two_dir, &deep_two_manifest, &[("deep.ds", &deep_two)]);
    let lib_two_manifest = format!(
        "name = \"@lib\"\nversion = \"1.3.0\"\n\n[deps]\n@deep = {{ ver = \"1.1.0\", hash = \
         \"blake3:{}\" }}\n",
        bundle_identity(&deep_two_manifest, &[("deep.ds", &deep_two)])
    );
    let lib_two_dir = work.join("lib-two");
    write_pkg(&lib_two_dir, &lib_two_manifest, &[(
        "helper.ds",
        LIB_MEMBER,
    )]);
    for dir in [&deep_two_dir, &lib_two_dir] {
        let (ok, out, err) = ds_env(&cache, None, &[
            "save",
            dir.to_str().expect("temp path is UTF-8"),
        ]);
        assert!(ok, "ds save {} exits 0 (stderr: {err})", dir.display());
        assert!(
            out.starts_with("saved "),
            "the install reports its identity: {out}"
        );
    }
    let lib_two_id = bundle_identity(&lib_two_manifest, &[("helper.ds", LIB_MEMBER)]);
    let root_two_manifest = format!(
        "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@lib = {{ ver = \"1.3.0\", hash = \
         \"blake3:{lib_two_id}\" }}"
    );
    fs::write(root.join("ds.pkg"), &root_two_manifest).expect("the repin writes");

    let (ok, repinned, err) = ds_debug(&cache, &["run", entry]);
    assert!(ok, "the repinned run succeeds (stderr: {err})");
    assert_eq!(
        repinned, "hi\n43\n",
        "the new dep content serves, never the stale"
    );
    assert!(
        err.contains("ds run cache: miss"),
        "the moved graph re-keys the root: {err}"
    );
    assert_eq!(
        err.matches("ds dep cache: miss").count(),
        2,
        "the moved identities re-key both member artifacts: {err}"
    );
    assert_eq!(
        run_artifacts(&cache).len(),
        6,
        "the repinned resolution lands its own three artifacts"
    );

    let (ok, warm_two, err) = ds_debug(&cache, &["run", entry]);
    assert!(ok, "the repinned warm run succeeds (stderr: {err})");
    assert_eq!(
        warm_two, repinned,
        "the repinned warm run prints its cold output"
    );
    assert!(
        err.contains("ds run cache: hit"),
        "the repinned root serves: {err}"
    );
    assert_eq!(
        err.matches("ds dep cache: hit").count(),
        2,
        "the repinned members serve warm: {err}"
    );

    // the old pins keep their own artifacts: the original resolution
    // still serves warm over its content
    fs::write(root.join("ds.pkg"), root_manifest(lib_id, false)).expect("the revert writes");
    let (ok, reverted, err) = ds_debug(&cache, &["run", entry]);
    assert!(ok, "the reverted run succeeds (stderr: {err})");
    assert_eq!(
        reverted, cold,
        "the original pins still serve their content"
    );
    assert!(
        err.contains("ds run cache: hit"),
        "the original artifacts remain: {err}"
    );
    assert_eq!(
        err.matches("ds dep cache: hit").count(),
        2,
        "the original member artifacts remain: {err}"
    );
    assert_eq!(run_artifacts(&cache).len(), 6, "nothing rewrites");

    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// The missing dep no provider serves: without a registry the refusal
/// names `ds save` and $DS_REGISTRY; with a registry that answered, the
/// taxonomy speaks alone.
#[test]
fn a_missing_dep_refuses_naming_the_fill() {
    let (work, lib_id) = fixtures("missing");
    let root = write_root(&work, &root_manifest(lib_id, true), ROOT_SOURCE);
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");

    let (ok, out, err) = ds(&["run", entry]);
    assert!(!ok, "a package run with a missing dep exits nonzero");
    assert_eq!(out, "");
    assert!(err.contains("`@ghost`"), "the refusal names the dep: {err}");
    assert!(
        err.contains("set $DS_REGISTRY to a registry index root"),
        "the refusal names the fill: {err}"
    );

    // a registry that answered carries its own taxonomy: the unknown
    // name refuses without the local-fill hint
    let cache = work.join("cache");
    let registry = write_registry(&work, lib_id);
    let (ok, out, err) = ds_env(&cache, Some(&registry), &["run", entry]);
    assert!(!ok, "a registry that never heard the name exits nonzero");
    assert_eq!(out, "");
    assert!(
        err.contains("`@ghost` at 0.9.0, which the provider does not serve"),
        "the registry's taxonomy refuses: {err}"
    );
    assert!(!err.contains("DS_REGISTRY"), "no local-fill hint: {err}");

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

/// Writes one registry index root below `work`: a row file per name under
/// `index/`, the bundles content-addressed under `pkg/`. The rows offer
/// the saved fixture pair at the identities the manifests pin.
fn write_registry(work: &Path, lib_id: Hash128) -> PathBuf {
    let deep_manifest = deep_manifest();
    let deep_id = bundle_identity(&deep_manifest, &[("deep.ds", DEEP_MEMBER)]);
    let lib_manifest = lib_manifest(deep_id);
    let root = work.join("registry");
    for (name, version, hash, manifest, sources) in [
        (
            "@deep",
            "1.0.0",
            deep_id,
            deep_manifest.as_str(),
            &[("deep.ds", DEEP_MEMBER)] as &[(&str, &str)],
        ),
        ("@lib", "1.2.0", lib_id, lib_manifest.as_str(), &[(
            "helper.ds",
            LIB_MEMBER,
        )]),
    ] {
        let row = root.join("index").join(name);
        fs::create_dir_all(row.parent().expect("the row file has a parent"))
            .expect("the index segment creates");
        fs::write(&row, format!("{version} blake3:{hash}\n")).expect("the row writes");
        let bundle = bundle_bytes(manifest, sources);
        fs::create_dir_all(root.join("pkg")).expect("the index storage creates");
        fs::write(root.join("pkg").join(format!("{hash}.pkg")), bundle).expect("the bundle writes");
    }
    root
}

/// The canonical bundle bytes a package's identity names.
fn bundle_bytes(manifest: &str, sources: &[(&str, &str)]) -> Vec<u8> {
    let manifest = Manifest::parse(manifest, FileId::new(0)).expect("fixture manifest parses");
    Package::new(
        &manifest,
        sources
            .iter()
            .map(|(path, text)| ((*path).to_owned(), text.as_bytes().to_vec())),
    )
    .expect("fixture bundle builds")
    .to_bytes()
}

/// A cold store plus a registry index: the misses fetch from the index's
/// rows, verify against them, install through the store's atomic path,
/// and the package run compiles and runs over the fetched graph.
#[test]
fn a_registry_index_fills_a_cold_store_run() {
    let work = temp_dir("registry");
    let deep_manifest = deep_manifest();
    let deep_id = bundle_identity(&deep_manifest, &[("deep.ds", DEEP_MEMBER)]);
    let lib_manifest = lib_manifest(deep_id);
    let lib_id = bundle_identity(&lib_manifest, &[("helper.ds", LIB_MEMBER)]);
    let registry = write_registry(&work, lib_id);
    let root = write_root(&work, &root_manifest(lib_id, false), ROOT_SOURCE);
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");

    let cache = work.join("cache");
    let (ok, out, err) = ds_env(&cache, Some(&registry), &["run", entry]);
    assert!(ok, "the registry-filled run exits 0 (stderr: {err})");
    assert_eq!(out, "hi\n42\n");
    // the fetched, verified bundles installed through the store
    for id in [deep_id, lib_id] {
        assert!(
            cache.join("pkg").join(format!("{id}.pkg")).is_file(),
            "the verified bundle installs: {id}"
        );
    }

    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A registry row whose fetched bytes hash to another identity refuses
/// hard naming both hashes: the index cannot serve content its own rows
/// do not verify.
#[test]
fn a_registry_hash_mismatch_refuses_naming_both_hashes() {
    let work = temp_dir("registry-mismatch");
    let deep_manifest = deep_manifest();
    let deep_id = bundle_identity(&deep_manifest, &[("deep.ds", DEEP_MEMBER)]);
    let lib_manifest = lib_manifest(deep_id);
    let lib_id = bundle_identity(&lib_manifest, &[("helper.ds", LIB_MEMBER)]);
    // the @lib row pins the deep identity; the storage file named by it
    // holds the lib bundle's bytes, which hash to another identity
    let registry = work.join("registry");
    let row = registry.join("index").join("@lib");
    fs::create_dir_all(row.parent().expect("the row file has a parent"))
        .expect("the index segment creates");
    fs::write(&row, format!("1.2.0 blake3:{deep_id}\n")).expect("the row writes");
    fs::create_dir_all(registry.join("pkg")).expect("the index storage creates");
    fs::write(
        registry.join("pkg").join(format!("{deep_id}.pkg")),
        bundle_bytes(&lib_manifest, &[("helper.ds", LIB_MEMBER)]),
    )
    .expect("the drifted bundle writes");

    let root = write_root(&work, &root_manifest(lib_id, false), ROOT_SOURCE);
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let cache = work.join("cache");
    let (ok, out, err) = ds_env(&cache, Some(&registry), &["run", entry]);
    assert!(!ok, "the mismatched fetch exits nonzero");
    assert_eq!(out, "");
    assert!(
        err.contains(&format!("blake3:{deep_id}")),
        "the refusal names the row's pin: {err}"
    );
    assert!(
        err.contains(&format!("blake3:{lib_id}")),
        "the refusal names the fetched bytes' own hash: {err}"
    );
    // nothing installed: unverified content never reaches the store
    assert!(
        !cache.join("pkg").join(format!("{lib_id}.pkg")).exists(),
        "unverified content never installs"
    );

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
        err.contains("`@other` names no member of the linked package"),
        "the refusal names the link: {err}"
    );
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A package whose bundle carries more than one source member composes
/// one module under its link name (G3): each member file joins one
/// compile unit, an import's member segment selects the member file's
/// grain, and the root calls functions from two members through one
/// link. The built artifact pins the package at the composed identity —
/// the ascending-path fold, not any single member's hash.
#[test]
fn a_multi_member_package_composes_one_module() {
    let (work, _lib_id) = fixtures("multi");
    let wide = work.join("wide");
    let manifest = "name = \"@wide\"\nversion = \"1.0.0\"\n".to_owned();
    let a_member = "pub fn a() -> int {\n1\n}\n";
    let b_member = "pub fn triple(n: int) -> int {\n3 * n\n}\n";
    let members = [("a.ds", a_member), ("b.ds", b_member)];
    write_pkg(&wide, &manifest, &members);
    let (ok, _, err) = ds(&["save", wide.to_str().expect("temp path is UTF-8")]);
    assert!(ok, "the multi-member bundle installs (stderr: {err})");
    let wide_id = bundle_identity(&manifest, &members);
    let root_manifest = format!(
        "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@wide = {{ ver = \"1.0.0\", hash = \
         \"blake3:{wide_id}\" }}\n"
    );
    let root = write_root(
        &work,
        &root_manifest,
        concat!(
            "import @wide/a (a)\n",
            "import @wide/b (triple)\n",
            "\n",
            "fn main() -> int {\n",
            "a() + triple(4)\n",
            "}\n",
        ),
    );
    // the composed identity the writer records: the ascending-path fold
    // over the members, not any single member's text hash
    let folded = {
        let mut buffer = Vec::new();
        for (path, text) in members {
            buffer.extend_from_slice(&(path.len() as u64).to_le_bytes());
            buffer.extend_from_slice(path.as_bytes());
            buffer.extend_from_slice(&(text.len() as u64).to_le_bytes());
            buffer.extend_from_slice(text.as_bytes());
        }
        Hash128::compute(&buffer)
    };
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(ok, "a multi-member package run exits 0 (stderr: {err})");
    assert_eq!(out, "13\n");

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
        &[Dep {
            name: "@wide".to_owned(),
            content_hash: folded,
        }],
        "the writer records the composed member identity"
    );
    assert_ne!(
        folded,
        Hash128::compute(a_member.as_bytes()),
        "the fold is not a single member's hash"
    );
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A whole-package import and a module alias bind (G4): `import @wide`
/// selects from every member's exports, and `as` binds a reference the
/// dotted call sites resolve through the linked member surface.
#[test]
fn a_whole_package_import_and_an_alias_bind() {
    let (work, _lib_id) = fixtures("alias");
    let wide = work.join("wide");
    let manifest = "name = \"@wide\"\nversion = \"1.0.0\"\n".to_owned();
    let a_member = "pub fn a() -> int {\n1\n}\n";
    let b_member = "pub fn triple(n: int) -> int {\n3 * n\n}\n";
    let members = [("a.ds", a_member), ("b.ds", b_member)];
    write_pkg(&wide, &manifest, &members);
    let (ok, _, err) = ds(&["save", wide.to_str().expect("temp path is UTF-8")]);
    assert!(ok, "the bundle installs (stderr: {err})");
    let wide_id = bundle_identity(&manifest, &members);
    let root_manifest = format!(
        "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@wide = {{ ver = \"1.0.0\", hash = \
         \"blake3:{wide_id}\" }}\n"
    );
    let root = write_root(
        &work,
        &root_manifest,
        concat!(
            "import @wide as w\n",
            "\n",
            "fn main() -> int {\n",
            "w.a() + w.triple(5)\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(ok, "the aliased run exits 0 (stderr: {err})");
    assert_eq!(out, "16\n");
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// The import selection tiers refuse at the use they fail (G4): a
/// private name, an unexported name, a generic function, a duplicate
/// binding, and a member segment naming no member file each exit
/// nonzero with their own diagnostic wording.
#[test]
fn the_import_tiers_refuse() {
    let (work, _lib_id) = fixtures("tiers");
    // a name the sibling tests never install: the store row is shared
    // per run, so two fixtures of one name race their saves against
    // each other's resolutions
    let pkg = work.join("tiers");
    let manifest = "name = \"@tiers\"\nversion = \"1.0.0\"\n".to_owned();
    let member = concat!(
        "pub fn a() -> int {\n",
        "1\n",
        "}\n",
        "\n",
        "fn hidden() -> int {\n",
        "2\n",
        "}\n",
        "\n",
        "pub fn gen[t: show](v: t) -> int {\n",
        "3\n",
        "}\n",
    );
    let members = [("a.ds", member)];
    write_pkg(&pkg, &manifest, &members);
    let (ok, _, err) = ds(&["save", pkg.to_str().expect("temp path is UTF-8")]);
    assert!(ok, "the bundle installs (stderr: {err})");
    let pkg_id = bundle_identity(&manifest, &members);
    let root_manifest = format!(
        "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@tiers = {{ ver = \"1.0.0\", hash = \
         \"blake3:{pkg_id}\" }}\n"
    );
    for (tag, source, refused) in [
        (
            "private",
            "import @tiers/a (hidden)\n\nfn main() -> int {\n7\n}\n",
            "is `pub(pkg)` or private in its package",
        ),
        (
            "missing",
            "import @tiers/a (nope)\n\nfn main() -> int {\n7\n}\n",
            "names no definition the imported member exports",
        ),
        (
            "generic",
            "import @tiers/a (gen)\n\nfn main() -> int {\n7\n}\n",
            "cannot bind across packages this round",
        ),
        (
            "duplicate",
            "import @tiers/a (a)\nimport @tiers/a (a)\n\nfn main() -> int {\n7\n}\n",
            "two imports bind the name `a` into this file",
        ),
        (
            "member",
            "import @tiers/nope (a)\n\nfn main() -> int {\n7\n}\n",
            "names no member of the linked package",
        ),
    ] {
        let root = write_root(&work, &root_manifest, source);
        let entry = root.join("app.ds");
        let entry = entry.to_str().expect("temp path is UTF-8");
        let (ok, out, err) = ds(&["run", entry]);
        assert!(!ok, "the {tag} selection refuses");
        assert_eq!(out, "");
        assert!(
            err.contains(refused),
            "the {tag} refusal names its tier (stderr: {err})"
        );
        fs::remove_dir_all(&root).expect("the temp tree removes");
    }
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// The fixture package whose surface carries every literal fact shape
/// beside the refused ones, installed once for the const tests.
fn facts_fixtures(tag: &str) -> (PathBuf, String) {
    let (work, _lib_id) = fixtures(tag);
    let pkg = work.join("facts");
    let manifest = "name = \"@facts\"
version = \"1.0.0\"
"
    .to_owned();
    let member = concat!(
        "pub const limit = 100
",
        "pub const step = -3
",
        "pub const greeting = \"hi\"
",
        "pub const ratio = 0.5
",
        "pub const ready = true
",
        "pub const initial = 'd'
",
        "pub const code = b'x'
",
        "const tucked = 5
",
        "pub const computed = 6 * 7
",
        "pub var counter = 0
",
        "pub tag point
",
        "pub fn bump(n: int) -> int {
",
        "  n + 1
",
        "}
",
    );
    let members = [("facts.ds", member)];
    write_pkg(&pkg, &manifest, &members);
    let (ok, _, err) = ds(&["save", pkg.to_str().expect("temp path is UTF-8")]);
    assert!(ok, "the facts fixture installs (stderr: {err})");
    let pkg_id = bundle_identity(&manifest, &members).to_string();
    (work, pkg_id)
}

/// A linked package's plain-literal consts bind and run (G4): the value
/// is the per-target link fact, so every root use embeds the literal —
/// the same word each spelling lowers to in the owning module — over
/// every scalar shape (int with a negation, string, float, bool, char,
/// byte).
#[test]
fn linked_const_facts_bind_and_run() {
    let (work, pkg_id) = facts_fixtures("consts");
    let root_manifest = format!(
        "name = \"app\"
version = \"0.1.0\"

[deps]
@facts = {{ ver = \"1.0.0\", hash =          \"blake3:{pkg_id}\" }}
"
    );
    let root = write_root(
        &work,
        &root_manifest,
        concat!(
            "import @std/io
",
            "import @facts (limit, step, greeting, ratio, ready, initial, code)
",
            "
",
            "fn main() -> int {
",
            "  println(greeting)
",
            "  println(join([\"a\", \"b\"], initial))
",
            "  println(\"ratio = ${ratio}\")
",
            "  limit + step + (if ready { 1 } else { 0 }) + len(bytes_of([code, code]))
",
            "}
",
        ),
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(ok, "the const-facts run exits 0 (stderr: {err})");
    assert_eq!(
        out,
        "hi
adb
ratio = 0.5
100
"
    );
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A linked package's alias-qualified const reads in value position
/// (G4): the qualified spelling binds through the same literal fact the
/// selection embeds — over every scalar shape and the nested contexts
/// (an argument, a list element, an interpolation hole, an `if`
/// condition) — so the run answers exactly what the selection's does.
#[test]
fn linked_const_alias_value_reads_bind_and_run() {
    let (work, pkg_id) = facts_fixtures("alias-value");
    let root_manifest = format!(
        "name = \"app\"
version = \"0.1.0\"

[deps]
@facts = {{ ver = \"1.0.0\", hash = \"blake3:{pkg_id}\" }}
"
    );
    let root = write_root(
        &work,
        &root_manifest,
        concat!(
            "import @std/io
",
            "import @facts as w
",
            "
",
            "fn main() -> int {
",
            "  println(w.greeting)
",
            "  println(join([\"a\", \"b\"], w.initial))
",
            "  println(\"ratio = ${w.ratio}\")
",
            "  w.limit + w.step + (if w.ready { 1 } else { 0 }) + len(bytes_of([w.code, w.code]))
",
            "}
",
        ),
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(ok, "the alias-qualified const run exits 0 (stderr: {err})");
    assert_eq!(
        out,
        "hi
adb
ratio = 0.5
100
"
    );
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// The const shapes with no literal fact and the type export keep their
/// refusals at the same walk (G4): a computed const, a mutable binding,
/// a type-namespace export, and the alias-qualified spellings — the
/// const's call form, a computed const's value read, a private const, a
/// missing name, a function value, and a type name in value position.
#[test]
fn the_non_literal_const_tiers_refuse() {
    let (work, pkg_id) = facts_fixtures("tiers2");
    let root_manifest = format!(
        "name = \"app\"
version = \"0.1.0\"

[deps]
@facts = {{ ver = \"1.0.0\", hash =          \"blake3:{pkg_id}\" }}
"
    );
    for (tag, source, refused) in [
        (
            "computed",
            concat!(
                "import @facts (computed)
",
                "
",
                "fn main() -> int {
",
                "7
",
                "}
",
            ),
            "its value is no literal fact",
        ),
        (
            "mutable",
            concat!(
                "import @facts (counter)
",
                "
",
                "fn main() -> int {
",
                "7
",
                "}
",
            ),
            "its value is no literal fact",
        ),
        (
            "type",
            concat!(
                "import @facts (point)
",
                "
",
                "fn main() -> int {
",
                "7
",
                "}
",
            ),
            "layout and tag rows are the artifact round",
        ),
        (
            "alias-const",
            concat!(
                "import @facts as w
",
                "
",
                "fn main() -> int {
",
                "  w.limit()
",
                "}
",
            ),
            "serves callables at call sites",
        ),
        (
            "alias-computed",
            concat!(
                "import @facts as w
",
                "
",
                "fn main() -> int {
",
                "  w.computed
",
                "}
",
            ),
            "its value is no literal fact",
        ),
        (
            "alias-private",
            concat!(
                "import @facts as w
",
                "
",
                "fn main() -> int {
",
                "  w.tucked
",
                "}
",
            ),
            "is `pub(pkg)` or private in its package",
        ),
        (
            "alias-missing",
            concat!(
                "import @facts as w
",
                "
",
                "fn main() -> int {
",
                "  w.nope
",
                "}
",
            ),
            "names no definition the module `w` exports",
        ),
        (
            "alias-fn",
            concat!(
                "import @facts as w
",
                "
",
                "fn main() -> int {
",
                "  w.bump
",
                "}
",
            ),
            "serves through the alias at its call sites alone",
        ),
        (
            "alias-type",
            concat!(
                "import @facts as w
",
                "
",
                "fn main() -> int {
",
                "  w.point
",
                "}
",
            ),
            "a linked type is a type-namespace name",
        ),
    ] {
        let root = write_root(&work, &root_manifest, source);
        let entry = root.join("app.ds");
        let entry = entry.to_str().expect("temp path is UTF-8");
        let (ok, out, err) = ds(&["run", entry]);
        assert!(!ok, "the {tag} selection refuses");
        assert_eq!(out, "");
        assert!(
            err.contains(refused),
            "the {tag} refusal names its tier (stderr: {err})"
        );
        fs::remove_dir_all(&root).expect("the temp tree removes");
    }
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// The fixture package whose surface carries a fact-carrying struct over
/// every word kind beside the refused type shapes, installed once for
/// the type-fact tests.
fn shapes_fixtures(tag: &str) -> (PathBuf, String) {
    let work = temp_dir(tag);
    let pkg = work.join("shapes");
    let manifest = "name = \"@shapes\"
version = \"1.0.0\"
"
    .to_owned();
    let member = concat!(
        "pub struct cell {\n",
        "  n: int,\n",
        "  label: string,\n",
        "  tags: list[string],\n",
        "  ratio: float,\n",
        "  on: bool,\n",
        "  initial: char,\n",
        "  code: byte,\n",
        "}\n",
        "pub struct pair {\n",
        "  a: int,\n",
        "  b: int,\n",
        "}\n",
        "pub fn make(n: int) -> cell {\n",
        "  cell { n: n, label: \"made\", tags: [\"a\", \"b\"], ratio: 0.5, on: true, initial: \
         'm', code: b'm' }\n",
        "}\n",
        "pub fn area(c: cell) -> int {\n",
        "  c.n * 2\n",
        "}\n",
        "pub fn opt(n: int) -> cell? {\n",
        "  if n > 0 {\n",
        "    cell { n: n, label: \"o\", tags: [], ratio: 0.5, on: true, initial: 'o', code: b'o' \
         }\n",
        "  } else {\n",
        "    none\n",
        "  }\n",
        "}\n",
        "pub fn sum(p: pair) -> int {\n",
        "  p.a + p.b\n",
        "}\n",
        "pub enum shade {\n",
        "  red,\n",
        "  green,\n",
        "  blue { n: int, label: string },\n",
        "}\n",
        "pub fn pick(n: int) -> shade {\n",
        "  if n < 0 {\n",
        "    red\n",
        "  } else {\n",
        "    if n == 0 {\n",
        "      green\n",
        "    } else {\n",
        "      blue(n, \"b\")\n",
        "    }\n",
        "  }\n",
        "}\n",
        "pub fn score(s: shade) -> int {\n",
        "  match s {\n",
        "    red => 0,\n",
        "    green => 1,\n",
        "    blue(n, _) => n,\n",
        "  }\n",
        "}\n",
        "pub enum trip {\n",
        "  one(int, int),\n",
        "}\n",
        "pub enum hollow {\n",
        "}\n",
        "pub enum mixed[t] {\n",
        "  one { v: t },\n",
        "}\n",
        "pub struct box[t] {\n",
        "  v: t,\n",
        "}\n",
        "pub struct guard: ~Copyable {\n",
        "  n: int,\n",
        "}\n",
        "pub struct nested {\n",
        "  c: cell,\n",
        "}\n",
    );
    let members = [("shapes.ds", member)];
    write_pkg(&pkg, &manifest, &members);
    let (ok, _, err) = ds(&["save", pkg.to_str().expect("temp path is UTF-8")]);
    assert!(ok, "the shapes fixture installs (stderr: {err})");
    let pkg_id = bundle_identity(&manifest, &members).to_string();
    (work, pkg_id)
}

/// The root manifest pinning the shapes fixture.
fn shapes_root_manifest(pkg_id: &str) -> String {
    format!(
        "name = \"app\"
version = \"0.1.0\"

[deps]
@shapes = {{ ver = \"1.0.0\", hash =          \"blake3:{pkg_id}\" }}
"
    )
}

/// A linked flat struct binds through its structural fact (G4): the
/// word layout is the per-target fact, so the link walk registers a
/// struct def the file's type positions reach, the consumer constructs,
/// reads, and narrows over it exactly like a local declaration, and a
/// linked fn's signature mentioning the type checks against the minted
/// shape — values crossing both ways over the same words.
#[test]
fn linked_struct_facts_bind_and_run() {
    let (work, pkg_id) = shapes_fixtures("shapes");
    let root_manifest = shapes_root_manifest(&pkg_id);
    let root = write_root(
        &work,
        &root_manifest,
        concat!(
            "import @std/io\n",
            "import @shapes (cell, make, area)\n",
            "\n",
            "fn widen(n: int) -> cell? {\n",
            "  if n > 0 {\n",
            "    cell { n: n, label: \"wide\", tags: [], ratio: 1.5, on: false, initial: 'w', \
             code: b'w' }\n",
            "  } else {\n",
            "    none\n",
            "  }\n",
            "}\n",
            "\n",
            "fn main() -> int {\n",
            "  const c = cell { n: 3, label: \"x\", tags: [\"a\"], ratio: 0.5, on: true, initial: \
             'd', code: b'x' }\n",
            "  const made = make(10)\n",
            "  var total = c.n + area(c) + made.n + len(made.tags) + area(make(4))\n",
            // the linked struct's `tags: list[string]` field crosses the
            // packages: the structural `==` chain compares it through the
            // list content compare, the linked-type facts minting exactly
            // this collection field
            "  const twin = cell { n: 3, label: \"x\", tags: [\"a\"], ratio: 0.5, on: true, \
             initial: 'd', code: b'x' }\n",
            "  const other = cell { n: 3, label: \"x\", tags: [\"b\"], ratio: 0.5, on: true, \
             initial: 'd', code: b'x' }\n",
            "  if c == twin { total = total + 1 }\n",
            "  if c == other { println(\"bad\") } else { total = total + 1 }\n",
            "  if made == make(10) { total = total + 1 }\n",
            "  const maybe = widen(2)\n",
            "  if maybe is cell {\n",
            "    total = total + maybe.n\n",
            "  }\n",
            "  const back = match widen(-1) {\n",
            "    none => 0,\n",
            "    other => other.n,\n",
            "  }\n",
            "  println(\"${total + back}\")\n",
            "  total + back\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(ok, "the struct-facts run exits 0 (stderr: {err})");
    // 3 + 6 + 10 + 2 + 8 = 29 through the linked and local spellings, the
    // narrowed arm adding widen(2).n = 2, the match arm adding 0, and the
    // linked `==` chain adding 3 (the equal twin, the differing tags
    // answered unequal, the anonymous make(10) operand); the run echoes
    // the entry's return after the println
    assert_eq!(out, "34\n34\n");
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// The fixture package whose surface carries the bytes-word shapes
/// beside the still-refused field shapes, installed once for the
/// bytes-field tests.
fn blobs_fixtures(tag: &str) -> (PathBuf, String) {
    let work = temp_dir(tag);
    let pkg = work.join("blobs");
    let manifest = "name = \"@blobs\"\nversion = \"1.0.0\"\n".to_owned();
    let member = concat!(
        "pub struct blob {\n",
        "  n: int,\n",
        "  data: bytes,\n",
        "}\n",
        "pub struct maybe {\n",
        "  n: int?,\n",
        "}\n",
        "pub struct pairfield {\n",
        "  p: (int, int),\n",
        "}\n",
        "pub enum tone {\n",
        "  flat,\n",
        "  loaded { n: int, data: bytes },\n",
        "}\n",
        "pub fn mk(n: int) -> blob {\n",
        "  blob { n: n, data: utf8_encode(\"hi\") }\n",
        "}\n",
        "pub fn wrap(b: blob, label: string) -> blob? {\n",
        "  if b.n > 0 {\n",
        "    blob { n: b.n, data: utf8_encode(label) }\n",
        "  } else {\n",
        "    none\n",
        "  }\n",
        "}\n",
        "pub fn sum(b: blob) -> int {\n",
        "  b.n + len(b.data)\n",
        "}\n",
        "pub fn pick(n: int) -> tone {\n",
        "  if n > 0 {\n",
        "    loaded(n, utf8_encode(\"picked\"))\n",
        "  } else {\n",
        "    flat\n",
        "  }\n",
        "}\n",
        "pub fn score(t: tone) -> int {\n",
        "  match t {\n",
        "    flat => 0,\n",
        "    loaded(n, _) => n,\n",
        "  }\n",
        "}\n",
    );
    let members = [("blobs.ds", member)];
    write_pkg(&pkg, &manifest, &members);
    let (ok, _, err) = ds(&["save", pkg.to_str().expect("temp path is UTF-8")]);
    assert!(ok, "the blobs fixture installs (stderr: {err})");
    let pkg_id = bundle_identity(&manifest, &members).to_string();
    (work, pkg_id)
}

/// The root manifest pinning the blobs fixture.
fn blobs_root_manifest(pkg_id: &str) -> String {
    format!(
        "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@blobs = {{ ver = \"1.0.0\", hash = \
         \"blake3:{pkg_id}\" }}\n"
    )
}

/// A linked struct with a `bytes` field binds through its word-layout
/// fact (G4): the field is one Ref word like a string field, so it joins
/// the linked word vocabulary and the consumer constructs it through the
/// utf8 bridge, matches, `is`-narrows, and structurally `==`-compares
/// across the boundary, the value flowing into the ambient bytes natives
/// (`len`, the checked `byte_at` read, the `show` dispatch) where the
/// type checks — and a linked enum's payload carries the same bytes
/// word beside its scalar fields.
#[test]
fn linked_bytes_field_facts_bind_and_run() {
    let (work, pkg_id) = blobs_fixtures("blobs");
    let root_manifest = blobs_root_manifest(&pkg_id);
    let root = write_root(
        &work,
        &root_manifest,
        concat!(
            "import @std/io\n",
            "import @blobs (blob, mk, wrap, sum, tone, flat, loaded, pick, score)\n",
            "\n",
            "fn main() -> int {\n",
            "  const b = blob { n: 3, data: utf8_encode(\"hi\") }\n",
            "  const made = mk(10)\n",
            "  var total = sum(b) + sum(made)\n",
            "  const twin = blob { n: 3, data: utf8_encode(\"hi\") }\n",
            "  const other = blob { n: 3, data: utf8_encode(\"no\") }\n",
            "  if b == twin { total = total + 1 }\n",
            "  if b == other { println(\"bad\") } else { total = total + 1 }\n",
            "  const maybe = wrap(made, \"made\")\n",
            "  if maybe is blob {\n",
            "    total = total + maybe.n\n",
            "  }\n",
            "  const back = match wrap(b, \"x\") {\n",
            "    none => 0,\n",
            "    other => other.n,\n",
            "  }\n",
            "  total = total + back + score(pick(2))\n",
            "  const localmatch = match pick(1) {\n",
            "    flat => 1,\n",
            "    loaded(n, _) => n,\n",
            "  }\n",
            "  total = total + localmatch\n",
            "  var w = 0\n",
            "  const probe = pick(4)\n",
            "  if probe is loaded {\n",
            "    w = len(probe.data)\n",
            "  }\n",
            "  total = total + w\n",
            "  const head = match byte_at(b.data, 0) {\n",
            "    none => 0,\n",
            "    code => int_of_byte(code),\n",
            "  }\n",
            "  total = total + head\n",
            "  println(\"${b.data}\")\n",
            "  total\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(ok, "the bytes-field run exits 0 (stderr: {err})");
    // sum(b) = 3+2 and sum(made) = 10+2 open at 17, the == chain adds 2
    // (the equal twin, the differing content answered unequal), the
    // is-narrow adds 10, the match arm adds 3, the enum round adds 2
    // (score) + 1 (local match) + 6 (the narrowed payload's len), the
    // checked byte_at read adds 104 ('h'), and the show dispatch prints
    // the payload; the run echoes the entry's return after the println
    assert_eq!(out, "bytes[104, 105]\n145\n");
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// The field shapes the bytes widening leaves outside the vocabulary
/// keep their refusals at the same walk (G4): an optional field and a
/// tuple field carry no fact, so their selections still refuse — the
/// same DS4021 the nested struct answers.
#[test]
fn the_optional_and_tuple_field_shapes_still_refuse() {
    let (work, pkg_id) = blobs_fixtures("blobs-tiers");
    let root_manifest = blobs_root_manifest(&pkg_id);
    for (tag, source, refused) in [
        (
            "optional-field",
            concat!(
                "import @blobs (maybe)\n",
                "\n",
                "fn main() -> int {\n",
                "7\n",
                "}\n",
            ),
            "layout and tag rows are the artifact round",
        ),
        (
            "tuple-field",
            concat!(
                "import @blobs (pairfield)\n",
                "\n",
                "fn main() -> int {\n",
                "7\n",
                "}\n",
            ),
            "layout and tag rows are the artifact round",
        ),
    ] {
        let root = write_root(&work, &root_manifest, source);
        let entry = root.join("app.ds");
        let entry = entry.to_str().expect("temp path is UTF-8");
        let (ok, out, err) = ds(&["run", entry]);
        assert!(!ok, "the {tag} selection refuses");
        assert_eq!(out, "");
        assert!(
            err.contains(refused),
            "the {tag} refusal names its tier (stderr: {err})"
        );
        fs::remove_dir_all(&root).expect("the temp tree removes");
    }
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A linked fn's signature binds over its own type without the type's
/// selection (G4): the fact registers from the member's surface alone,
/// so values of the type flow between linked calls — the consumer never
/// spelling the name, field access keyed on the minted atom.
#[test]
fn linked_signatures_bind_over_an_unselected_type() {
    let (work, pkg_id) = shapes_fixtures("shapes-opaque");
    let root_manifest = shapes_root_manifest(&pkg_id);
    let root = write_root(
        &work,
        &root_manifest,
        concat!(
            "import @std/io\n",
            "import @shapes (make, area)\n",
            "\n",
            "fn main() -> int {\n",
            "  const answer = area(make(3)) + make(4).n\n",
            "  println(\"${answer}\")\n",
            "  answer\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(ok, "the opaque-type run exits 0 (stderr: {err})");
    assert_eq!(out, "10\n10\n");
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A linked enum binds through its tag rows (G4): the fact registers the
/// enum and each case exactly like a local declaration, so the consumer
/// constructs, matches, and `is`-narrows over the type — and the shared
/// tag mapping (the linked module's own table rows seeded into the
/// consumer's) lets union-typed values cross both directions: the
/// producer's match over the consumer's construction, the consumer's
/// match and narrowing over the producer's, and the optional-over-struct
/// signature the tag seam used to refuse.
#[test]
fn linked_enum_facts_bind_and_run() {
    let (work, pkg_id) = shapes_fixtures("shapes-enum");
    let root_manifest = shapes_root_manifest(&pkg_id);
    let root = write_root(
        &work,
        &root_manifest,
        concat!(
            "import @std/io\n",
            "import @shapes (shade, red, green, blue, pick, score, opt)\n",
            "\n",
            "fn widen(n: int) -> shade? {\n",
            "  if n > 0 {\n",
            "    blue(n, \"w\")\n",
            "  } else {\n",
            "    none\n",
            "  }\n",
            "}\n",
            "\n",
            "fn main() -> int {\n",
            "  const own: shade = blue(5, \"m\")\n",
            "  const ownhit = match own {\n",
            "    red => 1,\n",
            "    green => 2,\n",
            "    blue(n, _) => n + 10,\n",
            "  }\n",
            "  const producer_scores = score(pick(-3)) + score(pick(0)) + score(pick(11))\n",
            "  const localmatch = match pick(2) {\n",
            "    red => 1,\n",
            "    green => 2,\n",
            "    blue(n, _) => n + 10,\n",
            "  }\n",
            "  var narrowed = 0\n",
            "  const probe = pick(4)\n",
            "  if probe is blue {\n",
            "    narrowed = probe.n\n",
            "  }\n",
            "  const opthit = match opt(9) {\n",
            "    none => 0,\n",
            "    other => other.n,\n",
            "  }\n",
            "  var w = 0\n",
            "  const wp = widen(3)\n",
            "  if wp is blue {\n",
            "    w = wp.n\n",
            "  }\n",
            "  const total = ownhit + producer_scores + localmatch + narrowed + opthit + w\n",
            "  println(\"${total}\")\n",
            "  total\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(ok, "the enum-facts run exits 0 (stderr: {err})");
    // 15 (own construct + match) + 0 + 1 + 11 (producer scores the
    // producer-made values) + 12 (consumer match over a producer value)
    // + 4 (is-narrowed payload) + 9 (the optional-over-struct answer the
    // tag seam used to refuse) + 3 (the consumer's own union over the
    // linked enum) = 55
    assert_eq!(out, "55\n55\n");
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// The type shapes the vocabulary cannot spell and the names a local
/// declaration or the consumer itself owns keep their refusals at the
/// same walk (G4): an enum with a tuple payload, an empty enum, a
/// generic enum, a generic struct, a linear struct, and a struct with a
/// field outside the word vocabulary each carry no fact; a locally
/// declared spelling of a linked type name refuses both at the type's
/// selection and at any linked signature naming it — the local would
/// mis-shape every spelling — and a case selection whose enum refused
/// the same way names its enum.
#[test]
fn the_uncarried_type_shapes_refuse() {
    let (work, pkg_id) = shapes_fixtures("shapes-tiers");
    let root_manifest = shapes_root_manifest(&pkg_id);
    for (tag, source, refused) in [
        (
            "tuple-payload",
            concat!(
                "import @shapes (trip)\n",
                "\n",
                "fn main() -> int {\n",
                "7\n",
                "}\n",
            ),
            "layout and tag rows are the artifact round",
        ),
        (
            "empty-enum",
            concat!(
                "import @shapes (hollow)\n",
                "\n",
                "fn main() -> int {\n",
                "7\n",
                "}\n",
            ),
            "layout and tag rows are the artifact round",
        ),
        (
            "generic-enum",
            concat!(
                "import @shapes (mixed)\n",
                "\n",
                "fn main() -> int {\n",
                "7\n",
                "}\n",
            ),
            "layout and tag rows are the artifact round",
        ),
        (
            "generic-struct",
            concat!(
                "import @shapes (box)\n",
                "\n",
                "fn main() -> int {\n",
                "7\n",
                "}\n",
            ),
            "layout and tag rows are the artifact round",
        ),
        (
            "linear-struct",
            concat!(
                "import @shapes (guard)\n",
                "\n",
                "fn main() -> int {\n",
                "7\n",
                "}\n",
            ),
            "layout and tag rows are the artifact round",
        ),
        (
            "nested-field",
            concat!(
                "import @shapes (nested)\n",
                "\n",
                "fn main() -> int {\n",
                "7\n",
                "}\n",
            ),
            "layout and tag rows are the artifact round",
        ),
        (
            "local-collision",
            concat!(
                "import @shapes (cell)\n",
                "\n",
                "struct cell {\n",
                "  x: int,\n",
                "}\n",
                "\n",
                "fn main() -> int {\n",
                "7\n",
                "}\n",
            ),
            "this package already declares `cell`",
        ),
        (
            "case-of-colliding-enum",
            concat!(
                "import @shapes (red)\n",
                "\n",
                "enum shade {\n",
                "  red,\n",
                "}\n",
                "\n",
                "fn main() -> int {\n",
                "7\n",
                "}\n",
            ),
            "its enum `shade` did not bind",
        ),
        (
            "signature-over-collision",
            concat!(
                "import @shapes (make)\n",
                "\n",
                "struct cell {\n",
                "  x: int,\n",
                "}\n",
                "\n",
                "fn main() -> int {\n",
                "7\n",
                "}\n",
            ),
            "a linked type whose name this package or another linked package already owns",
        ),
    ] {
        let root = write_root(&work, &root_manifest, source);
        let entry = root.join("app.ds");
        let entry = entry.to_str().expect("temp path is UTF-8");
        let (ok, out, err) = ds(&["run", entry]);
        assert!(!ok, "the {tag} selection refuses");
        assert_eq!(out, "");
        assert!(
            err.contains(refused),
            "the {tag} refusal names its tier (stderr: {err})"
        );
        fs::remove_dir_all(&root).expect("the temp tree removes");
    }
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// The linked type's two consumer-side seams refuse at the check (G4):
/// an inherent impl over the import-bound type refuses at its own
/// declaration (its members would register nowhere the receivers
/// resolve), and a bare linked type name as a dot-call receiver enters
/// the type-keyed tier, so a member the type does not carry answers the
/// checker's own refusal — the same wording the local spelling meets —
/// instead of sailing past the check into the lowering's catch-all.
#[test]
fn the_linked_type_impl_and_receiver_seams_refuse_at_the_check() {
    let (work, pkg_id) = shapes_fixtures("shapes-seams");
    let root_manifest = shapes_root_manifest(&pkg_id);
    for (tag, source, refused) in [
        (
            "impl-over-linked-type",
            concat!(
                "import @shapes (cell)\n",
                "\n",
                "impl cell {\n",
                "  fn extra(self) -> int {\n",
                "    1\n",
                "  }\n",
                "}\n",
                "\n",
                "fn main() -> int {\n",
                "  0\n",
                "}\n",
            ),
            "register nowhere this package's receivers resolve",
        ),
        (
            "linked-receiver-member",
            concat!(
                "import @shapes (cell)\n",
                "\n",
                "fn main() -> int {\n",
                "  cell.len()\n",
                "}\n",
            ),
            "type `cell` carries no member `len`",
        ),
    ] {
        let root = write_root(&work, &root_manifest, source);
        let entry = root.join("app.ds");
        let entry = entry.to_str().expect("temp path is UTF-8");
        let (ok, out, err) = ds(&["run", entry]);
        assert!(!ok, "the {tag} seam refuses");
        assert_eq!(out, "");
        assert!(
            err.contains(refused),
            "the {tag} refusal names its seam (stderr: {err})"
        );
        fs::remove_dir_all(&root).expect("the temp tree removes");
    }
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

/// The duplicate module alias a package run refuses cleanly: two
/// `import @std as a` keep the first binding and report one DS4022 —
/// the second import never mints a module id over an unpushed slot, so
/// the qualified use resolves instead of the resolver indexing out of
/// bounds.
#[test]
fn a_duplicate_module_alias_refuses_cleanly_at_a_run() {
    let work = temp_dir("dup-alias");
    let root = work.join("root");
    write_pkg(&root, "name = \"app\"\nversion = \"0.1.0\"\n", &[(
        "app.ds",
        concat!(
            "import @std as a\n",
            "import @std as a\n",
            "\n",
            "fn main() -> int {\n",
            "  a.println(\"hi\")\n",
            "  0\n",
            "}\n",
        ),
    )]);
    let (ok, out, err) = ds(&["run", root.join("app.ds").to_str().expect("path is UTF-8")]);
    assert!(!ok, "the duplicate-alias run exits nonzero");
    assert_eq!(out, "");
    assert!(
        err.contains("DS4022"),
        "the refusal names the duplicate: {err}"
    );
    assert!(!err.contains("panicked"), "no resolver panic: {err}");
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A package source-importing itself refuses with the cycle error: the
/// edge lives in the linked member's `import`, not the manifest
/// `[deps]` graph whose cycle refusal never sees it, so the link walk
/// itself must refuse instead of recursing without bound.
#[test]
fn a_self_importing_package_refuses_with_the_cycle_error() {
    let work = temp_dir("self-import");
    let manifest = "name = \"@loop\"\nversion = \"1.0.0\"\n";
    let member = concat!(
        "import @loop (twice)\n",
        "\n",
        "pub fn twice(v: int) -> int {\n",
        "  v + v\n",
        "}\n",
    );
    let pkg = work.join("loop");
    write_pkg(&pkg, manifest, &[("loop.ds", member)]);
    let (ok, out, err) = ds(&["save", pkg.to_str().expect("temp path is UTF-8")]);
    assert!(
        ok,
        "ds save installs the self-importing package (stderr: {err})"
    );
    assert!(
        out.starts_with("saved "),
        "the install reports its identity"
    );
    let root = work.join("root");
    write_pkg(
        &root,
        &format!(
            "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@loop = {{ ver = \"1.0.0\", hash = \
             \"blake3:{}\" }}\n",
            bundle_identity(manifest, &[("loop.ds", member)])
        ),
        &[(
            "app.ds",
            concat!(
                "import @loop (twice)\n",
                "\n",
                "fn main() -> int {\n",
                "  twice(1)\n",
                "}\n",
            ),
        )],
    );
    let (ok, out, err) = ds(&["run", root.join("app.ds").to_str().expect("path is UTF-8")]);
    assert!(!ok, "the self-importing run exits nonzero");
    assert_eq!(out, "");
    assert!(
        err.contains("cross-package import cycle") && err.contains("`@loop`"),
        "the refusal names the cycle: {err}"
    );
    assert!(!err.contains("overflow"), "no stack overflow: {err}");
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// Two packages source-importing each other refuse with the same cycle
/// error: the link walk re-enters the first package's compile through
/// the second's import and refuses there.
#[test]
fn mutual_source_imports_refuse_with_the_cycle_error() {
    let work = temp_dir("mutual");
    let first_manifest = "name = \"@ma\"\nversion = \"1.0.0\"\n";
    let second_manifest = "name = \"@mb\"\nversion = \"1.0.0\"\n";
    let first_member = concat!(
        "import @mb (g)\n",
        "\n",
        "pub fn f(n: int) -> int {\n",
        "  g(n)\n",
        "}\n",
    );
    let second_member = concat!(
        "import @ma (f)\n",
        "\n",
        "pub fn g(n: int) -> int {\n",
        "  f(n)\n",
        "}\n",
    );
    let ma = work.join("ma");
    write_pkg(&ma, first_manifest, &[("a.ds", first_member)]);
    let mb = work.join("mb");
    write_pkg(&mb, second_manifest, &[("b.ds", second_member)]);
    for dir in [&ma, &mb] {
        let (ok, out, err) = ds(&["save", dir.to_str().expect("temp path is UTF-8")]);
        assert!(ok, "ds save installs {} (stderr: {err})", dir.display());
        assert!(
            out.starts_with("saved "),
            "the install reports its identity"
        );
    }
    let root = work.join("root");
    write_pkg(
        &root,
        &format!(
            "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@ma = {{ ver = \"1.0.0\", hash = \
             \"blake3:{}\" }}\n@mb = {{ ver = \"1.0.0\", hash = \"blake3:{}\" }}\n",
            bundle_identity(first_manifest, &[("a.ds", first_member)]),
            bundle_identity(second_manifest, &[("b.ds", second_member)])
        ),
        &[(
            "app.ds",
            concat!(
                "import @ma (f)\n",
                "\n",
                "fn main() -> int {\n",
                "  f(1)\n",
                "}\n",
            ),
        )],
    );
    let (ok, out, err) = ds(&["run", root.join("app.ds").to_str().expect("path is UTF-8")]);
    assert!(!ok, "the mutual-import run exits nonzero");
    assert_eq!(out, "");
    assert!(
        err.contains("cross-package import cycle") && err.contains("`@ma`"),
        "the refusal names the re-entered package: {err}"
    );
    assert!(!err.contains("overflow"), "no stack overflow: {err}");
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// Runs the binary against one cache directory with the run-cache debug
/// observable on: the `ds run cache:` line on stderr names the path a
/// package run took without timing it.
fn ds_cache_debug(cache: &Path, args: &[&str]) -> (bool, String, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command
        .env("DS_CACHE", cache)
        .env("DS_RUN_CACHE_DEBUG", "1")
        .env_remove("DS_REGISTRY");
    let output = command.args(args).output().expect("the ds binary runs");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Saves one package tree into the named cache root.
fn save_into(cache: &Path, dir: &Path) {
    let (ok, out, err) = ds_env(cache, None, &["save", dir.to_str().expect("path is UTF-8")]);
    assert!(ok, "ds save installs {} (stderr: {err})", dir.display());
    assert!(
        out.starts_with("saved "),
        "the install reports the identity"
    );
}

/// The root's script-cache artifact below a private cache root:
/// dep-member artifacts share the segment, so the root is the
/// artifact whose module names the package root.
fn the_cached_artifact(cache: &Path) -> PathBuf {
    let mut stack = vec![cache.join("ir")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "dsmod") {
                let module = ds_mod::read(&fs::read(&path).expect("the artifact reads"))
                    .expect("the artifact decodes");
                if module.name() == "app" {
                    return path;
                }
            }
        }
    }
    panic!("no cached root artifact below {}", cache.display());
}

/// The warm-serve fixtures saved into one private cache root the
/// spawned runs use for the store and the script cache alike, so the
/// warm-serve assertions share state with no other test: the `@deep`
/// and `@lib` pair [`fixtures`] installs into the shared store. Answers
/// the identity the root pins `@lib` at.
fn warm_fixtures(work: &Path, cache: &Path) -> Hash128 {
    let deep = work.join("deep");
    write_pkg(&deep, &deep_manifest(), &[("deep.ds", DEEP_MEMBER)]);
    let lib_manifest = lib_manifest(bundle_identity(&deep_manifest(), &[(
        "deep.ds",
        DEEP_MEMBER,
    )]));
    let lib = work.join("lib");
    write_pkg(&lib, &lib_manifest, &[("helper.ds", LIB_MEMBER)]);
    save_into(cache, &deep);
    save_into(cache, &lib);
    bundle_identity(&lib_manifest, &[("helper.ds", LIB_MEMBER)])
}

/// A package run serves the script cache across runs: the second run of
/// the same source over the same resolution hits — the key folds the
/// resolved graph's dep identities — and the artifact the first run
/// wrote records those identities, the ambient and the linked package
/// at the member identity resolution pinned, for the hit path's walk
/// to validate.
#[test]
fn a_package_run_serves_the_script_cache_across_runs() {
    let work = temp_dir("warm-serve");
    let cache = work.join("cache");
    let lib_id = warm_fixtures(&work, &cache);
    let root = write_root(&work, &root_manifest(lib_id, false), ROOT_SOURCE);
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");

    let (ok, out, err) = ds_cache_debug(&cache, &["run", entry]);
    assert!(ok, "the cold package run exits 0 (stderr: {err})");
    assert_eq!(out, "hi\n42\n", "the call crosses two linked packages");
    assert!(
        err.contains("cache: miss"),
        "the first run compiles cold: {err}"
    );

    let (ok, warm_out, warm_err) = ds_cache_debug(&cache, &["run", entry]);
    assert!(ok, "the warm package run exits 0 (stderr: {warm_err})");
    assert_eq!(warm_out, out, "the warm run prints the cold output");
    assert!(
        warm_err.contains("cache: hit"),
        "the same source over the same resolution serves the cached root: {warm_err}"
    );

    // the artifact the run wrote records the graph's identities
    let artifact = the_cached_artifact(&cache);
    let module =
        ds_mod::read(&fs::read(&artifact).expect("the cached artifact reads")).expect("it decodes");
    assert_eq!(module.name(), "app");
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
        "the run-path writer records the ambient and the resolved package's identities"
    );

    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A dep's changed resolved content re-keys the served root: the
/// source text never changes while the store's bumped version answers
/// instead, so the miss that follows proves the dep identities in the
/// key, and the first artifact still serves the first pin.
#[test]
fn a_bumped_dep_rekeys_the_served_root() {
    let work = temp_dir("warm-bump");
    let cache = work.join("cache");
    let v1_manifest = "name = \"@wscale\"\nversion = \"1.0.0\"\n".to_owned();
    let v1_member = "pub fn scale(n: int) -> int {\n2 * n\n}\n";
    let v1 = work.join("v1");
    write_pkg(&v1, &v1_manifest, &[("scale.ds", v1_member)]);
    save_into(&cache, &v1);
    let v1_id = bundle_identity(&v1_manifest, &[("scale.ds", v1_member)]);
    let root = work.join("root");
    let manifest = |hash: Hash128, version: &str| {
        format!(
            "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@wscale = {{ ver = \"{version}\", \
             hash = \"blake3:{hash}\" }}\n"
        )
    };
    write_pkg(&root, &manifest(v1_id, "1.0.0"), &[(
        "app.ds",
        concat!(
            "import @wscale (scale)\n",
            "\n",
            "fn main() -> int {\n",
            "scale(21)\n",
            "}\n",
        ),
    )]);
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");

    let (ok, out, err) = ds_cache_debug(&cache, &["run", entry]);
    assert!(ok, "the first pinned run exits 0 (stderr: {err})");
    assert_eq!(out, "42\n");
    assert!(
        err.contains("cache: miss"),
        "the first run compiles cold: {err}"
    );
    let (ok, out, err) = ds_cache_debug(&cache, &["run", entry]);
    assert!(ok, "the second pinned run exits 0 (stderr: {err})");
    assert_eq!(out, "42\n");
    assert!(err.contains("cache: hit"), "the same pin serves: {err}");

    // the bumped version in the store: the same source text, other content
    let v2_manifest = "name = \"@wscale\"\nversion = \"2.0.0\"\n".to_owned();
    let v2_member = "pub fn scale(n: int) -> int {\n3 * n\n}\n";
    let v2 = work.join("v2");
    write_pkg(&v2, &v2_manifest, &[("scale.ds", v2_member)]);
    save_into(&cache, &v2);
    let v2_id = bundle_identity(&v2_manifest, &[("scale.ds", v2_member)]);
    fs::write(root.join("ds.pkg"), manifest(v2_id, "2.0.0")).expect("the re-pin writes");

    let (ok, out, err) = ds_cache_debug(&cache, &["run", entry]);
    assert!(ok, "the re-pinned run exits 0 (stderr: {err})");
    assert_eq!(out, "63\n", "the bumped content answers");
    assert!(
        err.contains("cache: miss"),
        "a dep's changed resolved content re-keys, never serving the old artifact: {err}"
    );
    let (ok, out, err) = ds_cache_debug(&cache, &["run", entry]);
    assert!(ok, "the re-pinned warm run exits 0 (stderr: {err})");
    assert_eq!(out, "63\n");
    assert!(err.contains("cache: hit"), "the new pin serves: {err}");

    // the first artifact still serves the first pin
    fs::write(root.join("ds.pkg"), manifest(v1_id, "1.0.0")).expect("the reversion writes");
    let (ok, out, err) = ds_cache_debug(&cache, &["run", entry]);
    assert!(ok, "the reverted run exits 0 (stderr: {err})");
    assert_eq!(out, "42\n", "the old content answers");
    assert!(
        err.contains("cache: hit"),
        "the first artifact still serves: {err}"
    );

    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// Runs the binary with both cache observables on: the `ds run cache:`
/// and `ds prefix cache:` lines on stderr name the path a package run
/// took without timing it.
fn ds_both_debug(cache: &Path, args: &[&str]) -> (bool, String, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command
        .env("DS_CACHE", cache)
        .env("DS_RUN_CACHE_DEBUG", "1")
        .env("DS_PREFIX_CACHE_DEBUG", "1")
        .env_remove("DS_REGISTRY");
    let output = command.args(args).output().expect("the ds binary runs");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Every `.dsi` prefix artifact below a cache root, recursively.
fn dsi_artifacts(cache: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![cache.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "dsi") {
                found.push(path);
            }
        }
    }
    found
}

/// Evicts every run-cache dsmod below a cache root, so the run below
/// must take the front end and consult the prefix tier — a re-pinned
/// run leaves both pins' artifacts behind.
fn evict_dsmods(cache: &Path) {
    let mut stack = vec![cache.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "dsmod") {
                fs::remove_file(&path).expect("the dsmod evicts");
            }
        }
    }
}

/// A package run persists a checked prefix a later run of the same
/// source over the same resolution serves: the cold run writes one
/// `.dsi` keyed on the graph's dep identities, an evicted dsmod makes
/// the next run take the prefix hit — the serve re-registers the
/// import-bound defs the artifact never carries — and the hit run
/// rewrites a module byte-identical to the cold one.
#[test]
fn a_package_run_serves_the_prefix_across_runs() {
    let work = temp_dir("prefix-serve");
    let cache = work.join("cache");
    let lib_id = warm_fixtures(&work, &cache);
    let root = write_root(&work, &root_manifest(lib_id, false), ROOT_SOURCE);
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");

    let (ok, cold_out, cold_err) = ds_both_debug(&cache, &["run", entry]);
    assert!(ok, "the cold package run exits 0 (stderr: {cold_err})");
    assert_eq!(cold_out, "hi\n42\n", "the call crosses two linked packages");
    assert!(
        cold_err.contains("ds prefix cache: miss"),
        "the cold run misses the prefix: {cold_err}"
    );
    assert_eq!(dsi_artifacts(&cache).len(), 1, "one prefix persists");
    let cold_module = fs::read(the_cached_artifact(&cache)).expect("the dsmod persists");

    // the eviction: only the dsmod goes away, so the run below serves
    // the prefix and re-runs the body pass over it
    fs::remove_file(the_cached_artifact(&cache)).expect("the dsmod evicts");
    let (ok, warm_out, warm_err) = ds_both_debug(&cache, &["run", entry]);
    assert!(ok, "the warm package run exits 0 (stderr: {warm_err})");
    assert_eq!(warm_out, cold_out, "the warm run prints the cold output");
    assert!(
        warm_err.contains("ds run cache: miss") && warm_err.contains("ds prefix cache: hit"),
        "the persisted prefix serves: {warm_err}"
    );
    assert_eq!(
        fs::read(the_cached_artifact(&cache)).expect("the hit run rewrites"),
        cold_module,
        "the served prefix answers a module byte-identical to the cold one"
    );
    assert_eq!(
        dsi_artifacts(&cache).len(),
        1,
        "the hit run writes no second prefix"
    );

    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A dep's changed resolved content re-keys the prefix with the dsmod:
/// the source text never changes while the store's bumped version
/// answers instead, so the prefix miss that follows proves the dep
/// identities in the prefix key, and the first prefix still serves the
/// first pin.
#[test]
fn a_bumped_dep_rekeys_the_served_prefix() {
    let work = temp_dir("prefix-bump");
    let cache = work.join("cache");
    let v1_manifest = "name = \"@wscale\"\nversion = \"1.0.0\"\n".to_owned();
    let v1_member = "pub fn scale(n: int) -> int {\n2 * n\n}\n";
    let v1 = work.join("v1");
    write_pkg(&v1, &v1_manifest, &[("scale.ds", v1_member)]);
    save_into(&cache, &v1);
    let v1_id = bundle_identity(&v1_manifest, &[("scale.ds", v1_member)]);
    let root = work.join("root");
    let manifest = |hash: Hash128, version: &str| {
        format!(
            "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@wscale = {{ ver = \"{version}\", \
             hash = \"blake3:{hash}\" }}\n"
        )
    };
    write_pkg(&root, &manifest(v1_id, "1.0.0"), &[(
        "app.ds",
        concat!(
            "import @wscale (scale)\n",
            "\n",
            "fn main() -> int {\n",
            "scale(21)\n",
            "}\n",
        ),
    )]);
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");

    let (ok, out, err) = ds_both_debug(&cache, &["run", entry]);
    assert!(ok, "the first pinned run exits 0 (stderr: {err})");
    assert_eq!(out, "42\n");
    assert!(
        err.contains("ds prefix cache: miss"),
        "the first run compiles cold: {err}"
    );
    evict_dsmods(&cache);
    let (ok, out, err) = ds_both_debug(&cache, &["run", entry]);
    assert!(ok, "the first warm run exits 0 (stderr: {err})");
    assert_eq!(out, "42\n");
    assert!(
        err.contains("ds prefix cache: hit"),
        "the first pin's prefix serves: {err}"
    );

    // the bumped version in the store: the same source text, other content
    let v2_manifest = "name = \"@wscale\"\nversion = \"2.0.0\"\n".to_owned();
    let v2_member = "pub fn scale(n: int) -> int {\n3 * n\n}\n";
    let v2 = work.join("v2");
    write_pkg(&v2, &v2_manifest, &[("scale.ds", v2_member)]);
    save_into(&cache, &v2);
    let v2_id = bundle_identity(&v2_manifest, &[("scale.ds", v2_member)]);
    fs::write(root.join("ds.pkg"), manifest(v2_id, "2.0.0")).expect("the re-pin writes");

    let (ok, out, err) = ds_both_debug(&cache, &["run", entry]);
    assert!(ok, "the re-pinned run exits 0 (stderr: {err})");
    assert_eq!(out, "63\n", "the bumped content answers");
    assert!(
        err.contains("ds prefix cache: miss"),
        "a dep's changed resolved content re-keys the prefix, never serving the old one: {err}"
    );
    evict_dsmods(&cache);
    let (ok, out, err) = ds_both_debug(&cache, &["run", entry]);
    assert!(ok, "the re-pinned warm run exits 0 (stderr: {err})");
    assert_eq!(out, "63\n");
    assert!(
        err.contains("ds prefix cache: hit"),
        "the new pin's prefix serves: {err}"
    );

    // the first prefix still serves the first pin
    fs::write(root.join("ds.pkg"), manifest(v1_id, "1.0.0")).expect("the reversion writes");
    evict_dsmods(&cache);
    let (ok, out, err) = ds_both_debug(&cache, &["run", entry]);
    assert!(ok, "the reverted run exits 0 (stderr: {err})");
    assert_eq!(out, "42\n", "the old content answers");
    assert!(
        err.contains("ds prefix cache: hit"),
        "the first prefix still serves: {err}"
    );

    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A root binding every linked entry kind — a function, a literal
/// const, and a fact-carrying struct type — persists a prefix a warm
/// run serves byte-identically: the serve re-registers the import-bound
/// defs from the fresh graph and re-installs the linked type's
/// declaration products over the replayed atom.
#[test]
fn a_linked_facts_root_serves_the_prefix_across_runs() {
    let work = temp_dir("prefix-facts");
    let cache = work.join("cache");
    // the const-facts and struct-facts fixtures, installed into this
    // test's own cache root
    let facts = work.join("facts");
    let facts_manifest = "name = \"@facts\"\nversion = \"1.0.0\"\n".to_owned();
    let facts_member = "pub const limit = 100\n\npub fn bump(n: int) -> int {\n  n + 1\n}\n";
    write_pkg(&facts, &facts_manifest, &[("facts.ds", facts_member)]);
    save_into(&cache, &facts);
    let facts_id = bundle_identity(&facts_manifest, &[("facts.ds", facts_member)]);
    let shapes = work.join("shapes");
    let shapes_manifest = "name = \"@shapes\"\nversion = \"1.0.0\"\n".to_owned();
    let shapes_member = concat!(
        "pub struct cell {\n",
        "  n: int,\n",
        "  label: string,\n",
        "}\n",
        "pub fn make(n: int) -> cell {\n",
        "  cell { n: n, label: \"made\" }\n",
        "}\n",
        "pub fn area(c: cell) -> int {\n",
        "  c.n * 2\n",
        "}\n",
        "pub enum tone {\n",
        "  quiet,\n",
        "  loud { volume: int },\n",
        "}\n",
        "pub fn pick(n: int) -> tone {\n",
        "  if n > 0 {\n",
        "    loud(n)\n",
        "  } else {\n",
        "    quiet\n",
        "  }\n",
        "}\n",
        "pub fn score(t: tone) -> int {\n",
        "  match t {\n",
        "    quiet => 0,\n",
        "    loud(v) => v,\n",
        "  }\n",
        "}\n",
    );
    write_pkg(&shapes, &shapes_manifest, &[("shapes.ds", shapes_member)]);
    save_into(&cache, &shapes);
    let shapes_id = bundle_identity(&shapes_manifest, &[("shapes.ds", shapes_member)]);
    let manifest = format!(
        "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@facts = {{ ver = \"1.0.0\", hash = \
         \"blake3:{facts_id}\" }}\n@shapes = {{ ver = \"1.0.0\", hash = \"blake3:{shapes_id}\" \
         }}\n"
    );
    let root = write_root(
        &work,
        &manifest,
        concat!(
            "import @std/io\n",
            "import @facts (limit, bump)\n",
            "import @shapes (cell, make, area, tone, quiet, loud, pick, score)\n",
            "\n",
            "fn main() -> int {\n",
            "  const c = make(limit)\n",
            "  const d = bump(c.n)\n",
            "  const t: tone = loud(d)\n",
            "  const local = match t {\n",
            "    quiet => 1,\n",
            "    loud(v) => v,\n",
            "  }\n",
            "  println(\"${area(c)} ${d} ${score(pick(3)) + local}\")\n",
            "  area(c) + d + local\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");

    let (ok, cold_out, cold_err) = ds_both_debug(&cache, &["run", entry]);
    assert!(ok, "the cold facts run exits 0 (stderr: {cold_err})");
    assert_eq!(cold_out, "200 101 104\n402\n");
    assert!(
        cold_err.contains("ds prefix cache: miss"),
        "the cold run misses: {cold_err}"
    );
    let cold_module = fs::read(the_cached_artifact(&cache)).expect("the dsmod persists");

    fs::remove_file(the_cached_artifact(&cache)).expect("the dsmod evicts");
    let (ok, warm_out, warm_err) = ds_both_debug(&cache, &["run", entry]);
    assert!(ok, "the warm facts run exits 0 (stderr: {warm_err})");
    assert_eq!(
        warm_out, cold_out,
        "the warm run prints the cold output over every linked entry kind"
    );
    assert!(
        warm_err.contains("ds prefix cache: hit"),
        "the persisted prefix serves: {warm_err}"
    );
    assert_eq!(
        fs::read(the_cached_artifact(&cache)).expect("the hit run rewrites"),
        cold_module,
        "the served prefix answers a module byte-identical to the cold one"
    );

    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A dep whose exports diverge worker-side: a world callee and a linked
/// spelling of the sibling-addressing control native.
const POOLED_DEP_MEMBER: &str = concat!(
    "pub fn shout(n: int) -> int {\n",
    "  println(\"dep\")\n",
    "  n\n",
    "}\n",
    "\n",
    "pub extern fn fiber_resume (id: int, v: int) -> int\n",
);

/// The pooled-body refusal over linked callees: the carried link row
/// refuses a world callee and the entry name refuses a control native,
/// before any worker runs.
#[test]
fn linked_callees_refuse_inside_pooled_bodies() {
    let work = temp_dir("pooled");
    let manifest = "name = \"@dep\"\nversion = \"1.0.0\"\n";
    let dep = work.join("dep");
    write_pkg(&dep, manifest, &[("lib.ds", POOLED_DEP_MEMBER)]);
    let (ok, out, err) = ds(&["save", dep.to_str().expect("temp path is UTF-8")]);
    assert!(ok, "ds save installs the dep (stderr: {err})");
    assert!(
        out.starts_with("saved "),
        "the install reports its identity"
    );
    let root = work.join("root");
    let root_manifest = format!(
        "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@dep = {{ ver = \"1.0.0\", hash = \
         \"blake3:{}\" }}\n",
        bundle_identity(manifest, &[("lib.ds", POOLED_DEP_MEMBER)])
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    write_pkg(&root, &root_manifest, &[(
        "app.ds",
        concat!(
            "import @dep (shout)\n",
            "\n",
            "fn main() -> int {\n",
            "  const xs = [1, 2]\n",
            "  const ys = xs.par_map((x: int) => shout(x))\n",
            "  ys.len()\n",
            "}\n",
        ),
    )]);
    let (ok, out, err) = ds(&["run", entry]);
    assert!(!ok, "the linked world callee inside a pooled body refuses");
    assert_eq!(out, "");
    assert!(
        err.contains("error[DS2036]") && err.contains("`shout`"),
        "the refusal is the pooled world call: {err}"
    );
    write_pkg(&root, &root_manifest, &[(
        "app.ds",
        concat!(
            "import @dep (fiber_resume)\n",
            "\n",
            "fn main() -> int {\n",
            "  const xs = [1, 2]\n",
            "  const ys = xs.par_map((x: int) => fiber_resume(x, 0))\n",
            "  ys.len()\n",
            "}\n",
        ),
    )]);
    let (ok, out, err) = ds(&["run", entry]);
    assert!(
        !ok,
        "the linked control native inside a pooled body refuses"
    );
    assert_eq!(out, "");
    assert!(
        err.contains("error[DS2035]") && err.contains("`fiber_resume`"),
        "the refusal is the pooled control native: {err}"
    );
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// The fixture artifact's library bytes: fake native-library content —
/// the slice installs and validates bytes, nothing in-tree loads them
/// yet, so any bytes serve.
const LIBRARY_BYTES: &[u8] = b"a prebuilt native library's bytes";

/// The fixture root declaring one artifact: dep-free, its source's own
/// declared extern (`str_int`, a native the ambient declares no spelling
/// for) exercising the externs seam over the installed graph.
const ARTIFACT_SOURCE: &str = concat!(
    "import @std/io (println)\n",
    "\n",
    "extern fn str_int (n: int) -> string\n",
    "\n",
    "fn main() -> int {\n",
    "println(str_int(42))\n",
    "0\n",
    "}\n",
);

/// The root manifest declaring one per-target, content-hashed artifact.
/// The triple is arbitrary data this round — install validates presence
/// and content, nothing host-filters yet.
fn artifact_manifest(identity: Hash128) -> String {
    format!(
        "name = \"app\"\nversion = \"0.1.0\"\n\n[artifacts]\nnativelib = {{ ver = \"1.0.0\", \
         target = \"x86_64-pc-windows-msvc\", hash = \"blake3:{identity}\" }}\n"
    )
}

/// One registry root whose artifact storage serves the fixture library.
fn artifact_registry(work: &Path, identity: Hash128) -> PathBuf {
    let root = work.join("registry");
    fs::create_dir_all(root.join("artifact")).expect("the artifact segment creates");
    fs::write(
        root.join("artifact").join(format!("{identity}.lib")),
        LIBRARY_BYTES,
    )
    .expect("the library lands");
    root
}

/// An artifact-declaring package resolves, installs its library from the
/// registry's artifact storage content-addressed, and runs its source's
/// declared extern; the store answers the second run without the
/// registry, and a missing artifact refuses before any compile, naming
/// the pin and the fill.
#[test]
fn an_artifact_declaring_package_installs_and_runs() {
    let work = temp_dir("artifact");
    let identity = ds_pkg::artifact_content_hash(LIBRARY_BYTES);
    let root = write_root(&work, &artifact_manifest(identity), ARTIFACT_SOURCE);
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");

    // no registry, nothing installed: the refusal names the artifact,
    // its pin, and where a fill comes from
    let cache = work.join("cache-a");
    let (ok, out, err) = ds_env(&cache, None, &["run", entry]);
    assert!(!ok, "a missing artifact refuses the run");
    assert_eq!(out, "");
    assert!(
        err.contains("artifact `nativelib` 1.0.0 for x86_64-pc-windows-msvc"),
        "the refusal names the artifact: {err}"
    );
    assert!(err.contains("blake3:"), "the refusal names the pin: {err}");
    assert!(
        err.contains("$DS_REGISTRY"),
        "the refusal names the fill: {err}"
    );

    // a registry serving the library installs it under its pin and the
    // run's declared extern answers
    let registry = artifact_registry(&work, identity);
    let cache = work.join("cache-b");
    let (ok, out, err) = ds_env(&cache, Some(&registry), &["run", entry]);
    assert!(ok, "the artifact-backed run exits 0 (stderr: {err})");
    assert_eq!(out, "42\n0\n", "the declared extern runs over the graph");
    let installed = cache.join("artifact").join(format!("{identity}.lib"));
    assert_eq!(
        fs::read(&installed).expect("the installed library reads"),
        LIBRARY_BYTES,
        "the library installed content-addressed under its pin"
    );

    // the store answers the second run with no registry at all
    let (ok, out, err) = ds_env(&cache, None, &["run", entry]);
    assert!(
        ok,
        "the installed library serves without a registry (stderr: {err})"
    );
    assert_eq!(out, "42\n0\n");

    // a registry whose storage drifted refuses hard, never serving
    // wrong bytes — the installed copy already answers, so the drift
    // surfaces only over a cold cache
    let drifted = work.join("registry-drift");
    fs::create_dir_all(drifted.join("artifact")).expect("the drifted segment creates");
    fs::write(
        drifted.join("artifact").join(format!("{identity}.lib")),
        b"drifted bytes under a foreign identity",
    )
    .expect("the drift lands");
    let cold = work.join("cache-c");
    let (ok, _, err) = ds_env(&cold, Some(&drifted), &["run", entry]);
    assert!(!ok, "drifted artifact storage refuses");
    assert!(
        err.contains("nativelib"),
        "the refusal names the artifact: {err}"
    );

    fs::remove_dir_all(&work).expect("the temp tree removes");
}

// ---- the reserved @std package (the library-boundary loading round) ----

/// The default @std package's facts the std-dep tests pin against: the
/// bundle identity the manifest's hash entry names and the toolchain
/// version the default install offers.
fn std_package_facts() -> (Package, Hash128, String) {
    let package = ds_stdlib::std_package();
    let identity = package.content_hash();
    let version = package
        .manifest(FileId::new(0))
        .expect("the default bundle's manifest parses")
        .version
        .to_string();
    (package, identity, version)
}

/// A root manifest declaring the @std dep at the given version and pin.
fn std_root_manifest(ver: &str, hash: &str) -> String {
    format!(
        "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@std = {{ ver = \"{ver}\", hash = \
         \"blake3:{hash}\" }}\n"
    )
}

/// A package declaring the @std dep resolves it through the default
/// install — no `ds save`, no registry, a cold store — and its imports
/// bind @std members through the linked surface: the selected spellings
/// check against the package's exported signatures and answer (an
/// extern's callee the compiler-wired native, a plain fact of the
/// reserved package's face), the run installs the compiled @std module
/// through the deps-first walk, and the built artifact pins `@std` at
/// the ambient source hash — the member identity the verbatim seeding
/// guarantees, the same identity the merged-ambient reading records.
#[test]
fn a_declared_std_dep_serves_the_default_install() {
    let work = temp_dir("std-default");
    let (_package, std_id, ver) = std_package_facts();
    let root = write_root(
        &work,
        &std_root_manifest(&ver, &std_id.to_string()),
        concat!(
            "import @std (concat, to_upper)\n",
            "\n",
            "fn main() -> int {\n",
            "  println(to_upper(concat(\"hi\", \"!\")))\n",
            "  0\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let cache = work.join("cache");

    let (ok, out, err) = ds_env(&cache, None, &["run", entry]);
    assert!(ok, "the default-install run exits 0 (stderr: {err})");
    assert_eq!(out, "HI!\n0\n", "the @std members answer through the link");

    // the default install landed in the content-addressed store
    assert!(
        cache.join("pkg").join(format!("{std_id}.pkg")).is_file(),
        "the tree's @std bundle installs under its own identity"
    );

    // the built artifact pins @std at the ambient source hash — the
    // verbatim seeding's identity coincidence with the merged reading
    let out_path = work.join("app.dsmod");
    let (ok, _, err) = ds_env(&cache, None, &[
        "build",
        entry,
        "--out",
        out_path.to_str().expect("path is UTF-8"),
    ]);
    assert!(ok, "the package build exits 0 (stderr: {err})");
    let bytes = fs::read(&out_path).expect("the artifact writes");
    let module = ds_mod::read(&bytes).expect("the built artifact reads");
    assert_eq!(
        module.deps(),
        &[Dep {
            name: "@std".to_owned(),
            content_hash: Hash128::compute(ds_stdlib::AMBIENT_SOURCE.as_bytes()),
        }],
        "the writer records @std at the ambient's source hash"
    );

    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// The reserved name refuses at resolution and at install: a root
/// manifest naming `@std` never resolves, and `ds save` never installs a
/// user tree under the name the toolchain's default package owns.
#[test]
fn a_user_package_named_std_refuses() {
    let work = temp_dir("std-reserved");
    let root = write_root(
        &work,
        "name = \"@std\"\nversion = \"1.0.0\"\n",
        "fn main() -> int {\n7\n}\n",
    );
    let entry = root.join("app.ds");
    let (ok, out, err) = ds(&["run", entry.to_str().expect("temp path is UTF-8")]);
    assert!(!ok, "a root named @std exits nonzero");
    assert_eq!(out, "");
    assert!(
        err.contains("reserved standard-library package name"),
        "the refusal names the reservation: {err}"
    );
    let (ok, _, err) = ds(&["save", root.to_str().expect("path is UTF-8")]);
    assert!(!ok, "ds save of an @std tree exits nonzero");
    assert!(
        err.contains("reserved standard-library package name"),
        "the save refusal names the reservation: {err}"
    );
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A demand above the tree's offering refuses with the honest bound: the
/// default install serves exactly its own version, so a manifest
/// declaring a higher minimum names what the tree offers.
#[test]
fn a_std_demand_above_the_tree_s_offering_refuses() {
    let work = temp_dir("std-version");
    let (_package, std_id, ver) = std_package_facts();
    let root = write_root(
        &work,
        &std_root_manifest("9.9.9", &std_id.to_string()),
        "fn main() -> int {\n7\n}\n",
    );
    let entry = root.join("app.ds");
    let (ok, out, err) = ds(&["run", entry.to_str().expect("temp path is UTF-8")]);
    assert!(!ok, "an unsatisfiable @std bound exits nonzero");
    assert_eq!(out, "");
    assert!(
        err.contains("`@std` at 9.9.9, above every offered version"),
        "the refusal names the demanded bound: {err}"
    );
    assert!(
        err.contains(&format!("({ver})")),
        "the refusal names the tree's offering: {err}"
    );
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// The @std linked surface keeps its tier refusals at the same walk as
/// every linked package: a generic member, a signature unioning over a
/// tag-bearing @std type (the cross-module tag seam), a signature naming
/// a @std-owned non-word type bare, and a member segment the verbatim
/// member set does not carry.
#[test]
fn the_std_linked_surface_tiers_refuse() {
    let work = temp_dir("std-tiers");
    let (_package, std_id, ver) = std_package_facts();
    let manifest = std_root_manifest(&ver, &std_id.to_string());
    for (tag, source, refused) in [
        (
            "generic",
            concat!(
                "import @std (str)\n",
                "\n",
                "fn main() -> int {\n",
                "7\n",
                "}\n",
            ),
            "cannot bind across packages this round",
        ),
        (
            "tag-union",
            concat!(
                "import @std (parse_json)\n",
                "\n",
                "fn main() -> int {\n",
                "7\n",
                "}\n",
            ),
            "a linked type whose name this package or another linked package already owns",
        ),
        (
            "handle-head",
            concat!(
                "import @std (open_file)\n",
                "\n",
                "fn main() -> int {\n",
                "7\n",
                "}\n",
            ),
            "a linked type whose name this package or another linked package already owns",
        ),
        (
            "member-segment",
            concat!(
                "import @std/csv (split)\n",
                "\n",
                "fn main() -> int {\n",
                "7\n",
                "}\n",
            ),
            "names no member of the linked package",
        ),
    ] {
        let root = write_root(&work, &manifest, source);
        let entry = root.join("app.ds");
        let (ok, out, err) = ds(&["run", entry.to_str().expect("temp path is UTF-8")]);
        assert!(!ok, "the {tag} selection refuses");
        assert_eq!(out, "");
        assert!(
            err.contains(refused),
            "the {tag} refusal names its tier (stderr: {err})"
        );
        fs::remove_dir_all(&root).expect("the temp tree removes");
    }
    fs::remove_dir_all(&work).expect("the temp tree removes");
}
