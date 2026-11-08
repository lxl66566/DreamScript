# 03 — 效应、错误与异步：决策目录

状态：写于 2026-11-04，把已完成的研究转化为可选项。主要证据：`docs/zh/research/02-concurrency-effects-errors.md`（下文按内部章节号引用为 research/02）、`docs/zh/research/04-syntax-ergonomics.md` 第 11 节（引用为 research/04 §11）、以及 `docs/zh/research/03-runtime-ir-jit.md` 概要第 5、6、8、9 条（引用为 research/03 exec N）。

姐妹目录，这里只按路径引用，内容由各自文件负责，正在并行撰写：`docs/zh/design/02-linearity-ownership.md`（线性/仿射类型、所有权、FFI 句柄）、`docs/zh/design/04-syntax.md`（表层语法）、类型目录 `docs/zh/design/01*`（集合论并集是它的未决选项）、以及索引 `docs/zh/design/00-decision-map.md`。

本文件的所有代码都使用中性的 TS 家族示意语法。示例写于 docs/zh/design/04 的表层决定之前，只说明语义。表层语法由 `docs/zh/design/04-syntax.md` 决定。

## 1. 决策点索引

| ID | 决策 | 选项 | 下文推荐 |
|---|---|---|---|
| B5 | 签名如何承载错误 | B5a 推断式错误并集，B5b 显式 Result，B5c 效应行标注，B5d 混合：并集为基、行后置 | B5d |
| E1 | 效应系统 | E1a 完整行多态可恢复效应，E1b 精简能力，E1c 能力参数，E1d 延后，底下保留处理器运行时 | E1d（标准库沿用 E1c 的惯用法） |
| E2 | 异步模型 | E2a 异步即效应，E2b VM 纤程 + 强制 nursery，E2c 非结构化 spawn，E2d 带颜色的 async/await | E2b |
| E3 | 并行 | E3a 类型系统保证安全的共享内存，E3b fork-join 数据并行，E3c actor/隔离，E3d 单线程 + FFI | E3b，落在 E3a 的底座上 |
| E4 | 错误处理 | E4a Result + 陷入（trap）二分，E4b 并集错误 + 集合差集处理器，E4c 异常即效应，E4d let-it-crash 监督 | E4b，保留 E4a 的陷入二分 |
| E5 | 取消与资源 | E5a nursery 作用域取消，E5b 线性资源，E5c defer/errdefer，E5d 超时即处理器 | E5a + E5b（E5c 作语法糖，E5d 作库） |

## 2. 共享前提（章程 + 研究，对所有选项生效）

- 执行是寄存器字节码 VM，带保证的 TCO。分派用 computed-goto 或尾调用穿线。值表示保持未装箱的类型化寄存器，ADT 与 Result 用未装箱的 tag+payload 对（research/03 exec 5, 6, 9）。
- 内存是 Perceus 风格的精确引用计数加复用。线性值的计数静态已知为 1，所以它们的 RC 操作被删除，它们的析构函数就是终结器钩子（research/03 exec 8、research/02 §8）。
- 堆纤程在这个 VM 里很便宜：纤程栈就是解释器数据（Lua `CallInfo` 数组），切换是一次指针交换，O(1)（research/02 §8）。
- 绑定默不可变。静态线性/仿射类型在编译期强制（章程硬性要求 2、5）。
- 没有 C++ 风格的特化（章程）。语法糖上限是 Kotlin 的全部语法糖面（章程第 4 条、research/04 §2）。
- 集合论并集是类型目录负责的未决选项（`docs/zh/design/01*`）。这里的多个选项假定它存在：平坦错误并集、`or`、`.err` 的集合差集类型。每个选项都会明说。
- `?` / `or` / `is` / `.err` 这些提取记号在 research/04 §11 研究，但由 `docs/zh/design/04-syntax.md` 决定。本文件只依赖它们已经研究出的语义。

## 3. 作者的问题，按选项回答

问题原文："can async/effects be abstracted so the benefits stay while learning cost and error rate drop?" 研究的拆解是：async 与效应的收益有四项。一是 IO 延迟而不引起线程爆炸。二是可组合的取消。三是调度抽象，同一份代码可阻塞、可事件驱动、可多线程。四是对触及外部世界的代码给出签名级审计。这四项都不严格要求行多态效应语法。现实世界中每个胜出的设计都把颜色放在恰好一个地方（运行时纤程、Job 树、或能力/行），颜色本身始终在场（research/02 §5.8, exec item 18, §d）。

| 选项 | 抽象机制 | 保留的收益 | 降低的学习/出错成本 | 残余成本 | 结论 |
|---|---|---|---|---|---|
| E1a 完整行 | 每个签名都推断效应行 | 审计、可恢复抽象、异步即效应 | 无，这就是成本最高的写法 | 每个签名都要行推断，行形式的错误消息 | 保留全部收益，学习成本最高（research/02 §1, combo 3） |
| E1b 精简能力 | 一个内建 IO 能力，环境式检查 | 粗粒度审计 | 行多态的复杂度 | 审计粗。用户不能定义可恢复抽象 | 最便宜的带类型审计，永久粗粒度（research/02 §4） |
| E1c 能力参数 | 显式 `io: Io` 参数 | 调度抽象、按参数审计、零类型机制 | 全部类型系统成本 | 调用点要一路传 `io` 的噪声 | 用参数化抽象异步，类型系统不参与（research/02 §5.1） |
| E1d 延后 | 处理器运行时内部化，没有表层语法 | 四项收益全保留，靠结构 + 能力约定 | 全部表层/推断成本，且无限期 | 用户定义的可恢复抽象被推迟 | 研究的结论：收益现在拿，成本延后（research/02 §d, combo 6） |
| E2a 异步即效应 | async = 推断出的行，没有 await | 可组合取消、没有 await 关键字、同步/异步混用 | 关键字的传染性 | 每个组合子签名都带行 | 表层词汇最少，编译器最重（research/02 §1） |
| E2b 纤程 + nursery | 把颜色搬进运行时纤程，结构由 nursery 块给出 | 四项收益全保留：取消靠作用域、调度靠池/事件循环、审计靠 E1c 约定 | 颜色、await、生命周期状态机 | 裸 detach 无法表达 | 找到的最高收益/学习成本比（research/02 combo 1, Table 1） |
| E2c 非结构化 spawn | 颜色由运行时擦除，没有结构 | 无色的阻塞风格 | 结构性收益一项都没拿到，泄漏照旧 | goroutine 泄漏、取消靠外挂、错误丢失 | 无色但无结构，出错率依旧高（research/02 §5.2） |
| E2d 带颜色的 async/await | 两种颜色，`await` 传染 | 作者熟悉 | 无，这就是成本中心的写法 | 双色标准库、API 重复、丢弃即取消的坑 | 熟悉感是它唯一优于 E2b 的方面（research/02 §5, §5.6, §5.8） |

其余四个决策点的逐选项回答并入各自的推荐：B5d 保住签名审计，同时错误仍是普通数据。E3b 落在 E3a 上，把无数据竞争保持为静态定理，只买一个调度器。E4b 用三个记号的表层保住类型化处理。E5a+E5b 保住取消的作用域与关闭保证，无需运行期打断机制。研究的净回答是：async 可以抽象，效应里非可恢复的那部分也可以抽象。抽象靠结构（nursery）、参数化（能力）和数据（并集）达成，底下用证据传递的处理器运行时，使完整效应这个逃生口保持可增量添加（research/02 §d 三部分回答）。

## 4. B5 — 签名如何承载错误

问题是“可能出错的地方”在签名里放在哪：作为返回类型的并集成员（数据）、包在包装值里、或放在效应行里（控制流）。这是表示选择，语义在这三种写法之间相同：`read : Path -> Data | NotFound | IoError` 与 `read : Path -> <exn{NotFound, IoError}> Data` 携带的信息完全一样，Koka 的指称表把这种同一性写成字面等式，`int -> exn int` 就是 `Z -> (Z + 1)`（research/02 §7.8, §1）。Effekt 的 `Exception[E]` 是带索引的中间形态（research/02 §2）。

### B5a — 返回类型里推断式错误并集（Roc 模型）

