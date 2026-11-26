//! The platform handler (H5.5, E1): the world surface as an installed
//! handler behind the platform declaration — the effect-handler form the
//! world-touching natives migrated to out of the native registry's
//! two-module split.
//!
//! A platform is data: a name (the module name the lowered world calls'
//! pool FuncRefs address — the source's `platform "..."` declaration
//! names it, and a host installs a platform under that name or the
//! module's install refuses) and the world capabilities it serves. The
//! ops are a fixed table — the world-call ABI, densely indexed — and a
//! platform serves the ops its capabilities own; an unserved op keeps
//! its table position as a reserved trap, defense in depth under the
//! install-time capability validation: a world call site's FuncRef
//! checks against the installed platform's served ops when its module
//! installs, so a console-only platform hosting a `read_file` call site
//! is a structured install error, never a runtime surprise.
//!
//! The capability idiom (E1): the `ds` CLI installs [`Platform::std`]
//! as the ambient default for scripts and [`Platform::embed`] beside it
//! under its own name, so a source declaring `platform "embed"` runs
//! against the console-only face; a library embedder constructs a bare
//! VM and installs nothing — no platform means no world surface — and
//! a test embedder installs the subset it wants to serve. The `time`
//! capability answers the wall clock, the monotonic clock, and the
//! parking sleep (the 2026-11-21 timer ruling).

use ds_abi::SlotType;

use crate::{
    native::{NativeFn, NativeFunction, NativeResult, OutputSink},
    net,
    object::Heaps,
    value::Value,
    vm::Trap,
};

/// One world capability of a platform (H5's provides list).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Capability {
    /// The printing surface: `println`, `print`, the stderr twins
    /// `eprintln`/`eprint`, and the stdin read `read_line`.
    Console,
    /// The file surface: the readers, writers, `fs_list`, and the path
    /// round (`mkdir`, `remove`, `fs_size`) beside the handle face.
    Fsys,
    /// The TCP surface: connect/send/recv/close, the listener face
    /// (listen/accept/listener-close), `http_get`.
    Net,
    /// The clock surface: the wall clock, the monotonic clock, the
    /// parking sleep (the 2026-11-21 timer ruling), and the local UTC
    /// offset read (the datetime face's one world touch).
    Time,
    /// The TLS surface (H5.5's 2026-11-17 ruling): https over rustls,
    /// selected or omitted per host — the `https_get` op, the
    /// https-routing `http_get` body, and the server half's
    /// `net_accept_tls` (the accepted connection's upgrade over the
    /// host-pinned identity, ruled 2026-11-23).
    Tls,
    /// The process surface (the CLI process surface, ruled 2026-11-21):
    /// string argv, the environment read, process spawn (the piped
    /// capture half and its linear `proc_handle`), and the
    /// deliberate-status `exit(code)` row.
    Proc,
}

impl Capability {
    /// The capability's name, as the provides list spells it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Console => "console",
            Self::Fsys => "fsys",
            Self::Net => "net",
            Self::Time => "time",
            Self::Tls => "tls",
            Self::Proc => "proc",
        }
    }

    /// The capability a provides-list name names, `None` when outside
    /// the vocabulary.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "console" => Some(Self::Console),
            "fsys" => Some(Self::Fsys),
            "net" => Some(Self::Net),
            "time" => Some(Self::Time),
            "tls" => Some(Self::Tls),
            "proc" => Some(Self::Proc),
            _ => None,
        }
    }
}

/// The default platform's name (H5, E1): the `ds` CLI's provider, the
/// name the stdlib ambient's declaration writes and world calls lower
/// against when no other declaration names one.
pub const STD_NAME: &str = "std";

/// The embed platform's name (H5): the minimal host-facing surface a
/// bare deployment declares — `platform "embed"` — and the run path's
/// second installed handler serves.
pub const EMBED_NAME: &str = "embed";

