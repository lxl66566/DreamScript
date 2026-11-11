#![no_main]

// Property: the full checker pipeline never panics on arbitrary source
// bytes: parse, then the bidirectional check with the affine pass, then
// the effects walk, then both ds-diag renderers over every diagnostic
// produced along the way. Semantic wrongness is fine (diagnostics exist
// for that); a panic, debug-assert trip, or hang is not. Declared ambient
// rows derived from the input exercise `effects_of`'s union edges, not
// just the empty-row walk.
use ds_check::{DeclaredRows, Effect, EffectRow, check, effects_of};
use ds_diag::{Files, render};
use ds_parse::parse;
use libfuzzer_sys::fuzz_target;

/// Declares up to 8 ambient rows named after ASCII identifier-shaped runs
/// in the input, so generated sources have a real chance of calling
/// row-declared names. Row bits select effects from the 5-effect
/// vocabulary.
fn declared_rows(data: &[u8]) -> DeclaredRows {
    const EFFECTS: [Effect; 5] = [
        Effect::Error,
        Effect::Trap,
        Effect::World,
        Effect::Cancel,
        Effect::Discontinue,
    ];
    let mut declared = DeclaredRows::new();
    let mut names = 0;
    let mut run_start = None;
    for (i, &byte) in data.iter().chain(std::iter::once(&0)).enumerate() {
        let ident = byte.is_ascii_alphanumeric() || byte == b'_';
        if ident && run_start.is_none() {
            run_start = Some(i);
        }
        if !ident
            && let Some(start) = run_start.take()
            && i - start <= 8
            && names < 8
        {
            names += 1;
            let name = String::from_utf8_lossy(&data[start..i]).into_owned();
            let mut row = EffectRow::PURE;
            for (bit, effect) in EFFECTS.iter().enumerate() {
                if data[start.midpoint(i)] >> bit & 1 == 1 {
                    row = row.union(EffectRow::of(*effect));
                }
            }
            declared = declared.with(&name, row);
        }
    }
    declared
}

fuzz_target!(|data: &[u8]| {
    let source = String::from_utf8_lossy(data);
    let mut files = Files::new();
    let file = files.add_source("fuzz.ds", &source);
    let ast = match parse(&files, file) {
        Ok(ast) => ast,
        Err(diags) => {
            // Parse diagnostics render too: the renderer must survive
            // real pipeline output, not just hand-built fixtures.
            for diag in &diags {
                std::hint::black_box(render::human(diag, &files));
            }
            return;
        },
    };
    let effects = effects_of(&[&ast], &declared_rows(data));
    let (checked, linearity) = check(&files, &[ast]);
    for diag in &checked.diags {
        std::hint::black_box(render::human(diag, &files));
    }
    std::hint::black_box((
        checked.diags.len(),
        linearity.moves.len(),
        effects.def_count(),
    ));
});
