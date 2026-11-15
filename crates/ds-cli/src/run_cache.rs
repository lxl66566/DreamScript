//! The script-mode dsmod cache behind `ds run <file.ds>`: the CLI-owned
//! key fold, the mmap hit path, and the temp-plus-rename write, over the
//! `$DS_CACHE` root ds-pkg resolves (the one authority the package store
//! shares). The cache never fails a run: a root that
//! cannot resolve, an artifact that cannot be read or validated, and a
//! write that cannot land are all silent falls back to the in-process
//! source pipeline.

use std::{
    io::Write,
    path::{Path, PathBuf},
};

use ds_mod::{EntryArg, FORMAT_VERSION, Hash128, Module, cache_path, read_mmap};

/// The segment below `$DS_CACHE/ir` holding script artifacts. The source
/// pipeline emits portable bytecode-class modules, so the segment names
/// the class; a tier that emits target-specific code takes its own
/// triple segment without disturbing this one.
const TARGET_SEGMENT: &str = "bytecode";

/// Set to any non-empty value for one lookup-decision line per run on
/// stderr: `hit`, `miss`, `bypassed`, or `off`.
const DEBUG_ENV: &str = "DS_RUN_CACHE_DEBUG";

/// The fold's domain tag, bumped when the folded component set changes:
/// old artifacts must never answer a new fold.
const KEY_DOMAIN: &[u8] = b"ds-run-cache-v1";

/// The build identity folded into the key: build.rs digests the source
/// trees of every workspace crate the source pipeline runs through plus
/// the workspace lock, so any rebuild whose bytecode output can differ
/// re-keys — another worktree, a rebuilt lowerer, an A/B pair never
/// share artifacts through a shared cache root. A packaged build
/// without those sibling sources falls back to the package version,
/// which at least follows releases.
pub(crate) const BUILD_ID: &str =
    if env!("DS_BUILD_ID").is_empty() { env!("CARGO_PKG_VERSION") } else { env!("DS_BUILD_ID") };

/// One run's cache handle: the resolved root (absent when the run is
/// uncached) and the ambient identity the key folds.
pub(crate) struct RunCache {
    root: Option<PathBuf>,
    ambient: Option<Hash128>,
    debug: bool,
}

impl RunCache {
    /// Resolves the cache for one run. `enabled` is the `--no-cache` off
    /// switch; `no_ambient` picks the ambient identity the key folds
    /// (the ambient's impl bodies lower into the script module, so the
    /// ambient text is a bytecode input).
    pub(crate) fn discover(enabled: bool, no_ambient: bool) -> Self {
        let debug = std::env::var_os(DEBUG_ENV).is_some_and(|value| !value.is_empty());
        let cache = Self {
            root: enabled.then(ds_pkg::store::cache_root).flatten(),
            ambient: (!no_ambient).then(|| Hash128::compute(ds_stdlib::AMBIENT_SOURCE.as_bytes())),
            debug,
        };
        if cache.debug && cache.root.is_none() {
            eprintln!("ds run cache: off");
        }
        cache
    }

    /// The cached artifact for this source together with the staging
    /// plan shape it serves; `None` is the miss every caller falls back
    /// from. An unreadable source, an unreadable, stale, or corrupt
    /// artifact, and a parameterized function the artifact carries no
    /// recorded plan for are all misses or bypasses — never errors.
    pub(crate) fn lookup(&self, file: &Path, entry: &str) -> Option<(Module, ServedPlan)> {
        let root = self.root.as_ref()?;
        // a source that cannot be read is a miss; the compile path reports it
        let source = std::fs::read_to_string(file).ok()?;
        let name = module_stem(file)?;
        let artifact = cache_path(
            root,
            TARGET_SEGMENT,
            self.key(&name, Hash128::compute(source.as_bytes())),
        );
        let Ok(mapped) = read_mmap(&artifact) else {
            return self.miss(&artifact);
        };
        // the fold already keys on the source text and the stem; the
        // header check pins the artifact honestly against both
        let module = mapped.module();
        if module.source_hash() != Hash128::compute(source.as_bytes()) || module.name() != name {
            return self.miss(&artifact);
        }
        let plan = match entry_staging(module, entry) {
            EntryStaging::Raw => ServedPlan::Raw,
            EntryStaging::NoParams => ServedPlan::Empty,
            EntryStaging::Args(plans) => ServedPlan::Args {
                plans: plans.to_vec(),
                int: module.tag_id("int"),
                none: module.tag_id("none"),
            },
            EntryStaging::FrontEndOnly => {
                self.report("bypassed", &artifact);
                return None;
            },
        };
        self.report("hit", &artifact);
        Some((mapped.into_module(), plan))
    }

