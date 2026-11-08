# 02 — Concurrency, async, algebraic effects, and error handling

Research notes for DreamScript, 2026-11-04. Scope: algebraic effect systems (Koka, Effekt, Unison, OCaml 5, Eff/Frank), the function-color problem and every escape attempt from it (Zig, Go, Java Loom, Kotlin, Trio/structured concurrency, Rust, Swift 6), isolation-based concurrency without a borrow checker (Pony, Verona, BEAM, Inko, Mojo), error handling models, and candidate design combos for DreamScript. Evidence is primarily from source code (cloned at Z:/Temp/plr2 during research, since removed) and papers/docs; URLs given inline.

## a) Executive summary

1. Koka proves async can be an ordinary effect: `std/async` (shipped in Koka 3.2.9, 2026-09) defines `pub alias async = <async-await, async-ioc, async-cancel, discontinue>` — four primitive effects — with cancellation scopes, `interleave`, `firstof`, and `timeout` all built as user-level handlers (lib/std/async/async.kk, samples/async/basic.kk).
2. Koka's handlers compile by "generalized evidence passing" (Xie & Leijen, ICFP'21) to plain C: no stack switching, continuations are heap closures composed up to 8-deep inline in the thread context (kklib/include/kklib.h `kk_yield_t`), evidence vectors dispatch operations by unique marker. This is directly portable to a bytecode interpreter.
3. Koka's honest claim is not "async has no color" — `wait : duration -> async ()` visibly carries `async` in the row — but that the color is *inferred, composable, and disappearable at a handler boundary*, and there is no `await` keyword anywhere.
4. Effekt is the compilation-efficiency leader: capabilities are second-class parameters, so handlers compile to direct-style multi-prompt continuations over real stacks (LLVM runtime: `%Prompt` per handler, frames `{ReturnAddress, Sharer, Eraser}`, stack copy/save on capture — libraries/llvm/rts.ll). Key papers: "Effects as capabilities" (OOPSLA 2020), "From Capabilities to Regions" (OOPSLA 2023), "Multiple Resumptions and Local Mutable State, Directly" (ICFP 2025).
5. Effekt also settles Result-vs-exceptions: its stdlib defines `Result[A,E]` as the literal "reification" of the `Exception[E]` effect, with `value` (monadic reflection) going back (libraries/common/result.effekt). They are two views of one mechanism.
6. Unison (1.0, Nov 2025) shows the minimal effect system: exactly one built-in ability `IO` (+ `Exception`), ambient-ability checking against the nearest enclosing lambda, existential row variables but no effect-polymorphism ergonomics push. Cheap to learn, coarse-grained.
7. OCaml 5 effect handlers are still marked experimental in the manual as of 5.4 (Oct 2025) — the strongest industrial validation attempt remains unfinished after 4 years.
8. Zig deleted colorless async (0.11, 2023, unsound implementation) and rebuilt it 0.15/0.16 around an explicit `Io` capability parameter: `fn connect(host, io: Io, port)` — the same code runs over blocking threads, thread pools, or io_uring event loops; `io.async(...)` gives colorless structured concurrency with `Io.Queue`/`Io.Group` and cancellation. Zig's fix is capability passing, i.e. effects minus row polymorphism.
9. Go: stackful goroutines + integrated netpoller + preemptive scheduling = colorless by construction; the price is unstructured spawn (`go` stmt), no cancellation story (context.Context bolted on), goroutine leaks, and ~2KB min stacks.
10. Java Loom made the color problem disappear in practice by making threads cheap (heap-allocated stack chunks, mount/unmount on blocking calls); virtual threads final since Java 21; Scoped Values final in Java 25; StructuredTaskScope still preview after 6 iterations (JEP 505/525/533, JDK 25-27) — structured concurrency is the hard part, not the threads.
11. Kotlin kept colors (`suspend`) and won on structured concurrency: the Job tree (children cancel with parent; parents wait for children; failures propagate up) is the part other languages keep copying.
12. Trio's nursery essay (2018) remains the clearest statement: task lifetime must be lexically scoped, cancellation must be cooperative and scoped, and the nursery must not return until children finish.
13. Rust async is the cautionary tale: Pin, Send bounds unnameable on `async fn` in traits, runtime fragmentation (tokio de facto standard), cancellation-by-drop. without.boats (Apr 2024) argues coroutines and effect handlers are isomorphic and that stackful coroutines are the sweet spot.
14. Swift 6's region-based isolation (SE-0414) + `sending` (SE-0430) is ownership-inference-lite, adopted precisely to make async data-race safety tolerable; DreamScript's linear types make the same reasoning exact rather than inferred.
15. Pony/Inko prove linear-ish sendability + process isolation gives data-race freedom without shared memory; BEAM adds preemptive latency guarantees and let-it-crash at the cost of copying sends. Inko (vendored in this repo) is the closest existing language to "linear messages, no async keyword, no shared mutable state".
16. Error models converge on: Result-as-data for expected errors (Rust/Roc/Zig/Swift), an unhandleable panic/trap for bugs (Rust/Zig/Swift/Inko), and let-it-crash for supervision (BEAM). Checked exceptions are extinct; "error unions in the return type" (old DreamScript idea) is essentially Roc's inferred tag-union errors and is formally a restricted effect row — Koka's `exn` and Effekt's `Exception[E]` are the same idea expressed as effects.
17. For an interpreted DreamScript: Result returns cost ~one branch per call in a register VM; unwinding exceptions cost interpreter-loop frame pops; heap-allocated stackful coroutines are cheap (Lua/LuaJIT precedent); algebraic effect handlers are implementable with evidence vectors at modest cost — but multi-shot resumption + linear resources interact badly, which is precisely why Koka has `linear effect` (tail-resumption only) and why Effekt restricts to lexical handlers.
18. Verdict (detail in section d): full row-polymorphic resumable effects do NOT pull their weight for DreamScript v1 given linear types + immutable defaults + Result + nurseries; a "capability-passing IO + cancellation scopes" core (Zig/Effekt-style, i.e. effects without row-polymorphism surface syntax) DOES, and leaves the door open to graduate to full effects later. Structured stackful green threads in the VM is the highest benefit/learning-cost ratio for async.

## b) Per-technology deep dives

### 1. Koka — effect rows, async-as-effect, and evidence-passing compilation

Repo: https://github.com/koka-lang/koka (cloned at commit d23ee7c, 2026-10-27; version 3.2.9). Tour: https://koka-lang.github.io/koka/doc/book.html. This section cites paths inside that clone.

**Effect rows and inference.** Koka infers the full effect of every function. The absence of effects is `total` (or `<>`); `exn` = exceptions, `div` = divergence, `ndet` = nondeterminism, `io` = the worst effect (doc/spec/tour.kk.md, "Effect types"). Rows are sets with extension syntax `<l|e>`: `while : (pred : () -> <div|e> bool, action : () -> <div|e> ()) -> <div|e> ()`. At call sites, row variables are extended and unified so the predicate's `ndet` and the action's `exn` automatically become `<exn,ndet,div|e3>` — the row takes the union for free. Builtin atomic effects include capability-flavored ones (lib/std/core.kk):

```koka
pub type net :: X        // may access the network
pub type fsys :: X       // may access the file system
pub type ui :: X         // may access the graphics system
pub type blocking :: X   // may block
pub alias ioc-total = <ndet,console,net,fsys,ui,st<global>>
pub alias ioc      = <div,ioc-total>   // "I/O as in C", no exceptions
pub alias io       = <exn,ioc>
pub alias pure     = <exn,div>
```

Two notable things for DreamScript: first, `net`/`fsys`/`ui` are *capability names as effects* — the row doubles as a permission set, which is exactly the "what can this function touch" question a capability system answers. Second, the tour makes the denotational point that `int -> exn int` is literally `Z -> (Z + 1)` — the effect row and the return-type union are two presentations of the same semantics. That observation is the formal bridge to the "errors collected in the return-type union" idea (section 7.8).

**Handlers.** Declaring and handling (tour "Handling" and "Resuming"):

```koka
effect raise
  ctl raise( msg : string ) : a

fun safe-divide( x : int, y : int ) : raise int
  if y==0 then raise("div-by-zero") else x / y

fun raise-const() : int          // handler discharges :raise, function is total again
  with handler
    ctl raise(msg) 42
  8 + safe-divide(1,0)
```

`resume` continues to the call site (multi-shot is allowed); not calling `resume` returns into the handler scope (used for `fuel`-style aborts). `final ctl` declares a non-resumable operation — Koka's built-in exceptions are `final ctl throw-exn( exn : exception ) : a` (lib/std/core/exn.kk), and the async library's strand-killer is likewise `final ctl discontinue() : a` (lib/std/async/async.kk). `raw ctl` exposes a first-class `rcontext` that can be resumed or finalized later even from another scope — the escape hatch for schedulers. `finally`/`initially` combinators run cleanup on normal return and on unwinding for non-resuming operations, because multi-shot resumption makes RAII-conditional-on-single-resumption wrong (tour, "fread/with-file" example; the docs admit "there is active research into using the type system to statically prevent this").

**Linear effects.** `pub linear effect async-cancel` appears verbatim in lib/std/async/internal.kk. Linear effects are documented (tour "Linear Effects") as effects whose operations are always tail-resumptive and use only linear effects themselves; that "removes monadic translation for such effects and can make code more compact and efficient". This is the typed form of the insight we need: *the expensive part of effect handlers is capturing a continuation; if the type system guarantees tail-resumption, capture disappears*. Relevant prior paper: Leijen & Brachthäuser, "Taming Control-flow through Linear Effect Handlers" (HOPE 2018).

