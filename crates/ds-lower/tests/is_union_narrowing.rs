//! The multi-atom `is` test over a written union type: the checker's
//! solved record (span-keyed) names the tested member set, and the
//! lowerer joins one TAG_TEST per atom by jumps — the first hit answers
//! true, the fall-through answers false. A test covering the operand's
//! whole member set folds to the true constant (legal specialization),
//! a disjoint one folds to false, and the kept and else branches narrow
//! per the checker's refinement so payload reads inside them address
//! the union region truthfully. A type variable in the tested set keeps
//! the runtime-reflection refusal (only the covered fold is
//! instantiation-stable).

use ds_abi::Uop;
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, Lowered, PlacedOp, lower};
use ds_mod::{Constant, Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

const SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "extern fn println (msg: string) -> ()\n",
    "\n",
    "enum scan {\n",
    "  miss\n",
    "  word { text: string }\n",
    "  count { n: int }\n",
    "}\n",
    "\n",
    "fn two(v: scan) -> int {\n",
    "  if v is (miss | word) { 1 } else { 0 }\n",
    "}\n",
    "\n",
    "fn bare(v: scan) -> int {\n",
    "  if v is miss | word { 1 } else { 0 }\n",
    "}\n",
    "\n",
    "fn covers(v: scan) -> int {\n",
    "  if v is (miss | word | count) { 1 } else { 0 }\n",
    "}\n",
    "\n",
    "fn disjoint(v: scan) -> int {\n",
    "  if v is (string | int) { 1 } else { 0 }\n",
    "}\n",
    "\n",
    "fn report(v: scan) -> () {\n",
    "  if v is (miss | word) {\n",
    "    if v is word { println(\"word ${v.text}\") } else { println(\"miss\") }\n",
    "  } else {\n",
    "    if v is count { println(\"count ${v.n}\") } else { println(\"never\") }\n",
    "  }\n",
    "}\n",
    "\n",
    "fn main() -> () {\n",
    "  const w: scan = word(text: \"hi\")\n",
    "  const c: scan = count(n: 3)\n",
    "  const m: scan = miss\n",
    "  println(\"${two(w)}\")\n",
    "  println(\"${two(c)}\")\n",
    "  println(\"${bare(m)}\")\n",
    "  println(\"${covers(w)}\")\n",
    "  println(\"${disjoint(m)}\")\n",
    "  report(w)\n",
    "  report(m)\n",
    "  report(c)\n",
    "}\n",
);

/// Parse -> check -> lower. Panics with the rendered diagnostics when an
/// earlier stage refuses.
fn lower_inline(source: &str, module: &str) -> Lowered {
    let mut files = Files::new();
    let id = files.add_source(source_path(module), source);
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
        module,
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers")
}

fn source_path(module: &str) -> String {
    format!("{module}.ds")
}

/// The run surface: every spelling answers by the tested member set,
/// and the narrowed branches read the payload words of the members the
/// union region carries — the string Ref payload and the int payload
/// both stage through their kinds.
#[test]
fn multi_atom_tests_run_and_narrow_end_to_end() {
    let lowered = lower_inline(SOURCE, "is_union");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run("is_union", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the unit entry result"
    );
    assert_eq!(
        String::from_utf8_lossy(vm.captured_output()),
        concat!(
            "1\n",
            "0\n",
            "1\n",
            "1\n",
            "0\n",
            "word hi\n",
            "miss\n",
            "count 3\n",
        ),
        "hit, miss, fold-true, fold-false, and both narrowed payloads"
    );
    assert!(
        vm.objects().is_empty(),
        "the string payload's count drains with the run ({} live)",
        vm.objects().len()
    );
}

/// The emitted test-time shape: one TAG_TEST per tested atom, each
/// short-circuiting by Jmpif to the true constant; the fall-through
/// writes false and jumps past it. Both spellings (parenthesized and
/// bare) emit the same chain.
#[test]
fn the_test_time_shape_is_a_per_atom_branch_chain() {
    let lowered = lower_inline(SOURCE, "is_union");
    let miss = lowered.tag("miss").expect("miss mints a tag");
    let word = lowered.tag("word").expect("word mints a tag");
    for name in ["two", "bare"] {
        let placed = lowered.placed(name).expect("the test function lowers");
        let uops: Vec<&PlacedOp> = placed
            .iter()
            .filter(|op| matches!(op, PlacedOp::Uop(_)))
            .collect();
        let start = uops
            .iter()
            .position(|op| matches!(op, PlacedOp::Uop(Uop::TagTest { .. })))
            .unwrap_or_else(|| panic!("{name} emits a tag test"));
        assert!(
            uops.len() >= start + 8,
            "{name}'s placed stream must hold the whole chain"
        );
        let chain = &uops[start..start + 8];
        let mut tags = Vec::new();
        for (i, op) in chain.iter().enumerate() {
            match op {
                PlacedOp::Uop(Uop::TagTest { tag, .. }) if i == 0 || i == 2 => tags.push(*tag),
                PlacedOp::Uop(Uop::Jmpif { .. }) if i == 1 || i == 3 => {},
                PlacedOp::Uop(Uop::Kconst { kidx, .. }) if i == 4 || i == 6 => {
                    let index = lowered
                        .function_index(name)
                        .expect("the test function is in the table");
                    let pool = lowered.module().functions()[index as usize].constants();
                    let expected = i != 4;
                    assert!(
                        matches!(&pool[usize::from(kidx.0)], Constant::Bool(v) if *v == expected),
                        "{name}'s chain folds {} at index {i}",
                        !expected
                    );
                },
                PlacedOp::Uop(Uop::Jmp { .. }) if i == 5 => {},
                PlacedOp::Uop(Uop::JmpifNot { .. }) if i == 7 => {},
                other => panic!("{name}'s chain shape is off at {i}: {other:?}"),
            }
        }
        assert_eq!(tags.len(), 2, "one test per tested atom, in member order");
        assert!(
            tags.iter().all(|tag| *tag == miss || *tag == word),
            "the chain tests only the written members: tags {tags:?}"
        );
    }
}

