//! The bounded channel through `ds run` (the 2026-11-21 shared-state
//! ruling): the source surface — `chan_new(cap)`, the `.send`/`.recv`
//! member spellings, payloads crossing child heaps into the
//! parent-owned buffer — under the CLI's own drive. What the fixtures
//! pin:
//!
//! - park on empty inside a nursery: a spawn that recvs an empty channel parks, a later sibling's
//!   send wakes it, and the received value prints.
//! - park on full: a spawn past the bound parks, the draining sibling refills the freed slot, both
//!   payloads print in order.
//! - a string payload crossing heaps: a child's own-heap string deposits through the foreign-post
//!   funnel and reads back on the parent's drain.
//! - the honest deadlock: a recv with no live sender traps (exit 1, the stall guard's report).
//! - the bound: `chan_new(0)` traps its named refusal.
//! - multi-word payloads (the collection span seam): a record and a tuple payload cross at the
//!   buffer's element stride, the recv answering the tag-less total of the span's words; a parked
//!   recv wakes to a multi-word claim, a parked multi-word send refills from the drain, and a union
//!   payload's bare member stages its whole region span.
//! - lowering refusals: a deinit-carrying payload refuses at the send's staging, before any run.

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
    support::shared_dir("ds-cli-chan-cache-")
}

/// Writes `source` to a scratch file and runs it, answering (status,
/// stdout, stderr).
fn run_source(source: &str) -> (std::process::ExitStatus, String, String) {
    let dir = support::guarded_dir("ds-cli-chan-src-");
    let path = dir.join("chan_main.ds");
    fs::write(&path, source).expect("scratch source writes");
    let out = ds().arg("run").arg(&path).output().expect("ds run spawns");
    (
        out.status,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_parked_recv_wakes_through_the_siblings_send() {
    let (status, stdout, stderr) = run_source(
        r#"fn main() -> int {
  const ch = chan[int](1)
  const _ = nursery {
    spawn { println("woke ${ch.recv()}") }
    spawn { ch.send(42) }
  }
  0
}
"#,
    );
    assert!(status.success(), "the run traps: {stderr}");
    assert!(
        stdout.contains("woke 42"),
        "the parked receiver printed: {stdout}"
    );
}

#[test]
fn a_parked_sender_refills_from_the_draining_sibling() {
    let (status, stdout, stderr) = run_source(
        r#"fn main() -> int {
  const ch = chan[int](1)
  const _ = nursery {
    spawn {
      ch.send(1)
      ch.send(2)
    }
    spawn {
      const a = ch.recv()
      const b = ch.recv()
      println("a=${a} b=${b}")
    }
  }
  0
}
"#,
    );
    assert!(status.success(), "the run traps: {stderr}");
    assert!(
        stdout.contains("a=1 b=2"),
        "both payloads crossed in order: {stdout}"
    );
}

#[test]
fn a_childs_string_crosses_heaps_into_the_parent_owned_buffer() {
    let (status, stdout, stderr) = run_source(
        r#"fn main() -> int {
  const ch = chan[string](1)
  const _ = nursery {
    spawn { ch.send("cross-heap") }
  }
  println(ch.recv())
  0
}
"#,
    );
    assert!(status.success(), "the run traps: {stderr}");
    assert!(
        stdout.contains("cross-heap"),
        "the foreign payload read back on the drain: {stdout}"
    );
}

#[test]
fn a_recv_with_no_live_sender_traps_the_stall_guard() {
    let (status, _stdout, stderr) = run_source(
        r"fn main() -> int {
  const ch = chan[int](1)
  ch.recv()
}
",
    );
    assert_eq!(status.code(), Some(1), "the deadlock traps: {status}");
    assert!(
        stderr.contains("nothing runnable"),
        "the stall guard reports: {stderr}"
    );
}

