//! The list and map collections' structural `==`/`!=` end to end [B9]:
//! the operands check clean through the string's one Ref word shape, and
//! the chain dispatches on the collection's content-compare native —
//! equal content compares equal behind distinct objects (a nested
//! reference pair compares the same way, so a list of lists and a map
//! over string values compose), a differing element, row, or length
//! unequal, `!=` the same chain's inverted bool. Both anonymous operands
//! (fresh literals, fresh builder calls) and bound names stage, so the
//! native window's borrowed counts balance on every spelling. The
//! per-word chains compare fields by their type, never their slot kind
//! alone: a struct, tuple, or union field of list or map type — the
//! linked-type facts mint exactly these `list[scalar]` fields — and `in`
//! membership over list elements (a list of lists, a one-word struct
//! element whose field is a collection) all dispatch the content compare
//! — the EqStr uop reads string objects alone, so the slot-kind chain
//! trapped at runtime over a collection word. An element kind without
//! content equality (a callable, a union element) refuses at lowering
//! naming the seam, never a silent EqStr fallback. A chain that answers
//! at an earlier field releases the skipped Ref-carrying fields' staged
//! counts at its merged exits, so the heap drains on the fail-fast
//! spellings too.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, lower};
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
    "struct map[k, v]\n",
    "extern fn println (s: string) -> ()\n",
    "extern fn bytes_of (xs: list[byte]) -> bytes\n",
    "extern fn map_new[k, v] () -> map[k, v]\n",
    "extern fn map_set[k, v] (m: mut map[k, v], key: k, value: v) -> ()\n",
    "extern fn list_push[t] (xs: mut list[t], x: t) -> ()\n",
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

/// Parses, checks, and lowers one inline source, asserting the named
/// refusal the collection word's element gate answers.
fn lower_refusal(body: &str, refused: &str) {
    let source = format!("{PRELUDE}{body}");
    let mut files = Files::new();
    let id = files.add_source("refuse.ds", &source);
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
    match lower(
        "refuse",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    ) {
        Err(LowerError::Unsupported { what, .. }) => assert!(
            what.contains(refused),
            "the refusal names the seam (got {what})"
        ),
        Err(other) => panic!("the source refuses at lowering (got {other:?})"),
        Ok(_) => panic!("the source refuses at lowering (it lowered)"),
    }
}

/// The acceptance over the plain list pair: equal elements compare equal
/// behind distinct objects, a differing element and a differing length
/// compare unequal, the empty lists compare equal, and `!=` inverts the
/// same chain.
#[test]
fn list_equality_answers_element_content() {
    let stdout = run_stdout(concat!(
        "fn main() -> () {\n",
        "  const one = [1, 2, 3]\n",
        "  const two = [1, 2, 3]\n",
        "  const swapped = [1, 2, 4]\n",
        "  const short = [1, 2]\n",
        "  const blank = list[int]()\n",
        "  const blank2 = list[int]()\n",
        "  println(\"${one == two} ${one == swapped} ${one == short}\")\n",
        "  println(\"${one != two} ${one != swapped} ${one != short}\")\n",
        "  println(\"${blank == blank2} ${blank == one}\")\n",
        "}\n",
    ));
    assert_eq!(stdout, "true false false\nfalse true true\ntrue false\n");
}

/// The plain map pair answers row content: equal rows compare equal
/// behind distinct objects, a differing value or a missing key unequal,
/// and the value channel composes over a collection (a `list[int]`
/// value compares through the nested walk).
#[test]
fn map_equality_answers_row_content() {
    let stdout = run_stdout(concat!(
        "fn main() -> () {\n",
        "  var one = map[string, int]()\n",
        "  map_set(one, \"a\", 1)\n",
        "  map_set(one, \"b\", 2)\n",
        "  var two = map[string, int]()\n",
        "  map_set(two, \"a\", 1)\n",
        "  map_set(two, \"b\", 2)\n",
        "  var swapped = map[string, int]()\n",
        "  map_set(swapped, \"a\", 1)\n",
        "  map_set(swapped, \"b\", 3)\n",
        "  var missing = map[string, int]()\n",
        "  map_set(missing, \"a\", 1)\n",
        "  var blank = map[string, int]()\n",
        "  var blank2 = map[string, int]()\n",
        "  println(\"${one == two} ${one == swapped} ${one == missing}\")\n",
        "  println(\"${one != two} ${blank == blank2} ${blank == one}\")\n",
        "  var nest_one = map[string, list[int]]()\n",
        "  map_set(nest_one, \"k\", [1, 2])\n",
        "  var nest_two = map[string, list[int]]()\n",
        "  map_set(nest_two, \"k\", [1, 2])\n",
        "  var nest_diff = map[string, list[int]]()\n",
        "  map_set(nest_diff, \"k\", [1, 3])\n",
        "  println(\"${nest_one == nest_two} ${nest_one == nest_diff}\")\n",
        "}\n",
    ));
    assert_eq!(stdout, "true false false\nfalse true false\ntrue false\n");
}

