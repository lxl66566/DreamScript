//! The assignment sugar end to end (ruled 2026-11-25): `p.x = v` and a
//! nested field path store into the var-bound root's frame sub-span in
//! place; `xs[i] = v` stages the collection's `set` native exactly like
//! the member call's own spelling (identical behavior by construction);
//! a nesting (`xs[i].x.y = v`, `rows[i][j] = v`) reads the element back
//! through the checked read, mutates the read copy, and stores the
//! element back through the same `set`. A compound operator walks the
//! same chain: the leaf's read copy is the old value, the probe evaluates
//! once, and the result returns through the read's own windows. The
//! place must resolve or halt: a list's own store refuses an
//! out-of-range probe (the plain store's trap), and a map's absent key
//! halts at the read. A field-terminated store whose leaf carries a
//! reference releases the replaced content's count before the store
//! threads — the mut-window member call answering the receiver's own
//! word (`self.free = fl.remove(last)`) included — and the loop drain
//! pins the counts back to baseline.

use ds_check::analyze;
use ds_diag::{ErrorCode, Files, human};
use ds_lower::{Lowered, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Trap, Value, Vm, loader, native};

/// Builds the probe module once per test VM: parse -> check -> lower ->
/// write -> read back -> install, with both native tables in place before
/// the loader's install (pool FuncRefs validate against installed tables).
fn build_vm() -> (Vm, Lowered) {
    let source = include_str!("fixtures/place_assign_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("place_assign_probe.ds", source);
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
    let lowered = lower(
        "place_assign_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    (Vm::new(registry), lowered)
}

#[test]
fn the_place_chains_store_end_to_end() {
    // every leg of the fixture: the field stores (plain, nested, the
    // list-typed field's subscript), the subscript store's identical
    // behavior beside its member-call spelling, the element read-back
    // nesting (a struct element's field, a list of lists), the map's
    // key/value staging, the reference-carrying element counts, and the
    // compound operator over each of those shapes
    let (mut vm, _) = build_vm();
    assert_eq!(
        vm.run("place_assign_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::Bool(true))),
        "every place leg matched"
    );
    assert_eq!(
        vm.objects().len(),
        0,
        "the reference-carrying place stores drain the heap, the loop's compound stores included"
    );
}

#[test]
fn a_field_store_of_a_member_call_answer_drains() {
    // the mut-window member call answering the receiver's own word: the
    // natural `self.free = fl.remove(last)` spelling inside a struct's
    // method, its loop riding one rollover store per iteration, and the
    // discard spelling (`fl.remove(last);`) beside it — the drain leaves
    // neither the replaced field content nor the answer's own count
    let (mut vm, _) = build_vm();
    assert_eq!(
        vm.run("place_assign_probe", "field_member_answer", &[]),
        Ok(FrameAnswer::one(Value::Bool(true))),
        "both spellings kept their list"
    );
    assert_eq!(
        vm.objects().len(),
        0,
        "the store spelling drains like the discard spelling"
    );
}

#[test]
fn a_heap_field_store_releases_the_replaced_content() {
    // two overwrites of a string field: each store's old word releases
    // its count at the store, so neither the first nor the second
    // replaced content survives the drain
    let (mut vm, _) = build_vm();
    assert_eq!(
        vm.run("place_assign_probe", "field_overwrite", &[]),
        Ok(FrameAnswer::one(Value::Bool(true))),
        "both overwrites landed"
    );
    assert_eq!(
        vm.objects().len(),
        0,
        "the replaced field contents die at their stores"
    );
}

#[test]
fn a_compound_store_of_a_member_call_answer_drains() {
    // the compound operator over a member-call RHS (`xs[i] += xs.len()`
    // shaped): the scalar answer folds into the leaf's read copy, the
    // string answer through the concat's own window, and every probe and
    // answer temp dies at its own store-back — the compound branch never
    // shares the plain store's replaced-content hole
    let (mut vm, _) = build_vm();
    assert_eq!(
        vm.run("place_assign_probe", "compound_member_answer", &[]),
        Ok(FrameAnswer::one(Value::Bool(true))),
        "the compound member-call legs answered"
    );
    assert_eq!(
        vm.objects().len(),
        0,
        "the compound member-call stores drain the heap"
    );
}

#[test]
fn an_out_of_range_subscript_store_traps() {
    // the place must resolve or halt: the store carries no miss answer
    // to extract (the checked read's `t | none` stays the probe's own),
    // so the out-of-range index traps naming itself
    let (mut vm, _) = build_vm();
    let trap = vm
        .run("place_assign_probe", "trap_out_of_range", &[])
        .expect_err("the out-of-range store traps");
    assert!(
        matches!(trap, Trap::ListIndex {
            index: 3,
            len: 2,
            ..
        }),
        "the trap names the rejected index: {trap:?}"
    );
}

/// One trap leg of the fixture: the named function halts, and the trap
/// answers `probe`.
fn trap_of(entry: &str, probe: impl FnOnce(&Trap) -> bool) -> Trap {
    let (mut vm, _) = build_vm();
    let trap = vm
        .run("place_assign_probe", entry, &[])
        .expect_err("the place does not resolve, so the store halts");
    assert!(probe(&trap), "the trap names the miss: {trap:?}");
    trap
}

#[test]
fn an_out_of_range_compound_store_traps_like_the_plain_store() {
    // the read's miss never reaches the compute: the list's own store
    // refuses the probe, an int and a string element alike, and the
    // inner probe of a nesting names the inner list
    for entry in ["trap_compound_int", "trap_compound_string"] {
        assert!(
            matches!(trap_of(entry, |_| true), Trap::ListIndex {
                index: 3,
                len: 2,
                ..
            }),
            "{entry}"
        );
    }
    assert!(matches!(
        trap_of("trap_compound_nested", |_| true),
        Trap::ListIndex {
            index: 5,
            len: 2,
            ..
        }
    ));
}

#[test]
fn an_absent_map_key_halts_at_the_read() {
    // a map's store inserts, so nothing downstream refuses: the read's
    // own miss halts through the bare trap primitive, before any word of
    // the miss answer becomes a value — the compound store and the plain
    // nesting share the read
    let expected = Trap::Die {
        message: "the map holds no value under the key; an assignment place must resolve, it \
                  carries no miss answer"
            .to_owned(),
    };
    for entry in ["trap_compound_map", "trap_nesting_map"] {
        assert_eq!(trap_of(entry, |_| true), expected, "{entry}");
    }
}

#[test]
fn a_compound_store_gates_the_root_like_a_plain_store() {
    // the mut-window spelling rules apply unchanged: a `const` root
    // refuses DS3006, and the operator table reads the leaf's declared
    // type, so a string operand over an int element refuses
    let source = format!(
        "{fixture}\n{refusals}",
        fixture = include_str!("fixtures/place_assign_probe.ds"),
        refusals = r#"
fn const_root() -> () {
  const xs = [1, 2]
  xs[0] += 1
}

fn string_over_int() -> () {
  var xs = [1]
  xs[0] += "x"
}
"#
    );
    let mut files = Files::new();
    let id = files.add_source("place_assign_probe.ds", &source);
    let asts = [parse(&files, id).expect("the source parses")];
    let analyzed = analyze(&files, &asts);
    let codes: Vec<ErrorCode> = analyzed
        .checked
        .diags
        .iter()
        .map(|diag| diag.code)
        .collect();
    assert_eq!(
        codes,
        [ErrorCode::MutWindowConstReceiver, ErrorCode::TypeMismatch],
        "{}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
}
