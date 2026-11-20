//! The bound extern's C-ABI seam (H5): a call into a bound artifact
//! resolves through the package graph's artifacts fold — the exported
//! symbol tables the lowering validates against — with every out-of-slice
//! shape refusing at the seam and naming it: a missing artifact entry,
//! a symbol the library does not export, and every signature outside the
//! scalar int/bool/float first slice (string and bytes words, a unit
//! return, generics, an inferred parameter, and the arity cap). The clean
//! shape lowers to the pool constant the VM's bound tier dispatches.

use std::sync::Arc;

use ds_abi::SlotType;
use ds_check::{Analyzed, ImportGraph};
use ds_diag::{Files, human};
use ds_lower::{LowerError, lower};
use ds_meta::analyze_owned_linked;
use ds_mod::{Constant, Hash128};
use ds_parse::Ast;

/// The fixture artifact's exported symbols, enumeration order.
const MATH_SYMBOLS: &[&str] = &["ds_add", "ds_clamp", "ds_scale"];

/// Parses and checks the probe against the graph's artifact fold,
/// asserting the source checks clean first (the refusals under test are
/// the lowerer's seam, never the checker's).
fn checked(source: &str, artifacts: &[(&str, &[&str])]) -> (Vec<Arc<Ast>>, Analyzed) {
    let mut files = Files::new();
    let id = files.add_source("probe.ds", source);
    let asts = vec![Arc::new(
        ds_parse::parse(&files, id).expect("the probe parses"),
    )];
    let mut imports = ImportGraph::new();
    for (name, symbols) in artifacts {
        imports.add_artifact(
            name,
            symbols.iter().map(|symbol| (*symbol).to_owned()).collect(),
        );
    }
    let analyzed = analyze_owned_linked(&files, asts.clone(), &[false], &imports);
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
    (asts, analyzed)
}

