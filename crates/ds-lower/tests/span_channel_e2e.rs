//! The span rework's native channel, end to end: a `list[record]`
//! element pushes its whole inline word span (the per-list element
//! stride the first push fixes), the checked `list_get` read answers
//! through the span channel — one payload word per element word, a
//! count per `Ref` word — and the ok side re-spans into the call's own
//! `record | none` union, extracted by match, `.ok`, and `unwrap_or`.
//! The refusal openings that stay keep honest names naming the seam
//! that still owns them.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// Parse -> check -> lower -> round-trip -> run, returning `main`'s
/// result value. Panics with the checker's renderings when the source
/// does not check clean, and with the lowering error when it does not
/// lower.
fn runs(name: &str, source: &str) -> FrameAnswer {
    let mut files = Files::new();
    let id = files.add_source("probe.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "{}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed)
        .unwrap_or_else(|err| panic!("the probe lowers: {err}"));
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    let answer = vm
        .run(name, "main", &[])
        .unwrap_or_else(|trap| panic!("the probe runs: {trap:?}"));
    // the entry's own frame must end in a pure expression (a native tail
    // at the root frame abandons staged counts by design, todo's
    // recorded leak class), so a drained heap is the Ref-word balance
    // assertion across the pushes and read-outs
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
    answer
}

/// Same pipeline, expecting lowering to refuse with the exact message
/// fragments.
fn refuses(name: &str, source: &str, seam: &str, needle: &str) {
    let mut files = Files::new();
    let id = files.add_source("probe.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "{}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let Err(error) = lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed) else {
        panic!("the probe refuses: {source}")
    };
    assert!(
        matches!(error, LowerError::Unsupported { .. }),
        "the refusal is Unsupported, got {error:?}"
    );
    let text = error.to_string();
    assert!(text.contains(seam), "the error names the seam: {text}");
    assert!(text.contains(needle), "the error names the future: {text}");
}

/// One probe's source over the shared list ambient.
fn with_ambient(body: &str) -> String {
    format!("{LIST_AMBIENT}{body}")
}

/// The ambient every probe shares: the list surface over records.
const LIST_AMBIENT: &str = concat!(
    "tag int\n",
    "tag string\n",
    "tag bool\n",
    "tag none\n",
    "struct list[t]\n",
    "extern fn println (msg: string) -> ()\n",
    "extern fn list_len[t] (xs: list[t]) -> int\n",
    "extern fn list_get[t] (xs: list[t], i: int) -> t | none\n",
    "extern fn list_push[t] (xs: mut list[t], x: t) -> ()\n",
);

/// The whole read-back story: a literal and direct pushes store whole
/// record spans at the stride, the checked read answers the span, and
/// the match and `.ok`/`.err` extractions see the record's fields. The
/// entry ends in a pure expression so the heap-drain assertion in
/// `runs` is the Ref-word balance across pushes and read-outs.
#[test]
fn record_elements_read_back_end_to_end() {
    let out = runs(
        "span_channel",
        &with_ambient(concat!(
            "struct point { x: int, y: int }
",
            "fn sum_at(xs: list[point], i: int) -> int {
",
            "  match list_get(xs, i) {
",
            "    none => 0,
",
            "    p => p.x + p.y,
",
            "  }
",
            "}
",
            "fn main() -> int {
",
            "  const xs = [point { x: 1, y: 2 }, point { x: 3, y: 4 }]
",
            "  const one = list_get(xs, 1)
",
            "  const fallback = point { x: 0, y: 0 }
",
            "  const bumped = one.ok((p: point) => point { x: p.x + 10, y: p.y }).err((_: none) \
             => fallback)
",
            "  sum_at(xs, 0) + sum_at(xs, 1) + sum_at(xs, 9) + sum_at([bumped], 0)
",
            "}
",
        )),
    );
    // 3 + 7 + 0 + 17: the two hits, the miss, and the bumped re-read
    assert_eq!(out, FrameAnswer::one(Value::I64(27)));
}

