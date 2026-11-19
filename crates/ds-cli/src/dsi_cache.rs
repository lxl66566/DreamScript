//! The checked-prefix cache behind `ds run`/`ds build` source compiles:
//! a user package's own `.dsi`-shaped prefix persisted into `$DS_CACHE`
//! (the one cache-root authority the script-mode dsmod cache and the
//! package store share), keyed like that cache — the module name, the
//! source hash, the ambient identity, the resolved graph's dep
//! identities, and the build identity, each component framed — so two
//! builds whose front-end output can differ
//! never share an artifact. The artifact carries the whole program's
//! front-end half (the ambient member's and the user member's resolve,
//! declaration, and effect tables), so a hit serves both and the cold
//! compile checks only bodies; a miss falls back to the ambient's own
//! checked-in artifact plus the fresh compute exactly as before. The
//! cache never fails a run: a root that cannot resolve, an artifact
//! that cannot be read or validated, and a write that cannot land are
//! all silent falls back.

use std::{collections::BTreeMap, io::Write, path::PathBuf};

use ds_check::{Analyzed, DeclaredRows, Effects, WorldCaps};
use ds_diag::FileId;
use ds_dsi::{ServedPrefix, artifact_bytes, decode_dsi};
use ds_mod::Hash128;

/// Set to any non-empty value for one lookup-decision line per run on
/// stderr: `hit`, `miss`, or `off`.
const DEBUG_ENV: &str = "DS_PREFIX_CACHE_DEBUG";

/// The key fold's domain tag, bumped when the folded component set
/// changes: old artifacts must never answer a new fold.
const KEY_DOMAIN: &[u8] = b"ds-prefix-dsi-v2";

/// One compile's prefix-cache handle: the resolved root (absent when
/// the compile is uncached), the ambient identity the key folds, and
/// the resolved graph's dep identities (the empty list for a plain
/// script).
pub(crate) struct PrefixCache {
    root: Option<PathBuf>,
    ambient: Option<Hash128>,
    deps: BTreeMap<String, Hash128>,
    debug: bool,
}

impl PrefixCache {
    /// Resolves the cache for one plain-script compile: no dep identity
    /// beyond the ambient, so the key folds the empty dep list.
    pub(crate) fn discover(enabled: bool, no_ambient: bool) -> Self {
        Self::discover_linked(enabled, no_ambient, &BTreeMap::new())
    }

    /// Resolves the cache for one package-root compile. `enabled` is
    /// the shared `--no-cache` off switch; `no_ambient` picks the
    /// ambient identity the key folds — a compile without the ambient
    /// has no composed prefix to serve this round, so it runs uncached;
    /// `identities` are the resolved graph's dep content identities,
    /// which the source text alone does not determine — a linked
    /// package's surface feeds the front end, so the key folds them and
    /// a dep whose resolved content changed is another key.
    pub(crate) fn discover_linked(
        enabled: bool,
        no_ambient: bool,
        identities: &BTreeMap<String, Hash128>,
    ) -> Self {
        Self::discover_at_linked(
            enabled,
            no_ambient,
            enabled.then(ds_pkg::store::cache_root).flatten(),
            identities,
        )
    }

    /// [`Self::discover`](Self::discover) over a caller-named root:
    /// the unit tests' private temp directories, since every binary
    /// path resolves `$DS_CACHE` through
    /// [`Self::discover_linked`](Self::discover_linked).
    #[cfg(test)]
    pub(crate) fn discover_at(enabled: bool, no_ambient: bool, root: Option<PathBuf>) -> Self {
        Self::discover_at_linked(enabled, no_ambient, root, &BTreeMap::new())
    }

    /// [`Self::discover_linked`](Self::discover_linked) over a
    /// caller-named root and dep identities: the in-process gates'
    /// private temp directories.
    pub(crate) fn discover_at_linked(
        enabled: bool,
        no_ambient: bool,
        root: Option<PathBuf>,
        identities: &BTreeMap<String, Hash128>,
    ) -> Self {
        let debug = std::env::var_os(DEBUG_ENV).is_some_and(|value| !value.is_empty());
        let cache = Self {
            root: enabled.then_some(root).flatten().filter(|_| !no_ambient),
            ambient: (!no_ambient).then(|| Hash128::compute(ds_stdlib::AMBIENT_SOURCE.as_bytes())),
            deps: identities.clone(),
            debug,
        };
        if cache.debug && cache.root.is_none() {
            eprintln!("ds prefix cache: off");
        }
        cache
    }

