//! End-to-end through the `ds` binary: build a dsmod with the ds-mod
//! builder, write it to a temp file, and assert what `ds run` prints and
//! how it fails.

use std::{env, fs, path::PathBuf, process::Command};

use ds_mod::{Constant, FuncRefKey, Function, Hash128, Module, TargetClass, write};
use ds_vm::{
    abi::{
        ConstIdx, FrameLayout, SlotType,
        SlotType::{Bool, I64},
        uop::Uop,
    },
    native::{PLATFORM_NONE, PLATFORM_PARSE_ERR},
};

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn layout(slots: &[SlotType], params: usize, args: usize) -> FrameLayout {
    FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout")
}

/// The canonical mutual even/odd pair plus a main, with two extra entries
/// (`answer`, `half`) exercising i64 and f64 result printing.
fn parity_module(depth: i64) -> Module {
    let even = Function::new(
        "even",
        layout(&[I64, Bool, I64, I64], 1, 1),
        vec![
            Constant::I64(0),
            Constant::I64(1),
            Constant::Bool(true),
            Constant::FuncRef {
                key: FuncRefKey::Dep(0), // the enclosing key: FuncRefKey::Native(odd), index 1
                func: 1,
            },
        ],
        vec![
            Uop::Funcf { frame_slots: 4 },
            Uop::Kimm { dst: 2, kidx: k(0) },
            Uop::EqI64 {
                dst: 1,
                lhs: 0,
                rhs: 2,
            },
            Uop::Jmpif { cond: 1, target: 7 },
            Uop::Kimm { dst: 2, kidx: k(1) },
            Uop::SubI64 {
                dst: 3,
                lhs: 0,
                rhs: 2,
            },
            Uop::Callt {
                callee: k(3),
                nargs: 1,
            },
            Uop::Kconst { dst: 1, kidx: k(2) },
            Uop::Ret { src: 1 },
        ],
    )
    .expect("valid function");
    let odd = Function::new(
        "odd",
        layout(&[I64, Bool, I64, I64], 1, 1),
        vec![
            Constant::I64(0),
            Constant::I64(1),
            Constant::Bool(false),
            Constant::FuncRef {
                key: FuncRefKey::Dep(0),
                func: 0,
            },
        ],
        vec![
            Uop::Funcf { frame_slots: 4 },
            Uop::Kimm { dst: 2, kidx: k(0) },
            Uop::EqI64 {
                dst: 1,
                lhs: 0,
                rhs: 2,
            },
            Uop::Jmpif { cond: 1, target: 7 },
            Uop::Kimm { dst: 2, kidx: k(1) },
            Uop::SubI64 {
                dst: 3,
                lhs: 0,
                rhs: 2,
            },
            Uop::Callt {
                callee: k(3),
                nargs: 1,
            },
            Uop::Kconst { dst: 1, kidx: k(2) },
            Uop::Ret { src: 1 },
        ],
    )
    .expect("valid function");
    let main = Function::new(
        "main",
        layout(&[I64], 0, 1),
        vec![Constant::I64(depth), Constant::FuncRef {
            key: FuncRefKey::Dep(0),
            func: 0,
        }],
        vec![
            Uop::Funcf { frame_slots: 1 },
            Uop::Kimm { dst: 0, kidx: k(0) },
            Uop::Call {
                callee: k(1),
                nargs: 1,
            },
            Uop::Halt { slot: 0 },
        ],
    )
    .expect("valid function");
    let answer = Function::new(
        "answer",
        layout(&[I64], 0, 0),
        vec![Constant::I64(42)],
        vec![
            Uop::Funcf { frame_slots: 1 },
            Uop::Kimm { dst: 0, kidx: k(0) },
            Uop::Ret { src: 0 },
        ],
    )
    .expect("valid function");
    let half = Function::new(
        "half",
        layout(&[SlotType::F64], 0, 0),
        vec![Constant::F64(2.5)],
        vec![
            Uop::Funcf { frame_slots: 1 },
            Uop::Kconst { dst: 0, kidx: k(0) },
            Uop::Ret { src: 0 },
        ],
    )
    .expect("valid function");
    Module::new(
        "mut",
        TargetClass::Bytecode,
        Hash128::compute(b"parity source"),
    )
    .with_function(even)
    .with_function(odd)
    .with_function(main)
    .with_function(answer)
    .with_function(half)
}

