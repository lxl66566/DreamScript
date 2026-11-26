//! The native convention's host reference: the output sink and its
//! targets, the console and stdin natives, the argument extractors,
//! and the bare trap primitive.

use std::{io::Write, time::Duration};

use super::*;
use crate::{net::Connections, value::RefId};

/// The native convention's second host reference (beside the object
/// store): where print/println write — process stdout by default, a
/// captured buffer for tests and embedders ([`crate::Vm::capture_output`])
/// — plus the VM-lifetime host state that is not heap state, which rides
/// this reference because a native body sees exactly the two (the store,
/// the sink). The net natives' connection table is that state (crate::net):
/// sockets are VM-lifetime like heap objects, but the store is the
/// per-fiber half a split hands a body, while this sink stays VM-level.
#[derive(Debug, Default)]
pub struct OutputSink {
    /// Where print/println write.
    target: SinkTarget,
    /// Where eprint/eprintln write.
    err_target: ErrTarget,
    /// The process arguments the `args` op answers (the CLI process
    /// surface): the host stages them, the VM never derives them — an
    /// embedder's `args()` reads exactly what its host staged.
    argv: Vec<String>,
    /// The connection table of the net natives.
    net: Connections,
    /// The listener table of the net natives' server half
    /// (`crate::net`).
    listeners: crate::net::Listeners,
    /// The process table of the capture half's natives (`crate::proc`).
    procs: crate::proc::Procs,
    /// The file-handle table of the fsys natives (`crate::fsys`).
    files: crate::fsys::Files,
    /// The close seams' wake notes: each close native notes
    /// the handle it closed, the dispatcher's native call site draining
    /// the notes to wake the parked waits the closed handles name — the
    /// one door the sink's own state reaches the wait table through,
    /// since a native body sees exactly the store and the sink.
    closed: Vec<(crate::sched::CloseSeam, i64)>,
}

/// The write target of an [`OutputSink`].
#[derive(Debug, Default, PartialEq)]
pub(super) enum SinkTarget {
    /// No capture requested yet; stdout. `Default` so `Vm::new` stays
    /// cheap and the switch to `Captured` is explicit.
    #[default]
    Stdout,
    /// The in-memory sink.
    Captured(Vec<u8>),
}

/// The stderr write target of an [`OutputSink`].
#[derive(Debug, Default, PartialEq)]
pub(super) enum ErrTarget {
    /// No capture requested yet; the process's stderr.
    #[default]
    Stderr,
    /// The in-memory sink, switched with [`OutputSink::capture`].
    Captured(Vec<u8>),
}

impl OutputSink {
    /// A fresh in-memory sink (tests and embedders construct the capture
    /// mode directly; the VM reaches it through [`crate::Vm::capture_output`]).
    #[must_use]
    pub fn captured() -> Self {
        Self {
            target: SinkTarget::Captured(Vec::new()),
            err_target: ErrTarget::Captured(Vec::new()),
            argv: Vec::new(),
            net: Connections::default(),
            listeners: crate::net::Listeners::default(),
            procs: crate::proc::Procs::default(),
            files: crate::fsys::Files::default(),
            closed: Vec::new(),
        }
    }

    /// Switches the write targets to fresh in-memory buffers, keeping
    /// every other field.
    pub(crate) fn capture(&mut self) {
        self.target = SinkTarget::Captured(Vec::new());
        self.err_target = ErrTarget::Captured(Vec::new());
    }

    /// The bytes captured so far, empty in stdout mode.
    #[must_use]
    pub fn captured_bytes(&self) -> &[u8] {
        match &self.target {
            SinkTarget::Captured(buf) => buf,
            SinkTarget::Stdout => &[],
        }
    }

    /// The stderr bytes captured so far, empty in stderr mode.
    #[must_use]
    pub fn captured_err_bytes(&self) -> &[u8] {
        match &self.err_target {
            ErrTarget::Captured(buf) => buf,
            ErrTarget::Stderr => &[],
        }
    }

    /// Stages the process arguments the `args` op answers — the host's
    /// own wording of the run's command line (the CLI passes its
    /// `--args` words verbatim).
    pub fn set_argv(&mut self, argv: Vec<String>) {
        self.argv = argv;
    }

    /// The staged process arguments.
    #[must_use]
    pub(crate) fn argv(&self) -> &[String] {
        &self.argv
    }

    /// The connection table of the net natives.
    pub(crate) fn net(&mut self) -> &mut Connections {
        &mut self.net
    }

    /// The listener table of the net natives' server half.
    pub(crate) fn listeners(&mut self) -> &mut crate::net::Listeners {
        &mut self.listeners
    }

    /// The process table of the capture half's natives.
    pub(crate) fn procs(&mut self) -> &mut crate::proc::Procs {
        &mut self.procs
    }

