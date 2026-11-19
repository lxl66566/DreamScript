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
//! writer so the artifacts pin the content they link.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
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
    Fetched, Manifest, Package, ProvidedPackage, ResolveError, Store, StoreProvider, Version,
    resolve,
};

use crate::source::{self, CompiledSource, Export, ExportKind, rebase_sig, rebase_span};

/// The manifest's file name inside a package root.
pub(crate) const MANIFEST_FILE: &str = "ds.pkg";

/// One resolved package's member file: the member's path inside the
/// package (the display path diagnostics render) and its text, in
/// ascending path order.
#[derive(Clone)]
struct Member {
    path: String,
    text: String,
}

/// The member identity the dep-list writer records and the walk
/// validates (G3.5): a single member's source hash, or the composed fold
/// over every member in ascending path order — one length-prefixed path
/// and text pair per member, so the composition is byte-stable and a
/// member rename or text edit changes it.
fn member_identity(members: &[Member]) -> Hash128 {
    let [one] = members else {
        let mut ordered: Vec<&Member> = members.iter().collect();
        ordered.sort_by(|left, right| left.path.cmp(&right.path));
        let mut buffer = Vec::new();
        for member in ordered {
            buffer.extend_from_slice(&(member.path.len() as u64).to_le_bytes());
            buffer.extend_from_slice(member.path.as_bytes());
            buffer.extend_from_slice(&(member.text.len() as u64).to_le_bytes());
            buffer.extend_from_slice(member.text.as_bytes());
        }
        return Hash128::compute(&buffer);
    };
    Hash128::compute(one.text.as_bytes())
}

/// One linked package's compiled module and member surfaces (G4): the
/// module the load walk installs, and the exports a dependent's imports
/// validate against, in the package compile's own file space.
struct LinkedPackage {
    module: Module,
    members: Vec<Member>,
    exports: Vec<Export>,
}

/// The graph one package run serves from: each resolved dependency's
/// member files, keyed by link name, the member identities the dep-list
/// writer records, the graph's native artifacts (installed
/// content-addressed ahead of the walk), and the compiled packages those
/// identities pin, memoized by link name so a package the walk and the
/// surface builder both demand compiles once.
pub(crate) struct Graph {
    members: BTreeMap<String, Vec<Member>>,
    identities: BTreeMap<String, Hash128>,
    artifacts: BTreeMap<String, ds_pkg::ArtifactBound>,
    no_ambient: bool,
    linked: RefCell<BTreeMap<String, Rc<LinkedPackage>>>,
    /// The link names currently compiling: a source-import cycle (a
    /// package importing itself or a chain of packages importing each
    /// other — edges the manifest `[deps]` graph's cycle refusal never
    /// sees) re-enters `linked` for a name whose memo entry does not
    /// exist yet, so this set is the recursion's only guard.
    in_progress: RefCell<BTreeSet<String>>,
}

