//! The registry's git client: the same index rows and content-addressed
//! bundles the local index serves, read from a remote git repository.
//! The client shells out to the system `git` (no vendored git
//! dependency rides the crate — git's own https transport is the
//! production spelling, `file://` the local-testing one), one shallow
//! snapshot clone landing in a temp dir at the first demand and
//! serving every later demand of the same client; the local index's
//! own fetch discipline then runs over the clone unchanged, so rows
//! parse ascending-verified, pinned content re-verifies by its own
//! hash, and a verified bundle installs into the content-addressed
//! store exactly like a local-index fetch.

use std::{cell::RefCell, path::PathBuf, process::Command};

use ds_mod::Hash128;
use tempfile::TempDir;

use crate::{
    provider::Remote,
    registry::{ArtifactFetch, Registry},
    version::Version,
};

/// The git-remote registry client. Opening touches nothing: the first
/// demand clones the remote's HEAD once (`git clone --depth 1`) and
/// later demands read the same snapshot, so one resolution fetches the
/// registry once. The clone drops with the client, so a snapshot never
/// outlives the resolution that demanded it.
pub struct GitRegistry {
    /// The remote's URL spelling.
    url: String,
    /// The snapshot clone, materialized at the first demand.
    snapshot: RefCell<Option<TempDir>>,
}

impl GitRegistry {
    /// Opens the registry at the remote `url`, any remote spelling the
    /// system git honors.
    #[must_use]
    pub fn open(url: &str) -> Self {
        Self {
            url: url.to_owned(),
            snapshot: RefCell::new(None),
        }
    }

    /// One fetch-hook demand: the snapshot clone serves the local
    /// index's fetch unchanged, so every verification and taxonomy the
    /// index enforces holds over the remote's bytes too. A clone the
    /// system git cannot land — the remote unreachable, the repository
    /// missing, no git binary on the machine — refuses hard with git's
    /// own words, never a taxonomy answer.
    #[must_use]
    pub fn fetch(&self, name: &str, version: &Version) -> Remote {
        let snapshot = match self.ensure_snapshot() {
            Ok(snapshot) => snapshot,
            Err(message) => return Remote::Refused(message),
        };
        Registry::open(&snapshot).fetch(name, version)
    }

    /// One native-artifact demand: the snapshot clone serves the local
    /// index's artifact fetch unchanged, its verified bytes installing
    /// into the content-addressed store exactly like a local-index
    /// fetch.
    #[must_use]
    pub fn fetch_artifact(&self, hash: Hash128) -> ArtifactFetch {
        let snapshot = match self.ensure_snapshot() {
            Ok(snapshot) => snapshot,
            Err(message) => return ArtifactFetch::Refused(message),
        };
        Registry::open(&snapshot).fetch_artifact(hash)
    }

    /// The snapshot's root, cloned at the first demand.
    fn ensure_snapshot(&self) -> Result<PathBuf, String> {
        let mut slot = self.snapshot.borrow_mut();
        if let Some(dir) = slot.as_ref() {
            return Ok(dir.path().to_path_buf());
        }
        let dir = tempfile::tempdir()
            .map_err(|source| format!("a temp dir for the registry snapshot: {source}"))?;
        let target = dir.path().to_string_lossy().into_owned();
        git(&["clone", "--quiet", "--depth", "1", &self.url, &target]).map_err(|message| {
            format!(
                "cloning the registry snapshot from `{}`: {message}",
                self.url
            )
        })?;
        let path = dir.path().to_path_buf();
        *slot = Some(dir);
        Ok(path)
    }
}

/// One system-git invocation, the client's only shell-out seam: a
/// missing binary refuses by name, and a failed invocation carries
/// git's own stderr, which names the unreachable host, the missing
/// repository, or the refused transport. The child inherits no git
/// repository context, so the clone cannot bind to a repository the
/// caller happens to stand in.
fn git(args: &[&str]) -> Result<(), String> {
    let output = Command::new("git")
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .map_err(|source| match source.kind() {
            std::io::ErrorKind::NotFound => "the system `git` is not installed; the remote \
                                             registry client shells out to it"
                .to_owned(),
            _ => format!("spawning git: {source}"),
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "`git {}` failed: {}",
        args.first().copied().unwrap_or_default(),
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}