```ts
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
function readConfig(path: Path): Config | IoError | ParseError   // inferable: annotation optional
function load(): Config | IoError | ParseError {
  const text = readFile(path)?     // callee's IoError widens this function's union automatically
  return parseConfig(text)?        // ParseError joins the same way — composition is union arithmetic
}
```

先例：Roc 没有 Option 类型，只有带推断式标签并集错误的 `Try`，“当一个函数调用另一个函数时，可能的错误会自动合并成该调用点上所有可能错误的标签并集”（research/02 §7.1、https://www.roc-lang.org/faq）。这也是旧 DreamScript 的“把错误收进返回类型并集”想法，research/02 §7.8 在全部五个维度上判定它成立。

交互：配合集合论并集（未决选项，`docs/zh/design/01*`）时，错误集就是字面上的集合，传播是一次子集检查。如果并集只能是名义的，本选项退化为枚举样板。

| 维度 | 评估 |
|---|---|
| 学习成本 | 最低：“函数返回集合”，把章程的集合论哲学用在失败上。零包装词汇。 |
| 运行性能 | 每个可失败调用一个标签分支。在计划的 VM 里，并集返回是跨寄存器的未装箱 tag+payload 对（research/03 exec 6、research/02 §7.7, §8）。 |
| 出错率 | 顺手得到审计。但签名里没有任何东西标明哪些成员是“错误”、哪些是普通分支，处理纪律来自 research/04 §11 的记号。 |
| 编译器复杂度 | 泛型代码里的并集推断与扩大，并与线性/仿射检查合一。中等，大部分是类型目录已经在考虑的工作。 |

排除：把错误通道锁定为数据（对 nursery 错误收集与监督是好事，research/02 §7.8）。以后想加控制流错误语义，必须走 B5d 的桥，无法原生支持。

### B5b — 显式 Result 包装值（Rust/Gleam 模型）

```ts
function readConfig(path: Path): Result<Config, IoError | ParseError>
readConfig(p).map(cfg => cfg.port)     // wrapper is user-visible in every fallible signature
```

先例：Rust 的 `io::Result<Data>`，错误枚举携带载荷。Gleam 只有 Result，用户代码里没有异常，`panic` 只留给 bug（research/02 §7.1, §7.4, §6 BEAM/Gleam）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 低到中等：一个包装惯用法，但 Ok/Err 词汇与单子式管线（`map`/`and_then`）出现在每个签名里，research/04 §11 与 research/02 §7.6 把这算作语法糖预算支出。 |
| 运行性能 | 包装是未装箱 tag+payload 对时，分支成本与 B5a 相同（research/03 exec 6）。包装若是名义类，就退化为堆装箱。 |
| 出错率 | 显式程度最高，包装名本身就说明了失败。嵌套（`Result<Option<T>>`）重新进入语言（research/02 §7.6）。 |
| 编译器复杂度 | 最简单：完全不需要错误集推断，包装是普通 ADT。 |

排除：`?` 式传播糖在这里同样必要，Rust 后来也补上了这一层，所以它是 B5a 的机制加额外词汇。平坦并集组合（`T | none | E`，research/02 §7.6）被排除在默认方案之外。

### B5c — 效应行错误标注（Koka `exn` / Effekt `Exception[E]`）

```ts
function readConfig(path: Path): throws(IoError | ParseError) Config   // row carries the error set
// propagation is control flow: no return tag, works identically inside callbacks/generators
```

先例：Koka 的 `exn` 是内建的非可恢复效应 `final ctl throw-exn`，签名可推断、可随行组合。同时还有一个普通的 `result<a,e>` 类型并存（research/02 §1, §7.3）。Effekt 的 `Exception[E]` 是以错误类型为索引的效应，它的标准库把 `Result[A,E]` 定义为该效应字面上的“具体化”，并可用 `value` 反映回去，两者可以互相定义（research/02 §2）。Swift 的类型化 throws（SE-0413，6.0 已发布）说明业界正在收敛到“错误类型写进签名”，与机制无关（research/02 §7.2）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 中到高：动态处理器作用域再加上行词汇。Java 的受检异常是标注负担变体的警示数据（research/02 §7.2），所以推断必须是强制的，不能可选。 |
| 运行性能 | 沿控制流传播：没有返回路径改写，穿过高阶调用与生成器很自然（research/02 §7.8）。throw 在解释器循环里是已经存在的栈帧遍历（research/02 §7.7）。 |
| 出错率 | 自动获得签名审计。但错误不再是可存储的值，不经过一步具体化就无法缓存、跨进程发送、或被 nursery 收集（research/02 §7.8）。 |
| 编译器复杂度 | 即使效应只用于错误，也需要完整的行推断机制（E1a 的全部负担，research/02 combo 3）。 |

排除：从第一天就把 B5 与 E1a/E1c 耦合起来。若先选它，之后再降级到并集（B5a/B5d）是破坏性重设计，反方向则是增量添加。

### B5d — 混合：现在用并集承载，以后可选效应行错误

```ts
// v1: errors are union members of the return type (B5a representation, research/04 §11 surface)
function readConfig(path: Path): Config | IoError | ParseError
// v2, additive sugar carrying the same information — "this row is exactly your error union":
function readConfig(path: Path): throws(IoError | ParseError) Config    // desugars to the v1 signature
```

先例：research/02 §7.8 的原话结论是“返回类型里的错误并集就是我们已有的受限效应行”，并推荐采用并集/Result 形态为基，等完整效应到来时再加 `throws` 语法糖。Effekt 的 `result`/`value` 对证明这个翻译在双向都可互相定义（research/02 §2）。research/04 §11 说明推荐的 `?`/`or`/`.err` 表层出于同样原因已经是效应就绪的。关键性质在于：并集承载的错误本身就是受限效应行，所以这是一条兼容的演进路径，现有设计原样沿用。

| 维度 | 评估 |
|---|---|
| 学习成本 | 现在付 B5a 的成本（最低）。v2 的语法糖在被使用之前零成本。 |
| 运行性能 | B5a 的成本（一个可预测分支）。以后的行写法复用 E1d 运行时已有的同一条证据分派路径（research/02 §8）。 |
| 出错率 | B5a 的审计，加上错误可存储，供 nursery/监督使用（research/02 §7.8）。 |
| 编译器复杂度 | 维护一个衔接点：并集降级为返回标签，行降级为证据分派。两者必须共享同一语义，§7.8 已经确立它们确实共享。 |

排除：要求 v1 的传播是并集风味的（`?` 用 Zig 规则做集合包含检查，不允许隐式转换，research/04 §11），这样后来的 `throws` 糖才能直接脱糖成并集形式。其余组合全部保持开放，本选项的定位就是保留全部组合。

推荐：**B5d**，以 research/04 §11 的表层用并集承载错误作为基表示，效应行错误作为后续的增量语法糖。理由：与行携带相同信息（research/02 §7.8）。错误仍是可存储数据，可供 nursery 收集与监督。在寄存器 VM 里传播是一个完全可预测的分支（research/02 §7.7、research/03 exec 6）。通往完整效应的路保持敞开，无需迁移。

## 5. E1 — 效应系统

章程第 7 条把效应列为可选项：“认真评估，只有当它挣得其复杂度时才采用。” E1a 与 E1d 共享的机制背景是证据传递：Koka 用广义证据传递编译处理器（Xie & Leijen, ICFP'21）。处理器是一个存在性的 `(marker, op-table)` 对。线程上下文携带一个证据向量，加上一个带 8 个续延槽的内联 yield 记录 `kk_yield_t`。perform 时设置 yielding 标记并遍历向量。捕获把 perform 与处理器之间的帧链组合成堆闭包。这套机制不切换栈，在 C 与 JS 后端上行为一致（research/02 §1、https://www.microsoft.com/en-us/research/publication/generalized-evidence-passing-for-effect-handlers-or-efficient-compilation-of-effect-handlers-to-c/）。在字节码 VM 里：每个帧本来就带恢复 IP 与寄存器，所以续延就是 perform 与处理器之间每帧一个闭包。尾恢复操作完全跳过捕获，处理器分支直接在调用者的续延上运行。稳态成本约为每个效应操作一次间接调用，与虚调用无法区分（research/02 §8）。

### E1a — 完整行多态可恢复效应（Koka 模型）

