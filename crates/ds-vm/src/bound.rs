//! The bound-artifact native tier (H5): each `[artifacts]` library the
//! run's graph resolved installs as a native module under its manifest
//! name — the loaded library's own symbols, in export enumeration order,
//! so a pool `Constant::Bound` callee addresses one symbol by its
//! position in the same table the lowerer validated against. The C-ABI
//! marshaling itself lives in the ds-ffi crate's scalar slice; this
//! module owns the registry face alone (the table's lifetime, pinned by
//! the load reference, and the word conversions around the call).
//!
//! The H5 contract clauses hold here: a bound extern means panic=abort (a
//! foreign abort kills the process, the VM never unwinding through a
//! foreign frame), and the blocking call pins its fiber for its whole
//! duration — no preemption point exists inside the C call, cancellation
//! observed only at VM re-entry. A bound module stays host-side like the
//! platform handlers: the fork-join workers' code snapshot strips it, so
//! a foreign call inside a pooled child refuses with the standard
//! unresolved-callee trap instead of running on a worker.

use std::fmt;

use ds_abi::SlotType;
use ds_ffi::{Library, Scalar, ScalarKind, ScalarSig, Symbol};

use crate::value::Value;

/// Why a bound module cannot bind: the export tables refuse to read, or a
/// listed export the platform loader cannot resolve.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindError {
    /// The library's export tables do not read.
    ExportRead,
    /// An enumerated export the loader refuses: the enumeration and the
    /// loader read the same table, so this names a library whose loader
    /// view disagrees with its bytes.
    MissingSymbol(String),
}

impl fmt::Display for BindError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExportRead => out.write_str("the library's export tables refuse to read"),
            Self::MissingSymbol(name) => {
                write!(out, "the loader resolves no symbol `{name}`")
            },
        }
    }
}

impl std::error::Error for BindError {}

/// One bound artifact's installed module: the loaded library (its load
/// reference keeping every symbol address valid for the table's
/// lifetime) and the exported symbols in enumeration order — the same
/// order `ds bind` reports and the lowerer validates a bound extern
/// against.
#[derive(Debug)]
pub struct BoundModule {
    /// The loaded library: the load reference that pins every symbol
    /// address the table holds — held for its drop, never read again.
    _library: Library,
    /// Each exported symbol, in export enumeration order.
    symbols: Vec<Symbol>,
    /// Each export's name, the same order (diagnostics).
    names: Vec<String>,
}

impl BoundModule {
    /// Binds one loaded library over its own bytes: the export reader
    /// enumerates the C symbols, and each one resolves through the
    /// loader — the addresses land in enumeration order, the indices the
    /// pool `Constant::Bound` callees address.
    ///
    /// # Errors
    ///
    /// [`BindError`] when the export tables refuse to read or a listed
    /// export does not resolve through the loader.
    pub fn bind(library: Library, bytes: &[u8]) -> Result<Self, BindError> {
        let names = ds_ffi::enumerate(bytes).map_err(|_| BindError::ExportRead)?;
        let mut symbols = Vec::with_capacity(names.len());
        for name in &names {
            symbols.push(
                library
                    .symbol(name)
                    .ok_or_else(|| BindError::MissingSymbol(name.clone()))?,
            );
        }
        Ok(Self {
            _library: library,
            symbols,
            names,
        })
    }

    /// The module's exported-symbol count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.symbols.len()
    }

    /// Whether the library exports nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }

    /// The symbol at `index`, when the index addresses the export
    /// enumeration.
    #[must_use]
    pub fn symbol(&self, index: u32) -> Option<Symbol> {
        self.symbols.get(usize::try_from(index).ok()?).copied()
    }

    /// The export's name at `index`, for diagnostics; the empty spelling
    /// when the index addresses nothing.
    #[must_use]
    pub fn name(&self, index: u32) -> &str {
        self.names
            .get(usize::try_from(index).unwrap_or(self.names.len()))
            .map_or("", String::as_str)
    }
}

/// One staged word's scalar marshaling form: `int` the i64 word, `bool`
/// the u8 word, `float` the f64 word — the staged value's own kind
/// deciding, since the lowering proved the written signature scalar and
/// the call site staged exactly its words. The index names the word that
/// refused.
pub fn scalar_of(value: Value, index: usize) -> Result<Scalar, usize> {
    match value {
        Value::I64(word) => Ok(Scalar::Int(word)),
        Value::Bool(word) => Ok(Scalar::Bool(word)),
        Value::F64(word) => Ok(Scalar::Float(word)),
        _ => Err(index),
    }
}

/// The scalar signature one call stages: the parameters read off the
/// staged words' own kinds (the call site's static fact, checked at
/// lowering), the return the pool constant's kind.
#[must_use]
pub fn staged_sig(args: &[Scalar], ret: SlotType) -> ScalarSig {
    ScalarSig {
        params: args.iter().map(|arg| arg.kind()).collect(),
        ret: scalar_kind(ret),
    }
}

/// The return word a scalar kind spells, for boxing the foreign answer.
#[must_use]
pub fn scalar_value(scalar: Scalar, ret: SlotType) -> Value {
    match (scalar, ret) {
        (Scalar::Int(word), SlotType::I64) => Value::I64(word),
        (Scalar::Bool(word), SlotType::Bool) => Value::Bool(word),
        (Scalar::Float(word), SlotType::F64) => Value::F64(word),
        // the loader validated the written return against the same slice,
        // so the pairing is the pool's own contract
        _ => Value::I64(0),
    }
}

/// The marshaling kind a return word spells, `Int` for anything outside
/// the scalar slice (the loader refused those at install).
fn scalar_kind(ret: SlotType) -> ScalarKind {
    match ret {
        SlotType::Bool => ScalarKind::Bool,
        SlotType::F64 => ScalarKind::Float,
        _ => ScalarKind::Int,
    }
}
