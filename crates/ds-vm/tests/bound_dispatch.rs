//! Bound-artifact dispatch fixtures: the VM's bound tier end to end
//! over a real C-ABI cdylib the host's rustc builds at test time. The
//! module installs under its manifest artifact name, a pool
//! `Constant::Bound` callee addresses one export by its enumeration
//! index, and the CALL and CALLT arms marshal the staged words per their
//! own kinds and box the foreign answer per the constant's return word —
//! the loader's install validation and the honest refusals pinned beside
//! the answers.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use ds_abi::BoundRet;

mod support;
use ds_mod::{Constant, Function, Hash128, Module, TargetClass, read, write};
use ds_vm::{
    BoundModule, FrameAnswer, LoadError, ModuleRegistry, Value, Vm,
    abi::{
        ConstIdx, FrameLayout, SlotType,
        SlotType::{F64, I64},
        uop::Uop,
    },
};

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn layout(slots: &[SlotType], params: usize, args: usize) -> FrameLayout {
    FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout")
}

/// The fixture library's file name per platform: the loader-recognized
/// spelling rustc emits for a cdylib.
fn library_name() -> &'static str {
    if cfg!(windows) {
        "dsvm_bound_fixture.dll"
    } else if cfg!(target_os = "macos") {
        "libdsvm_bound_fixture.dylib"
    } else {
        "libdsvm_bound_fixture.so"
    }
}

/// A unique guarded temp dir per tag, removed at the test thread's
/// exit.
fn temp_dir(tag: &str) -> PathBuf {
    support::guarded_dir(&format!("ds-vm-bound-{tag}-"))
}

/// Compiles the fixture cdylib into `dir` and answers its bytes. Cargo
/// sets `RUSTC` for the test binary, so a build failure is a test
/// failure carrying rustc's own stderr — never a silent pass.
fn build_fixture(dir: &Path) -> Vec<u8> {
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
    let out = dir.join(library_name());
    let built = Command::new(rustc)
        .arg("--edition=2024")
        .arg("--crate-type=cdylib")
        .arg("--crate-name=dsvm_bound_fixture")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/bound_cdylib.rs"
        ))
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap_or_else(|error| panic!("the fixture's rustc runs: {error}"));
    assert!(
        built.status.success(),
        "the fixture cdylib builds (rustc stderr: {})",
        String::from_utf8_lossy(&built.stderr)
    );
    fs::read(&out).expect("the built fixture reads back")
}

/// The fixture build written out for the loader.
fn fixture(tag: &str) -> (Vec<u8>, PathBuf) {
    let dir = temp_dir(tag);
    let bytes = build_fixture(&dir);
    let path = dir.join(library_name());
    (bytes, path)
}

/// The fixture's bound module plus its export enumeration: the order is
/// the table's index space.
fn bound_of(bytes: &[u8], path: &Path) -> (BoundModule, Vec<String>) {
    let library = ds_ffi::Library::open(path).expect("the loader takes the fixture");
    let names = ds_ffi::enumerate(bytes).expect("the export tables read");
    let module = BoundModule::bind(library, bytes).expect("the fixture binds");
    (module, names)
}

/// The fixture's bound module over the same bytes, a second load for a
/// second VM (the loader's reference counting keeps both live).
fn bound_again(bytes: &[u8], path: &Path) -> BoundModule {
    bound_of(bytes, path).0
}

/// One entry module calling a bound symbol: `main` stages every pool
/// constant but the last into the argument area, one word each, and
/// calls (or tail-calls) the last constant — the bound symbol — the
/// answer returning from the overlay slot.
fn call_module(slots: &[SlotType], consts: Vec<Constant>, tail: bool) -> Module {
    let nargs = consts.len() - 1;
    let stage = |body: &mut Vec<Uop>, index: u8, kidx: u16| {
        // an i64 constant stages through KIMM, every other scalar through
        // KCONST (the pool-kind rule the install validation pins)
        if matches!(consts[usize::from(kidx)], Constant::I64(_)) {
            body.push(Uop::Kimm {
                dst: index,
                kidx: k(kidx),
            });
        } else {
            body.push(Uop::Kconst {
                dst: index,
                kidx: k(kidx),
            });
        }
    };
    let mut body = vec![Uop::Funcf {
        frame_slots: u16::try_from(slots.len()).expect("the fixture frame fits u16"),
    }];
    for position in 0..nargs {
        stage(
            &mut body,
            u8::try_from(position).expect("the staged arguments fit u8"),
            u16::try_from(position).expect("the constant index fits u16"),
        );
    }
    let callee = k(u16::try_from(nargs).expect("the constant index fits u16"));
    let nargs = u8::try_from(nargs).expect("the staged arguments fit u8");
    if tail {
        body.push(Uop::Callt { callee, nargs });
    } else {
        body.push(Uop::Call { callee, nargs });
        body.push(Uop::Ret { src: 0 });
    }
    let main =
        Function::new("main", layout(slots, 0, slots.len()), consts, body).expect("valid function");
    Module::new(
        "app",
        TargetClass::Bytecode,
        Hash128::compute(b"bound dispatch source"),
    )
    .with_function(main)
}

