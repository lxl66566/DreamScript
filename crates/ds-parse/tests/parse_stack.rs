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

/// The budgets' claimed-safe worst source parses to completion on the dev
/// profile: 39 open parens (the scanner's `MAX_NEST - 1`) plus 39 arrow
/// layers (`MAX_ARROWS - 1`), the deepest nesting the two budgets admit
/// together. Conversion frames on the dev profile are what the worker
/// segment is sized against.
#[test]
fn worst_admitted_arrow_nesting_parses_to_completion() {
    let src = format!(
        "fn f() -> int {{\n  {}{}1{}\n}}\n",
        "(".repeat(39),
        "x => ".repeat(39),
        ")".repeat(39),
    );
    let mut files = Files::new();
    let f = files.add_source("worst.ds", src.as_str());
    parse(&files, f).expect("the worst admitted arrow depth parses");
}

/// The deepest combined source the budgets admit: the fn body's brace
/// takes one bracket slot, so 38 parens leave the one open block an
/// `else if` chain holds, over 39 arrow layers and 499 live elses.
#[test]
fn worst_admitted_combined_nesting_parses_to_completion() {
    use std::fmt::Write as _;
    let mut chain = String::from("if c { 0 }");
    for k in 1..499 {
        let _ = write!(chain, "\nelse if c {{ {k} }}");
    }
    chain.push_str("\nelse { 0 }");
    let src = format!(
        "fn f(c: bool) -> int {{\n  {}{}{}{}\n}}\n",
        "(".repeat(38),
        "x => ".repeat(39),
        chain,
        ")".repeat(38),
    );
    let mut files = Files::new();
    let f = files.add_source("worst.ds", src.as_str());
    parse(&files, f).expect("the worst admitted combined depth parses");
}
