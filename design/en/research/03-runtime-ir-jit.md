# 03 — Runtime, IR, and JIT: execution strategy research for DreamScript

Date: 2026-11-04. Scope: execution strategy, IR design, JIT, and interpreter-level performance for an interpreted, statically typed, linear/affine, immutable-by-default language with guaranteed tail-call optimization and ultra-high performance goals. Companion notes: DreamScript requirements — interpreted (source -> internal IR/bytecode expected), static linear/affine types, immutable by default, everything a compiler does plus interpreter-specific optimizations, guaranteed TCO, optional JIT; author taste: no heavyweight sugar, no C++-style specialization semantics (see philosophy_of_PL.md, sections "特化" (specialization) and "错误处理" (error handling): set-theoretic types, Result-style errors, interest in algebraic effects/Koka).

Primary sources are local clones (all under `Z:/Temp/plr3/`, untrusted, read-only): `luajit/src/lj_bc.h`, `lj_ir.h`, `lj_obj.h`, `lj_frame.h`, `lj_jit.h`, `lj_dispatch.h`; `lua/ldo.c`, `lvm.c`, `lopcodes.h`, `ljumptab.h`; `cpython/Python/bytecodes.c`, `ceval.c`, `ceval_macros.h`, `Tools/jit/template.c`, `Tools/jit/README.md`; `inko/rt/src/arc_without_weak.rs`, `inko/compiler/src/mir/escape.rs`, `inko/compiler/src/mir/inline.rs`; `koka/src/Backend/C/FromCore.hs`, `koka/src/Backend/C/Parc.hs`, `koka/src/Backend/C/ParcReuse.hs`, `koka/kklib/include/kklib.h`; `wasmtime/cranelift/docs/ir.md`, `cranelift/codegen/src/opts/README.md` and `*.isle`, `cranelift/codegen/src/egraph/`. Repo paths cited below are relative to those roots.

## (a) Executive summary

