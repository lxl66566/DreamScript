//! The bytes collection's structural `==`/`!=` end to end [B9]: the
//! operands check clean through the string's one Ref word shape, and
//! the chain dispatches on the bytes content-compare native — equal
//! payloads compare equal whatever their object identity, a different
//! length or a single differing byte unequal, `!=` the same chain's
//! inverted bool. Both anonymous operands (fresh `bytes_of` calls) and
//! bound names stage, so the native window's borrowed counts balance on
//! every spelling. The per-word chains compare fields by their type,
//! never their slot kind alone: a struct, tuple, or union field of
//! bytes, a bytes field nested inside a struct field, and `in`
//! membership over `list[bytes]` all dispatch the content compare —
//! the EqStr uop reads string objects alone, so the slot-kind chain
//! trapped at runtime over a bytes word. A chain that answers at an
//! earlier field releases the skipped Ref-carrying fields' staged
//! counts at its merged exits, so the heap drains on the fail-fast
//! spellings too (branch, statement, and interpolated; bytes or string
//! behind the differing field; tuple and union payload chains; wide
//! `in` membership).

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::{Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// The declaration prelude every case shares.
const PRELUDE: &str = concat!(
    "tag int\n",
    "tag byte\n",
    "tag bool\n",
    "tag string\n",
    "tag bytes\n",
    "struct list[t]\n",
    "extern fn println (s: string) -> ()\n",
    "extern fn bytes_of (xs: list[byte]) -> bytes\n",
    "extern fn byte_list (b: bytes) -> list[byte]\n",
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

/// The acceptance: equal payloads compare equal (two fresh `bytes_of`
/// results carry equal content behind different objects), a differing
/// byte and a differing length compare unequal, and `!=` inverts the
/// same chain.
#[test]
fn bytes_equality_answers_payload_content() {
    let stdout = run_stdout(concat!(
        "fn main() -> () {\n",
        "  const one = bytes_of([b'a', b'b', b'c'])\n",
        "  const two = bytes_of([b'a', b'b', b'c'])\n",
        "  const swapped = bytes_of([b'a', b'b', b'd'])\n",
        "  const short = bytes_of([b'a', b'b'])\n",
        "  println(\"${one == two} ${one == swapped} ${one == short}\")\n",
        "  println(\"${one != two} ${one != swapped} ${one != short}\")\n",
        "}\n",
    ));
    assert_eq!(stdout, "true false false\nfalse true true\n");
}

/// The equal-payload answer survives the `list[byte]` bridge round trip,
/// and the empty payloads of two fresh constructions compare equal.
#[test]
fn bytes_equality_compares_across_the_bridge_and_empty() {
    let stdout = run_stdout(concat!(
        "fn main() -> () {\n",
        "  const a = bytes_of([b'x', b'y'])\n",
        "  const back = bytes_of(byte_list(a))\n",
        "  const blank = bytes_of(list[byte]())\n",
        "  const blank2 = bytes_of(list[byte]())\n",
        "  println(\"${a == back} ${a != back}\")\n",
        "  println(\"${blank == blank2} ${blank != a}\")\n",
        "}\n",
    ));
    assert_eq!(stdout, "true false\ntrue true\n");
}

/// The chain runs inside branch conditions and compound boolean
/// expressions — the native window sits mid-expression exactly the
/// concat call shape's.
#[test]
fn bytes_equality_rides_branch_conditions() {
    let stdout = run_stdout(concat!(
        "fn pick(a: bytes, b: bytes) -> string {\n",
        "  if a == b && a != bytes_of([b'z']) { \"same\" } else { \"other\" }\n",
        "}\n",
        "fn main() -> () {\n",
        "  const p = bytes_of([b'a'])\n",
        "  println(pick(p, bytes_of([b'a'])))\n",
        "  println(pick(p, bytes_of([b'b'])))\n",
        "}\n",
    ));
    assert_eq!(stdout, "same\nother\n");
}

/// A struct field of bytes compares by its field type, not its slot
/// kind: the Ref word dispatches the content compare inside the
/// fail-fast field chain, beside an int field and a nested struct's
/// own bytes field.
#[test]
fn a_struct_field_of_bytes_compares_by_content() {
    let stdout = run_stdout(concat!(
        "struct inner { b: bytes }\n",
        "struct pair { b: bytes, n: int, deep: inner }\n",
        "fn main() -> () {\n",
        "  const one = pair { b: bytes_of([b'a', b'b']), n: 1, deep: inner { b: bytes_of([b'c']) \
         } }\n",
        "  const two = pair { b: bytes_of([b'a', b'b']), n: 1, deep: inner { b: bytes_of([b'c']) \
         } }\n",
        "  const swapped = pair { b: bytes_of([b'a', b'd']), n: 1, deep: inner { b: \
         bytes_of([b'c']) } }\n",
        "  const deep_differs = pair { b: bytes_of([b'a', b'b']), n: 1, deep: inner { b: \
         bytes_of([b'z']) } }\n",
        "  const short = pair { b: bytes_of([b'a']), n: 1, deep: inner { b: bytes_of([b'c']) } }\n",
        "  println(\"${one == two} ${one != two}\")\n",
        "  println(\"${one == swapped} ${one == deep_differs} ${one == short}\")\n",
        "}\n",
    ));
    assert_eq!(stdout, "true false\nfalse false false\n");
}

/// A tuple element of bytes compares through the same typed plan: the
/// tuple's per-word chain dispatches the content compare over the
/// Ref element.
#[test]
fn a_tuple_element_of_bytes_compares_by_content() {
    let stdout = run_stdout(concat!(
        "fn main() -> () {\n",
        "  const one = (bytes_of([b'x', b'y']), 7)\n",
        "  const two = (bytes_of([b'x', b'y']), 7)\n",
        "  const swapped = (bytes_of([b'x', b'z']), 7)\n",
        "  const other_int = (bytes_of([b'x', b'y']), 8)\n",
        "  println(\"${one == two} ${one == swapped} ${one == other_int}\")\n",
        "}\n",
    ));
    assert_eq!(stdout, "true false false\n");
}

/// A union payload field of bytes compares through the member's typed
/// plan after the tag dispatch, and a distinct member never compares
/// equal whatever its payload.
#[test]
fn a_union_payload_of_bytes_compares_by_content() {
    let stdout = run_stdout(concat!(
        "enum carrier { empty, loaded { b: bytes } }\n",
        "fn main() -> () {\n",
        "  const one: carrier = loaded(b: bytes_of([b'1', b'2']))\n",
        "  const two: carrier = loaded(b: bytes_of([b'1', b'2']))\n",
        "  const swapped: carrier = loaded(b: bytes_of([b'1', b'3']))\n",
        "  const bare: carrier = empty\n",
        "  println(\"${one == two} ${one != two}\")\n",
        "  println(\"${one == swapped} ${one == bare}\")\n",
        "}\n",
    ));
    assert_eq!(stdout, "true false\nfalse false\n");
}

/// `in` membership over `list[bytes]` compares the element by its type:
/// the one-word Ref element dispatches the content compare per
/// iteration, the EqStr slot-kind path having trapped at runtime.
#[test]
fn membership_over_a_bytes_list_compares_by_content() {
    let stdout = run_stdout(concat!(
        "fn main() -> () {\n",
        "  const xs = [bytes_of([b'a', b'b']), bytes_of([b'c'])]\n",
        "  const hit = bytes_of([b'a', b'b'])\n",
        "  const fresh_hit = bytes_of([b'c'])\n",
        "  const miss = bytes_of([b'a', b'd'])\n",
        "  println(\"${hit in xs} ${fresh_hit in xs} ${miss in xs}\")\n",
        "  println(\"${!(hit in xs)} ${!(miss in xs)}\")\n",
        "}\n",
    ));
    assert_eq!(stdout, "true true false\nfalse true\n");
}

/// Parses, checks, lowers, installs, runs — and asserts the heap drains
/// at run end. The body's own last statement is a constant-string
/// closer so the final tail transfer stays count-free; anything still
/// live then is a placement leak. Returns the captured stdout.
fn run_drains(body: &str) -> String {
    let source = format!("{PRELUDE}{body}");
    let mut files = Files::new();
    let id = files.add_source("drain.ds", &source);
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
        "drain",
        Hash128::compute(source.as_bytes()),
        &asts,
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
        vm.run("drain", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0)))
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
    String::from_utf8(vm.captured_output().to_vec()).expect("stdout is UTF-8")
}

