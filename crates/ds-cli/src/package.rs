//! The package-run wiring behind `ds run` and `ds build`: a `ds.pkg`
//! beside the entry file turns the subcommand over that source into a
//! package run. The manifest resolves over the store's provider seam —
//! no remote hook, so a demand nothing installed answers refuses naming
//! the registry round — and the deps-first load walk serves the graph:
//! `@std` from the checked-in ambient artifact, every other linked name
//! from its package's single source member compiled in-process under the
//! package's link name, the graph's identities feeding the dep-list
//! writer so the artifacts pin the content they link.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use ds_diag::{Files, render};
use ds_mod::{Hash128, Module};
use ds_pkg::{Manifest, Package, ResolveError, Store, StoreProvider, resolve};

use crate::source::{self, CompiledSource};

/// The manifest's file name inside a package root.
pub(crate) const MANIFEST_FILE: &str = "ds.pkg";

/// One resolved package's single source member: the member's path inside
/// the package (the display path diagnostics render) and its text, whose
/// hash is the identity the dep-list writer records and the walk
/// validates.
struct Member {
    path: String,
    text: String,
}

/// The graph one package run serves from: each resolved dependency's
/// single source member, keyed by link name, plus the member identities
/// the dep-list writer records.
pub(crate) struct Graph {
    members: BTreeMap<String, Member>,
    identities: BTreeMap<String, Hash128>,
    no_ambient: bool,
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
    let Some(root) = ds_pkg::store::cache_root() else {
        bail!(
            "no cache root resolves ($DS_CACHE unset, no user cache directory); a package run \\
             needs the package store below it"
        );
    };
    let store = Store::open(&root);
    // no remote hook: the installed rows answer, the store's own
    // knowledge carrying the unoffered/unknown taxonomy
    let mut provider = StoreProvider::cached(&store);
    let resolved = resolve(&manifest, |name, ver| provider.fetch(name, ver))
        .map_err(|error| refuse(&error))?;
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
        let member = single_member(name, &bundle)?;
        identities.insert(name.clone(), Hash128::compute(member.text.as_bytes()));
        members.insert(name.clone(), member);
    }
    Ok(Some(Graph {
        members,
        identities,
        no_ambient,
    }))
}

/// The manifest beside one entry file — the entry's directory is the
/// package root; a source with no manifest beside it stays a plain
/// script.
fn manifest_beside(entry: &Path) -> Option<PathBuf> {
    let manifest = match entry.parent() {
        Some(parent) => parent.join(MANIFEST_FILE),
        None => PathBuf::from(MANIFEST_FILE),
    };
    manifest.is_file().then_some(manifest)
}

/// The bundle's one source member (the reserved manifest member aside):
/// a package serves one module this round, so any other member count
/// refuses — the multi-member model lands with the manifest's
/// `[build]` section.
fn single_member(name: &str, bundle: &Package) -> Result<Member> {
    let sources: Vec<(&String, &[u8])> = bundle
        .members()
        .iter()
        .filter(|(path, _)| path.as_str() != ds_pkg::MANIFEST_MEMBER)
        .map(|(path, bytes)| (path, bytes.as_slice()))
        .collect();
    let &[(path, bytes)] = sources.as_slice() else {
        bail!(
            "package `{name}` carries {} source members; a package serves one module this round",
            sources.len()
        );
    };
    let text = std::str::from_utf8(bytes)
        .with_context(|| format!("member `{path}` of `{name}` is not UTF-8"))?;
    Ok(Member {
        path: path.clone(),
        text: text.to_owned(),
    })
}

/// Reads and parses one manifest, rendering its DS4014 drift diagnostics
/// to stderr the way the source pipeline renders its own.
///
/// # Errors
///
/// An unreadable manifest, and the drift diagnostics the parse reports.
pub(crate) fn read_manifest(path: &Path) -> Result<Manifest> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
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

/// The refusal for a resolution the store alone cannot finish: the
/// taxonomy's own message plus where the fill comes from — nothing
/// fetches remotely this round.
fn refuse(error: &ResolveError) -> anyhow::Error {
    match error {
        ResolveError::MissingPackage { .. } | ResolveError::UnsatisfiableBound { .. } => anyhow!(
            "{error}; nothing fetches remotely this round — `ds save` installs the package \\
             into the store, and the registry round's remote fetch fills the rest"
        ),
        _ => anyhow!("{error}"),
    }
}

impl Graph {
    /// The link-name identities the dep-list writer records: each
    /// resolved name at the source hash of the module the walk serves
    /// for it.
    pub(crate) fn identities(&self) -> &BTreeMap<String, Hash128> {
        &self.identities
    }

    /// One load-walk demand. `@std` serves the checked-in ambient
    /// artifact — the identity the writer records for the ambient — and
    /// any other name compiles its member in-process under the link
    /// name, its own recorded imports validating against the same
    /// graph. A demand nothing resolves answers `None`, the walk's
    /// provider miss.
    ///
    /// # Errors
    ///
    /// Every pipeline failure the member's compile reports, and the
    /// validation refusal of its recorded imports.
    pub(crate) fn serve(&self, name: &str) -> Result<Option<Module>> {
        if name == crate::ambient_dsmod::AMBIENT_MODULE_NAME {
            let module = ds_mod::read(&ds_stdlib::AMBIENT_DSMOD[crate::ambient_dsmod::HEADER..])
                .context("the checked-in ambient artifact does not read")?;
            return Ok(Some(module));
        }
        let Some(member) = self.members.get(name) else {
            return Ok(None);
        };
        let compiled = source::compile_member(
            name,
            &member.path,
            &member.text,
            self.no_ambient,
            &self.identities,
        )?;
        validate_links(name, compiled.lowered.module().deps(), &self.identities)?;
        Ok(Some(compiled.lowered.into_module()))
    }
}

/// Reads and compiles one package-shaped root: the module name keeps the
/// script rule (the file stem), the writer records the graph's
/// identities for the source's imports, and the recorded list validates
/// against the graph.
///
/// # Errors
///
/// Every pipeline failure [`crate::source::compile`] reports, and the
/// validation refusal of the recorded imports.
pub(crate) fn compile_root(file: &Path, entry: &str, graph: &Graph) -> Result<CompiledSource> {
    let source =
        std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    let name = source::script_module_name(file)?;
    let compiled = source::compile_package(
        &name,
        &file.display().to_string(),
        &source,
        entry,
        graph.no_ambient,
        graph.identities(),
    )?;
    validate_links(&name, compiled.lowered.module().deps(), graph.identities())?;
    Ok(compiled)
}

/// The recorded imports of one compiled package module validate against
/// the resolved graph: every linked name is the module itself (a
/// self-named entry stays local), the ambient, or a package the graph
/// resolved. Anything else — an undeclared package, a bare path the
/// manifest never names — refuses; undeclared names fetch with the
/// registry round.
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
            "import `{}` links no package the manifest graph resolves; declare it under [deps] \\
             in ds.pkg (undeclared names fetch with the registry round)",
            dep.name
        );
    }
    Ok(())
}
