# 09 — Decision rationale

Status: companion to `docs/en/design/08-decisions.md` (the Phase 3 record). For each of the 42 decision points this file details the basis of the decision: the binding constraints (charter hard requirements, author-taste calibration, budget ceilings, runtime baseline), the decisive argument for the chosen option, the decisive argument against every rejected option, the evidence pointers, and what the choice locks in. The catalogs `01-07` carry the full option analysis with prior-art detail; this file records why each choice fell where it did, and repeats catalog text only where the argument needs it. Chinese translation: `docs/zh/design/09-decision-rationale.md`.

Reading guide: every point uses the same shape — Constraints / Why the winner wins / Why the others lose / Evidence / Locks in. "Charter req N" refers to AGENTS.md hard requirements; "research/0N §X" refers to the research notes whose numbering the catalogs cite verbatim. Terminology follows 08: "deferred additive" means decided out of 1.0 scope with a migration path that requires no breaking change.

## A. Identity

### A1 — Positioning: standalone core with an embeddable platform mode

**Constraints.** Charter req 1 (interpreted execution with an internal IR) and req 3 (ultra-high performance with tiered optimization) presuppose a runtime the project owns end to end; the author's TS+bun tooling taste wants a first-class standalone CLI; embedding is valued (the Lua niche) but secondary.

**Why the winner wins.** (c) builds the standalone runtime the performance charter already demands, then obtains embedding as an application of two decisions already made: platforms are ordinary packages exporting effect handlers (06 H5b) over the E1d handler runtime. Embedding adds no new architecture — it is the same runtime entered from a host.

**Why the others lose.** (a) standalone-only forfeits an embedding niche that (c) delivers nearly for free. (b) embeddable-first makes the host own allocation and effects (Roc's posture); platform versioning then becomes the compatibility unit for every package and a rich fixed stdlib story weakens (research/06 §2.6, §5.4). (d) wasm-first subordinates the value representation to wasm GC constraints and forces E2b fibers to lower through JSPI-shaped mechanisms (research/03 §b.4; 07 H1d tradeoff) — wasm is a tier-3 deployment target, an architecture it is not.

**Evidence.** research/06 §5.4 (platform inversion decomposed per the RISC ethic), §7 combo D (embedding as an add-on axis, a standalone answer it is too narrow for); research/03 §b.12 (backend risk register).

**Locks in.** H5(a)+(b); A2's tooling priorities; the "stdlib owns no effects" cultural commitment lands softly (the default platform is the `ds` CLI itself).

### A2 — Primary workloads: CLI/tools/scripts first, games/simulation second

**Constraints.** Workload choice decides what v0 tooling must prove: startup latency, edit-run loop, hot reload. The author's daily use is small tools and scripts (TS+bun); AI/numeric work must remain reachable without shaping the core.

**Why the winner wins.** (a) is served directly by decisions already in the staging plan: instant mmap'd dsmod startup (research/06 §6.3's sub-10ms budget), zero-config script mode (H6c), and the interpreter-first tier that never demands warmup. (c) is served by the two-slot hot-reload runtime (06 H6-mandatory 5) and the query compiler, both committed for other reasons; Mun is the precedent that this pairing is what game reloading needs. (d) stays reachable through tier 3 without spending v0 budget on it.

**Why the others lose.** (b) web backend needs long-running supervision and serving polish; both arrive as library layers over E2b nurseries and E4d-style supervision once the core exists, so making them primary would reorder v0 around deferred features. Making (d) primary would pull B7 toward a full numeric tower and Julia-shaped demands the charter's explainability budget declines.

**Evidence.** research/06 §6.3 (startup levers), §4.5 (hot reload), §1.10 (script-to-package growth path); research/03 §b.10 (tier-1 3-10x target).

**Locks in.** H6(c) script mode; the two-slot runtime's priority in staging.

### A3 — Compatibility: rapid-break until 1.0, semver after; hashes at the package level

**Constraints.** Pre-1.0, the design must be free to fix checker-level mistakes (the B1c disjointness machinery is the riskiest part); post-1.0, an ecosystem needs stability promises. The author's Nix pains (global locks that break dependents, undocumented internals) rule out designs where upstream updates cascade breakage.

**Why the winner wins.** (a) until 1.0 is the Roc/MoonBit posture and matches reality: no users to break while the checker matures. (b) takes over exactly when ecosystem dynamics start. (c)'s hashing idea is adopted where it costs nothing culturally: package identity by blake3 content-hash cascade, so the manifest is the lock and `follows`-style global mutation cannot exist (H6b).

**Why the others lose.** (b) from day one freezes semantics before the design settles — Cargo discipline presumes a stable language. (c) as the source-level identity model abolishes plain-text files, so git, grep, patch, and text LSP get reinvented through a sync protocol (research/06 §1.8); the cheap borrow is hashing the IR artifacts and package identity, which G3.5's dsmod already does.

**Evidence.** research/06 §1.8 (Unison assessment), §2.3 (Zig `build.zig.zon` dependency-hash cascade, "the manifest is the lockfile"), §2.4 (Inko `require URL VERSION CHECKSUM`).

**Locks in.** H6(b); G3.5's content-hash validation.

## B. Types

### B1 — Type foundation: hybrid, nominal tags composing set-theoretically

**Constraints.** The author's set-theoretic taste (charter philosophy) and the README's set instructions (`A = B | C`, `B & C`, `B - C`); compile-time errors over runtime errors; the unboxed typed-register VM baseline where every slot's type is a compile-time constant (research/03 exec 6); error handling (B5/E4) and linearity (`~Copyable` as a negated constraint) both consume whatever algebra exists here.

**Why the winner wins.** (c) keeps the README's surface as ordinary type expressions over nominal atoms, grounds exhaustiveness in tag identity (enum-cover difficulty — the most-implemented checker in the survey, research/04 §4), keeps every union at the unboxed tag+payload cost of research/03, and gives `~Copyable` a native home as set negation composing with unions (`File[Open] | File[Closed]`). It has the strongest production-adjacent evidence: MLstruct (OOPSLA 2022) is exactly this model with principal inference claimed, and TypeScript's discriminated unions are its degraded production version.

**Why the others lose.** (a) full structural core has maximal expressiveness and the worst risk concentration: principal-type inference over the full Boolean algebra has no production compiler (research/05 §2.3), nominal encapsulation disappears, and ICs need shape hashing where nominal tags use one id. (b) nominal-only is the simplest checker and is recorded as the reversible fallback, but it deletes error-set arithmetic, the one-mechanism null/error/enum story (Phi), and forces Result wrappers everywhere — research/04 §12 Style D shows the sugar then spent compensating. (d) keeps the README syntax but its `any`-fallback converts every inference failure into dynamic typing (a charter violation) and difference types over non-tag operands compile matches to predicate chains — the one place set algebra stops being free on this VM.

**Evidence.** research/04 §3 (MLscript, Whiley, TS narrowing survey), §12 (Style D compensation ladder); research/05 §2.1-2.3 (semantic subtyping line, resolution as intersection subtyping); research/01 cross-cutting note 1 (copyability as a negated set constraint).

**Locks in.** B4(c), B5(d), E4(b) — the `T?`/`?`/`or`/`.err` surface exists only over unions; the disjointness checker becomes load-bearing for B6(a); the recorded fallback is B1(b) with E4(b) degrading to match-based handling.

### B2 — Inference: bidirectional skeleton with boundary annotations, plus flow refinement

**Constraints.** Explainability budget; scripting ergonomics (REPL, unannotated locals); the algebra of B1(c) must be checked at stable points; affine flow analysis prefers written boundary signatures (research/05 §7.1: the Hylo/Koka evidence all uses checked signatures).

**Why the winner wins.** The (a)+(c) combination makes the checker's job bounded: annotations anchor the set algebra at module boundaries; refinement only shrinks known types locally. Refinement is monotone, which is the structural reason it composes with intersections and negation without solver machinery (Whiley and TS both ship it; the only set-typed cost — overload-group emptiness — is quadratic in group size and cacheable, research/05 §2.2). Under the immutability default, the two documented hard cases of flow typing (mutation, closure capture of narrowed locals) mostly vanish (research/04 §3).

**Why the others lose.** (b) full HM without the algebra is clean but collapses B1 toward nominal-only; with the algebra it is the MLscript research program, whose principal-type result has no production compiler — a bet this project declines for v1. Unification errors over unions/negations are also an unsolved UX problem. (d) annotations everywhere is the Go/Zig posture — the two languages the author dislikes, and it kills REPL ergonomics, half the point of an interpreted language per the charter mission.

**Evidence.** research/04 §6 (annotation discipline: locals inferred, top-level required), §3 (flow typing survey, known limits); research/05 §2.3, §7.1.

**Locks in.** The `?` propagation check becomes a set containment against the written return union (research/04 §11); the affine checker gets fixed facts at every boundary. Known limit accepted: no narrowing through mutable fields of union type — rare under immutability, a real wall if `var` union fields are later demanded.

### B3 — Products and sums: nominal structs/enums plus anonymous tuples

**Constraints.** Exhaustiveness checker cost; VM layout stability (declared layouts, one type_id each — the optimal case of research/03); sugar budget; ergonomics for one-shot aggregates; the author's dislike of machinery that out-earns its sugar.

