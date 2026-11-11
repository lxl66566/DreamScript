# DreamScript implementation plan

Status: planning record, written 2026-11-09. This document plans the full implementation of DreamScript from empty repository to 1.0 in Rust (edition 2024). It is derived from and subordinate to `docs/en/design-decisions.md`, cited throughout by decision ID ([B1], [H2], ...); when this plan and that record conflict, the record wins and this plan is fixed. The milestone order refines the staging order of the record's section 10: M0 locks the two load-bearing contracts, M1 inserts the front end ahead of the checker (the record's step 2 presumes a parser), and M2-M9 follow the recorded staging one to one.

Companion documents: `docs/en/status.md` (what runs today and the boundary of each milestone), `docs/en/todo.md` (the ranked open-work queue), `docs/en/pitfalls.md` (invariants and lessons that prevent breakage), `docs/en/memory-arc.md` (the memory execution contract). This file carries scope, structure, and methodology; it never restates landed history.

## 1. Ground rules

Binding constraints, taken from the charter and the record:

1. Staging order follows the record section 10; each step lands on the previous one with no language-change rewrites.
2. The two load-bearing contracts — the slot-overlay call ABI [H2] and the fixed-width mmap-able dsmod format [BL-7, G3.5] — are locked before anything else; everything downstream compiles against them.
3. The two-slot hot-reload module runtime exists from day one: data structures and qualified cross-module calls at M0, the `ds dev --hot` UX at M7; retrofitting is the named mistake to avoid [H6 item 5].
4. Compiler-as-query-library from the first line: every consumer (CLI, LSP, REPL, hot reload, formatter) goes through memoized queries — parse, block_index, resolve, type_of, effects_of, linearity_check, lower — with invalidation at `##` separator-block grain plus package grain [H6 item 1; separator spelling per G3].
5. Invariants that hold in every milestone: no import-time execution [G3, the one exception being entry-file script-mode statements per G3]; guaranteed self-and-mutual TCO in every tier [H2]; dispatch correctness never depends on cache state [H3]; no `any` fallback — inference failure is a compile error [B1]; module bodies are pure compile-time declarations.
6. Explainability budget: any addition must re-fit the short tutorial; the deferred-additive backlog [section 11 of the record] stays out until the core is stable.
7. Casing carries no grammatical meaning; all type names in implementation tests, stdlib, and docs are lowercase (`int`, `string`) [section 0 of the record].

Rust engineering conventions: edition 2024; `cargo fmt` before every commit; clippy clean; tests run via cargo-nextest; release profile `codegen-units = 1` for all performance work (thin LTO only once hot paths cross crates, decided at M8); parallel worktrees share one `CARGO_TARGET_DIR`; benchmarks run under an exclusive `flock`, compiles and tests under shared locks. Crate choices follow section 3.

## 2. Workspace layout

One Cargo workspace at the repository root; a crate appears in the milestone that first fills it (no empty shells).

| Crate | Owns | Appears |
|---|---|---|
| `ds-diag` | Span model, error-code registry, diagnostic rendering, `--error-format=json` | M0 |
| `ds-abi` | Typed frame layout, slot-overlay call convention, CALLT semantics, two-word entry descriptors, tail-chain backtrace metadata | M0 |
| `ds-mod` | dsmod writer/reader, mmap loader, blake3 source hashing, cache path layout | M0 |
| `ds-query` | Memoized query engine and block/package-grain invalidation | M1, grows every milestone |
| `ds-parse` | Lexer, pest grammar, AST with spans and trivia attachments, `##` separator blocks | M1 |
| `ds-fmt` | Block-aware formatter, zero options; later the LSP on-type-formatting path | M1 |
| `ds-check` | Set-theoretic type core, emptiness/disjointness, bidirectional checking with refinement, affine/DI analysis, trait/instance coherence | M2 |
| `ds-lower` | Typed AST to frame layout to uop bytecode; dup/drop placement (Perceus pass at M4) | M3 |
| `ds-vm` | Tier-1 interpreter, fibers, nurseries, two-slot module table, inline caches, builtins | M0 skeleton, M3 real, M5 fibers |
| `ds-rt` | Per-frame arenas, precise non-atomic RC, reuse threading, object layout (the unsafe core) | M4 |
| `ds-meta` | Derive engine, comptime evaluator, static reflection | M6 |
| `ds-pkg` | ds.pkg manifests, MVS resolution, content-addressed store, script mode | M7 |
| `ds-ffi` | `extern` declarations, `ds bind` tooling, embedding API | M7 |
| `ds-jit` | bytecode-to-SSA lowering, copy-and-patch stencils (tier 2), Cranelift/wasm (tier 3) | M8, SSA may start M7 |
| `ds-cli` | The `ds` binary (clap): run, build, info, fmt, dev, save, bind, link | grows from M3 |
| `ds-lsp` | LSP server over `ds-query` | M7 |

