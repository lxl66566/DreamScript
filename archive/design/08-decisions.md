# 08 — Phase 3 decision record

Status: author decisions, recorded 2026-11-08. This file resolves every decision point of `docs/en/design/00-decision-map.md` — 42 points: A1-A3, B1-B9, C1-C5, D1-D3, E1-E5, F1-F7, G1-G4, H1-H6 — plus the open questions the catalogs left to the author. Option analysis lives in `docs/en/design/01-07`; evidence in `docs/en/research/01-06`; per-row marks are mirrored in the decision map; the detailed per-point rationale is `docs/en/design/09-decision-rationale.md`. Chinese translation: `docs/zh/design/08-decisions.md`. Nothing here is implementation; per AGENTS.md, phase 4 works from this record.

Method: each point was re-weighed against the charter's hard requirements and the author-taste calibration, treating each catalog recommendation as a strong default. This pass adopts every recommendation and overrides none; its added value is the resolution of every deliberately-open sub-question (staging, excluded spellings, token finalization, open-question list below). Rows marked "deferred additive" decide scope for 1.0 while keeping a migration path that requires no breaking change.

## Decision summary

| ID | Decision |
|---|---|
| A1 | (c) standalone core; embedding arrives as platform packages over the same runtime |
| A2 | (a) CLI/tools/scripts first, (c) games/simulation second, (d) numeric reachable via tier 3 |
| A3 | (a) rapid-break until 1.0, then (b) semver; hash addressing adopted at the package level (H6b) |
| B1 | (c) hybrid: nominal tags composing set-theoretically (`\|`, `&`, `-`); the README's `any`-fallback rejected |
| B2 | (c) on (a): bidirectional checking with required boundary signatures, plus flow-sensitive refinement |
| B3 | (d) nominal structs/enums plus anonymous tuples; anonymous record literals deferred additive |
| B4 | (c) `T?` as a zero-semantic alias for `T \| none` |
| B5 | (d) union-carried error members; a `throws(...)` row spelling stays later additive sugar |
| B6 | (a) coherent set-theoretic traits with pairwise-disjoint instance heads; scoped named instances later, additive |
| B7 | (a) Int=i64 / Float=f64 defaults, trapping overflow, `+%` wrapping family, width suffixes; decimal as a library type |
| B8 | (a) immutable UTF-8, checked slices returning `String \| none`, graphemes in stdlib |
| B9 | (a) universal structural `==` with the five carve-outs; Ord/Hash/Show via derives |
| C1 | (b) affine core plus opt-in `~Copyable` linear layer; inference staged as later pure optimization; runtime borrow counter as v0 diagnostics |
| C2 | (d) second-class parameter-passing modes: default borrow, `mut`, consume |
| C3 | (b) `const`/`var` bindings, mutation through `mut` exclusion windows; FBIP reuse as the runtime guarantee |
| C4 | (a)+(d) Perceus precise RC with block reuse, plus per-frame bump arenas |
| C5 | (c) `deinit` restricted to `~Copyable` types; DI checking as infrastructure; explicit `drop` as a debug/FFI tool |
| D1 | (b) exclusion-window mutation as the only channel |
| D2 | (b) linear-mutable COW collections: functional API, in place when unique |
| D3 | (d) combinators as the surface, hidden linear cursors as the protocol, compiler fusion; no public generator syntax |
| E1 | (d) no effect syntax in v1 over an evidence-passing handler runtime; (c) capability parameters as the stdlib idiom |
| E2 | (b) VM fibers with a mandatory nursery |
| E3 | (b) fork-join data parallelism on the (a) linear/immutable sendability substrate |
| E4 | (b) union errors handled by `?`/`or`/`.err`, plus (a)'s unhandleable trap for bugs |
| E5 | (a)+(b) nursery-scoped cooperative cancellation plus linear consume-to-close; `defer` as sugar, timeouts as library |
| F1 | (a) TS-family braces |
| F2 | `const`/`var`; `const name = (params) -> T { }` declarations; `name: T` and `-> T`; final-expression returns; `or return` early exits; README prefix-`?` return marker retired |
| F3 | (a) full UFCS; `\|>` deferred additive; capability parameters excluded from the dot position |
| F4 | (a) Rust-shaped arms, guards, or-patterns, set-difference exhaustiveness diagnostics; `@` binders excluded from v1 |
| F5 | (a) fixed trait-driven operator set, frozen C-family precedence, `+% -% *%` wrapping family |
| F6 | (a) `$x` / `${expr}` interpolation with an explicit `str()` boundary; `r#"..."#` raw strings; `'x'` / `b'x'` literals |
| F7 | Balanced tier: 22 features / 35 units against Kotlin's 38 / 74-76 ceiling |
| G1 | (b) comptime evaluation plus static reflection, no macros; built-in derives first, comptime before 1.0 |
| G2 | (b)+(d) comptime-only reflection over tags-as-data; no runtime type metadata |
| G3 | (b) definition < file < package < module layering, generated `.dsi`, no import-time execution; G3.5 dsmod contract binding |
| G4 | (b) private / `pub(pkg)` / `pub` plus selective imports; the `.dsi` is the visibility gate |
| H1 | (a) tier 1 register VM + tier 2 copy-and-patch + tier 3 Cranelift/wasm over one shared lowering |
| H2 | (a) guaranteed TCO from day one, self and mutual, frame reuse plus slot-overlay ABI |
| H3 | (a) one generic body plus evidence words, polymorphic inline caches, lazy JIT specialization |
| H4 | (a) Perceus RC + reuse + per-frame arenas, with (d)'s two-hook collector interface as growth path |
| H5 | (a) linear C-ABI handles as substrate, (b) platform-as-packages layered on, (c) `extern` as documented escape hatch |
| H6 | (b) hash-store core plus (c) zero-config script mode; the five mandatory tooling commitments bind |

