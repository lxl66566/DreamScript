# DreamScript implementation plan

Plans DreamScript from empty repository to 1.0 in Rust (edition 2024), subordinate to `docs/en/design-decisions.md`.

## 1. Ground rules
1. Each staging step lands on the previous one with no language-change rewrites.
2. The slot-overlay call ABI [H2] and the fixed-width mmap-able dsmod format [BL-7, G3.5] lock first; everything downstream compiles against them.
3. Compiler-as-query-library: every consumer reads the same memoized queries, invalidated at separator-block and package grain; the two-slot hot-reload runtime exists from day one [H6 item 1, item 5].
4. Invariants everywhere: no import-time execution [G3, entry-file script-mode statements excepted]; guaranteed self-and-mutual TCO [H2]; dispatch correctness never depends on cache state [H3]; no `any` fallback — inference failure is a compile error [B1].

Rust engineering: edition 2024; fmt and clippy clean per commit; nextest; `codegen-units = 1` with `lto = "thin"`. Unsafe code lives in `ds-rt` and `ds-jit` behind the debug-assertion layer CI enables; stencil builds need clang/LLVM, and CI is windows-msvc plus linux. Fibers are interpreter data: no native stack switching, no async color; fork-join workers receive ownership transfers only, keeping non-atomic RC sound [C4, E3]. The `ds` binary is the only crate configuring spdlog-rs; libraries log through the `log` facade.

## 2. Workspace layout
One Cargo workspace at the repository root; a crate appears in the milestone that first fills it (no empty shells).

| Crate | Owns | Appears |
|---|---|---|
| `ds-diag` | Spans, error codes, diagnostics, `--error-format=json` | M0 |
| `ds-abi` | Frame layout, call ABI, CALLT, entry descriptors | M0 |
| `ds-mod` | dsmod reader/writer, mmap loader, blake3, cache paths | M0 |
| `ds-query` | Memoized queries, block/package invalidation | M1, grows every milestone |
| `ds-stdlib` | The ambient stdlib sources, declared rows, and embedded serve artifacts | M3 |
| `ds-parse` | Lexer, pest grammar, lossless AST, separator blocks | M1 |
| `ds-fmt` | Block-aware zero-option formatter, on-type formatting | M1 |
| `ds-corpus-path` | Run-time corpus resolution for the workspace's test binaries | M1 |
| `ds-check` | Set algebra, disjointness, refinement, affine, trait coherence | M2 |
| `ds-lower` | Frame layouts, uop bytecode, dup/drop placement | M3 |
| `ds-vm` | Tier-1 interpreter, fibers, nurseries, module table, inline caches, natives | M0 skeleton, M3 real, M5 fibers |
| `ds-rt` | Arenas, non-atomic RC, reuse threading, object layout | M4 |
| `ds-meta` | Derives, comptime evaluator, static reflection | M6 |
| `ds-dsi` | The `.dsi` checked-prefix artifact: reader, writer, blob serve | M7 |
| `ds-pkg` | Manifests, MVS resolution, content-addressed store, script mode | M7 |
| `ds-ffi` | `extern` declarations, `ds bind`, embedding API | M7 |
| `ds-jit` | SSA lowering, copy-and-patch stencils, Cranelift/wasm | M8, SSA may start M7 |
| `ds-pins` | gungraun instruction pins and their pin-bake gate | M9 |
| `ds-cli` | The `ds` binary: run, build, check, info, fmt, save, bind, dev, repl, lsp | grows from M3 |
| `ds-lsp` | LSP server over `ds-query` | M7 |

Supporting directories: `conformance/` (seed corpus, stage expectations, goldens) from M1, `fuzz/`, `benchmarks/`. Consumption runs ds-parse -> ds-query -> ds-check -> ds-lower -> ds-vm -> ds-jit, ds-abi beneath the lowerer, VM and JIT, ds-mod beneath the VM loader and package store, ds-diag beneath everything.