/// The chain runs inside branch conditions and compound boolean
/// expressions — the native window sits mid-expression exactly the
/// concat call shape's.
#[test]
fn collection_equality_rides_branch_conditions() {
    let stdout = run_stdout(concat!(
        "fn pick(a: list[int], b: list[int]) -> string {\n",
        "  if a == b && a != [9] { \"same\" } else { \"other\" }\n",
        "}\n",
        "fn main() -> () {\n",
        "  const p = [7]\n",
        "  println(pick(p, [7]))\n",
        "  println(pick(p, [8]))\n",
        "}\n",
    ));
    assert_eq!(stdout, "same\nother\n");
}

/// A struct field of lists compares by its field type, not its slot
/// kind: the Ref word dispatches the content compare inside the
/// fail-fast field chain, beside an int field and a nested struct's own
/// collection field (a `list[bytes]` beside a `list[string]`).
#[test]
fn a_struct_field_of_lists_compares_by_content() {
    let stdout = run_stdout(concat!(
        "struct inner { b: list[bytes] }\n",
        "struct pair { tags: list[string], n: int, deep: inner }\n",
        "fn main() -> () {\n",
        "  const one = pair { tags: [\"a\", \"b\"], n: 1, deep: inner { b: [bytes_of([b'c'])] } \
         }\n",
        "  const two = pair { tags: [\"a\", \"b\"], n: 1, deep: inner { b: [bytes_of([b'c'])] } \
         }\n",
        "  const swapped = pair { tags: [\"a\", \"d\"], n: 1, deep: inner { b: [bytes_of([b'c'])] \
         } }\n",
        "  const deep_differs = pair { tags: [\"a\", \"b\"], n: 1, deep: inner { b: \
         [bytes_of([b'z'])] } }\n",
        "  const short = pair { tags: [\"a\"], n: 1, deep: inner { b: [bytes_of([b'c'])] } }\n",
        "  println(\"${one == two} ${one != two}\")\n",
        "  println(\"${one == swapped} ${one == deep_differs} ${one == short}\")\n",
        "}\n",
    ));
    assert_eq!(stdout, "true false\nfalse false false\n");
}

/// A struct field of maps compares the same way: the whole-map Ref word
/// dispatches the row compare inside the field chain.
#[test]
fn a_struct_field_of_maps_compares_by_content() {
    let stdout = run_stdout(concat!(
        "struct config { n: int, table: map[string, int] }\n",
        "fn main() -> () {\n",
        "  var table_one = map[string, int]()\n",
        "  map_set(table_one, \"k\", 1)\n",
        "  var table_two = map[string, int]()\n",
        "  map_set(table_two, \"k\", 1)\n",
        "  var table_diff = map[string, int]()\n",
        "  map_set(table_diff, \"k\", 2)\n",
        "  const one = config { n: 1, table: table_one }\n",
        "  const two = config { n: 1, table: table_two }\n",
        "  const swapped = config { n: 1, table: table_diff }\n",
        "  println(\"${one == two} ${one == swapped} ${one != two}\")\n",
        "}\n",
    ));
    assert_eq!(stdout, "true false false\n");
}

