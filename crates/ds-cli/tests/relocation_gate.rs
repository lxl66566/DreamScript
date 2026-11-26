//! The relocation-invariance gate over the full serving seam: a
//! relocated pinned declaration in the composed ambient text must not
//! change what a cold session compile computes. The fixture regenerates
//! the relocated composition's own artifacts in-test — the `.dsi` serve
//! and the ambient's pre-lowered module — runs one program through the
//! served path (the ambient's `.dsi` tables and the module splice, the
//! run path's cold composition) and once through the fully fresh path,
//! and byte-compares the finished modules. A positional seam answers a
//! regenerated-but-relocated composition with silently wrong ids — the
//! miscompile class (unrelated call sites trapping at the ABI over wrong
//! slot layouts), so this gate holds the serve name-addressed.

use std::sync::Arc;

use ds_dsi::serve_prefix;
use ds_mod::Hash128;
use ds_stdlib::AMBIENT_FILE;

/// The user half exercising the relocated composition: a struct field
/// over the json enum, a case construction, the json face's calls, and
/// the collection externs behind them.
const USER: &str = "struct wrap { v: json }\n\nfn main() -> () {\nconst w = wrap { v: json_null \
                    }\nconst j = parse_json(\"[1,{\\\"k\\\":true}]\")\nif j is parse_err \
                    {\nprintln(\"bad\")\n} else {\nprintln(\"${to_json(j)}\")\n}\nprintln(\"${w.v \
                    is json_null}\")\n}\n";

/// One block cut from the ambient text, relocated to the tail.
fn relocate(head: &str, tail: &str) -> String {
    let source = ds_stdlib::AMBIENT_SOURCE;
    let start = source.find(head).expect("the block heads");
    let end = source[start..].find(tail).expect("the block closes") + start + tail.len();
    let mut out = String::with_capacity(source.len());
    out.push_str(&source[..start]);
    out.push_str(&source[end..]);
    out.push('\n');
    out.push_str(&source[start..end]);
    out
}

/// The `json` enum, relocated to the composed text's tail: an
/// atom-minting declaration (its seven cases) whose move shifts every
/// later def atom's id. The fixture adds one unconditional tuple-head
/// impl beside the relocation: a head that solves to minted structural
/// atoms, the one membership class a positional seam consumes raw.
fn relocated_json() -> String {
    let mut out = relocate(
        "pub enum json {",
        "json_object { fields: map[string, json] }\n}\n",
    );
    out.push_str("\nimpl show for (int, int) {\n  fn show(self) -> string { \"pair\" }\n}\n");
    out
}

/// The private collection externs, relocated to the composed text's
/// tail: the four declarations pinned here (they mint no
/// atoms, so the move exercises every def-keyed seam alone).
fn relocated_externs() -> String {
    relocate(
        "extern fn list_get[t]",
        "extern fn map_get[k, v] (m: map[k, v], key: k) -> v | none\n",
    )
}