Supporting directories: `conformance/` (the seed corpus ported to the decided surface, with per-stage expectations and golden outputs) from M1; `fuzz/` (cargo-fuzz targets) from M0; `benchmarks/` for the cross-language suites.

Consumption shape (arrows read "is consumed by"):

```text
ds-parse ─► ds-query ─► ds-check ─► ds-lower ─► ds-vm (tier 1) ─► ds-jit (tiers 2-3)
                            │           │  │
                        ds-meta         │  └─► ds-mod (writer)
                                        └─► ds-rt (arenas, RC)
ds-abi ─► ds-lower, ds-vm, ds-jit          ds-mod ─► ds-vm (loader), ds-pkg (store)
ds-diag ─► everything                      ds-cli, ds-lsp on top of ds-query and the above
```

## 3. Dependency policy

House-preferred crates plus domain needs; new dependencies enter at the latest version via `cargo add`:

- `pest` + `pest_derive` — the parser. If prototyping shows the arrow-heavy grammar fighting PEG, the escalation order is: hand-written recursive descent behind the same `parse` query (no surface change), then the recorded fallback F1(b) Gleam/Roc brace headlines (surface change).
- `blake3` — source and package hashing [G3.5, H6]; `hex-simd` — hash rendering.
- `mimalloc` — global allocator fallback under the reuse path [C4].
- `memmap2` — the dsmod mmap load path [G3.5].
- `clap` (features = derive) — the `ds` CLI; `thiserror` — library errors (anyhow is allowed in the binary only); `spdlog-rs` (features flexible-string, log) — logging configured in `ds-cli` only, libraries use the `log` facade.
- `tempfile` — atomic writes in the package store.
- `cranelift-codegen` — tier 3 native; `wasm-encoder` — tier 3 wasm.
- `crossfire` (never crossbeam-channel) if channels appear; `youpipe` — candidate fork-join scheduler backend at M5, decided against hand-rolled work-stealing by measurement.
- Development: cargo-nextest, cargo-fuzz, criterion (wall-clock benches), gungraun (deterministic instruction-count benches on Linux), cargo-show-asm for VM loop and stencil inspection.

## 4. Milestones

Each milestone states its scope, work items, and acceptance criteria. What has landed and what boundary it left lives in `docs/en/status.md`; the residual work lives in `docs/en/todo.md`.

### M0 — Contracts and bootstrap

Lock the two load-bearing contracts as crate docs plus tests before any compiler exists: ds-abi (typed frame layout [BL-6], the slot-overlay convention with owner-relative word mapping, CALLT semantics, two-word entry descriptors, tail-chain metadata, the BL-5 uop set with canonical 32-bit encodings), ds-mod per [G3.5] (strict-canonical reader, byte-stable writer, blake3-128 checked-hash validation, mmap loader, dormant two-hook collector fields, fuzz target), ds-diag, and the ds-vm skeleton with the two-slot module table and fully-qualified cross-module calls [H6 item 5] executing hand-assembled fixtures.

Acceptance: mutual `even`/`odd` at depth 10^7 on a constant stack, dsmod write-then-read byte-stable, a corrupted file failing the hash check with a structured diagnostic.

### M1 — Front end

Lexer and pest grammar for the full surface [F1, F2, F6, B7] with structured interpolation holes (each a typed expression, not lexer concatenation), trivia-attached lossless AST, separator blocks [G3]; ds-query runs the real parse and block-index queries with syntax errors as data (broken files never poison sibling blocks); ds-fmt is the block-aware zero-option formatter over the parse query [H6 item 2]; the seed corpus lives in `conformance/` with per-file porting notes and stage expectations (parse, typecheck at M2, execute at M3-M6).

