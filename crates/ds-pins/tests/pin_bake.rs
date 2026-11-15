//! The pin-bake harness: a nextest-runnable bake of the gungraun pins.
//! The nine pinned kernels live in four `benches/` files and run
//! under `cargo bench` (the runner's model has no test target); this
//! companion compiles their fixtures into the test binary via
//! `#[path]` and measures the same `drive()` kernels under a raw
//! Callgrind child — so the committed pin table below is asserted on
//! every tree the gate runs,
//! and a worktree-vs-tree divergence fails the gate instead of
//! surfacing months later in an A/B.
//!
//! Measurement model: one child process per kernel, spawned as this
//! test binary itself with [`CHILD_ENV`] naming the kernel; Callgrind
//! counts the whole process (libtest startup, the registry build, and
//! the VM setup sit deterministically far below the 5M-iteration
//! kernel mass), so the count is a pure function of the binary and
//! Callgrind's simulation holds it exact run to run. Cross-build
//! jitter of identical source measured in single-digit Ir, while the
//! drifts the pins exist to catch (an enum widening flipping an
//! inlining decision, a dispatch arm re-lowering its neighbors) move
//! whole percents — the 0.1% band sits an order below the smallest
//! real signal and orders above the noise floor.
//!
//! The counts are only meaningful under an optimized profile (debug
//! codegen measures a different program): the gate runs
//! `cargo nextest run --cargo-profile bench -p ds-pins --test pin_bake`,
//! and a debug-profile run skips with a note. Valgrind is a Linux
//! tool; hosts without it skip the same way. A change that moves a
//! count past the band fails the gate with the measured value —
//! re-baseline by running the same gate command with
//! `PIN_BAKE_RECORD=1` (it measures and prints the table instead of
//! asserting), then updating [`PINS`] in the same commit that changed
//! the codegen, so the table stays tree-verified.
//!
//! The absolute Ir numbers in the original pinning commit messages
//! were worktree-only measurements and never reproduced on a committed
//! tree (scalar_loop's message recorded 6,895,036,857 while the pin's
//! own commit and every tree since measure 7,295,037,7xx under cargo
//! bench): this table is the pins' authoritative baseline, recorded by
//! the harness itself on the committed tree.
// the Ir counts sit under 2^53, so the drift fractions' u64 casts are
// exact; clippy's blanket precision-loss lint would deny them
#![allow(clippy::cast_precision_loss)]

#[path = "../benches/loop_claims.rs"]
// the gungraun registration mains are dead in this binary; the drive
// fns below are the harness's entry
#[allow(dead_code)]
mod loop_claims_bench;
#[path = "../benches/scalar_loop.rs"]
// the gungraun registration mains are dead in this binary; the drive
// fns below are the harness's entry
#[allow(dead_code)]
mod scalar_loop_bench;
#[path = "../benches/strcat_temp.rs"]
// the gungraun registration mains are dead in this binary; the drive
// fns below are the harness's entry
#[allow(dead_code)]
mod strcat_temp_bench;
#[path = "../benches/string_temp.rs"]
// the gungraun registration mains are dead in this binary; the drive
// fns below are the harness's entry
#[allow(dead_code)]
mod string_temp_bench;

use std::{hint::black_box, process::Command, thread};

use ds_vm::Value;

/// The child-mode selector: the harness parent sets this to a kernel
/// name from [`KERNELS`], and the child runs that kernel alone.
const CHILD_ENV: &str = "PIN_BAKE_KERNEL";

/// The re-baseline switch: with `PIN_BAKE_RECORD=1` the gate measures
/// and prints the pin table instead of asserting, for recording a new
/// tree-verified baseline in the same commit that legitimately changed
/// the codegen.
const RECORD_ENV: &str = "PIN_BAKE_RECORD";

/// The pin tolerance as a fraction of the recorded count.
const TOLERANCE: f64 = 0.001;

/// One kernel: its pin-table name (the gungraun bench path it mirrors)
/// and the drive function the child executes.
struct Kernel {
    name: &'static str,
    drive: fn() -> Value,
}

const KERNELS: &[Kernel] = &[
    Kernel {
        name: "scalar_loop::scalar_loop::scalar_dispatch",
        drive: || scalar_loop_bench::drive(),
    },
    Kernel {
        name: "strcat_temp::strcat_temp::serving_cats",
        drive: || strcat_temp_bench::drive(true),
    },
    Kernel {
        name: "strcat_temp::strcat_temp::claiming_cats",
        drive: || strcat_temp_bench::drive(false),
    },
    Kernel {
        name: "string_temp::string_temp::borrowing_loads",
        drive: || string_temp_bench::drive(true),
    },
    Kernel {
        name: "string_temp::string_temp::claiming_loads",
        drive: || string_temp_bench::drive(false),
    },
    Kernel {
        name: "loop_claims::loop_claims::reset_held_to_overwrite",
        drive: || loop_claims_bench::drive(false, false),
    },
    Kernel {
        name: "loop_claims::loop_claims::reset_released_at_use",
        drive: || loop_claims_bench::drive(false, true),
    },
    Kernel {
        name: "loop_claims::loop_claims::rebuild_held_to_overwrite",
        drive: || loop_claims_bench::drive(true, false),
    },
    Kernel {
        name: "loop_claims::loop_claims::rebuild_released_at_use",
        drive: || loop_claims_bench::drive(true, true),
    },
];

