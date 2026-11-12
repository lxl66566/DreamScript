//! The omitted-defaulted dispatch end to end [B6, H3]: a trait member
//! declaring a default body makes the impl's member omittable, and the
//! dispatch resolves into the trait's declared default body — the same
//! CALLG staging an impl body enters (the evidence words the default
//! body's nested calls demand trail the true arguments, the solved
//! instance word trailing them). The default body is the ONE generic
//! shape over the trait's own `Self` variable: the receiver stages as
//! the variable's single Raw word, a sibling call on `self` dispatches
//! at runtime through the staged evidence slot (the checker satisfied
//! it through the trait-scoped `Self` assumption, recording no
//! instance), and a generic impl's spelled sibling body rides the
//! follow chain the entering site stages.

use ds_abi::Uop;
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, PlacedOp, lower};
use ds_mod::{Constant, Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

const SOURCE: &str = include_str!("fixtures/trait_default_body_probe.ds");

/// Parse -> check -> lower; panics with the rendered diagnostics when an
/// earlier stage refuses.
fn lower_probe() -> Lowered {
    let mut files = Files::new();
    let id = files.add_source("trait_default_body_probe.ds", SOURCE);
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
        "trait_default_body_probe",
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

/// The omitted-defaulted dispatch runs end to end: every call site whose
/// solving impl spells no member dispatches into the trait's declared
/// default body, the monomorphic and the generic serving impls alike,
/// and the heap drains when the run ends.
#[test]
fn omitted_defaulted_dispatches_into_the_declared_default_body() {
    let lowered = lower_probe();
    let mut vm = run_lowered(&lowered);
    assert_eq!(
        vm.run("trait_default_body_probe", "main", &[]),
        Ok(Value::I64(0)),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        concat!(
            "<int>\n",
            "<str>\n",
            "<[int]>\n",
            "<[[int]]>\n",
            "3\n",
            "<int>\n"
        )
        .as_bytes(),
        "the default body answers for the monomorphic impls, the generic impl's spelled sibling \
         rides the follow chain (flat and nested), and the Self-typed return moves through"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// The staged evidence shape: every dispatch uop names the receiver as
/// its whole real-argument count (a defaulted member with no default
/// parameters stages the receiver alone), and the Kimms before each
/// CALLG stage the serving member bodies' indexes plus their follow
/// chains, the solved declared-instance word trailing — the same staged
/// shape a generic impl body's entering sites stage. The default body's
/// own sibling demand stages the spelled impl member under the site's
/// receiver (the int and string bodies directly, the generic list body
/// with its follow chain naming the element's serving body).
#[test]
fn dispatches_stage_serving_bodies_and_the_instance_word() {
    let lowered = lower_probe();
    let int_name = i64::from(
        lowered
            .function_index("shows#int#name")
            .expect("the int member body is planned"),
    );
    let string_name = i64::from(
        lowered
            .function_index("shows#string#name")
            .expect("the string member body is planned"),
    );
    let list_name = i64::from(
        lowered
            .function_index("shows#list[u]#name")
            .expect("the generic member body is planned"),
    );
    let main = lowered.function_index("main").expect("main is planned");
    let placed = lowered.placed("main").expect("main lowers");
    let pool = &lowered.module().functions()[main as usize].constants();
    let mut nargs: Vec<u8> = Vec::new();
    let mut evidence: Vec<Vec<i64>> = Vec::new();
    let mut pending: Vec<i64> = Vec::new();
    for op in placed {
        match op {
            PlacedOp::Uop(Uop::Kimm { kidx, .. }) => match pool[usize::from(kidx.0)] {
                Constant::I64(value) => pending.push(value),
                _ => pending.clear(),
            },
            PlacedOp::Uop(Uop::Callg { nargs: count, .. }) => {
                nargs.push(*count);
                evidence.push(pending.clone());
                pending.clear();
            },
            _ => pending.clear(),
        }
    }
    // the declared instances, in declaration order: shows int (0), shows
    // string (1), shows list[u] (2), resets int (3)
    assert_eq!(
        nargs,
        vec![1, 1, 1, 1, 1],
        "each dispatch names the receiver alone as its real argument"
    );
    assert_eq!(
        evidence,
        vec![
            vec![int_name, int_name, int_name, 0],
            vec![string_name, string_name, string_name, 1],
            vec![list_name, int_name, int_name, 2],
            vec![list_name, list_name, int_name, 2],
            vec![3],
        ],
        "the default body's sibling demand stages the serving member body plus its follow chain, \
         the declared instance trailing; the default-free reset body stages the instance alone"
    );
}

/// The default body's nested sibling call dispatches at runtime through
/// the frame's evidence slot: the CALLG carries the sentinel FuncRef
/// naming the slot (the receiver `self` is the one Raw word, so the
/// pair's area starts at word 1) and names the receiver alone — the
/// riders trail it for the serving body the slot names.
#[test]
fn the_sibling_call_dispatches_through_the_default_bodys_evidence_slot() {
    let lowered = lower_probe();
    let body = lowered
        .function_index("shows#show")
        .expect("the declared default body is planned");
    let placed = lowered
        .placed("shows#show")
        .expect("the default body lowers");
    let pool = &lowered.module().functions()[body as usize].constants();
    let sentinel = placed.iter().find_map(|op| match op {
        PlacedOp::Uop(Uop::Callg { callee, nargs }) => match &pool[usize::from(callee.0)] {
            Constant::FuncRef { module, func } if module == "$evidence" => Some((*func, *nargs)),
            _ => None,
        },
        _ => None,
    });
    let (slot, nargs) = sentinel.expect("the sibling call dispatches through the evidence slot");
    assert_eq!(slot, 1, "the sentinel names the body's evidence slot");
    assert_eq!(nargs, 1, "the sibling call stages the receiver alone");
}

/// The dispatch resolves into the declared default body itself: the
/// CALLG callee FuncRef names the trait's planned default body, not an
/// impl member — the omitted-member shape the completeness pass keeps
/// clean and the lowering previously refused at its backstop seam.
#[test]
fn the_dispatch_targets_the_planned_default_body() {
    let lowered = lower_probe();
    let default = lowered
        .function_index("shows#show")
        .expect("the declared default body is planned");
    let reset_default = lowered
        .function_index("resets#reset")
        .expect("the reset default body is planned");
    let main = lowered.function_index("main").expect("main is planned");
    let placed = lowered.placed("main").expect("main lowers");
    let pool = &lowered.module().functions()[main as usize].constants();
    let targets: Vec<u32> = placed
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Uop(Uop::Callg { callee, .. }) => match &pool[usize::from(callee.0)] {
                Constant::FuncRef { module, func } if module.is_empty() => Some(*func),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        targets,
        vec![default, default, default, default, reset_default],
        "every omitted-member dispatch names the trait's declared default body"
    );
}

/// A tail dispatch into the declared default body transfers through
/// CALLTG — the dispatch resolution over the CALLT frame-reuse slide,
/// its evidence and instance words staged after the exit claims — so
/// the frame reuses instead of growing the stack.
#[test]
fn a_tail_dispatch_into_a_default_body_transfers() {
    let lowered = lower_probe();
    let placed = lowered.placed("label").expect("the tail-site fn lowers");
    assert!(
        placed
            .iter()
            .any(|op| matches!(op, PlacedOp::Uop(Uop::Calltg { .. }))),
        "the tail dispatch into the default body transfers through CALLTG"
    );
}

/// A multi-word receiver refuses at the entering site: the default body
/// lowers its `Self` receiver as the variable's single Raw word, so a
/// two-word struct receiver would slide a staged evidence rider into the
/// missing field word — the one-word instantiation boundary the free
/// generic bodies already guard, refused here where the type is
/// concrete.
#[test]
fn a_multiword_receiver_refuses_at_the_one_word_boundary() {
    let source = concat!(
        "tag int\n",
        "struct point { x: int, y: int }\n",
        "trait sized {\n",
        "  fn area(self) -> int { 0 }\n",
        "}\n",
        "impl sized for point {\n",
        "}\n",
        "fn main() -> int {\n",
        "  point { x: 1, y: 2 }.area()\n",
        "}\n",
    );
    let mut files = Files::new();
    let id = files.add_source("multiword_default.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the omitted defaulted member checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let Err(err) = lower(
        "multiword_default",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    ) else {
        panic!("the two-word receiver must refuse at the entering site")
    };
    assert!(
        err.to_string()
            .contains("the substituted type spans more than one word"),
        "the refusal names the one-word instantiation boundary: {err}"
    );
}
