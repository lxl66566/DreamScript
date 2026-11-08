# 01 — Types: Decision Catalog

Status: Phase 2 option catalog, written 2026-11-04, awaiting author decisions per AGENTS.md. Inputs: charter (AGENTS.md: set-theoretic taste, monadic extraction, explainability budget), research notes 01 (linearity), 03 (runtime/IR), 04 (syntax/ergonomics), 05 (generics/metaprogramming), and the historical README.md (set instructions). This file covers seven decision points: B1 type foundation, B2 inference, B3 products/sums, B4 nullability, B7 numeric tower, B8 strings, B9 equality/ordering/hash. Error handling (B5) is owned by docs/en/design/03-effects-errors-async.md; linearity/ownership is owned by docs/en/design/02 (being written concurrently); derive mechanisms are owned by docs/en/design/05-generics-metaprogramming.md; surface tokens are owned by docs/en/design/04-syntax.md. All code in this file is illustrative neutral TS-family syntax; surface syntax is decided in docs/en/design/04-syntax.md.

Reading guide: each decision point presents 3-4 named options. Per option: a concrete syntax example, prior art with an evidence pointer (research file + section, or URL), a tradeoff line, and what the option forecloses. Each point ends with a comparison table (inference burden / VM representation / expressiveness / compiler complexity / forecloses), an interactions note (B5 errors and docs/en/design/02 linearity), and a one-line Recommendation. The runtime baseline assumed by every "VM representation" row is defined in "Shared assumptions" below and comes from research/03.

## Decision-point index

| ID | Decision point | Options | Recommendation (one line) |
|---|---|---|---|
| B1 | Type foundation | (a) setcore: structural unions/intersections/negations, (b) nomcore: nominal ADTs, (c) hybrid: nominal tags + set algebra, (d) setdecl: README set-instructions + `any` | (c) hybrid: nominal tags composing set-theoretically; keeps the README's set surface with tag-identity exhaustiveness and (b)-simple runtime |
| B2 | Type inference | (a) bidirectional + boundary annotations, (b) full HM no annotations, (c) HM/flow + refinement, (d) annotations everywhere | (c)'s flow refinement layered on (a)'s bidirectional skeleton; full-algebra inference (b) is still a research bet |
| B3 | Products and sums | (a) anonymous structural records/variants, (b) nominal structs/enums, (c) extensible rows, (d) tuples + records hybrid | (d): nominal structs/enums + anonymous tuples; anonymous record literals as compiler-synthesized sugar |
| B4 | Nullability | (a) no null, `T \| none` unions, (b) language null + strict flow typing, (c) `T?` sugar desugaring to (a) | (c): exactly (a)'s union semantics with a zero-semantic `T?` alias; chain tokens deferred to docs/en/design/04-syntax.md |
| B7 | Numeric tower | (a) Int/Float defaults, trapping overflow, (b) arbitrary-precision default, (c) full machine tower, (d) (a) + deferred decimal | (a), with (d)'s decimal as a later library type; trapping is the only predictable interpreter policy |
| B8 | Strings | (a) immutable UTF-8 bytes + index discipline, (b) interned strings + builder, (c) UTF-16/platform strings, (d) (a) + rope | (a) with zero-copy checked slices; ropes stay a library type; FFI conversion at the boundary |
| B9 | Equality/ordering/hash | (a) universal structural `==`, (b) trait-mediated equality, (c) structural builtin + derives, (d) reference identity | (a) with the five research/05 §5.2 carve-outs; Ord/Hash/Show derive owned by docs/en/design/05-generics-metaprogramming.md |

## Shared assumptions (binding on every option)

Immutable by default (charter hard requirement 5); sugar ceiling is Kotlin's total surface, about 38 features / cost-weighted 74-76 units, with a recommended budget of 20 or fewer (research/04 §2); no C++-style specialization, because a language whose generic instances cannot overlap never needs priority rules (research/05 §3); interpreted execution on a typed register VM (charter hard requirement 1); compile-time errors over runtime errors (charter); set-theoretic typing is an author taste, not a settled conclusion — this file presents structural, nominal, and hybrid cores with equal rigor.

Runtime baseline from research/03: typed register bytecode VM; every slot's type is a compile-time constant, so slots hold unboxed i64/f64/bool/bytes and raw GC references with no NaN-boxing and no per-slot tags (research/03, "Value representation" and the LuaJIT-vs-typed-encoding comparison); ADTs, unions, and Result-like values are unboxed tag-plus-payload pairs spread across consecutive frame slots, zero heap boxing in the common case (research/03, "Unboxed ADT/enum/Result layout"); heap values carry a fixed 8-byte header `{ u32 type_id; u32 rc }`, which doubles as the key for inline caches and dynamic membership tests (research/03; research/05 §6.3); frame layout is fully static per function, which is what makes TCO a pointer shuffle and the copy-and-patch JIT guard-free (research/03, frames and copy-and-patch sections). Every option below is scored against this baseline: an option that forces boxing or per-slot tags is paying the 2-10x boxing overhead term that research/03 §b.10 says static typing exists to delete.

## B1 — Type foundation

What is decided here: what the atoms of the type language are and whether union, intersection, and difference are type-level operators over them. This single choice fixes what B5 can say about errors, what docs/en/design/02 can say about `~Copyable` composition, how much of the README's set-instruction vision survives, and the shape of the exhaustiveness checker. The charter states set-theoretic taste as an author preference; the honest question is not "sets or not" but "how much set algebra can the checker and the VM afford".

### B1.a — `setcore`: structural set types as the core

All types are structural; union `|`, intersection `&`, negation `~` are first-class everywhere; two structurally identical types are the same type; nominal names are aliases for structural sets.

```ts
// B1.a -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
type Str      = Int | String              // union: value is a member, unwrapped, no wrapper
type Both     = Drawable & Resizable      // intersection: satisfies both structural shapes
type Plain    = User - Admin              // difference: user-shaped values that are not admins
type F        = (Int -> Str) & (Bool -> Int)  // overloaded function as intersection of arrows (research/05 §2.2)
fn area(s: Circle | Rect) -> Float {     // parameter is a set; coverage is set arithmetic
  match s { c: Circle => 3.14159 * c.r * c.r, r: Rect => r.w * r.h }
}
const j: Json = { port: 8080 }           // records are anonymous; same shape = same type
```