/// A one-function module whose main calls a foreign module that is never
/// installed (`dep`, deliberately not the reserved `std` platform module
/// the run path installs).
fn foreign_callee_module() -> Module {
    let main = Function::new(
        "main",
        layout(&[I64], 0, 1),
        vec![Constant::I64(5), Constant::FuncRef {
            key: FuncRefKey::Dep(1), // the "dep" dependency entry
            func: 0,
        }],
        vec![
            Uop::Funcf { frame_slots: 1 },
            Uop::Kimm { dst: 0, kidx: k(0) },
            Uop::Call {
                callee: k(1),
                nargs: 1,
            },
            Uop::Halt { slot: 0 },
        ],
    )
    .expect("valid function");
    Module::new("app", TargetClass::Bytecode, Hash128::compute(b"app"))
        .with_dep("dep", Hash128::ZERO)
        .with_function(main)
}

/// A one-function module whose main divides by zero: a mid-run trap with a
/// live frame for the backtrace summary.
fn divide_by_zero_module() -> Module {
    let main = Function::new(
        "main",
        layout(&[I64, I64, I64], 0, 0),
        vec![Constant::I64(1), Constant::I64(0)],
        vec![
            Uop::Funcf { frame_slots: 3 },
            Uop::Kimm { dst: 0, kidx: k(0) },
            Uop::Kimm { dst: 1, kidx: k(1) },
            Uop::DivI64 {
                dst: 2,
                lhs: 0,
                rhs: 1,
            },
            Uop::Halt { slot: 2 },
        ],
    )
    .expect("valid function");
    Module::new("div", TargetClass::Bytecode, Hash128::compute(b"div")).with_function(main)
}

/// Writes `bytes` under a unique temp name; every test removes its file.
fn temp_module(tag: &str, bytes: &[u8]) -> PathBuf {
    let path = env::temp_dir().join(format!("ds-cli-run-{tag}-{}.dsmod", std::process::id()));
    fs::write(&path, bytes).expect("temp dsmod writes");
    path
}

/// A one-function module whose main halts on a bare platform tag word: the
/// run-result decode under test, driven through the real entry path.
fn platform_tag_module(name: &str, tag: u8) -> Module {
    let main = Function::new("main", layout(&[SlotType::Tag], 0, 0), vec![], vec![
        Uop::Funcf { frame_slots: 1 },
        Uop::TagNew { dst: 0, tag },
        Uop::Halt { slot: 0 },
    ])
    .expect("valid function");
    Module::new(
        name,
        TargetClass::Bytecode,
        Hash128::compute(name.as_bytes()),
    )
    .with_function(main)
}

