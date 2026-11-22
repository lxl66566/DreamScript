//! A package read from a directory: the member walk below a root and
//! the directory-shaped package build. The walk admits every `.ds`
//! source below the root — real subdirectories recurse while directory
//! links (junctions and symlinks) never expand, and a `.ds` link joins
//! the walk only when its canonical target stays a file inside the root
//! — so the member set is a deterministic function of the worktree,
//! and a link can neither loop the walk back into the tree nor pull
//! directories in from outside it. The build frames the same canonical
//! bundle `ds save` lands, so a directory and its saved twin share one
//! content identity by construction — the identity a path dep's
//! resolution keys the store with.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use ds_diag::FileId;

use crate::{
    manifest::Manifest,
    package::{BundleError, Package},
};

/// The manifest's on-disk file name inside a package root.
pub const MANIFEST_FILE: &str = "ds.pkg";

/// Why a directory walk or a directory-shaped package build refuses.
#[derive(Debug, thiserror::Error)]
pub enum DirError {
    /// A directory or entry the platform refused to read; `path` names
    /// it.
    #[error("reading {}: {source}", path.display())]
    Read {
        /// The offending path.
        path: PathBuf,
        /// The platform's refusal.
        source: std::io::Error,
    },
    /// The manifest below the root does not read as UTF-8 text.
    #[error("reading {}: {source}", path.display())]
    ManifestText {
        /// The manifest's path.
        path: PathBuf,
        /// The platform's refusal.
        source: std::io::Error,
    },
    /// The manifest below the root does not parse; the drift messages
    /// ride verbatim.
    #[error(
        "the manifest at {} does not parse: {}",
        path.display(),
        messages.join("; ")
    )]
    ManifestDrift {
        /// The manifest's path.
        path: PathBuf,
        /// Every drift message the parse reported.
        messages: Vec<String>,
    },
    /// A member below the root does not read.
    #[error("reading {}: {source}", path.display())]
    MemberRead {
        /// The member's path.
        path: PathBuf,
        /// The platform's refusal.
        source: std::io::Error,
    },
    /// The bundle the directory shaped refuses to build (no source
    /// member, a reserved path, a member path outside the grammar).
    #[error(transparent)]
    Bundle(#[from] BundleError),
}

/// Every `.ds` source below one root the member grammar admits, in walk
/// order: real subdirectories recurse while directory links (junctions
/// and symlinks) never expand — the entry's own metadata decides, so a
/// link can neither loop the walk back into the tree nor pull
/// directories in from outside it — and a `.ds` link joins the walk
/// only when its canonical target stays a file inside the root. A
/// canonical visited set backs the same real directory walking twice.
///
/// # Errors
///
/// An unreadable root or directory entry ([`DirError::Read`]).
pub fn walk_sources(root: &Path) -> Result<Vec<PathBuf>, DirError> {
    let read = |path: &Path, source: std::io::Error| DirError::Read {
        path: path.to_path_buf(),
        source,
    };
    let inside = root.canonicalize().map_err(|source| read(root, source))?;
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
        for entry in fs::read_dir(&dir).map_err(|source| read(&dir, source))? {
            let entry = entry.map_err(|source| read(&dir, source))?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|source| read(&path, source))?;
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

/// Whether a `.ds` link's canonical target is a file below `inside`.
fn link_resolves_inside(link: &Path, inside: &Path) -> bool {
    link.canonicalize()
        .is_ok_and(|target| target.is_file() && target.strip_prefix(inside).is_ok())
}

/// The package one directory shapes: the manifest below it plus every
/// member the walk admits, each keyed by its `/`-separated path
/// relative to the directory, framed as the canonical bundle.
///
/// # Errors
///
/// An unreadable manifest or member, a manifest that does not parse
/// ([`DirError::ManifestDrift`]), and every framing refusal
/// [`DirError::Bundle`] names.
pub fn dir_package(dir: &Path) -> Result<Package, DirError> {
    let manifest_path = dir.join(MANIFEST_FILE);
    let text = fs::read_to_string(&manifest_path).map_err(|source| DirError::ManifestText {
        path: manifest_path.clone(),
        source,
    })?;
    let manifest =
        Manifest::parse(&text, FileId::new(0)).map_err(|diags| DirError::ManifestDrift {
            path: manifest_path,
            messages: diags.iter().map(|diag| diag.message.clone()).collect(),
        })?;
    let mut sources = Vec::new();
    for path in walk_sources(dir)? {
        let relative = path
            .strip_prefix(dir)
            .expect("the walk started at the directory");
        let member = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let bytes = fs::read(&path).map_err(|source| DirError::MemberRead {
            path: path.clone(),
            source,
        })?;
        sources.push((member, bytes));
    }
    Ok(Package::new(&manifest, sources)?)
}
