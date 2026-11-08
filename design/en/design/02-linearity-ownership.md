# DreamScript design 02 — linearity, ownership, and mutation

Decision catalog translating docs/en/research/01-linearity-ownership.md (primary evidence) and the memory/EA/TCO constraints of docs/en/research/03-runtime-ir-jit.md (§a items 6-9, §d) into eight selectable decision points; catalog index lives in docs/en/design/00-decision-map.md per the charter. Evidence discipline: every prior-art claim cites the research notes by file and section (`research/01 §B.x` = deep-dive subsection, `research/03 §a.N` = executive-summary item N, `research/02 §b.N` = deep-dive N); no claim without a pointer.

Shared assumptions (charter AGENTS.md; not re-litigated here): interpreted execution on a typed register bytecode VM with optional tiers (research/03 §d pipelines A-B); bindings immutable by default (charter req 5); no C++-style specialization semantics (charter philosophy); sugar ceiling at Kotlin's total surface (charter req 4); set-theoretic union/intersection types are an open option owned by docs/en/design/01; algebraic effects and async are owned by docs/en/design/03 and referenced here, never decided; compile-time errors are preferred over runtime errors (charter philosophy) — several options below are weighed against exactly that. **All code is illustrative; final surface syntax is decided in docs/en/design/04-syntax.md** — a neutral TS-family style (`const`/`var`, `fn name(a: T) -> R`, braces) is used so the substance under review is semantics, not spelling; the examples predate the docs/en/design/04 surface decisions and illustrate semantics only. Each point carries exactly one bold **Recommendation** line (on the option the point should pick); the other options end with a **Verdict** line.

## Decision-point index

| Point | Question | Options | Recommendation |
|---|---|---|---|
| C1 | What linearity flavor is the language's default? | 4 | (b) affine core + opt-in linear layer |
| C2 | How do users share values without copying (borrowing)? | 4 | (d) second-class parameter modes |
| C3 | What are the mechanics behind "immutable by default"? | 4 | (b) val/var + exclusion windows |
| C4 | What memory-management strategy runs underneath? | 4 | (a) Perceus RC + reuse, operationalized with (d)'s arenas |
| C5 | Who writes drop/deinit, and when does it run? | 4 | (c) linear RAII deinit on the resource layer |
| D1 | Where is mutation confined? | 4 | (b) exclusion-window mutation |
| D2 | What is the collection model? | 4 | (b) linear-mutable COW: functional API, in-place implementation |
| D3 | What is the iterator/generator story? | 4 | (d) combinators + hidden generators |

## C1 — Linearity flavor

Charter hard req 2 reads "static linear (or affine) typing, enforced at compile time"; charter req 6 adds "prefer designs with low learning cost and low error rate that keep the performance benefits". The research consensus: every linear fact a user could write is derivable as a uniqueness fact, and "deriving beats writing for ergonomics, writing beats deriving for guarantees" (research/01 §a, §B.12; research/01 §d Combo 6) — C1 decides who writes it down.

### C1.a — Quantified linearity by default (QTT everywhere)

```
fn dup(1 x: a) -> (a, a)            // multiplicity 1 on the binder: x used exactly once
fn map(f: fn(a) -> b, xs: List<a>) -> List<b>   // every arrow carries a multiplicity
drop : (1 x: a) -> ()               // fine; using a 1-binder twice is rejected (research/01 §B.6)
```

Prior art: Idris 2's QTT binders carrying quantities 0/1/omega with guaranteed erasure of 0-quantities (research/01 §B.6, ECOOP 2021); Granule's graded modalities generalizing quantities to semiring grades, `dup : a [2] -> (a, a)` (research/01 §B.6); Linear Haskell's `%1 ->` and multiplicity-polymorphic `%m` arrows, where polymorphic libraries leak UR constraints into ordinary signatures and `linear-base` exists to absorb the damage (research/01 §B.6, GHC proposal 0111).

| Dimension | Assessment |
|---|---|
| Learning cost | Highest in the survey: a number on every binder; multiplicity polymorphism is the specifically bad part (research/01 §B.6) |
| Runtime perf | Best static guarantees (erasure proofs, RC elision), but none of Idris 2/Granule/GHC demonstrates a memory payoff — all are GC'd (research/01 §B.6, §c Table 1 "n/a (GC'd)") |
| Expressiveness | Protocol-level strengths (assets, session types, erasure); painful as an everyday default (research/01 §B.6) |
| Compiler complexity | Very high: multiplicity unification interleaves with all inference; research/01 §c Table 3 rates QTT signature noise "very high" |
| Forecloses | C2 collapses to Austral-style borrow statements (research/01 §B.5); C3's exclusion windows need ATS-style linear views (research/01 §B.6); D2 persistent collections get hard under consume-once; the sugar ceiling (charter req 4) is breached on signatures alone |

**Verdict — no.** Quantities are the right internal checking vocabulary and the wrong surface syntax; the UR-constraint pain (research/01 §B.6) is exactly what charter req 6's low-learning-cost clause forbids.

### C1.b — Affine core + opt-in linear resource layer

```
struct File: ~Copyable { fd: RawFd }    // ~Copyable subtracts the default Copyable constraint
const g = f                             // f is affine: assignment moves; using f after: compile error
fn read_all(f: File, cap: Cap) -> Bytes // f consumed exactly once; the checker enforces it
```

Prior art: Swift SE-0390 noncopyable types — "values of noncopyable type always have unique ownership, and can never be copied", with `deinit` at the unique instance's lifetime end (research/01 §B.3); Hylo's `Copyable: Movable` traits, copyability as a default positive constraint subtracted by negation (research/01 §B.3); Inko's static move errors `error(moved): 'cats' can't be used as it has been moved` (research/01 §B.8); Austral as the strict end of the consume-once spectrum (research/01 §B.5); Rust's affine moves with the borrows stripped off (research/01 §B.4); the Entente Cordiale verdict that writing beats deriving for guarantees (research/01 §a, §B.12).

| Dimension | Assessment |
|---|---|
| Learning cost | Low-moderate: one everyday rule ("assigning moves; a moved binding is dead") plus one author-facing idea (`~Copyable`); ordinary immutable values never observably hit the move rule (research/01 §B.8 value-type note) |
| Runtime perf | Full compile-time enforcement; moves are register copies with no dup/drop (research/01 §B.12 item 1; research/03 §a.7) |
| Expressiveness | Everyday code unaffected; resources/protocols get static zero-check guarantees — exactly the gap inference cannot fill (research/01 §B.2) |
| Compiler complexity | Moderate: affine checking is a last-use analysis — Hylo runs it as an intraprocedural abstract interpreter (research/01 §B.3), Austral's state-table version is a single forward pass (research/01 §B.5) |
| Forecloses | Closes C1.a (no multiplicity syntax); makes C2 mandatory (moves without borrows still need a read-access answer — C2.d is the natural one); pairs naturally with C4.a because affine use-once semantics make most dup/drop statically elidable (research/03 §b.6) and with C5.c because deinit hooks live exactly on the `~Copyable` layer (research/01 §B.3) — the C1(b)+C4(a) natural pairing the research names (research/01 §d Combos 3+4, research/03 §a.8) |

**Recommendation — (b).** The honest reading of charter req 2 ("linear (or affine)") at the lowest learning cost, and `~Copyable` as a negated set-theoretic constraint composes with docs/en/design/01's unions natively — research/01 §a calls it "the best precedent for DreamScript's taste".

### C1.c — Hidden linearity (inference-only, no ownership surface)

```
const xs2 = map(sqr, xs)          // no annotation anywhere
const b = append(xs, x)           // may overwrite xs's buffer if unique, else copies:
                                  // a performance note, never an error (research/01 §d)