Prior art: MLscript/MLstruct make union, intersection, and negation first-class with claimed principal type inference over a Boolean algebra of structural types, classes treated transparently as their instance sets (research/04 §3; research/05 §2.3; https://github.com/hkust-taco/mlscript; MLstruct OOPSLA 2022, `https://lptk.github.io/files/[v6.2]%20mlstruct.pdf`); the semantic-subtyping line (CDuce, Castagna POPL 2015, the Elixir type system) ships set-based subtyping and overload resolution in production (research/05 §2.1-2.2); Whiley ships structural types with union/intersection/negation operators in its spec (research/04 §3; https://whiley.org/pdfs/WhileyLanguageSpec.pdf).

Tradeoffs: inference burden is the highest of the four — MLscript's principal-type claim covers it, but no production compiler ships that algorithm yet, and negation in inferred positions forces complement reasoning; expressiveness is maximal: error-set arithmetic `(T | E1 | E2) - E1` is native, exhaustive match is set cover, and overloading is arrow intersection with a decidable disjointness check (research/05 §2.2); compiler complexity is the checker: semantic subtyping, emptiness decidability, and precise "missing members are a set computation" diagnostics (research/04 §4) — real but bounded machinery. Forecloses: nominal encapsulation (private fields as identity, type invariants defended by constructor privacy), abstract types that hide representation, and any Go-style structural-interface story becomes redundant.

### B1.b — `nomcore`: nominal ADTs as the core

Types are named; enums are closed sums; structs are named products; there are no type-level union/intersection operators, only type aliases and generics.

```rust
// B1.b -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
enum Shape { Circle(f64), Rect { w: f64, h: f64 } }   // closed sum, nominal identity
struct User  { name: String, age: Int }               // nominal product
enum ReadError { Timeout, NotFound }                  // errors are enum cases
fn load(p: Path) -> Result[Config, ReadError] { ... } // success/error channel is a wrapper type
fn area(s: Shape) -> f64 {                            // exhaustiveness = cover the 2 enum cases
  match s { Circle(r) => 3.14159 * r * r, Rect { w, h } => w * h }
}
```

Prior art: Rust enums/structs with exhaustiveness-checked match (research/04 §2.4, §12 Style D), Gleam's deliberately small nominal core with no guards and no unions (research/04 §2.3: 9 features / cost 15, the leanest surveyed). Tradeoffs: inference burden is the lowest — no subtyping between distinct types means HM-style inference plus generics covers everything; VM representation is the cheapest — every slot single-shape except enums, which are the unboxed tag+payload layout research/03 already specifies; expressiveness is the lowest — `(T | E1 | E2) - E1` is inexpressible, error-set growth breaks every match on the error enum, and "growing the error set breaks only failing matches, never `or return` chains" (research/04 §3) is lost; compiler complexity is the minimum, which is (b)'s real advantage and the reason this option must be taken seriously. Forecloses: union types, cross-type flow narrowing beyond enum matching, the Phi one-mechanism null/error/enum story (research/04 §3), and most of the sugar savings research/04 §2.5 counts on (Kotlin features 4, 5, 6, 7, 8, 27 collapse only if unions exist).

### B1.c — `hybrid`: nominal tags composing set-theoretically

Declarations are nominal (tags/structs have identity), but the type language above them is set-theoretic: unions, intersections, and differences of nominal atoms are ordinary type expressions; a class/tag is its set of instances.

```ts
// B1.c -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
tag Timeout; tag NotFound                                  // nominal atoms: distinct identity
struct BadPort { got: Int }                                // nominal, carries payload
type ReadError = Timeout | NotFound | BadPort              // set expression over nominal atoms
struct Config { host: String, port: Int }
fn load(p: Path) -> Config | ReadError { ... }             // union-carried errors, unwrapped
const ok: Config - ReadError = load(p).err((e) => default())  // difference shrinks the handler result
trait Animal; struct Dog: Animal; struct Cat: Animal       // Dog | Cat matches by tag identity
const a: Animal = Dog{}                                    // nominal subtyping by declaration
match a { d: Dog => ..., c: Cat => ... }                   // exhaustive via tag identity
```

Prior art: MLstruct is exactly this model — nominal-looking class declarations whose types compose in a Boolean algebra, with principal inference claimed (research/04 §3; research/05 §2.3); TypeScript's discriminated unions are the degraded production version (nominal tags, unions, narrowing, no intersections of classes, research/04 §3); exhaustive match via tag identity is how every surveyed exhaustiveness checker actually works (research/04 §4). Tradeoffs: inference burden is medium — atoms are nominal so most inference is ordinary unification; the set algebra mostly checks rather than infers, which is what keeps MLstruct-style principal types plausible (research/05 §2.3); VM representation stays (b)-simple — a union of nominal atoms is the unboxed tag+payload layout, an intersection keeps the most precise member's layout with `type_id` in the heap header for membership ICs, and difference types erase entirely (compile-time only); expressiveness is (a)'s where it pays (error-set arithmetic, narrowing, exhaustive unions) and drops full structural power (anonymous record intersections) that nobody surveyed uses well; compiler complexity is the disjointness/emptiness checker over nominal atoms, which is strictly easier than (a)'s structural emptiness. Forecloses: two independently-written same-shape structural types unifying (the TS objects-are-equal property), and `type A = { x: Int } & { y: Int }`-style purely anonymous set definitions unless anonymous records are admitted as sugar (B3 recommendation does exactly that).

### B1.d — `setdecl`: the README's set instructions as declarations, plus `any`

The historical README (README.md, "Typing") proposes `type A = B | C`, `type A = B & C`, `type A = B - C`, `type A = (B & C) - D` as primitive declaration forms, a universal type `any` that every variable inhabits, and the rule "if the compiler cannot infer the type of a variable, it is treated as `any`". Evaluated honestly, as instructed:

```
// B1.d -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
type A = B | C;        // README verbatim: set instructions as declarations
type A = B & C;
type A = B - C;        // A is B but not C
type A = (B & C) - D;  // arbitrary set instructions
any                    // every variable is any; uninferred => any (README)
```

Prior art: Whiley's spec documents union/intersection/negation as type operators (research/04 §3), TypeScript reaches difference only through the `Exclude<T,U>` conditional-type idiom (research/04 §2.4, difference/negation row), and Go's untyped constants are the one principled "literal is a set of types" design (research/04 §8, costed S3). Tradeoffs: the set-instruction surface itself is sound and is exactly what (a) and (c) expose as expressions rather than a parallel declaration system — declaring `type A = ...` as an alias is what all three other options do; the two parts that do not survive scrutiny are (1) the silent fallback "cannot infer => any", which converts every inference failure into dynamic typing, directly violating the charter's compile-time-errors rule and producing exactly the opaque diagnostics Erlang's annotation-less success typing is cited as a warning for (research/04 §3), and (2) `B - C` where the subtracted set is not a finite union of tags: exhaustiveness checking and match compilation then need semantic-subtyping emptiness over arbitrary structural operands (decidable, research/05 §2.1, but real machinery), narrowing must track predicate-level facts rather than tag identity (Whiley's retyping rules, research/04 §3), and the runtime value must keep its concrete representation while the checker proves it outside the subtracted set — the difference type itself erases, but every match against a non-enumerable difference compiles to a predicate chain instead of a tag switch; on a typed register VM this is the one place set algebra stops being free. Forecloses: nothing by itself — it is (a) or (c) wearing the README's syntax — but its `any`-fallback forecloses the guarantee that frame slot layouts are stable across unrelated edits, since a slot's representation changes when inference succeeds or fails.

### B1 comparison

| Axis | a setcore | b nomcore | c hybrid | d setdecl |
|---|---|---|---|---|
| Inference burden | highest; principal types claimed by MLscript, no production compiler (research/04 §3, research/05 §2.3) | lowest; HM + generics, no subtyping (research/04 §2.3 Gleam) | medium; nominal atoms unify, algebra mostly checks (research/05 §2.3) | same as its base; `any`-fallback silently degrades everything |
| VM representation (research/03) | tag+payload pairs for unions; intersections/erased negation; some values boxed for type_id ICs | every slot single-shape except enums; simplest static frames | tag+payload for unions of tags; most precise member layout for intersections; differences erase | base cost plus boxed `any` slots with type_id tags |
| Expressiveness | maximal: unions, intersections, negation, error-set arithmetic, overloading (research/05 §2.2) | closed sums only; no subtraction, no narrowing beyond enums | error-set arithmetic and narrowing over nominal atoms; anonymous structural algebra excluded | as base; README surface preserved verbatim |
| Compiler complexity | semantic subtyping + emptiness checker + inference algorithm | a cover check over enum cases | disjointness/emptiness over nominal atoms; cover checks by tag identity | base checker plus predicate-level difference tracking (Whiley-style, research/04 §3) |
| Forecloses | nominal encapsulation, abstract types | one-mechanism null/error/enum (research/04 §3), sugar savings of research/04 §2.5 | fully anonymous structural algebra | `any`-fallback forecloses stable inference errors |

### B1 interactions

B5 errors (docs/en/design/03-effects-errors-async.md): (a) and (c) are the only options where union-carried errors with difference-typed handlers work as the research recommends (research/04 §11: `.err(h)` typed `(T | E) - E`); (b) forces `Result[T, E]` wrappers, manual `From`-free nesting, and the Style D compensation ladder (research/04 §12); (d) is (a)/(c) plus an `any` hole through which unchecked errors can flow — the worst pairing with B5's checked model. Linearity (docs/en/design/02, referenced by path only): the `~Copyable` negated-constraint idiom is itself a set-theoretic construction — a default positive `Copyable` constraint subtracted by negation, composing natively with union/intersection types (research/01, Hylo section and cross-cutting note 1); (a)/(c)/(d) accept it directly; (b) must model resource states as enums, losing `File[Open] | File[Closed]`-style union composition (research/05 §7.3) but keeping it expressible as enum cases.

**Recommendation (B1): (c) hybrid** — nominal tags composing set-theoretically keeps the README's set-instruction surface (as type expressions, absorbing (d)'s syntax while dropping its `any`-fallback), makes exhaustiveness decidable by tag identity, keeps the VM at (b)'s unboxed tag+payload cost, and has the strongest production-adjacent evidence (MLstruct/TS discriminated unions); the honest counterweight: (b) is the lowest-risk, fastest-to-correct core and gives up only error-set arithmetic and the TS feel — if the checker's disjointness machinery slips schedule, (b) is the fallback that costs nothing to reverse into.

## B2 — Type inference

What is decided here: who writes types — the programmer, the unifier, or the control flow — and whether the set algebra of B1 is inferred or only checked. The charter's explainability budget and the scripting-language sweet spot pull toward inference; the anti-specialization stance and compile-time-error rule pull toward checking.

### B2.a — bidirectional checking + annotations at module boundaries

Top-level and public signatures are required; local bindings, lambdas, and match arms are inferred by bidirectional propagation from the annotations.

```ts
// B2.a -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
const parse_age = (s: String) -> Int | ParseError { ... }  // boundary: annotated, checked
const parse_user = (row: Row) -> User | ParseError {       // boundary: annotated
  const age_text = row.get("age") or return Missing("age") // local: inferred
  const age      = parse_age(age_text)?                    // local: inferred from parse_age
  User { name: row.get("name") or "", age }                // local: inferred
}
```

Prior art: Rust requires item signatures, infers locals; Koka and MoonBit follow the same discipline (research/04 §6, "Recommendation `name: T` with locals inferred and top-level signatures required"); TypeScript is bidirectional with contextual typing (research/04 §2.3). Tradeoffs: inference burden is bounded and local — the checker never has to invent a type for an unannotated item, which is exactly what makes B1's algebra checkable rather than inferable; VM cost is nil; expressiveness is unchanged (annotations document APIs); compiler complexity is low and is the standard bidirectional pipeline. Forecloses: script-flavored zero-annotation code at module top level, and any claim that "the compiler figures everything out".

### B2.b — full Hindley-Milner, no annotations

Everything inferred by unification; annotations optional anywhere.

```ocaml
(* B2.b -- illustrative; surface syntax decided in docs/en/design/04-syntax.md *)
let f x = match x with Some n -> n + 1 | None -> 0
let g = List.map f
(* g : int option list -> int list, inferred; nothing was written *)
```

Prior art: OCaml and Haskell ship it; Koka infers heavily (research/04 §2.3, OCaml 14 features). Tradeoffs: inference burden is the classic HM price — principal types require no subtyping, so (b) in its pure form conflicts with B1's unions: adding set-theoretic subtyping to HM inference is precisely the MLscript/MLstruct research program, whose principal-type result (research/05 §2.3; OOPSLA 2022) has no production compiler behind it; without the algebra, (b) is clean but then B1 collapses to (b) nomcore with it. VM cost nil; expressiveness highest on paper; compiler complexity high if the algebra is included, and error messages under unification with unions/negations are an unsolved UX problem. Forecloses: required API signatures (docs suffer), and any predictable story about when inference succeeds — the README's "cannot infer => any" fallback is the temptation this option creates.

### B2.c — flow-sensitive refinement (on top of a or b)

Types of locals narrow through `is` tests, null/none tests, and match arms; unions refine monotonically; resets happen only at assignments, which immutability-by-default makes rare (research/04 §3, TS's narrowing sharp edges are closure/mutation/await — the first two vanish under `val`).

```ts
// B2.c -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
const j = json.get("port")          // j : Json | none
if j is Int && j >= 1 && j <= 65535 {
  serve(j)                          // j : Int here; no cast, no wrapper
}
match v {
  n: Int    => n + 1,               // arm narrows v to Int
  s: String => s.len(),             // arm narrows v to String
}                                   // exhaustiveness checked as set cover (research/04 §4)
```

Prior art: Whiley ships flow typing with structural types and updateable data, narrowing by `is` and null tests, with retyping rules in the spec (research/04 §3; https://whiley.org/pdfs/WhileyLanguageSpec.pdf); TypeScript narrows through `typeof`/`is`/discriminants and documents its resets (research/04 §3); Lobster shows narrowing is cheap enough for a small game-scripting implementation (research/04 §3, flow-sensitive typing survey); Kotlin smart casts are the S2 surface cost (research/04 §2.2 #8). Tradeoffs: inference burden is nil — refinement only shrinks known types, it never constructs unions or intersections during unification, which is the structural reason it composes with intersections/negation without exponential blowup (Whiley and TS both ship it without solver machinery; the blowup risk in set-typed systems concentrates in inferring intersections of function types — the Castagna ◁ relation — where the empirical cost is the pairwise emptiness check, quadratic in overload-group size and cacheable, research/05 §2.2); VM cost nil (narrowing is compile-time); expressiveness is the payoff research/04 §1 calls the single highest-leverage surface decision; compiler complexity is a flow-sensitive environment over locals — moderate, and the immutability default removes the two hard cases (mutation, closure capture). Forecloses: narrowing through mutable fields and loop-carried reassignment — the documented hard case of every flow-typed system (research/04 §3, known limit 2), acceptable under immutability but a real wall if `var` mutability of union-typed fields is later demanded.

### B2.d — annotations required everywhere

Zig/Go discipline: every binding and every parameter annotated; the checker never infers.

```zig
// B2.d -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
const port: Int = cfg.port or 8080;
const name: String = row.get("name") or "";
fn area(s: Shape): Float = match s { ... }
```

Prior art: Go and Zig (research/04 §2.3: 9 and 13 features, the deliberate near-zero baselines). Tradeoffs: inference burden zero, compiler simplest, diagnostics best (every expression has a written type); expressiveness unchanged but ergonomics collapse for a scripting sweet spot — Go and Zig are the two languages the author dislikes, and their verbosity is the reason; VM cost nil; compiler complexity minimal. Forecloses: TS/bun-feel local code, REPL-driven scripting, and half the point of an interpreted language per the charter's mission line.

### B2 comparison

| Axis | a bidirectional + boundaries | b full HM | c flow refinement | d annotations everywhere |
|---|---|---|---|---|
| Inference burden | bounded, local; boundaries anchor the algebra | principal types; conflicts with subtyping — MLscript-grade research to add B1 algebra (research/05 §2.3) | none added; shrinks known types only | zero |
| Composes with intersections/negation without blowup | yes — algebra is checked at annotated boundaries | unknown at production scale; research claim only (research/05 §2.3) | yes — monotone shrinking; overload emptiness is the only set-typed cost, quadratic in group size, cacheable (research/05 §2.2) | yes — trivially, everything is written |
| VM representation cost | none | none | none | none |
| Expressiveness | full, plus self-documenting APIs | full | full on locals; the highest-leverage surface feature (research/04 §1) | full but noisy |
| Compiler complexity | low-medium (standard pipeline) | high with algebra; medium without | moderate (flow environment, standard) | minimal |
| Forecloses | zero-annotation top level | required signatures, predictable inference | narrowing under mutation of union-typed fields (research/04 §3) | scripting ergonomics |

### B2 interactions

B5 errors: (a)/(c) make the `?` propagation check trivial — the enclosing return union is written at the boundary and containment is a set computation (research/04 §11); (b) must infer the return union, which is where unification and unions collide; (d) writes the union everywhere, safe but loud. Linearity (docs/en/design/02): bidirectional checking is the known-good substrate for affine flow analysis — the checker must track last-use anyway, and written boundary signatures give the linearity checker fixed facts; full HM under linearity is unsolved in practice (research/05 §7.1's Hylo/Koka evidence all use checked signatures).

**Recommendation (B2): (c)'s flow refinement layered on (a)'s bidirectional skeleton** — required boundary signatures anchor B1's set algebra where it is checked rather than inferred, refinement is monotone so it composes with intersections/negation without blowup (research/05 §2.2), and every component has production evidence (Rust, TS, Whiley); the honest counterweight: (b) is genuinely simpler to implement without the algebra, and "full HM + full set algebra" is a research bet this project should not place in v1.

## B3 — Products and sums

What is decided here: how records and variants are declared, named, and matched — label ergonomics, open vs closed variants, and what exhaustiveness costs.

### B3.a — anonymous structural records + variants

Records are their field sets; two same-shape types are the same type; variants are unions of tagged record shapes.

```ts
// B3.a -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
type Point = { x: Int, y: Int }                        // anonymous; {x:1,y:2} has this type by shape
type Shape = { kind: "circle", r: Float }
           | { kind: "rect",  w: Float, h: Float }     // variant = union of tagged shapes
fn area(s: Shape) -> Float {
  match s { c: { kind: "circle" } => 3.14159 * c.r * c.r, r: { kind: "rect" } => r.w * r.h }
}
const p: { x: Int, y: Int } = { x: 1, y: 2 }           // any same-shape record assigns
```

Prior art: TypeScript object types and discriminated unions (research/04 §2.4); OCaml polymorphic variants are the open-variant version (stable-surface knowledge; research/04 §2.4 "Sum types p (discriminated)"). Tradeoffs: label ergonomics — labels are the only identity, so typos are type errors (good) but two libraries' same-named different-shape records clash by design intent (the TS glue-world property); open vs closed — variants are closed as written, open variants need row machinery (that is option c); exhaustiveness is set cover over the union, with research/04 §4's set-difference diagnostics; VM cost — anonymous shapes cannot share declared layouts, so every record literal's layout is derived from its shape at the use site (fine unboxed, but type_id-based ICs need shape hashing where nominal types use a single id); compiler complexity — shape normalization/deduplication is real work TS pays for. Forecloses: nominal encapsulation and stable library layouts across versions without shape-locking discipline.

### B3.b — nominal structs/enums with patterns

Records and variants are declared, named, and matched by constructor; the Rust shape.

```rust
// B3.b -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
struct Point { x: Int, y: Int }
enum Shape {
  Circle { r: Float },
  Rect { w: Float, h: Float },
}
fn area(s: Shape) -> Float {
  match s { Circle { r } => 3.14159 * r * r, Rect { w, h } => w * h }
}
```

Prior art: Rust and Gleam (research/04 §2.4, §12 Style D); exhaustiveness over declared cases is the most-implemented checker in the survey (research/04 §4 table, all yes). Tradeoffs: label ergonomics — every one-shot aggregate needs a declaration (Gleam shows the cost is low in practice; research/04 §12 Style B); closed by construction; exhaustiveness trivial; VM cost optimal — declared layouts, one type_id each, the exact unboxed tag+payload story of research/03; compiler complexity minimal. Forecloses: ad-hoc structural gluing (`{x: p.x, y: p.y}` passing without a type), which B3.d's tuples and the anonymous-record sugar recover.

### B3.c — extensible rows (records with row variables)

Records carry a row variable for "the rest of the fields"; functions accept any record with at least the fields they name; Koka's effect rows are the same mechanism applied to effects rather than data.

```ts
// B3.c -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
fn loud(r: { msg: String | rho }) -> { msg: String | rho } {   // accepts any record with msg: String
  { r with msg: r.msg.upper() }                                 // extends/preserves the rest of the row
}
loud({ msg: "hi", at: now() })    // at flows through untouched; rho = { at: Time }
```

Prior art: Koka effect rows are production proof that row polymorphism works (research/04 §2.4 "effect rows", §3 "Zig's error-set merging and Koka's effect rows are the same phenomenon"); the OOPSLA 2020 resolution calculus subsumes extensible records under intersection-subtyping resolution (research/05 §2.3); PureScript/Elm rows are stable-surface knowledge. Tradeoffs: label ergonomics excellent for "accepts anything with these fields" APIs; open vs closed — inherently open, so exhaustiveness over row-typed scrutinees is partial (a row-typed value's full case set is unknown at the function); inference burden is the highest here — row unification is the classic source of unreadable inferred types, and it is machinery Koka pays for effects where rows are unavoidable but Gleam/Roc/MoonBit all declined for data; VM cost — row-typed parameters have statically unknown layout, forcing either boxing at row boundaries or per-shape ICs (research/05 §6.3's evidence words); compiler complexity highest per unit of sugar in this whole file. Forecloses: simple exhaustive-match semantics for row-typed values, and predictability of inferred signatures in errors (row variables leak into diagnostics).

### B3.d — tuples + records hybrid (Gleam/MoonBit shape)

Nominal structs and enums for named data; anonymous positional tuples for ad-hoc aggregates; no row variables.

```rust
// B3.d -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
struct User { name: String, age: Int }            // nominal record, punning supported (moonbit-core evidence)
enum Shape { Circle(Float), Rect(Float, Float) }  // tuple-enums: Gleam/MoonBit shape
let kv = ("host", 8080)                            // anonymous tuple, positional
match kv { (k, v) => println("${k}=${v}") }        // destructuring (research/04 §4)
const point = { x: 1, y: 2 }                       // anonymous record literal: sugar over a synthesized nominal record
```

Prior art: MoonBit structs + tuples + enums with record punning, verified in moonbit-core sources (research/04 §14; research/05 §5.1); Gleam records + first-arg tuples (research/04 §2.3, 9 features); the anonymous-record-literal keep-set item in research/04 §2.5 is exactly the sugar half. Tradeoffs: label ergonomics good where it matters (declared fields), positional where it does not (tuples); closed variants, trivial exhaustiveness; inference burden low (nominal atoms unify, tuples are structural products — no rows); VM cost optimal (declared layouts; tuples are fixed-size unboxed spans); compiler complexity low. Forecloses: "accepts any record with these fields" APIs (recoverable via generics over a minimal trait or by declaring a view type) and open variants.

### B3 comparison

| Axis | a anonymous structural | b nominal structs/enums | c extensible rows | d tuples + records |
|---|---|---|---|---|
| Label ergonomics | labels are identity; typo-safe, shape-clashing | declared labels; declarations for one-shots | best "at least these fields" ergonomics | declared + positional; punning |
| Open vs closed | closed as written; open needs rows | closed by construction | open by nature | closed |
| Exhaustiveness | set cover over union shapes | cover over declared cases | partial (rows are open) | cover over declared cases |
| Inference burden | shape normalization | lowest | highest (row unification) | low |
| VM representation (research/03) | per-shape derived layouts; ICs need shape hashing | declared layouts, one type_id each; optimal | layout unknown at row boundaries: boxing or per-shape ICs | declared layouts + fixed unboxed tuple spans |
| Compiler complexity | medium (shape dedup) | minimal | highest per unit of sugar | low |
| Forecloses | nominal encapsulation | ad-hoc structural gluing (recovered by tuples + sugar) | exhaustive matching over rows | "at least these fields" APIs |

### B3 interactions

B5 errors: error payloads are products; (b)/(d)'s declared error types give stable layouts for unboxed error returns (research/03's Result-in-registers); (a)'s anonymous error shapes work but hash-normalizing them into the IC machinery is extra cost. Linearity (docs/en/design/02): declared layouts (b)/(d) are what make drop order and scope-end drops statically known per frame (research/03's drop-discipline note); anonymous shapes complicate per-field affine tracking by forcing layout derivation into the checker.

**Recommendation (B3): (d) tuples + records hybrid** — nominal structs/enums plus anonymous tuples, with the anonymous record literal from the research/04 §2.5 keep-set as sugar over a compiler-synthesized nominal record, which recovers option (a)'s ad-hoc convenience at (b)'s runtime cost; rows (c) are the most expensive machinery per unit of sugar in this catalog, and their one unavoidable use (effect rows, if adopted) belongs to docs/en/design/03, not to data.

## B4 — Nullability

What is decided here: whether "missing" is a value family (`none`-style unit unions), a distinguished `null`, or both (sugar over unions). The Phi thesis (research/04 §3, https://www.arshad.fyi/writings/null-errors-and-enums-are-one-feature) is that null, errors, and enums are one feature: unit types in unions, `is` narrowing, `or` extraction. That thesis is the axis all three options sit on.

### B4.a — no null: `T | none` union discipline

There is no null literal; absence is the unit type `none` (a type whose only value is itself); optional is the plain union; values enter unwrapped.

```ts
// B4.a -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
type Port = Int | none
const p: Port = cfg.port              // no Some wrapper; the Int is the Int
const q: Int = p or 8080              // `or` extraction: first member wanted (research/04 §3)
if p is Int { serve(p) }              // narrowing; no cast
fn find(m: Map[String, V], k: String) -> V | none { ... }
```

Prior art: Phi's unit-type mechanism — `type Timeout` is a type and its only value; optional is `T | none` with no `Some`; enums are unions of unit types; one feature replaces three (research/04 §3). Tradeoffs: inference burden nil (unions already exist per B1); VM cost is the unboxed tag+payload pair — and when `T` has an unused bit pattern (references are never zero, f64 has NaN patterns research/03 declines to use) a niche optimization can pack `T | none` into one slot as a later optimizer pass without semantic change; expressiveness is the one-mechanism story: `none`, error members, and enum cases share `is`/`or`/match; compiler complexity nil beyond B1. Does `none` as one unit type among several break anything? No — unit types are ordinary values that compose with unions, narrowing, and equality like any member (research/04 §3); the one documented break is the flattening ambiguity: `V | none` where `V` itself can be `none` makes a stored `none` indistinguishable from a missing key — TypeScript shares this limit, and the fix is an explicit `Found[V] | none` wrapper at exactly the ambiguous sites (research/04 §3, known limit 1). Forecloses: nothing except a universal `null` that flows through every generic — which is the point.

### B4.b — language null + strict flow typing

`null` is a distinguished universal singleton; `T?`-style nullability is a type-system predicate rather than a union member; flow typing proves non-nullness (Kotlin, TS strictNullChecks, Whiley).

```kotlin
// B4.b -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
val p: Int? = cfg.port                // null is the special missing value
val q: Int = p ?: 8080                // elvis
if (p != null) { serve(p) }           // smart cast on null test (Whiley proves this with updateable data)
```

Prior art: Kotlin's `?`/elvis/`!!` and smart casts (research/04 §2.2 items 4-8); TS strictNullChecks (research/04 §3); Whiley is the formal proof that flow typing carries a non-null discipline over updateable structures (research/04 §3). Tradeoffs: inference burden nil; VM cost — `T?` is again a tag+payload pair, but `null` being universal means generic code over `T?` cannot niche-optimize as cleanly as a closed union per instantiation; expressiveness — `null` is one mechanism for missing but a second mechanism next to error unions and enums (Phi's point: three features where one suffices, research/04 §3), and the elvis/`!!`/`?.` spellings multiply (Kotlin spends 4 of its 38 sugar features here, research/04 §2.2); compiler complexity — null-flow tracking is option (c)'s narrowing restricted to one type; no harder. Forecloses: using `none`-style unit members as generic sentinels interchangeable with `none` (minor), and the B5 unification where `?`/`or` operate identically on `T | none` and `T | E` — the strongest argument against (b) (research/04 §11).

### B4.c — nullable-suffix sugar `T?` desugaring to (a)

Kotlin's surface, union semantics underneath: `T?` is exactly `T | none`; no new type, no new operator semantics.

```ts
// B4.c -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
const p: Int? = cfg.port              // sugar: T? == T | none, nothing else
const q: Int = p or 8080              // same `or` as errors and enums (research/04 §11)
p?.address?.city                      // optional chain: a thin desugar, token choice deferred to docs/en/design/04-syntax.md
if p is Int { serve(p) }              // same narrowing as B4.a
```

Prior art: Kotlin's `T?` surface (research/04 §2.2 #7, costed S2) over union semantics; research/04 §11 explicitly keeps `?.` out of v1 ("a thin `x?.f` desugaring to `x or none-map` can be added later if real code demands it — keep it out of v1 to respect the budget") while `or`/`is` carry the load. Tradeoffs: inference/VM/compiler costs identical to (a) — the suffix is a pure alias; expressiveness identical to (a) plus one familiar spelling; the only new decision is the chain token, which this file defers to docs/en/design/04-syntax.md per its charter (the research note already costs it S1-S2 and flags the risk). Forecloses: the reverse — keeping `T?` meaningful without unions (i.e., (b)) is what is foreclosed by choosing this option's desugaring.

### B4 comparison

| Axis | a `T \| none` unions | b language null + flow | c `T?` sugar over (a) |
|---|---|---|---|
| Inference burden | none beyond B1 | none | none beyond B1 |
| VM representation | unboxed tag+payload; niche packing possible later | tag+payload; `null` universal, weaker niching | identical to (a) |
| Expressiveness | one mechanism for null/errors/enums (research/04 §3) | one mechanism for null only; second to errors/enums | one mechanism, plus familiar spelling |
| Compiler complexity | B1's checker only | null-flow tracking (subset of B2.c) | B1's checker only |
| Sugar cost | 0 extra (`is`/`or` shared) | elvis + `!!` + `?.` + `T?` (4 Kotlin features, research/04 §2.2) | 1 alias |
| `none`-breaks-anything answer | only V\|none flattening; explicit wrapper at ambiguous sites, same limit as TS (research/04 §3) | shared-null simplifies that ambiguity but adds `!!`-shaped escapes | as (a) |
| Forecloses | universal null | B5 unification of extraction over unions | nothing beyond (a) |

### B4 interactions

B5 errors: (a)/(c) make `T | none` and `T | E` literally the same construct, so `?`, `or`, and `is` are stated once over unions — three tokens total for the whole optional/error story versus Kotlin's four and Swift's five (research/04 §11); (b) forces B5 to keep a parallel vocabulary. Linearity (docs/en/design/02): a union member test consumes/narrows an affine value exactly once — `x or default` where `x` is `~Copyable` must treat the taken branch as moved; this is the same borrow-to-compare shape research/05 §5.2 Break 4 requires and is visible to docs/en/design/02 whichever option wins.

**Recommendation (B4): (c)** — (a)'s union semantics exactly, with `T?` as a zero-semantic alias for `T | none` because the author's Kotlin/TS muscle memory expects the spelling and it costs nothing; `none` as one unit among several breaks nothing except the documented `V | none` flattening case that TypeScript shares and an explicit wrapper fixes locally; the `?.`-style chain token is deferred to docs/en/design/04-syntax.md with research/04 §11's bias (out of v1, add as desugar only if real code demands).

## B7 — Numeric tower

What is decided here: default integer/float types, literal rules, and the overflow policy. The interpreter context is decisive: research/04 §8 recommends Swift-style always-trap with explicit wrapping operators as "the only overflow policy that is predictable under an interpreter", and research/03 recommends defining overflow as a panic rather than an arbitrary-precision fallback so the arithmetic happy path has zero checks (the LuaJIT ADDOV note).

### B7.a — `Int` (i64) / `Float` (f64) defaults, trapping overflow

Literals default to Int/Float; suffixes select widths; no implicit narrowing; overflow traps; wrapping ops are explicit.

```ts
// B7.a -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
const n = 1_000_000          // Int (i64)
const x = 2.5               // Float (f64)
const b = 0xFF              // 255, still Int
const s = 42u8              // explicit width via suffix
const f = 2.0f32            // f32
const w = a +% b            // explicit wrapping add (Swift/Zig spelling family)
const t = i32(u)            // explicit conversion; no implicit narrowing (research/04 §8)
```

Literal syntax: `1_000_000`, `0xFF`, `0b1010`, `0o777`, suffixes `u8/i8/.../i64`, `f32/f64`; unsuffixed integers are Int unless the annotated context forces a suffixed width; unsuffixed floats are Float. Overflow table:

| Expression | Behavior |
|---|---|
| `9223372036854775807 + 1` | deterministic trap (panic) — reproducible, debuggable, cheap in a bytecode loop (research/04 §8) |
| `a +% b`, `a -% b`, `a *% b` | wrap modulo 2^64; for hash/PRNG use (research/04 §8) |
| `1.0 / 0.0` | `+inf` (IEEE 754, as everywhere) |
| `1 / 0` | trap by default; an error-union-returning division variant belongs to B5 (docs/en/design/03-effects-errors-async.md) |
| `f32(i)` / `i64(f)` | explicit conversions only; no implicit narrowing anywhere (research/04 §8) |

Prior art: Swift traps always with `&+` as the explicit escape; Zig's safe-mode panic with `+%` wrapping ops; Rust debug-panic/release-wrap (rejected: per-build divergence); Go/Kotlin silent wrap (rejected: unpredictable); research/04 §8 recommendation. Tradeoffs: inference burden nil; VM cost optimal — i64/f64 sit raw in unboxed slots, `add_i64` is one machine add with no tags (research/03's typed-encoding example), and trapping is a flags check the copy-and-patch tier folds; expressiveness — 64-bit ceiling without escape hatch to bignum (library BigInt covers it); compiler complexity minimal. Forecloses: silent-wrap users (hash code must switch to `+%` once, visibly) and implicit width coercion.

### B7.b — arbitrary-precision Int default

The default integer never overflows; machine words are opt-in via annotations.

```py
# B7.b -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
const n = 2 ** 100 + 1        # exact, no overflow, ever
const m = n * n               # grows; allocates
const fast: I64 = 42          # machine word is opt-in
```

Prior art: Python ints and Julia's promotion (research/04 §8, costed S3 "slow interpreter path"). Tradeoffs: inference burden nil; VM cost is the killer — either every Int is a boxed bignum (deleting the unboxed-slot design of research/03 wholesale) or a dual small-int/bignum representation with an overflow transition, which research/03 flags as the one guard class that would survive static typing (LuaJIT's ADDOV) and recommends against in favor of panics; type stability under the JIT tier degrades exactly Julia-style when values cross the small/big boundary; expressiveness — overflow bugs become impossible, at the price of unpredictable O(n) digit arithmetic appearing where O(1) was assumed; compiler complexity high (bignum fast paths in every arithmetic opcode). Forecloses: the 3-10x-of-C interpreter target on scalar code (research/03 §b.10 assumes unboxed scalars), and predictable frame slot sizes for Int.

