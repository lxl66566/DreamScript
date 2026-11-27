//! The ord-routed generic max/min pair over primitive, reference, and
//! multi-word record operands. Int sites and concrete float member sites
//! use ordinary inherent CALLs. Other sites stage both operand spans and
//! the solved compare callable for one generic dispatch.

use ds_abi::Inst;
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, lower};
use ds_mod::{FuncRefKey, Hash128, read, write};
use ds_parse::parse;
use ds_stdlib::AMBIENT_SOURCE;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

const SOURCE: &str = include_str!("fixtures/ord_pick_probe.ds");

fn build_vm() -> (Vm, Lowered) {
    let mut files = Files::new();
    let ambient = files.add_source("ambient.ds", AMBIENT_SOURCE);
    let id = files.add_source("ord_pick_probe.ds", SOURCE);
    let asts = [
        parse(&files, ambient).expect("the ambient parses"),
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
    let lowered = lower(
        "ord_pick_probe",
        Hash128::compute(SOURCE.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers");
    let module = read(&write(lowered.module())).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    (Vm::new(registry), lowered)
}

#[test]
fn generic_and_concrete_picks_preserve_their_ordering_contracts() {
    let (mut vm, _) = build_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("ord_pick_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0)))
    );
    assert_eq!(
        vm.captured_output(),
        b"ints 9 3 9 3\nwords pear apple\nrecords high low\nties first first\n\
          generic nan true true true true\nmember nan true true true true\n"
    );
    assert!(vm.objects().is_empty(), "the selected references drain");
}

#[test]
fn int_and_float_members_plain_call_while_generic_sites_stage_comparators() {
    let (_, lowered) = build_vm();
    let functions = lowered.module().functions();
    let mut generic_sites = 0;
    let mut record_sites = 0;
    let mut primitive_calls = 0;
    for function in functions {
        for (pc, word) in function.code().iter().enumerate() {
            let Inst::Uop(uop) = Inst::decode(pc as u32, *word).expect("valid instruction") else {
                continue;
            };
            match uop {
                ds_abi::Uop::Callg { callee, nargs } => {
                    let ds_mod::Constant::FuncRef {
                        key: FuncRefKey::Native(module),
                        func,
                    } = &function.constants()[usize::from(callee.0)]
                    else {
                        continue;
                    };
                    if module != native::BUILTIN_MODULE
                        || (*func != native::BUILTIN_MAX && *func != native::BUILTIN_MIN)
                    {
                        continue;
                    }
                    generic_sites += 1;
                    match nargs {
                        3 => {},
                        5 => record_sites += 1,
                        other => panic!("unexpected pick width {other}"),
                    }
                },
                ds_abi::Uop::Call { callee, nargs: 2 } => {
                    let ds_mod::Constant::FuncRef {
                        key: FuncRefKey::Dep(0),
                        func,
                    } = &function.constants()[usize::from(callee.0)]
                    else {
                        continue;
                    };
                    let name = functions[*func as usize].name();
                    if (name.ends_with("max") || name.ends_with("min"))
                        && (name.contains("int") || name.contains("float"))
                    {
                        primitive_calls += 1;
                    }
                },
                _ => {},
            }
        }
    }
    assert_eq!(
        generic_sites, 10,
        "two string, four record, four float sites"
    );
    assert_eq!(record_sites, 4, "both record spans precede the comparator");
    assert_eq!(primitive_calls, 8, "four int and four float member calls");
}

#[test]
fn two_maximal_operand_spans_refuse_before_overlay_addressing() {
    let fields = (0..128)
        .map(|index| format!("f{index}: int"))
        .collect::<Vec<_>>()
        .join(", ");
    let source = format!(
        "struct wide {{ {fields} }}\nimpl ord for wide {{ fn compare(self, other: Self) -> int {{ \
         0 }} }}\nfn pick(a: wide, b: wide) -> wide {{ max(a, b) }}\nfn main() -> int {{ 0 }}\n"
    );
    let mut files = Files::new();
    let ambient = files.add_source("ambient.ds", AMBIENT_SOURCE);
    let id = files.add_source("wide_pick.ds", &source);
    let asts = [
        parse(&files, ambient).expect("the ambient parses"),
        parse(&files, id).expect("the probe parses"),
    ];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the wide probe checks: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let Err(error) = lower(
        "wide_pick",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    ) else {
        panic!("257 staged words exceed the one-byte overlay vocabulary")
    };
    assert!(
        matches!(error, ds_lower::LowerError::FrameTooWide { .. }),
        "the typed width refusal precedes staging: {error}"
    );
}
