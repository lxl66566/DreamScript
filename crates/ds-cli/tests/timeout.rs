//! The timer race through `ds run`: the deadline is an answered failure — the io taxonomy's
//! own `timed_out` member riding the classed fallible native span —
//! so `?` and `.err` extract it and every race exits 0, the timeout
//! trap retired. What the fixtures pin:
//!
//! - the real deadline: a parked racer cut at its deadline, the `.err` fallback answering, the
//!   racer's own line absent.
//! - the past deadline: `timeout(0, ...)` answers the failure at once, no racer line.
//! - a looping racer that parks each iteration is cut at its deadline, and so is a never-parking
//!   one: the interpreter's back-edge deadline poll answers where no wheel door reaches.
//! - the cut racer's kept deinits fire at the teardown drain in the scope-exit order.
//! - the answer shapes: a string answer adopted whole off the overlay word; a `?`-propagating
//!   helper whose tail rides the operand's own union.
//! - the racer's own nursery: a thunk joining a block nursery is cut with its slow child, the
//!   child's line staying absent; the racer's own `cancel` crosses the block boundary as the
//!   payloaded cut, and a sibling's `cancel` cuts the racer with its deinit firing while the block
//!   answers the sibling's tag.
//! - a racer parked on an empty channel cut at the deadline; a nested race answering inside the
//!   outer deadline.
//! - the refusals: a wider-than-one-word answer and a bare tail call to the race refuse at the
//!   call's lowering with DS4009.

use std::{env, fs, path::PathBuf, process::Command};

mod support;

fn ds() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command.current_dir(env!("CARGO_MANIFEST_DIR"));
    command.env("DS_CACHE", test_cache());
    command
}

/// A private per-thread script-cache root: spawned runs never read or
/// write a real user cache directory, and the thread's exit removes it.
fn test_cache() -> PathBuf {
    support::shared_dir("ds-cli-timeout-cache-")
}

/// Writes `source` to a scratch file and runs it, answering (status,
/// stdout, stderr).
fn run_source(source: &str) -> (std::process::ExitStatus, String, String) {
    let dir = support::guarded_dir("ds-cli-timeout-src-");
    let path = dir.join("timeout_main.ds");
    fs::write(&path, source).expect("scratch source writes");
    let out = ds().arg("run").arg(&path).output().expect("ds run spawns");
    (
        out.status,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn the_real_deadline_cuts_a_parked_racer_and_the_err_fallback_answers() {
    let (status, stdout, stderr) = run_source(
        r#"fn main() -> () {
  const held = timeout(20000000) {
    sleep(2000000000)
    println("racer ran on")
    9
  }.err((e) => -1)
  println("cut ${held}")
}
"#,
    );
    assert!(
        status.success(),
        "the handled deadline exits clean: {stderr}"
    );
    assert!(stdout.contains("cut -1"), "the fallback answered: {stdout}");
    assert!(
        !stdout.contains("racer ran on"),
        "the racer never ran: {stdout}"
    );
}

#[test]
fn the_past_deadline_answers_at_once_with_no_racer() {
    let (status, stdout, stderr) = run_source(
        r#"fn main() -> () {
  const at_once = timeout(0) {
    println("racer ran")
    9
  }.err((e) => -1)
  println("at once ${at_once}")
}
"#,
    );
    assert!(
        status.success(),
        "the at-once deadline exits clean: {stderr}"
    );
    assert!(
        stdout.contains("at once -1"),
        "the failure answered at once: {stdout}"
    );
    assert!(!stdout.contains("racer ran"), "no racer ever ran: {stdout}");
}

#[test]
fn the_deadline_cuts_a_parking_looper() {
    let (status, stdout, stderr) = run_source(
        r#"fn main() -> () {
  const held = timeout(20000000) {
    var j = 0
    while j < 100000000 {
      j += 1
      sleep(1)
    }
    j
  }.err((e) => -1)
  println("cut ${held}")
}
"#,
    );
    assert!(status.success(), "the looper's cut exits clean: {stderr}");
    assert!(
        stdout.contains("cut -1"),
        "the parking looper was cut: {stdout}"
    );
}

#[test]
fn the_deadline_cuts_a_never_parking_looper() {
    // a body that never parks: the wheel fires only when no fiber is
    // runnable, so the cut answers through the interpreter's back-edge
    // deadline poll (the drive never spins past its own deadline)
    let (status, stdout, stderr) = run_source(
        r#"fn main() -> () {
  const held = timeout(20000000) {
    var j = 0
    while j >= 0 {
      j += 1
    }
    j
  }.err((e) => -1)
  println("cut ${held}")
}
"#,
    );
    assert!(status.success(), "the spinner's cut exits clean: {stderr}");
    assert!(
        stdout.contains("cut -1"),
        "the never-parking looper was cut: {stdout}"
    );
}

#[test]
fn the_cut_racer_drops_its_kept_values_innermost_first() {
    let (status, stdout, stderr) = run_source(
        r#"struct res: ~Copyable {
  id: int
  deinit { println("freed ${self.id}") }
}

fn main() -> () {
  const held = timeout(20000000) {
    const outer = res { id: 2 }
    const inner = res { id: 3 }
    sleep(2000000000)
    println("racer ran on")
    outer.id + inner.id
  }.err((e) => -1)
  println("cut ${held}")
}
"#,
    );
    assert!(status.success(), "the teardown drain exits clean: {stderr}");
    assert!(
        stdout.contains("cut -1"),
        "the cut racer answered the fallback: {stdout}"
    );
    let freed = stdout
        .lines()
        .filter(|line| line.starts_with("freed "))
        .collect::<Vec<_>>();
    assert_eq!(
        freed,
        ["freed 2", "freed 3"],
        "the cut racer's kept values dropped in the scope-exit order: {stdout}"
    );
}

