//! The linear-use refusal probe: a package can check clean under the
//! bidirectional pass while the affine walk reports violations (a
//! `~Copyable` value moved into a call and used again). Lowering derives
//! its move and deinit placement from that walk's facts, which hold only
//! over a clean walk, so the lowerer refuses the package exactly as it
//! refuses an unchecked one instead of emitting code whose deinit would
//! fire twice at runtime — the backstop for embedders that lower without
//! a CLI gate in front.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, lower};
use ds_mod::Hash128;
use ds_parse::parse;

const SOURCE: &str = concat!(
    "tag int\n",
    "trait Copyable { }\n",
    "struct res: ~Copyable { n: int }\n",
    "fn take (move r: res) -> int { r.n }\n",
    "fn main() -> () {\n",
    "  const r = res { n: 5 }\n",
    "  const a = take(r)\n",
    "  const b = take(r)\n",
    "}\n",
);

#[test]
fn lowering_refuses_a_package_the_affine_walk_reports() {
    let mut files = Files::new();
    let id = files.add_source("linear_refusal_probe.ds", SOURCE);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the moved-then-used probe checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let Err(error) = lower(
        "linear_refusal_probe",
        Hash128::compute(SOURCE.as_bytes()),
        &asts,
        &analyzed,
    ) else {
        panic!("a violating affine walk must not lower");
    };
    let LowerError::NotLinear { count } = &error else {
        panic!("the refusal is NotLinear, got {error:?}");
    };
    assert_eq!(*count, 1, "one DS3001 per moved-then-used pair");
    assert_eq!(
        error.to_string(),
        "the package carries 1 linear-use violations; lower only linearly clean packages"
    );
}
