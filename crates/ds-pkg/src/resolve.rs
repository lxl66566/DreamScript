//! Minimal-version-set resolution over a provider seam. Every registry
//! dep entry declares a minimum version and pins the content hash of that
//! version's package; demands unify by taking each name's maximum declared
//! minimum, the provider answers name@version exactly (the load walk's
//! provider discipline) with the winning declarer's pin riding the demand
//! — the key a store-backed provider resolves straight from, no name-keyed
//! index needed beside the content-addressed files — the same pin
//! validating the fetched content, and the resolved graph — one version
//! per name — serializing as the byte-stable lock. The graph's
//! native artifacts fold the same way: one identity per artifact name,
//! every declarer's pin agreeing, the manifest texts (artifacts inside)
//! carrying them into the lock.
//!
//! A path-shaped dep entry demands no version: the demand names a
//! directory (the walk joining the declaring package's root with the
//! entry's relative path), the fetch reads the package from source, and
//! the answer carries the content identity the reading actually saw — the
//! store a cache layer the fetch fills, never the path dep's source of
//! truth. One demand shape per name: a name demanded both from a path and
//! through a registry pin refuses, a name demanded at two directories
//! refuses, and a package the store or registry served (no directory of
//! its own) declaring a path dep refuses — a fetched bundle has no root
//! to resolve a relative path against. Path deps nest (a path package's
//! own manifest declaring path deps, resolved against ITS directory), and
//! the cycle check reads the final graph's edges over both shapes.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Path, PathBuf},
};

use ds_mod::Hash128;

use crate::{
    manifest::{ArtifactBound, DepBound, Manifest},
    version::Version,
};

/// One resolution demand's shape: a registry row (the name's demanded
/// version carrying the winning declarer's content pin) or a local
/// directory (the path dep's root, the walk having joined the entry's
/// relative path against the declaring package's directory — the fetch
/// seam reads absolute paths alone).
#[derive(Clone, Copy, Debug)]
pub enum Demand<'a> {
    /// The registry demand: the exact version plus the winning
    /// declarer's pin.
    Registry {
        /// The demanded exact version.
        ver: &'a Version,
        /// The winning declarer's content pin.
        pin: Hash128,
    },
    /// The path demand: the dep's directory, a package manifest inside.
    Path {
        /// The dep's package root directory.
        dir: &'a Path,
    },
}

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

/// The provider seam's answer for one demand. The distinction the
/// taxonomy needs is the provider's to report: an unknown name and a
/// known name whose demanded version is unoffered are different
/// refusals. A path demand answers `Found` or `Refused` alone — the
/// taxonomy is a registry concept (a directory either shapes a package
/// or reports why it cannot), and the walk maps a stray taxonomy answer
/// to a refusal naming it.
#[derive(Clone, Debug)]
pub enum Fetched {
    /// The package row for exactly this demand.
    Found(ProvidedPackage),
    /// No package under the name at all.
    UnknownName,
    /// The name exists but the exact version is unoffered; `best` is the
    /// provider's highest offered version of the name (`None`: the
    /// listing is empty), carried for the diagnostic, not the algorithm.
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
    /// One artifact name pinned at two identities across the graph —
    /// one version per name; multi-version coexistence is not served.
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
    /// One dep name demanded both from a local path and through a
    /// registry pin: one demand shape per name.
    MixedDemand {
        /// The demanded name.
        name: String,
        /// The manifest that demanded it from a path.
        path: String,
        /// The manifest that demanded it through a registry pin.
        pin: String,
    },
    /// One dep name demanded at two directories.
    PathConflict {
        /// The demanded name.
        name: String,
        /// The directory the first declarer named.
        dir: PathBuf,
        /// The first declarer.
        dependent: String,
        /// The directory the second declarer named.
        other: PathBuf,
        /// The second declarer.
        declarer: String,
    },
    /// A package the store or registry served — no directory of its own
    /// — declared a path dep: a fetched bundle carries no root to
    /// resolve a relative path against.
    OrphanPath {
        /// The path dep's name.
        name: String,
        /// The fetched package that declared it.
        dependent: String,
    },
    /// A path demand's fetch refused: the directory unreadable, its
    /// manifest drifting, or the member set refusing to bundle.
    PathRefused {
        /// The demanded name.
        name: String,
        /// The demanded directory.
        dir: PathBuf,
        /// The fetch's refusal message.
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
            Self::MixedDemand { name, path, pin } => write!(
                f,
                "`{name}` is demanded both from a local path (by `{path}`) and through a registry \
                 pin (by `{pin}`); one demand shape per name"
            ),
            Self::PathConflict {
                name,
                dir,
                dependent,
                other,
                declarer,
            } => write!(
                f,
                "`{name}` is demanded at two directories: `{dependent}` declares {}, `{declarer}` \
                 declares {}",
                dir.display(),
                other.display()
            ),
            Self::OrphanPath { name, dependent } => write!(
                f,
                "`{dependent}` was served from the store and declares the path dep `{name}`; a \
                 fetched bundle carries no directory to resolve a path against — declare the dep \
                 in the root manifest, or repin `{dependent}` when publishing"
            ),
            Self::PathRefused { name, dir, message } => {
                write!(f, "`{name}` at {}: {message}", dir.display())
            },
        }
    }
}

