# DreamScript 设计 02 — 线性性、所有权与可变性

本决策目录把 docs/zh/research/01-linearity-ownership.md（主要证据）以及 docs/zh/research/03-runtime-ir-jit.md 中关于内存、逃逸分析与 TCO 的约束（§a 第 6-9 条、§d）转化为八个可选决策点。按章程要求，目录索引位于 docs/zh/design/00-decision-map.md。证据纪律：每一条先例主张都按文件与章节引用研究笔记（`research/01 §B.x` 表示深入分析小节，`research/03 §a.N` 表示概要第 N 条，`research/02 §b.N` 表示深入分析第 N 条），没有出处的主张不写。

共享前提（章程 AGENTS.md，此处不再论证）：在带类型化寄存器的字节码虚拟机上解释执行，可选分层（research/03 §d 流水线 A-B）。绑定默不可变（章程硬性要求 5）。没有 C++ 风格的特化语义（章程设计哲学）。语法糖上限为 Kotlin 的全部语法糖面（章程硬性要求 4）。集合论并集与交集类型属于 docs/zh/design/01 负责的未决选项。代数效应与 async 由 docs/zh/design/03 负责，这里只引用，不做决定。编译期错误优先于运行期错误（章程设计哲学），下文多个选项正是以此为准绳权衡。**所有代码仅作示意，最终表层语法由 docs/zh/design/04-syntax.md 决定**：文中采用中性的 TS 家族风格（`const`/`var`、`fn name(a: T) -> R`、花括号），让讨论的落点是语义，写法只作载体。示例写于 docs/zh/design/04 的表层决定之前，只说明语义。每个决策点恰好带一行加粗的 **推荐**，落在该点应当选择的选项上。其余选项以一行 **结论** 收尾。

## 决策点索引

| 决策点 | 问题 | 选项数 | 推荐 |
|---|---|---|---|
| C1 | 语言默认采用哪种线性性风格？ | 4 | (b) 仿射核心 + 可选线性层 |
| C2 | 用户如何在不复制的前提下共享值（借用）？ | 4 | (d) 二等参数模式 |
| C3 | “默不可变”背后的机制是什么？ | 4 | (b) val/var + 排他窗口 |
| C4 | 底层运行哪种内存管理策略？ | 4 | (a) Perceus RC + 复用，由 (d) 的 arena 落实 |
| C5 | 丢弃与析构由谁书写，何时运行？ | 4 | (c) 资源层上的线性 RAII 析构 |
| D1 | 可变性被限制在哪里？ | 4 | (b) 排他窗口式修改 |
| D2 | 集合模型是什么？ | 4 | (b) 线性可变写时复制：函数式 API，原地实现 |
| D3 | 迭代器与生成器采用什么方案？ | 4 | (d) 组合子 + 隐藏生成器 |

## C1 — 线性性风格

章程硬性要求 2 的原文是“静态线性（或仿射）类型，在编译期强制检查”。硬性要求 6 补充“优先选择学习成本低、出错率低且保留性能收益的设计”。研究的共识是：用户能写下的每一条线性事实都可以推导为唯一性事实，而“为了易用性，推导优于书写。为了保证，书写优于推导”（research/01 §a、§B.12、research/01 §d Combo 6）。C1 决定由谁来写下它。

### C1.a — 默认全量线性性（处处 QTT）

```
fn dup(1 x: a) -> (a, a)            // multiplicity 1 on the binder: x used exactly once
fn map(f: fn(a) -> b, xs: List<a>) -> List<b>   // every arrow carries a multiplicity
drop : (1 x: a) -> ()               // fine; using a 1-binder twice is rejected (research/01 §B.6)
```

