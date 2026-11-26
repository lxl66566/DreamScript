//! The enum-block declaration story [B3]: cases mint into the tag
//! namespace like standalone tags, the enum name is the union of its
//! cases, construction is fixed per declaration with cross-spellings as
//! structured checker errors, and qualified case paths test the case's
//! own atom.

use ds_check::type_check;
use ds_diag::Files;
use ds_parse::parse;

/// Checks one self-contained source, returning its diagnostics.
fn diags(source: &str) -> Vec<ds_diag::Diagnostic> {
    let mut files = Files::new();
    let file = files.add_source("t.ds", source);
    let ast = parse(&files, file).unwrap_or_else(|d| panic!("parses: {d:?}"));
    type_check(&files, &[ast]).diags
}

/// Asserts the source checks clean, printing any diagnostics.
fn clean(source: &str) {
    let found = diags(source);
    assert!(
        found.is_empty(),
        "{}",
        found
            .iter()
            .map(|d| format!("{} {}", d.code.as_str(), d.message))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The (code, message) pairs of a source's diagnostics.
fn codes(source: &str) -> Vec<(&'static str, String)> {
    diags(source)
        .iter()
        .map(|d| (d.code.as_str(), d.message.clone()))
        .collect()
}

#[test]
fn cases_mint_into_the_tag_namespace_like_tags() {
    // A union alias over enum-minted atoms behaves exactly like one over
    // standalone tags and structs: construction, narrowing, matching.
    clean(concat!(
        "tag int\n",
        "tag string\n",
        "enum read_err {\n",
        "  timeout\n",
        "  bad_port { got: int }\n",
        "  bad_msg(string)\n",
        "}\n",
        "type err = timeout | bad_port | bad_msg\n",
        "fn mk() -> err { bad_port(got: -1) }\n",
        "fn msg() -> err { bad_msg(\"nan\") }\n",
        "fn wait(e: err) -> int {\n",
        "  match e {\n",
        "    timeout => 0,\n",
        "    other if other is bad_port => other.got,\n",
        "    _ => 0,\n",
        "  }\n",
        "}\n",
    ));
}

#[test]
fn the_enum_name_is_the_union_of_its_cases() {
    // The enum name narrows and carries exhaustiveness as a union would.
    clean(concat!(
        "tag int\n",
        "enum mark { red, blue { n: int } }\n",
        "fn f(m: mark) -> int {\n",
        "  if m is blue { m.n } else { 0 }\n",
        "}\n",
        "fn g(m: mark) -> int {\n",
        "  match m { red => 0, blue { n } => n }\n",
        "}\n",
        "type with_mark = mark | int\n",
        "fn h(x: with_mark) -> int {\n",
        "  match x { mark => 1, _ => 0 }\n",
        "}\n",
    ));
}

#[test]
fn construction_forms_check_clean_per_declaration() {
    // struct = brace form; payload case = call form (named or
    // positional); bare case = bare.
    clean(concat!(
        "tag int\n",
        "enum e { a, b { x: int }, c(int) }\n",
        "struct s { y: int }\n",
        "const v1 = a\n",
        "const v2 = b(x: 1)\n",
        "const v3 = c(2)\n",
        "const v4 = s { y: 3 }\n",
    ));
}

#[test]
fn brace_form_on_a_case_is_the_cross_spelling() {
    let found = codes("tag int\nenum e { b { x: int } }\nconst v = b { x: 1 }\n");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].0, "DS2001");
    assert!(found[0].1.contains("is an enum case"), "{}", found[0].1);
}

#[test]
fn a_payload_case_used_bare_is_the_cross_spelling() {
    let found = codes("tag int\nenum e { b { x: int } }\nconst v = b\n");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].0, "DS2002");
    assert!(
        found[0].1.contains("construct it by call form"),
        "{}",
        found[0].1
    );
}

