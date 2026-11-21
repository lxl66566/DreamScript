//! The process face of the CLI process surface (ruled 2026-11-21, H5):
//! the `proc` capability's ops — string argv, environment-variable
//! read, process spawn, and the `exit(code)` host member's seam —
//! served by the platform handler the host installs. Every op is total
//! at the type level: answers ride union spans (`args` is total by
//! contract, `env` and the spawn read answer the checked/fallible
//! spans), and the only trap the face raises is the deliberate one —
//! `exit`'s code riding out like `die`'s message, the host's exit-code
//! policy above the engine.

use ds_abi::SlotType;

use crate::{
    native::{NativeResult, OutputSink, PLATFORM_IO_OK, PLATFORM_NONE, i64_arg, io_err, ref_arg},
    object::Heaps,
    value::Value,
    vm::Trap,
};

/// `args() -> list[string]`: the process arguments the host staged on
/// the sink (`ds run`'s `--args` words, verbatim). Total by contract —
/// a host staging nothing answers the empty list.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn args_fn(
    heaps: &mut Heaps,
    out: &mut OutputSink,
    _args: &[Value],
) -> Result<NativeResult, Trap> {
    let mut elems = Vec::with_capacity(out.argv().len());
    for arg in out.argv() {
        elems.push(heaps.new_str(arg.clone()));
    }
    // each fresh element's birth count becomes the list's edge, the
    // fs_list precedent
    for elem in &elems {
        if let Value::Ref(word) = *elem {
            heaps.release_claim(word);
        }
    }
    Ok(NativeResult::Word(heaps.new_list(elems)))
}

/// `env(name) -> string | none`: the process environment variable's
/// value, the checked ok/none span — an unset variable (or one whose
/// value is not valid Unicode) answers the none member, never a trap.
pub(crate) fn env_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let name = heaps.str_at(ref_arg(args, 0)?)?.to_owned();
    Ok(match std::env::var(&name) {
        Ok(value) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: heaps.new_str(value),
        },
        Err(_) => NativeResult::Tag {
            tag: PLATFORM_NONE,
            payload: Value::I64(0),
        },
    })
}

/// `proc_run(cmd, args) -> int | io_err`: spawn one child process over
/// the argument list, inheriting the parent's standard streams, and
/// answer its exit status — a spawn failure answers the io failure
/// span, and a child ended without a status code (a signal on Unix)
/// answers -1. Output capture stays out of the face: the child
/// inherits, so its output lands exactly where the parent's would.
pub(crate) fn proc_run_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let cmd = heaps.str_at(ref_arg(args, 0)?)?.to_owned();
    let elems = heaps.list_at(ref_arg(args, 1)?)?.0.to_vec();
    let mut child_args = Vec::with_capacity(elems.len());
    for elem in elems {
        let Value::Ref(word) = elem else {
            return Err(Trap::SlotTypeMismatch {
                slot: 1,
                expected: SlotType::Ref,
                actual: elem.slot_type(),
            });
        };
        child_args.push(heaps.str_at(word)?.to_owned());
    }
    let status = std::process::Command::new(&cmd).args(&child_args).status();
    Ok(match status {
        Ok(exit) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::I64(i64::from(exit.code().unwrap_or(-1))),
        },
        Err(err) => io_err(heaps, "proc_run", &cmd, &err),
    })
}

