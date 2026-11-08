# DreamScript 研究笔记 03：运行时、IR 与 JIT 的执行策略

日期：2026-11-04。范围：面向一门解释执行、静态类型、线性/仿射、默认不可变、保证尾调用优化并追求超高性能的语言，研究执行策略、IR 设计、JIT 与解释器级性能。姊妹笔记列出的 DreamScript 硬性要求：解释执行（源文件 -> 内部 IR/字节码，这是预期之内的）、静态线性/仿射类型、默认不可变、编译器能做的一切加上解释器特有的优化、保证 TCO、可选 JIT。作者口味：不要重量级语法糖，不要 C++ 风格的特化语义（见 philosophy_of_PL.md 的“特化”与“错误处理”两节，那里主张集合论类型、Result 风格错误，并对代数效应/Koka 有兴趣）。

一手资料是本地克隆（全部在 `Z:/Temp/plr3/` 下，不可信，只读）：`luajit/src/lj_bc.h`、`lj_ir.h`、`lj_obj.h`、`lj_frame.h`、`lj_jit.h`、`lj_dispatch.h`。`lua/ldo.c`、`lvm.c`、`lopcodes.h`、`ljumptab.h`。`cpython/Python/bytecodes.c`、`ceval.c`、`ceval_macros.h`、`Tools/jit/template.c`、`Tools/jit/README.md`。`inko/rt/src/arc_without_weak.rs`、`inko/compiler/src/mir/escape.rs`、`inko/compiler/src/mir/inline.rs`。`koka/src/Backend/C/FromCore.hs`、`koka/src/Backend/C/Parc.hs`、`koka/src/Backend/C/ParcReuse.hs`、`koka/kklib/include/kklib.h`。`wasmtime/cranelift/docs/ir.md`、`cranelift/codegen/src/opts/README.md` 与 `*.isle`、`cranelift/codegen/src/egraph/`。下文引用的仓库路径都相对这些根目录。

## (a) 概要

1. 字节码决定性地胜过树遍历，寄存器字节码又胜过栈字节码。Lua 5.5 与 LuaJIT 使用 32 位寄存器式指令（`lua/lopcodes.h`：7 位 opcode、8 位 A、8 位 B/C、17 位 Bx，`luajit/src/lj_bc.h`：ABC 与 AD 格式，解码就是纯移位）。CPython 的栈机每个操作要执行多得多的指令，还要靠 PEP 659 那套相当可观的专门化工作，才换来约 1.25x 的提升。
2. LuaJIT 已经在小尺度上演示了“静态类型解释器”这一招。它的字节码带有按类型专门化的 opcode（`lj_bc.h` 里的 ADDVN/ADDNV/ADDVV、ISEQV/ISEQS/ISEQN/ISEQP），因为它的编译器能够推断操作数类型。DreamScript 从静态类型系统无偿且无条件地拿到这一点，还能让每个槽位保持拆箱，因为每个寄存器的类型是编译期常量，无需运行期猜测。
3. 调研到的每个 JIT，其机器的主体都是运行期的类型/形状猜测：LuaJIT 的守卫（`lj_ir.h` 里的 `IRT_GUARD`）、快照（`lj_jit.h` 里的 `SnapShot`/`SnapEntry`）与侧向轨迹。Maglev 的 map 检查与去优化帧。CPython 的 `_SPECIALIZE_*` 加守卫 uop。Truffle 的 `transferToInterpreter`。静态类型语言几乎消除了全部这些机器：没有隐藏类，字段访问跳过内联缓存（偏移已知，参见 LuaJIT `IRFLDEF` 用 `offsetof` 做字段加载），没有类型守卫。去优化只剩调试器、栈溢出与效应入口这三件事。对 DreamScript 的设计而言，这是最大的一个乘数。
4. 分层：最贴合的处理管线是类型化寄存器 VM（tier 1）加 copy-and-patch JIT（tier 2）。Copy-and-Patch（Xu & Kjolstad，arXiv:2011.13127）产出的代码比 LLVM -O0 快约 14%，编译速度比 LLVM 快两个数量级。CPython 3.13+ 已经发布该方案（PEP 744，`Tools/jit/template.c`）。有了静态类型，stencil 上零运行期检查、零退出，洞只放静态常量、地址与布局元数据。
5. 分派：计算 goto（Lua 的 `ljumptab.h`，CPython 的 `USE_COMPUTED_GOTOS`），或 C 语言的尾调用线索化（CPython 主干的 `_Py_TAIL_CALL_INTERP`，用 `musttail` + `preserve_none`，见 `Python/ceval_macros.h`）。两种做法都在生产环境里验证过。超级指令应当依据 profile 数据来组合（CPython 在 `bytecodes.c` 里对 `macro(BINARY_OP)` 的展开就是要照抄的范式）。
6. 值表示：有了静态类型，NaN boxing 直接跳过（LuaJIT 在 `lj_obj.h` 里的 GC64 格式是 13 位 NaN 前缀、第 47 位的 4 位 tag、47 位指针，类型检查就是一次 `it64 >> 47` 移位）。类型化寄存器直接存放拆箱的 i32/i64/f64/bool。ADT 与 Result 是拆箱的 tag 加 payload 对，分布在多个寄存器里。堆分配只发生在装箱值或共享值上。动态 VM 只在 JIT 轨迹内拿到这套表示：LuaJIT 内部的静态类型轨迹用的正是它（`lj_ir.h` 里的 `IRT_INT/I64/NUM`）。我们的解释器要在全域长期跑到这个状态，LuaJIT 仅能在轨迹内达到。
7. 线性类型把逃逸分析从启发式变成编译期事实。Inko 需要整程序跨过程逃逸分析加内联，才能把 47-58% 的堆分配提升到栈上（`inko/compiler/src/mir/escape.rs`，Inko 0.20.0 发行说明，HTTP 解析上 2-2.2x 提速）。DreamScript 无偿拿到 100% 的非移动临时值，包括解释器帧内 ADT 的标量替换。
8. 内存管理：Perceus 风格的精确引用计数加块复用（Koka，`kklib.h`：`kk_block_alloc_at(reuse, ...)`、`kk_block_check_drop_reuse`）与线性值最匹配。线性的一次性使用语义让大部分 dup/drop 可以被静态消除，RC 的开销于是收缩到显式的 clone/move 边界。再为解释器临时值叠加按帧的 bump arena，帧弹出时整体复位。线性性禁止静默逃逸，这一步因此可靠。通用追踪式 GC 初期留在设计之外。Inko 自己的历史很有教益：先用并行 immix，0.10.0 里彻底移除，转向所有权加 ARC（`inko/rt/src/arc_without_weak.rs`）。
9. 保证 TCO 在寄存器 VM 里既便宜又精确。Lua 的 `luaD_pretailcall`（`lua/ldo.c`）复用 CallInfo，并把被调方与实参沿栈向下移动。Koka 的 C 后端把尾调用降级成标签循环（`FromCore.hs` 里的 `goto kk__tailcall;`）。Wasm 3.0 标准化了 `return_call`（2025 年 9 月），因此 wasm 后端从平台处继承 TCO。Ruby 的 YJIT/ZJIT 在 2025-2026 仍然不保证 TCO。如果我们从第一天起就在规范里保证它，这就是 DreamScript 的一个真实差异点。
10. IR：类型化 SSA 核心 IR（Cranelift 风格，配 ISLE 式有方向重写规则与带代价抽取的 e-graph，`cranelift/codegen/src/opts/README.md`、`codegen/src/egraph/`）服务优化层与 wasm/原生后端（MoonBit 的管线：类型化核心 IR -> wasm/JS/原生，编译器级迭代器融合，尾递归转循环）。
11. 性能天花板的证据：LuaJIT 在数值循环上大约是 C 的 2-3x（独立测量，Cloudflare 记录了轨迹性能的稳定性，以及退出时的悬崖行为）。普通 Lua 比 LuaJIT 慢约 25x。PyPy 需要数秒预热，离开顺风路径时可能比 CPython 慢 2x。copy-and-patch 约等于 LLVM -O0 的质量。wasm 约为原生的 1.55x（Chrome，IMC 2019）。DreamScript 的现实目标：纯解释器为 C 的 3-10x，加 copy-and-patch 为 1.5-3x，加优化层为 1-2x，而且完全没有动态 VM 那种去优化的悬崖。
12. 推荐管线（细节见 d 节）：tier 1 为类型化寄存器字节码 VM，配拆箱寄存器、保证 TCO、Perceus RC 加复用、按帧 arena。tier 2 为以同一套 uop 为键的 copy-and-patch JIT。tier 3/部署为 Cranelift 支撑的类型化 SSA 优化层或 wasm AOT。所有层共用同一份字节码 -> SSA 降级，复杂度摊到三件产物上。

## (b) 各技术深入分析

### 1. VM 架构：树遍历对字节码，栈对寄存器，分派，格式

树遍历解释器（早期 CRuby、CPython 的 `ast.eval`、字节码之前的 Dart）每次求值都要重新付出解析与重新分派的代价，没有稳定的单元可供缓存或插桩，JIT 也没有东西可编译。本次调研里每一个高性能解释器都执行字节码。字节码还产出了持久工件，从而支撑字节码文件缓存与 mmap 启动、插桩钩子、调试以及分层。

栈式字节码与寄存器字节码。栈机编码密度高，但执行的指令更多（每个操作数都要压栈/出栈）。寄存器机在操作数里直接命名虚拟寄存器，指令数更少，代码尺寸略大。Lua 的格式（`lua/lopcodes.h`）：32 位指令，`SIZE_OP=7`、`SIZE_A=8`、`SIZE_B/C=8`、`SIZE_k=1`、`SIZE_Bx=17`、`SIZE_sJ=25`。`lvm.c` 的主循环每帧算一次 `RA(i) = base + GETARG_A(i)`，`base` 是本地变量，因此槽位访问就是一次带索引的加载。LuaJIT（`luajit/src/lj_bc.h`）用同样的 32 位定长布局，只有两种格式：`|B|C|A|OP|`（ABC）与 `|D|A|OP|`（AD，16 位 D）。解码宏为 `bc_op(i) = i & 0xff`、`bc_a(i) = (i>>8)&0xff` 等。有两处格式层的小技巧值得照抄。跳转偏移带偏置（`BCBIAS_J=0x8000`），有符号跳转于是变成一次纯无符号减法。opcode 表里的静态断言把彼此取反的指令配成对（枚举里 `BC_ISEQV^1 == BC_ISNEV`、`BC_ISLT^3 == BC_ISGT`），于是 JIT 与解释器可以用一次 XOR 取反一次比较。这些微细节在分派密集的代码里会累积成收益。

