//! Typed query accessors.
//!
//! Raw engine access works on [`QueryKey`]/[`QueryValue`]; this module adds
//! the compile-time-checked layer: each query is a marker type implementing
//! [`Query`], tying its key type to its value type so `db.get::<ParseFile>(f)`
//! cannot mix them up. Consumers may implement [`Query`] for their own
//! markers over existing key kinds (for example, tools keying into the
//! placeholder M2+ variants ahead of their milestones).

use std::sync::Arc;

use ds_diag::FileId;

use crate::{
    key::{BlockId, QueryKey},
    value::{BlockIndexOutput, ParseOutput, QueryValue},
};

/// Binds a query's key type to its value type.
pub trait Query {
    /// The caller-facing key.
    type Key;
    /// The caller-facing value.
    type Value;
    /// Builds the engine key.
    fn make_key(key: Self::Key) -> QueryKey;
    /// Extracts the typed value from the engine value, or `None` on a kind
    /// mismatch (mapped to [`QueryError::KindMismatch`] by the engine).
    fn extract(value: QueryValue) -> Option<Self::Value>;
}

/// Marker for the parse query: [`QueryKey::ParseFile`].
pub struct ParseFile;

impl Query for ParseFile {
    type Key = FileId;
    type Value = ParseOutput;

    fn make_key(key: Self::Key) -> QueryKey {
        QueryKey::ParseFile(key)
    }

    fn extract(value: QueryValue) -> Option<Self::Value> {
        match value {
            QueryValue::ParseFile(out) => Some(out),
            _ => None,
        }
    }
}

/// Marker for the block-index query: [`QueryKey::BlockIndex`].
pub struct BlockIndex;

impl Query for BlockIndex {
    type Key = FileId;
    type Value = BlockIndexOutput;

    fn make_key(key: Self::Key) -> QueryKey {
        QueryKey::BlockIndex(key)
    }

    fn extract(value: QueryValue) -> Option<Self::Value> {
        match value {
            QueryValue::BlockIndex(out) => Some(out),
            _ => None,
        }
    }
}

/// Marker for the block-source query: [`QueryKey::BlockSource`].
pub struct BlockSource;

impl Query for BlockSource {
    type Key = BlockId;
    type Value = Arc<str>;

    fn make_key(key: Self::Key) -> QueryKey {
        QueryKey::BlockSource(key)
    }

    fn extract(value: QueryValue) -> Option<Self::Value> {
        match value {
            QueryValue::BlockSource(text) => Some(text),
            _ => None,
        }
    }
}
