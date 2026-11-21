//! The store-backed provider seam: resolution's fetch interface
//! answered from the content-addressed store, each demand directed by
//! the winning declarer's pin — one identity's file read, hashed, and
//! decoded per demand, never a store-wide scan, the files their own
//! index — misses filled by a caller-supplied remote fetch hook. Every
//! answer carries the bundle's member identity beside its manifest,
//! folded where the content was decoded, so a consumer keying a cache
//! never loads and decodes the bundle again. The net natives stay out
//! of scope here — the platform's tls capability serves https, but the
//! remote side stays abstract: the registry client serves the hook — a
//! local index root or a git remote's snapshot — the verified bundles
//! installing through the store either way.

use ds_diag::FileId;
use ds_mod::Hash128;

use crate::{
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

/// The provider over the store: every demand answers from the installed
/// bundle under the winning declarer's pin — the one identity's own
/// file, read and validated per demand, so a valid bundle is never
/// fetched again with or without a remote, a demand never pays for the
/// bundles it does not name, and nothing memoizes: an install landing
/// after the provider's first demand serves the next one, whatever
/// process installed it. A miss falls to the remote hook, whose fetched
/// content installs under its own identity before it serves. Without a
/// hook the cache alone answers, its own knowledge setting the taxonomy
/// (a name the store has installed at other versions reports unoffered
/// with its highest, an unseen name unknown).
pub struct StoreProvider<'store, F> {
    store: &'store Store,
    hook: Option<F>,
}

impl<'store> StoreProvider<'store, NoRemote> {
    /// A cache-only provider: the installed rows answer or the demand
    /// refuses.
    #[must_use]
    pub fn cached(store: &'store Store) -> Self {
        Self { store, hook: None }
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
        }
    }

    /// One resolve-walk demand, directed by the winning declarer's pin:
    /// the installed bundle under the pin answers first — its own file,
    /// validated by its own bytes, the manifest and member identity
    /// folded where it decoded, no other installed bundle touched — a
    /// miss falls to the remote hook, and a fetched bundle installs
    /// before it serves so a repeat demand never refetches.
    pub fn fetch(&mut self, name: &str, version: &Version, pin: Hash128) -> Fetched {
        if let Some(served) = self.serve_installed(pin) {
            return served;
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

    /// The installed bundle under the demand's pin, served only when
    /// every step of its own validation passes: the file read, its hash
    /// recomputed from its own bytes, the canonical decode, the
    /// manifest read from the reserved member, and the member identity
    /// folded — exactly the rows a store-wide scan would have indexed,
    /// one file's worth of work. Any refusal — nothing installed under
    /// the identity, drifted bytes, a decode the bundle refuses, a
    /// manifest or member set that will not serve — answers [`None`]
    /// and the demand falls to the fetch path: the remote hook's
    /// install repairs the identity, and a hookless demand reports the
    /// taxonomy.
    fn serve_installed(&self, pin: Hash128) -> Option<Fetched> {
        let package = self.store.load(pin).ok()??;
        let manifest = package.manifest(FileId::new(0)).ok()?;
        let member_identity = package.member_identity().ok()?;
        Some(Fetched::Found(ProvidedPackage {
            manifest,
            content_hash: pin,
            member_identity,
        }))
    }

    /// The taxonomy of a demand nothing serves: a name the store has
    /// installed at other versions is unoffered with its highest, an
    /// unseen name unknown. The failing demand's path alone: resolution
    /// treats every taxonomy answer as terminal, so this pass over the
    /// installed rows runs at most once per resolution, never on a
    /// serving path.
    fn unserved(&self, name: &str) -> Fetched {
        let best = self
            .store
            .rows()
            .into_iter()
            .filter(|row| row.manifest.name == name)
            .map(|row| row.manifest.version)
            .max();
        match best {
            Some(best) => Fetched::Unoffered { best: Some(best) },
            None => Fetched::UnknownName,
        }
    }

    /// A hook-fetched bundle: the manifest must self-describe through
    /// the reserved member (a bundle that does not parse serves
    /// nothing), the source members must fold an identity (a bundle
    /// whose member set cannot serve a compile refuses, never keys a
    /// cache), the content installs under its own identity — an
    /// install failure degrades to serving without caching, never a
    /// wrong answer — and the served answer carries the manifest's own
    /// name and version, which resolution validates against the demand
    /// and the pin.
    fn serve_fetched(&mut self, name: &str, package: &Package) -> Fetched {
        let Ok(manifest) = package.manifest(FileId::new(0)) else {
            return self.unserved(name);
        };
        let member_identity = match package.member_identity() {
            Ok(member_identity) => member_identity,
            Err(error) => {
                return Fetched::Refused(format!(
                    "the fetched bundle for `{name}` does not serve a member set: {error}"
                ));
            },
        };
        let identity = package.content_hash();
        // an install failure degrades to serving without caching: the
        // next demand refetches rather than a wrong answer serving
        let _ = self.store.install(package);
        Fetched::Found(ProvidedPackage {
            manifest,
            content_hash: identity,
            member_identity,
        })
    }
}
