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
//!   for the deinit pairing. Still file-owned: `json` and the stack API (`push`/`pop`) — the map
//!   value channel holds one-word values, so the object case of a `json` enum cannot construct
//!   until the map round widens it, and `parse_json` answers the string channel (validate and
//!   minify). The failure taxonomy, by contrast, is ambient-owned (`io_err`, `net_err`, `parse_err`
//!   — the discriminants the platform natives answer), so the fallible reads (`read_file`,
//!   `parse_int`, the file face's reads) return real success unions.
//! * `max` carries no bound even though the ambient now owns the `ord` trait: the native's one
//!   generic body plans against no instance, so the bound would add nothing to route. Likewise
//!   `u32` is a distinct atom (B7) (mixed-width arithmetic is the corpus's staged noise).
//! * Every extern names a callee the lowering answers; the dead declarations (declared externs
//!   whose callee no native or rule answered — `get`, `log`, `close`, `to_u32_le`, `read_config`,
//!   `default_config`, `load_rows`, `fetch`, `consume`, `die`) are gone, so their uses are unknown
//!   names at the checker.

#![warn(missing_docs)]

use std::sync::LazyLock;

use ds_check::{DeclaredRows, Effect, EffectRow, WorldCaps};

#[cfg(feature = "ast-serde")]
pub mod ambient_blob;
#[cfg(feature = "ast-serde")]
pub mod ambient_dsi;
#[cfg(feature = "ast-serde")]
pub use ambient_blob::{AmbientBlobError, ambient_ast};
#[cfg(feature = "ast-serde")]
pub use ambient_dsi::{ambient_dsi, ambient_linearity};
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
        .with("net_connect", "net")
        .with("net_send", "net")
        .with("net_recv", "net")
        .with("net_close", "net")
        .with("http_get", "net")
        .with("open_file", "fsys")
        .with("read_raw", "fsys")
        .with("close_raw", "fsys")
        .with("time", "time")
});

/// The ambient functions' world-capability attribution (E1, H5): the
/// platform surface each world-touching function belongs to, read by the
/// effects pass beside [`declared_rows`] — the platform declaration's
/// provides list licenses per capability, so `fsys` usage under a
/// console-only platform is a check error. Every world-row name absent
/// here is an unattributed world touch, which licenses on any
/// world-bearing platform.
///
/// * `console`: the printing pair.
/// * `fsys`: the file readers, writers, and the handle face (`open_file`, `read_raw`, `close_raw`).
/// * `net`: the TCP surface, its linear handle face (`net_connect` answering `net_handle`, the
///   ds-source `send`/`recv`/`close_conn` handlers over it), and `http_get`.
/// * `time`: the clock op.
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
/// member the library-boundary un-merge has not migrated yet. A graph
/// that resolved the reserved `@std` package registers this text as its
/// ambient, so the migrated members bind through the linked surface
/// alone; every other registration serves the merged
/// [`AMBIENT_SOURCE`].
pub const AMBIENT_CORE_SOURCE: &str = include_str!("ambient.ds");

/// The migrated `@std` members' file name inside the default package:
/// the demo residue (`row`, `config`, `response`, `urls`), the un-merge's
/// first migrants.
pub const STD_MEMBERS_FILE: &str = "std/std_members.ds";

/// The migrated `@std` members' text: the declarations that left the
/// ambient core and live in the reserved package's own ds source.
pub const STD_MEMBERS_SOURCE: &str = include_str!("std_members.ds");

/// The ambient package member's text: the declarations of the M3 stdlib
/// surface. The merged reading — the core composed with the migrated
/// @std members after it, so the two sources are one namespace and every
/// undeclaring registration keeps the exact pre-un-merge surface. Register
/// it as a package member before the user's files.
pub const AMBIENT_SOURCE: &str = concat!(
    include_str!("ambient.ds"),
    "\n",
    include_str!("std_members.ds")
);

/// The reserved `@std` package's name in the package namespace: the
/// ds-source package the toolchain installs beside the ambient, never a
/// user package's name (a manifest under it refuses, and the default
/// install answers the name's resolution demands before any store or
/// registry row can).
pub const STD_PACKAGE_NAME: &str = "@std";

