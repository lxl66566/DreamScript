//! The grammar runs off the caller's stack: a worker thread owns the dedicated
//! segment, so the caller's stack size never bounds the admitted nesting.

use ds_diag::Files;
use ds_parse::parse;

#[test]
fn nested_source_parses_from_a_small_caller_stack() {
    let handle = std::thread::Builder::new()
        .stack_size(64 * 1024)
        .spawn(|| {
            let src = format!("fn f() {{ {}x{} }}\n", "(".repeat(38), ")".repeat(38));
            let mut files = Files::new();
            let f = files.add_source("t.ds", src.as_str());
            parse(&files, f).expect("parses");
        })
        .expect("spawning a small-stack thread");
    handle
        .join()
        .expect("the parse must not overflow its segment");
}