## A. Identity

**A1 — (c).** The performance charter requires a real standalone runtime with its own tiers; embedding then arrives as platform packages exporting effect handlers (H5b) over the same runtime, which covers the Lua embedding niche without a second product.

**A2 — (a) primary, (c) secondary.** Both workloads are served by the interpreter-first pipeline, mmap startup, and the two-slot hot-reload runtime; numeric/AI work stays reachable through the tier-3 Cranelift path without shaping v1.

**A3 — (a) then (b).** Rapid-break until 1.0 (Roc/MoonBit posture), semver discipline after. Content addressing is rejected as the source-level identity model and adopted as the package-level store (H6b), where its benefits are real and its cultural costs are absent.

## B. Types

**B1 — (c) hybrid.** Nominal tags give identity, encapsulation, and tag-keyed exhaustiveness; set algebra over them delivers the README's set instructions as type expressions, error-set arithmetic, and narrowing, while the checker stays at enum-cover difficulty and the VM stays at unboxed tag+payload cost. The README's `any`-fallback is rejected: it converts inference failures into dynamic typing, violating the compile-time-errors rule. Recorded fallback: if the disjointness machinery slips schedule, B1(b) is the reversible retreat, at the price of the union surface (E4b degrades to match-based handling with it).

**B2 — (c) on (a).** Required signatures at module boundaries anchor the set algebra where it is checked; locals, lambdas, and match arms are inferred bidirectionally; `is` tests and match arms narrow monotonically, which is why refinement composes with intersections and negation without blowup. Full HM over the full algebra remains a research bet this project declines.

**B3 — (d).** Nominal structs and enums for named data; anonymous tuples for ad-hoc aggregates; anonymous record literals as compiler-synthesized sugar over a nominal record — that sugar stays deferred additive until the inference story settles (F7 ledger row 25). Rows are rejected for data: the highest machinery per unit of sugar in the catalog, and their one unavoidable use (effect rows) is deferred anyway (E1d).

**B4 — (c).** `T?` is exactly `T | none`. Optionals, error unions, and enums share one mechanism — `is`, `or`, match — so `none`, error members, and enum cases are the same construct. No `?.` operator in v1; a thin `x?.f` desugar may be added later only if real code demands it.

