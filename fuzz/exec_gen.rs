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
//! combinators: the fork-join engine seams `fork_map`/`fork_reduce`
//! (the spellings the @std `par_map`/`par_reduce` library fns call):
//! `fork_map` (a capturing closure callable and a named
//! multi-word-answer callable, an empty-list leg) and `fork_reduce`
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
//! The `pub import` chain episode rides the chain as a three-module
//! compile: the generated origin member (a callable pair, a literal
//! const, and a flat struct, the callable's addend and the const drawn)
//! compiles, round-trips, and installs under its dep name, the middle
//! member re-exports every name through the origin's real linked
//! surface — its own body calling the re-exported callable and reading
//! the const — and the consumer binds all of them through the middle
//! package's surface, the pub row's bindings carrying the ORIGIN's dep
//! position and function index so the calls address the origin's module
//! through the middle's closure. Three drawn refusal variants keep the
//! honest refusals in the set: a consumer selection of a name the
//! middle imported privately (the private row records no re-export, so
//! the link walk refuses the selection), a selectionless pub row
//! refusing the middle's own compile, and a `pub import` of `@std`
//! refusing the merged-ambient seam — each refusing at its own compile
//! and never running.
//!
//! The @std declaration-graph episode compiles a second consumer over a
//! core-shaped ambient: the shrunk core (the episode's own tags, the
//! collection words, and the extern face) registers as the ambient member
//! ahead of the consumer's source, and the reserved @std package serves
//! its externs and the migrated members through a linked surface carrying
//! the std identity (the package's first member is the ambient core the
//! declaring compile registers, the migrated members riding beside it as
//! the episode's own model of the members file), the drawn selections
//! staying in the bindable vocabulary — the extern face: a two-word
//! concat under a drawn case transform and the trimmed word, every call
//! answered by the natives the consumer's own lowering wires by name —
//! beside the migrated members' shapes: the `row` alias annotating a
//! keyed map value, the `config` record constructing and reading
//! through the presence-tag word (the port's presence drawn), and the
//! `urls` list const embedding its elements over the list seam, its
//! trip count the const's own two elements. A drawn refusal variant
//! keeps the honest boundary in the set: `response`'s name binds in the
//! import row while its value use keeps the local spelling's own M3
//! contract refusal at lowering, the episode's compile refusing exactly
//! once and never running. The parser-budget episode draws the chain
//! and depth shapes at both sides of the parser's budgets: long type
//! chains (a `?` postfix run, an `|`/`&` chain) written into a helper's
//! parameter, return, and binding annotations beside near-budget
//! typed lambda arrow chains and paren nesting — all under the
//! budgets, checking clean and erasing at lowering, so the printed
//! answer stays constant — and two drawn refusal variants keep the
//! honest boundaries in the set: a past-budget chain refusing the
//! consumer's parse with the chain fold's NestingTooDeep diagnostic
//! and a past-budget arrow run refusing the pre-scan's live-arrow
//! diagnostic, each input never running.
//!
//! Termination (why every generated run stops by itself): main is
//! straight-line (no recursion; each episode lives in its own helper
//! function, so every frame stays under the width cap; the loop
//! families are the generic serving impl's for-in over a fixed-length
//! literal list of at most three elements, from_json's synthesized array
//! walk whose trip count mirrors the encoded field's own list length —
//! at most two — and the std consumer's for-in over the embedded urls
//! const whose two elements are fixed, so every iteration count is
//! drawn and bounded), the
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
    ConstFact, EffectRow, FnSig, ForeignEntry, ForeignKind, ImportGraph, ImportSurface,
    MemberSurface, TypeFact,
};
use ds_diag::{ErrorCode, FileId, Files, Span, human};
use ds_lower::{ComptimeValues, LowerError, lower, lower_library};
use ds_meta::{analyze, analyze_owned_linked};
use ds_mod::{Hash128, Module, read, write};
use ds_parse::{Type, parse};
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// The generated module's name (the run entry target).
pub const MODULE_NAME: &str = "fze";

/// The @std declaration-graph episode's run entry module name.
const STD_MODULE: &str = "fzs";

/// The linked library member's module name: the dep name the consumer's
/// import links and the name the library module installs under.
const LIB_MODULE: &str = "@lib";

/// The `pub import` chain's origin member module name: the dep the
/// middle's pub row links and the package whose module serves every
/// re-exported body.
const ORIGIN_MODULE: &str = "@olib";

/// The `pub import` chain's middle member module name: the package the
/// consumer's import row links and the surface the re-exports cross
/// through.
const MID_MODULE: &str = "@mlib";

/// The consumer's recorded dep positions, first-occurrence over its own
/// import rows: the linked-enum row's package first, the chain's middle
/// after it, and the chain's origin last — the position a re-exported
/// entry's FuncRef addresses, met through the middle's surface after
/// the source's own names.
const MID_POS: u32 = 1;
const ORIGIN_POS: u32 = 2;

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
    /// `pub import` chain episodes: a three-module compile where the
    /// consumer binds a re-exported callable, const, and struct through
    /// the middle package's surface, the calls addressing the origin's
    /// module, beside the middle's own callable.
    pub pub_imports: u32,
    /// The chain's privately-imported-name refusal drawn: a consumer
    /// selection of a name the middle imported privately refusing at
    /// the link walk.
    pub pub_private_refusals: u32,
    /// The chain's selectionless-pub-row refusal drawn: the middle's
    /// own compile refusing a row that binds no name to re-export.
    pub pub_selectionless_refusals: u32,
    /// The chain's merged-ambient refusal drawn: a `pub import` of the
    /// reserved `@std` re-exporting nothing under the merged-ambient
    /// reading.
    pub pub_std_refusals: u32,
    /// @std declaration-graph episodes: a second consumer compiling over
    /// a core-shaped ambient with the reserved package's linked extern
    /// face answering.
    pub std_graphs: u32,
    /// The `row` alias's bindable shape drawn: a keyed map value
    /// annotated through the linked alias fact and read by length.
    pub std_rows: u32,
    /// The `config` record's bindable shape drawn: an optional-field
    /// struct constructed through the linked struct fact, the port's
    /// presence drawn, and read through the presence-tag word.
    pub std_configs: u32,
    /// The `urls` const's bindable shape drawn: the plain-literal list
    /// fact embedding its elements over the list seam, iterated and
    /// read by length.
    pub std_url_consts: u32,
    /// The `response` member's honest boundary drawn: the opaque
    /// bodiless struct's name binding while its value use keeps the M3
    /// contract refusal at lowering, the episode refusing exactly once
    /// and never running.
    pub std_opaque_refusals: u32,
    /// Type chains at or under the parser's 128-term chain budget,
    /// written into a helper's parameter, return, and binding
    /// annotations: a `?` postfix run and an `|`/`&` chain, which solve
    /// to their atom and erase at lowering.
    pub type_chain_oks: u32,
    /// The episode's past-budget chain refusal drawn: the consumer's
    /// parse answers the chain fold's NestingTooDeep diagnostic and
    /// the input never runs.
    pub type_chain_over: u32,
    /// Near-budget depth shapes drawn just under the scanner's arrow
    /// and bracket budgets: typed lambda arrow chains and paren
    /// nesting, which parse to completion.
    pub depth_oks: u32,
    /// The episode's past-budget arrow refusal drawn: the pre-scan
    /// answers the live-arrow diagnostic and the input never runs.
    pub depth_over: u32,
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
    /// The `pub import` chain's origin member source: the callable pair,
    /// the literal const, and the flat struct the middle's pub row
    /// re-exports, compiled and installed as its own module.
    pub origin: String,
    /// The `pub import` chain's middle member source: the pub row over
    /// the origin's surface and the middle's own callable, compiled and
    /// installed between the origin and the consumer.
    pub mid: String,
    /// Which `pub import` refusal the chain drew, `Clean` for none.
    pub pub_variant: PubVariant,
    /// The @std episode's shrunk core source: the ambient member the
    /// declaring compile registers ahead of its own consumer.
    pub std_core: String,
    /// The @std episode's model of the migrated members' source: the
    /// hermetic member file the episode's own package compile analyzes
    /// beside the core, its declarations extracted into the linked
    /// surface's link facts.
    pub std_members: String,
    /// Whether the @std episode drew the `response` value-use refusal:
    /// the opaque member's name binds while the value use keeps the M3
    /// contract refusal at lowering, the module refusing and never
    /// running.
    pub std_opaque: bool,
    /// The @std episode's consumer source: the @std import row over the
    /// core's extern face, compiled as its own module.
    pub std_source: String,
    /// The @std episode's oracle stdout.
    pub std_expected: String,
    /// Which past-budget shape the type-chain episode drew: the
    /// consumer refuses at parse with the named budget diagnostic and
    /// never runs.
    pub chain_over: ChainOver,
}

