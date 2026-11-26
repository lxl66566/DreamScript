//! End-to-end `ds check` through the binary: the conformance corpus checks
//! clean as-is under the stdlib ambient (the M3 checked-declaration layer),
//! the plain fixtures stay self-contained under `--no-ambient`, and the
//! negative fixture reports its overlap and moved-then-used pair with
//! golden renderings.

use std::{
    env,
    fmt::Write,
    fs,
    process::{Command, Output},
};

mod support;

fn ds_check(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ds"))
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("the ds binary builds with the test")
}

/// One corpus file's documented noise: the file checks with the ambient
/// (exit 0), stdout stays empty, and stderr prints exactly the staged
/// leftovers the corpus README's stage vocabulary owns — the printed
/// count is the pin; every diagnostic must name an item staged away from
/// `typecheck`, or the command would have failed.
fn assert_corpus_clean(file: &str, noise: usize) {
    let path = ds_corpus_path::file(file);
    let out = ds_check(&["check", path.to_str().expect("corpus path is UTF-8")]);
    assert_eq!(out.status.code(), Some(0), "{file}");
    assert_eq!(out.stdout, [] as [u8; 0], "{file}");
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert_eq!(
        stderr.matches("error[").count(),
        noise,
        "{file}: staged leftovers changed: {stderr}"
    );
}

