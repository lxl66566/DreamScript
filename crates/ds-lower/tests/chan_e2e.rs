//! The channel send's deinit-carrying payload refusal pin: the staging
//! seam reports its own channel-specific diagnostic instead of the
//! generic native-argument message — the buffer's edge retention owns
//! counts, never claims, so the payload's hook has no door at either
//! wait-ending edge (a buffered element the channel never delivers dies
//! at the send with the sender's claim spent, and a parked sender cut
//! with its span queued fires no hook either way). Both bounds — the
//! buffered channel and the rendezvous — share the one send's staging,
//! and the refusal reports at the payload argument before any run.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::Hash128;
use ds_parse::parse;

const ATOMS: &str = concat!(
    "tag int\n",
    "tag string\n",
    "trait Copyable { }\n",
    "struct chan[t]\n",
    "extern fn println (msg: string) -> ()\n",
    "extern fn chan_new[t] (cap: int) -> chan[t]\n",
    "extern fn chan_send[t] (ch: chan[t], x: t) -> ()\n",
    "extern fn chan_recv[t] (ch: chan[t]) -> t\n",
    "\n",
    "impl[t] chan[t] {\n",
    "  fn send (self, x: t) -> ()\n",
    "  fn recv (self) -> t\n",
    "}\n",
    "\n",
    "struct held: ~Copyable {\n",
    "  id: int\n",
    "  deinit { println(\"freed ${self.id}\") }\n",
    "}\n",
    "\n",
);

/// One send source over the named bound.
fn send_source(bound: &str) -> String {
    format!(
        "{ATOMS}fn main() -> int {{
  const ch = chan[held]({bound})
  const h = held {{ id: 1 }}
  ch.send(h)
  0
}}
"
    )
}

#[test]
fn a_deinit_carrying_payload_refuses_at_the_send_with_the_channel_diagnostic() {
    for bound in ["1", "0"] {
        let source = send_source(bound);
        let mut files = Files::new();
        let id = files.add_source("chan_refusal_probe.ds", &source);
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
            "chan_refusal_probe",
            Hash128::compute(source.as_bytes()),
            &asts,
            &analyzed,
        ) else {
            panic!("the {bound}-bound send refuses at lowering");
        };
        let ds_lower::LowerError::Unsupported { span, .. } = &error else {
            panic!("the refusal is Unsupported, got {error:?}")
        };
        let report = error.to_string();
        assert!(
            report.contains("a deinit-carrying channel payload"),
            "the refusal names the channel's seam: {report}"
        );
        assert!(
            report.contains("no claim inside the channel's edge retention"),
            "the refusal names the edge-retention seam: {report}"
        );
        assert!(
            !report.contains("a native call borrows staged words"),
            "the generic native-argument message never reports: {report}"
        );
        let send = source.find("ch.send(h)").expect("the send is in the probe");
        let payload = send + "ch.send(".len();
        assert_eq!(
            span.start,
            u32::try_from(payload).expect("the probe is tiny"),
            "the refusal reports at the payload argument, not a sentinel span"
        );
    }
}