/// Which honest refusal a drawn `pub import` chain carries, each
/// refusing at its own compile seam so the input never runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PubVariant {
    /// The clean chain: every consumer selection binds.
    Clean,
    /// The consumer selects a name the middle imported privately: the
    /// private row records no re-export, so the link walk refuses the
    /// selection.
    Private,
    /// The middle's pub row carries no selection list: its own compile
    /// refuses the re-export of nothing.
    Selectionless,
    /// The middle re-exports the reserved `@std`: the merged-ambient
    /// reading binds the compile's own names, never import-bound defs.
    Std,
}

/// Which past-budget shape the type-chain episode drew, each refusing
/// at the consumer's own parse seam so the input never runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChainOver {
    /// No refusal: the episode's chains sit under the budgets, check
    /// clean, and run.
    Clean,
    /// A past-budget type chain: the parse answers the chain fold's
    /// NestingTooDeep diagnostic.
    TypeChain,
    /// A past-budget arrow run: the pre-scan answers the live-arrow
    /// diagnostic.
    ArrowChain,
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

/// The @std episode's shrunk core: the ambient member a declaring
/// compile registers ahead of its own source — the word-shaped atoms the
/// linked signatures spell, the collection words the migrated members'
/// bodies spell in (`row`'s map body, `urls`'s list body), and the extern
/// face the reserved package's surface serves (the episode's model of the
/// landed split: the compiler-required face stays ambient while the
/// members migrate into the package's own ds source, the extern face
/// remaining the first member's bindable vocabulary). Fixed, not drawn;
/// the drawn programs stay in the bindable vocabulary.
const STD_CORE: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "\n",
    "pub struct map[k, v]\n",
    "pub struct list[t]\n",
    "\n",
    "pub extern fn println (s: string) -> ()\n",
    "pub extern fn concat (a: string, b: string) -> string\n",
    "pub extern fn to_upper (s: string) -> string\n",
    "pub extern fn to_lower (s: string) -> string\n",
    "pub extern fn trim (s: string) -> string\n",
    "pub extern fn map_new[k, v] () -> map[k, v]\n",
    "pub extern fn map_set[k, v] (m: mut map[k, v], key: k, value: v) -> ()\n",
    "pub extern fn map_len[k, v] (m: map[k, v]) -> int\n",
    "pub extern fn list_len[t] (xs: list[t]) -> int\n",
);

