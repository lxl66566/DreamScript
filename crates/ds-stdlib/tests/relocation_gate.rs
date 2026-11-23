//! The relocation-invariance gate: a served prefix's checked facts must
//! hold under any declaration order its own sources admit, not only the
//! one the checked-in artifacts pin. The fixture relocates a pinned
//! atom-minting declaration (the `json` enum, whose seven cases mint
//! case atoms mid-composition) to the composed ambient's tail,
//! regenerates the prefix artifact over the relocated text in-test, and
//! runs the served-vs-fresh equivalence over the same trees: identical
//! mint sequences, plain and signature tables, instance rows, solved
//! records, and checked facts. A positional serve seam answers a
//! regenerated-but-relocated composition with silently wrong ids — the
//! miscompile class the pitfalls entry records — so this gate holds the
//! seam name-addressed.

use std::sync::Arc;

use ds_dsi::{gates, serve_prefix};

/// The user half exercising the relocated declaration: a struct field
/// over the enum, a case construction, and calls into the json face.
const USER: &str = "struct wrap { v: json }\n\nfn main() -> () {\nconst w = wrap { v: json_null \
                    }\nconst j = parse_json(\"[1,{\\\"k\\\":true}]\")\nif j is parse_err \
                    {\nprintln(\"bad\")\n} else {\nprintln(\"${to_json(j)}\")\n}\nprintln(\"${w.v \
                    is json_null}\")\n}\n";

/// The composed ambient with the `json` enum relocated: the declaration
/// block cut from the core and appended at the composed text's tail, the
/// move a member-boundary relocation that shifts every later def atom's
/// id while keeping the composition one namespace.
fn relocated_ambient() -> String {
    const HEAD: &str = "pub enum json {";
    const TAIL: &str = "json_object { fields: map[string, json] }\n}\n";
    let source = ds_stdlib::AMBIENT_SOURCE;
    let head = source.find(HEAD).expect("the json enum heads its block");
    let tail = source[head..]
        .find(TAIL)
        .expect("the json enum closes its block")
        + head
        + TAIL.len();
    let block = &source[head..tail];
    let mut out = String::with_capacity(source.len());
    out.push_str(&source[..head]);
    out.push_str(&source[tail..]);
    out.push('\n');
    out.push_str(block);
    // One unconditional tuple-head impl beside the relocation: a head
    // that solves to minted structural atoms, the membership class a
    // positional seam consumes raw.
    out.push_str("\nimpl show for (int, int) {\n  fn show(self) -> string { \"pair\" }\n}\n");
    out
}

/// One artifact generation over a given ambient text, mirroring the
/// checked-in generator's pipeline: parse, derive expansion, collect and
/// resolve, the declaration-phase snapshot, the clean gate, the effects
/// pass, and the method rows.
fn generate_over(files: &ds_diag::Files, tree: Arc<ds_parse::Ast>) -> ds_dsi::PrefixDsi {
    let mut package = ds_check::Package::new();
    let mut resolve_diags = package.add_file(&tree);
    let (uses, mut file_diags) = package.resolve(&tree);
    resolve_diags.append(&mut file_diags);
    let tables = vec![uses];
    let (decls, decl_diags) = ds_check::decls::build(&package, &[tree.as_ref()], &tables);
    assert!(
        decl_diags.is_empty(),
        "the relocated ambient does not declare clean: {decl_diags:?}"
    );
    let analyzed = ds_check::analyze_resolved(
        files,
        Arc::from(vec![tree]),
        package.into(),
        tables.into(),
        resolve_diags,
        Arc::default(),
    );
    assert!(
        analyzed.checked.diags.is_empty(),
        "the relocated ambient does not check clean: {:?}",
        analyzed.checked.diags
    );
    let rows = ds_stdlib::declared_rows();
    let caps = ds_stdlib::world_caps();
    let effects = ds_check::effects::effects_of_resolved(
        &analyzed.asts[..],
        &analyzed.package,
        &analyzed.resolutions[..],
        &analyzed.checked.hole_show_dispatches,
        rows,
        caps,
    );
    let method_rows = ds_check::effects::solved_method_rows(
        &analyzed.asts[..],
        &analyzed.package,
        &analyzed.resolutions[..],
        rows,
        caps,
    );
    ds_dsi::extract(&decls, &analyzed, &effects, &method_rows)
}