```ts
effect Fail { fail(msg: String): never }                       // resumable operation
function divide(x: Int, y: Int): Fail<Int> { return y == 0 ? fail("div0") : x / y }
const ok = handle { divide(1, 0) } with { fail(msg) => resume(0) }   // resumption continues at the call site
```

先例：Koka 在一小组内建效应（`exn`、`div`、`ndet`、`io`）上做推断行，外加能力风味的内建效应（`net`、`fsys`、`ui`），支持多次恢复，用 `final ctl` 表示非可恢复操作，支持带作用域参数的命名效应，以及 `pub linear effect async-cancel`（research/02 §1）。Effekt 是编译效率上的对位，走词法能力（research/02 §2）。OCaml 5 是工业采纳的数据点：处理器四年后仍标记为实验性，卡住的是类型/API 的故事，运行时并非瓶颈（research/02 §3, exec item 7）。

必须说明的坑：多次恢复与线性值。一次恢复被调用两次时，不能携带线性资源，那等于按构造双重关闭。Koka 的答案有两个：运行期组合子（`finally`/`initially`，文档承认“正在研究用类型系统静态阻止这种情况”），以及类型级手段（`linear effect`：操作只能尾恢复，从而取消捕获，research/02 §1）。DreamScript 可以做得更强：在类型检查时禁止多次恢复续延内部捕获线性值，或者把恢复默认标为仿射除非显式标注。这是一条小的类型规则，安全收益很大，也是线性类型让效应比任何现有效应语言更安全的地方（research/02 §8）。

线性性/TCO 交互：尾恢复保持保证的 TCO，Koka 把尾调用降级成标签循环，Lua 复用调用帧（`luaD_pretailcall`），所以解释器同时继承两者（research/03 exec 9）。非尾捕获每帧分配一个闭包，并与 Perceus 的 RC 记账交互（research/03 exec 8）。效应本身是操作，可发送性只约束值，所以这里没有额外要求。被捕获的续延本身是一个值，必须遵守线性性。

| 维度 | 评估 |
|---|---|
| 学习成本 | 最高：用户写的每个组合子签名都带行变量。错误消息里会提行。Koka 自己的文档也靠悬停式 UI 支撑（research/02 §1）。 |
| 运行性能 | 好：证据传递不切栈、尾恢复无捕获、多次恢复每帧一个闭包（research/02 §1, §8）。 |
| 出错率 | 运行期低，有作用域、有类型、没有隐藏的控制流。但推断错误的 UX 是一个真实的开发者出错面（research/02 combo 3）。 |
| 编译器复杂度 | 全部选项里最高：行推断必须与线性/仿射合一整合，而这一块的先例很薄（research/02 combo 3）。 |

排除：过早把整个类型系统押在行推断上，以后很难降级、花掉章程的可解释性预算（章程条目：“能在简短教程里完整解释”）、把 B5 推向 B5c。

### E1b — 精简能力（Unison 模型）

```ts
function main(): {IO, Exception}<void> { io.printLine("hi") }   // one built-in IO ability + Exception
```

先例：Unison 1.0 只保留一个内建 I/O 能力加 `Exception`。检查规则刻意简单：子项的环境能力等于最近外层 lambda 的所需能力减去已处理能力。Unison 明确认为 Frank 风格的一般化在他们的设定里不可靠，因而拒绝（research/02 §4, exec item 6、unisonweb docs/ability-typechecking.markdown）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 便宜：一条环境集规则，没有效应多态的复杂度。但 `{IO}` 仍会出现在每个触及世界的签名里，颜色变成一个集合字面量。 |
| 运行性能 | 能力操作通过环境处理器分派，就是普通调用。对解释器友好（research/02 §4）。 |
| 出错率 | 只有粗粒度审计：类型里分不出文件访问与网络访问（Koka 的 `net`/`fsys` 正好展示了丢失的东西，research/02 §1, §4）。并发留在运行期且不带类型（`IO.forkComp`）。 |
| 编译器复杂度 | 比完整行简单得多：存在性行变量由检查精化，类型检查后不做一般化。但它仍是整套签名标注系统。 |

排除：用户定义的可恢复控制抽象基本出局、Koka 精度的异步即效应出局（Unison 的并发是内建层的）、审计粒度永久粗，除非最后还是升级到 E1a。

### E1c — 能力参数，没有效应语法（Zig 0.16 / Effekt 去掉行）

```ts
function connect(host: Host, io: Io, port: Port): Conn | NetErr   // io is an ordinary interface value
val conn = connect(h, Io.eventLoop(), 443)   // identical code over Io.blocking() or Io.iouring()
```

先例：Zig 0.15/0.16 围绕显式的 `Io` 能力参数重建了异步，写作 `fn connect(host, io: Io, port)`。`io.async(...)`、`Io.Queue`、`Io.Group`、`error.Canceled` 与 `defer` 组合。宣称的性质是同一份代码能在阻塞线程、线程池或 io_uring 上同样最优地运行，“没有堆分配”，控制流也是普通的（research/02 §5.1, exec item 8）。这就是 Effekt 的能力模型（OOPSLA 2020，“Effects as capabilities”）删掉行多态：效应减行类型（research/02 §2）。

线性性/TCO 交互：能力是普通值。线性能力使权限可通过移动转移且不可复制，权限转移本身就是移动，这是特性。在回调里捕获它没有问题，因为它是数据，捕获不牵涉控制流。与 TCO 无交互。

| 维度 | 评估 |
|---|---|
| 学习成本 | 低到中等：一个惯用法（“把 io 传进去”），零类型系统特性。调用点一路传参数是噪声成本（Zig 用惯用法缓解，research/02 §5.1）。 |
| 运行性能 | 每个触及世界的操作一次 vtable 间接跳转，放在解释器分派旁边看不出来（research/03 exec 5）。 |
| 出错率 | 中等：忘记传能力是编译错误，但派生工作的作用域纪律要靠 nursery 组合子来围栏（research/02 combo 2）。 |
| 编译器复杂度 | 没有新东西：普通参数与泛型。完全没有推断子系统。 |

排除：没有用户定义的可恢复效应表层。审计放在参数约定里，类型系统不承载它。但以后再升级到行是语法兼容的，因为能力本身就是行成员，只是不带行类型（research/02 combo 2, combo 6）。

### E1d — 延后：现在 Result + panic，底下是证据传递的处理器运行时

```ts
// v1 surface: unions + trap + nursery. No effect syntax, no user handlers.
function main(): void | NetErr { nursery { spawn { fetchAll(URLS) } } }
// v2 escape hatch, only if ever wanted — additive, no migration:
with handler { op ask() => resume(42) }
```

先例：这是研究的结论。给定了线性类型、默不可变、Result 与 nursery 之后，完整行多态可恢复效应在 v1 挣不回成本。能力传递核心则挣得回。而 nursery/取消/strand 拆除的语义从第一天起就应该用证据传递 ABI 实现，这样运行时就是处理器形状的，完整效应保持可增量添加（research/02 exec item 18, §d 三部分回答, combo 6）。

字节码 VM 里的成本（必要细节）：内部 ABI 的成本等于 E1a 的成本（上面的机制），但只在 nursery 边界与 strand 拆除时为两个内部效应（`cancel`、`discontinue`）支付，每个用户操作的成本为零。用户可见的代码路径不变，尾恢复的内部操作（取消检查）不分配任何东西（research/02 §8）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 最低：v1 没有要学的东西。v2 的逃生口是可选的 Koka 风格 `with handler`。 |
| 运行性能 | 用户代码保持原样。内部效应每个 nursery 边界两次间接分派。 |
| 出错率 | 安全性由结构承担（强制 nursery）。永远不会出现行形式的错误消息。 |
| 编译器复杂度 | 必须保住降级衔接点：错误降级为返回标签，cancel/discontinue 降级为证据分派。并且避免把非处理器的语义烤进表层特性。 |

排除：在结构上不排除任何东西，本选项的目的就是如此。接受的成本是用户定义的可恢复效应可能永远不会发布（research/02 判断这是可接受的风险，§d 第 3 部分）。

