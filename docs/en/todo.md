# DreamScript open work

The queue is ranked: item 1 is what the next round takes unless the author redirects, and a lower item moves up only when the ones above it are closed or blocked. Landed history lives in git; `docs/en/status.md` carries what runs today and the boundary of each milestone, while `docs/en/implementation-plan.md` carries milestone scope and acceptance criteria.
Every item names its owning milestone then its effort tag (S, M, L per round). Author rulings are cited with their date and decision ID; questions marked as open are waiting on the author, and the refusal items are design decisions rather than gaps to paper over.

## Sequenced next (author-ruled)

1. [M4, L] Memory endgame arc: adopt ds-rt in three stages — slab entries become headered blocks behind the unchanged Ref id, then per-fiber heaps over the fiber pool, then arena temporaries with the placement pass deleting count traffic; author-ruled 2026-11-12 on the long-term view, sequenced before surface rounds resume, stage one already carrying every count through ds-rt while the arena half stays dormant; the execution contract is `docs/en/memory-arc.md`.
2. [M3, L] Inline multi-word spans, the native-channel and heap-list slice (union region spans, the width flips over struct-typed fields, and per-span claim accounting landed 2026-11-12): the `NativeResult` span channel unlocking `list[record]` elements, checked-read wider sides, `partition`, and tuple union members, then the inline-payload heap list at a per-list element stride — the tutorial's collections-of-records blocks 17/18/25 stay out of reach until it lands; boxing stays rejected (per-field indirection the JIT cannot remove, no unboxing entry in the optimization queue, BL-2's unboxed ADT story is the terminus).
3. [M6, M] Evidence across spawned-child frames: a spawn's entry argument area carries no evidence words, so a bound-demanding site inside a spawned child keeps the honest refusal; the lambda crossing landed through capture-block words.
4. [M6, M] Generic-impl transitive staging: a nested dispatch whose serving body is itself a generic impl's member refuses, because the entering site's substitution cannot name the second body's own evidence demands; the enclosing body's demand walk must also collect every demand lexically inside its lambdas, since the construction site looks the words up by pair.
5. [M6, S] Per-branch evidence: a receiver unioning type variables across match branches has no single serving body.
6. [M6, S] A checked read over a type-variable element refuses naming runtime reflection.
7. [M6, S] Mutable var-typed bindings have no overwrite contract.
8. [M2, S] Trait-member default authority (F7, ruled 2026-11-12): an impl body re-declaring a defaulted parameter must become a checker error so the staged literal has exactly one checked source, and a dispatch call stages the trait's declared default at the call site like the free-function spelling; inherent members keep their own body defaults; today an omitted defaulted parameter at a trait-method call still refuses at lowering.

## Questions awaiting the author

9. [M2, S] Checker expression-error epoch: is poison subtree-scoped or environment-scoped? A poisoned initializer bound to a name still prints one follow-on at a new use site, and threading the epoch through Binding doubles the state surface.
10. [M3, S] Destructured const binders alias a var's words, so a reassignment through the var is observable through the binders; the record ruling has to decide whether that alias is intended or the binders must re-read per access.
11. [M3, S] Whether native fn values should ever mint a callable word; today only planned module functions do.
12. [M7, S] The ambient `enum net_err { timeout, refused }` pre-commits two runtime case tags against the platform's single NET_ERR discriminant, so a future fetch native must answer both or the enum gets re-shaped before M7.
13. [M1, S] Doc-comment prefix design (G3): `///` is the reserved prefix and reads as trivia today; whether doc comments attach to definitions is undecided.
14. [M2, S] B1 per-element value-level refinement stays a possible extension owned by the checker queue (a value-level singleton subtracted from later arms); the 2026-11-12 value-match remainder ruling subtracts nothing and rejects the arm-ordering constraint the empty-remainder typing produced.

## Lowered-surface slices

