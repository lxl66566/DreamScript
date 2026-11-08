# 07 — 运行时：执行管线、尾调用与内存机制（决策目录）

状态：阶段 2 选项目录，写于 2026-11-04，等待作者按 AGENTS.md 做出决策。主要证据：`docs/zh/research/03-runtime-ir-jit.md`，下文引用为 research/03，其中 §a.N 表示概要第 N 条，§b.N 表示深入分析第 N 条，§c 表示表 N，§d 表示管线章节。研究笔记记录了 URL 的地方在原处引用。姐妹目录只按路径引用，内容由各自文件负责：`docs/zh/design/01-types.md`（类型、数值、字符串、相等性）、`docs/zh/design/02-linearity-ownership.md`（仿射核心 C1、内存 C4、析构 C5）、`docs/zh/design/03-effects-errors-async.md`（错误 B5、效应 E1、纤程 E2）、`docs/zh/design/06-modules-tooling.md`（模块 G3、dsmod 契约 G3.5、工具 H6）。`docs/zh/design/05-generics-metaprogramming.md` 在这里不读，derive/Ord/Hash 的问题由它负责。本文件处理 `docs/zh/design/00-decision-map.md` 分配的 H1、H2、H4 三个点。所有代码都是示例，写于 docs/zh/design/04 的表层决定之前，只说明语义。表层语法由 `docs/zh/design/04-syntax.md` 决定。

## 决策点索引

| ID | 决策点 | 选项 | 推荐（一行） |
|---|---|---|---|
| H1 | 执行管线 | (a) tier1 带类型寄存器 VM 加 tier2 copy-and-patch JIT 加 tier3 Cranelift/wasm-AOT，共用一套字节码到 SSA 的降级、(b) 寄存器 VM 加单一方法 JIT 层、(c) AST 解释器加元追踪 JIT（PyPy/RPython）、(d) wasm-AOT 作为主要目标，解释器兜底（MoonBit 的姿势） | (a)：三层摊薄同一套降级与同一套 uop。tier2 的 stencil 与 tier1 的 uop 一一对应，所以每一层都是增量，永不分裂成两套编译器（research/03 §d） |
| H2 | 尾调用保证 | (a) 从第一天起把 TCO 写进规范并保证生效，自身与互递归都算，tier1 靠栈帧复用，JIT 层靠标签循环与槽位叠加降级、(b) 尽力而为的 TCO（取决于 JIT）、(c) 用户层蹦床作为库约定、(d) 尾式恢复只由效应系统提供 | (a)：章程硬性要求 3 指定了这一保证。在 tier1 里它是一次指针重排（Lua `luaD_pretailcall`），在 JIT 层里是一个标签循环或槽位叠加的 jmp（Koka `kk__tailcall`），是菜单上最便宜的保证（research/03 §b.5） |
| H4 | 内存运行时机制 | (a) Perceus 精确非原子 RC 加块复用加逐帧 bump arena 存放不逃逸的临时值、(b) 为不可变数据调优的带代搬移式 GC、(c) 每个对象 malloc/free，不做 GC（纯线性所有权）、(d) 混合：共享/装箱值用 RC 加线性数据用 arena 加可选的并发循环收集器延后 | (a)，并明确把 (d) 的循环收集器接口继承为共享或带循环负载预先规划的成长路径（research/03 §a.8、§b.6） |

范围说明：H1 决定存在哪些执行层、控制如何在层间移动，它不重新决定值表示（docs/zh/design/01）、内存策略（H4）或字节码文件契约（docs/zh/design/06 G3.5）。H2 决定尾调用语义及其机制。H4 决定各层之下运行期的内存发生什么。三者是可分开的决定，但只有一致的组合才有意义，这些组合在第 7 节列出。第 1 节的固定基线对三者全部生效。编号遵循 `docs/zh/design/00-decision-map.md`。分派机制的选择归入基线 BL-3，作为基线条目。

## 1. 固定基线（对所有选项生效）

这些是 research/03 从章程推导出的不可谈判项（静态类型、极致性能、TCO、带内部 IR 的解释执行），对所有选项生效。下面每个 H1/H2/H4 选项都按它们打分，违反某条基线的选项会在「排除」里点名。

**BL-1 带类型的寄存器字节码。** 32 位定长指令，寄存器索引作为操作数，带类型的操作码：算术按表示特化（ADDI64/ADDF64，比较 EQI64/EQF64，加载 KIMM/KCONST），沿用 LuaJIT 的 ADDVN/ADDNV/ADDVV 纪律（`lj_bc.h`），唯一的差别是动态的「V」变体不存在，因为每个操作数的类型都是编译期事实（research/03 §b.1「Typed opcodes in bytecode」）。值得逐字照抄的格式细节：带偏置换算的跳转偏移（BCBIAS_J，一次无比较的无符号减法），以及异或取反的比较对（ISEQV^1 == ISNEV）（research/03 §b.1）。

**BL-2 拆箱槽位。** 不用 NaN-boxing，不用逐槽标签，不装箱标量：i32/i64/f64/bool 原样放在栈帧槽位里，引用就是裸指针，ADT、并集与 Result 是拆箱的标签加载荷对，摊在连续槽位上（research/03 §b.4、§c 表 4）。装箱正是静态类型要删掉的 2 到 10 倍标量代码开销（research/03 §b.10）。docs/zh/design/01 的「共享前提」已经按这条基线给每个类型选项打分。

**BL-3 分派。** computed goto（`lua/lvm.c` 加 `ljumptab.h`，CPython 的 `USE_COMPUTED_GOTOS`，比 switch 快 5-20%）或 C 尾调用穿线（CPython 的 `_Py_TAIL_CALL_INTERP`、`musttail` 加 `preserve_none`，clang/GCC/MSVC 2026+）。两者都有生产验证。最终发哪一个由构建可移植性决定，属构建配置（research/03 §b.1、§c 表 2）。

**BL-4 按 profile 数据合成超级指令。** 汇编器按实测的操作码频率合成超级指令（比较与分支融合、加载与运算融合、密集标签匹配跳转表），沿用 CPython 的 `macro(BINARY_OP)` 组合模式，LuaJIT 的 ISNEXT/RETM 折叠是先例（research/03 §b.1「Superinstructions and macro-ops」、§b.9）。融合示例由汇编器按实测频率组合，从不手工挑选：

```
;   TAGTEST r2, OK_TAG ; JMPIF_NOT r2 -> L    =>  TAGBR r2, OK_TAG, L     (match dispatch)
;   LDIMM r1, 0:i64 ; ADDI64 r1, r1, r0       =>  ADDIMM r1, r0, 0:i64    (load-op)
```

**BL-5 从第一天起做 uop 分解。** 指令集以带显式效应的微操作定义（CPython `bytecodes.c` 的纪律：显式的栈与槽位效应、`pure` 标注），copy-and-patch 的 stencil 集合按构造就是这些 uop 一一对应（research/03 §b.1、§d 管线 B）。正是这一点让 H1.a 的 tier2 复用同一套 uop，成为派生产物。

**BL-6 栈帧内联在有类型的值栈里。** 每个栈帧是一段连续的静态类型槽位加一个元数据字（返回地址、栈帧大小、TCO 链的标志），布局取自 LuaJIT `lj_frame.h`。选它的原因是布局按函数已知：调用便宜，TCO 是一次指针重排，stencil 的 ABI 平凡，调试器只要走元数据字就能物化栈帧（research/03 §b.1 的栈帧设计对比，「For DreamScript」段落）。

**BL-7 位置无关、可 mmap 的 dsmod。** 定宽、基于索引、无需重定位的字节码，带一个模块头（版本、类型与常量表、每函数栈帧槽位类型、栈帧大小、最大调用深度）与 IR 版本校验和。结果是一次 mmap，没有重定位，`load` 便宜（research/03 §b.1 的格式设计、§b.9 草案）。格式契约由 docs/zh/design/06 G3.5 负责：缓存在 `$DS_CACHE/ir/<triple>/<blake3>.dsmod`，按依赖列表惰性加载，`ds build --exe` 生成内嵌字节码的单一二进制（Bun 实测的 1.38 倍 `--compile --bytecode` 启动收益是下限），以及 Erlang 风格的双版本模块槽位支撑热重载（docs/zh/design/06 G3.5 与 H6 的必须项 5）。

