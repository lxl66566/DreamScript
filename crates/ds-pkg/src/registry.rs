//! The registry index: a metadata-only row file per package name plus
//! content-addressed bundle storage below one root directory, served
//! by a local-path client behind the store provider's fetch hook. The
//! row files are the committable metadata of the registry's git story
//! and the bundle files its content-addressed artifact storage: a
//! local path serves the index directly, and the git client serves
//! the same files from a remote repository's snapshot clone, its
//! verified bundles landing in the local content-addressed store
//! (`$DS_REGISTRY` names either — a root path or a `scheme://` git
//! remote URL, https the production transport).

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use ds_mod::Hash128;

use crate::{
    manifest::parse_hash,
    package::{Package, bundle_content_hash},
    provider::Remote,
    remote::GitRegistry,
    store::{ARTIFACT_SEGMENT, SEGMENT, artifact_content_hash},
    version::Version,
};

/// The row-file segment below the registry root: each package name one
/// text file holding its offered version rows.
pub const INDEX_SEGMENT: &str = "index";

/// The registry behind `$DS_REGISTRY`: the value names either a local
/// index root (a filesystem path) or a git remote (a `scheme://` URL
/// spelling — https the production transport, `file://` the
/// local-testing one). Nothing else configures the registry — the
/// manifest grammar carries no registry field, so a manifest section
/// spelling one is DS4014 drift the parse already refuses.
pub enum RegistryClient {
    /// A local index root, served by [`Registry`].
    Index(Registry),
    /// A git remote URL, served by [`GitRegistry`].
    Git(GitRegistry),
}

impl RegistryClient {
    /// One `$DS_REGISTRY` value's client: a `scheme://` spelling opens
    /// the git client, anything else a local index root.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        if value.contains("://") {
            Self::Git(GitRegistry::open(value))
        } else {
            Self::Index(Registry::open(Path::new(value)))
        }
    }

    /// The client the `$DS_REGISTRY` variable names, `None` when it is
    /// unset or empty. URL spellings are ASCII by nature, so a
    /// non-UTF8 value stays a local root path.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let value = std::env::var_os("DS_REGISTRY").filter(|value| !value.is_empty())?;
        Some(match value.to_str() {
            Some(value) => Self::parse(value),
            None => Self::Index(Registry::open(Path::new(&value))),
        })
    }

    /// One fetch-hook demand over whichever source the value named.
    #[must_use]
    pub fn fetch(&self, name: &str, version: &Version) -> Remote {
        match self {
            Self::Index(registry) => registry.fetch(name, version),
            Self::Git(git) => git.fetch(name, version),
        }
    }

    /// One native-artifact demand over whichever source the value named:
    /// the bytes under the demanded content identity or the taxonomy of
    /// their absence.
    #[must_use]
    pub fn fetch_artifact(&self, hash: Hash128) -> ArtifactFetch {
        match self {
            Self::Index(registry) => registry.fetch_artifact(hash),
            Self::Git(git) => git.fetch_artifact(hash),
        }
    }
}

/// A registry's answer for one artifact identity demand: libraries are
/// content-addressed, so the hash is the whole demand — found bytes,
/// an absent identity, or a hard refusal naming what failed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ArtifactFetch {
    /// The library bytes, verified against the demanded identity.
    Found(Vec<u8>),
    /// The storage holds no library under the identity.
    Absent,
    /// The storage could not be read, or the bytes it holds do not hash
    /// to the identity they sit under: a hard refusal carrying the
    /// specifics verbatim.
    Refused(String),
}

/// A local-path registry index. Every demand reads one name's row file,
/// fetches the version's bundle from the storage by its content
/// identity, and verifies the fetched bytes against the row — the
/// store's own discipline — so the index cannot serve content its own
/// rows do not name. An index that cannot be read refuses hard; only a
/// name the readable index does not offer reports the taxonomy.
#[derive(Clone, Debug)]
pub struct Registry {
    root: PathBuf,
}

