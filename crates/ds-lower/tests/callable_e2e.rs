//! The callable-word surface completion: named fn values mint the
//! scalar FuncRef word through the pool const channel (the same channel a
//! non-capturing lambda mints), so a named function passes to
//! map/filter/reduce, binds and calls, and stores in a list for a
//! subscript call; and fn-typed parameter slots are callable-defaulted —
//! a closure's Ref word inhabits them as a borrow (the callee reads the
//! word and never owns its count, each hosted/CALLS copy-in acquiring
//! one count per `Ref` capture that the callee body's placement
//! balances), while the call readers resolve the callee word by its
//! runtime kind. A fn-typed struct field, tuple element, and collection
//! element hold the closure Ref word: a non-capturing fn entering one of
//! those positions wraps into its static closure object, a capturing
//! closure or an unproven callable word stages as itself, and a lambda
//! may capture a fn-typed value — the capture block carrying whatever
//! word the binding holds. The frame's re-storable slots keep the
//! scalar rule: a closure or an unproven callable word refuses at a var
//! span, at a union region's fn-typed payload slot (the binding, the
//! re-assignment, the call argument, the join arm, the return — every
//! construction site funnels through the widening write, ok_or's err
//! arm staging directly), and at the fn-typed word answers where they
//! stage: unwrap_or's and unwrap_or_else's fallbacks, the .ok/.err
//! handler bodies, the same-type join arms, and the fn-typed return.
//! A computed callee (`f()()`, a subscript read's word) lowers through a
//! temporary binding, the indirect call reading it like any binding's.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// Runs one source end to end: parse -> check -> lower -> write -> read
/// back -> install -> run, asserting the result, the golden stdout, and
/// the heap drain (every staged, adopted, and copy-in count balanced).
fn run_source(name: &str, source: &str) -> (Vm, Result<FrameAnswer, ds_vm::Trap>) {
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
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    let result = vm.run(name, "main", &[]);
    (vm, result)
}