推荐：**E1d**，同时把 E1c 的能力惯用法采纳为标准库中触及世界 API 的约定，并把线性恢复限制（多次恢复续延里禁止捕获线性值）作为任何被捕获内容的常设类型规则。理由：研究的结论写得很明确（research/02 §d），运行时是处理器形状的，所以 E1a 保持可增量添加，语法糖预算与可解释性预算也都完好存活。

## 6. E2 — 异步模型

背景：堆纤程便宜，而且正是在这种 VM 形状里得到验证：纤程栈是解释器堆上可增长的帧数组，spawn 是一次分配，切换是一次指针交换（Lua/LuaJIT 先例）。Inko 以固定线程池、工作窃取和编译器注入的抢占点大规模运行这套设计（research/02 §8）。解释器里的抢占是分派循环里每 N 条指令或每次后向跳转做一次标志检查，严格比 Go 基于信号的方案简单（research/02 §8、分派循环形状见 research/03 exec 5）。

### E2a — 异步即效应，没有 await 关键字（Koka std/async 模型）

```ts
function wait(d: Duration): Async<void>    // async is an inferred row member — no await anywhere
function timeout(secs: Duration, act: () -> Async<A>): Async<A | none>
  = firstof(fn { wait(secs); none }, fn { some(act()) })   // a library function, not a language feature
```

先例：Koka 3.2.9 的 `std/async` 把 `pub alias async = <async-await, async-ioc, async-cancel, discontinue>` 定义为四个原始效应，取消作用域、`interleave`、`firstof`、`timeout` 全部作为用户级处理器实现，事件循环只是围绕 `main` 安装的默认处理器（research/02 §1, exec item 1、Leijen MSR-TR-2017-21）。准确的说法是：`wait : duration -> async ()` 显式携带行。Koka 拿掉的是让颜色带来痛苦的一切：没有 await 关键字、效应多态组合子只写一次、同步/异步混用。颜色本身仍然在场（research/02 §1）。

线性性/TCO 交互：`async-cancel` 是 `linear effect`，只能尾恢复，所以取消作用域链永不捕获续延，而无捕获的取消正是 `timeout`/`firstof` 下 TCO 完好的原因（research/02 §1）。await 恢复跨 strand 边界投递结果，所以被恢复的值必须可发送，线性/不可变数据按构造满足这一点（research/02 §7.9(2)）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 中到高：用户写的每个组合子签名里都出现行。颜色词汇搬进了推断，代价转移到编译器（research/02 §1）。 |
| 运行性能 | 好：证据传递，不切栈、尾恢复的异步操作无捕获（research/02 §1, §8）。 |
| 出错率 | 低：处理器作用域的取消、类型化的非确定性（`ndet`）、没有 await 传染性（research/02 §1）。 |
| 编译器复杂度 | E1a 的全部负担，再加上 async 的四个效应作为第一批真实行客户（research/02 combo 3）。 |

排除：需要 E1a 级别的行推断、把 B5 拉向 B5c、在以并集为基的 B5 上是最难后补的组合。

### E2b — VM 纤程 + 强制结构化并发（nursery/Job 树）

```ts
nursery {                                    // the only spawn site; lexical scope outlives children
  for url in urls { spawn { pages.push(fetchBody(url)) } }   // children are fibers on the VM pool
}                                            // exit: joined; first failure cancels the rest; errors union out
```

先例：Trio 的 nursery 语义，任务生命周期词法作用域化，取消是协作式且有作用域的，nursery 在子任务完成前不返回（research/02 §5.5, exec item 12、https://vorpus.org/blog/notes-on-structured-concurrency-or-go-statement-considered-harmful/）。Kotlin 的 Job 树，子任务随父任务取消，父任务等待子任务，失败向上传播，这套结构被其他语言反复抄走，在带颜色的前提下也被证明能修好出错率问题（research/02 §5.4）。“Go 去掉它唯一的致命缺陷”正是 research/02 combo 1。

线性性/TCO 交互：子任务收到复制值或线性/唯一值，所以没有共享可变状态可竞争（research/02 §5.5, §7.9）。取消与线性资源竞争：取消只在 await/抢占点被观察到，永远不会发生在消费过程中间，纤程拆除会遍历帧运行线性终结器，所以消费后关闭不可能被取消跳过（research/02 §7.9(4), §8）。保证的 TCO 是每纤程的，不受影响（research/03 exec 9）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 最低：处处阻塞风格，没有关键字，只多一个块形式（research/02 combo 1, Table 1）。 |
| 运行性能 | 对解释器极好：没有状态机装箱，没有 Pin 类的东西，O(1) 切换，堆纤程分块像 Loom 的栈分块（research/02 §8, §5.3）。 |
| 出错率 | nursery 强制则很低：Go 的部署记录去掉它唯一已记录的致命缺陷。泄漏与丢失的错误在结构上不可能（research/02 §5.2 vs §5.5）。 |
| 编译器复杂度 | 纤程支持 + spawn/nursery 指令 + 抢占标志：适度，有先例（research/02 §8）。 |

排除：裸 detach（`go f()`）刻意无法表达，被派生的任务需要显式的根 nursery/监督者、用户定义的可调度抽象需要 E1d 的衔接点、其余都不排除。

### E2c — 非结构化栈式 spawn（Go 模型）

```ts
go fetch(url)                          // fire and forget: no scope, no join, no cancellation
ctx := context.WithTimeout(parent, 2*s)   // cancellation becomes a threaded parameter convention
```

先例：Go 的栈式 goroutine、集成 netpoller、抢占式调度让它在构造上无色。已记录的成本是孤儿任务、以约定形式外挂的 `context.Context` 要穿过每个可取消 API、消失的 goroutine 里的错误丢失、手工泄漏检测、约 2KB 的栈底与每次调用增长检查（research/02 §5.2、https://fasterthanli.me/articles/lies-we-tell-ourselves-to-keep-using-golang 在那里被引用）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 起步最低，这就是陷阱：模型简单，失败模式不简单。 |
| 运行性能 | 好。2KB 栈底与增长检查在寄存器 VM 里是真实的每次调用成本，而这个 VM 本来不需要它们（research/02 §5.2）。 |
| 出错率 | 高：goroutine 泄漏、没有取消故事、错误丢失，这些是有记录的生产失败模式（research/02 §5.2, Table 1 “high without nursery”）。 |
| 编译器复杂度 | 需要 goroutine 级别的运行时工作（增长、抢占），而每单位工作买到的结构最少。 |

排除：与 E5a 直接冲突、nursery 无法后补，否则会破坏每个假定 detach 的 API、子任务的错误并集无法收集，因为子任务不可 join（research/02 §7.8 值论证）。

### E2d — 带颜色的 async/await（TS 风味模型）

```ts
async function fetch(url: Url): Promise<Page> { ... }   // the second color
const page = await fetch(u)                             // viral await; sync/async API duplication
```

先例：“What Color is Your Function?” 定义了成本模型：异步是第二种颜色，它要么阻塞地调用同步代码，要么 spawn 一个任务才能调用，`await` 传染，API 重复（research/02 §5 引言、https://journal.stuffwithstuff.com/2015/02/01/what-color-is-your-function/）。Rust 是带颜色加所有权结合的警示故事：Pin、async-fn-in-trait future 上不可命名的 Send 约束、运行时碎片化、丢弃即取消的坑。颜色加上所有权推断一起超出了可容忍的学习预算（research/02 §5.6）。Kotlin 保留了颜色，却在结构上取胜，颜色分裂仍是头号抱怨（research/02 §5.4）。

线性性/TCO 交互：跨挂起点的值决定 Rust 里 future 的 trait 约束，产生调用点上看不见的晦涩错误（research/02 §5.6）。有线性值时，对应的问题（“await 之间捕获了什么，它可发送吗？”）可以检查，但新增一类错误。无栈状态机与通用组合子下保证的 TCO 也配合不佳（research/03 exec 9 上下文）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 对这位作者表面上低，TS 背景是真实优势，但真实曲线是状态机生命周期与取消安全的坑（research/02 §5.6）。 |
| 运行性能 | 纸面上无栈最优（不换栈），在这里无关，因为这里的切换就是指针交换（research/02 §8）。 |
| 出错率 | 高：丢弃式取消、两个世界的 API 漂移、生命周期 bug（research/02 §5.6, Table 1）。 |
| 编译器复杂度 | 状态机降级：中等且众所周知，但每个通用组合子都要降级两次（同步/异步），除非效应把它们合一。 |

