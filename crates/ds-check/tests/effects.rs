//! The `effects_of` pass [B5, B7, E1, E2, E4]: row composition through
//! calls and branches, declared ambient rows, the E4 consuming forms,
//! nursery rows, the fixpoint over call cycles, and corpus sanity — every
//! corpus file computes rows over constructs of later milestones
//! (nursery, spawn, comptime) without panicking.

use std::path::PathBuf;

use ds_check::{DeclaredRows, Effect, EffectRow, WorldCaps, effects_of};
use ds_diag::Files;
use ds_parse::parse;

/// Rows of one source under a declared ambient table.
fn rows(source: &str, declared: &DeclaredRows) -> ds_check::Effects {
    effects_of_files(&[("t.ds", source)], declared)
}

/// Rows over several named sources, in order.
fn effects_of_files(files: &[(&str, &str)], declared: &DeclaredRows) -> ds_check::Effects {
    let mut registry = Files::new();
    let mut asts = Vec::new();
    for (name, source) in files {
        let file = registry.add_source(*name, source);
        let ast = parse(&registry, file).unwrap_or_else(|diags| panic!("{name} parses: {diags:?}"));
        asts.push(ast);
    }
    effects_of(&asts, declared, &WorldCaps::new())
}

/// One definition's row, failing loudly when the definition is not
/// row-bearing.
fn row_of(effects: &ds_check::Effects, name: &str) -> EffectRow {
    effects
        .of_name(name)
        .unwrap_or_else(|| panic!("`{name}` is row-bearing"))
}

#[test]
fn row_algebra_is_structural() {
    assert_eq!(EffectRow::PURE.render(), "pure");
    let full = EffectRow::of(Effect::Error)
        | EffectRow::of(Effect::Trap)
        | EffectRow::of(Effect::World)
        | EffectRow::of(Effect::Cancel)
        | EffectRow::of(Effect::Discontinue);
    assert_eq!(full.render(), "error | trap | world | cancel | discontinue");
    assert_eq!(
        full.minus(EffectRow::of(Effect::Error)),
        EffectRow::of(Effect::Trap)
            | EffectRow::of(Effect::World)
            | EffectRow::of(Effect::Cancel)
            | EffectRow::of(Effect::Discontinue)
    );
    assert!(EffectRow::PURE.is_pure());
    assert!(!full.is_pure());
    assert_eq!(full.effects().count(), 5);
}

#[test]
fn calls_union_rows_through_the_callee_edge() {
    let effects = rows(
        concat!(
            "fn f(a: int) -> int { a + 1 }\n",
            "fn g(a: int) -> int { f(f(a)) }\n",
            "fn pure(a: int) -> int { a }\n",
        ),
        &DeclaredRows::new(),
    );
    assert_eq!(row_of(&effects, "f").render(), "trap");
    assert_eq!(row_of(&effects, "g").render(), "trap");
    assert!(row_of(&effects, "pure").is_pure());
}

#[test]
fn declared_rows_are_respected_as_lower_bounds() {
    let declared = DeclaredRows::new()
        .with(
            "fetch",
            EffectRow::of(Effect::World) | EffectRow::of(Effect::Error),
        )
        .with("mine", EffectRow::of(Effect::World));
    let effects = rows(
        concat!(
            "extern fn fetch (u: int) -> int | io_err\n",
            "extern fn quiet (u: int) -> int\n",
            "fn declared_only(u: int) -> int | io_err { fetch(u) }\n",
            "fn undeclared_only(u: int) -> int { quiet(u) }\n",
            "fn mine(x: int) -> int { x + 1 }\n",
        ),
        &declared,
    );
    // externs carry exactly their declared row.
    assert_eq!(row_of(&effects, "fetch").render(), "error | world");
    // an undeclared extern is row-pure: the ambient table is the surface
    // for extern effects until M7's platform declaration [E1].
    assert!(row_of(&effects, "undeclared_only").is_pure());
    assert_eq!(row_of(&effects, "declared_only").render(), "error | world");
    // a declared row on an in-package function is a lower bound, unioning
    // with the body's own contributions.
    assert_eq!(row_of(&effects, "mine").render(), "trap | world");
}

#[test]
fn propagation_sites_contribute_error() {
    let effects = rows(
        concat!(
            "extern fn read (u: int) -> int | io_err\n",
            "fn t(u: int) -> int { read(u)? }\n",
            "fn r(u: int) -> int { if u < 0 { return 0 }\n read(u)? }\n",
            "fn early(u: int) -> int { if u is int { read(u)? } else { 0 } }\n",
        ),
        &DeclaredRows::new(),
    );
    assert_eq!(row_of(&effects, "t").render(), "error");
    assert_eq!(row_of(&effects, "r").render(), "error");
    assert_eq!(row_of(&effects, "early").render(), "error");
}

