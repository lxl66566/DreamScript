//! The in-language nursery/spawn surface end to end: the
//! `nursery` block opens a fresh pool nursery through the seam, its
//! `spawn` expressions become synthesized child functions (the lambda
//! construction's capture list, staged through a closure word), and
//! scope exit joins — driving children in spawn order, first failure
//! raising the cancel flag, siblings observing it at their next backward
//! jump, and the failing child's error payload unioning out through the
//! enclosing function's return union like a `?` exit.

use ds_abi::Inst;
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, Lowered, lower};
use ds_mod::{FuncRefKey, Hash128, read_checked, write};
use ds_parse::{parse, parse_script};
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

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

const AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "pub tag bool\n",
    "pub trait Copyable { }\n",
    "pub enum net_err {\n",
    "  timeout\n",
    "  refused\n",
    "}\n",
    "pub struct list[t]\n",
    "pub extern fn list_new[t] () -> list[t]\n",
    "pub extern fn list_len[t] (xs: list[t]) -> int\n",
    "pub extern fn list_get[t] (xs: list[t], i: int) -> t | none\n",
    "pub extern fn list_push[t] (xs: mut list[t], x: t) -> ()\n",
    "pub extern fn str_int (x: int) -> string\n",
    "pub extern fn println (msg: string) -> ()\n",
    "pub extern fn cancel[t] (e: t) -> ()\n",
);

/// Lowers one source against the ambient, round-trips it, and installs
/// it into a fresh VM with both native tables (pool FuncRefs validate at
/// install time).
fn build_vm(source: &str, script: bool) -> (Vm, Lowered) {
    let mut files = Files::new();
    let amb = files.add_source("ambient.ds", AMBIENT);
    let id = files.add_source("probe.ds", source);
    let asts = [
        parse(&files, amb).expect("the ambient parses"),
        if script {
            parse_script(&files, id).expect("the script parses")
        } else {
            parse(&files, id).expect("the fixture parses")
        },
    ];
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
        "probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers");
    let bytes = write(lowered.module());
    let module = read_checked(&bytes, source.as_bytes()).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    (Vm::new(registry), lowered)
}

fn run_main(source: &str, script: bool) -> (Vm, String) {
    let (mut vm, _) = build_vm(source, script);
    vm.capture_output();
    let out = vm.run("probe", "main", &[]);
    assert_eq!(
        out,
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns unit"
    );
    let text = String::from_utf8(vm.captured_output().to_vec()).expect("utf-8 stdout");
    (vm, text)
}

/// The lowerer refuses with the exact reason naming the span.
fn refused(source: &str, needle: &str) {
    let mut files = Files::new();
    let amb = files.add_source("ambient.ds", AMBIENT);
    let id = files.add_source("probe.ds", source);
    let asts = [
        parse(&files, amb).expect("the ambient parses"),
        parse(&files, id).expect("the fixture parses"),
    ];
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
    match lower(
        "probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    ) {
        Err(LowerError::Unsupported { reason, .. }) => {
            assert!(reason.contains(needle), "refused with `{reason}`");
        },
        Err(other) => panic!("the wrong refusal: {other:?}"),
        Ok(_) => panic!("the source must refuse at lowering"),
    }
}

/// The checker refuses with the exact reason naming the span.
fn check_refused(source: &str, needle: &str) {
    let mut files = Files::new();
    let amb = files.add_source("ambient.ds", AMBIENT);
    let id = files.add_source("probe.ds", source);
    let asts = [
        parse(&files, amb).expect("the ambient parses"),
        parse(&files, id).expect("the fixture parses"),
    ];
    let analyzed = analyze(&files, &asts);
    let text = analyzed
        .checked
        .diags
        .iter()
        .map(|diag| human(diag, &files))
        .collect::<String>();
    assert!(
        text.contains(needle),
        "the check refuses with `{needle}`, got: {text}"
    );
}

