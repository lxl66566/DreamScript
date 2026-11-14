//! The DreamScript stdlib ambient: the checked-declaration layer that
//! makes corpus sources check clean as-is (M3). The runtime half of the
//! stdlib is the ds-vm `builtin` native core (pure primitives, in every
//! VM) plus the world surface served through the platform handler the
//! host installs behind the platform declaration (H5.5, E1) — with a
//! ds-source `@std` package layer to come; this crate is the static
//! half — the vocabulary a `ds check` package receives before the
//! user's sources, so a corpus file needs no hand-written ambient
//! package member.
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
//! "Ambient names"), the M3 part only. Deliberately out of scope: the
//! comptime builtins `fields`/`type_name` (M6), the `os` build target and
//! the `c` FFI namespace with `extern fn` C declarations (M7). A use of an
//! out-of-scope name is the corpus's own staged noise, never an ambient
//! error.
//!
//! # Two-namespace consequences
//!
//! The package namespace is two (G3, F9) — types and values — so the
//! ambient's `struct map[k, v]` and the list-transform function
//! `fn map[t, r]` coexist: brace and type positions resolve the struct,
//! the dot position resolves the function. Consequences that remain:
//!
//! * Per-file types stay file-owned: `file`, `raw_fd`, `url`, `json`, and the stack API
//!   (`push`/`pop`) are corpus declarations. `open` and `read_exact` are absent for that reason
//!   (their signatures cannot name the caller's own `file` struct); their uses surface as staged
//!   run-item noise. The failure taxonomy, by contrast, is ambient-owned (`io_err`, `net_err`,
//!   `parse_err` — the discriminants the platform natives answer), so `read_file`, `parse_int`,
//!   `load_rows`, `read_config`, and `fetch` return real success unions.
//! * `max` carries no bound even though the ambient now owns the `ord` trait: the native's one
//!   generic body plans against no instance, so the bound would add nothing to route. Likewise
//!   `u32` is a distinct atom (B7) (mixed-width arithmetic is the corpus's staged noise), and
//!   `close` takes `int` because `raw_fd` is a file-owned alias of it.

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
pub use ambient_dsi::{AmbientDsiError, ServedAmbient, ambient_dsi};

/// The world-capability attribution, built once per process.
static WORLD_CAPS: LazyLock<WorldCaps> = LazyLock::new(|| {
    WorldCaps::new()
        .with("println", "console")
        .with("print", "console")
        .with("log", "console")
        .with("read_file", "fsys")
        .with("write_file", "fsys")
        .with("file_exists", "fsys")
        .with("fs_list", "fsys")
        .with("file_size", "fsys")
        .with("close", "fsys")
        .with("load_rows", "fsys")
        .with("read_config", "fsys")
        .with("net_connect", "net")
        .with("net_send", "net")
        .with("net_recv", "net")
        .with("net_close", "net")
        .with("http_get", "net")
        .with("fetch", "net")
});

/// The ambient functions' world-capability attribution (E1, H5): the
/// platform surface each world-touching function belongs to, read by the
/// effects pass beside [`declared_rows`] — the platform declaration's
/// provides list licenses per capability, so `fsys` usage under a
/// console-only platform is a check error. Every world-row name absent
/// here is an unattributed world touch, which licenses on any
/// world-bearing platform.
///
/// * `console`: the printing pair and `log`.
/// * `fsys`: the file readers and writers, `close`, and the config/rows readers.
/// * `net`: the TCP surface, `http_get`, and `fetch`.
///
/// The table is built on first read and shared: a consumer that must own
/// one clones it.
#[must_use]
pub fn world_caps() -> &'static WorldCaps {
    &WORLD_CAPS
}

/// The ambient package member's file name, for diagnostics that address it.
pub const AMBIENT_FILE: &str = "std/ambient.ds";

/// The ambient package member's text: the declarations of the M3 stdlib
/// surface. Register it as a package member before the user's files.
pub const AMBIENT_SOURCE: &str = include_str!("ambient.ds");

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
        .with("net_close", error | world)
        .with("http_get", error | world)
        .with("parse_json", error)
        .with("read_config", error | world)
        .with("log", world)
        .with("println", world)
        .with("print", world)
        .with("load_rows", error | world)
        .with("fetch", error | world)
        .with("parse_int", error)
        .with("close", world)
        .with("cancel", EffectRow::of(Effect::Cancel))
        .with("die", EffectRow::of(Effect::Trap))
});

/// The ambient functions' declared effect rows (E1) — the failure and
/// world capabilities the v1 surface has no syntax for. Applied by name;
/// a declared row is a lower bound on the computed row.
///
/// * `error`: failure handed to the caller — the parse and IO readers.
/// * `world`: the world-touching capability surface — IO and logging.
/// * `trap`: the unhandleable boundary (B7) — `die`.
/// * `cancel`: the nursery's internal effect (E1, E2) — `cancel`.
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
            "unwrap_or",
            "unwrap_or_else",
            "ok_or",
            "map_err",
        ] {
            assert!(package.lookup_value(name).is_some(), "`{name}` is ambient");
        }
        for name in [
            "byte",
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
        for name in [
            "net_connect",
            "net_send",
            "net_recv",
            "net_close",
            "http_get",
            "read_config",
        ] {
            assert_eq!(rows.row(name).render(), "error | world");
        }
        assert_eq!(rows.row("file_exists").render(), "world");
        assert_eq!(rows.row("println").render(), "world");
        assert_eq!(rows.row("die").render(), "trap");
        assert_eq!(rows.row("cancel").render(), "cancel");
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
}
