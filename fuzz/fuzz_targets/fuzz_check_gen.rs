#![no_main]

// Property: the checker pipeline never panics on grammar-generated
// programs. Fuzz bytes drive a structural DS source (declarations,
// statements, expressions over the checked surface, biased to the
// well-typed fragment with occasional wrong splices) — the shared
// generator lives in `check_gen.rs`, exercised here through parse, the
// effects walk over rows the generator picked, the bidirectional check
// with the affine pass, and both ds-diag renderers. Byte noise rarely
// reaches deep checker states; generation does. A nextest-runnable
// mirror lives in crates/ds-check/tests/fuzz_check_gen.rs.
#[path = "../check_gen.rs"]
mod check_gen;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let outcome = check_gen::exercise(data);
    std::hint::black_box((outcome.parsed, outcome.moves, outcome.effects));
});
