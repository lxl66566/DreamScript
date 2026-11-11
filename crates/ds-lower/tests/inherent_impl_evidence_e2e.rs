//! Generic inherent impls end to end: a generic impl's member bodies
//! lower once per impl under the `<target>#<member>` mangle, and every
//! solved dot-call entering them stages the per-demand evidence words —
//! the serving member body's table index per the checker-recorded
//! bounds — trailing the arguments, where the body claims them as its
//! trailing evidence parameters. An inherent impl has no instance id,
//! so no dispatch uop exists: the ordinary CALL carries the words, and
//! a member-call tail transfers through CALLT with the full staged
//! count, the natural consequence of the plain-local-callee identity.
//! Nested lambdas inside the body carry their demands' evidence words
//! through the capture block; the transitive shapes keep their honest
//! refusals.

use ds_abi::{SlotType, Uop};
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, PlacedOp, lower};
use ds_mod::{Constant, Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Value, Vm, loader, native};

const SOURCE: &str = include_str!("fixtures/inherent_impl_evidence_probe.ds");

/// Parse -> check -> lower; panics with the rendered diagnostics when an
/// earlier stage refuses.
fn lower_probe() -> (String, Lowered) {
    let mut files = Files::new();
    let id = files.add_source("inherent_impl_evidence_probe.ds", SOURCE);
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
        "inherent_impl_evidence_probe",
        Hash128::compute(SOURCE.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers");
    (SOURCE.to_owned(), lowered)
}

/// The lowered module installed into a fresh VM, output captured.
fn run_lowered(source: &str, lowered: &Lowered) -> Vm {
    let bytes = write(lowered.module());
    let module = read_checked(&bytes, source.as_bytes()).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_native(native::STD_PLATFORM_MODULE, native::std_platform_module());
    loader::install(&mut registry, &module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    vm
}

/// One placed sequence's uop kinds, RC ops in place.
fn shape(lowered: &Lowered, name: &str) -> Vec<String> {
    let placed = lowered
        .placed(name)
        .unwrap_or_else(|| panic!("{name} is planned (table: {:?})", lowered.functions()));
    placed
        .iter()
        .map(|op| match op {
            PlacedOp::Uop(uop) => {
                let repr = format!("{uop:?}");
                repr.chars().take_while(char::is_ascii_alphabetic).collect()
            },
            PlacedOp::Rc(_) => "Rc".to_owned(),
        })
        .collect()
}

/// The generic member shape runs for both instantiations of the one
/// body: the loop's `x.show()` answers through the frame's evidence
/// slot, the render lambda's demand rides its capture block, and the
/// Ref elements of the string run balance — the heap drains.
#[test]
fn two_instantiations_run_the_generic_member_shape() {
    let (source, lowered) = lower_probe();
    let mut vm = run_lowered(&source, &lowered);
    assert_eq!(
        vm.run("inherent_impl_evidence_probe", "main", &[]),
        Ok(Value::I64(0)),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        "list:n=1;n=2;\nlist:a!;b!;\nn=1;n=2\na!;b!\n".as_bytes(),
        "both members answer through the one body per member"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
    // one body per member plus the render lambda, never per instantiation
    assert!(
        lowered.functions().contains(&"list[t]#describe".to_owned()),
        "the describe body plans under its mangle (table: {:?})",
        lowered.functions()
    );
    assert!(
        lowered
            .functions()
            .iter()
            .filter(|name| name.starts_with("list[t]#render"))
            .count()
            == 2,
        "the render body plus its one lambda, no per-instantiation copies (table: {:?})",
        lowered.functions()
    );
}

/// The entering sites stage the serving member body's table index per
/// the body's demand — a Kimm word trailing the receiver — and the call
/// is the ordinary CALL: an inherent impl has no instance word, so no
/// CALLG exists at an inherent site, and the uop names the full staged
/// count (the receiver plus the evidence word).
#[test]
fn entering_sites_stage_the_evidence_words_ahead_of_plain_call() {
    let (_, lowered) = lower_probe();
    let main = lowered.function_index("main").expect("main is planned");
    let placed = lowered.placed("main").expect("main lowers");
    let pool = &lowered.module().functions()[main as usize].constants();
    let int_body = i64::from(
        lowered
            .function_index("shows#int#show")
            .expect("the int member body is planned"),
    );
    let string_body = i64::from(
        lowered
            .function_index("shows#string#show")
            .expect("the string member body is planned"),
    );
    // the staged evidence words immediately before each member CALL: one
    // serving body per entering site, in call order (describe int,
    // describe string, render int, render string)
    let mut evidence: Vec<i64> = Vec::new();
    let mut pending: Vec<i64> = Vec::new();
    for op in placed {
        match op {
            PlacedOp::Uop(Uop::Kimm { kidx, .. }) => match pool[usize::from(kidx.0)] {
                Constant::I64(value) => pending.push(value),
                _ => pending.clear(),
            },
            PlacedOp::Uop(Uop::Call { .. }) => {
                evidence.append(&mut pending);
                pending.clear();
            },
            _ => pending.clear(),
        }
    }
    assert!(
        pending.is_empty(),
        "no staged word survives past its call: {pending:?}"
    );
    assert_eq!(
        evidence,
        vec![int_body, string_body, int_body, string_body],
        "each entering site stages the serving body its demand calls"
    );
    // no dispatch uop exists at an inherent site
    let main_shape = shape(&lowered, "main");
    assert!(
        !main_shape.iter().any(|k| k == "Callg"),
        "no CALLG at an inherent site: {main_shape:?}"
    );
}

/// The member body claims its evidence word as a trailing parameter: the
/// declared receiver plus one I64 parameter per recorded demand, and the
/// nested runtime dispatch reads the word from its frame slot through
/// the sentinel CALLG.
#[test]
fn the_member_body_claims_its_evidence_parameter() {
    let (_, lowered) = lower_probe();
    let index = lowered
        .function_index("list[t]#describe")
        .expect("the describe body is planned");
    let function = &lowered.module().functions()[index as usize];
    assert_eq!(
        function.layout().params_len(),
        2,
        "the receiver plus the one evidence word"
    );
    assert_eq!(
        function.layout().slots()[1],
        SlotType::I64,
        "the evidence word binds as the trailing I64 parameter"
    );
    let placed = lowered.placed("list[t]#describe").expect("the body lowers");
    let pool = &lowered.module().functions()[index as usize].constants();
    let dispatched: Vec<u32> = placed
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Uop(Uop::Callg { callee, .. }) => match &pool[usize::from(callee.0)] {
                Constant::FuncRef { module, func } if module == "$evidence" => Some(*func),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        dispatched,
        vec![1],
        "the nested element call dispatches from the frame's evidence slot (word 1, past the \
         receiver)"
    );
}

/// A member-call tail transfers through CALLT with the evidence word
/// staged: the solved inherent body is a plain local callee, so the tail
/// recognition covers a generic member exactly like a monomorphic one,
/// and the frame's exit claims stage before the words (a claim's overlay
/// window would clobber words staged earlier). The run completes at a
/// depth a growing stack could not, and the max stack stays constant.
#[test]
fn a_member_call_tail_transfers_through_callt_with_the_evidence_word() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "struct list[t]\n",
        "extern fn println (s: string) -> ()\n",
        "extern fn concat (a: string, b: string) -> string\n",
        "trait shows {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "impl shows for int {\n",
        "  fn show(self) -> string { \"n=$self\" }\n",
        "}\n",
        "impl[t: shows] list[t] {\n",
        "  fn describe(self) -> string {\n",
        "    var acc = \"list:\"\n",
        "    for x in self {\n",
        "      acc = \"${acc}${x.show()};\"\n",
        "    }\n",
        "    acc\n",
        "  }\n",
        "}\n",
        "fn spin(n: int, xs: list[int]) -> string {\n",
        "  if n == 0 { xs.describe() } else { spin(n - 1, xs) }\n",
        "}\n",
        "fn main() -> int {\n",
        "  println(spin(100_000, [1, 2]))\n",
        "  0\n",
        "}\n",
    );
    let mut files = Files::new();
    let id = files.add_source("tail_probe.ds", source);
    let asts = [parse(&files, id).expect("the tail probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the tail probe checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "tail_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the tail probe lowers");
    let spin = shape(&lowered, "spin");
    assert!(
        spin.iter().any(|k| k == "Callt"),
        "the member tail transfers through CALLT: {spin:?}"
    );
    let mut vm = run_lowered(source, &lowered);
    assert_eq!(
        vm.run("tail_probe", "main", &[]),
        Ok(Value::I64(0)),
        "the run completes at depth 100_000"
    );
    assert_eq!(
        vm.captured_output(),
        "list:n=1;n=2;\n".as_bytes(),
        "the transferred member call answers the base case"
    );
    assert!(
        vm.max_stack_len() < 64,
        "the stack stays constant across every slide ({} words)",
        vm.max_stack_len()
    );
}
