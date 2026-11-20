# DreamScript working design decisions

Status: the authoritative working record of every language decision (44 points: A1-A3, B1-B9, C1-C5, D1-D3, E1-E5, F1-F9, G1-G4, H1-H6), authored 2026-11-08 from the frozen `design/` archive and amended through 2026-11-20. `design/` is read-only; lowering mechanics and measured constants live in `docs/en/pitfalls/` and the code.
Source map under `design/en/design/`: `08-decisions.md`, `01-types.md`, `02-linearity-ownership.md`, `03-effects-errors-async.md`, `04-syntax.md`, `05-generics-metaprogramming.md`, `06-modules-tooling.md`, `07-runtime.md`, `09-decision-rationale.md`, `00-decision-map.md`.

## 0. Author supplements — binding

1. The main direction follows the Phase 3 record and its rationale.
2. Casing carries no grammatical meaning; types are spelled lowercase.

## 1. The decided language

Set-theoretic types over nominal tags with boundary signatures and flow refinement; affine values with an opt-in linear layer; Perceus RC with block reuse and no GC; coherent traits over one generic body; comptime reflection without macros; errors as union members; VM fibers in mandatory nurseries with fork-join parallelism; an evidence-passing handler runtime with capability IO; three tiers with guaranteed TCO.

## 2. Identity and compatibility

- **A1 — standalone core, embedding via platform packages.** The standalone runtime is the only architecture; embedding arrives as ordinary packages exporting effect handlers over it, the default platform the `ds` CLI.
- **A2 — workloads.** CLI/tools/scripts primary, games/simulation secondary (interpreter-first startup, script mode, two-slot hot reload); numeric/AI reaches the Cranelift tier without shaping v1.
- **A3 — compatibility.** Rapid-break until 1.0, semver after; content addressing applies at package level (H6), never to source identity.

## 3. Type system

### B1 — hybrid set-theoretic types over nominal tags

Nominal declarations carry identity; unions, intersections and differences of nominal atoms are ordinary type expressions. Exhaustiveness checks by tag identity, differences erase at compile time, a union is its unboxed tag+payload layout, an intersection keeps the most precise member's layout, inference failure a compile error with no `any` fallback. Fallback B1b: a nominal-only core if the disjointness checker slips schedule.

### B2 — bidirectional checking with boundary signatures, plus flow refinement

Boundary signatures required at top level and on public items; locals, lambdas and arms infer bidirectionally. Refinement narrows through `is`/none tests and arms, reset at assignment; `?` is containment in the written union. Limit: no narrowing through mutable union fields. Rulings: a value-level arm subtracts nothing (2026-11-12), so later arms check the full scrutinee and arm ordering is refused; per-element refinement is out (2026-11-15), singleton subtraction reintroducing arm-order dependence.

### B3 — nominal structs/enums plus anonymous tuples

Named data is nominal structs and enums (declared layouts, tuple-enums, punning); ad-hoc aggregates are anonymous tuples with destructuring. Enum blocks the only payload-case form, `tag x;` declares unit tags, type aliases never declare cases (2026-11-10). Construction fixed per declaration, a cross-spelling a checker error. v1 has the list literal; map and anonymous record literals stay deferred additive; row types rejected.

### B4 — `T?` is exactly `T | none`

