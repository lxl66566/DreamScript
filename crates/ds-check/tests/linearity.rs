//! Focused affine-pass tests [C1, C5]: the move/borrow rules at each
//! consuming position, join and loop-back-edge semantics, deinit
//! well-formedness, and the exact two-span golden render.

use std::fmt::Write as _;

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
    "pub tag int\n",
    "pub tag string\n",
    "pub tag bool\n",
    "pub trait Copyable { }\n",
    "pub struct handle: ~Copyable {\n",
    "  fd: int\n",
    "  deinit { drop_handle(self.fd) }\n",
    "}\n",
    "pub extern fn open_handle (p: string) -> handle\n",
    "pub extern fn drop_handle (fd: int) -> ()\n",
    "pub extern fn take_handle (move h: handle) -> int\n",
    "pub extern fn share_handle (h: handle) -> int\n",
    "pub extern fn window_handle (h: mut handle) -> int\n",
);

fn case(body: &str) -> (ds_check::Checked, ds_check::Linearity) {
    check(&format!(
        "{PRELUDE}fn go(p: string) -> int {{\n{body}\n}}\n"
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
        "pub fn go(p: string) -> int {\n",
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
        "{PRELUDE}fn go(p: string) -> int {{\n  var n = share_handle(open_handle(p))\n  n += \
         share_handle(open_handle(p))\n  take_handle(open_handle(p))\n  n\n}}\n"
    );
    let (checked, lin) = check(&source);
    assert!(checked.diags.iter().all(|d| d.code.as_str() != "DS3001"));
    assert!(lin.diags.is_empty(), "{:#?}", lin.diags);
    assert!(lin.drops.is_empty(), "copyable bindings owe no drop");
}

#[test]
fn a_bare_ident_hole_is_a_consuming_use() {
    // `$h` is a value use like `${h}`: reading a moved binding through a
    // hole reports the same moved-then-used diagnostic
    let (checked, lin) = case(concat!(
        "  const h = open_handle(p)\n",
        "  take_handle(h)\n",
        "  const s = \"v=$h\"\n",
        "  0\n",
    ));
    assert!(checked.diags.iter().all(|d| d.code.as_str() == "DS2001"));
    let diag = expect_one(&lin);
    assert_eq!(diag.code.as_str(), "DS3001");
    assert_eq!(diag.message, "value `h` is no longer usable");
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
        "{PRELUDE}struct list[t]\nextern fn single (v: int) -> list[int]\nfn go(p: string) -> int \
         {{\nconst h = open_handle(p)\nvar n = 0\nfor x in single(3) {{\ntake_handle(h)\nn += \
         x\n}}\nn\n
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
        "pub tag int\n",
        "pub trait Copyable { }\n",
        "pub struct a {\n",
        "  x: int\n",
        "  deinit { drop_int(self.x) }\n",
        "}\n",
        "pub struct b: ~Copyable {\n",
        "  x: int\n",
        "  deinit { drop_int(self.x) }\n",
        "  deinit { drop_int(self.x) }\n",
        "}\n",
        "pub extern fn drop_int (x: int) -> ()\n",
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
fn go(p: string) -> int {{
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
        "pub tag int\n",
        "pub trait Copyable { }\n",
        "pub extern fn take[t] (move x: t) -> int\n",
        "pub fn go[t: ~Copyable](move x: t) -> int {\n",
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
        "pub tag int\n",
        "pub fn go[t](x: t) -> int {\n",
        "  take(x)\n",
        "  take(x)\n",
        "}\n",
        "pub extern fn take[t] (move x: t) -> int\n",
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
        "{PRELUDE}const h = open_handle(\"a\")\nfn first() -> int {{ take_handle(h) }}\nfn \
         second() -> int {{ take_handle(h) }}\n"
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
    // h, p, and the prelude deinit's `self` (its body walks like a
    // borrowed receiver's) each record a last use
    assert_eq!(
        lin.last_uses.len(),
        3,
        "h, p, and the deinit's self record last uses"
    );
    let h_use = lin
        .last_uses
        .iter()
        .find(|u| !u.borrowed)
        .expect("h's final use is the consuming one");
    assert_eq!(h_use.at, moved.at, "the consuming use is the last use");
    assert!(lin.last_uses.iter().any(|u| u.borrowed), "p borrows");
}

// ---- consume-mode parameters ---------------------------------------------------

#[test]
fn consume_mode_parameter_double_consume_renders_the_two_span_golden() {
    // A `move` parameter is an initialized-affine binding of the entry
    // state [C2]: the first consuming call moves it, the second is a
    // use-after-move with the same golden as locals.
    let source = format!(
        "{PRELUDE}fn twice(move h: handle) -> int {{\n  take_handle(h)\n  take_handle(h)\n}}\n"
    );
    let (checked, lin) = check(&source);
    assert!(checked.diags.is_empty(), "{:#?}", checked.diags);
    let diag = expect_one(&lin);
    assert_eq!(diag.code.as_str(), "DS3001");
    assert_eq!(diag.message, "value `h` is no longer usable");
    assert_eq!(diag.labels.len(), 2);
    assert_eq!(diag.labels[0].label, "moved here");
    assert_eq!(diag.labels[1].label, "used again here after move");
    assert_eq!(diag.notes, ["`h` is `~Copyable` and was consumed"]);
    assert_eq!(lin.moves.len(), 1, "only the first call is a move");

    let mut files = Files::new();
    files.add_source("t.ds", &source);
    let expected = "error[DS3001]: value `h` is no longer usable
 --> t.ds:16:15
   |
15 |   take_handle(h)
   |               - moved here
   |
16 |   take_handle(h)
   |               ^ used again here after move
   |
   = note: `h` is `~Copyable` and was consumed
";
    assert_eq!(ds_diag::render::human(diag, &files), expected);
}

#[test]
fn consume_mode_parameter_moved_once_owes_no_drop() {
    // The consuming call transfers the parameter's deinit responsibility,
    // so the function end records neither a move nor a drop.
    let (_, lin) = case(concat!(
        "  const twice = (move h: handle) -> int {\n",
        "    take_handle(h)\n",
        "    0\n",
        "  }\n",
        "  twice(open_handle(p))\n",
    ));
    assert!(lin.diags.is_empty(), "{:#?}", lin.diags);
    assert_eq!(lin.moves.len(), 1);
    assert!(lin.drops.is_empty(), "the consumed parameter owes no drop");
}

#[test]
fn consume_mode_parameter_left_live_drops_at_the_body_end() {
    let (_, lin) = case(concat!(
        "  const peek = (move h: handle) -> int {\n",
        "    share_handle(h)\n",
        "  }\n",
        "  peek(open_handle(p))\n",
    ));
    assert!(lin.diags.is_empty(), "{:#?}", lin.diags);
    assert!(
        lin.moves.is_empty(),
        "borrowing uses of the parameter never move"
    );
    assert_eq!(
        lin.drops.len(),
        1,
        "the live owned parameter dies at scope end"
    );
}

#[test]
fn borrow_mode_parameter_stays_non_moving_across_consuming_calls() {
    // C2: a borrow-mode parameter is a caller-retained window, so its
    // consuming call-argument uses never move it statically; the M4
    // runtime rc==1 uniqueness check owns this seam.
    let (_, lin) = case(concat!(
        "  const twice = (h: handle) -> int {\n",
        "    take_handle(h)\n",
        "    take_handle(h)\n",
        "    0\n",
        "  }\n",
        "  twice(open_handle(p))\n",
    ));
    assert!(lin.diags.is_empty(), "{:#?}", lin.diags);
    assert!(lin.moves.is_empty(), "windows never move");
    assert!(
        lin.drops.is_empty(),
        "the caller retains the value, so the callee owes no drop"
    );
}

#[test]
fn a_borrow_use_after_a_consume_of_a_parameter_fires() {
    let (_, lin) = case(concat!(
        "  const twice = (move h: handle) -> int {\n",
        "    take_handle(h)\n",
        "    share_handle(h)\n",
        "    0\n",
        "  }\n",
        "  twice(open_handle(p))\n",
    ));
    let diag = expect_one(&lin);
    assert_eq!(diag.code.as_str(), "DS3001");
    assert_eq!(diag.message, "value `h` is no longer usable");
}

#[test]
fn a_parameter_captured_by_a_lambda_is_moved_before_its_body_use() {
    // The capture at the lambda's use of the parameter is the move [C1];
    // the body's later consuming use reads the dead binding.
    let (_, lin) = case(concat!(
        "  const twice = (move h: handle) -> int {\n",
        "    const peek = () => take_handle(h)\n",
        "    take_handle(h)\n",
        "    0\n",
        "  }\n",
        "  twice(open_handle(p))\n",
    ));
    let diag = expect_one(&lin);
    assert_eq!(diag.code.as_str(), "DS3001");
}

#[test]
fn a_parameter_used_by_a_spawn_body_is_moved_before_its_body_use() {
    let (_, lin) = case(concat!(
        "  const twice = (move h: handle) -> int {\n",
        "    spawn { take_handle(h) }\n",
        "    take_handle(h)\n",
        "    0\n",
        "  }\n",
        "  twice(open_handle(p))\n",
    ));
    let diag = expect_one(&lin);
    assert_eq!(diag.code.as_str(), "DS3001");
}

#[test]
fn every_match_arm_moving_a_parameter_kills_it_for_the_tail_use() {
    // Each arm runs once, so its own move is fine; the join keeps the
    // binding dead, and the use after the match is a use-after-move.
    let (_, lin) = case(concat!(
        "  const twice = (move h: handle, k: int) -> int {\n",
        "    match k {\n",
        "      0 => {\n        take_handle(h)\n        1\n      },\n",
        "      _ => {\n        take_handle(h)\n        2\n      },\n",
        "    }\n",
        "    h.fd\n",
        "  }\n",
        "  twice(open_handle(p), 0)\n",
    ));
    let diag = expect_one(&lin);
    assert_eq!(diag.code.as_str(), "DS3001");
    assert_eq!(diag.message, "value `h` is no longer usable");
}

#[test]
fn every_match_arm_moving_a_parameter_once_passes_without_a_tail_use() {
    let (_, lin) = case(concat!(
        "  const twice = (move h: handle, k: int) -> int {\n",
        "    match k {\n",
        "      0 => {\n        take_handle(h)\n        1\n      },\n",
        "      _ => {\n        take_handle(h)\n        2\n      },\n",
        "    }\n",
        "  }\n",
        "  twice(open_handle(p), 0)\n",
    ));
    assert!(lin.diags.is_empty(), "{:#?}", lin.diags);
    assert_eq!(lin.moves.len(), 2, "each arm's call is one move");
    assert!(lin.drops.is_empty(), "the consumed parameter owes no drop");
}

// ---- field projections and destructuring re-binds ---------------------------------

#[test]
fn projecting_a_linear_field_kills_the_receiver_binding() {
    // `const inner = w.h` transfers w; a later projection reads the dead
    // receiver, not just the extracted field.
    let source = format!(
        "{PRELUDE}struct holder {{
  h: handle
}}
extern fn open_holder (p: string) -> holder
fn go(p: string) -> int {{
  const w = open_holder(p)
  const inner = w.h
  take_handle(inner)
  take_handle(w.h)
  0
}}
"
    );
    let (_, lin) = check(&source);
    let diag = expect_one(&lin);
    assert_eq!(diag.code.as_str(), "DS3001");
    assert_eq!(diag.message, "value `w` is no longer usable");
    assert_eq!(
        lin.moves.len(),
        2,
        "w.h projects out twice: first is a move"
    );
}

#[test]
fn projecting_a_copyable_field_keeps_the_receiver_usable() {
    let (_, lin) = case(concat!(
        "  const h = open_handle(p)\n",
        "  const fd = h.fd\n",
        "  take_handle(h)\n",
        "  fd\n",
    ));
    assert!(lin.diags.is_empty(), "{:#?}", lin.diags);
    assert_eq!(lin.moves.len(), 1, "only the consuming call moves h");
}

#[test]
fn destructuring_after_a_projection_move_fires_on_the_source() {
    let source = format!(
        "{PRELUDE}struct pair {{
  a: handle
  b: handle
}}
extern fn open_pair (p: string) -> pair
fn go(p: string) -> int {{
  const q = open_pair(p)
  take_handle(q.a)
  const pair {{ a, b }} = q
  take_handle(b)
  0
}}
"
    );
    let (_, lin) = check(&source);
    let diag = expect_one(&lin);
    assert_eq!(diag.code.as_str(), "DS3001");
    assert_eq!(diag.message, "value `q` is no longer usable");
}

