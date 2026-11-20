//! Minimal-version-set resolution over a provider seam. Every dep entry
//! declares a minimum version and pins the content hash of that version's
//! package; demands unify by taking each name's maximum declared minimum,
//! the provider answers name@version exactly (the load walk's provider
//! discipline), the winning declarer's pin validates the fetched content,
//! and the resolved graph — one version per name this round — serializes
//! as the byte-stable lock. The graph's native artifacts fold the same
//! way: one identity per artifact name, every declarer's pin agreeing,
//! the manifest texts (artifacts inside) carrying them into the lock.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use ds_mod::Hash128;

use crate::{
    manifest::{ArtifactBound, DepBound, Manifest},
    version::Version,
};

/// One row of provider metadata: the manifest the provider serves plus
/// the content hash of the package it names (a registry index row's
/// pin) and the member identity folded over the bundle's source
/// members where the provider decoded them — the value a consumer keys
/// a cache with, so asking for an identity never decodes a bundle.
#[derive(Clone, Debug)]
pub struct ProvidedPackage {
    /// The served manifest; its name and version must match the demand.
    pub manifest: Manifest,
    /// The package's blake3-128 content identity at that version.
    pub content_hash: Hash128,
    /// The composed member identity over the package's source members.
    pub member_identity: Hash128,
}

/// The provider seam's answer for one `(name, version)` demand. The
/// distinction the taxonomy needs is the provider's to report: an unknown
/// name and a known name whose demanded version is unoffered are different
/// refusals.
#[derive(Clone, Debug)]
pub enum Fetched {
    /// The package row for exactly this pair.
    Found(ProvidedPackage),
    /// No package under the name at all.
    UnknownName,
    /// The name exists but the exact version is unoffered; `best` is the
    /// provider's highest offered version of the name (`None`: the listing
    /// is empty), carried for the diagnostic, not the algorithm.
    Unoffered {
        /// The provider's highest offered version of the name.
        best: Option<Version>,
    },
    /// The fetch arrived but failed its own verification, or the
    /// provider's storage could not be read: a hard refusal carrying the
    /// hook's message verbatim, so resolution never guesses a taxonomy
    /// answer for content it could not validate.
    Refused(String),
}

/// Why a resolution refuses.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum ResolveError {
    /// The provider served nothing for the demanded pair: the name is
    /// unknown, or the answered content names another pair (the load
    /// walk's same-miss rule).
    MissingPackage {
        /// The demanded name.
        name: String,
        /// The demanded version.
        demanded: Version,
        /// The manifest whose dep entry demanded it.
        dependent: String,
    },
    /// The name is known but the demanded version exceeds every offered
    /// one: the declared minimum is unsatisfiable.
    UnsatisfiableBound {
        /// The demanded name.
        name: String,
        /// The demanded minimum.
        demanded: Version,
        /// The provider's highest offered version (`None`: the listing is
        /// empty), carried for the message, not the algorithm.
        best: Option<Version>,
        /// The manifest whose dep entry demanded it.
        dependent: String,
    },
    /// A hash pin disagrees: one `(name, ver)` pinned at two hashes, or a
    /// fetched package whose content hash is not the pin.
    HashMismatch {
        /// The demanded name.
        name: String,
        /// The version the pin covers.
        version: Version,
        /// The recorded pin.
        pinned: Hash128,
        /// The disagreeing hash (the other pin, or the fetched content).
        found: Hash128,
        /// The manifest that recorded the pin.
        dependent: String,
    },
    /// The resolved dependency graph carries a cross-package cycle.
    Cycle {
        /// The cycle's names, the repeated name first and last.
        cycle: Vec<String>,
    },
    /// One artifact name pinned at two identities across the graph —
    /// one version per name this round, multi-version coexistence a
    /// listed residual.
    ArtifactConflict {
        /// The conflicting artifact name.
        name: String,
        /// The pin already recorded.
        pinned: ArtifactBound,
        /// The disagreeing bound.
        found: ArtifactBound,
        /// The manifest that recorded the pin.
        dependent: String,
        /// The manifest that declared the disagreement.
        declarer: String,
    },
    /// The provider's fetch refused hard: the fetched content failed its
    /// own verification or the provider's storage could not be read.
    /// The hook owns the message; the refusal names the demand it
    /// stopped.
    Refused {
        /// The demanded name.
        name: String,
        /// The demanded version.
        version: Version,
        /// The hook's refusal message.
        message: String,
    },
}