1. Bytecode beats tree-walking decisively, and register bytecode beats stack bytecode: Lua 5.5 and LuaJIT use 32-bit register-based instructions (`lua/lopcodes.h`: 7-bit opcode, 8-bit A, 8-bit B/C, 17-bit Bx; `luajit/src/lj_bc.h`: ABC and AD formats, decode is pure shifts); CPython's stack machine executes many more instructions per operation and needed heroic specialization work (PEP 659) to improve only ~1.25x.
2. LuaJIT already demonstrates the "statically typed interpreter" trick in miniature: its bytecode has type-specialized opcodes (ADDVN/ADDNV/ADDVV, ISEQV/ISEQS/ISEQN/ISEQP in `lj_bc.h`) because its compiler infers operand types. DreamScript gets this for free and unconditionally from its static type system, and can additionally keep every slot unboxed, because the type of each register is a compile-time constant rather than a runtime guess.
3. The bulk of every surveyed JIT's machinery is runtime type/shape speculation: LuaJIT guards (`IRT_GUARD` in `lj_ir.h`), snapshots (`SnapShot`/`SnapEntry` in `lj_jit.h`), and side traces; Maglev map checks and deopt frames; CPython `_SPECIALIZE_*` + guard uops; Truffle `transferToInterpreter`. A statically typed language eliminates nearly all of it: no hidden classes, no inline caches for field access (offsets are known — cf. LuaJIT `IRFLDEF` field loads by `offsetof`), no type guards; deopt reduces to debugger, stack-overflow, and effect-entry concerns. This is the single largest multiplier for DreamScript's design.
4. Tiering: the best-fitting pipeline is a typed register VM (tier 1) plus a copy-and-patch JIT (tier 2). Copy-and-Patch (Xu & Kjolstad, arXiv:2011.13127) yields code ~14% faster than LLVM -O0 while compiling two orders of magnitude faster than LLVM; CPython 3.13+ ships it (PEP 744, `Tools/jit/template.c`). With static types the stencils need no runtime check-and-bail — holes are only static constants, addresses, and layout metadata.
5. Dispatch: computed goto (Lua `ljumptab.h`, CPython `USE_COMPUTED_GOTOS`) or C tail-call threading (CPython main's `_Py_TAIL_CALL_INTERP` with `musttail` + `preserve_none`, `Python/ceval_macros.h`). Both are validated in production; superinstructions should be composed on profile data (CPython `macro(BINARY_OP)` expansion in `bytecodes.c` is the pattern to copy).
6. Value representation: with static typing we do not need NaN-boxing at all (LuaJIT GC64 format in `lj_obj.h`: 13-bit NaN prefix, 4-bit tag at bit 47, 47-bit pointer; type check is one shift `it64 >> 47`). Typed registers hold unboxed i32/i64/f64/bool directly; ADTs and Result are unboxed tag+payload pairs spread across registers; heap allocation happens only for boxed/shared values. Dynamic VMs cannot do this; statically typed traces inside LuaJIT already use exactly this representation (`IRT_INT/I64/NUM` in `lj_ir.h`) — we would interpret permanently what LuaJIT only achieves inside traces.
7. Linear types make escape analysis a compile-time fact rather than a heuristic: Inko needed whole-program interprocedural EA plus inlining to promote 47-58% of heap allocations to the stack (`inko/compiler/src/mir/escape.rs`; Inko 0.20.0 release notes, with 2-2.2x speedups on HTTP parsing); DreamScript gets 100% of non-moved temporaries for free, including scalar replacement of ADTs in the interpreter frame.
8. Memory management: Perceus-style precise reference counting with block reuse (Koka; `kklib.h`: `kk_block_alloc_at(reuse, ...)`, `kk_block_check_drop_reuse`) is the best match for linear values; RC traffic shrinks to explicit clone/move boundaries because linear use-once semantics makes most dup/drop statically elidable. Combine with per-frame bump arenas for interpreter temporaries (reset on frame pop — safe precisely because linearity forbids silent escapes). Avoid a general tracing GC initially; Inko's own history (parallel immix, then removed entirely in 0.10.0 in favor of ownership+ARC, `inko/rt/src/arc_without_weak.rs`) is instructive.
9. Guaranteed TCO is cheap and exact in a register VM: Lua's `luaD_pretailcall` (`lua/ldo.c`) reuses the CallInfo and moves callee+args down the stack; Koka's C backend lowers tail calls to a label loop (`goto kk__tailcall;` in `FromCore.hs`); Wasm 3.0 standardized `return_call` (Sept 2025) so a wasm backend inherits TCO from the platform. Ruby YJIT/ZJIT still do not guarantee TCO in 2025-2026 — a real differentiator for DreamScript if we guarantee it in the spec from day one.
10. IR: a typed SSA core IR (Cranelift-style, with ISLE-like directional rewrite rules and an e-graph with cost-based extraction, `cranelift/codegen/src/opts/README.md`, `codegen/src/egraph/`) serves the optimizing tier and a wasm/native backend (MoonBit pipeline: typed core IR -> wasm/JS/native, compiler-level iterator fusion, tail-recursion-to-loop).
11. Performance ceiling evidence: LuaJIT lands ~2-3x from C on numeric loops (independent measurement; Cloudflare documents excellent trace-stable speed with cliff behavior on exits); plain Lua ~25x slower than LuaJIT; PyPy needs seconds of warmup and can be 2x slower than CPython off the happy path; copy-and-patch ≈ LLVM -O0 quality; wasm ≈ 1.55x native (Chrome, IMC 2019). Realistic DreamScript target: 3-10x of C for the pure interpreter, 1.5-3x with copy-and-patch, 1-2x with an optimizing tier — and none of the deopt cliffs dynamic VMs suffer.
12. Recommended pipeline (details in section d): typed register bytecode VM with unboxed registers + guaranteed TCO + Perceus RC-with-reuse + per-frame arenas as tier 1; copy-and-patch JIT keyed on the same uops as tier 2; Cranelift-backed typed-SSA optimizing tier or wasm AOT as tier 3/deployment. All tiers share one bytecode -> SSA lowering, so the complexity is amortized across three artifacts.

## (b) Deep dives

### 1. VM architectures: tree-walking vs bytecode, stack vs register, dispatch, formats

Tree-walking interpreters (early CRuby, CPython's `ast.eval`, pre-bytecode Dart) re-pay parsing and re-dispatch on every evaluation, offer no stable unit to cache or instrument, and give a JIT nothing to compile. Every high-performance interpreter in this survey executes bytecode. Bytecode also creates the durable artifact that enables: bytecode-file caching and mmap startup, instrumentation hooks, debugging, and tiering.

Stack vs register bytecode. Stack machines encode densely but execute more instructions (every operand is pushed/popped); register machines name virtual registers in operands and execute fewer instructions at slightly larger code size. Lua's format (`lua/lopcodes.h`): 32-bit instructions with `SIZE_OP=7`, `SIZE_A=8`, `SIZE_B/C=8`, `SIZE_k=1`, `SIZE_Bx=17`, `SIZE_sJ=25`; the main loop in `lvm.c` computes `RA(i) = base + GETARG_A(i)` once per frame (`base` is a local, so slot access is a single indexed load). LuaJIT (`luajit/src/lj_bc.h`) uses the same 32-bit fixed layout with two formats: `|B|C|A|OP|` (ABC) and `|D|A|OP|` (AD, 16-bit D); decode macros are `bc_op(i) = i & 0xff`, `bc_a(i) = (i>>8)&0xff`, etc. Two format-level tricks worth copying: jump offsets are biased (`BCBIAS_J=0x8000`) so a signed jump is an unsigned compare-free subtraction, and the opcode table's static assertions pair negations (`BC_ISEQV^1 == BC_ISNEV`, `BC_ISLT^3 == BC_ISGT` in the enum) so the JIT/interpreter can negate a comparison with an XOR — micro-details that add up in dispatch-heavy code.

Typed opcodes in bytecode. LuaJIT's `BCDEF` table in `lj_bc.h` is the key precedent: arithmetic is emitted as ADDVN/ADDNV/ADDVV (value-number, number-value, value-value), comparisons as ISEQV/ISEQS/ISEQN/ISEQP (variable/string/number/primitive), constant loads as KSTR/KNUM/KSHORT/KPRI, table access as TGETS/TGETB/TGETV, function prologues as FUNCF (fixed arity)/FUNCV (vararg)/FUNCC (C function). Each opcode's operand modes are encoded in a parallel table (`lj_bc_mode[]`: A/B/C modes + metamethod index per opcode), which the recorder and interpreter consult with shifts. The VM opcodes carry the types the compiler inferred. A statically typed DreamScript can go strictly further: every operand's type is known with certainty at bytecode-emit time, so opcodes specialize by representation (i32 vs i64 vs f64) with no fallback path, and the "V" (dynamic value) opcode variants simply do not exist.

Dispatch mechanisms, with source evidence:

- Switch: the portable baseline; competitive on modern CPUs when opcode frequency is skewed (branch predictors learn), still pays a bounds-check and table-load.
- Computed goto / threaded dispatch: Lua's `lvm.c` includes `ljumptab.h` under `LUA_USE_JUMPTABLE` — `vmdispatch(o)` expands to `goto *vmestimate[o]` style indirect threading; `vmfetch()` is a two-instruction fetch (`i = *(pc++)` plus a trap check). CPython gates the same on `USE_COMPUTED_GOTOS` in `Python/ceval.c` (the non-computed-goto build is a fallback: `#if USE_COMPUTED_GOTOS && !_Py_TAIL_CALL_INTERP`).
- Tail-call threading: CPython main contains a full tail-call interpreter: `_Py_TAIL_CALL_INTERP` (`Python/ceval_macros.h`) declares `typedef PyObject *(Py_PRESERVE_NONE_CC *py_tail_call_funcptr)(TAIL_CALL_PARAMS)` where handlers carry `(frame, stack_pointer, tstate, next_instr, instruction_funcptr_table, oparg)` and end with `return _TAIL_CALL_<op>(...)`; `Py_MUSTTAIL` is `__attribute__((musttail))` (clang/GCC) or `[[msvc::musttail]]` (MSVC 2026+), and the calling convention is `__attribute__((preserve_none))`. Benefit: every instruction handler is an independent function, so the C compiler register-allocates handler locals independently (CPython's shared-state switch loop suffers register pressure); dispatch is a single indirect call. Cost: hard toolchain requirement and unusual debug profiles.
- Trace-stitched dispatch (LuaJIT): the opcode set includes JFORL/JLOOP/JITERL/JFUNCF variants (`lj_bc.h` comment "Loops and branches. I/J = interp/JIT") that appear only inside JIT-compiled traces; the plain interpreter's dispatch lands directly in machine code when it fetches one, making tier transitions at loop back-edges free. This is an elegant tiering hook worth copying for any tiering scheme.

Superinstructions and macro-ops. CPython's `bytecodes.c` DSL composes them statically, e.g. `macro(BINARY_OP) = _SPECIALIZE_BINARY_OP + _RECORD_TOS_TYPE + _RECORD_NOS_TYPE + unused/4 + _BINARY_OP + POP_TOP + POP_TOP;` — one fetched instruction expands into a whole micro-op sequence; the cases generator emits `Python/generated_cases.c.h` (~13.8k lines) for tier 1 and `Python/executor_cases.c.h` for tier 2. The DSL also marks semantics the optimizer needs: `pure op(...)` annotations, `tier2 op(...)` uops with explicit stack effects like `_BINARY_OP_ADD_INT, (left, right -- res, l, r)`, and `replicate(2:4)` for unrolled variants (see `SWAP`). LuaJIT folds common sequences into single opcodes (ISNEXT iterator setup, KSHORT feeding typed arithmetic, RETM multi-returns). Recommendation for DreamScript: define the instruction set as micro-ops with explicit effects from day one (CPython discipline), then compose superinstructions in the assembler; the same uops become the copy-and-patch stencil set one-to-one.

Bytecode format design (LuaJIT vs Wasm). LuaJIT's in-memory bytecode is fixed 32-bit, host-endian, with operands being register indices and constant-table indices — position-independent, mmap-able as-is, with a portable serialized dump (`lj_bcdump.h`). Wasm uses LEB128 variable-length integers, a typed value stack, and sectioned modules — better compression, worse random access, needs decoding before execution. For DreamScript: fixed 32-bit instructions, index-based operands, module header with version/type/constant tables, and an IR-version checksum. This gives cheap `load` (one mmap, no relocation), supports bytecode-file caching (`dreamscript run foo.dsbc`), and lets the copy-and-patch patcher jump directly into the middle of a function body.

Frame design comparison (what carries the call stack). Three designs dominate, and the choice determines TCO cost, deopt cost, and async cost:

- Separate call-info records (Lua 5.4/5.5): the data stack holds only values; each call pushes a `CallInfo` (function position, saved pc, top, status flags) into a separate array. `lvm.c` keeps `ci`, `base`, `pc` in C locals; `updatebase(ci)` re-syncs after stack reallocation (the `trap` flag in `vmfetch()` covers hooks and reallocation). Pros: cheap frame introspection and vararg handling; cons: two structures to maintain per call.
- Frames inline in the value stack (LuaJIT): `lj_frame.h` documents the layout — for LJ_FR2 (64-bit PC), `[func | PC/delta/ft] | slots...`, with frame type packed in the low bits of the size word (`FRAME_LUA`, `FRAME_C`, `FRAME_CONT`, `FRAME_PCALL`, ...); continuation frames for pcall-like constructs live adjacent. `frame_delta(f)` recovers the frame size by a shift, which is exactly what CALLT/CALLMT tail calls exploit to slide args down. Pros: minimal per-call work, JIT-friendly (the stack is already an array of machine-word values); cons: value stack must be pre-sized per function (LuaJIT emits frame-size metadata per proto).
- Interpreter frames as first-class objects (CPython `_PyInterpreterFrame`): each frame holds locals, stack pointer, instr_ptr, return_offset, owner; the tier-1 loop and the tier-2 executor and the JIT all manipulate the same struct, which is what makes CPython's tier transitions possible at all (handlers take `frame, stack_pointer` as parameters — see TAIL_CALL_PARAMS in `Python/ceval_macros.h`). Pros: uniform substrate for instrumentation; cons: heavier per-call overhead than Lua's.

For DreamScript: inline frames in a typed value stack (LuaJIT style) are the right substrate — the frame layout is statically known per function (each slot's type is in the code object), so a frame is just a contiguous span of typed slots plus a small metadata word (return address or index into a return table, frame size, flags for TCO chains). This single decision makes interpreter calls cheap, TCO a pointer shuffle, the copy-and-patch stencil ABI trivial (slots are machine-sized and typed), and debugger materialization a matter of walking the metadata words.

Concrete example. Consider a DreamScript-like function (illustrative, not real syntax):

```
fn sum_to(n: i64) -> i64 {
  let mut acc = 0i64
  while n > 0 { acc += n; n -= 1 }
  acc
}
```

A LuaJIT-style encoding (dynamic VM, tags in slots) vs a DreamScript typed encoding:

```
-- LuaJIT-ish (lj_bc.h formats), TValue slots, tags checked via metamethod-indexed ops:
; FUNCF  r0            ; prologue: fixed arity
; KSHORT r1, 0         ; acc = 0 (tagged int)
; ISGE   r0, r1        ; n > 0 ? (guards exit -> side trace)
; ADDVV  r1, r1, r0    ; acc += n (tag check at runtime)
; SUBVV  r0, r0, K1    ; n -= 1
; FORL/JMP loop
; RET1  r1

-- DreamScript typed register bytecode (every slot's type static, no tags anywhere):
  0: FUNCF  frame=2           ; slots: r0=n:i64, r1=acc:i64
  1: LDIMM  r1, 0:i64         ; acc = 0 (unboxed immediate)
  2: LEI64  r2, r0, 0        ; r2 = (n <= 0), unboxed bool
  3: JMPIF  r2 -> 6           ; static branch, no guard
  4: ADDI64 r1, r1, r0        ; acc += n (pure machine add, no check)
  5: SUBI64 r0, r0, 1  ; JMP -> 2
  6: RET    r1                ; moves r1
```

The typed encoding executes 3 machine-level operations per iteration in the interpreter (compare, add, subtract) plus dispatch; the dynamic encoding additionally maintains and checks tags on every arithmetic op and comparison, which is exactly the overhead the static type system deletes.

### 2. Tiering and JIT strategies

V8 (Ignition -> Sparkplug -> Maglev -> TurboFan). Ignition is a register interpreter whose feedback vectors feed inline caches. Sparkplug is a baseline JIT that compiles bytecode 1:1 with no optimization but deliberately preserves interpreter frame layout so deopt and stack introspection stay trivial (v8.dev/blog/sparkplug: ~41% Speedometer improvement over Ignition alone). Maglev (v8.dev/blog/maglev) is an SSA mid-tier built on a CFG (not TurboFan's sea of nodes): a prepass (branch targets, loop assignments, liveness), a single-forward-pass SSA builder with pre-created loop phis, known-node-info tracking, representation selection, a simple forward-walk register allocator, and direct macro-assembler emission; it specializes via runtime feedback — e.g., emit a map check then a fixed-offset field load — registering dependencies for stable assumptions; compile speed is ~10x slower than Sparkplug but ~10x faster than TurboFan, with measured gains of roughly -3.5% JetStream and -10% Speedometer energy. TurboFan is the peak tier with full inlining and type inference from feedback. Leaptiering (v8.dev/blog/leaptiering) later decoupled per-function dispatch-table entries from JSFunctions to avoid invalidation storms. The lesson: three tiers plus leaptiering exist because dynamic types force speculation; each tier's added complexity is dominated by guard/deopt bookkeeping that DreamScript does not need.

PyPy meta-tracing (pypy.org/performance.html). The interpreter is written in RPython; the JIT observes hot *interpreter* loops and traces what the interpreter does (meta-tracing), so one JIT serves many interpreters. Documented characteristics: warmup "at least a couple of seconds"; scripts under ~0.2s and unit tests never benefit; `sys._getframe`, tracing hooks, and similar introspection disable the JIT over their scopes; runtime-support-heavy code (big ints, some sorts) can be up to ~2x slower than CPython. Meta-tracing is the cheapest route to a strong JIT for a dynamic language; for a static language it is the wrong trade — the interpreter is already type-directed, so tracing would spend most of its recording budget on tag bookkeeping that our tier-1 never does in the first place.

Truffle/Graal partial evaluation ("One VM to rule them all", Würthinger et al., Onward! 2013, dl.acm.org/doi/10.1145/2509578.2509581; accessible review at blog.acolyer.org/2014/11/18/one-vm-to-rule-them-all). Language ASTs are written in Java; Graal partially evaluates the interpreter against a hot guest method — constant-folding away interpreter dispatch, node types, and bookkeeping — producing specialized machine code "competitive with, and in many cases outperforms, the highest performance implementations". Speculation is guarded with `transferToInterpreter()` deopt; the AST rewrites itself (e.g., a polymorphic node becomes an inline cache) and recompiles. The architectural insight — partial evaluation erases interpreter overhead — is precisely what static typing gives DreamScript at the bytecode level without needing a Java-grade host VM: our bytecode opcodes are the "already partially evaluated" program representation.

LuaJIT trace compiler (source: `lj_record.c` recorder, `lj_fold.c` constant folding, `lj_asm.c` linear-scan register allocation, `lj_snap.c` snapshots, structures in `lj_jit.h`). Pipeline in detail: (1) hotness — per-bytecode-position counters in `hotcount[]` (`lj_dispatch.h`: 64 entries indexed by `(pc>>2)&63`, default trigger `HOTCOUNT_LOOP=2` hits); (2) recording — BC stream is read again while emitting SSA IR: each `IRIns` is 64 bits (`lj_ir.h`: two 16-bit operand refs `op1/op2`, combined 8-bit opcode + 8-bit `IRType`, plus a `prev` chain used for CSE); `TRef` operands are tagged with their IR type in the top byte so type checks are immediate compares; (3) optimization — constant folding, CSE via the `prev` hash chain, loop detection by unrolling one iteration and matching PHIs; (4) emission — linear-scan allocation and machine code emission, then the trace is installed and reachable from dispatch via J* opcodes. Guards are ordinary IR instructions flagged `IRT_GUARD` (LT/GE/ULE/EQ, ABC array-bounds checks, SLOADs with `IRSLOAD_TYPECHECK`); every guard corresponds to an exit covered by a `SnapShot` (`lj_jit.h`: `struct SnapShot { uint32 mapofs; IRRef1 ref; uint16 mcofs; uint8 nslots, topslot, nent, count; }` with `SnapEntry = slot<<24 | flags | ref`) that reconstructs interpreter state (stack slots, frame links, PC) so execution can resume anywhere in the bytecode. `count` per exit feeds side-trace promotion. Also notable: `ADDOV/SUBOV/MULOV` (`lj_ir.h` "Overflow-checking arithmetic ops") — checked integer arithmetic whose failure exits the trace; this is the one guard class that survives static typing if we support arbitrary-precision fallback for overflow. Recommendation: define overflow as a panic/effect (Koka-style) instead, so the happy path has zero checks, and offer checked ops as explicit library functions.

Copy-and-patch (Xu & Kjolstad, arXiv:2011.13127; CPython implementation PEP 744, `Tools/jit/`). The compiler "library" is a set of precompiled binary stencils — machine-code fragments emitted by LLVM from a master template — each carrying holes (relocations) for runtime constants, addresses, and type metadata. Compilation = copy stencils for the uop sequence + patch holes: measured compile cost two orders of magnitude below LLVM -O0 with output ~14% faster than LLVM -O0 (SQL compiler on TPC-H, ~10x faster than interpretation); the wasm variant compiles 4.9-6.5x faster than V8's Liftoff with 39-63% faster output (Coremark, PolyBenchC). CPython ships this as its tier-3: `Tools/jit/template.c` is the master template (it includes the real interpreter headers so each stencil is literally a copy of a handler body), `Tools/jit/_stencils.py` extracts holes from object-file relocations, `Tools/jit/_writer.py` patches them, and the tier-2 optimizer's traces trigger compilation through the `_JIT` uop (in `bytecodes.c`: `backoff_counter_triggers(counter)` then `_PyJit_TryInitializeTracing`), entered at `JUMP_BACKWARD_JIT` / `RESUME_CHECK_JIT` sites, with `JUMP_BACKWARD` bytecode patched at runtime to the JIT-aware variant when `interp->jit` exists. Crucial observation for DreamScript: CPython's stencils still contain runtime checks (compact-PyLong checks in `_BINARY_OP_ADD_INT`, layout assertions) because values are boxed `PyObject*`. Our stencils would be guard-free: a stencil for `add_i64` is literally `mov reg, reg; add reg, reg` with hole-patched operand slots. Copy-and-patch is therefore *more* attractive for a static language than for Python: we pay its near-zero compile cost and inherit none of its speculation burden.

OSR (on-stack replacement). The JVM and V8 need OSR to switch a running loop from interpreter to compiled code mid-frame; CPython inserts executors at back-edges (`ENTER_EXECUTOR` patching in `bytecodes.c`; `JUMP_BACKWARD` chooses JIT-variant opcodes — see `bytecodes.c`: `uint8_t desired = tstate->interp->jit ? JUMP_BACKWARD_JIT : JUMP_BACKWARD_NO_JIT;` where the interpreter *rewrites its own bytecode* when a JIT is installed). LuaJIT avoids explicit OSR: loops are bytecode positions, and trace entry replaces the interpreter loop directly at the back-edge. If DreamScript compiles loop bodies (or small traces) as its JIT unit and enters them at back-edges, OSR reduces to "start interpreting/compiling this unit" — no frame transformation machinery. Keep loop bodies small (they are, in a functional language with fold-shaped idioms) and this stays true. Note also CPython's patched-bytecode pattern: the *same* opcode address is rewritten to select behavior (JIT vs no-JIT, specialized vs generic) — cheap, cache-friendly, and a good fit for our hot-reload story.

A worked copy-and-patch example (illustrative). For the typed bytecode op `ADDI64 r1, r1, r0` above, the stencil library contains (conceptually) one x86-64 template per operand class, emitted at build time from a master template that includes the VM headers (CPython's `Tools/jit/template.c` includes ~30 interpreter headers precisely so stencils are literal copies of handler bodies):

```
; stencil ADDI64_rr (registers-resident operands), holes marked <H>
mov  rax, [r14 + <H slot_r1_offset>]   ; hole: u16 frame-slot offset of r1
add  rax, [r14 + <H slot_r0_offset>]   ; hole: u16 frame-slot offset of r0
mov  [r14 + <H slot_r1_offset>], rax
; stencil ADDI64_ri (immediate operand):
mov  rax, [r14 + <H>]
add  rax, <H imm:i64>                  ; hole: patched with the constant
mov  [r14 + <H>], rax
```

Compilation of the loop body is: select one stencil per uop (static operand classes), concatenate, patch the u16/u64 holes from the code object — a memory copy plus byte writes; no assembler, no register allocator (stencils agree on a fixed scratch/register convention), no guards. CPython measures this at sub-microsecond per stencil; our stencils are strictly simpler than CPython's because they contain no type checks, no refcount calls for temporaries, and no error-path glue (errors propagate through registers; the stencil for a fallible op ends in a conditional jump to a shared per-unit error stub).

Deopt strategy in each system, and ours. V8/CPython/LuaJIT/Truffle all maintain interpreter-state materialization for exits (snapshot maps, deopt frames, abstract interpreter state on guarded nodes):

| System | Guard form | Exit cost | Exit mechanism |
|---|---|---|---|
| LuaJIT | IR ops flagged `IRT_GUARD` (type/range/equality) | snapshot walk, restore nslots from `SnapEntry` map | exit to interpreter or side trace at bytecode PC |
| V8 Maglev/TurboFan | map checks, Smi checks, bounds | build deopt frame from translation state | re-enter interpreter, maybe re-optimize |
| CPython tier 2/3 | `_CHECK_*` uops, compactness checks | side exit to tier 1 executor | global side-exit list per trace |
| Truffle | node assumptions | `transferToInterpreter()`, AST rewrite | re-execute AST, recompile |
| DreamScript | none on data path; stack-limit check per function | n/a (no guards) | debugger-driven frame materialization only |

For DreamScript keep deopt only for: (i) debugger breakpoints (re-materialize the interpreter frame from JIT frame metadata — the one piece worth building), (ii) stack-overflow checks (static frame sizes make this a single compare per function entry), (iii) GC safepoints (unnecessary under RC+arena, see section 6), (iv) effect-handler entry (a control transfer, not a deopt). No type, shape, arity, or box-ness deopt exists because none of those are speculative. This collapses both the runtime cost (no guards on any hot path) and the engineering risk of tiering by roughly 70% by our estimate.

### 3. Inline caches, hidden classes, and what static typing removes

Background. V8 gives every object a hidden class (map) recording property offsets; property access compiles to "check map, load at fixed offset" with the check supplied by a monomorphic inline cache in the feedback vector, upgrading to polymorphic (2-4 shapes) or megamorphic (global stub) forms. LuaJIT bytecode has no per-site field ICs; instead, in traces, TGETS/HREFK lower to HLOAD/FLOAD with constant offsets — `lj_ir.h` `IRFLDEF` is literally a table of `(name, offsetof(structure, field))` pairs (`_(STR_LEN, offsetof(GCstr, len))`, `_(TAB_ARRAY, offsetof(GCtab, array))`, `_(FUNC_PC, offsetof(GCfunc, l.pc))`), i.e., once shape is confirmed via guards, access is a plain load at a static offset.

What static typing removes entirely: (1) map checks and field ICs — the field offset is emitted into the bytecode as an immediate (`FIELD_LOAD %dst, %obj, #12`), with no check because the receiver's type is static; (2) type guards on every operation — arithmetic opcodes are representation-specific; (3) arity checks on static calls; (4) most dynamic dispatch on primitives; (5) the feedback-vector infrastructure and its memory; (6) the deopt machinery those guards feed. Evidence of the scale of this win: CPython 3.11's ~1.25x improvement (PEP 659) came mostly from adding adaptive specialization to approximate statically-known types; LuaJIT spends a large fraction of its recorder/optimizer code (`lj_record.c`, `lj_opt_*.c`) inferring types that DreamScript's front end already proves; Maglev's headline features (map checks, stable-field dependencies) exist to recover type information at runtime.

Where IC-style mechanisms remain useful in DreamScript:

1. Trait/dyn dispatch: at a dyn (existential/trait-object) callsite, keep a 1-2 entry inline cache mapping receiver type id -> concrete function pointer (a monomorphic IC over a vtable); megamorphic sites fall through to the type's method table. The cache is a pure perf hint with no correctness role — the type-id check is part of the cached key, so a stale hit is impossible. This is the same shape as CPython's specialization counters or a LuaJIT CALLT dispatch, minus deopt.
2. Hot-reloadable module globals: a callsite can cache the resolved function and invalidate on redefinition via a module version counter (CPython achieves the analogous effect by rewriting bytecode opcodes in place — `_SPECIALIZE_*` — which is cheap and works well; we can patch cached pointers instead).
3. Effect-handler operation tables: per-effect-row dispatch resolved once per frame — a dictionary lookup amortized by frame, not per call.

### 4. Value representation, unboxing, and linearly-enabled stack allocation

Survey of representations. Boxed uniform values (CPython `PyObject*`): every scalar is a heap object — one indirection, one allocation per number, plus refcount traffic. Tagged/NaN-boxed values (LuaJIT `lj_obj.h`): one 64-bit word per value; in GC64 mode the encoding is 13 high bits all 1 (a non-canonical NaN prefix), a 4-bit itype at bits 47-50, and a 47-bit payload (pointer zero-extended, or all-ones for primitives, or a 32-bit int zero-extended under DUALNUM); `itype(o)` is `((o)->it64 >> 47)` — a single shift for any type test; `tvisint`/`tvisnum` distinguish DUALNUM int vs double. V8 uses Smi tagging (31-bit payloads in a word) plus pointer compression: heap pointers are 32-bit offsets into a 4GB cage, halving memory and improving cache density (v8.dev/blog/pointer-compression). LuaJIT's traces go further and unbox everything: `IRTDEF` in `lj_ir.h` has I8/U8/I16/U16/INT/U32/I64/U64/FLOAT/NUM as first-class IR types, and `SLOAD` mode bits (`IRSLOAD_CONVERT`, `IRSLOAD_TYPECHECK`) govern re-materializing typed values from tagged slots. A statically typed VM can simply run on the typed representation permanently: each bytecode register has a fixed type, so the frame is a heterogeneous register file — i32/i64/f64 stored raw, references as raw GC pointers, bools as single bytes. Concretely: `add_i64` reads two frame slots and emits one `add`; a dynamic VM's ADDVN must test tags (branch), unbox (shift/mask), add, re-tag, and possibly handle overflow-into-double.

Unboxed ADT/enum/Result layout. Represent ADTs as tag + payload across consecutive registers (e.g., `Result<T,E>` with T:E-bytes-wide occupies 1+N slots: tag slot, payload slots). Construction writes the tag; matching branches on it; zero heap allocation, zero boxing — unlike CPython (tuple/exc allocations) and dynamic VMs (every enum is a heap object). For heap values (shared or boxed structures), a fixed header: type id (for dyn dispatch ICs and RC), refcount, payload. Suggested header for DreamScript: `{ u32 type_id; u32 rc; }` = 8 bytes, keeping small boxed objects at 16 bytes total (Inko 0.20.0 reworked strings to a single allocation with a 16-byte minimum for the same cache-line economics).

ADT layout in a frame (illustrative):

```
let r: Result<Trade, ParseError> = parse(input)   -- parse returns unboxed
  frame slot layout: [tag:u8][Trade payload: 6 slots]  or  [tag:u8][ParseError payload: 2 slots]
  match r { Ok(t) => ..., Err(e) => ... }
  ->  0: CALL  r2..r9, parse          ; returns tag in r2, payload in r3..r9
      1: TAGTEST r2, OK_TAG           ; r10 = (r2 == OK_TAG)
      2: JMPIF_NOT r10 -> err_path    ; single branch, no tables, no heap
```

Escape analysis and scalar replacement. Inko's experience is the best public datapoint for an ownership-flavored language: Inko 0.20.0 (April 2026) added whole-program interprocedural EA run during the inlining pass (`inko/compiler/src/mir/escape.rs`, `inline.rs`; inlining processes methods in reverse-topological SCC order so callers reuse callees' non-escape results), promoting 47-58% of heap allocations to the stack across five real projects (shost 58%, OpenFlow 51%, kvi 50%, idoc 47%, wtml 54%), with HTTP header/method parsing 2.2x and 2x faster. Note the cost: whole-program analysis, inlining-coupled results, and still partial coverage. With linear types, non-escape is a typing invariant: a value is heap-allocated iff it is moved into a boxed/shared context, and the type checker already knows this per site at bytecode-emit time. Consequences for the VM: (i) every local ADT/tuple/Result lives in the frame's registers — zero allocation on the fast path; (ii) scalar replacement is trivial (the bytecode allocator assigns registers; there is nothing left to replace); (iii) closures that provably do not escape become stack frames rather than heap closures; (iv) any GC only ever sees genuinely boxed data, shrinking root sets; (v) error handling allocates nothing on either path. This is the biggest interpreter-specific dividend of the type system: dynamic interpreters structurally cannot do it, and even AOT languages only approximate it heuristically.

Strings. Immutable UTF-8 byte arrays with a length (Rust-style) give O(1) slicing via (ptr,len) views; ropes pay off only for heavy incremental concatenation; full interning (Lua/LuaJIT) makes equality pointer-equal but taxes creation with hashing. Inko 0.20.0 moved strings to a single allocation (min 16 bytes). DreamScript recommendation: immutable UTF-8 + slices-as-views + an optional rope in the standard library for editor-shaped workloads; intern only short identifiers/tags used in hot comparisons.

### 5. Guaranteed tail calls

Interpreter implementations, with sources:

- Frame reuse in a register VM (Lua): `luaD_pretailcall` (`lua/ldo.c`, around line 671) reuses the current `CallInfo`, moves function+args down by `delta` (needed for vararg frames), completes fixed params with nil, resets `ci->top` and `savedpc`, sets `CIST_TAIL`, and returns to the shared `luaV_execute` loop — O(args) work, no stack growth, correct for self and mutual tail calls (anything in tail position). LuaJIT does the same in its assembly interpreter via CALLMT/CALLT opcodes (`lj_bc.h`, "Calls and vararg handling. T = tail call"), copying args onto the caller's frame slots and jumping.
- Copy-and-jump in C (Koka): every function body compiles inside a loop with a `kk__tailcall:` label; a self tail call evaluates arguments into temporaries first (`genTailVarBinding`, and `genOverride` carefully skips self-assignments to avoid clobbering — `koka/src/Backend/C/FromCore.hs` lines ~1428-1470), reassigns the C parameters, then `goto kk__tailcall;` (line ~2817). Mutual tail recursion among a group is handled by the same label machinery within the merged body. The C compiler sees an ordinary loop, so register allocation and branch prediction are optimal.
- Trampolining (Chicken Scheme's "Cheney on the MTA"): every call returns the next thunk; a driver loop jumps. Simple and portable, but adds an indirect jump per call and complicates native-JIT interop. Not needed when we control the VM's calling convention.
- JIT frame-slot reuse: place caller argument slots exactly where the callee's frame slots live (LuaJIT does this on the value stack; wasm encodes it in the spec); a tail call in machine code is then a shuffle + `jmp` to the callee entry, and the frame pointer never moves. This requires the frame layout be part of the ABI design from day one — trivial for us since we own both sides.
- Platform inheritance (wasm): Wasm 3.0 (completed September 2025, webassembly.org/news/2025-09-17-wasm-3.0) standardized `return_call`/`return_call_indirect` (proposal merged from github.com/webassembly/tail-call; shipped in V8/Chrome 112, v8.dev/blog/wasm-tail-call), so a wasm backend gets guaranteed TCO from the platform, including through the engine's own optimizing tiers.

Counter-evidence that guaranteeing TCO is a deliberate choice: Ruby has no guaranteed TCO; CRuby's compile-time option is best-effort and off by default, and neither YJIT nor the newer ZJIT transforms tail-recursive calls into loops (as of 2025-2026; blog.tdg5.com/tail-call-optimization-in-ruby-background and ruby-core ZJIT threads). JIT teams consistently deprioritize it because frame elision interacts with deopt metadata and stack introspection. Guaranteeing it in the semantics from day one (Scheme/R7RS style) forces the correct design everywhere: tail position is a defined concept; frames are elidable; and debugger backtraces carry explicit tail-call chain metadata (Lua's `CIST_TAIL` flag supports `debug.traceback` despite full TCO).

Per-language TCO handling summary:

| Language/Runtime | Interpreter TCO | JIT TCO | Mechanism |
|---|---|---|---|
| Lua 5.4/5.5 | guaranteed (spec) | n/a | CallInfo reuse + arg slide (`luaD_pretailcall`, CIST_TAIL) |
| LuaJIT | guaranteed (spec) | yes | CALLT/CALLMT slide args on value stack, jump |
| Scheme/R7RS | guaranteed | yes | language requirement; frame reuse |
| Koka | n/a (AOT C) | yes | `kk__tailcall:` label loop, param reassignment (FromCore.hs) |
| Wasm engines | n/a | guaranteed via spec | `return_call`/`return_call_indirect` (wasm 3.0) |
| Ruby (YJIT/ZJIT) | optional, off | no | none; best-effort compile flag only |
| JS engines | no | no | ES6 requires TCO per spec but engines (V8) never shipped it |
| CPython | no | no | deep recursion overflow; 3.15 tail-call *dispatch* is unrelated |

Machine-level sketch of JIT tail calls with slot-over-laying frames (the ABI decision DreamScript should adopt): caller's outgoing args are written into the tail of its own frame exactly where the callee's incoming slots are; a tail call then compiles to (at most) a few register shuffles plus `jmp callee_entry` — the return address and frame pointer stay untouched, so the stack never grows regardless of call depth. In the interpreter the same invariant is `memcpy(args down by delta); reload prologue; continue dispatch loop` (LuaJIT CALLT). Both are O(args), allocation-free, and preserve RC ownership transfers (args move; no refcount traffic).

Interactions. (1) With effects/async: a tail call into an effectful function is just another effectful continuation jump; the subtlety is capture — a tail call under a `handle`/capture boundary must materialize a frame for the continuation, so the guarantee should read "proper tail calls within the same effect context" (Koka's effective rule). One-shot continuations (Koka-style, kklib's forked stack contexts) make handler entry cheap and keep TCO unaffected outside handlers. (2) With deopt/debugging: frame reuse destroys caller frames; support lazy materialization from TCO chain metadata when a debugger asks. (3) With RC: a tail call transferring ownership of arguments performs no refcount updates — linearity makes frame reuse the natural lowering (args move into the new frame's slots), so TCO is actually cheaper than a normal call.

### 6. Memory management for the runtime

Options surveyed, with evidence:

- Generational copying GC (Ocaml, Haskell, V8 young gen): allocation is a pointer bump; young-gen collection is fast but moving — requires write barriers on every pointer store, remembered sets, and JIT safepoints with precise stack maps (V8 documents the machinery in v8.dev/blog/trash-talk on Orinoco). Best absolute throughput for allocation-heavy immutable data; highest VM complexity; pauses need tuning.
- Immix/mark-region (Inko 1.x): 32KB bump-allocated blocks, non-moving happy path, evacuation only under fragmentation; yorickpeterse.com/articles/friendship-ended-with-the-garbage-collector records why Inko first removed its moving GC for this, and later removed GC entirely.
- Moving compacting full GC (V8 Orinoco, Go-style): best heap compactness; heaviest machinery (parallel marking, evacuation, barriers).
- Perceus precise RC with reuse (Koka; Reinking, Xie, de Moura, Dreyer, PLDI 2021, "Perceus: Garbage Free Reference Counting with Reuse"): the compiler places dup/decref statically and pairs frees with subsequent allocations via reuse analysis — `alloc` becomes "branch to reuse path, overwrite fields" with no allocator call on the hot path. Koka runtime evidence: `kklib/include/kklib.h` defines `kk_reuse_t` (nullable recycled block pointer), `kk_block_alloc_at(kk_reuse_t at, ...)` which allocates at the recycled block when non-null or falls back to `kk_malloc_small` (mimalloc thread-local `mi_theap_malloc`), and cold-path `kk_block_check_drop_reuse`; the codegen lives in `koka/src/Backend/C/Parc.hs` (borrowed/owned environments mirroring the paper's discipline) with the reuse pass in `ParcReuse.hs` (note its comment: all dups before drops, drops before drop-reuses, ordered by pattern-tree depth — the ordering constraint is solved for us by their pass structure).
- ARC + escape analysis (Swift; Inko 2.x uses its own non-weak atomic ARC, `inko/rt/src/arc_without_weak.rs`, after replacing tracing GC with ownership in 0.10.0, inko-lang.org/news/inko-0-10-0-released): deterministic, pauseless, but naive ARC pays refcount traffic on every copy; EA rescues about half of allocations (Inko's 47-58% figure), leaving the rest on the RC fast path.
- Arena/region allocation (Vale regions; per-frame arenas): O(1) bulk reclamation, ideal for interpreter temporaries and for bounded-lifetime module data; requires an escape proof to be safe — which linearity supplies statically.
- Cycle handling: tracing GCs handle cycles by construction; RC needs either acyclicity by typing, trial deletion, a backup mark-sweep over candidates, or explicit opt-in.

For DreamScript specifically: linear types mean values are used exactly once, so refcount updates collapse to explicit clone/move boundaries — precisely Perceus, but with far fewer dups than Koka must infer (we know usage counts statically from the type system). Immutable-by-default removes almost all pointer-store write barriers (no field mutation), which is what makes generational GCs complex in the first place. Recommended combination, in order: (1) per-frame bump arena for interpreter temporaries — allocation is an add, reset is a restore; sound because linearity guarantees temporaries do not escape unless moved out of the frame; (2) Perceus-style precise RC with block reuse for boxed values (Koka model, simplified by linearity; pair each drop with the next alloc via the reuse pass); (3) no tracing GC initially. Cycles are expressible only through explicit mutable ref cells or lazy knots; either reject them at the type level (module-level recursion handled specially) or provide an explicit `unsafe_cycle`/arena API with trial deletion, keeping the VM free of a general collector. Result: zero GC pauses by construction, near-zero allocator traffic, no barriers, no safepoints, no GC maps in JIT frames — while staying compatible with adding a concurrent collector later for shared data if profiling demands it.

A worked Perceus example (illustrative, in Koka's compiled-C style). Source: `let x = pair(1, f(y))` where `pair` is a boxed 2-tuple constructor and `f` consumes `y` linearly:

```
// compiled with reuse: decrement-then-alloc becomes alloc_at(reuse)
tmp2 = f(y);                       // y consumed linearly: no decref (moved)
rc_dec(tmp2_rc_field);             // tmp2 dies after field extraction -> candidate
reuse = (rc(tmp2) == 1) ? tmp2 : NULL;
x = alloc_at(reuse, sizeof(pair)); // reuses tmp2's block when it died uniquely
x.tag = TAG_PAIR; x.a = 1; x.b = extracted;
```

Koka's runtime implements exactly this shape: `kk_block_alloc_at(kk_reuse_t at, ...)` falls back to `kk_malloc_small` (mimalloc thread-heap) when `at == kk_reuse_null`, and the cold path `kk_block_check_drop_reuse` handles the general decrement-with-reuse case (`kklib/include/kklib.h`). The reuse pass (`koka/src/Backend/C/ParcReuse.hs`) enforces the ordering constraint documented in its source: dups before drops, drops before drop-reuses, each ordered by pattern-tree depth so parents are processed before children. For DreamScript the linearity checker supplies the usage counts directly, so the dup/drop placement pass degenerates to: insert `rc_inc` at each explicit `clone`, `rc_dec` at each value's unique death point (last use or scope end — syntactically visible), and thread reuse candidates from each drop to the next allocation in straight-line code. No borrow analysis, no interference analysis.

### 7. IR design

MLIR and Mojo. MLIR's core value is progressive lowering through dialects with shared infrastructure and round-trippable textual IR; Mojo compiles through MLIR to native code with SIMD types visible to the compiler's optimizers (mlir.llvm.org; Modular docs; recent third-party summaries describe the pipeline as "progressively lowers high-level code through multiple layers"). Adopting MLIR wholesale buys a large toolchain dependency and a C++-adjacent build story; for a small language the right move is to copy the pattern — a stack of small typed IRs that lower into each other (source -> typed bytecode -> typed SSA -> target IR) — without the dependency.

Cranelift (source: `wasmtime/cranelift/`). CLIF is a typed SSA IR per function (`docs/ir.md`: explicit block parameters, value types i8..i64/f32/f64/v128, explicit `stack_slot`s, direct branches; the doc walks an `average` function example with `ss0 = explicit_slot 8`). Optimizations are ISLE rewrite rules (`codegen/src/opts/cprop.isle`, `icmp.isle`, `arithmetic.isle`, `selects.isle`, ...) run inside an e-graph with a cost function and extraction (`codegen/src/egraph/{mod.rs, cost.rs, elaborate.rs}`); the rules-README (`opts/README.md`) documents the discipline: rules are directional, must not rewrite to worse code, canonicalizing rules must produce no-worse output, and any rule that removes value-uses must use `subsume` to stay sound. Instruction selection/lowering is also ISLE-generated per ISA. Using Cranelift as a library for DreamScript's optimizing tier is the highest-leverage reuse available: our typed SSA lowers 1:1 into CLIF (no boxing conversions, no type checks — every CLIF value type is known statically), and ISLE gives us ergonomic domain-specific rewrites (buffer fusion, bounds-check elimination by linearity, effect-directed reordering) at e-graph quality.

Wasm as a target. Wasm 3.0 (September 2025) standardizes tail calls and includes GC types; measured wasm overhead vs native is ~1.55x geometric mean in Chrome and ~1.45x in Firefox ("Not So Fast: Analyzing the Performance of WebAssembly vs. Native", IMC 2019, ar5iv.labs.arxiv.org/html/1901.09056). A wasm emitter for the typed SSA gives browser/sandbox deployment and near-native performance as a deployment mode rather than a JIT — with `return_call` carrying our TCO guarantee through the engine.

MoonBit (moonbitlang.com/blog/js-support; stdlib repo moonbit-core). A statically typed ML-flavored language whose pipeline is parallelized incremental type-check/inference -> core IR -> wasm/JS/native backends. Measured results: 25x faster than native JS iterator helpers on a data-processing benchmark, ~8x faster than Json5, attributed to compiler-level iterator fusion (a zero-cost `Iter` type, Rust-inspired), constant folding, tail-recursion-to-loop, and char-as-codepoint representation; their IDE achieves millisecond incremental compilation, which is also the substrate for fast hot reload. MoonBit is the existence proof that a small team with a clean typed IR plus a modest optimizer reaches near-C throughput on wasm — and its multi-backend economics (one IR, three targets) match our tier plan.

Typed-IR-enables-X evidence for our feature set. (1) Effect rows prove purity locally: an effect-free block may be reordered, CSE'd, and vectorized across calls — dynamic VMs approximate this with escape and state analyses, we get it from the type. (2) Linear types prove non-aliasing, which unlocks: buffer fusion (no intermediate allocation for map/filter/zip chains — the intermediate provably has no other readers), in-place mutation of uniquely-owned data (affine mut phases), and elimination of bounds checks where the linear indexing discipline already proves them (or reduction to checked-add forms the target ISA folds). (3) A typed SSA core (per-function, explicit types, explicit effect row per call, explicit linearity on values) turns all of these into local rewrites instead of global analyses — the same property that lets Cranelift express its optimizer as directional ISLE rules over SSA.

How the typed SSA lowers to CLIF, concretely. DreamScript SSA for `sum_to` (sketch):

```
v0: i64 = param n            ; linear: consumed by loop
v1: i64 = const 0
loop(v2: i64 = v1, v3: i64 = v0):        ; block params = phi-free SSA
  v4: bool = le v3, 0
  br_if v4 -> exit
  v5: i64 = add v2, v3
  v6: i64 = sub v3, 1
  br loop(v5, v6)
exit(v7: i64 = v2)
```

The corresponding CLIF (compare `cranelift/docs/ir.md`'s `average` example, which uses `explicit_slot` + `stack_store/stack_load`):

```
function %sum_to(i64) -> i64 {
block0(v0: i64):
    v1 = i64_const 0
    jump block1(v1, v0)
block1(v2: i64, v3: i64):
    v4 = icmp sle v3, 0
    brif v4, block2(v2), block1(...)      ; via block-args, no phis
    v5 = iadd v2, v3
    v6 = isub_imm v3, 1
    jump block1(v5, v6)
block2(v7: i64):
    return v7
}
```

Every value is typed; there are no boxing/unboxing ops and no checks to emit — the lowering is mechanical, which is why Cranelift (or a wasm emitter) is a small pass rather than a compiler project. Domain-specific optimizations slot into the ISLE rule layer; an example in Cranelift's own style (`cranelift/codegen/src/opts/arithmetic.isle` idiom, directional rules with `subsume` where uses vanish per `opts/README.md`):

```
;; fuse: (x * 2^c) + x  =>  x * (2^c + 1)   -- direction matters: rhs no worse
(rule ((value (ty I64) x) (value (ty I64) c))
      ((shl (add x (mul x (i64_const 1))) c))   ; matched pattern
      ((mul x (add (shl (i64_const 1) c) (i64_const 1))))
      (if (ult c (i64_const 63))))
```

DreamScript-specific rules of the same shape: fuse `append(map(a, f), map(b, f))` buffer chains into one loop when buffers are linear (no aliasing proof needed); rewrite effect-free call pairs into either order only when the effect row is empty (checked at rule-match time from IR metadata); eliminate bounds checks where a linear cursor discipline (e.g., `for` over a slice) has already proven range containment.

### 8. Generics implementation and the anti-specialization stance

The options and their runtime profiles:

- Monomorphization (Rust, C++, Swift's generic specialization path, Inko 2.x under LLVM): zero runtime cost per call; costs compile time and code size scaling with instantiations; in C++/Rust it drags in specialization/overlap rules, which the author explicitly rejects (philosophy_of_PL.md: "非常不喜欢特化" (strongly dislikes specialization) — prefers set-theoretic types with non-overlapping matches resolved by the programmer).
- Boxing + dictionary passing (Java, OCaml, GHC default): one compiled body; costs boxing for primitive args and an indirect call through the dictionary; predictable, coherent, no bloat.
- Erasure to dynamic (Gleam -> BEAM, Inko 1.x bytecode): generic code is truly dynamic at runtime; all costs are dispatch costs. Inko's decade article documents the specific pain: with values typed `Any`, keyword arguments could not be resolved at compile time and had to be encoded in bytecode and resolved by the VM at runtime — gradual typing blocked inlining and other optimizations, one of the forces that pushed Inko to full static types and LLVM.
- JIT devirtualization/speculation (V8, Dart VM, PyPy): best runtime profile when predictions hold; paid in guards, deopt, and inline-cache machinery — machinery we are eliminating.

Position for DreamScript that respects the author's taste: (1) the language has no specialization semantics at all — a generic function has exactly one meaning; instance coherence (global uniqueness, Haskell-style) supplies dictionaries; overlap resolution is a type-system error, not a priority rule. (2) The implementation may inline generic calls at callsites when profitable — this is compiler inlining, invisible to the programmer, and requires no overlap rules because every instantiation is by definition the same function (no user-written specialization bodies exist to prioritize). (3) For non-inlined calls, pass dictionaries as unboxed values: a dictionary for a single-method trait is a code pointer in a register; for numeric-representation-polymorphic operations, the runtime supplies per-representation code (the same per-representation stencils the copy-and-patch tier uses), which removes the classic boxing penalty of dictionary passing for primitives. Evidence that dictionary passing sustains high performance: OCaml and GHC ship it broadly; measured losses concentrate in primitive-type polymorphism, which an unboxed typed-IR VM largely eliminates. If profiling later shows hot generic numeric code suffering, add an explicit, user-listed `specialize` boundary (an import-list-style pragma that instantiates named functions at named types) — a controlled escape hatch that stays outside the core semantics, rather than C++/Rust-style open-ended specialization.

### 9. Other interpreter-level optimizations

- Bytecode caching and mmap startup: fixed-width, index-based, position-independent bytecode loads with one mmap and no relocation (LuaJIT's dump format, CPython .pyc, wasm modules all demonstrate this). With typed bytecode there is no lazily-built runtime metadata to warm up — unlike V8, whose feedback vectors start empty and whose tiers must observe behavior. Cold start = parse-free `load` + main-frame setup; target single-digit milliseconds for large programs. A sketch of the module format that supports this:

```
header:  magic, u32 ir_version, u32 flags, u64 source_hash
tables:  types[]   (id, kind, field offsets, sizes)     ; referenced by opcodes
         consts[]  (i64/f64/str/bytes pool, offsets)
funcs[]: for each function:
           name_idx, type_idx, frame_slots (with per-slot type ids),
           frame_size_bytes, max_call_depth,
           code: u32[] fixed-width instructions
no pointers, no relocations, host-endian in the on-disk fast format
```

- Hot reload: re-emit bytecode per module; call-site caches (section 3) invalidate via a module version counter; tier-2/3 code is keyed on (function version, stencil set) pairs so only touched functions recompile (MoonBit's millisecond incremental pipeline shows the toolchain-side of this is achievable). Two rules keep this simple in a linear language: modules are immutable values (a reloaded module is a new value bound at the call-site cache, never an in-place mutation), and running frames continue on their old code (like Erlang's two-version scheme) until they return.
- Call overhead engineering: static calls in the interpreter are a computed-goto hop plus a frame push (LuaJIT FUNCF prologue op does arg-shift + dispatch in a handful of instructions); dyn calls pay one type-id check + table load (the IC of section 3). This matters because functional-style DreamScript will make many small calls — keep the prologue/epilogue lean (Lua's CALL does push/pop of results inline; CPython's CALL costs several uops plus method-lookup fallbacks we never pay).
- Persistent vs mutable data: immutable-by-default enables structural sharing safely (RC'd persistent vectors/maps), while mutable unique (affine) data compiles to in-place mutation — one runtime can expose both: `Array` (unique, growable, zero-copy ops) vs `Shared`/boxed persistent structures. Cost asymmetry to document: persistent update allocates O(log n) RC'd nodes (reuse-friendly under Perceus), affine in-place update allocates zero. The language should make the common read path identical for both so switching is cheap.
- Error representation: unboxed Result in registers (section 4) means the error path costs one tag branch on return — no exception tables, no landing pads, no unwind tables on the hot path. Panics/unrecoverable errors use a cold `longjmp`-style unwinder or effect propagation. Compare: exception-table models (Lua's longjmp, Java, C++ zero-cost EH) pay metadata + table lookups to make throwing cheap; Result models pay a compare on every call that can fail. In a language where errors are tracked in the type (DreamScript's stance per philosophy_of_PL.md's Result preference), Result-in-registers is strictly better: the success path is one flagged branch, and the compiler can often fold it (e.g., when the caller immediately matches). Koka demonstrates exceptions-as-effects with continuation capture — the price is frame materialization at `handle` boundaries only, which fits our effect-entry deopt budget.
- Startup vs warmup posture: tier 1 starts instantly (no profiling phase needed before tier 2 because our tiers are type-driven, not behavior-driven — the copy-and-patch tier can compile on first execution or lazily per loop with backoff counters like CPython's `_JIT` uop); PyPy's documented multi-second warmup and V8's tiering budgets are costs we structurally avoid. Short scripts (<0.2s, PyPy's documented dead zone) stay on tier 1 and still hit 3-10x of C.
- Superinstructions and bytecode peephole: implement as an assembler pass with profile feedback (CPython's `macro(...)` composition is static; LuaJIT's I*/J* prologue variants show the value of specialized prologues). Cheap wins: compare-and-branch fusion (JMPIF after TAGTEST), load-op fusion (LDIMM feeding arithmetic), match-dispatch jump tables for ADT tags with dense tags.

### 10. Evidence for the performance ceiling

Data points with sources:

- Plain Lua 5.x (register VM, tagged slots): ~25x slower than LuaJIT and roughly 20-50x slower than C depending on workload (eklausmeier.goip.de/blog/2016/04-05-performance-comparison-c-vs-lua-vs-luajit-vs-java: C ~3x faster than LuaJIT; LuaJIT ~25x faster than Lua on his benchmark).
- LuaJIT: ~2-3x slower than C on numeric code (same measurement); near-C in tight traced loops; Cloudflare (blog.cloudflare.com/more-consistent-luajit-performance) documents excellent trace-stable performance with cliff behavior when traces exit — the dynamic-language tax paid even by the best dynamic VM.
- CPython: ~50-100x slower than C; PEP 659 specialization bought ~1.25x on pyperformance (3.11) — incremental gains from approximating static knowledge; the tail-call interpreter (3.15-era) and copy-and-patch JIT (3.13+) continue the grind from a boxed, dynamic base.
- PyPy: often 5-15x of C after multi-second warmup; documented pathologies include short scripts and up to 2x-slower-than-CPython cases (pypy.org/performance.html).
- Truffle/Graal: "competitive with, and in many cases outperforms, the highest performance implementations" (One VM to rule them all, Onward! 2013) — partial evaluation that erases interpreter overhead reaches peak-VM quality; our static bytecode is a cheap, static form of the same idea.
- Copy-and-patch: output ~14% faster than LLVM -O0; compile 100x faster than LLVM -O0; wasm variant 39-63% faster than Liftoff (arXiv:2011.13127).
- Wasm: ~1.55x native (Chrome geometric mean, IMC 2019); narrowing with wasm 3.0 features.
- MoonBit: 25x vs JS iterator helpers, ~8x vs Json5 (moonbitlang.com/blog/js-support) — near-C on wasm via typed IR + fusion; a small-team datapoint.
- Inko (LLVM, ownership + ARC + EA): interprocedural EA promoted 47-58% of allocations and gave 2-2.2x on HTTP parsing (Inko 0.20.0); a decade of effort to aspire to Go-level performance — both a target and a warning about scope.

Interpretation. The interpreter-to-C gap decomposes into: (i) dispatch overhead (~1.5-3x; mitigated by threaded dispatch, superinstructions, trace-stitching); (ii) boxing/tag overhead (~2-10x on scalar code; eliminated entirely by static typing + unboxed registers); (iii) guard/speculation overhead in dynamic JITs (~1.2-2x where present; absent for us); (iv) allocation pressure (largely eliminated for temporaries by linearity + frame registers + arenas). A well-built typed register interpreter should therefore land around 3-10x of C on scalar/numeric code (versus ~25-50x for dynamic register VMs); a copy-and-patch tier should approach 1.5-3x; an optimizing tier (Cranelift) should reach 1-2x — with the interpreter remaining the instant-start fallback and debugger substrate, and no deopt cliffs anywhere. These are conservative extrapolations anchored on the LuaJIT (dynamic ceiling) and copy-and-patch (baseline compiler) datapoints, with the boxing term — the one LuaJIT pays and we do not — removed from tier 1 entirely.

### 11. Quantifying what static typing buys the interpreter

The recurring question is how much a static type system is worth to an interpreter, beyond what a good dynamic JIT recovers. Per operation class, comparing DreamScript's tier 1 against the dynamic baselines:

| Operation | Dynamic VM cost (per op) | DreamScript tier-1 cost | Saving |
|---|---|---|---|
| i64 add | tag check x2, unbox x2, add, retag (LuaJIT ADDVV path) or full method dispatch (CPython) | one add | ~3-6x fewer uops |
| field access | map check + offset load (V8) / hash lookup in interpreter (Lua) | immediate-offset load, no check | ~2-4x |
| call (static target) | feedback check, arity check, frame alloc | prologue op + frame slide | ~2-3x |
| call (trait/dyn) | IC hit + call | type-id check + table load + call | ~1.2-1.5x |
| tuple/ADT construct | heap alloc + RC/GC | register writes | ~5-20x (allocation is the cost) |
| Result/enum match | heap tag read + branch | register tag read + branch | ~2-3x |
| loop iteration | same dispatch | same dispatch | 1.0x (dispatch is irreducible) |

Multiplying through a typical scalar workload (Lucas/series/fib-style loops are dominated by arithmetic + calls + ADT temps), the per-op savings compound to the 3-10x tier-1 estimate of section 10 — without any JIT. The same table explains why LuaJIT's trace tier is fast: it recovers exactly the first four rows via guards and speculation, at the price of the guard/deopt machinery; our type system recovers them for free and permanently.

### 12. Risks and open questions

1. Interpreter throughput is still dispatch-bound for call-heavy functional code; if profiling shows calls dominating, add call inlining at the bytecode level (an assembler pass, like CPython's macro ops) before reaching for a JIT.
2. Copy-and-patch ties tier 2 to a build-time LLVM and per-arch stencil libraries; if the toolchain burden is unacceptable, the fallback is a tiny hand-written emitter for a restricted uop subset (LuaJIT's lj_asm.c is a self-contained example of that philosophy, just larger).
3. Perceus-style RC needs the reuse pass to hit its "allocation is free" promise; without it, allocation falls back to mimalloc (still fast, ~15-25 cycles). Budget the reuse pass as part of the optimizer, not the checker.
4. Effect handlers + TCO + JIT interact at handler entry (continuation capture must materialize frames); keep handlers out of hot loops or make handler entry an explicit, cold-ish operation (Koka's one-shot continuations are the evidence this is acceptable).
5. Cross-platform stencil generation doubles CI matrix work; consider shipping tier 1 only on platforms without verified stencil builds (CPython effectively does this — the JIT is opt-in per platform).
6. The 1-2x-of-C ceiling for tier 3 depends on Cranelift's continued improvement (its egraph optimizer is young relative to LLVM); if the gap matters, the wasm/native AOT path can later substitute a heavier backend (LLVM) using the same SSA, at the cost of compile time and dependency weight.

### 13. What to steal from each system (condensed)

From Lua/LuaJIT: 32-bit fixed register instructions with typed opcodes; frames inline in the value stack with size-word metadata; CALLT-style arg slide for TCO; J* trace-stitching opcodes for free tier transitions; biased jump offsets and XOR-negated comparison pairs; per-bytecode-position hot counters. From CPython: the uop DSL with explicit stack effects and static superinstruction composition; the tail-call interpreter's musttail dispatch as the portable-ish tier-1 alternative; copy-and-patch infrastructure (stencil extraction, patcher) as tier 2; in-place opcode rewriting as the hot-reload invalidation mechanism; backoff counters for JIT triggering. From V8: nothing structural is needed from its tiers (guards/deopt are exactly what we avoid), but its inline-cache discipline survives at dyn-dispatch sites, and Sparkplug's frame-preservation rule is a good invariant for our tier 2 (preserve interpreter frame layout so the debugger always works). From Truffle: the partial-evaluation mental model — our bytecode is the pre-evaluated representation — and the node-rewriting idea maps to our call-site caches. From Koka: Perceus RC-with-reuse placement and the `kk__tailcall` label-loop lowering; effects-as-unified-control (async/errors) with one-shot continuations. From Inko: the value of static types + EA measured honestly (47-58% promotion), single-allocation 16-byte string economics, and the cautionary tale about gradual typing blocking optimization. From MoonBit: multi-backend economics from one typed IR and compiler-level iterator fusion as a standard optimization. From Cranelift: CLIF-style typed SSA, directional ISLE rules with subsume discipline, and an e-graph with cost extraction as the optimizing core.

## (c) Tradeoff tables

### Table 1: VM architecture

| Architecture | Perf vs C | Complexity | Startup | Notes |
|---|---|---|---|---|
| Tree-walking | 50-200x slower | very low | fast | No caching/tiering substrate; reject |
| Stack bytecode (CPython-style) | 30-100x slower | low | fast | More instructions per op; needed PEP 659 heroics to be decent |
| Register bytecode, dynamic (Lua/LuaJIT tier 1) | 10-30x slower | low-medium | fast | 32-bit insns; NaN-boxed tagged slots |
| Register bytecode, statically typed, unboxed (DreamScript tier 1) | 3-10x slower | medium | fast | No tags, no guards, unboxed ADTs; recommended |
| + copy-and-patch JIT (tier 2) | 1.5-3x slower | +medium | +us-ms compile, +stencil binary | Guard-free stencils; per-uop templates |
| + optimizing tier (Cranelift) (tier 3) | 1-2x slower | +high | +ms compile per function | Shared typed SSA; ISLE/egraph opts |
| Wasm AOT of the same IR (deployment) | 1.5-2.5x slower | +medium | +wasm compile | TCO via return_call; sandbox/browser |

### Table 2: Dispatch mechanism

| Mechanism | Speed | Portability | Complexity | Evidence |
|---|---|---|---|---|
| Switch | baseline | full | lowest | pre-3.11 CPython |
| Computed goto | +5-20% | GCC/Clang | low | `lua/lvm.c` + `ljumptab.h`; CPython `USE_COMPUTED_GOTOS` |
| Tail-call threading | similar or better RA | needs `musttail`+`preserve_none` (clang/GCC/MSVC 2026) | medium | CPython main `_Py_TAIL_CALL_INTERP` (`Python/ceval_macros.h`) |
| Trace-stitched (J* opcodes) | removes dispatch on hot loops | VM-internal | medium-high | LuaJIT `lj_bc.h` JFORL/JLOOP/JFUNCF |

### Table 3: JIT tiering strategies

| Strategy | Peak perf | Warmup | Deopt complexity | Fit for static types |
|---|---|---|---|---|
| Meta-tracing (PyPy) | high (for dynamic) | seconds | high (trace invalidation) | poor: records interpreter noise designed for dynamic langs |
| Partial eval of AST (Truffle) | highest | seconds | high | unnecessary: our bytecode is already specialized |
| Trace compiler (LuaJIT) | highest | ms | high (guards, snapshots, side traces) | guards nearly vanish; trace-state machinery remains |
| Copy-and-patch (CPython 3.13+) | ~LLVM -O0 | us per unit | near-zero for us | ideal: stencils keyed on static types |
| SSA method JIT via Cranelift | high | ms per function | near-zero for us | ideal: typed SSA lowers 1:1 to CLIF |

### Table 4: Value representation

| Scheme | Slot size | Unboxed ints | Type-check cost | Used by |
|---|---|---|---|---|
| Boxed pointers | 8B + heap object | no | pointer deref | CPython |
| NaN boxing GC64 | 8B | DUALNUM ints only | one shift (`lj_obj.h` `itype`) | LuaJIT |
| Smi tag + pointer compression | 4B in 4GB cage | 31-bit | tag bit test | V8 |
| Typed registers (static types) | 4/8B per type | any width | none at runtime | DreamScript; LuaJIT IR `IRT_*` at trace time |

### Table 5: Memory management

| Scheme | Pauses | Allocator traffic | VM complexity | Fit for linear/immutable |
|---|---|---|---|---|
| Generational copying GC | small, moving | high (copying + barriers) | high (barriers, safepoints, JIT maps) | good throughput, heavy machinery |
| Immix mark-region | short | medium (bump) | medium | decent; non-moving happy path |
| Perceus RC + reuse | none | lowest (reuse makes alloc ~free) | low-medium (compile-time dup/drop) | best: linearity minimizes RC ops (Koka-proven) |
| ARC + EA (Inko/Swift) | none | medium (EA rescues ~50%) | low | good, but EA heuristic; linearity makes it exact |
| Per-frame arena | none (bulk reset) | lowest for temporaries | low | perfect for temps; escape proof = linearity |

### Table 6: Generics implementation

| Strategy | Runtime cost | Code size | Semantics | Author-fit |
|---|---|---|---|---|
| Monomorphization (C++/Rust) | zero | bloat, slow compiles | invites specialization rules | poor (explicitly disliked) |
| Dictionary passing (OCaml/GHC) | boxed args + indirect call | one body | coherent | good; unboxed dicts in registers mitigate |
| Erasure (Gleam/BEAM, Inko 1.x) | full dynamic dispatch | smallest | dynamic | poor fit for perf goals |
| JIT devirt/spec (V8/Dart) | guards + deopt | grows at runtime | speculative | unnecessary: types known statically |
| Compiler inlining of generic calls; no language-level specialization | zero when inlined | moderate | one meaning per function | best: mono perf, dictionary semantics, no overlap rules |

### Table 7: Error representation

| Model | Success-path cost | Error-path cost | Metadata | Fit |
|---|---|---|---|---|
| Exceptions + tables (Java/C++ EH) | ~zero | high (unwind) | tables in binary | hides errors from types; rejected by author |
| Boxed Result (dynamic VMs) | allocation + tags | allocation | GC pressure | CPython/Koka-old; avoid |
| Unboxed Result in registers (DreamScript) | one flagged branch | same branch | none | tracked errors in types; recommended |
| Effects + one-shot continuations (Koka) | handler-entry frame capture | capture cost | handler tables | unifies async/errors; compatible with TCO rules |

## (d) Viable execution pipelines for DreamScript

### Pipeline A: Typed register VM only (interpreter-maximalism)

Design: 32-bit register bytecode with statically typed, representation-specialized opcodes (LuaJIT ADDVN style, but guaranteed by the type system); unboxed frame registers; computed-goto or tail-call dispatch; guaranteed TCO via frame reuse (Lua `luaD_pretailcall` model); Perceus RC + reuse for boxed values; per-frame bump arenas for temporaries; unboxed Result/ADT across registers; mmap-able bytecode files; uop-level instruction set with profile-driven superinstructions.
Complexity: lowest of the serious options — no JIT, no deopt, no GC barriers; roughly one strong engineer-year to a solid VM.
Performance: estimated 3-10x of C on scalar code (dispatch and a little indirection remain); zero warmup; deterministic latency; ideal for hot reload and debugging.
Tradeoffs: ceiling below compiled tiers; no cross-block optimization or vectorization unless done at bytecode-emit time (peephole only).
Best when: the language must be simple and predictable first; JIT is a later, additive step.

### Pipeline B: Typed register VM + copy-and-patch JIT (recommended core)

Design: Pipeline A as tier 1; tier 2 = copy-and-patch: one stencil per uop generated offline by LLVM from a master template (CPython `Tools/jit/template.c` model), patched with static constants/types/addresses only — zero runtime guards, zero deopt paths; compilation units = linear bytecode traces along edges (loop bodies, like CPython tier-2 traces), triggered by backoff counters (`_JIT` uop pattern); J*-style opcodes stitch compiled units into bytecode (LuaJIT trick) so tier transitions cost one dispatch.
Complexity: medium — a stencil build pipeline plus patcher; CPython proves the tooling (LLVM 21 as build-time dependency only); no optimization passes to write.
Performance: estimated 1.5-3x of C; compile cost microseconds per unit (arXiv:2011.13127); memory = stencil library (a few MB) + patched code.
Tradeoffs: stencil library is per-architecture (x86-64, aarch64 first); no cross-unit optimization (inlining needs extra machinery — mitigated because calls are cheap and RC placement is compile-time, so the main win of inlining is smaller than in dynamic VMs).
Best when: baseline-compiler-class performance is required without building a compiler team.

### Pipeline C: Typed SSA IR + Cranelift optimizing tier

Design: tier 3 = lower the shared typed SSA (built from the same bytecode) into CLIF and compile with Cranelift; ISLE rules for DreamScript-specific rewrites (effect-directed reordering, buffer fusion, bounds-check elimination by linearity, unboxed ADT lowering into CLIF aggregates); Cranelift handles ISA selection, register allocation, and emission.
Complexity: medium-high — SSA construction and lowering; Cranelift is a large but maintained upstream dependency.
Performance: estimated 1-2x of C (Cranelift targets between -O0 and -O2; the egraph optimizer closes part of the gap); enables cross-block opts, inlining, and vectorization hooks.
Tradeoffs: ms-scale compile per function (fine as tier 3); large dependency; overkill for short scripts, which stay on tier 1.
Best when: long-running server or compute workloads matter; natural companion to Pipeline B (same SSA feeds both).

### Pipeline D: Wasm/native AOT deployment of the same typed IR (MoonBit-style)

Design: the typed SSA compiles to wasm 3.0 (`return_call` carries TCO; wasm-GC optional for host interop) or to native via Cranelift; the interpreter remains the dev/debug substrate; DreamScript "release build" = AOT artifact.
Complexity: medium — a wasm emitter is far simpler than a JIT; native path reuses Pipeline C.
Performance: 1.5-2.5x of C in browsers/embedded (wasm overhead ~1.55x measured); native equals Pipeline C.
Tradeoffs: loses the instant edit-run loop unless used alongside the interpreter; engine feature support varies.
Best when: portability and sandboxing are product requirements; also the cheapest route to a "true compiler backend" story.

### Pipeline E: Trace compiler with static types (research bet)

Design: record hot loops from tier 1 into typed SSA; with static types the recorder emits no type/shape guards — only bounds (avoidable by linearity) and effect boundaries; snapshots only for debugger and effect entry; side traces rare.
Complexity: high — recorder, loop detection, snapshot mapping, trace linking; LuaJIT is famously intricate, and much of that intricacy is guard/snapshot machinery whose purpose our type system pre-empts.
Performance: potentially the highest (1-2x of C, sometimes parity) because cross-statement optimization within loops is automatic.
Tradeoffs: all of LuaJIT's maintenance burden with less of its justification — Pipelines B+C already cover most of the gap with less machinery; trace-exit cliffs (Cloudflare's documented inconsistency) are a real user-experience hazard.
Best when: numeric kernel performance is the product and nothing else suffices; otherwise skip.

### Recommendation and sequencing

Implement Pipeline A on a uop instruction set with explicit effects from day one (the CPython `bytecodes.c` discipline), and build the typed-SSA lowering early so B/C/D stay reachable from the same core. Add B (copy-and-patch) once the uop set stabilizes — its stencils and the interpreter's uops are the same code by construction. Add C (Cranelift) or D (wasm) when peak performance or deployment demands it; skip E unless numeric-kernel marketing requires it. This ordering matches the evidence: static types remove the guard/deopt/speculation layer that dominates both the complexity and the runtime cost of every dynamic tiering system surveyed, so DreamScript's tiers are strictly simpler than their dynamic counterparts, and the interpreter itself starts from a 3-10x-of-C baseline rather than 25-50x behind. Two design decisions to lock early because everything above depends on them: (1) the calling convention places callee frame slots over caller argument slots (TCO + cheap calls), and (2) the bytecode is position-independent, fixed-width, and index-based (mmap caching, stencil patching, J*-style stitching).

## (e) Per-system quick reference (what to read where)

| System | Bytecode/IR | Dispatch | TCO | JIT | Memory |
|---|---|---|---|---|---|
| Lua 5.5 | 32-bit register ops, 103 opcodes (`lopcodes.h`) | switch/computed goto (`ljumptab.h`) | CallInfo reuse (`ldo.c`) | none | incremental tracing GC (`lgc.c`) |
| LuaJIT | 32-bit typed register ops (`lj_bc.h`) | asm computed threading + J* ops (`lj_vm.S`) | CALLT/CALLMT arg slide | tracing, guards+snapshots (`lj_record.c`, `lj_snap.c`, `lj_jit.h`) | incremental GC, NaN-boxed values (`lj_obj.h`) |
| CPython 3.13-3.15 | stack codeunits -> uops (`bytecodes.c`) | computed goto or musttail handlers (`ceval.c`, `ceval_macros.h`) | none | copy-and-patch stencils (`Tools/jit/`) | refcounted PyObjects |
| V8 | Ignition register bytecode + feedback | inline-cached handlers | none | Sparkplug/Maglev/TurboFan | generational compacting GC |
| PyPy | RPython bytecode | JIT-driven | yes (mostly) | meta-tracing | generational GC per GC strategy |
| Truffle | ASTs | partial evaluation | language-dependent | Graal PE | host GC |
| Inko 2.x | MIR -> LLVM (no bytecode; `compiler/src/mir/`, `llvm/`) | n/a | n/a | none (AOT) | ARC + interprocedural EA (`rt/src/arc_without_weak.rs`, `mir/escape.rs`) |
| Koka | Core -> C | C compiler | label loops (`FromCore.hs`) | none (C toolchain) | Perceus RC + reuse (`Parc.hs`, `kklib.h`) |
| Cranelift | CLIF typed SSA (`docs/ir.md`) | n/a | n/a (target) | n/a (is the JIT backend) | n/a |
| MoonBit | typed core IR | n/a | tail-recursion -> loop | n/a (AOT wasm/JS/native) | GC or RC per target |

## References

Local clones (untrusted, read-only; clean up after research): Z:/Temp/plr3/{lua, luajit, cpython, inko, koka, wasmtime, moonbit-core}.

Key source files cited above: luajit/src/lj_bc.h (bytecode format, typed opcodes, CALLT/CALLMT, J* ops); luajit/src/lj_ir.h (64-bit SSA IRIns, guards, SLOAD modes, ADDOV, IRFLDEF field offsets, IRT types); luajit/src/lj_obj.h (NaN boxing GC64 encoding, 47-bit pointers, write-barrier rules); luajit/src/lj_frame.h (frame layout on the value stack, continuation frames); luajit/src/lj_jit.h (SnapShot/SnapEntry); luajit/src/lj_dispatch.h (hot counters); lua/ldo.c (luaD_pretailcall, CIST_TAIL); lua/lvm.c + ljumptab.h (computed goto, trap hooks); lua/lopcodes.h (field sizes); cpython/Python/bytecodes.c (DSL, macro ops, _BINARY_OP_ADD_INT, _JIT, tier2 uops); cpython/Python/ceval_macros.h + ceval.c (tail-call interpreter, musttail/preserve_none); cpython/Tools/jit/ (PEP 744 stencil JIT, template.c, _stencils.py, README.md); inko/rt/src/arc_without_weak.rs (ARC), inko/compiler/src/mir/escape.rs + inline.rs (interprocedural EA); koka/src/Backend/C/FromCore.hs (tryTailCall, kk__tailcall goto loops, genOverride), koka/src/Backend/C/Parc.hs + ParcReuse.hs (Perceus codegen, reuse ordering), koka/kklib/include/kklib.h (kk_block_alloc_at, mimalloc thread heaps, drop_reuse); wasmtime/cranelift/docs/ir.md (CLIF), cranelift/codegen/src/opts/README.md + *.isle (directional ISLE rules, subsume), cranelift/codegen/src/egraph/ (cost.rs, elaborate.rs).

Papers, posts, and docs:
- Copy-and-Patch Compilation, Xu & Kjolstad, OOPSLA 2021: https://arxiv.org/abs/2011.13127
- PEP 744, CPython JIT: https://peps.python.org/pep-0744/ ; PEP 659, specializing adaptive interpreter: https://peps.python.org/pep-0659/
- V8: Sparkplug https://v8.dev/blog/sparkplug ; Maglev https://v8.dev/blog/maglev ; Ignition https://v8.dev/blog/ignition ; Leaptiering https://v8.dev/blog/leaptiering ; GC https://v8.dev/blog/trash-talk ; wasm tail calls https://v8.dev/blog/wasm-tail-call
- PyPy JIT and performance: https://www.pypy.org/performance.html
- One VM to rule them all (Truffle), Onward! 2013: https://dl.acm.org/doi/10.1145/2509578.2509581 ; review: https://blog.acolyer.org/2014/11/18/one-vm-to-rule-them-all/
- Perceus: Garbage Free Reference Counting with Reuse, PLDI 2021: https://www.microsoft.com/en-us/research/publication/perceus-garbage-free-reference-counting-with-reuse/
- Inko: A decade of developing a programming language https://yorickpeterse.com/articles/a-decade-of-developing-a-programming-language/ ; Friendship ended with the garbage collector https://yorickpeterse.com/articles/friendship-ended-with-the-garbage-collector/ ; 0.20.0 escape analysis https://inko-lang.org/news/inko-0-20-0-reducing-heap-allocations-by-50 ; 0.10.0 GC removal https://inko-lang.org/news/inko-0-10-0-released
- MoonBit JS backend and benchmarks: https://www.moonbitlang.com/blog/js-support
- Wasm 3.0 (tail calls standardized): https://webassembly.org/news/2025-09-17-wasm-3.0 ; tail-call proposal: https://github.com/webassembly/tail-call
- Not So Fast: Analyzing the Performance of WebAssembly vs Native Code, IMC 2019: https://ar5iv.labs.arxiv.org/html/1901.09056
- C vs Lua vs LuaJIT vs Java measurements: https://eklausmeier.goip.de/blog/2016/04-05-performance-comparison-c-vs-lua-vs-luajit-vs-java ; LuaJIT trace stability at Cloudflare: https://blog.cloudflare.com/more-consistent-luajit-performance
- MLIR: https://mlir.llvm.org/ ; Mojo/MLIR pipeline summaries: https://arxiv.org/html/2606.16059v1
- Ruby TCO status (no guaranteed TCO; YJIT/ZJIT): http://blog.tdg5.com/tail-call-optimization-in-ruby-background/ and ruby-core ZJIT discussions (2025)
- Author taste notes: C:/programs/me/lxl66566.github.io/src/coding/philosophy_of_PL.md (anti-specialization, Result errors, effects)