/// A test covering the operand's whole member set folds to the true
/// constant and a disjoint one folds to false — no TAG_TEST either way
/// (legal specialization, the multi-atom reading of the single-atom
/// rule).
#[test]
fn covering_and_disjoint_tests_fold_to_constants() {
    let lowered = lower_inline(SOURCE, "is_union");
    for (name, folds_true) in [("covers", true), ("disjoint", false)] {
        let placed = lowered.placed(name).expect("the fold function lowers");
        assert!(
            !placed
                .iter()
                .any(|op| matches!(op, PlacedOp::Uop(Uop::TagTest { .. }))),
            "{name} folds with no tag test"
        );
        let kconsts: Vec<_> = placed
            .iter()
            .filter_map(|op| match op {
                PlacedOp::Uop(Uop::Kconst { kidx, .. }) => Some(*kidx),
                _ => None,
            })
            .collect();
        assert_eq!(
            kconsts.len(),
            1,
            "{name} writes exactly one boolean constant"
        );
        let index = lowered
            .function_index(name)
            .expect("the fold function is in the table");
        let pool = lowered.module().functions()[index as usize].constants();
        match &pool[usize::from(kconsts[0].0)] {
            Constant::Bool(value) => assert_eq!(*value, folds_true, "{name} folds correctly"),
            other => panic!("{name} folds to a bool constant: {other:?}"),
        }
    }
}

/// A type variable on either side of an `is` test keeps the
/// runtime-reflection refusal in every shape but the covered fold (which
/// is instantiation-stable, so it lowers): the variable's values ride
/// one raw word, so no tag discriminates them, and a member set an
/// instantiation may re-solve cannot fold. The generic-impl body is
/// the one lowering surface where type-variable values exist (generic
/// fns never plan).
const VAR_STORY: &str = concat!(
    "tag int\n",
    "tag string\n",
    "struct list[t]\n",
    "extern fn println (s: string) -> ()\n",
    "trait shows {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "impl shows for int {\n",
    "  fn show(self) -> string { \"n\" }\n",
    "}\n",
    "impl shows for string {\n",
    "  fn show(self) -> string { \"s\" }\n",
    "}\n",
    "impl [t: shows] shows for list[t] {\n",
    "  fn show(self) -> string {\n",
    "    var acc = \"\"\n",
    "    for x in self {\n",
    "      BODY\n",
    "    }\n",
    "    acc\n",
    "  }\n",
    "}\n",
    "fn main() -> () { }\n",
);

fn var_story(body: &str) -> String {
    VAR_STORY.replace("BODY", body)
}

/// Checks one source clean and asserts the lowering refuses with the
/// runtime-reflection seam named.
fn refuses_reflection(source: &str) {
    let mut files = Files::new();
    let id = files.add_source("is_var.ds", source);
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
    let Err(err) = lower(
        "is_var",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    ) else {
        panic!("a type-variable test must refuse at lowering")
    };
    assert!(
        matches!(err, LowerError::Unsupported { .. }),
        "an unsupported boundary: {err}"
    );
    assert!(
        err.to_string().contains("runtime reflection"),
        "the refusal names the reflection seam: {err}"
    );
}

#[test]
fn a_concrete_test_over_a_type_variable_operand_refuses() {
    // the operand carries the variable: a constant fold against its
    // member set would misread every instantiation (the element of a
    // generic impl's loop is the reachable shape — generic fns never
    // plan, and a union over a variable cannot shape)
    refuses_reflection(&var_story(
        "if x is int { acc = \"${acc}i\" } else { acc = \"${acc}s\" }",
    ));
}

/// The covered fold is the one instantiation-stable shape over a type
/// variable: every member the operand can hold sits in the tested set,
/// whatever the variable becomes — so the test lowers to the true
/// constant and emits no tag test.
#[test]
fn the_covered_fold_over_a_type_variable_lowers_true() {
    let source = var_story("if x is (t | int) { acc = \"${acc}y\" }");
    let lowered = lower_inline(&source, "is_var_covered");
    let name = lowered
        .functions()
        .iter()
        .find(|name| name.starts_with("shows#"))
        .expect("the generic impl member body plans under its mangle")
        .clone();
    let placed = lowered.placed(&name).expect("the member body lowers");
    assert!(
        !placed
            .iter()
            .any(|op| matches!(op, PlacedOp::Uop(Uop::TagTest { .. }))),
        "the covered fold emits no tag test"
    );
}
