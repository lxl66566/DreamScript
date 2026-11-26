//! The package-run wiring behind `ds run` and `ds build`: a `ds.pkg`
//! beside the entry file turns the subcommand over that source into a
//! package run. The manifest resolves over the store's provider seam —
//! `$DS_REGISTRY` naming a registry (a local index root or a git remote
//! URL; the git client's snapshot clone serves the same verified
//! content-addressed fetches) fills the store's misses, no registry
//! meaning the installed rows alone answer — the graph's `[artifacts]`
//! libraries install content-addressed beside the bundles, and the
//! deps-first load walk serves the graph:
//! `@std` from the checked-in ambient artifact, every other linked name
//! from its package's single source member compiled in-process under the
//! package's link name, the graph's identities feeding the dep-list
//! writer so the artifacts pin the content they link. The light key
//! entry [`resolve_identities`] serves the cache probes — `ds info`'s
//! prefix tier, the LSP's linked surfaces — the resolved graph's key
//! inputs alone, the deps' member identities carried from the provider
//! seam (folded where the provider decoded each demanded bundle — one
//! pinned file per demand, never a store-wide scan — so beyond the
//! resolution walk itself a probe loads and decodes no dep bundle),
//! stopping ahead of the artifact installs and the graph assembly the
//! run paths pay.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    rc::Rc,
};

use anyhow::{Context, Result, anyhow, bail};
use ds_check::{ForeignEntry, ForeignKind, ImportGraph, ImportSurface, MemberSurface};
use ds_diag::{FileId, Files, render};
use ds_mod::{Hash128, Module};
use ds_parse::ItemKind;
use ds_pkg::{
    Demand, Fetched, Manifest, Package, ProvidedPackage, ResolveError, Store, StoreProvider,
    Version, fetch_path, resolve,
};

use crate::source::{self, CompiledSource, Export, ExportKind, rebase_sig, rebase_span};

/// The manifest's file name inside a package root.
pub(crate) const MANIFEST_FILE: &str = ds_pkg::MANIFEST_FILE;

/// One package member file: the member's path inside the package — the
/// display path the compile registers and the identity fold records —
/// and its text.
#[derive(Clone)]
pub(crate) struct Member {
    path: String,
    text: String,
}

/// The member identity the dep-list writer records and the walk
/// validates (G3.5), delegated to the ds-pkg fold so a root's walked
/// members and a dep's resolved bundle share one identity definition:
/// a single member's source hash, or the composed fold over every
/// member in ascending path order — one length-prefixed path and text
/// pair per member, so the composition is byte-stable and a member
/// rename or text edit changes it. The package-root caches fold the
/// same identity over the root's own members, so a sibling edit
/// re-keys them exactly as a dep's moved content re-keys the dep folds.
pub(crate) fn member_identity(members: &[Member]) -> Hash128 {
    ds_pkg::member_identity(
        members
            .iter()
            .map(|member| (member.path.as_str(), member.text.as_bytes())),
    )
    .expect("a walked member set carries at least one UTF-8 member")
}

/// One linked package's compiled module and member surfaces (G4): the
/// module the load walk installs, and the exports a dependent's imports
/// validate against, in the package compile's own file space.
struct LinkedPackage {
    module: Module,
    members: Rc<[Member]>,
    exports: Vec<Export>,
    impls: Vec<ds_check::import::ImplFact>,
}

/// The graph one package run serves from: each resolved dependency's
/// member files, keyed by link name and held as one shared slice
/// (the memoized compile reads the same members, no second copy), the
/// member identities the dep-list writer records, the graph's native
/// artifacts (installed content-addressed ahead of the walk), and the
/// compiled packages those identities pin, memoized by link name so a
/// package the walk and the surface builder both demand compiles once.
/// The graph's bindable artifact loads (H5): the modules whose bytes load
/// and enumerate, beside each load that refused with its rendered reason.
type BoundLoads = (Vec<(String, ds_vm::BoundModule)>, Vec<(String, String)>);

pub(crate) struct Graph {
    members: BTreeMap<String, Rc<[Member]>>,
    identities: BTreeMap<String, Hash128>,
    artifacts: BTreeMap<String, ds_pkg::ArtifactBound>,
    /// Every directory a resolved path dep read from (the transitive
    /// closure, ascending): the extra watch roots a dev loop grows over
    /// the package tree.
    path_dirs: Vec<PathBuf>,
    /// The store the resolution served and the artifact installs landed
    /// in: the run path reopens it per demand (the bound libraries and
    /// their export enumerations read from here).
    store: Store,
    no_ambient: bool,
    linked: RefCell<BTreeMap<String, Rc<LinkedPackage>>>,
    /// The link names currently compiling: a source-import cycle (a
    /// package importing itself or a chain of packages importing each
    /// other — edges the manifest `[deps]` graph's cycle refusal never
    /// sees) re-enters `linked` for a name whose memo entry does not
    /// exist yet, so this set is the recursion's only guard.
    in_progress: RefCell<BTreeSet<String>>,
}

/// The resolved graph's cache-key inputs: each resolved dep's link name
/// over the member identity the resolution seam carries, and each
/// declared native artifact's name over its content pin — the maps the
/// script and prefix cache keys fold. [`resolve_identities`] answers
/// these alone, so a cache probe pays no dep bundle load, no artifact
/// install, and no graph assembly.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct GraphKeys {
    /// Each resolved dep's identity.
    pub(crate) identities: BTreeMap<String, Hash128>,
    /// Each declared native artifact's content pin, name-keyed.
    pub(crate) artifact_pins: BTreeMap<String, Hash128>,
}

/// The resolution front `open` and [`resolve_identities`] share: the
/// manifest read, the store and registry wiring, and the MVS graph
/// resolution (each demand carrying the winning declarer's pin, so the
/// store serves the one pinned file per demand without scanning its
/// installed rows) — everything the two entries answer before they
/// part, the run paths continuing into the artifact installs, the
/// member retention, and the graph's compile machinery.
struct Front {
    /// The store the resolution served and the artifact installs land
    /// in.
    store: Store,
    /// The registry behind `$DS_REGISTRY`, filling the store's misses;
    /// `None` resolves the installed rows alone.
    registry: Option<ds_pkg::RegistryClient>,
    /// The resolved graph.
    resolved: ds_pkg::Resolved,
}

