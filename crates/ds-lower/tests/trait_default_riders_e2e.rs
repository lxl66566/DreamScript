//! Trait defaults x staged evidence riders (F7 x H3): a static dispatch
//! into a generic impl body stages the trait's declared default among
//! its real arguments (the omitted-parameter spellings) with the pair's
//! rider area and the instance word trailing, and the transitive chain
//! omits the defaulted member at the nested sentinel site — the same
//! declared default stages there, the riders sliding one level per hop.

use ds_abi::Uop;
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, PlacedOp, lower};
use ds_mod::{Constant, FuncRefKey, Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

const SOURCE: &str = include_str!("fixtures/trait_default_riders_probe.ds");

/// Parse -> check -> lower; panics with the rendered diagnostics when an
/// earlier stage refuses.
fn lower_probe() -> Lowered {
    let mut files = Files::new();
    let id = files.add_source("trait_default_riders_probe.ds", SOURCE);
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
    lower(
        "trait_default_riders_probe",
        Hash128::compute(SOURCE.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers")
}

/// The lowered module installed into a fresh VM, output captured.
fn run_lowered(lowered: &Lowered) -> Vm {
    let bytes = write(lowered.module());
    let module = read_checked(&bytes, SOURCE.as_bytes()).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    vm
}

/// Every intersection answers through the trait's single declared
/// authority: the omitted default and the written argument agree through
/// both spellings with the riders trailing, and the transitive chain
/// stages the same default at the nested sentinel site.
#[test]
fn defaults_stage_inside_real_args_with_riders_trailing() {
    let lowered = lower_probe();
    let mut vm = run_lowered(&lowered);
    assert_eq!(
        vm.run("trait_default_riders_probe", "main", &[]),
        Ok(Value::I64(0)),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        concat!("5\n", "9\n", "5\n", "6\n").as_bytes(),
        "the omitted default and the written argument agree across spellings, the chain included"
    );
}

/// The staged shape: every dispatch uop names the receiver plus the
/// staged default as its real arguments (the omitted spellings stage the
/// declared literal), with the rider area trailing — at the entering
/// static sites into the generic body and at the nested sentinel sites
/// inside it alike.
#[test]
fn every_dispatch_names_receiver_and_default_with_riders_trailing() {
    let lowered = lower_probe();
    let index = lowered
        .function_index("sized#list[u]#pad")
        .expect("the generic pad body is planned");
    // the entering sites in main: three static dispatches into the
    // generic body, each staging receiver plus default (omitted or
    // written) as the real arguments
    let main = lowered.placed("main").expect("main lowers");
    let entering: Vec<u8> = main
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Uop(Uop::Callg { nargs, .. }) => Some(*nargs),
            _ => None,
        })
        .collect();
    assert_eq!(
        entering,
        vec![2, 2, 2, 2],
        "the receiver plus the default are the real arguments, riders trailing"
    );
    // the generic body's nested element call dispatches through the
    // sentinel with the element plus the staged default as its real
    // arguments (the follow chain trails them)
    let body = lowered
        .placed("sized#list[u]#pad")
        .expect("the body lowers");
    let pool = &lowered.module().functions()[index as usize].constants();
    let sentinels: Vec<u8> = body
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Uop(Uop::Callg { callee, nargs }) => match &pool[usize::from(callee.0)] {
                Constant::FuncRef {
                    key: FuncRefKey::Native(module),
                    ..
                } if module == "$evidence" => Some(*nargs),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        sentinels,
        vec![2],
        "the nested site stages the element plus the declared default, riders trailing"
    );
}