### B7.c — full machine tower i8..i128 / f16..f128, width-polymorphic stdlib

All widths first-class; the stdlib is generic over width; literals pick widths by suffix or default.

```rust
// B7.c -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
let a: i8  = 127;
let x: f16 = 1.5f16;
fn sum[T: Num](xs: List[T]) -> T { ... }   // width-polymorphic stdlib: evidence per width
sum([1i8, 2i8])                            // resolves T = i8
```

Prior art: Rust and MoonBit both ship the full tower with width-polymorphic stdlib via traits (research/05 §1.1; moonbit-core numeric packages); research/03 §b.8 states the runtime answer: per-representation code supplied by the runtime ("the same per-representation stencils the copy-and-patch tier uses"), which removes the classic boxing penalty of numeric dictionary passing. Tradeoffs: inference burden — unconstrained integer literals need a default (Rust's i32-fallback rule) and width ambiguity errors; VM cost good per width (each slot is its raw width) but the polymorphic stdlib means evidence words on every generic numeric call until ICs warm (research/05 §6.3), and f16/f128 take software paths on common hardware; expressiveness maximal for systems work, mostly unused in a scripting sweet spot; compiler complexity — a numeric trait family, per-width code paths, and the ambiguity rules. Forecloses: none of the others (this is a superset), but it spends checker and stdlib budget the charter's explainability rule would rather spend elsewhere.

### B7.d — (a) plus deferred decimal/fixed-point units

(a) exactly, plus a plan: decimal and fixed-point arrive as library types (nominal wrappers), not as builtins; units of measure stay out (research/04 §8: F# measures are S3 and touch every numeric; the 90% case is a nominal wrapper plus derives).

```ts
// B7.d -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
const price = Dec("19.99")           // library type: exact decimal (Roc Dec precedent, research/05 §5.1)
price == Dec("19.99")                // 0.1 + 0.2 == 0.3 holds by construction in Roc's Dec
type Meters = Wrap[Int]              // library-level units via nominal wrappers + derives (research/04 §8)
```

Prior art: Roc's `Dec` default decimal makes `0.1 + 0.2 == 0.3` true by construction (research/05 §5.1, builtin docs); F# units of measure as the rejected builtin path (research/04 §8). Tradeoffs: all of (a)'s costs, plus a library dependency for money/exactness cases; no builtin cost; forecloses nothing — it is (a) with a stated growth path.

### B7 comparison

| Axis | a trap-default i64/f64 | b bigint default | c full tower | d (a) + decimal plan |
|---|---|---|---|---|
| Inference burden | nil | nil | literal width defaults + ambiguity errors | nil |
| VM representation | i64/f64 raw in unboxed slots; trap = flags check (research/03) | boxed bignums or dual-rep with overflow guards — the guard class research/03 rejects | per-width raw slots; evidence words for polymorphic stdlib ops | as (a); Dec is a boxed library type |
| Expressiveness | 64-bit ceiling; BigInt library escape | no overflow ever; O(n) surprises | maximal widths | (a) + exact money path |
| Interpreter predictability | best: deterministic traps, zero-check happy path (research/04 §8, research/03) | worst: allocation-dependent timing, JIT type-stability cliff | good per width | best |
| Compiler complexity | minimal | high (bignum in every arithmetic opcode) | medium-high (traits, per-width paths, software f16) | minimal builtin |
| Forecloses | silent wrapping | unboxed Int slots (research/03) | nothing (superset) | nothing |

### B7 interactions

B5 errors: division by zero and overflow are the natural members of an arithmetic error union (the README's `ArithmeticError` instinct); with (a) they default to traps and any error-returning variants are library functions typed `T | ArithError`, fitting docs/en/design/03's union-carried model. Linearity (docs/en/design/02): numerics are the trivially `Copyable` case — unboxed i64/f64 slots are what make linear bookkeeping cheap elsewhere; (b)'s boxed bignums would make even Int an RC-managed value, polluting every affine flow with allocation sites.

**Recommendation (B7): (a), with (d)'s decimal as the stated library growth path** — trapping overflow is the only policy predictable under an interpreter (research/04 §8), keeps arithmetic zero-check on the happy path (research/03's explicit recommendation against bigint-fallback guards), preserves unboxed Int slots, and the suffix/cost story matches the keep-set already chosen in research/04 §8; the tower (c) can grow later behind the numeric-trait seam if systems workloads demand it.

## B8 — Strings

What is decided here: encoding, indexing discipline, interning, and what the VM holds in a slot versus on the heap. Strings are always heap objects under research/03's baseline — the slot holds one raw GC pointer — so the real questions are the byte format, whether equality is O(n) or O(1), and whether slices copy.

### B8.a — immutable UTF-8 byte strings, index-safety discipline

UTF-8 bytes on the heap; no O(1) character indexing; checked boundary slices; char iteration; graphemes in stdlib.

```rust
// B8.a -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
const s = "héllo"            // 6 UTF-8 bytes on the heap; slot holds the pointer
s.byte_len()                 // 6
s.slice_char(0, 2)           // "hé" | none  -- checked boundary, union return (research/04 §7)
s.chars()                    // iterator over Unicode scalar values
s.graphemes()                // stdlib, not core (research/04 §7)
s.utf8_bytes()               // escape hatch: raw bytes
```

Prior art: Rust `&str` — UTF-8, byte indexing that must land on char boundaries, `char_indices`/`chars` iterators, no O(1) char indexing (stable surface, consistent with research/04 §7); research/04 §7's own recommendation for an interpreter: store UTF-8, byte-index zero-copy slices that must land on char boundaries via a checked API returning `| none`, iterate `chars`, keep graphemes out of core. Tradeoffs: inference burden nil; VM cost — heap object `{ type_id, rc, byte_len, bytes }` (Inko's 16-byte-minimum single-allocation string is the layout precedent, research/03), slices are views into the parent buffer with RC keeping the parent alive (zero copy) or copies under simpler RC accounting — an implementation choice, not a semantic one; equality/hash O(n) with length fast-reject; expressiveness full for text processing, honest about grapheme clusters (Swift's no-integer-subscript stance is the alternative and is worse for an interpreter, research/04 §7); compiler complexity minimal. Forecloses: O(1) code-point indexing (nobody correct has it), free interning (option b's advantage).

### B8.b — interned immutable strings + builder

All strings interned at creation; equality is pointer comparison; a builder type for construction; slices re-intern.

```lua
-- B8.b -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
const a = "config"           -- interned on first use
const b = "con" .. "fig"     -- same pointer after interning: a == b is O(1)
const h = header_of(a)       -- dict keys need no hashing after first intern
const pre = s.slice(0, 3)    -- slice re-interns: O(k) hash + lookup, or an unmarked view
```

Prior art: Lua's interned 8-bit strings (stable surface; research/04's author-taste section lists Lua as known); MoonBit's string model with builders (research/04 §14 moonbit-core sources). Tradeoffs: inference burden nil; VM cost — equality and dict lookups are O(1) after first intern, but creation pays a global hash-table synchronized lookup, slices either re-intern (O(k), and short-lived slice-heavy code allocates the intern table badly) or lose interning (two-equality-regime confusion); memory is retained by the intern table (Lua's known behavior with dynamic key strings); expressiveness fine for symbol-like data, awkward for text processing; compiler complexity — intern table, synchronization, two slice regimes. Forecloses: cheap byte-level text processing patterns (you pay interning on every transient string), and multi-core scaling of string creation without sharded tables.