#[test]
fn consuming_forms_mask_the_lefts_failure_members() {
    let declared = DeclaredRows::new().with(
        "read",
        EffectRow::of(Effect::World) | EffectRow::of(Effect::Error),
    );
    let effects = rows(
        concat!(
            "extern fn read (u: int) -> int | io_err\n",
            "fn handled(u: int) -> int { read(u).err((e) => 0) }\n",
            "fn chained(u: int) -> int { read(u).err((e) => 0) + 1 }\n",
        ),
        &declared,
    );
    // `.err(h)` consumes the failure set: read's `error` is consumed,
    // `world` survives [E4].
    assert_eq!(row_of(&effects, "handled").render(), "world");
    // the mask is scoped to the operand: surrounding code is unmasked.
    assert_eq!(row_of(&effects, "chained").render(), "trap | world");
}

#[test]
fn branches_union_their_arms() {
    let declared = DeclaredRows::new().with("side", EffectRow::of(Effect::World));
    let effects = rows(
        concat!(
            "extern fn side (u: int) -> int\n",
            "fn br(u: int) -> int { if u == 0 { side(u) } else { 1 } }\n",
            "fn m(u: int) -> int { match u { a => side(a), _ => 2 } }\n",
            "fn w(u: int) -> int { var i = 0\n while i < u { i += 1 }\n i }\n",
        ),
        &declared,
    );
    assert_eq!(row_of(&effects, "br").render(), "world");
    assert_eq!(row_of(&effects, "m").render(), "world");
    assert_eq!(row_of(&effects, "w").render(), "trap");
}

#[test]
fn lambda_bodies_and_holes_contribute_in_place() {
    let declared = DeclaredRows::new().with("read", EffectRow::of(Effect::World));
    let effects = rows(
        concat!(
            "extern fn read (u: int) -> int\n",
            "fn lam(xs: int) -> int { xs.map((x) => x + 1).head() }\n",
            "fn s(x: int) -> string { \"v=${read(x)}\" }\n",
        ),
        &declared,
    );
    assert_eq!(row_of(&effects, "lam").render(), "trap");
    assert_eq!(row_of(&effects, "s").render(), "world");
}

#[test]
fn nursery_carries_the_internal_effects() {
    let effects = rows(
        concat!(
            "fn n(xs: int) -> int { nursery { spawn { xs + 1 } }\n 0 }\n",
            "fn d(u: int) -> int { defer { u + 1 }\n u }\n",
        ),
        &DeclaredRows::new(),
    );
    // the nursery boundary dispatches `cancel`/`discontinue` [E1, E2];
    // spawn bodies run inside the enclosing definition's extent.
    assert_eq!(
        row_of(&effects, "n").render(),
        "trap | cancel | discontinue"
    );
    // defer runs at scope exit [E5], under cancellation too.
    assert_eq!(row_of(&effects, "d").render(), "trap");
}

#[test]
fn rows_solve_over_call_cycles() {
    let effects = rows(
        concat!(
            "fn even(n: int) -> bool { if n == 0 { true } else { odd(n - 1) } }\n",
            "fn odd(n: int) -> bool { if n == 0 { false } else { even(n - 1) } }\n",
            "fn self_loop(n: int) -> int { self_loop(n - 1) }\n",
            "extern fn read (u: int) -> int | io_err\n",
            "fn fail(u: int) -> int { read(u)? }\n",
            "fn through(u: int) -> int { fail(u)? }\n",
        ),
        &DeclaredRows::new(),
    );
    assert_eq!(row_of(&effects, "even").render(), "trap");
    assert_eq!(row_of(&effects, "odd").render(), "trap");
    assert_eq!(row_of(&effects, "self_loop").render(), "trap");
    assert_eq!(row_of(&effects, "through").render(), "error");
}

#[test]
fn trait_calls_union_every_impl_body() {
    let effects = rows(
        concat!(
            "trait shows { fn show(self) -> string }\n",
            "impl shows for int { fn show(self) -> string { str(self) } }\n",
            "impl shows for list[int] { fn show(self) -> string { \"[\" + self.len() + \"]\" } }\n",
            "fn via_dot(x: int) -> string { x.show() }\n",
            "fn via_hatch(x: int) -> string { shows::show(x) }\n",
            "impl int { fn twice(self) -> int { self + self } }\n",
            "fn use_inherent(x: int) -> int { x.twice() }\n",
        ),
        &DeclaredRows::new(),
    );
    // one impl body traps; without solved receiver types the candidate set
    // unions conservatively [F3].
    assert_eq!(row_of(&effects, "via_dot").render(), "trap");
    assert_eq!(row_of(&effects, "via_hatch").render(), "trap");
    assert_eq!(row_of(&effects, "use_inherent").render(), "trap");
}

#[test]
fn top_level_pattern_bindings_share_the_initializer_row() {
    let declared = DeclaredRows::new().with(
        "read",
        EffectRow::of(Effect::World) | EffectRow::of(Effect::Error),
    );
    let effects = rows(
        concat!(
            "extern fn read (u: int) -> int\n",
            "const (a, b) = (read(1).err((e) => 0), 2)\n",
        ),
        &declared,
    );
    assert_eq!(row_of(&effects, "a").render(), "world");
    assert!(row_of(&effects, "b").is_pure());
}

