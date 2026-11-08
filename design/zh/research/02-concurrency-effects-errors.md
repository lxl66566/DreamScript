# DreamScript 研究笔记 02：并发、async、代数效应与错误处理

DreamScript 的研究笔记，2026-11-04。范围：代数效应系统（Koka、Effekt、Unison、OCaml 5、Eff/Frank），函数颜色问题及其每一种逃离尝试（Zig、Go、Java Loom、Kotlin、Trio/结构化并发、Rust、Swift 6），不依赖借用检查器的隔离式并发（Pony、Verona、BEAM、Inko、Mojo），错误处理模型，以及 DreamScript 的候选设计组合。证据主要来自源码（研究期间克隆在 Z:/Temp/plr2，现已删除）与论文/文档。URL 就地给出。

## a) 概要

1. Koka 证明 async 可以是一个普通效应：`std/async`（随 Koka 3.2.9 发布，2026-09）定义了 `pub alias async = <async-await, async-ioc, async-cancel, discontinue>`，即四个原语效应。取消作用域、`interleave`、`firstof`、`timeout` 全部作为用户级处理器构建（lib/std/async/async.kk、samples/async/basic.kk）。
2. Koka 的处理器通过“广义证据传递”（Xie & Leijen，ICFP'21）编译为普通 C：不做栈切换，续延是堆上的闭包，在线程上下文中最多内联组合 8 层（kklib/include/kklib.h 的 `kk_yield_t`），证据向量按唯一 marker 分派操作。这套机制可以直接搬到字节码解释器。
3. Koka 的诚实说法是：`wait : duration -> async ()` 就明显在行里带着 `async`，颜色本身可见。它真正主张的是颜色*可推断、可组合，并能在处理器边界处消失*，而且语言里任何地方都没有 `await` 关键字。
4. Effekt 是编译效率的领先者：能力是二等参数，因此处理器编译为在真实栈上运行的直接风格多 prompt 续延（LLVM 运行时：每个处理器一个 `%Prompt`，栈帧为 `{ReturnAddress, Sharer, Eraser}`，捕获时复制/保存栈，见 libraries/llvm/rts.ll）。关键论文：“Effects as capabilities”（OOPSLA 2020）、“From Capabilities to Regions”（OOPSLA 2023）、“Multiple Resumptions and Local Mutable State, Directly”（ICFP 2025）。
5. Effekt 也解决了 Result 与异常的取舍：其标准库把 `Result[A,E]` 定义成 `Exception[E]` 效应的字面“物化”，并提供 `value`（单子式反射）反向回去（libraries/common/result.effekt）。两者是同一机制的两个视图。
6. Unison（1.0，2025-11）展示了最小效应系统：内置能力恰好只有一个 `IO`（外加 `Exception`），按最近的封闭 lambda 做环境能力检查，有存在量化的行变量，但不推效应多态的工效学。学习成本低，粒度粗。
7. 在 5.4（2025-10）的手册里，OCaml 5 的效应处理器仍标注为实验性，最强的工业验证尝试在四年之后仍未完成。
8. Zig 删掉了无颜色的 async（0.11，2023，实现不可靠），并在 0.15/0.16 围绕显式的 `Io` 能力参数重建：`fn connect(host, io: Io, port)`：同一份代码可以跑在阻塞线程、线程池或 io_uring 事件循环上。`io.async(...)` 给出无颜色的结构化并发，配合 `Io.Queue`/`Io.Group` 与取消。Zig 的修法是传递能力，也就是去掉行多态的效应。
9. Go：有栈 goroutine + 内置 netpoller + 抢占式调度 = 构造上就无颜色。代价是无结构的 spawn（`go` 语句）、没有取消方案（`context.Context` 是后补的）、goroutine 泄漏，以及约 2KB 的最小栈。
10. Java Loom 通过让线程变便宜，在实践中让颜色问题消失了（堆分配的栈块，在阻塞调用上挂载/卸载）。虚拟线程在 Java 21 定稿。Scoped Values 在 Java 25 定稿。StructuredTaskScope 经过 6 轮迭代仍是预览（JEP 505/525/533，JDK 25-27）。难点在结构化并发，线程本身并不难。
11. Kotlin 保留了颜色（`suspend`），并在结构化并发上取胜：Job 树（子随父取消、父等子完成、失败向上传播）是其他语言一直在抄的部分。
12. Trio 的 nursery 文章（2018）仍是最清晰的表述：任务生命周期必须在词法作用域内，取消必须协作式且带作用域，nursery 在子任务完成前不得返回。
13. Rust 的 async 是反面教材：`async fn` 在 trait 中的 `Send` 约束无法命名、`Pin`、运行时碎片化（tokio 事实上成为标准）、按 drop 取消。without.boats（2024-04）论证协程与效应处理器同构，而有栈协程是更合适的落点。
14. Swift 6 的基于区域的隔离（SE-0414）加 `sending`（SE-0430）是精简版所有权推断，采纳它们正是为了让 async 的数据竞争安全可接受。DreamScript 的线性类型让同一套推理精确，无需推断。
15. Pony/Inko 证明近似线性的可发送性加进程隔离可以在没有共享内存的前提下获得数据竞争自由。BEAM 在此之上加了抢占式延迟保证与 let-it-crash，代价是发送时复制。Inko（本仓库已 vendor）是现有最接近“线性消息、无 async 关键字、无共享可变状态”的语言。
16. 错误模型正在收敛到：预期错误用 Result 即数据（Rust/Roc/Zig/Swift），bug 用无法处理的 panic/陷入（Rust/Zig/Swift/Inko），监督用 let-it-crash（BEAM）。受检异常已经消亡。“错误并集放进返回类型”（旧 DreamScript 的想法）本质就是 Roc 的推断式标签并集错误，形式上是一个受限的效应行。Koka 的 `exn` 和 Effekt 的 `Exception[E]` 是同一想法的效应表达。
17. 对解释执行的 DreamScript 而言：Result 返回在寄存器 VM 里约等于每次调用一次分支。展开式异常的成本是解释器循环里弹栈帧。堆分配的有栈协程很便宜（Lua/LuaJIT 先例）。代数效应处理器可以用证据向量以不大的成本实现，但多次恢复与线性资源冲突严重，这正是 Koka 要有 `linear effect`（仅尾恢复）、Effekt 要限制为词法处理器的原因。
18. 结论（细节见 d 节）：给定线性类型、默不可变、Result 与 nursery，完整的行多态可恢复效应在 DreamScript v1 里收不回成本。“传递能力式 IO + 取消作用域”内核（Zig/Effekt 风格，即没有行多态表层语法的效应）值得做，并为以后升级到完整效应留门。VM 内的有栈绿色线程在 async 上收益/学习成本比最高。

## b) 各技术深入分析

### 1. Koka：效应行、async 即效应、证据传递编译

仓库：https://github.com/koka-lang/koka（克隆于 commit d23ee7c，2026-10-27，版本 3.2.9）。导览：https://koka-lang.github.io/koka/doc/book.html。本节引用的路径都在该克隆内。

**效应行与推断。** Koka 推断每个函数的完整效应。效应为空是 `total`（或 `<>`）。`exn` 表示异常，`div` 表示发散，`ndet` 表示非确定性，`io` 是最坏的效应（doc/spec/tour.kk.md，“Effect types”）。行是集合，用扩展语法 `<l|e>`：`while : (pred : () -> <div|e> bool, action : () -> <div|e> ()) -> <div|e> ()`。在调用点，行变量被扩展并统一，于是谓词的 `ndet` 与动作的 `exn` 自动成为 `<exn,ndet,div|e3>`，行免费取并集。内置的原子效应包含能力风格的成员（lib/std/core.kk）：

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

对 DreamScript 而言有两点值得注意。第一，`net`/`fsys`/`ui` 是*作为效应的能力名*，行同时充当权限集，这恰好回答了能力系统要回答的问题：“这个函数能碰什么”。第二，导览给出了指称语义上的论点：`int -> exn int` 字面上就是 `Z -> (Z + 1)`，效应行与返回类型并集是同一语义的两种呈现。这个观察是通往“把错误收集进返回类型并集”想法（7.8 节）的形式桥梁。

**处理器。** 声明与处理（导览 “Handling” 与 “Resuming” 两节）：

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

`resume` 回到调用点继续执行（允许多次恢复）。不调用 `resume` 则返回到处理器作用域（用于 `fuel` 式的中止）。`final ctl` 声明一个不可恢复的操作：Koka 的内置异常就是 `final ctl throw-exn( exn : exception ) : a`（lib/std/core/exn.kk），async 库用来杀掉 strand 的同样写作 `final ctl discontinue() : a`（lib/std/async/async.kk）。`raw ctl` 暴露一等公民的 `rcontext`，之后甚至可以从另一个作用域恢复或终结它，这是给调度器用的逃生口。`finally`/`initially` 组合子在正常返回时、以及不可恢复操作展开时都执行清理，因为多次恢复会让“RAII 依赖单次恢复”这个假设失效（导览 “fread/with-file” 例子，文档承认“正在研究用类型系统静态阻止这种情况”）。

**线性效应。** `pub linear effect async-cancel` 逐字出现在 lib/std/async/internal.kk。线性效应（导览 “Linear Effects”）的文档定义是：操作永远是尾恢复、且只使用线性效应本身的效应。它“免除了这类效应的单子翻译，让代码更紧凑高效”。这正是我们要的那条洞察的类型化形式：*效应处理器贵在捕获续延，类型系统一旦保证尾恢复，捕获就消失了*。相关前置论文：Leijen & Brachthäuser，“Taming Control-flow through Linear Effect Handlers”（HOPE 2018）。

**async 即效应。** 现代库（lib/std/async/async.kk 的文档注释）基于 Leijen，“Structured Asynchrony with Algebraic Effects”（MSR-TR-2017-21，https://www.microsoft.com/en-us/research/wp-content/uploads/2017/05/asynceffects-msr-tr-2017-21.pdf）。async 效应是四个原语的别名：

