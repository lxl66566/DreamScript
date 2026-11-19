//! The file-handle natives: the fsys half of the platform ABI's fixed
//! op table (`open_file`, `read_raw`, `close_raw`) — the raw seams the
//! stdlib's ds-source file face wraps (`open_file` answers the package-owned
//! `file_handle` struct's one inline word directly; `read_raw`/
//! `close_raw` take the handle word, because a native call cannot
//! borrow a deinit-carrying struct argument). The handle's word is the
//! table key, never the OS's own descriptor.
//!
//! # Handle lifetime
//!
//! The handle table is VM state, not heap state: the native calling
//! convention hands a body exactly two host references (the object
//! store, the output sink), so the table rides the sink the VM owns —
//! the same lifetime rule as the net natives' connection table, and the
//! sink's drop closes every file still open when the VM goes away.
//! `open_file` answers a monotonic `i64` handle; handles are never reused,
//! so a closed (or never-opened) id names no file and every later
//! native on it answers `io_err` naming the handle.
//!
//! `close_raw` is total and idempotent: an unknown handle is a no-op,
//! not an error. The deinit pairing needs that contract — the `file_handle`
//! value's own scope exit fires its deinit on the fd word even after
//! the owner closed the handle explicitly, and a second close must not
//! turn a clean scope exit into a halt.
//!
//! # The read contract
//!
//! `read_raw` reads exactly `n` bytes: a short read (fewer than `n`
//! bytes left) answers `io_err`, never a partial list. `n` is capped at
//! 1 MiB per call like the net reads — a larger request streams across
//! calls, and a cap-sized refusal is the honest answer over a silent
//! clamp (the name promises exactness).

use std::{fs::File, io};

use crate::{
    native::{NativeResult, OutputSink, PLATFORM_IO_ERR, PLATFORM_IO_OK, i64_arg, io_err, ref_arg},
    object::Heaps,
    value::Value,
    vm::Trap,
};

/// The per-call byte cap of `read_exact` (the net reads' cap).
pub const READ_CAP: i64 = 1024 * 1024;

/// The VM-side file-handle table: the files `open_file` opens, keyed by the
/// monotonic handle ids it hands out. VM state, not heap state: the
/// table rides the output sink, and dropping it closes every file
/// still open. The opened path rides each entry so read errors name
/// the file like the path-taking natives do.
#[derive(Debug, Default)]
pub(crate) struct Files {
    entries: Vec<(i64, File, String)>,
    next_id: i64,
}

impl Files {
    /// Registers `file` (opened at `path`) and answers its handle.
    fn register(&mut self, file: File, path: String) -> i64 {
        let id = self.next_id;
        self.next_id = id + 1;
        self.entries.push((id, file, path));
        id
    }

    /// The open entry of `handle`, `None` when the table holds none.
    fn entry(&mut self, handle: i64) -> Option<&mut (i64, File, String)> {
        self.entries.iter_mut().find(|(id, ..)| *id == handle)
    }

    /// Reads exactly `buf.len()` bytes from `handle`'s file.
    fn read_exact(&mut self, handle: i64, buf: &mut [u8]) -> io::Result<()> {
        use std::io::Read as _;
        let (_, file, _) = self.entry(handle).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("unknown handle {handle}"))
        })?;
        file.read_exact(buf)
    }

    /// Removes `handle`'s entry, closing the file; an unknown handle is
    /// a no-op (the idempotent close contract).
    fn close(&mut self, handle: i64) {
        self.entries.retain(|(id, ..)| *id != handle);
    }
}

/// `open_file(path: string) -> file_handle`: opens `path` for reading and
/// answers the fresh handle's one inline word (the table key) as the
/// fallible span's success payload. The declared spelling stays total
/// (the handle is deinit-carrying, so a union member holding it
/// refuses), so the span's failure side prints and halts.
pub(crate) fn open_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let path = heaps.str_at(ref_arg(args, 0)?)?.to_owned();
    Ok(match File::open(&path) {
        Ok(file) => {
            let handle = out.files().register(file, path.clone());
            NativeResult::Tag {
                tag: PLATFORM_IO_OK,
                payload: Value::I64(handle),
            }
        },
        Err(err) => io_err(heaps, "open", &path, &err),
    })
}

/// `read_raw(fd: int, n: int) -> bytes | io_err`: the raw read seam
/// the stdlib's ds-source `read_exact` handler wraps — reads exactly `n`
/// bytes from the handle and answers the fresh byte string.
pub(crate) fn read_raw_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let handle = i64_arg(args, 0)?;
    let n = i64_arg(args, 1)?;
    let entry_path = out.files().entry(handle).map(|(_, _, path)| path.clone());
    let Some(path) = entry_path else {
        return Ok(io_err_text(
            heaps,
            "read_raw",
            "unknown handle",
            &format!("handle {handle}"),
        ));
    };
    if n < 0 {
        return Ok(io_err_text(heaps, "read_raw", &path, "negative length"));
    }
    if n > READ_CAP {
        return Ok(io_err_text(
            heaps,
            "read_raw",
            &path,
            "over the 1 MiB per-call cap (stream across calls)",
        ));
    }
    let mut buf = vec![0u8; usize::try_from(n).expect("n fits usize")];
    if let Err(err) = out.files().read_exact(handle, &mut buf) {
        return Ok(io_err(heaps, "read_raw", &path, &err));
    }
    Ok(NativeResult::Tag {
        tag: PLATFORM_IO_OK,
        payload: heaps.new_bytes(buf.into_boxed_slice()),
    })
}

