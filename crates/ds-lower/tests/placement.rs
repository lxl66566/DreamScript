//! The Perceus dup/drop placement contract: the placed op sequence —
//! uops plus RC ops at their exact walk positions — pins where dup and drop
//! go, and the runnable module body carries the same words (the 0xE pair
//! rides beside the uops and the VM executes it over counted heap blocks).
//! Every `run_main` also asserts the heap drains: a program returning a
//! scalar must have balanced every count it claimed.

use ds_abi::{RcOp, Uop};
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, PlacedOp, lower};
use ds_mod::{Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// Lowers one inline source through the full pipeline, asserting the
/// check is clean so failures are the lowerer's.
fn lower_inline(source: &str) -> Lowered {
    let mut files = Files::new();
    let id = files.add_source("placement.ds", source);
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
        "placement",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the source lowers")
}

/// Runs one lowered module's `main` on the tier-1 VM and asserts the
/// golden stdout — the placement pass must not change observable behavior.
fn run_main(lowered: &Lowered, source: &str, stdout: &str) -> FrameAnswer {
    let bytes = write(lowered.module());
    let module = read_checked(&bytes, source.as_bytes()).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    let result = vm.run("placement", "main", &[]).expect("main runs");
    assert_eq!(
        vm.captured_output(),
        stdout.as_bytes(),
        "observable behavior is unchanged by placement"
    );
    // the counts balance: every dup matches a drop, so a program whose
    // result owns no reference drains the heap
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
    result
}

/// The RC ops of one placed sequence, in order.
fn rc_ops(placed: &[PlacedOp]) -> Vec<RcOp> {
    placed
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Rc(rc) => Some(*rc),
            PlacedOp::Uop(_) => None,
        })
        .collect()
}