/// The @std episode's model of the migrated members' source: the demo
/// residue's spellings exactly as the reserved package's own members file
/// carries them — the map alias, the optional-field record, the opaque
/// bodiless struct, and the plain-literal list const — analyzed beside the
/// core as the package's second member so the episode's linked surface
/// extracts their link facts the same way the real exports carry them.
/// Fixed, not drawn; the drawn programs stay in the bindable vocabulary.
const STD_MEMBERS: &str = concat!(
    "pub type row = map[string, string]\n",
    "\n",
    "pub struct config { host: string, port: int?, retries: int }\n",
    "\n",
    "pub struct response\n",
    "\n",
    "pub const urls = [\"https://ds.dev\", \"https://example.org\"]\n",
);

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
    /// The `pub import` chain's origin member source, built by the
    /// episode.
    origin: String,
    /// The `pub import` chain's middle member source, built by the
    /// episode.
    mid: String,
    /// Which `pub import` refusal the chain drew for this input.
    pub_variant: PubVariant,
    /// The @std episode's shrunk core source (the ambient member the
    /// declaring compile registers).
    std_core: String,
    /// The @std episode's model of the migrated members' source.
    std_members: String,
    /// Whether the @std episode drew the `response` value-use refusal.
    std_opaque: bool,
    /// The @std episode's consumer source, built by the episode.
    std_source: String,
    /// The @std episode's oracle stdout.
    std_out: String,
    /// Which past-budget shape the type-chain episode drew.
    chain_over: ChainOver,
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
            "const {shifted} = {xs}.fork_map((x: int) => x * {k} + {bias_name})"
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
        self.line(&format!("const {wide} = {xs}.fork_map(both)"));
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
            self.line("println(\"empty ${len(list[int]().fork_map(both))}\")");
            self.drawn.par_map += 1;
            self.answer("empty 0");
        }
        // par_reduce with a scalar closure fold, in input order
        let init = self.lit(100);
        let sum = self.name("sum");
        self.line(&format!(
            "const {sum} = {xs}.fork_reduce({init}, (a: int, b: int) => a + b + {bias_name})"
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
            "const {joined} = {words_name}.fork_reduce(\"\", (a: string, b: string) => a + b)"
        ));
        self.line(&format!("println(\"joined ${{{joined}}}\")"));
        self.drawn.par_reduce += 1;
        self.drawn.closures += 1;
        self.answer(&format!("joined {}{}{}", words[0], words[1], words[2]));
        if self.s.roll(2) {
            let passthru = self.lit(50);
            self.line(&format!(
                "println(\"passthru ${{list[int]().fork_reduce({passthru}, (a: int, b: int) => a \
                 + b)}}\")"
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
            "const {folded} = {pairs_name}.fork_reduce(pair {{ left: 0, right: 0 }}, (a: pair, b: \
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

    /// The parser-budget episode: type chains and unbracketed depth
    /// drawn at both sides of the parser's budgets. The admissible arm
    /// writes a long `|` union chain into a helper's parameter
    /// annotation, a long `&` intersect chain into its return, a `?`
    /// postfix run into a binding annotation, and a near-budget typed
    /// lambda chain and paren nesting into the episode body — the
    /// chains solve to their atom and erase at lowering, so checking
    /// stays clean and the printed answer stays constant. The drawn
    /// refusal arms (rare, so the sweep keeps its well-typed bias)
    /// write a past-budget chain or arrow run instead: the consumer
    /// refuses at parse with the budget diagnostic and never runs.
    fn ep_type_chain(&mut self) {
        // the chain fold's term cap and the scanner's live-arrow
        // budget, spelled as the parser's current limits
        const CHAIN_TERMS: usize = 128;
        const ARROWS: usize = 40;
        self.chain_over = match self.s.below(40) {
            0 => ChainOver::TypeChain,
            1 => ChainOver::ArrowChain,
            _ => ChainOver::Clean,
        };
        match self.chain_over {
            ChainOver::TypeChain => {
                let name = self.name("co");
                let p = self.name("p");
                let chain = op_chain("int", "|", CHAIN_TERMS + 1 + self.s.below(80));
                self.decl(&format!("fn {name}({p}: {chain}) -> int {{ 0 }}"));
                self.drawn.type_chain_over += 1;
            },
            ChainOver::ArrowChain => {
                let name = self.name("co");
                let arrows = ARROWS + 1 + self.s.below(20);
                let body = format!("const k = {}1", "x => ".repeat(arrows));
                self.decl(&format!("fn {name}() -> int {{\n  {body}\n  0\n}}"));
                self.drawn.depth_over += 1;
            },
            ChainOver::Clean => {
                let name = self.name("cc");
                let p = self.name("p");
                let param = op_chain("int", "|", 60 + self.s.below(CHAIN_TERMS - 60));
                let ret = op_chain("int", "&", 60 + self.s.below(CHAIN_TERMS - 60));
                self.decl(&format!("fn {name}({p}: {param}) -> {ret} {{\n  {p}\n}}"));
                let call = self.name("c");
                let _ = writeln!(self.calls, "  println(\"{call}: ${{{name}(1)}}\")");
                self.answer(&format!("{call}: 1"));
                let w = self.name("w");
                let marks = 60 + self.s.below(CHAIN_TERMS - 61);
                self.line(&format!("const {w}: int{} = 1", "?".repeat(marks)));
                let k = self.name("k");
                let depth = 20 + self.s.below(ARROWS - 20);
                self.line(&format!("const {k} = {}", typed_arrows(depth)));
                let t = self.name("t");
                let nest = 20 + self.s.below(ARROWS - 20);
                self.line(&format!(
                    "const {t} = {}1{}",
                    "(".repeat(nest),
                    ")".repeat(nest)
                ));
                self.drawn.type_chain_oks += 1;
                self.drawn.depth_oks += 1;
            },
        }
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
        self.decl("tag parse_err { pos: int }");
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
             json_object { fields } => fields,\n    _ => parse_err(pos: 0),\n  }\n}",
        );
        self.decl(
            "fn json_as_array (v: json) -> list[json] | parse_err {\n  match v {\n    json_array \
             { items } => items,\n    _ => parse_err(pos: 0),\n  }\n}",
        );
        self.decl(
            "fn json_as_int (v: json) -> int | parse_err {\n  match v {\n    json_int { value } \
             => value,\n    _ => parse_err(pos: 0),\n  }\n}",
        );
        self.decl(
            "trait encoder {\n  fn enc_int (self, value: int) -> Self\n  fn enc_array (self, \
             items: list[Self]) -> Self\n  fn enc_object (self, fields: map[string, Self]) -> \
             Self\n}",
        );
        self.decl(
            "trait decoder {\n  fn dec_object (self) -> map[string, Self] | parse_err\n  fn \
             dec_array (self) -> list[Self] | parse_err\n  fn dec_member (self, key: string) -> \
             Self | none\n  fn dec_int (self) -> int | parse_err\n}",
        );
        self.decl("impl json {\n  fn default() -> json { json_null }\n}");
        self.decl(
            "impl decoder for json {\n  fn dec_object (self) -> map[string, json] | parse_err { \
             json_as_object(self) }\n  fn dec_array (self) -> list[json] | parse_err { \
             json_as_array(self) }\n  fn dec_member (self, key: string) -> json | none { \
             json_member(self, key) }\n  fn dec_int (self) -> int | parse_err { json_as_int(self) \
             }\n}",
        );
        self.decl(
            "impl encoder for json {\n  fn enc_int (self, value: int) -> json { json_int(value: \
             value) }\n  fn enc_array (self, items: list[json]) -> json { json_array(items: \
             items) }\n  fn enc_object (self, fields: map[string, json]) -> json { \
             json_object(fields: fields) }\n}",
        );
        self.decl("extern fn unwrap_or[t] (x: t?, y: t) -> t");
        self.decl("extern fn list_len[t] (xs: list[t]) -> int");
        self.decl("extern fn list_push[t] (xs: mut list[t], x: t) -> ()");
        self.decl("extern fn map_get[k, v] (m: map[k, v], key: k) -> v | none");
        self.decl(
            "impl[t] list[t] {\n  fn get (self, i: int) -> t | none\n  fn push (mut self, x: t) \
             -> list[t]\n}",
        );
        self.decl(
            "impl[k, v] map[k, v] {\n  fn get (self, key: k) -> v | none\n  fn set (mut self, \
             key: k, value: v) -> map[k, v]\n}",
        );
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

    /// The `pub import` chain episode: the origin member's source (the
    /// callable pair, the literal const, and the flat struct, the
    /// callable's addend and the const's literal drawn) into `origin`,
    /// the middle member's source (the pub row re-exporting every name
    /// through the origin's surface, its own body calling the
    /// re-exported callable and reading the const) into `mid`, the
    /// consumer's import row binding through the middle package, and the
    /// consumer episode's body — the re-exported callable, const, and
    /// struct crossing through the middle beside its own callable, every
    /// call addressing the origin's module. The refusal variants
    /// (drawn) keep the honest refusals in the set: a consumer selection
    /// of the privately-imported name refusing the link walk, a
    /// selectionless pub row refusing the middle's own compile, and a
    /// `pub import` of `@std` refusing the merged-ambient seam — each
    /// refusing at its own compile and never running.
    fn ep_pub_import(&mut self) {
        let (k, cap) = (1 + self.lit(9), self.lit(9));
        self.pub_variant = if self.s.roll(5) {
            match self.s.below(3) {
                0 => PubVariant::Private,
                1 => PubVariant::Selectionless,
                _ => PubVariant::Std,
            }
        } else {
            PubVariant::Clean
        };
        // the origin member: the struct, the callable pair, and the const
        self.origin = format!(
            concat!(
                "tag int\n\n",
                "pub struct box3 {{ a: int, b: int }}\n\n",
                "pub fn bump (n: int) -> int {{ n + {k} }}\n\n",
                "pub fn sum (b: box3) -> int {{ b.a + b.b }}\n\n",
                "pub const CAP = {cap}\n",
            ),
            k = k,
            cap = cap,
        );
        // the middle member: the pub row (the selectionless variant's
        // binds nothing, so its own body answers a plain word), the
        // drawn refusal's extra row, and its own callable
        self.mid = format!(
            "tag int\n\n{rows}{extra}\n{own}\n",
            rows = match self.pub_variant {
                PubVariant::Selectionless => "pub import @olib\n",
                _ => "pub import @olib (bump as raise, sum, CAP, box3)\n",
            },
            extra = match self.pub_variant {
                PubVariant::Private => "import @olib (bump as quiet)\n",
                PubVariant::Std => "pub import @std (row)\n",
                _ => "",
            },
            own = match self.pub_variant {
                PubVariant::Selectionless => "pub fn mid_own (n: int) -> int { n + 1 }",
                _ => "pub fn mid_own (n: int) -> int { raise(n) + CAP }",
            },
        );
        // the consumer's import row through the middle package (the
        // private variant selects the privately-imported name beside the
        // exported set, so the link walk refuses the selection)
        let mut row = String::from("import @mlib (raise, sum, CAP, box3, mid_own");
        if self.pub_variant == PubVariant::Private {
            row.push_str(", quiet");
        }
        row.push_str(")\n");
        self.imports.push_str(&row);
        self.drawn.pub_imports += 1;
        match self.pub_variant {
            PubVariant::Private => self.drawn.pub_private_refusals += 1,
            PubVariant::Selectionless => self.drawn.pub_selectionless_refusals += 1,
            PubVariant::Std => self.drawn.pub_std_refusals += 1,
            PubVariant::Clean => {},
        }
        // the consumer episode: every binding crossing through the middle
        let (k1, k2) = (self.lit(9), self.lit(9));
        let (x, y) = (self.lit(9), self.lit(9));
        self.line(&format!("println(\"bump ${{raise({k1})}}\")"));
        self.answer(&format!("bump {}", k1 + k));
        self.line("println(\"cap ${CAP}\")");
        self.answer(&format!("cap {cap}"));
        let bx = self.name("bx");
        self.line(&format!("const {bx}: box3 = box3 {{ a: {x}, b: {y} }}"));
        self.line(&format!(
            "println(\"box ${{{bx}.a}} ${{{bx}.b}} ${{sum({bx})}}\")"
        ));
        self.answer(&format!("box {x} {y} {}", x + y));
        self.line(&format!("println(\"mid ${{mid_own({k2})}}\")"));
        self.answer(&format!("mid {}", k2 + k + cap));
    }

    /// The @std declaration-graph episode: a second consumer compiles
    /// over a core-shaped ambient — the shrunk core registering as the
    /// ambient member ahead of its source, the reserved @std package
    /// serving its externs and the migrated members through the linked
    /// surface carrying the std identity — the drawn selections staying
    /// in the bindable vocabulary (the extern face: a two-word concat
    /// under a drawn case transform, and the trimmed word; the migrated
    /// members: the `row` alias annotating a keyed map value, the
    /// `config` record constructing and reading through the presence-tag
    /// word with the port's presence drawn, and the `urls` const
    /// embedding its elements over the list seam), every extern call
    /// answered by the natives the consumer's own lowering wires by name.
    /// A drawn refusal variant keeps the honest boundary in the set:
    /// `response`'s name binds in the import row while its value use
    /// keeps the local spelling's own M3 contract refusal, the episode's
    /// compile refusing exactly once and never running (the oracle
    /// staying empty).
    fn ep_std_graph(&mut self) {
        self.std_core = String::from(STD_CORE);
        self.std_members = String::from(STD_MEMBERS);
        let up = self.s.roll(2);
        let (w0, w1, w2) = (
            SHORTS[self.s.below(SHORTS.len())],
            SHORTS[self.s.below(SHORTS.len())],
            SHORTS[self.s.below(SHORTS.len())],
        );
        let case_fn = if up { "to_upper" } else { "to_lower" };
        let (a, b) = if up {
            (w0.to_owned(), w1.to_owned())
        } else {
            (w0.to_uppercase(), w1.to_uppercase())
        };
        self.std_opaque = self.s.roll(4);
        let host = WORDS[self.s.below(WORDS.len())];
        let retries = 1 + self.lit(9);
        let port = 1 + self.lit(99);
        let port_present = self.s.roll(2);
        let (rk, rv) = (
            WORDS[self.s.below(WORDS.len())],
            WORDS[self.s.below(WORDS.len())],
        );
        let mut body = String::new();
        let _ = writeln!(body, "  println({case_fn}(concat(\"{a}\", \"{b}\")))");
        let _ = writeln!(body, "  println(trim(\" {w2} \"))");
        // the migrated members' bindable shapes: the alias annotates the
        // keyed map value, the record constructs and reads through the
        // presence-tag word, the list const embeds over the list seam
        let _ = writeln!(body, "  var rows: row = map[string, string]()");
        let _ = writeln!(body, "  map_set(rows, \"{rk}\", \"{rv}\")");
        let _ = writeln!(
            body,
            "  const cfg = config {{ host: \"{host}\", port: {}, retries: {retries} }}",
            if port_present { port.to_string() } else { "none".to_owned() }
        );
        let _ = writeln!(body, "  const port = cfg.port");
        let _ = writeln!(body, "  if port is none {{");
        let _ = writeln!(body, "    println(\"no port\")");
        let _ = writeln!(body, "  }} else {{");
        let _ = writeln!(body, "    println(\"port ${{port}}\")");
        let _ = writeln!(body, "  }}");
        let _ = writeln!(body, "  for u in urls {{");
        let _ = writeln!(body, "    println(u)");
        let _ = writeln!(body, "  }}");
        let _ = writeln!(
            body,
            "  println(\"${{map_len(rows)}} ${{list_len(urls)}} ${{cfg.host}} ${{cfg.retries}}\")"
        );
        // the closer: a constant-string argument keeps the final tail
        // transfer count-free, so the heap-drain assert holds for this
        // root frame too
        let _ = writeln!(body, "  println(\"done\")");
        let mut import_row = format!("import @std (concat, {case_fn}, trim, row, config, urls");
        if self.std_opaque {
            import_row.push_str(", response");
        }
        import_row.push_str(")\n");
        let mut source = import_row;
        if self.std_opaque {
            // the opaque member's honest boundary: the name binds in the
            // import row while the value use keeps the M3 contract
            // refusal, the local spelling's own pass-through spelling
            source.push_str("\nfn pass(r: response) -> response {\n  r\n}\n");
        }
        let _ = write!(source, "\nfn main() -> () {{\n{body}}}\n");
        self.std_source = source;
        self.drawn.std_graphs += 1;
        self.drawn.std_rows += 1;
        self.drawn.std_configs += 1;
        self.drawn.std_url_consts += 1;
        if self.std_opaque {
            // the module refuses at lowering and never runs, so its
            // oracle stays empty — only the clean draws answer
            self.drawn.std_opaque_refusals += 1;
            return;
        }
        let joined = format!("{w0}{w1}");
        let cased = if up { joined.to_uppercase() } else { joined.to_lowercase() };
        self.std_answer(&cased);
        self.std_answer(w2);
        if port_present {
            self.std_answer(&format!("port {port}"));
        } else {
            self.std_answer("no port");
        }
        self.std_answer("https://ds.dev");
        self.std_answer("https://example.org");
        self.std_answer(&format!("1 2 {host} {retries}"));
        self.std_answer("done");
    }

    /// Records one @std episode oracle stdout line.
    fn std_answer(&mut self, text: &str) {
        self.std_out.push_str(text);
        self.std_out.push('\n');
    }
}

