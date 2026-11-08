//! Errors reported by the query engine.

use ds_diag::FileId;
use thiserror::Error;

use crate::key::{BlockId, QueryKey};

/// Everything the engine can fail with. Query computes that fail for
/// language reasons report `ds-diag` diagnostics through their values
/// instead; this type covers engine-level failures only.
#[derive(Clone, PartialEq, Eq, Debug, Error)]
pub enum QueryError {
    /// A query transitively read its own in-flight result. Query-level
    /// non-termination is an engine error [H6 item 1]; `path` lists the full
    /// cycle, starting and ending at the repeated key. Intra-package
    /// definition cycles are different — legal language semantics resolved
    /// by whole-package fixpoint at M2 [G3], no concern of this engine.
    #[error("query cycle: {path:?}")]
    Cycle {
        /// The cycle, from the repeated key through every query on the path
        /// back to the repeated key.
        path: Vec<QueryKey>,
    },
    /// No file with the given id was ever registered.
    #[error("no file with id {}", .0.as_u32())]
    UnknownFile(FileId),
    /// No block with the given id exists in the file's block index.
    #[error("no block {} in file {}", .0.index, .0.file.as_u32())]
    UnknownBlock(BlockId),
    /// The key belongs to a query whose compute is not registered: either a
    /// placeholder M2+ variant or a kind unregistered via
    /// [`Db::set_compute`](crate::Db::set_compute).
    #[error("no compute registered for {0:?}")]
    Unavailable(QueryKey),
    /// A value of the wrong kind was produced for the key: an internal
    /// contract break between a compute function and its typed accessor.
    #[error("value kind does not match {0:?}")]
    KindMismatch(QueryKey),
}