字节码里的类型化 opcode。`lj_bc.h` 的 `BCDEF` 表是关键先例。算术发成 ADDVN/ADDNV/ADDVV（值-数字、数字-值、值-值），比较发成 ISEQV/ISEQS/ISEQN/ISEQP（变量/字符串/数字/原语），常量加载发成 KSTR/KNUM/KSHORT/KPRI，表访问发成 TGETS/TGETB/TGETV，函数序言发成 FUNCF（定长参数）/FUNCV（可变参数）/FUNCC（C 函数）。每个 opcode 的操作数模式编码在并行表里（`lj_bc_mode[]`：每个 opcode 的 A/B/C 模式加元方法索引），记录器与解释器用移位查询该表。VM 的 opcode 携带了编译器推断出的类型。静态类型的 DreamScript 可以严格走得更远：每个操作数的类型在发出字节码时就已经确定，因此 opcode 按表示专门化（i32、i64、f64），没有回退路径，“V”（动态值）这类 opcode 变体根本不存在。

分派机制，附源码证据：

- switch：可移植的基线。在现代 CPU 上，当 opcode 频率偏斜时也很有竞争力（分支预测器能学到分布），代价是一次边界检查加一次查表。
- 计算 goto / 线索化分派：Lua 的 `lvm.c` 在 `LUA_USE_JUMPTABLE` 下引入 `ljumptab.h`，`vmdispatch(o)` 展开为 `goto *vmestimate[o]` 式的间接线索化。`vmfetch()` 是两条指令的取指（`i = *(pc++)` 加一次 trap 检查）。CPython 用 `USE_COMPUTED_GOTOS` 把同一机制做成可选项（非计算 goto 的构建是回退路径：`#if USE_COMPUTED_GOTOS && !_Py_TAIL_CALL_INTERP`）。
- 尾调用线索化：CPython 主干包含完整的尾调用解释器。`_Py_TAIL_CALL_INTERP`（`Python/ceval_macros.h`）声明 `typedef PyObject *(Py_PRESERVE_NONE_CC *py_tail_call_funcptr)(TAIL_CALL_PARAMS)`，处理器携带 `(frame, stack_pointer, tstate, next_instr, instruction_funcptr_table, oparg)`，结尾 `return _TAIL_CALL_<op>(...)`。`Py_MUSTTAIL` 在 clang/GCC 下是 `__attribute__((musttail))`，MSVC 2026+ 下是 `[[msvc::musttail]]`，调用约定为 `__attribute__((preserve_none))`。好处：每个指令处理器都是独立函数，C 编译器可以各自分配寄存器（CPython 那种共享状态的 switch 循环会遭遇寄存器压力），分派只是一次间接调用。代价：工具链要求严格，调试 profile 也不寻常。
- 轨迹缝合分派（LuaJIT）：opcode 集合里包含 JFORL/JLOOP/JITERL/JFUNCF 这些变体（`lj_bc.h` 注释 “Loops and branches. I/J = interp/JIT”），它们只在 JIT 编译的轨迹内出现。普通解释器取到这类 opcode 时，分派直接落到机器码，循环回边上的层级切换于是免费。对任何分层方案来说，这是值得照抄的钩子。

超级指令与宏操作。CPython 的 `bytecodes.c` DSL 以静态方式组合它们，例如 `macro(BINARY_OP) = _SPECIALIZE_BINARY_OP + _RECORD_TOS_TYPE + _RECORD_NOS_TYPE + unused/4 + _BINARY_OP + POP_TOP + POP_TOP;`。一条取来的指令展开成一整串微操作。cases 生成器为 tier 1 生成 `Python/generated_cases.c.h`（约 13.8k 行），为 tier 2 生成 `Python/executor_cases.c.h`。该 DSL 还标注了优化器需要的语义：`pure op(...)` 注解、`tier2 op(...)` 这类带显式栈效果的 uop（如 `_BINARY_OP_ADD_INT, (left, right -- res, l, r)`），以及用于展开变体的 `replicate(2:4)`（见 `SWAP`）。LuaJIT 把常见序列折成单条 opcode（ISNEXT 迭代器初始化、KSHORT 喂给类型化算术、RETM 多次返回）。给 DreamScript 的建议：从第一天起就把指令集定义成带显式效果的微操作（CPython 的纪律），再在汇编器里组合超级指令。同一套 uop 之后一对一成为 copy-and-patch 的 stencil 集合。

字节码格式设计（LuaJIT 对 Wasm）。LuaJIT 的内存内字节码是定长 32 位、宿主字节序的，操作数是寄存器索引与常量表索引。它位置无关，可以直接 mmap，另有可移植的序列化转储（`lj_bcdump.h`）。Wasm 用 LEB128 变长整数、带类型的值栈、分节模块，压缩更好，随机访问更差，执行前必须解码。给 DreamScript 的选择：定长 32 位指令、基于索引的操作数、模块头带版本/类型/常量表、加一个 IR 版本校验和。这样能得到廉价的 `load`（一次 mmap，无需重定位），支撑字节码文件缓存（`dreamscript run foo.dsbc`），也让 copy-and-patch 的修补器可以直接跳进函数体中间。

栈帧设计对比（调用栈由什么承载）。三种设计占主导，这个选择决定了 TCO 成本、去优化成本与异步成本：

- 独立的调用信息记录（Lua 5.4/5.5）：数据栈只放值，每次调用把一条 `CallInfo`（函数位置、保存的 pc、top、状态标志）压入独立数组。`lvm.c` 把 `ci`、`base`、`pc` 放在 C 局部变量里，栈重新分配后用 `updatebase(ci)` 重新同步（`vmfetch()` 里的 `trap` 标志负责钩子与重新分配）。优点：帧内省与可变参数处理便宜。缺点：每次调用要维护两个结构。
- 栈帧内联在值栈里（LuaJIT）：`lj_frame.h` 记录了布局。对 LJ_FR2（64 位 PC）是 `[func | PC/delta/ft] | slots...`，帧类型打包在尺寸字的低位里（`FRAME_LUA`、`FRAME_C`、`FRAME_CONT`、`FRAME_PCALL`，等等）。pcall 这类构造的续体帧紧邻存放。`frame_delta(f)` 用一次移位还原帧尺寸，这正是 CALLT/CALLMT 尾调用用来把参数下移的量。优点：每次调用的工作量最小，对 JIT 友好（栈本来就是机器字数组）。缺点：值栈必须按函数预先定好尺寸（LuaJIT 为每个 proto 发出帧尺寸元数据）。
- 解释器帧作为一等对象（CPython 的 `_PyInterpreterFrame`）：每个帧持有局部变量、栈指针、instr_ptr、return_offset、owner。tier 1 循环、tier 2 执行器与 JIT 操作同一个结构体，CPython 的层级切换之所以可能，全靠这一点（处理器把 `frame, stack_pointer` 作为参数，见 `Python/ceval_macros.h` 的 TAIL_CALL_PARAMS）。优点：插桩有统一底座。缺点：每次调用开销比 Lua 重。

给 DreamScript 的结论：把帧内联进类型化值栈（LuaJIT 风格）才是对的底座。每个函数的帧布局是静态已知的（每个槽位的类型都在代码对象里），因此一个帧就是一段连续的带类型槽位加一个小的元数据字（返回地址或返回表索引、帧尺寸、TCO 链标志）。这一个决定让解释器调用变便宜，让 TCO 变成一次指针移动，让 copy-and-patch 的 stencil ABI 只需一条约定（槽位是机器字宽且带类型）。调试器物化只需遍历元数据字。

具体例子。考虑一个类 DreamScript 的函数（示意用伪代码）：

```
fn sum_to(n: i64) -> i64 {
  let mut acc = 0i64
  while n > 0 { acc += n; n -= 1 }
  acc
}
```

LuaJIT 风格编码（动态 VM，槽位里带 tag）与 DreamScript 类型化编码的对比：

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

类型化编码在解释器里每次迭代执行 3 个机器级操作（比较、加、减）外加分派。动态编码在每次算术与比较上还要额外维护并检查 tag，这正是静态类型系统删掉的开销。

### 2. 分层与 JIT 策略

V8（Ignition -> Sparkplug -> Maglev -> TurboFan）。Ignition 是寄存器解释器，它的反馈向量喂给内联缓存。Sparkplug 是基线 JIT，与字节码一对一编译、不做优化，但刻意保留解释器帧布局，使去优化与栈内省保持平凡（v8.dev/blog/sparkplug：比只有 Ignition 快约 41% Speedometer）。Maglev（v8.dev/blog/maglev）是建在 CFG 上的 SSA 中间层（TurboFan 的节点海走另一条路线）：一遍前置扫描（分支目标、循环赋值、活跃性），一遍单向 SSA 构建（循环 phi 预创建），已知节点信息跟踪，表示选择，简单的前向寄存器分配器，直接宏汇编发射。它靠运行期反馈做专门化，例如先发一次 map 检查再发定偏移字段加载，并把稳定假设注册为依赖。编译速度比 Sparkplug 慢约 10x，比 TurboFan 快约 10x，实测收益约 -3.5% JetStream、-10% Speedometer 能耗。TurboFan 是峰值层，带完整内联与基于反馈的类型推断。Leaptiering（v8.dev/blog/leaptiering）后来把每函数的分派表条目从 JSFunction 上解耦，避免失效风暴。教训：三层加 leaptiering 之所以存在，是因为动态类型强制做猜测。每一层增加的复杂度，主体都是守卫与去优化记账，而这一层在 DreamScript 里整块消失。

PyPy 元追踪（pypy.org/performance.html）。解释器用 RPython 写成，JIT 观察热的*解释器*循环并记录解释器的行为（元追踪），于是同一个 JIT 服务多个解释器。已记录的特征：预热“至少几秒”。0.2 秒以下的脚本与单元测试永远得不到收益。`sys._getframe`、追踪钩子这类内省会在其作用域内停用 JIT。依赖大量运行期支持的代码（大整数、某些排序）最多比 CPython 慢约 2x。对动态语言来说，元追踪是通往强 JIT 最便宜的路线。对静态语言它就是错的交易：解释器本身已经按类型导向，追踪会把大部分记录预算花在 tag 记账上，而我们的 tier 1 从一开始就省掉这些记账。

Truffle/Graal 部分求值（“One VM to rule them all”，Würthinger 等，Onward! 2013，dl.acm.org/doi/10.1145/2509578.2509581，可访问的综述见 blog.acolyer.org/2014/11/18/one-vm-to-rule-them-all）。语言 AST 用 Java 写出。Graal 针对热点的 guest 方法对解释器做部分求值，把解释器分派、节点类型与记账常量折叠掉，产出“与最高性能实现相当、并且常常超过它们”的专门化机器码。猜测用 `transferToInterpreter()` 去优化来守住，AST 会自我重写（例如多态节点变成内联缓存）并重新编译。架构洞见在于部分求值能抹掉解释器开销。静态类型在字节码层面就把这一点交给 DreamScript，无需 Java 级的宿主 VM：我们的字节码 opcode 就是“已经部分求值过”的程序表示。

