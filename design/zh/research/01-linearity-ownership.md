# DreamScript 研究笔记 01：线性性、所有权与内存管理

日期：2026-11-04。范围：类型系统的线性性（仿射、线性、唯一、量化），所有权模型及其用户可见成本，以及解释执行（IR/字节码）语言可用的内存管理策略。该语言要追求超高性能，同时回避 C++ 级复杂度。

方法：一手资料。九个仓库克隆（`git clone --filter=blob:none --depth 1`，roc 用稀疏检出）到 `Z:/Temp/plr1/*` 后直接阅读，下文引用其路径。这些目录在本文写完后已于 2026-11-04 删除。补充材料来自官方文档、Swift evolution 提案、论文和语言作者博客，全部在 2026-11-03/04 抓取。网络搜索只用于定位一手资料和确认当前状态。

本文假定的作者口味约束（来自 philosophy_of_PL.md）：类型应当是集合论的（并集/交集），排除 C++ 风格特化，采纳单子 Option/Result，采纳 Koka 风格 UFCS，指针保持抽象，总心智负担是首要指标。所有“影响”小节都是针对这些约束写的。

## (a) 概要

最重要的单个发现：**2024-2026 年的趋势是把所有权从用户手里拿走，让编译器自行推断。**

Koka 自 v2.2.0（2021 年 8 月）起在 Perceus 精确引用计数之上具备自动借用推断，在零所有权标注下提供 “Functional But In-Place” 编程。

Roc 新的 Zig 编译器（2025-2026）实现了 ARC 插入，其源码头部的原话是 “borrow inference plus RC statement emission”，并带有跨过程的 owned/borrowed 签名求解与一项 born-unique 分析。Roc 的 FAQ 明确写着，Roc **没有**线性类型、依赖类型、精化类型与唯一类型。

Lobster 通过一次所有权分析得到“编译期引用计数”，消除了约 95% 的运行期引用计数操作，同样不需要用户标注，只留下两类罕见错误。

这三者都把借用检查器藏在编译器内部。借用检查器这一族（Rust）付出了有据可查的学习成本。它 2026 年的答案是 **Polonius Alpha**（流敏感的借用检查，自 2026-08-04 起进入 nightly，计划在 2026 年底前稳定），消除的是误报，学习难度照旧。

Hylo/Swift 展示了中间道路：值语义，加上以参数传递模式表达的二等借用（Hylo 的 `let/inout/sink/set`，Swift 的 `borrowing/consuming`），再加上可选启用的不可复制类型（`~Copyable`，Swift 5.9）。

`~Copyable` 是一个*被取反的 trait 约束*，属于集合论构造，因此不可复制的资源类型能原生地与并集/交集类型组合。这是 DreamScript 口味可用的最好先例。

量化方案（Idris 2 的 QTT 0/1/omega、Granule 的分级模态、Linear Haskell 的 `%m`）在类型里回答“这个值被用了多少次”。它们在擦除保证和协议（资产、会话类型）上很出色，作为默认却很痛苦，其中签名里的多重性*多态*是格外糟糕的部分（Linear Haskell 的 UR 约束之苦）。

内存管理可用的一批策略是：精确的非原子 RC + 复用分析（Perceus/Roc：无垃圾、确定性、最适合解释器），RC + 运行期借用计数（Inko：编译器成本近乎为零、确定性 panic），带代引用（Vale：没有移动错误、访问时检出失效），搬移式/Immix GC（Inko 实现过一个，随后带着有教益的复盘放弃了），以及 arena/平台分配（Roc 把分配器的选择推给平台）。

“Fusion” 研究线（线性类型 x RC x GC）从 Chirimar 1995 一直到 ESOP 2022 的 “Linearity and Uniqueness: An Entente Cordiale”，收敛为一条共识：**用户可能写下的每一条线性事实都能推导为唯一性事实（refcount==1）。就工效学而言推导优于书写，就保证而言书写优于推导。同一套运行时能同时服务两者。**

推荐方向（论证见 d 节的组合 6）：值语义、绑定默不可变的语言。解释器 v0 运行精确 RC，配合用于原地修改的运行期唯一性检查，以及 Inko 风格的借用计数以提供确定性诊断。用 `~Copyable` 风格的线性资源类型承载句柄/资产。Koka/Roc 风格的借用推断、TRMC 与栈帧复用作为后续的编译器优化加入，语言层面零改动。

## (b) 各技术深入分析

### B.1 Roc：用户不可见的所有权、C 级速度，读自编译器源码

状态（2026-11-03 核对）：编译器**已从 Rust 改写为 Zig**，2025 年 2 月宣布。改写耗时 487 天、约 30 万行，Zig 代码库此后增长到约 46.4 万行（https://ziggit.dev/t/roc-lang-to-be-rewritten-in-zig/8363、https://news.ycombinator.com/item?id=42935516、https://www.developersdigest.tech/blog/roc-rust-to-zig-rewrite-feldman）。

改写以来的发布：0.0.0-alpha1（2025-01-29）、0.0.0-alpha2-rolling、alpha3（2025-02-26）、alpha4-rolling（2025-08-26）。主分支在 2026 年 11 月仍活跃。Richard Feldman 的演讲 “A Preview of Roc 0.1.0”（Func Prog Sweden，讨论见 https://www.reddit.com/r/programming/comments/1vn7t7t/a_preview_of_roc_010_by_richard_feldman）预览了第一个编号版本及其“历程”。

旧的 Rust 编译器对部分值带有追踪式 GC 组件（Utrecht 学位论文 “Reference Counting with Reuse in Roc” 把 RC 建成其追踪式 GC 的替代方案）。新编译器是**纯引用计数加静态推断的所有权**：LIR 契约里没有任何 GC。

语言层面的立场取自 FAQ（https://www.roc-lang.org/faq）：Roc 没有线性类型，没有唯一类型，没有依赖类型或精化类型。所有权推理完全是编译器的内部事务。

面向用户的语法（仓库中的 docs/langref/）刻意保持 Elm 风格：单一的 lambda 语法 `my_fn = |arg1, arg2| ...`、记录、标签并集、opaque 类型、静态分派、在类型里用 `!` 和 `=>` 标记的效应（`pure_fn : Str, Str -> Str` 与 `run_fx! : Str, Str => Str`）。语法里不存在任何形式的所有权语法。

同一受检边界背后共存两种降级策略（仓库根目录 `design.md`）：`.lss`（Cor 风格：Monotype IR -> Monotype Lifted -> 可选 SpecConstr -> Lambda Solved -> solved inline plan -> LIR -> ARC 插入）与 `.boxy`（布局多态：checked CIR -> LIR -> ARC 插入，每个模板一份函数体，运行时类型描述符与分派字典作为隐藏参数）。两者都终结于同一个 LIR+ARC 契约，这也是四个后端（解释器 `src/eval`、dev native、wasm、LLVM）字节级一致的原因：测试套件要求如此。

优化构建还会按所有权模式特化过程变体（issue 12672 的 “owned/borrowed variant specialization”）：能静态证明某个实参正在死亡的调用点会得到一个变体，跳过该参数上的运行期唯一性检查。issue 12420 展示了反面（“a string-pattern match in a loop makes calls pick the borrowed variant for dying arguments, so appends copy”），说明模式特化机制真实存在，并且在两个方向上都有可观察的性能行为。

平台模型（https://www.roc-lang.org/fast）把分配策略变成平台（宿主库）的决定：原话 “Roc platforms get to determine how memory gets allocated and deallocated in applications built on them”。CLI 平台可以完全不释放，web-server 平台可以按请求 arena 分配并一次性回收，长生命周期状态的应用可以用 meshing compaction。对于“谁来选分配器”，这是一个明确的答案，解释执行的 DreamScript 可以用 `platform`/`host` 边界照抄。

内部术语（Glossary.md，克隆于 `Z:/Temp/plr1/roc/Glossary.md`，小节 Borrow/Consume/Copy-on-Write/Seamless Slice）：

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

这些是编译器与内建函数之间的契约，用户语法用不到它们。

核心机制位于 `Z:/Temp/plr1/roc/src/lir/arc.zig`，它的模块头（第 1-13 行）是对整个系统最好的一段说明：

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

`arc_solve.zig`（同一目录）精确给出了推断算法。一个绑定在三个条件同时成立时求解为**借用**：(1) 它唯一的定义语句具备借用能力（字段/标签载荷读取、别名、一个 `RcEffect.result_borrows_args` 恰好命名一个引用计数实参的低层操作，与一个返回结果恰好借用了一个实参的调用）。(2) 每个出现点都接受借用，只有 owned 调用实参、被消费或被保留的低层实参、聚合与捕获操作数、`set_local` 源与 owned 返回要求所有权。(3) 出租方链恰好解析到一个被绑定一次的 leader 局部变量。

签名分两个阶段跨过程求解：阶段 A 在精确的反向依赖上把参数模式推到不动点，参数从借用出发，一旦任一出现点要求一个单元就翻为 owned，原话 “so every parameter bit is queued at most once”。阶段 B 在每一个返回值都是锚定于某个借用参数的借用时，把该返回标记为借用，然后重新求解绑定模式，使调用方可以借用这类结果。

随后运行 **born-unique 分析**（新生即唯一）直到不动点，原话：“a proc's return is unique when every `ret` returns a born-unique value surviving to the return with no other holder, and a direct-call result of a unique-returning callee is itself a unique birth in its caller.” 唯一返回让调用点跳过运行期 refcount==1 检查。这就是 Koka 的 fan-in 复用，被表述为一条数据流事实。

