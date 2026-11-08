# 07 — Runtime: execution pipeline, tail calls, and memory mechanics: Decision Catalog

Status: Phase 2 option catalog, written 2026-11-04, awaiting author decisions per AGENTS.md. Primary evidence: `docs/en/research/03-runtime-ir-jit.md`, cited as research/03 with §a.N = executive summary item N, §b.N = deep dive N, §c Table N, §d = pipeline section; URLs cited inline where research/03 records them. Sister catalogs referenced by path only, contents owned there: `docs/en/design/01-types.md` (types, numerics, strings, equality), `docs/en/design/02-linearity-ownership.md` (affine core C1, memory C4, deinit C5), `docs/en/design/03-effects-errors-async.md` (errors B5, effects E1, fibers E2), `docs/en/design/06-modules-tooling.md` (modules G3, dsmod contract G3.5, tooling H6); `docs/en/design/05-generics-metaprogramming.md` is not read here and derive/Ord/Hash questions are owned there. Points H1, H2, H4 as assigned by `docs/en/design/00-decision-map.md`. All code is illustrative — the examples predate the docs/en/design/04 surface decisions and illustrate semantics only; surface syntax is decided in `docs/en/design/04-syntax.md`.

## Decision-point index

| ID | Decision point | Options | Recommendation (one line) |
|---|---|---|---|
| H1 | Execution pipeline | (a) tier1 typed register VM + tier2 copy-and-patch JIT + tier3 Cranelift/wasm-AOT over one shared bytecode-to-SSA lowering, (b) register VM + single method-JIT tier, (c) AST interpreter + meta-tracing JIT (PyPy/RPython), (d) wasm-AOT primary target + interpreter fallback (MoonBit posture) | (a): the three tiers amortize one lowering and one uop set; tier2's stencils are the tier1 uops one-to-one, so each tier is additive, never a fork (research/03 §d) |
| H2 | Tail-call guarantee | (a) guaranteed TCO in the spec from day one, self AND mutual, via frame reuse in the VM and label-loop/slot-overlay lowering in JIT tiers, (b) best-effort TCO (JIT-dependent), (c) user-level trampolines as a library convention, (d) tail-resumptions provided only by the effect system | (a): charter req 3; it is a pointer shuffle in tier1 (Lua `luaD_pretailcall`) and a label loop or slot-overlay jmp in JIT tiers (Koka `kk__tailcall`) — the cheapest guarantee on the menu (research/03 §b.5) |
| H4 | Memory runtime mechanics | (a) Perceus precise non-atomic RC + block reuse + per-frame bump arenas for non-escaping temporaries, (b) generational moving GC tuned for immutable data, (c) malloc/free per object with no GC (pure linear ownership), (d) hybrid: RC for shared/boxed + arenas for linear data + optional concurrent cycle collector deferred | (a), explicitly inheriting (d)'s cycle-collector interface as the pre-planned growth path for shared/cyclic workloads (research/03 §a.8, §b.6) |

Scope note: H1 decides which execution tiers exist and how control moves between them; it does not re-decide the value representation (docs/en/design/01), the memory strategy (H4), or the bytecode file contract (docs/en/design/06 G3.5). H2 decides the tail-call semantics and its mechanism; H4 decides what happens to memory at runtime beneath the tiers. The three are separable decisions but combinable only in coherent ways, catalogued in section 7; the fixed baseline of section 1 binds all of them. Point numbering follows `docs/en/design/00-decision-map.md`; the dispatch-mechanism choice is baseline BL-3, not a separate point.

## 1. Fixed baseline (binding on every option)

These are not options. They are the non-negotiables research/03 derives from the charter (static types, ultra-perf, TCO, interpreted-with-IR), and every H1/H2/H4 option below is scored against them; an option that violates a baseline item is listed as foreclosed by it.

**BL-1 Typed register bytecode.** 32-bit fixed-format instructions, register-index operands, typed opcodes: arithmetic specializes by representation (ADDI64/ADDF64, comparisons EQI64/EQF64, loads KIMM/KCONST) following LuaJIT's ADDVN/ADDNV/ADDVV discipline (`lj_bc.h`), except the dynamic "V" variants do not exist because every operand's type is a compile-time fact (research/03 §b.1 "Typed opcodes in bytecode"). Format details worth copying verbatim: biased jump offsets (BCBIAS_J, an unsigned compare-free subtraction) and XOR-negated comparison pairs (ISEQV^1 == ISNEV) (research/03 §b.1).

**BL-2 Unboxed slots.** No NaN-boxing, no per-slot tags, no boxed scalars: i32/i64/f64/bool sit raw in frame slots, references are raw pointers, ADTs/unions/Result are unboxed tag+payload pairs spread across consecutive slots (research/03 §b.4, §c Table 4). Boxing is the 2-10x scalar-code overhead term static typing exists to delete (research/03 §b.10); docs/en/design/01 "Shared assumptions" already scores every type option against this baseline.

**BL-3 Dispatch.** Computed goto (`lua/lvm.c` + `ljumptab.h`, CPython `USE_COMPUTED_GOTOS`, +5-20% over switch) or C tail-call threading (CPython `_Py_TAIL_CALL_INTERP`, `musttail` + `preserve_none`, clang/GCC/MSVC 2026+) — both production-validated; which one ships is a build-portability choice, not a design point (research/03 §b.1, §c Table 2).

**BL-4 Superinstructions on profile data.** The assembler composes superinstructions (compare-and-branch fusion, load-op fusion, dense-tag match jump tables) from measured opcode frequencies — CPython's `macro(BINARY_OP)` composition pattern, LuaJIT's ISNEXT/RETM folds as precedent (research/03 §b.1 "Superinstructions and macro-ops", §b.9). Fusion examples composed by the assembler from measured frequency, never hand-picked:

```
;   TAGTEST r2, OK_TAG ; JMPIF_NOT r2 -> L    =>  TAGBR r2, OK_TAG, L     (match dispatch)
;   LDIMM r1, 0:i64 ; ADDI64 r1, r1, r0       =>  ADDIMM r1, r0, 0:i64    (load-op)
```

**BL-5 Uop decomposition from day one.** The instruction set is defined as micro-ops with explicit effects (CPython `bytecodes.c` discipline: explicit stack/slot effects, `pure` annotations), and the copy-and-patch stencil set is these uops one-to-one by construction (research/03 §b.1, §d Pipeline B). This is what keeps H1.a's tier2 a derivative artifact rather than a second compiler.

**BL-6 Frames inline in a typed value stack.** Each frame is a contiguous span of statically-typed slots plus a metadata word (return address, frame size, flags for TCO chains) — LuaJIT `lj_frame.h` layout, chosen because the layout is known per function: calls are cheap, TCO is a pointer shuffle, the stencil ABI is trivial, debugger materialization walks metadata words (research/03 §b.1 frame-design comparison, "For DreamScript" paragraph).

