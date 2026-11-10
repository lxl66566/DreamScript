#![no_main]

// Property: the lexer and parser never panic on arbitrary bytes, valid
// UTF-8 or not. The scanner's depth guard, the two-span bracket/string
// diagnostics, and the grammar's own failure reporting must all behave;
// a panic, debug-assert trip, or hang is a parser bug. Both arms are
// consumed so coverage counts accepted and rejected inputs alike.
use ds_diag::Files;
use ds_parse::parse;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let source = String::from_utf8_lossy(data);
    let mut files = Files::new();
    let file = files.add_source("fuzz.ds", &source);
    match parse(&files, file) {
        Ok(ast) => {
            std::hint::black_box((
                ast.blocks.len(),
                ast.blocks.first().map_or(0, |b| b.items.len()),
            ));
        },
        Err(diags) => {
            std::hint::black_box((diags.len(), diags.first().map_or(0, |d| d.labels.len())));
        },
    }
});