基线端到端跑一次的样子（编码取自 research/03 §b.1，每个槽位都有类型，任何地方都没有标签）：

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

## 2. H1 — 执行管线

本点的问题是：存在哪些执行层，顺序如何，层与层之间放什么机械。research/03 的证据是：每个被调研的动态 VM 都把大部分 JIT 花在运行期类型猜测上（守卫、快照、反馈向量），而这些 DreamScript 的类型系统无条件删掉（research/03 §a.3、§b.3），所以可行的设计主要差在剩下那些没有猜测的工作里各自做多少。每个选项都原样跑在 BL-1 到 BL-7 上：层不同，底座不变。下文一直使用的性能锚点：copy-and-patch 的输出比 LLVM -O0 快约 14%，编译速度快约 100 倍（arXiv:2011.13127）、CPython 把它作为 PEP 744 发布、wasm 在 Chrome 里约为原生的 1.55 倍（IMC 2019）、纯 Lua 比 C 慢约 20 到 50 倍、LuaJIT 约为 C 的 2 到 3 倍、DreamScript 的现实目标是纯解释器达到 C 的 3-10 倍，加 copy-and-patch 为 1.5-3 倍，加优化层为 1-2 倍（research/03 §a.11、§b.10）。

### H1.a — 分层：tier1 带类型寄存器 VM 加 tier2 copy-and-patch 加 tier3 Cranelift 或 wasm-AOT（共享降级）

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

具体例子：`ADDI64 r1, r1, r0` 的 tier2 stencil（research/03 §b.2 的完整示例）。stencil 库为每个操作数类别准备一份 x86-64 片段，在构建期由 LLVM 从一个包含 VM 头文件的主模板生成（CPython `Tools/jit/template.c` 的模式），栈帧槽位偏移与常量处留洞。

```
; stencil ADDI64_rr, holes marked <H>; no type checks, no RC calls, no error glue
mov  rax, [r14 + <H off_r1>]     ; hole: u16 frame-slot offset of r1
add  rax, [r14 + <H off_r0>]
mov  [r14 + <H off_r1>], rax
; compile loop body = concat stencils + patch holes; sub-microsecond per stencil
```

tier3 的具体形态是共享降级到带类型 SSA，再进 CLIF（research/03 §b.7）。同一个 `sum_to` 循环可以机械地降级，因为每个 CLIF 值类型都静态已知，既不需要装箱转换，也无需发射检查：

```
v0: i64 = param n                function %sum_to(i64) -> i64 {
loop(v2: i64 = 0, v3: i64 = v0):   block0(v0: i64):
  v4: bool = le v3, 0                v4 = icmp sle v0, 0 ; brif v4, block2, block1
  br_if v4 -> exit                 block1(v2: i64, v3: i64):      ; block params, no phis
  v5: i64 = add v2, v3               v5 = iadd v2, v3 ; v6 = isub_imm v3, 1
  v6: i64 = sub v3, 1                jump block1(v5, v6)
  br loop(v5, v6)                  block2(v7: i64): return v7 }
```

DreamScript 特有的重写在这一层以定向 ISLE 规则插入（Cranelift `opts/README.md` 的纪律：规则不得改写出更差的代码，用途消失处用 `subsume`）：线性缓冲区做缓冲区融合，不需要别名证明、效应导向的重排在效应行为空时才放行、边界检查消除用在那些线性游标纪律已经证明过界内的地方（research/03 §b.7「DreamScript-specific rules」）。

先例：copy-and-patch，Xu & Kjolstad（https://arxiv.org/abs/2011.13127），CPython PEP 744（`Tools/jit/`）、Cranelift 的带类型 SSA 加定向 ISLE 规则（research/03 §b.7）、wasm 3.0 的 `return_call`（research/03 §b.5）。完整管线是 research/03 §d 的管线 B+C+D 及其「Recommendation and sequencing」段落。

**层间切换。** 计数器触发编译（逐字节码位置的热计数，或 CPython 在 `_JIT` uop 上的退避计数器）。安装后原地把字节码改写成 J 变体的操作码（LuaJIT 的 JFORL/JLOOP/JFUNCF 手法），于是层间切换只花一次分派，OSR 退化成「在后向边上开始解释或编译这个单元」。这里没有栈帧变换的机械，因为编译单元是循环体，规模小于整个函数（research/03 §b.1 的 trace-stitched 条目、§b.2 的 OSR 段落）。tier3 按 profile 编译整个函数，键是（函数版本，stencil 集合），所以热重载只重编被触碰的函数（research/03 §b.9）。

**去优化范围。** 数据路径上没有守卫。仅有的出口是：(i) 调试器断点，从 JIT 栈帧元数据重新物化栈帧，Sparkplug 的栈帧保留不变式就是规则、(ii) 每函数入口一次栈上限比较，栈帧大小静态已知、(iii) 效应处理器入口，那是控制转移，属正常控制流（docs/zh/design/03 E1d）、(iv) 在 BL-2 之下，只有当 B7 的溢出即陷入算术需要一条冷溢出边时，ADDOV 这类守卫才留下（research/03 §b.2 的去优化表、§b.2 的 ADDOV 建议）。

**相对 C 的性能目标。** tier1 为 3-10 倍，tier2 为 1.5-3 倍，tier3 为 1-2 倍，同一份 SSA 的 wasm 部署为 1.5-2.5 倍（research/03 §c 表 1、§d）。任何地方都没有去优化的悬崖，不像 LuaJIT 有记录的追踪退出悬崖（https://blog.cloudflare.com/more-consistent-luajit-performance）。

**启动。** tier1 在 mmap 的 dsmod 上瞬时启动（BL-7）。stencil 库只有几 MB，惰性加载。tier2 的编译是每单元微秒级。短脚本在 0.2 秒以内，即 PyPy 记录在案的死区，完全不用付分层的账，却仍有 3-10 倍（research/03 §b.9、§b.2）。

| 维度 | 评估 |
|---|---|
| 性能天花板 | tier3 为 C 的 1-2 倍。仅 tier2 就已达到基线编译器级别（research/03 §d 管线 B） |
| 实现复杂度 | 总体中等：stencil 构建管线加补丁器（工具链由 CPython 验证，LLVM 21 只在构建期作依赖），再加 tier3 的 SSA 降级、没有手写的优化遍 |
| 启动 | 所有选项里最好：瞬时 tier1，微秒级 tier2 触发，没有预热阶段 |
| 可移植性 | stencil 按架构（先 x86-64，再 aarch64）。没有验证过 stencil 构建的平台只发 tier1，正是 CPython 按平台自选 JIT 的姿势（research/03 §b.12 第 5 条风险） |

排除：单 VM 的简单性（这里有三个产物），以及任何让解释器的 uop 与 JIT 的单元发生分叉的设计。BL-5 正是让这种分叉不可能发生的东西。

### H1.b — 寄存器 VM 加单一方法 JIT 层

解释器之上只有一层 JIT：方法级编译器（在带类型 SSA 之上跑 Cranelift，或者按 research/03 §b.12 第 2 条风险的退路哲学手写一个小发射器）按热度编译整个函数。

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

先例：research/03 §d 管线 C。方法级编译正是 Sparkplug/Maglev/TurboFan 与 JVM 的分层方式，但这几家都需要 OSR，因为它们的编译单元是整个函数（research/03 §b.2 的 OSR 段落）。

**层间切换。** 按函数计数。在下一次调用时进入编译代码是平凡的，但一个冷调用函数内部的热循环要么需要 OSR 加栈帧变换（research/03 说要回避的那套机械），要么接受这类循环留在解释器里。这就是「只有一层」的诚实代价。**去优化范围。** 与 H1.a 相同的四条残余出口，另加一类新的问题：物化时 SSA 值必须映回栈帧槽位，而这正是 Sparkplug 靠保留解释器栈帧布局避开的记账。这里的缓解手段（tier2 必须复用 BL-6 的槽位布局）是硬性规则。**相对 C 的性能目标。** 编译前 3-10 倍，编译后 1-2 倍。1.5-3 倍这一档被跳过。从未触发编译计数的短脚本停留在 tier1 的水平，这可以接受，因为 tier1 本身已经很强。**启动。** 与 H1.a 相同，只少了 stencil 库、编译成本是每函数毫秒级，所以热函数的首跑延迟可见但有界。

