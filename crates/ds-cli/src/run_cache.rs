//! The script-mode dsmod caches behind `ds run <file.ds>`: the CLI-owned
//! key folds, the mmap hit paths, and the temp-plus-rename writes, over
//! the `$DS_CACHE` root ds-pkg resolves (the one authority the package
//! store shares). Three artifact families share the root: the package
//! root's own module, each linked dep member's compiled module, and the
//! same member's checked prefix — its `.dsi`, written beside the dsmod
//! under the same key so `ds info` and the LSP hover serve the surface a
//! run of the same graph cached — the dep families keyed on the member's
//! own link name and content identity over the same graph fold, so a
//! warm package run serves the walk's dep demands from disk and compiles
//! nothing in-process. None of the caches ever fails a run: a root that
//! cannot resolve, an artifact that cannot be read or validated, and a
//! write that cannot land all silently fall back to the in-process
//! source pipeline.

use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
};

use ds_mod::{FORMAT_VERSION, Hash128, LoadError, MappedModule, Module, cache_path, read_mmap};

/// The segment below `$DS_CACHE/ir` holding script artifacts. The source
/// pipeline emits portable bytecode-class modules, so the segment names
/// the class; a tier that emits target-specific code takes its own
/// triple segment without disturbing this one.
const TARGET_SEGMENT: &str = "bytecode";

/// Maps one cache artifact read-only, the `read_mmap` contract's only
/// caller-side guarantee held here once: an artifact lands by
/// temp-plus-rename under its content-addressed name and nothing rewrites
/// it after, so the mapping stays valid for the parse and the module's
/// serve.
fn read_artifact(path: &Path) -> Result<MappedModule, LoadError> {
    // SAFETY: the artifact is content-addressed and written by
    // temp-plus-rename; no writer truncates or rewrites it in place.
    unsafe { read_mmap(path) }
}

/// Set to any non-empty value for one lookup-decision line per run on
/// stderr: `hit`, `miss`, `bypassed`, or `off`.
const DEBUG_ENV: &str = "DS_RUN_CACHE_DEBUG";

/// The fold's domain tag, bumped when the folded component set changes:
/// old artifacts must never answer a new fold.
const KEY_DOMAIN: &[u8] = b"ds-run-cache-v2";

/// The dep-member fold's domain tag: a linked package's compiled module
/// is its own artifact family, never the root's — the root compile and
/// a dep compile of the same name and content answer different modules
/// (the entry session's host member and the root's entry folding ride
/// the root compile alone), so the two folds share components but never
/// a domain.
const DEP_KEY_DOMAIN: &[u8] = b"ds-dep-cache-v2";

/// The artifact-pin list's marker inside a fold: the dep list and the
/// artifact list share the name-then-hash pair framing, so the marker
/// keeps one list's tail from splicing into the other's head.
pub(crate) const ARTIFACT_LIST_TAG: &[u8] = b"ds-artifact-pins-v1";

/// The build identity folded into the key: build.rs digests the source
/// trees of every workspace crate the source pipeline runs through plus
/// the workspace lock, so any rebuild whose bytecode output can differ
/// re-keys — another worktree, a rebuilt lowerer, an A/B pair never
/// share artifacts through a shared cache root. A packaged build
/// without those sibling sources falls back to the package version,
/// which at least follows releases.
pub(crate) const BUILD_ID: &str =
    if env!("DS_BUILD_ID").is_empty() { env!("CARGO_PKG_VERSION") } else { env!("DS_BUILD_ID") };

/// The ambient identity a cache key folds over one graph's identities:
/// the shrunk core's hash when the graph resolved the reserved `@std`
/// package and the compile is package-shaped (the un-merge's ambient
/// shape — those compiles register the core alone), the session
/// composition's hash for an entry script session over a std-declaring
/// graph (the core with the @std bodied members composed back), and the
/// merged source's hash otherwise — `script` follows the same session
/// rule the compile's ambient registration picks. The deps map and the
/// session shape alone decide, so every discover path derives the same
/// identity.
pub(crate) fn ambient_identity(identities: &BTreeMap<String, Hash128>, script: bool) -> Hash128 {
    let std_declared = identities.contains_key(ds_stdlib::STD_PACKAGE_NAME);
    Hash128::compute(
        match (std_declared, script) {
            (true, false) => ds_stdlib::AMBIENT_CORE_SOURCE,
            (true, true) => ds_stdlib::AMBIENT_SESSION_SOURCE,
            (false, _) => ds_stdlib::AMBIENT_SOURCE,
        }
        .as_bytes(),
    )
}

