//! The strippable pc-to-span section end to end: the lowered
//! module's span table names file, line, and column per instruction
//! region; the trap renderer's backtrace frames, the `?` error leg's
//! fiber-local ring, the debugger's breakpoints, and the sampling
//! profiler all name source only through it. The stripped module runs
//! identically and answers no positions.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_stdlib::AMBIENT_SOURCE;
use ds_vm::{
    Breakpoint, DebugStop, FIBER_ROOT, FrameAnswer, ModuleRegistry, Platform, SourceLocation, Trap,
    Value, Vm, loader, native,
};

const SOURCE: &str = include_str!("fixtures/span_probe.ds");

/// Builds the installed VM over the fixture's module: parse -> check ->
/// lower -> write -> read back -> install, the pipeline every tool face
/// shares.
fn build_vm() -> Vm {
    build_vm_over(&module_bytes().0)
}

/// The fixture's module bytes, with the flags the module carries.
fn module_bytes() -> (Vec<u8>, usize) {
    let mut files = Files::new();
    let amb = files.add_source("ambient.ds", AMBIENT_SOURCE);
    let id = files.add_source("span_probe.ds", SOURCE);
    let asts = [
        parse(&files, amb).expect("the ambient parses"),
        parse(&files, id).expect("the fixture parses"),
    ];
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
        "span_probe",
        Hash128::compute(SOURCE.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers");
    let names: Vec<String> = lowered.functions().to_vec();
    (write(lowered.module()), names.len())
}

fn build_vm_over(bytes: &[u8]) -> Vm {
    let module = read(bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    Vm::new(registry)
}

/// The function-table index of one named function.
fn func_of(vm: &Vm, name: &str) -> (u32, usize) {
    let version = vm
        .registry()
        .current_version("span_probe")
        .expect("installed");
    let index = version
        .functions()
        .iter()
        .position(|f| f.name == name)
        .unwrap_or_else(|| panic!("no function {name}"));
    (
        u32::try_from(index).expect("function index fits u32"),
        version.functions()[index].code.len(),
    )
}

/// The one region of `func` whose line is `line`, or None.
fn region_at(vm: &Vm, func: u32, line: u32) -> Option<u32> {
    let version = vm
        .registry()
        .current_version("span_probe")
        .expect("installed");
    let spans = version.pc_spans()?;
    let row = spans.funcs.iter().find(|row| row.func == func)?;
    row.regions
        .iter()
        .find(|region| region.at.line == line)
        .map(|region| region.pc)
}

fn loc(file: &str, line: u32, col: u32) -> SourceLocation {
    SourceLocation {
        file: file.to_owned(),
        line,
        col,
    }
}

/// The section's face: the lowered module carries the table, the file
/// row names the fixture, and the `?` sites' pcs resolve to their lines
/// through the region rows.
#[test]
fn the_section_maps_pcs_to_source_positions() {
    let (bytes, _len) = module_bytes();
    let module = read(&bytes).expect("the module reads back");
    let spans = module
        .pc_spans()
        .expect("the lowered module carries the table");
    assert_eq!(
        spans.files,
        ["ambient.ds", "span_probe.ds"],
        "the file rows in first-use order (the ambient prefix lowers first)"
    );
    let vm = build_vm();
    for (name, line) in [("leaf", 2), ("middle", 7)] {
        let (func, code_len) = func_of(&vm, name);
        let pc =
            region_at(&vm, func, line).unwrap_or_else(|| panic!("no region for {name}:{line}"));
        let code_len = u32::try_from(code_len).expect("code length fits u32");
        assert!(pc < code_len, "the region pc stays inside the body");
        let version = vm
            .registry()
            .current_version("span_probe")
            .expect("installed");
        let expected_col = if name == "leaf" { 18 } else { 15 };
        assert_eq!(
            version.location(func, pc),
            Some(("span_probe.ds", line, expected_col)),
            "{name}'s `?` marks the propagation site"
        );
    }
}

/// The error-trace face: the switch armed, the `?` error legs record the
/// failing frames innermost first (leaf, then middle, then main), each
/// resolved through the span table; disarmed, nothing records.
#[test]
fn the_error_trace_records_the_propagation_chain() {
    let mut vm = build_vm();
    vm.set_trace_errors(true);
    let answer = vm
        .run("span_probe", "main", &[])
        .expect("the entry answers");
    assert!(
        matches!(answer.as_slice(), [Value::Tag(_), Value::Ref(_)]),
        "the unhandled failure answers its two-word span: {answer:?}"
    );
    let trace = vm.error_trace();
    assert_eq!(trace.len(), 2, "one hop per taken error leg: {trace:?}");
    assert_eq!(trace[0].module, "span_probe");
    assert_eq!(trace[0].func, "leaf");
    assert_eq!(trace[0].location, Some(loc("span_probe.ds", 2, 18)));
    assert_eq!(trace[1].func, "middle");
    assert_eq!(trace[1].location, Some(loc("span_probe.ds", 7, 15)));
}

#[test]
fn the_switch_off_records_nothing() {
    let mut vm = build_vm();
    let _ = vm
        .run("span_probe", "main", &[])
        .expect("the entry answers");
    assert!(
        vm.error_trace().is_empty(),
        "the disarmed switch records no hops"
    );
}

/// The trap renderer's face: a trapped frame's backtrace entry carries
/// the span table's position (the trapping uop's own line and column).
#[test]
fn a_trapped_frame_renders_its_position() {
    let mut vm = build_vm();
    let error = vm
        .run("span_probe", "boom", &[])
        .expect_err("the overflow traps");
    assert_eq!(error, Trap::IntegerOverflow);
    let frames = vm.backtrace();
    assert!(
        !frames.is_empty(),
        "the kept frames render the trap's backtrace"
    );
    assert_eq!(frames[0].func, "overflow");
    assert_eq!(
        frames[0].location,
        Some(loc("span_probe.ds", 22, 5)),
        "the trapping `x + 1` names its own position"
    );
}

/// The debugger's face: the armed line stops the running fiber before
/// the word executes, the hit names its position, and the resume runs
/// to the next stop (the parked pc's own re-take skipped once) and then
/// to the entry's answer.
#[test]
fn the_debugger_stops_at_the_armed_line_and_resumes() {
    let mut vm = build_vm();
    let resolved = vm.arm_breakpoints(&[Breakpoint {
        file: "span_probe.ds".to_owned(),
        line: 15,
    }]);
    assert!(resolved >= 1, "the loop line resolves to at least one pc");
    let mut stops = 0;
    // the first drive enters the entry; every later drive resumes the
    // parked fiber, the parked pc's own re-take skipped once
    let mut first = true;
    loop {
        let step = if first {
            first = false;
            vm.run_debugged("span_probe", "spinmain", &[])
        } else {
            // a resumed drive ends on the next stop or the answer
            vm.resume_fiber(FIBER_ROOT, Value::Tag(native::PLATFORM_NONE))
                .map(|answer| {
                    if vm.parked_at_breakpoint() {
                        DebugStop::Breakpoint
                    } else {
                        DebugStop::Answer(answer)
                    }
                })
        };
        match step {
            Ok(DebugStop::Breakpoint) => {
                stops += 1;
                // a statement's line names several region starts (the
                // rhs's ops and the store among them), so a bounded
                // number of stops per iteration, every one on the line
                assert!(stops <= 5000, "a bounded number of stops");
                let hit = vm.breakpoint_hit().expect("a stop names its breakpoint");
                assert_eq!(hit.module, "span_probe");
                assert_eq!(hit.func, "spin");
                assert_eq!(hit.location, Some(loc("span_probe.ds", 15, 15)));
                // the resume itself is the loop's next drive: the parked
                // pc's re-take is the resume, skipped once
            },
            Ok(DebugStop::Answer(answer)) => {
                assert_eq!(answer, FrameAnswer::one(Value::I64(499_500)));
                assert!(stops >= 1000, "every iteration stopped: {stops}");

                assert!(
                    vm.breakpoint_hit().is_none(),
                    "a finished run names no stop"
                );
                return;
            },
            Err(trap) => panic!("the debugged run traps: {trap:?}"),
        }
    }
}

/// The sampling profiler's face: the armed sampler records the taken
/// back edges, and the report attributes the hits through the span
/// table (the loop body's own lines).
#[test]
fn the_sampler_attributes_the_hot_loop() {
    let mut vm = build_vm();
    vm.arm_sampling(1);
    let answer = vm
        .run("span_probe", "spinmain", &[])
        .expect("the entry answers");
    assert_eq!(answer, FrameAnswer::one(Value::I64(499_500)));
    let report = vm.sampling_report();
    assert!(!report.is_empty(), "the loop leaves samples");
    let hits: u64 = report.iter().map(|row| row.hits).sum();
    assert!(hits >= 1000, "every taken back edge sampled: {hits}");
    let spin_rows: Vec<_> = report.iter().filter(|row| row.func == "spin").collect();
    assert!(
        spin_rows
            .iter()
            .all(|row| matches!(&row.location, Some(loc) if loc.file == "span_probe.ds" && (14..=16).contains(&loc.line))),
        "the spin rows name the loop: {spin_rows:?}"
    );
    assert!(
        spin_rows
            .iter()
            .any(|row| matches!(&row.location, Some(loc) if loc.line == 14)),
        "the back edge lands in the loop head's region: {spin_rows:?}"
    );
}

/// The stripped artifact path: stripping drops the section, the module
/// still installs, runs, traps, and records hops, and every position
/// resolution answers None — behavior identical, positions degraded.
#[test]
fn the_stripped_artifact_runs_and_answers_no_positions() {
    let (bytes, _len) = module_bytes();
    let module = read(&bytes).expect("the module reads back");
    let stripped = module.stripped();
    assert!(stripped.flags().strips_debug_spans());
    let bytes = write(&stripped);
    let stripped = read(&bytes).expect("the stripped module reads back");
    assert_eq!(stripped.pc_spans(), None, "the section is gone");
    let mut vm = build_vm_over(&bytes);
    vm.set_trace_errors(true);
    let answer = vm
        .run("span_probe", "main", &[])
        .expect("the entry answers");
    assert!(
        matches!(answer.as_slice(), [Value::Tag(_), Value::Ref(_)]),
        "behavior identical: {answer:?}"
    );
    let trace = vm.error_trace();
    assert_eq!(trace.len(), 2, "the marks ride the code, not the section");
    assert!(
        trace.iter().all(|hop| hop.location.is_none()),
        "no table, no positions: {trace:?}"
    );
    let _ = vm
        .run("span_probe", "boom", &[])
        .expect_err("the overflow traps");
    let frames = vm.backtrace();
    assert!(
        frames.iter().all(|frame| frame.location.is_none()),
        "the stripped trap renders no positions: {frames:?}"
    );
}