LuaJIT 轨迹编译器（源码：`lj_record.c` 记录器、`lj_fold.c` 常量折叠、`lj_asm.c` 线性扫描寄存器分配、`lj_snap.c` 快照，结构在 `lj_jit.h`）。管线细节：（1）热度，`hotcount[]` 里按字节码位置计数（`lj_dispatch.h`：64 个条目，用 `(pc>>2)&63` 索引，默认触发阈值 `HOTCOUNT_LOOP=2` 次命中）。（2）记录，重新读取 BC 流并发出 SSA IR。每个 `IRIns` 是 64 位（`lj_ir.h`：两个 16 位操作数引用 `op1/op2`，加上 8 位 opcode 与 8 位 `IRType`，另有一条用于 CSE 的 `prev` 链）。`TRef` 操作数在高字节里标注自己的 IR 类型，类型检查于是变成即时比较。（3）优化，常量折叠，经 `prev` 哈希链做 CSE，展开一次迭代并匹配 PHI 来检测循环。（4）发射，线性扫描分配加机器码发射，然后安装轨迹并通过 J* opcode 让它可从分派到达。守卫是普通 IR 指令，只是打了 `IRT_GUARD` 标记（LT/GE/ULE/EQ、ABC 数组边界检查、带 `IRSLOAD_TYPECHECK` 的 SLOAD）。每个守卫对应一个由 `SnapShot` 覆盖的退出（`lj_jit.h`：`struct SnapShot { uint32 mapofs; IRRef1 ref; uint16 mcofs; uint8 nslots, topslot, nent, count; }` 与 `SnapEntry = slot<<24 | flags | ref`），它重建解释器状态（栈槽、帧链、PC），使执行可以在字节码任意位置恢复。每个退出的 `count` 喂给侧向轨迹的晋升。还有一点值得注意：`ADDOV/SUBOV/MULOV`（`lj_ir.h` “Overflow-checking arithmetic ops”）是带检查的整数算术，失败即退出轨迹。若我们支持溢出时退回任意精度，这是静态类型下唯一幸存的守卫类。建议：把溢出定义为 panic/效应（Koka 风格），让快路径零检查，把带检查的算术作为显式库函数提供。

copy-and-patch（Xu & Kjolstad，arXiv:2011.13127，CPython 实现为 PEP 744，`Tools/jit/`）。这里的“编译器库”是一组预编译的二进制 stencil，即 LLVM 从主模板发出的机器码片段，每片都带着留给运行期常量、地址与类型元数据的洞（重定位）。编译等于按 uop 序列拷贝 stencil 再补洞。实测编译成本比 LLVM -O0 低两个数量级，产物比 LLVM -O0 快约 14%（SQL 编译器跑 TPC-H，比解释执行快约 10x）。wasm 变体编译比 V8 的 Liftoff 快 4.9-6.5x，产物快 39-63%（Coremark、PolyBenchC）。CPython 把它作为 tier 3 发布：`Tools/jit/template.c` 是主模板（它 include 真正的解释器头文件，于是每个 stencil 字面上就是某个处理器函数体的拷贝）。`Tools/jit/_stencils.py` 从目标文件重定位里抽出洞，`Tools/jit/_writer.py` 负责补洞。tier 2 优化器的轨迹通过 `_JIT` uop 触发编译（`bytecodes.c` 里先 `backoff_counter_triggers(counter)` 再 `_PyJit_TryInitializeTracing`）。入口落在 `JUMP_BACKWARD_JIT` / `RESUME_CHECK_JIT` 位置。当 `interp->jit` 存在时，`JUMP_BACKWARD` 字节码会在运行期被改写成 JIT 变体。给 DreamScript 的关键观察：CPython 的 stencil 里依然有运行期检查（`_BINARY_OP_ADD_INT` 里的 compact-PyLong 检查、布局断言），因为值是装箱的 `PyObject*`。我们的 stencil 无守卫：`add_i64` 的 stencil 字面上就是 `mov reg, reg; add reg, reg` 加上补洞的操作数槽位。所以对静态语言来说，copy-and-patch 的吸引力*比*对 Python 更大：我们付出它接近零的编译成本，并绕开了它全部的猜测负担。

OSR（栈上替换）。JVM 与 V8 需要 OSR 把正在跑的循环从解释器切到编译代码，切换发生在帧中间。CPython 在回边上插入执行器（`bytecodes.c` 里补 `ENTER_EXECUTOR`，`JUMP_BACKWARD` 选择 JIT 变体 opcode，见 `bytecodes.c`：`uint8_t desired = tstate->interp->jit ? JUMP_BACKWARD_JIT : JUMP_BACKWARD_NO_JIT;`，即解释器在装上 JIT 后会*改写自己的字节码*）。LuaJIT 避开显式 OSR：循环就是字节码位置，轨迹入口在回边上直接替换解释器循环。如果 DreamScript 以循环体（或小轨迹）为 JIT 单元并在回边进入，OSR 就退化为“开始解释/编译这个单元”，也省掉了帧变换机器。只要循环体保持小（在带 fold 风格写法的函数式语言里它们本来就小），这一点就成立。另外注意 CPython 的补丁式字节码模式：同一个 opcode 地址被改写成不同行为（JIT 与非 JIT、专门化与通用）。它便宜、缓存友好，也很贴合我们的热重载方案。

一个 copy-and-patch 的完整例子（示意）。对上面的类型化字节码 op `ADDI64 r1, r1, r0`，stencil 库在概念上为每类操作数各存一份 x86-64 模板，它们在构建期由一份 include 了 VM 头文件的主模板发出（CPython 的 `Tools/jit/template.c` 正是 include 了约 30 个解释器头文件，好让 stencil 成为处理器函数体的字面拷贝）：

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

编译该循环体就是：为每个 uop 选一个 stencil（操作数类别静态可知）、拼接、从代码对象补 u16/u64 的洞。过程是一次内存拷贝加若干字节写入，没有汇编器，没有寄存器分配器（stencil 之间约定固定的暂存与寄存器规范），也没有守卫。CPython 实测每个 stencil 亚微秒级。我们的 stencil 比 CPython 的更简单，因为其中没有类型检查、没有临时值的引用计数调用、没有错误路径胶水（错误经寄存器传播，可能失败的 op 其 stencil 以一次条件跳转结束，跳到该单元共享的错误桩）。

各系统的去优化策略，以及我们的。V8/CPython/LuaJIT/Truffle 都为退出维护解释器状态物化（快照映射、去优化帧、守卫生成节点上的抽象解释器状态）：

| 系统 | 守卫形式 | 退出成本 | 退出机制 |
|---|---|---|---|
| LuaJIT | 打 `IRT_GUARD` 标记的 IR 指令（类型/范围/相等） | 快照遍历，从 `SnapEntry` 映射恢复 nslots | 在字节码 PC 上退回解释器或进入侧向轨迹 |
| V8 Maglev/TurboFan | map 检查、Smi 检查、边界 | 从翻译状态构建去优化帧 | 重新进入解释器，可能重新优化 |
| CPython tier 2/3 | `_CHECK_*` uop、紧凑性检查 | 侧向退出到 tier 1 执行器 | 每条轨迹的全局侧向退出列表 |
| Truffle | 节点假设 | `transferToInterpreter()`、AST 重写 | 重新执行 AST，重新编译 |
| DreamScript | 数据路径零守卫，每函数一次栈上限检查 | 不适用（无守卫） | 仅调试器驱动的帧物化 |

DreamScript 只在四种情况下保留去优化：（i）调试器断点（从 JIT 帧元数据重建解释器帧，这是唯一值得建的部件）。（ii）栈溢出检查（帧尺寸静态已知，于是每个函数入口一次比较）。（iii）GC 安全点（RC 加 arena 下已取消，见第 6 节）。（iv）效应处理器入口（这是控制转移，属于控制流）。类型、形状、参数个数、装箱状态这四类都没有去优化路径，它们在编译期就已经确定。按我们的估计，这把分层的运行期成本（热路径上没有守卫）与工程风险都压缩了约 70%。

### 3. 内联缓存、隐藏类，以及静态类型删掉的东西

背景。V8 给每个对象一个隐藏类（map），记录属性偏移。属性访问编译成“检查 map，按定偏移加载”，检查由反馈向量里的单态内联缓存提供，之后可以升到多态（2-4 种形状）或巨态（全局桩）。LuaJIT 字节码没有按站点的字段内联缓存，而是在轨迹内把 TGETS/HREFK 降为带常量偏移的 HLOAD/FLOAD。`lj_ir.h` 的 `IRFLDEF` 字面上就是 `(name, offsetof(structure, field))` 对的表（`_(STR_LEN, offsetof(GCstr, len))`、`_(TAB_ARRAY, offsetof(GCtab, array))`、`_(FUNC_PC, offsetof(GCfunc, l.pc))`），也就是说一旦形状经守卫确认，访问就是按静态偏移的普通加载。

静态类型彻底删掉的东西：（1）map 检查与字段内联缓存。字段偏移作为立即数发进字节码（`FIELD_LOAD %dst, %obj, #12`），接收者类型是静态的，检查这一步因此省掉。（2）每个操作上的类型守卫。算术 opcode 按表示专门化。（3）静态调用上的参数个数检查。（4）原语上的大部分动态分派。（5）反馈向量基础设施及其内存。（6）那些守卫喂养的去优化机器。这项收益的规模有证据：CPython 3.11 约 1.25x 的提升（PEP 659）主要来自加入自适应专门化去近似静态已知的类型。LuaJIT 把记录器/优化器代码（`lj_record.c`、`lj_opt_*.c`）的很大一部分花在推断 DreamScript 前端已经证明的类型上。Maglev 的招牌特性（map 检查、稳定字段依赖）存在的意义就是在运行期恢复类型信息。

在 DreamScript 里，内联缓存式机制仍然有用的地方：

1. trait/dyn 分派：在 dyn（存在类型/trait 对象）调用点上，保留 1-2 条的内联缓存，把接收者类型 id 映射到具体函数指针（vtable 上的单态内联缓存）。巨态站点退到类型的方法表。该缓存只提供性能提示，正确性由缓存键里的类型 id 检查保证，陈旧命中因此被排除。形状与 CPython 的专门化计数器、LuaJIT 的 CALLT 分派相同，去优化环节整段省掉。
2. 可热重载的模块全局变量：调用点可以缓存已解析的函数，并通过模块版本计数器在重定义时失效（CPython 用就地改写字节码 opcode，即 `_SPECIALIZE_*`，达到类似效果。它便宜且效果好，我们可以改写缓存的指针）。
3. 效应处理器的操作表：按效应行每帧解析一次分派，字典查找按帧摊薄一次。

### 4. 值表示、拆箱，以及线性性带来的栈分配