#[test]
fn the_answer_shapes_adopt_a_string_and_a_helper_propagates() {
    let (status, stdout, stderr) = run_source(
        r#"fn helper() -> int | timed_out {
  timeout(500000000, () => 5)?
}

fn main() -> () {
  const held = timeout(500000000, () => "owned").err((e) => "cut")
  println("held ${held}")
  println("helper ${helper().err((e) => -1)}")
}
"#,
    );
    assert!(status.success(), "the answer shapes exit clean: {stderr}");
    assert!(
        stdout.contains("held owned") && stdout.contains("helper 5"),
        "the string adopted and the helper's `?` answered: {stdout}"
    );
}

#[test]
fn the_racers_own_nursery_is_torn_down_at_the_cut() {
    let (status, stdout, stderr) = run_source(
        r#"fn main() -> () {
  const held = timeout(20000000) {
    nursery {
      spawn {
        sleep(10000000)
        println("child quick")
      }
      spawn {
        sleep(2000000000)
        println("child never")
      }
    }
  }.err((e) => -1)
  println("cut ${held}")
}
"#,
    );
    assert!(
        status.success(),
        "the racer's teardown exits clean: {stderr}"
    );
    assert!(stdout.contains("cut -1"), "the racer was cut: {stdout}");
    assert!(
        !stdout.contains("child never"),
        "the slow child never ran on: {stdout}"
    );
}

#[test]
fn the_racers_cancel_crosses_the_block_boundary() {
    let (status, stdout, stderr) = run_source(
        r#"tag bail

fn thunk() -> bail {
  cancel(bail)
  bail
}

fn raced() -> bail | int {
  const ys = nursery {
    spawn {
      const v = timeout(500000000, thunk).err((e) => 0)
      println("race answered ${v}")
    }
  }
  ys.len()
}

fn main() -> () {
  match raced() {
    bail => println("racer cancelled")
    n => println("won ${n}")
  }
}
"#,
    );
    assert!(status.success(), "the racer's cancel exits clean: {stderr}");
    assert!(
        stdout.contains("racer cancelled"),
        "the racer's own cancel crossed the block boundary as the payloaded cut: {stdout}"
    );
}

#[test]
fn the_sibling_cancel_cuts_the_racer_and_fires_its_deinit() {
    let (status, stdout, stderr) = run_source(
        r#"struct res: ~Copyable {
  id: int
  deinit { println("freed ${self.id}") }
}

tag bail

fn main() -> () {
  match raced() {
    bail => println("owner cancelled")
    n => println("won ${n}")
  }
}

fn raced() -> bail | int {
  const ys = nursery {
    spawn {
      timeout(500000000, () => {
        const held = res { id: 2 }
        sleep(5000000000)
        println("racer ran on")
        held.id
      }).err((e) => 0)
    }
    spawn {
      sleep(30000000)
      cancel(bail)
    }
  }
  ys.len()
}
"#,
    );
    assert!(status.success(), "the sibling cancel exits clean: {stderr}");
    assert!(
        stdout.contains("owner cancelled"),
        "the block answered the sibling's tag: {stdout}"
    );
    assert!(
        stdout.contains("freed 2"),
        "the cut racer's deinit fired: {stdout}"
    );
    assert!(
        !stdout.contains("racer ran on"),
        "the racer never ran on: {stdout}"
    );
}

#[test]
fn a_racer_parked_on_an_empty_channel_is_cut_at_the_deadline() {
    let (status, stdout, stderr) = run_source(
        r#"fn main() -> () {
  const ch = chan[int](1)
  const held = timeout(20000000, () => ch.recv()).err((e) => -1)
  println("cut ${held}")
}
"#,
    );
    assert!(status.success(), "the parked racer exits clean: {stderr}");
    assert!(
        stdout.contains("cut -1"),
        "the chan-parked racer was cut: {stdout}"
    );
}

#[test]
fn the_nested_race_answers_inside_the_outer_deadline() {
    let (status, stdout, stderr) = run_source(
        r#"fn main() -> () {
  const held = timeout(500000000, () => {
    timeout(500000000, () => {
      sleep(10000000)
      7
    }).err((e) => 0)
  }).err((e) => -1)
  println("nested ${held}")
}
"#,
    );
    assert!(status.success(), "the nested race exits clean: {stderr}");
    assert!(
        stdout.contains("nested 7"),
        "the inner race answered inside the outer deadline: {stdout}"
    );
}

#[test]
fn the_wider_answer_refuses_at_the_calls_lowering() {
    let (status, stdout, stderr) = run_source(
        r#"struct wide { a: int, b: int }

fn widened() -> wide {
  wide { a: 1, b: 2 }
}

fn main() -> () {
  const held = timeout(500000000, widened).err((e) => wide { a: 0, b: 0 })
  println("w ${held.a}")
}
"#,
    );
    assert!(!status.success(), "the wider answer refuses: {stdout}");
    assert!(
        stderr.contains("a deadline answer wider than one word"),
        "the refusal names its reason: {stderr}"
    );
}

#[test]
fn the_bare_tail_call_refuses_at_the_calls_lowering() {
    let (status, stdout, stderr) = run_source(
        r#"fn quick() -> int {
  41
}

fn tailed(nanos: int) -> int | timed_out {
  timeout(nanos, quick)
}

fn main() -> () {
  const v = tailed(0).err((e) => 0)
  println("tail ${v}")
}
"#,
    );
    assert!(!status.success(), "the bare tail refuses: {stdout}");
    assert!(
        stderr.contains("a tail call to a fallible native"),
        "the refusal names its reason: {stderr}"
    );
}