Acceptance: a parser fuzz campaign (arbitrary bytes through the public parse API) with zero panics, plus a nextest mirror (random bytes plus corpus-seed mutations) in CI.

### M2 — Checker core

The whole static story as a head-less library: the B1 set-algebra core with the emptiness/disjointness procedure (canonical atom-set solving, law tests against a bitset oracle), B2 bidirectional type_of with the flow-refinement overlay and `?` containment [DS2008], B6 instance environment (union-head decomposition, witness-carrying overlap errors, condition-cycle rejection), C1/C5 affine checking (golden DS3001/3002/3004 two-span diagnostics; Linearity exposes moves/drops/last_uses for the lowerer), E1 effect rows (EffectRow bitset, least-fixpoint effects_of consumed at M5/M7), the B7/B8/B9 numeric/string/equality tables, package-wide resolution with the UFCS order [F3], and `analyze() -> Analyzed` as the linearity/lowering seam beside the query path.

Acceptance: the negative suite renders golden diagnostics (overlap witness, moved-here/used-again pairs, set-difference exhaustiveness, inference failure without any `any` fallback).

### M3 — Lowering and tier-1 VM

ds-lower lowers the checked subset to static frame layouts and uops (overlay staging with per-word Raw widening, `?`/`.err` to TAG_TEST + Jmpif + RETSPAN respan exits, Perceus dup/drop placement); ds-vm tier 1 executes from encoded programs (decode-on-dispatch, unboxed slots, tag+payload unions across consecutive slots, frame-reuse TCO with argument slide, tag-keyed 1-2 entry inline caches with full-lookup fallback [H3]); `ds run <file.ds>` and `ds build <file.ds>` compile source end to end with the source hash recorded; natives split per [H5.5] into `builtin` (pure core, every VM) and `std` (the default platform surface, CLI-installed).

Acceptance: every file in `examples/` runs green; the lowered subset boundary in `docs/en/status.md` matches the corpus; diagnostics stay two-span goldens.

### M4 — Memory

Goal: the Perceus/arena runtime with deterministic death.

Work items:

- `ds-rt`: per-frame bump arenas (mark at entry, bump-allocate, restore on every pop path; segment growth by chunk append so captured frame addresses stay stable; per-fiber marks so fiber switching needs no arena action) [C4, H4].
- Precise non-atomic RC: compiler-inserted dup/drop via the Perceus placement pass in `ds-lower`; each drop threads to the next allocation as a reuse candidate (`alloc_at`), mimalloc fallback [C4].
- Diagnostic layer shipping first [C1 staging]: runtime `rc==1` uniqueness checks plus an Inko-style borrow counter, behind a flag, kept shippable — it is the debugging instrument for the whole memory story.
- `deinit` execution [C5]: static dispatch (concrete type known), exactly once at the unique instance's death, on every path including early return, panic, and cancellation teardown; explicit `drop` as a debug/FFI tool.
- Cycles rejected at the type level for v0; the two-hook collector interface stays header-committed and dormant [C4].

The memory endgame (per-fiber heaps, arena temporaries, inline multi-word spans) extends this milestone; its execution contract is `docs/en/memory-arc.md`.

Acceptance: linearity_modes_rc.ds runs with deinit observed on success, early-return, and panic paths (via test-only observers); free order is deterministic across repeated runs; arena-residency accounting for the corpus reports 100% of non-escaping temporaries as register/arena-resident.

### M5 — Concurrency

Goal: fibers and nurseries over the same VM; race-free fork-join.

Work items:

- Fibers as interpreter data [E2]: heap segments in the VM pool, spawn is one allocation, switch is an O(1) pointer swap; chunk-list segment growth (stable frame addresses); per-fiber TCO intact (tail-looping fibers never grow their segment).
- `nursery` blocks and `spawn` [E2]: scope exit joins children; first failure cancels siblings; child errors union out at the boundary; bare detach requires an explicit root nursery or supervisor.
- Internal effects `cancel`/`discontinue` dispatched by evidence at nursery boundaries; tail-resumptive internal ops allocate nothing [E1]; cooperative cancellation observed only at the M3 preemption points, so a linear consume in flight is never interrupted; teardown walks frames running deinits [E5].
- Panic path [E4]: the fiber aborts, linear deinits run during the frame walk, the trap surfaces at the nursery or supervisor boundary.
- Fork-join [E3]: `par_map`/`par_reduce` over immutable or linear-partitioned inputs; ownership transfer at fork keeps RC counts thread-confined; cheap fork-side snapshots via static RC counts; deterministic and replayable; scheduler backend decided by measurement (`youpipe` vs hand-rolled work-stealing).

