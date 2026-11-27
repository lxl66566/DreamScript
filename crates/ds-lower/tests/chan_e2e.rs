//! The channel's deinit hook fact end to end: a deinit-carrying payload
//! stages with its claim spent at the send — the hook fact (the payload's
//! synthesized `$deinit`, the sending module's index) rides the minted
//! stamp into the channel, beside the buffer's edge counts, owning no
//! count and no ledger row — and fires at whichever door ends the
//! element's wait in a loss. Delivery keeps today's adopt path (the
//! receiver's own registration owns the hook); a buffered element the
//! channel never delivers fires as the buffer's own death door; a parked
//! sender cut at the cancel fires from the lease sweep; a deadline-cut
//! sender fires from the wake's deregistration. The standing seams keep
//! their refusals (the list's element slot, the arena's slot).

use std::fmt::Write as _;

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::Hash128;
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

const ATOMS: &str = concat!(
    "tag int\n",
    "tag string\n",
    "tag oops\n",
    "trait Copyable { }\n",
    "enum net_err {\n",
    "  timeout\n",
    "  refused\n",
    "}\n",
    "struct chan[t]\n",
    "extern fn chan_new[t] (cap: int) -> chan[t]\n",
    "extern fn chan_send[t] (ch: chan[t], move x: t) -> ()\n",
    "extern fn chan_recv[t] (ch: chan[t]) -> t\n",
    "extern fn println (msg: string) -> ()\n",
    "extern fn str_int (x: int) -> string\n",
    "struct list[t]\n",
    "extern fn list_len[t] (xs: list[t]) -> int\n",
    "extern fn list_push[t] (xs: mut list[t], x: t) -> ()\n",
    "struct arena[t]\n",
    "extern fn arena_new[t] () -> arena[t]\n",
    "extern fn arena_insert[t] (a: mut arena[t], x: t) -> int\n",
    "extern fn cancel[t] (e: t) -> ()\n",
    "\n",
    "impl[t] chan[t] {\n",
    "  fn send (self, move x: t) -> ()\n",
    "  fn recv (self) -> t\n",
    "}\n",
    "\n",
    "struct held: ~Copyable {\n",
    "  id: int\n",
    "  deinit { println(\"dying ${self.id}\") }\n",
    "}\n",
    "\n",
);

/// The declared preamble plus one body.
fn source(body: &str) -> String {
    format!("{ATOMS}{body}")
}

/// Runs one source end to end: parse -> check -> lower -> install -> run,
/// asserting the check and the lower are clean so failures are the VM's.
fn run_source(name: &str, src: &str) -> (Vm, Result<FrameAnswer, ds_vm::Trap>) {
    let mut files = Files::new();
    let id = files.add_source("chan_probe.ds", src);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the probe checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lin = ds_check::linearity_check(
        &asts,
        &analyzed.package,
        &analyzed.resolutions,
        &analyzed.decls,
        &analyzed.checked,
    );
    assert!(
        lin.diags.is_empty(),
        "the probe walks linear: {}",
        lin.diags.iter().fold(String::new(), |mut out, diag| {
            let _ = writeln!(out, "{diag:?}");
            out
        })
    );
    let lowered = lower(name, Hash128::compute(src.as_bytes()), &asts, &analyzed)
        .expect("the deinit-carrying send lowers");
    let bytes = ds_mod::write(lowered.module());
    let module = ds_mod::read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    let result = vm.run(name, "main", &[]);
    (vm, result)
}

