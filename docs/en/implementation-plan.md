# DreamScript implementation plan

Status: planning record, written 2026-11-09. This document plans the full implementation of DreamScript from empty repository to 1.0 in Rust (edition 2024). It is derived from and subordinate to `docs/en/design-decisions.md`, cited throughout by decision ID ([B1], [H2], ...); when this plan and that record conflict, the record wins and this plan is fixed. The milestone order refines the staging order of the record's section 11: M0 locks the two load-bearing contracts, M1 inserts the front end ahead of the checker (the record's step 2 presumes a parser), and M2-M9 follow the recorded staging one to one. The milestone status table at the end is the living tracking state of this plan.

## 1. Ground rules

Binding constraints, taken from the charter and the record:

1. Staging order follows the record section 11; each step lands on the previous one with no language-change rewrites.
2. The two load-bearing contracts — the slot-overlay call ABI [H2] and the fixed-width mmap-able dsmod format [BL-7, G3.5] — are locked before anything else; everything downstream compiles against them.
3. The two-slot hot-reload module runtime exists from day one: data structures and qualified cross-module calls at M0, the `ds dev --hot` UX at M7; retrofitting is the named mistake to avoid [H6 item 5].
4. Compiler-as-query-library from the first line: every consumer (CLI, LSP, REPL, hot reload, formatter) goes through memoized queries — parse, block_index, resolve, type_of, effects_of, linearity_check, lower — with invalidation at `///|` block grain plus package grain [H6 item 1].
5. Invariants that hold in every milestone: no import-time execution [G3]; guaranteed self-and-mutual TCO in every tier [H2]; dispatch correctness never depends on cache state [H3]; no `any` fallback — inference failure is a compile error [B1]; module bodies are pure compile-time declarations.
6. Explainability budget: any addition must re-fit the short tutorial; the deferred-additive backlog [section 12 of the record] stays out until the core is stable.
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
| `ds-parse` | Lexer, pest grammar, AST with spans and trivia attachments, `///|` blocks | M1 |
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

### M0 — Contracts and bootstrap

Goal: lock the two load-bearing contracts and prove them with hand-assembled programs before any compiler exists.

Work items:

- Cargo workspace, CI on windows-msvc and linux (fmt, clippy, nextest), release profile `codegen-units = 1`.
- `ds-abi`: typed frame layout [BL-6] — contiguous slot spans plus a metadata word (return address, frame size, TCO-chain flags); the slot-overlay convention (callee argument slots overlay the caller's outgoing-arg area, so a tail call is an O(delta) argument slide plus a jump and the stack never grows at any depth); CALLT semantics; the two-word entry descriptor `{tier1_prologue_ptr, tier2_code_ptr}` that flips when a callee later compiles; tail-chain metadata for honest backtraces. The spec lives as crate docs plus tests; changes after lock require written justification.
- `ds-mod` per [G3.5]: magic `\0DSM`, format version, target class, flags (STRIP, FFI-USED, HAS-JIT-METADATA), uleb module name and dependency list, blake3-128 source hash with checked-hash validation (never mtime), sections blocks / constants / type metadata / effect rows / debug spans (strippable); writer, reader, mmap loader. The two-hook collector interface fields [C4, H4] are committed in the header from byte one.
- `ds-diag` skeleton; `ds-vm` skeleton: typed value stack, frame push/pop, a minimal dispatch loop able to execute hand-assembled uop fixtures.
- Two-slot module table data structures and fully-qualified cross-module call paths in the skeleton [H6 item 5].
- Hand-assemble the record's section 13.7 programs (`sum_to`, mutual `even`/`odd`) as test fixtures.

Acceptance: fixtures execute correctly; mutual tail recursion at depth 10^7 runs at constant stack; dsmod write-then-read is byte-stable; a corrupted file fails the hash check with a structured diagnostic; a cargo-fuzz target on the ds-mod reader runs from day one.

### M1 — Front end

Goal: parse the whole decided surface; the seed corpus becomes conformance files.

Work items:

- Lexer and pest grammar for [F1, F2, F6]: newline-terminated statements, no semicolons, trailing commas everywhere, `$x` and `${expr}` interpolation as structured AST nodes (each hole is a typed expression, not lexer concatenation), raw strings `r"..."` and hash-counted `r#"..."#` to any depth, `'x'` chars and `b'x'` bytes, numeric separators and width/float suffixes [B7].
- AST with spans; trivia (comments) attached as AST payload so the formatter is lossless; `///|` block separation; order-independent top-level definitions [G3].
- `ds-query` engine skeleton: memoized parse and block_index queries keyed by file and block, invalidation at block grain.
- `ds-fmt`: block-aware, zero options, implemented as a compiler component over the parse query [H6 item 2].
- Port sections 13.1-13.6 of the record to the decided surface (const-arrow functions, `=>` arms, lowercase types) into `conformance/`, each file carrying stage expectations: parse now, typecheck at M2, execute at M3-M5.
- Grammar coverage: trait/impl/generic/comptime syntax parses from the start (needed by 13.5 even though checking lands at M2).

Acceptance: all conformance files parse; `fmt(fmt(x)) == fmt(x)` and re-parsing formatted output yields the same AST on the corpus; syntax diagnostics carry two spans; the lexer/parser fuzz target never panics on arbitrary bytes.

### M2 — Checker core

Goal: the whole static story over nominal atoms, delivered as a library.

Work items:

- `ds-query` full: resolve, type_of, effects_of, linearity_check as memoized queries (package-grain invalidation arrives with M7); effects_of is computed now and consumed by platforms (M7) and nursery internals (M5).
- Name resolution for one package: files share one namespace, zero intra-package imports, intra-package cycles resolved by whole-package fixpoint [G3]; UFCS resolution order fixed at free functions first, then trait methods, `Trait::f(x)` qualification as escape hatch, capability parameters excluded from the dot position [F3].
- Type core [B1]: nominal atoms (tags, structs, enums, one type_id each), set expressions `|`, `&`, `-` over them, and the emptiness/disjointness decision procedure over atoms plus negated constraints (`~Copyable`); difference types erase after checking; unions of atoms lower to the unboxed tag+payload layout.
- Bidirectional checking [B2]: required top-level and public signatures anchoring the set algebra; locals, lambdas, and match arms inferred; monotone flow refinement through `is` tests, none tests, and match arms, resetting only at assignment; `?` propagation as set containment against the written return union; `T?` in every annotation position [B4]; the numeric table — trapping defaults, wrapping `+%`/`-%`/`*%`, explicit conversions only [B7]; string typing with the checked slice API [B8]; structural `==` with the five carve-outs [B9].
- Traits and generics [B6]: instance environment; union-head decomposition into per-branch instances; pairwise disjointness checked at declaration and again over the final instance environment at link time; overlap errors naming both heads and a witness value type; the structural cycle check rejecting self-satisfying conditional instances; no orphan rule.
- Affine checking and DI [C1, C5]: last-use/definite-initialization as a single intraprocedural forward pass; moved-binding liveness; `~Copyable` subtraction; deinit well-formedness (exactly once, all paths).
- Structured diagnostics [H6 item 3]: the DS3001-class two-span linear errors ("moved here / used again here"), machine-readable suggestions, `--error-format=json`, spans computed at typed-AST level.

Acceptance: conformance files typecheck (runtime-free parts); the negative suite renders golden diagnostics — `impl Json for int | bool` rejected with witness, moved-then-used pairs, non-exhaustive match printing the set difference ("missing: NotFound | Locked"), inference failure without any `any` fallback; `ds-check` runs head-less as a library with no I/O beyond query-context file reads.

### M3 — Lowering and tier-1 VM

Goal: `ds run` executes the corpus; the M0 ABI is now fed by a real compiler.

Work items:

- uop set [BL-5]: defined once as a Rust enum with explicit slot effects and `pure` annotations; 32-bit fixed encoding; typed opcodes (ADDI64/ADDF64/EQI64/...); this one set is the contract for every tier and the stencil source.
- `ds-lower`: typed AST to static frame layout (slot assignment per BL-6) to uops; CALLT emitted in tail positions; operator calls as trait-method calls [F5]; generic calls emit `CALL_GENERIC fn, evidence_words` with evidence words as ordinary register arguments (statically resolved instances as constant operands) [H3]; `or`/`.err`/`.ok` lowering per [E4]; `defer` as scope-exit sugar lowered to teardown blocks [E5]; naive RC operations as placeholders (the Perceus placement pass lands at M4).
- `ds-vm` tier 1: dense `match` dispatch loop (see section 7 for the computed-goto note); unboxed slots, no NaN-boxing, no boxed scalars [BL-2]; tag+payload ADTs across consecutive slots; frames inline in the typed value stack; per-entry stack-limit compare; preemption flag check every N instructions and on backward jumps; frame-reuse TCO with argument slide [H2]; the two-slot module table live with fully-qualified cross-module calls; 1-2 entry tag-keyed inline caches at trait call sites, miss falls back to full lookup [H3].
- Minimal stdlib surface the corpus needs (int/float/string/list/tuple basics, `println`, `log`, ranges).
- First performance smoke test: `sum_to` against a C build, no formal gate yet.

Acceptance: `ds run` executes sections 13.1 and 13.4 with golden output; mutual tail recursion at depth 10^7 runs at constant stack; a panic prints a backtrace with lazy-materialized elided tail frames; the vm fuzz target (differential against a reference evaluator or golden traces) starts.

### M4 — Memory

Goal: the Perceus/arena runtime with deterministic death.

Work items:

- `ds-rt`: per-frame bump arenas (mark at entry, bump-allocate, restore on every pop path; segment growth by chunk append so captured frame addresses stay stable; per-fiber marks so fiber switching needs no arena action) [C4, H4].
- Precise non-atomic RC: compiler-inserted dup/drop via the Perceus placement pass in `ds-lower` (replacing M3 placeholders); each drop threads to the next allocation as a reuse candidate (`alloc_at`), mimalloc fallback [C4].
- Diagnostic layer shipping first [C1 staging]: runtime `rc==1` uniqueness checks plus an Inko-style borrow counter, behind a flag, kept shippable — it is the debugging instrument for the whole memory story.
- `deinit` execution [C5]: static dispatch (concrete type known), exactly once at the unique instance's death, on every path including early return, panic, and cancellation teardown; explicit `drop` as a debug/FFI tool.
- Cycles rejected at the type level for v0; the two-hook collector interface stays header-committed and dormant [C4].

Acceptance: section 13.2 runs with deinit observed on success, early-return, and panic paths (via test-only observers); free order is deterministic across repeated runs; arena-residency accounting for the corpus reports 100% of non-escaping temporaries as register/arena-resident.

### M5 — Concurrency

Goal: fibers and nurseries over the same VM; race-free fork-join.

Work items:

- Fibers as interpreter data [E2]: heap segments in the VM pool, spawn is one allocation, switch is an O(1) pointer swap; chunk-list segment growth (stable frame addresses); per-fiber TCO intact (tail-looping fibers never grow their segment).
- `nursery` blocks and `spawn` [E2]: scope exit joins children; first failure cancels siblings; child errors union out at the boundary; bare detach requires an explicit root nursery or supervisor.
- Internal effects `cancel`/`discontinue` dispatched by evidence at nursery boundaries; tail-resumptive internal ops allocate nothing [E1]; cooperative cancellation observed only at the M3 preemption points, so a linear consume in flight is never interrupted; teardown walks frames running deinits [E5].
- Panic path [E4]: the fiber aborts, linear deinits run during the frame walk, the trap surfaces at the nursery or supervisor boundary.
- Fork-join [E3]: `par_map`/`par_reduce` over immutable or linear-partitioned inputs; ownership transfer at fork keeps RC counts thread-confined; cheap fork-side snapshots via static RC counts; deterministic and replayable; scheduler backend decided by measurement (`youpipe` vs hand-rolled work-stealing).

Acceptance: section 13.3 conformance; cancellation of a many-spawn nursery takes effect only at safe points; par combinators replay identically across runs; a deep tail-looping fiber runs unbounded without segment growth.

### M6 — Derives, comptime, reflection

Goal: metaprogramming without macros.

Work items:

- Derive engine [G1]: `hash`, `show`, `ord` (field-order lexicographic; the `total_compare` float variant must be supplied explicitly for sort keys), `clone` (deep copy), `to_json`/`from_json`, `default`; `==` is universal and never derived.
- Comptime evaluator [G1, G2]: ordinary code evaluated at compile time on the same uop VM in a fenced context (no world effects — the capability idiom is enforced at the fence); static reflection over tags-as-data (fields and cases of a type, comptime iteration and branching, `comptime T: type` parameters); no runtime type metadata tables anywhere; existentials as evidence-carrying boxes (`box Json` — value plus dictionary) for heterogeneous collections.

Acceptance: section 13.5 runs including `schema(comptime T)`; user derives cover user structs; the comptime fence rejects world-touching calls as compile errors.

### M7 — Modules, packages, FFI, tooling

Goal: the ecosystem layer and the five tooling commitments, complete.

Work items:

- Visibility and interfaces [G3, G4]: private by default, `pub(pkg)`, `pub`; selective, aliased, conditional imports and `pub import`; `.dsi` generation via `ds info` as the reviewable API surface, LSP hover source, and incremental-compilation boundary; cross-package cycles are link errors whose diagnostic suggests moving the shared abstraction to a base package; package-grain invalidation lands in `ds-query`.
- Loading [G3.5]: lazy dependency-driven loading from the dsmod header dep list; `$DS_CACHE/ir/<triple>/<blake3-of-source+deps>.dsmod` with the mmap hit path; `ds build --exe` embeds the dependency closure plus the VM; cold startup measured against the sub-10ms target.
- Packages [H6]: ds.pkg manifests with the blake3 hash cascade (the manifest is the lock, no lockfile); MVS resolution; a global content-addressed store doubling as the bytecode cache; script mode (`ds run script.ds` zero-config, bare `@std/...` imports, `ds save` freezing a script into a package); optional `ds link` profile linker (scoop shims on Windows).
- FFI [H5]: plain `extern` declarations as the documented escape hatch; linear C handles (`~Copyable`, double-free is a compile error, call-scoped raw borrows that cannot escape); callbacks on VM-managed stacks; `ds bind header.h` emitting typed externs then handle wrapping; the FFI-USED flag at load time; the embedding API in the Lua shape (`ds_push`/`ds_ref`, a `Value` as an index into the object table, `ctx: &mut Vm`, two embedders are two independent VMs, no GIL); blocking foreign calls pin their fiber; cancellation observed only at VM re-entry.
- Platforms [H5]: `platform "std" provides <console, fsys, net, time>` with the `ds` CLI as default; the checker verifies user code's effect usage is a subset of the platform's declared row (consuming effects_of from M2).
- REPL [H6 item 4]: linear-aware, prints inferred type and effect row, runs over the same bytecode IR, session-owned linear context with explicit `:drop` and `:reset`.
- Hot reload [H6 item 5]: `ds dev --hot` recompiles changed blocks, swaps the second slot, retires old-version frames at their next cross-module call, type-checks heap values against new defs — now exercising the M0 structures end to end.
- LSP over `ds-query`: diagnostics, hover from `.dsi`, on-type formatting from `ds-fmt`.

Acceptance: a multi-package project (app plus local lib plus `@std`) builds, runs, and hot-reloads; section 13.6 conformance; `ds save` round-trips a script into a package; cold startup under 10ms on the mmap hit path.

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
- Triage the deferred-additive backlog [section 12 of the record] against the remaining sugar headroom (16 features / 40-42 units); anything adopted ships as pure additive with no migration.
- Stage the post-1.0 pure-optimization queue (no language change): borrow inference, full reuse pairing, TRMC, fusion upgrades, `T | none` niche packing, rope.
- Version stamping in dsmod headers; semver discipline from 1.0 on.

## 5. Test strategy

- Conformance corpus: sections 13.1-13.7 ported at M1 plus every bug regression; each file carries stage expectations and golden output; from M8 every file runs on every tier.
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

Permanently out or out of 1.0, enforced at review: no import-time execution; no runtime type metadata tables or type objects; no macros at any level; no user-defined operators, Deref coercions, or convention-based privacy; no hidden classes, field inline caches, speculation, deopt frames, or side traces in any tier; no GC in v0 beyond the committed two-hook interface; no HKT or GATs in 1.0; no `?.`, elvis, `!!`, smart casts, receiver-lambda DSLs, primary constructors, reified type parameters, or backtick identifiers [F7]; no public generator syntax in 1.0 [D3]; no `|>`, anonymous record literals, `@` binders, vararg/spread, labeled break/continue, or `it` shorthand unless triaged in at M9 [section 12 of the record].

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
| M3 | Lowering and tier-1 VM (ds-lower, ds-vm, `ds run`) | lowering landed 2026-11-09 (ds-lower lowers the checked types_hybrid subset to executable modules: golden stdout on the tier-1 VM, CALLT at 10^6 constant stack; runtime contract extended with strings KSTR/EQSTR/STRCAT, RETSPAN, SlotType::Raw, builtin native module; generic/comptime calls and `ds run <source>` wiring open) |
| M4 | Memory (ds-rt: arenas, Perceus RC, reuse, deinit) | arena half done (bump arenas with chunk-append growth and free-list pooling, per-fiber arenas, restore guards; RC/reuse/deinit next) |
| M5 | Concurrency (fibers, nursery, cancellation, fork-join) | not started |
| M6 | Derives, comptime, reflection (ds-meta) | not started |
| M7 | Modules, packages, FFI, tooling (ds-pkg, ds-ffi, REPL, hot reload, LSP) | not started |
| M8 | Tier 2 copy-and-patch and tier 3 (ds-jit, SSA, differential tiers) | not started |
| M9 | Hardening and 1.0 (fuzz gates, perf gates, tutorial) | not started |

## Progress log

2026-11-09, M0 done in one round: workspace bootstrap (glob members `crates/*`, shared lints/profiles, CI on windows+ubuntu: nightly fmt, stable clippy+nextest), then ds-diag (spans, 24-code DS registry, human+JSON renderers), ds-abi (locked frame layout, CALLT slide plan with memmove-safe direction, two-word entry descriptors with CAS flip, tail-chain counters), ds-mod (G3.5 format: strict-canonical reader, byte-stable writer, blake3-128 checked hash, mmap loader, dormant collector hooks; cargo-fuzz target ran 30k execs clean), ds-vm skeleton (provisional 13-op set, two-slot module registry with retiring-slot liveness, CALLT per the locked ABI: mutual even/odd at depth 10^7 holds a 7-word constant stack). M1 partially landed in parallel: conformance corpus (sections 13.1-13.6, 59 items, 1 unresolved v2 handler kept as comment) and the ds-query engine (red-green renewal gives block-grain invalidation; placeholder computes await ds-parse wiring). Pitfalls recorded: rustfmt.toml unstable options force nightly fmt (CI fmt job pinned to nightly); clippy's cargo lint group demands full package metadata, so every crate must inherit workspace.package fields; parallel-agent protocol that produced zero merge conflicts: per-agent git worktrees + branches, additive-only root Cargo.toml edits, shared CARGO_TARGET_DIR, Cargo.lock conflicts on rebase resolved by delete + `cargo check` regen. Record addendum 6.1 pins the corpus surface normalizations; the linear two-span error example code moved to DS3001 to match the shipped registry's class partition.

2026-11-09 (later), contracts hardened and M2/M3 prep landed: a read-only cross-crate audit caught a P0 in ds-vm's CALLT (wide-callee params slid through caller body_len — a 3-param callee read (10, 0, 20)); fix 10e2ad8 pins owner-relative word mapping (callee param i at fp+1+i), now also stated in ds-abi's locked docs with a CALL/CALLT identity table. Audit P1/P2s all fixed: dsmod header commits BL-7's max_call_depth + ir_checksum fields (format v2, values pending until M3), reader is strictly canonical (gapless section tiling), ds-diag pins the full 27-code snapshot table, weekly CI fuzz cron runs the ds-mod mirror (FUZZ_RUNS knob). Contracts grew: ds-abi now defines the BL-5 uop set (39 ops, canonical 32-bit encoding, BIAS_J=0x8000, XOR-negated comparisons, slot-effect/purity tables; 13.7 programs encode exactly), ds-mod defines the blocks payload v1 over uop words + FrameLayouts (every code word validated by decode at read), ds-check landed the B1 type core (canonical atom-set solving, exhaustive small-universe law tests vs a bitset oracle) and the B6 instance environment (union-head decomposition, witness-carrying overlap errors at declare and link, DFS condition-cycle rejection, no orphan rule). Discipline that worked: ErrorCode additions update the code-table snapshot in the same commit; coordinator rebases agent branches whose base moved. Still open: ds-parse is the long pole gating the M1/M2 wiring wave.

2026-11-09 (query fuzz), ds-query randomized invariant harness landed: seeded xorshift PRNG (no rand dep) drives edit sequences over 4 synthetic files (block rewrites, separator indent toggles = layout shifts with all block texts held, insert/remove separators, no-op edits, ghost reads) against a Db with chained downstream computes (TypeOf reads BlockSource, LinearityCheck reads TypeOf). After every op a full sweep asserts: no panic; revision bumps exactly once per real change; every value equals a from-scratch replay (memo state invisible); compute counts move at most +1 per sweep (0 on untouched files, 0 for downstream of text-held blocks — renewal, layout shifts included; a changed file must re-derive its parse exactly once). 4 seeds x 800 iterations in ~1.4s under nextest (FUZZ_RUNS knob); ~150k stress sweeps across 8 seeds found no engine violation — red-green renewal held under every tested interleaving. cargo-fuzz target fuzz_query_engine mirrors the op vocabulary (no-panic + revision exactness + replay equality; 17k execs smoke-clean). Pitfall: nightly clippy's format_collect/format_push_string reject `map(format!).collect::<String>()` and `push_str(&format!())` — build strings with plain format! composition. When ds-parse replaces the placeholder computes, the harness needs its FileModel split oracle synced only if block-split semantics change; the engine-side checks are compute-agnostic.

2026-11-09 (front end), ds-parse landed (3167597, 250-rule pest grammar, spanned AST with line-aware trivia, two-span lexer pre-scan diagnostics, 8 MiB dedicated parse stack with nesting guard): the entire conformance corpus parses clean including reject items. Record addendum 6.1 gained the parser rulings (square-bracket generics canonical with angle laxity; newline-separated members canonical with comma laxity; C2 param modes in-annotation; contextual move/comptime; ASCII identifiers at M1) and the 13.7 sum_to erratum note. ds-query now runs the real parse/block-index queries with syntax errors as data (broken files never poison sibling blocks); ds-diag gained Files::set_source so the edit path no longer rebuilds the registry. Runtime half of M3 closed: the VM executes the uop contract from encoded programs (decode-on-dispatch), ds-mod carries FuncRef constants (format v3) and the loader installs/validates modules end to end — `main` calling even/odd at 10^6 holds the constant 7-word stack through the loaded path, and a cross-module FuncRef redirects through the two-slot swap. Coordination pattern that worked for cross-crate breaks: the dependent agent stacks its adaptation commit on the breaking branch and the coordinator merges the stack whole, keeping main green at every step. Open: ds-fmt in flight; name resolution in flight; type_of/effect row/linearity checker queued behind resolution.

2026-11-09 (later still), ds-cli landed: the `ds` binary (clap derive, anyhow in the binary only) with `ds run <file.dsmod> [--entry <name>] [--args <i64>...]` — validated mmap read via `read_mmap`, install into a fresh `Vm`, entry execution, and i64/f64/bool result printing; traps print the trap plus `Vm::backtrace` summary to stderr; every failure class exits 1 with a self-describing message (missing file, ds-diag-rendered format failure, unknown entry naming the available functions, unresolved foreign callee naming the missing module). Pitfalls recorded: `read_checked` cannot serve a bare-dsmod run (it verifies the stored source hash against source bytes the run path does not have — source-hash checking arrives with the compile pipeline, `ds build`); ds-diag sources are strings, so binary bytes are sanitized one-for-one to printable ASCII to keep diagnostic spans exact; current clippy pedantic bites glue code via `format_push_string` and `comparison_to_empty`. Integration tests build fixtures with the ds-mod builder in-test and drive the binary through `CARGO_BIN_EXE_ds`.

2026-11-09 (audit round 3, ds-parse), nine parser findings fixed in one requirement-scoped commit: `in` moved to comparison level (F5; `x in xs && flag` was `x in (xs && flag)`, kept `x in a..b` via a range-tolerant right side); non-lambda impl members (`impl t { const x = 1 }`) silently dropped — now DS1010; flat unbracketed chains (`a+b+...`, stacked unary, postfix `()`/`.`/`?` chains) built depth-N `Box` chains whose recursive Drop aborts the caller's stack at ~10^4 terms — capped at 128 terms (DS1009; the AST drops on the caller's thread, not the 8 MiB parse thread; 14 fold levels x 128 stays inside 1 MiB); interpolation holes used `sp` and could carry a string across newlines — now `h`, single-line per 6.1; expression-bodied match arms rejected under newline separation (6.1 made it canonical) — accepted; trait/impl member lists took neither commas nor trailing commas — now `decl_sep` like struct/enum bodies; F7 named + literal-only default arguments landed (CallArg/Param.default AST, DS1011 duplicate, DS1012 non-literal incl. interpolating strings; positional-after-named rejected); nested unary nodes carried whole-level spans — exact token spans now (stacked `??` spans were already exact, adjudicated not-reproduced); lone `\r` accepted inside strings/holes/chars but rejected in code — rejected everywhere except raw strings. Compound assigns `*=` `/=` `%=` `<<=` `>>=` kept as adopted laxity with a grammar comment. New codes DS1010-DS1012, snapshot updated same-commit. Pitfall: string literals are Expr::Str/Expr::RawStr, not Expr::Literal — the literal-only default check must match those variants explicitly.