**B5 — (d).** Errors are union members of return types: storable data for nursery collection and supervision, one predicted branch in the VM, identical information to an effect row. A `throws(...)` spelling remains later sugar desugaring to the same union.

**B6 — (a).** Traits may carry associated types; instances are declared for type heads; the coherence rule is pairwise disjointness of heads of one trait (`H1 & H2 = Empty`, decidable set emptiness), checked at declaration and again over the final instance environment at link; any module may define an instance for any type, foreign included; no priority machinery ever exists — the author's anti-specialization position made structural. Scoped named instances (b) stay a later additive escape hatch with ambiguity always an error.

**B7 — (a) with decimal as a library type.** Int/Float default to i64/f64 raw in unboxed slots; overflow traps deterministically; `+% -% *%` (and `-%a`) wrap explicitly; suffixes select widths (`42u8`, `2.0f32`); `1_000_000` / `0x` / `0b` / `0o` literal forms; no implicit narrowing; division by zero and shift-overflow trap. Decimal (`Dec`) and units-of-measure are nominal library types plus derives.

**B8 — (a).** Immutable UTF-8 behind one raw pointer plus byte length; checked slices return `String | none`; `chars` iteration; graphemes in stdlib; rope as a future library type; encoding conversion happens at FFI boundaries.

**B9 — (a).** One structural `==` over all data, total by construction: function comparison rejected statically; floats compared bit-structurally (reflexive, safe as dict keys) with `is_float_eq` / `is_approx_eq` named alongside; mutable cells and `~Copyable` resources compared by identity; operands borrowed, never consumed; Hash co-required. `Ord` / `Hash` / `Show` arrive as derives (G1); no `===` exists.

## C/D. Linearity, ownership, memory, data

**C1 — (b).** The everyday rule is one sentence: assigning an affine value moves it, and a moved binding is dead. Ordinary copyable values never observably hit the rule. `~Copyable` subtracts the default copyability constraint, composes with the B1(c) algebra (`File[Open] | File[Closed]` phantom states), and is where `deinit` lives. This is the honest reading of charter req 2 ("static linear or affine typing") at the lowest learning cost, and it answers the open ownership question directly: ownership machinery exists as opt-in resource types plus parameter modes (C2d) and is invisible everywhere else. Staging per the research's combo 6: runtime `rc==1` uniqueness checks and an Inko-style borrow counter ship first as the diagnostic layer; borrow inference, full reuse pairing, and TRMC arrive later as pure optimization with zero language change.

**C2 — (d).** Three second-class parameter modes fixed at the definition site — default borrow, `mut` (exclusive), consume — and call sites carry no mode syntax; UFCS chains stay clean. Borrows never escape a call, so checking is intraprocedural and no lifetime annotations exist. First-class references are permanently out, deliberately: the four coupled features (moves, borrows, lifetimes, drop) collapse to two.

**C3 — (b).** `const`/`var` bindings; `var` is the rebindable local; callee-side mutation goes through `mut` windows — the same mechanism as C2(d), one rule for parameters, windows, and confinement. FBIP reuse underneath makes functional updates run in place when the value is unique; shared mutation stays unrepresentable by design. No user-facing interior-mutability cell types.

**C4 — (a)+(d).** Precise non-atomic reference counting with Perceus block reuse for boxed/shared values; per-frame bump arenas for temporaries, sound because linearity makes escape a compile-time fact. No tracing GC. Cycles are rejected at the type level for v0, with the two-hook collector interface (H4d) as the declared growth path.

**C5 — (c).** `deinit` is expressible only on `~Copyable` types and runs exactly once at the unique instance's death, on every path including panic and cancellation — the RAII layer that also supplies fiber teardown (E5). Definite-initialization checking is infrastructure under every option. Explicit `drop` exists as a debug/FFI tool; users never write routine drops.

**D1 — (b).** The `mut` window is the only mutation channel: exclusive, lexically visible at the call site, statically proven, zero runtime checks inside the window.

**D2 — (b).** Collections expose a functional API and mutate in place iff uniquely owned (COW with guaranteed, observable reuse); persistent HAMT-style structures are ordinary library types on the same runtime; `Bytes`/`Vec` are the unboxed unique members of the same model.