```koka
pub effect discontinue
  final ctl discontinue() : a

pub alias async = <async-await,async-ioc,async-cancel,discontinue>
```

`async-await` 执行 `do-await(setup, scope, label)`。setup 回调在*处理器作用域*内运行，稍后以 `await-result<a> = Cancel | Exception | Result` 恢复该 strand。`async-cancel` 是一个持有当前取消 `async-scope` 的线性效应（唯一 id 的链条，子作用域构成一棵树）。取消以 `Cancel` 结果的形式被观察到，并被送达尚未完成的操作，包装层把它翻译成 `discontinue()`，因此 `finally` 块仍会执行：取消带清理，无需打断线程。版本历史（doc/spec/news.mdk）：3.1.0（2024-02）重新设计了具名效应的类型。完整的 `std/async` 模块（事件循环、channel、交错）首次随 3.2.9（2026-09-17）发布。实验性的并行任务从 2.2.1（2021）起就在 `std/os/task` 里：`task : (() -> a) -> promise<a>`、`await`、`parallel` 实现在一个 C 线程池上（`kk_task_schedule`），实验性的 LVar 被标注为“目前在 pure 效应下不安全！！需要工作”。

结构化组合子（lib/std/async/interleave.kk）就是基于处理器的普通库函数，这就是“async 不需要任何语法”的落点：

```koka
pub fun timeout( secs : duration, action : () -> <async,ndet|e> a ) : <async,ndet|e> maybe<a>
  firstof { wait(secs); Nothing } { Just(action()) }

pub fun two/interleaved( action1 : () -> <async|e> a, action2 : () -> <async|e> b ) : <async|e> (a,b)
```

channel 同样是库模块（lib/std/async/channel.kk），其类型如实暴露了发生的事情：`receive : channel<a> -> <async,ndet> a` 诚实地声明自己非确定（两个发送者竞争），而 `emit : (channel<a>, a) -> async ()` 声明自己确定。channel 的状态机（`Empty | Values | Waiting(listeners)`）就是普通数据，配合 `ioc` 作用域内的可变性。示例程序（samples/async/basic.kk）读起来像 Go，但里面没有 goroutine 关键字：`val (a,b) = interleaved(stranda, strandb)` 与 `val r = firstof(slow, fast)`，结构化派生、竞争、超时由三个组合子实现，语言特性无需增加。

核心库里还有两个类型特性对能力设计有意义。第一，Koka 的效应词汇里包含权限风格的原子效应（`net`、`fsys`、`ui`、`blocking`，见 core.kk），它们充当*受检的能力名*，行就是一次权限审计。第二，“具名效应”（`pub type nmd :: X`、`pub type scope :: S -> X`，见 core.kk，据 news.mdk 在 3.1.0 重新设计）给效应附加一个作用域参数，使得该效应的证据值无法逃出其处理器。这是对“一等恢复值到处都会泄漏动态作用域”这一问题的类型级修法，导览里的 `raw ctl`/`rcontext` 逃生口与它并存。

注意类型里的 `<ndet>`：让两个 strand 竞争是非确定的，行就这么写。`collapse-ndet`/`pretend-no-ndet` 组合子会在实现确实保证确定性的地方重新认证确定性（例如 `list/interleaved` 承诺输出列表与输入顺序一致）。主流语言都不在类型里追踪非确定性。对数据不可变的 DreamScript 来说，暴露它的成本几乎为零。

“async 是普通效应”这一主张的精确表述是：`wait` 的类型是 `duration -> async ()`，所以 async 颜色可见，也是被推断出来的。Koka 并不隐藏它。Koka 去掉的是所有让颜色*带来痛苦*的东西：（1）没有 `await` 关键字，执行 `wait` 就是一次普通调用。（2）效应多态的组合子（`map`、`bracket`、用户代码）只写一次，就能在任何处理器下工作，包括 async 处理器。（3）事件循环就是当 `main` 带 `async` 效应时安装在它周围的默认处理器（lib/std/async/internal.kk 的文档注释）。（4）同步与异步代码可以混用：同步函数可以从异步调用，反之亦然，不需要复制 API。残余成本是真实的：你写的每个组合子都要带一个行变量。错误信息会提到行。两个代码库仍然可能对“允许哪些效应”有分歧（道德层面的颜色问题转移进了推断里）。

**处理器如何编译（不做栈切换）。** 运行时契约在 lib/std/core/hnd.kk（编译器生成的处理器原语）与 kklib/include/kklib.h 里可见：

- 处理器即证据：一个存在量化的四元组 `(marker, handler, htag, evidence-vector)`，其中 `marker : marker<e,r>` 是代表当前应答上下文 `<e,r>` 的唯一整数，处理器“包含所有操作（很像虚方法表）”（hnd.kk 头部注释）。
- 线程上下文携带一个证据向量 `evv`，以及内联的 yield 记录 `kk_yield_t { marker, clause, conts_count, conts[8] }`。执行时生成的代码设置 `ctx.yielding`，运行时找到 marker 匹配的处理器，用操作参数调用它的子句。“最终的续延 `k` 组合为 `fN ○ ... ○ f1`”，若 8 个槽位用满，则由一个新数组持有组合后的闭包（kklib.h，`kk_context_t`）。
- 于是：续延是堆上的函数组合，每个捕获段一次分配，没有机器栈切换，在 C 与 JavaScript 后端行为一致。形式基础是 Xie & Leijen，“Generalized Evidence Passing for Effect Handlers, or Efficient Compilation of Effect Handlers to C”（ICFP'21，https://www.microsoft.com/en-us/research/publication/generalized-evidence-passing-for-effect-handlers-or-efficient-compilation-of-effect-handlers-to-c/）。
- 尾恢复操作完全跳过捕获（子句带着调用者的续延运行），这就是 `linear effect` 存在的原因。
- 内存管理是 Perceus：精确引用计数加静态复用分析，即“无垃圾”。复用分析与“functional but in place”（FBIP）让函数式代码原地修改（导览 “FBIP” 节，Reinking、Xie、de Moura、Leijen，PLDI 2021）。Perceus 正好接上线性类型：线性值的 RC 静态为 1，其 RC 操作可以直接删掉。

**对字节码 VM 版 DreamScript 的影响。** 证据传递 ABI 可以在没有 OS 线程机制的解释器里实现：处理器变成上下文向量里的 (marker, op-table) 对。执行 = 走向量。捕获 = 把执行点与处理器之间的栈帧链物化成闭包。多次恢复 = 重放闭包（必须不持有线性资源：类型系统要么禁止在多次恢复的续延里捕获线性值，要么把效应限制为 `linear effect` 那样的尾恢复）。这是我们找到的、在 IR/字节码语言里实现效应最便宜且可靠的设计。

### 2. Effekt：能力传递、二等的效应、多 prompt 续延

仓库：https://github.com/effekt-lang/effekt。论文列表：https://effekt-lang.org/publications（2026-11 克隆）。

**基于能力的类型。** “Effects as capabilities: effect handlers and lightweight effect polymorphism”（Brachthäuser、Schuster、Ostermann，OOPSLA 2020，doi:10.1145/3428194）是该语言的正式引用。每个带效应的函数都把它的处理器作为显式的二等参数（*能力*）接收。效应多态存在，但形式是上下文式的：绑定器记录一个块可以使用哪些能力，而这些能力不能被一等值捕获并逃出作用域。调度器案例研究（examples/casestudies/scheduler.effekt.md）展示了整套模型：

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

关键在那段解说：`prog` 是二等的，因此它可以使用*未体现在其类型里*的能力（上下文式效应多态）。存放在可变单元里的恢复值必须把 `{this, prog}` 记录为它的捕获集，而由于这个单元本身是二等的（由 `region` 作用域的变量），“即使恢复值被存进可变单元，它也无法泄漏”。这就是用区域做生命周期追踪，做的是 Rust 用 lifetime 做的工作，回报是编译效率。

**编译。** 流水线降级到一台显式的机器 IR，指令为 `Reset(prompt, frame, rest)`、`Resume(value, rest)`、`Shift(name, prompt, rest)`（effekt/shared/src/main/scala/effekt/machine/Tree.scala 经 machine/Analysis.scala），即在显式栈上的多 prompt 定界控制。LLVM 运行时（libraries/llvm/rts.ll）实现真实栈：每个栈帧是 `{ReturnAddress, Sharer, Eraser, payload...}`，环境带引用计数，每个处理器有唯一的 `%Prompt`，捕获时保存 prompt 与当前指针之间的栈段，恢复时还原。论文：“Capability-Passing Style for Zero-Cost Effect Handlers”（ICFP 2020）、“A Typed Continuation-Passing Translation for Lexical Effect Handlers”（PLDI 2022）、“From Capabilities to Regions: Enabling Efficient Compilation of Lexical Effect Handlers”（OOPSLA 2023）、“Multiple Resumptions and Local Mutable State, Directly”（ICFP 2025），另有一篇正面针对效应的追踪 JIT 论文（Gaißert、Bolz-Tereick、Brachthäuser，OOPSLA 2025，https://pl.cs.uni-tuebingen.de/publications/gaissert2025tracing.pdf），与 DreamScript 的 JIT tier 直接相关。effekt/jvm/src/main/scala/effekt/Backend.scala 中提供的后端有：`llvm`、`js`，以及三种 Chez Scheme 方案（`chez-cps`、`chez-monadic`、`chez-callcc`），同一语义被证明可以在四种执行策略上表达。

核心主张是：由于能力是二等的、处理器是词法的，常见情形根本不分配续延。效应处理器基准测试与手写直接风格代码相当（OOPSLA 2023 论文）。代价是表达力：没有一等恢复值（调度器之所以能工作，只是因为队列活在处理器自己的区域里）、没有深层动态作用域，而且效应“颜色”仍然出现在块类型里（`=> Unit / Proc`）。但用户从不写行多态的花活，他们传递能力。