排除：标准库永久双色、同步/异步 API 重复、最难与解耦优先的章程哲学调和（函数着色让每个函数与它的调度纠缠，章程设计哲学）。

推荐：**E2b**，强制 nursery 的 VM 纤程，在 E1d 的处理器运行时上实现（nursery 本身就是内部 `cancel`/`discontinue` 效应的处理器），`firstof`/`timeout`/`interleave` 按 Koka 目录作为库组合子。理由：research/02 Table 1 与 combo 1，学习成本最低、解释器原生性能、有强制结构所以出错率低，而且 E2a 的每项收益都能增量到达。

子回答（必答）：带线性可发送性的 E2b 是否覆盖了 E2a 的收益，处理器仍在哪里胜出？

- 大体上是，逐项对应。可组合取消：nursery 取消作用域与 Koka 的 cancel-scope 处理器语义相同，只是块形式与行形式的差别（research/02 §5.5 vs §1）。调度抽象：由能力或可插拔事件循环提供，行不提供（research/02 combo 2, combo 6）。无 await：纤程就是阻塞风格，表层比 Koka 还少。同步/异步混用：彻底，因为根本没有颜色。审计：由能力参数承载（research/02 §d）。
- 处理器真正仍胜出的地方：（1）用户定义的可恢复控制抽象，解析器、迭代器、UI 会话、概率编程。（2）类型化非确定性，Koka 的 `ndet` 成员在行里被跟踪，纤程模型里没有任何东西跟踪它（research/02 §1）。（3）逃出词法作用域的一等恢复（`raw ctl`），用于内建之外的调度器（research/02 §1）。（4）零约定的多态，Koka 的 `map` 在任何处理器下都能工作而无需穿线，纤程模型则要按约定一路传 `io`（research/02 §1, §d）。
- 净结论：E2b 加线性可发送性覆盖了当初促使应用代码采用异步即效应的收益。处理器的优势是真实的，但属于库形态，E1d 让它保持可达而无需提前付账（research/02 §d 三部分回答）。

## 7. E3 — 并行

背景：并行（多核）与 E2（IO 延迟）正交，但共享运行时池。核心类型系统已经免费覆盖三件事。无数据竞争，因为语言里没有别名可变性。可发送性，线性值即可移动，对应 `sending`/`uni`/`iso` 角色。确定性并行，对不可变输入的纯函数可在任意线程数上运行。仍未解决的只有调度器、取消作用域、监督与无死锁（research/02 §7.9, Table 4）。

### E3a — 类型系统保证安全的共享内存

```ts
spawn { worker(move buffer) }      // linear Buffer moves in; caller's binding is dead — structural sendability
spawn { analyze(sharedGrid) }      // immutable data is shareable by construction
```

先例：Swift SE-0414 基于区域的隔离 + SE-0430 `sending` 参数，用推断后补的正是线性类型在语法上直接给出的转移推理。研究的明确表述是“线性值按构造就是 `sending`，不可变值按构造就是 Sendable”（research/02 §5.7, exec item 14、https://github.com/swiftlang/swift-evolution/blob/main/proposals/0414-region-based-isolation.md, /0430-transferring-parameters-and-results.md）。显式对位：Inko 的 `uni T`（唯一、可发送，永不给可变借用）与 Pony 的 `iso`（research/02 §6）。

线性/不可变已经免费买到什么：整套 Send/Sync 机制塌缩为两种可检查情形，线性值可移动，不可变值可共享（research/02 §7.9(1)-(2)）。剩下的是：调度决定（哪个线程跑什么）、局部性/伪共享调优、以及任何并发可变的共享结构，它在 v1 里要么不可表示，要么需要显式能力。

| 维度 | 评估 |
|---|---|
| 学习成本 | 低：两条规则，对比 Pony 的六能力格，后者存在的原因正是 Pony 没有线性类型（research/02 §6）。 |
| 运行性能 | 对细粒度共享内存并行算法最好。没有锁，移动的数据没有复制。 |
| 出错率 | 低：无数据竞争是静态推论，纪律不参与其中。 |
| 编译器复杂度 | 在 `docs/zh/design/02-linearity-ownership.md` 已经要求的线性检查之外为零：可发送性由线性检查推出，无需单独的子系统。 |

排除：没有。它就是章程已经规定的底座。它让以后添加任何 actor 或监督层都是安全的（E3c, E4d）。

### E3b — 只有 fork-join 数据并行（Rayon/Roc 模型）

```ts
const rendered = pages.par_map(fn(p) { p.render() })          // deterministic, order-preserving
const total    = rendered.par_reduce(0, fn(a, b) { a + b })   // associativity is the only requirement
```

先例：research/02 combo 5 与 §7.9(3)，对不可变输入或线性划分输入的纯函数并行化无需锁，且保持确定性。Koka 的 `list/interleaved` 确定性证明（`collapse-ndet`）与 Mojo 的 `parallelize` 是被引的对位（research/02 §1, §6 Mojo）。

线性买到什么免费的东西：无数据竞争是定理，所以组合子不需要运行期检查与加锁。剩下的是调度器里的工作划分与负载均衡。IO 延迟的并发在这里明确不处理，那属于 E2 的职责，两者组合在同一个池上。

| 维度 | 评估 |
|---|---|
| 学习成本 | 全部 E3 选项中最低：两个组合子，确定性，可重放。 |
| 运行性能 | 计算负载极好。完全不用同步。Perceus + 线性静态计数让 fork 侧的快照便宜（research/03 exec 8、research/02 §8 RC+线性性协同）。 |
| 出错率 | 最低：输出确定，竞争不可表示。 |
| 编译器复杂度 | 只有调度器与组合子降级。没有新类型规则。 |

排除：共享可变的并行惯用法按设计出局（按章程这是特性）。与 E2b 直接组合，IO 用纤程，计算用 par，共用一池（research/02 Table 4 第 3 行）。

### E3c — 消息传递的隔离/actor（BEAM/Pony/Inko）

```ts
process Counter { mut count: Int; fn async bump(n: Int) { count += n } }
val c = spawn Counter(0)
c.bump(3)      // message send: n is moved in if linear, copied if shared-immutable
```

先例：BEAM，每进程堆、复制式消息发送、抢占式基于 reduction 的调度、let-it-crash 监督（research/02 §6）。Inko，`uni T` 移动式消息、固定线程池加工作窃取、编译器注入的抢占点，研究中已 vendored 并验证（research/02 §6, §8）。Pony，`iso`/`val` 能力加因果消息与 ORCA GC（research/02 §6）。

线性买到什么免费的东西：线性消息走移动传递，BEAM 的主要成本（复制式发送）正好在大消息这个痛点上被消除（research/02 §6：“copying is unnecessary when messages are linear — you can move instead of copy”）。剩下的是：进程/消息协议设计、非线性的共享数据仍需复制、以及细粒度并行算法的最差情形，它必须分解成消息（research/02 combo 4）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 低到中等：进程模型直观。“我能发送什么”的规则需要教学，Inko 的 `recover`/`uni` 教学记录下了摩擦点（research/02 §6）。 |
| 运行性能 | 好：永远不用锁，移动式发送。对细粒度共享内存并行最弱（research/02 combo 4）。 |
| 出错率 | 在留在模型内的系统里是已知最低：隔离让整类失败不可表示（research/02 §6 BEAM）。 |
| 编译器复杂度/运行时 | 运行时增加最大的选项：邮箱、进程调度、可能还有每进程堆。给定线性性之后类型系统没有新东西。 |

排除：细粒度数据并行变得别扭、监督（E4d）在这里变得自然、如果把“进程”定义为一个带隔离类型状态的纤程，它能与 E2b 组合。

### E3d — 单线程 VM + 经原生 FFI 并行（Python 模型）

```ts
// in-language: nothing concurrent; all parallelism is foreign
val out = ffi.numpyTransform(bigArray)     // happens outside the language's rules entirely
```

先例：CPython 的 GIL 模型，语言本身不声称并行，并行归原生扩展。research/02 Table 1 最后一行给它定价：学习成本最低，语言内任务一个核。