/// Ref words inside a multi-word element: the push acquires one count
/// per `Ref` word for the list's edge, the read hands one out per word,
/// and the deaths balance (the heap drains, asserted by `runs`).
#[test]
fn ref_words_inside_elements_balance() {
    let out = runs(
        "span_refs",
        &with_ambient(concat!(
            "struct pair { a: string, b: string }
",
            "fn render(p: pair) -> string { p.a + \"/\" + p.b }
",
            "fn main() -> int {
",
            "  var xs = list[pair]()
",
            "  list_push(xs, pair { a: \"left\", b: \"right\" })
",
            "  xs.list_push(pair { a: \"up\", b: \"down\" })
",
            "  var total = 0
",
            "  var i = 0
",
            "  while i < list_len(xs) {
",
            "    const hit = list_get(xs, i)
",
            "    const text = hit.ok((p: pair) => render(p)).err((_: none) => \"?\")
",
            "    if text != \"?\" { total += 1 }
",
            "    i += 1
",
            "  }
",
            "  total
",
            "}
",
        )),
    );
    assert_eq!(out, FrameAnswer::one(Value::I64(2)));
}

/// `unwrap_or` and `head`'s checked span ride the same channel: the
/// eager fallback over a wider optional and the first-element read both
/// re-span whole member fields.
#[test]
fn unwrap_or_and_head_read_wider_sides() {
    let out = runs(
        "span_extract",
        &with_ambient(concat!(
            "extern fn head[t] (xs: list[t]) -> t | none
",
            "extern fn unwrap_or[t] (x: t?, y: t) -> t
",
            "struct point { x: int, y: int }
",
            "fn main() -> int {
",
            "  const xs = [point { x: 5, y: 6 }]
",
            "  const p = unwrap_or(list_get(xs, 0), point { x: -1, y: -1 })
",
            "  const first = match head(xs) {
",
            "    none => 0,
",
            "    q => q.x,
",
            "  }
",
            "  const miss = unwrap_or(list_get(xs, 4), point { x: 0, y: 9 })
",
            "  p.x + p.y + first + miss.y
",
            "}
",
        )),
    );
    assert_eq!(out, FrameAnswer::one(Value::I64(11 + 5 + 9)));
}

/// The stride-aware copy closes over wider elements: `cons`'s head span
/// rides the staging front (the first prepend onto an empty list fixes
/// the stride), and `tail`/`collect`/`list_concat` copy whole spans at
/// the source's stride — the copy natives' one walk taking the fresh
/// edge's count per `Ref` word (the heap drains, asserted by `runs`).
#[test]
fn copy_closes_carry_wider_strides() {
    let out = runs(
        "span_closes",
        &with_ambient(concat!(
            "extern fn cons[t] (x: t, xs: list[t]) -> list[t]\n",
            "extern fn tail[t] (xs: list[t]) -> list[t]\n",
            "extern fn collect[t] (xs: list[t]) -> list[t]\n",
            "extern fn list_concat[t] (a: list[t], b: list[t]) -> list[t]\n",
            "struct point { x: int, y: int }\n",
            "fn read(xs: list[point], i: int) -> int {\n",
            "  match list_get(xs, i) {\n",
            "    none => 0,\n",
            "    p => p.x + p.y,\n",
            "  }\n",
            "}\n",
            "fn main() -> int {\n",
            "  const empty = list[point]()\n",
            "  const one = cons(point { x: 1, y: 2 }, empty)\n",
            "  const two = cons(point { x: 3, y: 4 }, one)\n",
            "  const joined = list_concat(two, collect(two))\n",
            "  const rest = tail(two)\n",
            "  read(two, 0) + read(two, 1) + read(joined, 3) + read(rest, 0)\n",
            "}\n",
        )),
    );
    // two = [3+4, 1+2]; joined repeats it; the tail keeps the last
    assert_eq!(out, FrameAnswer::one(Value::I64(7 + 3 + 3 + 3)));
}