**错误。** Effekt 的标准库把 Result/效应的二重性写明了（libraries/common/exception.effekt 与 result.effekt）：

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

注意 `Exception[E]` 是*以错误类型为索引*的：效应行携带错误集，正是“可能出什么问题都写在签名里”这一性质，但配有控制流语义。对 DreamScript 而言，这是现有最直接的证明：“返回类型里的错误并集”与“错误效应行”可以互相定义。选择只关乎工效学（值可以被存储、返回、当数据组合，效应会在高阶调用中自动传播）。

### 3. Eff、Frank、OCaml 5：效应谱系的其余部分

Eff（Preu & Plotkin，“Handling Algebraic Effects”，2017，实现 “Eff directly in OCaml”）是最小的动态作用域处理器语言。它的历史意义重要，本设计放弃它，理由是无类型的动态处理器。Frank（Lindley、McBride、McLaughlin，标题为 “Do be do be do”，POPL 2017，https://homepages.inf.ed.ac.uk/slindley/papers/frank.pdf）引入了*双向*内联处理器（`do`/`handler` 加 `yield->k` 绑定），写控制抽象时很顺手，但它的效应推断（类型检查之后做 Frank 式泛化）正是 Unison 后来在其场景里发现不可靠的东西（见 Unison 的 docs/ability-typechecking.markdown），也是行推断普遍难以处理的部分。

OCaml 5 在多核运行时里发布了效应处理器（5.0，2022-12）。截至 5.4（2025-10，https://ocaml.org/manual/5.4/）的状态：手册仍把效应放在“实验性”章节。5.2 加入了浅处理器语法，5.3（2025-01）加入了一等*深*处理器语法（https://ocaml.org/changelog/2025-01-08-ocaml-530）。生态里基于效应的 IO 库是 Eio。浅处理器形式直接展示 perform/continue：

```ocaml
type 'a continuation                    (* opaque *)
effect Get : int continuation          (* effect carrying a continuation *)

let ask () = perform Get
let answer =
  match ask () with
  | effect Get k -> continue k 42      (* handler receives the continuation *)
  | v -> v
```

教训：（1）在久经考验的原生运行时里，处理器快到足以发布，但卡住稳定化的是*API/类型*故事：OCaml 完全没有效应类型，因此 Koka/Roc 那种签名审计缺失，采纳依赖约定。（2）OCaml 完全绕开了颜色：包括 IO 在内的每个效应都是效应，所以没有两个世界的分裂（Eio 程序看起来就是普通代码）。（3）相对 OCaml 4 代码，栈密集负载确实存在 happy path 开销，GC 并行度回退也花了好几个版本才修好。交叉参考：Sivaramakrishnan 等，“Concurrent System Programming with Effect Handlers”（https://kcsrk.info/papers/system_effects_feb_18.pdf），这是更早的 Koka 时代工作，展示 fork/join、取消、超时都只是库代码形式的处理器。

### 4. Unison：把能力做成最小可行的效应系统

仓库：https://github.com/unisonweb/unison（关键内部设计笔记是 docs/ability-typechecking.markdown）。文档：https://www.unison-lang.org/docs/fundamentals/abilities/。Unison 1.0 于 2025-11 发布（https://news.ycombinator.com/item?id=46049722）。

Unison 只保留一个内置 I/O 能力，外加 `Exception`。类型在箭头上携带所需的能力：`a ->{IO, Abort, State Nat} b`。检查规则被刻意做得简单（引自设计笔记）：“子项的环境能力集定义为：*最近的封闭 lambda* 的类型上所需的能力，加上封闭处理器所消除的能力。”所以 `foo : Text ->{} ()` 调用 `IO.printLine` 会失败，因为封闭 lambda 的环境集为空。部分应用只在携带该能力的绑定之后才获得能力。用户定义的能力与处理器就是对请求做模式匹配并显式恢复。文档的心智模型页（https://www.unison-lang.org/learn/fundamentals/abilities/，用简化伪代码呈现该想法）展示了 KVStore 的形状：处理器通过恢复把自身状态传递下去：

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

Unison 最初考虑过 Frank 式泛化并否决了它：“我意识到在类型检查之后做 Frank 式效应泛化并不可靠”。替代方案是 `a -> b` 表示带一个存在量化的 `e` 的 `a ->{e} b`，由检查精化。处理器使用 `handle ... with ...`，而 `run` 命令在顶层提供 IO/Exception 处理器（文档 “Running Unison programs”，https://www.unison-lang.org/docs/usage-topics/running-programs）：

```unison
main : '{IO, Exception} ()
main = do
  printLine "Hello"
-- ucm: .> run main
```

Unison 用这份简单换来的有：每个效应操作都是经该能力分派的普通延迟函数调用，没有行多态动物园，也没有效应包含关系难题。测试可以用纯处理器替换 IO（FAQ 强调了纯测试处理器）。内容寻址的代码加签名里的 `{IO}` 给出精确的效应审计。它付出的代价：只有粗略的一级 IO 能力，意味着在类型上无法区分文件访问与网络访问（Koka 可以，靠 `net`/`fsys`）。并发留在运行时层（内置函数含 `IO.forkComp.v2 : (() -> a) -> IO ThreadId`，见 parser-typechecker/src/Unison/Builtin.hs），语言里没有类型化效应。用户定义能力的分派在值层（处理器就是接收 `Request` 的普通函数），因此多次恢复可行但无追踪。

**对 DreamScript 的要点：** 效应谱系上的“精简能力”一端的做法是一两个内置效应、环境检查、存在量化行、没有效应多态组合子动物园，实现成本低、学习成本低，并且在 Koka 推断复杂度的一小部分里交付了大部分审计价值，也就是能看出哪些函数会接触外部世界。

### 5. 函数颜色问题与各路逃离尝试

参考点：Paul Phillips 的 “What Color is Your Function?”（journal.stuffwithstuff.com，2015，https://journal.stuffwithstuff.com/2015/02/01/what-color-is-your-function/）定义了这个问题的内容：异步函数构成第二种颜色，它不能在不阻塞的情况下调用同步函数，同步函数也不能在不派生的前提下调用异步函数，`await` 会传染，而你要为这种分裂付出 API 复制的代价。without.boats，“Coroutines and effects”（2024-04-20，https://without.boats/blog/coroutines-and-effects/，作者读过的那篇文章）补充了结构性分析：效应系统（Koka/Effekt）与协程（Rust async/generator）在某些意义上同构，都是“静态类型但动态作用域”的控制转移。用协程时调用者在每个调用点显式转发效应（`.await?`），用效应系统时转发通过函数类型隐式进行。作者自述的理想是有栈协程，以便允许递归。下面有六次逃离尝试。

**5.1 Zig：删掉 async，重建为能力传递。** Zig 原本的无颜色 async/await（由编译器推算挂起图）在 0.11（2023）被移除，因为维护者判定该实现有相当部分不可靠。0.14/0.15 完全不带 async 发布。0.15.1（2025-08）落地了 “Writergate” I/O 重设计，包含新的 `std.Io` 接口（https://ziglang.org/download/0.15.1/release-notes.html）。0.16 把 async/await 作为其上的薄层带回来（devlog “Async DNS Resolution”，2025-10-15，https://ziglang.org/devlog/2025/，HN 讨论 “Zig's New Async I/O”，https://news.ycombinator.com/item?id=44545949）。新模型（devlog 代码）：每个具备 I/O 能力的函数都接收一个 `Io` 参数：`pub fn connect(host_name: HostName, io: Io, port: u16, ...)`，其中 `Io` 是一个接口（vtable），其实现是阻塞线程式或事件式（io_uring）。并发原语是该能力的方法：`io.async(fn)` 派生无颜色的并发工作，`Io.Queue` 与 `Io.Group` 收集/取消结果，取消是显式的（`group.cancel(io)`、`error.Canceled`）并与 `defer` 组合。Kelley 为 DNS 例子声称的性质：在线程、事件循环或单线程阻塞下都能最优工作，不需要堆分配，“我们仍然使用所有常规控制流：try、defer、while”。对 DreamScript 的分析：Zig 把颜色变成一个显式的*能力参数*（`Io` 就是行成员，在运行时传递），并用接口分派代替类型级效应多态。这是 Effekt 的能力模型且完全没有效应系统，也是“能力传递式 async”可以在零类型系统机制的系统语言里落地的最强证据。

**5.2 Go：构造上就有栈且无颜色。** Goroutine 是有栈协程（2KB 可增长栈），由 M:N 运行时可抢占式调度：用户 goroutine（G）复用到 OS 线程（M）上，每个线程在一个逻辑处理器（P）上执行，P 有本地运行队列。调度器做工作窃取，而且关键在于*集成了 netpoller*（src/runtime/netpoll.go 中的 epoll/kqueue/IOCP），因此阻塞在网络 I/O 上的 goroutine 会被挂起，它的 P 转而做其他工作。事件循环与调度器是同一个东西，这就是为什么完全没有 async 库分裂。自 Go 1.14（2020）起，非协作式异步抢占用信号打断计算循环（设计文档：https://go.googlesource.com/proposal/+/master/design/24543-non-cooperative-preemption.md），堵上了“紧凑循环饿死调度器”的洞。系统调用通过脱离 M 或注册 netpoller 处理。后果：单一函数颜色，到处都是阻塞风格代码，FFI 调用与系统调用由运行时吸收。代价侧同样真实：函数入口的栈增长检查（每次调用一次比较加分支的前导代码）、增长时的栈复制、每个 goroutine 约 2KB 的地板，以及（真正的痛点）无结构并发。`go f()` 让任务变成孤儿，取消是后补的 `context.Context` 参数约定，穿过每个想支持取消的 API，消失的 goroutine 的错误会丢失，泄漏检测靠手工（“Lies we tell ourselves to keep using Golang”，fasterthanli.me，https://fasterthanli.me/articles/lies-we-tell-ourselves-to-keep-using-golang，已在作者阅读清单里。注意那篇文章的另一半：Go 没有数据竞争自由的故事，竞争检测器是动态的）。Go 证明：只要运行时吸收成本，无颜色的有栈 async 非常出色。它也证明无颜色加无结构仍然错误率很高，这正是 Trio/Kotlin 随后在结构轴上修掉的东西。