/// The shared resolution front over the manifest beside `entry`:
/// `None` names a manifest-free entry, which stays the plain script the
/// existing paths serve.
///
/// # Errors
///
/// An unreadable or drifting manifest, an unresolvable cache root, and
/// every resolution refusal the store's own knowledge reports.
fn resolve_front(entry: &Path) -> Result<Option<Front>> {
    let Some(manifest_path) = manifest_beside(entry) else {
        return Ok(None);
    };
    let manifest = read_manifest(&manifest_path)?;
    if manifest.name == ds_stdlib::STD_PACKAGE_NAME {
        bail!(
            "`{}` is the reserved standard-library package name; a user package cannot be run, \
             built, or saved under it",
            ds_stdlib::STD_PACKAGE_NAME
        );
    }
    let Some(root) = ds_pkg::store::cache_root() else {
        bail!(
            "no cache root resolves ($DS_CACHE unset, no user cache directory); a package run \
             needs the package store below it"
        );
    };
    let store = Store::open(&root);
    // the base a path dep's relative directory resolves against: the
    // manifest's own directory, the declaring package's root
    let root_dir = manifest_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let registry = ds_pkg::RegistryClient::from_env();
    // the registry client fills the store's misses — a local index or
    // a git remote's snapshot clone, its verified bundles installing
    // before they serve; no registry means the installed rows answer,
    // the store's own knowledge carrying the taxonomy. The reserved
    // `@std` name never reaches either: the default install answers it
    // ahead of the store and the registry, so the toolchain's package
    // owns the name whatever a row claims
    let resolved = if let Some(registry) = &registry {
        let mut provider =
            StoreProvider::with_remote(&store, |name, ver| registry.fetch(name, ver));
        resolve(&manifest, &root_dir, |name, demand| match demand {
            Demand::Registry { ver, pin } => default_std_or(&store, &mut provider, name, ver, pin),
            Demand::Path { dir } => path_demand(&store, name, dir),
        })
        .map_err(|error| refuse(&error, true))?
    } else {
        let mut provider = StoreProvider::cached(&store);
        resolve(&manifest, &root_dir, |name, demand| match demand {
            Demand::Registry { ver, pin } => default_std_or(&store, &mut provider, name, ver, pin),
            Demand::Path { dir } => path_demand(&store, name, dir),
        })
        .map_err(|error| refuse(&error, false))?
    };
    Ok(Some(Front {
        store,
        registry,
        resolved,
    }))
}

/// The per-dep member sets the run's graph retains, link-name keyed.
type Members = BTreeMap<String, Rc<[Member]>>;
/// The per-dep identities the dep-list writer records, link-name keyed.
type Identities = BTreeMap<String, Hash128>;

/// Each resolved dep's member set, the run path's own fold: one store
/// bundle load and validation per dep, the members the run's graph
/// retains — the identities both graph entries record ride the
/// resolution seam instead (the provider folded them where it decoded
/// the bundle), so the light key entry never loads a bundle and the
/// heavy entry's own identities are the seam's by construction.
///
/// # Errors
///
/// A dep whose installed bundle will not validate, and one framing no
/// source member.
fn dep_members(store: &Store, resolved: &ds_pkg::Resolved) -> Result<Members> {
    let mut members = BTreeMap::new();
    for (name, package) in resolved.deps() {
        let bundle = store
            .load(package.content_hash)
            .map_err(|error| anyhow!("{error}"))?
            .with_context(|| {
                format!(
                    "the store holds no valid bundle for `{name}` at {} after resolving it",
                    package.content_hash
                )
            })?;
        let package_members = members_of(name, &bundle)?;
        members.insert(name.clone(), package_members.into());
    }
    Ok(members)
}

/// The resolved graph's per-dep identities, link-name keyed: the member
/// folds the resolution seam carried out of the provider — folded where
/// the provider decoded the demand's pinned bundle — one value for the
/// light key entry and the run's graph alike.
fn identities_of(resolved: &ds_pkg::Resolved) -> Identities {
    resolved
        .deps()
        .iter()
        .map(|(name, package)| (name.clone(), package.member_identity))
        .collect()
}

/// The resolved graph's cache-key inputs, the light entry: the same
/// manifest read and resolution `open` runs, the deps' identities read
/// off the seam (the provider folded them where it decoded each
/// bundle, so no dep bundle loads here), stopping ahead of the
/// native-artifact installs (each `[artifacts]` entry's
/// `ensure_artifact`, a cold artifact store behind a git registry
/// paying a snapshot clone) and the graph assembly the run paths add —
/// a cache probe (`ds info`'s prefix tier, the LSP's linked surfaces)
/// reads its key inputs without touching the artifacts or a dep's
/// bundle. `None` names a manifest-free entry.
///
/// # Errors
///
/// Every refusal `open` reports ahead of its artifact installs: an
/// unreadable or drifting manifest, an unresolvable cache root, and a
/// resolution refusal (a dep whose bundle folds no servable member set
/// included, the provider's own refusal).
pub(crate) fn resolve_identities(entry: &Path) -> Result<Option<GraphKeys>> {
    let Some(front) = resolve_front(entry)? else {
        return Ok(None);
    };
    Ok(Some(GraphKeys {
        identities: identities_of(&front.resolved),
        artifact_pins: artifact_pins_of(front.resolved.artifacts()),
    }))
}

/// The fold domain of the resolution-input fingerprint, so a
/// fingerprint never shares a spelling with another blake3 use.
const FINGERPRINT_DOMAIN: &[u8] = b"dreamscript-resolution-inputs";

/// The content fingerprint of the input set the root's resolution
/// consumes: the manifest's bytes — the registry pins, the artifact
/// pins, and the path dep directories all live inside them — plus each
/// path dep directory's own package content hash over the same edges
/// resolution walks, nested path deps included. The LSP's
/// linked-surface memo compares this once per hover ahead of every
/// map read: a match stands on the memo, a mismatch re-resolves, and
/// no mtime is read. `None` names a manifest-free entry (or one whose
/// reads refuse) — the shape the memo re-probes per hover anyway.
pub(crate) fn resolution_fingerprint(entry: &Path) -> Option<Hash128> {
    let manifest_path = manifest_beside(entry)?;
    let bytes = fs::read(&manifest_path).ok()?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(FINGERPRINT_DOMAIN);
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(&bytes);
    if let Ok(manifest) = Manifest::parse(&String::from_utf8_lossy(&bytes), FileId::new(0)) {
        // the base a path dep's relative directory resolves against:
        // the manifest's own directory, the declaring package's root
        let declaring_root = manifest_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        fold_path_deps(
            &mut hasher,
            &manifest,
            &declaring_root,
            &mut BTreeSet::new(),
        );
    }
    let digest = hasher.finalize();
    let mut hash = [0u8; 16];
    hash.copy_from_slice(&digest.as_bytes()[..16]);
    Some(Hash128::new(hash))
}

/// The path dep half of the resolution fingerprint: each directory's
/// package content hash — the content-addressed fold the path-dep
/// round keys the store by, so a dep source edit moves the
/// fingerprint — followed by the dep's own manifest's path edges
/// against the dep's own root, the same relative resolution the graph
/// walk applies. Each directory folds once, so a cycle the resolution
/// refuses cannot loop the fold.
fn fold_path_deps(
    hasher: &mut blake3::Hasher,
    manifest: &Manifest,
    declaring_root: &Path,
    visited: &mut BTreeSet<PathBuf>,
) {
    for bound in manifest.deps.values() {
        let ds_pkg::DepBound::Path { path } = bound else {
            continue;
        };
        let dir = declaring_root.join(path);
        if !visited.insert(dir.clone()) {
            continue;
        }
        let Ok(package) = ds_pkg::dir_package(&dir) else {
            continue;
        };
        hasher.update(package.content_hash().as_bytes());
        if let Ok(dep_manifest) = package.manifest(FileId::new(0)) {
            fold_path_deps(hasher, &dep_manifest, &dir, visited);
        }
    }
}