#[test]
fn destructuring_moves_the_source_into_its_pattern_binders() {
    let source = format!(
        "{PRELUDE}struct pair {{
  a: handle
  b: handle
}}
extern fn open_pair (p: string) -> pair
fn go(p: string) -> int {{
  const q = open_pair(p)
  const pair {{ a, b }} = q
  take_handle(a)
  take_handle(b)
  0
}}
"
    );
    let (_, lin) = check(&source);
    assert!(lin.diags.is_empty(), "{:#?}", lin.diags);
    // The destructure moves q; each field binder is consumed once, so no
    // drop is owed at scope end.
    assert_eq!(lin.moves.len(), 3, "q into the pattern, a and b out");
    assert!(lin.drops.is_empty(), "every moved-out binder was consumed");
}

#[test]
fn a_lambda_parameter_is_a_local_not_a_capture_of_itself() {
    // Parameters bind inside the lambda's capture boundary, so a
    // borrowing use followed by a consuming use of one `move` parameter
    // is clean, and the parameter left live dies at the body end.
    let (_, lin) = case(concat!(
        "  const use = (move h: handle) -> int {\n",
        "    share_handle(h)\n",
        "    take_handle(h)\n",
        "    0\n",
        "  }\n",
        "  use(open_handle(p))\n",
    ));
    assert!(lin.diags.is_empty(), "{:#?}", lin.diags);
    assert_eq!(lin.moves.len(), 1, "only the consuming call moves h");
    assert!(lin.drops.is_empty(), "the consumed parameter owes no drop");

    let (_, lin) = case(concat!(
        "  const peek = (move h: handle) -> int {\n",
        "    share_handle(h)\n",
        "  }\n",
        "  peek(open_handle(p))\n",
    ));
    assert!(lin.diags.is_empty(), "{:#?}", lin.diags);
    assert!(
        lin.moves.is_empty(),
        "a borrowing use of the parameter never moves"
    );
    assert_eq!(lin.drops.len(), 1, "a live parameter dies at the body end");
}

