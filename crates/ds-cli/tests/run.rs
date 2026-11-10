//! End-to-end through the `ds` binary: build a dsmod with the ds-mod
//! builder, write it to a temp file, and assert what `ds run` prints and
//! how it fails.

use std::{env, fs, path::PathBuf, process::Command};

use ds_mod::{Constant, Function, Hash128, Module, TargetClass, write};
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
                module: String::new(), // the enclosing module: odd, index 1
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
                module: String::new(),
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
            module: String::new(),
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
            module: "dep".into(),
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
    Module::new("app", TargetClass::Bytecode, Hash128::compute(b"app")).with_function(main)
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

/// Runs the built binary and flattens the outcome.
fn ds_run(args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_ds"))
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

    // a named entry with staged i64 arguments
    let (ok, out, err) = ds_run(&["run", file, "--entry", "odd", "--args", "11"]);
    assert!(ok, "ds run --entry odd (stderr: {err})");
    assert_eq!(out, "true\n");

    // i64 and f64 results print
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
