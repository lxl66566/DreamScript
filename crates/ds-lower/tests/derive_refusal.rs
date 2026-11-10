//! The derive-attribute honesty probe: a `#[derive(...)]` declaration
//! parses and checks clean (the checker never observes attributes), but
//! the derive engine is M6's [G1] — so lowering refuses with a named
//! boundary instead of silently dropping the attribute. The refusal
//! points at the attribute's own span; a file whose items carry no
//! derive lowers as before.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, lower};
use ds_mod::Hash128;
use ds_parse::parse;

const DERIVED: &str = concat!(
    "tag int\n",
    "#[derive(hash, show, ord)]\n",
    "struct point { x: int, y: int }\n",
    "fn main() -> int { 0 }\n",
);

const PLAIN: &str = concat!(
    "tag int\n",
    "struct point { x: int, y: int }\n",
    "fn main() -> int { 0 }\n",
);

/// A derive attribute checks clean and then refuses at lowering.
#[test]
fn a_derive_attribute_refuses_at_lowering_with_the_engine_named() {
    let mut files = Files::new();
    let id = files.add_source("derive.ds", DERIVED);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the checker accepts attributed declarations: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let Err(err) = lower(
        "derive",
        Hash128::compute(DERIVED.as_bytes()),
        &asts,
        &analyzed,
    ) else {
        panic!("the derive engine does not exist yet")
    };
    assert!(
        matches!(err, LowerError::Unsupported { .. }),
        "an unsupported boundary, not a miscompile: {err}"
    );
    assert_eq!(
        err.to_string(),
        "a derive attribute is outside the M3 runtime contract (the M6 derive engine owns it; \
         until it lands the attribute generates no behavior); the checker accepted it, the uop \
         set does not cover it yet"
    );
    // The refusal points at the attribute itself, not the declaration.
    let span = err
        .span()
        .expect("the refusal carries the attribute's span");
    let rendered = human(
        &ds_diag::Diagnostic::error(ds_diag::ErrorCode::LowerUnsupported, err.to_string())
            .with_primary(span, ""),
        &files,
    );
    assert!(
        rendered.contains("#[derive(hash, show, ord)]"),
        "the span addresses the attribute: {rendered}"
    );
}

/// The refusal is per item: an unattributed package lowers as before.
#[test]
fn an_unattributed_package_lowers_unaffected() {
    let mut files = Files::new();
    let id = files.add_source("plain.ds", PLAIN);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert_eq!(analyzed.checked.diags, [], "the plain package checks clean");
    lower(
        "plain",
        Hash128::compute(PLAIN.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the unattributed package lowers");
}
