//! Per-branch condition solving over a decomposed generic union head, end
//! to end: `impl [t: showable] showable for int | list[t]` accepts, the
//! `int` branch names no bound variable so its call sites resolve with no
//! staged evidence and run, and the `list[t]` branch keeps the `t`
//! condition — its call site records the element's serving instance under
//! the branch's own substitution (the impl's requirement set is the union
//! over branches). The conditioned site stages its substituted member
//! into the union-typed `self` slot through the same tag/zero/write
//! re-spanning the widening family uses, the member resolved through the
//! site's substitution (the head keys it by the variable's atoms): a heap
//! application by its constructor (one Ref word whatever the argument)
//! and a tuple member over the variable structurally (its Raw-claimed
//! element words filled from the substituted value's own words, the
//! reference counts riding the kind-checked hand-off). The refusals that
//! survive name their own seams: a struct-over-variable member at the
//! union's own shape (a non-heap application has no inline-span
//! vocabulary), a substitution widening a tuple element past one word at
//! the source's own shape, and the free-generic one-word instantiation
//! boundary.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, lower};
use ds_mod::{Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// The shared prelude every head story composes over: the primitive
/// tags, the console extern, the trait, and the string impl that serves
/// every string-element condition.
const PRELUDE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "extern fn println (s: string) -> ()\n",
    "trait showable {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "impl showable for string {\n",
    "  fn show(self) -> string { \"s\" }\n",
    "}\n",
);

/// One head story: the prelude plus a single written head impl whose
/// member body `answer` the test chooses.
fn head_story(list_decl: bool, head: &str, body: &str) -> String {
    format!(
        "{}{}impl [t: showable] showable for {} {{\n  fn show(self) -> string {}\n}}\n",
        PRELUDE,
        if list_decl { "struct list[t]\n" } else { "" },
        head,
        body,
    )
}

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
        Ok(FrameAnswer::one(Value::I64(0))),
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
        Ok(FrameAnswer::one(Value::I64(0))),
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
    assert_eq!(
        vm.run(name, "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0)))
    );
    assert_eq!(vm.captured_output(), "x\nx\n".as_bytes());
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// The tuple-over-variable member stages at a concrete site: the head
/// `int | ($t, $t)` keys its member by a tuple over the variable, whose
/// claimed region words are Raw (the runtime kind is the site's fact),
/// so the substituted `(1, 2)` value resolves the member structurally
/// and its element words fill the member's claim — the tag writes the
/// member entry's own minted id, the dead `int` claim zero-inits, and
/// the one body answers.
#[test]
fn the_conditioned_tuple_member_stages_and_runs_the_body() {
    let source = format!(
        "{}\n{}",
        head_story(false, "int | (t, t)", "{ \"x\" }"),
        concat!(
            "fn main() -> int {\n",
            "  println((1, 2).show())\n",
            "  0\n",
            "}\n",
        )
    );
    let name = "conditioned_tuple_member.ds";
    let analyzed = check_clean(name, &source);
    let site = analyzed
        .checked
        .trait_calls
        .values()
        .next()
        .expect("the tuple site records its dispatch");
    let universe = &analyzed.decls.universe;
    assert_eq!(universe.render(site.recv), "(int, int)");
    assert_eq!(
        universe.render(analyzed.decls.instances.head(site.instance).unwrap()),
        "($t#1, $t#1)",
        "the conditioned branch serves the tuple receiver"
    );
    let mut files = Files::new();
    let id = files.add_source(name, &source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let lowered = lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed)
        .expect("the tuple site stages its member into the union self slot");
    let mut vm = run_lowered(&lowered, &source);
    assert_eq!(
        vm.run(name, "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        "x\n".as_bytes(),
        "the conditioned branch's body answers over the staged tuple member"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// A string-element tuple site stages its reference counts into the
/// member's Raw-claimed words: the staging copy incs each element word,
/// the call hand-off records the count over the Raw-classified staged
/// word (the RC ops check the runtime kind, so the int instantiation of
/// the same site shape is a no-op there), and the body's parameter death
/// decs them — the heap drains at the end.
#[test]
fn string_element_tuple_sites_stage_their_counts_and_drain() {
    let source = format!(
        "{}\n{}",
        head_story(false, "int | (t, t)", "{ \"x\" }"),
        concat!(
            "fn main() -> int {\n",
            "  println((\"a\", \"b\").show())\n",
            "  0\n",
            "}\n",
        )
    );
    let name = "string_tuple_member.ds";
    let analyzed = check_clean(name, &source);
    let site = analyzed
        .checked
        .trait_calls
        .values()
        .next()
        .expect("the string-element tuple site records its dispatch");
    let universe = &analyzed.decls.universe;
    assert_eq!(universe.render(site.recv), "(string, string)");
    let [(_, _, served)] = site.bounds.as_slice() else {
        panic!("the tuple branch's own bound is recorded under its substitution");
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
        .expect("the string-element tuple site stages its member");
    let mut vm = run_lowered(&lowered, &source);
    assert_eq!(
        vm.run(name, "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        "x\n".as_bytes(),
        "the body answers over the staged string-element tuple member"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// A body narrowing over the tuple member dispatches through the
/// member entry's own tag: the staged site wrote that tag, so the `is`
/// test's TAG_TEST selects the tuple arm for a staged tuple and the
/// else arm for an int — the wrong-tag hazard (a re-mint of the site's
/// substituted atom) would answer the wrong arm silently.
#[test]
fn the_tuple_head_dispatches_through_its_own_tag() {
    let source = format!(
        "{}\n{}",
        head_story(
            false,
            "int | (t, t)",
            concat!(
                "{\n",
                "    if self is (t, t) {\n",
                "      \"p\"\n",
                "    } else {\n",
                "      \"i\"\n",
                "    }\n",
                "  }",
            )
        ),
        concat!(
            "fn main() -> int {\n",
            "  println(1.show())\n",
            "  println((1, 2).show())\n",
            "  println((\"a\", \"b\").show())\n",
            "  0\n",
            "}\n",
        )
    );
    let name = "tuple_tag_dispatch.ds";
    let analyzed = check_clean(name, &source);
    let mut files = Files::new();
    let id = files.add_source(name, &source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let lowered = lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed)
        .expect("the narrowing body lowers over the staged members");
    let mut vm = run_lowered(&lowered, &source);
    assert_eq!(
        vm.run(name, "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        "i\np\np\n".as_bytes(),
        "each site answers through the member its own value staged"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// A mixed-condition head carries every member shape at once — the
/// unconditioned `int`, the heap-application `list[t]`, and the tuple
/// `(t, t)` — and one body answers through its narrowing for each
/// site's own staged member.
#[test]
fn mixed_condition_heads_stage_every_member_shape() {
    let source = format!(
        "{}\n{}",
        head_story(
            true,
            "int | list[t] | (t, t)",
            concat!(
                "{\n",
                "    if self is list[t] {\n",
                "      \"l\"\n",
                "    } else {\n",
                "      if self is (t, t) {\n",
                "        \"p\"\n",
                "      } else {\n",
                "        \"i\"\n",
                "      }\n",
                "    }\n",
                "  }",
            )
        ),
        concat!(
            "fn main() -> int {\n",
            "  println(1.show())\n",
            "  println([1].show())\n",
            "  println((1, 2).show())\n",
            "  0\n",
            "}\n",
        )
    );
    let name = "mixed_head.ds";
    let analyzed = check_clean(name, &source);
    let mut files = Files::new();
    let id = files.add_source(name, &source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let lowered = lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed)
        .expect("every member shape of the mixed head stages");
    let mut vm = run_lowered(&lowered, &source);
    assert_eq!(
        vm.run(name, "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        "i\nl\np\n".as_bytes(),
        "each site answers through the member its own value staged"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// A strict sub-union of the substituted head re-spans into the head's
/// self slot member-wise: the tag-dispatch walk reads the source
/// union's own tags, each taken arm re-spans through the substitution's
/// member entry, and the substituted tuple arm mints the entry's own
/// tag (the source union's tuple tag names no member of the head).
#[test]
fn a_subunion_argument_respans_into_the_head() {
    let source = format!(
        "{}\n{}",
        head_story(false, "int | (t, t)", "{ \"x\" }"),
        concat!(
            "fn main() -> int {\n",
            "  const u: int | (int, int) = (1, 2)\n",
            "  println(u.show())\n",
            "  0\n",
            "}\n",
        )
    );
    let name = "subunion_into_head.ds";
    let analyzed = check_clean(name, &source);
    let mut files = Files::new();
    let id = files.add_source(name, &source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let lowered = lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed)
        .expect("the sub-union value re-spans into the head's self slot");
    let mut vm = run_lowered(&lowered, &source);
    assert_eq!(
        vm.run(name, "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        "x\n".as_bytes(),
        "the body answers over the re-spanned tuple member"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// A struct-over-variable member keeps its refusal at the union's own
/// shape: `pair[t]` is a non-heap application, which has no inline-span
/// vocabulary at all (its fields would need a per-instantiation
/// layout), so the head refuses before any site stages — the member
/// write seam never sees it.
#[test]
fn a_struct_over_variable_member_lays_out_its_reserved_word() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "struct pair[t]\n",
        "extern fn println (s: string) -> ()\n",
        "trait showable {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "impl showable for string {\n",
        "  fn show(self) -> string { \"s\" }\n",
        "}\n",
        "impl [t: showable] showable for int | pair[t] {\n",
        "  fn show(self) -> string { \"x\" }\n",
        "}\n",
        "fn main() -> int {\n",
        "  println(1.show())\n",
        "  0\n",
        "}\n",
    );
    // a part-less generic struct's application keeps the reserved word
    // the part-less struct carries in every value position: the union
    // member claims it like any member payload, argument-independent,
    // so the head story lowers and runs
    let analyzed = check_clean("struct_over_variable_member.ds", source);
    let lowered = lower(
        "struct_over_variable_member",
        Hash128::compute(source.as_bytes()),
        &analyzed.asts,
        &analyzed,
    )
    .expect("the head story lowers: the application member claims its reserved word");
    let mut vm = run_lowered(&lowered, source);
    assert_eq!(
        vm.run("struct_over_variable_member", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        b"x\n" as &[u8],
        "the dispatch answers"
    );
}

/// A substitution that widens a tuple element past one word keeps its
/// refusal at the source's own shape: the tuple value vocabulary is
/// one plain word per element, so a struct-element instantiation (even
/// a part-less struct, whose shape is a span, not a word) refuses
/// before the member write — the member's Raw claim would pair against
/// a wider element the tuple vocabulary cannot spell.
#[test]
fn a_multiword_element_substitution_refuses_at_the_sources_shape() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "struct point {}\n",
        "extern fn println (s: string) -> ()\n",
        "trait showable {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "impl showable for string {\n",
        "  fn show(self) -> string { \"s\" }\n",
        "}\n",
        "impl showable for point {\n",
        "  fn show(self) -> string { \"pt\" }\n",
        "}\n",
        "impl [t: showable] showable for int | (t, t) {\n",
        "  fn show(self) -> string { \"x\" }\n",
        "}\n",
        "fn main() -> int {\n",
        "  const p = point {}\n",
        "  println((p, p).show())\n",
        "  0\n",
        "}\n",
    );
    refuse_lowering(
        "multiword_element_substitution.ds",
        source,
        "a multi-word tuple element is outside the M3 runtime contract",
    );
}

/// A substituted value wider than one word entering a free generic fn
/// whose bound demand dispatches into a generic impl member keeps the
/// one-word instantiation boundary: the instance's own parameters carry
/// the argument's whole span, but the body's runtime-evidence dispatch
/// stages the receiver into the serving member's Raw-word body — the
/// shape the impl-member instantiation owns, so the generic impl members
/// still ride one Raw body.
#[test]
fn a_free_generic_call_keeps_its_one_word_instantiation_boundary() {
    let source = format!(
        "{}\n{}",
        head_story(false, "int | (t, t)", "{ \"x\" }"),
        concat!(
            "fn twice[u: showable](x: u) -> string {\n",
            "  x.show()\n",
            "}\n",
            "fn main() -> int {\n",
            "  println(twice((1, 2)))\n",
            "  0\n",
            "}\n",
        )
    );
    refuse_lowering(
        "free_generic_tuple_argument.ds",
        &source,
        "the substituted type spans more than one word",
    );
}

/// Two members of one head both matching a site's receiver is the
/// checker's ambiguity refusal, never a lowerer guess: the lowerer's
/// member lookup keeps its own uniqueness guard (it answers no member
/// when two structurally match), but the checker refuses the site
/// first with its dispatch-ambiguity diagnostic.
#[test]
fn an_ambiguous_structural_match_is_the_checkers_refusal() {
    let source = format!(
        "{}\n{}",
        head_story(false, "int | (t, t) | (t, int)", "{ \"x\" }"),
        concat!(
            "fn main() -> int {\n",
            "  println((1, 1).show())\n",
            "  0\n",
            "}\n",
        )
    );
    let name = "ambiguous_members.ds";
    let mut files = Files::new();
    let id = files.add_source(name, &source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    let rendered = analyzed
        .checked
        .diags
        .iter()
        .map(|diag| human(diag, &files))
        .collect::<String>();
    assert!(
        rendered.contains("2 impls of `showable` accept receiver `(int, int)`"),
        "the checker refuses the ambiguous site: {rendered}"
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
        Ok(FrameAnswer::one(Value::I64(0))),
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
