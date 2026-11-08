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
- Fuzz (cargo-fuzz): ds-mod reader (M0), lexer/parser (M1), checker generator (M2), VM differential (M3); overflow checks and the diagnostic layer (rc==1 checks, borrow counter) stay on in fuzz and CI builds.
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
| M1 | Front end (ds-parse, ds-query skeleton, ds-fmt, conformance corpus) | in progress (corpus, ds-query engine landed; ds-parse in flight) |
| M2 | Checker core (ds-check, diagnostics, negative suite) | in progress (type core B1, trait instance env B6 landed; checker awaits ds-parse) |
| M3 | Lowering and tier-1 VM (ds-lower, ds-vm, `ds run`) | prep landed (uop set contract in ds-abi, dsmod blocks payload, bench harness; VM uop migration in flight) |
| M4 | Memory (ds-rt: arenas, Perceus RC, reuse, deinit) | not started |
| M5 | Concurrency (fibers, nursery, cancellation, fork-join) | not started |
| M6 | Derives, comptime, reflection (ds-meta) | not started |
| M7 | Modules, packages, FFI, tooling (ds-pkg, ds-ffi, REPL, hot reload, LSP) | not started |
| M8 | Tier 2 copy-and-patch and tier 3 (ds-jit, SSA, differential tiers) | not started |
| M9 | Hardening and 1.0 (fuzz gates, perf gates, tutorial) | not started |

## Progress log

2026-11-09, M0 done in one round: workspace bootstrap (glob members `crates/*`, shared lints/profiles, CI on windows+ubuntu: nightly fmt, stable clippy+nextest), then ds-diag (spans, 24-code DS registry, human+JSON renderers), ds-abi (locked frame layout, CALLT slide plan with memmove-safe direction, two-word entry descriptors with CAS flip, tail-chain counters), ds-mod (G3.5 format: strict-canonical reader, byte-stable writer, blake3-128 checked hash, mmap loader, dormant collector hooks; cargo-fuzz target ran 30k execs clean), ds-vm skeleton (provisional 13-op set, two-slot module registry with retiring-slot liveness, CALLT per the locked ABI: mutual even/odd at depth 10^7 holds a 7-word constant stack). M1 partially landed in parallel: conformance corpus (sections 13.1-13.6, 59 items, 1 unresolved v2 handler kept as comment) and the ds-query engine (red-green renewal gives block-grain invalidation; placeholder computes await ds-parse wiring). Pitfalls recorded: rustfmt.toml unstable options force nightly fmt (CI fmt job pinned to nightly); clippy's cargo lint group demands full package metadata, so every crate must inherit workspace.package fields; parallel-agent protocol that produced zero merge conflicts: per-agent git worktrees + branches, additive-only root Cargo.toml edits, shared CARGO_TARGET_DIR, Cargo.lock conflicts on rebase resolved by delete + `cargo check` regen. Record addendum 6.1 pins the corpus surface normalizations; the linear two-span error example code moved to DS3001 to match the shipped registry's class partition.

2026-11-09 (later), contracts hardened and M2/M3 prep landed: a read-only cross-crate audit caught a P0 in ds-vm's CALLT (wide-callee params slid through caller body_len — a 3-param callee read (10, 0, 20)); fix 10e2ad8 pins owner-relative word mapping (callee param i at fp+1+i), now also stated in ds-abi's locked docs with a CALL/CALLT identity table. Audit P1/P2s all fixed: dsmod header commits BL-7's max_call_depth + ir_checksum fields (format v2, values pending until M3), reader is strictly canonical (gapless section tiling), ds-diag pins the full 27-code snapshot table, weekly CI fuzz cron runs the ds-mod mirror (FUZZ_RUNS knob). Contracts grew: ds-abi now defines the BL-5 uop set (39 ops, canonical 32-bit encoding, BIAS_J=0x8000, XOR-negated comparisons, slot-effect/purity tables; 13.7 programs encode exactly), ds-mod defines the blocks payload v1 over uop words + FrameLayouts (every code word validated by decode at read), ds-check landed the B1 type core (canonical atom-set solving, exhaustive small-universe law tests vs a bitset oracle) and the B6 instance environment (union-head decomposition, witness-carrying overlap errors at declare and link, DFS condition-cycle rejection, no orphan rule). Discipline that worked: ErrorCode additions update the code-table snapshot in the same commit; coordinator rebases agent branches whose base moved. Still open: ds-parse is the long pole gating the M1/M2 wiring wave.


