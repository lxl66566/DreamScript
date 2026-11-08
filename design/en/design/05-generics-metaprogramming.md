# 05 — Generics and Metaprogramming (decision catalog)

Status: Phase 2 option catalog, written 2026-11-04, awaiting author decisions per AGENTS.md. Primary input: docs/en/research/05-generics-metaprogramming.md (polymorphism survey, anti-specialization evidence, set-theoretic overloading, comptime spectrum, derives, VM execution, linear interplay, five combos). Secondary inputs: docs/en/research/03-runtime-ir-jit.md (exec items 3/6/10-12, section 2 on dyn inline caches, section 8 "Generics implementation and the anti-specialization stance", Table 6) and the decision-point index tables of docs/en/design/01-types.md (B1c hybrid set types, B9a universal structural equality), docs/en/design/02-linearity-ownership.md (C1b affine core, C2d second-class parameter modes), docs/en/design/03-effects-errors-async.md (E1d deferred effects, B5d rows-later). docs/en/design/04-syntax.md is referenced by path only (may be mid-write); it owns all surface tokens.

Reading guide: each decision point presents 3-4 named options. Per option: a syntax example (illustrative neutral TS-family syntax; the examples predate the docs/en/design/04 surface decisions and illustrate semantics only; surface syntax is decided in docs/en/design/04-syntax.md), a prior-art pointer into research/05, tradeoffs across learning/perf/sugar/compiler, and what the option forecloses. Each point ends with a one-line Recommendation. The runtime baseline every "perf" claim assumes is fixed in "Shared assumptions" below and comes from research/03.

## Decision-point index

| ID | Decision point | Options | Recommendation (one line) |
|---|---|---|---|
| B6 | Polymorphism mechanism | (a) cohtraits: coherent set-theoretic traits + disjoint heads + built-in derives, (b) explicit: dictionary/capability passing with no implicit search, (c) modules: ML functors, (d) comptime: Zig-style comptime generics | (a) — the only option satisfying all four hard constraints (research/05 section 9 scoring); pairwise-disjoint instance heads (`T1 & T2 = Empty`) replace the orphan rule |
| G1 | Metaprogramming | (a) none: built-in derives only, (b) comptime: comptime evaluation of ordinary code + static type reflection, no macros, (c) macros: hygienic syntax macros, (d) procmacros: token/proc macros | (b) — covers research/05's minimal sufficient set and deletes the hygiene question entirely; built-in derive attributes ship on top under every option |
| G2 | First-class types and reflection | (a) types-as-values: types are runtime values (Julia), (b) comptime-reflection: reflection at comptime only, no runtime metadata, (c) metadata: Java/C# runtime metadata tables, (d) tags-as-data: unions are ordinary data, `is`/match covers runtime questions | (b) — comptime-only reflection; its runtime half is (d), which docs/en/design/01's B1c hybrid set types deliver for free |
| H3 | Generics execution | (a) evidence-ic: one body + evidence words + polymorphic inline caches, lazy JIT specialization, (b) mono-cache: bytecode-level monomorphization cache, (c) erasure: full erasure with boxed values, (d) comptime-mono: comptime monomorphization only | (a) — compiler-internal caching of already-unique dispatch, semantics-invisible (research/05 section 6.4; research/03 section 8); the JIT tier's lazy-specialization cache is (b)'s table |

## Shared assumptions (binding on every option)

- Runtime baseline (research/03): typed register bytecode with unboxed slots per static type (exec item 6 — typed registers hold unboxed i32/i64/f64 directly; ADTs are unboxed tag+payload pairs), no runtime type guards or field inline caches because every operand's type is a compile-time constant (exec item 3), and a tier pipeline of interpreter plus copy-and-patch JIT plus optional typed-SSA tier (exec item 12). Existential/dyn call sites keep a 1-2 entry inline cache as a pure perf hint with no correctness role (research/03 section 2, item 1).
- Type foundation: docs/en/design/01 recommends B1c hybrid (nominal tags composing set-theoretically: unions, intersections, negations over nominal tags) and B9a universal structural `==` with the five carve-outs of research/05 section 5.2. This file builds on those recommendations and does not re-decide them.
- Linearity: docs/en/design/02 recommends C1b (affine core + opt-in linear layer) and C2d (second-class parameter modes carrying borrow/consume/mutate). Referenced by path only; see "Linear interplay" below.
- Effects and errors: docs/en/design/03 recommends E1d (handler runtime underneath, no surface effect syntax; E1c capability idiom in the stdlib) and B5d (inferred error unions now, rows later). The HKT deferral below references E1d.
- Anti-specialization stance (charter; research/05 section 3): dispatch is determined by types and the set algebra, never by declaration order, priority rules, or "most specific match". Every option below is stated so it either satisfies this invariant or says plainly where it does not.
- Two-axis decomposition (research/05 executive summary): every polymorphism mechanism splits into (a) how instances are found and (b) how calls execute. B6 decides axis (a); H3 decides axis (b). The pain points of Rust and Haskell live on axis (a); axis (b) is representation strategy.

## B6 — Polymorphism mechanism

The question: when a generic function needs `Eq[T]`, where does the implementation of `Eq` for the concrete `T` come from — a globally-registered coherent instance set (a), a value the caller passes (b), a module the caller applies (c), or nowhere because every instantiation was generated at compile time (d)? All four must coexist with set-theoretic types (docs/en/design/01 B1c) and affine values (docs/en/design/02 C1b).

### B6.a — `cohtraits`: coherent set-theoretic traits with disjoint instance heads + built-in derives

The Rust-flavor shape with the coherence rule replaced: traits may carry associated types; instances are declared for type heads; and the only rule is pairwise disjointness of heads of the same trait — for every pair of heads `H1`, `H2`, the checker must prove `H1 & T2 = Empty` (decidable set emptiness). This is resolution-as-intersection-subtyping: an instance `I => C[T]` is an arrow `I -> C`, the instance environment is an intersection of arrows, and resolving a constraint is a subtyping judgment via modus ponens (Marntirosian, Schrijvers, Thiemann, Steel, "Resolution as Intersection Subtyping via Modus Ponens", OOPSLA 2020; research/05 sections 2.2-2.3). Uniqueness of match then follows from the type algebra; no priority machinery ever exists.

```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
trait Ord { fn compare(&self, other: &Self) -> Int }                 // one-parameter trait
trait Iterator { type Item; fn next(&mut self) -> Option<Self.Item> } // associated type

impl Ord for Int { ... }                // concrete head: a set of values
impl[T: Ord] Ord for List[T] { ... }    // conditional head: an intersection arrow (OOPSLA'20)

// the disjoint-head rule: for every pair of heads of one trait, H1 & H2 = Empty
impl Draw for Circle { ... }
impl Draw for Square { ... }
impl Draw for Circle | Square { ... }   // REJECTED: intersects both heads; witness type: Circle
impl Draw for Circle & ~Shadowed { ... } // negation carves exclusivity when overlap is intended
```