/// The package run `entry` shapes, when a manifest sits beside it:
/// `None` is the plain script the existing paths serve unchanged.
///
/// # Errors
///
/// An unreadable or drifting manifest, an unresolvable cache root, and
/// every resolution refusal the store's own knowledge reports.
pub(crate) fn open(entry: &Path, no_ambient: bool) -> Result<Option<Graph>> {
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
        resolve(&manifest, |name, ver| {
            default_std_or(&store, &mut provider, name, ver)
        })
        .map_err(|error| refuse(&error, true))?
    } else {
        let mut provider = StoreProvider::cached(&store);
        resolve(&manifest, |name, ver| {
            default_std_or(&store, &mut provider, name, ver)
        })
        .map_err(|error| refuse(&error, false))?
    };
    // the graph's native artifacts install before the walk serves: each
    // declared library validated against its content pin and landing in
    // the store's artifact segment, a registry's storage filling misses
    // — the `[artifacts]` entries are dependencies like `[deps]`, an
    // unserved one an unsatisfied demand
    for (name, artifact) in resolved.artifacts() {
        ensure_artifact(&store, registry.as_ref(), name, artifact)?;
    }
    let mut members = BTreeMap::new();
    let mut identities = BTreeMap::new();
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
        identities.insert(name.clone(), member_identity(&package_members));
        members.insert(name.clone(), package_members);
    }
    Ok(Some(Graph {
        members,
        identities,
        artifacts: resolved.artifacts().clone(),
        no_ambient,
        linked: RefCell::new(BTreeMap::new()),
        in_progress: RefCell::new(BTreeSet::new()),
    }))
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
/// order: real subdirectories recurse while directory links (junctions
/// and symlinks) never expand — the entry's own metadata decides, so a
/// link can neither loop the walk back into the tree nor pull
/// directories in from outside it — and a .ds link joins the walk only
/// when its canonical target stays a file inside the root. The member
/// set is therefore a deterministic function of the worktree, and a
/// canonical visited set backs the same real directory walking twice.
///
/// # Errors
///
/// An unreadable root or directory entry.
pub(crate) fn walk_sources(root: &Path) -> Result<Vec<PathBuf>> {
    let inside = root
        .canonicalize()
        .with_context(|| format!("reading {}", root.display()))?;
    let mut visited = HashSet::new();
    let mut sources = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        // The visited key is the directory's canonical form, so the
        // backstop holds whatever spelling the walk reached it by.
        let key = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        if !visited.insert(key) {
            continue;
        }
        for entry in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
            let entry = entry.with_context(|| format!("reading {}", dir.display()))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .with_context(|| format!("reading {}", path.display()))?;
            // The link's own metadata (no follow): a junction or a
            // directory symlink is not `is_dir` here, so it never
            // enters the stack and its target never doubles the walk.
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("ds") {
                continue;
            }
            if file_type.is_symlink() && !link_resolves_inside(&path, &inside) {
                continue;
            }
            sources.push(path);
        }
    }
    Ok(sources)
}