```

Prior art: Koka's automatic borrow inference since v2.2.0 over Perceus (research/01 §B.2); Roc's ARC insertion — module header "borrow inference plus RC statement emission" — with two-phase interprocedural `arc_solve` and a born-unique analysis, on a language whose FAQ explicitly states it has "no linear, dependent, refinement, or uniqueness types" (research/01 §B.1); Lobster's ownership analysis removing ~95% of runtime RC operations with only three rare compile errors (research/01 §B.9); the 2024-2026 trend statement (research/01 §a).

Honest tension with the charter, stated plainly: hard req 2 says "static linear (or affine) typing, enforced at compile time" — this option removes user-visible linearity entirely, so as written it violates the requirement rather than stretching it. What "linear" could still mean here: (1) an IR-level invariant — Hylo treats register-resident objects as linear resources consumed by any operand use (research/01 §B.3), which makes escape analysis a compile-time fact and scalar replacement free (research/03 §a.7); (2) derived uniqueness — every linear fact is recoverable as a refcount==1 fact (research/01 §a, §B.12), so the compiler holds the guarantees and the user holds the ergonomics; (3) an opt-in linear layer can survive underneath (Koka's position: linearity is unnecessary for the memory payoff but still needed for static zero-check resource guarantees — research/01 §B.2). Adopting (c) means re-wording req 2, which is the author's call, not this catalog's.

| Dimension | Assessment |
|---|---|
| Learning cost | Lowest possible: zero keywords, maybe `mut` (research/01 §c Table 3) |
| Runtime perf | Near-C for functional workloads (research/01 §B.1, §B.2); runtime uniqueness checks alone deliver the Koka fast path before any inference exists (research/01 §d Combo 1) |
| Expressiveness | High for value-oriented code; resources and protocols lose static guarantees unless the opt-in layer is retained |
| Compiler complexity | Highest of the low-learning-cost options: interprocedural mode solving, reuse pairing, TRMC — Roc's bug tracker shows the analysis is load-bearing and debugged in the open (issues 11830, 10920, 12436; research/01 §B.1) |
| Forecloses | C2.b becomes the borrow answer (modes would be redundant); coherent only with this hidden flavor; C5.c's guaranteed deinit has no user-visible anchor without the linear layer; conflicts with charter req 2 as written |

**Verdict — no for v1 semantics, yes as the optimization endgame.** Stage (c)'s machinery behind (b)'s surface exactly per research/01 §d Combo 6: runtime uniqueness checks first (semantics), inference second (pure optimization), zero language change when it lands — the ship order Koka and Roc both prove works.

### C1.d — Owned core + runtime-checked borrows + opt-in static linear annotations

```
let r = ref xs                    // borrow; NOT lifetime-checked
xs = nil                          // move/drop while r alive => deterministic panic:
                                  // "can't drop a value of type 'List' as it still has 1 reference(s)"
```

Prior art: Inko's full model — visible moves, explicit `ref`/`mut` borrows that are not lifetime-checked, a per-object borrow counter checked at drop producing a deterministic panic with stack trace, from "Ownership You Can Count On" (2006) (research/01 §B.8); Austral's linear annotations plus block-scoped `borrow x as y in { ... }` statements for the opt-in layer (research/01 §B.5); research/01 §d Combos 2+4; zest's stack-resident counts where each safety check is a single integer comparison (research/01 §B.11).

| Dimension | Assessment |
|---|---|
| Learning cost | Very low — research/01 §d Combo 2: "Inko teaches the whole system in one manual page" |
| Runtime perf | Good: one counter update per borrow, optimizable later (Inko ships unoptimized and plans elision, research/01 §B.8); the linear layer is zero-cost (research/01 §d Combo 4) |
| Expressiveness | Highest of the no-borrow-checker options: linked lists and graphs are natural (research/01 §B.8) |
| Compiler complexity | Very low — but ownership errors become runtime panics, against the charter's "compile-time errors over runtime errors" philosophy |
| Forecloses | C2.d (modes are redundant with `ref`/`mut`); C5's guarantee story (drops can panic instead of running); caps correctness at runtime-checked aliasing unless Inko's planned gradual static upgrade is reproduced (research/01 §B.8); conflicts with charter req 2's "enforced at compile time" for the aliasing half of the model |

**Verdict — no as the final flavor, yes as the v0 substrate.** Its borrow counter is precisely the diagnostic layer research/01 §d Combo 6 prescribes under (b)+(d) while the static checker matures: cheap, deterministic, and forward-compatible (the counter field doubles as the refcount==1 check, research/01 §d Combo 2).

## C2 — Borrowing

With affine moves chosen (C1.b), values must still be readable without transferring ownership; research/01 §B.4's checklist (without.boats: moves, borrows, lifetimes, drop are four semi-coupled features and the difficulty is the coupling) is the design constraint — adopt the cheapest coupling that works.

### C2.a — No user borrows: moves and explicit copies only

```
fn len(s: Str) -> Int { ... }          // consumes s
const n = len(clone(s1))               // every read of a kept value is an explicit clone
fn swap(xs: List<Int>) -> List<Int>    // consume-and-return chaining replaces "mutation"
```

Prior art: Move's resource types with explicit `copy`/`move`, the cleanest no-borrow industrial design (research/01 §B.11); Rust's `clone()` culture, the first item on research/01 §B.4's workaround list ("erase sharing"); Inko's value types copy on move so small values escape the aliasing machinery (research/01 §B.8).

| Dimension | Assessment |
|---|---|
| Learning cost | Trivial rule, heavy practice: every read site becomes a transfer decision |
| Runtime perf | Poor: copying as the only sharing path is Rust-without-borrows, i.e. the clone() workaround culture research/01 §B.4 documents |
| Expressiveness | Poor: shared reads of large values always copy; consume-return chains replace in-place algorithms |
| Compiler complexity | Lowest of all C2 options |
| Forecloses | C2.d and C3.b/D1.b entirely (any user-visible borrowing is gone); pushes D2 to persistent-only or chain-style APIs; forfeits the performance charter (req 3) for read-heavy code |

**Verdict — no.** research/01 §B.4 shows every workaround in a no-borrow system converges on "dynamic check or dynamic count" — paying copies to avoid modes buys neither simplicity nor speed.

### C2.b — Inferred borrows (compiler-invisible)

```
fn head(xs: List<a>) -> a              // user never writes a mode; compiler passes xs borrowed
const h = head(xs)                     // xs still usable: the borrow was inferred; if the
                                       // compiler must copy instead: a perf note, not an error
```

Prior art: Koka v2.2.0's automatic borrowing of user functions, borrowed locals emitting no incref/decref (research/01 §B.2, `src/Backend/C/Parc.hs`); Roc's `arc_solve` two-phase interprocedural signature solving with a debug `arc_certifier` re-checking output (research/01 §B.1); Lobster's AST-node ownership analysis with retroactive ownership — across dozens of game prototypes enabling it required "minor changes in only 2 locations" (research/01 §B.9).

| Dimension | Assessment |
|---|---|
| Learning cost | Lowest: zero surface (research/01 §c Table 2, Koka/Roc/Lobster rows) |
| Runtime perf | Near-C: ~95% RC-op elision (Lobster), near-C results (Koka) (research/01 §B.9, §B.2) |
| Expressiveness | High for value code, but the user cannot state a sharing contract where inference fails — Roc's issue 12420 shows the compiler's choice has observable perf effects in both directions (research/01 §B.1) |
| Compiler complexity | Highest here: interprocedural fixpoint over signatures; Frame Limited Reuse exists precisely to keep the analysis local and separate-compilation-friendly (research/01 §B.2) |
| Forecloses | C2.d (redundant); coherent only with C1.c; delays a shippable v1 — Koka and Roc both shipped runtime uniqueness checks first and inference second (research/01 §d Combo 6); strains the charter's explainability budget when borrow behavior must be explained with no user-facing concept to attach it to |

**Verdict — no for v1.** This is the optimization layer to grow later with zero language change (research/01 §d cross-cutting note 5: ownership decisions surface "as informative notes about copies, never as errors"); starting here means shipping the hardest compiler pass before the first working interpreter.

### C2.c — Runtime-checked borrow counter

```
let r = ref xs                         // borrow increments xs's borrow count
drop(xs)                               // deterministic panic while r is alive:
                                       // "can't drop value of type 'Array' as it still has 1 reference(s)"