/// Generates the program for one input: the source, its oracle stdout,
/// and the draw census. Deterministic per input.
/// `members` copies of `atom` folded with `op` (`|` or `&`): one
/// unbracketed chain term per copy past the first.
fn op_chain(atom: &str, op: &str, members: usize) -> String {
    let mut out = String::from(atom);
    for _ in 1..members {
        let _ = write!(out, " {op} {atom}");
    }
    out
}

/// A typed lambda arrow chain `depth` deep: one arrow and one bracket
/// per level, at both scanner budgets' edge when drawn just under
/// them.
fn typed_arrows(depth: usize) -> String {
    let mut out = String::new();
    for i in 0..depth {
        let _ = write!(out, "(x{i}: int) => ");
    }
    out.push('1');
    out
}

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
        origin: String::new(),
        mid: String::new(),
        pub_variant: PubVariant::Clean,
        std_core: String::new(),
        std_members: String::new(),
        std_opaque: false,
        std_source: String::new(),
        std_out: String::new(),
        chain_over: ChainOver::Clean,
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
         int\nextern fn list_get[t] (xs: list[t], i: int) -> t | none\nextern fn fork_map[t, r] \
         (xs: list[t], f: fn(t) -> r) -> list[r]\nextern fn fork_reduce[t] (xs: list[t], init: t, \
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
    g.ep_type_chain();
    g.seal_episode();
    g.ep_free_generic();
    g.seal_episode();
    g.ep_json_derive();
    g.seal_episode();
    g.ep_linked_enum();
    g.seal_episode();
    g.ep_pub_import();
    g.seal_episode();
    g.ep_std_graph();
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
        origin: std::mem::take(&mut g.origin),
        mid: std::mem::take(&mut g.mid),
        pub_variant: g.pub_variant,
        chain_over: g.chain_over,
        std_core: std::mem::take(&mut g.std_core),
        std_members: std::mem::take(&mut g.std_members),
        std_opaque: g.std_opaque,
        std_source: std::mem::take(&mut g.std_source),
        std_expected: std::mem::take(&mut g.std_out),
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
        bind: None,
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
            origin: None,
            func,
            std: false,
        }
    };
    let tone = lib
        .package
        .lookup_type("tone")
        .expect("the library exports the enum");
    let facts = TypeFact::member_facts(lib, 0, &HashMap::default());
    let enum_fact = Arc::clone(facts.get(&tone).expect("the enum's fact carries"));
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
        let fact = Arc::clone(facts.get(&def).expect("the case's fact carries"));
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
    let tags = module.tags().to_vec();
    MemberSurface {
        entries,
        tags,
        impls: Vec::new(),
        inherent: Vec::new(),
    }
}

