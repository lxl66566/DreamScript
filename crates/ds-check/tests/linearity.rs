//! Focused affine-pass tests [C1, C5]: the move/borrow rules at each
//! consuming position, join and loop-back-edge semantics, deinit
//! well-formedness, and the exact two-span golden render.

use ds_check::linearity;
use ds_diag::{Diagnostic, Files};
use ds_parse::parse;

/// Parses `source` alone and runs both passes over it.
fn check(source: &str) -> (ds_check::Checked, ds_check::Linearity) {
    let mut files = Files::new();
    let file = files.add_source("t.ds", source);
    let asts = [parse(&files, file).unwrap_or_else(|d| panic!("parse failed: {d:?}"))];
    linearity::check(&files, &asts)
}

/// The shared vocabulary of the linear cases.
const PRELUDE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "tag bool\n",
    "trait Copyable { }\n",
    "struct handle: ~Copyable {\n",
    "  fd: int\n",
    "  deinit { drop_handle(self.fd) }\n",
    "}\n",
    "extern fn open_handle (p: string) -> handle\n",
    "extern fn drop_handle (fd: int) -> ()\n",
    "extern fn take_handle (move h: handle) -> int\n",
    "extern fn share_handle (h: handle) -> int\n",
    "extern fn window_handle (h: mut handle) -> int\n",
);

fn case(body: &str) -> (ds_check::Checked, ds_check::Linearity) {
    check(&format!(
        "{PRELUDE}const go = (p: string) -> int {{\n{body}\n}}\n"
    ))
}

fn expect_one(lin: &ds_check::Linearity) -> &Diagnostic {
    let [only] = lin.diags.as_slice() else {
        panic!("expected exactly one diagnostic, got {:#?}", lin.diags);
    };
    only
}

#[test]
fn moved_then_used_renders_the_two_span_golden() {
    let body = concat!(
        "const go = (p: string) -> int {\n",
        "  const h = open_handle(p)\n",
        "  take_handle(h)\n",
        "  take_handle(h)\n",
        "  0\n",
        "}\n",
    );
    let source = format!("{PRELUDE}{body}");
    let (checked, lin) = check(&source);
    assert!(checked.diags.is_empty(), "{:#?}", checked.diags);
    let diag = expect_one(&lin);
    assert_eq!(diag.code.as_str(), "DS3001");
    assert_eq!(diag.message, "value `h` is no longer usable");
    assert_eq!(diag.labels.len(), 2);
    assert_eq!(diag.labels[0].label, "moved here");
    assert_eq!(diag.labels[1].label, "used again here after move");
    assert_eq!(diag.notes, ["`h` is `~Copyable` and was consumed"]);

    // The render matches the registry's golden shape exactly.
    let mut files = Files::new();
    files.add_source("t.ds", &source);
    let expected = "error[DS3001]: value `h` is no longer usable
 --> t.ds:17:15
   |
16 |   take_handle(h)
   |               - moved here
   |
17 |   take_handle(h)
   |               ^ used again here after move
   |
   = note: `h` is `~Copyable` and was consumed
";
    assert_eq!(ds_diag::render::human(diag, &files), expected);
}

#[test]
fn copyable_bindings_reuse_freely() {
    // `int` and `string` are tags, and tags copy: two consuming calls on
    // one binding are two copies, and no drop is owed.
    let source = format!(
        "{PRELUDE}const go = (p: string) -> int {{\n  var n = share_handle(open_handle(p))\n  n \
         += share_handle(open_handle(p))\n  take_handle(open_handle(p))\n  n\n}}\n"
    );
    let (checked, lin) = check(&source);
    assert!(checked.diags.iter().all(|d| d.code.as_str() != "DS3001"));
    assert!(lin.diags.is_empty(), "{:#?}", lin.diags);
    assert!(lin.drops.is_empty(), "copyable bindings owe no drop");
}

#[test]
fn mut_windows_do_not_consume_the_caller_binding() {
    // C2: borrow and `mut` are call-scoped windows; the caller keeps its
    // value, so the same binding can window again in every iteration.
    let (_, lin) = case(concat!(
        "  const h = open_handle(p)\n",
        "  var n = 0\n",
        "  while n < 3 {\n",
        "    n += window_handle(h)\n",
        "    n += share_handle(h)\n",
        "  }\n",
        "  n\n",
    ));
    assert!(lin.diags.is_empty(), "{:#?}", lin.diags);
    assert_eq!(lin.moves.len(), 0, "windows never move");
    // h still dies live at scope end: the deinit site.
    assert_eq!(lin.drops.len(), 1);
}