```

Prior art: Inko's borrow counter checked at drop, with the determinism argument that "running the same program with the same input 10 times will produce the same error 10 times", plus the empirical claim that borrow errors are rare (research/01 §B.8); Swift's dynamic `-enforce-exclusivity=checked` as the mature-language analogue of backing static rules with runtime checks (research/01 §B.3); research/01 §B.12 item (3) and research/03 §a.8's "Inko-style borrow counter for deterministic diagnostics".

| Dimension | Assessment |
|---|---|
| Learning cost | Low: one concept (`ref` borrows), errors are readable panics with stack traces |
| Runtime perf | One counter op per borrow; mostly optimizable later (Inko's stated plan, research/01 §B.8) |
| Expressiveness | Highest of the no-checker options: graphs, linked lists, iterators all natural (research/01 §B.8) |
| Compiler complexity | Lowest: no static aliasing analysis at all |
| Forecloses | As the sole mechanism (i.e. with C1.d), correctness is capped at runtime — a permanent violation of the compile-time-errors philosophy for aliasing bugs; as a substrate under C2.d it forecloses nothing (second-class borrows statically cannot dangle, so the counter only fires on FFI/unsafe bugs) |

**Verdict — no as the user-facing model; yes as the v0 diagnostic substrate under (d).** research/01 §d Combo 6 stages exactly this: the borrow counter runs while the static story is immature ("cheap insurance") and is removable later without semantic change.

### C2.d — Static second-class borrows as parameter-passing modes

```
fn len(s: Str) -> Int                  // default mode: immutable borrow (Hylo `let`, Swift `borrowing`)
fn push(xs: mut List<Int>, x: Int)     // exclusive mutable borrow (Hylo `inout`)
fn consume(f: File) -> Int             // consuming move (Hylo `sink`, Swift `consuming`)
xs.push(1)                             // UFCS: the mode lives on the declaration; call sites never annotate
```

Prior art: Hylo's `let`/`inout`/`sink`/`set` modes with borrows as IR-level projections and no references in the surface language (research/01 §B.3, LocalBindings.md `fun add(let Int, let Int) -> sink Int`); Swift SE-0377's `borrowing`/`consuming` with the accepted revision requiring an explicit `copy` operator so every copy is visible (research/01 §B.3); research/01 §d Combo 3; OxCaml's 2025 modes, noted as the industrial re-arrival of "modes, not lifetimes" (research/01 §B.11).

Interaction with UFCS and the immutability default: modes are fixed per parameter at the definition site — the anti-multiplicity-polymorphism lesson applied to modes (research/01 §B.6, cross-cutting note 2) — so UFCS chains stay clean and call sites carry zero mode syntax. Immutability-by-default is then enforced by which modes a binding permits: `const`-bound values can only feed borrow-mode parameters, `var`-bound owned values feed `mut` windows, and Koka-style UFCS (`xs.push(1)`) stays syntactically identical across modes. Subscripts with `let`/`inout`/`set` accessors extend the same modes to element access, replacing getters/setters and iterator protocols (research/01 §d Combo 3; Hylo's four accessor kinds on one subscript, research/01 §B.3).

| Dimension | Assessment |
|---|---|
| Learning cost | Low-moderate: two keywords beyond the default, one concept ("borrows are parameter modes, not values"); research/01 §c Table 3: "modes on decls only" |
| Runtime perf | Excellent: no aliasing means no runtime checks and trivial escape analysis (research/01 §d Combo 3) |
| Expressiveness | Second-class only: no `Option<&T>`, no stored borrows — external-style iteration goes through subscripts or cursors (research/01 §d Combo 3 catalogs the breakage) |
| Compiler complexity | Moderate and local: borrows cannot escape the call, so checking is intraprocedural (Hylo's DI abstract interpreter already computes the needed states, research/01 §B.3) |
| Forecloses | C2.a/b/c as the sole model; first-class borrows permanently — deliberately, per the without.boats coupling checklist (research/01 §B.4); pairs with C3.b and D1.b (the `mut` mode IS the exclusion window — one mechanism, three uses) and with D3.a/d (subscripts/cursors as the iteration substrate); known wrinkle: `Result<~Copyable>` payloads need a `Movable` requirement or auto-borrow (research/01 §d Combo 3) |

**Recommendation — (d).** Second-class modes are the only borrow design in the survey with a page-sized rule set, no lifetime annotations, and a direct fit for the immutable-by-default UFCS surface (research/01 §a, §d Combo 3; research/01 §c Table 1 ranks the Hylo row low-cost/high-perf).

## C3 — Immutability-default mechanics

Charter req 5: "Bindings immutable by default; mutation is an explicit, locally-visible choice" — the question is what mutation, once chosen, may touch. A deliberate non-feature recorded from the research: neither Hylo nor Swift allows mutating an immutable binding, and research/01 §B.3 says to keep it that way — "rebinding-immutability adds aliasing paradoxes for marginal sugar".

### C3.a — val/var split; var is function-local only; no shared mutable state

```
const total = fold(xs, 0, add)         // immutable everywhere, including fields and captures
var acc = 0                            // rebindable local; dies at scope end
acc = acc + x                          // no var fields, no var captures, no inout anywhere
```

Prior art: Koka's `var` locals over an otherwise purely functional surface (research/01 §B.2); research/02 §b.7: linear+immutable makes shared mutable access impossible, collapsing the entire Send/Sync apparatus; the "mutating let" non-feature (research/01 §B.3).

| Dimension | Assessment |
|---|---|
| Learning cost | Lowest: mutation exists only where the variable is written |
| Runtime perf | Good with FBIP reuse (Koka compiles functional updates in place), but cross-function in-place algorithms (sort_in_place) must become consume-return chains or builder patterns |
| Expressiveness | No mutable fields, no callee-side mutation: large-state code (game loops, UI state) rebinds through builders or persistent updates |
| Compiler complexity | Lowest: no exclusivity tracking beyond local rebinding |
| Forecloses | C3.b/D1.b (no exclusion windows); D2's in-place methods become builder-only; pushes algorithmic code toward D2.c-style builders even where (b)'s window is simpler |

**Verdict — no.** It saves two keywords over (b) but forbids the one mutation shape value semantics needs — exclusive callee-side mutation — which is exactly what Hylo/Swift/Inko all provide (research/01 §B.3, §B.8).

### C3.b — val/var + exclusion windows

```
var xs = List.of(3, 1, 2)              // owned, rebindable local
sort(xs)                               // `mut List<Int>` parameter: one exclusive mutable window
                                       // for the call, statically guaranteed; shared-immutable again after
```

Prior art: Hylo's `inout` bindings and parameters — mutation goes through `inout` windows, and an `inout` binding "is always represented as a borrowed access at the IR level" (research/01 §B.3); Swift's `mutating` methods, with even Swift additionally enforcing exclusivity dynamically in debug builds — a mature system still backs static rules with runtime checks (research/01 §B.3); research/01 §d Combo 3: "borrows are raw pointers, no refcounts needed for mode-passed values".

| Dimension | Assessment |
|---|---|
| Learning cost | Low: one idea — "one exclusive mutable access, statically guaranteed"; the window is lexically visible as the call |
| Runtime perf | Best for mutation: no runtime checks inside windows; stack/arena locals work because borrows cannot escape (research/01 §d Combo 3) |
| Expressiveness | Full in-place algorithms, mutating methods, subscript setters (research/01 §B.3) |
| Compiler complexity | Moderate and local: exclusivity is intraprocedural — the same machinery as C2.d, one checker, two uses |
| Forecloses | C3.c (interior mutability becomes unnecessary); D1.a/c/d; requires C2.d's modes as the window mechanism; shared mutation is permanently out — by design |

**Recommendation — (b).** The same `mut` mode from C2.d is the window, so mutation stays "explicit and locally visible" (charter req 5) with zero additional mechanism, and the statically-guaranteed exclusivity lets the compiler skip every runtime check inside the window (research/01 §d Combo 3).

### C3.c — val/var + typed interior mutability (Cell/RefCell analogs)

```
const c: Cell<Int> = Cell(0)                     // copyable single-threaded interior mutability
const r: RefCell<Vec<Int>> = RefCell.empty()     // runtime-borrow-checked interior mutability
c.set(c.get() + 1)                               // legal behind an immutable binding
```

Prior art: Rust's `Cell`/`RefCell` — research/01 §B.4 describes them as "move the exclusivity check to runtime — precisely the trade Inko and Roc's runtime uniqueness checks make globally"; Dada's `atomic` blocks as the transactional variant of shared mutation (research/01 §B.10).

| Dimension | Assessment |
|---|---|
| Learning cost | Moderate: two new types plus the when-to-which decision |
| Runtime perf | RefCell pays a runtime borrow check on every access; Cell is cheap but silently breaks immutability-based reasoning (equality/hash surprises) |
| Expressiveness | Shared mutation behind immutable bindings — the one trick windows cannot do |
| Compiler complexity | Low, but it undermines two load-bearing invariants: barrier-free RC over immutable data (research/03 §a.8) and elision of Vale-style generation checks for immutable values (research/01 §B.7) |
| Forecloses | C3.b (the redundant escape hatch weakens window discipline); weakens the immutable-by-default invariant other recommendations rest on; D1.b becomes optional, splitting the mutation story |

**Verdict — no.** With C2.d/C3.b windows the language does not have the problem interior mutability solves; keep at most one hidden `UnsafeCell`-equivalent in the stdlib for implementing persistent structures, not a user-facing type family.

### C3.d — Immutable core + linear destructive updates (FBIP)

```
const xs2 = map(sqr, xs)              // functional surface, no var anywhere...
                                      // ...overwrites xs's cells in place when rc==1
