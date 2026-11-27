//! The platform tag classification, the io error-class mapping, and the
//! file and clock natives.

use std::{
    sync::LazyLock,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use super::*;

/// Whether a platform discriminant is one of the io failure classes.
#[must_use]
pub fn is_io_class(tag: u8) -> bool {
    matches!(
        tag,
        PLATFORM_IO_NOT_FOUND
            | PLATFORM_IO_PERMISSION_DENIED
            | PLATFORM_IO_ALREADY_EXISTS
            | PLATFORM_IO_TIMED_OUT
            | PLATFORM_IO_UNEXPECTED_EOF
            | PLATFORM_IO_OTHER
    )
}

/// Whether a platform discriminant is one of the net failure classes.
#[must_use]
pub fn is_net_class(tag: u8) -> bool {
    matches!(
        tag,
        PLATFORM_NET_TIMEOUT
            | PLATFORM_NET_REFUSED
            | PLATFORM_NET_ADDR_IN_USE
            | PLATFORM_NET_RESET
            | PLATFORM_NET_OTHER
    )
}

/// The io class of one OS failure: the `io_err` member the platform
/// discriminant answers by error class — every kind the classes name
/// lands on its tag, the rest folding into the `io_other` floor.
#[must_use]
pub(crate) fn io_class(err: &std::io::Error) -> u8 {
    use std::io::ErrorKind;
    match err.kind() {
        ErrorKind::NotFound => PLATFORM_IO_NOT_FOUND,
        ErrorKind::PermissionDenied => PLATFORM_IO_PERMISSION_DENIED,
        ErrorKind::AlreadyExists => PLATFORM_IO_ALREADY_EXISTS,
        ErrorKind::TimedOut | ErrorKind::WouldBlock => PLATFORM_IO_TIMED_OUT,
        ErrorKind::UnexpectedEof => PLATFORM_IO_UNEXPECTED_EOF,
        _ => PLATFORM_IO_OTHER,
    }
}

/// The `io_err` span: the failure's class discriminant with the word
/// the re-spanned member carries as the payload — the path classes
/// (not_found, permission_denied, already_exists) answer the path they
/// failed on, `io_other` the rendered message `"<op> <path>: <error>"`
/// (the OS message rides the floor alone), and the bare classes a
/// diagnostic detail the bare member discards at the classed re-span
/// and the total spelling prints.
pub(crate) fn io_err(
    heaps: &mut Heaps,
    op: &str,
    path: &str,
    err: &std::io::Error,
) -> NativeResult {
    let class = io_class(err);
    let detail = match class {
        PLATFORM_IO_NOT_FOUND | PLATFORM_IO_PERMISSION_DENIED | PLATFORM_IO_ALREADY_EXISTS => {
            path.to_owned()
        },
        _ => format!("{op} {path}: {err}"),
    };
    NativeResult::Tag {
        tag: class,
        payload: heaps.new_str(detail),
    }
}

pub(crate) fn read_file_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let path = heaps.str_at(ref_arg(args, 0)?)?.to_owned();
    Ok(match std::fs::read_to_string(&path) {
        Ok(text) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: heaps.new_str(text),
        },
        Err(err) => io_err(heaps, "read_file", &path, &err),
    })
}

pub(crate) fn write_file_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let path = heaps.str_at(ref_arg(args, 0)?)?.to_owned();
    // the contents stay a borrow of the store: the path is owned for the
    // error arm's message alone, and writing reads the bytes once
    let contents = heaps.str_at(ref_arg(args, 1)?)?;
    Ok(match std::fs::write(&path, contents.as_bytes()) {
        // create-or-truncate; the payload word is the inert unit word
        Ok(()) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::I64(0),
        },
        Err(err) => io_err(heaps, "write_file", &path, &err),
    })
}

