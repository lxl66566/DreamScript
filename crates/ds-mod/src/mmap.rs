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
/// # Errors
///
/// Returns [`LoadError::Io`] for open/map failures and
/// [`LoadError::Format`] with the first structural [`ReadError`] found.
pub fn read_mmap(path: &Path) -> Result<MappedModule, LoadError> {
    let file = File::open(path)?;
    // SAFETY: mapping a file another process truncates while we hold the
    // read-only map would be undefined behavior. DreamScript cache files
    // are content-addressed (`ir/<triple>/<hash>.dsmod`) and immutable
    // once written, so callers must only map such stable artifacts. The
    // map keeps `file` open for the lifetime of the returned value.
    let map = unsafe { Mmap::map(&file) }?;
    let module = crate::read(&map)?;
    Ok(MappedModule { map, module })
}
