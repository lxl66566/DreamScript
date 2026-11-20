# DreamScript decision map

This file is the master worksheet of the DreamScript language design. Every decision point of the language is listed here with its options, the recommendation from its catalog document, and a pointer to where the full analysis lives. Status: Phase 3 complete — every row carries a Decision (recorded 2026-11-08); the full decision record with staging and resolved open questions is `docs/en/design/08-decisions.md`, and the detailed per-point rationale is `docs/en/design/09-decision-rationale.md`.

How to read: each row lists the options in the order presented by the catalog document (letters match the documents). "Rec" is the catalog's recommendation with its one-line rationale; "Decision" is the author's Phase 3 pick, mirroring `08-decisions.md`. Options marked "likely-rejected" survived analysis only as reference points.

## Document index

| File | Domain | Points |
|---|---|---|
| docs/en/design/01-types.md | Type system | B1, B2, B3, B4, B7, B8, B9 |
| docs/en/design/02-linearity-ownership.md | Linearity, ownership, memory, data | C1-C5, D1-D3 |
| docs/en/design/03-effects-errors-async.md | Effects, errors, async, parallelism | B5, E1-E5 |
| docs/en/design/04-syntax.md | Syntax surface and sugar budget | F1-F7 |
| docs/en/design/05-generics-metaprogramming.md | Polymorphism, metaprogramming | B6, G1, G2, H3 |
| docs/en/design/06-modules-tooling.md | Modules, visibility, FFI, ecosystem | G3, G4, H5, H6 |
| docs/en/design/07-runtime.md | Execution pipeline, TCO, memory runtime | H1, H2, H4 |
| docs/en/design/08-decisions.md | Phase 3 decision record | all |
| docs/en/design/09-decision-rationale.md | Phase 3 decision rationale | all |

Raw research behind every claim: docs/en/research/01-06 (per-domain notes with primary-source citations).

## A. Identity (decided in this file, no separate catalog)

- A1 Positioning: (a) standalone general-purpose scripting language, (b) embeddable-first (host provides everything, platform model), (c) standalone core with an embeddable platform mode layered on, (d) web/wasm-first. Rec: (c). **Decision: (c)** — the perf charter requires a real standalone runtime; the platform mode is then "just packages exporting effect handlers" (docs/en/design/06 H5b), giving Lua-niche embedding for free.
- A2 Primary workloads (multi-select): (a) CLI/tools/scripts, (b) web backend, (c) games/simulation (needs hot reload), (d) AI/numeric (Mojo niche). Rec: (a) first, (c) second. **Decision: (a) primary, (c) secondary** — both are served by the interpreter-first pipeline and hot-reload tooling; (d) stays reachable via H1a's Cranelift tier without shaping v1.
- A3 Compatibility policy: (a) rapid-break v0, no compat promise before 1.0 (Roc/MoonBit posture), (b) semver discipline early (Cargo posture), (c) content-addressed forever (Unison posture). Rec: (a) until 1.0, then (b). **Decision: (a) then (b)**; (c) rejected as the source-level identity model, adopted at the package level (H6b) where its benefits are real.

## B. Types (docs/en/design/01-types.md)

| ID | Question | Options | Rec | Decision |
|---|---|---|---|---|
| B1 | Type-system foundation | (a) set-theoretic structural core; (b) nominal ADT core; (c) hybrid: nominal tags composing set-theoretically (union/intersection/negation over named types); (d) old-README set declarations + `any` | (c) | **(c)**; `any`-fallback rejected; (b) recorded as the reversible fallback if the disjointness machinery slips |
| B2 | Inference | (a) bidirectional + boundary annotations; (b) full HM; (c) refinement on a bidirectional skeleton; (d) annotations everywhere | (c) | **(c) on (a)** |
| B3 | Products and sums | (a) anonymous structural; (b) nominal structs/enums; (c) extensible rows; (d) structs/enums + tuples, anonymous literals as sugar | (d) | **(d)**; anonymous-record sugar deferred additive (F7 row 25) |
| B4 | Nullability | (a) `T \| none` union discipline; (b) language null + strict flow typing; (c) (a)'s semantics with a zero-cost `T?` spelling | (c) | **(c)**; no `?.` in v1 |
| B7 | Numeric tower | (a) Int(i64)/Float(f64) defaults, suffixes, trapping overflow + explicit `+%`; (b) bigint default; (c) full machine tower; (d) (a) + decimal as library | (a) | **(a)** + (d)'s decimal library path |
| B8 | Strings | (a) immutable UTF-8 bytes, checked slices typed `String \| none`, graphemes in stdlib; (b) interned strings + builder; (c) UTF-16; (d) (a) + rope | (a) | **(a)**; rope as future library type |
| B9 | Equality/ordering/hash | (a) universal structural `==` with five totality carve-outs; (b) trait-mediated; (c) builtin + derives; (d) reference identity | (a) | **(a)**; floats bit-structural with named `is_float_eq`; Ord/Hash/Show via derives |