/// `println`: console.
pub const OP_PRINTLN: u32 = 0;
/// `print`: console.
pub const OP_PRINT: u32 = 1;
/// `read_file`: fsys.
pub const OP_READ_FILE: u32 = 2;
/// `write_file`: fsys.
pub const OP_WRITE_FILE: u32 = 3;
/// `file_exists`: fsys.
pub const OP_FILE_EXISTS: u32 = 4;
/// `fs_list`: fsys.
pub const OP_FS_LIST: u32 = 5;
/// `file_size`: fsys.
pub const OP_FILE_SIZE: u32 = 6;
/// `net_connect`: net.
pub const OP_NET_CONNECT: u32 = 7;
/// `net_send`: net.
pub const OP_NET_SEND: u32 = 8;
/// `net_recv`: net.
pub const OP_NET_RECV: u32 = 9;
/// `net_close`: net (the raw idempotent-close seam, `close_raw`'s
/// contract — the handle deinit pairs with it).
pub const OP_NET_CLOSE: u32 = 10;
/// `http_get`: net.
pub const OP_HTTP_GET: u32 = 11;
/// `open_file`: fsys (answers the package-owned file handle's word).
pub const OP_OPEN_FILE: u32 = 12;
/// `read_raw`: fsys (the raw exact-read seam the ds-source face wraps).
pub const OP_READ_RAW: u32 = 13;
/// `close_raw`: fsys (the raw idempotent-close seam).
pub const OP_CLOSE_RAW: u32 = 14;
/// `time`: time (the clock op, epoch nanoseconds).
pub const OP_TIME: u32 = 15;
/// `https_get`: tls (the TLS capability's own op, https only — no
/// ambient spelling declares it; the net face's `http_get` routes
/// https URLs onto the tier).
pub const OP_HTTPS_GET: u32 = 16;
/// `read_line`: console (the stdin read, one line per call).
pub const OP_READ_LINE: u32 = 17;
/// `eprintln`: console (the stderr twin of `println`).
pub const OP_EPRINTLN: u32 = 18;
/// `eprint`: console (the stderr twin of `print`).
pub const OP_EPRINT: u32 = 19;
/// `args`: proc (the host-staged process arguments).
pub const OP_ARGS: u32 = 20;
/// `env`: proc (the environment-variable read).
pub const OP_ENV: u32 = 21;
/// `proc_run`: proc (spawn one child, answer its exit status).
pub const OP_PROC_RUN: u32 = 22;
/// `exit`: proc (the deliberate-status row: the native never answers,
/// the status riding the trap out to the drive).
pub const OP_EXIT: u32 = 23;
/// `mono`: time (the monotonic clock read, the 2026-11-21 timer ruling).
pub const OP_MONO: u32 = 24;
/// `sleep`: time (the parking sleep op over the timer-only wait shape).
pub const OP_SLEEP: u32 = 25;
/// `net_listen`: net (binds a listener, answers its handle's word).
pub const OP_NET_LISTEN: u32 = 26;
/// `net_accept`: net (takes one pending connection, answers the
/// accepted connection's handle's word; parks under a reactor).
pub const OP_NET_ACCEPT: u32 = 27;
/// `net_listener_close`: net (the raw idempotent-close seam the
/// `net_listener` deinit pairs with, `net_close`'s contract).
/// `net_local_addr`: net (the bound listener's local address, the
/// port-0 bind's report).
pub const OP_NET_LISTENER_CLOSE: u32 = 28;
/// `proc_spawn`: proc (spawns one child over pipes, answers the
/// linear `proc_handle`'s word; the ambient's total spelling pairs
/// with the fallible span on the printing-halt path).
pub const OP_PROC_SPAWN: u32 = 29;
/// `proc_write`: proc (writes the child's stdin pipe, the unit span
/// on success).
pub const OP_PROC_WRITE: u32 = 30;
/// `proc_read`: proc (reads the child's captured stdout, one read of
/// at most the requested bytes).
pub const OP_PROC_READ: u32 = 31;
/// `proc_wait`: proc (waits for the child, answers its exit status
/// idempotently).
pub const OP_PROC_WAIT: u32 = 32;
/// `proc_close`: proc (the raw idempotent-close seam the `proc_handle`
/// deinit pairs with — kills and reaps a still-running child).
pub const OP_PROC_CLOSE: u32 = 33;
/// `proc_close_in`: proc (closes the child's stdin pipe, the child's
/// end-of-input while the handle stays live; total and idempotent).
pub const OP_PROC_CLOSE_IN: u32 = 34;
/// `proc_read_err`: proc (reads the child's captured stderr, one read
/// of at most the requested bytes).
pub const OP_PROC_READ_ERR: u32 = 35;
/// `proc_try_wait`: proc (polls the child without blocking — the
/// already-reaped status under the ok member, none while it runs).
pub const OP_PROC_TRY_WAIT: u32 = 36;
/// `net_local_addr`: net (the bound listener's local address, the
/// port-0 bind's report — the `net_listener` face's one address op).
pub const OP_NET_LOCAL_ADDR: u32 = 37;
/// `net_accept_tls`: tls (takes one pending connection off the bound
/// listener, upgrades it over the host-pinned rustls server identity,
/// answers the accepted connection's handle word — the raw seam
/// behind the ambient's total `accept_tls`).
pub const OP_NET_ACCEPT_TLS: u32 = 38;
/// `mkdir`: fsys (creates one directory, a single level; the io error
/// class answers the fallible span's failure side).
pub const OP_MKDIR: u32 = 39;
/// `remove`: fsys (removes one entry — a file or link over
/// `remove_file`, a directory over the empty-only `remove_dir`).
pub const OP_REMOVE: u32 = 40;
/// `fs_size`: fsys (the entry's byte count, the same read as `file_size`).
pub const OP_FS_SIZE: u32 = 41;
/// `utc_offset`: time (the host's local UTC offset for a timestamp,
/// seconds east — the datetime face's one world touch; the calendar
/// math and formatting compose in @std ds source over it).
pub const OP_UTC_OFFSET: u32 = 42;