钉住的签名（从不求解）是 ABI 契约：根过程、托管过程、擦除后的可调用过程、无函数体的过程，以及地址会逃逸的过程。原话 “Everything else stays owned, which is always sound.”

`src/base/rc_effect_rules.zig` 是一张经 comptime 校验的结构化规则表，覆盖每个内建函数的 `RcEffect` 行（消费/保留/借用哪些实参、结果是否与已消费实参互为别名、是否执行运行期唯一性检查）。取自源码的样例规则：`retain_and_consume_same_arg` 的原话是 “one argument is both retained and consumed; the two cancel, so the row describes a borrow and must name neither”。`runtime_check_without_allocate` 的原话是 “a runtime uniqueness check exists to choose between mutating in place and copying, so the copy path allocates; may_allocate must be true”。`borrow_of_consumed_arg` 的原话是 “the lender's unit is gone by the time the result would be read through it”。第二层（`eval/rc_conformance.zig`）把声明的说法与每个 Zig 内建函数在运行期的实际行为交叉核对，调试版的借用校验器再校验一次 ARC 输出。对一个所有权 pass 来说，这是异常严格的正确性纪律。

`src/lir/` 中的支撑 pass：`trmc.zig` + `tail_drive.zig` + `tail_call_builder.zig`（tail-recursion modulo constructors，遵循 Koka 的 “Generalized Tail-Recursion Modulo Cons”）、`box_reuse.zig`、`return_slot.zig`（返回槽重写）、`loop_append_promote.zig`、`immortal_locals.zig`、`scalarize_joins.zig`、`tag_case_fusion.zig`、`StackPlan.zig`。`design.md`（仓库根目录，受检之后的权威设计，约 2.6 万行）给出流水线契约，原话 “Backends do not reason about reference counting... Reference-counting policy belongs to LIR ARC insertion.”

bug 追踪器说明这套分析是承重的，并且在公开场合被调试：issue 11830 “ARC: borrowed local crosses a join without a live owner; --opt=speed builds segfault at runtime”。issue 10920 “Tail-call arguments are forced owned, losing borrows from outside the SCC”。issue 12436 “Dev backend: a record field moved into a call is still read through the record afterwards”。issue 12672 “Investigate whether skipping static uniqueness proofs and owned/borrowed variant specialization would speed up --opt=dev builds”。

已放弃的 2024 年设计（背景）：在 Rust 编译器时期，Roc 设计过一套面向用户的借用检查器，包括记录/opaque 类型的 “restricted borrows”、`Value`/`Ref` 参数类型，以及 “transparent functions”（编译器可以跨其内联或重排的纯函数，因为它们无法观察分配）。那段讨论留在 roc Zulip（`#design`/`#ownership` 流，不在搜索索引内）和零散的 issue 里（https://github.com/roc-lang/roc/issues/6027 时期）。已发布的 Zig 编译器把这些全部删去：语言保持零所有权语法，一切都移入推断。

DreamScript 应从 Roc 取走的是：(1) 解释器可以廉价地采用同一条快路径，即每值引用计数，当且仅当 refcount==1 时原地修改，否则复制。(2) 语言从不提所有权。(3) 分配策略是平台/库的决定（https://www.roc-lang.org/fast 的原话 “Roc platforms get to determine how memory gets allocated and deallocated”，CLI 平台可以完全不释放，web 平台按请求使用 bump arena）。(4) `arc_solve.zig` 的文件头就是一份完整的跨过程借用推断规格，如果日后我们要加优化 IR，这是现成的最佳模板。

### B.2 Koka：Perceus、复用分析、FBIP、借用推断

仓库：`Z:/Temp/plr1/koka`（https://github.com/koka-lang/koka）。面向用户的叙述：`doc/spec/why.kk.md`（小节 “Perceus Optimized Reference Counting” 与 “Reuse Analysis”）以及在线书（https://koka-lang.github.io/koka/doc/book.html）。

Perceus（Reinking、Xie、de Moura、Leijen，“Perceus: Garbage Free Reference Counting with Reuse”，MSR-TR-2020-42，PLDI 2021 杰出论文，https://www.microsoft.com/en-us/research/publication/perceus-garbage-free-reference-counting-with-reuse/）是**精确** RC：每个值的引用计数恰好等于逻辑引用的数量，由静态插入的 dup/drop 指令维护。Perceus 直接编译到纯 C，循环收集器、GC 与运行时系统全部省略。

取自 `why.kk.md` 的原话 “Perceus uses extensive static analysis to aggressively optimize the reference counts. Here the strong semantic foundation of Koka helps a lot: inductive data types cannot form cycles, and potential sharing across threads can be reliably determined.” 大意是 Perceus 用大量静态分析激进优化引用计数，并依赖 Koka 的两条语义基础：归纳数据类型无法成环，跨线程的潜在共享可可靠判定。性能目标是 “within 2x of the performance of C/C++”，初始基准在 “close to C performance on various memory intensive benchmarks”。

复用分析（fan-out/fan-in）：Perceus 把一个模式匹配与一个大小相同的构造器配对，在引用计数为 1 时原地复用被匹配的分配。书中给出的经典例子：

```
fun map( xs : list<a>, f : a -> e b ) : e list<b>
  match xs
    Cons(x,xx) -> Cons( f(x), map(xx,f) )
    Nil        -> Nil
```

如果列表不被共享（`list(1,100000).map(sqr).sum`），新的 `Cons` 单元会就地覆盖旧的。生成的 C 快路径（来自书中的旁注，收录于 `why.kk.md`）：

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

Koka 还实现了 **TRMC**（tail-recursion modulo cons），把上述递归 `map` 在快路径上变成原地更新的循环。一般化理论是 Leijen 的 “Tail-Recursion Modulo Cons” 研究线（ICFP 2024）。

书中的关键主张，原话：“the reuse optimization is guaranteed and a programmer can see when the optimization applies. This leads to a new programming technique we call FBIP: functional but in-place. Just like tail-recursion allows us to express loops with regular function calls, reuse analysis allows us to express many imperative algorithms in a purely functional style.” 大意是复用优化的适用条件有保证，程序员能看出来。由此得到 FBIP 这门技法：正如尾递归让我们用普通函数调用表达循环，复用分析让我们用纯函数风格表达许多命令式算法。

fan-in/fan-out 的形式化：“fan-out” 复用发生在已知唯一的实参进入函数的位置（无需复制），“fan-in” 复用发生在刚产生、大小匹配的结果落入一个刚被消费实参腾出的栈帧槽位时。Frame Limited Reuse（Xie、Leijen 等，“Reference Counting with Frame Limited Reuse”，MSR-TR-2021，https://www.microsoft.com/en-us/research/wp-content/uploads/2021/11/flreuse-tr.pdf）把复用限制在当前栈帧，使分析保持局部、可预测、对分离编译友好。

借用推断：用户函数的自动借用于 **Koka v2.2.0（2021-08-26）** 引入。在此之前只有内建原语借用（FLR 论文本身如此陈述）。实现位于 `src/Backend/C/Parc.hs`，头部注释原话：“precise automatic reference counting (now called Perceus)... The monad has a borrowed and owned (multi-set) environment just like the paper. The live variable set is a state. To calculate the live variables we visit the expression tree _in reverse_.” 配套模块 `src/Core/Borrowed.hs`。借用的局部变量跳过 incref/decref。owned 的局部变量在最后一次使用时丢弃。readme 路线图中的条目 “Borrowing analysis for Perceus and improved reuse analysis (contact: Anton Lorenzen)” 在后续版本中兑现（“Fully In-Place Functional Programming”，Lorenzen & Leijen 等，ICFP 2023）。

效应的正交性（与我们的可选效应计划相关）：Koka 的复用/借用栈在效应处理器*之下*运行，两者互不干扰。上面那个 `map` 经由带效应的 `f` 编译后，生成的 C 里原地更新旁边就是 `if (yielding()) { ... }` 这类续延钩子。Xie 的 “evidence passing” 编译（koka readme 中引用为 `@Xie:evp`、`@Xie:evidently`）是让处理器分派保持廉价、同时让 Perceus 保持内存精确的机制。readme 把 “the theory and practice of evidence passing” 与 “the formalization of Perceus reference counting” 归功于 Ningning Xie。教训：效应与所有权推断可以分层。效应系统决定*控制流*（让出点），RC 系统决定*数据流*（dup/drop），两者只在让出钩子上有交叠。

与线性类型目标的交互（我们的长期问题）：Koka 展示的立场是，**内存管理的收益在语言层面无需线性性**。唯一性分析（refcount==1 检查 + 复用配对 + 借用推断）为函数式代码恢复了原地行为，同时语言保持“无标注也无 GC”的工效学。纯线性类型仍然能增加的只有：对用户定义资源的静态零检查保证、会话类型协议，以及对不受引用计数管辖的未装箱数据的修改。这正是我们的组合 4（可选线性层）所填补的缺口。

### B.3 Hylo 与 Swift：值语义、参数模式、不可复制类型

Hylo 仓库：`Z:/Temp/plr1/hylo`（https://github.com/hylo-lang/hylo，最后提交 2026 年 10 月，仍活跃）。Hylo（原名 Val）是“值语义加二等借用”最纯粹的设计：表层语言里没有指针、没有引用、完全没有解引用运算符。共享是编译器的实现细节。

参数传递约定：`let`（不可变借用，默认）、`inout`（可变借用）、`sink`（消费，被调用方取得所有权）、`set`（先写后读的 inout，供访问器 setter 使用）。证据：`docs/LocalBindings.md` 展示 IR 签名 `fun add(let Int, let Int) -> sink Int`。`docs/MiscNotes.md` 在同一个下标上展示全部四种访问器：