## C/D. Linearity, ownership, memory, data (docs/en/design/02-linearity-ownership.md)

| ID | Question | Options | Rec | Decision |
|---|---|---|---|---|
| C1 | Linearity flavor | (a) explicit-linear-as-default; (b) affine core + opt-in `~Copyable` linear resource layer; (c) fully hidden linearity; (d) single-ownership + runtime borrows + opt-in static linear | (b) | **(b)**; (c)'s inference staged as later pure optimization, (d)'s borrow counter staged as the v0 diagnostic layer |
| C2 | Borrowing | (a) none (moves + copies); (b) compiler-invisible inference; (c) runtime borrow counter; (d) second-class parameter-passing modes (Hylo `let/inout/sink`, Swift `borrowing/consuming`) | (d) | **(d)** |
| C3 | Immutability mechanics | (a) val/var, var is local-only; (b) val/var + statically-guaranteed exclusion windows; (c) + typed interior mutability; (d) immutable core + FBIP | (b) | **(b)**, with (d)'s FBIP as the runtime guarantee underneath |
| C4 | Memory strategy | (a) Perceus precise RC + block reuse; (b) RC + generational references; (c) tracing GC; (d) arenas primary + RC at escape boundaries | (a)+(d) | **(a)+(d)**; cycles type-level rejected in v0 |
| C5 | Move/drop/definite-init | (a) implicit drop; (b) explicit `drop` + DI checks; (c) RAII deinit restricted to the `~Copyable` layer; (d) effect-handler finalizers | (c) | **(c)**; DI as infrastructure; explicit `drop` as debug/FFI tool only |
| D1 | Mutation confinement | (a) function-local var only; (b) exclusion-window mutation; (c) capability-gated mutation; (d) pure surface | (b) | **(b)** |
| D2 | Collections | (a) persistent only; (b) linear-mutable COW (functional API, in-place when unique); (c) dual-mode; (d) pragmatic hybrid | (b) | **(b)**; (d) is its stdlib outcome, a separate classification rule |
| D3 | Iterators | (a) linear cursors; (b) first-class generators; (c) combinators only; (d) combinators + hidden generators (fusion) | (d) | **(d)**; no public generator syntax in 1.0, `-*>'` retired |

## E. Errors, effects, async, parallelism (docs/en/design/03-effects-errors-async.md)

| ID | Question | Options | Rec | Decision |
|---|---|---|---|---|
| B5 | How signatures carry errors | (a) inferred tag unions (Roc); (b) explicit `Result<T,E>`; (c) effect rows; (d) union-carried base + opt-in rows later | (d) | **(d)**; `throws(...)` stays later additive sugar |
| E1 | Effect system | (a) full row-polymorphic resumable effects; (b) abilities-lite (single IO ability); (c) capability parameters, no effect syntax; (d) deferred: Result+panic now over an evidence-passing handler runtime | (d) | **(d)** with (c) as the stdlib idiom; ambient default handler for scripts, explicit capability for tests/embedding |
| E2 | Async model | (a) async-as-effect, no `await` (Koka); (b) VM fibers + mandatory structured concurrency (nursery/Job-tree); (c) unstructured stackful spawn (Go); (d) colored async/await (TS) | (b) | **(b)** |
| E3 | Parallelism | (a) shared memory made safe by sendability typing; (b) fork-join data parallel on the (a) substrate; (c) actors/isolation; (d) single-thread + FFI parallelism | (b) on (a) | **(b) on (a)** |
| E4 | Error handling | (a) Result + unhandleable trap split; (b) union errors + `?`/`or`/`.err` set-difference handlers; (c) exceptions-as-effects; (d) let-it-crash supervision layer | (b) + (a)'s trap | **(b) + (a)'s trap**; (d) as a library pattern |
| E5 | Cancellation/resources | (a) nursery-scoped cooperative cancellation; (b) linear consume-to-close; (c) defer/errdefer blocks; (d) timeouts as handlers | (a)+(b) | **(a)+(b)**; (c) as sugar, (d) as library |