Acceptance: effects_nursery.ds conformance; cancellation of a many-spawn nursery takes effect only at safe points; par combinators replay identically across runs; a deep tail-looping fiber runs unbounded without segment growth.

### M6 — Derives, comptime, reflection

Goal: metaprogramming without macros.

Work items:

- Derive engine [G1]: `hash`, `show`, `ord` (field-order lexicographic; the `total_compare` float variant must be supplied explicitly for sort keys), `clone` (deep copy), `to_json`/`from_json`, `default`; `==` is universal and never derived.
- Comptime evaluator [G1, G2]: ordinary code evaluated at compile time on the same uop VM in a fenced context (no world effects — the capability idiom is enforced at the fence); static reflection over tags-as-data (fields and cases of a type, comptime iteration and branching, `comptime T: type` parameters); no runtime type metadata tables anywhere; existentials as evidence-carrying boxes (`box Json` — value plus dictionary) for heterogeneous collections.

Acceptance: generics_coherent_traits.ds runs including `schema(comptime T)`; user derives cover user structs; the comptime fence rejects world-touching calls as compile errors.

### M7 — Modules, packages, FFI, tooling

Goal: the ecosystem layer and the five tooling commitments, complete.

Work items:

- Visibility and interfaces [G3, G4]: private by default, `pub(pkg)`, `pub`; selective, aliased, conditional imports and `pub import`; `.dsi` generation via `ds info` as the reviewable API surface, LSP hover source, and incremental-compilation boundary; cross-package cycles are link errors whose diagnostic suggests moving the shared abstraction to a base package; package-grain invalidation lands in `ds-query`.
- Loading [G3.5]: lazy dependency-driven loading from the dsmod header dep list; `$DS_CACHE/ir/<triple>/<blake3-of-source+deps>.dsmod` with the mmap hit path; `ds build --exe` embeds the dependency closure plus the VM; cold startup measured against the sub-10ms target.
- Packages [H6]: ds.pkg manifests with the blake3 hash cascade (the manifest is the lock, no lockfile); MVS resolution; a global content-addressed store doubling as the bytecode cache; script mode (`ds run script.ds` zero-config, bare `@std/...` imports, `ds save` freezing a script into a package); optional `ds link` profile linker (scoop shims on Windows).
- Registry [H6 registry ruling]: a git repository holding package metadata only, configurable as a local path or a remote URL; dependency resolution reads the index metadata (name -> versions -> manifest hashes and artifact URLs) while store fetch stays content-addressed and blake3-verified; dependencies are never resolved by cloning or tracking a VCS working tree (no branch heads, no mutable pseudo-versions); a local git index makes offline and private registries first-class.
- FFI [H5]: plain `extern` declarations as the documented escape hatch; linear C handles (`~Copyable`, double-free is a compile error, call-scoped raw borrows that cannot escape); callbacks on VM-managed stacks; `ds bind header.h` emitting typed externs then handle wrapping; the FFI-USED flag at load time; the embedding API in the Lua shape (`ds_push`/`ds_ref`, a `Value` as an index into the object table, `ctx: &mut Vm`, two embedders are two independent VMs, no GIL); blocking foreign calls pin their fiber; cancellation observed only at VM re-entry.
- Platforms [H5]: `platform "std" provides <console, fsys, net, time>` with the `ds` CLI as default; the checker verifies user code's effect usage is a subset of the platform's declared row (consuming effects_of from M2). Platform seam [H5.5, E1]: the world-touching natives migrate from the M3 stopgap native registry to effect-handler form behind the platform declaration — the `builtin`/`std` two-native-module split retires (the pure `builtin` core stays).
- REPL [H6 item 4]: linear-aware, prints inferred type and effect row, runs over the same bytecode IR, session-owned linear context with explicit `:drop` and `:reset`.
- Hot reload [H6 item 5]: `ds dev --hot` recompiles changed blocks, swaps the second slot, retires old-version frames at their next cross-module call, type-checks heap values against new defs — exercising the M0 structures end to end.
- LSP over `ds-query`: diagnostics, hover from `.dsi`, on-type formatting from `ds-fmt`.