const ALIAS_ORDER_A: &str = "
tag int
tag string
tag bool
extern fn println (s: string) -> ()
extern fn concat (a: string, b: string) -> string
fn main() -> int {
  const s = concat(\"al\", \"pha\")
  const t = s
  println(s)
  println(t)
  0
}";

const ALIAS_ORDER_B: &str = "
tag int
tag string
tag bool
extern fn println (s: string) -> ()
extern fn concat (a: string, b: string) -> string
fn main() -> int {
  const s = concat(\"be\", \"ta\")
  const t = s
  println(t)
  println(s)
  0
}";

/// A string bound, aliased (`const t = s` names the same word — no copy,
/// no dup), and last-used in both orders: exactly one drop at the final
/// use, zero dups, whichever alias dies last. The one reference is a
/// counted STRCAT result (stage three's temporary slice made the KSTR
/// literals count-free borrow loads, so a literal-bound alias would have
/// no drop at all); the operand literals load as KSTR_BORROW.
#[test]
fn aliasing_places_one_drop_at_the_last_use_in_both_orders() {
    for (source, stdout) in [
        (ALIAS_ORDER_A, "alpha\nalpha\n"),
        (ALIAS_ORDER_B, "beta\nbeta\n"),
    ] {
        let lowered = lower_inline(source);
        let placed = lowered.placed("main").expect("main is planned");
        let ops = rc_ops(placed);
        assert!(
            ops.iter().all(|op| matches!(op, RcOp::Dec { .. })),
            "aliasing never dups: {ops:?}"
        );
        assert_eq!(ops.len(), 1, "exactly one drop for the one reference");
        assert!(
            placed
                .iter()
                .any(|op| matches!(op, PlacedOp::Uop(Uop::KstrBorrow { .. }))),
            "the operand literals load as count-free borrows"
        );
        // the drop lands right after the last println call returned
        let drop_at = placed
            .iter()
            .position(|op| matches!(op, PlacedOp::Rc(RcOp::Dec { .. })))
            .expect("the drop is placed");
        let before = &placed[drop_at - 1];
        assert!(
            matches!(before, PlacedOp::Uop(Uop::Call { .. })),
            "the drop follows the last use's call: {before:?}"
        );
        assert_eq!(
            run_main(&lowered, source, stdout),
            FrameAnswer::one(Value::I64(0))
        );
    }
}

const CALL_SOURCE: &str = "
tag int
tag string
tag bool
extern fn println (s: string) -> ()
fn keep(x: string) -> int {
  println(x)
  0
}
fn main() -> int {
  const s = \"shared\"
  const a = keep(s)
  println(s)
  keep(\"done\")
}";

/// A shared reference passed to a borrow callee: the caller dups at the
/// call (the source stays live), the callee drops the handed reference at
/// its own unique last use, and a dying argument forwards without a dup.
#[test]
fn a_shared_call_argument_dups_at_the_call_and_the_callee_drops_at_its_last_use() {
    let lowered = lower_inline(CALL_SOURCE);
    let main = rc_ops(lowered.placed("main").expect("main is planned"));
    let [inc, dec] = main.as_slice() else {
        panic!("main places one dup and one drop: {main:?}");
    };
    // the dup is the staged argument of the live `keep(s)` call; the drop
    // is s's own death at println
    assert!(matches!(inc, RcOp::Inc { .. }), "the dup: {inc:?}");
    assert!(matches!(dec, RcOp::Dec { .. }), "the drop: {dec:?}");

    let keep = rc_ops(lowered.placed("keep").expect("keep is planned"));
    // the callee's parameter dies at its unique last use (the println
    // inside keep) — the handed count drops there, nowhere else
    assert_eq!(keep.len(), 1, "one drop at the callee's last use: {keep:?}");
    assert!(matches!(keep[0], RcOp::Dec { .. }));
    let placed = lowered.placed("keep").expect("keep is planned");
    let drop_at = placed
        .iter()
        .position(|op| matches!(op, PlacedOp::Rc(RcOp::Dec { .. })))
        .expect("the callee drop is placed");
    assert!(
        matches!(placed[drop_at - 1], PlacedOp::Uop(Uop::Call { .. })),
        "the callee drop follows its last-use call"
    );

    // the dying literal `keep(\"done\")` forwards: no extra dup for it —
    // main's totals stay exactly one inc and one dec
    assert_eq!(
        run_main(&lowered, CALL_SOURCE, "shared\nshared\ndone\n"),
        FrameAnswer::one(Value::I64(0))
    );
}

const WILD_DISCARD_SOURCE: &str = "
tag int
tag string
tag bool
extern fn println (s: string) -> ()
extern fn concat (a: string, b: string) -> string
fn main() -> int {
  const s = concat(\"ke\", \"pt\")
  const _ = concat(\"dro\", \"pped\")
  const _ = s
  println(s)
  0
}";

/// `const _ = e` discards the initializer: the wildcard binds no name,
/// so a fresh reference dies at the statement (its dec sits right there)
/// and an aliased one keeps its binder's death untouched. The heap
/// drains through both. The discarded and kept references are counted
/// STRCAT results — a bare KSTR literal would be a count-free borrow
/// load with no dec to place.
#[test]
fn a_wildcard_const_binding_discards_and_keeps_alias_deaths() {
    let lowered = lower_inline(WILD_DISCARD_SOURCE);
    let placed = lowered.placed("main").expect("main is planned");
    let ops = rc_ops(placed);
    // two decs: the discarded literal at its statement, s at its final
    // use (the println read); the `const _ = s` alias adds none
    assert_eq!(
        ops.len(),
        2,
        "the wildcard discard owns exactly two decs: {ops:?}"
    );
    assert!(ops.iter().all(|op| matches!(op, RcOp::Dec { .. })));
    // the discard dec precedes the println call (it follows the
    // discarded concat call it belongs to), the kept dec follows the
    // println call
    let drop_at = placed
        .iter()
        .position(|op| matches!(op, PlacedOp::Rc(RcOp::Dec { .. })))
        .expect("the discard dec is placed");
    let call_at = placed
        .iter()
        .rposition(|op| matches!(op, PlacedOp::Uop(Uop::Call { .. })))
        .expect("the println call is placed");
    assert!(
        drop_at < call_at,
        "the discarded value dies at its statement, before the use"
    );
    assert_eq!(
        run_main(
            &lowered,
            WILD_DISCARD_SOURCE,
            "kept
"
        ),
        FrameAnswer::one(Value::I64(0))
    );
}

const REUSE_SOURCE: &str = "
tag int
tag string
tag bool
extern fn concat (a: string, b: string) -> string
fn main() -> bool {
  const t = \"y\"
  const a: string = concat(\"x\", t)
  const u = a == \"z\"
  const b: string = concat(\"fr\", t)
  u
}";

/// A last-use drop immediately preceding an allocation of a compatible
/// shape: the placed sequence is a plain `Dec` directly before the
/// allocating concat — the reuse pairing itself is implicit (a unique
/// death frees the heap block and the next allocation claims the freed
/// slot), so no alloc-at uop exists in this ISA. The counted producers
/// are native concat results: a native's return hands a count the caller
/// must drop, and the STRCAT temporaries of stage three's arena slice
/// carry none, so the dec-to-alloc pairing is pinned against the native
/// allocation across the count-free literal loads.
#[test]
fn a_last_use_drop_sits_immediately_before_the_next_string_allocation() {
    let lowered = lower_inline(REUSE_SOURCE);
    let placed = lowered.placed("main").expect("main is planned");
    // the fresh allocation is the sequence's last native concat
    let alloc_at = placed
        .iter()
        .rposition(|op| matches!(op, PlacedOp::Uop(Uop::Call { .. })))
        .expect("the fresh concat allocates");
    // the dying binder's drop feeds the allocation directly: the dec,
    // then exactly the count-free literal load, then the STRCAT — the
    // borrow load allocates nothing, so the freed block is still the
    // one this allocation claims
    assert!(
        matches!(placed[alloc_at - 4], PlacedOp::Rc(RcOp::Dec { .. }))
            && matches!(placed[alloc_at - 3], PlacedOp::Uop(Uop::KstrBorrow { .. })),
        "a drop feeds the allocation across the zero-cost load and the staging moves: {:?} {:?} \
         {:?} {:?} {:?}",
        placed[alloc_at - 4],
        placed[alloc_at - 3],
        placed[alloc_at - 2],
        placed[alloc_at - 1],
        placed[alloc_at]
    );
    // the dead binder drops at the comparison (the operand literals are
    // count-free borrows), the fresh binder at its scope end — plain
    // decs, no reuse-specific op
    assert_eq!(rc_ops(placed).len(), 2);
    assert!(
        placed
            .iter()
            .any(|op| matches!(op, PlacedOp::Uop(Uop::KstrBorrow { .. })))
            && !placed
                .iter()
                .any(|op| matches!(op, PlacedOp::Uop(Uop::Kstr { .. }))),
        "every literal loads as a count-free borrow"
    );
    assert_eq!(
        run_main(&lowered, REUSE_SOURCE, ""),
        FrameAnswer::one(Value::Bool(false))
    );
}

/// Determinism: the same input lowers to the identical placed sequence,
/// run after run (every emission iterates ordered collections).
#[test]
fn the_placed_sequence_is_identical_across_runs() {
    for source in [ALIAS_ORDER_A, CALL_SOURCE, REUSE_SOURCE] {
        let first = lower_inline(source);
        let second = lower_inline(source);
        for name in ["main", "keep"] {
            assert_eq!(
                first.placed(name).map(<[PlacedOp]>::to_vec),
                second.placed(name).map(<[PlacedOp]>::to_vec),
                "{name}'s placed sequence is deterministic"
            );
        }
    }
}

/// The record's body decoded to instructions, one per pc.
fn insts(words: &[u32]) -> Vec<ds_abi::Inst> {
    words
        .iter()
        .enumerate()
        .map(|(pc, word)| {
            ds_abi::Inst::decode(u32::try_from(pc).expect("pc fits u32"), *word)
                .expect("ds-mod validated every word")
        })
        .collect()
}

/// The module body is the placed sequence verbatim, RC ops included: the
/// 0xE words ride beside the uops in the same instruction space, so the
/// executable stream and the placement contract cannot disagree.
#[test]
fn the_module_body_carries_the_rc_ops() {
    let lowered = lower_inline(CALL_SOURCE);
    let placed = lowered.placed("keep").expect("keep is planned");
    let module = lowered.module();
    let index = lowered.function_index("keep").expect("planned") as usize;
    let body = insts(module.functions()[index].code());
    let expected: Vec<ds_abi::Inst> = placed
        .iter()
        .map(|op| match *op {
            PlacedOp::Uop(uop) => ds_abi::Inst::Uop(uop),
            PlacedOp::Rc(rc) => ds_abi::Inst::Rc(rc),
        })
        .collect();
    assert_eq!(
        body, expected,
        "the module's code is the placed sequence, RC ops included"
    );
    assert!(
        placed.iter().any(|op| matches!(op, PlacedOp::Rc(_))),
        "the placed sequence itself carries the RC ops"
    );
}

const ZERO_RC_SOURCE: &str = "
tag int
tag string
tag bool
extern fn println (s: string) -> ()
fn main() -> int {
  if \"a\" == \"b\" {
    println(\"no\")
  }
  println(\"done\")
  0
}";

/// Stage three's temporary slice, the zero-RcOps pin: a frame whose
/// every KSTR word is a designated temporary (read, staged-native, and
/// discard deaths; no dup, no forward, no exit transfer, no loop carry)
/// carries zero RC ops — every literal loads as KSTR_BORROW and every
/// terminal dec is gone.
#[test]
fn designated_temporaries_carry_zero_rc_ops() {
    let lowered = lower_inline(ZERO_RC_SOURCE);
    let placed = lowered.placed("main").expect("main is planned");
    assert!(
        rc_ops(placed).is_empty(),
        "a temporary-only frame pays no count traffic: {placed:?}"
    );
    assert!(
        placed
            .iter()
            .filter(|op| { matches!(op, PlacedOp::Uop(Uop::Kstr { .. } | Uop::KstrBorrow { .. })) })
            .count()
            > 0
            && placed
                .iter()
                .all(|op| !matches!(op, PlacedOp::Uop(Uop::Kstr { .. }))),
        "every literal in the frame is a borrow load: {placed:?}"
    );
    assert_eq!(
        run_main(&lowered, ZERO_RC_SOURCE, "done\n"),
        FrameAnswer::one(Value::I64(0))
    );
}

const ESCAPE_SOURCE: &str = "
tag int
tag string
tag bool
extern fn println (s: string) -> ()
fn hand_back() -> string {
  \"stored\"
}
fn carried() -> int {
  const s = \"held\"
  var i: int = 0
  while i < 3 {
    i += 1
  }
  0
}
fn main() -> int {
  println(hand_back())
  println(hand_back())
  carried()
}";

/// The exclusion pin: a literal the proof cannot classify keeps its
/// count. An exit transfer (the returned literal hands its count to the
/// caller) and a loop-carried word (produced before the loop entry)
/// both stay acquiring KSTR loads — the borrow rewrite is a proof, not
/// a default.
#[test]
fn escaping_and_carried_literals_keep_their_counts() {
    let lowered = lower_inline(ESCAPE_SOURCE);
    for (name, want_kstr, want_borrow) in [("hand_back", 1, 0), ("carried", 1, 0)] {
        let placed = lowered.placed(name).expect("the frame is planned");
        let kstr = placed
            .iter()
            .filter(|op| matches!(op, PlacedOp::Uop(Uop::Kstr { .. })))
            .count();
        let borrow = placed
            .iter()
            .filter(|op| matches!(op, PlacedOp::Uop(Uop::KstrBorrow { .. })))
            .count();
        assert_eq!(
            (kstr, borrow),
            (want_kstr, want_borrow),
            "{name} keeps its acquiring load: {placed:?}"
        );
    }
    // hand_back returns its literal's count to the caller, so main pays
    // the handed dec twice; carried's loop-carried word decs at its own
    // frame exit — the counts still balance and the run drains
    let main = lowered.placed("main").expect("main is planned");
    assert_eq!(
        rc_ops(main).len(),
        2,
        "the two handed-back counts dec at their uses: {main:?}"
    );
    assert_eq!(
        run_main(&lowered, ESCAPE_SOURCE, "stored\nstored\n"),
        FrameAnswer::one(Value::I64(0))
    );
}

const LOOP_STRCAT_SOURCE: &str = "
tag int
tag string
tag bool
extern fn println (s: string) -> ()
fn main() -> int {
  var i: int = 0
  while i < 3 {
    const hit = \"x${i}y\" == \"x1y\"
    println(\"<${i}>\")
    if hit { println(\"hit\") }
    i += 1
  }
  0
}";

/// Stage three's temporary slice, the STRCAT side: a loop whose body
/// builds an interpolation chain, compares it, and drops it before the
/// back edge — every link of the chain takes the arena spelling with
/// one mark/restore pair for the loop, and the chain's terminal decs
/// leave the stream — while the interpolation handed to println keeps
/// its counted spelling (a native can retain its argument, so the proof
/// refuses to classify it).
#[test]
fn designated_loop_concats_serve_from_the_arena() {
    let lowered = lower_inline(LOOP_STRCAT_SOURCE);
    let placed = lowered.placed("main").expect("main is planned");
    let served: Vec<_> = placed
        .iter()
        .filter(|op| matches!(op, PlacedOp::Uop(Uop::StrCatArena { .. })))
        .collect();
    assert_eq!(
        served.len(),
        3,
        "both comparison-chain links and the println chain's inner link serve from the arena: \
         {placed:?}"
    );
    let dsts = served
        .iter()
        .map(|op| match op {
            PlacedOp::Uop(Uop::StrCatArena { dst, .. }) => *dst,
            _ => unreachable!("the filter above matched only arena servings"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        placed
            .iter()
            .filter(|op| matches!(op, PlacedOp::Uop(Uop::ArenaMark { .. })))
            .count(),
        1,
        "the marked loop gains exactly one head mark: {placed:?}"
    );
    assert_eq!(
        placed
            .iter()
            .filter(|op| matches!(op, PlacedOp::Uop(Uop::ArenaRestore { .. })))
            .count(),
        1,
        "the marked loop gains exactly one back-edge restore: {placed:?}"
    );
    assert!(
        placed.iter().all(|op| match op {
            PlacedOp::Rc(RcOp::Dec { src }) => !dsts.contains(src),
            _ => true,
        }),
        "the arena temps' counts leave the stream: {placed:?}"
    );
    let staged = placed
        .iter()
        .filter(|op| matches!(op, PlacedOp::Uop(Uop::StrCat { .. })))
        .count();
    assert_eq!(
        staged, 1,
        "the println chain's terminal link keeps its counted spelling: {placed:?}"
    );
    assert_eq!(
        run_main(&lowered, LOOP_STRCAT_SOURCE, "<0>\n<1>\nhit\n<2>\n"),
        FrameAnswer::one(Value::I64(0))
    );
}

const CARRY_STRCAT_SOURCE: &str = "
tag int
tag string
tag bool
extern fn println (s: string) -> ()
fn main() -> int {
  var piece: string = \"a\"
  var i: int = 0
  while i < 4 {
    piece = \"${piece}${piece}\"
    i += 1
  }
  println(piece)
  0
}";

/// The exclusion pin: a concatenation whose word crosses the back edge
/// alive (the doubling piece survives every iteration) keeps its
/// counted spelling — no arena serving, no mark pair, and the counts
/// still balance at run end.
#[test]
fn a_back_edge_carried_concat_keeps_its_count() {
    let lowered = lower_inline(CARRY_STRCAT_SOURCE);
    let placed = lowered.placed("main").expect("main is planned");
    assert!(
        placed.iter().all(|op| {
            !matches!(
                op,
                PlacedOp::Uop(
                    Uop::StrCatArena { .. } | Uop::ArenaMark { .. } | Uop::ArenaRestore { .. }
                )
            )
        }),
        "the carried word keeps the counted spelling: {placed:?}"
    );
    assert_eq!(
        run_main(&lowered, CARRY_STRCAT_SOURCE, "aaaaaaaaaaaaaaaa\n"),
        FrameAnswer::one(Value::I64(0))
    );
}

const CANONICAL_UNIT_SOURCE: &str = "
tag int
fn main() -> int {
  var x: int = 0
  var i: int = 0
  while i < 3 {
    if x % 2 == 0 {
      x += 1
    } else {
      x += 2
    }
    i += 1
  }
  x
}";

/// The canonical unit word: one Kimm 0 at the frame top answers every
/// unit value in the frame — the loop tail, both statement-if arms, and
/// both compound assignments share it — and nothing ever writes the word
/// afterwards, so the unit join carries no result span and no per-arm
/// Mov, while the run still answers through it.
#[test]
fn one_entry_zero_serves_every_unit_value_in_the_frame() {
    let lowered = lower_inline(CANONICAL_UNIT_SOURCE);
    let placed = lowered.placed("main").expect("main is planned");
    let index = lowered.function_index("main").expect("main is planned");
    let func = &lowered.module().functions()[index as usize];
    // placed[0] is FUNCF, so the first uop is the entry zero
    let PlacedOp::Uop(Uop::Kimm { dst: unit, kidx }) = placed[1] else {
        panic!(
            "the frame opens with the canonical unit zero: {:?}",
            placed[1]
        );
    };
    assert!(
        matches!(
            func.constants()[usize::from(kidx.0)],
            ds_mod::Constant::I64(0)
        ),
        "the entry zero writes the 0 constant"
    );
    assert!(
        placed.iter().skip(2).all(|op| {
            !matches!(
                op,
                PlacedOp::Uop(
                    Uop::Kimm {
                        dst: word,
                        ..
                    }
                    | Uop::Kconst {
                        dst: word,
                        ..
                    }
                    | Uop::Mov {
                        dst: word,
                        ..
                    }
                ) if *word == unit
            )
        }),
        "no later op writes the canonical unit word: {placed:?}"
    );
    assert_eq!(
        run_main(&lowered, CANONICAL_UNIT_SOURCE, ""),
        FrameAnswer::one(Value::I64(5))
    );
}

const DIRECT_SCALAR_STORE_SOURCE: &str = "
tag int
fn main() -> int {
  var x: int = 5
  x += 3
  x = 7
  x
}";

/// The direct scalar store: a scalar var's literal init, its compound
/// assignment, and a literal assignment all write the var's own word —
/// no compute-into-temp, no re-store Mov — and the compound AddI64
/// safely aliases its destination with its own lhs read.
#[test]
fn scalar_stores_write_the_var_word_directly() {
    let lowered = lower_inline(DIRECT_SCALAR_STORE_SOURCE);
    let placed = lowered.placed("main").expect("main is planned");
    assert!(
        placed
            .iter()
            .all(|op| !matches!(op, PlacedOp::Uop(Uop::Mov { .. }))),
        "no re-store Mov rides the scalar stores: {placed:?}"
    );
    let adds: Vec<u8> = placed
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Uop(Uop::AddI64 { dst, lhs, .. }) if dst == lhs => Some(*dst),
            _ => None,
        })
        .collect();
    assert_eq!(
        adds.len(),
        1,
        "one compound store over its own word: {placed:?}"
    );
    let var_word = adds[0];
    let loads: Vec<u8> = placed
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Uop(Uop::Kimm { dst, .. }) if *dst == var_word => Some(*dst),
            _ => None,
        })
        .collect();
    assert_eq!(
        loads.len(),
        2,
        "the init and the reassignment load their constants into the var word: {placed:?}"
    );
    assert_eq!(
        run_main(&lowered, DIRECT_SCALAR_STORE_SOURCE, ""),
        FrameAnswer::one(Value::I64(7))
    );
}

const DIRECT_COMPUTED_STORE_SOURCE: &str = "
tag int
fn main() -> int {
  var x: int = 8
  x = x % 3
  x
}";

/// The plain assignment of a computed scalar: the right-hand side's
/// outermost data op writes the var word itself, aliasing its own
/// operand read (the VM reads both operands before writing dst), and no
/// re-store Mov rides the store.
#[test]
fn computed_scalar_stores_write_the_var_word_directly() {
    let lowered = lower_inline(DIRECT_COMPUTED_STORE_SOURCE);
    let placed = lowered.placed("main").expect("main is planned");
    assert!(
        placed
            .iter()
            .all(|op| !matches!(op, PlacedOp::Uop(Uop::Mov { .. }))),
        "no re-store Mov rides the computed store: {placed:?}"
    );
    let mods: Vec<u8> = placed
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Uop(Uop::ModI64 { dst, lhs, .. }) if dst == lhs => Some(*dst),
            _ => None,
        })
        .collect();
    assert_eq!(
        mods.len(),
        1,
        "the outermost mod writes the var word over its own read: {placed:?}"
    );
    assert_eq!(
        run_main(&lowered, DIRECT_COMPUTED_STORE_SOURCE, ""),
        FrameAnswer::one(Value::I64(2))
    );
}

const DIRECT_ARG_STAGING_SOURCE: &str = "
tag int
fn add(a: int, b: int) -> int {
  a + b
}

fn main() -> int {
  var x: int = 4
  var y: int = add(2, x * 3 + 1)
  y
}";

/// The overlay word count below `main`'s body span, for telling the
/// staging words a Mov or data op addresses apart from body words.
fn body_len(lowered: &Lowered, name: &str) -> usize {
    let index = lowered
        .function_index(name)
        .expect("the function is planned");
    lowered.module().functions()[usize::try_from(index).expect("an index fits usize")]
        .layout()
        .body_len()
}

/// The direct argument staging: a literal scalar argument's constant load
/// writes its overlay word at the staging position, a single-use
/// computed scalar argument's outermost data op writes its overlay word,
/// and no staging Mov rides the call (the one Mov left copies the
/// result span out of the overlay).
#[test]
fn scalar_arguments_write_their_overlay_words_directly() {
    let lowered = lower_inline(DIRECT_ARG_STAGING_SOURCE);
    let placed = lowered.placed("main").expect("main is planned");
    let body = body_len(&lowered, "main");
    let staging_movs: Vec<Uop> = placed
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Uop(m @ Uop::Mov { dst, .. }) if usize::from(*dst) >= body => Some(*m),
            _ => None,
        })
        .collect();
    assert!(
        staging_movs.is_empty(),
        "no staging Mov rides the direct arguments: {placed:?}"
    );
    let const_words: Vec<u8> = placed
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Uop(Uop::Kimm { dst, .. }) if usize::from(*dst) >= body => Some(*dst),
            _ => None,
        })
        .collect();
    assert_eq!(
        const_words,
        vec![u8::try_from(body).expect("the first overlay word fits u8")],
        "the literal argument's load writes the first overlay word: {placed:?}"
    );
    let data_words: Vec<u8> = placed
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Uop(Uop::AddI64 { dst, .. }) if usize::from(*dst) >= body => Some(*dst),
            _ => None,
        })
        .collect();
    assert_eq!(
        data_words,
        vec![u8::try_from(body + 1).expect("the second overlay word fits u8")],
        "the computed argument's outermost op writes the second overlay word: {placed:?}"
    );
    assert_eq!(
        run_main(&lowered, DIRECT_ARG_STAGING_SOURCE, ""),
        FrameAnswer::one(Value::I64(15))
    );
}

const DEFERRED_ARG_STAGING_SOURCE: &str = "
tag int
fn add(a: int, b: int) -> int {
  a + b
}

fn main() -> int {
  var x: int = 4
  var y: int = add(x % 3, x % 5)
  y
}";

/// An argument whose later sibling also computes keeps its staging Mov:
/// a later argument's evaluation may stage over an already written
/// overlay word, so only the last argument of a call may write its
/// overlay word directly.
#[test]
fn a_later_computing_argument_keeps_the_earlier_staging_mov() {
    let lowered = lower_inline(DEFERRED_ARG_STAGING_SOURCE);
    let placed = lowered.placed("main").expect("main is planned");
    let body = body_len(&lowered, "main");
    let staging_movs: Vec<Uop> = placed
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Uop(m @ Uop::Mov { dst, .. }) if usize::from(*dst) >= body => Some(*m),
            _ => None,
        })
        .collect();
    assert_eq!(
        staging_movs.len(),
        1,
        "the first argument keeps its staging Mov: {placed:?}"
    );
    assert_eq!(
        run_main(&lowered, DEFERRED_ARG_STAGING_SOURCE, ""),
        FrameAnswer::one(Value::I64(5))
    );
}