/// One run's cache handle: the resolved root (absent when the run is
/// uncached), the ambient identity the key folds, the resolved graph's
/// dep identities (the empty list for a plain script), and its native
/// artifact pins (the empty list for a plain script).
pub(crate) struct RunCache {
    root: Option<PathBuf>,
    ambient: Option<Hash128>,
    deps: BTreeMap<String, Hash128>,
    artifacts: BTreeMap<String, Hash128>,
    debug: bool,
}

impl RunCache {
    /// Resolves the cache for one plain-script run: no dep identity
    /// beyond the folded components — every name but the ambient
    /// records the zero marker — so the key folds the empty dep list.
    pub(crate) fn discover(enabled: bool, no_ambient: bool) -> Self {
        Self::discover_linked(
            enabled,
            no_ambient,
            &BTreeMap::new(),
            &BTreeMap::new(),
            true,
        )
    }

    /// Resolves the cache for one package run. `enabled` is the
    /// `--no-cache` off switch; `no_ambient` picks the ambient identity
    /// the key folds (the ambient's impl bodies lower into the script
    /// module, so the ambient text is a bytecode input — a package
    /// compile over a graph that resolved `@std` lowers the shrunk
    /// core, an entry script session the session composition, so
    /// `script` follows the shape the session picks); `identities` are
    /// the resolved graph's dep content identities, which the source
    /// text alone does not determine, so the key folds them and a dep
    /// whose resolved content changed is another key; `artifacts` are
    /// the resolved graph's native-artifact pins, each artifact's name
    /// over the content hash of the library the resolution installed —
    /// a bound constant addresses an export by the installed library's
    /// enumeration index, so a repinned library is another key and a
    /// warm module never serves bytecode that addresses the old
    /// library's exports.
    #[allow(clippy::fn_params_excessive_bools)]
    pub(crate) fn discover_linked(
        enabled: bool,
        no_ambient: bool,
        identities: &BTreeMap<String, Hash128>,
        artifacts: &BTreeMap<String, Hash128>,
        script: bool,
    ) -> Self {
        let ambient = (!no_ambient).then(|| ambient_identity(identities, script));
        let debug = std::env::var_os(DEBUG_ENV).is_some_and(|value| !value.is_empty());
        let cache = Self {
            root: enabled.then(ds_pkg::store::cache_root).flatten(),
            ambient,
            deps: identities.clone(),
            artifacts: artifacts.clone(),
            debug,
        };
        if cache.debug && cache.root.is_none() {
            eprintln!("ds run cache: off");
        }
        cache
    }

    /// The cached artifact for this source; `None` is the miss every
    /// caller falls back from. An unreadable source and an unreadable,
    /// stale, or corrupt artifact are all misses — never errors.
    pub(crate) fn lookup(&self, file: &Path) -> Option<Module> {
        let root = self.root.as_ref()?;
        // a source that cannot be read is a miss; the compile path reports it
        let source = std::fs::read_to_string(file).ok()?;
        let name = module_stem(file)?;
        self.lookup_at(root, &name, Hash128::compute(source.as_bytes()))
    }

    /// The cached artifact for one package root's composed member set:
    /// the key folds the root's member identity — the same
    /// ascending-path fold a dep-side identity records — so a sibling
    /// member's edit is another key and a warm artifact never serves the
    /// old member set. A one-member root folds its lone member's text
    /// hash, the key the plain-script lookup always answered, so the fix
    /// lands no cache churn there. `None` is the miss every caller falls
    /// back from, exactly `lookup`'s refusals.
    pub(crate) fn lookup_root(
        &self,
        file: &Path,
        members: &[crate::package::Member],
    ) -> Option<Module> {
        let root = self.root.as_ref()?;
        let name = module_stem(file)?;
        let identity = crate::package::member_identity(members);
        self.lookup_at(root, &name, identity)
    }

    /// One cached artifact's keyed read for `(name, identity)`: the fold
    /// already keys on both, so the header check pins the artifact
    /// honestly against the demand.
    fn lookup_at(&self, root: &Path, name: &str, identity: Hash128) -> Option<Module> {
        let artifact = cache_path(root, TARGET_SEGMENT, self.key(name, identity));
        let Ok(mapped) = read_artifact(&artifact) else {
            return self.miss(Family::Run, &artifact);
        };
        let module = mapped.module();
        if module.source_hash() != identity || module.name() != name {
            return self.miss(Family::Run, &artifact);
        }
        self.report(Family::Run, "hit", &artifact);
        Some(mapped.into_module())
    }

