//! The OS loader primitives, raw and cfg-split: dlopen/dlsym/dlclose
//! over the platform libc on Unix, LoadLibraryW/GetProcAddress/FreeLibrary
//! over kernel32 on Windows — no loader crate between the binding and the
//! system, so nothing unvetted enters the dependency policy's queue. The
//! Windows path takes the wide entry point because a cache root may carry
//! non-ASCII bytes the ANSI variant's codepage fold would corrupt.

use std::{ffi::c_void, path::Path};

#[cfg(windows)]
mod imp {
    use std::{
        ffi::{CString, c_void},
        os::windows::ffi::OsStrExt,
        path::Path,
        slice,
    };

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LoadLibraryW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
        fn FreeLibrary(module: *mut c_void) -> i32;
        fn GetModuleHandleExW(flags: u32, name: *const u16, module: *mut *mut c_void) -> i32;
        fn GetLastError() -> u32;
        fn FormatMessageW(
            flags: u32,
            source: *const c_void,
            code: u32,
            lang: u32,
            buffer: *mut *mut u16,
            size: u32,
            arguments: *const c_void,
        ) -> u32;
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }

    const FORMAT_MESSAGE_FROM_SYSTEM: u32 = 0x1000;
    const FORMAT_MESSAGE_ALLOCATE_BUFFER: u32 = 0x200;
    const FROM_ADDRESS: u32 = 4;
    const UNCHANGED_REFCOUNT: u32 = 2;

    /// One landmark function whose address names this module: the
    /// FROM_ADDRESS flag resolves it to the module holding the code.
    fn mark() {}

    /// The loader's own refusal, rendered: the system message for the
    /// last error, the bare code when formatting itself refuses.
    fn last_error() -> String {
        let code = unsafe { GetLastError() };
        let mut buffer = std::ptr::null_mut::<u16>();
        let size = unsafe {
            FormatMessageW(
                FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_ALLOCATE_BUFFER,
                std::ptr::null(),
                code,
                0,
                std::ptr::addr_of_mut!(buffer),
                0,
                std::ptr::null(),
            )
        };
        if size == 0 || buffer.is_null() {
            return format!("error {code}");
        }
        let wide = unsafe { slice::from_raw_parts(buffer, size as usize) };
        let text = String::from_utf16_lossy(wide);
        unsafe { LocalFree(buffer.cast()) };
        text.trim().to_owned()
    }

    /// Loads one library by path, the handle owning its load reference.
    pub fn open(path: &Path) -> Result<*mut c_void, String> {
        let wide = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let handle = unsafe { LoadLibraryW(wide.as_ptr()) };
        if handle.is_null() {
            return Err(last_error());
        }
        Ok(handle)
    }

    /// This process's own module handle, unowned: the refcount stays
    /// untouched so a drop never frees the running image.
    pub fn this_process() -> Result<*mut c_void, String> {
        let address = (mark as *const ()).cast::<u16>();
        let mut handle = std::ptr::null_mut();
        if unsafe {
            GetModuleHandleExW(
                FROM_ADDRESS | UNCHANGED_REFCOUNT,
                address,
                std::ptr::addr_of_mut!(handle),
            )
        } == 0
        {
            return Err(last_error());
        }
        Ok(handle)
    }

    /// One named symbol's address, `None` when the module does not
    /// export it. C symbols are NUL-terminated ASCII, so any other
    /// spelling names nothing.
    pub fn symbol(handle: *mut c_void, name: &str) -> Option<usize> {
        let bytes = CString::new(name).ok()?;
        let address = unsafe { GetProcAddress(handle, bytes.as_bytes_with_nul().as_ptr()) };
        (!address.is_null()).then_some(address as usize)
    }

    /// Releases a load reference; a failure is a refcount the process
    /// cannot recover anyway, so the caller drops the handle regardless.
    pub fn close(handle: *mut c_void) {
        unsafe { FreeLibrary(handle) };
    }
}

#[cfg(unix)]
mod imp {
    use std::{
        ffi::{CStr, CString, c_char, c_int, c_void},
        path::Path,
        ptr,
    };

