# Docs-to-code: which runtime/memory invariants still need a code-site comment

Read-only survey of `/root/programs/DreamScript`. Nothing in the repo was modified, no build/test was run.

Boards audited: `docs/en/memory-arc.md` (25 entries: 17 binding-register, 4 gates, 1 silent-breakage, 1 STOP, 2 baselines), `docs/en/pitfalls/runtime-memory.md` (80), `docs/en/pitfalls/lowering.md` (96), plus `docs/en/pitfalls/testing-and-perf.md` (45) where it overlaps the runtime boards. Total 246 entries.

## 1. Headline

The tree already carries these invariants *at the site that enforces them*. Of the 25 memory-arc entries, 16 are IN-CODE-ALREADY, 1 is DOC-ONLY (the refusal/audit roster) and 8 are DOC-ONLY by nature (gates, escalation, measurements); of the 176 pitfall entries, the ones I checked (roughly 60, weighted to the newest and the highest-consequence) restate a code comment nearly verbatim. The pitfall prose reads as a transcription of the code comments, not as their source.

So the answer to "which invariants live only in docs" is: **almost none of the behavior, and all of the process.** The docs' irreducible content is (a) which gates must stay green and what they cover, (b) the escalation policy, (c) measured Ir/wall baselines, (d) author rulings captured as rejected alternatives, and (e) fixture/gate-runbook facts. Relocating any of that into code would either rot (numbers) or duplicate (gate lists).

6 entries are genuine MOVEs (§6) — one of them a doc-side shrink — plus 1 DEDUPE-shrink. The reverse direction is the more actionable half: **two doc entries and one code comment encode stale numbers** (§7).

## 2. Method

For each entry I located the owner by its distinctive noun (`ClaimTable`, `produced_strcat`, `KSTR_BORROW`, `STRCAT_ARENA`, `CAPTURE_STORE`, `ARENA_MARK`, `ElementStride`, `ForeignMapKey`, `ReentrancyRefused`, `RetSpanTooWide`, `union_shape`, `slide_plan`, `settle`, `id_for_install`, `rearm_step`, `take_ready`, `wake_chan_waiter`, `bound_traits`, `lens_drift`, `comptime_loop_info`, `row_admits`, `staging_base`, `PRIMITIVE_WORDS`, `word_shaped_ambient_atom`, …) and read the surrounding 40 lines. Verdicts:

- IN-CODE-ALREADY: the code site states the operational rule (cite `file:line`).
- DEDUPE: both places state it; the doc entry can shrink or drop.
- MOVE: doc-only behavior; code site silent or weaker; proposed comment given.
- DOC-ONLY: process, gate inventory, measurement, author-facing/rejected-alternative.

Comments proposed below are behavior-only, name the failure they prevent, and cite no document.

## 3. memory-arc.md — Binding register (17)