**5.3 Java Loom：让线程变便宜，从而让颜色消失。** 虚拟线程（JEP 444，Java 21，2023 定稿）是由 JVM 管理的有栈续延：一个虚拟线程挂载到载体 OS 线程（ForkJoinPool）上，并在 JDK 任一个阻塞 API 上阻塞时*卸载*，那个阻塞了 20 年的 `Socket.read` 现在改为挂起一个堆分配的栈块。没有新语法，没有 `await`，没有执行器分裂：颜色问题通过重新解释既有阻塞 API 集合被抹掉。2025-2026 状态：Scoped Values 在 Java 25 定稿（JEP 506，2025-09）。结构化并发经过六轮迭代仍是预览，即 JEP 505（第 5 次预览，JDK 25）、JEP 525（第 6 次预览，JDK 26）、JEP 533（JDK 27）（https://openjdk.org/jeps/505）。pinning 的毛病（`synchronized` 会 pin 住载体线程）由 JEP 491 在 JDK 24（2025-03）修掉。有教益的部分在于：线程本身早已解决，难的是 `StructuredTaskScope` 的汇合策略、取消策略与异常传播语义，它们花了六个预览周期，且仍在争议中。设计精力要花在结构上，机制很便宜。

**5.4 Kotlin：保留颜色，在结构上取胜。** Kotlin 的 `suspend` 函数编译成状态机。颜色分裂是真实的（suspend 与非 suspend API，`suspendCancellableCoroutine` 胶水），但采纳率站住了，因为（a）JVM 互操作意味着阻塞库仍然能用，（b）结构化并发是默认行为。引自 https://kotlinlang.org/docs/coroutine-context-and-dispatchers.html：“当一个协程在另一个协程的 CoroutineScope 中启动时，它通过 CoroutineScope.coroutineContext 继承其上下文，新协程的 Job 成为父协程 Job 的子项。父协程被取消时，它所有子项也递归被取消”，“父协程总是等待其所有子项完成”。该页的经典例子：

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

异常默认沿 Job 树向上传播（失败的子项取消父项与兄弟项）。`SupervisorJob` 为 UI 式监督提供退出选项。`coroutineScope { }` 是等到所有子项完成、且任一子项失败即失败的那个构造器。所以 Kotlin 的数据点是：即便有颜色，只要结构（Job 树、`coroutineScope {}`、协作式取消）是对的，错误率问题基本就消失了。反过来，颜色分裂仍是反复出现的烦恼（它是每个 Kotlin async 讨论串里的头号抱怨）。

**5.5 Trio 与结构化并发。** Nathaniel J. Smith，“Notes on structured concurrency, or: Go statement considered harmful”（2018-04，https://vorpus.org/blog/notes-on-structured-concurrency-or-go-statement-considered-harmful/）：并发任务应当活在 *nursery* 里，即 `async with trio.open_nursery() as nursery: nursery.start_soon(job)`，从而（1）任务生命周期在词法作用域内，无法活得比块更久，（2）nursery 在所有子任务完成前不退出，（3）取消是带作用域的协作操作（`cancel_scope`），（4）子任务的错误在父项里以 `ExceptionGroup` 重新抛出（这个设计进入了 Python 3.11 的 `TaskGroup` 与 `except*`）。Smith 更深的论点：结构化并发对控制流做的事情，正是结构化编程对跳转做的事情，它恢复了“函数的动态范围界定其效应”这一不变量，而这恰好是效应处理器已经具备的不变量。结构化并发 + 不可变数据 + 线性类型三者叠起来时规则很少：子任务只能接收复制或线性/唯一的值（没有可竞争的共享可变状态），取消是子任务能对父项施加的唯一“效应”，nursery 就是该效应的处理器。

**5.6 Rust：有颜色、有类型、且痛苦。** Rust 的 async/await 编译为无栈 future（状态机）。`Pin` 存在是因为自引用的生成器不能移动（移动会破坏正确性，只要你存储 future，这个 API 就泄漏进用户代码），而基于 poll 的模型意味着取消就是*丢弃 future*：drop 在 `await` 中途运行析构函数，这就是为什么“取消安全性”是每个组合子都有的、写进文档的坑。2024-2026 的痛点：（1）trait 中 `async fn` 返回的 future 上的 `Send` 约束无法命名，被认定为该语法糖最大的遗留缺陷（https://internals.rust-lang.org/t/specifying-bounds-on-futures-returned-by-async-functions/19740）。问题的形状：

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

用户退回到 `Pin<Box<dyn Future + Send>>` 加具体运行时的 trait。（2）运行时碎片化：tokio 是事实标准，async-std 无人维护，运行时专属 trait 撕裂生态（https://www.reddit.com/r/rust/comments/day0x0/figthting_the_async_fragmentation/）。（3）跨 `await` 的 `Send` 分析产生不透明的错误，因为它依赖哪些值在挂起点之间是活的：一个跨 `.await` 持有的值会让整个 future 变成 `!Send`，调用者从签名上看不出来。without.boats 的结论（Coroutines and effects，2024）是：有栈协程本会是更好的基础，但 Rust 无法安全地补救。Rust 是存在性证明：颜色加所有权推断（Pin/Send/Sync）*合在一起*超出了可容忍的学习预算。这一点很重要，因为 DreamScript 有线性类型，必须避免重建这套栈。

**5.7 Swift 6：通过精简版所有权推断实现严格并发。** Swift 6 默认开启完整的数据竞争检查。关键提案：SE-0414 “Region Based Isolation”（Swift 6.0，2024，https://github.com/swiftlang/swift-evolution/blob/main/proposals/0414-region-based-isolation.md）：编译器计算*隔离区域*，当区域分析证明调用者之后不再使用某个值，就可以把非 Sendable 值跨隔离边界转移。SE-0430 “`sending` Parameter and Result Values”（https://github.com/swiftlang/swift-evolution/blob/main/proposals/0430-transferring-parameters-and-results.md）把该转移显式暴露出来。SE-0461（Approachable Concurrency，Swift 6.2 时代，https://github.com/swiftlang/swift-evolution/blob/main/proposals/0461-async-function-isolation.md）把 `nonisolated(nonsending)` 设为默认，于是 nonisolated 的异步函数在调用者的 actor 上运行，消掉了一整类 Sendable 变通。SE-0430 规则的形状：

```swift
func process(input: sending Buffer) async -> Summary {
    // Buffer is not Sendable, but `sending` promises the caller will not
    // touch it while `process` runs — the compiler verifies that, and the
    // value may cross the actor boundary (move-in / copy-out of the region).
    let s = await actor.work(input)   // input is dead in the caller afterwards
    return s
}
```

对照 DreamScript 来读：Swift 正在用推断，把线性类型在语法上直接给你的那套“唯一所有权转移”推理补装上去。线性值在构造上就是 `sending`，不可变值在构造上就是 `Sendable`。Swift 其余机制（actor、全局 actor 推断、Sendable 一致性）存在的目的，就是逼近 DreamScript 从线性性 + 不可变性直接得到的东西。这是有力证据，表明 DreamScript 的原语集合攻击的是正确的问题，而*调度*侧（actor、执行器）是正交且可选的。

**5.8 计分板：哪些逃离真正赢了。** 按 2026 年判断：靠运行时实现无颜色（Go、Loom）在部署上完胜，它没有语法、没有推断、坐拥十亿级用户代码库。它未解决的那一半是结构（Go 的泄漏、Java 六轮预览的 StructuredTaskScope）以及 Go 缺失的数据竞争故事。有颜色但有结构（Kotlin）在框架生态中赢得口碑，因为 Job 树修掉了原本被归咎于颜色问题的错误率问题。能力传递（Zig 0.16）是最新的，也是对新语言最有意思的：它在不用任何类型系统特性的前提下得到调度抽象，代价是一个要穿线的参数。效应优先（Koka）在思想上赢得了争论：async、异常、迭代器是同一机制，没有 await 关键字。但它仍是研究语言，而让它好用的两个特性（行推断、悬停驱动开发）恰恰是年轻语言最难交付的两个特性。给 DreamScript 的教训在于：所有获胜设计都把颜色收进同一个地方，即一个运行时纤程、一棵 Job 树、或一个能力/行，然后在上面构建结构。颜色从不免费，它只会被*约束住*。

### 6. 没有借用检查器的隔离模型

**Pony：引用能力 + 因果消息传递。** Pony（https://www.ponylang.io，论文：Clebsch 等，“Co-Designing a Type System and a Runtime”，https://www.ponylang.io/media/papers/codesigning.pdf）为每个引用标注一个能力：`iso`（隔离、可写、唯一引用）、`val`（不可变）、`ref`（可变、actor 本地）、`box`（只读）、`tag`（仅身份）、`trn`（写唯一、可转换）。actor 通过异步消息通信。数据竞争自由随之而来，因为只有 `val`（不可变）与 `iso`（唯一）数据可以跨 actor：

```pony
class Mailer
  be send(to: Receiver tag, msg: String iso) =>
    // `iso` param: caller must give up all other references ("recover"),
    // so the message is exclusively owned by this actor upon receipt
    ...
```

因果消息传递保证“一条消息保证在任何依赖它的消息之前被处理”（Drossopoulou 等，InfoQ 演讲 https://www.infoq.com/presentations/pony-types-garbage-collection），从而支持带因果追踪的 GC（ORCA）与每个 actor 一个堆。actor“不能阻塞”：行为是协作式的，调度器做工作窃取，每核一个。阻塞式 FFI 需要小心。性能画像：良好（每 actor 一个堆，没有全局 GC 停顿），但跨 actor `iso` 转移上的 RC 流量以及不能阻塞，让一些模式变得别扭。采纳仍是小众，语言仍是 beta（0.72.x，2025）。十年的社区经验给出的学习成本结论：能力格（`iso`/`val`/`trn` 与 `recover` 表达式）是难的部分：Pony 需要六种能力，因为它没有线性类型。有线性类型的语言大致只需要两种（线性/拥有 + 共享不可变），见 Inko。

