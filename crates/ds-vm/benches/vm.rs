//! Criterion wall-clock benches over the encoded fixture programs of
//! tests/fixtures.rs: the `sum_to` arithmetic loop, the mutual even/odd
//! CALLT tail chain, and qualified-call dispatch through the two-slot
//! module table.
//!
//! Fixture builders are duplicated from tests/fixtures.rs on purpose:
//! benches link ds-vm as an external crate, so sharing would require new
//! public API, and the builders are small and stable.

use std::hint::black_box;

use criterion::{Criterion, SamplingMode, Throughput, criterion_group, criterion_main};
use ds_vm::{
    Const, FunctionDef, ModuleRegistry, ModuleVersion, Value, Vm,
    abi::{
        ConstIdx, FrameLayout, SlotType,
        SlotType::{Bool, I64},
        uop::Uop,
    },
    assemble,
};

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn funcref(module: &str, func: u32) -> Const {
    Const::FuncRef {
        module: module.into(),
        func,
    }
}

fn def(name: &str, slots: &[SlotType], params: usize, args: usize, body: &[Uop]) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout"),
        pool: Vec::new(),
        code: assemble(body).expect("fixture body encodes"),
    }
}

fn registry(module: &str, pool: &[Const], defs: Vec<FunctionDef>) -> ModuleRegistry {
    // mirrors tests/fixtures.rs: one shared authoring pool, copied per function
    let defs = defs
        .into_iter()
        .map(|mut d| {
            d.pool = pool.to_vec();
            d
        })
        .collect();
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new(module, defs).expect("valid fixture module"));
    reg
}

/// `fn sum_to(n: i64) -> i64` as in tests/fixtures.rs: one root CALL frame
/// running an arithmetic loop of LEI64/JMPIF/ADDI64/SUBI64/JMP with pooled
/// immediates.
fn sum_to_fixture() -> ModuleRegistry {
    registry("mut", &[Const::I64(0), Const::I64(1)], vec![def(
        "sum_to",
        &[I64, I64, Bool, I64, I64],
        1,
        0,
        &[
            Uop::Funcf { frame_slots: 5 },
            Uop::Kimm { dst: 1, kidx: k(0) }, // acc = 0
            Uop::Kimm { dst: 3, kidx: k(0) }, // z = 0
            Uop::LeI64 {
                dst: 2,
                lhs: 0,
                rhs: 3,
            }, // t = n <= z
            Uop::Jmpif { cond: 2, target: 9 },
            Uop::AddI64 {
                dst: 1,
                lhs: 1,
                rhs: 0,
            }, // acc += n
            Uop::Kimm { dst: 4, kidx: k(1) }, // one = 1
            Uop::SubI64 {
                dst: 0,
                lhs: 0,
                rhs: 4,
            }, // n -= 1
            Uop::Jmp { target: 3 },
            Uop::Ret { src: 1 },
        ],
    )])
}

/// Mutual `even`/`odd` tail calls as in tests/fixtures.rs (the canonical
/// ds-abi encoding): one CALLT slide per depth step at constant stack.
fn even_odd_fixture() -> ModuleRegistry {
    let body = |callee: u16, base: u16| {
        vec![
            Uop::Funcf { frame_slots: 4 },
            Uop::Kimm { dst: 2, kidx: k(0) }, // stage 0
            Uop::EqI64 {
                dst: 1,
                lhs: 0,
                rhs: 2,
            }, // t = n == 0
            Uop::Jmpif { cond: 1, target: 7 },
            Uop::Kimm { dst: 2, kidx: k(1) }, // stage 1
            Uop::SubI64 {
                dst: 3,
                lhs: 0,
                rhs: 2,
            }, // stage n - 1 in the arg area
            Uop::Callt {
                callee: k(callee),
                nargs: 1,
            }, // slide the staged arg into slot 0
            Uop::Kconst {
                dst: 1,
                kidx: k(base),
            }, // t = true / false
            Uop::Ret { src: 1 },
        ]
    };
    registry(
        "mut",
        &[
            Const::I64(0),
            Const::I64(1),
            Const::Bool(true),
            funcref("mut", 1), // odd
            funcref("mut", 0), // even
            Const::Bool(false),
        ],
        vec![
            def("even", &[I64, Bool, I64, I64], 1, 1, &body(3, 2)),
            def("odd", &[I64, Bool, I64, I64], 1, 1, &body(4, 5)),
        ],
    )
}

fn counter_version(get_value: i64) -> ModuleVersion {
    ModuleVersion::new("counter", vec![FunctionDef {
        pool: vec![Const::I64(get_value)],
        ..def("get", &[I64], 0, 0, &[
            Uop::Funcf { frame_slots: 1 },
            Uop::Kimm { dst: 0, kidx: k(0) },
            Uop::Ret { src: 0 },
        ])
    }])
    .expect("valid fixture module")
}

fn bench_sum_to(c: &mut Criterion) {
    let mut group = c.benchmark_group("sum_to");
    for (label, n) in [("1e3", 1_000_i64), ("1e5", 100_000_i64)] {
        // loop iterations per run: gives uops/sec for the arithmetic body
        group.throughput(Throughput::Elements(n.unsigned_abs()));
        let mut vm = Vm::new(sum_to_fixture());
        group.bench_function(label, |b| {
            b.iter(|| {
                let v = vm
                    .run("mut", "sum_to", &[Value::I64(n)])
                    .expect("sum_to runs");
                black_box(v);
            });
        });
    }
    group.finish();
}

fn bench_even_odd_callt(c: &mut Criterion) {
    let mut group = c.benchmark_group("even_odd_callt");
    // The 1e7 chain runs long enough per iteration that Auto's batched
    // sampling would dominate the whole bench run: Flat takes one
    // measurement per sample, and 10 samples suffice for a smoke-grade
    // harness (no formal gate before the M3 C-baseline comparison).
    group.sampling_mode(SamplingMode::Flat);
    group.sample_size(10);
    for (label, depth) in [("1e5", 100_000_i64), ("1e7", 10_000_000_i64)] {
        // tail calls per run: gives CALLT slides/sec
        group.throughput(Throughput::Elements(depth.unsigned_abs()));
        let mut vm = Vm::new(even_odd_fixture());
        group.bench_function(label, |b| {
            b.iter(|| {
                let v = vm
                    .run("mut", "even", &[Value::I64(depth)])
                    .expect("even runs");
                black_box(v);
            });
        });
    }
    group.finish();
}

fn bench_module_dispatch(c: &mut Criterion) {
    // Steady-state churn on the two-slot module table: every pool FuncRef
    // call re-resolves "counter"/get through the current slot while a
    // retiring version occupies the second slot. The install/swap itself is
    // NOT benched: ModuleVersion construction dominates it and would hide
    // the per-call dispatch cost this bench exists to expose.
    let mut vm = Vm::new(ModuleRegistry::new());
    vm.install_module(counter_version(1));
    vm.install_module(counter_version(2));
    let mut group = c.benchmark_group("module_dispatch");
    group.bench_function("qualified_get", |b| {
        b.iter(|| {
            let v = vm.run("counter", "get", &[]).expect("get runs");
            black_box(v);
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_sum_to,
    bench_even_odd_callt,
    bench_module_dispatch
);
criterion_main!(benches);