**Why the winner wins.** (d) gives declared layouts (best VM story), trivial exhaustiveness (cover over declared cases), low inference burden (nominal atoms unify, tuples are structural products), and tuples for ad-hoc aggregates; the anonymous record literal from the research/04 §2.5 keep-set recovers structural convenience as compiler-synthesized sugar over a nominal record — and that sugar is deferred until the inference story settles, so the v1 core pays nothing for it.

**Why the others lose.** (a) anonymous structural records make every literal's layout shape-derived and ICs shape-hashed, and two libraries' same-shape types unify by design (the TS glue-world property, fine for TS, a liability for a language with nominal encapsulation). (c) rows are the most machinery per unit of sugar in the whole catalog: row unification leaks variables into diagnostics, open variants break exhaustive matching, and row-typed parameters have statically unknown layout (boxing or per-shape ICs, research/05 §6.3). Koka pays that cost for effects where rows are unavoidable; Gleam, Roc, and MoonBit all declined them for data. (b) nominal-only loses ad-hoc gluing, which (d)'s tuples recover.

**Evidence.** research/04 §2.5 (keep-set), §14 / research/05 §5.1 (moonbit-core struct/tuple/enum evidence); research/05 §6.3 (IC keying).

**Locks in.** F7 row 25 (record sugar deferred); D3's subscripts operate over declared layouts; unboxed error returns get stable layouts (research/03's Result-in-registers).

### B4 — Nullability: `T?` as a zero-semantic alias for `T | none`

**Constraints.** The Phi thesis (research/04 §3): null, errors, and enums are one feature — unit members of unions with `is` narrowing and `or` extraction; the author's Kotlin/TS muscle memory; the sugar budget.

**Why the winner wins.** (c) is (a)'s exact semantics with a familiar spelling at the cost of one alias: `none` is a unit type among several, `T?` is `T | none`, and the same three tokens (`is`/`or`/match) serve null, errors, and enums. Under B1(c), `T | none` and `T | E` are literally the same construct, so the extraction vocabulary is stated once (research/04 §11: three tokens versus Kotlin's four and Swift's five).

**Why the others lose.** (b) language null makes "missing" a second mechanism beside error unions — exactly the three-features-where-one-suffices structure Phi documents — and drags in the elvis/`!!`/`?.` spellings (four Kotlin sugar features). It also breaks B5's unification where `?` and `or` operate identically on `T | none` and `T | E`. (a) pure unions are the same semantics with a less familiar spelling; the alias costs nothing, so refusing it buys nothing.

**Evidence.** research/04 §3 (Phi mechanism, the `V | none` flattening limit — an explicit `Found[V] | none` wrapper at ambiguous sites, the same limit TypeScript has), §11 (token accounting).

**Locks in.** Niche packing of `T | none` into one slot remains a legal later optimizer pass (references are never zero); `?.` stays out of v1, addable as a thin desugar only if real code demands.

### B5 — Error carrier in signatures: union members now, rows as later sugar

**Constraints.** Errors must stay storable data (nursery collection, supervision, caching — research/02 §7.8); propagation must be cheap in a register VM; the path to full effects must remain additive; the monadic-extraction taste (no wrapper vocabulary in every signature).

**Why the winner wins.** (d) starts from the representation identity the research establishes: `T | E` in the return type and an `exn` row carry identical information (Koka's denotational table makes it literal, research/02 §7.8). Union-carried errors keep errors as data, cost one predicted branch (research/03 exec 6), compose by union arithmetic (Roc's automatic combining), and leave `throws(...)` as a v2 spelling that desugars to the same union — a compatible evolution, a redesign it never becomes.

**Why the others lose.** (b) Result wrappers re-introduce nesting (`Result<Option<T>>`), put Ok/Err vocabulary in every fallible signature, and still need `?`-style propagation sugar — Rust added it — so it is (a)'s mechanics plus a wrapper. (c) rows require the full E1a inference machinery even when only errors use it, and errors stop being values a nursery can collect without an Effekt-style reification step. (a) alone (annotation-optional inferred unions) is the same base as (d) without the stated effect-ready bridge; (d) keeps it and names the seam.

**Evidence.** research/02 §7.8 (the interdefinability verdict and recommendation), §7.7 (one-branch cost); research/04 §11 (the surface already effect-ready); Effekt's `result`/`value` pair (research/02 §2).

**Locks in.** E4(b); the later `throws` sugar must desugar losslessly, so v1 propagation uses the Zig containment rule (no implicit conversions).

### B6 — Polymorphism: coherent set-theoretic traits with disjoint instance heads

**Constraints.** The author's anti-specialization position (charter philosophy: generic instances must not overlap, removing priority rules entirely); B1(c)'s algebra as the resolution substrate; linear friendliness; bytecode-native execution; coherence without the orphan rule's known costs.

**Why the winner wins.** (a) replaces both the orphan rule and specialization with one decidable computation: for every pair of heads of one trait, prove `H1 & H2 = Empty` at declaration and again over the final instance environment at link. Resolution is then intersection subtyping via modus ponens (OOPSLA 2020), uniqueness follows from the algebra, and no priority machinery ever exists — dispatch is determined by types and the set algebra, the charter's invariant verbatim. Foreign-type instances are legal, deleting Rust's newtype wrappers and blanket-impl vetoes (research/05 §3.2). Instance heads being tag sets makes the inline caches of H3(a) tie-break-free. Evidence covers every axis: Rust traits' shape, MLstruct's compatibility, .NET's invisible per-value-type stenciling as the runtime precedent.

**Why the others lose.** (b) explicit dictionary passing is the best linear fit and is kept as the escape hatch, but its boilerplate wall is why Scala built implicit search; its `_` sugar quietly re-imports the resolution it deleted. (c) ML modules strain the sugar ceiling directly (signatures, applicative vs generative functors, first-class packing) and make every generic container a two-step instantiation — OCaml grew ppx codegen around exactly that friction. (d) comptime generics escape the set algebra (generated heads can intersect, discovered after generation), require linear checking of generated code, and pay instantiation fan-out at startup; research/05 §9's verdict is to adopt its reflection half (G1b) and reject its generics half.

**Evidence.** research/05 §2.2 (resolution as intersection subtyping; the pairwise emptiness cost, quadratic in group size, cacheable), §3.1-3.5 (specialization and coherence costs), §6.3-6.4 (evidence words and ICs), §9 (the scoring table: (a) is the only option satisfying all four hard constraints simultaneously).

**Locks in.** H3(a)'s evidence ABI; the disjointness checker is load-bearing and is the named schedule risk of B1(c); scoped named instances (b-lite) deferred additive with ambiguity always an error.

### B7 — Numeric tower: i64/f64 defaults, trapping overflow, explicit wrapping

**Constraints.** Unboxed slots are the interpreter's dividend (research/03 exec 6 — boxing is the 2-10x term static typing exists to delete); overflow policy must be predictable under interpretation (research/04 §8: trapping is the only such policy); zero checks on the arithmetic happy path (research/03's ADDOV note recommends panic over bigint-fallback guards).

**Why the winner wins.** (a) puts i64/f64 raw in slots (`ADDI64` is one machine add with no tag work), makes overflow a deterministic trap — reproducible, debuggable, one cold overflow edge the copy-and-patch tier folds — and gives hash/PRNG code the explicit `+%` family. Debug and release behave identically, which Rust's debug-panic/release-wrap split does not deliver. Suffixes cover the rare width needs; the tower can grow later behind the numeric-trait seam.