/// A VM with the fixture installed as `math` and one module loaded
/// through the dsmod round-trip (the install path the run drives).
fn vm_over(bound: BoundModule, module: &Module) -> Vm {
    let mut registry = ModuleRegistry::new();
    registry.install_bound("math", bound);
    let mut vm = Vm::new(registry);
    vm.install_dsmod(read(&write(module)).expect("the module round-trips"))
        .expect("the module installs");
    vm
}

/// The bound call answers over both call arms: the GP pair marshals, the
/// scalar answer overlaying argument slot 0 for a CALL and riding the
/// reused frame for the tail form.
#[test]
fn a_bound_call_answers_over_call_and_tail() {
    let (bytes, path) = fixture("answer");
    let (_, names) = bound_of(&bytes, &path);
    let index = names
        .iter()
        .position(|name| name == "ds_add")
        .expect("the fixture exports ds_add");
    for tail in [false, true] {
        let module = call_module(
            &[I64, I64],
            vec![Constant::I64(20), Constant::I64(22), Constant::Bound {
                module: "math".to_owned(),
                index: u32::try_from(index).expect("the export index fits u32"),
                ret: BoundRet::Int,
            }],
            tail,
        );
        let mut vm = vm_over(bound_again(&bytes, &path), &module);
        assert_eq!(
            vm.run("app", "main", &[]),
            Ok(FrameAnswer::one(Value::I64(42))),
            "the bound {} answers",
            if tail { "tail call" } else { "call" }
        );
    }
}

/// The FP pair marshals through the same dispatch, the answer boxed by
/// the constant's return kind.
#[test]
fn the_fp_pair_answers() {
    let (bytes, path) = fixture("fp");
    let (_, names) = bound_of(&bytes, &path);
    let index = names
        .iter()
        .position(|name| name == "ds_scale")
        .expect("the fixture exports ds_scale");
    let module = call_module(
        &[F64, F64],
        vec![Constant::F64(1.5), Constant::F64(4.0), Constant::Bound {
            module: "math".to_owned(),
            index: u32::try_from(index).expect("the export index fits u32"),
            ret: BoundRet::Float,
        }],
        false,
    );
    let mut vm = vm_over(bound_again(&bytes, &path), &module);
    assert_eq!(
        vm.run("app", "main", &[]),
        Ok(FrameAnswer::one(Value::F64(6.0))),
        "the FP pair marshals"
    );
}

/// The mixed GP+FP sequence marshals: the bool word rides the GP file
/// and the float the FP file in one call.
#[test]
fn the_mixed_gp_fp_sequence_answers() {
    let (bytes, path) = fixture("mixed");
    let (_, names) = bound_of(&bytes, &path);
    let index = names
        .iter()
        .position(|name| name == "ds_gated")
        .expect("the fixture exports ds_gated");
    let module = call_module(
        &[SlotType::Bool, F64],
        vec![Constant::Bool(true), Constant::F64(3.5), Constant::Bound {
            module: "math".to_owned(),
            index: u32::try_from(index).expect("the export index fits u32"),
            ret: BoundRet::Float,
        }],
        false,
    );
    let mut vm = vm_over(bound_again(&bytes, &path), &module);
    assert_eq!(
        vm.run("app", "main", &[]),
        Ok(FrameAnswer::one(Value::F64(3.5))),
        "the mixed sequence marshals"
    );
}

/// A call at the fast-path word boundary answers: four GP words ride
/// the enumerated transmute's widest all-GP arm.
#[test]
fn a_call_at_the_word_boundary_answers_enumerated() {
    let (bytes, path) = fixture("sum4");
    let (_, names) = bound_of(&bytes, &path);
    let index = names
        .iter()
        .position(|name| name == "ds_sum4")
        .expect("the fixture exports ds_sum4");
    let module = call_module(
        &[I64, I64, I64, I64],
        vec![
            Constant::I64(1),
            Constant::I64(2),
            Constant::I64(3),
            Constant::I64(4),
            Constant::Bound {
                module: "math".to_owned(),
                index: u32::try_from(index).expect("the export index fits u32"),
                ret: BoundRet::Int,
            },
        ],
        false,
    );
    let mut vm = vm_over(bound_again(&bytes, &path), &module);
    assert_eq!(
        vm.run("app", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(10))),
        "the boundary call marshals"
    );
}

/// The boundary's mixed placement answers: the bool words ride the GP
/// file and the floats the FP file in one four-word call.
#[test]
fn the_mixed_placement_at_the_word_boundary_answers() {
    let (bytes, path) = fixture("pick4");
    let (_, names) = bound_of(&bytes, &path);
    let index = names
        .iter()
        .position(|name| name == "ds_pick4")
        .expect("the fixture exports ds_pick4");
    let module = call_module(
        &[SlotType::Bool, F64, SlotType::Bool, F64],
        vec![
            Constant::Bool(true),
            Constant::F64(1.5),
            Constant::Bool(false),
            Constant::F64(2.5),
            Constant::Bound {
                module: "math".to_owned(),
                index: u32::try_from(index).expect("the export index fits u32"),
                ret: BoundRet::Float,
            },
        ],
        false,
    );
    let mut vm = vm_over(bound_again(&bytes, &path), &module);
    assert_eq!(
        vm.run("app", "main", &[]),
        Ok(FrameAnswer::one(Value::F64(1.5))),
        "the boundary's mixed placement marshals"
    );
}

