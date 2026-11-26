//! The platform handler: the world surface as an installed
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
//! The capability idiom: the `ds` CLI installs [`Platform::std`]
//! as the ambient default for scripts and [`Platform::embed`] beside it
//! under its own name, so a source declaring `platform "embed"` runs
//! against the console-only face; a library embedder constructs a bare
//! VM and installs nothing — no platform means no world surface — and
//! a test embedder installs the subset it wants to serve. The `time`
//! capability answers the wall clock, the monotonic clock, and the
//! parking sleep.

use crate::{
    native::{NativeFn, NativeFunction, NativeResult, OutputSink},
    net,
    object::Heaps,
    value::Value,
    vm::Trap,
};

/// One world capability a platform can serve.
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
    /// parking sleep, and the local UTC
    /// offset read (the datetime face's one world touch).
    Time,
    /// The TLS surface: https over rustls,
    /// selected or omitted per host — the `https_get` op, the
    /// https-routing `http_get` body, and the server half's
    /// `net_accept_tls` (the accepted connection's upgrade over the
    /// host-pinned identity).
    Tls,
    /// The process surface:
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

/// The default platform's name: the `ds` CLI's provider, and what a source that declares no
/// platform of its own addresses.
pub const STD_NAME: &str = ds_abi::native::STD_PLATFORM_NAME;

/// The embed platform's name: the minimal host-facing surface a bare deployment declares,
/// served beside the default platform.
pub const EMBED_NAME: &str = "embed";

// The world ops' reserved indices: the fixed, densely indexed run the abi's
// [`ds_abi::native`] rows carry, and the platform ABI's own width.
pub use ds_abi::native::{
    OP_ARGS, OP_CLOSE_RAW, OP_COUNT, OP_ENV, OP_EPRINT, OP_EPRINTLN, OP_EXIT, OP_FILE_EXISTS,
    OP_FILE_SIZE, OP_FS_LIST, OP_FS_SIZE, OP_HTTP_GET, OP_HTTPS_GET, OP_MKDIR, OP_MONO,
    OP_NET_ACCEPT, OP_NET_ACCEPT_TLS, OP_NET_CLOSE, OP_NET_CONNECT, OP_NET_LISTEN,
    OP_NET_LISTENER_CLOSE, OP_NET_LOCAL_ADDR, OP_NET_RECV, OP_NET_SEND, OP_OPEN_FILE, OP_PRINT,
    OP_PRINTLN, OP_PROC_CLOSE, OP_PROC_CLOSE_IN, OP_PROC_READ, OP_PROC_READ_ERR, OP_PROC_RUN,
    OP_PROC_SPAWN, OP_PROC_TRY_WAIT, OP_PROC_WAIT, OP_PROC_WRITE, OP_READ_FILE, OP_READ_LINE,
    OP_READ_RAW, OP_REMOVE, OP_SLEEP, OP_TIME, OP_UTC_OFFSET, OP_WRITE_FILE,
};

/// One op's owning capability and body; the call name, signature, and result span are the abi's
/// rows, index-parallel to this table.
struct Op {
    capability: Capability,
    func: NativeFn,
}

