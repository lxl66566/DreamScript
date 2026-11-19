//! The shared source generator for the VM execution fuzz leg's match
//! and fork-join vocabulary: fuzz bytes become one well-typed DS program
//! — straight-line episodes over the pattern rounds' landed shapes and
//! the fork-join combinators — that runs the whole chain end to end:
//! parse, the derive-expanding check, lower, module round trip, loader
//! install, VM run, and the captured answers must equal the generator's
//! own oracle, with the heap drained at the end. Included by the
//! cargo-fuzz target `fuzz_vm` and the nextest mirror in
//! `crates/ds-lower/tests/fuzz_exec_gen.rs`.
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
//! The json derive roundtrip rides the same chain: a struct with an
//! optional list field (`list[int]?`) encodes through to_json's
//! synthesized member and decodes back through from_json's (the
//! optional list field's statement-position member row), over the
//! typed json vocabulary the episode declares itself (the json enum's
//! four cases and the reader fns mirroring the ambient's spellings,
//! so the run stays hermetic — no ambient module installs), the
//! oracle asserting the presence and absence roundtrips, each leg
//! counted in the census.
//!
//! The cross-package linked-enum episode rides the chain as a
//! two-module compile: the generated library member — a non-generic
//! enum (unit cases plus one struct-payload case over the word
//! vocabulary, the payload's second field kind drawn) exported beside
//! three signatures crossing the boundary both ways — lowers as its
//! own module and installs under the dep name, and the consumer
//! program imports it through a real linked surface: the link walk
//! registers the enum and each case like a local declaration, the
//! module's own tag rows seed the consumer's table at lowering, and
//! union-typed answers cross both directions over the agreed ids —
//! the consumer matches, narrows, and constructs over the linked type,
//! the producer scoring consumer-made values and the consumer scoring
//! producer-made ones, each leg counted in the census. A drawn variant
//! keeps the lowering's honest refusal in the set: a second linked
//! surface claiming a tag id the first already seeded refuses at
//! lowering (two packages minting independently collide on low ids —
//! the code immediates are frozen in their artifacts), the input never
//! running.
//!
//! The bytes representation's structural equality rides the
//! union-equality episode's carrier shapes: a bytes field inside a
//! struct compared beside an int field and a nested struct's own bytes
//! field (the per-word chain dispatching on the field's type, never its
//! slot kind), `in` membership over a `list[bytes]` element, and the
//! plain operand pair — each counted in the census.
//!
//! Termination (why every generated run stops by itself): main is
//! straight-line (no recursion; each episode lives in its own helper
//! function, so every frame stays under the width cap; the loop
//! families are the generic serving impl's for-in over a fixed-length
//! literal list of at most three elements and from_json's synthesized
//! array walk, whose trip count mirrors the encoded field's own list
//! length — at most two — so every iteration count is drawn and
//! bounded), the
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

use std::{collections::HashMap, fmt::Write as _, sync::Arc};

use ds_check::{
    EffectRow, FnSig, ForeignEntry, ForeignKind, ImportGraph, ImportSurface, MemberSurface,
    TypeFact,
};
use ds_diag::{FileId, Files, Span, human};
use ds_lower::{ComptimeValues, LowerError, lower, lower_library};
use ds_meta::{analyze, analyze_owned_linked};
use ds_mod::{Hash128, Module, read, write};
use ds_parse::{Type, parse};
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// The generated module's name (the run entry target).
pub const MODULE_NAME: &str = "fze";

/// The linked library member's module name: the dep name the consumer's
/// import links and the name the library module installs under.
const LIB_MODULE: &str = "@lib";

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
    /// Bytes-typed equality chains: struct fields (flat and nested),
    /// list membership, and plain operand pairs over `bytes` words —
    /// beside the list and map collection words riding the same
    /// per-word chains (fields, nesting, membership, the map's rows).
    pub bytes_equality: u32,
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
    /// Value blocks whose tail reads a local binder — the staged
    /// claim the outer consumer takes (var and const binders alike).
    pub block_tails: u32,
    /// Json derive roundtrip legs: a struct with an optional list
    /// field encoded through to_json and decoded back through
    /// from_json (presence and absence).
    pub json_derive: u32,
    /// Linked-enum episodes: a two-module compile where the consumer
    /// constructs, matches, narrows, and scores over the linked type
    /// while union-typed answers cross the boundary both directions.
    pub linked_enums: u32,
    /// The linked surface's honest refusal drawn: a second package
    /// claiming a seeded tag id refusing at lowering.
    pub linked_refusals: u32,
}

/// One generated program: its source, the oracle stdout, and the census.
pub struct Program {
    pub source: String,
    pub expected: String,
    pub drawn: Drawn,
    /// The linked library member's source: the enum and signatures the
    /// consumer's import binds through, compiled and installed as its
    /// own module ahead of the consumer.
    pub library: String,
    /// Whether the conflict variant drew: the second linked surface
    /// whose one-case enum's tag row claims an id the first already
    /// seeded, the input refusing at lowering and never running.
    pub linked_conflict: bool,
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

/// The payload word kinds the linked-enum episode draws over: the
/// scalar atoms plus a list of one scalar — the vocabulary a linked
/// struct-payload case's fields spell in.
#[derive(Clone, Copy)]
enum LinkedWord {
    Int,
    Str,
    Bool,
    Char,
    Byte,
    Float,
    ListInt,
}

const LINKED_WORDS: [LinkedWord; 7] = [
    LinkedWord::Int,
    LinkedWord::Str,
    LinkedWord::Bool,
    LinkedWord::Char,
    LinkedWord::Byte,
    LinkedWord::Float,
    LinkedWord::ListInt,
];

impl LinkedWord {
    /// The payload field's type spelling inside the library's enum.
    const fn spelling(self) -> &'static str {
        match self {
            Self::Int => "int",
            Self::Str => "string",
            Self::Bool => "bool",
            Self::Char => "char",
            Self::Byte => "byte",
            Self::Float => "float",
            Self::ListInt => "list[int]",
        }
    }