### B8.c — UTF-16 or platform strings

Code units are the index space; O(1) unit indexing; native interop with JS engines, Windows, and the JVM.

```ts
// B8.c -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
const s = "héllo"            // UTF-16: 6 code units; s[1] == '\u00E9' in O(1)
s[5]                          // code-unit index: the surrogate-pair hazard lives here
win32_call(s)                 // wide-char APIs: zero-conversion
JSON.parse(src)               // JS-engine-shaped data flows in directly
```

Prior art: JavaScript/Java/C#/Windows wide APIs (stable surface); the interop argument is real for FFI (research/06 modules/FFI notes own that boundary). Tradeoffs: inference burden nil; VM cost — 2x memory for ASCII-heavy text (the common case for config/logs), and every scalar-value operation carries surrogate-pair logic (the bug class JS/Java live with); equality O(n) as (a); expressiveness — O(1) indexing is nominal: correct character indexing still needs pair-aware logic, so it is O(1) footguns; compiler complexity minimal. Forecloses: cheap ASCII density and the byte-string escape hatch becomes a second string type.

### B8.d — (a) plus rope for editor/large-text workloads

(a)'s String plus a stdlib `Rope` (chunk tree) for O(log n) insert/delete/slice on large texts.

```ts
// B8.d -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
const doc = Rope.from(text)          // chunk tree over immutable UTF-8 chunks
doc.insert(1_000_000, "x")           // O(log n), no full copy
const line = doc.line(42)            // materializes a String on demand
```