    /// Writes one successfully compiled module under its key. Every
    /// failure is silent — the run already succeeded.
    pub(crate) fn store(&self, module: &Module) {
        let Some(root) = &self.root else {
            return;
        };
        let name = module.name();
        let hash = module.source_hash();
        let artifact = cache_path(root, TARGET_SEGMENT, self.key(name, hash));
        if artifact_validates(&artifact, name, hash) {
            return;
        }
        write_atomic(&artifact, &ds_mod::write(module));
    }

    /// The cached compiled module for one linked dep member, when the
    /// graph resolved the name and the load walk demands exactly the
    /// content the graph pinned: `None` is the miss every caller falls
    /// back from. A name the graph never resolved (the ambient's merged
    /// reading, a zero-marker link) is no lookup — the ordinary serve
    /// answers those. The header check re-validates the artifact's
    /// name and source hash against the demand, exactly the pair the
    /// load walk itself validates.
    pub(crate) fn lookup_dep(&self, name: &str, linked: Hash128) -> Option<Module> {
        let root = self.root.as_ref()?;
        if self.deps.get(name) != Some(&linked) {
            return None;
        }
        let artifact = cache_path(root, TARGET_SEGMENT, self.dep_key(name, linked));
        let Ok(mapped) = read_artifact(&artifact) else {
            return self.miss(Family::Dep, &artifact);
        };
        let module = mapped.module();
        if module.name() != name || module.source_hash() != linked {
            return self.miss(Family::Dep, &artifact);
        }
        self.report(Family::Dep, "hit", &artifact);
        Some(mapped.into_module())
    }

    /// Writes one linked dep member's compiled module under the key
    /// `lookup_dep` reads: the module the walk just installed for this
    /// name at the identity the graph pinned. Every failure is silent —
    /// the run already succeeded.
    pub(crate) fn store_dep(&self, name: &str, linked: Hash128, module: &Module) {
        let Some(root) = &self.root else {
            return;
        };
        if self.deps.get(name) != Some(&linked)
            || module.name() != name
            || module.source_hash() != linked
        {
            return;
        }
        let artifact = cache_path(root, TARGET_SEGMENT, self.dep_key(name, linked));
        if artifact_validates(&artifact, name, linked) {
            return;
        }
        write_atomic(&artifact, &ds_mod::write(module));
    }

    /// The cached `.dsi` beside one linked dep member's dsmod: the same
    /// dep fold key with the artifact extension, so the member's checked
    /// prefix — the surface `ds info` and the LSP hover serve — lands
    /// beside its module. `None` when the cache is off.
    pub(crate) fn dep_dsi_path(&self, name: &str, linked: Hash128) -> Option<PathBuf> {
        let root = self.root.as_ref()?;
        Some(cache_path(root, TARGET_SEGMENT, self.dep_key(name, linked)).with_extension("dsi"))
    }

    /// Persists one linked dep member's checked prefix beside its cached
    /// dsmod, under the same key: the fresh declaration-phase snapshot
    /// (a second declare pass, the same shape the prefix tier's own
    /// store pays once per changed source), the solved method rows, and
    /// the compile's own effect rows, headed by the member's identity —
    /// the same hash the dsmod beside it validates against. A
    /// `--no-ambient` compile persists the same way under the zero
    /// marker its dep key folds. Every failure is silent — the
    /// run already succeeded.
    pub(crate) fn store_dep_dsi(
        &self,
        name: &str,
        identity: Hash128,
        analyzed: &ds_check::Analyzed,
        effects: &ds_check::Effects,
        declared: &ds_check::DeclaredRows,
        world: &ds_check::WorldCaps,
    ) {
        if self.root.is_none() {
            return;
        }
        if self.deps.get(name) != Some(&identity) {
            return;
        }
        let Some(artifact) = self.dep_dsi_path(name, identity) else {
            return;
        };
        // content addressing makes an artifact that already decodes
        // against this identity the exact bytes this write would
        // produce, so an existing one skips the extraction entirely
        if std::fs::read(&artifact)
            .is_ok_and(|bytes| ds_dsi::decode_prefix(&bytes, identity).is_ok())
        {
            return;
        }
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
        write_atomic(&artifact, &ds_dsi::artifact_bytes(identity, &payload));
    }

