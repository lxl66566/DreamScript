//! The byte word end to end [F6]: `b'x'` types through a declared
//! `byte` atom and lowers as its 0-255 integer value through the
//! integer slot encoding — its own atom in the u8 width class, outside
//! the int conversion family, so mixed arithmetic refuses at the
//! checker's numeric table (pinned in ds-check's byte_in suite) and a
//! byte prints only through an explicit `str(b)` (interpolation holes
//! stay checker-gated to the five printable primitives). Covered
//! shapes: a bound literal, a byte parameter and return, a byte-typed
//! list element read back through list_get, a `byte | none` union
//! narrowed by `is`, a `byte | int` union sharing one I64 region word,
//! and a match arm binding the byte payload — the payload word is
//! I64-class and follows the union region's ordinary member-claim
//! rules.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::{Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Value, Vm, loader, native};

/// The declaration prelude every case shares.
const PRELUDE: &str = concat!(
    "tag int\n",
    "tag byte\n",
    "tag bool\n",
    "tag string\n",
    "struct list[t]\n",
    "extern fn println (s: string) -> ()\n",
    "extern fn concat (a: string, b: string) -> string\n",
    "extern fn str[t] (v: t) -> string\n",
    "extern fn list_get[t] (xs: list[t], i: int) -> t | none\n",
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
        &asts,
        &analyzed,
    )
    .expect("the source lowers");
    let bytes = write(lowered.module());
    let module = read_checked(&bytes, source.as_bytes()).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_native(native::STD_PLATFORM_MODULE, native::std_platform_module());
    loader::install(&mut registry, &module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(vm.run("inline", "main", &[]), Ok(Value::I64(0)));
    String::from_utf8(vm.captured_output().to_vec()).expect("stdout is UTF-8")
}

/// A bound literal, a byte parameter, and a byte return: every word
/// stages as the 0-255 integer value and prints through `str` (the
/// str_int native over the I64 word — no byte-specific native exists).
#[test]
fn literals_params_and_returns_ride_the_integer_word() {
    let stdout = run_stdout(concat!(
        "fn pass(b: byte) -> byte {\n",
        "  b\n",
        "}\n",
        "fn spell(b: byte) -> string {\n",
        "  str(b)\n",
        "}\n",
        "fn main() -> () {\n",
        "  const sep = b'|'\n",
        "  const nl = b'\\n'\n",
        "  println(str(sep))\n",
        "  println(str(nl))\n",
        "  println(spell(b'a'))\n",
        "  println(str(pass(b'Q')))\n",
        "}\n",
    ));
    assert_eq!(stdout, "124\n10\n97\n81\n");
}

/// A byte-typed list element round-trips through the list object: the
/// literal pushes one I64 word per element, list_get's ok/none span
/// re-spans into `byte | none`, and the catchall arm binds the byte
/// payload word (the refinement the checker hands the lowerer).
#[test]
fn list_elements_round_trip_and_match_arms_bind_the_payload() {
    let stdout = run_stdout(concat!(
        "fn main() -> () {\n",
        "  const xs = [b'a', b'b']\n",
        "  match list_get(xs, 1) {\n",
        "    none => println(\"none\"),\n",
        "    b => println(str(b)),\n",
        "  }\n",
        "  match list_get(xs, 9) {\n",
        "    none => println(\"none\"),\n",
        "    b => println(str(b)),\n",
        "  }\n",
        "}\n",
    ));
    assert_eq!(stdout, "98\nnone\n");
}

/// A `byte | none` union narrowed by `is`: the byte member's payload is
/// one I64-class region word claimed per the ordinary member rules, the
/// TAG_TEST dispatches on the member's tag, and the narrowed read in
/// the true branch addresses the payload word.
#[test]
fn a_byte_union_member_narrows_through_is() {
    let stdout = run_stdout(concat!(
        "fn pick(hit: bool) -> string {\n",
        "  const v: byte | none = if hit { b'z' } else { none }\n",
        "  if v is byte { str(v) } else { \"none\" }\n",
        "}\n",
        "fn main() -> () {\n",
        "  println(pick(true))\n",
        "  println(pick(false))\n",
        "}\n",
    ));
    assert_eq!(stdout, "122\nnone\n");
}

/// A `byte | int` union: both payload members are I64-class, so the
/// member-claim rule gives the region ONE shared word (the golden
/// fixtures' precedent for two int-ish members) and TAG_TEST tells the
/// members apart by tag alone; each narrowed read addresses that same
/// word, and str renders it through each atom's own arm. (A bare
/// member literal annotated to the union — `const a: byte | int = b'x'`
/// — stays the pre-existing single-atom-binder boundary the int analog
/// hits too, so both members construct through the if-expression's
/// per-member re-span.)
#[test]
fn byte_and_int_members_share_the_i64_region_word() {
    let stdout = run_stdout(concat!(
        "fn render(v: byte | int) -> string {\n",
        "  if v is byte { concat(\"byte \", str(v)) } else { concat(\"int \", str(v)) }\n",
        "}\n",
        "fn main() -> () {\n",
        "  const a: byte | int = if true { b'x' } else { 7 }\n",
        "  const i: byte | int = if false { b'x' } else { 7 }\n",
        "  println(render(a))\n",
        "  println(render(i))\n",
        "}\n",
    ));
    assert_eq!(stdout, "byte 120\nint 7\n");
}