| # | entry | verdict | target file:line | proposed comment |
|---|---|---|---|---|
| 1 | Per-fiber heaps, slot keeps its store, foreign-held object outlives recycling | IN-CODE-ALREADY | `crates/ds-vm/src/value.rs:167-173`, `crates/ds-vm/src/vm.rs:3978-3984` | — |
| 2 | Ref word `(fiber u32, id u32)`, `Value` 16 B, no dsmod impact | IN-CODE-ALREADY | `crates/ds-vm/src/value.rs:84-88,160-175`; size asserted `crates/ds-vm/tests/stage_two_evidence.rs:34` | — |
| 3 | `RcHeader.flags` reserved for the dormant collector | IN-CODE-ALREADY | `crates/ds-rt/src/rc.rs:57-60` | — |
| 4 | Claim ledger per pool slot; foreign rows key by whole word | IN-CODE-ALREADY | `crates/ds-vm/src/claims.rs:1-11,190-235` | — |
| 5 | Establishment band outranks the ledger (Ref-free spans fire once, dead claims refuse, moved claims silent) | IN-CODE-ALREADY | `crates/ds-lower/src/place.rs:62-76`; `crates/ds-vm/src/claims.rs:40-49` | — |
| 6 | Executor principle: count mutations only through the owning heap's funnels, foreign posts drained to quiescence | IN-CODE-ALREADY | `crates/ds-vm/src/object.rs:44-54,2355-2375,2536-2548` | — |
| 7 | Map row keys (`ForeignMapKey` before any count moves) | IN-CODE-ALREADY | `crates/ds-vm/src/object.rs:2003-2022,2079-2087` | see MOVE-4 (seam rationale) |
| 8 | Counts-stay: host deliveries release, fiber deliveries transfer; rendezvous claim lands on resumer/joiner at `die_current` | IN-CODE-ALREADY | `crates/ds-vm/src/vm.rs:7527-7550`; `crates/ds-vm/src/object.rs:2536-2548` | — |
| 9 | Teardown drive rule (walk decs against the word's own heap; hook must return to its driver → `ReentrancyRefused`) | IN-CODE-ALREADY | `crates/ds-vm/src/vm.rs:56,3310-3314`; `crates/ds-vm/src/object.rs:2355-2375` | — |
| 10 | CLOSURE_NEW's per-capture acquire stays inside the op (fuzz-mirror count noise) | IN-CODE-ALREADY | `crates/ds-vm/src/vm.rs:10586-10597` | DEDUPE: doc may shrink to the ruling line |
| 11 | Intern tables per heap; `kstr_cache` fiber axis; rows reclaimed at retiring swap and slot reuse; global pool rejected | IN-CODE-ALREADY (behavior) | `crates/ds-vm/src/vm.rs:3088-3096,3960-3990`; `crates/ds-vm/src/object.rs:833-842` | rejected-alternative half is DOC-ONLY |
| 12 | KSTR borrow seam (`produced_kstr`, `finalize` rewrite, paired terminal RCDECs deleted, `intern_borrow` pins) | IN-CODE-ALREADY | `crates/ds-lower/src/place.rs:355-372,390-400`; `crates/ds-lower/src/func.rs:8995-9012`; `crates/ds-vm/src/object.rs:833,3834` | — |
| 13 | CAPTURE_STORE window (one edge acquire before the swap, release after, self-store safe, routed by owner) | IN-CODE-ALREADY | `crates/ds-vm/src/vm.rs:10689-10710` | — |
| 14 | STRCAT arena seam (`produced_strcat`, `STRCAT_ARENA`, entry mark, popped at RET/CALLT) | IN-CODE-ALREADY | `crates/ds-lower/src/place.rs:363-372`; `crates/ds-lower/src/func.rs:9006-9020`; `crates/ds-vm/src/vm.rs:10573-10615` | — |
| 15 | `ARENA_MARK`/`ARENA_RESTORE` bracket, one I64 cursor; un-hostable frame keeps counted spelling | IN-CODE-ALREADY | `crates/ds-lower/src/func.rs:353-356,2656-2672,9017-9032`; `crates/ds-vm/src/vm.rs:10851-10880` | — |
| 16 | Packed handle `chunk:16|offset:24|len:24`, named refusals, length trap before allocation | IN-CODE-ALREADY | `crates/ds-vm/src/value.rs:80-122`; `crates/ds-vm/src/vm.rs:10588-10600` | — |
| 17 | Dispatch layout: raw opcode byte keyed, one slow arm via decoders, threaded side-table rejected | IN-CODE-ALREADY (behavior) | `crates/ds-vm/src/vm.rs:9517-9525,9808-9815,10180-10190` | rejected-alternative half DOC-ONLY |
| 18 | Span shapes: `union_shape` lowest free contiguous span, `slide_plan` pairwise | IN-CODE-ALREADY | `crates/ds-lower/src/shape.rs:925-935`; `crates/ds-abi/src/tailcall.rs:163-186` | — |
| 19 | `NativeResult::Span` 0..=n, `RetSpanTooWide`, `Total` no discriminant, `Staging` discipline, 4-word inline channel | IN-CODE-ALREADY | `crates/ds-vm/src/vm.rs:272-282,784-800,2637-2660,909-920`; `crates/ds-abi/src/layout.rs:145-159` | — |
| 20 | Arena payload is a third `Value` domain; escape proof compile-time; no RCINC/forward/capture/exit/loop-carry | IN-CODE-ALREADY | `crates/ds-vm/src/vm.rs:2131-2139,2150-2160`; `crates/ds-lower/src/place.rs:40-62,150-160` | — |
| 21 | Element spans at a per-list stride; `ElementStride` on disagreement | IN-CODE-ALREADY | `crates/ds-vm/src/object.rs:950-975,1340-1360,1590-1605` | — |
| 22 | Refusal roster (reboxing, union/deinit elements, tuple arm, multi-word tuple elements, deinit channel payload) + terminal-RCDEC audit | DOC-ONLY | — | design roster + audit verdict; the enforcement sites already refuse by name (`LowerError`/`unsupported`), and `crates/ds-lower/tests/deinit_e2e.rs` is the pin |

## 4. memory-arc.md — Gates / silent-breakage / STOP / baselines (8)

| entry | verdict | reason |
|---|---|---|
| Fibers and nurseries suite list, `effects_nursery.ds`, `nursery_surface.ds`, diag-armed release run | DOC-ONLY | gate inventory + runbook; no single code owner |
| Teardown deinit: exactly one fire per established claim, band-gated (`teardown_deinit` corpus) | DEDUPE | the rule is `crates/ds-lower/src/place.rs:62-76`; the doc line is the gate name — keep one line |
| Arena temporaries gate list (strcat_arena, placement pins, zero-RcOps, kstr_borrow, native_frame_balance) | DOC-ONLY | test inventory |
| Spans gate list (shape pins, width-flip e2e, fold/tuple e2e, byte-identical corpus) | DOC-ONLY | test inventory |
| Silent-breakage register (half-done stage two frees early; half-done span rework overlaps words; half-done stage three deletes a live dec) | DOC-ONLY | the fail-if-partial framing is a change-review note; the invariant itself is already stated at `crates/ds-lower/src/func.rs:9006-9012` ("the classification survived the whole walk, so no path moves a deleted count") |
| STOP: dsmod format change, `RcHeader` layout change, cross-fiber count mutation, cross-heap counted KSTR constants | DOC-ONLY | escalation policy, author-facing |
| Gungraun pins: string_temp 8,205,025,167 / 29,205,020,550; strcat_temp 11.79G / 24.45G; restore footprint | DOC-ONLY **and STALE** | see §7.1 |
| `scalar_loop` is the absolute-Ir pin; wall gates ±2% | DOC-ONLY | measurement policy |

## 5. pitfalls/runtime-memory.md (80) — grouped

Nearly every entry names its own function, and that function's doc comment already says the same thing. Representative citations; a cluster is "IN-CODE" when each member I read had its statement, and DEDUPE when the wording is near-identical (so the doc line may shrink).

| cluster (entries) | verdict | owner |
|---|---|---|
| Arena grants/aliasing contract, `RestoreOnDrop`, miri leg | IN-CODE | `crates/ds-rt/src/arena.rs:72-80`; `Mark` LIFO doc at `:38-44` |
| `UnsafeCell` re-tag hazard | IN-CODE | `crates/ds-rt/src/lib.rs` cell docs + guard |
| ds-ffi 64-bit narrowing contract | DOC-ONLY | a port-plan warning, no 32-bit site exists to annotate |
| Install natives+platform before `loader::install`; `with_limit` installs builtin core only | IN-CODE (partial) | `crates/ds-vm/src/vm.rs:3302-3311` states the core-only half; the ordering half lives at the CLI run paths — see MOVE-5 |
| World call addresses the `platform "..."` name → `UnprovidedPlatformOp` | IN-CODE | `crates/ds-vm/src/native.rs:164-206` |
| Bound-artifact tier installs before the module | IN-CODE | `crates/ds-vm/src/module.rs` bound-table docs + `Constant::Bound` validation |
| Fork-join snapshot strips the bound tier and platforms | IN-CODE | `crates/ds-vm/src/forkpool.rs:17-21,633-649` |
| `id_for_install` / never renumber ids | IN-CODE | `crates/ds-vm/src/module.rs:702-721` |
| Reserve ids from both ends (unions up from 0, platform down from 255, builtin 0-48) | IN-CODE | `crates/ds-vm/src/native.rs:164-206,3560-3600`; `crates/ds-vm/src/shape.rs` tag counter (`:425-450`) |
| New pure builtin lands in four places | DEDUPE | `crates/ds-vm/src/native.rs:3587-3641` (table order contract) + the ambient regeneration notes; the "four places" checklist itself is process → keep in docs |
| Fallible native two-word span, `io|net error:`, CALLT-to-fallible Unsupported | IN-CODE | `crates/ds-vm/src/native.rs:2476-2500`; `crates/ds-vm/src/vm.rs:12790-12800` |
| Net failure classes / `net_err` spelling | IN-CODE | `crates/ds-vm/src/net.rs:1700-1720` |
| Teardown door over another live fiber: park and re-adopt | IN-CODE | `crates/ds-vm/src/vm.rs:8760-8780` |
| Native borrow window opens before operand eval, pops at flush; one window per `native_done` | IN-CODE | `crates/ds-lower/src/place.rs:30-36` |
| Hosted calls inc Ref args; `DriveOutcome::Frame` sentinel u32::MAX; only root native tail may CALLT | IN-CODE | `crates/ds-vm/src/vm.rs:8340-8360,8700-8712` |
| Two spawn closure forms stage separately | IN-CODE | `crates/ds-lower/src/func.rs:917-930` |
| Fork-join completion acquires the receiving claim row before the joiner's dec | IN-CODE | `crates/ds-vm/src/vm.rs:5520-5545,5860-5870` |
| Dead-context drive routes through the pool (`ctx_live`) | IN-CODE | `crates/ds-vm/src/vm.rs:2137-2150,6319-6344` |
| Debug yield answers a raw word (second park `Tag where I64`) | IN-CODE | `crates/ds-vm/src/vm.rs:7908-7917` |
| List copy natives copy into the one adopted `Vec` | IN-CODE | `crates/ds-vm/src/native.rs:6380-6400,6480-6500` |
| Re-entry of a parked child is a preemption point; CALLT pc-reset outside observation points | IN-CODE | `crates/ds-vm/src/vm.rs:4980,5160-5180` |
| Nesting a drive uses the interlock stack, 2048-level pin | IN-CODE | `crates/ds-vm/src/vm.rs` interlock docs; pinned by `tests/nursery_interlock.rs` |
| Teardown hook must return to its driver (`ReentrancyRefused`), never cut the driving fiber | IN-CODE | `crates/ds-vm/src/vm.rs:56,3308-3314` |
| Claim directory from the body's drop shape; band tags above `FIRST_RESERVED_TAG` | IN-CODE | `crates/ds-vm/src/claims.rs:40-85,103-140` |
| Borrowed registration/alias never anchors a claim | IN-CODE | `crates/ds-lower/src/place.rs` pinned/binder docs + `deinit_e2e` |
| Fn-classified widening proves a non-capturing scalar (`scalar_fn_words`) | IN-CODE | `crates/ds-lower/src/func.rs:5840-5900,6510-6520` |
| Generic trait bodies lower per impl; refused chains | IN-CODE | `crates/ds-lower/src/func.rs` instance/subst docs; `tests/` pins |
| `Raw`-staged words admit a `Raw` callee parameter | IN-CODE | `crates/ds-abi/src/tailcall.rs:174-186` |
| Captures copy by value at CLOSURE_NEW; callable-word refusals | IN-CODE | `crates/ds-vm/src/vm.rs:10586-10600`; `crates/ds-lower/src/func.rs:293-300` |
| Module-id space is registry state (`tests/module_ids.rs`) | IN-CODE | `crates/ds-vm/src/module.rs:702-721` |
| One pending-foreign queue site per store, `settle` to quiescence | IN-CODE | `crates/ds-vm/src/object.rs:2355-2400` |
| Call-site layout facts ride the inline-cache entry under (caller, pc, stamp) | IN-CODE | `crates/ds-vm/src/vm.rs:671-686` |
| `VersionIdx` rows own survival; sweep judges the detached list; reuse freelist | IN-CODE | `crates/ds-vm/src/module.rs:651-661,729-742` |
| Swap re-check's two judges never merge | IN-CODE | `crates/ds-vm/src/vm.rs` swap re-check docs |
| Dying frame's hook drives through its own version row; trapped release drains first | IN-CODE | `crates/ds-vm/src/vm.rs:4633-4647` |
| Open nursery dies with its dead owner; cancel-cut runs the same teardown | IN-CODE | `crates/ds-vm/src/vm.rs:4940-4990` |
| Peek-next fusion's three invariants | IN-CODE | `crates/ds-vm/src/vm.rs` fusion arm docs; `tests/load_op_fusion.rs` |
| Callee answering an unwritten word reads the previous occupant (frames never zeroed) | IN-CODE | `crates/ds-vm/src/vm.rs:1519-1525,7908-7913` |
| Opcode-arm placement re-lowers neighbours | IN-CODE | `crates/ds-vm/src/vm.rs:3230,10180-10190`; `crates/ds-pins/tests/pin_bake.rs:15-20` |
| Host run entry answers the whole span (`FrameAnswer`, death delivery per Ref) | IN-CODE | `crates/ds-vm/src/vm.rs:7908-7917` |
| Reflection table thread-local | IN-CODE | `crates/ds-vm/src/comptime.rs:75-92` |
| Recycled pool slot vs nursery children list; homecoming death walk | IN-CODE | `crates/ds-vm/src/vm.rs:5460-5530`; `crates/ds-vm/src/forkpool.rs:60-80` |
| Map foreign-key refusal at the store seam | IN-CODE (behavior) / MOVE-4 (why the seam) | `crates/ds-vm/src/object.rs:2079` |
| Class pools are intrusive freelists; heap drop walks every row | IN-CODE | `crates/ds-rt/src/heap.rs` pool docs + `Heap` drop |
| ds-rt raw-store tests must `rc_dec_count` + `rc_dispose` | DEDUPE | `crates/ds-rt/src/heap.rs` drop/raw-api docs; the test-side reminder belongs in the test module doc |
| Host reporting `Err(trap)` must `release_interrupted(FIBER_ROOT)` | **MOVE-1** | see §6 |
| Seam handing words to a parked fiber must allocate in its store (`Heaps{live:false}` allocates into the scratch) | **MOVE-2** | see §6 |
| Boundary clean-join predicate (`reenter_joiner`) | IN-CODE | `crates/ds-vm/src/vm.rs:7895-7906` |
| Wake door must deliver the parked site's shape (`wake_chan_waiter`, deadline row) | IN-CODE | `crates/ds-vm/src/vm.rs:12747-12788` |
| Direct entrance of a woken fiber takes its run-queue entry (`take_ready`) | IN-CODE | `crates/ds-vm/src/vm.rs:4505-4514,5281-5290` |
| Wait registration refusal releases the context first | IN-CODE | `crates/ds-vm/src/vm.rs` `register_wait` Err branch + `crates/ds-vm/tests/net_server.rs` |
| Epoll one row per fd; cross-token refusal (EEXIST→MOD) | IN-CODE | `crates/ds-vm/src/reactor.rs:250-270` |
| Accept's IO interest resolves through the listener table | IN-CODE | `crates/ds-vm/src/vm.rs` `register_wait` dispatch docs |
| Blocking accept polls the non-blocking listener | IN-CODE | `crates/ds-vm/src/net.rs` accept twin + `set_net_timeout` |
| Close-wake goes through `Vm::wake_closed` | IN-CODE | `crates/ds-vm/src/vm.rs:6400-6420` |
| Channel waiter registers in channel queues, not the wait table; zero-bound rendezvous doors | IN-CODE | `crates/ds-vm/src/vm.rs` channel docs + `tests/chan.rs` |
| Executor eager start rescans before it waits | IN-CODE | `crates/ds-vm/src/vm.rs` executor loop docs |
| Nursery child never hands its step to the host loop | IN-CODE | `crates/ds-vm/src/vm.rs` `executor_continue` docs |
| Deinit hook needs a body drop site; channel payload rides no claim | IN-CODE (rule) / DOC-ONLY (the "until item 80 lands" residual) | `crates/ds-vm/src/claims.rs:69-85,129-145` |
| Builtin core run has three mirrors (debug assert, `native::tests`, `types_hybrid` length pin) | IN-CODE | `crates/ds-vm/src/native.rs:3600-3607` + the three mirrors |
| Callable-defaulted `Fn` slot at four sites | IN-CODE | `crates/ds-vm/src/vm.rs:1502-1508`; `write_slot` docs |
| `Procs::close` kills then reaps | IN-CODE | `crates/ds-vm/src/proc.rs` close docs |
| So_ERROR pollution under a security broker; probe with a second connect | IN-CODE | `crates/ds-vm/src/net.rs` connect-probe docs |
| rustls three teeth | IN-CODE | `crates/ds-vm/src/tls.rs` header docs |
| HUP/ERR filter, drained-pipe hangup | IN-CODE | `crates/ds-vm/src/reactor.rs:42-57,340-360` |
| Parked machine non-blocking at handoff | IN-CODE | `crates/ds-vm/src/net.rs` `start_tls_accept` docs |
| Staged machine's moved registration key (`rearm_step`) | IN-CODE | `crates/ds-vm/src/vm.rs:6609-6620` |
| Helper thread owns its fd through the shared cell | IN-CODE | `crates/ds-vm/src/net.rs` resolver job-cell docs |
| Staged connect machine takes the first connecting address | IN-CODE | `crates/ds-vm/src/net.rs` connect machine docs |
| Blocking wait twin polls on a bounded sleep loop | IN-CODE | `crates/ds-vm/src/proc.rs` `Procs::wait` docs |
| signalfd mask before any thread spawns; glibc 2.44 flags arg | IN-CODE | `crates/ds-cli/src/shutdown.rs:38-48,61-80` |
| Bounded pump caps the poll even with no deadline | IN-CODE | `crates/ds-vm/src/vm.rs:4516-4525,7002-7045` |
| World-parked fiber is the scheduler's alone | **MOVE-3** (host contract missing from `resume_fiber`) | see §6 |
| REPL pump after the read returns | IN-CODE | `crates/ds-cli/src/repl.rs:163-166` |
| A bounced resume orphans the resumee | IN-CODE | `crates/ds-vm/src/vm.rs` io_pump docs + `tests/io_sched.rs` |
| Budgeted executor answers spent, not the stall trap | IN-CODE | `crates/ds-vm/src/vm.rs:7024-7033` |
| `struct tm` row heads every CRT-written field | IN-CODE | `crates/ds-vm/src/tz.rs` |
| CLOSURE_NEW must not adopt the staged count (fuzz mirror, seed 626) | IN-CODE | `crates/ds-vm/src/vm.rs:10586-10597` |
| Race racer never entered from a host drive step | IN-CODE | `crates/ds-vm/src/vm.rs:5370-5385` |
| Deadline wheel fires only on a quiet executor; back-edge poll door | IN-CODE | `crates/ds-vm/src/vm.rs` wheel/back-edge docs; `tests/timeout.rs` |

## 6. MOVE list (6 + 1 shrink), ordered by value

1. **Host trapped-drive release duty** — `crates/ds-vm/src/vm.rs:7908-7916` (`Vm::run` doc, fn at `:7918`). The doc says the state is kept for `backtrace` and nothing about the release; `release_interrupted` (`:4633-4647`) says only "the caller reports first". Failure prevented: a host that reports and exits leaks the kept frames' hooks and claim rows (freed only at the release door, the next run's reset, or slot reuse).
   `/// A host that reports the trap must then call [`Vm::release_interrupted`] for the root:`
   `/// the kept frames' deinit hooks and their claim rows release only at a release door, the`
   `/// next run's reset, or pool-slot reuse.`
