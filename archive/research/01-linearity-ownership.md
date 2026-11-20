# DreamScript research 01: linearity, ownership, and memory management

Date: 2026-11-04. Scope: type-system linearity (affine/linear/unique/quantitative), ownership models and their user-facing cost, and memory-management strategies usable by an interpreted (IR/bytecode) language that wants ultra-high performance without exposing C++-level complexity.

Method: primary sources. Nine repositories were cloned (`git clone --filter=blob:none --depth 1`, roc sparsely) into `Z:/Temp/plr1/*` and read directly (paths cited below; the dirs were deleted after this document was written, on 2026-11-04). Complemented by official docs, Swift evolution proposals, papers, and language-author blog posts, all fetched 2026-11-03/04. Web search was used only to locate primary sources and check current status.

Author-taste constraints this document assumes (from philosophy_of_PL.md): types should be set-theoretic (union/intersection), C++-style specialization is out, monadic Option/Result is in, Koka-style UFCS is in, pointers should stay abstracted away, and total mental burden is the primary metric. All "implications" sections are written against those constraints.

## (a) Executive summary

The single most important discovery: **the 2024-2026 trend is to remove ownership from the user's plate and let the compiler infer it.**

Koka has had automatic borrow inference since v2.2.0 (Aug 2021) over Perceus precise reference counting, and delivers "Functional But In-Place" programming with zero ownership annotations.

Roc's new Zig compiler (2025-2026) implements ARC insertion whose own source header reads "borrow inference plus RC statement emission", with interprocedural owned/borrowed signature solving and a born-unique analysis; Roc's FAQ explicitly states Roc has **no linear, dependent, refinement, or uniqueness types**.

Lobster gets "compile-time reference counting" from an ownership analysis that removes ~95% of runtime refcount operations, again with no user annotations and only two rare error kinds.

None of these three expose a borrow checker. The borrow-checker family (Rust) pays a documented learning cost; its 2026 answer, **Polonius Alpha** (flow-sensitive borrow checking, nightly since 2026-08-04, stabilization planned before end of 2026), removes false positives, not difficulty.

Hylo/Swift demonstrate the middle path: value semantics + second-class borrows expressed as parameter-passing modes (`let/inout/sink/set` in Hylo; `borrowing/consuming` in Swift) plus opt-in noncopyable types (`~Copyable`, Swift 5.9).

`~Copyable` is a *negated trait constraint* — a set-theoretic construction — so noncopyable resource types compose with union/intersection types natively; this is the best precedent for DreamScript's taste.