    /// The persisted prefix for this source together with the
    /// registration the caller collected its files under; `None` is the
    /// miss every caller falls back from. An unreadable, stale, or
    /// corrupt artifact is a miss — never an error.
    pub(crate) fn lookup(
        &self,
        module_name: &str,
        source: &str,
        files: &[FileId],
    ) -> Option<ServedPrefix> {
        self.root.as_ref()?;
        let artifact = self.path(module_name, Hash128::compute(source.as_bytes()));
        let Ok(bytes) = std::fs::read(&artifact) else {
            return self.miss(&artifact);
        };
        match decode_dsi(&bytes, Hash128::compute(source.as_bytes()), files) {
            Ok(served) => {
                self.report("hit", &artifact);
                Some(served)
            },
            Err(_) => self.miss(&artifact),
        }
    }

    /// Persists one successfully compiled program's own prefix: the
    /// fresh declaration-phase snapshot (the artifact extracts the phase
    /// boundary, so a second declare pass runs here — the write happens
    /// once per changed source), the solved method rows, and the compile's
    /// own effect rows, keyed under this source. Every failure is silent
    /// — the compile already succeeded.
    pub(crate) fn store(
        &self,
        module_name: &str,
        source: &str,
        files: &[FileId],
        analyzed: &Analyzed,
        effects: &Effects,
        declared: &DeclaredRows,
        world: &WorldCaps,
    ) {
        if self.root.is_none() {
            return;
        }
        let source_hash = Hash128::compute(source.as_bytes());
        let artifact = self.path(module_name, source_hash);
        // content addressing makes an artifact that already validates
        // against this key the exact bytes this write would produce, so
        // an existing one is the write
        if let Ok(bytes) = std::fs::read(&artifact)
            && decode_dsi(&bytes, source_hash, files).is_ok()
        {
            return;
        }
        let Some(dir) = artifact.parent() else {
            return;
        };
        let (snapshot, snapshot_diags) = ds_check::decls::build(
            &analyzed.package,
            &analyzed.asts[..],
            &analyzed.resolutions[..],
        );
        if !snapshot_diags.is_empty() {
            return;
        }
        let method_rows = ds_check::effects::solved_method_rows(
            &analyzed.asts[..],
            &analyzed.package,
            &analyzed.resolutions[..],
            declared,
            world,
        );
        let payload = ds_dsi::extract(&snapshot, analyzed, effects, &method_rows);
        let bytes = artifact_bytes(source_hash, &payload);
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        let Ok(mut temp) = tempfile::NamedTempFile::new_in(dir) else {
            return;
        };
        if temp.write_all(&bytes).is_err() {
            return;
        }
        // temp-plus-rename in the artifact's own directory: concurrent
        // writers race to byte-identical content and a reader never sees
        // a partial file. Windows rename refuses to replace, so a lost
        // race removes and retries once.
        if let Err(persist) = temp.persist(&artifact)
            && persist.error.kind() == std::io::ErrorKind::AlreadyExists
        {
            let _ = std::fs::remove_file(&artifact);
            let temp = persist.file;
            let _ = temp.persist(&artifact);
        }
    }

    /// The artifact's path below the cache root: one flat file per key
    /// under the `.dsi` segment.
    fn path(&self, module_name: &str, source_hash: Hash128) -> PathBuf {
        self.root
            .as_ref()
            .expect("the root resolves on every path call")
            .join("dsi")
            .join(format!("{}.dsi", self.key(module_name, source_hash)))
    }

