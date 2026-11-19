//! The store-backed provider seam: resolution's fetch interface
//! answered from the content-addressed cache, misses filled by a
//! caller-supplied remote fetch hook. The net natives stay out of
//! scope here — the platform's tls capability serves https, but the
//! remote side stays abstract: the registry client serves the hook —
//! a local index root or a git remote's snapshot — the verified
//! bundles installing through the store either way.

use std::collections::BTreeMap;

use ds_diag::FileId;
use ds_mod::Hash128;

use crate::{
    manifest::Manifest,
    package::Package,
    resolve::{Fetched, ProvidedPackage},
    store::Store,
    version::Version,
};

/// The remote fetch hook's answer for one `(name, version)` demand: the
/// bundle the remote serves for the pair, or the taxonomy of its
/// absence.
#[derive(Clone, Debug)]
pub enum Remote {
    /// The bundle for exactly this pair. The manifest member
    /// self-describes the name and version, and resolution validates
    /// both plus the winning declarer's pin — the hook is never trusted
    /// with the identity bookkeeping.
    Found(Package),
    /// No package under the name at all.
    UnknownName,
    /// The name exists but the exact version is unoffered; `best` is the
    /// remote's highest offered version of the name (`None`: the
    /// listing is empty), carried for the diagnostic.
    Unoffered {
        /// The remote's highest offered version of the name.
        best: Option<Version>,
    },
    /// The fetch arrived but failed its own verification, or the
    /// remote's storage could not be read: a hard refusal, not a
    /// taxonomy answer — the message names what failed and resolution
    /// carries it verbatim.
    Refused(String),
}

/// The no-remote constructor's hook slot: a fn-pointer type held empty,
/// so a cache-only provider needs no hook closure.
type NoRemote = fn(&str, &Version) -> Remote;

/// The demand index: the store's installed rows keyed by the manifest's
/// self-described `(name, version)`, each identity carrying its manifest.
/// A `(name, version)` self-described at two identities is registry
/// drift; the lowest identity serves deterministically and resolution's
/// pin validation refuses a wrong pick.
type Index = BTreeMap<(String, Version), BTreeMap<Hash128, Manifest>>;

/// The provider over the store: every demand answers from the installed
/// rows first — a valid bundle is never fetched again, with or without a
/// remote — and a miss falls to the remote hook, whose fetched content
/// installs under its own identity before it serves. Without a hook the
/// cache alone answers, its own knowledge setting the taxonomy (a name
/// the store has installed at other versions reports unoffered with its
/// highest, an unseen name unknown).
pub struct StoreProvider<'store, F> {
    store: &'store Store,
    hook: Option<F>,
    /// The store's rows, scanned lazily at the first demand and
    /// extended by every install this provider lands.
    index: Option<Index>,
}

impl<'store> StoreProvider<'store, NoRemote> {
    /// A cache-only provider: the installed rows answer or the demand
    /// refuses.
    #[must_use]
    pub fn cached(store: &'store Store) -> Self {
        Self {
            store,
            hook: None,
            index: None,
        }
    }
}

impl<'store, F> StoreProvider<'store, F>
where
    F: FnMut(&str, &Version) -> Remote,
{
    /// A provider whose misses fall to `hook`.
    #[must_use]
    pub fn with_remote(store: &'store Store, hook: F) -> Self {
        Self {
            store,
            hook: Some(hook),
            index: None,
        }
    }

    /// One resolve-walk demand: the installed rows answer first, a miss
    /// falls to the remote hook, and a fetched bundle installs before it
    /// serves so a repeat demand never refetches.
    pub fn fetch(&mut self, name: &str, version: &Version) -> Fetched {
        self.ensure_index();
        let index = self.index.as_ref().expect("the index is built");
        if let Some(candidates) = index.get(&(name.to_owned(), *version)) {
            let (identity, manifest) = candidates
                .iter()
                .next()
                .expect("a key holds at least one identity");
            return Fetched::Found(ProvidedPackage {
                manifest: manifest.clone(),
                content_hash: *identity,
            });
        }
        let Some(hook) = self.hook.as_mut() else {
            return self.unserved(name);
        };
        match hook(name, version) {
            Remote::Found(package) => self.serve_fetched(name, &package),
            Remote::UnknownName => Fetched::UnknownName,
            Remote::Unoffered { best } => Fetched::Unoffered { best },
            Remote::Refused(message) => Fetched::Refused(message),
        }
    }

    /// Builds the demand index from the store's self-described rows if
    /// no demand built it yet.
    fn ensure_index(&mut self) {
        if self.index.is_some() {
            return;
        }
        let mut index: Index = BTreeMap::new();
        for row in self.store.rows() {
            index
                .entry((row.manifest.name.clone(), row.manifest.version))
                .or_default()
                .insert(row.identity, row.manifest);
        }
        self.index = Some(index);
    }

    /// The taxonomy of a demand nothing serves: a name the store has
    /// installed at other versions is unoffered with its highest, an
    /// unseen name unknown.
    fn unserved(&self, name: &str) -> Fetched {
        let index = self.index.as_ref().expect("the index is built");
        let best = index
            .keys()
            .filter(|(seen, _)| seen == name)
            .map(|(_, version)| *version)
            .next_back();
        match best {
            Some(best) => Fetched::Unoffered { best: Some(best) },
            None => Fetched::UnknownName,
        }
    }

    /// A hook-fetched bundle: the manifest must self-describe through
    /// the reserved member (a bundle that does not parse serves
    /// nothing), the content installs under its own identity — an
    /// install failure degrades to serving without caching, never a
    /// wrong answer — and the row joins the index under the manifest's
    /// own name and version, so the content the bundle is serves later
    /// demands whatever name the fetch was asked under.
    fn serve_fetched(&mut self, name: &str, package: &Package) -> Fetched {
        let Ok(manifest) = package.manifest(FileId::new(0)) else {
            return self.unserved(name);
        };
        let identity = package.content_hash();
        if self.store.install(package).is_ok() {
            let index = self.index.get_or_insert_with(Index::new);
            index
                .entry((manifest.name.clone(), manifest.version))
                .or_default()
                .insert(identity, manifest.clone());
        }
        Fetched::Found(ProvidedPackage {
            manifest,
            content_hash: identity,
        })
    }
}