/// The `pub import` chain's origin surface: each exported definition's
/// linked facts — the two callables' written signatures and function
/// indexes, the literal const's value fact, the flat struct's word
/// layout — every span rebased into the consuming compile's registry
/// through `map` (the middle member's registry, the pub row's home),
/// the linked dep position naming the origin package itself.
fn origin_surface(
    lib: &ds_check::Analyzed,
    lowered: &ds_lower::Lowered,
    module: &Module,
    map: &HashMap<FileId, FileId>,
) -> MemberSurface {
    let facts = TypeFact::member_facts(lib, 0, &HashMap::default());
    let entry = |name: &str, def: ds_check::DefId, kind: ForeignKind| ForeignEntry {
        name: name.to_owned(),
        vis: lib.package.def(def).vis,
        kind,
        generic: false,
        defaulted: false,
        decl_span: rebase_span(lib.package.def(def).span, map),
        sig: lib.decls.fns.get(&def).map(|sig| rebase_sig(sig, map)),
        row: EffectRow::default(),
        caps: 0,
        value: lib
            .decls
            .consts
            .get(&def)
            .filter(|local| !local.mutable)
            .and_then(|local| ConstFact::of_expr(&local.init)),
        ty: facts.get(&def).cloned(),
        link: 0,
        origin: None,
        func: lowered
            .functions()
            .iter()
            .position(|f| f == name)
            .map(|at| u32::try_from(at).expect("function tables are tiny")),
        std: false,
    };
    let entries = vec![
        entry(
            "bump",
            lib.package
                .lookup_value("bump")
                .expect("the origin exports the callable"),
            ForeignKind::Fn,
        ),
        entry(
            "sum",
            lib.package
                .lookup_value("sum")
                .expect("the origin exports the struct scorer"),
            ForeignKind::Fn,
        ),
        entry(
            "CAP",
            lib.package
                .lookup_value("CAP")
                .expect("the origin exports the const"),
            ForeignKind::Binding,
        ),
        entry(
            "box3",
            lib.package
                .lookup_type("box3")
                .expect("the origin exports the struct"),
            ForeignKind::Type,
        ),
    ];
    let tags = module.tags().to_vec();
    MemberSurface {
        entries,
        tags,
        impls: Vec::new(),
        inherent: Vec::new(),
    }
}

/// The `pub import` chain's middle surface: the pub row's re-exported
/// bindings under the names the row bound — each carrying the ORIGIN
/// package's dep position and the origin's function index, so a
/// dependent's call addresses the origin's module, never the middle's
/// table — beside the middle's own exported callable, every span
/// rebased into the consuming compile's registry through `map` (the
/// origin's sig spans and the middle's own declarations riding the same
/// map, the two registries' registrations of the same texts).
fn middle_surface(
    mid: &ds_check::Analyzed,
    lowered: &ds_lower::Lowered,
    module: &Module,
    map: &HashMap<FileId, FileId>,
) -> MemberSurface {
    let mut entries = Vec::new();
    for (def, name, span) in mid.package.reexports() {
        let entry = mid
            .package
            .foreign()
            .get(def)
            .expect("a re-export binds a foreign def");
        entries.push(ForeignEntry {
            name: name.clone(),
            vis: entry.vis,
            kind: entry.kind,
            generic: entry.generic,
            defaulted: entry.defaulted,
            decl_span: rebase_span(*span, map),
            sig: entry.sig.as_ref().map(|sig| rebase_sig(sig, map)),
            row: entry.row,
            caps: entry.caps,
            value: entry.value.clone(),
            ty: entry.ty.clone(),
            link: ORIGIN_POS,
            origin: Some(ORIGIN_MODULE.to_owned()),
            func: entry.func,
            std: false,
        });
    }
    let own = mid
        .package
        .lookup_value("mid_own")
        .expect("the middle exports its own callable");
    let def_row = mid.package.def(own);
    entries.push(ForeignEntry {
        name: "mid_own".to_owned(),
        vis: def_row.vis,
        kind: ForeignKind::Fn,
        generic: false,
        defaulted: false,
        decl_span: rebase_span(def_row.span, map),
        sig: mid.decls.fns.get(&own).map(|sig| rebase_sig(sig, map)),
        row: EffectRow::default(),
        caps: 0,
        value: None,
        ty: None,
        link: MID_POS,
        origin: None,
        func: lowered
            .functions()
            .iter()
            .position(|f| f == "mid_own")
            .map(|at| u32::try_from(at).expect("function tables are tiny")),
        std: false,
    });
    let tags = module.tags().to_vec();
    MemberSurface {
        entries,
        tags,
        impls: Vec::new(),
        inherent: Vec::new(),
    }
}