2. **`Heaps` with `live: false` still allocates into the scratch store** — `crates/ds-vm/src/object.rs:2265-2285` (`Heaps::live`). The field doc explains routing but not this. Failure: a `Ref` word minted into the stale stash names an object its fiber's store never held → `UnknownObject` at the answer.
   `/// Reads and decs route through the pool here, but allocation still lands in`
   `/// `current` (the scratch while parked), so a word a seam hands a parked fiber must be`
   `/// allocated inside that fiber's own store swap, never through this router.`
3. **A world-parked fiber is the scheduler's alone** — `crates/ds-vm/src/vm.rs:4373-4387` (`resume_fiber` doc). Only host-suspension parking is described. Failure: a plain resume enters the parked frame with the payload as the call's answer and abandons the registered wait, so the later wake delivers a stale answer onto a moved-on frame.
   `/// A fiber parked on a world wait is the scheduler's: an ordinary resume delivers the payload`
   `/// as the parked call's answer and abandons its registered wait, so the later wake lands`
   `/// stale. World-parked drives go through [`Vm::resume_fiber_io`]/[`Vm::pump_io`].`
4. **Why the foreign-key refusal sits at the store seam** — `crates/ds-vm/src/object.rs:2079-2087` (comment already says the refusal exists). Failure: a refactor hoisting the check to the router would miss direct store callers, and a foreign row is content-unfindable.
   `// the fiber test runs here, at the store, not at the router: only the store knows its own`
   `// owner, so every direct caller of this seam is covered by the one check`