Optionals, error unions and enums share one mechanism (`is`, match, `?` with E4's extraction family), `none` a unit type among several. No `?.` in v1; `??` on `t | none` is the single recorded operator valve, rejected by default and reopenable only through the M9 backlog triage on corpus evidence that `t | none` fallback dominates (ruled 2026-11-17). Limit: `V | none` where `V` may itself be `none` is ambiguous, so wrap explicitly.

### B7 — numerics: i64/f64 defaults, trapping overflow

`int` is i64, `float` f64, raw in unboxed slots; conversions always explicit calls. Integer overflow, `1 / 0` and `int_min / -1` trap; `+%`/`-%`/`*%`/`-%a` wrap for hash and PRNG use; floats follow IEEE and never trap; error-union arithmetic variants are library functions; `Dec` and units-of-measure are nominal library types plus derives. Ruling (2026-11-15): a shift amount outside `0..64` traps, `>>` sign-propagating, `&`/`^`/`|` never trap. Width construction ruling (2026-11-19): the u32 trapping conversion is an ambient member over the checked `u32_of` and the bare `trap` primitive, `u32_of` the fallible form, spelled `u32.of(x)` because the bare `u32(x)` is unlandable (DS2002 call-form resolution; an ambient value named `u32` makes every call form DS2014-ambiguous under F9). Open question: whether the bare spelling warrants a checker round amending F9's priority.

### B8 — strings: immutable UTF-8, checked slices

Immutable UTF-8 bytes behind one raw pointer plus byte length. Slices are non-owning borrows kept alive by the parent's RC, landing on char boundaries through a checked API returning `string | none`; equality is O(n) with length fast-reject. Ruling (2026-11-12): string-operand `+` spells the concat native, `+=` the same operation, mixed operands refusing at both layers. Surface cleanup (2026-11-17): `substring` and `char_at` answer the checked forms (`string | none`, `char | none`); `head` retires, `first` the one list-front spelling.

### B9 — universal structural `==` with five carve-outs

One structural `==` over all data, total by construction, never user-overloadable. Carve-outs: function comparison rejected statically; floats compare bit-structurally (`nan == nan` true) with named `is_float_eq`/`is_approx_eq`; mutable cells and `~Copyable` resources compare by identity; operands borrowed; `hash` stays consistent. `Ord`/`hash`/`show` are derives, ordering never structural. Ruling (2026-11-14): `fn hash(self) -> int` over a borrowed receiver with ambient primitive impls, float normalizing `-0.0` and every NaN payload, one seed folded per part. Ruling (2026-11-21): collection content equality and map key matching ride the same bit-structural word equality as scalar `==`; `scalar_hash` hashes bits; `-0.0` and `0.0` are distinct keys; NaN keys are reflexive and overwritable. (Closes the 2026-11-20 review B10 question, todo item 84.)

## 4. Linearity, ownership, memory, data

### C1 — affine core, opt-in `~Copyable` linear layer

Assigning an affine value moves it, and a moved binding is dead. `~Copyable` subtracts the default copyability constraint, carries `deinit` and admits phantom-state unions; affine checking is an intraprocedural last-use/DI pass. The `rc==1` uniqueness checks plus a borrow counter ship as the diagnostic layer. Ruled 2026-11-17: this is the ownership endgame, no lifetime-annotation or GC escape hatch reserved.

### C2 — second-class parameter-passing modes

Three modes fixed at the definition site — default borrow, `mut` (exclusive), consume — with no mode syntax at call sites. Borrows never escape a call, so checking is intraprocedural and no lifetime annotations exist; first-class references permanently out. Subscripts extend the modes to element access.

### C3 — `const`/`var` bindings, mutation through `mut` windows

`var` is a rebindable local; callee-side mutation goes through `mut` parameter windows — one mechanism serving parameters, windows and confinement (D1) — with zero runtime checks inside (alias-freeness proven).

### C4 — Perceus precise RC + block reuse over per-frame bump arenas

Precise non-atomic RC (bump per clone, dec per unique death, each drop a reuse candidate) over per-frame bump arenas holding non-escaping temporaries, sound because linearity makes escape a compile-time fact. Ruling (2026-11-12): every count mutation runs through the owning heap's count funnels, a foreign heap only posting requests drained to quiescence. Clone-family amendment (2026-11-15): `fn clone(self) -> Self` over a borrowed receiver, the derive constructing from one dispatch per part so the duplicate is shallow (a Ref dups by a count bump), adding no uop and no native. Unit-word ruling (2026-11-15): a part-less struct occupies one reserved word in every value position, never read. Fallback C4b: generational references wait on a quantified trigger, the moved-after-use rate crossing a threshold the tutorial feedback confirms is ownership-specific.

### C5 — `deinit` restricted to `~Copyable` types

`deinit` runs exactly once at the unique instance's death on every path, including early return, panic and cancellation teardown, dispatching statically. `drop` is a debug/FFI tool only.

### D1 — exclusion-window mutation is the only channel

Mutation is always a named call on an owned receiver with a `mut` mode: statically exclusive, lexically visible as the call, zero runtime checks; shared mutation unrepresentable.

### D2 — linear-mutable COW collections

Collections expose a functional API and mutate in place iff uniquely owned (guaranteed, observable reuse); aliasing means copying, a perf note and never an error. Ruling (2026-11-10): a `mut` window parameter takes a `var` binding at the call site whenever its argument copies, a `const`-binding receiver being DS3006; COW-on-alias is rejected, the hidden copy keeping mutation invisible at the call site.

### D3 — combinators over a hidden linear cursor

The user surface is combinators (`map`/`filter`/`reduce`) plus `for`, the hidden protocol a linear cursor fused into loops by the compiler; no public generator syntax in 1.0. Iteration-count ruling (2026-11-20): a `for` over a list takes the element count once at loop entry, so a body appending to its own iterable never visits the appended elements this loop; a checker-side borrow-conflict refusal over the iterated window stays a queued refinement.

## 5. Errors, effects, async, parallelism

### B5 — errors are union members of return types

Errors are storable data, one predicted branch in the VM, carrying an effect row's information; a `throws(...)` spelling stays later sugar. Propagation requires the enclosing return union to contain the member (checked containment, no implicit conversions).

### E1 — no effect syntax in v1; handler-shaped runtime, capability idiom

The runtime is handler-shaped from day one: evidence passing serves `cancel` and `discontinue` at nursery boundaries, tail-resumptive with zero allocation. World-touching stdlib APIs follow the capability idiom — an ambient handler installed by the `ds` CLI, an explicit capability parameter for tests, embedding and scheduler choice. User-defined handlers stay a v2 additive escape hatch under a recorded adoption trigger (ruled 2026-11-17): only a real use case a capability parameter cannot cover cleanly (test injection, mid-tree policy switch), resumptions affine with the multi-shot valve closed, FFI calls pinning the fiber (H5).

### E2 — VM fibers with a mandatory nursery

Stackful heap fibers on the VM pool. `spawn` exists only inside `nursery` blocks; scope exit joins children, first failure cancels siblings, child errors union out at the boundary. Blocking-style code, no colors, no `await`; preemption a flag check on backward jumps; bare detach requires an explicit root nursery. Amendments (2026-11-12): re-entry of a parked child by the join/teardown driver is preemption-shaped, so a parked child of a cancelled nursery dies with the cancel sentinel to its resumer; a payloaded cancel rides the native channel's flat span. Lowering refuses bare `spawn` outside a nursery, a member outside the enclosing return union, and a payloaded member whose payload is a union or non-heap application.

### E3 — fork-join parallelism on the sendability substrate
Race-freedom and sendability are static consequences of linear/immutable data: `par_map`/`par_reduce` over immutable or partitioned inputs are deterministic, replayable and lock-free by theorem, RC counts thread-confined through the fork-side transfer. The join parks the call site through the interlock drive, nested calls correct at any depth, a fork at a tail dispatch site refusing with a structured trap (2026-11-13); `par_reduce` is a sequential fold with byte-identical answers, the `associative` marker trait rejected and the members migrating to @std rather than natives; a worker owns whole fibers, ownership transferring at spawn and back at join, keeping precise non-atomic RC sound, immutable code shared as a per-drive version-table snapshot, caches and hooks per-worker and the world surface refused on workers; a pooled drive's children read foreign immutable `Ref` entry words without touching counts, the join adopting on the host with one inc per surviving reference (all 2026-11-15). Pooled legality (2026-11-17): the refusal is language legality, permanent and independent of every runtime flag (`Vm::enable_fork_pool` included) — the child contract is deterministic data parallelism, no sibling-fiber control, no world touch, the sequential drive never a second semantics. The pooled gate is dynamic: only `par_map` frames whose children hold exclusively scalar entry words go to workers, the pool a host opt-in. Queued M-tier behind 0.1.

### E4 — union errors with `?`, `.err`/`.ok`, extraction family; `or` retired 2026-11-10

Expected failures are union members handled by tokens and methods stated once over unions: `?` (propagation with checked containment), `.err(h)`/`.ok(h)` (difference-typed handlers whose remainder type shows what is left), and the @std extraction family `unwrap_or(y)`, `unwrap_or_else(fn)`, `ok_or(e)`, `map_err(fn)`. Bugs `panic` unhandleably: the fiber aborts, linear deinits run, let-it-crash supervision a library pattern.

### E5 — nursery-scoped cooperative cancellation; consume-to-close
Cancellation is nursery-scoped and cooperative, observed only at await/preemption points (taken backward branches plus the parked-child re-entry), so a linear consume is never interrupted mid-flight; teardown walks frames running deinits, and resources close by consumption. `defer` is non-normative scope-exit sugar under cancellation; timeouts, `firstof` and `interleave` are library combinators over the timer capability. Release door (2026-11-18): an open nursery dies with its dead owner — the trapped-drive release tears down every open nursery whose owner fiber is dead, releasing live children cold (deinits through the owning frames' version rows, claims drained, staged words dying with the segment), so a never-entered child's staged callable words stop gating swaps; the first-entry-lands rule governs live teardown drives alone. Rollback door (2026-11-18): a trapping REPL line's root survives its own rollback, so the rollback reads the pre-step nursery pool itself and tears down every root-owned open nursery the line opened, the slot recycling.

## 6. Syntax surface and sugar budget

### F1 — TS-family braces

Every block opens with a visible token; line-oriented grammar with no layout state; newline-terminated statements without semicolons; trailing commas; `if` and `match` are expressions. Fallback F1b: Gleam/Roc-style brace headlines if the arrow-assignment grammar fights the parser.

### F2 — bindings, functions, annotations, returns
`const`/`var` bindings, `var` the rebindable local. `fn name(p) -> t { }` is the only named-function declaration — top level, trait/impl member, bodiless in traits — its name in scope in its own body; const-arrow the only anonymous lambda form. Parameters `name: t`, returns `-> t`, union returns, `T?` anywhere; final-expression returns, bare `return` early exit (2026-11-10), `break`/`continue`, `?` postfix only. Callable word (2026-11-10): a fn-typed value is a one-word slot classification, never boxed — a non-capturing function a copyable scalar `FuncRef`, a capturing lambda a heap closure holding its capture block — captures under the ordinary binding rules, no mutable upvalue channel in v1. Callable defaulting (2026-11-11): a fn-typed parameter keeps the classification while the arriving word may be a `FuncRef` or a closure Ref, resolved by runtime kind, staging a borrow window (C2) so the callee never owns the count. Boundary: a closure word never widens into a var span, never gets lambda-captured, never enters a collection element or field without proven scalar provenance.

### F3 — full UFCS

`x.f(a)` desugars to `f(x, a)` for any first-argument function, at zero runtime cost and with no candidate search. Resolution is free functions first, then trait methods, `Trait::f(x)` the escape hatch, true ambiguity a compile error. Capability parameters excluded from the dot position. `|>` deferred additive.

### F4 — Rust-shaped pattern matching

Arms `pat => expr`, guards `pat if cond`, or-patterns `a | b`, destructuring, `_`, exhaustive matching with set-difference diagnostics naming the missing members; `@` binders excluded from v1. Struct patterns (2026-11-12) test each literal field in place over a non-union scrutinee and compose into or-patterns and under guards. Tag-dispatch tests (2026-11-12): a member arm may carry value-level tests over its own payload, a failed test sliding to the next arm's dispatch entry, so a tested arm needs a later no-test arm or catchall. Nested struct sub-patterns (2026-11-12) compose at any depth, nested tuple and member sub-patterns refused. Guards (2026-11-12) run after the member tests; a guard must not take a count the base state holds, a can-fail arm needs a later settling arm or catchall, a guarded catchall refuses, the first catchall wins.

### F5 — fixed trait-driven operator set, frozen precedence

A fixed token set maps to fixed traits (Add, Sub, Mul, ..., Index), so an operator call is an inline-cacheable trait-method call. One frozen C-family precedence table published once in the tutorial and never extended; no user-defined operators; the word forms exactly `in` and `is`; boolean contexts bool-typed; `Deref` coercions excluded. `==` is B9's, ordering from the `Ord` derive, trap/wrap behavior B7's.

### F6 — strings and literals
Interpolation `$x` (bare identifier) and `${expr}` in a string-only context: the operand a printable primitive or a `show` carrier, a union or optional operand without a covering `show` an error; every other position strictly explicit. Escapes `\n \t \r \\ \" \' \0 \u{...}`; raw and hash-counted raw strings nest at any depth with no escapes. `'x'` a Unicode scalar char, `b'x'` a byte; `byte` its own atom outside the int conversion family, declared by the ambient (2026-11-10). `bytes` (2026-11-17): one immutable byte string per value behind one Ref word, built through `bytes_of(list[byte])`, bridged back by `byte_list`, measured by `len` through the `Bytes` instance, read through the checked `byte_at` answering `byte | none`; `read_raw`/`read_exact` answer `bytes | io_err`, the file face keeping no `list[byte]` member. Representation (2026-11-18): `show` renders the payload as a byte list, `ord`/`hash` walk it lexicographically with a shorter prefix first, and the string<->bytes bridge decodes through a dedicated `utf8_err` tag, `parse_err` naming document shape failures alone.

### F7 — sugar budget: Balanced tier (22 features / 34 units)

Against Kotlin's 38 features / 74-76 cost-unit ceiling, with 16 features / 40-42 units reserved as headroom. Included rows at S1: newline-terminated statements, interpolation, raw strings, list literals (adopted 2026-11-10), match guards, or-patterns, type aliases, ranges plus `in`, numeric suffixes plus separators, the `const`/`var` split; at S2: postfix `?`, `is` narrowing, `.err(h)`/`.ok(h)`, match/if expressions with exhaustive-union diagnostics, destructuring, named plus literal-only defaults, trailing lambda, UFCS, sum-type declaration, derive attribute, operator-overload traits, `T?` alias. Kotlin features deleted wholesale by set typing and UFCS; excluded permanently: custom operators, Deref coercions, convention-based privacy, truthiness coercion, chained comparisons, comprehension syntax, subscript slicing, vararg/spread packing.

### F8 — extraction methods, no prose operators or implicit coercions (2026-11-10)
`or` is retired (2026-11-10): six behaviors — option and error-union fallback, early exit, fallback onto a diverging callee, union coalescing, loop control — under contextual wanted-member inference, colliding with boolean `||`. Replacements use existing tokens plus one @std family (`unwrap_or`, `unwrap_or_else`, `ok_or(e)?`, `map_err(fn)?`, `err(h)`, a catchall `match`, `if x is none { break }`); bare `return` enters with the retirement. Audit, binding at review: C-family symbols with `in` and `is` the only word forms; boolean contexts bool-typed; chained comparisons, comprehension syntax, subscript slicing and vararg/spread packing permanently out; mutable defaults impossible because defaults are literal-only, mixed numeric arithmetic an error naming the conversion (B7). Ruling (2026-11-10): `in` is range-wired in v1 with a non-range right side refused, collection membership permanently a @std method (`xs.contains(x)`); one word one meaning (2026-11-17). A guard ending in a bare identifier before `=>` needs parentheses (2026-11-12).

### F9 — two namespaces: types and values (2026-11-10)

Constructors (brace construction, bare tags, generic application) resolve in the type namespace and functions in the value namespace, a call-form name present in both an ambiguity error. The `map` struct and `map` function coexist; an enum case colliding with a function is the residual error class.

### 6.1 Implementation-normalized surface spellings (corpus-adopted; to be locked at M2)
Corpus-adopted spellings: trait receivers `self` (borrow) or `mut self` (window, C2), `#[derive(...)]`, `struct stack[t]`, `list[page]()`. Receiverless member (2026-11-15): an impl member may omit the receiver, callable as `mytype.make()` — a type-keyed static entry with no instance or evidence words — while trait members keep it; a dot-receiver bare name names that type's own impl members, a tag or case name naming no member keeping its constructor reading. Instantiated receiver (2026-11-17): the type-keyed entry accepts an applied receiver (`list[int].make()`), the applied type solving as the call's `Self` and binding a generic impl's head variables, a bounded body's evidence staging at the call; the bare name refuses a generic impl's members. `#[default]` (2026-11-15): one enum case names the union's default the family synthesizes — the annotated enum's own `default`, a defaulting union-typed field answering the same construction, a foreign union through its own member; no annotation keeps the ambiguity refusal, a second refuses as a duplicate.

Parser rulings: block bodies, match arms and fields newline-separated with trailing commas; `mut self` shorthand with `move` and `comptime` contextual keywords; top-level `fn` the only named declaration; `none`, `self` and `Self` not reserved; named arguments after positional ones with literal-only defaults.

Comptime fence (2026-11-15..17): the world guarantee is structural — a world call the rows cannot attribute refuses at the fenced registry install. Admission is by the callee's own solved effect row: pure admits, impure refuses naming the comptime fn, the callee and the call, and a `trap`-only row admits, so the unsolved-type over-approximation fences nothing. The admitted subgraph lowers into the slice beside the comptime fns, a deinit-carrying construction refusing. Residuals: a generic callee refuses; a member call inside an admitted body refuses.

## 7. Polymorphism and metaprogramming

Trait-member default authority (2026-11-12): the trait declaration is the single authority for a defaulted member parameter, so an impl re-declaring one is a checker error and a dispatch call stages the trait's default; an impl-only default refuses, the registered signature carrying none.

### B6 — coherent set-theoretic traits with pairwise-disjoint instance heads
Traits may carry associated types; instances are declared for type heads; the only coherence rule is pairwise disjointness of one trait's heads — for every pair prove `H1 & H2 = Empty` over the closed instance set, at declaration and again over the final environment at link time. Any module may instance any type, foreign included: no orphan rule, no newtype wrappers. Resolution is intersection subtyping by modus ponens with no priority machinery; an overlap error names both heads and a witness type. Instantiation (2026-11-12): a declaration probes whether some grounding of either head is served by both, an unprovable pair falling to the closure pass, a self-requiring conditional instance rejected as a structural cycle. Union heads decompose per branch; scoped named instances stay deferred. Call-site resolution (2026-11-11): a query unifies a generic head with each receiver member under its own substitution, every bound proven recursively; several serving impls are true ambiguity, a failed bound diagnosed as the unsatisfied bound. An impl member must agree with its trait member's registered signature. Union receiver (2026-11-14): `Self` binds the solved instance's written head when closed and containing the refined receiver, a generic head keeping the call's substitution; a union receiver no single instance serves is covered branch-wise by sibling instances of one written head, exactly one covering group serving it, several true ambiguity, uncovered members refusing. The inline cache keys the written head, never the branch id, and the synthesized compare evaluates every part eagerly, so member compare bodies are required pure. Per-branch conditions (2026-11-14) name only their own branch's variables.

### G1 — comptime evaluation + static reflection, no macros
The meta-language is the language: comptime evaluation of ordinary code plus static type reflection; macros of every level are rejected, so hygiene is deleted structurally. The derive vocabulary stays closed to third parties (2026-11-14), the builtin families checker-synthesized and unknown names refusing; the one sanctioned comptime seam is field traversal (`fields(t)`), so libraries write ordinary comptime functions without code synthesis or identifier minting.

Rulings (2026-11-15..16): `type` solves to a builtin atom every universe declares beside `none`, a type name a value of it exactly inside a comptime context. The world fence sits at the evaluator's install — the slice's registry serves the package's platform name with an empty provides list — so a world call refuses at install as a compile error naming the capability. An answer embeds at the call site as the literal a user would have written, gated against the call's checked type; comptime functions never reach a runtime module. Field record: `name`, `type` (reflection index), `tag` (the nominal name of a bare atom). Enum case traversal answers through `fields`, one record per declared case in declaration order, `type` the payload atom (traversable like a struct) or `none` when the case constructs bare, `tag` the constructor tag. A comptime parameter's annotation admits `type` and the four scalar atoms as one staged literal each; holed or suffixed literals, omitted defaults, other annotations, mixed modes, generics and non-free-fn positions refuse.

### G2 — comptime-only reflection over tags-as-data

Reflection exists only at comptime; runtime type questions are answered by the tags data already carries (`is`, match), no type objects or metadata tables existing. Existentials remain the one justified boxing site (`box Json`) for heterogeneous collections and plugin boundaries, signatures defaulting to universals since existentials break affine reasoning.

### H3 — one generic body, evidence words, inline caches, lazy JIT specialization
Every generic function exists once in bytecode with its constraint list. A generic call site emits its callee plus evidence words, each a statically-resolved instance constant or a runtime-resolved tag-keyed slot patched as a small inline cache whose miss falls back to full lookup, so correctness never depends on cache state; evidence words are ordinary register arguments staged after the true arguments, so tail-position generic calls tail-call normally, the tag-dispatched form included (H2's 2026-11-12 ruling). The JIT tier lazily materializes per-type specializations keyed by the type vector, with no correctness role. Anti-specialization invariant (verbatim): "dispatch is determined by types and the set algebra, never by declaration order or priority; the runtime may cache and specialize any dispatch that is already unique." Variance follows set inclusion; no HKT and no GATs in 1.0. Free generic fns (2026-11-13) bind one Raw word per own type variable, each call site staging demands for its declared bounds.

## 8. Modules, visibility, FFI, ecosystem

### G3 — definition < file < package < module; no import-time execution
Four layers: definitions (visibility and identity), files (namespace spelling), packages/directories (the compilation unit, one namespace per package with zero intra-package imports), and the module (`ds.pkg`: dependency and distribution unit). Intra-package cycles are legal (whole-package fixpoint resolution), cross-package cycles a link error suggesting a base package; generated `.dsi` files are the reviewable API surface. Module bodies are pure compile-time declarations with no import-time execution, script mode alone admitting top-level statements in the entry file (2026-11-10). A line-initial `##` separates blocks; `///` doc comments attach as parse-layer data to the next definition or member `fn`, struct field or enum-case field (2026-11-12).

**G3.5 — the dsmod bytecode contract (binding).** Fixed-width, position-independent, mmap-able format whose header carries the dep list, source hash (checked-hash validation, never mtime), the artifact checksum and strippable sections. Loading rulings (2026-11-15): the dep list is the link table (deps-first install walk with install-time content-hash validation), the constants-pool `FuncRef` dep-position-keyed, the module-id space holding installed names only, deterministic in dep-walk order, never serialized. Dep hashes validate at install while resolution reads the name's current slot, so a hot swap composes with staged words; same-name multi-hash coexistence stays refused. Multi-package watch (2026-11-20, item 67): a manifest edit pinning new dep content moves the dep's identity through the per-name two-slot swap, which re-checks the staged words the VM owns, and the pin is the only lever over dep content. @std borderline: `bytes` joins the collections round, `max`/`min` migrate with the library-boundary round. `die` splits (ruled 2026-11-17, landed 2026-11-18): the builtin `trap` native carries the message out as a `Die` trap with the engine printing nothing and deciding no exit code, and the spelling is a CLI host member registered beside the ambient for its entry sessions, so an un-provisioned compile keeps `die` unknown; a block-tail `die` against an annotated return keeps the containment refusal.

### G4 — private / `pub(pkg)` / `pub` plus selective imports

Definitions are private by default; `pub(pkg)` opens them package-wide and stays out of the `.dsi`, `pub` opens them to dependents and enters it. Imports are symbol-selective with aliasing, conditional per target and re-exportable via `pub import`, in sigil form, the quoted form retired (2026-11-10). The `.dsi` is the visibility gate, the API-review artifact, the LSP hover source and the incremental-compilation boundary; the `pub(in ...)` ladder is left out to stay under the sugar ceiling.

### H5 — linear C-ABI handles as substrate, platform packages layered on
Foreign resources are opaque linear handles (`~Copyable`): double-free a compile error, extern borrows call-scoped raw pointers the type layer forbids from escaping, callbacks on VM-managed stacks. The embedding API is a Lua-shaped handle table where a `Value` indexes the VM's object table, two embedders being two independent VMs. Objects never move, so a blocking foreign call pins its fiber and cancellation is observed only at VM re-entry. Platforms are ordinary packages exporting effect handlers, selected per build target (`platform "std" provides <console, fsys, net, time>` default, `"web"`, `"embed"`), the checker verifying effect usage against the declared row. Extern consumption (landed 2026-11-20): `extern "<artifact>" fn <name>(scalar...) -> scalar`, the string naming the `[artifacts]` entry whose installed library owns the C symbol, the name resolving through the package graph's exported-symbol fold ahead of the wired native lookup; the slice serves `int`/`bool`/`float` signatures of at most four parameters (an enumerated per-register-class transmute, no libffi) with the return word in the pool constant. String and bytes marshaling, deinit-carrying handles, unit returns, generics and a bound extern as a callable value keep honest lowering refusals naming the seam; a foreign call inside a fork-join pooled child refuses like a world op.

**H5.5 — the std/builtin/platform layering (binding).** `builtin` holds the irreducible VM natives ds source cannot express, pure or error-union-total and never world-touching; `@std` is an ordinary ds-source package holding everything expressible in DreamScript and resolving script mode's bare specifiers; platforms are the world surface as effect handlers (H5/E1). Platform seam (2026-11-13): `builtin` is the only native module of record, world ops in the platform ABI's dense table, install-time validation refusing an unprovided capability. TLS is a platform capability (ruled 2026-11-17): https over rustls in `platform "std"`'s net face, per host, never builtin or @std, over webpki-roots' static set, no system store and no client certs. Library boundary (2026-11-14): the ambient shrinks to the compiler-required face (atom tags, derive traits and `Copyable`, the failure taxonomy, `str`/extraction/`cancel` spellings, one platform declaration), everything else in a default-installed `@std`; the combinator surface stays compiler-recognized; user and library errors are ordinary declared types. Collection face (2026-11-17): inherent members plus UFCS spellings (`xs.len()`, `m.get(k)`, `list[t]()`), never `list_*`/`map_*` prefixes. Json (2026-11-17..18): @std-owned source, a seven-case enum, int-faithful numbers, `to_json` a plain function, a depth-budgeted reader, and `from_json` a derive family over the typed readers (a wrong shape answering `parse_err`), never a comptime extension, the embed vocabulary fenced small (DS2034). Pure builtins: `parse_float`, `char_of`, `code_of`.

### H6 — hash-store core plus zero-config script mode; five mandatory tooling commitments
**Package stack.** A manifest declares deps as `name = { ver, hash }`, the hash blake3 over content including dependency hashes, so identity flows transitively and no lockfile exists; MVS picks the max declared minimum; a global content-addressed store keyed by output hash is also the bytecode cache. Native members (ruled 2026-11-17): native code ships as per-target content-hashed prebuilt libraries under `[artifacts]`, consumed through H5's C-ABI seam, externs binding panic=abort and a blocking foreign call pinning its fiber; a `[build]` source-build round stays deferred on a pinned-toolchain reproducibility story, never permanently rejected. Artifacts grammar (landed 2026-11-18): `name = { ver, target, hash }` in canonical field order after `[deps]`, `target` a two-to-four-segment lowercase triple, `hash` under the artifact's own digest domain; one identity per artifact name across the graph, and a missing artifact refuses the run before any compile. The ds-ffi loading layer (2026-11-20) binds an installed library over the raw OS loader primitives, dependency-free, reading exported C symbols off the PE export directory and the ELF64 dynamic table, `ds bind` reporting each binding. Marshaling queues in the packages remainder (item 63).

Package rulings (2026-11-15): `name`, `version`, and one `[deps]` section of `dep = { ver, hash }`, `ver` one to three numeric components without leading zeros; non-canonical spelling is reported drift, and a canonical serializer makes the manifest's bytes a pure function of its model, so the manifest is the lock. Resolution is MVS over a caller-supplied provider answering name@version exactly: the max declared minimum wins, the winning declarer's pin validates the fetched hash, and an unknown name, an unoffered version, a pin or content disagreement or a cross-package cycle refuses; one version per name this round. Package content is single-member, installed as one hash-validated bundle.

Registry rulings (2026-11-17): semver pre-release and build metadata (`1.2.3-rc.1+build.42`, a numeric pre-release identifier without leading zeros), precedence numeric-then-pre-release, build metadata in identity and the lock bytes but never precedence, canonical three components, both segments bounded at 64 bytes. The remote client is git-over-https: `$DS_REGISTRY` names a local index root or a git remote URL, the client shelling out to the system `git` with no vendored dependency, one shallow snapshot clone per resolution; an uncloneable remote refuses with git's own words.

**Script mode.** `ds run script.ds` with zero config, bare-specifier imports in sigil form (G4), `ds save` freezing what the script used into a generated `ds.pkg`.

**Five tooling commitments, binding from day one.** Compiler-as-query-library (memoized parse, block_index, resolve, type_of, effects_of, linearity_check, lower shared by CLI, LSP, REPL and hot reload, invalidated at `##` and package grain); a zero-option `ds fmt` serving the LSP's on-type formatting; two-span linear diagnostics with machine-readable suggestions and `--error-format=json`; a linear-aware REPL reporting type and effect row with `:drop`/`:reset`; Erlang two-version code slots, old frames retiring at their next fully qualified cross-module call.

Module-version handle (2026-11-14): a per-VM append-only version table owns every module version's survival, frames carrying an index into it (no per-call refcount), the table's `Arc<ModuleVersion>` keeping immutable code `Send + Sync` for workers; retirement rides an install-time sweep over frames, parked fibers and caches. `Vm` stays `!Send`.

Hot-dev rulings (2026-11-16..18): `ds dev --hot` suspension is host-injected through a capability face, never a user-spellable debug seam; the poll read is the parked entry's tick rate, a changed round swaps the module first so the frame's next fully-qualified call lands in the new version, the differential contract pinning byte-identical output against a fresh run. Every slot swap validates the staged callable words the VM owns by recorded identity (name plus frame layout) — a surviving identity re-keys, a dropped, retyped or unknowable one refuses the swap while the installed version keeps running — and the CALL/CALLT/CALLG/CALLTG sites of every row a runnable frame or parked fiber pins, refusing when a site's function index re-keys onto a different name; an unnamed row takes the ordinary redirect. Position-stable lambda naming (2026-11-18): a reordering reload re-keys a moved staged lambda or moved index-embedded call site exactly when its content identity survives the move (body plus capture set hashing equal against exactly one incoming candidate), an edit, capture-set difference or ambiguous match keeping the refusal.

## 9. Runtime

### Fixed baseline (BL-1..BL-7, binding on every tier)

- **BL-1 Typed register bytecode.** 32-bit fixed-format instructions with register-index operands and typed opcodes, no dynamic variants since every operand's type is a compile-time fact; biased jump offsets and XOR-negated comparison pairs.
- **BL-2 Unboxed slots.** No NaN-boxing, no per-slot tags, no boxed scalars: scalars raw in frame slots, references raw pointers, ADTs/unions unboxed tag+payload pairs.
- **BL-3 Dispatch.** Computed goto or C tail-call threading, a build-portability choice.
- **BL-4 Superinstructions on profile data.** The assembler composes fusions from measured opcode frequencies.
- **BL-5 Uop decomposition from day one.** The instruction set is micro-ops with explicit slot effects and `pure` annotations, so the copy-and-patch stencils are these uops one-to-one.
- **BL-6 Frames inline in a typed value stack.** A contiguous span of statically-typed slots plus a metadata word per frame, so TCO is a pointer shuffle.
- **BL-7 Position-independent, mmap-able dsmod.** Fixed-width, index-based, relocation-free bytecode with a module header and an IR-version checksum, contracted in G3.5.

### H1 — three tiers over one shared lowering

Tier 1: typed register VM, computed-goto uop loop, unboxed frames, target 3-10x of C. Tier 2: copy-and-patch stencils, one fragment per uop emitted at build time, ~14% faster than LLVM -O0 at ~100x its compile speed, target 1.5-3x. Tier 3: bytecode -> typed SSA -> Cranelift CLIF, or wasm 3.0 (`return_call` carries TCO), target 1-2x native. One uop set and one bytecode-to-SSA lowering shared by all tiers and built early. No guards exist on any data path — only debugger breakpoints, the per-entry stack-limit compare, effect-handler entry and B7's cold overflow edge — so no deopt cliffs nor hidden classes, type guards, deopt frames or side traces. Short scripts (<0.2s) never pay tiering, platforms without verified stencil builds shipping tier-1 only. Fallback H1b: a single method-JIT tier if the per-arch stencil toolchain proves unacceptable. The span-answer seam's whole-interpreter cost stays accepted and baked in `PINS` (the measured narrowing, a one-word `Died` payload plus a `DiedSpan` side slot, was refused); the seam reopens only behind the JIT specialization tier's own dispatch.

### H2 — guaranteed TCO from day one, self and mutual
Frame reuse plus argument slide in tier 1, label loops and slot-overlay `jmp` in JIT tiers, `return_call` on wasm. This ABI and BL-7 lock before anything else. Guarantee closure (2026-11-12): no callee-class exception — the tag-dispatched tail call transfers through the same frame-reuse slide with instance/evidence words staged after the exit claims, the tag-keyed inline cache keying it identically; a count-free closure callee word transfers through the plain tail call by maybe-Ref tracking. Cross-module (2026-11-20): a tail call into a linked package's function transfers through the same CALLT slide — the module boundary riding the pool FuncRef alone, the slide a pure fact of the two layouts, the linked callee owning its handed words like a local's — and the local shapes keep their rules across it (a closure word staged into a callable-defaulted parameter keeps the in-place tail, a deinit-carrying argument into a non-consuming parameter keeping the refusal). Queue-closure rulings (2026-11-12): destructured `const` binders alias the source `var`'s words, so reassignment through the var is observable through them; native fn values never mint a callable word in 1.0; the future fetch native answers both ambient `net_err` cases by error class.

### H4 — Perceus RC + reuse + per-frame arenas, collector interface as growth path

Precise non-atomic RC with reuse threading for boxed/shared values, over per-frame bump arenas for non-escaping temporaries: zero pauses, no barriers, no safepoints or GC maps in any tier, deterministic frees feeding deterministic deinits, per-fiber arena marks making fiber switching free. All non-moved temporaries are register/arena-resident, the boxed/shared residue keeps paying RC, and the reuse pass is budgeted as optimizer work. The two-hook collector interface (cycle candidates, weak refs) is committed as the growth path.

## 10. Staging (v0 -> 1.0)

Ordered; each step lands on the previous one with no language-change rewrites.

1. Lock the two load-bearing contracts: the slot-overlay call ABI (H2) and the fixed-width mmap-able dsmod format (BL-7, G3.5).
2. Checker core: the B1 set algebra, B2 bidirectional checking plus refinement, B3 products/sums, B4, B7, B8, B9, and the affine checker plus DI (C1, C5).
3. Tier-1 interpreter: uop loop, unboxed slots, tag+payload ADTs, frame-reuse TCO.
4. Memory: Perceus dup/drop placement, reuse threading, per-frame arenas; `rc==1` checks and the borrow counter as the diagnostic layer.
5. Concurrency: fibers, nursery, `cancel`/`discontinue` evidence dispatch, fork-join combinators.
6. Derives; then the comptime evaluator and reflection API (before 1.0).
7. Tooling commitments from day one, sequenced with the query compiler they consume.
8. Tier 2 copy-and-patch (x86-64, then aarch64; tier-1-only elsewhere), bytecode-to-SSA lowering early, tier 3 on demand.
9. Deferred-additive backlog after the core is stable (§11); borrow inference, TRMC and fusion upgrades land as pure optimization.

## 11. Deferred-additive backlog and recorded fallbacks

Deferred additive (in headroom order of likelihood): `|>`, anonymous record literals (the map literal rides this reservation per B3), `@` binders, `??` on `t | none` (B4, F8); then `?.` desugar, `throws(...)` sugar, user-defined handlers, scoped named instances, labeled break/continue, `it` shorthand. Retired: vararg/spread (F8); `or` (E4, F8); bare `return` adopted 2026-11-10 (F2). Pure-optimization items: borrow inference, full reuse pairing, TRMC, fusion upgrades, `T | none` niche packing, rope. Recorded fallbacks (triaged 2026-11-17): B1b nominal-core and F1b brace headlines retired, both triggers unreachable (B6, the M1 parser); H1b single method-JIT tier stays until the x86-64 stencil build lands (M8); C4b generational references stays with its quantified trigger (C4).
