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

mod support;

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

/// A unique guarded temp dir per tag, removed at the test thread's exit.
fn temp_dir(tag: &str) -> PathBuf {
    support::guarded_dir(&format!("ds-cli-pkg-{tag}-"))
}

/// A private per-thread store: spawned runs never touch a real user
/// cache directory, and the thread's exit removes the root.
fn test_cache() -> PathBuf {
    support::shared_dir("ds-cli-pkg-cache-")
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

    // the repin: new dep content ships as a version bump, so both
    // graph identities move and both artifact families re-key — the
    // run over the new pins answers the new content, never the stale
    // member artifact
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
/// one module under its link name: each member file joins one
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

/// Runs the binary with both script-mode cache-debug observables on:
/// the `ds run cache:` and `ds prefix cache:` lines on stderr name
/// which tier each lookup took, the observable byte-identical output
/// cannot prove.
fn ds_caches_debug(cache: &Path, args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_ds"))
        .env("DS_CACHE", cache)
        .env("DS_RUN_CACHE_DEBUG", "1")
        .env("DS_PREFIX_CACHE_DEBUG", "1")
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

/// A multi-member package root composes one namespace at the root:
/// `ds run` and `ds build` compile the entry member and every
/// sibling below the root into one module — the same member walk a
/// dep-side compile rides — so the root that resolves when it is a dep
/// resolves when it runs. Both script-mode caches key on the composed
/// member fold, so a sibling edit is another key (the warm legs prove
/// both tiers re-serve) and the built artifact's header carries the
/// composed identity, exactly a dep-side dsmod's. A broken sibling
/// refuses under the sibling's own member path, the member grammar's
/// diagnostic spelling.
#[test]
fn a_multi_member_root_composes_and_rekeys_on_sibling_edits() {
    let work = temp_dir("multi-root");
    let root = work.join("root");
    let manifest = "name = \"app\"\nversion = \"0.1.0\"\n".to_owned();
    let entry_source = "fn main() -> int {\ntwice(3)\n}\n";
    let helper = "pub fn twice(n: int) -> int {\n2 * n\n}\n";
    let members = [("app.ds", entry_source), ("helper.ds", helper)];
    write_pkg(&root, &manifest, &members);
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let cache = work.join("cache");

    // the cold run: both tiers miss and the compose answers
    let (ok, out, err) = ds_caches_debug(&cache, &["run", entry]);
    assert!(ok, "the multi-member root run exits 0 (stderr: {err})");
    assert_eq!(out, "6\n", "the sibling member binds inside the root");
    assert!(err.contains("ds run cache: miss"), "the root misses: {err}");
    assert!(
        err.contains("ds prefix cache: miss"),
        "the prefix misses: {err}"
    );

    // the warm run: the run artifact serves, byte-identical output (a
    // run-cache hit installs without the front end, so the prefix tier's
    // own observable rides the build leg below)
    let (ok, out, err) = ds_caches_debug(&cache, &["run", entry]);
    assert!(ok, "the warm multi-member root run exits 0 (stderr: {err})");
    assert_eq!(out, "6\n", "the warm run prints the cold output");
    assert!(err.contains("ds run cache: hit"), "the root serves: {err}");

    // the build leg carries the prefix tier's observable: a build always
    // compiles, so the stored prefix's hit line proves the same member
    // set's `.dsi` serves; the artifact is one module over both members,
    // its header the composed fold, exactly a dep-side dsmod's
    let out_path = work.join("app.dsmod");
    let (ok, _, err) = ds_caches_debug(&cache, &[
        "build",
        entry,
        "--out",
        out_path.to_str().expect("temp path is UTF-8"),
    ]);
    assert!(ok, "the multi-member root build exits 0 (stderr: {err})");
    assert!(
        err.contains("ds prefix cache: hit"),
        "the stored prefix serves the same member set: {err}"
    );
    let bytes = fs::read(&out_path).expect("the artifact writes");
    let module = ds_mod::read(&bytes).expect("the built artifact reads");
    assert_eq!(module.name(), "app");
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
    assert_eq!(
        module.source_hash(),
        folded,
        "the artifact's header carries the composed member identity"
    );
    let (ok, out, err) = ds(&["run", out_path.to_str().expect("temp path is UTF-8")]);
    assert!(ok, "the built artifact runs (stderr: {err})");
    assert_eq!(out, "6\n");

    // the sibling edit: the composed fold moves, so both tiers re-key
    // and the run answers the new member set, never a warm artifact of
    // the old one
    fs::write(
        root.join("helper.ds"),
        "pub fn twice(n: int) -> int {\n3 * n\n}\n",
    )
    .expect("the sibling edit writes");
    let (ok, out, err) = ds_caches_debug(&cache, &["run", entry]);
    assert!(ok, "the sibling-edited run exits 0 (stderr: {err})");
    assert_eq!(out, "9\n", "the new member set serves");
    assert!(
        err.contains("ds run cache: miss"),
        "the sibling edit re-keys: {err}"
    );
    assert!(
        err.contains("ds prefix cache: miss"),
        "the sibling edit re-keys the prefix tier: {err}"
    );
    let (ok, out, err) = ds_caches_debug(&cache, &["run", entry]);
    assert!(ok, "the re-keyed warm run exits 0 (stderr: {err})");
    assert_eq!(out, "9\n");
    assert!(
        err.contains("ds run cache: hit"),
        "the new key serves warm: {err}"
    );
    let (ok, _, err) = ds_caches_debug(&cache, &[
        "build",
        entry,
        "--out",
        out_path.to_str().expect("temp path is UTF-8"),
    ]);
    assert!(ok, "the sibling-edited build exits 0 (stderr: {err})");
    assert!(
        err.contains("ds prefix cache: hit"),
        "the new member set's prefix serves warm: {err}"
    );

    // a broken sibling refuses under its own member path: the sibling
    // joined the compile, so its diagnostic names it
    fs::write(root.join("helper.ds"), "fn broken {\n").expect("the broken sibling writes");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(!ok, "a broken sibling member refuses the run");
    assert_eq!(out, "");
    assert!(
        err.contains("--> helper.ds:"),
        "the diagnostic names the sibling: {err}"
    );

    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A three-member root binds a cross-member impl: the struct lives in
/// one member, its trait impl in a second, and the entry calls through
/// the trait's dispatch from the third — one namespace, the members
/// composing exactly as `ds check` over the same three files checks.
#[test]
fn a_three_member_root_binds_a_cross_member_impl() {
    let work = temp_dir("three-root");
    let root = work.join("root");
    let manifest = "name = \"app\"\nversion = \"0.1.0\"\n".to_owned();
    write_pkg(&root, &manifest, &[
        ("cell.ds", "pub struct cell {\n  n: int,\n}\n"),
        (
            "impls.ds",
            "impl show for cell {\n  fn show(self) -> string {\n    \"cell ${self.n}\"\n  }\n}\n",
        ),
        (
            "app.ds",
            concat!(
                "import @std/io\n",
                "\n",
                "fn main() -> int {\n",
                "  const c = cell { n: 3 }\n",
                "  println(\"${c.show()}\")\n",
                "  len(c.show())\n",
                "}\n",
            ),
        ),
    ]);
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(ok, "the three-member root run exits 0 (stderr: {err})");
    assert_eq!(out, "cell 3\n6\n", "the cross-member impl dispatches");
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A whole-package import and a module alias bind: `import @wide`
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

/// The import selection tiers refuse at the use they fail: a
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
        "pub const shout = concat(\"dr\", \"eam\")
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

/// A linked package's plain-literal and computed consts bind and run:
/// the value is the per-target link fact — a plain initializer's
/// literal, a computed initializer's dep-side evaluation answer — so
/// every root use embeds it exactly as the literal a user would have
/// written, over every scalar shape (int with a negation, string,
/// float, bool, char, byte) and the computed int and string beside
/// them.
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
            "import @facts (limit, step, greeting, ratio, ready, initial, code, computed, shout)
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
            "  println(\"computed = ${computed} shout = ${shout}\")
",
            "  limit + step + computed + (if ready { 1 } else { 0 }) + len(bytes_of([code, code]))
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
computed = 42 shout = dream
142
"
    );
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A linked package's alias-qualified const reads in value position:
/// the qualified spelling binds through the same value fact the
/// selection embeds — plain and computed alike, over every scalar shape
/// and the nested contexts (an argument, a list element, an
/// interpolation hole, an `if` condition) — so the run answers exactly
/// what the selection's does.
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
            "  println(\"computed = ${w.computed} shout = ${w.shout}\")
",
            "  w.limit + w.step + w.computed + (if w.ready { 1 } else { 0 }) + \
             len(bytes_of([w.code, w.code]))
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
computed = 42 shout = dream
142
"
    );
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// The const shapes with no value fact keep their refusals at the same
/// walk: a mutable binding and the alias-qualified spellings — the
/// const's call form, a private const, a missing name, a function
/// value, and a type name in value position.
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
            "its value is no const fact",
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

/// Saves one single-member fixture package under `@probe` and answers
/// its bundle identity: the dep-side const evaluation's refusal tests'
/// producer, its member the caller's own text.
fn probe_fixtures(tag: &str, member: &str) -> (PathBuf, String) {
    let work = temp_dir(tag);
    let pkg = work.join("probe");
    let manifest = "name = \"@probe\"
version = \"1.0.0\"
"
    .to_owned();
    let members = [("probe.ds", member)];
    write_pkg(&pkg, &manifest, &members);
    let (ok, _, err) = ds(&["save", pkg.to_str().expect("temp path is UTF-8")]);
    assert!(ok, "the probe fixture installs (stderr: {err})");
    let pkg_id = bundle_identity(&manifest, &members).to_string();
    (work, pkg_id)
}

/// One probe fixture's consumer root: a manifest depending on `@probe`
/// and a main importing `safe` alone — the refusals under test fire on
/// the producer's own compile, whatever the consumer selects.
fn probe_root(work: &Path, pkg_id: &str) -> PathBuf {
    let root_manifest = format!(
        "name = \"app\"
version = \"0.1.0\"

[deps]
@probe = {{ ver = \"1.0.0\", hash = \"blake3:{pkg_id}\" }}
"
    );
    write_root(
        work,
        &root_manifest,
        concat!(
            "import @probe (safe)
",
            "
",
            "fn main() -> int {
",
            "  safe()
",
            "}
",
        ),
    )
}

/// A linked package's exported const whose initializer traps refuses
/// the producer's own artifact build: the dep-side evaluation runs
/// on the producer's compile, and a trapping initializer means the
/// value a dependent would read does not exist — the refusal names the
/// const and its initializer with the const's own code, never a silent
/// drop or a consumer-side mystery.
#[test]
fn a_trapping_exported_const_refuses_the_producer_build() {
    let (work, pkg_id) = probe_fixtures(
        "const-trap",
        concat!(
            "pub const bad = 1 / 0
",
            "
",
            "pub fn safe() -> int {
",
            "  1
",
            "}
",
        ),
    );
    let root = probe_root(&work, &pkg_id);
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(!ok, "the trapping const refuses the producer build");
    assert_eq!(out, "");
    assert!(
        err.contains("DS2043") && err.contains("pub const bad") && err.contains("trapped"),
        "the refusal names the const and the trap (stderr: {err})"
    );
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A linked package's exported const whose initializer touches the
/// world surface refuses at the same producer-side build: the
/// evaluation fence admits only pure and trap-only reaches, so a world
/// call inside an initializer is a compile error naming the capability.
#[test]
fn a_world_reaching_exported_const_refuses_the_producer_build() {
    let (work, pkg_id) = probe_fixtures(
        "const-world",
        concat!(
            "import @std/io
",
            "
",
            "pub fn safe() -> int {
",
            "  1
",
            "}
",
            "
",
            "pub const noise = { println(\"side\")
",
            "5 }
",
        ),
    );
    let root = probe_root(&work, &pkg_id);
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(!ok, "the world-reaching const refuses the producer build");
    assert_eq!(out, "");
    assert!(
        err.contains("DS2043") && err.contains("world surface") && err.contains("pub const noise"),
        "the refusal names the const and the capability (stderr: {err})"
    );
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
        "pub fn total(p: pair) -> int {\n",
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
        "pub tag ping\n",
        "pub tag pong\n",
        "pub fn rally(n: int) -> ping | pong {\n",
        "  if n > 0 {\n",
        "    ping\n",
        "  } else {\n",
        "    pong\n",
        "  }\n",
        "}\n",
        "pub fn point(p: ping | pong) -> int {\n",
        "  match p {\n",
        "    ping => 1,\n",
        "    pong => 2,\n",
        "  }\n",
        "}\n",
        "pub enum trip {\n",
        "  one(int, int),\n",
        "}\n",
        "pub fn trip_first(t: trip) -> int {\n",
        "  match t {\n",
        "    one(a, _) => a,\n",
        "  }\n",
        "}\n",
        "pub fn trip_make(a: int, b: int) -> trip {\n",
        "  one(a, b)\n",
        "}\n",
        "pub fn drain(g: guard) -> int {\n",
        "  g.n\n",
        "}\n",
        "pub struct owned: ~Copyable {\n",
        "  n: int,\n",
        "  deinit { }\n",
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
        "pub struct spread {\n",
        "  xs: list[int]?,\n",
        "  grid: list[list[byte]],\n",
        "  table: map[string, int],\n",
        "}\n",
        "pub fn sparse(n: int) -> spread {\n",
        "  var table = map[string, int]()\n",
        "  table.set(\"k\", 7)\n",
        "  if n > 0 {\n",
        "    spread { xs: [n], grid: [[b'a'], [b'b']], table: table }\n",
        "  } else {\n",
        "    spread { xs: none, grid: [], table: table }\n",
        "  }\n",
        "}\n",
        "pub fn weigh(s: spread) -> int {\n",
        "  const head = match s.xs {\n",
        "    none => 0,\n",
        "    only => len(only),\n",
        "  }\n",
        "  const keyed = match s.table.get(\"k\") {\n",
        "    none => 0,\n",
        "    v => v,\n",
        "  }\n",
        "  head + len(s.grid) + keyed\n",
        "}\n",
        "pub struct fleet {\n",
        "  head: cell,\n",
        "  tail: list[cell],\n",
        "  pick: cell?,\n",
        "  index: map[string, cell],\n",
        "  tint: shade,\n",
        "}\n",
        "pub fn fleet_of(n: int) -> fleet {\n",
        "  var index = map[string, cell]()\n",
        "  index.set(\"k\", cell { n: n, label: \"i\", tags: [], ratio: 0.5, on: true, initial: \
         'i', code: b'i' })\n",
        "  fleet { head: make(n), tail: [make(1)], pick: opt(n), index: index, tint: pick(n + 1) \
         }\n",
        "}\n",
        "pub fn fleet_weight(f: fleet) -> int {\n",
        "  const picked = match f.pick {\n",
        "    none => 0,\n",
        "    other => other.n,\n",
        "  }\n",
        "  const keyed = match f.index.get(\"k\") {\n",
        "    none => 0,\n",
        "    v => v.n,\n",
        "  }\n",
        "  area(f.head) + len(f.tail) + picked + keyed + score(f.tint)\n",
        "}\n",
        "pub type words = list[string]\n",
        "pub type chain = words\n",
        "pub type wide[k] = map[k, int]\n",
        "pub type either = int | string\n",
        "pub type cells = list[cell]\n",
        "pub struct partisan {\n",
        "  pick: red | green,\n",
        "}\n",
        "pub fn mk_partisan(n: int) -> partisan {\n",
        "  if n > 0 {\n",
        "    partisan { pick: green }\n",
        "  } else {\n",
        "    partisan { pick: red }\n",
        "  }\n",
        "}\n",
        "pub fn tally(p: partisan) -> int {\n",
        "  match p.pick {\n",
        "    red => 5,\n",
        "    green => 6,\n",
        "  }\n",
        "}\n",
        "pub fn head_of(xs: cells) -> int {\n",
        "  const first = xs.get(0)\n",
        "  match first {\n",
        "    none => 0,\n",
        "    c => c.n,\n",
        "  }\n",
        "}\n",
        "pub trait shapey {\n",
        "  fn size(self) -> int\n",
        "}\n",
        "impl shapey for pair {\n",
        "  fn size(self) -> int {\n",
        "    self.a + self.b\n",
        "  }\n",
        "}\n",
        "impl shapey for cell {\n",
        "  fn size(self) -> int {\n",
        "    self.n\n",
        "  }\n",
        "}\n",
        "pub trait defaulted {\n",
        "  fn answer(self) -> int {\n",
        "    7\n",
        "  }\n",
        "}\n",
        "impl defaulted for int {\n",
        "}\n",
        "impl defaulted for string {\n",
        "  fn answer(self) -> int {\n",
        "    5\n",
        "  }\n",
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

/// A linked flat struct binds through its structural fact: the
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

/// A linked struct whose fields ride the word vocabulary's own closure
/// binds and runs across the boundary: an optional over a list, a
/// nested list, and a map over scalars all mint exactly the local
/// spelling's declaration products, so the consumer constructs each
/// field (the optional's `none` side included), reads them through the
/// collection faces, and moves whole values into the linked module's
/// own functions — the map field crossing both directions with no
/// e2e before it.
#[test]
fn linked_container_word_fields_bind_and_run() {
    let (work, pkg_id) = shapes_fixtures("shapes-spread");
    let root_manifest = shapes_root_manifest(&pkg_id);
    let root = write_root(
        &work,
        &root_manifest,
        concat!(
            "import @std/io\n",
            "import @shapes (spread, sparse, weigh)\n",
            "\n",
            "fn main() -> int {\n",
            "  const s = spread { xs: [3], grid: [[b'x'], [b'y']], table: map[string, int]() }\n",
            "  const made = sparse(2)\n",
            "  var total = weigh(s) + weigh(made)\n",
            "  const own = spread { xs: none, grid: [], table: map[string, int]() }\n",
            "  total = total + weigh(own)\n",
            "  println(\"${total}\")\n",
            "  total\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(ok, "the spread run exits 0 (stderr: {err})");
    // weigh(s) = len([3]) + len(grid) + miss = 1 + 2 + 0 = 3; the sparse
    // twin adds 1 + 2 + 7 = 10 over its filled table; the empty own adds
    // 0 — 13 in all, the run echoing the entry's return after the println
    assert_eq!(out, "13\n13\n");
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A linked struct whose fields name another linked declared type of
/// the same member binds and runs across the boundary: the Named
/// word — a direct nested field, a `list[cell]`, a `cell?`, a
/// `map[string, cell]`, and a field naming the member's enum — resolves
/// onto the consumer's own mint for that same declaration, so the
/// consumer constructs every field (the nested literal, a list literal
/// of linked calls, the optional's absent side, the empty map), reads
/// the nested fields back, and moves whole values into the linked
/// module's own functions.
#[test]
fn linked_named_type_fields_bind_and_run() {
    let (work, pkg_id) = shapes_fixtures("shapes-fleet");
    let root_manifest = shapes_root_manifest(&pkg_id);
    let root = write_root(
        &work,
        &root_manifest,
        concat!(
            "import @std/io\n",
            "import @shapes (cell, make, fleet, fleet_of, fleet_weight, green)\n",
            "\n",
            "fn main() -> int {\n",
            "  const made = fleet_of(3)\n",
            "  var total = fleet_weight(made)\n",
            "  const own = fleet {\n",
            "    head: cell { n: 5, label: \"x\", tags: [], ratio: 0.5, on: true, initial: 'd', \
             code: b'x' },\n",
            "    tail: [make(1), make(2)],\n",
            "    pick: none,\n",
            "    index: map[string, cell](),\n",
            "    tint: green,\n",
            "  }\n",
            "  total = total + fleet_weight(own)\n",
            "  const picked = match own.pick {\n",
            "    none => 0,\n",
            "    other => other.n,\n",
            "  }\n",
            "  total = total + own.head.n + len(own.tail) + picked\n",
            "  println(\"${total}\")\n",
            "  total\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(ok, "the fleet run exits 0 (stderr: {err})");
    // fleet_weight(made) = area(make(3)) 6 + len([make(1)]) 1 + opt(3).n 3
    // + index["k"].n 3 + score(pick(4)) 4 = 17; fleet_weight(own) = 10 + 2
    // + 0 + 0 + score(green) 1 = 13; the reads add 5 + 2 + 0 — 37 in
    // all, the run echoing the entry's return after the println
    assert_eq!(out, "37\n37\n");
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
        "pub struct widemap {\n",
        "  index: map[string, blob],\n",
        "}\n",
        "pub struct pairfield {\n",
        "  p: (int, int),\n",
        "}\n",
        "pub struct tagged_pair {\n",
        "  t: (string, list[byte]),\n",
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
        "pub fn total(b: blob) -> int {\n",
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
        "pub fn mk_pair(a: int, b: int) -> pairfield {\n",
        "  pairfield { p: (a, b) }\n",
        "}\n",
        "pub fn pair_sum(pf: pairfield) -> int {\n",
        "  const (a, b) = pf.p\n",
        "  a + b\n",
        "}\n",
        "pub fn mk_tagged(label: string) -> tagged_pair {\n",
        "  tagged_pair { t: (label, [b'k']) }\n",
        "}\n",
        "pub fn tag_weight(tp: tagged_pair) -> int {\n",
        "  const (label, raw) = tp.t\n",
        "  len(label) + len(raw)\n",
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
/// fact: the field is one Ref word like a string field, so it joins
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
            "import @blobs (blob, mk, wrap, total, tone, flat, loaded, pick, score)\n",
            "\n",
            "fn main() -> int {\n",
            "  const b = blob { n: 3, data: utf8_encode(\"hi\") }\n",
            "  const made = mk(10)\n",
            "  var count = total(b) + total(made)\n",
            "  const twin = blob { n: 3, data: utf8_encode(\"hi\") }\n",
            "  const other = blob { n: 3, data: utf8_encode(\"no\") }\n",
            "  if b == twin { count = count + 1 }\n",
            "  if b == other { println(\"bad\") } else { count = count + 1 }\n",
            "  const maybe = wrap(made, \"made\")\n",
            "  if maybe is blob {\n",
            "    count = count + maybe.n\n",
            "  }\n",
            "  const back = match wrap(b, \"x\") {\n",
            "    none => 0,\n",
            "    other => other.n,\n",
            "  }\n",
            "  count = count + back + score(pick(2))\n",
            "  const localmatch = match pick(1) {\n",
            "    flat => 1,\n",
            "    loaded(n, _) => n,\n",
            "  }\n",
            "  count = count + localmatch\n",
            "  var w = 0\n",
            "  const probe = pick(4)\n",
            "  if probe is loaded {\n",
            "    w = len(probe.data)\n",
            "  }\n",
            "  count = count + w\n",
            "  const head = match byte_at(b.data, 0) {\n",
            "    none => 0,\n",
            "    code => int_of_byte(code),\n",
            "  }\n",
            "  count = count + head\n",
            "  println(\"${b.data}\")\n",
            "  count\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(ok, "the bytes-field run exits 0 (stderr: {err})");
    // total(b) = 3+2 and total(made) = 10+2 open at 17, the == chain adds 2
    // (the equal twin, the differing content answered unequal), the
    // is-narrow adds 10, the match arm adds 3, the enum round adds 2
    // (score) + 1 (local match) + 6 (the narrowed payload's len), the
    // checked byte_at read adds 104 ('h'), and the show dispatch prints
    // the payload; the run echoes the entry's return after the println
    assert_eq!(out, "bytes[104, 105]\n145\n");
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A linked struct's tuple field binds through its word-layout fact
/// fact: the positional product is one plain word per member, so it
/// joins the linked word vocabulary and the consumer constructs,
/// destructures in place, and passes values through the producer's own
/// tuple-taking functions — a scalar pair and a pair over the Ref
/// words (string, list[byte]) both crossing the packages.
#[test]
fn the_tuple_fields_cross_and_run() {
    let (work, pkg_id) = blobs_fixtures("blobs-tuple");
    let root_manifest = blobs_root_manifest(&pkg_id);
    let root = write_root(
        &work,
        &root_manifest,
        concat!(
            "import @std/io\n",
            "import @blobs (pairfield, tagged_pair, mk_pair, pair_sum, mk_tagged, tag_weight)\n",
            "\n",
            "fn main() -> int {\n",
            "  const own = pairfield { p: (3, 4) }\n",
            "  const (a, b) = own.p\n",
            "  const made = mk_pair(10, 20)\n",
            "  var total = a + b + pair_sum(made) + pair_sum(own)\n",
            "  const tagged = mk_tagged(\"hey\")\n",
            "  const (label, raw) = tagged.t\n",
            "  total = total + len(label) + len(raw) + tag_weight(tagged)\n",
            "  const own_tagged = tagged_pair { t: (\"hi\", [b'z', b'q']) }\n",
            "  total = total + tag_weight(own_tagged)\n",
            "  println(\"${total}\")\n",
            "  total\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(ok, "the tuple-field run exits 0 (stderr: {err})");
    // the destructure 7, the made pair 30, the own pair 7, the tagged
    // legs 3+1+4, and the consumer-built tagged pair 2+2 — 56 in all,
    // the run echoing the entry's return after the println
    assert_eq!(out, "56\n56\n");
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A producer's trait impls cross the link seam: the trait's
/// member rows bind as foreign method-registry rows, the impl's
/// owner-keyed member links declare the instance over the minted head,
/// and the consumer's dot-call dispatches through CALLG to the linked
/// member body — a plain site, a tail site, and a second head all
/// answering, over both a consumer-built value and the producer's own.
#[test]
fn the_trait_impls_cross_and_run() {
    let (work, pkg_id) = shapes_fixtures("shapes-traits");
    let root_manifest = shapes_root_manifest(&pkg_id);
    let root = write_root(
        &work,
        &root_manifest,
        concat!(
            "import @std/io\n",
            "import @shapes (pair, cell, shapey, make)\n",
            "\n",
            "fn weigh(p: pair) -> int {\n",
            "  p.size()\n",
            "}\n",
            "\n",
            "fn main() -> int {\n",
            "  const made = pair { a: 30, b: 12 }\n",
            "  const once = made.size()\n",
            "  const c = make(4)\n",
            "  println(\"${once + weigh(made) + c.size()}\")\n",
            "  once + weigh(made) + c.size()\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(ok, "the trait-impl run exits 0 (stderr: {err})");
    // the plain site 42, the tail site 42, and the second head's
    // producer-made value 4 — 88 in all, the run echoing the return
    assert_eq!(out, "88\n88\n");
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A producer's named impls cross the link seam with their vocabulary:
/// the linked row declares into its slot, a
/// consumer call site's `as` selection resolves and dispatches the named
/// body through the same CALLG machinery, the qualified spelling selects
/// the same instance, and the unnamed row keeps answering the plain
/// spelling — the default vocabulary unchanged beside the named slot.
#[test]
fn the_named_impls_cross_and_select_at_call_sites() {
    let (work, pkg_id) = bespoke_pkg(
        "lib-named-instances",
        concat!(
            "pub trait gauge {\n",
            "  fn weight(self) -> int\n",
            "}\n",
            "pub struct lump {\n",
            "  n: int,\n",
            "}\n",
            "impl gauge for lump {\n",
            "  fn weight(self) -> int { self.n }\n",
            "}\n",
            "impl gauge for lump as heavy {\n",
            "  fn weight(self) -> int { self.n * 10 }\n",
            "}\n",
        ),
    );
    let (ok, out, err) = bespoke_run(
        &work,
        &pkg_id,
        concat!(
            "import @lib (gauge, lump)\n",
            "\n",
            "fn main() -> int {\n",
            "  const m = lump { n: 3 }\n",
            "  const plain = m.weight()\n",
            "  const named = m.weight() as heavy\n",
            "  const viaq = gauge::weight(m) as heavy\n",
            "  println(\"${plain} ${named} ${viaq}\")\n",
            "  plain + named + viaq\n",
            "}\n",
        ),
    );
    assert!(ok, "the named-impl run exits 0 (stderr: {err})");
    // The plain site 3 over the unnamed row, both selections 30 over
    // the named one — the run echoing the return 63.
    assert_eq!(out, "3 30 30\n63\n");
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A consumer's selection naming no impl the link carries refuses at
/// the consumer's own compile (DS2051): the vocabulary is the closed
/// set of the consuming compile's slots.
#[test]
fn a_selection_no_linked_impl_carries_refuses() {
    let (work, pkg_id) = bespoke_pkg(
        "lib-named-missing",
        concat!(
            "pub trait gauge {\n",
            "  fn weight(self) -> int\n",
            "}\n",
            "pub struct lump {\n",
            "  n: int,\n",
            "}\n",
            "impl gauge for lump as heavy {\n",
            "  fn weight(self) -> int { self.n * 10 }\n",
            "}\n",
        ),
    );
    let (ok, _, err) = bespoke_run(
        &work,
        &pkg_id,
        concat!(
            "import @lib (gauge, lump)\n",
            "\n",
            "fn main() -> int {\n",
            "  const m = lump { n: 3 }\n",
            "  m.weight() as brisk\n",
            "}\n",
        ),
    );
    assert!(!ok, "the missing selection must refuse");
    assert!(
        err.contains("DS2051") && err.contains("`as brisk`"),
        "the refusal names the selection: {err}"
    );
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// One bespoke fixture package answering a single root: writes the
/// member source as `lib.ds`, installs it, and returns the work dir
/// with the root manifest pinning the installed identity.
fn bespoke_pkg(tag: &str, member: &str) -> (PathBuf, String) {
    let work = temp_dir(tag);
    let pkg = work.join("lib");
    let manifest = "name = \"@lib\"\nversion = \"1.0.0\"\n".to_owned();
    write_pkg(&pkg, &manifest, &[("lib.ds", member)]);
    let (ok, _, err) = ds(&["save", pkg.to_str().expect("temp path is UTF-8")]);
    assert!(ok, "the {tag} fixture installs (stderr: {err})");
    let pkg_id = bundle_identity(&manifest, &[("lib.ds", member)]).to_string();
    (work, pkg_id)
}

/// One bespoke root over the installed fixture: the run's flattened
/// outcome.
fn bespoke_run(work: &Path, pkg_id: &str, source: &str) -> (bool, String, String) {
    let manifest = format!(
        "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@lib = {{ ver = \"1.0.0\", hash = \
         \"blake3:{pkg_id}\" }}\n"
    );
    let root = write_root(work, &manifest, source);
    let entry = root.join("app.ds");
    ds(&["run", entry.to_str().expect("temp path is UTF-8")])
}

/// The qualified `Trait::f` spelling crosses the link seam over a
/// linked trait: the import's registration mints the trait's def
/// outside the type namespace, so the qualified head resolves through
/// the foreign table and the call dispatches through the same per-site
/// record the dot spelling takes — plain and tail sites both.
#[test]
fn the_qualified_trait_spelling_dispatches_over_the_link() {
    let (work, pkg_id) = shapes_fixtures("shapes-qualified");
    let root_manifest = shapes_root_manifest(&pkg_id);
    let root = write_root(
        &work,
        &root_manifest,
        concat!(
            "import @shapes (pair, shapey)\n",
            "\n",
            "fn weigh(p: pair) -> int {\n",
            "  shapey::size(p)\n",
            "}\n",
            "\n",
            "fn main() -> int {\n",
            "  const p = pair { a: 30, b: 12 }\n",
            "  const viaq = shapey::size(p)\n",
            "  println(\"${viaq} ${weigh(p)}\")\n",
            "  viaq + weigh(p)\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let (ok, out, err) = ds(&["run", entry.to_str().expect("temp path is UTF-8")]);
    assert!(ok, "the qualified run exits 0 (stderr: {err})");
    // the plain site 42, the tail site 42 — the run echoing the return
    assert_eq!(out, "42 42\n84\n");
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A producer's trait default bodies cross the link seam: the
/// member's row carries the declared body's table index, the mint
/// marks the member omittable, and a consumer's dispatch whose solving
/// impl spells no member — the producer's own omitting impl or the
/// consumer's local one — CALLGs into the linked default body, the
/// spelled impl answering its own body over the same trait.
#[test]
fn the_trait_default_bodies_cross_and_run() {
    let (work, pkg_id) = shapes_fixtures("shapes-defaults");
    let manifest = shapes_root_manifest(&pkg_id);
    let root = write_root(
        &work,
        &manifest,
        concat!(
            "import @shapes (defaulted)\n",
            "\n",
            "impl defaulted for float {\n",
            "}\n",
            "\n",
            "fn weigh(s: string) -> int {\n",
            "  s.answer()\n",
            "}\n",
            "\n",
            "fn main() -> int {\n",
            "  const once = 3.answer()\n",
            "  const own = 2.5.answer()\n",
            "  once + own + weigh(\"x\")\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(ok, "the default-body run exits 0 (stderr: {err})");
    // the linked omitting impl's site 7, the consumer's own omitting
    // impl 7, and the spelled string head's tail site 5
    assert_eq!(out, "19\n", "the run echoes the dispatched total");
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A default body the link walk cannot carry refuses at the producer's
/// own walk, naming the member at its declaration: a body dispatching
/// on `self` stages runtime evidence a consumer cannot build, and a
/// body carrying an effect row unions nothing at the consumer's
/// dispatch — both fail the producer's compile before any consumer
/// sees them, exactly like a dep member's own checker refusals.
#[test]
fn the_uncarriable_default_bodies_refuse_at_the_producer() {
    for (tag, member, refused) in [
        (
            "demanded-body",
            concat!(
                "pub trait chained {\n",
                "  fn a(self) -> int\n",
                "  fn b(self) -> int {\n",
                "    self.a()\n",
                "  }\n",
                "}\n",
                "impl chained for int {\n",
                "  fn a(self) -> int {\n",
                "    3\n",
                "  }\n",
                "}\n",
            ),
            "default body for `b` cannot cross packages this round: the body stages runtime \
             evidence",
        ),
        (
            "effectful-body",
            concat!(
                "import @std/io\n",
                "\n",
                "pub trait noisy {\n",
                "  fn speak(self) -> int {\n",
                "    7\n",
                "  }\n",
                "  fn quiet(self) -> int {\n",
                "    println(\"x\")\n",
                "    7\n",
                "  }\n",
                "}\n",
            ),
            "default body for `quiet` cannot cross packages this round: the body carries an \
             effect row",
        ),
    ] {
        let (work, pkg_id) = bespoke_pkg(tag, member);
        let root_source = match tag {
            "demanded-body" => "import @lib (chained)\n\nfn main() -> int {\n  7\n}\n",
            _ => "import @lib (noisy)\n\nfn main() -> int {\n  7\n}\n",
        };
        let (ok, out, err) = bespoke_run(&work, &pkg_id, root_source);
        assert!(!ok, "the {tag} producer refuses");
        assert_eq!(out, "");
        assert!(
            err.contains(refused),
            "the {tag} refusal names the member (stderr: {err})"
        );
        fs::remove_dir_all(&work).expect("the temp tree removes");
    }
}

/// A linked default body's receiver claims its `Self` variable's single
/// Raw word, so a consumer dispatch over a multi-word head keeps the
/// local one-word instantiation boundary's own refusal: the
/// consumer's omitting impl over a two-word linked struct refuses at
/// the lowering, naming the linked default body's receiver.
#[test]
fn a_wide_receiver_into_a_linked_default_body_refuses() {
    let (work, pkg_id) = shapes_fixtures("shapes-wide-default");
    let manifest = shapes_root_manifest(&pkg_id);
    let root = write_root(
        &work,
        &manifest,
        concat!(
            "import @shapes (pair, defaulted)\n",
            "\n",
            "impl defaulted for pair {\n",
            "}\n",
            "\n",
            "fn main() -> int {\n",
            "  const made = pair { a: 30, b: 12 }\n",
            "  made.answer()\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(!ok, "the wide receiver refuses");
    assert_eq!(out, "");
    assert!(
        err.contains("a linked default body's receiver"),
        "the refusal names the seam (stderr: {err})"
    );
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A producer's inherent impls cross the link seam (the type-keyed
/// receiver seam): the impl's member rows bind as foreign method-registry
/// rows keyed by the owner type's name, and the consumer's calls — the
/// value-receiver dot-call and the receiverless static spelling —
/// dispatch into the linked member bodies, a plain site, a tail site,
/// and the producer's own function riding through beside them.
#[test]
fn the_inherent_impls_cross_and_run() {
    let (work, pkg_id) = bespoke_pkg(
        "inherent-traits",
        concat!(
            "pub struct pair {\n",
            "  a: int,\n",
            "  b: int,\n",
            "}\n",
            "impl pair {\n",
            "  fn size(self) -> int {\n",
            "    self.a + self.b\n",
            "  }\n",
            "  fn make(a: int, b: int) -> pair {\n",
            "    pair { a: a, b: b }\n",
            "  }\n",
            "}\n",
            "pub fn base(p: pair) -> int {\n",
            "  p.size()\n",
            "}\n",
        ),
    );
    let (ok, out, err) = bespoke_run(
        &work,
        &pkg_id,
        concat!(
            "import @std/io\n",
            "import @lib (pair, base)\n",
            "\n",
            "fn weigh(p: pair) -> int {\n",
            "  p.size()\n",
            "}\n",
            "\n",
            "fn main() -> int {\n",
            "  const made = pair.make(30, 12)\n",
            "  println(\"${made.size()} ${weigh(made)} ${base(made)}\")\n",
            "  made.size() + weigh(made) + base(made)\n",
            "}\n",
        ),
    );
    assert!(ok, "the inherent-impl run exits 0 (stderr: {err})");
    // the static spelling's made pair, the value receiver's plain and
    // tail sites, and the producer's own function — 42 each, the run
    // echoing the total
    assert_eq!(out, "42 42 42\n126\n");
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// The root package's own impl over a linked type owns the members it
/// spells (the link layering's root-over-deps rule): a linked member of
/// the same name loses at the link pass with the shadowing warning
/// naming both sides, and the consumer's calls keep the root's bodies —
/// the linked row's own package keeping its behavior through its baked
/// direct calls, exactly like every inherent site.
#[test]
fn the_root_impl_shadows_the_linked_members() {
    let (work, pkg_id) = bespoke_pkg(
        "inherent-shadow",
        concat!(
            "pub struct pair {\n",
            "  a: int,\n",
            "  b: int,\n",
            "}\n",
            "impl pair {\n",
            "  fn size(self) -> int {\n",
            "    self.a + self.b\n",
            "  }\n",
            "  fn make(a: int, b: int) -> pair {\n",
            "    pair { a: a, b: b }\n",
            "  }\n",
            "}\n",
        ),
    );
    let (ok, out, err) = bespoke_run(
        &work,
        &pkg_id,
        concat!(
            "import @std/io\n",
            "import @lib (pair)\n",
            "\n",
            "impl pair {\n",
            "  fn size(self) -> int {\n",
            "    self.a * 100\n",
            "  }\n",
            "}\n",
            "\n",
            "fn main() -> int {\n",
            "  const made = pair.make(30, 12)\n",
            "  println(\"${made.size()}\")\n",
            "  made.size()\n",
            "}\n",
        ),
    );
    assert!(ok, "the shadowing run exits 0 (stderr: {err})");
    // the root's body answers both sites: 30 * 100
    assert_eq!(out, "3000\n3000\n");
    assert!(
        err.contains("DS2052"),
        "the shadowing warning names the class: {err}"
    );
    assert!(
        err.contains("the root package's impl member `size` of `pair` shadows"),
        "the warning names the winner and the loser: {err}"
    );
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A member the producer's walk refuses to carry keeps the consumer's
/// own call refusal (the type-keyed seam's own DS2044 story): a
/// defaulted member stages its literal at the linked module's own call
/// sites and a generic member's bodies are per-instantiation facts, so
/// neither row crosses — the value-receiver spelling and the static
/// spelling both refuse naming the missing member.
#[test]
fn the_unplanned_inherent_members_keep_their_call_refusals() {
    let (work, pkg_id) = bespoke_pkg(
        "inherent-refusals",
        concat!(
            "pub struct pair {\n",
            "  a: int,\n",
            "  b: int,\n",
            "}\n",
            "impl pair {\n",
            "  fn size(self) -> int {\n",
            "    self.a + self.b\n",
            "  }\n",
            "  fn defaulted(self, scale: int = 2) -> int {\n",
            "    (self.a + self.b) * scale\n",
            "  }\n",
            "  fn generic[t](self) -> int {\n",
            "    self.a + 1\n",
            "  }\n",
            "  fn make(a: int, b: int = 5) -> pair {\n",
            "    pair { a: a, b: b }\n",
            "  }\n",
            "}\n",
        ),
    );
    for (member, spelling) in [
        ("defaulted", "  made.defaulted()\n"),
        ("generic", "  made.generic()\n"),
        ("make", "  pair.make(3, 4)\n"),
    ] {
        let body = format!(
            "import @lib (pair)\n\nfn main() -> int {{\n  const made = pair {{ a: 1, b: 2 \
             }}\n{spelling}}}\n"
        );
        let (ok, out, err) = bespoke_run(&work, &pkg_id, &body);
        assert!(!ok, "the `{member}` call refuses cross-link");
        assert_eq!(out, "");
        let expected = if member == "make" {
            format!("type `pair` carries no member `{member}`")
        } else {
            format!("no name `{member}` behind the dot")
        };
        assert!(
            err.contains(&expected),
            "the `{member}` refusal names the missing member: {err}"
        );
    }
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A linked fn's signature binds over its own type without the type's
/// selection: the fact registers from the member's surface alone,
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

/// A linked enum binds through its tag rows: the fact registers the
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

/// A linked enum's tuple payload binds through its positional-word
/// fact: the consumer constructs by call form, matches positionally,
/// and hands values to the producer's own functions over the same
/// positional shape the richer fact encoding spells.
#[test]
fn linked_tuple_payloads_bind_and_run() {
    let (work, pkg_id) = shapes_fixtures("shapes-trip");
    let root_manifest = shapes_root_manifest(&pkg_id);
    let root = write_root(
        &work,
        &root_manifest,
        concat!(
            "import @std/io\n",
            "import @shapes (trip, one, trip_first, trip_make)\n",
            "\n",
            "fn main() -> int {\n",
            "  const own = trip_first(one(3, 4))\n",
            "  const made = trip_first(trip_make(9, 1))\n",
            "  const localmatch = match one(5, 6) {\n",
            "    one(a, _) => a,\n",
            "  }\n",
            "  const total = own + made + localmatch\n",
            "  println(\"${total}\")\n",
            "  total\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(ok, "the tuple-payload run exits 0 (stderr: {err})");
    assert_eq!(out, "17\n17\n");
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A linked linear struct binds with its move-discipline bit:
/// the consumer constructs the linked linear shape, moves it into the
/// producer's consuming function, and the linearity discipline holds
/// exactly like a local declaration — the fact carrying the bit.
#[test]
fn linked_linear_structs_bind_and_run() {
    let (work, pkg_id) = shapes_fixtures("shapes-linear");
    let root_manifest = shapes_root_manifest(&pkg_id);
    let root = write_root(
        &work,
        &root_manifest,
        concat!(
            "import @std/io\n",
            "import @shapes (guard, drain)\n",
            "\n",
            "fn main() -> int {\n",
            "  const total = drain(guard { n: 5 })\n",
            "  println(\"${total}\")\n",
            "  total\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(ok, "the linear-struct run exits 0 (stderr: {err})");
    assert_eq!(out, "5\n5\n");
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A lone tag binds through its unit-atom fact: the link walk
/// registers the tag def exactly like a local `tag` declaration, so
/// the consumer constructs, annotates, narrows, and matches over
/// unions of linked tags, and a linked fn's union-typed signature
/// spells — the values crossing both ways over the shared tag
/// mapping the producer's rows seed.
#[test]
fn linked_tag_facts_bind_and_run() {
    let (work, pkg_id) = shapes_fixtures("shapes-tags");
    let root_manifest = shapes_root_manifest(&pkg_id);
    let root = write_root(
        &work,
        &root_manifest,
        concat!(
            "import @std/io\n",
            "import @shapes (ping, pong, rally, point)\n",
            "\n",
            "fn main() -> int {\n",
            "  const own: ping | pong = ping\n",
            "  const ownhit = match own {\n",
            "    ping => 1,\n",
            "    pong => 2,\n",
            "  }\n",
            "  const producer = point(rally(5)) + point(rally(-1))\n",
            "  const localmatch = match rally(3) {\n",
            "    ping => 1,\n",
            "    pong => 2,\n",
            "  }\n",
            "  const crossed: ping | pong = rally(2)\n",
            "  var narrowed = 0\n",
            "  if crossed is ping {\n",
            "    narrowed = 4\n",
            "  }\n",
            "  const total = ownhit + producer + localmatch + narrowed\n",
            "  println(\"${total}\")\n",
            "  total\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(ok, "the tag-facts run exits 0 (stderr: {err})");
    // 1 (own construct + match) + 1 + 2 (the producer scores its
    // own values both ways) + 1 (the consumer match over a
    // producer value) + 4 (the is-narrowed member) = 9
    assert_eq!(out, "9\n9\n");
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// An empty linked enum binds its name and now solves it like the
/// local spelling (the empty case rows ride the fact, so the
/// name-solved head the local empty enum gained holds on the consumer
/// side too): uninhabited, no value crossing, an annotation boundary
/// naming it checking clean.
#[test]
fn an_empty_linked_enum_binds_its_name() {
    let (work, pkg_id) = shapes_fixtures("shapes-empty");
    let root_manifest = shapes_root_manifest(&pkg_id);
    let root = write_root(
        &work,
        &root_manifest,
        concat!(
            "import @shapes (hollow)\n",
            "\n",
            // the return annotation names the linked empty enum: the
            // name solves across the link, the diverging body never
            // hands it a value (a never-typed parameter refuses under
            // the bottom rule)
            "fn give_up() -> hollow { exit(1) }\n",
            "\n",
            "fn main() -> int {\n",
            "  7\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(ok, "the empty-enum name solves (stderr: {err})");
    assert_eq!(out, "7\n");
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// The type shapes the vocabulary cannot spell and the names a local
/// declaration or the consumer itself owns keep their refusals at the
/// same walk: a deinit-carrying struct, a generic enum, a generic
/// struct, and a struct with a field outside the word
/// vocabulary each carry no fact; a generic alias — a declaration
/// resolving through use-site arguments — carries none the same way;
/// a locally declared spelling of a linked type name refuses both at
/// the type's selection and at any linked signature naming it — the
/// local would mis-shape every spelling — and a case selection whose
/// enum refused the same way names its enum.
#[test]
fn the_uncarried_type_shapes_refuse() {
    let (work, pkg_id) = shapes_fixtures("shapes-tiers");
    let root_manifest = shapes_root_manifest(&pkg_id);
    for (tag, source, refused) in [
        (
            "deinit-struct",
            concat!(
                "import @shapes (owned)\n",
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
            "generic-alias",
            concat!(
                "import @shapes (wide)\n",
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

/// An inherent impl over a linked type binds and dispatches: the
/// minted def answers the layout seams, so the consumer's own impl
/// members register under the linked name and select against the
/// minted atom exactly like a local impl — the body compiling in this
/// package.
#[test]
fn an_impl_over_a_linked_type_dispatches() {
    let (work, pkg_id) = shapes_fixtures("shapes-impl");
    let root_manifest = shapes_root_manifest(&pkg_id);
    let root = write_root(
        &work,
        &root_manifest,
        concat!(
            "import @std/io\n",
            "import @shapes (cell, make)\n",
            "\n",
            "impl cell {\n",
            "  fn bump(self) -> int {\n",
            "    self.n + 1\n",
            "  }\n",
            "}\n",
            "\n",
            "fn main() -> int {\n",
            "  const total = make(4).bump()\n",
            "  println(\"${total}\")\n",
            "  total\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(ok, "the impl-over-linked-type run exits 0 (stderr: {err})");
    assert_eq!(out, "5\n5\n");
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// The richer alias and union words cross: a
/// linked alias whose body names a declared type of the same member
/// (`cells = list[cell]`), an alias chain expanding through another
/// alias (`chain = words`), a union-body alias (`either = int |
/// string`), and a linked struct whose field holds a partial
/// case-subset union (`partisan`'s `red | green`) — the consumer
/// annotates through the aliases, constructs and matches over the
/// subset, and passes its values through the producer's own
/// subset-taking functions, the signature naming the alias binding
/// the same way.
#[test]
fn the_alias_bodies_and_the_subset_union_cross() {
    let (work, pkg_id) = shapes_fixtures("shapes-richer");
    let root_manifest = shapes_root_manifest(&pkg_id);
    let root = write_root(
        &work,
        &root_manifest,
        concat!(
            "import @std/io\n",
            "import @shapes (cell, shade, red, green, make, chain, either, cells, head_of)\n",
            "import @shapes (partisan, mk_partisan, tally)\n",
            "\n",
            "fn main() -> int {\n",
            "  const made = mk_partisan(1)\n",
            "  var total = tally(made)\n",
            "  const own = partisan { pick: green }\n",
            "  const sub = match own.pick {\n",
            "    red => 10,\n",
            "    green => 20,\n",
            "  }\n",
            "  total = total + sub\n",
            "  const xs: cells = [make(3)]\n",
            "  total = total + head_of(xs)\n",
            "  const words: chain = [\"one\", \"two\"]\n",
            "  total = total + len(words)\n",
            "  const e: either = \"hi\"\n",
            "  if e is string { total = total + len(e) } else { total = total + 1000 }\n",
            "  println(\"${total}\")\n",
            "  total\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(ok, "the richer-words run exits 0 (stderr: {err})");
    // tally(made) = 6 (green), the subset match adds 20, head_of adds
    // 3, the chain list's len 2, and the string-narrowed either adds
    // 2 — 33 in all, the run echoing the entry's return after the
    // println
    assert_eq!(out, "33\n33\n");
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// Comptime reflection reads a linked type: the mint's
/// declaration products are the consuming compile's own, so `fields`
/// and `type_name` answer over a linked struct and a linked enum's
/// cases — the reflection projection consulting the foreign name
/// index the install filled.
#[test]
fn comptime_reflection_reads_a_linked_type() {
    let (work, pkg_id) = shapes_fixtures("shapes-reflect");
    let root_manifest = shapes_root_manifest(&pkg_id);
    let root = write_root(
        &work,
        &root_manifest,
        concat!(
            "import @std/io\n",
            "import @shapes (cell, shade)\n",
            "\n",
            "fn width(comptime t: type) -> int {\n",
            "  var n = 0\n",
            "  for f in fields(t) {\n",
            "    n += 1\n",
            "  }\n",
            "  n\n",
            "}\n",
            "\n",
            "fn label(comptime t: type) -> string {\n",
            "  type_name(t)\n",
            "}\n",
            "\n",
            "fn main() -> int {\n",
            "  const fields_of_cell = width(cell)\n",
            "  const cases_of_shade = width(shade)\n",
            "  const named = label(cell)\n",
            "  println(\"${named} ${fields_of_cell} ${cases_of_shade}\")\n",
            "  fields_of_cell + cases_of_shade\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds(&["run", entry]);
    assert!(ok, "the reflection run exits 0 (stderr: {err})");
    // cell carries seven fields; shade's entry carries one record per
    // case (red, green, blue)
    assert_eq!(out, "cell 7 3\n10\n");
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// The linked type's remaining receiver seam refuses at the check:
/// a bare linked type name as a dot-call receiver enters the type-keyed
/// tier, so a member the type does not carry answers the checker's own
/// refusal — the same wording the local spelling meets — instead of
/// sailing past the check into the lowering's catch-all.
#[test]
fn the_linked_type_receiver_seam_refuses_at_the_check() {
    let (work, pkg_id) = shapes_fixtures("shapes-seams");
    let root_manifest = shapes_root_manifest(&pkg_id);
    let (tag, source, refused) = (
        "linked-receiver-member",
        concat!(
            "import @shapes (cell)\n",
            "\n",
            "fn main() -> int {\n",
            "  cell.len()\n",
            "}\n",
        ),
        "type `cell` carries no member `len`",
    );
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

/// Every `.dsi` prefix artifact below the cache root's prefix tier: the
/// composed-prefix family under `dsi/` alone — each linked member's own
/// `.dsi` lands beside its dsmod under `ir/bytecode/` (its own family), not here.
fn dsi_artifacts(cache: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![cache.join("dsi")];
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
/// The triple is arbitrary data: install validates presence
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
/// the bundle's member identity — the composed fold over the ambient
/// core member and the migrated members' own source, the identity the
/// resolution installed and the walk validates.
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

    // the built artifact pins @std at the bundle's member identity — the
    // ambient's source hash differs from it, the members living in the
    // package's own source
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
            content_hash: ds_stdlib::std_member_identity(),
        }],
        "the writer records @std at the bundle's member identity"
    );

    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// The un-merge's reading switch: a graph declaring the @std dep
/// compiles over the shrunk ambient core (the demo residue's names gone
/// from its namespace — a bare use is an unknown name), the extern face
/// still binds through the linked surface, and the migrated shapes bind
/// through the widened link vocabulary — the map alias annotates and
/// constructs, the optional-field record constructs and reads through
/// the presence-tag word, the list-literal const embeds its elements
/// over the same `list_new`/`list_push` seam a local literal lowers
/// through, and the opaque bodiless struct binds its name in the type
/// namespace while every value use keeps the local spelling's own
/// contract refusal (no construction lowers, no witness exists).
#[test]
fn a_declared_std_dep_compiles_over_the_shrunk_core() {
    let work = temp_dir("std-migrated");
    let (_package, std_id, ver) = std_package_facts();
    let root = write_root(
        &work,
        &std_root_manifest(&ver, &std_id.to_string()),
        concat!(
            "import @std (to_upper)\n",
            "\n",
            "fn main() -> int {\n",
            "  println(to_upper(\"core\"))\n",
            "  0\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let cache = work.join("cache");

    let (ok, out, err) = ds_env(&cache, None, &["run", entry]);
    assert!(ok, "the shrunk-core run exits 0 (stderr: {err})");
    assert_eq!(
        out, "CORE\n0\n",
        "the linked extern face answers over the core"
    );

    // the migrated residue binds through the linked surface: the alias
    // annotates a map construction, the optional-field record constructs
    // and narrows, the list const embeds, and the opaque selection binds
    // (its name resolving is the pin — the pre-widening vocabulary
    // refused the selection itself at the walk)
    let residue = concat!(
        "import @std (row, config, response, urls)\n",
        "\n",
        "fn main() -> int {\n",
        "  var rows: row = map[string, string]()\n",
        "  rows.set(\"a\", \"1\")\n",
        "  const c = config { host: \"localhost\", port: 8080, retries: 3 }\n",
        "  const port = c.port\n",
        "  if port is none {\n",
        "    println(\"no port\")\n",
        "  } else {\n",
        "    println(\"port ${port}\")\n",
        "  }\n",
        "  for u in urls {\n",
        "    println(u)\n",
        "  }\n",
        "  println(\"${c.host} ${c.retries} ${rows.len()} ${urls.len()}\")\n",
        "  0\n",
        "}\n",
    );
    let root = write_root(
        &work,
        &std_root_manifest(&ver, &std_id.to_string()),
        residue,
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds_env(&cache, None, &["run", entry]);
    assert!(
        ok,
        "the migrated residue binds through the link (stderr: {err})"
    );
    let expected = concat!(
        "port 8080\n",
        "https://ds.dev\n",
        "https://example.org\n",
        "localhost 3 1 2\n",
        "0\n",
    );
    assert_eq!(out, expected, "the four migrated shapes answer end to end");

    // the warm run: the debug observable proves the warm serve — the
    // root and the @std member's dsmods read from disk — where
    // byte-identical stdout alone cannot (a fresh compile answers the
    // same bytes). The `.dsi` tiers have no line on this leg, and
    // honestly cannot: a fully-warm run serves the whole compile from
    // the run cache and runs no front end, so no path consults a
    // prefix (the composed prefix serves a dsmod miss's compile and
    // `ds info`'s file target, the dep `.dsi` only the `ds lsp` hover)
    let (ok, warm, warm_err) = ds_both_debug(&cache, &["run", entry]);
    assert!(ok, "the warm residue run exits 0 (stderr: {warm_err})");
    assert_eq!(warm, expected, "the warm run answers the same bytes");
    assert!(
        warm_err.contains("ds run cache: hit"),
        "the root dsmod serves from disk: {warm_err}"
    );
    assert_eq!(
        warm_err.matches("ds dep cache: hit").count(),
        1,
        "the @std member's dsmod serves from disk: {warm_err}"
    );
    assert_eq!(
        warm_err.matches("ds run cache: miss").count()
            + warm_err.matches("ds dep cache: miss").count(),
        0,
        "the warm run compiles nothing in-process: {warm_err}"
    );

    // the opaque shape's honest boundary: the name binds, but every
    // value use keeps the local spelling's own contract refusal —
    // the parameter spelling here, the construction spelling below
    let root = write_root(
        &work,
        &std_root_manifest(&ver, &std_id.to_string()),
        concat!(
            "import @std (response)\n",
            "\n",
            "fn pass(r: response) -> response {\n",
            "  r\n",
            "}\n",
            "\n",
            "fn main() -> int {\n",
            "  0\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds_env(&cache, None, &["run", entry]);
    assert!(!ok, "the opaque value use refuses");
    assert_eq!(out, "");
    assert!(
        err.contains("outside the M3 lowering contract"),
        "the refusal is the local spelling's own (stderr: {err})"
    );

    // the construction spelling splits the same way: the checker
    // accepts the part-less construction (resolve's zero-field rows
    // assert the walk clean), while the lowerer refuses it through
    // the lowerer arm — no construction lowers, no witness exists
    let root = write_root(
        &work,
        &std_root_manifest(&ver, &std_id.to_string()),
        concat!(
            "import @std (response)\n",
            "\n",
            "fn main() -> int {\n",
            "  const r = response { }\n",
            "  0\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds_env(&cache, None, &["run", entry]);
    assert!(!ok, "the opaque construction refuses at the lowering");
    assert_eq!(out, "");
    assert!(
        err.contains("outside the M3 lowering contract"),
        "the checker accepted the construction; the lowerer owns the refusal (stderr: {err})"
    );

    // the shrunk core really is the declaring graph's ambient: the demo
    // residue's names are unknown bare (their @std selections bind —
    // this test's residue leg — but a use without the import row never
    // resolves)
    let root = write_root(
        &work,
        &std_root_manifest(&ver, &std_id.to_string()),
        concat!(
            "fn take(c: config, r: response) -> int {\n",
            "  1\n",
            "}\n",
            "\n",
            "fn main() -> int {\n",
            "  0\n",
            "}\n",
        ),
    );
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let (ok, out, err) = ds_env(&cache, None, &["run", entry]);
    assert!(!ok, "the bare demo-residue names refuse over the core");
    assert_eq!(out, "");
    assert!(
        err.contains("unknown type `config`"),
        "the refusal names the migrated-out record: {err}"
    );
    assert!(
        err.contains("unknown type `response`"),
        "the refusal names the migrated-out opaque value: {err}"
    );

    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// The bare-specifier switch: a manifest-free script's implicit
/// resolution treats `@std` like a declaring graph — the default bundle
/// resolves ahead of the compile, the ambient the shrunk core, the
/// script's selections binding through the linked surface — so the
/// migrated shapes construct and read end to end over a plain script
/// (the alias annotating a map construction, the optional-field record
/// constructing and reading through the presence tag, the list const
/// embedding its elements), the opaque name binds while its value uses
/// keep the local spelling's lowering refusal, the @std path segments
/// are real member selectors (a segment the bundle does not carry
/// refusing), the warm run serves root and dep artifacts from the
/// cache byte-identically, and the built artifact records `@std` at the
/// bundle's member identity — the composed fold the declaring-graph
/// writer records too.
#[test]
fn a_plain_script_resolves_std_through_the_implicit_graph() {
    let work = temp_dir("std-script");
    let script_dir = work.join("script");
    fs::create_dir_all(&script_dir).expect("the script dir creates");
    let script = script_dir.join("app.ds");
    fs::write(
        &script,
        concat!(
            "import @std (row, config, response, urls)\n",
            "\n",
            "fn main() -> int {\n",
            "  var rows: row = map[string, string]()\n",
            "  rows.set(\"a\", \"1\")\n",
            "  const c = config { host: \"localhost\", port: 8080, retries: 3 }\n",
            "  const port = c.port\n",
            "  if port is none {\n",
            "    println(\"no port\")\n",
            "  } else {\n",
            "    println(\"port ${port}\")\n",
            "  }\n",
            "  for u in urls {\n",
            "    println(u)\n",
            "  }\n",
            "  println(\"${c.host} ${c.retries} ${rows.len()} ${urls.len()}\")\n",
            "  0\n",
            "}\n",
        ),
    )
    .expect("the script writes");
    let entry = script.to_str().expect("temp path is UTF-8");
    let cache = work.join("cache");

    // the migrated shapes bind through the linked surface over the
    // implicit resolution, answering exactly the declaring-graph bytes
    let (ok, out, err) = ds_env(&cache, None, &["run", entry]);
    assert!(
        ok,
        "the implicit-resolution script run exits 0 (stderr: {err})"
    );
    let expected = concat!(
        "port 8080\n",
        "https://ds.dev\n",
        "https://example.org\n",
        "localhost 3 1 2\n",
        "0\n",
    );
    assert_eq!(out, expected, "the four migrated shapes answer end to end");

    // the warm run serves the root and the @std dep member from the
    // cached artifacts, answering byte-identically
    let (ok, warm, err) = ds_env(&cache, None, &["run", entry]);
    assert!(ok, "the warm script run exits 0 (stderr: {err})");
    assert_eq!(warm, expected, "the warm run answers the same bytes");

    // the opaque shape's honest boundary: the name binds through the
    // link, but every value use keeps the local spelling's contract
    // refusal
    let opq = script_dir.join("opq.ds");
    fs::write(
        &opq,
        concat!(
            "import @std (response)\n",
            "\n",
            "fn pass(r: response) -> response {\n",
            "  r\n",
            "}\n",
            "\n",
            "fn main() -> int {\n",
            "  0\n",
            "}\n",
        ),
    )
    .expect("the opaque script writes");
    let (ok, out, err) = ds_env(&cache, None, &[
        "run",
        opq.to_str().expect("temp path is UTF-8"),
    ]);
    assert!(!ok, "the opaque value use refuses");
    assert_eq!(out, "");
    assert!(
        err.contains("outside the M3 lowering contract"),
        "the refusal is the local spelling's own (stderr: {err})"
    );

    // the @std path segments are real member selectors over the bundle:
    // a segment naming no member file refuses at the import
    let seg = script_dir.join("seg.ds");
    fs::write(
        &seg,
        "import @std/csv (split)\n\nfn main() -> int {\n  7\n}\n",
    )
    .expect("the segment script writes");
    let (ok, out, err) = ds_env(&cache, None, &[
        "run",
        seg.to_str().expect("temp path is UTF-8"),
    ]);
    assert!(!ok, "the phantom member segment refuses");
    assert_eq!(out, "");
    assert!(
        err.contains("`csv` names no member of the linked package"),
        "the refusal names the member segment: {err}"
    );

    // the built artifact records @std at the bundle's member identity —
    // the same composed fold the declaring-graph writer records
    let out_path = work.join("app.dsmod");
    let (ok, _, err) = ds_env(&cache, None, &[
        "build",
        entry,
        "--out",
        out_path.to_str().expect("path is UTF-8"),
    ]);
    assert!(ok, "the script build exits 0 (stderr: {err})");
    let bytes = fs::read(&out_path).expect("the artifact writes");
    let module = ds_mod::read(&bytes).expect("the built artifact reads");
    assert_eq!(
        module.deps(),
        &[Dep {
            name: "@std".to_owned(),
            content_hash: ds_stdlib::std_member_identity(),
        }],
        "the script writer records @std at the bundle's member identity"
    );

    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A graph that does not declare the @std dep keeps the merged-ambient
/// reading exactly as before the un-merge: the demo residue stays bare
/// (a row-typed binding constructing through the map face, the config
/// record), and an `@std` import's selections validate against the merged
/// namespace (the opaque response names a merged declaration) — while
/// the same source as a manifest-free script takes the implicit
/// resolution instead (the bare-specifier switch): the ambient is the
/// shrunk core, so the bare migrated names refuse as unknown types, the
/// honest merged-reading differential the script-mode e2e pins end to
/// end.
#[test]
fn an_undeclaring_graph_keeps_the_merged_ambient_reading() {
    let work = temp_dir("std-merged");
    let source = concat!(
        "import @std (response)\n",
        "\n",
        "fn main() -> int {\n",
        "  const cfg = config { host: \"localhost\", port: 8080, retries: 3 }\n",
        "  var rows: row = map[string, string]()\n",
        "  rows.set(\"a\", \"1\")\n",
        "  println(\"${cfg.host} ${cfg.retries} ${rows.len()}\")\n",
        "  0\n",
        "}\n",
    );
    let root = write_root(&work, "name = \"app\"\nversion = \"0.1.0\"\n", source);
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");
    let cache = work.join("cache");

    let (ok, out, err) = ds_env(&cache, None, &["run", entry]);
    assert!(ok, "the merged-reading package run exits 0 (stderr: {err})");
    let expected = "localhost 3 1\n0\n";
    assert_eq!(
        out, expected,
        "the demo residue answers bare through the merged ambient"
    );

    // the same source as a plain script (no manifest beside it) takes
    // the implicit resolution: the ambient is the shrunk core, so the
    // bare migrated names refuse as unknown types — the merged-reading
    // differential
    let script_dir = work.join("script");
    fs::create_dir_all(&script_dir).expect("the script dir creates");
    let script = script_dir.join("app.ds");
    fs::write(&script, source).expect("the script writes");
    let (ok, script_out, err) = ds_env(&cache, None, &["run", script.to_str().expect("UTF-8")]);
    assert!(!ok, "the script run refuses the bare migrated names");
    assert_eq!(script_out, "", "nothing executes");
    assert!(
        err.contains("unknown struct `config`"),
        "the refusal names the migrated-out record: {err}"
    );
    assert!(
        err.contains("unknown type `row`"),
        "the refusal names the migrated-out alias: {err}"
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
/// a @std-owned non-word type bare, and a member segment the bundle's
/// member set does not carry. The migrated demo residue binds through
/// the widened vocabulary now (the shrunk-core test's residue leg), so
/// no residue row refuses here anymore.
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

/// The re-export fixture's deepest member: exported functions, a literal
/// const, and a flat struct, the middle package re-exporting the first
/// of each kind and privately importing the other function.
const REEXPORT_DEEP_MEMBER: &str = concat!(
    "pub fn base() -> int {\n",
    "21\n",
    "}\n",
    "\n",
    "pub fn secret() -> int {\n",
    "7\n",
    "}\n",
    "\n",
    "pub const step = 2\n",
    "\n",
    "pub struct unit {\n",
    "  n: int,\n",
    "}\n",
);

/// The re-export fixture's middle member: `pub import` re-exports the
/// deepest package's callable, const, and struct onto its own export
/// surface (the transitive surface closure), while the private row
/// binding `secret` stays out.
const REEXPORT_LIB_MEMBER: &str = concat!(
    "pub import @deep (base, step, unit)\n",
    "import @deep (secret)\n",
    "\n",
    "pub fn twice(n: int) -> int {\n",
    "base() + n\n",
    "}\n",
    "\n",
    "fn main() -> int {\n",
    "twice(21)\n",
    "}\n",
);

/// The re-export fixture's root: imports the middle package only, the
/// re-exported callable, const, and struct answering through one row
/// beside the middle package's own function.
const REEXPORT_ROOT_SOURCE: &str = concat!(
    "import @std/io\n",
    "import @lib (twice, base, step, unit)\n",
    "\n",
    "fn main() -> int {\n",
    "println(\"hi\")\n",
    "const one = unit { n: 1 }\n",
    "base() + twice(21) + step + one.n\n",
    "}\n",
);

/// The re-export fixture packages saved into the test store, and the
/// identity the root pins the middle package at.
fn reexport_fixtures(tag: &str) -> (PathBuf, Hash128) {
    let work = temp_dir(tag);
    let deep = work.join("deep");
    write_pkg(&deep, &deep_manifest(), &[(
        "deep.ds",
        REEXPORT_DEEP_MEMBER,
    )]);
    let deep_id = bundle_identity(&deep_manifest(), &[("deep.ds", REEXPORT_DEEP_MEMBER)]);
    let lib_manifest = lib_manifest(deep_id);
    let lib = work.join("lib");
    write_pkg(&lib, &lib_manifest, &[("helper.ds", REEXPORT_LIB_MEMBER)]);
    for dir in [&deep, &lib] {
        let (ok, out, err) = ds(&["save", dir.to_str().expect("temp path is UTF-8")]);
        assert!(ok, "ds save {} exits 0 (stderr: {err})", dir.display());
        assert!(
            out.starts_with("saved "),
            "the install reports the identity it pinned (stdout: {out})"
        );
    }
    let lib_id = bundle_identity(&lib_manifest, &[("helper.ds", REEXPORT_LIB_MEMBER)]);
    (work, lib_id)
}

/// A `pub import` re-export crosses the middle package: the middle package's `pub import` row
/// joins the deepest member onto its own export surface, so the root —
/// importing the middle package alone — selects and calls the re-exported
/// member through the ordinary linked surface; the call's FuncRef
/// addresses the ORIGIN package, so the root's own dep list records
/// `@deep` beside its declared imports and the load walk installs it.
/// The middle package's private import of `secret` keeps today's skip:
/// the name never joins the surface, so a root selecting it refuses
/// naming the missing export.
#[test]
fn a_pub_import_reexport_crosses_the_middle_package() {
    let (work, lib_id) = reexport_fixtures("reexport");
    let root = write_root(&work, &root_manifest(lib_id, false), REEXPORT_ROOT_SOURCE);
    let entry = root.join("app.ds");
    let entry = entry.to_str().expect("temp path is UTF-8");

    let (ok, out, err) = ds(&["run", entry]);
    assert!(ok, "the re-exported call serves the run (stderr: {err})");
    assert_eq!(
        out, "hi\n66\n",
        "the re-exported callable, const, and struct answer beside the middle package's own"
    );

    // the built artifact's link table records the origin: the root's
    // FuncRef for the re-exported call addresses @deep's position, so
    // the dep list the walk validates names it beside the declared rows
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
                content_hash: Hash128::compute(REEXPORT_LIB_MEMBER.as_bytes()),
            },
            Dep {
                name: "@deep".to_owned(),
                content_hash: Hash128::compute(REEXPORT_DEEP_MEMBER.as_bytes()),
            },
        ],
        "the writer records the declared rows and the re-export's origin"
    );

    // the private import's binding never joins the surface: a root
    // selecting the privately-imported name refuses at the ordinary seam
    let refused = concat!(
        "import @std/io\n",
        "import @lib (secret)\n",
        "\n",
        "fn main() -> int {\n",
        "secret()\n",
        "}\n",
    );
    let root = write_root(&work, &root_manifest(lib_id, false), refused);
    let entry = root.join("app.ds");
    let (ok, out, err) = ds(&["run", entry.to_str().expect("temp path is UTF-8")]);
    assert!(!ok, "the privately-imported name refuses");
    assert_eq!(out, "");
    assert!(
        err.contains("`secret` names no definition the imported member exports"),
        "the refusal names the missing export (stderr: {err})"
    );

    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// The Named-word re-export fixture's deepest member: a flat struct and
/// a unit-cased enum whose facts cross onto the middle package's own
/// struct fields through its `pub import` rows, beside the private seed
/// struct the negative leg names in a field.
const NAMED_DEEP_MEMBER: &str = concat!(
    "pub struct cell {\n",
    "  n: int,\n",
    "  label: string,\n",
    "}\n",
    "pub enum shade {\n",
    "  red,\n",
    "  green,\n",
    "}\n",
    "pub struct seed {\n",
    "  k: int,\n",
    "}\n",
    "pub fn make(n: int) -> cell {\n",
    "  cell { n: n, label: \"made\" }\n",
    "}\n",
    "pub fn area(c: cell) -> int {\n",
    "  c.n * 2\n",
    "}\n",
    "pub fn score(s: shade) -> int {\n",
    "  match s {\n",
    "    red => 1,\n",
    "    green => 2,\n",
    "  }\n",
    "}\n",
);

/// The Named-word re-export fixture's middle member: `pub import`
/// re-exports the deepest struct, enum, and both its cases (the enum
/// word's resolution needs the case rows the surface carries), while
/// its own `fleet` names the re-exported types in every Named-word
/// spelling — a nested struct field, a container over it, an optional
/// over it, an enum-typed field — and `sealed` names the PRIVATELY
/// imported `seed`, the boundary the widening must keep refused.
const NAMED_LIB_MEMBER: &str = concat!(
    "import @deep (make, area, score, seed)\n",
    "pub import @deep (cell, shade, red, green)\n",
    "\n",
    "pub struct fleet {\n",
    "  head: cell,\n",
    "  tail: list[cell],\n",
    "  pick: cell?,\n",
    "  tint: shade,\n",
    "}\n",
    "\n",
    "pub struct sealed {\n",
    "  s: seed,\n",
    "}\n",
    "\n",
    "pub fn fleet_of(n: int) -> fleet {\n",
    "  fleet { head: make(n), tail: [make(1)], pick: none, tint: green }\n",
    "}\n",
    "\n",
    "pub fn fleet_weight(f: fleet) -> int {\n",
    "  const picked = match f.pick {\n",
    "    none => 0,\n",
    "    other => other.n,\n",
    "  }\n",
    "  area(f.head) + len(f.tail) + picked + score(f.tint)\n",
    "}\n",
);

/// The `pub import`-re-exported Named-word fixture packages saved into
/// the test store: the root pins the middle package alone, the deepest
/// package entering its own dep closure through the re-export's origin.
fn named_reexport_fixtures(tag: &str) -> (PathBuf, Hash128) {
    let work = temp_dir(tag);
    let deep = work.join("deep");
    write_pkg(&deep, &deep_manifest(), &[("deep.ds", NAMED_DEEP_MEMBER)]);
    let deep_id = bundle_identity(&deep_manifest(), &[("deep.ds", NAMED_DEEP_MEMBER)]);
    let lib = work.join("lib");
    let lib_manifest = lib_manifest(deep_id);
    write_pkg(&lib, &lib_manifest, &[("helper.ds", NAMED_LIB_MEMBER)]);
    for dir in [&deep, &lib] {
        let (ok, _, err) = ds(&["save", dir.to_str().expect("temp path is UTF-8")]);
        assert!(ok, "ds save {} exits 0 (stderr: {err})", dir.display());
    }
    let lib_id = bundle_identity(&lib_manifest, &[("helper.ds", NAMED_LIB_MEMBER)]);
    (work, lib_id)
}

/// A `pub import`-re-exported type carries as the Named word (todo item
/// 58's richer encoding): the middle package's `fleet` names the
/// re-exported `cell` in every word spelling — a nested struct field, a
/// `list[cell]`, a `cell?`, and the enum-typed field over the re-exported
/// `shade` — so the fact crosses, and the consumer's mint resolves the
/// word through its link table onto its own def for the re-exported
/// entry, forcing the entry's install forward past def order; the
/// consumer then constructs and moves whole `fleet` values, builds its
/// own `cell` literals beside a local struct naming the re-exported
/// type, and reads every field back through the middle package's own
/// functions.
#[test]
fn a_reexported_named_type_field_crosses_the_middle_package() {
    let (work, lib_id) = named_reexport_fixtures("reexport-named");
    let root_manifest = root_manifest(lib_id, false);
    let source = concat!(
        "import @std/io\n",
        "import @lib/helper (cell, shade, green, fleet, fleet_of, fleet_weight)\n",
        "\n",
        "struct wrap {\n",
        "  c: cell,\n",
        "}\n",
        "\n",
        "fn main() -> int {\n",
        "  const made = fleet_of(3)\n",
        "  var total = fleet_weight(made)\n",
        "  const own = fleet {\n",
        "    head: cell { n: 5, label: \"x\" },\n",
        "    tail: [cell { n: 1, label: \"a\" }],\n",
        "    pick: none,\n",
        "    tint: green,\n",
        "  }\n",
        "  total = total + fleet_weight(own)\n",
        "  const picked = match own.pick {\n",
        "    none => 0,\n",
        "    other => other.n,\n",
        "  }\n",
        "  const held = wrap { c: cell { n: 3, label: \"held\" } }\n",
        "  total = total + own.head.n + len(own.tail) + picked + held.c.n\n",
        "  println(\"${total}\")\n",
        "  total\n",
        "}\n",
    );
    let root = write_root(&work, &root_manifest, source);
    let entry = root.join("app.ds");
    let (ok, out, err) = ds(&["run", entry.to_str().expect("temp path is UTF-8")]);
    assert!(ok, "the re-exported Named-word run exits 0 (stderr: {err})");
    // fleet_weight(made) = area(make(3)) 6 + len([make(1)]) 1 + 0
    // + score(green) 2 = 9; fleet_weight(own) = area(5) 10 + len 1 + 0
    // + 2 = 13; the reads add 5 + 1 + 0 + the local wrap's 3 — 31 in
    // all, the run echoing the entry's return after the println
    assert_eq!(out, "31\n31\n");
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// The Named word's private-import boundary keeps its refusal: a field
/// naming a type the middle package imported WITHOUT a `pub import` row
/// carries no fact (the name joins no dependent's surface, so no link
/// table row would resolve the word), and the selection refuses at the
/// ordinary DS4021 seam.
#[test]
fn a_privately_imported_type_field_keeps_its_refusal() {
    let (work, lib_id) = named_reexport_fixtures("reexport-named-refused");
    let root_manifest = root_manifest(lib_id, false);
    let source = concat!(
        "import @lib/helper (sealed)\n",
        "\n",
        "fn main() -> int {\n",
        "7\n",
        "}\n",
    );
    let root = write_root(&work, &root_manifest, source);
    let entry = root.join("app.ds");
    let (ok, out, err) = ds(&["run", entry.to_str().expect("temp path is UTF-8")]);
    assert!(!ok, "the privately-imported type field refuses");
    assert_eq!(out, "");
    assert!(
        err.contains("layout and tag rows are the artifact round"),
        "the refusal names its tier (stderr: {err})"
    );
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

// ---- the cross-module tail transfer over the linked surface ------------

/// The linked tail-loop package's member: the mutual pair (ping/pong)
/// inside one linked module — the deepest loop shape the acyclic package
/// graph permits the cross-module mirror, since a cycle between two
/// packages refuses at resolve, so the pair lives in the linked module
/// and the root's tail transfer crosses the boundary into it. The bases
/// differ so a direction bug cannot pass as the right parity.
const TAIL_LOOP_MEMBER: &str = concat!(
    "pub fn ping(n: int) -> int {\n",
    "  if n == 0 { 4242 } else { pong(n - 1) }\n",
    "}\n",
    "\n",
    "fn pong(n: int) -> int {\n",
    "  if n == 0 { 2424 } else { ping(n - 1) }\n",
    "}\n",
);

/// Installs the tail-loop package and answers the root manifest pinning
/// it plus the app tree for one source spelling. The root tree is one
/// shared directory: a later call overwrites the earlier source, so a
/// test writes, runs, and builds one spelling before writing the next.
fn tail_loop_tree(work: &Path, source: &str) -> PathBuf {
    let pkg = work.join("tloop");
    let manifest = "name = \"@tloop\"\nversion = \"1.0.0\"\n".to_owned();
    let members = [("loop.ds", TAIL_LOOP_MEMBER)];
    write_pkg(&pkg, &manifest, &members);
    let (ok, _, err) = ds(&["save", pkg.to_str().expect("temp path is UTF-8")]);
    assert!(ok, "the loop package installs (stderr: {err})");
    let pkg_id = bundle_identity(&manifest, &members);
    let root_manifest = format!(
        "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@tloop = {{ ver = \"1.0.0\", hash = \
         \"blake3:{pkg_id}\" }}\n"
    );
    write_root(work, &root_manifest, source)
}

/// The dep-keyed FuncRefs one built function's pool carries: the uop form
/// each Callt/Call site addressed, with its dependency position.
fn dep_funcrefs(function: &ds_mod::Function) -> Vec<(String, u32)> {
    function
        .code()
        .iter()
        .enumerate()
        .filter_map(|(pc, &word)| {
            let pc = u32::try_from(pc).expect("code length fits u32");
            let ds_vm::abi::Inst::Uop(uop) = ds_vm::abi::Inst::decode(pc, word).ok()? else {
                return None;
            };
            let (ds_vm::abi::uop::Uop::Callt { callee: index, .. }
            | ds_vm::abi::uop::Uop::Call { callee: index, .. }) = uop
            else {
                return None;
            };
            match function.constants().get(usize::from(index.0)) {
                Some(ds_mod::Constant::FuncRef {
                    key: ds_mod::FuncRefKey::Dep(position),
                    ..
                }) => Some((repr_op(uop), *position)),
                _ => None,
            }
        })
        .collect()
}

/// One uop's alphabetic kind, the call-site rows' own label.
fn repr_op(uop: ds_vm::abi::uop::Uop) -> String {
    let repr = format!("{uop:?}");
    repr.chars().take_while(char::is_ascii_alphabetic).collect()
}

/// The cross-module tail transfer's acceptance over the linked
/// surface: the root's tail call into the linked package's function
/// lowers to CALLT — the frame-reuse transfer across the dsmod seam —
/// and the ten-million-hop mutual loop it enters runs to completion on
/// the constant stack the transfer leaves (a stacked 10^7-deep chain
/// would need ~60M stack words against the VM's 1Mi-word limit and traps
/// instead of exiting). The built artifact pins the lowering: main's
/// body carries a Callt whose pool FuncRef addresses the dep position,
/// never the plain Call the stacked reference lowers to.
#[test]
fn a_cross_module_tail_transfer_runs_a_ten_million_hop_loop_over_a_linked_package() {
    let (work, _lib_id) = fixtures("tailloop");
    let source = concat!(
        "import @tloop/loop (ping)\n",
        "\n",
        "fn main() -> int {\n",
        "  ping(10000000)\n",
        "}\n",
    );
    let root = tail_loop_tree(&work, source);
    let entry = root.join("app.ds");
    let (ok, out, err) = ds(&["run", entry.to_str().expect("temp path is UTF-8")]);
    assert!(ok, "the ten-million-hop run exits 0 (stderr: {err})");
    assert_eq!(
        out, "4242\n",
        "the even-depth base answers across the boundary"
    );

    let out_path = work.join("tail.dsmod");
    let (ok, _, err) = ds(&[
        "build",
        entry.to_str().expect("temp path is UTF-8"),
        "--out",
        out_path.to_str().expect("temp path is UTF-8"),
    ]);
    assert!(ok, "the tail variant builds (stderr: {err})");
    let module = ds_mod::read(&fs::read(&out_path).expect("the artifact writes"))
        .expect("the artifact reads");
    assert_eq!(
        module.deps(),
        &[Dep {
            name: "@tloop".to_owned(),
            content_hash: Hash128::compute(TAIL_LOOP_MEMBER.as_bytes()),
        }],
        "the writer records the linked loop package at its member identity"
    );
    let main = module
        .functions()
        .iter()
        .find(|f| f.name() == "main")
        .expect("main lowers");
    assert_eq!(
        dep_funcrefs(main),
        vec![("Callt".to_owned(), 1)],
        "the foreign tail lowers to the frame-reuse transfer over the first linked dep"
    );
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// The differential: at small depth the frame-reuse path answers
/// byte-identically to the stacked reference — the same source with the
/// call as an arithmetic operand (a plain CALL, every hop a pushed
/// frame) — both parities of the loop's base and the built artifacts
/// pinning the two uop shapes apart.
#[test]
fn the_cross_module_frame_reuse_answers_like_the_stacked_reference() {
    let (work, _lib_id) = fixtures("taildiff");
    for (depth, base) in [(1001_usize, "2424"), (1000, "4242")] {
        // one spelling at a time over the shared root tree: write, run,
        // and build the transfer source, then the stacked one
        let mut answers = Vec::new();
        let mut shapes = Vec::new();
        for (source, artifact) in [
            (
                format!("import @tloop/loop (ping)\n\nfn main() -> int {{\n  ping({depth})\n}}\n"),
                work.join(format!("tail-{depth}.dsmod")),
            ),
            (
                format!(
                    "import @tloop/loop (ping)\n\nfn main() -> int {{\n  ping({depth}) + 0\n}}\n"
                ),
                work.join(format!("stacked-{depth}.dsmod")),
            ),
        ] {
            let root = tail_loop_tree(&work, &source);
            let entry = root.join("app.ds");
            let (ok, out, err) = ds(&["run", entry.to_str().expect("temp path is UTF-8")]);
            assert!(ok, "the variant at depth {depth} exits 0 (stderr: {err})");
            answers.push(out);
            let (ok, _, err) = ds(&[
                "build",
                entry.to_str().expect("temp path is UTF-8"),
                "--out",
                artifact.to_str().expect("temp path is UTF-8"),
            ]);
            assert!(ok, "the variant builds (stderr: {err})");
            let module = ds_mod::read(&fs::read(&artifact).expect("the artifact writes")).unwrap();
            let main = module
                .functions()
                .iter()
                .find(|f| f.name() == "main")
                .expect("main lowers");
            shapes.push(dep_funcrefs(main));
        }
        assert_eq!(
            answers[0], answers[1],
            "frame reuse and stacking answer byte-identically at depth {depth}"
        );
        assert_eq!(answers[0], format!("{base}\n"));
        assert_eq!(
            shapes[0],
            vec![("Callt".to_owned(), 1)],
            "the tail variant transfers through the dep FuncRef"
        );
        assert_eq!(
            shapes[1],
            vec![("Call".to_owned(), 1)],
            "the stacked variant keeps the plain call"
        );
    }
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A fn word staged into a linked function's callable-defaulted parameter
/// splits exactly like a local site's: a non-capturing lambda mints the
/// count-free scalar FuncRef word, so the tail transfers through CALLT
/// across the boundary; a capturing lambda's closure word is a borrow the
/// callee reads during the call, so that tail lowers in place (CALL plus
/// return) — the count cannot ride a frame-reuse transfer.
#[test]
fn a_closure_word_into_a_linked_fn_parameter_keeps_the_in_place_tail() {
    let (work, _lib_id) = fixtures("tailclos");
    let pkg = work.join("fnlib");
    let manifest = "name = \"@fnlib\"\nversion = \"1.0.0\"\n".to_owned();
    let member = "pub fn apply(f: fn(int) -> int, n: int) -> int {\n  f(n)\n}\n";
    let members = [("fnlib.ds", member)];
    write_pkg(&pkg, &manifest, &members);
    let (ok, _, err) = ds(&["save", pkg.to_str().expect("temp path is UTF-8")]);
    assert!(ok, "the fn package installs (stderr: {err})");
    let pkg_id = bundle_identity(&manifest, &members);
    let root_manifest = format!(
        "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@fnlib = {{ ver = \"1.0.0\", hash = \
         \"blake3:{pkg_id}\" }}\n"
    );
    // one spelling at a time over the shared root tree, the differential's
    // own discipline: the scalar word transfers, the closure word stays
    for (tag, body, expected) in [
        ("scalar", "  apply((x: int) => x + 1, 41)\n", "Callt"),
        (
            "closure",
            "  const bump = 1\n  apply((x: int) => x + bump, 41)\n",
            "Call",
        ),
    ] {
        let source = format!("import @fnlib/fnlib (apply)\n\nfn main() -> int {{\n{body}}}\n");
        let root = write_root(&work, &root_manifest, &source);
        let entry = root.join("app.ds");
        let (ok, out, err) = ds(&["run", entry.to_str().expect("temp path is UTF-8")]);
        assert!(ok, "the {tag} run exits 0 (stderr: {err})");
        assert_eq!(out, "42\n");
        let out_path = work.join(format!("{tag}.dsmod"));
        let (ok, _, err) = ds(&[
            "build",
            entry.to_str().expect("temp path is UTF-8"),
            "--out",
            out_path.to_str().expect("temp path is UTF-8"),
        ]);
        assert!(ok, "the {tag} variant builds (stderr: {err})");
        let module = ds_mod::read(&fs::read(&out_path).expect("the artifact writes"))
            .expect("the artifact reads");
        let main = module
            .functions()
            .iter()
            .find(|f| f.name() == "main")
            .expect("main lowers");
        assert_eq!(
            dep_funcrefs(main),
            vec![(expected.to_owned(), 1)],
            "the {tag} fn word takes its own tail shape"
        );
    }
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// The counted-word member: a string param rides the cross-module tail
/// transfer — the Ref word's claim hands to the linked callee like a
/// local's, its count riding the frame-reuse transfer and the callee's
/// own exit dropping it — and the mutual pair then carries the word
/// down its local hops, so the base's content test reads the word after
/// it survived the seam and every slide below it. The bases answer
/// through the string's content (the local analogue of the claim,
/// ds-lower's deinit tail exit, stages the string the same way), so a
/// slide that lands the Ref word one slot off — or a transfer that
/// drops it early — answers the miss branch instead.
const LEND_MEMBER: &str = concat!(
    "pub fn ping(s: string, n: int) -> int {\n",
    "  if n == 0 {\n",
    "    if s == \"alpha\" { 42 } else { 24 }\n",
    "  } else {\n",
    "    pong(s, n - 1)\n",
    "  }\n",
    "}\n",
    "\n",
    "fn pong(s: string, n: int) -> int {\n",
    "  if n == 0 {\n",
    "    if s == \"alpha\" { 12 } else { 21 }\n",
    "  } else {\n",
    "    ping(s, n - 1)\n",
    "  }\n",
    "}\n",
    "\n",
);

/// Installs the counted-word package and answers the root manifest
/// pinning it plus the app tree for one source spelling, over the
/// shared root tree like the loop package's own helper.
fn lend_tree(work: &Path, source: &str) -> PathBuf {
    let pkg = work.join("lend");
    let manifest = "name = \"@lend\"\nversion = \"1.0.0\"\n".to_owned();
    let members = [("loop.ds", LEND_MEMBER)];
    write_pkg(&pkg, &manifest, &members);
    let (ok, _, err) = ds(&["save", pkg.to_str().expect("temp path is UTF-8")]);
    assert!(ok, "the counted-word package installs (stderr: {err})");
    let pkg_id = bundle_identity(&manifest, &members);
    let root_manifest = format!(
        "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@lend = {{ ver = \"1.0.0\", hash = \
         \"blake3:{pkg_id}\" }}\n"
    );
    write_root(work, &root_manifest, source)
}

/// The counted Ref word rides the cross-module tail transfer: the
/// root's tail call into the linked package's function hands a string
/// — its claim crossing the dsmod seam with the word, the callee owning
/// it like a local's — and the differential answers byte-identically to
/// the stacked reference at small depth both parities, the built
/// artifacts pinning Callt against Call over the dep-keyed FuncRef.
/// The content-decided bases keep the pin honest: a transfer that
/// lands the Ref word wrong answers the miss branch, not the base.
#[test]
fn a_cross_module_tail_transfer_hands_a_counted_ref_word() {
    let (work, _lib_id) = fixtures("tailref");
    for (depth, base) in [(1001_usize, "12"), (1000, "42")] {
        // one spelling at a time over the shared root tree, the loop
        // package differential's own discipline
        let mut answers = Vec::new();
        let mut shapes = Vec::new();
        for (index, source) in [
            format!(
                "import @lend/loop (ping)\n\nfn main() -> int {{\n  ping(\"alpha\", {depth})\n}}\n"
            ),
            format!(
                "import @lend/loop (ping)\n\nfn main() -> int {{\n  ping(\"alpha\", {depth}) + \
                 0\n}}\n"
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let root = lend_tree(&work, &source);
            let entry = root.join("app.ds");
            let (ok, out, err) = ds(&["run", entry.to_str().expect("temp path is UTF-8")]);
            assert!(
                ok,
                "the spelling {index} at depth {depth} exits 0 (stderr: {err})"
            );
            answers.push(out);
            let out_path = work.join(format!("lend-{index}-{depth}.dsmod"));
            let (ok, _, err) = ds(&[
                "build",
                entry.to_str().expect("temp path is UTF-8"),
                "--out",
                out_path.to_str().expect("temp path is UTF-8"),
            ]);
            assert!(ok, "the spelling {index} builds (stderr: {err})");
            let module = ds_mod::read(&fs::read(&out_path).expect("the artifact writes"))
                .expect("the artifact reads");
            assert_eq!(
                module.deps(),
                &[Dep {
                    name: "@lend".to_owned(),
                    content_hash: Hash128::compute(LEND_MEMBER.as_bytes()),
                }],
                "the writer records the counted-word package at its member identity"
            );
            let main = module
                .functions()
                .iter()
                .find(|f| f.name() == "main")
                .expect("main lowers");
            shapes.push(dep_funcrefs(main));
        }
        assert_eq!(
            answers[0], answers[1],
            "the handed Ref word answers byte-identically to the stacked reference at depth \
             {depth}"
        );
        assert_eq!(answers[0], format!("{base}\n"));
        assert_eq!(
            shapes[0],
            vec![("Callt".to_owned(), 1)],
            "the tail variant transfers the counted word through the dep FuncRef"
        );
        assert_eq!(
            shapes[1],
            vec![("Call".to_owned(), 1)],
            "the stacked variant keeps the plain call"
        );
    }
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// The path-shaped deep fixture: the same member the pinned deep
/// package serves.
const PATH_DEEP_MEMBER: &str = "pub fn base() -> int {\n7\n}\n";

/// A directory-shaped dep declaration serves a package run from source:
/// the root's `[deps.@lib]` names the sibling lib directory, resolution
/// reads the package from source (nothing installs it ahead of the run),
/// the content identity lands in the store as a cache fill, the dep's
/// own path dep nests transitively, and the built artifact's link table
/// records the dep at the member identity the directory's sources fold
/// — the same reproducibility story a registry pin buys.
#[test]
fn a_local_path_dep_serves_a_package_run() {
    let work = temp_dir("pathdep");
    let cache = work.join("cache");
    fs::create_dir_all(&cache).expect("the private cache creates");
    let deep = work.join("deep");
    write_pkg(&deep, "name = \"@deep\"\nversion = \"1.0.0\"\n", &[(
        "deep.ds",
        PATH_DEEP_MEMBER,
    )]);
    let lib_manifest = "name = \"@lib\"\nversion = \"1.2.0\"\n\n[deps.@deep]\npath = \"../deep\"\n";
    let lib_member = "import @deep (base)\n\npub fn twice(n: int) -> int {\nbase() + n\n}\n";
    let lib = work.join("lib");
    write_pkg(&lib, lib_manifest, &[("helper.ds", lib_member)]);
    let root = write_root(
        &work,
        "name = \"app\"\nversion = \"0.1.0\"\n\n[deps.@lib]\npath = \"../lib\"\n",
        ROOT_SOURCE,
    );
    let entry = root.join("app.ds");
    let entry_str = entry.to_str().expect("temp path is UTF-8");

    let (ok, out, err) = ds_env(&cache, None, &["run", entry_str]);
    assert!(ok, "the path-dep run exits 0 (stderr: {err})");
    assert_eq!(out, "hi\n28\n", "the call crosses two source-read packages");

    // the store holds the cache fills under the bundle identities the
    // canonical framing computes — the directory and its saved twin
    // share one identity by construction
    let deep_id = bundle_identity("name = \"@deep\"\nversion = \"1.0.0\"\n", &[(
        "deep.ds",
        PATH_DEEP_MEMBER,
    )]);
    let lib_id = bundle_identity(lib_manifest, &[("helper.ds", lib_member)]);
    let store = cache.join("pkg");
    assert!(
        store.join(format!("{deep_id}.pkg")).is_file(),
        "the deep directory's bundle cached under its identity"
    );
    assert!(
        store.join(format!("{lib_id}.pkg")).is_file(),
        "the lib directory's bundle cached under its identity"
    );

    // and the built artifact's link table records the deps at their
    // source-folded member identities
    let out_path = work.join("app.dsmod");
    let (ok, _, err) = ds_env(&cache, None, &[
        "build",
        entry_str,
        "--out",
        out_path.to_str().expect("UTF-8"),
    ]);
    assert!(ok, "the path-dep build exits 0 (stderr: {err})");
    let bytes = fs::read(&out_path).expect("the artifact writes");
    let module = ds_mod::read(&bytes).expect("the built artifact reads");
    assert!(
        module
            .deps()
            .iter()
            .any(|dep| dep.name == "@lib"
                && dep.content_hash == Hash128::compute(lib_member.as_bytes())),
        "the writer records the path dep at its member identity"
    );
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// One dep declared at both shapes in one manifest is parse-time drift:
/// the DS4014 names the shape mix, and the run refuses before any
/// resolution.
#[test]
fn a_dep_declared_at_both_shapes_refuses() {
    let work = temp_dir("pathdep-both");
    let cache = work.join("cache");
    fs::create_dir_all(&cache).expect("the private cache creates");
    let lib = work.join("lib");
    write_pkg(&lib, "name = \"@lib\"\nversion = \"1.0.0\"\n", &[(
        "helper.ds",
        "pub fn twice(n: int) -> int {\nn\n}\n",
    )]);
    let root = write_root(
        &work,
        concat!(
            "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n",
            "@lib = { ver = \"1.0.0\", hash = \"blake3:00000000000000000000000000000000\" }\n",
            "\n[deps.@lib]\npath = \"../lib\"\n",
        ),
        ROOT_SOURCE,
    );
    let (ok, _, err) = ds_env(&cache, None, &[
        "run",
        root.join("app.ds").to_str().expect("UTF-8"),
    ]);
    assert!(!ok, "the both-shape manifest refuses");
    assert!(
        err.contains("DS4014") && err.contains("both a registry pin and a local path"),
        "the drift names the shape mix: {err}"
    );
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// A transitive path dep naming the root (a sibling spelling of the
/// package under development) refuses at the cycle check, both shapes'
/// edges reading the one final graph.
#[test]
fn a_transitive_path_dep_cycle_refuses() {
    let work = temp_dir("pathdep-cycle");
    let cache = work.join("cache");
    fs::create_dir_all(&cache).expect("the private cache creates");
    let side = work.join("side");
    write_pkg(
        &side,
        "name = \"side\"\nversion = \"1.0.0\"\n\n[deps.app]\npath = \"../root\"\n",
        &[("side.ds", "pub fn member() -> int {\n1\n}\n")],
    );
    let root = write_root(
        &work,
        "name = \"app\"\nversion = \"0.1.0\"\n\n[deps.side]\npath = \"../side\"\n",
        "fn main() -> int {\n1\n}\n",
    );
    let (ok, _, err) = ds_env(&cache, None, &[
        "run",
        root.join("app.ds").to_str().expect("UTF-8"),
    ]);
    assert!(!ok, "the path cycle refuses");
    assert!(
        err.contains("cross-package dependency cycle"),
        "the refusal names the cycle: {err}"
    );
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

// ---- the link layering (the four layers over one trait's head) ----

/// The layering fixture's trait and type: one trait, one struct, one
/// head every impl of the face declares over.
const LAYER_CELL: &str = concat!(
    "pub trait greet {\n",
    "  fn greet(self) -> string\n",
    "}\n",
    "\n",
    "pub struct cell {\n",
    "  n: int\n",
    "}\n",
    "\n",
);

/// The layering root's manifest beside its entry: every layer face's
/// deps are sibling directories the resolution reads from source.
fn layer_root_manifest(deps: &[&str]) -> String {
    let mut manifest = "name = \"app\"\nversion = \"0.1.0\"\n".to_owned();
    for dep in deps {
        manifest.push_str(&format!(
            "\n[deps.{dep}]\npath = \"../{}\"\n",
            dep.trim_start_matches('@')
        ));
    }
    manifest
}

/// Writes one layering fixture package and answers its member text.
fn write_layer_pkg(work: &Path, name: &str, deps: &[&str], member: &str) {
    let mut manifest = format!("name = \"{name}\"\nversion = \"1.0.0\"\n");
    for dep in deps {
        manifest.push_str(&format!(
            "\n[deps.{dep}]\npath = \"../{}\"\n",
            dep.trim_start_matches('@')
        ));
    }
    let dir = work.join(name.trim_start_matches('@'));
    write_pkg(&dir, &manifest, &[(
        format!("{}.ds", name.trim_start_matches('@')).as_str(),
        member,
    )]);
}

/// The root package's own impl wins over every dependency's: the
/// linked impl of the dependency's own head shadowing is the warning
/// that names the winner, the loser, and the action, and the run's
/// dispatch answers the root's body.
#[test]
fn the_root_package_impl_shadows_a_dependency_impl_with_a_warning() {
    let work = temp_dir("layer-root");
    let cache = work.join("cache");
    fs::create_dir_all(&cache).expect("the private cache creates");
    write_layer_pkg(
        &work,
        "@lib",
        &[],
        &format!(
            "{LAYER_CELL}impl greet for cell {{\n  fn greet(self) -> string {{\n    \"lib \
             ${{self.n}}\"\n  }}\n}}\n"
        ),
    );
    let root = work.join("root");
    write_pkg(&root, &layer_root_manifest(&["@lib"]), &[(
        "app.ds",
        concat!(
            "import @lib (cell, greet)\n",
            "import @std/io (println)\n",
            "\n",
            "impl greet for cell {\n",
            "  fn greet(self) -> string {\n",
            "    \"root ${self.n}\"\n",
            "  }\n",
            "}\n",
            "\n",
            "fn main() -> () {\n",
            "  println(cell { n: 3 }.greet())\n",
            "}\n",
        ),
    )]);
    let (ok, out, err) = ds_env(&cache, None, &[
        "run",
        root.join("app.ds").to_str().expect("UTF-8"),
    ]);
    assert!(
        ok,
        "the shadowing resolves and the run stands (stderr: {err})"
    );
    assert_eq!(out, "root 3\n0\n", "the root's own impl takes the head");
    assert!(
        err.contains("DS2047"),
        "the shadowing warns, never silent (stderr: {err})"
    );
    assert!(
        err.contains("`@lib@1.0.0`'s impl is shadowed: the root package's own impl takes the head"),
        "the warning names the winner and the loser: {err}"
    );
    assert!(
        err.contains("drop @lib@1.0.0's impl to adopt it, or keep the current selection"),
        "the warning names the action: {err}"
    );
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// An origin impl (the package declaring the type or the trait) wins
/// over a third party's: the third party impls the origin's trait over
/// the origin's type through its own `pub import`, the link resolves
/// the overlap to the origin's row, and the warning names both
/// packages while the run answers the origin's body.
#[test]
fn an_origin_impl_shadows_a_third_party_impl() {
    let work = temp_dir("layer-origin");
    let cache = work.join("cache");
    fs::create_dir_all(&cache).expect("the private cache creates");
    write_layer_pkg(
        &work,
        "@base",
        &[],
        &format!(
            "{LAYER_CELL}impl greet for cell {{\n  fn greet(self) -> string {{\n    \"base \
             ${{self.n}}\"\n  }}\n}}\n"
        ),
    );
    write_layer_pkg(
        &work,
        "@ext",
        &["@base"],
        concat!(
            "pub import @base (cell, greet)\n",
            "\n",
            "impl greet for cell {\n",
            "  fn greet(self) -> string {\n",
            "    \"ext ${self.n}\"\n",
            "  }\n",
            "}\n",
        ),
    );
    let root = work.join("root");
    write_pkg(&root, &layer_root_manifest(&["@base", "@ext"]), &[(
        "app.ds",
        concat!(
            "import @base (cell, greet)\n",
            "import @ext\n",
            "import @std/io (println)\n",
            "\n",
            "fn main() -> () {\n",
            "  println(cell { n: 3 }.greet())\n",
            "}\n",
        ),
    )]);
    let (ok, out, err) = ds_env(&cache, None, &[
        "run",
        root.join("app.ds").to_str().expect("UTF-8"),
    ]);
    assert!(
        ok,
        "the origin's row wins and the run stands (stderr: {err})"
    );
    assert_eq!(
        out, "base 3\n0\n",
        "the origin impl takes the head over the third party's"
    );
    assert!(
        err.contains("`@ext@1.0.0`'s impl is shadowed: `@base@1.0.0`'s impl takes the head"),
        "the warning names the origin as the winner and the third party as the loser: {err}"
    );
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// Two third-party impls of one trait over one head refuse the link
/// naming both `name@version`: no package-identity layer separates
/// them, so the link takes neither and the run refuses.
#[test]
fn two_third_party_impls_refuse_the_link_naming_both() {
    let work = temp_dir("layer-tie");
    let cache = work.join("cache");
    fs::create_dir_all(&cache).expect("the private cache creates");
    // the origin declares the trait and the type but impls neither:
    // both extending packages are third parties
    write_layer_pkg(&work, "@base", &[], LAYER_CELL);
    for ext in ["@exta", "@extb"] {
        let body = format!(
            "pub import @base (cell, greet)\n\nimpl greet for cell {{\n  fn greet(self) -> string \
             {{\n    \"{ext} ${{self.n}}\"\n  }}\n}}\n"
        );
        write_layer_pkg(&work, ext, &["@base"], &body);
    }
    let root = work.join("root");
    write_pkg(
        &root,
        &layer_root_manifest(&["@base", "@exta", "@extb"]),
        &[(
            "app.ds",
            concat!(
                "import @base (cell, greet)\n",
                "import @exta\n",
                "import @extb\n",
                "import @std/io (println)\n",
                "\n",
                "fn main() -> () {\n",
                "  println(cell { n: 3 }.greet())\n",
                "}\n",
            ),
        )],
    );
    let (ok, _, err) = ds_env(&cache, None, &[
        "run",
        root.join("app.ds").to_str().expect("UTF-8"),
    ]);
    assert!(!ok, "two third-party impls refuse the link (stderr: {err})");
    assert!(
        err.contains("DS2048"),
        "the refusal is the layer conflict's own code: {err}"
    );
    assert!(
        err.contains("@exta@1.0.0") && err.contains("@extb@1.0.0"),
        "the refusal names both packages' name@version: {err}"
    );
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// The internal-exercise check: a shadowed impl whose declaring
/// package statically dispatches its own head (its linked artifact
/// baked a statically-resolved instance word naming it) refuses the
/// link naming the loser — the shadowed body would keep its own
/// behavior under the winner.
#[test]
fn an_exercised_impl_refuses_its_shadowing_naming_the_loser() {
    let work = temp_dir("layer-exercised");
    let cache = work.join("cache");
    fs::create_dir_all(&cache).expect("the private cache creates");
    let member = format!(
        "{LAYER_CELL}impl greet for cell {{\n  fn greet(self) -> string {{\n    \"lib \
         ${{self.n}}\"\n  }}\n}}\n\npub fn banner(c: cell) -> string {{\n  c.greet()\n}}\n"
    );
    write_layer_pkg(&work, "@lib", &[], &member);
    let root = work.join("root");
    write_pkg(&root, &layer_root_manifest(&["@lib"]), &[(
        "app.ds",
        concat!(
            "import @lib (banner, cell, greet)\n",
            "import @std/io (println)\n",
            "\n",
            "impl greet for cell {\n",
            "  fn greet(self) -> string {\n",
            "    \"root ${self.n}\"\n",
            "  }\n",
            "}\n",
            "\n",
            "fn main() -> () {\n",
            "  println(banner(cell { n: 3 }))\n",
            "  println(cell { n: 4 }.greet())\n",
            "}\n",
        ),
    )]);
    let (ok, _, err) = ds_env(&cache, None, &[
        "run",
        root.join("app.ds").to_str().expect("UTF-8"),
    ]);
    assert!(
        !ok,
        "the exercised impl refuses its shadowing (stderr: {err})"
    );
    assert!(
        err.contains("DS2049"),
        "the refusal is the internal-exercise check's own code: {err}"
    );
    assert!(
        err.contains("`@lib@1.0.0`'s impl of `greet` for `cell` cannot be shadowed"),
        "the refusal names the loser: {err}"
    );
    fs::remove_dir_all(&work).expect("the temp tree removes");
}

/// The internal-exercise exemption: the declaring package's only
/// dispatch is runtime-resolved (a generic body's evidence slot, never
/// entered at a static site), so no instance word is baked and the
/// shadowing stands as the warning it is — the link's solved tables
/// carry the winner alone.
#[test]
fn a_runtime_resolved_impl_shadows_with_a_warning() {
    let work = temp_dir("layer-runtime");
    let cache = work.join("cache");
    fs::create_dir_all(&cache).expect("the private cache creates");
    let member = format!(
        "{LAYER_CELL}impl greet for cell {{\n  fn greet(self) -> string {{\n    \"lib \
         ${{self.n}}\"\n  }}\n}}\n\npub fn shout[t: greet](v: t) -> string {{\n  v.greet()\n}}\n"
    );
    write_layer_pkg(&work, "@lib", &[], &member);
    let root = work.join("root");
    write_pkg(&root, &layer_root_manifest(&["@lib"]), &[(
        "app.ds",
        concat!(
            "import @lib (cell, greet)\n",
            "import @std/io (println)\n",
            "\n",
            "impl greet for cell {\n",
            "  fn greet(self) -> string {\n",
            "    \"root ${self.n}\"\n",
            "  }\n",
            "}\n",
            "\n",
            "fn main() -> () {\n",
            "  println(cell { n: 4 }.greet())\n",
            "}\n",
        ),
    )]);
    let (ok, out, err) = ds_env(&cache, None, &[
        "run",
        root.join("app.ds").to_str().expect("UTF-8"),
    ]);
    assert!(
        ok,
        "the runtime-resolved impl shadows with a warning (stderr: {err})"
    );
    assert_eq!(out, "root 4\n0\n", "the winner serves every dispatch");
    assert!(err.contains("DS2047"), "the shadowing warns: {err}");
    assert!(
        !err.contains("DS2049"),
        "no static instance word is baked: {err}"
    );
    fs::remove_dir_all(&work).expect("the temp tree removes");
}