/// `Ref` words inside elements through the copy closes: each copied
/// `Ref` word gains the fresh list's edge count and the sources keep
/// their own, so the deaths balance (the heap drains).
#[test]
fn copy_closes_balance_ref_words() {
    let out = runs(
        "span_closes_refs",
        &with_ambient(concat!(
            "extern fn cons[t] (x: t, xs: list[t]) -> list[t]\n",
            "extern fn collect[t] (xs: list[t]) -> list[t]\n",
            "extern fn list_concat[t] (a: list[t], b: list[t]) -> list[t]\n",
            "struct pair { a: string, b: string }\n",
            "fn mark(xs: list[pair], i: int) -> int {\n",
            "  match list_get(xs, i) {\n",
            "    none => 0,\n",
            "    p => if p.a == \"t\" { 1 } else { 2 },\n",
            "  }\n",
            "}\n",
            "fn main() -> int {\n",
            "  const xs = [pair { a: \"l\", b: \"r\" }, pair { a: \"u\", b: \"d\" }]\n",
            "  const grown = cons(pair { a: \"t\", b: \"b\" }, list_concat(xs, collect(xs)))\n",
            "  mark(grown, 0) + mark(grown, 4)\n",
            "}\n",
        )),
    );
    // the prepended head reads 1, the joined tail's first 2
    assert_eq!(out, FrameAnswer::one(Value::I64(3)));
}

/// for-in and membership bind whole element spans per step: the fetch
/// answers the stride's words, the binder names the gathered span, and
/// the membership compare walks the structural equality chain (a
/// fail-fast field mismatch strands no count — the heap drains).
#[test]
fn for_in_and_membership_bind_wider_elements() {
    let out = runs(
        "span_iter",
        &with_ambient(concat!(
            "struct point { x: int, y: int }\n",
            "struct pair { a: string, b: string }\n",
            "fn main() -> int {\n",
            "  var total = 0\n",
            "  for p in [point { x: 1, y: 2 }, point { x: 3, y: 4 }] {\n",
            "    total += p.x * p.y\n",
            "  }\n",
            "  const hit = point { x: 3, y: 4 } in [point { x: 1, y: 2 }, point { x: 3, y: 4 }]\n",
            "  const miss = pair { a: \"ab\", b: \"zz\" } in [pair { a: \"ab\", b: \"cd\" }]\n",
            "  if hit { total += 10 }\n",
            "  if miss { total += 100 }\n",
            "  total\n",
            "}\n",
        )),
    );
    // 1*2 + 3*4, the hit adds 10, the field-2 mismatch stays a miss
    assert_eq!(out, FrameAnswer::one(Value::I64(14 + 10)));
}

/// for-in over `Ref`-carrying elements: the handed-out count per `Ref`
/// word dies at the binder's scope rollover each iteration, never
/// carried across the back edge (the heap drains).
#[test]
fn for_in_balances_ref_words() {
    let out = runs(
        "span_iter_refs",
        &with_ambient(concat!(
            "struct pair { a: string, b: string }\n",
            "fn main() -> int {\n",
            "  var total = 0\n",
            "  for p in [pair { a: \"l\", b: \"r\" }, pair { a: \"u\", b: \"d\" }] {\n",
            "    if p.a == \"l\" { total += 1 }\n",
            "    if p.b == \"d\" { total += 3 }\n",
            "  }\n",
            "  total\n",
            "}\n",
        )),
    );
    // the first element's `a` and the second's `b`
    assert_eq!(out, FrameAnswer::one(Value::I64(4)));
}