- Orphan-rule replacement. Rust forbids overlap positionally ("wrong crate", research/05 section 3.3 mechanism 1) and pays in newtype wrappers, blanket-impl vetoes on future impls, and sealed-trait contortions (research/05 section 3.2). DreamScript proves non-overlap precisely instead of forbidding it pessimistically: any module may define an instance for any type (foreign-type instances are legal), registration is global, and the disjointness checker runs over the final instance environment at link time; a collision is a loud compile error naming the two heads and a witness value type in their intersection (research/05 section 3.5: collisions must be impossible or loud, never silent). This refines research/03 section 8's "instance coherence (global uniqueness, Haskell-style) supplies dictionaries" — same dictionary semantics, coherence rule upgraded from positional uniqueness to set disjointness.
- Unions (docs/en/design/01 B1c). Heads are sets, so union heads normalize: `impl Eq for Int | Str` holds exactly when each member holds — it is derived evidence combining per-branch instances, and the checker decomposes it into components before the pairwise disjointness check. Conversely, a union appearing inside another head intersects the components and must be disjoint from them. For a value whose static type is a union `A | B`, the runtime tag selects the branch's evidence; because heads are pairwise disjoint, the tag identifies at most one instance, so an inline cache keyed by tag never needs priority tie-breaking and invalidation is monotone (a value's type only narrows, and the narrower type hits the same unique instance — research/05 section 6.3, closing paragraph).
- Termination. Conditional instances are resolution rules; recursive resolution needs a termination argument, which the Modus Ponens calculus supplies (research/05 section 2.3 and combo-A risk note). The compiler must reject a conditional instance whose satisfaction requires itself.
- Prior art: research/05 sections 1.1, 2.2, 2.3, 3.2, 3.3 (mechanisms 1 and 5), section 9 combo A; Castagna, Nguyen, Xu, Frisch, "Polymorphic Functions with Set-Theoretic Types" (POPL 2015 — overloading as arrow intersection, the `◁` relation); Parreaux and Chau, MLstruct (OOPSLA 2022 — type classes listed as compatible extension of a Boolean-algebra type system); Xu, Huang, Oliveira (OOPSLA 2025 — merges kept coherent by apartness, i.e. provable non-overlap).
- Tradeoffs. Learning: low-medium — traits, associated types, one disjointness rule; no orphan rule, no coherence lawyer, no specialization vocabulary (research/05 section 9). Perf: instances compile to evidence words passed as arguments; statically-resolved evidence is a constant operand, runtime-resolved evidence goes through a per-site inline cache (research/05 section 6.3). Sugar: near zero — built-in derive attributes carry the boilerplate and Koka-style UFCS gives method-call ergonomics on plain functions. Compiler: a pairwise emptiness check per instance group (quadratic in group size, small in practice, cacheable — research/05 section 2.2), a termination check for conditional instances, and overlap errors that must name a witness type to stay instructive.
- Forecloses: overlapping instances and specialization permanently (no OVERLAPPING/INCOHERENT-style pragmas, no most-specific-match); first-match or priority-ordered resolution anywhere instances appear, including comptime-generated ones (research/05 section 3.1 corollary); and competing same-head instances in different scopes (that is B6.b's feature, not this one's).

Recommendation: adopt B6(a) — coherent set-theoretic traits, pairwise-disjoint instance heads checked by set emptiness at declaration and link, built-in derives; it is the only option that simultaneously satisfies set-theoretic types, no-overlap, linear-friendliness, and bytecode-native execution (research/05 section 9 scoring table).

### B6.b — `explicit`: explicit dictionary/capability passing, no global implicit search

There is no resolution at all: the instance is an ordinary value, whoever needs it takes it as a parameter. Effekt's capability-passing effect system proves the explicit path scales to full effect polymorphism with zero coherence questions (Brachthäuser, Schuster, Ostermann, OOPSLA 2020; research/05 sections 3.3 mechanism 3, 3.4).

```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
type Eq[T] = { fn equal(&T, &T) -> Bool }        // the instance is an ordinary value
fn dict_get[K, V](d: &Dict[K, V], k: &K, eq: Eq[K]) -> Option[V]

dict_get(m, key, eqPair(eqInt, eqStr))           // fully explicit: nothing is ever resolved
dict_get(m, key, _)                              // sugar: insert the unique Eq[K] when provable
```

- The `_` line is where magic seeps back in: inserting "the unique instance when provable" is scoped resolution lite (research/05 section 3.4), so pure B6.b and B6.b-with-sugar are different points on one dial. Scala 3's `given`/`using` is the evidence for the sugared end: lexical scoping, nearest-wins, ambiguity-as-error keeps scoped search deterministic (research/05 section 1.5); Kotlin KEEP-367 context parameters (shipped in 2.2, 2025) are the mainstream scoped variant. Ambiguity-as-error is non-negotiable if any sugar is added.
- Tradeoffs. Learning: lowest conceptually ("instances are values, nothing is found for you") but the boilerplate wall is real — Scala's implicit search exists precisely because manual threading hurts, and Effekt mitigates by making handlers ambient per scope rather than manual parameters (research/05 sections 3.4, 9 combo B). Perf: identical to B6.a's representation — evidence words either way, ICs apply either way, statically-resolved evidence is a constant operand. Sugar: high without mitigation; call sites at API boundaries grow evidence arguments. Compiler: trivial — no search, no global instance table, no disjointness checker; every error message names the exact evidence passed.
- Linearity fit is the best of the four: capabilities are ordinary (possibly affine) values, so affine flow analysis sees every use, and evidence consumed once is expressed with the same C1b/C2d machinery as any other value.
- Forecloses: implicit instance ecosystems — retroactive conformances, blanket-instance ergonomics, `x == y` operator sugar unless the unique instance is insertable, and trait-object-style extension without visible plumbing. Also forecloses B6.a's global "implement once, usable everywhere" library experience.
- Prior art: research/05 sections 1.4 (OCaml functors as explicit module arguments), 1.5 (Scala givens), 3.3, 3.4, 9 combo B; Effekt capabilities (OOPSLA 2020); Koka handler values.

Recommendation: keep B6(b) as the escape hatch, not the v1 default — adopt its named scoped instances later if disjoint-head resolution ever feels too implicit (research/05 section 3.4 synthesis: coherent declarations by default, scoped named instances as second path, ambiguity always an error).

### B6.c — `modules`: ML modules and functors

Generics are functor applications; instances are module arguments; generative functors make two applications of the same functor two distinct abstract types, so multiple instances coexist without ambiguity by construction; first-class modules (packing a module into a runtime value) are the explicit spelling of existentials (research/05 section 1.4).

```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
module type Ordered = { type T; fn compare(&T, &T) -> Int }
module Map(O: Ordered) = { ... }          // generative: two applications, two distinct types
module StringMap  = Map(StringOrd)        // explicit instantiation 1
module ReverseMap = Map(ReverseOrd)       // explicit instantiation 2: coexists, no conflict
```

- Tradeoffs. Learning: high — signatures, applicative vs generative functors, first-class-module packing; this strains the Kotlin sugar ceiling directly (charter hard requirement 4; research/05 section 9 combo C). Perf: excellent and fully static — no runtime machinery at all. Sugar: heavy for collection code — the reason OCaml's ecosystem grew ppx codegen and first-class-module shims; a `BTreeMap[K: Ord]` one-liner becomes two-step instantiation. Compiler: moderate (signature matching, generativity) but the derive story is the weakest of all four options — no constraint-directed member synthesis; OCaml needed a universal polymorphic compare plus external ppx plugins (research/05 sections 1.4, 5.1).
- Forecloses: lightweight constraint-based overloading, retroactive instance definitions, and method-call ergonomics over trait-like bundles; it makes the everyday generic container API visibly two-step.
- Worth stealing into B6.a either way: first-class modules as existentials (`box Trait`-style evidence-carrying boxes, research/05 section 8.1) and generative abstract types for capabilities.
- Prior art: research/05 sections 1.4, 9 combo C; Rossberg, 1ML (ICFP 2018 — core and module language unified, "F-ing modules").

