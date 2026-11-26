//! The word-union optionals inside parts, end to end over the real
//! ambient: a `list[t?]` literal stages each element's whole
//! tag-plus-region span (the stride the first push fixes), the for
//! iteration hands the span back so the presence narrow over the
//! element binder lowers both spellings, an optional enum payload
//! field narrows over the payload binder (the member's own tag word
//! binding the union's whole span), and an is-narrowed binder whose
//! physical layout is a wider union's re-spans into the callee's own
//! region at the call (the payload's multi-word span interleaves the
//! wider region differently). The to_json derive's optional-in-part
//! rows compose these seams; this suite pins them hand-written, the
//! derive e2e living beside the @std json driver's ds-cli fixture.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_stdlib::AMBIENT_SOURCE;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// Parse -> check -> lower -> round-trip -> run over the composed
/// ambient, answering the captured stdout of `main`. Panics with the
/// checker's renderings when the source does not check clean, and with
/// the lowering error when it does not lower.
fn runs(name: &str, probe: &str) -> String {
    let mut files = Files::new();
    let amb = files.add_source("ambient.ds", AMBIENT_SOURCE);
    let id = files.add_source("probe.ds", probe);
    let asts = [
        parse(&files, amb).expect("the ambient parses"),
        parse(&files, id).expect("the probe parses"),
    ];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "{}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(name, Hash128::compute(probe.as_bytes()), &asts, &analyzed)
        .unwrap_or_else(|err| panic!("the probe lowers: {err}"));
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run(name, "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the unit entry result"
    );
    String::from_utf8_lossy(vm.captured_output()).into_owned()
}

/// A `list[int?]` literal stages one whole tag-plus-region span per
/// element — the stride the first push fixes — so the count, the
/// checked read, and the iteration all answer over the same elements
/// both narrow spellings (the `is` test and the match).
#[test]
fn optional_elements_ride_the_list_channel_whole() {
    let is_spelling = concat!(
        "fn count_some(xs: list[int?]) -> int {\n",
        "  var n = 0\n",
        "  for e in xs {\n",
        "    if e is none { } else { n = n + 1 }\n",
        "  }\n",
        "  n\n",
        "}\n",
        "fn main() -> () {\n",
        "  println(\"${count_some([1, none, 3])}\")\n",
        "}\n",
    );
    assert_eq!(runs("opt_elem_is", is_spelling), "2\n");
    let match_spelling = concat!(
        "fn count_some(xs: list[int?]) -> int {\n",
        "  var n = 0\n",
        "  for e in xs {\n",
        "    match e {\n",
        "      none => { }\n",
        "      v => { n = n + 1 }\n",
        "    }\n",
        "  }\n",
        "  n\n",
        "}\n",
        "fn main() -> () {\n",
        "  println(\"${count_some([none, 8, none])}\")\n",
        "}\n",
    );
    assert_eq!(runs("opt_elem_match", match_spelling), "1\n");
}

/// The presence narrow over an optional enum payload field binds the
/// catchall over the member's own tag word — the union's whole span at
/// that word — so the inner value reads from the payload region both
/// spellings, presence and absence.
#[test]
fn the_optional_payload_field_narrows_over_the_binder() {
    let probe = concat!(
        "enum wav { on, blip { times: int? } }\n",
        "fn hold(v: wav) -> int {\n",
        "  match v {\n",
        "    on => { 0 }\n",
        "    blip { times: p0 } => {\n",
        "      match p0 {\n",
        "        none => { 0 }\n",
        "        n0 => { n0 }\n",
        "      }\n",
        "    }\n",
        "  }\n",
        "}\n",
        "fn main() -> () {\n",
        "  println(\"${hold(blip(times: 9))}\")\n",
        "  println(\"${hold(blip(times: none))}\")\n",
        "  println(\"${hold(on)}\")\n",
        "}\n",
    );
    assert_eq!(runs("opt_payload", probe), "9\n0\n0\n");
}

/// A struct field spelled `t | none` enters the universe atom exactly
/// like the `t?` spelling (one optional, two spellings), so it
/// constructs and narrows both sides — before the conversion carried
/// the predeclared member, the field silently missed its atom and the
/// construction refused at lowering over the wrong vocabulary.
#[test]
fn the_union_spelled_optional_field_constructs_like_the_sugar() {
    let probe = concat!(
        "struct cell { v: int | none }\n",
        "fn spell(c: cell) -> string {\n",
        "  match c.v {\n",
        "    none => { \"empty\" }\n",
        "    n => { \"full ${n}\" }\n",
        "  }\n",
        "}\n",
        "fn main() -> () {\n",
        "  println(spell(cell { v: none }))\n",
        "  println(spell(cell { v: 5 }))\n",
        "}\n",
    );
    assert_eq!(runs("opt_union_field", probe), "empty\nfull 5\n");
}

/// An is-narrowed binder whose physical words keep a wider union's
/// layout re-spans into the callee's own region at the call: the
/// optional payload field's multi-word span interleaves the wider
/// region differently, so the flat staging would hand the callee words
/// its parameter span never laid out.
#[test]
fn an_equal_typed_argument_re_spans_its_wider_layout() {
    let probe = concat!(
        "enum wav { on, blip { times: int? } }\n",
        "fn pick(v: int) -> wav | none {\n",
        "  if v == 0 { none } else { blip(times: v) }\n",
        "}\n",
        "fn spell(v: wav) -> string {\n",
        "  match v {\n",
        "    on => { \"on\" }\n",
        "    blip { times: p0 } => {\n",
        "      match p0 {\n",
        "        none => { \"never\" }\n",
        "        n0 => { \"t${n0}\" }\n",
        "      }\n",
        "    }\n",
        "  }\n",
        "}\n",
        "fn main() -> () {\n",
        "  const r = pick(3)\n",
        "  if r is none { println(\"none\") } else {\n",
        "    println(spell(r))\n",
        "  }\n",
        "  const s = pick(0)\n",
        "  if s is none { println(\"none\") } else {\n",
        "    println(spell(s))\n",
        "  }\n",
        "}\n",
    );
    assert_eq!(runs("opt_respan", probe), "t3\nnone\n");
}
