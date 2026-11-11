# DreamScript implementation plan

Status: planning record, written 2026-11-09. This document plans the full implementation of DreamScript from empty repository to 1.0 in Rust (edition 2024). It is derived from and subordinate to `docs/en/design-decisions.md`, cited throughout by decision ID ([B1], [H2], ...); when this plan and that record conflict, the record wins and this plan is fixed. The milestone order refines the staging order of the record's section 10: M0 locks the two load-bearing contracts, M1 inserts the front end ahead of the checker (the record's step 2 presumes a parser), and M2-M9 follow the recorded staging one to one. The milestone status table at the end is the living tracking state of this plan.

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

Supporting directories: `conformance/` (the seed corpus ported to the decided surface, with per-stage expectations and golden outputs) from M1; `fuzz/` (cargo-fuzz targets) from M0.

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

### M0 — Contracts and bootstrap (done 2026-11-09)

Locked the two load-bearing contracts as crate docs plus tests before any compiler exists: ds-abi (typed frame layout [BL-6], the slot-overlay convention with owner-relative word mapping, CALLT semantics, two-word entry descriptors, tail-chain metadata, the BL-5 uop set with canonical 32-bit encodings), ds-mod per [G3.5] (strict-canonical reader, byte-stable writer, blake3-128 checked-hash validation, mmap loader, dormant two-hook collector fields, fuzz target), ds-diag, and the ds-vm skeleton with the two-slot module table and fully-qualified cross-module calls [H6 item 5] executing hand-assembled fixtures — mutual `even`/`odd` at depth 10^7 on a constant stack, dsmod write-then-read byte-stable, a corrupted file failing the hash check with a structured diagnostic.

### M1 — Front end (parse, query wiring, corpus, ds-fmt landed)

Lexer and pest grammar for the full surface [F1, F2, F6, B7] with structured interpolation holes (each a typed expression, not lexer concatenation), trivia-attached lossless AST, separator blocks (landed as `///|`, retokenizing to `##` per the G3 ruling in a queued round), order-independent top-level definitions [G3]; ds-query runs the real parse and block-index queries with syntax errors as data (broken files never poison sibling blocks); ds-fmt is the block-aware zero-option formatter over the parse query [H6 item 2]; the seed corpus lives in `conformance/` with per-file porting notes and stage expectations (parse now, typecheck at M2, execute at M3-M6). The parser fuzz acceptance gate closed 2026-11-10: the fuzz_parse target (arbitrary bytes through the public parse API) ran a 30-worker dict-driven campaign of 617,631 executions with zero panics, and a nextest mirror (random bytes plus corpus-seed mutations) runs in CI's test path.

### M2 — Checker core (done 2026-11-09)

The whole static story as a head-less library: the B1 set-algebra core with the emptiness/disjointness procedure (canonical atom-set solving, law tests against a bitset oracle), B2 bidirectional type_of with the flow-refinement overlay and `?` containment [DS2008], B6 instance environment (union-head decomposition, witness-carrying overlap errors, condition-cycle rejection), C1/C5 affine checking (golden DS3001/3002/3004 two-span diagnostics; Linearity exposes moves/drops/last_uses for the lowerer), E1 effect rows (EffectRow bitset, least-fixpoint effects_of consumed at M5/M7), the B7/B8/B9 numeric/string/equality tables, package-wide resolution with the UFCS order [F3], and `analyze() -> Analyzed` as the linearity/lowering seam beside the query path. The negative suite renders golden diagnostics (overlap witness, moved-here/used-again pairs, set-difference exhaustiveness, inference failure without any `any` fallback).

### M3 — Lowering and tier-1 VM (core done 2026-11-09; boundary shrunk round 4)