表示形式调研。统一装箱的值（CPython 的 `PyObject*`）：每个标量都是堆对象，一次间接访问，每个数字一次分配，外加引用计数流量。带 tag/NaN 装箱的值（LuaJIT 的 `lj_obj.h`）：每个值一个 64 位字。GC64 模式下编码是高 13 位全 1（非规范 NaN 前缀）、第 47-50 位的 4 位 itype、47 位载荷（指针零扩展，原语全 1，DUALNUM 下 32 位整数零扩展）。`itype(o)` 就是 `((o)->it64 >> 47)`，任何类型测试只需一次移位。`tvisint`/`tvisnum` 区分 DUALNUM 下的整数与双精度。V8 用 Smi 标记（一个字里 31 位载荷）加指针压缩：堆指针是指向 4GB 笼子的 32 位偏移，内存减半，缓存密度提升（v8.dev/blog/pointer-compression）。LuaJIT 的轨迹更进一步，把一切都拆箱：`lj_ir.h` 的 `IRTDEF` 把 I8/U8/I16/U16/INT/U32/I64/U64/FLOAT/NUM 作为一等 IR 类型，`SLOAD` 的模式位（`IRSLOAD_CONVERT`、`IRSLOAD_TYPECHECK`）负责从带 tag 的槽位重建类型化值。静态类型 VM 可以直接长期跑在类型化表示上：每条字节码寄存器有固定类型，帧于是是异构寄存器堆，i32/i64/f64 原样存放，引用是裸 GC 指针，bool 是单字节。具体来说，`add_i64` 读两个帧槽位并发出一条 `add`。动态 VM 的 ADDVN 则必须测 tag（分支）、拆箱（移位/掩码）、相加、重新打 tag，还可能处理溢出转双精度。

拆箱的 ADT/enum/Result 布局。把 ADT 表示为跨连续寄存器的 tag 加 payload（例如 T 占 E 字节的 `Result<T,E>` 占 1+N 个槽位：tag 槽、payload 槽）。构造时写 tag，匹配时按 tag 分支，零堆分配，零装箱。这与 CPython（元组、异常都要分配）和动态 VM（每个 enum 都是堆对象）不同。对堆值（共享或装箱的结构），用固定头部：类型 id（给 dyn 分派内联缓存与 RC 用）、引用计数、payload。建议 DreamScript 的头部为 `{ u32 type_id; u32 rc; }`，共 8 字节，小装箱对象总尺寸保持 16 字节（Inko 0.20.0 出于同样的缓存行经济性，把字符串改成单次分配、最小 16 字节）。

帧内的 ADT 布局（示意）：

```
let r: Result<Trade, ParseError> = parse(input)   -- parse returns unboxed
  frame slot layout: [tag:u8][Trade payload: 6 slots]  or  [tag:u8][ParseError payload: 2 slots]
  match r { Ok(t) => ..., Err(e) => ... }
  ->  0: CALL  r2..r9, parse          ; returns tag in r2, payload in r3..r9
      1: TAGTEST r2, OK_TAG           ; r10 = (r2 == OK_TAG)
      2: JMPIF_NOT r10 -> err_path    ; single branch, no tables, no heap
```

逃逸分析与标量替换。就一门带所有权气味的语言而言，Inko 的经验是最好的公开数据点。Inko 0.20.0（2026 年 4 月）在内联过程中加入整程序跨过程逃逸分析（`inko/compiler/src/mir/escape.rs`、`inline.rs`。内联按逆拓扑 SCC 顺序处理方法，好让调用方复用被调方的非逃逸结果），在五个真实项目上把 47-58% 的堆分配提升到栈上（shost 58%、OpenFlow 51%、kvi 50%、idoc 47%、wtml 54%），HTTP 头/方法解析分别快 2.2x 与 2x。注意代价：整程序分析、结果与内联耦合、覆盖仍然只是部分。有了线性类型，非逃逸就是类型不变量：一个值当且仅当被移动进装箱/共享上下文时才堆分配，而类型检查器在发出字节码时已按站点知道这一点。对 VM 的后果：（i）每个局部 ADT/元组/Result 都活在帧寄存器里，快路径零分配。（ii）标量替换退化为寄存器指派（字节码分配器直接指派寄存器，没有什么剩下要替换）。（iii）可证明不逃逸的闭包落在栈帧上，堆分配这条路径被跳过。（iv）任何 GC 只会看到真正装箱的数据，根集合因此缩小。（v）错误处理的两条路径零分配。红利规模之大，动态解释器结构上给不出，即便 AOT 语言也只是在启发式地近似。

字符串。不可变 UTF-8 字节数组加长度（Rust 风格）用 (ptr,len) 视图给出 O(1) 切片。rope 只在大量增量拼接时划算。完整驻留（Lua/LuaJIT）让相等变成指针比较，但创建时要付哈希代价。Inko 0.20.0 把字符串改成单次分配（最小 16 字节）。DreamScript 的建议：不可变 UTF-8 加视图切片，标准库为编辑器类负载提供可选 rope。只驻留热比较里用到的短标识符与 tag。

### 5. 保证尾调用

解释器实现，附源码：

- 寄存器 VM 里的栈帧复用（Lua）：`luaD_pretailcall`（`lua/ldo.c`，约第 671 行）复用当前 `CallInfo`，把函数与参数按 `delta` 下移（可变参数帧需要）。随后用 nil 补齐定长参数，重置 `ci->top` 与 `savedpc`，置 `CIST_TAIL`，再返回共享的 `luaV_execute` 循环。工作量 O(args)，栈不增长，自尾调用与互尾调用（尾位置的任何调用）都正确。LuaJIT 在汇编解释器里通过 CALLMT/CALLT opcode 做同样的事（`lj_bc.h`，“Calls and vararg handling. T = tail call”），把参数拷到调用者的帧槽位再跳转。
- C 里的拷贝加跳转（Koka）：每个函数体都编在一个带 `kk__tailcall:` 标签的循环里。自尾调用先把实参求值到临时变量（`genTailVarBinding`，`genOverride` 会小心跳过自赋值以免覆盖，见 `koka/src/Backend/C/FromCore.hs` 约第 1428-1470 行），重新给 C 参数赋值，然后 `goto kk__tailcall;`（约第 2817 行）。一组函数之间的互尾递归由合并函数体内同一套标签机制处理。C 编译器看到的是普通循环，寄存器分配与分支预测因此最优。
- 蹦床（Chicken Scheme 的 “Cheney on the MTA”）：每次调用返回下一个 thunk，由驱动循环跳转。简单可移植，但每次调用多一次间接跳转，也让原生 JIT 互操作变复杂。我们掌控了 VM 的调用约定，这条路因此省掉。
- JIT 帧槽位复用：把调用者的实参槽位正好放在被调方的帧槽位所在之处（LuaJIT 在值栈上就是这样，wasm 在规范里编码了这一点）。机器码里的尾调用于是是一次重排加 `jmp` 到被调方入口，帧指针始终不动。这要求帧布局从第一天起就是 ABI 设计的一部分，而两侧都由我们掌控，这一点因此已经成立。
- 平台继承（wasm）：Wasm 3.0（2025 年 9 月完成，webassembly.org/news/2025-09-17-wasm-3.0）标准化了 `return_call`/`return_call_indirect`（提案从 github.com/webassembly/tail-call 合入，V8/Chrome 112 起发布，v8.dev/blog/wasm-tail-call），因此 wasm 后端从平台处得到保证的 TCO，包括穿过引擎自身的优化层。

反证：保证 TCO 是一个有意的选择。Ruby 没有保证的 TCO，CRuby 的编译期选项是尽力而为且默认关闭，YJIT 与更新的 ZJIT 都不把尾递归调用变换成循环（截至 2025-2026，blog.tdg5.com/tail-call-optimization-in-ruby-background 与 ruby-core 的 ZJIT 讨论）。JIT 团队一直把它排在后面，因为帧消除与去优化元数据、栈内省相互作用。从第一天起就在语义里保证它（Scheme/R7RS 风格）会迫使各处设计正确：尾位置成为有定义的概念，帧可以消除，调试器回溯携带显式的尾调用链元数据（Lua 的 `CIST_TAIL` 标志让 `debug.traceback` 在完整 TCO 下仍然可用）。

各语言的 TCO 处理汇总：

| 语言/运行时 | 解释器 TCO | JIT TCO | 机制 |
|---|---|---|---|
| Lua 5.4/5.5 | 保证（语言规范） | n/a | CallInfo 复用加参数下移（`luaD_pretailcall`、CIST_TAIL） |
| LuaJIT | 保证（语言规范） | 是 | CALLT/CALLMT 在值栈上重排参数后跳转 |
| Scheme/R7RS | 保证 | 是 | 语言要求、栈帧复用 |
| Koka | n/a（AOT C） | 是 | `kk__tailcall:` 标签循环，重赋参数（FromCore.hs） |
| Wasm 引擎 | n/a | 由规范保证 | `return_call`/`return_call_indirect`（wasm 3.0） |
| Ruby（YJIT/ZJIT） | 可选，默认关 | 否 | 无，只有尽力而为的编译开关 |
| JS 引擎 | 否 | 否 | ES6 规范要求 TCO，引擎（V8）从未发布 |
| CPython | 否 | 否 | 深递归溢出，3.15 的尾调用*分派*是另一回事 |

带槽位叠加帧的 JIT 尾调用的机器级示意（DreamScript 应当采纳的 ABI 决定）：调用者的出站实参正好写在自己帧的尾部，即被调方入站槽位所在之处。尾调用于是编译成（至多）几次寄存器重排加 `jmp callee_entry`，返回地址与帧指针保持不动，所以无论调用深度多大栈都不会增长。解释器里同一条不变量是 `memcpy(args down by delta); reload prologue; continue dispatch loop`（LuaJIT CALLT）。两者都是 O(args)、零分配，并保住 RC 所有权转移（实参被移动，没有引用计数流量）。

交互。（1）与效应/异步：进入带效应函数的尾调用只是又一次带效应的续体跳转。微妙之处在于捕获：位于 `handle`/捕获边界之下的尾调用必须为续体物化一个帧，所以保证应当读作“同一效应上下文内的正规尾调用”（Koka 的实际规则）。一次性续体（Koka 风格，kklib 的分叉栈上下文）让处理器入口便宜，并让处理器之外的 TCO 不受影响。（2）与去优化/调试：栈帧复用会销毁调用者帧，需要在调试器索要时从 TCO 链元数据惰性物化。（3）与 RC：转移实参所有权的尾调用不做任何引用计数更新。线性性让栈帧复用成为自然的降级（实参移入新帧的槽位），所以 TCO 实际上比普通调用更便宜。

### 6. 运行时的内存管理

调研过的选项，附证据：

