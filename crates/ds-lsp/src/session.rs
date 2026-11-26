//! The session state and the state commands: registering, replacing, and
//! editing an open document, resolving its file id and client URI, and
//! publishing its diagnostics. The feature answers live in the `features` sibling.

use std::path::{Path, PathBuf};

use ds_diag::FileId;
use ds_query::{
    CheckedPackage, Db, PackageId, PackageLinearity, ParseFile, PlatformCheck, PooledCheck,
    QueryError, Resolve, Revision,
};
use rustc_hash::FxHashMap;
use tower_lsp::lsp_types as lsp;

use crate::{SurfaceProvider, convert};

/// One open (or once-opened) document: its file id and the client's own
/// URI spelling, which answers echo verbatim instead of re-synthesizing
/// a URI from the decoded path (a path that is not valid UTF-8 cannot
/// round-trip through one).
pub(super) struct Doc {
    /// The registry file id, stable across close/reopen cycles.
    pub(super) file: FileId,
    /// The URI spelling the client shipped at the last open.
    uri: lsp::Url,
}

/// The worker-thread state: the query database, the open-document
/// table, and the per-file semantic-token cache. Everything runs on the
/// worker thread; values never leave it except through the `Send` reply
/// payloads.
pub(super) struct Session {
    pub(super) db: Db,
    /// Each document the server has ever opened, keyed by its decoded
    /// on-disk path; a close/reopen cycle reuses the entry.
    pub(super) docs: FxHashMap<PathBuf, Doc>,
    /// The ambient's file id: its declarations answer first in
    /// completion, wherever the session registered it.
    pub(super) ambient: FileId,
    /// The last computed semantic-token stream per file, stamped with
    /// the engine revision it was computed at. The revision is the
    /// queries' own invalidation clock — bumped by every real input
    /// change (an edit with new text, a registration) and nothing else —
    /// so an unchanged revision answers from the cache and any change
    /// invalidates every entry. The grain is honest, not conservative:
    /// the stream reads the package-grain resolve and analysis queries,
    /// so another document's edit can move this one's stream.
    pub(super) token_cache: FxHashMap<FileId, (Revision, Vec<lsp::SemanticToken>)>,
    /// How many times the token walk has actually run (test
    /// introspection; cache hits are invisible here by design).
    pub(super) token_walks: usize,
    /// The embedder's linked-surface tier: the `.dsi` artifacts a run of
    /// the document's own package graph cached, consulted only after the
    /// session's own resolution tiers answer nothing (the session
    /// carries no package graph, so an import-bound def resolves
    /// nothing).
    pub(super) linked: SurfaceProvider,
}

impl Session {
    /// A session with the stdlib ambient registered as the package's
    /// first member: stdlib names resolve in every document, and
    /// the ambient's file id never gains a client URI. The registration
    /// is the session composition — the run path's implicit-resolution
    /// shape — so a bare use of a migrated @std face answers hover and
    /// completion exactly as `ds run` resolves it, while the
    /// data-shaped members stay link-bound and unknown bare.
    #[cfg(test)]
    pub(super) fn new() -> Self {
        Self::with_linked(crate::no_surface())
    }

    /// [`Session::new`] over the embedder's linked-surface provider.
    pub(super) fn with_linked(linked: SurfaceProvider) -> Self {
        Self::with_ambient(linked, ds_stdlib::AMBIENT_SESSION_SOURCE)
    }

    /// A session over the shrunk ambient core: a declaring graph's
    /// registration shape, where the migrated `@std` members are not
    /// ambient members and stay unknown bare — the session runs no link
    /// pass, so an `@std` row's selection keeps its validation in the
    /// graph's compile alone. The auto-import tier's members' table
    /// serves the offers over this shape.
    #[cfg(test)]
    pub(super) fn over_core_ambient() -> Self {
        Self::with_ambient(crate::no_surface(), ds_stdlib::AMBIENT_CORE_SOURCE)
    }

    /// [`Session::with_linked`] over a chosen ambient registration: the
    /// session composition the production registration serves (the run
    /// path's implicit-resolution shape — the migrated faces resolve
    /// bare, the data-shaped members staying link-bound), or the shrunk
    /// core a declaring graph's registration takes.
    fn with_ambient(linked: SurfaceProvider, ambient_source: &str) -> Self {
        let mut db = Db::with_declared_rows(ds_stdlib::declared_rows().clone())
            .with_world_caps(ds_stdlib::world_caps().clone());
        let ambient = db.add_file(ds_stdlib::AMBIENT_FILE, ambient_source);
        Self {
            db,
            docs: FxHashMap::default(),
            ambient,
            token_cache: FxHashMap::default(),
            token_walks: 0,
            linked,
        }
    }

