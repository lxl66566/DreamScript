//! The DreamScript stdlib ambient: the checked-declaration layer that
//! makes corpus sources check clean as-is (M3). The runtime half of the
//! stdlib is the ds-vm `builtin` native core (pure primitives, in every
//! VM) plus the world surface served through the platform handler the
//! host installs behind the platform declaration (H5.5, E1) — with the
//! ds-source `@std` package layer beside it (the library-boundary
//! un-merge: the members that migrated out of the ambient live in
//! [`STD_MEMBERS_SOURCE`], the merged [`AMBIENT_SOURCE`] composing them
//! back for every registration that does not declare the dep); this
//! crate is the static half — the vocabulary a `ds check` package
//! receives before the user's sources, so a corpus file needs no
//! hand-written ambient package member.
//!
//! # Shape
//!
//! The ambient is data, three halves:
//!
//! * [`ambient_blob`]: the precompiled expanded tree of the source member (the derive expansion
//!   runs at artifact build time), behind the `ast-serde` feature — a checked-in postcard artifact
//!   the cold-start seams decode in place of parsing the member (a pure cache: every seam falls
//!   back to the fresh parse on any artifact miss).
//!
//! * [`AMBIENT_SOURCE`]: one package member's text. The `ds check` driver registers it as a file
//!   before the user's files (`--no-ambient` turns it off), and the standard pipeline declares it —
//!   parse, resolve, the declaration tables, the bidirectional check. A file that redeclares an
//!   ambient name in the same namespace is a duplicate-definition error (G3, F9), so self-contained
//!   packages check with `--no-ambient`.
//! * [`declared_rows`]: the [`ds_check::DeclaredRows`] table of the ambient functions — the
//!   effect-row surface v1 has no syntax for (E1). The rows enter through the ds-query database's
//!   ambient-rows seam and are a lower bound on the computed row.
//! * [`world_caps`]: the world functions' capability attribution, read beside the rows — the
//!   ambient's `platform "std" provides <...>` declaration (H5) licenses per capability.
//! * [`AMBIENT_SOURCE`] carries the platform declaration itself: the ambient names the default
//!   platform `std` with every capability, so a package including it is checked against the
//!   platform the `ds` CLI provides.
//!
//! # Scope
//!
//! The surface is the corpus's assumed stdlib (conformance/README.md,
//! "Ambient names"), the M3 part plus the comptime reflection vocabulary
//! (the `field` record struct and the `fields`/`type_name` builtins the
//! evaluator's fenced VM serves). Deliberately out of scope: the `os`
//! build target and the `c` FFI namespace with `extern fn` C
//! declarations (M7). A use of an out-of-scope name is the corpus's own
//! staged noise, never an ambient error.
//!
//! # Two-namespace consequences
//!
//! The package namespace is two (G3, F9) — types and values — so the
//! ambient's `struct map[k, v]` and the list-transform function
//! `fn map[t, r]` coexist: brace and type positions resolve the struct,
//! the dot position resolves the function. Consequences that remain:
//!
//! * The file face is ambient-owned (the package-owned-types round): the `file_handle` struct
//!   (named apart from the corpus files' own `file` declarations) with the `open_file`/
//!   `read_raw`/`close_raw` externs and the ds-source `read_exact`/`close_file` handlers over it,
//!   its deinit closing the handle; `read_exact` answers `bytes | io_err` (the `bytes` collection's
//!   runtime kind: one immutable byte string behind one Ref word, constructed through `bytes_of`
//!   over `list[byte]`, read through `len`/`byte_at`/`byte_list`), while `open_file` keeps the
//!   total spelling because a deinit-carrying union member refuses. The net face rides the same
//!   ownership: the `net_handle` struct with `net_connect` answering it, the ds-source
//!   `send`/`recv`/`close_conn` handlers reading its id word out to the raw `net_send`/`net_recv`/
//!   `net_close` externs, the deinit closing the connection, and `net_close` total and idempotent
//!   for the deinit pairing. The json face (`json`, `parse_json`, `to_json`, the typed readers) is
//!   the @std package's own source (json.ds) and the string face (the pure string operations over
//!   the core `concat`/`str` pair) its own (strings.ds), both composed back after the core for
//!   every composing reading and into the @std bundle's core member. The failure taxonomy, by
//!   contrast, is ambient-owned (`io_err`, `net_err`, `parse_err` — the discriminants the platform
//!   natives answer), so the fallible reads (`read_file`, `parse_int`, the file face's reads)
//!   return real success unions.
//! * `max`/`min` migrated to the @std bodied members' file as the int pair (the library-boundary
//!   round): the word-shaped signature a linked call binds and a session composes back, the float
//!   and ord-generic spellings riding the later linked-surface rounds. Likewise `u32` is a distinct
//!   atom (B7) (mixed-width arithmetic is the corpus's staged noise).
//! * Every extern names a callee the lowering answers; the dead declarations (declared externs
//!   whose callee no native or rule answered — `get`, `log`, `close`, `to_u32_le`, `read_config`,
//!   `default_config`, `load_rows`, `fetch`, `consume`, `die`) are gone, so their uses are unknown
//!   names at the checker.

#![warn(missing_docs)]

use std::sync::LazyLock;

use ds_check::{DeclaredRows, Effect, EffectRow, WorldCaps};
use ds_mod::Hash128;

#[cfg(feature = "ast-serde")]
pub mod ambient_blob;
#[cfg(feature = "ast-serde")]
pub mod ambient_dsi;
#[cfg(feature = "ast-serde")]
pub use ambient_blob::{AmbientBlobError, ambient_ast};
#[cfg(feature = "ast-serde")]
pub use ambient_dsi::{ambient_dsi, ambient_effect_names, ambient_linearity};
#[cfg(feature = "ast-serde")]
pub use ds_dsi::{PrefixDsiError, ServedPrefix};