/// The package run `entry` shapes, when a manifest sits beside it:
/// `None` is the plain script the existing paths serve unchanged.
///
/// # Errors
///
/// An unreadable or drifting manifest, an unresolvable cache root, and
/// every resolution refusal the store's own knowledge reports.
pub(crate) fn open(entry: &Path, no_ambient: bool) -> Result<Option<Graph>> {
    let Some(front) = resolve_front(entry)? else {
        return Ok(None);
    };
    // the graph's native artifacts install before the walk serves: each
    // declared library validated against its content pin and landing in
    // the store's artifact segment, a registry's storage filling misses
    // — the `[artifacts]` entries are dependencies like `[deps]`, an
    // unserved one an unsatisfied demand
    for (name, artifact) in front.resolved.artifacts() {
        ensure_artifact(&front.store, front.registry.as_ref(), name, artifact)?;
    }
    let members = dep_members(&front.store, &front.resolved)?;
    let identities = identities_of(&front.resolved);
    let artifacts = front.resolved.artifacts().clone();
    let path_dirs = front.resolved.path_dirs();
    Ok(Some(Graph {
        members,
        identities,
        artifacts,
        path_dirs,
        store: front.store,
        no_ambient,
        linked: RefCell::new(BTreeMap::new()),
        in_progress: RefCell::new(BTreeSet::new()),
    }))
}

/// The folded native-artifact pins over one artifact map: each declared
/// library's name over its content pin, the pairs the cache keys fold.
fn artifact_pins_of(
    artifacts: &BTreeMap<String, ds_pkg::ArtifactBound>,
) -> BTreeMap<String, Hash128> {
    artifacts
        .iter()
        .map(|(name, bound)| (name.clone(), bound.hash))
        .collect()
}

/// The manifest beside one entry file — the entry's directory is the
/// package root; a source with no manifest beside it stays a plain
/// script.
pub(crate) fn manifest_beside(entry: &Path) -> Option<PathBuf> {
    let manifest = match entry.parent() {
        Some(parent) => parent.join(MANIFEST_FILE),
        None => PathBuf::from(MANIFEST_FILE),
    };
    manifest.is_file().then_some(manifest)
}

/// The package graph behind one root directory: a manifest at the root
/// resolves exactly like a run beside it, `None` a manifest-free root
/// (the caller's bare-script shape). The `ds test` walk's entry.
///
/// # Errors
///
/// Every refusal [`open`] reports.
pub(crate) fn open_root(root: &Path, no_ambient: bool) -> Result<Option<Graph>> {
    open(&root.join(MANIFEST_FILE), no_ambient)
}

/// The default `@std` bundle's source members, in-process: the tree's
/// own package bytes — the same members the default install serves
/// into the content-addressed store when package resolution demands
/// the name — so a manifest-free resolution needs no store row to
/// serve the same content.
fn default_std_members() -> Vec<Member> {
    members_of(ds_stdlib::STD_PACKAGE_NAME, &ds_stdlib::std_package())
        .expect("the default @std bundle frames its source members")
}

/// The manifest-free run's implicit `@std` identity (the bare-specifier
/// switch): one row, the bundle's composed member identity — the same
/// ascending-path fold a store-resolved declaration records — so the
/// cache keys and the dep-list writer fold what a declaring graph's
/// `[deps]` row folds and an artifact the merged reading wrote never
/// serves.
pub(crate) fn implicit_std_identities() -> BTreeMap<String, Hash128> {
    let identity = ds_stdlib::std_package()
        .member_identity()
        .expect("the default @std bundle folds its member identity");
    BTreeMap::from([(ds_stdlib::STD_PACKAGE_NAME.to_owned(), identity)])
}

/// The bare script's implicit graph (the bare-specifier switch): a
/// manifest-free run resolves the reserved `@std` package ahead of its
/// own compile — the dep implicit, the same discipline a manifest's
/// `[deps]` row buys — so the script session composes the @std bodied
/// members back beside the shrunk core (the bare load-bearing migrated
/// names keep resolving and lowering) while its `@std` imports bind
/// members through the linked surface; a package compile over the same
/// graph registers the shrunk core, where a bare use of a migrated name
/// stays unknown. The bundle's members ride in-process at the composed
/// member identity; a store root never enters (a script runs with no
/// cache at all).
pub(crate) fn implicit_std_graph() -> Graph {
    let members = default_std_members();
    let identity = member_identity(&members);
    Graph {
        members: BTreeMap::from([(ds_stdlib::STD_PACKAGE_NAME.to_owned(), members.into())]),
        identities: BTreeMap::from([(ds_stdlib::STD_PACKAGE_NAME.to_owned(), identity)]),
        artifacts: BTreeMap::new(),
        path_dirs: Vec::new(),
        // The implicit graph never lands an artifact install (its
        // artifact table is empty, so the bound-module walk reads no
        // store path), and Store::open touches nothing: the row rides
        // for the field the resolution path fills, rooted at the
        // process's cache root when one exists and never consulted
        // otherwise.
        store: ds_pkg::store::cache_root()
            .map_or_else(|| Store::open(Path::new("")), |root| Store::open(&root)),
        no_ambient: false,
        linked: RefCell::new(BTreeMap::new()),
        in_progress: RefCell::new(BTreeSet::new()),
    }
}

/// The bundle's source members (the reserved manifest member aside), in
/// ascending path order: one package serves one module composed of
/// every member file (G3), so an empty package refuses — the module has
/// no trees to serve — and each member must read as UTF-8 text.
fn members_of(name: &str, bundle: &Package) -> Result<Vec<Member>> {
    let mut members = Vec::new();
    for (path, bytes) in bundle.members() {
        if path == ds_pkg::MANIFEST_MEMBER {
            continue;
        }
        let text = std::str::from_utf8(bytes)
            .with_context(|| format!("member `{path}` of `{name}` is not UTF-8"))?;
        members.push(Member {
            path: path.clone(),
            text: text.to_owned(),
        });
    }
    if members.is_empty() {
        bail!("package `{name}` carries no source member");
    }
    Ok(members)
}

/// Every .ds source below one root the member grammar admits, in walk
/// order — the ds-pkg member walk (the same walk a path dep's directory
/// package reads), delegating so a root's walked files and a dep's
/// bundle share one walk policy.
///
/// # Errors
///
/// An unreadable root or directory entry.
pub(crate) fn walk_sources(root: &Path) -> Result<Vec<PathBuf>> {
    ds_pkg::walk_sources(root).map_err(|error| anyhow!("{error}"))
}

/// The members one package-root entry carries (G3: one package, one
/// namespace): a manifest-free entry stays the single member the
/// script rule always compiled — the manifest-free run path over the
/// implicit `@std` graph never sees a sibling — while a manifest beside
/// the entry composes the entry member and every sibling `walk_sources`
/// admits below the entry's directory, the same member set `ds save`
/// bundles and a dep-side compile serves. The entry member rides first
/// (its display path keeps the invoked spelling); the siblings follow in
/// ascending bundle-relative path order, each member's path the
/// bundle-relative spelling so the identity fold records the same
/// member set whatever path spelling invoked the entry.
///
/// # Errors
///
/// An unreadable entry, an unreadable root or sibling member, and a
/// sibling that is not UTF-8 text.
pub(crate) fn root_members(entry: &Path) -> Result<Vec<Member>> {
    let text = fs::read_to_string(entry).with_context(|| format!("reading {}", entry.display()))?;
    let path = entry
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let mut members = vec![Member { path, text }];
    if manifest_beside(entry).is_none() {
        return Ok(members);
    }
    let root = entry
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut siblings = Vec::new();
    for walked in walk_sources(root)? {
        let relative = walked
            .strip_prefix(root)
            .expect("the walk started at the root");
        let path = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        if path == members[0].path {
            continue;
        }
        let text =
            fs::read_to_string(&walked).with_context(|| format!("reading {}", walked.display()))?;
        siblings.push(Member { path, text });
    }
    siblings.sort_by(|left, right| left.path.cmp(&right.path));
    members.append(&mut siblings);
    Ok(members)
}

