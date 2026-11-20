# 03 — Effects, errors, and async: decision catalog

Status: written 2026-11-04, translating completed research into selectable options. Primary evidence: `docs/en/research/02-concurrency-effects-errors.md` (cited below as research/02 with its internal section numbers), `docs/en/research/04-syntax-ergonomics.md` section 11 (cited as research/04 §11), and `docs/en/research/03-runtime-ir-jit.md` executive summary items 5, 6, 8, 9 (cited as research/03 exec N).

Sister catalogs, referenced by path only — their contents are owned there and are being written concurrently: `docs/en/design/02-linearity-ownership.md` (linear/affine typing, ownership, FFI handles), `docs/en/design/04-syntax.md` (surface syntax), the types catalog `docs/en/design/01*` (set-theoretic unions are its open option), and the index `docs/en/design/00-decision-map.md`.

All code in this file uses a neutral TS-family illustrative syntax; the examples predate the docs/en/design/04 surface decisions and illustrate semantics only — surface syntax is decided in `docs/en/design/04-syntax.md`.

## 1. Decision-point index

| ID | Decision | Options | Recommended below |
|---|---|---|---|
| B5 | How signatures carry errors | B5a inferred error unions, B5b explicit Result, B5c effect-row annotations, B5d hybrid union-base + rows-later | B5d |
| E1 | Effect system | E1a full row-polymorphic resumable effects, E1b abilities-lite, E1c capability parameters, E1d deferred with handler runtime underneath | E1d (+ E1c idiom in stdlib) |
| E2 | Async model | E2a async-as-effect, E2b VM fibers + mandatory nursery, E2c unstructured spawn, E2d colored async/await | E2b |
| E3 | Parallelism | E3a type-safe shared memory, E3b fork-join data parallel, E3c actors/isolation, E3d single-thread + FFI | E3b on the E3a substrate |
| E4 | Error handling | E4a Result + trap split, E4b union errors with set-difference handlers, E4c exceptions-as-effects, E4d let-it-crash supervision | E4b with E4a's trap split |
| E5 | Cancellation and resources | E5a nursery-scoped cancellation, E5b linear resources, E5c defer/errdefer, E5d timeouts-as-handlers | E5a + E5b (E5c as sugar, E5d as library) |

## 2. Shared assumptions (charter + research, common to every option)

- Execution is a register bytecode VM with guaranteed TCO; dispatch is computed-goto or tail-call threading; value representation keeps unboxed typed registers with ADTs and Result as unboxed tag+payload pairs (research/03 exec 5, 6, 9).
- Memory is Perceus-style precise reference counting with reuse; linear values have statically known count 1, so their RC ops are deleted and their destructors are the finalizer hooks (research/03 exec 8; research/02 §8).
- Heap fibers are cheap in this VM: a fiber stack is interpreter data (Lua `CallInfo` arrays), switching is a pointer swap, O(1) (research/02 §8).
- Bindings are immutable by default; static linear/affine typing is enforced at compile time (charter hard requirements 2, 5).
- No C++-style specialization (charter); sugar ceiling is Kotlin's total surface (charter item 4; research/04 §2).
- Set-theoretic unions are an open option owned by the types catalog (`docs/en/design/01*`). Several options here (flat error unions, `or`, `.err` set-difference typing) presume it; each says so explicitly.
- The `?` / `or` / `is` / `.err` extraction tokens are researched in research/04 §11 but decided in `docs/en/design/04-syntax.md`; this file only relies on their researched semantics.

## 3. The author's framing question, answered per option

The question, verbatim: "can async/effects be abstracted so the benefits stay while learning cost and error rate drop?" The research decomposition: the benefits of async and effects are four — IO latency without thread explosion, composable cancellation, scheduling abstraction (same code blocking, evented, threaded), and a signature-level audit of world-touching code. None of the four strictly requires row-polymorphic effect syntax; every winning real-world design contains the color in exactly one place (a runtime fiber, a Job tree, or a capability/row) rather than dissolving it (research/02 §5.8, exec item 18, §d).

| Option | Abstraction mechanism | Benefits kept | Learning/error cost dropped | Residual cost | Verdict |
|---|---|---|---|---|---|
| E1a full rows | effect rows inferred in every signature | audit, resumable abstractions, async-as-effect | nothing — this is the maximal-cost spelling | row inference in every signature, row-error messages | keeps all benefits at the highest learning cost (research/02 §1, combo 3) |
| E1b abilities-lite | one built-in IO ability, ambient checking | coarse audit | row-polymorphism zoo | audit is coarse; no user resumable abstractions | cheapest typed audit, permanently coarse (research/02 §4) |
| E1c capabilities | explicit `io: Io` parameter | scheduling abstraction, audit-by-parameter, zero type machinery | all type-system cost | call-site noise of threading `io` | abstracts async by parameterization, not typing (research/02 §5.1) |
| E1d deferred | handler runtime internal, no surface syntax | all four, via structure + capability convention | all surface/inference cost, indefinitely | user-defined resumable abstractions delayed | the research verdict: benefits now, costs deferred (research/02 §d, combo 6) |
| E2a async-as-effect | async = inferred row, no await | composable cancellation, no await keyword, sync/async intermix | keyword virality | rows in every combinator signature | most elegant, heaviest compiler (research/02 §1) |
| E2b fibers + nursery | color moved into the runtime fiber; structure via nursery block | all four (cancellation via scope, scheduling via pool/event loop, audit via E1c convention) | colors, await, lifecycle state machines | bare detach not expressible | highest benefit/learning-cost ratio found (research/02 combo 1, Table 1) |
| E2c unstructured spawn | color erased by runtime, no structure | colorless blocking style | nothing structural — leaks remain | goroutine leaks, bolted-on cancellation, lost errors | colorless without structure keeps the high error rate (research/02 §5.2) |
| E2d colored async/await | two colors, `await` viral | familiar to the author | nothing — this is the cost-center spelling | two-color stdlib, API duplication, drop-cancellation footguns | familiarity is its only advantage not shared by E2b (research/02 §5, §5.6, §5.8) |

For the remaining four points the per-option answer is folded into each recommendation: B5d keeps the signature audit while errors stay ordinary data; E3b-on-E3a keeps race-freedom as a static theorem and buys only a scheduler; E4b keeps typed handling at three tokens of surface; E5a+E5b keep cancellation scoping and close guarantees without runtime-interruption machinery. Net research answer: yes for async and for the restricted (non-resumable) part of effects — abstraction is achieved by structure (nursery), parameterization (capability), and data (unions), with the evidence-passing handler runtime underneath so the full-effects escape hatch stays additive (research/02 §d three-part answer).

## 4. B5 — How signatures carry errors

The question is where "what can go wrong" lives in a signature: as union members of the return type (data), inside a wrapper value, or in an effect row (control flow). This is a representation choice, not a semantics choice: `read : Path -> Data | NotFound | IoError` and `read : Path -> <exn{NotFound, IoError}> Data` carry identical information, and Koka's denotational table makes the identity literal — `int -> exn int` is `Z -> (Z + 1)` (research/02 §7.8, §1). Effekt's `Exception[E]` is the indexed middle form (research/02 §2).

### B5a — Inferred error unions in the return type (Roc model)

```ts
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
function readConfig(path: Path): Config | IoError | ParseError   // inferable: annotation optional
function load(): Config | IoError | ParseError {
  const text = readFile(path)?     // callee's IoError widens this function's union automatically
  return parseConfig(text)?        // ParseError joins the same way — composition is union arithmetic
}
```

