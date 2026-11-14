//! The span rework's width flips, end to end: a struct-typed field
//! carries its whole inline word span through construction, binding,
//! calls, returns, union writes, match arms, and equality — the emitted
//! layouts show the real widths and the values survive every hop.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

/// Parse -> check -> lower -> round-trip -> run, returning the captured
/// stdout of `main`. Panics with the checker's renderings when the
/// source does not check clean, and with the lowering error when it
/// does not lower.
fn runs(name: &str, source: &str) -> String {
    let mut files = Files::new();
    let id = files.add_source("probe.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
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
    let lowered = lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed)
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
        Ok(Value::I64(0)),
        "the unit entry result"
    );
    String::from_utf8_lossy(vm.captured_output()).into_owned()
}

/// Same pipeline, expecting lowering to refuse with the exact message
/// fragment.
fn refuses(name: &str, source: &str, seam: &str, needle: &str) {
    let mut files = Files::new();
    let id = files.add_source("probe.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
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
    let Err(error) = lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed) else {
        panic!("the probe refuses: {source}")
    };
    assert!(
        matches!(error, LowerError::Unsupported { .. }),
        "the refusal is Unsupported, got {error:?}"
    );
    let text = error.to_string();
    assert!(text.contains(seam), "the error names the seam: {error}");
    assert!(text.contains(needle), "the error names the future: {error}");
}

/// The whole multi-word field story: a two-word `point` field inside a
/// struct, through a call, a return, a union write, a match arm, nested
/// reads, and memberwise equality.
#[test]
fn a_struct_typed_field_flows_its_whole_span() {
    let out = runs(
        "span_fields",
        concat!(
            "tag int\n",
            "tag string\n",
            "tag io_error\n",
            "extern fn println (msg: string) -> ()\n",
            "struct point { x: int, y: int }\n",
            "struct box { at: point, label: string }\n",
            "fn shift(p: point) -> point {\n",
            "  point { x: p.x + 1, y: p.y }\n",
            "}\n",
            "fn total(p: point) -> int { p.x + p.y }\n",
            "fn show(v: box | io_error) -> () {\n",
            "  match v {\n",
            "    io_error => println(\"io\"),\n",
            "    c => println(c.label),\n",
            "  }\n",
            "}\n",
            "fn main() -> () {\n",
            "  const p = point { x: 1, y: 2 }\n",
            "  const b = box { at: shift(p), label: \"b\" }\n",
            "  println(\"${total(b.at)}\")\n",
            "  show(b)\n",
            "  var v: box | io_error = b\n",
            "  show(v)\n",
            "  v = io_error\n",
            "  show(v)\n",
            "}\n",
        ),
    );
    assert_eq!(out, "4\nb\nb\nio\n");
}

/// A member whose fields span Ref words behind a multi-word struct
/// field: the claims, the union re-spans, and the deaths balance with
/// counts on both the struct field's words and the trailing Ref.
#[test]
fn ref_words_inside_a_multi_word_field_balance() {
    let out = runs(
        "span_refs",
        concat!(
            "tag int\n",
            "tag string\n",
            "tag none\n",
            "extern fn println (msg: string) -> ()\n",
            "struct pair { a: string, b: string }\n",
            "struct card { sides: pair, rank: int }\n",
            "fn render(c: card) -> string { c.sides.a + \"/\" + c.sides.b }\n",
            "fn main() -> () {\n",
            "  var i = 0\n",
            "  var total = 0\n",
            "  while i < 3 {\n",
            "    const c = card { sides: pair { a: \"left\", b: \"right\" }, rank: i }\n",
            "    const text = render(c)\n",
            "    if text == \"left/right\" { total += c.rank + 1 }\n",
            "    i += 1\n",
            "  }\n",
            "  println(\"${total}\")\n",
            "}\n",
        ),
    );
    assert_eq!(out, "6\n");
}

/// A tuple member of a union region: the elements claim one word each
/// (possibly scattered among other members' fields), and every read
/// gathers positionally — the member binder's destructuring, the
/// widening call, and the pass-through return.
#[test]
fn a_tuple_member_binds_positionally_over_the_region() {
    let out = runs(
        "tuple_member",
        concat!(
            "tag int\n",
            "tag string\n",
            "tag miss\n",
            "extern fn println (msg: string) -> ()\n",
            "fn sum(v: (int, int) | miss) -> int {\n",
            "  match v {\n",
            "    miss => 0,\n",
            "    other => {\n",
            "      const (a, b) = other\n",
            "      a + b\n",
            "    },\n",
            "  }\n",
            "}\n",
            "fn passthru(v: (int, string) | miss) -> (int, string) | miss { v }\n",
            "fn main() -> () {\n",
            "  println(\"${sum((3, 4))}\")\n",
            "  println(\"${sum(miss)}\")\n",
            "  match passthru((7, \"z\")) {\n",
            "    miss => println(\"m\"),\n",
            "    other => {\n",
            "      const (n, s) = other\n",
            "      println(\"${n} ${s}\")\n",
            "    },\n",
            "  }\n",
            "}\n",
        ),
    );
    assert_eq!(out, "7\n0\n7 z\n");
}

/// The widened surfaces refuse honestly: the field vocabulary beyond
/// struct-typed fields, tuple spans over wider elements, and the
/// one-word native seams.
#[test]
fn the_refusal_openings_keep_their_honest_names() {
    // a tuple product over a union element: the inline-span vocabulary
    // refuses it (the literal forces the field's shape)
    refuses(
        "refuse_union_field",
        concat!(
            "tag int\n",
            "tag string\n",
            "struct outer { x: (int | string, int) }\n",
            "fn main() -> () {\n",
            "  const box = outer { x: (1, 2) }\n",
            "  println(\"unreachable\")\n",
            "}\n",
            "extern fn println (msg: string) -> ()\n",
        ),
        "a struct payload field",
        "this field's shape stays outside the inline-span vocabulary",
    );
    // a tuple whose element type spans words: the tuple vocabulary keeps
    // one word per element
    refuses(
        "refuse_tuple_element",
        concat!(
            "tag int\n",
            "tag string\n",
            "struct point { x: int, y: int }\n",
            "fn take(t: (point, int)) -> int { 0 }\n",
            "fn main() -> () { println(\"unreachable\") }\n",
            "extern fn println (msg: string) -> ()\n",
        ),
        "a multi-word tuple element",
        "tuple elements lower as one plain frame word",
    );
    // a wider map value under map_set's positional contract
    refuses(
        "refuse_map_value",
        concat!(
            "tag int\n",
            "tag string\n",
            "struct map[k, v]\n",
            "struct point { x: int, y: int }\n",
            "extern fn map_set (m: mut map[string, point], key: string, value: point) -> ()\n",
            "fn main() -> () {\n",
            "  var m = map[string, point]()\n",
            "  map_set(m, \"origin\", point { x: 0, y: 0 })\n",
            "}\n",
        ),
        "a map_set argument",
        "map keys and values lower as one machine word",
    );
    // a deinit-carrying field type: the inline span has no claim of its
    // own, so the hook would run at the wrong death
    refuses(
        "refuse_deinit_field",
        concat!(
            "tag int\n",
            "tag string\n",
            "extern fn println (msg: string) -> ()\n",
            "struct session { user: string\n",
            "  deinit { println(\"freed ${self.user}\") }\n",
            "}\n",
            "struct holder { inner: session, mark: int }\n",
            "fn main() -> () {\n",
            "  const h = holder { inner: session { user: \"u\" }, mark: 1 }\n",
            "}\n",
        ),
        "a struct payload field",
        "per-span deinit claims own the seam",
    );
}