/// The members one `ds test` entry composes: the test file as the script
/// entry beside the package's own members — the walk never lists
/// `tests/` — so the test reads the package's declarations in the one
/// namespace (G3) while every sibling test stays an independent entry.
/// A manifest-free root composes the test file alone, the bare-script
/// shape. The entry rides first; the walked members follow in ascending
/// bundle-relative path order, the identity fold deterministic.
///
/// # Errors
///
/// An unreadable test file, root, or member, and a member that is not
/// UTF-8 text.
pub(crate) fn test_members(root: &Path, test: &Path) -> Result<Vec<Member>> {
    let entry_text =
        fs::read_to_string(test).with_context(|| format!("reading {}", test.display()))?;
    let mut members = vec![Member {
        path: relative_path(root, test),
        text: entry_text,
    }];
    if !root.join(MANIFEST_FILE).is_file() {
        return Ok(members);
    }
    let mut walked = Vec::new();
    for member_file in walk_sources(root)? {
        let path = relative_path(root, &member_file);
        let member_text = fs::read_to_string(&member_file)
            .with_context(|| format!("reading {}", member_file.display()))?;
        walked.push(Member {
            path,
            text: member_text,
        });
    }
    walked.sort_by(|left, right| left.path.cmp(&right.path));
    members.append(&mut walked);
    Ok(members)
}

/// The `/`-separated path below the root, the bundle-relative spelling
/// the identity fold and the dep-side renderings record.
fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Reads and parses one manifest, rendering its DS4014 drift diagnostics
/// to stderr the way the source pipeline renders its own.
///
/// # Errors
///
/// An unreadable manifest, and the drift diagnostics the parse reports.
pub(crate) fn read_manifest(path: &Path) -> Result<Manifest> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut files = Files::new();
    let id = files.add_source(path.display().to_string(), &text);
    match Manifest::parse(&text, id) {
        Ok(manifest) => Ok(manifest),
        Err(diags) => {
            for diag in &diags {
                eprint!("{}", render::human(diag, &files));
            }
            bail!(
                "the manifest does not parse ({} diagnostic(s))",
                diags.len()
            )
        },
    }
}

/// The refusal for a resolution the installed store cannot finish: the
/// taxonomy's own message plus where the fill comes from — `ds save` or
/// a registry behind `$DS_REGISTRY` (a local index root or a git remote
/// URL). A registry that answered carries its own taxonomy in the
/// message, and a hard refusal (an index whose served bytes fail
/// verification, a remote the git client cannot clone) names its own
/// specifics already.
fn refuse(error: &ResolveError, remote: bool) -> anyhow::Error {
    match error {
        ResolveError::MissingPackage { .. } | ResolveError::UnsatisfiableBound { .. }
            if !remote =>
        {
            anyhow!(
                "{error}; nothing fetches remotely — `ds save` installs the package into the \
                 store, or set $DS_REGISTRY to a registry index root or a git remote URL"
            )
        },
        _ => anyhow!("{error}"),
    }
}

/// One resolution demand with the reserved `@std` name answered by the
/// default install ahead of the store-backed provider: the toolchain's
/// own package owns the name, so neither an installed row nor a
/// registry's row can ever serve under it. Every other name falls to
/// the provider unchanged.
fn default_std_or<F>(
    store: &Store,
    provider: &mut StoreProvider<'_, F>,
    name: &str,
    ver: &Version,
    pin: Hash128,
) -> Fetched
where
    F: FnMut(&str, &Version) -> ds_pkg::Remote,
{
    if name == ds_stdlib::STD_PACKAGE_NAME {
        return default_std(store, ver);
    }
    provider.fetch(name, ver, pin)
}

/// The default `@std` install answering one `(name, version)` demand:
/// the tree's package serves exactly its own version (a demand above it
/// is the honest unsatisfiable bound naming what the tree offers) and
/// installs into the content-addressed store before it serves, so the
/// ordinary bundle validation, identity flow, and repeat-demand caching
/// all hold over the toolchain's bytes.
fn default_std(store: &Store, ver: &Version) -> Fetched {
    let package = ds_stdlib::std_package();
    let Ok(manifest) = package.manifest(FileId::new(0)) else {
        return Fetched::Refused(
            "the default @std bundle does not parse its own manifest member".to_owned(),
        );
    };
    if &manifest.version != ver {
        return Fetched::Unoffered {
            best: Some(manifest.version),
        };
    }
    let Ok(member_identity) = package.member_identity() else {
        return Fetched::Refused(
            "the default @std bundle does not fold its member identity".to_owned(),
        );
    };
    match store.install(&package) {
        Ok(content_hash) => Fetched::Found(ProvidedPackage {
            manifest,
            content_hash,
            member_identity,
        }),
        Err(error) => Fetched::Refused(format!(
            "the default @std package does not install into the store: {error}"
        )),
    }
}

/// One path demand answered from the directory: the reserved `@std`
/// name never takes the shape (the toolchain's own package owns the
/// name whatever a directory claims), every other name reading through
/// the provider's directory fetch — the source of truth the store only
/// caches beside.
fn path_demand(store: &Store, name: &str, dir: &Path) -> Fetched {
    if name == ds_stdlib::STD_PACKAGE_NAME {
        return Fetched::Refused(format!(
            "`{name}` is the reserved standard-library package name; the toolchain's own package \
             owns it and a path dep cannot take it"
        ));
    }
    fetch_path(store, dir)
}

/// One graph artifact's install: the store answers an installed identity
/// first — a validated library is never fetched again — a miss falls to
/// the registry's content-addressed artifact storage when one is
/// configured, and the fetched bytes install under the pin before they
/// serve. A missing artifact refuses the run before any compile: the
/// declared externs have no library behind them until the bytes land.
///
/// # Errors
///
/// A store read or write the platform refuses, and the missing-artifact
/// refusal naming the artifact, its pin, and where a fill comes from.
fn ensure_artifact(
    store: &Store,
    registry: Option<&ds_pkg::RegistryClient>,
    name: &str,
    artifact: &ds_pkg::ArtifactBound,
) -> Result<()> {
    match store.load_artifact(artifact.hash) {
        Ok(Some(_)) => return Ok(()),
        Ok(None) => {},
        Err(error) => return Err(anyhow!("{error}")),
    }
    let demand = format!(
        "artifact `{name}` {} for {} (blake3:{}) is not installed",
        artifact.ver, artifact.target, artifact.hash
    );
    match registry {
        None => bail!(
            "{demand}; nothing fetches it — set $DS_REGISTRY to a registry whose artifact storage \
             serves the library by its content hash"
        ),
        Some(registry) => match registry.fetch_artifact(artifact.hash) {
            ds_pkg::ArtifactFetch::Found(bytes) => store
                .install_artifact(artifact.hash, &bytes)
                .map_err(|error| anyhow!("{error}")),
            ds_pkg::ArtifactFetch::Absent => {
                bail!("{demand}; the registry holds no library under that identity")
            },
            ds_pkg::ArtifactFetch::Refused(message) => bail!("{demand}; {message}"),
        },
    }
}