/// The default `@std` package's bundle: the manifest (`@std` at the
/// toolchain's own version, dependency-free) plus the ambient core member
/// and the migrated members' own source — the core beside the members so
/// the package's own compile (which registers no ambient ahead of it)
/// still resolves every name, the members' file the migration home the
/// un-merge grows. A pure function of the tree: two calls build
/// byte-identical bundles, so the store's content addressing holds one
/// row per toolchain.
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
            AMBIENT_CORE_SOURCE.as_bytes().to_vec(),
        ),
        (
            STD_MEMBERS_FILE.to_owned(),
            STD_MEMBERS_SOURCE.as_bytes().to_vec(),
        ),
    ])
    .expect("the default @std bundle frames")
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
        .with("net_connect", error | world)
        .with("net_send", error | world)
        .with("net_recv", error | world)
        .with("net_close", world)
        .with("http_get", error | world)
        .with("open_file", error | world)
        .with("read_raw", error | world)
        .with("close_raw", world)
        .with("time", world)
        .with("parse_json", error)
        .with("println", world)
        .with("print", world)
        .with("parse_int", error)
        .with("cancel", EffectRow::of(Effect::Cancel))
        // the trap primitive's private spelling (the u32 conversion's
        // seam): the row rides the declared table the same way the host
        // provision's die row rides the session rows
        .with("trap", EffectRow::of(Effect::Trap))
});

/// The ambient functions' declared effect rows (E1) — the failure and
/// world capabilities the v1 surface has no syntax for. Applied by name;
/// a declared row is a lower bound on the computed row.
///
/// * `error`: failure handed to the caller — the parse and IO readers.
/// * `world`: the world-touching capability surface — IO, the file and net handle faces (the reads
///   and connects fallible, the two idempotent close seams total), the clock, and printing.
/// * `cancel`: the nursery's internal effect (E1, E2) — `cancel`.
/// * `trap`: the trap primitive's private spelling (B7, E4) — the seam the `u32` explicit-call
///   conversion routes its out-of-range failure through, the same row the CLI host's `die`
///   provision declares for its own member.
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
            "close_file",
            "net_connect",
            "send",
            "recv",
            "close_conn",
            "time",
            "parse_json",
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
        assert_eq!(rows.row("cancel").render(), "cancel");
        assert_eq!(
            rows.row("trap").render(),
            "trap",
            "the trap primitive's private spelling rows trap, the die row's own"
        );
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
    /// source (the demo residue's file the migration home), and two calls
    /// build byte-identical bundles.
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
            source_members, 2,
            "the bundle carries the core and the members"
        );
        assert!(
            package.members().iter().any(
                |(path, bytes)| path == AMBIENT_FILE && bytes == AMBIENT_CORE_SOURCE.as_bytes()
            ),
            "the core member is the shrunk ambient under its own file name"
        );
        assert!(
            package
                .members()
                .iter()
                .any(|(path, bytes)| path == STD_MEMBERS_FILE
                    && bytes == STD_MEMBERS_SOURCE.as_bytes()),
            "the migrated members ride under their own file name"
        );
        assert_eq!(
            std_package().to_bytes(),
            package.to_bytes(),
            "two calls build byte-identical bundles"
        );
    }

    /// The un-merge's split is one namespace: the shrunk core parses and
    /// resolves clean on its own (a declaring graph's ambient and the
    /// @std package's first member) with the demo residue gone, the
    /// members' file beside the core composes the @std package's own
    /// compile shape, and the merged composition — the core with the
    /// members' file appended — keeps the exact pre-un-merge surface.
    #[test]
    fn the_split_composes_the_merged_namespace() {
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
        // the @std package's own compile shape: the core member ahead of
        // the migrated members' file, one namespace
        let (pkg_package, _, pkg_diags) = ds_check::resolve_package(&[core_ast, members_ast]);
        assert!(
            pkg_diags.is_empty(),
            "the package shape resolves: {pkg_diags:?}"
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

        // the declaring graph's ambient shape: the core alone, the demo
        // residue's names gone
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

        // the merged reading every undeclaring registration serves: the
        // composition carries the demo residue back, namespace-identical
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
        // the merged text is the core composed with the members' file: the
        // composition rule the consts pin
        assert!(
            AMBIENT_SOURCE
                .strip_prefix(AMBIENT_CORE_SOURCE)
                .is_some_and(|rest| {
                    rest.strip_prefix('\n')
                        .is_some_and(|rest| rest == STD_MEMBERS_SOURCE)
                }),
            "the merged source is the core, a newline, then the members' file"
        );
    }
}