/// The committed pin table: the whole-process Callgrind Ir of each
/// kernel's 5M-iteration drive under the harness's own test binary,
/// recorded by `PIN_BAKE_RECORD=1` on the committed tree. Every
/// [`KERNELS`] entry must appear here exactly once — a kernel without
/// a pin or a pin without a kernel fails the gate.
const PINS: &[(&str, u64)] = &[
    ("scalar_loop::scalar_loop::scalar_dispatch", 6_443_340_756),
    ("strcat_temp::strcat_temp::serving_cats", 8_530_833_738),
    ("strcat_temp::strcat_temp::claiming_cats", 15_210_833_824),
    ("string_temp::string_temp::borrowing_loads", 4_540_829_221),
    ("string_temp::string_temp::claiming_loads", 16_355_827_319),
    (
        "loop_claims::loop_claims::reset_held_to_overwrite",
        4_445_828_179,
    ),
    (
        "loop_claims::loop_claims::reset_released_at_use",
        8_870_828_175,
    ),
    (
        "loop_claims::loop_claims::rebuild_held_to_overwrite",
        10_710_833_853,
    ),
    (
        "loop_claims::loop_claims::rebuild_released_at_use",
        10_710_833_017,
    ),
];

/// The child half: run the named kernel once on a full-size stack (the
/// libtest thread's default is too thin for the VM's frames) and exit
/// with the client's status, never reaching the assertion half.
fn run_child(name: &str) -> ! {
    let Some(kernel) = KERNELS.iter().find(|k| k.name == name) else {
        eprintln!("pin_bake: unknown kernel {name}");
        std::process::exit(2);
    };
    let drive = kernel.drive;
    let handle = thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(move || black_box(drive()))
        .expect("the kernel thread spawns");
    let got = handle.join().expect("the kernel runs to completion");
    black_box(got);
    std::process::exit(0);
}

/// One measured kernel: Callgrind's summary Ir of the child run.
fn measure(name: &str, out_file: &std::path::Path) -> Result<u64, String> {
    let exe = std::env::current_exe().expect("the test binary knows its own path");
    let output = Command::new("valgrind")
        .arg("--tool=callgrind")
        .arg("--cache-sim=no")
        .arg(format!("--callgrind-out-file={}", out_file.display()))
        .env(CHILD_ENV, name)
        .arg(&exe)
        .output()
        .map_err(|err| format!("valgrind did not run: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "the kernel child failed ({:?}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let report = std::fs::read_to_string(out_file)
        .map_err(|err| format!("callgrind wrote no report: {err}"))?;
    let summary = report
        .lines()
        .rev()
        .find_map(|line| {
            let rest = line.strip_prefix("summary:")?;
            rest.trim().split_ascii_whitespace().next()
        })
        .ok_or_else(|| "callgrind wrote no summary line".to_owned())?;
    summary
        .parse::<u64>()
        .map_err(|err| format!("callgrind's summary did not parse ({summary}): {err}"))
}

#[test]
fn bakes_the_gungraun_pins_against_the_committed_table() {
    if let Some(name) = std::env::var_os(CHILD_ENV) {
        run_child(name.to_string_lossy().as_ref());
    }
    if cfg!(debug_assertions) {
        eprintln!("pin_bake: skipped, the counts need an optimized profile");
        eprintln!("pin_bake: cargo nextest run --cargo-profile bench -p ds-pins --test pin_bake");
        return;
    }
    if Command::new("valgrind").arg("--version").output().is_err() {
        eprintln!("pin_bake: skipped, no valgrind on this host");
        return;
    }
    let record = std::env::var_os(RECORD_ENV).is_some();
    let tmp = std::env::temp_dir().join(format!("pin_bake-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("the measurement dir creates");
    // one child per kernel, all in flight: each stream is a simulation,
    // wall time is bounded by the heaviest kernel
    let handles: Vec<_> = KERNELS
        .iter()
        .map(|kernel| {
            let name = kernel.name;
            let out_file = tmp.join(format!(
                "{}.out",
                name.rsplit("::").next().unwrap_or(name).replace(' ', "_")
            ));
            thread::Builder::new()
                .stack_size(1 << 20)
                .spawn(move || (name, measure(name, &out_file)))
                .expect("the measurement thread spawns")
        })
        .collect();
    let mut results = Vec::with_capacity(KERNELS.len());
    for handle in handles {
        results.push(handle.join().expect("the measurement thread joins"));
    }
    for (name, _) in &results {
        let out_file = tmp.join(format!(
            "{}.out",
            name.rsplit("::").next().unwrap_or(name).replace(' ', "_")
        ));
        let _ = std::fs::remove_file(&out_file);
    }
    let _ = std::fs::remove_dir(&tmp);
    if record {
        eprintln!(
            "pin_bake: {} kernels measured; the committed table has {} pins",
            results.len(),
            PINS.len()
        );
        eprintln!("pin_bake: the tree-verified pin table:");
        for (name, count) in &results {
            let count = count.as_ref().expect("the kernel measured");
            eprintln!("    (\"{name}\", {count}),");
        }
        return;
    }
    assert!(
        KERNELS.len() == PINS.len(),
        "every kernel must carry a pin and every pin a kernel: {} kernels, {} pins",
        KERNELS.len(),
        PINS.len()
    );
    let mut failures = Vec::new();
    for (name, count) in &results {
        let count = count.as_ref().expect("the kernel measured");
        let recorded = PINS
            .iter()
            .find(|(pin, _)| pin == name)
            .unwrap_or_else(|| panic!("the pin table lost {name}"))
            .1;
        let drift = (*count as f64 - recorded as f64) / recorded as f64;
        if drift.abs() > TOLERANCE {
            failures.push(format!(
                "{name}: measured {count} Ir, pinned {recorded} Ir, drift {:+.2}% past the band",
                drift * 100.0
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "pin-bake divergences: {}\nre-baseline with PIN_BAKE_RECORD=1 and update PINS in the same \
         commit that changed the codegen",
        failures.join("; ")
    );
}