**Verona：区域，已停摆。** 微软研究院的 Verona（https://github.com/microsoft/verona）把可变数据分组到带 `snd`/`rcv` 标注的区域里，并用 `cown`（并发所有者）对象加行为做协调。该语言工作停摆了（“不准备在研究之外使用”，原设计留在 `old_version` 分支）。想法转向了 Pyrona，“Dynamic Region Ownership for Concurrency Safety”（PLDI 2025），把动态区域所有权改装到 Python 上，失败是确定性的动态失败。教训：静态的区域/收发纪律很难作为*整门语言*推销。研究价值以更小的剂量存活（Swift 的区域、线性性）。

**BEAM（Erlang/Elixir/Gleam）：隔离 + 抢占 + let-it-crash。** BEAM 的每个进程有自己的堆，独立 GC。进程间的消息是*复制*的（大二进制按引用计数共享），因此没有可竞争的共享可变状态：

```erlang
loop(Count) ->
  receive
    {incr, N} -> loop(Count + N);    % N was deep-copied into this heap
    stop      -> ok
  end.
```

调度按归约计数抢占（每个进程时间片约 4,000 次归约），无论任何进程做什么都给出软实时的延迟公平性，没有协作式 yield 的坑。错误处理是架构式的：let-it-crash，进入意外状态的进程会死掉，由监督者重启，而“错误内核”模式让可重启的核心保持小。牺牲的性能：发送时复制（小消息没问题，大消息是成本）、历史上装箱算术（OTP 24+ 的 JIT 基本修好）、没有进程内并行（靠进程扩展）。Gleam（https://gleam.run）展示了在 BEAM 上一门静态类型、只有 Result、无效应的语言长什么样：用户代码里没有异常（`panic` 用于 bug），错误即 `Result`（`pub fn parse(path: String) -> Result(Data, ReadError)`），消息传递通过带类型的进程 subject，在值跨越处做检查。对 DreamScript 相关：BEAM 的模型是已知错误率最低的并发设计，而它的成本恰恰是默不可变的线性语言能缓解的那些（消息是线性时复制不再必要，直接移动所有权）。

**Inko：现有最接近“线性消息、无 async”的语言。** Inko（https://inko-lang.org，本仓库已 vendor 在 inko/，文档见 inko/docs/source/getting-started/concurrency.md 与 design/runtime.md）使用不共享内存的轻量进程：“值在进程之间被移动”，一个值可发送“如果它是值类型（例如 String 或 Int），或是唯一值，其类型签名的语法是 `uni T`”。可变借用永远不可发送。`uni T` 就是 Pony 的 `iso` 改名，通过 `recover` 创建（`let a = recover [10, 20]  # => uni Array[Int]`）。进程定义为 `type async Counter { ... fn async mut increment(amount: Int) { @number += amount } }`，通过 `async` 方法交互，没有 async/await，没有函数颜色。async 方法发送消息。运行时（docs/source/design/runtime.md）：“进程被调度到固定大小的 OS 线程池上”（默认 = 核数），工作窃取加有界本地队列，“由编译器驱动的协作式多任务”，通过注入抢占点实现，另有超时工作线程处理定时器。错误处理（docs/source/getting-started/hello-error-handling.md）：预期错误用 `Result`，以及“在运行时无法处理”的 panic：进程 panic 会终止该进程并打印栈回溯。Inko 端到端证明了精简所有权（唯一值）+ 进程隔离 + 无颜色 async 方法可构建且可理解。它是用 Rust 实现、带精确 RC 管理的堆，即性能档次与 DreamScript 相关。注意 Inko *每个进程*是单线程的，并行 = 多个进程，进程内数据并行明确在它的模型之外。

**Mojo：有所有权加并行，async 仍缺失。** Mojo（https://www.modular.com/mojo，文档 https://docs.modular.com/mojo/）有 Rust/Swift 风格的参数约定（`owned`、`borrowed`、`inout`）以及成长中的并行故事（`parallelize`、`runtime.asyncrt` 里的 `TaskGroup`），但 async/await 经历了先 WIP 再重做：2025 年的社区反馈显示 `TaskGroup` 脆弱，并提出了“结构化 async”的重设计方案。Mojo 1.0 在 Modular 26.5 发布（2026-08）时 async 仍是路线图条目。教训：即便是资金充足、拥有所有权系统的团队，也选择先发布并行、推迟 async 语义，而语义确实难，这支持 DreamScript 采用“先把结构弄对，再管颜色”的次序。

### 7. 错误处理模型

分类交叉参考：typesanitizer，“An epic treatise on error models for systems programming”（https://typesanitizer.com/blog/errors.html），以及它引用的 Yuan 等人的发现：“几乎所有灾难性失败（92%）都源于对软件中显式标记的非致命错误的错误处理”。

**7.1 Result 即数据（Rust、Roc、Zig、Gleam）。** 错误是值。传播是显式的或轻度加糖的（`?`、`try`）。Zig 的变体是错误*集*：函数返回 `!T`（错误并集）配具名错误集（`error{FileNotFound, ...}`）、`try`/`catch` 运算符、`errdefer` 做作用域清理，错误本身是单一机器字的标签集，错误上不带载荷，上下文活在调用者已有的结构里：

```zig
fn read(path: []const u8) ReadError!Data {          // or inferred: !Data
    const f = try open(path);                        // propagate ReadError
    errdefer f.close();                              // scoped cleanup on error exit
    return f.read_all();
}
```

Rust 的变体把载荷放进错误枚举，用 `?` 传播（`fn read(path: &Path) -> io::Result<Data>`），`panic!` 留给 bug。对 DreamScript 重要的是 Roc 的变体：完全没有 Option 类型，只有 `Try` 加*推断出来的*标签并集错误类型：“如果函数返回潜在错误，它就使用 Try，其错误类型带一个描述出错原因的标签”，并且“当一个函数调用另一个函数时，可能的错误自动合并成该调用点上所有可能错误的标签并集”（https://www.roc-lang.org/faq，https://news.ycombinator.com/item?id=41934360）。也就是说：错误集是*按函数推断*的，在组合时取并集，像一个效应行，但物化在返回值里。

**7.2 异常（非受检）、受检异常与带类型的 throws。** 非受检异常（C++、OCaml、Python、JS）：签名里看不见，对审计是灾难，但在 JIT 里 happy path 零成本（表驱动栈展开）。Java 的受检异常是警示数据：typesanitizer 指出它们“在实践中不常见，因为语法开销大，且难以演进 API 去返回新的错误情形”，这*正是*后来针对效应行的同一条演进抱怨，也是为什么 Roc/Koka 式*推断*集（新增一个情形只是拓宽一个推断并集）严格优于要求标注的设计。Swift 是有趣的中间态：它保留了 `try`/`catch` 语法（比 Result 管道更轻），但从无类型的 `throws`（任意 `Error` 存在类型）转向*带类型的* throws（SE-0413，随 Swift 6.0 发布），于是 `func read(path: Path) throws(ReadError) -> Data` 现在把错误类型放进签名。这条轨迹（throw 语法 + 类型化错误签名）说明工业界正在收敛到“可能出什么问题都写在签名里”这一性质，无论机制是返回值还是控制流。

**7.3 效应即错误（Koka、Effekt）。** Koka：`exn` 是内置效应，实现为 `final ctl throw-exn : a`（lib/std/core/exn.kk），不可恢复，因此语义上是异常。std/core 里也有普通的 `result<a,e>` 类型（lib/std/core/result.kk）供数据式错误使用。签名 `() -> exn int` 表达“可能抛出”的方式与 Java 的 `throws` 完全一样，区别是它被推断出来、且按行可组合。Effekt：以错误类型参数化的 `Exception[E]`（第 2 节），`result` 是它的物化，两者在语言内可证互相定义，所以语言只需要一种，它为了工效学保留了两种。

**7.4 panic/陷入的分裂。** 新语言里的通用模式：两种*不同*语义的失败。Rust：预期错误用 `Result`，bug/不变量用 `panic!`（展开或 abort），`catch_unwind` 是狭窄的合法边界。Zig：预期错误用 `error` 并集，bug 用 `panic`，清理用 `errdefer`。Swift：预期错误用 `throws`，bug 用 `fatalError`/前置条件。Inko：预期错误用 `Result`，bug 用无法处理、终止进程的 `panic`。Go：`panic/recover` 居中（可恢复，但文化上留给 bug），外加错误值。BEAM：没有这种分裂，一切意外都按设计让进程崩溃。对 DreamScript 而言，这个分裂在解释器里近乎免费（panic = 带回溯中止纤程，Result = 数据），而且直接服务于章程里“编译期错误优于运行期错误”的偏好：bug 响亮地陷入，预期错误被类型化。

**7.5 Let-it-crash（BEAM）。** 这条模型作用于*系统*层：监督树把崩溃变成重启。它与进程/隔离（第 6 节）以及 panic/陷入分裂配对：当存在监督者时，panic 变成监督事件，重启由监督树接管。

**7.6 “Null, Errors and Enums Are One Feature”（https://www.arshad.fyi/writings/null-errors-and-enums-are-one-feature）。** 该文章（已在作者阅读清单里）论证可空性、布尔、枚举与错误都是并集：`bool = true | false`，可选是 `T | none`，枚举是“单位类型的并集”，错误是一个结构体变体。“一个既可能缺失又可能失败的值是一个平坦集合，`Data | none | Timeout`，`Result<Option<T>, E>` 把同一信息拆成两层”。处理方式是 `is`/`match`/`or` 配穷尽匹配，“没有任何东西被解包，值一直在并集里”。文章致谢的先例：Ceylon（null 作为并集成员）、TypeScript（收窄）、Zig（错误集）、Kotlin（`?:`）。对具备集合论类型（章程）的 DreamScript 来说，这几乎是原生惯用法。设计问题只在于“带一个隐式多余尾巴的平坦并集”（`or` 传播运算符）是否值得一条特殊传播规则，还是用显式的 `Result<T, E>`。