/// One resolved dependency: the winning version, its validated content
/// identity, its member identity (the provider's fold over the decoded
/// bundle's source members), its manifest, and — for a path-resolved
/// package — the directory it read from (the base its own path deps
/// resolve against, and the root a dev loop watches).
#[derive(Clone, Debug)]
pub struct ResolvedPackage {
    /// The resolved version (the maximum declared minimum for a registry
    /// demand; the directory's own manifest version for a path demand).
    pub version: Version,
    /// The content pin that validated this package.
    pub content_hash: Hash128,
    /// The composed member identity over the package's source members,
    /// carried from the provider seam: the fold a cache key or a
    /// dep-list writer records, available without loading the bundle.
    pub member_identity: Hash128,
    /// The manifest at the resolved version.
    pub manifest: Manifest,
    /// The directory a path demand read from; a registry-served package
    /// carries none.
    pub dir: Option<PathBuf>,
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

    /// Every directory a resolved path dep read from, in ascending
    /// order: the transitive watch set a dev loop grows over the
    /// package tree.
    #[must_use]
    pub fn path_dirs(&self) -> Vec<PathBuf> {
        self.deps
            .values()
            .filter_map(|package| package.dir.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
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
/// work queue drains in first-raise order, and each name is fetched at
/// most once per demand shape (a name holds one pending queue entry; a
/// further raise while queued updates the demand the fetch reads).
/// `root_dir` is the root package's own directory — the base its path
/// deps resolve against.
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
/// with the winning pin, [`ResolveError::Refused`] when a registry
/// fetch fails its own verification or cannot read its storage,
/// [`ResolveError::PathRefused`] when a path demand's directory will
/// not shape a package, [`ResolveError::MixedDemand`] when one name is
/// demanded at both shapes, [`ResolveError::PathConflict`] when one
/// name is demanded at two directories, [`ResolveError::OrphanPath`]
/// when a fetched package declares a path dep,
/// [`ResolveError::ArtifactConflict`] when one artifact name carries
/// two identities across the graph, and [`ResolveError::Cycle`] when
/// the final graph carries a cross-package cycle.
pub fn resolve(
    root: &Manifest,
    root_dir: &Path,
    mut fetch: impl FnMut(&str, Demand<'_>) -> Fetched,
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
        walk.record(&root.name, Some(root_dir), name, bound)?;
    }
    while let Some(name) = walk.queue.pop_front() {
        walk.queued.remove(&name);
        let row = walk
            .demands
            .get(&name)
            .cloned()
            .expect("a queued name carries its demand");
        match row {
            DemandRow::Registry { ver, dependent } => {
                // the pin of the winning declared minimum rides the
                // demand: the identity a store-backed provider reads
                // directly and the same pin that validates whatever the
                // fetch served
                let (pinned, pinning) = walk
                    .pins
                    .get(&(name.clone(), ver))
                    .cloned()
                    .expect("a registry demand carries the pin of its version");
                match fetch(&name, Demand::Registry {
                    ver: &ver,
                    pin: pinned,
                }) {
                    Fetched::UnknownName => {
                        return Err(ResolveError::MissingPackage {
                            name,
                            demanded: ver,
                            dependent,
                        });
                    },
                    Fetched::Unoffered { best } => {
                        return Err(ResolveError::UnsatisfiableBound {
                            name,
                            demanded: ver,
                            best,
                            dependent,
                        });
                    },
                    Fetched::Refused(message) => {
                        return Err(ResolveError::Refused {
                            name,
                            version: ver,
                            message,
                        });
                    },
                    Fetched::Found(provided) => {
                        // content under another name or version is the
                        // demand unserved, the load walk's same-miss rule
                        if provided.manifest.name != name || provided.manifest.version != ver {
                            return Err(ResolveError::MissingPackage {
                                name,
                                demanded: ver,
                                dependent,
                            });
                        }
                        // the pin of the winning declared minimum
                        // validates the recorded content identity
                        if provided.content_hash != pinned {
                            return Err(ResolveError::HashMismatch {
                                name: name.clone(),
                                version: ver,
                                pinned,
                                found: provided.content_hash,
                                dependent: pinning,
                            });
                        }
                        // a store- or registry-served package carries no
                        // directory, so its own path deps (any) refuse
                        // inside record
                        for (dep, bound) in &provided.manifest.deps {
                            walk.record(&name, None, dep, bound)?;
                        }
                        walk.resolved.insert(name, ResolvedPackage {
                            version: ver,
                            content_hash: pinned,
                            member_identity: provided.member_identity,
                            manifest: provided.manifest,
                            dir: None,
                        });
                    },
                }
            },
            DemandRow::Path { dir, dependent: _ } => {
                match fetch(&name, Demand::Path { dir: &dir }) {
                    Fetched::UnknownName | Fetched::Unoffered { .. } => {
                        return Err(ResolveError::PathRefused {
                            name,
                            dir,
                            message: "the fetch answered a registry taxonomy for a path demand"
                                .to_owned(),
                        });
                    },
                    Fetched::Refused(message) => {
                        return Err(ResolveError::PathRefused { name, dir, message });
                    },
                    Fetched::Found(provided) => {
                        // the directory must name the demanded dep; the
                        // version is whatever the directory's manifest
                        // declares (no pin validates a path)
                        if provided.manifest.name != name {
                            return Err(ResolveError::PathRefused {
                                name,
                                dir,
                                message: format!(
                                    "the package names itself `{}`",
                                    provided.manifest.name
                                ),
                            });
                        }
                        for (dep, bound) in &provided.manifest.deps {
                            walk.record(&name, Some(&dir), dep, bound)?;
                        }
                        walk.resolved.insert(name, ResolvedPackage {
                            version: provided.manifest.version,
                            content_hash: provided.content_hash,
                            member_identity: provided.member_identity,
                            manifest: provided.manifest,
                            dir: Some(dir),
                        });
                    },
                }
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
/// version per name, the dep walk's one-slot rule over
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

/// One name's current demand.
#[derive(Clone, Debug)]
enum DemandRow {
    /// The registry demand: the current demanded maximum and the
    /// declarer that raised it.
    Registry {
        /// The demanded version.
        ver: Version,
        /// The raising declarer.
        dependent: String,
    },
    /// The path demand: the resolved directory and the first declarer.
    Path {
        /// The dep's package directory.
        dir: PathBuf,
        /// The first declarer.
        dependent: String,
    },
}

/// The walk's accumulators.
struct Walk {
    /// The root package's name (a dep entry naming it is never demanded or
    /// fetched — the root is under development, not provided — and the
    /// cycle check refuses it as a graph edge).
    root: String,
    /// Each name's current demand.
    demands: BTreeMap<String, DemandRow>,
    /// Every declared registry pin, keyed by the (name, version) it
    /// covers.
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
    /// Records one dep bound: a registry pin records itself (one
    /// `(name, ver)` pinned at two hashes refuses) then the demand,
    /// re-enqueuing the name only when its demanded maximum rose; a path
    /// entry resolves against the declarer's directory (a declarer with
    /// none — a store- or registry-served package — refuses), agreeing
    /// with a prior identical path demand and refusing a different
    /// directory or the other shape. A bound naming the root package
    /// records nothing — the cycle check reads the final graph's edges,
    /// where the bound already appears.
    #[allow(clippy::result_large_err)]
    fn record(
        &mut self,
        dependent: &str,
        declarer_dir: Option<&Path>,
        name: &str,
        bound: &DepBound,
    ) -> Result<(), ResolveError> {
        if name == self.root {
            return Ok(());
        }
        match bound {
            DepBound::Pin { ver, hash } => {
                if let Some(DemandRow::Path {
                    dependent: first, ..
                }) = self.demands.get(name)
                {
                    return Err(ResolveError::MixedDemand {
                        name: name.to_owned(),
                        path: first.clone(),
                        pin: dependent.to_owned(),
                    });
                }
                let key = (name.to_owned(), *ver);
                match self.pins.get(&key) {
                    Some((pinned, first)) if *pinned != *hash => {
                        return Err(ResolveError::HashMismatch {
                            name: name.to_owned(),
                            version: *ver,
                            pinned: *pinned,
                            found: *hash,
                            dependent: first.clone(),
                        });
                    },
                    Some(_) => {},
                    None => {
                        self.pins.insert(key, (*hash, dependent.to_owned()));
                    },
                }
                let risen = !matches!(self.demands.get(name), Some(DemandRow::Registry { ver: seen, .. }) if *seen >= *ver);
                if risen {
                    self.demands.insert(name.to_owned(), DemandRow::Registry {
                        ver: *ver,
                        dependent: dependent.to_owned(),
                    });
                    if self.queued.insert(name.to_owned()) {
                        self.queue.push_back(name.to_owned());
                    }
                }
            },
            DepBound::Path { path } => {
                if let Some(DemandRow::Registry {
                    dependent: first, ..
                }) = self.demands.get(name)
                {
                    return Err(ResolveError::MixedDemand {
                        name: name.to_owned(),
                        path: dependent.to_owned(),
                        pin: first.clone(),
                    });
                }
                let Some(base) = declarer_dir else {
                    return Err(ResolveError::OrphanPath {
                        name: name.to_owned(),
                        dependent: dependent.to_owned(),
                    });
                };
                // the canonical spelling compares directories whatever
                // relative path reached them; a directory the platform
                // will not resolve keeps the lexical spelling and the
                // fetch reports the miss with its own words
                let joined = base.join(path);
                let dir = joined.canonicalize().unwrap_or(joined);
                match self.demands.get(name) {
                    Some(DemandRow::Path {
                        dir: seen,
                        dependent: first,
                    }) => {
                        if seen != &dir {
                            return Err(ResolveError::PathConflict {
                                name: name.to_owned(),
                                dir: seen.clone(),
                                dependent: first.clone(),
                                other: dir,
                                declarer: dependent.to_owned(),
                            });
                        }
                        // the same directory: the queued or resolved
                        // demand already covers it
                        return Ok(());
                    },
                    Some(DemandRow::Registry { .. }) => unreachable!("the registry arm refused"),
                    None => {},
                }
                self.demands.insert(name.to_owned(), DemandRow::Path {
                    dir,
                    dependent: dependent.to_owned(),
                });
                if self.queued.insert(name.to_owned()) {
                    self.queue.push_back(name.to_owned());
                }
            },
        }
        Ok(())
    }

    /// Refuses cross-package cycles over the final graph — the root's deps
    /// plus every resolved manifest's deps, both shapes' edges alike,
    /// neighbors in name order, an explicit stack (a long dep chain grows
    /// the heap, not the call stack). The root itself is a node like any
    /// other, so a dep naming it (from the root's own list or a resolved
    /// manifest's) closes a cycle here; the root-dep of an unselected manifest
    /// of a manifest the resolution did not select is not walked.
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
