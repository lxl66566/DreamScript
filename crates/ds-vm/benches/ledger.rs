//! Criterion timing of the claim ledger's two row-cost shapes (the ledger
//! shape decision's reproducible record): the nursery shape — a spawn/join
//! cycle where every child holds one foreign capture, one row per child
//! ledger born at the handoff and freed at the child's drain — and the
//! delivered shape — one fork-join batch whose joiner's ledger holds one
//! foreign row per child-owned `Ref` answer, many rows in one ledger. The
//! plain-child and i64-answer groups are the baselines each shape
//! subtracts; their difference is the ledger row's own time.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
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

/// The children each spawn/join cycle drives and the elements each fork
/// batch forks: one workload size for both shapes.
const CHILDREN: usize = 64;

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

/// `(n: i64) -> i64`: the plain child, no claim ever enters its ledger.
fn plain_def() -> FunctionDef {
    def("plain", &[I64], 1, 0, vec![], &[
        Uop::Funcf { frame_slots: 1 }.into(),
        Uop::Ret { src: 0 }.into(),
    ])
}

/// `(r: Ref) -> i64`: holds one foreign capture for its whole life, the
/// exit dec releasing the claim before the death (the lowered drop shape).
fn hold_def() -> FunctionDef {
    def("hold", &[Ref, I64], 1, 0, vec![Const::I64(1)], &[
        Uop::Funcf { frame_slots: 2 }.into(),
        Inst::Rc(RcOp::Dec { src: 0 }),
        Uop::Kimm { dst: 1, kidx: k(0) }.into(),
        Uop::Ret { src: 1 }.into(),
    ])
}

/// `(x: i64) -> Ref`: the fork element body minting a fresh string on the
/// child's own store, so the joiner's ledger holds a foreign row per
/// answer.
fn mint_def() -> FunctionDef {
    def("mint", &[I64, Ref], 1, 0, vec![Const::Str("e".into())], &[
        Uop::Funcf { frame_slots: 2 }.into(),
        Uop::Kstr { dst: 1, kidx: k(0) }.into(),
        Uop::Ret { src: 1 }.into(),
    ])
}

/// `() -> Ref`: `par_map` over `elems` through `callable` (0 = the i64
/// passthrough, 2 = the string mint), the result list's word answering.
fn par_main_def(name: &str, elems: &[i64], callable: u32) -> FunctionDef {
    let mut pool = vec![
        builtin(BUILTIN_LIST_NEW),
        builtin(BUILTIN_LIST_PUSH),
        builtin(BUILTIN_PAR_MAP),
        Const::FuncRef(ConstFuncRef::Named {
            module: "lb".into(),
            func: callable,
        }),
        Const::I64(BuiltinInstance::List as i64),
    ];
    let word_at = |pool: &mut Vec<Const>, word: i64| {
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
    body.push(Uop::Mov { dst: 0, src: 5 }.into()); // the fresh list
    for &elem in elems {
        let word = word_at(&mut pool, elem);
        body.push(Uop::Mov { dst: 5, src: 0 }.into());
        body.push(Uop::Kimm { dst: 6, kidx: word }.into());
        body.push(
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
        );
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
        &[Ref, Raw, Raw, Raw, Raw, Raw, Raw, Raw, Raw],
        0,
        4,
        pool,
        &body,
    )
}

fn lb_registry() -> ModuleRegistry {
    let elems: Vec<i64> = (1..=CHILDREN)
        .map(i64::try_from)
        .map(Result::unwrap)
        .collect();
    let mut reg = ModuleRegistry::new();
    reg.install(
        ModuleVersion::new("lb", vec![
            plain_def(),
            hold_def(),
            mint_def(),
            par_main_def("main_plain", &elems, 0),
            par_main_def("main_ref", &elems, 2),
        ])
        .expect("valid bench module"),
    );
    reg
}

/// One spawn/join cycle over `children` children, `capture` staging the
/// same root-owned word into every child (one foreign row per child
/// ledger) or none.
fn spawn_join(vm: &mut Vm, children: usize, capture: Option<Value>) {
    let n = vm.open_root_nursery();
    let (func, arg) = match capture {
        Some(word) => ("hold", word),
        None => ("plain", Value::I64(1)),
    };
    for _ in 0..children {
        vm.spawn_fiber_in(n, "lb", func, &[arg]).expect("spawn");
    }
    vm.join_nursery(n).expect("the join drives");
}

fn ledger_shapes(c: &mut Criterion) {
    let mut group = c.benchmark_group("ledger_shapes");
    let mut vm = Vm::new(lb_registry());
    let capture = vm.intern_str("capture");
    group.bench_function("nursery_64_plain_children", |b| {
        b.iter(|| spawn_join(&mut vm, CHILDREN, None));
    });
    group.bench_function("nursery_64_capture_children", |b| {
        b.iter(|| spawn_join(&mut vm, CHILDREN, Some(capture)));
    });
    group.bench_function("delivered_64_i64_answers", |b| {
        b.iter(|| {
            black_box(
                vm.run("lb", "main_plain", &[])
                    .expect("the i64 batch lands"),
            )
        });
    });
    group.bench_function("delivered_64_ref_answers", |b| {
        b.iter(|| black_box(vm.run("lb", "main_ref", &[]).expect("the ref batch lands")));
    });
    group.finish();
}

criterion_group!(benches, ledger_shapes);
criterion_main!(benches);