#[test]
fn a_bound_below_one_traps_its_named_refusal() {
    let (status, _stdout, stderr) = run_source(
        r"fn main() -> int {
  const ch = chan[int](0)
  0
}
",
    );
    assert_eq!(status.code(), Some(1), "the bound refuses: {status}");
    assert!(
        stderr.contains("refuses a bound below one"),
        "the refusal names the seam: {stderr}"
    );
}

#[test]
fn a_record_payload_crosses_fibers_at_the_element_stride() {
    let (status, stdout, stderr) = run_source(
        r#"struct pair { a: int, b: int }

fn main() -> int {
  const ch = chan[pair](1)
  const _ = nursery {
    spawn { ch.send(pair { a: 3, b: 4 }) }
  }
  const p = ch.recv()
  println("got ${p.a}+${p.b}")
  0
}
"#,
    );
    assert!(status.success(), "the run traps: {stderr}");
    assert!(
        stdout.contains("got 3+4"),
        "the record payload crossed whole: {stdout}"
    );
}

#[test]
fn a_tuple_payload_crosses_and_destructures() {
    let (status, stdout, stderr) = run_source(
        r#"fn main() -> int {
  const ch = chan[(int, string)](1)
  const _ = nursery {
    spawn { ch.send((42, "answer")) }
  }
  const (n, label) = ch.recv()
  println("${label}=${n}")
  0
}
"#,
    );
    assert!(status.success(), "the run traps: {stderr}");
    assert!(
        stdout.contains("answer=42"),
        "the tuple payload crossed whole: {stdout}"
    );
}

#[test]
fn a_parked_recv_wakes_to_a_multi_word_claim() {
    let (status, stdout, stderr) = run_source(
        r#"struct pair { a: int, b: int }

fn main() -> int {
  const ch = chan[pair](1)
  const _ = nursery {
    spawn {
      const p = ch.recv()
      println("woke ${p.a}+${p.b}")
    }
    spawn { ch.send(pair { a: 5, b: 6 }) }
  }
  0
}
"#,
    );
    assert!(status.success(), "the run traps: {stderr}");
    assert!(
        stdout.contains("woke 5+6"),
        "the woken recv claimed the whole span: {stdout}"
    );
}

#[test]
fn a_parked_multi_word_send_refills_from_the_drain() {
    let (status, stdout, stderr) = run_source(
        r#"struct pair { a: int, b: int }

fn main() -> int {
  const ch = chan[pair](1)
  const _ = nursery {
    spawn {
      ch.send(pair { a: 1, b: 2 })
      ch.send(pair { a: 3, b: 4 })
    }
    spawn {
      const p = ch.recv()
      const q = ch.recv()
      println("first=${p.a},${p.b} second=${q.a},${q.b}")
    }
  }
  0
}
"#,
    );
    assert!(status.success(), "the run traps: {stderr}");
    assert!(
        stdout.contains("first=1,2 second=3,4"),
        "both spans crossed in order, the parked one refilled: {stdout}"
    );
}

#[test]
fn a_union_payload_stages_its_whole_member_region() {
    let (status, stdout, stderr) = run_source(
        r#"type slot = int | string

fn main() -> int {
  const ch = chan[slot](2)
  ch.send(11)
  ch.send("twelve")
  const a = ch.recv()
  const b = ch.recv()
  const ok = (a is int && b is string)
  println("ok=${ok}")
  0
}
"#,
    );
    assert!(status.success(), "the run traps: {stderr}");
    assert!(
        stdout.contains("ok=true"),
        "both members crossed at the union's region span: {stdout}"
    );
}

#[test]
fn a_deinit_carrying_payload_refuses_at_lowering() {
    let (status, _stdout, stderr) = run_source(
        r#"struct held: ~Copyable {
  id: int
  deinit { println("freed ${self.id}") }
}

fn main() -> int {
  const ch = chan[held](1)
  const h = held { id: 1 }
  ch.send(h)
  0
}
"#,
    );
    assert!(!status.success(), "a deinit-carrying payload refuses");
    assert!(
        stderr.contains("deinit-carrying"),
        "the refusal names the claim seam: {stderr}"
    );
}
