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
    key::{BlockId, DefId, PackageId, QueryKey},
    value::{
        BlockIndexOutput, LinearityOutput, ParseOutput, QueryValue, ResolveOutput, TypeOfOutput,
    },
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

/// Marker for the linearity-check query: [`QueryKey::LinearityCheck`].
pub struct LinearityCheck;

impl Query for LinearityCheck {
    type Key = DefId;
    type Value = LinearityOutput;

    fn make_key(key: Self::Key) -> QueryKey {
        QueryKey::LinearityCheck(key)
    }

    fn extract(value: QueryValue) -> Option<Self::Value> {
        match value {
            QueryValue::LinearityCheck(out) => Some(out),
            _ => None,
        }
    }
}

/// Marker for the resolve query: [`QueryKey::Resolve`]. The key is the
/// package id — 0 while M2's one package is every registered file; the
/// value is the package's namespace, per-file resolutions, and the
/// collection and resolution diagnostics [G3].
pub struct Resolve;

impl Query for Resolve {
    type Key = PackageId;
    type Value = ResolveOutput;

    fn make_key(key: Self::Key) -> QueryKey {
        QueryKey::Resolve(key)
    }

    fn extract(value: QueryValue) -> Option<Self::Value> {
        match value {
            QueryValue::Resolve(out) => Some(out),
            _ => None,
        }
    }
}

/// Marker for the type-of query: [`QueryKey::TypeOf`]. The key is the
/// definition's block packed `(file << 8) | block` (see
/// [`QueryKey::TypeOf`]); the value is its typed story [B2].
pub struct TypeOf;

impl Query for TypeOf {
    type Key = DefId;
    type Value = TypeOfOutput;

    fn make_key(key: Self::Key) -> QueryKey {
        QueryKey::TypeOf(key)
    }

    fn extract(value: QueryValue) -> Option<Self::Value> {
        match value {
            QueryValue::TypeOf(out) => Some(out),
            _ => None,
        }
    }
}

/// Marker for the effects-of query: [`QueryKey::EffectsOf`]. The key is
/// the definition's package index — declaration order across the
/// registered files [G3]; the value is its effect row [E1].
pub struct EffectsOf;

impl Query for EffectsOf {
    type Key = DefId;
    type Value = ds_check::EffectRow;

    fn make_key(key: Self::Key) -> QueryKey {
        QueryKey::EffectsOf(key)
    }

    fn extract(value: QueryValue) -> Option<Self::Value> {
        match value {
            QueryValue::EffectRow(row) => Some(row),
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
