//! The trait-method dispatch seam end to end: a checked impl member body
//! enters the plan under its mangled identity
//! (`<trait>#<target>#<member>`), lowers with the `Self` receiver as an
//! ordinary first parameter, and every trait-method call site dispatches
//! through CALLG — the callee FuncRef names the impl body, the solved
//! instance rides as the final staged word. A GENERIC impl's bodies
//! lower once per impl: the call sites entering them stage one evidence
//! word per nested demand (the serving member body under the site's
//! substitution, resolved from the checker-recorded bounds) ahead of
//! the instance word, and the body's nested calls on the bound variable
//! dispatch at runtime through the staged slot (the sentinel FuncRef
//! the CALLG carries names the frame's evidence slot). Inherent
//! dot-calls no longer refuse here: the inherent dispatch surface has
//! its own end-to-end suite, and the out-of-scope generic shapes still
//! refuse naming their seams. A defaulted member parameter stages the
//! trait's declared literal at every dispatch spelling [F7] — the
//! declaration is the default's single authority.

use ds_abi::{ConstIdx, Uop};
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, PlacedOp, lower};
use ds_mod::{Constant, FuncRefKey, Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

const SOURCE: &str = include_str!("fixtures/trait_dispatch_probe.ds");

/// Parse -> check -> lower. Panics with the rendered diagnostics when a
/// earlier stage refuses.
fn lower_inline() -> (String, Lowered) {
    let mut files = Files::new();
    let id = files.add_source("trait_dispatch_probe.ds", SOURCE);
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
        "trait_dispatch_probe",
        Hash128::compute(SOURCE.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers");
    (SOURCE.to_owned(), lowered)
}

const GENERIC_SOURCE: &str = include_str!("fixtures/generic_impl_probe.ds");

/// Parse -> check -> lower for the generic-impl probe.
fn lower_generic() -> (String, Lowered) {
    let mut files = Files::new();
    let id = files.add_source("generic_impl_probe.ds", GENERIC_SOURCE);
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
        "generic_impl_probe",
        Hash128::compute(GENERIC_SOURCE.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers");
    (GENERIC_SOURCE.to_owned(), lowered)
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

/// The golden run: both impls answer behind the same call-site shapes, the
/// qualified form dispatches, the struct receiver reads its fields, and
/// the Ref receivers' counts balance — the heap drains at the end.
#[test]
fn two_impls_dispatch_through_the_same_call_site_shapes() {
    let (source, lowered) = lower_inline();
    let mut vm = run_lowered(&source, &lowered);
    assert_eq!(
        vm.run("trait_dispatch_probe", "main", &[]),
        Ok(Value::I64(0)),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        concat!(
            "n=7\n", "hey!\n", "n=9\n", "n=n=3\n", "s=abc\n", "list\n", "n=11\n", "(2, 3)\n",
        )
        .as_bytes()
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// The mangled identities enter the function table; `#` sits outside the
/// identifier charset, so no user-declared name collides.
#[test]
fn impl_bodies_enter_the_table_under_mangled_identity() {
    let (_, lowered) = lower_inline();
    for expected in [
        "shows#int#show",
        "shows#int#tagged",
        "shows#string#show",
        "shows#string#tagged",
        "shows#list[int]#show",
        "spots#point#spot",
    ] {
        assert!(
            lowered.functions().contains(&expected.to_owned()),
            "{expected} planned (table: {:?})",
            lowered.functions()
        );
    }
}

/// The dispatch sites emit CALLG with the instance evidence word staged
/// right before it — never a plain CALL. The wrapper's tail-position site
/// transfers through CALLTG (the dispatch tail form): the instance word
/// stages after the exit claims, right before the transfer, and no RET
/// follows it.
#[test]
fn trait_call_sites_emit_callg_with_the_instance_evidence_word() {
    let (_, lowered) = lower_inline();
    let main = shape(&lowered, "main");
    let callg = main.iter().filter(|k| *k == "Callg").count();
    assert_eq!(callg, 7, "every trait-method site dispatches: {main:?}");
    assert!(
        !main.iter().any(|k| k == "Callt" || *k == "Calltg"),
        "the root frame's value-position trait sites never transfer: {main:?}"
    );
    let wrap = shape(&lowered, "wrap");
    assert!(
        wrap.iter().any(|k| k == "Calltg"),
        "the wrapper's tail transfers through the dispatch tail call: {wrap:?}"
    );
    assert!(
        !wrap
            .iter()
            .any(|k| *k == "Callg" || *k == "Callt" || *k == "Ret"),
        "the tail transfer replaces the in-place dispatch and the return: {wrap:?}"
    );
    let wrap_placed = lowered.placed("wrap").expect("wrap is planned");
    let kinds: Vec<String> = wrap_placed
        .iter()
        .map(|op| match op {
            PlacedOp::Uop(Uop::Kimm { .. }) => "Kimm".to_owned(),
            PlacedOp::Uop(Uop::Calltg { .. }) => "Calltg".to_owned(),
            PlacedOp::Uop(_) => "_".to_owned(),
            PlacedOp::Rc(_) => "Rc".to_owned(),
        })
        .collect();
    let kimm = kinds.iter().position(|k| k == "Kimm");
    let transfer_at = kinds
        .iter()
        .position(|k| k == "Calltg")
        .expect("the dispatch tail transfer");
    assert!(
        kimm.is_some_and(|at| at < transfer_at),
        "the instance evidence word stages before the CALLTG, past the exit claims: {kinds:?}"
    );
}

/// The evidence words are i64 instance ids from the package's instance
/// declaration order (shows int, shows string, shows list[int], spots
/// point): each CALLG site stages the solved instance of its receiver,
/// and the four receiver kinds of `main` cover all four declared
/// instances.
#[test]
fn user_instance_evidence_words_are_instance_ids() {
    let (_, lowered) = lower_inline();
    let placed = lowered.placed("main").expect("main is planned");
    let main_index = lowered
        .function_index("main")
        .expect("main is in the table");
    let pool = &lowered.module().functions()[main_index as usize].constants();
    let mut evidence = Vec::new();
    let mut pending: Option<ConstIdx> = None;
    for op in placed {
        match op {
            PlacedOp::Uop(Uop::Kimm { kidx, .. }) => pending = Some(*kidx),
            PlacedOp::Uop(Uop::Callg { .. }) => {
                let kidx = pending
                    .take()
                    .expect("evidence words stage before the CALLG");
                match pool[usize::from(kidx.0)] {
                    Constant::I64(value) => evidence.push(value),
                    ref other => panic!("the evidence word is an i64 instance id: {other:?}"),
                }
            },
            _ => pending = None,
        }
    }
    evidence.sort_unstable();
    assert_eq!(
        evidence,
        [0, 0, 0, 1, 1, 2, 3],
        "the solved instance per site"
    );
}

/// An inherent dot-call lowers this round: the body plans under its
/// two-part mangle and the call site is a plain local call (the
/// end-to-end run lives in the inherent dispatch suite).
#[test]
fn an_inherent_dot_call_lowers_through_the_local_callee_path() {
    let source = concat!(
        "tag int\n",
        "struct counter { ticks: int }\n",
        "impl counter { fn peek(self) -> int { self.ticks } }\n",
        "fn main() -> int {\n",
        "  const c = counter { ticks: 1 }\n",
        "  c.peek()\n",
        "}\n",
    );
    let mut files = Files::new();
    let id = files.add_source("inherent.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the inherent surface checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "inherent",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("inherent bodies lower this round");
    assert!(
        lowered.functions().contains(&"counter#peek".to_owned()),
        "the inherent mangle is two-part (table: {:?})",
        lowered.functions()
    );
}

/// The generic-impl seam end to end: both instantiations run through
/// ONE lowered body, the nested `x.show()` dispatches to the int and
/// string member bodies from the staged evidence words, and the heap
/// drains — the string instantiation exercises the kind-checked RC
/// balance over the type variable's Raw runtime word.
#[test]
fn two_instantiations_run_through_one_generic_body() {
    let (_, lowered) = lower_generic();
    let mut vm = run_lowered(GENERIC_SOURCE, &lowered);
    assert_eq!(
        vm.run("generic_impl_probe", "main", &[]),
        Ok(Value::I64(0)),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        "list:n=1;n=2;\nlist:a!;b!;\n".as_bytes(),
        "both instantiations answer through one body"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// The generic body enters the table exactly once under its mangled
/// identity (never per instantiation), beside the two monomorphic
/// member bodies its nested sites dispatch into.
#[test]
fn the_generic_body_plans_once_under_mangled_identity() {
    let (_, lowered) = lower_generic();
    let table = lowered.functions();
    let count = table
        .iter()
        .filter(|name| *name == "shows#list[t]#show")
        .count();
    assert_eq!(
        count, 1,
        "one body per impl, not per instantiation: {table:?}"
    );
    for expected in ["shows#int#show", "shows#string#show"] {
        assert!(
            table.contains(&expected.to_owned()),
            "{expected} planned (table: {table:?})"
        );
    }
}

/// The dispatch shapes: the entering sites stage the serving member
/// bodies' indexes ahead of the declared instance id (Kimm words before
/// each CALLG), and the nested sites dispatch through the sentinel
/// FuncRef naming the frame's evidence slot.
#[test]
fn generic_dispatch_stages_evidence_and_dispatches_from_the_slot() {
    let (_, lowered) = lower_generic();
    let body = lowered
        .function_index("shows#list[t]#show")
        .expect("the generic body is planned");
    let placed = lowered
        .placed("shows#list[t]#show")
        .expect("the body lowers");
    let pool = &lowered.module().functions()[body as usize].constants();
    // the nested site's CALLG carries the sentinel FuncRef whose func
    // field is the frame's evidence slot
    let sentinel = placed.iter().find_map(|op| match op {
        PlacedOp::Uop(Uop::Callg { callee, .. }) => match &pool[usize::from(callee.0)] {
            Constant::FuncRef {
                key: FuncRefKey::Native(module),
                func,
            } if module == "$evidence" => Some(*func),
            _ => None,
        },
        _ => None,
    });
    let slot = sentinel.expect("the nested site dispatches through the evidence slot");
    // the evidence parameters bind right after the declared ones: the
    // receiver `self` is the body's one real parameter word, so the pair's
    // slot is word 1
    assert_eq!(slot, 1, "the sentinel names the body's evidence slot");
    // the two entering sites stage the serving bodies' indexes then the
    // declared instance id, in Kimm words right before their CALLG
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
}

const DEFAULT_SOURCE: &str = include_str!("fixtures/trait_default_probe.ds");

/// Parse -> check -> lower for the defaulted-dispatch probe.
fn lower_default_probe() -> (String, Lowered) {
    let mut files = Files::new();
    let id = files.add_source("trait_default_probe.ds", DEFAULT_SOURCE);
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
        "trait_default_probe",
        Hash128::compute(DEFAULT_SOURCE.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers");
    (DEFAULT_SOURCE.to_owned(), lowered)
}

/// The trait-member default authority end to end [F7]: every dispatch
/// spelling stages the trait's declared default at the call site — the
/// dot form and the qualified form answer identically for the omitted
/// default and for the written argument, the string default's Ref word
/// stages with its count balancing (the heap drains), and the generic
/// body's nested sites on the bound variable stage the same declared
/// default through the sentinel dispatch (formerly a staged-width
/// trap: the site staged no default and the VM's slide validation
/// refused the transfer).
#[test]
fn every_dispatch_spelling_stages_the_trait_declared_default() {
    let (source, lowered) = lower_default_probe();
    let mut vm = run_lowered(&source, &lowered);
    assert_eq!(
        vm.run("trait_default_probe", "main", &[]),
        Ok(Value::I64(0)),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        concat!("4\n", "4\n", "8\n", "8\n", "3!\n", "3?\n", "15\n", "4\n").as_bytes(),
        "both spellings answer identically, the string default stages, and the nested sites sum \
         (3+1) + (10+1)"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// The dispatch tail keeps its transfer with the staged default aboard:
/// `go` ends in a tail trait-site whose omitted default stages before
/// the CALLTG, so the frame transfers instead of growing the stack and
/// the uop's real-argument count names the staged default too.
#[test]
fn the_dispatch_tail_transfers_with_the_staged_default() {
    let (_, lowered) = lower_default_probe();
    let go = shape(&lowered, "go");
    assert!(
        go.contains(&"Calltg".to_owned()),
        "the tail dispatch transfers with its staged default: {go:?}"
    );
}

/// A full impl — every trait member bodied — checks and runs end to
/// end: both members plan under their mangled identities, both call
/// spellings answer, and the heap drains.
#[test]
fn a_full_impl_checks_and_runs_end_to_end() {
    let source = concat!(
        "tag int\n",
        "trait sized {\n",
        "  fn twice(self) -> int\n",
        "  fn thrice(self) -> int\n",
        "}\n",
        "impl sized for int {\n",
        "  fn twice(self) -> int { self + self }\n",
        "  fn thrice(self) -> int { self.twice() + self }\n",
        "}\n",
        "fn main() -> int {\n",
        "  const n = 4\n",
        "  n.twice() + n.thrice()\n",
        "}\n",
    );
    let mut files = Files::new();
    let id = files.add_source("full_impl.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the full impl checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "full_impl",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the full impl lowers");
    let mut vm = run_lowered(source, &lowered);
    assert_eq!(
        vm.run("full_impl", "main", &[]),
        Ok(Value::I64(20)),
        "main answers 8 + 12 through both members"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// The partial-impl refusal fires at checking, ahead of lowering: an
/// impl missing a bodiless, non-defaulted trait member reports DS2023
/// at the impl's own span, so a package carrying the shape never
/// reaches the dispatch seam's defensive diagnostic.
#[test]
fn a_partial_impl_refuses_at_checking_before_lowering() {
    let source = concat!(
        "tag int\n",
        "trait sized {\n",
        "  fn twice(self) -> int\n",
        "}\n",
        "impl sized for int {\n",
        "}\n",
        "fn main() -> int {\n",
        "  4.twice()\n",
        "}\n",
    );
    let mut files = Files::new();
    let id = files.add_source("partial_impl.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    let diag = analyzed
        .checked
        .diags
        .iter()
        .find(|d| d.code.as_str() == "DS2023")
        .unwrap_or_else(|| {
            panic!(
                "the incomplete impl refuses at checking: {}",
                analyzed
                    .checked
                    .diags
                    .iter()
                    .map(|diag| human(diag, &files))
                    .collect::<String>()
            )
        });
    assert!(
        diag.message.contains("implements no `twice` member"),
        "the refusal names the missing member: {}",
        diag.message
    );
}