/// The world-capability attribution, built once per process.
static WORLD_CAPS: LazyLock<WorldCaps> = LazyLock::new(|| {
    WorldCaps::new()
        .with("println", "console")
        .with("print", "console")
        .with("read_file", "fsys")
        .with("write_file", "fsys")
        .with("file_exists", "fsys")
        .with("fs_list", "fsys")
        .with("file_size", "fsys")
        .with("mkdir", "fsys")
        .with("remove", "fsys")
        .with("fs_size", "fsys")
        .with("net_connect", "net")
        .with("net_send", "net")
        .with("net_recv", "net")
        .with("net_close", "net")
        .with("http_get", "net")
        .with("open_file", "fsys")
        .with("read_raw", "fsys")
        .with("close_raw", "fsys")
        .with("time", "time")
        .with("mono", "time")
        .with("sleep", "time")
        .with("utc_offset", "time")
        .with("eprintln", "console")
        .with("eprint", "console")
        .with("read_line", "console")
        .with("args", "proc")
        .with("env", "proc")
        .with("proc_run", "proc")
        .with("exit", "proc")
});

/// The ambient functions' world-capability attribution (E1, H5): the
/// platform surface each world-touching function belongs to, read by the
/// effects pass beside [`declared_rows`] — the platform declaration's
/// provides list licenses per capability, so `fsys` usage under a
/// console-only platform is a check error. Every world-row name absent
/// here is an unattributed world touch, which licenses on any
/// world-bearing platform.
///
/// * `console`: the printing pair, the stderr twins, and the stdin read.
/// * `fsys`: the file readers, writers, the path round (`mkdir`, `remove`, `fs_size`), and the
///   handle face (`open_file`, `read_raw`, `close_raw`).
/// * `net`: the TCP surface, its linear handle face (`net_connect` answering `net_handle`, the
///   ds-source `send`/`recv`/`close_conn` handlers over it), the server half (`net_listen`
///   answering the `net_listener` handle with the ds-source `accept`/`close_listener` handlers over
///   it), and `http_get`.
/// * `time`: the clock pair (`time` the wall clock, `mono` the monotonic read), the parking `sleep`
///   (the 2026-11-21 timer ruling), and the local UTC offset read the datetime face composes over.
/// * `proc`: the CLI process surface — string argv, the environment read, process spawn (the piped
///   capture half answering the linear `proc_handle` over the idempotent `proc_close` seam), and
///   the deliberate-status `exit(code)` row.
///
/// The table is built on first read and shared: a consumer that must own
/// one clones it.
#[must_use]
pub fn world_caps() -> &'static WorldCaps {
    &WORLD_CAPS
}

/// The ambient package member's file name, for diagnostics that address it.
pub const AMBIENT_FILE: &str = "std/ambient.ds";

/// The shrunk ambient core's text: the compiler-required face plus every
/// member the library-boundary un-merge has not migrated yet. A package
/// compile whose graph resolved the reserved `@std` package registers
/// this text as its ambient, so the migrated members bind through the
/// linked surface alone; an entry session composes the
/// [`BODIED_SOURCE`] members back ([`AMBIENT_SESSION_SOURCE`] — the
/// run path, `ds check`, and the LSP alike), and every undeclaring
/// package registration serves the merged [`AMBIENT_SOURCE`].
pub const AMBIENT_CORE_SOURCE: &str = include_str!("ambient.ds");

/// The migrated `@std` members' file name inside the default package:
/// the demo residue (`row`, `config`, `response`, `urls`), the un-merge's
/// first migrants.
pub const STD_MEMBERS_FILE: &str = "std/std_members.ds";

/// The migrated `@std` members' text: the data-shaped declarations that
/// left the ambient core and live in the reserved package's own ds
/// source, bindable through the linked surface alone (no session
/// composes them back — ambient data values do not lower).
pub const STD_MEMBERS_SOURCE: &str = include_str!("std_members.ds");

/// The `@std` bodied members' file name inside the default package: the
/// migrated declarations whose bodies are plain ds source over
/// word-shaped atoms — the ordering pair (`max`/`min`) the
/// library-boundary round landed, the list pair (`enumerate`/`zip`) the
/// FP surface round, the pooled pair (`par_map`/`par_reduce`) the
/// fork-join migration over its private engine seams, and the seeded
/// PRNG family (`rand`/`rand_next`/`rand_seed`, splitmix64) the
/// scarcity audit's first std gap — the later bodied migrations' home.
pub const BODIED_FILE: &str = "std/bodied.ds";

/// The `@std` bodied members' text: the one shape a linked call surface
/// binds and the one a session composes back beside the shrunk core
/// (the bodies lower as ambient bodies, keeping the bare spellings
/// resolving and running for every entry script session).
pub const BODIED_SOURCE: &str = include_str!("bodied.ds");

/// The `@std` json face's file name: the typed value family over the
/// JSON grammar with its parser, serializer, and the typed readers and
/// writers the json derive walks consume. The face rides composed into
/// the @std bundle's core member (the collection seams it calls are the
/// core's own private externs at their pinned positions, so the face's
/// free seam calls must stay same-file with them), not as the bundle's
/// own member file.
pub const JSON_FILE: &str = "std/json.ds";

/// The `@std` json face's text: the ds source that lived at the ambient
/// core's tail, now the reserved package's own source. Both composing
/// readings fold it back (the merged source and the entry script
/// session's — the bare spellings and the derive walks stay
/// load-bearing), so the migration is invisible to every undeclaring
/// registration while a declaring graph binds the face through the
/// linked surface alone.
pub const JSON_SOURCE: &str = include_str!("json.ds");

/// The `@std` string face's file name: the pure string operations (the
/// separator pair, the case pair, the checked slice and char read,
/// the search family, the parses) that left the ambient core as the
/// reserved package's own source. Like the json face it rides composed
/// into the @std bundle's core member — the json face's free calls
/// into it (`char_at`, `substring`, the parses) stay same-file there
/// — not as the bundle's own member file.
pub const STRINGS_FILE: &str = "std/strings.ds";

/// The `@std` string face's text: each declaration names a builtin
/// core native the lowering routes by name, so the package owns the
/// spelling while the engine owns the seam. Both composing readings
/// fold it back beside the core (the merged source and the entry
/// script session's — the bare and UFCS spellings stay load-bearing),
/// while a declaring graph binds the face through the linked surface
/// alone. The declarations mint no atoms and no tags, so their
/// relocation to the composed tail leaves the ambient's atom mint
/// order and tag ids untouched.
pub const STRINGS_SOURCE: &str = include_str!("strings.ds");

/// The @std serialization driver face's file name: the two traits every
/// format driver implements over its document type, the derive
/// synthesis's closed target. Rides as the bundle's own member and
/// composes last in both ambient readings (a tail append — the mint
/// order of existing declarations stays untouched).
pub const CODEC_FILE: &str = "std/codec.ds";