/// A chain that answers at an earlier field still releases the counts
/// the skipped Ref-carrying fields staged: a differing first bytes
/// field with a nested struct's bytes field behind it (the reproducer's
/// three-field shape, in branch, statement, and interpolated spellings,
/// for `==` and `!=`), and the two-field shape's interpolation — the
/// heap drains on every spelling.
#[test]
fn a_fail_fast_chain_releases_the_skipped_fields_counts() {
    let stdout = run_drains(concat!(
        "struct inner { b: bytes }\n",
        "struct pair { b: bytes, n: int, deep: inner }\n",
        "fn main() -> () {\n",
        "  const one = pair { b: bytes_of([b'a']), n: 1, deep: inner { b: bytes_of([b'c']) } }\n",
        "  const swapped = pair { b: bytes_of([b'd']), n: 1, deep: inner { b: bytes_of([b'c']) } \
         }\n",
        "  if one == swapped { println(\"eq\") } else { println(\"ne\") }\n",
        "  one == swapped\n",
        "  println(\"${one == swapped} ${one != swapped}\")\n",
        "  println(\"end\")\n",
        "}\n",
    ));
    assert_eq!(stdout, "ne\nfalse true\nend\n");
}

/// The fail-fast release is not bytes-specific: a differing int first
/// field with a string field behind it (the EqStr read the chain
/// skipped) drains in both spellings.
#[test]
fn a_fail_fast_chain_before_a_string_field_releases_its_reads() {
    let stdout = run_drains(concat!(
        "struct spair { n: int, s: string }\n",
        "fn main() -> () {\n",
        "  const one = spair { n: 1, s: \"ab\" }\n",
        "  const swapped = spair { n: 2, s: \"ab\" }\n",
        "  if one == swapped { println(\"eq\") } else { println(\"ne\") }\n",
        "  println(\"${one == swapped} ${one != swapped}\")\n",
        "  println(\"end\")\n",
        "}\n",
    ));
    assert_eq!(stdout, "ne\nfalse true\nend\n");
}

