//! The multi-atom use matrix, end to end (todo item 18): a union value
//! used whole binds, copies, passes, returns, and compares against
//! another union or a unit tag from either side — and every use that
//! needs one member (arithmetic, a narrowed call argument, interpolation,
//! a field read, ordering, a condition, a logical op, a call through the
//! value, list membership) refuses at the checker seam that owns it. The
//! one checker-accepted class the lowerer owns is equality against one
//! member's payload value: the per-member tag dispatch has no tag word
//! on the payload side, so every mixed spelling refuses naming the
//! narrowing, and the narrowed spellings (a match arm's binder, an `is`
//! refinement, `.ok` extraction, `?` propagation) run.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

/// The ambient every probe shares.
const PRELUDE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "tag bool\n",
    "tag none\n",
    "extern fn println (msg: string) -> ()\n",
);

/// Parse -> check -> lower -> round-trip -> run, returning the captured
/// stdout of `main`. Panics with the checker's renderings when the
/// source does not check clean, and with the lowering error when it
/// does not lower.
fn runs(name: &str, source: &str) -> String {
    let combined = format!("{PRELUDE}{source}");
    let mut files = Files::new();
    let id = files.add_source("probe.ds", &combined);
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
/// fragments.
fn refuses(name: &str, source: &str, seam: &str, needle: &str) {
    let combined = format!("{PRELUDE}{source}");
    let mut files = Files::new();
    let id = files.add_source("probe.ds", &combined);
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
    assert!(
        text.contains(needle),
        "the error names the narrowing: {error}"
    );
}

/// One checker-refusal probe: the source must fail checking, and the
/// rendered diagnostics must name the seam that owns the refusal.
fn check_refuses(source: &str, needle: &str) {
    let combined = format!("{PRELUDE}{source}");
    let mut files = Files::new();
    let id = files.add_source("probe.ds", &combined);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        !analyzed.checked.diags.is_empty(),
        "the probe refuses at checking: {source}"
    );
    let rendered = analyzed
        .checked
        .diags
        .iter()
        .map(|diag| human(diag, &files))
        .collect::<String>();
    assert!(
        rendered.contains(needle),
        "the refusal names its seam: {rendered}"
    );
}

/// A multi-atom value used whole: it binds, copies, passes as a
/// same-union argument, and returns — the whole span region moves.
#[test]
fn whole_span_values_bind_pass_and_return() {
    let out = runs(
        "whole_span",
        concat!(
            "fn pick() -> int | none { 1 }\n",
            "fn take(v: int | none) -> () { const w = v }\n",
            "fn passthru(v: int | none) -> int | none { v }\n",
            "fn main() -> () {\n",
            "  const x = pick()\n",
            "  const y = x\n",
            "  take(y)\n",
            "  const w = passthru(pick())\n",
            "  match w { none => println(\"none\"), n => println(\"num ${n}\") }\n",
            "}\n",
        ),
    );
    assert_eq!(out, "num 1\n");
}

/// Union equality: same member and payload compare true, a member
/// mismatch or payload difference compares false, and a unit-tag value
/// compares against the union from either side (the tag word rides the
/// same per-member tag tests).
#[test]
fn union_equality_covers_members_payloads_and_the_tag_mirror() {
    let out = runs(
        "union_eq",
        concat!(
            "fn pick() -> int | none { 1 }\n",
            "fn miss() -> int | none { none }\n",
            "fn main() -> () {\n",
            "  if pick() == pick() { println(\"same\") }\n",
            "  if miss() == miss() { println(\"both\") }\n",
            "  if pick() == miss() { println(\"bad\") } else { println(\"diff\") }\n",
            "  if pick() != miss() { println(\"neq\") }\n",
            "  if none == miss() { println(\"literal mirror\") }\n",
            "  const t = none\n",
            "  if t == miss() { println(\"var mirror\") }\n",
            "  if miss() == t { println(\"var mirror back\") }\n",
            "}\n",
        ),
    );
    assert_eq!(
        out,
        "same\nboth\ndiff\nneq\nliteral mirror\nvar mirror\nvar mirror back\n"
    );
}

