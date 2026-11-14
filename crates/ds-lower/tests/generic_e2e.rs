//! The generic-call seam end to end [H3, phase 1]: `len` call sites over
//! string and list receivers lower to CALLG — the callee is the ambient
//! generic's ONE body and the staged words end in the instantiated-type
//! evidence word — the module round-trips through the writer and loader,
//! and the VM dispatches distinct outputs per receiver. The UFCS dot
//! spelling routes through the same site shape. A receiver no builtin
//! instance covers is a lowering error, never a runtime misdispatch.

use ds_abi::{BuiltinInstance, Inst};
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, Lowered, lower};
use ds_mod::{FuncRefKey, Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

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
/// -> read back -> install. Both native tables must precede the loader's
/// install (pool FuncRefs validate against installed tables); the std
/// platform table is this test's to provide, as any platform host must.
fn build_vm() -> (Vm, Lowered) {
    let source = include_str!("fixtures/generic_len_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("generic_len_probe.ds", source);
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
        "generic_len_probe",
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

/// Lowers one inline source, returning the lowering error verbatim.
fn lower_error(source: &'static str) -> LowerError {
    let mut files = Files::new();
    let id = files.add_source("probe.ds", source);
    let asts = [parse(&files, id).expect("parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the negative probe checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    match lower(
        "probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    ) {
        Err(error) => error,
        Ok(_) => panic!("the negative probe must not lower"),
    }
}

/// The one acceptance: `len` over a string counts chars, over a list
/// counts elements — distinct outputs from the same generic, in both
/// the free-call and UFCS spellings.
#[test]
fn len_dispatches_string_and_list_receivers_distinctly() {
    let (mut vm, _) = build_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("generic_len_probe", "main", &[]),
        Ok(Value::I64(0)),
        "main returns unit"
    );
    assert_eq!(
        vm.captured_output(),
        b"string 11 11\nlist 3 3\nmixed 11 3\n"
    );
}

/// The lowering shape is pinned: every `len` use emits exactly one CALLG
/// whose pool callee is the reserved generic slot, stage count 1 (the
/// receiver), and the immediately preceding uop stages the evidence word
/// — an i64 instance id from the builtin vocabulary.
#[test]
fn len_sites_lower_to_one_callg_with_instance_evidence() {
    let (_, lowered) = build_vm();
    let mut sites = 0;
    for f in lowered.module().functions() {
        let code = insts(f);
        for (i, uop) in code.iter().enumerate() {
            let Inst::Uop(ds_abi::Uop::Callg { callee, nargs }) = uop else {
                continue;
            };
            sites += 1;
            assert_eq!(*nargs, 1, "len stages exactly its receiver");
            assert_eq!(
                f.constants()[usize::from(callee.0)],
                ds_mod::Constant::FuncRef {
                    key: FuncRefKey::Native("builtin".into()),
                    func: 10,
                },
                "the callee addresses the generic len slot"
            );
            let Some(Inst::Uop(ds_abi::Uop::Kimm { kidx, .. })) =
                i.checked_sub(1).and_then(|p| code.get(p))
            else {
                panic!("no evidence word staged before the Callg");
            };
            match &f.constants()[usize::from(kidx.0)] {
                ds_mod::Constant::I64(id)
                    if *id == BuiltinInstance::Str as i64
                        || *id == BuiltinInstance::List as i64 => {},
                other => panic!("the staged evidence is a builtin instance id, got {other:?}"),
            }
        }
    }
    assert_eq!(sites, 6, "one CALLG per len use");
}

/// A receiver no builtin instance covers never reaches the VM: the call
/// is a lowering error naming the covered vocabulary (`bool` stays
/// uncovered — the numeric scalars joined the instance set with the
/// container closes).
#[test]
fn len_over_an_uncovered_type_is_a_lowering_error() {
    let error = lower_error(concat!(
        "tag bool\n",
        "tag int\n",
        "tag string\n",
        "extern fn println (s: string) -> ()\n",
        "extern fn len[t] (xs: t) -> int\n",
        "fn main() -> () {\n",
        "  println(\"${len(true)}\")\n",
        "}\n",
    ));
    let LowerError::Unsupported { reason, .. } = &error else {
        panic!("the uncovered receiver must report Unsupported, got {error:?}")
    };
    assert!(
        reason.contains("no builtin instance"),
        "the error names the uncovered instantiation: {error}"
    );
}

/// `len` implements the string and list instances only: a map receiver
/// names an instance the one body covers nothing of — a lowering error,
/// never a runtime misdispatch through the list instance.
#[test]
fn len_over_a_map_receiver_is_a_lowering_error() {
    let error = lower_error(concat!(
        "tag int\n",
        "tag string\n",
        "struct map[k, v]\n",
        "extern fn println (s: string) -> ()\n",
        "extern fn len[t] (xs: t) -> int\n",
        "fn main() -> () {\n",
        "  const m = map[string, int]()\n",
        "  println(\"${len(m)}\")\n",
        "}\n",
    ));
    let LowerError::Unsupported { reason, .. } = &error else {
        panic!("the uncovered receiver must report Unsupported, got {error:?}")
    };
    assert!(
        reason.contains("implements no instance"),
        "the error names the uncovered instance: {error}"
    );
}