Prior art: Roc has no Option type, only `Try` with inferred tag-union errors — "when one function calls another, the possible errors combine automatically into a tag union of all possible errors at that call site" (research/02 §7.1; https://www.roc-lang.org/faq). This is also the old-DreamScript "errors collected into the return-type union" idea, assessed sound in research/02 §7.8 on all five axes.

Interaction: with set-theoretic unions (open option, `docs/en/design/01*`) the error set is literally a set and propagation is a subset check; if unions are nominal-only, this option degrades to enum plumbing.

| Axis | Assessment |
|---|---|
| Learning | Lowest: "functions return sets" — the charter's set-theoretic philosophy applied to failure; zero wrapper vocabulary. |
| Perf | One tag branch per fallible call; union returns are unboxed tag+payload pairs across registers in the planned VM (research/03 exec 6; research/02 §7.7, §8). |
| Error rate | Good audit for free; but nothing in the signature marks which members are "errors" vs ordinary alternatives — handling discipline comes from the research/04 §11 tokens. |
| Compiler | Union inference and widening across generic code, unified with linear/affine checking; moderate, and mostly work the types catalog already contemplates. |

Forecloses: locks the error channel to data (good for nursery error collection and supervision, research/02 §7.8); adding control-flow error semantics later must go through B5d's bridge rather than being native.

### B5b — Explicit Result wrapper values (Rust/Gleam model)

```ts
function readConfig(path: Path): Result<Config, IoError | ParseError>
readConfig(p).map(cfg => cfg.port)     // wrapper is user-visible in every fallible signature
```

Prior art: Rust `io::Result<Data>` with payload-carrying error enums; Gleam is Result-only — no exceptions in user code, `panic` reserved for bugs (research/02 §7.1, §7.4, §6 BEAM/Gleam).

| Axis | Assessment |
|---|---|
| Learning | Low-medium: one wrapper idiom, but Ok/Err vocabulary and monadic plumbing (`map`/`and_then`) appear in every signature, which research/04 §11 and research/02 §7.6 count against the sugar budget. |
| Perf | Same one-branch cost as B5a when the wrapper is an unboxed tag+payload pair (research/03 exec 6); degrades to heap boxing if wrappers are nominal classes. |
| Error rate | Highest explicitness — the wrapper name itself documents failure; nesting (`Result<Option<T>>`) re-enters the language (research/02 §7.6). |
| Compiler | Simplest: no error-set inference at all; the wrapper is an ordinary ADT. |

Forecloses: does not by itself remove the need for `?`-style propagation sugar (Rust added it), so it is B5a's mechanics plus extra vocabulary; flat-union composition (`T | none | E`, research/02 §7.6) is foreclosed as the default.

### B5c — Effect-row error annotations (Koka `exn` / Effekt `Exception[E]`)

```ts
function readConfig(path: Path): throws(IoError | ParseError) Config   // row carries the error set
// propagation is control flow: no return tag, works identically inside callbacks/generators
```

Prior art: Koka's `exn` is a builtin non-resumable effect — `final ctl throw-exn` — with inferred, row-composable signatures; a plain `result<a,e>` type also exists alongside (research/02 §1, §7.3). Effekt's `Exception[E]` is an effect indexed by the error type, and its stdlib defines `Result[A,E]` as the literal "reification" of that effect, with `value` reflecting back — the two are interdefinable (research/02 §2). Swift's typed throws (SE-0413, shipped 6.0) shows the industry converging on "error type in the signature" regardless of mechanism (research/02 §7.2).

| Axis | Assessment |
|---|---|
| Learning | Medium-high: dynamic handler scoping plus row vocabulary; Java's checked exceptions are the cautionary datum for annotation-burden variants (research/02 §7.2) — inference is mandatory, not optional. |
| Perf | Propagation through control flow: no return-path rewriting, natural across higher-order calls and generators (research/02 §7.8); a throw is a frame-walk in the interpreter loop already present (research/02 §7.7). |
| Error rate | Automatic signature audit; but errors stop being storable values — they cannot be cached, sent across processes, or collected by a nursery without a reification step (research/02 §7.8). |
| Compiler | Full row-inference machinery (the entire E1a burden) is required even if effects are used only for errors (research/02 combo 3). |

Forecloses: couples B5 to E1a/E1c from day one; if chosen first, demotion to unions (B5a/B5d) is a breaking redesign, while the reverse direction is additive.

### B5d — Hybrid: union-carried default now, opt-in effect-row errors later

```ts
// v1: errors are union members of the return type (B5a representation, research/04 §11 surface)
function readConfig(path: Path): Config | IoError | ParseError
// v2, additive sugar carrying the same information — "this row is exactly your error union":
function readConfig(path: Path): throws(IoError | ParseError) Config    // desugars to the v1 signature
```

Prior art: research/02 §7.8 concludes verbatim that "an error-union-in-return-type is the restricted effect row we already have" and recommends adopting the union/Result form as the base with `throws` sugar if/when full effects arrive; Effekt's `result`/`value` pair proves the translation is interdefinable in both directions (research/02 §2); research/04 §11 shows the recommended `?`/`or`/`.err` surface is already effect-ready for the same reason. The key property: union-carried errors ARE a restricted effect row, so this is a compatible evolution path, not a redesign.

| Axis | Assessment |
|---|---|
| Learning | B5a's cost now (lowest); the v2 sugar costs nothing until used. |
| Perf | B5a's cost (one predicted branch); a later row spelling reuses the same evidence-dispatch path the E1d runtime already has (research/02 §8). |
| Error rate | B5a's audit plus storable errors for nurseries/supervision (research/02 §7.8). |
| Compiler | One seam to maintain: unions lower to return tags, rows to evidence dispatch; the two must share a semantics, which §7.8 establishes they do. |

Forecloses: requires v1 propagation to be union-flavored (`?` checks set containment with the Zig rule — no implicit conversions, research/04 §11) so the later `throws` sugar desugars cleanly; nothing else — this option exists to foreclose nothing.

Recommendation: **B5d** — union-carried errors on the research/04 §11 surface as the base representation, with effect-row errors as additive later sugar. Rationale: identical information to rows (research/02 §7.8), errors stay storable data for nursery collection and supervision, propagation is one perfectly-predicted branch in a register VM (research/02 §7.7; research/03 exec 6), and the path to full effects remains open without migration.

## 5. E1 — Effect system

Charter item 7 makes effects optional: "evaluate them seriously; adopt only if they earn their complexity." Mechanism background shared by E1a and E1d — evidence passing: Koka compiles handlers by generalized evidence passing (Xie & Leijen, ICFP'21): a handler is an existential `(marker, op-table)` pair; the thread context carries an evidence vector plus an inline yield record `kk_yield_t` with 8 continuation slots; performing sets a yielding marker and walks the vector; capturing composes the frame chain between perform and handler into heap closures — no stack switching, identical on C and JS backends (research/02 §1; https://www.microsoft.com/en-us/research/publication/generalized-evidence-passing-for-effect-handlers-or-efficient-compilation-of-effect-handlers-to-c/). In a bytecode VM: each frame already holds a resume IP and registers, so a continuation is one closure per frame between perform and handler; tail-resumptive operations skip capture entirely (the handler clause runs with the caller's continuation); steady-state cost is roughly one indirect call per effectful operation, indistinguishable from a virtual call (research/02 §8).

### E1a — Full row-polymorphic resumable effects (Koka model)

```ts
effect Fail { fail(msg: String): never }                       // resumable operation
function divide(x: Int, y: Int): Fail<Int> { return y == 0 ? fail("div0") : x / y }
const ok = handle { divide(1, 0) } with { fail(msg) => resume(0) }   // resumption continues at the call site
```

Prior art: Koka — inferred rows over a small set of builtin effects (`exn`, `div`, `ndet`, `io`) plus capability-flavored atomic effects (`net`, `fsys`, `ui`), multi-shot resumption, `final ctl` for non-resumable operations, named effects with scope parameters, and `pub linear effect async-cancel` (research/02 §1). Effekt is the compilation-efficiency counterpart via lexical capabilities (research/02 §2). OCaml 5 is the industrial-adoption datum: handlers still marked experimental after four years — the typing/API story is what stalls, not the runtime (research/02 §3, exec item 7).

The sharp edge (required): multi-shot resumption vs linear values. A resumption invoked twice must not carry a linear resource — double-close by construction. Koka's answers are operational (`finally`/`initially` combinators, with the docs admitting "active research into using the type system to statically prevent this") and typed (`linear effect`: operations are tail-resumptive only, which removes capture — research/02 §1). DreamScript can do strictly better: forbid capturing linear values inside multi-shot continuations at typecheck, or type resumptions affine unless marked — a small typing rule with a large safety payoff, and the one place linear types make effects safer than any existing effect language (research/02 §8).

Linear/TCO interaction: tail-resumptions preserve guaranteed TCO — Koka lowers tail calls to a label loop and Lua reuses the call frame (`luaD_pretailcall`), so the interpreter inherits both (research/03 exec 9); non-tail captures allocate one closure per frame and interact with Perceus RC bookkeeping (research/03 exec 8). Sendability is unaffected (effects are not values), but a captured continuation is itself a value that must obey linearity.

| Axis | Assessment |
|---|---|
| Learning | Highest: every combinator signature carries a row variable; error messages mention rows; Koka's own docs lean on hover-driven UIs (research/02 §1). |
| Perf | Good: evidence passing has no stack switching; tail-resumption is capture-free; multi-shot capture is one closure per frame (research/02 §1, §8). |
| Error rate | Low at runtime — scoped, typed, no hidden control flow; but inference-error UX is a real developer-error surface (research/02 combo 3). |
| Compiler | Highest of all options: row inference must unify with linear/affine unification, an area with thin prior art (research/02 combo 3). |

Forecloses: commits the whole type system to row inference early and is hard to demote later; spends the charter's explainability budget (charter item: "fully explainable in a short tutorial"); pushes B5 toward B5c.

### E1b — Abilities-lite (Unison model)

```ts
function main(): {IO, Exception}<void> { io.printLine("hi") }   // one built-in IO ability + Exception
```

Prior art: Unison 1.0 keeps exactly one built-in I/O ability plus `Exception`; the checking rule is deliberately simple — ambient abilities at a subterm equal the required abilities of the nearest enclosing lambda minus handled abilities; Unison explicitly rejected Frank-style generalization as unsound in their setting (research/02 §4, exec item 6; unisonweb docs/ability-typechecking.markdown).

| Axis | Assessment |
|---|---|
| Learning | Cheap: one ambient-set rule, no effect-polymorphism zoo; but `{IO}` still appears in every world-touching signature — the color becomes a set literal. |
| Perf | Ability operations dispatch through the ambient handler — an ordinary call; interpreter-friendly (research/02 §4). |
| Error rate | Coarse audit only: cannot distinguish file from network access in types (Koka's `net`/`fsys` show exactly what is lost, research/02 §1, §4); concurrency stays runtime-level and untyped (`IO.forkComp`). |
| Compiler | Much simpler than full rows — existential row variable refined by checks, no generalization after typechecking; still a whole-signature annotation system. |

Forecloses: user-defined resumable control abstractions are effectively out; async-as-effect at Koka fidelity is out (Unison's concurrency is builtin-level); audit granularity is permanently coarse unless the design graduates to E1a anyway.

### E1c — Capability parameters, no effect syntax (Zig 0.16 / Effekt-minus-rows)

```ts
function connect(host: Host, io: Io, port: Port): Conn | NetErr   // io is an ordinary interface value
val conn = connect(h, Io.eventLoop(), 443)   // identical code over Io.blocking() or Io.iouring()
```

Prior art: Zig 0.15/0.16 rebuilt async around an explicit `Io` capability parameter — `fn connect(host, io: Io, port)`; `io.async(...)`, `Io.Queue`, `Io.Group`, `error.Canceled` compose with `defer`; the claimed property is that the same code runs optimally over blocking threads, thread pools, or io_uring, with "no heap allocation" and ordinary control flow (research/02 §5.1, exec item 8). This is Effekt's capability model (OOPSLA 2020, "Effects as capabilities") with the row polymorphism deleted — effects minus row types (research/02 §2).

Linear/TCO interaction: the capability is an ordinary value; a linear capability makes authority transferable by move and uncopyable, which is a feature (permission transfer is a move); capture in callbacks is unproblematic because it is data, not control; no TCO interaction.

| Axis | Assessment |
|---|---|
| Learning | Low-medium: one idiom ("pass the io"), zero type-system features; call-site parameter threading is the noise cost (Zig mitigates with idioms, research/02 §5.1). |
| Perf | One vtable indirection per world-touching op — invisible next to interpreter dispatch (research/03 exec 5). |
| Error rate | Medium: forgetting to pass a capability is a compile error, but scoping discipline for spawned work is manual unless nursery combinators fence it (research/02 combo 2). |
| Compiler | Nothing new: ordinary parameters and generics; no inference subsystem at all. |

Forecloses: no user-defined resumable effects surface; the audit lives in parameter conventions rather than types; but graduating later to rows is syntax-compatible — the capability IS the row member without row types (research/02 combo 2, combo 6).

### E1d — Deferred: Result + panic now, evidence-passing handler runtime underneath

```ts
// v1 surface: unions + trap + nursery. No effect syntax, no user handlers.
function main(): void | NetErr { nursery { spawn { fetchAll(URLS) } } }
// v2 escape hatch, only if ever wanted — additive, no migration:
with handler { op ask() => resume(42) }
```

Prior art: this is the research verdict — full row-polymorphic resumable effects do NOT pull their weight for v1 given linear types + immutable defaults + Result + nurseries; a capability-passing core DOES; and the nursery/cancellation/strand-teardown semantics should be implemented with the evidence-passing ABI from day one so the runtime is handler-shaped and full effects remain additive (research/02 exec item 18, §d three-part answer, combo 6).

Cost in a bytecode VM (required detail): the internal ABI costs what E1a costs (mechanism above) but is paid only at nursery boundaries and strand teardown for the two internal effects (`cancel`, `discontinue`), not per user operation; user-facing code paths are unchanged, and tail-resumptive internal ops (cancellation checks) allocate nothing (research/02 §8).

| Axis | Assessment |
|---|---|
| Learning | Lowest: nothing to learn in v1; the v2 hatch is opt-in Koka-style `with handler`. |
| Perf | User code unaffected; internal effects cost two indirect dispatches per nursery boundary. |
| Error rate | Structure carries the safety (mandatory nursery); no row-error messages ever. |
| Compiler | Must preserve the lowering seam — errors to return tags, cancel/discontinue to evidence dispatch — and avoid baking non-handler semantics into surface features. |

Forecloses: nothing structural — its purpose is to foreclose nothing; the accepted cost is that user-defined resumable effects may never ship (research/02 judges that an acceptable risk, §d part 3).

Recommendation: **E1d**, adopting E1c's capability idiom as the stdlib convention for world-touching APIs and the linear-resumption restriction (no linear capture in multi-shot continuations) as a standing typing rule for anything captured. Rationale: the research verdict is explicit (research/02 §d), the runtime is handler-shaped so E1a remains additive, and both the sugar budget and the explainability budget survive intact.

## 6. E2 — Async model

Background: heap fibers are cheap and proven in exactly this VM shape — a fiber stack is a growable frame array on the interpreter heap, spawn is one allocation, switch is a pointer swap (Lua/LuaJIT precedent), and Inko runs this design at scale with a fixed thread pool, work stealing, and compiler-injected preemption points (research/02 §8). Preemption in an interpreter is one flag check in the dispatch loop every N instructions or on backward jumps — strictly simpler than Go's signal-based scheme (research/02 §8; dispatch-loop shape per research/03 exec 5).

### E2a — Async-as-effect, no await keyword (Koka std/async model)

```ts
function wait(d: Duration): Async<void>    // async is an inferred row member — no await anywhere
function timeout(secs: Duration, act: () -> Async<A>): Async<A | none>
  = firstof(fn { wait(secs); none }, fn { some(act()) })   // a library function, not a language feature
```

Prior art: Koka 3.2.9 `std/async` defines `pub alias async = <async-await, async-ioc, async-cancel, discontinue>` — four primitive effects — with cancellation scopes, `interleave`, `firstof`, and `timeout` all built as user-level handlers, and the event loop is simply the default handler installed around `main` (research/02 §1, exec item 1; Leijen MSR-TR-2017-21). The honest claim: `wait : duration -> async ()` visibly carries the row — what Koka removes is everything that makes coloring hurt (no await keyword, effect-polymorphic combinators written once, sync/async intermix), not the color itself (research/02 §1).

Linear/TCO interaction: `async-cancel` is a `linear effect` — tail-resumptive only, so the cancellation-scope chain never captures continuations, and capture-free cancellation is what keeps TCO intact under `timeout`/`firstof` (research/02 §1). Await resumptions deliver results across strand boundaries, so resumed values must be sendable — which linear/immutable data gives by construction (research/02 §7.9(2)).

| Axis | Assessment |
|---|---|
| Learning | Medium-high: rows appear in every combinator signature users write; the color vocabulary moves into inference rather than disappearing (research/02 §1). |
| Perf | Good: evidence passing, no stack switching; tail-resumptive async ops are capture-free (research/02 §1, §8). |
| Error rate | Low: handler-scoped cancellation, typed nondeterminism (`ndet`), no await-virality (research/02 §1). |
| Compiler | Full E1a burden plus async's four effects as the first real row clients (research/02 combo 3). |

Forecloses: requires E1a-level row inference; ties B5 toward B5c; hardest combination to retrofit onto a union-based B5.

### E2b — VM fibers + mandatory structured concurrency (nursery/Job tree)

```ts
nursery {                                    // the only spawn site; lexical scope outlives children
  for url in urls { spawn { pages.push(fetchBody(url)) } }   // children are fibers on the VM pool
}                                            // exit: joined; first failure cancels the rest; errors union out
```

Prior art: Trio's nursery semantics — task lifetime is lexically scoped, cancellation is cooperative and scoped, and the nursery does not return until children finish (research/02 §5.5, exec item 12; https://vorpus.org/blog/notes-on-structured-concurrency-or-go-statement-considered-harmful/); Kotlin's Job tree — children cancel with the parent, parents wait for children, failures propagate up — the structure other languages keep copying, proven to fix the error-rate problem even with colors present (research/02 §5.4); "Go minus its one fatal flaw" is exactly research/02 combo 1.

Linear/TCO interaction: children receive copies or linear/unique values, so there is no shared mutable state to race (research/02 §5.5, §7.9). Cancellation racing a linear resource: cancellation is observed only at await/preemption points, never mid-consume, and fiber teardown walks the frames running linear finalizers — so consume-to-close cannot be skipped by a cancel (research/02 §7.9(4), §8). Guaranteed TCO is per-fiber and unaffected (research/03 exec 9).

| Axis | Assessment |
|---|---|
| Learning | Lowest: blocking style everywhere, no keywords, one new block form (research/02 combo 1, Table 1). |
| Perf | Excellent for an interpreter: no state-machine boxing, no Pin-analog, O(1) switch, heap fiber chunks like Loom's stack chunks (research/02 §8, §5.3). |
| Error rate | Low if the nursery is mandatory — Go's deployment record with its one documented fatal flaw removed; leaks and lost errors are structurally impossible (research/02 §5.2 vs §5.5). |
| Compiler | Fiber support + spawn/nursery opcodes + preemption flag: modest, precedented (research/02 §8). |

Forecloses: bare detach (`go f()`) is deliberately not expressible — a detached task needs an explicit root nursery/supervisor; user-defined schedulable abstractions need the E1d seam; nothing else.

### E2c — Unstructured stackful spawn (Go model)

```ts
go fetch(url)                          // fire and forget: no scope, no join, no cancellation
ctx := context.WithTimeout(parent, 2*s)   // cancellation becomes a threaded parameter convention
```

Prior art: Go — stackful goroutines, integrated netpoller, preemptive scheduling make it colorless by construction; the documented costs are orphaned tasks, `context.Context` bolted on as a convention threading through every cancellable API, errors from vanished goroutines lost, manual leak detection, ~2KB stack floors and per-call growth checks (research/02 §5.2; https://fasterthanli.me/articles/lies-we-tell-ourselves-to-keep-using-golang cited there).

| Axis | Assessment |
|---|---|
| Learning | Lowest upfront — and that is the trap: the model is simple, the failure modes are not. |
| Perf | Good; 2KB floors and growth checks are real per-call costs in a register VM that would not otherwise need them (research/02 §5.2). |
| Error rate | High: goroutine leaks, no cancellation story, lost errors are the documented production failure modes (research/02 §5.2, Table 1 "high without nursery"). |
| Compiler | Needs goroutine-grade runtime work (growth, preemption) while buying the least structure per unit of work. |

Forecloses: directly conflicts with E5a; a nursery cannot be retrofitted without breaking every API that assumed detach; child error unions cannot be collected because children are unjoinable (research/02 §7.8 values argument).

### E2d — Colored async/await (TS-flavor model)

```ts
async function fetch(url: Url): Promise<Page> { ... }   // the second color
const page = await fetch(u)                             // viral await; sync/async API duplication
```

Prior art: "What Color is Your Function?" defined the cost model — async as a second color that cannot call sync without blocking or vice versa without spawning, viral `await`, duplicated APIs (research/02 §5 intro; https://journal.stuffwithstuff.com/2015/02/01/what-color-is-your-function/). Rust is the colored + ownership cautionary tale: Pin, unnameable Send bounds on async-fn-in-trait futures, runtime fragmentation, cancellation-by-drop footguns — coloring plus ownership inference together exceed a tolerable learning budget (research/02 §5.6). Kotlin kept colors and won on structure, with the color split remaining the top complaint (research/02 §5.4).

Linear/TCO interaction: values live across suspend points determine the future's trait bounds in Rust, producing opaque errors invisible at call sites (research/02 §5.6) — with linear values the analogous question ("what is captured across await, and is it sendable?") is checkable but adds a new error class; stackless state machines also interact awkwardly with guaranteed TCO in generic combinators (research/03 exec 9 context).

| Axis | Assessment |
|---|---|
| Learning | Superficially low for this author (TS background — a real and honest advantage), but the actual curve is state-machine lifecycle and cancellation-safety footguns (research/02 §5.6). |
| Perf | Best on paper for stackless (no stack swap) — irrelevant here where the swap is a pointer swap (research/02 §8). |
| Error rate | High: drop-based cancellation, two-world API drift, lifecycle bugs (research/02 §5.6, Table 1). |
| Compiler | State-machine lowering: moderate and well-known, but every generic combinator lowers twice (sync/async) unless effects unify them. |

Forecloses: a two-color stdlib permanently; sync/async API duplication; hardest to reconcile with the decoupling-first charter philosophy (function-coloring entangles every function with its scheduling — charter, design philosophy).

Recommendation: **E2b** — mandatory-nursery VM fibers, implemented on the E1d handler runtime (a nursery IS a handler for the internal `cancel`/`discontinue` effects), with `firstof`/`timeout`/`interleave` as library combinators per the Koka catalog. Rationale: research/02 Table 1 and combo 1 — lowest learning cost, interpreter-native performance, low error rate with mandatory structure, and every E2a benefit remains reachable additively.

Sub-answer (required): does E2b with linear sendability subsume E2a's benefits, and where do handlers still win?

- Mostly yes, point by point. Composable cancellation: a nursery cancel scope and a Koka cancel-scope handler have the same semantics in block form instead of row form (research/02 §5.5 vs §1). Scheduling abstraction: delivered by the capability or pluggable event loop (E1c/E1d), not by rows (research/02 combo 2, combo 6). No-await: fibers are blocking-style — even less surface than Koka. Sync/async intermix: total, since there are no colors at all. Audit: the capability parameter carries it (research/02 §d).
- Where handlers still genuinely win: (1) user-defined resumable control abstractions — parsers, iterators, UI sessions, probabilistic programming; (2) typed nondeterminism — Koka's `ndet` member is tracked in rows, nothing in the fiber model tracks it (research/02 §1); (3) first-class resumptions escaping lexical scope (`raw ctl`) for schedulers beyond the built-in (research/02 §1); (4) zero-convention polymorphism — Koka's `map` works under any handler with no threading, whereas the fiber model must thread `io` by convention (research/02 §1, §d).
- Net: E2b plus linear sendability subsumes the benefits that motivated async-as-effect for application code; the handler advantage is real but library-shaped, and E1d keeps it reachable without pre-payment (research/02 §d three-part answer).

## 7. E3 — Parallelism

Background: parallelism (multiple cores) is orthogonal to E2 (IO latency) but shares the runtime pool. The core type system already covers, for free: no data races (no aliased mutability), sendability (linear = movable, the `sending`/`uni`/`iso` role), and deterministic parallelism (pure functions over immutable inputs run on any number of threads) — what remains unsolved is only the scheduler, cancellation scoping, supervision, and deadlock freedom (research/02 §7.9, Table 4).

### E3a — Shared memory made safe by the type system

```ts
spawn { worker(move buffer) }      // linear Buffer moves in; caller's binding is dead — structural sendability
spawn { analyze(sharedGrid) }      // immutable data is shareable by construction
```

Prior art: Swift SE-0414 region-based isolation + SE-0430 `sending` parameters retrofit, by inference, exactly the transfer reasoning linear types give syntactically — the research's explicit framing is that "a linear value is by construction `sending`, and an immutable value is by construction Sendable" (research/02 §5.7, exec item 14; https://github.com/swiftlang/swift-evolution/blob/main/proposals/0414-region-based-isolation.md, /0430-transferring-parameters-and-results.md). The explicit analogs: Inko `uni T` (unique, sendable; mutable borrows never) and Pony `iso` (research/02 §6).

What linear/immutable already buys free: the entire Send/Sync apparatus collapses to two checkable cases — linear values may move, immutable values may share (research/02 §7.9(1)-(2)). What remains: the scheduler decision (which thread runs what), locality/false-sharing tuning, and any concurrently-mutable shared structure — which is either unrepresentable in v1 or needs an explicit capability.

| Axis | Assessment |
|---|---|
| Learning | Low: two rules, versus Pony's six-capability lattice which exists precisely because Pony lacks linear types (research/02 §6). |
| Perf | Best for fine-grained shared-memory parallel algorithms; no locks, no copies for moved data. |
| Error rate | Low: race-freedom is a static consequence, not a discipline. |
| Compiler | Zero beyond the linear checking `docs/en/design/02-linearity-ownership.md` already requires — sendability is a consequence, not a subsystem. |

Forecloses: nothing; it is the substrate the charter already mandates. It makes any later actor or supervisor layer safe to add (E3c, E4d).

### E3b — Fork-join data parallel only (Rayon/Roc model)

```ts
const rendered = pages.par_map(fn(p) { p.render() })          // deterministic, order-preserving
const total    = rendered.par_reduce(0, fn(a, b) { a + b })   // associativity is the only requirement
```

Prior art: research/02 combo 5 and §7.9(3) — pure functions over immutable or linear-partitioned inputs parallelize with no locks and stay deterministic; Koka's `list/interleaved` determinism certification (`collapse-ndet`) and Mojo's `parallelize` are the cited exemplars (research/02 §1, §6 Mojo).

What linear buys free: race-freedom is a theorem, so the combinators need no runtime checks and no locking; what remains: work splitting and load balancing in the scheduler — and IO-latency concurrency is explicitly not addressed here (that is E2's job; the two compose on one pool).

| Axis | Assessment |
|---|---|
| Learning | Lowest of all E3 options: two combinators, deterministic, replayable. |
| Perf | Excellent for compute; no synchronization at all; Perceus + linear static counts make fork-side snapshots cheap (research/03 exec 8; research/02 §8 RC+linearity synergy). |
| Error rate | Lowest: deterministic output, races unrepresentable. |
| Compiler | Scheduler + combinator lowering only; no new typing rules. |

Forecloses: shared-mutable parallel idioms are out by design (a feature, per charter); composes cleanly with E2b — fibers for IO, par for compute, one pool (research/02 Table 4 row 3).

### E3c — Isolation/actors with message passing (BEAM/Pony/Inko)

```ts
process Counter { mut count: Int; fn async bump(n: Int) { count += n } }
val c = spawn Counter(0)
c.bump(3)      // message send: n is moved in if linear, copied if shared-immutable
```

Prior art: BEAM — per-process heaps, copied message sends, preemptive reduction-based scheduling, let-it-crash supervision (research/02 §6); Inko — `uni T` move-based messages, fixed thread pool with work stealing, compiler-injected preemption points, vendored and verified in the research (research/02 §6, §8); Pony — `iso`/`val` capabilities plus causal messaging and ORCA GC (research/02 §6).

What linear buys free: linear messages MOVE instead of copying — BEAM's main cost (copying sends) is eliminated exactly where it hurts, large messages (research/02 §6: "copying is unnecessary when messages are linear — you can move instead of copy"). What remains: process/message-protocol design, copying for non-linear shared data, and the worst case for fine-grained parallel algorithms, which must decompose into messages (research/02 combo 4).

| Axis | Assessment |
|---|---|
| Learning | Low-medium: the process model is intuitive; "what can I send" rules need teaching — Inko's `recover`/`uni` pedagogy documents the friction points (research/02 §6). |
| Perf | Good: no locks ever, move-based sends; weakest for fine-grained shared-memory parallelism (research/02 combo 4). |
| Error rate | Lowest known for systems that stay inside the model — isolation makes whole failure classes unrepresentable (research/02 §6 BEAM). |
| Compiler/Runtime | The biggest runtime addition: mailboxes, process scheduling, possibly per-process heaps; nothing new in the type system given linearity. |

Forecloses: fine-grained data parallelism becomes awkward; supervision (E4d) becomes natural here; composes with E2b if "process" is defined as a fiber with isolation-typed state.

### E3d — Single-threaded VM + parallelism via native FFI (Python model)

```ts
// in-language: nothing concurrent; all parallelism is foreign
val out = ffi.numpyTransform(bigArray)     // happens outside the language's rules entirely
```

Prior art: CPython's GIL model — the language claims no parallelism and native extensions own it; research/02 Table 1 last row prices it: lowest learning, one core for in-language tasks.

What linear buys free: in-language safety still holds and the FFI boundary can require linear ownership transfer of buffers; what remains: all parallel performance is foreign, untyped, and outside the guarantee system — the charter's performance goal (hard requirement 3) is unreachable for parallel in-language workloads.

| Axis | Assessment |
|---|---|
| Learning | Lowest. |
| Perf | Capped at one core for in-language compute; the interpreter's 3-10x-of-C target (research/03 exec 11) cannot be met on parallel workloads. |
| Error rate | Low in-language, unmeasurable across FFI. |
| Compiler | Smallest possible. |

Forecloses: conflicts with charter hard requirement 3 for any compute-shaped workload; the coloring problem is evicted to the FFI boundary, not solved (research/02 combo 5 analysis).

Recommendation: **E3b on the E3a substrate** — deterministic fork-join combinators as the only parallelism primitive, with the linear/immutable sendability rules (which the type system enforces anyway per the charter) documented as the general safety net so an actor/supervision layer (E3c shape, E4d) can be added later as a library. Rationale: research/02 §7.9 and Table 4 — races and sendability are already covered by the core; the missing piece is a scheduler plus combinators, and fork-join is the cheapest correct one.

## 8. E4 — Error handling

Background: two failure kinds must be separated — expected errors (typed, handled) and bugs (trap). The motivating datum: "almost all catastrophic failures (92%) are the result of incorrect handling of non-fatal errors explicitly signaled in software" (Yuan et al., via research/02 §7). VM costs: a Result/union return is a tag in the return slot plus a branch at the consumer, fusable with the consumer opcode; a trap is a fiber abort with a frame walk (research/02 §7.7, §8; research/03 exec 6).

### E4a — Result-as-data + unhandleable trap split

```ts
function read(path: Path): Result<Data, IoError>   // expected failure: data
panic("invariant violated: negative size")          // bug: aborts the fiber with a trace, unhandleable
```

Prior art: Rust (`Result` for expected, `panic!` for bugs, `catch_unwind` as the narrow boundary), Zig (error unions + `panic`), Swift (`throws` + `fatalError`/preconditions), Inko (unhandleable process-terminating panic) — the near-universal convergence of new languages (research/02 §7.1, §7.4).

Trap mechanics in this VM: trap aborts the fiber, walks its frames running linear finalizers, and surfaces at the nursery/supervisor boundary — near-free to implement (research/02 §7.4, §8).

| Axis | Assessment |
|---|---|
| Learning | Low: two kinds of failure, one rule each. |
| Perf | One predicted branch per fallible call; tag+payload unboxed in registers (research/03 exec 6); traps are rare-path only. |
| Error rate | Good: typed handling attacks exactly the 92% class; bugs trap loudly per the charter's compile-time-over-runtime preference. |
| Compiler | Simplest of all E4 options. |

Forecloses: no hidden control flow — every propagation is visible (sugared per research/04 §11); nursery error collection is natural because errors are data.

### E4b — Union-carried errors with set-difference handling (`?` + `or` + `.err`)

The repaired old-DreamScript idea. Worked example (research/04 §11, end-to-end):

```ts
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
function loadConfig(path: String): Config | IoError | ParseError {
  const text = readFile(path)?          // `?`: members outside T return early; return union contains IoError
  const cfg  = parseConfig(text)?       // ParseError propagates the same way — containment is checked
  return cfg
}

const cfg  = loadConfig("app.cfg") or defaultConfig()   // `or`: fallback consumes the error side
const cfg2 = loadConfig("app.cfg").err((e) => {         // handler covers IoError | ParseError;
  log("config failed: ${e}")                            // type is (Config | IoError | ParseError) - (IoError | ParseError)
  return defaultConfig()                                // == Config — no remainder left to handle
})
const port = cfg2.port or 8080                           // Option is T | none: the same three tokens
```

What was repaired versus the historical idea (research/04 §11): the old `{1/0}` block-typing gave one expression different types at different use sites — non-compositional, breaking local reasoning and inference (flaw 1); the repair types the expression once and does the work in the handlers via set difference. The old `.err` returning `double | null` just renamed the monad (flaw 2); here `.err` is difference-typed so the remainder is visible. Braces doing triple duty as block, thunk, and error boundary (flaw 3) are dropped entirely.

Prior art: research/04 §11 surveys the surfaces — Rust `?` (minus the `From` conversion rule, adopted Zig's cheaper containment rule), Zig `try`/`catch`/`orelse`, Roc's postfix `?` (verified verbatim), Phi's `or`, Swift's rejected three spellings; the union-everything framing is research/02 §7.6 (Ceylon, TypeScript narrowing, Zig error sets, Kotlin elvis credited there).

| Axis | Assessment |
|---|---|
| Learning | Three tokens with one semantics stated over unions; net sugar beats Kotlin's four, Swift's five, Zig's four (research/04 §11). |
| Perf | Identical to E4a: one branch, unboxed tag+payload (research/02 §7.7; research/03 exec 6). |
| Error rate | Exhaustive handling enforced by difference types; the flat union kills `Result<Option<T>>` nesting (research/02 §7.6). |
| Compiler | Needs set difference on unions — presumes the `docs/en/design/01*` open option on set-theoretic types; without it, `.err` degrades to an ordinary `match` and the elegance claim weakens. |

Forecloses: depends on `docs/en/design/01*` unions; if errors later also travel as a row (B5d v2), `or`/`.err` must keep working — research/04 §11 argues they already operate on union members, which is precisely what makes the surface effect-ready.

### E4c — Exceptions-as-effects (Koka `exn` / Effekt `Exception[E]`)

```ts
function read(path: Path): throws(IoError) Data     // control-flow propagation, no return tag
try { read(p) } catch (e: IoError) { recover }      // handler discharges the row
```

Prior art: Koka's `exn` is a builtin non-resumable effect (`final ctl throw-exn`) — semantically an exception, inferred and row-composable; a plain `result<a,e>` type coexists in std/core (research/02 §1, §7.3). Effekt's stdlib defines `Result[A,E]` as the literal "reification" of `Exception[E]`, with `value` (monadic reflection) going back — the two are interdefinable, so a language needs at most one plus sugar for the other (research/02 §2, §7.3, §7.8).

| Axis | Assessment |
|---|---|
| Learning | Medium-high: dynamic handler scoping on top of whichever E1 is chosen. |
| Perf | Propagation is free of return-path rewriting and natural across callbacks/generators; a non-resumable throw is a cheap frame-walk (research/02 §7.7, §7.8). |
| Error rate | Automatic audit; but errors are not values — they cannot be stored, cached, or sent across processes, and a nursery collecting child errors as data needs the Effekt-style `result` reification shipped alongside (research/02 §7.8). |
| Compiler | The full row machinery (E1a burden) is required even if effects serve only errors. |

Forecloses: couples E4 to E1a; conflicts with E4b's storable-error property unless both mechanisms are shipped (Effekt's actual choice — both, plus the translation between them, research/02 §2).

### E4d — Let-it-crash supervision layer (BEAM)

```ts
supervise(one_for_one) {                // traps become restart events instead of aborts
  spawn { cacheLoop() }
  spawn { connAcceptor(sock) }
}
```

Prior art: BEAM supervision trees and the error-kernel pattern — a process that hits an unexpected state dies, its supervisor restarts it, and the restartable core is kept small (research/02 §7.5, §6). Inko shows the no-supervision default: a panic takes down the process with a trace (research/02 §6).

Composition (required): with E4a/E4b — a trap inside a supervised fiber is delivered to the supervisor as a restart event instead of aborting the process; trap semantics become supervision events exactly when a supervisor exists (research/02 §7.5). With E4c — the supervisor is the outermost handler catching the final `throw` operation. With a nursery (E2b/E5a) — a supervisor is simply a nursery with a restart policy, which research/02 combo 4 sketches as "let-it-crash-lite"; BEAM's copying costs, its other half, are what linear move-based messages remove (research/02 §6).

| Axis | Assessment |
|---|---|
| Learning | Medium: one more concept, but a well-known one. |
| Perf | Negligible: supervisor bookkeeping only on failure paths. |
| Error rate | Lowest for long-running systems: self-healing by construction (research/02 §6 BEAM verdict). |
| Compiler | Runtime bookkeeping; nothing in the type system. |

Forecloses: nothing — it is a layer over any of E4a-E4c; it presumes only that a task unit (fiber or process) has an identity that can be restarted.

Recommendation: **E4b as the handling surface with E4a's trap split retained for bugs** — expected errors are union data handled by `?`/`or`/`.err` (research/04 §11), bugs trap unhandleably; the split is orthogonal and near-free (research/02 §7.4). E4c is reachable only through the B5d/E1d bridge as later sugar; E4d is adoptable whenever long-running systems need it, as a nursery policy. Rationale: research/02 §7.8's recommendation verbatim (union base, `throws` as later sugar), the three-token budget (research/04 §11), and storable errors for nursery collection.

## 9. E5 — Cancellation and resources

Background: linearity does NOT give cancellation semantics (who cancels whom, when finalizers run) nor deadlock freedom — these are exactly the "missing pieces" column of research/02 Table 4 and the left-list of §7.9. Resources split into a guarantee question (who closes, provably) and a syntax question (how cleanup is written).

### E5a — Scoped cancellation only, via structured concurrency

```ts
nursery {
  spawn { download(f) }        // cooperative: observes cancellation at await points
  spawn { render(g) }
}                              // scope exit: joined; a parent cancel cancels children, cleanup runs first
```

Prior art: Trio — task lifetime lexically scoped, cancellation cooperative and scoped, the nursery does not exit until children finish; this design entered Python 3.11's `TaskGroup`/`except*` (research/02 §5.5, exec item 12). Kotlin's Job tree is the adopted-in-practice variant (research/02 §5.4). Koka's async scopes are the handler-flavored equivalent: cancellation is observed as `Cancel` results translated into a final `discontinue()`, so cleanup still runs — cancellation-with-cleanup without thread interruption (research/02 §1).

Linear/TCO interaction: cancellation is delivered only at await/preemption points, never mid-consume, so a linear resource is either fully consumed or its finalizer runs at teardown; teardown is the frame-walk that runs linear destructors (research/02 §7.9(4), §8). TCO is unaffected.

| Axis | Assessment |
|---|---|
| Learning | Low: one invariant — "tasks live in the block that spawned them" (research/02 §5.5). |
| Perf | One predictable flag check per preemption point (research/02 §8). |
| Error rate | Low: restores the invariant that a function's dynamic extent bounds its effects — the essay's structured-programming analogy (research/02 §5.5). |
| Compiler | Nursery opcodes + cancel flag + teardown walk; all already needed by E2b. |

Forecloses: detached tasks require an explicit root scope; identical mechanism to E2b (they are the same decision seen from the resource side).

### E5b — Linear resources guarantee (consume-to-close)

```ts
val f = io.open(path)?        // File is linear: exactly one owner (typing per docs/en/design/02-linearity-ownership.md)
const data = f.readAll()      // consuming read: f is dead afterwards — the consume IS the close
// no execution path lets f leak: not return, not drop, not cancellation
```

Prior art: the direct consequence of the charter's linear typing; FFI handles as linear values tie to the ownership decision C1 in `docs/en/design/02-linearity-ownership.md` (contents owned by that document). Supporting evidence: research/02 §7.9(4) — "linear resources cannot be leaked by a dropped future (Rust's cancellation-by-drop footgun is unsoundness-by-construction in a linear language — the destructor must run or the value must be consumed)", with §5.6 documenting the Rust footgun this deletes.

| Axis | Assessment |
|---|---|
| Learning | Low-medium: consume-to-close is a new idiom, but it matches the charter's monadic-extraction taste and needs no new syntax. |
| Perf | Best possible: close is a static move; no GC finalization, no drop flags; linear RC ops are compile-time deleted (research/03 exec 8). |
| Error rate | Leak-freedom by construction for in-language handles; FFI handles must be wrapped linearly with the pinning rule (research/02 §8 FFI note). |
| Compiler | The linear checking `docs/en/design/02` already mandates, plus finalizer emission at consume/teardown points. |

Forecloses: requires docs/en/design/02's linear enforcement (already a charter hard requirement); pairs naturally with E5a — cancellation cannot skip a consume, teardown runs finalizers.

### E5c — defer/errdefer blocks (Zig)

```ts
val f = io.open(path)?
errdefer f.close()            // runs on error exit only
defer log("done")             // runs on every scope exit
```

Prior art: Zig `errdefer`/`defer` over error unions — scoped cleanup with zero type-system content (research/02 §7.1, §7.4).

The sharp question it must answer here: does `defer` run when the fiber is cancelled at a point inside the scope? Zig never faces this (no cancellation); under E2b the answer must be "yes, at teardown" — which is precisely Koka's `finally`/`initially` semantics, required because multi-shot resumption makes RAII-conditional-on-single-resumption wrong (research/02 §1).

| Axis | Assessment |
|---|---|
| Learning | Lowest: familiar from Zig/Rust drop patterns. |
| Perf | Cheap: scope-exit thunks. |
| Error rate | Medium: open/defer pairing is discipline, not a guarantee — forgetting the defer is the exact hole E5b closes by construction. |
| Compiler | Trivial. |

Forecloses: nothing; but with multi-shot effects (E1a) `defer` semantics become subtle, and it should not be the normative resource story if E5b exists (belt vs suspenders — pick the guarantee as normative).

### E5d — Timeouts-as-handlers vs runtime primitives

```ts
with timeout(2 * seconds) {          // user-level combinator: firstof(timer-fiber, action-fiber)
  const page = fetchBody(url)?
} or none                            // timed out: cancellation scoped to the block
// primitive alternative: timeout(2s) { ... } as a VM opcode over a timer wheel
```

Prior art: Koka builds `timeout(secs, action)` as a plain library function — `firstof { wait(secs); Nothing } { Just(action()) }` — with `firstof`, `interleave`, and cancellation scopes all user-level handlers over the four async effects; nothing timeout-shaped is a language primitive (research/02 §1, samples/async/basic.kk).

| Axis | Assessment |
|---|---|
| Learning | Low either way; the handler shape teaches "timeouts are library code", which transfers to user-defined combinators. |
| Perf | Handler shape: two fibers plus a timer — cheap in a fiber VM; primitive shape: one opcode. |
| Error rate | Handler shape composes (nested timeouts) and inherits cancellation semantics for free (research/02 §1). |
| Compiler | Handler shape needs only the E1d runtime already required by E2b; primitive shape hardens a timer into the spec — sugar-budget and spec-growth cost (charter item 4). |

Forecloses: the handler shape presumes the nursery/handler runtime (E1d) — already on the table; the primitive shape freezes the timer abstraction at v1.

Recommendation: **E5a + E5b** — cancellation is nursery-scoped and cooperative (Trio semantics), resources are linear consume-to-close (typing owned by `docs/en/design/02-linearity-ownership.md`), with Zig-style `defer` available inside scopes as sugar over scope exit (E5c, non-normative) and timeouts as library combinators over the timer capability (E5d handler shape). Rationale: research/02 §7.9's missing-pieces list names cancellation scoping and teardown as exactly the two things linear types do not give; this pair covers both minimally, and Koka's `linear effect` + `finally` evidence shows the combined semantics are sound (research/02 §1).

## 10. Cross-point interaction map

| If you pick | It requires | It strengthens | It blocks or complicates |
|---|---|---|---|
| B5d unions-base, rows later | `docs/en/design/01*` unions (for `.err` difference typing) | E4b, E1d (the shared lowering seam), E4d (storable errors) | B5c as a starting point (demotion would be breaking) |
| E1d handler runtime, no surface effects | E2b (nursery is its first client) | E1a stays additive; E5d library shape; B5d v2 sugar | E1a as a starting point (row inference is hard to demote) |
| E2b fibers + mandatory nursery | research/03 exec 5/8/9 runtime pieces (dispatch, RC, TCO) | E5a, E3b (one shared pool), E4d (supervisor = restarting nursery) | E2c (detach), E2d (colors) |
| E3b on E3a substrate | docs/en/design/02 linearity (already charter-mandated) | E2b (shared pool, no races by theorem) | E3d (charter conflict on performance) |
| E4b + E4a trap split | `docs/en/design/01*` unions; research/04 §11 tokens | B5d, E4d, nursery error collection as data | E4c standalone (would need Effekt-style reification too) |
| E5a + E5b | docs/en/design/02 C1 (FFI handles as linear values) | E4d, E2b (same mechanism) | E5c as the normative resource story (discipline vs guarantee) |

Two standing typing obligations whatever is picked: the affine-resumption rule (no linear value captured in a multi-shot continuation — research/02 §8, to be specified in `docs/en/design/02-linearity-ownership.md` if E1a/E1d v2 ever activates) and the FFI pinning rule (foreign calls pin the fiber or run on carrier threads — research/02 §8).

## 11. Interfaces with the other catalogs (paths only; contents owned there)

- `docs/en/design/00-decision-map.md` — the index; this file contributes decision points B5 and E1-E5.
- `docs/en/design/01*` (types catalog) — owns set-theoretic unions as an open option. B5a/B5d and E4b's `.err` difference typing presume it; if it is rejected there, E4b degrades to a match-based combinator and B5a degrades to nominal enum plumbing, which changes the recommendations' cost rows but not their ordering.
- `docs/en/design/02-linearity-ownership.md` — owns linear/affine checking, ownership forms, and FFI handle representation. E3a and E5b consume its rules; the affine-resumption rule above is a typing obligation it would need to spec. This file makes no ownership decisions.
- `docs/en/design/04-syntax.md` — owns surface syntax. Every example here is illustrative; the `?`/`or`/`is`/`.err` tokens are researched (research/04 §11) but not decided here; the nursery block shape (block form vs `with`-form vs trailing-lambda builder) is an open question inherited from research/02 §d and belongs to that file.

## 12. Coherent combos

Each combo is a complete, internally consistent answer to all six points, shown as one program covering async fetch + error handling + resource close. All syntax illustrative; decided in `docs/en/design/04-syntax.md`.

### Combo A — "Capability-core" (E1c + E2b + B5a/E4b + E3b + E5a/E5b)

Point selection: B5a, E1c, E2b, E3b+E3a, E4b+E4a, E5a+E5b.

```ts
// Combo A — capability-passing IO, fiber nursery, union errors, linear resources
// illustrative; surface syntax decided in docs/en/design/04-syntax.md

interface Io {                                  // the capability: the only world-touching permission
  fn open(path: String): File | IoError         // File is linear: consume-to-close (E5b)
  fn fetch(url: Url): Resp | NetErr
}

fn fetchBody(io: Io, url: Url): Page | NetErr {
  const resp = io.fetch(url)?                   // `?` propagates NetErr (B5a/E4b)
  const body = resp.consume()                   // linear consume: read + close in one move (E5b)
  return Page(url, body)
}                                               // no path leaves the socket open

fn fetchAll(io: Io, urls: List[Url]): List[Page] | NetErr {
  val pages = Mut[List[Page]]()                 // nursery-owned accumulator
  nursery {                                     // E2b/E5a: join at exit, cancel on first failure
    for url in urls {
      spawn {
        match fetchBody(io, url) {
          page    => pages.push(page)           // moved in; pages never escapes the nursery unspoken
          error e => cancel(e)                  // cancels siblings; e re-raised at nursery exit
        }
      }
    }
  }                                             // here: all children joined or cancelled, finalizers run
  return pages.freeze()
}

fn renderTotal(io: Io, urls: List[Url]): Int | NetErr {
  const pages = fetchAll(io, urls) or return error   // `or` early-exit form (research/04 §11)
  return pages.par_map(fn(p) { p.size }).par_reduce(0, fn(a, b) { a + b })   // E3b, deterministic
}

fn main(): void | NetErr {
  val io = Io.eventLoop()                       // tests and scripts pass Io.blocking(): same code (E1c)
  print("total: ${renderTotal(io, URLS) or 0}")
}
```

Verdict — learning: low (one idiom "pass io", one block "nursery", three error tokens); perf: one vtable call per world-touching op plus O(1) fiber switches — excellent for the interpreter; error rate: low (mandatory structure, typed unions, close-by-construction). Implications for `docs/en/design/02`: linear File/Resp handles (its C1), move-into-spawn sendability, nothing exotic. Implications for `docs/en/design/04`: capability parameter convention, `nursery`/`spawn`/`cancel`, and the `?`/`or`/`is` tokens; `.err` optional at v1.

### Combo B — "Effect-ready" (E1d + E2b + B5d/E4b + E3b + E5a/E5b)

Point selection: B5d, E1d (with E1c convention kept implicit in the stdlib default), E2b, E3b+E3a, E4b+E4a, E5a+E5b.

```ts
// Combo B — unions + nursery now; evidence-passing handler runtime underneath, effects additive
// illustrative; surface syntax decided in docs/en/design/04-syntax.md

fn fetchBody(url: Url): Page | NetErr {         // no io parameter: the stdlib default handler is ambient
  const resp = fetch(url)?                      // performed through the default io handler (E1d runtime)
  return Page(url, resp.consume())              // linear consume closes (E5b)
}

fn fetchAll(urls: List[Url]): List[Page] | NetErr {
  val pages = Mut[List[Page]]()
  nursery {                                     // internally: a handler for `cancel`/`discontinue`
    for url in urls {
      spawn {
        match fetchBody(url) {
          page    => pages.push(page)
          error e => cancel(e)                  // evidence-dispatched internal effect, zero surface syntax
        }
      }
    }
  }
  return pages.freeze()
}

fn withRetry(n: Int, url: Url): Page | NetErr { // plain function: loop + unions, no effects needed
  var last = error NetErr.Timeout
  for i in 0..n {
    match fetchBody(url) { page => return page, error e => last = e }
  }
  return last
}

fn main(): void | NetErr {
  const pages = fetchAll(URLS) or die("all fetches failed")
  print("total: ${pages.par_map(fn(p) { p.size }).par_reduce(0, fn(a, b) { a + b })}")
}

// v2, additive with no migration — user handlers for final/tail-resumptive effects only (research/02 §d):
with handler { op ask() => resume(42) }         // the E1a subset, only if ever wanted
```

Verdict — learning: lowest of the three (nothing about effects is visible); perf: default-handler dispatch costs the same as Combo A's vtable call, so identical class; error rate: same low as A — the honest difference vs A is that the world-access audit is implicit (a default handler) rather than explicit (a threaded parameter), which is the one axis where A scores better. Implications for `docs/en/design/02`: same linear requirements as A, plus the affine-resumption rule worth speccing early even though v2 is deferred (research/02 §d open question 5). Implications for `docs/en/design/04`: same tokens as A; it should decide whether `with handler`/`op` are reserved as future keywords or left unassigned.

### Combo C — "Full-effects" (E1a + E2a + B5c/E4c rows + E3b + E5a/E5d)

Point selection: B5c (+E4c), E1a, E2a, E3b+E3a, E4c+E4a trap, E5a+E5b (E5d as handlers).

```ts
// Combo C — Koka-flavor: async and errors are inferred effect rows; no await keyword anywhere
// illustrative; surface syntax decided in docs/en/design/04-syntax.md

effect Http { fetch(url: Url): Resp }            // operation; some handler supplies the implementation
effect Fail { fail(e: NetErr): never }           // declared resumable, used non-resumably (final-style)

fn fetchBody(url: Url): Page | NetErr {          // rows <Http, Fail, Async> are INFERRED; shown for clarity
  const resp = fetch(url)                        // an ordinary call — performs Http, no await keyword
  return Page(url, resp.consume())               // linear consume closes; safe because resumptions
}                                                // here are affine (linear capture forbidden, research/02 §8)

fn main(): void {
  with timeout(2 * seconds) {                    // user-level handler: firstof(timerFiber, action) (E5d)
    const page = handle { fetchBody(URLS[0]) } with {
      fail(e) => defaultPage()                   // handler boundary: the Fail row simply disappears
    }
    print("size: ${page.size}")
  }
}

// the signature payoff: a pure test handler, no network, no mocks library
fn testFetch(): Bool {
  handle { fetchBody(URLS[0]); true } with {
    fetch(url) => resume(FakeResp(url))          // resumption returns control to the call site
    fail(e)    => panic("unexpected in test: ${e}")
  }
}
```

Verdict — learning: highest (rows in every signature users write; row-bearing error messages; handler scoping); perf: good — evidence passing, capture-free tail-resumptions, per-frame closure capture otherwise; error rate: low at runtime, but inference-error UX is a real cost and the compiler carries the highest complexity of any option (row inference unified with linear/affine unification, thin prior art — research/02 combo 3). Implications for `docs/en/design/02`: must spec the affine-resumption rule BEFORE surface effects ship (research/02 §d open question 5) and lean on Perceus static-count deletion for linear values (research/03 exec 8). Implications for `docs/en/design/04`: `effect`/`with handler`/`resume`/row-display conventions — by far the largest sugar-budget ask of the three combos, all spent on one feature (charter item 7's "adopt only if they earn their complexity" is the question this combo must answer yes to).

### Combo comparison at a glance

| Combo | Points | Learning | Perf | Error rate | 02 impact | 04 impact |
|---|---|---|---|---|---|---|
| A Capability-core | E1c+E2b+E3b+E4b+E5a/b | low | excellent (vtable + O(1) switch) | low | linear handles, move-sendability | capability convention, nursery, 3 tokens |
| B Effect-ready | E1d+E2b+E3b+E4b+E5a/b | lowest | same class as A | low (+ implicit audit) | same as A + affine-resumption rule early | same as A; reserve `with handler` |
| C Full-effects | E1a+E2a+E3b+E4c+E5a/d | highest | good (evidence passing) | low runtime, high inference-UX | affine-resumption rule mandatory pre-v1 | largest new surface: effect/handler syntax |

The research's own ordering is B over A over C: B is combo 6 of research/02 §d, A is combos 1+2, C is combo 3 — and §d's stated risk for C is committing to row inference across the whole type system before the linear/affine unification story exists.

## 13. Sources

Primary carriers are the research notes, which hold the full source lists: research/02 (all concurrency/effects/error claims above), research/04 §11 (extraction surface), research/03 exec items 5-9 (VM constraints). Direct links for the load-bearing claims: Xie & Leijen ICFP'21 https://www.microsoft.com/en-us/research/publication/generalized-evidence-passing-for-effect-handlers-or-efficient-compilation-of-effect-handlers-to-c/; Leijen structured asynchrony https://www.microsoft.com/en-us/research/wp-content/uploads/2017/05/asynceffects-msr-tr-2017-21.pdf; Koka https://github.com/koka-lang/koka; Effekt https://github.com/effekt-lang/effekt and https://effekt-lang.org/publications; Unison https://www.unison-lang.org/docs/fundamentals/abilities/; Zig 0.15.1 notes https://ziglang.org/download/0.15.1/release-notes.html and devlog https://ziglang.org/devlog/2025/; Trio https://vorpus.org/blog/notes-on-structured-concurrency-or-go-statement-considered-harmful/; Kotlin https://kotlinlang.org/docs/coroutine-context-and-dispatchers.html; What Color https://journal.stuffwithstuff.com/2015/02/01/what-color-is-your-function/; without.boats https://without.boats/blog/coroutines-and-effects/; Swift SE-0414/0430 https://github.com/swiftlang/swift-evolution/blob/main/proposals/; Roc https://www.roc-lang.org/faq; typesanitizer error models https://typesanitizer.com/blog/errors.html; unions essay https://www.arshad.fyi/writings/null-errors-and-enums-are-one-feature; Go preemption https://go.googlesource.com/proposal/+/master/design/24543-non-cooperative-preemption.md; Pony https://www.ponylang.io/media/papers/codesigning.pdf; Inko vendored docs (research/02 §6); OCaml 5 manual https://ocaml.org/manual/5.4/.