## F. Syntax (docs/en/design/04-syntax.md)

| ID | Question | Options | Rec | Decision |
|---|---|---|---|---|
| F1 | Global shape | (a) TS-family braces; (b) Gleam/Roc braces; (c) Koka indentation; (d) Rust-family | (a) | **(a)**; (b) is the recorded parser fallback |
| F2 | Bindings/functions/returns | `const`/`var`; named fns as const-arrow; `name: T` + `-> T`; final-expression returns with `or return`; README's `?`-return marker retired | as stated | **as stated**; `?` reserved for propagation |
| F3 | UFCS/pipes | (a) Koka full UFCS; (b) + Gleam first-arg `\|>`; (c) method-only; (d) pipe-first | (a) | **(a)**; `\|>` deferred additive; capability params never in the dot position |
| F4 | Pattern matching | (a) Rust-shaped: guards, or-patterns, exhaustive with set-difference diagnostics; (b) Kotlin when-guards; (c) Gleam guardless; (d) OCaml when/as | (a) | **(a)**; `@` binders excluded from v1, deferred additive |
| F5 | Operators | (a) fixed trait-driven set, frozen C-family precedence; (b) no overloading; (c) free overloading/custom ops; (d) comptime-extensible | (a) | **(a)** + Zig-style `+% -% *%` overflow spelling; frozen precedence table |
| F6 | Strings/literals | (a) `$x`/`${expr}`; (b) `\(x)`; (c) `{x}` format-fn; (d) none | (a) | **(a)** + `r#"..."#` raw strings + `'x'`/`b'x'` literals + explicit `str()` boundary |
| F7 | Sugar budget | Minimal 12/18u; Balanced 22/35u; Generous 26/40u — against Kotlin's 38/74-76u ceiling | Balanced | **Balanced** (22/35u); headroom reserved for `\|>`, record sugar, `@` binders, vararg |

## G. Polymorphism, metaprogramming, modules (docs/en/design/05 + 06)

| ID | Question | Options | Rec | Decision |
|---|---|---|---|---|
| B6 | Polymorphism mechanism | (a) coherent set-theoretic traits, disjoint instance heads; (b) explicit dictionary passing; (c) ML modules; (d) comptime generics | (a) | **(a)**; foreign-type instances legal, disjointness checked at declaration and link; (b)'s scoped named instances deferred additive |
| G1 | Metaprogramming | (a) none beyond derives; (b) comptime evaluation + static reflection, no macros; (c) hygienic macros; (d) proc macros | (b) | **(b)**; derives ship first, comptime evaluator + reflection before 1.0 |
| G2 | First-class types | (a) types as runtime values; (b) comptime-only reflection; (c) runtime metadata tables; (d) tags-as-data | (b) + (d) | **(b) + (d)** |
| H3 | Generics execution | (a) one body + evidence words + polymorphic inline caches + lazy JIT specialization; (b) bytecode monomorphization cache; (c) erasure with boxing; (d) comptime mono | (a) | **(a)**; no HKT/GATs in 1.0, variance by set inclusion |
| G3 | Module system | (a) Rust crate tree; (b) MoonBit-flavor layering, generated `.dsi`, order-independent blocks, intra-package cycles legal, no import-time execution; (c) Unison content addressing; (d) TS/ESM | (b) | **(b)**; `///\|` optional separator, G3.5 dsmod contract binding |
| G4 | Visibility | (a) pub/priv; (b) layered + selective exports; (c) capsules; (d) Go convention | (b) | **(b)**: private / `pub(pkg)` / `pub` + selective imports |
| H5 | FFI/embedding | (a) linear-typed C handles + handle-table embedding; (b) platform-as-packages-exporting-handlers; (c) manual `extern`; (d) wasm-first | (a) + (b) | **(a) substrate + (b) layered**; (c) documented escape hatch; pinning policy resolved in 08 |
| H6 | Ecosystem stack | (a) Cargo-style; (b) hash-store native; (c) bun-style zero-config scripts; (d) platform-first | (b) + (c) | **(b) core + (c) script mode**; five mandatory tooling commitments bind from day one |

