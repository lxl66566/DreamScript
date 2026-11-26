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

/// Analyzes the ambient plus one test member whose import binds a
/// hand-built linked surface (G4) — one entry per (name, parameter
/// names, solved row) fixture — and returns the pooled refusal's
/// diagnostics: a linked callee's carried row is the only fact the hunt
/// holds.
fn pooled_linked_of(
    source: &str,
    fixture: &[(&str, &[&str], ds_check::EffectRow)],
) -> Vec<ds_diag::Diagnostic> {
    let mut files = Files::new();
    let ambient_file = files.add_source(ds_stdlib::AMBIENT_FILE, ds_stdlib::AMBIENT_SOURCE);
    let member_file = files.add_source("pooled_linked_probe.ds", source);
    let ambient = ds_parse::parse(&files, ambient_file).expect("the ambient parses");
    let member = match ds_parse::parse(&files, member_file) {
        Ok(ast) => ast,
        Err(diags) => panic!("the probe does not parse: {diags:?}"),
    };
    let entries: Vec<ds_check::ForeignEntry> = fixture
        .iter()
        .map(|(name, params, row)| linked_fn(name, params, member_file, *row))
        .collect();
    let mut imports = ds_check::ImportGraph::new();
    for item in member.blocks.iter().flat_map(|block| block.items.iter()) {
        if let ds_parse::ItemKind::Import(import) = &item.kind {
            imports.add(
                import.span,
                ds_check::ImportSurface::Linked(ds_check::MemberSurface {
                    tags: Vec::new(),
                    impls: Vec::new(),
                    entries: entries.clone(),
                }),
            );
        }
    }
    let (package, resolutions, _) =
        ds_check::resolve_package_linked(&[&ambient, &member], &imports);
    let analyzed = type_of::analyze_resolved(
        &files,
        vec![std::sync::Arc::new(ambient), std::sync::Arc::new(member)].into(),
        package.into(),
        resolutions.into(),
        Vec::new(),
        std::sync::Arc::default(),
    );
    check_pooled_bodies(&analyzed, ds_stdlib::declared_rows())
}

/// One linked import entry: a pub fn of `name` over int parameters with
/// the given solved row, the shape the pooled judgment reads off the
/// link surface.
fn linked_fn(
    name: &str,
    params: &[&str],
    file: ds_diag::FileId,
    row: ds_check::EffectRow,
) -> ds_check::ForeignEntry {
    let at = ds_diag::Span::new(
        file,
        1,
        u32::try_from(name.len()).expect("fixture name fits a span"),
    );
    ds_check::ForeignEntry {
        name: name.to_owned(),
        vis: ds_check::Vis::Pub,
        kind: ds_check::ForeignKind::Fn,
        generic: false,
        defaulted: false,
        decl_span: at,
        sig: Some(std::sync::Arc::new(ds_check::FnSig {
            generics: Vec::new(),
            params: params
                .iter()
                .map(|param| ds_check::decls::ParamSig {
                    name: (*param).to_owned(),
                    span: at,
                    mode: ds_parse::ParamMode::Borrow,
                    ty: Some(int_type(at)),
                    default: None,
                })
                .collect(),
            ret: Some(int_type(at)),
            bind: None,
        })),
        row,
        caps: 0,
        value: None,
        ty: None,
        link: 0,
        origin: None,
        func: Some(0),
        std: false,
    }
}

/// The written `int` type at a span, a fixture signature's parameter and
/// return.
fn int_type(span: ds_diag::Span) -> ds_parse::Type {
    ds_parse::Type::Path {
        span,
        path: ds_parse::Path {
            span,
            segments: vec![ds_parse::Ident {
                span,
                name: "int".to_owned(),
            }],
        },
        args: Vec::new(),
    }
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
         mapped.first().ok((x: int) => x).err((_: none) => -1)\n}\n",
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