ds-lower lowers the checked subset to static frame layouts and uops (overlay staging with per-word Raw widening, `?`/`.err` to TAG_TEST + Jmpif + RETSPAN respan exits, naive RC placeholders until the M4 pass); ds-vm tier 1 executes from encoded programs (decode-on-dispatch, unboxed slots, tag+payload unions across consecutive slots, frame-reuse TCO with argument slide, tag-keyed 1-2 entry inline caches with full-lookup fallback [H3]); `ds run <file.ds>` and `ds build <file.ds>` compile source end to end with the source hash recorded; natives split per [H5.5] into `builtin` (pure core, every VM) and `std` (the default platform surface, CLI-installed). The live lowered-subset boundary lives in the status table; the ranked open items live in the queue at the end of this file.

### M4 — Memory

Goal: the Perceus/arena runtime with deterministic death.

Work items:

- `ds-rt`: per-frame bump arenas (mark at entry, bump-allocate, restore on every pop path; segment growth by chunk append so captured frame addresses stay stable; per-fiber marks so fiber switching needs no arena action) [C4, H4].
- Precise non-atomic RC: compiler-inserted dup/drop via the Perceus placement pass in `ds-lower` (replacing M3 placeholders); each drop threads to the next allocation as a reuse candidate (`alloc_at`), mimalloc fallback [C4].
- Diagnostic layer shipping first [C1 staging]: runtime `rc==1` uniqueness checks plus an Inko-style borrow counter, behind a flag, kept shippable — it is the debugging instrument for the whole memory story.
- `deinit` execution [C5]: static dispatch (concrete type known), exactly once at the unique instance's death, on every path including early return, panic, and cancellation teardown; explicit `drop` as a debug/FFI tool.
- Cycles rejected at the type level for v0; the two-hook collector interface stays header-committed and dormant [C4].

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
- Hot reload [H6 item 5]: `ds dev --hot` recompiles changed blocks, swaps the second slot, retires old-version frames at their next cross-module call, type-checks heap values against new defs — now exercising the M0 structures end to end.
- LSP over `ds-query`: diagnostics, hover from `.dsi`, on-type formatting from `ds-fmt`.

Acceptance: a multi-package project (app plus local lib plus `@std`) builds, runs, and hot-reloads; modules_hash_core.ds conformance; `ds save` round-trips a script into a package; cold startup under 10ms on the mmap hit path.

### M8 — Tier 2 and tier 3

Goal: JIT tiers over the one shared lowering.

Work items:

- bytecode-to-typed-SSA lowering shared by all JIT tiers; may start during M7 once tier 1 is stable — it only consumes uops [H1].
- Copy-and-patch tier 2 [H1]: build-time clang/LLVM compiles a master template including the VM headers into one x86-64 fragment per uop, holes for frame-slot offsets and constants patched as u16/u64; tier transitions are in-place J-variant opcode rewrites (one dispatch); OSR reduces to compiling the loop body, no frame transformation machinery; the two-word entry descriptors flip on tier-up (the same invalidation story as hot reload); no guards on the data path — the only exits are debugger breakpoints, the per-entry stack-limit compare, effect-handler entry, and B7's cold overflow edge.
- Superinstructions [BL-4]: the assembler composes fusions (compare-and-branch, load-op, dense-tag match jump tables) from measured opcode frequencies, never hand-picked. Measured on the current tier 1: the pure-loop kernels still spend ~190ns/iteration on decode-on-dispatch after the inline caches, so fusion and the tier-2 dispatch table are the next linear-loop wins.
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

### Near-term scheduling (advisory)

Advisory queue only — not a re-staging; section 10 of the record and the milestone table above stay authoritative.

- Then, in order: the ranked queue at the end of this file (the generic-impl lowerer seam first); the M5 in-language nursery/spawn surface; M6 derives/comptime; M7 (platforms + registry per the H6 ruling); M8.

#### Author scheduling backlog (advisory, post-M8, not a restaging)

Recorded as backlog beyond M8, not a restaging: a wasm runner (the tier-3 wasm execution surface plus a wasm-platform runner); JIT tiers — already M8, see that milestone; major non-micro performance optimization rounds once the tiers exist; docs completion plus a web playground; a cross-language example suite and benchmarks against peer scripting languages (python, lua, node, bun).

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
- When actual wall time and instruction counts disagree, re-measure and trust wall time.

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
| RC/arena soundness bugs [C4] | any use-after-free or double-free in fuzzing | the diagnostic layer (rc==1 checks, borrow counter) is the debugging instrument; keep it shippable behind a flag |