/// Three spawns into one nursery answer through the join channel: the
/// children's answer spans collect into one list in spawn order (1, 2,
/// 3) through the nursery's result channel — and the heap drains when the
/// run ends, the collected list's own count included.
#[test]
fn spawns_join_collecting_answers_in_spawn_order() {
    let (vm, out) = run_main(
        r"
fn main() -> () {
  const xs = nursery {
    spawn { 1 }
    spawn { 2 }
    spawn { 3 }
  }
  println(str_int(xs.list_len()))
  for x in xs {
    println(str_int(x))
  }
  ()
}
",
        false,
    );
    assert_eq!(out, "3\n1\n2\n3\n");
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// First failure cancels the rest: the failing child declares its error
/// first, the spinning sibling stops at its first backward branch (a
/// million loop iterations never run), and the boundary unions the
/// payload out through the return union — the caller's match catches the
/// member.
#[test]
fn first_failure_cancels_siblings_at_safe_points_and_unions_out() {
    let (_, out) = run_main(
        r#"
fn fetch_all(n: int) -> int | net_err {
  nursery {
    spawn {
      cancel(timeout)
    }
    spawn {
      var i = 0
      while i < n {
        i = i + 1
      }
      i
    }
  }
  0
}

fn main() -> () {
  match fetch_all(1000000) {
    timeout => println("caught timeout"),
    refused => println("caught refused"),
    n => println("kept ${n}")
  }
}
"#,
        false,
    );
    assert_eq!(out, "caught timeout\n");
}

/// A completing first child lands before a later failure cancels: first
/// failure is spawn-drive order, so the first child's observable work
/// (its print) stands, and the failed join takes no collected answers —
/// the answer that completed dies at the boundary instead of riding out.
#[test]
fn a_completed_first_child_lands_before_a_later_failure_cancels() {
    let (vm, out) = run_main(
        r#"
fn mixed(n: int) -> int | net_err {
  nursery {
    spawn {
      println("first landed")
      7
    }
    spawn {
      cancel(refused)
    }
    spawn {
      var i = 0
      while i < n {
        i = i + 1
      }
      i
    }
  }
  0
}

fn main() -> () {
  match mixed(500000) {
    timeout => println("t"),
    refused => println("r"),
    n => println("kept ${n}")
  }
}
"#,
        false,
    );
    assert_eq!(out, "first landed\nr\n");
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// A deep tail-looping child runs unbounded self-recursion at a constant
/// segment [: per-fiber TCO] and the join still completes; a
/// straight-line child under the raised flag finishes its in-flight work
/// and lands cooperatively.
#[test]
fn a_tail_looping_child_joins_and_a_straight_line_sibling_lands() {
    let (_, out) = run_main(
        r#"
fn spin(n: int) -> int {
  if n == 0 {
    0
  } else {
    spin(n - 1)
  }
}

fn guarded_cancel() -> int | net_err {
  nursery {
    spawn {
      println("looped ${spin(20000)}")
    }
    spawn {
      cancel(refused)
    }
  }
  0
}

fn main() -> () {
  match guarded_cancel() {
    timeout => println("t"),
    refused => println("r"),
    n => println("kept ${n}")
  }
}
"#,
        false,
    );
    assert_eq!(out, "looped 0\nr\n");
}

/// A completing child fires its frame's deinit claim exactly once at its
/// own frame exit [composes with ]: the child's declared value dies
/// with its fiber, and the run's heap drains.
#[test]
fn a_completing_child_fires_its_deinit_claim_once() {
    let (vm, out) = run_main(
        r#"
struct guarded: ~Copyable {
  id: int
  deinit {
    println("freed ${self.id}")
  }
}

fn main() -> () {
  nursery {
    spawn {
      const g = guarded { id: 1 }
      println("child ran ${g.id}")
    }
    spawn {
      const h = guarded { id: 2 }
      println("child ran ${h.id}")
    }
  }
  ()
}
"#,
        false,
    );
    assert_eq!(out, "child ran 1\nfreed 1\nchild ran 2\nfreed 2\n");
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// The join channel's element discipline: a `~Copyable` answer refuses
/// at the spawn site exactly like a list literal element — the
/// collected list's edge retention owns counts, never claims, so the
/// answer's deinit hook has no drop position inside the channel.
#[test]
fn a_deinit_carrying_spawn_answer_refuses() {
    refused(
        r#"
struct claimed: ~Copyable {
  id: int
  deinit {
    println("freed ${self.id}")
  }
}

fn work(n: int) -> claimed { claimed { id: n } }

fn main() -> () {
  nursery {
    spawn { work(7) }
  }
  ()
}
"#,
        "per-span deinit claims own the seam",
    );
}

/// A spawned child answering a non-`~Copyable` reference or a scalar
/// keeps the balanced answer discipline: the reference's one dec
/// runs at the child's own completion instead of riding the join, the
/// scalar answers nothing, and the run's heap drains with the same
/// single dec the seam used to owe.
#[test]
fn a_spawned_reference_or_scalar_answer_keeps_its_single_dec() {
    let (vm, out) = run_main(
        r#"
fn make() -> list[int] { list[int]() }

fn main() -> () {
  nursery {
    spawn { make() }
    spawn { 1 }
  }
  println("done")
}
"#,
        false,
    );
    assert_eq!(out, "done\n");
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// The tutorial's fetch shape over the channel: the spawned bodies'
/// match answers ride the join in spawn order while the cancel arms
/// union out at the boundary on failure — the clean run collects the
/// pages in url order, the failing run catches the member, and the heap
/// drains both times.
#[test]
fn the_tutorial_fetch_shape_collects_pages_and_unions_errors() {
    let (vm, out) = run_main(
        r#"
fn fetch_body(u: string) -> string | net_err {
  if u == "https://down.example" {
    refused
  } else {
    if u == "https://slow.example" {
      timeout
    } else {
      "body of ${u}"
    }
  }
}

fn fetch_all(urls: list[string]) -> list[string] | net_err {
  nursery {
    for u in urls {
      spawn {
        match fetch_body(u) {
          timeout => cancel(timeout)
          refused => cancel(refused)
          body => body
        }
      }
    }
  }
}

fn main() -> () {
  const good = ["https://ds.dev", "https://example.org"]
  match fetch_all(good) {
    timeout => println("caught timeout")
    refused => println("caught refused")
    bodies => {
      println("kept ${bodies.list_len()}")
      for b in bodies {
        println(b)
      }
    }
  }
  const bad = ["https://ds.dev", "https://down.example", "https://slow.example"]
  match fetch_all(bad) {
    timeout => println("caught timeout")
    refused => println("caught refused")
    bodies => println("kept ${bodies.list_len()}")
  }
}
"#,
        false,
    );
    assert_eq!(
        out,
        "kept 2\nbody of https://ds.dev\nbody of https://example.org\ncaught refused\n"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// A nursery with no spawn answers the empty list: the join channel
/// stands open with nothing to collect, the unit element typing the
/// block's value.
#[test]
fn an_empty_nursery_answers_the_empty_list() {
    let (vm, out) = run_main(
        r#"
fn main() -> () {
  const xs = nursery {
    println("no children stand")
  }
  println(str_int(xs.list_len()))
  ()
}
"#,
        false,
    );
    assert_eq!(out, "no children stand\n0\n");
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// Bare `spawn` outside any nursery refuses at lowering: bare detach
/// requires an explicit root nursery or supervisor.
#[test]
fn a_bare_spawn_refuses_naming_the_root_nursery_story() {
    refused(
        r#"
fn main() -> () {
  spawn {
    println("never")
  }
}
"#,
        "spawn exists only inside a nursery block; bare detach requires an explicit root nursery \
         or supervisor",
    );
}

/// A `cancel` payload outside the enclosing function's return union
/// refuses at the spawn site — the boundary unions child errors out
/// through the return, so an uncontained member has nowhere to land.
#[test]
fn an_uncontained_cancel_payload_refuses() {
    refused(
        r#"
enum local_err {
  bad
}

fn f(n: int) -> int | net_err {
  nursery {
    spawn {
      cancel(bad)
    }
  }
  n
}

fn main() -> () {
  match f(1) {
    timeout => println("t"),
    refused => println("r"),
    v => println("kept ${v}")
  }
}
"#,
        "the error member is not contained in the enclosing function's return union",
    );
}

/// A scalar `cancel` payload unions out through the boundary: the child
/// mints its member's tag ahead of the int word, the boundary delivers
/// the two-word span, and the caller's variable arm extracts the int.
#[test]
fn a_scalar_cancel_payload_unions_out() {
    let (_, out) = run_main(
        r#"
fn f(n: int) -> int | net_err {
  nursery {
    spawn {
      cancel(n)
    }
  }
  n
}

fn main() -> () {
  match f(7) {
    timeout => println("t"),
    refused => println("r"),
    v => println("kept ${v}")
  }
}
"#,
        false,
    );
    assert_eq!(out, "kept 7\n");
}

/// A payloaded member cancels end to end: the child stages the struct
/// member's whole flat span behind its tag, the boundary delivers it,
/// the fail path gathers the payload words into the return union's
/// claimed offsets, and both the caller's catch arm and the `.err`
/// handler read the fields. The carried string's count crosses the
/// death handoff and the RETSPAN, so the run drains.
#[test]
fn a_payloaded_member_cancels_and_extracts_end_to_end() {
    let (vm, out) = run_main(
        r#"
struct fail { code: int, reason: string }

fn f() -> fail | none {
  nursery {
    spawn {
      cancel(fail { code: 3, reason: "gateway" })
    }
  }
  none
}

fn g() -> int | fail {
  nursery {
    spawn {
      cancel(fail { code: 9, reason: "handoff" })
    }
  }
  1
}

fn main() -> () {
  match f() {
    none => println("clean")
    e => println("code ${e.code} reason ${e.reason}")
  }
  const r = g().err((e: fail) => e.code)
  println("err ${r}")
  println("done")
}
"#,
        false,
    );
    assert_eq!(out, "code 3 reason gateway\nerr 9\ndone\n");
    assert!(
        vm.objects().is_empty(),
        "the payloaded cancel's counts balance ({} live)",
        vm.objects().len()
    );
}

/// The generic-application clause: a union member over `list[int]`
/// cancels end to end. The member claims one Ref word (the heap
/// application's shape), the child stages the captured list behind its
/// minted tag, and the caller's catch arm reads the list through its
/// natives — the captured list's count rides the death handoff whole.
#[test]
fn a_list_member_cancels_end_to_end() {
    let (vm, out) = run_main(
        r#"
fn f(n: int) -> list[int] | none {
  const xs = [1, 2, n]
  nursery {
    spawn {
      cancel(xs)
    }
  }
  none
}

fn main() -> () {
  match f(3) {
    none => println("clean")
    xs => println("list ${xs.list_len()}")
  }
  println("done")
}
"#,
        false,
    );
    assert_eq!(out, "list 3\ndone\n");
    assert!(
        vm.objects().is_empty(),
        "the captured list's count balances ({} live)",
        vm.objects().len()
    );
}

/// A member whose payload is itself a union rides the child's cancel
/// into the join like any optional slot (the union's own
/// tag-plus-region span): the none-carrying spelling once dropped out
/// of the case's atom and the cancel path refused at lowering over the
/// wrong vocabulary.
#[test]
fn a_union_payloaded_member_rides_the_cancel_into_the_join() {
    let (_, out) = run_main(
        r#"
enum wrap {
  carrier(int | none)
}

fn f(x: int | none) -> int | wrap {
  nursery {
    spawn {
      cancel(carrier(x))
    }
  }
  1
}

fn main() -> () {
  match f(1) {
    carrier(n) => {
      match n {
        none => println("inner-none")
        v => println("inner-some ${v}")
      }
    }
    v => println("kept")
  }
  match f(none) {
    carrier(n) => {
      match n {
        none => println("second-none")
        v => println("second-some ${v}")
      }
    }
    v => println("kept2")
  }
}
"#,
        false,
    );
    assert_eq!(out, "inner-some 1\nsecond-none\n");
}

/// A `?` inside a nursery body refuses: the early return would skip the
/// block's join, so the site names the scope-exit contract.
#[test]
fn a_question_mark_inside_a_nursery_refuses() {
    refused(
        r#"
fn f() -> int | none {
  nursery {
    spawn { 1 }
    const maybe = list_get(list[int](), 0)
    println(str_int(maybe?))
  }
  0
}

fn main() -> () {
  match f() {
    none => println("none"),
    v => println("kept ${v}")
  }
}
"#,
        "the block's scope exit must join its children",
    );
}

/// A spawned body's own `?` refuses: the child's frame returns its own
/// completion value, and the enclosing function's return union is not
/// the child's. The tail is the join channel's answer, so the exit
/// refuses at the check — the value-position rule names the consumed
/// position — before the lowering's own spawn-exit guard ever stands.
#[test]
fn a_question_mark_inside_a_spawn_body_refuses() {
    check_refused(
        r#"
fn f(n: int | net_err) -> int | net_err {
  nursery {
    spawn {
      return n?
    }
  }
  n
}

fn main() -> () {
  match f(1) {
    timeout => println("t"),
    refused => println("r"),
    v => println("kept ${v}")
  }
}
"#,
        "return in a value position",
    );
}

/// The lowering shape is pinned: one open seam call (zero arguments),
/// one spawn seam call per `spawn` (nursery id plus the callable), and
/// one boundary seam call (id plus the containment mask) — no CALLT or
/// CALLG reaches a nursery seam.
#[test]
fn every_nursery_site_is_one_plain_seam_call() {
    let (_, lowered) = build_vm(
        r#"
fn f() -> int | net_err {
  nursery {
    spawn {
      1
    }
    spawn {
      cancel(refused)
    }
  }
  0
}

fn main() -> () {
  match f() {
    timeout => println("t"),
    refused => println("r"),
    v => println("kept ${v}")
  }
}
"#,
        false,
    );
    let mut opens = 0;
    let mut spawns = 0;
    let mut boundaries = 0;
    for f in lowered.module().functions() {
        let code = insts(f);
        for inst in code {
            let Inst::Uop(uop) = inst else {
                continue;
            };
            let (ds_abi::Uop::Call { callee, nargs }
            | ds_abi::Uop::Callt { callee, nargs }
            | ds_abi::Uop::Callg { callee, nargs }) = uop
            else {
                continue;
            };
            let ds_mod::Constant::FuncRef {
                key: FuncRefKey::Native(module),
                func,
            } = &f.constants()[usize::from(callee.0)]
            else {
                continue;
            };
            if module != "builtin" {
                continue;
            }
            match *func {
                41 => {
                    opens += 1;
                    assert_eq!(nargs, 0, "the open seam stages nothing");
                },
                42 => spawns += 1,
                43 => {
                    boundaries += 1;
                    assert_eq!(nargs, 2, "the boundary seam stages id plus mask");
                },
                _ => {},
            }
            if *func >= 41 && *func <= 44 {
                assert!(
                    !matches!(uop, ds_abi::Uop::Callt { .. } | ds_abi::Uop::Callg { .. }),
                    "the nursery seams never lower in tail or generic position"
                );
            }
        }
    }
    assert_eq!(opens, 1, "one open seam per nursery block");
    assert_eq!(spawns, 2, "one spawn seam per spawn expression");
    assert_eq!(boundaries, 1, "one boundary seam per nursery block");
}

/// A script entry runs the nursery surface too: the top-level
/// statements fold into the synthesized `main`, and a nursery block joins
/// there like in any function.
#[test]
fn the_script_entry_runs_nurseries_too() {
    let (_, out) = run_main(
        r"
const xs = nursery {
  spawn { 9 }
}
println(str_int(xs.list_len()))
",
        true,
    );
    assert_eq!(out, "1\n");
}
