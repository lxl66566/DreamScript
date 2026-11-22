//! DreamScript packages: the `ds.pkg` manifest, minimal-version-set
//! resolution over a provider seam, and the content-addressed package
//! store under the `$DS_CACHE` root.
//!
//! The manifest is the lock — there is no lockfile. A manifest's serialized
//! bytes are a pure function of its model, and a package's identity is the
//! blake3-128 hash over its canonical bundle (the manifest text as the
//! reserved `ds.pkg` member beside the source members), so dependency
//! identity flows transitively through the hash cascade: a package's hash
//! covers its manifest, whose dep entries carry the deps' hashes and whose
//! artifact entries pin the native libraries the package ships.
//!
//! Resolution picks the minimal version set: every dep entry declares a
//! minimum version, demands unify by taking each name's maximum declared
//! minimum, and a caller-supplied provider answers by name@version exactly
//! (the load walk's provider discipline), the winning declarer's hash pin
//! validating the fetched content. An unknown name, an unoffered version,
//! a pin or content disagreement, or a cross-package cycle refuses, and
//! the resolved graph serializes byte-identically across runs as the lock.
//! The graph's native artifacts fold beside the deps — one identity per
//! artifact name — and install content-addressed into the store's artifact
//! segment, the registry's storage serving misses by content hash.
//!
//! A dep entry may take a directory shape instead: `[deps.<name>]` with
//! `path = "<relative directory>"` resolves the package from source, the
//! content it reads keyed through the same store mechanism as a cache
//! layer — never the path dep's source of truth — the path and the
//! registry pin refusing to mix for one name, path deps nesting through
//! their own directories, and a cycle over either shape's edges refusing
//! at resolution.

#![warn(missing_docs)]

pub mod dir;
pub mod manifest;
pub mod package;
pub mod provider;
pub mod registry;
pub mod remote;
pub mod resolve;
pub mod store;
pub mod version;

pub use dir::{DirError, MANIFEST_FILE, dir_package, walk_sources};
pub use manifest::{ArtifactBound, DepBound, Manifest, valid_dep_path, valid_name, valid_target};
pub use package::{
    BundleError, MANIFEST_MEMBER, Package, bundle_content_hash, member_identity, valid_member_path,
};
pub use provider::{Remote, StoreProvider, fetch_path};
pub use registry::{ArtifactFetch, INDEX_SEGMENT, Registry, RegistryClient};
pub use remote::GitRegistry;
pub use resolve::{
    Demand, Fetched, ProvidedPackage, ResolveError, Resolved, ResolvedPackage, resolve,
};
pub use store::{ARTIFACT_SEGMENT, SEGMENT, Store, StoreError, artifact_content_hash, cache_root};
pub use version::{Version, VersionError};
