//! The pooled-body refusal pins [E3, the 2026-11-15 per-fiber sendability
//! ruling]: the fork-join combinators' element bodies refuse the members
//! a pooled drive cannot serve on a worker — the sibling-addressing
//! fiber control natives and the world surface — unconditionally, so
//! legality never depends on the host's pool opt-in and the worker-side
//! divergences are unreachable from the source surface. Every case rides
//! the real ambient (its `par_map`/`par_reduce` declarations and effect
//! rows), exactly the bundle the CLI compiles.

use ds_check::{check_pooled_bodies, type_of};
use ds_diag::{ErrorCode, Files};

/// Analyzes the ambient plus one test member and returns the pooled
/// refusal's diagnostics.
fn pooled_of(source: &str) -> Vec<ds_diag::Diagnostic> {
    let mut files = Files::new();
    let ambient = files.add_source(ds_stdlib::AMBIENT_FILE, ds_stdlib::AMBIENT_SOURCE);
    let member = files.add_source("pooled_probe.ds", source);
    let ambient = ds_parse::parse(&files, ambient).expect("the ambient parses");
    let member = match ds_parse::parse(&files, member) {
        Ok(ast) => ast,
        Err(diags) => panic!("the probe does not parse: {diags:?}"),
    };
    let analyzed = type_of::analyze(&files, &[ambient, member]);
    check_pooled_bodies(&analyzed, ds_stdlib::declared_rows())
}

/// The refusal's codes, one per class.
fn codes(diags: &[ds_diag::Diagnostic]) -> Vec<ErrorCode> {
    diags.iter().map(|diag| diag.code).collect()
}

#[test]
fn a_clean_callable_refuses_nothing() {
    let diags = pooled_of(
        "fn main() -> list[int] {\n  const xs = [1, 2, 3]\n  const doubled = xs.par_map((x: int) \
         => x * 2)\n  doubled\n}\n",
    );
    assert!(
        diags.is_empty(),
        "a clean element body checks clean: {diags:?}"
    );
}

#[test]
fn a_named_clean_callable_refuses_nothing() {
    let diags = pooled_of(
        "fn both(x: int) -> (int, int) {\n  (x * 2, x * 3)\n}\n\nfn main() -> list[(int, int)] \
         {\n  const xs = [1, 2, 3]\n  xs.par_map(both)\n}\n",
    );
    assert!(
        diags.is_empty(),
        "a named clean callable checks clean: {diags:?}"
    );
}

#[test]
fn the_bounce_native_stays_in_contract() {
    // fiber_yield bounces identically on both drives; it is not a
    // sibling-addressing native.
    let diags =
        pooled_of(
            "extern fn fiber_yield (a: int) -> int\n\nfn main() -> list[int] {\n  const xs = [1, \
             2]\n  xs.par_map((x: int) => fiber_yield(x))\n}\n",
        );
    assert!(diags.is_empty(), "the yield bounce stays legal: {diags:?}");
}

#[test]
fn a_resume_native_inside_a_pooled_body_refuses() {
    let diags = pooled_of(
        "extern fn fiber_resume (id: int, v: int) -> int\n\nfn main() -> list[int] {\n  const xs \
         = [1, 2]\n  xs.par_map((x: int) => fiber_resume(x, 0))\n}\n",
    );
    assert_eq!(codes(&diags), vec![ErrorCode::PooledControlNative]);
}

#[test]
fn a_foreign_nursery_join_inside_a_pooled_body_refuses() {
    let diags = pooled_of(
        "extern fn nursery_join (id: int) -> int\n\nfn main() -> list[int] {\n  const xs = [1, \
         2]\n  xs.par_map((x: int) => nursery_join(x))\n}\n",
    );
    assert_eq!(codes(&diags), vec![ErrorCode::PooledControlNative]);
}

#[test]
fn a_world_call_inside_a_pooled_body_refuses() {
    let diags = pooled_of(
        "fn main() -> list[int] {\n  const xs = [1, 2]\n  xs.par_map((x: int) => {\n    \
         println(\"elem\")\n    x\n  })\n}\n",
    );
    assert_eq!(codes(&diags), vec![ErrorCode::PooledWorldCall]);
}