impl Graph {
    /// The link-name identities the dep-list writer records: each
    /// resolved name at the source hash of the module the walk serves
    /// for it.
    pub(crate) fn identities(&self) -> &BTreeMap<String, Hash128> {
        &self.identities
    }

    /// Every directory a resolved path dep read from: the transitive
    /// path closure in ascending order — the watch roots a dev loop
    /// adds beside the package tree.
    pub(crate) fn path_dirs(&self) -> &[PathBuf] {
        &self.path_dirs
    }

    /// The graph's folded native artifacts, name-keyed: each declared
    /// library's bound (version, target, content pin) over the artifact
    /// identity the run already installed — the `ds bind` face reads
    /// these and the source-level extern consumption seam will address
    /// them.
    pub(crate) fn artifacts(&self) -> &BTreeMap<String, ds_pkg::ArtifactBound> {
        &self.artifacts
    }

    /// The graph's native-artifact pins the cache keys fold: each
    /// declared library's name over the content identity the run
    /// installed — the artifact map reduced to the pairs the keys fold.
    pub(crate) fn artifact_pins(&self) -> BTreeMap<String, Hash128> {
        artifact_pins_of(&self.artifacts)
    }

    /// The graph's bindable artifacts (H5): each resolved `[artifacts]`
    /// library whose bytes load and enumerate installs under its manifest
    /// name — the native module a bound extern's pool constant addresses
    /// — beside the loads that refused, each with its reason. A library
    /// nothing binds never installs and the run proceeds; a module whose
    /// pool addresses a refused load gets the honest unresolved-callee
    /// refusal, the recorded reason naming the cause.
    pub(crate) fn bound_modules(&self) -> BoundLoads {
        let mut modules = Vec::new();
        let mut refused = Vec::new();
        for (name, artifact) in &self.artifacts {
            let path = self.store.artifact_path(artifact.hash);
            let bound = (|| {
                let library = ds_ffi::Library::open(&path).map_err(|error| {
                    anyhow!(
                        "{error}; the artifact declares the target `{}`, and a library built for \
                         another triple is among the refusals",
                        artifact.target
                    )
                })?;
                let bytes = self
                    .store
                    .load_artifact(artifact.hash)
                    .map_err(|error| anyhow!("{error}"))?
                    .with_context(|| {
                        format!("artifact `{name}` {} is not installed", artifact.ver)
                    })?;
                ds_vm::BoundModule::bind(library, &bytes)
                    .with_context(|| format!("artifact `{name}` does not bind"))
            })();
            match bound {
                Ok(module) => modules.push((name.clone(), module)),
                Err(error) => refused.push((name.clone(), format!("{error:#}"))),
            }
        }
        (modules, refused)
    }

    /// Installs the graph's bindable artifacts into a VM registry (H5) —
    /// the run path's own step, ahead of the module installs whose pool
    /// validation resolves a bound extern's constant against the bound
    /// tables exactly like a platform's served ops. Answers the loads
    /// that refused, each artifact with its reason.
    pub(crate) fn install_bound(
        &self,
        registry: &mut ds_vm::ModuleRegistry,
    ) -> Vec<(String, String)> {
        let (modules, refused) = self.bound_modules();
        for (name, module) in modules {
            registry.install_bound(&name, module);
        }
        refused
    }

    /// Whether the graph resolved the reserved `@std` package: the
    /// library-boundary un-merge's per-graph ambient shape — a declaring
    /// graph compiles over the shrunk ambient core (the migrated members
    /// bind through the linked surface), every other registration the
    /// merged ambient.
    pub(crate) fn declares_std(&self) -> bool {
        self.members.contains_key(ds_stdlib::STD_PACKAGE_NAME)
    }

    /// One linked package's compiled module and exports (G4), memoized by
    /// link name: the package's members compose one module under the link
    /// name at the recorded identity, its own imports validating and
    /// binding against this same graph, dependencies first. A name the
    /// graph never resolved answers `None`. `dep_dsi` is the
    /// linked-member `.dsi` tier the compile persists beside its dsmod
    /// when the script-mode run path asked for it.
    ///
    /// # Errors
    ///
    /// Every pipeline failure the package's compile reports, the
    /// validation refusal of its recorded imports, and the cycle refusal
    /// of a source import that re-links a package already compiling.
    fn linked(
        &self,
        name: &str,
        dep_dsi: Option<&crate::run_cache::RunCache>,
    ) -> Result<Option<Rc<LinkedPackage>>> {
        if let Some(package) = self.linked.borrow().get(name) {
            return Ok(Some(Rc::clone(package)));
        }
        let Some(members) = self.members.get(name) else {
            return Ok(None);
        };
        // The in-progress guard: compiling this package walks its own
        // imports' surfaces through the same `linked`, so a name
        // re-entered here closes a source-import cycle and refuses
        // instead of recursing without bound.
        if !self.in_progress.borrow_mut().insert(name.to_owned()) {
            bail!(
                "cross-package import cycle: `{name}` source-imports itself, directly or through \
                 other linked packages"
            );
        }
        let outcome = self.compile(name, members, dep_dsi);
        self.in_progress.borrow_mut().remove(name);
        let compiled = outcome?;
        let package = Rc::new(LinkedPackage {
            module: compiled.lowered.into_module(),
            members: Rc::clone(members),
            exports: compiled.exports,
            impls: compiled.impls,
        });
        self.linked
            .borrow_mut()
            .insert(name.to_owned(), Rc::clone(&package));
        Ok(Some(package))
    }

    /// One linked package's compile, the guarded body of [`Graph::linked`]:
    /// every fallible step runs inside, so the guard comes off on the
    /// failure path too and a refusal never pins a name as compiling.
    ///
    /// # Errors
    ///
    /// Every pipeline failure the package's compile reports, and the
    /// validation refusal of its recorded imports.
    fn compile(
        &self,
        name: &str,
        members: &[Member],
        dep_dsi: Option<&crate::run_cache::RunCache>,
    ) -> Result<CompiledSource> {
        let identity = self.identities[name];
        let sources: Vec<(String, String)> = members
            .iter()
            .map(|member| (member.path.clone(), member.text.clone()))
            .collect();
        let compiled = source::compile_linked(
            name,
            &sources,
            identity,
            false,
            false,
            self.no_ambient,
            &self.identities,
            Some(self),
            None,
            dep_dsi,
            source::BundleAfter::Drop,
        )?;
        validate_links(name, compiled.lowered.module().deps(), &self.identities)?;
        Ok(compiled)
    }