/// Whether a .ds link's canonical target is a file below `inside`.
fn link_resolves_inside(link: &Path, inside: &Path) -> bool {
    link.canonicalize()
        .is_ok_and(|target| target.is_file() && target.strip_prefix(inside).is_ok())
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
) -> Fetched
where
    F: FnMut(&str, &Version) -> ds_pkg::Remote,
{
    if name == ds_stdlib::STD_PACKAGE_NAME {
        return default_std(store, ver);
    }
    provider.fetch(name, ver)
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
    match store.install(&package) {
        Ok(content_hash) => Fetched::Found(ProvidedPackage {
            manifest,
            content_hash,
        }),
        Err(error) => Fetched::Refused(format!(
            "the default @std package does not install into the store: {error}"
        )),
    }
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

    /// The graph's folded native artifacts, name-keyed: each declared
    /// library's bound (version, target, content pin) over the artifact
    /// identity the run already installed — the `ds bind` face reads
    /// these and the source-level extern consumption seam will address
    /// them.
    pub(crate) fn artifacts(&self) -> &BTreeMap<String, ds_pkg::ArtifactBound> {
        &self.artifacts
    }

    /// One linked package's compiled module and exports (G4), memoized by
    /// link name: the package's members compose one module under the link
    /// name at the recorded identity, its own imports validating and
    /// binding against this same graph, dependencies first. A name the
    /// graph never resolved answers `None`.
    ///
    /// # Errors
    ///
    /// Every pipeline failure the package's compile reports, the
    /// validation refusal of its recorded imports, and the cycle refusal
    /// of a source import that re-links a package already compiling.
    fn linked(&self, name: &str) -> Result<Option<Rc<LinkedPackage>>> {
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
        let outcome = self.compile(name, members);
        self.in_progress.borrow_mut().remove(name);
        let compiled = outcome?;
        let package = Rc::new(LinkedPackage {
            module: compiled.lowered.into_module(),
            members: members.clone(),
            exports: compiled.exports,
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
    fn compile(&self, name: &str, members: &[Member]) -> Result<CompiledSource> {
        let identity = self.identities[name];
        let sources: Vec<(String, String)> = members
            .iter()
            .map(|member| (member.path.clone(), member.text.clone()))
            .collect();
        let compiled = source::compile_linked(
            name,
            &sources,
            identity,
            "main",
            false,
            self.no_ambient,
            &self.identities,
            Some(self),
            None,
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
    /// resolves answers `None`, the walk's provider miss.
    ///
    /// # Errors
    ///
    /// Every pipeline failure the package's compile reports, and the
    /// validation refusal of its recorded imports.
    pub(crate) fn serve(&self, name: &str) -> Result<Option<Module>> {
        if name == crate::ambient_dsmod::AMBIENT_MODULE_NAME && !self.members.contains_key(name) {
            let module = ds_mod::read(&ds_stdlib::AMBIENT_DSMOD[crate::ambient_dsmod::HEADER..])
                .context("the checked-in ambient artifact does not read")?;
            return Ok(Some(module));
        }
        Ok(self.linked(name)?.map(|package| package.module.clone()))
    }

    /// The import surfaces one compile's trees bind through (G4): one
    /// [`ImportSurface`] per import declaration the graph serves, keyed
    /// by the declaration's span. `@std` keeps the merged-ambient
    /// reading (its selections validate against the compile unit's
    /// merged namespace, the ambient round's behavior); a linked
    /// package's import serves the named members' exported entries with
    /// the link position a call lowering addresses — the positions the
    /// dep-list writer records, so both derive from the same
    /// first-occurrence walk. An import nothing links gains no entry
    /// and stays declarative.
    pub(crate) fn surfaces(
        &self,
        files: &mut Files,
        asts: &[std::sync::Arc<ds_parse::Ast>],
        ambient_files: usize,
    ) -> Result<ImportGraph> {
        let mut imports = ImportGraph::new();
        // The first-occurrence link positions: exactly the dep-list
        // writer's walk, so a linked entry's position names the dep slot
        // its FuncRef addresses.
        let mut positions: HashMap<String, u32> = HashMap::new();
        for ast in &asts[ambient_files..] {
            for block in &ast.blocks {
                for item in &block.items {
                    let ItemKind::Import(import) = &item.kind else {
                        continue;
                    };
                    let name = source::import_link_name(import);
                    let next =
                        u32::try_from(positions.len()).expect("link position count fits u32");
                    positions.entry(name).or_insert(next);
                }
            }
        }
        for ast in &asts[ambient_files..] {
            for block in &ast.blocks {
                for item in &block.items {
                    let ItemKind::Import(import) = &item.kind else {
                        continue;
                    };
                    let surface = self.surface_of(files, import, &positions, ambient_files)?;
                    imports.add(import.span, surface);
                }
            }
        }
        Ok(imports)
    }

    /// One import declaration's surface: a graph that resolved the `@std`
    /// package serves its linked members, otherwise `@std` keeps the
    /// merged-ambient reading (the compile unit's ambient namespace
    /// serves, no graph link), and every other name serves its linked
    /// package's selected members — no entry-shaped [`ImportSurface`]
    /// for an import nothing serves, the `NoSuchMember` variant the
    /// linked package that lacks the named member, so the refusal names
    /// the selector.
    fn surface_of(
        &self,
        files: &mut Files,
        import: &ds_parse::ImportDecl,
        positions: &HashMap<String, u32>,
        ambient_files: usize,
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
        let Some(package) = self.linked(&name)? else {
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
        // enters (a selector's grain). The linked module's tag-table
        // rows ride beside them — the shared tag mapping's identity,
        // seeded into the consuming compile's own table at lowering.
        let mut entries = Vec::new();
        for export in &package.exports {
            let old = export.decl_span.file;
            if !rebase.contains_key(&old) {
                continue;
            }
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
                ty: export.ty.clone(),
                link: position,
                func: export.func,
                std: std_pkg,
            });
        }
        let tags = package
            .module
            .tags()
            .iter()
            .map(|row| (row.name.clone(), row.id))
            .collect();
        Ok(ImportSurface::Linked(MemberSurface { entries, tags }))
    }
}

/// Reads and compiles one package-shaped root: the module name keeps the
/// script rule (the file stem), the writer records the graph's
/// identities for the source's imports, and the recorded list validates
/// against the graph. `prefix_cache` is the caller's prefix tier, keyed
/// on the same dep identities; `None` runs uncached.
///
/// # Errors
///
/// Every pipeline failure [`crate::source::compile`] reports, and the
/// validation refusal of the recorded imports.
pub(crate) fn compile_root(
    file: &Path,
    entry: &str,
    graph: &Graph,
    prefix_cache: Option<&crate::dsi_cache::PrefixCache>,
) -> Result<CompiledSource> {
    let source = fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    let name = source::script_module_name(file)?;
    let compiled = source::compile_package(
        &name,
        &file.display().to_string(),
        &source,
        entry,
        graph.no_ambient,
        graph.identities(),
        Some(graph),
        prefix_cache,
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
}