/// The relocated composition's full pipeline: the ambient's `.dsi`
/// artifact, its pre-lowered module, the served analyze of the
/// ambient-plus-user program, and the fresh analyze of the same.
fn composition(source: &str) -> (ds_mod::Module, ds_mod::Module) {
    let mut files = ds_diag::Files::new();
    let ambient = files.add_source(AMBIENT_FILE, source);
    let user = files.add_source("user.ds", USER);
    let user_ast = ds_parse::parse_script(&files, user).expect("the user source parses");
    let parsed = ds_parse::parse(&files, ambient).expect("the relocated ambient parses");
    let (expanded, expand_diags) = ds_meta::expand_derives(&[&parsed]);
    assert!(
        expand_diags.is_empty(),
        "the relocated ambient expands clean: {expand_diags:?}"
    );
    let tree = expanded.into_iter().next().expect("one");

    // the `.dsi` over the relocated text: collect, resolve, the
    // declaration-phase snapshot, the clean gate, the effects rows
    let mut package = ds_check::Package::new();
    let mut resolve_diags = package.add_file(&tree);
    let (uses_rows, mut file_diags) = package.resolve(&tree);
    resolve_diags.append(&mut file_diags);
    let tables = vec![uses_rows];
    let (decls, decl_diags) = ds_check::decls::build(&package, &[tree.as_ref()], &tables);
    assert!(
        decl_diags.is_empty(),
        "the relocated ambient declares clean: {decl_diags:?}"
    );
    let analyzed = ds_check::analyze_resolved(
        &files,
        Arc::from(vec![Arc::clone(&tree)]),
        package.into(),
        tables.into(),
        resolve_diags,
        Arc::default(),
    );
    assert!(
        analyzed.checked.diags.is_empty(),
        "the relocated ambient checks clean: {:?}",
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
    let artifact = ds_dsi::extract(&decls, &analyzed, &effects, &method_rows);

    // the ambient module over the same analyze: the splice source the
    // dsmod artifact would regenerate to
    let ambient_lowered = ds_lower::lower_ambient(
        "@std",
        Hash128::compute(source.as_bytes()),
        &analyzed.asts[..],
        &analyzed,
    )
    .expect("the relocated ambient lowers");
    let ambient_module = ambient_lowered.module().clone();
    assert!(
        ambient_module.chains().is_empty(),
        "the ambient lowers to the splice's shape"
    );

    // the served run: the artifact's tables, the spliced module
    let served = serve_prefix(&artifact, &[ambient]).expect("the relocated artifact serves");
    let served_run = ds_meta::analyze_served(
        &files,
        vec![Arc::clone(&tree), Arc::new(user_ast.clone())],
        &[true, false],
        served.package,
        served.resolutions,
        served.decls,
    );
    assert!(
        served_run.checked.diags.is_empty(),
        "the served run checks clean: {:?}",
        served_run.checked.diags
    );
    let park = ds_check::Park::default();
    let served_linear = ds_check::linearity_check_served(
        &served_run.asts,
        &served_run.package,
        &served_run.resolutions,
        &served_run.decls,
        &served_run.checked,
        1,
        &park,
    );
    assert!(served_linear.diags.is_empty(), "the served run walks clean");
    let served_lowered = ds_lower::lower_with_served(
        "user",
        Hash128::new([7; 16]),
        &served_run.asts[..],
        &served_run,
        ds_lower::Served {
            functions: ambient_module.clone().into_functions(),
            tags: ambient_module.tags().to_vec(),
            spans: ambient_module.pc_spans().cloned(),
        },
        Some(&served_linear),
        &ds_lower::ComptimeValues::default(),
    )
    .expect("the served run lowers");

    // the fresh run over the same sources
    let fresh_run = ds_meta::analyze_owned(&files, vec![Arc::new(parsed), Arc::new(user_ast)], &[
        false, false,
    ]);
    assert!(
        fresh_run.checked.diags.is_empty(),
        "the fresh run checks clean: {:?}",
        fresh_run.checked.diags
    );
    let fresh_linear = ds_check::linearity_check(
        &fresh_run.asts,
        &fresh_run.package,
        &fresh_run.resolutions,
        &fresh_run.decls,
        &fresh_run.checked,
    );
    assert!(fresh_linear.diags.is_empty(), "the fresh run walks clean");
    let fresh_lowered = ds_lower::lower_with_comptime(
        "user",
        Hash128::new([7; 16]),
        &fresh_run.asts[..],
        &fresh_run,
        Some(&fresh_linear),
        &ds_lower::ComptimeValues::default(),
    )
    .expect("the fresh run lowers");

    (
        served_lowered.module().clone(),
        fresh_lowered.module().clone(),
    )
}

/// The relocated json enum serves a byte-identical module.
#[test]
fn a_relocated_json_enum_lowers_identically_served() {
    let (served, fresh) = composition(&relocated_json());
    assert_eq!(
        ds_mod::write(&served),
        ds_mod::write(&fresh),
        "the served splice and the fresh lowering must agree over the relocated composition"
    );
}

/// The relocated private externs serve a byte-identical module.
#[test]
fn a_relocated_extern_lowers_identically_served() {
    let (served, fresh) = composition(&relocated_externs());
    assert_eq!(
        ds_mod::write(&served),
        ds_mod::write(&fresh),
        "the served splice and the fresh lowering must agree over the relocated composition"
    );
}