/// The @std serialization driver face's text: the `encoder`/`decoder`
/// driver traits, json the first implementor (its impls in the json
/// member's own source).
pub const CODEC_SOURCE: &str = include_str!("codec.ds");

/// The @std datetime face's file name: the calendar over unix seconds
/// — the pure civil-from-days walk over the proleptic Gregorian rules,
/// the local composition over the ambient's `utc_offset` op, and the
/// fixed-shape ISO spelling. Rides as the bundle's own member and
/// composes last in both ambient readings (a tail append — the mint
/// order of existing declarations stays untouched).
pub const DATETIME_FILE: &str = "std/datetime.ds";

/// The @std datetime face's text: the one shape both the linked
/// surface binds and the session composes back, its only world touch
/// the ambient-declared `utc_offset` extern.
pub const DATETIME_SOURCE: &str = include_str!("datetime.ds");

/// The @std error-box face's file name: the application boundary's
/// open error box — one tag over the boxed payload and a message, the
/// explicit conversion the boundary enters. Rides as the bundle's own
/// member and composes last in both ambient readings (a tail append —
/// the mint order of existing declarations stays untouched).
pub const ERROR_FILE: &str = "std/error.ds";

/// The @std error-box face's text: the `any_err` payload set over the
/// failure taxonomy's aliases, the `box_err` tag, and the
/// `box_err.of` conversion on the box type's own static tier.
pub const ERROR_SOURCE: &str = include_str!("error.ds");

/// The @std bundle's core member: the shrunk core with the json face
/// and the string face composed after it, the bundle's own
/// `std/ambient.ds` — the package's self-contained first member (its
/// own compile registers no ambient, so every name the faces resolve
/// must live in the package, and their free seam calls — the json
/// face's into the core's private collection externs, its string-face
/// calls into the string face — must stay same-file).
pub const BUNDLE_CORE_SOURCE: &str = concat!(
    include_str!("ambient.ds"),
    "\n",
    include_str!("json.ds"),
    "\n",
    include_str!("strings.ds")
);

/// The session composition's ambient text: the shrunk core with the
/// @std json and string faces and the bodied members composed after
/// it — the implicit resolution every entry-session registration
/// serves (`ds run`/`ds build` over the implicit `@std` graph, the
/// `ds check` driver, and the LSP session), holding the bare
/// load-bearing migrated names alive (the json spellings, the string
/// spellings, and the derive walks included) while the data-shaped
/// members stay bound through the linked surface. The merged
/// [`AMBIENT_SOURCE`] serves every undeclaring package registration.
pub const AMBIENT_SESSION_SOURCE: &str = concat!(
    include_str!("ambient.ds"),
    "\n",
    include_str!("json.ds"),
    "\n",
    include_str!("strings.ds"),
    "\n",
    include_str!("bodied.ds"),
    "\n",
    include_str!("codec.ds"),
    "\n",
    include_str!("datetime.ds"),
    "\n",
    include_str!("error.ds")
);

/// The ambient package member's text: the declarations of the M3
/// stdlib surface. The merged reading — the core composed with the
/// migrated @std json and string faces, the members, and the bodied
/// members after it, so the sources are one namespace and every
/// undeclaring registration keeps the exact pre-un-merge surface.
/// Register it as a package member before the user's files.
pub const AMBIENT_SOURCE: &str = concat!(
    include_str!("ambient.ds"),
    "\n",
    include_str!("json.ds"),
    "\n",
    include_str!("strings.ds"),
    "\n",
    include_str!("std_members.ds"),
    "\n",
    include_str!("bodied.ds"),
    "\n",
    include_str!("codec.ds"),
    "\n",
    include_str!("datetime.ds"),
    "\n",
    include_str!("error.ds")
);

/// The reserved `@std` package's name in the package namespace: the
/// ds-source package the toolchain installs beside the ambient, never a
/// user package's name (a manifest under it refuses, and the default
/// install answers the name's resolution demands before any store or
/// registry row can).
pub const STD_PACKAGE_NAME: &str = "@std";

/// The default `@std` package's bundle: the manifest (`@std` at the
/// toolchain's own version, dependency-free) plus the bundle's core
/// member (the shrunk ambient with the json and string faces composed
/// into it), the codec face, the bodied members' file, and the
/// migrated members' own source — the core beside the members so the
/// package's own compile (which registers no ambient ahead of it)
/// still resolves every name, the members' files the migration home
/// the un-merge grows. A pure function of the tree: two calls
/// build byte-identical bundles, so the store's content addressing
/// holds one row per toolchain.
#[must_use]
pub fn std_package() -> ds_pkg::Package {
    let version = ds_pkg::Version::parse(env!("CARGO_PKG_VERSION"))
        .expect("the toolchain version parses as a package version");
    let manifest = ds_pkg::Manifest {
        name: STD_PACKAGE_NAME.to_owned(),
        version,
        deps: std::collections::BTreeMap::new(),
        artifacts: std::collections::BTreeMap::new(),
    };
    ds_pkg::Package::new(&manifest, [
        (
            AMBIENT_FILE.to_owned(),
            BUNDLE_CORE_SOURCE.as_bytes().to_vec(),
        ),
        (CODEC_FILE.to_owned(), CODEC_SOURCE.as_bytes().to_vec()),
        (
            DATETIME_FILE.to_owned(),
            DATETIME_SOURCE.as_bytes().to_vec(),
        ),
        (ERROR_FILE.to_owned(), ERROR_SOURCE.as_bytes().to_vec()),
        (BODIED_FILE.to_owned(), BODIED_SOURCE.as_bytes().to_vec()),
        (
            STD_MEMBERS_FILE.to_owned(),
            STD_MEMBERS_SOURCE.as_bytes().to_vec(),
        ),
    ])
    .expect("the default @std bundle frames")
}

/// The default @std bundle's member identity — the dep hash a dep list
/// records for the reserved package, answered through the one fold the
/// load walk itself uses, over [`std_package`]'s own members so a new
/// bundle member re-keys this pin in the same edit that adds it. A test
/// seam, not library API.
#[doc(hidden)]
#[must_use]
pub fn std_member_identity() -> Hash128 {
    std_package()
        .member_identity()
        .expect("the default bundle's members fold")
}