**D3 — (d).** Combinators plus `for` are the iteration surface; the hidden protocol is a linear cursor; the compiler fuses chains; subscripts extend in-place element access. No public generator syntax in 1.0; the hidden lowering may adopt the fiber facility later with no surface change.

## E. Errors, effects, async, parallelism

**E1 — (d) with (c) as the stdlib idiom.** No effect syntax in v1. The runtime is handler-shaped from day one: evidence passing serves the two internal effects (`cancel`, `discontinue`) at nursery boundaries, so user-defined handlers remain an additive escape hatch (v2, `with handler`), never a migration. World-touching stdlib APIs follow the capability idiom: an ambient default handler installed by the `ds` CLI for scripts, and an explicit capability parameter for tests, embedding, and scheduler selection — the same code runs over blocking, event-loop, or custom schedulers. The world-access audit is convention-plus-capability, implicit by default; that is the one axis where an explicit-parameter posture would score higher, and it is available by taking the capability parameter deliberately.

**E2 — (b).** Stackful heap fibers on the VM pool; `spawn` exists only inside `nursery` blocks; scope exit joins children, first failure cancels siblings, child errors union out at the boundary. Blocking-style code, no colors, no `await`, O(1) switches, per-fiber TCO intact. Bare detach requires an explicit root nursery or supervisor.

**E3 — (b) on (a).** Race-freedom and sendability are already static consequences of linear/immutable data (a linear value is `sending` by construction; an immutable value is shareable by construction); the missing piece is only a scheduler. `par_map` / `par_reduce` fork-join combinators over immutable or linear-partitioned inputs, deterministic and replayable; actor and supervision layers can be added later as libraries (a supervisor is a nursery with a restart policy).

**E4 — (b) with (a)'s trap.** Expected failures are union members handled by `?` / `or` / `.err` with exhaustive difference-typed coverage; bugs `panic` unhandleably — the fiber aborts, linear deinits run during the frame walk, and the trap surfaces at the nursery or supervisor boundary. Let-it-crash supervision (E4d) is a library pattern on any of this.

**E5 — (a)+(b).** Cancellation is nursery-scoped and cooperative, observed only at await/preemption points, so a linear consume is never interrupted mid-flight; teardown walks frames running deinits. Resources close by consumption; `defer` exists as scope-exit sugar (non-normative); timeouts, `firstof`, and `interleave` are library combinators over the timer capability.

## F. Syntax

**F1 — (a).** TS-family braces: the historical README's own form, the author's daily surface, and the lowest grammar delta of the four families. F1(b) is the recorded fallback if parser prototyping shows the arrow-assignment grammar fighting the interpreter's parser; F1(c) and F1(d) are dropped.

**F2 — as stated.** `const`/`var` bindings; named functions as `const name = (params) -> T { body }`, with the defined name in scope inside its own body; `name: T` parameters and `-> T` returns; anonymous lambdas `(x) => e`; final-expression returns with `or return` / `or break` / `or continue` early exits; newline-terminated statements, no semicolons; trailing commas allowed. `?` is reserved for propagation; the README's prefix-`?` return marker is retired.

**F3 — (a).** Full UFCS: `x.f(a)` desugars to `f(x, a)` for any first-argument function. Resolution is free functions first, then trait methods, with `Trait::f(x)` qualification as the escape hatch; true ambiguity is a compile error, never a priority search. Capability parameters are named, excluded from the dot position, and passed explicitly or via a scope form. `|>` is a deferred additive (Roc deleted its pipe as redundant with chaining plus `?`).

**F4 — (a).** Rust-shaped arms: `pat if guard`, or-patterns `a | b`, destructuring, exhaustive matching with set-difference diagnostics ("missing: NotFound | Locked"). `@` binders are excluded from v1: `is` narrowing keeps the scrutinee variable usable and destructuring covers field extraction, so the binder's remaining use is affine re-binding of a moved-out whole, a question that deserves its own design if real code demands it. Deferred additive.

