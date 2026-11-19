//! DreamScript's FFI substrate (H5): binding a native library's C
//! symbols with no loader dependency — the OS primitives called raw
//! (dlopen/dlsym over the platform libc, LoadLibraryW/GetProcAddress
//! over kernel32), cfg-split per platform — and enumerating a library's
//! exported C symbols by reading its export table (PE and ELF64)
//! straight off the bytes. The packages store's artifact segment serves
//! the libraries: one content-addressed file per installed artifact, and
//! the caller hands that path to the loader.
//!
//! The round this crate opens: externs name compiler-wired natives the
//! lowering answers today, and a call into an installed artifact refuses
//! at lowering; the consumption seam rides the C-ABI marshaling round.
//! Its H5 contract clauses, holding from the first bound library: a
//! bound extern means panic=abort (a foreign panic cannot unwind through
//! the interpreter), and a blocking foreign call pins its fiber
//! (cancellation is observed only at VM re-entry). String and bytes
//! marshaling and deinit-carrying linear handles are the same round's
//! own refusals until it lands.

mod exports;
mod sys;

pub use exports::{ExportError, enumerate};
pub use sys::LoadError;

/// One loaded native library: a load reference over the platform loader
/// (an owned reference for [`Library::open`], a borrowed one for the
/// process handle) released on drop, with symbol lookup by C name.
#[derive(Debug)]
pub struct Library {
    handle: sys::RawHandle,
}

impl Library {
    /// Loads a library by path, owning the load reference.
    ///
    /// # Errors
    ///
    /// [`LoadError::Open`] when the platform loader refuses the path —
    /// a missing file, a foreign architecture, a format the host cannot
    /// map; the loader's own message rides along.
    pub fn open(path: &std::path::Path) -> Result<Self, LoadError> {
        sys::open(path).map(|handle| Self { handle })
    }

    /// This process's own handle, borrowed: the image stays loaded
    /// whatever the drop does, so the lookup face alone is served — the
    /// portable target the loading layer's tests pin.
    ///
    /// # Errors
    ///
    /// [`LoadError::Open`] when the loader refuses the process handle.
    pub fn this_process() -> Result<Self, LoadError> {
        sys::this_process().map(|handle| Self { handle })
    }

    /// One named symbol's address, `None` when the library does not
    /// export the name. A name with an interior NUL or a non-ASCII byte
    /// cannot spell a C symbol and names nothing.
    #[must_use]
    pub fn symbol(&self, name: &str) -> Option<Symbol> {
        self.handle.symbol(name).map(|address| Symbol { address })
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // the borrowed process handle releases nothing; an owned
        // reference decrements its load count
        self.handle.release();
    }
}

/// One bound symbol: its address alone, the shape a marshaling round
/// transmutes into a typed call.
#[derive(Clone, Copy, Debug)]
pub struct Symbol {
    address: usize,
}

impl Symbol {
    /// The symbol's address.
    #[must_use]
    pub fn address(self) -> usize {
        self.address
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{ExportError, Library, enumerate};

    /// A missing path refuses, the error naming the path.
    #[test]
    fn opening_a_missing_path_refuses() {
        let missing = PathBuf::from("no-such-directory/no-such-library");
        let error = Library::open(&missing).expect_err("the loader refuses a missing path");
        assert!(
            error.to_string().contains("no-such-library"),
            "the refusal names the path: {error}"
        );
    }

    /// The process handle acquires on every platform and drops without
    /// freeing the running image.
    #[test]
    fn the_process_handle_acquires() {
        let process = Library::this_process().expect("the loader hands the process handle");
        drop(process);
        // a second acquisition after the borrowed handle's drop still
        // answers: the image was never released
        Library::this_process().expect("the process handle survives its drop");
    }

    #[cfg(windows)]
    #[test]
    fn a_system_library_loads_and_answers_symbol_lookups() {
        let kernel32 = Library::open(std::path::Path::new("kernel32.dll"))
            .expect("kernel32 is loaded in every Windows process");
        assert!(
            kernel32.symbol("LoadLibraryW").is_some(),
            "kernel32 exports its own loader entry"
        );
        assert!(
            kernel32.symbol("ds_no_such_symbol").is_none(),
            "a name nothing exports answers None"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_process_handle_answers_symbol_lookups() {
        let process = Library::this_process().expect("the loader hands the process handle");
        assert!(
            process.symbol("malloc").is_some(),
            "the platform libc's allocator is a global symbol of the process"
        );
        assert!(
            process.symbol("ds_no_such_symbol").is_none(),
            "a name nothing exports answers None"
        );
    }

    /// Garbage bytes refuse by format, never by panic: the empty file,
    /// the truncated magic, and a Mach-O magic each name their refusal.
    #[test]
    fn enumeration_refuses_unrecognized_bytes() {
        assert!(matches!(enumerate(&[]), Err(ExportError::UnknownFormat)));
        assert!(matches!(
            enumerate(b"MZ"),
            Err(ExportError::Malformed { .. })
        ));
        assert!(matches!(
            enumerate(&[0x7f, b'E', b'L', b'F']),
            Err(ExportError::Malformed { .. })
        ));
        assert!(matches!(
            enumerate(&[0xcf, 0xfa, 0xed, 0xfe, 0, 0]),
            Err(ExportError::MachO)
        ));
    }
}