5. **Install order at the CLI run paths** — `crates/ds-cli/src/run.rs` / `crates/ds-cli/src/source.rs` platform install block. `Vm::with_limit` (`crates/ds-vm/src/vm.rs:3302-3311`) documents that it installs the builtin core only; the ordering requirement itself ("natives and platform before the module, pool FuncRefs validate at install") is unstated at either call path. Failure: a pool FuncRef installs unvalidated and misroutes.
   `// every native table and the platform go in before the module: pool FuncRefs resolve`
   `// against the installed tables at install time`
6. **Lifetime of a `KstrRowSlot` across installs** — `crates/ds-vm/src/vm.rs:9460-9470` (kstr load doc). The row key/reclaim discipline is stated; the slot-cache re-resolution clause ("a row reclaimed or shifted under the frame re-resolves through the key index once") is the operative half and is present, so this is a **DEDUPE shrink**, not a MOVE: keep the code line, drop it from the doc.
7. **`word_shaped_ambient_atom` admits a bare scalar head only** — `crates/ds-check/src/resolve.rs:1659-1673` (fn at `:1668`). The set and its purpose are stated; the consequence is not. Failure: adding a tag-bearing or generic head to the set makes a `t | none` / `list[string]` member bind across a linked import instead of refusing DS4021.
   `/// The closed set is scalar words only: a head answering `t | none` or a generic head keeps`
   `/// its cross-package refusal (the import's @std equivalence arm does not admit it).`