    /// The tag the consumer's own prelude does not declare, when the
    /// drawn word needs one (the prelude carries int, string, bool,
    /// none, byte, and bytes — char and float must arrive ahead of the
    /// linked fact that spells them).
    const fn head_tag(self) -> Option<&'static str> {
        match self {
            Self::Char => Some("char"),
            Self::Float => Some("float"),
            _ => None,
        }
    }

    /// The library's own tag declaration for the word, when it names
    /// one beside `int` (the consumer's prelude already declares every
    /// kind but float, which the episode's import head adds).
    const fn lib_tag(self) -> Option<&'static str> {
        match self {
            Self::Int | Self::ListInt => None, // the list struct declaration covers the list word
            Self::Str => Some("string"),
            Self::Bool => Some("bool"),
            Self::Char => Some("char"),
            Self::Byte => Some("byte"),
            Self::Float => Some("float"),
        }
    }
}

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
    /// The linked library member's source, built by the episode.
    lib: String,
    /// The consumer's import head: the linked rows (and the conflict
    /// variant's second row) emitted ahead of every declaration.
    imports: String,
    /// Whether the conflict variant drew for this input.
    conflict: bool,
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

    /// The block tail-read claim episode: a value block whose tail reads
    /// a local binder answers the binder's own words, and the block's
    /// scope end runs before the consumer claims it, so the tail read
    /// stages the answer's claim past the scope's release — drawn over
    /// a var binder's slot and a const binder's alias, the count
    /// releasing at the consumer's own use.
    fn ep_block_tail(&mut self) {
        let elems: Vec<i64> = (0..3).map(|_| self.lit(9)).collect();
        let list = elems
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let (xs, ys) = (self.name("xs"), self.name("ys"));
        self.line(&format!("const {ys} = {{"));
        self.line(&format!("  var {xs} = [{list}]"));
        self.line(&format!("  {xs}"));
        self.line("}");
        self.line(&format!("println(\"len ${{len({ys})}}\")"));
        self.drawn.block_tails += 1;
        self.answer(&format!("len {}", elems.len()));
        let (cs, ws) = (self.name("cs"), self.name("ws"));
        self.line(&format!("const {ws} = {{"));
        self.line(&format!("  const {cs} = concat(\"a\", \"b\")"));
        self.line(&format!("  {cs}"));
        self.line("}");
        self.line(&format!("println(\"str ${{{ws}}}\")"));
        self.drawn.block_tails += 1;
        self.answer("str ab");
    }

    /// The bytes-equality episode: the per-word equality chains over
    /// bytes-typed parts dispatch on the field's type, never its slot
    /// kind — a flat bytes field beside an int field, a nested struct's
    /// own bytes field, `in` membership over `list[bytes]`, and the
    /// plain operand pair, all over fresh `bytes_of` constructions so
    /// equal content rides distinct objects. The fail-fast shapes ride
    /// both spellings too: a chain that answers at an earlier field must
    /// still release the counts the skipped Ref-carrying fields staged
    /// (bytes or string, first field or behind a differing one).
    fn ep_bytes_equality(&mut self) {
        const LETTERS: [char; 3] = ['a', 'b', 'c'];
        let head = self.s.below(3);
        let tailed = self.s.roll(2);
        // three pairwise-distinct payloads: the drawn one, its last byte
        // replaced, its first byte replaced
        let spell = |bytes: &[usize]| {
            let elems = bytes
                .iter()
                .map(|&i| format!("b'{}'", LETTERS[i]))
                .collect::<Vec<_>>()
                .join(", ");
            format!("bytes_of([{elems}])")
        };
        let base: Vec<usize> = if tailed { vec![head, (head + 1) % 3] } else { vec![head] };
        let mut last_flip = base.clone();
        *last_flip.last_mut().expect("nonempty") = (*base.last().expect("nonempty") + 1) % 3;
        let mut first_flip = base.clone();
        first_flip[0] = (head + 2) % 3;
        let (one, two, deepdiff, firstdiff, xs) = (
            self.name("one"),
            self.name("two"),
            self.name("deepdiff"),
            self.name("firstdiff"),
            self.name("xs"),
        );
        let deep = format!("binner {{ b: {} }}", spell(&base));
        self.line(&format!(
            "const {one} = bpair {{ b: {}, n: 1, deep: {deep} }}",
            spell(&base)
        ));
        self.line(&format!(
            "const {two} = bpair {{ b: {}, n: 1, deep: {deep} }}",
            spell(&base)
        ));
        self.line(&format!(
            "const {deepdiff} = bpair {{ b: {}, n: 1, deep: binner {{ b: {} }} }}",
            spell(&base),
            spell(&last_flip)
        ));
        self.line(&format!(
            "const {firstdiff} = bpair {{ b: {}, n: 1, deep: binner {{ b: {} }} }}",
            spell(&first_flip),
            spell(&base)
        ));
        // the chain answers at an earlier field over both spellings: the
        // skipped Ref-carrying fields' staged counts release at the
        // chain's merged exits, so the drain discipline holds for the
        // fail-fast shapes too
        self.line(&format!(
            "println(\"${{{one} == {two}}} ${{{one} != {two}}}\")"
        ));
        self.line(&format!(
            "if {one} == {deepdiff} {{ println(\"bad\") }} else {{ println(\"deep no\") }}"
        ));
        self.line(&format!(
            "if {one} == {firstdiff} {{ println(\"bad\") }} else {{ println(\"first no\") }}"
        ));
        self.line(&format!(
            "println(\"${{{one} == {firstdiff}}} ${{{one} != {firstdiff}}}\")"
        ));
        let (flat_one, flat_two) = (self.name("fa"), self.name("fb"));
        self.line(&format!(
            "const {flat_one} = bflat {{ b: {}, n: 1 }}",
            spell(&base)
        ));
        self.line(&format!(
            "const {flat_two} = bflat {{ b: {}, n: 1 }}",
            spell(&last_flip)
        ));
        self.line(&format!(
            "if {flat_one} == {flat_two} {{ println(\"bad\") }} else {{ println(\"flat no\") }}"
        ));
        self.line(&format!(
            "println(\"${{{flat_one} == {flat_two}}} ${{{flat_one} != {flat_two}}}\")"
        ));
        let (s_one, s_two) = (self.name("sa"), self.name("sb"));
        self.line(&format!(
            "const {s_one} = spair {{ n: 1, s: \"{}{}\" }}",
            LETTERS[head],
            LETTERS[(head + 1) % 3]
        ));
        self.line(&format!(
            "const {s_two} = spair {{ n: 2, s: \"{}{}\" }}",
            LETTERS[head],
            LETTERS[(head + 1) % 3]
        ));
        self.line(&format!(
            "if {s_one} == {s_two} {{ println(\"bad\") }} else {{ println(\"str no\") }}"
        ));
        self.line(&format!(
            "println(\"${{{s_one} == {s_two}}} ${{{s_one} != {s_two}}}\")"
        ));
        self.line(&format!(
            "const {xs} = [{}, {}]",
            spell(&base),
            spell(&last_flip)
        ));
        let (hit, miss, plain_a, plain_b) = (
            self.name("hit"),
            self.name("miss"),
            self.name("pa"),
            self.name("pb"),
        );
        self.line(&format!("const {hit} = {}", spell(&base)));
        self.line(&format!("const {miss} = {}", spell(&first_flip)));
        self.line(&format!(
            "println(\"${{{hit} in {xs}}} ${{{miss} in {xs}}}\")"
        ));
        self.line(&format!("const {plain_a} = {}", spell(&base)));
        self.line(&format!("const {plain_b} = {}", spell(&last_flip)));
        self.line(&format!(
            "println(\"${{{plain_a} == {plain_b}}} ${{{plain_a} != {plain_b}}}\")"
        ));
        self.drawn.bytes_equality += 16;
        self.answer("true false");
        self.answer("deep no");
        self.answer("first no");
        self.answer("false true");
        self.answer("flat no");
        self.answer("false true");
        self.answer("str no");
        self.answer("false true");
        self.answer("true false");
        self.answer("false true");
    }

    /// The collection-equality episode: the list and map collection words
    /// ride the same per-word chains the bytes word does — the Ref word
    /// dispatches the list content compare over drawn content (a
    /// list[string] field beside a nested list[bytes] field, a list of
    /// lists, membership over one), the fail-fast shape releasing the
    /// skipped collection fields' staged counts at the merged exits, and
    /// the map's rows comparing through the lookup face over an int
    /// value and a nested list value. The mirror asserts the drain over
    /// every spelling.
    fn ep_collection_equality(&mut self) {
        const LETTERS: [char; 3] = ['a', 'b', 'c'];
        let head = self.s.below(3);
        let (front, back, alien) = (
            LETTERS[head],
            LETTERS[(head + 1) % 3],
            LETTERS[(head + 2) % 3],
        );
        let (l_one, l_two, l_deep_diff) =
            (self.name("lone"), self.name("ltwo"), self.name("ldeep"));
        self.line(&format!(
            "const {l_one} = lpair {{ l: [\"{front}\", \"{back}\"], n: 1, deep: linner {{ l: \
             [bytes_of([b'{back}'])] }} }}"
        ));
        self.line(&format!(
            "const {l_two} = lpair {{ l: [\"{front}\", \"{back}\"], n: 1, deep: linner {{ l: \
             [bytes_of([b'{back}'])] }} }}"
        ));
        self.line(&format!(
            "const {l_deep_diff} = lpair {{ l: [\"{front}\", \"{back}\"], n: 1, deep: linner {{ \
             l: [bytes_of([b'{alien}'])] }} }}"
        ));
        self.line(&format!(
            "println(\"${{{l_one} == {l_two}}} ${{{l_one} != {l_two}}}\")"
        ));
        self.line(&format!(
            "if {l_one} == {l_deep_diff} {{ println(\"bad\") }} else {{ println(\"ldeep no\") }}"
        ));
        let (nest_one, nest_two, nest_diff) =
            (self.name("nest"), self.name("nest2"), self.name("nestd"));
        let (r0, r1, r2) = (self.lit(9), self.lit(9), self.lit(9));
        self.line(&format!("const {nest_one} = [[{r0}, {r1}], [{r2}]]"));
        self.line(&format!("const {nest_two} = [[{r0}, {r1}], [{r2}]]"));
        // the differing nest's second row is one element longer, so it
        // never matches whatever the drawn values are
        self.line(&format!("const {nest_diff} = [[{r0}, {r1}], [{r2}, {r2}]]"));
        self.line(&format!(
            "println(\"${{{nest_one} == {nest_two}}} ${{{nest_one} == {nest_diff}}}\")"
        ));
        let (row_hit, row_miss) = (self.name("rowh"), self.name("rowm"));
        self.line(&format!("const {row_hit} = [{r0}, {r1}]"));
        // the miss row is one element longer than every row, so it never
        // matches whatever the drawn values are
        self.line(&format!("const {row_miss} = [{r0}, {r1}, {r1}]"));
        self.line(&format!(
            "println(\"${{{row_hit} in {nest_one}}} ${{{row_miss} in {nest_one}}}\")"
        ));
        let (m_one, m_two, m_diff) = (self.name("m1"), self.name("m2"), self.name("md"));
        for (name, value) in [(&m_one, 1), (&m_two, 1), (&m_diff, 2)] {
            self.line(&format!("var {name} = map[string, int]()"));
            self.line(&format!("map_set({name}, \"{front}\", {value})"));
        }
        self.line(&format!(
            "println(\"${{{m_one} == {m_two}}} ${{{m_one} == {m_diff}}}\")"
        ));
        let (lmap_one, lmap_two, ln) = (self.name("lm1"), self.name("lm2"), self.lit(9));
        for name in [&lmap_one, &lmap_two] {
            self.line(&format!("var {name} = map[string, list[int]]()"));
            self.line(&format!("map_set({name}, \"{back}\", [{ln}, {ln}])"));
        }
        self.line(&format!("println(\"${{{lmap_one} == {lmap_two}}}\")"));
        self.drawn.bytes_equality += 10;
        self.answer("true false");
        self.answer("ldeep no");
        self.answer("true false");
        self.answer("true false");
        self.answer("true false");
        self.answer("true");
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

    /// The json derive roundtrip episode: a struct with an optional
    /// list field encodes through to_json's synthesized member and
    /// decodes back through from_json's (the optional list field's
    /// statement-position member row), over the typed json vocabulary
    /// the episode declares itself — the json enum's four cases and the
    /// reader fns mirroring the ambient's spellings, so the chain stays
    /// hermetic (no ambient module installs). The oracle asserts the
    /// presence and absence roundtrips over the drawn element values.
    fn ep_json_derive(&mut self) {
        let (a, b) = (self.lit(10), self.lit_not(10, &[0]));
        self.decl("tag parse_err");
        self.decl(
            "enum json {\n  json_null\n  json_int { value: int }\n  json_array { items: \
             list[json] }\n  json_object { fields: map[string, json] }\n}",
        );
        self.decl(
            "fn json_member (v: json, key: string) -> json | none {\n  match v {\n  json_object { \
             fields } => {\n      const m = map_get(fields, key)\n      m\n    },\n    _ => \
             none,\n  }\n}",
        );
        self.decl(
            "fn json_as_object (v: json) -> map[string, json] | parse_err {\n  match v {\n  \
             json_object { fields } => fields,\n    _ => parse_err,\n  }\n}",
        );
        self.decl(
            "fn json_as_array (v: json) -> list[json] | parse_err {\n  match v {\n    json_array \
             { items } => items,\n    _ => parse_err,\n  }\n}",
        );
        self.decl(
            "fn json_as_int (v: json) -> int | parse_err {\n  match v {\n    json_int { value } \
             => value,\n    _ => parse_err,\n  }\n}",
        );
        self.decl("extern fn unwrap_or[t] (x: t?, y: t) -> t");
        self.decl("extern fn list_len[t] (xs: list[t]) -> int");
        self.decl("extern fn list_push[t] (xs: mut list[t], x: t) -> ()");
        self.decl("extern fn map_get[k, v] (m: map[k, v], key: k) -> v | none");
        self.decl("#[derive(from_json, to_json)]\nstruct slack { xs: list[int]? }");
        let (sp, sa) = (self.name("sp"), self.name("sa"));
        self.line(&format!("const {sp} = slack {{ xs: [{a}, {b}] }}"));
        self.line(&format!("const {sa} = slack {{ xs: none }}"));
        // presence: encode, decode, and observe the decoded list
        let (j, d, back, hit) = (
            self.name("j"),
            self.name("d"),
            self.name("bk"),
            self.name("hit"),
        );
        self.line(&format!("const {j} = slack.to_json({sp})"));
        self.line(&format!("const {d} = slack.from_json({j})"));
        self.line(&format!(
            "const {back} = {d}.ok((s: slack) => s).err((_: parse_err) => slack {{ xs: none }})"
        ));
        self.line(&format!("const {hit} = {back}.xs.unwrap_or(list[int]())"));
        self.line(&format!(
            "println(\"presence ${{len({hit})}} ${{at_int({hit}, 0)}} ${{at_int({hit}, 1)}}\")"
        ));
        self.drawn.json_derive += 1;
        self.answer(&format!("presence 2 {a} {b}"));
        // absence: the absent member decodes back to none
        let (j2, d2, back2, miss) = (
            self.name("j"),
            self.name("d"),
            self.name("bk"),
            self.name("miss"),
        );
        self.line(&format!("const {j2} = slack.to_json({sa})"));
        self.line(&format!("const {d2} = slack.from_json({j2})"));
        self.line(&format!(
            "const {back2} = {d2}.ok((s: slack) => s).err((_: parse_err) => slack {{ xs: none }})"
        ));
        self.line(&format!("const {miss} = {back2}.xs.unwrap_or(list[int]())"));
        self.line(&format!("println(\"absence ${{len({miss})}}\")"));
        self.drawn.json_derive += 1;
        self.answer("absence 0");
    }

    /// One drawn int in -4..=4: the linked producers' sign-sensitive
    /// input, so the sweep covers every case arm (positive, zero,
    /// negative) across seeds.
    fn linked_k(&mut self) -> i64 {
        self.lit(9) - 4
    }

    /// One drawn value literal of the payload's second-field kind, for
    /// the consumer's own constructions.
    fn linked_lit(&mut self, word: LinkedWord) -> String {
        match word {
            LinkedWord::Int => self.lit(9).to_string(),
            LinkedWord::Str => {
                let w = WORDS[self.s.below(WORDS.len())];
                format!("\"{w}\"")
            },
            LinkedWord::Bool => "true".to_owned(),
            LinkedWord::Char => "'q'".to_owned(),
            LinkedWord::Byte => "b'z'".to_owned(),
            LinkedWord::Float => "1.5".to_owned(),
            LinkedWord::ListInt => {
                let (a, b) = (self.lit(9), self.lit(9));
                format!("[{a}, {b}]")
            },
        }
    }

    /// The linked-enum episode: the library member's source (a
    /// non-generic enum — unit cases plus one struct-payload case whose
    /// second field kind is drawn — exported beside three signatures
    /// crossing the boundary both ways) into `lib`, the consumer's
    /// import rows into `imports`, and the consumer episode's body —
    /// construction, match, and `is`-narrowing over the linked type,
    /// producer answers scored over consumer-made values and the
    /// consumer's own matches over producer-made ones, the optional
    /// signature crossing through the seeded ids. The conflict variant
    /// (drawn) adds a second import row selecting a one-case enum whose
    /// tag row claims an id the first library already seeded: the input
    /// refuses at lowering and never runs, keeping the honest refusal
    /// in the draw set.
    fn ep_linked_enum(&mut self) {
        let word = LINKED_WORDS[self.s.below(LINKED_WORDS.len())];
        self.conflict = self.s.roll(5);
        // the library member: its own tag vocabulary, the enum, and the
        // signatures — `pick` constructs by sign, `score` matches, `opt`
        // answers the optional over the same type
        let lib_lit = match word {
            LinkedWord::Int => "1".to_owned(),
            LinkedWord::Str => "\"made\"".to_owned(),
            LinkedWord::Bool => "true".to_owned(),
            LinkedWord::Char => "'m'".to_owned(),
            LinkedWord::Byte => "b'm'".to_owned(),
            LinkedWord::Float => "0.5".to_owned(),
            LinkedWord::ListInt => "[1, 2]".to_owned(),
        };
        let mut lib = String::from("tag int\n");
        if let Some(tag) = word.lib_tag() {
            let _ = writeln!(lib, "tag {tag}");
        }
        let _ = writeln!(lib, "tag none");
        if matches!(word, LinkedWord::ListInt) {
            let _ = writeln!(lib, "struct list[t]");
        }
        let _ = write!(
            lib,
            "\npub enum tone {{\n  quiet,\n  loud,\n  tagged {{ n: int, w: {} }},\n}}\n\n",
            word.spelling()
        );
        let _ = write!(
            lib,
            concat!(
                "pub fn pick (k: int) -> tone {{\n",
                "  if k > 0 {{\n",
                "    tagged(k, {lit})\n",
                "  }} else {{\n",
                "    if k == 0 {{\n",
                "      quiet\n",
                "    }} else {{\n",
                "      loud\n",
                "    }}\n",
                "  }}\n",
                "}}\n\n",
                "pub fn score (t: tone) -> int {{\n",
                "  match t {{\n",
                "    quiet => 0,\n",
                "    loud => 1,\n",
                "    tagged(n, _) => n,\n",
                "  }}\n",
                "}}\n\n",
                "pub fn opt (k: int) -> tone? {{\n",
                "  if k > 0 {{\n",
                "    tagged(k, {lit})\n",
                "  }} else {{\n",
                "    none\n",
                "  }}\n",
                "}}\n",
            ),
            lit = lib_lit
        );
        self.lib = lib;
        // the consumer's import head: the linked row (plus the word's
        // tag when the prelude does not declare it, and the conflict
        // variant's second row)
        let mut imports =
            String::from("import @lib (tone, quiet, loud, tagged, pick, score, opt)\n");
        if let Some(tag) = word.head_tag() {
            let _ = writeln!(imports, "tag {tag}");
        }
        if self.conflict {
            imports.push_str("import @other (spare, clash)\n");
            self.drawn.linked_refusals += 1;
        }
        self.imports = imports;
        // the consumer episode: the producer's score over its own
        // construction, the consumer's match over a producer value, the
        // consumer's construction scored both ways, the narrowing, and
        // the optional crossing
        let k1 = self.linked_k();
        self.line(&format!("println(\"pick ${{score(pick({k1}))}}\")"));
        self.answer(&format!("pick {}", match k1 {
            1.. => k1,
            0 => 0,
            _ => 1,
        }));
        let k2 = self.linked_k();
        let local = self.name("lm");
        self.line(&format!(
            "const {local} = match pick({k2}) {{ quiet => 1, loud => 2, tagged(n, _) => n + 10 }}"
        ));
        self.line(&format!("println(\"local ${{{local}}}\")"));
        self.answer(&format!("local {}", match k2 {
            1.. => k2 + 10,
            0 => 1,
            _ => 2,
        }));
        let own_n = self.lit(9);
        let own = self.name("own");
        let own_lit = self.linked_lit(word);
        self.line(&format!("const {own}: tone = tagged({own_n}, {own_lit})"));
        let ownhit = self.name("oh");
        self.line(&format!(
            "const {ownhit} = match {own} {{ quiet => 1, loud => 2, tagged(n, _) => n + 10 }}"
        ));
        self.line(&format!("println(\"own ${{score({own})}} ${{{ownhit}}}\")"));
        self.answer(&format!("own {own_n} {}", own_n + 10));
        let k3 = self.linked_k();
        let narrow = self.name("nw");
        let probe = self.name("pr");
        self.line(&format!("var {narrow} = 0"));
        self.line(&format!("const {probe} = pick({k3})"));
        self.line(&format!("if {probe} is tagged {{ {narrow} = {probe}.n }}"));
        self.line(&format!("println(\"narrow ${{{narrow}}}\")"));
        self.answer(&format!("narrow {}", if k3 > 0 { k3 } else { 0 }));
        let k4 = self.linked_k();
        let opthit = self.name("op");
        self.line(&format!(
            "const {opthit} = match opt({k4}) {{ none => 0, quiet => 0, loud => 1, tagged(n, _) \
             => n }}"
        ));
        self.line(&format!("println(\"opt ${{{opthit}}}\")"));
        self.answer(&format!("opt {}", if k4 > 0 { k4 } else { 0 }));
        self.drawn.linked_enums += 1;
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
        lib: String::new(),
        imports: String::new(),
        conflict: false,
    };
    g.decls.push_str(
        "tag int\ntag string\ntag bool\ntag none\ntag byte\ntag bytes\n\nstruct pt { x: int, y: \
         int }\nstruct wrap { at: pt, id: int }\nstruct pair { left: int, right: int }\nstruct \
         list[t]\nstruct binner { b: bytes }\nstruct bpair { b: bytes, n: int, deep: binner \
         }\nstruct bflat { b: bytes, n: int }\nstruct spair { n: int, s: string }\nstruct linner \
         { l: list[bytes] }\nstruct lpair { l: list[string], n: int, deep: linner }\nstruct \
         map[k, v]\n\nenum grade {\n  plain\n  scored(int)\n}\n\nenum mark {\n  word(string)\n  \
         empty\n}\n\nenum shape {\n  dot\n  at(pt)\n}\n\nextern fn println (s: string) -> \
         ()\nextern fn bytes_of (xs: list[byte]) -> bytes\nextern fn len[t] (xs: list[t]) -> \
         int\nextern fn list_get[t] (xs: list[t], i: int) -> t | none\nextern fn par_map[t, r] \
         (xs: list[t], f: fn(t) -> r) -> list[r]\nextern fn par_reduce[t] (xs: list[t], init: t, \
         f: fn(t, t) -> t) -> t\nextern fn map_new[k, v] () -> map[k, v]\nextern fn map_set[k, v] \
         (m: mut map[k, v], key: k, value: v) -> ()\n\n",
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
    g.ep_bytes_equality();
    g.seal_episode();
    g.ep_collection_equality();
    g.seal_episode();
    g.ep_fork_join();
    g.seal_episode();
    g.ep_block_tail();
    g.seal_episode();
    g.ep_free_generic();
    g.seal_episode();
    g.ep_json_derive();
    g.seal_episode();
    g.ep_linked_enum();
    g.seal_episode();
    g.decls.push_str(
        "fn at_int(xs: list[int], i: int) -> int {\n  const cell = list_get(xs, i)\n  cell.ok((x: \
         int) => x).err((_: none) => -1)\n}\n\nfn pair_at(xs: list[pair], i: int) -> pair {\n  \
         const cell = list_get(xs, i)\n  cell.ok((p: pair) => p).err((_: none) => pair { left: 0, \
         right: 0 })\n}\n\nfn both(x: int) -> pair {\n  pair { left: x * 2, right: x * 3 }\n}\n",
    );
    let mut source = std::mem::take(&mut g.imports);
    source.push('\n');
    source.push_str(&std::mem::take(&mut g.decls));
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
        library: std::mem::take(&mut g.lib),
        linked_conflict: g.conflict,
    }
}