/// The world-call ABI's width: the fixed op table's length.
pub const OP_COUNT: u32 = 43;

/// One op of the fixed table: its call name, owning capability,
/// signature, result span, and body.
struct Op {
    name: &'static str,
    capability: Capability,
    params: &'static [SlotType],
    result_words: u8,
    func: NativeFn,
}

/// The fixed op table, densely indexed: the platform ABI both the
/// lowering's world natives and the installed platform serve. The
/// indices are contract (pool FuncRefs address them positionally); the
/// fallible fsys and net ops each answer the two-word platform span.
#[rustfmt::skip]
const OPS: [Op; OP_COUNT as usize] = [
    Op { name: "println",     capability: Capability::Console, params: &[SlotType::Ref],        result_words: 1, func: crate::native::println_fn },
    Op { name: "print",       capability: Capability::Console, params: &[SlotType::Ref],        result_words: 1, func: crate::native::print_fn },
    Op { name: "read_file",   capability: Capability::Fsys,    params: &[SlotType::Ref],        result_words: 2, func: crate::native::read_file_fn },
    Op { name: "write_file",  capability: Capability::Fsys,    params: &[SlotType::Ref, SlotType::Ref], result_words: 2, func: crate::native::write_file_fn },
    Op { name: "file_exists", capability: Capability::Fsys,    params: &[SlotType::Ref],        result_words: 1, func: crate::native::file_exists_fn },
    Op { name: "fs_list",     capability: Capability::Fsys,    params: &[SlotType::Ref],        result_words: 2, func: crate::native::fs_list_fn },
    Op { name: "file_size",   capability: Capability::Fsys,    params: &[SlotType::Ref],        result_words: 2, func: crate::native::file_size_fn },
    Op { name: "net_connect", capability: Capability::Net,     params: &[SlotType::Ref, SlotType::I64], result_words: 2, func: net::net_connect_fn },
    Op { name: "net_send",    capability: Capability::Net,     params: &[SlotType::I64, SlotType::Ref], result_words: 2, func: net::net_send_fn },
    Op { name: "net_recv",    capability: Capability::Net,     params: &[SlotType::I64, SlotType::I64], result_words: 2, func: net::net_recv_fn },
    Op { name: "net_close",   capability: Capability::Net,     params: &[SlotType::I64],        result_words: 1, func: net::net_close_fn },
    Op { name: "http_get",    capability: Capability::Net,     params: &[SlotType::Ref],        result_words: 2, func: net::http_get_fn },
    Op { name: "open_file",    capability: Capability::Fsys,    params: &[SlotType::Ref],        result_words: 2, func: crate::fsys::open_fn },
    Op { name: "read_raw",    capability: Capability::Fsys,    params: &[SlotType::I64, SlotType::I64], result_words: 2, func: crate::fsys::read_raw_fn },
    Op { name: "close_raw",   capability: Capability::Fsys,    params: &[SlotType::I64],        result_words: 1, func: crate::fsys::close_raw_fn },
    Op { name: "time",        capability: Capability::Time,    params: &[],                    result_words: 1, func: crate::native::time_fn },
    Op { name: "https_get",   capability: Capability::Tls,     params: &[SlotType::Ref],       result_words: 2, func: crate::tls::https_get_fn },
    Op { name: "read_line",   capability: Capability::Console, params: &[],                    result_words: 2, func: crate::native::read_line_fn },
    Op { name: "eprintln",    capability: Capability::Console, params: &[SlotType::Ref],        result_words: 1, func: crate::native::eprintln_fn },
    Op { name: "eprint",      capability: Capability::Console, params: &[SlotType::Ref],        result_words: 1, func: crate::native::eprint_fn },
    Op { name: "args",        capability: Capability::Proc,    params: &[],                    result_words: 1, func: crate::proc::args_fn },
    Op { name: "env",         capability: Capability::Proc,    params: &[SlotType::Ref],        result_words: 2, func: crate::proc::env_fn },
    Op { name: "proc_run",    capability: Capability::Proc,    params: &[SlotType::Ref, SlotType::Ref], result_words: 2, func: crate::proc::proc_run_fn },
    Op { name: "exit",        capability: Capability::Proc,    params: &[SlotType::I64],       result_words: 1, func: crate::proc::exit_fn },
    Op { name: "mono",        capability: Capability::Time,    params: &[],                    result_words: 1, func: crate::native::mono_fn },
    Op { name: "sleep",       capability: Capability::Time,    params: &[SlotType::I64],       result_words: 1, func: crate::native::sleep_fn },
    Op { name: "net_listen", capability: Capability::Net,     params: &[SlotType::Ref, SlotType::I64], result_words: 2, func: net::net_listen_fn },
    Op { name: "net_accept",  capability: Capability::Net,     params: &[SlotType::I64],       result_words: 2, func: net::net_accept_fn },
    Op { name: "net_listener_close", capability: Capability::Net, params: &[SlotType::I64], result_words: 1, func: net::net_listener_close_fn },
    Op { name: "proc_spawn", capability: Capability::Proc, params: &[SlotType::Ref, SlotType::Ref], result_words: 2, func: crate::proc::proc_spawn_fn },
    Op { name: "proc_write", capability: Capability::Proc, params: &[SlotType::I64, SlotType::Ref], result_words: 2, func: crate::proc::proc_write_fn },
    Op { name: "proc_read", capability: Capability::Proc, params: &[SlotType::I64, SlotType::I64], result_words: 2, func: crate::proc::proc_read_fn },
    Op { name: "proc_wait", capability: Capability::Proc, params: &[SlotType::I64], result_words: 2, func: crate::proc::proc_wait_fn },
    Op { name: "proc_close", capability: Capability::Proc, params: &[SlotType::I64], result_words: 1, func: crate::proc::proc_close_fn },
    Op { name: "proc_close_in", capability: Capability::Proc, params: &[SlotType::I64], result_words: 1, func: crate::proc::proc_close_in_fn },
    Op { name: "proc_read_err", capability: Capability::Proc, params: &[SlotType::I64, SlotType::I64], result_words: 2, func: crate::proc::proc_read_err_fn },
    Op { name: "proc_try_wait", capability: Capability::Proc, params: &[SlotType::I64], result_words: 2, func: crate::proc::proc_try_wait_fn },
    Op { name: "net_local_addr", capability: Capability::Net, params: &[SlotType::I64], result_words: 1, func: net::net_local_addr_fn },
    Op { name: "net_accept_tls", capability: Capability::Tls, params: &[SlotType::I64], result_words: 2, func: crate::tls::net_accept_tls_fn },
    Op { name: "mkdir",       capability: Capability::Fsys,    params: &[SlotType::Ref],        result_words: 2, func: crate::fsys::mkdir_fn },
    Op { name: "remove",      capability: Capability::Fsys,    params: &[SlotType::Ref],        result_words: 2, func: crate::fsys::remove_fn },
    Op { name: "fs_size",     capability: Capability::Fsys,    params: &[SlotType::Ref],        result_words: 2, func: crate::fsys::fs_size_fn },
    Op { name: "utc_offset", capability: Capability::Time,    params: &[SlotType::I64],        result_words: 1, func: crate::tz::utc_offset_fn },
];

