//! The bounded channel through `ds run`: the source surface — `chan_new(cap)`, the `.send`/`.recv`
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
//! - the honest deadlock: a recv with no live sender traps (exit 1, the stall guard's report), the
//!   rendezvous send's own degenerate with it (a parked sender no live fiber serves).
//! - the bound: `chan_new(-1)` traps its named refusal; zero is the rendezvous bound.
//! - the rendezvous (the zero-bound channel): a send parks its staged span until a receive takes it
//!   whole, a receive parks until a send pairs directly — both orders, a multi-word span, and the
//!   answer ordering across a re-parking sender.
//! - multi-word payloads (the collection span seam): a record and a tuple payload cross at the
//!   buffer's element stride, the recv answering the tag-less total of the span's words; a parked
//!   recv wakes to a multi-word claim, a parked multi-word send refills from the drain, and a union
//!   payload's bare member stages its whole region span.
//! - the deadline twins (the channel send/recv deadlines round): `send_deadline`/`recv_deadline`
//!   bound the wait with the io taxonomy's own `timed_out` loser — a past deadline answering at
//!   once, the win door waking before the deadline with the ok span, the timed-out recv leaving the
//!   next waiter the wake, the timed-out send's staged payload never crossing, a cut loser (the
//!   timer race over a parked deadline send) keeping its deinit and leaving the queue clean, and
//!   the lowering refusals (a wider element answer, a deinit-carrying payload, a bare tail).
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
fn a_negative_bound_traps_its_named_refusal() {
    let (status, _stdout, stderr) = run_source(
        r"fn main() -> int {
  const ch = chan[int](-1)
  0
}
",
    );
    assert_eq!(status.code(), Some(1), "the bound refuses: {status}");
    assert!(
        stderr.contains("refuses a negative bound"),
        "the refusal names the seam: {stderr}"
    );
}

#[test]
fn a_rendezvous_sender_parks_until_the_sibling_receives() {
    // bound zero: the send parks its staged payload in the channel's
    // own queue, and the later sibling's receive takes it whole — the
    // waking peer the resumer
    let (status, stdout, stderr) = run_source(
        r#"fn main() -> int {
  const ch = chan[int](0)
  const _ = nursery {
    spawn { ch.send(41) }
    spawn { println("rendez ${ch.recv()}") }
  }
  0
}
"#,
    );
    assert!(status.success(), "the rendezvous runs: {stderr}");
    assert!(
        stdout.contains("rendez 41"),
        "the staged payload crossed at the rendezvous: {stdout}"
    );
}

#[test]
fn a_rendezvous_receiver_parks_and_the_sibling_send_pairs_directly() {
    // the other door: the receive parks first, and the send — no
    // buffer room ever — hands its payload straight to the parked
    // receiver
    let (status, stdout, stderr) = run_source(
        r#"fn main() -> int {
  const ch = chan[int](0)
  const _ = nursery {
    spawn { println("paired ${ch.recv()}") }
    spawn { ch.send(77) }
  }
  0
}
"#,
    );
    assert!(status.success(), "the rendezvous runs: {stderr}");
    assert!(
        stdout.contains("paired 77"),
        "the parked receiver woke with the paired payload: {stdout}"
    );
}

#[test]
fn a_rendezvous_answers_in_staged_order_across_a_reparking_sender() {
    // both of the sender's sends park (the fiber re-parks after each
    // wake), and the draining receives answer in staged order — the
    // channel's own queue, FIFO
    let (status, stdout, stderr) = run_source(
        r#"fn main() -> int {
  const ch = chan[int](0)
  const _ = nursery {
    spawn {
      ch.send(41)
      ch.send(77)
    }
    spawn { println("drain ${ch.recv()} ${ch.recv()}") }
  }
  0
}
"#,
    );
    assert!(status.success(), "the rendezvous runs: {stderr}");
    assert!(
        stdout.contains("drain 41 77"),
        "the staged payloads crossed in park order: {stdout}"
    );
}

