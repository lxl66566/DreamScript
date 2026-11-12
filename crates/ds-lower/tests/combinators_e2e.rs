//! The lambda-taking container combinators end to end [the reentrant
//! CALLG phase]: `map`, `filter`, `reduce`, `partition` lower every call
//! site to CALLG addressing the native's ONE body, the lambda argument
//! staging its word into the callable-defaulted `Fn` parameter (the
//! synthesized function's scalar FuncRef, a named function's minted
//! word, or a closure object's Ref word — the hosted per-element call
//! resolves either kind), and the evidence words recording the
//! receiver's list instance behind the real arguments (`map`'s
//! constraint list `[t, r]` carries two, the `map_len` precedent).
//! `partition`'s two-list tuple answers as the native channel's
//! tag-less multi-word total — no discriminant, the two `Ref` words are
//! the tuple itself. The VM reenters the dispatcher per element over a
//! synthetic reentry frame, so the block-body side effects interleave in
//! element order and map-in-lambda stacks frames on the one chain.

use ds_abi::{BuiltinInstance, Inst};
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, PlacedOp, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

/// The record's body decoded to instructions, one per pc.
fn insts(function: &ds_mod::Function) -> Vec<Inst> {
    function
        .code()
        .iter()
        .enumerate()
        .map(|(pc, word)| {
            Inst::decode(u32::try_from(pc).expect("pc fits u32"), *word)
                .expect("ds-mod validated every word")
        })
        .collect()
}

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
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
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
        let code = insts(f);
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
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
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

/// One inline source lowered and installed into a fresh VM (the same
/// discipline the fixture builder follows), for the tail-transfer probes.
fn run_inline(name: &str, source: &str) -> (Vm, Lowered) {
    let mut files = Files::new();
    let id = files.add_source(name, source);
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
    let lowered = lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed)
        .expect("the source lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    (vm, lowered)
}

/// One function's placed uop kinds, RC ops in place (the trait probe's
/// spelling).
fn shape(lowered: &Lowered, name: &str) -> Vec<String> {
    lowered
        .placed(name)
        .unwrap_or_else(|| panic!("{name} is planned"))
        .iter()
        .map(|op| match op {
            PlacedOp::Uop(uop) => {
                let repr = format!("{uop:?}");
                repr.chars().take_while(char::is_ascii_alphabetic).collect()
            },
            PlacedOp::Rc(_) => "Rc".to_owned(),
        })
        .collect()
}

/// A non-root frame's combinator tail lowers in place (CALLG plus return,
/// the native-tail shape): the one body runs inside the instruction and no
/// frame is pushed, so the stack never grows and the post-call flush
/// settles the staged borrows — the wrapper runs, and the caller's own
/// generic tail (the root frame's len transfer) abandons its staged count
/// as the accepted root-tail corner.
#[test]
fn a_non_root_combinator_tail_lowers_in_place() {
    let source = "
tag int
struct list[t]
extern fn map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]
extern fn len[t] (xs: t) -> int

fn shout(xs: list[int]) -> list[int] {
  map(xs, (x: int) => x * 2)
}

fn main() -> int {
  len(shout([1, 2]))
}";
    let (mut vm, lowered) = run_inline("non_root_combinator_tail", source);
    assert_eq!(
        vm.run("non_root_combinator_tail", "main", &[]),
        Ok(Value::I64(2)),
        "the wrapper's in-place combinator tail answers the element count"
    );
    let shout = shape(&lowered, "shout");
    assert!(
        shout.iter().any(|k| k == "Callg") && !shout.iter().any(|k| k == "Calltg"),
        "the non-root native tail stays in place: {shout:?}"
    );
}