#[test]
fn the_fold_callable_refuses_like_the_map_callable() {
    let diags = pooled_of(
        "fn main() -> int {\n  const xs = [1, 2]\n  xs.par_reduce(0, (a: int, b: int) => {\n    \
         read_file(\"x\")\n    a + b\n  })\n}\n",
    );
    assert_eq!(codes(&diags), vec![ErrorCode::PooledWorldCall]);
}

#[test]
fn a_world_call_through_a_helper_fn_refuses() {
    // The offense sits inside the helper's body; the pooled body reaches
    // it through the call.
    let diags = pooled_of(
        "fn log(x: int) -> int {\n  println(\"elem\")\n  x\n}\n\nfn main() -> list[int] {\n  \
         const xs = [1, 2]\n  xs.par_map((x: int) => log(x))\n}\n",
    );
    assert_eq!(codes(&diags), vec![ErrorCode::PooledWorldCall]);
}

#[test]
fn a_control_native_through_a_named_callable_refuses() {
    let diags = pooled_of(
        "extern fn fiber_resume (id: int, v: int) -> int\n\nfn resumer(x: int) -> int {\n  \
         fiber_resume(x, 0)\n}\n\nfn main() -> list[int] {\n  const xs = [1, 2]\n  \
         xs.par_map(resumer)\n}\n",
    );
    assert_eq!(codes(&diags), vec![ErrorCode::PooledControlNative]);
}

#[test]
fn a_control_native_through_a_local_callable_refuses() {
    let diags = pooled_of(
        "extern fn nursery_join (id: int) -> int\n\nfn main() -> list[int] {\n  const xs = [1, \
         2]\n  const g = (x: int) => nursery_join(x)\n  xs.par_map(g)\n}\n",
    );
    assert_eq!(codes(&diags), vec![ErrorCode::PooledControlNative]);
}

#[test]
fn a_nested_combinator_lambda_inside_a_pooled_body_refuses_the_same() {
    // A hosted combinator's element lambda runs inside the pooled
    // child's extent, so its body hunts with the callable's own.
    let diags = pooled_of(
        "extern fn fiber_resume (id: int, v: int) -> int\n\nfn main() -> list[list[int]] {\n  \
         const xs = [1, 2]\n  xs.par_map((x: int) => [x].map((y: int) => fiber_resume(y, 0)))\n}\n",
    );
    assert_eq!(codes(&diags), vec![ErrorCode::PooledControlNative]);
}

#[test]
fn a_parenthesized_call_spelling_refuses_the_same() {
    let diags = pooled_of(
        "fn main() -> list[int] {\n  const xs = [1, 2]\n  (par_map(xs, (x: int) => {\n    \
         write_file(\"p\", \"x\")\n    x\n  }))\n}\n",
    );
    assert_eq!(codes(&diags), vec![ErrorCode::PooledWorldCall]);
}

#[test]
fn the_same_members_outside_pooled_bodies_stay_legal() {
    // The refusal scopes to the pooled-body seam: the identical members
    // in a plain body keep their sequential-drive legality.
    let diags = pooled_of(
        "extern fn fiber_resume (id: int, v: int) -> int\n\nfn main() -> int {\n  const xs = [1, \
         2]\n  const mapped = xs.map((x: int) => fiber_resume(x, 0))\n  println(\"done\")\n  \
         mapped.head().ok((x: int) => x).err((_: none) => -1)\n}\n",
    );
    assert!(
        diags.is_empty(),
        "outside the seam the members stay legal: {diags:?}"
    );
}

#[test]
fn a_bodied_shadow_of_a_control_native_name_stays_legal() {
    // A bodied fn of the native's name plans local and never routes to
    // the native, so it carries no divergence.
    let diags = pooled_of(
        "fn fiber_resume(id: int, v: int) -> int {\n  id + v\n}\n\nfn main() -> list[int] {\n  \
         const xs = [1, 2]\n  xs.par_map((x: int) => fiber_resume(x, 0))\n}\n",
    );
    assert!(
        diags.is_empty(),
        "a local body of the name pools legally: {diags:?}"
    );
}