impl Registry {
    /// Opens the registry below `root`: nothing touches the disk here,
    /// every demand reading lazily.
    #[must_use]
    pub fn open(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    /// The row file of one name: the package name verbatim below the
    /// index segment — the name grammar admits no path traversal, and
    /// a slashed name nests as its own segments.
    #[must_use]
    pub fn row_path(&self, name: &str) -> PathBuf {
        self.root.join(INDEX_SEGMENT).join(name)
    }

    /// The storage path of one bundle identity: the same
    /// content-addressed `<hash>.pkg` layout the store installs under.
    #[must_use]
    pub fn bundle_path(&self, hash: Hash128) -> PathBuf {
        self.root.join(SEGMENT).join(format!("{hash}.pkg"))
    }

    /// The storage path of one native-artifact identity: the same
    /// content-addressed layout the store's artifact segment installs
    /// under.
    #[must_use]
    pub fn artifact_path(&self, hash: Hash128) -> PathBuf {
        self.root.join(ARTIFACT_SEGMENT).join(format!("{hash}.lib"))
    }

    /// One native-artifact demand: the storage serves the bytes under
    /// the demanded identity and the bytes must recompute to it — the
    /// store's own discipline, so the registry cannot serve content
    /// under an identity its own bytes refute.
    #[must_use]
    pub fn fetch_artifact(&self, hash: Hash128) -> ArtifactFetch {
        let path = self.artifact_path(hash);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return ArtifactFetch::Absent;
            },
            Err(source) => {
                return ArtifactFetch::Refused(format!(
                    "reading the registry artifact at {} ({source})",
                    path.display()
                ));
            },
        };
        let found = artifact_content_hash(&bytes);
        if found != hash {
            return ArtifactFetch::Refused(format!(
                "the registry artifact at {} hashes to blake3:{found}, not the demanded \
                 blake3:{hash}",
                path.display()
            ));
        }
        ArtifactFetch::Found(bytes)
    }

    /// One fetch-hook demand. The row names the demanded version's
    /// bundle identity; the storage serves the bytes; the bytes must
    /// recompute to the row's identity — a mismatch refusing hard with
    /// both hashes named — and the verified bytes must decode as the
    /// bundle they claim.
    #[must_use]
    pub fn fetch(&self, name: &str, version: &Version) -> Remote {
        if !self.root.is_dir() {
            return Remote::Refused(format!(
                "the registry index at {} is not a readable directory",
                self.root.display()
            ));
        }
        let text = match fs::read_to_string(self.row_path(name)) {
            Ok(text) => text,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Remote::UnknownName;
            },
            Err(source) => {
                return Remote::Refused(format!("reading the registry row for `{name}`: {source}"));
            },
        };
        let rows = match self.parse_rows(name, &text) {
            Ok(rows) => rows,
            Err(reason) => return Remote::Refused(reason),
        };
        let Some(&hash) = rows.get(version) else {
            return Remote::Unoffered {
                best: rows.keys().next_back().copied(),
            };
        };
        let path = self.bundle_path(hash);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(source) => {
                return Remote::Refused(format!(
                    "the registry row for `{name}` at {version} names blake3:{hash}, but the \
                     storage holds no readable bundle at {} ({source})",
                    path.display()
                ));
            },
        };
        let found = bundle_content_hash(&bytes);
        if found != hash {
            return Remote::Refused(format!(
                "the registry row for `{name}` at {version} pins blake3:{hash}, but the fetched \
                 bytes at {} hash to blake3:{found}",
                path.display()
            ));
        }
        match Package::from_bytes(&bytes) {
            Ok(package) => Remote::Found(package),
            Err(error) => Remote::Refused(format!(
                "the registry's bundle for `{name}` at {version} (blake3:{hash}) does not decode: \
                 {error}"
            )),
        }
    }

    /// Parses one name's row file into its offered versions, each
    /// carrying the bundle identity the row pins. The canonical shape
    /// is one `<version> blake3:<hash>` row per line in strictly
    /// ascending version order, LF line ends, one trailing newline, an
    /// empty file an honest empty listing — so the row bytes are a pure
    /// function of the offering.
    fn parse_rows(&self, name: &str, text: &str) -> Result<BTreeMap<Version, Hash128>, String> {
        let path = self.row_path(name);
        let file = path.display();
        let mut rows = BTreeMap::new();
        if text.is_empty() {
            return Ok(rows);
        }
        let Some(body) = text.strip_suffix('\n') else {
            return Err(format!("{file}: the last row carries no trailing newline"));
        };
        for (index, line) in body.split('\n').enumerate() {
            let number = index + 1;
            let (ver_text, hash_text) = line.split_once(' ').ok_or_else(|| {
                format!("{file} line {number}: a row is `<version> blake3:<hash>`")
            })?;
            let version = Version::parse(ver_text)
                .map_err(|error| format!("{file} line {number}: {error}"))?;
            if version.to_string() != ver_text {
                return Err(format!(
                    "{file} line {number}: row versions are canonically spelled (\"{ver_text}\")"
                ));
            }
            let hash = parse_hash(hash_text).map_err(|_| {
                format!(
                    "{file} line {number}: hash must be `blake3:` followed by 32 lowercase hex \
                     digits"
                )
            })?;
            if rows.keys().next_back().is_some_and(|last| *last >= version) {
                return Err(format!("{file} line {number}: rows must ascend by version"));
            }
            rows.insert(version, hash);
        }
        Ok(rows)
    }
}