const m2 = insert(m, k, v)            // "new" map; same buffer when m is uniquely owned
```

Prior art: Koka's FBIP with the verbatim guarantee — "the reuse optimization is guaranteed and a programmer can see when the optimization applies... allows us to express many imperative algorithms in a purely functional style" (research/01 §B.2); Perceus fan-in/fan-out reuse and Frame Limited Reuse (research/01 §B.2); Roc's COW builtin contracts and `box_reuse`/`loop_append_promote` passes (research/01 §B.1); research/03 §a.8.

| Dimension | Assessment |
|---|---|
| Learning cost | Zero new syntax — the discipline is "write functional code; it runs in place", with reuse visible as a documented guarantee (research/01 §B.2) |
| Runtime perf | Near-C for functional workloads (FBIP + TRMC, research/01 §B.2) |
| Expressiveness | As a total surface (no `var` at all): poor for algorithmic code — consume-return chains are what the FBIP/TRMC papers exist to fix; as a runtime guarantee under (b): strictly additive |
| Compiler complexity | Reuse pairing and TRMC are real passes with ordering constraints (research/03 §b.6, ParcReuse.hs) — budgeted in C4.a either way |
| Forecloses | Nothing when treated as the runtime strategy (it is what C4.a delivers); as the whole surface it forecloses C3.b/D1.b and local accumulation ergonomics |

**Verdict — (b) on the surface, with (d) as the runtime guarantee layered underneath.** C4.a's reuse pass makes (d)'s behavior automatic for all functional code while (b)'s windows keep explicit in-place algorithms writable; this is the combination Koka itself ships (var locals + FBIP reuse, research/01 §B.2).

## C4 — Memory strategy

Constraints from research/03 §a items 6-9: typed unboxed registers mean no NaN-boxing and no tag checks (item 6); linearity makes escape analysis a compile-time fact — Inko needed whole-program EA to promote 47-58% of allocations while a linear language gets 100% of non-moved temporaries for free (item 7); guaranteed TCO is cheap in a register VM, and a tail call transferring ownership performs no refcount updates (item 9, §b.5); research/03 §a.8 prescribes the combination this section recommends. Per option: pauses, allocator traffic, interpreter fit, cycle handling.

### C4.a — Perceus precise non-atomic RC + block reuse

```
// surface: const xs2 = map(sqr, xs)         — no memory syntax at all
//   rc_dec(tmp2)                            // tmp2 dies after field extraction (research/03 §b.6)
//   reuse = (rc(tmp2) == 1) ? tmp2 : NULL   // dying uniquely => candidate
//   x = alloc_at(reuse, TAG_PAIR)           // overwrite in place; no allocator call
```

Prior art: Perceus — precise RC, statically inserted dup/drop, reuse analysis, PLDI 2021 (research/01 §B.2); the runtime hooks `kk_block_alloc_at` / `kk_block_check_drop_reuse` in kklib (research/03 §b.6); Roc's ARC over the same contract (research/01 §B.1); research/01 §B.12 item (2); research/03 §c Table 5 ("best: linearity minimizes RC ops").

| Dimension | Assessment |
|---|---|
| Learning cost | Zero surface syntax; reuse applies automatically and is documented as observable (research/01 §B.2) |
| Pauses | None; frees are immediate and cache-friendly; finalizers run deterministically at last use (research/01 §B.12 item 2) |
| Allocator traffic | Lowest: reuse makes allocation ~free on the fast path, and affine/linear use-once semantics collapse dup/drop to explicit clone/move boundaries (research/03 §b.6) |
| Interpreter fit | Excellent: placement degenerates to "rc_inc at each clone, rc_dec at each unique death" because the linear checker supplies usage counts (research/03 §b.6); non-atomic counts are safe in the single-threaded-per-isolation model (research/01 §d Combo 1) |
| Cycles | Not collected: inductive data types cannot cycle (research/01 §B.2); Lobster's exit cycle report is the diagnostic pattern (research/01 §B.9); a weak/arena escape hatch covers the rest |
| Compiler complexity | Moderate but small under linearity — "no borrow analysis, no interference analysis" (research/03 §b.6) |
| Forecloses | C4.c (a moving GC conflicts with precise RC, interior pointers, and FFI — research/01 §B.12); requires known drop points, i.e. C5.b's DI infrastructure; shared-data concurrency must use per-process/message-passing isolation rather than atomics (research/01 §d Combo 1) — ties to docs/en/design/03 |

**Recommendation — (a), operationalized with (d)'s arena fast path.** research/03 §a.8 prescribes exactly this pairing ("Perceus-style precise reference counting with block reuse... Combine with per-frame bump arenas for interpreter temporaries"), and C1.b's affine values are what degenerate the pass to trivial (research/03 §b.6) — the C1(b)+C4(a) natural pairing.

### C4.b — RC + generational references

```
const r: Gen<List<Int>> = gen(&xs)    // 8-16 byte fat reference: (ptr, rememberedGeneration)
use(r)                                // unproven derefs pay a generation __check; stale access
                                      // => deterministic trap; destroy(xs) invalidates every r
```

Prior art: Vale's generational references — 16-byte refs or an 8-byte table variant, random generations with 2^-64 false-safety odds, `__check` via a protected page, BenchmarkRL terrain generator +10.84% vs +25.29% for naive RC (research/01 §B.7); research/01 §B.12 item (5) and the reference-layout comparison table (§B.7).

| Dimension | Assessment |
|---|---|
| Learning cost | Zero surface, and never a move error — its unique selling point (research/01 §B.7) |
| Pauses | None |
| Allocator traffic | No counts at all, but every heap reference grows 8 bytes and unproven derefs pay one compare; immutable-by-default elides most checks because immutable values are never invalidated behind an alias (research/01 §B.7) |
| Interpreter fit | Good — checks are single integer comparisons (zest's design point, research/01 §B.11); but 16-byte references do not fit the unboxed typed-register frame model (research/03 §a.6, §c Table 4) |
| Cycles | Safe to create (no counters to tangle); leaked unless weak refs or a cycle report is added (research/01 §d Combo 5) |
| Compiler complexity | Low analysis burden; generation maintenance in the allocator and destructors |
| Forecloses | Nothing of C4.a mechanically, but it abandons the compile-time-guarantee story — with staleness caught at access, C1.b's static affine discipline and C2.d's exclusivity stop being load-bearing, and the +8B/ref tax works against the unboxed-register dividend of research/03 §a.6 |

**Verdict — no as primary.** The right fallback if affine friction proves worse than expected ("no move errors ever", research/01 §B.7), but it doubles reference size and converts a static guarantee into a dynamic one.

### C4.c — Tracing GC (immix / moving)

```
const xs = List.of(1, 2, 3)           // no ownership, no RC; GC reclaims
const g = Node(x, nil)                // cycles fine
```

Prior art: Inko's Immix era and its post-mortem — "the implementation was quite complex and difficult to debug; Immix suffers from fragmentation, and without a GC (what it's designed for) it's hard to clean up the fragmentation; our implementation was unlikely to outperform highly optimised allocators such as jemalloc" (research/01 §B.8; research/03 §b.6); Immix PLDI 2008 (research/01 §B.12); research/01 §c Table 1 tracing-GC row.

| Dimension | Assessment |
|---|---|
| Learning cost | Lowest conceivable — no ownership concept at all; expressiveness highest for shared cyclic data |
| Pauses | Short but real; moving variants conflict with precise RC, interior pointers, and FFI (research/01 §B.12) |
| Allocator traffic | Highest: write barriers on pointer stores, copying, remembered sets (research/03 §b.6) |
| Interpreter fit | Poor-to-medium for a small team: safepoints, precise stack maps, barrier bookkeeping in every tier (research/03 §a.8); Inko — a strong solo implementer — abandoned its hand-rolled GC entirely (research/01 §B.8) |
| Cycles | Handled by construction — the one genuine advantage |
| Compiler complexity | The runtime, not the compiler, carries the complexity — but that runtime work is recurring and hard to debug (research/01 §B.8) |
| Forecloses | Wastes C1's linearity dividend (compile-time EA, research/03 §a.7) and C3's immutability dividend (no barriers needed, research/03 §a.8); coexisting coherently with C4.a is impossible; FFI and hot-reload stories get harder (running frames on old code, research/03 §b.9) |

**Verdict — no.** Inko's post-mortem is the strongest available evidence on true cost for a small team (research/01 §B.8), and linearity + immutability remove exactly the workloads GC is best at (cyclic shared mutables, allocation-heavy mutation).

### C4.d — Per-frame bump arenas primary + RC at escape boundaries

```
fn hot(xs: List<Int>) -> Int { score(build_tree(xs)) }  // build_tree bumps the frame arena; its
                                                        // nodes provably do not escape: freed on pop