/// The reentrant combinators over wider elements: the hosted call hands
/// the element's whole span in and the answer's span out — `map` to a
/// scalar, `map` to a record (the answer's width fixes the result
/// list's stride), `filter` by field, and a capturing lambda in the
/// chain. The heap drains.
#[test]
fn combinators_take_wider_elements_each_way() {
    let out = runs(
        "span_combinators",
        &with_ambient(concat!(
            "extern fn map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]\n",
            "extern fn filter[t] (xs: list[t], f: fn(t) -> bool) -> list[t]\n",
            "extern fn reduce[t] (xs: list[t], init: t, f: fn(t, t) -> t) -> t\n",
            "struct point { x: int, y: int }\n",
            "fn read(xs: list[point], i: int) -> int {\n",
            "  match list_get(xs, i) {\n",
            "    none => 0,\n",
            "    p => p.x * 10 + p.y,\n",
            "  }\n",
            "}\n",
            "fn main() -> int {\n",
            "  const xs = [point { x: 1, y: 2 }, point { x: 3, y: 4 }, point { x: 5, y: 6 }]\n",
            "  const widths = map(xs, (p: point) => p.x + p.y)\n",
            "  const bump = 10\n",
            "  const lifted = map(xs, (p: point) => point { x: p.x + bump, y: p.y })\n",
            "  const kept = filter(xs, (p: point) => p.x >= 3)\n",
            "  const folded = reduce(widths, 0, (a: int, w: int) => a + w)\n",
            "  read(lifted, 2) + read(kept, 0) + folded\n",
            "}\n",
        )),
    );
    // lifted[2] = (5+10)*10+6; kept[0] = 34; the widths fold = 21
    assert_eq!(out, FrameAnswer::one(Value::I64(156 + 34 + 21)));
}

/// The refusal openings that stay, each naming the seam that owns it —
/// never reboxing, never the landed channel.
#[test]
fn the_remaining_openings_keep_honest_names() {
    // a deinit-carrying element: the list's edge retention owns counts,
    // never claims
    refuses(
        "refuse_deinit_element",
        concat!(
            "tag int\n",
            "tag string\n",
            "tag none\n",
            "trait Copyable { }\n",
            "struct list[t]\n",
            "extern fn println (msg: string) -> ()\n",
            "extern fn list_push[t] (xs: mut list[t], x: t) -> ()\n",
            "extern fn list_len[t] (xs: list[t]) -> int\n",
            "struct session: ~Copyable { user: string\n",
            "  deinit { println(\"freed ${self.user}\") }\n",
            "}\n",
            "fn main() -> int {\n",
            "  const xs = [session { user: \"u\" }]\n",
            "  list_len(xs)\n",
            "}\n",
        ),
        "a list literal element",
        "per-span deinit claims own the seam",
    );
    // a deinit-carrying fold init: the native call borrows the staged
    // words, so the init's hook has no drop position of its own — the
    // elements themselves refuse at the list's own seam first, and the
    // fold's wide staging carries deinit-free spans only
    refuses(
        "refuse_reduce_deinit_init",
        &with_ambient(concat!(
            "extern fn reduce[t] (xs: list[t], init: t, f: fn(t, t) -> t) -> t
",
            "trait Copyable { }
",
            "struct session: ~Copyable { user: string
",
            "  deinit { println(\"freed ${self.user}\") }
",
            "}
",
            "fn main() -> int {
",
            "  const xs = list[session]()
",
            "  const total = reduce(xs, session { user: \"u\" }, (a: session, s: session) => a)
",
            "  println(\"held ${total.user}\")
",
            "  list_len(xs)
",
            "}
",
        )),
        "a deinit-carrying argument",
        "the value's deinit runs only at its binding's death",
    );
    // a union-typed element: the checked read extracts one atom, so the
    // iteration fetch keeps its refusal (an empty list[union]
    // constructor checks clean, so the refusal is the fetch's own)
    refuses(
        "refuse_union_iter",
        concat!(
            "tag int\n",
            "tag string\n",
            "tag none\n",
            "struct list[t]\n",
            "type word = int | string\n",
            "fn main() -> int {\n",
            "  var total = 0\n",
            "  for t in list[word]() { total += 1 }\n",
            "  total\n",
            "}\n",
        ),
        "a list element of this shape",
        "union elements keep the checked-read refusal",
    );
}

