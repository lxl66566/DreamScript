//! Generic-impl transitive evidence end to end [H3]: a nested dispatch
//! whose serving body is itself a generic impl's member. The entering
//! site stages one evidence word per demanded pair — the index of an
//! evidence-chain object carrying the serving body's table index plus
//! one tail chain per the body's own demands — and a nested runtime
//! dispatch reads its frame's slot word, resolves the chain object, and
//! stages the tail objects' indices as the serving body's trailing
//! evidence parameters (a static table read per hop, so depth is
//! unbounded); a nested static site inside a generic body (its record
//! the runtime form) stages the callee's words verbatim from its own
//! frame slots.

use ds_abi::{SlotType, Uop};
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, PlacedOp, lower};
use ds_mod::{Constant, FuncRefKey, Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

const SOURCE: &str = include_str!("fixtures/transitive_impl_probe.ds");

/// Parse -> check -> lower; panics with the rendered diagnostics when an
/// earlier stage refuses.
fn lower_probe() -> Lowered {
    let mut files = Files::new();
    let id = files.add_source("transitive_impl_probe.ds", SOURCE);
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
        "transitive_impl_probe",
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

/// The one acceptance: every transitive shape answers through the one
/// body per member — the two-level tells->shows chain over int and
/// string elements, the direct entering chain one and two follow words
/// deep, and the mixed-branch run — and the Ref elements balance through
/// the runtime dispatches (the heap drains).
#[test]
fn transitive_chains_run_through_one_body_per_member() {
    let lowered = lower_probe();
    let mut vm = run_lowered(&lowered);
    assert_eq!(
        vm.run("transitive_impl_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        concat!(
            "t:l:n=1;\n",
            "t:l:a!;\n",
            "t:l:l:n=1;;\n",
            "l:l:n=1;;\n",
            "l:l:l:n=1;;;\n",
            "t:l:l:a!;;l:b!;;\n",
        )
        .as_bytes(),
        "every transitive shape answers through the staged chains"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// The generic body's frame claims one evidence word per demanded pair
/// as a trailing I64 parameter (the receiver plus the chain word here),
/// so the entering CALLGs name the real arguments and the runtime
/// sentinel dispatches stage the tail chains themselves.
#[test]
fn the_generic_body_claims_the_pair_area_as_trailing_parameters() {
    let lowered = lower_probe();
    let index = lowered
        .function_index("shows#list[t]#show")
        .expect("the generic show body is planned");
    let function = &lowered.module().functions()[index as usize];
    assert_eq!(
        function.layout().params_len(),
        2,
        "the receiver plus the pair's one chain word"
    );
    assert!(
        function.layout().slots()[1..2]
            .iter()
            .all(|slot| *slot == SlotType::I64),
        "the evidence word binds as the trailing I64 parameter"
    );
    // the entering sites: one CALLG per direct show/tell call, each
    // naming its real arguments (the receiver) with the pair area and
    // the instance word riding past
    let main = lowered.placed("main").expect("main lowers");
    let calls: Vec<&PlacedOp> = main
        .iter()
        .filter(|op| matches!(op, PlacedOp::Uop(Uop::Callg { .. })))
        .collect();
    assert_eq!(calls.len(), 6, "one dispatch uop per call site");
    for op in &calls {
        let PlacedOp::Uop(Uop::Callg { nargs, .. }) = op else {
            unreachable!("filtered above");
        };
        assert_eq!(*nargs, 1, "the receiver is the one real argument");
    }
    // the generic body's nested element call dispatches through the
    // sentinel with the chain's tail words staged by the dispatch itself
    // (nargs names the element alone)
    let body = lowered
        .placed("shows#list[t]#show")
        .expect("the body lowers");
    let pool = &lowered.module().functions()[index as usize].constants();
    let sentinels: Vec<u8> = body
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Uop(Uop::Callg { callee, nargs }) => match &pool[usize::from(callee.0)] {
                Constant::FuncRef {
                    key: FuncRefKey::Native(module),
                    func,
                } if module == "$evidence" => Some(*nargs),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        sentinels,
        vec![1],
        "the nested element call dispatches from the frame's slot, the chain's tails trailing"
    );
}

/// A generic body whose own frame demands two members runs at depth
/// zero: the entering site stages one evidence word per demand pair and
/// the frame binds one slot per pair, so each nested dispatch reads its
/// own slot — and the transitive level answers the same way now (one
/// tail chain per the serving body's own demands, pinned below).
#[test]
fn a_body_demanding_two_members_runs_at_depth_zero() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "struct list[t]\n",
        "extern fn println (s: string) -> ()\n",
        "extern fn str_int (x: int) -> string\n",
        "trait shows {\n",
        "  fn show(self) -> string\n",
        "  fn size(self) -> int { 1 }\n",
        "}\n",
        "impl shows for int {\n",
        "  fn show(self) -> string { \"n\" }\n",
        "  fn size(self) -> int { 1 }\n",
        "}\n",
        "impl [t: shows] shows for list[t] {\n",
        "  fn show(self) -> string {\n",
        "    var acc = \"l\"\n",
        "    for x in self {\n",
        "      acc = \"${acc}${x.show()}${str_int(x.size())}\"\n",
        "    }\n",
        "    acc\n",
        "  }\n",
        "}\n",
        "fn main() -> int {\n",
        "  println([1].show())\n",
        "  0\n",
        "}\n",
    );
    let mut files = Files::new();
    let id = files.add_source("two_demand_probe.ds", source);
    let asts = [parse(&files, id).expect("the two-demand probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the two-demand probe checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "two_demand_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the two-demand probe lowers");
    let mut vm = {
        let bytes = write(lowered.module());
        let module = read_checked(&bytes, source.as_bytes()).expect("the module round-trips");
        let mut registry = ModuleRegistry::new();
        registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
        registry.install_platform(Platform::std());
        loader::install(&mut registry, module).expect("the module installs");
        let mut vm = Vm::new(registry);
        vm.capture_output();
        vm
    };
    assert_eq!(
        vm.run("two_demand_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        "ln1\n".as_bytes(),
        "both members answer through the body's own two slots"
    );
    // the body claims both words: the receiver plus one chain word per
    // demand pair
    let index = lowered
        .function_index("shows#list[t]#show")
        .expect("the generic show body is planned");
    let function = &lowered.module().functions()[index as usize];
    assert_eq!(
        function.layout().params_len(),
        3,
        "the receiver plus one chain word per demand pair"
    );
}

/// A body whose second demand is collected from a value-site lambda runs
/// at depth zero: the walk collects lambda bodies transitively, the frame
/// stages one evidence area per demand pair, and the lambda's
/// construction copies its own pair's area into the capture block — so a
/// direct call of the evidence-carrying closure enters its Raw-parameter
/// body with the element staged Raw, the ABI's wildcard pairing. The
/// transitive level of the same body keeps its refusal (pinned in
/// `generic_impl_refusals.rs`).
#[test]
fn a_lambda_collected_demand_runs_at_depth_zero() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "struct list[t]\n",
        "extern fn println (s: string) -> ()\n",
        "trait shows {\n",
        "  fn show(self) -> string\n",
        "  fn size(self) -> int\n",
        "}\n",
        "impl shows for int {\n",
        "  fn show(self) -> string { \"n\" }\n",
        "  fn size(self) -> int { 1 }\n",
        "}\n",
        "impl [t: shows] shows for list[t] {\n",
        "  fn show(self) -> string {\n",
        "    var acc = \"l\"\n",
        "    for x in self {\n",
        "      const f = (e: t) => e.size()\n",
        "      acc = \"${acc}${f(x)}${x.show()}\"\n",
        "    }\n",
        "    acc\n",
        "  }\n",
        "  fn size(self) -> int { 1 }\n",
        "}\n",
        "fn main() -> int {\n",
        "  println([1].show())\n",
        "  0\n",
        "}\n",
    );
    let mut files = Files::new();
    let id = files.add_source("lambda_demand_probe.ds", source);
    let asts = [parse(&files, id).expect("the lambda-demand probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the lambda-demand probe checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "lambda_demand_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the lambda-demand probe lowers");
    let mut vm = {
        let bytes = write(lowered.module());
        let module = read_checked(&bytes, source.as_bytes()).expect("the module round-trips");
        let mut registry = ModuleRegistry::new();
        registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
        registry.install_platform(Platform::std());
        loader::install(&mut registry, module).expect("the module installs");
        let mut vm = Vm::new(registry);
        vm.capture_output();
        vm
    };
    assert_eq!(
        vm.run("lambda_demand_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        "l1n\n".as_bytes(),
        "the lambda's demand answers through its capture-carried area"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// One source checked, lowered, round-tripped, installed, and run: the
/// shared positive probe of the lifted transitive shapes.
fn lands(source: &str, stdout: &[u8]) {
    let mut files = Files::new();
    let id = files.add_source("probe.ds", source);
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
    let lowered = lower(
        "probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the probe lowers");
    let bytes = write(lowered.module());
    let module = read_checked(&bytes, source.as_bytes()).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run("probe", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0)))
    );
    assert_eq!(vm.captured_output(), stdout);
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// The chain-table pin over the fixture: the serialized module carries
/// one chain object per staged word, the direct `[[1]].show()` site's
/// chain naming the generic serving body whose own tail names the
/// concrete int body — the shape a nested dispatch walks one hop at a
/// time.
#[test]
fn the_module_carries_the_evidence_chain_table() {
    let lowered = lower_probe();
    let show_list = lowered
        .function_index("shows#list[t]#show")
        .expect("the generic show body is planned");
    let bytes = write(lowered.module());
    let module = read_checked(&bytes, SOURCE.as_bytes()).expect("the module round-trips");
    let chains = module.chains();
    // the `[[1]].show()` and `[[[1]]].show()` sites stage the chain whose
    // serving body is the generic list body; its one tail chain ends at
    // the concrete int body, and the deeper site's chain shares both
    // entries through the walk's memo
    let generic: Vec<u32> = chains
        .iter()
        .enumerate()
        .filter(|(_, chain)| chain.func == show_list)
        .map(|(index, _)| u32::try_from(index).expect("chain index fits u32"))
        .collect();
    assert!(
        generic.len() >= 2,
        "the depth-1 and depth-2 entering sites each stage a chain over the generic body"
    );
    for index in generic {
        // walk the chain to its end: one tail per the body's one demand,
        // each hop naming the level's serving body, the concrete int body
        // last
        let mut hop = index;
        let mut hops = 0;
        while !chains[hop as usize].next.is_empty() {
            assert_eq!(
                chains[hop as usize].next.len(),
                1,
                "the generic body's one demand rides one tail chain"
            );
            hop = chains[hop as usize].next[0];
            hops += 1;
            assert!(hops < 8, "the chain terminates");
        }
        assert_ne!(
            chains[hop as usize].func, show_list,
            "the walked chain ends at a concrete serving body"
        );
    }
}

/// A transitive chain deeper than any fixed frame area: the table read
/// is one hop per dispatch, so five-level nestings lower and run with
/// the same one-word evidence parameters at every level.
#[test]
fn a_deep_chain_runs_past_any_frame_bound() {
    let deep = concat!(
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
        "impl[t: shows] shows for list[t] {\n",
        "  fn show(self) -> string {\n",
        "    var acc = \"l:\"\n",
        "    for x in self {\n",
        "      acc = concat(acc, concat(x.show(), \";\"))\n",
        "    }\n",
        "    acc\n",
        "  }\n",
        "}\n",
        "fn main() -> int {\n",
        "  println([[[[1]]]].show())\n",
        "  println([[[[[1]]]]].show())\n",
        "  0\n",
        "}\n",
    );
    lands(
        deep,
        concat!("l:l:l:l:n=1;;;;\n", "l:l:l:l:l:n=1;;;;;\n",).as_bytes(),
    );
}

/// A transitive serving body demanding two members directly: one tail
/// chain per the body's own demands, so the nested dispatch into it
/// stages both words — the same body at depth zero is pinned above.
#[test]
fn a_transitive_body_demanding_two_members_runs() {
    lands(
        concat!(
            "tag int\n",
            "tag string\n",
            "struct list[t]\n",
            "extern fn println (s: string) -> ()\n",
            "extern fn str_int (x: int) -> string\n",
            "trait shows {\n",
            "  fn show(self) -> string\n",
            "  fn size(self) -> int { 1 }\n",
            "}\n",
            "impl shows for int {\n",
            "  fn show(self) -> string { \"n\" }\n",
            "  fn size(self) -> int { 1 }\n",
            "}\n",
            "impl [t: shows] shows for list[t] {\n",
            "  fn show(self) -> string {\n",
            "    var acc = \"l\"\n",
            "    for x in self {\n",
            "      acc = \"${acc}${x.show()}${str_int(x.size())}\"\n",
            "    }\n",
            "    acc\n",
            "  }\n",
            "}\n",
            "fn main() -> int {\n",
            "  println([[1]].show())\n",
            "  0\n",
            "}\n",
        ),
        b"lln11\n",
    );
}

/// A transitive serving body whose one single demand is a different
/// member: the tail chain keys the demanded member's own chain, so the
/// mutual tells/shows spelling runs — the shallower form refuses at
/// checking (no instance serves the bare element's bound), keeping the
/// transitive depth as the reachable form.
#[test]
fn a_transitive_body_demanding_a_single_different_member_runs() {
    lands(
        concat!(
            "tag int\n",
            "tag string\n",
            "struct list[t]\n",
            "extern fn println (s: string) -> ()\n",
            "trait tells {\n",
            "  fn tell(self) -> string\n",
            "}\n",
            "trait shows {\n",
            "  fn show(self) -> string\n",
            "}\n",
            "impl tells for int {\n",
            "  fn tell(self) -> string { \"n\" }\n",
            "}\n",
            "impl [u: tells] shows for list[u] {\n",
            "  fn show(self) -> string {\n",
            "    var acc = \"l\"\n",
            "    for x in self {\n",
            "      acc = \"${acc}${x.tell()}\"\n",
            "    }\n",
            "    acc\n",
            "  }\n",
            "}\n",
            "impl [v: shows] tells for list[v] {\n",
            "  fn tell(self) -> string {\n",
            "    var acc = \"t\"\n",
            "    for x in self {\n",
            "      acc = \"${acc}${x.show()}\"\n",
            "    }\n",
            "    acc\n",
            "  }\n",
            "}\n",
            "fn main() -> int {\n",
            "  println([[1]].tell())\n",
            "  0\n",
            "}\n",
        ),
        b"tln\n",
    );
}

/// A transitive serving body whose second demand is collected from a
/// value-site lambda: the nested dispatch stages both chain words and
/// the lambda's construction copies its own pair's word from the serving
/// body's frame.
#[test]
fn a_transitive_lambda_demand_runs() {
    lands(
        concat!(
            "tag int\n",
            "tag string\n",
            "struct list[t]\n",
            "extern fn println (s: string) -> ()\n",
            "trait shows {\n",
            "  fn show(self) -> string\n",
            "  fn size(self) -> int\n",
            "}\n",
            "impl shows for int {\n",
            "  fn show(self) -> string { \"n\" }\n",
            "  fn size(self) -> int { 1 }\n",
            "}\n",
            "impl [t: shows] shows for list[t] {\n",
            "  fn show(self) -> string {\n",
            "    var acc = \"l\"\n",
            "    for x in self {\n",
            "      const f = (e: t) => e.size()\n",
            "      acc = \"${acc}${f(x)}${x.show()}\"\n",
            "    }\n",
            "    acc\n",
            "  }\n",
            "  fn size(self) -> int { 1 }\n",
            "}\n",
            "fn main() -> int {\n",
            "  println([[1]].show())\n",
            "  0\n",
            "}\n",
        ),
        b"l1l1n\n",
    );
}

/// A transitive serving body whose second demand is collected from a
/// spawned subtree: the child's entry area carries its own pair's chain
/// word staged from the serving body's frame.
#[test]
fn a_transitive_spawned_demand_runs() {
    lands(
        concat!(
            "tag int\n",
            "tag string\n",
            "struct list[t]\n",
            "extern fn println (s: string) -> ()\n",
            "extern fn str_int (x: int) -> string\n",
            "trait shows {\n",
            "  fn show(self) -> string\n",
            "  fn size(self) -> int { 1 }\n",
            "}\n",
            "impl shows for int {\n",
            "  fn show(self) -> string { \"n\" }\n",
            "  fn size(self) -> int { 1 }\n",
            "}\n",
            "impl [t: shows] shows for list[t] {\n",
            "  fn show(self) -> string {\n",
            "    var acc = \"l\"\n",
            "    for x in self {\n",
            "      acc = \"${acc}${x.show()}\"\n",
            "    }\n",
            "    nursery {\n",
            "      for x in self {\n",
            "        spawn { println(str_int(x.size())) }\n",
            "      }\n",
            "    }\n",
            "    acc\n",
            "  }\n",
            "}\n",
            "fn main() -> int {\n",
            "  println([[1]].show())\n",
            "  0\n",
            "}\n",
        ),
        // the inner invocation (x.show() over [1]) runs its own nursery
        // too — the ONE body serves every level
        concat!("1\n", "1\n", "lln\n").as_bytes(),
    );
}

/// A transitive serving body whose second demand is collected from a
/// callee: the demand walk keys the free generic fn's own demand onto
/// the serving body's frame, and the nested dispatch stages its chain
/// word with the rest.
#[test]
fn a_transitive_callee_demand_runs() {
    lands(
        concat!(
            "tag int\n",
            "tag string\n",
            "struct list[t]\n",
            "extern fn println (s: string) -> ()\n",
            "extern fn str_int (x: int) -> string\n",
            "trait shows {\n",
            "  fn show(self) -> string\n",
            "  fn size(self) -> int { 1 }\n",
            "}\n",
            "impl shows for int {\n",
            "  fn show(self) -> string { \"n\" }\n",
            "}\n",
            "fn size_word[t: shows] (x: t) -> string { str_int(x.size()) }\n",
            "impl [t: shows] shows for list[t] {\n",
            "  fn show(self) -> string {\n",
            "    var acc = \"l\"\n",
            "    for x in self {\n",
            "      acc = \"${acc}${x.show()}${size_word(x)}\"\n",
            "    }\n",
            "    acc\n",
            "  }\n",
            "}\n",
            "fn main() -> int {\n",
            "  println([[1]].show())\n",
            "  0\n",
            "}\n",
        ),
        b"lln11\n",
    );
}