| 维度 | 评估 |
|---|---|
| 性能天花板 | C 的 1-2 倍（同样的 tier3 数字，用一个编译器而非两套机制达到） |
| 实现复杂度 | 纸面上是 JIT 选项里最低的（一个编译器），但「要么做 OSR，要么接受损失」是明确的设计负债、失去 BL-5 的「uop 即 stencil 一一对应」摊薄 |
| 启动 | 好、热函数首编毫秒级 |
| 可移植性 | JIT 选项里最好：Cranelift（或 wasm）本来就是多架构、没有按架构的 stencil CI 矩阵 |

排除：无守卫的 stencil 经济学（编译成本接近零，没有寄存器分配器），以及 CPython PEP 744 式的增量分层。如果 stencil 工具链被证明确实不可接受，这个形态就是退路（research/03 §b.12 第 2 条）。什么时候 (b) 会胜出：如果剖析显示按架构的 stencil CI 矩阵吃掉大部分维护成本，而 Cranelift 早已是 wasm/原生部署路径的依赖，那么把 tier2 与 tier3 合成一个编译器就是诚实的简化。**结论 — 否，保留为退路。**

### H1.c — AST 解释器加元追踪 JIT（PyPy/RPython 姿势）

用 RPython 风格的静态类型子集来写解释器。元追踪 JIT 观察热的解释器循环，追踪解释器自己做了什么，于是同一套 JIT 服务这门语言，不需要为语言单独写编译器。

```
eval(node: Expr, env: Env) -> Value {          // tree-walker; JIT records this loop
  match node { Bin(op, l, r) => apply(op, eval(l, env), eval(r, env)), ... }
}
// meta-trace: unfold eval + apply for the hot subtree; compile; deopt on new shape
```

先例：PyPy，预热「至少好几秒」。0.2 秒以下的脚本与单元测试永远拿不到收益。内省（`sys._getframe`、追踪钩子）会在其作用范围内禁用 JIT。运行期支持密集的代码最多比 CPython 慢约 2 倍（https://www.pypy.org/performance.html，research/03 §b.2）。RPython 自举策略是真实存在的：PyPy 自己就是这么建的，也是已知最便宜的「为动态语言拿到一套强 JIT」的路线。用在这里意味着用一门静态类型的受限宿主语言写 DreamScript 解释器，让元 JIT 观察解释器自己的循环，并把翻译器工具链当成硬性构建依赖。换来一套免费的强 JIT，代价是继承 PyPy 的预热悬崖、它的内省即禁用语义（而这些位置恰好就是调试器与 docs/zh/design/06 H6 的热重载所在），以及一个社区之外没人维护的翻译器（research/03 §b.2 的 PyPy 条目）。

**层间切换。** 热解释器循环的计数在元 JIT 内部。语言能看到的只有预热悬崖。**去优化范围。** 假设失效时的追踪作废，加上内省即禁用的问题。对我们来说，调试器与热重载（docs/zh/design/06 H6 的双版本槽位）会正好落在禁用 JIT 的作用范围里。**相对 C 的性能目标。** 多秒预热之后有可能达到 5-15 倍，同时有记录在案的慢 2 倍病态情形。research/03 的结论很直白：对静态语言这是错误的取舍，解释器本身已经是类型导向的，追踪把预算花在 AST 节点分派上，而 tier1 的带类型字节码免费就去掉了它（research/03 §b.2 的 PyPy 条目、§c 表 3 的「poor fit」）。**启动。** 四个选项里最差：没有定宽字节码产物，就没有可 mmap 的 dsmod（BL-7），没有 `ds build --exe` 的字节码故事，也没有按 profile 合成超级指令的底座。每次运行都要重新解析或反序列化 AST。

| 维度 | 评估 |
|---|---|
| 性能天花板 | 预热之后很高，但天花板的原因（消掉动态分派）对带类型解释器几乎不适用（research/03 §b.2） |
| 实现复杂度 | 持续成本最高：追踪记录器、循环检测、追踪链接、作废，LuaJIT 规模的机械，理由却更少（research/03 §d 管线 E） |
| 启动 | 长跑程序要多秒预热、完全没有字节码缓存的故事 |
| 可移植性 | 绑在元 JIT 工具链上（RPython 或等价物） |

排除：BL-7 整个作废（章程硬性要求 1 说内部 IR/字节码「是允许的，并且是预期的」，这个选项拒绝它），J 变体的分层钩子，以及 docs/zh/design/06 的 dsmod/热重载契约。**结论 — 否。**

### H1.d — wasm-AOT 作为主要目标，解释器兜底（MoonBit 姿势）

带类型 SSA 编译到 wasm 3.0（`return_call` 于 2025 年 9 月标准化，把 TCO 保证沿引擎分层传下去）。解释器是开发与调试底座，也是没有 wasm 3.0 的引擎的兜底（research/03 §b.7 的 MoonBit、§b.5 的平台继承、§d 管线 D）。

```
release: foo.ds -> [check] -> typed SSA -> wasm 3.0 module -> engine (AOT/Liftoff)
dev:     foo.ds -> [check/emit] -> dsmod -> tier1 interpreter (instant edit-run)
```

先例：MoonBit，带类型核心 IR 到 wasm/JS/原生，在原生 JS 迭代器辅助函数上快 25 倍，比 Json5 快约 8 倍，归因于编译器层的迭代器融合与尾递归转循环。这是存在性证明：一个只有带类型 IR 的小团队能在 wasm 上做到接近 C 的吞吐（https://www.moonbitlang.com/blog/js-support，research/03 §b.7）。**层间切换。** 运行期没有切换，「分层」变成构建模式的选择（dev 用解释器，release 用 wasm）。引擎内部的分层是继承来的，自己不管。**去优化范围。** 没有（AOT）。调试靠解释器加 wasm 源映射。栈上限归引擎。**相对 C 的性能目标。** 引擎里为 C 的 1.5-2.5 倍（wasm 约原生 1.55 倍几何均值，IMC 2019，https://ar5iv.labs.arxiv.org/html/1901.09056），随 wasm 3.0 的特性继续收窄。如果再加上经 Cranelift 的原生 AOT，就等于 H1.a 的 tier3。**启动。** 每个模块要做 wasm 编译（有快速分层，但引擎差异是真实的）。除非仍然维护解释器，否则丢掉即时编辑运行的循环，而一旦维护解释器就有两套运行时，这正是 H1.a 减去 tier2 的形态。

| 维度 | 评估 |
|---|---|
| 性能天花板 | wasm 上为 1.5-2.5 倍、只有日后补上原生后端才有 1-2 倍 |
| 实现复杂度 | 低到中等：一个 wasm 发射器远比任何 JIT 简单（research/03 §d 管线 D） |
| 启动 | 取决于引擎的 wasm 编译、解释器兜底保住脚本编写的循环 |
| 可移植性 | 最大：浏览器、沙箱、嵌入，四个选项里最强的故事 |

排除：微秒级的中间层（wasm-AOT 之下 copy-and-patch stencil 没有意义）。与 docs/zh/design/03 E2b 的纤程交互很差，那些纤程得降级成 wasm 线程/JSPI 或 JS async，不能降级成解释器数据结构，这是 E2b 的推荐目前没付的真实成本（research/03 §d 管线 D 的权衡）。**结论 — 不作主力，作为 H1.a 的 tier3 生长出来的部署目标可以。**

### H1 对比表

| 维度 | (a) 分层 C&P 加 Cranelift/wasm | (b) 单一方法 JIT | (c) AST 加元追踪 | (d) wasm-AOT 为主 |
|---|---|---|---|---|
| 相对 C，峰值 | tier3 为 1-2 倍、tier2 为 1.5-3 倍 | 编译后 1-2 倍 | 多秒预热后 5-15 倍、有记录在案的慢 2 倍情形 | 1.5-2.5 倍（wasm） |
| 相对 C，地板 | tier1 为 3-10 倍，没有悬崖 | tier1 地板 3-10 倍 | 0.2 秒以下的脚本拿不到收益（死区） | 解释器兜底 3-10 倍 |
| 实现复杂度 | stencil 管线加 SSA 降级、约 70% 的动态 VM 分层风险被删掉（research/03 §b.2） | 一个编译器，但背着「要么 OSR，要么接受损失」的债 | 持续成本最高：记录器、链接、作废 | 最低：一个 wasm 发射器，没有自有 JIT |
| 启动 | 瞬时 tier1、微秒级 tier2 触发 | 瞬时、热函数首编毫秒级 | 多秒预热、完全没有字节码产物 | 引擎侧 wasm 编译、解释器保住开发循环 |
| 可移植性 | 按架构的 stencil CI、未验证平台退回只跑 tier1 | JIT 选项里最好（Cranelift 多架构） | 绑在元 JIT 工具链上 | 最大：浏览器、沙箱、嵌入 |
| 可逆性 | 严格增量：去掉 tier2 或 tier3，tier1 完好 | tier1 完好、那一个 JIT 可替换 | 最难回退：解释器是为 JIT 写的 | 一次部署模式切换，而非重写 |
| 章程/基线冲突 | 无 | 失去 BL-5 的摊薄 | BL-7 作废、章程硬性要求 1 被拒绝 | E2b 纤程的降级成本 |