Recommendation: reject B6(c) as the whole-language direction; borrow its first-class-module existential fragment into B6(a) if plugin-style abstract types are ever needed.

### B6.d — `comptime`: comptime generics (types are compile-time values)

Generics are compile-time functions from types to types; there are no instances, no evidence, no resolution — only instantiation. Zig is the existence proof (research/05 sections 4.1 level 3, 9 combo D).

```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
fn List(comptime T: type) type {
  return struct { items: [T], len: Int }
}
var xs: List(Point) = ...
```

- Interaction with the typed register VM: there is no runtime polymorphism at all — every instantiation is its own bytecode body, so every register stays unboxed and every opcode is fully representation-specific (the research/03 exec 6/section 1 baseline taken to its maximum). The cost side: the interpreter must materialize a bytecode body per instantiation at first use, so compile cost is visible at startup, and bytecode size scales with the instantiation fan-out (research/05 sections 6.1, 9 combo D — Julia pays this at the JIT tier, not the interpreter tier).
- Set-theoretic fit is poor: comptime-generated instances escape the static set algebra, so the non-overlap invariant is not enforceable over them — a comptime loop can generate two heads that intersect, discovered (if at all) after generation (research/05 section 9 combo D). The linear checker must run on generated code, which Zig does not do.
- Anti-specialization fit is mixed: no source-level overload priority exists, but the semantics is template instantiation — a new body per argument combination — which is the mechanism the charter rejects when it entangles generics with ad-hoc rules; here nothing is ad-hoc, but the explainability story shifts from "types are sets" to "code that runs at compile time".
- Tradeoffs. Learning: medium — one concept, but a second execution phase users must learn to debug, and comptime error messages are Zig's documented weak spot. Perf: best steady state, startup risk in an interpreter. Sugar: low syntax, high semantic overhead. Compiler: a comptime evaluator plus per-instantiation materialization plus post-generation checking passes.
- Forecloses: global disjointness enforcement, set-algebra-based resolution (docs/en/design/01 B1c becomes decorative for polymorphism), evidence-polymorphic dynamic paths (no existential substrate without bolting one on).
- Prior art: research/05 sections 1.8 (C++ constexpr as the half-step), 4.1 level 3, 9 combo D; Zig comptime language reference.

Recommendation: reject B6(d) as the polymorphism mechanism; adopt its reflection half as the metaprogramming substrate instead (G1.b below) — research/05 section 9's verdict on combo D is exactly this split.

### B6 comparison