- 分代复制 GC（Ocaml、Haskell、V8 新生代）：分配就是指针递增。新生代收集快但是搬移式，需要在每次指针写入上加屏障、记忆集，以及带精确栈映射的 JIT 安全点（V8 在 v8.dev/blog/trash-talk 里记录了 Orinoco 的机制）。对分配密集的不可变数据而言绝对吞吐最好，VM 复杂度最高，暂停需要调参。
- Immix/标记区域（Inko 1.x）：32KB 的 bump 分配块，快路径不搬移，只在碎片化时疏散。yorickpeterse.com/articles/friendship-ended-with-the-garbage-collector 记录了 Inko 为何先移除搬移式 GC，后来又彻底移除 GC。
- 搬移式紧凑全量 GC（V8 Orinoco、Go 风格）：堆最紧凑，机器最重（并行标记、疏散、屏障）。
- Perceus 精确 RC 加复用（Koka，Reinking、Xie、de Moura、Dreyer，PLDI 2021，“Perceus: Garbage Free Reference Counting with Reuse”）：编译器静态放置 dup/decref，并通过复用分析把释放与后续分配配对，`alloc` 于是变成“跳到复用路径，覆盖字段”，快路径上零分配器调用。Koka 运行期的证据：`kklib/include/kklib.h` 定义 `kk_reuse_t`（可空的回收块指针）、`kk_block_alloc_at(kk_reuse_t at, ...)`（非空时在回收块处分配，否则退回 `kk_malloc_small`，即 mimalloc 线程局部的 `mi_theap_malloc`），以及冷路径 `kk_block_check_drop_reuse`。代码生成在 `koka/src/Backend/C/Parc.hs`（借用/拥有环境对应论文的纪律），复用 pass 在 `ParcReuse.hs`（注意它的注释：所有 dup 在 drop 之前，drop 在 drop-reuse 之前，按模式树深度排序。这个顺序约束由它们的 pass 结构替我们解决了）。
- ARC 加逃逸分析（Swift，Inko 2.x 在 0.10.0 用所有权取代追踪式 GC 之后，使用自己的非弱原子 ARC，见 `inko/rt/src/arc_without_weak.rs` 与 inko-lang.org/news/inko-0-10-0-released）：确定性、无暂停，但朴素 ARC 在每次复制上都要付引用计数流量。逃逸分析救回大约一半分配（Inko 的 47-58%），其余仍走 RC 快路径。
- arena 与 region 分配（Vale 的 region、逐帧 arena）：O(1) 批量回收，适合解释器临时值与生命周期有界的模块数据。要安全就需要逃逸证明，而线性性在静态层面提供了它。
- 环的处理：追踪式 GC 直接处理环。RC 需要要么类型上禁止成环，要么试验删除，要么对候选集合做备份标记清扫，要么显式选择加入。

针对 DreamScript：线性类型意味着每个值只被使用一次，引用计数更新于是坍缩为显式的 clone/move 边界。这正是 Perceus，只是 dup 比 Koka 必须推断的少得多（我们从类型系统静态得知使用次数）。默认不可变消除了几乎所有指针写入屏障（没有字段修改），而屏障恰恰是分代 GC 复杂的根源。建议的组合，按顺序：（1）解释器临时值用按帧 bump arena，分配是一次加法，复位是一次恢复。它之所以可靠，是因为线性性保证临时值不会逃出帧，除非被移出帧。（2）装箱值用 Perceus 风格精确 RC 加块复用（Koka 模型，被线性性简化。通过复用 pass 把每次 drop 与下一次 alloc 配对）。（3）初期不做追踪式 GC。环只能通过显式的可变引用单元或惰性结表达。要么在类型层面拒绝（模块级递归特殊处理），要么提供显式的 `unsafe_cycle`/arena API 加试验删除，让 VM 不带通用收集器。结果：按构造零 GC 暂停、近乎零分配器流量、没有屏障、没有安全点、JIT 帧里没有 GC 映射，同时在分析数据显示需要时，仍可与为共享数据引入并发收集器兼容。

一个完整的 Perceus 例子（示意，Koka 编译到 C 的风格）。源程序是 `let x = pair(1, f(y))`，其中 `pair` 是装箱的二元组构造器，`f` 线性消费 `y`：

```
// compiled with reuse: decrement-then-alloc becomes alloc_at(reuse)
tmp2 = f(y);                       // y consumed linearly: no decref (moved)
rc_dec(tmp2_rc_field);             // tmp2 dies after field extraction -> candidate
reuse = (rc(tmp2) == 1) ? tmp2 : NULL;
x = alloc_at(reuse, sizeof(pair)); // reuses tmp2's block when it died uniquely
x.tag = TAG_PAIR; x.a = 1; x.b = extracted;
```

Koka 运行时恰好实现了这个形状：`kk_block_alloc_at(kk_reuse_t at, ...)` 在 `at == kk_reuse_null` 时退回 `kk_malloc_small`（mimalloc 线程堆），冷路径 `kk_block_check_drop_reuse` 处理一般的带复用递减（`kklib/include/kklib.h`）。复用 pass（`koka/src/Backend/C/ParcReuse.hs`）执行其源码里记录的顺序约束：dup 在 drop 之前，drop 在 drop-reuse 之前，各自按模式树深度排序，父节点先于子节点处理。对 DreamScript 而言，线性检查器直接给出使用次数，于是 dup/drop 放置 pass 退化为三条规则：在每个显式 `clone` 处插入 `rc_inc`。在每个值的唯一死亡点插入 `rc_dec`（最后一次使用或作用域结束，语法上可见）。把每个 drop 的复用候选沿直线代码串到下一次分配。没有借用分析，没有干扰分析。

### 7. IR 设计

MLIR 与 Mojo。MLIR 的核心价值是通过 dialect 做渐进降级，共享基础设施，文本 IR 可往返。Mojo 经 MLIR 编到原生码，SIMD 类型对编译器优化器可见（mlir.llvm.org、Modular 文档，近期第三方综述把该管线描述为“把高层代码逐层降级”）。整体采纳 MLIR 会带来庞大的工具链依赖与接近 C++ 的构建体验。对小语言来说，正确做法是照抄这个模式（一叠小的类型化 IR 互相降级：源码 -> 类型化字节码 -> 类型化 SSA -> 目标 IR），同时把那个依赖省掉。

Cranelift（源码：`wasmtime/cranelift/`）。CLIF 是每函数的类型化 SSA IR（`docs/ir.md`：显式块参数、i8..i64/f32/f64/v128 值类型、显式 `stack_slot`、直接分支。文档用 `ss0 = explicit_slot 8` 走了一遍 `average` 函数的例子）。优化是 ISLE 重写规则（`codegen/src/opts/cprop.isle`、`icmp.isle`、`arithmetic.isle`、`selects.isle` 等），在带代价函数与抽取的 e-graph 里运行（`codegen/src/egraph/{mod.rs, cost.rs, elaborate.rs}`）。规则说明（`opts/README.md`）写下了纪律：规则是有方向的，重写后的代码必须更好或持平。规范化规则必须产出不逊于原来的结果。任何删掉值使用者的规则必须用 `subsume` 以保持可靠。指令选择与降级同样按 ISA 由 ISLE 生成。把 Cranelift 当作库来支撑 DreamScript 的优化层，是可用杠杆最高的复用：我们的类型化 SSA 一对一降为 CLIF（没有装箱转换，没有类型检查，每个 CLIF 值类型都是静态已知的）。ISLE 让我们以 e-graph 的质量写出符合领域的重写（buffer 融合、线性性驱动的边界检查消除、效应导向的重排）。

Wasm 作为目标。Wasm 3.0（2025 年 9 月）标准化尾调用并包含 GC 类型。实测 wasm 相对原生的开销在 Chrome 里约 1.55x 几何平均，Firefox 里约 1.45x（“Not So Fast: Analyzing the Performance of WebAssembly vs. Native”，IMC 2019，ar5iv.labs.arxiv.org/html/1901.09056）。为类型化 SSA 写一个 wasm 发射器，就能得到浏览器/沙箱部署与接近原生的性能，作为部署模式而非 JIT，`return_call` 则把 TCO 保证穿过引擎带过去。

MoonBit（moonbitlang.com/blog/js-support，stdlib 仓库 moonbit-core）。一门静态类型的 ML 风味语言，管线是并行增量类型检查/推断 -> 核心 IR -> wasm/JS/原生后端。实测结果：在数据处理基准上比原生 JS 迭代器辅助函数快 25x，比 Json5 快约 8x，功劳归于编译器级迭代器融合（零成本的 `Iter` 类型，借鉴 Rust）、常量折叠、尾递归转循环、字符即码点表示。它的 IDE 达到毫秒级增量编译，这也是快速热重载的底座。MoonBit 是小团队用一份类型化 IR 加适度优化器在 wasm 上达到接近 C 吞吐的存在证明，它的多后端经济学（一份 IR，三个目标）与我们的分层计划契合。

类型化 IR 支撑我们这些特性的证据。（1）效应行局部地证明纯度：无效应的块可以跨调用重排、做 CSE、向量化。动态 VM 用逃逸与状态分析来近似，我们从类型直接得到。（2）线性类型证明非别名，于是解锁：buffer 融合（map/filter/zip 链不产生中间分配，中间值可证明没有别的读者）、唯一拥有数据的原地修改（仿射 mut 阶段）、以及在线性索引纪律已经证明位置的前提下消除边界检查（或化简成目标 ISA 能折叠的带检查加法形式）。（3）类型化 SSA 核心（每函数、显式类型、每次调用显式效应行、值上显式线性性）把这些都变成局部重写，分析范围停在函数内。同样的性质让 Cranelift 能把优化器表达成 SSA 上的有方向 ISLE 规则。

类型化 SSA 具体如何降为 CLIF。`sum_to` 的 DreamScript SSA（草稿）：

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

对应 CLIF（对照 `cranelift/docs/ir.md` 的 `average` 例子，那里用了 `explicit_slot` 加 `stack_store/stack_load`）：

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

每个值都有类型，装箱/拆箱操作与待发检查都归零，降级是机械的。这也是为什么 Cranelift（或 wasm 发射器）只需一趟小 pass，工程量停在 pass 级别。领域专用的优化接入 ISLE 规则层。一个 Cranelift 风格的例子（`cranelift/codegen/src/opts/arithmetic.isle` 的写法，按 `opts/README.md` 在用途消失处使用 `subsume` 的有方向规则）：

```
;; fuse: (x * 2^c) + x  =>  x * (2^c + 1)   -- direction matters: rhs no worse
(rule ((value (ty I64) x) (value (ty I64) c))
      ((shl (add x (mul x (i64_const 1))) c))   ; matched pattern
      ((mul x (add (shl (i64_const 1) c) (i64_const 1))))
      (if (ult c (i64_const 63))))
```

形状相同的 DreamScript 专用规则：当 buffer 是线性的（别名证明省掉）时，把 `append(map(a, f), map(b, f))` 这样的 buffer 链融成一个循环。只有在效应行为空时把无效应调用对重排成任一顺序（规则匹配时从 IR 元数据检查）。在线性游标纪律（例如对切片做 `for`）已经证明范围包含处消除边界检查。

### 8. 泛型实现与反特化立场

选项及其运行期画像：

- 单态化（Rust、C++、Swift 的泛型专门化路径、Inko 2.x 在 LLVM 下）：每次调用零成本。代价是编译时间与随实例化增长而膨胀的代码尺寸。在 C++/Rust 里它还会拖来特化/重叠规则，而作者明确拒绝这些（philosophy_of_PL.md：“非常不喜欢特化”，宁可要集合论类型，让不相交的匹配由程序员解决）。
- 装箱加字典传递（Java、OCaml、GHC 默认）：一份编译体。代价是原语实参的装箱与穿过字典的间接调用。可预测、一致、不膨胀。
- 擦除到动态（Gleam -> BEAM、Inko 1.x 字节码）：泛型代码在运行期真正是动态的，所有成本都是分派成本。Inko 的十年回顾记录了具体的痛处：值被标成 `Any` 后，关键字参数无法在编译期解析，必须编码进字节码并由 VM 在运行期解析。渐进类型挡住了内联等优化，这是把 Inko 推向完整静态类型与 LLVM 的诸多力量之一。
- JIT 去虚化/猜测（V8、Dart VM、PyPy）：预测成立时运行期画像最好。代价花在守卫、去优化与内联缓存机器上，而这些机器我们正在拆除。