Prior art: rope data structures are standard editor technology (stable surface); research/04 §7 does not survey them — this option is (a) plus a library-scale data structure, not a core change. Tradeoffs: all of (a)'s costs; VM cost — rope nodes are ordinary heap trees of chunk references, no VM primitive; expressiveness adds the editor niche; compiler complexity nil (stdlib only). Forecloses: nothing.

### B8 comparison

| Axis | a UTF-8 bytes | b interned | c UTF-16 | d (a) + rope |
|---|---|---|---|---|
| VM slot holds | one raw GC pointer to `{type_id, rc, byte_len, bytes}` (research/03) | pointer into intern table | pointer to UTF-16 buffer | pointer, as (a) |
| Equality/hash | O(n) with length fast-reject | O(1) pointer compare after intern | O(n) | O(n) per chunk op |
| Slices | zero-copy views, checked boundaries, `| none` (research/04 §7) | re-intern (O(k)) or unmarked views | views in code units | O(log n) chunk views |
| Memory | 1x, ASCII-dense | 1x + intern-table retention | 2x for ASCII-heavy | 1x + tree overhead |
| Inference burden | nil | nil | nil | nil |
| Compiler complexity | minimal | intern table + sync + two regimes | minimal | stdlib only |
| Forecloses | O(1) char indexing (correctly) | transient-string workloads | ASCII density, byte escape hatch | nothing |

