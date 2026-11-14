//! The served-body splice the checked-in ambient dsmod rides: a served
//! function list whose names reproduce the plan's prefix installs
//! verbatim and answers the fresh lowering byte for byte, and any
//! disagreement refuses with the splice error the caller falls back
//! from. The probe is package-independent — the ambient is one consumer
//! of the seam, not the seam itself.

use ds_check::analyze;
use ds_diag::Files;
use ds_lower::{ComptimeValues, LowerError, lower, lower_with_served};
use ds_mod::{Hash128, write};
use ds_parse::parse;

/// Impl members first, then `main`: the planned prefix a served artifact
/// splices over, exactly the ambient's shape in the run path's combined
/// package. The atoms are declared here (the probe runs ambient-free).
const SOURCE: &str = "tag int\n\ntag bool\n\ntrait twice {\nfn twice(self) -> int\n}\n\nimpl \
                      twice for int {\nfn twice(self) -> int { self + self }\n}\n\nimpl twice \
                      for bool {\nfn twice(self) -> int { if self { 2 } else { 0 } }\n}\n\nfn \
                      main() -> int {\nconst x = 21\nx.twice()\n}\n";

/// The analyzed probe plus its freshly lowered module bytes.
fn fresh() -> (ds_check::Analyzed, Vec<u8>) {
    let mut files = Files::new();
    let id = files.add_source("splice.ds", SOURCE);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(analyzed.checked.diags.is_empty(), "the probe checks clean");
    let lowered = lower(
        "splice",
        Hash128::compute(SOURCE.as_bytes()),
        &asts,
        &analyzed,
    )
    .unwrap_or_else(|error| panic!("the probe lowers: {error:?}"));
    let bytes = write(lowered.module());
    (analyzed, bytes)
}

/// The freshly lowered prefix's function records: what an artifact
/// generated from this package's own impl members would carry.
fn served_prefix(analyzed: &ds_check::Analyzed, count: usize) -> Vec<ds_mod::Function> {
    let lowered = lower(
        "splice",
        Hash128::compute(SOURCE.as_bytes()),
        &analyzed.asts,
        analyzed,
    )
    .expect("the probe lowers again");
    lowered.module().functions()[..count].to_vec()
}

/// A served prefix that names the plan's own functions answers the fresh
/// module byte for byte — the splice changes where the bodies come
/// from, never what they are. The empty serve answers it too.
#[test]
fn a_matching_prefix_splices_to_the_fresh_module() {
    let (analyzed, fresh_bytes) = fresh();
    for count in [0, 2] {
        let spliced = lower_with_served(
            "splice",
            Hash128::compute(SOURCE.as_bytes()),
            &analyzed.asts,
            &analyzed,
            served_prefix(&analyzed, count),
            &ComptimeValues::default(),
        )
        .unwrap_or_else(|error| panic!("the splice lowers: {error:?}"));
        assert_eq!(
            write(spliced.module()),
            fresh_bytes,
            "a {count}-body serve must answer the fresh module"
        );
    }
}

/// A served prefix that disagrees with the plan — bodies swapped, so
/// the prefix names part — refuses with the splice error; a serve
/// longer than the plan refuses the same way.
#[test]
fn a_disagreeing_prefix_refuses() {
    let (analyzed, _) = fresh();
    let mut swapped = served_prefix(&analyzed, 2);
    swapped.swap(0, 1);
    let Err(error) = lower_with_served(
        "splice",
        Hash128::compute(SOURCE.as_bytes()),
        &analyzed.asts,
        &analyzed,
        swapped,
        &ComptimeValues::default(),
    ) else {
        panic!("the swapped prefix must refuse")
    };
    assert!(
        matches!(error, LowerError::ServedDrift { .. }),
        "the refusal is the splice error, not {error:?}"
    );

    let mut long = served_prefix(&analyzed, 2);
    long.extend(long.clone());
    let Err(error) = lower_with_served(
        "splice",
        Hash128::compute(SOURCE.as_bytes()),
        &analyzed.asts,
        &analyzed,
        long,
        &ComptimeValues::default(),
    ) else {
        panic!("the overlong prefix must refuse")
    };
    assert!(matches!(error, LowerError::ServedDrift { .. }));
}