## 3. Dependency policy
House-preferred crates plus domain needs; new dependencies enter at the latest version via `cargo add`.

- `pest` + `pest_derive` for the parser; if the arrow-heavy grammar fights PEG, escalate to hand-written recursive descent behind the same `parse` query, then to fallback F1b.
- `blake3` for source and package hashing [G3.5, H6] with `hex-simd`; `memmap2` for the dsmod mmap path; `mimalloc` under the reuse path [C4]; `tempfile` for atomic store writes.
- `clap` (derive) for the CLI; `thiserror` for library errors, anyhow in the binary only.
- The registry's remote client shells out to the system `git` over https (`file://` locally); an unvetted git dependency queues on the author's policy like `notify`.
- `cranelift-codegen` and `wasm-encoder` for tier 3; `crossfire` (never crossbeam-channel) if channels appear; `youpipe` as the measured fork-join scheduler candidate.

## 4. Milestones
The milestone order is the schedule; each statement is its scope and acceptance criteria.

**M0 — Contracts and bootstrap.** Scope: the frame layout and call ABI [H2, BL-6], the dsmod format [BL-7, G3.5], ds-diag, and the ds-vm skeleton. Acceptance: mutual `even`/`odd` at depth 10^7 on a constant stack, dsmod write-then-read byte-stable, a corrupted file failing the hash check with a structured diagnostic.

**M1 — Front end.** Scope: lexer and parser for the full surface [F1, F2, F6, B7], separator blocks [G3], the parse and block-index queries, the formatter [H6 item 2], and the `conformance/` corpus. Acceptance: a parser fuzz campaign (arbitrary bytes through the public parse API) with zero panics.

**M2 — Checker core.** Scope: the whole static story as a head-less library [B1, B2, B6, C1, C5, E1, B7-B9, F3, DS2008] behind `analyze()`. Acceptance: the negative suite renders golden two-span diagnostics.

**M3 — Lowering and tier-1 VM.** Scope: lower the checked subset to static frame layouts and uops on tier 1 [H3], with `ds run`/`ds build` end to end. Acceptance: every `examples/` file runs green, and the lowered subset in status matches the corpus.

**M4 — Memory.** Scope: the Perceus/arena runtime with deterministic death and the shippable `rc==1`/borrow-counter layer [C4, C5, H4]; a root frame's native tail abandon leak is accepted. Acceptance: linearity_modes_rc.ds runs with deinit observed on success, early-return, and panic paths; free order is deterministic; arena residency reports 100% of non-escaping temporaries as register/arena-resident.

**M5 — Concurrency.** Scope: fibers, nurseries, the panic path, and fork-join over the sendability substrate [E1-E5]. Acceptance: effects_nursery.ds conformance; a many-spawn nursery's cancellation takes effect only at safe points; par combinators replay identically; a deep tail-looping fiber runs unbounded without segment growth.

**M6 — Derives, comptime, reflection.** Scope: metaprogramming without macros — derives, world-fenced comptime evaluation, and static reflection over tags-as-data with no runtime type metadata [G1, G2]. Acceptance: generics_coherent_traits.ds runs including `schema(comptime T)`; user derives cover user structs; the comptime fence rejects world-touching calls as compile errors.

**M7 — Modules, packages, FFI, tooling.** Scope: visibility [G3, G4], loading [G3.5], packages with the content-addressed store and `ds link`, a metadata-only registry, FFI [H5], platform providers [H5.5], the REPL, hot reload, and an LSP [H6]. Acceptance: a multi-package project (app, lib, `@std`) builds, runs, and hot-reloads; modules_hash_core.ds conformance; `ds save` round-trips a script into a package; cold startup under 10ms on the mmap hit path.

