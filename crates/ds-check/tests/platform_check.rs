//! The platform check: the package's world usage verified
//! inside what its platform declaration provides. The check is the
//! record's subset rule at its finer grain — each definition's world
//! usage names the capabilities whose ambient functions it calls, and
//! every attributed capability must be provided; world usage with no
//! declaration refuses, a second declaration refuses, an unknown
//! capability name refuses, and an unattributed world touch licenses on
//! any world-bearing platform (the FFI seam).

use ds_check::{DeclaredRows, Effect, EffectRow, WorldCaps, check_platform, effects_of};
use ds_diag::{ErrorCode, Files};
use ds_parse::parse;

/// Checks `sources` against the ambient world tables: rows, capability
/// attribution, and the effects pass.
fn check(sources: &[(&str, &str)]) -> Vec<ds_diag::Diagnostic> {
    let mut files = Files::new();
    let mut asts = Vec::new();
    for (name, source) in sources {
        let file = files.add_source(*name, source);
        let ast = parse(&files, file).unwrap_or_else(|diags| panic!("{name} parses: {diags:?}"));
        asts.push(ast);
    }
    let effects = effects_of(&asts, ds_stdlib::declared_rows(), ds_stdlib::world_caps());
    check_platform(&asts, &effects)
}

/// The one diagnostic of a refused source: its code.
fn refused(sources: &[(&str, &str)]) -> ErrorCode {
    let diags = check(sources);
    let [diag] = &diags[..] else {
        panic!("exactly one diagnostic: {diags:?}");
    };
    diag.code
}

/// The ambient's own declaration licenses every capability: a caller of
/// the whole world surface checks clean against `std`.
#[test]
fn the_ambient_platform_licenses_every_capability() {
    let diags = check(&[
        (ds_stdlib::AMBIENT_FILE, ds_stdlib::AMBIENT_SOURCE),
        ("app.ds", "fn main () { println(\"hi\") }"),
    ]);
    assert_eq!(diags, Vec::new());
}

/// A console-only platform does not license the fsys surface. The rows
/// and attribution arrive by hand (the embedder's way), because a second
/// declaration beside the ambient's is the duplicate refusal.
#[test]
fn an_unprovided_capability_refuses() {
    let src = concat!(
        "extern fn read_file (path: string) -> string\n",
        "platform \"host\" provides <console>\n",
        "fn main () { read_file(\"app.ds\") }\n",
    );
    let rows = DeclaredRows::new().with("read_file", EffectRow::of(Effect::World));
    let caps = WorldCaps::new().with("read_file", "fsys");
    let mut files = Files::new();
    let file = files.add_source("app.ds", src);
    let ast = parse(&files, file).expect("parses");
    let effects = effects_of(&[&ast], &rows, &caps);
    assert_eq!(
        check_platform(&[&ast], &effects)[0].code,
        ErrorCode::PlatformNotProvided
    );
}

/// The capability attribution flows through wrappers: the caller of a
/// wrapper needs the capability its callee's world call names, and a
/// console-only platform licenses the console wrapper while the fsys
/// wrapper refuses.
#[test]
fn the_capability_flows_through_wrappers() {
    let console_wrapper = concat!(
        "fn note (msg: string) -> () { println(msg) }\n",
        "fn main () { note(\"hi\") }\n",
    );
    let diags = check(&[
        (ds_stdlib::AMBIENT_FILE, ds_stdlib::AMBIENT_SOURCE),
        ("app.ds", console_wrapper),
    ]);
    assert_eq!(diags, Vec::new());
    let fsys_wrapper = concat!(
        "extern fn read_file (path: string) -> string\n",
        "platform \"host\" provides <console>\n",
        "fn slurp (p: string) -> string { read_file(p) }\n",
        "fn main () { }\n",
    );
    let rows = DeclaredRows::new().with("read_file", EffectRow::of(Effect::World));
    let caps = WorldCaps::new().with("read_file", "fsys");
    let mut files = Files::new();
    let file = files.add_source("app2.ds", fsys_wrapper);
    let ast = parse(&files, file).expect("parses");
    let effects = effects_of(&[&ast], &rows, &caps);
    assert_eq!(
        check_platform(&[&ast], &effects)[0].code,
        ErrorCode::PlatformNotProvided
    );
}