#[test]
fn call_forms_on_bare_cases_and_structs_stay_rejected() {
    // A bare case and a plain struct keep the existing "type called as a
    // function" error; a named argument against a tuple payload names no
    // field.
    let found = codes("tag int\nenum e { a, c(int) }\nstruct s { y: int }\nconst v1 = a()\n");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].0, "DS2002");
    let found = codes("tag int\nenum e { a, c(int) }\nstruct s { y: int }\nconst v1 = s(1)\n");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].0, "DS2002");
    let found = codes("tag int\nenum e { a, c(int) }\nconst v1 = c(x: 1)\n");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].0, "DS2001");
    assert!(found[0].1.contains("has no field"), "{}", found[0].1);
}

#[test]
fn a_case_label_beside_a_value_follows_the_fn_at_call_form() {
    // F9.5: tag names left the value namespace, so the call form always
    // means the fn — even beside a payloaded case of the same label. The
    // constructor families keep F9's residual DS2014 class.
    let found = codes(concat!(
        "tag int\n",
        "enum e { make(int) }\n",
        "fn make(n: int) -> int { n }\n",
        "const v = make(1)\n",
    ));
    assert!(found.is_empty(), "{found:?}");
    let found = codes("tag int\ntag page\nfn page() -> int { 1 }\nconst v = page()\n");
    assert!(found.is_empty(), "{found:?}");
    let found = codes(concat!(
        "tag int\n",
        "struct make { n: int }\n",
        "fn make(n: int) -> int { n }\n",
        "const v = make(1)\n",
    ));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].0, "DS2014");
}

#[test]
fn duplicate_case_names_report_both_spans() {
    let found = codes("enum e { a, a }\n");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].0, "DS2005");
}

#[test]
fn qualified_case_paths_test_the_case_atom() {
    clean(concat!(
        "tag int\n",
        "enum e { a(int) }\n",
        "fn f(x: e) -> int {\n",
        "  match x {\n",
        "    e::a(n) => n,\n",
        "    _ => 0,\n",
        "  }\n",
        "}\n",
    ));
}

#[test]
fn generic_enums_keep_the_opaque_atom_until_substitution_lands() {
    // A generic enum still checks as before (an applied atom); a
    // payloaded case's name stays uncollected — its shape is a
    // per-instantiation fact — so a use reports the unknown name.
    clean("tag int\nenum opt[t] { some(t) }\nfn f(x: opt[int]) -> int { 0 }\n");
    let found = codes("tag int\nenum opt[t] { some(t) }\nconst v = some\n");
    // The resolver reports the unknown name and the checker's value walk
    // follows with its unresolvable-value note, as for any unknown name.
    assert!(
        found
            .iter()
            .any(|(code, msg)| *code == "DS2004" && msg.contains("some")),
        "{found:?}"
    );
}

/// A generic enum's unit cases test over the applied head: a unit word
/// carries no substitution, so its atom is a member of every
/// application (the set algebra keeps applications opaque, the
/// membership reading the head's case atoms instead) and the arms
/// exhaust the head case-wise. A payloaded case can never be covered —
/// no spelling resolves its atom — so a match missing it stays honestly
/// non-exhaustive behind a catchall, and another enum's case over the
/// head refuses as a non-member.
#[test]
fn unit_cases_of_a_generic_enum_test_over_the_applied_head() {
    clean(concat!(
        "tag int\n",
        "enum flag[t] { on, off }\n",
        "fn f(x: flag[int]) -> int {\n",
        "  match x {\n",
        "    on => 1,\n",
        "    off => 2,\n",
        "  }\n",
        "}\n",
    ));
    let found = codes(concat!(
        "tag int\n",
        "enum flag[t] { on, off }\n",
        "fn f(x: flag[int]) -> int {\n",
        "  match x {\n",
        "    on => 1,\n",
        "  }\n",
        "}\n",
    ));
    assert!(
        found
            .iter()
            .any(|(code, msg)| *code == "DS2003" && msg.contains("not exhaustive")),
        "a missing unit case stays non-exhaustive: {found:?}"
    );
    clean(concat!(
        "tag int\n",
        "enum keyed[t] { tiny, big(t) }\n",
        "fn f(x: keyed[int]) -> int {\n",
        "  match x {\n",
        "    tiny => 1,\n",
        "    _ => 2,\n",
        "  }\n",
        "}\n",
    ));
    let found = codes(concat!(
        "tag int\n",
        "enum flag[t] { on, off }\n",
        "enum other { alien }\n",
        "fn f(x: flag[int]) -> int {\n",
        "  match x {\n",
        "    alien => 1,\n",
        "    _ => 2,\n",
        "  }\n",
        "}\n",
    ));
    // the directed arm label refuses unknown-member (F9.5), the
    // non-member reading of today's not-a-member mismatch
    assert!(
        found
            .iter()
            .any(|(code, msg)| *code == "DS2045" && msg.contains("names no member")),
        "another enum's case refuses as a non-member: {found:?}"
    );
}