impl core::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::MissingPackage {
                name,
                demanded,
                dependent,
            } => write!(
                f,
                "`{dependent}` demands `{name}` at {demanded}, which the provider does not serve"
            ),
            Self::UnsatisfiableBound {
                name,
                demanded,
                best,
                dependent,
            } => {
                let best = match best {
                    Some(best) => best.to_string(),
                    None => "none offered".to_owned(),
                };
                write!(
                    f,
                    "`{dependent}` demands `{name}` at {demanded}, above every offered version \
                     ({best})"
                )
            },
            Self::HashMismatch {
                name,
                version,
                pinned,
                found,
                dependent,
            } => write!(
                f,
                "`{name}` at {version}: `{dependent}` pinned content {pinned}, but {found} arrived"
            ),
            Self::Cycle { cycle } => {
                write!(f, "cross-package dependency cycle: {}", cycle.join(" -> "))
            },
            Self::ArtifactConflict {
                name,
                pinned,
                found,
                dependent,
                declarer,
            } => write!(
                f,
                "artifact `{name}` pinned at two identities: `{dependent}` pinned {} for {} \
                 (blake3:{}), `{declarer}` declares {} for {} (blake3:{}); one version per name \
                 this round",
                pinned.ver, pinned.target, pinned.hash, found.ver, found.target, found.hash
            ),
            Self::Refused {
                name,
                version,
                message,
            } => write!(f, "`{name}` at {version}: {message}"),
        }
    }
}

/// One resolved dependency: the winning version, its validated content
/// identity, its member identity (the provider's fold over the decoded
/// bundle's source members), and its manifest.
#[derive(Clone, Debug)]
pub struct ResolvedPackage {
    /// The resolved version (the maximum declared minimum).
    pub version: Version,
    /// The content pin that validated this package.
    pub content_hash: Hash128,
    /// The composed member identity over the package's source members,
    /// carried from the provider seam: the fold a cache key or a
    /// dep-list writer records, available without loading the bundle.
    pub member_identity: Hash128,
    /// The manifest at the resolved version.
    pub manifest: Manifest,
}

/// A finished resolution: the root manifest, every resolved dependency
/// name-keyed in canonical order, and the graph's folded artifacts —
/// one identity per name, every declarer's pin agreeing.
#[derive(Clone, Debug)]
pub struct Resolved {
    root: Manifest,
    deps: BTreeMap<String, ResolvedPackage>,
    artifacts: BTreeMap<String, ArtifactBound>,
}

impl Resolved {
    /// The root manifest the resolution started from.
    #[must_use]
    pub fn root(&self) -> &Manifest {
        &self.root
    }

    /// The resolved dependencies, name-keyed in canonical (name) order.
    #[must_use]
    pub fn deps(&self) -> &BTreeMap<String, ResolvedPackage> {
        &self.deps
    }

    /// The graph's native artifacts, name-keyed in canonical order: the
    /// root's and every resolved dependency's declarations folded, one
    /// version per name.
    #[must_use]
    pub fn artifacts(&self) -> &BTreeMap<String, ArtifactBound> {
        &self.artifacts
    }

    /// The lock: every manifest of the resolved graph (root included),
    /// framed deterministically — u32-LE entry count, then per entry in
    /// ascending name order the u32-LE-framed name and canonical manifest
    /// text. Two resolutions of one graph produce identical bytes.
    #[must_use]
    pub fn lock_bytes(&self) -> Vec<u8> {
        let entries: BTreeMap<&str, &Manifest> = self
            .deps
            .iter()
            .map(|(name, package)| (name.as_str(), &package.manifest))
            .chain([(self.root.name.as_str(), &self.root)])
            .collect();
        let texts: Vec<(&str, String)> = entries
            .into_iter()
            .map(|(name, manifest)| (name, manifest.to_text()))
            .collect();
        let size = 4 + texts
            .iter()
            .map(|(name, text)| 4 + name.len() + 4 + text.len())
            .sum::<usize>();
        let mut out = Vec::with_capacity(size);
        out.extend_from_slice(
            &u32::try_from(texts.len())
                .expect("entry count fits u32")
                .to_le_bytes(),
        );
        for (name, text) in texts {
            out.extend_from_slice(
                &u32::try_from(name.len())
                    .expect("name fits u32")
                    .to_le_bytes(),
            );
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(
                &u32::try_from(text.len())
                    .expect("text fits u32")
                    .to_le_bytes(),
            );
            out.extend_from_slice(text.as_bytes());
        }
        out
    }
}

