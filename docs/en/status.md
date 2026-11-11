# DreamScript status

What the tree runs today and what boundary each milestone left. Landed history lives in git; the ranked residual work lives in `docs/en/todo.md`; the milestone scope and acceptance criteria live in `docs/en/implementation-plan.md`.

| Milestone | State |
|---|---|
| M0 Contracts and bootstrap | done 2026-11-09 |
| M1 Front end | landed: lexer, parser, block-index queries, `ds-fmt`, conformance corpus, parser fuzz gate |
| M2 Checker core | done 2026-11-09 |
| M3 Lowering and tier-1 VM | core done 2026-11-09; the lowered subset below |
| M4 Memory | done 2026-11-10 at the compiler level; per-fiber heaps and arena temporaries continue in `docs/en/memory-arc.md` |
| M5 Concurrency | in-language nursery/spawn surface landed 2026-11-12 over the substrate; `par_map`/`par_reduce` open |
| M6 Derives, comptime, reflection | not started; the dispatch surface it builds on runs |
| M7 Modules, packages, FFI, tooling | not started |
| M8 Tier 2 and tier 3 | not started |
| M9 Hardening and 1.0 | not started; three fuzz legs and the tutorial exist early |

## The lowered subset

`ds run` compiles in-process (parse, static check, Perceus dup/drop placement, lowering) and executes on tier 1. What lowers today:

- Bindings and control: `var` locals with plain and compound assignment, `var` destructuring (one re-storable slot per element word, a `const` binder aliasing the source words), union-annotated `const` bindings re-spanning a bare member into the destination union, `while`/for-range/for-in loops with `break`/`continue`, `return` in any statement position, script entry folding an entry file's top-level statements into a synthesized `main` (a library parse rejects them, DS1014).
- Calls and values: monomorphic UFCS dot-calls; lambdas capturing and non-capturing, named fn values, callable-defaulted fn parameters (captures copy by value at closure construction); call-site argument widening into union-typed parameters through a staging temp; an omitted defaulted member parameter staged at the call site; list and map literals and constructors with `list_push`/`map_*`; `map`/`filter`/`reduce` and the container closes through CALLG with interpreter reentry; anonymous tuples with destructuring.
- Unions, errors, tests: `.ok`/`.err`/`?` extraction over the success-typed ambient with fallible native spans re-spanned into the call-site union, the failure bit pairing by the checked union's member shapes; `is` narrowing over written fn types and multi-atom `is` tests over written unions; `match` over unions (tag dispatch) and over non-union values (per-element constant test chain with guards and a join reconcile); interpolation auto-`str`; char and byte literals.
- Ownership effects: declared `deinit` blocks run at the value's unique death (scope exit, early return, `?`, overwrite, frame exit) through the rc_bind hook, with the five honest refusals tracked in `docs/en/todo.md`.
- Dispatch: trait call sites through CALLG over impl member bodies; generic bodies lower once per impl with runtime evidence staged by the entering call site, a nested bound call transferring through the frame's evidence slot and a lambda inside the body carrying its demands in its capture block; inherent dot-calls dispatch as plain local calls under `<target>#<member>`, a generic inherent impl's entering site staging its per-bound evidence through the ordinary CALL/CALLT. A dispatch tail (a trait site, a nested sentinel site, or the root frame's generic call) transfers through CALLTG — the CALLG resolution over the CALLT frame-reuse slide, keyed by the CALL-site inline cache with the full-lookup fallback, its evidence/instance words staged after the exit claims; a closure-staging tail whose callee word the scalar-provenance tracking proves count-free transfers through CALLT, the maybe-Ref words (a callable-defaulted parameter, a closure's Ref) keeping the in-place CALLS tail.
- Natives: the pure `builtin` core in every VM plus the `std` platform module (console, file, net) installed by the CLI; a total spelling over a fallible native prints `io|net error: <msg>` and halts.
- Runtime: `ds-rt` is the consumed runtime since the stage-one block migration — ds-vm drives ids and counts through its block store (alloc_store, rc_inc_raw, rc_dec_count, rc_dispose); the arena half stays dormant until arc stage three; the `diag` feature is the only balance-tripwire layer.

The boundary of this subset and every open seam are listed in `docs/en/todo.md`.

## Early legs of later milestones

- M9 fuzz: the checker generator leg (fuzz_check_gen over the shared generator, 240-seed nextest mirror in CI, an 8-worker ASan campaign of 95,758 executions with no panics) and the VM execution leg (generator template plus host driver, 240-seed fiber mirror; the first sweep found and fixed two joiner-re-entry panics).
- M9 docs: `docs/en/tutorial.md` exists as the explainability-budget proof.
