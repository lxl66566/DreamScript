//! Read-only mmap loader (G3.5).
//!
//! Zero-copy intent: the reader validates every section range against the
//! map length up front and copies header-owned data (names, hashes,
//! constants, code words) into the returned [`Module`]; a later loader
//! pass can hand the VM borrows into the mapped region for the code words
//! instead of the owned copies taken today.

use std::{fs::File, io, path::Path};

use memmap2::Mmap;

use crate::{module::Module, read::ReadError};

/// A module loaded from a read-only memory map, keeping the mapping alive
/// for as long as the parsed data is used.
pub struct MappedModule {
    map: Mmap,
    module: Module,
}

impl MappedModule {
    /// The parsed module.
    #[must_use]
    pub fn module(&self) -> &Module {
        &self.module
    }

    /// The parsed module, consumed (the mapping drops with this value):
    /// the owning install path takes the function bodies over without
    /// copying them.
    #[must_use]
    pub fn into_module(self) -> Module {
        self.module
    }

    /// The full mapped dsmod bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.map
    }
}

/// Failures of mapping a dsmod file: I/O errors plus every format error.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// Opening or mapping the file failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The mapped bytes are not a valid dsmod module.
    #[error(transparent)]
    Format(#[from] ReadError),
}

/// Memory-maps `path` read-only and parses the module from the mapped
/// bytes.
///
/// # Safety
///
/// The caller guarantees the file is neither truncated nor rewritten in
/// place for as long as the returned [`MappedModule`] lives: mapping a
/// file that shrinks or mutates under the mapping is undefined behavior
/// (`Mmap::map`'s own contract, which the type system cannot carry). The
/// in-tree discipline is the content-addressed cache artifact, written
/// by temp-plus-rename and never touched again, so each caller states
/// that guarantee at its own `unsafe` block.
///
/// # Errors
///
/// Returns [`LoadError::Io`] for open/map failures and
/// [`LoadError::Format`] with the first structural [`ReadError`] found.
pub unsafe fn read_mmap(path: &Path) -> Result<MappedModule, LoadError> {
    let file = File::open(path)?;
    // SAFETY: the caller owns the file-stability contract this
    // function's safety section documents.
    let map = unsafe { Mmap::map(&file) }?;
    let module = crate::read(&map)?;
    Ok(MappedModule { map, module })
}