/// A struct-payloaded member compares over its whole payload span: a
/// member differing only in a later field answers false, the equal case
/// answers true, the unit member's tag pair still routes, and the same
/// struct spelling outside a union is unchanged.
#[test]
fn union_equality_walks_a_struct_members_whole_payload() {
    let out = runs(
        "union_eq_payload",
        concat!(
            "struct pair { a: int, b: int }\n",
            "struct wide { a: int, b: int, c: int }\n",
            "fn one() -> pair | none { pair { a: 1, b: 2 } }\n",
            "fn two() -> pair | none { pair { a: 1, b: 9 } }\n",
            "fn w1() -> wide | none { wide { a: 1, b: 2, c: 3 } }\n",
            "fn w2() -> wide | none { wide { a: 1, b: 2, c: 9 } }\n",
            "fn gone() -> wide | none { none }\n",
            "fn main() -> () {\n",
            "  if one() == two() { println(\"bad\") } else { println(\"second diff\") }\n",
            "  if one() == one() { println(\"pair same\") } else { println(\"bad\") }\n",
            "  if w1() == w2() { println(\"bad\") } else { println(\"third diff\") }\n",
            "  if w1() == w1() { println(\"wide same\") } else { println(\"bad\") }\n",
            "  if gone() == gone() { println(\"unit same\") } else { println(\"bad\") }\n",
            "  if w1() == gone() { println(\"bad\") } else { println(\"tag diff\") }\n",
            "  const l = pair { a: 1, b: 2 }\n",
            "  const r = pair { a: 1, b: 9 }\n",
            "  if l == r { println(\"bad\") } else { println(\"bare diff\") }\n",
            "}\n",
        ),
    );
    assert_eq!(
        out,
        "second diff\npair same\nthird diff\nwide same\nunit same\ntag diff\nbare diff\n"
    );
}

/// The narrowed spellings run: an `is` refinement carries the payload
/// use, a match arm's binder compares the member's value, `.ok`
/// extracts the payload side, and `?` propagates the none side.
#[test]
fn narrowed_uses_run_at_every_spelling() {
    let out = runs(
        "narrowed",
        concat!(
            "fn pick() -> int | none { 1 }\n",
            "fn chain() -> int | none { pick()? }\n",
            "fn main() -> () {\n",
            "  const v = pick()\n",
            "  if v is int { println(\"int ${v + 1}\") } else { println(\"other\") }\n",
            "  match v { none => println(\"none\"), n => println(\"num ${n}\") }\n",
            "  const d = pick().ok((n: int) => n * 2).err((_: none) => 0)\n",
            "  println(\"ok ${d}\")\n",
            "  match chain() { none => println(\"c none\"), n => println(\"c ${n}\") }\n",
            "}\n",
        ),
    );
    assert_eq!(out, "int 2\nnum 1\nok 2\nc 1\n");
}

/// The narrowed compare itself runs where the unnarrowed spelling
/// refuses: a match arm's binder holds one member's value, so `n == 1`
/// is an ordinary word compare.
#[test]
fn a_match_binder_compares_the_member_value_that_the_union_cannot() {
    let out = runs(
        "narrowed_compare",
        concat!(
            "fn pick() -> int | none { 1 }\n",
            "fn miss() -> int | none { none }\n",
            "fn main() -> () {\n",
            "  const v = pick()\n",
            "  match v {\n",
            "    none => println(\"none\"),\n",
            "    n => {\n",
            "      if n == 1 { println(\"one\") } else { println(\"other\") }\n",
            "    },\n",
            "  }\n",
            "  const w = miss()\n",
            "  match w { none => println(\"is none\"), n => println(\"?\") }\n",
            "  const u = pick()\n",
            "  if u is int { println(\"int ${u + 1}\") }\n",
            "}\n",
        ),
    );
    assert_eq!(out, "one\nis none\nint 2\n");
}

