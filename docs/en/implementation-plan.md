# DreamScript implementation plan

Status: planning record. Plans DreamScript from empty repository to 1.0 in Rust (edition 2024), subordinate to `docs/en/design-decisions.md` and cited by decision ID; when the two conflict the record wins and this plan is fixed. The milestone order refines the record's staging order: M1 inserts the front end ahead of the checker, and M2-M9 follow the recorded staging one to one.

Companion documents: `docs/en/status.md` (what runs today and each milestone's boundary), `docs/en/todo.md` (the ranked open-work queue), `docs/en/pitfalls.md` (invariants and lessons), `docs/en/memory-arc.md` (the memory execution contract). This file carries scope, structure, and methodology; it never restates landed history.

## 1. Ground rules
1. Each staging step lands on the previous one with no language-change rewrites.
2. The slot-overlay call ABI [H2] and the fixed-width mmap-able dsmod format [BL-7, G3.5] are locked first; everything downstream compiles against them.
3. The two-slot hot-reload module runtime exists from day one: data structures and qualified cross-module calls at M0, the `ds dev --hot` UX at M7 [H6 item 5].
4. Compiler-as-query-library: every consumer (CLI, LSP, REPL, hot reload, formatter) goes through the same memoized queries (parse, block_index, resolve, type_of, effects_of, linearity_check, lower), invalidated at `##` separator-block grain plus package grain [H6 item 1].
5. Invariants in every milestone: no import-time execution [G3, entry-file script-mode statements excepted]; guaranteed self-and-mutual TCO in every tier [H2]; dispatch correctness never depends on cache state [H3]; no `any` fallback — inference failure is a compile error [B1].
6. Explainability budget: any addition must re-fit the short tutorial; the deferred-additive backlog stays out until the core is stable.

Rust engineering conventions: edition 2024; `cargo fmt` and clippy clean before every commit; tests via cargo-nextest; release profile `codegen-units = 1` with `lto = "thin"` (hot paths cross crates); one private `CARGO_TARGET_DIR` per parallel worktree (a shared dir overwrites same-package artifacts across lanes); benchmarks under an exclusive `flock`, compiles and tests under shared locks.

## 2. Workspace layout
One Cargo workspace at the repository root; a crate appears in the milestone that first fills it (no empty shells).

| Crate | Owns | Appears |
|---|---|---|
| `ds-diag` | Spans, error-code registry, diagnostic rendering, `--error-format=json` | M0 |
| `ds-abi` | Frame layout, slot-overlay call convention, CALLT semantics, entry descriptors | M0 |
| `ds-mod` | dsmod reader/writer, mmap loader, blake3 hashing, cache paths | M0 |
| `ds-query` | Memoized queries, block/package-grain invalidation | M1, grows every milestone |
| `ds-parse` | Lexer, pest grammar, lossless AST, `##` separator blocks | M1 |
| `ds-fmt` | Block-aware zero-option formatter; later LSP on-type formatting | M1 |
| `ds-check` | Set algebra, disjointness, refinement, affine/DI, trait coherence | M2 |
| `ds-lower` | Frame layouts and uop bytecode; dup/drop placement | M3 |
| `ds-vm` | Tier-1 interpreter, fibers, nurseries, two-slot module table, inline caches, natives | M0 skeleton, M3 real, M5 fibers |
| `ds-rt` | Arenas, precise non-atomic RC, reuse threading, object layout (the unsafe core) | M4 |
| `ds-meta` | Derives, comptime evaluator, static reflection | M6 |
| `ds-pkg` | Manifests, MVS resolution, content-addressed store, script mode | M7 |
| `ds-ffi` | `extern` declarations, `ds bind`, embedding API | M7 |
| `ds-jit` | SSA lowering, copy-and-patch stencils, Cranelift/wasm | M8, SSA may start M7 |
| `ds-pins` | gungraun instruction pins and their pin-bake gate over `ds-vm` | M9 |
| `ds-cli` | The `ds` binary (clap): run, build, info, fmt, dev, save, bind, link | grows from M3 |
| `ds-lsp` | LSP server over `ds-query` | M7 |

Supporting directories: `conformance/` (seed corpus with stage expectations and golden outputs) from M1; `fuzz/`; `benchmarks/`. Consumption runs ds-parse -> ds-query -> ds-check -> ds-lower -> ds-vm -> ds-jit, with ds-abi beneath the lowerer, VM and JIT, ds-mod beneath the VM loader and the package store, and ds-diag beneath everything.

## 3. Dependency policy
House-preferred crates plus domain needs; new dependencies enter at the latest version via `cargo add`.

- `pest` + `pest_derive` — the parser. If the arrow-heavy grammar fights PEG in prototyping, escalate to hand-written recursive descent behind the same `parse` query, then to recorded fallback F1(b).
- `blake3` for source and package hashing [G3.5, H6] with `hex-simd` rendering; `memmap2` for the dsmod mmap path [G3.5]; `mimalloc` as the allocator fallback under the reuse path [C4]; `tempfile` for atomic package-store writes.
- `clap` (features = derive) for the `ds` CLI; `thiserror` for library errors, with anyhow in the binary only.
- The registry's remote client shells out to the system `git` over https (`file://` for local testing) rather than vendoring one — an unvetted git dependency (git2/libgit2) queues on the author's policy like `notify` does.
- `cranelift-codegen` and `wasm-encoder` for tier 3; `crossfire` (never crossbeam-channel) if channels appear; `youpipe` as the measured fork-join scheduler candidate.

## 4. Milestones
Each milestone states its intent, the items still open in it, and its acceptance criteria; the milestone order is the schedule. Landed work and its boundary live in `docs/en/status.md`, the ranked residual items in `docs/en/todo.md`.

**M0 — Contracts and bootstrap.** Goal: the ds-abi frame layout and call ABI [H2, BL-6] and the fixed-width dsmod format [BL-7, G3.5] locked, plus ds-diag and the ds-vm skeleton with its two-slot module table and qualified cross-module calls. Acceptance: mutual `even`/`odd` at depth 10^7 on a constant stack, dsmod write-then-read byte-stable, a corrupted file failing the hash check with a structured diagnostic.

**M1 — Front end.** Goal: lexer and parser for the full surface [F1, F2, F6, B7] with structured typed interpolation holes, a trivia-attached lossless AST, separator blocks [G3], the parse and block-index queries with syntax errors as data (broken files never poison sibling blocks), the zero-option block-aware formatter [H6 item 2], and the `conformance/` seed corpus with stage expectations. Acceptance: a parser fuzz campaign (arbitrary bytes through the public parse API) with zero panics, plus a nextest mirror in CI.

**M2 — Checker core.** Goal: the whole static story as a head-less library — the B1 set algebra with emptiness/disjointness, B2 bidirectional `type_of` with flow refinement and `?` containment [DS2008], the B6 instance environment, C1/C5 affine checking with golden two-span diagnostics, E1 effect rows, the B7/B8/B9 tables, package-wide resolution in UFCS order [F3], and `analyze()` as the linearity/lowering seam. Acceptance: the negative suite renders golden diagnostics (overlap witness, moved-here/used-again pairs, set-difference exhaustiveness, inference failure without any `any` fallback).

**M3 — Lowering and tier-1 VM.** Goal: lower the checked subset to static frame layouts and uops and execute it on tier 1 (decode-on-dispatch, unboxed slots, frame-reuse TCO with argument slide, inline caches with full-lookup fallback [H3]), with `ds run`/`ds build` end to end and the source hash recorded. Acceptance: every file in `examples/` runs green; the lowered subset boundary in `docs/en/status.md` matches the corpus; diagnostics stay two-span goldens.

**M4 — Memory.** Goal: the Perceus/arena runtime with deterministic death — per-frame bump arenas, precise non-atomic RC with reuse threading [C4, H4], `deinit` exactness on every path [C5], and the shippable `rc==1`/borrow-counter layer. A root frame's native tail abandon leak is accepted. Acceptance: linearity_modes_rc.ds runs with deinit observed on success, early-return, and panic paths (via test-only observers); free order is deterministic across repeated runs; arena-residency accounting reports 100% of non-escaping temporaries as register/arena-resident.

**M5 — Concurrency.** Goal: fibers and nurseries over one VM (spawn one allocation, switch a pointer swap, per-fiber TCO intact), nursery scope-exit join with first-failure sibling cancellation [E2], evidence-dispatched internal effects [E1], cancellation only at the preemption points [E5], the panic path [E4], and fork-join over the sendability substrate [E3]. Acceptance: effects_nursery.ds conformance; cancellation of a many-spawn nursery takes effect only at safe points; par combinators replay identically across runs; a deep tail-looping fiber runs unbounded without segment growth.

**M6 — Derives, comptime, reflection.** Goal: metaprogramming without macros — the derive families, comptime evaluation of ordinary code in a world-fenced context, and static reflection over tags-as-data with no runtime type metadata [G1, G2]. Acceptance: generics_coherent_traits.ds runs including `schema(comptime T)`; user derives cover user structs; the comptime fence rejects world-touching calls as compile errors.

**M7 — Modules, packages, FFI, tooling.** Goal: the ecosystem layer and the tooling commitments — visibility and interfaces [G3, G4], loading [G3.5], packages with the content-addressed store and the optional `ds link` linker [H6], a metadata-only registry [H6], FFI [H5], platform providers [H5.5], the REPL [H6 item 4], hot reload [H6 item 5], and an LSP over `ds-query`. Acceptance: a multi-package project (app plus local lib plus `@std`) builds, runs, and hot-reloads; modules_hash_core.ds conformance; `ds save` round-trips a script into a package; cold startup under 10ms on the mmap hit path.

The milestone's open work — the loading remainder, FFI, the platform providers, and the tooling residuals — is ranked in `docs/en/todo.md`; this plan keeps the scope and the acceptance criteria only.

**M8 — Tier 2 and tier 3.** Goal: JIT tiers over the one shared lowering. Open: bytecode-to-typed-SSA lowering shared by all tiers [H1]; copy-and-patch tier 2 with in-place J-variant tier transitions and loop-body OSR; superinstructions from measured opcode frequencies [BL-4]; aarch64 stencils after x86-64; tier 3 through Cranelift CLIF and wasm 3.0 `return_call`; cross-tier differential testing. Acceptance: performance gates against C baselines — tier 1 within 3-10x, tier 2 within 1.5-3x, tier 3 within 1-2x native and 1.5-2.5x wasm; short scripts (under 0.2s) never pay tiering; startup unaffected by JIT machinery.

**M9 — Hardening and 1.0.** Goal: close the rapid-break window [A3]. Open: extended fuzz campaigns with shrinking regression seeds; performance regression gates on instruction counts and wall clock with a per-commit startup bench; a full docs pass with the tutorial as the explainability-budget proof; triage of the deferred-additive backlog against the remaining sugar headroom, adopted items shipping pure additive; the post-1.0 pure-optimization queue (borrow inference, full reuse pairing, TRMC, fusion upgrades, `T | none` niche packing, rope); version stamping and semver discipline from 1.0 on. Backlog beyond M8, not a restaging: a wasm runner, major non-micro performance rounds once the tiers exist, docs completion plus a web playground, and a cross-language example suite.

## 5. Test strategy
- Conformance corpus: the seed corpus at `conformance/` plus every bug regression, each with stage expectations and golden output; from M8 every file runs on every tier.
- Negative suite: golden-rendered diagnostics — overlap witnesses, moved-here/used-again pairs, set-difference exhaustiveness, inference failures.
- Differential: across tiers and across optimization toggles (reuse and fusion on/off) — identical observable behavior; par combinators replay deterministically.
- Fuzz (cargo-fuzz): ds-mod reader, lexer/parser, checker generator, VM differential, query-engine edit sequences (seeded nextest mirrors in `crates/ds-query/tests/invariants.rs` and CI); overflow checks and the diagnostic layer stay on in fuzz and CI builds.
- Unit tests are few but sharp: the emptiness/disjointness procedure, refinement rules, the ABI argument-slide edge cases, and arena/RC invariants get dense coverage; trivial plumbing gets none.

## 6. Performance methodology
- Every performance change lands with a before/after bench; release profile `codegen-units = 1` with `lto = "thin"`.
- On Linux: hotpath instrumentation and perf for hotspot discovery; gungraun for deterministic instruction-count regressions; cargo-show-asm for the VM dispatch loop and stencils.
- Fixed suites: startup (cold script, mmap hit, `--exe`), corpus throughput against C baselines, allocation and arena-residency counts, fiber switch and spawn cost, par-combinator scaling. The kernel A/B runner is `benchmarks/ab.sh` with `benchmarks/run.sh check` as the regression gate, and the fork-join spawn-plus-join bench is `crates/ds-vm/benches/fork_join.rs`.
- When wall time and instruction counts disagree, re-measure and trust wall time.

## 7. Rust-specific design notes
- Dispatch [BL-3]: stable Rust has neither computed goto nor stable `musttail`; tier 1 starts as a dense `match` loop (LLVM lowers it to a jump table). If profiling shows dispatch cost, escalate behind a cfg (nightly tail-call threading or build-script-generated asm dispatch), a build-portability choice.
- Unsafe code is confined to `ds-rt` (arenas, RC, object layout) and `ds-jit` (stencil patching); both carry the debug-assertion diagnostic layer, and CI runs with it enabled.
- Fibers are interpreter data: no native stack switching, no async color; fork-join worker threads only receive ownership transfers, which keeps non-atomic RC sound [C4, E3].
- Stencil builds need clang/LLVM at build time; the CI matrix is windows-msvc plus linux; dsmod target classes make cross-platform artifacts first-class. The `ds` binary is the only crate configuring spdlog-rs; libraries log through the `log` facade; no anyhow in library crates.

## 8. Risks and recorded fallbacks
| Risk | Trigger | Fallback |
|---|---|---|
| Disjointness machinery slips schedule [B1] | emptiness checker not converging in M2 | B1(b) nominal-only core; error handling degrades to match-based |
| Arrow-assignment grammar fights the parser [F1] | pest prototyping dead-ends in M1 | recursive descent behind the same `parse` query, then F1(b) Gleam/Roc brace headlines |
| Per-arch stencil toolchain unacceptable [H1] | windows-msvc stencil build unstable in M8 | H1(b) single method-JIT tier |
| Affine friction worse than expected [C1] | real corpus code drowning in move errors by M5 | C4(b) generational references |
| Sub-10ms startup missed [G3.5] | cold-run bench above target in M7 | profile the mmap path, strip debug spans, lazy section loading |
| RC/arena soundness bugs [C4] | any use-after-free or double-free in fuzzing | the diagnostic layer stays shippable behind a flag |

## 9. Scope guards
Permanently out or out of 1.0: no import-time execution; no runtime type metadata tables or type objects; no macros at any level; no user-defined operators, Deref coercions, or convention-based privacy; no hidden classes, field inline caches, speculation, deopt frames, or side traces in any tier; no GC in v0 beyond the committed two-hook interface; no HKT or GATs in 1.0; no `?.`, elvis, `!!`, smart casts, receiver-lambda DSLs, primary constructors, reified type parameters, or backtick identifiers [F7]; no public generator syntax in 1.0 [D3]; no `|>`, anonymous record literals, `@` binders, labeled break/continue, or `it` shorthand unless triaged in at M9; no `or` extraction operator, truthiness coercion in boolean contexts, chained comparisons, comprehension syntax, subscript slicing sugar, vararg/spread packing, or word-form operators beyond `in`/`is` — permanently [E4, F8].

## 10. Working discipline
- Design changes go to `docs/en/design-decisions.md` first, then this plan follows; `docs/en/status.md`, `docs/en/todo.md`, and `docs/en/pitfalls.md` carry what runs and each boundary, the ranked open work, and the invariants. No file restates another.
- Landed work with no residual discussion is deleted from the docs and lives in git history; every commit updates the progress, pitfall, and next-step files alongside the code.
- Docs and commit messages in English, no emoji; commits are requirement-scoped (code, tests, and docs together) with explicit `git add` paths, and no commits unless the author asks. Files are strictly per-contributor disjoint; parallel worktrees keep private target dirs (a shared one overwrites same-package artifacts across lanes).