    /// Writes one successfully compiled module under its key. A run
    /// whose entry the cache can never serve does not write: a
    /// parameterized function with no recorded entry plan would stage
    /// differently warm than the compile path does. Every failure is
    /// silent — the run already succeeded.
    pub(crate) fn store(&self, entry: &str, module: &Module) {
        let Some(root) = &self.root else {
            return;
        };
        if matches!(entry_staging(module, entry), EntryStaging::FrontEndOnly) {
            return;
        }
        let name = module.name();
        let hash = module.source_hash();
        let artifact = cache_path(root, TARGET_SEGMENT, self.key(name, hash));
        // content addressing makes an artifact that already validates
        // against this key the exact bytes this write would produce, so
        // an existing one is the write
        if read_mmap(&artifact).is_ok_and(|mapped| {
            mapped.module().source_hash() == hash && mapped.module().name() == name
        }) {
            return;
        }
        let Some(dir) = artifact.parent() else {
            return;
        };
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        let bytes = ds_mod::write(module);
        let Ok(mut temp) = tempfile::NamedTempFile::new_in(dir) else {
            return;
        };
        if temp.write_all(&bytes).is_err() {
            return;
        }
        // temp-plus-rename in the artifact's own directory: concurrent
        // writers race to byte-identical content and a reader never sees
        // a partial file. Windows rename refuses to replace, so a lost
        // race (or a repair over a corrupt artifact) removes and retries
        // once; a mapped file that cannot be removed waits for a later
        // run.
        if let Err(persist) = temp.persist(&artifact) {
            if persist.error.kind() == std::io::ErrorKind::AlreadyExists {
                let _ = std::fs::remove_file(&artifact);
                let temp = persist.file;
                let _ = temp.persist(&artifact);
            }
        }
    }

    /// The blake3-128 fold over every input that changes the produced
    /// bytecode for one source text, each component framed so none can
    /// prefix another:
    /// - the domain tag above;
    /// - the module name: the file stem lands in the artifact header;
    /// - the entry source hash;
    /// - the ambient identity: the stdlib ambient's text hash when its impl bodies lower into the
    ///   module, a marker byte when `--no-ambient` compiles without it;
    /// - each linked dep's content hash in link order: the dep-list writer records the source's
    ///   imports into the header, but this round every recorded hash is a pure function of
    ///   components already folded — the source hash determines the imports, the ambient identity
    ///   determines @std's hash, every other name records the zero marker — so folding them adds no
    ///   key entropy, and the lookup side cannot derive dep content without the front end (the key
    ///   must be computable before the artifact is read). The fold lands when package resolution
    ///   produces dep identities the source text alone does not determine;
    /// - the dsmod format version: the reader refuses foreign versions anyway, and folding it
    ///   re-keys instead of paying the refused read;
    /// - the build identity: a build-time digest of the compiler crates' source trees (the package
    ///   version alone in packaged builds), so two builds whose bytecode output can differ never
    ///   share an artifact.
    fn key(&self, module_name: &str, source_hash: Hash128) -> Hash128 {
        let mut hasher = blake3::Hasher::new();
        hasher.update(KEY_DOMAIN);
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
        // no dep identity beyond the folded components yet (see `key`'s
        // doc); resolved dep hashes fold right here when they exist
        hasher.update(&0u64.to_le_bytes());
        hasher.update(&[FORMAT_VERSION]);
        fold_str(&mut hasher, BUILD_ID);
        let digest = hasher.finalize();
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest.as_bytes()[..16]);
        Hash128::new(bytes)
    }

    /// Reports one lookup decision on stderr when the debug switch is
    /// set: the observable that proves which path a run took without
    /// timing it.
    fn report(&self, state: &str, artifact: &Path) {
        if self.debug {
            eprintln!("ds run cache: {state} {}", artifact.display());
        }
    }

    fn miss<T>(&self, artifact: &Path) -> Option<T> {
        self.report("miss", artifact);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::BUILD_ID;

    #[test]
    fn the_build_identity_is_present() {
        assert_ne!(BUILD_ID, "");
    }
}

