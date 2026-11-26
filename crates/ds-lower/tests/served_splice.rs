//! The served-body splice the checked-in ambient dsmod rides: a served
//! function list whose names reproduce the plan's prefix installs
//! verbatim and answers the fresh lowering byte for byte, and any
//! disagreement refuses with the splice error the caller falls back
//! from. The probe is package-independent — the ambient is one consumer
//! of the seam, not the seam itself.

use ds_check::analyze;
use ds_diag::Files;
use ds_lower::{ComptimeValues, LowerError, Served, lower, lower_with_served};
use ds_mod::{Hash128, write};
use ds_parse::parse;

/// Impl members first, then `main`: the planned prefix a served artifact
/// splices over, exactly the ambient's shape in the run path's combined
/// package. The atoms are declared here (the probe runs ambient-free).
const SOURCE: &str = "tag int\n\ntag bool\n\ntrait twice {\nfn twice(self) -> int\n}\n\nimpl \
                      twice for int {\nfn twice(self) -> int { self + self }\n}\n\nimpl twice for \
                      bool {\nfn twice(self) -> int { if self { 2 } else { 0 } }\n}\n\nfn main() \
                      -> int {\nconst x = 21\nx.twice()\n}\n";

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

/// A served prefix: the freshly lowered function records a prefix of
/// the plan would serve, wrapped with the member-tag rows the bodies
/// minted (the rows ride the merged lowering's tag table, in mint
/// order).
fn served(analyzed: &ds_check::Analyzed, count: usize) -> Served {
    let lowered = lower(
        "splice",
        Hash128::compute(SOURCE.as_bytes()),
        &analyzed.asts,
        analyzed,
    )
    .expect("the probe lowers again");
    let module = lowered.module();
    let mut spans = module.pc_spans().cloned();
    if let Some(table) = &mut spans {
        // the served table names the served prefix only
        table.funcs.retain(|row| (row.func as usize) < count);
    }
    Served {
        functions: module.functions()[..count].to_vec(),
        tags: module.tags().to_vec(),
        spans,
    }
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
            served(&analyzed, count),
            None,
            &ComptimeValues::default(),
        )
        .unwrap_or_else(|error| panic!("the splice lowers: {error:?}"));
        assert_eq!(
            write(spliced.module()),
            fresh_bytes,
            "a {count}-body serve must answer the fresh module"
        );
        assert_eq!(
            spliced.spliced(),
            count,
            "the install count names the served prefix's length"
        );
    }
}

/// A served prefix that disagrees with the plan — bodies swapped, so
/// the prefix names part — refuses with the splice error; a serve
/// longer than the plan refuses the same way.
#[test]
fn a_disagreeing_prefix_refuses() {
    let (analyzed, _) = fresh();
    let mut swapped = served(&analyzed, 2);
    swapped.functions.swap(0, 1);
    let Err(error) = lower_with_served(
        "splice",
        Hash128::compute(SOURCE.as_bytes()),
        &analyzed.asts,
        &analyzed,
        swapped,
        None,
        &ComptimeValues::default(),
    ) else {
        panic!("the swapped prefix must refuse")
    };
    assert!(
        matches!(error, LowerError::ServedDrift { .. }),
        "the refusal is the splice error, not {error:?}"
    );

    let mut long = served(&analyzed, 2);
    long.functions.extend(long.functions.clone());
    let Err(error) = lower_with_served(
        "splice",
        Hash128::compute(SOURCE.as_bytes()),
        &analyzed.asts,
        &analyzed,
        long,
        None,
        &ComptimeValues::default(),
    ) else {
        panic!("the overlong prefix must refuse")
    };
    assert!(matches!(error, LowerError::ServedDrift { .. }));
}

/// The probe's own union answer: `kind`'s `is` tests mint the `int` and
/// `string` member rows, so a served row planted ahead of them pins
/// that an unresolved name seeds, rides the module's table verbatim,
/// and the bodies lowered here continue minting after it.
const NARROW: &str = "tag int\n\ntag string\n\nfn kind(v: int | string) -> int { if v is int { 1 \
                      } else { 2 } }\n\nfn main() -> int {\nconst n: int | string = \
                      7\nkind(n)\n}\n";

/// A served row whose name resolves to no atom of this package — the
/// shape a served artifact's structural-atom rows take in a compile
/// whose analysis never mints them — seeds instead of refusing: the
/// row lands in the module's table at its id, the splice's install
/// count is observable, and the bodies lowered here mint after it.
#[test]
fn a_row_without_an_atom_rides_the_table() {
    let mut files = Files::new();
    let id = files.add_source("narrow.ds", NARROW);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(analyzed.checked.diags.is_empty(), "the probe checks clean");

    // the fresh reference: the rows this package itself mints
    let fresh = lower(
        "narrow",
        Hash128::compute(NARROW.as_bytes()),
        &asts,
        &analyzed,
    )
    .unwrap_or_else(|error| panic!("the fresh probe lowers: {error:?}"));
    assert_eq!(fresh.spliced(), 0, "the fresh lowering installs nothing");
    let fresh_rows = fresh.module().tags().to_vec();

    // the serve: one unresolved row planted as the prefix, the fresh
    // rows following it shifted by one — the artifact's own shape
    let mut served = served(&analyzed, 0);
    served.tags.insert(0, ds_mod::MemberTag {
        decl: String::new(),
        name: "list[byte]".to_owned(),
        id: 0,
    });
    for row in served.tags.iter_mut().skip(1) {
        row.id += 1;
    }
    let spliced = lower_with_served(
        "narrow",
        Hash128::compute(NARROW.as_bytes()),
        &asts,
        &analyzed,
        served,
        None,
        &ComptimeValues::default(),
    )
    .unwrap_or_else(|error| panic!("the splice lowers: {error:?}"));
    assert_eq!(spliced.spliced(), 0, "the empty prefix installs no records");
    let mut expected = vec![ds_mod::MemberTag {
        decl: String::new(),
        name: "list[byte]".to_owned(),
        id: 0,
    }];
    expected.extend(fresh_rows.iter().map(|row| ds_mod::MemberTag {
        decl: row.decl.clone(),
        name: row.name.clone(),
        id: row.id + 1,
    }));
    assert_eq!(
        spliced.module().tags(),
        expected.as_slice(),
        "the unresolved row rides the table and the minted rows continue after it"
    );
}
