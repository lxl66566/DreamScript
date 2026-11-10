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

ds-lower lowers the checked subset to static frame layouts and uops (overlay staging with per-word Raw widening, `?`/`.err` to TAG_TEST + Jmpif + RETSPAN respan exits, naive RC placeholders until the M4 pass); ds-vm tier 1 executes from encoded programs (decode-on-dispatch, unboxed slots, tag+payload unions across consecutive slots, frame-reuse TCO with argument slide, tag-keyed 1-2 entry inline caches with full-lookup fallback [H3]); `ds run <file.ds>` and `ds build <file.ds>` compile source end to end with the source hash recorded; natives split per [H5.5] into `builtin` (pure core, every VM) and `std` (the default platform surface, CLI-installed). The remaining lowered-subset boundary lives in the status table: generic/trait-method calls (M6 CALL_GENERIC), block-argument sugar, named arguments at call sites, byte literals, bare `$name` holes, multi-atom uses without is/match splitting. Landed since: `.ok` handler lowering, the list surface wired end to end (split/list_* natives, ambient stdlib declarations, `list[t]` as one heap Ref word, list_get as a checked ok/none span), and char literals lowering as one-char string refs.

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

### Near-term scheduling (advisory)

Advisory queue only — not a re-staging; section 10 of the record and the milestone table above stay authoritative.

- Landed earlier rounds: the fn sweep + receiver spelling [F2, record 6.1] (`fn` is the only named-function declaration, `self`/`mut self` typed `Self`), `.ok` handler lowering, the list runtime shape, and F9 two-namespace resolution.
- Landed: two-namespace resolution [F9] — types and values resolve in separate namespaces (constructor positions against types, call-form collisions as DS2014 ambiguity errors), the `map` collision retired with the ambient's `struct map` + `fn map` coexisting, and the per-file success types staying success-with-error-row (their blocker is error-taxonomy ownership, not namespaces — see the open thread).
- Landed 2026-11-10 (the R5 surface round, closing the top of the gap ranking): script entry [G3] (entry-file top-level statements fold into a synthesized `main`; library parse rejects them with DS1014), the interpolation boundary [F6] (holes are string contexts desugaring to `str(e)` over the printable primitives, bare `$name` holes lower, the empty string literal is a plain constant), enum blocks [B3] (the only payload-case declaration, call-form construction, cross-spellings are checker errors), the list surface (the `[a, b, c]` literal, the `list[t]()` constructor, a `list_push` native with a `mut` window), ranges [F8, D3] (`a..b`/`a..=b` with `for x in` loops and `x in a..b` membership; a range is sugar, never a storable value), anonymous tuples [B3] (consecutive frame words, destructuring in bindings and patterns), the byte-literal parse surface [F6] (`b'x'` pinned to a declared `byte` atom; the typing ruling is an open thread), the separator re-spelling (line-initial `##`, `///` reserved as doc-comment trivia, sigil-form imports pinned with the quoted form retired), and the expected-typing closes (`return e?` and if/match branch finals carry the declared return; strict-sub-union returns and widenings re-span per member into the destination union).
- In flight: CALL_GENERIC phase 1 — the ABI reservation under Major::Call (minors 4/5, CALLG/CALLTG, per H3's pinned emission form), one generic body dispatching on evidence words, the ambient `len[t]` over string and list as the acceptance target; and the M4 VM seam activation (the placement pass's finalize emitting RcOps over counted heap blocks).
- Then: generic-call phase 2 (trait-method dispatch on the same evidence), the M5 in-language nursery/spawn surface, the byte-atom typing ruling, M6 derives/comptime, M7 (platforms + registry per the H6 ruling), M8.

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
| M3 | Lowering and tier-1 VM (ds-lower, ds-vm, `ds run`) | core done 2026-11-09, boundary shrunk round 4 (ds-lower + `ds run <file.ds>`/`ds build <file.ds>` end to end; var mutation — `var` locals, plain/compound assignment, `while` — and monomorphic UFCS dot-calls lower; the `or`-retirement round landed statement-position `return` (value and bare) mid-block, so the corpus if-is + `return` early exits run; examples/ hello/fib/strings/mutation/files/net run green; natives split per H5.5 over the historical 0-24 run: `builtin` core (str_*, string ops) in every VM + `std` platform native module (console/file/net) host-installed, the CLI by default; open: generic/trait-method calls (CALL_GENERIC phase 1 in flight), block-argument sugar, named arguments (admitted for enum case constructors only), byte-literal typing (needs the byte-atom ruling), multi-atom uses without is/match splitting; landed 2026-11-10: `.ok` handlers, the list runtime, char literals, bare `$name` holes, script entry, interpolation auto-str with the empty literal, enum blocks, list literals/constructors with list_push, ranges + for-in + in-membership, tuples, and the `##` separator re-spelling) |
| M4 | Memory (ds-rt: arenas, Perceus RC, reuse, deinit) | done 2026-11-10 at the compiler level: ds-rt (bump arenas with chunk-append growth and free-list pooling, per-fiber arenas, restore guards; precise non-atomic RC, reuse threading, deinit dispatch, `diag` diagnostic layer) plus the ds-lower Perceus dup/drop placement pass (place.rs; ownership model over Ref-typed body words, alias-widened last-use deaths, call-staging dup/forward rules, fork/join per arm with loop-depth guard, one emit_drop routing deinits; the RcOp Inc/Dec pair lives in reserved uop major 0xE beside `Uop`, runnable module code still omits it — the VM seam: counted Heap blocks for strings/lists, one 0xE dispatch arm, finalize flipping to emit RcOps, ds-mod accepting 0xE via the RcOp path; the VM activation is in flight 2026-11-10) |
| M5 | Concurrency (fibers, nursery, cancellation, fork-join) | substrate done 2026-11-10 (fiber pool with chunk-list segments and O(1) switch; nursery join with spawn-order determinism, first-failure cancellation observed exactly at backward jumps, boundary error unioning with per-child outcomes, teardown walks; bare detach routes to the live root nursery or the host supervisor); the in-language nursery/spawn surface and fork-join combinators wait on the checker/lowering rounds |
| M6 | Derives, comptime, reflection (ds-meta) | not started; CALL_GENERIC phase 1 (ABI reservation under Major::Call minors 4/5) in flight |
| M7 | Modules, packages, FFI, tooling (ds-pkg, ds-ffi, REPL, hot reload, LSP) | not started |
| M8 | Tier 2 copy-and-patch and tier 3 (ds-jit, SSA, differential tiers) | not started |
| M9 | Hardening and 1.0 (fuzz gates, perf gates, tutorial) | not started; docs/en/tutorial.md exists early as the explainability-budget proof |