#[test]
fn consume_mode_parameter_used_twice_inside_its_body() {
    let (_, lin) = case(concat!(
        "  const f = (move h: handle) -> int {\n",
        "    take_handle(h)\n",
        "    take_handle(h)\n",
        "  }\n",
        "  f(open_handle(p))\n",
    ));
    let diag = expect_one(&lin);
    assert_eq!(diag.code.as_str(), "DS3001");
}

#[test]
fn lambda_capture_moves_the_binding() {
    let (_, lin) = case(concat!(
        "  const h = open_handle(p)\n",
        "  const peek = () => h.fd\n",
        "  take_handle(h)\n",
        "  0\n",
    ));
    let diag = expect_one(&lin);
    assert_eq!(diag.code.as_str(), "DS3001");
    assert_eq!(diag.message, "value `h` is no longer usable");
}

#[test]
fn var_reassignment_reinitializes_and_drops_the_replaced_value() {
    let (_, lin) = case(concat!(
        "  var h = open_handle(p)\n",
        "  take_handle(h)\n",
        "  h = open_handle(p)\n",
        "  h = open_handle(p)\n",
        "  take_handle(h)\n",
        "  0\n",
    ));
    assert!(lin.diags.is_empty(), "{:#?}", lin.diags);
    // The first reassignment re-initializes a dead binding: no drop. The
    // second replaces a live value: the death is the assignment itself.
    assert_eq!(lin.moves.len(), 2, "{:#?}", lin.moves);
    assert_eq!(lin.drops.len(), 1, "{:#?}", lin.drops);
}

#[test]
fn a_move_in_one_branch_kills_the_binding_after_the_join() {
    let (_, lin) = case(concat!(
        "  const h = open_handle(p)\n",
        "  const r = if h.fd > 0 {\n",
        "    take_handle(h)\n",
        "    1\n",
        "  } else {\n",
        "    2\n",
        "  }\n",
        "  share_handle(h)\n",
        "  r\n",
    ));
    let diag = expect_one(&lin);
    assert_eq!(diag.code.as_str(), "DS3001");
}

#[test]
fn a_move_in_a_diverging_branch_leaves_the_join_live() {
    let (_, lin) = case(concat!(
        "  const h = open_handle(p)\n",
        "  const r = if h.fd > 0 {\n",
        "    take_handle(h)\n",
        "    return 1\n",
        "  } else {\n",
        "    0\n",
        "  }\n",
        "  share_handle(h)\n",
        "  r\n",
    ));
    assert!(lin.diags.is_empty(), "{:#?}", lin.diags);
    // The then path exits; only the else path reaches the join, where h
    // is still live and dies at scope end.
    assert_eq!(lin.drops.len(), 1);
}

#[test]
fn a_move_inside_a_loop_reports_the_re_executing_path() {
    let (_, lin) = case(concat!(
        "  const h = open_handle(p)\n",
        "  var n = 0\n",
        "  while n < 3 {\n",
        "    take_handle(h)\n",
        "    n += 1\n",
        "  }\n",
        "  n\n",
    ));
    let diag = expect_one(&lin);
    assert_eq!(diag.code.as_str(), "DS3001");
    assert_eq!(
        diag.message,
        "value `h` is moved again on a re-executing path"
    );
    assert_eq!(diag.labels.len(), 1, "the loop move names its own site");
}

#[test]
fn a_move_inside_a_for_body_reports_the_re_executing_path() {
    let source = format!(
        "{PRELUDE}struct list[t]\nextern fn single (v: int) -> list[int]\nconst go = (p: string) \
         -> int {{\nconst h = open_handle(p)\nvar n = 0\nfor x in single(3) {{\ntake_handle(h)\nn \
         += x\n}}\nn\n
}}\n"
    );
    let (_, lin) = check(&source);
    let diag = expect_one(&lin);
    assert_eq!(diag.code.as_str(), "DS3001");
    assert_eq!(
        diag.message,
        "value `h` is moved again on a re-executing path"
    );
}

#[test]
fn a_reinitialized_var_in_a_loop_reexecutes_cleanly() {
    let (_, lin) = case(concat!(
        "  var n = 0\n",
        "  while n < 3 {\n",
        "    var h = open_handle(p)\n",
        "    take_handle(h)\n",
        "    n += 1\n",
        "  }\n",
        "  n\n",
    ));
    assert!(lin.diags.is_empty(), "{:#?}", lin.diags);
}

