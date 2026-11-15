//! The registry index: a metadata-only row file per package name plus
//! content-addressed bundle storage below one root directory, served
//! by a local-path client behind the store provider's fetch hook. The
//! row files are the committable metadata of the registry's git story
//! and the bundle files its content-addressed artifact storage, so the
//! future remote client reads the same files over https once the net
//! natives admit it; a local path serves this round (`$DS_REGISTRY`),
//! the remote client queuing behind the https ruling.

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
    store::SEGMENT,
    version::Version,
};

/// The row-file segment below the registry root: each package name one
/// text file holding its offered version rows.
pub const INDEX_SEGMENT: &str = "index";

/// The `$DS_REGISTRY` root, the directory the index rows and the bundle
/// storage sit below: the variable wins, an empty value meaning unset.
/// Nothing else configures the registry this round — the manifest
/// grammar carries no registry field, so a manifest section spelling
/// one would be DS4014 drift the parse already refuses.
#[must_use]
pub fn registry_root() -> Option<PathBuf> {
    std::env::var_os("DS_REGISTRY")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
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

    /// The registry root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
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
                    "{file} line {number}: row versions are three-component (\"{ver_text}\")"
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