**F5 — (a).** A fixed operator token set mapped to fixed traits (Add, Sub, Mul, ..., Index), one frozen C-family precedence table published once in the tutorial, and no user-defined operators ever. `==` is B9's structural equality and is never user-overloadable; ordering goes through the `Ord` derive. Overflow spelling per B7: trapping defaults, `+% -% *%` / `-%a` wrapping, trapping division and shifts.

**F6 — (a).** `$x` bare-identifier and `${expr}` interpolation, with an explicit `str()` call at the non-string boundary (no hidden stringification). `r"..."` and hash-counted `r#"..."#` raw strings serve raw, multiline, and heredoc needs in one construct; `'x'` is a Unicode scalar Char, `b'x'` a byte.

**F7 — Balanced.** 22 ledger rows / 35 cost units: roughly half of Kotlin's feature count and under half its weighted surface, with 16 features / 40-42 units of headroom. First candidates for that headroom, in order of likelihood: `|>`, anonymous record sugar, `@` binders, vararg/spread.

## G. Polymorphism, metaprogramming, modules, ecosystem

**G1 — (b).** Comptime evaluation of ordinary code plus static type reflection; macros of every level are rejected — hygiene is deleted by construction and the meta-language is the language. Staged delivery: the built-in derives (`Hash`, `Show`, `Ord`, `Clone`, `ToJson`/`FromJson`, `Default`; `==` is universal and never derived) ship first; the comptime evaluator and reflection API land before 1.0.

**G2 — (b)+(d).** Reflection exists only at comptime; runtime type questions are answered by the tags data already carries (`is`, match); no type objects and no runtime metadata tables exist anywhere.

**H3 — (a).** One generic body per function; evidence words travel as extra register arguments — constant operands when the instance is statically resolved, tag-keyed 1-2 entry inline caches on dynamic paths; the JIT tier lazily materializes per-type specializations in a cache with no correctness role. This satisfies the anti-specialization stance: which code runs is fixed by the set algebra before any caching, and the runtime may cache dispatch that is already unique. Variance falls out of set inclusion; no HKT or GATs in 1.0.

**G3 — (b).** Four layers: definition < file < package < module. Files in a package share one namespace with zero intra-package imports; intra-package cycles are legal (whole-package fixpoint resolution); cross-package cycles are a link error whose diagnostic suggests moving the shared abstraction to a base package. Generated `.dsi` interface files are the reviewable API surface; module bodies are pure compile-time declarations — no import-time execution, period. `///|` is an optional explicit block separator that gives tooling sub-file granularity; top-level definitions are order-independent with or without it, and a file without separators is one block. The G3.5 dsmod contract (mmap-able, dep-list lazy loading, blake3 checked-hash validation, embedded-bytecode single binaries) binds.

**G4 — (b).** Private by default; `pub(pkg)` opens package-wide; `pub` opens to dependents and enters the `.dsi`; imports are symbol-selective with aliasing, conditional per target, and re-export via `pub import`. The interface file is the visibility gate and the API-review artifact.

**H5 — (a)+(b).** FFI substrate: opaque foreign resources are linear handles (`~Copyable`), double-free is a compile error, extern borrows are call-scoped, callbacks run on VM-managed stacks, `ds bind` generates typed externs from headers. Embedding API: a handle table in the Lua shape (`ds_push`/`ds_ref`), values are indices into the VM's object table, and two embedders are two independent VMs. Layered on top: platforms are ordinary packages exporting effect handlers, selectable per build target; wasm/WIT is a tier-3 deployment target. Plain `extern` declarations remain the documented escape hatch inside the handle discipline.

**H6 — (b)+(c).** blake3 hash-cascade manifests (the manifest is the lock; identity flows transitively), MVS resolution, an output-hash content-addressed store that doubles as the bytecode cache, bin-first per-triple artifacts, an optional FHS profile linker, and the registry deferred to a static JSON index over the store. Layered script mode: `ds run script.ds` with zero config, bare-specifier std imports, `ds save` freezing a manifest. The five mandatory tooling commitments bind from day one: compiler-as-query-library, `ds fmt` with zero options, structured two-span linear diagnostics, a linear-aware REPL, and two-slot hot reload.

