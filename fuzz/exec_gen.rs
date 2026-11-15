//! The shared source generator for the VM execution fuzz leg's match
//! and fork-join vocabulary: fuzz bytes become one well-typed DS program
//! — straight-line episodes over the pattern rounds' landed shapes and
//! the fork-join combinators — that runs the whole chain end to end:
//! parse, check, lower, module round trip, loader install, VM run, and
//! the captured answers must equal the generator's own oracle, with the
//! heap drained at the end. Included by the cargo-fuzz target `fuzz_vm`
//! and the nextest mirror in `crates/ds-lower/tests/fuzz_exec_gen.rs`.
//!
//! Vocabulary: value-level or-patterns (literal ladders over scalar and
//! string scrutinees, struct-pattern alternative arms, field or-ladders,
//! payload or-ladders), struct patterns over struct-typed scrutinees
//! (literal field tests, punning binders), case constructor arms with
//! literal payload tests (int, string, and struct payloads), guards
//! over union dispatch (value comparisons over payload binders,
//! including Ref-carrying payloads), nested struct sub-patterns over
//! struct-typed fields and payloads, whole-span union equality (member
//! values against the union from either side, a unit-tag mirror, and a
//! match binder comparing its member's value), and the fork-join
//! combinators: `par_map` (a capturing closure callable and a named
//! multi-word-answer callable, an empty-list leg) and `par_reduce`
//! (a scalar closure fold, a string fold, the whole-span accumulator
//! fold over a struct-typed init, and an empty-input init
//! passthrough), and the CALL_GENERIC round's free generic fn
//! calls: declared bounds over a serving trait, the int, string,
//! and list instantiation chains, the nested runtime form (a free
//! generic callee whose demand rides the calling frame's own
//! staged slot), the tail transfer, the two-level list[list[int]]
//! instantiation, the boundless call staging no evidence, and the
//! inert uncalled declaration — each counted in the draw census
//! the mirror asserts.
//!
//! Termination (why every generated run stops by itself): main is
//! straight-line (no recursion; each episode lives in its own helper
//! function, so every frame stays under the width cap; the one loop
//! family is the generic serving impl's for-in over a fixed-length
//! literal list of at most three elements, so every iteration count
//! is drawn and bounded), the
//! combinator inputs are
//! fixed-length lists (at most four elements, folded or mapped by the
//! VM's own bounded single-thread drive), and the spawn tree stays
//! depth one — every `par_map`/`par_reduce` call site sits in main, and
//! no callable body (closure or named) contains a combinator call, so
//! no spawned child can reach another combinator site.
//!
//! Oracle discipline: the generator mirrors each episode's semantics in
//! the same order the chain lowers it (arms in order, or-ladders
//! short-circuiting, the left fold in input order), so the captured
//! stdout must match exactly. The drain assert rides a closer: main
//! ends in `println("done")`, a constant-string argument, because a
//! root frame's native tail transfer abandons its staged owned words
//! (the heap dies whole at the store's Drop) — an interpolated argument
//! on the final println would legitimately stay live, so the closer
//! keeps the final call's argument count-free.

use std::fmt::Write as _;

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// The generated module's name (the run entry target).
pub const MODULE_NAME: &str = "fze";

/// The draw census over the landed execution vocabulary, for the
/// mirror's coverage assertions (generation facts, not runtime depth).
#[derive(Default)]
pub struct Drawn {
    /// Value-level or-patterns: literal ladders, struct alternatives,
    /// field or-ladders, payload or-ladders.
    pub or_patterns: u32,
    /// Struct-pattern arms over struct-typed scrutinees.
    pub struct_patterns: u32,
    /// Case constructor arms with literal payload tests.
    pub payload_tests: u32,
    /// Value-comparison guards over union dispatch arms.
    pub union_guards: u32,
    /// Nested struct sub-patterns over fields and payloads.
    pub nested_sub_patterns: u32,
    /// Whole-span equality conditions over union operands.
    pub union_equality: u32,
    /// `par_map` call sites.
    pub par_map: u32,
    /// `par_reduce` call sites.
    pub par_reduce: u32,
    /// Closure callables staged at combinator sites.
    pub closures: u32,
    /// Multi-word answers: struct-typed `par_map` results and
    /// whole-span accumulator folds.
    pub multiword: u32,
    /// Free generic fn call sites into the ONE bodies.
    pub generic_calls: u32,
    /// Serving impl bodies the evidence chains resolve through.
    pub generic_impls: u32,
    /// Nested generic callee bodies riding the calling frame's own
    /// staged slot (the runtime form).
    pub generic_runtime_forms: u32,
    /// Tail-position generic calls transferring through the CALLT.
    pub generic_tails: u32,
    /// Two-level instantiations (list[list[int]] through the
    /// generic impl's follow chain).
    pub two_level_calls: u32,
    /// Declared-but-uncalled free generic fns (the inert ONE body).
    pub inert_generics: u32,
}

