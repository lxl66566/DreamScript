//! Allocation-count pins for the claim ledger's two foreign-row shapes
//! (the ledger-shape decision's evidence): a counting global allocator
//! measures whole fork-batch windows, the paired baseline subtracts
//! everything the capture does not cause, and the minimum over repeats is
//! the window's exact count — the harness's own reporting traffic only
//! ever adds. The counter is process-global, so every test here holds
//! one lock for its whole body: nextest isolates each test in its own
//! process, but a plain `cargo test` run shares one, and two windows
//! measuring at once read each other's resets.
//!
//! The two pins:
//! - the nursery shape — one foreign capture per child of a 64-element batch, one row per child
//!   ledger: the capture batch pays a measured 129 allocations over the bare batch (64 child ledger
//!   tables, 64 capture-block copies at spawn, the closure's own capture block), a cost that
//!   survives unchanged because the row's allocation is not a time cost (a scratch that removed it
//!   entirely left the cycle inside the timing noise floor, which is why the small-map hybrid is
//!   refused).
//! - the delivered shape — one joiner's ledger holding one foreign row per child-owned `Ref` answer
//!   of a 64-answer batch: many rows in one table, amortized; the pin reads: a steady-state batch's
//!   allocation count is stable run over run.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use ds_vm::{
    Const, ConstFuncRef, FunctionDef, ModuleRegistry, ModuleVersion, Value, Vm,
    abi::{
        BuiltinInstance, ConstIdx, FrameLayout, Inst, RcOp, SlotType,
        SlotType::{I64, Raw, Ref},
        uop::Uop,
    },
    assemble,
    native::{BUILTIN_LIST_NEW, BUILTIN_LIST_PUSH, BUILTIN_PAR_MAP},
};

/// The elements every fork batch spans: one workload size for both shapes.
const CHILDREN: usize = 64;

/// Serializes this file's tests: the counting allocator and its counter
/// are process-global, so two concurrent windows race each other's
/// zeroing (a racing window can read another test's reset as its own
/// count).
static SERIAL: Mutex<()> = Mutex::new(());

static ALLOCS: AtomicUsize = AtomicUsize::new(0);

/// A pass-through counting allocator: every entry point the system
/// allocator exposes counts one allocation; deallocation stays free of
/// bookkeeping (the pins read allocation counts, not bytes).
struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Runs `window` with the allocation counter zeroed and answers its
/// allocation count. Everything the VM allocates before the window (the
/// registry, the pool) stays outside it.
fn count_allocs<R>(mut window: impl FnMut() -> R) -> usize {
    ALLOCS.store(0, Ordering::Relaxed);
    let _ = window();
    ALLOCS.load(Ordering::Relaxed)
}

/// The window's own allocation count: the minimum over `reps` repeats.
/// The test harness's own thread allocates concurrently with the window
/// (its channel and reporting traffic), so a single sample reads high and
/// varies run to run — the noise is only ever additive, never missing, so
/// the minimum is the window's exact count.
fn exact_allocs<R>(mut window: impl FnMut() -> R, reps: usize) -> usize {
    let counts: Vec<usize> = (0..reps).map(|_| count_allocs(&mut window)).collect();
    println!("rep counts: {counts:?}");
    counts.into_iter().min().expect("at least one repeat")
}

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn builtin(index: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: "builtin".into(),
        func: index,
    })
}

fn def(
    name: &str,
    slots: &[SlotType],
    params: usize,
    args: usize,
    pool: Vec<Const>,
    body: &[Inst],
) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout"),
        pool,
        code: assemble(body).expect("fixture body encodes"),
    }
}

/// `(x: i64) -> i64`: answers its argument — the bare fork body.
fn plain_def() -> FunctionDef {
    def("plain", &[I64], 1, 0, vec![], &[
        Uop::Funcf { frame_slots: 1 }.into(),
        Uop::Ret { src: 0 }.into(),
    ])
}

/// `(x: i64) -> i64` with one foreign capture at slot 1 (unread by the
/// body, exit-dec'd before the return — the lowered drop shape): every
/// child spawned from a closure over this body holds one foreign capture
/// for its whole life.
fn capture_child_def() -> FunctionDef {
    def(
        "capture_child",
        &[I64, Ref, I64],
        1,
        0,
        vec![Const::I64(2)],
        &[
            Uop::Funcf { frame_slots: 3 }.into(),
            Uop::Kimm { dst: 2, kidx: k(0) }.into(),
            Uop::MulI64 {
                dst: 2,
                lhs: 0,
                rhs: 2,
            }
            .into(),
            Inst::Rc(RcOp::Dec { src: 1 }),
            Uop::Ret { src: 2 }.into(),
        ],
    )
}