## Progress log

2026-11-09 to 2026-11-10, M0-M2 done and the M3 core landed through four parallel rounds (per-round detail lives in the git history and the status table): workspace and CI (nightly fmt, stable clippy+nextest), the locked contracts (ds-abi uop set and call ABI, ds-mod format v3), ds-diag (27-code registry), ds-parse (250-rule pest grammar, DS1009-DS1012 rulings), ds-query (block-grain invalidation, randomized invariant harness), ds-fmt (idempotence plus re-parse shape gates), ds-check (B1/B2/B6/C1/C5/E1 with the golden negative suite), ds-lower and `ds run`/`ds build` end to end, the H5.5 native split, var mutation and monomorphic UFCS lowering, and the `or` retirement swept through parser, checker, lowerer, and corpus (statement-position `return` lowers; `or` is a plain identifier again).

2026-11-10, same-window follow-ons: the list runtime shape in ds-vm (one heap list object behind list_new/list_len/list_get/list_concat, plus split and fs_list), ds-rt precise non-atomic RC with reuse threading, deinit dispatch, and the `diag` diagnostic feature, the cross-language benchmark suite under benchmarks/ (check/bench runner, correctness goldens, no committed timings), the one-short-tutorial artifact docs/en/tutorial.md, and the zh decision-mirror resync.