macro_rules! unserved {
    ($name:ident at $index:literal) => {
        fn $name(
            _heaps: &mut Heaps,
            _out: &mut OutputSink,
            _args: &[Value],
        ) -> Result<NativeResult, Trap> {
            Err(Trap::UnservedWorldOp { op: $index })
        }
    };
}

unserved!(unserved_op_0 at 0);
unserved!(unserved_op_1 at 1);
unserved!(unserved_op_2 at 2);
unserved!(unserved_op_3 at 3);
unserved!(unserved_op_4 at 4);
unserved!(unserved_op_5 at 5);
unserved!(unserved_op_6 at 6);
unserved!(unserved_op_7 at 7);
unserved!(unserved_op_8 at 8);
unserved!(unserved_op_9 at 9);
unserved!(unserved_op_10 at 10);
unserved!(unserved_op_11 at 11);
unserved!(unserved_op_12 at 12);
unserved!(unserved_op_13 at 13);
unserved!(unserved_op_14 at 14);
unserved!(unserved_op_15 at 15);
unserved!(unserved_op_16 at 16);
unserved!(unserved_op_17 at 17);
unserved!(unserved_op_18 at 18);
unserved!(unserved_op_19 at 19);
unserved!(unserved_op_20 at 20);
unserved!(unserved_op_21 at 21);
unserved!(unserved_op_22 at 22);
unserved!(unserved_op_23 at 23);
unserved!(unserved_op_24 at 24);
unserved!(unserved_op_25 at 25);
unserved!(unserved_op_26 at 26);
unserved!(unserved_op_27 at 27);
unserved!(unserved_op_28 at 28);
unserved!(unserved_op_29 at 29);
unserved!(unserved_op_30 at 30);
unserved!(unserved_op_31 at 31);
unserved!(unserved_op_32 at 32);
unserved!(unserved_op_33 at 33);
unserved!(unserved_op_34 at 34);
unserved!(unserved_op_35 at 35);
unserved!(unserved_op_36 at 36);
unserved!(unserved_op_37 at 37);
unserved!(unserved_op_38 at 38);
unserved!(unserved_op_39 at 39);
unserved!(unserved_op_40 at 40);
unserved!(unserved_op_41 at 41);
unserved!(unserved_op_42 at 42);

