//! The golden negative suite: every diagnostic class the M2 checker
//! rejects, driven through `ds check` on one minimal fixture per class,
//! pinned byte-identical in both renderings. A renderer or message
//! change is a one-line golden diff, never a silent drift.
//!
//! Coverage decisions: DS2001 is exempt (ordering noise confined to the
//! get_port tension per the corpus README); DS3002 has no triggering
//! surface yet (every M2 binding form carries an initializer, so the
//! uninitialized state never reaches a use; the lattice base is pinned by
//! a unit test inside the affine pass instead). DS2012 is pinned here on
//! both of its surface routes: the tuple expression and the tuple type
//! annotation. The fn-only round adds its retirement classes: DS1013
//! (a lambda named at the top level), DS2015 (`self` as a type name), and
//! DS2016 (the annotated receiver). The namespace round adds the second
//! DS2014 route: the call-form collision between the type and value
//! namespaces [F9]. The script-entry round adds DS1014 (a top-level
//! statement outside the entry file); DS1015, the script-mode collision
//! with a written `main`, fires only through `parse_script` and is pinned
//! in ds-parse's unit suite instead. The mut-window round adds DS3006
//! (a `const` binding fed to a `mut` window parameter [D2]). The
//! signature-agreement round adds DS2018 (an impl member diverging from
//! its trait member's registered signature); the trait-residuals round
//! adds its inferred-return route and the generic-head route of DS2006
//! (the instantiation probe's witness).

use std::{
    env, fs,
    process::{Command, Output},
};

/// One pinned case: the fixture stem and its two goldens.
struct Case {
    stem: &'static str,
    human: &'static str,
    json: &'static str,
}

/// The golden directory, relative to this crate's manifest (the tests'
/// working directory).
const GOLDENS: &str = "tests/goldens";
/// The negative fixtures, relative to this crate's manifest.
const FIXTURES: &str = "tests/fixtures/negative";

const CASES: &[Case] = &[
    Case {
        stem: "ds1008_unclosed_delimiter",
        human: include_str!("goldens/ds1008_unclosed_delimiter.human"),
        json: include_str!("goldens/ds1008_unclosed_delimiter.json"),
    },
    Case {
        stem: "ds1013_named_const_lambda",
        human: include_str!("goldens/ds1013_named_const_lambda.human"),
        json: include_str!("goldens/ds1013_named_const_lambda.json"),
    },
    Case {
        stem: "ds1014_top_level_statement",
        human: include_str!("goldens/ds1014_top_level_statement.human"),
        json: include_str!("goldens/ds1014_top_level_statement.json"),
    },
    Case {
        stem: "ds2003_non_exhaustive_match",
        human: include_str!("goldens/ds2003_non_exhaustive_match.human"),
        json: include_str!("goldens/ds2003_non_exhaustive_match.json"),
    },
    Case {
        stem: "ds2004_unresolved_dot_callee",
        human: include_str!("goldens/ds2004_unresolved_dot_callee.human"),
        json: include_str!("goldens/ds2004_unresolved_dot_callee.json"),
    },
    Case {
        stem: "ds2006_instance_overlap",
        human: include_str!("goldens/ds2006_instance_overlap.human"),
        json: include_str!("goldens/ds2006_instance_overlap.json"),
    },
    Case {
        stem: "ds2006_generic_head_overlap",
        human: include_str!("goldens/ds2006_generic_head_overlap.human"),
        json: include_str!("goldens/ds2006_generic_head_overlap.json"),
    },
    Case {
        stem: "ds2008_propagation_containment",
        human: include_str!("goldens/ds2008_propagation_containment.human"),
        json: include_str!("goldens/ds2008_propagation_containment.json"),
    },
    Case {
        stem: "ds2012_tuple_distribution_cap",
        human: include_str!("goldens/ds2012_tuple_distribution_cap.human"),
        json: include_str!("goldens/ds2012_tuple_distribution_cap.json"),
    },
    Case {
        stem: "ds2013_unsatisfied_constraint",
        human: include_str!("goldens/ds2013_unsatisfied_constraint.human"),
        json: include_str!("goldens/ds2013_unsatisfied_constraint.json"),
    },
    Case {
        stem: "ds2014_ambiguous_call",
        human: include_str!("goldens/ds2014_ambiguous_call.human"),
        json: include_str!("goldens/ds2014_ambiguous_call.json"),
    },
    Case {
        stem: "ds2014_namespace_call_collision",
        human: include_str!("goldens/ds2014_namespace_call_collision.human"),
        json: include_str!("goldens/ds2014_namespace_call_collision.json"),
    },
    Case {
        stem: "ds2015_lowercase_self_type",
        human: include_str!("goldens/ds2015_lowercase_self_type.human"),
        json: include_str!("goldens/ds2015_lowercase_self_type.json"),
    },
    Case {
        stem: "ds2016_annotated_receiver",
        human: include_str!("goldens/ds2016_annotated_receiver.human"),
        json: include_str!("goldens/ds2016_annotated_receiver.json"),
    },
    Case {
        stem: "ds2018_impl_member_signature",
        human: include_str!("goldens/ds2018_impl_member_signature.human"),
        json: include_str!("goldens/ds2018_impl_member_signature.json"),
    },
    Case {
        stem: "ds2018_inferred_impl_return",
        human: include_str!("goldens/ds2018_inferred_impl_return.human"),
        json: include_str!("goldens/ds2018_inferred_impl_return.json"),
    },
    Case {
        stem: "ds3001_moved_then_used",
        human: include_str!("goldens/ds3001_moved_then_used.human"),
        json: include_str!("goldens/ds3001_moved_then_used.json"),
    },
    Case {
        stem: "ds3006_mut_window_const_receiver",
        human: include_str!("goldens/ds3006_mut_window_const_receiver.human"),
        json: include_str!("goldens/ds3006_mut_window_const_receiver.json"),
    },
    Case {
        stem: "ds3004_deinit_shape",
        human: include_str!("goldens/ds3004_deinit_shape.human"),
        json: include_str!("goldens/ds3004_deinit_shape.json"),
    },
];