    /// One load-walk demand. A graph that resolved the `@std` package
    /// serves its compiled module through the ordinary linked path;
    /// otherwise `@std` serves the checked-in ambient artifact — the
    /// merged-ambient reading's demands, the identity the writer records
    /// for the ambient. Any other name serves its linked package's
    /// module, compiled in process under the link name, its own recorded
    /// imports validated against the same graph. A demand nothing
    /// resolves answers `None`, the walk's provider miss. `dep_dsi` is
    /// the linked-member `.dsi` tier the compile persists beside its
    /// dsmod; `None` (the dev watch round) writes the module alone.
    ///
    /// # Errors
    ///
    /// Every pipeline failure the package's compile reports, and the
    /// validation refusal of its recorded imports.
    pub(crate) fn serve(
        &self,
        name: &str,
        dep_dsi: Option<&crate::run_cache::RunCache>,
    ) -> Result<Option<Module>> {
        if name == crate::ambient_dsmod::AMBIENT_MODULE_NAME && !self.members.contains_key(name) {
            // The walk serves the same once-per-process park the splice
            // serves (the identity-validated resident), cloning its own
            // module out — a drifted artifact refuses here with the same
            // miss the splice's callers fall back from.
            let module = crate::ambient_dsmod::resident()
                .context("the checked-in ambient artifact does not verify")?;
            return Ok(Some((*module).clone()));
        }
        Ok(self
            .linked(name, dep_dsi)?
            .map(|package| package.module.clone()))
    }

    /// The import surfaces one compile's trees bind through (G4): one
    /// [`ImportSurface`] per import declaration the graph serves, keyed
    /// by the declaration's span. `@std` keeps the merged-ambient
    /// reading (its selections validate against the compile unit's
    /// merged namespace, the ambient round's behavior); a linked
    /// package's import serves the named members' exported entries with
    /// the link position a call lowering addresses — the positions the
    /// dep-list writer records, so both derive from the same
    /// first-occurrence walk. A `pub import` re-export's origin package
    /// joins the same walk after the source's own names, so a transitive
    /// entry's FuncRef addresses the origin's dep slot; the recorded
    /// order rides the graph for the dep-list writer to reuse. An import
    /// nothing links gains no entry and stays declarative.
    pub(crate) fn surfaces(
        &self,
        files: &mut Files,
        asts: &[std::sync::Arc<ds_parse::Ast>],
        ambient_files: usize,
        dep_dsi: Option<&crate::run_cache::RunCache>,
    ) -> Result<ImportGraph> {
        let mut imports = ImportGraph::new();
        // The first-occurrence link positions: exactly the dep-list
        // writer's walk, so a linked entry's position names the dep slot
        // its FuncRef addresses. The order vector mirrors the map's
        // insertion sequence — the walk the graph records below.
        let mut positions: HashMap<String, u32> = HashMap::new();
        let mut order: Vec<String> = Vec::new();
        for ast in &asts[ambient_files..] {
            for block in &ast.blocks {
                for item in &block.items {
                    let ItemKind::Import(import) = &item.kind else {
                        continue;
                    };
                    let name = source::import_link_name(import);
                    if !positions.contains_key(&name) {
                        let next =
                            u32::try_from(positions.len()).expect("link position count fits u32");
                        positions.insert(name.clone(), next);
                        order.push(name);
                    }
                }
            }
        }
        for ast in &asts[ambient_files..] {
            for block in &ast.blocks {
                for item in &block.items {
                    let ItemKind::Import(import) = &item.kind else {
                        continue;
                    };
                    let surface = self.surface_of(
                        files,
                        import,
                        &mut positions,
                        &mut order,
                        ambient_files,
                        dep_dsi,
                    )?;
                    imports.add(import.span, surface);
                }
            }
        }
        // the run's bound artifacts join the graph (H5): each resolved
        // `[artifacts]` library a walked source's extern binds enumerates
        // into the fold in export order — the table a bound extern's
        // declaration validates against and the VM install binds below.
        // An artifact nothing binds enumerates nothing: a library the
        // host never loads costs the compile its export read never
        let mut binds = BTreeSet::new();
        for ast in &asts[ambient_files..] {
            for block in &ast.blocks {
                for item in &block.items {
                    if let ItemKind::ExternFn(f) = &item.kind
                        && let Some(bind) = &f.bind
                    {
                        binds.insert(bind.name.clone());
                    }
                }
            }
        }
        for name in binds {
            let Some(artifact) = self.artifacts.get(&name) else {
                continue;
            };
            let bytes = self
                .store
                .load_artifact(artifact.hash)
                .map_err(|error| anyhow!("{error}"))?
                .with_context(|| format!("artifact `{name}` {} is not installed", artifact.ver))?;
            let symbols = ds_ffi::enumerate(&bytes)
                .with_context(|| format!("artifact `{name}`'s export tables refuse to read"))?;
            imports.add_artifact(&name, symbols);
        }
        imports.set_link_order(order);
        Ok(imports)
    }