/// The staging plan shape a served artifact carries, which is the whole
/// plan: the artifact's entry table records each candidate entry's
/// per-parameter staging facts and its tag table the int and none member
/// ids, so the hit path stages exactly as the compile path does; entries
/// the table does not record fall back to the function table's shape (no
/// function of the name is raw staging, a zero-parameter function stages
/// nothing).
#[derive(Clone)]
pub(crate) enum ServedPlan {
    /// No function of the entry name: the compile path found no entry
    /// binding, and `--args` stage as raw i64 words (the run refuses at
    /// entry resolution).
    Raw,
    /// The entry function takes no parameter words: nothing stages when
    /// `--args` is empty, and a staged list answers the compile path's
    /// parameter-count refusal.
    Empty,
    /// The artifact's recorded plan for the entry: one staging fact per
    /// parameter, plus the int and none member tags the optional arm
    /// stages.
    Args {
        /// The per-parameter staging facts, in parameter order.
        plans: Vec<EntryArg>,
        /// The `int` member's module tag id.
        int: Option<u8>,
        /// The `none` member's module tag id.
        none: Option<u8>,
    },
}

/// Which `--args` staging rule an entry can reproduce from the finished
/// module alone — the hit path's correctness story.
enum EntryStaging<'a> {
    /// No function of the entry name.
    Raw,
    /// The entry function takes no parameter words.
    NoParams,
    /// The artifact's entry table records the entry's per-parameter
    /// staging plan.
    Args(&'a [EntryArg]),
    /// A parameterized function with no recorded plan (a synthesized
    /// member spelling, or an artifact whose writer recorded no entry
    /// facts): its staging plan is a front-end fact this artifact does
    /// not carry. These runs always compile in-process and never write an
    /// artifact.
    FrontEndOnly,
}

/// The entry's staging class, read off the module's own tables: the
/// entry table first (a top-level lambda always lowers under its binding
/// name, and the compile path records every one of them), then the
/// function table's shape for the spellings the entry table cannot
/// cover.
fn entry_staging<'a>(module: &'a Module, entry: &str) -> EntryStaging<'a> {
    if let Some(plans) = module.entry_args(entry) {
        return EntryStaging::Args(plans);
    }
    match module.functions().iter().find(|f| f.name() == entry) {
        None => EntryStaging::Raw,
        Some(function) if function.layout().params().is_empty() => EntryStaging::NoParams,
        Some(_) => EntryStaging::FrontEndOnly,
    }
}

/// Folds one length-prefixed UTF-8 component so adjacent
/// variable-length components cannot splice.
pub(crate) fn fold_str(hasher: &mut blake3::Hasher, text: &str) {
    hasher.update(&(text.len() as u64).to_le_bytes());
    hasher.update(text.as_bytes());
}

/// The module name the source path derives (the file stem, lossily) —
/// the same derivation the compile path records; a path with no stem is
/// a miss (the compile path reports it).
fn module_stem(path: &Path) -> Option<String> {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
}
