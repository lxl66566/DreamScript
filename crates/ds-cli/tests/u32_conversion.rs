//! The u32 width's trapping construction: an ambient library member over the checked `u32_of`
//! native and the bare `trap` primitive — in range it answers the width's word,
//! out of range it traps naming the value and the range, the message
//! riding the Die trap so the CLI's trap report prints it exactly like
//! `die` (a failed exit, nothing running past the trap). The compiler
//! is untouched: the member rides the u32 type's receiverless static
//! tier (`u32.of(x)`, the `int.default()` precedent), because the
//! bare `u32(x)` call form cannot coexist with the width's
//! type atom — no value-namespace `u32` means the callee resolves the
//! tag, and a fn beside the tag would be the DS2014 ambiguity error —
//! and the checked `u32_of` stays the fallible form.

use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Output},
};

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
    support::shared_dir("ds-cli-u32-cache-")
}

/// Writes one script under a temp dir and answers its path.
fn script(tag: &str, name: &str, text: &str) -> PathBuf {
    let dir = support::guarded_dir(&format!("ds-cli-u32-{tag}-"));
    let file = dir.join(name);
    fs::write(&file, text).expect("the script writes");
    file
}

fn run(args: &[&str]) -> Output {
    ds().args(args).output().expect("the binary runs")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Asserts each fragment appears, in order.
fn in_order(haystack: &str, fragments: &[&str]) {
    let mut cursor = 0;
    for fragment in fragments {
        let found = haystack[cursor..]
            .find(fragment)
            .unwrap_or_else(|| panic!("missing `{fragment}` after byte {cursor} in:\n{haystack}"));
        cursor += found + fragment.len();
    }
}

/// The in-range answers: both range edges read back through
/// `int_of_u32`, and the checked `u32_of` beside them stays the
/// fallible form (an out-of-range int answering none, never a trap).
#[test]
fn the_in_range_construction_answers_the_width_word() {
    let file = script(
        "range",
        "u32_edges.ds",
        concat!(
            "fn main() -> () {\n",
            "  println(\"${int_of_u32(u32.of(0))}\")\n",
            "  println(\"${int_of_u32(u32.of(4294967295))}\")\n",
            "  println(\"${u32_of(4294967296) is none} ${u32_of(-1) is none}\")\n",
            "}\n",
        ),
    );
    let out = run(&["run", file.to_str().expect("temp path is UTF-8")]);
    assert!(out.status.success(), "the edges run failed: {out:?}");
    assert_eq!(
        text(&out.stdout),
        "0\n4294967295\ntrue true\n0\n",
        "both edges answer and the checked form stays fallible"
    );
}

/// The out-of-range trap rides the Die trap like `die`: the report
/// prints the member's message — the value and the range named —
/// through the trap report, the run exits failed, and nothing runs past
/// the trap.
#[test]
fn the_out_of_range_construction_traps_with_the_named_message() {
    let file = script(
        "trap",
        "u32_trap.ds",
        concat!(
            "fn main() -> () {\n",
            "  println(\"before\")\n",
            "  println(\"${int_of_u32(u32.of(4294967296))}\")\n",
            "  println(\"unreachable\")\n",
            "}\n",
        ),
    );
    let out = run(&["run", file.to_str().expect("temp path is UTF-8")]);
    assert_eq!(out.status.code(), Some(1), "the trapped run exits failed");
    assert_eq!(text(&out.stdout), "before\n", "nothing runs past the trap");
    let err = text(&out.stderr);
    in_order(&err, &[
        "trap: die: u32: 4294967296 is out of range 0..4294967295",
        "u32_trap::main",
    ]);
}

/// The negative edge traps the same way (the range's low side), and the
/// fresh lowering agrees with the served cold path: `--no-cache`
/// answers the same trap line.
#[test]
fn the_negative_edge_traps_and_the_fresh_path_agrees() {
    let file = script(
        "negative",
        "u32_negative.ds",
        concat!(
            "fn main() -> () {\n",
            "  println(\"${int_of_u32(u32.of(-1))}\")\n",
            "}\n",
        ),
    );
    for extra in [&[] as &[&str], &["--no-cache"]] {
        let mut args = vec!["run"];
        args.extend_from_slice(extra);
        args.push(file.to_str().expect("temp path is UTF-8"));
        let out = run(&args);
        assert_eq!(
            out.status.code(),
            Some(1),
            "the negative edge traps ({extra:?})"
        );
        in_order(&text(&out.stderr), &[
            "trap: die: u32: -1 is out of range 0..4294967295"
        ]);
    }
}

/// The bare `u32(x)` call form cannot land beside the width's
/// type atom: with no value-namespace `u32` the callee resolves the tag
/// and refuses as a type called as a function, and a value-namespace fn
/// beside the tag would be the DS2014 ambiguity (a call-form name in
/// both namespaces is an error, never a priority search) — so the bare
/// form stays refused and the receiverless member above is the
/// construction surface.
#[test]
fn the_bare_call_form_stays_refused_beside_the_type_atom() {
    let file = script(
        "bare",
        "u32_bare.ds",
        "fn main() -> () {\n  println(\"${int_of_u32(u32(4294967295))}\")\n}\n",
    );
    let out = run(&["check", file.to_str().expect("temp path is UTF-8")]);
    assert_eq!(out.status.code(), Some(1), "the bare form refuses");
    in_order(&text(&out.stderr), &["error[DS2002]: `u32` is a type; \
                                    construct structs with `name { ... \
                                    }` and tags by name"]);
}