```
subscript identity<T>(x: T): T {
  let { yield x }
  sink where T:Movable { x }   // consuming read, requires movability
  inout { yield x }
  set { x = newValue }
}
```

局部绑定（`docs/LocalBindings.md`）：`var` 总有存储。一个 `let` 在“所有初始化表达式都是左值”时只做借用，不带任何存储。所有初始化表达式都是右值时它得到 `T` 存储，在混合情形得到 `Optional<T>` 存储。一个 `inout` 绑定“从不拥有存储，在 IR 层面始终表示为一个借用访问”（`start_borrow [inout] %0`）。确定初始化分析是一个过程内的抽象解释器，负责插入 `deinit` 指令。

混合情形的 IR 很有教益，因为它展示了借用与存储如何被机械地统一。对于 `let bar = if condition { foo } else { 42 }`（其中 `foo` 是 `var Int`），一个分支把 `bar` 绑定为 `foo` 存储上的 `start_borrow [let] %0`，另一个分支把字面量包进 `Option<Int>` 存储。在汇合点，`bar : &Int` 从可选值中被重新借用出来（`start_borrow [let] %15`），而由于 `bar` 被一个 `copy()` 调用使用，编译器显式物化出 `Hylo.Int.copy`。存储、借用与复制全都是 IR 层面的决定，由编译器代劳。

确定初始化（`docs/DefiniteInitialization.md`）：内存位置被跟踪为 Raw / Uninitialized / Initialized / Partial 四种状态。`load` 要求 Initialized 并留下 Uninitialized。抽象解释器建模调用栈并在作用域结束时插入 `deinit`。也就是说，Hylo 的所有权叙事是“单一所有者、静态跟踪、借用是投影、编译器包揽全部记账”。

该 IR 把常驻寄存器的对象视为**线性资源**，原话：“Objects that live in registers are initialized when first assigned to a register and eventually get consumed before the function returns. Because they are treated as linear resources, any operation in which an object appears as an operand consumes it.” 线性性活在源码之下的一层，由编译器管理。

不可复制类型（StandardLibrary/Sources/Core/Copyable.hylo 与 Movable.hylo）：

```
public trait Copyable: Movable {
  fun copy() -> Self
}
public trait Movable {
  fun take_value(from source: sink Self) { ... }
}
```

用户写 `struct FileDescriptor: ~Copyable` 就得到唯一所有权，并在该唯一实例的生命周期结束时运行 `deinit`。下标 sink 用 `where T:Movable` 守护。可复制性是默认的正向约束，用取反减去。这正是 DreamScript 想要的集合论惯用法。

“可变 let”：Hylo 和 Swift 都禁止修改不可变绑定。Hylo 的答案是，绑定到左值的 `let` *本身就是*借用，修改经由 `inout` 绑定（`inout bar = &foo`）进行。Swift 的答案是普通的 `var` 加上所有权修饰符。这项非特性 DreamScript 也有意保留：重新绑定的不可变性带来别名悖论，换来的语法糖很少。

Swift（提案以本地副本 `Z:/Temp/plr1/se0377.md`、`se0390.md`、`se0430.md` 阅读，来自 https://github.com/swiftlang/swift-evolution）：

SE-0377（Swift 5.9）加入 `borrowing`/`consuming` 参数修饰符，原话 “Other functions default to borrowing their parameters”。方法可以声明 `consuming self`。被接受的修订要求用一个显式的 `copy` 运算符把值移入消费位置，使每一次复制都可见。

SE-0390（Swift 5.9），不可复制的结构体与枚举，原话：“Values of noncopyable type always have unique ownership, and can never be copied... Since values of noncopyable structs and enums have unique identities, they can also have `deinit` declarations, like classes, which run automatically at the end of the unique instance's lifetime.” 提案中的例子：`struct FileDescriptor: ~Copyable { private var fd: Int32; ... deinit { close(fd) } }`，原话 “no object needs to be allocated; only a simple struct... that uniquely owns the instance”。

SE-0414（基于区域的隔离）加 SE-0430 的 `sending`（Swift 6.0）：`sending` 参数/结果 “is required to be disconnected at the function boundary and thus possesses the capability of being safely sent across an isolation domain”。它在并发域之间转移独占所有权，由编译期证明，为了数据竞争安全一般化移动语义，类型无需全局 `Sendable`。

Swift 的排他性还会被**动态**强制：`-enforce-exclusivity=checked`（调试版默认）在运行期对冲突访问触发陷入。即便成熟系统也用运行期检查兜底静态所有权规则。

Swift 在普通代码里暴露多少所有权 UX？基本为零：ARC 管理生命周期，COW 是库约定（Array/String 的唯一性检查是运行期引用计数检查，不可见），那些修饰符是给标准库级代码的可选支持。代价是：性能关键处缺少一种声明“这个函数不会共享接收者”的手段，而 ARC 开销真实存在。scattered-thoughts 的文章引用基准，某些 Swift 程序从原子 RC 换成非原子 RC 后性能翻倍。

### B.4 Rust：精确规则、NLL、Polonius Alpha、学习成本、绕行手段

今天实现的规则：每个值恰好一个所有者。赋值与按值传递会**移动**非 `Copy` 类型的值，使来源失效。访问经由 `&T`（共享）或 `&mut T`（唯一可变）借用。借用的寿命不超过所指对象。活跃的 `&mut` 排斥对所指对象的所有其他使用。`Drop` 类型不能被部分移动。丢弃顺序是声明顺序的逆序，由 RFC 1857 稳定（https://rust-lang.github.io/rfcs/1857-stabilize-drop-order.html）。

借用检查运行在 MIR 上。**NLL**（2018 年发布）让借用结束于最后一次使用而非作用域结束，但该分析*对活性区域不敏感*：借用的区域是它可能被使用的所有点的并集，因此一个返回借用的分支会污染整个函数。

经典的误报，来自 Polonius 公告（https://blog.rust-lang.org/2026/08/04/enabling-polonius-alpha-on-nightly）：`HashMap::get_mut_or_default` 就是一例：`Some(value) => value` 分支的借用被当作在 `None` 分支里仍然活跃，而那里必须修改 map，于是程序被拒绝，尽管没有任何路径在 match 之后使用该借用：

```
fn get_mut_or_default(map: &mut Map, key: K) -> &mut V {
    match map.get_mut(key) {
        Some(v) => v,          // borrow tied to fn lifetime here ...
        None => { map.entry(key).or_insert(default) }  // ... so this mutate is rejected
    }
}
```

Polonius Alpha 接受它，因为该分析是位置敏感的：借用可证明在 `None` 路径上已经死亡。另一种经典形态即使在 Polonius Alpha 下仍被拒绝（在 `while let` 里用 `p = &mut p.next` 遍历链表），这显示了当前重构的上限。

**Polonius Alpha** 自 2026-08-04 起在 nightly 上启用，原话 “in preparation for stabilization in the next few releases”（目标：2026 年底之前）。它加入**对生命周期 outlives 关系的流敏感借用检查**。它知道 `get_mut_or_default` 的借用不在 `None` 路径上活跃，于是接受该程序。实现：2018 年的 Datalog 表述健全但太慢。2023 年的重构需要 “minimal rearchitecture of the existing NLL implementation”，产出了 “polonius-next”/Polonius Alpha。在该集合之外，编译期回归在前 1 万个 crate 上大多低于 1%，最坏情况是某些程序的 2-3 倍。限制：旧版 Polonius 是它的严格超集，一些链表风格的 `p = &mut now.next` 循环仍被拒绝，可以用 `-Zpolonius=off` 关闭。

学习成本为何高（对一手评论的归纳）：(1) 类型系统编码的是*位置与时间*，用户必须学会变量命名的是一个位置、移动会毒化它、借用是活性的投影。(2) 错误关于非结构（“does not live long enough”“borrow later used here”），需要整 CFG 推理。(3) 函数签名必须预先承诺一种别名纪律，因此设计失误以重写的形式暴露（`&mut` -> `&` -> owned -> `Rc<RefCell<T>>`）。(4) 自引用与循环结构只能在加入间接层后表示。without.boats 的文章 “Ownership”（https://without.boats/blog/ownership）把 Rust 的所有权拆成四个半耦合的特性（仿射移动、借用、生命周期、丢弃），并把难度定位在耦合上。这份清单同样列出了 DreamScript 不该耦合什么。Stack Overflow 调查一致地把 Rust 同时评为最受喜爱和最难过学，这就是这项取舍的通俗一句话总结。

人们实际会去抓的绕行手段，每一个的存在都源于检查器挡住了合法程序：`clone()`（抹掉共享）、`Cow<'a, T>`（在 API 边界写时复制）、`Rc<T>`/`Arc<T>`（共享所有权，`Arc` 要付原子操作的钱，按 scattered-thoughts 文章引用的基准，原子与非原子 RC 可以差 2 倍）、`RefCell<T>`/`Mutex<T>`（把排他性检查推到运行期，这正是 Inko 与 Roc 的运行期唯一性检查在全局所做的取舍）、`Box::leak`/arena（永远不释放，从而逃出生命周期系统），以及用索引/id 的图代替指针。每个绕行手段都收敛到“动态检查或动态计数”，这也正是 Inko/Lobster/Roc 的设计点：那些语言把动态机制当作唯一机制，于是没有静态的东西要学。

### B.5 Austral：严格实现的线性类型（free/借用、线性方法）

仓库：`Z:/Temp/plr1/austral`（https://github.com/austral/austral，最后提交 2025 年 7 月，开发已放缓）。Austral 证明了一个完全线性的内核加上 C 级代码生成可以是*简单的*：没有 GC，没有 RC。线性值就是编译器负责移动的值。README 的主张：内存安全且 “no runtime overhead, avoiding double `free()`, use-after-`free` errors, and double fetch”。

