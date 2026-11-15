//! Block-argument sugar e2e (F7's trailing lambda, last argument only):
//! the parser attaches the trailing block as the call's zero-parameter
//! lambda and the checker feeds it at the next open positional slot, so
//! the lowering stages it exactly like a written trailing lambda
//! argument — a non-capturing block mints the scalar FuncRef word, a
//! capturing block the closure Ref word, over every callee class that
//! takes one (a named fn, the UFCS dot spelling, and an indirect call
//! through a fn-typed binding). The pinned callable-word refusals keep
//! the written spelling's honesty: a block capturing a closure word or
//! a callable-defaulted binding refuses identically, and the lowered
//! module is byte-identical to the parenthesized spelling. The hosted
//! combinators refuse the zero-parameter block at the check — a lambda
//! literal's own arity must match the expected fn type's exactly — so
//! no lowering surface exists for that shape at all.

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
        "the block argument answers"
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

/// The module bytes of one check-clean source, lowered under one fixed
/// name and source hash so two spellings of the same program compare
/// directly (the module embeds the source hash it was lowered with).
fn module_bytes(source: &str) -> Vec<u8> {
    let mut files = Files::new();
    let id = files.add_source("sugar.ds", source);
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
    let lowered = lower("sugar", Hash128::compute(b"one program"), &asts, &analyzed)
        .expect("the source lowers");
    write(lowered.module())
}

/// The declarations every probe shares.
const PRELUDE: &str = "
tag int
tag string

extern fn println (s: string) -> ()
";