/// One generated program: its source, the oracle stdout, and the census.
pub struct Program {
    pub source: String,
    pub expected: String,
    pub drawn: Drawn,
}

/// Byte stream over the fuzz input: cycles through the data, so any
/// input (including empty) yields a full program and any mutation
/// shifts the generated program deterministically.
pub struct Stream<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Stream<'a> {
    /// Wraps the fuzz input.
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn byte(&mut self) -> u8 {
        if self.data.is_empty() {
            return 0;
        }
        let b = self.data[self.pos % self.data.len()];
        self.pos = self.pos.wrapping_add(1);
        b
    }

    fn below(&mut self, bound: usize) -> usize {
        usize::from(self.byte()) % bound.max(1)
    }

    fn roll(&mut self, one_in: usize) -> bool {
        self.below(one_in.max(1)) == 0
    }
}

/// The value vocabulary the episodes draw from.
const WORDS: [&str; 4] = ["zero", "nine", "later", "zed"];
const SHORTS: [&str; 4] = ["al", "be", "ga", "ox"];

/// The free-generic story's declarations: a serving trait impl'd
/// over int, string, and the generic list head (the list impl
/// body's for-in iterating the fixed-length literal lists the
/// episodes draw), and the free generic fns over it — the bounded
/// call, the nested runtime form (tag_it's demand riding
/// show_pair's own staged slot), the boundless echo, the bounded
/// shout, drive's tail-position call, and the inert quiet.
const GENERIC_STORY: &str = r#"
trait shows {
  fn show(self) -> string
}

impl shows for int {
  fn show(self) -> string { "n=$self" }
}

impl shows for string {
  fn show(self) -> string { "$self!" }
}

impl [t: shows] shows for list[t] {
  fn show(self) -> string {
    var acc = "l:"
    for x in self {
      acc = concat(acc, concat(x.show(), ";"))
    }
    acc
  }
}

extern fn concat (a: string, b: string) -> string

fn tag_it[t: shows] (x: t) -> string { x.show() }

fn show_pair[t: shows] (a: t, b: t) -> string { concat(a.show(), tag_it(b)) }

fn echo[t] (x: t) -> t { x }

fn shout[t: shows] (x: t) -> string { x.show() }

fn drive (n: int) -> string { shout(n) }

fn quiet[t: shows] (x: t) -> string { x.show() }
"#;

/// One episode emitter over the shared output buffers: top-level
/// declarations collect in `decls`, main's statements in `body`, the
/// oracle in `out`.
struct Gen<'a> {
    s: &'a mut Stream<'a>,
    decls: String,
    body: String,
    calls: String,
    out: String,
    drawn: Drawn,
    fresh: usize,
}