设计（docs/walkthrough.md 的 “Linearity Checking”）：线性值必须**恰好被消费一次**。借用以语句形式出现，语言里没有借用类型。检查器是一个抽象解释器，维护一张状态表，把每个线性变量映射到 `(definition loop depth, state in {Unconsumed, BorrowedRead, BorrowedWrite, Consumed})`。

精确规则，从 walkthrough 归纳：消费要求变量处于 `Unconsumed`，且与定义处在同一循环深度（在循环外定义的线性变量不能在循环内被消费，因为它可能被消费多次）。在一个表达式内至多一次消费（`C > 1` 是错误），并且一个消费排斥同一表达式内的所有读取、借用与路径（`R`、`W`、`P` 必须为零）。每个表达式至多一个 `&!` 写借用，并排斥读取。`if`/`case` 各分支必须在已消费集合上一致。在 `return` 上，所有活跃的线性变量都必须被消费。离开一个绑定的作用域时，该作用域引入的线性变量必须是 `Consumed`。

借用语句是块作用域的：`borrow x: T as y in <body>` 产生读借用，`borrow x: T as ref y in <body>` 产生写借用。借用就是区域，因此没有一等引用的表达力问题，也没人为它惋惜。

循环深度规则的原始形式（docs/walkthrough.md）：在循环深度 d 定义的线性变量不能在任何大于 d 的深度被消费，因为控制流可能把这次消费经过两次：

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

正是这个“深度跟踪”技巧让 Austral 的检查器保持单趟前向、无需不动点计算，实现者能手工验证它。

`free` 是语句：`free x` 消费一个线性值，并把底层内存作为线性指针资源返回，于是手工内存管理经过类型检查。线性方法（线性类型上的 `method`）线性地取得接收者，提供类似 RAII 的协议（close/commit/cancel）。标准库提供基于裸内存的线性 `Pointer` 与 `Span`。

为何如此严格：Austral 面向零运行时的高保障 C 替代，并且想靠构造保证可判定性。代价是消费一次的纪律会塑造*所有*代码，资源代码之外的代码同样受它约束。对线性值的随手闭包写不出来，图需要显式的借用编排。对于可选的资源层，这是正确的模型。对一个通用解释型语言，这是错误的默认。

### B.6 量化与分级方案：Idris 2、Granule、Linear Haskell、ATS、Clean

Idris 2（QTT）：绑定携带量 0（运行期擦除）、1（运行期恰好使用一次）、omega（不受限）。检查器强制使用，并**保证 0 量被擦除**，实际收益是证明实参可证为零成本。一手资料：Edwin Brady，“Idris 2: Quantitative Type Theory in Practice”，ECOOP 2021（https://drops.dagstuhl.de/entities/document/10.4230/LIPIcs.ECOOP.2021.9，https://arxiv.org/abs/2104.00480）。教程 https://idris2.readthedocs.io/en/latest/tutorial/multiplicities.html。基础：Atkey “Quantitative Type Theory”（2018）。McBride “I got plenty o' nuttin'”（2016）。QTT 做的是使用记账，内存由 GC 负责，代价是每个绑定上一个数字。多重性教程中的对照例子：`dup : (1 x : a) -> (a, a)` 被拒绝（不能使用一个 `1` 量两次），而 `drop : (1 x : a) -> ()` 没问题。`0` 量给那些*保证被擦除*的证明实参定类型（`+` 的序关系证明永远不会活到运行期）。0/1/omega 的三分恰好也是擦除决策可检查而非启发式的粒度，即便在一个从不暴露 `1` 的语言里，这个性质也值得拥有。

Granule（`Z:/Temp/plr1/granule`，2026 年 7 月活跃，https://granule-project.github.io/）：分级模态类型把量一般化为任意分级（半环）并带上效应。取自 README：

```
dup : forall {a : Type} . a [2] -> (a, a)
dup [x] = (x, x)

map : forall {a, b, n} . (a -> b) [n] -> Vec n a -> Vec n b
-- the function is used exactly n times, once per element
```

对资源/协议正确性（会话类型、文件句柄）很美，对日常代码太重。发行状态是研究级的（macOS 二进制，依赖 Z3）。

Linear Haskell（GHC `-XLinearTypes`，自 9.0，用户手册 https://ghc.gitlab.haskell.org/ghc/doc/users_guide/exts/linear_types.html，提案 https://ghc-proposals.readthedocs.io/en/latest/proposals/0111-linear-types.html）：带多重性标注的箭头 `a %1 -> b` 与多重性多态箭头 `a %m -> b`。普通 omega 代码不受影响，因此采纳是可选启用的。编写多重性多态库会把 `UR`（unrestricted-ramified）约束泄漏到签名里，`linear-base` 的存在就是为了吸收这一点。给 DreamScript 的教训：多重性*多态*是工效学杀手，解决办法是在每个定义点固定多重性（Austral 风格），或者推断它（Koka 风格）。

ATS（视图）：一个 `viewtype` 把一个类型与一个线性*视图*配对，这个视图就是一条内存布局的证明。消费该视图即消费资源。证明函数以零运行期成本静态验证内存安全（教程：“Introduction to Programming in ATS” 第 13 章，http://ats-lang.github.io/FROZEN000/DOCUMENT/INT2PROGINATS/HTML/，概览 https://learnxinyminutes.com/ats）。它在本次调研中记号负担最高，展示的思路是把线性类型当作关于内存的证明，验证在编译期完成。

Clean（唯一性类型）：`*` 唯一性属性让纯函数可以破坏性更新未被共享的数组。参考：Clean 语言报告（https://clean.cs.ru.nl/Clean）、“Uniqueness Typing Simplified”（de Vries 等，https://www.cs.tufts.edu/comp/150FP/archive/edsko-de-vries/uniqueness.pdf），以及 Futhark 的现代处理（https://futhark-lang.org/blog/2022-06-13-uniqueness-types.html）。唯一性是“恰好一次使用”的“至多一个引用”弱化版。这正是它对推断友好的原因：Koka 的 refcount==1 检查与 Futhark 的唯一性分析是同一想法的运行期/分析版本，并在 “Linearity and Uniqueness: An Entente Cordiale”（Marshall、Ostermann、Brady 等，ESOP 2022，https://recurial.com/LinearityAndUniqueness.pdf）中被系统化。

### B.7 Vale：带代引用、区域池化、更高阶 RAII

仓库：`Z:/Temp/plr1/vale`（https://github.com/ValeLang/Vale，最后提交 2026 年 9 月）。权威文章：https://verdagon.dev/blog/generational-references，下文所有细节来自该文。

机制：一个带代引用是 16 字节，即裸指针加 `rememberedGeneration`。附录中的 “generation tables” 变体是 8 字节（对象指针 + 32 位表索引 + 32 位代）。每个分配携带一个 `currentGeneration` 头，`gmalloc`/`gfree` 递增它。编译器无法证明安全的每次解引用都会发射一次 `__check` 比较代，实现为对受保护页的访问（手写 segfault，比 assert 快）。

默认方案（“random generational references”）：不做计数，而是在分配/栈进入时在每个对象之上写入一个新鲜的伪随机数，并在销毁时清零。失效引用漏检的概率是 2^-64。文章计算过，即便每秒 600 万次失效，平均也要约 73250 年才会误判安全。因为没有中心计数器，普通 malloc 就能用，对象可以活在栈上、内联在父对象里、数组里或自定义分配器里，内存也能正常归还操作系统。

检查的骨架形式（取自文章的 C 风格阐述）：

```
ref  = { alloc: *Word, rememberedGeneration: u64 }   // 16 bytes
word above each allocation: currentGeneration: u64

__check(r):  if (*r.alloc.generation != r.rememberedGeneration) fault();
             // fault = store/load through a protected page => SIGSEGV
gmalloc(sz): slot = find_free(sz); slot.generation += 1; return ref(slot)
gfree(slot): slot.generation += 1; return slot to the free list
destroy(r):  r.alloc.generation = 0   // every existing ref now mismatches
```

调研范围内各系统的引用布局对比（每个堆引用，64 位）：追踪式 GC 8 字节（无检查）。普通 RC 8 字节加头部计数。Inko 8 字节加头部 refcount 与借用计数。Vale 16 字节（代在引用里）或 8 字节（代表）。Rust 共享引用 8 字节，`&mut` 8 字节且有完整静态证明。zest 风格动态借用 16 字节（出租方 + 所有者 + 栈上的租约）。对 DreamScript 而言，8 字节还是 16 字节的选择，实质上就是在“计数”与“代”之间选一个动态安全基底。

基准（BenchmarkRL 地形生成器，仅 Vale 各变体）：unsafe 43.82 秒（基线）、朴素 RC 54.90 秒（+25.29%）、带代引用 48.57 秒（+10.84%），原话 “GR's overhead was less than half that of reference counting”。没有直接的 C++/Rust/GC 数字，作者称击败 RC “a pretty low bar”。

混合带代内存（HGM）：三种机制可以消除检查。(1) 单一所有权（经由 owned 值的访问跳过检查）。(2) **线性风格**：临时把所有权移入/移出函数，完全避免检查，原话 “whole programs can theoretically reach zero checks”。(3) 区域 + 静态分析：`pure` 标注让数据不可变，于是后续许多检查被一次 `__precheck` 取代（文章例子里的 “200 checks into 1”），而不可变区域的借用直接跳过检查。预期的混搭是：默认可变别名的灵活代码，然后在热路径上选择性使用 `pure`/区域/线性风格。作者提醒，把检查压到零就像 Rust 那样，只是把成本转移到克隆、边界检查和哈希。

