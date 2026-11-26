//! The ord-routed list sort end to end over a no-ambient universe
//! (todo item 101): the `sort[t: ord]` extern's call sites route the
//! comparator out of the solved element type's serving ord instance —
//! the checker's per-call bound record answers the instance, the
//! lowering mints the instance's `compare` body as the callable word
//! staged beside the list `Ref` (the receiver's list-instance evidence
//! word trailing them like filter's), and the VM's stable merge sort
//! reenters the dispatcher per comparison through the reduce seam's
//! CALLG discipline. Every site lowers to exactly one CALLG addressing
//! the reserved generic slot with the two real arguments, and a
//! constant compare (every string "equal") keeps the input order —
//! the stability pin. A declaration whose signature disagrees with the
//! native's staged word contract refuses at the free-generic name
//! routing instead of lowering a call that would trap at the CALLG's
//! arity check.

use ds_abi::{BuiltinInstance, Inst};
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, lower};
use ds_mod::{FuncRefKey, Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// The record's body decoded to instructions, one per pc.
fn insts(function: &ds_mod::Function) -> Vec<Inst> {
    function
        .code()
        .iter()
        .enumerate()
        .map(|(pc, word)| {
            Inst::decode(u32::try_from(pc).expect("pc fits u32"), *word)
                .expect("ds-mod validated every word")
        })
        .collect()
}

/// Builds the module once per test VM: parse -> check -> lower -> write
/// -> read back -> install. Both native tables must precede the
/// loader's install (pool FuncRefs validate against installed tables).
fn build_vm() -> (Vm, Lowered) {
    let source = include_str!("fixtures/sort_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("sort_probe.ds", source);
    let asts = [parse(&files, id).expect("the fixture parses")];
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
        "sort_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    (Vm::new(registry), lowered)
}

/// One refusal probe: the source must check clean, and lowering must
/// report the named seam.
fn refused(source: &str, needle: &str) {
    let mut files = Files::new();
    let id = files.add_source("refusal_probe.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the probe checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let Err(error) = lower(
        "refusal_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    ) else {
        panic!("the probe refuses at lowering")
    };
    let text = format!("{error}");
    assert!(
        text.contains(needle),
        "the refusal names `{needle}`: {text}"
    );
}

/// The acceptance: both spellings sort the int run, the constant
/// string compare keeps the input order (stability), and the empty
/// list answers the empty list — the heap draining with the frame.
#[test]
fn the_ord_routed_sort_runs_over_the_module_local_spelling() {
    let (mut vm, _) = build_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("sort_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns unit"
    );
    assert_eq!(
        vm.captured_output(),
        b"free 1 1 2 3\nufcs 1 3\nwords 3\nempty 0\n"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// The lowering shape is pinned: every sort site emits exactly one
/// CALLG addressing the reserved generic slot with the two real
/// arguments (the list word and the compare callable), the receiver's
/// list-instance evidence word staged immediately before it — and no
/// plain CALL or CALLT reaches the reserved slot.
#[test]
fn every_sort_site_is_one_callg_with_list_evidence() {
    let (_, lowered) = build_vm();
    let mut sites = 0usize;
    for f in lowered.module().functions() {
        let code = insts(f);
        for (i, uop) in code.iter().enumerate() {
            let Inst::Uop(uop) = uop else {
                continue;
            };
            match uop {
                ds_abi::Uop::Call { callee, .. } | ds_abi::Uop::Callt { callee, .. } => {
                    if let ds_mod::Constant::FuncRef {
                        key: FuncRefKey::Native(module),
                        func,
                    } = &f.constants()[usize::from(callee.0)]
                    {
                        assert!(
                            !(module == native::BUILTIN_MODULE && *func == native::BUILTIN_SORT),
                            "a plain call must not name-route the reserved slot {func}"
                        );
                    }
                },
                ds_abi::Uop::Callg { callee, nargs } => {
                    let ds_mod::Constant::FuncRef {
                        key: FuncRefKey::Native(module),
                        func,
                    } = &f.constants()[usize::from(callee.0)]
                    else {
                        panic!("the Callg callee must be a pool FuncRef");
                    };
                    if module != native::BUILTIN_MODULE || *func != native::BUILTIN_SORT {
                        continue;
                    }
                    sites += 1;
                    assert_eq!(*nargs, 2, "nargs names the list word and the callable");
                    let Some(Inst::Uop(ds_abi::Uop::Kimm { kidx, .. })) =
                        i.checked_sub(1).and_then(|p| code.get(p))
                    else {
                        panic!("no evidence word staged before the Callg");
                    };
                    match &f.constants()[usize::from(kidx.0)] {
                        ds_mod::Constant::I64(id) if *id == BuiltinInstance::List as i64 => {},
                        other => {
                            panic!("the staged evidence names the list instance, got {other:?}")
                        },
                    }
                },
                _ => {},
            }
        }
    }
    assert_eq!(sites, 4, "the fixture's four sort sites each lower once");
}

/// A module-local declaration whose signature disagrees with the
/// native's staged word contract refuses at the free-generic name
/// routing: the wider spelling stages extra words the CALLG's
/// contract carries no place for, so the call would trap at the
/// body's arity check instead of lowering.
#[test]
fn a_mismatched_signature_refuses_at_the_name_routing() {
    refused(
        concat!(
            "tag int\n",
            "tag string\n",
            "tag bool\n",
            "struct list[t]\n",
            "pub trait ord {\n",
            "  fn compare(self, other: Self) -> int\n",
            "}\n",
            "impl ord for int {\n",
            "  fn compare(self, other: Self) -> int { 0 }\n",
            "}\n",
            "extern fn sort[t] (xs: list[t], k: int, j: int) -> list[t]\n",
            "\n",
            "fn main() -> int {\n",
            "  const ys = sort([3, 1], 0, 0)\n",
            "  0\n",
            "}\n",
        ),
        "no lowering",
    );
}