**7.7 JIT 里的零成本异常 vs 寄存器 VM 里的 Result 返回。** 在 JIT/AOT 世界里（HotSpot、JVM、.NET、Itanium-ABI C++），异常是表驱动的：不抛的路径几乎不付费，抛出付费很重（展开、找处理器），这没问题，因为抛出很少。相比之下 `Result` 式返回每次调用都要付费（一次标签检查和标签用的寄存器/槽位），但出错时便宜且可预测。在*字节码解释器*里这笔账变了：每条指令本身就是一次分派，`Result` 返回只是“把标签留在返回槽，由消费者分支”（常常与消费者操作码融合），而抛出的异常只是在你已有的解释器循环里走一遍栈帧（没有 OS 展开）。实测的经验数据与此吻合：解释器级 try/catch（Lua 的 `pcall`、JVM 解释器）的成本主要是栈帧管理，机制本身占小头。Result 检查在*寄存器* VM（DreamScript 可能的形状）里每次可失败调用付一次可预测的分支，并伤及寄存器压力。对 DreamScript 的实用综合：（1）预期错误用 Result 即数据：一次分支，没有隐藏控制流，与并集组合。（2）bug 用 panic 即陷入：一条代码路径。（3）若采纳效应，`throw` 即效应的成本与任何尾恢复操作调用相同，`finally` 式清理来自处理器或线性析构器，展开表用不上。

**7.8 评估旧 DreamScript 想法：“错误类型收集进函数的返回类型并集”。** 该想法：函数 `read(path) -> Data | NotFound | IoError`，调用者 `match` 它，调用点的并集自动增长。对照 Koka 的行，从五个轴评估：

- 形式地位：信息完全相同。`read : Path -> <exn{NotFound, IoError}> Data` 与 `read : Path -> Data | NotFound | IoError` 携带相同的集合。Effekt 的 `Exception[E]`（以错误类型为索引的效应）就是字面上的中间形式，而 Koka 的指称表（`int -> exn int` = `Z -> (Z+1)`）说明语义一致。所以这个想法是可靠的，它只是一个*表示选择*。
- 传播：并集版本通过*数据*传播（返回并集的调用者只是拓宽自己的并集，即 Roc 的自动组合）。效应版本通过*控制流*传播（完全不用改写返回路径，在生成器/回调中自然工作）。对高阶组合子而言两者都需要一个行/并集变量：`map : (A -> E B) -> List<A> -> List<B> | E` 对 `map : (A -> e B) -> List<A> -> e List<B>`，负担相同。
- 值与控制流：返回并集是值，可以被存储、发往另一个进程、缓存、之后再模式匹配。效应 `throw` 属于控制流，无法被存储（Koka 的 `raw ctl rcontext` 是那个毛糙的例外）。如果 DreamScript 想把错误跨进程传递或收集（结构化并发：nursery 把子错误*作为数据*收集，即 ExceptionGroup），并集/Result 形式完胜。
- panic 路径：无论哪种，你都想要一个独立的、无法处理的陷入来处理 bug（7.4）。效应形式的 `final ctl` 与陷入一一对应。
- 学习成本：并集版本就是“函数返回集合”（作者的集合论类型）。效应版本额外要求理解动态处理器作用域。鉴于 DreamScript 已经计划把类型当并集用，返回类型里的错误并集就是*我们已经有的受限效应行*。推荐：以并集/Result 形式为基底，等完整效应到来时，把 `throws` 糖暴露为“这个效应行恰好是你的错误并集”（Effekt 的 `result`/`value` 对展示了双向翻译）。

**7.9 “默不可变 + 线性类型”已经为并发买到了什么，还剩下什么。** 买到的：（1）语言内部共享状态上没有数据竞争，可变别名不可能存在。整个 Rust 的 `Send`/`Sync` 机制存在的目的就是逼近“没有共享可变访问”，而它收敛为“线性值可以移动（像 `sending`，SE-0430），共享值必须不可变（像 `val`/Sendable-sync）”，两种情况，都可平凡检查。（2）消息传递的可发送性免费得到：线性值就是 Inko 的 `uni T`/Pony 的 `iso`，不需要 `recover` 花活。（3）确定性并行：基于不可变输入的纯函数可以跑在任意数量的线程上（`list/interleaved` 式认证，或 Mojo 的 `parallelize`），无需锁。（4）取消/资源安全：线性资源无法被丢弃的 future 泄漏（Rust 的按 drop 取消这个坑在线性语言里是构造上的不可靠，析构函数*必须*运行，或者值必须被消费）。（5）引用计数可行（Perceus 式），且线性值的计数可在编译期删掉。剩下的部分是线性类型*不*给的东西：(a) 调度器，IO 延迟仍然需要颜色/纤程/进程。(b) 取消语义（谁能取消谁，终结器何时运行，即 nursery/结构化层）。(c) 跨任务的监督与错误传播。(d) 多次恢复的交互：多次恢复的续延可能复制或延迟线性值，因此效应设计必须要么限制恢复（Koka `linear effect`，仅尾恢复），要么禁止捕获线性值（Effekt 的词法能力）。(e) 无死锁（Pony 的因果消息传递、BEAM 的设计纪律，线性性推不出它）。

### 8. 字节码 VM 版 DreamScript 的实现要点

本节汇集与在解释器 VM 里真正实现胜出方案相关的证据，它是与运行时/IR 研究（03-*）的接口点。

**字节码 VM 里的纤程便宜且有先例。** Lua 的非对称协程是经典先例：一个协程的栈是一个 `CallInfo` 数组加上堆上的值槽。让出只在解释器层面交换栈指针，从不切换 OS 栈，最多 O(栈上帧数)，在 LuaJIT 的设计里是 O(1)。Inko 的运行时（design/runtime.md）在规模上就是这么做的：固定线程池、工作窃取、编译器注入的抢占点。对寄存器 VM 而言，纤程栈是一个可增长的栈帧数组。派生 = 分配栈帧数组 + 上下文。切换 = 在线程上下文里保存/恢复 IP、栈帧基址和“当前纤程”指针（对比 Koka 的 `kk_context_t`，它正是贯穿生成代码的上下文对象）。约 2KB 的初始分配（Go）或一页栈帧数组（Lua）是正确的起点，通过重新分配加栈帧指针修正来增长是已解决的问题。

**解释器里的抢占。** Go 需要信号，是因为编译后的循环不会查询调度器。解释器可以在分派循环里注入检查：每 N 条指令（或在向后跳转/循环头部与调用边界处）测试一个标志（Go 的抢占标志、Inko 的“抢占点”）。这比基于信号的抢占严格更简单也更安全，前提是 N 足够小以满足延迟，且该检查是一条可预测的分支。只做协作式（Rust 风格）是要避免的反面模式：一个热循环就会卡住整个池。

**证据传递在寄存器 VM 里可行。** Koka 的 ABI（第 1 节）不需要栈切换，也不需要特殊机器支持：处理器 = 保存在上下文向量里的 (marker, 操作表) 对。执行 = 设置 yielding 标记并走向量。捕获 = 把操作点与处理器之间的续延组合成堆闭包（KK_YIELD_CONT_MAX=8 个内联槽位，之后溢出）。寄存器 VM 里的对应物是：每个栈帧持有一个返回续延，执行时把操作与处理器之间的栈帧链物化为闭包（每个闭包捕获该帧的寄存器与恢复 IP）。尾恢复操作是快速路径：处理器子句带着完整的调用者续延被调用，零分配。这让“受限效应”运行时（组合 6）的成本约等于每个带效应操作一次间接调用，与虚调用无法区分。

**多次恢复与线性资源是那个坑。** 一个被使用两次的恢复值必须不携带线性资源（否则构造上就是双重释放或双重关闭）。Koka 用运行时手段处理（`finally`/`initially`，导览相应小节，文档称“正在研究用类型系统静态阻止”），并提供 `linear effect`（仅尾恢复）作为类型化的安全子集。Effekt 把恢复值限制在词法处理器作用域内，用区域界定其生命周期。Unison 什么都不追踪，依赖 GC。拥有线性类型的 DreamScript 可以做得更好：在类型检查期禁止在多次恢复的续延内捕获线性值（线性 = 至多一次使用，一次恢复就是一次使用），或者除非标注，否则把恢复值类型化为仿射（至多一次）。这是一条新颖但很小的类型规则，安全回报很大。它是 DreamScript 的线性类型让效应*比任何现有效应语言更安全*的唯一之处，值得写进设计文档。

**寄存器 VM 里的 Result 返回。** `Result<T,E>` 返回就是：把标签留在指定的返回寄存器/槽，消费者操作码（或融合的 `and_then` 式操作）按标签分支。若 E 是指针标签载荷或 NaN 装箱，标签与载荷共用一个字（arshad.fyi 指出标签可以藏在未用的位模式里：“所以 `*Node | none` 是一个机器字”）。带零尺寸单位变体的错误并集不额外付费。分支在 happy path 上被完美预测。真实成本是与异常每次调用零成本相比的额外寄存器/槽压力，但解释器的分派开销本来就压过它。panic/陷入：触发纤程中止，遍历该纤程的栈帧运行终结器（线性析构器在这里运行），在 nursery/监督者边界处浮现。

**FFI 与 pinning。** 任何纤程设计都需要一条规则来处理会阻塞或在外部线程上跑回调的外部调用：要么在专用的载体 OS 线程上运行（Loom），要么在调用期间把纤程 pin 在其线程上（Inko 记录在案的 FFI 注意事项），要么要求非阻塞的 FFI 包装。DreamScript 应当把 pinning 设为默认语义（写进文档，正如 JDK 的 JEP 491 历史展示了在 `synchronized` 上搞错的代价），并在标准库里提供载体模式。