```

Prior art: research/03 §a.7 — "linearity makes escape analysis a compile-time fact rather than a heuristic", with Inko's 47-58% whole-program EA as the heuristic baseline; research/03 §a.8 — per-frame bump arenas are "safe precisely because linearity forbids silent escapes"; Roc's platform allocation model (CLI platforms skip deallocations, web platforms arena per request — research/01 §B.1); Hylo's no-storage `let` bindings (research/01 §B.3); Koka's FLR frame-slot reuse as the compiled analogue (research/01 §B.2).

| Dimension | Assessment |
|---|---|
| Learning cost | Zero surface; one perf rule to document ("temporaries are free if they do not escape") |
| Pauses | None (bulk reset on frame pop) |
| Allocator traffic | Lowest possible for temporaries — allocation is an add, free is a restore (research/03 §a.8) |
| Interpreter fit | Perfect for non-escaping values, which linearity proves per-site (research/03 §a.7); long-lived/shared data still needs the RC layer, so this is a complement to (a), not a replacement |
| Cycles | Only in escaped RC data — same answer as (a) |
| Compiler complexity | Low: the arena allocator is trivial; the escape proof comes from the C1 checker, not a new analysis |
| Forecloses | Without (a) it cannot host persistent/shared structures at all; with (a) it forecloses nothing — the two compose into research/03 §a.8's recommended combination verbatim |

**Recommendation — (a) + (d) jointly.** RC-with-reuse for escaped/boxed values plus arenas for frame-local temporaries, no tracing GC, is research/03 §a.8's recommendation verbatim, enabled by C1.b; the index records C4 as "(a) + (d)".

## C5 — Move, drop, and definite initialization

Affine values need a well-defined death point; research/01 §B.12 item (3) distills the requirement — scope-end drops emitted by the bytecode compiler, last-use drops where liveness is confident, finalizers running deterministically at the drop point — and research/02 §b.8 adds that linear destructors are the hooks which run at fiber abort/cancellation, the cleanup story docs/en/design/03 will need.

### C5.a — Implicit drop; no user hooks beyond RC reuse

```
const xs2 = make_list()               // compiler inserts the drop at last use / scope end
                                      // no deinit anywhere; File/Socket cannot exist as safe types
```

Prior art: Koka and Roc expose no user drop for ordinary values (research/01 §B.1, §B.2); Perceus deferred drops amortize deep frees (research/01 §B.12 item 2); Swift's ARC likewise has no cycle collector and relies on conventions for cycles (research/01 §B.11).

| Dimension | Assessment |
|---|---|
| Learning cost | Lowest |
| Runtime perf | Best for plain data: concrete drops inline, no virtual drop dispatch |
| Expressiveness | Cannot define resource types (File, Socket, Lock) — there is no hook to close them |
| Compiler complexity | Lowest |
| Forecloses | C5.c entirely and C1.b's resource layer with it (a `~Copyable` File without deinit is unusable — SE-0390's deinit is the whole point, research/01 §B.3); design/03's cancellation-cleanup story (research/02 §b.8: linear destructors run at fiber abort) |

**Verdict — no.** Even the two inference-first languages keep deinit for resources; research/01 §B.12 item (3) makes deterministic finalizers a checklist item, not an option.

### C5.b — Explicit drop/discard for linear values + definite-initialization checking

```
const buf = alloc_buffer(1024)        // linear here: consume exactly once
drop(buf)                             // required, exactly once; double-drop is a compile error
fn f(cond: Bool) -> Int {
  let x: Int                          // uninitialized; DI tracks Raw/Uninit/Init per path
  if cond { x = 1 } else { x = 2 }
  x                                   // ok: initialized on every path
}
```

Prior art: Austral's consume-once discipline with an abstract interpreter over per-variable states and the loop-depth rule that keeps the checker a single forward pass (research/01 §B.5); Hylo's definite-initialization abstract interpreter over Raw/Uninitialized/Initialized/Partial states, which also inserts `deinit` instructions (research/01 §B.3); Move's explicit `copy`/`move` (research/01 §B.11).

| Dimension | Assessment |
|---|---|
| Learning cost | Moderate-high: the drop discipline shapes all resource code; casual closures over linear values become impossible (research/01 §B.5) |
| Runtime perf | Perfect: checker-only, zero runtime cost |
| Expressiveness | Full explicit control; but manual drops under early returns and panics are exactly the bug class RAII exists to remove (Vale built higher-RAII for this reason, research/01 §B.7) |
| Compiler complexity | Moderate and well-understood: one forward pass (Austral) + DI interpreter (Hylo) — both are infrastructure this catalog needs anyway |
| Forecloses | C5.a for linear values; C5.c's ergonomics (every close is manual); nothing else — DI is required infrastructure under every option |

**Verdict — no as the universal rule, yes as infrastructure.** Adopt the DI checker (every option needs it — research/01 §B.3) and keep an explicit `drop` as a debug/FFI tool on top of (c); making users write every drop recreates Austral's "consume-once discipline shapes all code" cost (research/01 §B.5) for a general-purpose scripting language.

### C5.c — RAII-style deinit hooks guaranteed by linearity

```
struct File: ~Copyable { fd: RawFd    // deinit is only expressible on the linear layer
  deinit { close(self.fd) } }         // runs exactly once, at the unique instance's death
fn with_file(path: Str, body: fn(mut File) -> T) -> T {
  const f = open(path)                // linearity: f cannot be copied or silently dropped
  body(f)                             // deinit runs at f's unique death — including on panic paths
}
```

Prior art: Swift SE-0390 — noncopyable structs/enums "can also have `deinit` declarations... which run automatically at the end of the unique instance's lifetime", with no heap allocation required (research/01 §B.3); Vale's higher-RAII: "ownership in the type system guarantees a future call happens; destructors can take arguments and return values", enabling structured concurrency and replayable debugging (research/01 §B.7); Austral's linear methods (close/commit/cancel protocols, research/01 §B.5); research/02 §b.8: linear destructors are the finalizer hooks at fiber abort.

| Dimension | Assessment |
|---|---|
| Learning cost | Low: one concept for resource authors only; resource users write nothing |
| Runtime perf | Deterministic drops at last use with finalizers inlined (research/01 §B.12 item 3) |
| Expressiveness | Full resource story: files, locks, sessions, FFI handles, RAII protocols |
| Compiler complexity | Needs C5.b's DI + last-use analysis — infrastructure already required; deinit dispatch is a static call, not a virtual one, because the concrete type is known |
| Forecloses | C5.a; requires C1.b's linear layer to define which types may declare deinit (a copyable type has no unique death point — SE-0390's exact rule, research/01 §B.3); interacts with D3.b: a multi-shot resumption cannot capture a `~Copyable` value (research/02 §b.8 sharp edge) |

**Recommendation — (c).** Deinit restricted to the `~Copyable` linear layer buys RAII without Rust's Drop-everywhere complexity, supplies design/03's cancellation cleanup for free (research/02 §b.8), and matches the value-first staging of research/01 §d Combo 6.

### C5.d — Drop hooks only via effect handlers / finalizers (deferred)

```
handle {
  use(r)                              // cleanup delivered by the enclosing handler scope
} with res <File> { ... }             // resource release = handler logic, not a destructor
```

Prior art: Koka ships `finally`/`initially` combinators precisely because multi-shot resumption makes RAII-conditional-on-single-resumption wrong (research/02 §b.1); research/02 §a: "multi-shot resumption + linear resources interact badly — which is precisely why Koka has `linear effect` (tail-resumption only) and why Effekt restricts to lexical handlers"; Effekt's lexical handlers bounding resource lifetime (research/02 §b.2).

| Dimension | Assessment |
|---|---|
| Learning cost | Deferred, not avoided: v0 ships with no resource story at all |
| Runtime perf | Handler-entry frame capture at every resource boundary (research/03 §b.12 risk 4) |
| Expressiveness | Resources tied to dynamic scopes; no first-class resources |
| Compiler complexity | None until design/03's effects decision lands — then the full effects machinery |
| Forecloses | C5.c; makes C1.b resource types unusable until effects exist; couples the memory design to the effects design — the very coupling the charter's decoupling-first philosophy rejects (charter philosophy; research/01 §B.4 for the Rust analogue) |

**Verdict — no for v1.** Revisit only if docs/en/design/03 lands full multi-shot effects; note that Koka ships deinit-style resources and handler combinators side by side (research/02 §b.1), so (c) does not preclude (d) later.

## D1 — Mutation confinement

### D1.a — Function-local var only

```
var acc = 0                           // the only mutable binding kind
for x in xs { acc += x }              // no callee-side mutation of any kind
const out = normalize(acc)            // everything else consume-and-return
```

Prior art: Koka's `var` locals (research/01 §B.2); research/02 §b.7's "no shared mutable state" consequence of linear+immutable.

| Dimension | Assessment |
|---|---|
| Learning cost | Lowest |
| Runtime perf | Good via FBIP, but in-place algorithms need rebind chains or builders |
| Expressiveness | No mutating methods, no subscript setters — the Hylo/Swift `inout`/`mutating` idiom is gone |
| Compiler complexity | Lowest |
| Forecloses | D1.b; makes C2.d's `mut` mode unusable (windows are callee-side mutation); pushes D2 toward builder-heavy APIs |

**Verdict — no.** It spends real expressiveness to buy no additional safety over (b): windows are statically exclusive and lexically visible, so "locally-visible mutation" (charter req 5) already holds.

### D1.b — Exclusion-window mutation

```
var acc = Account.zero()
deposit(acc, 100)                     // deposit takes `mut Account`: the only mutation channel;
                                      // one exclusive mutable borrow for the call, statically proven