尊重作者口味的 DreamScript 立场：（1）语言里根本没有特化语义，一个泛型函数只有一种含义。实例一致性（全局唯一，Haskell 风格）提供字典，重叠解析直接归入类型系统错误，语言层只保留这一种处理。（2）实现在划算时可以在调用点内联泛型调用。这是编译器内联，对程序员不可见，重叠规则因此省掉，因为每个实例化按定义都是同一个函数（特化函数体由编译器独占，程序员无从提供）。（3）对未内联的调用，把字典作为拆箱值传递：单方法 trait 的字典是寄存器里的一个代码指针。对数值表示多态的操作，运行期按表示提供代码（即 copy-and-patch 层用的同一套按表示的 stencil），这样就去掉了字典传递对原语值的经典装箱惩罚。字典传递能支撑高性能的证据：OCaml 与 GHC 广泛使用它，实测损失集中在原语类型的多态上，而拆箱的类型化 IR VM 大体消除了这一点。如果之后的分析数据显示热的泛型数值代码受损，再加一个用户显式列出的 `specialize` 边界（一种 import 列表风格的 pragma，把指定的函数在指定的类型上实例化）。这是受控的逃生口，留在核心语义之外，C++/Rust 式的开放式特化则留在门外。

### 9. 其他解释器级优化

- 字节码缓存与 mmap 启动：定长、基于索引、位置无关的字节码，一次 mmap 加载，零重定位（LuaJIT 的转储格式、CPython 的 .pyc、wasm 模块都证明了这一点）。有了类型化字节码，惰性构建的运行期元数据整块省掉，这点与 V8 不同，那里的反馈向量初始为空，各层必须先观察行为。冷启动等于免解析的 `load` 加主帧建立，大型程序的目标是一位数毫秒。支撑它的模块格式草稿：

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

- 热重载：按模块重新发出字节码。调用点缓存（第 3 节）通过模块版本计数器失效。tier 2/3 代码以（函数版本、stencil 集合）对为键，于是只有被改动的函数重新编译（MoonBit 的毫秒级增量管线表明工具链这一侧可行）。在线性语言里，两条规则让这件事保持简单：模块是不可变值（重载后的模块是新值，绑定到调用点缓存，绝不原地修改）。正在运行的帧继续用旧代码，直到返回（类似 Erlang 的双版本方案）。
- 调用开销工程：解释器里的静态调用是一次计算 goto 跳跃加一次帧压入（LuaJIT 的 FUNCF 序言 op 在几条指令里完成参数重排加分派）。dyn 调用付一次类型 id 检查加一次表加载（第 3 节的内联缓存）。这很重要，因为函数式的 DreamScript 会有大量小调用。把函数序言与尾声保持精简（Lua 的 CALL 就地压入/弹出结果，CPython 的 CALL 要花好几个 uop，还要包含我们完全绕开的方法查找回退）。
- 持久化数据对可变数据：默认不可变让结构共享安全可行（带 RC 的持久化向量/映射），而可变的唯一（仿射）数据编译成原地修改。同一个运行时可以同时提供两者：`Array`（唯一、可增长、零拷贝操作）与 `Shared`/装箱持久化结构。需要记录的成本不对称：持久化更新分配 O(log n) 个带 RC 的节点（在 Perceus 下对复用友好），仿射原地更新分配零个。语言应当让两者常见读取路径完全一致，切换成本才低。
- 错误表示：寄存器里的拆箱 Result（第 4 节）意味着错误路径在返回时只需一次 tag 分支，热路径上零异常表、零 landing pad、零 unwind 表。Panic/不可恢复错误走冷的 `longjmp` 式 unwinder 或效应传播。对比：异常表模型（Lua 的 longjmp、Java、C++ 零成本 EH）付元数据加表查找，换来抛出的便宜。Result 模型在每个可能失败的调用上付一次比较。在一门把错误放进类型里的语言里（按 philosophy_of_PL.md 对 Result 的偏好，这是 DreamScript 的立场），寄存器里的 Result 严格更好：成功路径是一次带标志的分支，编译器常常能把它折叠掉（例如调用方紧接着就匹配时）。Koka 展示了把异常做成带续体捕获的效应，代价只在 `handle` 边界上物化帧，这与我们的效应入口去优化预算相符。
- 启动与预热姿态：tier 1 立即启动。进入 tier 2 之前可以跳过 profile 阶段：我们的层由类型驱动，行为数据不参与决策。copy-and-patch 层可以在首次执行时编译，也可以按循环惰性编译，用 CPython `_JIT` uop 那样的退避计数器控制。PyPy 记录在案的多秒预热与 V8 的分层预算，是我们在结构上避开的成本。短脚本（不足 0.2 秒，PyPy 记录的死区）留在 tier 1，仍然有 3-10x 于 C 的表现。
- 超级指令与字节码窥孔：实现为带 profile 反馈的汇编器 pass（CPython 的 `macro(...)` 组合是静态的，LuaJIT 的 I*/J* 序言变体展示了专门化序言的价值）。便宜的收益：比较与分支融合（TAGTEST 之后的 JMPIF）、加载与操作融合（LDIMM 喂给算术）、为 tag 密集的 ADT 用匹配分派跳转表。

### 10. 性能天花板的证据

数据点与出处：

- 普通 Lua 5.x（寄存器 VM、带 tag 槽位）：比 LuaJIT 慢约 25x，视负载约比 C 慢 20-50x（eklausmeier.goip.de/blog/2016/04-05-performance-comparison-c-vs-lua-vs-luajit-vs-java：C 比 LuaJIT 快约 3x，LuaJIT 比 Lua 快约 25x）。
- LuaJIT：数值代码比 C 慢约 2-3x（同上测量）。紧致轨迹循环里接近 C。Cloudflare（blog.cloudflare.com/more-consistent-luajit-performance）记录了轨迹性能的稳定性，以及轨迹退出时的悬崖行为，也就是最好的动态 VM 也要付的动态语言税。
- CPython：比 C 慢约 50-100x。PEP 659 的专门化在 pyperformance 上买到约 1.25x（3.11）。从装箱的动态底座出发，尾调用解释器（3.15 时代）与 copy-and-patch JIT（3.13+）只是在继续磨。
- PyPy：多秒预热之后常常是 C 的 5-15x。已记录的病理包括短脚本，以及最多比 CPython 慢 2x 的情形（pypy.org/performance.html）。
- Truffle/Graal：“与最高性能实现相当，并且在很多情况下超过它们”（One VM to rule them all，Onward! 2013）。抹掉解释器开销的部分求值能达到峰值 VM 的质量。我们的静态字节码是同一想法的便宜静态形式。
- copy-and-patch：产物比 LLVM -O0 快约 14%。编译比 LLVM -O0 快 100x。wasm 变体比 Liftoff 快 39-63%（arXiv:2011.13127）。
- Wasm：原生约 1.55x（Chrome 几何平均，IMC 2019）。随着 wasm 3.0 特性在缩小。
- MoonBit：比 JS 迭代器辅助函数快 25x，比 Json5 快约 8x（moonbitlang.com/blog/js-support）。凭类型化 IR 加融合在 wasm 上接近 C，这是小团队的数据点。
- Inko（LLVM、所有权加 ARC 加逃逸分析）：跨过程逃逸分析把 47-58% 的分配提升，HTTP 解析快 2-2.2x（Inko 0.20.0）。十年努力去向 Go 级性能靠拢，既是目标，也是关于范围的警告。

解读。解释器与 C 的差距分解为：（i）分派开销（约 1.5-3x，靠线索化分派、超级指令、轨迹缝合缓解）。（ii）装箱/tag 开销（标量代码上约 2-10x，被静态类型加拆箱寄存器彻底消除）。（iii）动态 JIT 的守卫/猜测开销（存在时约 1.2-2x，我们完全省掉）。（iv）分配压力（对临时值而言，被线性性、帧寄存器与 arena 大体消除）。因此一个做得好的类型化寄存器解释器在标量/数值代码上应当落在 C 的 3-10x 附近（动态寄存器 VM 是 25-50x）。copy-and-patch 层应当接近 1.5-3x。优化层（Cranelift）应当达到 1-2x。解释器始终是即时启动的回退与调试底座，且各处都省掉了去优化的悬崖。这些是保守外推，锚点在 LuaJIT（动态语言天花板）与 copy-and-patch（基线编译器）两个数据点，并且把装箱项（LuaJIT 要付而我们省掉的那一项）从 tier 1 里彻底去掉。

### 11. 量化静态类型给解释器带来的收益

反复出现的问题是：除了一个好的动态 JIT 所能追回的，静态类型系统对解释器还值多少。按操作类别把 DreamScript 的 tier 1 与动态基线对比：

| 操作 | 动态 VM 成本（每操作） | DreamScript tier 1 成本 | 节省 |
|---|---|---|---|
| i64 加法 | 两次 tag 检查、两次拆箱、相加、重打 tag（LuaJIT ADDVV 路径），或完整方法分派（CPython） | 一次 add | uop 少约 3-6x |
| 字段访问 | map 检查加定偏移加载（V8），或解释器里的哈希查找（Lua） | 立即数偏移加载，无检查 | 约 2-4x |
| 调用（静态目标） | 反馈检查、参数个数检查、帧分配 | 序言 op 加帧滑动 | 约 2-3x |
| 调用（trait/dyn） | 内联缓存命中加调用 | 类型 id 检查加表加载加调用 | 约 1.2-1.5x |
| 元组/ADT 构造 | 堆分配加 RC/GC | 寄存器写入 | 约 5-20x（成本就是分配） |
| Result/enum 匹配 | 读堆上的 tag 加分支 | 读寄存器里的 tag 加分支 | 约 2-3x |
| 循环迭代 | 同样的分派 | 同样的分派 | 1.0x（分派不可约） |

乘进一个典型标量负载（Lucas/级数/fib 式循环由算术、调用、ADT 临时值主导），每操作的节省复合成第 10 节的 tier 1 估计 3-10x，而且完全不用 JIT。同一张表也解释了 LuaJIT 的轨迹层为什么快：它用守卫与猜测恰好追回前四行，代价是守卫/去优化机器。我们的类型系统无偿且永久地追回它们。

### 12. 风险与未决问题

1. 对调用密集的函数式代码，解释器吞吐仍受分派限制。如果分析显示调用占主导，先在字节码层面做调用内联（一个汇编器 pass，类似 CPython 的宏 op），再考虑上 JIT。
2. copy-and-patch 把 tier 2 绑定到构建期的 LLVM 与按架构的 stencil 库。如果工具链负担不可接受，回退方案是为受限制的 uop 子集手写一个小发射器（LuaJIT 的 lj_asm.c 是同一哲学的自我封闭例子，只是更大）。
3. Perceus 风格 RC 要靠复用 pass 才能兑现“分配免费”的承诺。没有它，分配退回 mimalloc（仍然快，约 15-25 个周期）。把复用 pass 预算进优化器，检查器专注在类型上。
4. 效应处理器、TCO 与 JIT 在处理器入口处相互作用（续体捕获必须物化帧）。把处理器挡在热循环之外，或让处理器入口成为显式的偏冷操作（Koka 的一次性续体证明这是可接受的）。
5. 跨平台 stencil 生成会让 CI 矩阵翻倍。考虑在没有验证过 stencil 构建的平台上只发 tier 1（CPython 实际上就是这么做的，JIT 按平台选择加入）。
6. tier 3 达到 C 的 1-2x 依赖 Cranelift 持续改进（相对 LLVM，它的 e-graph 优化器还年轻）。如果差距重要，wasm/原生 AOT 路径之后可以用同一份 SSA 换更重的后端（LLVM），代价是编译时间与依赖重量。