/// The fixed op table, densely indexed: the world surface the abi rows declare, with this crate's
/// capability ownership and body per op. The indices are contract (pool `FuncRef`s address them
/// positionally); the fallible fsys and net ops each answer the two-word platform span.
#[rustfmt::skip]
const OPS: [Op; OP_COUNT as usize] = [
    Op { capability: Capability::Console, func: crate::native::println_fn },
    Op { capability: Capability::Console, func: crate::native::print_fn },
    Op { capability: Capability::Fsys, func: crate::native::read_file_fn },
    Op { capability: Capability::Fsys, func: crate::native::write_file_fn },
    Op { capability: Capability::Fsys, func: crate::native::file_exists_fn },
    Op { capability: Capability::Fsys, func: crate::native::fs_list_fn },
    Op { capability: Capability::Fsys, func: crate::native::file_size_fn },
    Op { capability: Capability::Net, func: net::net_connect_fn },
    Op { capability: Capability::Net, func: net::net_send_fn },
    Op { capability: Capability::Net, func: net::net_recv_fn },
    Op { capability: Capability::Net, func: net::net_close_fn },
    Op { capability: Capability::Net, func: net::http_get_fn },
    Op { capability: Capability::Fsys, func: crate::fsys::open_fn },
    Op { capability: Capability::Fsys, func: crate::fsys::read_raw_fn },
    Op { capability: Capability::Fsys, func: crate::fsys::close_raw_fn },
    Op { capability: Capability::Time, func: crate::native::time_fn },
    Op { capability: Capability::Tls, func: crate::tls::https_get_fn },
    Op { capability: Capability::Console, func: crate::native::read_line_fn },
    Op { capability: Capability::Console, func: crate::native::eprintln_fn },
    Op { capability: Capability::Console, func: crate::native::eprint_fn },
    Op { capability: Capability::Proc, func: crate::proc::args_fn },
    Op { capability: Capability::Proc, func: crate::proc::env_fn },
    Op { capability: Capability::Proc, func: crate::proc::proc_run_fn },
    Op { capability: Capability::Proc, func: crate::proc::exit_fn },
    Op { capability: Capability::Time, func: crate::native::mono_fn },
    Op { capability: Capability::Time, func: crate::native::sleep_fn },
    Op { capability: Capability::Net, func: net::net_listen_fn },
    Op { capability: Capability::Net, func: net::net_accept_fn },
    Op { capability: Capability::Net, func: net::net_listener_close_fn },
    Op { capability: Capability::Proc, func: crate::proc::proc_spawn_fn },
    Op { capability: Capability::Proc, func: crate::proc::proc_write_fn },
    Op { capability: Capability::Proc, func: crate::proc::proc_read_fn },
    Op { capability: Capability::Proc, func: crate::proc::proc_wait_fn },
    Op { capability: Capability::Proc, func: crate::proc::proc_close_fn },
    Op { capability: Capability::Proc, func: crate::proc::proc_close_in_fn },
    Op { capability: Capability::Proc, func: crate::proc::proc_read_err_fn },
    Op { capability: Capability::Proc, func: crate::proc::proc_try_wait_fn },
    Op { capability: Capability::Net, func: net::net_local_addr_fn },
    Op { capability: Capability::Tls, func: crate::tls::net_accept_tls_fn },
    Op { capability: Capability::Fsys, func: crate::fsys::mkdir_fn },
    Op { capability: Capability::Fsys, func: crate::fsys::remove_fn },
    Op { capability: Capability::Fsys, func: crate::fsys::fs_size_fn },
    Op { capability: Capability::Time, func: crate::tz::utc_offset_fn },
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
    /// The default platform: `std`, every capability (the tls
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

    /// The embed platform: the minimal host-facing surface — the
    /// console face alone, so a deployment embedding the VM serves
    /// exactly print/println; every world op beyond it stays a reserved
    /// trap and install-time validation refuses its call sites as
    /// `UnprovidedPlatformOp`.
    #[must_use]
    pub fn embed() -> Self {
        Self::new(EMBED_NAME, &[Capability::Console])
    }

    /// A platform named `name` serving exactly `capabilities` — the
    /// test and embedding entrance (the capability idiom): install the
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
    ds_abi::native::rows(ds_abi::native::NativeModule::Platform)
        .zip(&OPS)
        .zip(UNSERVED)
        .map(|((row, op), unserved)| {
            let index = row.index;
            // the tls capability's host-level selection: the net face's `http_get` slot carries
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
                name: row.name,
                params: row.params,
                variadic: false,
                result_words: row.shape.result_words(),
                control: None,
                channel: None,
                timer: None,
                func,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixed table is the contract: each op's constant names its abi row, and the row's
    /// capability and result span width are the VM's own.
    #[test]
    fn the_fixed_op_table_is_contract() {
        for (index, name) in [
            (OP_PRINTLN, "println"),
            (OP_PRINT, "print"),
            (OP_READ_FILE, "read_file"),
            (OP_WRITE_FILE, "write_file"),
            (OP_FILE_EXISTS, "file_exists"),
            (OP_FS_LIST, "fs_list"),
            (OP_FILE_SIZE, "file_size"),
            (OP_NET_CONNECT, "net_connect"),
            (OP_NET_SEND, "net_send"),
            (OP_NET_RECV, "net_recv"),
            (OP_NET_CLOSE, "net_close"),
            (OP_HTTP_GET, "http_get"),
            (OP_OPEN_FILE, "open_file"),
            (OP_READ_RAW, "read_raw"),
            (OP_CLOSE_RAW, "close_raw"),
            (OP_TIME, "time"),
            (OP_HTTPS_GET, "https_get"),
            (OP_READ_LINE, "read_line"),
            (OP_EPRINTLN, "eprintln"),
            (OP_EPRINT, "eprint"),
            (OP_ARGS, "args"),
            (OP_ENV, "env"),
            (OP_PROC_RUN, "proc_run"),
            (OP_EXIT, "exit"),
            (OP_MONO, "mono"),
            (OP_SLEEP, "sleep"),
            (OP_NET_LISTEN, "net_listen"),
            (OP_NET_ACCEPT, "net_accept"),
            (OP_NET_LISTENER_CLOSE, "net_listener_close"),
            (OP_PROC_SPAWN, "proc_spawn"),
            (OP_PROC_WRITE, "proc_write"),
            (OP_PROC_READ, "proc_read"),
            (OP_PROC_WAIT, "proc_wait"),
            (OP_PROC_CLOSE, "proc_close"),
            (OP_PROC_CLOSE_IN, "proc_close_in"),
            (OP_PROC_READ_ERR, "proc_read_err"),
            (OP_PROC_TRY_WAIT, "proc_try_wait"),
            (OP_NET_LOCAL_ADDR, "net_local_addr"),
            (OP_NET_ACCEPT_TLS, "net_accept_tls"),
            (OP_MKDIR, "mkdir"),
            (OP_REMOVE, "remove"),
            (OP_FS_SIZE, "fs_size"),
            (OP_UTC_OFFSET, "utc_offset"),
        ] {
            let row =
                ds_abi::native::lookup(name).unwrap_or_else(|| panic!("`{name}` is a tabled op"));
            assert_eq!(row.index, index, "op {index} is `{name}`");
            assert_eq!(
                row.module,
                ds_abi::native::NativeModule::Platform,
                "`{name}` is a world op"
            );
        }
        // every op's owning capability is the VM's, reachable publicly
        assert_eq!(OPS[OP_PRINTLN as usize].capability, Capability::Console);
        assert_eq!(OPS[OP_TIME as usize].capability, Capability::Time);
        assert_eq!(OPS[OP_HTTPS_GET as usize].capability, Capability::Tls);
        assert_eq!(OPS[OP_ARGS as usize].capability, Capability::Proc);
        assert_eq!(OPS[OP_NET_LISTEN as usize].capability, Capability::Net);
        assert_eq!(OPS[OP_MKDIR as usize].capability, Capability::Fsys);
        assert_eq!(OPS.len(), OP_COUNT as usize);
        assert_eq!(
            ds_abi::native::rows(ds_abi::native::NativeModule::Platform).count(),
            OP_COUNT as usize,
            "the abi's world rows are the platform ABI's whole run"
        );
        for index in 0..OP_COUNT {
            assert_eq!(
                Platform::op_capability(index),
                Some(OPS[index as usize].capability),
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
        for (at, row) in ds_abi::native::rows(ds_abi::native::NativeModule::Platform).enumerate() {
            assert_eq!(
                table[at].name, row.name,
                "the built table is the abi's rows"
            );
            assert_eq!(table[at].params, row.params);
            assert_eq!(table[at].result_words, row.shape.result_words());
        }
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
