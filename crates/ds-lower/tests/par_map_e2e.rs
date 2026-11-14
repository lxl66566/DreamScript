//! The `par_map` lowering seam end to end [E3]: the fork call site lowers
//! to CALLG — one site staging (list, callable) plus the two List evidence
//! words — and the callable's struct-typed answer rides the fiber
//! completion channel whole, the module round-tripping through the writer
//! and loader and joining on the VM. A capturing closure callable runs
//! the same site: the staged Ref word is the caller's borrow, the fork
//! staging the capture block per spawn into the child's entry frame past
//! the element's arguments.

use ds_abi::Inst;
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::{FuncRefKey, Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Vm, loader, native};

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

/// Builds the VM once per test: parse -> check -> lower -> write -> read
/// back -> install. Both native tables must precede the loader's install
/// (pool FuncRefs validate against installed tables).
fn build_vm() -> Vm {
    let source = include_str!("fixtures/par_map_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("par_map_probe.ds", source);
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
        "par_map_probe",
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
    Vm::new(registry)
}

#[test]
fn the_wide_answer_site_lowers_round_trips_and_joins() {
    let mut vm = build_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("par_map_probe", "main", &[]),
        Ok(ds_vm::Value::I64(0)),
        "main returns unit"
    );
    // the wide answers assemble in input order: element 0 the pair
    // (2, 3), element 4 the pair (10, 15), and the empty input answers
    // the empty result list
    assert_eq!(
        vm.captured_output(),
        b"first 2 3 last 10 15 len 5\nempty 0\n"
    );
}

/// The site shape is pinned: exactly one CALLG staging (list, callable)
/// — the callable a scalar `Fn` word — with the two List evidence words
/// riding the overlay past the staged span.
#[test]
fn the_callg_stages_the_list_callable_and_two_evidence_words() {
    let source = include_str!("fixtures/par_map_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("par_map_probe.ds", source);
    let asts = [parse(&files, id).expect("the fixture parses")];
    let analyzed = analyze(&files, &asts);
    let lowered = lower(
        "par_map_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers");
    let mut sites = 0;
    for f in lowered.module().functions() {
        let code = insts(f);
        for (i, uop) in code.iter().enumerate() {
            let Inst::Uop(ds_abi::Uop::Callg { callee, nargs }) = uop else {
                continue;
            };
            // the checked reads and `len` are CALLG sites too; only the
            // fork sites stage the callable
            if f.constants()[usize::from(callee.0)]
                != (ds_mod::Constant::FuncRef {
                    key: FuncRefKey::Native("builtin".into()),
                    func: native::BUILTIN_PAR_MAP,
                })
            {
                continue;
            }
            sites += 1;
            assert_eq!(*nargs, 2, "the fork stages list, callable");
            // the two List evidence words ride the staged span's end
            for back in [1usize, 2] {
                let Some(Inst::Uop(ds_abi::Uop::Kimm { kidx, .. })) =
                    i.checked_sub(back).and_then(|p| code.get(p))
                else {
                    panic!("no evidence word staged before the Callg");
                };
                assert!(matches!(
                    &f.constants()[usize::from(kidx.0)],
                    ds_mod::Constant::I64(_)
                ));
            }
            // the callable stages as the scalar Fn word: the span's last
            // argument slot moves a body slot an earlier Kconst loaded
            // from a FuncRef constant
            let Some(Inst::Uop(ds_abi::Uop::Mov {
                src: callable_slot, ..
            })) = i.checked_sub(3).and_then(|p| code.get(p))
            else {
                panic!("no callable word staged before the Callg");
            };
            let loaded = code.iter().take(i).find_map(|inst| match inst {
                Inst::Uop(ds_abi::Uop::Kconst { dst, kidx }) if *dst == *callable_slot => {
                    Some((kidx, *dst))
                },
                _ => None,
            });
            let Some((kidx, _)) = loaded else {
                panic!("the callable slot has no FuncRef load");
            };
            assert!(
                matches!(
                    &f.constants()[usize::from(kidx.0)],
                    ds_mod::Constant::FuncRef { .. }
                ),
                "the callable is the scalar Fn word"
            );
        }
    }
    assert_eq!(sites, 2, "one wide par_map site plus the empty-input site");
}

#[test]
fn a_capturing_closure_callable_lowers_round_trips_and_forks() {
    // the closure site: the staged callable word is the closure's Ref
    // (the caller's borrow), the fork staging the capture block per
    // spawn — the answers assemble in input order
    let source = include_str!("fixtures/par_map_closure_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("par_map_closure_probe.ds", source);
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
        "par_map_closure_probe",
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
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run("par_map_closure_probe", "main", &[]),
        Ok(ds_vm::Value::I64(0)),
        "main returns unit"
    );
    // [1, 2, 3] through the closure capturing bias 10: 11, 12, 13
    assert_eq!(vm.captured_output(), b"first 11 last 13 len 3\n");
}