/// The reserved trap bodies, index-parallel to [`OPS`].
#[rustfmt::skip]
const UNSERVED: [NativeFn; OP_COUNT as usize] = [
    unserved_op_0, unserved_op_1, unserved_op_2, unserved_op_3,
    unserved_op_4, unserved_op_5, unserved_op_6, unserved_op_7,
    unserved_op_8, unserved_op_9, unserved_op_10, unserved_op_11,
    unserved_op_12, unserved_op_13, unserved_op_14, unserved_op_15,
    unserved_op_16, unserved_op_17, unserved_op_18, unserved_op_19,
    unserved_op_20, unserved_op_21, unserved_op_22, unserved_op_23,
    unserved_op_24, unserved_op_25, unserved_op_26, unserved_op_27,
    unserved_op_28, unserved_op_29, unserved_op_30, unserved_op_31,
    unserved_op_32, unserved_op_33, unserved_op_34, unserved_op_35,
    unserved_op_36, unserved_op_37, unserved_op_38, unserved_op_39,
    unserved_op_40, unserved_op_41, unserved_op_42,
];

/// An installed platform handler: the world surface one named platform
/// serves.
#[derive(Clone, Debug)]
pub struct Platform {
    name: String,
    capabilities: Vec<Capability>,
}

impl Platform {
    /// The default platform (H5, E1): `std`, every capability (the tls
    /// tier included — the `ds` CLI host selects it, an embedding host
    /// omits it), the surface the `ds` CLI provides for scripts.
    #[must_use]
    pub fn std() -> Self {
        Self::new("std", &[
            Capability::Console,
            Capability::Fsys,
            Capability::Net,
            Capability::Time,
            Capability::Tls,
            Capability::Proc,
        ])
    }

    /// The embed platform (H5): the minimal host-facing surface — the
    /// console face alone, so a deployment embedding the VM serves
    /// exactly print/println; every world op beyond it stays a reserved
    /// trap and install-time validation refuses its call sites as
    /// `UnprovidedPlatformOp`.
    #[must_use]
    pub fn embed() -> Self {
        Self::new(EMBED_NAME, &[Capability::Console])
    }

    /// A platform named `name` serving exactly `capabilities` — the
    /// test and embedding entrance (E1's capability idiom): install the
    /// subset to serve, nothing more.
    #[must_use]
    pub fn new(name: impl Into<String>, capabilities: &[Capability]) -> Self {
        Self {
            name: name.into(),
            capabilities: capabilities.to_vec(),
        }
    }

    /// The platform's name: the module name the lowered world calls
    /// address (the source's platform declaration names it).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The capabilities the platform serves.
    #[must_use]
    pub fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    /// Whether the platform serves the op at `index` of the fixed
    /// table: in range and its capability provided.
    #[must_use]
    pub fn serves(&self, index: u32) -> bool {
        OPS.get(index as usize)
            .is_some_and(|op| self.capabilities.contains(&op.capability))
    }

    /// The capability owning the op at `index`, `None` out of range.
    #[must_use]
    pub fn op_capability(index: u32) -> Option<Capability> {
        OPS.get(index as usize).map(|op| op.capability)
    }
}