**推荐（H1）：(a)**。先 tier1，再 copy-and-patch，最后 Cranelift/wasm，全部跑在一套 uop 与一次早建的字节码到 SSA 降级之上，这样其他每一层都保持增量（research/03 §d 的顺序）。如果 stencil 工具链被证明不可接受，(b) 是退路。(d) 是 (a) 的部署模式，不替代 (a)。

## 3. H2 — 尾调用

章程硬性要求 3 把保证的 TCO 列为固定输入，所以 (b) 到 (d) 三个选项的打分方式就是看它们放弃了什么。机制上的证据：Lua 的 `luaD_pretailcall`（`lua/ldo.c` 约 671 行）复用 CallInfo，把被调用方与实参向下滑动，重置 pc，置 `CIST_TAIL`，成本 O(参数个数)，对自身与互递归尾调用都正确。Koka 把每个函数体降级成一个带 `kk__tailcall:` 标签的循环，先用临时变量重新绑定参数（`genTailVarBinding`，`FromCore.hs` 约 2817 行）。LuaJIT 的 CALLT/CALLMT 把实参复制到调用方的栈帧槽位再跳转。wasm 3.0 的 `return_call` 让整个平台继承这份保证（research/03 §b.5、§c 表）。

### H2.a — 从第一天起在规范里保证 TCO（自身与互递归），栈帧复用加槽位叠加 ABI

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

让这一点到处都成立的 ABI 约束是：被调用方的实参槽位叠加在调用方的实参槽位之上，并在栈帧布局时锁定（BL-6）。调用方的出参区就是自己栈帧的尾部，被调用方的形参槽位正落在这里，所以一次尾调用最多编译成 O(增量) 的实参滑动加一次 jmp。返回地址与栈帧指针从不移动，栈在任何深度都不增长（research/03 §b.5 的机器级草图、§d「两个要早点锁定的设计决定」）。JIT 层把自身尾调用降级成 Koka 的标签循环（`goto kk__tailcall`，C 编译器看到的是一段普通循环，因此返回地址与分支预测都最佳），把互递归尾调用降级成槽位叠加重排加 `jmp callee_entry`（LuaJIT 的 CALLT）。两者都是 O(参数个数)、不分配，且 RC 流量为零，因为被移动的实参转移了所有权（research/03 §b.5「Interactions」第 3 条）。

**TCO 与纤程（docs/zh/design/03 E2b）。** 纤程栈是解释器数据，即一段住在堆上的栈帧/CallInfo 区段。切换是一次指针交换，O(1)（docs/zh/design/03 的共享前提，research/02 §8）。栈帧复用发生在纤程自己的区段内部，所以尾递归同样不会让纤程栈增长：一个刚创建就开始尾循环的纤程会永远停在初始区段大小，nursery 的 spawn 成本不受影响。这个设计里不存在纤程栈与 TCO 的冲突。唯一的规则是纤程区段的增长（深度非尾递归）用块链表追加，不用 realloc，这样已捕获的栈帧地址在增长前后保持稳定。**TCO 与 copy-and-patch stencil。** stencil 集合只需要两个与调用相关的模板：`TAILCALL`（寄存器重排加 `jmp <H callee_entry_descriptor>`）与 `RET`。编译单元内部的自身尾调用就是标签循环，完全不需要 stencil。跨层尾调用经过一个两字的被调用方入口描述符 `{tier1_prologue_ptr, tier2_code_ptr}`：从编译代码尾调用到一个还没编译的被调用方，就跳到它的 tier1 序言（一次分派，栈不增长），等被调用方后来被编译，描述符里的一个字翻转，这与热重载是同一套作废故事（docs/zh/design/06 G3.b 的双版本规则：正在运行的栈帧在返回前继续跑旧代码）。**调试器。** 栈帧复用会销毁调用方栈帧，所以回溯要携带显式的尾调用链元数据（Lua 的 `CIST_TAIL` 在完整 TCO 下仍然支持 `debug.traceback`），被省略的栈帧只在调试器询问时才惰性物化（research/03 §b.5「Interactions」第 2 条）。**效应。** 这份保证的措辞是「同一效应上下文内的正经尾调用」：在 `handle` 边界下的尾调用会为续延物化一个栈帧（docs/zh/design/03 E1d 的处理器运行期，Koka 的一次性续延就是这可以接受的证据，research/03 §b.5「Interactions」第 1 条）。

先例：Scheme/R7RS 的规范级保证、Lua `luaD_pretailcall` 加 `CIST_TAIL`、LuaJIT CALLT/CALLMT、Koka `kk__tailcall`、wasm 3.0 `return_call`（research/03 §b.5 的各语言表）。权衡：规范上的承诺逼着尾位置成为一个有定义的概念，也逼着回溯携带链元数据，这是不大的规范与调试器成本，只付一次。tier1 的实现成本是一次指针重排加实参滑动。排除：ES6 那种「规范有、引擎无」按构造就不会出现（tier1 先实现它），以及任何把深度递归留给 JIT 行为的设计。

### H2.b — 尽力而为的 TCO（取决于 JIT）

CRuby 的编译期尾调用选项默认关闭，截至 2025-2026，YJIT 与更新的 ZJIT 都不把尾递归调用改写成循环（http://blog.tdg5.com/tail-call-optimization-in-ruby-background/ 与 ruby-core 的 ZJIT 讨论串，research/03 §b.5）。JIT 团队一直把 TCO 排到最后，因为栈帧消除与去优化元数据、栈内省纠缠在一起，而这正是动态 VM 背着、DreamScript 不背的那套机械。**层间切换。** 没有可设计的切换，陷阱就在这里：这份保证的可用性变成「哪一层编译了调用方」的属性。**去优化范围。** 不变，但回溯与栈上限行为现在取决于 JIT 覆盖范围。**相对 C 的性能目标。** 生效处与 H2.a 相同、不生效处栈无限增长。**启动。** 不变。权衡：省下 ABI 承诺（槽位叠加）与尾链元数据、代价是章程拒绝交易的那一点上的可预测性（硬性要求 3）。排除：规范级保证，以及脚本语言常被用来写的深度递归惯用法（状态机、对无界结构的折叠）。**结论 — 否。**

### H2.c — 用户层蹦床（库）

```ts
enum Bounce { Done(Bool), Call(fn() -> Bounce) }         // library type
fn even(n: i64): Bounce { if n == 0 { Done(true) } else { Call(() => odd(n - 1)) } }
run(trampoline(even(1_000_000)))                          // driver loop jumps thunk to thunk
```

先例：Chicken Scheme 的「Cheney on the MTA」，每次调用返回下一个 thunk。简单、可移植，但每次调用多一次间接跳转，并且让原生 JIT 的互操作变复杂（research/03 §b.5 的蹦床条目）。**层间切换。** 驱动循环是用户代码、JIT 看到的是一段普通循环，可能会编译它，于是 TCO 的质量取决于能否内联 thunk，而这正是我们删掉的猜测形态的工作。**去优化范围。** 不变，外加每次弹跳一次的闭包分配（每个 `Call` 装箱一个 thunk），寄存器里的 ADT 收益（research/03 §b.4）恰好在热路径上部分作废。**相对 C 的性能目标。** 比 H2.a 差：H2.a 做一次重排，这里每次迭代多一次间接调用加一次分配。**启动。** 不变。权衡：运行期零承诺，负担全在用户身上。这个约定会传染库触及的每一个尾递归签名（调用方与被调用方都得说 Bounce）。排除：TCO 作为语言隐形属性这件事。它唯一的优势是能移植到任何宿主。**结论 — 否作机制，作为效应上下文边界的逃生口可以接受，那里 H2.a 的保证被划在范围之外。**