### B8 interactions

B5 errors: encoding is orthogonal; `slice_char` returning `String | none` reuses the B4 union discipline rather than panicking (research/04 §7's explicit design). Linearity (docs/en/design/02): string views into parent buffers are non-owning borrows — under affine rules a slice must not outlive its parent unless the parent's RC keeps it alive, which is exactly the borrowed-operand shape research/05 §5.2 Break 4 prescribes; interning (b) sidesteps this only by keeping everything alive in the table.

**Recommendation (B8): (a)** — immutable UTF-8 with a byte-length header, zero-copy checked slices typed `String | none`, `chars` iteration, graphemes in stdlib (research/04 §7's recommendation verbatim); it keeps the unboxed-slot VM story (strings are just heap refs), makes equality honest, and (d)'s rope is a library add-on when an editor workload appears; interning (b) and UTF-16 (c) both tax the common case to serve narrow ones — and the FFI boundary (research/06, docs/design catalog for modules/FFI) converts at the edge.

## B9 — Equality, ordering, hashing

What is decided here: what `==` means, whether it is total, and what stays trait/derive-mediated. The derive mechanism itself (Hash/Show/serde attributes, synthesis rules) is owned by docs/en/design/05-generics-metaprogramming.md and is only pointed at here, per instructions. research/05 §5.2 already establishes the totality conditions any structural choice must satisfy; they are built into option (a) below.

### B9.a — universal structural `==` (Roc model, with the five carve-outs)

`==` is structural over all data, takes borrows, and is total by construction: function comparison is rejected statically; floats compare bit-structurally with a named IEEE comparison beside them; mutable/reference cells and `~Copyable` resources compare by identity; hashing is co-required.

```ts
// B9.a -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
p == q                  // structural, borrows both operands (Break 4: comparison never consumes)
f == g                  // compile error: functions carry no decidable equality (Break 1)
nan == nan              // true: bit-structural; reflexivity holds for dict keys (Break 2, choice i)
ieee_eq(nan, nan)       // false: the named IEEE comparison, Roc's is_float_eq split (research/05 §5.2)
cell == cell2           // identity: mutable/reference cells compare by address (Break 3)
file == file2           // identity for ~Copyable resources (Break 4; docs/en/design/02 owns the linear layer)
```

Prior art: Roc ships one structural `==` with no derive at all, splitting `is_eq` / `is_float_eq` / `is_approx_eq` and coupling `is_eq` with `to_hash` in stdlib signatures (research/05 §5.1-5.2); OCaml's universal polymorphic compare is the same idea with documented costs — slow reflective traversal and divergence on cycles, which the identity-for-cells carve-out avoids (research/05 §5.1 OCaml row, §5.2 Break 3); Elm rejects function comparison statically (research/05 §5.2 Break 1). Tradeoffs: inference burden nil; VM cost — primitive slots compare in one opcode; aggregates deep-compare with a per-type_id inline-cached fast path (research/05 §6.3), and immutability means the compare result is cacheable for interned/normalized values; expressiveness — the only losses are function equality (usually a bug, research/05 §5.2) and IEEE-`==` as the default spelling (reachable by name); compiler complexity — the checker needs the "no Eq capability on function types" rule and the cell-identity rule, both simple; coherence problems do not arise because there is nothing to override per-type in a conflicting way. Forecloses: per-type custom value semantics for `==` (e.g., a Money type whose `==` ignores currency — it must use an `equiv`-style named method instead), and IEEE-float keys behaving IEEE-style in dicts (they behave bit-style; `is_float_eq` stays available).

### B9.b — trait-mediated equality (Rust `PartialEq` model)

`==` exists only where an `Eq` implementation exists; every comparison is user- or derive-defined.

```rust
// B9.b -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
impl Eq for Point {                     // or #derive(Eq): nothing free, everything explicit
  fn equal(a: &Point, b: &Point) -> Bool { a.x == b.x && a.y == b.y }
}
// MoonBit evidence: builtin/tuple_eq.mbt hand-writes Eq for (T0,T1), then (T0,T1,T2), ... per arity
```

Prior art: Rust `PartialEq`/`Eq` (research/05 §5.1); MoonBit's `Eq` trait with per-arity tuple impls hand-written in `builtin/tuple_eq.mbt` — the cited boilerplate evidence that trait-mediated equality generates per-shape work even in a derive-rich language (research/05 §5.1); the coherence friction Rust pays for it is documented in research/05 §3.2. Tradeoffs: inference burden — the checker resolves which Eq applies, trivial when heads are disjoint per research/05 §2.2; VM cost identical to (a) once resolved (the derived body is the same structural compare); expressiveness — maximum per-type control including IEEE semantics per type (MoonBit's choice, with the documented NaN precondition leaking into every derived type that contains floats, research/05 §5.1-5.2); compiler complexity — trait resolution plus orphan/coherence questions the charter would rather not have. Forecloses: comparing types nobody wrote Eq for (generic code must carry `T: Eq` bounds and evidence words everywhere), and the no-boilerplate property.

### B9.c — structural `==` builtin + derive-based ordering/hash (MoonBit model)

`==` is compiler-generated structural for all data types unconditionally (Kotlin data-class semantics without the data-class ceremony); `Ord`, `Hash`, `Show` are explicit derives.

```moonbit
// B9.c -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
#derive(Hash, Show, Ord)
struct Point { x: Int, y: Int }   // == is builtin structural; Ord/Hash/Show come from the derive
p1 == p2                          // structural, always available
p1 < p2                           // uses derived Ord (field order = lexicographic)
```

Prior art: MoonBit — derives for Eq/Hash/Show/ToJson with the IEEE float caveat documented in `builtin/traits.mbt` (research/05 §5.1); Kotlin data classes generate equals/hashCode/toString unconditionally from constructor parameters (research/05 §5.1); Swift SE-0185 synthesis (research/05 §5.1). Tradeoffs: as (a) except floats — if the builtin `==` follows IEEE (MoonBit's documented choice), reflexivity breaks and the preconditions leak; if it follows bit-structure, this collapses into (a) plus derive optics; ordering cannot be structural (any total order over a set is arbitrary, research/05 §5.3), so Ord staying derive-mediated is correct in every option. Forecloses: the same per-type `==` overrides as (a).

### B9.d — reference identity only

`==` compares references; structural comparison is an `equals` method tradition.

```java
// B9.d -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
a == b            // identity
a.equals(b)       // structural if someone remembered to override; default is identity (the Java trap)
```

Prior art: Go and Java (research/05 §1.9 and §5.1); Go's incomparable-runtime-panic hole — `==` on interface values panics when the dynamic type is incomparable — is the cautionary evidence that "identity-first" is not simpler, merely later-failing (research/05 §1.9, "static totality over equality is worth more than the syntax Go saves"). Tradeoffs: under immutable-by-default with value semantics, identity is almost meaningless — two equal immutable records are interchangeable, which is Roc's exact justification (research/04 §9); identity remains meaningful only for linear resources and memoization, both covered by (a)'s cell/resource carve-out; inference burden nil; VM cost — pointer compares are cheap but every structural need descends into method calls; expressiveness worst; compiler complexity minimal. Forecloses: value-semantic programming as the default idiom; this option fights the charter's own immutability stance, which is why it is listed.

### B9 comparison

| Axis | a universal structural | b trait-mediated | c builtin structural + derives | d identity only |
|---|---|---|---|---|
| Totality | total by construction: 5 carve-outs from research/05 §5.2 | partial by default: missing Eq = compile error (honest, but boilerplate) | total on data; float semantics decision inherited from MoonBit (IEEE + preconditions) | holes: Go-style runtime panics on incomparable values (research/05 §1.9) |
| Inference burden | nil | Eq resolution; `T: Eq` bounds spread through generic signatures | nil | nil |
| VM representation | slot primitives + IC-cached deep compare; borrows only (Break 4) | same body once resolved; evidence words in generic code | as (a) | pointer compares; deep compares are method calls |
| Expressiveness | loses per-type `==` overrides; IEEE reachable by name | full per-type control; NaN preconditions documented everywhere (research/05 §5.1) | as (a) modulo float choice | value semantics abandoned |
| Boilerplate | none (MoonBit's tuple-arity impls disappear, research/05 §5.3) | per-type impls, per-arity tuples (moonbit-core `tuple_eq.mbt`) | none for ==; derives for Ord/Hash/Show | none |
| Forecloses | custom `==` semantics per type | no-boilerplate equality | per-type `==` overrides | immutable value-semantic idiom |

### B9 interactions

B5 errors: error payloads compare structurally in tests (`e == Timeout`) with no Eq-impl ceremony — (b) makes even test assertions carry trait bounds. Linearity (docs/en/design/02): every option must take borrows so comparison never consumes an affine value (research/05 §5.2 Break 4), and resources compare by identity — this is the `~Copyable`-interplay point this file was asked to carry from research/01: copyability is expressible as a negated set constraint (research/01 Hylo section, cross-cutting note 1), so `==`'s identity carve-out for `~Copyable` values is not an ad-hoc rule but the same set algebra deciding both.

**Recommendation (B9): (a) universal structural `==` with the five research/05 §5.2 carve-outs** (functions rejected statically; bit-structural floats with named `is_float_eq`; cells and `~Copyable` resources by identity; borrow-only operands; Hash co-required), plus derive-mediated `Ord`/`Hash`/`Show` — it deletes MoonBit's per-arity tuple boilerplate (research/05 §5.3), keeps reflexivity (the load-bearing property for dicts and dedup, research/05 §5.2 Break 2), and the derive mechanism itself is decided in docs/en/design/05-generics-metaprogramming.md, not here.

## Cross-cutting interaction summary

B5 (docs/en/design/03-effects-errors-async.md, union-carried errors as its recommended base): presupposes B1 in {a, c, d} — difference-typed `.err` handlers (`(T | E) - E`, research/04 §11) and grow-without-breaking error sets (research/04 §3) exist only where unions are type-language citizens; B1(b) forces Result wrappers and should be recorded as the fallback that downgrades docs/en/design/03's recommended surface, not as a co-equal partner for it. docs/en/design/02 (linearity, being written concurrently — referenced by path only, contents not assumed here): the `~Copyable` negated-constraint idiom is set-theoretic by construction (research/01), so it composes with B1 {a, c, d} directly and with (b) via enum-modeled resource states; B2(a+c)'s checked boundaries are the substrate affine flow analysis prefers; B9(a)'s borrow-only `==` and identity-for-resources carve-out are the equality-side obligations docs/en/design/02 can rely on.

## Coherent combos

Three named end-to-end type systems, each shown on the same program: a config parser that reads JSON-ish input, validates a port, and handles errors — exercising products, sums, union/narrowing, error sets, and equality. All code is illustrative; surface syntax is decided in docs/en/design/04-syntax.md.

### Combo 1 — "Set-core": MLstruct-flavor unions + flow refinement + universal ==

B1(a) + B2(c on a bidirectional skeleton) + B3(a) + B4(a) + B7(a) + B8(a) + B9(a).

```ts
// Combo 1 "Set-core" -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
type Json = null | Bool | Int | Float | String | Json[] | { [String]: Json }

type CfgErr = IoErr | MissingKey(String) | BadPort(Int)     // error set: a plain union
type Port    = Int & { }                                    // (no-op here; see intersection use below)

const get = (j: Json, k: String) -> Json | MissingKey {
  match j {
    m: { [String]: Json } => m[k] or none or return MissingKey(k),  // missing key: none member
    _                     => return MissingKey(k),
  }
}

const load = (path: String) -> Config | CfgErr {
  const text = read_file(path)?                 // IoErr propagates; return union contains it
  const raw  = get(parse_json(text)?, "port")   // Json parse errors propagate likewise
  if raw is Int && raw >= 1 && raw <= 65535 {   // flow refinement: raw: Int in the branch
    Config { host: "localhost", port: raw, retries: 3 }
  } else {
    return BadPort(raw is Int ? raw : -1)
  }
}

const cfg = load("app.cfg").err((e) => {        // handler covers the whole error set:
  match e {                                     // result type is Config - CfgErr == Config
    io: IoErr             => log("io: ${io}"),
    MissingKey(k)         => log("missing: ${k}"),
    BadPort(p)            => log("bad port: ${p}"),
  }
  default_config()
})

const total = [1, 2, 3].map((x) => x * 2).reduce(0, (a, b) => a + b)
const same = cfg == default_config()            // universal structural ==, borrows
```

Verdict: maximal expressiveness and the truest match to the charter's set-theoretic taste and the README's set instructions; error handling needs no wrapper at all and matches diagnose by set arithmetic (research/04 §3-4). Costs concentrated where the research says they are: the checker (semantic subtyping, emptiness) and the inference bet (MLscript-grade principal types have no production compiler, research/05 §2.3); anonymous structural records (B3.a) additionally pay shape normalization in the VM's IC machinery. Implications: docs/en/design/02 gets `~Copyable` composition for free (negated constraints are native set terms, research/01); docs/en/design/03 can adopt union-carried errors and difference-typed `.err` exactly as researched (research/04 §11); docs/en/design/04 must reserve `|`, `&`, `-`, `is`, `or` in type and expression positions and drop Kotlin items 4-8, 27 (research/04 §2.5); docs/en/design/05's disjointness checker becomes load-bearing for overloading (research/05 §2.2).

### Combo 2 — "Nominal-core": Rust/Gleam-flavor + Result/Option

B1(b) + B2(a) + B3(b) + B4-as-Option/Result wrappers + B7(a) + B8(a) + B9(b).

```rust
// Combo 2 "Nominal-core" -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
enum Json { Null, Bool(Bool), Num(Float), Str(String), Arr(List[Json]), Obj(Map[String, Json]) }

enum CfgErr { Io(IoErr), MissingKey(String), BadPort(Int) }

struct Config { host: String, port: Int, retries: Int }

fn get_port(j: &Json) -> Result[Int, CfgErr] {
  match j {
    Json::Obj(m) => match m.get("port") {
      Some(Json::Num(f)) if f >= 1.0 && f <= 65535.0 => Ok(f as Int),
      Some(_) => Err(CfgErr::BadPort(-1)),
      None => Err(CfgErr::MissingKey("port".into())),
    },
    _ => Err(CfgErr::MissingKey("port".into())),
  }
}

fn load(path: String) -> Result[Config, CfgErr] {
  let text = read_file(path)?;                  // Err propagates through the wrapper
  let port = get_port(&parse_json(text)?)?;
  Ok(Config { host: "localhost", port, retries: 3 })
}

fn main() -> Result[(), IoErr] {
  let cfg = load("app.cfg").unwrap_or_else(|e| { log("${e}"); default_config() });
  let total = [1, 2, 3].map(|x| x * 2).reduce(0, |a, b| a + b);
  let same = cfg == default_config();           // requires derived PartialEq on Config
  Ok(())
}
```

Verdict: the simplest correct compiler in this whole file — no subtyping, no set algebra, no flow environment; exhaustiveness over enum cases is the best-understood checker there is; and it transfers the author's Rust fluency directly. The costs are the compensation ladders research/04 §12 Style D documents: `Some`/`Ok`/`Err` wrapping everywhere, `.ok_or(...)?` shapes, `unwrap_or_else` instead of `or`, and the loss of error-set arithmetic (adding a `CfgErr` case breaks every match on it; `(T | E1 | E2) - E1` is inexpressible). Implications: docs/en/design/02 must model resource states as enums (`File[Open]` becomes `enum FileState`), losing phantom-state unions (research/05 §7.3) but keeping everything expressible; docs/en/design/03 must base B5 on `Result[T, E]` with propagation but no difference-typed handlers — its union-carried recommendation is unavailable; docs/en/design/04 keeps the budget but spends it on wrapper-adjacent spellings; docs/en/design/05's trait/instance story works unchanged and its derive machinery becomes mandatory for equality.

### Combo 3 — "Hybrid": nominal tags + set arithmetic + narrowing (recommended)

B1(c) + B2(a + c) + B3(d with anonymous-record sugar) + B4(c) + B7(a/d) + B8(a) + B9(a).

```ts
// Combo 3 "Hybrid" -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
tag IoErr; tag Missing; struct BadPort { got: Int }         // nominal atoms
type CfgErr = IoErr | Missing | BadPort                     // set instruction over tags

struct JsonObj { fields: Map[String, Json] }
struct Config  { host: String, port: Int, retries: Int }

const get_port = (j: Json) -> Int | Missing | BadPort {
  const obj  = j is JsonObj or return Missing { key: "port" }
  const pv   = obj.fields.get("port") or return Missing { key: "port" }   // T | none union
  const p    = pv is Int or return BadPort { got: -1 }                    // narrows pv: Int
  if p >= 1 && p <= 65535 { p } else { return BadPort { got: p } }
}

const load = (path: String) -> Config | CfgErr {
  const text = read_file(path)?                              // CfgErr member propagates
  const port = get_port(parse_json(text)?)?
  Config { host: "localhost", port, retries: 3 }
}

const cfg = load("app.cfg").err((e) => {                     // exhaustive over the tag union:
  match e {                                                  // result: Config - CfgErr == Config
    b: BadPort => log("bad port ${b.got}"),
    _          => log("config read failed"),
  }
  default_config()
})

const kv   = ("host", 8080)                                  // anonymous tuple (B3.d)
const same = cfg == default_config()                         // universal structural == (B9.a)
```

Verdict: keeps the README's set-instruction surface (`type CfgErr = IoErr | Missing | BadPort` is literally `A = B | C`), grounds exhaustiveness in tag identity so the checker stays at enum-cover difficulty, keeps every slot at the unboxed tag+payload cost of research/03, and preserves the one-mechanism extraction story (`?`, `or`, `is` over `T | none` and `T | CfgErr` alike). What it gives up versus Combo 1 is only fully anonymous structural algebra — and B3.d's anonymous-record sugar recovers the common case of that. Implications: docs/en/design/02 can express `~Copyable` constraints and phantom state unions (`File[Open] | File[Closed]`, research/05 §7.3) over the same tag algebra; docs/en/design/03's union-carried errors and difference-typed `.err` work with tag-subtraction semantics; docs/en/design/04 owns the tokens (`|`, `&`, `-`, `is`, `or`, `?`, `T?`) with research/04 §11's surface already designed for exactly this semantics; docs/en/design/05 needs its disjointness checker only for overload/instance groups, which the tag-identity restriction makes strictly easier than the fully structural version (research/05 §2.2).

## Sources

Research notes (primary evidence for every citation above): docs/en/research/04-syntax-ergonomics.md §2 (sugar ceiling), §3 (set-theoretic/flow typing: Phi https://www.arshad.fyi/writings/null-errors-and-enums-are-one-feature, MLscript https://github.com/hkust-taco/mlscript, MLstruct OOPSLA 2022 `https://lptk.github.io/files/[v6.2]%20mlstruct.pdf`, Whiley https://whiley.org/pdfs/WhileyLanguageSpec.pdf), §4 (pattern matching/exhaustiveness), §6 (annotation discipline), §7 (strings), §8 (numerics), §9 (equality), §11 (error surface), §12 (styles A-D), §14 (sources). docs/en/research/05-generics-metaprogramming.md §2.1-2.3 (set-theoretic subtyping, overloading as arrow intersection, resolution as intersection subtyping OOPSLA 2020, disjoint merges OOPSLA 2025), §3.2 (coherence costs), §5.1 (derive survey incl. moonbit-core `builtin/tuple_eq.mbt` and `builtin/traits.mbt` evidence, Roc `is_eq`/`is_float_eq`/`Dec`, Swift SE-0185, OCaml universal compare), §5.2 (the five totality breaks), §5.3 (verdict), §6.3 (evidence words and inline caches), §7.1-7.3 (Hylo `~Copyable` generics, variance, phantom states). docs/en/research/03-runtime-ir-jit.md: value representation (no NaN-boxing, unboxed slots), unboxed ADT/tag+payload layout, `{u32 type_id; u32 rc}` header, overflow-as-panic recommendation, interpreter-vs-C gap decomposition. docs/en/research/01-linearity-ownership.md: Hylo/Swift `~Copyable` as a negated default constraint (SE-0390 quote), cross-cutting note 1 (copyability as a set-theoretic constraint), `Result<~Copyable>` payload caveat. README.md: the historical set instructions (`A = B | C`, `B & C`, `B - C`, `(B & C) - D`, `any`) evaluated inside B1.d, not preserved unconditionally.