/// The tuple-element story: a literal and direct pushes store whole
/// tuple spans at the stride (one word per element), the checked read
/// answers the span, and the match, `unwrap_or`, and `.ok`/`.err`
/// extractions destructure positionally. The entry ends in a pure
/// expression so the heap-drain assertion in `runs` holds.
#[test]
fn tuple_elements_read_back_end_to_end() {
    let out = runs(
        "span_tuples",
        &with_ambient(concat!(
            "extern fn unwrap_or[t] (x: t?, y: t) -> t\n",
            "fn read(xs: list[(int, string)], i: int) -> int {\n",
            "  match list_get(xs, i) {\n",
            "    none => 0,\n",
            "    t => {\n",
            "      const (n, s) = t\n",
            "      n\n",
            "    },\n",
            "  }\n",
            "}\n",
            "fn main() -> int {\n",
            "  var xs = list[(int, string)]()\n",
            "  list_push(xs, (1, \"a\"))\n",
            "  xs.list_push((2, \"b\"))\n",
            "  const ys = [(3, \"c\"), (4, \"d\")]\n",
            "  const one = list_get(ys, 1)\n",
            "  const flat = unwrap_or(one, (9, \"z\"))\n",
            "  const (dn, ds) = flat\n",
            "  const text = list_get(ys, 0).ok((t: (int, string)) => {\n",
            "    const (n, s) = t\n",
            "    s\n",
            "  }).err((_: none) => \"?\")\n",
            "  const bonus = if text == \"c\" { 1 } else { 0 }\n",
            "  read(xs, 0) + read(xs, 1) + read(xs, 9) + dn + bonus\n",
            "}\n",
        )),
    );
    // 1 + 2 + the miss + 4 + the ok arm's extracted string
    assert_eq!(out, FrameAnswer::one(Value::I64(8)));
}

/// Ref words inside tuple elements: the push acquires one count per
/// element's `Ref` word for the list's edge, the read hands them out,
/// and the member destructures balance (the heap drains, asserted by
/// `runs`).
#[test]
fn tuple_elements_balance_ref_words() {
    let out = runs(
        "span_tuple_refs",
        &with_ambient(concat!(
            "fn main() -> int {\n",
            "  var xs = list[(string, string)]()\n",
            "  list_push(xs, (\"left\", \"right\"))\n",
            "  list_push(xs, (\"up\", \"down\"))\n",
            "  var total = 0\n",
            "  var i = 0\n",
            "  while i < list_len(xs) {\n",
            "    const hit = list_get(xs, i)\n",
            "    const text = hit.ok((t: (string, string)) => {\n",
            "      const (a, b) = t\n",
            "      a\n",
            "    }).err((_: none) => \"?\")\n",
            "    if text != \"?\" { total += 1 }\n",
            "    i += 1\n",
            "  }\n",
            "  total\n",
            "}\n",
        )),
    );
    assert_eq!(out, FrameAnswer::one(Value::I64(2)));
}

/// for-in and membership over tuple elements: the loop binder receives
/// the whole element span and destructures positionally, and the wide
/// membership walks the structural comparison one element word at a
/// time.
#[test]
fn for_in_and_membership_bind_tuple_elements() {
    let out = runs(
        "span_tuple_iter",
        &with_ambient(concat!(
            "fn main() -> int {\n",
            "  var total = 0\n",
            "  for p in [(1, \"a\"), (2, \"b\"), (3, \"c\")] {\n",
            "    const (n, s) = p\n",
            "    total += n\n",
            "  }\n",
            "  const hit = (2, \"b\") in [(1, \"a\"), (2, \"b\")]\n",
            "  const miss = (2, \"x\") in [(1, \"a\"), (2, \"b\")]\n",
            "  if hit { total += 10 }\n",
            "  if miss { total += 100 }\n",
            "  total\n",
            "}\n",
        )),
    );
    // 1 + 2 + 3, the hit adds 10, the word-2 mismatch stays a miss
    assert_eq!(out, FrameAnswer::one(Value::I64(16)));
}