/// The platform's native table: the fixed op table with every unserved
/// op as a reserved trap — the table a host installs under the
/// platform's name.
#[must_use]
pub fn platform_module(platform: &Platform) -> Vec<NativeFunction> {
    OPS.iter()
        .zip(UNSERVED)
        .enumerate()
        .map(|(index, (op, unserved))| {
            let index = u32::try_from(index).expect("op index fits u32");
            // the tls capability's host-level selection (H5.5's
            // 2026-11-17 ruling): the net face's `http_get` slot carries
            // the https-routing body only where the platform provides
            // the tls capability — the https op's own serving check — so
            // an omitting host keeps the plain body that refuses https
            // URLs, defense in depth under the install-time validation
            let func = if index == OP_HTTP_GET && platform.serves(OP_HTTPS_GET) {
                crate::tls::http_get_tls_fn
            } else if platform.serves(index) {
                op.func
            } else {
                unserved
            };
            NativeFunction {
                name: op.name,
                params: op.params,
                variadic: false,
                result_words: op.result_words,
                control: None,
                channel: None,
                func,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixed table is the contract: every op sits at its reserved
    /// index with its owning capability and result span width.
    #[test]
    fn the_fixed_op_table_is_contract() {
        assert_eq!(OPS[OP_PRINTLN as usize].name, "println");
        assert_eq!(OPS[OP_PRINT as usize].name, "print");
        assert_eq!(OPS[OP_READ_FILE as usize].name, "read_file");
        assert_eq!(OPS[OP_WRITE_FILE as usize].name, "write_file");
        assert_eq!(OPS[OP_FILE_EXISTS as usize].name, "file_exists");
        assert_eq!(OPS[OP_FS_LIST as usize].name, "fs_list");
        assert_eq!(OPS[OP_FILE_SIZE as usize].name, "file_size");
        assert_eq!(OPS[OP_NET_CONNECT as usize].name, "net_connect");
        assert_eq!(OPS[OP_NET_SEND as usize].name, "net_send");
        assert_eq!(OPS[OP_NET_RECV as usize].name, "net_recv");
        assert_eq!(OPS[OP_NET_CLOSE as usize].name, "net_close");
        assert_eq!(OPS[OP_HTTP_GET as usize].name, "http_get");
        assert_eq!(OPS[OP_OPEN_FILE as usize].name, "open_file");
        assert_eq!(OPS[OP_READ_RAW as usize].name, "read_raw");
        assert_eq!(OPS[OP_CLOSE_RAW as usize].name, "close_raw");
        assert_eq!(OPS[OP_TIME as usize].name, "time");
        assert_eq!(OPS[OP_TIME as usize].capability, Capability::Time);
        assert_eq!(OPS[OP_HTTPS_GET as usize].name, "https_get");
        assert_eq!(OPS[OP_HTTPS_GET as usize].capability, Capability::Tls);
        assert_eq!(OPS[OP_READ_LINE as usize].name, "read_line");
        assert_eq!(OPS[OP_READ_LINE as usize].capability, Capability::Console);
        assert_eq!(OPS[OP_EPRINTLN as usize].name, "eprintln");
        assert_eq!(OPS[OP_EPRINT as usize].name, "eprint");
        assert_eq!(OPS[OP_ARGS as usize].name, "args");
        assert_eq!(OPS[OP_ARGS as usize].capability, Capability::Proc);
        assert_eq!(OPS[OP_ENV as usize].name, "env");
        assert_eq!(OPS[OP_PROC_RUN as usize].name, "proc_run");
        assert_eq!(OPS[OP_EXIT as usize].name, "exit");
        assert_eq!(OPS[OP_MONO as usize].name, "mono");
        assert_eq!(OPS[OP_MONO as usize].capability, Capability::Time);
        assert_eq!(OPS[OP_SLEEP as usize].name, "sleep");
        assert_eq!(OPS[OP_SLEEP as usize].capability, Capability::Time);
        assert_eq!(OPS[OP_NET_LISTEN as usize].name, "net_listen");
        assert_eq!(OPS[OP_NET_LISTEN as usize].capability, Capability::Net);
        assert_eq!(OPS[OP_NET_ACCEPT as usize].name, "net_accept");
        assert_eq!(OPS[OP_NET_ACCEPT as usize].capability, Capability::Net);
        assert_eq!(
            OPS[OP_NET_LISTENER_CLOSE as usize].name,
            "net_listener_close"
        );
        assert_eq!(OPS[OP_NET_LOCAL_ADDR as usize].name, "net_local_addr");
        assert_eq!(OPS[OP_NET_LOCAL_ADDR as usize].capability, Capability::Net);
        assert_eq!(OPS[OP_NET_ACCEPT_TLS as usize].name, "net_accept_tls");
        assert_eq!(OPS[OP_NET_ACCEPT_TLS as usize].capability, Capability::Tls);
        assert_eq!(OPS[OP_MKDIR as usize].name, "mkdir");
        assert_eq!(OPS[OP_MKDIR as usize].capability, Capability::Fsys);
        assert_eq!(OPS[OP_REMOVE as usize].name, "remove");
        assert_eq!(OPS[OP_REMOVE as usize].capability, Capability::Fsys);
        assert_eq!(OPS[OP_FS_SIZE as usize].name, "fs_size");
        assert_eq!(OPS[OP_FS_SIZE as usize].capability, Capability::Fsys);
        assert_eq!(OPS[OP_UTC_OFFSET as usize].name, "utc_offset");
        assert_eq!(OPS[OP_UTC_OFFSET as usize].capability, Capability::Time);
        assert_eq!(
            OPS[OP_NET_LISTENER_CLOSE as usize].capability,
            Capability::Net
        );
        assert_eq!(OPS[OP_PROC_SPAWN as usize].name, "proc_spawn");
        assert_eq!(OPS[OP_PROC_SPAWN as usize].capability, Capability::Proc);
        assert_eq!(OPS[OP_PROC_WRITE as usize].name, "proc_write");
        assert_eq!(OPS[OP_PROC_WRITE as usize].capability, Capability::Proc);
        assert_eq!(OPS[OP_PROC_READ as usize].name, "proc_read");
        assert_eq!(OPS[OP_PROC_READ as usize].capability, Capability::Proc);
        assert_eq!(OPS[OP_PROC_WAIT as usize].name, "proc_wait");
        assert_eq!(OPS[OP_PROC_WAIT as usize].capability, Capability::Proc);
        assert_eq!(OPS[OP_PROC_CLOSE as usize].name, "proc_close");
        assert_eq!(OPS[OP_PROC_CLOSE as usize].capability, Capability::Proc);
        assert_eq!(OPS[OP_PROC_CLOSE_IN as usize].name, "proc_close_in");
        assert_eq!(OPS[OP_PROC_CLOSE_IN as usize].capability, Capability::Proc);
        assert_eq!(OPS[OP_PROC_READ_ERR as usize].name, "proc_read_err");
        assert_eq!(OPS[OP_PROC_READ_ERR as usize].capability, Capability::Proc);
        assert_eq!(OPS[OP_PROC_TRY_WAIT as usize].name, "proc_try_wait");
        assert_eq!(OPS[OP_PROC_TRY_WAIT as usize].capability, Capability::Proc);
        assert_eq!(OPS.len(), OP_COUNT as usize);
        for (index, op) in OPS.iter().enumerate() {
            let index = u32::try_from(index).expect("op index fits u32");
            assert_eq!(
                Platform::op_capability(index),
                Some(op.capability),
                "op {index}'s capability is public"
            );
        }
        assert_eq!(Platform::op_capability(OP_COUNT), None, "past the table");
    }

    /// The default platform serves everything; a subset platform serves
    /// exactly its capabilities' ops, and its table keeps every unserved
    /// op's position as a trap with the same signature contract.
    #[test]
    fn a_platform_serves_exactly_its_capabilities() {
        let std = Platform::std();
        assert_eq!(std.name(), "std");
        for index in 0..OP_COUNT {
            assert!(std.serves(index), "std serves op {index}");
        }
        let console = Platform::new("test", &[Capability::Console]);
        assert!(console.serves(OP_PRINTLN));
        assert!(console.serves(OP_PRINT));
        assert!(console.serves(OP_READ_LINE));
        assert!(console.serves(OP_EPRINTLN));
        assert!(console.serves(OP_EPRINT));
        for index in [
            OP_READ_FILE,
            OP_WRITE_FILE,
            OP_FILE_EXISTS,
            OP_FS_LIST,
            OP_FILE_SIZE,
            OP_OPEN_FILE,
            OP_READ_RAW,
            OP_CLOSE_RAW,
            OP_MKDIR,
            OP_REMOVE,
            OP_FS_SIZE,
        ] {
            assert!(!console.serves(index), "a console platform refuses fsys");
        }
        assert!(!console.serves(OP_TIME), "a console platform refuses time");
        assert!(!console.serves(OP_MONO), "a console platform refuses mono");
        assert!(
            !console.serves(OP_SLEEP),
            "a console platform refuses sleep"
        );
        assert!(
            !console.serves(OP_UTC_OFFSET),
            "a console platform refuses the offset read"
        );
        for index in [OP_ARGS, OP_ENV, OP_PROC_RUN, OP_EXIT] {
            assert!(!console.serves(index), "a console platform refuses proc");
        }
        for index in [
            OP_PROC_SPAWN,
            OP_PROC_WRITE,
            OP_PROC_READ,
            OP_PROC_WAIT,
            OP_PROC_CLOSE,
            OP_PROC_CLOSE_IN,
            OP_PROC_READ_ERR,
            OP_PROC_TRY_WAIT,
        ] {
            assert!(
                !console.serves(index),
                "a console platform refuses the capture half"
            );
        }
        for index in [
            OP_NET_CONNECT,
            OP_NET_SEND,
            OP_NET_RECV,
            OP_NET_CLOSE,
            OP_HTTP_GET,
            OP_HTTPS_GET,
            OP_NET_LISTEN,
            OP_NET_ACCEPT,
            OP_NET_LISTENER_CLOSE,
            OP_NET_LOCAL_ADDR,
            OP_NET_ACCEPT_TLS,
        ] {
            assert!(!console.serves(index), "a console platform refuses net");
        }
        let table = platform_module(&console);
        assert_eq!(table.len(), OP_COUNT as usize);
        assert_eq!(table[OP_PRINTLN as usize].name, "println");
        assert_eq!(table[OP_READ_FILE as usize].result_words, 2);
    }

    /// The embed platform serves the console face alone: print/println,
    /// their stderr twins, and the stdin read answer, and every world op
    /// beyond them stays a reserved trap at its contract index —
    /// install-time validation is the refusal the e2e pins.
    #[test]
    fn the_embed_platform_serves_the_console_face_alone() {
        let embed = Platform::embed();
        assert_eq!(embed.name(), EMBED_NAME);
        assert!(embed.serves(OP_PRINTLN), "embed serves println");
        assert!(embed.serves(OP_PRINT), "embed serves print");
        assert!(embed.serves(OP_READ_LINE), "embed serves read_line");
        assert!(embed.serves(OP_EPRINTLN), "embed serves eprintln");
        assert!(embed.serves(OP_EPRINT), "embed serves eprint");
        for index in [
            OP_READ_FILE,
            OP_WRITE_FILE,
            OP_FILE_EXISTS,
            OP_FS_LIST,
            OP_FILE_SIZE,
            OP_OPEN_FILE,
            OP_READ_RAW,
            OP_CLOSE_RAW,
            OP_NET_CONNECT,
            OP_NET_SEND,
            OP_NET_RECV,
            OP_NET_CLOSE,
            OP_HTTP_GET,
            OP_NET_LISTEN,
            OP_NET_ACCEPT,
            OP_NET_LISTENER_CLOSE,
            OP_NET_LOCAL_ADDR,
            OP_TIME,
            OP_HTTPS_GET,
            OP_NET_ACCEPT_TLS,
            OP_ARGS,
            OP_ENV,
            OP_PROC_RUN,
            OP_EXIT,
            OP_PROC_SPAWN,
            OP_PROC_WRITE,
            OP_PROC_READ,
            OP_PROC_WAIT,
            OP_PROC_CLOSE,
            OP_PROC_CLOSE_IN,
            OP_PROC_READ_ERR,
            OP_PROC_TRY_WAIT,
            OP_MKDIR,
            OP_REMOVE,
            OP_FS_SIZE,
            OP_UTC_OFFSET,
        ] {
            assert!(
                !embed.serves(index),
                "the embed platform refuses op {index}"
            );
        }
        let table = platform_module(&embed);
        let println_body: NativeFn = crate::native::println_fn;
        assert_eq!(
            table[OP_PRINTLN as usize].func as usize, println_body as usize,
            "the console op carries its real body"
        );
        assert_eq!(
            table[OP_TIME as usize].func as usize, UNSERVED[OP_TIME as usize] as usize,
            "an unserved op keeps its reserved trap"
        );
    }

    /// The tls capability's host-level selection: a net-without-tls
    /// platform serves `http_get` with the plain body and keeps the
    /// `https_get` and `net_accept_tls` ops as their reserved traps,
    /// while the default platform carries the routing body and both
    /// served ops.
    #[test]
    fn the_tls_capability_selects_the_https_bodies() {
        let no_tls = Platform::new("embed", &[Capability::Net]);
        assert!(no_tls.serves(OP_HTTP_GET), "net serves http_get");
        assert!(!no_tls.serves(OP_HTTPS_GET), "the tls op stays unserved");
        assert!(
            !no_tls.serves(OP_NET_ACCEPT_TLS),
            "the server half's tls op stays unserved"
        );
        let plain = platform_module(&no_tls);
        let plain_body: NativeFn = net::http_get_fn;
        assert_eq!(
            plain[OP_HTTP_GET as usize].func as usize, plain_body as usize,
            "an omitting host keeps the plain body"
        );
        let std = platform_module(&Platform::std());
        let routing_body: NativeFn = crate::tls::http_get_tls_fn;
        assert_eq!(
            std[OP_HTTP_GET as usize].func as usize, routing_body as usize,
            "a providing host carries the routing body"
        );
        let https_body: NativeFn = crate::tls::https_get_fn;
        assert_eq!(
            std[OP_HTTPS_GET as usize].func as usize, https_body as usize,
            "a providing host serves the https op"
        );
        let accept_body: NativeFn = crate::tls::net_accept_tls_fn;
        assert_eq!(
            std[OP_NET_ACCEPT_TLS as usize].func as usize, accept_body as usize,
            "a providing host serves the server half's tls op"
        );
        let trap = plain[OP_HTTPS_GET as usize].func;
        assert_eq!(
            trap as usize, UNSERVED[OP_HTTPS_GET as usize] as usize,
            "an omitting host keeps the https op's reserved trap"
        );
        assert_eq!(
            plain[OP_NET_ACCEPT_TLS as usize].func as usize,
            UNSERVED[OP_NET_ACCEPT_TLS as usize] as usize,
            "an omitting host keeps the server half's tls op's reserved trap"
        );
    }

    /// An unserved op's reserved trap body answers the UnservedWorldOp
    /// trap with its own index: the defense under install-time validation.
    #[test]
    fn an_unserved_op_traps_with_its_index() {
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
        let console = Platform::new("test", &[Capability::Console]);
        let table = platform_module(&console);
        assert_eq!(
            (table[OP_READ_FILE as usize].func)(&mut heaps, &mut OutputSink::captured(), &[]),
            Err(Trap::UnservedWorldOp { op: OP_READ_FILE })
        );
    }

    /// The capability vocabulary names round-trip.
    #[test]
    fn capability_names_round_trip() {
        for cap in [
            Capability::Console,
            Capability::Fsys,
            Capability::Net,
            Capability::Time,
            Capability::Tls,
            Capability::Proc,
        ] {
            assert_eq!(Capability::from_name(cap.name()), Some(cap));
        }
        assert_eq!(Capability::from_name("telepathy"), None);
    }
}
