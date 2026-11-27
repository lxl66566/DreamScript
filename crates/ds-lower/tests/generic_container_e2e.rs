//! The generic container natives end to end [, phase 2]: `list_len`,
//! `list_get`, `list_concat` over list[int] and list[string] receivers,
//! `map_len`, `map_get`, and the free `len` over map[string, int] and
//! map[int, string] receivers,
//! all lower every call site to CALLG — the callee is the ambient generic's
//! ONE body and the staged words end in the evidence words naming the
//! receiver's container instance — and the checked reads answer their
//! hit and miss members. The container closes [phase 3] run the same
//! dispatch: `cons` (the evidence word names the element instance — int
//! and string both), `tail`, `collect`, the `first` checked
//! first element (the empty list answers none). A plain CALL never
//! reaches the reserved slots, so
//! the phase-1 name-routed monomorphic fallback is gone.

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

/// The routed natives' reserved slots: the FuncRef index, the real
/// arguments a site stages, the evidence words behind them, and the
/// builtin instances the evidence words name (the receiver's container,
/// the element for `cons` — the per-element spellings name their own instance).
const ROUTED: &[(u32, u8, u8, &[BuiltinInstance])] = &[
    (21, 1, 1, &[BuiltinInstance::List]), // list_len
    (22, 2, 1, &[BuiltinInstance::List]), // list_get
    (23, 2, 1, &[BuiltinInstance::List]), // list_concat
    (29, 1, 2, &[BuiltinInstance::Map]),  // map_len
    (30, 2, 2, &[BuiltinInstance::Map]),  // map_get
    (32, 2, 1, &[BuiltinInstance::Int, BuiltinInstance::Str]), // cons
    (34, 1, 1, &[BuiltinInstance::List]), // tail
    (35, 1, 1, &[BuiltinInstance::List]), // collect
    (36, 1, 1, &[BuiltinInstance::List]), // first
    (10, 1, 1, &[BuiltinInstance::Map]),  // len
];

/// Builds the module once per test VM: parse -> check -> lower -> write
/// -> read back -> install. Both native tables must precede the loader's
/// install (pool FuncRefs validate against installed tables); the std
/// platform table is this test's to provide, as any platform host must.
fn build_vm() -> (Vm, Lowered) {
    let source = include_str!("fixtures/generic_container_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("generic_container_probe.ds", source);
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
        "generic_container_probe",
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

/// The one acceptance: every routed native runs over both of its
/// receiver's element types with distinct outputs, and the checked reads
/// answer their hit and miss members in both spellings' shared path.
/// The closes add the cons/tail/collect compositions over int and
/// string elements and the empty list's none arm under `first`.
#[test]
fn container_natives_run_two_element_types_and_both_checked_arms() {
    let (mut vm, _) = build_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("generic_container_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns unit"
    );
    assert_eq!(
        vm.captured_output(),
        b"ints 3 6 3 0\ntexts 3 gamma ?\nnames 2 2 0\nschemes 1 http ?\n\
          pairs 2 1\nclosed 4 7 3\nheaded delta 3\ndropped 2 3 alpha\nblank 0 ?\n"
    );
}

/// The lowering shape is pinned: every routed native's call site emits
/// exactly one CALLG addressing the reserved generic slot with the
/// site's real-argument count, the evidence words (Kimm i64 instance
/// ids) staged immediately before it — and no plain CALL or CALLT
/// reaches the reserved slots, so the monomorphic name-route is gone.
#[test]
fn every_container_native_site_is_one_callg_with_container_evidence() {
    let (_, lowered) = build_vm();
    let mut sites = [0usize; ROUTED.len()];
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
                            !(module == "builtin" && ROUTED.iter().any(|r| r.0 == *func)),
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
                    assert_eq!(module, "builtin", "the generic bodies live in the core");
                    let slot = ROUTED
                        .iter()
                        .position(|r| r.0 == *func)
                        .expect("the Callg addresses a routed native's slot");
                    let (_, real_args, evidence, instances) = ROUTED[slot];
                    assert_eq!(*nargs, real_args, "nargs names the site's real arguments");
                    for back in 1..=usize::from(evidence) {
                        let Some(Inst::Uop(ds_abi::Uop::Kimm { kidx, .. })) =
                            i.checked_sub(back).and_then(|p| code.get(p))
                        else {
                            panic!("no evidence word staged before the Callg");
                        };
                        match &f.constants()[usize::from(kidx.0)] {
                            ds_mod::Constant::I64(id)
                                if instances.iter().any(|i| *i as i64 == *id) => {},
                            other => {
                                panic!("the staged evidence names a routed instance, got {other:?}")
                            },
                        }
                    }
                    sites[slot] += 1;
                },
                _ => {},
            }
        }
    }
    assert_eq!(
        sites,
        [7, 2, 2, 2, 2, 2, 3, 2, 2, 2],
        "one Callg per use: list_len seven (one UFCS), list_get and list_concat two, map_len and \
         map_get two, cons two, tail three (one UFCS), collect two, first two, len two (one UFCS \
         over each map)"
    );
}