/// The root frame's generic tail transfers through CALLTG [H2's guarantee
/// closure]: the evidence words stage after the exit claims, the uop names
/// the real arguments, the one body runs inside the instruction, and the
/// frame returns the body's result — the program's answer.
#[test]
fn a_root_generic_tail_transfers_through_calltg() {
    let source = "
tag int
tag string
extern fn len[t] (xs: t) -> int

fn main() -> int {
  len(\"four\")
}";
    let (mut vm, lowered) = run_inline("root_generic_tail", source);
    assert_eq!(
        vm.run("root_generic_tail", "main", &[]),
        Ok(Value::I64(4)),
        "the transfer runs the one body and returns its result"
    );
    let main = shape(&lowered, "main");
    assert!(
        main.iter().any(|k| k == "Calltg") && !main.iter().any(|k| k == "Callg"),
        "the root frame's generic tail is the dispatch tail transfer: {main:?}"
    );
}

/// A checked-read generic keeps its tail refusal: the ok/none span re-spans
/// into the call's union at the call site, and the transfer leaves no
/// position for it.
#[test]
fn a_checked_generic_tail_still_refuses() {
    refused(
        "
tag int
struct list[t]
extern fn list_get[t] (xs: list[t], at: int) -> t

fn main() -> int {
  list_get([1, 2], 0)
}",
        "a tail call to a checked-read generic",
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
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
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

/// A struct-returning callable runs: the hosted per-element call's
/// result channel carries whole word spans, so the answer's width
/// pushes as one element and fixes the result list's stride (the
/// heap-list slice's combinator close). Both the lambda and the
/// named-fn spellings lower, and the run drains.
#[test]
fn a_multi_word_callable_result_maps_at_the_answer_stride() {
    let source = "
tag int
tag none
struct list[t]
struct point { x: int, y: int }
extern fn map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]
extern fn list_get[t] (xs: list[t], i: int) -> t | none

fn widen(x: int) -> point {
  point { x: x, y: x * 10 }
}

fn main() -> int {
  const out = map([1, 2], (x: int) => point { x: x, y: x })
  const named = map([1, 2], widen)
  const a = match list_get(out, 1) { none => 0, p => p.x + p.y }
  const b = match list_get(named, 1) { none => 0, p => p.x * 100 + p.y }
  a + b
}";
    let (mut vm, _) = run_inline("wider_answer", source);
    assert_eq!(vm.run("wider_answer", "main", &[]), Ok(Value::I64(4 + 220)));
    // the wider answers' counts and both lists died with the frame
    assert!(vm.objects().is_empty(), "the heap drains");
}

/// `partition` answers the two lists of the tag-less total: the free and
/// UFCS spellings agree over one-word elements, an empty input answers
/// two empty lists, the predicate sees each element once in order (the
/// println interleaving pins it), and each push takes the destination
/// list's own edge count — no element-side count moves, so the heap
/// drains.
#[test]
fn partition_answers_both_sides_over_one_word_elements() {
    let source = "
tag int
tag string
tag bool
struct list[t]
extern fn println (msg: string) -> ()
extern fn partition[t] (xs: list[t], f: fn(t) -> bool) -> (list[t], list[t])
extern fn len[t] (xs: t) -> int

fn main() -> int {
  const (yes, no) = partition([1, 2, 3, 4], (x: int) => {
    println(\"visit ${x}\")
    x > 2
  })
  const words = [\"alpha\", \"beta\", \"gamma\", \"delta\"]
  const (long, short) = words.partition((w: string) => len(w) > 4)
  const (a, b) = partition(list[int](), (x: int) => x > 0)
  println(\"split ${len(yes)} ${len(no)}\")
  len(yes) * 1000 + len(no) * 100 + len(long) * 10 + len(short) + len(a) + len(b)
}";
    let (mut vm, _) = run_inline("partition_one_word", source);
    assert_eq!(
        vm.run("partition_one_word", "main", &[]),
        // yes=2, no=2, long=3, short=1, both empty sides 0
        Ok(Value::I64(2231)),
        "both sides answer at their lengths"
    );
    assert_eq!(
        vm.captured_output(),
        b"visit 1\nvisit 2\nvisit 3\nvisit 4\nsplit 2 2\n",
        "the hosted predicate runs once per element in order, before either side is read"
    );
    assert!(vm.objects().is_empty(), "the heap drains");
}

/// `partition` runs at the element stride over struct-typed elements —
/// the wider side is the point: the hosted predicate takes the element's
/// whole span, each side pushes at the source's stride, and a non-root
/// frame's partition tail returns the tuple in place (the native-tail
/// shape over a two-word result). The heap drains.
#[test]
fn partition_runs_at_the_element_stride_over_structs() {
    let source = "
tag int
tag bool
struct list[t]
struct point { x: int, y: int }
extern fn partition[t] (xs: list[t], f: fn(t) -> bool) -> (list[t], list[t])
extern fn len[t] (xs: t) -> int

fn split_pts(pts: list[point]) -> (list[point], list[point]) {
  partition(pts, (p: point) => p.x > 1)
}

fn main() -> int {
  const src = [point { x: 1, y: 10 }, point { x: 2, y: 20 }, point { x: 3, y: 30 }]
  const (hot, cold) = split_pts(src)
  var total = 0
  for p in hot {
    total += p.y
  }
  total * 100 + len(cold)
}";
    let (mut vm, _) = run_inline("partition_stride", source);
    // hot holds the x>1 points (y 20+30), cold the one x=1 point
    assert_eq!(
        vm.run("partition_stride", "main", &[]),
        Ok(Value::I64(5001))
    );
    assert!(vm.objects().is_empty(), "the heap drains");
}

/// `Ref`-carrying elements balance through both sides: each pushed word
/// takes its destination list's edge count and the source list keeps
/// its own, over string elements with a capturing predicate riding the
/// closure branch (the callable-defaulted `Fn` word).
#[test]
fn partition_balances_ref_words_with_a_capturing_predicate() {
    let source = "
tag int
tag string
tag bool
struct list[t]
extern fn concat (a: string, b: string) -> string
extern fn partition[t] (xs: list[t], f: fn(t) -> bool) -> (list[t], list[t])
extern fn reduce[t] (xs: list[t], init: t, f: fn(t, t) -> t) -> t
extern fn len[t] (xs: t) -> int

fn main() -> int {
  const mark = \"!\"
  const words = [\"alpha\", \"be\", \"gamma\", \"do\"]
  const (shouted, short) = partition(words, (w: string) => len(w) > 3)
  const glued = reduce(shouted, \"\", (a: string, w: string) => concat(a, concat(w, mark)))
  len(glued) * 10 + len(short)
}";
    let (mut vm, _) = run_inline("partition_refs", source);
    // shouted = alpha+gamma both glued with "!": 12 chars, short = 2
    assert_eq!(vm.run("partition_refs", "main", &[]), Ok(Value::I64(122)));
    assert!(vm.objects().is_empty(), "the heap drains");
}

/// A partition call site's staging is filter's own, byte-stable: over
/// probes identical but for the callee, the placed sequence through the
/// one Callg (list borrow, fn word, evidence, the dispatch) matches the
/// equivalent filter site's up to frame slot numbering (the wider
/// result temp shifts the frame's slots, never the discipline), the
/// Callg operands agree byte for byte, and the whole function lowers
/// to the filter module plus exactly one extra Mov — the tuple's second
/// result-gather word.
#[test]
fn a_partition_site_lowers_the_filter_staging_plus_the_two_word_result() {
    let staged = |combinator: &str, bind: &str| {
        let source = format!(
            "
tag int
tag bool
struct list[t]
extern fn {combinator}[t] (xs: list[t], f: fn(t) -> bool) -> {ret}
extern fn len[t] (xs: t) -> int

fn main() -> int {{
  const {bind} = {combinator}([1, 2, 3], (x: int) => x > 1)
  0
}}",
            ret = if combinator == "partition" { "(list[t], list[t])" } else { "list[t]" },
        );
        let (_, lowered) = run_inline("partition_staging", &source);
        let placed = lowered
            .placed("main")
            .expect("main is planned")
            .iter()
            .map(|op| format!("{op:?}"))
            .collect::<Vec<_>>();
        let at = placed
            .iter()
            .position(|op| op.starts_with("Uop(Callg"))
            .expect("one Callg names the dispatch");
        (placed, at)
    };
    // digit runs collapse to one mark: frame slot numbers shift with the
    // wider result temp, and the discipline must not
    let normalize = |ops: &[String]| {
        ops.iter()
            .map(|op| {
                let mut out = String::new();
                let mut mark = false;
                for c in op.chars() {
                    if c.is_ascii_digit() {
                        if !mark {
                            out.push('#');
                            mark = true;
                        }
                    } else {
                        out.push(c);
                        mark = false;
                    }
                }
                out
            })
            .collect::<Vec<_>>()
    };
    let (filter_ops, filter_at) = staged("filter", "kept");
    let (partition_ops, partition_at) = staged("partition", "(yes, no)");
    assert_eq!(
        normalize(&filter_ops[..=filter_at]),
        normalize(&partition_ops[..=partition_at]),
        "the staging through the dispatch is the filter discipline, unchanged"
    );
    // the Callg itself is byte-identical: same pool slot, same nargs
    assert_eq!(filter_ops[filter_at], partition_ops[partition_at]);
    assert!(filter_ops[filter_at].contains("nargs: 2"));
    // and the whole function is the filter module plus one Mov — the
    // tuple's second gather word — and one Rc dec — the second list's
    // death at scope end (the flush dec follows the dispatch, then the
    // gather Movs)
    let mut with_second_word = normalize(&filter_ops);
    with_second_word.insert(filter_at + 2, "Uop(Mov { dst: #, src: # })".to_owned());
    let ret_at = with_second_word
        .iter()
        .rposition(|op| op.starts_with("Uop(Ret"))
        .expect("main returns");
    with_second_word.insert(ret_at, "Rc(Dec { src: # })".to_owned());
    assert_eq!(
        normalize(&partition_ops),
        with_second_word,
        "partition lowers to the filter emission plus the tuple's second gather word and death"
    );
}

/// The wider fold: `reduce` over struct-typed elements at the interior
/// span — the init's whole word span stages between the list and the
/// callable, the callable receives the accumulator span plus the
/// element span per step and answers a whole span, and each step's
/// answer replaces the owned accumulator (the first step's borrowed
/// init never drops). The println interleaving pins the visit order and
/// the running accumulator's leading position, the capturing lambda
/// rides the callable-defaulted closure word past the interior span,
/// and the empty list answers the staged init. The heap drains.
#[test]
fn reduce_folds_struct_elements_at_the_interior_span() {
    let source = "
tag int
tag string
tag bool
struct list[t]
struct point { x: int, y: int }
extern fn println (msg: string) -> ()
extern fn reduce[t] (xs: list[t], init: t, f: fn(t, t) -> t) -> t
extern fn len[t] (xs: t) -> int

fn add_pts(a: point, p: point) -> point {
  point { x: a.x + p.x, y: a.y + p.y }
}

fn main() -> int {
  const bias = 100
  const xs = [point { x: 1, y: 10 }, point { x: 2, y: 20 }, point { x: 3, y: 30 }]
  const total = reduce(xs, point { x: 0, y: 0 }, (a: point, p: point) => {
    println(\"visit ${a.x} ${p.x}\")
    point { x: a.x + p.x + bias, y: a.y + p.y }
  })
  const named = xs.reduce(point { x: 10, y: 0 }, add_pts)
  const empty = reduce(list[point](), point { x: 7, y: 5 }, add_pts)
  println(\"total ${total.x} ${total.y}\")
  len(xs) * 100000 + total.x * 100 + total.y * 10 + named.x + empty.x
}";
    let (mut vm, _) = run_inline("reduce_span", source);
    assert_eq!(
        vm.run("reduce_span", "main", &[]),
        Ok(Value::I64(300_000 + 30_600 + 600 + 16 + 7)),
        "the folded record answers through its fields, the named-fn UFCS fold agrees, and the \\
         empty list answers its init"
    );
    assert_eq!(
        vm.captured_output(),
        b"visit 0 1\nvisit 101 2\nvisit 203 3\ntotal 306 60\n",
        "the callable visits each element once in order with the accumulator leading"
    );
    assert!(vm.objects().is_empty(), "the heap drains");
}

/// The fold's `Ref`-word balance across steps (C4): the accumulator
/// span carries a `Ref` word (a string field), so each step's answer
/// replaces the owned one per `Ref` word while the callable's copy-ins
/// are claimed for the callee, and the empty fold hands the borrowed
/// init out under its own count — driven in a loop, the heap drains.
#[test]
fn reduce_balances_ref_words_across_the_fold_steps() {
    const SOURCE: &str = "
tag int
tag string
tag bool
struct list[t]
struct tagline { text: string, hits: int }
extern fn concat (a: string, b: string) -> string
extern fn reduce[t] (xs: list[t], init: t, f: fn(t, t) -> t) -> t
extern fn len[t] (xs: t) -> int

fn merge(a: tagline, t: tagline) -> tagline {
  tagline { text: concat(a.text, t.text), hits: a.hits + t.hits }
}

fn main() -> int {
  const seed = tagline { text: \"go:\", hits: 0 }
  const parts = [tagline { text: \"one\", hits: 1 }, tagline { text: \"two\", hits: 2 }]
  var seen = 0
  var i = 0
  while i < 25 {
    const merged = reduce(parts, seed, merge)
    const empty = reduce(list[tagline](), seed, merge)
    seen += len(merged.text) + merged.hits + len(empty.text)
    i += 1
  }
  seen
}";
    let mut files = Files::new();
    let id = files.add_source("reduce_ref_balance.ds", SOURCE);
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
        "reduce_ref_balance",
        Hash128::compute(SOURCE.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the source lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    // 25 folds each replaced the owned string-field accumulator twice and
    // handed the empty fold's borrowed init out under its own count
    assert_eq!(
        vm.run("reduce_ref_balance", "main", &[]),
        Ok(Value::I64(25 * (9 + 3 + 3))),
        "every fold answered its merged text, hits, and the empty handout"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// `partition` over tuple elements: the element's positional span rides
/// the predicate each step, the two-answer total destructures at the
/// call site, and the sides read back through destructuring (the
/// record-stride sibling over the tuple vocabulary).
#[test]
fn partition_runs_over_tuple_elements() {
    let source = "
tag int
tag bool
struct list[t]
extern fn partition[t] (xs: list[t], f: fn(t) -> bool) -> (list[t], list[t])
extern fn len[t] (xs: t) -> int

fn split_pairs(pairs: list[(int, int)]) -> (list[(int, int)], list[(int, int)]) {
  partition(pairs, (t: (int, int)) => {
    const (a, b) = t
    a > 1
  })
}

fn main() -> int {
  const src = [(1, 10), (2, 20), (3, 30)]
  const (hot, cold) = split_pairs(src)
  var total = 0
  for p in hot {
    const (a, b) = p
    total += b
  }
  total * 100 + len(cold)
}";
    let (mut vm, _) = run_inline("partition_tuples", source);
    // hot holds the a>1 pairs (b 20+30), cold the one a=1 pair
    assert_eq!(
        vm.run("partition_tuples", "main", &[]),
        Ok(Value::I64(5001))
    );
    assert!(vm.objects().is_empty(), "the heap drains");
}