/// The reserved @std package's surface over the episode's own two-member
/// package compile: the core member's exported externs — bodiless linked
/// declarations whose calls the consumer's own lowering answers by
/// native name — beside the migrated members' link facts extracted the
/// same way the real exports carry them (the map alias's resolved word,
/// the optional-field record's word layout, the opaque bodiless
/// struct's reserved identity, the list const's plain literals), every
/// entry carrying the std identity (the package's first member is the
/// ambient core the declaring compile registers, so the word-shaped
/// atoms the signatures and facts spell resolve through the consumer's
/// own ambient atoms). No tag rows ride: the extern face and the
/// migrated members cross no tag-bearing value.
fn std_surface(pkg: &ds_check::Analyzed, map: &HashMap<FileId, FileId>) -> MemberSurface {
    // the first tree is the shrunk core itself, the ambient member the
    // declaring compile registers ahead of the members' file
    let facts = TypeFact::member_facts(pkg, 1, &HashMap::default());
    let mut entries = Vec::new();
    let mut entry = |name: &str,
                     def: ds_check::DefId,
                     kind: ForeignKind,
                     sig: Option<Arc<FnSig>>,
                     value: Option<ConstFact>,
                     ty: Option<Arc<TypeFact>>| {
        let def_row = pkg.package.def(def);
        entries.push(ForeignEntry {
            name: name.to_owned(),
            vis: def_row.vis,
            kind,
            generic: false,
            defaulted: false,
            decl_span: rebase_span(def_row.span, map),
            sig,
            row: EffectRow::default(),
            caps: 0,
            value,
            ty,
            link: 0,
            origin: None,
            func: None,
            std: true,
        });
    };
    for name in ["concat", "to_upper", "to_lower", "trim"] {
        let def = pkg
            .package
            .lookup_value(name)
            .expect("the core declares the extern");
        let sig = pkg.decls.fns.get(&def).map(|sig| rebase_sig(sig, map));
        entry(name, def, ForeignKind::Fn, sig, None, None);
    }
    for name in ["row", "config", "response"] {
        let def = pkg
            .package
            .lookup_type(name)
            .expect("the members file declares the type");
        let ty = facts.get(&def).cloned();
        entry(name, def, ForeignKind::Type, None, None, ty);
    }
    let urls = pkg
        .package
        .lookup_value("urls")
        .expect("the members file declares the const");
    let value = pkg
        .decls
        .consts
        .get(&urls)
        .filter(|local| !local.mutable)
        .and_then(|local| ConstFact::of_expr(&local.init));
    entry("urls", urls, ForeignKind::Binding, None, value, None);
    MemberSurface {
        entries,
        tags: Vec::new(),
        impls: Vec::new(),
        inherent: Vec::new(),
    }
}

/// The `pub import` chain's compiled half: the analyzed bundle and
/// read-back modules the consumer's compile consumes.
struct PubChain {
    /// The origin's text registered in the middle's registry.
    olib_in_mid: FileId,
    /// The origin's read-back module.
    olib_back: Module,
    /// The origin source's content hash.
    origin_hash: Hash128,
    /// The middle member's analysis (the re-exports' home).
    mid: ds_check::Analyzed,
    /// The middle's own source registered in the middle's registry.
    mid_file: FileId,
    /// The middle's lowered module (its own callable's body).
    mid_lowered: ds_lower::Lowered,
    /// The middle's read-back module.
    mid_back: Module,
    /// The middle source's content hash.
    mid_hash: Hash128,
}

/// Compiles the `pub import` chain's two members as their own units: the
/// origin member first (its exports serving the middle's pub row through
/// the surface below), then the middle — the origin's text registering
/// after its own source, the pub row linking the origin's surface, and
/// the middle's own linked call addressing the origin over its recorded
/// dep. The middle-side refusal variants (the selectionless pub row, the
/// merged-ambient `@std` row) refuse the middle's own compile — asserted
/// at their seams — so a refusal answers `None` and the input never runs.
fn pub_chain(program: &Program) -> Option<PubChain> {
    let mut olib_files = Files::new();
    let olib_file = olib_files.add_source("@olib/lib.ds", &program.origin);
    let olib_ast = Arc::new(parse(&olib_files, olib_file).expect("the origin parses"));
    let olib = analyze(&olib_files, std::slice::from_ref(&olib_ast));
    assert!(
        olib.checked.diags.is_empty(),
        "the origin checks clean: {}",
        olib.checked
            .diags
            .iter()
            .map(|diag| human(diag, &olib_files))
            .collect::<String>()
    );
    let origin_hash = Hash128::compute(program.origin.as_bytes());
    let olib_lowered = lower_library(
        ORIGIN_MODULE,
        origin_hash,
        &[olib_ast],
        &olib,
        None,
        None,
        &ComptimeValues::default(),
    )
    .expect("the origin lowers");
    let olib_bytes = write(olib_lowered.module());
    let olib_back = read(&olib_bytes).expect("the origin round-trips");
    assert_eq!(
        write(&olib_back),
        olib_bytes,
        "the origin round trip stays byte-stable"
    );
    let mut mid_files = Files::new();
    let mid_file = mid_files.add_source("@mlib/mid.ds", &program.mid);
    let olib_in_mid = mid_files.add_source("@olib/lib.ds", &program.origin);
    let mut olib_mid_rebase = HashMap::new();
    olib_mid_rebase.insert(olib_file, olib_in_mid);
    let face = origin_surface(&olib, &olib_lowered, &olib_back, &olib_mid_rebase);
    let mid_ast = Arc::new(parse(&mid_files, mid_file).expect("the middle parses"));
    let mut mid_imports = ImportGraph::new();
    for import in mid_ast
        .blocks
        .iter()
        .flat_map(|block| block.items.iter())
        .filter_map(|item| match &item.kind {
            ds_parse::ItemKind::Import(import) => Some(import),
            _ => None,
        })
    {
        let head = &import.path.segments[0];
        // the @std row stays surfaceless (the merged-ambient reading):
        // the resolver's own pub refusal owns the word there
        if import.path.at_sign && head.name == "olib" {
            mid_imports.add(import.span, ImportSurface::Linked(face.clone()));
        }
    }
    let mid = analyze_owned_linked(&mid_files, vec![mid_ast], &[false], &mid_imports);
    let mid_refusal = |needle: &str| {
        mid.checked
            .diags
            .iter()
            .any(|diag| diag.code == ErrorCode::VisNoTarget && format!("{diag:?}").contains(needle))
    };
    match program.pub_variant {
        PubVariant::Selectionless => {
            assert!(
                mid.checked.diags.len() == 1 && mid_refusal("no selection list"),
                "the selectionless pub row refuses exactly once: {}",
                mid.checked
                    .diags
                    .iter()
                    .map(|diag| human(diag, &mid_files))
                    .collect::<String>()
            );
            return None;
        },
        PubVariant::Std => {
            assert!(
                mid_refusal("merged-ambient reading"),
                "the @std pub row refuses at the merged-ambient seam: {}",
                mid.checked
                    .diags
                    .iter()
                    .map(|diag| human(diag, &mid_files))
                    .collect::<String>()
            );
            return None;
        },
        _ => {},
    }
    assert!(
        mid.checked.diags.is_empty(),
        "the middle checks clean: {}",
        mid.checked
            .diags
            .iter()
            .map(|diag| human(diag, &mid_files))
            .collect::<String>()
    );
    let mid_hash = Hash128::compute(program.mid.as_bytes());
    let mid_lowered = lower_library(
        MID_MODULE,
        mid_hash,
        &mid.asts,
        &mid,
        None,
        None,
        &ComptimeValues::default(),
    )
    .expect("the middle lowers");
    // the middle's own linked call addresses the origin, so its dep list
    // names the origin before the write validates the FuncRef against it
    let mid_lowered = mid_lowered.with_deps(vec![ds_mod::Dep {
        name: ORIGIN_MODULE.to_owned(),
        content_hash: origin_hash,
    }]);
    let mid_bytes = write(mid_lowered.module());
    let mid_back = read(&mid_bytes).expect("the middle round-trips");
    assert_eq!(
        write(&mid_back),
        mid_bytes,
        "the middle round trip stays byte-stable"
    );
    Some(PubChain {
        olib_in_mid,
        olib_back,
        origin_hash,
        mid,
        mid_file,
        mid_lowered,
        mid_back,
        mid_hash,
    })
}