pub(crate) fn file_exists_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // the checked spelling `bool | io_err`: a path the process can stat
    // answers its presence (an absent path the false side), and every
    // other stat failure its io_err class
    let path = heaps.str_at(ref_arg(args, 0)?)?.to_owned();
    Ok(match std::fs::metadata(&path) {
        Ok(_) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::Bool(true),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::Bool(false),
        },
        Err(err) => io_err(heaps, "file_exists", &path, &err),
    })
}

pub(crate) fn fs_list_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let dir = heaps.str_at(ref_arg(args, 0)?)?.to_owned();
    // sorted, so listing order never depends on the platform's readdir
    let names = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .map(|entry| entry.map(|d| d.file_name().to_string_lossy().into_owned()))
            .collect::<Result<Vec<String>, _>>(),
        Err(err) => Err(err),
    };
    let mut names = match names {
        Ok(names) => names,
        Err(err) => return Ok(io_err(heaps, "fs_list", &dir, &err)),
    };
    names.sort_unstable();
    let mut elems = Vec::with_capacity(names.len());
    for name in names {
        elems.push(heaps.new_str(name));
    }
    // each fresh name's birth count becomes the list's edge, so the
    // frame claim its alloc entered leaves with the adoption
    for elem in &elems {
        if let Value::Ref(word) = *elem {
            heaps.release_claim(word);
        }
    }
    Ok(NativeResult::Tag {
        tag: PLATFORM_IO_OK,
        payload: heaps.new_list(elems),
    })
}

/// `time() -> int`: the body behind the platform ABI's `time` op —
/// epoch nanoseconds, the duration since the Unix epoch clamped at
/// zero before it (the wall clock; durations measured over it bend
/// with the host's own clock adjustments).
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn time_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    _args: &[Value],
) -> Result<NativeResult, Trap> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_nanos())
        .unwrap_or_default();
    Ok(NativeResult::Word(Value::I64(
        i64::try_from(nanos).unwrap_or(i64::MAX),
    )))
}

/// The monotonic clock's origin: fixed at the process's first `mono`
/// read, an arbitrary point whose only contract is that later reads
/// never go backward — differences between two reads measure elapsed
/// time, the absolute word carries no meaning.
pub(super) static MONO_ORIGIN: LazyLock<Instant> = LazyLock::new(Instant::now);

/// `mono() -> int`: the body behind the platform ABI's `mono` op — monotonic
/// nanoseconds since the process-fixed origin, never moving backward
/// and never bending with the host's wall-clock adjustments. The
/// natural pair for `sleep`: a span measured over `mono` words is the
/// span `sleep` waits.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn mono_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    _args: &[Value],
) -> Result<NativeResult, Trap> {
    let nanos = MONO_ORIGIN.elapsed().as_nanos();
    Ok(NativeResult::Word(Value::I64(
        i64::try_from(nanos).unwrap_or(i64::MAX),
    )))
}

/// `sleep(nanos: int) -> ()`: the body behind the platform ABI's
/// `sleep` op — park the calling fiber for at least the requested
/// nanoseconds (a negative span answers at once), the timer-only wait
/// shape: no IO interest, the deadline wheel's firing the completion
/// itself. Under no installed reactor the blocking twin sleeps the
/// thread — the embedder's platform choice, never a language fact.
pub(crate) fn sleep_fn(
    _heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let nanos = i64_arg(args, 0)?;
    let span = Duration::from_nanos(nanos.max(0).cast_unsigned());
    if out.net().parking() {
        let deadline = Instant::now() + span;
        return Ok(NativeResult::Park(crate::sched::WorldWait::sleep(deadline)));
    }
    std::thread::sleep(span);
    Ok(NativeResult::Word(Value::I64(0)))
}

pub(crate) fn file_size_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let path = heaps.str_at(ref_arg(args, 0)?)?.to_owned();
    Ok(match std::fs::metadata(&path) {
        // no filesystem carries a length past i64::MAX
        Ok(meta) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::I64(i64::try_from(meta.len()).expect("file size fits i64")),
        },
        Err(err) => io_err(heaps, "file_size", &path, &err),
    })
}