## H. Runtime

**H1 — (a).** Tier 1 typed register VM (3-10x of C) → tier 2 copy-and-patch stencils (1.5-3x, microseconds per unit) → tier 3 Cranelift or wasm (1-2x / 1.5-2.5x), all over one uop set (BL-5) and one bytecode-to-SSA lowering built early. Tier transitions are in-place J-variant opcode rewrites; OSR reduces to compiling the loop body; no deopt cliffs. Baseline BL-1 through BL-7 binds as fixed input. H1(b) is the recorded fallback if the per-arch stencil toolchain proves unacceptable.

**H2 — (a).** Guaranteed proper tail calls, self and mutual, are spec-level from day one: frame reuse plus argument slide in tier 1 (with tail-chain metadata so backtraces stay honest), label loops and slot-overlay `jmp` in JIT tiers, `return_call` on wasm. Callee argument slots overlay caller argument slots, locked at frame-layout time — this ABI and BL-7 are the two decisions to lock before anything else. The guarantee is scoped to tail calls within the same effect context; handler entry materializes a frame.

**H4 — (a).** Perceus precise RC with reuse threading plus per-frame bump arenas: zero pauses, zero barriers, no safepoints or GC maps in any tier, deterministic frees feeding deterministic deinits. The H4(d) two-hook collector interface (cycle-candidate registration, weak refs) is committed in the header format now as the documented growth path for shared cyclic workloads.

## Resolved open questions

