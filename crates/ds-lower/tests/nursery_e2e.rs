//! The in-language nursery/spawn surface end to end [E2, E5]: the
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
use ds_mod::{Hash128, read_checked, write};
use ds_parse::{parse, parse_script};
use ds_vm::{ModuleRegistry, Value, Vm, loader, native};

const AMBIENT: &str = concat!(
    "tag int\n",
    "tag string\n",
    "tag bool\n",
    "trait Copyable { }\n",
    "enum net_err {\n",
    "  timeout\n",
    "  refused\n",
    "}\n",
    "struct list[t]\n",
    "extern fn list_new[t] () -> list[t]\n",
    "extern fn list_len[t] (xs: list[t]) -> int\n",
    "extern fn list_get[t] (xs: list[t], i: int) -> t | none\n",
    "extern fn list_push[t] (xs: mut list[t], x: t) -> ()\n",
    "extern fn str_int (x: int) -> string\n",
    "extern fn println (msg: string) -> ()\n",
    "extern fn cancel[t] (e: t) -> ()\n",
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
    registry.install_native(native::STD_PLATFORM_MODULE, native::std_platform_module());
    loader::install(&mut registry, &module).expect("the module installs");
    (Vm::new(registry), lowered)
}

fn run_main(source: &str, script: bool) -> (Vm, String) {
    let (mut vm, _) = build_vm(source, script);
    vm.capture_output();
    let out = vm.run("probe", "main", &[]);
    assert_eq!(out, Ok(Value::I64(0)), "main returns unit");
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

/// Three spawns into one nursery append to a shared list through their
/// captured Ref: join drives children in spawn order (1, 2, 3), the
/// shared object carries the mutation, and the heap drains when the run
/// ends — the child frames' capture counts balance their copy-in incs.
#[test]
fn spawns_join_in_spawn_order_over_a_shared_list() {
    let (vm, out) = run_main(
        r#"
fn main() -> () {
  var xs = list[int]()
  nursery {
    spawn {
      xs.list_push(1)
    }
    spawn {
      xs.list_push(2)
    }
    spawn {
      xs.list_push(3)
    }
  }
  println(str_int(xs.list_len()))
  for x in xs {
    println(str_int(x))
  }
}
"#,
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
  var xs = list[int]()
  nursery {
    spawn {
      cancel(timeout)
    }
    spawn {
      var i = 0
      while i < n {
        xs.list_push(i)
        i = i + 1
      }
    }
  }
  const kept = xs.list_len()
  kept
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

/// A clean nursery with cancelling children keeps the later siblings'
/// completed values: first failure is spawn-drive order, so a completing
/// first child's pushes survive a later child's failure.
#[test]
fn a_completed_first_child_lands_before_a_later_failure_cancels() {
    let (_, out) = run_main(
        r#"
fn mixed(n: int) -> int | net_err {
  var xs = list[int]()
  nursery {
    spawn {
      xs.list_push(7)
    }
    spawn {
      cancel(refused)
    }
    spawn {
      var i = 0
      while i < n {
        xs.list_push(i)
        i = i + 1
      }
    }
  }
  const kept = xs.list_len()
  kept
}

fn main() -> () {
  match mixed(500000) {
    timeout => println("caught timeout"),
    refused => println("caught refused"),
    n => println("kept ${n}")
  }
}
"#,
        false,
    );
    assert_eq!(out, "caught refused\n");
}

/// A deep tail-looping child runs unbounded self-recursion at a constant
/// segment [H2: per-fiber TCO] and the join still completes; a
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
/// own frame exit [C5 composes with E2]: the child's declared value dies
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

/// Bare `spawn` outside any nursery refuses at lowering, naming the
/// ruled surface [E2]: bare detach requires an explicit root nursery or
/// supervisor.
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

/// A scalar `cancel` payload refuses: the boundary unions one bare enum
/// member word this round.
#[test]
fn a_scalar_cancel_payload_refuses() {
    refused(
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
  match f(1) {
    timeout => println("t"),
    refused => println("r"),
    v => println("kept ${v}")
  }
}
"#,
        "the error value must be a bare enum member this round",
    );
}

/// A `?` inside a nursery body refuses: the early return would skip the
/// block's join, so the site names the scope-exit contract.
#[test]
fn a_question_mark_inside_a_nursery_refuses() {
    refused(
        r#"
fn f() -> int | none {
  var xs = list[int]()
  nursery {
    spawn {
      xs.list_push(1)
    }
    println(str_int(xs.list_get(0)?))
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
/// the child's.
#[test]
fn a_question_mark_inside_a_spawn_body_refuses() {
    refused(
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
        "a spawned body runs as its own fiber and completes with its own value",
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
  var xs = list[int]()
  nursery {
    spawn {
      xs.list_push(1)
    }
    spawn {
      cancel(refused)
    }
  }
  const kept = xs.list_len()
  kept
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
        let code = f.code();
        for inst in code.iter() {
            let Inst::Uop(uop) = inst else {
                continue;
            };
            let (callee, nargs) = match uop {
                ds_abi::Uop::Call { callee, nargs } => (callee, *nargs),
                ds_abi::Uop::Callt { callee, nargs } => (callee, *nargs),
                ds_abi::Uop::Callg { callee, nargs } => (callee, *nargs),
                _ => continue,
            };
            let ds_mod::Constant::FuncRef { module, func } = &f.constants()[usize::from(callee.0)]
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
                44 => {},
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
        r#"
var xs = list[int]()
nursery {
  spawn {
    xs.list_push(9)
  }
}
println(str_int(xs.list_len()))
"#,
        true,
    );
    assert_eq!(out, "1\n");
}