**引用计数与线性性的协同。** 如果 DreamScript 采纳 Perceus 式 RC（Koka）或 Inko 式精确 RC，线性值的计数静态已知为 1，它们的 RC 指令在编译期被删除，其析构函数就是取消/终结器的挂钩。默不可变 + RC 还意味着共享数据上没有写屏障，以及对 fork 友好的快照（为组合 5 的确定性 `par-map` 提供写时复制并行）。Koka 的 FBIP 复用分析还展示了近似线性数据上的函数式代码如何编译成原地修改，与“超高性能”相关，且无需暴露所有权花活（章程第 6 条：偏好学习成本低而保留收益的设计）。

## c) 权衡表

表 1：async/并发模型（颜色轴 x 结构轴）：

| 模型 | 有颜色？ | 学习成本 | 运行时性能 | 取消/结构 | 错误率 | 范例 |
|---|---|---|---|---|---|---|
| 有颜色的无栈 async | 两种颜色，传染 | 高（Pin、Send、生命周期） | 最好（无栈切换） | 手动或组合子，基于 drop | 高（取消 bug） | Rust |
| 有颜色 + 结构化 | 两种颜色 | 中（一个关键字，几条规则） | 好（状态机） | Job 树，协作式 | 低 | Kotlin、Trio |
| 无颜色的有栈（VM 纤程） | 无 | 低（阻塞风格） | 在解释器里好（栈切换便宜） | 需要加 nursery 层 | 有 nursery 则低，缺 nursery 则高 | Go、Loom、Lua 协程 |
| 无颜色的能力（Io 参数） | 无（显式参数） | 低-中（穿线 Io） | 取决于实现（线程或事件式） | 显式（Queue/Group、error.Canceled） | 中（手动作用域） | Zig 0.16 |
| async 即效应 | 一条推断出的行 | 中-高（行） | 好（证据传递） | 处理器作用域内（取消作用域） | 低（带作用域、类型化） | Koka |
| 能力 + 词法处理器 | 块类型 | 中（传递能力） | 已知最好（直接风格） | 处理器作用域内 | 低 | Effekt |
| actor/隔离 | 无 | 低-中（进程模型） | 好（无锁，发送复制成本） | 监督树 | 最低（let-it-crash） | BEAM、Pony、Inko |
| 单线程 + 数据并行 | 无 | 最低 | 受限（任务只有一个核） | 不需要 | 最低 | Unison 式 IO、脚本 |

表 2：效应系统对比：

| 系统 | 类型 | 恢复 | 编译策略 | 一等续延 | async 故事 | 2026 状态 |
|---|---|---|---|---|---|---|
| Koka | 推断行、完整多态、`linear effect`、`final ctl`、具名效应 | 允许多次恢复（资源不安全，`finally` 缓解） | 证据传递到 C/JS，堆闭包 | 通过 `raw ctl` rcontext | async = 4 个效应，std/async 库（3.2.9） | 活跃，v3.2.9 |
| Effekt | 能力作参数、上下文式多态、默认二等 | 词法作用域内多次恢复 | 直接风格、多 prompt 续延、LLVM 栈 | 无（受区域约束） | 调度器作为库案例研究 | 活跃，研究级 |
| Unison | 1 个内置能力（IO）+ Exception，环境集，存在量化行 | 通过普通处理器函数 | 解释器（Haskell RTS） | 无 | IO.forkComp 内置，无类型 | 1.0（2025-11） |
| OCaml 5 | 无类型效应（无行类型） | 深（5.3 语法）+ 浅 | 原生运行时、栈切换（堆分配的 OCaml 栈） | 一等 | Eio 库，无颜色 | 已发布，仍带“实验性”标签 |
| Frank | 双向内联处理器、行推断 | 多次恢复 | 研究解释器 | 有 | 不适用 | 研究 |

表 3：错误模型：

| 模型 | 签名里可见？ | 传播成本（VM） | 能当值存储？ | 穷尽性 | 演进（新增错误） | 范例 |
|---|---|---|---|---|---|---|
| Result 即数据 | 是（显式或推断） | 每次调用 1 次分支 + 槽位 | 是 | 经由 match | 调用者并集拓宽 | Rust、Roc、Zig、Gleam |
| 返回类型里的错误并集 | 是（推断并集） | 1 次分支 + 槽位 | 是 | 经由 match | 自动（Roc） | Roc、Zig 错误集、旧 DreamScript 想法 |
| 异常即效应（`final ctl`） | 是（行） | 执行/分派，尾恢复便宜 | 否（控制流） | 经由签名里的行 | 自动（行） | Koka exn |
| 带类型的异常效应 | 是（`Exception[E]`） | 执行/分派 | 双向可物化 | 经由类型里的 E | 拓宽 E | Effekt |
| 受检异常 | 是（throws） | JIT 表 | 否 | 否（仅编译期检查） | 破坏性变更 | Java |
| 非受检异常 | 否 | JIT 表（happy path 便宜） | 否 | 否 | 静默 | C++、Python、JS |
| panic/陷入 | 否（bug） | 中止纤程 | 否 | 不适用 | 不适用 | Rust、Zig、Swift、Inko |
| let-it-crash | 否 | 进程死亡 | 否 | 不适用 | 不适用 | BEAM |

表 4：DreamScript 原语集已覆盖什么，并发还必须补什么：

| 并发问题 | 线性 + 不可变性已覆盖？ | 缺失的部分 |
|---|---|---|
| 数据竞争 | 是（没有别名可变性） | 无 |
| 跨任务/进程的可发送性 | 是（线性 = 可移动） | 无 |
| 确定性并行 | 是（不可变数据上的纯函数） | 一个 par/map-reduce 组合子 + 调度器 |
| IO 延迟（大量并发操作） | 否 | 纤程、效应、或能力传递的 Io |
| 函数颜色 | 不适用 | 选定的 async 模型（见下方组合） |
| 取消作用域 | 否 | 结构化并发（nursery/取消作用域） |
| 取消下的资源安全 | 部分（线性析构器必须运行） | 作用域退出保证（nursery 或处理器） |
| 监督/部分失败 | 否 | 监督者或 nursery 错误收集 |
| 无死锁 | 否 | 设计纪律或因果消息传递 |

## d) DreamScript 的可行设计组合

章程约束回顾：解释执行（IR/字节码）、静态线性/仿射类型、默不可变、超高性能、语法糖预算不超过 Kotlin、效应可选。作者的关键问题：能否把 async（以及效应）抽象出来，让收益保留，同时降低学习成本与错误率？下面每个组合给出机制、学习成本、性能、错误率、语法影响，以及它对效应系统的含义。

**组合 1：“结构化无颜色”，即 VM 纤程 + nursery + Result（Go/Trio/Inko 的综合）。** 机制：在 VM 里实现有栈协程（栈帧栈本来就是解释器数据，切换栈只是一次指针交换，LuaJIT/Lua 先例），调度到核数大小的线程池上，用编译器注入的抢占点（Inko 的设计，docs/source/design/runtime.md）或与 IO 集成的挂起（Go 的 netpoller）。所有 IO/阻塞 API 都写成阻塞风格。唯一的并发原语是 nursery 块内的 `spawn`，该块的作用域长于子任务，收集它们的 Result/错误并集，并在退出时取消它们。取消通过抢占点上的受检标志协作完成。学习成本：所有组合里最低，没有关键字，没有颜色，只多一种块形式。性能：对解释器极好（没有状态机装箱，没有 Pin 的对应物，栈交换是 O(1)，每个纤程的栈是类似 Loom 的堆块）。错误率：低，*前提是*nursery 强制使用（不允许裸 spawn）。这就是 Go 去掉它那唯一致命缺陷后的样子。panic 让纤程陷入并在 nursery 的错误并集里浮现（没有监督者时会带掉进程）。语法影响：`spawn` 表达式 + `nursery { }`（或 `concurrent { }`）块，约 Kotlin 级别的糖。效应：不暴露。内部上 nursery 就是 `cancel` 效应的处理器，效应若到来，运行时不变。风险：把 FFI 回调放进纤程（栈切换对 C 栈）需要 Loom 式载体技巧或 pin 住 FFI 的规则。草图（DreamScript 语法仅作示意）：

```dreamscript
fun fetch_all(urls: List[Url]) -> List[Data] | NetErr {
  nursery {
    for url in urls { spawn { fetch(url) } }   // children join at scope exit
  }                                            // default: first failure cancels all,
}                                              // surfaces as NetErr with child errors attached
```

**组合 2：“能力传递式 async”（Zig 0.16 / Effekt 的综合，不含行多态）。** 机制：一个 `Io` 风格的能力（接口值）是所有接触外部世界的代码的显式参数，由线程池、事件循环或阻塞式实现提供。`io.async { }`（或 `spawn under io`）给出挂在能力上的结构化并发原语（queue/group/cancel），颜色上什么都没有，这个能力就是数据。学习成本：低-中，一个惯用法（“把 io 传进去”），零类型系统特性（完全用普通参数/泛型）。性能：与所选实现一样好。同一份用户代码可以跑阻塞（测试、脚本）或事件式（服务器），这正是作者“抽象 async”问题所要求的性质，由参数化交付。错误率：中，手动穿线能力很吵，忘记给 `io.async` 的工作划作用域是可能的（Zig 用 defer 惯用法缓解，DreamScript 的线性类型 + nursery 组合子可以围住它）。语法影响：没有新语法（一个约定 + 标准库），恰好契合语法糖预算。效应：这就是精简效应，能力是行成员，但没有行类型，以后升级到行多态效应在语法上兼容。草图：

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