/// Lowers the probe, answering the error.
fn lower_probe(
    source: &str,
    artifacts: &[(&str, &[&str])],
) -> Result<ds_lower::Lowered, LowerError> {
    let (asts, analyzed) = checked(source, artifacts);
    lower(
        "probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
}

/// One probe's refusal: the error names the extern and the reason names
/// the seam.
fn refusal(source: &str, artifacts: &[(&str, &[&str])], needle: &str) {
    let error = lower_probe(source, artifacts)
        .map(|_| panic!("the probe refuses"))
        .unwrap_err();
    let LowerError::ForeignBind { name, reason, .. } = &error else {
        panic!("the refusal rides the C-ABI seam: {error}");
    };
    assert_eq!(name, "ds_add", "the refusal names the extern");
    assert!(
        reason.contains(needle),
        "the refusal names the seam (got: {reason})"
    );
}

/// The clean shape lowers: the pool constant addresses the symbol by its
/// enumeration index with the return word's scalar kind.
#[test]
fn a_scalar_signature_lowers_to_the_bound_constant() {
    let source = concat!(
        "tag int\ntag bool\ntag float\ntag byte\ntag string\ntag bytes\n\n",
        "extern \"math\" fn ds_add (left: int, right: int) -> int\n",
        "\n",
        "fn main() -> int {\n",
        "ds_add(20, 22)\n",
        "}\n",
    );
    let lowered = lower_probe(source, &[("math", MATH_SYMBOLS)]).expect("the probe lowers");
    let bound = lowered
        .module()
        .functions()
        .iter()
        .flat_map(|function| function.constants().iter())
        .find(|constant| matches!(constant, Constant::Bound { .. }))
        .expect("the call site's pool carries the bound constant");
    assert_eq!(
        bound,
        &Constant::Bound {
            module: "math".to_owned(),
            index: 0,
            ret: SlotType::I64,
        },
        "the constant addresses ds_add at its enumeration index with the scalar return"
    );
}

/// A graph that declares no such artifact entry refuses, naming the
/// entry and the package fold — the graphless and wrong-name shapes
/// alike.
#[test]
fn a_missing_artifact_entry_refuses() {
    let source = concat!(
        "tag int\ntag bool\ntag float\ntag byte\ntag string\ntag bytes\n\n",
        "extern \"math\" fn ds_add (left: int, right: int) -> int\n",
        "\n",
        "fn main() -> int {\n",
        "ds_add(20, 22)\n",
        "}\n",
    );
    refusal(source, &[], "declares no [artifacts] entry `math`");
    refusal(
        source,
        &[("other", MATH_SYMBOLS)],
        "declares no [artifacts] entry `math`",
    );
}

/// A symbol the library does not export refuses, naming the spelling.
#[test]
fn a_missing_symbol_refuses() {
    let source = concat!(
        "tag int\ntag bool\ntag float\ntag byte\ntag string\ntag bytes\n\n",
        "extern \"math\" fn ds_add (left: int, right: int) -> int\n",
        "\n",
        "fn main() -> int {\n",
        "ds_add(20, 22)\n",
        "}\n",
    );
    refusal(
        source,
        &[("math", &["ds_answer", "ds_clamp"])],
        "exports no `ds_add`",
    );
}

/// A string parameter refuses: the first slice marshals scalar words
/// alone, string and bytes riding the next slice.
#[test]
fn a_string_parameter_refuses() {
    let source = concat!(
        "tag int\ntag bool\ntag float\ntag byte\ntag string\ntag bytes\n\n",
        "extern \"math\" fn ds_add (left: string, right: int) -> int\n",
        "\n",
        "fn main() -> int {\n",
        "ds_add(\"x\", 22)\n",
        "}\n",
    );
    refusal(
        source,
        &[("math", MATH_SYMBOLS)],
        "string, bytes, handle, and aggregate marshaling",
    );
}

/// A bytes parameter refuses at the same seam.
#[test]
fn a_bytes_parameter_refuses() {
    let source = concat!(
        "tag int\ntag bool\ntag float\ntag byte\ntag string\ntag bytes\n\n",
        "extern \"math\" fn ds_add (left: bytes, right: int) -> int\n",
        "\n",
        "fn main (b: bytes) -> int {\n",
        "ds_add(b, 22)\n",
        "}\n",
    );
    refusal(
        source,
        &[("math", MATH_SYMBOLS)],
        "string, bytes, handle, and aggregate marshaling",
    );
}

/// A unit return refuses: the slice answers one scalar word.
#[test]
fn a_unit_return_refuses() {
    let source = concat!(
        "tag int\ntag bool\ntag float\ntag byte\ntag string\ntag bytes\n\n",
        "extern \"math\" fn ds_add (left: int, right: int)\n",
        "\n",
        "fn main() {\n",
        "ds_add(20, 22)\n",
        "}\n",
    );
    refusal(
        source,
        &[("math", MATH_SYMBOLS)],
        "the slice answers one scalar word",
    );
}

/// A generic bound extern refuses: a C symbol has no generic body.
#[test]
fn a_generic_signature_refuses() {
    let source = concat!(
        "tag int\ntag bool\ntag float\ntag byte\ntag string\ntag bytes\n\n",
        "extern \"math\" fn ds_add[t] (left: t, right: int) -> int\n",
        "\n",
        "fn main() -> int {\n",
        "ds_add(20, 22)\n",
        "}\n",
    );
    refusal(
        source,
        &[("math", MATH_SYMBOLS)],
        "no generic body; the signature must be scalar and monomorphic",
    );
}

/// A signature wider than the enumerated transmute's arity cap refuses,
/// naming the cap.
#[test]
fn a_wide_signature_refuses() {
    let source = concat!(
        "tag int\ntag bool\ntag float\ntag byte\ntag string\ntag bytes\n\n",
        "extern \"math\" fn ds_add (a: int, b: int, c: int, d: int, e: int) -> int\n",
        "\n",
        "fn main() -> int {\n",
        "ds_add(1, 2, 3, 4, 5)\n",
        "}\n",
    );
    refusal(
        source,
        &[("math", MATH_SYMBOLS)],
        &format!("serves at most {} parameters", ds_ffi::MAX_PARAMS),
    );
}