### 13. 从每个系统该偷什么（精简版）

从 Lua/LuaJIT：32 位定长寄存器指令加类型化 opcode、帧内联在值栈里并用尺寸字元数据、CALLT 式的参数重排实现 TCO、J* 轨迹缝合 opcode 让层级切换免费、带偏置的跳转偏移与 XOR 取反的比较对、按字节码位置的热度计数器。从 CPython：带显式栈效果与静态超级指令组合的 uop DSL、尾调用解释器的 musttail 分派作为近似可移植的 tier 1 备选、copy-and-patch 基础设施（stencil 抽取、修补器）作为 tier 2、就地改写 opcode 作为热重载的失效机制、用于触发 JIT 的退避计数器。从 V8：它分层里的结构性部件我们整段省掉（守卫与去优化正是我们要避开的），但它的内联缓存纪律在 dyn 分派站点仍然成立，Sparkplug 的帧保留规则对我们的 tier 2 是个好不变量（保留解释器帧布局，让调试器永远可用）。从 Truffle：部分求值的心智模型（我们的字节码就是预先求值过的表示），节点重写的想法对应我们的调用点缓存。从 Koka：Perceus RC 加复用的放置，以及 `kk__tailcall` 标签循环降级。效应作为统一控制（异步/错误）加一次性续体。从 Inko：静态类型加逃逸分析被诚实测量的价值（47-58% 提升）、单次分配 16 字节字符串的经济性，以及渐进类型阻塞优化的警示故事。从 MoonBit：一份类型化 IR 的多后端经济学，以及编译器级迭代器融合作为标准优化。从 Cranelift：CLIF 风格的类型化 SSA、带 subsume 纪律的有方向 ISLE 规则、以及带代价抽取的 e-graph 作为优化核心。

## (c) 权衡表

### 表 1：VM 架构

| 架构 | 相对 C 性能 | 复杂度 | 启动 | 备注 |
|---|---|---|---|---|
| 树遍历 | 慢 50-200x | 很低 | 快 | 无缓存/分层底座，排除 |
| 栈字节码（CPython 风格） | 慢 30-100x | 低 | 快 | 每操作指令更多，全靠 PEP 659 的英勇工作才像样 |
| 寄存器字节码，动态（Lua/LuaJIT 的 tier 1） | 慢 10-30x | 低到中 | 快 | 32 位指令，NaN 装箱的带 tag 槽位 |
| 寄存器字节码，静态类型，拆箱（DreamScript 的 tier 1） | 慢 3-10x | 中 | 快 | 无 tag、无守卫、拆箱 ADT，推荐 |
| 加 copy-and-patch JIT（tier 2） | 慢 1.5-3x | 加中 | 加微秒到毫秒级编译，加 stencil 二进制 | 无守卫 stencil，按 uop 模板 |
| 加优化层（Cranelift）（tier 3） | 慢 1-2x | 加高 | 加每函数毫秒级编译 | 共用的类型化 SSA，ISLE/e-graph 优化 |
| 同一 IR 的 wasm AOT（部署） | 慢 1.5-2.5x | 加中 | 加 wasm 编译 | 经 `return_call` 得 TCO，面向沙箱/浏览器 |

### 表 2：分派机制

| 机制 | 速度 | 可移植性 | 复杂度 | 出处 |
|---|---|---|---|---|
| switch | 基线 | 完整 | 最低 | 3.11 之前的 CPython |
| 计算 goto | +5-20% | GCC/Clang | 低 | `lua/lvm.c` 加 `ljumptab.h`，CPython `USE_COMPUTED_GOTOS` |
| 尾调用线索化 | 相当或更好的寄存器分配 | 需要 `musttail`+`preserve_none`（clang/GCC/MSVC 2026） | 中 | CPython 主干 `_Py_TAIL_CALL_INTERP`（`Python/ceval_macros.h`） |
| 轨迹缝合（J* opcode） | 热循环上省掉分派 | VM 内部 | 中到高 | LuaJIT `lj_bc.h` JFORL/JLOOP/JFUNCF |

### 表 3：JIT 分层策略

| 策略 | 峰值性能 | 预热 | 去优化复杂度 | 对静态类型的适配 |
|---|---|---|---|---|
| 元追踪（PyPy） | 高（对动态语言） | 秒级 | 高（轨迹失效） | 差：记录的是为动态语言设计的解释器噪声 |
| AST 部分求值（Truffle） | 最高 | 秒级 | 高 | 整层省掉：我们的字节码已经专门化 |
| 轨迹编译器（LuaJIT） | 最高 | 毫秒级 | 高（守卫、快照、侧向轨迹） | 守卫几乎消失，轨迹状态机器仍在 |
| copy-and-patch（CPython 3.13+） | 约 LLVM -O0 | 每单元微秒级 | 对我们近乎零 | 理想：stencil 以静态类型为键 |
| 经 Cranelift 的 SSA 方法 JIT | 高 | 每函数毫秒级 | 对我们近乎零 | 理想：类型化 SSA 一对一降为 CLIF |

### 表 4：值表示

| 方案 | 槽位尺寸 | 拆箱整数 | 类型检查成本 | 使用者 |
|---|---|---|---|---|
| 装箱指针 | 8B 加堆对象 | 否 | 解指针 | CPython |
| NaN 装箱 GC64 | 8B | 仅 DUALNUM 整数 | 一次移位（`lj_obj.h` 的 `itype`） | LuaJIT |
| Smi 标记加指针压缩 | 4GB 笼子里的 4B | 31 位 | tag 位测试 | V8 |
| 类型化寄存器（静态类型） | 每类型 4/8B | 任意宽度 | 运行期无 | DreamScript，LuaJIT 轨迹期的 IR `IRT_*` |

### 表 5：内存管理

| 方案 | 暂停 | 分配器流量 | VM 复杂度 | 对线性/不可变的适配 |
|---|---|---|---|---|
| 分代复制 GC | 短，搬移式 | 高（拷贝加屏障） | 高（屏障、安全点、JIT 映射） | 吞吐好，机器重 |
| Immix 标记区域 | 短 | 中（bump） | 中 | 尚可，快路径不搬移 |
| Perceus RC 加复用 | 无 | 最低（复用让分配近乎免费） | 低到中（编译期 dup/drop） | 最佳：线性性最小化 RC 操作（Koka 已验证） |
| ARC 加逃逸分析（Inko/Swift） | 无 | 中（逃逸分析救回约 50%） | 低 | 好，但逃逸分析是启发式，线性性让它精确 |
| 按帧 arena | 无（批量复位） | 临时值最低 | 低 | 临时值零开销，逃逸证明即线性性 |

### 表 6：泛型实现

| 策略 | 运行期成本 | 代码尺寸 | 语义 | 与作者偏好 |
|---|---|---|---|---|
| 单态化（C++/Rust） | 零 | 膨胀，编译变慢 | 招来特化规则 | 差（明确不喜欢） |
| 字典传递（OCaml/GHC） | 装箱实参加间接调用 | 一份函数体 | 一致 | 好，寄存器里的拆箱字典可缓解 |
| 擦除（Gleam/BEAM、Inko 1.x） | 完整动态分派 | 最小 | 动态 | 与性能目标不匹配 |
| JIT 去虚化/专门化（V8/Dart） | 守卫加去优化 | 运行期增长 | 猜测式 | 整层省掉：类型静态已知 |
| 编译器内联泛型调用，语言层只有单一语义 | 内联后为零 | 中等 | 一个函数一种含义 | 最佳：单态化性能、字典语义、零重叠规则 |

### 表 7：错误表示

| 模型 | 成功路径成本 | 错误路径成本 | 元数据 | 适配 |
|---|---|---|---|---|
| 异常加表（Java/C++ EH） | 近乎零 | 高（展开） | 二进制里的表 | 把错误藏在类型外，作者拒绝 |
| 装箱 Result（动态 VM） | 分配加 tag | 分配 | GC 压力 | CPython/旧 Koka，避开 |
| 寄存器里的拆箱 Result（DreamScript） | 一次带标志分支 | 同一次分支 | 无 | 错误在类型里可追踪，推荐 |
| 效应加一次性续体（Koka） | 处理器入口帧捕获 | 捕获成本 | 处理器表 | 统一异步/错误，与 TCO 规则兼容 |

## (d) DreamScript 可行的执行管线

### 管线 A：只有类型化寄存器 VM（解释器极大化）

设计：32 位寄存器字节码，opcode 按静态类型与表示专门化（LuaJIT ADDVN 风格，但由类型系统保证）、拆箱帧寄存器、计算 goto 或尾调用分派、经帧复用保证 TCO（Lua `luaD_pretailcall` 模型）、装箱值用 Perceus RC 加复用、临时值用按帧 bump arena、拆箱 Result/ADT 跨寄存器、可 mmap 的字节码文件、uop 级指令集加 profile 驱动的超级指令。
复杂度：严肃选项里最低的，JIT、去优化与 GC 屏障一个都不出场，大约一个强工程师一年能到扎实的 VM。
性能：标量代码上估计为 C 的 3-10x（分派与少量间接仍在）。零预热。延迟确定，适合热重载与调试。
权衡：天花板低于编译层。跨块优化与向量化只在发出字节码时做（窥孔级）。
最佳场景：语言必须先简单可预测，JIT 是之后的增量步骤。

### 管线 B：类型化寄存器 VM 加 copy-and-patch JIT（推荐核心）

设计：管线 A 作为 tier 1。tier 2 为 copy-and-patch：每个 uop 一份 stencil，由 LLVM 离线从主模板生成（CPython `Tools/jit/template.c` 模型）。补洞只用静态常量、类型与地址，零运行期守卫，零去优化路径。编译单元是沿边的线性字节码轨迹（循环体，类似 CPython 的 tier 2 轨迹），由退避计数器触发（`_JIT` uop 模式）。J* 式 opcode 把编译好的单元缝合进字节码（LuaJIT 的技巧），于是层级切换只花一次分派。
复杂度：中，一条 stencil 构建管线加一个修补器。CPython 证明了工具链可行（LLVM 21 只作为构建期依赖）。优化 pass 完全省掉。
性能：估计为 C 的 1.5-3x。每单元编译成本微秒级（arXiv:2011.13127）。内存是 stencil 库（几 MB）加补好的代码。
权衡：stencil 库按架构区分（先 x86-64、aarch64）。跨单元优化缺席（内联需要额外机器。这一点被缓解，因为调用便宜且 RC 放置是编译期的，内联的主要收益比动态 VM 里小）。
最佳场景：需要基线编译器级性能，但不想组建编译器团队。