**M8 — Tier 2 and tier 3.** Scope: JIT tiers over the one shared lowering — SSA shared by all tiers, copy-and-patch tier 2 with J-variant transitions and loop-body OSR, superinstructions [BL-4], aarch64 stencils, tier 3 through Cranelift and wasm [H1]. Acceptance: tier 1 within 3-10x the C baselines, tier 2 within 1.5-3x, tier 3 within 1-2x native and 1.5-2.5x wasm; scripts under 0.2s never pay tiering; startup unaffected by JIT machinery.

**M9 — Hardening and 1.0.** Scope: close the rapid-break window [A3] — fuzz campaigns with shrinking regression seeds, performance regression gates on instruction counts and wall clock, a docs pass with the tutorial as the explainability-budget proof, deferred-additive triage, the post-1.0 optimization queue (borrow inference, reuse pairing, TRMC, niche packing, rope), and semver from 1.0 on. Beyond M8, not a restaging: a wasm runner, major non-micro performance rounds, docs with a web playground, and a cross-language example suite.

## 5. Test strategy
- Conformance corpus at `conformance/` plus every bug regression, each with stage expectations and goldens; from M8 every file runs on every tier.
- Negative suite: golden two-span diagnostics — overlap witnesses, moved/used pairs, set-difference exhaustiveness, inference failures.
- Differential across tiers and optimization toggles: identical observable behavior; par combinators replay deterministically.
- Fuzz (cargo-fuzz): ds-mod reader, lexer/parser, checker generator, VM differential, and query-engine edit sequences, with seeded nextest mirrors in CI; overflow and diagnostic layers stay on there.
- Unit tests are few but sharp — emptiness/disjointness, refinement, ABI argument-slide edges, arena/RC invariants; trivial plumbing gets none.

## 6. Performance methodology
- Every performance change lands with a before/after bench. On Linux: hotpath and perf for hotspots, gungraun for instruction-count regressions, cargo-show-asm for dispatch and stencils.
- Fixed suites: startup (cold script, mmap hit, `--exe`), corpus throughput against C baselines, allocation and arena-residency counts, fiber switch and spawn cost, par scaling; the kernel A/B runner is `benchmarks/ab.sh`, the regression gate `benchmarks/run.sh check`, the fork-join bench `crates/ds-vm/benches/fork_join.rs`.
- When wall time and instruction counts disagree, re-measure and trust wall time.

## 7. Risks and recorded fallbacks
| Risk | Trigger | Fallback |
|---|---|---|
| Disjointness machinery slips schedule [B1] | emptiness checker not converging in M2 | B1b nominal-only core |
| Arrow-assignment grammar fights the parser [F1] | pest prototyping dead-ends in M1 | recursive descent behind `parse`, then F1b Gleam/Roc brace headlines |
| Per-arch stencil toolchain unacceptable [H1] | windows-msvc stencil build unstable in M8 | H1b single method-JIT tier |
| Affine friction worse than expected [C1] | corpus code drowning in move errors by M5 | C4b generational references |
| Sub-10ms startup missed [G3.5] | cold-run bench above target in M7 | profile the mmap path, strip debug spans, lazy section loading |
| RC/arena soundness bugs [C4] | any use-after-free or double-free in fuzzing | the diagnostic layer stays shippable behind a flag |

## 8. Scope guards
Permanently out or out of 1.0: no import-time execution; no runtime type metadata tables or type objects; no macros at any level; no user-defined operators, Deref coercions, or convention-based privacy; no hidden classes, field inline caches, speculation, deopt frames, or side traces in any tier; no GC in v0 beyond the committed two-hook interface; no HKT or GATs in 1.0; no `?.`, elvis, `!!`, smart casts, receiver-lambda DSLs, primary constructors, reified type parameters, or backtick identifiers [F7]; no public generator syntax in 1.0 [D3]; no `|>`, anonymous record literals, `@` binders, labeled break/continue, or `it` shorthand unless triaged in at M9; no `or` extraction operator, truthiness coercion in boolean contexts, chained comparisons, comprehension syntax, subscript slicing sugar, vararg/spread packing, or word-form operators beyond `in`/`is` — permanently [E4, F8].