    unsafe extern "C" {
        fn dlopen(path: *const c_char, flags: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
        fn dlclose(handle: *mut c_void) -> c_int;
        fn dlerror() -> *const c_char;
    }

    const RTLD_NOW: c_int = 2;

    /// The loader's own refusal, rendered: dlerror's message for the
    /// failed call, the bare fact when it names none.
    fn last_error() -> String {
        let message = unsafe { dlerror() };
        if message.is_null() {
            return "the loader named no error".to_owned();
        }
        unsafe { CStr::from_ptr(message) }
            .to_string_lossy()
            .into_owned()
    }

    /// Loads one library by path, the handle owning its load reference.
    /// A path with interior NUL bytes cannot spell a C path and refuses.
    pub fn open(path: &Path) -> Result<*mut c_void, String> {
        let text = CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| "the path carries an interior NUL byte".to_owned())?;
        // clear any error left by an earlier call before reading ours
        unsafe { dlerror() };
        let handle = unsafe { dlopen(text.as_ptr(), RTLD_NOW) };
        if handle.is_null() {
            return Err(last_error());
        }
        Ok(handle)
    }

    /// This process's own handle (the null path's answer), unowned: the
    /// running image stays loaded whatever a drop decrements.
    pub fn this_process() -> Result<*mut c_void, String> {
        unsafe { dlerror() };
        let handle = unsafe { dlopen(ptr::null(), RTLD_NOW) };
        if handle.is_null() {
            return Err(last_error());
        }
        Ok(handle)
    }

    /// One named symbol's address, `None` when the process does not
    /// export it. The dlerror protocol separates a symbol at address
    /// zero from a missing one.
    pub fn symbol(handle: *mut c_void, name: &str) -> Option<usize> {
        let text = CString::new(name).ok()?;
        unsafe { dlerror() };
        let address = unsafe { dlsym(handle, text.as_ptr()) };
        if !address.is_null() {
            return Some(address as usize);
        }
        (unsafe { dlerror() }.is_null()).then_some(address as usize)
    }

    /// Releases a load reference; the count the process cannot observe
    /// from here, so the caller drops the handle regardless.
    pub fn close(handle: *mut c_void) {
        unsafe { dlclose(handle) };
    }
}

/// A raw loader handle: an owned load reference or a borrowed one (the
/// process image), so the close path stays with the open path that
/// created it.
#[derive(Debug)]
pub(crate) struct RawHandle {
    address: *mut c_void,
    owned: bool,
}

impl RawHandle {
    /// One named symbol's address, `None` when the library does not
    /// export the name.
    pub(crate) fn symbol(&self, name: &str) -> Option<usize> {
        imp::symbol(self.address, name)
    }

    /// Releases the reference when the handle owns one.
    pub(crate) fn release(&mut self) {
        if self.owned {
            let address = std::mem::replace(&mut self.address, std::ptr::null_mut());
            self.owned = false;
            imp::close(address);
        }
    }
}

// the handle is an OS counter reference, not shared Rust state: moving
// it across threads is exactly what a loader handle supports
unsafe impl Send for RawHandle {}

/// Loads one library by path (an owned reference).
///
/// # Errors
///
/// [`LoadError::Open`] when the platform loader refuses the path.
pub(crate) fn open(path: &Path) -> Result<RawHandle, LoadError> {
    imp::open(path)
        .map(|address| RawHandle {
            address,
            owned: true,
        })
        .map_err(|detail| LoadError::Open {
            path: path.to_path_buf(),
            detail,
        })
}

/// Acquires this process's own handle (a borrowed reference).
///
/// # Errors
///
/// [`LoadError::Open`] when the loader refuses the process handle.
pub(crate) fn this_process() -> Result<RawHandle, LoadError> {
    imp::this_process()
        .map(|address| RawHandle {
            address,
            owned: false,
        })
        .map_err(|detail| LoadError::Open {
            path: std::env::current_exe().unwrap_or_default(),
            detail,
        })
}

/// Why a library cannot load.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// The platform loader refused the path, its own message carried.
    #[error("{path}: the platform loader refused the library: {detail}")]
    Open {
        /// The path the loader refused.
        path: std::path::PathBuf,
        /// The loader's own refusal detail.
        detail: String,
    },
}
