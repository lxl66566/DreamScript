//! Per-branch condition solving over a decomposed generic union head, end
//! to end: `impl [t: showable] showable for int | list[t]` accepts, the
//! `int` branch names no bound variable so its call sites resolve with no
//! staged evidence and run, and the `list[t]` branch keeps the `t`
//! condition — its call site records the element's serving instance under
//! the branch's own substitution (the impl's requirement set is the union
//! over branches). The conditioned list site stages its member into the
//! union-typed `self` slot through the same tag/zero/write re-spanning
//! the widening family uses, the member resolved through the site's
//! substitution (the head keys it by the variable's atom); a member whose
//! claimed words depend on the substitution keeps its honest refusal.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, lower};
use ds_mod::{Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

const STORY: &str = concat!(
    "tag int\n",
    "tag string\n",
    "struct list[t]\n",
    "extern fn println (s: string) -> ()\n",
    "trait showable {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "impl showable for string {\n",
    "  fn show(self) -> string { \"s\" }\n",
    "}\n",
    "impl [t: showable] showable for int | list[t] {\n",
    "  fn show(self) -> string { \"x\" }\n",
    "}\n",
);

/// Parse and check one source, asserting the checker's stage is clean.
fn check_clean(name: &str, source: &str) -> ds_check::Analyzed {
    let mut files = Files::new();
    let id = files.add_source(name, source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the surface checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    analyzed
}

/// Lowers a checked-clean source, panicking on success with the returned
/// closure running the module.
fn refuse_lowering(name: &str, source: &str, expected: &str) {
    let mut files = Files::new();
    let id = files.add_source(name, source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
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

/// The unconditioned branch's call site runs end to end: no condition to
/// prove, no evidence words to stage, one instance word behind the
/// receiver, and the heap drains.
#[test]
fn the_unconditioned_branch_call_runs_with_no_evidence() {
    let source = format!(
        "{}\n{}",
        STORY,
        concat!(
            "fn main() -> int {\n",
            "  println(1.show())\n",
            "  0\n",
            "}\n",
        )
    );
    let name = "unconditioned_branch.ds";
    let analyzed = check_clean(name, &source);
    assert_eq!(
        analyzed.checked.trait_calls.len(),
        1,
        "the int site records its dispatch"
    );
    let site = analyzed
        .checked
        .trait_calls
        .values()
        .next()
        .expect("the int site records its dispatch");
    assert!(
        site.bounds.is_empty(),
        "the unconditioned branch stages no evidence"
    );
    let mut files = Files::new();
    let id = files.add_source(name, &source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let lowered = lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed)
        .expect("the int site lowers");
    let mut vm = run_lowered(&lowered, &source);
    assert_eq!(
        vm.run(name, "main", &[]),
        Ok(Value::I64(0)),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        "x\n".as_bytes(),
        "the unconditioned branch's body answers"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// The conditioned branch's call site records its own bound — the
/// element's showable instance under the branch's substitution — and the
/// concrete list receiver stages into the union-typed `self` slot: the
/// member resolved through the substitution (the head keys it by the
/// variable's atom `list[$t]`), the staging temp re-spanned through
/// tag/zero/write like every widened member argument, and the one body
/// answers. The list's string elements are Ref words, so the member's
/// count rides the staging and the heap drains at the end.
#[test]
fn the_conditioned_branch_stages_its_list_member_and_runs_the_body() {
    let source = format!(
        "{}\n{}",
        STORY,
        concat!(
            "fn main() -> int {\n",
            "  println([\"a\"].show())\n",
            "  0\n",
            "}\n",
        )
    );
    let name = "conditioned_branch.ds";
    let analyzed = check_clean(name, &source);
    let site = analyzed
        .checked
        .trait_calls
        .values()
        .next()
        .expect("the list site records its dispatch");
    let universe = &analyzed.decls.universe;
    assert_eq!(universe.render(site.recv), "list[string]");
    assert_eq!(
        universe.render(analyzed.decls.instances.head(site.instance).unwrap()),
        "list[$t#1]",
        "the conditioned branch serves the list receiver"
    );
    let [(_, _, served)] = site.bounds.as_slice() else {
        panic!("the branch's own bound is recorded under its substitution");
    };
    let served = served.expect("the string instance serves the element");
    assert_eq!(
        universe.render(analyzed.decls.instances.head(served).unwrap()),
        "string",
        "the staged evidence is the element's showable instance"
    );
    let mut files = Files::new();
    let id = files.add_source(name, &source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let lowered = lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed)
        .expect("the list site stages its member into the union self slot");
    let mut vm = run_lowered(&lowered, &source);
    assert_eq!(
        vm.run(name, "main", &[]),
        Ok(Value::I64(0)),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        "x\n".as_bytes(),
        "the conditioned branch's body answers over the staged list member"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// An int-element list site (the goal shape — the element's showable
/// comes from the same written head's unconditioned branch) and a list
/// of lists (the bound resolved transitively through the head itself)
/// stage their members the same way and run.
#[test]
fn int_element_and_nested_list_sites_stage_their_members_too() {
    let source = format!(
        "{}\n{}",
        STORY,
        concat!(
            "fn main() -> int {\n",
            "  println([1, 2].show())\n",
            "  println([[\"a\"]].show())\n",
            "  0\n",
            "}\n",
        )
    );
    let name = "nested_list_site.ds";
    let analyzed = check_clean(name, &source);
    let mut files = Files::new();
    let id = files.add_source(name, &source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let lowered = lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed)
        .expect("the int-element and nested list sites lower");
    let mut vm = run_lowered(&lowered, &source);
    assert_eq!(vm.run(name, "main", &[]), Ok(Value::I64(0)));
    assert_eq!(vm.captured_output(), "x\nx\n".as_bytes());
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// A member whose claimed words depend on the substitution keeps its
/// honest refusal: the head `int | ($t, $t)` keys its member by a tuple
/// over the variable, whose region word kinds the site's concrete
/// argument would have to agree with at runtime — the constructor-mate
/// resolution covers heap applications alone (one Ref word whatever the
/// argument), so the tuple member names the seam.
#[test]
fn a_substitution_shaped_member_keeps_its_refusal() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "extern fn println (s: string) -> ()\n",
        "trait showable {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "impl showable for string {\n",
        "  fn show(self) -> string { \"s\" }\n",
        "}\n",
        "impl [t: showable] showable for int | (t, t) {\n",
        "  fn show(self) -> string { \"x\" }\n",
        "}\n",
        "fn main() -> int {\n",
        "  println((1, 2).show())\n",
        "  0\n",
        "}\n",
    );
    refuse_lowering(
        "substitution_shaped_member.ds",
        source,
        "a union member write is outside the M3 runtime contract",
    );
}

const DEMANDING_BODY_STORY: &str = concat!(
    "tag int\n",
    "tag string\n",
    "struct list[t]\n",
    "extern fn println (s: string) -> ()\n",
    "trait showable {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "impl showable for string {\n",
    "  fn show(self) -> string { \"s\" }\n",
    "}\n",
    "impl [t: showable] showable for int | list[t] {\n",
    "  fn show(self) -> string {\n",
    "    var acc = \"\"\n",
    "    if self is list[t] {\n",
    "      for x in self {\n",
    "        acc = \"${acc}${x.show()}\"\n",
    "      }\n",
    "    } else {\n",
    "      acc = \"i\"\n",
    "    }\n",
    "    acc\n",
    "  }\n",
    "}\n",
    "fn main() -> int {\n",
    "  println(1.show())\n",
    "  0\n",
    "}\n",
);

/// A body demanding the impl's bound on a variable a dispatching branch
/// does not mention runs green through the int site: the demand walk
/// collects the union over branches (the list branch's `t` condition),
/// while the int site's own branch carries no condition to answer it —
/// the demanding code sits under an `is list[t]` narrowing an int
/// receiver can never satisfy, so the site stages the dead sentinel
/// (a chain word naming no object) for that demand and the run answers
/// through the unconditioned arm.
#[test]
fn an_int_site_into_a_body_demanding_the_unions_bound_runs_green() {
    let name = "demanding_body.ds";
    let source = DEMANDING_BODY_STORY;
    let (asts, analyzed) = {
        let mut files = Files::new();
        let id = files.add_source(name, source);
        let asts = [parse(&files, id).expect("the probe parses")];
        let analyzed = analyze(&files, &asts);
        assert!(
            analyzed.checked.diags.is_empty(),
            "the demanding body checks clean: {}",
            analyzed
                .checked
                .diags
                .iter()
                .map(|diag| human(diag, &files))
                .collect::<String>()
        );
        (asts, analyzed)
    };
    let lowered = lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed)
        .expect("the int site lowers with the dead sentinel staged");
    let mut vm = run_lowered(&lowered, source);
    assert_eq!(
        vm.run(name, "main", &[]),
        Ok(Value::I64(0)),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        "i\n".as_bytes(),
        "the unconditioned arm answers; the list-demanding arm never runs for an int receiver"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}