const snapshot = acc                  // outside windows, everything is shared-immutable
```

Prior art: Hylo's `inout` (research/01 §B.3); Swift's `mutating` with dynamic exclusivity as the safety net (research/01 §B.3); research/01 §d Combo 3.

| Dimension | Assessment |
|---|---|
| Learning cost | Low: mutation is always a named window at a call site |
| Runtime perf | Best: alias-free windows need no runtime checks (research/01 §d Combo 3) |
| Expressiveness | Full in-place algorithms, mutating methods, subscript setters |
| Compiler complexity | Moderate; identical machinery to C2.d — one checker, three uses (parameters, windows, confinement) |
| Forecloses | D1.a/c/d; shared mutation permanently (by design); requires C2.d |

**Recommendation — (b).** One mechanism confined by construction to owned receivers; the window is the call, so mutation is locally visible without a discipline to enforce.

### D1.c — Capability-gated mutation

```
const cap = bank.mut_capability()     // capability is a first-class (linear?) value
transfer(cap, from, to)               // mutating ops require a `mut Cap` in scope;
                                      // signatures audit who may mutate what
```

Prior art: Koka's permission-style atomic effects (`net`, `fsys`, `ui`, `blocking`) acting as checked capability names in effect rows (research/02 §b.1); Effekt's capability passing (research/02 §b.2); research/02 §d Combo 2's "capability-passing async" as the Zig/Effekt synthesis; the effects/capability-IO decision itself is owned by docs/en/design/03 — referenced, not decided here.

| Dimension | Assessment |
|---|---|
| Learning cost | Moderate: capability threading is noisy ("pass the io", research/02 §d Combo 2) |
| Runtime perf | None beyond ordinary parameter passing |
| Expressiveness | Auditable mutation — but a capability gates *who*, not *which alias is exclusive*; exclusivity is still needed underneath |
| Compiler complexity | Low locally, but it couples this catalog to design/03's effects decision |
| Forecloses | Independence from design/03 (if capability-IO is rejected there, this option loses its substrate); D1.b remains required regardless, so (c) can only ever be an additive layer, never the answer |

**Verdict — no in this catalog.** Exclusivity (b) and capability gating (c) solve different problems; if docs/en/design/03 adopts capability-IO, a mutation capability can be layered on top of (b) later without semantic change — the graduation path research/02 §d Combo 2 describes.

### D1.d — Mutation via linear destructors returning new values only (pure surface)

```
const s2 = stack_push(s, x)           // "mutation" = consume-and-return
const (top, s3) = stack_pop(s2)       // pure surface; compiler reuses in place
```

Prior art: Koka's FBIP (research/01 §B.2); Clean's uniqueness typing letting pure functions destructively update unshared arrays behind a pure surface (research/01 §B.6); research/03 §b.9's note that one runtime can expose both modes.

| Dimension | Assessment |
|---|---|
| Learning cost | Zero syntax, long chains: algorithmic code reads as dataflow plumbing |
| Runtime perf | Equal to (b) via reuse analysis |
| Expressiveness | Poor for in-place algorithms — the awkwardness is what FBIP+TRMC research addresses by adding loops and recursion-modulo-cons, not by banning windows (research/01 §B.2) |
| Compiler complexity | Reuse analysis required (research/03 §b.6) |
| Forecloses | D1.b and C3.b entirely; subscript-style in-place element access (C2.d's setter accessors, research/01 §B.3) becomes impossible |

**Verdict — no as the only channel.** It is the right runtime behavior for the functional surface (C4.a delivers it), but algorithms need (b)'s windows; Koka itself keeps `var` locals alongside FBIP (research/01 §B.2).

## D2 — Collections

research/03 §b.9 sets the frame: "one runtime can expose both: Array (unique, growable, zero-copy ops) vs Shared/boxed persistent structures", with the cost asymmetry "persistent update allocates O(log n) RC'd nodes... affine in-place update allocates zero".

### D2.a — Persistent/structural-sharing only (HAMT etc.)

```
const m2 = insert(m, k, v)            // HAMT: O(log n) new nodes; m untouched
const xs2 = xs.push(x)                // persistent vector; free snapshots everywhere
```

Prior art: research/03 §b.9 — "immutable-by-default enables structural sharing safely (RC'd persistent vectors/maps)"; the value-representation groundwork for RC'd persistent nodes is research/01 §B.12 item (1).

| Dimension | Assessment |
|---|---|
| Learning cost | Low: one mental model, everything is a value |
| Runtime perf | Reads fine; every update allocates O(log n) RC'd nodes — the exact asymmetry research/03 §b.9 flags; no zero-copy buffers for FFI or hot loops |
| Expressiveness | Snapshots and undo are free; in-place buffering impossible |
| Compiler complexity | Low (stdlib machinery, not compiler) — but scalar replacement cannot help opaque heap nodes (research/03 §a.7) |
| Forecloses | D2.b's in-place API; FFI-grade Vec/Bytes (D2.d); the compiled-language performance charter (req 3) for data-heavy scripts |

**Verdict — no as the only story.** Persistent maps/sets belong in the stdlib as ordinary library types on the same RC runtime (research/03 §b.9), not as the mandated collection model.

### D2.b — Linear-mutable only: functional API with in-place implementation

```
const xs2 = append(xs, x)             // "new" list; overwrites xs's buffer iff uniquely owned
const m2 = insert(m, k, v)            // map update: in-place when m is unique, else copies
sort(xs)                              // in-place via the C3.b window (mut mode)
```

Prior art: Koka's FBIP lists with the guaranteed-reuse quote (research/01 §B.2); Roc's builtin ownership contracts — Borrow/Consume/Copy-on-Write/Seamless Slice from the glossary (research/01 §B.1); research/03 §b.9.

| Dimension | Assessment |
|---|---|
| Learning cost | Low: one rule — "updates are copy-on-write; aliasing means copying", surfaced as a perf note, never an error (research/01 §d cross-cutting note 4) |
| Runtime perf | Near-C when unique; O(n) copy when shared — Roc issue 12420 shows this flip side is observable and real (research/01 §B.1) |
| Expressiveness | Both functional and imperative styles readable; persistent snapshots available as library HAMT types; unboxed buffers fit as the unique corner |
| Compiler complexity | Requires C4.a's reuse pairing to deliver the promise — already budgeted |
| Forecloses | D2.a-only; nothing else — persistent types are ordinary library values, and D2.d's buffers are simply the unboxed members of this same model |

**Recommendation — (b).** COW-by-unique is the single semantics under which "immutable by default" (charter req 5) and "performance approaching compiled languages" (charter req 3) are both true, and it is exactly the shipped Koka/Roc model (research/01 §B.1, §B.2).

### D2.c — Dual-mode: persistent by default + linear builder types for hot paths

```
const m2 = insert(m, k, v)                      // persistent by default (HAMT)
const b = Builder<List<Int>>.init()             // explicit linear builder for hot paths
const xs = b.freeze()                           // after b.push(1); b.push(2): consume, produce value
```

Prior art: research/03 §b.9 (both modes on one runtime); Vale's HGM philosophy of selectively applying linear style in hot paths (research/01 §B.7).

| Dimension | Assessment |
|---|---|
| Learning cost | Moderate: two collection mental models plus the when-to-switch decision |
| Runtime perf | Good where builders are used; the persistent default penalizes the common case with O(log n) node churn |
| Expressiveness | High, but doubled |
| Compiler complexity | Two implementations to maintain per collection — against the sugar/orthogonality budget (charter req 4, philosophy) |
| Forecloses | D2.b as the default; doubles the stdlib surface; the switch point becomes a teaching topic — research/01 §B.7 itself cautions that juggling multiple memory styles is a real cost ("pushing checks to zero Rust-style just shifts costs into cloning") |

**Verdict — no.** (b) already degrades gracefully: COW gives persistent semantics with reuse when unique, so a separate persistent default doubles the API for little gain.

### D2.d — Pragmatic hybrid: small values persistent, big buffers linear-mutable

```
const tags = Set.of("a", "b")                     // small: copied freely, no aliasing machinery
const buf: mut Bytes = Bytes.with_capacity(4096)  // big: unique, unboxed, zero-copy ops
buf.write_u32_le(n)
```

Prior art: Inko's value types — Int/Float/String/FFI structs copy on move, so small values escape the aliasing machinery entirely (research/01 §B.8); Lobster's inline immutable structs reducing ownership pressure (research/01 §B.9); Inko 0.20.0's single-allocation 16-byte string economics (research/03 §b.4); research/03 §b.9's unique-growable Array.

| Dimension | Assessment |
|---|---|
| Learning cost | Low but with an extra rule to teach: where is the small/big line, and why did my update copy? |
| Runtime perf | Best FFI/interop and buffer story: unboxed unique buffers ride the typed-register frame model (research/03 §a.6, §a.7) |
| Expressiveness | Pragmatic; but the classification leaks into API design everywhere |
| Compiler complexity | Unboxed unique buffers need distinct typed opcodes — cheap under research/03 §b.1's typed-opcode plan |
| Forecloses | As a classification rule it competes with (b)'s uniform COW; as a stdlib layer it complements (b) — `Bytes`/`Vec` are simply the unboxed linear types of the same model, research/03 §b.9's exact suggestion |

**Verdict — no as the classification rule, yes as the stdlib consequence.** Under (b), small immutable values already copy by value semantics and big buffers exist as unique unboxed types; the hybrid is an outcome, not a separate decision.

## D3 — Iterators and generators

The language needs `for` loops, combinator chains, and a way for library authors to define custom iteration; research/03 §a.10 and §b.7 supply the key number (MoonBit's compiler-level fusion over a zero-cost `Iter` type: 25x over native JS iterator helpers) and research/02 §b.8 supplies the constraint for any generator design (multi-shot resumption must not capture linear resources).

### D3.a — Linear cursors (external iterator owning a borrow; move = consume)

```
const it = xs.cursor()                // it: Cursor<Int> — owns a borrow of xs (second-class)
const v = it.next()                   // consuming: it is dead after next (move = consume)
for v in xs { ... }                   // bounds checks eliminated by the cursor discipline (research/03 §b.7)
```

Prior art: Rust's external-iterator model as the affine ancestor (research/01 §B.4's affine move rules govern the cursor); "move = consume" is Inko's move-error rule (research/01 §B.8); research/03 §b.7 — "eliminate bounds checks where a linear cursor discipline (e.g., for over a slice) has already proven range containment"; Hylo answers external iteration with subscripts precisely because second-class borrows cannot be stored (research/01 §d Combo 3).

| Dimension | Assessment |
|---|---|
| Learning cost | Low for users; one protocol for authors |
| Runtime perf | Excellent: fusion-friendly, bounds-check elimination by linearity (research/03 §b.7) |
| Expressiveness | Cursors are values but their borrows are not (C2.d): a cursor over a mutably-borrowed structure cannot be stored inside another structure |
| Compiler complexity | Moderate: cursor type + desugaring + fusion pass |
| Forecloses | D3.b (two iteration stories); requires C2.d (the cursor holds a borrow); as the public protocol it duplicates what `for` + combinators can hide |

**Verdict — viable, but as the internal lowering rather than the public protocol** — adopt inside (d), with an opt-in cursor type for low-level code.

### D3.b — First-class generators via VM fibers

```
gen fn read_records(f: File) -> Record {         // generator = a fiber in the VM
  while true { yield parse_next(f) }             // yield suspends the fiber
}
for r in read_records(file) { ... }              // for drives the fiber
```

Prior art: research/02 §b.8 — heap-allocated stackful coroutines are cheap in a bytecode VM (Lua/LuaJIT precedent); the sharp edge, quoted: "multi-shot resumption = replay closures (must not hold linear resources — type system must forbid capturing linear values in multi-shot continuations, or restrict effects to tail-resumption à la linear effect)" (research/02 §b.8; §a); Koka's `linear effect` as the typed tail-resumptive safe subset (research/02 §b.1); the async/fiber design itself is owned by docs/en/design/03 — referenced, not decided here.

| Dimension | Assessment |
|---|---|
| Learning cost | Low surface; the linear-capture rule is one more thing to teach |
| Runtime perf | A fiber switch per element versus a fused loop — MoonBit's 25x-over-JS-iterators number measures what that costs on hot paths (research/03 §a.10) |
| Expressiveness | Highest: tree traversal, infinite streams, custom iteration are trivial |
| Compiler complexity | VM fiber support plus the linear-capture typing rule; couples to design/03's async machinery — one fiber mechanism would serve both, if design/03 chooses fibers |
| Forecloses | D3.c/d (combinators become sugar over generators, and guaranteed fusion is lost); adoption requires design/03's fiber decision first; if adopted later, research/02 §b.8's rule is mandatory — forbid linear capture in multi-shot resumptions, or type resumptions affine unless marked |

**Verdict — not in v1.** Powerful but couples this catalog to docs/en/design/03 and pays per-element suspension costs; the door stays open because (d)'s hidden lowering can upgrade to fibers without surface changes.

### D3.c — Combinator/fusion only (no iterator protocol at all)

```
xs.map(sqr).filter(is_odd).sum()      // one fused loop; no intermediate collections
xs.map(f).zip(ys).collect()           // compiler-level fusion; custom iteration: manual loops only
```

Prior art: MoonBit's compiler-level iterator fusion and 25x number (research/03 §a.10, §b.7, §13); Roc's compiler doing the adjacent work (`trmc`, `loop_append_promote`, `tag_case_fusion`) on a language with no user iteration protocol (research/01 §B.1); Gleam as the statically-typed combinator-style language on BEAM (research/02 §b.6).

| Dimension | Assessment |
|---|---|
| Learning cost | Lowest: TS/Python map/filter habits transfer directly |
| Runtime perf | Best on fusible chains (research/03 §a.10) |
| Expressiveness | Non-fusable patterns (irregular multi-collection walks, stateful pipelines) fall back to manual loops; user-defined iteration needs some protocol and this option provides none |
| Compiler complexity | Fusion pass over typed SSA — directional rules in the Cranelift style (research/03 §b.7) |
| Forecloses | D3.b; library-defined iteration is stuck with hand-written loops or subscripts; hard to grow into (d) without deciding where the hidden protocol lives |

**Verdict — close, but (d) is strictly safer.** Same surface, with a defined fallback for non-fusable shapes.

### D3.d — Combinators + hidden generators (compiler fuses chains into loops; users see combinators only)

```
xs.map(sqr).filter(is_odd).sum()      // fused into one loop (typed-SSA rule, research/03 §b.7)
for (k, v) in entries(m) { ... }      // for desugars to a hidden cursor — never user-visible
fn primes(upto: Int) -> List<Int>     // library authors: combinators + subscripts (C2.d), no gen syntax
```

Prior art: MoonBit's zero-cost `Iter` plus compiler fusion, 25x over JS iterator helpers (research/03 §a.10, §b.7); Koka's TRMC turning functional recursion into in-place loops — the same philosophy applied to recursion (research/01 §B.2); CPython's static macro-op composition as the uop-level analogue of lowering chains into single units (research/03 §b.1).

| Dimension | Assessment |
|---|---|
| Learning cost | Lowest surface; one hidden concept (the cursor) that only stdlib authors meet |
| Runtime perf | Fused fast path; the hidden-cursor fallback keeps semantics uniform where fusion fails |
| Expressiveness | Combinators + `for` cover nearly all iteration; custom iteration via C2.d subscripts or the opt-in linear cursor (D3.a); user-facing generators can be added later as library sugar without surface breakage |
| Compiler complexity | Fusion rules plus one hidden lowering — moderate; the hidden layer can start as a plain cursor and upgrade to fibers later if design/03 lands them |
| Forecloses | D3.b as public syntax in v1 (reversible later); nothing else — it subsumes (c) and gives (a) a home as the internal protocol |

**Recommendation — (d).** Combinators are the surface, linear cursors the hidden protocol, fusion the optimization, and design/03's fiber decision (if it lands) upgrades the hidden layer without touching user code — the same "hidden mechanism, stable surface" staging research/01 §d Combo 6 applies to ownership.

## Cross-point interactions

| Pairing | Status |
|---|---|
| C1.b + C4.a | Natural pairing: affine values have statically-known rc==1, so dup/drop degenerate to clone/move boundaries (research/03 §b.6) — the pairing the research names (research/01 §d Combos 3+4) |
| C2.d + C3.b + D1.b | One mechanism (`mut` modes), three uses: parameter mode, exclusion window, mutation confinement |
| C5.c + D3.b | Conflict: a multi-shot resumption must not capture a `~Copyable` value (research/02 §b.8) |
| C4.c + any C1 option | A moving GC conflicts with precise RC, interior pointers, and FFI (research/01 §B.12) |

## Coherent combos

Three end-to-end combinations the author can adopt wholesale; Combo A is what the per-point recommendations assemble, B and C are the honest alternatives from the research. Staging applies to all three (research/01 §d Combo 6, research/03 §a.8): build the affine checker and DI analysis first, then precise RC with runtime refcount==1 checks plus the Inko-style borrow counter for diagnostics, then per-frame arenas and guaranteed TCO, then reuse pairing, borrow inference, TRMC, and fusion as pure optimizing passes with zero language change — every late pass replaces a runtime check with a proven fact without observable semantic change (research/01 §d cross-cutting note 5, §B.9, §B.10).

### Combo A — "Modes and RC" (Hylo/Swift surface over Perceus): C1.b, C2.d, C3.b, C4.a(+d), C5.c, D1.b, D2.b, D3.d

```
// Combo A — illustrative; final surface syntax decided in docs/en/design/04-syntax.md
struct File: ~Copyable {                 // C1.b linear resource layer (set-theoretic negation)
  fd: RawFd
  deinit { close(self.fd) }              // C5.c: runs exactly once, at unique death
}
fn read_u32(f: mut File) -> u32 | IoError {   // C2.d/C3.b: exclusive borrow for the call
  const b = f.read_exact(4)?            // window on f; ? propagates IoError; b is a fresh linear buffer
  b.to_u32_le()                         // b consumed here (affine move)
}
fn sum_file(path: Str) -> u32 | IoError {
  const f = open(path)                  // unique owner; cannot be copied
  var total: u32 = 0                    // C3.b: local var — the only rebindable binding
  var n: u32 = 0
  while n < 1000 { total += read_u32(f)?; n += 1 }   // ? moves the error out; f's deinit still runs
  total                                 // f dies here: deinit closes fd (also on early return)
}
const words = "alpha beta gamma".split(' ')          // D2.b COW: "new" list, reuses when unique
const lens  = words.map(w => w.len()).collect()      // D3.d: fused into one loop by the compiler
const total = (sum_file("metrics.bin") or 0) + lens.len()  // D1.b: mutation only inside windows/locals
```

Verdict: learning cost low-moderate (two mode keywords for everyday code, `~Copyable` + deinit for resource authors); performance near-C on functional paths — Perceus reuse plus compile-time escape analysis that linearity makes exact (research/03 §a.7, §b.6); expressiveness high for value-oriented code, no first-class references, graphs via index/id patterns (research/01 §B.4 workaround list). Choosing it means: for docs/en/design/01, express copyability/linearity as negated set-theoretic constraints so `~Copyable` composes with unions (research/01 §a, cross-cutting note 1); for docs/en/design/03, linear values are `sending` for free (research/01 §B.3, SE-0430) and linear destructors are the cancellation cleanup hooks (research/02 §b.8) — no function coloring; for docs/en/design/04, the surface budget is `mut` mode + `~Copyable` + `deinit` only, within Kotlin's ceiling (research/01 §c Table 3, Hylo row).

### Combo B — "Inference and RC" (Koka/Roc hidden flavor): C1.c, C2.b, C3.d(+a), C4.a, C5.a, D1.a, D2.b, D3.c

```
// Combo B — illustrative; final surface syntax decided in docs/en/design/04-syntax.md
// No ownership, borrow, or mutation syntax exists anywhere in this file.
fn map(xs: List<Int>, f: fn(Int) -> Int) -> List<Int> {
  match xs {
    cons(h, t) => cons(f(h), map(t, f))  // FBIP: cells overwritten in place when xs is unique
    nil        => nil
  }
}
fn sum(xs: List<Int>, acc: Int) -> Int { // guaranteed TCO + TRMC: compiles to one loop
  match xs {
    cons(h, t) => sum(t, acc + h)
    nil        => acc
  }
}
const total = sum(map(list_range(1, 1000000), x => x * x), 0)  // zero annotations; zero allocations
                                          // when unshared; if shared, copies happen — a perf note
