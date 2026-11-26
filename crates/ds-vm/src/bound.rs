//! The bound-artifact native tier: each `[artifacts]` library the
//! run's graph resolved installs as a native module under its manifest
//! name — the loaded library's own symbols, in export enumeration order,
//! so a pool `Constant::Bound` callee addresses one symbol by its
//! position in the same table the lowerer validated against. The C-ABI
//! marshaling itself lives in the ds-ffi crate's marshaling face (the
//! scalar words and the string/bytes pair encodings over one enumerated
//! transmute); this module owns the registry face alone (the table's
//! lifetime, pinned by the load reference, and the return-word
//! conversions around the call).
//!
//! The bound contract clauses hold here: a bound extern means panic=abort (a
//! foreign abort kills the process, the VM never unwinding through a
//! foreign frame), and the blocking call pins its fiber for its whole
//! duration — no preemption point exists inside the C call, cancellation
//! observed only at VM re-entry. A bound module stays host-side like the
//! platform handlers: the fork-join workers' code snapshot strips it, so
//! a foreign call inside a pooled child refuses with the standard
//! unresolved-callee trap instead of running on a worker.

use std::fmt;

use ds_abi::BoundRet;
use ds_ffi::{Answer, Library, Ret, Symbol};

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

/// The marshaling return word a pool constant's return spells: the
/// absent word or one of the scalar words, the same vocabulary the
/// marshaling face dispatches under.
#[must_use]
pub fn marshal_ret(ret: BoundRet) -> Ret {
    match ret {
        BoundRet::Unit => Ret::Unit,
        BoundRet::Int => Ret::Int,
        BoundRet::Bool => Ret::Bool,
        BoundRet::Float => Ret::Float,
    }
}

/// The frame word a foreign answer boxes into: a scalar word its own
/// kind, the unit answer the canonical unit word (one I64 word, the
/// `()` value's own spelling — the call site's unit-typed answer span
/// reads exactly it).
#[must_use]
pub fn answer_value(answer: Answer, ret: BoundRet) -> Value {
    match (answer, ret) {
        (Answer::Int(word), BoundRet::Int) => Value::I64(word),
        (Answer::Bool(word), BoundRet::Bool) => Value::Bool(word),
        (Answer::Float(word), BoundRet::Float) => Value::F64(word),
        // every other pairing — the unit answer included — answers the
        // canonical unit word: the staged words and the dispatch agreed
        // on the return's word, so a mismatch names hand-assembled code
        // the boxing declines to mis-read
        _ => Value::I64(0),
    }
}
