//! The comptime declaration surface through the checker: a
//! free fn whose parameters are all `comptime name: type` checks clean
//! with type names as argument values, the reflection builtins answer
//! inside its body, a scalar-annotated comptime parameter checks clean
//! beside them, and every other shape refuses at its own door — a `type`
//! annotation without the comptime mode (DS2030), a comptime parameter
//! outside that shape (DS2031),
//! and a reflection builtin outside a comptime evaluation context
//! (DS2032).

use ds_check::analyze;
use ds_diag::Files;

fn analyze_source(name: &str, source: &str) -> ds_check::Analyzed {
    let mut files = Files::new();
    let id = files.add_source(name, source);
    let ast = ds_parse::parse(&files, id).expect("parses");
    let asts = [ast];
    analyze(&files, &asts)
}

fn messages(analyzed: &ds_check::Analyzed) -> Vec<String> {
    analyzed
        .checked
        .diags
        .iter()
        .map(|d| format!("{} {}", d.code, d.message))
        .collect()
}

const AMBIENT: &str = concat!(
    "tag int\n",
    "tag string\n",
    "tag bool\n",
    "trait Copyable { }\n",
    "struct list[t]\n",
    "struct field { name: string, type: type, tag: string }\n",
    "extern fn fields (t: type) -> list[field]\n",
    "extern fn type_name (t: type) -> string\n",
    "extern fn concat (a: string, b: string) -> string\n",
    "extern fn join (parts: list[string], sep: string) -> string\n",
    "extern fn println (msg: string) -> ()\n",
);

#[test]
fn comptime_schema_checks_clean() {
    let source = format!(
        "{AMBIENT}struct point {{ x: int, y: int }}
fn schema(comptime t: type) -> string {{
  var s = \"table {{\"
  for f in fields(t) {{
    s += f.name + \": \" + type_name(f.type) + \", \"
  }}
  s + \"}}\"
}}
fn main() -> () {{
  println(schema(point))
}}
"
    );
    let analyzed = analyze_source("schema.ds", &source);
    let rendered = messages(&analyzed);
    assert!(
        rendered.is_empty(),
        "the schema fn checks clean: {rendered:?}"
    );
}

#[test]
fn type_param_without_comptime_refuses() {
    let analyzed = analyze_source(
        "plain_type_param.ds",
        &format!("{AMBIENT}fn plain(t: type) -> int {{ 0 }}\n"),
    );
    let rendered = messages(&analyzed);
    assert_eq!(rendered.len(), 1, "exactly the mode refusal: {rendered:?}");
    assert!(
        rendered[0].starts_with("DS2030"),
        "the code is the mode refusal: {rendered:?}"
    );
}

#[test]
fn mixed_comptime_params_refuse() {
    let analyzed = analyze_source(
        "mixed.ds",
        &format!("{AMBIENT}fn mixed(comptime t: type, n: int) -> int {{ n }}\n"),
    );
    let rendered = messages(&analyzed);
    assert!(
        rendered.iter().any(|m| m.starts_with("DS2031")),
        "the mixed shape refuses: {rendered:?}"
    );
}

#[test]
fn comptime_scalar_annotation_checks_clean() {
    let source = format!(
        "{AMBIENT}fn sized(comptime n: int) -> int {{ n }}
fn labeled(comptime label: string) -> string {{ label }}
fn main() -> () {{
  println(\"${{sized(4)}} ${{labeled(\"x\")}}\")
}}
"
    );
    let analyzed = analyze_source("scalar.ds", &source);
    let rendered = messages(&analyzed);
    assert!(
        rendered.is_empty(),
        "the scalar-annotated comptime parameters check clean: {rendered:?}"
    );
}

#[test]
fn comptime_non_scalar_annotation_refuses() {
    let analyzed = analyze_source(
        "struct_param.ds",
        &format!(
            "{AMBIENT}fn sized(comptime p: point) -> int {{ 0 }}\nstruct point {{ x: int }}\n"
        ),
    );
    let rendered = messages(&analyzed);
    assert!(
        rendered.iter().any(|m| m.starts_with("DS2031")),
        "a non-scalar comptime parameter refuses: {rendered:?}"
    );
}

#[test]
fn nested_lambda_comptime_param_refuses() {
    let source = format!(
        "{AMBIENT}fn main() -> () {{
  const f = (comptime t: type) => \"x\"
}}
"
    );
    let analyzed = analyze_source("nested.ds", &source);
    let rendered = messages(&analyzed);
    assert!(
        rendered.iter().any(|m| m.starts_with("DS2031")),
        "a nested lambda's comptime parameter refuses: {rendered:?}"
    );
}

#[test]
fn reflection_outside_comptime_refuses() {
    let source = format!(
        "{AMBIENT}struct point {{ x: int }}
fn main() -> () {{
  println(type_name(point))
}}
"
    );
    let analyzed = analyze_source("runtime_fields.ds", &source);
    let rendered = messages(&analyzed);
    assert!(
        rendered.iter().any(|m| m.starts_with("DS2032")),
        "the reflection builtin refuses outside comptime: {rendered:?}"
    );
}

#[test]
fn call_into_plain_type_param_refuses_at_the_argument() {
    let source = format!(
        "{AMBIENT}struct point {{ x: int }}
fn id(t: type) -> string {{ \"x\" }}
fn main() -> () {{
  println(id(point))
}}
"
    );
    let analyzed = analyze_source("runtime_type_value.ds", &source);
    let rendered = messages(&analyzed);
    // the declaration carries DS2030, the call's type-value argument adds
    // the refusal outside any comptime context
    assert!(
        rendered.iter().any(|m| m.starts_with("DS2032")),
        "the type-value argument refuses outside comptime: {rendered:?}"
    );
}