**组合 3：“Koka-lite”，async 即效应，受限行。** 机制：完整的 Koka 式行推断，但 v1 只有*最终*（不可恢复）与*线性*（尾恢复）效应，即异常即效应与 async 即效应，没有用户多次恢复处理器。事件循环是默认 async 处理器（Koka std/async 模型），取消作用域作为线性效应（lib/std/async/internal.kk 是模板）。学习成本：中-高。用户从不写行（它被推断），但库作者会碰到行变量，关于 `<async,ndet|e>` 统一的错误信息是真实成本（Koka 自己的文档依赖悬停 UI）。性能：好，只含尾恢复效应的证据传递意味着执行 = 间接调用，无捕获，ndet 追踪只在编译期。错误率：低，取消由处理器划作用域，async 与一切组合，没有 await 传染，确定性可通过 `ndet` 审计。语法影响：除了给高级用户用的 `with handler` 之外基本没有新表层语法，糖最少，但编译器里*类型推断*复杂度最高。效应：这是完整采纳（受限）。风险是过早把行推断绑到整个类型系统上（Koka 的推断建立在数十年 HM+行研究之上，DreamScript 还必须把行与线性/仿射统一结合起来，这一领域先例较少）。草图：

```dreamscript
fun wait(d: Duration) -> async () = ...          // async is a row member, inferred;
                                                 // no await keyword anywhere
fun race2(a: () -> async a, b: () -> async a) -> ndet|async a =
  with cancel_scope {                             // handler-scoped cancellation
    firstof(a, b)
  }
```

**组合 4：“actor + 线性消息”（Inko/Pony 的综合）。** 机制：带隔离堆的进程（或一个堆加所有权纪律），消息是被移动进来的值（线性/唯一或不可变复制），进程类型上有 async 方法，无颜色（无 async/await）。调度器 = 固定线程池 + 工作窃取 + 注入抢占（Inko）。可选监督：一个监督进程接收子进程 panic 通知，规模到 let-it-crash-lite 为止。学习成本：低-中，进程模型直观，但“我能发什么”的规则（线性 = 移动，共享 = 复制，可变 = 禁止）需要教学（Inko 的 `recover`/`uni` 教学展示了摩擦点）。性能：好，永不加锁，线性发送避免复制（移动无 memcpy，转移所有权），对细粒度共享内存并行算法最差（必须拆成消息）。错误率：留在模型内时最低（隔离让整类失败不可表示），进程泛滥与消息协议是新的失败模式。语法影响：`type async` 式进程类型 + `async` 方法标记（Inko），适度的糖。效应：不暴露，进程*就是*效应分派器（每次 async 方法调用就是一次消息执行）。注意它与组合 1 兼容（nursery = 受监督的进程派生）。草图：

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

**组合 5：“确定性内核 + 显式并行，无 async”。** 机制：语言完全没有并发关键字。阻塞 IO 就是阻塞（底层是线程池，跟任何脚本语言一样，即 Lua/Python 模型）。并行只通过数据并行组合子（`par-map`、`par-reduce`）作用于不可变或线性分区的数据，它们由类型系统保证确定性且无竞争。IO 密集服务器的“async”故事交给组合 2 的能力（可选库）或干脆不管。学习成本：最低。性能：对 IO 密集并发最差（每个阻塞操作一个线程，或一个库级加颜色的事件循环），对计算最好（确定性并行没有同步）。错误率：语言内最低，但需要服务器的用户会去用 FFI/外部 async（生态分裂在库层重现，颜色问题没有解决，只是被赶出去）。语法影响：无。效应：无。这是诚实的极小内核。问题在于作者的目标工作负载是计算型（那它就赢）还是 IO 型（那它就输）。草图：

```dreamscript
// deterministic data parallelism; closures capture immutables or linear partitions
let squared = xs.par_map(fn(x) { x * x })     // race-free by construction
let total    = xs.par_reduce(0, fn(a, b) { a + b })
```

**组合 6：“效应就绪的能力 + nursery，完整效应推迟”（推荐的综合）。** 机制：构建组合 1 的运行时（VM 纤程 + nursery + 取消作用域）与组合 2 的表层（所有接触外部世界的标准库都接收一个显式能力参数，例如 `io`），v1 不暴露效应类型，也不暴露处理器。内部为运行时已经需要的两个效应（nursery 边界上的 `cancel`、strand 拆除用的 `discontinue`）实现 Koka 的证据传递 ABI，从而语义从第一天起就是处理器形状。之后用户定义的可恢复效应可以作为库级处理器加进来，无需重构架构，而 async 保持抽象：同一份程序可以跑阻塞（单纤程）、事件式（事件循环 Io）或线程式（池 Io），因为调度由能力承载。学习成本：低（v1），效应逃生口在被使用前不付费。性能：组合 1 的性能。错误率：组合 1 的错误率，取消在内部已经是处理器作用域。语法影响：nursery 块 + 能力约定，糖远低于 Kotlin。这个组合直接回答作者的问题：async 与效应的*收益*（可组合取消、调度抽象、审计接触外部世界的代码）通过结构 + 能力 + 效应形状的运行时被保留，而*成本*（行推断、处理器二重性、颜色词汇）被推迟或消除。分层表层的草图：

```dreamscript
// v1 surface: nursery + io capability. Two built-in "capabilities", zero syntax.
nursery {
  spawn { io.print_line("concurrent") }
}                                    // exit joins + cancels; errors union out

// v2 escape hatch (only if wanted): user-defined handlers for non-resumable
// (final) or tail-resumptive (linear) effects, Koka-style — additive.
with handler { ctl ask() resume(42) }
```

**给定线性类型 + 默不可变 + Result，代数效应收得回成本吗？** 精确回答分三部分。（1）作为*面向用户的类型系统*（Koka/Effekt 式的行，或每个签名里的能力）：v1 不收。章程的语法糖预算与可解释性排除了行多态推断。线性类型已经交付效应会带来的安全（通过析构器的资源安全、通过 nursery 作用域的取消安全），Result 已经交付类型化错误（7.8 节说明返回并集与效应行是同一信息）。最强的工业尝试（OCaml 5）四年后仍是实验性，而实践中击败颜色问题的语言（Go、Loom、Zig）都*没有*效应系统。（2）作为*内部执行模型*：收，但低调。nursery/取消/strand 拆除都是处理器，在 VM 里以 Koka 式（证据向量、尾恢复、final 操作）实现它们很便宜，保留了未来的选项。而且若选组合 2 或 6，能力参数*就是*效应行成员，因此以后做成完整效应语言只需增量变更。（3）作为*未来的库特性*，用于用户定义的控制抽象（迭代器、解析器、UI、概率编程）：确实有价值，组合 6 的推迟选项保留了它。若作者以后想要，从 `final ctl` + `linear effect`（仅不可恢复 + 仅尾恢复）起步，这个子集避开了多次恢复/线性资源的坑，而 Koka 的 `finally` 变通与 Effekt 的区域限制正是为了围住这个坑。

**交给设计阶段的开放问题。**（1）nursery 的形状：块形式（`nursery { spawn { ... } }`）、尾随 lambda 构造器，还是 `with` 语句绑定（章程已经欣赏 Koka 式 `with`）。Koka 的 `with handler` 与 Kotlin 的 `coroutineScope {}` 是两个极。（2）nursery 退出时的错误收集语义：合并所有子错误（ExceptionGroup）、首个错误取消其余（Koka 的 `firstof`），还是两者都要、按 nursery 选择。（3）`io` 能力参数是显式的（Zig）还是隐式可推断的（一个单成员效应行）。推断版正是组合 6 悄悄变成组合 3 的地方。（4）纤程下的 panic 语义：中止纤程（Inko）还是中止进程（Rust 默认）。监督故事决定。（5）第 8 节提出的线性恢复类型规则（禁止在多次恢复的续延里捕获线性值）在考虑任何效应表层语法前需要一点形式化工作。（6）FFI pinning 策略。每一条都有 2-3 个取自上述范例的具体选项，已可提取进决策目录（`docs/zh/design/`）。

一手来源：Koka https://github.com/koka-lang/koka（lib/std/async/、lib/std/core/hnd.kk、kklib/include/kklib.h、doc/spec/tour.kk.md、doc/spec/news.mdk）与 https://koka-lang.github.io/koka/doc/book.html。Xie & Leijen ICFP'21 https://www.microsoft.com/en-us/research/publication/generalized-evidence-passing-for-effect-handlers-or-efficient-compilation-of-effect-handlers-to-c/。Leijen MSR-TR-2017-21 https://www.microsoft.com/en-us/research/wp-content/uploads/2017/05/asynceffects-msr-tr-2017-21.pdf。Effekt https://github.com/effekt-lang/effekt（libraries/llvm/rts.ll、libraries/common/result.effekt、examples/casestudies/scheduler.effekt.md、machine IR）与 https://effekt-lang.org/publications。Unison https://github.com/unisonweb/unison（docs/ability-typechecking.markdown、parser-typechecker/src/Unison/Builtin.hs）与 https://www.unison-lang.org/docs/。OCaml https://ocaml.org/manual/5.4/ 与 https://ocaml.org/changelog/2025-01-08-ocaml-530。Zig https://ziglang.org/download/0.15.1/release-notes.html 与 https://ziglang.org/devlog/2025/。Go 抢占 https://go.googlesource.com/proposal/+/master/design/24543-non-cooperative-preemption.md。Java https://openjdk.org/jeps/444, /505, /506, /491。Kotlin https://kotlinlang.org/docs/coroutine-context-and-dispatchers.html。Trio https://vorpus.org/blog/notes-on-structured-concurrency-or-go-statement-considered-harmful/。without.boats https://without.boats/blog/coroutines-and-effects/。Swift SE-0413/0414/0430/0461 https://github.com/swiftlang/swift-evolution/blob/main/proposals/。Pony https://www.ponylang.io/media/papers/codesigning.pdf。Verona https://github.com/microsoft/verona。Inko vendor 的文档（inko/docs/source/）与 https://inko-lang.org。Mojo https://docs.modular.com/mojo/。错误模型 https://typesanitizer.com/blog/errors.html。Roc https://www.roc-lang.org/faq。并集文章 https://www.arshad.fyi/writings/null-errors-and-enums-are-one-feature。
