//! The read-path surface end to end through `ds run`: the value-position
//! subscript over the ambient collections with the assignment place's
//! resolution semantics — an out-of-range list index traps, an absent
//! map key halts at the read, the checked `get` members keep `t |
//! none` — and the for-in destructuring pattern lowering over the
//! binding sites' machinery. What the fixtures pin:
//!
//! - the read answers the element: scalars, strings (their counts balancing across a loop's
//!   repeated reads), tuples' fields, nested reads, and reads over computed receivers and probe
//!   expressions.
//! - the trap rows: an out-of-range list read reports the list-index trap naming the read, an
//!   absent map key halts through the bare trap primitive.
//! - `get` keeps the checked spelling beside the trapping read.
//! - the assignment store spelling still routes the store path, and a `const` root keeps the
//!   mut-window refusal.
//! - for-in destructures tuple elements: plain names, `_` elements, `break`/`continue` over the
//!   borrowed bindings, the bindings staying immutable.

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
    support::shared_dir("ds-cli-readpath-cache-")
}

/// Writes `source` to a scratch file and runs it, answering (status,
/// stdout, stderr).
fn run_source(source: &str) -> (std::process::ExitStatus, String, String) {
    let dir = support::guarded_dir("ds-cli-readpath-src-");
    let path = dir.join("readpath_main.ds");
    fs::write(&path, source).expect("scratch source writes");
    let out = ds().arg("run").arg(&path).output().expect("ds run spawns");
    (
        out.status,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn reads_answer_the_element_over_every_collection_shape() {
    let (status, stdout, stderr) = run_source(
        r#"fn say(s: string) -> () { println(s) }

fn main() -> () {
  const xs = [10, 20, 30]
  say("scalar ${xs[1]}")
  const words = ["alpha", "beta"]
  var i = 0
  while i < 2 {
    say("${words[i]}!")
    i += 1
  }
  const grid = [[1, 2], [3, 4]]
  say("nested ${grid[1][0]}")
  const pairs = [(1, "one"), (2, "two")]
  match pairs[0] {
    (n, s) => say("pair ${n}${s}"),
  }
  say("computed ${xs[1 + 1]}")
  say("receiver ${words[0] + words[1]}")
  var m = map[string, int]()
  m.set("a", 7)
  say("map ${m["a"]}")
}
"#,
    );
    assert!(status.success(), "the reads run clean: {stderr}");
    assert_eq!(
        stdout,
        "scalar 20\nalpha!\nbeta!\nnested 3\npair 1one\ncomputed 30\nreceiver alphabeta\nmap \
         7\n0\n",
        "the read answers carry the elements"
    );
}

#[test]
fn an_out_of_range_list_read_traps() {
    let (status, _stdout, stderr) = run_source(
        r#"fn main() -> () {
  const xs = [10, 20, 30]
  println(str(xs[5]))
}
"#,
    );
    assert!(!status.success(), "the out-of-range read stops the run");
    assert!(
        stderr.contains(
            "list object 0 holds 3 elements; index 5 names none — a subscript read, store, or \
             removal must resolve its place"
        ),
        "the trap names the read: {stderr}"
    );
}

#[test]
fn an_absent_map_key_halts_at_the_read() {
    let (status, _stdout, stderr) = run_source(
        r#"fn main() -> () {
  var m = map[string, int]()
  m.set("a", 1)
  println(str(m["a"]))
  println(str(m["b"]))
}
"#,
    );
    assert!(!status.success(), "the absent key stops the run");
    assert_eq!(
        _stdout, "1\n",
        "the present key's read answers before the halt"
    );
    assert!(
        stderr.contains(
            "trap: die: the map holds no value under the key; a subscript read must resolve, it \
             carries no miss answer"
        ),
        "the halt names the read: {stderr}"
    );
}

#[test]
fn the_checked_get_members_keep_their_spelling() {
    let (status, stdout, stderr) = run_source(
        r#"fn main() -> () {
  const xs = [10, 20]
  var i = 0
  while i < 3 {
    println(str(xs.get(i).unwrap_or(-1)))
    i += 1
  }
}
"#,
    );
    assert!(status.success(), "get keeps the checked union: {stderr}");
    assert_eq!(
        stdout, "10\n20\n-1\n0\n",
        "the miss answers none's fallback"
    );
}

#[test]
fn the_store_spelling_keeps_its_own_path() {
    let (status, stdout, stderr) = run_source(
        r#"fn main() -> () {
  var xs = [10, 20]
  xs[0] = 99
  xs[1] += 5
  println(str(xs[0]))
  println(str(xs[1]))
}
"#,
    );
    assert!(status.success(), "the store spelling runs: {stderr}");
    assert_eq!(stdout, "99\n25\n0\n");
    // a const root keeps the mut-window refusal on the store
    let (status, _stdout, stderr) = run_source(
        r#"fn main() -> () {
  const xs = [10, 20]
  xs[0] = 99
  println(str(xs[0]))
}
"#,
    );
    assert!(!status.success(), "a const root refuses the store");
    assert!(
        stderr.contains("DS3006"),
        "the mut-window refusal fires: {stderr}"
    );
}
#[test]
fn for_in_destructures_the_tuple_elements() {
    let (status, stdout, stderr) = run_source(
        r#"fn main() -> () {
  const pairs = [(1, "one"), (2, "two"), (3, "three")]
  var total = 0
  for (n, _) in pairs {
    total += n
    if n == 2 { continue }
    if n == 3 { break }
  }
  println(str(total))
  for (n, s) in pairs {
    println("${s}:${n}")
  }
}
"#,
    );
    assert!(status.success(), "the destructuring loops run: {stderr}");
    assert_eq!(stdout, "6\none:1\ntwo:2\nthree:3\n0\n");
}

#[test]
fn the_destructured_bindings_stay_immutable() {
    let (status, _stdout, stderr) = run_source(
        r#"fn main() -> () {
  const pairs = [(1, "one")]
  for (n, s) in pairs {
    s = "x"
    println(s)
  }
}
"#,
    );
    assert!(
        !status.success(),
        "a destructured binding refuses assignment"
    );
    assert!(
        stderr.contains("assignment needs a `var` binding"),
        "the immutability refusal fires: {stderr}"
    );
}