    /// One import declaration's surface: a graph that resolved the `@std`
    /// package serves its linked members (the ambient core member plus
    /// the migrated members' own source — the declarations a declaring
    /// graph's shrunk ambient lacks, binding through the linked surface),
    /// otherwise `@std` keeps the merged-ambient reading (the compile
    /// unit's ambient namespace serves, no graph link), and every other
    /// name serves its linked package's selected members — no
    /// entry-shaped [`ImportSurface`] for an import nothing serves, the
    /// `NoSuchMember` variant the linked package that lacks the named
    /// member, so the refusal names the selector. A re-exported entry
    /// (`pub import` upstream) binds its ORIGIN package's position —
    /// assigned here in first-occurrence order after the source's own
    /// names — so the call lowering addresses the module whose table
    /// holds the body.
    fn surface_of(
        &self,
        files: &mut Files,
        import: &ds_parse::ImportDecl,
        positions: &mut HashMap<String, u32>,
        order: &mut Vec<String>,
        ambient_files: usize,
        dep_dsi: Option<&crate::run_cache::RunCache>,
    ) -> Result<ImportSurface> {
        let name = source::import_link_name(import);
        if name == ds_stdlib::STD_PACKAGE_NAME && !self.members.contains_key(&name) {
            if ambient_files == 0 {
                return Ok(ImportSurface::NoSuchMember {
                    member: "std".into(),
                });
            }
            return Ok(ImportSurface::Merged {
                member: FileId::new(0),
            });
        }
        let Some(position) = positions.get(&name).copied() else {
            return Ok(ImportSurface::NoSuchMember { member: name });
        };
        // The member selector: the at-sigil path's segments after the
        // link stem name member files (G3); a bare path links whole.
        let selector = if import.path.at_sign && import.path.segments.len() > 1 {
            Some(&import.path.segments[1])
        } else {
            None
        };
        let Some(package) = self.linked(&name, dep_dsi)? else {
            return Ok(ImportSurface::NoSuchMember { member: name });
        };
        let mut rebase: HashMap<FileId, FileId> = HashMap::new();
        // The members the selector names: one file's stem, or the whole
        // package when the path links it whole. The package compiles one
        // module either way; the selector narrows the validation grain.
        // Registration order inside the package compile: the ambient
        // first, then each member in the same order — the `@std` package
        // the exception, its member compiled with no ambient ahead of it
        // — so member `at` registered at `at + package_ambient_files`.
        let package_ambient_files = source::package_ambient_files(self.no_ambient, &name);
        let std_pkg = name == ds_stdlib::STD_PACKAGE_NAME;
        let mut selected = false;
        for (at, member) in package.members.iter().enumerate() {
            let stem = member
                .path
                .rsplit_once('.')
                .map_or(member.path.as_str(), |(stem, _)| stem);
            if selector.is_none_or(|selector| stem == selector.name) {
                selected = true;
                let old = FileId::new(
                    u32::try_from(at + package_ambient_files).expect("file count fits u32"),
                );
                let display = format!("@{}/{}", name.trim_start_matches('@'), member.path);
                let new = files.add_source(display, &member.text);
                rebase.insert(old, new);
            }
        }
        if let (false, Some(member)) = (selected, selector) {
            return Ok(ImportSurface::NoSuchMember {
                member: member.name.clone(),
            });
        }
        // The registered members' exports compose the surface: an export
        // whose defining span names a member the selector skipped never
        // enters (a selector's grain; a re-export's defining span IS the
        // `pub import` row's, so it names the member the same way). A
        // re-exported export links its origin package's position — one
        // this walk assigns in first-occurrence order, the position the
        // entry's FuncRef addresses — while a direct member links the
        // imported package itself. The linked module's tag-table rows
        // ride beside them — the shared tag mapping's identity, seeded
        // into the consuming compile's own table at lowering.
        let mut entries = Vec::new();
        for export in &package.exports {
            let old = export.decl_span.file;
            if !rebase.contains_key(&old) {
                continue;
            }
            let origin = export.origin.clone();
            let link = if let Some(link) = origin.as_deref().and_then(|name| positions.get(name)) {
                *link
            } else if let Some(name) = origin.as_deref() {
                let link = u32::try_from(positions.len()).expect("link position count fits u32");
                positions.insert(name.to_owned(), link);
                order.push(name.to_owned());
                link
            } else {
                position
            };
            entries.push(ForeignEntry {
                name: export.name.clone(),
                vis: export.vis,
                kind: match export.kind {
                    ExportKind::Fn => ForeignKind::Fn,
                    ExportKind::Binding => ForeignKind::Binding,
                    ExportKind::Type => ForeignKind::Type,
                },
                generic: export.generic,
                defaulted: export.defaulted,
                decl_span: rebase_span(export.decl_span, &rebase),
                sig: export.sig.as_ref().map(|sig| rebase_sig(sig, &rebase)),
                row: export.row,
                caps: export.caps,
                value: export.value.clone(),
                ty: export.ty.as_ref().map(|fact| match &**fact {
                    // a trait's member rows carry written signatures, so
                    // the rebase walks them like the fn rows' own
                    ds_check::TypeFact::Trait(trait_fact) => {
                        let members = trait_fact
                            .members
                            .iter()
                            .map(|member| ds_check::import::TraitMemberFact {
                                name: member.name.clone(),
                                span: rebase_span(member.span, &rebase),
                                sig: rebase_sig(&member.sig, &rebase),
                                default_func: member.default_func,
                            })
                            .collect();
                        std::sync::Arc::new(ds_check::TypeFact::Trait(
                            ds_check::import::TraitFact { members },
                        ))
                    },
                    _ => std::sync::Arc::clone(fact),
                }),
                link,
                origin,
                func: export.func,
                std: std_pkg,
            });
        }
        // The impl rows ride the surface beside the entries: each
        // member body's index addresses THIS package's module, so the
        // row's link position is the imported package's own — the
        // spans rebased like the entries'
        let impls = package
            .impls
            .iter()
            .map(|fact| ds_check::import::ImplFact {
                trait_name: fact.trait_name.clone(),
                head: fact.head.clone(),
                members: fact.members.clone(),
                span: rebase_span(fact.span, &rebase),
                link: position,
            })
            .collect();
        let tags = package
            .module
            .tags()
            .iter()
            .map(|row| (row.name.clone(), row.id))
            .collect();
        Ok(ImportSurface::Linked(MemberSurface {
            entries,
            tags,
            impls,
        }))
    }
}

/// Reads and compiles one package-shaped root over its member set (G3:
/// one package, one namespace): a manifest-free root stays the single
/// member the script rule always compiled, while a manifest beside the
/// entry composes the entry member and every sibling into one compile
/// unit — the same member walk a dep-side compile rides — under the
/// composed member identity the caches key on. The module name keeps
/// the script rule (the file stem), the first member's display path
/// keeps the invoked entry spelling, and the writer records the graph's
/// identities for the imports the whole member set declares, the
/// recorded list validating against the graph. `script` parses the
/// root's first member as a script — top-level statements folding into
/// the synthesized `main`, the manifest-free run path's rule over the
/// implicit `@std` graph — while a manifest-declared root keeps the
/// member grammar. `prefix_cache` is the caller's prefix tier, keyed on
/// the same dep identities; `None` runs uncached.
///
/// # Errors
///
/// Every pipeline failure [`crate::source::compile`] reports, and the
/// validation refusal of the recorded imports.
pub(crate) fn compile_root(
    file: &Path,
    graph: &Graph,
    script: bool,
    prefix_cache: Option<&crate::dsi_cache::PrefixCache>,
    dep_dsi: Option<&crate::run_cache::RunCache>,
) -> Result<CompiledSource> {
    let members = root_members(file)?;
    let name = source::script_module_name(file)?;
    let identity = member_identity(&members);
    let sources = entry_sources(file, &members);
    let compiled = source::compile_linked(
        &name,
        &sources,
        identity,
        script,
        true,
        graph.no_ambient,
        graph.identities(),
        Some(graph),
        prefix_cache,
        dep_dsi,
        source::BundleAfter::Keep,
    )?;
    validate_links(&name, compiled.lowered.module().deps(), graph.identities())?;
    Ok(compiled)
}

/// The entry member keeps the invoked path as its display; every
/// sibling registers under its bundle-relative path, the spelling a
/// dep-side compile renders the same member under.
fn entry_sources(file: &Path, members: &[Member]) -> Vec<(String, String)> {
    members
        .iter()
        .enumerate()
        .map(|(at, member)| {
            (
                if at == 0 { file.display().to_string() } else { member.path.clone() },
                member.text.clone(),
            )
        })
        .collect()
}

/// The compile one `ds test` entry rides: the composed members (the
/// package's own beside the test entry) through the same package
/// compile a run takes — no script-mode caches, a test run is one-shot
/// — and the same link validation.
///
/// # Errors
///
/// Every refusal the package compile and the link validation report.
pub(crate) fn compile_test(
    file: &Path,
    members: &[Member],
    graph: &Graph,
) -> Result<CompiledSource> {
    let name = source::script_module_name(file)?;
    let identity = member_identity(members);
    let compiled = source::compile_linked(
        &name,
        &entry_sources(file, members),
        identity,
        true,
        true,
        graph.no_ambient,
        graph.identities(),
        Some(graph),
        None,
        None,
        source::BundleAfter::Keep,
    )?;
    validate_links(&name, compiled.lowered.module().deps(), graph.identities())?;
    Ok(compiled)
}