    /// The file-handle table of the fsys natives (`crate::fsys`).
    pub(crate) fn files(&mut self) -> &mut crate::fsys::Files {
        &mut self.files
    }

    /// Notes one close seam's handle for the wake door (the close
    /// native's own half of the wake-on-close contract).
    pub(crate) fn note_closed(&mut self, seam: crate::sched::CloseSeam, id: i64) {
        self.closed.push((seam, id));
    }

    /// Takes the close seams' notes: the dispatcher's native call site
    /// drains the journal after every native call, so a note never
    /// outlives the call that wrote it.
    pub(crate) fn take_closed(&mut self) -> Vec<(crate::sched::CloseSeam, i64)> {
        std::mem::take(&mut self.closed)
    }

    /// Bounds every blocking net wait ([`crate::net`]) — the connect,
    /// data, and accept waits together.
    pub(crate) fn set_net_timeout(&mut self, timeout: Duration) {
        self.net.set_timeout(timeout);
        self.listeners.set_timeout(timeout);
    }

    /// Flips the net face to the parking discipline (a reactor was
    /// installed on the VM): registered connections and bound
    /// listeners go non-blocking, `net_recv`/`net_send`/`net_accept`
    /// answering park requests on a would-block.
    pub(crate) fn enable_net_parking(&mut self) {
        self.net.enable_parking();
        self.listeners.enable_parking();
    }

    /// Flips the process face's blocking rows to the parking
    /// discipline (a reactor was installed on the VM): the read rows
    /// answer park requests on a would-block over the child's pipes,
    /// the wait row over its pidfd — Linux alone, the pipes owning no
    /// portable non-blocking shape (`crate::proc`).
    pub(crate) fn enable_proc_parking(&mut self) {
        self.procs.enable_parking();
    }

    /// Bounds every parked proc wait ([`crate::proc`]); the blocking
    /// twins stay unbounded whatever the bound.
    pub(crate) fn set_proc_timeout(&mut self, timeout: Duration) {
        self.procs.set_timeout(timeout);
    }

    /// Installs the host's blocking-drive stop flag: the bounded blocking accept
    /// poll and the blocking proc wait twin observe it between their
    /// poll rounds and answer the shutdown trap — a
    /// reactor-installing host's parked twin rides the registered
    /// shutdown interest instead and never reads this flag.
    pub(crate) fn set_shutdown_flag(
        &mut self,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        self.listeners.set_stop_flag(std::sync::Arc::clone(&stop));
        self.procs.set_stop_flag(stop);
    }

    /// Pins the TLS client of the net face's tls tier (`[crate::tls]`):
    /// custom trust roots for an embedding host or a test, the default
    /// being the webpki root set.
    pub fn set_tls_config(&mut self, config: std::sync::Arc<rustls::ClientConfig>) {
        self.net.set_tls_config(config);
    }

    /// Pins the TLS server identity of the net face's tls tier
    /// (`[crate::tls]`'s server half): no default identity exists, the
    /// unprovisioned `net_accept_tls` answering its own refusal.
    pub fn set_tls_server_config(&mut self, config: std::sync::Arc<rustls::ServerConfig>) {
        self.net.set_tls_server_config(config);
    }

    /// Advances one parked accept-tls machine one step (the machine's
    /// row and its session the connection table owns, the accept stage
    /// probing the listener table it names).
    pub(crate) fn tls_accept_wake(&mut self, accept: i64) -> crate::net::AcceptWake {
        self.net.tls_accept_wake(accept, &mut self.listeners)
    }

    /// Writes `text` with no newline.
    ///
    /// # Errors
    ///
    /// [`Trap::OutputWrite`] when the underlying write fails.
    pub fn write_str(&mut self, text: &str) -> Result<(), Trap> {
        match &mut self.target {
            // Stdout is line-buffered, so println's trailing newline is
            // the flush point; print keeps its no-newline contract
            SinkTarget::Stdout => std::io::stdout()
                .write_all(text.as_bytes())
                .map_err(|e| Trap::OutputWrite { kind: e.kind() }),
            SinkTarget::Captured(buf) => {
                buf.extend_from_slice(text.as_bytes());
                Ok(())
            },
        }
    }

    /// Writes `text` followed by one newline.
    ///
    /// # Errors
    ///
    /// [`Trap::OutputWrite`] when the underlying write fails.
    pub fn write_line(&mut self, text: &str) -> Result<(), Trap> {
        self.write_str(text)?;
        self.write_str("\n")
    }

    /// Writes `text` to stderr with no newline — the eprint/eprintln
    /// target, captured beside stdout in capture mode.
    ///
    /// # Errors
    ///
    /// [`Trap::OutputWrite`] when the underlying write fails.
    pub fn write_err_str(&mut self, text: &str) -> Result<(), Trap> {
        match &mut self.err_target {
            ErrTarget::Stderr => std::io::stderr()
                .write_all(text.as_bytes())
                .map_err(|e| Trap::OutputWrite { kind: e.kind() }),
            ErrTarget::Captured(buf) => {
                buf.extend_from_slice(text.as_bytes());
                Ok(())
            },
        }
    }