/// Every corpus file's rows, parse-standalone, no ambient table.
fn corpus(name: &str) -> ds_check::Effects {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance")
        .join(name);
    let source = std::fs::read_to_string(&path).expect("corpus file");
    effects_of_files(&[(name, &source)], &DeclaredRows::new())
}

#[test]
fn corpus_computes_rows_without_panicking() {
    for name in [
        "types_hybrid.ds",
        "generics_coherent_traits.ds",
        "linearity_modes_rc.ds",
        "effects_nursery.ds",
        "syntax_ts_heart.ds",
        "modules_hash_core.ds",
    ] {
        let effects = corpus(name);
        assert!(
            !effects.defs().is_empty(),
            "{name} has row-bearing definitions"
        );
    }
}

#[test]
fn effects_nursery_rows_match_the_record() {
    let effects = corpus("effects_nursery.ds");
    // row-bearing definitions are the four functions; the file's two
    // type declarations carry none (the net failure taxonomy is the
    // ambient's).
    assert_eq!(effects.defs().len(), 4);
    assert_eq!(effects.def_count(), 6);
    assert_eq!(effects.of_name("url"), None);
    assert_eq!(effects.of_name("page"), None);
    assert_eq!(row_of(&effects, "fetch_body").render(), "error");
    assert_eq!(
        row_of(&effects, "fetch_all").render(),
        "error | cancel | discontinue"
    );
    assert_eq!(row_of(&effects, "with_retry").render(), "error");
    let main = row_of(&effects, "main");
    assert!(main.contains(Effect::Trap));
    assert!(main.contains(Effect::Cancel));
    // the `.err` handler consumed fetch_all's failure members [E4].
    assert!(!main.contains(Effect::Error));
}

#[test]
fn linearity_rows_match_the_record() {
    let effects = corpus("linearity_modes_rc.ds");
    assert_eq!(row_of(&effects, "read_u32").render(), "error");
    assert_eq!(row_of(&effects, "sum_file").render(), "error | trap");
    // the `.err` handler consumed sum_file's failure members; the `+`
    // remains.
    assert_eq!(row_of(&effects, "total").render(), "trap");
    assert!(row_of(&effects, "words").is_pure());
    assert!(row_of(&effects, "lens").is_pure());
}

#[test]
fn types_hybrid_rows_match_the_record() {
    let effects = corpus("types_hybrid.ds");
    // bare `return` is no propagation site: only `?` contributes `error`,
    // and get_port calls nothing declared [E4].
    assert_eq!(row_of(&effects, "get_port").render(), "trap");
    assert_eq!(row_of(&effects, "load").render(), "error | trap");
    // the `.err` handler consumed load's failure members; its `trap`
    // survives [E4].
    let cfg = row_of(&effects, "cfg");
    assert!(cfg.contains(Effect::Trap));
    assert!(!cfg.contains(Effect::Error));
    assert_eq!(row_of(&effects, "same").render(), "trap");
    assert!(row_of(&effects, "kv").is_pure());
}

#[test]
fn generics_rows_match_the_record() {
    let effects = corpus("generics_coherent_traits.ds");
    assert!(row_of(&effects, "push").is_pure());
    assert!(row_of(&effects, "pop").is_pure());
    // p.to_json() unions every impl body; the list impl's `+` traps.
    assert_eq!(row_of(&effects, "js").render(), "trap");
    assert_eq!(row_of(&effects, "schema").render(), "trap");
}

#[test]
fn ambient_declared_rows_flow_over_the_corpus() {
    let ambient = concat!(
        "extern fn read_file (path: string) -> string | io_err\n",
        "extern fn parse_json (text: string) -> json | io_err\n",
        "extern fn str[t] (v: t) -> string\n",
        "extern fn log (msg: string) -> ()\n",
        "extern fn default_config () -> config\n",
        "extern fn get[k, v] (m: map[k, v], key: k) -> v | none\n",
    );
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance/types_hybrid.ds");
    let source = std::fs::read_to_string(&path).expect("corpus file");
    let declared = DeclaredRows::new()
        .with(
            "read_file",
            EffectRow::of(Effect::World) | EffectRow::of(Effect::Error),
        )
        .with(
            "parse_json",
            EffectRow::of(Effect::World) | EffectRow::of(Effect::Error),
        )
        .with("log", EffectRow::of(Effect::World));
    let effects = effects_of_files(
        &[("ambient.ds", ambient), ("corpus.ds", &source)],
        &declared,
    );
    // the world rows flow through the ambient externs [E1]; the `.err`
    // handler still consumes only the failure members.
    assert_eq!(row_of(&effects, "load").render(), "error | trap | world");
    let cfg = row_of(&effects, "cfg");
    assert!(cfg.contains(Effect::World));
    assert!(cfg.contains(Effect::Trap));
    assert!(!cfg.contains(Effect::Error));
    assert_eq!(row_of(&effects, "same").render(), "trap | world");
}