## H. Runtime (docs/en/design/07-runtime.md)

| ID | Question | Options | Rec | Decision |
|---|---|---|---|---|
| H1 | Execution pipeline | (a) tier1 typed register VM + tier2 copy-and-patch + tier3 Cranelift/wasm over one shared SSA lowering; (b) register VM + single method JIT; (c) AST interpreter + meta-tracing; (d) wasm-AOT primary | (a) | **(a)**; (b) recorded as the stencil-toolchain fallback |
| H2 | Tail calls | (a) guaranteed TCO in the spec from day one (self + mutual), frame reuse + slot-overlay ABI; (b) best-effort; (c) user trampolines; (d) effects-only tail resumptions | (a) | **(a)**; scoped to the same effect context |
| H4 | Memory runtime | (a) Perceus RC + block reuse + per-frame arenas; (b) generational moving GC; (c) malloc/free, no GC; (d) hybrid with deferred collector | (a) | **(a)**, with (d)'s two-hook collector interface committed as the growth path |

Fixed runtime baseline shared by all H1 options (docs/en/design/07 "Fixed baseline"): typed 32-bit register bytecode, unboxed slots (no NaN-boxing), computed-goto/musttail dispatch, profile-driven superinstructions, uop decomposition from day one, mmap-able dsmod bytecode format. Binding as fixed input per Phase 3.

## The decided language (Phase 3, all rows resolved)

A standalone scripting language (embeddable via platform packages) with hybrid set-theoretic types (named tags, union/intersection/negation, `is` refinement, `T?` sugar over unions), affine moves plus an opt-in `~Copyable` linear resource layer with second-class parameter modes and exclusion-window mutation, universal structural equality, Perceus reference counting with block reuse and per-frame arenas, coherent traits with disjoint instance heads over one generic body with evidence words and inline caches, comptime reflection without macros, union-carried errors handled by `?`/`or`/`.err` with a separate unhandleable trap, VM fibers with mandatory nurseries and fork-join parallelism over a sendability-proven substrate, a TS-family brace syntax with Koka UFCS under a 22-feature/35-unit sugar budget, MoonBit-flavor layered modules with generated interfaces and no import-time execution, a blake3 hash-store package manager with zero-config script mode, linear C-ABI handles for FFI, and a three-tier runtime (typed register VM, copy-and-patch JIT, Cranelift/wasm) with spec-guaranteed tail calls. Full record, staging plan, and resolved open questions: docs/en/design/08-decisions.md.

## Open questions (resolved in Phase 3)

All resolved in docs/en/design/08-decisions.md "Resolved open questions": FFI pinning policy (call-scoped borrows, linear handles, pinned fibers for blocking calls, carrier-thread escape); nursery block shape (`nursery { }` + `spawn { }`) and the finalized `?`/`or`/`is`/`.err` token set; `.err` difference typing (moot, B1c adopted); fibers vs hidden generators (cursor in v0, one shared fiber facility permitted later, no surface change); generator arrow `-*>'` (retired). Additionally resolved from docs/en/design/05: scoped named instances (deferred additive), disjointness/termination requirements (link-time check, witness-naming errors, self-satisfaction rejected), float `==` (bit-structural, named IEEE comparisons), comptime in v1 (derives first, evaluator before 1.0), `@` binders (excluded from v1).

## Decision protocol

1. Read this map top to bottom; adopt a named combo per document where offered (docs/en/design/01 "Hybrid", 02 "Modes and RC", 03 "Capability-core"/"Effect-ready", 04 "TS-heart", 05 "Coherent-traits", 06 "Hash-core", 07 Combo A) or pick options individually.
2. Record decisions by editing this file: mark the chosen letter per row; unresolved rows stay as-is.
3. Once every row has a decision, AGENTS.md status moves to Phase 4 (implementation); the chosen combos' example programs become the language's first test corpus.

Protocol complete (2026-11-08): every row resolved, all named combos adopted in their documented shape ("Effect-ready" for 03), decisions and rationale recorded in docs/en/design/08-decisions.md, seed test corpus listed there. AGENTS.md status has moved to Phase 4.