const TAIL_ARG_STAGING_SOURCE: &str = "
tag int
fn id(v: int) -> int {
  v
}

fn main() -> int {
  var x: int = 4
  id(x % 7)
}";

/// A tail transfer keeps the evaluated temp + staging Mov: the transfer
/// stages its arguments past the exit claims, which stage through the
/// overlay over them, so a directly written word would be clobbered.
#[test]
fn a_tail_transfer_argument_keeps_its_staging_mov() {
    let lowered = lower_inline(TAIL_ARG_STAGING_SOURCE);
    let placed = lowered.placed("main").expect("main is planned");
    let body = body_len(&lowered, "main");
    let staging_movs: Vec<Uop> = placed
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Uop(m @ Uop::Mov { dst, .. }) if usize::from(*dst) >= body => Some(*m),
            _ => None,
        })
        .collect();
    assert_eq!(
        staging_movs.len(),
        1,
        "the tail argument keeps its staging Mov: {placed:?}"
    );
    assert_eq!(
        run_main(&lowered, TAIL_ARG_STAGING_SOURCE, ""),
        FrameAnswer::one(Value::I64(4))
    );
}

const DIRECT_CALLS_STAGING_SOURCE: &str = "
tag int
fn add(a: int, b: int) -> int {
  a + b
}