/// Maps one span's file through the registry map; a span whose file
/// does not map keeps its id.
fn rebase_span(span: Span, map: &HashMap<FileId, FileId>) -> Span {
    match map.get(&span.file) {
        Some(file) => Span::new(*file, span.start, span.end),
        None => span,
    }
}

/// Rebase one written type's spans through the registry map.
fn rebase_type(ty: &mut Type, map: &HashMap<FileId, FileId>) {
    match ty {
        Type::Path { span, args, .. } => {
            *span = rebase_span(*span, map);
            for arg in args {
                rebase_type(arg, map);
            }
        },
        Type::Optional { span, inner }
        | Type::Negation { span, inner }
        | Type::Intersect {
            span, left: inner, ..
        }
        | Type::Difference {
            span, left: inner, ..
        }
        | Type::Paren { span, inner }
        | Type::Ptr { span, inner, .. } => {
            *span = rebase_span(*span, map);
            rebase_type(inner, map);
        },
        Type::Union { span, left, right } => {
            *span = rebase_span(*span, map);
            rebase_type(left, map);
            rebase_type(right, map);
        },
        Type::Tuple { span, items } => {
            *span = rebase_span(*span, map);
            for item in items {
                rebase_type(item, map);
            }
        },
        Type::Unit(span) | Type::TypeKw(span) | Type::Foreign { span, .. } => {
            *span = rebase_span(*span, map);
        },
        Type::Fn { span, params, ret } => {
            *span = rebase_span(*span, map);
            for param in params {
                param.name.span = rebase_span(param.name.span, map);
                if let Some(ty) = &mut param.ty {
                    rebase_type(ty, map);
                }
            }
            if let Some(ret) = ret {
                rebase_type(ret, map);
            }
        },
    }
}