### 管线 C：类型化 SSA IR 加 Cranelift 优化层

设计：tier 3 把共用的类型化 SSA（由同一份字节码构建）降为 CLIF 并用 Cranelift 编译。ISLE 规则承载 DreamScript 专用重写（效应导向重排、buffer 融合、线性性驱动的边界检查消除、拆箱 ADT 降为 CLIF 聚合体）。指令选择、寄存器分配与发射交给 Cranelift。
复杂度：中到高，需要 SSA 构建与降级。Cranelift 是庞大但在维护的上游依赖。
性能：估计为 C 的 1-2x（Cranelift 目标区间在 -O0 与 -O2 之间，e-graph 优化器补上一部分差距）。支撑跨块优化、内联与向量化钩子。
权衡：每函数毫秒级编译（作为 tier 3 没问题）。依赖体积大。短脚本用不到它，留在 tier 1。
最佳场景：长跑的服务器或计算负载重要时。它是管线 B 的自然搭档（同一份 SSA 喂给两者）。

### 管线 D：同一份类型化 IR 的 wasm/原生 AOT 部署（MoonBit 风格）

设计：类型化 SSA 编到 wasm 3.0（`return_call` 承载 TCO，wasm-GC 可选，用于宿主互操作），或经 Cranelift 编到原生。解释器继续作为开发/调试底座。DreamScript 的 “release build” 就是 AOT 产物。
复杂度：中，wasm 发射器远比 JIT 简单。原生路径复用管线 C。
性能：浏览器/嵌入式里为 C 的 1.5-2.5x（实测 wasm 开销约 1.55x）。原生等同管线 C。
权衡：与解释器并用才能保住即时的编辑-运行循环。引擎特性支持不一。
最佳场景：可移植性与沙箱是产品需求时。这也是通往“真正的编译器后端”叙事最便宜的路线。

### 管线 E：静态类型的轨迹编译器（研究性押注）

设计：从 tier 1 记录热循环成类型化 SSA。有了静态类型，记录器直接跳过类型/形状守卫，只发边界（线性性可消除）与效应边界。快照只服务调试器与效应入口。侧向轨迹很少。
复杂度：高，需要记录器、循环检测、快照映射、轨迹链接。LuaJIT 以错综复杂著称，而其中很大一部分错综复杂是守卫/快照机器，其目的已被我们的类型系统预先化解。
性能：可能最高（C 的 1-2x，有时打平），因为循环内的跨语句优化是自动的。
权衡：背 LuaJIT 全部维护负担，却省掉它大部分理由，因为管线 B+C 已用更少的机器覆盖大部分差距。轨迹退出的悬崖（Cloudflare 记录的不一致）是真实的用户体验风险。
最佳场景：数值内核性能就是产品，别的办法都不够。其余情况跳过。

### 推荐与顺序

从第一天起就在带显式效果的 uop 指令集上实现管线 A（CPython `bytecodes.c` 的纪律），并尽早构建类型化 SSA 降级，让 B/C/D 都能从同一个核心到达。等 uop 集合稳定后再加 B（copy-and-patch），它的 stencil 与解释器的 uop 按构造就是同一份代码。等峰值性能或部署需要时加 C（Cranelift）或 D（wasm）。E 只在数值内核的卖点要求时才上。这个顺序与证据一致：静态类型移除了守卫/去优化/猜测层，而这一层在调研到的每个动态分层系统里同时主导复杂度与运行期成本。所以 DreamScript 的各层严格比动态对应物简单，解释器本身起步基线是 C 的 3-10x，动态寄存器 VM 的 25-50x 差距就此填平。两个决定要尽早锁定，因为上面一切都依赖它们：（1）调用约定把被调方帧槽位叠在调用者实参槽位之上（支撑 TCO 与廉价调用）。（2）字节码位置无关、定长、基于索引（支撑 mmap 缓存、stencil 补洞、J* 式缝合）。

## (e) 各系统速查（读哪里）

| 系统 | 字节码/IR | 分派 | TCO | JIT | 内存 |
|---|---|---|---|---|---|
| Lua 5.5 | 32 位寄存器 op，103 个 opcode（`lopcodes.h`） | switch/计算 goto（`ljumptab.h`） | CallInfo 复用（`ldo.c`） | 无 | 增量追踪式 GC（`lgc.c`） |
| LuaJIT | 32 位类型化寄存器 op（`lj_bc.h`） | 汇编计算线索化加 J* op（`lj_vm.S`） | CALLT/CALLMT 参数重排 | 轨迹，守卫加快照（`lj_record.c`、`lj_snap.c`、`lj_jit.h`） | 增量 GC，NaN 装箱值（`lj_obj.h`） |
| CPython 3.13-3.15 | 栈 codeunit -> uop（`bytecodes.c`） | 计算 goto 或 musttail 处理器（`ceval.c`、`ceval_macros.h`） | 无 | copy-and-patch stencil（`Tools/jit/`） | 带引用计数的 PyObject |
| V8 | Ignition 寄存器字节码加反馈 | 内联缓存处理器 | 无 | Sparkplug/Maglev/TurboFan | 分代紧凑 GC |
| PyPy | RPython 字节码 | JIT 驱动 | 是（多数情况） | 元追踪 | 按 GC 策略的分代 GC |
| Truffle | AST | 部分求值 | 取决于语言 | Graal PE | 宿主 GC |
| Inko 2.x | MIR -> LLVM（无字节码，`compiler/src/mir/`、`llvm/`） | n/a | n/a | 无（AOT） | ARC 加跨过程逃逸分析（`rt/src/arc_without_weak.rs`、`mir/escape.rs`） |
| Koka | Core -> C | C 编译器 | 标签循环（`FromCore.hs`） | 无（C 工具链） | Perceus RC 加复用（`Parc.hs`、`kklib.h`） |
| Cranelift | CLIF 类型化 SSA（`docs/ir.md`） | n/a | n/a（是目标） | n/a（本身就是 JIT 后端） | n/a |
| MoonBit | 类型化核心 IR | n/a | 尾递归 -> 循环 | n/a（AOT wasm/JS/原生） | 按目标用 GC 或 RC |

## 参考文献

本地克隆（不可信，只读，调研结束后清理）：Z:/Temp/plr3/{lua, luajit, cpython, inko, koka, wasmtime, moonbit-core}。

上文引用的关键源文件：luajit/src/lj_bc.h（字节码格式、类型化 opcode、CALLT/CALLMT、J* op）。luajit/src/lj_ir.h（64 位 SSA IRIns、守卫、SLOAD 模式、ADDOV、IRFLDEF 字段偏移、IRT 类型）。luajit/src/lj_obj.h（NaN 装箱 GC64 编码、47 位指针、写屏障规则）。luajit/src/lj_frame.h（值栈上的帧布局、续体帧）。luajit/src/lj_jit.h（SnapShot/SnapEntry）。luajit/src/lj_dispatch.h（热度计数器）。lua/ldo.c（luaD_pretailcall、CIST_TAIL）。lua/lvm.c + ljumptab.h（计算 goto、trap 钩子）。lua/lopcodes.h（字段尺寸）。cpython/Python/bytecodes.c（DSL、宏 op、_BINARY_OP_ADD_INT、_JIT、tier2 uop）。cpython/Python/ceval_macros.h + ceval.c（尾调用解释器、musttail/preserve_none）。cpython/Tools/jit/（PEP 744 stencil JIT、template.c、_stencils.py、README.md）。inko/rt/src/arc_without_weak.rs（ARC）、inko/compiler/src/mir/escape.rs + inline.rs（跨过程逃逸分析）。koka/src/Backend/C/FromCore.hs（tryTailCall、kk__tailcall goto 循环、genOverride）、koka/src/Backend/C/Parc.hs + ParcReuse.hs（Perceus 代码生成、复用排序）、koka/kklib/include/kklib.h（kk_block_alloc_at、mimalloc 线程堆、drop_reuse）。wasmtime/cranelift/docs/ir.md（CLIF）、cranelift/codegen/src/opts/README.md + *.isle（有方向 ISLE 规则、subsume）、cranelift/codegen/src/egraph/（cost.rs、elaborate.rs）。

论文、文章与文档：
- Copy-and-Patch Compilation, Xu & Kjolstad, OOPSLA 2021: https://arxiv.org/abs/2011.13127
- PEP 744, CPython JIT: https://peps.python.org/pep-0744/ ; PEP 659, specializing adaptive interpreter: https://peps.python.org/pep-0659/
- V8: Sparkplug https://v8.dev/blog/sparkplug ; Maglev https://v8.dev/blog/maglev ; Ignition https://v8.dev/blog/ignition ; Leaptiering https://v8.dev/blog/leaptiering ; GC https://v8.dev/blog/trash-talk ; wasm tail calls https://v8.dev/blog/wasm-tail-call
- PyPy JIT 与性能：https://www.pypy.org/performance.html
- One VM to rule them all (Truffle)，Onward! 2013：https://dl.acm.org/doi/10.1145/2509578.2509581。综述：https://blog.acolyer.org/2014/11/18/one-vm-to-rule-them-all/
- Perceus: Garbage Free Reference Counting with Reuse, PLDI 2021: https://www.microsoft.com/en-us/research/publication/perceus-garbage-free-reference-counting-with-reuse/
- Inko: A decade of developing a programming language https://yorickpeterse.com/articles/a-decade-of-developing-a-programming-language/ ; Friendship ended with the garbage collector https://yorickpeterse.com/articles/friendship-ended-with-the-garbage-collector/ ; 0.20.0 逃逸分析 https://inko-lang.org/news/inko-0-20-0-reducing-heap-allocations-by-50 ; 0.10.0 移除 GC https://inko-lang.org/news/inko-0-10-0-released
- MoonBit JS 后端与基准：https://www.moonbitlang.com/blog/js-support
- Wasm 3.0（尾调用标准化）：https://webassembly.org/news/2025-09-17-wasm-3.0。尾调用提案：https://github.com/webassembly/tail-call
- Not So Fast: Analyzing the Performance of WebAssembly vs Native Code, IMC 2019: https://ar5iv.labs.arxiv.org/html/1901.09056
- C / Lua / LuaJIT / Java 对比测量：https://eklausmeier.goip.de/blog/2016/04-05-performance-comparison-c-vs-lua-vs-luajit-vs-java。Cloudflare 的 LuaJIT 轨迹稳定性：https://blog.cloudflare.com/more-consistent-luajit-performance
- MLIR: https://mlir.llvm.org/。Mojo/MLIR 管线综述：https://arxiv.org/html/2606.16059v1
- Ruby 的 TCO 状态（无保证 TCO，YJIT/ZJIT）：http://blog.tdg5.com/tail-call-optimization-in-ruby-background/ 与 ruby-core 的 ZJIT 讨论（2025）
- 作者口味笔记：C:/programs/me/lxl66566.github.io/src/coding/philosophy_of_PL.md（反特化、Result 错误、效应）