/// A call one word past the fast-path boundary answers through
/// libffi's CIF path: five GP words marshal onto the same bound
/// dispatch.
#[test]
fn a_call_past_the_word_boundary_answers_through_the_cif() {
    let (bytes, path) = fixture("sum5");
    let (_, names) = bound_of(&bytes, &path);
    let index = names
        .iter()
        .position(|name| name == "ds_sum5")
        .expect("the fixture exports ds_sum5");
    let module = call_module(
        &[I64, I64, I64, I64, I64],
        vec![
            Constant::I64(1),
            Constant::I64(2),
            Constant::I64(3),
            Constant::I64(4),
            Constant::I64(5),
            Constant::Bound {
                module: "math".to_owned(),
                index: u32::try_from(index).expect("the export index fits u32"),
                ret: BoundRet::Int,
            },
        ],
        false,
    );
    let mut vm = vm_over(bound_again(&bytes, &path), &module);
    assert_eq!(
        vm.run("app", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(15))),
        "the past-boundary call marshals through the CIF path"
    );
}

/// An export index past the enumeration refuses at install: the loader
/// validates every bound constant against the installed table.
#[test]
fn an_out_of_range_export_index_refuses_the_install() {
    let (bytes, path) = fixture("range");
    let (bound, names) = bound_of(&bytes, &path);
    let past = u32::try_from(names.len()).expect("the enumeration fits u32");
    let module = call_module(
        &[I64, I64],
        vec![Constant::I64(20), Constant::I64(22), Constant::Bound {
            module: "math".to_owned(),
            index: past,
            ret: BoundRet::Int,
        }],
        false,
    );
    let mut registry = ModuleRegistry::new();
    registry.install_bound("math", bound);
    let error = ds_vm::loader::install(
        &mut registry,
        read(&write(&module)).expect("the module round-trips"),
    )
    .expect_err("the install refuses the out-of-range index");
    assert!(
        matches!(
            &error,
            LoadError::UnresolvedCallee { module, index, .. }
                if module == "math" && *index == past
        ),
        "the refusal names the artifact and the index ({error:?})"
    );
}

/// A bound constant naming an artifact nothing installed refuses at
/// install, naming the artifact.
#[test]
fn an_uninstalled_artifact_name_refuses_the_install() {
    let (bytes, path) = fixture("uninstalled");
    let (_, names) = bound_of(&bytes, &path);
    let index = names
        .iter()
        .position(|name| name == "ds_add")
        .expect("the fixture exports ds_add");
    let module = call_module(
        &[I64, I64],
        vec![Constant::I64(20), Constant::I64(22), Constant::Bound {
            module: "math".to_owned(),
            index: u32::try_from(index).expect("the export index fits u32"),
            ret: BoundRet::Int,
        }],
        false,
    );
    let error = ds_vm::loader::install(
        &mut ModuleRegistry::new(),
        read(&write(&module)).expect("the module round-trips"),
    )
    .expect_err("the install refuses the uninstalled artifact");
    assert!(
        matches!(
            &error,
            LoadError::UnresolvedCallee {
                module,
                index: at,
                ..
            } if module == "math" && *at == u32::try_from(index).expect("the export index fits u32")
        ),
        "the refusal names the artifact ({error:?})"
    );
}

/// A unit-returning bound call answers the canonical unit word over
/// both call arms: the absent return word boxes as the `()` value's
/// single I64 word, the empty staging leaving the overlay word to the
/// answer alone.
#[test]
fn a_unit_return_answers_the_unit_word() {
    let (bytes, path) = fixture("unit");
    let (_, names) = bound_of(&bytes, &path);
    let index = names
        .iter()
        .position(|name| name == "ds_silent")
        .expect("the fixture exports ds_silent");
    for tail in [false, true] {
        let mut body = vec![Uop::Funcf { frame_slots: 1 }];
        if tail {
            body.push(Uop::Callt {
                callee: k(0),
                nargs: 0,
            });
        } else {
            body.push(Uop::Call {
                callee: k(0),
                nargs: 0,
            });
            body.push(Uop::Ret { src: 0 });
        }
        let main = Function::new(
            "main",
            layout(&[I64], 0, 1),
            vec![Constant::Bound {
                module: "math".to_owned(),
                index: u32::try_from(index).expect("the export index fits u32"),
                ret: BoundRet::Unit,
            }],
            body,
        )
        .expect("valid function");
        let module = Module::new(
            "app",
            TargetClass::Bytecode,
            Hash128::compute(b"bound unit source"),
        )
        .with_function(main);
        let mut vm = vm_over(bound_again(&bytes, &path), &module);
        assert_eq!(
            vm.run("app", "main", &[]),
            Ok(FrameAnswer::one(Value::I64(0))),
            "the unit {} answers the canonical unit word",
            if tail { "tail call" } else { "call" }
        );
    }
}