#[test]
fn a_rendezvous_record_payload_crosses_whole() {
    // the multi-word span discipline the full-buffer park already
    // owns: the parked send stages its whole record span and the take
    // answers the tag-less total
    let (status, stdout, stderr) = run_source(
        r#"struct pair { a: int, b: int }
fn main() -> int {
  const ch = chan[pair](0)
  const _ = nursery {
    spawn { ch.send(pair { a: 41, b: 59 }) }
    spawn {
      const p = ch.recv()
      println("pair ${p.a} ${p.b}")
    }
  }
  0
}
"#,
    );
    assert!(status.success(), "the multi-word rendezvous runs: {stderr}");
    assert!(
        stdout.contains("pair 41 59"),
        "the staged record span crossed word-for-word: {stdout}"
    );
}

#[test]
fn a_rendezvous_send_with_no_receiver_traps_the_stall_guard() {
    // the rendezvous's own honest degenerate: a send parked on a
    // channel no live fiber serves deadlocks through the stall guard
    // exactly the empty receive's
    let (status, _stdout, stderr) = run_source(
        r"fn main() -> int {
  const ch = chan[int](0)
  ch.send(41)
  0
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
fn the_empty_race_parks_the_rendezvous_answers_channel_into_the_deadlock() {
    // firstof's empty race builds a zero-bound answers channel: the
    // rendezvous bound keeps the construction from trapping, so the one
    // receive parks with no fiber to serve it and the stall guard
    // answers the honest deadlock (the degenerate race)
    let (status, _stdout, stderr) = run_source(
        r"fn main() -> int {
  const x = firstof(list[fn() -> int]())
  x
}
",
    );
    assert_eq!(status.code(), Some(1), "the empty race deadlocks: {status}");
    assert!(
        stderr.contains("nothing runnable"),
        "the stall guard reports: {stderr}"
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

#[test]
fn a_past_deadline_answers_timed_out_at_once() {
    // the deadline bounds only the wait: a servable channel answers the
    // ok span under the same past deadline, a blocked one the failure
    let (status, stdout, stderr) = run_source(
        r#"fn main() -> int {
  const full = chan[int](1)
  full.send(5)
  const blocked = full.send_deadline(0, 6).err((e) => -1)
  const drained = full.recv_deadline(0).err((e) => -3)
  full.send_deadline(0, 7).err((e) => 0)
  const stocked = full.recv_deadline(0).err((e) => -5)
  const empty = chan[int](1)
  const starved = empty.recv_deadline(0).err((e) => -4)
  println("blocked=${blocked} drained=${drained} stocked=${stocked} starved=${starved}")
  0
}
"#,
    );
    assert!(status.success(), "the at-once answers exit clean: {stderr}");
    assert!(
        stdout.contains("blocked=-1 drained=5 stocked=7 starved=-4"),
        "the past deadline answered each side: {stdout}"
    );
}

#[test]
fn a_deadline_recv_wakes_through_the_win_door_before_its_deadline() {
    // the woken deadline call overlays the two-word ok span at the
    // parked site: the element answers, no failure member
    let (status, stdout, stderr) = run_source(
        r#"fn main() -> int {
  const ch = chan[int](1)
  const _ = nursery {
    spawn { println("woke ${ch.recv_deadline(500000000).err((e) => -1)}") }
    spawn { ch.send(7) }
  }
  0
}
"#,
    );
    assert!(status.success(), "the win door exits clean: {stderr}");
    assert!(stdout.contains("woke 7"), "the element answered: {stdout}");
}

#[test]
fn the_timed_out_recv_leaves_the_next_waiter_the_wake() {
    // the loser deregisters from the receiver queue, so the later
    // plain recv still takes the delayed send's wake in order
    let (status, stdout, stderr) = run_source(
        r#"fn main() -> int {
  const ch = chan[int](1)
  const _ = nursery {
    spawn { println("loser ${ch.recv_deadline(20000000).err((e) => -1)}") }
    spawn {
      sleep(150000000)
      println("survivor ${ch.recv()}")
    }
    spawn {
      sleep(250000000)
      ch.send(42)
    }
  }
  0
}
"#,
    );
    assert!(status.success(), "the loser's cut exits clean: {stderr}");
    let lines: Vec<&str> = stdout
        .lines()
        .filter(|line| !line.is_empty() && *line != "0")
        .collect();
    assert_eq!(
        lines,
        ["loser -1", "survivor 42"],
        "the loser answered the failure span and the survivor took the wake: {stdout}"
    );
}