/// Rebases one signature's span set across file registries: every span
/// and written type names the consuming compile's own registry, so the
/// diagnostics a link pass mints render there.
fn rebase_sig(sig: &Arc<FnSig>, map: &HashMap<FileId, FileId>) -> Arc<FnSig> {
    let mut sig = FnSig {
        generics: sig.generics.clone(),
        params: sig.params.clone(),
        ret: sig.ret.clone(),
    };
    for generic in &mut sig.generics {
        generic.span = rebase_span(generic.span, map);
        if let Some(bound) = &mut generic.bound {
            rebase_type(bound, map);
        }
    }
    for param in &mut sig.params {
        param.span = rebase_span(param.span, map);
        if let Some(ty) = &mut param.ty {
            rebase_type(ty, map);
        }
    }
    if let Some(ret) = &mut sig.ret {
        rebase_type(ret, map);
    }
    Arc::new(sig)
}

/// The linked library's export surface: the enum's tag-row fact, each
/// case's own row, and the three signatures crossing the boundary,
/// with the function indexes the consumer's calls address and the
/// module's own tag-table rows riding beside them (the shared tag
/// mapping's identity, seeded into the consumer's table at lowering).
/// Every span rebases into the consuming compile's registry through
/// `map`.
fn linked_surface(
    lib: &ds_check::Analyzed,
    lowered: &ds_lower::Lowered,
    module: &Module,
    map: &HashMap<FileId, FileId>,
) -> MemberSurface {
    let entry = |name: &str,
                 def: ds_check::DefId,
                 kind: ForeignKind,
                 sig: Option<Arc<FnSig>>,
                 ty: Option<Arc<TypeFact>>,
                 func: Option<u32>| {
        let def_row = lib.package.def(def);
        ForeignEntry {
            name: name.to_owned(),
            vis: def_row.vis,
            kind,
            generic: false,
            defaulted: false,
            decl_span: rebase_span(def_row.span, map),
            sig,
            row: EffectRow::default(),
            caps: 0,
            value: None,
            ty,
            link: 0,
            func,
            std: false,
        }
    };
    let tone = lib
        .package
        .lookup_type("tone")
        .expect("the library exports the enum");
    let enum_fact = Arc::new(
        TypeFact::of_enum(&lib.package, &lib.decls, tone).expect("the enum's fact carries"),
    );
    let mut entries = vec![entry(
        "tone",
        tone,
        ForeignKind::Type,
        None,
        Some(enum_fact),
        None,
    )];
    for case in ["quiet", "loud", "tagged"] {
        let def = lib.package.lookup_type(case).expect("the case registers");
        let fact =
            Arc::new(TypeFact::of_case(&lib.decls, def, "tone").expect("the case's fact carries"));
        entries.push(entry(case, def, ForeignKind::Type, None, Some(fact), None));
    }
    for name in ["pick", "score", "opt"] {
        let def = lib.package.lookup_value(name).expect("the fn registers");
        let sig = lib.decls.fns.get(&def).map(|sig| rebase_sig(sig, map));
        let func = lowered
            .functions()
            .iter()
            .position(|f| f == name)
            .map(|at| u32::try_from(at).expect("function tables are tiny"));
        entries.push(entry(name, def, ForeignKind::Fn, sig, None, func));
    }
    let tags = module
        .tags()
        .iter()
        .map(|row| (row.name.clone(), row.id))
        .collect();
    MemberSurface { entries, tags }
}

