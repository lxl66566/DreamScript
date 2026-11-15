//! Runtime evidence across lambda captures end to end: a lambda
//! lexically inside a generic impl member body carries its bound
//! demands' evidence words in its capture block — the construction
//! stages them from the enclosing frame's evidence slots, and every
//! frame the callable enters (the hosted per-element call) copies them
//! into the trailing capture slots where the sentinel CALLG
//! dispatches from. Nested lambdas stage transitively, one word
//! crossing two capture boundaries; the spawned-child crossing (the
//! same words riding a child's entry argument area) runs in
//! `spawn_evidence_e2e.rs`.

use ds_abi::{SlotType, Uop};
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, PlacedOp, lower};
use ds_mod::{Constant, FuncRefKey, Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

const MAP_PROBE: &str = include_str!("fixtures/generic_lambda_map_probe.ds");
const FILTER_PROBE: &str = include_str!("fixtures/generic_lambda_filter_probe.ds");
const NESTED_PROBE: &str = include_str!("fixtures/generic_lambda_nested_probe.ds");

/// Parse -> check -> lower, refusing on any earlier stage's
/// diagnostics.
fn lower_probe(name: &str, source: &str) -> Lowered {
    let mut files = Files::new();
    let id = files.add_source(name, source);
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
    lower(
        "probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the probe lowers")
}

/// The lowered module installed into a fresh VM, output captured.
fn run_lowered(source: &str, lowered: &Lowered) -> Vm {
    let bytes = write(lowered.module());
    let module = read_checked(&bytes, source.as_bytes()).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    vm
}

/// One mangled member body plus the mangled lambda bodies lowered
/// inside it, by name prefix.
fn table_names(lowered: &Lowered, prefix: &str) -> Vec<String> {
    lowered
        .functions()
        .iter()
        .filter(|name| name.starts_with(prefix))
        .cloned()
        .collect()
}

/// The one lambda body lowered inside `member`, by table prefix.
fn the_lambda(lowered: &Lowered, member: &str) -> String {
    let mut names = table_names(lowered, &format!("{member}$"));
    assert_eq!(names.len(), 1, "one lambda under {member}");
    names.remove(0)
}

/// The one evidence slot a sentinel CALLG/CALLTG inside `body` dispatches
/// from (the tail form transfers through the same sentinel), or panics
/// when the body dispatches nowhere.
fn evidence_slot(lowered: &Lowered, body: &str) -> u32 {
    let index = lowered.function_index(body).expect("the body is planned");
    let placed = lowered.placed(body).expect("the body lowers");
    let pool = &lowered.module().functions()[index as usize].constants();
    placed
        .iter()
        .find_map(|op| match op {
            PlacedOp::Uop(Uop::Callg { callee, .. } | Uop::Calltg { callee, .. }) => {
                match &pool[usize::from(callee.0)] {
                    Constant::FuncRef {
                        key: FuncRefKey::Native(module),
                        func,
                    } if module == "$evidence" => Some(*func),
                    _ => None,
                }
            },
            _ => None,
        })
        .unwrap_or_else(|| panic!("{body} dispatches through its evidence slot"))
}

/// The map shape runs for both instantiations of the one body: the
/// lambda's `x.show()` answers through the capture-carried evidence
/// word, and the Ref elements of the string run balance (the heap
/// drains).
#[test]
fn two_instantiations_run_the_lambda_evidence_shape() {
    let lowered = lower_probe("generic_lambda_map_probe.ds", MAP_PROBE);
    let mut vm = run_lowered(MAP_PROBE, &lowered);
    assert_eq!(
        vm.run("probe", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        "n=1;n=2\na!;b!\n".as_bytes(),
        "both instantiations answer through the one lambda body"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
    // the member body and its lambda each enter the table exactly once
    assert_eq!(
        table_names(&lowered, "shows#list[t]#show").len(),
        2,
        "one member body plus one lambda, no per-instantiation copies"
    );
}

/// The lowered evidence flow, pinned: the member body's closure
/// construction carries exactly the lambda's evidence word (the pair's
/// chain word, no real captures), the entering site stages the chain
/// object plus the declared instance ahead of its CALLG, and the lambda
/// body's own dispatch reads the trailing capture slot its frame bound
/// (the `x` parameter at word 0, the evidence slot at word 1).
#[test]
fn the_capture_block_carries_the_lambda_evidence_word() {
    let lowered = lower_probe("generic_lambda_map_probe.ds", MAP_PROBE);
    let body = "shows#list[t]#show";
    let placed = lowered.placed(body).expect("the member body lowers");
    // the closure construction stages the lambda's evidence word: n
    // names the capture block's words, and there are no real captures
    let closures: Vec<u8> = placed
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Uop(Uop::ClosureNew { n, .. }) => Some(*n),
            _ => None,
        })
        .collect();
    assert_eq!(
        closures,
        vec![1],
        "one closure over the pair's one chain word"
    );
    // the entering sites in main stage the chain word then the declared
    // instance, in Kimm words right before each CALLG
    let main = lowered.function_index("main").expect("main is planned");
    let placed = lowered.placed("main").expect("main lowers");
    let pool = &lowered.module().functions()[main as usize].constants();
    let mut evidence: Vec<Vec<i64>> = Vec::new();
    let mut pending: Vec<i64> = Vec::new();
    for op in placed {
        match op {
            PlacedOp::Uop(Uop::Kimm { kidx, .. }) => match pool[usize::from(kidx.0)] {
                Constant::I64(value) => pending.push(value),
                _ => pending.clear(),
            },
            PlacedOp::Uop(Uop::Callg { .. }) => {
                evidence.push(pending.clone());
                pending.clear();
            },
            _ => pending.clear(),
        }
    }
    let int_body = lowered
        .function_index("shows#int#show")
        .expect("the int member body is planned");
    let string_body = lowered
        .function_index("shows#string#show")
        .expect("the string member body is planned");
    let instance = i64::from(2); // shows int, shows string, shows list[t]
    assert_eq!(evidence.len(), 2, "one staged span per entering site");
    let chains = lowered.module().chains();
    for (site, span) in evidence.iter().enumerate() {
        assert_eq!(
            span.len(),
            2,
            "the chain word then the instance trail the receiver"
        );
        assert_eq!(span[1], instance, "the declared instance rides last");
        let chain = chains
            .get(usize::try_from(span[0]).expect("chain index fits usize"))
            .expect("the staged word names a chain object");
        let expected = if site == 0 { int_body } else { string_body };
        assert_eq!(chain.func, expected, "the chain names the serving body");
        assert!(
            chain.next.is_empty(),
            "the concrete serving body demands nothing"
        );
    }
    // the lambda body dispatches from its trailing capture slot: the
    // parameter at word 0, the evidence word at word 1
    let lambda = the_lambda(&lowered, "shows#list[t]#show");
    assert_eq!(
        evidence_slot(&lowered, &lambda),
        1,
        "the sentinel names the lambda frame's evidence capture slot"
    );
}

/// The lambda frame's slot table declares the evidence word: one
/// parameter, then the I64 capture word right where the CALLS copy-in
/// writes it.
#[test]
fn the_lambda_frame_declares_the_evidence_capture_word() {
    let lowered = lower_probe("generic_lambda_map_probe.ds", MAP_PROBE);
    let lambda = the_lambda(&lowered, "shows#list[t]#show");
    let index = lowered
        .function_index(&lambda)
        .expect("the lambda is planned");
    let function = &lowered.module().functions()[index as usize];
    assert_eq!(
        function.layout().params_len(),
        1,
        "the lambda's parameters stay the one `x` word"
    );
    let slots = function.layout().slots();
    assert_eq!(
        slots[0],
        SlotType::Raw,
        "the type-variable parameter is the raw runtime word"
    );
    assert_eq!(
        slots[1],
        SlotType::I64,
        "the evidence word binds as the trailing capture word"
    );
}

/// Sibling evidence-carrying lambdas: a filter predicate and a map
/// body in one member share the frame's one evidence slot (the two
/// demands deduplicate), and the predicate's length test diverges per
/// instantiation — the int elements pass, the string elements drop.
#[test]
fn a_filter_predicate_demands_evidence_through_its_own_lambda() {
    let lowered = lower_probe("generic_lambda_filter_probe.ds", FILTER_PROBE);
    let mut vm = run_lowered(FILTER_PROBE, &lowered);
    assert_eq!(
        vm.run("probe", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        "n=1;n=2\n\n".as_bytes(),
        "the ints pass the predicate, the strings drop to the empty line"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
    // both lambdas dispatch from the same recorded pair: one evidence
    // slot per member frame, shared by the filter and the map lambda
    let mut slots = table_names(&lowered, "keeps#list[t]#long$")
        .iter()
        .map(|lambda| evidence_slot(&lowered, lambda))
        .collect::<Vec<u32>>();
    slots.sort_unstable();
    slots.dedup();
    assert_eq!(
        slots,
        vec![1],
        "both sibling lambdas read their own trailing capture slot (the raw parameter at word 0, \
         the evidence word at word 1)"
    );
}

/// Nested lambdas stage the evidence transitively: the outer lambda's
/// capture block carries the word from the member frame, the inner
/// lambda's carries it from the outer's frame, and both instantiations
/// run through the two capture boundaries.
#[test]
fn nested_lambdas_stage_evidence_transitively() {
    let lowered = lower_probe("generic_lambda_nested_probe.ds", NESTED_PROBE);
    let mut vm = run_lowered(NESTED_PROBE, &lowered);
    assert_eq!(
        vm.run("probe", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        "n=1;n=2\na!;b!\n".as_bytes(),
        "the inner lambda's demand crosses both capture boundaries"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}