/// Compiles the @std episode's declaring graph: the package's own
/// compile analyzes the core beside the migrated members' file (the
/// episode's model of the reserved package, no ambient ahead of it),
/// then the declaring compile registers the same texts at fresh
/// registry ids ahead of the episode's consumer — the core as the
/// ambient member, the members file beside it — while the @std row
/// binds through the linked surface over both. The drawn `response`
/// refusal variant (the opaque member's name binding while its value
/// use keeps the M3 contract refusal) refuses at the consumer's own
/// lowering — asserted at its seam — so a refusal answers `None` and
/// the episode never runs. Answers the compiled consumer module, its
/// dep list naming the reserved package at the core's own identity
/// (no linked call addresses it — the extern face is bodiless, its
/// calls answered by the natives the consumer's own lowering wires by
/// name).
fn std_chain(program: &Program) -> Option<Module> {
    let mut pkg_files = Files::new();
    let pkg_core = pkg_files.add_source("@std/core.ds", &program.std_core);
    let pkg_members = pkg_files.add_source("@std/std_members.ds", &program.std_members);
    let pkg_core_ast = Arc::new(parse(&pkg_files, pkg_core).expect("the package core parses"));
    let pkg_members_ast =
        Arc::new(parse(&pkg_files, pkg_members).expect("the members file parses"));
    let pkg = analyze(&pkg_files, &[pkg_core_ast, pkg_members_ast]);
    assert!(
        pkg.checked.diags.is_empty(),
        "the @std package members check clean: {}",
        pkg.checked
            .diags
            .iter()
            .map(|diag| human(diag, &pkg_files))
            .collect::<String>()
    );
    let mut std_files = Files::new();
    let ambient = std_files.add_source("@std/core.ds", &program.std_core);
    let std_id = std_files.add_source("gen_std.ds", &program.std_source);
    // the linked members' text registers after the compile unit's own
    // files, the pipeline's own order — its spans rebase into it for
    // rendering while its defs cross as link facts alone
    let members_in_cons = std_files.add_source("@std/std_members.ds", &program.std_members);
    let mut pkg_rebase = HashMap::new();
    pkg_rebase.insert(pkg_core, ambient);
    pkg_rebase.insert(pkg_members, members_in_cons);
    let std_face = std_surface(&pkg, &pkg_rebase);
    let ambient_ast = Arc::new(parse(&std_files, ambient).expect("the ambient core parses"));
    let std_ast = Arc::new(parse(&std_files, std_id).expect("the std consumer parses"));
    let mut std_imports = ImportGraph::new();
    for import in std_ast
        .blocks
        .iter()
        .flat_map(|block| block.items.iter())
        .filter_map(|item| match &item.kind {
            ds_parse::ItemKind::Import(import) => Some(import),
            _ => None,
        })
    {
        let head = &import.path.segments[0];
        if import.path.at_sign && head.name == "std" {
            std_imports.add(import.span, ImportSurface::Linked(std_face.clone()));
        }
    }
    let std_analyzed = analyze_owned_linked(
        &std_files,
        vec![ambient_ast, std_ast],
        &[false, false],
        &std_imports,
    );
    assert!(
        std_analyzed.checked.diags.is_empty(),
        "the std consumer checks clean over the core ambient: {}",
        std_analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &std_files))
            .collect::<String>()
    );
    let std_lowered = match lower(
        STD_MODULE,
        Hash128::compute(program.std_source.as_bytes()),
        &std_analyzed.asts,
        &std_analyzed,
    ) {
        Ok(lowered) => lowered,
        Err(LowerError::Unsupported { span, .. }) if program.std_opaque => {
            // the honest refusal: the opaque member's value use keeps the
            // local spelling's own M3 contract refusal, refused at the
            // consumer's own file exactly once
            assert_eq!(
                span.file, std_id,
                "the response value use refuses inside the std consumer"
            );
            return None;
        },
        Err(other) => panic!("the std consumer lowers: {other}"),
    };
    assert!(
        !program.std_opaque,
        "the opaque value use must refuse at lowering"
    );
    let std_lowered = std_lowered.with_deps(vec![ds_mod::Dep {
        name: "@std".to_owned(),
        content_hash: Hash128::compute(program.std_core.as_bytes()),
    }]);
    let std_bytes = write(std_lowered.module());
    let std_back = read(&std_bytes).expect("the std consumer round-trips");
    assert_eq!(
        write(&std_back),
        std_bytes,
        "the std consumer round trip stays byte-stable"
    );
    Some(std_back)
}