#[test]
fn deinit_on_a_copyable_struct_is_ill_formed_and_twice_is_rejected() {
    let source = concat!(
        "tag int\n",
        "trait Copyable { }\n",
        "struct a {\n",
        "  x: int\n",
        "  deinit { drop_int(self.x) }\n",
        "}\n",
        "struct b: ~Copyable {\n",
        "  x: int\n",
        "  deinit { drop_int(self.x) }\n",
        "  deinit { drop_int(self.x) }\n",
        "}\n",
        "extern fn drop_int (x: int) -> ()\n",
    );
    let (checked, _) = check(source);
    assert!(checked.diags.is_empty(), "{:#?}", checked.diags);
    let mut files = Files::new();
    let file = files.add_source("t.ds", source);
    let asts = [parse(&files, file).unwrap()];
    let (_, lin) = linearity::check(&files, &asts);
    // `a` carries one deinit outside `~Copyable`; `b`'s second deinit
    // breaks the at-most-one rule.
    assert_eq!(lin.diags.len(), 2, "{:#?}", lin.diags);
    assert!(lin.diags.iter().any(|d| d.code.as_str() == "DS3004"
        && d.message == "deinit is restricted to `~Copyable` types [C5]"));
    assert!(lin.diags.iter().any(|d| d.code.as_str() == "DS3004"
        && d.message == "a struct declares at most one `deinit` [C5]"));
}

#[test]
fn a_struct_with_a_linear_field_is_itself_linear() {
    let source = format!(
        "{PRELUDE}struct wrapper {{
  h: handle
}}
const go = (p: string) -> int {{
  const w = wrapper {{ h: open_handle(p) }}
  take_wrapper(w)
  take_wrapper(w)
  0
}}
extern fn take_wrapper (move w: wrapper) -> int
"
    );
    let (_, lin) = check(&source);
    let diag = expect_one(&lin);
    assert_eq!(diag.code.as_str(), "DS3001");
    assert_eq!(diag.message, "value `w` is no longer usable");
}

#[test]
fn linear_generic_parameter_flows_from_the_marker_bound() {
    let source = concat!(
        "tag int\n",
        "trait Copyable { }\n",
        "extern fn take[t] (move x: t) -> int\n",
        "const go = [t: ~Copyable] (move x: t) -> int {\n",
        "  take(x)\n",
        "  take(x)\n",
        "}\n",
    );
    let (_, lin) = check(source);
    let diag = expect_one(&lin);
    assert_eq!(diag.code.as_str(), "DS3001");
    assert_eq!(diag.message, "value `x` is no longer usable");
}

#[test]
fn a_copyable_generic_parameter_flows_from_the_default() {
    let source = concat!(
        "tag int\n",
        "const go = [t] (x: t) -> int {\n",
        "  take(x)\n",
        "  take(x)\n",
        "}\n",
        "extern fn take[t] (move x: t) -> int\n",
    );
    let (_, lin) = check(source);
    assert!(lin.diags.is_empty(), "{:#?}", lin.diags);
}

#[test]
fn tuple_elements_consume_and_the_second_use_of_one_binding_fails() {
    let (_, lin) = case(concat!(
        "  const h = open_handle(p)\n",
        "  const pair = (h, h)\n",
        "  0\n",
    ));
    let diag = expect_one(&lin);
    assert_eq!(diag.code.as_str(), "DS3001");
}

#[test]
fn structural_equality_and_is_borrow_their_operands() {
    let (_, lin) = case(concat!(
        "  const h = open_handle(p)\n",
        "  const same = h == open_handle(p)\n",
        "  const what = h is handle\n",
        "  take_handle(h)\n",
        "  0\n",
    ));
    assert!(lin.diags.is_empty(), "{:#?}", lin.diags);
    assert_eq!(lin.moves.len(), 1, "only the final take moves");
}

#[test]
fn a_top_level_linear_definition_consumed_twice_across_items() {
    let source = format!(
        "{PRELUDE}const h = open_handle(\"a\")\nconst first = () -> int {{ take_handle(h) \
         }}\nconst second = () -> int {{ take_handle(h) }}\n"
    );
    let (_, lin) = check(&source);
    let diag = expect_one(&lin);
    assert_eq!(diag.code.as_str(), "DS3001");
    assert_eq!(diag.message, "value `h` is no longer usable");
}

#[test]
fn last_use_positions_report_borrowed_and_consuming_final_uses() {
    let (_, lin) = case(concat!(
        "  const h = open_handle(p)\n",
        "  share_handle(h)\n",
        "  take_handle(h)\n",
        "  0\n",
    ));
    assert_eq!(lin.moves.len(), 1);
    let moved = &lin.moves[0];
    assert_eq!(lin.last_uses.len(), 2, "h and p each record a last use");
    let h_use = lin
        .last_uses
        .iter()
        .find(|u| !u.borrowed)
        .expect("h's final use is the consuming one");
    assert_eq!(h_use.at, moved.at, "the consuming use is the last use");
    assert!(lin.last_uses.iter().any(|u| u.borrowed), "p borrows");
}