线性买到什么免费的东西：语言内安全仍然成立，FFI 边界可以要求缓冲区线性所有权转移。剩下的是：所有并行性能都来自外部、不带类型、在保证体系之外，章程的性能目标（硬性要求 3）在并行语言内负载上不可达。

| 维度 | 评估 |
|---|---|
| 学习成本 | 最低。 |
| 运行性能 | 语言内计算封顶一个核。解释器 3-10 倍于 C 的目标（research/03 exec 11）在并行负载上无法达成。 |
| 出错率 | 语言内低，跨 FFI 无法度量。 |
| 编译器复杂度 | 最小可能。 |

排除：对任何计算型负载都与章程硬性要求 3 冲突。着色问题被推到 FFI 边界，从这里开始归外部代码负责（research/02 combo 5 分析）。

推荐：**E3b，落在 E3a 的底座上**，确定性的 fork-join 组合子是唯一的并行原语，线性/不可变可发送性规则（章程要求类型系统本来就会强制）作为通用安全网写进文档，这样 actor/监督层（E3c 形状，E4d）以后可以作为库添加。理由：research/02 §7.9 与 Table 4，竞争与可发送性已被核心覆盖。缺的只是一个调度器加组合子，fork-join 是最便宜的正确选择。

## 8. E4 — 错误处理

背景：两种失败必须分开，预期错误（有类型、被处理）与 bug（陷入 trap）。动机数据：“几乎所有灾难性失败（92%）都来自对软件中显式信号化的非致命错误的错误处理”（Yuan et al., 经由 research/02 §7）。VM 成本：Result/并集返回是返回槽里的一个标签加消费端一个分支，可与消费端指令融合。陷入是带帧遍历的纤程中止（research/02 §7.7, §8、research/03 exec 6）。

### E4a — Result 即数据 + 无法处理的陷入二分

```ts
function read(path: Path): Result<Data, IoError>   // expected failure: data
panic("invariant violated: negative size")          // bug: aborts the fiber with a trace, unhandleable
```

先例：Rust（预期失败用 `Result`，bug 用 `panic!`，`catch_unwind` 作为窄边界）、Zig（错误并集 + `panic`）、Swift（`throws` + `fatalError`/前置条件）、Inko（不可处理的进程终止 panic），这是新语言近乎普遍的收敛（research/02 §7.1, §7.4）。

这个 VM 里的陷入机制：陷入中止纤程，遍历它的帧运行线性终结器，并在 nursery/监督者边界浮出，实现近乎免费（research/02 §7.4, §8）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 低：两种失败，各一条规则。 |
| 运行性能 | 每个可失败调用一个可预测分支、tag+payload 在寄存器里未装箱（research/03 exec 6）、陷入只在罕见路径上。 |
| 出错率 | 好：类型化处理正好打中那 92% 的类别。bug 按章程的编译期优先于运行期偏好大声陷入。 |
| 编译器复杂度 | 全部 E4 选项中最低。 |

排除：没有隐藏控制流，每次传播都可见（按 research/04 §11 加糖）。nursery 错误收集很自然，因为错误是数据。

### E4b — 并集承载错误 + 集合差集处理（`?` + `or` + `.err`）

修补后的旧 DreamScript 想法。完整示例（research/04 §11，端到端）：

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

与历史想法相比修好了什么（research/04 §11）：旧的 `{1/0}` 块式类型让一个表达式在不同使用点有不同类型。它不可组合，破坏局部推理与推断（缺陷 1）。修补版只对表达式定一次类型，把工作放在处理器里用集合差集完成。旧的 `.err` 返回 `double | null`，只是给单子起了个新名字（缺陷 2）。这里的 `.err` 是差集类型，剩余部分是可见的。花括号同时充当块、thunk 与错误边界的三重职责（缺陷 3）被完全去掉。

先例：research/04 §11 调研了这些表层，Rust 的 `?`（去掉 `From` 转换规则，采纳 Zig 更便宜的包含规则）、Zig 的 `try`/`catch`/`orelse`、Roc 的后缀 `?`（逐字验证）、Phi 的 `or`、Swift 被拒绝的三种写法。并集统摄一切的框架出自 research/02 §7.6（Ceylon、TypeScript 收窄、Zig 错误集、Kotlin elvis 在那里被记功）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 三个记号，语义统一在一句话里陈述于并集之上。净糖面胜过 Kotlin 的四个、Swift 的五个、Zig 的四个（research/04 §11）。 |
| 运行性能 | 与 E4a 相同：一个分支，未装箱 tag+payload（research/02 §7.7、research/03 exec 6）。 |
| 出错率 | 由差集类型强制穷尽处理。平坦并集消灭 `Result<Option<T>>` 嵌套（research/02 §7.6）。 |
| 编译器复杂度 | 需要对并集做集合差集，前提是 `docs/zh/design/01*` 的集合论类型这个未决选项被采用。没有它，`.err` 退化为普通 `match`，三个记号的论据随之减弱。 |

排除：依赖 `docs/zh/design/01*` 的并集。如果以后错误也作为行传递（B5d v2），`or`/`.err` 必须继续工作，research/04 §11 论证它们本来就作用于并集成员，这正是让表层效应就绪的原因。

### E4c — 异常即效应（Koka `exn` / Effekt `Exception[E]`）

```ts
function read(path: Path): throws(IoError) Data     // control-flow propagation, no return tag
try { read(p) } catch (e: IoError) { recover }      // handler discharges the row
```

先例：Koka 的 `exn` 是内建非可恢复效应（`final ctl throw-exn`），语义上就是异常，可推断、可随行组合。`std/core` 里并存一个普通 `result<a,e>` 类型（research/02 §1, §7.3）。Effekt 的标准库把 `Result[A,E]` 定义为 `Exception[E]` 字面上的“具体化”，并可用 `value`（单子式反射）返回去，两者可以互相定义，所以一门语言最多只需要其中一个，再给另一个配糖（research/02 §2, §7.3, §7.8）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 中到高：在选定的 E1 之上再加动态处理器作用域。 |
| 运行性能 | 传播不需要返回路径改写，穿过回调/生成器很自然。非可恢复 throw 是一次便宜的帧遍历（research/02 §7.7, §7.8）。 |
| 出错率 | 自动审计。但错误走控制流传递，存储、缓存、跨进程发送都要先物化一次。nursery 想以数据形式收集子任务错误，就需要同时提供 Effekt 风格的 `result` 具体化（research/02 §7.8）。 |
| 编译器复杂度 | 即使效应只服务于错误，也需要完整的行机制（E1a 负担）。 |

排除：把 E4 与 E1a 耦合。与 E4b 的可存储错误性质冲突，除非两套机制同时发布（Effekt 的实际选择就是两套，外加两者之间的翻译，research/02 §2）。

### E4d — let-it-crash 监督层（BEAM）

```ts
supervise(one_for_one) {                // traps become restart events instead of aborts
  spawn { cacheLoop() }
  spawn { connAcceptor(sock) }
}
```

先例：BEAM 的监督树与 error-kernel 模式，进程进入意外状态就死掉，它的监督者重启它，可重启核心保持很小（research/02 §7.5, §6）。Inko 展示了无监督的默认：panic 带堆栈终止进程（research/02 §6）。

组合（必答）：与 E4a/E4b 一起，受监督纤程里的陷入作为重启事件投递给监督者，进程不会中止。正好在存在监督者时，陷入语义变成监督事件（research/02 §7.5）。与 E4c 一起，监督者就是捕获最终 throw 操作的最外层处理器。与 nursery（E2b/E5a）一起，监督者就是一个带重启策略的 nursery，research/02 combo 4 把它勾画为“let-it-crash-lite”。BEAM 的另一半成本即复制成本，正是线性移动式消息消除的东西（research/02 §6）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 中等：多一个概念，但这是众所周知的概念。 |
| 运行性能 | 可忽略：监督记账只在失败路径上。 |
| 出错率 | 长期运行系统里最低：按构造自愈（research/02 §6 BEAM 结论）。 |
| 编译器复杂度 | 运行时记账。类型系统里没有东西。 |

排除：没有，它叠加在 E4a-E4c 任何一个之上。它只假定任务单元（纤程或进程）有可重启的身份。

