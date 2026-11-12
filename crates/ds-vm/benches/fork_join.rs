//! The fork-join scheduler backend measurement [E3]: the parked-CALLG
//! slice's own per-child fork overhead — one spawned child plus the
//! interlock drive's join per element, the whole sequential single-thread
//! cost — against the candidate parallel backends' per-element dispatch
//! overhead over the same trivial element body: youpipe's fused scoped
//! pipeline and std's scoped threads (the naive bracket of any
//! hand-rolled work-stealing pool's spawn/steal/wakeup floor). The M5
//! backend decision reads these numbers, so the bench is the decision's
//! reproducible record: per-child and per-element costs are the totals'
//! difference over the 64-element span.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use ds_vm::{
    Const, FunctionDef, ModuleRegistry, ModuleVersion, Vm,
    abi::{BuiltinInstance, ConstIdx, FrameLayout, SlotType, SlotType::Raw, uop::Uop},
    assemble,
    native::{BUILTIN_LIST_NEW, BUILTIN_LIST_PUSH, BUILTIN_PAR_MAP},
};

/// The elements one measurement run forks over.
const ELEMENTS: u32 = 64;

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn builtin(index: u32) -> Const {
    Const::FuncRef {
        module: "builtin".into(),
        func: index,
    }
}

/// `(x: i64) -> i64`: the trivial element body (the backend's dispatch
/// overhead, not the child's work, is the measured cost).
fn passthrough_def() -> FunctionDef {
    FunctionDef {
        name: "pass".into(),
        layout: FrameLayout::new(vec![SlotType::I64], 1, 0).expect("valid fixture layout"),
        pool: vec![],
        code: assemble(&[Uop::Funcf { frame_slots: 1 }, Uop::Ret { src: 0 }])
            .expect("fixture body encodes"),
    }
}

/// `() -> i64 Ref`: `par_map` over `elems` i64s through the passthrough
/// callable; the result list's word answers, nothing else runs.
fn par_main_def(name: &str, elems: &[i64]) -> FunctionDef {
    let mut pool = vec![
        builtin(BUILTIN_LIST_NEW),
        builtin(BUILTIN_LIST_PUSH),
        builtin(BUILTIN_PAR_MAP),
        Const::FuncRef {
            module: "bm".into(),
            func: 2, // the passthrough callable
        },
        Const::I64(BuiltinInstance::List as i64),
    ];
    let word_at = |pool: &mut Vec<Const>, word: i64| {
        pool.push(Const::I64(word));
        k(u16::try_from(pool.len() - 1).expect("pool index fits u16"))
    };
    let mut body = vec![Uop::Funcf { frame_slots: 9 }];
    body.push(Uop::Call {
        callee: k(0),
        nargs: 0,
    });
    body.push(Uop::Mov { dst: 0, src: 5 }); // the fresh list
    for &elem in elems {
        let word = word_at(&mut pool, elem);
        body.push(Uop::Mov { dst: 5, src: 0 });
        body.push(Uop::Kimm { dst: 6, kidx: word });
        body.push(Uop::Call {
            callee: k(1),
            nargs: 2,
        });
    }
    body.push(Uop::Mov { dst: 5, src: 0 });
    body.push(Uop::Kconst { dst: 6, kidx: k(3) });
    body.push(Uop::Kimm { dst: 7, kidx: k(4) });
    body.push(Uop::Kimm { dst: 8, kidx: k(4) });
    body.push(Uop::Callg {
        callee: k(2),
        nargs: 2,
    });
    body.push(Uop::Mov { dst: 1, src: 5 });
    body.push(Uop::Ret { src: 1 });
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(
            vec![SlotType::Ref, Raw, Raw, Raw, Raw, Raw, Raw, Raw, Raw],
            0,
            4,
        )
        .expect("valid fixture layout"),
        pool,
        code: assemble(&body).expect("fixture body encodes"),
    }
}

fn fork_registry(elems: &[i64]) -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    reg.install(
        ModuleVersion::new("bm", vec![
            par_main_def("main", elems),
            par_main_def("main_empty", &[]),
            passthrough_def(),
        ])
        .expect("valid bench module"),
    );
    reg
}

fn fork_join_backend(c: &mut Criterion) {
    let mut group = c.benchmark_group("fork_join_backend");
    let elems: Vec<i64> = (1..=ELEMENTS).map(i64::from).collect();
    let mut vm = Vm::new(fork_registry(&elems));
    group.bench_function("vm_slice_64_children", |b| {
        b.iter(|| {
            black_box(vm.run("bm", "main", &[]).expect("the slice join lands"));
        });
    });
    group.bench_function("vm_slice_0_children", |b| {
        b.iter(|| {
            black_box(
                vm.run("bm", "main_empty", &[])
                    .expect("the empty slice lands"),
            );
        });
    });
    group.bench_function("youpipe_pipe_64", |b| {
        b.iter(|| {
            black_box(youpipe::scope(|s| {
                s.pipe(1..=ELEMENTS as usize).map(|x: usize| x + 1).count()
            }));
        });
    });
    group.bench_function("std_scoped_threads_64", |b| {
        b.iter(|| {
            black_box(std::thread::scope(|scope| {
                let handles: Vec<_> = (1..=ELEMENTS)
                    .map(|x| scope.spawn(move || u64::from(x) + 1))
                    .collect();
                let mut total = 0u64;
                for handle in handles {
                    total = total.wrapping_add(handle.join().expect("the thread answers"));
                }
                total
            }))
        });
    });
    group.finish();
}

criterion_group!(benches, fork_join_backend);
criterion_main!(benches);