1. **FFI pinning policy for non-copyable data crossing the C boundary.** The RC/arena heap never moves objects (H4a), so pinning is a lifetime question. Rule: extern parameters in borrow mode are call-scoped raw pointers that the type layer forbids escaping the call; owning foreign resources are `~Copyable` handles whose deinit releases exactly once; foreign callbacks execute on VM-managed stacks; a blocking foreign call pins its fiber, and carrier-thread execution is the escape when a blocking C call must overlap the scheduler; cancellation is observed only at VM re-entry, so a linear consume in flight is never interrupted.
2. **Nursery block shape and the extraction token set.** Block form: `nursery { ... }` containing `spawn { ... }` child expressions. Tokens finalized as researched: postfix `?` (propagation with a checked containment rule, no implicit conversions), `or` (first-wanted-member extraction, right operand evaluated lazily, with `or return` / `or break` / `or continue` early-exit forms), `is` (narrowing), `.err(h)` / `.ok(h)` (set-difference-typed handlers). No `?.` in v1.
3. **`.err` difference-typing fallback.** Moot: B1(c) is adopted, so difference typing stands as designed.
4. **Fibers vs hidden generators: one facility or two.** The v0 hidden iteration protocol is a linear cursor with no fiber involvement; once E2(b) fibers exist, the hidden lowering may adopt the same fiber facility. One shared VM mechanism is permitted and staged; the user surface is identical either way.
5. **Generator arrow `-*>' (historical README).** Retired; no public generator syntax in 1.0 (D3d). Recorded as prior art for the arrow-encodes-kind idea.
6. **Scoped named instances (B6b-lite).** Deferred additive, post-1.0 unless disjoint-head resolution demonstrably fails library authors earlier; ambiguity remains an error in any future sugar.
7. **Disjointness algorithm and recursive-instance termination.** Implementation-defined with fixed requirements: the check runs over the final instance environment at link time; an overlap error must name both heads and a witness value type in their intersection; a conditional instance whose satisfaction requires itself is rejected by a structural cycle check over resolution steps.
8. **Float equality under `==`.** Bit-structural (reflexive, total, dict-safe); IEEE semantics via named `is_float_eq` / `is_approx_eq`.
9. **Comptime reflection in v1.** Staged: built-in derives first, comptime evaluator plus reflection before 1.0.
10. **`@` binders in match.** Excluded from v1; deferred additive with affine re-binding semantics to be designed if demanded.

## The decided language

DreamScript is a standalone general-purpose interpreted language (embeddable via platform packages) with hybrid set-theoretic types — nominal tags composing under union, intersection, and difference, `is` narrowing, `T?` as an alias for `T | none` — checked bidirectionally with required boundary signatures and flow refinement, and universal structural equality with five carve-outs. Values are affine by default with an opt-in `~Copyable` linear resource layer (`deinit`, phantom states), second-class parameter modes (borrow / `mut` / consume), exclusion-window mutation, and linear-mutable COW collections; memory is Perceus reference counting with block reuse over per-frame arenas, with no GC. Polymorphism is coherent set-theoretic traits with pairwise-disjoint instance heads, one generic body carrying evidence words and inline caches; metaprogramming is comptime reflection without macros. Errors are union members handled by `?` / `or` / `.err` beside an unhandleable trap; concurrency is VM fibers in mandatory nurseries with fork-join parallelism on the linear/immutable sendability substrate; effects stay beneath the surface over an evidence-passing handler runtime with capability-parameter IO. The surface is TS-family braces with full UFCS, const-arrow functions, Rust-shaped matching with set-difference diagnostics, and Kotlin interpolation, under a 22-feature / 35-unit sugar budget. Modules are definition < file < package < module with generated interfaces and no import-time execution; packages are blake3 hash-cascade manifests over a content-addressed store with a zero-config script mode; FFI is linear C handles. Execution is a three-tier pipeline — typed register VM, copy-and-patch JIT, Cranelift/wasm — with spec-guaranteed self-and-mutual tail calls.

## Staging (v0 → 1.0)

Ordered; each step lands on the previous one with no language-change rewrites.

1. Lock the two load-bearing contracts first: the slot-overlay call ABI (H2a) and the fixed-width mmap-able dsmod format (BL-7, G3.5). Everything downstream compiles against them.
2. Checker core: B1(c) set algebra over nominal tags, B2 bidirectional plus refinement, B3(d) products/sums, B4(c), B7(a), B8(a), B9(a); affine checker plus DI (C1b, C5c).
3. Tier-1 interpreter: computed-goto or tail-call-threaded uop loop, unboxed slots, tag+payload ADTs, CALLT frame-reuse TCO.
4. Memory: Perceus dup/drop placement, reuse threading, per-frame arenas; runtime `rc==1` checks and the borrow counter as the diagnostic layer.
5. Concurrency: fibers, nursery, internal `cancel`/`discontinue` evidence dispatch, fork-join combinators.
6. Derives; then the comptime evaluator and reflection API (before 1.0).
7. Tooling commitments from day one, sequenced with the query compiler they consume: `ds fmt`, structured diagnostics, REPL, two-slot hot reload.
8. Tier 2 copy-and-patch (x86-64, then aarch64; tier-1-only elsewhere), bytecode-to-SSA lowering built early, tier 3 on demand.
9. Deferred-additive backlog after the core is stable: `|>`, anonymous record sugar, `@` binders, vararg/spread, `?.` desugar, `throws(...)` sugar, user-defined handlers, scoped named instances; borrow inference, TRMC, and fusion upgrades land as pure optimization with zero language change.

## Seed test corpus

Per the decision protocol, the chosen combos' example programs are the language's first test corpus, in catalog order:

- `01-types.md` Combo 3 "Hybrid" — config parser exercising tags, unions, narrowing, error sets, structural `==`.
- `02-linearity-ownership.md` Combo A "Modes and RC" — linear File with deinit, `mut` window reads, COW list update, fused map chain.
- `03-effects-errors-async.md` Combo B "Effect-ready" — nursery fetchAll with cancel-on-failure, union errors, par_map/par_reduce.
- `04-syntax.md` Combo 1 "TS-heart" — the full surface program (guards, `or return`, `?`, `.err`, interpolation, destructuring).
- `05-generics-metaprogramming.md` Combo 1 "Coherent-traits" — `~Copyable` generic stack, disjoint-head rejection case, derive plus comptime schema.
- `06-modules-tooling.md` Combo 1 "Hash-core" — ds.pkg manifest, selective imports, linear sqlite handle binding.
- `07-runtime.md` Combo A — `sum_to` uop encoding and the mutual `even`/`odd` tail-call encoding.
