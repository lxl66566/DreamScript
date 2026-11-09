//! The DreamScript stdlib ambient: the checked-declaration layer that
//! makes corpus sources check clean as-is [M3]. The runtime half of the
//! stdlib already exists as the ds-vm builtin native module; this crate is
//! the static half — the vocabulary a `ds check` package receives before
//! the user's sources, so a corpus file needs no hand-written ambient
//! package member.
//!
//! # Shape
//!
//! The ambient is data, two halves:
//!
//! * [`AMBIENT_SOURCE`]: one package member's text. The `ds check` driver registers it as a file
//!   before the user's files (`--no-ambient` turns it off), and the standard pipeline declares it —
//!   parse, resolve, the declaration tables, the bidirectional check. A file that redeclares an
//!   ambient name is a duplicate-definition error [G3], so self-contained packages check with
//!   `--no-ambient`.
//! * [`declared_rows`]: the [`ds_check::DeclaredRows`] table of the ambient functions — the
//!   effect-row surface v1 has no syntax for [E1]. The rows enter through the ds-query database's
//!   ambient-rows seam and are a lower bound on the computed row; M7's platform declarations merge
//!   through the same seam.
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
//! # Single-namespace consequences
//!
//! The package namespace is single [G3]: types and values share it, one
//! meaning per name. Three consequences shape this ambient:
//!
//! * `map` is the type constructor `map[k, v]`, so the list-transform function `map` cannot
//!   coexist: `xs.map(f)` resolves the free-function tier to the struct and types vacuously (the
//!   call returns the empty type without a diagnostic). Every corpus use of the function sits in
//!   run-stage or M6 items, which the conformance scoping exempts. A surface decision (separate
//!   type/value namespaces, or a stdlib spelling for the transform) is required before `map` can
//!   mean both.
//! * Per-file types stay file-owned: `config`, `io_err`/`io_error`, `file`, `raw_fd`, `url`,
//!   `net_err`, `ord`, `json`, and the stack API (`push`/`pop`) are corpus declarations. The
//!   ambient's signatures never reference them, so world-touching functions return their success
//!   type and declare the failure set as the `error` row: `read_file` is `(string) -> string` with
//!   row `error | world`, standing for the record's `string | io_err` once packages own their error
//!   taxonomy. `open`, `read_exact`, `default_config`, and `read_config` are absent for the same
//!   reason (their parameter or return types are file-owned); their uses surface as staged run-item
//!   noise.
//! * `max` carries no bound: the `[t: ord]` constraint needs a stdlib-owned `ord` trait, and the
//!   corpus declares its own. Likewise `u32` is a distinct atom [B7] (mixed-width arithmetic is the
//!   corpus's staged noise), and `close` takes `int` because `raw_fd` is a file-owned alias of it.

#![warn(missing_docs)]

use ds_check::{DeclaredRows, Effect, EffectRow};

/// The ambient package member's file name, for diagnostics that address it.
pub const AMBIENT_FILE: &str = "std/ambient.ds";

/// The ambient package member's text: the declarations of the M3 stdlib
/// surface. Register it as a package member before the user's files.
pub const AMBIENT_SOURCE: &str = include_str!("ambient.ds");

/// The ambient functions' declared effect rows [E1] — the failure and
/// world capabilities the v1 surface has no syntax for. Applied by name;
/// a declared row is a lower bound on the computed row.
///
/// * `error`: failure handed to the caller — the parse and IO readers.
/// * `world`: the world-touching capability surface — IO and logging.
/// * `trap`: the unhandleable boundary [B7] — `die`.
/// * `cancel`: the nursery's internal effect [E1, E2] — `cancel`.
///
/// Every other ambient function is row-pure: the combinators run their
/// lambdas inside the enclosing definition's extent [D3], which the
/// effects pass already accounts for.
#[must_use]
pub fn declared_rows() -> DeclaredRows {
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
        .with("log", world)
        .with("println", world)
        .with("print", world)
        .with("load_rows", error | world)
        .with("fetch", error | world)
        .with("parse_int", error)
        .with("close", world)
        .with("cancel", EffectRow::of(Effect::Cancel))
        .with("die", EffectRow::of(Effect::Trap))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ambient parses clean and collects with no duplicate definition:
    /// every corpus file shares this member's namespace.
    #[test]
    fn ambient_collects_into_one_clean_package() {
        let mut files = ds_diag::Files::new();
        let file = files.add_source(AMBIENT_FILE, AMBIENT_SOURCE);
        let ast = ds_parse::parse(&files, file).expect("the ambient parses");
        let diags = ds_check::Package::new().add_file(&ast);
        assert!(diags.is_empty(), "duplicate ambient names: {diags:?}");
        let (package, _, diags) = ds_check::resolve_package(&[ast]);
        assert!(diags.is_empty(), "ambient resolution failures: {diags:?}");
        for name in ["read_file", "map", "list", "str", "max", "Copyable"] {
            assert!(package.lookup(name).is_some(), "`{name}` is ambient");
        }
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
        ] {
            assert_eq!(rows.row(name).render(), "error | world");
        }
        assert_eq!(rows.row("file_exists").render(), "world");
        assert_eq!(rows.row("println").render(), "world");
        assert_eq!(rows.row("die").render(), "trap");
        assert_eq!(rows.row("cancel").render(), "cancel");
        assert!(rows.row("cons").is_pure(), "the combinators are row-pure");
        assert!(
            rows.row("absent").is_pure(),
            "absent names carry the pure row"
        );
    }
}