**Async as an effect.** The modern library (doc comment in lib/std/async/async.kk) is based on Leijen, "Structured Asynchrony with Algebraic Effects" (MSR-TR-2017-21, https://www.microsoft.com/en-us/research/wp-content/uploads/2017/05/asynceffects-msr-tr-2017-21.pdf). The async effect is an alias of four primitives:

```koka
pub effect discontinue
  final ctl discontinue() : a

pub alias async = <async-await,async-ioc,async-cancel,discontinue>
```

`async-await` performs `do-await(setup, scope, label)`; the setup callback runs in the *handler scope* and later resumes the strand with an `await-result<a> = Cancel | Exception | Result`. `async-cancel` is a linear effect holding the current cancellation `async-scope` (a chain of unique ids; child scopes form a tree). Cancellation is observed as `Cancel` results delivered to outstanding operations, which the wrappers translate into `discontinue()`, so `finally` blocks still run — cancellation-with-cleanup without thread interruption. Version history (doc/spec/news.mdk): 3.1.0 (2024-02) redesigned named-effect typing; the full `std/async` module (event loop, channels, interleaving) first shipped in 3.2.9 (2026-09-17); experimental parallel tasks have existed in `std/os/task` since 2.2.1 (2021): `task : (() -> a) -> promise<a>`, `await`, `parallel` implemented on a C thread pool (`kk_task_schedule`), with experimental LVars marked "currently unsafe in the pure effect!! needs work".

Structured combinators (lib/std/async/interleave.kk) are plain library functions over handlers — this is the punchline that async needs *no syntax*:

```koka
pub fun timeout( secs : duration, action : () -> <async,ndet|e> a ) : <async,ndet|e> maybe<a>
  firstof { wait(secs); Nothing } { Just(action()) }

pub fun two/interleaved( action1 : () -> <async|e> a, action2 : () -> <async|e> b ) : <async|e> (a,b)
```

Channels are likewise a library module (lib/std/async/channel.kk) whose types expose exactly what is going on: `receive : channel<a> -> <async,ndet> a` is honest about being nondeterministic (two emitters racing), while `emit : (channel<a>, a) -> async ()` is not; the channel state machine (`Empty | Values | Waiting(listeners)`) is ordinary data with `ioc`-scoped mutation. The sample program (samples/async/basic.kk) reads like Go with no goroutine keyword: `val (a,b) = interleaved(stranda, strandb)` and `val r = firstof(slow, fast)` — structured spawn, racing, and timeout are three combinators, not three language features.

Two further typing features visible in the core library matter for a capability design. First, Koka's effect vocabulary includes permission-style atomic effects (`net`, `fsys`, `ui`, `blocking`, core.kk) that act as *checked capability names* — the row is a permission audit. Second, "named effects" (`pub type nmd :: X`, `pub type scope :: S -> X`, core.kk; redesigned in 3.1.0 per news.mdk) attach a scope parameter to an effect so that values of the effect's evidence cannot escape their handler — a type-level fix for the dynamic-scoping leak that plagues first-class resumptions everywhere; the tour's `raw ctl`/`rcontext` escape hatch coexists with it.

Note `<ndet>` in the types: racing two strands is nondeterministic, and the row says so; `collapse-ndet`/`pretend-no-ndet` combinators re-certify determinism where the implementation guarantees it (e.g. `list/interleaved` promises the output list equals the input order). No mainstream language tracks nondeterminism in types; for DreamScript with immutable data this is nearly free to expose.

The "not a function color" claim, stated precisely: `wait` has type `duration -> async ()`, so the async color is visible and inferred — Koka does not hide it. What Koka removes is everything that makes coloring *hurt*: (1) there is no `await` keyword; performing `wait` is an ordinary call, (2) effect-polymorphic combinators (`map`, `bracket`, user code) are written once and work under any handler, including the async one, (3) the event loop is just the default handler installed around `main` when it has the `async` effect (lib/std/async/internal.kk doc comment), (4) sync and async code intermix — a sync function can be called from async and vice versa without API duplication. The residual costs are real, though: every combinator you write carries a row variable; error messages mention rows; and two codebases can still disagree about which effects they permit (the moral coloring problem moves into inference).

**How handlers compile (no stack switching).** The runtime contract is visible in lib/std/core/hnd.kk (the compiler-emitted handler primitives) and kklib/include/kklib.h:

- Handlers are evidence: an existential quadruple `(marker, handler, htag, evidence-vector)` where `marker : marker<e,r>` is a unique integer standing for the current answer context `<e,r>`, and the handler "contains all operations (much like a virtual method table)" (hnd.kk header).
- The thread context carries an evidence vector `evv` plus an inline yield record `kk_yield_t { marker, clause, conts_count, conts[8] }`; on perform, generated code sets `ctx.yielding` and the runtime finds the handler whose marker matches, calling its clause with the operation arguments; "the final continuation `k` is composed as `fN ○ ... ○ f1`" and if the 8-slot array fills, a fresh array holds the composed closure (kklib.h, `kk_context_t`).
- So: continuations are heap-allocated function compositions, single allocation per capture segment, no machine-stack switching, works identically on C and JavaScript backends. The formal foundation is Xie & Leijen, "Generalized Evidence Passing for Effect Handlers, or Efficient Compilation of Effect Handlers to C" (ICFP'21, https://www.microsoft.com/en-us/research/publication/generalized-evidence-passing-for-effect-handlers-or-efficient-compilation-of-effect-handlers-to-c/).
- Tail-resumptive operations skip capture entirely (the clause runs with the caller's continuation), which is why `linear effect` exists.
- Memory management is Perceus: precise reference counting with static reuse analysis ("garbage free"; reuse analysis lets functional code mutate in place — "functional but in place", FBIP; tour "FBIP" section; Reinking, Xie, de Moura, Leijen, PLDI 2021). Perceus + linear types are natural allies: a linear value's RC is statically 1, so its RC ops can be deleted.

**Implications for a bytecode-VM DreamScript.** The evidence-passing ABI is implementable in an interpreter without OS-thread machinery: handlers become (marker, op-table) pairs in a context vector; perform = walk vector, capture = materialize the frame chain between perform and handler into closures. Multi-shot resumption = replay closures (must not hold linear resources — type system must forbid capturing linear values in multi-shot continuations, or restrict effects to tail-resumption à la `linear effect`). This is the cheapest sound design for effects in an IR/bytecode language that we found.

### 2. Effekt — capability passing, second-class effects, multi-prompt continuations

Repo: https://github.com/effekt-lang/effekt; publications: https://effekt-lang.org/publications (cloned 2026-11).

**Capability-based typing.** "Effects as capabilities: effect handlers and lightweight effect polymorphism" (Brachthäuser, Schuster, Ostermann, OOPSLA 2020, doi:10.1145/3428194) is the language's citation-of-record. Every effectful function takes its handler as an explicit second-class parameter (a *capability*); effect polymorphism exists but is contextual: binders track what capabilities a block may use, and those capabilities cannot be captured by first-class values and escape their scope. The scheduler case study (examples/casestudies/scheduler.effekt.md) shows the whole model:

```effekt
interface Proc {
  def yield(): Unit
  def fork(): Bool
  def exit(): Unit
}

def scheduler { prog: => Unit / Proc } = region this {
  var queue: Dequeue[() => Unit at {this, prog}] in this = emptyQueue();
  def run(): Unit = queue.popBack match { ... }
  try { prog() } with Proc {
    def yield() = { queue = queue.pushFront(box { resume(()) }) }
    def fork()  = { queue = queue.pushFront(box { resume(true) })
                                 .pushFront(box { resume(false) }) }
    def exit()  = ()
  }
  run()
}
```

The commentary is the important part: `prog` is second-class, so it may use capabilities *not reflected in its type* (contextual effect polymorphism); a resumption stored in a mutable cell must record `{this, prog}` as its capture set, and because the cell itself is second-class (a `region`-scoped variable), "the resumption cannot leak even though it is stored in the mutable cell". This is regions-as-lifetime-tracking doing the job Rust does with lifetimes — and the payoff is compilation efficiency.

**Compilation.** The pipeline lowers to an explicit machine IR with `Reset(prompt, frame, rest)`, `Resume(value, rest)`, `Shift(name, prompt, rest)` instructions (effekt/shared/src/main/scala/effekt/machine/Tree.scala via machine/Analysis.scala), i.e. multi-prompt delimited control over an explicit stack. The LLVM runtime (libraries/llvm/rts.ll) implements real stacks: each frame is `{ReturnAddress, Sharer, Eraser, payload...}` with reference-counted environments, each handler has a unique `%Prompt`, and capturing saves the stack segment between prompt and current pointer; resuming restores it. Papers: "Capability-Passing Style for Zero-Cost Effect Handlers" (ICFP 2020), "A Typed Continuation-Passing Translation for Lexical Effect Handlers" (PLDI 2022), "From Capabilities to Regions: Enabling Efficient Compilation of Lexical Effect Handlers" (OOPSLA 2023), "Multiple Resumptions and Local Mutable State, Directly" (ICFP 2025), plus a tracing-JIT paper aimed squarely at effects (Gaißert, Bolz-Tereick, Brachthäuser, OOPSLA 2025, https://pl.cs.uni-tuebingen.de/publications/gaissert2025tracing.pdf) — directly relevant to a DreamScript JIT tier. Backends shipped in effekt/jvm/src/main/scala/effekt/Backend.scala: `llvm`, `js`, and three Chez Scheme schemes (`chez-cps`, `chez-monadic`, `chez-callcc`) — the same semantics proven expressible across four execution strategies.

The headline claims: because capabilities are second-class and handlers lexical, the common case never heap-allocates continuations; effect handler benchmarks are competitive with hand-written direct-style code (OOPSLA 2023 paper). The cost is expressiveness: no first-class resumptions (the scheduler works only because the queue lives inside the handler's region), no deep dynamic scoping, and the effect "color" still shows in block types (`=> Unit / Proc`) — but users never write row-polymorphism gymnastics; they pass capabilities.

**Errors.** Effekt's stdlib makes the Result/effect duality explicit (libraries/common/exception.effekt + result.effekt):

```effekt
interface Exception[E] {
  def raise(exception: E, msg: String): Nothing
}

type Result[A, E] {
  Error(exception: E, msg: String);
  Success(a: A)
}

/// It is the "reification" of the Exception[E] effect.
def result[A, E] { f: => A / Exception[E] }: Result[A, E] =
  try { Success(f()) } with Exception[E] { def raise(exc, msg) = Error(exc, msg) }

/// Monadic "reflection"
def value[A, E](r: Result[A, E]): A / Exception[E] = r match {
  case Success(a) => a
  case Error(exc, msg) => do raise(exc, msg)
}
```

Note `Exception[E]` is *indexed by the error type*: the effect row carries the error set, exactly the "what can go wrong is in the signature" property, but with control-flow semantics. For DreamScript this is the cleanest existing demonstration that "return-type union of errors" and "error effect row" are interdefinable; the choice is about ergonomics (values can be stored/returned/composed as data; effects propagate across higher-order calls automatically).

### 3. Eff, Frank, OCaml 5 — the rest of the effect lineage

Eff (Preu & Plotkin, "Handling Algebraic Effects", 2017; implementation "Eff directly in OCaml") is the minimal dynamic-scoped handler language; historically important, not a design fit (untyped dynamic handlers). Frank (Lindley, McBride, McLaughlin, "Do be do be do: A Functional Pearl on Effect Handlers" — actually titled "Do be do be do", POPL 2017, https://homepages.inf.ed.ac.uk/slindley/papers/frank.pdf) introduced *bidirectional* inline handlers (`do`/`handler` with `yield->k` bindings), which read beautifully for control abstractions but its effect inference (Frank-style generalization after typechecking) is exactly what Unison later found unsound in their setting (see Unison docs/ability-typechecking.markdown) and what row inference in general struggles with.

OCaml 5 shipped effect handlers in the multicore runtime (5.0, Dec 2022). Status as of 5.4 (Oct 2025, https://ocaml.org/manual/5.4/): the manual still hosts effects under an "experimental" chapter; 5.2 added shallow handler syntax, 5.3 (Jan 2025) added first-class *deep* handler syntax (https://ocaml.org/changelog/2025-01-08-ocaml-530); the ecosystem effect-based IO library is Eio. The shallow form, which shows perform/continue directly:

```ocaml
type 'a continuation                    (* opaque *)
effect Get : int continuation          (* effect carrying a continuation *)

let ask () = perform Get
let answer =
  match ask () with
  | effect Get k -> continue k 42      (* handler receives the continuation *)
  | v -> v
```

Lessons: (1) handlers in a battle-tested native runtime are fast enough to ship but the *API/typing* story is what stalls stabilization — OCaml has no effect typing at all, so the signature audit Koka/Roc give is absent and adoption relies on convention; (2) OCaml sidesteps coloring entirely — every effect including IO is an effect, so there is no two-world split (Eio programs look like ordinary code); (3) there are real happy-path overheads relative to OCaml 4 code on stack-heavy workloads, and GC-parallelism regressions took several releases to fix. Cross-reference: Sivaramakrishnan et al., "Concurrent System Programming with Effect Handlers" (https://kcsrk.info/papers/system_effects_feb_18.pdf) — earlier Koka-era work showing async/IO/cancellation as handlers with fork/join, cancellation, timeouts as library code.

### 4. Unison — abilities: the minimal viable effect system

Repo: https://github.com/unisonweb/unison (docs/ability-typechecking.markdown is the key internal design note). Docs: https://www.unison-lang.org/docs/fundamentals/abilities/. Unison 1.0 released Nov 2025 (https://news.ycombinator.com/item?id=46049722).

Unison keeps exactly one built-in I/O ability plus `Exception`. Types carry required abilities on the arrow: `a ->{IO, Abort, State Nat} b`. The checking rule is deliberately simple (quoting the design note): "the ambient abilities at a subterm is defined to be equal to the required abilities on the type of the *nearest enclosing lambda*, plus the abilities eliminated by enclosing handlers". So `foo : Text ->{} ()` calling `IO.printLine` fails, because the ambient set of the enclosing lambda is empty; partial application gets the ability only after the binding that carries it. User-defined abilities and handlers are pattern-matching on the request with an explicit resumption; the docs' mental-model page (https://www.unison-lang.org/learn/fundamentals/abilities/, which presents the idea in simplified pseudocode) shows the KVStore shape — the handler threads its own state through the resumption:

```unison
mapBasedStorage map =
  case (KVStore.put key value -> resume) ->
    updated = put key value map
    resume () with mapBasedStorage updated
  case (KVStore.get key -> resume) ->
    desiredValue = get key map
    resume desiredValue with mapBasedStorage map
-- real Unison binds this with `handle body with handler`
```

Unison originally considered Frank-style generalization and rejected it: "I realized it's not sound to do Frank-style effect generalization after typechecking" — instead `a -> b` means `a ->{e} b` for one existential `e` refined by checks. Handlers use `handle ... with ...`, and the `run` command supplies the IO/Exception handler at the top level (docs "Running Unison programs", https://www.unison-lang.org/docs/usage-topics/running-programs):

```unison
main : '{IO, Exception} ()
main = do
  printLine "Hello"
-- ucm: .> run main
```

What Unison buys for that simplicity: every effect operation is an ordinary delayed function call dispatched through the ability — no row-polymorphism zoo, no effect subsumption puzzles; tests can replace IO with pure handlers (the FAQ highlights pure test handlers); content-addressed code + `{IO}` in signatures gives precise effect audits. What it pays: only one coarse IO ability means you cannot distinguish file access from network access in types (Koka can, via `net`/`fsys`); concurrency is runtime-level (builtins include `IO.forkComp.v2 : (() -> a) -> IO ThreadId` in parser-typechecker/src/Unison/Builtin.hs) rather than a typed effect; and user-defined ability dispatch is value-level (handlers are ordinary functions receiving `Request`), so multi-shot resumption is possible but untracked.

**Takeaway for DreamScript:** the "abilities-lite" point on the effects spectrum — one or two built-in effects, ambient checking, existential rows, no effect-polymorphism combinator zoo — is cheap to implement and cheap to learn, and delivers most of the audit value (which functions touch the world) at a fraction of Koka's inference complexity.

### 5. The function-color problem and the escape attempts

Reference points: Paul Phillips' "What Color is Your Function?" (journal.stuffwithstuff.com, 2015, https://journal.stuffwithstuff.com/2015/02/01/what-color-is-your-function/) defined the problem: async functions form a second color that cannot call sync functions without blocking or vice versa without spawning, `await` is viral, and you pay for the split in API duplication. without.boats, "Coroutines and effects" (Apr 20, 2024, https://without.boats/blog/coroutines-and-effects/ — the essay the author read) adds the structural analysis: effect systems (Koka/Effekt) and coroutines (Rust async/generators) are in some ways isomorphic — "statically typed but dynamically scoped" control transfer; with coroutines the caller forwards effects explicitly at each call site (`.await?`), with effect systems forwarding is implicit through the function type; the author's stated ideal is stackful coroutines to permit recursion. Six escape attempts follow.

**5.1 Zig — remove async, rebuild as capability passing.** Zig's original colorless async/await (a compiler-figured-out suspension graph) was removed in 0.11 (2023) after maintainers concluded much of the implementation was unsound; 0.14/0.15 shipped without any async. 0.15.1 (Aug 2025) landed the "Writergate" I/O redesign including the new `std.Io` interface (https://ziglang.org/download/0.15.1/release-notes.html); 0.16 brings async/await back as a thin layer over it (devlog "Async DNS Resolution", Oct 15 2025, https://ziglang.org/devlog/2025/; HN discussion "Zig's New Async I/O", https://news.ycombinator.com/item?id=44545949). The new model (devlog code): every I/O-capable function takes an `Io` parameter — `pub fn connect(host_name: HostName, io: Io, port: u16, ...)`, where `Io` is an interface (vtable) whose implementation is blocking-threaded or evented (io_uring). Concurrency primitives are methods of that capability: `io.async(fn)` spawns colorless concurrent work; `Io.Queue` and `Io.Group` collect/cancel results; cancellation is explicit (`group.cancel(io)`, `error.Canceled`) and composes with `defer`. Kelley's claimed properties for the DNS example: works optimally over threads, event loop, or single-threaded blocking; requires no heap allocation; "We still use all the regular control flow: try, defer, while". Analysis for DreamScript: Zig did not defeat coloring by magic — it made the color an explicit *capability parameter* (`Io` is the row member, passed at runtime), and used interface dispatch instead of type-level effect polymorphism. This is Effekt's capability model with no effect system at all, and it is the strongest evidence that "capability-passing async" is deployable in a systems language with zero type-system machinery.

**5.2 Go — stackful colorless by construction.** Goroutines are stackful coroutines (2KB growable stacks) scheduled preemptively by an M:N runtime: user goroutines (G) multiplexed onto OS threads (M), each executing on a logical processor (P) with a local run queue; the scheduler work-steals and, crucially, *integrates the netpoller* (epoll/kqueue/IOCP in src/runtime/netpoll.go) so that a goroutine blocking on network I/O parks and its P moves on to other work — the event loop and the scheduler are one thing, which is why there is no async library split at all. Since Go 1.14 (2020) non-cooperative async preemption interrupts compute loops via signals (design doc: https://go.googlesource.com/proposal/+/master/design/24543-non-cooperative-preemption.md), closing the "tight loop starves the scheduler" hole; syscalls are handled by detaching the M or by netpoller registration. Consequences: one function color; blocking-style code everywhere; FFI calls and syscalls absorbed by the runtime. The cost side is real too — stack-growth checks on function entry (a prologue compare-and-branch per call), stack copying on growth, ~2KB floor per goroutine, and (the actual pain) unstructured concurrency: `go f()` orphans tasks, cancellation is a bolted-on `context.Context` parameter convention threading through every API that wants to be cancellable, errors from vanished goroutines are lost, and leak detection is manual ("Lies we tell ourselves to keep using Golang", fasterthanli.me, https://fasterthanli.me/articles/lies-we-tell-ourselves-to-keep-using-golang — already in the author's reading list; note that essay's other half: Go's data-race freedom story is absent, the race detector is dynamic). Go proves colorless stackful async is excellent *if* the runtime absorbs the cost; it also proves that colorless + unstructured still has a high error rate, which is what Trio/Kotlin then fixed on the structure axis.

**5.3 Java Loom — make the color disappear by making threads cheap.** Virtual threads (JEP 444, final in Java 21, 2023) are stackful continuations managed by the JVM: a virtual thread mounts onto a carrier OS thread (ForkJoinPool) and *unmounts* when it blocks on any of the JDK's blocking APIs — the same `Socket.read` that blocked for 20 years now parks a heap-allocated stack chunk instead. No new syntax, no `await`, no executor split: the color problem is erased by reinterpreting the existing blocking API set. Status 2025-2026: Scoped Values final in Java 25 (JEP 506, Sept 2025); Structured Concurrency remains preview after six iterations — JEP 505 (5th preview, JDK 25), JEP 525 (6th preview, JDK 26), JEP 533 (JDK 27) (https://openjdk.org/jeps/505). The pinning wart (`synchronized` blocks pinned carriers) was fixed by JEP 491 in JDK 24 (Mar 2025). The instructive part: threading was never the hard part — `StructuredTaskScope` joiners, cancellation policy, and exception propagation semantics took six preview cycles and are still contested. Structure is where design effort goes; the mechanism is cheap.

**5.4 Kotlin — keep the colors, win on structure.** Kotlin `suspend` functions compile to state machines; the color split is real (suspend vs non-suspend APIs, `suspendCancellableCoroutine` glue), yet adoption stuck because (a) the JVM interop means blocking libraries still work, and (b) structured concurrency is the default: quoting https://kotlinlang.org/docs/coroutine-context-and-dispatchers.html — "When a coroutine is launched in the CoroutineScope of another coroutine, it inherits its context via CoroutineScope.coroutineContext and the Job of the new coroutine becomes a child of the parent coroutine's job. When the parent coroutine is cancelled, all its children are recursively cancelled, too"; "A parent coroutine always waits for the completion of all its children". The canonical example from that page:

```kotlin
val request = launch {
    launch(Job()) {        // independent: survives parent cancellation
        delay(1000); println("job1: I am not affected by cancellation")
    }
    launch {               // child: cancelled with parent
        delay(100); delay(1000); println("job2: never printed if parent cancelled")
    }
}
delay(500); request.cancel()
```

Exceptions propagate up the Job tree by default (a failing child cancels parent and siblings); `SupervisorJob` opts out for UI-style supervision; `coroutineScope { }` is the builder that suspends until all children complete and fails if any child fails. So Kotlin's data point: even with colors, if the structure (Job tree, `coroutineScope {}`, cooperative cancellation) is right, the error rate problem mostly evaporates — and conversely the color split remains the recurring annoyance (it is the top complaint in every Kotlin async thread).

**5.5 Trio and structured concurrency.** Nathaniel J. Smith, "Notes on structured concurrency, or: Go statement considered harmful" (Apr 2018, https://vorpus.org/blog/notes-on-structured-concurrency-or-go-statement-considered-harmful/): concurrent tasks should live in a *nursery* — `async with trio.open_nursery() as nursery: nursery.start_soon(job)` — so that (1) task lifetime is lexically scoped and cannot outlive the block, (2) the nursery does not exit until all children finish, (3) cancellation is a scoped, cooperative operation (`cancel_scope`), and (4) errors from children are re-raised in the parent as `ExceptionGroup` (this design went into Python 3.11's `TaskGroup` and `except*`). Smith's deeper argument: structured concurrency does for control flow what structured programming did for jumps — it restores the invariant that a function's dynamic extent bounds its effects, which is exactly the invariant effect handlers already have. Structured concurrency + immutable data + linear types compose unusually well: children can only receive copies or linear/unique values (no shared mutable state to race on), cancellation is the only "effect" a child can perform on its parent, and the nursery is just a handler for it.

**5.6 Rust — colored, typed, and painful.** Rust async/await compiles to stackless futures (state machines), `Pin` exists because self-referential generators must not move (unsound to move; the API leaks into user code whenever you store futures), and the poll-based model means cancellation is *dropping the future* — drop runs destructors mid-`await`, which is why cancellation safety is a documented footgun of every combinator. The 2024-2026 pain points: (1) Send bounds on returned futures are unnameable for `async fn` in traits — identified as the biggest remaining flaw of the sugar (https://internals.rust-lang.org/t/specifying-bounds-on-futures-returned-by-async-functions/19740); the shape of the problem:

```rust
trait Fetch {
    async fn fetch(&self) -> Bytes;      // which runtime? Send or !Send?
}
// A multi-threaded runtime demands:  Pin<Box<dyn Future<Output=Bytes> + Send>>
// but nothing in the trait says so — the bound lives on the *returned future*,
// inferred from what the body holds across .await points, invisible at the
// call site until the spawned task fails to compile. Workaround:
fn spawn_fetch<F: Future<Output = Bytes> + Send + 'static>(f: F);
```

users fall back to `Pin<Box<dyn Future + Send>>` and concrete-runtime traits; (2) runtime fragmentation: tokio is the de facto standard, async-std is unmaintained, and runtime-specific traits fragment the ecosystem (https://www.reddit.com/r/rust/comments/day0x0/figthting_the_async_fragmentation/); (3) the `Send`-across-`await` analysis produces opaque errors because it depends on what values are live across suspension points — a value held across `.await` makes the whole future `!Send` in a way callers cannot see from the signature. without.boats' conclusion (Coroutines and effects, 2024) is that stackful coroutines would be the better foundation but Rust cannot retcon them safely. Rust is the existence proof that coloring + ownership inference (Pin/Send/Sync) *together* exceed a tolerable learning budget — important because DreamScript has linear types and must avoid re-creating this stack.

**5.7 Swift 6 — strict concurrency via ownership-inference-lite.** Swift 6 enables complete data-race checking by default. Key proposals: SE-0414 "Region Based Isolation" (Swift 6.0, 2024, https://github.com/swiftlang/swift-evolution/blob/main/proposals/0414-region-based-isolation.md): the compiler computes *isolation regions* and may transfer a non-Sendable value across an isolation boundary when region analysis proves the caller will not use it afterwards; SE-0430 "`sending` Parameter and Result Values" (https://github.com/swiftlang/swift-evolution/blob/main/proposals/0430-transferring-parameters-and-results.md) exposes that transfer explicitly; SE-0461 (Approachable Concurrency, Swift 6.2 era, https://github.com/swiftlang/swift-evolution/blob/main/proposals/0461-async-function-isolation.md) makes `nonisolated(nonsending)` the default so nonisolated async functions run on the caller's actor — collapsing a whole class of Sendable workarounds. The shape of the SE-0430 rule:

```swift
func process(input: sending Buffer) async -> Summary {
    // Buffer is not Sendable, but `sending` promises the caller will not
    // touch it while `process` runs — the compiler verifies that, and the
    // value may cross the actor boundary (move-in / copy-out of the region).
    let s = await actor.work(input)   // input is dead in the caller afterwards
    return s
}
```

Read against DreamScript: Swift is retrofitting, by inference, exactly the "unique ownership transfer" reasoning that linear types give you syntactically; a linear value is by construction `sending`, and an immutable value is by construction `Sendable`. The remaining Swift machinery (actors, global-actor inference, Sendable conformances) exists to approximate what DreamScript gets from linearity + immutability — strong evidence that the DreamScript primitive set attacks the right problem, and that the *scheduling* side (actors, executors) is orthogonal and optional.

**5.8 Scorecard: which escapes actually won.** Judged as of 2026: colorless-by-runtime (Go, Loom) won deployment outright — no syntax, no inference, billion-user codebases; its unresolved half is structure (Go leaks, Java's six-preview StructuredTaskScope) and Go's absent data-race story. Colored-but-structured (Kotlin) won mindshare among framework ecosystems because the Job tree fixes the error-rate problem the color problem was blamed for. Capability-passing (Zig 0.16) is the newest and the most interesting for a new language: it gets scheduling abstraction without any type-system feature, at the cost of a threaded parameter. Effects-first (Koka) won the argument intellectually — async, exceptions, iterators as one mechanism, no await keyword — but remains a research language, and the two features that make it pleasant (row inference, hover-driven development) are exactly the two features hardest to ship in a young language. The lesson for DreamScript is not "pick a winner" but "the winning designs all move the color into one place — a runtime fiber, a Job tree, or a capability/row — and then build structure on top"; the color is never free, it is only ever *contained*.

### 6. Isolation models without a borrow checker

**Pony — reference capabilities + causal messaging.** Pony (https://www.ponylang.io; paper: Clebsch et al., "Co-Designing a Type System and a Runtime", https://www.ponylang.io/media/papers/codesigning.pdf) types every reference with a capability: `iso` (isolated, writable, uniquely referenced), `val` (immutable), `ref` (mutable, actor-local), `box` (read-only), `tag` (identity only), `trn` (write-unique, transitionable). Actors communicate by asynchronous messages; data-race freedom follows because only `val` (immutable) and `iso` (unique) data can cross actors:

```pony
class Mailer
  be send(to: Receiver tag, msg: String iso) =>
    // `iso` param: caller must give up all other references ("recover"),
    // so the message is exclusively owned by this actor upon receipt
    ...
```

Causal messaging guarantees "a message is guaranteed to be processed before any message that depends on it" (Drossopoulou et al.; InfoQ talk https://www.infoq.com/presentations/pony-types-garbage-collection), enabling a distributed causality-tracking GC (ORCA) with per-actor heaps. Actors "can't block": behaviours are cooperative, schedulers are work-stealing, one per core; blocking FFI requires care. Perf profile: good (per-actor heaps, no global GC pauses), but RC traffic on cross-actor `iso` transfers and the inability to block make some patterns awkward; adoption remains niche and the language is still beta (0.72.x, 2025). The learn-cost verdict from a decade of community experience: the capability lattice (`iso`/`val`/`trn` recover expressions) is the hard part — Pony needs six capabilities because it has no linear types; a language with linear types needs roughly two (linear/owned + shared-immutable) — see Inko.

**Verona — regions, dormant.** Microsoft Research's Verona (https://github.com/microsoft/verona) grouped mutable data into regions with `snd`/`rcv` annotations and used `cown` (concurrent owner) objects + behaviours for coordination. The language effort stalled ("not ready to be used outside of research", original design on `old_version` branch); the idea pivoted to Pyrona — "Dynamic Region Ownership for Concurrency Safety" (PLDI 2025) retrofitting dynamic region ownership onto Python with deterministic dynamic failures. Lesson: static region/send-receive discipline is hard to sell as a *whole language*; the research value survives in smaller doses (Swift's regions, linearity).

**BEAM (Erlang/Elixir/Gleam) — isolation + preemption + let-it-crash.** Each BEAM process has its own heap, GC'd independently; messages between processes are *copied* (large binaries refcount-shared), so there is no shared mutable state to race on:

```erlang
loop(Count) ->
  receive
    {incr, N} -> loop(Count + N);    % N was deep-copied into this heap
    stop      -> ok
  end.
```

Scheduling is preemptive by reduction counting (~4,000 reductions per process time slice), giving soft-real-time latency fairness regardless of what any process does — no cooperative-yield footguns. Error handling is architectural: let-it-crash — a process that hits an unexpected state dies, its supervisor restarts it, and the "error kernel" pattern keeps the restartable core small. Perf sacrificed: copying sends (fine for small messages, a cost for large ones), boxed arithmetic historically (largely fixed by the JIT, OTP 24+), and no intra-process parallelism (you scale by processes). Gleam (https://gleam.run) shows what a statically-typed, Result-only, effect-free language looks like on BEAM: no exceptions in user code (`panic` exists for bugs), errors as `Result` (`pub fn parse(path: String) -> Result(Data, ReadError)`), message passing via process subjects with types checked where values cross. DreamScript-relevant: BEAM's model is the *lowest error-rate* concurrency design known, and its costs are exactly the ones an immutable-by-default linear language can mitigate (copying is unnecessary when messages are linear — you can move instead of copy).

**Inko — the closest existing "linear messages, no async" language.** Inko (https://inko-lang.org; vendored in this repo at inko/, docs at inko/docs/source/getting-started/concurrency.md and design/runtime.md) uses lightweight processes that do not share memory: "values are moved between processes", and a value is sendable "if it's either a value type (String or Int for example), or a unique value, of which the type signature syntax is `uni T`"; mutable borrows are never sendable. `uni T` is Pony's `iso` renamed, created via `recover` (`let a = recover [10, 20]  # => uni Array[Int]`). Processes are defined `type async Counter { ... fn async mut increment(amount: Int) { @number += amount } }` and interacted with via `async` methods — there is no async/await and no function coloring; async methods send messages. The runtime (docs/source/design/runtime.md): "Processes are scheduled onto a fixed-size pool of OS threads" (default = cores), work stealing with bounded local queues, "cooperative multitasking, driven by the compiler" via injected preemption points, plus a timeout worker for timers. Error handling (docs/source/getting-started/hello-error-handling.md): `Result` for expected errors, and panics that "can't be handled at runtime" — a process panic takes down the process, printing a stack trace. Inko demonstrates end-to-end that ownership-lite (unique values) + process isolation + colorless async methods is buildable and comprehensible — and it is Rust-implemented with a precise RC-managed heap, i.e. performance-class relevant to DreamScript. Note Inko is single-threaded *per process* — parallelism = many processes; intra-process data parallelism is explicitly not its model.

**Mojo — ownership plus parallelism, async still missing.** Mojo (https://www.modular.com/mojo; docs https://docs.modular.com/mojo/) has Rust/Swift-style argument conventions (`owned`, `borrowed`, `inout`) and a growing parallelism story (`parallelize`, `TaskGroup` in `runtime.asyncrt`), but async/await were WIP-then-reworked: community reports through 2025 show `TaskGroup` fragility and a "structured async" redesign proposal; Mojo 1.0 shipped in the Modular 26.5 release (Aug 2026) with async still a roadmap item. Lesson: even a well-funded team with an ownership system chose to ship parallelism first and defer async semantics — the semantics are genuinely hard, supporting the "get structure right before color" ordering for DreamScript.

### 7. Error handling models

Taxonomy cross-reference: typesanitizer, "An epic treatise on error models for systems programming" (https://typesanitizer.com/blog/errors.html), and the Yuan et al. finding it cites: "almost all catastrophic failures (92%) are the result of incorrect handling of non-fatal errors explicitly signaled in software".

**7.1 Result-as-data (Rust, Roc, Zig, Gleam).** Errors are values; propagation is explicit or lightly sugared (`?`, `try`). Zig's variant: error *sets* — functions return `!T` (error union) with named error sets (`error{FileNotFound, ...}`), `try`/`catch` operators, `errdefer` for scoped cleanup, and errors are a single machine-word tag set (no payload on the error itself; context lives in structs the caller already has):

```zig
fn read(path: []const u8) ReadError!Data {          // or inferred: !Data
    const f = try open(path);                        // propagate ReadError
    errdefer f.close();                              // scoped cleanup on error exit
    return f.read_all();
}
```

Rust's variant carries payloads in the error enum and propagates with `?` (`fn read(path: &Path) -> io::Result<Data>`), with `panic!` reserved for bugs. Roc's variant is the important one for DreamScript: no Option type at all, only `Try` with *inferred* tag-union error types — "if a function returns a potential error, it uses Try with an error type that has a single tag describing what went wrong", and "when one function calls another, the possible errors combine automatically into a tag union of all possible errors at that call site" (https://www.roc-lang.org/faq; https://news.ycombinator.com/item?id=41934360). That is: the error set is *inferred per function* and unions under composition, like an effect row but materialized in the return value.

**7.2 Exceptions (unchecked), checked exceptions, and typed throws.** Unchecked exceptions (C++, OCaml, Python, JS): invisible in signatures, catastrophic for audit, but zero-cost on the happy path in JITs (table-driven unwinding). Java's checked exceptions are the cautionary datum: typesanitizer notes they are "uncommon in practice due to syntactic overhead and the difficulty of evolving APIs to return new error cases" — the *same* evolution complaint later leveled at effect rows, and the reason Roc/Koka-style *inferred* sets (where adding a case just widens an inferred union) are strictly better than annotation-burden designs. Swift is the interesting middle: it kept `try`/`catch` syntax (lighter than Result plumbing) but moved from untyped `throws` (any `Error` existential) to *typed* throws — SE-0413, shipped in Swift 6.0 — so `func read(path: Path) throws(ReadError) -> Data` now puts the error type in the signature. That trajectory (throw-syntax + error-typed signature) is the industry converging on exactly the "what can go wrong is in the signature" property, regardless of whether the mechanism is a return value or control flow.

**7.3 Effects-as-errors (Koka, Effekt).** Koka: `exn` is a builtin effect implemented as `final ctl throw-exn : a` (lib/std/core/exn.kk) — non-resumable, so semantically an exception; a plain `result<a,e>` type also exists in std/core (lib/std/core/result.kk) for data-style errors. The signature `() -> exn int` says "can throw" exactly like Java's `throws`, except inferred and row-composable. Effekt: `Exception[E]` parameterized by the error type (section 2), with `result` as its reification — the two are provably interdefinable in-language, so the language does not need both; it has both for ergonomics.

**7.4 Panic/trap split.** The pattern across new languages: two *kinds* of failure with different semantics. Rust: `Result` for expected, `panic!` (unwinding or abort) for bugs/invariants, `catch_unwind` as the narrow legal boundary. Zig: `error` unions for expected, `panic` for bugs, `errdefer` for cleanup. Swift: `throws` for expected, `fatalError`/preconditions for bugs. Inko: `Result` for expected, unhandleable process-terminating `panic` for bugs. Go: `panic/recover` in-between (recoverable, but culturally reserved for bugs), plus error values. BEAM: no such split — everything unexpected crashes the process by design. For DreamScript the split is near-free to implement in an interpreter (panic = abort the fiber with a trace; Result = data) and it directly serves the charter's "compile-time errors over runtime errors" preference: bugs trap loudly, expected errors are typed.

**7.5 Let-it-crash (BEAM).** Not an error model for values but for *systems*: supervision trees turn crashes into restarts. It pairs with processes/isolation (section 6) and with the panic/trap split: panics become supervision events instead of aborts when a supervisor exists.

**7.6 "Null, Errors and Enums Are One Feature" (https://www.arshad.fyi/writings/null-errors-and-enums-are-one-feature).** The post (already in the author's reading list) argues optionality, booleans, enums, and errors are all unions: `bool = true | false`, optional is `T | none`, an enum is "a union of unit types", an error is a struct variant; "a value that may be missing and may also fail is one flat set, `Data | none | Timeout`, not `Result<Option<T>, E>`"; handling is `is`/`match`/`or` with exhaustive matching; "nothing is unwrapped — the value was in the union all along". Prior art credited: Ceylon (null as union member), TypeScript (narrowing), Zig (error sets), Kotlin (`?:`). For DreamScript with set-theoretic types (charter) this is nearly the native idiom; the design question is only whether "flat union with an implicit unwanted tail" (the `or`-propagation operator) is worth a special propagation rule versus explicit `Result<T, E>`.

**7.7 Zero-cost exceptions in JITs vs Result returns in register VMs.** In JIT/AOT worlds (HotSpot, JVM, .NET, Itanium-ABI C++), exceptions are table-driven: the non-throwing path pays ~nothing, the throw pays heavily (unwind, handler search), which is fine because throws are rare; `Result`-style returns, by contrast, pay on every call (a tag check and a register/slot for the tag) but are cheap and predictable when errors occur. In a *bytecode interpreter* the calculus changes: every instruction is already a dispatch, a `Result` return is just "leave the tag in the return slot, branch at the consumer" (often fused with the consumer opcode), and a thrown exception is a frame-walk in the interpreter loop you already have (no OS unwinding). Measured folklore matches this: interpreter-level try/catch (Lua `pcall`, JVM interpreters) costs are dominated by frame management, not the mechanism; Result checks cost a predictable branch per fallible call and hurt register pressure in a *register* VM (DreamScript's likely shape). Practical synthesis for DreamScript: (1) Result-as-data for expected errors — one branch, no hidden control flow, composes with unions; (2) panic-as-trap for bugs — one code path; (3) if effects are adopted, `throw`-as-effect costs the same as any tail-resumptive operation call, and `finally`-style cleanup comes from handlers or linear destructors, not unwinding tables.

**7.8 Evaluating the old DreamScript idea: "error types collected into the function's return-type union".** The idea: a function `read(path) -> Data | NotFound | IoError`, callers `match` it, and call-site unions grow automatically. Assessment against Koka's rows, on five axes:

- Formal status: identical information. `read : Path -> <exn{NotFound, IoError}> Data` and `read : Path -> Data | NotFound | IoError` carry the same sets; Effekt's `Exception[E]` (an effect indexed by the error type) is the literal middle form, and Koka's denotational table (`int -> exn int` = `Z -> (Z+1)`) shows the semantics coincide. So the idea is sound; it is a *representation choice*.
- Propagation: the union version propagates through *data* (a caller returning the union just widens its own union — Roc's automatic composition); the effect version propagates through *control flow* (no return-path rewriting at all, works across generators/callbacks naturally). For higher-order combinators both need a row/union variable — `map : (A -> E B) -> List<A> -> List<B> | E` vs `map : (A -> e B) -> List<A> -> e List<B>` — same burden.
- Values vs control flow: a union-in-return is a value — it can be stored, sent to another process, cached, pattern-matched later; an effect `throw` is not a value — it cannot be stored (Koka's `raw ctl rcontext` is the hairy exception). If DreamScript wants to ship errors across processes or collect them (structured concurrency: a nursery collecting child errors *as data* — ExceptionGroup), the union/Result form wins outright.
- Panic path: either way you want a separate unhandleable trap for bugs (7.4); the effect form's `final ctl` maps 1:1 onto the trap.
- Learning cost: the union version is just "functions return sets" (the author's set-theoretic types); the effect version additionally requires understanding dynamic handler scoping. Given that DreamScript already plans rows-as-unions for types, an error-union-in-return-type is the *restricted effect row we already have* — recommendation: adopt the union/Result form as the base, and if/when full effects arrive, expose `throws` sugar as "this effect row is exactly your error union" (Effekt's `result`/`value` pair shows the translation both ways).

**7.9 What "immutable by default + linear types" already buys for concurrency, and what's left.** Buys: (1) no data races on shared state *within* the language — mutable aliasing is impossible; the entire Rust `Send`/`Sync` apparatus exists to approximate "no shared mutable access", and collapses to "linear values may move (like `sending`, SE-0430), shared values must be immutable (like `val`/Sendable-sync)" — two cases, both checkable trivially; (2) sendability for message passing is free: a linear value is Inko's `uni T`/Pony's `iso` without `recover` gymnastics; (3) deterministic parallelism: pure functions over immutable inputs can be run on any number of threads (`list/interleaved`-style certification, or Mojo's `parallelize`) with no locks; (4) cancellation/resource safety: linear resources cannot be leaked by a dropped future (Rust's cancellation-by-drop footgun is unsoundness-by-construction in a linear language — the destructor *must* run or the value must be consumed); (5) reference counting works (Perceus-style) with statically-removable counts for linear values. Left (what linear types do NOT give): (a) a scheduler — colors/fibers/processes are still needed for IO latency; (b) cancellation semantics (who can cancel whom, when do finalizers run — the nursery/structured layer); (c) supervision and error propagation across tasks; (d) multi-shot resumption interaction — multi-shot continuations can duplicate or defer linear values, so an effects design must either restrict resumption (Koka `linear effect`, tail-only) or forbid linear capture (Effekt's lexical capabilities); (e) deadlock-freedom (Pony's causal messaging, BEAM's design discipline — not implied by linearity).

### 8. Implementation notes for a bytecode-VM DreamScript

This section collects the evidence relevant to actually building the winner in an interpreted VM; it is the interface point with the runtime/IR research (03-*).

**Fibers in a bytecode VM are cheap and proven.** Lua's asymmetric coroutines are the canonical precedent: a coroutine's stack is a `CallInfo` array plus value slots on the heap; yielding swaps an interpreter-level stack pointer, never an OS stack — cost is O(frames-on-stack) at most, O(1) in LuaJIT's design. Inko's runtime (design/runtime.md) does exactly this at scale: fixed thread pool, work stealing, compiler-injected preemption points. For a register VM the fiber stack is a growable array of frames; spawn = allocate frame array + context; switch = save/restore IP, frame base, and the "current fiber" pointer in the thread context (compare Koka's `kk_context_t`, which is precisely a context object threaded through generated code). A 2KB-ish initial allocation (Go) or one-page frame array (Lua) is the right starting point; growth by reallocation with frame-pointer fixup is a solved problem.

**Preemption in an interpreter.** Go needs signals because compiled loops do not consult the scheduler; an interpreter can inject the check in the dispatch loop: every N instructions (or on backward jumps/loop headers and call boundaries), test a flag (Go's preemption flag, Inko's "preemption point"). This is strictly simpler and safer than signal-based preemption, with the caveat that N must be small enough for latency and the check is one predictable branch. Cooperative-only (Rust-style) is the anti-pattern to avoid: one hot loop stalls the pool.

**Evidence passing works in a register VM.** Koka's ABI (section 1) requires no stack switching and no special machine support: handlers = (marker, operation table) pairs held in a context vector; perform = set yielding marker + walk vector; capture = compose the continuations from the operation site to the handler as heap closures (KK_YIELD_CONT_MAX=8 inline slots before spilling). In a register VM the analog is: each frame holds a return-continuation; on perform, materialize the frame chain between the perform and the handler as closures (each closure captures the frame's registers and resume IP). Tail-resumptive operations are the fast path — the handler clause is called with the caller's continuation intact, zero allocation. This makes a "restricted effects" runtime (Combo 6) cost roughly one indirect call per effectful op, indistinguishable from a virtual call.

**Multi-shot resumption vs linear resources is the sharp edge.** A resumption used twice must not carry a linear resource (double-free or double-close by construction). Koka handles it operationally (`finally`/`initially`, tour section; "active research into using the type system to statically prevent this") and provides `linear effect` (tail-resumptive only) as the typed safe subset; Effekt confines resumptions to lexical handler scope where regions bound their lifetime; Unison tracks nothing and relies on GC. DreamScript, having linear types, can do better: forbid capturing linear values inside multi-shot continuations at typecheck time (linear = at-most-once use; a resumption is a use), or type resumptions as affine (at-most-once) unless marked. This is a novel but small typing rule with a big safety payoff — it is the one place where DreamScript's linear types make effects *safer than any existing effect language*, and worth stating in the design docs.

**Result returns in a register VM.** A `Result<T,E>` return is: leave tag in a designated return register/slot; consumer opcode (or fused `and_then`-style op) branches on tag. Tag + payload share one word if E is a pointer-tagged payload or NaN-boxed (arshad.fyi notes tags can hide in unused bit patterns: "so *Node | none is one machine word"); union-of-errors with zero-sized unit variants costs nothing extra. The branch is perfectly predicted on the happy path; the real cost is the extra register/slot pressure versus an exception's zero per-call cost — but an interpreter's dispatch overhead dominates that anyway. Panics/traps: raise a fiber abort, walk the fiber's frames for finalizers (linear destructors run here), surface at the nursery/supervisor boundary.

**FFI and pinning.** Any fiber design needs a rule for foreign calls that block or run callbacks on foreign threads: either run them on dedicated carrier OS threads (Loom), pin the fiber to its thread for the call's duration (Inko's documented FFI caveat), or require non-blocking FFI wrappers. DreamScript should make pinning the default semantic (documented, like JDK's JEP 491 history shows the cost of getting it wrong with `synchronized`) and provide the carrier pattern in the stdlib.

**Reference counting + linearity synergy.** If DreamScript adopts Perceus-style RC (Koka) or Inko-style precise RC, linear values have statically-known count 1 — their RC instructions are deleted at compile time, and their destructors are the cancellation/finalizer hooks. Immutable-by-default + RC also means no write barriers on shared data and fork-friendly snapshots (copy-on-write parallelism for the deterministic `par-map` of Combo 5). Koka's FBIP reuse analysis additionally shows how functional-style code over linear-ish data compiles to in-place mutation — relevant to "ultra-high performance" without exposing ownership gymnastics (charter item 6: prefer designs with low learning cost that keep the benefits).

## c) Tradeoff tables

Table 1 — Async/concurrency models (the color axis x the structure axis):

| Model | Colors? | Learning cost | Runtime perf | Cancellation/structure | Error rate | Exemplars |
|---|---|---|---|---|---|---|
| Colored stackless async | two colors, viral | high (Pin, Send, lifecycle) | best (no stack swap) | manual or combinators; drop-based | high (cancellation bugs) | Rust |
| Colored + structured | two colors | medium (one keyword, rules) | good (state machines) | Job tree, cooperative | low | Kotlin, Trio |
| Colorless stackful (VM fibers) | none | low (blocking style) | good in interpreters (cheap stack swap) | needs nursery layer added | low with nursery; high without | Go, Loom, Lua coroutines |
| Colorless capability (Io param) | none (explicit param) | low-medium (thread the Io) | depends on impl (threads or evented) | explicit (Queue/Group, error.Canceled) | medium (manual scoping) | Zig 0.16 |
| Async-as-effect | one inferred row | medium-high (rows) | good (evidence passing) | handler-scoped (cancel scopes) | low (scoped, typed) | Koka |
| Capabilities + lexical handlers | block types | medium (pass capabilities) | best known (direct style) | handler-scoped | low | Effekt |
| Actors/isolation | none | low-medium (process model) | good (no locks, copying sends cost) | supervision trees | lowest (let-it-crash) | BEAM, Pony, Inko |
| Single-thread + data parallel | none | lowest | limited (one core for tasks) | none needed | lowest | Unison-style IO, scripts |

Table 2 — Effect systems compared:

| System | Typing | Resumption | Compile strategy | First-class continuations | Async story | Status 2026 |
|---|---|---|---|---|---|---|
| Koka | inferred rows, full polymorphism, `linear effect`, `final ctl`, named effects | multi-shot allowed (resources unsafe, `finally` mitigates) | evidence passing to C/JS; heap closures | via `raw ctl` rcontext | async = 4 effects; std/async lib (3.2.9) | active, v3.2.9 |
| Effekt | capabilities as params, contextual polymorphism, second-class default | multi-shot inside lexical scope | direct style, multi-prompt conts, LLVM stacks | no (region-confined) | scheduler as library case study | active, research-grade |
| Unison | 1 builtin ability (IO) + Exception; ambient sets; existential rows | via ordinary handler functions | interpreter (Haskell RTS) | no | IO.forkComp builtin; untyped | 1.0 (Nov 2025) |
| OCaml 5 | untyped effects (no row typing) | deep (5.3 syntax) + shallow | native runtime, stack switching (heap-allocated OCaml stacks) | first-class | Eio library; no coloring | shipped, still "experimental" label |
| Frank | bidirectional inline handlers, row inference | multi-shot | research interpreters | yes | n/a | research |

Table 3 — Error models:

| Model | Signature shows it? | Propagation cost (VM) | Can store as value? | Exhaustiveness | Evolution (add error) | Exemplars |
|---|---|---|---|---|---|---|
| Result-as-data | yes (explicit or inferred) | 1 branch + slot per call | yes | via match | caller unions widen | Rust, Roc, Zig, Gleam |
| Error union in return type | yes (inferred union) | 1 branch + slot | yes | via match | automatic (Roc) | Roc, Zig error sets, old DreamScript idea |
| Exceptions as effect (`final ctl`) | yes (row) | perform/dispatch; tail-resume cheap | no (control flow) | via row in signature | automatic (rows) | Koka exn |
| Typed exception effect | yes (`Exception[E]`) | perform/dispatch | reifiable both ways | via E in type | widen E | Effekt |
| Checked exceptions | yes (throws) | JIT tables | no | no (compile check only) | breaking change | Java |
| Unchecked exceptions | no | JIT tables (cheap happy path) | no | no | silent | C++, Python, JS |
| Panic/trap | no (bugs) | abort fiber | no | n/a | n/a | Rust, Zig, Swift, Inko |
| Let-it-crash | no | process death | no | n/a | n/a | BEAM |

Table 4 — What the DreamScript primitive set already covers vs what concurrency must add:

| Concurrency problem | Covered by linear+immutable? | Missing piece needed |
|---|---|---|
| Data races | yes (no aliased mutability) | none |
| Sendability across tasks/processes | yes (linear = movable) | none |
| Deterministic parallelism | yes (pure fns over immutable data) | a par/map-reduce combinator + scheduler |
| IO latency (many concurrent ops) | no | fibers OR effects OR capability-passed Io |
| Function coloring | n/a | chosen async model (combo below) |
| Cancellation scoping | no | structured concurrency (nursery/cancel scope) |
| Resource safety under cancellation | partially (linear dtors must run) | scope-exit guarantee (nursery or handler) |
| Supervision / partial failure | no | supervisors or nursery error collection |
| Deadlock freedom | no | design discipline or causal messaging |

## d) Viable design combos for DreamScript

Constraints recap from the charter: interpreted (IR/bytecode), static linear/affine types, immutable by default, ultra-high performance, sugar budget at most Kotlin's, effects optional. The author's key question: can async (and effects) be abstracted so the benefits stay while learning cost and error rate drop? Each combo below states mechanism, learning cost, perf, error rate, syntax implications, and its effect-system implications.

**Combo 1 — "Structured colorless": VM fibers + nursery + Result (the Go/Trio/Inko synthesis).** Mechanism: stackful coroutines implemented in the VM (the frame stack is already interpreter data; swapping stacks is a pointer swap — LuaJIT/Lua precedent), scheduled on a core-sized thread pool with compiler-injected preemption points (Inko's design, docs/source/design/runtime.md) or IO-integrated parking (Go's netpoller); all IO/blocking APIs written in blocking style; the ONLY concurrency primitive is `spawn` inside a nursery block whose scope outlives children, collects their Result/error unions, and cancels them on exit; cancellation is cooperative via a checked flag at preemption points. Learning cost: lowest of all combos — no keywords, no colors, one new block form. Perf: excellent for an interpreter (no state-machine boxing, no Pin-analog; stack swap is O(1); per-fiber stacks are heap chunks like Loom's). Error rate: low *if* the nursery is mandatory (no bare spawn) — this is Go minus its one fatal flaw; panics trap the fiber and surface in the nursery's error union (or take down the process absent a supervisor). Syntax implications: `spawn` expression + `nursery { }` (or `concurrent { }`) block; ~Kotlin-level sugar. Effects: none exposed; internally, a nursery is a handler for a `cancel` effect — if effects ever arrive, the runtime does not change. Risk: FFI callbacks into fibers (stack switching vs C stacks) needs the Loom-style carrier trick or pinned-FFI rule. Sketch (DreamScript-syntax is illustrative only):

```dreamscript
fun fetch_all(urls: List[Url]) -> List[Data] | NetErr {
  nursery {
    for url in urls { spawn { fetch(url) } }   // children join at scope exit
  }                                            // default: first failure cancels all,
}                                              // surfaces as NetErr with child errors attached
```

**Combo 2 — "Capability-passing async" (Zig 0.16 / Effekt synthesis, no row polymorphism).** Mechanism: an `Io`-style capability (interface value) is an explicit parameter of all world-touching code, provided by thread-pool, event-loop, or blocking implementations; `io.async { }` (or `spawn under io`) gives structured concurrency primitives attached to the capability (queue/group/cancel), colored by nothing — the capability is data. Learning cost: low-medium — one idiom ("pass the io"), zero type-system features (works entirely with ordinary parameters/generics). Perf: as good as the chosen implementation; identical user code runs blocking (tests, scripts) or evented (servers) — the property the author's "abstract async" question asks for, delivered by parameterization. Error rate: medium — manual capability threading is noisy and forgetting to scope `io.async` work is possible (Zig mitigates with defer idioms; DreamScript's linear types + nursery combinators can fence it). Syntax implications: none new (a convention + stdlib), which suits the sugar budget exactly. Effects: this IS effects-lite — the capability is a first-class row member without row types; graduating later to row-polymorphic effects would be syntax-compatible. Sketch:

```dreamscript
fun connect(host: Host, io: Io, port: Port) -> Conn | NetErr {
  let group = io.group()                       // structured: cancels children on exit
  let q = io.queue[Conn]()
  group.spawn { q.put(dns_lookup(host, io)) }
  group.spawn { q.put(tcp_connect(q.pop(), io)) }
  match q.first_ok() { ... }                   // first success cancels the rest
}
// call sites choose the scheduler: connect(h, Io.blocking(), 443)
//                                    connect(h, Io.iouring(), 443)
```

**Combo 3 — "Koka-lite: async-as-effect with restricted rows".** Mechanism: full Koka-style row inference, but only *final* (non-resumable) and *linear* (tail-resumptive) effects in v1 — i.e., exceptions-as-effect and async-as-effect, no user multi-shot handlers; the event loop is the default async handler (Koka std/async model); cancellation scopes as a linear effect (lib/std/async/internal.kk is the template). Learning cost: medium-high — users never write rows (inferred), but library authors meet row variables; error messages about `<async,ndet|e>` unification are a real cost (Koka's own docs lean on hover UIs). Perf: good — evidence passing with tail-resumption-only effects means perform = indirect call, no capture; ndet tracking is compile-time only. Error rate: low — cancellation scoped by handlers, async composes with everything, no await-virality; determinism auditable via `ndet`. Syntax implications: essentially zero new surface syntax beyond `with handler` for advanced users — the lowest-sugar option, but the highest *type-inference* complexity in the compiler. Effects: this is full adoption (restricted); the risk is committing to row inference across the whole type system early (Koka's inference sits on decades of HM+rows research; DreamScript must also unify rows with linear/affine unification, an area with less prior art). Sketch:

```dreamscript
fun wait(d: Duration) -> async () = ...          // async is a row member, inferred;
                                                 // no await keyword anywhere
fun race2(a: () -> async a, b: () -> async a) -> ndet|async a =
  with cancel_scope {                             // handler-scoped cancellation
    firstof(a, b)
  }
```

**Combo 4 — "Actors + linear messages" (Inko/Pony synthesis).** Mechanism: processes with isolated heaps (or one heap with ownership discipline), messages are values moved in (linear/unique or immutable copies), async methods on process types, colorless (no async/await); scheduler = fixed thread pool + work stealing + injected preemption (Inko); optional supervision: a supervisor process receives child panic notifications (let-it-crash-lite) instead of full OTP trees. Learning cost: low-medium — the process model is intuitive, but "what can I send" rules (linear = move, shared = copy, mutable = no) need teaching (Inko's `recover`/`uni` pedagogy shows the friction points). Perf: good; no locks ever; copying avoided for linear sends (move is memcpy-free — transfer ownership); worst for fine-grained shared-memory parallel algorithms (must decompose into messages). Error rate: lowest for systems that stay in the model (isolation makes whole failure classes unrepresentable); process spam and message protocols are the new failure modes. Syntax implications: `type async`-style process types + `async` method marker (Inko) — modest sugar. Effects: none exposed; processes *are* the effect dispatcher (each async method call is a message perform). Notably compatible with Combo 1 (nursery = supervised spawn of processes). Sketch:

```dreamscript
process Counter {
  mut count: Int
  async fn bump(n: Int) { count += n }     // n must be Copy or linear (moved in)
}
let c = spawn Counter(0)
c.bump(3)                                   // message send: returns immediately
let total = await c.count()                 // awaiting a reply is a library choice,
                                            // not a language color
```

**Combo 5 — "Deterministic core + explicit parallelism, no async".** Mechanism: the language has no concurrency keywords at all; blocking IO is just blocking (thread pool underneath like any scripting language — Lua/Python model); parallelism only via data-parallel combinators (`par-map`, `par-reduce`) over immutable or linear-partitioned data, which are deterministic and race-free by the type system; the "async" story for IO-heavy servers is delegated to Combo 2's capability (opt-in library) or punted. Learning cost: lowest. Perf: worst for IO-bound concurrency (a thread per blocking op or event-loop library, colored at the library boundary); best for compute (deterministic parallelism has no synchronization). Error rate: lowest in-language, but users needing servers will reach for FFI/external async (ecosystem split re-appears at the library level — the coloring problem is not solved, only evicted). Syntax implications: none. Effects: none. This is the honest minimal core; the question is whether the author's target workloads are compute-shaped (then it wins) or IO-shaped (then it loses). Sketch:

```dreamscript
// deterministic data parallelism; closures capture immutables or linear partitions
let squared = xs.par_map(fn(x) { x * x })     // race-free by construction
let total    = xs.par_reduce(0, fn(a, b) { a + b })
```

**Combo 6 — "Effects-ready capabilities + nursery, full effects deferred" (recommended synthesis).** Mechanism: build the Combo 1 runtime (VM fibers + nursery + cancellation scopes) and the Combo 2 surface (all world-touching stdlib takes an explicit capability parameter, e.g. `io`); expose no effect types, no handlers in v1. Internally implement Koka's evidence-passing ABI for the two effects the runtime already needs (`cancel` at nursery boundaries, `discontinue` for strand teardown) so the semantics are handler-shaped from day one. Then user-defined resumable effects can be added later as library-level handlers without re-architecting, and async remains abstract: the same program runs blocking (single fiber), evented (event-loop Io), or threaded (pool Io) because scheduling is a capability, not a color. Learning cost: low (v1); the effects escape hatch costs nothing until used. Perf: Combo-1 perf. Error rate: Combo-1 error rate, with cancellation already handler-scoped internally. Syntax implications: nursery block + capability convention; sugar well under Kotlin's. This combo directly answers the author's question: the *benefits* of async and effects (composable cancellation, scheduling abstraction, audit of world-touching code) are kept via structure + capabilities + an effects-shaped runtime, while the *costs* (row inference, handler duality, color vocabulary) are deferred or eliminated. Sketch of the layered surface:

```dreamscript
// v1 surface: nursery + io capability. Two built-in "capabilities", zero syntax.
nursery {
  spawn { io.print_line("concurrent") }
}                                    // exit joins + cancels; errors union out

// v2 escape hatch (only if wanted): user-defined handlers for non-resumable
// (final) or tail-resumptive (linear) effects, Koka-style — additive.
with handler { ctl ask() resume(42) }
```

**Does algebraic effects pull its weight, given linear types + immutable defaults + Result?** Precise answer, in three parts. (1) As a *user-facing typing system* (Koka/Effekt-style rows or capabilities in every signature): NO for v1. The charter's sugar budget and explainability rule out row-polymorphism inference; linear types already deliver the safety effects would (resource safety via destructors, cancellation safety via nursery scopes); Result already delivers typed errors (section 7.8 shows the return-union and the effect row are the same information); and the strongest industrial attempt (OCaml 5) remains experimental after four years, while the languages that beat the color problem in practice (Go, Loom, Zig) did it with *no* effect system. (2) As an *internal execution model*: YES, quietly. Nursery/cancellation/strand-teardown are handlers; implementing them Koka-style (evidence vectors, tail-resumption, final ops) in the VM is cheap, keeps future options open, and — if Combo 2 or 6 is chosen — the capability parameter IS the effect row member, so a later full-effects language is an additive change, not a migration. (3) As a *future library feature* for user-defined control abstractions (iterators, parsers, UI, probabilistic programming): genuinely valuable, and the deferred option in Combo 6 preserves it; if the author later wants it, start from `final ctl` + `linear effect` (non-resumable + tail-resumptive only) — that subset avoids the multi-shot/linear-resource trap that both Koka's `finally` workarounds and Effekt's region restrictions exist to contain.

**Open questions handed to the design phase.** (1) Nursery shape: block form (`nursery { spawn { ... } }`) vs trailing-lambda builder vs `with`-statement binding (charter already has Koka-style `with` appreciation) — Koka's `with handler` and Kotlin's `coroutineScope {}` are the two poles. (2) Error-collection semantics at nursery exit: union all child errors (ExceptionGroup) vs first-error-cancels-rest (Koka `firstof`) vs both, selected per nursery. (3) Whether the `io` capability parameter is explicit (Zig) or implicit-and-inferred (a one-member effect row) — the inferred version is where Combo 6 silently becomes Combo 3. (4) Panic semantics under fibers: abort the fiber (Inko) vs abort the process (Rust default); the supervision story decides. (5) The linear-resumption typing rule proposed in section 8 (forbid linear capture in multi-shot continuations) needs a small formal treatment before any effects surface syntax is considered. (6) FFI pinning policy. Each of these has 2-3 concrete options drawn from the exemplars above and is ready to be lifted into the decision catalog (`docs/en/design/`).

Sources (primary): Koka https://github.com/koka-lang/koka (lib/std/async/, lib/std/core/hnd.kk, kklib/include/kklib.h, doc/spec/tour.kk.md, doc/spec/news.mdk) and https://koka-lang.github.io/koka/doc/book.html; Xie & Leijen ICFP'21 https://www.microsoft.com/en-us/research/publication/generalized-evidence-passing-for-effect-handlers-or-efficient-compilation-of-effect-handlers-to-c/; Leijen MSR-TR-2017-21 https://www.microsoft.com/en-us/research/wp-content/uploads/2017/05/asynceffects-msr-tr-2017-21.pdf; Effekt https://github.com/effekt-lang/effekt (libraries/llvm/rts.ll, libraries/common/result.effekt, examples/casestudies/scheduler.effekt.md, machine IR) and https://effekt-lang.org/publications; Unison https://github.com/unisonweb/unison (docs/ability-typechecking.markdown, parser-typechecker/src/Unison/Builtin.hs) and https://www.unison-lang.org/docs/; OCaml https://ocaml.org/manual/5.4/ and https://ocaml.org/changelog/2025-01-08-ocaml-530; Zig https://ziglang.org/download/0.15.1/release-notes.html and https://ziglang.org/devlog/2025/; Go preemption https://go.googlesource.com/proposal/+/master/design/24543-non-cooperative-preemption.md; Java https://openjdk.org/jeps/444, /505, /506, /491; Kotlin https://kotlinlang.org/docs/coroutine-context-and-dispatchers.html; Trio https://vorpus.org/blog/notes-on-structured-concurrency-or-go-statement-considered-harmful/; without.boats https://without.boats/blog/coroutines-and-effects/; Swift SE-0413/0414/0430/0461 https://github.com/swiftlang/swift-evolution/blob/main/proposals/; Pony https://www.ponylang.io/media/papers/codesigning.pdf; Verona https://github.com/microsoft/verona; Inko vendored docs (inko/docs/source/) and https://inko-lang.org; Mojo https://docs.modular.com/mojo/; error models https://typesanitizer.com/blog/errors.html; Roc https://www.roc-lang.org/faq; unions post https://www.arshad.fyi/writings/null-errors-and-enums-are-one-feature.
