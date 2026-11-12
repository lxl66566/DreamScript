//! Runtime evidence across spawned-child frames end to end: a spawn
//! lexically inside a generic impl member body stages the pair's whole
//! evidence area — the serving member body's slot plus its follow words
//! — from the enclosing frame's evidence slots into the child's entry
//! argument area beside the captures (the closure object's capture
//! block becomes the spawned fiber's entry arguments verbatim, so the
//! words ride the entry frame the VM writes). A lambda lexically
//! inside the spawned body stages the same words again from the child
//! frame's slots into its capture block, so a demand crosses two child
//! boundaries. The residual keeps its honest refusal in
//! `generic_impl_refusals.rs`: a serving body whose spawned subtrees
//! demand a different member than its own chain.

use ds_abi::{SlotType, Uop};
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, PlacedOp, lower};
use ds_mod::{Constant, Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

const SPAWN_PROBE: &str = include_str!("fixtures/spawn_evidence_probe.ds");
const NESTED_PROBE: &str = include_str!("fixtures/spawn_lambda_probe.ds");

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

/// The one spawned child lowered inside `member`, by table prefix: the
/// child's name adds exactly one `$` level (a lambda inside the child
/// adds another, so the prefix alone would over-match).
fn the_spawn_child(lowered: &Lowered, member: &str) -> String {
    let depth = member.matches('$').count() + 1;
    let mut names: Vec<String> = lowered
        .functions()
        .iter()
        .filter(|name| {
            name.starts_with(&format!("{member}$")) && name.matches('$').count() == depth
        })
        .cloned()
        .collect();
    assert_eq!(names.len(), 1, "one spawned child under {member}");
    names.remove(0)
}

/// The one evidence slot a sentinel CALLG/CALLTG inside `body`
/// dispatches from, or panics when the body dispatches nowhere.
fn evidence_slot(lowered: &Lowered, body: &str) -> u32 {
    let index = lowered.function_index(body).expect("the body is planned");
    let placed = lowered.placed(body).expect("the body lowers");
    let pool = &lowered.module().functions()[index as usize].constants();
    placed
        .iter()
        .find_map(|op| match op {
            PlacedOp::Uop(Uop::Callg { callee, .. } | Uop::Calltg { callee, .. }) => {
                match &pool[usize::from(callee.0)] {
                    Constant::FuncRef { module, func } if module == "$evidence" => Some(*func),
                    _ => None,
                }
            },
            _ => None,
        })
        .unwrap_or_else(|| panic!("{body} dispatches through its evidence slot"))
}

/// The spawned children run for both instantiations of the one body:
/// each child's `x.show()` answers through the entry-area evidence word
/// (the join drives children in spawn order over the shared captured
/// list), and the Ref words of the string run balance (the heap drains).
#[test]
fn spawned_children_run_the_evidence_shape() {
    let lowered = lower_probe("spawn_evidence_probe.ds", SPAWN_PROBE);
    let mut vm = run_lowered(SPAWN_PROBE, &lowered);
    assert_eq!(
        vm.run("probe", "main", &[]),
        Ok(Value::I64(0)),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        "n=1;n=2\na!;b!\n".as_bytes(),
        "both instantiations answer through the one spawned child body"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
    // the member body and its spawned child enter the table exactly once
    assert_eq!(
        lowered
            .functions()
            .iter()
            .filter(|name| name.starts_with("shows#list[t]#show"))
            .count(),
        2,
        "one member body plus one spawned child, no per-instantiation copies"
    );
}

/// The lowered evidence flow, pinned: the spawn site's closure
/// construction carries the captures plus the pair's whole evidence
/// area (the slot and two follow words), the child frame declares that
/// area as its trailing entry parameters (the entry argument area's
/// words), and the child's own dispatch reads the trailing slot.
#[test]
fn the_spawn_site_stages_the_entry_evidence_words() {
    let lowered = lower_probe("spawn_evidence_probe.ds", SPAWN_PROBE);
    let body = "shows#list[t]#show";
    let placed = lowered.placed(body).expect("the member body lowers");
    // the spawn site stages the child's entry area: n names the capture
    // block's words — two real captures (the element word and the shared
    // list) plus the pair's slot and two follow words
    let closures: Vec<u8> = placed
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Uop(Uop::ClosureNew { n, .. }) => Some(*n),
            _ => None,
        })
        .collect();
    assert_eq!(
        closures,
        vec![5],
        "one spawn over two captures and the pair's three-word evidence area"
    );
    // the child frame declares the area as trailing entry parameters:
    // params_len covers the captures plus the evidence words, all I64
    // trailing the element's raw word and the list's Ref word
    let child = the_spawn_child(&lowered, body);
    let index = lowered
        .function_index(&child)
        .expect("the spawned child is planned");
    let function = &lowered.module().functions()[index as usize];
    assert_eq!(
        function.layout().params_len(),
        5,
        "the entry argument area carries two captures plus the evidence area"
    );
    let slots = function.layout().slots();
    assert_eq!(
        slots[2..5],
        [SlotType::I64; 3],
        "the pair's slot and follow words bind as the entry area's trailing words"
    );
    // the child's nested element call dispatches from its own entry-area
    // slot — the sentinel names the trailing evidence word
    assert_eq!(
        evidence_slot(&lowered, &child),
        2,
        "the sentinel names the child frame's evidence word"
    );
}

/// A lambda inside the spawned body demands transitively: the spawn
/// carries the pair's area into the child's entry words, the lambda's
/// construction stages them again from the child frame's slot into its
/// capture block, and both instantiations run through the two child
/// boundaries.
#[test]
fn a_nested_lambda_inside_the_spawned_body_demands_transitively() {
    let lowered = lower_probe("spawn_lambda_probe.ds", NESTED_PROBE);
    let mut vm = run_lowered(NESTED_PROBE, &lowered);
    assert_eq!(
        vm.run("probe", "main", &[]),
        Ok(Value::I64(0)),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        "n=1+n=2\na!+b!\n".as_bytes(),
        "the lambda's demand crosses the entry area and the capture block"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
    // the child frame carries the area as its trailing entry words and
    // the lambda's closure carries them onward: its construction stages
    // no real captures, only the pair's three words
    let child = the_spawn_child(&lowered, "shows#list[t]#show");
    let placed = lowered.placed(&child).expect("the child lowers");
    let closures: Vec<u8> = placed
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Uop(Uop::ClosureNew { n, .. }) => Some(*n),
            _ => None,
        })
        .collect();
    assert_eq!(
        closures,
        vec![3],
        "the lambda's capture block carries the pair's area alone"
    );
    let mut lambdas: Vec<String> = lowered
        .functions()
        .iter()
        .filter(|name| name.starts_with(&format!("{child}$")))
        .cloned()
        .collect();
    assert_eq!(lambdas.len(), 1, "one lambda under the spawned child");
    assert_eq!(
        evidence_slot(&lowered, &lambdas.remove(0)),
        1,
        "the lambda dispatches from its own trailing capture slot"
    );
}