impl Gen<'_> {
    /// A small literal, 0..`bound`.
    fn lit(&mut self, bound: usize) -> i64 {
        i64::try_from(self.s.below(bound)).expect("rng bounds are tiny")
    }

    /// A fresh binder or local name.
    fn name(&mut self, prefix: &str) -> String {
        self.fresh += 1;
        format!("{prefix}{}", self.fresh)
    }

    /// One literal under `bound` outside `avoid`: a few redraws, then a
    /// deterministic forward walk. The rejection redraw alone could
    /// spin forever on a constant (all-zero) byte stream, and the
    /// checker refuses an arm literal or guard an earlier arm's failed
    /// test already excluded, so tested literals must stay disjoint
    /// across arms.
    fn lit_not(&mut self, bound: usize, avoid: &[i64]) -> i64 {
        for _ in 0..8 {
            let v = self.lit(bound);
            if !avoid.contains(&v) {
                return v;
            }
        }
        let mut v = (avoid[0] + 1) % i64::try_from(bound).expect("bounds are tiny");
        while avoid.contains(&v) {
            v = (v + 1) % i64::try_from(bound).expect("bounds are tiny");
        }
        v
    }

    /// Three distinct literals under `bound`.
    fn distinct3(&mut self, bound: usize) -> (i64, i64, i64) {
        let a = self.lit(bound);
        let b = self.lit_not(bound, &[a]);
        let c = self.lit_not(bound, &[a, b]);
        (a, b, c)
    }

    /// One drawn word outside `avoid`: the same bounded-redraw
    /// discipline as [`Gen::lit_not`].
    fn word_not(&mut self, avoid: &str) -> &'static str {
        for _ in 0..8 {
            let w = WORDS[self.s.below(WORDS.len())];
            if w != avoid {
                return w;
            }
        }
        WORDS
            .iter()
            .find(|w| **w != avoid)
            .expect("the word pool leaves a spare")
    }

    /// Emits one main-body statement line.
    fn line(&mut self, text: &str) {
        self.body.push_str("  ");
        self.body.push_str(text);
        self.body.push('\n');
    }

    /// Emits one top-level declaration.
    fn decl(&mut self, text: &str) {
        self.decls.push_str(text);
        self.decls.push('\n');
    }

    /// Seals the current episode body into its own helper function and
    /// records its call for main: one frame per episode keeps every
    /// frame under the width limit (the per-function slot cap).
    fn seal_episode(&mut self) {
        let body = std::mem::take(&mut self.body);
        self.fresh += 1;
        let name = format!("ep{}", self.fresh);
        let _ = writeln!(self.decls, "fn {name}() -> () {{\n{body}}}");
        let _ = writeln!(self.calls, "  {name}()");
    }

    /// Records one oracle stdout line.
    fn answer(&mut self, text: &str) {
        self.out.push_str(text);
        self.out.push('\n');
    }

    /// The scalar or-ladder episode: a literal ladder arm, a literal
    /// arm, a guarded binder, and the settling binder, over a drawn
    /// value.
    fn ep_scalar_ladder(&mut self) {
        let value = self.lit(10);
        let (first, second, third) = self.distinct3(10);
        let scrut = self.name("n");
        let binder = self.name("k");
        self.line(&format!("const {scrut} = {value}"));
        self.line(&format!("match {scrut} {{"));
        self.line(&format!("{first} | {second} => println(\"ladder\"),"));
        self.line(&format!("{third} => println(\"lit\"),"));
        self.line(&format!(
            "{binder} if {binder} > 5 => println(\"big ${{{binder}}}\"),"
        ));
        self.line(&format!("{binder} => println(\"val ${{{binder}}}\"),"));
        self.line("}");
        self.drawn.or_patterns += 1;
        if value == first || value == second {
            self.answer("ladder");
        } else if value == third {
            self.answer("lit");
        } else if value > 5 {
            self.answer(&format!("big {value}"));
        } else {
            self.answer(&format!("val {value}"));
        }
    }

    /// The string match episode: a string literal or-arm, a guard over
    /// the Ref-carrying scrutinee, and a catchall.
    fn ep_string_match(&mut self) {
        let w = WORDS[self.s.below(WORDS.len())];
        let (w0, w1) = (
            WORDS[self.s.below(WORDS.len())],
            WORDS[self.s.below(WORDS.len())],
        );
        // the guard literal must escape the or-arm's exclusions, else the
        // checker proves the guard constant and refuses it
        let gl = WORDS
            .iter()
            .find(|word| **word != w0 && **word != w1)
            .expect("two distinct-or-equal draws leave a spare word");
        let s = self.name("s");
        let q = self.name("q");
        self.line(&format!("const {s} = \"{w}\""));
        self.line(&format!("match {s} {{"));
        self.line(&format!("\"{w0}\" | \"{w1}\" => println(\"verb\"),"));
        self.line(&format!("{q} if {q} == \"{gl}\" => println(\"guard\"),"));
        self.line("_ => println(\"other\"),");
        self.line("}");
        self.drawn.or_patterns += 1;
        if w == w0 || w == w1 {
            self.answer("verb");
        } else if w == *gl {
            self.answer("guard");
        } else {
            self.answer("other");
        }
    }

    /// The struct pattern episode: a field or-ladder arm, a struct
    /// alternative or-arm, a guarded punned arm, and the settling binder
    /// arm, over a two-field struct scrutinee.
    fn ep_struct_pattern(&mut self) {
        let (fx, fy) = (self.lit(10), self.lit(10));
        let (alt0, alt1, _) = self.distinct3(10);
        let fy2 = if self.s.roll(2) { fy } else { self.lit(10) };
        let threshold = 2 + self.lit(6);
        let scrut = self.name("p");
        let mid = self.name("m");
        let (nx, wy) = (self.name("n"), self.name("w"));
        self.line(&format!("const {scrut} = pt {{ x: {fx}, y: {fy} }}"));
        self.line(&format!("match {scrut} {{"));
        self.line(&format!(
            "pt {{ x: {alt0} | {alt1}, y: {fy2} }} => println(\"ladder\"),"
        ));
        self.line("pt { x: 1, y: _ } | pt { x: 3, y: _ } => println(\"small\"),");
        self.line(&format!(
            "pt {{ x, y: {mid} }} if x > {threshold} => println(\"big ${{x}} ${{{mid}}}\"),"
        ));
        self.line(&format!(
            "pt {{ x: {nx}, y: {wy} }} => println(\"rest ${{{nx}}} ${{{wy}}}\"),"
        ));
        self.line("}");
        self.drawn.struct_patterns += 1;
        self.drawn.or_patterns += 2;
        self.drawn.union_guards += 1;
        if (fx == alt0 || fx == alt1) && fy == fy2 {
            self.answer("ladder");
        } else if fx == 1 || fx == 3 {
            self.answer("small");
        } else if fx > threshold {
            self.answer(&format!("big {fx} {fy}"));
        } else {
            self.answer(&format!("rest {fx} {fy}"));
        }
    }

    /// The enum payload episode: a literal payload test, a payload
    /// or-ladder, a payload binder, and the bare case arm.
    fn ep_case_payload(&mut self) {
        let value = self.lit(10);
        let plain = self.s.roll(4);
        let (first, second, third) = self.distinct3(10);
        let scrut = self.name("g");
        let binder = self.name("n");
        self.line(&format!(
            "const {scrut}: grade = {}",
            if plain { "plain".to_owned() } else { format!("scored({value})") }
        ));
        self.line(&format!("match {scrut} {{"));
        self.line(&format!("scored({first}) => println(\"one\"),"));
        self.line(&format!("scored({second} | {third}) => println(\"mid\"),"));
        self.line(&format!(
            "scored({binder}) => println(\"n ${{{binder}}}\"),"
        ));
        self.line("plain => println(\"plain\"),");
        self.line("}");
        self.drawn.payload_tests += 2;
        self.drawn.or_patterns += 1;
        if plain {
            self.answer("plain");
        } else if value == first {
            self.answer("one");
        } else if value == second || value == third {
            self.answer("mid");
        } else {
            self.answer(&format!("n {value}"));
        }
    }

    /// The Ref payload episode: a literal string payload test, a guarded
    /// payload binder, the payload binder, and the bare case arm.
    fn ep_string_payload(&mut self) {
        let wd = WORDS[self.s.below(WORDS.len())];
        let empty = self.s.roll(5);
        let wa = WORDS[self.s.below(WORDS.len())];
        let wb = self.word_not(wa);
        let mk = self.name("mk");
        let w = self.name("w");
        self.line(&format!(
            "const {mk}: mark = {}",
            if empty { "empty".to_owned() } else { format!("word(\"{wd}\")") }
        ));
        self.line(&format!("match {mk} {{"));
        self.line(&format!("word(\"{wa}\") => println(\"the {wa}\"),"));
        self.line(&format!(
            "word({w}) if {w} == \"{wb}\" => println(\"guarded\"),"
        ));
        self.line(&format!("word({w}) => println(\"word ${{{w}}}\"),"));
        self.line("empty => println(\"empty\"),");
        self.line("}");
        self.drawn.payload_tests += 1;
        self.drawn.union_guards += 1;
        if empty {
            self.answer("empty");
        } else if wd == wa {
            self.answer(&format!("the {wa}"));
        } else if wd == wb {
            self.answer("guarded");
        } else {
            self.answer(&format!("word {wd}"));
        }
    }

    /// The struct payload episode: a nested struct sub-pattern payload
    /// test, the whole-span payload binder, and the bare case arm.
    fn ep_struct_payload(&mut self) {
        let (x, y) = (self.lit(8), self.lit(8));
        let dot = self.s.roll(3);
        let l = self.lit(8);
        let sh = self.name("sh");
        let p = self.name("p");
        self.line(&format!(
            "const {sh}: shape = {}",
            if dot { "dot".to_owned() } else { format!("at(pt {{ x: {x}, y: {y} }})") }
        ));
        self.line(&format!("match {sh} {{"));
        self.line(&format!(
            "at(pt {{ x: {l}, y: _ }}) => println(\"at {l}\"),"
        ));
        self.line(&format!(
            "at({p}) => println(\"at ${{{p}.x}} ${{{p}.y}}\"),"
        ));
        self.line("dot => println(\"dot\"),");
        self.line("}");
        self.drawn.payload_tests += 1;
        self.drawn.nested_sub_patterns += 1;
        if dot {
            self.answer("dot");
        } else if x == l {
            self.answer(&format!("at {l}"));
        } else {
            self.answer(&format!("at {x} {y}"));
        }
    }

    /// The nested sub-pattern episode: a struct field carrying its own
    /// struct sub-pattern one level deeper, over a wrapper scrutinee.
    fn ep_nested_sub_pattern(&mut self) {
        let (x, y) = (self.lit(8), self.lit(8));
        let id = self.lit(6);
        let hit = self.s.roll(2);
        let (y2, id2) = if hit {
            (y, id)
        } else {
            // both nested fields must differ, else the literal arm could
            // still hit and the oracle would disagree with the chain
            (self.lit_not(8, &[y]), self.lit_not(6, &[id]))
        };
        let b = self.name("b");
        self.line(&format!(
            "const {b} = wrap {{ at: pt {{ x: {x}, y: {y} }}, id: {id} }}"
        ));
        self.line(&format!("match {b} {{"));
        self.line(&format!(
            "wrap {{ at: pt {{ x: {x}, y: {y2} }}, id: {id2} }} => println(\"nested\"),"
        ));
        self.line("wrap { at, id } => println(\"at ${at.x} ${at.y} id ${id}\"),");
        self.line("}");
        self.drawn.nested_sub_patterns += 1;
        self.drawn.struct_patterns += 1;
        if hit {
            self.answer("nested");
        } else {
            self.answer(&format!("at {x} {y} id {id}"));
        }
    }

    /// The union equality episode: whole-span equality over union
    /// operands (call against call, member against union from either
    /// side, the unit-tag mirror, inequality, a struct-payloaded
    /// member's full field span), plus a match binder comparing its
    /// member's payload. The two member functions are
    /// top-level declarations the episode stages before main's body.
    fn ep_union_equality(&mut self) {
        let v = self.lit(5);
        let none = self.s.roll(3);
        let lit = self.lit(5);
        let (u, t, n) = (self.name("u"), self.name("t"), self.name("n"));
        self.decl(&format!(
            "fn pick_one() -> int | none {{ {} }}",
            if none { "none".to_owned() } else { format!("{v}") }
        ));
        self.decl("fn pick_none() -> int | none { none }");
        self.line(&format!("const {u} = pick_one()"));
        self.line(&format!("const {t} = none"));
        self.line(&format!("if {u} == pick_one() {{ println(\"same\") }}"));
        self.line("if pick_one() == pick_none() { println(\"hit\") } else { println(\"diff\") }");
        self.line("if none == pick_none() { println(\"mirror\") }");
        self.line(&format!(
            "if {t} == pick_none() {{ println(\"var mirror\") }}"
        ));
        self.line("if pick_one() != pick_none() { println(\"neq\") }");
        self.line(&format!("match {u} {{"));
        self.line("none => println(\"is none\"),");
        self.line(&format!(
            "{n} => {{ if {n} == {lit} {{ println(\"binder eq\") }} else {{ println(\"binder \
             ${{{n}}}\") }} }},"
        ));
        self.line("}");
        // a struct-payloaded member compares over its whole span:
        // the members differ only in the payload's later field, so a
        // first-field-only chain answers true and the oracle catches it
        self.decl("fn pt_left() -> pt | none { pt { x: 1, y: 2 } }");
        self.decl("fn pt_right() -> pt | none { pt { x: 1, y: 9 } }");
        self.line("if pt_left() == pt_right() { println(\"bad\") } else { println(\"pt diff\") }");
        self.line("if pt_left() == pt_left() { println(\"pt same\") }");
        self.drawn.union_equality += 8;
        self.answer("same");
        if none {
            self.answer("hit");
        } else {
            self.answer("diff");
        }
        self.answer("mirror");
        self.answer("var mirror");
        if !none {
            self.answer("neq");
        }
        if none {
            self.answer("is none");
        } else if v == lit {
            self.answer("binder eq");
        } else {
            self.answer(&format!("binder {v}"));
        }
        self.answer("pt diff");
        self.answer("pt same");
    }

    /// The fork-join episodes: `par_map` over the drawn int list (a
    /// capturing closure callable, a named multi-word-answer callable,
    /// an empty-list leg) and `par_reduce` (a scalar closure fold, a
    /// string fold, the whole-span accumulator fold over a struct-typed
    /// init, an empty-input init passthrough), all driven by the VM's
    /// bounded single-thread map and fold.
    fn ep_fork_join(&mut self) {
        let elems: Vec<i64> = (0..4).map(|_| self.lit(9)).collect();
        let (k, bias) = (2 + self.lit(4), self.lit(20));
        let (xs, bias_name) = (self.name("xs"), self.name("bias"));
        let list = elems
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        self.line(&format!("const {xs} = [{list}]"));
        self.line(&format!("const {bias_name} = {bias}"));
        // par_map with a capturing closure callable
        let shifted = self.name("shifted");
        self.line(&format!(
            "const {shifted} = {xs}.par_map((x: int) => x * {k} + {bias_name})"
        ));
        self.line(&format!(
            "println(\"head ${{at_int({shifted}, 0)}} len ${{len({shifted})}}\")"
        ));
        self.drawn.par_map += 1;
        self.drawn.closures += 1;
        self.answer(&format!("head {} len 4", elems[0] * k + bias));
        // par_map with a named callable answering a struct-typed span
        let (wide, first) = (self.name("wide"), self.name("first"));
        let idx = self.s.below(4);
        self.line(&format!("const {wide} = {xs}.par_map(both)"));
        self.line(&format!("const {first} = pair_at({wide}, {idx})"));
        self.line(&format!(
            "println(\"first ${{{first}.left}} ${{{first}.right}} len ${{len({wide})}}\")"
        ));
        self.drawn.par_map += 1;
        self.drawn.multiword += 1;
        self.answer(&format!(
            "first {} {} len 4",
            elems[idx] * 2,
            elems[idx] * 3
        ));
        if self.s.roll(2) {
            self.line("println(\"empty ${len(list[int]().par_map(both))}\")");
            self.drawn.par_map += 1;
            self.answer("empty 0");
        }
        // par_reduce with a scalar closure fold, in input order
        let init = self.lit(100);
        let sum = self.name("sum");
        self.line(&format!(
            "const {sum} = {xs}.par_reduce({init}, (a: int, b: int) => a + b + {bias_name})"
        ));
        self.line(&format!("println(\"sum ${{{sum}}}\")"));
        self.drawn.par_reduce += 1;
        self.drawn.closures += 1;
        let mut acc = init;
        for &e in &elems {
            acc += e + bias;
        }
        self.answer(&format!("sum {acc}"));
        // par_reduce over strings
        let words: Vec<&str> = (0..3).map(|_| SHORTS[self.s.below(SHORTS.len())]).collect();
        let (words_name, joined) = (self.name("words"), self.name("joined"));
        self.line(&format!(
            "const {words_name} = [\"{}\", \"{}\", \"{}\"]",
            words[0], words[1], words[2]
        ));
        self.line(&format!(
            "const {joined} = {words_name}.par_reduce(\"\", (a: string, b: string) => a + b)"
        ));
        self.line(&format!("println(\"joined ${{{joined}}}\")"));
        self.drawn.par_reduce += 1;
        self.drawn.closures += 1;
        self.answer(&format!("joined {}{}{}", words[0], words[1], words[2]));
        if self.s.roll(2) {
            let passthru = self.lit(50);
            self.line(&format!(
                "println(\"passthru ${{list[int]().par_reduce({passthru}, (a: int, b: int) => a + \
                 b)}}\")"
            ));
            self.drawn.par_reduce += 1;
            self.answer(&format!("passthru {passthru}"));
        }
        // par_reduce chaining a whole-span struct accumulator
        let pairs: Vec<(i64, i64)> = (0..3).map(|_| (self.lit(9), self.lit(9))).collect();
        let (pairs_name, folded) = (self.name("pairs"), self.name("folded"));
        let rows = pairs
            .iter()
            .map(|(l, r)| format!("pair {{ left: {l}, right: {r} }}"))
            .collect::<Vec<_>>()
            .join(", ");
        self.line(&format!("const {pairs_name}: list[pair] = [{rows}]"));
        self.line(&format!(
            "const {folded} = {pairs_name}.par_reduce(pair {{ left: 0, right: 0 }}, (a: pair, b: \
             pair) => pair {{ left: a.left * 10 + b.left + {bias_name}, right: a.right + b.right \
             }})"
        ));
        self.line(&format!(
            "println(\"folded ${{{folded}.left}} ${{{folded}.right}}\")"
        ));
        self.drawn.par_reduce += 1;
        self.drawn.closures += 1;
        self.drawn.multiword += 1;
        let (mut left, mut right) = (0, 0);
        for &(l, r) in &pairs {
            left = left * 10 + l + bias;
            right += r;
        }
        self.answer(&format!("folded {left} {right}"));
    }

    /// The free-generic episode: call sites into the ONE bodies over
    /// the drawn instantiations — the int and string chains through
    /// show_pair's staged evidence, the list chain through the generic
    /// impl, the two-level list[list[int]] instantiation through the
    /// follow chain, the boundless echo staging no evidence, and the
    /// tail driver — each answer computed by the same folds the
    /// serving bodies run.
    fn ep_free_generic(&mut self) {
        let (a, b) = (self.lit(9), self.lit(9));
        self.line(&format!("println(show_pair({a}, {b}))"));
        self.drawn.generic_calls += 1;
        self.answer(&format!("n={a}n={b}"));
        let w0 = WORDS[self.s.below(WORDS.len())];
        let w1 = WORDS[self.s.below(WORDS.len())];
        self.line(&format!("println(show_pair(\"{w0}\", \"{w1}\"))"));
        self.drawn.generic_calls += 1;
        self.answer(&format!("{w0}!{w1}!"));
        let elems: Vec<i64> = (0..2 + self.s.below(2)).map(|_| self.lit(9)).collect();
        let list = elems
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        self.line(&format!("println(shout([{list}]))"));
        self.drawn.generic_calls += 1;
        let mut shown = String::new();
        for e in &elems {
            let _ = write!(shown, "n={e};");
        }
        self.answer(&format!("l:{shown}"));
        let inner: Vec<i64> = (0..=self.s.below(2)).map(|_| self.lit(9)).collect();
        let inner_list = inner
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        self.line(&format!("println(shout([[{inner_list}]]))"));
        self.drawn.generic_calls += 1;
        self.drawn.two_level_calls += 1;
        let mut inner_shown = String::new();
        for e in &inner {
            let _ = write!(inner_shown, "n={e};");
        }
        self.answer(&format!("l:l:{inner_shown};"));
        let w2 = WORDS[self.s.below(WORDS.len())];
        self.line(&format!("println(echo(\"{w2}\"))"));
        self.drawn.generic_calls += 1;
        self.answer(w2);
        self.line(&format!("println(drive({a}))"));
        self.answer(&format!("n={a}"));
    }
}