/// The checked-in ambient dsmod artifact: a 16-byte build-identity
/// header followed by the module bytes the `gen-ambient-dsmod` example
/// writes (the ambient member lowered alone, the `lower_ambient`
/// discipline). Raw bytes only — no feature, no dependency — the
/// serving seam (the CLI run path) validates and decodes them.
pub static AMBIENT_DSMOD: &[u8] = include_bytes!("ambient.dsmod");

/// The declared rows, built once per process.
static DECLARED_ROWS: LazyLock<DeclaredRows> = LazyLock::new(|| {
    let world = EffectRow::of(Effect::World);
    let error = EffectRow::of(Effect::Error);
    DeclaredRows::new()
        .with("read_file", error | world)
        .with("write_file", error | world)
        .with("file_exists", world)
        .with("fs_list", error | world)
        .with("file_size", error | world)
        .with("mkdir", error | world)
        .with("remove", error | world)
        .with("fs_size", error | world)
        .with("net_connect", error | world)
        .with("net_send", error | world)
        .with("net_recv", error | world)
        .with("net_close", world)
        .with("net_listen", error | world)
        .with("net_accept", error | world)
        .with("net_listener_close", world)
        .with("net_local_addr", world)
        .with("net_accept_tls", error | world)
        .with("proc_spawn", error | world)
        .with("proc_write", error | world)
        .with("proc_read", error | world)
        .with("proc_wait", error | world)
        .with("proc_close", world)
        .with("proc_close_in", world)
        .with("proc_read_err", error | world)
        .with("proc_try_wait", world)
        .with("http_get", error | world)
        .with("open_file", error | world)
        .with("read_raw", error | world)
        .with("close_raw", world)
        .with("time", world)
        .with("utc_offset", world)
        .with("parse_json", error)
        .with("println", world)
        .with("print", world)
        .with("eprintln", world)
        .with("eprint", world)
        .with("read_line", world)
        .with("args", world)
        .with("env", world)
        .with("proc_run", error | world)
        .with("exit", EffectRow::of(Effect::Trap) | world)
        .with("parse_int", error)
        .with("cancel", EffectRow::of(Effect::Cancel))
        // the trap primitive's private spelling (the u32 conversion's
        // seam) and the policy-free abort over it: the rows ride the
        // declared table the same way the host provision's die row rides
        // the session rows
        .with("trap", EffectRow::of(Effect::Trap))
        .with("panic", EffectRow::of(Effect::Trap))
});