## 9. Scope guards

Permanently out or out of 1.0, enforced at review: no import-time execution; no runtime type metadata tables or type objects; no macros at any level; no user-defined operators, Deref coercions, or convention-based privacy; no hidden classes, field inline caches, speculation, deopt frames, or side traces in any tier; no GC in v0 beyond the committed two-hook interface; no HKT or GATs in 1.0; no `?.`, elvis, `!!`, smart casts, receiver-lambda DSLs, primary constructors, reified type parameters, or backtick identifiers [F7]; no public generator syntax in 1.0 [D3]; no `|>`, anonymous record literals, `@` binders, labeled break/continue, or `it` shorthand unless triaged in at M9 [section 11 of the record]; no `or` extraction operator, truthiness coercion in boolean contexts, chained comparisons, comprehension syntax, subscript slicing sugar, vararg/spread packing, or word-form operators beyond `in`/`is` — permanently [E4, F8].

## 10. Working discipline

- The milestone status table is updated as work lands; design changes go to `docs/en/design-decisions.md` first, then this plan follows — this file never becomes a second design record.
- Docs and commit messages in English, no emoji; commits are requirement-scoped (code, tests, and docs together) with explicit `git add` paths; no commits unless the author asks.
- Files are strictly per-contributor disjoint; parallel worktrees share one `CARGO_TARGET_DIR`.

## Milestone status

