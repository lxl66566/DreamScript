//! Criterion bench of the validating dsmod reader over one fixture module
//! built in-process: `read` is the structural pass alone, `read_checked` adds
//! the blake3 source-hash verification. The writer runs once before timing,
//! so the group measures the reader alone; contrast with the write-then-read
//! round trip, which is the writer's bench, not this one.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use ds_abi::{
    ConstIdx, FrameLayout,
    SlotType::{Bool, I64},
    uop::Uop,
};
use ds_mod::{Constant, Function, Hash128, Module, TargetClass, read, read_checked, write};

const SOURCE: &[u8] = b"module reader round trip\n";

/// The number of function records the fixture module carries: enough code
/// words, names, and pools that the per-uop and per-string validation paths
/// dominate the section-directory paperwork.
const FUNCTIONS: usize = 256;

/// One `sum_to`-shaped arithmetic loop: pool k0 = 0, k1 = 1, slots
/// [n, acc, t, z, one].
fn function(index: usize) -> Function {
    Function::new(
        format!("fn_{index}"),
        FrameLayout::new(vec![I64, I64, Bool, I64, I64], 1, 0).expect("valid fixture layout"),
        vec![Constant::I64(0), Constant::I64(1)],
        [
            Uop::Funcf { frame_slots: 5 },
            Uop::Kimm {
                dst: 1,
                kidx: ConstIdx(0),
            }, // acc = 0
            Uop::Kimm {
                dst: 3,
                kidx: ConstIdx(0),
            }, // z = 0
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
            Uop::Kimm {
                dst: 4,
                kidx: ConstIdx(1),
            }, // one = 1
            Uop::SubI64 {
                dst: 0,
                lhs: 0,
                rhs: 4,
            }, // n -= 1
            Uop::Jmp { target: 3 },
            Uop::Ret { src: 1 },
        ],
    )
    .expect("the fixture function encodes")
}

/// The serialized fixture: a triple-target module with `FUNCTIONS` records,
/// module-level constants of every kind, two dependencies, and debug spans.
fn fixture_bytes() -> Vec<u8> {
    let mut module = Module::new(
        "reader_fixture",
        TargetClass::Triple("x86_64-unknown-linux-gnu".to_owned()),
        Hash128::compute(SOURCE),
    )
    .with_dep("std", Hash128::compute(b"std package content"))
    .with_dep("csv", Hash128::compute(b"csv package content"))
    .with_constant(Constant::I64(-42))
    .with_constant(Constant::F64(3.25))
    .with_constant(Constant::Bool(true))
    .with_constant(Constant::Str("h\u{e9}llo reader".to_owned()))
    .with_constant(Constant::FuncRef {
        module: "csv".to_owned(),
        func: 2,
    });
    for index in 0..FUNCTIONS {
        let span = u32::try_from(index).expect("fixture index fits u32");
        module = module
            .with_function(function(index))
            .with_debug_span(span, span + 1);
    }
    write(&module)
}

fn bench_read(c: &mut Criterion) {
    let bytes = fixture_bytes();
    // the fixture is what both functions below claim to read
    assert_eq!(
        read(&bytes)
            .expect("the fixture reads back")
            .functions()
            .len(),
        FUNCTIONS
    );
    let mut group = c.benchmark_group("dsmod_read");
    group.throughput(Throughput::Bytes(bytes.len() as u64));
    group.bench_function("read", |b| {
        b.iter(|| read(&bytes).expect("the fixture reads back"));
    });
    group.bench_function("read_checked", |b| {
        b.iter(|| read_checked(&bytes, SOURCE).expect("the fixture reads back checked"));
    });
    group.finish();
}

criterion_group!(benches, bench_read);
criterion_main!(benches);