    /// The blake3-128 fold over every input that changes the produced
    /// bytecode for one source text, each component framed so none can
    /// prefix another:
    /// - the domain tag above;
    /// - the module name: the file stem lands in the artifact header;
    /// - the compile's content identity: a plain script's or one-member root's source hash, a
    ///   multi-member root's composed member fold, so a sibling edit is another key;
    /// - the ambient identity: the stdlib ambient's text hash when its impl bodies lower into the
    ///   module, a marker byte when `--no-ambient` compiles without it;
    /// - each resolved dep's link name and content hash in ascending name order: the identities
    ///   package resolution pinned, which the source text alone does not determine, so a dep whose
    ///   resolved content changed is another key and never serves an artifact linked against the
    ///   old content. A plain script folds the empty list — its recorded dep hashes are pure
    ///   functions of the components above (the ambient's for `@std`, the zero marker otherwise);
    /// - each resolved native artifact's name and content hash in ascending name order, over the
    ///   list's own marker: a bound constant addresses an export by the installed library's
    ///   enumeration index, so a repinned library (a moved `[artifacts]` hash under one name) is
    ///   another key and a warm module never serves bytecode that addresses the old library's
    ///   exports;
    /// - the dsmod format version: the reader refuses foreign versions anyway, and folding it
    ///   re-keys instead of paying the refused read;
    /// - the build identity: a build-time digest of the compiler crates' source trees (the package
    ///   version alone in packaged builds), so two builds whose bytecode output can differ never
    ///   share an artifact.
    fn key(&self, module_name: &str, source_hash: Hash128) -> Hash128 {
        self.fold(KEY_DOMAIN, module_name, source_hash)
    }

    /// The dep-member fold: the same component set over its own domain
    /// tag, keyed on the member's link name and content identity. The
    /// graph identities and artifact pins ride the fold like the root's:
    /// a dep module's own dep list pins the content its imports linked
    /// at, so any dep's moved identity is another key for every member,
    /// and a moved artifact pin re-keys the member the same way — an
    /// artifact linked against a transitive dep's old content never
    /// serves a new resolution.
    fn dep_key(&self, name: &str, identity: Hash128) -> Hash128 {
        self.fold(DEP_KEY_DOMAIN, name, identity)
    }

    /// One domain-tagged blake3-128 fold over the shared component set
    /// (see `key`'s component list).
    fn fold(&self, domain: &[u8], module_name: &str, source_hash: Hash128) -> Hash128 {
        let mut hasher = blake3::Hasher::new();
        hasher.update(domain);
        fold_str(&mut hasher, module_name);
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
        // framed around its hash so no two pairs splice; a plain
        // script folds the empty list (see `key`'s doc)
        hasher.update(&(self.deps.len() as u64).to_le_bytes());
        for (name, content) in &self.deps {
            fold_str(&mut hasher, name);
            hasher.update(content.as_bytes());
        }
        // the resolved graph's artifact pins, each name framed around
        // its hash over the list's own marker (a plain script folds the
        // empty list; see `key`'s doc)
        hasher.update(ARTIFACT_LIST_TAG);
        hasher.update(&(self.artifacts.len() as u64).to_le_bytes());
        for (name, hash) in &self.artifacts {
            fold_str(&mut hasher, name);
            hasher.update(hash.as_bytes());
        }
        hasher.update(&[FORMAT_VERSION]);
        fold_str(&mut hasher, BUILD_ID);
        let digest = hasher.finalize();
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest.as_bytes()[..16]);
        Hash128::new(bytes)
    }

    /// Reports one lookup decision on stderr when the debug switch is
    /// set: the observable that proves which path a run took without
    /// timing it. The family names the artifact tier the decision is
    /// about — a dep tier's `hit` is the proof a warm run served the
    /// dep members from disk instead of compiling them, since the
    /// module bytes cannot prove it (a fresh compile answers the same
    /// bytes).
    fn report(&self, family: Family, state: &str, artifact: &Path) {
        if self.debug {
            eprintln!("{}: {state} {}", family.prefix(), artifact.display());
        }
    }

    fn miss<T>(&self, family: Family, artifact: &Path) -> Option<T> {
        self.report(family, "miss", artifact);
        None
    }
}

/// Which cache family one debug line names: the root fold's module
/// tier or a linked dep member's — the two artifact families share the
/// root, and the line tells them apart.
enum Family {
    Run,
    Dep,
}