/// One acceptance: the source runs to the expected result, prints the
/// golden bytes, and drains the heap.
fn acceptance(name: &str, source: &str, result: Value, stdout: &[u8]) {
    let (vm, got) = run_source(name, source);
    assert_eq!(
        got,
        Ok(FrameAnswer::one(result)),
        "the callable surface answers"
    );
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

/// A named fn binds then calls, and stores in a list: the minted word
/// wraps into the function's static closure object at the literal's
/// store, the element holds the closure Ref word, and the subscript
/// read hands it out under its own count for the computed call.
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
  const called = fs[1](7)
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
/// lowers in place (CALL plus flush plus return, the native-tail
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

/// A lambda captures a fn-typed value: the callable-defaulted
/// parameter's maybe-Ref word stages into the capture block as itself,
/// the construction acquiring one count per runtime Ref inside the op,
/// and the body's call dispatches the word's kind.
#[test]
fn a_lambda_capturing_a_callable_parameter_runs() {
    acceptance(
        "capture_callable",
        &format!(
            "{PRELUDE}
fn apply(f: fn(int) -> int) -> int {{
  const g = (x: int) => f(x)
  g(1)
}}

fn main() -> int {{ apply((x: int) => x + 40) }}
"
        ),
        Value::I64(41),
        b"",
    );
}

/// A collection element holds the closure Ref word: a capturing lambda
/// and a named fn both inhabit the element (the staging wrapping the
/// proven scalar, the channel's count traffic covering the Ref), and
/// the subscript read hands the word out for the computed call — the
/// capture-composing shape the @std race combinators ride. An unproven
/// callable word (a callable-defaulted parameter's) refuses: its
/// runtime kind is the caller's fact, and the element's Ref slot takes
/// the closure Ref only.
#[test]
fn callables_inhabit_collection_elements() {
    acceptance(
        "callable_elements",
        &format!(
            "{PRELUDE}
fn main() -> int {{
  const k = 10
  const thunks = [(x: int) => x + k, (x: int) => x * k]
  println(\"race ${{thunks[0](1) + thunks[1](2)}}\")
  0
}}
"
        ),
        Value::I64(0),
        b"race 31\n",
    );
    refused(
        &format!(
            "{PRELUDE}
fn keep(f: fn(int) -> int) -> int {{
  const fs = [f]
  fs[0](6)
}}

fn main() -> int {{ keep((x: int) => x * 7) }}
"
        ),
        "a callable word's runtime kind is the caller's fact",
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
/// CALLT with the named function's own pool FuncRef.
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
    assert_eq!(got, Ok(FrameAnswer::one(Value::I64(42))));
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
    assert_eq!(got, Ok(FrameAnswer::one(Value::I64(42))));
    assert!(vm.objects().is_empty(), "the heap drains");
}

/// A callable-defaulted parameter's word is maybe-Ref at runtime (a
/// closure may inhabit it), so tracking cannot prove it count-free: the
/// tail lowers in place, CALLS plus return.
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
    assert_eq!(got, Ok(FrameAnswer::one(Value::I64(42))));
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
    assert_eq!(got, Ok(FrameAnswer::one(Value::I64(40))));
    assert!(vm.objects().is_empty(), "the heap drains");
}

/// An unproven callable in a var binding refuses: the callable-defaulted
/// parameter's word may hold a closure's Ref at runtime, and the var's
/// re-storable fn-typed slot traps at the store (the same provenance the
/// collection elements demand).
#[test]
fn an_unproven_callable_in_a_var_binding_refuses() {
    refused(
        &format!(
            "{PRELUDE}
fn take(f: fn(int) -> int) -> int {{
  var g = f
  g(1)
}}

fn main() -> int {{ 0 }}
"
        ),
        "an unproven callable in a var binding",
    );
    // the re-assignment path shares the guard
    refused(
        &format!(
            "{PRELUDE}
fn one(x: int) -> int {{ x + 1 }}
fn take(f: fn(int) -> int) -> int {{
  var g: fn(int) -> int = one
  g = f
  g(1)
}}

fn main() -> int {{ 0 }}
"
        ),
        "an unproven callable in a var binding",
    );
}

/// The just-inside sides run: a named fn and a non-capturing lambda (the
/// proven scalar words) inhabit and re-assign a re-storable fn-typed var
/// slot. A fn-typed tuple element destructuring into a var refuses: the
/// element holds the closure Ref word and the re-storable var slot
/// stays scalar.
#[test]
fn proven_scalars_inhabit_re_storable_var_slots() {
    acceptance(
        "var_scalar_words",
        &format!(
            "{PRELUDE}
fn one(x: int) -> int {{ x + 1 }}
fn two(x: int) -> int {{ x * 3 }}

fn main() -> int {{
  var v = one
  println(\"first ${{v(7)}}\")
  v = two
  println(\"second ${{v(7)}}\")
  var l = (x: int) => x - 1
  println(\"lambda ${{l(7)}}\")
  l(0)
}}
"
        ),
        Value::I64(-1),
        b"first 8\nsecond 21\nlambda 6\n",
    );
    refused(
        &format!(
            "{PRELUDE}
fn two(x: int) -> int {{ x * 3 }}

fn main() -> int {{
  var (g, n) = (two, 4)
  g(n)
}}
"
        ),
        "a var destructuring element word",
    );
}

/// A tuple element holds the closure Ref word: the span's count traffic
/// copies the Ref-classified field, a capturing lambda rides one, and
/// the destructured binder aliases the element's word for the call. An
/// unproven callable word refuses at the literal (the runtime kind the
/// caller's fact, the element's Ref slot taking the closure Ref only).
#[test]
fn callables_inhabit_tuple_elements() {
    acceptance(
        "callable_tuple_elements",
        &format!(
            "{PRELUDE}
fn main() -> int {{
  const k = 4
  const (tf, tn) = ((x: int) => x + k, 8)
  println(\"tuple ${{tf(tn)}}\")
  0
}}
"
        ),
        Value::I64(0),
        b"tuple 12\n",
    );
    refused(
        &format!(
            "{PRELUDE}
fn take(f: fn(int) -> int) -> int {{
  const (g, n) = (f, 6)
  g(n)
}}

fn main() -> int {{ take((x: int) => x * 7) }}
"
        ),
        "a callable word's runtime kind is the caller's fact",
    );
}

/// A fn-typed struct field holds the closure Ref word: the field's span
/// is the one Ref word the count traffic covers, a named fn and a
/// capturing lambda both construct through the literal (the named fn
/// wrapping into its static closure object), and the field read's word
/// stages as a callable argument. The dot-call spelling through a field
/// name stays refused at the check (the resolver's dot tiers name free
/// functions and members, never fields).
#[test]
fn fn_typed_struct_fields_hold_closures() {
    acceptance(
        "fn_fields",
        &format!(
            "{PRELUDE}
struct box {{ f: fn(int) -> int }}

fn one(x: int) -> int {{ x + 1 }}
fn apply(f: fn(int) -> int, v: int) -> int {{ f(v) }}

fn main() -> int {{
  const k = 2
  const a = box {{ f: one }}
  const b = box {{ f: (x: int) => x + k }}
  println(\"a ${{apply(a.f, 1)}} b ${{apply(b.f, 2)}}\")
  0
}}
"
        ),
        Value::I64(0),
        b"a 2 b 4\n",
    );
}

/// A generic fn's bare name has no callable word: the generic exemption
/// skips its body, so the value use refuses naming the minting seam —
/// the same message the native callee's name keeps. Passing it where a
/// callable word stages refuses earlier, at checking (the fn type
/// escapes containment).
#[test]
fn a_generic_fn_value_refuses() {
    refused(
        &format!(
            "{PRELUDE}
fn id[t] (x: t) -> t {{ x }}

fn main() -> int {{
  const p = id
  0
}}
"
        ),
        "the word carries no instantiation",
    );
    let mut files = Files::new();
    let id = files.add_source(
        "generic_arg.ds",
        &format!(
            "{PRELUDE}
fn id[t] (x: t) -> t {{ x }}

fn main() -> int {{
  const ys = map([1, 2], id)
  0
}}
"
        ),
    );
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        !analyzed.checked.diags.is_empty(),
        "a generic fn argument refuses at checking"
    );
}

/// A callable word widened into a union region's fn-typed payload slot
/// keeps the var slot's provenance rule: the payload word is
/// Fn-classified and every extraction re-reads it, so an unproven
/// callable word (a callable-defaulted binding's, an alias of one) or a
/// capturing closure lands a Ref word there and traps — every
/// construction site (the annotated binding, the re-assignment, the
/// call argument, the join arm, the return) refuses at its own span.
#[test]
fn unproven_callables_in_union_payload_slots_refuse() {
    // the annotated const binding, and the read-back that would call it
    refused(
        &format!(
            "{PRELUDE}
fn take(f: fn(int) -> int) -> int {{
  const g: (fn(int) -> int) | none = f
  0
}}

fn main() -> int {{ take((x: int) => x) }}
"
        ),
        "an unproven callable in a union payload slot",
    );
    // a var over the union type: the binding and the re-assignment share
    // the seam
    refused(
        &format!(
            "{PRELUDE}
fn take(f: fn(int) -> int) -> int {{
  var g: (fn(int) -> int) | none = none
  g = f
  0
}}

fn main() -> int {{ take((x: int) => x) }}
"
        ),
        "an unproven callable in a union payload slot",
    );
    // the call argument into a union-typed parameter
    refused(
        &format!(
            "{PRELUDE}
fn eat(u: (fn(int) -> int) | none) -> int {{ 0 }}
fn take(f: fn(int) -> int) -> int {{ eat(f) }}

fn main() -> int {{ take((x: int) => x) }}
"
        ),
        "an unproven callable in a union payload slot",
    );
    // the if-join arm
    refused(
        &format!(
            "{PRELUDE}
fn take(f: fn(int) -> int, c: bool) -> int {{
  const g = if c {{ f }} else {{ none }}
  0
}}

fn main() -> int {{ take((x: int) => x, true) }}
"
        ),
        "an unproven callable in a union payload slot",
    );
    // the return into a union-typed return
    refused(
        &format!(
            "{PRELUDE}
fn take(f: fn(int) -> int) -> (fn(int) -> int) | none {{
  f
}}

fn main() -> int {{
  const h = take((x: int) => x)
  0
}}
"
        ),
        "an unproven callable in a union payload slot",
    );
}

/// The closure family of the same slot: a capturing closure (a
/// Ref-classified word) refuses at the join, the return, and a var
/// binding over the union type — the annotated const binding keeps its
/// own earlier refusal message.
#[test]
fn closures_in_union_payload_slots_refuse() {
    refused(
        &format!(
            "{PRELUDE}
fn make(k: int, c: bool) -> int {{
  const g = if c {{ (x: int) => x + k }} else {{ none }}
  0
}}

fn main() -> int {{ make(1, true) }}
"
        ),
        "a capturing closure in a union payload slot",
    );
    refused(
        &format!(
            "{PRELUDE}
fn make(k: int) -> (fn(int) -> int) | none {{
  (x: int) => x + k
}}

fn main() -> int {{
  const h = make(1)
  0
}}
"
        ),
        "a capturing closure in a union payload slot",
    );
    refused(
        &format!(
            "{PRELUDE}
fn make(k: int) -> int {{
  var g: (fn(int) -> int) | none = (x: int) => x + k
  0
}}

fn main() -> int {{ make(1) }}
"
        ),
        "a capturing closure in a union payload slot",
    );
    refused(
        &format!(
            "{PRELUDE}
fn make(k: int) -> int {{
  const g: (fn(int) -> int) | none = (x: int) => x + k
  0
}}

fn main() -> int {{ make(1) }}
"
        ),
        "a capturing closure in a union binding",
    );
}

/// The just-inside runs: a proven scalar word (a named fn, a
/// non-capturing lambda) inhabits the union's fn-typed payload slot and
/// reads back end to end — the annotated binding, the join, the return,
/// the call result, and the extraction families (the handler chain and
/// unwrap_or), the heap draining through every read-back chain.
#[test]
fn proven_scalars_inhabit_union_payload_slots() {
    // a named fn widened at the binding, read back through unwrap_or
    // with a proven fallback (a callable-defaulted parameter's word
    // refuses as the fallback — the payload slot's provenance rule)
    acceptance(
        "payload_named_unwrap_or",
        &format!(
            "{PRELUDE}
fn inc(x: int) -> int {{ x + 1 }}
extern fn unwrap_or[t] (x: t?, y: t) -> t
fn take() -> int {{
  const g: (fn(int) -> int) | none = inc
  const h = g.unwrap_or(inc)
  h(5)
}}

fn main() -> int {{ println(\"${{take()}}\")
  0
}}
"
        ),
        Value::I64(0),
        b"6\n",
    );
    // a non-capturing lambda in the payload slot
    acceptance(
        "payload_lambda",
        &format!(
            "{PRELUDE}
fn main() -> int {{
  const g: (fn(int) -> int) | none = (x: int) => x * 3
  const h = g.ok((x: fn(int) -> int) => x).err((_: none) => (x: int) => 0)
  h(8)
}}
"
        ),
        Value::I64(24),
        b"",
    );
    // the join, the union return, and the chained read-backs
    acceptance(
        "payload_join_return_chain",
        &format!(
            "{PRELUDE}
fn inc(x: int) -> int {{ x + 1 }}
fn pick(c: bool) -> (fn(int) -> int) | none {{
  if c {{ inc }} else {{ none }}
}}

fn main() -> int {{
  const g = pick(true)
  const h = g.ok((x: fn(int) -> int) => x).err((_: none) => inc)
  const r = h(7)
  const u = pick(false)
  const v = u.ok((x: fn(int) -> int) => x).err((_: none) => inc)
  println(\"r ${{r}} v ${{v(2)}}\")
  0
}}
"
        ),
        Value::I64(0),
        b"r 8 v 3\n",
    );
    // a named fn returned from a fn-typed callee joins the provenance
    // oracle: the result word widens into the payload slot and into a
    // re-storable var slot like a named fn value
    acceptance(
        "payload_call_result",
        &format!(
            "{PRELUDE}
fn inc(x: int) -> int {{ x + 1 }}
fn choose() -> fn(int) -> int {{ inc }}

fn main() -> int {{
  const g: (fn(int) -> int) | none = choose()
  const h = g.ok((x: fn(int) -> int) => x).err((_: none) => (x: int) => 0)
  const a = h(10)
  var v = choose()
  const b = v(3)
  println(\"a ${{a}} b ${{b}}\")
  0
}}
"
        ),
        Value::I64(0),
        b"a 11 b 4\n",
    );
}

/// The enum vocabulary's fn payload holds the closure Ref word the
/// struct field carries: a proven scalar wraps at the case construction,
/// and the payload binder's word calls through the member arm (the
/// fn-typed join keeps the scalar rule, so the call lives in the arm).
#[test]
fn an_enum_fn_payload_holds_proven_scalars() {
    acceptance(
        "enum_fn_payload",
        &format!(
            "{PRELUDE}
enum handler {{
  boxed(fn(int) -> int)
  nada
}}

fn inc(x: int) -> int {{ x + 1 }}

fn main() -> int {{
  const g: handler = boxed(inc)
  const r = match g {{
    boxed(f) => f(5)
    nada => 0
  }}
  println(\"boxed ${{r}}\")
  0
}}
"
        ),
        Value::I64(0),
        b"boxed 6\n",
    );
}

/// The fn-typed word answers carry the union payload slot's provenance
/// rule where the answer stages: the word is Fn-classified and every
/// later re-read trusts it scalar, so an unproven callable word (a
/// callable-defaulted binding's, an alias of one) or a capturing closure
/// lands a Ref word there, traps, and strands the count. Each seam
/// refuses at its own span: unwrap_or's fallback, unwrap_or_else's
/// inlined body, the handler answer, the join arms (the if and the
/// match), the fn-typed return, and ok_or's err member (the one
/// construction site staging its words outside the widening write).
#[test]
fn unproven_callables_refuse_at_fn_typed_word_answers() {
    // unwrap_or's fallback: the callable-defaulted parameter's word
    refused(
        &format!(
            "{PRELUDE}
extern fn unwrap_or[t] (x: t?, y: t) -> t
fn take(f: fn(int) -> int) -> int {{
  const cell: (fn(int) -> int) | none = none
  const h = cell.unwrap_or(f)
  h(5)
}}

fn main() -> int {{ take((x: int) => x) }}
"
        ),
        "an unproven callable in unwrap_or's fallback",
    );
    // the fallback spelled as the closure expression itself
    refused(
        &format!(
            "{PRELUDE}
extern fn unwrap_or[t] (x: t?, y: t) -> t
fn make(k: int) -> int {{
  const cell: (fn(int) -> int) | none = none
  const h = cell.unwrap_or((x: int) => x + k)
  h(5)
}}

fn main() -> int {{ make(1) }}
"
        ),
        "a capturing closure in unwrap_or's fallback",
    );
    // unwrap_or_else's inlined fallback body answering the parameter
    refused(
        &format!(
            "{PRELUDE}
extern fn unwrap_or_else[t] (x: t?, f: fn() -> t) -> t
fn take(f: fn(int) -> int) -> int {{
  const cell: (fn(int) -> int) | none = none
  const h = cell.unwrap_or_else(() => f)
  h(5)
}}

fn main() -> int {{ take((x: int) => x) }}
"
        ),
        "an unproven callable in unwrap_or_else's fallback",
    );
    // the .err handler's answer over a fn-word remainder
    refused(
        &format!(
            "{PRELUDE}
fn take(f: fn(int) -> int) -> int {{
  const cell: (fn(int) -> int) | none = none
  const h = cell.err((_: none) => f)
  h(5)
}}

fn main() -> int {{ take((x: int) => x) }}
"
        ),
        "an unproven callable in a handler's answer",
    );
    // the if-join arm, both arms callable-defaulted parameters
    refused(
        &format!(
            "{PRELUDE}
fn take(f: fn(int) -> int, g: fn(int) -> int, c: bool) -> int {{
  const h = if c {{ f }} else {{ g }}
  h(5)
}}

fn main() -> int {{
  const k = 1
  take((x: int) => x, (x: int) => x + k, true)
}}
"
        ),
        "an unproven callable in a fn-typed join arm",
    );
    // the if-join arm spelled as the closure expression itself
    refused(
        &format!(
            "{PRELUDE}
fn make(k: int, c: bool) -> int {{
  const h = if c {{ (x: int) => x + k }} else {{ (x: int) => x }}
  h(5)
}}

fn main() -> int {{ make(1, true) }}
"
        ),
        "a capturing closure in a fn-typed join arm",
    );
    // the match-join arm rides the same guard (the literal-arm chain
    // over a non-union scrutinee)
    refused(
        &format!(
            "{PRELUDE}
fn take(f: fn(int) -> int, g: fn(int) -> int, n: int) -> int {{
  const h = match n {{ 1 => f, 2 => g, _ => f }}
  h(5)
}}

fn main() -> int {{
  const k = 1
  take((x: int) => x, (x: int) => x + k, 2)
}}
"
        ),
        "an unproven callable in a fn-typed join arm",
    );
    // the param-through fn-typed return: the caller classifies the
    // result word Fn, so the callable-defaulted word refuses here
    refused(
        &format!(
            "{PRELUDE}
fn idf(f: fn(int) -> int) -> fn(int) -> int {{ f }}

fn main() -> int {{
  const h = idf((x: int) => x)
  h(5)
}}
"
        ),
        "an unproven callable in a fn-typed return",
    );
    // ok_or's err member stages its words directly, so the union
    // payload guard runs at the arm instead of the widening write
    refused(
        &format!(
            "{PRELUDE}
extern fn ok_or[t, e] (x: t?, err: e) -> t | e
fn take(f: fn(int) -> int) -> int {{
  const cell: int | none = none
  const u = cell.ok_or(f)
  const r = u.ok((x: int) => 0).err((g: fn(int) -> int) => g(5))
  r
}}

fn main() -> int {{ take((x: int) => x) }}
"
        ),
        "an unproven callable in a union payload slot",
    );
}

/// The proven scalar words answer at every guarded seam and run end to
/// end — the named fn fallback, the join arms, the fn-typed return (the
/// named value, a proven call result, a non-capturing lambda), the
/// inlined fallbacks, the handler answer, and ok_or's err member — the
/// heap draining through every leg. The factory return keeps its
/// boundary: a closure word's own return lowers and runs, so the return
/// guard refuses only the unproven callable words.
#[test]
fn proven_scalars_answer_fn_typed_words_end_to_end() {
    // unwrap_or picks the named fallback on the none side and the named
    // payload on the ok side
    acceptance(
        "word_unwrapor_named",
        &format!(
            "{PRELUDE}
extern fn unwrap_or[t] (x: t?, y: t) -> t
fn inc(x: int) -> int {{ x + 1 }}

fn main() -> int {{
  const none_cell: (fn(int) -> int) | none = none
  const h = none_cell.unwrap_or(inc)
  const some_cell: (fn(int) -> int) | none = inc
  const h2 = some_cell.unwrap_or(inc)
  println(\"h ${{h(5)}} h2 ${{h2(5)}}\")
  0
}}
"
        ),
        Value::I64(0),
        b"h 6 h2 6\n",
    );
    // unwrap_or_else's inlined body answering the named fn
    acceptance(
        "word_unwraporelse_named",
        &format!(
            "{PRELUDE}
extern fn unwrap_or_else[t] (x: t?, f: fn() -> t) -> t
fn inc(x: int) -> int {{ x + 1 }}

fn main() -> int {{
  const cell: (fn(int) -> int) | none = none
  const h = cell.unwrap_or_else(() => inc)
  h(5)
}}
"
        ),
        Value::I64(6),
        b"",
    );
    // the .err handler answering the named fn over the none side
    acceptance(
        "word_err_handler_named",
        &format!(
            "{PRELUDE}
fn inc(x: int) -> int {{ x + 1 }}

fn main() -> int {{
  const cell: (fn(int) -> int) | none = none
  const h = cell.err((_: none) => inc)
  h(5)
}}
"
        ),
        Value::I64(6),
        b"",
    );
    // the join arms answering named fns, the if and the match alike
    acceptance(
        "word_join_named",
        &format!(
            "{PRELUDE}
fn inc(x: int) -> int {{ x + 1 }}
fn dec(x: int) -> int {{ x - 1 }}

fn main() -> int {{
  const a = if true {{ inc }} else {{ dec }}
  const n = 1
  const b = match n {{ 1 => inc, _ => dec }}
  println(\"a ${{a(2)}} b ${{b(3)}}\")
  0
}}
"
        ),
        Value::I64(0),
        b"a 3 b 4\n",
    );
    // the fn-typed return: a named fn, a proven call result, and a
    // non-capturing lambda value
    acceptance(
        "word_return_named",
        &format!(
            "{PRELUDE}
fn inc(x: int) -> int {{ x + 1 }}
fn choose() -> fn(int) -> int {{ inc }}
fn pick() -> fn(int) -> int {{ choose() }}
fn lambda_pick() -> fn(int) -> int {{ (x: int) => x * 3 }}

fn main() -> int {{
  const f = pick()
  const g = lambda_pick()
  println(\"f ${{f(5)}} g ${{g(2)}}\")
  0
}}
"
        ),
        Value::I64(0),
        b"f 6 g 6\n",
    );
    // the factory return keeps its boundary: the closure word's own
    // return lowers, runs, and drains
    acceptance(
        "word_factory_return",
        &format!(
            "{PRELUDE}
fn make(k: int) -> fn(int) -> int {{ (x: int) => x + k }}

fn main() -> int {{
  const h = make(2)
  h(5)
}}
"
        ),
        Value::I64(7),
        b"",
    );
    // ok_or's err member carries the named fn, read back through the
    // handler chain
    acceptance(
        "word_okor_named",
        &format!(
            "{PRELUDE}
extern fn ok_or[t, e] (x: t?, err: e) -> t | e
fn inc(x: int) -> int {{ x + 1 }}

fn main() -> int {{
  const cell: int | none = none
  const u = cell.ok_or(inc)
  const r = u.ok((x: int) => 0).err((g: fn(int) -> int) => g(5))
  r
}}
"
        ),
        Value::I64(6),
        b"",
    );
}

/// A computed callee lowers through a temporary binding: the inner
/// call's fn-typed answer word binds ahead of the argument staging, and
/// the indirect call reads it like any binding's word.
#[test]
fn a_computed_callee_calls_through_a_temporary() {
    acceptance(
        "computed_callee",
        &format!(
            "{PRELUDE}
fn inc(x: int) -> int {{ x + 1 }}
fn choose() -> fn(int) -> int {{ inc }}

fn main() -> int {{
  const fs = [(x: int) => x * 3]
  println(\"a ${{choose()(5)}} b ${{fs[0](7)}}\")
  0
}}
"
        ),
        Value::I64(0),
        b"a 6 b 21\n",
    );
}

/// The element-capture composition: a fn-typed list's iteration binder
/// carries the closure Ref word (the fetch handing it out under its own
/// count), the spawned-style sequential walk calling it per element, and
/// a lambda capturing a fn-typed value rides the same staging.
#[test]
fn a_fn_element_walk_calls_and_captures() {
    acceptance(
        "fn_element_walk",
        &format!(
            "{PRELUDE}
fn run(thunks: list[fn() -> int]) -> int {{
  var seen = 0
  for x in thunks {{
    seen += x()
  }}
  seen
}}

fn main() -> int {{
  const k = 20
  const pass = (thunks: list[fn() -> int]) => run(thunks)
  println(\"${{pass([() => 1 + k, () => 2 + k])}}\")
  0
}}
"
        ),
        Value::I64(0),
        b"43\n",
    );
}

/// A closure capturing an interpolation temporary: the capture is a
/// migration point, so the captured StrCat result keeps its counted
/// spelling and the construction acquires its own count: the capture
/// staging runs the placement bookkeeping, so a pinned arena word never
/// survives to finalization and ClosureNew never traps ArenaHandoff. The
/// spawn seam shares the same staging, so its captures migrate the same
/// way.
#[test]
fn a_closure_capturing_an_interpolation_temporary_answers_counted() {
    acceptance(
        "capture_arena",
        &format!(
            "{PRELUDE}
fn main() -> int {{
  const k = \"VALUE-${{1 + 2}}-AAAAAAAAAAAAAAAAAAAA\"
  const f = () => k
  println(\"${{f()}}\")
  0
}}
"
        ),
        Value::I64(0),
        b"VALUE-3-AAAAAAAAAAAAAAAAAAAA\n" as &[u8],
    );
}