### H2.d — 尾式恢复只给效应（Koka 线性效应姿势）

只有效应操作获得恢复式的非栈控制。普通递归无界增长栈。与 docs/zh/design/03 E1d 相关（底下是处理器运行期，表层没有效应语法）：效应延后之后，这个选项在 v1 里退化成「完全没有 TCO」，直接违反章程硬性要求 3，这正是 research/03 把在语义里保证 TCO 列为 Ruby 从未做过的刻意选择的原因（research/03 §b.5 的反证段落）。**层间切换。** 不适用。**去优化范围。** 处理器捕获要物化栈帧（H2.a 已经预算了同样的成本）。**相对 C 的性能目标。** 受深度限制的程序会溢出。语义缺失的地方性能天花板无关。**启动。** 不变。权衡：如果效应是唯一的捕获机制，运行期机械最少、代价是直接放弃保证。排除：章程硬性要求 3。只有在 docs/zh/design/03 E1 落地后作为 H2.a 的补充才可重开，从不作为替代。**结论 — 否。**

### H2 对比表

| 选项 | 保证强度 | tier1 成本 | JIT 层成本 | 规范/回溯成本 | 契合章程硬性要求 3 |
|---|---|---|---|---|---|
| (a) 第一天就保证 | 自身与互递归，恒定 | 实参滑动加栈帧复用（接近免费） | 标签循环或叠加 jmp 加一个 TAILCALL stencil | 尾位置有了定义、链元数据 | 是 |
| (b) 尽力而为 | 无、取决于层 | 无 | 推迟，常常永远不做（Ruby YJIT/ZJIT） | 无，但回溯误报深度 | 否 |
| (c) 蹦床 | 只有用户强制 | 库循环，每次弹跳一次分配 | 内联 thunk 属于猜测形态的工作 | 约定传染每个签名 | 否 |
| (d) 只做效应 | 只有恢复 | 效应落地前没有 | 处理器处的捕获物化 | 限于效应上下文 | 否（在 E1d 下退化成「没有 TCO」） |

逐语言的证据（research/03 §b.5 的表）：Lua 5.4/5.5 靠复用 CallInfo 保证（`luaD_pretailcall`、`CIST_TAIL`）、LuaJIT 靠 CALLT/CALLMT 的实参滑动保证、Scheme/R7RS 由语言要求保证、Koka 靠 `kk__tailcall` 标签循环做到、wasm 引擎靠 wasm 3.0 的 `return_call` 保证（V8 自 Chrome 112 起）、Ruby 是可选的编译开关，默认关闭，YJIT/ZJIT 从不做、JS 由 ES6 要求，但 V8 从未发布、CPython 没有。规律如下：每一个保证 TCO 的系统自己拥有调用约定。每一个指望 JIT 以后补上的系统，到 2026 年仍然没有。

**推荐（H2）：(a)**。规范级的自身与互递归保证，先在 tier1 里实现（那里只是一次指针重排），槽位叠加 ABI 在栈帧布局时锁定，`TAILCALL`/`RET` 这对 stencil 对着两字的被调用方入口描述符来设计。

## 4. H4 — 内存运行时机制

每个选项共享的布局基线（research/03 §b.4、docs/zh/design/01 B8a、docs/zh/design/02 C4）：堆值带一个固定的 8 字节头 `{ u32 type_id; u32 rc }`（对象最小 16 字节，取自 Inko 单次分配的字符串经济性）。ADT、并集与 Result 这类值在寄存器里是拆箱的标签加载荷对。字符串是不可变 UTF-8，用一个裸指针加长度表示（docs/zh/design/01 B8a），切片是 (ptr,len) 槽位对。还有一条每个选项都依赖的结构性事实：一个值当且仅当被移动进装箱或共享上下文时才在堆上分配，因为线性性让逃逸成为编译期事实，而非启发式（research/03 §a.7）。分工：docs/zh/design/02 C4 决定内存管理策略（它推荐 Perceus RC 加复用，并用 arena 落实）。这里的 H4 决定该策略之下的运行期机制，即分配器路径、停顿行为、循环策略，以及每个 JIT 层必须与之互操作的东西。

### H4.a — Perceus 精确非原子 RC 加块复用加逐帧 bump arena 存放不逃逸的临时值

两套机制。第一，给解释器临时值用的逐帧 bump arena：分配是一次指针加法，回收是栈帧弹出时的一次恢复。之所以成立，正是因为线性性禁止悄悄逃逸，凡是离开栈帧的东西都是被移动的，而证明这次移动的最后使用分析就是检查器已经在跑的那一份（research/03 §a.8、§b.6 的 arena 条目）。第二，给真正装箱的值用 Perceus 风格的精确 RC 加块复用：每次显式 clone 处 `rc_inc`，每个值的唯一死亡点处 `rc_dec`（在仿射检查下语法可见，docs/zh/design/02 C1.b），每次丢弃都作为复用候选串到下一次分配上。Koka 的 `kk_block_alloc_at(reuse, ...)` 只在没有候选时退回 mimalloc（research/03 §b.6、`kklib.h`、复用处理是 `ParcReuse.hs`，它强制「先复制后丢弃」的顺序，而线性性免费给我们这个顺序）。

```
let x = pair(1, f(y))            // pair is boxed; f consumes y linearly
tmp2 = f(y)                      // y moved: no rc_dec (moved, not dropped)
rc_dec(tmp2)                     // tmp2 dies after field extraction -> reuse candidate
reuse = (rc(tmp2) == 1) ? tmp2 : NULL
x = alloc_at(reuse, sizeof(pair))// reuses tmp2's block when it died uniquely
x.tag = TAG_PAIR; x.a = 1; x.b = extracted
```

arena 的机械（解释器这一半）：

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

先例：Perceus，Reinking 等，PLDI 2021（https://www.microsoft.com/en-us/research/publication/perceus-garbage-free-reference-counting-with-reuse/）、Koka 的 `Parc.hs`/`ParcReuse.hs`/`kklib.h`、research/03 §a.8、§b.6、docs/zh/design/02 C4 推荐的正是这一套（「Perceus RC 加复用，用 (d) 的 arena 落实」）。H4.a 就是那条推荐点名的运行期机制。**循环处理。** 线性值无法参与循环。循环需要共享（Copyable）的可变单元或惰性结，所以 v0 在类型层拒绝它们（模块级递归特殊处理），并把 H4.d 的收集器接口作为预先规划的逃生口：对已注册的共享单元候选做试探删除，弱引用只用于闭包环境循环，即一个闭包被同时捕获它的可变单元捕获。**停顿行为。** 按构造零停顿。没有写屏障（默认不可变去掉了指针写入），没有安全点，JIT 栈帧里没有 GC 映射（research/03 §a.8）。**分配器流量，如实说明。** research/03 的主张刻意很窄：因为逃逸是类型事实，100% 的未移动临时值住在寄存器或 arena 里。它没有承诺所有分配都免费。诚实的对照物：Inko 需要全程序过程间 EA 加内联才能提升掉 47-58% 的堆分配（HTTP 解析上 2-2.2 倍，启发式、部分、全程序、`inko/compiler/src/mir/escape.rs`），Lobster 的所有权分析以三处罕见的编译错误消掉约 95% 的运行期 RC 操作（research/01 §B.9）。我们剩下的装箱与共享部分仍然付 RC。如果复用处理表现不佳，分配退回 mimalloc 约 15-25 个周期（research/03 §b.12 第 3 条）。所以复用处理要当优化器工作来预算，不要当检查器工作。

| 维度 | 评估 |
|---|---|
| 性能天花板 | 四个选项里分配画像最好：零分配临时值、接近免费的装箱分配、没有屏障、没有安全点（research/03 §c 表 5 的「best fit」） |
| 实现复杂度 | 低到中等：dup/drop 放置退化为在最后使用处做语法插入，再加一趟复用穿线、没有借用或干扰分析（research/03 §b.6） |
| 启动 | 最好：没有收集器要初始化，没有堆要预先映射 |
| 可移植性 | 非原子 RC 在值限定于线程时成立。docs/zh/design/03 E3b 的 fork-join 在分叉与汇合处转移所有权，所以线性层不需要原子操作。跨线程的装箱共享数据需要原子 RC 或限定于线程的堆，这是要留给 docs/zh/design/03 的 E3 交互，这里不做决定 |