区域池化：仓库文档 `docs/LinearRegion.md`、`docs/IRegion.md`、`docs/RegionBookkeeping.md` 描述若干区域种类，其中指针变成偏移（用于录制与重放的 arena/序列化缓冲）。`architectural-direction.md` 记录了把此前的组合（unsafe/resilient/generational/linear 区域）合并成一种 “Hybrid-Generational Memory” 区域，并在验证它能编译成与 C 等价的汇编之后，从不安全区域种类重建。

更高阶 RAII：类型系统中的所有权保证未来某次调用一定会发生。析构器可以接收实参并返回值。这带来无缝的结构化并发（没有 async/Send 函数着色）和完美的重放调试。

与 DreamScript 的相关性：带代引用是调研范围内唯一**完全没有移动错误**的方案：别名免费，失效在访问时检出。成本：每个堆引用增加 8 字节，无法证明的解引用要付一次检查。默不可变的语言（我们的）会跳过大部分检查，因为不可变值永远不会在别名背后失效。Vale 机制 (3) 与我们默认可变性策略的交集，对解释器格外有利（值不可变时维护代字非常容易）。

### B.8 Inko：无借用检查器的单一所有权，运行期检查的借用

仓库：`Z:/Temp/plr1/inko`（https://github.com/inko-lang/inko，手册源码在 docs/source/getting-started/memory-management.md，运行时设计在 docs/source/design/runtime.md）。

手册给出的完整模型：值是 owned 引用。赋值**移动**，编译器错误是 `error(moved): 'cats' can't be used as it has been moved`。借用是显式的 `ref`（不可变）与 `mut`（可变），且**跳过生命周期检查**：

```
Inko allows you to move owned references while borrows to the owned
reference exist. If an owned value is dropped while borrows to it still
exist, a runtime error known as a "panic" is produced, terminating the
program:
  panicked: can't drop a value of type 'Array' as it still has 1 reference(s)
```

安全性理由，原话：“each heap allocated value stores a borrow counter. This counter is incremented when borrowing the value, and decremented when the borrow is discarded. When dropping an owned value, the borrow counter is checked and a panic is produced if the count is not zero. Using this approach we can avoid having to implement a complex compile-time borrow checking mechanism, and still implement patterns otherwise difficult or impossible to implement when using borrow checking (i.e. linked lists or graphs).” 大意是每个堆分配值带一个借用计数，借用时递增、归还时递减。丢弃 owned 值时检查该计数，非零即 panic。这样不必实现复杂的编译期借用检查，同时仍支持借用检查下难以实现的模式，例如链表与图。

该机制基于 2006 年的论文 “Ownership You Can Count On: A Hybrid Approach to Safe Explicit Memory Management”（手册链接：https://inko-lang.org/papers/ownership.pdf）。确定性论证，原话：“the check is performed when dropping a value and not when dereferencing a borrow, so if the program triggers a borrow error with input A, then running the same program with the same input 10 times will produce the same error 10 times”，外加一个指出丢弃点的栈回溯。经验主张：“encountering borrow errors is rare to begin with as it just isn't that common for borrows to outlive the owned values they borrow.”

优化的姿态，原话：“most of the borrow count mutations can be optimized away. While we don't implement such optimizations at the time of writing, we intend to do so in the future”。也就是说，Inko 先交付运行期模型，之后再做编译期消除，与 Roc/Koka 的分期相同。

唯一引用（`recover`）用于进程间消息传递：唯一值保证没有未完成的借用，因此在进程间发送它不会竞争，Rust 交给 `Send`/`Sync` 的并发角色，在这里被归结为一次移动。

调用边界上的自动借用转换（手册中的表格，逐字复现，这就是 Inko 程序员看到的全部“签名噪音”）：

| 输入 | 期望 | 传入 |
|---|---|---|
| `T` | `ref T` | `ref T` |
| `T` | `mut T` | `mut T` |
| `ref T` | `ref T` | `ref T` |
| `mut T` | `mut T` | `mut T` |
| `mut T` | `ref T` | `ref T` |

表中值得注意的缺失：期望 `mut T` 时不接受 `ref T`（没有到可变的静默升级），期望 `T` 时不接受 `T`（在语言模型中移动是显式的，即便编译器会消除它们）。

值类型（Int、Float、String、进程、FFI C 结构体）在移动时复制，所有权留在原处。小值完全绕开别名机制。