Acceptance: a multi-package project (app plus local lib plus `@std`) builds, runs, and hot-reloads; modules_hash_core.ds conformance; `ds save` round-trips a script into a package; cold startup under 10ms on the mmap hit path.

### M8 — Tier 2 and tier 3

Goal: JIT tiers over the one shared lowering.

Work items:

- bytecode-to-typed-SSA lowering shared by all JIT tiers; may start during M7 once tier 1 is stable — it only consumes uops [H1].
- Copy-and-patch tier 2 [H1]: build-time clang/LLVM compiles a master template including the VM headers into one x86-64 fragment per uop, holes for frame-slot offsets and constants patched as u16/u64; tier transitions are in-place J-variant opcode rewrites (one dispatch); OSR reduces to compiling the loop body, no frame transformation machinery; the two-word entry descriptors flip on tier-up (the same invalidation story as hot reload); no guards on the data path — the only exits are debugger breakpoints, the per-entry stack-limit compare, effect-handler entry, and B7's cold overflow edge.
- Superinstructions [BL-4]: the assembler composes fusions (compare-and-branch, load-op, dense-tag match jump tables) from measured opcode frequencies, never hand-picked.
- aarch64 stencils after x86-64; platforms without verified stencil builds ship tier-1-only.
- Tier 3: the SSA path into Cranelift CLIF; wasm 3.0 with `return_call` carrying TCO.
- Differential testing: tier 1 vs tier 2 vs tier 3 over the full conformance corpus, observable behavior identical, TCO preserved in every tier.

Acceptance: performance gates against C baselines — tier 1 within 3-10x, tier 2 within 1.5-3x, tier 3 within 1-2x native and 1.5-2.5x wasm; short scripts (under 0.2s) never pay tiering; startup unaffected by JIT machinery.

### M9 — Hardening and 1.0

Goal: close the rapid-break window [A3].

Work items:

- Extended fuzz campaigns: lexer/parser, checker via a grammar-based program generator, ds-mod reader, VM differential; failed inputs shrink and land as committed regression seeds; long runs are interruption-tolerant and use at least 95% of cores.
- Performance regression gates: gungraun deterministic instruction counts on Linux plus wall-clock suites (criterion); startup bench (cold script, mmap hit) tracked per commit.
- Tutorial: the whole language explained in one short tutorial — the explainability-budget proof; full docs pass.
- Triage the deferred-additive backlog [section 11 of the record] against the remaining sugar headroom (16 features / 40-42 units); anything adopted ships as pure additive with no migration.
- Stage the post-1.0 pure-optimization queue (no language change): borrow inference, full reuse pairing, TRMC, fusion upgrades, `T | none` niche packing, rope.
- Version stamping in dsmod headers; semver discipline from 1.0 on.

### Scheduling

The milestone order is the schedule; the ranked order of the work inside the current milestone lives in `docs/en/todo.md`. Recorded as backlog beyond M8, not a restaging: a wasm runner (the tier-3 wasm execution surface plus a wasm-platform runner); major non-micro performance rounds once the tiers exist; docs completion plus a web playground; a cross-language example suite and benchmarks against peer scripting languages (python, lua, node, bun).

## 5. Test strategy

- Conformance corpus: the seed corpus ported to `conformance/` at M1 plus every bug regression; each file carries stage expectations and golden output; from M8 every file runs on every tier.
- Negative suite: golden-rendered diagnostics — overlap witnesses, moved-here/used-again pairs, set-difference exhaustiveness messages, inference failures.
- Differential: across tiers (M8) and across optimization toggles (reuse and fusion on/off at M4/M6) — identical observable behavior; par combinators replay deterministically (M5).
- Fuzz (cargo-fuzz): ds-mod reader (M0), lexer/parser (M1), checker generator (M2), VM differential (M3), query-engine edit sequences (M1, seeded nextest mirror in crates/ds-query/tests/invariants.rs); overflow checks and the diagnostic layer (rc==1 checks, borrow counter) stay on in fuzz and CI builds.
- Unit tests are few but sharp: the emptiness/disjointness procedure, refinement rules, the ABI argument-slide edge cases, and arena/RC invariants get dense coverage; trivial plumbing gets none.

