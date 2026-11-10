//! The one-byte width ceilings as typed refusals: a call staging more
//! than 255 words (the overlay addressing and every nargs conversion
//! are u8) refuses with the frame error instead of panicking the
//! compiler, on both the pool-call and callable-value paths, and a
//! module minting more tag ids than the space below the platform's
//! reserved top leaves refuses instead of wrapping into the reserved
//! range where TAG_TEST dispatch would alias a platform discriminant.

use std::fmt::Write;

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, lower};
use ds_mod::Hash128;
use ds_parse::parse;

/// parse -> check -> lower, asserting the package checks clean; the
/// lowering outcome is the caller's to inspect.
fn lower_clean(name: &str, source: &str) -> Result<(), LowerError> {
    let mut files = Files::new();
    let id = files.add_source(format!("{name}.ds").as_str(), source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
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
    lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed).map(|_| ())
}

/// A direct call staging 256 words from one binding: repeated
/// argument references stage without per-argument temps, so nothing
/// but the staging guard stands between the span and the u8
/// conversions.
fn wide_direct_source() -> String {
    let args = (0..256)
        .map(|_| "s".to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let params = (0..256)
        .map(|i| format!("a{i}: int"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "tag int\nfn caller(s: int) -> int {{ sink({args}) }}\nfn sink({params}) -> int {{ a0 \
         }}\nfn main() -> int {{ caller(1) }}\n"
    )
}

/// The same call through a lambda value: the CALLS staging path
/// builds the same 256-word span.
fn wide_callable_source() -> String {
    let args = (0..256)
        .map(|_| "s".to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let params = (0..256)
        .map(|i| format!("a{i}: int"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "tag int\nfn caller(s: int) -> int {{\n  const f = ({params}) => a0\n  f({args})\n}}\nfn \
         main() -> int {{ caller(1) }}\n"
    )
}

/// `count` unit tags whose one union return mints them all, in
/// declaration order.
fn many_atoms_source(count: usize) -> String {
    let mut source = String::from("tag int\n");
    for i in 0..count {
        let _ = writeln!(source, "tag m{i}");
    }
    let members = (0..count)
        .map(|i| format!("m{i}"))
        .collect::<Vec<_>>()
        .join(" | ");
    let _ = writeln!(source, "fn u() -> {members} {{ m0 }}");
    let _ = writeln!(source, "fn main() -> int {{ 0 }}");
    source
}

/// A 256-word staged span is a typed FrameTooWide on the direct call
/// path, never a panic in the Mov or nargs conversion.
#[test]
fn a_direct_call_staging_past_255_words_refuses_with_the_frame_error() {
    let Err(err) = lower_clean("wide_direct", &wide_direct_source()) else {
        panic!("a 256-word staging must refuse")
    };
    assert!(
        matches!(err, LowerError::FrameTooWide { .. }),
        "the staged width is a frame fact, not a crash: {err}"
    );
}

/// The same refusal on the callable-value (CALLS) staging path.
#[test]
fn a_callable_value_call_staging_past_255_words_refuses_with_the_frame_error() {
    let Err(err) = lower_clean("wide_callable", &wide_callable_source()) else {
        panic!("a 256-word staging must refuse")
    };
    assert!(
        matches!(err, LowerError::FrameTooWide { .. }),
        "the staged width is a frame fact, not a crash: {err}"
    );
}

/// The tag mint ceiling: 250 distinct atoms fill the ids below the
/// platform's reserved top exactly and lower; the 251st mint refuses
/// with a typed error naming the space, never a silent wrap into the
/// reserved range.
#[test]
fn the_tag_mint_ceiling_refuses_past_the_platforms_reserved_top() {
    lower_clean("tags_fit", &many_atoms_source(250))
        .expect("250 atoms exactly fill the space below the reserved top");
    let Err(err) = lower_clean("tags_overflow", &many_atoms_source(251)) else {
        panic!("the 251st tag mint must refuse")
    };
    assert!(
        matches!(err, LowerError::Unsupported { .. }),
        "the tag-id space is a typed refusal: {err}"
    );
    assert_eq!(
        err.to_string(),
        "a union member is outside the M3 runtime contract (the module's distinct tag atoms \
         exceed the tag-id space below the platform's reserved top); the checker accepted it, the \
         uop set does not cover it yet"
    );
}