/// World usage with no platform declaration refuses: the declaration is
/// what the package is checked against, so a self-declared world surface
/// with rows passed by hand (the way an embedder's ambient would) refuses
/// until the package names a platform.
#[test]
fn world_usage_with_no_platform_declaration_refuses() {
    let src = concat!(
        "extern fn say (msg: string) -> ()\n",
        "fn main () { say(\"x\") }\n",
    );
    let rows = DeclaredRows::new().with("say", EffectRow::of(Effect::World));
    let caps = WorldCaps::new().with("say", "console");
    let mut files = Files::new();
    let file = files.add_source("app.ds", src);
    let ast = parse(&files, file).expect("parses");
    let effects = effects_of(&[&ast], &rows, &caps);
    assert_eq!(
        check_platform(&[&ast], &effects)[0].code,
        ErrorCode::PlatformMissing
    );
}

/// A second platform declaration refuses: one package, one platform.
#[test]
fn a_duplicate_platform_declaration_refuses() {
    assert_eq!(
        refused(&[
            (ds_stdlib::AMBIENT_FILE, ds_stdlib::AMBIENT_SOURCE),
            ("app.ds", "platform \"embed\""),
        ]),
        ErrorCode::PlatformDuplicate,
    );
}

/// A capability outside the world vocabulary refuses at the declaration.
#[test]
fn an_unknown_platform_capability_refuses() {
    let mut files = Files::new();
    let file = files.add_source("app.ds", "platform \"std\" provides <console, telepathy>\n");
    let ast = parse(&files, file).expect("parses");
    let effects = effects_of(&[&ast], &DeclaredRows::new(), &WorldCaps::new());
    assert_eq!(
        check_platform(&[&ast], &effects)[0].code,
        ErrorCode::PlatformUnknownCapability
    );
}

/// An unattributed world touch (an extern whose declared row carries
/// `world` with no capability entry) licenses on any world-bearing
/// platform and refuses on one that provides no world surface.
#[test]
fn an_unattributed_world_touch_needs_a_world_bearing_platform() {
    let src = concat!(
        "extern fn hook () -> ()\n",
        "platform \"host\"\n",
        "fn main () { hook() }\n",
    );
    let world = DeclaredRows::new().with("hook", EffectRow::of(Effect::World));
    let mut files = Files::new();
    let file = files.add_source("app.ds", src);
    let ast = parse(&files, file).expect("parses");
    let effects = effects_of(&[&ast], &world, &WorldCaps::new());
    assert!(
        check_platform(&[&ast], &effects)
            .iter()
            .any(|d| d.code == ErrorCode::PlatformNotProvided),
        "a provides-nothing platform licenses no world usage"
    );
    let src = concat!(
        "extern fn hook () -> ()\n",
        "platform \"host\" provides <console>\n",
        "fn main () { hook() }\n",
    );
    let file = files.add_source("app2.ds", src);
    let ast = parse(&files, file).expect("parses");
    let effects = effects_of(&[&ast], &world, &WorldCaps::new());
    assert!(
        check_platform(&[&ast], &effects).is_empty(),
        "any world-bearing platform licenses an unattributed world touch"
    );
}

/// The same unattributed world touch with no platform declaration at all
/// refuses under the missing-declaration code.
#[test]
fn unattributed_world_usage_with_no_declaration_refuses() {
    let src = concat!("extern fn hook () -> ()\n", "fn main () { hook() }\n");
    let world = DeclaredRows::new().with("hook", EffectRow::of(Effect::World));
    let mut files = Files::new();
    let file = files.add_source("app.ds", src);
    let ast = parse(&files, file).expect("parses");
    let effects = effects_of(&[&ast], &world, &WorldCaps::new());
    assert_eq!(
        check_platform(&[&ast], &effects)[0].code,
        ErrorCode::PlatformMissing
    );
}