Quantitative approaches (Idris 2 QTT 0/1/omega, Granule graded modalities, Linear Haskell `%m`) answer "how many times is this used" in the type; they are excellent for erasure guarantees and protocols (assets, session types) but painful as a default, and multiplicity *polymorphism* in signatures is the specifically bad part (Linear Haskell's UR-constraint pain).

For memory management the viable menu is: precise non-atomic RC + reuse analysis (Perceus/Roc: garbage-free, deterministic, best fit for interpreters), RC + runtime borrow counts (Inko: near-zero compiler cost, deterministic panics), generational references (Vale: no move errors, staleness detected on access), moving/immix GC (Inko built one, then abandoned it with an instructive post-mortem), and arena/platform allocation (Roc pushes allocator choice into platforms).

The "Fusion" research line (linear types x RC x GC) resolves, across Chirimar 1995 through ESOP 2022's "Linearity and Uniqueness: An Entente Cordiale", into one consensus: **every linear fact a user could write is derivable as a uniqueness fact (refcount==1); deriving beats writing for ergonomics, writing beats deriving for guarantees; one runtime can serve both.**

Recommended direction (argued in section d, Combo 6): value-semantic, immutable-by-default language; interpreter v0 runs precise RC with runtime uniqueness checks for in-place mutation plus an Inko-style borrow counter for deterministic diagnostics; opt-in `~Copyable`-style linear resource types for handles/assets; Koka/Roc-style borrow inference, TRMC and frame-reuse added later as compiler optimizations with zero language change.

## (b) Per-technology deep dives

### B.1 Roc — "no-user-visible ownership with C-level speed", read from the compiler source

Status (checked 2026-11-03): the compiler was **rewritten from Rust to Zig**, announced February 2025; the rewrite took 487 days and ~300k lines, and the Zig codebase has since grown to ~464k lines (https://ziggit.dev/t/roc-lang-to-be-rewritten-in-zig/8363, https://news.ycombinator.com/item?id=42935516, https://www.developersdigest.tech/blog/roc-rust-to-zig-rewrite-feldman).

Releases since the rewrite: 0.0.0-alpha1 (2025-01-29), 0.0.0-alpha2-rolling, alpha3 (2025-02-26), alpha4-rolling (2025-08-26); main branch active as of Nov 2026. Richard Feldman's talk "A Preview of Roc 0.1.0" (Func Prog Sweden, discussed at https://www.reddit.com/r/programming/comments/1vn7t7t/a_preview_of_roc_010_by_richard_feldman) previews the first numbered release and its "journey".

The old Rust compiler carried a tracing-GC component for some values (the Utrecht thesis "Reference Counting with Reuse in Roc" built RC as an alternative to its tracing GC). The new compiler is **pure reference counting with statically inferred ownership** — the LIR contract has no GC anywhere.

Language-level position, from the FAQ (https://www.roc-lang.org/faq): Roc has no linear types, no uniqueness types, no dependent or refinement types. Ownership reasoning is entirely a compiler concern.

The user-facing language (docs/langref/ in the repo) is deliberately Elm-like: one lambda syntax `my_fn = |arg1, arg2| ...`, records, tag unions, opaque types, static dispatch, effects marked with `!` and `=>` in types (`pure_fn : Str, Str -> Str` vs `run_fx! : Str, Str => Str`). No ownership syntax of any kind exists in the grammar.

Two lowering strategies coexist behind the same checked boundary (`design.md`, repo root): `.lss` (Cor-style: Monotype IR -> Monotype Lifted -> optional SpecConstr -> Lambda Solved -> solved inline plan -> LIR -> ARC insertion) and `.boxy` (layout-polymorphic: checked CIR -> LIR -> ARC insertion, one body per template with runtime type descriptors and dispatch dictionaries as hidden arguments). Both end at the same LIR+ARC contract, which is what keeps the four backends (interpreter `src/eval`, dev native, wasm, LLVM) byte-identical — the test suite requires it.

Optimized builds additionally specialize procedure variants by ownership mode ("owned/borrowed variant specialization", per issue 12672): a call site that can prove a dying argument is statically unique gets a variant that elides that parameter's runtime uniqueness checks; issue 12420 shows the flip side ("a string-pattern match in a loop makes calls pick the borrowed variant for dying arguments, so appends copy") — the mode-specialization machinery is real and has observable performance behavior in both directions.

The platform model (https://www.roc-lang.org/fast) makes allocation strategy a platform (host library) decision: "Roc platforms get to determine how memory gets allocated and deallocated in applications built on them" — CLI platforms may skip deallocations entirely; web-server platforms can arena-allocate per request and reclaim in one operation; long-lived-state apps can use meshing compaction. This is a clean answer to "who chooses the allocator" that an interpreted DreamScript could copy with a `platform`/`host` boundary.

Internal vocabulary (Glossary.md, clone `Z:/Temp/plr1/roc/Glossary.md`, sections Borrow/Consume/Copy-on-Write/Seamless Slice):

```
Borrow:   "A borrowing function reads its argument without affecting its
           reference count. The caller retains ownership and can continue
           using the value after the call."  Examples: strEqual, listLen.
Consume:  "A consuming function takes ownership of its argument. The caller
           transfers ownership to the callee and must not use the argument
           after the call."  Examples: strConcat, listConcat.
Copy-on-Write: "A variant of consuming where the function may return the same
           allocation if the input is unique (reference count == 1). If the
           input is shared, the function decrefs the original and allocates
           a new copy."  Examples: strWithAsciiUppercased, listAppend.
Seamless Slice: result shares the parent allocation; borrowing variant
           increfs, consuming variant inherits the reference.
```

These are contracts between compiler and builtins, not user syntax.

The core mechanism is `Z:/Temp/plr1/roc/src/lir/arc.zig`, whose module header (lines 1-13) is the best one-paragraph description of the whole system:

```
//! ARC insertion for LIR: borrow inference plus RC statement emission.
//! ... It first solves binding modes and proc ownership signatures
//! (`arc_solve`), then fills stable plans for each structured ownership
//! context and materializes their explicit RC decisions: borrowed bindings
//! emit nothing, owned final occurrences move, and lifetime-ending releases
//! land right after the last use of a binding's borrow group. Optimized
//! builds also emit optional mode-specialized proc variants for call sites
//! that can move arguments into positions the solved signature borrows, or
//! that prove a dying argument statically unique so the variant elides the
//! runtime uniqueness checks that parameter reaches. ... Debug builds
//! re-check the output with the borrow certifier (`arc_certify`).
```

`arc_solve.zig` (same dir) specifies the inference algorithm exactly. A binding solves to **borrowed** when three conditions hold: (1) its single defining statement is borrow-capable (field/tag-payload reads, aliases, a low-level op whose `RcEffect.result_borrows_args` names exactly one refcounted argument, or a call whose return borrows exactly one argument); (2) no occurrence demands ownership (never an owned call-argument position, consumed/retained low-level argument, aggregate or capture operand, `set_local` source, or owned return); (3) the lender chain resolves to a leader local bound exactly once.

Signatures solve interprocedurally in two phases: Phase A takes parameter modes to a fixpoint over exact reverse dependencies — parameters start borrowed and flip to owned when any occurrence demands a unit, "so every parameter bit is queued at most once"; Phase B marks returns borrowed when every returned value is a borrow anchored on a borrowed parameter, then re-solves binding modes so callers may borrow such results.

Then a **born-unique analysis** runs to fixpoint: "a proc's return is unique when every `ret` returns a born-unique value surviving to the return with no other holder, and a direct-call result of a unique-returning callee is itself a unique birth in its caller." Unique returns let call sites elide the runtime refcount==1 checks — this is Koka's fan-in reuse, formulated as a dataflow fact.

Pinned signatures (never solved) are ABI contracts: root procs, hosted procs, erased-callable procs, bodyless procs, and procs whose address escapes. "Everything else stays owned, which is always sound."

`src/base/rc_effect_rules.zig` is a comptime-verified structural rule table for every builtin's `RcEffect` row (which args it consumes/retains/borrows, whether the result aliases consumed args, whether it performs a runtime uniqueness check). Sample rules, quoted from the source: `retain_and_consume_same_arg` — "one argument is both retained and consumed; the two cancel, so the row describes a borrow and must name neither"; `runtime_check_without_allocate` — "a runtime uniqueness check exists to choose between mutating in place and copying, so the copy path allocates; may_allocate must be true"; `borrow_of_consumed_arg` — "the lender's unit is gone by the time the result would be read through it". A second layer (`eval/rc_conformance.zig`) cross-checks the declared claims against what each Zig builtin actually does at runtime, and the debug borrow certifier re-validates ARC output — an unusually serious correctness discipline for an ownership pass.

Supporting passes in `src/lir/`: `trmc.zig` + `tail_drive.zig` + `tail_call_builder.zig` (tail-recursion modulo constructors, following Koka's "Generalized Tail-Recursion Modulo Cons"), `box_reuse.zig`, `return_slot.zig` (return-slot rewriting), `loop_append_promote.zig`, `immortal_locals.zig`, `scalarize_joins.zig`, `tag_case_fusion.zig`, `StackPlan.zig`. `design.md` (repo root, the authoritative post-check design, ~26k lines) states the pipeline contract: "Backends do not reason about reference counting... Reference-counting policy belongs to LIR ARC insertion."

The bug tracker shows the analysis is load-bearing and debugged in the open: issue 11830 "ARC: borrowed local crosses a join without a live owner; --opt=speed builds segfault at runtime"; issue 10920 "Tail-call arguments are forced owned, losing borrows from outside the SCC"; issue 12436 "Dev backend: a record field moved into a call is still read through the record afterwards"; issue 12672 "Investigate whether skipping static uniqueness proofs and owned/borrowed variant specialization would speed up --opt=dev builds".

The abandoned 2024 design (context): during the Rust-compiler era Roc designed a user-facing borrow checker — "restricted borrows" of records/opaque types, `Value`/`Ref` parameter types, and "transparent functions" (pure functions the compiler may inline/reorder across because they cannot observe allocation). The record of that discussion lives on roc Zulip (`#design`/`#ownership` streams, not search-indexed) and in scattered issues (https://github.com/roc-lang/roc/issues/6027 era). The shipped Zig compiler contains none of it: the language stayed free of ownership syntax and everything moved into inference. (Note: the codename "feral" from the task brief does not appear in the current repo; no primary source for it was found.)

What DreamScript should take from Roc: (1) an interpreter can adopt the same fast path cheaply — per-value refcount, mutate-in-place iff refcount==1 else copy; (2) the language never mentions ownership; (3) allocation strategy is a platform/library decision (https://www.roc-lang.org/fast: "Roc platforms get to determine how memory gets allocated and deallocated" — CLI platforms may skip deallocations, web platforms use per-request bump arenas); (4) `arc_solve.zig`'s file headers are a complete readable spec of interprocedural borrow inference — the best available template if we later add an optimizing IR.

### B.2 Koka — Perceus, reuse analysis, FBIP, borrow inference

Repo: `Z:/Temp/plr1/koka` (https://github.com/koka-lang/koka). User-facing story: `doc/spec/why.kk.md` (sections "Perceus Optimized Reference Counting" and "Reuse Analysis") and the online book (https://koka-lang.github.io/koka/doc/book.html).

Perceus (Reinking, Xie, de Moura, Leijen, "Perceus: Garbage Free Reference Counting with Reuse", MSR-TR-2020-42, distinguished paper at PLDI 2021; https://www.microsoft.com/en-us/research/publication/perceus-garbage-free-reference-counting-with-reuse/) is **precise** RC: every value's reference count is exactly the number of logical references, maintained by statically inserted dup/drop instructions; no cycle collector, no GC, no runtime system needed — Koka compiles to plain C.

From `why.kk.md`: "Perceus uses extensive static analysis to aggressively optimize the reference counts. Here the strong semantic foundation of Koka helps a lot: inductive data types cannot form cycles, and potential sharing across threads can be reliably determined." Performance goal: "within 2x of the performance of C/C++", with initial benchmarks "close to C performance on various memory intensive benchmarks".

Reuse analysis (fan-out/fan-in): Perceus pairs a pattern match with a constructor of the same size and reuses the matched allocation in place if the refcount is 1. Canonical example from the book:

```
fun map( xs : list<a>, f : a -> e b ) : e list<b>
  match xs
    Cons(x,xx) -> Cons( f(x), map(xx,f) )
    Nil        -> Nil
```

If the list is not shared (`list(1,100000).map(sqr).sum`), the new `Cons` cells overwrite the old ones in place. The generated C fast path (from the book's aside, included in `why.kk.md`):

```
void map( list_t xs, function_t f, list_t* res) {
  while (is_Cons(xs)) {
    if (is_unique(xs)) {            // if xs is not shared..
      box_t y = apply(dup(f),xs->head);
      if (yielding()) { ... }       // if f yields to a general ctl operation..
      else {
        xs->head = y;
        *res = xs;                  // update previous node in-place
        res = &xs->tail;
        xs = xs->tail;              // .. and continue with the next node
      }
    }
    else { ... }                    // slow path allocates fresh nodes
  }
  *res = Nil;
}
```

Koka additionally implements **TRMC** (tail-recursion modulo cons), turning the recursive `map` into an in-place updating loop on the fast path; the generalized theory is Leijen's "Tail-Recursion Modulo Cons" line (ICFP 2024).

The book's key claim, verbatim: "the reuse optimization is guaranteed and a programmer can see when the optimization applies. This leads to a new programming technique we call FBIP: functional but in-place. Just like tail-recursion allows us to express loops with regular function calls, reuse analysis allows us to express many imperative algorithms in a purely functional style."

Fan-in/fan-out formally: "fan-out" reuse happens where an argument known unique enters a function (no copy needed), "fan-in" where a freshly produced result of matching size lands in a frame slot just vacated by a consumed argument; Frame Limited Reuse (Xie, Leijen et al., "Reference Counting with Frame Limited Reuse", MSR-TR-2021, https://www.microsoft.com/en-us/research/wp-content/uploads/2021/11/flreuse-tr.pdf) restricts reuse to the current frame, keeping the analysis local, predictable, and separate-compilation-friendly.

Borrow inference: automatic borrowing of user functions was introduced in **Koka v2.2.0 (2021-08-26)**; before that only builtin primitives borrowed (stated in the FLR paper itself). Implementation: `src/Backend/C/Parc.hs` — header comment: "precise automatic reference counting (now called Perceus)... The monad has a borrowed and owned (multi-set) environment just like the paper. The live variable set is a state. To calculate the live variables we visit the expression tree _in reverse_." Supporting module `src/Core/Borrowed.hs`. Borrowed locals emit no incref/decref; owned locals drop at last use. The readme's roadmap item "Borrowing analysis for Perceus and improved reuse analysis (contact: Anton Lorenzen)" was realized in later releases ("Fully In-Place Functional Programming", Lorenzen & Leijen et al., ICFP 2023).

Orthogonality of effects (relevant to our optional-effects plan): Koka's reuse/borrow stack runs *beneath* effect handlers without interaction — the same `map` above compiles through effectful `f` (the generated C contains `if (yielding()) { ... }` continuation hooks right beside the in-place update). Xie's "evidence passing" compilation (cited in the koka readme: `@Xie:evp`, `@Xie:evidently`) is the mechanism that keeps handler dispatch cheap while Perceus keeps memory precise; the readme credits Ningning Xie for "the theory and practice of evidence passing" and "the formalization of Perceus reference counting". Lesson: effects and ownership inference can be layered — the effect system decides *control flow* (yield points), the RC system decides *data flow* (dup/drop), and neither needs to know about the other beyond the yielding hook.

Interaction with a linear-type goal (our standing question): Koka's demonstrated position is that **linearity at the language level is unnecessary for the memory-management payoff** — uniqueness analysis (refcount==1 checks + reuse pairing + borrow inference) recovers in-place behavior for functional code while the language keeps GC-free-without-annotations ergonomics. What pure linear types would still add: static zero-check guarantees for user-defined resources, session-type protocols, and mutation of unrefcounted unboxed data — exactly the gap our Combo 4 (opt-in linear layer) fills.

### B.3 Hylo and Swift — value semantics, parameter modes, noncopyable types

Hylo repo: `Z:/Temp/plr1/hylo` (https://github.com/hylo-lang/hylo; last commit Oct 2026, active). Hylo (formerly Val) is the purest "value semantics with second-class borrows" design: the surface language has no pointers, no references, no dereference operator at all; sharing is a compiler implementation detail.

Parameter passing conventions: `let` (immutable borrow, the default), `inout` (mutable borrow), `sink` (consuming — callee takes ownership), `set` (write-before-read inout, used by accessor setters). Evidence: `docs/LocalBindings.md` shows the IR signature `fun add(let Int, let Int) -> sink Int`; `docs/MiscNotes.md` shows all four accessor kinds on one subscript:

```
subscript identity<T>(x: T): T {
  let { yield x }
  sink where T:Movable { x }   // consuming read, requires movability
  inout { yield x }
  set { x = newValue }
}
```

Local bindings (`docs/LocalBindings.md`): `var` always has storage; a `let` "gets no storage if all its initializers evaluate to lvalues" (it is a borrow), gets `T` storage if all initializers are rvalues, and gets `Optional<T>` storage in mixed cases; an `inout` binding "never has storage and is always represented as a borrowed access at the IR level" (`start_borrow [inout] %0`). Definite-initialization analysis is an intraprocedural abstract interpreter that inserts `deinit` instructions.

The mixed-case IR is instructive because it shows borrows and storage being unified mechanically. For `let bar = if condition { foo } else { 42 }` (where `foo` is a `var Int`), one branch binds `bar` as `start_borrow [let] %0` of `foo`'s storage while the other wraps the literal in `Option<Int>` storage; at the join, `bar : &Int` is re-borrowed out of the optional (`start_borrow [let] %15`), and since `bar` is used by a `copy()` call the compiler materializes `Hylo.Int.copy` explicitly. Storage, borrows, and copies are all IR-level decisions — the user wrote none of them.

Definite initialization (`docs/DefiniteInitialization.md`): memory locations are tracked in states Raw / Uninitialized / Initialized / Partial; `load` requires Initialized and leaves Uninitialized; the abstract interpreter models the call stack and inserts `deinit` at scope end — i.e., Hylo's ownership story is "single owner, statically tracked, borrows are projections, and the compiler owns all the bookkeeping".

The IR treats register-resident objects as **linear resources** — quoted: "Objects that live in registers are initialized when first assigned to a register and eventually get consumed before the function returns. Because they are treated as linear resources, any operation in which an object appears as an operand consumes it." Linearity lives one level below the source, where the compiler can manage it.

Noncopyable types (StandardLibrary/Sources/Core/Copyable.hylo and Movable.hylo):

```
public trait Copyable: Movable {
  fun copy() -> Self
}
public trait Movable {
  fun take_value(from source: sink Self) { ... }
}
```

A user writes `struct FileDescriptor: ~Copyable` to get unique ownership with a `deinit` running at the end of the unique instance's lifetime; subscript sinks are guarded with `where T:Movable`. Copyability is a default positive constraint subtracted by negation — precisely the set-theoretic idiom DreamScript wants.

"Mutating let": neither Hylo nor Swift allows mutating an immutable binding. Hylo's answer is that a `let` bound to an lvalue *is* a borrow, and mutation goes through `inout` bindings (`inout bar = &foo`); Swift's answer is plain `var` plus ownership modifiers. Record this as a deliberate non-feature for DreamScript too: rebinding-immutability adds aliasing paradoxes for marginal sugar.

Swift (proposals read as local copies `Z:/Temp/plr1/se0377.md`, `se0390.md`, `se0430.md` from https://github.com/swiftlang/swift-evolution):

SE-0377 (Swift 5.9) adds `borrowing`/`consuming` parameter modifiers: "Other functions default to borrowing their parameters"; methods may take `consuming self`; the accepted revision requires an explicit `copy` operator to move a value into a consuming position, making every copy visible.

SE-0390 (Swift 5.9), noncopyable structs and enums: "Values of noncopyable type always have unique ownership, and can never be copied... Since values of noncopyable structs and enums have unique identities, they can also have `deinit` declarations, like classes, which run automatically at the end of the unique instance's lifetime." Example from the proposal: `struct FileDescriptor: ~Copyable { private var fd: Int32; ... deinit { close(fd) } }` — "no object needs to be allocated; only a simple struct... that uniquely owns the instance".

SE-0414 (region-based isolation) + SE-0430 `sending` (Swift 6.0): a `sending` parameter/result "is required to be disconnected at the function boundary and thus possesses the capability of being safely sent across an isolation domain" — compile-time-proved exclusive ownership transfer across concurrency domains, generalizing move semantics for data-race safety without making types globally `Sendable`.

Swift's exclusivity is additionally enforced **dynamically**: `-enforce-exclusivity=checked` (default in debug) traps on conflicting accesses at runtime — even a mature system backs static ownership rules with runtime checks.

How much ownership UX does Swift expose? Essentially none in ordinary code: ARC handles lifetime, COW is a library convention (Array/String is-unique checks are runtime refcount checks, invisible), and the modifiers are opt-in affordances for stdlib-grade code. The price: users cannot state "this function will not share the receiver" for performance, and ARC overhead is real — the scattered-thoughts article cites benchmarks where switching from atomic to non-atomic RC doubled performance of some Swift programs.

### B.4 Rust — exact rules, NLL, Polonius Alpha, learning cost, workarounds

Rules as implemented today: every value has exactly one owner; assignment and by-value passing **move** values of non-`Copy` types, invalidating the source; access is via `&T` (shared) or `&mut T` (unique mutable) borrows; borrows must not outlive the referent; a live `&mut` excludes all other uses of the referent; `Drop` types cannot be partially moved; drop order is reverse declaration order, stabilized by RFC 1857 (https://rust-lang.github.io/rfcs/1857-stabilize-drop-order.html).

Borrow checking runs on MIR. **NLL** (shipped 2018) made borrows end at last use instead of scope end, but the analysis is *flow-insensitive about liveness regions*: a borrow's region is the join of all points where it might be used, so one branch that returns a borrow taints the whole function.

The canonical false positive, from the Polonius announcement (https://blog.rust-lang.org/2026/08/04/enabling-polonius-alpha-on-nightly): `HashMap::get_mut_or_default` — the `Some(value) => value` arm's borrow is treated as live in the `None` arm, where the map must be mutated, so the program is rejected although no path uses the borrow after the match:

```
fn get_mut_or_default(map: &mut Map, key: K) -> &mut V {
    match map.get_mut(key) {
        Some(v) => v,          // borrow tied to fn lifetime here ...
        None => { map.entry(key).or_insert(default) }  // ... so this mutate is rejected
    }
}
```

Polonius Alpha accepts it because the analysis is location-sensitive: the borrow is provably dead on the `None` path. A second canonical shape that still fails even under Polonius Alpha (linked-list walking with reassignment `p = &mut p.next` inside `while let`) shows the ceiling of the current reformulation.

**Polonius Alpha** is enabled on nightly since 2026-08-04 "in preparation for stabilization in the next few releases" (target: before end of 2026). It adds **flow-sensitive borrow checking of lifetime outlives relationships**: it knows the `get_mut_or_default` borrow is not live on the `None` path and accepts the program. Implementation: the 2018 Datalog formulation was sound but too slow; the 2023 reformulation required "minimal rearchitecture of the existing NLL implementation" and became "polonius-next"/Polonius Alpha; compile-time regressions over the top 10k crates were mostly under 1%, worst case 2-3x on some programs outside that set. Limitations: not a strict superset of legacy Polonius — some linked-list-style `p = &mut now.next` loops are still rejected; opt-out via `-Zpolonius=off`.

Why the learning cost is high (synthesis of primary commentary): (1) the type system encodes *places and time*, not values — users must learn that a variable names a place, that moves poison it, and that borrows are liveness projections; (2) errors are about non-structure ("does not live long enough", "borrow later used here") and require whole-CFG reasoning; (3) function signatures must commit to an aliasing discipline up front, so design mistakes surface as rewrites (`&mut` -> `&` -> owned -> `Rc<RefCell<T>>`); (4) self-referential and cyclic structures are unrepresentable without indirection. without.boats' essay "Ownership" (https://without.boats/blog/ownership) decomposes Rust's ownership into four semi-coupled features (affine moves, borrows, lifetimes, drop) and locates the difficulty in the coupling — a checklist for what not to couple in DreamScript. The Stack Overflow survey consistently rates Rust most-admired and hardest-to-learn simultaneously, which is the popular one-line summary of this trade.

Workarounds people reach for, each existing because the checker blocks a legitimate program: `clone()` (erase sharing); `Cow<'a, T>` (clone-on-write at API boundaries); `Rc<T>`/`Arc<T>` (shared ownership; `Arc` pays atomics — atomic-vs-non-atomic RC can be a 2x swing per the benchmarks cited in the scattered-thoughts article); `RefCell<T>`/`Mutex<T>` (move the exclusivity check to runtime — precisely the trade Inko and Roc's runtime uniqueness checks make globally); `Box::leak`/arenas (escape the lifetime system by never freeing); index/id-based graphs instead of pointers. Every workaround converges on "dynamic check or dynamic count" — which is exactly the design point of Inko/Lobster/Roc: those languages make the dynamic mechanism the only mechanism, so there is nothing static to learn.

### B.5 Austral — linear types done strictly (free/borrow, linear methods)

Repo: `Z:/Temp/plr1/austral` (https://github.com/austral/austral; last commit Jul 2025 — development has slowed). Austral proves a fully linear core with C-grade codegen can be *simple*: no GC, no RC; linear values are just values the compiler moves. README claim: memory safety with "no runtime overhead, avoiding double `free()`, use-after-`free` errors, and double fetch".

Design (docs/walkthrough.md, "Linearity Checking"): linear values must be **consumed exactly once**; borrows are statements, not types. The checker is an abstract interpreter over a state table mapping each linear variable to `(definition loop depth, state in {Unconsumed, BorrowedRead, BorrowedWrite, Consumed})`.

The exact rules, condensed from the walkthrough: consuming requires the variable `Unconsumed` and at the same loop depth where it was defined (a linear variable defined outside a loop cannot be consumed inside it, since it could be consumed multiple times); within one expression at most one consume (`C > 1` is an error), and a consume excludes all reads, borrows, and paths in the same expression (`R`, `W`, `P` must be zero); at most one `&!` write borrow per expression, excluding reads; `if`/`case` branches must agree on the consumed-set; on `return`, all live linear variables must be consumed; on leaving a binding's scope, the introduced linear variables must be `Consumed`.

Borrow statements are block-scoped: `borrow x: T as y in <body>` yields a read borrow, `borrow x: T as ref y in <body>` a write borrow — borrows are regions, so no first-class-reference expressiveness problems exist and none are mourned.

The loop-depth rule in its original form (docs/walkthrough.md): a linear variable defined at loop depth d cannot be consumed at any depth greater than d, because control flow could pass the consume twice:

```
let x: T := Make_T();      -- x: defined at depth 0, consumed at depth 0 -> ok
while ... do
    let y: U := Make_U();  -- y: defined at depth 1
    for i from 0 to n do
        while ... do
            consume_y(y);  -- consumed at depth 3 > 1 -> REJECTED
        end while;
    end for;
end while;
consume_t(x);
```

This "depth tracking" trick is what lets Austral's checker stay a single forward pass with no fixpoint computation — a checker a single implementer can verify by hand.

`free` is a statement: `free x` consumes a linear value and returns the underlying memory as a linear pointer resource — manual memory management, typechecked. Linear methods (`method` on linear types) take the receiver linearly, providing RAII-like protocols (close/commit/cancel); the stdlib ships linear `Pointer` and `Span` over raw memory.

Why so strict: Austral targets high-assurance C replacement with zero runtime and wants decidability by construction. The cost: the consume-once discipline shapes *all* code, not just resource code; casual closures over linear values are impossible; graphs need explicit borrow choreography. This is the right model for an opt-in resource layer and the wrong default for a general-purpose interpreted language.

### B.6 Quantitative and graded approaches — Idris 2, Granule, Linear Haskell, ATS, Clean

Idris 2 (QTT): binders carry quantities 0 (erased at runtime), 1 (used exactly once at runtime), omega (unrestricted). The checker enforces usage and **guarantees erasure of 0-quantities** — the practical win: proof arguments provably cost nothing. Primary sources: Edwin Brady, "Idris 2: Quantitative Type Theory in Practice", ECOOP 2021 (https://drops.dagstuhl.de/entities/document/10.4230/LIPIcs.ECOOP.2021.9; https://arxiv.org/abs/2104.00480); tutorial https://idris2.readthedocs.io/en/latest/tutorial/multiplicities.html. Foundations: Atkey "Quantitative Type Theory" (2018); McBride "I got plenty o' nuttin'" (2016). QTT is not a memory scheme (Idris 2 is GC'd); it is usage accounting whose price is a number on every binder. The complementary QTT example (from the multiplicity tutorial): `dup : (1 x : a) -> (a, a)` is rejected (cannot use a `1`-quantity twice) while `drop : (1 x : a) -> ()` is fine; `0`-quantities type proof arguments that are *guaranteed erased* (`+`'s proof of an ordering relation never survives to runtime). The three-way split 0/1/omega is also exactly the granularity at which erasure decisions become checkable rather than heuristic — a property worth having even in a language that never exposes `1`.

Granule (`Z:/Temp/plr1/granule`, active Jul 2026; https://granule-project.github.io/): graded modal types generalize quantities to arbitrary grades (semirings) with effects. From the README:

```
dup : forall {a : Type} . a [2] -> (a, a)
dup [x] = (x, x)

map : forall {a, b, n} . (a -> b) [n] -> Vec n a -> Vec n b
-- the function is used exactly n times, once per element
```

Beautiful for resource/protocol correctness (session types, file handles); heavyweight for everyday code; distribution is research-grade (macOS binaries, Z3 dependency).

Linear Haskell (GHC `-XLinearTypes`, since 9.0; user guide https://ghc.gitlab.haskell.org/ghc/doc/users_guide/exts/linear_types.html; proposal https://ghc-proposals.readthedocs.io/en/latest/proposals/0111-linear-types.html): multiplicity-annotated arrows `a %1 -> b` and multiplicity-polymorphic arrows `a %m -> b`; ordinary omega code untouched, so adoption is opt-in; but writing multiplicity-polymorphic libraries leaks `UR` (unrestricted-ramified) constraints into signatures — `linear-base` exists to absorb this. Lesson for DreamScript: multiplicity *polymorphism* is the ergonomics killer; fix multiplicities per definition site (Austral-style) or infer them (Koka-style).

ATS (views): a `viewtype` pairs a type with a linear *view* — a proof of a memory layout; consuming the view consumes the resource; proof functions verify memory safety statically at zero runtime cost (tutorial: "Introduction to Programming in ATS" ch. 13, http://ats-lang.github.io/FROZEN000/DOCUMENT/INT2PROGINATS/HTML/; overview https://learnxinyminutes.com/ats). Highest notation burden in this survey; demonstrates linear types as proofs about memory rather than a runtime mechanism.

Clean (uniqueness typing): `*` uniqueness attributes let pure functions destructively update unshared arrays; references: Clean language report (https://clean.cs.ru.nl/Clean), "Uniqueness Typing Simplified" (de Vries et al., https://www.cs.tufts.edu/comp/150FP/archive/edsko-de-vries/uniqueness.pdf), and Futhark's modern treatment (https://futhark-lang.org/blog/2022-06-13-uniqueness-types.html). Uniqueness is the "at most one reference" weakening of "exactly one use" — which is exactly why it is inference-friendly: Koka's refcount==1 checks and Futhark's uniqueness analysis are runtime/analysis versions of the same idea, systematized in "Linearity and Uniqueness: An Entente Cordiale" (Marshall, Ostermann, Brady, et al., ESOP 2022, https://recurial.com/LinearityAndUniqueness.pdf).

### B.7 Vale — generational references, region pooling, higher RAII

Repo: `Z:/Temp/plr1/vale` (https://github.com/ValeLang/Vale; last commit Sep 2026). Canonical writeup: https://verdagon.dev/blog/generational-references (all details below are from it).

Mechanism: a generational reference is 16 bytes — raw pointer plus `rememberedGeneration`; the appendix "generation tables" variant is 8 bytes (object pointer + 32-bit table index + 32-bit generation). Each allocation carries a `currentGeneration` header; `gmalloc`/`gfree` increment it; every dereference the compiler cannot prove safe emits a `__check` comparing generations, implemented as an access to a protected page (manual segfault, faster than assert).

Default scheme ("random generational references"): instead of counting, write a fresh pseudo-random number above each object at allocation/stack-entry and zero it at destruction; stale references escape detection with probability 2^-64 — the article computes ~73,250 years of mean false-safety even at 6 million failures per second. Because there is no central counter, plain malloc works, objects live on the stack, inline in parents, in arrays, or in custom allocators, and memory returns to the OS normally.

The check in skeletal form (from the article's C-like exposition):

```
ref  = { alloc: *Word, rememberedGeneration: u64 }   // 16 bytes
word above each allocation: currentGeneration: u64

__check(r):  if (*r.alloc.generation != r.rememberedGeneration) fault();
             // fault = store/load through a protected page => SIGSEGV
gmalloc(sz): slot = find_free(sz); slot.generation += 1; return ref(slot)
gfree(slot): slot.generation += 1; return slot to the free list
destroy(r):  r.alloc.generation = 0   // every existing ref now mismatches
```

Reference-layout comparison across the surveyed systems (per heap reference, 64-bit): tracing GC 8 bytes (no check); plain RC 8 bytes + count in header; Inko 8 bytes + refcount + borrow count in header; Vale 16 bytes (gen in ref) or 8 bytes (gen tables); Rust shared 8, `&mut` 8 with full static proof; zest-style dynamic borrows 16 bytes (lender + owner + lease on the stack). For DreamScript, the 8-vs-16-byte choice is effectively the choice between "counts" and "generations" as the dynamic safety substrate.

Benchmark (BenchmarkRL terrain generator, Vale flavors only): unsafe 43.82s (baseline); naive RC 54.90s (+25.29%); generational references 48.57s (+10.84%) — "GR's overhead was less than half that of reference counting". No direct C++/Rust/GC numbers; the author calls beating RC "a pretty low bar".

Hybrid Generational Memory (HGM): three mechanisms eliminate checks — (1) single ownership (access through owned values skips checks); (2) **linear style**: temporarily moving ownership in/out of a function avoids checks entirely, "whole programs can theoretically reach zero checks"; (3) regions + static analysis: `pure` annotations make data immutable so one `__precheck` replaces many later checks ("200 checks into 1" in the article's example), and immutable region borrowing skips checks outright. The intended blend: flexible mutable-aliasing code by default, then selectively apply `pure`/regions/linear style in hot paths; the author cautions that pushing checks to zero Rust-style just shifts costs into cloning, bounds checks, and hashing.

Region pooling: repo docs `docs/LinearRegion.md`, `docs/IRegion.md`, `docs/RegionBookkeeping.md` describe region kinds where pointers become offsets (arena/serialize buffers for recording and replay); `architectural-direction.md` records the consolidation of the former zoo (unsafe/resilient/generational/linear regions) into one "Hybrid-Generational Memory" region kind, rebuilt from the unsafe region kind after verifying it compiles to C-equivalent assembly.

Higher RAII: ownership in the type system guarantees a future call happens; destructors can take arguments and return values; this enables seamless structured concurrency (no async/Send function coloring) and perfect replayability debugging.

Relevance to DreamScript: generational references are the only surveyed scheme with **no move errors at all** — aliasing is free and staleness is detected on access. Costs: every heap reference grows by 8 bytes, and unprovable derefs pay a check. Immutable-by-default languages (ours) elide most checks because immutable values are never invalidated behind an alias — the intersection of Vale's mechanism (3) with our default mutability policy is unusually favorable for an interpreter (generation words are trivial to maintain when values are immutable).

### B.8 Inko — single ownership without a borrow checker, runtime-checked borrows

Repo: `Z:/Temp/plr1/inko` (https://github.com/inko-lang/inko; manual source at docs/source/getting-started/memory-management.md, runtime design at docs/source/design/runtime.md).

Full model from the manual: values are owned references; assignment **moves** — the compiler error is `error(moved): 'cats' can't be used as it has been moved`; borrows are explicit `ref` (immutable) and `mut` (mutable) and are **not** lifetime-checked:

```
Inko allows you to move owned references while borrows to the owned
reference exist. If an owned value is dropped while borrows to it still
exist, a runtime error known as a "panic" is produced, terminating the
program:
  panicked: can't drop a value of type 'Array' as it still has 1 reference(s)
```

Safety rationale, quoted: "each heap allocated value stores a borrow counter. This counter is incremented when borrowing the value, and decremented when the borrow is discarded. When dropping an owned value, the borrow counter is checked and a panic is produced if the count is not zero. Using this approach we can avoid having to implement a complex compile-time borrow checking mechanism, and still implement patterns otherwise difficult or impossible to implement when using borrow checking (i.e. linked lists or graphs)."

The mechanism is based on the 2006 paper "Ownership You Can Count On: A Hybrid Approach to Safe Explicit Memory Management" (linked from the manual: https://inko-lang.org/papers/ownership.pdf). Determinism argument: "the check is performed when dropping a value and not when dereferencing a borrow, so if the program triggers a borrow error with input A, then running the same program with the same input 10 times will produce the same error 10 times" — plus a stack trace naming the drop site. Empirical claim: "encountering borrow errors is rare to begin with as it just isn't that common for borrows to outlive the owned values they borrow."

Optimization posture: "most of the borrow count mutations can be optimized away. While we don't implement such optimizations at the time of writing, we intend to do so in the future" — i.e., Inko ships the runtime model first and plans compile-time elision later, the same staging as Roc/Koka.

Unique references (`recover`) exist for message passing between processes: a unique value is guaranteed to have no outstanding borrows, so sending it between processes cannot race — the concurrency role Rust assigns to `Send`/`Sync`, here reduced to a move.

Automatic borrow coercion at call boundaries (the manual's table, reproduced exactly — this is the entire "signature noise" an Inko programmer sees):

| Input | Expected | Passed |
|---|---|---|
| `T` | `ref T` | `ref T` |
| `T` | `mut T` | `mut T` |
| `ref T` | `ref T` | `ref T` |
| `mut T` | `mut T` | `mut T` |
| `mut T` | `ref T` | `ref T` |

Notably absent from the table: `ref T` accepted where `mut T` is expected (no silent upgrade to mutable), and `T` accepted where `T` is expected (moves are explicit in the language model even when the compiler elides them).

Value types (Int, Float, String, processes, FFI C structs) copy on move instead of transferring ownership — small values escape the aliasing machinery entirely.

GC history: earlier Inko used a custom Immix-based allocator/GC (with a moving, defragmenting heap in the pre-0.16 process era); docs/source/design/runtime.md records why they left it: "The implementation was quite complex and difficult to debug; Immix suffers from fragmentation, and without a GC (what it's designed for) it's hard to clean up the fragmentation; our implementation was unlikely to outperform highly optimised allocators such as jemalloc, so we figured we may as well use an existing allocator." They now use the system allocator directly — a candid data point on the true cost of a hand-rolled GC for a small team.

### B.9 Lobster — flow-typed ownership analysis over refcounting

Repo: `Z:/Temp/plr1/lobster` (https://github.com/aardappel/lobster; active Nov 2026). Primary source: docs/source/memory_management.md (Wouter van Oortmerssen).

Lobster combines runtime RC with an ownership analysis to get "compile time reference counting". Quoted: "the algorithm picks a single owner for each new heap allocation, which is usually the first variable, field, or vector element it gets assigned to, and then tries as best as possible to make all uses from there on 'borrows'. ... If somewhere else in the code something wants to own that same value, it will insert a reference count increase only for this particular use (in Rust, this would cause an error instead). Using this analysis was able to remove around 95% of runtime reference count operations."

The key architectural property: the analysis can never be unsound, only suboptimal — runtime RC is the ground truth, static analysis only elides operations. There are no owned/borrowed signatures, no runtime uniqueness checks, and COW is expressed by the programmer calling different builtins.

Distinctive mechanics worth stealing: (1) the analysis works on **AST nodes, not variables** — every node has an ownership kind it expects from children and passes to its parent; on mismatch, an incref is inserted (parent wants owned, child lends) or an anonymous scope-end drop (parent borrows, child owns) — most matches are free ("in `let a = [1,2,3]` ... everybody is happy. No Reference counts touched").

(2) Retroactive ownership: "A variable that is never assigned to ... and is initialized from a variable, field or vector element borrows it, for the rest of its scope... If something then writes to that L-value while the variable is alive, the analysis does not error ... but retroactively makes the variable own (its initializer gets the reference count increase, and it gets the decrease at the end of its scope), so the program means the same either way, and only the cost differs. This is what makes the `old := cur` pattern work."

(3) Borrow-across-call keeps the value alive for the caller's scope with a single incref "for that call only"; only same-expression borrow/mutate conflicts error (`f(v[0], v.pop())` — "cannot modify x while y borrows it"), reported as rare.

(4) Ergonomics evidence: across dozens of medium-sized game prototypes, enabling compile-time RC required "minor changes in only 2 locations".

(5) Opt-in strictness: per-variable Rust-like mode designed ("guarantee there will only be one owner ever, and error out if the analysis is not able to guarantee this. This will also be able to remove the reference count field from objects entirely") but not yet implemented.

(6) Cycles: RC cannot collect them; the accepted mitigation is a "cycle report" at program exit telling the programmer where cycles were leaked — pragmatic for game-shaped code.

(7) Inline `struct` types allocate in the parent (stack, field, or vector slot), are immutable, and copy on assign; the author notes they "reduce pressure" on the ownership algorithm by removing aliasing opportunities — the small-immutable-value lesson again, and a better tool than best-effort escape analysis for predictable performance.

(8) The ownership analysis is interleaved with flow-sensitive type specialization: functions are checked in call-graph order and specialized on types *and* ownership, which is why it is not a standalone pass.

L-value borrowing mechanics, for completeness: borrowed values refer to a stack of currently active L-values (variables, fields, vector elements); the L-value itself is reference-counted since multiple borrows may be active; mutating a borrowed L-value whose count is > 0 is the one hard error, and "the reference count not being 0 at the end is a bug in the ownership analysis" (the author's own invariant). Two further error kinds exist ("variable x still has 1 borrowers", "x used in <feature> without being borrowed") that the author reports never having seen trigger in practice. The full error inventory is thus three rare compile errors plus one retroactive-cost rule — the smallest ownership-error surface in this entire survey.

### B.10 Dada — permissions with runtime cancellation

Repo: `Z:/Temp/plr1/dada` (https://github.com/dada-lang/dada; last commit Feb 2026, still merging contributions from Niko Matsakis). Dada is the most explicit experiment in rethinking ownership UX.

Design goals (book/docs/design_docs/goals.md), quoted: "Easy to learn, but not dumbed down... it brings [ownership and borrowing] to the fore, but in a packaging that is (hopefully) easy to learn"; "Predictable and reasonably efficient... It's ok to call malloc, but not to run a garbage collector"; "Values, not places or pointers" — no pointer types and no dereference operator; "Sharing xor mutability" — Rust's rule kept; "Type erasure": given a Typed Dada program, delete all annotations and you get **Dynamic Dada**, which runs identically with permissions checked at runtime — static checking is a pure verification layer, never a semantic change.

Permission lattice (book/docs/dyn_tutorial/): `my` (unique/owned); `our` (shared — the object is read-only while shared; copyable); `leased` (a *movable* unique borrow — can be copied and moved, and is **cancelled** when the lessor writes); `shleased` (shared lease — read-only, also cancellable on write). From shlease.md: "When you have a shlease to an object, you know that the object will not be mutated so long as your shlease remains valid. Once a lessor writes to the object, your shlease is canceled."

There are no lifetime annotations and no explicit lease ends (design doc "why_not_explicit_end_for_lease.md" records the decision): leases end when the lessor writes, detected dynamically. "Atomic" storage allows mutation while shared, but only inside transaction blocks (atomic.md: "access to shared, atomic field outside of atomic block").

Tutorial-flavored examples (dyn_tutorial/field_permissions.md, our.md): `let pair: our = Pair(Widget(), Widget())` — an `our` pair whose fields the reader cannot distinguish from owned; copying it copies the permission, not the object. Leasing from a mutable value: `let l1: leased = p` then `let l2: leased = l1` creates a sublease chain, and the tutorial's IDE draws lessor variables "with dashes" while a lease is outstanding — permissions are values with a *runtime* validity, not compile-time regions.

Dada's design stance maps almost one-to-one onto an interpreter: the "Dynamic Dada" runtime (permission monitoring with cancellation) is precisely the kind of machinery a bytecode interpreter can host cheaply, and the "type erasure" principle (annotations are semantic no-ops under a GC) is exactly the property that lets a later static checker be added without changing semantics. It is the most ambitious of the "ownership can be abstracted" experiments; nothing in it has shipped as a static system yet.

Status: still a design-stage language; the calculus is being formalized in PLT Redex (design_docs/calculus.md: operational semantics "getting close", static type system on hold). Dada is the clearest blueprint if DreamScript ever wants first-class, movable borrows with GC-like ergonomics and no lifetime annotations; the price is a runtime permission-monitoring layer (fine for an interpreter) and a static story that has not landed.

### B.11 Shorter statuses and adjacent systems

Verona (https://github.com/microsoft/verona): "research programming language for concurrent ownership" — regions plus checker assertions (cown-based) instead of per-object annotations. Repo checked via API on 2026-11-03: not formally archived (archived=false, last push 2026-02) but README says early research "undergoing a massive refactoring" with the previous version on the `old_version` branch — effectively dormant. Idea worth keeping: region inference + checkers, ownership reasoned about per region rather than per object.

Mezzo (fpottier): per-alias permissions (`unique`, `shared`, `readonly`) as refinements; the GitHub repo is gone (API 404 on 2026-11-03); dead since ~2016. Historical value: first serious permission-refinement design; failure mode: permission polymorphism in signatures.

MoonBit (https://www.moonbitlang.com/blog; beta 2025-06, https://www.moonbitlang.com/blog/beta-release; 1.0 roadmap 2025-07): during its blockchain era (2023) MoonBit shipped `asset` types — affine bindings for on-chain resources that could not be duplicated or silently dropped (Move-flavored). The current 2025-2026 language docs (https://docs.moonbitlang.com/en/latest/language/) document no asset/affine feature; the language is GC-based (Wasm GC, js, native backends). Treat MoonBit's asset story as "linearity as an opt-in domain feature, later de-scoped".

Move (context): resource types with move semantics and explicit `copy`/`move`; the cleanest industrial linear-asset language (survey: https://github.com/s-tikhomirov/smart-contract-languages). Relevant only if DreamScript grows an asset story.

Swift ARC runtime (context): atomic RC, **no cycle collector** — cycles are avoided by convention (`weak` references); this is the production failure mode of plain ARC that Perceus's "inductive types cannot cycle" argument and Lobster's exit cycle report each address differently.

Jamie Brandon's zest demo ("Borrow-checking without type-checking", https://www.scattered-thoughts.net/writing/borrow-checking-without-type-checking): a dynamically typed language with single ownership, stack-resident refcounts, and dynamic borrow checking; owned refs (`^` move), borrows (`!`, returned to lender on drop), shares (`&`); each safety check is a single integer comparison on a stack-resident count (moved = INT_MIN, borrow count negative, share count positive); error messages locate the offending value by scanning the stack. Its summary of the design space matches ours: Rust solves value-semantics + interior-pointers "at the cost of a surprising amount of type system complexity"; Hylo/Mojo/Swift restrict references for a simpler model; C#/OxCaml refs and CHERI/fil-c solve only the memory-safety half; "R and (a subset of) Swift solve problem 1 by using reference counting and copy-on-write... none of this is compatible with my goal of predictable performance." Also cites OxCaml (modes for OCaml, 2025) as the industrial re-arrival of "modes, not lifetimes".

### B.12 Memory management for an interpreter: the survey

Reference counting families, in order of increasing compiler sophistication:

(1) Atomic ARC (Swift): predictable, no pauses, but atomics dominate cost (2x penalty in cited benchmarks); no cycle collector.

(2) Precise RC with reuse (Perceus/Koka; Roc's ARC): non-atomic counts, statically precise dup/drop, refcount==1 checks enable in-place mutation, reuse analysis removes allocations, "garbage-free" (frees are immediate, cache-friendly; finalizers run at last use via deferred cleanup); Koka's semantic foundation (inductive types cannot cycle, sharing determinable) removes the need for a cycle collector in practice.

(3) RC + runtime borrow counts (Inko): one counter per object, checked at drop; deterministic panics; trivial to implement; elision planned later.

(4) Ownership-analysis RC (Lobster): static elision over (2)/(3) with no user model; unsound-only-if-suboptimal.

(5) Generational references (Vale): no counts at all; 8-16 byte fat references; staleness detected on access; zero move/clone friction.

Tracing GC: generational (nursery + remembered set) and Immix (region-based, mostly non-moving with opportunistic defrag; Blackburn & McKinley, PLDI 2008, https://www.cs.utexas.edu/users/speedway/DaCapo/papers/immix-pldi-2008.pdf) are the practical designs; Inko's post-mortem (complexity, fragmentation without a GC to clean it, unlikely to beat jemalloc) is the cautionary tale for a small team. Moving GC conflicts with precise RC, interior pointers, and FFI. The GC-adjacent trick that keeps most of the benefit with none of the machinery is the arena-per-request pattern (Roc platform model: bump-allocate, free once at request end).

Escape analysis and stack allocation: value semantics + immutability make EA mostly unnecessary as a user-visible feature. Hylo allocates locals on the stack and lets `let`-bound lvalues have *no storage at all* (docs/LocalBindings.md); Lobster's inline `struct`s make stack/inline allocation explicit and predictable ("better than best-effort EA", per its author); Roc plans locals at LIR level (StackPlan.zig, immortal_locals.zig); Koka's FLR reuses frame slots for constructor churn. For an interpreter the pragmatic form is: allocate value cells in per-frame arenas and promote on escape — a runtime decision, which fits "interpreted, may compile to internal IR". Zero-cost moves are automatic when values are single-word handles: an owned reference moves by register copy with no dup/drop — Perceus formalizes this as emitting nothing.

The Fusion line (linear types x RC x GC), chronologically: Chirimar, Gunter, Riecke, "Reference Counting as a Computational Interpretation of Linear Logic" (1995) — the original identity between linearity and RC; "Garbage Collection Based on a Linear Type System" (CMU-CS-00-161); Wadler's linear-resource line; Perceus (PLDI 2021) with dup/drop fusion; Frame Limited Reuse (2021); "Fully In-Place Functional Programming" (Lorenzen, Leijen et al., ICFP 2023) — in-place functional programming via uniqueness checks instead of linear types; "Generalized Tail-Recursion Modulo Cons" (Leijen, 2024); "Linearity and Uniqueness: An Entente Cordiale" (Marshall et al., ESOP 2022, https://recurial.com/LinearityAndUniqueness.pdf) — the formal bridge showing uniqueness types are "linearity with revertible consumption", which is exactly why uniqueness is inferable while linearity is checkable-only; Andrew Johnson, "Hybrid Linear / Reference-Counted Garbage Collection" (https://andrewjohnson4.substack.com/p/hybrid-linear-reference-counted-garbage, L2 language) — linear types guaranteeing RC elision with a GC fallback for shared data. Settled conclusion: the linear/uniqueness duality lets one runtime serve both, and the user-facing question is only *who writes it down* — the user (Austral/Move) or the compiler (Koka/Roc/Lobster).

Interpreter implementation checklist distilled from all of the above (the "what would we actually build" list):

(1) Value representation: single-word handles for aggregates; tag word + refcount word in the header; small values (ints, floats, bools, short strings) unboxed or interned so they carry no count at all (Roc's numerics; Lobster's inline structs; Hylo's register objects).

(2) Semantics: every binding owns; assignment moves; `ref`/`mut`-free surface per Combo 1, or `ref`/`mut` with runtime counts per Combo 2; mutation compiled to "if refcount==1 mutate else copy" — one compare on the interpreter fast path, exactly Koka's `is_unique(xs)` and Roc's `may_runtime_uniqueness_check_args` row.

(3) Drop discipline: scope-end drops emitted by the bytecode compiler (Perceus-style, precise), with last-use drops where the bytecode liveness pass is confident; finalizers (deinit of `~Copyable` resources) run at the drop point deterministically.

(4) Diagnostics: Inko-style borrow counter (or a single "shared flag") checked at drop for deterministic "dropped while borrowed" panics with stack traces — cheap insurance while the static story is immature.

(5) Reuse hooks in the bytecode: constructor-after-match pairs and call-result-into-frame slots annotated so a JIT can later turn "allocate fresh" into "reuse the consumed cell" (Perceus fan-in/fan-out, Roc's box_reuse/return_slot passes).

(6) TCO and TRMC in the interpreter loop: direct tail calls via frame reuse (cheap, do it first); TRMC (constructor loops) as a later interpreter-level optimization — Koka's papers give the algorithm, Roc's `trmc.zig` gives a second implementation to cross-check against.

(7) Escapes: per-frame cell arenas with promotion on capture; closures allocate only when they capture (Hylo-style "no storage if bound to an lvalue").

(8) Measurement gates: count of dynamic uniqueness checks surviving in hot loops, fraction of allocations reused, and interpreter dispatch cost — the three numbers that decide whether the Combo 1 fast path is actually delivering Koka/Roc-class behavior.

## (c) Tradeoff tables

Table 1 — strategy vs. our criteria (LC = user learning cost; PE = peak performance; EX = expressiveness of sharing/mutation; IF = implementation cost inside an interpreter; DET = determinism of timing and memory).

| Strategy (exemplars) | LC | PE | EX | IF | DET | Notes |
|---|---|---|---|---|---|---|
| Tracing GC (Go; old Inko) | lowest | mid; pauses | highest (cycles fine) | high | poor | Inko abandoned its Immix clone for cost |
| Atomic ARC (Swift) | low | mid (atomics) | high (COW) | low | good | no cycle collector; ~2x vs non-atomic |
| Precise RC + reuse + borrow inference (Koka, Roc) | low (zero annotations) | near-C | high; cycles leak (Koka: inductive types acyclic) | high (compiler) | excellent | language never mentions ownership |
| Ownership analysis over RC (Lobster) | lowest | good (95% ops elided) | high; exit cycle report | medium | excellent | unsound-only-if-suboptimal |
| Runtime borrow counts (Inko) | low | good (one counter) | highest of no-checker options | very low | good (panic at drop) | elision planned later |
| Generational references (Vale) | lowest (no move errors) | good (+11% vs +25% RC) | high | medium (fat refs, checks) | good | immutable-by-default elides most checks |
| Full affine + block borrows (Austral) | high-ish (new discipline) | C-level | low-mid | low | perfect | rules fit one page; shapes all code |
| Affine + first-class borrows + lifetimes (Rust) | highest | C-level | highest (via unsafe/idx for graphs) | very high | perfect | Polonius Alpha reduces false positives, not difficulty |
| QTT/graded (Idris2, Granule, GHC) | highest (numbers in types) | n/a (GC'd) | protocol-level (assets, sessions) | very high | n/a | 0-quantities give guaranteed erasure; opt-in only |

Table 2 — what each language makes the *user* write (the UX surface), 2026 state.

| Language | Move errors | Borrow annotations | Lifetime annotations | Type-level linearity | Runtime ownership checks | Reuse/in-place optimization |
|---|---|---|---|---|---|---|
| Rust | yes | yes (`&`, `&mut`) | yes (signatures) | no | exclusivity (debug) | manual move semantics |
| Swift | no | opt-in (`borrowing/consuming`) | no | opt-in (`~Copyable`) | exclusivity (dynamic) | COW conventions |
| Hylo | no (no refs exist) | modes `let/inout/sink/set` | no | opt-in (`~Copyable`) | none needed | value semantics + EA |
| Koka | no | none (inferred) | no | no | unique checks (refcount==1) | guaranteed, observable (FBIP, TRMC) |
| Roc | no | none (inferred) | no | explicitly rejected | unique checks (elided in opt builds) | TRMC, box reuse, COW builtins |
| Inko | yes (moves) | yes (`ref`/`mut`) | no | no | borrow counts (panic) | planned, not implemented |
| Lobster | no | none (inferred) | no | no | RC ground truth | compile-time RC (~95%) |
| Vale | no | no | no | linear style opt-in | generation checks | linear style / regions |
| Austral | yes (consume-once) | `borrow` statements | no | yes (all linear types) | none | manual via free/borrow |
| Dada (design) | no | permissions (`my/our/leased/shleased`) | no (leases cancel on write) | no | permission monitoring | n/a (design stage) |

Table 3 — syntax implications for DreamScript (sugar budget compared against Kotlin, whose annotation surface users learn but which stays out of the happy path).

| Option | New keywords/symbols to learn | Signature noise | Worst-case error message |
|---|---|---|---|
| Hidden affine (Koka/Roc) | 0 (maybe `mut`) | none | "value shared, so copied" (info, not error) |
| Dynamic single ownership (Inko) | `ref`, `mut` | low | "can't drop value: still has 1 reference(s)" (panic + stack trace) |
| Param modes (Hylo) | `inout`, `sink` (2) | modes on decls only | "cannot capture inout parameter" |
| Opt-in linear (Austral-style) | `linear`, `borrow` | high on linear APIs only | "x is consumed but not returned" |
| QTT (Idris2) | `0`/`1`/omega per binder | very high | multiplicity mismatch |
| Generational refs (Vale) | 0 | none | "use of destroyed object" (deterministic trap) |

## (d) Viable design combos for DreamScript

Recap of the binding constraints: interpreted (may lower to internal IR/bytecode); static linear/affine types desired; immutable by default; ultra-high performance with all non-low-level optimizations (TCO, unboxing, escape analysis, possibly JIT); sugar budget at most Kotlin's; ownership/async UX open to reinvention; algebraic effects optional; set-theoretic types preferred; no C++ specialization; Koka UFCS and monadic Option/Result loved. Combos are ordered by increasing user-visible complexity.

Combo 1 — "Hidden affine" (Koka/Roc architecture; primary recommendation). Language: values immutable by default; the compiler tracks a single logical owner per value; no ownership syntax at all except an optional `mut` marker requesting in-place mutation of a local; stdlib is COW-flavored (append reuses when unique, like Roc's `listAppend`). Runtime: non-atomic precise refcounts (single-threaded interpreter, or per-process isolation for concurrency); every mutation site is a refcount==1 check (mutate in place) or copy; deferred drop amortizes deep frees. Compiler (later): two-phase borrow/owned inference exactly as specified in Roc's `arc_solve.zig` headers; FLR frame reuse; TCO plus TRMC. Learning cost: lowest; ownership feedback appears as performance notes, never errors. Perf: near-C for functional workloads (Koka's published results); interpreter overhead amortized by reuse loops. Expressiveness: high for value-oriented code; graphs/cycles need an exit cycle report (Lobster) or weak/id indirection. Syntax implications: zero keywords; at most `mut`. Risks: the inference layer is the most compiler work among low-LC options; mitigated by shipping runtime uniqueness checks first (they alone deliver the Koka fast path) and adding inference purely as optimization — Roc's and Koka's actual ship order.

Combo 2 — "Inko baseline": single ownership with dynamic borrow counts. Language: visible moves (trivially explainable), explicit `ref`/`mut` borrows, no lifetime checking, deterministic panic on drop-while-borrowed. Runtime: one borrow counter per heap object checked at drop. Learning cost: very low (Inko teaches the whole system in one manual page); perf: good, increment-heavy without analysis; expressiveness: highest of the no-borrow-checker options (linked lists, graphs, iterators all natural). Best *v0* for an interpreter: implementable in days, no unsound corners, and forward-compatible with Combo 1 (the same refcount field later carries the uniqueness fast path; the borrow counter doubles as the shared-count check). Syntax: `ref`/`mut` only. Risk: aliasing bugs become runtime panics instead of compile errors; acceptable and upgradable (Inko plans gradual compile-time checks for exactly this).

Combo 3 — "Hylo surface": value semantics + parameter modes + opt-in noncopyable. Language: pure value semantics; no references in user code; parameter modes default to `let` (borrow) with `inout` and `sink` written only when non-default; `~Copyable` types with `deinit` for resources; subscripts with `let/inout/set/sink` accessors replace getters/setters and iterator protocols, and compose with Koka-style UFCS (`xs[i].mutate()` reads as a method call but is a subscript borrow). Runtime: stack/arena locals, borrows are raw pointers, no refcounts needed for mode-passed values; RC only at true sharing points (closures, collections). Learning cost: low-moderate (two keywords, one concept); perf: excellent (no aliasing -> no checks -> trivial escape analysis); expressiveness: limited first-class-reference patterns (no `Option<&T>`; external iterators need coroutines — Hylo's answer, and the scattered-thoughts article catalogs what else breaks). Syntax: `inout`/`sink`/`~Copyable`, within budget. Composes with set-theoretic types perfectly (copyability as a negated constraint) and with monadic Option/Result with one caveat: `Result<~Copyable>` payloads need a Movable requirement or auto-borrow.

Combo 4 — "Opt-in linear resource layer" (Austral-flavored, layered on Combo 1 or 3). Language: default types are runtime-managed; a marked category (syntax suggestion: trailing `!` as in `File!`, or declaration-site `linear`) must be consumed exactly once; borrows only as block-scoped `borrow x as y in { ... }` statements; linear methods provide RAII-like protocols (close/commit/cancel). This is where assets (MoonBit/Move-style), session-typed connections, and unmanaged buffers live. Runtime: zero cost — linear values are unrefcounted raw handles; the checker guarantees no double-use, so no counts and no checks. Learning cost: contained to the resource layer; Austral proves the full rule set fits one page. Perf: perfect on that layer. Syntax: 2-3 keywords confined to resource types. This directly answers the author's question "can ownership be abstracted to reduce learning cost while keeping benefits": yes — linearity as a type-class-like lens over a value-semantic language, not a global discipline.

Combo 5 — "Generational + immutable" (Vale specialized to our defaults). Language: immutable by default; shared references copy freely; no move errors ever; mutable cells are an explicit `mut T` type on which generation checks appear; overwritten/destroyed values invalidate stale references deterministically. Runtime: 8-16 byte fat references plus generation words; because immutable values are never invalidated behind an alias, checks appear only on writes through `mut` cells, so steady-state overhead approaches zero while retaining "no dangling ever". Learning cost: lowest conceptually (`mut` is the only new idea); perf: good, slightly behind Combo 1 on mutation-heavy code; expressiveness: highest for sharing (cycles are safe but leak unless weak refs or a cycle report is added). Syntax: zero new keywords beyond `mut`. Best fit if "no puzzling errors ever" ranks above raw peak speed and compiler work must stay minimal.

Combo 6 (staging plan; recommended) — "Value-first, infer-later". Ship Combo 2's runtime (precise RC + refcount==1 mutation checks + Inko-style borrow counter for deterministic diagnostics) under Combo 3's surface (immutable by default; `inout`/`sink` modes; `~Copyable` resources), add Combo 4's optional linear layer for handles/assets, then grow Combo 1's compiler analysis (borrow/owned inference, TRMC, FLR reuse) as a JIT/AOT optimization with zero language change. Evidence this order works: Roc and Koka both shipped runtime uniqueness checks first (semantics) and inference second (optimization); Inko and Lobster prove the runtime pieces are small-team-implementable; Hylo's mode surface and Austral's linear layer prove the user-facing syntax fits our sugar budget; and the decade-long Polonius saga shows the one design NOT to start with is first-class borrows with lifetime inference.

Cross-cutting notes for the type system: (1) express copyability, sendability, and linearity as *set-theoretic* constraints (default positive trait `Copyable`, subtracted with `~`, intersection-composable with union/intersection types) so resources integrate with generic code without a second type universe — Swift SE-0390 demonstrates the mechanism in production; (2) avoid multiplicity polymorphism in user signatures (Linear Haskell's UR pain); infer or fix multiplicities; (3) algebraic effects are orthogonal — Koka runs its entire reuse/borrow stack beneath effect handlers (evidence passing composes cleanly), so async/ownership should not be coupled Rust-style; Swift's region isolation shows concurrency can be a separate later axis; (4) error-message policy: follow Roc/Lobster — ownership decisions surface as informative notes about copies, never as errors; reserve hard errors for the opt-in linear layer; (5) for the interpreter, treat the refcount word as an implementation detail of value cells and keep semantics "as if immutable copies" so a later compiler can replace runtime checks with proven facts without observable change (Dynamic-Dada's type-erasure principle, and Lobster's "analysis only elides" principle, both guarantee this upgradability).

## Appendix: primary-source index

Cloned repos (read 2026-11-03/04, deleted after writing): Z:/Temp/plr1/roc (design.md, Glossary.md, reunify.md, docs/langref/, src/lir/arc.zig, arc_solve.zig, arc_certify.zig, rc_effect_rules.zig, trmc.zig, box_reuse.zig, return_slot.zig, StackPlan.zig, immortal_locals.zig); Z:/Temp/plr1/koka (doc/spec/why.kk.md, readme.md, src/Backend/C/Parc.hs, src/Core/Borrowed.hs); Z:/Temp/plr1/inko (docs/source/getting-started/memory-management.md, docs/source/design/runtime.md, docs/source/design/compiler.md); Z:/Temp/plr1/hylo (docs/LocalBindings.md, docs/MiscNotes.md, docs/RemoteParts.md, StandardLibrary/Sources/Core/Copyable.hylo, Movable.hylo); Z:/Temp/plr1/austral (docs/walkthrough.md, README.md); Z:/Temp/plr1/granule (README.md); Z:/Temp/plr1/vale (architectural-direction.md, docs/GenerationalReferences.md, docs/LinearRegion.md, docs/DropFree.md); Z:/Temp/plr1/lobster (docs/source/memory_management.md); Z:/Temp/plr1/dada (book/docs/design_docs/goals.md, calculus.md, why_not_explicit_end_for_lease.md, book/docs/dyn_tutorial/my.md, our.md, lease.md, shlease.md, atomic.md); Z:/Temp/plr1/se0377.md, se0390.md, se0430.md (Swift evolution proposals SE-0377, SE-0390, SE-0430).

Key URLs: Perceus paper https://www.microsoft.com/en-us/research/publication/perceus-garbage-free-reference-counting-with-reuse/; FLR paper https://www.microsoft.com/en-us/research/wp-content/uploads/2021/11/flreuse-tr.pdf; Koka book https://koka-lang.github.io/koka/doc/book.html; Roc FAQ https://www.roc-lang.org/faq; Roc fast page https://www.roc-lang.org/fast; Roc Zig rewrite https://ziggit.dev/t/roc-lang-to-be-rewritten-in-zig/8363 and https://news.ycombinator.com/item?id=42935516; Roc 0.1.0 preview https://www.reddit.com/r/programming/comments/1vn7t7t/a_preview_of_roc_010_by_richard_feldman; Polonius Alpha https://blog.rust-lang.org/2026/08/04/enabling-polonius-alpha-on-nightly; RFC 1857 https://rust-lang.github.io/rfcs/1857-stabilize-drop-order.html; without.boats "Ownership" https://without.boats/blog/ownership; scattered-thoughts "Borrow-checking without type-checking" https://www.scattered-thoughts.net/writing/borrow-checking-without-type-checking; Idris 2 ECOOP 2021 https://drops.dagstuhl.de/entities/document/10.4230/LIPIcs.ECOOP.2021.9; Idris2 multiplicities https://idris2.readthedocs.io/en/latest/tutorial/multiplicities.html; Granule https://granule-project.github.io/; GHC linear types https://ghc.gitlab.haskell.org/ghc/doc/users_guide/exts/linear_types.html and proposal 0111 https://ghc-proposals.readthedocs.io/en/latest/proposals/0111-linear-types.html; ATS book ch.13 http://ats-lang.github.io/FROZEN000/DOCUMENT/INT2PROGINATS/HTML/; Clean report https://clean.cs.ru.nl/Clean; Uniqueness Typing Simplified https://www.cs.tufts.edu/comp/150FP/archive/edsko-de-vries/uniqueness.pdf; Futhark uniqueness https://futhark-lang.org/blog/2022-06-13-uniqueness-types.html; Entente Cordiale https://recurial.com/LinearityAndUniqueness.pdf; Vale generational references https://verdagon.dev/blog/generational-references; Vale memory-safe overview https://vale.dev/memory-safe; Immix paper https://www.cs.utexas.edu/users/speedway/DaCapo/papers/immix-pldi-2008.pdf; Inko ownership paper https://inko-lang.org/papers/ownership.pdf; Lobster https://github.com/aardappel/lobster and https://strlen.com/lobster; Dada https://github.com/dada-lang/dada; Verona https://github.com/microsoft/verona; MoonBit beta https://www.moonbitlang.com/blog/beta-release and roadmap https://github.com/moonbitlang/website/blob/main/blog/2025-07-24-roadmap/index.md; Andrew Johnson hybrid linear/RC/GC https://andrewjohnson4.substack.com/p/hybrid-linear-reference-counted-garbage; smart-contract language survey https://github.com/s-tikhomirov/smart-contract-languages; Inko manual https://docs.inko-lang.org/manual/; Hylo https://github.com/hylo-lang/hylo.