/// Generates the program for one input: the source, its oracle stdout,
/// and the draw census. Deterministic per input.
pub fn generate(data: &[u8]) -> Program {
    let mut stream = Stream::new(data);
    let g = &mut Gen {
        s: &mut stream,
        decls: String::new(),
        body: String::new(),
        calls: String::new(),
        out: String::new(),
        drawn: Drawn::default(),
        fresh: 0,
    };
    g.decls.push_str(
        "tag int\ntag string\ntag bool\ntag none\n\nstruct pt { x: int, y: int }\nstruct wrap { \
         at: pt, id: int }\nstruct pair { left: int, right: int }\nstruct list[t]\n\nenum grade \
         {\n  plain\n  scored(int)\n}\n\nenum mark {\n  word(string)\n  empty\n}\n\nenum shape \
         {\n  dot\n  at(pt)\n}\n\nextern fn println (s: string) -> ()\nextern fn len[t] (xs: \
         list[t]) -> int\nextern fn list_get[t] (xs: list[t], i: int) -> t | none\nextern fn \
         par_map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]\nextern fn par_reduce[t] (xs: \
         list[t], init: t, f: fn(t, t) -> t) -> t\n\n",
    );
    g.decls.push_str(GENERIC_STORY);
    // the story's static facts: the three serving impl bodies, the
    // nested runtime form riding show_pair's own dot-call, drive's
    // tail-position call, and the inert uncalled declaration
    g.drawn.generic_impls += 3;
    g.drawn.generic_runtime_forms += 1;
    g.drawn.generic_tails += 1;
    g.drawn.inert_generics += 1;
    g.ep_scalar_ladder();
    g.seal_episode();
    g.ep_string_match();
    g.seal_episode();
    g.ep_struct_pattern();
    g.seal_episode();
    g.ep_case_payload();
    g.seal_episode();
    g.ep_string_payload();
    g.seal_episode();
    g.ep_struct_payload();
    g.seal_episode();
    g.ep_nested_sub_pattern();
    g.seal_episode();
    g.ep_union_equality();
    g.seal_episode();
    g.ep_fork_join();
    g.seal_episode();
    g.ep_free_generic();
    g.seal_episode();
    g.decls.push_str(
        "fn at_int(xs: list[int], i: int) -> int {\n  const cell = list_get(xs, i)\n  cell.ok((x: \
         int) => x).err((_: none) => -1)\n}\n\nfn pair_at(xs: list[pair], i: int) -> pair {\n  \
         const cell = list_get(xs, i)\n  cell.ok((p: pair) => p).err((_: none) => pair { left: 0, \
         right: 0 })\n}\n\nfn both(x: int) -> pair {\n  pair { left: x * 2, right: x * 3 }\n}\n",
    );
    let mut source = std::mem::take(&mut g.decls);
    source.push_str("\nfn main() -> () {\n");
    source.push_str(&g.calls);
    // the closer: a constant-string argument keeps the final tail
    // transfer count-free, so the heap-drain assert stays valid
    source.push_str("  println(\"done\")\n}\n");
    g.answer("done");
    Program {
        source,
        expected: std::mem::take(&mut g.out),
        drawn: std::mem::take(&mut g.drawn),
    }
}

/// Runs the whole chain over one fuzz input: parse, check, lower, module
/// round trip, install, run — and asserts the discipline: the generated
/// program parses and checks clean, lowers, round-trips, installs, main
/// returns unit, the captured answers equal the oracle, and the heap
/// drains. Returns the draw census for the mirror's coverage asserts.
pub fn exercise(data: &[u8]) -> Drawn {
    let program = generate(data);
    let mut files = Files::new();
    let id = files.add_source("gen_exec.ds", &program.source);
    let asts = [parse(&files, id).expect("the generated program parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the generated program checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        MODULE_NAME,
        Hash128::compute(program.source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the generated program lowers");
    let bytes = write(lowered.module());
    let back = read(&bytes).expect("the lowered module round-trips");
    assert_eq!(
        write(&back),
        bytes,
        "the reader round trip stays byte-stable"
    );
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, back).expect("the generated program installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run(MODULE_NAME, "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns unit"
    );
    assert_eq!(
        vm.captured_output(),
        program.expected.as_bytes(),
        "the observable answers match the generator's oracle"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
    program.drawn
}