/// A tuple element of list type compares through the same typed plan.
#[test]
fn a_tuple_element_of_lists_compares_by_content() {
    let stdout = run_stdout(concat!(
        "fn main() -> () {\n",
        "  const one = ([\"a\"], 7)\n",
        "  const two = ([\"a\"], 7)\n",
        "  const swapped = ([\"b\"], 7)\n",
        "  const other_int = ([\"a\"], 8)\n",
        "  println(\"${one == two} ${one == swapped} ${one == other_int}\")\n",
        "}\n",
    ));
    assert_eq!(stdout, "true false false\n");
}

/// A union payload field of list type compares through the member's
/// typed plan after the tag dispatch, and a distinct member never
/// compares equal whatever its payload.
#[test]
fn a_union_payload_of_lists_compares_by_content() {
    let stdout = run_stdout(concat!(
        "enum carrier { empty, loaded { tags: list[string] } }\n",
        "fn main() -> () {\n",
        "  const one: carrier = loaded(tags: [\"1\", \"2\"])\n",
        "  const two: carrier = loaded(tags: [\"1\", \"2\"])\n",
        "  const swapped: carrier = loaded(tags: [\"1\", \"3\"])\n",
        "  const bare: carrier = empty\n",
        "  println(\"${one == two} ${one != two}\")\n",
        "  println(\"${one == swapped} ${one == bare}\")\n",
        "}\n",
    ));
    assert_eq!(stdout, "true false\nfalse false\n");
}

/// Nested collections compose through the native's own walk: a list of
/// lists, a list of structs carrying list fields, and a list of lists of
/// bytes compare by full content.
#[test]
fn nested_collections_compose() {
    let stdout = run_stdout(concat!(
        "struct tagged { tags: list[string] }\n",
        "fn main() -> () {\n",
        "  const one = [[1, 2], [3]]\n",
        "  const two = [[1, 2], [3]]\n",
        "  const diff = [[1, 2], [4]]\n",
        "  println(\"${one == two} ${one == diff}\")\n",
        "  const rows_one = [tagged { tags: [\"x\"] }, tagged { tags: [] }]\n",
        "  const rows_two = [tagged { tags: [\"x\"] }, tagged { tags: [] }]\n",
        "  const rows_diff = [tagged { tags: [\"y\"] }, tagged { tags: [] }]\n",
        "  println(\"${rows_one == rows_two} ${rows_one == rows_diff}\")\n",
        "  const blobs_one = [[bytes_of([b'a'])], [bytes_of([b'b'])]]\n",
        "  const blobs_two = [[bytes_of([b'a'])], [bytes_of([b'b'])]]\n",
        "  const blobs_diff = [[bytes_of([b'a'])], [bytes_of([b'c'])]]\n",
        "  println(\"${blobs_one == blobs_two} ${blobs_one == blobs_diff}\")\n",
        "}\n",
    ));
    assert_eq!(stdout, "true false\ntrue false\ntrue false\n");
}

/// `in` membership compares the element by its type: a list element
/// (over a list of lists) dispatches the content compare per iteration,
/// a wide struct element rides the structural chain per element, and a
/// one-word struct element whose single field is a collection dispatches
/// the content compare too — the EqStr slot-kind path having trapped at
/// runtime over the collection word.
#[test]
fn membership_over_collection_lists_compares_by_content() {
    let stdout = run_stdout(concat!(
        "struct tagged { tags: list[string] }\n",
        "fn main() -> () {\n",
        "  const xs = [[\"a\"], [\"b\"]]\n",
        "  println(\"${[\"a\"] in xs} ${[\"c\"] in xs}\")\n",
        "  const rows = [tagged { tags: [\"x\"] }, tagged { tags: [] }]\n",
        "  println(\"${tagged { tags: [\"x\"] } in rows} ${tagged { tags: [\"q\"] } in rows}\")\n",
        "  const wide = [(1, [\"a\"]), (2, [\"b\"])]\n",
        "  println(\"${(2, [\"b\"]) in wide} ${(1, [\"b\"]) in wide}\")\n",
        "}\n",
    ));
    assert_eq!(stdout, "true false\ntrue false\ntrue false\n");
}