/// The recorded imports of one compiled package module validate against
/// the resolved graph: every linked name is the module itself (a
/// self-named entry stays local), the ambient, or a package the graph
/// resolved. Anything else — an undeclared package, a bare path the
/// manifest never names — refuses; resolution demands only what
/// `[deps]` declares.
///
/// # Errors
///
/// The first linked name that resolves nowhere in the graph.
pub(crate) fn validate_links(
    module: &str,
    deps: &[ds_mod::Dep],
    identities: &BTreeMap<String, Hash128>,
) -> Result<()> {
    for dep in deps {
        if dep.name == module
            || dep.name == crate::ambient_dsmod::AMBIENT_MODULE_NAME
            || identities.contains_key(&dep.name)
        {
            continue;
        }
        bail!(
            "import `{}` links no package the manifest graph resolves; declare it under [deps] in \
             ds.pkg",
            dep.name
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use ds_mod::Hash128;

    use super::{Member, member_identity};

    fn member(path: &str, text: &str) -> Member {
        Member {
            path: path.to_owned(),
            text: text.to_owned(),
        }
    }

    /// The composed identity is the ascending-path fold: byte-stable
    /// across member order, moved by a path or text edit, and a single
    /// member keeps the landed single-source identity, its text hash.
    #[test]
    fn the_multi_member_identity_is_the_ascending_path_fold() {
        let a = member("a.ds", "one");
        let b = member("b.ds", "two");
        let composed = member_identity(&[b.clone(), a.clone()]);
        assert_eq!(
            composed,
            member_identity(&[a.clone(), b.clone()]),
            "the fold is order-independent"
        );
        assert_ne!(
            composed,
            member_identity(&[a.clone(), member("b.ds", "two ")])
        );
        assert_ne!(
            composed,
            member_identity(&[member("a.ds", "one"), member("c.ds", "two")])
        );
        assert_eq!(
            member_identity(&[member("only.ds", "text")]),
            Hash128::compute(b"text")
        );
    }

    /// The light key entry answers the heavy graph's own folds over a
    /// two-package tree — the dep installed in the store, the root
    /// manifest pinning it — so the identities and pins the cache keys
    /// fold are the same values either entry computes, and the dep's
    /// two members land the composed ascending-path fold through the
    /// seam.
    #[test]
    fn the_light_key_entry_answers_the_heavy_graph_folds() {
        let work =
            std::env::temp_dir().join(format!("ds-cli-package-light-{}", std::process::id()));
        let cache = work.join("cache");
        let root = work.join("root");
        std::fs::create_dir_all(&cache).expect("the private cache root creates");
        std::fs::create_dir_all(&root).expect("the package root creates");
        // The cache-root environment is test-local: the runner gives
        // each test its own process, and no other unit test reads it.
        let prior_cache = std::env::var_os("DS_CACHE");
        unsafe {
            std::env::set_var("DS_CACHE", &cache);
            std::env::remove_var("DS_REGISTRY");
        }
        let dep_manifest_text = "name = \"@dep\"\nversion = \"1.0.0\"\n";
        let manifest = ds_pkg::Manifest::parse(dep_manifest_text, ds_diag::FileId::new(0))
            .expect("the fixture manifest parses");
        let member = "pub fn base() -> int {\n21\n}\n";
        let sibling = "pub fn extra() -> int {\n1\n}\n";
        let bundle = ds_pkg::Package::new(&manifest, [
            ("dep.ds".to_owned(), member.as_bytes().to_vec()),
            ("extra.ds".to_owned(), sibling.as_bytes().to_vec()),
        ])
        .expect("the fixture bundle builds");
        let hash = ds_pkg::Store::open(&cache)
            .install(&bundle)
            .expect("the dep installs");
        std::fs::write(
            root.join(super::MANIFEST_FILE),
            format!(
                "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@dep = {{ ver = \"1.0.0\", hash = \
                 \"blake3:{hash}\" }}\n"
            ),
        )
        .expect("the root manifest writes");
        std::fs::write(
            root.join("main.ds"),
            "import @dep (base)\n\nfn main() -> int {\n21\n}\n",
        )
        .expect("the entry writes");
        let entry = root.join("main.ds");
        let heavy = super::open(&entry, false)
            .expect("the heavy entry resolves")
            .expect("the root carries a manifest");
        let light = super::resolve_identities(&entry)
            .expect("the light entry resolves")
            .expect("the root carries a manifest");
        // restore the environment ahead of the assertions, so a failure
        // leaves it untouched
        unsafe {
            match prior_cache {
                Some(value) => std::env::set_var("DS_CACHE", value),
                None => std::env::remove_var("DS_CACHE"),
            }
        }
        std::fs::remove_dir_all(&work).expect("the work dir removes");
        assert_eq!(
            light.identities,
            *heavy.identities(),
            "one fold, two entries"
        );
        assert_eq!(
            light.artifact_pins,
            heavy.artifact_pins(),
            "one pin fold, two entries"
        );
        let dep = light.identities.get("@dep").expect("the dep resolved");
        assert_eq!(
            *dep,
            ds_pkg::member_identity([
                ("dep.ds", member.as_bytes()),
                ("extra.ds", sibling.as_bytes()),
            ])
            .expect("the fixture members fold")
        );
    }

    /// The resolution fingerprint keys the consumed inputs' content: a
    /// manifest-free entry answers `None`, a root's fingerprint stands
    /// while the inputs hold unchanged, and it moves with a manifest
    /// edit (the dep repin) and with a path dep's source edit — nested
    /// path edges included, never an mtime.
    #[test]
    fn the_fingerprint_moves_with_the_consumed_inputs() {
        let work =
            std::env::temp_dir().join(format!("ds-cli-package-fingerprint-{}", std::process::id()));
        let root = work.join("root");
        let lib = root.join("lib");
        std::fs::create_dir_all(&lib).expect("the package dirs create");
        std::fs::write(
            lib.join(super::MANIFEST_FILE),
            "name = \"@lib\"\nversion = \"1.0.0\"\n",
        )
        .expect("the dep manifest writes");
        std::fs::write(lib.join("lib.ds"), "pub fn one() -> int {\n1\n}\n")
            .expect("the dep member writes");
        let root_manifest = "name = \"app\"\nversion = \"0.1.0\"\n\n[deps.@lib]\npath = \"lib\"\n";
        std::fs::write(root.join(super::MANIFEST_FILE), root_manifest)
            .expect("the root manifest writes");
        std::fs::write(root.join("main.ds"), "fn main() -> int {\n1\n}\n")
            .expect("the entry writes");
        let free = std::env::temp_dir().join(format!(
            "ds-cli-package-fingerprint-free-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&free).expect("the plain dir creates");
        let entry = root.join("main.ds");
        assert_eq!(
            super::resolution_fingerprint(&free.join("main.ds")),
            None,
            "a manifest-free entry answers nothing"
        );
        let before = super::resolution_fingerprint(&entry).expect("the root carries a manifest");
        assert_eq!(
            super::resolution_fingerprint(&entry),
            Some(before),
            "the unchanged inputs fold the same fingerprint"
        );
        std::fs::write(
            root.join(super::MANIFEST_FILE),
            "name = \"app\"\nversion = \"0.2.0\"\n\n[deps.@lib]\npath = \"lib\"\n",
        )
        .expect("the repinned manifest writes");
        let repinned = super::resolution_fingerprint(&entry).expect("the root carries a manifest");
        assert_ne!(repinned, before, "the manifest edit moves the fingerprint");
        std::fs::write(lib.join("lib.ds"), "pub fn one() -> int {\n2\n}\n")
            .expect("the dep member rewrites");
        let edited = super::resolution_fingerprint(&entry).expect("the root carries a manifest");
        assert_ne!(
            edited, repinned,
            "the path dep's source edit moves the fingerprint"
        );
        std::fs::remove_dir_all(&work).expect("the work dir removes");
        std::fs::remove_dir_all(&free).expect("the free dir removes");
    }
}