Everything else in the three boards is DOC-ONLY for one of four reasons: gate/test inventory (which suites must stay green), escalation policy (STOP conditions, author-ruled amendments), measurement policy (pin re-baselining, A/B attribution, wall vs Ir), or a rejected alternative (`pin-counted global KSTR pool`, `threaded side-table dispatch`, `reboxing`) that no code site can own.

## 7. Reverse direction — doc entries that are wrong or stale

### 7.1 memory-arc "Measured cost baselines": every number is stale

The authoritative baseline is the committed `PINS` table (`crates/ds-pins/tests/pin_bake.rs:146-163`), which the harness verifies every release run at ±0.1%:

| doc claim | `PINS` |
|---|---|
| string_temp `8,205,025,167` Ir borrowed | `string_temp::borrowing_loads` = **4,800,897,010** |
| string_temp `29,205,020,550` Ir acquired | `string_temp::claiming_loads` = **16,910,895,225** |
| "~4,200 Ir per iteration, 3.6x" | delta / 5M = **2,422 Ir**, ratio **3.52x** |
| strcat_temp `11.79G` serving | `strcat_temp::serving_cats` = **9,000,901,854** |
| strcat_temp `24.45G` claiming | `strcat_temp::claiming_cats` = **15,850,902,295** |

A comment or doc built on those numbers would encode a lie; the doc line must point at `PINS` (or drop the numbers).