排除：没有根本性损失。类型层拒绝循环这条立场必须成为诚实的 API 表层（共享图需要显式的收集器或 arena API）。

### H4.b — 为不可变数据调优的带代搬移式 GC

年轻代回收对不可变数据很便宜：对象死在年轻代，而且因为不可变，晋升后从不存指针，所以记忆集与写屏障（通常是压垮带代 GC 的那套机械）在默认不可变下大部分消失（research/03 §b.6 的带代条目）。

```
alloc: bump in young gen (nursery)          | minor GC: copy survivors, promote
mutate: only in D1/D2 exclusion windows      | barrier on windowed pointer stores only
JIT: safepoints + precise stack maps per tier | roots = typed frame slots + headers
```

先例：OCaml/Haskell/V8 的年轻代（research/03 §b.6）、V8 的 Orinoco 机械规模（https://v8.dev/blog/trash-talk）。任务要求的诚实复盘：Inko 发布过并行 immix 搬移式 GC，公布了它的移除（「Friendship ended with the garbage collector」，https://yorickpeterse.com/articles/friendship-ended-with-the-garbage-collector），然后在 0.10.0 里彻底删掉 GC，改为所有权加 ARC（`inko/rt/src/arc_without_weak.rs`）。这是一个十年尺度的信号：对带所有权风味的语言，收集器是复杂度而没有回报（research/03 §a.8、§b.6 的 Immix 条目）。**循环处理。** 按构造免费（追踪式）。**停顿行为。** 小、可调，但真实存在，是菜单上唯一有停顿的选项。**分配器流量。** 保留共享的选项里最高：复制加可变窗口上的屏障（docs/zh/design/02 D1/D2）。成本：每个 JIT 层都要安全点与精确栈映射，扫描带类型栈帧作为根，以及与 docs/zh/design/01 B8a 的直接冲突：搬移对象会让单裸指针的字符串表示失效，逼着每次字符串访问都做 pinning 或句柄间接。排除：docs/zh/design/02 C5.c 的确定性线性 RAII 析构（丢弃变成终结器时刻，而这正是线性资源层存在的理由）、零停顿的主张。**结论 — 否。**

### H4.c — 每个对象 malloc/free，不做 GC（纯线性所有权）

每次分配在语法上的死亡点配对一次 free。没有共享，于是不需要 RC。

```
let xs = List.make(1_000_000)     // owned
let ys = xs.pop()                 // ??? the tail: owned by xs (dead) and ys (alive)
// pure ownership forces a deep copy of the tail, or forbids pop-and-keep entirely
```

具体在哪里破。第一，共享闭包：逃逸闭包的环境必须比创建它的栈帧活得更久，而捕获的变量之后可能还要使用。唯一所有权逼着深拷捕获物，或者禁止逃逸闭包，而函数式风格的 DreamScript 让闭包成为日常写法（research/03 §b.6 的 RC 与其它方案对比）。第二，持久化集合：结构共享意味着两个版本引用同一个尾节点，丢弃其中任何一个头节点都会双重释放，所以纯所有权放弃了默认不可变章程（硬性要求 5）指向的那半个语言。第三，异常与纤程栈：捕获的续延（docs/zh/design/03 E1d 的处理器运行期）与纤程栈（E2b）是住在堆上的栈帧，引用的值归已挂起的栈帧所有，栈帧槽位的所有权于是变成动态的，正是栈纪律无法表达的情形。第四，循环：悄悄泄漏，或者需要手工池。**循环处理。** 没有收集器就不可能。不可达的循环泄漏。**停顿行为。** 零停顿，但分配器流量是四个选项里最高的（每个对象付一次 malloc/free，即 CPython 的成本模型减去装箱），丢弃持久化结构是 O(n)。**相对 C 的性能目标。** 分配密集的函数式代码是这个选项最差的负载类别，而它正是这门语言的主要负载类别。**启动。** 可以。排除：共享状态上的闭包、持久化集合、效应续延，也就是这门语言。**结论 — 否作全局机制。只作为 H4.a 的 arena 之下的底座可行（arena 就是带整体释放的 malloc/free）。**

### H4.d — 混合：共享/装箱值用 RC 加线性数据用 arena 加可选的并发循环收集器延后

与 H4.a 同样的两套机制，但作为策略陈述，并带一条明确的成长路径：arena 覆盖所有已证明不逃逸的线性数据（包括通过带区域标注的 API 达到更长寿命的线性缓冲区，不只是逐帧临时值），Perceus RC 覆盖共享与装箱的值，而在共享可变单元之上做试探删除的并发循环收集器是预先承诺的可选组件：从不在热路径上，也从不对线性数据适用，因为线性数据不会形成循环。

```
// the committed interface, two hooks + one header bit:
block.header.mark_suspect()             // set when a shared mutable cell is born
runtime.register_cycle_candidate(block) // collector scans/trial-deletes off-thread
weak<T>                                 // weak refs for closure-environment cycles
```

先例：research/03 §b.6 的「循环只能通过显式可变引用单元或惰性结表达。要么在类型层拒绝它们……要么提供一个带试探删除的显式 `unsafe_cycle`/arena API，让 VM 保持没有通用收集器」。这个选项与 H4.a 的差别只在承诺时机。**循环处理。** H4.a 的类型层拒绝，加上这份为共享密集负载预先规划的收集器接口（带回指指针的持久化 map、观察者图）。**停顿行为。** 修改者零停顿（收集器是并发的，工作量受候选数量限制）。**分配器流量。** 与 H4.a 相同。相对 H4.a 的成本：现在就承诺两个运行期钩子与一个头部位，以及如果收集器永远用不上、这段接口代码却死不掉的风险。排除：H4.a 不排除的东西它也不排除。它只是用今天的接口表层换明天的升级路径。**结论 — 是，作为 H4.a 声明的成长路径**（一个设计，两个承诺级别，现在选 H4.a，并把这一项记为它的文档化扩展，依据 research/03 §a.8 的「同时保持以后能加入并发收集器」）。

### H4 对比表

| 维度 | (a) Perceus 加 arena | (b) 带代 GC | (c) 只有 malloc/free | (d) 混合加延后收集器 |
|---|---|---|---|---|
| 停顿 | 无 | 小、可调、真实存在 | 无 | 修改者无停顿（收集器并发） |
| 分配器流量 | 最低：零分配临时值，复用让装箱分配接近免费 | 高：复制加窗口化屏障 | 最高：每对象一次 malloc/free | 与 (a) 相同 |
| 循环处理 | 类型层拒绝、收集器接口作为成长路径 | 按构造免费 | 不可能（泄漏） | 在候选上预先承诺的试探删除 |
| JIT 交互 | 无屏障、无安全点、栈帧里无 GC 映射 | 每一层都要安全点加精确栈映射 | 无 | 收集器启用前无 |
| 冲突 | 无循环这条立场必须成为诚实的 API 表层 | docs/zh/design/02 C5.c 的析构、docs/zh/design/01 B8a 的裸指针字符串需要 pinning | 闭包、持久化集合、续延、纤程 | 两个钩子加一个头部位提前承诺 |
| 证据 | Perceus PLDI 2021、Inko 47-58% 的 EA 作启发式对照、Lobster 约 95% 的 RC 消除（research/01 §B.9） | Inko 移除 immix，之后彻底移除 GC（0.10.0） | 没有这个规模上的生产语言 | Koka 自己的路线图形状 |

**推荐（H4）：(a)**。Perceus 精确非原子 RC 加块复用加逐帧 bump arena，并把 H4.d 的两钩子收集器接口（循环候选注册、弱引用）从第一天就记为它的成长路径，好让共享与带循环的负载拿到一条永不需要重设计 VM 的升级路径。