/// `(x: i64) -> Ref`: the fork body minting a fresh string on the child's
/// own store — the answer is a child-owned word, so the joiner's ledger
/// holds a foreign row per delivered answer.
fn mint_def() -> FunctionDef {
    def("mint", &[I64, Ref], 1, 0, vec![Const::Str("e".into())], &[
        Uop::Funcf { frame_slots: 2 }.into(),
        Uop::Kstr { dst: 1, kidx: k(0) }.into(),
        Uop::Ret { src: 1 }.into(),
    ])
}

/// One fork batch over `CHILDREN` i64 elements through `callable` (a
/// function-table index staged as the closure header). `capture` picks the
/// closure's shape: `None` builds a bare closure (no capture block),
/// `Some(slot)` stages the root-owned string at `slot` as the one capture
/// word. Slots: 0 the result list, 1 the answer, 3 the closure header, 4
/// the shared string word, 5 the closure word; 6.. the overlay.
fn fork_main_def(name: &str, callable: u32, capture: bool) -> FunctionDef {
    let mut pool = vec![
        builtin(BUILTIN_LIST_NEW),  // k0
        builtin(BUILTIN_LIST_PUSH), // k1
        builtin(BUILTIN_PAR_MAP),   // k2
        Const::FuncRef(ConstFuncRef::Named {
            module: "lb".into(),
            func: callable,
        }), // k3 the closure header
        Const::I64(BuiltinInstance::List as i64), // k4 the list evidence
        Const::Str("shared".into()), // k5
    ];
    let elem = |pool: &mut Vec<Const>, word: i64| {
        pool.push(Const::I64(word));
        k(u16::try_from(pool.len() - 1).expect("pool index fits u16"))
    };
    let mut body = vec![Uop::Funcf { frame_slots: 10 }.into()];
    body.push(
        Uop::Call {
            callee: k(0),
            nargs: 0,
        }
        .into(),
    );
    body.push(Uop::Mov { dst: 0, src: 6 }.into()); // the fresh list
    for i in 1..=i64::try_from(CHILDREN).expect("child count fits i64") {
        body.push(Uop::Mov { dst: 6, src: 0 }.into());
        body.push(
            Uop::Kimm {
                dst: 7,
                kidx: elem(&mut pool, i),
            }
            .into(),
        );
        body.push(
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
        );
        body.push(RcOp::Dec { src: 6 }.into());
    }
    // the shared word loads in both mains (the intern and the main's own
    // claim ride the baseline), the capturing closure staging it as its
    // one capture word
    body.push(Uop::Kstr { dst: 4, kidx: k(5) }.into());
    body.push(Uop::Kconst { dst: 3, kidx: k(3) }.into());
    if capture {
        body.push(Uop::Mov { dst: 6, src: 4 }.into()); // the capture at overlay 0
        body.push(
            Uop::ClosureNew {
                dst: 5,
                func: 3,
                n: 1,
            }
            .into(),
        );
    } else {
        body.push(
            Uop::ClosureNew {
                dst: 5,
                func: 3,
                n: 0,
            }
            .into(),
        );
    }
    body.push(Uop::Mov { dst: 6, src: 0 }.into()); // the list
    body.push(Uop::Mov { dst: 7, src: 5 }.into()); // the callable
    body.push(Uop::Kimm { dst: 8, kidx: k(4) }.into());
    body.push(Uop::Kimm { dst: 9, kidx: k(4) }.into());
    body.push(
        Uop::Callg {
            callee: k(2),
            nargs: 2,
        }
        .into(),
    );
    body.push(Uop::Mov { dst: 1, src: 6 }.into());
    body.push(Uop::Ret { src: 1 }.into());
    def(
        name,
        &[Ref, Ref, Raw, Raw, Ref, Raw, Raw, Raw, Raw, Raw],
        0,
        4,
        pool,
        &body,
    )
}