/// The fixtures are self-contained negative packages: checked without the
/// stdlib ambient, whose single namespace would layer unrelated DS2005
/// collisions under each pinned code.
fn ds_check(case: &Case, format: &str) -> Output {
    let fixture = format!("{FIXTURES}/{}.ds", case.stem);
    Command::new(env!("CARGO_BIN_EXE_ds"))
        .args(["check", "--no-ambient", "--error-format", format, &fixture])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("the ds binary builds with the test")
}

/// Every fixture is a plain file (no `// items:` header), so its
/// diagnostics are all fatal: exit 1, nothing on stdout, and the human
/// rendering byte-identical to the golden.
#[test]
fn human_rendering_matches_the_golden() {
    for case in CASES {
        let out = ds_check(case, "human");
        assert_eq!(out.status.code(), Some(1), "{}: exit", case.stem);
        assert_eq!(out.stdout, [] as [u8; 0], "{}: stdout", case.stem);
        assert_eq!(
            String::from_utf8(out.stderr).unwrap(),
            case.human,
            "{}: human rendering",
            case.stem
        );
    }
}

/// The JSONL rendering: one object per diagnostic, byte-identical to the
/// golden, so a schema or field-order change is a diff here.
#[test]
fn json_rendering_matches_the_golden() {
    for case in CASES {
        let out = ds_check(case, "json");
        assert_eq!(out.status.code(), Some(1), "{}: exit", case.stem);
        assert_eq!(out.stdout, [] as [u8; 0], "{}: stdout", case.stem);
        assert_eq!(
            String::from_utf8(out.stderr).unwrap(),
            case.json,
            "{}: json rendering",
            case.stem
        );
    }
}

/// Fixture/golden parity: every `.ds` file under the negative fixtures
/// has a case with both goldens, and every golden file belongs to a
/// case, so a fixture without a golden fails here instead of passing
/// silently.
#[test]
fn every_fixture_and_golden_belongs_to_a_case() {
    let mut fixtures: Vec<String> =
        fs::read_dir(format!("{}/{FIXTURES}", env!("CARGO_MANIFEST_DIR")))
            .expect("the negative fixtures directory exists")
            .filter_map(|entry| {
                let path = entry.expect("directory entry reads").path();
                (path.extension().is_some_and(|ext| ext == "ds"))
                    .then(|| path.file_stem().unwrap().to_string_lossy().into_owned())
            })
            .collect();
    fixtures.sort();

    let mut goldens: Vec<String> =
        fs::read_dir(format!("{}/{GOLDENS}", env!("CARGO_MANIFEST_DIR")))
            .expect("the goldens directory exists")
            .filter_map(|entry| {
                let path = entry.expect("directory entry reads").path();
                (path
                    .extension()
                    .is_some_and(|ext| ext == "human" || ext == "json"))
                .then(|| path.file_stem().unwrap().to_string_lossy().into_owned())
            })
            .collect();
    goldens.sort();
    goldens.dedup();

    let mut cases: Vec<&str> = CASES.iter().map(|c| c.stem).collect();
    cases.sort_unstable();

    assert_eq!(
        fixtures.iter().map(String::as_str).collect::<Vec<_>>(),
        cases,
        "every fixture has a case"
    );
    assert_eq!(
        goldens.iter().map(String::as_str).collect::<Vec<_>>(),
        cases,
        "every golden belongs to a case"
    );
}

/// Drift guard: the diagnostic code a golden pins appears in its
/// fixture's file name, so the suite's organization is checkable from
/// the goldens themselves. Every diagnostic in a multi-diagnostic
/// golden must carry the file-name code.
#[test]
fn every_golden_code_names_its_fixture() {
    for case in CASES {
        let prefix = case.stem.split('_').next().unwrap_or("");
        for line in case.human.lines().filter(|l| l.starts_with("error[")) {
            let code = line
                .strip_prefix("error[")
                .and_then(|rest| rest.split(']').next())
                .unwrap_or_default()
                .to_ascii_lowercase();
            assert_eq!(code, prefix, "{}: human code {}", case.stem, line);
        }
        for line in case.json.lines() {
            let code = line
                .split_once(r#""code":""#)
                .and_then(|(_, rest)| rest.split('"').next())
                .unwrap_or_default()
                .to_ascii_lowercase();
            assert_eq!(code, prefix, "{}: json code", case.stem);
        }
    }
}
