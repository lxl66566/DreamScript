//! The show-carrying interpolation hole end to end: an operand whose
//! type implements `show` — a derived struct, a payloaded enum, a
//! union-typed binding through the sibling branches, a nested dispatch
//! inside a derive-synthesized body (the enum-binder and the
//! struct-field spellings), an inherent member, a bound-type receiver
//! through the frame's runtime evidence — stages as the ordinary
//! member-call dispatch a written `x.show()` uses, and the answered
//! string concatenates into the STRCAT chain like any hole.

use ds_diag::{Files, human};
use ds_lower::lower;
use ds_meta::analyze;
use ds_mod::{Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// The shared declarations: the primitives, the console, and the show
/// trait itself; every case derives or implements against these.
const PRELUDE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "extern fn println (s: string) -> ()\n",
    "trait show {\n",
    "  fn show(self) -> string\n",
    "}\n",
);

/// Parses, checks, lowers, and installs one inline source, then runs
/// `main` and returns the captured stdout.
fn run_stdout(body: &str) -> String {
    let source = format!("{PRELUDE}{body}");
    let mut files = Files::new();
    let id = files.add_source("inline.ds", &source);
    let asts = [parse(&files, id).expect("the source parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the source checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "inline",
        Hash128::compute(source.as_bytes()),
        &analyzed.asts,
        &analyzed,
    )
    .expect("the source lowers");
    let bytes = write(lowered.module());
    let module = read_checked(&bytes, source.as_bytes()).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run("inline", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0)))
    );
    String::from_utf8(vm.captured_output().to_vec()).expect("stdout is UTF-8")
}

/// A derived struct and a payloaded enum inside interpolation, in both
/// hole spellings, beside the primitives' untouched auto-str holes —
/// the dispatch's answered string concatenates into the same chain.
#[test]
fn derived_struct_and_payloaded_enum_holes() {
    let stdout = run_stdout(concat!(
        "#[derive(show)]\n",
        "struct point { x: int, y: int }\n",
        "#[derive(show)]\n",
        "enum mark { miss, hit(int), span { from: int, to: int } }\n",
        "#[derive(show)]\n",
        "struct named { label: string, n: int }\n",
        "fn main() -> int {\n",
        "  const p = point { x: 1, y: 2 }\n",
        "  const m = hit(3)\n",
        "  const s = span(from: 2, to: 9)\n",
        "  const w = named { label: \"wd\", n: 4 }\n",
        "  const n = 7\n",
        "  println(\"p is $p\")\n",
        "  println(\"${p} again\")\n",
        "  println(\"m=$m miss=${miss}\")\n",
        "  println(\"s=$s\")\n",
        "  println(\"w=$w\")\n",
        "  println(\"n=$n p=$p end\")\n",
        "  0\n",
        "}\n",
    ));
    assert_eq!(
        stdout,
        concat!(
            "p is point { x: 1, y: 2 }\n",
            "point { x: 1, y: 2 } again\n",
            "m=hit(3) miss=miss\n",
            "s=span { from: 2, to: 9 }\n",
            "w=named { label: wd, n: 4 }\n",
            "n=7 p=point { x: 1, y: 2 } end\n",
        )
    );
}

/// A union-typed binding's hole runs the full-union show through the
/// sibling branches of the one written head — each inhabitant answers
/// from its own case arm.
#[test]
fn union_typed_hole_runs_the_full_union_show() {
    let stdout = run_stdout(concat!(
        "#[derive(show)]\n",
        "enum mark { miss, hit(int), span { from: int, to: int } }\n",
        "fn main() -> int {\n",
        "  const u1: mark = miss\n",
        "  const u2: mark = hit(3)\n",
        "  const u3: mark = span(from: 2, to: 9)\n",
        "  println(\"first=$u1\")\n",
        "  println(\"second=${u2}\")\n",
        "  println(\"third=$u3\")\n",
        "  0\n",
        "}\n",
    ));
    assert_eq!(
        stdout,
        concat!(
            "first=miss\n",
            "second=hit(3)\n",
            "third=span { from: 2, to: 9 }\n",
        )
    );
}

/// A derive-synthesized body's written `.show()` dispatch over a
/// derived part composes through the enum-binder spelling: the outer
/// hole dispatches the outer body, whose own inner written dispatch
/// answers the bound part — and the written outer call agrees.
#[test]
fn nested_show_dispatch_inside_a_derived_body() {
    let stdout = run_stdout(concat!(
        "#[derive(show)]\n",
        "struct inner { v: int }\n",
        "#[derive(show)]\n",
        "enum wrapper { plain, box(inner) }\n",
        "fn main() -> int {\n",
        "  const w: wrapper = box(inner { v: 7 })\n",
        "  println(\"w=$w\")\n",
        "  println(w.show())\n",
        "  println(\"plain=${plain}\")\n",
        "  0\n",
        "}\n",
    ));
    assert_eq!(
        stdout,
        concat!(
            "w=box(inner { v: 7 })\n",
            "box(inner { v: 7 })\n",
            "plain=plain\n",
        )
    );
}