/// The relocated ambient's artifact over one registration, served.
fn served_prefix(
    files: &ds_diag::Files,
    file: ds_diag::FileId,
    tree: Arc<ds_parse::Ast>,
) -> ds_dsi::ServedPrefix {
    let artifact = generate_over(files, tree);
    serve_prefix(&artifact, &[file]).expect("the relocated artifact serves")
}

/// The served-vs-fresh equivalence over the relocated composition:
/// identical bundles through the gates views, with the prefix arrived
/// served (a fallback that rebuilt fresh would answer equal vacuously).
#[test]
fn a_relocated_declaration_serves_identical_facts() {
    let source = relocated_ambient();
    let mut files = ds_diag::Files::new();
    let ambient = files.add_source(ds_stdlib::AMBIENT_FILE, &source);
    let user = files.add_source("user.ds", USER);
    let user_ast = ds_parse::parse_script(&files, user).expect("the user source parses");

    let parsed = ds_parse::parse(&files, ambient).expect("the relocated ambient parses");
    let (expanded, expand_diags) = ds_meta::expand_derives(&[&parsed]);
    assert!(
        expand_diags.is_empty(),
        "the relocated ambient expands clean: {expand_diags:?}"
    );
    let served_tree = expanded.into_iter().next().expect("one");
    let served = served_prefix(&files, ambient, Arc::clone(&served_tree));
    let served_run = ds_meta::analyze_served(
        &files,
        vec![served_tree, Arc::new(user_ast.clone())],
        &[true, false],
        served.package,
        served.resolutions,
        served.decls,
    );
    assert!(
        served_run.checked.diags.is_empty(),
        "the served run must check clean: {:?}",
        served_run.checked.diags
    );

    let fresh_run = ds_meta::analyze_owned(&files, vec![Arc::new(parsed), Arc::new(user_ast)], &[
        false, false,
    ]);
    assert!(
        fresh_run.checked.diags.is_empty(),
        "the fresh run must check clean: {:?}",
        fresh_run.checked.diags
    );

    assert!(
        served_run.decls.served_defs > 0 && served_run.decls.instances.served_len() > 0,
        "the relocated prefix must arrive served, not rebuilt"
    );
    assert_eq!(
        gates::mint_facts(&served_run.decls.universe),
        gates::mint_facts(&fresh_run.decls.universe),
        "the universe mint sequences must match"
    );
    assert_eq!(
        gates::plain_tables(&served_run),
        gates::plain_tables(&fresh_run),
        "the plain declaration tables must match"
    );
    assert_eq!(
        gates::signature_tables(&served_run),
        gates::signature_tables(&fresh_run),
        "the signature tables must match"
    );
    assert_eq!(
        gates::instance_rows(&served_run),
        gates::instance_rows(&fresh_run),
        "the instance rows must match"
    );
    assert_eq!(
        gates::trait_facts(&served_run),
        gates::trait_facts(&fresh_run),
        "the trait registries must match"
    );
    assert_eq!(
        served_run.decls.trait_impl_args, fresh_run.decls.trait_impl_args,
        "the trait-reference arguments must match"
    );
    assert_eq!(
        served_run.decls.inherent_impl_bounds, fresh_run.decls.inherent_impl_bounds,
        "the inherent bounds must match"
    );
    assert_eq!(
        served_run.decls.impl_fn_rets, fresh_run.decls.impl_fn_rets,
        "the impl-member returns must match"
    );
    assert_eq!(
        served_run.checked.def_types, fresh_run.checked.def_types,
        "the top-level types must match"
    );
    assert_eq!(
        gates::rendered_exprs(&served_run),
        gates::rendered_exprs(&fresh_run),
        "the checked expression facts must match"
    );
}