/// `() -> Ref`: the delivered-answer batch — the same fork span through
/// the minting body, so the joiner's ledger holds one foreign row per
/// answer.
fn delivered_main_def(name: &str) -> FunctionDef {
    let mut pool = vec![
        builtin(BUILTIN_LIST_NEW),  // k0
        builtin(BUILTIN_LIST_PUSH), // k1
        builtin(BUILTIN_PAR_MAP),   // k2
        Const::FuncRef(ConstFuncRef::Named {
            module: "lb".into(),
            func: 2,
        }), // k3 mint
        Const::I64(BuiltinInstance::List as i64), // k4
    ];
    let elem = |pool: &mut Vec<Const>, word: i64| {
        pool.push(Const::I64(word));
        k(u16::try_from(pool.len() - 1).expect("pool index fits u16"))
    };
    let mut body = vec![Uop::Funcf { frame_slots: 9 }.into()];
    body.push(
        Uop::Call {
            callee: k(0),
            nargs: 0,
        }
        .into(),
    );
    body.push(Uop::Mov { dst: 0, src: 5 }.into());
    for i in 1..=i64::try_from(CHILDREN).expect("child count fits i64") {
        body.push(Uop::Mov { dst: 5, src: 0 }.into());
        body.push(
            Uop::Kimm {
                dst: 6,
                kidx: elem(&mut pool, i),
            }
            .into(),
        );
        body.push(
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
        );
        body.push(RcOp::Dec { src: 5 }.into());
    }
    body.push(Uop::Mov { dst: 5, src: 0 }.into());
    body.push(Uop::Kconst { dst: 6, kidx: k(3) }.into());
    body.push(Uop::Kimm { dst: 7, kidx: k(4) }.into());
    body.push(Uop::Kimm { dst: 8, kidx: k(4) }.into());
    body.push(
        Uop::Callg {
            callee: k(2),
            nargs: 2,
        }
        .into(),
    );
    body.push(Uop::Mov { dst: 1, src: 5 }.into());
    body.push(Uop::Ret { src: 1 }.into());
    def(
        name,
        &[Ref, Ref, Raw, Raw, Raw, Raw, Raw, Raw, Raw],
        0,
        4,
        pool,
        &body,
    )
}

fn lb_registry() -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    reg.install(
        ModuleVersion::new("lb", vec![
            plain_def(),                            // 0
            capture_child_def(),                    // 1
            mint_def(),                             // 2
            fork_main_def("main_bare", 0, false),   // 3
            fork_main_def("main_capture", 1, true), // 4
            delivered_main_def("main_ref"),         // 5
        ])
        .expect("valid fixture module"),
    );
    reg
}

/// Runs one fork main and checks the join answered the result list word.
fn run_batch(vm: &mut Vm, func: &str) {
    let answer = vm.run("lb", func, &[]).expect("the batch lands");
    let [Value::Ref(_)] = answer.as_slice() else {
        panic!("{func} must answer its result list word");
    };
}

/// The capture fixture's own correctness: the batch runs the capturing
/// child body (each answer doubled), so the allocation deltas below
/// really measure the capture's cost.
#[test]
fn capture_children_answer_doubled() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut vm = Vm::new(lb_registry());
    let answer = vm.run("lb", "main_capture", &[]).expect("lands");
    let [Value::Ref(res)] = answer.as_slice() else {
        panic!("ref answer");
    };
    let res = *res;
    let store = vm.objects_of(res);
    let elems = store.list_at(res.id()).expect("reads");
    assert_eq!(elems.len(), CHILDREN);
    assert_eq!(elems[0], Value::I64(2), "capture_child doubles");
    assert_eq!(elems[CHILDREN - 1], Value::I64(128));
}

/// Shape one, pinned at its measured cost: one foreign capture per child
/// of a 64-element batch — the spawn copy-in's row, held for the child's
/// whole life, released by its exit dec. The capture batch pays 129
/// allocations over the bare batch: 64 child ledger tables (the hashbrown
/// first-insert allocation, one per child), 64 capture-block copies the
/// spawn staging materializes (`closure_parts`), and the closure's own
/// capture block. Re-bake this pin when any of those three sites changes.
#[test]
fn capture_children_pin_their_measured_cost_over_bare() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut vm = Vm::new(lb_registry());
    run_batch(&mut vm, "main_bare"); // warm the drive and the stores
    run_batch(&mut vm, "main_capture");
    let bare = exact_allocs(|| run_batch(&mut vm, "main_bare"), 8);
    let hold = exact_allocs(|| run_batch(&mut vm, "main_capture"), 8);
    println!(
        "nursery shape over {CHILDREN} children: bare-closure batch {bare} allocs, capture batch \
         {hold} allocs"
    );
    assert_eq!(
        hold.saturating_sub(bare),
        129,
        "the capture batch's measured cost: {CHILDREN} child ledger tables, {CHILDREN} \
         capture-block copies at spawn, one closure block"
    );
}

/// Shape two: one joiner's ledger, one foreign row per delivered answer of
/// a 64-answer batch — many rows in one table, the shape the hybrid
/// refused to touch. The pin: the steady-state batch's allocation count is
/// stable run over run (nothing per-row accumulates).
#[test]
fn delivered_answer_rows_stay_batch_bounded() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut vm = Vm::new(lb_registry());
    run_batch(&mut vm, "main_ref"); // warm the drive, the stores, the joiner's table path
    run_batch(&mut vm, "main_ref");
    let first = exact_allocs(|| run_batch(&mut vm, "main_ref"), 8);
    let again = exact_allocs(|| run_batch(&mut vm, "main_ref"), 8);
    println!(
        "delivered shape over {CHILDREN} answers: steady-state batch {first} allocs, repeat \
         {again} allocs"
    );
    assert_eq!(
        first, again,
        "a repeated batch must not accumulate per-row allocations"
    );
}