impl Family {
    /// The debug line's family prefix.
    fn prefix(self) -> &'static str {
        match self {
            Family::Run => "ds run cache",
            Family::Dep => "ds dep cache",
        }
    }
}

/// Folds one length-prefixed UTF-8 component so adjacent
/// variable-length components cannot splice.
pub(crate) fn fold_str(hasher: &mut blake3::Hasher, text: &str) {
    hasher.update(&(text.len() as u64).to_le_bytes());
    hasher.update(text.as_bytes());
}

/// Whether the artifact already under `path` validates as the module
/// `(name, hash)` names: content addressing makes an artifact that
/// passes this check the exact bytes the write would produce, so an
/// existing one is the write and the store skips it.
fn artifact_validates(path: &Path, name: &str, hash: Hash128) -> bool {
    read_artifact(path).is_ok_and(|mapped| {
        let module = mapped.module();
        module.name() == name && module.source_hash() == hash
    })
}

/// Lands one artifact's bytes under its content-addressed path with
/// the temp-plus-rename discipline every `$DS_CACHE` writer shares
/// (this helper and the prefix cache's `.dsi` store both ride it):
/// concurrent writers race to byte-identical content and a reader
/// never sees a partial file. Windows rename refuses to replace, so a
/// lost race (or a repair over a corrupt artifact) removes and retries
/// once; a mapped file that cannot be removed waits for a later run.
/// Every failure is silent — the caller's run already succeeded.
pub(crate) fn write_atomic(artifact: &Path, bytes: &[u8]) {
    let Some(dir) = artifact.parent() else {
        return;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let Ok(mut temp) = tempfile::NamedTempFile::new_in(dir) else {
        return;
    };
    if temp.write_all(bytes).is_err() {
        return;
    }
    if let Err(persist) = temp.persist(artifact)
        && persist.error.kind() == std::io::ErrorKind::AlreadyExists
    {
        let _ = std::fs::remove_file(artifact);
        let temp = persist.file;
        let _ = temp.persist(artifact);
    }
}

/// The module name the source path derives (the file stem, lossily) —
/// the same derivation the compile path records; a path with no stem is
/// a miss (the compile path reports it).
fn module_stem(path: &Path) -> Option<String> {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ds_mod::Hash128;

    use super::{BUILD_ID, RunCache};

    #[test]
    fn the_build_identity_is_present() {
        assert_ne!(BUILD_ID, "");
    }

    /// The dep-identity fold re-keys: one dep's changed content moves
    /// the key, the link name frames its hash (no two pairs splice),
    /// and a plain script keeps the empty-list fold its recorded hashes
    /// (pure functions of the folded components) already determine.
    #[test]
    fn the_dep_identity_fold_rekeys() {
        let source = Hash128::compute(b"fn main() -> () { }");
        let plain = RunCache::discover(false, false);
        let at = |identity: &[u8]| {
            RunCache::discover_linked(
                false,
                false,
                &BTreeMap::from([("@lib".to_owned(), Hash128::compute(identity))]),
                &BTreeMap::new(),
                false,
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
        let named = RunCache::discover_linked(
            false,
            false,
            &BTreeMap::from([
                ("@a".to_owned(), Hash128::compute(b"one")),
                ("@b".to_owned(), Hash128::compute(b"two")),
            ]),
            &BTreeMap::new(),
            false,
        );
        let swapped = RunCache::discover_linked(
            false,
            false,
            &BTreeMap::from([
                ("@a".to_owned(), Hash128::compute(b"two")),
                ("@b".to_owned(), Hash128::compute(b"one")),
            ]),
            &BTreeMap::new(),
            false,
        );
        assert_ne!(
            named.key("app", source),
            swapped.key("app", source),
            "the link name frames its hash"
        );
    }

    /// The session shape re-keys over a std-declaring graph: an entry
    /// script session composes the @std members back (the merged
    /// ambient's bodies lower into its module) while a package compile
    /// lowers the shrunk core, so two compiles of the same members over
    /// the same resolution never share a key — an artifact the merged
    /// reading wrote never serves the core reading.
    #[test]
    fn the_session_shape_rekeys_over_a_std_declaring_graph() {
        let source = Hash128::compute(b"fn main() -> () { }");
        let identities = BTreeMap::from([("@std".to_owned(), Hash128::compute(b"bundle"))]);
        let session = RunCache::discover_linked(false, false, &identities, &BTreeMap::new(), true);
        let package = RunCache::discover_linked(false, false, &identities, &BTreeMap::new(), false);
        assert_ne!(
            session.key("app", source),
            package.key("app", source),
            "the session composition is a bytecode input the key folds"
        );
    }

    /// The no-ambient marker re-keys both folds: a `--no-ambient`
    /// graph's root and dep-member keys (the dsmod and the `.dsi` beside
    /// it) never agree with the ambient-shaped fold over the same
    /// resolution, so a warm artifact of one shape never serves the
    /// other's compile.
    #[test]
    fn the_no_ambient_marker_rekeys_both_folds() {
        let source = Hash128::compute(b"fn main() -> () { }");
        let member = Hash128::compute(b"pub fn twice(n: int) -> int { 2 * n }");
        let deps = BTreeMap::from([("@lib".to_owned(), Hash128::compute(b"one"))]);
        let ambient = RunCache::discover_linked(false, false, &deps, &BTreeMap::new(), false);
        let bare = RunCache::discover_linked(false, true, &deps, &BTreeMap::new(), false);
        assert_ne!(
            ambient.key("app", source),
            bare.key("app", source),
            "the root fold re-keys on the ambient's absence"
        );
        assert_ne!(
            ambient.dep_key("@lib", member),
            bare.dep_key("@lib", member),
            "the dep fold re-keys on the ambient's absence"
        );
    }

    /// The dep-member fold re-keys on the member's own identity and on
    /// every graph identity, and its domain never collides with the
    /// root fold — a root artifact and a dep artifact of the same name
    /// and content are different modules (the entry session's host
    /// member rides the root compile alone), so the domains must keep
    /// them apart.
    #[test]
    fn the_dep_member_fold_rekeys_and_stays_its_own_domain() {
        let member = Hash128::compute(b"pub fn twice(n: int) -> int { 2 * n }");
        let at = |identity: &[u8]| {
            RunCache::discover_linked(
                false,
                false,
                &BTreeMap::from([("@deep".to_owned(), Hash128::compute(identity))]),
                &BTreeMap::new(),
                false,
            )
        };
        assert_ne!(
            at(b"one").dep_key("@lib", member),
            at(b"two").dep_key("@lib", member),
            "a graph dep's moved identity re-keys the member fold"
        );
        let other_member = Hash128::compute(b"pub fn twice(n: int) -> int { 3 * n }");
        assert_ne!(
            at(b"one").dep_key("@lib", member),
            at(b"one").dep_key("@lib", other_member),
            "the member's own content identity re-keys"
        );
        assert_ne!(
            at(b"one").dep_key("@lib", member),
            at(b"one").key("@lib", member),
            "the dep fold never answers the root fold's domain"
        );
    }

    /// The artifact-pin fold re-keys: a moved pin moves the key (a bound
    /// constant addresses an export by the installed library's
    /// enumeration index, so a repinned library must never serve the
    /// module compiled against the old one), the pin's name frames its
    /// hash, and the dep-member fold re-keys alike.
    #[test]
    fn the_artifact_pin_fold_rekeys() {
        let source = Hash128::compute(b"fn main() -> () { }");
        let plain = RunCache::discover(false, false);
        let pinned = |hash: &[u8]| {
            RunCache::discover_linked(
                false,
                false,
                &BTreeMap::new(),
                &BTreeMap::from([("math".to_owned(), Hash128::compute(hash))]),
                false,
            )
        };
        assert_ne!(
            pinned(b"one").key("app", source),
            pinned(b"two").key("app", source),
            "a moved pin is another key"
        );
        assert_ne!(
            plain.key("app", source),
            pinned(b"one").key("app", source),
            "a pinned graph never serves a plain script's key"
        );
        assert_ne!(
            pinned(b"one").dep_key("@lib", source),
            pinned(b"two").dep_key("@lib", source),
            "a moved pin re-keys the dep-member fold"
        );
        let both = RunCache::discover_linked(
            false,
            false,
            &BTreeMap::new(),
            &BTreeMap::from([
                ("aa".to_owned(), Hash128::compute(b"one")),
                ("bb".to_owned(), Hash128::compute(b"two")),
            ]),
            false,
        );
        let swapped = RunCache::discover_linked(
            false,
            false,
            &BTreeMap::new(),
            &BTreeMap::from([
                ("aa".to_owned(), Hash128::compute(b"two")),
                ("bb".to_owned(), Hash128::compute(b"one")),
            ]),
            false,
        );
        assert_ne!(
            both.key("app", source),
            swapped.key("app", source),
            "the pin's name frames its hash"
        );
    }
}
