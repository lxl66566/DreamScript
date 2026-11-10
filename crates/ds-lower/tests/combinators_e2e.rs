//! The lambda-taking container combinators end to end [the reentrant
//! CALLG phase]: `map`, `filter`, `reduce` lower every call site to
//! CALLG addressing the native's ONE body, the lambda argument staging
//! its word into the callable-defaulted `Fn` parameter (the synthesized
//! function's scalar FuncRef, a named function's minted word, or a
//! closure object's Ref word — the hosted per-element call resolves
//! either kind), and the evidence words recording the receiver's list
//! instance behind the real arguments (`map`'s constraint list `[t, r]`
//! carries two, the `map_len` precedent). The VM reenters the dispatcher
//! per element over a synthetic reentry frame, so the block-body side
//! effects interleave in element order and map-in-lambda stacks frames
//! on the one chain.

use ds_abi::{BuiltinInstance, Inst};
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Value, Vm, loader, native};

/// The combinators' reserved slots: the FuncRef index, the real
/// arguments a site stages, the evidence words behind them, and the
/// builtin instance the evidence words name (the receiver's list).
const ROUTED: &[(u32, u8, u8, &[BuiltinInstance])] = &[
    (38, 2, 2, &[BuiltinInstance::List]), // map
    (39, 2, 1, &[BuiltinInstance::List]), // filter
    (40, 3, 1, &[BuiltinInstance::List]), // reduce
];