2026-11-10, surface adoption rounds: fn-only declarations with the bare `self`/`mut self` receiver typed `Self` (DS1013/DS2015/DS2016, corpus and examples swept), `.ok` handler lowering plus the ambient list surface, F9 two-namespace resolution in the checker (the `map` struct and `map` function coexist; call-form collisions are DS2014), the M5 substrate completed through the nursery round (join, first-failure cancellation at backward jumps, boundary error unioning, teardown), the parser fuzz gate closed (617k executions clean, nextest mirror, ds-mod section-order canonicalization fixed after the campaign found the reader accepting transposed directories), and the tutorial verified against the live pipeline (two tutorial bugs fixed; three pipeline bugs plus a ranked expected-gap table recorded in Open threads). The M4 compiler story closed the same day: the Perceus placement pass in ds-lower over the ds-rt RC primitives, with the VM-side RC activation seam enumerated above. The tutorial-found pipeline bugs (match misdispatch, unit-tag or-patterns, tail-`?` containment) were fixed the same day with conformance and e2e regressions.

2026-11-10, the R5 surface round: the tutorial expected-gap list closed from the top — script entry (top-level statements fold into a synthesized main, library parse rejects them with DS1014, a block's binding tail now checks and returns unit), interpolation auto-str over printable primitives with bare `$name` holes and the empty literal, enum blocks minting cases into the tag namespace with fixed construction forms, list literals plus the `list[t]()` constructor and a `list_push` native, for-in over int ranges with `in` membership, anonymous tuples as frame words with destructuring, byte literals pinned to a declared `byte` atom at the parse layer, the `##` separator re-spelling with sigil-form imports pinned, and the expected-typing closes (`return e?`, if/match branch finals, strict-sub-union returns and widenings re-spanning per member). CALL_GENERIC phase 1 and the M4 VM RC activation are in flight; a fresh tutorial verification pass is running against the live pipeline.

## Standing pitfalls

- Parallel-agent protocol: per-agent git worktrees plus branches, additive-only root Cargo.toml edits, one shared CARGO_TARGET_DIR, agents never run git in the main checkout, the coordinator is main's single writer; Cargo.lock conflicts on rebase resolve by delete plus `cargo check` regen; after any hand-resolved merge run `cargo clean -p <affected>` plus a fresh check before pushing — the shared target dir serves stale fingerprints and masked two merge breakages once, and a workspace test can likewise pick a parallel worktree's rlib (a `touch` of the affected crate's sources plus a rebuild refreshes the fingerprint without any clean); an orphaned test exe (interrupted or parallel nextest run) locks its own output file and fails relinking with LNK1104 — find the exact PID by image name and kill it by PID. The staleness has a deeper form: `cargo build -p <downstream>` recompiles downstream but not a touched upstream crate, and a stale rlib was once linked while cargo claimed Fresh (the `touch` remedy failed; `cargo clean -p <touched>` then building the touched crate directly and verifying its `Compiling` line is the reliable remedy before trusting any all-green suite). The shared `target/debug/ds` binary is also cross-worktree — parallel agents' builds clobber it, so run-probes pin a private copy (`cp target/debug/ds /tmp/<private>`).
- Extraction spellings: an un-annotated `.err` handler under no contextual expectation consumes the whole union ("`.err` leaves nothing" plus an empty-use cascade) — ported fallback sites annotate the handler parameter (`(e: net_err) => ...`, `(_: io_error) => 0`). The retired `or`'s contextual empty-intersection typing hid real width meetings: linearity_modes_rc.ds:39's u32 + int surfaces one honest DS2001 now (no conversion callable exists yet; the B7 message's `u32(x)` suggestion is unimplemented). `let x = e?` over a fully-contained union reports DS2002 by design — expected=None propagates every member so nothing binds; the honest spellings are `return e?` and the final expression.
- Nursery/fiber substrate facts: a child's pool slot can be recycled while the nursery's children list still holds the id — drivers re-check `fiber.nursery` before driving (outcomes are ledgered before recycling); a child yielding inside an infinite loop under a join drive hangs the driver by construction (substrate fact; the language surface must bound it); CALLT pc-reset is deliberately not a preemption point, so a self-tail-loop never observes cancellation.
- CI runs fmt on nightly only (rustfmt.toml unstable options; a harmless nightly rustfmt stderr quirk around single_line_if_else_max_width is left as-is); every crate must inherit workspace.package fields (clippy's cargo lint group fails on missing readme metadata); nightly clippy rejects `map(format!).collect::<String>()` and `push_str(&format!())` — build strings with plain `format!` composition; cargo doc belongs in the verification loop too, not just clippy/nextest — single-token bracket refs like `[C4]` in doc comments parse as broken intra-doc links (the tree currently carries ~250 of them), so write parenthesized `(C4)` or slash forms.
- Round-closing protocol: corpus noise counts measured in a worktree go stale as parallel landings retire errors — re-measure every count on the merged main before pushing (the effects_nursery count dropped 7 to 4 when the list constructor landed under it). The host's GIT_EDITOR is nvim — interactive git (a bare `rebase --continue`, commit prompts) hangs the shell-proxy session; kill the nvim PID and rerun with `GIT_EDITOR=true`. Never run cargo fmt outside your worktree — a stray fmt in the main checkout rewrites tracked files and breaks merges. cargo nextest name filters match test names, not files — use `cargo test --test <file>` for a new test file. Union-member render order in diagnostics follows tag allocation order, not source spelling — pin goldens from live output. Multi-statement one-line blocks are parse errors; write block statements on separate lines.
- Native/platform seam: install natives into the registry before loader::install (pool FuncRefs validate at install time; `Vm::with_limit` installs only the builtin core; the CLI calls provide_platform on both run paths). Tag ids: module lowering allocates union ids upward from 0, the platform reserves the u8 top downward (IO_OK 255, IO_ERR 254, NET_ERR 253, PARSE_ERR 252, NONE 251 — the cross-module none member minted MAX-4, so the countdown reservation is 4 deep). The builtin and std native modules' index spaces have diverged (builtin now runs past its historical 0-19: split 17, list natives 20-23 against std's net 20-23); per-module indices must be re-pinned when the M7 freeze happens. Fallible natives: a non-tail CALL TAG_TESTs and extracts the payload, failure prints `io|net error: <msg>` and halts, CALLT-to-fallible is Unsupported.
- Lowering invariants: union payloads share one region per union type — a member claims the lowest free word of its kind so every word keeps one SlotType, and a constructed member zero-writes its dead payload words (declaring them Raw was falsified: the call slide validates staged words against typed callee params); scalar `var` places are PlaceShape::Word; float `%` is rejected at the lowering boundary (the locked uop set has ModI64 only); generic callees — ambient `len[t]` included — wait for M6's CALL_GENERIC (name-routing a monomorphic len miscompiles non-string receivers); list_new's zero-arg call still requires a one-word overlay area at the call site (the result must land somewhere — the lowering contract notes it)
- Test assertions on OS error text pin locale-independent facts (operation plus target prefix, `(os error N)`), never English substrings.
- Fuzzing notes: cargo-fuzz has no automatic dictionary discovery, pass `-dict=` explicitly; a Rust panic aborts before libFuzzer's deadly-signal callback registers, so crash artifacts/tmin miss it — recover the input from the panic message instead. Valid dsmod modules are unguessable by mutation (content hash gates acceptance); generate seeds deterministically (fuzz/tools/gen_dsmod_seeds.rs) before fuzzing the reader; fuzz/corpus is gitignored yet tracked — seed additions need `git add -f`. The ds-mod reader rejects non-canonical section-directory order since 2026-11-10 (found by the campaign, 70-byte reproducer in the corpus).
- Cross-language benchmark conventions: substring is O(string length) per call in tier 1 (char_indices().nth) — a measured optimization target before any strings-heavy benchmarking; string lengths are tracked arithmetically in benchmarks/examples until generic `len[t]` lowers at M6; Rust f64 Display prints `250.0` as `250` while Python/Lua print `250.0` and Lua tostring is %.14g, so float goldens print values every printer agrees on by construction (odd fourths); `ds run` echoes the unit entry result as a trailing `0` line, absorbed by the `^result:` extraction rule.

## Open threads

- The syntax-review thread is closed: every pending ruling was adopted into the record 2026-11-10 (interpolation boundary, declaration/construction story, fn-only declarations, receiver spelling, two namespaces, script entry, block separator and import spellings, `unit`, ledger recompute), `docs/en/syntax-review.md` is deleted — its rationale trail lives in git history — and the implementation rounds are queued in the near-term schedule above.
- The ambient stdlib's per-file success types stay unnameable after F9 (read_file/load_rows/fetch remain success-with-error-row): the failure unions are file-owned with per-file spellings (`io_err`, `io_error`, `net_err`), so no shared ambient signature can name them — F9 split type from value, not per-package type scoping. The checker half already expresses the intended typing wherever the ambient stub is per-file (the conformance gate's stubs say `read_file -> string | io_err`); the shared ambient waits on packages owning their error taxonomy (M7) or an error-row-to-union bridge in the `?` operator.
- https stays rejected in the net natives until the M7 platform decision.
- Pipeline bugs found by the tutorial verification pass (2026-11-10) — FIXED the same day (regressions in conformance/pattern_dispatch.ds plus crate suites): (A) silent match misdispatch — a bare unit-tag arm compiled as a binding/catchall arm, so a member-arm match always executed the last arm; now a bare identifier in a test position (match arms, nested sub-patterns) resolves through the type namespace first (`none`, else constructor-kind hits), a hit is a member test binding nothing, and declaration positions (lets, for-patterns, the const-of-lambda a top-level `fn` converts to) always bind; the predicate lives in three aligned places (resolve `arm_tag`, type_of `unit_pattern_atom`, ds-lower `binding_atom`) and must stay aligned; (B) or-patterns of unit tags form one member set (fell out of A); (C) tail-`?` over a fully-contained union is the return — the final expression carries the declared return as its expected type, and the lowerer's tail `?` re-spans per member (an operand's raw span can never be returned directly; union regions are per-union-type); a catchall-free match is accepted at lowering (exhaustiveness is the checker's proof). The fixes also silently corrected effects_nursery's `none => last` arm (previously a catchall that always fired). The `return e?` follow-up closed 2026-11-10: the returned expression and the if/match branch finals carry the declared return as their expected type, and strict-sub-union returns and widenings re-span per member into the destination union (emit_return/widen_into mirror try_tail; only the fallible-native runtime seam stays, halting at the native boundary). The verification pass also ranked the expected-gap surface; the 2026-11-10 R5 round landed the top items (script entry, interpolation auto-str, enum-block case construction, ranges + for, list literals, `list[t]()` constructors, tuple lowering). The remaining ranked blockers are generic/trait calls (CALL_GENERIC phase 1 in flight), the @std extraction family, ambient success-type degradation, byte-literal typing, and the tuple pattern-reboxing seams; a fresh verification pass against the live pipeline is running and will re-rank.
- Byte-literal typing needs a record ruling: F6 pins `b'x'` as a single byte but B7's tables type no byte, so the checker routes byte literals through a declared atom named `byte` (undeclared reports DS2002) and lowering refuses by name (no byte slot in the uop contract) — the record must rule on the atom's width class, its conversion-family membership, and whether the ambient declares it.
- Tuple seams open after the R5 landing: value-level tuple tests (`(0, _) =>`) and guarded multi-arm tuple matches need their own seam plus reboxing when M6 lands; multi-word elements and tuple union members wait on the reboxing alternative; `var` destructuring needs per-element var slots; destructured const binders alias a var's words (reassignment observable through them — needs a record ruling); `const _ = e` does not lower (pre-existing oddity); a corpus `tag none` shadows the predeclared `none` strangely.
- Non-range `in` operands type to bool unconditionally in the checker but refuse at lowering — collection membership belongs with the list surface; the record's Kotlin-shaped `in` is range-wired only.