/// The M3 acceptance: the corpus files check clean as-is, the stdlib
/// ambient arriving as the package's first member. The documented noise
/// per file, all confined to items the header stages away from
/// `typecheck`:
///
/// * types_hybrid: zero noise — the json family and its parse_err ride the ambient's own enum and
///   reader (`get_port` narrows the json_object/json_int cases, `load` carries parse_err in its
///   union) — the io failure and the config type ride the ambient, the fallback's
///   `log`/`default_config`/`get` externs are the file's own.
/// * generics: the reject item's intended overlap plus the receiverless `to_json` member's call
///   spelling (`p.to_json()` falls to the free json writer's UFCS reading, its `json` parameter not
///   containing `point` — the member answers `point.to_json(p)`); the derive itself now synthesizes
///   clean, and the comptime `fields`/`type_name` items check clean under the ambient's reflection
///   vocabulary.
/// * syntax_ts_heart: run items only — the `.map` chain's partition seam (partition's one-`t`
///   signature keeps the error members in `users`, so the later field accesses name the union that
///   wants narrowing); the config/rows readers resolve through the file's own externs.
/// * linearity_modes_rc: run items only — the ambient's `open_file` is named apart from the file's
///   own `open` (the unknown-name pair), its `read_exact` answering the dot call over the file's
///   `file`-typed receiver with the `not contained` item (the file's face predates the ambient's;
///   `to_u32_le` still refuses at lowering as before) and `u32`'s distinct width [B7] (the `.err`
///   fallback meets `lens.len()`'s int — the retired `or` hid that width meeting); the epoch
///   suppresses the `empty`-typed follow-ons those roots used to print.
/// * effects_nursery: zero noise — `urls` and the net failure taxonomy are ambient, and the
///   fetch/retry matches use member-test arms plus the leftover page binder.
/// * range_for: zero noise — the for-over-range and `in` membership items are self-contained under
///   the ambient.
/// * pattern_dispatch: zero noise — the tutorial-pass regressions check clean against the
///   ambient-owned io_err (the tail `?` over the contained union included).
/// * while_cond_refs: zero noise — the dying string/list/deinit-claim while conditions are
///   self-contained under the ambient (the corpus README's run stage documents their placement).
/// * nursery_surface: zero noise — the nursery items are self-contained under the ambient (the net
///   failure taxonomy and the cancel seam are ambient-owned).
/// * tutorial_surface: zero noise — the interpolation and receiverless-default items are
///   self-contained under the ambient (the derive traits and `println` beside the primitives).
/// * shift_bitwise: the one error is the file's own reject item (the float `&` operand); the value,
///   precedence, and trap-edge items are self-contained under the ambient.
#[test]
fn the_corpus_checks_clean_under_the_stdlib_ambient() {
    assert_corpus_clean("types_hybrid.ds", 0);
    assert_corpus_clean("generics_coherent_traits.ds", 2);
    assert_corpus_clean("syntax_ts_heart.ds", 5);
    assert_corpus_clean("linearity_modes_rc.ds", 9);
    assert_corpus_clean("effects_nursery.ds", 0);
    assert_corpus_clean("range_for.ds", 0);
    assert_corpus_clean("pattern_dispatch.ds", 0);
    // the one error is the file's own reject item (the mixed-element literal)
    assert_corpus_clean("lists_surface.ds", 1);
    // the one error is the file's own reject item (the union destructuring)
    assert_corpus_clean("tuples_surface.ds", 1);
    // the one error is the file's own reject item (the constructor
    // arguments); the `get` spelling item checks under the file's own
    // `get` extern (the ambient shed its dead declarations)
    assert_corpus_clean("maps_surface.ds", 1);
    // the one error is the file's own reject item (the non-optional
    // unwrap_or receiver); the run items ride the checked list reads
    assert_corpus_clean("extraction_family.ds", 1);
    // the one error is the file's own reject item (the extra argument at
    // an indirect fn-typed call); the defaults call and the widened
    // argument shapes are self-contained under the ambient, and the
    // inline-member items check clean with their lowering refusal as the
    // e2e pin
    assert_corpus_clean("calls_surface.ds", 1);
    // the one error is the file's own reject item (the wrong-receiver
    // dot-call); the dot-calls and field reads check clean and the
    // member bodies dispatch through the inherent seam
    assert_corpus_clean("inherent_impls.ds", 1);
    // the one error is the file's own reject item (the twice-consumed
    // seed's moved/used pair); the reversed-order and cross-file
    // consumption shapes ride the gate's two-file harness
    assert_corpus_clean("toplevel_consumption.ds", 1);
    // zero noise — the deinit surface runs self-contained under the
    // ambient (record C5, the deinit wiring round)
    assert_corpus_clean("deinit_surface.ds", 0);
    // zero noise — the while-cond Ref operands are loop-carried, so the
    // dying-string/list/deinit-claim items are self-contained under the
    // ambient (record C4, the while-cond placement round)
    assert_corpus_clean("while_cond_refs.ds", 0);
    // zero noise — the spawn/join/cancel/unioning items are
    // self-contained under the ambient (M5, the nursery surface round)
    assert_corpus_clean("nursery_surface.ds", 0);
    // zero noise — the defaulted-parameter spellings are self-contained
    // under the ambient (F7, the trait-default authority round)
    assert_corpus_clean("trait_defaults.ds", 0);
    // zero noise — the list[record] items ride the ambient list surface
    // (the native result span channel round)
    assert_corpus_clean("records_lists.ds", 0);
    // zero noise — the generic impl heads and their nested dispatch are
    // self-contained under the ambient (H3, the transitive evidence round)
    assert_corpus_clean("transitive_dispatch.ds", 0);
    // zero noise — the nursery teardown items are self-contained under
    // the ambient (E5, the drain-doors round)
    assert_corpus_clean("teardown_deinit.ds", 0);
    // zero noise — the tutorial's recently landed interpolation and
    // receiverless-default items are self-contained under the ambient
    assert_corpus_clean("tutorial_surface.ds", 0);
    // the one error is the file's own reject item (the float `&`
    // operand the integer table refuses)
    assert_corpus_clean("shift_bitwise.ds", 1);
    // zero noise — the annotated member's default() spellings and the
    // defaulting struct field are self-contained under the ambient (the
    // union-default member annotation round)
    assert_corpus_clean("derive_default_union.ds", 0);
    // zero noise — the pure-row helper admits at the comptime fence, and
    // `ds check` never runs the fence (the comptime pure-fn admission round)
    assert_corpus_clean("comptime_admission.ds", 0);
    // the three errors are the file's own reject items (the `pub import`
    // re-export, the `pub impl`, the missing `@std` selection); the tier
    // spellings and the clean `@std` selection check clean (the G4
    // visibility round)
    assert_corpus_clean("visibility_surface.ds", 3);
    // the nine errors are the file's own reject items (the eight
    // none-typed field spellings and the none-headed application); the
    // two optional spellings construct alike at a field and a case
    // slot (the ds-check conformance gate pins the reject class)
    assert_corpus_clean("none_fields.ds", 9);
}

