//! The generic-impl runtime-evidence seam's honest boundaries: the shapes
//! whose runtime dispatch or runtime word has no contract this round
//! refuse at lowering naming exactly the seam they wait on — transitive
//! evidence, evidence across a nested lambda boundary, an overwrite
//! contract for a type-variable's word, and a checked read whose union
//! payload spans the variable's runtime word.

use ds_check::analyze;
use ds_diag::Files;
use ds_lower::{LowerError, lower};
use ds_mod::Hash128;
use ds_parse::parse;

/// The shared trait story: two monomorphic member bodies and one generic
/// impl over `list[t]` whose body iterates its elements, so a probe can
/// append statements inside the loop or call sites after the impl.
const TRAIT_STORY: &str = concat!(
    "tag int\n",
    "tag string\n",
    "struct list[t]\n",
    "extern fn println (s: string) -> ()\n",
    "extern fn map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]\n",
    "extern fn head[t] (xs: list[t]) -> t | none\n",
    "trait shows {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "impl shows for int {\n",
    "  fn show(self) -> string { \"n\" }\n",
    "}\n",
    "impl shows for string {\n",
    "  fn show(self) -> string { \"s\" }\n",
    "}\n",
    "impl [t: shows] shows for list[t] {\n",
    "  fn show(self) -> string {\n",
    "    var acc = \"l\"\n",
    "    for x in self {\n",
    "      BODY",
    "    }\n",
    "    acc\n",
    "  }\n",
    "}\n",
);

/// Builds a source with `BODY` statements inside the loop.
fn loop_body(body: &str) -> String {
    TRAIT_STORY.replace("BODY", body)
}

/// Checks one source clean and asserts the lowering refuses with the
/// expected seam named.
fn refuses_with(source: &str, expected: &str) {
    let mut files = Files::new();
    let id = files.add_source("refusal.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the surface checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| ds_diag::human(diag, &files))
            .collect::<String>()
    );
    let Err(err) = lower(
        "refusal",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    ) else {
        panic!("the shape must refuse at lowering")
    };
    assert!(
        matches!(err, LowerError::Unsupported { .. }),
        "an unsupported boundary: {err}"
    );
    assert!(
        err.to_string().contains(expected),
        "the refusal names the seam: {err}"
    );
}

/// A nested dispatch whose serving body is itself a generic impl's: the
/// entering site would need to stage the second body's own evidence
/// demands, which its substitution cannot know.
#[test]
fn transitive_dispatch_into_another_generic_impl_refuses() {
    let source = format!(
        "{}\n{}",
        loop_body("      acc = \"${acc}${x.show()}\""),
        concat!(
            "fn main() -> int {\n",
            "  println([[1]].show())\n",
            "  0\n",
            "}\n",
        )
    );
    refuses_with(
        &source,
        "transitive runtime evidence staging is the next seam",
    );
}

/// A runtime-dispatched member call inside a nested lambda: the lambda
/// value is its own frame, and no evidence slot reaches inside it.
#[test]
fn a_runtime_call_inside_a_nested_lambda_refuses() {
    let source = format!(
        "{}\n{}",
        concat!(
            "tag int\n",
            "tag string\n",
            "struct list[t]\n",
            "extern fn println (s: string) -> ()\n",
            "extern fn map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]\n",
            "trait shows {\n",
            "  fn show(self) -> string\n",
            "}\n",
            "impl shows for int {\n",
            "  fn show(self) -> string { \"n\" }\n",
            "}\n",
            "impl [t: shows] shows for list[t] {\n",
            "  fn show(self) -> string {\n",
            "    const parts = self.map((e) => e.show())\n",
            "    \"l\"\n",
            "  }\n",
            "}\n",
        ),
        concat!(
            "fn main() -> int {\n",
            "  println([1].show())\n",
            "  0\n",
            "}\n",
        )
    );
    refuses_with(
        &source,
        "a nested lambda or a generic function body has none",
    );
}

/// A mutable binding over the impl's type variable: a reassignment would
/// drop the previous word's count by runtime kind, and the placement has
/// no overwrite contract for that yet.
#[test]
fn a_mutable_binding_over_the_type_variable_refuses() {
    let source = format!(
        "{}\n{}",
        loop_body(concat!(
            "      var y = x\n",
            "      y = x\n",
            "      acc = \"${acc}\"\n",
        )),
        concat!(
            "fn main() -> int {\n",
            "  println([1].show())\n",
            "  0\n",
            "}\n",
        )
    );
    refuses_with(&source, "no overwrite contract");
}

/// A checked read whose ok/none union would carry the variable's runtime
/// word as a payload: the union's payload region has no static shape for
/// it.
#[test]
fn a_checked_read_spanning_the_variable_word_refuses() {
    let source = format!(
        "{}\n{}",
        loop_body("      const y = self.head()\n      acc = \"${acc}\""),
        concat!(
            "fn main() -> int {\n",
            "  println([1].show())\n",
            "  0\n",
            "}\n",
        )
    );
    refuses_with(&source, "a union member over a type variable");
}