/// The ambient functions' declared effect rows (E1) — the failure and
/// world capabilities the v1 surface has no syntax for. Applied by name;
/// a declared row is a lower bound on the computed row.
///
/// * `error`: failure handed to the caller — the parse and IO readers.
/// * `world`: the world-touching capability surface — IO, the file and net handle faces (the reads
///   and connects fallible, the idempotent close seams total — `close_raw`, `net_close`,
///   `net_listener_close`, `proc_close`, and `proc_close_in`), the clock (the pair, the parking
///   sleep, and the datetime face's local offset read), printing (stdout, stderr, stdin), and the
///   process face (argv, the environment read, process spawn, the piped capture rows).
/// * `cancel`: the nursery's internal effect (E1, E2) — `cancel`.
/// * `trap`: the trap primitive's private spelling (B7, E4) — the seam the `u32` explicit-call
///   conversion routes its out-of-range failure through, the same row the CLI host's `die`
///   provision declares for its own member and `panic` carries as the policy-free abort; `exit`
///   adds `world` beside it, the deliberate status riding the proc capability's op row.
///
/// Every other ambient function is row-pure: the combinators run their
/// lambdas inside the enclosing definition's extent (D3), which the
/// effects pass already accounts for.
///
/// The table is built on first read and shared: a consumer that must own
/// one clones it.
#[must_use]
pub fn declared_rows() -> &'static DeclaredRows {
    &DECLARED_ROWS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ambient parses clean and collects with no duplicate definition:
    /// every corpus file shares this member's namespaces. `map` rides both:
    /// the struct in the type namespace, the transform in the value
    /// namespace (F9).
    #[test]
    fn ambient_collects_into_one_clean_package() {
        let mut files = ds_diag::Files::new();
        let file = files.add_source(AMBIENT_FILE, AMBIENT_SOURCE);
        let ast = ds_parse::parse(&files, file).expect("the ambient parses");
        let diags = ds_check::Package::new().add_file(&ast);
        assert!(diags.is_empty(), "duplicate ambient names: {diags:?}");
        let (package, _, diags) = ds_check::resolve_package(&[ast]);
        assert!(diags.is_empty(), "ambient resolution failures: {diags:?}");
        for name in [
            "read_file",
            "str",
            "max",
            "total_compare",
            "str_compare",
            "char_compare",
            "byte_compare",
            "str_hash",
            "float_hash",
            "char_hash",
            "list_new",
            "list_len",
            "list_get",
            "list_concat",
            "map_new",
            "map_set",
            "map_get",
            "map_len",
            "map_keys",
            "bytes_of",
            "byte_list",
            "byte_at",
            "unwrap_or",
            "unwrap_or_else",
            "ok_or",
            "map_err",
            "open_file",
            "read_raw",
            "close_raw",
            "read_exact",
            "panic",
            "exit",
            "mkdir",
            "remove",
            "fs_size",
            "close_file",
            "net_connect",
            "send",
            "recv",
            "close_conn",
            "net_listen",
            "accept",
            "close_listener",
            "local_addr",
            "proc_spawn",
            "write",
            "read_out",
            "wait",
            "close_in",
            "read_err",
            "try_wait",
            "time",
            "parse_json",
            "enumerate",
            "zip",
            "par_map",
            "par_reduce",
            "rand",
            "rand_next",
            "rand_seed",
        ] {
            assert!(package.lookup_value(name).is_some(), "`{name}` is ambient");
        }
        for name in [
            "byte",
            "bytes",
            "list",
            "map",
            "ord",
            "hash",
            "row",
            "config",
            "io_err",
            "parse_err",
            "net_err",
            "Copyable",
            "response",
            "file_handle",
            "net_handle",
            "net_listener",
            "proc_handle",
        ] {
            assert!(package.lookup_type(name).is_some(), "`{name}` is ambient");
        }
        assert_eq!(
            package.def(package.lookup_type("map").unwrap()).kind,
            ds_check::DefKind::Struct,
            "the type namespace holds the map struct"
        );
        assert!(
            package.lookup_value("map").is_some(),
            "the value namespace holds the map transform [F9]"
        );
    }

    /// The declared rows name only ambient functions, and the rows render
    /// in the record's vocabulary.
    #[test]
    fn declared_rows_cover_the_world_surface() {
        let rows = declared_rows();
        assert_eq!(rows.row("read_file").render(), "error | world");
        assert_eq!(rows.row("write_file").render(), "error | world");
        assert_eq!(rows.row("fs_list").render(), "error | world");
        assert_eq!(rows.row("file_size").render(), "error | world");
        for name in ["mkdir", "remove", "fs_size"] {
            assert_eq!(
                rows.row(name).render(),
                "error | world",
                "the path round answers the io_err union"
            );
        }
        for name in ["net_connect", "net_send", "net_recv", "http_get"] {
            assert_eq!(rows.row(name).render(), "error | world");
        }
        assert_eq!(
            rows.row("net_close").render(),
            "world",
            "the idempotent close seam is total"
        );
        assert_eq!(rows.row("file_exists").render(), "world");
        assert_eq!(rows.row("open_file").render(), "error | world");
        assert_eq!(rows.row("read_raw").render(), "error | world");
        assert_eq!(rows.row("close_raw").render(), "world");
        assert_eq!(rows.row("time").render(), "world");
        assert_eq!(rows.row("println").render(), "world");
        assert_eq!(rows.row("eprintln").render(), "world");
        assert_eq!(rows.row("read_line").render(), "world");
        assert_eq!(rows.row("args").render(), "world");
        assert_eq!(rows.row("env").render(), "world");
        assert_eq!(rows.row("proc_run").render(), "error | world");
        assert_eq!(rows.row("cancel").render(), "cancel");
        assert_eq!(
            rows.row("trap").render(),
            "trap",
            "the trap primitive's private spelling rows trap, the die row's own"
        );
        assert_eq!(
            rows.row("panic").render(),
            "trap",
            "the abort rides the trap primitive's row"
        );
        assert_eq!(
            rows.row("exit").render(),
            "trap | world",
            "the deliberate status rides the proc capability's op row and never answers"
        );
        let caps = world_caps();
        assert_eq!(
            caps.bit("exit"),
            caps.bit("args"),
            "the deliberate status is a proc op"
        );
        for name in ["mkdir", "remove", "fs_size"] {
            assert_eq!(
                caps.bit(name),
                caps.bit("read_file"),
                "`{name}` is an fsys op"
            );
        }
        assert!(rows.row("cons").is_pure(), "the combinators are row-pure");
        assert!(
            rows.row("concat").is_pure(),
            "the string surface is row-pure"
        );
        assert!(
            rows.row("absent").is_pure(),
            "absent names carry the pure row"
        );
    }

    /// The removed dead declarations stay removed at the honest boundary:
    /// a use of any of them is an unknown name at the checker, never a
    /// callee that checks clean only to refuse at lowering.
    #[test]
    fn the_dead_externs_are_unknown_names() {
        for name in [
            "get",
            "log",
            "close",
            "to_u32_le",
            "read_config",
            "default_config",
            "load_rows",
            "fetch",
            "consume",
            "die",
        ] {
            let mut files = ds_diag::Files::new();
            let ambient = files.add_source(AMBIENT_FILE, AMBIENT_SOURCE);
            let user = files.add_source("use.ds", &format!("fn probe() -> () {{ {name} }}\n"));
            let ambient_ast = ds_parse::parse(&files, ambient).expect("the ambient parses");
            let user_ast = ds_parse::parse(&files, user).expect("the use parses");
            let (_, _, diags) = ds_check::resolve_package(&[ambient_ast, user_ast]);
            assert!(
                diags
                    .iter()
                    .any(|diag| diag.message.contains(&format!("unknown name `{name}`"))),
                "`{name}` must be an unknown name at the checker: {diags:?}"
            );
        }
    }

    /// The trap primitive's spelling stays the ambient file's own `[G4]`:
    /// a user's call resolves the private def but refuses at the file
    /// grain, and a user redeclaring the name collides like any ambient
    /// face — no bare trap spelling ever serves below the host.
    #[test]
    fn the_trap_spelling_stays_private_to_the_ambient_file() {
        let mut files = ds_diag::Files::new();
        let ambient = files.add_source(AMBIENT_FILE, AMBIENT_SOURCE);
        let user = files.add_source("use.ds", "fn probe() -> () { trap(\"boom\") }\n");
        let ambient_ast = ds_parse::parse(&files, ambient).expect("the ambient parses");
        let user_ast = ds_parse::parse(&files, user).expect("the use parses");
        let (_, _, diags) = ds_check::resolve_package(&[ambient_ast, user_ast]);
        assert!(
            diags.iter().any(|diag| diag
                .message
                .contains("`trap` is private to its defining file")),
            "the private extern refuses at the use site: {diags:?}"
        );

        let mut files = ds_diag::Files::new();
        let ambient = files.add_source(AMBIENT_FILE, AMBIENT_SOURCE);
        let user = files.add_source("use.ds", "extern fn trap[t](msg: string) -> t\n");
        let ambient_ast = ds_parse::parse(&files, ambient).expect("the ambient parses");
        let user_ast = ds_parse::parse(&files, user).expect("the use parses");
        let (_, _, diags) = ds_check::resolve_package(&[ambient_ast, user_ast]);
        assert!(
            diags
                .iter()
                .any(|diag| diag.message.contains("`trap` is already defined")),
            "the redeclaration collides with the ambient's name: {diags:?}"
        );
    }

    /// The default @std package is a pure function of the tree: the
    /// manifest names the reserved package at the toolchain's version, the
    /// members are the ambient core beside the migrated members' own
    /// source and the bodied members' file (the migration homes the
    /// un-merge grows), and two calls build byte-identical bundles.
    #[test]
    fn the_default_std_package_carries_the_core_and_the_migrated_members() {
        let package = std_package();
        let manifest = package
            .manifest(ds_diag::FileId::new(0))
            .expect("the bundle's manifest member parses");
        assert_eq!(manifest.name, STD_PACKAGE_NAME);
        assert_eq!(
            manifest.version.to_string(),
            env!("CARGO_PKG_VERSION"),
            "the default package rides the toolchain's version"
        );
        assert!(manifest.deps.is_empty() && manifest.artifacts.is_empty());
        let source_members = package
            .members()
            .iter()
            .filter(|(path, _)| path.as_str() != ds_pkg::MANIFEST_MEMBER)
            .count();
        assert_eq!(
            source_members, 6,
            "the bundle carries the composed core, the codec face, the datetime face, the \
             error-box face, the members, and the bodied file"
        );
        assert!(
            package.members().iter().any(
                |(path, bytes)| path == AMBIENT_FILE && bytes == BUNDLE_CORE_SOURCE.as_bytes()
            ),
            "the bundle's core member is the shrunk ambient with the json face composed in"
        );
        assert!(
            package
                .members()
                .iter()
                .any(|(path, bytes)| path == CODEC_FILE && bytes == CODEC_SOURCE.as_bytes()),
            "the driver face rides under its own file name"
        );
        assert!(
            package
                .members()
                .iter()
                .any(|(path, bytes)| path == STD_MEMBERS_FILE
                    && bytes == STD_MEMBERS_SOURCE.as_bytes()),
            "the migrated members ride under their own file name"
        );
        assert!(
            package
                .members()
                .iter()
                .any(|(path, bytes)| path == BODIED_FILE && bytes == BODIED_SOURCE.as_bytes()),
            "the bodied members ride under their own file name"
        );
        assert!(
            package
                .members()
                .iter()
                .any(|(path, bytes)| path == ERROR_FILE && bytes == ERROR_SOURCE.as_bytes()),
            "the error-box face rides under its own file name"
        );
        assert_eq!(
            std_package().to_bytes(),
            package.to_bytes(),
            "two calls build byte-identical bundles"
        );
    }

    /// The un-merge's split is one namespace: the shrunk core parses and
    /// resolves clean on its own (a declaring graph's ambient and the
    /// @std package's first member) with every migrated name gone, the
    /// members' and bodied files beside the core compose the @std
    /// package's own compile shape, and the bundle's core member pairs
    /// with the driver face beside them.
    #[test]
    fn the_split_composes_the_package_and_core_namespaces() {
        let mut files = ds_diag::Files::new();
        let core = files.add_source(AMBIENT_FILE, AMBIENT_CORE_SOURCE);
        let core_ast = ds_parse::parse(&files, core).expect("the core parses");
        let mut collect = ds_check::Package::new();
        let diags = collect.add_file(&core_ast);
        assert!(
            diags.is_empty(),
            "duplicate names inside the core: {diags:?}"
        );
        let members = files.add_source(STD_MEMBERS_FILE, STD_MEMBERS_SOURCE);
        let members_ast = ds_parse::parse(&files, members).expect("the migrated members parse");
        let diags = collect.add_file(&members_ast);
        assert!(
            diags.is_empty(),
            "duplicate names across the core and the members: {diags:?}"
        );
        let bodied = files.add_source(BODIED_FILE, BODIED_SOURCE);
        let bodied_ast = ds_parse::parse(&files, bodied).expect("the bodied members parse");
        let diags = collect.add_file(&bodied_ast);
        assert!(
            diags.is_empty(),
            "duplicate names across the bundle core and the bodied file: {diags:?}"
        );
        // the bundle's core member (the shrunk core with the json face
        // and the json driver's impls composed in) resolves beside the
        // driver face's own member — the impls name the traits, so the
        // two members pair — and the face's free collection-seam calls
        // stay same-file with the core's private externs
        let bundle_file = files.add_source("@bundle-core", BUNDLE_CORE_SOURCE);
        let bundle_core_ast =
            ds_parse::parse(&files, bundle_file).expect("the bundle's core member parses");
        let codec_file = files.add_source(CODEC_FILE, CODEC_SOURCE);
        let codec_ast = ds_parse::parse(&files, codec_file).expect("the driver face parses");
        let (bundle_package, _, bundle_diags) =
            ds_check::resolve_package(&[bundle_core_ast.clone(), codec_ast.clone()]);
        assert!(
            bundle_diags.is_empty(),
            "the bundle's core member resolves beside the driver face: {bundle_diags:?}"
        );
        assert!(
            bundle_package.lookup_type("json").is_some(),
            "`json` lives in the bundle's core member (the composed face)"
        );
        assert!(
            bundle_package.lookup_value("parse_json").is_some(),
            "`parse_json` lives in the bundle's core member (the composed face)"
        );
        for name in ["trim", "substring", "char_at", "parse_int"] {
            assert!(
                bundle_package.lookup_value(name).is_some(),
                "`{name}` lives in the bundle's core member (the composed string face)"
            );
        }
        let diags = collect.add_file(&codec_ast);
        assert!(
            diags.is_empty(),
            "duplicate names across the bundle core and the codec file: {diags:?}"
        );
        // the @std package's own compile shape: the composed core member
        // and the driver face ahead of the migrated files, one namespace
        let error_file = files.add_source(ERROR_FILE, ERROR_SOURCE);
        let error_ast = ds_parse::parse(&files, error_file).expect("the error-box face parses");
        let (pkg_package, _, pkg_diags) = ds_check::resolve_package(&[
            bundle_core_ast,
            codec_ast,
            members_ast,
            bodied_ast,
            error_ast,
        ]);
        assert!(
            pkg_diags.is_empty(),
            "the package shape resolves: {pkg_diags:?}"
        );
        for name in ["encoder", "decoder"] {
            assert!(
                pkg_package.lookup_type(name).is_some(),
                "`{name}` lives in the @std driver face beside the core"
            );
        }
        assert!(
            pkg_package.lookup_type("any_err").is_some(),
            "`any_err` lives in the @std error-box face beside the core"
        );
        assert!(
            pkg_package.lookup_type("box_err").is_some(),
            "`box_err` lives in the @std error-box face beside the core"
        );
        for name in ["row", "config", "response"] {
            assert!(
                pkg_package.lookup_type(name).is_some(),
                "`{name}` lives in the @std members' source beside the core"
            );
        }
        assert!(
            pkg_package.lookup_value("urls").is_some(),
            "`urls` lives in the @std members' source beside the core"
        );
        for name in ["max", "min"] {
            assert!(
                pkg_package.lookup_value(name).is_some(),
                "`{name}` lives in the @std bodied file as a bodied member"
            );
        }
        for name in ["enumerate", "zip"] {
            assert!(
                pkg_package.lookup_value(name).is_some(),
                "`{name}` lives in the @std bodied file as a combinator"
            );
        }
        for name in ["par_map", "par_reduce"] {
            assert!(
                pkg_package.lookup_value(name).is_some(),
                "`{name}` lives in the @std bodied file as the pooled pair"
            );
        }
        for name in ["rand", "rand_next", "rand_seed"] {
            assert!(
                pkg_package.lookup_value(name).is_some(),
                "`{name}` lives in the @std bodied file as the seeded PRNG family"
            );
        }
        assert!(
            pkg_package.lookup_type("json").is_some(),
            "`json` lives in the @std json face beside the core"
        );
        for name in [
            "parse_json",
            "to_json",
            "json_member",
            "json_as_int",
            "json_of_byte",
        ] {
            assert!(
                pkg_package.lookup_value(name).is_some(),
                "`{name}` lives in the @std json face beside the core"
            );
        }

        // the declaring graph's ambient shape: the core alone, the demo
        // residue's, the json face's, and the ordering pair's names gone
        let mut core_files = ds_diag::Files::new();
        let core_only = core_files.add_source(AMBIENT_FILE, AMBIENT_CORE_SOURCE);
        let core_only_ast = ds_parse::parse(&core_files, core_only)
            .expect("the core parses under its own registry");
        let (core_package, _, core_diags) = ds_check::resolve_package(&[core_only_ast]);
        assert!(
            core_diags.is_empty(),
            "the core resolves standalone: {core_diags:?}"
        );
        for name in ["row", "config", "response"] {
            assert!(
                core_package.lookup_type(name).is_none(),
                "`{name}` left the shrunk core"
            );
        }
        assert!(
            core_package.lookup_value("urls").is_none(),
            "`urls` left the shrunk core"
        );
        for name in ["max", "min"] {
            assert!(
                core_package.lookup_value(name).is_none(),
                "`{name}` left the shrunk core (the extern and the bodied pair both)"
            );
        }
        for name in ["enumerate", "zip"] {
            assert!(
                core_package.lookup_value(name).is_none(),
                "`{name}` left the shrunk core (the @std bodied file's own source)"
            );
        }
        for name in ["par_map", "par_reduce", "fork_map", "fork_reduce"] {
            assert!(
                core_package.lookup_value(name).is_none(),
                "`{name}` left the shrunk core (the pooled pair rides the @std bodied file)"
            );
        }
        for name in ["rand", "rand_next", "rand_seed"] {
            assert!(
                core_package.lookup_value(name).is_none(),
                "`{name}` left the shrunk core (the PRNG family rides the @std bodied file)"
            );
        }
        assert!(
            core_package.lookup_type("json").is_some(),
            "`json` stays pinned in the core at its declaration position (its case atoms mint              there; the compile seams address ambient mint order positionally)"
        );
        for name in [
            "parse_json",
            "to_json",
            "json_member",
            "json_as_int",
            "json_of_byte",
        ] {
            assert!(
                core_package.lookup_value(name).is_none(),
                "`{name}` left the shrunk core (the @std json face's own source)"
            );
        }
        for name in [
            "join",
            "trim",
            "parse_int",
            "parse_float",
            "split",
            "substring",
            "to_upper",
            "to_lower",
            "char_at",
            "contains",
            "index_of",
            "replace",
        ] {
            assert!(
                core_package.lookup_value(name).is_none(),
                "`{name}` left the shrunk core (the @std string face's own source)"
            );
        }
        assert!(
            core_package.lookup_type("any_err").is_none(),
            "`any_err` left the shrunk core (the @std error-box face's own source)"
        );
        assert!(
            core_package.lookup_type("box_err").is_none(),
            "`box_err` left the shrunk core (the @std error-box face's own source)"
        );
        assert!(
            core_package.lookup_value("concat").is_some(),
            "`concat` stays pinned in the core (the core string face's own seam — the bytes show \
             body and the `+`/`+=` spellings call it)"
        );
    }

    /// The two composing readings keep the load-bearing bare spellings:
    /// the entry script session's composition (the core with the json
    /// and string faces and the bodied members back) keeps the bare
    /// spellings resolving while the data-shaped members stay
    /// link-bound, and the merged composition keeps the exact
    /// pre-un-merge surface, the composition order the consts pin.
    #[test]
    fn the_session_and_merged_readings_compose_the_migrated_names() {
        // the entry script session's shape: the core with the bodied
        // members composed back, the load-bearing bare spellings
        // resolving while the data-shaped members stay link-bound
        let mut session_files = ds_diag::Files::new();
        let session = session_files.add_source(AMBIENT_FILE, AMBIENT_SESSION_SOURCE);
        let session_ast =
            ds_parse::parse(&session_files, session).expect("the session ambient parses");
        let (session_package, _, session_diags) = ds_check::resolve_package(&[session_ast]);
        assert!(
            session_diags.is_empty(),
            "the session composition resolves clean: {session_diags:?}"
        );
        for name in ["max", "min"] {
            assert!(
                session_package.lookup_value(name).is_some(),
                "the session composition keeps the bare `{name}`"
            );
        }
        for name in ["enumerate", "zip"] {
            assert!(
                session_package.lookup_value(name).is_some(),
                "the session composition keeps the bare combinator `{name}`"
            );
        }
        for name in ["par_map", "par_reduce"] {
            assert!(
                session_package.lookup_value(name).is_some(),
                "the session composition keeps the bare pooled `{name}`"
            );
        }
        for name in ["rand", "rand_next", "rand_seed"] {
            assert!(
                session_package.lookup_value(name).is_some(),
                "the session composition keeps the bare PRNG `{name}`"
            );
        }
        assert!(
            session_package.lookup_value("parse_json").is_some(),
            "the session composition keeps the bare `parse_json` (the face composed back)"
        );
        assert!(
            session_package.lookup_value("json_as_int").is_some(),
            "the session composition keeps the bare typed readers (the derive walks stay              load-bearing)"
        );
        for name in ["row", "config", "response"] {
            assert!(
                session_package.lookup_type(name).is_none(),
                "`{name}` stays link-bound in the session shape"
            );
        }
        for name in ["trim", "substring", "parse_int", "contains"] {
            assert!(
                session_package.lookup_value(name).is_some(),
                "the session composition keeps the bare string face's `{name}`"
            );
        }

        // the merged reading every undeclaring registration serves: the
        // composition carries every migrated name back, namespace-identical
        let mut merged_files = ds_diag::Files::new();
        let merged = merged_files.add_source(AMBIENT_FILE, AMBIENT_SOURCE);
        let merged_ast = ds_parse::parse(&merged_files, merged).expect("the merged ambient parses");
        let (merged_package, _, merged_diags) = ds_check::resolve_package(&[merged_ast]);
        assert!(
            merged_diags.is_empty(),
            "the merged composition resolves clean: {merged_diags:?}"
        );
        for name in ["row", "config", "response"] {
            assert!(
                merged_package.lookup_type(name).is_some(),
                "the merged reading keeps `{name}` (namespace-identical)"
            );
        }
        assert!(
            merged_package.lookup_value("urls").is_some(),
            "the merged reading keeps `urls`"
        );
        for name in ["max", "min"] {
            assert!(
                merged_package.lookup_value(name).is_some(),
                "the merged reading keeps the bodied `{name}` (namespace-identical)"
            );
        }
        for name in ["enumerate", "zip"] {
            assert!(
                merged_package.lookup_value(name).is_some(),
                "the merged reading keeps the combinator `{name}` (namespace-identical)"
            );
        }
        for name in ["par_map", "par_reduce"] {
            assert!(
                merged_package.lookup_value(name).is_some(),
                "the merged reading keeps the pooled `{name}` (namespace-identical)"
            );
        }
        for name in ["rand", "rand_next", "rand_seed"] {
            assert!(
                merged_package.lookup_value(name).is_some(),
                "the merged reading keeps the PRNG `{name}` (namespace-identical)"
            );
        }
        for name in ["utc_datetime", "local_datetime", "datetime_str"] {
            assert!(
                merged_package.lookup_value(name).is_some(),
                "the merged reading keeps the datetime `{name}` (namespace-identical)"
            );
        }
        assert!(
            merged_package.lookup_type("any_err").is_some(),
            "the merged reading keeps `any_err` (namespace-identical)"
        );
        assert!(
            merged_package.lookup_type("box_err").is_some(),
            "the merged reading keeps `box_err` (namespace-identical)"
        );
        assert!(
            merged_package.lookup_value("parse_json").is_some(),
            "the merged reading keeps `parse_json` (namespace-identical)"
        );
        assert!(
            merged_package.lookup_value("json_of_byte").is_some(),
            "the merged reading keeps the typed writers (namespace-identical)"
        );
        for name in [
            "trim",
            "to_upper",
            "split",
            "index_of",
            "replace",
            "parse_float",
        ] {
            assert!(
                merged_package.lookup_value(name).is_some(),
                "the merged reading keeps the string face's `{name}` (namespace-identical)"
            );
        }
        // the composition rules the consts pin: the session text is
        // the core, the json face, the string face, the bodied file,
        // the codec face, then the datetime face; the merged text the
        // same with the members' file before the bodied one
        assert!(
            AMBIENT_SESSION_SOURCE
                .strip_prefix(AMBIENT_CORE_SOURCE)
                .is_some_and(|rest| {
                    rest.strip_prefix('\n').is_some_and(|rest| {
                        rest.strip_prefix(JSON_SOURCE).is_some_and(|rest| {
                            rest.strip_prefix('\n').is_some_and(|rest| {
                                rest.strip_prefix(STRINGS_SOURCE).is_some_and(|rest| {
                                    rest.strip_prefix('\n').is_some_and(|rest| {
                                        rest.strip_prefix(BODIED_SOURCE).is_some_and(|rest| {
                                            rest.strip_prefix('\n').is_some_and(|rest| {
                                                rest.strip_prefix(CODEC_SOURCE).is_some_and(
                                                    |rest| {
                                                        rest.strip_prefix('\n').is_some_and(
                                                            |rest| {
                                                                rest.strip_prefix(DATETIME_SOURCE)
                                                                    .is_some_and(|rest| {
                                                                        rest.strip_prefix('\n')
                                                                            .is_some_and(
                                                                                |rest| {
                                                                                    rest
                                                                                        == ERROR_SOURCE
                                                                                },
                                                                            )
                                                                    })
                                                            },
                                                        )
                                                    },
                                                )
                                            })
                                        })
                                    })
                                })
                            })
                        })
                    })
                }),
            "the session source is the core, the json file, the string face, the bodied file, \
             then the codec face, then the error-box face, one newline between neighbors"
        );
        assert!(
            AMBIENT_SOURCE
                .strip_prefix(AMBIENT_CORE_SOURCE)
                .is_some_and(|rest| {
                    rest.strip_prefix('\n').is_some_and(|rest| {
                        rest.strip_prefix(JSON_SOURCE).is_some_and(|rest| {
                            rest.strip_prefix('\n').is_some_and(|rest| {
                                rest.strip_prefix(STRINGS_SOURCE).is_some_and(|rest| {
                                    rest.strip_prefix('\n').is_some_and(|rest| {
                                        rest.strip_prefix(STD_MEMBERS_SOURCE).is_some_and(|rest| {
                                            rest.strip_prefix('\n').is_some_and(|rest| {
                                                rest.strip_prefix(BODIED_SOURCE).is_some_and(
                                                    |rest| {
                                                        rest.strip_prefix('\n').is_some_and(
                                                            |rest| {
                                                                rest.strip_prefix(CODEC_SOURCE)
                                                                    .is_some_and(|rest| {
                                                                        rest.strip_prefix('\n')
                                                                            .is_some_and(|rest| {
                                                                                rest.strip_prefix(
                                                                                    DATETIME_SOURCE,
                                                                                )
                                                                                .is_some_and(
                                                                                    |rest| {
                                                                                        rest
                                                                                            .strip_prefix(
                                                                                                '\n',
                                                                                            )
                                                                                            .is_some_and(
                                                                                                |rest| {
                                                                                                    rest
                                                                                                        == ERROR_SOURCE
                                                                                                },
                                                                                            )
                                                                                    },
                                                                                )
                                                                            })
                                                                    })
                                                            },
                                                        )
                                                    },
                                                )
                                            })
                                        })
                                    })
                                })
                            })
                        })
                    })
                }),
            "the merged source is the core, the json file, the string face, the members' file, \
             the bodied file, then the codec face, then the error-box face, one newline between \
             neighbors"
        );
    }
}