推荐：**E4b 作为处理表层，保留 E4a 的陷入二分给 bug**，预期错误是并集数据，由 `?`/`or`/`.err` 处理（research/04 §11），bug 以无法处理的方式陷入。这个二分正交且近乎免费（research/02 §7.4）。E4c 只能经由 B5d/E1d 的桥作为后续糖到达。E4d 在长期运行系统需要时随时可采纳，形态是 nursery 的策略。理由：research/02 §7.8 的原话推荐（并集为基，`throws` 作为后续糖）、三记号的预算（research/04 §11）、以及错误可存储供 nursery 收集。

## 9. E5 — 取消与资源

背景：线性性并不给出取消语义（谁取消谁、终结器何时运行），也不给出无死锁，这些正是 research/02 Table 4 的“缺失部分”列与 §7.9 的左侧清单。资源拆成两个问题：保证问题是谁关闭、如何证明，语法问题是清理怎么写。

### E5a — 只做作用域取消，靠结构化并发

```ts
nursery {
  spawn { download(f) }        // cooperative: observes cancellation at await points
  spawn { render(g) }
}                              // scope exit: joined; a parent cancel cancels children, cleanup runs first
```

先例：Trio，任务生命周期词法作用域化，取消协作式且有作用域，nursery 在子任务完成前不退出。这套设计进入了 Python 3.11 的 `TaskGroup`/`except*`（research/02 §5.5, exec item 12）。Kotlin 的 Job 树是实践中的采纳变体（research/02 §5.4）。Koka 的异步作用域是处理器风味的对位：取消被观察为 `Cancel` 结果，翻译成最终的 `discontinue()`，所以清理仍然运行，即带清理的取消，无需线程打断（research/02 §1）。

线性性/TCO 交互：取消只在 await/抢占点投递，永不在消费中间，所以线性资源要么被完整消费，要么其终结器在拆除时运行。拆除就是运行线性析构的帧遍历（research/02 §7.9(4), §8）。TCO 不受影响。

| 维度 | 评估 |
|---|---|
| 学习成本 | 低：一条不变式，“任务活在派生它的块里”（research/02 §5.5）。 |
| 运行性能 | 每个抢占点一次可预测标志检查（research/02 §8）。 |
| 出错率 | 低：恢复了“函数的动态范围界定其效应”这条不变式，即那篇随笔的结构化编程类比（research/02 §5.5）。 |
| 编译器复杂度 | nursery 指令 + 取消标志 + 拆除遍历。E2b 本来就需要全部这些。 |

排除：被派生的任务需要显式根作用域。与 E2b 机制相同，从资源视角看是同一个决策。

### E5b — 线性资源保证（消费即关闭）

```ts
val f = io.open(path)?        // File is linear: exactly one owner (typing per docs/en/design/02-linearity-ownership.md)
const data = f.readAll()      // consuming read: f is dead afterwards — the consume IS the close
// no execution path lets f leak: not return, not drop, not cancellation
```

先例：章程线性类型的直接推论。FFI 句柄作为线性值，绑定到 `docs/zh/design/02-linearity-ownership.md` 中的所有权决策 C1（内容由该文件负责）。支撑证据：research/02 §7.9(4)，“线性资源不可能被丢弃的 future 泄漏（Rust 的丢弃即取消坑在线性语言里是按构造的不健全，析构函数必须运行，或者值必须被消费）”，§5.6 记录了这里删掉的 Rust 坑。

| 维度 | 评估 |
|---|---|
| 学习成本 | 低到中等：消费即关闭是新惯用法，但它契合章程的单子式提取口味，且不需要新语法。 |
| 运行性能 | 可达的最好：关闭是一次静态移动。没有 GC 终结，没有 drop 标志。线性 RC 操作在编译期被删除（research/03 exec 8）。 |
| 出错率 | 语言内句柄按构造无泄漏。FFI 句柄必须按 pinning 规则线性包装（research/02 §8 FFI 说明）。 |
| 编译器复杂度 | `docs/zh/design/02` 已经强制的线性检查，加上消费/拆除点上的终结器发射。 |

排除：需要 docs/zh/design/02 的线性强制（章程硬性要求已经如此）。与 E5a 自然配套，取消不能跳过消费，拆除会运行终结器。

### E5c — defer/errdefer 块（Zig）

```ts
val f = io.open(path)?
errdefer f.close()            // runs on error exit only
defer log("done")             // runs on every scope exit
```

先例：Zig 在错误并集上的 `errdefer`/`defer`，有作用域清理，零类型系统内容（research/02 §7.1, §7.4）。

它在这里必须回答的尖锐问题：纤程在作用域内部的某点被取消时，`defer` 会运行吗？Zig 从不面对这个问题（没有取消）。在 E2b 下答案必须是“会，在拆除时”，这正是 Koka 的 `finally`/`initially` 语义，之所以必要，是因为多次恢复让“RAII 以单次恢复为条件”变错（research/02 §1）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 最低：从 Zig/Rust 的 drop 模式熟悉。 |
| 运行性能 | 便宜：作用域退出 thunk。 |
| 出错率 | 中等：open/defer 配对靠纪律维持，安全保证来自 E5b 的按构造关闭，忘写 defer 正是它堵上的洞。 |
| 编译器复杂度 | 琐碎。 |

排除：没有。但在多次恢复效应（E1a）下 `defer` 语义变得微妙，如果 E5b 存在，它不应作为规范的资源故事（双保险式的二选一，应当把保证选为规范）。

### E5d — 超时即处理器 vs 运行期原语

```ts
with timeout(2 * seconds) {          // user-level combinator: firstof(timer-fiber, action-fiber)
  const page = fetchBody(url)?
} or none                            // timed out: cancellation scoped to the block
// primitive alternative: timeout(2s) { ... } as a VM opcode over a timer wheel
```

先例：Koka 把 `timeout(secs, action)` 写成普通库函数 `firstof { wait(secs); Nothing } { Just(action()) }`，`firstof`、`interleave` 与取消作用域都是四个异步效应之上的用户级处理器。没有任何超时形状的东西是语言原语（research/02 §1, samples/async/basic.kk）。

| 维度 | 评估 |
|---|---|
| 学习成本 | 两种都低。处理器形状教会“超时是库代码”，这一点可迁移到用户定义组合子。 |
| 运行性能 | 处理器形状：两个纤程加一个定时器，在纤程 VM 里便宜。原语形状：一条指令。 |
| 出错率 | 处理器形状可组合（嵌套超时）并免费继承取消语义（research/02 §1）。 |
| 编译器复杂度 | 处理器形状只需要 E1d 运行时，而 E2b 已经要求它。原语形状把定时器固化进规范，带来语法糖预算与规范增长的成本（章程第 4 条）。 |

排除：处理器形状以 nursery/处理器运行时（E1d）为前提，而它已经在桌上。原语形状在 v1 就冻结了定时器抽象。

推荐：**E5a + E5b**，取消是 nursery 作用域且协作式（Trio 语义），资源是线性消费即关闭（类型归 `docs/zh/design/02-linearity-ownership.md`）。Zig 风格的 `defer` 在作用域内作为作用域退出的糖可用（E5c，非规范）。超时作为定时器能力之上的库组合子（E5d 处理器形状）。理由：research/02 §7.9 的缺失清单点名取消作用域与拆除正是线性类型不提供的两样东西。这一对最小地覆盖了两者，Koka 的 `linear effect` + `finally` 证据表明合并后的语义是健全的（research/02 §1）。

## 10. 决策点交叉交互图

| 如果你选 | 它需要 | 它强化 | 它阻碍或复杂化 |
|---|---|---|---|
| B5d 并集为基、行后置 | `docs/zh/design/01*` 的并集（用于 `.err` 差集类型） | E4b、E1d（共享的降级衔接点）、E4d（可存储错误） | 以 B5c 为起点（降级将是破坏性的） |
| E1d 处理器运行时，没有表层效应 | E2b（nursery 是它的第一个客户） | E1a 保持可增量添加、E5d 库形状、B5d v2 糖 | 以 E1a 为起点（行推断很难降级） |
| E2b 纤程 + 强制 nursery | research/03 exec 5/8/9 的运行时部件（分派、RC、TCO） | E5a、E3b（共享一个池）、E4d（监督者 = 会重启的 nursery） | E2c（detach）、E2d（颜色） |
| E3b 落在 E3a 底座 | docs/zh/design/02 线性性（章程已经强制） | E2b（共享池，竞争按定理不存在） | E3d（与章程性能冲突） |
| E4b + E4a 陷入二分 | `docs/zh/design/01*` 的并集、research/04 §11 的记号 | B5d、E4d、nursery 以数据形式收集错误 | 独立的 E4c（还需要 Effekt 风格的具体化） |
| E5a + E5b | docs/zh/design/02 C1（FFI 句柄作为线性值） | E4d、E2b（同一机制） | 把 E5c 作为规范资源故事（纪律与保证之别） |

