//! The return-union shape refusal probe: a function whose written
//! return union contains a member whose payload is itself a union
//! checks clean and then refuses at lowering — the union payload has no
//! flat word shape this round (a heap-ref application member like
//! `list[int]` lowers; the enum-payload member is the refusing shape).
//! The refusal must report at the user's own code (the tail expression
//! whose value needs the return span), never at the type-keyed sentinel
//! span that renders against the first file's first line — which is the
//! stdlib ambient in the CLI pipeline, nowhere near the user's source.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, lower};
use ds_mod::Hash128;
use ds_parse::parse;

const SOURCE: &str = concat!(
    "tag int\n",
    "tag none\n",
    "enum net_err {\n",
    "  timeout\n",
    "  refused\n",
    "}\n",
    "enum wrap {\n",
    "  carrier(int | none)\n",
    "}\n",
    "fn plain(n: int) -> wrap | net_err {\n",
    "  carrier(n)\n",
    "}\n",
    "fn main() -> () {\n",
    "  const r = plain(3)\n",
    "}\n",
);

#[test]
fn a_return_union_over_a_union_payload_member_refuses_at_the_tail_expression() {
    let mut files = Files::new();
    let id = files.add_source("ret_refusal_probe.ds", SOURCE);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the written union checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let Err(error) = lower(
        "ret_refusal_probe",
        Hash128::compute(SOURCE.as_bytes()),
        &asts,
        &analyzed,
    ) else {
        panic!("the union-payload member has no flat word shape yet")
    };
    let LowerError::Unsupported { span, .. } = &error else {
        panic!("the refusal is Unsupported, got {error:?}")
    };
    assert!(
        error
            .to_string()
            .contains("a struct payload field is outside the M3 runtime contract"),
        "the seam is named: {error}"
    );
    // the tail expression `carrier(n)`, not the sentinel span over the
    // file start
    let needle = "\n  carrier(n)";
    let offset = SOURCE
        .rfind(needle)
        .map(|index| index + "\n  ".len())
        .expect("the tail expression is in the probe source");
    assert_eq!(
        span.start,
        u32::try_from(offset).expect("probe sources are tiny"),
        "the refusal reports at the tail expression, not the sentinel span"
    );
    assert_eq!(
        span.file.as_u32(),
        id.as_u32(),
        "the refusal stays in the probe's own file"
    );
}