| Option | Resolution rule | Learning | Perf (interp tier) | Sugar cost | Compiler cost | Anti-spec fit |
|---|---|---|---|---|---|---|
| B6.a cohtraits | disjoint heads; resolution = subtyping (OOPSLA'20) | low-med | good (evidence + ICs) | low | emptiness checker + termination check | perfect |
| B6.b explicit | none — evidence is a value | low concept, high boilerplate | good (same representation) | med-high (mitigated by `_` sugar that re-imports resolution) | trivial | perfect |
| B6.c modules | functor application | high | excellent | high | signatures, generativity | perfect |
| B6.d comptime | comptime instantiation | medium | startup risk, best steady state | low syntax, high semantics | second phase + checking generated code | mixed |

### B6 interactions

- docs/en/design/01: B6.a consumes the set algebra for resolution (heads are tag sets under B1c; disjointness is a set computation); B6.d bypasses it. B9a universal structural `==` makes equality a language universal rather than a trait in every option, leaving `Ord`/`Hash`/`Show`/serde trait-mediated (research/05 section 5.3).
- docs/en/design/02 (C1b, C2d): B6.a expresses affine generics via `~Copyable`-style bounds and parameter conventions; B6.b composes best (evidence is an ordinary affine value); B6.c is fine (modules are values); B6.d is weakest (linearity must be checked on generated code).
- docs/en/design/03 (E1d, B5d): evidence dispatch is the same substrate E1d's handler runtime needs; B6.b and E1c are the same idiom (capability parameters) — picking one normalizes the other.
- Existentials are orthogonal and cheap in every option: an evidence-carrying box (`box Json`) is a value plus its dictionary, first-class modules are the explicit spelling (research/05 section 8.1). Default to universals in signatures — existentials box and break affine ownership reasoning (research/05 section 8.1), so reserve them for heterogeneous collections and plugin boundaries.

## G1 — Metaprogramming

The spectrum from research/05 section 4.1 runs level 0 (none; Go `generate` out-of-band codegen) through level 2 (compiler built-in synthesis: Swift SE-0185, Kotlin data classes, MoonBit `#derive`), level 3 (comptime evaluation + static reflection: Zig, D CTFE, Mojo, C++26 P2996), level 4-5 (templates, hygienic syntax macros, Racket phases) to level 6 (dependent types, out of scope under the sugar ceiling). The decision is where on it DreamScript lives.

### The minimal sufficient set (research/05 section 4.3)

All surveyed derive-like needs decompose into four needs: (1) static reflection over type structure — the field/case list with names, types, order; (2) compile-time iteration and branching over that list; (3) compile-time evaluation of ordinary code to assemble member implementations; (4) built-in derive attributes for the hot cases so the common ones are one word long and compile fast. Zig demonstrates (1)+(2)+(3) suffice for JSON serialization of arbitrary structs (`@typeInfo` + `inline for` is the whole recipe); MoonBit demonstrates (4) with `#derive(Eq, Hash, Debug, ToJson)` used pervasively in moonbit-core; Swift demonstrates that built-ins alone cover the daily cases (SE-0185). What the minimal set cannot cover: arbitrary syntax extension, grammar-level DSLs, term rewriting, and phase splitting — none of the surveyed derive-like needs requires them (research/05 section 4.3). Token macros exist mainly because those languages lack static reflection; C++ is converging to the same conclusion with C++26 static reflection (P2996) — twenty-five years after templates, "reflection, not rewriting" (research/05 sections 1.8, 4.1).

### G1.a — `none`: nothing beyond built-in derives (Go/Roc posture)

- Covers: need (4) only. Cannot cover: any user-defined derive-like abstraction the compiler did not ship — custom serialization shapes, schema generation, ORMs, test-data generation. Escape hatches are out-of-band codegen (Go `go generate`: a program writes source files; no compiler involvement, no type safety across the generation boundary — research/05 section 4.1 level 0) or runtime reflection (moves errors from compile time to runtime, against the charter, and costs the perf the charter demands).
```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
type Point = { x: Int, y: Int }        // no attribute: the compiler decides what is synthesized
// user serialization needs -> out-of-band: `dreamtool generate schema.ds` writes .ds source
```

- Tradeoffs. Learning: lowest. Perf: zero overhead, zero machinery. Sugar: zero. Compiler: zero. But the ceiling is the compiler team's shipping schedule: every ecosystem need becomes a compiler feature request.
- Forecloses: all user extensibility of member generation; pushes the ecosystem toward external generators with their parse/regenerate fragility.
- Prior art: research/05 sections 4.1 level 0/2, 5.1 (Go row, weakest surveyed derive story); Roc (structural equality plus capability requirements, no derive at all).

Recommendation within G1: viable only as a v1 staging point on the way to G1.b; as a terminal choice it starves the ecosystem or invites out-of-band codegen.

### G1.b — `comptime`: comptime evaluation of ordinary code + static type reflection, no macros

The language's own semantics run at compile time over `type` values; reflection reads type structure (`typeof`/fields/cases); there is no second language and no token manipulation. Zig's philosophy statement is the cleanest articulation: comptime is just Zig code evaluated at compile time, and macros are unnecessary because code can generate code (research/05 section 4.1 level 3).

```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
fn schema(comptime T: type) -> String {        // need (1)+(2): fields of T at comptime
  var s = "{"
  for f in fields(T) { s += f.name + ": " + type_name(f.type) + ", " }
  return s + "}"
}
@derive(Show, ToJson)                          // need (4): built-in derives for hot cases
type Point = { x: Int, y: Int }
```

- Coverage: needs (1)(2)(3) fully; with built-in derives on top (they are compiler synthesis and ship under every G1 pick — see Derives section) the minimal set is fully covered.
- Hygiene: deletes the question entirely — there is no substitution, generated code is ordinary code, and reflection reads structure instead of rewriting it (research/05 section 4.2). This is the structural argument over G1.c/G1.d, not a stylistic one.
- Tradeoffs. Learning: medium — one new concept ("run ordinary code at compile time") but a second execution phase to learn to debug; comptime failures are compile-time-eval failures with a distinctive error shape. Perf: zero runtime overhead — everything resolves before bytecode emission; compile-time cost grows with comptime usage. Sugar: low syntax; the comptime/runtime boundary is a semantic line users must locate. Compiler: one comptime evaluator sharing tier-1 semantics (two phases, one language), plus per-type reflection tables generated at declaration.
- Forecloses: token macros and user syntax extension permanently in practice — they could be bolted on later, but the explainability budget (charter: fully explainable in a short tutorial) is spent once and G1.b spends it on a concept users already know (functions) rather than a second meta-language.
- Prior art: research/05 sections 4.1 level 3, 4.2, 4.3, 9 combo D (reflection half); Zig comptime and `@typeInfo`; D CTFE; Mojo `comptime`; C++26 P2996.

Recommendation within G1: adopt G1(b) — it is the only option that covers the minimal sufficient set with zero hygiene machinery and keeps the meta-language identical to the object language.

### G1.c — `macros`: hygienic syntax macros (Rust `macro_rules`/Scheme flavor)

Pattern-to-expansion substitution with hygiene: identifiers introduced by the macro body are renamed per expansion site so they cannot capture call-site identifiers (Rust token-level hygiene; Racket syntax objects make hygiene default with explicit escape — research/05 section 4.2).

```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
macro vec {
  ($($x:expr),*) => { { let mut v = List(); $(v.push($x);)* v } }
}
```

- The hygiene burden made concrete: expansion happens before type checking, so a typed language must either re-implement typing inside the macro system (Typed Racket's approach) or defer errors to post-expansion tokens with no type information (Rust's token trees) — research/05 section 4.1 level 5. Hygiene itself is per-identifier rename bookkeeping with escape hatches (`datum->syntax`, Nim's `{.inject.}`), and making macros compose soundly requires a phase discipline ("macros that work together", Flatt/Findler 2018). Each of these is a subsystem G1.b does not need.
- Coverage: needs (1)-(4) partially — derives are expressible (Rust does them via proc macros, not `macro_rules`, precisely because typed member generation needs type information), but the real product of syntax macros is sugar: control-flow-shaped DSLs and zero-cost idioms. The charter's sugar budget is capped at Kotlin's total surface, which contains no macro system; spending it here means cutting elsewhere.
- Tradeoffs. Learning: high — a second pattern/expansion language with its own scoping rules. Perf: zero runtime cost. Compiler: expansion infrastructure, hygiene, phases, macro debugging. Error quality: type errors surface after expansion, attributed to generated code.
- Forecloses: nothing G1.b covers, and composes worse — functions compose predictably, templates less so (research/05 section 4.1 level 4). Adding macros after G1.b would create two overlapping meta-mechanisms, which the decoupling-first charter rejects.
- Prior art: research/05 sections 4.1 levels 4-5, 4.2; Nim templates (hygienic by default, stdlib built on them); Racket phases; Rust `macro_rules!`.

Recommendation within G1: reject G1(c) — it buys syntax sugar the budget does not pay for and re-implements machinery G1.b gets for free.

### G1.d — `procmacros`: token/proc macros (Rust proc flavor) — likely rejected

Proc macros are ordinary programs consuming and producing untyped token trees at compile time. They are marked likely-rejected here under the explainability budget; the catalog records the trade honestly.

- Why hygiene plus tooling cost is the reason, concretely. Hygiene: proc macros are fundamentally unhygienic — they manipulate raw token trees, so introduced identifiers can capture or be captured, and this is a leading source of proc-macro fragility (research/05 section 4.2; serde's attribute DSL living inside token streams is the canonical symptom). Tooling: a compiler-as-library boundary (parse/emit tokens), a separate proc-macro crate with its own build step and compile-time cost, IDE/debug support for expansions, and error messages pointing at expansion output rather than user intent. Coherence interaction: macro-generated impls can accidentally overlap an instance not yet written, re-opening the disjointness hole B6.a just closed unless macro output re-enters the checker (research/05 section 3.2, point 4).
```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
#[proc_derive]                                  // a user program over untyped tokens:
fn derive_json(input: Tokens) -> Tokens {       // sees token trees, not types
    let fields = parse_tokens(input);           // re-parses the language by hand
    emit_tokens("fn to_json(&self) -> String { ... }")   // emits raw syntax
}
```

- Explainability budget: "what code runs?" becomes unanswerable without knowing each macro's expansion; the charter requires the language to remain fully explainable in a short tutorial, and a token-rewriting tier in front of the typechecker is the single largest explainability cost on this page.
- Coverage: everything, at maximal machinery — Rust's ecosystem proves both the power (serde, derive_more) and the cost (a proc-macro sub-industry, slower builds, token-level error quality — research/05 section 5.1).
- Forecloses: stable, tutorial-sized language semantics; fast cold builds; and the "one language" property.
- Prior art: research/05 sections 4.1 level 5, 4.2, 5.1 (Rust derive costs), 3.2 point 4.

Recommendation within G1: reject G1(d) for v1; if a concrete serialization ecosystem demand ever appears, G1.b's reflection plus built-in derives is the evidence-backed substitute.

### G1 comparison

| Option | Minimal set covered | Hygiene machinery | Learning | Compiler cost | Forecloses |
|---|---|---|---|---|---|
| G1.a none | (4) only | none | lowest | zero | all user extensibility |
| G1.b comptime | (1)(2)(3)(4) | none needed — no substitution | medium | one comptime evaluator | token macros in practice |
| G1.c macros | (2)(3) via patterns; (1) weakly typed | rename bookkeeping + phases | high | expansion infra | composition simplicity |
| G1.d procmacros | all, untyped | none — unhygienic by construction | high | compiler-as-library + crate boundary | explainability budget |

## G2 — First-class types and reflection

The question: beyond the tags that data already carries, does a type exist at runtime as a value or metadata record? This decides what "type-level question" means at runtime versus compile time, and it constrains H3's representation choices.

### G2.a — `types-as-values`: types as runtime values (Julia flavor)

Types are ordinary runtime values; functions can take them as arguments; dispatch can key on them. This is the natural substrate for dynamic multiple dispatch (research/05 sections 1.10, 2.4).

```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
fn describe(T: type) -> String { T.name }        // a type is an ordinary runtime value
let t = typeof(p)                                // runtime reification of p's type
```

- Cost versus unboxed slots: either every slot carries a type word — tagged values, contradicting the unboxed typed-register representation of research/03 exec item 6 and re-importing exactly the type-guard machinery exec item 3 deletes — or only boxed `any` values carry type words while static slots stay unboxed. The second is coherent but is then G2.d plus comptime reflection for the static half: the runtime type value adds expressiveness only on the boxed path.
- Perf contingency: Julia's own evidence says dispatch is not the bottleneck — inference failure is (LU case study: 12 GB / 4.15 s naive down to 0.4 s after inference-friendly rewrites; arXiv 1808.03370). A statically-typed language re-importing runtime type values re-imports the type-instability cliff it never had (research/05 sections 1.10, 2.4).
- Learning: low. Compiler: type objects, method tables, and inference to recover statically-provable facts. Runtime errors: "method not found"/ambiguity move to runtime unless a static exhaustiveness check over set-typed clauses is added (research/05 section 2.4).
- Forecloses: the static-only story; unconditionally-unboxed generic paths; compile-time totality of dispatch by default.
- Prior art: research/05 sections 1.10, 2.4, 9 combo E; Julia dispatch study (arXiv 1808.03370).

### G2.b — `comptime-reflection`: comptime-only reflection

`typeof`, field enumeration, and constructor enumeration exist only inside the compiler; there is no runtime metadata of any kind. Runtime type questions are answered by the tags data already carries (G2.d's posture); compile-time type questions are answered by reflection (this option's posture).

```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
comptime {
  let info = typeof(Point)          // fields, their types, constructor cases — compile time only
  for f in info.fields { ... }      // drives derives, serializers, schemas (G1.b's engine)
}
```

- Perf: zero runtime cost — no metadata words, no tables, no reflection API in the VM. This is the C++26 P2996 / Zig posture (research/05 sections 4.1 level 3, 4.3).
- Honest boundary: a serializer driven by runtime input (a REPL inspector, a wire format chosen at runtime) cannot consult reflection — it uses tags/match or an evidence object instead. Every surveyed runtime-reflection use case in DreamScript's scope (derives, JSON, schemas) is compile-time-bounded (research/05 section 4.3).
- Forecloses: user-facing type objects, runtime type introspection APIs, runtime-driven class loading.
- Prior art: research/05 sections 4.1 level 3, 4.3; Zig `@typeInfo`; C++26 P2996.

### G2.c — `metadata`: runtime metadata tables (Java/C#)

Every value, or at least every boxed value, carries a class pointer into a metadata table; a reflection API reads it; runtime casts check against it.

```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
let t = runtime_type_of(p)                 // every value (or box) carries a class pointer
for f in t.fields { print(f.name) }        // runtime metadata table behind it
if p is_runtime Point { ... }              // checked against the table, at runtime
```

- Honest cost analysis under the perf charter: metadata words on boxed objects (memory tax on every allocation), pressure toward uniform boxed layouts for anything reflection must see (directly contradicting research/03 exec item 6's unboxed typed registers and unboxed tag+payload ADTs), reflection-driven paths as slow paths (Go's `reflect.DeepEqual` economics — research/05 section 5.1), and errors moving from compile time to runtime (charter: compile-time errors over runtime errors). Java's erased-generics-plus-reflection combination is the cautionary pairing (research/05 sections 1.7, 6.2).
- What it buys: runtime-checked casts with rich errors, late-bound plugin loading, serializer frameworks without codegen. DreamScript's substitutes: evidence-carrying boxes for dynamic checks, comptime serializers (G1.b/G2.b), first-class modules for plugins (B6 fragment).
- Forecloses: the unboxed-slot representation as stated (research/03 exec 6); a metadata-free frame layout; the 3-10x-of-C interpreter baseline's premise.
- Prior art: research/05 sections 1.7 (C# reified vs Java erased), 5.1 (Go reflection row), 6.2.

### G2.d — `tags-as-data`: tags only; everything else is comptime

Under docs/en/design/01's B1c hybrid, unions are ordinary data: a value's runtime tag IS its type identity, nominal tags compose set-theoretically, and `is` tests plus `match` cover every runtime type-level question. No type objects, no metadata tables, no reflection at runtime — anything else is comptime (G2.b).

```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
let s: Shape = Circle(r: 1.0)              // a union value: an ordinary tag + payload
if s is Circle { ... }                     // the tag test IS the type test — no type object
match s { Circle(r) => ..., Square(w) => ... }   // exhaustive over the set algebra
// everything deeper (fields of Point, case lists) is comptime: G2.b's reflection
```

- This follows from docs/en/design/01 rather than being a new decision: once B1c makes tags the ordinary data representation, "what type is this value at runtime" is exactly "which tag does it carry", which the data representation answers for free. Adding a type-object layer on top would duplicate the tag system with a second, slower one.
- Cost: none — it is the absence of a feature. Forecloses: user-facing type objects and runtime introspection APIs, same as G2.b, plus nothing else.
- Prior art: research/05 sections 2.1 (set-theoretic membership as the type test), 6.3 (tags as IC keys); docs/en/design/01 B1c.

### G2 comparison

| Option | Runtime representation | Perf cost | Errors | Coverage | Forecloses |
|---|---|---|---|---|---|
| G2.a types-as-values | type words / type objects | tagged slots or boxed-only | runtime dispatch errors possible | full runtime + comptime | unboxed slots (exec 6) |
| G2.b comptime-reflection | none | zero | compile time | all compile-time needs | runtime introspection |
| G2.c metadata | per-object class pointer | memory + boxing pressure + slow paths | runtime | runtime + comptime | unboxed slots, perf charter |
| G2.d tags-as-data | tags only (already there) | zero | compile time (match exhaustive) | runtime tag tests only | type objects |

### G2 interactions

- With B6: evidence words for existentials are the runtime face of polymorphism, keyed by tags (research/05 section 6.3); G2.a would duplicate that with a user-visible type layer, G2.b/d keep one mechanism.
- With H3: inline caches key on tags/type ids; G2.a's type values would be a second key space for the same information.
- With G1: G2.b is the engine G1.b's comptime code reads; G2.a/c do not feed derives any better than compiler-internal reflection already does.
- Recommendation: adopt G2(b) — comptime-only reflection — whose runtime half is G2(d); (d) follows from docs/en/design/01's B1c hybrid set types at zero cost, so the combined recommendation reads "comptime-only reflection over tags-as-data": no runtime metadata exists, tags answer runtime questions, reflection answers compile-time questions.

## H3 — Generics execution

Axis (b) of the decomposition: B6 decided how instances are found; H3 decides how the calls execute in the typed register VM. All options assume the research/03 baseline (unboxed typed registers, no guards); the differences are where instantiation, evidence, and caching live.

### H3.a — `evidence-ic`: one generic body + evidence words + inline caches; lazy JIT specialization

Every generic function is represented exactly once in bytecode, with its constraint list attached. At a generic call site, emit `CALL_GENERIC fn, evidence_words`; each evidence word is either a statically-resolved instance reference (receiver type known — a constant operand) or a runtime-resolved slot keyed by the runtime tag. The interpreter patches a 1-2 entry inline cache per call site after first execution — Self/V8-style polymorphic inline caches (Ungar, Chambers, Patterson, ECOOP 1991) — and a cache miss falls back to full lookup, so correctness never depends on cache state (research/03 section 2 item 1 states the same for dyn sites). The JIT tier lazily materializes per-type specializations keyed by the concrete type vector, cached in the function's instantiation table and dropped under memory pressure — Julia-style on-demand devirtualization (research/05 sections 6.1-6.3; research/03 section 8 position: dictionaries as unboxed values, per-representation code for numeric-polymorphic operations).

```
-- generic function, stored once (illustrative IR, not surface syntax)
fn map[T: Eq, U](f: fn(T) -> U, xs: List[T]) -> List[U]
  ; constraint words: [Eq_T_slot]                 -- resolved per call site

CALL_GENERIC map, [static_ref: eq_instance_int]   ; statically known T: constant operand, no lookup
CALL_GENERIC map, [dynamic: tag_of(x)]            ; dynamic T (set-typed value): IC keyed by tag
IC_PATCH map@42: { tag_list -> callee, expected_shape }  ; patched in place after first execution
```

- Does this satisfy the anti-specialization stance? Argue both sides. Yes-side: which code is semantically selected is fixed before any caching — the unique disjoint instance of B6.a — and the cache is a pure perf hint with no correctness role; a stale or cold cache falls back to lookup and cannot change behavior; the precedent is .NET's per-value-type stenciling, which is invisible, versus C++ partial specialization, which is a visible source-level priority rule (research/05 sections 1.7, 6.4). No-side: the runtime does maintain per-type compiled variants — a runtime cache of monomorphizations — and first-execution timing differs from steady state; a debugger will show which variant ran; and if a future feature made specializations observable (introspection over instantiations, per-instantiation user code), the invariant would erode from the tooling side even though the semantics never changed. Ruling: compatible, under the stated invariant "dispatch is determined by types and the set algebra, never by declaration order or priority; the runtime may cache and specialize any dispatch that is already unique" (research/05 section 6.4) — the same distinction research/03 section 8 draws between compiler inlining of generic calls (invisible, no overlap rules needed) and language-level specialization (rejected).
- Why the interpreter tier stays fast: at most call sites the type is static, so evidence is a constant operand and the "cache" is empty because there is nothing to look up; the IC only matters on set-typed/dynamic paths, and there the disjoint-head guarantee means the IC never needs priority tie-breaking (research/05 section 6.3). Julia's 10x spreads came from inference failure, which DreamScript's static types delete (arXiv 1808.03370; research/05 section 6.2).
- Tradeoffs. Learning: zero — no user-visible concept. Perf: indirect call through evidence until the JIT inlines/folds; IC hit costs one compare on dynamic paths. Sugar: none. Compiler: evidence construction, IC patching, instantiation table in the JIT tier.
- Forecloses: nothing at the language level — which is the point. It does commit the VM to an evidence-word calling convention (generic calls carry extra register arguments).
- Prior art: research/05 sections 6.1-6.4; research/03 sections 2 (dyn IC), 8, Table 6 (row "compiler inlining of generic calls; no language-level specialization — best: mono perf, dictionary semantics, no overlap rules"); ECOOP 1991 PICs; arXiv 1808.03370.

### H3.b — `mono-cache`: bytecode-level monomorphization cache

Each instantiation of a generic function gets its own compiled bytecode body, materialized on first execution of that instantiation and cached in a per-function instantiation table — the interpreter-tier version of what H3.a reserves for the JIT tier.

- Gains: best tier-1 performance of the sharing options — direct calls, evidence folded away at materialization, representation-specific opcodes throughout the body (research/03 section 1's typed-opcode discipline applied per instantiation).
- Costs: a materialization pause per new instantiation at first call (startup-adjacent latency; Julia does this at the JIT tier precisely so the interpreter tier stays uniform — research/05 section 6.1); bytecode size grows as O(instantiations) and is unbounded across dynamically loaded modules; an eviction policy (drop cold instantiations) becomes VM-visible behavior.
- Forecloses: the one-body size economy; a stable, instantiation-independent bytecode artifact for mmap caching (research/03 section 1's dump story complicates when bodies multiply); it makes instantiation policy semantically adjacent (memory and timing depend on the table).
- Prior art: research/05 sections 6.1 (monomorphization pros/cons), 6.2 (Julia on-demand specialization); .NET CLR per-value-type instantiation (Matt Warren retrospective, via research/05 section 1.7).

### H3.c — `erasure`: full erasure with boxed values (Java)

One body per generic function; all type parameters erased; every generic value boxed to a uniform representation; dispatch through runtime lookups.

- Direct conflict with the runtime baseline: research/03 exec item 6 commits to typed registers holding unboxed i32/i64/f64 and ADTs as unboxed tag+payload pairs; erasure re-imports the boxing penalty for every primitive flowing through generic code, makes megamorphic dispatch a standing cost center (research/05 section 6.2, Java row), and blocks escape analysis and inlining when the callee is statically unknown (research/03 section 6.2) — the exact machinery the charter's perf requirement says to build and the static type system says is unnecessary.
- What it buys: the simplest possible VM (one body, uniform slots, no evidence ABI) and Java familiarity. The charter's ultra-high-performance requirement rejects the trade.
- Forecloses: the unboxed-slot representation; unboxed generic numerics; guaranteed-TCO-friendly generic calls (evidence-less dispatch is fine for TCO, but boxing is not).
- Prior art: research/05 sections 1.7, 6.2; research/03 exec items 3/6, section 8.

### H3.d — `comptime-mono`: comptime monomorphization only (Zig)

Every instantiation exists before execution; there is no runtime polymorphism at all and no evidence words ever exist.

- Interaction with the typed register VM: the cleanest bytecode of all options — registers unboxed, opcodes fully representation-specific, no evidence ABI, no caches (research/03 section 1 applied without exception). But the VM never sees generic code, so load/first-use pays for every instantiation whether hot or cold: startup cost is the whole instantiation fan-out, bytecode size is the worst of all options, and there is no cold-instantiation pruning because there is no lazy tier to prune from.
- In an interpreter, this is H3.b with a strictly worse policy: eager instead of lazy materialization, no sharing, no eviction. It is the natural execution story for B6.d and a poor standalone choice otherwise (research/05 section 9 combo D: "compile-time visible at runtime" is the listed risk).
- Forecloses: evidence-polymorphic dynamic paths (set-typed values carrying runtime-resolved evidence), the lazy JIT-tier design of H3.a, and startup-latency control on instantiation-heavy programs.
- Prior art: research/05 sections 4.1 level 3, 6.1, 9 combo D; Zig compilation model.

### H3 tradeoff table (startup / bytecode size / devirtualization latency)

| Option | Startup cost | Bytecode size | Devirtualization latency | Tier-1 steady state | JIT ceiling | Semantically visible? |
|---|---|---|---|---|---|---|
| H3.a evidence + ICs | none | minimal (one body per generic fn) | first hot JIT compile per hot instantiation (lazy) | evidence indirection; IC hit ~ direct | high (fold evidence, inline) | no — cache has no correctness role |
| H3.b mono cache | pause per new instantiation at first call | O(instantiations) | zero at tier 1 | best of the sharing options | high | materialization timing/memory visible |
| H3.c erasure | none | minimal | never — permanent indirection | worst: boxing + megamorphic dispatch | capped without speculation machinery | no |
| H3.d comptime mono | all instantiations at load/first-use (eager) | worst (full fan-out) | zero | best (no evidence at all) | highest | instantiation is the semantics |

### H3 interactions

- With B6: B6.a/b produce evidence words that H3.a consumes (and H3.b folds); B6.d produces fully monomorphic code, for which H3.d is the only coherent story. B6.a + H3.b is also coherent (per-instantiation bodies of unique instances) but pays startup for an economy one body already provides.
- With docs/en/design/03 (E1d): evidence-word dispatch is the same path handler/effect dispatch would reuse; H3.a builds that substrate once (research/03 section 8 position 3; docs/en/design/03 B5d row).
- With guaranteed TCO: evidence-passing generic calls in tail position tail-call normally (the evidence words are ordinary register arguments; research/03 section 4's frame-reuse TCO is unaffected).
- Recommendation: adopt H3(a) — one generic body plus evidence words plus polymorphic inline caches in tier 1, with the JIT tier's lazy per-type specialization table (which is H3.b's table, lazily filled, at the tier where it pays). This is research/03 section 8's stated position and Table 6's best-rated row.

## Variance and HKT

- Variance by set inclusion: for immutable containers, `Dog <: Animal` implies `List[Dog] <: List[Animal]` as sets of values — covariance falls out of semantic subtyping with no annotations, and linearity does not break it (coercing `Box[Dog]`, one affine dog, to `Box[Animal]` is still one value, provided the coercion cannot copy or drop contents, which immutable data guarantees — research/05 section 7.2).
- Invariance exactly where interior mutation or unique-borrow escape exists: read roles are covariant, write roles contravariant, and a field with both roles is invariant (the Rust `&mut T`/`Cell[T]` rule, derived here from the set algebra instead of memorized — research/05 section 7.2). Users meet variance vocabulary only when writing uniquely-mutable containers.
- HKT deferral: Kotlin shipped and thrives without HKT (KEEP-87 stalled since 2017; Arrow abandoned the simulated encoding); Swift went a decade without HKT, filling the gap with opaque types plus primary associated types (SE-0358); Rust's partial HKT (GATs) is documented by its own initiative as "too hard to learn" (research/05 section 8.2). The classic drivers — abstracting `map`/`>>=` over container shapes — vanish under DreamScript's declared style: `option.map`, `result.and_then`, `list.map` as concrete methods, effects via capabilities not monad towers (research/05 section 8.2).
- What future effect rows would need: if docs/en/design/03's E1d ever graduates to surface effect rows (the "rows-later" half of B5d), effect-polymorphic signatures will need row polymorphism or explicit capability parameterization — and the evidence-dispatch substrate H3(a) builds is the same path those rows would reuse (research/03 section 8; docs/en/design/03 B5d interactions row). That is the one place HKT-shaped machinery could return; defer until E1d graduates, and revisit then.
- Decision: v1 ships type parameters with set-theoretic bounds, associated types, and set-inclusion variance; no HKT, no parameterized associated types (GATs) — the Hylo borrowing-convention design covers lending-iterator-shaped needs without them (research/05 section 8.1).

## Linear interplay (by reference to docs/en/design/02; not re-decided here)

- Generics over noncopyable values: the `~Copyable`-style negated constraint declares a parameter that may be noncopyable, so one generic body serves copyable and affine `T` alike — `fn push[T: ~Copyable](s: &mut Stack[T], x: T)` — Hylo's model (research/05 section 7.1). Ownership flavor is C1(b) affine-core-plus-opt-in-linear (docs/en/design/02-linearity-ownership.md); this file does not re-decide it.
- Parameter conventions carry linearity, not types: borrow/consume/mutate parameter modes (C2(d) second-class parameter modes, docs/en/design/02-linearity-ownership.md; Hylo `let/set/inout/sink`) let a generic signature say how it uses each argument without introducing address types into the value language (research/05 section 7.1).
- Consequences this file inherits: `==` takes two borrows and never consumes its operands, so comparing affine values leaves them affine and derived equality on owned affine data only reads (research/05 section 5.2 break 4); container operations over affine values need borrows or a consume-and-return convention, with Perceus-style reuse turning `cons(h, tail)` into an in-place node overwrite when `tail` is about to die (research/05 section 7.1; research/03 exec item 8).
- What B6/H3 add on top: instance heads over affine types are ordinary sets, so the disjointness checker is purely type-level and unaffected by linearity; evidence words may themselves be affine (a single-use capability is just an affine value passed as evidence); the evidence calling convention of H3(a) passes evidence in registers like any other argument, so TCO and frame reuse are unchanged.

## Derives (owned by this file per docs/en/design/01 B9)

- Under docs/en/design/01 B9(a) universal structural `==` (with research/05 section 5.2's five carve-outs: functions rejected statically; bit-structural float semantics with a named IEEE comparison; mutable/reference cells compared by identity; `==` takes borrows; hash co-required), equality is NOT derived — it is universal. `Eq` therefore appears in the built-in derive list only if the author later re-picks B9; the MoonBit `builtin/tuple_eq.mbt` per-arity impl boilerplate that trait-mediated equality generates is deleted entirely by the structural choice (research/05 sections 5.1-5.3).
- Built-in derives (compiler synthesis, one attribute each, no macro system, good errors because the compiler does it): `Hash` (canonical-bits hashing, consistent with structural equality), `Show`, `Ord` (field-order lexicographic, following Rust/Kotlin/Swift; a `total_compare` float variant must be supplied explicitly for sort keys, per the MoonBit precedent — research/05 section 5.3), `Clone` (deep copy — meaningful precisely because copy is explicit under affine ownership), `ToJson`/`FromJson` (serde), `Default`.
- How user derives are expressed under the recommended G1(b): comptime functions over static reflection enumerate fields/cases and assemble members — the Zig `@typeInfo` + `inline for` recipe that research/05 section 4.3 verifies sufficient for arbitrary-struct JSON. The built-in `#derive` attributes exist so the hot cases are one word long and compile fast (need (4) of the minimal set).
- MoonBit evidence (local read-only tree `C:/programs/me/DreamScript/moonbit-core/`): `#derive(Eq, Hash, Debug, ToJson)` used pervasively across packages (e.g. `path/path.mbt`); default method bodies plus the NaN/IEEE-equality caveats documented in `builtin/traits.mbt`; the hand-written per-arity tuple impls in `builtin/tuple_eq.mbt` demonstrating what structural equality deletes (research/05 sections 5.1, 5.3).
- Forecloses: derive proc macros (G1.d), reflection-driven runtime serializers (G2.c), and out-of-band codegen (G1.a's escape hatch).

## Coherent combos

A combo picks one option per decision point. The three below span the credible space; research/05 section 9's combos A-E map onto these three plus the rejected dynamic-dispatch direction (E).

### Combo 1 — Coherent-traits: B6a + G1b + G2b + H3a

```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
// 1. polymorphic container API over affine elements (B6.a + ~Copyable bound)
type Stack[T] = { items: List[T] }
impl[T: ~Copyable] Stack[T] {
    fn push(&mut self, x: T) { self.items = cons(x, self.items) }  // consume-and-return
    fn pop(&mut self) -> Option[T] { ... }
}

// 2. trait with disjoint instance heads (B6.a)
trait Json { fn to_json(&self) -> String }
impl Json for Int  { fn to_json(&self) -> String { ... } }
impl Json for Bool { fn to_json(&self) -> String { ... } }
impl[T: Json] Json for List[T] {                     // conditional head: an intersection arrow
    fn to_json(&self) -> String { "[" + self.map(|x| x.to_json()).join(",") + "]" }
}
// impl Json for Int | Bool { ... }                  // REJECTED: head intersects both above

// 3. derive + comptime reflection (G1.b, G2.b)
@derive(Hash, Show, ToJson)                          // one word, no macro system
type Point = { x: Int, y: Int }
fn schema(comptime T: type) -> String {              // user derive-like via reflection
    var s = "{"
    for f in fields(T) { s += f.name + ": " + type_name(f.type) + ", " }
    return s + "}"
}

// 4. execution (H3.a): one body per generic fn + evidence words
let p  = max(p1, p2)            // evidence: static ref to impl Ord[Point] — constant operand
let js = p.to_json()            // dynamic site: IC keyed by tag -> the unique impl
```

- Verdict: the recommended baseline. The only combo satisfying all four hard constraints simultaneously — set-theoretic types, no overlap, linear-friendly, bytecode-native (research/05 section 9 scoring table) — and every component has independent production evidence: MoonBit derives, Swift synthesis, Rust evidence-passing `dyn`, Julia IC-style caching and lazy specialization, Roc structural equality, Scala scoped resolution as the escape hatch.
- Implications for docs/en/design/01: B1c hybrid is load-bearing — instance heads are tag sets and disjointness is checked in the set algebra; B9a structural `==` deletes Eq-derivation and the tuple-arity boilerplate. For docs/en/design/02: `~Copyable` bounds plus C2(d) parameter conventions carry affine generics; evidence words are ordinary borrowable values. For docs/en/design/03: E1d's handler runtime and B6.a's evidence dispatch share one substrate; B5d error unions lower through the same tag machinery. For docs/en/design/04: surface needs trait/impl/derive/comptime tokens and no macro syntax; the disjointness error wording ("these heads intersect at X; refine with a negation or merge") is a UX decision 04 should budget for.

### Combo 2 — Explicit-evidence: B6b + G1b + G2b + H3a

```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
// 1. polymorphic container API — evidence is an explicit parameter
type Eq[T]   = { fn equal(&T, &T) -> Bool }
fn eq_int(): Eq[Int] = { fn equal(a, b) { a == b } }
fn eq_pair[T, U](e1: Eq[T], e2: Eq[U]): Eq[(T, U)] =
    { fn equal((a1, b1), (a2, b2)) { e1.equal(a1, a2) && e2.equal(b1, b2) } }

fn contains[T](xs: &List[T], x: &T, eq: Eq[T]) -> Bool {   // capability as parameter
    for y in xs { if eq.equal(y, x) { return true } }
    false
}
contains(xs, &needle, eq_pair(eq_int(), eq_int()))          // fully explicit
contains(xs, &needle, _)                                    // sugar: unique-instance insert

// 2. JSON serializer with derive + reflection (G1.b, G2.b)
@derive(ToJson) type Point = { x: Int, y: Int }
fn json_fields(comptime T: type) -> Int { var n = 0; for f in fields(T) { n += 1 }; n }

// 3. "disjoint heads" become coexisting evidence values — no global rule needed
let fast_eq = eq_int()
let logged_eq = { fn equal(a, b) { log("eq"); a == b } }    // two Eq[Int] alive at once
contains(xs, &needle, logged_eq)                            // caller picks, visibly
let j = any_json(box Json(p))                               // existential: value + evidence
```

- Verdict: coherent and the most "no-magic" (research/05 section 3.4 answers yes, with the boilerplate caveat); runtime representation identical to Combo 1, so perf is identical; the cost is API noise — every polymorphic function grows an evidence parameter — and the `_` sugar quietly re-imports scoped resolution, at which point B6.a's disjoint-head search is the more principled version of the same convenience. Choose this only if zero implicit search is a terminal value for the author.
- Implications for docs/en/design/01: none — fully compatible. For docs/en/design/02: the best linear fit of the three (capabilities are ordinary affine values; affine flow sees every use). For docs/en/design/03: E1c's capability idiom generalizes to all polymorphism, pre-validating E1d's runtime substrate. For docs/en/design/04: no resolution-related syntax, but call sites carry evidence arguments, which creates surface pressure toward the `_` sugar — an explicit 04 decision with a stated rule (ambiguity-as-error always).

### Combo 3 — Comptime: B6d + G1b + G2b + H3d

```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
// 1. polymorphic container API — generics are comptime functions
fn Stack(comptime T: type) type {
    return struct {
        items: [T], len: Int,
        fn push(&mut self, x: T) { self.items[self.len] = x; self.len += 1 }
        fn pop(&mut self) -> Option[T] { ... }
    }
}

// 2. JSON serializer — derive via type reflection (G1.b, G2.b)
fn Jsonify(comptime T: type) type {
    return struct {
        fn to_json(self: &T) -> String {
            var s = "{"
            inline for f in fields(T) { s += "\"" + f.name + "\":" + render(self, f) + "," }
            return s + "}"
        }
    }
}

// 3. "trait impl with disjoint heads" has no analogue — bodies are generated per type
impl Json for Int { ... }            // i.e., Jsonify(Int) is materialized at comptime
let s: Stack(Point) = Stack(Point)() // each instantiation is its own bytecode body
let js = Jsonify(Point).to_json(&p)  // zero runtime polymorphism; all cost at load
```

- Verdict: best steady-state performance and the most elegant derives, worst overall fit — comptime-generated instances escape the set algebra so disjointness is unenforceable (research/05 section 9 combo D); the linear checker must run on generated code, which adds a real checker phase (Zig does not do this); the comptime/runtime boundary is a second type system to learn and debug; an interpreter pays materialization at startup; and semantically it is template instantiation, a mixed fit with the anti-specialization stance. Adopt its reflection half (G1.b/G2.b), not its generics half — which is exactly what Combo 1 does.
- Implications for docs/en/design/01: the set algebra demotes from resolution substrate to optional checking tool that comptime can bypass, weakening B1c's guarantee. For docs/en/design/02: linearity must be checked post-expansion — a new checker phase and a real cost. For docs/en/design/03: effects under comptime need explicit threading or lose the E1d substrate. For docs/en/design/04: comptime becomes the language's center of gravity and spends the sugar budget on a second execution phase rather than surface ergonomics.

### Cross-combo reading

Combos 1 and 2 share the entire runtime — evidence words, inline caches, comptime reflection, built-in derives — and differ only in resolution policy, so Combo 1 can adopt Combo 2's named scoped instances later without re-plumbing the VM. Combo 3 shares G1b/G2b with them but diverges at the VM (no evidence ABI at all), so choosing it after tier 1 is built around evidence words would be a rewrite. This is why B6 and H3 should be decided together, as one motion.

## Open questions this file leaves to the author

- The exact disjointness-checking algorithm for instance heads and the wording of its witness-naming errors (research/05 section 9, open question 1).
- The termination rule for recursive conditional instances (Modus Ponens calculus provides the shape; the concrete check is open — research/05 sections 2.3, 9).
- Whether scoped named instances (B6.b-lite) ship in v1 or are deferred (research/05 section 9, open question 3).
- The float-equality choice under `==` — Roc's structural/IEEE split versus MoonBit's IEEE-with-documented-preconditions — owned by docs/en/design/01 B9, inputs in research/05 section 5.2.
- Whether comptime reflection ships in v1 or built-in derives alone suffice until demand appears (research/05 section 9, open question 5).