    /// The file id of an open (or once-opened) document.
    pub(super) fn file(&self, path: &Path) -> Option<FileId> {
        self.docs.get(path).map(|doc| doc.file)
    }

    /// The client URI of one file id, when the document was ever opened
    /// (the ambient answers none, so definitions into it stay honest).
    pub(super) fn uri(&self, file: FileId) -> Option<lsp::Url> {
        self.docs
            .values()
            .find(|doc| doc.file == file)
            .map(|doc| doc.uri.clone())
    }

    /// Registers a document (or restores a reopened one onto its
    /// original file id). The registry name is the path's lossy spelling
    /// (it only renders inside diagnostics); the exact path and the
    /// client's URI live in the document table.
    pub(super) fn open(
        &mut self,
        path: &Path,
        uri: lsp::Url,
        text: &str,
    ) -> Result<FileId, QueryError> {
        if let Some(doc) = self.docs.get_mut(path) {
            let file = doc.file;
            // A reopen may ship a different URI spelling; answers echo
            // the newest one.
            doc.uri = uri;
            self.db.set_file_content(file, text)?;
            return Ok(file);
        }
        let file = self.db.add_file(path.to_string_lossy(), text);
        self.docs.insert(path.to_owned(), Doc { file, uri });
        Ok(file)
    }

    /// Replaces a document's content; a change for a document never
    /// opened syncs nothing (the protocol orders `didOpen` first).
    pub(super) fn set(&mut self, path: &Path, text: &str) -> Result<(), QueryError> {
        if let Some(file) = self.docs.get(path).map(|doc| doc.file) {
            return self.db.set_file_content(file, text);
        }
        Ok(())
    }

    /// Applies a didChange content array to the document: each change
    /// in array order — a range-bearing one splices into the held text
    /// through the LSP UTF-16 position columns, a rangeless one
    /// replaces the whole content (full-text clients ride the same
    /// command). A change for a document never opened syncs nothing
    /// (the protocol orders `didOpen` first).
    pub(super) fn edit(
        &mut self,
        path: &Path,
        changes: &[lsp::TextDocumentContentChangeEvent],
    ) -> Result<(), String> {
        let Some(file) = self.docs.get(path).map(|doc| doc.file) else {
            return Ok(());
        };
        for change in changes {
            let new_text = match change.range {
                Some(range) => {
                    convert::apply_change(self.db.files().source(file), range, &change.text)?
                },
                None => change.text.clone(),
            };
            self.db
                .set_file_content(file, &new_text)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    /// The document's diagnostics: the parse, resolve, check, linearity,
    /// platform, and pooled classes out of the package-grain query values,
    /// each class's set filtered to this file's labels (the ambient's
    /// diagnostics carry no label here and drop out), sorted by range.
    pub(super) fn diagnostics(&mut self, file: FileId) -> Result<Vec<lsp::Diagnostic>, QueryError> {
        let Self { db, docs, .. } = self;
        let uri_of = |file: FileId| -> Option<lsp::Url> {
            docs.values()
                .find(|doc| doc.file == file)
                .map(|doc| doc.uri.clone())
        };
        let parse = db.get::<ParseFile>(file)?;
        let resolved = db.get::<Resolve>(PackageId::new(0))?;
        let checked = db.get::<CheckedPackage>(PackageId::new(0))?;
        let linear = db.get::<PackageLinearity>(PackageId::new(0))?;
        let platform = db.get::<PlatformCheck>(PackageId::new(0))?;
        let pooled = db.get::<PooledCheck>(PackageId::new(0))?;
        let mut out = Vec::new();
        for diag in parse
            .diagnostics
            .iter()
            .chain(resolved.diags.iter())
            .chain(checked.diags.iter())
            .chain(linear.diags.iter())
            .chain(platform.diags.iter())
            .chain(pooled.diags.iter())
        {
            if let Some(mapped) = convert::diagnostic(db.files(), &uri_of, diag, file) {
                out.push(mapped);
            }
        }
        out.sort_by_key(|diag| (diag.range.start.line, diag.range.start.character));
        Ok(out)
    }
}
