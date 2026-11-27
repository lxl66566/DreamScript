//! The omitted-defaulted dispatch end to end: a trait member
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
use ds_mod::{Constant, FuncRefKey, Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

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
fn run_lowered(lowered: &Lowered, source: &str) -> Vm {
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

/// The omitted-defaulted dispatch runs end to end: every call site whose
/// solving impl spells no member dispatches into the trait's declared
/// default body, the monomorphic and the generic serving impls alike,
/// and the heap drains when the run ends.
#[test]
fn omitted_defaulted_dispatches_into_the_declared_default_body() {
    let lowered = lower_probe();
    let mut vm = run_lowered(&lowered, SOURCE);
    assert_eq!(
        vm.run("trait_default_body_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
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
    // each staged span is one chain word per the entered body's demand
    // plus the declared instance; the resets default body demands
    // nothing, so its dispatch stages the instance alone
    let chains = lowered.module().chains();
    assert_eq!(evidence.len(), 5, "one staged span per dispatch");
    let expected_bodies = [
        Some(int_name),
        Some(string_name),
        Some(list_name),
        Some(list_name),
        None,
    ];
    let expected_instances = [0i64, 1, 2, 2, 3];
    // the first site's receiver is a scalar literal the staging writes
    // straight into the overlay, so its own Kimm rides the span ahead of
    // the evidence words; every span ends chain-word-then-instance
    let expected_lens = [3usize, 2, 2, 2, 2];
    for (site, span) in evidence.iter().enumerate() {
        assert_eq!(
            span.last(),
            Some(&expected_instances[site]),
            "the declared instance trails site {site}"
        );
        assert_eq!(
            span.len(),
            expected_lens[site],
            "site {site} stages its receiver word, then the evidence words"
        );
        let Some(&expected) = expected_bodies[site].as_ref() else {
            continue;
        };
        let chain = chains
            .get(usize::try_from(span[span.len() - 2]).expect("chain index fits usize"))
            .expect("the staged word names a chain object");
        assert_eq!(
            i64::from(chain.func),
            expected,
            "site {site} stages the chain of its demand's serving body"
        );
    }
    // the third dispatch's chain (into the generic list name body)
    // carries the tail chain the nested dispatch slides, ending at the
    // concrete body; the fourth's walks one level deeper, sharing it
    let shallow = &chains[usize::try_from(evidence[2][0]).expect("chain index fits usize")];
    let deep = &chains[usize::try_from(evidence[3][0]).expect("chain index fits usize")];
    assert_eq!(
        shallow.next.len(),
        1,
        "the generic body's own demand rides one tail chain"
    );
    assert_eq!(
        i64::from(chains[shallow.next[0] as usize].func),
        int_name,
        "the shallow tail chain ends at the concrete int body"
    );
    assert_eq!(
        i64::from(chains[deep.next[0] as usize].func),
        list_name,
        "the deep tail chain names the list body again"
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
            Constant::FuncRef {
                key: FuncRefKey::Native(module),
                func,
            } if module == "$evidence" => Some((*func, *nargs)),
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
/// clean and the lowering's backstop seam admits.
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
                Constant::FuncRef {
                    key: FuncRefKey::Dep(_),
                    func,
                } => Some(*func),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let names = |targets: &[u32]| {
        targets
            .iter()
            .map(|&index| {
                lowered.module().functions()[index as usize]
                    .name()
                    .to_owned()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(targets.len(), 5, "one dispatch per defaulted call in main");
    assert_eq!(
        targets.iter().filter(|&&t| t == default).count(),
        0,
        "no omitted-member dispatch needs the ONE body: every written receiver is concrete"
    );
    let show_names = names(&targets[..4]);
    assert!(
        show_names
            .iter()
            .all(|name| name.starts_with("shows#show@")),
        "every show dispatch names its per-shape default instance: {show_names:?}"
    );
    assert_eq!(
        show_names[2], show_names[3],
        "the two list shapes share the default's list instance: the body bakes only the Raw \
         receiver word (every shape decision rides the staged evidence)"
    );
    assert_ne!(
        show_names[0], show_names[1],
        "the int and string shapes take distinct instances"
    );
    assert!(
        show_names[0] != show_names[2],
        "the scalar shapes take their own instances"
    );
    assert!(
        names(&targets[4..])[0].starts_with("resets#reset@"),
        "the reset dispatch names the default's int instance"
    );
    assert_ne!(reset_default, default);
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

/// A two-word receiver enters its default body's per-shape instance:
/// the body lays out over the receiver's whole span, where the ONE
/// Raw-word body refused it at the one-word boundary.
#[test]
fn a_multiword_receiver_enters_its_default_instance() {
    // the per-shape instance lifted the ONE body's one-word receiver
    // boundary: a two-word receiver lowers and runs over its whole span,
    // a default body demanding a sibling on `self` still refusing at the
    // chain's one-word boundary (the demand's serving body receives the
    // receiver as one Raw word)
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
    let lowered = lower(
        "multiword_default",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the two-word receiver enters its default instance");
    let mut vm = run_lowered(&lowered, source);
    assert_eq!(
        vm.run("multiword_default", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns the default body's area"
    );
}

// The generic-trait half of the surface: a trait declaring its own type
// parameters (`trait tells[t: shows]`) defaults a member whose body
// demands on `t` — a non-Self pair. The entering site derives the
// substitution from the impl's trait reference (the solved instance
// names the impl, whose reference argument fixes what `t` maps to), and
// the demand chains through the ordinary transitive machinery.

/// The shared story: the bound trait, the generic trait defaulting its
/// member over `t`, and the serving impls.
const GENERIC_STORY: &str = concat!(
    "tag int\n",
    "tag string\n",
    "struct list[t]\n",
    "extern fn println (s: string) -> ()\n",
    "trait shows {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "trait tells[t: shows] {\n",
    "  fn tell(self, other: t) -> string { other.show() }\n",
    "}\n",
    "impl shows for int {\n",
    "  fn show(self) -> string { \"int!\" }\n",
    "}\n",
);

/// Parse and check one inline source, asserting the checker's stage is
/// clean.
fn check_clean(name: &str, source: &str) -> ([ds_parse::Ast; 1], ds_check::Analyzed) {
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
    (asts, analyzed)
}

/// Lowers a checked-clean inline source, refusing with the expected
/// sentence.
fn refuse_lowering(name: &str, source: &str, expected: &str) {
    let (asts, analyzed) = check_clean(name, source);
    let Err(err) = lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed) else {
        panic!("the shape must refuse at lowering");
    };
    assert!(
        matches!(err, ds_lower::LowerError::Unsupported { .. }),
        "an unsupported boundary: {err}"
    );
    assert!(
        err.to_string().contains(expected),
        "the refusal names the seam: {err}"
    );
}

/// A generic trait's omitted member dispatches through the default body
/// whose body demands the trait's own variable, the impl substituting it
/// concretely (`impl tells[int] for string` fixes `t` to `int`): the
/// entering site stages the chain over the int serving body, the default
/// body's nested call rides the frame slot, and the run answers green
/// with the heap drained.
#[test]
fn a_concrete_trait_var_substitution_runs_through_the_default_body() {
    let source = format!(
        "{}{}",
        GENERIC_STORY,
        concat!(
            "impl tells[int] for string {\n",
            "}\n",
            "fn main() -> int {\n",
            "  println(\"a\".tell(3))\n",
            "  0\n",
            "}\n",
        )
    );
    let name = "tells_concrete.ds";
    let (asts, analyzed) = check_clean(name, &source);
    let lowered = lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed)
        .expect("the concrete substitution lowers");
    let mut vm = run_lowered(&lowered, &source);
    assert_eq!(
        vm.run(name, "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        "int!\n".as_bytes(),
        "the default body's demand on the trait's own variable answers through the substituted \
         serving body"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// The trait's variable mapping to the impl's own variable chains
/// transitively: `impl[u: shows] tells[u] for list[u]` fixes `t` to `u`,
/// the receiver binds `u`, and the impl's declared bound carries the
/// next level exactly as the generic-impl branch's step — a list-of-list
/// receiver walks the generic shows body whose own tail chain ends at
/// the int body.
#[test]
fn an_impl_var_trait_substitution_chains_transitively() {
    let source = format!(
        "{}{}",
        GENERIC_STORY,
        concat!(
            "impl[v: shows] shows for list[v] {\n",
            "  fn show(self) -> string {\n",
            "    var acc = \"[\"\n",
            "    for x in self {\n",
            "      acc = \"${acc}${x.show()}\"\n",
            "    }\n",
            "    \"${acc}]\"\n",
            "  }\n",
            "}\n",
            "impl[u: shows] tells[u] for list[u] {\n",
            "}\n",
            "fn main() -> int {\n",
            "  println([3].tell(4))\n",
            "  println([[3]].tell([7]))\n",
            "  0\n",
            "}\n",
        )
    );
    let name = "tells_impl_var.ds";
    let (asts, analyzed) = check_clean(name, &source);
    let lowered = lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed)
        .expect("the impl-var substitution lowers");
    let mut vm = run_lowered(&lowered, &source);
    assert_eq!(
        vm.run(name, "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        "int!\n[int!]\n".as_bytes(),
        "the trait variable resolves through the impl's own variable and chains transitively \
         through the generic serving body"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// A default body calling an omitted sibling chains the sibling's own
/// default body: each demand's tail names the serving body of the member
/// it calls under the same substitution, so the sibling chain terminates
/// at the concrete default — the cyclic-table story belongs to a
/// self-recursive demand alone.
#[test]
fn a_default_body_calling_an_omitted_sibling_chains_the_siblings_default() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "extern fn println (s: string) -> ()\n",
        "trait chain3 {\n",
        "  fn a(self) -> string { self.b() }\n",
        "  fn b(self) -> string { self.c() }\n",
        "  fn c(self) -> string { \"c!\" }\n",
        "}\n",
        "impl chain3 for int {\n",
        "}\n",
        "fn main() -> int {\n",
        "  println(1.a())\n",
        "  0\n",
        "}\n",
    );
    let name = "sibling_chain.ds";
    let (asts, analyzed) = check_clean(name, source);
    let lowered = lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed)
        .expect("the sibling chain lowers");
    // the chain staged for `a`'s Self demand names `b`'s default body,
    // whose own tail names `c`'s — never `b`'s own entry again
    let body_b = i64::from(
        lowered
            .function_index("chain3#b")
            .expect("b's default body is planned"),
    );
    let body_c = i64::from(
        lowered
            .function_index("chain3#c")
            .expect("c's default body is planned"),
    );
    let chains = lowered.module().chains();
    let [outer, inner] = chains else {
        panic!("the sibling chain builds two chain objects, found {chains:?}")
    };
    assert_eq!(i64::from(outer.func), body_b, "the outer chain serves b");
    assert_eq!(
        i64::from(chains[outer.next[0] as usize].func),
        body_c,
        "the tail chain serves c"
    );
    assert_eq!(
        inner.next,
        Vec::<u32>::new(),
        "the concrete default demands nothing"
    );
    let mut vm = run_lowered(&lowered, source);
    assert_eq!(
        vm.run(name, "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        "c!\n".as_bytes(),
        "the sibling chain walks a to b to c and terminates at the concrete default"
    );
}

/// A concrete substitution no instance serves keeps its honest refusal:
/// the trait demands its bound at the substituted type, and the chain
/// build resolves one body per bound.
#[test]
fn an_unserved_concrete_substitution_refuses_at_the_chain_build() {
    refuse_lowering(
        "tells_unserved.ds",
        &format!(
            "{}{}",
            GENERIC_STORY,
            concat!(
                "impl tells[string] for int {\n",
                "}\n",
                "fn main() -> int {\n",
                "  println(1.tell(\"b\"))\n",
                "  0\n",
                "}\n",
            )
        ),
        "the trait variable's substituted type has no serving instance at this site",
    );
}

/// The trait's variable mapping to an impl variable the impl never
/// bounds keeps its honest refusal naming the impl's own bound: nothing
/// serves the demand the default body makes.
#[test]
fn an_unbounded_impl_var_substitution_refuses_naming_the_impls_bound() {
    refuse_lowering(
        "tells_unbounded.ds",
        &format!(
            "{}{}",
            GENERIC_STORY,
            concat!(
                "impl[u] tells[u] for list[u] {\n",
                "}\n",
                "fn main() -> int {\n",
                "  println([3].tell(4))\n",
                "  0\n",
                "}\n",
            )
        ),
        "the impl's own bound on the trait variable's substitution has no unique serving instance \
         at this site",
    );
}

/// A trait variable substituting to a compound type over the impl's
/// variables refuses: the seam resolves the next level per variable
/// alone, so an argument shaped `list[u]` names no single bound.
#[test]
fn a_compound_trait_var_substitution_refuses_at_the_chain_build() {
    refuse_lowering(
        "tells_compound.ds",
        &format!(
            "{}{}",
            GENERIC_STORY,
            concat!(
                "impl[u: shows] tells[list[u]] for list[u] {\n",
                "}\n",
                "fn main() -> int {\n",
                "  println([3].tell([4]))\n",
                "  0\n",
                "}\n",
            )
        ),
        "the trait variable substitutes to a compound type over the impl's variables",
    );
}

/// A substituted concrete type spanning more than one word keeps the
/// one-word instantiation boundary: the default body lowers the trait
/// variable's value to a single Raw word, so the argument itself refuses
/// where the type is concrete.
#[test]
fn a_multiword_substituted_argument_refuses_at_the_one_word_boundary() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "struct point { x: int, y: int }\n",
        "extern fn println (s: string) -> ()\n",
        "trait shows {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "trait tells[t: shows] {\n",
        "  fn tell(self, other: t) -> string { other.show() }\n",
        "}\n",
        "impl shows for point {\n",
        "  fn show(self) -> string { \"p\" }\n",
        "}\n",
        "impl tells[point] for string {\n",
        "}\n",
        "fn main() -> int {\n",
        "  println(\"a\".tell(point { x: 1, y: 2 }))\n",
        "  0\n",
        "}\n",
    );
    refuse_lowering(
        "tells_multiword.ds",
        source,
        "the substituted type spans more than one word",
    );
}