值得记录的交互：H4.a 的 arena 是逐帧的，绑定在栈帧的栈范围上，所以纤程切换（docs/zh/design/03 E2b）不需要任何 arena 动作：每个纤程的栈帧自带标记，纤程区段增长是块链表追加，不用 realloc，所以 arena 标记在增长前后依然有效。TCO 让内存这边的交互更便宜：尾调用移动实参时转移所有权，RC 流量为零（research/03 §b.5），这就是 H2.a 与 H4.a 成对设计、而非两个独立选择的原因。又因为 H4.a 里没有任何东西在分配之后搬移堆对象，docs/zh/design/01 B8a 的单裸指针字符串表示才是安全的。换成 H4.b 就需要 pinning，换成 H4.c 就会随不可达循环一起泄漏。

## 5. 静态类型从 JIT 里删掉了什么：释放出来的复杂度预算

research/03 对这门语言的核心主张：每个被调研的 JIT 的大部分都在做运行期类型或形状猜测，而静态类型几乎无条件地把它全部删掉（research/03 §a.3、§b.3）。这就是 H1.a 拿去花在 stencil 与 SSA 上的预算。

| 动态 VM 里的子系统 | 在宿主里的规模 | 在 DreamScript 里的状态 |
|---|---|---|
| 隐式类/映射与迁移 | V8 的对象表示核心 | 删除：字段偏移是字节码立即数（`FIELD_LOAD %dst, %obj, #12`），参见 LuaJIT `IRFLDEF`（research/03 §b.3） |
| 字段访问内联缓存与反馈向量 | 按站点的内存，喂分层的基建 | 完全删除，内存收回（research/03 §b.3 第 1、5 条） |
| 类型守卫与去优化栈帧 | LuaJIT `IRT_GUARD` 加 `SnapShot`、Maglev 的映射检查 | 数据路径上删除。剩下的是栈上限比较，以及在 B7a 溢出即陷入之下的那条冷溢出边（research/03 §b.2 的去优化表） |
| 猜测记录 | `lj_record.c` 加 `lj_opt_*.c`，推断前端已经证明的类型 | 删除：类型是编译期常量（research/03 §b.3） |
| 侧追踪、退出计数器与追踪链接 | `lj_jit.h` 的 `SnapEntry.count` 侧追踪提升 | 删除：单入口单元，没有悬崖（Cloudflare 记录的 LuaJIT 不一致在结构上不可达） |
| 反馈驱动的分层预算 | V8 leaptiering、CPython `_SPECIALIZE_*` | 简化成普通的热度计数（BL-4/H1.a） |

同一份账的去优化一侧，按系统列出（research/03 §b.2 的去优化表）：

| 系统 | 守卫形态 | 退出成本 | 退出机制 |
|---|---|---|---|
| LuaJIT | 标了 `IRT_GUARD` 的 IR 操作（类型、范围、相等） | 快照遍历，按 `SnapEntry` 映射恢复 nslots | 在字节码 PC 处退到解释器或侧追踪 |
| V8 Maglev/TurboFan | 映射检查、Smi 检查、边界 | 从翻译状态构建去优化栈帧 | 重新进入解释器，可能重新优化 |
| CPython tier 2/3 | `_CHECK_*` uop、紧凑性检查 | 侧退出到 tier 1 执行器 | 每条追踪一份全局侧退出列表 |
| Truffle | 节点假设 | `transferToInterpreter()`，重写 AST | 重新执行 AST，重新编译 |
| DreamScript | 数据路径上没有、每函数入口一次栈上限比较 | 不适用（没有守卫） | 只有调试器驱动的栈帧物化 |

用两种方式量化。运行期：每次操作的节省累积成 tier1 的 3-10 倍估计，i64 加法比 ADDVV 的标签路径少约 3-6 倍 uop，字段访问约 2-4 倍，静态调用约 2-3 倍，ADT 构造约 5-20 倍（分配），Result 匹配约 2-3 倍，循环迭代 1.0 倍（分派是无法消除的那部分）（research/03 §b.11 的表）。工程上：research/03 估计把守卫与去优化这一层收掉，分层的运行期成本与工程风险都降低约 70%（research/03 §b.2 的去优化段落）。释放出来的预算正好付 stencil 管线、SSA 降级与调试器栈帧物化。以 IC 形式留下的东西全是性能提示，不承担正确性职责：dyn/trait 调用点上 1-2 条按类型 id 索引的 IC、以模块版本计数器为键的热重载模块全局缓存、以及逐帧的效应处理器操作表（research/03 §b.3 的「Where IC-style mechanisms remain useful」）。净账：动态 VM 的 JIT 大约背着六个猜测子系统，DreamScript 用两个产物替换掉它们，即一个 stencil 补丁器与一趟降级。这个比例，而非任何单个基准，就是本节关于复杂度预算的主张。

## 6. 值表示的后果（由 docs/zh/design/01 负责，这里引用，不重新决策）

这些是操作码集合与 stencil 集合从类型目录的推荐里继承来的下游义务，决定本身住在 docs/zh/design/01。下面每一条都不重开该文件的任何决定，只说明那条推荐对本文件各层提出的操作码级、槽位级或 stencil 级义务。

- **B7 数值（docs/zh/design/01 B7，推荐 (a)：Int/Float 默认加溢出即陷入）。** 槽位是定宽 8 字节的拆箱 i64/f64。算术操作码按表示区分（ADDI64/ADDF64），没有槽位是双重类型的，动态 VM 那套 DUALNUM 机械在这里不存在（research/03 §c 表 4）。溢出即陷入给每个算术 uop/stencil 加一条冷溢出边（`add; jo -> panic stub`）：快乐路径上只有一条被标记的分支，与 research/03 §b.2 的 ADDOV 建议一致（溢出即 panic，预测路径上零检查，显式受检操作作为库函数）。任意精度回退（B7 后来的库类型）走同一条边进 panic 路径，从不进 tier1 操作码。
- **B4c 可空性（`T?` 是 `T | none` 的零语义别名）。** `none` 占一个标签槽位、零载荷槽位。多槽位的 `T?` 和任何并集一样，把标签加载荷摊在连续栈帧槽位上（research/03 §b.4 的 ADT 布局示例）。空检查编译成 TAGTEST 加 JMPIF，可以融成一条超级指令（BL-4）。装箱 Option 的分配项（约 5-20 倍，research/03 §b.11 第 5 行）在检查的两侧都不会出现。
- **B9a 相等性（通用结构化 `==`，带若干例外）。** 结构化相等按静态操作数类型降级：整数用 EQI64，浮点用 EQF64（把 NaN != NaN 与 ±0.0 的语义钉在操作码层），装箱聚集体先按引用同一性再逐字段遍历。浮点切分的后果：对一个混有 Int 与 Float 成员的并集做 `==`，编译成在这些切分操作码上的标签分支。一个运算符，N 份静态降级，全部是静态操作码。这是 docs/zh/design/01 B9a 在这里建立的操作码集合义务，也是它的五条例外对跨表示相等有意义的原因。
- **B8a 字符串（不可变 UTF-8，一个裸指针）。** 切片是 (ptr,len) 槽位对。EQSTR 先走指针同一性快路径，再 memcmp。驻留只用于短小的热标识符（research/03 §b.4 的字符串）。搬移式 GC（H4.b）会需要为那个裸指针做 pinning 或句柄，这是 H4.a 的非搬移 RC/arena 底座契合 docs/zh/design/01 的又一个理由。
- **泛型字典（语义由 docs/zh/design/05-generics-metaprogramming.md 负责，只按路径引用）。** research/03 §b.8 的立场：没有语言级特化。字典传递而字典拆箱。单方法 trait 的字典就是寄存器里的一个代码指针，数值表示多态的操作拿到的按表示区分的代码，恰好就是 tier2 的 stencil 集合。于是值表示对原语删掉了字典传递那个经典装箱惩罚，也没有新增任何操作码类别。
- **B3 积类型（docs/zh/design/01 B3，推荐：名义 struct/enum 加匿名元组）。** struct 字段是连续的带类型槽位。字段访问是带立即数偏移的加载，没有检查，即 research/03 §b.3 的 `FIELD_LOAD %dst, %obj, #12` 形式。元组构造与解构是寄存器写入，从不分配。research/03 §b.11 那条约 5-20 倍的 ADT 构造节省适用于每个积类型，不只是 enum。

## 7. 一致的组合