/// The tuple chain fail-fasts at its first element the same way: the
/// skipped bytes element's staged count releases at the chain's merge.
#[test]
fn a_fail_fast_tuple_chain_releases_the_skipped_bytes_element() {
    let stdout = run_drains(concat!(
        "fn main() -> () {\n",
        "  const one = (1, bytes_of([b'x']))\n",
        "  const swapped = (2, bytes_of([b'x']))\n",
        "  if one == swapped { println(\"eq\") } else { println(\"ne\") }\n",
        "  println(\"${one == swapped} ${one != swapped}\")\n",
        "  println(\"end\")\n",
        "}\n",
    ));
    assert_eq!(stdout, "ne\nfalse true\nend\n");
}

/// A union member's payload chain fail-fasts at an earlier field too:
/// the skipped bytes field's count releases on the member's fail-fast
/// exit and its full path alike.
#[test]
fn a_fail_fast_union_payload_chain_releases_the_skipped_field() {
    let stdout = run_drains(concat!(
        "enum carrier { empty, loaded { n: int, b: bytes } }\n",
        "fn main() -> () {\n",
        "  const one: carrier = loaded(n: 1, b: bytes_of([b'x']))\n",
        "  const swapped: carrier = loaded(n: 2, b: bytes_of([b'x']))\n",
        "  if one == swapped { println(\"eq\") } else { println(\"ne\") }\n",
        "  println(\"${one == swapped} ${one != swapped}\")\n",
        "  println(\"end\")\n",
        "}\n",
    ));
    assert_eq!(stdout, "ne\nfalse true\nend\n");
}

/// Wide `in` membership fail-fasts inside the per-element chain: the
/// skipped Ref-carrying field's handed-out count releases through the
/// chain's merged exits beside the list's own edge counts.
#[test]
fn wide_membership_fail_fast_releases_the_elements_counts() {
    let stdout = run_drains(concat!(
        "struct flat { b: bytes, n: int }\n",
        "fn main() -> () {\n",
        "  const xs = [flat { b: bytes_of([b'a']), n: 1 }, flat { b: bytes_of([b'b']), n: 2 }]\n",
        "  const hit = flat { b: bytes_of([b'b']), n: 2 }\n",
        "  const miss = flat { b: bytes_of([b'z']), n: 9 }\n",
        "  println(\"${hit in xs} ${miss in xs}\")\n",
        "  println(\"end\")\n",
        "}\n",
    ));
    assert_eq!(stdout, "true false\nend\n");
}