/// A trailing block lowers and runs through a named fn's last parameter:
/// the zero-parameter block is the callee's lambda argument, invoked
/// once, and the scalar FuncRef word needs no count.
#[test]
fn a_trailing_block_lowers_and_runs() {
    acceptance(
        "block_once",
        &format!(
            "{PRELUDE}
fn answer(f: fn() -> int) -> int {{ f() }}

fn main() -> int {{
  println(\"got ${{answer {{ 42 }}}}\")
  answer {{ 7 }}
}}
"
        ),
        Value::I64(7),
        b"got 42\n",
    );
}

/// A capturing trailing block invoked twice: the closure Ref word stages
/// once into the callable-defaulted parameter as a borrow window, both
/// CALLS reads copy their captures in, and the counts balance — the
/// heap drains after the object's death.
#[test]
fn a_capturing_trailing_block_runs_twice_and_balances() {
    acceptance(
        "block_twice",
        &format!(
            "{PRELUDE}
fn twice(f: fn() -> int) -> int {{ f() + f() }}

fn main() -> int {{
  const k = 10
  println(\"twice ${{twice {{ k + 11 }}}}\")
  twice {{ k + 11 }}
}}
"
        ),
        Value::I64(42),
        b"twice 42\n",
    );
}

/// The UFCS dot spelling takes the block the same way: `x.f { ... }`
/// stages the receiver as the first argument and the block as the last
/// (the free function resolved behind the dot, zero runtime cost).
#[test]
fn a_ufcs_dot_call_takes_the_trailing_block() {
    acceptance(
        "block_ufcs",
        &format!(
            "{PRELUDE}
fn scale(base: int, f: fn() -> int) -> int {{ base + f() }}

fn main() -> int {{
  println(\"ufcs ${{5.scale {{ 37 }}}}\")
  5.scale {{ 37 }}
}}
"
        ),
        Value::I64(42),
        b"ufcs 42\n",
    );
}

/// An indirect call through a fn-typed binding takes the block too: the
/// callee word is the binding's scalar FuncRef and the block stages as
/// the trailing argument of the CALLS slide.
#[test]
fn an_indirect_call_takes_the_trailing_block() {
    acceptance(
        "block_indirect",
        &format!(
            "{PRELUDE}
fn pick(x: int, f: fn() -> int) -> int {{ x + f() }}

fn main() -> int {{
  const g = pick
  println(\"indirect ${{g(5) {{ 37 }}}}\")
  g(5) {{ 37 }}
}}
"
        ),
        Value::I64(42),
        b"indirect 42\n",
    );
}

/// The sugar is pure: `f { ... }` and `f(() => ...)` lower to the same
/// module bytes — the scalar block and the capturing block both — so no
/// calling-convention surface of the sugar's own exists.
#[test]
fn the_block_spelling_lowers_byte_identical_to_the_lambda_spelling() {
    let scalar_sugar = format!(
        "{PRELUDE}
fn answer(f: fn() -> int) -> int {{ f() }}

fn main() -> int {{ answer {{ 42 }} }}
"
    );
    let scalar_written = format!(
        "{PRELUDE}
fn answer(f: fn() -> int) -> int {{ f() }}

fn main() -> int {{ answer(() => 42) }}
"
    );
    assert_eq!(
        module_bytes(&scalar_sugar),
        module_bytes(&scalar_written),
        "the scalar block and the written lambda lower to one module"
    );
    let closure_sugar = format!(
        "{PRELUDE}
fn answer(f: fn() -> int) -> int {{ f() }}

fn main() -> int {{
  const base = 7
  answer {{ base + 35 }}
}}
"
    );
    let closure_written = format!(
        "{PRELUDE}
fn answer(f: fn() -> int) -> int {{ f() }}

fn main() -> int {{
  const base = 7
  answer(() => base + 35)
}}
"
    );
    assert_eq!(
        module_bytes(&closure_sugar),
        module_bytes(&closure_written),
        "the capturing block and the written lambda lower to one module"
    );
}

/// A trailing block whose body captures a closure word keeps the written
/// spelling's refusal: the capture block would carry a Ref word behind
/// a Fn-classified capture slot.
#[test]
fn a_trailing_block_capturing_a_closure_refuses() {
    let needle = "a lambda capturing a closure";
    refused(
        &format!(
            "{PRELUDE}
fn answer(f: fn() -> int) -> int {{ f() }}

fn main() -> int {{
  const k = 1
  const g = (x: int) => x + k
  answer {{ g(1) }}
}}
"
        ),
        needle,
    );
    refused(
        &format!(
            "{PRELUDE}
fn answer(f: fn() -> int) -> int {{ f() }}

fn main() -> int {{
  const k = 1
  const g = (x: int) => x + k
  answer(() => g(1))
}}
"
        ),
        needle,
    );
}

/// A trailing block whose body captures a callable-defaulted parameter
/// keeps the written spelling's refusal: the capture cannot prove a
/// non-capturing scalar.
#[test]
fn a_trailing_block_capturing_a_callable_parameter_refuses() {
    let needle = "a lambda capturing a callable binding";
    refused(
        &format!(
            "{PRELUDE}
fn answer(f: fn() -> int) -> int {{ f() }}
fn outer(g: fn() -> int) -> int {{ answer {{ g() }} }}

fn main() -> int {{ 0 }}
"
        ),
        needle,
    );
    refused(
        &format!(
            "{PRELUDE}
fn answer(f: fn() -> int) -> int {{ f() }}
fn outer(g: fn() -> int) -> int {{ answer(() => g()) }}

fn main() -> int {{ 0 }}
"
        ),
        needle,
    );
}

/// The hosted combinators refuse the zero-parameter block at the check:
/// the block is a lambda literal and `map`'s parameter is `fn(t) -> r`,
/// so the literal's own arity (zero) mismatches the expected fn type's
/// (one) at the literal's check — both spellings refuse identically, the
/// written lambda exactly like the block, and the module never builds
/// (the old probe pinned the runtime `ArityMismatch` trap the few-params
/// acceptance let through; the checker-level refusal replaced it).
#[test]
fn the_hosted_combinators_refuse_the_zero_parameter_block_at_check() {
    let host_ambient = "
tag int
tag string

struct list[t]

extern fn map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]
";
    for (name, source) in [
        (
            "block",
            format!(
                "{host_ambient}
fn main() -> int {{
  const ys = map([1, 2]) {{ 3 }}
  0
}}
"
            ),
        ),
        (
            "written",
            format!(
                "{host_ambient}
fn main() -> int {{
  const ys = map([1, 2], () => 3)
  0
}}
"
            ),
        ),
    ] {
        let mut files = Files::new();
        let id = files.add_source(format!("{name}.ds"), &source);
        let asts = [parse(&files, id).expect("the source parses")];
        let analyzed = analyze(&files, &asts);
        let rendered = analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>();
        assert_eq!(
            analyzed.checked.diags.len(),
            1,
            "one refusal, got {rendered}"
        );
        assert!(
            rendered.contains("DS2007"),
            "the refusal is the arity code, got {rendered}"
        );
        assert!(
            rendered.contains("the lambda takes 0 parameters, the expected fn type takes 1"),
            "the refusal names both arities, got {rendered}"
        );
        assert!(
            rendered.contains("expected fn type"),
            "the refusal cites the callee's fn type as its second span, got {rendered}"
        );
    }
}