先例：Idris 2 的 QTT 绑定携带 0/1/omega 数量，并保证 0 数量的内容被擦除（research/01 §B.6，ECOOP 2021）。Granule 的带梯度模态把数量推广为半环刻度，写作 `dup : a [2] -> (a, a)`（research/01 §B.6）。Linear Haskell 的 `%1 ->` 与多重性多态箭头 `%m`，多态库会把 UR 约束泄漏进普通签名，`linear-base` 的存在就是为了吸收这份代价（research/01 §B.6，GHC proposal 0111）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 调研中最高：每个绑定都带一个数量，多重性多态是其中特别糟糕的部分（research/01 §B.6） |
| 运行性能 | 静态保证最强（擦除证明、RC 省略），但 Idris 2、Granule、GHC 都没有展示内存收益，三者全部依赖 GC（research/01 §B.6，§c 表 1 “n/a (GC'd)”） |
| 表达力 | 在协议层面很强（asset、会话类型、擦除），作为日常默认则很痛苦（research/01 §B.6） |
| 编译器复杂度 | 非常高：多重性合一与所有推断交织。research/01 §c 表 3 把 QTT 的签名噪声评为“very high” |
| 排除 | C2 退化为 Austral 风格的借用语句（research/01 §B.5）。C3 的排他窗口需要 ATS 风格的线性视图（research/01 §B.6）。在 consume-once 下 D2 的持久化集合变难。仅签名一项就突破语法糖上限（章程硬性要求 4） |

**结论 — 否。** 数量作为内部检查词汇是对的，作为表层语法是错的。UR 约束带来的痛苦（research/01 §B.6）正是章程硬性要求 6 中低学习成本条款所禁止的。

### C1.b — 仿射核心 + 可选线性资源层

```
struct File: ~Copyable { fd: RawFd }    // ~Copyable subtracts the default Copyable constraint
const g = f                             // f is affine: assignment moves; using f after: compile error
fn read_all(f: File, cap: Cap) -> Bytes // f consumed exactly once; the checker enforces it
```

先例：Swift SE-0390 的不可复制类型，“不可复制类型的值始终拥有唯一所有权，永远不能被复制”，`deinit` 在唯一实例的生命周期结束时运行（research/01 §B.3）。Hylo 的 `Copyable: Movable` trait，把可复制性作为默认可加的正向约束，再用取反相减（research/01 §B.3）。Inko 的静态移动错误 `error(moved): 'cats' can't be used as it has been moved`（research/01 §B.8）。Austral 代表 consume-once 谱系的严格一端（research/01 §B.5）。Rust 去掉借用后的仿射移动（research/01 §B.4）。Entente Cordiale 的结论：为了保证，书写优于推导（research/01 §a、§B.12）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 低到中等：一条日常规则（“赋值即移动，移动后的绑定已死”），加上一个面向库作者的概念（`~Copyable`）。普通不可变值在观测上永远不会触发移动规则（research/01 §B.8 关于值类型的说明） |
| 运行性能 | 编译期完整强制。移动就是寄存器复制，没有 dup/drop（research/01 §B.12 第 1 条、research/03 §a.7） |
| 表达力 | 日常代码不受影响。资源与协议拿到静态的零检查保证，正是推断无法填补的空白（research/01 §B.2） |
| 编译器复杂度 | 中等：仿射检查就是最后使用分析。Hylo 以过程内抽象解释器的形式运行它（research/01 §B.3），Austral 的状态表版本是一趟前向扫描（research/01 §B.5） |
| 排除 | 排除 C1.a（没有多重性语法）。使 C2 成为必须（只有移动而没有借用，仍需回答读取访问怎么办，C2.d 是自然的答案）。与 C4.a 搭配得当，因为仿射的一次性使用语义让大部分 dup/drop 可在静态消除（research/03 §b.6）。也与 C5.c 搭配，因为析构钩子恰好落在 `~Copyable` 层（research/01 §B.3）。研究命名的 C1(b)+C4(a) 自然搭配即指此（research/01 §d Combos 3+4，research/03 §a.8） |

**推荐 — (b)。** 这是在最低学习成本下对章程硬性要求 2（“线性（或仿射）”）的诚实解读。`~Copyable` 作为取反的集合论约束，可以原生地同 docs/zh/design/01 的并集组合。research/01 §a 称它为“DreamScript 口味上最好的先例”。

### C1.c — 隐藏线性性（纯推断，没有所有权表层）

```
const xs2 = map(sqr, xs)          // no annotation anywhere
const b = append(xs, x)           // may overwrite xs's buffer if unique, else copies:
                                  // a performance note, never an error (research/01 §d)
```

先例：Koka 自 v2.2.0 起在 Perceus 之上做自动借用推断（research/01 §B.2）。Roc 的 ARC 插入，模块头注释写着“borrow inference plus RC statement emission”，实现为两阶段过程间 `arc_solve` 加 born-unique 分析，而这个语言的 FAQ 明确声明它“no linear, dependent, refinement, or uniqueness types”（research/01 §B.1）。Lobster 的所有权分析消除了约 95% 的运行期 RC 操作，只留下三种罕见的编译错误（research/01 §B.9）。2024-2026 的趋势陈述（research/01 §a）。

直白地说，本选项与章程之间存在真实张力：硬性要求 2 写的是“静态线性（或仿射）类型，在编译期强制检查”，而本选项完全移除了用户可见的线性性，按字面理解它违背了该要求，只能通过改写该要求来解决。这里的“线性”仍可能指三件事。一是 IR 层的不变式，Hylo 把驻留寄存器的对象视为线性资源，任何操作数使用都会消耗它（research/01 §B.3），于是逃逸分析成为编译期事实，标量替换无需代价（research/03 §a.7）。二是推导出的唯一性，每一条线性事实都能还原为 refcount==1 的事实（research/01 §a、§B.12），保证留给编译器，易用性留给用户。三是可选的线性层仍可留在底下，这也是 Koka 的立场：内存收益并不需要线性性，但静态的零检查资源保证需要它（research/01 §B.2）。采用 (c) 意味着改写硬性要求 2，这属于作者的权限，不在本目录的决定范围内。

| 维度 | 评估 |
|---|---|
| 学习成本 | 最低：零关键字，最多一个 `mut`（research/01 §c 表 3） |
| 运行性能 | 函数式负载下接近 C（research/01 §B.1、§B.2）。在推断还不存在时，仅靠运行期唯一性检查就能达到 Koka 的快路径（research/01 §d Combo 1） |
| 表达力 | 面向值的代码表达力高。若不留可选的线性层，资源与协议失去静态保证 |
| 编译器复杂度 | 低学习成本选项中最高：过程间模式求解、复用配对、TRMC。Roc 的缺陷跟踪记录显示这套分析是承重结构，而且调试过程完全公开（issue 11830、10920、12436、research/01 §B.1） |
| 排除 | C2.b 成为借用的答案（模式会变得冗余）。只与本隐藏风格保持一致。没有线性层时，C5.c 的确定性析构找不到用户可见的落点。按字面与章程硬性要求 2 冲突 |

**结论 — v1 语义不采用，作为最终优化目标采用。** 把 (c) 的机制放在 (b) 的表层之下，正是 research/01 §d Combo 6 的做法：先做运行期唯一性检查（语义），再做推断（纯优化），推断落地时语言零改动。这就是 Koka 与 Roc 都验证过的交付顺序。

### C1.d — 所有者核心 + 运行期检查借用 + 可选静态线性标注

```
let r = ref xs                    // borrow; NOT lifetime-checked
xs = nil                          // move/drop while r alive => deterministic panic:
                                  // "can't drop a value of type 'List' as it still has 1 reference(s)"
```

先例：Inko 的完整模型，可见的移动、显式的 `ref`/`mut` 借用且不做生命周期检查、每个对象带借用计数并在丢弃时检查，一旦违规就产生带堆栈的确定性 panic，来源是 “Ownership You Can Count On”（2006）（research/01 §B.8）。Austral 的线性标注，加上为可选层准备的块作用域语句 `borrow x as y in { ... }`（research/01 §B.5）。research/01 §d Combos 2+4。zest 把计数放在栈上，每次安全检查只是一次整数比较（research/01 §B.11）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 很低。research/01 §d Combo 2 的说法是“Inko 用一页手册就讲完了整套系统” |
| 运行性能 | 好：每次借用更新一次计数，之后可优化（Inko 以未优化状态发布，并计划省略计数，research/01 §B.8）。线性层零开销（research/01 §d Combo 4） |
| 表达力 | 无借用检查器的选项中最高：链表和图都很自然（research/01 §B.8） |
| 编译器复杂度 | 很低。但所有权错误变成运行期 panic，违背章程“编译期错误优先于运行期错误”的设计哲学 |
| 排除 | C2.d（模式与 `ref`/`mut` 重复）。C5 的保证叙事（析构可能 panic，无法确定运行）。除非复刻 Inko 计划中的渐进式静态升级，正确性上限停在运行期检查的别名（research/01 §B.8）。对模型的别名那一半，与章程硬性要求 2 的“在编译期强制检查”冲突 |

**结论 — 不作为最终风格，作为 v0 底座采用。** 它的借用计数正是 research/01 §d Combo 6 在 (b)+(d) 之下、静态检查器成熟之前指定的诊断层：便宜、确定，并且前向兼容（计数字段同时充当 refcount==1 检查，research/01 §d Combo 2）。

## C2 — 借用

选定了仿射移动（C1.b）之后，值仍然需要在不转移所有权的前提下被读取。research/01 §B.4 的检查清单是设计约束，without.boats 的观点是：移动、借用、生命周期、丢弃是四个半耦合的特性，难点在于耦合本身。因此要选成本最低的耦合方式。

### C2.a — 没有用户借用：只有移动与显式复制

```
fn len(s: Str) -> Int { ... }          // consumes s
const n = len(clone(s1))               // every read of a kept value is an explicit clone
fn swap(xs: List<Int>) -> List<Int>    // consume-and-return chaining replaces "mutation"
```

先例：Move 的资源类型配合显式 `copy`/`move`，是不引入借用的工业设计里最完整的一套（research/01 §B.11）。Rust 的 `clone()` 文化，位于 research/01 §B.4 变通清单的第一项（“erase sharing”）。Inko 的值类型在移动时复制，让小的值绕开别名机制（research/01 §B.8）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 规则最简单，实践最重：每个读取点都变成一次转移决策 |
| 运行性能 | 差：把复制当作唯一的共享路径，等于没有借用的 Rust，也就是 research/01 §B.4 记录的 clone() 变通文化 |
| 表达力 | 差：读取大值必然复制。原地算法退化为消费并返回的链式调用 |
| 编译器复杂度 | 所有 C2 选项中最低 |
| 排除 | 完全排除 C2.d 与 C3.b/D1.b（任何用户可见的借用都不存在）。把 D2 推向纯持久化或链式 API。对读取密集的代码放弃性能承诺（硬性要求 3） |

**结论 — 否。** research/01 §B.4 表明，无借用系统里的每一种变通最终都收敛到“动态检查或动态计数”，用复制来换取模式的缺席，既买不到简单也买不到速度。

### C2.b — 推断借用（编译器内部，用户不可见）

```
fn head(xs: List<a>) -> a              // user never writes a mode; compiler passes xs borrowed
const h = head(xs)                     // xs still usable: the borrow was inferred; if the
                                       // compiler must copy instead: a perf note, not an error
```

先例：Koka v2.2.0 对用户函数自动借用，被借用的局部变量不产生 incref/decref（research/01 §B.2，`src/Backend/C/Parc.hs`）。Roc 的 `arc_solve` 两阶段过程间签名求解，配一个调试用的 `arc_certifier` 复核输出（research/01 §B.1）。Lobster 基于 AST 节点的所有权分析配合事后所有权，在数十个游戏原型上启用它只需要“minor changes in only 2 locations”（research/01 §B.9）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 最低：零表层（research/01 §c 表 2，Koka/Roc/Lobster 行） |
| 运行性能 | 接近 C：约 95% 的 RC 操作被省略（Lobster），Koka 取得接近 C 的结果（research/01 §B.9、§B.2） |
| 表达力 | 面向值的代码表达力高，但推断失败时用户无法声明共享约定。Roc 的 issue 12420 显示编译器的选择在两个方向上都会产生可观测的性能影响（research/01 §B.1） |
| 编译器复杂度 | 这里最高：对签名做过程间不动点求解。Frame Limited Reuse 的存在正是为了让分析局部化并支持分离编译（research/01 §B.2） |
| 排除 | C2.d（冗余）。只与 C1.c 保持一致。推迟可交付的 v1，因为 Koka 与 Roc 都先交付运行期唯一性检查，再交付推断（research/01 §d Combo 6）。借用行为没有用户可见的概念可以挂靠，解释成本压迫章程的可解释性预算 |

**结论 — v1 不采用。** 这是后续增长时语言零改动的优化层（research/01 §d 横切说明 5：所有权决定“以关于复制的提示性说明出现，永不表现为错误”）。从这里起步意味着在第一个可工作的解释器之前，先交付最难的那趟编译器处理。

### C2.c — 运行期检查借用计数

```
let r = ref xs                         // borrow increments xs's borrow count
drop(xs)                               // deterministic panic while r is alive:
                                       // "can't drop value of type 'Array' as it still has 1 reference(s)"
```

先例：Inko 在丢弃时检查借用计数，其确定性论据是“同一程序、同样输入跑 10 次会产生 10 次相同错误”，并附带借用错误很少见的经验判断（research/01 §B.8）。Swift 的动态 `-enforce-exclusivity=checked` 是成熟语言用运行期检查兜住静态规则的对应物（research/01 §B.3）、research/01 §B.12 第 3 条与 research/03 §a.8 的“Inko 风格借用计数用于确定性诊断”。

| 维度 | 评估 |
|---|---|
| 学习成本 | 低：只有一个概念（`ref` 借用），错误表现为带堆栈的 panic |
| 运行性能 | 每次借用一次计数操作。多数可在之后优化（Inko 已声明的计划，research/01 §B.8） |
| 表达力 | 无检查器的选项中最高：图、链表、迭代器都很自然（research/01 §B.8） |
| 编译器复杂度 | 最低：完全不需要静态别名分析 |
| 排除 | 作为唯一机制时（也就是配合 C1.d），正确性停在运行期，对别名缺陷永久违背编译期错误优先的哲学。作为 C2.d 之下的底座则什么都不排除（二等借用静态上无法悬垂，计数只会在 FFI 或不安全代码的缺陷上触发） |

**结论 — 不作为面向用户的模型采用，作为 (d) 之下的 v0 诊断底座采用。** research/01 §d Combo 6 的分期正是这样：静态方案还没成熟时，借用计数先跑起来充当“便宜的保险”，之后可以在语义不变的情况下移除。

### C2.d — 静态二等借用作为参数传递模式

```
fn len(s: Str) -> Int                  // default mode: immutable borrow (Hylo `let`, Swift `borrowing`)
fn push(xs: mut List<Int>, x: Int)     // exclusive mutable borrow (Hylo `inout`)
fn consume(f: File) -> Int             // consuming move (Hylo `sink`, Swift `consuming`)
xs.push(1)                             // UFCS: the mode lives on the declaration; call sites never annotate
```

先例：Hylo 的 `let`/`inout`/`sink`/`set` 模式，借用是 IR 层投影，表层语言里没有引用（research/01 §B.3，LocalBindings.md `fun add(let Int, let Int) -> sink Int`）。Swift SE-0377 的 `borrowing`/`consuming`，通过的修订版要求显式 `copy` 运算符，让每次复制都可见（research/01 §B.3）、research/01 §d Combo 3。OxCaml 2025 的模式，被记为“模式优于生命周期”在工业界的再次出现（research/01 §B.11）。

与 UFCS 和默认不可变性的交互：模式在定义处按参数固定下来，这是把“反对多重性多态”那一课应用到模式上的结果（research/01 §B.6，横切说明 2）。因此 UFCS 调用链全程只写值与方法名，调用点不出现模式语法。默认不可变性则由绑定允许的模式来强制：`const` 绑定的值只能传给借用模式的参数，`var` 绑定的所有者值可以进入 `mut` 窗口，而 Koka 风格的 UFCS（`xs.push(1)`）在各种模式下写法完全一致。带 `let`/`inout`/`set` 访问器的下标把同一套模式扩展到元素访问，取代 getter/setter 与迭代器协议（research/01 §d Combo 3，另有 Hylo 在同一个下标上提供四种访问器，research/01 §B.3）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 低到中等：默认之上多两个关键字，多一个概念，即借用以参数模式写在声明处。research/01 §c 表 3 的表述是“只在声明处写模式” |
| 运行性能 | 很好：没有别名就不需要运行期检查，逃逸分析也变得简单（research/01 §d Combo 3） |
| 表达力 | 只限二等：没有 `Option<&T>`，不能存储借用。外部风格的迭代通过下标或游标完成（research/01 §d Combo 3 记录了这些断裂处） |
| 编译器复杂度 | 中等且局部：借用无法逃出调用，检查在过程内完成（Hylo 的 DI 抽象解释器已经算出所需状态，research/01 §B.3） |
| 排除 | 排除 C2.a/b/c 作为唯一模型。永久排除一等借用，这是按 without.boats 的耦合清单有意为之（research/01 §B.4）。与 C3.b、D1.b 搭配（`mut` 模式本身就是排他窗口，一套机制三处使用），也与 D3.a/d 搭配（下标与游标作为迭代底座）。已知的麻烦处：`Result<~Copyable>` 载荷需要 `Movable` 要求或自动借用（research/01 §d Combo 3） |

**推荐 — (d)。** 二等模式是调研中唯一具备一页纸规则集、无需生命周期标注、又直接适配默认不可变 UFCS 表层的借用设计（research/01 §a、§d Combo 3、research/01 §c 表 1 把 Hylo 行评为低学习成本、高性能）。

## C3 — 默认不可变的机制

章程硬性要求 5 的原文是“绑定默认不可变。可变性是需要显式选择且局部可见的”。这里的问题是：一旦选择了可变性，它允许触碰什么。研究记录了一条有意为之的非特性：Hylo 与 Swift 都不允许修改不可变绑定，research/01 §B.3 建议维持这一点，理由是“重新绑定的不可变性带来别名悖论，换来的语法糖却很少”。

### C3.a — val/var 划分、var 仅限函数局部、没有共享可变状态

```
const total = fold(xs, 0, add)         // immutable everywhere, including fields and captures
var acc = 0                            // rebindable local; dies at scope end
acc = acc + x                          // no var fields, no var captures, no inout anywhere
```

先例：Koka 的 `var` 局部变量，其余表层保持纯函数式（research/01 §B.2）。research/02 §b.7 的结论是线性加不可变让共享可变访问不可能存在，整个 Send/Sync 装置随之坍缩。前述“可变的 let”非特性（research/01 §B.3）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 最低：可变性只存在于变量被书写的地方 |
| 运行性能 | 配合 FBIP 复用表现良好（Koka 把函数式更新编译为原地操作），但跨函数的原地算法（sort_in_place）必须变成消费并返回的链式调用或 builder 模式 |
| 表达力 | 没有可变字段，没有被调用方修改。大状态代码（游戏主循环、UI 状态）要通过 builder 或持久化更新重新绑定 |
| 编译器复杂度 | 最低：除了局部重新绑定，不需要排他性跟踪 |
| 排除 | 排除 C3.b/D1.b（没有排他窗口）。D2 的原地方法只剩 builder。即使 (b) 的窗口更简单，算法代码也被推向 D2.c 风格的 builder |

**结论 — 否。** 相比 (b) 它省下两个关键字，却禁止了值语义真正需要的唯一一种可变形态，也就是被调用方的排他修改，而 Hylo、Swift、Inko 三者都提供这种形态（research/01 §B.3、§B.8）。

### C3.b — val/var + 排他窗口

```
var xs = List.of(3, 1, 2)              // owned, rebindable local
sort(xs)                               // `mut List<Int>` parameter: one exclusive mutable window
                                       // for the call, statically guaranteed; shared-immutable again after
```

先例：Hylo 的 `inout` 绑定与参数，修改通过 `inout` 窗口进行，而 `inout` 绑定“在 IR 层始终表示为一个借用访问”（research/01 §B.3）。Swift 的 `mutating` 方法，Swift 甚至在 debug 构建里额外做动态排他性检查，一个成熟系统仍然用运行期检查兜住静态规则（research/01 §B.3）、research/01 §d Combo 3：“借用就是裸指针，模式传递的值不需要引用计数”。

| 维度 | 评估 |
|---|---|
| 学习成本 | 低：一个想法，“一次排他可变访问，静态保证”。窗口在词法上就是那次调用 |
| 运行性能 | 修改场景最好：窗口内没有运行期检查。栈上或 arena 上的局部变量可用，因为借用无法逃逸（research/01 §d Combo 3） |
| 表达力 | 完整的原地算法、修改方法、下标 setter（research/01 §B.3） |
| 编译器复杂度 | 中等且局部：排他性在过程内判定，与 C2.d 相同的一套机制，一个检查器两处使用 |
| 排除 | 排除 C3.c（内部可变性变得不必要）。D1.a/c/d。需要 C2.d 的模式作为窗口机制。共享可变被永久排除，这是有意为之 |

**推荐 — (b)。** C2.d 的同一个 `mut` 模式就是窗口，因此可变性保持“显式且局部可见”（章程硬性要求 5），不需要额外机制。静态保证的排他性让编译器可以跳过窗口内的所有运行期检查（research/01 §d Combo 3）。

### C3.c — val/var + 带类型的内部可变性（Cell/RefCell 类）

```
const c: Cell<Int> = Cell(0)                     // copyable single-threaded interior mutability
const r: RefCell<Vec<Int>> = RefCell.empty()     // runtime-borrow-checked interior mutability
c.set(c.get() + 1)                               // legal behind an immutable binding
```

先例：Rust 的 `Cell`/`RefCell`，research/01 §B.4 说它们“把排他性检查移到运行期，正是 Inko 与 Roc 的运行期唯一性检查在全局所做的取舍”。Dada 的 `atomic` 块作为共享可变的事务式变体（research/01 §B.10）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 中等：两个新类型，外加“何时用哪个”的决策 |
| 运行性能 | RefCell 每次访问都付出运行期借用检查。Cell 便宜，但会悄悄破坏基于不可变性的推理（相等性与哈希上的意外） |
| 表达力 | 不可变绑定背后的共享可变，这是窗口唯一做不到的技巧 |
| 编译器复杂度 | 低，但它动摇两个承重不变式：不可变数据上的无屏障 RC（research/03 §a.8），以及不可变值上省略 Vale 风格的代检查（research/01 §B.7） |
| 排除 | 排除 C3.b（多余的逃生口削弱窗口纪律）。削弱其他推荐所依赖的默认不可变不变式。D1.b 变得可选，可变性叙事被拆成两半 |

**结论 — 否。** 有了 C2.d/C3.b 的窗口，内部可变性所解决的问题在这个语言里并不存在。最多在标准库里保留一个隐藏的 `UnsafeCell` 等价物，用于实现持久化结构，不必做成面向用户的类型家族。

### C3.d — 不可变核心 + 线性破坏性更新（FBIP）

```
const xs2 = map(sqr, xs)              // functional surface, no var anywhere...
                                      // ...overwrites xs's cells in place when rc==1
const m2 = insert(m, k, v)            // "new" map; same buffer when m is uniquely owned
```

先例：Koka 的 FBIP 及其原话保证，“reuse optimization is guaranteed and a programmer can see when the optimization applies... allows us to express many imperative algorithms in a purely functional style”（research/01 §B.2）。Perceus 的 fan-in/fan-out 复用与 Frame Limited Reuse（research/01 §B.2）。Roc 的 COW 内建契约与 `box_reuse`/`loop_append_promote` 处理（research/01 §B.1）、research/03 §a.8。

| 维度 | 评估 |
|---|---|
| 学习成本 | 零新语法：纪律是“写函数式代码，它就地运行”，复用作为有文档的保证可见（research/01 §B.2） |
| 运行性能 | 函数式负载下接近 C（FBIP + TRMC，research/01 §B.2） |
| 表达力 | 作为完整表层（完全没有 `var`）：对算法代码表达力差，消费并返回的链式写法正是 FBIP/TRMC 论文要修掉的东西。作为 (b) 之下的运行期保证：纯增量 |
| 编译器复杂度 | 复用配对与 TRMC 是真实存在的处理阶段，且有顺序约束（research/03 §b.6，ParcReuse.hs），无论选哪条路都要在 C4.a 里预算 |
| 排除 | 作为运行策略时什么都不排除（这正是 C4.a 交付的东西）。作为整个表层则排除 C3.b/D1.b 以及局部累加的易用性 |

**结论 — 表层采用 (b)，运行期保证以 (d) 叠在下面。** C4.a 的复用处理让 (d) 的行为对所有函数式代码自动成立，(b) 的窗口则让显式的原地算法仍然可写。这正是 Koka 自己交付的组合（`var` 局部变量 + FBIP 复用，research/01 §B.2）。

## C4 — 内存策略

research/03 §a 第 6-9 条给出的约束：带类型的拆箱寄存器意味着不用 NaN-boxing，也不需要标签检查（第 6 条）。线性性让逃逸分析成为编译期事实，Inko 需要全程序逃逸分析才提升 47-58% 的分配，而线性语言可以无偿拿到 100% 的非移动临时值（第 7 条）。在寄存器虚拟机上保证 TCO 的成本很低，传递所有权的尾调用不做任何引用计数更新（第 9 条、§b.5）。research/03 §a.8 给出的组合正是本节推荐的方案。每个选项按暂停、分配器流量、解释器契合度、循环处理四个方面评估。

### C4.a — Perceus 精确非原子 RC + 块复用

```
// surface: const xs2 = map(sqr, xs)         — no memory syntax at all
//   rc_dec(tmp2)                            // tmp2 dies after field extraction (research/03 §b.6)
//   reuse = (rc(tmp2) == 1) ? tmp2 : NULL   // dying uniquely => candidate
//   x = alloc_at(reuse, TAG_PAIR)           // overwrite in place; no allocator call
```

先例：Perceus，精确 RC、静态插入 dup/drop、复用分析，PLDI 2021（research/01 §B.2）。kklib 里的运行期钩子 `kk_block_alloc_at` / `kk_block_check_drop_reuse`（research/03 §b.6）。Roc 在同一套契约上实现 ARC（research/01 §B.1）、research/01 §B.12 第 2 条、research/03 §c 表 5（“best: linearity minimizes RC ops”）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 零表层语法。复用自动生效，并作为可观测行为写入文档（research/01 §B.2） |
| 暂停 | 无。释放立即发生且对缓存友好。终结器在最后使用处确定运行（research/01 §B.12 第 2 条） |
| 分配器流量 | 最低：复用让快路径上的分配近乎免费，仿射或线性的一次性使用语义把 dup/drop 收敛到显式的复制与移动边界（research/03 §b.6） |
| 解释器契合度 | 很好：放置退化为“每次复制一次 rc_inc，每次唯一死亡一次 rc_dec”，因为线性检查器已经给出使用次数（research/03 §b.6）。在每隔离域单线程的模型里非原子计数是安全的（research/01 §d Combo 1） |
| 循环 | 不回收：归纳数据类型无法成环（research/01 §B.2）。Lobster 退出时的环报告是诊断范式（research/01 §B.9）。其余情况由弱引用或 arena 逃生口覆盖 |
| 编译器复杂度 | 在线性性之下中等但规模小：“没有借用分析，没有干扰分析”（research/03 §b.6） |
| 排除 | 排除 C4.c（搬移式 GC 与精确 RC、内部指针、FFI 冲突，research/01 §B.12）。需要已知的丢弃点，即 C5.b 的 DI 基础设施。共享数据的并发必须改成按进程隔离或消息传递，不用原子操作（research/01 §d Combo 1），与 docs/zh/design/03 相关 |

**推荐 — (a)，并用 (d) 的 arena 快路径落实。** research/03 §a.8 给出的正是这个搭配（“Perceus-style precise reference counting with block reuse... Combine with per-frame bump arenas for interpreter temporaries”），而 C1.b 的仿射值正是让这趟处理退化为平凡的原因（research/03 §b.6），也就是 C1(b)+C4(a) 这个自然搭配。

### C4.b — RC + 带代引用

```
const r: Gen<List<Int>> = gen(&xs)    // 8-16 byte fat reference: (ptr, rememberedGeneration)
use(r)                                // unproven derefs pay a generation __check; stale access
                                      // => deterministic trap; destroy(xs) invalidates every r
```

先例：Vale 的带代引用，16 字节引用或 8 字节表变体，随机代带来 2^-64 的误判安全概率，`__check` 通过受保护页实现，BenchmarkRL 地形生成器下开销 +10.84%，朴素 RC 为 +25.29%（research/01 §B.7）、research/01 §B.12 第 5 条以及引用布局对比表（§B.7）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 零表层，而且永远不会出现移动错误，这是它的独特卖点（research/01 §B.7） |
| 暂停 | 无 |
| 分配器流量 | 完全没有计数，但每个堆引用增加 8 字节，未证明的解引用要付一次比较。默认不可变会省掉大部分检查，因为不可变值永远不会在别名背后失效（research/01 §B.7） |
| 解释器契合度 | 好：检查只是一次整数比较（zest 的设计要点，research/01 §B.11）。但 16 字节引用不适合拆箱的类型化寄存器帧模型（research/03 §a.6、§c 表 4） |
| 循环 | 可以安全创建（没有计数纠缠）。除非加入弱引用或环报告，否则会泄漏（research/01 §d Combo 5） |
| 编译器复杂度 | 分析负担低。代的维护落在分配器与析构器里 |
| 排除 | 机制上不排除 C4.a 的任何东西，但它放弃了编译期保证这条叙事：失效只在访问时被捕获，C1.b 的静态仿射纪律和 C2.d 的排他性不再承重。每引用 +8 字节的开销与 research/03 §a.6 的拆箱寄存器收益相抵 |

**结论 — 不作为主力。** 如果仿射的摩擦比预期更重，它是对的退路（“no move errors ever”，research/01 §B.7），但它把引用大小翻倍，并把静态保证转成动态保证。

### C4.c — 追踪式 GC（Immix / 搬移式）

```
const xs = List.of(1, 2, 3)           // no ownership, no RC; GC reclaims
const g = Node(x, nil)                // cycles fine
```

先例：Inko 的 Immix 时期及其复盘，“the implementation was quite complex and difficult to debug; Immix suffers from fragmentation, and without a GC (what it's designed for) it's hard to clean up the fragmentation; our implementation was unlikely to outperform highly optimised allocators such as jemalloc”（research/01 §B.8、research/03 §b.6）。Immix PLDI 2008（research/01 §B.12）。research/01 §c 表 1 的追踪式 GC 行。

| 维度 | 评估 |
|---|---|
| 学习成本 | 最低：完全没有所有权概念。对共享的环形数据表达力最高 |
| 暂停 | 短但真实存在。搬移式变体与精确 RC、内部指针、FFI 冲突（research/01 §B.12） |
| 分配器流量 | 最高：指针写入要有写屏障，还有复制与记忆集（research/03 §b.6） |
| 解释器契合度 | 对小团队而言差到中等：安全点、精确栈映射、每一层都要记账屏障（research/03 §a.8）。Inko 是一位很强的独立实现者，仍彻底放弃了自研 GC（research/01 §B.8） |
| 循环 | 由构造方式自然处理，这是唯一的真实优势 |
| 编译器复杂度 | 复杂度落在运行时而不在编译器，但这份运行时工作是持续性的，且很难调试（research/01 §B.8） |
| 排除 | 浪费 C1 的线性性收益（编译期逃逸分析，research/03 §a.7）与 C3 的不可变性收益（无需屏障，research/03 §a.8）。与 C4.a 无法一致共存。FFI 与热重载的路径变难（旧代码上仍在运行的栈帧，research/03 §b.9） |

**结论 — 否。** Inko 的复盘是关于小团队真实成本最有力的现有证据（research/01 §B.8），而线性性加不可变性恰好移除了 GC 最擅长的负载类型，即共享环形可变数据与分配密集的修改。

### C4.d — 以每帧 bump arena 为主 + 在逃逸边界做 RC

```
fn hot(xs: List<Int>) -> Int { score(build_tree(xs)) }  // build_tree bumps the frame arena; its
                                                        // nodes provably do not escape: freed on pop
```

先例：research/03 §a.7 指出“线性性让逃逸分析成为编译期事实，而不再是启发式”，Inko 的 47-58% 全程序逃逸分析是启发式基线。research/03 §a.8 认为每帧 bump arena“safe precisely because linearity forbids silent escapes”。Roc 的平台分配模型（CLI 平台跳过释放，web 平台按请求分配 arena，research/01 §B.1）。Hylo 的无存储 `let` 绑定（research/01 §B.3）。Koka 的 FLR 栈帧槽位复用是编译式对应物（research/01 §B.2）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 零表层。只需文档化一条性能规则（“不逃逸的临时值免费”） |
| 暂停 | 无（栈帧弹出时整体重置） |
| 分配器流量 | 临时值的最低可能：分配是一次加法，释放是一次恢复（research/03 §a.8） |
| 解释器契合度 | 对不逃逸的值完美，而线性性按使用点证明其不逃逸（research/03 §a.7）。长寿命或共享数据仍然需要 RC 层，因此它与 (a) 组合使用 |
| 循环 | 只出现在逃逸后的 RC 数据中，答案与 (a) 相同 |
| 编译器复杂度 | 低：arena 分配器本身很简单。逃逸证明来自 C1 检查器，不需要新的分析 |
| 排除 | 单独使用则完全无法承载持久化或共享结构。与 (a) 组合则什么都不排除，两者合起来就是 research/03 §a.8 推荐的组合 |

**推荐 — (a) + (d) 联合。** 对逃逸或装箱的值用带复用的 RC，对帧内临时值用 arena，不引入追踪式 GC，这就是 research/03 §a.8 的原话推荐，由 C1.b 使其成立。索引里把 C4 记为“(a) + (d)”。

## C5 — 移动、丢弃与确定初始化

仿射值需要一个定义明确的死亡点。research/01 §B.12 第 3 条把要求浓缩为：由字节码编译器在作用域结束时发射丢弃，在活跃性确定的地方按最后使用处丢弃，终结器在丢弃点确定运行。research/02 §b.8 补充说，线性析构器正是在纤程中止或取消时运行的钩子，也就是 docs/zh/design/03 将来需要的清理叙事。

### C5.a — 隐式丢弃、除 RC 复用外没有用户钩子

```
const xs2 = make_list()               // compiler inserts the drop at last use / scope end
                                      // no deinit anywhere; File/Socket cannot exist as safe types
```

先例：Koka 与 Roc 不为普通值暴露用户可写的丢弃（research/01 §B.1、§B.2）。Perceus 的延迟丢弃摊薄深层释放的成本（research/01 §B.12 第 2 条）。Swift 的 ARC 同样没有环收集器，靠约定处理环（research/01 §B.11）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 最低 |
| 运行性能 | 普通数据最好：具体类型的丢弃可内联，没有虚分派 |
| 表达力 | 无法定义资源类型（File、Socket、Lock），因为没有钩子来关闭它们 |
| 编译器复杂度 | 最低 |
| 排除 | 完全排除 C5.c，也连带排除 C1.b 的资源层（没有析构的 `~Copyable` File 不可用，SE-0390 的析构正是全部要点，research/01 §B.3）。design/03 的取消清理叙事随之落空（research/02 §b.8：线性析构器在纤程中止时运行） |

**结论 — 否。** 即便是两个以推断为先的语言，也为资源保留了析构。research/01 §B.12 第 3 条把确定性终结器列为必需清单项。

### C5.b — 线性值的显式丢弃/弃用 + 确定初始化检查

```
const buf = alloc_buffer(1024)        // linear here: consume exactly once
drop(buf)                             // required, exactly once; double-drop is a compile error
fn f(cond: Bool) -> Int {
  let x: Int                          // uninitialized; DI tracks Raw/Uninit/Init per path
  if cond { x = 1 } else { x = 2 }
  x                                   // ok: initialized on every path
}
```

先例：Austral 的 consume-once 纪律，配上对所有变量状态做抽象解释的解释器，以及让检查器保持单趟前向扫描的循环深度规则（research/01 §B.5）。Hylo 的确定初始化抽象解释器，状态为 Raw/Uninitialized/Initialized/Partial，它同时还会插入 `deinit` 指令（research/01 §B.3）。Move 的显式 `copy`/`move`（research/01 §B.11）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 中到高：丢弃纪律塑造所有资源代码。对线性值的随手闭包变得不可能（research/01 §B.5） |
| 运行性能 | 很好：只做检查，零运行期成本 |
| 表达力 | 完全显式控制。但提前返回与 panic 路径上的手工丢弃，正是 RAII 存在要消灭的那类缺陷（Vale 为此造了更高层 RAII，research/01 §B.7） |
| 编译器复杂度 | 中等且已被充分理解：一趟前向扫描（Austral）加 DI 解释器（Hylo），两者都是本目录本来就要的基础设施 |
| 排除 | 对线性值排除 C5.a。C5.c 的易用性（每次关闭都要手工写）。其余不排除，因为 DI 在每个选项下都是必需基础设施 |

**结论 — 不作为通用规则，作为基础设施采用。** 采用 DI 检查器（每个选项都需要它，research/01 §B.3），并在 (c) 之上保留显式 `drop` 作为调试与 FFI 工具。要求用户写下每一次丢弃，会为通用脚本语言重新引入 Austral 的“consume-once 纪律塑造所有代码”成本（research/01 §B.5）。

### C5.c — 由线性性保证的 RAII 风格析构钩子

```
struct File: ~Copyable { fd: RawFd    // deinit is only expressible on the linear layer
  deinit { close(self.fd) } }         // runs exactly once, at the unique instance's death
fn with_file(path: Str, body: fn(mut File) -> T) -> T {
  const f = open(path)                // linearity: f cannot be copied or silently dropped
  body(f)                             // deinit runs at f's unique death — including on panic paths
}
```

先例：Swift SE-0390，不可复制的 struct/enum“can also have `deinit` declarations... which run automatically at the end of the unique instance's lifetime”，且不需要堆分配（research/01 §B.3）。Vale 的更高层 RAII，“ownership in the type system guarantees a future call happens; destructors can take arguments and return values”，由此实现结构化并发与可重放的调试（research/01 §B.7）。Austral 的线性方法（close/commit/cancel 协议，research/01 §B.5）。research/02 §b.8 指出，线性析构器是纤程中止时的终结器钩子。

| 维度 | 评估 |
|---|---|
| 学习成本 | 低：只有一个概念，且只有资源作者需要。资源使用者什么都不用写 |
| 运行性能 | 在最后使用处确定丢弃，终结器可内联（research/01 §B.12 第 3 条） |
| 表达力 | 完整的资源叙事：文件、锁、会话、FFI 句柄、RAII 协议 |
| 编译器复杂度 | 需要 C5.b 的 DI 与最后使用分析，这些基础设施本来就要求。析构的分派是静态调用，因为具体类型已知 |
| 排除 | 排除 C5.a。需要 C1.b 的线性层来界定哪些类型可以声明析构（可复制类型没有唯一死亡点，这正是 SE-0390 的规则，research/01 §B.3）。与 D3.b 交互：多次恢复的续体不能捕获 `~Copyable` 值（research/02 §b.8 提到的坑） |

**推荐 — (c)。** 把析构限制在 `~Copyable` 线性层上，就获得 RAII 而无需 Rust 那种到处 Drop 的复杂度，还无偿提供 design/03 的取消清理（research/02 §b.8），并与 research/01 §d Combo 6 的值优先分期一致。

### C5.d — 只在效应处理器或终结器里提供丢弃钩子（推迟）

```
handle {
  use(r)                              // cleanup delivered by the enclosing handler scope
} with res <File> { ... }             // resource release = handler logic, not a destructor
```

先例：Koka 交付 `finally`/`initially` 组合子，正是因为多次恢复会让“RAII 依赖单次恢复”这个前提不成立（research/02 §b.1）。research/02 §a 指出：“multi-shot resumption + linear resources interact badly — which is precisely why Koka has `linear effect` (tail-resumption only) and why Effekt restricts to lexical handlers”。Effekt 用词法处理器界定资源的生命周期（research/02 §b.2）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 先推迟：v0 完全没有资源叙事 |
| 运行性能 | 每个资源边界都要捕获处理器入口的栈帧（research/03 §b.12 风险 4） |
| 表达力 | 资源绑定到动态作用域。没有一等资源 |
| 编译器复杂度 | 在 design/03 的效应决定落地之前为零，之后则是整套效应机制 |
| 排除 | 排除 C5.c。让 C1.b 的资源类型在效应存在之前不可用。把内存设计与效应设计耦合起来，这正是章程“解耦优先”哲学所拒绝的耦合（章程设计哲学，Rust 的对应情况见 research/01 §B.4） |

**结论 — v1 不采用。** 只有当 docs/zh/design/03 采用完整的多次恢复效应时才重新考虑。注意 Koka 同时交付析构式资源与处理器组合子（research/02 §b.1），所以 (c) 并不妨碍日后加入 (d)。

## D1 — 可变性的限制范围

### D1.a — 仅函数局部 var

```
var acc = 0                           // the only mutable binding kind
for x in xs { acc += x }              // no callee-side mutation of any kind
const out = normalize(acc)            // everything else consume-and-return
```

先例：Koka 的 `var` 局部变量（research/01 §B.2）、research/02 §b.7 关于线性加不可变带来“没有共享可变状态”的结论。

| 维度 | 评估 |
|---|---|
| 学习成本 | 最低 |
| 运行性能 | 通过 FBIP 表现良好，但原地算法需要重新绑定链或 builder |
| 表达力 | 没有修改方法，没有下标 setter，Hylo/Swift 的 `inout`/`mutating` 惯用法消失了 |
| 编译器复杂度 | 最低 |
| 排除 | 排除 D1.b。使 C2.d 的 `mut` 模式无法使用（窗口属于被调用方修改）。把 D2 推向 builder 密集的 API |

**结论 — 否。** 它牺牲真实的表达力，却没有比 (b) 多买到任何安全性：窗口在静态上排他且词法可见，章程硬性要求 5 的“局部可见的可变性”已经成立。

### D1.b — 排他窗口式修改

```
var acc = Account.zero()
deposit(acc, 100)                     // deposit takes `mut Account`: the only mutation channel;
                                      // one exclusive mutable borrow for the call, statically proven
const snapshot = acc                  // outside windows, everything is shared-immutable
```

先例：Hylo 的 `inout`（research/01 §B.3）。Swift 的 `mutating`，把动态排他性作为安全网（research/01 §B.3）、research/01 §d Combo 3。

| 维度 | 评估 |
|---|---|
| 学习成本 | 低：修改总是调用点上的一次具名窗口 |
| 运行性能 | 最好：无别名窗口不需要运行期检查（research/01 §d Combo 3） |
| 表达力 | 完整的原地算法、修改方法、下标 setter |
| 编译器复杂度 | 中等。与 C2.d 完全相同的机制，一个检查器三处使用（参数、窗口、限制范围） |
| 排除 | D1.a/c/d。永久排除共享可变，这是有意为之。需要 C2.d |

**推荐 — (b)。** 一套机制在构造上就被限定在所有者接收者之上。窗口就是那次调用，因此修改在局部可见，不需要额外纪律去维持。

### D1.c — 能力门控的修改

```
const cap = bank.mut_capability()     // capability is a first-class (linear?) value
transfer(cap, from, to)               // mutating ops require a `mut Cap` in scope;
                                      // signatures audit who may mutate what
```

先例：Koka 的许可式原子效应（`net`、`fsys`、`ui`、`blocking`）在效应行里充当受检的能力名（research/02 §b.1）。Effekt 的能力传递（research/02 §b.2）。research/02 §d Combo 2 的“能力传递式 async”作为 Zig/Effekt 的综合方案。效应与能力式 IO 的决定本身归 docs/zh/design/03 负责，这里只引用，不做决定。

| 维度 | 评估 |
|---|---|
| 学习成本 | 中等：能力穿透各处，读起来很吵（research/02 §d Combo 2 的说法是“pass the io”） |
| 运行性能 | 除普通参数传递外没有额外成本 |
| 表达力 | 可审计的修改，但能力管的是谁可以修改，不管哪个别名排他。底层仍然需要排他性 |
| 编译器复杂度 | 局部很低，但把本目录与 design/03 的效应决定耦合在一起 |
| 排除 | 与 design/03 的独立性（若那边否决能力式 IO，本选项失去底座）。无论如何 D1.b 仍然必需，因此 (c) 只能作为附加层，永远不能成为答案 |

**结论 — 本目录不采用。** 排他性 (b) 与能力门控 (c) 解决的是不同问题。如果 docs/zh/design/03 采用能力式 IO，修改能力日后可以叠在 (b) 之上而不改变语义，也就是 research/02 §d Combo 2 描述的渐进路径。

### D1.d — 只通过返回新值的线性析构实现修改（纯表层）

```
const s2 = stack_push(s, x)           // "mutation" = consume-and-return
const (top, s3) = stack_pop(s2)       // pure surface; compiler reuses in place
```

先例：Koka 的 FBIP（research/01 §B.2）。Clean 的唯一性类型，让纯函数在纯表层背后就地更新未被共享的数组（research/01 §B.6）。research/03 §b.9 指出同一个运行时可以同时暴露两种模式。

| 维度 | 评估 |
|---|---|
| 学习成本 | 零语法，但链条很长：算法代码读起来像数据流管线 |
| 运行性能 | 通过复用分析与 (b) 相当 |
| 表达力 | 对原地算法表达力差。这份别扭正是 FBIP 与 TRMC 研究要解决的对象，它们改用循环与 modulo cons 递归来处理（research/01 §B.2） |
| 编译器复杂度 | 需要复用分析（research/03 §b.6） |
| 排除 | 完全排除 D1.b 与 C3.b。下标式的原地元素访问（C2.d 的 setter 访问器，research/01 §B.3）变得不可能 |

**结论 — 不作为唯一通道。** 它是函数式表层的正确运行期行为（由 C4.a 交付），但算法需要 (b) 的窗口。Koka 自己就让 `var` 局部变量与 FBIP 并存（research/01 §B.2）。

## D2 — 集合

research/03 §b.9 给出了框架：同一个运行时可以同时暴露两者，即 Array（唯一、可增长、零复制操作）与共享或装箱的持久化结构。成本的不对称在于：持久化更新要分配 O(log n) 个带 RC 的节点，仿射的原地更新分配为零。

### D2.a — 仅持久化与结构共享（HAMT 等）

```
const m2 = insert(m, k, v)            // HAMT: O(log n) new nodes; m untouched
const xs2 = xs.push(x)                // persistent vector; free snapshots everywhere
```

先例：research/03 §b.9 指出“不可变默认让结构共享可以安全进行（带 RC 的持久化 vector/map）”。带 RC 的持久化节点的值表示基础见 research/01 §B.12 第 1 条。

| 维度 | 评估 |
|---|---|
| 学习成本 | 低：一个心智模型，一切都是值 |
| 运行性能 | 读取没问题。每次更新都分配 O(log n) 个带 RC 的节点，正是 research/03 §b.9 标记的那份不对称。对 FFI 与热循环没有零复制缓冲区 |
| 表达力 | 快照与撤销免费。原地缓冲不可能 |
| 编译器复杂度 | 低（属于标准库机制，不在编译器），但标量替换对不透明的堆节点无能为力（research/03 §a.7） |
| 排除 | D2.b 的原地 API。FFI 级别的 Vec/Bytes（D2.d）。数据密集型脚本上的编译语言性能承诺（硬性要求 3） |

**结论 — 不作为唯一叙事。** 持久化 map/set 属于标准库，是同一 RC 运行时上的普通库类型（research/03 §b.9），不必变成强制的集合模型。

### D2.b — 仅线性可变：函数式 API，原地实现

```
const xs2 = append(xs, x)             // "new" list; overwrites xs's buffer iff uniquely owned
const m2 = insert(m, k, v)            // map update: in-place when m is unique, else copies
sort(xs)                              // in-place via the C3.b window (mut mode)
```

先例：Koka 的 FBIP 链表及其保证复用的原话（research/01 §B.2）。Roc 的内建所有权契约，即术语表里的 Borrow/Consume/Copy-on-Write/Seamless Slice（research/01 §B.1）、research/03 §b.9。

| 维度 | 评估 |
|---|---|
| 学习成本 | 低：一条规则，“更新即写时复制，有别名就复制”，以性能提示出现，永不表现为错误（research/01 §d 横切说明 4） |
| 运行性能 | 唯一时接近 C。共享时为 O(n) 复制，Roc 的 issue 12420 显示这个反面是可观测且真实的（research/01 §B.1） |
| 表达力 | 函数式与命令式两种风格都能直接写。持久化快照作为库里的 HAMT 类型提供。拆箱缓冲区落在唯一性这一角 |
| 编译器复杂度 | 需要 C4.a 的复用配对来兑现承诺，这项预算已经列好 |
| 排除 | 排除仅 D2.a 的方案。其余不排除，持久化类型就是普通库值，D2.d 的缓冲区不过是同一模型里的拆箱成员 |

**推荐 — (b)。** 按唯一性写时复制是唯一一种同时让“默认不可变”（章程硬性要求 5）与“性能接近编译语言”（章程硬性要求 3）都成立的语义，而且它正是 Koka 与 Roc 已交付的模型（research/01 §B.1、§B.2）。

### D2.c — 双模式：默认持久化 + 热路径上的线性 builder 类型

```
const m2 = insert(m, k, v)                      // persistent by default (HAMT)
const b = Builder<List<Int>>.init()             // explicit linear builder for hot paths
const xs = b.freeze()                           // after b.push(1); b.push(2): consume, produce value
```

先例：research/03 §b.9（同一个运行时上两种模式）。Vale 的 HGM 哲学，只在热路径上有选择地采用线性风格（research/01 §B.7）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 中等：两套集合心智模型，加上何时切换的决策 |
| 运行性能 | 用 builder 的地方好。持久化的默认让常见情况背上 O(log n) 的节点翻动成本 |
| 表达力 | 高，但翻倍 |
| 编译器复杂度 | 每种集合维护两套实现，违背语法糖与正交性预算（章程硬性要求 4、章程设计哲学） |
| 排除 | 排除 D2.b 作为默认。标准库表层翻倍。切换点本身变成教学话题。research/01 §B.7 自己也提醒，在多种内存风格之间来回权衡是真实成本（“pushing checks to zero Rust-style just shifts costs into cloning”） |

**结论 — 否。** (b) 本身已经兼得两者：写时复制在唯一时给出复用，同时保留持久化语义，额外的持久化默认只会让 API 翻倍而收益甚微。

### D2.d — 务实混合：小值持久化，大缓冲区线性可变

```
const tags = Set.of("a", "b")                     // small: copied freely, no aliasing machinery
const buf: mut Bytes = Bytes.with_capacity(4096)  // big: unique, unboxed, zero-copy ops
buf.write_u32_le(n)
```

先例：Inko 的值类型，Int/Float/String/FFI struct 在移动时复制，于是小的值完全绕开别名机制（research/01 §B.8）。Lobster 的内联不可变 struct 降低了所有权压力（research/01 §B.9）。Inko 0.20.0 单次分配 16 字节字符串的经济性（research/03 §b.4）。research/03 §b.9 的唯一可增长 Array。

| 维度 | 评估 |
|---|---|
| 学习成本 | 低，但要多教一条规则：小与大的界线在哪里，我的更新为什么复制了 |
| 运行性能 | FFI、互操作与缓冲区的表现最好：拆箱的唯一缓冲区能搭上类型化寄存器帧模型（research/03 §a.6、§a.7） |
| 表达力 | 务实。但这种分类会渗进所有 API 设计 |
| 编译器复杂度 | 拆箱的唯一缓冲区需要专门的类型化操作码，按 research/03 §b.1 的类型化操作码计划成本很低 |
| 排除 | 作为分类规则，它与 (b) 统一的写时复制竞争。作为标准库的一层，它补充 (b)，因为 `Bytes`/`Vec` 就是同一模型里的拆箱线性类型，也正是 research/03 §b.9 的建议 |

**结论 — 不作为分类规则，作为标准库的结果采用。** 在 (b) 之下，小的不可变值本来就按值语义复制，大缓冲区本来就是拆箱的唯一类型，混合是 (b) 之下的结果，不需要另设决策。

## D3 — 迭代器与生成器

语言需要 `for` 循环、组合子链，以及让库作者定义自定义迭代的手段。research/03 §a.10 与 §b.7 提供了关键数字：MoonBit 在零成本 `Iter` 类型上做编译器级融合，比原生 JS 迭代器助手快 25 倍。research/02 §b.8 给出了任何生成器设计都必须满足的约束：多次恢复不得捕获线性资源。

### D3.a — 线性游标（持有借用的外部迭代器，移动即消费）

```
const it = xs.cursor()                // it: Cursor<Int> — owns a borrow of xs (second-class)
const v = it.next()                   // consuming: it is dead after next (move = consume)
for v in xs { ... }                   // bounds checks eliminated by the cursor discipline (research/03 §b.7)
```

先例：Rust 的外部迭代器模型作为仿射祖先（research/01 §B.4 的仿射移动规则支配游标）。“移动即消费”是 Inko 的移动错误规则（research/01 §B.8）。research/03 §b.7 指出“在线性游标纪律（例如对切片做 for）已经证明范围包含的地方消除边界检查”。Hylo 之所以用下标回答外部迭代，正是因为二等借用无法被存储（research/01 §d Combo 3）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 用户侧低。作者侧多一个协议 |
| 运行性能 | 很好：易于融合，并且靠线性性消除边界检查（research/03 §b.7） |
| 表达力 | 游标是值，其借用属于二等（C2.d）：针对被可变借用的结构的游标不能存进另一个结构里 |
| 编译器复杂度 | 中等：游标类型、脱糖、融合处理 |
| 排除 | 排除 D3.b（两套迭代叙事）。需要 C2.d（游标持有借用）。作为公开协议，它重复了 `for` 加组合子本可隐藏的东西 |

**结论 — 可行，定位是内部降级。** 在 (d) 内部采用它，并为底层代码提供一个可选的游标类型。

### D3.b — 通过 VM 纤程提供一等生成器

```
gen fn read_records(f: File) -> Record {         // generator = a fiber in the VM
  while true { yield parse_next(f) }             // yield suspends the fiber
}
for r in read_records(file) { ... }              // for drives the fiber
```

先例：research/02 §b.8 指出堆分配的栈式协程在字节码虚拟机上成本很低（Lua/LuaJIT 的先例）。这里有一个坑，原话是：“multi-shot resumption = replay closures (must not hold linear resources — type system must forbid capturing linear values in multi-shot continuations, or restrict effects to tail-resumption à la linear effect)”（research/02 §b.8、§a）。Koka 的 `linear effect` 是带类型的尾恢复安全子集（research/02 §b.1）。async 与纤程的设计本身归 docs/zh/design/03 负责，这里只引用，不做决定。

| 维度 | 评估 |
|---|---|
| 学习成本 | 表层低。线性捕获规则是多出来要教的一件事 |
| 运行性能 | 每个元素一次纤程切换，对比一个融合循环。MoonBit 比 JS 迭代器快 25 倍的数字正是在衡量热路径上这笔开销（research/03 §a.10） |
| 表达力 | 最高：树遍历、无限流、自定义迭代都很简单 |
| 编译器复杂度 | 需要 VM 纤程支持与线性捕获的类型规则，并与 design/03 的 async 机制耦合。若 design/03 选择纤程，一套纤程机制可以同时服务两边 |
| 排除 | 排除 D3.c/d（组合子退化为生成器之上的语法糖，保证融合也随之失去）。采用它必须先有 design/03 的纤程决定。若日后采用，research/02 §b.8 的规则是强制的：多次恢复禁止捕获线性值，或者除非显式标注，否则把恢复类型设为仿射 |

**结论 — v1 不采用。** 它很强，但会把本目录耦合到 docs/zh/design/03，并为每个元素付出挂起成本。后续仍有加入的余地，因为 (d) 的隐藏降级能在不改动表层的情况下升级为纤程。

### D3.c — 只用组合子与融合（完全没有迭代器协议）

```
xs.map(sqr).filter(is_odd).sum()      // one fused loop; no intermediate collections
xs.map(f).zip(ys).collect()           // compiler-level fusion; custom iteration: manual loops only
```

先例：MoonBit 的编译器级迭代器融合与 25 倍数字（research/03 §a.10、§b.7、§13）。Roc 的编译器做相邻工作（`trmc`、`loop_append_promote`、`tag_case_fusion`），而该语言没有用户迭代协议（research/01 §B.1）。Gleam 作为 BEAM 上静态类型的组合子风格语言（research/02 §b.6）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 最低：TS/Python 的 map/filter 习惯可以直接迁移 |
| 运行性能 | 可融合的链上最好（research/03 §a.10） |
| 表达力 | 不可融合的形态（不规则的多集合遍历、有状态的流水线）退化为手写循环。用户自定义迭代需要某种协议，而本选项没有提供 |
| 编译器复杂度 | 在类型化 SSA 上做融合处理，规则方向性类似 Cranelift（research/03 §b.7） |
| 排除 | 排除 D3.b。库定义的迭代只能停在手写循环或下标上。想长成 (d) 必须先决定隐藏协议放在哪里 |

**结论 — 接近，但 (d) 明显更稳。** 表层相同，且为不可融合的形态留了定义明确的退路。

### D3.d — 组合子 + 隐藏生成器（编译器把链融合成循环，用户只见组合子）

```
xs.map(sqr).filter(is_odd).sum()      // fused into one loop (typed-SSA rule, research/03 §b.7)
for (k, v) in entries(m) { ... }      // for desugars to a hidden cursor — never user-visible
fn primes(upto: Int) -> List<Int>     // library authors: combinators + subscripts (C2.d), no gen syntax
```

先例：MoonBit 的零成本 `Iter` 加编译器融合，比 JS 迭代器助手快 25 倍（research/03 §a.10、§b.7）。Koka 的 TRMC 把函数式递归变成原地循环，是把同一套哲学应用到递归上（research/01 §B.2）。CPython 的静态宏操作组合，是把链降级为单一单元的 uop 层对应物（research/03 §b.1）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 表层最低。一个隐藏概念（游标），只有标准库作者会遇到 |
| 运行性能 | 融合快路径。融合失败的地方由隐藏游标的退路保持语义一致 |
| 表达力 | 组合子加 `for` 覆盖几乎全部迭代。自定义迭代通过 C2.d 的下标或可选的线性游标（D3.a）完成。面向用户的生成器日后可以作为库里的语法糖加入，不破坏表层 |
| 编译器复杂度 | 融合规则加一次隐藏降级，中等。隐藏层可以先做成普通游标，等 design/03 落地纤程后再升级 |
| 排除 | v1 里排除 D3.b 作为公开语法（日后可逆）。其余不排除，它把 (c) 纳入自身，并给 (a) 一个作为内部协议的位置 |

**推荐 — (d)。** 表层是组合子，隐藏协议是线性游标，优化是融合。design/03 的纤程决定若落地，可以在不触碰用户代码的情况下升级隐藏层。这与 research/01 §d Combo 6 应用到所有权上的分期是同一套“隐藏机制，稳定表层”。

## 跨决策点的交互

| 搭配 | 状态 |
|---|---|
| C1.b + C4.a | 自然搭配：仿射值的 rc==1 在静态上已知，于是 dup/drop 退化为复制与移动的边界（research/03 §b.6），也就是研究命名的那个搭配（research/01 §d Combos 3+4） |
| C2.d + C3.b + D1.b | 一套机制（`mut` 模式）三处使用：参数模式、排他窗口、可变性的限制范围 |
| C5.c + D3.b | 冲突：多次恢复的续体不得捕获 `~Copyable` 值（research/02 §b.8） |
| C4.c + 任一 C1 选项 | 搬移式 GC 与精确 RC、内部指针、FFI 冲突（research/01 §B.12） |

## 自洽的组合

作者可以整体采用三个端到端组合。Combo A 是各点推荐拼起来的方案，B 与 C 是研究给出的诚实备选。三者都适用同一套分期（research/01 §d Combo 6，research/03 §a.8）：先做仿射检查器与 DI 分析。再做精确 RC，配运行期 refcount==1 检查和 Inko 风格借用计数用于诊断。然后做每帧 arena 与保证的 TCO。最后把复用配对、借用推断、TRMC 与融合作为纯优化处理阶段加入，语言零改动。每一趟后期处理都用已证明的事实替换一次运行期检查，且不产生可观测的语义变化（research/01 §d 横切说明 5、§B.9、§B.10）。

### Combo A — “模式与 RC”（Hylo/Swift 表层，Perceus 运行）：C1.b、C2.d、C3.b、C4.a(+d)、C5.c、D1.b、D2.b、D3.d

```
// Combo A — illustrative; final surface syntax decided in docs/en/design/04-syntax.md
struct File: ~Copyable {                 // C1.b linear resource layer (set-theoretic negation)
  fd: RawFd
  deinit { close(self.fd) }              // C5.c: runs exactly once, at unique death
}
fn read_u32(f: mut File) -> u32 | IoError {   // C2.d/C3.b: exclusive borrow for the call
  const b = f.read_exact(4)?            // window on f; ? propagates IoError; b is a fresh linear buffer
  b.to_u32_le()                         // b consumed here (affine move)
}
fn sum_file(path: Str) -> u32 | IoError {
  const f = open(path)                  // unique owner; cannot be copied
  var total: u32 = 0                    // C3.b: local var — the only rebindable binding
  var n: u32 = 0
  while n < 1000 { total += read_u32(f)?; n += 1 }   // ? moves the error out; f's deinit still runs
  total                                 // f dies here: deinit closes fd (also on early return)
}
const words = "alpha beta gamma".split(' ')          // D2.b COW: "new" list, reuses when unique
const lens  = words.map(w => w.len()).collect()      // D3.d: fused into one loop by the compiler
const total = (sum_file("metrics.bin") or 0) + lens.len()  // D1.b: mutation only inside windows/locals
```

结论：学习成本低到中等（日常代码只需两个模式关键字，资源作者才用 `~Copyable` 与析构）。函数式路径上性能接近 C，因为 Perceus 复用加上线性性让逃逸分析成为精确的编译期事实（research/03 §a.7、§b.6）。面向值的代码表达力高，没有一等引用，图通过索引与 id 模式表达（research/01 §B.4 的变通清单）。选择它的含义是：对 docs/zh/design/01，把可复制性与线性性表达为取反的集合论约束，让 `~Copyable` 能与并集组合（research/01 §a，横切说明 1）。对 docs/zh/design/03，线性值本身就是 `sending`（research/01 §B.3，SE-0430），线性析构器就是取消清理的钩子（research/02 §b.8），没有函数着色。对 docs/zh/design/04，表层预算只有 `mut` 模式、`~Copyable` 与析构，落在 Kotlin 的上限之内（research/01 §c 表 3，Hylo 行）。

### Combo B — “推断与 RC”（Koka/Roc 隐藏风格）：C1.c、C2.b、C3.d(+a)、C4.a、C5.a、D1.a、D2.b、D3.c

```
// Combo B — illustrative; final surface syntax decided in docs/en/design/04-syntax.md
// No ownership, borrow, or mutation syntax exists anywhere in this file.
fn map(xs: List<Int>, f: fn(Int) -> Int) -> List<Int> {
  match xs {
    cons(h, t) => cons(f(h), map(t, f))  // FBIP: cells overwritten in place when xs is unique
    nil        => nil
  }
}
fn sum(xs: List<Int>, acc: Int) -> Int { // guaranteed TCO + TRMC: compiles to one loop
  match xs {
    cons(h, t) => sum(t, acc + h)
    nil        => acc
  }
}
const total = sum(map(list_range(1, 1000000), x => x * x), 0)  // zero annotations; zero allocations
                                          // when unshared; if shared, copies happen — a perf note
fn with_log(path: Str, body: fn(Log!) -> a) -> a {  // resources: the one opt-in linear corner
  const lg = open_log(path)               // lg: Log! — consumed exactly once
  const r  = body(lg)
  close_log(lg)                           // checker: consumed on every path
  r
}
```

结论：学习成本是全部调研设计中最低的（research/01 §c 表 2，Koka/Roc/Lobster 行）。可融合的函数式负载性能接近 C，且复用有保证、可观测（research/01 §B.2）。面向值的代码表达力高，但用户没有办法声明共享约定，资源被推进一个狭窄的可选层。选择它的含义是：对 docs/zh/design/01，章程硬性要求 2 必须改写，把“静态线性类型”改成“在仿射 IR 上做静态唯一性推断”，也就是 C1.c 记录的那份真实张力。对 docs/zh/design/03，多次恢复的效应必须禁止在续体中捕获线性值，或者限制为尾恢复（research/02 §b.8）。对 docs/zh/design/04，关键字预算为零，但错误信息政策改为输出性能提示，取代所有权错误（research/01 §d 横切说明 4），并且编译器团队要最先交付最难的那趟处理（以 Roc 的 `arc_solve` 为模板，research/01 §B.1）。

### Combo C — “保证”（Austral/Move 显式风格）：C1.a（固定多重性）、C2.a、C3.a、C4.d(+逃逸处 RC)、C5.b、D1.a、D2.b、D3.a

```
// Combo C — illustrative; final surface syntax decided in docs/en/design/04-syntax.md
// Every binding is exactly-once (linear); sharing only via block-scoped borrows.
linear type Bytes { ptr: RawPtr, len: Int }      // linear by default in this flavor
linear fn parse(raw: Bytes!) -> (Header!, Bytes!) {  // consume raw exactly once, split it
  const (h, rest) = split_at(raw, 64)
  (h, rest)
}
fn main() -> () {
  const raw = read_all("blob.bin")        // raw: Bytes!
  const (header, body) = parse(raw)       // raw consumed; checker: exactly once, every path
  borrow header as h in {                 // block-scoped read borrow (no first-class refs)
    print(h.magic)
  }
  const payload = decode(body)            // body consumed
  drop(payload)                           // explicit drop: required exactly once (C5.b)
  free_header(header)                     // consumed before scope end, or compile error
  // loop-depth rule: a linear bound outside a loop cannot be consumed inside it (research/01 §B.5)
}
```

结论：学习成本最高（consume-once 纪律塑造所有代码，对线性值的随手闭包不可能存在，research/01 §B.5）。性能上的确定性完美，运行期零成本，arena 也极易做到安全，因为逃逸只是一个类型事实（research/03 §a.7）。表达力三者中最低。research/01 §B.5 对这个模型的评价是“对可选的资源层来说是正确模型，对通用解释型语言来说是错误的默认”。选择它的含义是：对 docs/zh/design/01，多重性标注与泛型相互作用，必须按定义处固定多重性，否则会淹没在 UR 约束里（research/01 §B.6）。对 docs/zh/design/03，线性（尾恢复）效应写起来顺手，但多次恢复的处理器受重度限制（research/02 §b.1、§b.8）。对 docs/zh/design/04，这是三者中最重的表层（`linear`、`borrow`、`drop` 三个关键字），而章程的短教程可解释性预算几乎全花在所有权上。