#[test]
fn a_parameter_still_captures_across_nested_lambdas() {
    // The capture rule keeps its outer direction: an fn parameter moved
    // into the body of a lambda inside a lambda is one capture move.
    let (_, lin) = case(concat!(
        "  const twice = (move h: handle) -> int {\n",
        "    const g = () => () => take_handle(h)\n",
        "    0\n",
        "  }\n",
        "  twice(open_handle(p))\n",
    ));
    assert!(lin.diags.is_empty(), "{:#?}", lin.diags);
    assert_eq!(lin.moves.len(), 1, "the innermost body use is the capture");
}

#[test]
fn calling_a_consumed_capture_closure_twice_is_currently_unaudited() {
    // Closure call cardinality is an open surface decision (an Fn-vs-once
    // contract): the body walks once at its source position, so the
    // capture move happens there and re-executing the body through a
    // second call is unaudited. This pins that behavior; the decision
    // lands as a diff against it, not as a discovery.
    let (checked, lin) = case(concat!(
        "  const h = open_handle(p)\n",
        "  const c = (x: int) -> int { take_handle(h) }\n",
        "  c(0)\n",
        "  c(0)\n",
        "  0\n",
    ));
    assert!(checked.diags.is_empty(), "{:#?}", checked.diags);
    assert!(lin.diags.is_empty(), "{:#?}", lin.diags);
    assert_eq!(
        lin.moves.len(),
        1,
        "the capture moves once, at the lambda body"
    );
    assert!(
        lin.drops.is_empty(),
        "the captured binding is dead, not dropped"
    );
}