/// The reentrant combinators over tuple elements: the hosted call hands
/// the element's positional span in and the answer's span out — `map`
/// to a scalar, `map` back to a tuple (the answer's width fixes the
/// result list's stride), `filter` by an element word, and `reduce`
/// folding a whole tuple accumulator through the interior staging. The
/// heap drains.
#[test]
fn combinators_take_tuple_elements_each_way() {
    let out = runs(
        "span_tuple_combinators",
        &with_ambient(concat!(
            "extern fn map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]\n",
            "extern fn filter[t] (xs: list[t], f: fn(t) -> bool) -> list[t]\n",
            "extern fn reduce[t] (xs: list[t], init: t, f: fn(t, t) -> t) -> t\n",
            "fn read(xs: list[(int, string)], i: int) -> int {\n",
            "  match list_get(xs, i) {\n",
            "    none => 0,\n",
            "    t => {\n",
            "      const (n, s) = t\n",
            "      n\n",
            "    },\n",
            "  }\n",
            "}\n",
            "fn main() -> int {\n",
            "  const xs = [(1, \"a\"), (2, \"b\"), (3, \"c\")]\n",
            "  const ns = map(xs, (t: (int, string)) => {\n",
            "    const (n, s) = t\n",
            "    n * 10\n",
            "  })\n",
            "  const lifted = map(ns, (n: int) => (n + 1, \"z\"))\n",
            "  const kept = filter(xs, (t: (int, string)) => {\n",
            "    const (n, s) = t\n",
            "    n >= 2\n",
            "  })\n",
            "  const folded = reduce(xs, (0, \"\"), (a: (int, string), t: (int, string)) => {\n",
            "    const (an, as2) = a\n",
            "    const (n, s) = t\n",
            "    (an + n, s)\n",
            "  })\n",
            "  const (fn2, fs2) = folded\n",
            "  read(lifted, 2) + read(kept, 0) + fn2\n",
            "}\n",
        )),
    );
    // lifted[2] = 31; kept[0] = 2; the fold's first word = 6
    assert_eq!(out, FrameAnswer::one(Value::I64(31 + 2 + 6)));
}

/// The root drive's host seam answers whole spans (the 2026-11-15
/// span-answer ruling): a tuple-answering entry crosses with every word,
/// never the head word alone.
#[test]
fn a_tuple_answering_entry_surfaces_whole_at_the_host() {
    let out = runs(
        "span_host_tuple",
        &with_ambient(concat!("fn main() -> (int, int) {\n", "  (3, 4)\n", "}\n",)),
    );
    assert_eq!(out.as_slice(), &[Value::I64(3), Value::I64(4)]);
}

/// A widened answer — a union value whose span carries a tag word plus
/// its region — crosses whole too: the host reads the tag, the payload
/// survives, and the entry's RETSPAN width is the seam's width.
#[test]
fn a_widened_union_answer_surfaces_whole_at_the_host() {
    let out = runs(
        "span_host_wide",
        &with_ambient(concat!(
            "type opt = int | none\n",
            "fn main() -> opt {\n",
            "  const v: opt = 4\n",
            "  v\n",
            "}\n",
        )),
    );
    let words = out.as_slice();
    assert_eq!(words.len(), 2, "the widened span carries both words");
    assert!(
        matches!(words[0], Value::Tag(_)),
        "the tag word leads the span, got {words:?}"
    );
    assert_eq!(words[1], Value::I64(4), "the member payload rides the tail");
}
