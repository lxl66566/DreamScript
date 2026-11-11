//! End-to-end: the trimmed types_hybrid fixture source parses, checks
//! clean, lowers to a ds-mod module, round-trips through the writer and
//! reader, installs into the VM's registry, and runs with the exact
//! golden stdout of the hand-assembled fixtures in
//! `crates/ds-vm/tests/types_hybrid.rs`.

use ds_abi::SlotType::{self, I64, Raw, Tag};
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, Lowered, lower};
use ds_mod::{Hash128, read_checked, write};
use ds_parse::{parse, parse_script};
use ds_vm::{ModuleRegistry, Value, Vm, loader, native};

/// Builds the module once per test VM: parse -> check -> lower -> write
/// -> read back -> install.
fn build_vm() -> (Vm, Lowered) {
    let source = include_str!("fixtures/types_hybrid_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("types_hybrid_probe.ds", source);
    let asts = [parse(&files, id).expect("the fixture parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the fixture checks clean"
    );
    let lowered = lower(
        "types_hybrid",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers");
    let bytes = write(lowered.module());
    let module = read_checked(&bytes, source.as_bytes()).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    // the loader validates pool FuncRefs at install time, so both native
    // tables must be present first (`Vm::new` would install the builtin
    // core too late for the ds-mod path; the std platform table is this
    // test's to provide, as any platform host must)
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_native(native::STD_PLATFORM_MODULE, native::std_platform_module());
    loader::install(&mut registry, &module).expect("the module installs");
    (Vm::new(registry), lowered)
}

/// A staged `p_num` probe: [Tag, I64, dead Ref].
fn probe_num(vm: &mut Vm, lowered: &Lowered, value: i64) -> Vec<Value> {
    vec![
        Value::Tag(lowered.tag("p_num").expect("p_num has a tag")),
        Value::I64(value),
        vm.intern_str("probe"),
    ]
}

fn probe_str(vm: &mut Vm, lowered: &Lowered, text: &str) -> Vec<Value> {
    vec![
        Value::Tag(lowered.tag("p_str").expect("p_str has a tag")),
        Value::I64(0),
        vm.intern_str(text),
    ]
}

fn probe_none(vm: &mut Vm, lowered: &Lowered) -> Vec<Value> {
    vec![
        Value::Tag(lowered.tag("p_none").expect("p_none has a tag")),
        Value::I64(0),
        vm.intern_str("probe"),
    ]
}

/// Runs `main` over one probe and asserts the golden stdout plus the
/// `same` result word.
fn run_main(probe: fn(&mut Vm, &Lowered) -> Vec<Value>, stdout: &str, same: bool) {
    let (mut vm, lowered) = build_vm();
    vm.capture_output();
    let args = probe(&mut vm, &lowered);
    assert_eq!(
        vm.run("types_hybrid", "main", &args),
        Ok(Value::Bool(same)),
        "the run result is the `same` comparison"
    );
    assert_eq!(
        vm.captured_output(),
        stdout.as_bytes(),
        "the golden stdout of the types_hybrid flow"
    );
}

#[test]
fn ok_path_runs_the_config_to_the_same_true_golden() {
    // a valid port: no handler line, the concatenated host equals the
    // interned one by content (EQSTR across distinct objects)
    run_main(|vm, l| probe_num(vm, l, 8080), "same true\n", true);
}

#[test]
fn bad_port_prints_the_interpolated_handler_message() {
    // port 0 and 70000 both fail the range check: `bad_port { got: -1 }`
    // propagates, the handler prints the interpolation chain
    run_main(
        |vm, l| probe_num(vm, l, 0),
        "bad port -1\nsame true\n",
        true,
    );
    run_main(
        |vm, l| probe_num(vm, l, 70000),
        "bad port -1\nsame true\n",
        true,
    );
}

#[test]
fn missing_and_none_print_the_fallback_handler_line() {
    // `p_none` and `p_str` both miss `p_num`: find_num answers none, the
    // `return missing` early exit fires, the fallback arm prints
    run_main(probe_none, "config read failed\nsame true\n", true);
    run_main(
        |vm, l| probe_str(vm, l, "oops"),
        "config read failed\nsame true\n",
        true,
    );
}

#[test]
fn structural_eq_fails_on_a_differing_payload() {
    // port 9999 passes the range check but differs from the default
    run_main(|vm, l| probe_num(vm, l, 9999), "same false\n", false);
}

#[test]
fn structural_eq_across_unions_covers_members_and_payloads() {
    let case = |a: fn(&mut Vm, &Lowered) -> Vec<Value>,
                b: fn(&mut Vm, &Lowered) -> Vec<Value>|
     -> Result<Value, ds_vm::Trap> {
        let (mut vm, lowered) = build_vm();
        let mut args = a(&mut vm, &lowered);
        args.extend(b(&mut vm, &lowered));
        vm.run("types_hybrid", "probe_same", &args)
    };
    // same member, same payload
    assert_eq!(
        case(|v, l| probe_num(v, l, 8080), |v, l| probe_num(v, l, 8080)),
        Ok(Value::Bool(true))
    );
    // differing i64 payload
    assert_eq!(
        case(|v, l| probe_num(v, l, 8080), |v, l| probe_num(v, l, 80)),
        Ok(Value::Bool(false))
    );
    // member mismatch: p_num against p_str
    assert_eq!(
        case(|v, l| probe_num(v, l, 8080), |v, l| probe_str(v, l, "8080")),
        Ok(Value::Bool(false))
    );
    // string payloads compare by content
    assert_eq!(
        case(|v, l| probe_str(v, l, "ab"), |v, l| probe_str(v, l, "ab")),
        Ok(Value::Bool(true))
    );
    assert_eq!(
        case(|v, l| probe_str(v, l, "ab"), |v, l| probe_str(v, l, "cd")),
        Ok(Value::Bool(false))
    );
    // unit members
    assert_eq!(case(probe_none, probe_none), Ok(Value::Bool(true)));
}

#[test]
fn interpolation_chain_uses_print_and_the_float_conversion() {
    let (mut vm, _) = build_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("types_hybrid", "report", &[Value::F64(2.5)]),
        Ok(Value::F64(2.5))
    );
    assert_eq!(vm.captured_output(), b"value: 2.5\n");
}

#[test]
fn tail_recursion_runs_a_million_deep_at_constant_stack() {
    // a non-tail call at this depth would need ~9M stack words against
    // the VM's 1MiB-word limit, so completing at all proves the CALLT
    // slide kept the stack constant [H2]
    let (mut vm, _) = build_vm();
    assert_eq!(
        vm.run("types_hybrid", "count", &[
            Value::I64(1_000_000),
            Value::I64(0)
        ]),
        Ok(Value::I64(1_000_000))
    );
    assert_eq!(vm.stack_len(), 2, "the metadata word plus the result");
}

/// The layout of one lowered function, read back from the module bytes.
fn fn_layout<'m>(
    module: &'m ds_mod::Module,
    lowered: &Lowered,
    name: &str,
) -> &'m ds_abi::FrameLayout {
    module.functions()[lowered.function_index(name).expect("planned") as usize].layout()
}