/// One acceptance: the source runs to the expected unit result, prints
/// the golden bytes, and drains the heap (the fired hooks' counts
/// release through the fire doors, the dec-to-alloc pairing balanced).
fn acceptance(name: &str, src: &str, stdout: &str) {
    let (vm, got) = run_source(name, src);
    assert_eq!(got, Ok(FrameAnswer::one(Value::I64(0))), "the run result");
    assert_eq!(vm.captured_output(), stdout.as_bytes(), "the golden stdout");
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// One refusal probe: the source must check clean, and lowering must
/// report the named seam.
fn refused(src: &str, needle: &str) {
    let mut files = Files::new();
    let id = files.add_source("refusal_probe.ds", src);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the probe checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let Err(error) = lower(
        "refusal_probe",
        Hash128::compute(src.as_bytes()),
        &asts,
        &analyzed,
    ) else {
        panic!("the probe refuses at lowering")
    };
    let ds_lower::LowerError::Unsupported { .. } = &error else {
        panic!("the refusal is Unsupported, got {error:?}")
    };
    assert!(
        error.to_string().contains(needle),
        "the refusal names the seam: {error}"
    );
}

/// Delivery: the buffered hand-off answers the receiving binding, whose
/// own registration owns the hook — the deinit fires exactly once, at
/// the receiver's death. The sender's claim spent at the send (the
/// binding's scope end fires nothing), so no second fire ever rides the
/// send path.
#[test]
fn a_delivered_payload_fires_at_the_receivers_death() {
    acceptance(
        "chan_delivered",
        &source(
            r#"
fn main() -> int {
  const ch = chan[held](1)
  const h = held { id: 1 }
  ch.send(h)
  const got = ch.recv()
  println("got ${got.id}")
  0
}
"#,
        ),
        "got 1\ndying 1\n",
    );
}

/// Buffered delivery beside a never-delivered twin: the delivered
/// element's fact row pops with it (the receiver's registration owns
/// the hook), the unmatched element's fact fires at the channel's own
/// death — one fire each, no second fire riding the buffer.
#[test]
fn a_buffered_element_delivers_and_the_unmatched_one_fires_at_the_buffers_death() {
    acceptance(
        "chan_buffered",
        &source(
            r#"
fn main() -> int {
  const ch = chan[held](2)
  const a = held { id: 1 }
  const b = held { id: 2 }
  ch.send(a)
  ch.send(b)
  const got = ch.recv()
  println("got ${got.id}")
  0
}
"#,
        ),
        "dying 2\ngot 1\ndying 1\n",
    );
}

/// A buffered element the channel never delivers fires at the channel's
/// own death: the channel binding's last use is the send, so the
/// channel's death door runs at the send's flush and the fire drains at
/// the very next call boundary — the sender's claim spent at the send,
/// the buffer holding only its edge count, the fact firing before those
/// counts release.
#[test]
fn a_never_delivered_element_fires_at_the_channels_death() {
    acceptance(
        "chan_never_delivered",
        &source(
            r#"
fn main() -> int {
  const ch = chan[held](1)
  const h = held { id: 3 }
  ch.send(h)
  println("sent")
  0
}
"#,
        ),
        "dying 3\nsent\n",
    );
}

/// A parked sender cut at the cancel fires the kept hook from the lease
/// sweep: the queued span is the innermost value's death, its hook
/// reading objects the drain has not released yet. The canceller's
/// failure unions out through main's return, so the acceptance reads
/// the golden stdout and the heap drain alone.
#[test]
fn a_cut_parked_sender_fires_the_kept_hook() {
    let (vm, got) = run_source(
        "chan_cut_sender",
        &source(
            r#"
fn main() -> int | net_err {
  const ch = chan[held](0)
  const joined = nursery {
    spawn { cancel(timeout) }
    spawn {
      const h = held { id: 7 }
      ch.send(h)
      0
    }
  }
  println("joined ${list_len(joined)}")
  0
}
"#,
        ),
    );
    // the canceller's failure unions out through main's return: the
    // answer is the failure span (the timeout member's tag plus the
    // payload word), the run clean
    assert!(
        matches!(
            &got,
            Ok(FrameAnswer::Words { head, n: 2 }) if head[0] == Value::Tag(1)
        ),
        "the failure span answers: {got:?}"
    );
    assert_eq!(vm.captured_output(), "dying 7\n".as_bytes());
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// The buffered drop never double-fires: a channel whose element
/// delivered keeps no fact (the row popped with the element), so the
/// channel's own death fires nothing extra.
#[test]
fn a_drained_channel_drops_silently() {
    acceptance(
        "chan_drained",
        &source(
            r#"
fn main() -> int {
  const ch = chan[held](1)
  const h = held { id: 5 }
  ch.send(h)
  const got = ch.recv()
  println("got ${got.id}")
  0
}
"#,
        ),
        "got 5\ndying 5\n",
    );
}

/// The standing seams keep their refusals: the list's element slot and
/// the arena's slot refuse a deinit-carrying element at their own
/// diagnostics (their edge retention owns counts, never claims — the
/// per-span deinit-claim seam over collections stays queued).
#[test]
fn the_collection_element_refusals_stay_pinned() {
    refused(
        &source(
            r"
fn main() -> int {
  const xs = list[held]()
  const h = held { id: 1 }
  list_push(xs, h)
  0
}
",
        ),
        "a deinit-carrying element's hook has no claim inside the list's edge retention",
    );
    refused(
        &source(
            r"
fn main() -> int {
  const cells = arena[held]()
  const h = held { id: 1 }
  arena_insert(cells, h)
  0
}
",
        ),
        "a deinit-carrying arena element",
    );
}