/// Resolves the root's dependency closure to the minimal version set.
/// Deterministic by construction: demands live in name-sorted maps, the
/// work queue drains in first-raise order, and each `(name, version)` is
/// fetched at most once (a name holds one pending queue entry; a further
/// raise while queued updates the demand the fetch reads).
///
/// A version carries its pre-release and build segments inline (a
/// copyable value), so a refusal naming several of them is large; the
/// resolution walk is a cold path, so the size rides.
#[allow(clippy::result_large_err)]
/// # Errors
///
/// [`ResolveError::MissingPackage`] when the provider serves nothing for a
/// demand (content under another name or version included),
/// [`ResolveError::UnsatisfiableBound`] when a known name's demanded
/// version is unoffered, [`ResolveError::HashMismatch`] when one
/// `(name, version)` carries two pins or the fetched content disagrees
/// with the winning pin, [`ResolveError::Refused`] when the provider's
/// fetch fails its own verification or cannot read its storage,
/// [`ResolveError::ArtifactConflict`] when one artifact name carries two
/// identities across the graph, and [`ResolveError::Cycle`] when the
/// final graph carries a cross-package cycle.
pub fn resolve(
    root: &Manifest,
    mut fetch: impl FnMut(&str, &Version) -> Fetched,
) -> Result<Resolved, ResolveError> {
    let mut walk = Walk {
        root: root.name.clone(),
        demands: BTreeMap::new(),
        pins: BTreeMap::new(),
        resolved: BTreeMap::new(),
        queue: VecDeque::new(),
        queued: BTreeSet::new(),
    };
    for (name, bound) in &root.deps {
        walk.record(&root.name, name, *bound)?;
    }
    while let Some(name) = walk.queue.pop_front() {
        walk.queued.remove(&name);
        let (demanded, dependent) = walk
            .demands
            .get(&name)
            .cloned()
            .expect("a queued name carries its demand");
        match fetch(&name, &demanded) {
            Fetched::UnknownName => {
                return Err(ResolveError::MissingPackage {
                    name,
                    demanded,
                    dependent,
                });
            },
            Fetched::Unoffered { best } => {
                return Err(ResolveError::UnsatisfiableBound {
                    name,
                    demanded,
                    best,
                    dependent,
                });
            },
            Fetched::Refused(message) => {
                return Err(ResolveError::Refused {
                    name,
                    version: demanded,
                    message,
                });
            },
            Fetched::Found(provided) => {
                // content under another name or version is the demand
                // unserved, the load walk's same-miss rule
                if provided.manifest.name != name || provided.manifest.version != demanded {
                    return Err(ResolveError::MissingPackage {
                        name,
                        demanded,
                        dependent,
                    });
                }
                // the pin of the winning declared minimum validates the
                // recorded content identity
                let (pinned, pinning) = walk
                    .pins
                    .get(&(name.clone(), demanded))
                    .cloned()
                    .expect("a demand carries the pin of its version");
                if provided.content_hash != pinned {
                    return Err(ResolveError::HashMismatch {
                        name: name.clone(),
                        version: demanded,
                        pinned,
                        found: provided.content_hash,
                        dependent: pinning,
                    });
                }
                for (dep, bound) in &provided.manifest.deps {
                    walk.record(&name, dep, *bound)?;
                }
                walk.resolved.insert(name, ResolvedPackage {
                    version: demanded,
                    content_hash: pinned,
                    member_identity: provided.member_identity,
                    manifest: provided.manifest,
                });
            },
        }
    }
    walk.refuse_cycles(root)?;
    let artifacts = fold_artifacts(root, &walk.resolved)?;
    Ok(Resolved {
        root: root.clone(),
        deps: walk.resolved,
        artifacts,
    })
}

/// Folds the graph's native artifacts over the settled resolution: the
/// root's manifest first, then every resolved package's in ascending
/// name order — only the versions the final graph resolved, so a
/// version a later demand retired never pins. The first declarer of a
/// name records it, and a later declarer's disagreement refuses — one
/// version per name this round, the dep walk's one-slot rule over
/// exact pins instead of declared minima.
#[allow(clippy::result_large_err)]
fn fold_artifacts(
    root: &Manifest,
    resolved: &BTreeMap<String, ResolvedPackage>,
) -> Result<BTreeMap<String, ArtifactBound>, ResolveError> {
    let mut folded: BTreeMap<String, (ArtifactBound, String)> = BTreeMap::new();
    let mut record = |dependent: &str,
                      artifacts: &BTreeMap<String, ArtifactBound>|
     -> Result<(), ResolveError> {
        for (name, bound) in artifacts {
            match folded.get(name) {
                Some((pinned, first)) if *pinned != *bound => {
                    return Err(ResolveError::ArtifactConflict {
                        name: name.clone(),
                        pinned: pinned.clone(),
                        found: bound.clone(),
                        dependent: first.clone(),
                        declarer: dependent.to_owned(),
                    });
                },
                Some(_) => {},
                None => {
                    folded.insert(name.clone(), (bound.clone(), dependent.to_owned()));
                },
            }
        }
        Ok(())
    };
    record(&root.name, &root.artifacts)?;
    for (name, package) in resolved {
        record(name, &package.manifest.artifacts)?;
    }
    Ok(folded
        .into_iter()
        .map(|(name, (bound, _))| (name, bound))
        .collect())
}

