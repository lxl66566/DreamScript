//! The callable-word surface completion (F2): named fn values mint the
//! scalar FuncRef word through the pool const channel (the same channel a
//! non-capturing lambda mints), so a named function passes to
//! map/filter/reduce, binds and calls, and stores in a list for a
//! list_get call; and fn-typed parameter slots are callable-defaulted —
//! a closure's Ref word inhabits them as a borrow (the callee reads the
//! word and never owns its count, each hosted/CALLS copy-in acquiring
//! one count per `Ref` capture that the callee body's placement
//! balances), while the call readers resolve the callee word by its
//! runtime kind. The unproven-callable boundaries refuse at lowering:
//! a closure or a callable parameter as a collection element, a lambda
//! capturing a callable binding, and a native or generic callee's value.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Value, Vm, loader, native};

/// Runs one source end to end: parse -> check -> lower -> write -> read
/// back -> install -> run, asserting the result, the golden stdout, and
/// the heap drain (every staged, adopted, and copy-in count balanced).
fn run_source(name: &str, source: &str) -> (Vm, Result<Value, ds_vm::Trap>) {
    let mut files = Files::new();
    let file_name = format!("{name}.ds");
    let id = files.add_source(&file_name, source);
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
    let lowered = lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed)
        .expect("the source lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_native(native::STD_PLATFORM_MODULE, native::std_platform_module());
    loader::install(&mut registry, &module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    let result = vm.run(name, "main", &[]);
    (vm, result)
}

/// One acceptance: the source runs to the expected result, prints the
/// golden bytes, and drains the heap.
fn acceptance(name: &str, source: &str, result: Value, stdout: &[u8]) {
    let (vm, got) = run_source(name, source);
    assert_eq!(got, Ok(result), "the callable surface answers");
    assert_eq!(vm.captured_output(), stdout, "the golden stdout");
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// One refusal probe: the source must check clean, and lowering must
/// report the named seam.
fn refused(source: &str, needle: &str) {
    let mut files = Files::new();
    let id = files.add_source("refusal_probe.ds", source);
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
    let Err(error) = lower(
        "refusal_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    ) else {
        panic!("the probe refuses at lowering")
    };
    let LowerError::Unsupported { span, what, reason } = &error else {
        panic!("the refusal is Unsupported, got {error:?}")
    };
    assert!(span.start > 0, "the refusal reports at a source span");
    let rendered = format!("{what}: {reason}");
    assert!(
        rendered.contains(needle),
        "the refusal names the seam, got {rendered:?}"
    );
}

/// The declarations every acceptance source shares: the list surface and
/// the container combinators over it, plus the checked read.
const PRELUDE: &str = "
tag int
tag string
tag bool

struct list[t]

extern fn println (s: string) -> ()
extern fn map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]
extern fn filter[t] (xs: list[t], f: fn(t) -> bool) -> list[t]
extern fn reduce[t] (xs: list[t], init: t, f: fn(t, t) -> t) -> t
extern fn len[t] (xs: t) -> int
extern fn list_get[t] (xs: list[t], i: int) -> t | none
";

/// Named fn values pass to the combinators: the same scalar FuncRef word
/// a non-capturing lambda mints rides the CALLG staging, and the hosted
/// per-element call resolves it through the module table.
#[test]
fn named_fn_values_pass_to_the_combinators() {
    acceptance(
        "named_combinators",
        &format!(
            "{PRELUDE}
fn double(x: int) -> int {{ x * 2 }}
fn even(x: int) -> bool {{ x % 2 == 0 }}
fn add(a: int, b: int) -> int {{ a + b }}

fn main() -> int {{
  const xs = [1, 2, 3, 4]
  const doubled = map(xs, double)
  println(\"doubled ${{len(doubled)}} ${{at(doubled, 3)}}\")
  const evens = filter(xs, even)
  println(\"evens ${{len(evens)}}\")
  println(\"sum ${{reduce(xs, 0, add)}}\")
  0
}}

fn at(xs: list[int], i: int) -> int {{
  const cell = list_get(xs, i)
  cell.ok((x: int) => x).err((_: none) => -1)
}}
"
        ),
        Value::I64(0),
        b"doubled 4 8\nevens 2\nsum 10\n",
    );
}

/// A named fn binds then calls, and stores in a list for a list_get call:
/// the minted word is the copyable scalar, so binding aliases it and the
/// stored element hands the same word back through the checked read.
#[test]
fn a_named_fn_binds_calls_and_stores_in_a_list() {
    acceptance(
        "named_values",
        &format!(
            "{PRELUDE}
fn add(a: int, b: int) -> int {{ a + b }}
fn double(x: int) -> int {{ x * 2 }}
fn bump(x: int) -> int {{ x + 1 }}

fn main() -> int {{
  const f = add
  println(\"bound ${{f(3, 4)}}\")
  const fs = [double, bump]
  const second = list_get(fs, 1)
  const called = second.ok((g: fn(int) -> int) -> int {{ g(7) }}).err((_: none) => 0)
  println(\"stored ${{called}}\")
  f(1, 1)
}}
"
        ),
        Value::I64(2),
        b"bound 7\nstored 8\n",
    );
}

/// Closures inhabit callable-defaulted parameters: the user fn-typed
/// parameter takes a capturing closure and a named fn through the one
/// slot classification, and the combinators drive closures per element
/// (the hosted call resolving the closure word's header and captures).
#[test]
fn closures_inhabit_callable_defaulted_parameters() {
    acceptance(
        "closures_apply",
        &format!(
            "{PRELUDE}
fn apply(f: fn(int) -> int, v: int) -> int {{ f(v) }}
fn inc(x: int) -> int {{ x + 1 }}

fn main() -> int {{
  const k = 10
  println(\"closure ${{apply((x: int) => x * k, 4)}}\")
  println(\"named ${{apply(inc, 4)}}\")
  const mapped = map([1, 2, 3], (x: int) => x + k)
  println(\"mapped ${{len(mapped)}}\")
  const kept = filter([1, 2, 3, 4], (x: int) => x + k > 12)
  println(\"kept ${{len(kept)}}\")
  apply(inc, 1)
}}
"
        ),
        Value::I64(2),
        b"closure 40\nnamed 5\nmapped 3\nkept 2\n",
    );
}

/// A closure staged into a callable-defaulted parameter in tail position
/// lowers in place (CALL plus flush plus return, the R13 native-tail
/// shape): the borrow window's dec lands after the call, and the frame's
/// exit drops never run before the callee reads the word.
#[test]
fn a_closure_tail_call_lowers_in_place() {
    acceptance(
        "closure_tail",
        &format!(
            "{PRELUDE}
fn apply(f: fn(int) -> int, v: int) -> int {{ f(v) }}

fn main() -> int {{
  const k = 2
  apply((x: int) => x * k, 21)
}}
"
        ),
        Value::I64(42),
        b"",
    );
}

/// The closure-driven combinators balance across a loop: every hosted
/// copy-in count drops inside its callee, the adopted element counts die
/// with their lists, and the closure word's own count stays with the
/// constructing frame — the heap drains after the last iteration.
#[test]
fn closure_combinators_balance_across_a_loop() {
    // per iteration i: two interpolated words plus the folded
    // (1 + i) + (2 + i) = 3 + 2i, so seen sums 5 + 2i over i in 0..20
    acceptance(
        "closure_balance",
        &format!(
            "{PRELUDE}
fn main() -> int {{
  var seen = 0
  var i = 0
  while i < 20 {{
    const k = i
    const bias = \"!\"
    const words = map([\"a\", \"bb\"], (w: string) => \"${{w}}${{bias}}\")
    const scored = reduce(map([1, 2], (x: int) => x + k), 0, (a: int, b: int) => a + b)
    seen += len(words) + scored
    i += 1
  }}
  println(\"seen ${{seen}}\")
  seen
}}
"
        ),
        Value::I64(20 * 5 + 2 * 190),
        b"seen 480\n",
    );
}

/// A lambda capturing a callable-defaulted parameter refuses: the capture
/// slot classifies Fn while the parameter may hold a closure's Ref word
/// at runtime, and the copy-in would write it there.
#[test]
fn a_lambda_capturing_a_callable_parameter_refuses() {
    refused(
        &format!(
            "{PRELUDE}
fn apply(f: fn(int) -> int) -> int {{
  const g = (x: int) => f(x)
  g(1)
}}

fn main() -> int {{ 0 }}
"
        ),
        "a lambda capturing a callable binding",
    );
}

/// A collection element must prove a non-capturing scalar callable: a
/// fresh closure and a callable-defaulted parameter both refuse — the
/// element staging could not track the closure's count and the read-back
/// would hand a Ref word to a Fn-classified payload slot.
#[test]
fn unproven_callable_collection_elements_refuse() {
    refused(
        &format!(
            "{PRELUDE}
fn main() -> int {{
  const k = 1
  const fs = [(x: int) => x + k]
  0
}}
"
        ),
        "a callable element must prove a non-capturing scalar",
    );
    refused(
        &format!(
            "{PRELUDE}
fn keep(f: fn(int) -> int) -> int {{
  const fs = [f]
  0
}}

fn main() -> int {{ 0 }}
"
        ),
        "a callable element must prove a non-capturing scalar",
    );
}

/// A native callee's name has no callable word: natives run inside their
/// CALL (never through a frame push), so the value use refuses at
/// lowering (a generic's bare name refuses earlier still — the checker
/// types no value for it).
#[test]
fn a_native_fn_value_refuses() {
    refused(
        &format!(
            "{PRELUDE}
fn main() -> int {{
  const p = println
  0
}}
"
        ),
        "only planned module functions mint callable words",
    );
}

/// Parse -> check -> lower, for the op-shape pins (no install or run).
fn lower_only(name: &str, source: &str) -> ds_lower::Lowered {
    let mut files = Files::new();
    let file_name = format!("{name}.ds");
    let id = files.add_source(&file_name, source);
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
    lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed).expect("the source lowers")
}

/// One function's placed uop kinds, RC ops in place.
fn uop_kinds(lowered: &ds_lower::Lowered, name: &str) -> Vec<String> {
    use ds_lower::PlacedOp;
    lowered
        .placed(name)
        .unwrap_or_else(|| panic!("{name} is planned"))
        .iter()
        .map(|op| match op {
            PlacedOp::Uop(uop) => {
                let repr = format!("{uop:?}");
                repr.chars().take_while(char::is_ascii_alphabetic).collect()
            },
            PlacedOp::Rc(_) => "Rc".to_owned(),
        })
        .collect()
}

/// A tail call through a fn-typed binding whose word the tracking proves
/// a count-free scalar (a const alias of a named fn) transfers through
/// CALLT with the named function's own pool FuncRef [H2's closure ruling].
#[test]
fn a_proven_scalar_callee_word_transfers_through_callt() {
    const SOURCE: &str = "
tag int

fn double(x: int) -> int { x * 2 }

fn relay(n: int) -> int {
  const f = double
  f(n)
}

fn main() -> int { relay(21) }
";
    let lowered = lower_only("proven_scalar_tail", SOURCE);
    let relay = uop_kinds(&lowered, "relay");
    assert!(
        relay.iter().any(|k| k == "Callt") && !relay.iter().any(|k| k == "Calls"),
        "the proven count-free callee word tail-transfers: {relay:?}"
    );
    let (vm, got) = run_source("proven_scalar_tail", SOURCE);
    assert_eq!(got, Ok(Value::I64(42)));
    assert!(vm.objects().is_empty(), "the heap drains");
}

/// A non-capturing lambda bound in the frame mints the same proven scalar
/// word, so its tail call transfers through CALLT too — the
/// closure-staging surface's count-free half.
#[test]
fn a_non_capturing_lambda_callee_word_transfers_through_callt() {
    const SOURCE: &str = "
tag int

fn relay(n: int) -> int {
  const f = (x: int) => x + 1
  f(n)
}

fn main() -> int { relay(41) }
";
    let lowered = lower_only("noncap_lambda_tail", SOURCE);
    let relay = uop_kinds(&lowered, "relay");
    assert!(
        relay.iter().any(|k| k == "Callt") && !relay.iter().any(|k| k == "Calls"),
        "the non-capturing lambda word is the proven scalar: {relay:?}"
    );
    let (vm, got) = run_source("noncap_lambda_tail", SOURCE);
    assert_eq!(got, Ok(Value::I64(42)));
    assert!(vm.objects().is_empty(), "the heap drains");
}

/// A callable-defaulted parameter's word is maybe-Ref at runtime (a
/// closure may inhabit it), so tracking cannot prove it count-free: the
/// tail lowers in place, CALLS plus return, exactly as before the ruling.
#[test]
fn a_callable_parameter_tail_stays_in_place() {
    const SOURCE: &str = "
tag int

fn double(x: int) -> int { x * 2 }

fn relay(f: fn(int) -> int, n: int) -> int {
  f(n)
}

fn main() -> int { relay(double, 21) }
";
    let lowered = lower_only("callable_param_tail", SOURCE);
    let relay = uop_kinds(&lowered, "relay");
    assert!(
        relay.iter().any(|k| k == "Calls") && !relay.iter().any(|k| k == "Callt"),
        "the maybe-Ref parameter word keeps the in-place CALLS tail: {relay:?}"
    );
    assert!(
        relay.iter().any(|k| k == "Ret"),
        "the in-place tail returns its result: {relay:?}"
    );
    let (vm, got) = run_source("callable_param_tail", SOURCE);
    assert_eq!(got, Ok(Value::I64(42)));
    assert!(vm.objects().is_empty(), "the heap drains");
}

/// A capturing lambda's bound word is the closure object's Ref — a count
/// the transfer cannot settle — so its tail stays in place behind the
/// runtime CALLS read.
#[test]
fn a_capturing_lambda_callee_tail_stays_in_place() {
    const SOURCE: &str = "
tag int

fn relay(n: int) -> int {
  const f = (x: int) => x + n
  f(n)
}

fn main() -> int { relay(20) }
";
    let lowered = lower_only("capturing_lambda_tail", SOURCE);
    let relay = uop_kinds(&lowered, "relay");
    assert!(
        relay.iter().any(|k| k == "Calls") && !relay.iter().any(|k| k == "Callt"),
        "the closure's Ref word keeps the in-place CALLS tail: {relay:?}"
    );
    let (vm, got) = run_source("capturing_lambda_tail", SOURCE);
    assert_eq!(got, Ok(Value::I64(40)));
    assert!(vm.objects().is_empty(), "the heap drains");
}
