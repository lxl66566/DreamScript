//! The staged-native borrow window in non-root frames (C4): a plain
//! native tail-called from a `fn` frame lowers in place (CALL + RET), so
//! the post-call flush drops the staged borrows and the return's exit
//! drops the rest — the CALLT-to-native path pops the frame inside the
//! instruction, which would strand the counts in a program that keeps
//! running at the caller. The root frame (`main`) keeps the tail
//! transfer: its CALLT ends the program, where the heap dies whole (the
//! named corner). The fixtures pin the balance end to end under the diag
//! tripwires and the placed shapes structurally.
//!
//! Stage three's temporary slice changed two emissions deliberately: a
//! KSTR literal whose count never moves in its frame loads as
//! KSTR_BORROW with no terminal dec, so `bang`'s "!" literal carries no
//! birth count and the root's staged "done" abandons nothing — its
//! count never existed. The forwarded parameter count (a word staged
//! into a local callee) keeps its dec, and the frames still drain.

use ds_abi::RcOp;
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, PlacedOp, lower};
use ds_mod::{Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

const PRELUDE: &str = "
tag int
tag string
tag bool

struct list[t]

extern fn println (s: string) -> ()
extern fn concat (a: string, b: string) -> string
extern fn map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]
extern fn list_push[t] (xs: mut list[t], x: t) -> ()
extern fn list_get[t] (xs: list[t], i: int) -> t | none
";

const BALANCE: &str = "
fn twice_str(w: string) -> string {
  concat(w, \"!\")
}

fn shout(w: string, loud: bool) -> string {
  if loud {
    return concat(w, \"!\")
  }
  concat(w, \"?\")
}

fn word_at(xs: list[string], at: int) -> string {
  const cell = list_get(xs, at)
  cell.ok((w: string) => w).err((_: none) => \"?\")
}

fn main() -> int {
  println(twice_str(\"ab\"))
  println(shout(\"cd\", true))
  println(shout(\"ef\", false))
  var words = list[string]()
  list_push(words, \"alpha\")
  const banged = map(words, (w: string) => concat(w, \"!\"))
  const first = list_get(banged, 0)
  println(first.ok((w: string) => w).err((_: none) => \"?\"))
  0
}";

const TAIL_SHAPES: &str = "
fn bang(w: string) -> string {
  concat(w, \"!\")
}

fn main() -> () {
  println(bang(\"ab\"))
  println(\"done\")
}";

fn lower_inline(source: &str) -> Lowered {
    let mut files = Files::new();
    let id = files.add_source("native_frame_balance.ds", source);
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
    lower(
        "native_frame_balance",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the source lowers")
}

fn run_lowered(lowered: &Lowered, source: &str) -> Vm {
    let bytes = write(lowered.module());
    let module = read_checked(&bytes, source.as_bytes()).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    vm
}

/// One uop's variant name, for structural assertions over placed shapes.
fn uop_kind(op: &PlacedOp) -> Option<String> {
    match op {
        PlacedOp::Uop(uop) => {
            let repr = format!("{uop:?}");
            let name = repr
                .chars()
                .take_while(char::is_ascii_alphabetic)
                .collect::<String>();
            Some(name)
        },
        PlacedOp::Rc(_) => None,
    }
}

/// Every non-root native-call shape stays count-balanced: the frame-tail
/// `concat` (a dying parameter's forwarded count plus the literal's birth
/// count), the early-return path's exit drops, the checked generic read,
/// and the lambda body's `concat` under `map` — the heap drains.
#[test]
fn native_calls_inside_fn_frames_balance_their_borrow_windows() {
    let source = format!("{PRELUDE}{BALANCE}");
    let lowered = lower_inline(&source);
    let mut vm = run_lowered(&lowered, &source);
    assert_eq!(
        vm.run("native_frame_balance", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the fn-frame native calls answer"
    );
    assert_eq!(vm.captured_output(), b"ab!\ncd!\nef?\nalpha!\n");
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// The placed shapes: a non-root frame-tail native call is CALL with the
/// borrow-window decs between the call and the result copy, then the RET
/// (never a CALLT, which would pop the frame inside the instruction and
/// strand the counts); the root frame's tail native call keeps the
/// CALLT transfer (the named corner — the program ends there, the heap
/// dies whole, and its staged word stays live by design).
#[test]
fn a_fn_frame_tail_native_call_lowers_in_place_but_main_keeps_the_transfer() {
    let source = format!("{PRELUDE}{TAIL_SHAPES}");
    let lowered = lower_inline(&source);
    let bang = lowered.placed("bang").expect("bang is planned");
    let kinds: Vec<String> = bang
        .iter()
        .map(|op| match op {
            PlacedOp::Rc(_) => "Rc".to_owned(),
            PlacedOp::Uop(_) => uop_kind(op).unwrap_or_default(),
        })
        .collect();
    assert!(
        !kinds.iter().any(|k| k == "Callt"),
        "a non-root tail native call transfers nothing: {kinds:?}"
    );
    let call_at = kinds
        .iter()
        .position(|k| k == "Call")
        .expect("the native call lowers in place");
    let ret_at = kinds
        .iter()
        .position(|k| k == "Ret")
        .expect("the frame returns");
    let between = &kinds[call_at + 1..ret_at];
    assert_eq!(
        between,
        ["Rc", "Mov"],
        "the forwarded parameter's borrow-window dec flushes right after the call, then the \
         result copies out; the literal is a count-free borrow load (KSTR_BORROW), so no \
         birth-count dec exists"
    );
    let rc_ops: Vec<RcOp> = bang
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Rc(rc) => Some(*rc),
            PlacedOp::Uop(_) => None,
        })
        .collect();
    assert_eq!(
        rc_ops,
        [RcOp::Dec { src: 0 }],
        "only the forwarded parameter count drops — the literal borrowed the intern map's copy"
    );
    assert!(
        bang.iter()
            .any(|op| matches!(op, PlacedOp::Uop(ds_abi::Uop::KstrBorrow { .. }))),
        "the count-free literal loads as KSTR_BORROW: {kinds:?}"
    );
    let main = lowered.placed("main").expect("main is planned");
    let main_kinds: Vec<String> = main.iter().filter_map(uop_kind).collect();
    assert_eq!(
        main_kinds.iter().filter(|k| *k == "Callt").count(),
        1,
        "the root frame's tail native call keeps the CALLT transfer: {main_kinds:?}"
    );
    // the corner after the temporary slice: the root CALLT still strands
    // what it stages, but the staged "done" is a count-free borrow load,
    // so nothing countable was ever handed to the transfer — the store
    // drains to zero (a staged ACQUIRING word would still strand live)
    let mut vm = run_lowered(&lowered, &source);
    assert_eq!(
        vm.run("native_frame_balance", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the tail shapes run"
    );
    assert_eq!(vm.captured_output(), b"ab!\ndone\n");
    assert_eq!(
        vm.objects().len(),
        0,
        "the staged borrow word had no count to abandon, so the store drains"
    );
}