2026-11-09 (audit round 3, ds-check), tuple distribution capped: tuple_solve minted the product-of-element-sizes atoms unbounded, so an adversarial `(bigunion, bigunion, ...)` was an OOM; the product is now checked (checked_mul, so overflow also trips) against a 4096-combination cap before any atom is minted, failing with the typed Error::TupleDistributionCap naming the rendered tuple, per-element sizes, and cap (new DS2012; snapshot same-commit). Honest distributions are unaffected — the corpus's largest is four combinations.

2026-11-09 (lowering), ds-lower landed (10110ef + 1efba9d): the M3 pass over the checked types_hybrid subset parses, checks, lowers, encodes, installs, and runs a trimmed fixture (crates/ds-lower/tests/fixtures/types_hybrid_probe.ds) with the golden stdout of the hand-assembled ds-vm fixtures — ok path, `.err` handler with match dispatch and `bad_port` interpolation, `or return missing` fallback, structural `==` across union members and payloads, float/string interpolation chains, and tail recursion at depth 10^6 holding a 2-word stack (1.3s). Architecture: ds-check gained an additive seam (analyze() returns the Analyzed bundle; Checked gained the expr_types side table holding every expression's solved Val, flow-refined inside `is` branches — the lowering never re-derives types); Shapes maps Ty to struct/union word layouts with module-wide tag ids; FnLower emits a symbolic uop buffer (labels resolved +1 past FUNCF at finalize), stages call arguments into the overlay area, widens disagreeing overlay words to Raw per the slot rules, and lowers `?`/`or return` to TAG_TEST + Jmpif to a respan exit (RETSPAN copies the propagated member's tag and live payload, or constructs the alternative in place). Union payload strategy: one shared payload region per union type — members claim the lowest free word matching their field kind, so same-kind fields of different members alias one word and every word keeps one SlotType; nested/multi-word member payloads stay unsupported (the minority-payloads-reboxed-to-Ref alternative is the documented escape hatch). Zero-init rule that closed the last two failures: a constructed member writes kind-correct zeros (0 / 0.0 / false / empty Ref) into its dead payload words, because whole-span moves copy dead words into typed slots and a foreign value kind traps SlotTypeMismatch; declaring the payload words Raw instead was falsified — the call slide validates staged words against the callee's typed params and rejects Raw. Pitfalls: builtin natives must be installed into the registry before loader::install (the loader validates pool FuncRefs at install time; Vm::new installs them too late for the ds-mod path); scalar parameters need PlaceShape::Word or field access reads through a span; field access must key off the receiver's refined type from expr_types, not the declared union; universe.fn_sig takes the fn atom TypeId, not the Ty. Trimmed scope (documented, not lost): the ambient stdlib surface (read_file/parse_json/list/map as a checked package member), `j.fields.get`, guarded arms (`b if b is bad_port`), the `.ok` handler, json struct-payload nesting, and method/trait calls — generic lambdas are exempt by plan (M6's CALL_GENERIC owns them), so none of the ambient calls lower yet.

2026-11-09 (audit round 3, ds-query), degraded block scan aligned with the grammar: split_blocks demanded line-initial `///|` and split inside strings of broken files, while sep_unit accepts mid-line `item ///|` and `/* c */ ///|` at file level only. The scan is now a byte-level state machine tracking strings (single-line resync), raw strings, char literals, interpolation holes, comments, and bracket depth: a `///|` separates only at depth 0 outside every token context, owns the rest of its line, and indentation before it extends the preceding block (matching tree spans). Residual approximation documented at the fn: mis-lexed pairing on broken input (stray closer, unterminated raw string) can hide a later separator — degraded extents are advisory until the file parses. The separator-rejection pitfall recorded here earlier is a false positive (falsified 2026-11-09): its examples used bare `const b` without the required `= expr`, which has never parsed; with valid items all 11 item kinds parse in leading-separator, trailing-separator, and wrapped positions, and the file/sep rules are byte-identical since cca1387. The behavior is now pinned: parse_suite cases for the three layouts plus a ds-query mirror test asserting tree-vs-scan block-index agreement on 11 layouts (extents differ by one newline at edges, advisory per the documented rule).
2026-11-09 (formatter), ds-fmt landed (block-aware zero-option formatter over the parse query [H6 item 2]; M1 acceptance for fmt met): format_source(Files, FileId, &Ast) -> String renders each ///| block from its own items+trivia (render_block, pub(crate)) and the file level only reassembles blocks with separators recovered verbatim from source (text+column, blank line each side); unit test pins isolated-block rendering == whole-file assembly. Canonical form: square generics only (angle laxity never emitted), case-type refs keep parens (the AST models both as Path{args}, delimiter read from the span tail), trailing commas never emitted except the one-element tuple, 2-space indent, no wrapping (inline iff the container span is one source line), one trailing newline, strings verbatim from spans, trailing-block calls render without their own parens, else-if keeps the if, const-arrow members render as fn. Gates: fmt(fmt(x))==fmt(x) plus re-parse shape equality over all conformance/*.ds and a 90-fixture mirror of the ds-parse positives (shape = span-stripped, trivia-dropped Debug dump; trailing_comma compared only for 1-element tuples — everything else must match exactly), plus trivia edge suite (attachment, blank-run collapse, doc travel, EOF comments, deep-trivia migration with nothing-dropped counts). Formatting the corpus flushed out four latent ds-parse conversion bugs, all keyword-pair-vs-payload mistakes now regression-pinned in parse_suite: guard/import_cond took the kw_if pair as the expression; type_arg silent wrapper dropped generic args; case_type's head is a plain `path` pair (not type_path) so the case name vanished; kw_block (nursery/spawn/defer) took the keyword pair and silently emptied every block body. Pitfall: the shared CARGO_TARGET_DIR poisons fingerprints across concurrently building worktrees (phantom `CallArg` mismatch); `cargo clean -p ds-parse -p ds-fmt` before a gate run fixes it.

2026-11-09 (type_of), ds-check's bidirectional checker landed [B2]: one expression walk `expr(e, expected) -> Val` over the Resolutions side-table — the `expected` hint is consumed only where inference cannot proceed (lambda parameters, `?` splits, generic construction), containment is checked explicitly at anchors (call arguments, field values, returns, annotated bindings), and inference failure is a compile error with no `any` fallback. Boundary anchoring per [B2]: top-level/public signatures are required and checked by Universe containment; locals, lambdas, and match arms infer. Flow refinement is a per-binder overlay (binder span -> Ty) narrowed monotonically by intersection: `is` tests, none-comparisons, match-arm pattern coverage through the scrutinee binder, guards; the overlay resets at assignment and loop entry (sound: refined is always contained in declared). `?` splits its operand against the success context — at an expected type members outside it propagate, elsewhere members inside the enclosing return union — and every propagated member must be contained in the written return union [DS2008]; `or`/`or return` extract the wanted members per E4; `.err(h)`/`.ok(h)` are difference-typed. B7 checking half: mixed int/float arithmetic is an error naming the conversion, wrapping operators are integer-only (floats are IEEE), trapping defaults need nothing beyond the VM trap. Universe grew two opaque structural atoms: generic applications `Apply(ctor, args)` and function signatures `FnSig(params, ret)` — opaque to the set algebra (a container never distributes over a union argument), structural equality, hash-consed. Method-candidate selection narrows DotRes::Methods by receiver type through the instance env; unsatisfied constraint = DS2013, surviving ambiguity = true-ambiguity DS2014 (codes and snapshot same-commit). Tests: the conformance gate asserts every stage:typecheck item in types_hybrid and generics_coherent_traits checks clean (only the documented get_port tension, exactly two ordering DS2001s, confined), the stage:reject item renders the exact overlap-witness DS2006 message and label, and remaining diagnostics stay inside later-milestone (m6) items; a 20-case type_of suite pins goldens: non-exhaustive match names the missing set difference (`missing: io_err | ...`), `?` propagation, difference-typed err handlers, numeric table splits, structural == with B9 carve-outs, interpolation holes as string operands. Pitfalls: the inherited work was an uncommitted WIP left mid-module-split — the type_of/{mod,ops,pattern}.rs split never compiled (private-fn E0624 across the split), fixed with pub(crate) before anything else; the branch's own ds-parse guard/type_args fixes were superseded by 1baffb0's, so the rebase took main's side with zero semantic loss; both nightly and stable clippy now run the cargo lint group, so missing package.readme metadata fails `-D warnings` on main too (pre-existing, workspace-wide — every crate needs a readme or the workspace lints adjusted). Known limits recorded for later: no narrowing through mutable fields of union-typed values; linearity and effect rows remain for M2.
2026-11-09 (gates), workspace clippy cargo metadata fixed: every crate now inherits `readme.workspace = true` (workspace.package points at ../../README.md), so `cargo clippy --all-targets -- -D warnings` passes on both stable and nightly without `-A clippy::cargo` - the missing package.readme had been failing all 9 crates. Investigated the nightly rustfmt stderr noise (`single_line_if_else_max_width cannot have a value that exceeds max_width` at value 100 == max_width): it is a nightly rustfmt quirk fired only while walking certain module files (bisected to ds-abi/uop/encode.rs's full parse, reproduces with any config carrying the two keys), formatting output is unchanged (the value is clamped to max_width as declared) and `cargo fmt --all --check` exits 0, so it is left as-is; lowering the value would reformat working single-line ifs and shrink the style.

2026-11-09 (round 1, M2 closed + runtime contract extension), landed in parallel worktrees and merged: (1) ds-check linearity [C1, C5] — one intraprocedural affine pass computing definite initialization, moves, and deinit shape together; DS3001 two-span moved-here/used-again (single-span variant on re-executing paths), DS3002 uninitialized, DS3004 deinit-shape; copyable subtraction (int/float/bool/string; structs linear iff ~Copyable marker or non-copyable field, cycle-guarded; [t: ~Copyable] bounds propagate); the pass exposes Linearity{moves, drops, last_uses} as the facts the M3/M4 lowerer consumes (moves = consume transfers, drops = scope-exit deaths incl. assignment replacement, last_uses carries a borrowed flag); lambda/spawn bodies capture-move outer bindings (epoch-based). LinearityCheck is a real query compute but invalidates at file grain (package fixpoint); the block-grain seam stays for the lowerer. (2) ds-check effects [E1] — Effect {Error, Trap, World, Cancel, Discontinue}, EffectRow bitset with union/minus/contains/render (empty renders `pure`), rows = declared row (ambient DeclaredRows until M7 platform seams) union intrinsics union call edges, solved to a least fixpoint; ?-propagation contributes error, or/.err/.ok mask the operand's error at the edge, nursery contributes cancel|discontinue; EffectsOf is a memoized compute keyed by DefId. (3) ds-abi/ds-vm runtime contract extension — KSTR (interned string Ref), EQSTR/NEQSTR (content equality), STRCAT (fresh concat), RETSPAN (multi-word span returns through the physical overlay; RET stays one-word), SlotType::Raw for mixed-shape overlay words (wire tag 6, additive; callers' arg_area_len must cover max(nargs, callee RETSPAN width)); builtins as a reserved native module `builtin` addressed by ordinary FuncRef {module, func} through the normal CALL/CALLT path (0 println, 1 print, 2 str_int, 3 str_float, 4 str_bool); strings are Value::Ref into a per-VM object store, no reclamation until the M4 Perceus pass (ds-rt arenas deliberately unused: interned constants outlive frames). Golden fixtures in ds-vm/tests/types_hybrid.rs pin the lowering conventions: unions as uniform-width tag+payload spans with per-member payload offsets, or-return/? as TAG_TEST+JMPIF+RETSPAN, interpolation as str_*+STRCAT chains. Open for ds-lower: mixed-payload unions (rebox minority payloads to Ref vs per-member offsets — fixtures demonstrate offsets). Pitfalls: the M0 framing assumed one call shape per frame, which heterogeneous staging (println(str(x))) broke until SlotType::Raw; two vm tests used major 0xD as reserved — it is now assigned, reserved moved to 0xE/0xF; Ref run-arguments must be interned by the executing Vm instance; reserving 0xD-style majors in fixtures is itself the anti-pattern. Process: parallel-agent protocol addendum — agents must never run git in the main checkout (an uncommitted stray iteration there blocked merging until detected and discarded); coordinator merges land as merges and the fix commit amends the merge before agents rebase onto it (agent worktrees rebased by reset --hard from a clean status, untracked files survive, plus a steer notice).