**BL-7 Position-independent, mmap-able dsmod.** Fixed-width, index-based, relocation-free bytecode with a module header (version, type/constant tables, per-function frame-slot types, frame sizes, max call depth) and an IR-version checksum — one mmap, no relocation, cheap `load` (research/03 §b.1 format design, §b.9 sketch). The format contract is owned by docs/en/design/06 G3.5: cache at `$DS_CACHE/ir/<triple>/<blake3>.dsmod`, dep-list-driven lazy loading, embedded-bytecode single binaries via `ds build --exe` (Bun's measured 1.38x `--compile --bytecode` startup gain is the floor), and Erlang-style two-version module slots for hot reload (docs/en/design/06 G3.5 and H6 mandatory item 5).

The baseline executed end-to-end (the `sum_to` encoding from research/03 §b.1, every slot typed, no tags anywhere):

```
fn sum_to(n: i64) -> i64 { let mut acc = 0i64; while n > 0 { acc += n; n -= 1 }; acc }
  0: FUNCF  frame=2            ; slots: r0=n:i64, r1=acc:i64 (static layout)
  1: LDIMM  r1, 0:i64
  2: LEI64  r2, r0, 0          ; unboxed bool
  3: JMPIF_NOT r2 -> 6
  4: ADDI64 r1, r1, r0         ; pure machine add, no tag work
  5: SUBI64 r0, r0, 1 ; JMP 2
  6: RET    r1
```

## 2. H1 — Execution pipeline

The question is which execution tiers exist, in what order, and what machinery sits between them. research/03's evidence: every surveyed dynamic VM spends the bulk of its JIT on runtime type speculation (guards, snapshots, feedback vectors) that DreamScript's type system deletes unconditionally (research/03 §a.3, §b.3), so the viable designs differ mainly in how much of the remaining, speculation-free work they do. Every option runs on baseline BL-1 through BL-7 unchanged — the tiers differ, the substrate does not. Perf anchors used throughout: copy-and-patch output ~14% faster than LLVM -O0 at ~100x its compile speed (arXiv:2011.13127); CPython ships it as PEP 744; wasm ~1.55x native in Chrome (IMC 2019); plain Lua ~20-50x slower than C; LuaJIT ~2-3x from C; realistic DreamScript targets 3-10x of C for the pure interpreter, 1.5-3x with copy-and-patch, 1-2x with an optimizing tier (research/03 §a.11, §b.10).

### H1.a — Tiered: tier1 typed register VM + tier2 copy-and-patch + tier3 Cranelift or wasm-AOT (shared lowering)

```
foo.ds -> [check/emit] -> typed uop bytecode (dsmod, mmap, BL-7)
                            |
  tier 1  interpreter       | computed-goto loop over uops; unboxed frame (BL-2/3/6)
  (instant start)           | hot back-edge? per-position counter (LuaJIT hotcount /
                            | CPython backoff-counter pattern)
                            v
  tier 2  copy-and-patch    | select one stencil per uop, patch u16/u64 holes from the
  (us per unit)             | code object; rewrite bytecode in place to J-variant opcodes
                            | hot unit + profile?
                            v
  tier 3  optimizing        | bytecode -> typed SSA -> CLIF (Cranelift, ISLE rules)
  (ms per function)         | or -> wasm 3.0 module (return_call carries TCO)
```

Concrete example, the tier2 stencil for `ADDI64 r1, r1, r0` (research/03 §b.2 worked example): the stencil library holds one x86-64 fragment per operand class, emitted at build time by LLVM from a master template that includes the VM headers (CPython `Tools/jit/template.c` model), with holes for frame-slot offsets and constants.

```
; stencil ADDI64_rr, holes marked <H>; no type checks, no RC calls, no error glue
mov  rax, [r14 + <H off_r1>]     ; hole: u16 frame-slot offset of r1
add  rax, [r14 + <H off_r0>]
mov  [r14 + <H off_r1>], rax
; compile loop body = concat stencils + patch holes; sub-microsecond per stencil
```

Tier3's concrete form is the shared lowering into typed SSA and then CLIF (research/03 §b.7); the same `sum_to` loop lowers mechanically because every CLIF value type is known statically — no boxing conversions, no checks to emit:

```
v0: i64 = param n                function %sum_to(i64) -> i64 {
loop(v2: i64 = 0, v3: i64 = v0):   block0(v0: i64):
  v4: bool = le v3, 0                v4 = icmp sle v0, 0 ; brif v4, block2, block1
  br_if v4 -> exit                 block1(v2: i64, v3: i64):      ; block params, no phis
  v5: i64 = add v2, v3               v5 = iadd v2, v3 ; v6 = isub_imm v3, 1
  v6: i64 = sub v3, 1                jump block1(v5, v6)
  br loop(v5, v6)                  block2(v7: i64): return v7 }
```

DreamScript-specific rewrites slot in as directional ISLE rules at this layer (Cranelift's `opts/README.md` discipline: rules must not rewrite to worse code, `subsume` where uses vanish): buffer fusion for linear buffers (no aliasing proof needed), effect-directed reordering gated on empty effect rows, bounds-check elimination where a linear cursor discipline already proved containment (research/03 §b.7 "DreamScript-specific rules").

Prior art: copy-and-patch, Xu & Kjolstad (https://arxiv.org/abs/2011.13127), CPython PEP 744 (`Tools/jit/`); Cranelift typed SSA with directional ISLE rules (research/03 §b.7); wasm 3.0 `return_call` (research/03 §b.5); the full pipeline is research/03 §d Pipelines B+C+D and its "Recommendation and sequencing" paragraph.

**Tier transition.** Counters trigger compilation (per-bytecode-position hot counters or CPython's backoff counter at the `_JIT` uop); installation then rewrites the bytecode in place to J-variant opcodes (LuaJIT's JFORL/JLOOP/JFUNCF trick), so tier transitions cost one dispatch and OSR reduces to "start interpreting/compiling this unit at the back-edge" — no frame transformation machinery, because the unit is the loop body, not the function (research/03 §b.1 trace-stitched bullet, §b.2 OSR paragraph). Tier3 compiles whole functions on profile, keyed on (function version, stencil set) so hot reload recompiles only touched functions (research/03 §b.9).

**Deopt scope.** No guards on any data path; the only exits are (i) debugger breakpoints (re-materialize frames from JIT frame metadata — Sparkplug's frame-preservation invariant is the rule), (ii) a stack-limit compare per function entry (static frame sizes), (iii) effect-handler entry, a control transfer not a deopt (docs/en/design/03 E1d), and (iv) under BL-2 the ADDOV-class guard survives only if B7 trapping arithmetic needs a cold overflow edge (research/03 §b.2 deopt table, §b.2 ADDOV recommendation).

**Perf target vs C.** tier1 3-10x, tier2 1.5-3x, tier3 1-2x, wasm deployment of the same SSA 1.5-2.5x (research/03 §c Table 1, §d); no deopt cliffs anywhere, unlike LuaJIT's documented trace-exit cliffs (https://blog.cloudflare.com/more-consistent-luajit-performance).

**Startup.** tier1 starts instantly on mmap'd dsmod (BL-7); stencil library is a few MB loaded lazily; tier2 compile is microseconds per unit; short scripts (<0.2s, PyPy's documented dead zone) never pay tiering at all and still hit 3-10x (research/03 §b.9, §b.2).

| Axis | Assessment |
|---|---|
| Perf ceiling | 1-2x of C at tier3; tier2 alone already reaches baseline-compiler class (research/03 §d Pipeline B) |
| Implementation complexity | Medium overall: stencil build pipeline + patcher (tooling proven by CPython, LLVM 21 as build-time-only dependency), plus SSA lowering for tier3; no optimization passes written by hand |
| Startup | Best of all options: instant tier1, us-scale tier2 triggers, no warmup phase |
| Portability | Stencils are per-architecture (x86-64, aarch64 first); platforms without verified stencil builds ship tier1-only, exactly CPython's opt-in-per-platform JIT posture (research/03 §b.12 risk 5) |

Forecloses: single-VM simplicity (this is three artifacts), and any design where the interpreter's uops and the JIT's units diverge — BL-5 is what makes the divergence impossible.

### H1.b — Register VM + a single method-JIT tier

One JIT tier above the interpreter: a method-level compiler (Cranelift over the typed SSA, or a small hand-written emitter per research/03 §b.12 risk 2's fallback philosophy) compiles whole functions on hotness.

```
foo.ds -> [check/emit] -> dsmod (mmap)
                            |
  tier 1  interpreter       | 3-10x of C; per-function counter
                            | hot function? compile whole body
                            v
  tier 2  method JIT        | bytecode -> typed SSA -> CLIF:
  (ms per function)         |   v5: i64 = add v2, v3        (research/03 §b.7 sketch)
                            |   br loop(v5, v6)             block params, no phis
```

Prior art: research/03 §d Pipeline C; method compilation is how Sparkplug/Maglev/TurboFan and the JVM all tier — but every one of them needed OSR because the compile unit is the whole function (research/03 §b.2 OSR paragraph).

**Tier transition.** Per-function counters; entering compiled code at the next call is trivial, but a hot loop inside a cold-called function needs either OSR with frame transformation (the machinery research/03 says we avoid) or acceptance that such loops stay interpreted — the honest cost of "one tier". **Deopt scope.** Same four residual exits as H1.a, plus one new class: SSA values must be mapped back to frame slots for materialization, which is exactly the bookkeeping Sparkplug avoids by preserving interpreter frame layout — the mitigation (tier2 must reuse BL-6 slot layout) is a hard rule here, not a nicety. **Perf target vs C.** 3-10x before compile, 1-2x after; the 1.5-3x band is skipped, and short scripts that never hit the compile counter stay at tier1 quality — acceptable, since tier1 is already strong. **Startup.** Identical to H1.a minus the stencil library; compile cost is ms per function, so first-run latency of hot functions is visible but bounded.

| Axis | Assessment |
|---|---|
| Perf ceiling | 1-2x of C (same tier3 numbers, reached via one compiler instead of two mechanisms) |
| Implementation complexity | Lowest of the JIT options on paper (one compiler), but OSR-or-accept-the-loss is a real design debt; loses the "uops = stencils one-to-one" amortization of BL-5 |
| Startup | Good; ms-scale first-run compile of hot functions |
| Portability | Best of the JIT options: Cranelift (or wasm) is already multi-arch; no per-arch stencil CI matrix |

Forecloses: the guard-free stencil economics (near-zero compile cost, no register allocator) and CPython-PEP-744-style additive tiering; if the stencil toolchain proves unacceptable this is the fallback shape (research/03 §b.12 risk 2). When (b) wins: if profiling shows the per-arch stencil CI matrix dominating maintenance cost while Cranelift is already a dependency for the wasm/native deployment path, collapsing tiers 2+3 into one compiler is the honest simplification. **Verdict — no, keep as fallback.**

### H1.c — AST interpreter + meta-tracing JIT (PyPy/RPython posture)

Write the interpreter in an RPython-style statically typed subset; a meta-tracing JIT observes hot interpreter loops and traces what the interpreter does, so one JIT serves the language without a per-language compiler.

```
eval(node: Expr, env: Env) -> Value {          // tree-walker; JIT records this loop
  match node { Bin(op, l, r) => apply(op, eval(l, env), eval(r, env)), ... }
}
// meta-trace: unfold eval + apply for the hot subtree; compile; deopt on new shape
```

Prior art: PyPy — warmup "at least a couple of seconds"; scripts under ~0.2s and unit tests never benefit; introspection (`sys._getframe`, tracing hooks) disables the JIT over its scopes; runtime-support-heavy code up to ~2x slower than CPython (https://www.pypy.org/performance.html, research/03 §b.2). The RPython bootstrap strategy is real: it is how PyPy itself was built, and it is the cheapest known route to a strong JIT *for a dynamic language*. Applied here it would mean writing the DreamScript interpreter in a statically typed restricted host language, letting the meta-JIT observe the interpreter's own loops, and accepting the translator toolchain as a hard build dependency — one strong JIT "for free" in exchange for owning PyPy's warmup cliffs, its introspection-disable semantics (which sit exactly where debuggers and docs/en/design/06 H6 hot reload live), and a translator nobody outside that community maintains (research/03 §b.2 PyPy bullet).

**Tier transition.** Hot interpreter-loop counters inside the meta-JIT; the language sees only warmup cliffs. **Deopt scope.** Trace invalidation on assumption failure plus the introspection-disable problem — for us, debuggers and hot-reload (docs/en/design/06 H6 two-version slots) would sit exactly in the disable-the-JIT scope. **Perf target vs C.** Potentially 5-15x after multi-second warmup, with documented 2x-slower pathologies; research/03's verdict is blunt: for a static language this is the wrong trade — the interpreter is already type-directed, so tracing spends its budget on AST-node dispatch that tier1's typed bytecode eliminates for free (research/03 §b.2 PyPy bullet, §c Table 3 "poor fit"). **Startup.** Worst of the four: no fixed-width bytecode artifact means no mmap dsmod (BL-7), no `ds build --exe` bytecode story, no superinstruction-on-profile substrate; startup is parse-or-serialized-AST every run.

| Axis | Assessment |
|---|---|
| Perf ceiling | High after warmup, but the ceiling reason (erasing dynamic dispatch) barely applies to a typed interpreter (research/03 §b.2) |
| Implementation complexity | Highest ongoing: trace recorder, loop detection, trace linking, invalidation — LuaJIT-scale machinery with less of its justification (research/03 §d Pipeline E) |
| Startup | Multi-second warmup for long-running programs; no bytecode-cache story at all |
| Portability | Tied to the meta-JIT toolchain (RPython or equivalent) |

Forecloses: BL-7 entirely (charter req 1 says internal IR/bytecode "is allowed and expected" — this option declines it), the J-variant tiering hook, and docs/en/design/06's dsmod/hot-reload contract. **Verdict — no.**

### H1.d — wasm-AOT as primary target with interpreter fallback (MoonBit posture)

The typed SSA compiles to wasm 3.0 (`return_call` standardized September 2025 carries the TCO guarantee through engine tiers); the interpreter is the dev/debug substrate and the fallback for engines without wasm 3.0 (research/03 §b.7 MoonBit, §b.5 platform inheritance, §d Pipeline D).

```
release: foo.ds -> [check] -> typed SSA -> wasm 3.0 module -> engine (AOT/Liftoff)
dev:     foo.ds -> [check/emit] -> dsmod -> tier1 interpreter (instant edit-run)
```

Prior art: MoonBit — typed core IR to wasm/JS/native, 25x faster than native JS iterator helpers, ~8x faster than Json5, attributed to compiler-level iterator fusion and tail-recursion-to-loop; the existence proof that a small team with one clean typed IR reaches near-C throughput on wasm (https://www.moonbitlang.com/blog/js-support, research/03 §b.7). **Tier transition.** None at runtime — "tiering" becomes a build-mode choice (dev = interpreter, release = wasm); engine-internal tiering is inherited, not owned. **Deopt scope.** None (AOT); debugging rides the interpreter plus wasm source maps; stack limits are the engine's. **Perf target vs C.** 1.5-2.5x of C in engines (wasm ~1.55x native geomean, IMC 2019, https://ar5iv.labs.arxiv.org/html/1901.09056), narrowing with wasm 3.0 features; native AOT via Cranelift equals H1.a's tier3 if that path is added. **Startup.** Wasm compile per module (fast tiers exist, but engine variance is real); loses the instant edit-run loop unless the interpreter is maintained anyway — at which point two runtimes are being maintained, which is H1.a's shape minus tier2.

| Axis | Assessment |
|---|---|
| Perf ceiling | 1.5-2.5x on wasm; native 1-2x only if a native backend is added later |
| Implementation complexity | Low-medium: a wasm emitter is far simpler than any JIT (research/03 §d Pipeline D) |
| Startup | Engine-dependent wasm compile; interpreter fallback preserves the scripting loop |
| Portability | Maximal: browser, sandbox, embedding — the strongest story of the four |

Forecloses: the us-scale middle tier (copy-and-patch stencils are pointless under wasm-AOT); interacts badly with docs/en/design/03 E2b fibers, which must lower to wasm threads/JSPI or JS async rather than interpreter data structures — a real cost the E2b recommendation does not currently pay for (research/03 §d Pipeline D tradeoff). **Verdict — no as primary, yes as the deployment target H1.a's tier3 grows into.**

### H1 comparison

| Axis | a tiered C&P + Cranelift/wasm | b single method JIT | c AST + meta-tracing | d wasm-AOT primary |
|---|---|---|---|---|
| Perf vs C, peak | 1-2x at tier3; 1.5-3x at tier2 | 1-2x after compile | 5-15x after seconds of warmup; 2x-slower cases documented | 1.5-2.5x (wasm) |
| Perf vs C, floor | 3-10x tier1, no cliffs | 3-10x tier1 floor | <0.2s scripts never benefit (dead zone) | 3-10x interpreter fallback |
| Implementation complexity | stencil pipeline + SSA lowering; ~70% of dynamic-VM tiering risk deleted (research/03 §b.2) | one compiler, but OSR-or-accept-the-loss debt | highest ongoing: recorder, linking, invalidation | lowest: a wasm emitter, no owned JIT |
| Startup | instant tier1; us-scale tier2 triggers | instant; ms-scale first-compile of hot functions | multi-second warmup; no bytecode artifact at all | engine wasm compile; interpreter preserves the dev loop |
| Portability | per-arch stencil CI; tier1-only fallback on unverified platforms | best of the JIT options (Cranelift multi-arch) | tied to the meta-JIT toolchain | maximal: browser, sandbox, embedding |
| Reversibility | strictly additive: dropping tier2 or tier3 leaves tier1 intact | tier1 intact; the one JIT is swappable | hardest to reverse: the interpreter is written for the JIT | a deployment-mode switch, not a rewrite |
| Charter/baseline conflicts | none | BL-5 amortization lost | BL-7 foreclosed; charter req 1 declined | E2b fiber lowering cost |

**Recommendation (H1): (a)** — tier1 first, copy-and-patch second, Cranelift/wasm third, all over one uop set and one bytecode-to-SSA lowering built early so every other tier stays additive (research/03 §d sequencing); (b) is the fallback if the stencil toolchain proves unacceptable, and (d) is (a)'s deployment mode, not its replacement.

## 3. H2 — Tail calls

Charter hard req 3 names guaranteed TCO as a fixed input, so options (b)-(d) are graded on giving it up. The mechanism evidence: Lua's `luaD_pretailcall` (`lua/ldo.c` ~line 671) reuses the CallInfo, slides callee+args down, resets pc, sets `CIST_TAIL` — O(args), correct for self AND mutual tail calls; Koka lowers every function body into a loop with a `kk__tailcall:` label, reassigning parameters via temporaries first (`genTailVarBinding`, `FromCore.hs` ~line 2817); LuaJIT's CALLT/CALLMT copy args onto the caller's frame slots and jump; wasm 3.0 `return_call` inherits the guarantee platform-wide (research/03 §b.5, §c table).

### H2.a — Guaranteed TCO in the spec from day one (self AND mutual), frame reuse + slot-overlay ABI

```
; fn even(n: i64) -> Bool { if n == 0 { true } else { odd(n - 1) } }   ; mutual
even:
  0: EQI64   r1, r0, 0
  1: JMPIF   r1 -> 3
  2: SUBI64  r3, r0, 1
  3: CALLT   odd, nargs=1, src=r3    ; tier1: slide r3 into slot0, reuse frame,
             ;                        reload prologue, continue loop (CIST_TAIL)
  4: RET     true
```

The ABI constraint that makes this exact everywhere: callee argument slots overlay caller argument slots, locked at frame-layout time (BL-6) — the caller's outgoing-arg area is the tail of its own frame where the callee's param slots land, so a tail call compiles to (at most) an O(delta) arg slide plus a jmp; the return address and frame pointer never move, and the stack never grows at any depth (research/03 §b.5 machine-level sketch, §d "two design decisions to lock early"). JIT tiers lower self tail calls as Koka's label loop (`goto kk__tailcall` — the C compiler sees an ordinary loop, so RA and branch prediction are optimal) and mutual tail calls as the slot-overlay shuffle + `jmp callee_entry` (LuaJIT CALLT); both are O(args), allocation-free, and perform zero RC traffic because moved arguments transfer ownership (research/03 §b.5 "Interactions" item 3).

**TCO x fibers (docs/en/design/03 E2b).** A fiber stack is interpreter data — a heap-resident frame/CallInfo segment; switching is a pointer swap, O(1) (docs/en/design/03 shared assumptions, research/02 §8). Frame reuse happens *inside* the fiber's own segment, so tail recursion never grows fiber stacks either: a spawned fiber that immediately tail-loops stays at its initial segment size forever, and the nursery's spawn cost is unchanged. No fiber-stack-vs-TCO conflict exists in this design; the only rule is that fiber segment growth (deep non-tail recursion) is a chunk-list append, not a realloc, so captured frame addresses stay stable across growth. **TCO x copy-and-patch stencils.** The stencil set needs exactly two call-related templates: `TAILCALL` (register shuffles + `jmp <H callee_entry_descriptor>`) and `RET`; a self tail call inside a compiled unit is the label loop, needing no stencil at all. Cross-tier tail calls go through a two-word callee entry descriptor `{tier1_prologue_ptr, tier2_code_ptr}`: a tail call from compiled code into an uncompiled callee jmps to its tier1 prologue (one dispatch, no growth), and when the callee later compiles, one descriptor word flips — the same invalidation story as hot reload (docs/en/design/06 G3.b two-version rule: running frames continue on old code until they return). **Debugger.** Frame reuse destroys caller frames, so backtraces carry explicit tail-call chain metadata (Lua's `CIST_TAIL` supports `debug.traceback` despite full TCO) with lazy materialization of elided frames only when the debugger asks (research/03 §b.5 "Interactions" item 2). **Effects.** The guarantee reads "proper tail calls within the same effect context" — a tail call under a `handle` boundary materializes a frame for the continuation (docs/en/design/03 E1d handler runtime; Koka's one-shot continuations are the evidence this is acceptable, research/03 §b.5 "Interactions" item 1).

Prior art: Scheme/R7RS spec-level guarantee; Lua `luaD_pretailcall` + `CIST_TAIL`; LuaJIT CALLT/CALLMT; Koka `kk__tailcall`; wasm 3.0 `return_call` (research/03 §b.5 per-language table). Tradeoffs: the spec commitment forces tail position to be a defined concept and backtraces to carry chain metadata — a small spec and debugger cost, paid once; implementation cost in tier1 is a pointer shuffle plus arg slide. Forecloses: ES6-style "spec without engines" is avoided by construction (tier1 implements it first), and any design where deep recursion is JIT-behavior-dependent.

### H2.b — Best-effort TCO (JIT-dependent)

CRuby's compile-time tail-call option is off by default, and neither YJIT nor the newer ZJIT transforms tail-recursive calls into loops as of 2025-2026 (http://blog.tdg5.com/tail-call-optimization-in-ruby-background/ and ruby-core ZJIT threads, research/03 §b.5) — JIT teams consistently deprioritize TCO because frame elision interacts with deopt metadata and stack introspection, precisely the machinery a dynamic VM carries and DreamScript does not. **Tier transition.** None to design — which is the trap: the guarantee's availability becomes a property of which tier compiled the caller. **Deopt scope.** Unchanged, but backtraces and stack-limit behavior now depend on JIT coverage. **Perf target vs C.** Identical to H2.a where it fires; unbounded stack growth where it does not. **Startup.** Unchanged. Tradeoffs: saves the ABI commitment (slot overlay) and the tail-chain metadata; costs predictability in the one area the charter refuses to trade (req 3). Forecloses: spec-level guarantee, and the deep-recursion idioms (state machines, folds over unbounded structures) that scripting languages are used for. **Verdict — no.**

### H2.c — User-level trampolines (library)

```ts
enum Bounce { Done(Bool), Call(fn() -> Bounce) }         // library type
fn even(n: i64): Bounce { if n == 0 { Done(true) } else { Call(() => odd(n - 1)) } }
run(trampoline(even(1_000_000)))                          // driver loop jumps thunk to thunk
```

Prior art: Chicken Scheme's "Cheney on the MTA" — every call returns the next thunk; simple and portable, but adds an indirect jump per call and complicates native-JIT interop (research/03 §b.5 trampolining bullet). **Tier transition.** The driver loop is user code; the JIT sees an ordinary loop and may compile it — TCO quality then depends on inlining the thunks, which is speculation-shaped work we deleted. **Deopt scope.** Unchanged, plus closure allocations per bounce (each `Call` boxes a thunk) — the ADT-in-registers dividend (research/03 §b.4) partially forfeited on exactly the hot path. **Perf target vs C.** Worse than H2.a: one indirect call plus allocation per iteration where H2.a performs a shuffle. **Startup.** Unchanged. Tradeoffs: zero runtime commitment, full user burden; the convention infects every tail-recursive signature the library touches (caller and callee must both speak Bounce). Forecloses: TCO as an invisible language property; portable to any host, which is its only advantage. **Verdict — no as the mechanism, fine as an escape hatch for effect-context boundaries where H2.a's guarantee is scoped out.**

### H2.d — Tail-resumptions only for effects (Koka linear-effect posture)

Only effect operations get resumption-style non-stack control; ordinary recursion grows the stack unbounded. Ties to docs/en/design/03 E1d (handler runtime underneath, no surface effect syntax): with effects deferred, this option reduces to "no TCO at all" in v1 — a direct violation of charter req 3, which is why research/03 lists guaranteeing TCO in the semantics as the deliberate choice Ruby never made (research/03 §b.5 counter-evidence paragraph). **Tier transition.** N/A. **Deopt scope.** Handler capture materializes frames (the same cost H2.a already budgets). **Perf target vs C.** Depth-limited programs overflow; the performance ceiling is irrelevant where the semantics are missing. **Startup.** Unchanged. Tradeoffs: minimal runtime machinery if effects are the only capture mechanism; costs the guarantee outright. Forecloses: charter req 3; revisitable only as an *addition* to H2.a once docs/en/design/03 E1 surfaces, never as a substitute. **Verdict — no.**

### H2 comparison

| Option | Guarantee strength | tier1 cost | JIT-tier cost | Spec/backtrace cost | Fits charter req 3 |
|---|---|---|---|---|---|
| a guaranteed day one | self + mutual, always | arg slide + frame reuse (near-free) | label loop / overlay jmp + one TAILCALL stencil | tail position becomes defined; chain metadata | yes |
| b best-effort | none; tier-dependent | none | deferred, often never (Ruby YJIT/ZJIT) | none, but backtraces misreport depth | no |
| c trampolines | user-enforced only | library loop, allocation per bounce | thunk inlining is speculation-shaped work | convention infects every signature | no |
| d effects only | resumptions only | none until effects land | capture materialization at handlers | scoped to effect contexts | no (reduces to "no TCO" under E1d) |

Per-language evidence (research/03 §b.5 table): Lua 5.4/5.5 guaranteed via CallInfo reuse (`luaD_pretailcall`, `CIST_TAIL`); LuaJIT guaranteed via CALLT/CALLMT arg slide; Scheme/R7RS guaranteed by language requirement; Koka yes via `kk__tailcall` label loops; wasm engines guaranteed via wasm 3.0 `return_call` (V8 since Chrome 112); Ruby compile-flag optional, off by default, YJIT/ZJIT never; JS mandated by ES6 but V8 never shipped it; CPython none. The pattern is clean: every system that guarantees TCO owns its calling convention; every system that hoped its JIT would do it later still has none in 2026.

**Recommendation (H2): (a)** — spec-level self-and-mutual guarantee implemented in tier1 first (a pointer shuffle there), with the slot-overlay ABI locked at frame-layout time and the TAILCALL/RET stencil pair designed against the two-word callee entry descriptor.

## 4. H4 — Memory runtime mechanics

Layout baseline shared by every option (research/03 §b.4; docs/en/design/01 B8a; docs/en/design/02 C4): heap values carry a fixed 8-byte header `{ u32 type_id; u32 rc }` (16-byte minimum total object, Inko's single-allocation string economics); ADTs, unions, and Result-like values are unboxed tag+payload pairs in registers; strings are immutable UTF-8 behind one raw pointer + length (docs/en/design/01 B8a), with slices as (ptr,len) slot pairs; and — the structural fact every option leans on — a value is heap-allocated iff it is moved into a boxed/shared context, because linearity makes escape a compile-time fact rather than a heuristic (research/03 §a.7). Division of labor: docs/en/design/02 C4 decides the memory-management strategy (it recommends Perceus RC + reuse operationalized with arenas); H4 here decides the runtime mechanics beneath that strategy — the allocator paths, pause behavior, cycle policy, and what each JIT tier must interoperate with.

### H4.a — Perceus precise non-atomic RC + block reuse + per-frame bump arenas for non-escaping temporaries

Two mechanisms. (1) Per-frame bump arenas for interpreter temporaries: allocation is a pointer add, reclamation is a restore on frame pop; sound precisely because linearity forbids silent escapes — anything leaving the frame is moved, and the last-use analysis that proves the move is the same one the checker already runs (research/03 §a.8, §b.6 arena bullet). (2) Perceus-style precise RC with block reuse for genuinely boxed values: `rc_inc` at each explicit clone, `rc_dec` at each value's unique death point (syntactically visible under affine checking, docs/en/design/02 C1.b), each drop threaded to the next allocation as a reuse candidate — Koka's `kk_block_alloc_at(reuse, ...)` falls back to mimalloc only when no candidate exists (research/03 §b.6, `kklib.h`; the reuse pass is `ParcReuse.hs`, which enforces dups-before-drops ordering that linearity hands us for free).

```
let x = pair(1, f(y))            // pair is boxed; f consumes y linearly
tmp2 = f(y)                      // y moved: no rc_dec (moved, not dropped)
rc_dec(tmp2)                     // tmp2 dies after field extraction -> reuse candidate
reuse = (rc(tmp2) == 1) ? tmp2 : NULL
x = alloc_at(reuse, sizeof(pair))// reuses tmp2's block when it died uniquely
x.tag = TAG_PAIR; x.a = 1; x.b = extracted
```

Arena mechanics (the interpreter half):

```
fn handle(req: Request) -> Resp | Err {
  mark = frame_arena.save()              // one add at entry
  buf  = frame_arena.alloc(hdr.len)      // bump; segment grows by chunk if needed
  ...                                  // non-escaping temps live here / in registers
  resp = Resp(...).move_out()            // leaving value is copied to RC heap or in registers
  frame_arena.restore(mark)              // one store on every pop path
  return resp
}
```

Prior art: Perceus, Reinking et al., PLDI 2021 (https://www.microsoft.com/en-us/research/publication/perceus-garbage-free-reference-counting-with-reuse/); Koka's `Parc.hs`/`ParcReuse.hs`/`kklib.h`; research/03 §a.8, §b.6; docs/en/design/02 C4 recommends exactly this ("Perceus RC + reuse, operationalized with (d)'s arenas") — H4.a is the runtime mechanics that recommendation names. **Cycle handling.** Linear values cannot participate in cycles; cycles require shared (Copyable) mutable cells or lazy knots, so v0 rejects them at the type level (module-level recursion handled specially), with the H4.d collector interface as the pre-planned escape: trial deletion over registered shared-cell candidates, plus weak refs only for the closure-environment cycle (a closure captured by a mutable cell it also captures). **Pause behavior.** Zero pauses by construction; no write barriers (immutable by default removes pointer stores), no safepoints, no GC maps in JIT frames (research/03 §a.8). **Allocator traffic, stated honestly.** research/03's claim is deliberately narrow — 100% of *non-moved temporaries* are register/arena-resident because escape is a typing fact; it is not a promise that all allocations are free. The honest comparators: Inko needed whole-program interprocedural EA plus inlining to promote 47-58% of heap allocations (2-2.2x on HTTP parsing — heuristic, partial, whole-program; `inko/compiler/src/mir/escape.rs`), and Lobster's ownership analysis elides ~95% of runtime RC operations with three rare compile errors (research/01 §B.9). Our boxed/shared residue keeps paying RC, and if the reuse pass underperforms, allocation falls back to mimalloc at ~15-25 cycles (research/03 §b.12 risk 3) — budget the reuse pass as optimizer work, not checker work.

| Axis | Assessment |
|---|---|
| Perf ceiling | Highest allocation profile of the four: zero-alloc temps, near-free boxed allocs, no barriers, no safepoints (research/03 §c Table 5 "best fit") |
| Implementation complexity | Low-medium: dup/drop placement degenerates to syntactic last-use insertion + a reuse-threading pass; no borrow or interference analysis (research/03 §b.6) |
| Startup | Best: no collector to initialize, no heap to pre-map |
| Portability | Non-atomic RC is sound while values are thread-confined; docs/en/design/03 E3b fork-join transfers ownership at fork/join, so the linear layer needs no atomics — boxed shared data crossing threads needs atomic rc or thread-confined heaps, an E3 interplay flagged for docs/en/design/03, not decided here |

Forecloses: nothing fundamental; the type-level acyclicity stance must be honest API surface (shared graphs need the explicit collector or arena API).

### H4.b — Generational moving GC tuned for immutable data

Young-gen collection is cheap for immutable data: objects die young and, being immutable, never store pointers after promotion, so remembered sets and write barriers — the machinery that usually sinks generational GCs — mostly vanish under immutable-by-default (research/03 §b.6 generational bullet).

```
alloc: bump in young gen (nursery)          | minor GC: copy survivors, promote
mutate: only in D1/D2 exclusion windows      | barrier on windowed pointer stores only
JIT: safepoints + precise stack maps per tier | roots = typed frame slots + headers
```

Prior art: Ocaml/Haskell/V8 young gen (research/03 §b.6); V8's Orinoco machinery scale (https://v8.dev/blog/trash-talk). The honest post-mortem the task requires: Inko shipped a parallel immix moving GC, published its removal ("Friendship ended with the garbage collector", https://yorickpeterse.com/articles/friendship-ended-with-the-garbage-collector), and then removed GC *entirely* in 0.10.0 in favor of ownership + ARC (`inko/rt/src/arc_without_weak.rs`) — a decade-scale signal that for an ownership-flavored language the collector was complexity without payoff (research/03 §a.8, §b.6 Immix bullet). **Cycle handling.** Free by construction (tracing). **Pause behavior.** Small, tunable, but present — the only option on the menu with pauses. **Allocator traffic.** Highest of the four that keep sharing: copying plus barriers on the mutation windows (docs/en/design/02 D1/D2). Costs: safepoints and precise stack maps in every JIT tier, root scanning of typed frames, and a concrete conflict with docs/en/design/01 B8a — moving objects invalidates the one-raw-pointer string representation, forcing pinning or handle indirection on every string access. Forecloses: docs/en/design/02 C5.c deterministic linear RAII deinit (drops become finalizer-time, the exact gap the linear resource layer exists to close); the zero-pause claim. **Verdict — no.**

### H4.c — malloc/free per object with no GC (pure linear ownership)

Every allocation is paired with a free at its syntactic death; no sharing exists, so no RC is needed.

```
let xs = List.make(1_000_000)     // owned
let ys = xs.pop()                 // ??? the tail: owned by xs (dead) and ys (alive)
// pure ownership forces a deep copy of the tail, or forbids pop-and-keep entirely
```

Where it breaks, concretely. (1) Shared closures: an escaping closure's environment must outlive the creating frame while the captured variables may also be used after — unique ownership forces deep-copying captures or forbids escaping closures, and functional-style DreamScript makes closures ordinary (research/03 §b.6 RC-vs-others framing). (2) Persistent collections: structural sharing means two versions reference one tail node — freeing on drop of either head double-frees, so pure ownership forfeits the immutable-persistent half of the language the immutable-by-default charter (req 5) points at. (3) Exception/fiber stacks: captured continuations (docs/en/design/03 E1d handler runtime) and fiber stacks (E2b) are heap-resident frames referencing values owned by suspended frames — ownership of frame slots becomes dynamic, exactly the case stack discipline cannot express. (4) Cycles: leak silently or require manual pools. **Cycle handling.** None possible without a collector; unreachable cycles leak. **Pause behavior.** Zero pauses, but with the highest allocator traffic of the four (every object pays malloc/free — the CPython cost model minus boxing) and O(n) free of persistent structures on drop. **Perf target vs C.** Allocation-heavy functional code is the workload class this option is worst at, which for this language is the main workload class. **Startup.** Fine. Forecloses: closures-over-shared-state, persistent collections, effect continuations — i.e., the language. **Verdict — no as the global mechanism; viable only as the substrate under H4.a's arenas (which are malloc/free with bulk free).**

### H4.d — Hybrid: RC for shared/boxed + arenas for linear data + optional concurrent cycle collector deferred

The same two mechanisms as H4.a, stated as policy with an explicit growth path: arenas cover all linear data whose non-escape is proven (including longer-lived linear buffers via region-annotated APIs, not just per-frame temps), Perceus RC covers shared/boxed values, and a concurrent trial-deletion cycle collector over shared mutable cells is a pre-committed optional component — never on the hot path, never applicable to linear data, which cannot cycle.

```
// the committed interface, two hooks + one header bit:
block.header.mark_suspect()             // set when a shared mutable cell is born
runtime.register_cycle_candidate(block) // collector scans/trial-deletes off-thread
weak<T>                                 // weak refs for closure-environment cycles
```

Prior art: research/03 §b.6 "Cycles are expressible only through explicit mutable ref cells or lazy knots; either reject them at the type level ... or provide an explicit `unsafe_cycle`/arena API with trial deletion, keeping the VM free of a general collector"; the option's difference from H4.a is commitment timing only. **Cycle handling.** H4.a's type-level rejection plus this pre-planned collector interface for shared-heavy workloads (persistent maps with back-pointers, observer graphs). **Pause behavior.** Zero on the mutator (collector is concurrent and bounded by candidate count). **Allocator traffic.** Same as H4.a. Costs vs H4.a: two runtime hooks and a header bit committed now, and the risk of never deleting dead interface code if the collector is never needed. Forecloses: nothing that H4.a doesn't; it merely spends interface surface today for an upgrade path tomorrow. **Verdict — yes, as H4.a's declared growth path** (one design, two commitment levels; pick H4.a now and name this as its documented extension, per research/03 §a.8 "while staying compatible with adding a concurrent collector later").

### H4 comparison

| Axis | a Perceus + arenas | b generational GC | c malloc/free only | d hybrid + deferred collector |
|---|---|---|---|---|
| Pauses | zero | small, tunable, present | zero | zero on the mutator (collector concurrent) |
| Allocator traffic | lowest: zero-alloc temps, reuse makes boxed allocs near-free | high: copying plus windowed barriers | highest: malloc/free per object | same as (a) |
| Cycle handling | type-level rejection; collector interface as growth path | free by construction | impossible (leak) | pre-committed trial deletion over candidates |
| JIT interaction | no barriers, no safepoints, no GC maps in frames | safepoints + precise stack maps in every tier | none | none until the collector is enabled |
| Conflicts | acyclicity must be honest API surface | docs/en/design/02 C5.c deinit; docs/en/design/01 B8a raw-pointer strings need pinning | closures, persistent collections, continuations, fibers | two hooks + a header bit committed early |
| Evidence | Perceus PLDI 2021; Inko 47-58% EA as the heuristic contrast; Lobster ~95% RC elision (research/01 §B.9) | Inko removed immix, then removed GC entirely (0.10.0) | no surveyed production language at this scope | Koka's own roadmap shape |

**Recommendation (H4): (a)** — Perceus precise non-atomic RC + block reuse + per-frame bump arenas, with H4.d's two-hook collector interface (cycle-candidate registration, weak refs) documented as its growth path from day one so shared/cyclic workloads get an upgrade path that never redesigns the VM.

Interactions worth recording: H4.a's arenas are per-frame and bound to the frame's stack extent, so fiber switching (docs/en/design/03 E2b) needs no arena action — each fiber's frames carry their own marks, and fiber segment growth is a chunk-list append, not a realloc, so arena marks stay valid across it. TCO makes the memory interplay cheaper still: a tail call moving arguments transfers ownership with zero RC traffic (research/03 §b.5), which is why H2.a and H4.a are a paired design rather than two independent choices. And the one-raw-pointer string representation of docs/en/design/01 B8a is safe precisely because nothing in H4.a moves heap objects after allocation — under H4.b it needs pinning, under H4.c it leaks with unreachable cycles.

## 5. What static typing deletes from the JIT — the complexity budget freed

research/03's central claim for this language: the bulk of every surveyed JIT is runtime type/shape speculation, and static typing deletes nearly all of it unconditionally (research/03 §a.3, §b.3). This is the budget H1.a spends on stencils and SSA instead.

| Subsystem in dynamic VMs | Scale in its host | Status for DreamScript |
|---|---|---|
| Hidden classes / maps + transitions | V8's core object representation | deleted: field offsets are bytecode immediates (`FIELD_LOAD %dst, %obj, #12`), cf. LuaJIT `IRFLDEF` (research/03 §b.3) |
| Field-access inline caches + feedback vectors | per-site memory, tier-feeding infra | deleted entirely, memory reclaimed (research/03 §b.3 items 1, 5) |
| Type guards + deopt frames | LuaJIT `IRT_GUARD` + `SnapShot`s; Maglev map checks | deleted on the data path; survivors are the stack-limit compare and (under B7a trapping) one cold overflow edge (research/03 §b.2 deopt table) |
| Speculation recording | `lj_record.c` + `lj_opt_*.c` inferring types the front end already proves | deleted: types are compile-time constants (research/03 §b.3) |
| Side traces + exit counters + trace linking | `lj_jit.h` `SnapEntry.count` side-trace promotion | deleted: single-entry units, no cliffs (Cloudflare's documented LuaJIT inconsistency is structurally unreachable) |
| Feedback-driven tiering budgets | V8 leaptiering, CPython `_SPECIALIZE_*` | reduced to plain hotness counters (BL-4/H1.a) |

The deopt side of the same ledger, per system (research/03 §b.2 deopt table):

| System | Guard form | Exit cost | Exit mechanism |
|---|---|---|---|
| LuaJIT | IR ops flagged `IRT_GUARD` (type/range/equality) | snapshot walk, restore nslots from `SnapEntry` map | exit to interpreter or side trace at bytecode PC |
| V8 Maglev/TurboFan | map checks, Smi checks, bounds | build deopt frame from translation state | re-enter interpreter, maybe re-optimize |
| CPython tier 2/3 | `_CHECK_*` uops, compactness checks | side exit to tier 1 executor | global side-exit list per trace |
| Truffle | node assumptions | `transferToInterpreter()`, AST rewrite | re-execute AST, recompile |
| DreamScript | none on the data path; stack-limit compare per function entry | n/a (no guards) | debugger-driven frame materialization only |

Quantified two ways. Runtime: the per-operation savings compound to the 3-10x tier-1 estimate — i64 add ~3-6x fewer uops than the ADDVV tag path, field access ~2-4x, static call ~2-3x, ADT construct ~5-20x (allocation), Result match ~2-3x, loop iteration 1.0x (dispatch is irreducible) (research/03 §b.11 table). Engineering: research/03 estimates that collapsing the guard/deopt layer reduces tiering runtime cost and engineering risk by roughly 70% (research/03 §b.2 deopt paragraph) — the freed budget is what pays for the stencil pipeline, the SSA lowering, and debugger frame materialization. What survives in IC form (all perf-hint-only, no correctness role): 1-2 entry type-id-keyed ICs at dyn/trait callsites, hot-reload module-global caches keyed on a module version counter, and per-frame effect-handler operation tables (research/03 §b.3 "Where IC-style mechanisms remain useful"). Net accounting: a dynamic VM's JIT carries roughly six speculation subsystems that DreamScript replaces with two artifacts — a stencil patcher and a lowering pass — and that ratio, not any single benchmark, is the complexity-budget claim of this section.

## 6. Value-representation consequences (owned by docs/en/design/01 — referenced, not re-decided)

These are downstream obligations the opcode set and stencil set inherit from the type catalog's recommendations; the decisions live in docs/en/design/01. None of the bullets below reopens a docs/en/design/01 decision; each states only the opcode-, slot-, or stencil-level obligation the cataloged recommendation creates for this file's tiers.

- **B7 numerics (docs/en/design/01 B7, recommendation (a): Int/Float defaults with trapping overflow).** Slots are unboxed i64/f64 at fixed 8-byte width; arithmetic opcodes are representation-specific (ADDI64/ADDF64) and no slot is ever dual-typed — the DUALNUM machinery of dynamic VMs does not exist here (research/03 §c Table 4). Trapping overflow adds a cold overflow edge to each arithmetic uop/stencil (`add; jo -> panic stub`): one flagged branch on the happy path, consistent with research/03 §b.2's ADDOV guidance (overflow as panic, zero checks on the predicted path, explicit checked ops as library functions). Arbitrary-precision fallback (B7's later library type) rides the same edge into the panic path, never into tier1 opcodes.
- **B4c nullability (`T?` as zero-semantic alias of `T | none`).** `none` occupies a tag slot with zero payload slots; a `T?` of multi-slot T spreads tag+payload across consecutive frame slots exactly like any union (research/03 §b.4 ADT layout example). Null checks compile to TAGTEST+JMPIF, fusable into one superinstruction (BL-4); the boxed-Option allocation term (~5-20x, research/03 §b.11 row 5) never appears on either side of the check.
- **B9a equality (universal structural `==` with carve-outs).** Structural equality lowers per static operand type: EQI64 for integers, EQF64 for floats (pinning NaN != NaN and ±0.0 semantics at the opcode level), reference-identity-then-walk for boxed aggregates. The float-split consequence: `==` over a union mixing Int and Float members compiles to a tag-switch over the split opcodes — one operator, N static lowerings, never one dynamic opcode. This is the opcode-set obligation docs/en/design/01 B9a creates here, and the reason its five carve-outs matter for cross-representation equality.
- **B8a strings (immutable UTF-8, one raw pointer).** Slices are (ptr,len) slot pairs; EQSTR is pointer-identity fast path then memcmp; interning only for short hot identifiers (research/03 §b.4 strings). A moving GC (H4.b) would need pinning or handles for that raw pointer — one more reason the non-moving RC/arena substrate of H4.a fits docs/en/design/01.
- **Generic dictionaries (semantics owned by docs/en/design/05-generics-metaprogramming.md, referenced by path only).** research/03 §b.8's stance: no language-level specialization; dictionary passing with unboxed dictionaries — a single-method-trait dictionary is a code pointer in a register, and numeric-representation-polymorphic operations get per-representation code that is exactly the tier2 stencil set — so the value representation deletes the classic boxing penalty of dictionary passing for primitives without adding a new opcode class.
- **B3 products (docs/en/design/01 B3, recommendation: nominal structs/enums + anonymous tuples).** Struct fields are consecutive typed slots; field access is an immediate-offset load with no check, the `FIELD_LOAD %dst, %obj, #12` form of research/03 §b.3; tuple construction and deconstruction are register writes and never allocate — the ~5-20x ADT-construct saving of research/03 §b.11 applies to every product type, not just enums.

## 7. Coherent combos

Three named packages over the 3x3 H1xH2xH4 space. Combos not listed are dominated: H1c violates BL-7 and charter req 1; H2c/H2d violate charter req 3 as mechanisms; H4c forfeits the language's core workloads. Reading guide per combo: text pipeline diagram, perf-target table with evidence anchors, the v0 implementation sequence, and implications for docs/en/design/02 (linearity, deinit), docs/en/design/03 (errors, effects, fibers), and docs/en/design/06 (modules, tooling); a combo is coherent only where no member forecloses another's baseline.

### Combo A — "Tiered-interpreter-first": H1a + H2a + H4a (recommended)

```
run foo.ds
  -> compile-as-query (docs/en/design/06 H6) -> typed uop bytecode
  -> dsmod cache hit: one mmap, no relocation (sub-10ms cold target, docs/en/design/06 G3.5)
  -> tier1: computed-goto interpreter | unboxed slots | CALLT frame-slide TCO
  |         Perceus RC+reuse for boxed | per-frame arena for temps | zero pauses
  |         back-edge counter -> tier2 copy-and-patch (guard-free stencils, us/unit)
  |         profile -> tier3: typed SSA -> Cranelift CLIF | or wasm 3.0 (deployment)
  -> debugger: materialize frames from metadata words; CIST_TAIL backtrace chains
```

| Workload / phase | Perf vs C | Evidence anchor |
|---|---|---|
| tier1, scalar/numeric | 3-10x | research/03 §b.10 |
| tier1, ADT/Result-heavy functional | 3-10x (zero-alloc temps) | research/03 §b.11 rows 5-6 |
| tier1, call-heavy functional | dispatch is the only irreducible cost; lean prologues + superinstructions | research/03 §b.12 risk 1, §b.9 call-overhead bullet |
| tier2 (copy-and-patch) | 1.5-3x | arXiv:2011.13127 (~14% better than -O0 at 100x speed) |
| tier3 (Cranelift) / wasm deployment | 1-2x / 1.5-2.5x | research/03 §c Table 1, IMC 2019 |
| Dynamic baselines for contrast | Lua 20-50x, CPython 50-100x, PyPy 5-15x after seconds of warmup | research/03 §b.10 |
| Worst case | tier1 floor everywhere; no cliffs | research/03 §b.2 deopt table |

Sequencing for a v0 (what to build, in order): (1) dsmod format + mmap loader + typed bytecode emitter, locking the frame layout and the slot-overlay call ABI (BL-6/BL-7, H2.a's constraint) — everything downstream compiles against this; (2) tier1 interpreter: computed-goto uop loop, unboxed slots, ADT tag+payload, CALLT frame-slide TCO — the 3-10x core, roughly one strong engineer-year (research/03 §d Pipeline A); (3) Perceus dup/drop placement + reuse threading + per-frame arenas; (4) fibers + nursery (docs/en/design/03 E2b) as heap stack segments; (5) profile-feedback superinstructions, reserving J-variant opcode space from day one; (6) tier2 stencil pipeline (LLVM at build time; x86-64, then aarch64; tier1-only elsewhere, CPython's opt-in posture); (7) the bytecode-to-SSA lowering — built early, exercised by tier3/wasm when demanded (research/03 §d sequencing). Implications: docs/en/design/02 — C1.b affine core + C4.a Perceus-with-arenas is exactly this substrate, and C5.c deinit hooks run at rc_dec/reuse points while arena reset is invisible to them; docs/en/design/03 — E2b fibers get O(1) switches on segments that TCO keeps at initial size, E1d handler entry is the one frame-materialization path, E4b union errors ride unboxed tag+payload; docs/en/design/06 — G3.b/G3.5 dsmod is the tier-transition substrate (in-place J-variant patching is the same mechanism as hot-reload invalidation), H6's store caches dsmod per content hash, `ds build --exe` embeds the dependency closure; docs/en/design/05 (by path only) — derive-generated Ord/Hash/Show bodies compile to the same uops and stencils, so derived instances cost nothing extra to tier.

### Combo B — "Wasm-first": H1d + H2a + H4d

```
release: foo.ds -> typed SSA -> wasm 3.0 module (return_call = TCO) -> engine
dev:     foo.ds -> dsmod -> tier1 interpreter (instant edit-run, TCO in-VM)
memory:  arenas for linear data | RC for boxed | collector interface pre-committed
```

| Path | Perf vs C | Evidence anchor |
|---|---|---|
| wasm 3.0 AOT | 1.5-2.5x | IMC 2019 ~1.55x geomean (research/03 §b.7) |
| MoonBit-tuned same pipeline | near-C on benchmarks | 25x vs JS iterators, ~8x vs Json5 (moonbitlang.com/blog/js-support) |
| interpreter fallback | 3-10x | research/03 §d Pipeline A |
| warmup | engine-tier dependent | no owned tiering |

Sequencing: (1) typed SSA lowering + wasm emitter first (the release path is the product); (2) tier1 interpreter as dev substrate; (3) TCO via `return_call` in wasm plus in-VM frame reuse for the interpreter — two mechanisms, one guarantee, verified by a shared test suite; (4) H4.d interface hooks before persistent collections ship. Implications: docs/en/design/02 — C1.b/C4.a unchanged (RC lives inside wasm linear memory); docs/en/design/03 — E2b fibers are the expensive part, lowering to wasm threads/JSPI or JS async instead of interpreter segments (the combo's main hidden cost); docs/en/design/06 — G3's multi-backend economics is the MoonBit model verbatim, and H6 artifacts become wasm components (H5.d); docs/en/design/01 — untouched at the type level, but the trapping-overflow edge of B7a rides wasm's own trapping semantics, so the panic stub is host-engine-mediated rather than a stencil.

### Combo C — "Method-JIT-simple": H1b + H2b + H4b

```
run foo.ds -> dsmod (mmap) -> tier1 interpreter (3-10x)
  hot function counter -> tier2 method JIT: SSA -> CLIF (1-2x, ms/function)
  memory: generational GC | safepoints + stack maps in JIT frames | pauses tunable
  TCO: best-effort — guaranteed only where the compiler rewrites (Ruby posture)
```

| Phase | Perf vs C | Note |
|---|---|---|
| pre-compile | 3-10x | tier1 floor |
| post-compile | 1-2x | Cranelift quality |
| compile cost | ms per function | no us-scale middle band |
| worst case | GC pause + unbounded recursion | the only combo with both |

Honest flag, stated plainly: this combo gives up two guarantees the rest of the design exists to provide — charter req 3's TCO guarantee (H2.b) and docs/en/design/02 C5.c deterministic deinit (H4.b) — and inherits GC safepoints, precise stack maps, and write barriers in JIT frames (V8 Orinoco-scale machinery, plus a pinning story for docs/en/design/01 B8a raw-pointer strings). Its one honest virtue is dependency minimalism: no stencil CI matrix, one compiler, one GC. A cheaper variant worth noting is H1b + H2a + H4b, which keeps the TCO guarantee (it is nearly free in tier1) and pays only the GC. Sequencing if chosen: (1) dsmod + tier1 as in Combo A steps 1-2; (2) GC with stack maps before any JIT lands; (3) method JIT with Sparkplug's frame-preservation rule; (4) OSR or accept the loss on mid-function hot loops. Implications: docs/en/design/02 C5.c collapses (finalizers, not deinit); docs/en/design/03 E1d handler capture needs GC-safe frames; docs/en/design/06's two-version hot reload must GC-scan both module versions; docs/en/design/01 B8a's one-raw-pointer strings need a pinning or handle story, the combo's least-advertised cost.

## Closing note

Pending author decisions per AGENTS.md phase 3: H1 (recommend (a)), H2 (recommend (a)), H4 (recommend (a), with H4.d's collector interface as its declared growth path); record the outcomes in `docs/en/design/00-decision-map.md` and treat BL-1 through BL-7 as already-fixed inputs in every later catalog. If a non-recommended option is picked on any point, the section 7 combo diagrams and sequencing plans must be re-derived before implementation begins — they assume their named packages, and a substituted member changes what v0 must build first. Two decisions above all others are load-bearing for every tier: the slot-overlay calling ABI (H2.a) and the fixed-width position-independent dsmod (BL-7, docs/en/design/06 G3.5) — both are named by research/03 §d as the things to lock early because everything above them depends on them.