无论选什么，都有两条常设类型义务：仿射恢复规则（多次恢复续延里不捕获线性值，research/02 §8，如果 E1a/E1d v2 将来启用，由 `docs/zh/design/02-linearity-ownership.md` 规定）与 FFI pinning 规则（外部调用要么 pin 住纤程，要么运行在载体线程上，research/02 §8）。

## 11. 与其他目录的接口（只给路径，内容由那里负责）

- `docs/zh/design/00-decision-map.md`：索引。本文件贡献决策点 B5 与 E1-E5。
- `docs/zh/design/01*`（类型目录）：把集合论并集作为未决选项。B5a/B5d 与 E4b 的 `.err` 差集类型以它为前提。如果那里拒绝它，E4b 退化为基于 match 的组合子，B5a 退化为名义枚举样板，这会改变各推荐的成本行，但不改变它们的排序。
- `docs/zh/design/02-linearity-ownership.md`：负责线性/仿射检查、所有权形态与 FFI 句柄表示。E3a 与 E5b 消费它的规则。上面的仿射恢复规则是它需要规定的类型义务。本文件不做任何所有权决策。
- `docs/zh/design/04-syntax.md`：负责表层语法。这里每个示例都是示意性的。`?`/`or`/`is`/`.err` 记号已被研究（research/04 §11），但不在本文件决定。nursery 块的形状（块形式、`with` 形式、尾随 lambda 构造器）是继承自 research/02 §d 的未决问题，属于那个文件。

## 12. 一致的组合

每个组合都是对全部六个决策点的完整内部一致回答，用一个覆盖异步抓取 + 错误处理 + 资源关闭的程序展示。所有语法都是示意性的，由 `docs/zh/design/04-syntax.md` 决定。

### 组合 A — “能力核心”（E1c + E2b + B5a/E4b + E3b + E5a/E5b）

决策点选择：B5a, E1c, E2b, E3b+E3a, E4b+E4a, E5a+E5b。

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

结论：学习成本低（一个惯用法“传 io”，一个块“nursery”，三个错误记号）。性能：每个触及世界的操作一次 vtable 调用，加上 O(1) 纤程切换，对解释器极好。出错率：低（强制结构、类型化并集、按构造关闭）。对 `docs/zh/design/02` 的影响：线性 File/Resp 句柄（其 C1）、移动进 spawn 的可发送性，没有别的奇怪要求。对 `docs/zh/design/04` 的影响：能力参数约定、`nursery`/`spawn`/`cancel`，以及 `?`/`or`/`is` 记号。`.err` 在 v1 可选。

### 组合 B — “效应就绪”（E1d + E2b + B5d/E4b + E3b + E5a/E5b）

决策点选择：B5d, E1d（E1c 约定隐含保留在标准库默认里）, E2b, E3b+E3a, E4b+E4a, E5a+E5b。

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

结论：学习成本三者中最低（关于效应什么都看不见）。性能：默认处理器分派与组合 A 的 vtable 调用同级，所以是同一档。出错率：与 A 一样低。与 A 的诚实差别在于，世界访问审计由默认处理器隐式完成，组合 A 则把参数显式穿线，这是 A 分数更好的唯一一条轴。对 `docs/zh/design/02` 的影响：与 A 相同的线性要求，外加仿射恢复规则，值得提早规定，即使 v2 被延后（research/02 §d 未决问题 5）。对 `docs/zh/design/04` 的影响：与 A 相同的记号。它应当决定 `with handler`/`op` 是预留为将来关键字，还是留着不分配。

### 组合 C — “完整效应”（E1a + E2a + B5c/E4c 行 + E3b + E5a/E5d）

决策点选择：B5c (+E4c), E1a, E2a, E3b+E3a, E4c+E4a 陷入, E5a+E5b（E5d 作为处理器）。

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

结论：学习成本最高（用户写的每个签名都有行、带行的错误消息、处理器作用域）。性能：好，证据传递、尾恢复无捕获，其余情形每帧一次闭包捕获。出错率：运行期低，但推断错误的 UX 是真实成本，编译器承担所有选项里最高的复杂度（行推断要与线性/仿射合一，先例很薄，research/02 combo 3）。对 `docs/zh/design/02` 的影响：必须在表层效应发布之前规定仿射恢复规则（research/02 §d 未决问题 5），并依赖 Perceus 的静态计数删除处理线性值（research/03 exec 8）。对 `docs/zh/design/04` 的影响：`effect`/`with handler`/`resume`/行显示约定，是三个组合里远超其他的语法糖预算请求，全部花在一个特性上（章程第 7 条“只有当它挣得其复杂度时才采用”正是本组合必须回答“是”的问题）。

### 组合一览对比

| 组合 | 决策点 | 学习成本 | 性能 | 出错率 | 02 影响 | 04 影响 |
|---|---|---|---|---|---|---|
| A 能力核心 | E1c+E2b+E3b+E4b+E5a/b | 低 | 极好（vtable + O(1) 切换） | 低 | 线性句柄、移动式可发送性 | 能力约定、nursery、3 个记号 |
| B 效应就绪 | E1d+E2b+E3b+E4b+E5a/b | 最低 | 与 A 同档 | 低（外加隐式审计） | 与 A 相同，外加提早规定仿射恢复规则 | 与 A 相同。预留 `with handler` |
| C 完整效应 | E1a+E2a+E3b+E4c+E5a/d | 最高 | 好（证据传递） | 运行期低，推断 UX 高 | 仿射恢复规则在 v1 之前强制 | 最大新增表层：effect/handler 语法 |

研究自己的排序是 B 优于 A，A 优于 C：B 是 research/02 §d 的 combo 6，A 是 combo 1+2，C 是 combo 3。§d 给 C 陈述的风险是在线性/仿射合一的故事存在之前，就把整个类型系统押在行推断上。

## 13. 来源

主要载体是研究笔记，它们持有完整来源列表：research/02（上面全部并发/效应/错误主张）、research/04 §11（提取表层）、research/03 exec items 5-9（VM 约束）。承重主张的直接链接：Xie & Leijen ICFP'21 https://www.microsoft.com/en-us/research/publication/generalized-evidence-passing-for-effect-handlers-or-efficient-compilation-of-effect-handlers-to-c/、Leijen structured asynchrony https://www.microsoft.com/en-us/research/wp-content/uploads/2017/05/asynceffects-msr-tr-2017-21.pdf、Koka https://github.com/koka-lang/koka、Effekt https://github.com/effekt-lang/effekt 与 https://effekt-lang.org/publications、Unison https://www.unison-lang.org/docs/fundamentals/abilities/、Zig 0.15.1 notes https://ziglang.org/download/0.15.1/release-notes.html 与 devlog https://ziglang.org/devlog/2025/、Trio https://vorpus.org/blog/notes-on-structured-concurrency-or-go-statement-considered-harmful/、Kotlin https://kotlinlang.org/docs/coroutine-context-and-dispatchers.html、What Color https://journal.stuffwithstuff.com/2015/02/01/what-color-is-your-function/、without.boats https://without.boats/blog/coroutines-and-effects/、Swift SE-0414/0430 https://github.com/swiftlang/swift-evolution/blob/main/proposals/、Roc https://www.roc-lang.org/faq、typesanitizer error models https://typesanitizer.com/blog/errors.html、unions essay https://www.arshad.fyi/writings/null-errors-and-enums-are-one-feature、Go preemption https://go.googlesource.com/proposal/+/master/design/24543-non-cooperative-preemption.md、Pony https://www.ponylang.io/media/papers/codesigning.pdf、Inko vendored docs（research/02 §6）、OCaml 5 manual https://ocaml.org/manual/5.4/。