/// The implicit dispatch's receiver is a borrow: the operand stays live
/// across the hole (a Ref-carrying field's count survives the staging),
/// and a hand-written trait impl answers the hole exactly as the
/// written call does.
#[test]
fn hand_written_impl_answers_and_the_hole_borrows() {
    let stdout = run_stdout(concat!(
        "struct cell { v: int }\n",
        "impl show for cell {\n",
        "  fn show(self) -> string { \"cell(${self.v})\" }\n",
        "}\n",
        "fn main() -> int {\n",
        "  const c = cell { v: 5 }\n",
        "  println(\"one=$c\")\n",
        "  println(c.show())\n",
        "  println(\"two=$c\")\n",
        "  0\n",
        "}\n",
    ));
    assert_eq!(stdout, "one=cell(5)\ncell(5)\ntwo=cell(5)\n");
}

/// An inherent `show` member answers the hole through the same solved
/// dispatch (the type-keyed record), no trait impl in sight.
#[test]
fn inherent_show_member_answers_the_hole() {
    let stdout = run_stdout(concat!(
        "struct gauge { reading: int }\n",
        "impl gauge {\n",
        "  fn show(self) -> string { \"gauge[${self.reading}]\" }\n",
        "}\n",
        "fn main() -> int {\n",
        "  const g = gauge { reading: 9 }\n",
        "  println(\"reading $g now\")\n",
        "  0\n",
        "}\n",
    ));
    assert_eq!(stdout, "reading gauge[9] now\n");
}

/// The struct-field nesting composes: a derived body's field-read
/// receiver addresses the field's own span even when the field
/// occupies one word (the read place keys on the checked shape, never
/// the raw width), so a derived outer over a derived inner answers the
/// hole and the written call, and the two-level nesting composes
/// beside the enum-binder form above.
#[test]
fn derived_struct_field_nesting_composes() {
    let stdout = run_stdout(concat!(
        "#[derive(show)]\n",
        "struct inner { v: int }\n",
        "#[derive(show)]\n",
        "struct outer { i: inner }\n",
        "#[derive(show)]\n",
        "struct deep { m: outer }\n",
        "fn main() -> int {\n",
        "  const o = outer { i: inner { v: 7 } }\n",
        "  const d = deep { m: outer { i: inner { v: 8 } } }\n",
        "  println(\"o=$o\")\n",
        "  println(o.show())\n",
        "  println(\"d=$d\")\n",
        "  0\n",
        "}\n",
    ));
    assert_eq!(
        stdout,
        concat!(
            "o=outer { i: inner { v: 7 } }\n",
            "outer { i: inner { v: 7 } }\n",
            "d=deep { m: outer { i: inner { v: 8 } } }\n",
        )
    );
}

/// A one-word struct-typed field reads as its span in every consuming
/// seam: a hand-written body's hole over the field read, and a plain
/// call argument taking the field by value — the same place the
/// derived body's written dispatch stages.
#[test]
fn one_word_struct_field_reads_stage_everywhere() {
    let stdout = run_stdout(concat!(
        "struct inner { v: int }\n",
        "struct outer { i: inner, n: int }\n",
        "impl show for inner {\n",
        "  fn show(self) -> string { \"i=${self.v}\" }\n",
        "}\n",
        "impl show for outer {\n",
        "  fn show(self) -> string { \"outer[${self.i}] n=${self.n}\" }\n",
        "}\n",
        "fn take(x: inner) -> string { \"got ${x.v}\" }\n",
        "fn main() -> int {\n",
        "  const o = outer { i: inner { v: 3 }, n: 4 }\n",
        "  println(\"o=$o\")\n",
        "  println(take(o.i))\n",
        "  0\n",
        "}\n",
    ));
    assert_eq!(stdout, "o=outer[i=3] n=4\ngot 3\n");
}

/// A hole over a bound-type receiver runs through the frame's runtime
/// evidence: `fn f[t: show]` interpolating `$v` records the member at
/// the hole's span, the entering site stages the evidence word, and the
/// synthetic dispatch resolves at runtime — directly from a concrete
/// caller and through another generic frame whose own slot the chain
/// slides.
#[test]
fn bound_receiver_hole_dispatches_on_runtime_evidence() {
    let stdout = run_stdout(concat!(
        "struct cell { v: int }\n",
        "impl show for cell {\n",
        "  fn show(self) -> string { \"cell(${self.v})\" }\n",
        "}\n",
        "impl show for int {\n",
        "  fn show(self) -> string { \"n=${self}\" }\n",
        "}\n",
        "fn shown[t: show](v: t) -> string { \"v=$v\" }\n",
        "fn relay[t: show](v: t) -> string { shown(v) }\n",
        "fn main() -> int {\n",
        "  const c = cell { v: 5 }\n",
        "  println(shown(c))\n",
        "  println(shown(7))\n",
        "  println(relay(c))\n",
        "  0\n",
        "}\n",
    ));
    assert_eq!(stdout, concat!("v=cell(5)\n", "v=n=7\n", "v=cell(5)\n",));
}