在 3x3 的 H1xH2xH4 空间上有三个命名的组合。未列出的组合都被支配：H1c 违反 BL-7 与章程硬性要求 1。H2c/H2d 作为机制违反章程硬性要求 3。H4c 放弃这门语言的核心负载。每个组合的阅读指南：文字版的管线图、带证据锚点的性能目标表、v0 的实现顺序，以及对 docs/zh/design/02（线性性、析构）、docs/zh/design/03（错误、效应、纤程）与 docs/zh/design/06（模块、工具）的影响。一个组合只有在其成员不排除另一个成员的基线时才是一致的。

### Combo A — 「分层解释器优先」：H1a 加 H2a 加 H4a（推荐）

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

| 负载/阶段 | 相对 C | 证据锚点 |
|---|---|---|
| tier1，标量/数值 | 3-10 倍 | research/03 §b.10 |
| tier1，ADT/Result 密集的函数式 | 3-10 倍（零分配临时值） | research/03 §b.11 第 5-6 行 |
| tier1，调用密集的函数式 | 分派是唯一无法消除的成本、精简序言加超级指令 | research/03 §b.12 第 1 条、§b.9 的调用开销条目 |
| tier2（copy-and-patch） | 1.5-3 倍 | arXiv:2011.13127（比 -O0 好约 14%，速度快 100 倍） |
| tier3（Cranelift）/ wasm 部署 | 1-2 倍 / 1.5-2.5 倍 | research/03 §c 表 1、IMC 2019 |
| 对照用的动态基线 | Lua 20-50 倍、CPython 50-100 倍、PyPy 多秒预热后 5-15 倍 | research/03 §b.10 |
| 最差情形 | 到处是 tier1 的地板、没有悬崖 | research/03 §b.2 的去优化表 |

v0 的顺序（按顺序建什么）：(1) dsmod 格式加 mmap 加载器加带类型字节码发射器，锁定栈帧布局与槽位叠加的调用 ABI（BL-6/BL-7，H2.a 的约束），下游一切都对着它编译。(2) tier1 解释器：computed-goto 的 uop 循环、拆箱槽位、ADT 标签加载荷、CALLT 的栈帧滑动 TCO，这就是 3-10 倍的核心，大约一名强工程师一年（research/03 §d 管线 A）。(3) Perceus 的 dup/drop 放置加复用穿线加逐帧 arena。(4) 纤程与 nursery（docs/zh/design/03 E2b）作为堆上的栈区段。(5) 按 profile 反馈合成超级指令，并从第一天起预留 J 变体的操作码空间。(6) tier2 的 stencil 管线（构建期用 LLVM，先 x86-64，再 aarch64，其余平台只跑 tier1，即 CPython 的自选姿势）。(7) 字节码到 SSA 的降级，早建，在被 tier3/wasm 需要时启用（research/03 §d 的顺序）。影响：docs/zh/design/02 的 C1.b 仿射核心加 C4.a 的 Perceus 配 arena 正好就是这个底座，C5.c 的析构钩子在 rc_dec 与复用点运行，而 arena 重置对它们不可见。docs/zh/design/03 的 E2b 纤程在 TCO 保持初始大小的区段上拿到 O(1) 切换，E1d 的处理器入口是唯一一条栈帧物化路径，E4b 的并集错误走拆箱标签加载荷。docs/zh/design/06 的 G3.b/G3.5 的 dsmod 就是层间切换的底座（原地打 J 变体补丁与热重载作废是同一套机制），H6 的存储按内容哈希缓存 dsmod，`ds build --exe` 内嵌依赖闭包。docs/zh/design/05（只按路径引用）的 derive 生成的 Ord/Hash/Show 函数体编译成同一批 uop 与 stencil，所以派生实例不需要额外的分层成本。

### Combo B — 「wasm 优先」：H1d 加 H2a 加 H4d

```
release: foo.ds -> typed SSA -> wasm 3.0 module (return_call = TCO) -> engine
dev:     foo.ds -> dsmod -> tier1 interpreter (instant edit-run, TCO in-VM)
memory:  arenas for linear data | RC for boxed | collector interface pre-committed
```

| 路径 | 相对 C | 证据锚点 |
|---|---|---|
| wasm 3.0 AOT | 1.5-2.5 倍 | IMC 2019 约 1.55 倍几何均值（research/03 §b.7） |
| MoonBit 调优过的同一管线 | 基准上接近 C | 相对 JS 迭代器 25 倍、相对 Json5 约 8 倍（moonbitlang.com/blog/js-support） |
| 解释器兜底 | 3-10 倍 | research/03 §d 管线 A |
| 预热 | 取决于引擎分层 | 没有自己的分层 |

顺序：(1) 先做带类型 SSA 降级加 wasm 发射器，发布路径就是产品。(2) tier1 解释器作为开发底座。(3) wasm 里用 `return_call`，解释器里用栈帧复用，两套机制一份保证，由同一套测试验证。(4) 持久化集合发布前先做 H4.d 的接口钩子。影响：docs/zh/design/02 的 C1.b/C4.a 不变（RC 住在 wasm 线性内存里）。docs/zh/design/03 的 E2b 纤程是最贵的部分，要降级成 wasm 线程/JSPI 或 JS async，不能降级成解释器区段（这个组合的主要隐性成本）。docs/zh/design/06 的 G3 的多后端经济学逐字就是 MoonBit 模式，H6 的产物变成 wasm 组件（H5.d）。docs/zh/design/01 的类型层不变，但 B7a 的溢出即陷入边骑在 wasm 自己的陷入语义上，panic 桩因此由宿主引擎中介。

### Combo C — 「简单方法 JIT」：H1b 加 H2b 加 H4b

```
run foo.ds -> dsmod (mmap) -> tier1 interpreter (3-10x)
  hot function counter -> tier2 method JIT: SSA -> CLIF (1-2x, ms/function)
  memory: generational GC | safepoints + stack maps in JIT frames | pauses tunable
  TCO: best-effort — guaranteed only where the compiler rewrites (Ruby posture)
```

| 阶段 | 相对 C | 备注 |
|---|---|---|
| 编译前 | 3-10 倍 | tier1 地板 |
| 编译后 | 1-2 倍 | Cranelift 的质量 |
| 编译成本 | 每函数毫秒级 | 没有微秒级的中间档 |
| 最差情形 | GC 停顿加无界递归 | 唯一同时具备两者的组合 |

这一点必须明说：这个组合放弃了设计其余部分存在的两个保证，即章程硬性要求 3 的 TCO 保证（H2.b）与 docs/zh/design/02 C5.c 的确定性析构（H4.b），并继承 GC 的安全点、精确栈映射与 JIT 栈帧里的写屏障（V8 Orinoco 规模的机械，外加 docs/zh/design/01 B8a 裸指针字符串的 pinning 故事）。它唯一诚实的优点是依赖最小化：没有 stencil CI 矩阵，一个编译器，一个 GC。值得记录的更便宜变体是 H1b + H2a + H4b，它保住 TCO 保证（在 tier1 里几乎免费），只付 GC 的账。若被选中，顺序：(1) dsmod 加 tier1，与 Combo A 的步骤 1-2 相同。(2) 任何 JIT 落地之前先做带栈映射的 GC。(3) 按 Sparkplug 的栈帧保留规则做方法 JIT。(4) 做 OSR，或者接受函数中途热循环的损失。影响：docs/zh/design/02 C5.c 塌掉（变成终结器，而非析构）。docs/zh/design/03 E1d 的处理器捕获需要 GC 安全的栈帧。docs/zh/design/06 的双版本热重载必须对两个模块版本都做 GC 扫描。docs/zh/design/01 B8a 的单裸指针字符串需要 pinning 或句柄故事，这是本组合最不宣传的成本。

## 收尾说明

按 AGENTS.md 阶段 3 等待作者的决策：H1（推荐 (a)）、H2（推荐 (a)）、H4（推荐 (a)，并把 H4.d 的收集器接口作为它声明的成长路径）。把结果记进 `docs/zh/design/00-decision-map.md`，并在之后每一份目录里把 BL-1 到 BL-7 当作已经固定的输入。如果任何点选了非推荐的选项，第 7 节的组合图与顺序计划必须在实现开始前重新推导：它们假定的是自己的命名组合，换掉一个成员会改变 v0 必须先建什么。有两个决定压倒性地承重于每一层：槽位叠加的调用 ABI（H2.a）与定宽位置无关的 dsmod（BL-7，docs/zh/design/06 G3.5）。research/03 §d 点名要早点锁定的正是这两项，因为其上的一切都依赖它们。