| Milestone | Scope | Status |
|---|---|---|
| M0 | Contracts and bootstrap (ds-abi, ds-mod, ds-diag, ds-vm skeleton) | done 2026-11-09 |
| M1 | Front end (ds-parse, ds-query skeleton, ds-fmt, conformance corpus) | in progress (parse, query wiring, corpus, ds-fmt landed) |
| M2 | Checker core (ds-check, diagnostics, negative suite) | done 2026-11-09 (B1 core + tuples B3, B6 instances, resolution, B2 type_of with refinement, C1/C5 linearity with golden DS3001/3002/3004, E1 effect rows; effects consumed at M5/M7) |
| M3 | Lowering and tier-1 VM (ds-lower, ds-vm, `ds run`) | core done 2026-11-09; `ds run`/`ds build` compile and execute end to end. Lowers today: `var` locals with plain/compound assignment and `while` loops, monomorphic UFCS dot-calls, statement-position `return` (value and bare) mid-block, script entry (entry-file top-level statements fold into a synthesized `main`; library parse rejects them with DS1014), `.ok`/`.err`/`?` extraction over the success-typed ambient with fallible native spans re-spanned into the call-site unions (every example in examples/ runs green, files and mutation included), enum blocks with payload cases and call-form construction, list and map literals/constructors with `list_push`/`map_*`, for-in over int ranges and lists (map iteration refuses naming the ordered-entries native) with `in` membership over ranges and lists, anonymous tuples with destructuring, interpolation auto-str over the printable primitives, char and byte literals (byte words ride the integer slot encoding), non-capturing and capturing lambdas with named-fn values and callable-defaulted fn-typed parameters, and the container closes plus map/filter/reduce dispatching through CALLG. Impl member bodies lower under mangled identity with statically-solved trait call sites dispatching through CALLG on an instance-id evidence word (monomorphic impls run end to end), and declared `deinit` blocks execute at the value's unique death (scope exit, early return, `?`, overwrite, frame exit). Natives split per H5.5: `builtin` core (str_*, string ops) in every VM + `std` platform native module (console/file/net) host-installed, the CLI by default. Open boundary: generic-impl call sites (checked; refusing at lowering naming the runtime-evidence seam), inherent-impl dot-calls (the checker accepts; the lowerer names the UFCS alternative), mut/move parameter modes, block-argument sugar, named arguments (admitted for enum case constructors only), multi-atom uses without is/match splitting, multi-word collection elements (struct/tuple) and multi-word struct fields, value-level tuple tests and guarded multi-arm matches on non-union values, `var` destructuring, and the M5 nursery/spawn statements |
| M4 | Memory (ds-rt: arenas, Perceus RC, reuse, deinit) | done 2026-11-10 at the compiler level: ds-rt (bump arenas with chunk-append growth and free-list pooling, per-fiber arenas, restore guards; precise non-atomic RC, reuse threading, deinit dispatch, `diag` diagnostic layer) plus the ds-lower Perceus dup/drop placement pass (place.rs; ownership model over Ref-typed body words, alias-widened last-use deaths, call-staging dup/forward rules, fork/join per arm with loop-depth guard, a loop-claims stack that suppresses in-body deaths of loop-carried words, one emit_drop routing deinits); the RcOp Inc/Dec pair lives in reserved uop major 0xE beside `Uop`, and module code emits and executes the placed RcOps over counted Heap blocks (strings/lists) with the native retain contract explicit. Residual: host-abandoned words and the root frame's tail-native abandon leak by design; a non-root frame's native tail call lowers in place (CALL plus flush plus return); declared deinit blocks execute through the rc_bind hook with five honest refusals (closure-captured words, union payload fields, native/generic arguments, tail arguments, generic-struct deinit). Honesty note: ds-rt itself is an unconsumed parallel implementation kept as reference (no crate depends on it), the consumed runtime being ds-vm/src/object.rs — final disposition (adopt, freeze, or delete) is a pending author decision) |
| M5 | Concurrency (fibers, nursery, cancellation, fork-join) | substrate done 2026-11-10 (fiber pool with chunk-list segments and O(1) switch; nursery join with spawn-order determinism, first-failure cancellation observed exactly at backward jumps, boundary error unioning with per-child outcomes, teardown walks; bare detach routes to the live root nursery or the host supervisor); the in-language nursery/spawn surface and fork-join combinators wait on the checker/lowering rounds |
| M6 | Derives, comptime, reflection (ds-meta) | not started; CALLG dispatch runs through phase 3 (the ambient generic vocabulary on instance evidence — len/str, the list/map reads and counts, the container closes, map/filter/reduce over interpreter reentry — plus impl member bodies under mangled identity: statically-solved trait call sites dispatch on an instance-id evidence word and monomorphic impls run end to end); generic impl call sites resolve in the checker (bound satisfaction, member-wise union coverage, DS2018 signature agreement, DS2010 ambiguity) and refuse at lowering naming the runtime-evidence seam, so the lowerer half of generic impls — bodies in the plan plus runtime-resolved evidence through ds-mod's FuncRef instance registry — is the open planning item, ranked first in the queue below |
| M7 | Modules, packages, FFI, tooling (ds-pkg, ds-ffi, REPL, hot reload, LSP) | not started |
| M8 | Tier 2 copy-and-patch and tier 3 (ds-jit, SSA, differential tiers) | not started |
| M9 | Hardening and 1.0 (fuzz gates, perf gates, tutorial) | not started; the fuzz item has its checker generator leg (fuzz_check_gen plus the 240-seed nextest mirror in CI; the capped 8-worker ASan campaign ran 95,758 executions with no panics), and docs/en/tutorial.md exists early as the explainability-budget proof |

## Standing pitfalls