#[test]
fn a_pooled_call_inside_a_struct_deinit_block_refuses() {
    // A deinit block runs at the value's death site, so a pooled call
    // written there is a site the scan must reach.
    let diags = pooled_of(
        "struct file: ~Copyable {\n  fd: int\n  deinit {\n    const xs = [1, 2]\n    const ys = \
         xs.par_map((x: int) => {\n      println(\"teardown\")\n      x\n    })\n    \
         println(\"${ys.len()}\")\n  }\n}\n\nfn main() -> int {\n  const f = file { fd: 1 }\n  \
         f.fd\n}\n",
    );
    assert_eq!(codes(&diags), vec![ErrorCode::PooledWorldCall]);
}

#[test]
fn a_fn_value_passed_to_a_hosted_combinator_refuses_like_the_lambda() {
    // A named helper a hosted combinator receives as a value
    // (`[x].filter(helper)`) runs its body inside the child's extent, so
    // the plain-path spelling refuses exactly like the lambda spelling.
    let diags = pooled_of(
        "fn helper(x: int) -> bool {\n  println(\"h\")\n  true\n}\n\nfn main() -> list[int] {\n  \
         const xs = [1, 2]\n  xs.par_map((x: int) => {\n    const zs = [x].filter(helper)\n    \
         x\n  })\n}\n",
    );
    assert_eq!(codes(&diags), vec![ErrorCode::PooledWorldCall]);
}

#[test]
fn an_alias_chain_of_any_depth_still_refuses() {
    // The chase follows a local alias chain through a visited set, so no
    // depth truncates the reach (this chain runs past the old cap of
    // eight hops, which once let the world helper escape silently).
    let diags = pooled_of(concat!(
        "fn helper(x: int) -> int {\n",
        "  println(\"h\")\n",
        "  x\n",
        "}\n",
        "\n",
        "fn main() -> list[int] {\n",
        "  const a0 = helper\n",
        "  const a1 = a0\n",
        "  const a2 = a1\n",
        "  const a3 = a2\n",
        "  const a4 = a3\n",
        "  const a5 = a4\n",
        "  const a6 = a5\n",
        "  const a7 = a6\n",
        "  const a8 = a7\n",
        "  const a9 = a8\n",
        "  const a10 = a9\n",
        "  const a11 = a10\n",
        "  const xs = [1, 2]\n",
        "  xs.par_map(a11)\n",
        "}\n",
    ));
    assert_eq!(codes(&diags), vec![ErrorCode::PooledWorldCall]);
}

#[test]
fn a_linked_world_callee_inside_a_pooled_body_refuses() {
    // The linked callee's body lives in the linked module, so the row
    // its link surface carries judges it — the same fact the comptime
    // fence reads.
    let diags = pooled_linked_of(
        "import @dep (shout)\n\nfn main() -> list[int] {\n  const xs = [1, 2]\n  xs.par_map((x: \
         int) => shout(x))\n}\n",
        &[(
            "shout",
            &["n"],
            ds_check::EffectRow::of(ds_check::Effect::World),
        )],
    );
    assert_eq!(codes(&diags), vec![ErrorCode::PooledWorldCall]);
}

#[test]
fn a_linked_control_native_inside_a_pooled_body_refuses() {
    // A linked fn entry named for a control native routes to the native
    // exactly as a local extern spelling does, so the pooled body
    // refuses it by entry name.
    let diags = pooled_linked_of(
        "import @dep (fiber_resume)\n\nfn main() -> list[int] {\n  const xs = [1, 2]\n  \
         xs.par_map((x: int) => fiber_resume(x, 0))\n}\n",
        &[("fiber_resume", &["id", "v"], ds_check::EffectRow::PURE)],
    );
    assert_eq!(codes(&diags), vec![ErrorCode::PooledControlNative]);
}

#[test]
fn a_linked_clean_callee_inside_a_pooled_body_stays_legal() {
    // A pure-row linked callee pools legally: the carried row refuses
    // only the diverging members.
    let diags = pooled_linked_of(
        "import @dep (bump)\n\nfn main() -> list[int] {\n  const xs = [1, 2]\n  xs.par_map((x: \
         int) => bump(x))\n}\n",
        &[("bump", &["n"], ds_check::EffectRow::PURE)],
    );
    assert!(
        diags.is_empty(),
        "a pure linked callee pools legally: {diags:?}"
    );
}