### 7.2 A code comment is stale against its own table

`crates/ds-pins/tests/pin_bake.rs:34-39` narrates: "scalar_loop's message recorded 6,895,036,857 while the pin's own commit and every tree since measure 7,295,037,7xx". The table at `:147` says `scalar_loop::scalar_dispatch` = **6,485,912,913**. The comment is also git-history narration (the project rule forbids it) and its number contradicts the pin it sits above.

### 7.3 Entries whose doc wording no longer matches the code

- memory-arc claim-ledger bullet says "a per-fiber `ClaimTable` per pool slot (`crates/ds-vm/src/claims.rs`)" — true, but it describes the ledger as the teardown witness while `crates/ds-vm/src/claims.rs:73-80` and `crates/ds-lower/src/place.rs:62-76` now make the **establishment band** the witness and the ledger only the count gate. The doc's own next clause says so, so the bullet contradicts itself; shrink to the band rule.
- memory-arc "Span shapes": "`NativeResult::Span` rides overlay words `0..=n`" — the code checks `1 + payload.len()` against `caller.arg_area_len()` and refuses `RetSpanTooWide` (`crates/ds-vm/src/vm.rs:2637-2645`), i.e. the span is `0..n` **inclusive of the tag word**, not `0..=n` of the payload. The current wording invites an off-by-one in a fixture.
- memory-arc "Ref word": "the decode funnels (`read_ref`/`read_counted`/`ref_arg`) apply the mask, as does each direct-match site". `ref_arg` is a native-args helper (`crates/ds-vm/src/net.rs:130`), not a decode funnel, and the mask is a struct invariant of `RefId` (`crates/ds-vm/src/value.rs:160-190`), not something any funnel applies. No behavioral content left to state.
- pitfalls/runtime-memory "The deadline wheel fires only when `wait_world` sees no runnable fiber … (item 153)" — the `(item 153)` pointer is the only referent for "which door"; the code sites already state both doors (`crates/ds-vm/src/vm.rs:7002-7045` wheel, back-edge poll in the interpreter). Drop the item number.
- pitfalls/runtime-memory "A staged word's count hand-off … CALLS staging it with no hand-off leaks that Ref into a Ref-claiming body's exit dec" and "An entry frame ending in a native call leaves its staged counts (an accepted leak class)" are the *same* accepted-leak class stated twice with different names; keep one.

### 7.4 Entries that state a rule the code contradicts (naming)

- pitfalls/lowering "A `is` test over a written type other than a nominal single-segment path lowers through the checker's solved test at the test expression's span (`Checked::is_tests`)" — the field is read through the lowerer's lens helpers; the direct-record read the entry names is exactly what `crates/ds-lower/tests/lens_drift.rs` forbids outside the allowlist. The entry should name the lens helper, not `Checked::is_tests`, or a fixer following it will trip the drift pin.

## 8. Notes for whoever applies this

- Do not paste §7.1 numbers into code comments.
- The largest class of remaining doc-reference hits (`[C4]`, `[E5]`, `[F2]`, `[H2]`, `[BL-6]`, …) is the bracketed decision-ID vocabulary; it is orthogonal to this survey (see the comment audit's class-A rows) and should be rewritten as behavior wording in the same pass, not carried along.