fn main() -> int {
  const f = add
  var x: int = 4
  var y: int = f(x % 5, 2)
  y
}";

/// The callable-value call path stages its scalar arguments the same
/// way: the fn-typed binding's CALLS reads the directly written overlay
/// words with no staging Mov.
#[test]
fn callable_value_arguments_write_their_overlay_words_directly() {
    let lowered = lower_inline(DIRECT_CALLS_STAGING_SOURCE);
    let placed = lowered.placed("main").expect("main is planned");
    let body = body_len(&lowered, "main");
    assert!(
        placed.iter().all(|op| match op {
            PlacedOp::Uop(Uop::Mov { dst, .. }) => usize::from(*dst) < body,
            _ => true,
        }),
        "no staging Mov rides the direct arguments: {placed:?}"
    );
    assert_eq!(
        run_main(&lowered, DIRECT_CALLS_STAGING_SOURCE, ""),
        FrameAnswer::one(Value::I64(6))
    );
}

const BLOCK_TAIL_CLAIM_SOURCE: &str = "
tag int
tag string
tag bool
extern fn println (s: string) -> ()
extern fn concat (a: string, b: string) -> string
fn main() -> int {
  const ys = {
    var xs = concat(\"al\", \"pha\")
    xs
  }
  println(ys)
  0
}";

/// A value block's tail read of a `var` binder stages the answer's claim
/// past the block's scope end: the scope once dec'd the slot's count
/// before the consumer claimed it, so the consumer read a freed word.
/// The count now releases at the consumer's own death site — the one
/// drop lands after the consuming call, exactly one, never before it.
#[test]
fn a_value_blocks_tail_read_releases_at_the_consumer_not_the_scope_end() {
    let lowered = lower_inline(BLOCK_TAIL_CLAIM_SOURCE);
    let placed = lowered.placed("main").expect("main is planned");
    let ops = rc_ops(placed);
    assert!(
        ops.iter().all(|op| matches!(op, RcOp::Dec { .. })),
        "the staged tail read never dups: {ops:?}"
    );
    assert_eq!(ops.len(), 1, "exactly one release for the one reference");
    let drop_at = placed
        .iter()
        .position(|op| matches!(op, PlacedOp::Rc(RcOp::Dec { .. })))
        .expect("the release is placed");
    let before = &placed[drop_at - 1];
    assert!(
        matches!(before, PlacedOp::Uop(Uop::Call { .. })),
        "the release follows the consumer's call: {before:?}"
    );
    assert_eq!(
        run_main(&lowered, BLOCK_TAIL_CLAIM_SOURCE, "alpha\n"),
        FrameAnswer::one(Value::I64(0))
    );
}

const CAPTURED_ROW_SOURCE: &str = "
tag int
tag string
tag bool

extern fn println (s: string) -> ()
extern fn concat (a: string, b: string) -> string
fn make_suffix (s: string) -> fn(string) -> string {
  (w: string) => concat(w, s)
}

fn main() -> int {
  const f = make_suffix(\"tail\")
  println(f(\"head\"))
  0
}";

/// A frame reads the affine rows across function boundaries: the
/// synthesized lambda registers its captured enclosing binding (a
/// binder whose span lives in another function's text), so the
/// capture's own last-use row still drives the lambda frame's
/// placement — the parameter and the captured count both dec inside the
/// frame at the concat use, while the enclosing frame's stream carries
/// only its own count traffic. A per-function row slice keyed by each
/// frame's own text would drop the capture's row and strand its count;
/// this pin holds the shared facts table to the whole-table behavior.
#[test]
fn a_lambda_frame_places_its_captured_binder_row() {
    let lowered = lower_inline(CAPTURED_ROW_SOURCE);
    let lambda = lowered
        .functions()
        .iter()
        .find(|name| name.starts_with("make_suffix$"))
        .expect("the synthesized lambda lands in the function table");
    let placed = lowered
        .placed(lambda)
        .expect("the lambda's placed stream exists");
    let ops = rc_ops(placed);
    assert_eq!(
        ops.len(),
        2,
        "the parameter and the capture dec once each: {ops:?}"
    );
    assert!(
        ops.iter().all(|op| matches!(op, RcOp::Dec { .. })),
        "both counts die inside the frame: {ops:?}"
    );
    let first = placed
        .iter()
        .position(|op| matches!(op, PlacedOp::Rc(RcOp::Dec { .. })))
        .expect("the first dec is placed");
    let before = &placed[first - 1];
    assert!(
        matches!(before, PlacedOp::Uop(Uop::Call { .. })),
        "both decs follow the concat use: {before:?}"
    );

    let maker = rc_ops(
        lowered
            .placed("make_suffix")
            .expect("make_suffix is planned"),
    );
    assert_eq!(
        maker.len(),
        1,
        "the enclosing frame keeps only its own dec: {maker:?}"
    );
    assert!(matches!(maker[0], RcOp::Dec { .. }));

    assert_eq!(
        run_main(&lowered, CAPTURED_ROW_SOURCE, "headtail\n"),
        FrameAnswer::one(Value::I64(0)),
        "observable behavior is unchanged and the heap drains"
    );
}