/// A private per-process script-cache root: spawned runs never read or
/// write a real user cache directory.
fn test_cache() -> PathBuf {
    static CACHE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            let dir = env::temp_dir().join(format!("ds-cli-test-cache-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            dir
        })
        .clone()
}
/// Runs the built binary and flattens the outcome.
fn ds_run(args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_ds"))
        .env("DS_CACHE", test_cache())
        .args(args)
        .output()
        .expect("the ds binary runs");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn run_executes_main_and_named_entries() {
    // deep enough that only constant-stack tail calls can finish it
    let path = temp_module("parity", &write(&parity_module(100_000)));
    let file = path.to_str().expect("temp path is UTF-8");

    let (ok, out, err) = ds_run(&["run", file]);
    assert!(ok, "ds run exits 0 (stderr: {err})");
    assert_eq!(out, "true\n");

    // a named entry
    let (ok, out, err) = ds_run(&["run", file, "--entry", "answer"]);
    assert!(ok, "ds run --entry answer (stderr: {err})");
    assert_eq!(out, "42\n");
    let (ok, out, err) = ds_run(&["run", file, "--entry", "half"]);
    assert!(ok, "ds run --entry half (stderr: {err})");
    assert_eq!(out, "2.5\n");

    fs::remove_file(&path).expect("temp dsmod removes");
}

#[test]
fn a_trap_prints_the_backtrace_summary() {
    let path = temp_module("trap", &write(&divide_by_zero_module()));
    let (ok, out, err) = ds_run(&["run", path.to_str().expect("temp path is UTF-8")]);
    fs::remove_file(&path).expect("temp dsmod removes");
    assert!(!ok, "a trapped run exits nonzero");
    assert_eq!(out, "");
    assert!(err.contains("trap: division by zero"), "stderr: {err}");
    assert!(err.contains("div::main at pc 3"), "stderr: {err}");
}

/// A hosted call's caller frame (a `map` combinator's reentrant
/// per-element call parked inside the native) has no bytecode position:
/// the backtrace names the hosted seam for it, never the sentinel
/// integer the synthetic reentry frame carries as its return pc.
#[test]
fn a_hosted_callers_backtrace_frame_names_the_seam() {
    let path = env::temp_dir().join(format!("ds-cli-run-hosted-trap-{}.ds", std::process::id()));
    fs::write(
        &path,
        "fn main() -> () {\n  const xs = [1, 0]\n  const ys = xs.map((x: int) => 100 / x)\n}\n",
    )
    .expect("the source writes");
    let (ok, out, err) = ds_run(&["run", path.to_str().expect("temp path is UTF-8")]);
    fs::remove_file(&path).expect("temp source removes");
    assert!(!ok, "a trapped run exits nonzero");
    assert_eq!(out, "", "stdout stays clean");
    assert!(err.contains("trap: division by zero"), "stderr: {err}");
    assert!(
        err.contains("at hosted"),
        "the parked caller names the seam: {err}"
    );
    assert!(
        !err.contains("4294967295"),
        "the sentinel return pc never surfaces: {err}"
    );
}

/// A script whose shift leaves the bit width behaves identically on
/// both sides of the trap: `ds check` accepts it (the types are sound;
/// the shift amount is a runtime fact), and `ds run` prints the trap
/// like division by zero does.
#[test]
fn an_oversized_shift_checks_clean_and_traps_at_run() {
    let path = env::temp_dir().join(format!("ds-cli-run-shift-trap-{}.ds", std::process::id()));
    fs::write(&path, "fn main() -> int { 1 << 64 }\n").expect("the source writes");
    let (ok, _, err) = ds_run(&["check", path.to_str().expect("temp path is UTF-8")]);
    assert!(ok, "the trapping script checks clean: {err}");
    let (ok, out, err) = ds_run(&["run", path.to_str().expect("temp path is UTF-8")]);
    fs::remove_file(&path).expect("temp source removes");
    assert!(!ok, "a trapped run exits nonzero");
    assert_eq!(out, "", "stdout stays clean");
    assert!(err.contains("trap: oversized shift"), "stderr: {err}");
}

/// A trapped run releases the interrupted root after the trap report:
/// the kept frames' deinit hooks fire exactly once, so a `~Copyable`
/// holder's cleanup survives a bug-panic path while the report and the
/// nonzero exit stay unchanged. The untrapped counterpart pins that the
/// hook fires exactly once there too — the release never double-fires.
#[test]
fn a_trapped_run_fires_the_kept_frames_deinit_after_the_report() {
    let source = |divisor: &str| {
        format!(
            concat!(
                "struct holder: ~Copyable {{\n",
                "  id: int\n",
                "  deinit {{ println(\"freed ${{self.id}}\") }}\n",
                "}}\n",
                "fn boom(h: holder) -> int {{\n",
                "  const keep = h\n",
                "  const z = 10 / {divisor}\n",
                "  z + keep.id\n",
                "}}\n",
                "fn main() -> int {{\n",
                "  println(\"${{boom(holder {{ id: 7 }})}}\")\n",
                "  0\n",
                "}}\n",
            ),
            divisor = divisor,
        )
    };
    let path = env::temp_dir().join(format!("ds-cli-run-trap-deinit-{}.ds", std::process::id()));
    fs::write(&path, source("0")).expect("the source writes");
    let (ok, out, err) = ds_run(&["run", path.to_str().expect("temp path is UTF-8")]);
    fs::remove_file(&path).expect("temp source removes");
    assert!(!ok, "a trapped run exits nonzero");
    assert!(err.contains("trap: division by zero"), "stderr: {err}");
    assert!(
        err.contains("boom at pc"),
        "the backtrace names the frame: {err}"
    );
    assert_eq!(
        out.matches("freed 7").count(),
        1,
        "the trapped frame's deinit fires exactly once: {out}"
    );

    fs::write(&path, source("2")).expect("the source writes");
    let (ok, out, err) = ds_run(&["run", path.to_str().expect("temp path is UTF-8")]);
    fs::remove_file(&path).expect("temp source removes");
    assert!(ok, "the untrapped run exits 0 (stderr: {err})");
    assert_eq!(
        out.matches("freed 7").count(),
        1,
        "the completed frame's deinit fires exactly once: {out}"
    );
}

#[test]
fn corrupted_bytes_exit_nonzero_with_a_diagnostic() {
    let mut bytes = write(&parity_module(4));
    bytes[0] = b'X'; // break the magic: DS4002 with a span at byte 0
    let path = temp_module("corrupt", &bytes);
    let (ok, out, err) = ds_run(&["run", path.to_str().expect("temp path is UTF-8")]);
    fs::remove_file(&path).expect("temp dsmod removes");
    assert!(!ok, "corrupted module exits nonzero");
    assert_eq!(out, "", "stdout stays clean");
    assert!(err.contains("error[DS4002]"), "stderr: {err}");
    assert!(err.contains("magic"), "stderr: {err}");
}

#[test]
fn missing_file_is_a_clean_error() {
    let missing = env::temp_dir().join("ds-cli-run-absent.dsmod");
    let _ = fs::remove_file(&missing);
    let (ok, out, err) = ds_run(&["run", missing.to_str().expect("temp path is UTF-8")]);
    assert!(!ok, "missing file exits nonzero");
    assert_eq!(out, "");
    assert!(err.contains("cannot open module"), "stderr: {err}");
}

#[test]
fn unknown_entry_names_the_module() {
    let path = temp_module("entry", &write(&parity_module(4)));
    let (ok, out, err) = ds_run(&[
        "run",
        path.to_str().expect("temp path is UTF-8"),
        "--entry",
        "nope",
    ]);
    fs::remove_file(&path).expect("temp dsmod removes");
    assert!(!ok, "unknown entry exits nonzero");
    assert_eq!(out, "");
    assert!(
        err.contains("module `mut` has no function `nope`"),
        "stderr: {err}"
    );
}

#[test]
fn foreign_callee_reports_the_missing_module() {
    let path = temp_module("foreign", &write(&foreign_callee_module()));
    let (ok, out, err) = ds_run(&["run", path.to_str().expect("temp path is UTF-8")]);
    fs::remove_file(&path).expect("temp dsmod removes");
    assert!(!ok, "unresolved callee exits nonzero");
    assert_eq!(out, "");
    assert!(
        err.contains("module `dep` is not installed"),
        "stderr: {err}"
    );
}

#[test]
fn platform_tags_in_run_results_decode_by_name() {
    // main halts on the parse_err tag: a failed run naming the failure,
    // where the decode previously printed the raw id.
    let path = temp_module(
        "parse-err",
        &write(&platform_tag_module("parse_err", PLATFORM_PARSE_ERR)),
    );
    let (ok, out, err) = ds_run(&["run", path.to_str().expect("temp path is UTF-8")]);
    fs::remove_file(&path).expect("temp dsmod removes");
    assert!(!ok, "an unhandled parse failure exits nonzero");
    assert_eq!(out, "");
    assert!(err.contains("unhandled parse failure"), "stderr: {err}");

    // The none member of a checked read is a plain result: printed by
    // name, not as the reservation's raw id.
    let path = temp_module(
        "none-tag",
        &write(&platform_tag_module("none_tag", PLATFORM_NONE)),
    );
    let (ok, out, err) = ds_run(&["run", path.to_str().expect("temp path is UTF-8")]);
    fs::remove_file(&path).expect("temp dsmod removes");
    assert!(ok, "a none result is a successful run (stderr: {err})");
    assert_eq!(out, "none\n");
}

#[test]
fn run_refuses_an_order_hidden_overlap_at_check() {
    // The pair declares clean in source order; the package-level
    // coherence pass rejects it, so the run stops at the check, never
    // at lowering or the missing entry.
    let (ok, out, err) = ds_run(&[
        "run",
        "--no-ambient",
        "tests/fixtures/negative/ds2006_late_bound_pair.ds",
    ]);
    assert!(!ok, "an incoherent package exits nonzero");
    assert_eq!(out, "");
    assert!(
        err.contains("DS2006"),
        "the overlap reports at check: {err}"
    );
    assert!(
        err.contains("the source does not check clean"),
        "the refusal names the check stage: {err}"
    );
}

#[test]
fn run_refuses_a_moved_then_used_value_at_the_linearity_gate() {
    // The affine walk's refusals gate the run exactly as they gate
    // `ds check`: a `~Copyable` value moved into a call and used again
    // never lowers, so its deinit cannot fire twice at runtime.
    let (ok, out, err) = ds_run(&[
        "run",
        "--no-ambient",
        "tests/fixtures/negative/ds3001_moved_then_used.ds",
    ]);
    assert!(!ok, "a moved-then-used script exits nonzero");
    assert_eq!(out, "", "nothing executes");
    assert!(
        err.contains("DS3001"),
        "the move reports at the linearity gate: {err}"
    );
    assert!(
        err.contains("used again here after move"),
        "the two-span pair renders: {err}"
    );
    assert!(
        err.contains("the source fails the linearity check"),
        "the refusal names the stage: {err}"
    );
}

/// The comptime fence's step budget end to end: a healthy source embeds
/// its comptime answer while an ordinary runtime `while` in `main` still
/// runs, and a comptime fn whose loop never terminates checks clean but
/// refuses `ds run` with the budget's diagnostic at the call site —
/// bounded wall time instead of the pre-budget hang.
#[test]
fn a_runaway_comptime_loop_answers_the_step_budget_at_the_run() {
    let path = env::temp_dir().join(format!(
        "ds-cli-run-comptime-budget-{}.ds",
        std::process::id()
    ));
    fs::write(
        &path,
        concat!(
            "fn twice(comptime n: int) -> int { n + n }\n",
            "fn main() -> () {\n",
            "  var t = 0\n",
            "  var j = 0\n",
            "  while j < 3 {\n",
            "    t = t + j\n",
            "    j = j + 1\n",
            "  }\n",
            "  println(\"${twice(21)} ${t}\")\n",
            "}\n",
        ),
    )
    .expect("the source writes");
    let (ok, out, err) = ds_run(&["run", path.to_str().expect("temp path is UTF-8")]);
    fs::remove_file(&path).expect("temp source removes");
    assert!(ok, "the healthy source runs (stderr: {err})");
    assert_eq!(out, "42 3\n0\n");

    let path = env::temp_dir().join(format!(
        "ds-cli-run-comptime-spin-{}.ds",
        std::process::id()
    ));
    fs::write(
        &path,
        concat!(
            "fn spin(comptime n: int) -> int {\n",
            "  var i = n\n",
            "  while i > 0 {\n",
            "    i = i + 1\n",
            "  }\n",
            "  i\n",
            "}\n",
            "fn main() -> () {\n",
            "  println(\"${spin(1)}\")\n",
            "}\n",
        ),
    )
    .expect("the source writes");
    let (ok, _, err) = ds_run(&["check", path.to_str().expect("temp path is UTF-8")]);
    assert!(ok, "the runaway body checks clean: {err}");
    let (ok, out, err) = ds_run(&["run", path.to_str().expect("temp path is UTF-8")]);
    fs::remove_file(&path).expect("temp source removes");
    assert!(!ok, "the runaway comptime run exits nonzero");
    assert_eq!(out, "", "stdout stays clean before the refusal");
    assert!(err.contains("DS2034"), "the budget refusal prints: {err}");
    assert!(
        err.contains("step budget"),
        "the refusal names the budget: {err}"
    );
}