/// The `exit(code)` host member's seam: the deliberate process status
/// rides the trap out to the drive, the CLI host's exit-code policy
/// above it — the engine decides none (the die split's own rule). A
/// negative status traps (ruled 2026-11-22, the trapping-arithmetic
/// precedent: a deliberate failure never reads as clean success), the
/// message riding the Die trap out like the u32 range failure's own
/// note; a wider status wraps to its low byte, POSIX process
/// semantics. Never answers.
pub(crate) fn exit_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let code = i64_arg(args, 0)?;
    if code < 0 {
        return Err(Trap::Die {
            message: format!("exit: {code} is out of range 0..=255"),
        });
    }
    Err(Trap::Exit { code: code & 0xff })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        claims::ClaimTable,
        fiber::{FIBER_ROOT, Fiber},
        object::Objects,
    };

    /// `args()` answers the sink's staged arguments as a list of
    /// strings, empty when the host staged nothing.
    #[test]
    fn args_answers_the_staged_argv() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        out.set_argv(vec!["one".to_owned(), "two words".to_owned()]);
        let result = args_fn(&mut heaps, &mut out, &[]).expect("args is total");
        let word = result.word().expect("args answers the list word");
        let Value::Ref(word) = word else {
            panic!("args answers the list word: {word:?}");
        };
        let texts: Vec<String> = heaps
            .list_at(word)
            .expect("the list reads back")
            .0
            .to_vec()
            .iter()
            .map(|elem| {
                let Value::Ref(word) = *elem else {
                    panic!("an argv element is a string word: {elem:?}");
                };
                heaps.str_at(word).expect("the element reads").to_owned()
            })
            .collect();
        assert_eq!(texts, ["one", "two words"]);

        let empty = args_fn(&mut heaps, &mut OutputSink::captured(), &[])
            .expect("an unstaged host answers the empty list");
        let Value::Ref(word) = empty.word().expect("the list word") else {
            unreachable!()
        };
        let (elems, _) = heaps.list_at(word).expect("the empty list reads");
        assert!(elems.is_empty(), "no staged arguments, no elements");
    }

    /// `env(name)` answers the variable's value under the ok member and
    /// none for an unset name.
    #[test]
    fn env_reads_the_process_environment() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        let name = "DS_PROC_TEST_ENV";
        // test-local like the CLI cache roots: the runner gives each
        // test its own process, and no other unit test reads the name
        unsafe {
            std::env::set_var(name, "staged");
        }
        let set_name = heaps.new_str(name.to_owned());
        let set = env_fn(&mut heaps, &mut out, &[set_name]).expect("env is total");
        let NativeResult::Tag { tag, payload } = set else {
            panic!("env answers the checked span: {set:?}");
        };
        assert_eq!(tag, PLATFORM_IO_OK, "a set variable answers ok");
        let Value::Ref(word) = payload else {
            panic!("the ok payload is the value string: {payload:?}");
        };
        assert_eq!(heaps.str_at(word).expect("the payload reads"), "staged");

        let unset_name = heaps.new_str("DS_PROC_TEST_ENV_UNSET".to_owned());
        let unset = env_fn(&mut heaps, &mut out, &[unset_name]).expect("env is total");
        assert_eq!(
            unset,
            NativeResult::Tag {
                tag: PLATFORM_NONE,
                payload: Value::I64(0)
            },
            "an unset variable answers the none member"
        );
        unsafe {
            std::env::remove_var(name);
        }
    }

    /// `proc_run` answers the child's exit status under the ok member
    /// and the io failure span when the program cannot spawn.
    #[test]
    fn proc_run_answers_the_child_status() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        // the child is this test binary listing itself: spawnable on
        // every platform, exits 0, touches no shared state
        let exe = std::env::current_exe().expect("the test binary's path");
        let argv = {
            let flag = heaps.new_str("--list".to_owned());
            let list = heaps.new_list(vec![flag]);
            if let Value::Ref(word) = flag {
                heaps.release_claim(word);
            }
            list
        };
        let program = heaps.new_str(exe.to_string_lossy().into_owned());
        let result =
            proc_run_fn(&mut heaps, &mut out, &[program, argv]).expect("proc_run is total");
        assert_eq!(
            result,
            NativeResult::Tag {
                tag: PLATFORM_IO_OK,
                payload: Value::I64(0)
            },
            "the listed child exits 0"
        );

        let missing = heaps.new_str("ds-proc-test-no-such-program".to_owned());
        let no_args = heaps.new_list(Vec::new());
        let refused =
            proc_run_fn(&mut heaps, &mut out, &[missing, no_args]).expect("proc_run is total");
        let NativeResult::Tag { tag, .. } = refused else {
            panic!("a spawn failure answers the failure span: {refused:?}");
        };
        assert_eq!(
            tag,
            crate::native::PLATFORM_IO_NOT_FOUND,
            "a missing program answers the not-found io class"
        );
    }

    /// `exit(code)` never answers: the wrapped status rides the trap
    /// out, a negative status trapping like the u32 range failure, the
    /// host's policy above the engine.
    #[test]
    fn exit_traps_negative_and_wraps_wide_statuses() {
        let mut objects = Objects::new(FIBER_ROOT);
        let mut fibers: Vec<Fiber> = Vec::new();
        let mut claims = vec![ClaimTable::default()];
        let mut heaps = Heaps::new(FIBER_ROOT, &mut objects, &mut fibers, &mut claims, true);
        let mut out = OutputSink::captured();
        assert_eq!(
            exit_fn(&mut heaps, &mut out, &[Value::I64(3)]),
            Err(Trap::Exit { code: 3 })
        );
        assert_eq!(
            exit_fn(&mut heaps, &mut out, &[Value::I64(255)]),
            Err(Trap::Exit { code: 255 })
        );
        assert_eq!(
            exit_fn(&mut heaps, &mut out, &[Value::I64(256)]),
            Err(Trap::Exit { code: 0 }),
            "a wide status wraps to its low byte"
        );
        assert_eq!(
            exit_fn(&mut heaps, &mut out, &[Value::I64(1000)]),
            Err(Trap::Exit { code: 232 }),
            "a wide status wraps to its low byte"
        );
        assert_eq!(
            exit_fn(&mut heaps, &mut out, &[Value::I64(-1)]),
            Err(Trap::Die {
                message: "exit: -1 is out of range 0..=255".to_owned()
            }),
            "a negative status traps, never reading as clean success"
        );
    }
}