15. [M3, M] Tuple and guard residual slices: a chain that can fail (any literal test or guard) refuses without one unguarded catchall arm, because value-level tests and guards prove no coverage and the fall-through stays open, which the checker's tuple-arity coverage cannot supply; guards over union dispatch refuse (the tag dispatch precedes any arm body, so a guard's fall-through needs the dispatch reordering); or-patterns and struct patterns over non-union values refuse; a guarded arm over a deinit-carrying scrutinee refuses (the binder copy would anchor a second claim); a guard consuming the scrutinee's own Ref word refuses (later entries still test it).
16. [M3, M] mut/move parameter modes do not lower.
17. [M3, M] Block-argument sugar does not lower.
18. [M3, S] Named arguments lower at enum case construction only.
19. [M3, M] Multi-atom uses without `is`/match splitting refuse.
20. [M3, M] Pinned callable-word refusals: var-span widening, a lambda capturing a closure word, a lambda capturing a callable-defaulted binding, callable collection elements and struct fields, and native/generic fn values minting callable words; captures copy by value at CLOSURE_NEW, so mutable-capture shapes stay out of the kernels by construction.
21. [M3, S] `partition` refuses: its tuple result needs the multi-word span channel in the native path, owned by the span rework.
22. [M3, S] The combinator reboxing refusal calls a named-fn argument a "lambda" in ds-lower; the message should name the callable.
23. [M4, M] Five honest deinit refusals: closure-captured words, union payload fields, native/generic arguments, tail arguments, and generic-struct deinit refuse at lowering; owned by the arc's span stage (`docs/en/memory-arc.md`).
24. [M4, S] Host-abandoned words and the root frame's tail-native abandon leak by design: a root frame's native tail may CALLT with abandoned staged counts, so the leak is the accepted cost of that transfer.
25. [M5, M] Fork-join combinators: `par_map`/`par_reduce` (E3) over the landed nursery substrate — ownership transfer at fork keeping RC counts thread-confined, cheap fork-side snapshots via static RC counts, deterministic replay, scheduler backend decided by measurement.
26. [M5, M] A cancel-cut child frame's captured counts await the teardown deinit pass.
27. [M5, S] Payloaded `cancel` values and unions over generic applications wait on the arc's span rework (`docs/en/memory-arc.md`).
28. [M5, M] A child yielding inside an infinite loop under a join drive hangs the driver by construction, so the language surface has to bound it.
29. [M5, S] CALLT pc-reset is deliberately outside the preemption points, so a self-tail-loop never observes cancellation.
30. [M7, M] Platform seam (H5.5, E1): the world-touching natives still run through the M3 stopgap registry; the effect-handler form behind the platform declaration lands at M7 and retires the `builtin`/`std` two-native-module split, the pure `builtin` core staying.

## Plumbing and ambient

31. [M3, S] The module-id space stays registry-assigned interim until the ds-mod link table assigns indices.
32. [M3, S] The CALL inline cache retains swapped-out hot-reload versions until eviction (bounded, memory only).
33. [M3, S] KSTR intern rows for dead pools are never reclaimed.
34. [M3, S] Placement keeps a loop-carried word's count from its textual last use in-body until the next overwrite or exit (the loop-claims stack's deliberate extension); an accepted over-approximation.
35. [M5, M] Staged Ref counts stay with interrupted fibers' kept frames awaiting the deinit walk.
36. [M5, S] The cross-nursery join interlock recursion is open.
37. [M7, M] Ambient taxonomy: the unnameable set is parse_json/open/read_exact, whose signatures must name file-owned types (`json`, the caller's `~Copyable` file struct), which waits on M7 package-owned types; https stays rejected in the net natives until the M7 platform decision.

## Verification slices

38. [M9, S] fuzz_vm coverage boundary: file/net natives stay unreferenced (execution sealed, only the captured-output console pair is reachable) and the run limit stays stack words because the VM has no step budget, so termination rests on the generator's invariants — every backward edge passes the loop template's decrement exactly once, the spawn tree stays depth one, every switch re-enters a parked fiber at pc+1 — and any relaxation of the loop template must keep them, the emit path's forward-target retargeting invariants being the re-audit point if the uop ABI changes.

## Queue maintenance

An item moves or leaves once its residual clauses land: the closed part becomes git history, a surviving clause stays as its own numbered line, and a new residual from the same round takes its place in rank order. This file carries open work only, so it never restates milestone scope, acceptance criteria, or the milestone boundary.