/// An element kind without content equality refuses at lowering naming
/// the seam — never a silent EqStr fallback that traps at runtime: a
/// callable element and a union element over the list word, a union
/// value over the map word.
#[test]
fn elements_without_content_equality_refuse_at_lowering() {
    lower_refusal(
        concat!(
            "fn id(x: int) -> int { x }\n",
            "fn main() -> () {\n",
            "  const one = [id]\n",
            "  const two = [id]\n",
            "  println(\"${one == two}\")\n",
            "}\n",
        ),
        "a structural equality over list elements",
    );
    lower_refusal(
        concat!(
            "enum grade { plain, scored(int) }\n",
            "fn main() -> () {\n",
            "  var one = list[grade]()\n",
            "  list_push(one, plain)\n",
            "  var two = list[grade]()\n",
            "  list_push(two, plain)\n",
            "  println(\"${one == two}\")\n",
            "}\n",
        ),
        "a structural equality over list elements",
    );
    lower_refusal(
        concat!(
            "enum grade { plain, scored(int) }\n",
            "fn main() -> () {\n",
            "  var one = map[string, grade]()\n",
            "  map_set(one, \"k\", plain)\n",
            "  var two = map[string, grade]()\n",
            "  map_set(two, \"k\", plain)\n",
            "  println(\"${one == two}\")\n",
            "}\n",
        ),
        "a structural equality over map values",
    );
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
/// the skipped Ref-carrying fields staged: a differing first int field
/// with a list field and a nested struct's list-of-bytes field behind it
/// (in branch, statement, and interpolated spellings, for `==` and
/// `!=`) — the heap drains on every spelling.
#[test]
fn a_fail_fast_chain_releases_the_skipped_collection_fields_counts() {
    let stdout = run_drains(concat!(
        "struct inner { b: list[bytes] }\n",
        "struct pair { tags: list[string], n: int, deep: inner }\n",
        "fn main() -> () {\n",
        "  const one = pair { tags: [\"a\"], n: 1, deep: inner { b: [bytes_of([b'c'])] } }\n",
        "  const swapped = pair { tags: [\"d\"], n: 1, deep: inner { b: [bytes_of([b'c'])] } }\n",
        "  if one == swapped { println(\"eq\") } else { println(\"ne\") }\n",
        "  one == swapped\n",
        "  println(\"${one == swapped} ${one != swapped}\")\n",
        "  const other = pair { tags: [\"a\"], n: 2, deep: inner { b: [bytes_of([b'c'])] } }\n",
        "  println(\"${one == other}\")\n",
        "  println(\"end\")\n",
        "}\n",
    ));
    assert_eq!(stdout, "ne\nfalse true\nfalse\nend\n");
}

/// A differing map field behind a matching first field releases the
/// map's staged count at the merged exit too.
#[test]
fn a_fail_fast_chain_before_a_map_field_releases_its_count() {
    let stdout = run_drains(concat!(
        "struct config { n: int, table: map[string, int] }\n",
        "fn main() -> () {\n",
        "  var table_one = map[string, int]()\n",
        "  map_set(table_one, \"k\", 1)\n",
        "  var table_two = map[string, int]()\n",
        "  map_set(table_two, \"k\", 1)\n",
        "  const one = config { n: 1, table: table_one }\n",
        "  const swapped = config { n: 2, table: table_two }\n",
        "  if one == swapped { println(\"eq\") } else { println(\"ne\") }\n",
        "  println(\"${one == swapped} ${one != swapped}\")\n",
        "  println(\"end\")\n",
        "}\n",
    ));
    assert_eq!(stdout, "ne\nfalse true\nend\n");
}

/// Membership fail-fasts inside the loop over collection elements: the
/// staged operand and element counts release through the loop's exits
/// beside the list's own edge counts.
#[test]
fn membership_over_collections_drains() {
    let stdout = run_drains(concat!(
        "struct tagged { tags: list[string] }\n",
        "fn main() -> () {\n",
        "  const xs = [[\"a\"], [\"b\"]]\n",
        "  println(\"${[\"b\"] in xs} ${[\"c\"] in xs}\")\n",
        "  const rows = [tagged { tags: [\"x\"] }, tagged { tags: [] }]\n",
        "  println(\"${tagged { tags: [] } in rows}\")\n",
        "  println(\"end\")\n",
        "}\n",
    ));
    assert_eq!(stdout, "true false\ntrue\nend\n");
}