/// The equality class the checker accepts but the lowerer owns: a
/// multi-atom value against one member's payload value has no tag word
/// on the payload side, so the per-member tag dispatch cannot route it —
/// every mixed spelling refuses naming the narrowing seam.
#[test]
fn unnarrowed_equality_refuses_naming_the_multi_atom_seam() {
    // a scalar payload member, union on the left
    refuses(
        "refuse_eq_scalar",
        concat!(
            "fn pick() -> int | none { 1 }\n",
            "fn main() -> () { if pick() == 1 { println(\"one\") } }\n",
        ),
        "an equality over a multi-atom value against one member's value",
        "narrow the union with `is` or a match arm before comparing",
    );
    // the same spelling with the union on the right
    refuses(
        "refuse_eq_scalar_mirror",
        concat!(
            "fn pick() -> int | none { 1 }\n",
            "fn main() -> () { if 1 == pick() { println(\"one\") } }\n",
        ),
        "an equality over a multi-atom value against one member's value",
        "narrow the union with `is` or a match arm before comparing",
    );
    // `!=` routes through the same chain
    refuses(
        "refuse_ne_scalar",
        concat!(
            "fn pick() -> int | none { 1 }\n",
            "fn main() -> () { if pick() != 1 { println(\"ne\") } }\n",
        ),
        "an equality over a multi-atom value against one member's value",
        "narrow the union with `is` or a match arm before comparing",
    );
    // a Ref payload member (the string side)
    refuses(
        "refuse_eq_string",
        concat!(
            "fn word() -> string | none { \"ab\" }\n",
            "fn main() -> () { if word() == \"ab\" { println(\"hit\") } }\n",
        ),
        "an equality over a multi-atom value against one member's value",
        "narrow the union with `is` or a match arm before comparing",
    );
    // a struct member's whole span
    refuses(
        "refuse_eq_struct",
        concat!(
            "struct pair { a: int, b: int }\n",
            "tag p_flag\n",
            "fn pick() -> pair | p_flag { pair { a: 5, b: 6 } }\n",
            "fn main() -> () {\n",
            "  if (pair { a: 5, b: 6 }) == pick() { println(\"eq\") }\n",
            "}\n",
        ),
        "an equality over a multi-atom value against one member's value",
        "narrow the union with `is` or a match arm before comparing",
    );
    // a tuple member's positional words
    refuses(
        "refuse_eq_tuple",
        concat!(
            "tag miss\n",
            "fn pick() -> (int, int) | miss { (1, 2) }\n",
            "fn main() -> () { if (1, 2) == pick() { println(\"eq\") } }\n",
        ),
        "an equality over a multi-atom value against one member's value",
        "narrow the union with `is` or a match arm before comparing",
    );
}

/// Every use site class that needs one member refuses at the checker's
/// own seam, each message naming its reason (the field read names the
/// narrowing outright).
#[test]
fn unnarrowed_uses_refuse_at_the_checker_seams() {
    // arithmetic
    check_refuses(
        concat!(
            "fn pick() -> int | none { 1 }\n",
            "fn main() -> () { println(\"${pick() + 1}\") }\n",
        ),
        "arithmetic needs numeric operands",
    );
    // a call argument the parameter must contain
    check_refuses(
        concat!(
            "extern fn shout (n: int) -> int\n",
            "fn pick() -> int | none { 1 }\n",
            "fn main() -> () { shout(pick()) }\n",
        ),
        "not contained",
    );
    // interpolation
    check_refuses(
        concat!(
            "fn pick() -> int | none { 1 }\n",
            "fn main() -> () { println(\"${pick()}\") }\n",
        ),
        "the printable primitives",
    );
    // a field read names the narrowing
    check_refuses(
        concat!(
            "struct num { v: int }\n",
            "fn pick() -> num | none { num { v: 1 } }\n",
            "fn main() -> () { println(\"${pick().v}\") }\n",
        ),
        "narrow with `is` first",
    );
    // ordering
    check_refuses(
        concat!(
            "fn pick() -> int | none { 1 }\n",
            "fn main() -> () { if pick() > 1 { println(\"big\") } }\n",
        ),
        "ordering needs one numeric type",
    );
    // a condition
    check_refuses(
        concat!(
            "fn pick() -> int | none { 1 }\n",
            "fn main() -> () { if pick() { println(\"t\") } }\n",
        ),
        "condition has type",
    );
    // a logical operator
    check_refuses(
        concat!(
            "fn pick() -> int | bool { 1 }\n",
            "fn main() -> () { if pick() && true { println(\"t\") } }\n",
        ),
        "logical operators need `bool`",
    );
    // a call through the union value
    check_refuses(
        concat!(
            "fn pick() -> (fn(int) -> int) | none { (x: int) => x }\n",
            "fn main() -> () {\n",
            "  const f = pick()\n",
            "  const r = f(1)\n",
            "}\n",
        ),
        "is not callable; expected one function type",
    );
    // list membership over the wrong element type
    check_refuses(
        concat!(
            "struct list[t]\n",
            "fn pick() -> int | none { 1 }\n",
            "fn main() -> () { if pick() in [1, 2] { println(\"t\") } }\n",
        ),
        "list membership needs the list's element type",
    );
}