/// The two-file visibility package: `pub(pkg)` and `pub` names answer a
/// cross-file use site while the private name refuses per use site — the
/// pair checked together (the user file alone resolves nothing), the three
/// defining-file rejects beside the cross-file DS4017.
#[test]
fn the_visibility_pair_enforces_the_file_grain() {
    let lib = ds_corpus_path::file("visibility_surface.ds");
    let user = ds_corpus_path::file("visibility_user.ds");
    let out = ds_check(&[
        "check",
        lib.to_str().expect("corpus path is UTF-8"),
        user.to_str().expect("corpus path is UTF-8"),
    ]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(out.stdout, [] as [u8; 0]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert_eq!(stderr.matches("error[").count(), 4, "{stderr}");
    assert!(stderr.contains("error[DS4017]: `hidden` is private to its defining file"));
}

/// The intended rejection prints as noise under the ambient and pins its
/// golden rendering: the overlap diagnostic the corpus header expects.
#[test]
fn the_generics_reject_item_prints_its_overlap() {
    let path = ds_corpus_path::file("generics_coherent_traits.ds");
    let out = ds_check(&["check", path.to_str().expect("corpus path is UTF-8")]);
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8(out.stderr).unwrap().contains(
        "error[DS2006]: instance head `int | bool` intersects the `int` instance; witness value \
         type `int`"
    ));
}

/// Without the ambient the corpus's stdlib names do not resolve: the
/// ambient is what makes the corpus check clean.
#[test]
fn without_the_ambient_the_corpus_names_stay_unresolved() {
    let path = ds_corpus_path::file("types_hybrid.ds");
    let out = ds_check(&[
        "check",
        "--no-ambient",
        path.to_str().expect("corpus path is UTF-8"),
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .contains("unknown name `read_file`")
    );
}

/// A self-contained package under the ambient collides with the shared
/// namespace [G3]: the file's own `tag int` redefines the ambient's.
#[test]
fn a_package_redeclaring_an_ambient_name_reports_the_duplicate() {
    let out = ds_check(&["check", "tests/fixtures/clean.ds"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .contains("`int` is already defined")
    );
}

/// A valid single-file package that declares every name it uses: nothing
/// to report, in either rendering.
#[test]
fn a_clean_single_file_package_is_silent() {
    for format in ["human", "json"] {
        let out = ds_check(&[
            "check",
            "--no-ambient",
            "--error-format",
            format,
            "tests/fixtures/clean.ds",
        ]);
        assert_eq!(out.status.code(), Some(0), "format {format}");
        assert_eq!(out.stdout, [] as [u8; 0], "format {format}");
        assert_eq!(out.stderr, [] as [u8; 0], "format {format}");
    }
}

/// The negative fixture: the intended overlap rejection [B6] and
/// moved-then-used pair [C1] are fatal and render golden in both formats.
#[test]
fn the_negative_fixture_renders_golden_human_diagnostics() {
    let out = ds_check(&["check", "--no-ambient", "tests/fixtures/moved_overlap.ds"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(out.stdout, [] as [u8; 0]);
    assert_eq!(
        String::from_utf8(out.stderr).unwrap(),
        "\
error[DS2006]: instance head `int | bool` intersects the `int` instance; witness value type `int`
 --> tests/fixtures/moved_overlap.ds:16:14
   |
16 | impl enc for int | bool {
   |              ^^^^^^^^^^ overlapping head
   |
   = note: the earlier instance's head covers `int`; two instances of one trait must be pairwise \
         disjoint [B6]
error[DS3001]: value `c` is no longer usable
 --> tests/fixtures/moved_overlap.ds:26:8
   |
25 |   send(c)
   |        - moved here
   |
26 |   send(c)
   |        ^ used again here after move
   |
   = note: `c` is `~Copyable` and was consumed
"
    );
}

#[test]
fn the_negative_fixture_renders_golden_json_diagnostics() {
    let out = ds_check(&[
        "check",
        "--no-ambient",
        "--error-format",
        "json",
        "tests/fixtures/moved_overlap.ds",
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(out.stdout, [] as [u8; 0]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 2, "one JSON object per diagnostic");
    assert!(lines[0].starts_with(r#"{"code":"DS2006""#));
    assert!(lines[0].contains(r#""message":"instance head `int | bool` intersects the `int` instance; witness value type `int`""#));
    assert!(lines[0].contains(r#""primary":true"#));
    assert!(lines[1].starts_with(r#"{"code":"DS3001""#));
    assert!(lines[1].contains(r#""label":"moved here","primary":false"#));
    assert!(lines[1].contains(r#""label":"used again here after move","primary":true"#));
    assert!(lines[1].contains(r#""notes":["`c` is `~Copyable` and was consumed"]"#));
    assert!(stderr.ends_with('\n'));
}

/// A missing source is a self-describing failure, not a crash.
#[test]
fn a_missing_source_reports_the_path() {
    let out = ds_check(&["check", "tests/fixtures/absent.ds"]);
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(stderr.contains("reading tests/fixtures/absent.ds"));
}

/// A derived member's call checks clean: the resolve and check queries run
/// over the derive-expanded trees, so the synthesized impl resolves like a
/// hand-written one and the call's story is silent.
#[test]
fn a_derived_members_call_checks_clean() {
    let out = ds_check(&["check", "--no-ambient", "tests/fixtures/derive_call.ds"]);
    assert_eq!(out.status.code(), Some(0));
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert_eq!(stderr, "", "the derived call carries no diagnostics");
    assert_eq!(out.stdout, [] as [u8; 0]);
}

/// The @std faces resolve implicitly: `ds check` registers the session
/// composition — the run path's implicit-resolution shape — so a file
/// using the migrated json and string faces with no import row checks
/// clean exactly as `ds run` resolves it.
#[test]
fn the_std_faces_resolve_implicitly_without_an_import_row() {
    let dir = support::guarded_dir("ds-cli-check-faces-");
    let path = dir.join("faces.ds");
    fs::write(
        &path,
        concat!(
            "fn label(s: string) -> string {\n",
            "  s.trim().to_upper()\n",
            "}\n",
            "\n",
            "fn port(text: string) -> int | parse_err {\n",
            "  json_as_int(parse_json(text)?)\n",
            "}\n",
        ),
    )
    .expect("the temp source writes");
    let out = ds_check(&["check", path.to_str().expect("temp path is UTF-8")]);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(0));
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert_eq!(stderr, "", "the bare face uses carry no diagnostics");
    assert_eq!(out.stdout, [] as [u8; 0]);
}

/// The data-shaped @std members stay link-bound under the session
/// registration: a bare use is an unknown name at the check, the same
/// boundary `ds run` serves over the implicit graph (a declaring
/// graph's import row binds the member through the linked surface).
#[test]
fn the_std_data_members_stay_link_bound_at_the_check() {
    let dir = support::guarded_dir("ds-cli-check-residue-");
    let path = dir.join("residue.ds");
    fs::write(&path, "fn probe(r: row) -> int {\n  1\n}\n").expect("the temp source writes");
    let out = ds_check(&["check", path.to_str().expect("temp path is UTF-8")]);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("unknown type `row`"),
        "the bare residue use stays link-bound: {stderr}"
    );
}

/// A file past the DefId block budget reports a spanned diagnostic per
/// overflowing block instead of aborting: blocks 0..=255 still check (an
/// early block's real error prints beside the packing diagnostics, never
/// swallowed by a bare engine escape), and block 256 would silently alias
/// the next file's block 0, so the overflow stays fatal.
#[test]
fn a_file_past_the_block_budget_reports_each_block_and_keeps_checking() {
    let mut source = String::from("const dup = 0\n##\nconst dup = 1\n");
    for i in 0..=256_u32 {
        source.push_str("##\n");
        // writing into a String cannot fail
        let _ = writeln!(source, "const b{i} = {i}");
    }
    let dir = support::guarded_dir("ds-cli-check-blocks-");
    let path = dir.join("many_blocks.ds");
    fs::write(&path, source).expect("the temp source writes");
    let out = ds_check(&[
        "check",
        "--no-ambient",
        path.to_str().expect("temp path is UTF-8"),
    ]);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("`dup` is already defined"),
        "the early blocks' own diagnostics still print: {stderr}"
    );
    assert_eq!(
        stderr.matches("error[DS4023]").count(),
        3,
        "blocks 256..=258 each report their own span: {stderr}"
    );
    assert!(
        stderr.contains("block 256 of file 0 does not fit the DefId packing"),
        "stderr: {stderr}"
    );
}

/// Any number of same-trait generic impls over distinct constructors
/// checks in bounded time: the coherence probe is a pure query (it once
/// grounded every candidate atom into the shared universe, and its own
/// products became later probes' candidates — five impls never returned).
#[test]
fn many_generic_impls_of_one_trait_check_clean() {
    for count in [5_u32, 50] {
        let mut source = String::from("pub trait sized { fn size(self) -> int }\n");
        for i in 0..count {
            // writing into a String cannot fail
            let _ = writeln!(source, "pub struct c{i}[t] {{ a: t }}");
        }
        for i in 0..count {
            let _ = writeln!(
                source,
                "impl[t] sized for c{i}[t] {{ fn size(self) -> int {{ 1 }} }}"
            );
        }
        source.push_str("fn main() -> int { 0 }\n");
        let dir = support::guarded_dir(&format!("ds-cli-check-coherence-{count}-"));
        let path = dir.join("many_impls.ds");
        fs::write(&path, source).expect("the temp source writes");
        let out = ds_check(&["check", path.to_str().expect("temp path is UTF-8")]);
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(out.status.code(), Some(0), "count {count}");
        assert_eq!(out.stdout, [] as [u8; 0], "count {count}");
        let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
        assert_eq!(stderr, "", "count {count}");
    }
}