/// Runs the whole chain over one fuzz input: parse, check, lower, module
/// round trip, install, run — and asserts the discipline: the generated
/// program parses and checks clean, lowers, round-trips, installs, main
/// returns unit, the captured answers equal the oracle, and the heap
/// drains. The linked-enum episode makes the chain a two-module compile:
/// the library member analyzes, lowers, and round-trips as its own
/// module first, its export surface serves the consumer's import row
/// through the link walk, and the consumer's module links it as its one
/// recorded dep. The `pub import` episode widens the chain to three
/// modules — the origin member's exports serving the middle's pub row,
/// the middle's surface (the re-exports carrying the origin's position)
/// serving the consumer — while the @std episode compiles a second
/// consumer over the core-shaped ambient with the reserved package's
/// linked surface serving the extern face beside the migrated members'
/// shapes, run as its own module entry after the first. The conflict
/// variant refuses at lowering and the drawn pub refusal variants and
/// the drawn `response` value-use refusal refuse at their own compile
/// seams, none ever running.
/// Returns the draw census for the mirror's coverage asserts.
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
    // the drawn budget refusal: the consumer's parse answers the chain
    // or arrow budget diagnostic, so the input never runs (parsed over
    // its own registry, since no cross-file span is involved)
    match program.chain_over {
        ChainOver::Clean => {},
        ChainOver::TypeChain => {
            let mut own = Files::new();
            let file = own.add_source("gen_exec.ds", &program.source);
            let diags = parse(&own, file).expect_err("a past-budget chain refuses at parse");
            assert!(
                diags
                    .iter()
                    .any(|diag| diag.code == ErrorCode::NestingTooDeep),
                "the chain budget answers its diagnostic: {}",
                diags
                    .iter()
                    .map(|diag| human(diag, &own))
                    .collect::<String>()
            );
            return program.drawn;
        },
        ChainOver::ArrowChain => {
            let mut own = Files::new();
            let file = own.add_source("gen_exec.ds", &program.source);
            let diags = parse(&own, file).expect_err("a past-budget arrow run refuses at parse");
            assert!(
                diags
                    .iter()
                    .any(|diag| diag.code == ErrorCode::ArrowChainTooDeep),
                "the arrow budget answers its diagnostic: {}",
                diags
                    .iter()
                    .map(|diag| human(diag, &own))
                    .collect::<String>()
            );
            return program.drawn;
        },
    }
    // the `pub import` chain: the origin and middle members compile as
    // their own units, the drawn middle-side refusals (the selectionless
    // pub row, the merged-ambient @std row) already asserted at their
    // seams — a refusal answers None and the input never runs
    let Some(chain) = pub_chain(&program) else {
        return program.drawn;
    };
    let mut files = Files::new();
    let id = files.add_source("gen_exec.ds", &program.source);
    let member = files.add_source("@lib/lib.ds", &program.library);
    let olib_in_cons = files.add_source("@olib/lib.ds", &program.origin);
    let mid_in_cons = files.add_source("@mlib/mid.ds", &program.mid);
    let mut rebase = HashMap::new();
    rebase.insert(lib_file, member);
    let surface = linked_surface(&lib, &lib_lowered, &lib_back, &rebase);
    // the middle's surface over the consumer's registry: the pub row's
    // spans and the two packages' texts all rebasing into it
    let mut mid_rebase = HashMap::new();
    mid_rebase.insert(chain.mid_file, mid_in_cons);
    mid_rebase.insert(chain.olib_in_mid, olib_in_cons);
    let middle = middle_surface(&chain.mid, &chain.mid_lowered, &chain.mid_back, &mid_rebase);
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
            // a fresh dep position past the chain's packages: the refusal
            // fires at the tag seeding before any FuncRef addresses it
            link: ORIGIN_POS + 1,
            origin: None,
            func: None,
            std: false,
        };
        let unit = || ds_check::import::CasePayload::Unit;
        conflict.entries.push(entry("spare", TypeFact::Enum {
            cases: vec![ds_check::import::TypeCase {
                name: "clash".to_owned(),
                payload: unit(),
            }],
            linear: false,
        }));
        conflict.entries.push(entry(
            "clash",
            TypeFact::Case(ds_check::import::CaseFact {
                enum_name: "spare".to_owned(),
                payload: unit(),
            }),
        ));
        conflict.tags.push(ds_mod::MemberTag {
            decl: "clash".to_owned(),
            name: "clash".to_owned(),
            id: claimed,
        });
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
        } else if import.path.at_sign && head.name == "mlib" {
            &middle
        } else if import.path.at_sign && head.name == "other" && program.linked_conflict {
            &conflict
        } else {
            continue;
        };
        imports.add(import.span, ImportSurface::Linked(linked.clone()));
    }
    let analyzed = analyze_owned_linked(&files, vec![ast], &[false], &imports);
    if program.pub_variant == PubVariant::Private {
        // the honest refusal: the privately-imported name never re-exported
        // (the private row records no binding), so the selection names no
        // definition the middle exports
        assert!(
            analyzed.checked.diags.len() == 1
                && analyzed.checked.diags[0].code == ErrorCode::ImportNameMissing
                && format!("{:?}", analyzed.checked.diags[0]).contains("`quiet`"),
            "the privately-imported name refuses the consumer's link walk: {}",
            analyzed
                .checked
                .diags
                .iter()
                .map(|diag| human(diag, &files))
                .collect::<String>()
        );
        return program.drawn;
    }
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
    // consuming module must name @lib, the chain's middle, and the
    // chain's origin (the position the re-exported FuncRefs address)
    // before it round-trips
    let lowered = lowered.with_deps(vec![
        ds_mod::Dep {
            name: LIB_MODULE.to_owned(),
            content_hash: lib_hash,
        },
        ds_mod::Dep {
            name: MID_MODULE.to_owned(),
            content_hash: chain.mid_hash,
        },
        ds_mod::Dep {
            name: ORIGIN_MODULE.to_owned(),
            content_hash: chain.origin_hash,
        },
    ]);
    let bytes = write(lowered.module());
    let back = read(&bytes).expect("the lowered module round-trips");
    assert_eq!(
        write(&back),
        bytes,
        "the reader round trip stays byte-stable"
    );
    let consumer = back;
    let Some(std_back) = std_chain(&program) else {
        // the drawn opaque refusal already asserted its seam (the
        // `response` value use refusing the consumer's own lowering), so
        // the input never runs
        return program.drawn;
    };
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, lib_back).expect("the library installs");
    loader::install(&mut registry, chain.olib_back).expect("the origin installs");
    loader::install(&mut registry, chain.mid_back).expect("the middle installs");
    loader::install(&mut registry, consumer).expect("the generated program installs");
    loader::install(&mut registry, std_back).expect("the std consumer installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run(MODULE_NAME, "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns unit"
    );
    assert_eq!(
        vm.run(STD_MODULE, "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the std consumer's main returns unit"
    );
    let mut expected = program.expected;
    expected.push_str(&program.std_expected);
    assert_eq!(
        vm.captured_output(),
        expected.as_bytes(),
        "the observable answers match the generator's oracle"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the runs end ({} live)",
        vm.objects().len()
    );
    program.drawn
}