/// `none` is the predeclared unit atom (B4), so a payloaded case named
/// `none` claims no atom of its own: the construction refuses at its own
/// seam instead of typing the binding empty and surfacing the nonsense
/// match diagnostics later.
#[test]
fn a_payloaded_case_named_none_refuses_at_its_construction() {
    for (label, source) in [
        (
            "statement binding",
            concat!(
                "tag int\n",
                "enum opt {\n",
                "  none(int)\n",
                "}\n",
                "fn mk() -> int {\n",
                "  const g = none(7)\n",
                "  0\n",
                "}\n",
            ),
        ),
        (
            "top-level binding",
            concat!(
                "tag int\n",
                "enum opt {\n",
                "  none(int)\n",
                "}\n",
                "const g = none(7)\n",
            ),
        ),
        (
            "named payload",
            concat!(
                "tag int\n",
                "enum opt {\n",
                "  none { x: int }\n",
                "}\n",
                "fn mk() -> int {\n",
                "  const g = none(x: 7)\n",
                "  0\n",
                "}\n",
            ),
        ),
    ] {
        let found = codes(source);
        assert_eq!(
            found.len(),
            1,
            "{label}: the construction's refusal stands alone: {found:?}"
        );
        let (code, message) = &found[0];
        assert_eq!(*code, "DS2022", "{label}: the refusal code pins");
        assert!(
            message.contains("`none` is the predeclared unit atom"),
            "{label}: the refusal names the unit-atom seam: {message}"
        );
    }
}

/// A payloaded case with a minted atom types the binding with the case's
/// own atom — the union member carrying its payload span — so the match
/// arm binds the payload and the construction stays clean.
#[test]
fn a_payloaded_case_construction_types_the_binding_with_the_case_atom() {
    clean(concat!(
        "tag int\n",
        "enum opt {\n",
        "  boxed(int)\n",
        "  nada\n",
        "}\n",
        "fn mk() -> int {\n",
        "  const g = boxed(7)\n",
        "  match g {\n",
        "    boxed(n) => n,\n",
        "  }\n",
        "}\n",
    ));
}

/// A tag carrying a struct payload (the failure taxonomy's payloaded
/// members): it mints a struct-shaped atom, constructs by call form,
/// matches by struct pattern and by `is` narrowing, and both bare use
/// and the brace form refuse as cross-spellings.
#[test]
fn a_payloaded_tag_constructs_by_call_form() {
    clean(
        "tag int\ntag string\ntag bad { pos: int }\nfn f() -> int { match bad(pos: 3) { bad { pos \
         } => pos } }\nfn g(v: int | bad) -> int { if v is bad { 1 } else { 2 } }\n",
    );
    let found = codes("tag int\ntag bad { pos: int }\nconst v = bad\n");
    assert!(
        found[0].1.contains("construct it by call form"),
        "{found:?}"
    );
    let found = codes("tag int\ntag bad { pos: int }\nconst v = bad { pos: 1 }\n");
    assert!(
        found[0].1.contains("construct it by call form"),
        "{found:?}"
    );
}

/// A tag's payload spells named fields: the tuple form is a structured
/// refusal, not a silently accepted shape.
#[test]
fn a_tuple_payload_on_a_tag_refuses() {
    let found = codes("tag bad(int)\n");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].0, "DS2041", "{found:?}");
    assert!(found[0].1.contains("named fields"), "{found:?}");
}