#[test]
fn the_timed_out_senders_staged_payload_never_crosses() {
    // the loser parks on the full channel staging 11, times out, and a
    // later plain sender's 22 refills the drain in its place
    let (status, stdout, stderr) = run_source(
        r#"fn main() -> int {
  const ch = chan[int](1)
  ch.send(5)
  const _ = nursery {
    spawn { println("loser ${ch.send_deadline(20000000, 11).err((e) => -1)}") }
    spawn {
      sleep(150000000)
      ch.send(22)
    }
    spawn {
      sleep(250000000)
      println("drain ${ch.recv()} ${ch.recv()}")
    }
  }
  0
}
"#,
    );
    assert!(status.success(), "the drain exits clean: {stderr}");
    let lines: Vec<&str> = stdout
        .lines()
        .filter(|line| !line.is_empty() && *line != "0")
        .collect();
    assert_eq!(
        lines,
        ["loser -1", "drain 5 22"],
        "the survivor's payload refilled, the loser's 11 never crossed: {stdout}"
    );
}

#[test]
fn a_cut_deadline_loser_keeps_its_deinit_and_leaves_the_queue_clean() {
    // the timer race cuts a sender parked on a deadline send: the kept
    // frame's hook fires at the teardown drain, and the staged entry is
    // gone with the cut — a later rendezvous pair still crosses
    let (status, stdout, stderr) = run_source(
        r#"struct res: ~Copyable {
  id: int
  deinit { println("freed ${self.id}") }
}

fn main() -> () {
  const ch = chan[int](0)
  const held = timeout(20000000, () => {
    const r = res { id: 7 }
    ch.send_deadline(5000000000, r.id).err((e) => 0)
    0
  }).err((e) => -1)
  println("cut ${held}")
  const _ = nursery {
    spawn { ch.send(99) }
    spawn { println("after ${ch.recv()}") }
  }
}
"#,
    );
    assert!(status.success(), "the cut exits clean: {stderr}");
    let lines: Vec<&str> = stdout
        .lines()
        .filter(|line| !line.is_empty() && *line != "0")
        .collect();
    assert_eq!(
        lines,
        ["freed 7", "cut -1", "after 99"],
        "the cut loser's deinit fired and the queue pairs the later peers: {stdout}"
    );
}

#[test]
fn the_helper_extraction_propagates_timed_out_through_the_call() {
    // the deadline member composes with `?`, `.err`, and a match arm
    // exactly the timer race's answer shapes
    let (status, stdout, stderr) = run_source(
        r#"fn helper(ch: chan[int], nanos: int) -> int | timed_out {
  ch.recv_deadline(nanos)?
}

fn main() -> () {
  const ch = chan[int](1)
  ch.send(5)
  println("helper ${helper(ch, 0).err((e) => -1)}")
  println("member ${ch.recv_deadline(0).err((e) => -2)}")
  match helper(ch, 0) {
    n => println("match ${n.err((e) => -3)}")
  }
}
"#,
    );
    assert!(
        status.success(),
        "the extraction shapes exit clean: {stderr}"
    );
    let lines: Vec<&str> = stdout
        .lines()
        .filter(|line| !line.is_empty() && *line != "0")
        .collect();
    assert_eq!(
        lines,
        ["helper 5", "member -2", "match -3"] as [&str; 3],
        "the ok word extracted and the deadline propagated: {stdout}"
    );
}

#[test]
fn a_wider_element_answer_refuses_at_the_recv_deadlines_lowering() {
    // the deadline recv adopts exactly one word off its element, the
    // timer race's own boundary
    let (status, _stdout, stderr) = run_source(
        r#"struct pair { a: int, b: int }

fn main() -> int {
  const ch = chan[pair](1)
  const p = ch.recv_deadline(500000000).err((e) => pair { a: 0, b: 0 })
  p.a
}
"#,
    );
    assert!(!status.success(), "the wider answer refuses: {stderr}");
    assert!(
        stderr.contains("a deadline answer wider than one word"),
        "the refusal names its reason: {stderr}"
    );
}

#[test]
fn a_deinit_carrying_payload_refuses_at_the_send_deadlines_staging() {
    let (status, _stdout, stderr) = run_source(
        r#"struct held: ~Copyable {
  id: int
  deinit { println("freed ${self.id}") }
}

fn main() -> int {
  const ch = chan[held](1)
  const h = held { id: 1 }
  ch.send_deadline(500000000, h).err((e) => 0)
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
