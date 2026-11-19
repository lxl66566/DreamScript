//! The `$DS_CACHE` content-addressed store: one file per installed
//! bundle under the `pkg` segment, and one file per installed native
//! library under the `artifact` segment — each named by its blake3-128
//! identity and validated by recomputing the hash from the installed
//! file's own bytes, so the installed files are their own index, no
//! side table can drift, and a corrupt or drifted file serves no demand
//! until a refetch repairs it.

use std::{
    io::Write,
    path::{Path, PathBuf},
};

use ds_diag::FileId;
use ds_mod::Hash128;

use crate::{
    manifest::Manifest,
    package::{BundleError, Package, bundle_content_hash},
};

/// The store's segment below the cache root.
pub const SEGMENT: &str = "pkg";

/// The artifact store's segment below the cache root: prebuilt native
/// libraries, one file per content identity.
pub const ARTIFACT_SEGMENT: &str = "artifact";

/// The artifact file extension.
const ARTIFACT_EXT: &str = "lib";

/// The artifact digest's domain tag: a library's content hash is never
/// a package bundle's, so the two identity spaces cannot collide.
const ARTIFACT_HASH_DOMAIN: &[u8] = b"ds-pkg-artifact-v1";

/// The content identity of native library bytes: the domain tag folded
/// before the bytes, so the store validates an installed library by
/// recomputing this over the file's own bytes.
#[must_use]
pub fn artifact_content_hash(bytes: &[u8]) -> Hash128 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(ARTIFACT_HASH_DOMAIN);
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = [0u8; 16];
    out.copy_from_slice(&digest.as_bytes()[..16]);
    Hash128::new(out)
}

/// Why a store read or write refuses.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The file under a demanded identity hashes to another identity:
    /// corrupt or drifted content.
    #[error("{} does not hash to {expected}: its own bytes hash to {found}", path.display())]
    Disagreement {
        /// The offending file.
        path: PathBuf,
        /// The identity the demand named.
        expected: Hash128,
        /// The identity the file's own bytes recompute to.
        found: Hash128,
    },
    /// A file that hashes to its claim but does not decode as a
    /// canonical bundle.
    #[error("{} is not a canonical bundle: {error}", path.display())]
    Bundle {
        /// The offending file.
        path: PathBuf,
        /// The decode refusal.
        error: BundleError,
    },
    /// The platform refused a read or write.
    #[error("{}: {source}", path.display())]
    Io {
        /// The path the operation named.
        path: PathBuf,
        /// The platform's refusal.
        source: std::io::Error,
    },
}

/// The content-addressed store below a cache root: installed bundles
/// one file each under the [`SEGMENT`] subtree, installed native
/// libraries one file each under the [`ARTIFACT_SEGMENT`] subtree —
/// every file named by its own identity, written atomically, and
/// validated by the file's own bytes alone.
#[derive(Clone, Debug)]
pub struct Store {
    root: PathBuf,
    dir: PathBuf,
}

/// One installed row the store self-describes: the validated bundle,
/// the identity its bytes recompute to, and the manifest read from the
/// reserved member.
#[derive(Clone, Debug)]
pub struct InstalledRow {
    /// The validated bundle.
    pub package: Package,
    /// The identity the file's own bytes recompute to.
    pub identity: Hash128,
    /// The manifest read from the bundle's reserved member.
    pub manifest: Manifest,
}

impl Store {
    /// Opens the store below `root`'s [`SEGMENT`] subtree. Nothing
    /// touches the disk: a missing directory materializes lazily at the
    /// first install, and every read of an absent directory answers
    /// empty.
    #[must_use]
    pub fn open(root: &Path) -> Self {
        Self {
            dir: root.join(SEGMENT),
            root: root.to_path_buf(),
        }
    }

    /// The store's directory below the cache root.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// One identity's installed file path.
    #[must_use]
    pub fn path_of(&self, identity: Hash128) -> PathBuf {
        self.dir.join(format!("{identity}.pkg"))
    }