/// Runs the whole chain over one fuzz input: parse, check, lower, module
/// round trip, install, run — and asserts the discipline: the generated
/// program parses and checks clean, lowers, round-trips, installs, main
/// returns unit, the captured answers equal the oracle, and the heap
/// drains. The linked-enum episode makes the chain a two-module compile:
/// the library member analyzes, lowers, and round-trips as its own
/// module first, its export surface serves the consumer's import row
/// through the link walk, and the consumer's module links it as its one
/// recorded dep. The conflict variant refuses at lowering and never
/// runs. Returns the draw census for the mirror's coverage asserts.
pub fn exercise(data: &[u8]) -> Drawn {
    let program = generate(data);
    // the library compiles in its own registry — a linked package's
    // member compile is its own unit — and the consumer in a fresh one
    // that registers the library's text after its own source, the
    // pipeline's own order (a compile unit's files sit contiguous from
    // zero, the linked members' texts after them, their spans rebasing
    // into the consuming registry)
    let mut lib_files = Files::new();
    let lib_file = lib_files.add_source("@lib/lib.ds", &program.library);
    let lib_ast = Arc::new(parse(&lib_files, lib_file).expect("the library parses"));
    let lib = analyze(&lib_files, std::slice::from_ref(&lib_ast));
    assert!(
        lib.checked.diags.is_empty(),
        "the library checks clean: {}",
        lib.checked
            .diags
            .iter()
            .map(|diag| human(diag, &lib_files))
            .collect::<String>()
    );
    let lib_hash = Hash128::compute(program.library.as_bytes());
    let lib_lowered = lower_library(
        LIB_MODULE,
        lib_hash,
        &[lib_ast],
        &lib,
        None,
        None,
        &ComptimeValues::default(),
    )
    .expect("the library lowers");
    let lib_bytes = write(lib_lowered.module());
    let lib_back = read(&lib_bytes).expect("the library round-trips");
    assert_eq!(
        write(&lib_back),
        lib_bytes,
        "the library round trip stays byte-stable"
    );
    let mut files = Files::new();
    let id = files.add_source("gen_exec.ds", &program.source);
    let member = files.add_source("@lib/lib.ds", &program.library);
    let mut rebase = HashMap::new();
    rebase.insert(lib_file, member);
    let surface = linked_surface(&lib, &lib_lowered, &lib_back, &rebase);
    // the conflict variant's second surface: a linked package exporting a
    // one-case enum the import selects, so the consuming compile mints the
    // enum and its case — and its tag row claims the id the first library
    // already seeded. The seed keys each row to a selected mint, so an
    // unselected name would silently drop: the refusal rides a name the
    // import actually binds.
    let mut conflict = MemberSurface::default();
    if program.linked_conflict {
        let claimed = lib_back
            .tags()
            .iter()
            .find(|row| row.name == "quiet")
            .expect("the library's table holds the case's row")
            .id;
        let at = Span::new(member, 1, 1);
        let entry = |name: &str, fact: TypeFact| ForeignEntry {
            name: name.to_owned(),
            vis: ds_check::Vis::Pub,
            kind: ForeignKind::Type,
            generic: false,
            defaulted: false,
            decl_span: at,
            sig: None,
            row: EffectRow::default(),
            caps: 0,
            value: None,
            ty: Some(Arc::new(fact)),
            link: 1,
            func: None,
            std: false,
        };
        let unit = || ds_check::import::CasePayload::Unit;
        conflict.entries.push(entry(
            "spare",
            TypeFact::Enum(vec![ds_check::import::TypeCase {
                name: "clash".to_owned(),
                payload: unit(),
            }]),
        ));
        conflict.entries.push(entry(
            "clash",
            TypeFact::Case(ds_check::import::CaseFact {
                enum_name: "spare".to_owned(),
                payload: unit(),
            }),
        ));
        conflict.tags.push(("clash".to_owned(), claimed));
    }
    let ast = Arc::new(parse(&files, id).expect("the generated program parses"));
    let mut imports = ImportGraph::new();
    for import in ast
        .blocks
        .iter()
        .flat_map(|block| block.items.iter())
        .filter_map(|item| match &item.kind {
            ds_parse::ItemKind::Import(import) => Some(import),
            _ => None,
        })
    {
        let head = &import.path.segments[0];
        let linked = if import.path.at_sign && head.name == "lib" {
            &surface
        } else if program.linked_conflict {
            &conflict
        } else {
            continue;
        };
        imports.add(import.span, ImportSurface::Linked(linked.clone()));
    }
    let analyzed = analyze_owned_linked(&files, vec![ast], &[false], &imports);
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
    let lowering = lower(
        MODULE_NAME,
        Hash128::compute(program.source.as_bytes()),
        &analyzed.asts,
        &analyzed,
    );
    let lowered = match lowering {
        Ok(lowered) => lowered,
        Err(LowerError::LinkedTagRow { .. }) if program.linked_conflict => {
            // the honest refusal: the seeded id is claimed twice
            return program.drawn;
        },
        Err(other) => panic!("the generated program lowers: {other}"),
    };
    assert!(
        !program.linked_conflict,
        "the conflicting tag row must refuse at lowering"
    );
    // the dep list rides the header before the write: the writer
    // validates every linked call's dep position against it, so the
    // consuming module must name @lib before it round-trips
    let lowered = lowered.with_deps(vec![ds_mod::Dep {
        name: LIB_MODULE.to_owned(),
        content_hash: lib_hash,
    }]);
    let bytes = write(lowered.module());
    let back = read(&bytes).expect("the lowered module round-trips");
    assert_eq!(
        write(&back),
        bytes,
        "the reader round trip stays byte-stable"
    );
    let consumer = back;
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, lib_back).expect("the library installs");
    loader::install(&mut registry, consumer).expect("the generated program installs");
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