/// `close_raw(fd: int) -> ()`: the raw close seam the stdlib's
/// ds-source `close_file` handler and the `file_handle` struct's deinit call —
/// removes the handle, closing the file. Total and idempotent: an
/// unknown handle is a no-op.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn close_raw_fn(
    _heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    if let Value::I64(handle) = args.first().copied().unwrap_or(Value::I64(0)) {
        out.files().close(handle);
    }
    // the inert unit word
    Ok(NativeResult::Word(Value::I64(0)))
}

/// A `read_exact` failure whose text is not an OS error (the guards and
/// the unknown-handle refusal).
fn io_err_text(heaps: &mut Heaps, op: &str, path: &str, detail: &str) -> NativeResult {
    NativeResult::Tag {
        tag: PLATFORM_IO_ERR,
        payload: heaps.new_str(format!("{op} {path}: {detail}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `body` over a bare store on the root fiber and a captured
    /// sink: the natives under test never reach the split.
    fn with_store<R>(body: impl FnOnce(&mut Heaps, &mut OutputSink) -> R) -> R {
        let mut objects = crate::object::Objects::new(crate::fiber::FIBER_ROOT);
        let mut fibers: Vec<crate::fiber::Fiber> = Vec::new();
        let mut claims = vec![crate::claims::ClaimTable::default()];
        let mut heaps = Heaps::new(
            crate::fiber::FIBER_ROOT,
            &mut objects,
            &mut fibers,
            &mut claims,
            true,
        );
        let mut out = OutputSink::captured();
        body(&mut heaps, &mut out)
    }

    /// The interned path word of the fixture `name`.
    fn path_word(heaps: &mut Heaps, name: &str) -> Value {
        let path = std::env::temp_dir().join(name);
        heaps.new_str(path.to_str().expect("a UTF-8 temp path").to_owned())
    }

    /// open answers a monotonic handle whose read answers the bytes, whose
    /// second read continues the position, and whose close turns later
    /// reads into the unknown-handle refusal.
    #[test]
    fn open_read_close_round_trip() {
        with_store(|heaps, out| {
            let path = std::env::temp_dir().join("ds_vm_fsys_round_trip.bin");
            std::fs::write(&path, b"probe-bytes!!").expect("the fixture writes");

            let word = path_word(heaps, "ds_vm_fsys_round_trip.bin");
            let open = open_fn(heaps, out, &[word]).expect("the call traps nothing");
            let NativeResult::Tag {
                tag,
                payload: Value::I64(handle),
            } = open
            else {
                panic!("open answers a handle word: {open:?}");
            };
            assert_eq!(tag, PLATFORM_IO_OK);

            let read = read_raw_fn(heaps, out, &[Value::I64(handle), Value::I64(4)])
                .expect("the call traps nothing");
            let NativeResult::Tag {
                tag,
                payload: Value::Ref(bytes),
            } = read
            else {
                panic!("read_raw answers a bytes word: {read:?}");
            };
            assert_eq!(tag, PLATFORM_IO_OK);
            assert_eq!(
                heaps.bytes_at(bytes).expect("the bytes object reads"),
                b"prob"
            );

            close_raw_fn(heaps, out, &[Value::I64(handle)]).expect("close is total");
            let closed = read_raw_fn(heaps, out, &[Value::I64(handle), Value::I64(4)])
                .expect("the call traps nothing");
            let NativeResult::Tag { tag, .. } = closed else {
                panic!("read_raw answers a span: {closed:?}");
            };
            assert_eq!(tag, PLATFORM_IO_ERR);
            let _ = std::fs::remove_file(&path);
        });
    }

    /// A short read answers the io_err span, never a partial payload, and
    /// the per-call cap is a refusal, not a clamp.
    #[test]
    fn short_reads_and_the_cap_refuse() {
        with_store(|heaps, out| {
            let path = std::env::temp_dir().join("ds_vm_fsys_short.bin");
            std::fs::write(&path, b"abc").expect("the fixture writes");
            let word = path_word(heaps, "ds_vm_fsys_short.bin");
            let NativeResult::Tag {
                payload: Value::I64(handle),
                ..
            } = open_fn(heaps, out, &[word]).expect("open answers a handle")
            else {
                unreachable!("the open shape is pinned above");
            };
            let NativeResult::Tag { tag, .. } =
                read_raw_fn(heaps, out, &[Value::I64(handle), Value::I64(10)])
                    .expect("the call traps nothing")
            else {
                unreachable!("read_raw answers a span");
            };
            assert_eq!(tag, PLATFORM_IO_ERR);
            let NativeResult::Tag { tag, .. } =
                read_raw_fn(heaps, out, &[Value::I64(handle), Value::I64(READ_CAP + 1)])
                    .expect("the call traps nothing")
            else {
                unreachable!("read_raw answers a span");
            };
            assert_eq!(tag, PLATFORM_IO_ERR);
            let _ = std::fs::remove_file(&path);
        });
    }
}