/// Builds the module once per test VM: parse -> check -> lower -> write
/// -> read back -> install. Both native tables must precede the loader's
/// install (pool FuncRefs validate against installed tables).
fn build_vm() -> (Vm, Lowered) {
    let source = include_str!("fixtures/combinators_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("combinators_probe.ds", source);
    let asts = [parse(&files, id).expect("the fixture parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the fixture checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "combinators_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_native(native::STD_PLATFORM_MODULE, native::std_platform_module());
    loader::install(&mut registry, &module).expect("the module installs");
    (Vm::new(registry), lowered)
}

/// One refusal probe: the source must check clean, and lowering must
/// report the named seam.
fn refused(source: &str, needle: &str) {
    let mut files = Files::new();
    let id = files.add_source("refusal_probe.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the probe checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let Err(error) = lower(
        "refusal_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    ) else {
        panic!("the probe refuses at lowering")
    };
    let text = format!("{error}");
    assert!(
        text.contains(needle),
        "the refusal names `{needle}`: {text}"
    );
}

/// The one acceptance: every combinator runs over int and string
/// elements, the free and UFCS spellings agree, the hosted block body's
/// println output interleaves in element order, the empty reduce answers
/// its init, and the nested map-in-lambda stacks reentry frames.
#[test]
fn combinators_run_reentrant_bodies_over_both_element_types() {
    let (mut vm, _) = build_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("combinators_probe", "main", &[]),
        Ok(Value::I64(0)),
        "main returns unit"
    );
    assert_eq!(
        vm.captured_output(),
        b"doubled 4 2 8\nshifted 11 14\nevens 2 2 4\nsum 10 product 24\nempty 7\n\
          visiting 5\nvisiting 6\ntraced 50 60\nbanged alpha!\nnested 1 1\npipeline 10\n"
    );
    // the run's every list died with its frame: the transferred element
    // counts, the adopted list counts, and the handed-out init all
    // balanced (the scalar return drops nothing on the way out)
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// The lowering shape is pinned: every combinator site emits exactly one
/// CALLG addressing the reserved generic slot with the site's
/// real-argument count, the evidence words (Kimm i64 instance ids)
/// staged immediately before it — and no plain CALL or CALLT reaches
/// the reserved slots.
#[test]
fn every_combinator_site_is_one_callg_with_list_evidence() {
    let (_, lowered) = build_vm();
    let mut sites = [0usize; ROUTED.len()];
    for f in lowered.module().functions() {
        let code = f.code();
        for (i, uop) in code.iter().enumerate() {
            let Inst::Uop(uop) = uop else {
                continue;
            };
            match uop {
                ds_abi::Uop::Call { callee, .. } | ds_abi::Uop::Callt { callee, .. } => {
                    if let ds_mod::Constant::FuncRef { module, func } =
                        &f.constants()[usize::from(callee.0)]
                    {
                        assert!(
                            !(module == "builtin" && ROUTED.iter().any(|r| r.0 == *func)),
                            "a plain call must not name-route the reserved slot {func}"
                        );
                    }
                },
                ds_abi::Uop::Callg { callee, nargs } => {
                    let ds_mod::Constant::FuncRef { module, func } =
                        &f.constants()[usize::from(callee.0)]
                    else {
                        panic!("the Callg callee must be a pool FuncRef");
                    };
                    assert_eq!(module, "builtin", "the generic bodies live in the core");
                    // the fixture's reads (`len`, `list_get`) route
                    // through their own generic slots, pinned by the
                    // container probes — only the combinators count here
                    let Some(slot) = ROUTED.iter().position(|r| r.0 == *func) else {
                        assert!(
                            native::resolve_generic("builtin", *func).is_some(),
                            "the Callg addresses a known generic body (slot {func})"
                        );
                        continue;
                    };
                    let (_, real_args, evidence, instances) = ROUTED[slot];
                    assert_eq!(*nargs, real_args, "nargs names the site's real arguments");
                    for back in 1..=usize::from(evidence) {
                        let Some(Inst::Uop(ds_abi::Uop::Kimm { kidx, .. })) =
                            i.checked_sub(back).and_then(|p| code.get(p))
                        else {
                            panic!("no evidence word staged before the Callg");
                        };
                        match &f.constants()[usize::from(kidx.0)] {
                            ds_mod::Constant::I64(id)
                                if instances.iter().any(|i| *i as i64 == *id) => {},
                            other => {
                                panic!("the staged evidence names a routed instance, got {other:?}")
                            },
                        }
                    }
                    sites[slot] += 1;
                },
                _ => {},
            }
        }
    }
    assert_eq!(
        sites,
        [7, 2, 4],
        "one Callg per use: map seven (one UFCS, one untyped-param, one block body, one string \
         run, two nested, one pipelined), filter two, reduce four"
    );
}

/// The combinators' retain contract balances across iterations (C4): a
/// loop driving map's adopted element counts, filter's borrowed
/// elements, and reduce's owned-accumulator replacement — over `Ref`
/// elements, the counted path — must keep every claim balanced, so the
/// scalar-returning program drains the heap.
#[test]
fn combinator_counts_stay_balanced_across_iterations() {
    const SOURCE: &str = "
tag int
tag string
tag bool

struct list[t]

extern fn println (s: string) -> ()
extern fn concat (a: string, b: string) -> string
extern fn map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]
extern fn filter[t] (xs: list[t], f: fn(t) -> bool) -> list[t]
extern fn reduce[t] (xs: list[t], init: t, f: fn(t, t) -> t) -> t
extern fn len[t] (xs: t) -> int

fn main() -> int {
  var words = [\"alpha\", \"beta\", \"gamma\"]
  var i = 0
  var seen = 0
  while i < 25 {
    const shouted = map(words, (w: string) => concat(w, \"!\"))
    const same = map(words, (w: string) => w)
    const short = filter(words, (w: string) => len(w) < 5)
    const joined = reduce(shouted, \"\", (a: string, b: string) => concat(a, b))
    const spread = reduce(words, \"\", (a: string, b: string) => concat(a, b))
    seen += len(shouted) + len(same) + len(short) + len(joined) + len(spread)
    i += 1
  }
  println(\"seen ${seen}\")
  seen
}";
    let mut files = Files::new();
    let id = files.add_source("combinator_balance.ds", SOURCE);
    let asts = [parse(&files, id).expect("the source parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the source checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "combinator_balance",
        Hash128::compute(SOURCE.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the source lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_native(native::STD_PLATFORM_MODULE, native::std_platform_module());
    loader::install(&mut registry, &module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run("combinator_balance", "main", &[]),
        Ok(Value::I64(38 * 25)),
        "every iteration answered the mapped, filtered, and folded counts"
    );
    assert_eq!(vm.captured_output(), b"seen 950\n");
    // 25 iterations each drove two maps (one adopting native-concat
    // results born inside the lambda body — the once-routed-around
    // frame-tail shape — and one transferring its borrowed param words
    // straight back, the handed-count path), one filter (borrowed
    // elements), and two reduces (one folded owner chain per list plus
    // the empty-init handout discipline): every transferred, adopted,
    // and replaced count dropped exactly once
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// A combinator call in tail position inherits the generic-table
/// reservation: CALLTG stays reserved, so the site refuses at lowering.
#[test]
fn a_tail_position_combinator_call_stays_reserved() {
    refused(
        "
tag int
struct list[t]
extern fn map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]

fn shout(xs: list[int]) -> list[int] {
  map(xs, (x: int) => x * 2)
}

fn main() -> int {
  const out = shout([1, 2])
  0
}",
        "tail call to a generic builtin",
    );
}

/// A capturing lambda as the combinator argument runs through the hosted
/// closure branch (the callable-defaulted `Fn` parameter): the staged
/// Ref word is a borrow, each per-element hosted call copies the capture
/// block in under one acquired count per `Ref` capture that the callee
/// body's placement balances, and the caller's flush drops the borrow —
/// the heap drains.
#[test]
fn a_capturing_lambda_argument_runs_the_hosted_closure_branch() {
    const SOURCE: &str = "
tag int
tag string
tag bool

struct list[t]

extern fn println (s: string) -> ()
extern fn map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]
extern fn filter[t] (xs: list[t], f: fn(t) -> bool) -> list[t]
extern fn reduce[t] (xs: list[t], init: t, f: fn(t, t) -> t) -> t
extern fn len[t] (xs: t) -> int

fn main() -> int {
  const bias = 10
  const suffix = \"!\"
  const mapped = map([1, 2], (x: int) => x + bias)
  println(\"mapped ${len(mapped)}\")
  const kept = filter([1, 2, 3, 4], (x: int) => x + bias > 12)
  println(\"kept ${len(kept)}\")
  const folded = reduce(mapped, 0, (a: int, b: int) => a + b + bias)
  println(\"folded ${folded}\")
  const shouted = map([\"hey\", \"yo\"], (w: string) => \"${w}${suffix}\")
  println(\"shouted ${len(shouted)}\")
  0
}";
    let mut files = Files::new();
    let id = files.add_source("capturing_combinators.ds", SOURCE);
    let asts = [parse(&files, id).expect("the source parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the source checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "capturing_combinators",
        Hash128::compute(SOURCE.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the source lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_native(native::STD_PLATFORM_MODULE, native::std_platform_module());
    loader::install(&mut registry, &module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run("capturing_combinators", "main", &[]),
        Ok(Value::I64(0)),
        "every closure-driven combinator answers"
    );
    assert_eq!(
        vm.captured_output(),
        b"mapped 2\nkept 2\nfolded 43\nshouted 2\n",
        "the golden stdout of the closure-driven runs"
    );
    // the borrow window, the per-call copy-ins, and the adopted result
    // counts all balanced: nothing stays live
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// A struct-returning lambda refuses: the hosted per-element call
/// answers one word, and a wider return would silently keep its first
/// word only.
#[test]
fn a_multi_word_lambda_result_refuses_at_lowering() {
    refused(
        "
tag int
struct list[t]
struct point { x: int, y: int }
extern fn map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]

fn main() -> int {
  const out = map([1, 2], (x: int) => point { x: x, y: x })
  0
}",
        "container-combinator lambda",
    );
}