**Why the others lose.** (b) bigint default either boxes every Int (deleting the unboxed-slot design wholesale) or runs a dual small/bignum representation whose overflow transition is exactly the guard class research/03 rejects — and JIT type stability degrades Julia-style at the small/big boundary. (c) the full tower costs width-ambiguity rules, evidence words on every polymorphic numeric call until ICs warm, and software f16/f128 paths, for a scripting sweet spot that rarely wants them. (d) is (a) plus a plan; its decimal half is adopted as the library path (Roc's `Dec` makes `0.1 + 0.2 == 0.3` true by construction), so (d) as a whole adds nothing more to decide.

**Evidence.** research/04 §8 (the overflow-policy survey and recommendation); research/03 §b.10, §b.2 (interpreter-vs-C gap decomposition, ADDOV guidance); research/05 §5.1 (Roc `Dec`).

**Locks in.** Division by zero and shift-width overflow trap; error-union-returning arithmetic variants are library functions typed `T | ArithError`; B9's float-equality choice interacts (bit-structural `==` over raw f64).

### B8 — Strings: immutable UTF-8 with checked slices

**Constraints.** Honest equality and hashing; ASCII density (config/log text is the common case); the one-raw-pointer slot representation of research/03; grapheme honesty without slowing the core; FFI conversion is allowed at the boundary (owned by 06).

**Why the winner wins.** (a) is research/04 §7's recommendation verbatim: UTF-8 bytes behind `{type_id, rc, byte_len, bytes}` (Inko's single-allocation layout precedent), slices that must land on char boundaries via a checked API returning `String | none` (reusing B4's union discipline instead of panicking), `chars` iteration, graphemes in stdlib. Equality is O(n) with length fast-reject; memory is 1x for ASCII-heavy text.

**Why the others lose.** (b) interning buys O(1) equality after intern but makes creation pay a synchronized global-table lookup, makes slices either re-intern (O(k)) or lose interning (two equality regimes), retains memory in the table (Lua's known behavior), and shards badly across cores. (c) UTF-16 doubles ASCII memory and puts surrogate-pair logic on every scalar operation — its O(1) indexing is O(1) footguns, since correct character access still needs pair-aware logic. (d) is (a) plus a rope library type; adopted as the future add-on when an editor workload appears.

**Evidence.** research/04 §7 (the strings survey and recommendation); research/03 §b.4 (string layout, slice-as-(ptr,len) slot pairs).

**Locks in.** Slices are non-owning borrows kept alive by the parent's RC — the borrowed-operand shape B9/C2 already prescribe; H4's non-moving heap is what makes the raw pointer safe.

### B9 — Equality: universal structural `==` with the five carve-outs

**Constraints.** Totality is load-bearing (reflexivity is what makes dict keys and dedup sound — research/05 §5.2 Break 2); value semantics under immutable-by-default make identity almost meaningless (Roc's justification, research/04 §9); comparison must never consume an affine operand; the no-boilerplate taste.

**Why the winner wins.** (a) is total by construction: function comparison is rejected statically (usually a bug — Elm's rule); floats compare bit-structurally so `nan == nan` holds and dicts stay sound, with `is_float_eq`/`is_approx_eq` named alongside for IEEE semantics; mutable cells and `~Copyable` resources compare by identity (avoiding OCaml's divergence-on-cycles); operands are borrowed, never consumed. It deletes MoonBit's hand-written per-arity tuple `Eq` impls (`builtin/tuple_eq.mbt`) and every `#[derive(Eq)]` line, and there is no `===`. Per-type custom semantics remain reachable by named methods (`equiv`-style), which is where they belong under the explainability budget.

**Why the others lose.** (b) trait-mediated equality spreads `T: Eq` bounds through every generic signature and re-imports the coherence questions the charter avoids; MoonBit's boilerplate is the documented cost even in a derive-rich language. (c) builtin-plus-derives collapses into (a) once the float choice is bit-structural; ordering stays derive-mediated in both. (d) reference identity fights the language's own value semantics, and Go's runtime panic on incomparable interface values is the evidence that identity-first is later-failing, simpler it is not.

**Evidence.** research/05 §5.1 (Roc `is_eq` split, OCaml polymorphic compare, MoonBit derives, Kotlin/Swift synthesis), §5.2 (the five totality breaks), §5.3 (the verdict); research/04 §9; research/05 §1.9 (Go).

**Locks in.** `Ord`/`Hash`/`Show` are derives owned by 05; `==` is never user-overloadable (F5); hashing must be consistent with structural equality (the `Hash` derive's contract).

## C/D. Linearity, ownership, memory, data

### C1 — Linearity flavor: affine core plus an opt-in `~Copyable` linear layer

**Constraints.** Charter req 2 reads "static linear (or affine) typing, enforced at compile time" — affine is a legal reading; req 6 asks for low learning cost and low error rate while keeping the performance benefits; req 3 wants the memory payoff; the research consensus is "deriving beats writing for ergonomics, writing beats deriving for guarantees" (research/01 §a).

**Why the winner wins.** (b) splits the two halves: everyday code carries one rule — assignment of an affine value moves it, a moved binding is dead — which ordinary copyable values never observably hit; guarantees live on the opt-in `~Copyable` layer where writing them down is the point (double-use is a compile error; `deinit` runs exactly once; phantom states compose with B1(c) unions). The checker is a last-use/DI analysis — a single forward pass (Austral) or the abstract interpreter Hylo already runs. This directly answers the author's open ownership question: the machinery exists exactly where resources exist and nowhere else.

**Why the others lose.** (a) QTT puts a multiplicity on every binder; GHC's experience shows polymorphic multiplicities leaking UR constraints into ordinary signatures, and research/01 §c Table 3 rates the signature noise very high — a charter req 6 violation. (c) hidden linearity violates req 2 as written (it would need re-wording) and ships the hardest compiler pass first; its machinery is adopted as the staged optimization endgame instead (Roc and Koka both shipped runtime uniqueness checks first and inference second — research/01 §d combo 6). (d) runtime-checked borrows cap aliasing correctness at runtime panics — against the compile-time-errors philosophy; its borrow counter is adopted as the v0 diagnostic substrate, cheap and removable.

**Evidence.** research/01 §B.3 (Swift SE-0390, Hylo `Copyable: Movable`), §B.5 (Austral's single-pass checker), §B.6 (QTT/Granule/GHC costs), §B.8 (Inko's readable move errors), §B.12, §d combo 6 (the staging order).

**Locks in.** C2(d) is mandatory (moves without borrows still need a read-access answer); the natural pairing with C4(a); C5(c)'s deinit lives on this layer; E3's sendability falls out (a linear value is `sending` by construction).

### C2 — Borrowing: second-class parameter-passing modes

**Constraints.** without.boats' checklist (research/01 §B.4): moves, borrows, lifetimes, and drop are four semi-coupled features and the difficulty is the coupling — adopt the cheapest coupling that works; no lifetime annotations anywhere; UFCS call sites must stay mode-free.

**Why the winner wins.** (d) puts modes on declarations only (default borrow, `mut` exclusive, consume), so call sites carry zero syntax and `xs.push(1)` reads identically regardless of mode. Second-class borrows cannot escape the call, so checking is intraprocedural — no lifetimes exist, no aliasing analysis exists, escape analysis becomes a compile-time fact. Subscripts extend the same modes to element access, replacing getter/setter protocols. The rule set is a page.

**Why the others lose.** (a) moves-plus-explicit-copies is the clone() culture research/01 §B.4 documents — every read site becomes a transfer decision, and the workarounds all converge on dynamic checks. (b) inferred borrows deliver the near-C numbers (Koka/Roc/Lobster) but are an interprocedural fixpoint with no user-visible contract where inference fails (Roc issue 12420's observable perf effects), and shipping them first means the hardest pass before the first working interpreter — staged later as pure optimization. (c) a runtime borrow counter as the user-facing model makes aliasing bugs deterministic panics; kept as the v0 diagnostic layer under (d), where second-class borrows statically cannot dangle and the counter only ever catches FFI/unsafe bugs.

**Evidence.** research/01 §B.3 (Hylo `let`/`inout`/`sink`/`set`, borrow as IR-level projection), §B.4 (the coupling checklist and workaround catalog), §d combo 3 (the modes-not-lifetimes synthesis); Swift SE-0377's `borrowing`/`consuming` as the industrial convergence.

**Locks in.** C3(b) and D1(b) are the same mechanism with two more uses — one checker, three jobs; first-class references are permanently out, deliberately.

### C3 — Immutability mechanics: const/var plus exclusion windows, FBIP underneath

**Constraints.** Charter req 5: bindings immutable by default, mutation explicit and locally visible; value semantics need exclusive callee-side mutation (the shape Hylo, Swift, and Inko all provide); the non-feature both Hylo and Swift keep — no mutating an immutable binding (rebinding-immutability adds aliasing paradoxes for marginal sugar, research/01 §B.3).

**Why the winner wins.** (b) makes the `mut` parameter mode the window: one exclusive mutable access for the call, statically guaranteed, lexically visible as the call itself — so "locally visible" is a property of the syntax, a discipline it does not require. Inside the window there are zero runtime checks (alias-freeness is proven); outside it, everything is shared-immutable. (d)'s FBIP is layered underneath as the runtime guarantee (C4a delivers it), so functional updates also run in place when unique — the exact combination Koka ships (var locals + FBIP).

**Why the others lose.** (a) var-locals-only forbids the one mutation shape value semantics needs, pushing in-place algorithms into builder patterns. (c) interior mutability (Cell/RefCell) solves a problem windows leave unsolved — shared mutation behind immutable bindings — at the cost of runtime borrow checks and two load-bearing invariants (barrier-free RC over immutable data, elision of generation checks); with (b) chosen, the problem does not exist, so the escape hatch only weakens the discipline. (d) as the whole surface (no var at all) makes algorithmic code read as consume-return plumbing — the awkwardness FBIP/TRMC research exists to fix.

**Evidence.** research/01 §B.3 (Hylo `inout` windows, Swift `mutating` plus dynamic exclusivity as the mature-language safety net), §B.2 (Koka FBIP's guaranteed-reuse quote).

**Locks in.** The `var` keyword's meaning (rebindable local) composes with F2's `const`/`var`; D1(b) is this decision seen from the confinement side.

### C4 — Memory strategy: Perceus RC with block reuse, plus per-frame arenas

**Constraints.** Deterministic deinit (C5c) rules out tracing collection of resource-bearing values; zero pauses and zero barriers rule out write-barrier regimes; FFI pinning wants a non-moving heap; the runtime must be buildable by a small team (Inko's immortem is the evidence class); linearity makes escape a compile-time fact (research/03 §a.7).

**Why the winner wins.** (a)+(d) is research/03 §a.8's prescribed pairing verbatim: Perceus precise RC with reuse for boxed/shared values — frees immediate and cache-friendly, finalizers deterministic, allocation near-free when a dying block is reused — plus per-frame bump arenas for temporaries, sound precisely because linearity forbids silent escapes (allocation an add, reclamation a restore). Under C1(b), dup/drop placement degenerates to "incref at clone, decref at each unique death" because the linear checker supplies the usage counts (research/03 §b.6). Non-atomic counts are sound while values are thread-confined, which E3(b)'s fork-join ownership transfer maintains.

**Why the others lose.** (b) generational references double reference size (16-byte refs do not fit the unboxed register frame model) and convert a static guarantee into a dynamic access check; it is the named fallback if affine friction proves worse than expected. (c) tracing GC carries Inko's post-mortem — immix complex and hard to debug, then removed entirely in favor of ownership+ARC — plus safepoints, precise stack maps, and barriers in every tier, and a moving collector conflicts with raw-pointer strings, interior pointers, and FFI; it also wastes the linearity dividend (compile-time escape analysis) and the immutability dividend (no barriers needed). Cycles, the one genuine GC advantage, are handled by type-level rejection plus the H4(d) two-hook collector interface as the growth path.

**Evidence.** research/01 §B.2 (Perceus PLDI 2021, kklib hooks, FLR), §B.7 (Vale), §B.8 (Inko's GC removal arc); research/03 §a.7-8, §b.6 (reuse pairing, "no borrow analysis, no interference analysis").

**Locks in.** H4(a) implements it; the honest accounting (boxed/shared residue keeps paying RC; the reuse pass is optimizer work with a mimalloc fallback) is carried into staging.

### C5 — Move, drop, definite initialization: RAII deinit on the `~Copyable` layer

**Constraints.** Affine values need a defined death point; finalizers must run deterministically (research/01 §B.12 item 3); fiber abort/cancellation needs cleanup hooks (research/02 §b.8); a copyable type has no unique death point (SE-0390's rule).

**Why the winner wins.** (c) restricts `deinit` to `~Copyable` types, where it runs exactly once at the unique instance's death — on every path, including early return, panic, and cancellation teardown. Resource users write nothing; resource authors write one block. Deinit dispatch is a static call (the concrete type is known), and the DI/last-use machinery it needs is infrastructure every option already requires. This is also E5's cleanup story: teardown walks frames running linear destructors.

**Why the others lose.** (a) implicit-drop-only has no hook, so File/Socket cannot exist as safe types — it kills C1(b)'s resource layer, whose point is the hook. (b) universal explicit drops recreate Austral's consume-once-shapes-all-code cost, and manual drops under early returns and panics are exactly the bug class RAII exists to remove (Vale built higher-RAII for this reason). (d) handler-based cleanup couples the memory design to the effects schedule — the coupling the charter rejects — and defers all resources until effects exist; Koka ships deinit-style resources and handler combinators side by side, so (c) forecloses nothing.

**Evidence.** research/01 §B.3 (SE-0390 deinit semantics), §B.5 (Austral), §B.7 (Vale higher-RAII: "ownership guarantees a future call happens"); research/02 §b.8 (destructors as fiber-abort hooks).

**Locks in.** E5(a)+(b); the explicit `drop` builtin survives as a debug/FFI tool only.

### D1 — Mutation confinement: exclusion windows only

**Constraints.** Same as C3 (req 5), seen from the confinement side: where may mutation touch, and what enforces it.

**Why the winner wins.** (b) needs zero additional mechanism — the `mut` window is the confinement. Mutation is always a named call on an owned receiver, statically exclusive, lexically visible; shared mutation is unrepresentable by construction rather than by discipline.

**Why the others lose.** (a) function-local var only spends real expressiveness (no mutating methods, no subscript setters) to buy no safety over (b) — windows are already statically exclusive. (c) capability gating audits who may mutate but not which alias is exclusive, so (b) is required underneath regardless; (c) can layer on later if E1c-style auditing is wanted (the graduation path research/02 §d combo 2 describes). (d) a pure surface is the right runtime behavior (C4a delivers it via reuse) and the wrong sole channel — Koka itself keeps var locals beside FBIP.

**Evidence.** research/01 §d combo 3 (one mechanism, three uses), §B.3.

**Locks in.** Nothing new — this row records that C2(d)/C3(b) close it.

### D2 — Collections: linear-mutable COW with a functional API

**Constraints.** "Immutable by default" (req 5) and "performance approaching compiled languages" (req 3) must both hold for data-heavy scripts; research/03 §b.9's asymmetry is decisive: persistent update allocates O(log n) RC'd nodes, affine in-place update allocates zero.

**Why the winner wins.** (b) is the single semantics under which both charter clauses are true: updates are copy-on-write at the API level and overwrite in place iff the value is uniquely owned — guaranteed and observable (Koka's FBIP contract; Roc's Borrow/Consume/COW builtin contracts). When shared, copying happens and surfaces as a perf note, an error it never becomes. Persistent HAMTs remain ordinary library types on the same RC runtime; `Bytes`/`Vec` are the unboxed unique corner of the same model, which is FFI-grade.

**Why the others lose.** (a) persistent-only forfeits zero-copy buffers and penalizes the common case with node churn — the exact asymmetry research/03 flags. (c) dual-mode doubles the stdlib surface and makes the switch point a teaching topic; (b) already degrades gracefully. (d) the small/big classification is an outcome of (b) (small values copy by value semantics; big buffers are unique unboxed types), a rule it does not need to be.

**Evidence.** research/03 §b.9; research/01 §B.2 (FBIP guarantee quote), §B.1 (Roc ownership contracts, issue 12420's observable flip side).

**Locks in.** C4(a)'s reuse pass is what delivers the promise; aliasing-means-copying is documented behavior, an error it never becomes.

### D3 — Iterators: combinators over a hidden cursor protocol, compiler fusion

**Constraints.** The 25x number (MoonBit's compiler-level fusion over a zero-cost `Iter`, research/03 §a.10) shows what fusion buys; multi-shot resumption must not capture linear values (research/02 §b.8); library authors need a way to define custom iteration; the sugar budget.

**Why the winner wins.** (d) gives users the surface they already know (map/filter/reduce chains, `for` loops), the compiler a fusible form (typed-SSA directional rules), and stdlib authors a hidden protocol (the linear cursor, bounds-check-eliminable by linearity, research/03 §b.7). Non-fusable shapes fall back to the same cursor semantics, so behavior is uniform whether or not fusion fires. The hidden layer can later upgrade to fibers (if E2b lands them) with zero surface change — the "hidden mechanism, stable surface" staging the whole design leans on.

**Why the others lose.** (a) linear cursors as the public protocol work but duplicate what `for` + combinators hide, and their second-class borrows cannot be stored (a cursor over a mutably-borrowed structure cannot live inside another structure). (b) first-class generators pay a fiber switch per element where fusion pays none (the 25x gap measures exactly that), need the linear-capture typing rule on day one, and couple this catalog to E2's schedule. (c) combinators-only provides no protocol for library-defined iteration — irregular walks fall back to manual loops.

**Evidence.** research/03 §a.10, §b.7 (fusion and cursor discipline); research/02 §b.8 (the multi-shot × linear constraint); research/01 §B.2 (TRMC as the recursion-side analogue).

**Locks in.** The `-*>'` generator arrow stays retired; the v0→fiber upgrade path is permitted, staged.

## E. Errors, effects, async, parallelism

### E1 — Effect system: deferred surface, handler-shaped runtime, capability idiom

**Constraints.** Charter req 7: adopt effects only if they earn their complexity. The research decomposition (research/02 §5.8): the four benefits of async/effects — IO latency without thread explosion, composable cancellation, scheduling abstraction, signature-level audit — none strictly requires row syntax; every winning design contains the color in exactly one place.

**Why the winner wins.** (d) pays nothing at the surface in v1 (nothing to learn, no row vocabulary, no inference-error UX) while building the machinery that makes full effects additive later: the runtime is handler-shaped from day one, with evidence passing serving the two internal effects (`cancel`, `discontinue`) at nursery boundaries only — cost concentrated where it is already needed. (c)'s capability idiom becomes the stdlib convention (Zig 0.16's `Io` parameter is the shipping precedent): ambient default handler for scripts, explicit capability for tests, embedding, and scheduler selection — the same code over blocking, evented, or custom schedulers.

**Why the others lose.** (a) full rows put a row variable in every combinator signature users write; row inference unified with linear/affine unification has thin prior art (research/02 combo 3); OCaml 5's handlers are still experimental after four years — the typing/API story stalls, the runtime does not. (b) abilities-lite is cheap but permanently coarse (cannot distinguish file from network in types) and forecloses user resumable abstractions. (c) alone leaves the audit implicit-only and the scheduling abstraction a convention without a substrate; as the sole choice it also makes "pass the io" noise the default posture — the reason (d)+(c)-idiom is the ordered pair the research itself ranks (B over A).

**Evidence.** research/02 §d (the three-part answer and combo 6), §1 (Koka evidence passing), §5.1 (Zig's Io capability), §8 (VM cost analysis: one indirect call per effectful op, tail-resumptive ops allocate nothing), exec item 18.

**Locks in.** E2(b) is the runtime's first client; B5(d)'s v2 `throws` sugar rides the same seam; two standing typing obligations are recorded (affine resumptions; no linear capture in multi-shot continuations) for whenever v2 activates.

### E2 — Async: VM fibers with a mandatory nursery

**Constraints.** The author's decoupling philosophy names function coloring as the canonical entangled feature (charter; the "What Color is Your Function?" essay is cited in the author's own writing); cancellation must compose; the interpreter must stay fast; TCO must survive.

**Why the winner wins.** (b) moves the color into the runtime: heap fibers are interpreter data (a growable frame array; Lua `CallInfo` precedent), spawn is one allocation, switch is a pointer swap, O(1) — so blocking-style code needs no state machines, no Pin analog, no duplicated APIs. The mandatory nursery imports Trio's semantics with Kotlin's Job-tree adoption record: task lifetime is lexically scoped, first failure cancels siblings, errors union out at the boundary — leaks and lost errors become structurally impossible. Preemption is a flag check in the dispatch loop every N instructions, strictly simpler than Go's signal scheme. TCO is per-fiber and unaffected; a tail-looping spawned fiber never grows its segment.

**Why the others lose.** (a) async-as-effect is the most elegant spelling and needs E1a's row inference in every combinator first. (c) unstructured spawn is colorless without structure: orphaned tasks, `context.Context` threading, lost errors are the documented production failure modes (the fasterthanli.m evidence the author's own links reference), and a nursery cannot be retrofitted without breaking every detach-assuming API. (d) colored await keeps the two-world stdlib, viral `await`, and drop-cancellation footguns; Rust's Pin/Send-bound history shows coloring plus ownership exceeding a tolerable learning budget together (research/02 §5.6) — the author's two open questions answered wrong simultaneously.

**Evidence.** research/02 §5.5 (Trio), §5.4 (Kotlin Jobs), §5.2 (Go's flaws), §5.6 (Rust's colored+ownership cost), §8 (fiber economics), Table 1 and combo 1 ("Go minus its fatal flaw").

**Locks in.** E5(a) is the same mechanism from the resource side; E3(b) shares the pool; bare detach requires an explicit root nursery.

### E3 — Parallelism: fork-join on the sendability substrate

**Constraints.** Charter req 3 must hold for multi-core compute; the type system already proves race-freedom (no aliased mutability) and sendability (linear = movable, immutable = shareable) as theorems — research/02 §7.9's list shows the missing pieces are only a scheduler, cancellation scoping, supervision.

**Why the winner wins.** (b)-on-(a) buys exactly the missing piece: `par_map`/`par_reduce` fork-join combinators over immutable or linear-partitioned inputs — deterministic, replayable, lock-free by theorem, with Perceus static counts making fork-side snapshots cheap (research/03 exec 8). Learning cost is two combinators; the sendability rules are two sentences versus Pony's six-capability lattice (which exists precisely because Pony lacks linear types).

**Why the others lose.** (c) actors/isolation need mailboxes, process scheduling, possibly per-process heaps — the biggest runtime addition — and are weakest for fine-grained data parallelism (research/02 combo 4); linear move-based messages remove BEAM's copying cost where it hurts, which is why an actor layer can be added later as a library (a supervisor is a nursery with a restart policy). (d) single-thread plus FFI caps in-language compute at one core — a direct charter req 3 conflict; the coloring problem is evicted to the FFI boundary, solved it is not.

**Evidence.** research/02 §7.9 (the free-consequences list), Table 4, §5.7 (Swift SE-0414/SE-0430 as the retrofit evidence that linear/immutable already give what isolation systems rebuild), §6 (BEAM/Pony/Inko).

**Locks in.** Non-atomic RC stays sound via ownership transfer at fork/join; E4(d) supervision remains a library candidate.

### E4 — Error handling: union errors with `?`/`or`/`.err`, plus an unhandleable trap

**Constraints.** The 92% datum (catastrophic failures overwhelmingly come from mishandled non-fatal errors, Yuan et al. via research/02 §7) demands typed handling of expected failures; bugs need a loud, unhandleable channel; the sugar budget; errors must remain storable for nurseries.

**Why the winner wins.** (b)+(a) split the two failure kinds: expected errors are union members — flat (no `Result<Option<T>>` nesting), storable (nursery collection), handled by three tokens whose semantics are stated once over unions (`?` propagates with a checked containment rule; `or` extracts the first wanted member with a lazy fallback; `.err(h)`/`.ok(h)` are set-difference-typed, so the remainder type shows what is left). The old README's `.err` survives here, repaired: the expression is typed once and the handler does the work via set difference, fixing the non-compositionality research/04 §11 documents. Bugs `panic`: fiber abort, linear deinits run, the trap surfaces at the nursery boundary — near-free in this VM.

**Why the others lose.** (a) alone (Result-as-data with the trap) is the same runtime mechanics with wrapper vocabulary Rust's ladder shows (`Some`/`Ok`/`unwrap_or_else` noise — research/04 §12 Style D). (c) exceptions-as-effects need E1a machinery and un-store the errors (no caching, no nursery collection without reification). (d) supervision is a layer over any of these, a rival it is not; adoptable as a library whenever long-running systems need it.

**Evidence.** research/04 §11 (the surface survey, end-to-end example, and the three-flaw audit of the historical idea); research/02 §7 (the 92% datum, Rust/Zig/Swift/Inko convergence), §7.6 (flat-union precedent: Ceylon, TS, Zig error sets, Kotlin elvis).

**Locks in.** B5(d); the token set is finalized (08 resolved question 2); Rust's `From` conversion inside `?` is rejected in favor of Zig's containment rule.

### E5 — Cancellation and resources: nursery-scoped cancellation plus linear consume-to-close

**Constraints.** Linearity gives neither cancellation semantics (who cancels whom) nor the guarantee that cleanup runs at teardown — exactly the two gaps research/02 Table 4 names; a linear consume in flight must never be interrupted mid-operation; cancellation must not mean thread interruption.

**Why the winner wins.** (a)+(b) cover the two gaps with one pairing: cancellation is cooperative and nursery-scoped, observed only at await/preemption points (a flag check), so a consume either completes or its deinit runs at teardown — the frame-walk that runs linear destructors (research/02 §7.9(4), §8). Closing by consumption is a static move: no drop flags, no GC finalization, leak-freedom by construction for in-language handles.

**Why the others lose.** (c) defer blocks are discipline where (b) is guarantee — forgetting the defer is the hole consume-to-close closes; kept as scope-exit sugar, with the rule that it runs at teardown under cancellation (Koka's `finally` semantics, required because resumption-conditioned cleanup is wrong under multi-shot). (d) timeouts-as-primitive freeze one timer abstraction into the spec; the handler/combinator shape (`firstof(timer, action)`, Koka's library-only `timeout`) composes, nests, and teaches that these are library code.

**Evidence.** research/02 §7.9 (the missing-pieces column), §8 (teardown walk mechanics), §5.5 (Trio cancellation scoping), §1 (Koka `linear effect` + `finally` soundness evidence).

**Locks in.** C5(c) supplies the destructors; E2(b) supplies the scope; the FFI pinning rule (08 resolved question 1) completes the boundary story.

## F. Syntax

### F1 — Global shape: TS-family braces

**Constraints.** Author taste: TS+bun daily surface, loved; the historical README's own function form is TS-family; parser and tooling simplicity (LSP-first author); the no-semicolon newline rule rides any brace family.

**Why the winner wins.** (a) is the least novel surface of the four demonstrated styles (research/04 §12): every block opens with a visible token, the grammar is line-oriented with no layout state, and TS tooling intuitions transfer. It is also the README's own form, so the historical corpus reads as almost-correct DreamScript.

**Why the others lose.** (b) Gleam/Roc braces are the strongest alternative (`fn name(` is the best grep anchor; every construct a trivial AST node) and are the recorded fallback if the arrow-assignment grammar fights the parser — but their `fn`/`let`/`|x|` sigils buy no TS transfer. (c) significant indentation requires whitespace-aware infrastructure in parser, formatter, REPL, and error recovery simultaneously, and carries Python/Nim's permanent copy-paste and refactoring sensitivity; Koka is the lone healthy instance and it pays for the tooling. (d) Rust-family is familiar and fights the foundation: research/04 §12's verdict is that its sugar is spent compensating for missing set types (the `Result`/`Option` wrappers F1.d re-imports are what B1/B4 exist to delete).

**Evidence.** research/04 §12 (the four-style demonstration), §6 (the indentation survey), §13.

**Locks in.** F2's const-arrow declaration; the block rule (newlines, no semicolons, trailing commas).

### F2 — Bindings, functions, annotations, returns

**Constraints.** README prior art (`const f = (a: int) -> int`, the prefix-`?` return marker); functions are values (README stance); the `?` token is claimed by E4's postfix propagation; expression orientation (if/match are expressions).

**Why the winner wins.** `const`/`var` is the README's word, TS habit, and a five-letter promise (immutability is the default spelling); `val`/`var` rhymes but breaks TS transfer, `let`/`let mut` adds a modifier nothing else needs. Const-arrow declarations keep one form for bindings and functions (uniform with functions-are-values), with the name in scope in its own body for recursion. `name: T` plus `-> T` gives one colon meaning "has type" and one arrow meaning "produces", matching the function-type arrow. Final-expression returns make blocks values; `or return`/`or break`/`or continue` reuse E4's `or` as early-exit sugar rather than a new keyword.

**Why the others lose.** On returns specifically, the README's prefix `?` collides with postfix `?` propagation — one token, two unrelated meanings, disambiguated only by position; machines parse that, humans and error messages do not. The resolution (i) retires the marker and reserves `?` for propagation; a bare `return` keyword remains addable later as a pure S1 if real code demands (it forecloses nothing). Declaration alternatives (`fn` keyword, `fun`/`fn` split, allowing both) each add a keyword or a style schism the budget declines; the grep-anchor concern is met by `const name = (` being perfectly greppable as a definition pattern.

**Evidence.** research/04 §6 (the binding/keyword surveys), §11 (the token-collision flag and resolution options).

**Locks in.** F1(a); B2's boundary-annotation rule (top-level signatures required); generic-parameter spelling is owned by 05.

### F3 — UFCS and pipelines: full UFCS, pipe deferred

**Constraints.** The author's appreciation of Koka's UFCS; the sugar budget (Kotlin spends four features on extension functions/properties that UFCS deletes); resolution must never become a priority search (anti-specialization); E1c's capability parameters must not be swallowed by dot-call.

**Why the winner wins.** (a) is one desugaring rule (`x.f(a)` = `f(x, a)`) with zero runtime cost and no candidate search, proven in three production languages (Koka, D, Nim) with no confusion reports; research/04 §5's verdict. Resolution is fixed and simple: free functions first, then trait methods, `Trait::f(x)` qualification as the escape hatch, ambiguity an error. The capability rule keeps the audit honest: capability parameters are named and excluded from the dot position, so `println(io, msg)` can never be written `msg.println()` with `io` hidden.

**Why the others lose.** (b) `|>` is the same desugaring as a second chaining idiom — Roc deleted its pipeline as redundant with method chaining plus `?`, which is why it is deferred (S1, additive) rather than included. (c) methods-only re-imports extension machinery through the trait back door and pays Rust's auto-ref/deref candidate search, a hidden S3. (d) pipe-first without UFCS deletes dot-chaining sugar entirely and curries artifacts it is.

**Evidence.** research/04 §5 (the four-way comparison, the Roc deletion datum, the Koka-verified semantics).

**Locks in.** The dot position's meaning; Kotlin items 13-15 (infix, extensions) are deleted by this row.

### F4 — Pattern matching: Rust-shaped arms with set-difference diagnostics

**Constraints.** Exhaustiveness is a set-cover computation under B1(c) — the diagnostic can literally be the missing set; guards prevent Gleam's arm-nesting inflation; the sugar budget; linear match ergonomics.

**Why the winner wins.** (a) takes guards (`pat if cond`, the same feature as Kotlin 2.1's stabilized guards), or-patterns, destructuring, and exhaustive checking whose failure message names the missing members as a set ("missing: NotFound | Locked") — under B1(c) that diagnostic is a set computation, which no surveyed language matches because none has the algebra (research/04 §4). If B1 ever retreats to nominal-only, the same checker degrades gracefully to naming missing enum cases.

**Why the others lose.** (b) Kotlin's `when` is the same power in a second keyword with `->` arms against the baseline's `=>`. (c) Gleam-minimal (guardless) is the documented cautionary tale: flat decision tables inflate into nested `if` inside arms. (d) OCaml's `when`/`as` differs from (a) by word choice alone; `as` reads longer than a binder. The `@` binder itself is excluded from v1: the commonest pattern needing it (`u is User && u.age >= 18`) is covered by `is` narrowing on the original variable, and the remaining use — re-binding a moved-out whole while destructuring — has affine semantics that deserve their own design; deferred additive.

**Evidence.** research/04 §4 (the exhaustiveness survey and set-difference diagnostics), §2.2 (Kotlin items), §6 (the Gleam cautionary tale).

**Locks in.** Ledger rows 8-10; the old README's `.err`/`.ok` salvage lives beside this as the non-match handler path.

### F5 — Operators: fixed trait-driven set, frozen precedence

**Constraints.** The author's constraints "pick Rust's slot" (research/04 §10); explainability (precedence must be learnable once, forever); the anti-priority stance applies to operator resolution too; overflow spelling comes from B7(a).

**Why the winner wins.** (a) maps a fixed token set to fixed traits — an operator call is a trait-method call, inline-cacheable, with one frozen C-family precedence table published in the tutorial and never extended. `==` is B9's structural equality, never user-overloadable; ordering goes through the `Ord` derive. The `+%`/`-%`/`*%` family gives hash/PRNG code its explicit wrapping; division by zero and shift-width overflow trap.

**Why the others lose.** (c) free overloading plus custom operators is S3 and makes precedence declaration-local, breaking grep-ability and the explainability budget — likely-rejected under the ceiling. (b) no overloading hurts exactly where the language grows (BigInt, vectors, decimals read `x.add(y)`), and the numeric tower can later expand behind the same trait seam. (d) comptime-generated by-name arithmetic is F5(a)'s future helper, a rival it is not.

**Evidence.** research/04 §10 (the operator survey), §8 (overflow table); B7's trapping policy.

**Locks in.** Ledger row 21; `Deref`-style coercions are excluded from user surface alongside.

### F6 — Strings and literals: Kotlin interpolation with an explicit boundary

**Constraints.** Interpolation must not hide allocation (charter's compile-time-errors taste extends to cost visibility); raw strings, multiline, and heredoc should be one construct; Char and byte literals for text and wire work.

**Why the winner wins.** (a) is research/04 §7's winner on every axis except implicit stringification — which the explicit `str()` boundary fixes (Roc's rule): `"${name.str().upper()}"` shows the conversion and its cost. `$x` for bare identifiers and `${expr}` for expressions cover both habits; `r#"..."#` hash-counted raw strings nest to any depth and embed any quotes (Rust evidence; Go backticks cannot embed backticks; Zig's `\\` line form is weakest); `'x'`/`b'x'` match Rust's Char/byte pair.

**Why the others lose.** (b) `\(x)` collides visually with tuple syntax and pays two characters per interpolation. (c) format-function placeholders move interpolation out of the literal into a call and invite a format-spec mini-language — stringly-typed unless checked, against the charter. (d) concat-only is the S0 option that deletes literal readability.

**Evidence.** research/04 §7 (the survey, the raw-string comparison, the boundary rule).

**Locks in.** `str()` is the Show-family projection named by 05; `T?`-valued interpolations still require explicit handling — no silent optional printing.

### F7 — Sugar budget: Balanced tier

**Constraints.** Charter req 4: at most Kotlin's total sugar surface (38 features / 74-76 cost units per research/04 §2); the recommended budget is about 20 features / 32 units; the ledger must cover every construct F1-F6 use.

**Why the winner wins.** Balanced (22 rows / 35 units) covers every construct the decided syntax uses, lands at roughly half of Kotlin's feature count and 43 percent of its weighted cost, and still leaves 16 features / 40-42 units of headroom — deliberate reserve for the deferred-additive queue (`|>`, record sugar, `@` binders, vararg). The two-feature overage versus the letter of research/04 §1's recommendation comes from pricing guards, or-patterns, and `.err`/`.ok` as their own rows; any two share one tutorial paragraph each, so the letter is recoverable by merging.

**Why the others lose.** Minimal (12/18u) drops `.err`, or-patterns, destructuring, named defaults, trailing lambdas, and derives — several of which the decided error story and 05's derive story load-bear. Generous (26/40u) spends the reserve now, on `it`, loop labels, and features the deferred queue would rather hold.

**Evidence.** research/04 §2 (the ceiling and cost scale), §2.5 (the keep-set and the Kotlin reconciliation table — items 4-8 and 27 deleted wholesale by set typing).

**Locks in.** The deferred queue's order; the Kotlin-reconciliation table is the canonical "what replaced what" reference.

## G. Polymorphism, metaprogramming, modules, ecosystem

### G1 — Metaprogramming: comptime evaluation plus static reflection, no macros

**Constraints.** The minimal sufficient set (research/05 §4.3): static reflection over type structure, compile-time iteration/branching over it, compile-time evaluation of ordinary code, and built-in derives for hot cases — Zig demonstrates the first three suffice for arbitrary-struct JSON; nothing surveyed needs token rewriting; the explainability budget ("fully explainable in a short tutorial").

**Why the winner wins.** (b) covers the minimal set with the meta-language equal to the object language: no substitution exists, so the hygiene question is deleted structurally, a stylistic point it is not. C++26 P2996's convergence ("reflection, not rewriting") is the industry arriving at the same conclusion. Built-in derives ship on top so hot cases are one word and compile fast.

**Why the others lose.** (c) hygienic macros buy syntax sugar the budget does not pay for and re-implement typing inside the macro system (Typed Racket's burden) or defer errors to post-expansion tokens (Rust's); typed member generation — the actual derive need — is why Rust derives are proc macros, macro_rules they are not. (d) proc macros are unhygienic by construction, need a compiler-as-library boundary with a separate build step, and make "what code runs?" unanswerable without knowing each expansion — the single largest explainability cost available; macro-generated impls can also re-open the disjointness hole B6(a) closed. (a) nothing-beyond-derives caps the ecosystem at the compiler team's shipping schedule and pushes needs to out-of-band generators with their parse/regenerate fragility.

**Evidence.** research/05 §4.1 (the spectrum), §4.2 (hygiene costs made concrete), §4.3 (the minimal sufficient set), §1.8 (C++26 P2996).

**Locks in.** Staging: derives first, evaluator before 1.0; G2(b) is its reflection engine.

### G2 — First-class types: comptime-only reflection over tags-as-data

**Constraints.** The unboxed-slot baseline (research/03 exec 6) forbids per-slot type words; compile-time errors over runtime errors; runtime type questions should be answered by machinery the data already carries.

**Why the winner wins.** (b)+(d) split the question at the phase boundary: compile-time type questions go to reflection (G1b's engine, zero runtime cost); runtime type questions are answered by the tags B1(c) values already carry — `is` and match are the type test, the IC key, and the exhaustiveness subject, one mechanism. Adding a type-object layer on top would duplicate the tag system with a second, slower one.

**Why the others lose.** (a) types-as-values either tags every slot (contradicting exec 6) or boxes the dynamic path only (then it is (d) plus comptime for the static half); Julia's own evidence says dispatch was never the bottleneck — inference failure was (the LU case study), and a statically-typed language re-importing runtime type values re-imports the type-instability cliff it never had. (c) metadata tables tax every allocation, pressure uniform boxed layouts, and move reflection errors to runtime (Go's `reflect.DeepEqual` economics).

**Evidence.** research/05 §4.3, §1.10, §2.4 (the Julia dispatch study, arXiv 1808.03370), §6.2; research/03 exec 6.

**Locks in.** Existentials remain available as evidence-carrying boxes (05 §8.1) for heterogeneous collections — the one justified boxing site.

### H3 — Generics execution: one body, evidence words, inline caches, lazy JIT specialization

**Constraints.** The anti-specialization invariant, stated as the ruling (research/05 §6.4, research/03 §8): dispatch is determined by types and the set algebra, never by declaration order or priority; the runtime may cache and specialize any dispatch that is already unique. Startup must not pay the instantiation fan-out; generic values must not box.

**Why the winner wins.** (a) satisfies the invariant by construction: which code runs is fixed before any caching (the unique disjoint instance of B6a), the inline cache is a perf hint with no correctness role (a stale or cold cache falls back to lookup), and the JIT's per-type specialization table is (b)'s table filled lazily at the tier where it pays. Statically-resolved evidence is a constant operand — at most call sites the type is static and there is nothing to look up; on dynamic paths the disjoint-head guarantee means the tag-keyed IC never needs tie-breaking. Generic calls in tail position tail-call normally (evidence words are ordinary register arguments).

**Why the others lose.** (b) bytecode-level monomorphization pays a materialization pause per new instantiation at first call, grows bytecode O(instantiations), and makes eviction policy VM-visible. (c) erasure re-imports boxing for every primitive in generic code — the exact 2-10x term exec 6 exists to delete — and caps the JIT without speculation machinery. (d) comptime monomorphization pays the full fan-out at load with no lazy tier to prune from; in an interpreter it is (b) with a strictly worse policy.

**Evidence.** research/05 §6.1-6.4 (the two-axis decomposition and the ruling), research/03 §8 and Table 6 (the best-rated row), §2 item 1 (IC-as-hint).

**Locks in.** The evidence-word calling convention; E1d's handler dispatch reuses the same substrate; variance by set inclusion, no HKT/GATs in 1.0 (05's Variance and HKT section).

### G3 — Module system: definition < file < package < module layering

**Constraints.** The author's Python pains (import-time execution, runtime-failing cycles, directory-granularity visibility, `__init__` boilerplate — research/06 §1.7's five-mechanism decomposition); the `#include` hatred (text inclusion, no reviewable API surface); hot reload; startup; zero-boilerplate growth from script to package.

**Why the winner wins.** (b) makes module bodies pure compile-time declarations — one rule buying three designs: intra-package cycles become a name-resolution detail (the cross-reference pain solved), hot reload becomes safe (Erlang's model needs no top-level side effects), and lazy dependency-driven loading cuts startup. Files in a package share one namespace with zero intra-package imports; the generated `.dsi` is the reviewable API diff (MoonBit's `.mbti` practice: "if nothing in .mbti changes it is typically a safe refactoring"); `///|` blocks give tooling sub-file granularity and are optional — definitions are order-independent regardless, a file without separators is one block. Cross-package cycles are banned because cycles defeat per-package incremental checking (the second reason, research/06 §4.1).

**Why the others lose.** (a) Rust's tree works but pays `mod` declarations and the 2018-edition path grammar, and invalidates at package grain — coarse for a scripting LSP. (c) Unison abolishes plain text, so git/grep/patch/text-LSP get reinvented; its hashing idea is borrowed into G3.5's content-addressed dsmod instead, 90% of the benefit at zero cultural cost. (d) ESM files-as-modules needs index files or import maps to be packages (the `__init__` chore reborn), needs a project layer bolted on for cross-file checking (the tsconfig situation), and weakens the hot-reload story (per-file re-evaluation versus versioned package slots).

**Evidence.** research/06 §1.7 (the five Python mechanisms), §1.9-1.11 (MoonBit model, Inko import grammar), §4.1 (incrementality), §6.3 (startup levers), G3.5 (the dsmod contract).

**Locks in.** G3.5 binds every option; H6(b)'s store caches what this produces; the five tooling commitments consume the block model.

### G4 — Visibility: layered with selective exports

**Constraints.** The Python `_`-convention pain rules out convention-based answers; the interface file should be the review artifact; intra-package cycles make file-scoped visibility incoherent (all files in a package resolve as one namespace, so visibility attaches to definitions).

**Why the winner wins.** (b) gives three spellings — private by default, `pub(pkg)` package-wide, `pub` to dependents and into the `.dsi` — plus Inko-style selective/aliased/conditional imports and re-export. Per-definition visibility makes moving code between files refactor-neutral; internal packages are expressible without opening the public API; the `.dsi` diff is the gate itself.

**Why the others lose.** (a) pub/priv binary conflates "one sibling package" with "the world"; mid-sized libraries expose internals or over-publish. (c) capsules make every visibility question a type-unification question, duplicate E1c at higher cost, and breach the sugar ceiling. (d) Go's capitalization spells the API into identifiers — renames become API diffs, and the author's taste already rejects convention-enforced privacy.

**Evidence.** research/06 §1.9 (`.mbti` sections), §1.11 (Inko `import mod (A, B as C)` grammar), §1.3 (Ada's conceptual ancestry).

**Locks in.** The `.dsi` doubles as the LSP hover source and the incremental boundary; the path-scoped `pub(in ...)` ladder is deliberately left out to stay under the ceiling.

### H5 — FFI and embedding: linear handles as substrate, platforms layered

**Constraints.** "The FFI layer is where linearity earns its keep" (research/06 §5.1): the charter's linear layer should make C resource discipline a type fact; the Lua-vs-Python C API lesson — minimal handles suffice, pointer-plus-convention is where extension bugs live; embedding wants two-embedders-two-VMs (no GIL-by-design).

**Why the winner wins.** (a) makes the convention a type error: opaque foreign resources are `~Copyable` handles, double-free does not compile, extern borrows are call-scoped, callbacks run on VM-managed stacks, and the embedding API is a handle table (`ds_push`/`ds_ref`, Lua-registry shape) where a `Value` is an index into the object table. (b) layers platforms as ordinary packages exporting effect handlers — embedding without Roc's monolith, selectable per target, with the `ds` CLI itself as the default platform. (c) remains the documented escape hatch (extern declarations are what `ds bind` emits before handle wrapping).

**Why the others lose.** (c) as the policy discards the one thing the linear layer uniquely buys — "must free exactly once" becomes a lint, and the embedding API inherits Python's convention swamp. (d) wasm-first constrains the value representation (typed registers must map onto wasm GC or box) and demotes the native VM from reference semantics; it is (a)'s tier-3 deployment target, an FFI architecture it is not.

**Evidence.** research/06 §5.1-5.2 (the C API comparison tables), §5.4 (platform inversion decomposed), §5.3 (WIT worlds); 08 resolved question 1 (the pinning rule).

**Locks in.** C1(b)+C5(c) become load-bearing at the boundary; dsmod's FFI-USED flag declares dlopen needs at load.

### H6 — Ecosystem: hash-store core with zero-config script mode

**Constraints.** The author's UPM essay (per-package locks, output-hash store, FHS linking, bin-first artifacts) and Nix pains (global `flake.lock` breaking dependents, no FHS, undocumented builtins); bun-style script ergonomics loved; LSP-first tooling is not optional.

**Why the winner wins.** (b) grounds the UPM in proven mechanics: blake3 dependency-hash cascade makes the manifest the lock (Zig's `build.zig.zon`; `follows` cannot exist by construction), MVS resolution delivers "never a version you never tested against" (Go/Inko), the output-hash content-addressed store doubles as the bytecode cache (the artifacts are dsmod files), bin-first per-triple artifacts kill the build-from-source default, and the registry defers to a static JSON index until ecosystem size demands it. (c) layers `ds run script.ds` with bare-specifier imports and `ds save` freezing a manifest — the growth path the author's daily use actually exercises. The five mandatory tooling commitments (query compiler, zero-option fmt, structured two-span diagnostics, linear REPL, two-slot hot reload) bind under every option because each is load-bearing for an IDE-centric author.

**Why the others lose.** (a) Cargo-style makes the registry the long-pole infrastructure, carries feature-fork and lockfile-drift taxes (cargo#10489's surprise-feature unification), and forecloses the UPM by construction. (d) platform-first starves the library ecosystem while the platform story matures (Roc's trajectory) and is delivered anyway by H5(b) — an axis, an answer it is not.

**Evidence.** research/06 §2.2-2.7 (pnpm/Zig/Inko/Nix/Bazel mechanics mapped against the UPM, with each element's proven precedent and remaining gap), §2.6 (Roc's deferred registry), §4 (tooling), §6 (startup/distribution); UPM essay (author input).

**Locks in.** G3.5 dsmod is the store's artifact; the two-slot runtime must exist from day one for (c)'s hot reload — retrofitting is the named mistake to avoid.

## H. Runtime

### H1 — Execution pipeline: three tiers over one shared lowering

**Constraints.** Charter reqs 1 and 3; research/03's central claim — the bulk of every dynamic VM's JIT is runtime type/shape speculation, and static typing deletes ~70% of the tiering machinery unconditionally (§a.3, §b.3); no deopt cliffs; instant startup for scripts.

**Why the winner wins.** (a) amortizes one uop set across all tiers (BL-5: the stencil set is the uop set one-to-one), so tier 2 copy-and-patch is a derivative artifact — a second compiler it is not (arXiv:2011.13127: ~14% faster than LLVM -O0 at ~100x its compile speed; CPython ships it as PEP 744). Tier transitions are in-place J-variant opcode rewrites (LuaJIT's JFORL trick); OSR reduces to compiling the loop body, so no frame transformation machinery exists. The data path has no guards — the four residual exits are the debugger, the per-entry stack-limit compare, effect-handler entry (a control transfer), and B7a's cold overflow edge — which is why there are no cliffs.

**Why the others lose.** (b) single method-JIT is the recorded fallback (if the per-arch stencil CI matrix dominates maintenance) but carries the OSR-or-accept-the-loss debt and loses the stencil economics. (c) meta-tracing is the wrong trade for a static language: the interpreter is already type-directed, so tracing spends its budget on AST-node dispatch tier-1 bytecode eliminates; PyPy's warmup dead zone (scripts under 0.2s never benefit) and introspection-disables-JIT semantics sit exactly where debuggers and hot reload live; no bytecode artifact means BL-7 foreclosed. (d) wasm-AOT-primary pays the E2b fiber-lowering cost and value-representation compromises; adopted as tier 3's deployment mode instead.

**Evidence.** research/03 §a.3, §b.2-3 (the deleted-subsystem ledger and deopt table), §b.1 (uops/stencils/superinstructions), §d (Pipelines B+C+D and the sequencing paragraph).

**Locks in.** BL-1 through BL-7 as fixed inputs; tier-1-only platforms ship where stencils are unverified (CPython's opt-in posture).

### H2 — Tail calls: guaranteed from day one, self and mutual

**Constraints.** Charter req 3 names guaranteed TCO as a fixed input; the per-language evidence table has a clean pattern — every system that guarantees TCO owns its calling convention (Lua, LuaJIT, Scheme, Koka, wasm 3.0 `return_call`), and every system that hoped its JIT would do it later still has none in 2026 (Ruby YJIT/ZJIT, V8 versus ES6, CPython).

**Why the winner wins.** (a) is the cheapest guarantee on the menu: in tier 1 a tail call is an argument slide plus frame reuse (Lua's `luaD_pretailcall`, `CIST_TAIL`); in JIT tiers a label loop (Koka's `kk__tailcall`) or the slot-overlay `jmp` (LuaJIT CALLT). The slot-overlay ABI (callee argument slots overlay the caller's outgoing-arg area, locked at frame-layout time) makes the cost O(args) with no stack growth at any depth, in any tier, deterministically. Backtraces stay honest via tail-chain metadata (Lua supports `debug.traceback` despite full TCO); the guarantee is scoped to the same effect context, where handler entry materializes a frame (Koka's one-shot continuations are the evidence this is acceptable).

**Why the others lose.** (b) best-effort makes stack behavior a property of which tier compiled the caller — unpredictability in exactly the area the charter refuses to trade, and the Ruby record shows JIT teams consistently deprioritize it. (c) trampolines pay an indirect call plus a closure allocation per bounce and infect every tail-recursive signature with the Bounce convention; an escape hatch for effect-context boundaries, a mechanism it is not. (d) effects-only-tail-resumptions reduces to "no TCO at all" under E1d (no surface effects in v1) — a direct req 3 violation.

**Evidence.** research/03 §b.5 (the mechanism evidence and per-language table), §d ("two design decisions to lock early": the slot-overlay ABI and BL-7).

**Locks in.** The ABI locks before anything else in staging; per-fiber guarantee holds (segments never grow under tail loops); the TAILCALL/RET stencil pair designs against the two-word callee entry descriptor.

### H4 — Memory runtime: Perceus RC plus reuse plus arenas, collector interface as growth path

**Constraints.** Same substrate as C4 (decided there); the runtime-mechanics questions here are allocator paths, pauses, cycle policy, and what each JIT tier must interoperate with; deterministic deinit and raw-pointer strings both depend on a non-moving heap.

**Why the winner wins.** (a) is C4(a)+(d) stated as runtime mechanics: per-frame bump arenas (an add to allocate, a restore to free, sound because linearity proves non-escape per site) plus Perceus RC with reuse threading for boxed values (`kk_block_alloc_at` falling back to mimalloc at ~15-25 cycles when no candidate exists). Zero pauses, zero barriers, no safepoints or GC maps in any tier; frees feed deinits deterministically. The honest accounting is carried: 100% of non-moved temporaries are register/arena-resident; the boxed/shared residue keeps paying RC, and the reuse pass is budgeted as optimizer work. (d)'s two-hook collector interface (cycle-candidate registration, weak refs) is committed in the header format now — the upgrade path for shared cyclic workloads that never redesigns the VM.

**Why the others lose.** (b) generational GC is the only option with pauses, needs safepoints and precise stack maps in every tier, and a moving collector forces pinning or handles on B8a's one-raw-pointer strings; Inko's arc — ship immix, publish its removal, then remove GC entirely — is the decade-scale signal. (c) malloc/free-only breaks on closures, persistent collections, continuations, and fiber stacks — the language's own core workloads; viable only as the substrate under arenas, which (a) already is.

**Evidence.** research/03 §a.8, §b.4, §b.6; research/01 §B.8 (Inko), §B.2 (Perceus/kklib).

**Locks in.** The acyclicity stance is honest API surface (shared graphs need the explicit collector or arena API); fiber switching needs no arena action (per-fiber marks, chunk-list growth).

## Resolved open questions — rationale

**FFI pinning (q1).** The RC/arena heap never moves objects, so pinning reduces to lifetime: call-scoped borrow parameters (second-class, cannot escape by typing), owning pointers as `~Copyable` handles, callbacks on VM stacks, blocking calls pinning their fiber with carrier threads as the overlap escape, cancellation observed only at VM re-entry so a consume in flight is never interrupted. This is the minimal rule set under which C2(d) modes and C5(c) deinit stay sound across the boundary.

**Nursery shape and token set (q2).** The block form (`nursery { }` + `spawn { }`) makes scope exit the join point — lexical structure is the entire semantics Trio needs; a `with`-form or trailing-lambda builder would add surface for the same scoping. The tokens are finalized exactly as researched (postfix `?` with the Zig containment rule; `or` with a lazy right operand and early-exit forms; `is`; difference-typed `.err`/`.ok`) because research/04 §11's accounting (three tokens for the whole optional/error story) is the budget argument.

**`.err` fallback (q3).** Moot once B1(c) is chosen — the fallback branch existed only for a nominal-only type core.

**Fibers vs hidden generators (q4).** The cursor protocol needs no fibers and keeps v0's runtime smaller; permitting a later shared facility preserves the option without paying for it — the surface is identical either way, so the decision is purely a scheduling one.

**Generator arrow (q5).** Public generator syntax would spend sugar budget on the one iteration shape combinators already cover; the arrow-encodes-kind idea is recorded as prior art (04 F2) so it is not silently lost.

**Scoped named instances (q6).** Deferred because B6(a)'s resolution has no known failure mode yet; adding the escape hatch preemptively would spend surface on a problem that may never appear, and ambiguity-as-error keeps any future sugar honest.

**Disjointness and termination (q7).** Left implementation-defined with fixed requirements (link-time check over the final environment; witness-naming errors; self-satisfying conditional instances rejected) because the algorithm choice does not change any language-level semantics — the requirements pin everything a user can observe.

**Float equality (q8).** Bit-structural `==` keeps reflexivity (the load-bearing property for dict keys) at the cost of IEEE surprises at the default spelling; the named comparisons (`is_float_eq`, `is_approx_eq`) make IEEE reachable without a second operator token. This follows Roc, the only surveyed language that faced the totality question directly.

**Comptime in v1 (q9).** Derives cover the hot cases with one word each and no evaluator; the evaluator is real machinery (a second execution phase to debug), so it lands after the core proves itself — both before 1.0, because G1(b) is the adopted mechanism and `none beyond derives` is a v1-shape G1(a) would be.

**`@` binders (q10).** Excluded because `is` narrowing covers the dominant pattern and the remaining affine re-binding case deserves a designed semantics rather than an inherited one; S1 additive later, within the reserved headroom.