- Parallel-agent protocol: per-agent git worktrees plus branches, additive-only root Cargo.toml edits, one shared CARGO_TARGET_DIR, agents never run git in the main checkout, the coordinator is main's single writer; Cargo.lock conflicts on rebase resolve by delete plus `cargo check` regen; after any hand-resolved merge run `cargo clean -p <affected>` plus a fresh check before pushing — the shared target dir serves stale fingerprints and masked two merge breakages once, and a workspace test can likewise pick a parallel worktree's rlib (a `touch` of the affected crate's sources plus a rebuild refreshes the fingerprint without any clean); an orphaned test exe (interrupted or parallel nextest run) locks its own output file and fails relinking with LNK1104 — find the exact PID by image name and kill it by PID. The staleness has a deeper form: `cargo build -p <downstream>` recompiles downstream but not a touched upstream crate, and a stale rlib was once linked while cargo claimed Fresh (the `touch` remedy failed; `cargo clean -p <touched>` then building the touched crate directly and verifying its `Compiling` line is the reliable remedy before trusting any all-green suite). The shared `target/debug/ds` binary is also cross-worktree — parallel agents' builds clobber it, so run-probes pin a private copy (`cp target/debug/ds /tmp/<private>`).
- Extraction spellings: an un-annotated `.err` handler under no contextual expectation consumes the whole union ("`.err` leaves nothing" plus an empty-use cascade) — ported fallback sites annotate the handler parameter (`(e: net_err) => ...`, `(_: io_error) => 0`). The retired `or`'s contextual empty-intersection typing hid real width meetings: linearity_modes_rc.ds:39's u32 + int surfaces one honest DS2001 now (no conversion callable exists yet; the B7 message's `u32(x)` suggestion is unimplemented). `let x = e?` over a fully-contained union reports DS2002 by design — expected=None propagates every member so nothing binds; the honest spellings are `return e?` and the final expression.
- Nursery/fiber substrate facts: a child's pool slot can be recycled while the nursery's children list still holds the id — drivers re-check `fiber.nursery` before driving (outcomes are ledgered before recycling); a child yielding inside an infinite loop under a join drive hangs the driver by construction (substrate fact; the language surface must bound it); CALLT pc-reset is deliberately not a preemption point, so a self-tail-loop never observes cancellation.
- CI runs fmt on nightly only (rustfmt.toml unstable options; a harmless nightly rustfmt stderr quirk around single_line_if_else_max_width is left as-is); every crate must inherit workspace.package fields (clippy's cargo lint group fails on missing readme metadata); nightly clippy rejects `map(format!).collect::<String>()` and `push_str(&format!())` — build strings with plain `format!` composition; cargo doc belongs in the verification loop too, not just clippy/nextest — single-token bracket refs like `[C4]` in doc comments parse as broken intra-doc links (the tree still carries silent ones on private items, plus a few in ds-lower/ds-vm), so write parenthesized `(C4)` or slash forms.
- Round-closing protocol: corpus noise counts measured in a worktree go stale as parallel landings retire errors — re-measure every count on the merged main before pushing (the effects_nursery count dropped 7 to 4 when the list constructor landed under it). The host's GIT_EDITOR is nvim — interactive git (a bare `rebase --continue`, commit prompts) hangs the shell-proxy session; kill the nvim PID and rerun with `GIT_EDITOR=true`. Never run cargo fmt outside your worktree — a stray fmt in the main checkout rewrites tracked files and breaks merges. cargo nextest name filters match test names, not files — use `cargo test --test <file>` for a new test file. Union-member render order in diagnostics follows tag allocation order, not source spelling — pin goldens from live output. Multi-statement one-line blocks are parse errors; write block statements on separate lines.
- Native/platform seam: install natives into the registry before loader::install (pool FuncRefs validate at install time; `Vm::with_limit` installs only the builtin core; the CLI calls provide_platform on both run paths). Tag ids: module lowering allocates union ids upward from 0, the platform reserves the u8 top downward (IO_OK 255, IO_ERR 254, NET_ERR 253, PARSE_ERR 252, NONE 251 — the cross-module none member minted MAX-4, so the countdown reservation is 4 deep). The builtin and std native modules' index spaces have diverged (builtin runs 0-40: split 17, the list natives 20-23, the map block 28-31, the container closes 32-37, the combinators 38-40 — the monomorphic slots stay reserved with the bodies in the generic table — against std's net 20-23); per-module indices must be re-pinned when the M7 freeze happens. Fallible natives: over the success-typed ambient the two-word platform span re-spans into the call's failure union; a total spelling extracts the payload, and failure prints `io|net error: <msg>` and halts; CALLT-to-fallible is Unsupported.
- Reentry seam facts (R12): generic builtin bodies receive `&mut Vm` and may call user code through `Vm::call_hosted` — hosted calls inc each Ref argument (the callee's placement owns its parameters; staging them as borrowed double-decs), a boundary drive answers DriveOutcome::Frame keyed on the sentinel reentry frame (u32::MAX return pc, precise across fiber switches), and yield/HALT escaping a hosted drive is the ReentrancyRefused trap. A non-root frame's native tail call lowers in place (CALL plus flush plus return, R13) — only the root frame's native tail may CALLT with abandoned staged counts.
- Lowering invariants: union payloads share one region per union type — a member claims the lowest free word of its kind so every word keeps one SlotType, and a constructed member zero-writes its dead payload words (declaring them Raw was falsified: the call slide validates staged words against typed callee params); scalar `var` places are PlaceShape::Word; float `%` is rejected at the lowering boundary (the locked uop set has ModI64 only); generic callees route through GENERIC_NATIVES and CALLG (name-routing a monomorphic body for an un-tabled generic miscompiles receivers); a bare identifier in a match test position resolves through the type namespace first — a hit is a member test binding nothing while declaration positions always bind — and the predicate lives in three aligned places (resolve `arm_tag`, type_of `unit_pattern_atom`, ds-lower `binding_atom`) that must stay aligned; a catchall-free match is accepted at lowering (exhaustiveness is the checker's proof); an operand's raw span can never be returned directly — returns re-span per member into the destination union; list_new's zero-arg call still requires a one-word overlay area at the call site (the result must land somewhere — the lowering contract notes it); a construct that re-executes its head must fork the placement map before the head (rc.save plus loop_enter) — a Ref cond operand born outside the loop is loop-carried with its count dying at the owning scope's end past the loop, and anonymous carried operands sweep through drop_unnamed once per evaluation at the exit; for-range keeps the pre-fork head only because its replayed test reads frame-static scalar words, so any future Ref-reading head must fork first or the head's read-dec re-fires per back edge and frees the object after the first iteration; a frame ending in a tail call stages the exit path's deinit claims before the argument Movs — claims first, arguments after, or each claim's overlay window clobbers the staged argument span (a Ref-expecting callee trapped on a staged/expected mismatch, an i64-expecting callee silently received garbage), and a Ref argument rides the CALLT transfer with its count
- Test assertions on OS error text pin locale-independent facts (operation plus target prefix, `(os error N)`), never English substrings.
- Fuzzing notes: cargo-fuzz has no automatic dictionary discovery, pass `-dict=` explicitly; a Rust panic aborts before libFuzzer's deadly-signal callback registers, so crash artifacts/tmin miss it — recover the input from the panic message instead. Valid dsmod modules are unguessable by mutation (content hash gates acceptance); generate seeds deterministically (fuzz/tools/gen_dsmod_seeds.rs) before fuzzing the reader; fuzz/corpus is gitignored yet tracked — seed additions need `git add -f`. The ds-mod reader rejects non-canonical section-directory order since 2026-11-10 (found by the campaign, 70-byte reproducer in the corpus). The checker generator leg (fuzz_check_gen over the shared generator in fuzz/check_gen.rs, 240-seed nextest mirror in CI) covers declarations through the extraction family biased to the well-typed surface with weighted wrong splices; its mirror bring-up found the linearity audit compounding exponentially in nesting depth (26-deep took ~8s, 30-deep never finished) — the audit walk is linear in program size by invariant, and a nested loop's duplicate round mutates nothing and must stay skipped; multi-worker sanitizer campaigns fit only on a quiet host — the 8-worker ASan legs were OOM-killed by earlyoom under parallel-agent load during bring-up, so serialize or cap workers when the host is loaded.
- Shared-cache link ICE (2026-11-10): a whole-workspace test build against the shared CARGO_TARGET_DIR can hit a rustc ICE (1.101.0-nightly 75a75c3e0, `add_static_crate`'s `src.rlib.as_ref().unwrap()` at link.rs:3741 — cargo handed rustc an rmeta-only check artifact for a crate a linking unit needed as a real rlib; the backtrace's ensure_removed frame is symbolization noise, and the sibling path already degrades gracefully as OnlyRmetaFound). Trigger: check-heavy sessions leave rmeta-only state fingerprinted fresh for some crates, then a workspace test link in the same cache unwraps None — the panic aborts cargo instantly (no memory hazard; the failure constructs the linker command before any spawn, so observed RAM pressure is collateral process trees, not this path). Heal: targeted `cargo clean -p <crate>` for the affected cones (blind retries keep failing while fingerprints claim fresh); prevention: a per-worktree private CARGO_TARGET_DIR when mixing check-heavy and link-heavy work. Likely unreported upstream (nearest is #138243; the obvious fix is OnlyRmetaFound instead of unwrap); the incident scene is preserved as round/r47 @ fc4c7ee.
- Cross-language benchmark conventions: substring/char_at/len answer from a cached per-string char map (lazy one-time build for non-ASCII, zero storage for ASCII) — the old O(string length) char_indices().nth scans are gone, so strings-heavy benchmarking is unblocked; string lengths are tracked arithmetically in benchmarks/examples until generic `len[t]` lowers at M6; Rust f64 Display prints `250.0` as `250` while Python/Lua print `250.0` and Lua tostring is %.14g, so float goldens print values every printer agrees on by construction (odd fourths); `ds run` echoes the unit entry result as a trailing `0` line, absorbed by the `^result:` extraction rule. benchmarks/ab.sh times whole-process wall clock, so its percentages understate in-VM deltas symmetrically.

## Open threads

- Ambient taxonomy residuals: the unnameable set is parse_json/open/read_exact, whose signatures must name file-owned types (json, the caller's `~Copyable` file struct) — waiting on M7 package-owned types. Record question: the ambient `enum net_err { timeout, refused }` pre-commits two runtime case tags against the platform's single NET_ERR discriminant — a future fetch native must answer both or the enum gets re-shaped. https stays rejected in the net natives until the M7 platform decision.
- Callable words, remaining seams: partition refuses (its tuple result needs the multi-word span channel in the native path); par_map/par_reduce wait on the M5 in-language surface; the module-id space stays registry-assigned interim until the ds-mod link table assigns indices; captures copy by value at CLOSURE_NEW, so mutable-capture shapes stay out of the kernels by construction. Pinned honest refusals: var-span widening, a lambda capturing a closure word, a lambda capturing a callable-defaulted binding, callable collection elements and struct fields, and native/generic fn values minting callable words; tail calls to routed generic natives stay CALLTG-reserved. Wording finding: the combinator reboxing refusal calls a named-fn argument a "lambda" (ds-lower). Record questions: the in-place closure tail as the one TCO carve-out (or maybe-Ref callee tracking to restore CALLT later), and whether native fn values should ever mint a callable word.
- Tuple and multi-word seams: value-level tuple tests (`(0, _) =>`) and guarded multi-arm matches on non-union values refuse at lowering (disjoint tests need a union); `var` destructuring needs per-element var slots; destructured const binders alias a var's words — reassignment through the var is observable through the binders and needs a record ruling; multi-word list elements (struct/tuple), multi-word struct fields, and tuple union members all wait on the reboxing alternative.
- Generic-impl lowerer seam: monomorphic impl bodies lower under mangled identity and their trait call sites dispatch through CALLG, but generic impl bodies never enter the plan (plan_item does not collect them), so a call site that resolves to a generic impl checks clean and refuses at lowering naming the runtime-evidence seam; the missing half is bodies in the plan (mangled identity plus DefId/FnSig entries over FnDecl-plus-Self shapes) plus runtime-resolved evidence words through ds-mod's FuncRef instance registry over CALLG.
- Checker expression-error epoch, ruling question: poison is subtree-scoped, not environment-scoped — a poisoned initializer bound to a name can still print one follow-on at a new use site; threading it through Binding doubles the state surface.
- VM honesty residuals: staged Ref counts stay with interrupted fibers' kept frames awaiting the deinit walk; the cross-nursery join interlock recursion is open; the CALL inline cache retains swapped-out versions until eviction (bounded, memory only); KSTR intern rows for dead pools are never reclaimed.
- fuzz_vm coverage boundary: the generator's vocabulary covers the builtin 0-40 and std console natives only (file/net excluded to keep execution sealed), there are no fibers/nursery interfaces, and the run limit is stack words not steps — the retargeting invariants in emit_function are the re-audit point if the uop ABI changes.
- Placement conservative lifetime: a loop-carried word's textual last use in-body holds its count until the next overwrite or exit (the loop-claims stack's deliberate extension).

### Next-round queue, ranked 2026-11-12 (doc-truth round: plan, README, tutorial, and the conformance rows re-probed against the merged tree)

Every item below was probed or source-verified on the current base; landed history lives in git.

1. Generic-impl lowerer seam (M6, top engineering item): generic impl bodies never enter the plan, so a trait call site that resolves to a generic impl checks clean and refuses at lowering naming the runtime-evidence seam (the tutorial's `list` to_json call site is the live repro); bodies need mangled identity plus DefId/FnSig entries, FnLower needs the FnDecl-plus-Self shape, and dispatch needs runtime-resolved evidence through ds-mod's FuncRef instance registry over CALLG; effort M-L.
2. Checker residuals of the generic-impl round: a dot-call whose serving head matches while a bound fails reports the receiver-level `no instance` message without the substituted-bound note the generic-propagation spelling shows (quiet-filter), declaration-time coherence does not look through generic heads, the impl-of-trait check assumes `Self` implies the trait, and inferred-return trait members are not agreed against the declared signature; effort S-M.
3. Trait-surface residuals: inherent impl bodies refuse at lowering naming the UFCS alternative (the tutorial's spelling stays free functions), union-head impls are not yet covered by `is`/match narrowing, and `mut self` receivers check but have no lowerer plan; effort S-M.
4. Deinit refusal residuals: closure-captured words, union payload fields, native/generic arguments, tail arguments, and generic-struct deinit refuse honestly at lowering (the five named cases); effort M behind the reboxing ruling.
5. The M5 in-language nursery/spawn surface: the nursery block parses and checks clean and refuses at lowering as "a control statement" (DS4009); the substrate (fiber pool, join, first-failure cancellation at backedges, teardown, trap-attribution honesty) is done, so this is surface wiring plus the effects story. effects_nursery's leading-binding-arm shapes surface here (M5-exempt today); effort M.
6. Multi-word element reboxing (a ruling first): list_push of struct elements refuses ("list elements lower as one scalar word"), multi-word struct field types refuse at payload_fields, and tuple union members refuse — the wider-list-element ruling blocks the tutorial's collections-of-records story; effort M after the ruling.
7. Union-annotated bindings over bare member literals: `const n: int | string = 7` passed to a union-typed parameter refuses through the word_addrs catchall ("this value's representation") while direct literals pass — construction must re-span per member via if/match; effort S.
8. Tuple seam closes: value-level tuple tests, guarded multi-arm matches on non-union values, and `var` destructuring (per-element var slots) all refuse DS4009 today; effort M.
9. Fallible-native error-member pairing: which error member the failure discriminant constructs rides the ambient signature (fallible_native_union in ds-lower) — only ambient spellings reach the path, and a user re-spelling of read_file's union would construct the wrong member; in flight with a parallel round; effort S-M.
10. `is fn(...)` written-type narrowing checks clean but refuses at lowering ("a written type") — the read-back call-through idiom routes through a fn-typed binding or the `.ok` handler spelling today; effort S.
11. ds-rt disposition: adopt, freeze, or delete the unconsumed reference implementation — an author decision (the M4 row carries the honesty note); effort S.