fn with_log(path: Str, body: fn(Log!) -> a) -> a {  // resources: the one opt-in linear corner
  const lg = open_log(path)               // lg: Log! — consumed exactly once
  const r  = body(lg)
  close_log(lg)                           // checker: consumed on every path
  r
}
```

Verdict: learning cost lowest of all surveyed designs (research/01 §c Table 2, Koka/Roc/Lobster rows); performance near-C for fusible functional workloads with reuse guaranteed and observable (research/01 §B.2); expressiveness high for value code but with no user way to state sharing contracts, and resources pushed into a narrow opt-in layer. Choosing it means: for docs/en/design/01, charter req 2 must be re-worded — "static linear types" becomes "static uniqueness inference over an affine IR" (the honest tension documented at C1.c); for docs/en/design/03, multi-shot effects must forbid capturing linear values in resumptions or restrict to tail-resumption (research/02 §b.8); for docs/en/design/04, zero keyword budget but an error-message policy of performance notes instead of ownership errors (research/01 §d cross-cutting note 4), and the compiler team ships the hardest pass first (Roc's `arc_solve` as the template, research/01 §B.1).

### Combo C — "Guarantees" (Austral/Move explicit flavor): C1.a (fixed multiplicities), C2.a, C3.a, C4.d(+RC at escape), C5.b, D1.a, D2.b, D3.a

```
// Combo C — illustrative; final surface syntax decided in docs/en/design/04-syntax.md
// Every binding is exactly-once (linear); sharing only via block-scoped borrows.
linear type Bytes { ptr: RawPtr, len: Int }      // linear by default in this flavor
linear fn parse(raw: Bytes!) -> (Header!, Bytes!) {  // consume raw exactly once, split it
  const (h, rest) = split_at(raw, 64)
  (h, rest)
}
fn main() -> () {
  const raw = read_all("blob.bin")        // raw: Bytes!
  const (header, body) = parse(raw)       // raw consumed; checker: exactly once, every path
  borrow header as h in {                 // block-scoped read borrow (no first-class refs)
    print(h.magic)
  }
  const payload = decode(body)            // body consumed
  drop(payload)                           // explicit drop: required exactly once (C5.b)
  free_header(header)                     // consumed before scope end, or compile error
  // loop-depth rule: a linear bound outside a loop cannot be consumed inside it (research/01 §B.5)
}
```

Verdict: learning cost highest (the consume-once discipline shapes all code; casual closures over linear values are impossible — research/01 §B.5); performance with perfect determinism, zero runtime cost, and trivially safe arenas, since escape is a typing fact (research/03 §a.7); expressiveness lowest of the three — research/01 §B.5 calls this model "the right model for an opt-in resource layer and the wrong default for a general-purpose interpreted language". Choosing it means: for docs/en/design/01, multiplicity annotations interact with generics — fix multiplicities per definition site or drown in UR constraints (research/01 §B.6); for docs/en/design/03, linear (tail-resumptive) effects are natural but multi-shot handlers are heavily constrained (research/02 §b.1, §b.8); for docs/en/design/04, the heaviest surface of the three (`linear`, `borrow`, `drop` keywords), and the short-tutorial explainability budget (charter philosophy) is spent almost entirely on ownership.