/// The walk's accumulators.
struct Walk {
    /// The root package's name (a dep entry naming it is never demanded or
    /// fetched — the root is under development, not provided — and the
    /// cycle check refuses it as a graph edge).
    root: String,
    /// Each name's current demanded maximum and the declarer that raised it.
    demands: BTreeMap<String, (Version, String)>,
    /// Every declared pin, keyed by the (name, version) it covers.
    pins: BTreeMap<(String, Version), (Hash128, String)>,
    /// The manifest at each name's current demanded version.
    resolved: BTreeMap<String, ResolvedPackage>,
    /// Names whose demand needs a (re)fetch, in first-raise order; a name
    /// holds at most one pending entry, so one pop means one fetch per
    /// demand raise.
    queue: VecDeque<String>,
    /// The names currently holding a queue entry.
    queued: BTreeSet<String>,
}

impl Walk {
    /// Records one dep bound: the pin first (one `(name, ver)` pinned at
    /// two hashes refuses), then the demand, re-enqueuing the name only
    /// when its demanded maximum rose. A bound naming the root package
    /// records nothing — the cycle check reads the final graph's edges,
    /// where the bound already appears.
    #[allow(clippy::result_large_err)]
    fn record(&mut self, dependent: &str, name: &str, bound: DepBound) -> Result<(), ResolveError> {
        if name == self.root {
            return Ok(());
        }
        let key = (name.to_owned(), bound.ver);
        match self.pins.get(&key) {
            Some((pinned, first)) if *pinned != bound.hash => {
                return Err(ResolveError::HashMismatch {
                    name: name.to_owned(),
                    version: bound.ver,
                    pinned: *pinned,
                    found: bound.hash,
                    dependent: first.clone(),
                });
            },
            Some(_) => {},
            None => {
                self.pins.insert(key, (bound.hash, dependent.to_owned()));
            },
        }
        let risen = !matches!(self.demands.get(name), Some((ver, _)) if *ver >= bound.ver);
        if risen {
            self.demands
                .insert(name.to_owned(), (bound.ver, dependent.to_owned()));
            if self.queued.insert(name.to_owned()) {
                self.queue.push_back(name.to_owned());
            }
        }
        Ok(())
    }

    /// Refuses cross-package cycles over the final graph — the root's deps
    /// plus every resolved manifest's deps, neighbors in name order, an
    /// explicit stack (a long dep chain grows the heap, not the call
    /// stack). The root itself is a node like any other, so a dep naming
    /// it (from the root's own list or a resolved manifest's) closes a
    /// cycle here; a superseded manifest's root-dep retired with it.
    #[allow(clippy::result_large_err)]
    fn refuse_cycles(&self, root: &Manifest) -> Result<(), ResolveError> {
        let deps_of = |name: &str| -> Option<&BTreeMap<String, DepBound>> {
            if name == root.name {
                Some(&root.deps)
            } else {
                self.resolved.get(name).map(|pkg| &pkg.manifest.deps)
            }
        };
        // color by name: absent = unvisited, Some(false) = on the path,
        // Some(true) = done
        let mut done: BTreeMap<&str, bool> = BTreeMap::new();
        let mut path: Vec<&str> = Vec::new();
        let mut stack: Vec<(
            &str,
            std::collections::btree_map::Keys<'_, String, DepBound>,
        )> = Vec::new();
        done.insert(&root.name[..], false);
        path.push(&root.name[..]);
        stack.push((&root.name[..], root.deps.keys()));
        while let Some((name, neighbors)) = stack.last_mut() {
            let name: &str = name;
            if let Some(dep) = neighbors.next() {
                match done.get(dep.as_str()) {
                    Some(false) => {
                        // the dep names a package on the path: report
                        // the cycle, repeated name first and last
                        let at = path
                            .iter()
                            .position(|entry| *entry == dep.as_str())
                            .expect("an on-path name is on the path");
                        let mut cycle: Vec<String> =
                            path[at..].iter().map(|entry| (*entry).to_owned()).collect();
                        cycle.push(dep.clone());
                        return Err(ResolveError::Cycle { cycle });
                    },
                    Some(true) => {},
                    None => {
                        // every edge target other than the root was
                        // demanded, hence fetched and resolved
                        let deps = deps_of(dep).expect("an edge target is resolved");
                        done.insert(dep, false);
                        path.push(dep);
                        stack.push((dep, deps.keys()));
                    },
                }
            } else {
                done.insert(name, true);
                stack.pop();
                path.pop();
            }
        }
        Ok(())
    }
}