    /// The validated bundle for a demanded identity, the identity
    /// recomputed from the installed file's own bytes:
    /// [`None`](Option::None) names an identity nothing is installed
    /// under, and a file whose bytes hash to another identity is a
    /// disagreement that names the file — corrupt or drifted content
    /// never answers a demand.
    ///
    /// # Errors
    ///
    /// [`StoreError::Disagreement`] when the installed file's bytes
    /// hash to another identity, [`StoreError::Bundle`] when bytes
    /// that hash to the claim still refuse the canonical decode, and
    /// [`StoreError::Io`] when the platform refuses the read.
    pub fn load(&self, identity: Hash128) -> Result<Option<Package>, StoreError> {
        let path = self.path_of(identity);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            },
            Err(source) => return Err(StoreError::Io { path, source }),
        };
        let found = bundle_content_hash(&bytes);
        if found != identity {
            return Err(StoreError::Disagreement {
                path,
                expected: identity,
                found,
            });
        }
        let package =
            Package::from_bytes(&bytes).map_err(|error| StoreError::Bundle { path, error })?;
        Ok(Some(package))
    }

    /// Installs a bundle atomically: the canonical bytes land in a temp
    /// file beside the target and rename over it, so a reader never
    /// sees a partial install and an interrupted install leaves no file
    /// under the identity. A file that already validates as this
    /// identity is the install — a valid bundle is never rewritten —
    /// while disagreeing bytes under the identity are the repair this
    /// write lands. Answers the installed content identity.
    ///
    /// # Errors
    ///
    /// [`StoreError::Io`] when the write cannot land.
    pub fn install(&self, package: &Package) -> Result<Hash128, StoreError> {
        let bytes = package.to_bytes();
        let identity = bundle_content_hash(&bytes);
        // a file that validates by its own bytes already is this content
        if matches!(self.load(identity), Ok(Some(_))) {
            return Ok(identity);
        }
        Self::write_atomic(&self.dir, &self.path_of(identity), &bytes)?;
        Ok(identity)
    }

    /// The artifact segment's directory below the cache root.
    #[must_use]
    pub fn artifact_dir(&self) -> PathBuf {
        self.root.join(ARTIFACT_SEGMENT)
    }

    /// One artifact identity's installed file path.
    #[must_use]
    pub fn artifact_path(&self, identity: Hash128) -> PathBuf {
        self.artifact_dir()
            .join(format!("{identity}.{ARTIFACT_EXT}"))
    }

    /// Installs a native library under its pinned identity, validating
    /// the bytes against the pin before anything lands — the winning
    /// declarer's pin validates fetched content, the dep install's own
    /// rule. A file that already validates as this identity is the
    /// install; disagreeing bytes under the identity are the repair this
    /// write lands.
    ///
    /// # Errors
    ///
    /// [`StoreError::Disagreement`] when the bytes do not hash to
    /// `identity`, and [`StoreError::Io`] when the write cannot land.
    pub fn install_artifact(&self, identity: Hash128, bytes: &[u8]) -> Result<(), StoreError> {
        let found = artifact_content_hash(bytes);
        if found != identity {
            return Err(StoreError::Disagreement {
                path: self.artifact_path(identity),
                expected: identity,
                found,
            });
        }
        if matches!(self.load_artifact(identity), Ok(Some(_))) {
            return Ok(());
        }
        let dir = self.artifact_dir();
        Self::write_atomic(&dir, &self.artifact_path(identity), bytes)
    }

    /// The validated library bytes for a demanded identity, the identity
    /// recomputed from the installed file's own bytes:
    /// [`None`](Option::None) names an identity nothing is installed
    /// under, and a file whose bytes hash to another identity is a
    /// disagreement that names the file — corrupt or drifted content
    /// never answers a demand.
    ///
    /// # Errors
    ///
    /// [`StoreError::Disagreement`] when the installed file's bytes
    /// hash to another identity, and [`StoreError::Io`] when the
    /// platform refuses the read.
    pub fn load_artifact(&self, identity: Hash128) -> Result<Option<Vec<u8>>, StoreError> {
        let path = self.artifact_path(identity);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            },
            Err(source) => return Err(StoreError::Io { path, source }),
        };
        let found = artifact_content_hash(&bytes);
        if found != identity {
            return Err(StoreError::Disagreement {
                path,
                expected: identity,
                found,
            });
        }
        Ok(Some(bytes))
    }

    /// The temp-plus-rename write both installs share: the bytes land in
    /// a temp file beside the target and rename over it, so a reader
    /// never sees a partial install and an interrupted install leaves no
    /// file under the identity. Windows rename refuses to replace, so a
    /// lost race (or a repair over corrupt bytes) removes the target and
    /// retries once.
    fn write_atomic(dir: &Path, path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
        std::fs::create_dir_all(dir).map_err(|source| StoreError::Io {
            path: dir.to_owned(),
            source,
        })?;
        let io = |path: PathBuf, source: std::io::Error| StoreError::Io { path, source };
        let mut temp =
            tempfile::NamedTempFile::new_in(dir).map_err(|source| io(dir.to_owned(), source))?;
        temp.write_all(bytes)
            .map_err(|source| io(temp.path().to_owned(), source))?;
        // temp-plus-rename in the target's own directory: concurrent
        // writers race to byte-identical content and a reader never sees
        // a partial file
        if let Err(persist) = temp.persist(path) {
            if persist.error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(io(path.to_owned(), persist.error));
            }
            let temp = persist.file;
            std::fs::remove_file(path).map_err(|source| io(path.to_owned(), source))?;
            temp.persist(path)
                .map_err(|persist| io(path.to_owned(), persist.error))?;
        }
        Ok(())
    }

    /// Every installed row the store self-describes: each `*.pkg` file
    /// read, its identity recomputed from its own bytes, the bundle
    /// decoded, and the manifest read from the reserved member. A file
    /// that fails any step — unreadable, hashing to another identity,
    /// refusing the decode or the manifest parse — serves no row and is
    /// skipped; the next install of its true content repairs it. An
    /// absent directory answers empty.
    #[must_use]
    pub fn rows(&self) -> Vec<InstalledRow> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_none_or(|ext| ext != std::ffi::OsStr::new("pkg"))
            {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let identity = bundle_content_hash(&bytes);
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            // the row serves only what load serves: the file must sit at
            // its own identity's path
            if stem != identity.to_string() {
                continue;
            }
            let Ok(package) = Package::from_bytes(&bytes) else {
                continue;
            };
            let Ok(manifest) = package.manifest(FileId::new(0)) else {
                continue;
            };
            rows.push(InstalledRow {
                package,
                identity,
                manifest,
            });
        }
        rows
    }
}

/// The `$DS_CACHE` root, the one authority the script-mode dsmod cache
/// and the package store share: the variable wins (an empty value means
/// unset); without it the per-OS user cache directory is the default —
/// `%LOCALAPPDATA%\ds\cache` on Windows, `~/Library/Caches/ds` on macOS,
/// and `$XDG_CACHE_HOME` (absolute only) or `~/.cache` plus `ds`
/// elsewhere. A machine naming no home directory runs uncached.
#[must_use]
pub fn cache_root() -> Option<PathBuf> {
    if let Some(set) = std::env::var_os("DS_CACHE")
        && !set.is_empty()
    {
        return Some(PathBuf::from(set));
    }
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA").map(|dir| PathBuf::from(dir).join("ds").join("cache"))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Caches/ds"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        match std::env::var_os("XDG_CACHE_HOME") {
            Some(dir) if !dir.is_empty() && Path::new(&dir).is_absolute() => {
                Some(PathBuf::from(dir).join("ds"))
            },
            _ => std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache").join("ds")),
        }
    }
    #[cfg(not(any(windows, unix)))]
    {
        None
    }
}
