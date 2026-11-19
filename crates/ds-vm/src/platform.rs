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
//! capability's op answers epoch nanoseconds.

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
    /// The printing surface: `println`, `print`.
    Console,
    /// The file surface: the readers, writers, and `fs_list`.
    Fsys,
    /// The TCP surface: connect/send/recv/close, `http_get`.
    Net,
    /// The clock surface (no op exists yet).
    Time,
    /// The TLS surface (H5.5's 2026-11-17 ruling): https over rustls,
    /// selected or omitted per host — the `https_get` op and the
    /// https-routing `http_get` body.
    Tls,
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

/// The world-call ABI's width: the fixed op table's length.
pub const OP_COUNT: u32 = 17;

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
/// fsys five and net five each answer the two-word platform span.
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

/// The reserved trap bodies, index-parallel to [`OPS`].
#[rustfmt::skip]
const UNSERVED: [NativeFn; OP_COUNT as usize] = [
    unserved_op_0, unserved_op_1, unserved_op_2, unserved_op_3,
    unserved_op_4, unserved_op_5, unserved_op_6, unserved_op_7,
    unserved_op_8, unserved_op_9, unserved_op_10, unserved_op_11,
    unserved_op_12, unserved_op_13, unserved_op_14, unserved_op_15,
    unserved_op_16,
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
        assert_eq!(OPS.len(), 17);
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
        for index in [
            OP_READ_FILE,
            OP_WRITE_FILE,
            OP_FILE_EXISTS,
            OP_FS_LIST,
            OP_FILE_SIZE,
            OP_OPEN_FILE,
            OP_READ_RAW,
            OP_CLOSE_RAW,
        ] {
            assert!(!console.serves(index), "a console platform refuses fsys");
        }
        assert!(!console.serves(OP_TIME), "a console platform refuses time");
        for index in [
            OP_NET_CONNECT,
            OP_NET_SEND,
            OP_NET_RECV,
            OP_NET_CLOSE,
            OP_HTTP_GET,
            OP_HTTPS_GET,
        ] {
            assert!(!console.serves(index), "a console platform refuses net");
        }
        let table = platform_module(&console);
        assert_eq!(table.len(), 17);
        assert_eq!(table[OP_PRINTLN as usize].name, "println");
        assert_eq!(table[OP_READ_FILE as usize].result_words, 2);
    }

    /// The embed platform serves the console face alone: print/println
    /// answer and every world op beyond them stays a reserved trap at
    /// its contract index — install-time validation is the refusal the
    /// e2e pins.
    #[test]
    fn the_embed_platform_serves_the_console_face_alone() {
        let embed = Platform::embed();
        assert_eq!(embed.name(), EMBED_NAME);
        assert!(embed.serves(OP_PRINTLN), "embed serves println");
        assert!(embed.serves(OP_PRINT), "embed serves print");
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
            OP_TIME,
            OP_HTTPS_GET,
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
    /// `https_get` op as its reserved trap, while the default platform
    /// carries the routing body and the served op.
    #[test]
    fn the_tls_capability_selects_the_https_bodies() {
        let no_tls = Platform::new("embed", &[Capability::Net]);
        assert!(no_tls.serves(OP_HTTP_GET), "net serves http_get");
        assert!(!no_tls.serves(OP_HTTPS_GET), "the tls op stays unserved");
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
        let trap = plain[OP_HTTPS_GET as usize].func;
        assert_eq!(
            trap as usize, UNSERVED[OP_HTTPS_GET as usize] as usize,
            "an omitting host keeps the https op's reserved trap"
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
        ] {
            assert_eq!(Capability::from_name(cap.name()), Some(cap));
        }
        assert_eq!(Capability::from_name("telepathy"), None);
    }
}