    /// The blake3-128 fold over every input that changes the produced
    /// prefix for one source text, each component framed so none can
    /// prefix another:
    /// - the domain tag above;
    /// - the module name: the artifact's spans and id spaces continue the registration a name
    ///   re-keys;
    /// - the source hash: the artifact's header pins it;
    /// - the ambient identity: the user member's ids continue the ambient's served spaces, so a
    ///   drifted ambient text is a different prefix;
    /// - each resolved dep's link name and content hash in ascending name order: the identities
    ///   package resolution pinned, which the source text alone does not determine — a linked
    ///   package's surface feeds the front end, so a dep whose resolved content changed is another
    ///   key and never serves a prefix linked against the old content. A plain script folds the
    ///   empty list;
    /// - the build identity: the same build digest the script-mode dsmod cache keys fold, so two
    ///   builds whose front-end output can differ never share an artifact.
    fn key(&self, module_name: &str, source_hash: Hash128) -> Hash128 {
        let mut hasher = blake3::Hasher::new();
        hasher.update(KEY_DOMAIN);
        crate::run_cache::fold_str(&mut hasher, module_name);
        hasher.update(source_hash.as_bytes());
        match self.ambient {
            Some(ambient) => {
                hasher.update(&[1]);
                hasher.update(ambient.as_bytes());
            },
            None => {
                hasher.update(&[0]);
            },
        }
        // the dep identities the resolved graph pinned, each name
        // framed around its hash so no two pairs splice, the same fold
        // the script-mode dsmod cache key applies
        hasher.update(&(self.deps.len() as u64).to_le_bytes());
        for (name, content) in &self.deps {
            crate::run_cache::fold_str(&mut hasher, name);
            hasher.update(content.as_bytes());
        }
        crate::run_cache::fold_str(&mut hasher, crate::run_cache::BUILD_ID);
        let digest = hasher.finalize();
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest.as_bytes()[..16]);
        Hash128::new(bytes)
    }

    /// Reports one lookup decision on stderr when the debug switch is
    /// set: the observable that proves which path a run took without
    /// timing it.
    fn report(&self, state: &str, artifact: &std::path::Path) {
        if self.debug {
            eprintln!("ds prefix cache: {state} {}", artifact.display());
        }
    }

    fn miss<T>(&self, artifact: &std::path::Path) -> Option<T> {
        self.report("miss", artifact);
        None
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::PrefixCache;

    /// The gate program the prefix serve must answer identically to the
    /// fresh compute: a derive (synthesized impls checked against the
    /// served tables), a generic trait impl spelling its trait
    /// reference's arguments (the rebuilt trait-argument record), a
    /// generic inherent impl with a declared bound (the rebuilt
    /// inherent-bounds record), a conditional dispatch through it, and
    /// calls into ambient functions and derives.
    const GATE: &str =
        "#[derive(show, hash)]\nstruct cell { n: int, label: string }\n\ntrait keyed[t: show] \
         {\nfn key(self, other: t) -> string { other.show() }\n}\n\nimpl[u: show] keyed[cell] for \
         list[u] {\nfn key(self, other: cell) -> string { other.show() }\n}\n\nimpl[u: show] \
         list[u] {\nfn shout(self) -> string {\nvar acc = \"!\"\nfor x in self {\nacc = \
         \"${acc}${x.show()}\"\n}\nacc\n}\n}\n\nfn main() -> () {\nconst c = cell { n: 3, label: \
         \"x\" }\nprintln(\"${[1, 2].shout()} ${[1, 2].key(c)} ${c.show()} ${max(1, 2)}\")\n}\n";

    /// One private cache root per tag: tests never share a root and never
    /// touch a real user cache directory.
    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ds-cli-dsi-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the private cache root creates");
        dir
    }

    /// One source file per tag: the key folds the module name (the file's
    /// stem) and the content.
    fn write_source(tag: &str, text: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ds-cli-dsi-src-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("the source directory creates");
        let path = dir.join(format!("{tag}.ds"));
        std::fs::write(&path, text).expect("the source writes");
        path
    }

    /// The one `.dsi` artifact under a root, recursively: the write
    /// observable.
    fn the_artifact(root: &Path) -> PathBuf {
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|ext| ext == "dsi") {
                    return path;
                }
            }
        }
        panic!("no persisted prefix under {}", root.display());
    }

    /// The ambient-registered file ids one run path compile registers:
    /// the ambient first, the user member after it, the host provision's
    /// die member last (the entry session's registration order).
    fn registered_ids() -> [ds_diag::FileId; 3] {
        let mut files = ds_diag::Files::new();
        let ambient = files.add_source(ds_stdlib::AMBIENT_FILE, ds_stdlib::AMBIENT_SOURCE);
        let user = files.add_source("gate.ds", GATE);
        let host = files.add_source(crate::source::HOST_FILE, crate::source::HOST_SOURCE);
        [ambient, user, host]
    }

    /// A cold compile persists one prefix under the private root, a warm
    /// compile serves it (the module bytes identical, the rebuilt records
    /// present), and the persisted bytes are the fresh generation's own —
    /// the artifact is a pure function of the sources and the compiler,
    /// the mint-determinism pin the whole cache rests on.
    #[test]
    fn a_cold_compile_persists_what_a_warm_compile_serves() {
        let root = temp_root("round-trip");
        let path = write_source("gate", GATE);
        let cache = PrefixCache::discover_at(true, false, Some(root.clone()));
        let cold = crate::source::compile(
            &path,
            "main",
            false,
            Some(&cache),
            crate::source::BundleAfter::Drop,
        )
        .expect("compiles");
        let artifact = the_artifact(&root);
        let persisted = std::fs::read(&artifact).expect("the persisted prefix reads");

        // The warm compile serves the persisted prefix and answers the cold
        // module byte for byte.
        let warm = crate::source::compile(
            &path,
            "main",
            false,
            Some(&cache),
            crate::source::BundleAfter::Drop,
        )
        .expect("compiles");
        assert_eq!(
            ds_mod::write(warm.lowered.module()),
            ds_mod::write(cold.lowered.module()),
            "the served prefix must answer the fresh module"
        );

        // The fresh generation over the same sources answers the persisted
        // bytes exactly (a store from any tier writes the same artifact):
        // the entry session's own registration — the ambient, the gate,
        // the host provision's die member — over the session's rows.
        let mut files = ds_diag::Files::new();
        let ambient = files.add_source(ds_stdlib::AMBIENT_FILE, ds_stdlib::AMBIENT_SOURCE);
        let user = files.add_source("gate.ds", GATE);
        let host = files.add_source(crate::source::HOST_FILE, crate::source::HOST_SOURCE);
        let fresh_tree = ds_parse::parse(&files, ambient).expect("the ambient parses");
        let user_ast = ds_parse::parse_script(&files, user).expect("the gate parses");
        let host_ast = ds_parse::parse(&files, host).expect("the host member parses");
        let fresh_run = ds_meta::analyze_owned(
            &files,
            vec![
                std::sync::Arc::new(fresh_tree),
                std::sync::Arc::new(user_ast),
                std::sync::Arc::new(host_ast),
            ],
            &[false, false, false],
        );
        assert!(
            fresh_run.checked.diags.is_empty(),
            "the gate program checks clean: {:?}",
            fresh_run.checked.diags
        );
        let rows = crate::source::session_rows();
        let caps = ds_stdlib::world_caps();
        let effects = ds_check::effects::effects_of_resolved(
            &fresh_run.asts[..],
            &fresh_run.package,
            &fresh_run.resolutions[..],
            &fresh_run.checked.hole_show_dispatches,
            rows,
            caps,
        );
        let (snapshot, snapshot_diags) = ds_check::decls::build(
            &fresh_run.package,
            &fresh_run.asts[..],
            &fresh_run.resolutions[..],
        );
        assert!(snapshot_diags.is_empty(), "the gate program declares clean");
        let method_rows = ds_check::effects::solved_method_rows(
            &fresh_run.asts[..],
            &fresh_run.package,
            &fresh_run.resolutions[..],
            rows,
            caps,
        );
        let payload = ds_dsi::extract(&snapshot, &fresh_run, &effects, &method_rows);
        let generated = ds_dsi::artifact_bytes(ds_mod::Hash128::compute(GATE.as_bytes()), &payload);
        assert_eq!(
            persisted, generated,
            "the persisted prefix must be the fresh generation's bytes"
        );
    }

    /// The served prefix's analyze answers the fresh analyze over the
    /// same program: identical trees, namespace, resolutions, universe
    /// mint sequence, plain and signature tables, instance rows, solved
    /// span-keyed records (raw handles, both runs minting identical
    /// sequences), checked facts, and effect rows.
    #[test]
    fn the_served_prefix_answers_a_fresh_analyze() {
        let root = temp_root("equivalence");
        let path = write_source("gate", GATE);
        let cache = PrefixCache::discover_at(true, false, Some(root));
        crate::source::compile(
            &path,
            "main",
            false,
            Some(&cache),
            crate::source::BundleAfter::Drop,
        )
        .expect("compiles");

        let [ambient, user, host] = registered_ids();
        let served = cache
            .lookup("gate", GATE, &[ambient, user, host])
            .expect("the persisted prefix serves");
        let mut files = ds_diag::Files::new();
        let ambient = files.add_source(ds_stdlib::AMBIENT_FILE, ds_stdlib::AMBIENT_SOURCE);
        let user = files.add_source("gate.ds", GATE);
        let host = files.add_source(crate::source::HOST_FILE, crate::source::HOST_SOURCE);
        let served_tree = ds_stdlib::ambient_ast(ambient).expect("the AST blob verifies");
        let user_ast = ds_parse::parse_script(&files, user).expect("the gate parses");
        let host_ast = ds_parse::parse(&files, host).expect("the host member parses");
        let served_run = ds_meta::analyze_served(
            &files,
            vec![
                served_tree,
                std::sync::Arc::new(user_ast.clone()),
                std::sync::Arc::new(host_ast.clone()),
            ],
            &[true, false, false],
            served.package,
            served.resolutions,
            served.decls,
        );
        assert!(
            served_run.checked.diags.is_empty(),
            "the served run checks clean: {:?}",
            served_run.checked.diags
        );
        // the rebuild must have answered the program's own records
        assert!(
            !served_run.decls.trait_impl_args.is_empty(),
            "the gate program records trait arguments"
        );
        assert!(
            !served_run.decls.inherent_impl_bounds.is_empty(),
            "the gate program records inherent bounds"
        );

        let fresh_tree = ds_parse::parse(&files, ambient).expect("the ambient parses");
        let fresh_run = ds_meta::analyze_owned(
            &files,
            vec![
                std::sync::Arc::new(fresh_tree),
                std::sync::Arc::new(user_ast),
                std::sync::Arc::new(host_ast),
            ],
            &[false, false, false],
        );
        assert!(
            fresh_run.checked.diags.is_empty(),
            "the fresh run checks clean: {:?}",
            fresh_run.checked.diags
        );

        for (served_tree, fresh_tree) in served_run.asts.iter().zip(fresh_run.asts.iter()) {
            assert_eq!(
                postcard::to_allocvec(served_tree).expect("serializes"),
                postcard::to_allocvec(fresh_tree).expect("serializes"),
                "the trees must answer identically"
            );
        }
        assert_eq!(
            served_run.package, fresh_run.package,
            "the namespaces match"
        );
        assert_eq!(
            served_run.resolutions[..],
            fresh_run.resolutions[..],
            "the resolutions match"
        );
        assert_eq!(
            ds_dsi::gates::mint_facts(&served_run.decls.universe),
            ds_dsi::gates::mint_facts(&fresh_run.decls.universe),
            "the universe mint sequences match"
        );
        assert_eq!(
            ds_dsi::gates::plain_tables(&served_run),
            ds_dsi::gates::plain_tables(&fresh_run),
            "the plain declaration tables match"
        );
        assert_eq!(
            ds_dsi::gates::signature_tables(&served_run),
            ds_dsi::gates::signature_tables(&fresh_run),
            "the signature tables match"
        );
        assert_eq!(
            ds_dsi::gates::instance_rows(&served_run),
            ds_dsi::gates::instance_rows(&fresh_run),
            "the instance rows match"
        );
        assert_eq!(
            ds_dsi::gates::const_rows(&served_run),
            ds_dsi::gates::const_rows(&fresh_run),
            "the top-level binding rows match"
        );
        assert_eq!(
            ds_dsi::gates::trait_facts(&served_run),
            ds_dsi::gates::trait_facts(&fresh_run),
            "the trait registries match"
        );
        assert_eq!(
            served_run.decls.trait_impl_args, fresh_run.decls.trait_impl_args,
            "the rebuilt trait arguments match the fresh records"
        );
        assert_eq!(
            served_run.decls.inherent_impl_bounds, fresh_run.decls.inherent_impl_bounds,
            "the rebuilt inherent bounds match the fresh records"
        );
        assert_eq!(
            served_run.decls.impl_fn_rets, fresh_run.decls.impl_fn_rets,
            "the impl-member returns match"
        );
        assert_eq!(
            served_run.checked.def_types, fresh_run.checked.def_types,
            "the top-level types match"
        );
        assert_eq!(
            ds_dsi::gates::rendered_exprs(&served_run),
            ds_dsi::gates::rendered_exprs(&fresh_run),
            "the checked expression facts match"
        );
        let served_effects = ds_check::effects::effects_of_resolved_served(
            &served_run.asts[3..],
            &served_run.package,
            &served_run.resolutions[3..],
            &served_run.checked.hole_show_dispatches,
            &served.effect_defs,
            &served.effect_methods,
            crate::source::session_rows(),
            ds_stdlib::world_caps(),
        );
        let fresh_effects = ds_check::effects::effects_of_resolved(
            &fresh_run.asts[..],
            &fresh_run.package,
            &fresh_run.resolutions[..],
            &fresh_run.checked.hole_show_dispatches,
            crate::source::session_rows(),
            ds_stdlib::world_caps(),
        );
        assert_eq!(
            served_effects.defs(),
            fresh_effects.defs(),
            "the served effect rows match the fresh pass"
        );
    }

    /// Two independent cold compiles persist byte-identical prefixes —
    /// the ids the artifact records are a pure function of the sources
    /// and the compiler, so one run's artifact serves the next.
    #[test]
    fn two_cold_compiles_persist_identical_bytes() {
        let path = write_source("gate", GATE);
        let roots = [temp_root("determinism-a"), temp_root("determinism-b")];
        let mut persisted = Vec::new();
        for root in &roots {
            let cache = PrefixCache::discover_at(true, false, Some(root.clone()));
            crate::source::compile(
                &path,
                "main",
                false,
                Some(&cache),
                crate::source::BundleAfter::Drop,
            )
            .expect("compiles");
            persisted.push(std::fs::read(the_artifact(root)).expect("the artifact reads"));
        }
        assert_eq!(
            persisted[0], persisted[1],
            "two cold compiles must persist identical prefixes"
        );
    }

    /// The miss matrix: a drifted source hash, a foreign registration,
    /// and corrupt bytes are all misses — the cache never answers a
    /// program it did not persist for exactly this registration.
    #[test]
    fn every_drift_is_a_miss() {
        let root = temp_root("misses");
        let path = write_source("gate", GATE);
        let cache = PrefixCache::discover_at(true, false, Some(root.clone()));
        crate::source::compile(
            &path,
            "main",
            false,
            Some(&cache),
            crate::source::BundleAfter::Drop,
        )
        .expect("compiles");
        let artifact = the_artifact(&root);
        let [ambient, user, host] = registered_ids();

        // an edited source is another key
        assert!(
            cache
                .lookup("gate", "fn main() -> () { }\n", &[ambient, user, host])
                .is_none(),
            "an edited source misses"
        );
        // a foreign registration (a third file registered before the
        // member) is a miss
        let mut files = ds_diag::Files::new();
        let foreign = files.add_source("other.ds", "fn f() -> () { }\n");
        let _ = files.add_source(ds_stdlib::AMBIENT_FILE, ds_stdlib::AMBIENT_SOURCE);
        let _ = files.add_source("gate.ds", GATE);
        assert!(
            cache.lookup("gate", GATE, &[foreign]).is_none(),
            "a foreign registration misses"
        );
        // corrupt bytes are a miss
        let mut corrupt = std::fs::read(&artifact).expect("the artifact reads");
        let last = corrupt.len() - 1;
        corrupt[last] = corrupt[last].wrapping_add(1);
        std::fs::write(&artifact, &corrupt).expect("the corrupt bytes write");
        assert!(
            cache.lookup("gate", GATE, &[ambient, user, host]).is_none(),
            "corrupt bytes miss"
        );
    }

    /// The dep-identity fold re-keys: one dep's changed content moves
    /// the key, the link name frames its hash, and a plain script's
    /// empty-list fold differs from every resolved graph's — a prefix
    /// linked against a dep's old content never serves the new
    /// resolution.
    #[test]
    fn the_dep_identity_fold_rekeys() {
        use std::collections::BTreeMap;

        use ds_mod::Hash128 as H;

        let source = H::compute(b"fn main() -> () { }");
        let plain = PrefixCache::discover(false, false);
        let at = |identity: &[u8]| {
            PrefixCache::discover_at_linked(
                false,
                false,
                None,
                &BTreeMap::from([("@lib".to_owned(), H::compute(identity))]),
            )
        };
        assert_ne!(
            at(b"one").key("app", source),
            at(b"two").key("app", source),
            "a dep's changed content is another key"
        );
        assert_ne!(
            plain.key("app", source),
            at(b"one").key("app", source),
            "a resolved identity never serves a plain script's key"
        );
        let named = PrefixCache::discover_at_linked(
            false,
            false,
            None,
            &BTreeMap::from([
                ("@a".to_owned(), H::compute(b"one")),
                ("@b".to_owned(), H::compute(b"two")),
            ]),
        );
        let swapped = PrefixCache::discover_at_linked(
            false,
            false,
            None,
            &BTreeMap::from([
                ("@a".to_owned(), H::compute(b"two")),
                ("@b".to_owned(), H::compute(b"one")),
            ]),
        );
        assert_ne!(
            named.key("app", source),
            swapped.key("app", source),
            "the link name frames its hash"
        );
    }
}