## 6. Performance methodology

- Every performance change lands with a before/after bench; release profile `codegen-units = 1`; thin LTO once hot paths cross crates.
- On Linux: hotpath instrumentation and perf for hotspot discovery; gungraun for deterministic instruction-count regressions; cargo-show-asm for the VM dispatch loop and stencils.
- Fixed suites: startup (cold script, mmap hit, `--exe`), corpus throughput against C baselines, allocation and arena-residency counts, fiber switch and spawn cost, par-combinator scaling.
- When actual wall time and instruction counts disagree, re-measure and trust wall time. Benchmarks run under an exclusive `flock`; compilation and tests take shared locks.

## 7. Rust-specific design notes

- Dispatch [BL-3]: stable Rust has neither computed goto nor stable `musttail`; tier 1 starts as a dense `match` loop (LLVM lowers it to a jump table). If profiling shows dispatch cost, escalate behind a cfg — nightly tail-call threading or a build-script-generated asm dispatch — keeping it a build-portability choice, never a design point.
- Unsafe code is confined to `ds-rt` (arenas, RC, object layout) and `ds-jit` (stencil patching); both carry the debug-assertion diagnostic layer, and CI runs with it enabled.
- Fibers are interpreter data: no native stack switching, no async color; fork-join worker threads only receive ownership transfers, which keeps non-atomic RC sound [C4, E3].
- Stencil builds require clang/LLVM at build time; the CI matrix is windows-msvc plus linux; dsmod target classes make cross-platform artifacts first-class.
- The `ds` binary is the only crate configuring spdlog-rs; libraries log through the `log` facade; no anyhow in library crates.

## 8. Risks and recorded fallbacks

| Risk | Trigger | Fallback |
|---|---|---|
| Disjointness machinery slips schedule [B1] | emptiness checker not converging in M2 | B1(b) nominal-only core; error handling degrades to match-based, no difference-typed `.err` |
| Arrow-assignment grammar fights the parser [F1] | pest prototyping dead-ends in M1 | hand-written recursive descent behind the same `parse` query, then F1(b) Gleam/Roc brace headlines |
| Per-arch stencil toolchain unacceptable [H1] | windows-msvc stencil build unstable in M8 | H1(b) single method-JIT tier |
| Affine friction worse than expected [C1] | real corpus code drowning in move errors by M5 | C4(b) generational references |
| Sub-10ms startup missed [G3.5] | cold-run bench above target in M7 | profile the mmap path, strip debug spans, lazy section loading; `ds build --exe` as the floor |
| RC/arena soundness bugs [C4] | any use-after-free or double-free in fuzzing | the diagnostic layer (rc==1 checks, borrow counter) stays shippable behind a flag |

## 9. Scope guards

Permanently out or out of 1.0, enforced at review: no import-time execution; no runtime type metadata tables or type objects; no macros at any level; no user-defined operators, Deref coercions, or convention-based privacy; no hidden classes, field inline caches, speculation, deopt frames, or side traces in any tier; no GC in v0 beyond the committed two-hook interface; no HKT or GATs in 1.0; no `?.`, elvis, `!!`, smart casts, receiver-lambda DSLs, primary constructors, reified type parameters, or backtick identifiers [F7]; no public generator syntax in 1.0 [D3]; no `|>`, anonymous record literals, `@` binders, labeled break/continue, or `it` shorthand unless triaged in at M9 [section 11 of the record]; no `or` extraction operator, truthiness coercion in boolean contexts, chained comparisons, comprehension syntax, subscript slicing sugar, vararg/spread packing, or word-form operators beyond `in`/`is` — permanently [E4, F8].

## 10. Working discipline

- Design changes go to `docs/en/design-decisions.md` first, then this plan follows; `docs/en/status.md` records what runs and what boundary each milestone left, `docs/en/todo.md` the ranked open work, `docs/en/pitfalls.md` the invariants that prevent breakage. No file restates another.
- Landed work with no residual discussion is deleted from the docs and lives in git history; every commit updates the progress, pitfall, and next-step files in the same commit as the code.
- Docs and commit messages in English, no emoji; commits are requirement-scoped (code, tests, and docs together) with explicit `git add` paths; no commits unless the author asks.
- Files are strictly per-contributor disjoint; parallel worktrees share one `CARGO_TARGET_DIR`.