/// Lowers one inline source through the full pipeline (parse -> analyze
/// -> lower), asserting the check is clean so failures are the lowerer's.
fn lower_inline(source: &str) -> Result<Lowered, LowerError> {
    let mut files = Files::new();
    let id = files.add_source("inline.ds", source);
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
    lower(
        "inline",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
}

#[test]
fn float_remainder_rejects_at_the_lowering_boundary() {
    // the numeric table admits float rem [B7]; the locked uop set has no
    // ModF64, so both spellings reject at compile time instead of
    // compiling a runtime SlotTypeMismatch trap
    for source in [
        "tag int\ntag float\nfn main() -> float { 7.5 % 2.0 }",
        "tag int\ntag float\nfn main() -> () {\n  var x: float = 7.5\n  x %= 2.0\n}",
    ] {
        let Err(error) = lower_inline(source) else {
            panic!("float rem does not lower: {source}")
        };
        assert!(
            error.to_string().contains("a float remainder"),
            "the error names the seam: {error}"
        );
    }
}

#[test]
fn byte_literals_lower_through_the_integer_slot() {
    // `b'x'` stages as its 0-255 integer value [F6]: the byte atom rides
    // the integer slot encoding while staying outside the int conversion
    // family at the checker; the run-level shapes live in bytes_e2e.rs
    lower_inline("tag byte\nfn main() -> byte { b'x' }").expect("the byte literal lowers");
}

#[test]
fn list_push_rejects_non_scalar_elements_at_the_lowering_boundary() {
    // the direct list_push call stages its value argument as one word;
    // a struct element (multi-word or one field wide) would store its
    // raw field words as the element — a two-field page traps in the VM
    // and a one-field struct silently mis-stages — so both call
    // spellings and both shapes reject here with the reboxing seam named
    // (cons shares the guard with its element in the receiver position)
    const LIST_AMBIENT: &str = concat!(
        "tag int\n",
        "tag string\n",
        "struct list[t]\n",
        "extern fn list_len[t] (xs: list[t]) -> int\n",
        "extern fn list_push[t] (xs: mut list[t], x: t) -> ()\n",
        "extern fn cons[t] (x: t, xs: list[t]) -> list[t]\n",
    );
    for (what, source) in [
        (
            "a cons element",
            format!(
            "{LIST_AMBIENT}struct page {{ url: string, body: string }}\nfn main() -> int {{\n  \
             const pages = list[page]()\n  list_len(cons(page {{ url: \"a\", body: \"b\" }}, \
             pages))\n}}"
        ),
        ),
        (
            "a list_push element",
            format!(
            "{LIST_AMBIENT}struct page {{ url: string, body: string }}\nfn main() -> int {{\n  \
             var pages = list[page]()\n  pages.list_push(page {{ url: \"a\", body: \"b\" }})\n  \
             list_len(pages)\n}}"
        ),
        ),
        (
            "a list_push element",
            format!(
            "{LIST_AMBIENT}struct page {{ url: string, body: string }}\nfn main() -> int {{\n  \
             var pages = list[page]()\n  list_push(pages, page {{ url: \"a\", body: \"b\" }})\n  \
             list_len(pages)\n}}"
        ),
        ),
        (
            "a list_push element",
            format!(
                "{LIST_AMBIENT}struct wrap {{ x: string }}\nfn main() -> int {{\n  var xs = \
                 list[wrap]()\n  xs.list_push(wrap {{ x: \"a\" }})\n  list_len(xs)\n}}"
            ),
        ),
        (
            "a list literal element",
            format!(
                "{LIST_AMBIENT}struct wrap {{ x: string }}\nfn main() -> int {{\n  const xs = \
                 [wrap {{ x: \"a\" }}]\n  list_len(xs)\n}}"
            ),
        ),
    ] {
        let Err(error) = lower_inline(&source) else {
            panic!("{what} lowers: {source}")
        };
        assert!(
            error.to_string().contains(what),
            "the error names the rejection: {error}"
        );
        assert!(
            error.to_string().contains("reboxing"),
            "the error names the future: {error}"
        );
    }
}

#[test]
fn trailing_block_arguments_reject_until_block_argument_sugar_lands() {
    // the block types as the callee's final lambda argument [F7], but
    // routing it through the callable word is block-argument sugar's own
    // lowering round; the inline parentheses spelling is the working
    // form, so the call rejects with that seam
    let Err(error) = lower_inline(
        "tag int\ntag string\nextern fn with_box(p: string, f: fn() -> int) -> int\nfn main() -> \
         int {\n  with_box(\"a\") { 42 }\n}",
    ) else {
        panic!("trailing block arguments do not lower: the sugar awaits its round")
    };
    assert!(
        error.to_string().contains("a trailing block argument"),
        "the error names the seam: {error}"
    );
    assert!(
        error.to_string().contains("inline in parentheses"),
        "the error names the working spelling: {error}"
    );
}

#[test]
fn integer_compound_remainder_assign_runs() {
    let source = "tag int\nfn main() -> int {\n  var x: int = 7\n  x %= 3\n  x\n}";
    let lowered = lower_inline(source).expect("integer %= lowers");
    let bytes = write(lowered.module());
    let module = read_checked(&bytes, source.as_bytes()).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_native(native::STD_PLATFORM_MODULE, native::std_platform_module());
    loader::install(&mut registry, &module).expect("the module installs");
    let mut vm = Vm::new(registry);
    assert_eq!(vm.run("inline", "main", &[]), Ok(Value::I64(1)));
}

#[test]
fn frame_layouts_follow_the_locked_slot_rules() {
    // the trimmed probe union has three members; per BL-6 the parameter
    // frames start at the incoming overlay words with true per-word
    // types: [Tag, I64(got|value|port), Ref(host|text)]
    let (_, lowered) = build_vm();
    let bytes = write(lowered.module());
    let source = include_str!("fixtures/types_hybrid_probe.ds");
    let module = read_checked(&bytes, source.as_bytes())
        .map_err(|e| format!("{e:?}"))
        .expect("reads");
    assert_eq!(
        &fn_layout(&module, &lowered, "find_num").params()[..3],
        [Tag, I64, SlotType::Ref],
        "the union parameter claims the incoming overlay words with true types"
    );
    assert_eq!(fn_layout(&module, &lowered, "find_num").arg_area_len(), 0);
    // count tail-calls with two I64 overlay words: max of staged and result
    assert_eq!(fn_layout(&module, &lowered, "count").arg_area_len(), 2);
    assert_eq!(
        &fn_layout(&module, &lowered, "count").slots()[..2],
        &[I64, I64],
        "agreed shapes stay typed"
    );
    // load stages a probe (Tag, I64, Ref) for get_port whose result
    // spans two words: the own result span (width 4) counts against the
    // callers' overlays, not against its own
    assert_eq!(fn_layout(&module, &lowered, "load").arg_area_len(), 3);
    assert_eq!(fn_layout(&module, &lowered, "main").arg_area_len(), 4);
    // main mixes Tag, Bool, I64 and Ref staging in its first overlay
    // word: the per-word rule widens it to Raw [H1]
    assert_eq!(
        &fn_layout(&module, &lowered, "main").slots()[35..39],
        // word 0 mixes Tag/Ref staging, word 2 mixes Ref/I64 results:
        // both widen to Raw, the agreeing words stay typed
        [Raw, I64, Raw, I64],
        "main's four overlay words"
    );
}

#[test]
fn a_binding_tail_lowers_and_returns_unit() {
    // A block ending in a binding is a statement tail: it lowers, and the
    // frame returns the unit value, never the binding's type. Script mode
    // folds an entry file's statements into exactly this shape, so the
    // tail path is load-bearing for `ds run`.
    let source = "tag int\nfn bump(n: int) -> int { n + 1 }\nconst a = 40\nconst b = bump(a)";
    let mut files = Files::new();
    let id = files.add_source("script.ds", source);
    let asts = [parse_script(&files, id).expect("the entry parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the script checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "script",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the synthesized main lowers");
    let bytes = write(lowered.module());
    let module = read_checked(&bytes, source.as_bytes()).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_native(native::STD_PLATFORM_MODULE, native::std_platform_module());
    loader::install(&mut registry, &module).expect("the module installs");
    let mut vm = Vm::new(registry);
    assert_eq!(vm.run("script", "main", &[]), Ok(Value::I64(0)));
}