    /// Writes `text` to stderr followed by one newline.
    ///
    /// # Errors
    ///
    /// [`Trap::OutputWrite`] when the underlying write fails.
    pub fn write_err_line(&mut self, text: &str) -> Result<(), Trap> {
        self.write_err_str(text)?;
        self.write_err_str("\n")
    }
}

/// Reads the `Ref` word at `index` — the full owner-routing pair —
/// refusing like a mismatched slot with the offending argument's own
/// kind (never another argument's).
pub(crate) fn ref_arg(args: &[Value], index: usize) -> Result<RefId, Trap> {
    match args.get(index) {
        Some(Value::Ref(word)) => Ok(*word),
        // an arena temporary has no count and no object identity; a
        // native's reference argument is a count-bearing hand-off the
        // placement proof keeps designated temporaries away from
        Some(Value::Arena(_)) => Err(Trap::ArenaHandoff {
            slot: u8::try_from(index).expect("slot index fits u8"),
        }),
        _ => Err(Trap::SlotTypeMismatch {
            slot: u8::try_from(index).expect("slot index fits u8"),
            expected: SlotType::Ref,
            actual: args.get(index).map_or(SlotType::I64, Value::slot_type),
        }),
    }
}

/// Reads the `i64` word at `index`, refusing with the offending
/// argument's own kind.
pub(crate) fn i64_arg(args: &[Value], index: usize) -> Result<i64, Trap> {
    match args.get(index) {
        Some(Value::I64(v)) => Ok(*v),
        _ => Err(Trap::SlotTypeMismatch {
            slot: u8::try_from(index).expect("slot index fits u8"),
            expected: SlotType::I64,
            actual: args.get(index).map_or(SlotType::I64, Value::slot_type),
        }),
    }
}

/// Reads the `f64` word at `index`, refusing with the offending
/// argument's own kind.
pub(crate) fn f64_arg(args: &[Value], index: usize) -> Result<f64, Trap> {
    match args.get(index) {
        Some(Value::F64(v)) => Ok(*v),
        _ => Err(Trap::SlotTypeMismatch {
            slot: u8::try_from(index).expect("slot index fits u8"),
            expected: SlotType::F64,
            actual: args.get(index).map_or(SlotType::I64, Value::slot_type),
        }),
    }
}

pub(crate) fn println_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let text = heaps.str_at(ref_arg(args, 0)?)?;
    out.write_line(text)?;
    Ok(NativeResult::Word(Value::I64(0)))
}

pub(crate) fn print_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let text = heaps.str_at(ref_arg(args, 0)?)?;
    out.write_str(text)?;
    Ok(NativeResult::Word(Value::I64(0)))
}

/// `eprintln`: the console face's stderr twin of [`println_fn`].
pub(crate) fn eprintln_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let text = heaps.str_at(ref_arg(args, 0)?)?;
    out.write_err_line(text)?;
    Ok(NativeResult::Word(Value::I64(0)))
}

/// `eprint`: the console face's stderr twin of [`print_fn`].
pub(crate) fn eprint_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let text = heaps.str_at(ref_arg(args, 0)?)?;
    out.write_err_str(text)?;
    Ok(NativeResult::Word(Value::I64(0)))
}

/// `read_line`: one line from the process's stdin, the checked ok/none
/// span — end of input (or a stdin the process cannot read) answers the
/// none member, never a trap. The line answer strips its trailing
/// newline (and a CRLF pair's carriage return).
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn read_line_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    _args: &[Value],
) -> Result<NativeResult, Trap> {
    let mut line = String::new();
    let read = std::io::stdin().read_line(&mut line);
    Ok(match read {
        // zero bytes is end of input; a read failure folds into the
        // same none member (the checked read's convention)
        Ok(0) | Err(_) => NativeResult::Tag {
            tag: PLATFORM_NONE,
            payload: Value::I64(0),
        },
        Ok(_) => {
            if line.ends_with('\n') {
                line.pop();
                if line.ends_with('\r') {
                    line.pop();
                }
            }
            NativeResult::Tag {
                tag: PLATFORM_IO_OK,
                payload: heaps.new_str(line),
            }
        },
    })
}

/// The bare trap primitive's body: the message rides the trap out to the
/// drive; no print, no exit decision — the host's halt policy applies
/// above the engine. Never answers.
pub(crate) fn trap_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let text = heaps.str_at(ref_arg(args, 0)?)?;
    Err(Trap::Die {
        message: text.to_owned(),
    })
}