#[test]
fn cross_file_top_level_consumption_is_order_independent() {
    // A body in file A consuming file B's top-level binding: every
    // file's top-levels come live before any body walks, so the move
    // and the use-after-move fire with the consuming file processed
    // first [G3] — a per-file interleave would silently skip them.
    let mut files = Files::new();
    let a = files.add_source(
        "a.ds",
        concat!(
            "pub fn go() -> int {\n",
            "  take_handle(shared)\n",
            "  take_handle(shared)\n",
            "  0\n",
            "}\n",
        ),
    );
    let b = files.add_source(
        "b.ds",
        &format!("{PRELUDE}pub const shared = open_handle(\"x\")\n"),
    );
    let ast_a = parse(&files, a).unwrap_or_else(|d| panic!("a parses: {d:?}"));
    let ast_b = parse(&files, b).unwrap_or_else(|d| panic!("b parses: {d:?}"));
    let (checked, lin) = linearity::check(&files, &[ast_a, ast_b]);
    assert!(checked.diags.is_empty(), "{:#?}", checked.diags);
    let diag = expect_one(&lin);
    assert_eq!(diag.code.as_str(), "DS3001");
    assert_eq!(diag.message, "value `shared` is no longer usable");
}

/// The grammar-based generator round found the loop audit compounding: a
/// loop's audit round re-walks its body, and a nested loop ran its own
/// round inside the outer audit, doubling the walk per nesting level —
/// checking a 26-deep nested loop took ~8s, a 30-deep generated source
/// ~80s, and depth 40 would take days. The audit walk mutates nothing
/// (binds, reads, and drops all no-op or dedup), so a nested round inside
/// an outer audit is a pure duplicate re-check; it is skipped, and the
/// walk stays linear in program size. This pins boundedness by
/// completion: before the fix this depth did not finish.
#[test]
fn deeply_nested_loops_finish() {
    for kind in [Kind::For, Kind::While] {
        let mut src = String::from("tag int\nfn deep() -> int {\n");
        let mut closers = String::new();
        for d in 0..30 {
            let indent = (d + 1) * 2;
            let head = match kind {
                Kind::For => format!("for i{d} in 0..2 {{"),
                Kind::While => format!(
                    "var i{d}: int = 0\n{spaces}while i{d} < 2 {{",
                    spaces = " ".repeat(indent)
                ),
            };
            writeln!(src, "{spaces}{head}", spaces = " ".repeat(indent))
                .expect("String writes are infallible");
            let step = match kind {
                Kind::For => format!("const x{d}: int = i{d}"),
                Kind::While => format!("i{d} += 1"),
            };
            writeln!(src, "{spaces}{step}", spaces = " ".repeat(indent + 2))
                .expect("String writes are infallible");
            closers.insert_str(0, &format!("{} }}\n", " ".repeat(indent - 2)));
        }
        src.push_str(&closers);
        src.push_str("  0\n");
        src.push_str("}\n");
        let (checked, linearity) = check(&src);
        assert!(checked.diags.is_empty(), "{:#?}", checked.diags);
        assert_eq!(linearity.moves.len(), 0);
    }
}

enum Kind {
    For,
    While,
}