GC 历史：早期 Inko 用过自制的基于 Immix 的分配器/GC（0.16 之前的进程时代还带搬移、去碎片的堆）。docs/source/design/runtime.md 记录了放弃它的原因，原话：“The implementation was quite complex and difficult to debug; Immix suffers from fragmentation, and without a GC (what it's designed for) it's hard to clean up the fragmentation; our implementation was unlikely to outperform highly optimised allocators such as jemalloc, so we figured we may as well use an existing allocator.” 他们现在直接用系统分配器，这是手写 GC 真实成本的一份坦率数据，对一个小组尤其如此。

### B.9 Lobster：在引用计数之上做流敏感的所有权分析

仓库：`Z:/Temp/plr1/lobster`（https://github.com/aardappel/lobster，2026 年 11 月活跃）。一手资料：docs/source/memory_management.md（Wouter van Oortmerssen）。

Lobster 把运行期 RC 与一项所有权分析结合，得到“编译期引用计数”。原话：“the algorithm picks a single owner for each new heap allocation, which is usually the first variable, field, or vector element it gets assigned to, and then tries as best as possible to make all uses from there on 'borrows'. ... If somewhere else in the code something wants to own that same value, it will insert a reference count increase only for this particular use (in Rust, this would cause an error instead). Using this analysis was able to remove around 95% of runtime reference count operations.” 大意是算法为每个新堆分配挑选单一所有者，通常是它被赋给的第一个变量、字段或向量元素，然后尽可能把此后所有使用变成借用。别处若需要拥有同一个值，只为那一处插入一次引用计数递增，而 Rust 在同样情形会报错。该分析消除了约 95% 的运行期引用计数操作。

关键的架构性质：该分析永远不会不健全，只会次优：运行期 RC 是基准事实，静态分析只消除操作。没有 owned/borrowed 签名，没有运行期唯一性检查，COW 由程序员调用不同的内建函数来表达。

值得借鉴的独有机制：(1) 分析作用于 **AST 节点**，粒度是节点。每个节点有一个它期望子节点提供、并向父节点传递的所有权种类。不匹配时，插入一次 incref（父要 owned，子出借）或一个作用域结束处的匿名 drop（父借用，子拥有）。大多数匹配都是免费的（原话：“in `let a = [1,2,3]` ... everybody is happy. No Reference counts touched”）。

(2) 追溯式所有权：一个从未被赋值的变量，由某个变量、字段或向量元素初始化时借用该来源，并持续到它的作用域结束。原文：“A variable that is never assigned to ... and is initialized from a variable, field or vector element borrows it, for the rest of its scope... If something then writes to that L-value while the variable is alive, the analysis does not error ... but retroactively makes the variable own (its initializer gets the reference count increase, and it gets the decrease at the end of its scope), so the program means the same either way, and only the cost differs. This is what makes the `old := cur` pattern work.”

(3) 跨调用借用让该值对调用方的作用域保持存活，只“为这次调用”付一次 incref。只有同一表达式内的借用/修改冲突会报错，例如 `f(v[0], v.pop())` 报出 “cannot modify x while y borrows it”，这类冲突据报告很少见。

(4) 工效学证据：在数十个中型游戏原型上，启用编译期 RC 只需要 “minor changes in only 2 locations”。

(5) 可选启用的严格性：设计过类似 Rust 的逐变量模式（原话：“guarantee there will only be one owner ever, and error out if the analysis is not able to guarantee this. This will also be able to remove the reference count field from objects entirely”），尚未实现。

(6) 环：RC 无法收集它们。被接受的缓解是在程序退出时给出“环报告”，告诉程序员环在何处泄漏，对游戏形态的代码足够务实。

(7) 内联 `struct` 类型分配在父对象内（栈、字段或向量槽位），不可变，赋值时复制。作者指出它们通过消除别名机会“reduce pressure”于所有权算法，这又一次印证了小不可变值的教训，而且比尽力而为的逃逸分析更适合可预测的性能。

(8) 所有权分析与流敏感的类型特化交织在一起：函数按调用图顺序检查，并同时按类型*与*所有权特化，因此它嵌在类型特化里，抽不出独立的 pass。

左值借用机制，为完整性而记：借用的值指向一个当前活跃左值（变量、字段、向量元素）的栈。左值本身是引用计数的，因为可能有多个借用同时活跃。修改一个借用计数大于 0 的被借用左值是唯一的硬错误，此外“结束时引用计数不为 0 就是所有权分析的 bug”，这是作者自己的不变式。还有两类错误（“variable x still has 1 borrowers”、“x used in <feature> without being borrowed”），作者报告在实践中从未见过触发。完整错误清单因此是三个罕见的编译错误加一条追溯成本规则，这是本次调研中最小的所有权错误面。

### B.10 Dada：权限加运行期取消

仓库：`Z:/Temp/plr1/dada`（https://github.com/dada-lang/dada，最后提交 2026 年 2 月，仍在合并 Niko Matsakis 的贡献）。Dada 是重新思考所有权 UX 最明确的实验。

设计目标（book/docs/design_docs/goals.md），原话：“Easy to learn, but not dumbed down... it brings [ownership and borrowing] to the fore, but in a packaging that is (hopefully) easy to learn”。“Predictable and reasonably efficient... It's ok to call malloc, but not to run a garbage collector”。“Values, not places or pointers”，于是没有指针类型，没有解引用运算符。“Sharing xor mutability”，保留 Rust 的规则。“Type erasure”：给定一个带标注的 Typed Dada 程序，删掉所有标注就得到 **Dynamic Dada**，它以相同方式运行，只是权限在运行期检查。静态检查是纯验证层，从不改变语义。

权限格（book/docs/dyn_tutorial/）：`my`（唯一/拥有）、`our`（共享，对象在被共享期间只读，可复制）、`leased`（可*移动*的唯一借用，可复制可移动，出租方写入时被**取消**）、`shleased`（共享租约，只读，写入时同样可取消）。取自 shlease.md 的原话：“When you have a shlease to an object, you know that the object will not be mutated so long as your shlease remains valid. Once a lessor writes to the object, your shlease is canceled.”

没有生命周期标注，也没有显式的租约结束（设计文档 “why_not_explicit_end_for_lease.md” 记录了该决定）：租约在出租方写入时结束，由运行期检出。“Atomic” 存储允许在被共享时修改，但只在事务块内（atomic.md：“access to shared, atomic field outside of atomic block”）。

教程风格的例子（dyn_tutorial/field_permissions.md、our.md）：`let pair: our = Pair(Widget(), Widget())` 得到一个 `our` 对，字段与 owned 在读者眼里没有区别。复制它会复制权限，对象本身留在原处。从可变值租借：先 `let l1: leased = p` 再 `let l2: leased = l1` 形成转租链，教程的 IDE 会在租约未结时用“虚线”画出出租方变量。权限是带*运行期*有效性的值，编译器按运行期区域处理它。

Dada 的设计立场几乎一对一地映射到解释器上：“Dynamic Dada” 运行时（带取消的权限监控）正是字节码解释器可以廉价承载的那类机制，而“类型擦除”原则（标注在 GC 之下是语义空操作）恰好是让后续静态检查器能在不改变语义的前提下加进来的性质。它是“所有权可以被抽象”系列中最有野心的一项。其中还没有任何部分作为静态系统落地。

状态：仍处设计阶段的语言。演算正在 PLT Redex 中形式化（design_docs/calculus.md：操作语义 “getting close”，静态类型系统暂缓）。如果 DreamScript 想要一等、可移动、且具备 GC 级工效学、无需生命周期标注的借用，Dada 是最清晰的蓝图。代价是一层运行期权限监控（对解释器没问题）与一个尚未落地的静态方案。

### B.11 较短的状态与相邻系统

Verona（https://github.com/microsoft/verona）：“research programming language for concurrent ownership”，用区域加检查器断言（基于 cown）代替逐对象标注。2026-11-03 通过 API 核对仓库：未正式归档（archived=false，最后推送 2026-02），但 README 说早期研究 “undergoing a massive refactoring”，上一版留在 `old_version` 分支，实际上处于蛰伏。值得保留的想法：区域推断加检查器，以区域为单位推理所有权。

Mezzo（fpottier）：把逐别名的权限（`unique`、`shared`、`readonly`）作为精化。GitHub 仓库已消失（2026-11-03 API 404），自约 2016 年起死亡。历史价值：第一个严肃的权限精化设计。失败模式：签名中的权限多态。

MoonBit（https://www.moonbitlang.com/blog，beta 2025-06，https://www.moonbitlang.com/blog/beta-release，路线图 2025-07）：在区块链时期（2023 年）MoonBit 发布过 `asset` 类型，那是面向链上资源的仿射绑定，既不能复制也不能被静默丢弃（Move 风格）。当前的 2025-2026 语言文档（https://docs.moonbitlang.com/en/latest/language/）不再记录任何 asset/affine 特性。该语言基于 GC（Wasm GC、js、native 后端）。把 MoonBit 的资产叙事视为“线性性作为可选开启的领域特性，后来被撤出范围”。

Move（背景）：资源类型带移动语义与显式的 `copy`/`move`，也是迄今为止最成熟的工业级线性资产语言（综述：https://github.com/s-tikhomirov/smart-contract-languages）。只有当 DreamScript 长出资产叙事时才相关。

Swift ARC 运行时（背景）：原子 RC，**没有循环收集器**。环靠约定（`weak` 引用）规避，这是朴素 ARC 在生产中的失效模式。Perceus 的“归纳类型无法成环”论证与 Lobster 的退出环报告各自以不同方式应对。

Jamie Brandon 的 zest 演示（“Borrow-checking without type-checking”，https://www.scattered-thoughts.net/writing/borrow-checking-without-type-checking）：一个动态类型语言，单一所有权，栈上的引用计数，动态借用检查。owned 引用用 `^` 移动，借用用 `!`、丢弃时还给出借方，共享用 `&`。每次安全检查都是对栈上计数的一次整数比较（已移动 = INT_MIN，借用计数为负，共享计数为正）。错误信息通过扫描栈定位出问题的值。它对设计空间的总结与我们的判断一致：Rust 解决值语义加内部指针 “at the cost of a surprising amount of type system complexity”。Hylo/Mojo/Swift 限制引用以换取更简单的模型。C#/OxCaml 的引用与 CHERI/fil-c 只解决内存安全那一半。“R and (a subset of) Swift solve problem 1 by using reference counting and copy-on-write... none of this is compatible with my goal of predictable performance.” 文中还引用 OxCaml（OCaml 的模式，2025）作为“模式而非生命周期”在工业界的再次到来。

### B.12 解释器的内存管理：调研

引用计数族，按编译器复杂度递增：

(1) 原子 ARC（Swift）：可预测、无停顿，但原子操作主导成本（引用基准中 2 倍代价）。它没有循环收集器。

(2) 带复用的精确 RC（Perceus/Koka，Roc 的 ARC）：非原子计数，静态精确的 dup/drop，refcount==1 检查支持原地修改，复用分析移除分配，实现“无垃圾”（释放即时、对缓存友好，终结器经由延迟清理在最后一次使用时运行）。Koka 的语义基础（归纳类型无法成环、共享可判定）在实践中移除了对循环收集器的需求。

(3) RC + 运行期借用计数（Inko）：每对象一个计数器，在丢弃时检查，给出确定性 panic。实现极其简单，消除计划留待以后。

(4) 所有权分析式 RC（Lobster）：在 (2)/(3) 之上做静态消除，无需用户模型，结果只可能次优，不会不健全。

(5) 带代引用（Vale）：完全不计数，用 8-16 字节的 fat reference，访问时检出失效，移动与克隆没有摩擦。

追踪式 GC：分代（nursery + 记忆集）与 Immix（基于区域、大部分不搬移、机会性去碎片，Blackburn & McKinley，PLDI 2008，https://www.cs.utexas.edu/users/speedway/DaCapo/papers/immix-pldi-2008.pdf）是实用设计。Inko 的复盘（复杂度、碎片却无 GC 来清理、不太可能击败 jemalloc）对小组来说是警示故事。搬移式 GC 与精确 RC、内部指针和 FFI 冲突。能保住大部分收益又绕开机制的 GC 邻域技巧是按请求 arena（Roc 平台模型：bump 分配，请求结束时一次性释放）。

逃逸分析与栈分配：值语义加不可变性让 EA 作为用户可见特性基本不必要。Hylo 把局部变量分配在栈上，并让 `let` 绑定的左值*完全没有存储*（docs/LocalBindings.md）。Lobster 的内联 `struct` 让栈/内联分配显式且可预测（按其作者说法，“better than best-effort EA”）。Roc 在 LIR 层面规划局部变量（StackPlan.zig、immortal_locals.zig）。Koka 的 FLR 为构造器churn 复用栈帧槽位。对解释器来说，务实形态是：在每帧 arena 里分配值单元，逃逸时晋升。这是一个运行期决定，符合“解释执行，可降级到内部 IR”。当值是单字句柄时，零成本移动是自动的：一个 owned 引用靠寄存器复制移动，没有 dup/drop，Perceus 把它形式化为“什么都不发射”。

Fusion 线（线性类型 x RC x GC），按时间顺序：Chirimar、Gunter、Riecke，“Reference Counting as a Computational Interpretation of Linear Logic”（1995），这是线性性与 RC 之间最初的同一性。“Garbage Collection Based on a Linear Type System”（CMU-CS-00-161）。Wadler 的线性资源线。Perceus（PLDI 2021）与 dup/drop 融合。Frame Limited Reuse（2021）。“Fully In-Place Functional Programming”（Lorenzen、Leijen 等，ICFP 2023），用唯一性检查而非线性类型实现原地函数式编程。“Generalized Tail-Recursion Modulo Cons”（Leijen，2024）。“Linearity and Uniqueness: An Entente Cordiale”（Marshall 等，ESOP 2022，https://recurial.com/LinearityAndUniqueness.pdf），它是形式化桥梁，说明唯一类型是“带可回退消费的线性性”，这正是唯一性可推断、而线性性只能检查的原因。Andrew Johnson，“Hybrid Linear / Reference-Counted Garbage Collection”（https://andrewjohnson4.substack.com/p/hybrid-linear-reference-counted-garbage，L2 语言），其中线性类型保证 RC 消除，并为共享数据保留 GC 兜底。已定的结论：线性性/唯一性的对偶让一套运行时服务两者，面向用户的问题只剩*由谁写下来*，即用户（Austral/Move）还是编译器（Koka/Roc/Lobster）。

从上述一切中提炼的解释器实现清单（即“我们究竟会造什么”）：

(1) 值表示：聚合值用单字句柄，头部带一个标签字与一个引用计数字。小值（整数、浮点、布尔、短字符串）拆箱或驻留，从而完全不带计数（Roc 的数值、Lobster 的内联 struct、Hylo 的寄存器对象）。

(2) 语义：每个绑定拥有，赋值移动。表层按组合 1 取无 `ref`/`mut` 的形式，或按组合 2 取带运行期计数的 `ref`/`mut`。修改编译为“refcount==1 则原地修改，否则复制”，这是解释器快路径上的一次比较，正是 Koka 的 `is_unique(xs)` 与 Roc 的 `may_runtime_uniqueness_check_args` 行。

(3) 丢弃纪律：由字节码编译器发射作用域结束时的丢弃（Perceus 风格，精确），在字节码活性分析有把握处改为最后一次使用时丢弃。终结器（`~Copyable` 资源的 deinit）在丢弃点确定性运行。

(4) 诊断信息：Inko 式借用计数（或单个“共享标志”），在丢弃时检查，给出带栈回溯的确定性“被借用时丢弃”panic，这是静态方案未成熟时的廉价保险。

(5) 字节码中的复用钩子：为“匹配后的构造器”配对与“调用结果落入栈帧槽位”加标注，使 JIT 日后能把“新分配”变成“复用已消费的单元”（Perceus 的 fan-in/fan-out，Roc 的 box_reuse/return_slot pass）。

(6) 解释器循环里的 TCO 与 TRMC：直接尾调用经由栈帧复用（便宜，先做）。TRMC（构造器循环）作为后续的解释器级优化，Koka 的论文给出算法，Roc 的 `trmc.zig` 提供第二份实现可供交叉核对。

(7) 逃逸：每帧的单元 arena，捕获时晋升。闭包只在捕获时才分配（Hylo 风格“绑定到左值则无存储”）。

(8) 度量闸门：热循环中存活的动态唯一性检查数量、被复用的分配占比、解释器分派成本，这三个数字决定组合 1 的快路径是否真的交付了 Koka/Roc 级的行为。

## (c) 权衡表

表 1：策略对我们的判据（LC = 用户学习成本，PE = 峰值性能，EX = 共享/修改的表达力，IF = 在解释器内部的实现成本，DET = 时间与内存的确定性）。

| 策略（代表实现） | LC | PE | EX | IF | DET | 备注 |
|---|---|---|---|---|---|---|
| 追踪式 GC（Go，旧 Inko） | 最低 | 中，有停顿 | 最高（环无碍） | 高 | 差 | Inko 因成本放弃了自己的 Immix 克隆 |
| 原子 ARC（Swift） | 低 | 中（原子操作） | 高（COW） | 低 | 好 | 无循环收集器，相对非原子约 2 倍代价 |
| 精确 RC + 复用 + 借用推断（Koka、Roc） | 低（零标注） | 近 C | 高，环会泄漏（Koka：归纳类型无环） | 高（编译器） | 优秀 | 语言从不提所有权 |
| RC 之上的所有权分析（Lobster） | 最低 | 好（消除 95% 操作） | 高，退出时环报告 | 中 | 优秀 | 只有次优，不会不健全 |
| 运行期借用计数（Inko） | 低 | 好（一个计数器） | 无检查器方案中最高 | 极低 | 好（丢弃时 panic） | 消除计划留待以后 |
| 带代引用（Vale） | 最低（无移动错误） | 好（相对 RC +25% 变 +11%） | 高 | 中（fat reference，检查） | 好 | 默不可变会跳过多数检查 |
| 完全仿射 + 块借用（Austral） | 偏高（新纪律） | C 级 | 低-中 | 低 | 完美 | 规则一页纸，会塑造所有代码 |
| 仿射 + 一等借用 + 生命周期（Rust） | 最高 | C 级 | 最高（图靠 unsafe/索引） | 极高 | 完美 | Polonius Alpha 减少误报，不减难度 |
| QTT/分级（Idris2、Granule、GHC） | 最高（类型里的数字） | 不适用（GC） | 协议级（资产、会话） | 极高 | 不适用 | 0 量给出保证擦除，仅可选启用 |

表 2：每种语言让*用户*写什么（UX 面），2026 年状态。

| 语言 | 移动错误 | 借用标注 | 生命周期标注 | 类型级线性性 | 运行期所有权检查 | 复用/原地优化 |
|---|---|---|---|---|---|---|
| Rust | 有 | 有（`&`、`&mut`） | 有（签名） | 无 | 排他性（调试版） | 手工移动语义 |
| Swift | 无 | 可选（`borrowing/consuming`） | 无 | 可选（`~Copyable`） | 排他性（动态） | COW 约定 |
| Hylo | 无（不存在引用） | 模式 `let/inout/sink/set` | 无 | 可选（`~Copyable`） | 不需要 | 值语义 + EA |
| Koka | 无 | 无（推断） | 无 | 无 | 唯一性检查（refcount==1） | 有保证且可观察（FBIP、TRMC） |
| Roc | 无 | 无（推断） | 无 | 明确拒绝 | 唯一性检查（优化构建中消除） | TRMC、box 复用、COW 内建 |
| Inko | 有（移动） | 有（`ref`/`mut`） | 无 | 无 | 借用计数（panic） | 计划中，未实现 |
| Lobster | 无 | 无（推断） | 无 | 无 | RC 为基准事实 | 编译期 RC（约 95%） |
| Vale | 无 | 无 | 无 | 线性风格可选 | 代检查 | 线性风格 / 区域 |
| Austral | 有（消费一次） | `borrow` 语句 | 无 | 有（全部线性类型） | 无 | 经 free/borrow 手工管理 |
| Dada（设计） | 无 | 权限（`my/our/leased/shleased`） | 无（租约写入时取消） | 无 | 权限监控 | 不适用（设计阶段） |

表 3：对 DreamScript 的语法影响（语法糖预算以 Kotlin 为参照，用户学习其标注面，但标注不进顺手路径）。

| 选项 | 需学习的新关键字/符号 | 签名噪音 | 最坏情况下的错误信息 |
|---|---|---|---|
| 隐藏仿射（Koka/Roc） | 0（也许 `mut`） | 无 | “value shared, so copied”（信息性提示） |
| 动态单一所有权（Inko） | `ref`、`mut` | 低 | “can't drop value: still has 1 reference(s)”（panic 加栈回溯） |
| 参数模式（Hylo） | `inout`、`sink`（2 个） | 仅在声明上有模式 | “cannot capture inout parameter” |
| 可选线性层（Austral 风格） | `linear`、`borrow` | 仅在线性 API 上偏高 | “x is consumed but not returned” |
| QTT（Idris2） | 每个绑定 `0`/`1`/omega | 非常高 | 多重性不匹配 |
| 带代引用（Vale） | 0 | 无 | “use of destroyed object”（确定性陷入） |

## (d) DreamScript 的可行设计组合

约束回顾：解释执行，可降级到内部 IR/字节码。希望有静态线性/仿射类型，绑定默不可变。超高性能且具备一切非底层优化（TCO、拆箱、逃逸分析，可能还有 JIT）。语法糖预算不超过 Kotlin。所有权/async 的 UX 允许重新发明，代数效应可选。偏好集合论类型，排除 C++ 特化，喜爱 Koka UFCS 与单子 Option/Result。组合按用户可见复杂度递增排列。

组合 1：“隐藏仿射”（Koka/Roc 架构，首选推荐）。语言：值默不可变，编译器为每个值跟踪单一逻辑所有者。完全没有所有权语法，只留一个可选的 `mut` 标记用于请求原地修改某个局部变量。标准库是 COW 风味（唯一时追加复用，如 Roc 的 `listAppend`）。运行时：非原子精确引用计数（单线程解释器，或并发时按进程隔离）。每个修改点都是一次 refcount==1 检查（原地修改）或一次复制，延迟丢弃摊薄深层释放。编译器（后续）：按 Roc 的 `arc_solve.zig` 文件头所述实现两阶段借用/owned 推断，加 FLR 栈帧复用与 TCO 加 TRMC。学习成本：最低，所有权反馈以性能提示出现，从不作为错误。性能：函数式工作负载近 C（Koka 已发表的结果），解释器开销被复用循环摊薄。表达力：对面向值的代码高，图/环需要退出时环报告（Lobster）或 weak/id 间接。语法影响：零关键字，至多 `mut`。风险：在低 LC 选项中，推断层需要的编译器工作量最大。缓解办法是先交付运行期唯一性检查（它们本身就能交付 Koka 快路径），把推断纯粹作为优化加入，这正是 Roc 和 Koka 实际的发布顺序。

组合 2：“Inko 基线”，单一所有权加动态借用计数。语言：可见的移动（极易解释）、显式 `ref`/`mut` 借用、无生命周期检查、被借用时丢弃触发确定性 panic。运行时：每个堆对象一个借用计数器，在丢弃时检查。学习成本：极低（Inko 用一页手册讲完整个系统）。性能：好，但没有分析时 incref 密集。表达力：无借用检查器方案中最高（链表、图、迭代器都自然）。对解释器来说是最好的 *v0*：几天可实现，没有不健全的角落，并且与组合 1 向前兼容（同一个 refcount 字段日后承载唯一性快路径，借用计数器兼作共享计数检查）。语法：仅 `ref`/`mut`。风险：别名 bug 变成运行期 panic，错误时机从编译期推到运行期，可接受且可升级（Inko 正是为此计划逐步的编译期检查）。

组合 3：“Hylo 表层”，值语义 + 参数模式 + 可选不可复制。语言：纯值语义，用户代码里没有引用。参数模式默认 `let`（借用），只在非默认时写 `inout` 与 `sink`。资源用带 `deinit` 的 `~Copyable` 类型。带 `let/inout/set/sink` 访问器的下标取代 getter/setter 与迭代器协议，并与 Koka 风格 UFCS 组合（`xs[i].mutate()` 读起来像方法调用，实际是下标借用）。运行时：栈/arena 局部变量，借用是裸指针，按模式传递的值不需要引用计数，RC 只出现在真正的共享点（闭包、集合）。学习成本：低到中（两个关键字，一个概念）。性能：优秀（无别名 -> 无检查 -> 逃逸分析变得平凡）。表达力：限制了部分一等引用形态（没有 `Option<&T>`，外部迭代器需要协程，这是 Hylo 的答案，scattered-thoughts 文章列出了其他会破的地方）。语法：`inout`/`sink`/`~Copyable`，在预算内。它与集合论类型完美组合（可复制性作为取反约束），与单子 Option/Result 组合只有一个注意点：`Result<~Copyable>` 载荷需要 Movable 要求或自动借用。

组合 4：“可选线性资源层”（Austral 风味，叠加在组合 1 或 3 之上）。语言：默认类型由运行期管理，另一个被标记的类别（语法建议：尾部 `!`，如 `File!`，或声明处的 `linear`）必须恰好被消费一次。借用只以块作用域的 `borrow x as y in { ... }` 语句出现，线性方法提供类似 RAII 的协议（close/commit/cancel）。资产（MoonBit/Move 风格）、会话类型连接、不受管理的缓冲都住在这里。运行时：零成本，线性值是不带引用计数的裸句柄，检查器保证不会重复使用，因此既没有计数也没有检查。学习成本：限制在资源层内，Austral 证明整套规则能装进一页。性能：该层完美。语法：2-3 个关键字，限制在资源类型上。这直接回答作者的问题“所有权能否被抽象以降低学习成本同时保住收益”：答案是能，把线性性当作值语义语言之上的一种类 trait 透镜，而非全局纪律。

组合 5：“带代 + 不可变”（把 Vale 特化到我们的默认）。语言：默不可变，共享引用自由复制，永远没有移动错误。可变单元是显式的 `mut T` 类型，代检查只出现在它上面，被覆盖/销毁的值会确定性地让失效引用不再有效。运行时：8-16 字节的 fat reference 加代字。因为不可变值永远不会在别名背后失效，检查只出现在经由 `mut` 单元的写入上，于是稳态开销趋近于零，同时保留“永不悬垂”。学习成本：概念上最低（`mut` 是唯一的新想法）。性能：好，在修改密集代码上略落后于组合 1。表达力：共享方面最高（环是安全的，但会泄漏，除非加入 weak 引用或环报告）。语法：除 `mut` 外零新关键字。如果“永不出现令人困惑的错误”排在原始峰值速度之上，且编译器工作量必须最小，这是最佳选择。

组合 6（分期计划，推荐）：“值优先，推断在后”。先按组合 3 的表层交付组合 2 的运行时（精确 RC + refcount==1 修改检查 + Inko 风格借用计数以提供确定性诊断），其中绑定默不可变，参数用 `inout`/`sink` 模式，资源用 `~Copyable`。再加入组合 4 的可选线性层用于句柄/资产。然后把组合 1 的编译器分析（借用/owned 推断、TRMC、FLR 复用）作为 JIT/AOT 优化培育，语言零改动。这个顺序可行的证据：Roc 与 Koka 都先交付运行期唯一性检查（语义）、后交付推断（优化）。Inko 与 Lobster 证明运行期部分是小组可实现的。Hylo 的模式表层与 Austral 的线性层证明面向用户的语法在我们的语法糖预算内。长达十年的 Polonius 传奇说明，最该避开的设计起点正是带生命周期推断的一等借用。

给类型系统的横切说明：(1) 把可复制性、可发送性与线性性表达为*集合论*约束（默认为正 trait `Copyable`，用 `~` 减去，可与并集/交集类型组合），让资源无需第二个类型宇宙就能与泛型代码集成，Swift SE-0390 在生产中演示了该机制。(2) 避免用户签名中的多重性多态（Linear Haskell 的 UR 之苦），改为推断多重性或固定多重性。(3) 代数效应是正交的：Koka 的整套复用/借用栈在效应处理器之下运行（evidence passing 组合干净），因此 async/所有权不该像 Rust 那样耦合，Swift 的区域隔离表明并发可以是独立的后续轴。(4) 错误信息政策：遵循 Roc/Lobster，所有权决策以关于复制的信息性提示出现，从不作为错误，硬错误留给可选线性层。(5) 对解释器而言，把引用计数字视为值单元的实现细节，并让语义保持“仿佛是不可变副本”，使后续编译器能把运行期检查换成已证明的事实而没有可观察变化（Dynamic-Dada 的类型擦除原则与 Lobster 的“分析只做消除”原则都保证了这种可升级性）。

## 附录：一手资料索引

克隆的仓库（2026-11-03/04 阅读，写完即删除）：Z:/Temp/plr1/roc（design.md、Glossary.md、reunify.md、docs/langref/、src/lir/arc.zig、arc_solve.zig、arc_certify.zig、rc_effect_rules.zig、trmc.zig、box_reuse.zig、return_slot.zig、StackPlan.zig、immortal_locals.zig）。Z:/Temp/plr1/koka（doc/spec/why.kk.md、readme.md、src/Backend/C/Parc.hs、src/Core/Borrowed.hs）。Z:/Temp/plr1/inko（docs/source/getting-started/memory-management.md、docs/source/design/runtime.md、docs/source/design/compiler.md）。Z:/Temp/plr1/hylo（docs/LocalBindings.md、docs/MiscNotes.md、docs/RemoteParts.md、StandardLibrary/Sources/Core/Copyable.hylo、Movable.hylo）。Z:/Temp/plr1/austral（docs/walkthrough.md、README.md）。Z:/Temp/plr1/granule（README.md）。Z:/Temp/plr1/vale（architectural-direction.md、docs/GenerationalReferences.md、docs/LinearRegion.md、docs/DropFree.md）。Z:/Temp/plr1/lobster（docs/source/memory_management.md）。Z:/Temp/plr1/dada（book/docs/design_docs/goals.md、calculus.md、why_not_explicit_end_for_lease.md、book/docs/dyn_tutorial/my.md、our.md、lease.md、shlease.md、atomic.md）。Z:/Temp/plr1/se0377.md、se0390.md、se0430.md（Swift evolution 提案 SE-0377、SE-0390、SE-0430）。

关键 URL：Perceus 论文 https://www.microsoft.com/en-us/research/publication/perceus-garbage-free-reference-counting-with-reuse/。FLR 论文 https://www.microsoft.com/en-us/research/wp-content/uploads/2021/11/flreuse-tr.pdf。Koka 书 https://koka-lang.github.io/koka/doc/book.html。Roc FAQ https://www.roc-lang.org/faq。Roc fast 页面 https://www.roc-lang.org/fast。Roc 的 Zig 改写 https://ziggit.dev/t/roc-lang-to-be-rewritten-in-zig/8363 与 https://news.ycombinator.com/item?id=42935516。Roc 0.1.0 预览 https://www.reddit.com/r/programming/comments/1vn7t7t/a_preview_of_roc_010_by_richard_feldman。Polonius Alpha https://blog.rust-lang.org/2026/08/04/enabling-polonius-alpha-on-nightly。RFC 1857 https://rust-lang.github.io/rfcs/1857-stabilize-drop-order.html。without.boats 的 “Ownership” https://without.boats/blog/ownership。scattered-thoughts 的 “Borrow-checking without type-checking” https://www.scattered-thoughts.net/writing/borrow-checking-without-type-checking。Idris 2 ECOOP 2021 https://drops.dagstuhl.de/entities/document/10.4230/LIPIcs.ECOOP.2021.9。Idris2 多重性 https://idris2.readthedocs.io/en/latest/tutorial/multiplicities.html。Granule https://granule-project.github.io/。GHC 线性类型 https://ghc.gitlab.haskell.org/ghc/doc/users_guide/exts/linear_types.html 与提案 0111 https://ghc-proposals.readthedocs.io/en/latest/proposals/0111-linear-types.html。ATS 书第 13 章 http://ats-lang.github.io/FROZEN000/DOCUMENT/INT2PROGINATS/HTML/。Clean 报告 https://clean.cs.ru.nl/Clean。Uniqueness Typing Simplified https://www.cs.tufts.edu/comp/150FP/archive/edsko-de-vries/uniqueness.pdf。Futhark 唯一性 https://futhark-lang.org/blog/2022-06-13-uniqueness-types.html。Entente Cordiale https://recurial.com/LinearityAndUniqueness.pdf。Vale 带代引用 https://verdagon.dev/blog/generational-references。Vale 内存安全概览 https://vale.dev/memory-safe。Immix 论文 https://www.cs.utexas.edu/users/speedway/DaCapo/papers/immix-pldi-2008.pdf。Inko 所有权论文 https://inko-lang.org/papers/ownership.pdf。Lobster https://github.com/aardappel/lobster 与 https://strlen.com/lobster。Dada https://github.com/dada-lang/dada。Verona https://github.com/microsoft/verona。MoonBit beta https://www.moonbitlang.com/blog/beta-release 与路线图 https://github.com/moonbitlang/website/blob/main/blog/2025-07-24-roadmap/index.md。Andrew Johnson 的 hybrid linear/RC/GC https://andrewjohnson4.substack.com/p/hybrid-linear-reference-counted-garbage。智能合约语言综述 https://github.com/s-tikhomirov/smart-contract-languages。Inko 手册 https://docs.inko-lang.org/manual/。Hylo https://github.com/hylo-lang/hylo。
