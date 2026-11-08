# 05 — 泛型与元编程（决策目录）

状态：阶段 2 选项目录，写于 2026-11-04，等待作者按 AGENTS.md 做出决策。主要输入：docs/zh/research/05-generics-metaprogramming.md（多态调研、反特化证据、集合论重载、comptime 谱系、derive、VM 执行、线性交互、五个命名的组合）。次要输入：docs/zh/research/03-runtime-ir-jit.md（概要第 3/6/10-12 条、§2 关于 dyn 内联缓存、§8「泛型的实现与反特化立场」、表 6），以及下面这些文件的决策点索引表：docs/zh/design/01-types.md（B1c 混合集合类型、B9a 通用结构化相等）、docs/zh/design/02-linearity-ownership.md（C1b 仿射核心、C2d 二等参数模式）、docs/zh/design/03-effects-errors-async.md（E1d 延后的效应、B5d rows-later）。docs/zh/design/04-syntax.md 可能在撰写中，此处仅按路径引用。表层记号全部归它负责。

阅读指南：每个决策点给出 3 到 4 个有名字的选项。每个选项包含：一段语法示例（示例性的中性 TS 家族语法，写于 docs/zh/design/04 的表层决定之前，只说明语义，表层语法由 docs/zh/design/04-syntax.md 决定）、一条指向 research/05 的先例出处、跨学习成本/性能/语法糖/编译器的权衡，以及该选项排除的内容。每个决策点以一行推荐收尾。下文每条「性能」主张所依据的运行时基线在「共享前提」中固定，来自 research/03。

## 决策点索引

| ID | 决策点 | 选项 | 推荐（一行） |
|---|---|---|---|
| B6 | 多态机制 | (a) cohtraits：一致的集合论 trait + 不相交的实例头 + 内建 derive，(b) explicit：字典/能力传递，没有隐式搜索，(c) modules：ML functor，(d) comptime：Zig 风格的 comptime 泛型 | (a) — 唯一满足全部四条硬性要求的选项（research/05 §9 的打分）。两两不相交的实例头（`T1 & T2 = Empty`）取代孤儿规则 |
| G1 | 元编程 | (a) none：只有内建 derive，(b) comptime：在 comptime 求值普通代码 + 静态类型反射，不做宏，(c) macros：卫生语法宏，(d) procmacros：token/proc 宏 | (b) — 覆盖 research/05 的最小充分集，并彻底删掉卫生问题。内建 derive 属性在每个选项下都会一并提供 |
| G2 | 一等类型与反射 | (a) types-as-values：类型是运行期值（Julia），(b) comptime-reflection：只在 comptime 反射，没有运行期元数据，(c) metadata：Java/C# 的运行期元数据表，(d) tags-as-data：并集就是普通数据，`is`/match 覆盖运行期问题 | (b) — 只在 comptime 反射。它的运行期一半是 (d)，由 docs/zh/design/01 的 B1c 混合集合类型零成本提供 |
| H3 | 泛型执行 | (a) evidence-ic：一份函数体 + 证据词 + 多态内联缓存，JIT 惰性特化，(b) mono-cache：字节码级单态化缓存，(c) erasure：全量擦除并装箱，(d) comptime-mono：只在 comptime 单态化 | (a) — 在编译器内部缓存已经唯一的 dispatch，语义不可见（research/05 §6.4、research/03 §8）。JIT 层的惰性特化缓存就是 (b) 的那张表 |

## 共享前提（对所有选项生效）

- 运行时基线（research/03）：带类型的寄存器字节码，每个静态类型对应未装箱的槽位（概要第 6 条：带类型寄存器直接存放未装箱的 i32/i64/f64，ADT 是未装箱的标签加载荷对）。任何操作数的类型都是编译期常量，因此不需要运行期类型守卫，也不需要字段内联缓存（概要第 3 条）。分层流水线是解释器加 copy-and-patch JIT 加可选的 typed-SSA 层（概要第 12 条）。存在类型/dyn 调用点保留 1 到 2 条目的内联缓存，它只是性能提示，不承担正确性职责（research/03 §2 第 1 条）。
- 类型基础：docs/zh/design/01 推荐 B1c 混合方案（名义标签按集合论方式组合：名义标签上的并集、交集、取反）与 B9a 通用结构化 `==`，带 research/05 §5.2 的五条例外。本文件建立在这些推荐之上，不重新决策它们。
- 线性性：docs/zh/design/02 推荐 C1b（仿射核心 + 可选线性层）与 C2d（二等参数模式，承载借用/消费/修改）。此处仅按路径引用，见下文「线性交互」。
- 效应与错误：docs/zh/design/03 推荐 E1d（底层有处理器运行期，表层没有效应语法，标准库提供 E1c 的能力惯用法）与 B5d（现在用推断的错误并集，效应行稍后）。下文 HKT 的延后引用 E1d。
- 反特化立场（章程、research/05 §3）：分派由类型与集合代数决定，从不看声明顺序、优先级规则或「最具体匹配」。下面每个选项的表述要么满足这条不变式，要么明说它在哪一点上不满足。
- 两轴分解（research/05 概要）：每种多态机制都拆成 (a) 实例如何被找到与 (b) 调用如何执行。B6 决定 (a) 轴，H3 决定 (b) 轴。Rust 与 Haskell 的痛点在 (a) 轴上。(b) 轴是表示策略。

## B6 — 多态机制

本点的问题：当泛型函数需要 `Eq[T]` 时，`Eq` 针对具体 `T` 的实现从哪里来：来自全局注册的一致实例集合 (a)，来自调用方传入的值 (b)，来自调用方应用的模块 (c)，还是哪里都不来，因为每个实例化都在编译期生成 (d)？四个选项都必须与集合论类型（docs/zh/design/01 B1c）和仿射值（docs/zh/design/02 C1b）共存。

### B6.a — `cohtraits`：一致的集合论 trait，实例头不相交，内建 derive

Rust 风格的外形，替换掉一致性规则：trait 可以携带关联类型。实例按类型头声明，唯一的规则是同一 trait 的实例头两两不相交：对每一对实例头 `H1`、`H2`，检查器必须证明 `H1 & T2 = Empty`（可判定的空集）。这就是「消解即交集子类型」：实例 `I => C[T]` 是一条箭头 `I -> C`，实例环境是箭头的交集，而消解一个约束是通过肯定前件（modus ponens）做的子类型判定（Marntirosian、Schrijvers、Thiemann、Steel，"Resolution as Intersection Subtyping via Modus Ponens"，OOPSLA 2020，research/05 §2.2-2.3）。匹配的唯一性于是从类型代数推出，全程没有优先级机制。

```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
trait Ord { fn compare(&self, other: &Self) -> Int }                 // one-parameter trait
trait Iterator { type Item; fn next(&mut self) -> Option<Self.Item> } // associated type

impl Ord for Int { ... }                // concrete head: a set of values
impl[T: Ord] Ord for List[T] { ... }    // conditional head: an intersection arrow (OOPSLA'20)

// the disjoint-head rule: for every pair of heads of one trait, H1 & H2 = Empty
impl Draw for Circle { ... }
impl Draw for Square { ... }
impl Draw for Circle | Square { ... }   // REJECTED: intersects both heads; witness type: Circle
impl Draw for Circle & ~Shadowed { ... } // negation carves exclusivity when overlap is intended
```

- 孤儿规则的替代方案。Rust 按位置禁止重叠（"wrong crate"，research/05 §3.3 机制 1），代价是 newtype 包装、对未来实现的 blanket 实现否决，以及 sealed trait 的折腾（research/05 §3.2）。DreamScript 精确证明不重叠，取代悲观地禁止：任何模块都可以为任何类型定义实例（外来类型的实例合法），注册是全局的，不相交检查在链接时针对最终的实例环境运行。冲突是响亮的编译错误，指出两个实例头以及它们交集中的见证值类型（research/05 §3.5：冲突必须要么不可能，要么响亮，绝不静默）。这精化了 research/03 §8 的「实例一致性（全局唯一，Haskell 风格）提供字典」一说：字典语义相同，一致性规则从位置唯一升级为集合不相交。
- 并集（docs/zh/design/01 B1c）。实例头是集合，因此并集实例头会归一：`impl Eq for Int | Str` 当且仅当每个成员都成立时成立。它是合并各分支实例的推导证据，检查器在成对不相交检查之前把它分解为分量。反过来，出现在另一个实例头内部的并集与各分量求交，必须与它们不相交。对于静态类型为并集 `A | B` 的值，运行期标签选出分支的证据。因为实例头两两不相交，标签最多命中一个实例，所以按标签做键的内联缓存永远不需要优先级决胜，失效也是单调的：值的类型只会收窄，而更窄的类型命中同一个唯一实例（research/05 §6.3 末段）。
- 终止性。条件实例是消解规则。递归消解需要终止性论证，Modus Ponens 演算提供了它（research/05 §2.3 与组合 A 的风险注记）。编译器必须拒绝「满足它需要它自身」的条件实例。
- 先例：research/05 §1.1、§2.2、§2.3、§3.2、§3.3（机制 1 与机制 5）、§9 组合 A。Castagna、Nguyen、Xu、Frisch，"Polymorphic Functions with Set-Theoretic Types"（POPL 2015，重载即箭头交集，`◁` 关系）。Parreaux 与 Chau，MLstruct（OOPSLA 2022，类型类被列为布尔代数类型系统的兼容扩展）。Xu、Huang、Oliveira（OOPSLA 2025，用 apartness 保持合并一致，也就是可证明的不重叠）。
- 权衡。学习：低到中等，覆盖 trait、关联类型、一条不相交规则。没有孤儿规则，没有一致性律师，没有特化词汇（research/05 §9）。性能：实例编译成作为参数传递的证据词。静态消解的证据是常量操作数，运行期消解的证据走每个调用点的内联缓存（research/05 §6.3）。语法糖：接近零，内建 derive 属性承担样板代码，Koka 风格的 UFCS 让普通函数具备方法调用的易用性。编译器：每个实例组的成对空集检查（组大小的平方，实践中很小，可缓存，见 research/05 §2.2）、条件实例的终止性检查，以及必须点出见证类型才有指导意义的重叠错误。
- 排除：永久排除重叠实例与特化（没有 OVERLAPPING/INCOHERENT 之类的 pragma，没有最具体匹配）。在实例出现的任何地方排除首次匹配或按优先级排序的消解，包括 comptime 生成的实例（research/05 §3.1 推论）。在不同作用域里排除同一实例头的竞争实例（那是 B6.b 的特性，本选项不要）。

推荐：采用 B6(a)，即一致的集合论 trait：实例头在声明与链接时用空集检查证明两两不相交，内建 derive。它是唯一同时满足集合论类型、不重叠、线性友好、字节码原生执行四条要求的选项（research/05 §9 的打分表）。

### B6.b — `explicit`：显式字典/能力传递，没有全局隐式搜索

完全没有消解：实例是普通值，需要它的人把它作为参数取走。Effekt 的能力传递效应系统证明显式路线可以扩展到完整的效应多态，且零一致性疑问（Brachthäuser、Schuster、Ostermann，OOPSLA 2020，research/05 §3.3 机制 3、§3.4）。

```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
type Eq[T] = { fn equal(&T, &T) -> Bool }        // the instance is an ordinary value
fn dict_get[K, V](d: &Dict[K, V], k: &K, eq: Eq[K]) -> Option[V]

dict_get(m, key, eqPair(eqInt, eqStr))           // fully explicit: nothing is ever resolved
dict_get(m, key, _)                              // sugar: insert the unique Eq[K] when provable
```

- `_` 那一行就是魔法渗回来的地方：在可证明时插入「唯一实例」，属于轻量版受限消解（scoped resolution lite，research/05 §3.4），所以纯 B6.b 与加了语法糖的 B6.b 是同一个刻度上的两个位置。Scala 3 的 `given`/`using` 是加糖那一端的证据：词法作用域、最近者胜、歧义即错误，让受限搜索保持确定性（research/05 §1.5）。Kotlin KEEP-367 的 context parameters（2025 年在 2.2 落地）是主流的受限变体。只要加任何语法糖，歧义即错误就不可让步。
- 权衡。学习：概念上最低（「实例是值，没人替你找」），但样板代码的墙是真实的：Scala 的隐式搜索之所以存在，正是因为手工穿线很痛。Effekt 的缓解办法是把处理器做成每个作用域的环境值（research/05 §3.4、§9 组合 B）。性能：与 B6.a 的表示完全相同，两边都是证据词，两边都适用内联缓存，静态消解的证据是常量操作数。语法糖：不做缓解就很高，API 边界上的调用点会长出证据参数。编译器：很轻，没有搜索，没有全局实例表，没有不相交检查器。每条错误信息都点出实际传入的证据。
- 线性契合度是四个选项里最好的：能力是普通（可能是仿射的）值，所以仿射流分析能看到每一次使用，只消费一次的证据用与任何其他值相同的 C1b/C2d 机械表达。
- 排除：隐式实例生态，包括追溯性实现（retroactive conformances）、blanket 实例的易用性、`x == y` 运算符语法糖（除非唯一实例可插入），以及没有可见管线的 trait 对象式扩展。也排除 B6.a 的「实现一次，处处可用」的库体验。
- 先例：research/05 §1.4（OCaml functor 作为显式模块参数）、§1.5（Scala given）、§3.3、§3.4、§9 组合 B。Effekt 能力（OOPSLA 2020）。Koka 处理器值。

推荐：B6(b) 定位为逃生口，v1 的默认取 B6(a)。如果按不相交实例头的消解将来显得过于隐式，再采用它命名的受限实例（research/05 §3.4 的综述：默认用一致声明，受限命名实例作为第二条路，歧义永远是错误）。

### B6.c — `modules`：ML 模块与 functor

泛型即 functor 应用，实例即模块参数。生成式 functor 让同一个 functor 的两次应用得到两个不同的抽象类型，于是多个实例按构造共存而不产生歧义。一等模块（把模块打包成运行期值）是存在类型的显式写法（research/05 §1.4）。

```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
module type Ordered = { type T; fn compare(&T, &T) -> Int }
module Map(O: Ordered) = { ... }          // generative: two applications, two distinct types
module StringMap  = Map(StringOrd)        // explicit instantiation 1
module ReverseMap = Map(ReverseOrd)       // explicit instantiation 2: coexists, no conflict
```

- 权衡。学习：高，涉及签名、应用式对生成式 functor、一等模块打包，这直接顶到 Kotlin 语法糖上限（章程硬性要求 4，research/05 §9 组合 C）。性能：极好且全静态，完全没有运行期机械。语法糖：对集合代码来说很重，这正是 OCaml 生态长出 ppx 代码生成与一等模块垫片的原因。一行 `BTreeMap[K: Ord]` 变成两步实例化。编译器：中等（签名匹配、生成性），但 derive 的故事是四个选项里最弱的：没有约束驱动的成员合成，OCaml 需要一个通用多态 compare 加外部 ppx 插件（research/05 §1.4、§5.1）。
- 排除：轻量的约束式重载、追溯性实例定义，以及 trait 式捆绑包上的方法调用易用性。它让日常的泛型容器 API 明显变成两步。
- 无论如何都值得搬进 B6.a 的部分：一等模块作为存在类型（`box Trait` 式的携带证据的盒子，research/05 §8.1），以及用于能力的生成式抽象类型。
- 先例：research/05 §1.4、§9 组合 C。Rossberg，1ML（ICFP 2018，核心语言与模块语言统一，"F-ing modules"）。

推荐：拒绝 B6(c) 作为整门语言的方向。如果将来需要插件式的抽象类型，把它的「一等模块作为存在类型」这一片段借进 B6(a)。

### B6.d — `comptime`：comptime 泛型（类型是编译期值）

泛型是从类型到类型的编译期函数，没有实例，没有证据，没有消解，只有实例化。Zig 是存在性证明（research/05 §4.1 第 3 级、§9 组合 D）。

```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
fn List(comptime T: type) type {
  return struct { items: [T], len: Int }
}
var xs: List(Point) = ...
```

- 与带类型寄存器 VM 的交互：这里完全没有运行期多态，每个实例化都有自己的字节码体，所以每个寄存器都保持未装箱，每条操作码都完全按表示形式专用（把 research/03 概要第 6 条/§1 的基线推到极致）。代价一侧：解释器必须在首次使用时为每个实例化物化字节码体，因此编译成本在启动时可见，字节码大小随实例化扇出增长（research/05 §6.1、§9 组合 D）。Julia 把这笔账放在 JIT 层，解释器层保持统一。
- 集合论契合度很差：comptime 生成的实例逃出静态集合代数，于是不重叠不变式在它们之上无法强制。一个 comptime 循环可以生成两个相交的实例头，这种交集只能在生成之后（如果真能发现）被发现（research/05 §9 组合 D）。线性检查器必须在生成的代码上运行，Zig 省略了这一步。
- 反特化契合度是混合的：源码层没有重载优先级，但语义就是模板实例化，每个参数组合一份新函数体，这正是章程在「泛型与 ad-hoc 规则纠缠」时拒绝的机制。这里没有 ad-hoc 的东西，但可解释性的落点从「类型是集合」变成「有在编译期运行的代码」。
- 权衡。学习：中等，只涉及一个概念，但用户要学习调试第二个执行阶段，而且 comptime 的错误信息是 Zig 有记录的弱点。性能：稳态最好，但在解释器里有启动风险。语法糖：语法少，语义开销大。编译器：一个 comptime 求值器，加逐实例化物化，加生成后的检查趟。
- 排除：全局不相交强制、基于集合代数的消解（docs/zh/design/01 B1c 对多态变成装饰），以及证据多态的动态路径（不额外加装就没有存在类型的基础）。
- 先例：research/05 §1.8（C++ constexpr 作为半步）、§4.1 第 3 级、§9 组合 D。Zig comptime 语言参考。

推荐：拒绝 B6(d) 作为多态机制，改为采用它的反射那一半作为元编程的基础（下文的 G1.b）。research/05 §9 对组合 D 的结论正是这个拆分。

### B6 对比表

| 选项 | 消解规则 | 学习 | 性能（解释器层） | 语法糖成本 | 编译器成本 | 反特化契合 |
|---|---|---|---|---|---|---|
| B6.a cohtraits | 不相交实例头，消解 = 子类型判定（OOPSLA'20） | 低到中 | 好（证据 + 内联缓存） | 低 | 空集检查器 + 终止性检查 | 完美 |
| B6.b explicit | 无，证据就是值 | 概念低，样板高 | 好（表示相同） | 中到高（被重新引入消解的 `_` 语法糖缓解） | 很轻 | 完美 |
| B6.c modules | functor 应用 | 高 | 极好 | 高 | 签名、生成性 | 完美 |
| B6.d comptime | comptime 实例化 | 中 | 启动风险，稳态最好 | 语法少，语义多 | 第二阶段 + 检查生成的代码 | 混合 |

### B6 与其他设计轴的交互

- docs/zh/design/01：B6.a 为消解消费集合代数（在 B1c 下实例头是标签集合，不相交性是一次集合计算）。B6.d 绕过它。在每个选项里，B9a 的通用结构化 `==` 都让相等成为语言级通用能力。`Ord`/`Hash`/`Show`/serde 仍由 trait 中介（research/05 §5.3）。
- docs/zh/design/02（C1b、C2d）：B6.a 通过 `~Copyable` 式约束和参数约定表达仿射泛型。B6.b 组合得最好（证据是普通的仿射值）。B6.c 也可行（模块是值）。B6.d 最弱（线性性必须在生成的代码上检查）。
- docs/zh/design/03（E1d、B5d）：证据 dispatch 与 E1d 的处理器运行期需要的是同一套基础。B6.b 与 E1c 是同一种惯用法（能力参数）：选其中一个就等于统一另一个。
- 存在类型在每个选项里都是正交且廉价的：一个携带证据的盒子（`box Json`）就是值加它的字典，一等模块是显式写法（research/05 §8.1）。签名里默认用通用类型：存在类型会装箱并破坏仿射所有权推理（research/05 §8.1），所以只留给异构集合与插件边界。

## G1 — 元编程

research/05 §4.1 给出的谱系从第 0 级（什么都不做，Go 用 `generate` 做带外代码生成）经过第 2 级（编译器内建合成：Swift SE-0185、Kotlin data class、MoonBit `#derive`）、第 3 级（comptime 求值 + 静态反射：Zig、D 的 CTFE、Mojo、C++26 P2996）、第 4-5 级（模板、卫生语法宏、Racket 的 phase）一直排到第 6 级（依赖类型，在语法糖上限之下属于范围之外）。决策是 DreamScript 落在这条谱系的哪一段。

### 最小充分集（research/05 §4.3）

所有调研到的 derive 式需求都分解为四个需求：(1) 对类型结构的静态反射：带名字、类型、顺序的字段/分支列表。(2) 对该列表的编译期迭代与分支。(3) 为拼装成员实现而对普通代码做编译期求值。(4) 为热点情形提供内建 derive 属性，让常见需求只有一个词长、编译得快。Zig 证明 (1)+(2)+(3) 足以对任意 struct 做 JSON 序列化（`@typeInfo` + `inline for` 就是全部配方）。MoonBit 用 `#derive(Eq, Hash, Debug, ToJson)` 在 moonbit-core 里密集使用来证明 (4)。Swift 证明仅有内建合成就能覆盖日常情形（SE-0185）。最小充分集覆盖不了的是：任意语法扩展、语法层 DSL、项重写，以及 phase 拆分，调研到的 derive 式需求没有一个需要它们（research/05 §4.3）。token 宏存在的主要原因是那些语言缺少静态反射。C++ 用 C++26 静态反射（P2996）正朝同一结论收敛：在模板出现二十五年之后，「用反射，不用重写」（research/05 §1.8、§4.1）。

### G1.a — `none`：除内建 derive 之外不做（Go/Roc 姿态）

- 覆盖：只有需求 (4)。无法覆盖：编译器没有随附提供的任何用户自定义 derive 式抽象，包括自定义序列化形状、schema 生成、ORM、测试数据生成。逃生口是带外代码生成（Go `go generate` 让一个程序写源文件，编译器对此无感知，跨生成边界没有类型安全，见 research/05 §4.1 第 0 级）或运行期反射（把错误从编译期挪到运行期，违反章程，并花掉章程要求的性能）。
```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
type Point = { x: Int, y: Int }        // no attribute: the compiler decides what is synthesized
// user serialization needs -> out-of-band: `dreamtool generate schema.ds` writes .ds source
```

- 权衡。学习：最低。性能：零开销，零机械。语法糖：零。编译器：零。但上限是编译器团队的发布计划：每个生态需求都变成一个编译器特性请求。
- 排除：成员生成方面一切用户可扩展性。生态因此转向自带解析/重生成脆弱性的外部生成器。
- 先例：research/05 §4.1 第 0/2 级、§5.1（Go 那一行，调研中最弱的 derive 故事）。Roc（结构化相等加能力要求，完全没有 derive）。

G1 内的推荐：它只有在通往 G1.b 的路上作为 v1 的过渡阶段才可行。作为终局选择，它要么饿死生态，要么招来带外代码生成。

### G1.b — `comptime`：在 comptime 求值普通代码 + 静态类型反射，不做宏

语言自身的语义在编译期对 `type` 值运行，反射读取类型结构（`typeof`/字段/分支），没有第二门语言，也没有 token 操作。Zig 的哲学表述最清晰：comptime 就是在编译期求值的 Zig 代码，代码可以生成代码，宏因此多余（research/05 §4.1 第 3 级）。

```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
fn schema(comptime T: type) -> String {        // need (1)+(2): fields of T at comptime
  var s = "{"
  for f in fields(T) { s += f.name + ": " + type_name(f.type) + ", " }
  return s + "}"
}
@derive(Show, ToJson)                          // need (4): built-in derives for hot cases
type Point = { x: Int, y: Int }
```

- 覆盖：完整覆盖需求 (1)(2)(3)。再加上内建 derive（它们是编译器合成，在每个 G1 选项下都会提供，见「derive」一节），最小充分集就被完整覆盖。
- 卫生：这个问题在此不存在。这里没有替换，生成的代码就是普通代码，反射读取结构，不重写结构（research/05 §4.2）。这一点相对 G1.c/G1.d 是结构性差异。
- 权衡。学习：中等，需要掌握一个新概念（「在编译期运行普通代码」），还要学习调试第二个执行阶段。comptime 失败属于编译期求值失败，错误形状很独特。性能：零运行期开销，一切都在字节码发射之前消解完成，编译期成本随 comptime 用量增长。语法糖：语法少，comptime 与运行期的边界是用户必须找到的语义线。编译器：一个共享 tier-1 语义的 comptime 求值器（两个阶段，一门语言），以及在声明时生成的逐类型反射表。
- 排除：实践中永久排除 token 宏与用户语法扩展。以后可以加装，但可解释性预算（章程：能在一篇短教程里完整解释）只能花一次，G1.b 把它花在用户已经知道的概念（函数）上，第二门元语言因此没有位置。
- 先例：research/05 §4.1 第 3 级、§4.2、§4.3、§9 组合 D（反射那一半）。Zig comptime 与 `@typeInfo`。D 的 CTFE。Mojo `comptime`。C++26 P2996。

G1 内的推荐：采用 G1(b)，它是唯一零卫生机械覆盖最小充分集的选项，并让元语言与目标语言保持同一。

### G1.c — `macros`：卫生语法宏（Rust `macro_rules`/Scheme 风格）

带卫生的模式到展开的替换：宏体引入的标识符按每个展开点重命名，因此捕获不到调用点的标识符（Rust 的 token 级卫生，Racket 的 syntax object 让卫生成为默认并另设显式逃生口，见 research/05 §4.2）。

```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
macro vec {
  ($($x:expr),*) => { { let mut v = List(); $(v.push($x);)* v } }
}
```

- 卫生的负担在这里具体化：展开发生在类型检查之前，所以带类型的语言要么在宏系统内部重新实现类型检查（Typed Racket 的做法），要么把错误推迟到展开后的 token 上，且没有类型信息（Rust 的 token tree，见 research/05 §4.1 第 5 级）。卫生本身就是逐标识符的重命名记账，附带逃生口（`datum->syntax`、Nim 的 `{.inject.}`）。要让宏可靠地组合，还需要 phase 纪律（"macros that work together"，Flatt/Findler 2018）。这些都是 G1.b 不需要的子系统。
- 覆盖：部分覆盖需求 (1)-(4)：derive 可以表达，Rust 用 proc 宏实现它，`macro_rules` 不够用，因为带类型的成员生成需要类型信息。语法宏真正的产物是语法糖：控制流形状的 DSL 与零成本惯用法。章程的语法糖预算上限是 Kotlin 的全部表层，其中没有宏系统，把它花在这里就意味着别处要砍。
- 权衡。学习：高，需要第二门模式/展开语言，带自己的作用域规则。性能：零运行期成本。编译器：展开基础设施、卫生、phase、宏调试。错误质量：类型错误在展开之后出现，归因到生成的代码上。
- 排除：G1.c 在 G1.b 覆盖的东西之外没有任何新增，而且组合性更差：函数组合可预测，模板组合性更差（research/05 §4.1 第 4 级）。在 G1.b 之后再加宏会造成两种重叠的元机制，这正是「解耦优先」的章程所拒绝的。
- 先例：research/05 §4.1 第 4-5 级、§4.2。Nim 模板（默认卫生，标准库建立在它们之上）。Racket 的 phase。Rust `macro_rules!`。

G1 内的推荐：拒绝 G1(c)，因为它买的是预算付不起的语法糖，并重新实现 G1.b 免费得到的东西。

### G1.d — `procmacros`：token/proc 宏（Rust proc 风格），很可能被排除

proc 宏是在编译期消费并产生无类型 token 树的普通程序。在可解释性预算之下，这里把它们标为很可能被排除。目录如实记录其权衡。

- 为什么卫生加工具成本就是原因，具体来说。卫生：proc 宏本质上不卫生，它们操作裸 token 树，所以引入的标识符可能捕获别的标识符，也可能被捕获，这是 proc 宏脆弱性的主要来源（research/05 §4.2，serde 的属性 DSL 活在 token 流里就是典型症状）。工具：编译器即库的边界（解析/发射 token）、一个自带构建步骤和编译期成本的独立 proc-macro crate、展开的 IDE/调试支持，以及指向展开输出的错误信息，用户原本的写法在其中已经消失。与一致性的交互：宏生成的实现可能意外与尚未写下的实例重叠，重新打开 B6.a 刚刚堵上的不相交漏洞，除非宏输出重新进入检查器（research/05 §3.2 第 4 点）。
```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
#[proc_derive]                                  // a user program over untyped tokens:
fn derive_json(input: Tokens) -> Tokens {       // sees token trees, not types
    let fields = parse_tokens(input);           // re-parses the language by hand
    emit_tokens("fn to_json(&self) -> String { ... }")   // emits raw syntax
}
```

- 可解释性预算：「哪些代码在运行？」在不了解每个宏的展开时无法回答。章程要求语言能在一篇短教程里完整解释，而类型检查器前面的一层 token 重写层是本页最大的单项可解释性成本。
- 覆盖：一切，但机械最重：Rust 生态同时证明了它的力量（serde、derive_more）与代价（一个 proc 宏子产业、更慢的构建、token 级的错误质量，见 research/05 §5.1）。
- 排除：稳定、教程规模的语义、冷的快速构建，以及「一门语言」这一性质。
- 先例：research/05 §4.1 第 5 级、§4.2、§5.1（Rust 的 derive 成本）、§3.2 第 4 点。

G1 内的推荐：v1 拒绝 G1(d)。如果将来出现具体的序列化生态需求，G1.b 的反射加内建 derive 就是有证据支持的替代品。

### G1 对比表

| 选项 | 覆盖的最小充分集 | 卫生机械 | 学习 | 编译器成本 | 排除 |
|---|---|---|---|---|---|
| G1.a none | 仅 (4) | 无 | 最低 | 零 | 一切用户可扩展性 |
| G1.b comptime | (1)(2)(3)(4) | 不需要，没有替换 | 中 | 一个 comptime 求值器 | 实践中的 token 宏 |
| G1.c macros | (2)(3) 经模式，(1) 弱类型 | 重命名记账 + phase | 高 | 展开基础设施 | 组合的简单性 |
| G1.d procmacros | 全部，无类型 | 无，按构造即不卫生 | 高 | 编译器即库 + crate 边界 | 可解释性预算 |

## G2 — 一等类型与反射

本点的问题：除了数据已经携带的标签之外，类型在运行期是否作为值或元数据记录存在？这决定了在运行期与编译期「类型层问题」各指什么，也约束 H3 的表示选择。

### G2.a — `types-as-values`：类型作为运行期值（Julia 风格）

类型是普通的运行期值，函数可以把它们作为参数，dispatch 可以按它们做键。这是动态多分派（research/05 §1.10、§2.4）的自然基础。

```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
fn describe(T: type) -> String { T.name }        // a type is an ordinary runtime value
let t = typeof(p)                                // runtime reification of p's type
```

- 相对于未装箱槽位的成本：要么每个槽位都携带一个类型词，即带标签的值，它与 research/03 概要第 6 条的未装箱带类型寄存器表示冲突，并把概要第 3 条删掉的那套类型守卫机械原样搬回来。要么只有装箱的 `any` 值携带类型词，静态槽位保持未装箱。后者是自洽的，但那样它就是 G2.d 加上静态那一半的 comptime 反射：运行期类型值只在装箱路径上增加表达力。
- 性能前提：Julia 自己的证据说瓶颈在推断失败，不在 dispatch（LU 案例研究：朴素写法 12 GB / 4.15 s，改成推断友好的写法后降到 0.4 s，见 arXiv 1808.03370）。一门静态类型语言重新引入运行期类型值，就重新引入了它本来没有的类型不稳定性悬崖（research/05 §1.10、§2.4）。
- 学习：低。编译器：类型对象、方法表，以及用来恢复静态可证明事实的推断。运行期错误：除非对集合类型的子句加上静态穷尽性检查，否则「找不到方法」/歧义会挪到运行期（research/05 §2.4）。
- 排除：纯静态的叙事、无条件未装箱的泛型路径，以及 dispatch 默认待在编译期的那份全性。
- 先例：research/05 §1.10、§2.4、§9 组合 E。Julia dispatch 研究（arXiv 1808.03370）。

### G2.b — `comptime-reflection`：只在 comptime 反射

`typeof`、字段枚举与构造器枚举只存在于编译器内部，没有任何形式的运行期元数据。运行期的类型问题由数据已经携带的标签回答（G2.d 的姿态）。编译期的类型问题由反射回答（本选项的姿态）。

```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
comptime {
  let info = typeof(Point)          // fields, their types, constructor cases — compile time only
  for f in info.fields { ... }      // drives derives, serializers, schemas (G1.b's engine)
}
```

- 性能：零运行期成本，没有元数据词，没有表，VM 里没有反射 API。这是 C++26 P2996 / Zig 的姿态（research/05 §4.1 第 3 级、§4.3）。
- 诚实的边界：由运行期输入驱动的序列化器（REPL 检查器、在运行期选定的线格式）无法查询反射，它改用标签/match 或证据对象。DreamScript 范围里调研到的每个运行期反射用例（derive、JSON、schema）都限于编译期（research/05 §4.3）。
- 排除：面向用户的类型对象、运行期类型内省 API、由运行期驱动的类加载。
- 先例：research/05 §4.1 第 3 级、§4.3。Zig `@typeInfo`。C++26 P2996。

### G2.c — `metadata`：运行期元数据表（Java/C#）

每个值，或至少每个装箱值，携带一个指向元数据表的类指针。反射 API 读它，运行期转换对照它检查。

```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
let t = runtime_type_of(p)                 // every value (or box) carries a class pointer
for f in t.fields { print(f.name) }        // runtime metadata table behind it
if p is_runtime Point { ... }              // checked against the table, at runtime
```

- 在性能章程下的诚实成本核算：装箱对象上的元数据词（每次分配都要付内存税）、对「反射必须看到的东西」施加统一装箱布局的压力（直接违反 research/03 概要第 6 条的未装箱带类型寄存器与未装箱标签加载荷 ADT）、反射驱动的路径变成慢路径（Go `reflect.DeepEqual` 的经济学，见 research/05 §5.1），以及错误从编译期挪到运行期（章程：编译期错误优先于运行期错误）。Java 的「擦除泛型加反射」组合就是那个警示性的配对（research/05 §1.7、§6.2）。
- 它买到什么：带丰富错误信息的运行期检查转换、晚绑定插件加载、不需要代码生成的序列化框架。DreamScript 的替代品：动态检查用携带证据的盒子，comptime 序列化器（G1.b/G2.b），插件用一等模块（B6 片段）。
- 排除：如陈述的未装箱槽位表示（research/03 概要第 6 条）、无元数据的栈帧布局，以及解释器「C 的 3-10 倍」基线的前提。
- 先例：research/05 §1.7（C# 具化对 Java 擦除）、§5.1（Go 反射那一行）、§6.2。

### G2.d — `tags-as-data`：只有标签，其余一切都在 comptime

在 docs/zh/design/01 的 B1c 混合方案下，并集就是普通数据：值的运行期标签就是它的类型身份，名义标签按集合论方式组合，`is` 测试加 `match` 覆盖所有运行期类型层问题。没有类型对象，没有元数据表，运行期没有反射，其余一切都在 comptime（G2.b）。

```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
let s: Shape = Circle(r: 1.0)              // a union value: an ordinary tag + payload
if s is Circle { ... }                     // the tag test IS the type test — no type object
match s { Circle(r) => ..., Square(w) => ... }   // exhaustive over the set algebra
// everything deeper (fields of Point, case lists) is comptime: G2.b's reflection
```

- 这一点来自 docs/zh/design/01，本文件不新增决策：一旦 B1c 让标签成为普通的数据表示，「这个值在运行期是什么类型」就正好是「它携带哪个标签」，数据表示免费回答了。在它之上再加类型对象层，就是用第二套更慢的系统复制标签系统。
- 成本：无，它就是一个特性的缺席。排除：面向用户的类型对象与运行期内省 API，与 G2.b 相同，此外再无别的。
- 先例：research/05 §2.1（集合论成员资格即类型测试）、§6.3（标签作为内联缓存键）、docs/zh/design/01 B1c。

### G2 对比表

| 选项 | 运行期表示 | 性能成本 | 错误 | 覆盖 | 排除 |
|---|---|---|---|---|---|
| G2.a types-as-values | 类型词 / 类型对象 | 带标签槽位或仅装箱 | 可能出现运行期 dispatch 错误 | 完整运行期 + comptime | 未装箱槽位（概要第 6 条） |
| G2.b comptime-reflection | 无 | 零 | 编译期 | 全部编译期需求 | 运行期内省 |
| G2.c metadata | 逐对象类指针 | 内存 + 装箱压力 + 慢路径 | 运行期 | 运行期 + comptime | 未装箱槽位、性能章程 |
| G2.d tags-as-data | 只有标签（本来就在） | 零 | 编译期（match 穷尽） | 仅运行期标签测试 | 类型对象 |

### G2 与其他设计轴的交互

- 与 B6：存在类型的证据词是多态在运行期的门面，按标签做键（research/05 §6.3）。G2.a 会用一层用户可见的类型层复制它，G2.b/G2.d 保持一套机制。
- 与 H3：内联缓存按标签/类型 id 做键。G2.a 的类型值会成为同一信息的第二个键空间。
- 与 G1：G2.b 是 G1.b 的 comptime 代码所读取的引擎。G2.a/G2.c 对 derive 的支撑比编译器内部反射更差。
- 推荐：采用 G2(b)，只在 comptime 反射，它的运行期一半是 G2(d)。(d) 由 docs/zh/design/01 的 B1c 混合集合类型零成本推出，所以合并后的推荐读作「在标签即数据之上只做 comptime 反射」：不存在运行期元数据，标签回答运行期问题，反射回答编译期问题。

## H3 — 泛型执行

分解的 (b) 轴：B6 决定了实例如何被找到，H3 决定这些调用在带类型寄存器 VM 里如何执行。所有选项都假定 research/03 的基线（未装箱的带类型寄存器，没有守卫）。差别在于实例化、证据与缓存各自住在哪里。

### H3.a — `evidence-ic`：一份泛型函数体 + 证据词 + 内联缓存，JIT 惰性特化

每个泛型函数在字节码里恰好表示一次，并附带它的约束列表。在泛型调用点发射 `CALL_GENERIC fn, evidence_words`。每个证据词要么是静态消解的实例引用（接受者类型已知，是常量操作数），要么是按运行期标签做键的运行期消解槽位。解释器在首次执行后为每个调用点打上 1 到 2 条目的内联缓存，即 Self/V8 风格的多态内联缓存（Ungar、Chambers、Patterson，ECOOP 1991）。缓存未命中回落到完整查找，所以正确性从不依赖缓存状态（research/03 §2 第 1 条对 dyn 调用点说的是同一件事）。JIT 层按具体类型向量惰性物化逐类型特化，它们缓存在函数的实例化表里，并在内存压力下被丢弃，这就是 Julia 风格的按需去虚化（research/05 §6.1-6.3，research/03 §8 的立场：字典作为未装箱值，数值多态操作走按表示形式的代码）。

```
-- generic function, stored once (illustrative IR, not surface syntax)
fn map[T: Eq, U](f: fn(T) -> U, xs: List[T]) -> List[U]
  ; constraint words: [Eq_T_slot]                 -- resolved per call site

CALL_GENERIC map, [static_ref: eq_instance_int]   ; statically known T: constant operand, no lookup
CALL_GENERIC map, [dynamic: tag_of(x)]            ; dynamic T (set-typed value): IC keyed by tag
IC_PATCH map@42: { tag_list -> callee, expected_shape }  ; patched in place after first execution
```

- 这满足反特化立场吗？两边都论证一下。是的一侧：哪段代码被语义选中在任何缓存之前就已确定，也就是 B6.a 的唯一不相交实例。缓存只是性能提示，不承担正确性职责，过期或冷的缓存回落到查找，行为不会改变。先例是 .NET 的逐值类型刻印，它在语义层不可见。C++ 的偏特化则是源码层可见的优先级规则（research/05 §1.7、§6.4）。否的一侧：运行期确实维护逐类型的编译变体，即一份单态化的运行期缓存，首次执行的时序与稳态不同，调试器会显示哪个变体跑了。如果将来的特性让特化变得可观测（对实例化的内省、逐实例化的用户代码），不变式会从工具一侧被侵蚀，即使语义从未改变。裁定：兼容。所依赖的不变式是「分派由类型和集合代数决定，从不看声明顺序或优先级。运行期可以缓存并特化任何已经唯一的分派」（research/05 §6.4）。这与 research/03 §8 在「编译器内联泛型调用」（不可见，无需重叠规则）与「语言级特化」（被拒绝）之间画的区分相同。
- 为什么解释器层仍然快：大多数调用点上类型是静态的，所以证据是常量操作数，「缓存」是空的，因为没有任何东西需要查找。内联缓存只在集合类型/动态路径上才有意义，而在那里不相交实例头的保证意味着缓存永远不需要优先级决胜（research/05 §6.3）。Julia 的 10 倍差距来自推断失败，DreamScript 的静态类型把它删掉了（arXiv 1808.03370，research/05 §6.2）。
- 权衡。学习：零，没有用户可见的概念。性能：在 JIT 内联/折叠之前走证据间接调用，动态路径上缓存命中是一次比较。语法糖：无。编译器：证据构造、内联缓存打补丁、JIT 层的实例化表。
- 排除：语言层什么都不排除，这正是重点。它把 VM 固定到证据词调用约定上（泛型调用携带额外的寄存器参数）。
- 先例：research/05 §6.1-6.4。research/03 §2（dyn 内联缓存）、§8、表 6（"compiler inlining of generic calls; no language-level specialization — best: mono perf, dictionary semantics, no overlap rules" 那一行）。ECOOP 1991 的多态内联缓存。arXiv 1808.03370。

### H3.b — `mono-cache`：字节码级单态化缓存

泛型函数的每个实例化都有自己的已编译字节码体，在该实例化首次执行时物化，并缓存在逐函数的实例化表里。这是 H3.a 留给 JIT 层的做法在解释器层的版本。

- 收益：共享路线里最好的 tier-1 性能。直接调用，物化时证据被折叠掉，整个函数体都是按表示形式专用的操作码（research/03 §1 的带类型操作码纪律，按实例化逐次施加）。
- 成本：每个新实例化在首次调用时有一次物化停顿（贴近启动的延迟，Julia 正是把它放在 JIT 层，好让解释器层保持统一，见 research/05 §6.1）。字节码大小按 O(实例数) 增长，并且在动态加载的模块之间没有上界。淘汰策略（丢弃冷的实例化）变成 VM 可见的行为。
- 排除：一份函数体的大小经济性、用于 mmap 缓存的稳定且与实例化无关的字节码产物（research/03 §1 的 dump 故事在函数体倍增之后会复杂化），以及语义上远离实例化策略的可能（内存与时序取决于那张表）。
- 先例：research/05 §6.1（单态化的利弊）、§6.2（Julia 的按需特化）。.NET CLR 的逐值类型实例化（Matt Warren 的回顾，经由 research/05 §1.7）。

### H3.c — `erasure`：全量擦除并装箱（Java）

每个泛型函数一份函数体，所有类型参数被擦除，每个泛型值装箱成统一表示，dispatch 走运行期查找。

- 与运行时基线的直接冲突：research/03 概要第 6 条承诺带类型寄存器存放未装箱的 i32/i64/f64，ADT 是未装箱的标签加载荷对。擦除为流经泛型代码的每个原始值重新引入装箱开销，让巨型多态 dispatch 变成常设的成本中心（research/05 §6.2 的 Java 那一行），并在被调用者静态未知时阻断逃逸分析与内联（research/03 §6.2）。这正是那套机械：章程的性能要求要建它，静态类型系统说根本不需要它。
- 它买到什么：尽可能简单的 VM（一份函数体、统一槽位、没有证据 ABI）和 Java 的熟悉感。章程的超高性能要求拒绝这笔交易。
- 排除：未装箱槽位表示、未装箱的泛型数值，以及保证 TCO 友好的泛型调用（无证据的 dispatch 对 TCO 没问题，装箱会破坏它）。
- 先例：research/05 §1.7、§6.2。research/03 概要第 3/6 条、§8。

### H3.d — `comptime-mono`：只在 comptime 单态化（Zig）

每个实例化在执行之前就存在，完全没有运行期多态，也从不出现证据词。

- 与带类型寄存器 VM 的交互：字节码在所有选项里最短，寄存器未装箱，操作码完全按表示形式专用，没有证据 ABI，没有缓存（research/03 §1 无例外地施加）。但 VM 从来看不到泛型代码，所以加载/首次使用要为每个实例化付账，无论冷热。启动成本是完整的实例化扇出，字节码大小是所有选项里最差的，也没有冷实例化剪枝，因为没有惰性层可剪。
- 在解释器里，这是策略上严格更差的 H3.b：立即物化取代惰性物化，不共享，不淘汰。它是 B6.d 的自然执行故事，除此之外是个糟糕的独立选择（research/05 §9 组合 D：「编译期在运行期可见」是列出的风险）。
- 排除：证据多态的动态路径（携带运行期消解证据的集合类型值）、H3.a 的惰性 JIT 层设计，以及实例化密集程序上的启动延迟控制。
- 先例：research/05 §4.1 第 3 级、§6.1、§9 组合 D。Zig 的编译模型。

### H3 权衡表（启动 / 字节码大小 / 去虚化延迟）

| 选项 | 启动成本 | 字节码大小 | 去虚化延迟 | Tier-1 稳态 | JIT 上限 | 语义可见？ |
|---|---|---|---|---|---|---|
| H3.a 证据 + 内联缓存 | 无 | 最小（每个泛型函数一份函数体） | 每个热实例化首次热 JIT 编译（惰性） | 证据间接，缓存命中接近直接调用 | 高（折叠证据、内联） | 否，缓存不承担正确性职责 |
| H3.b 单态化缓存 | 每个新实例化首次调用时停顿 | O(实例数) | tier 1 为零 | 共享路线里最好 | 高 | 物化时序/内存可见 |
| H3.c 擦除 | 无 | 最小 | 从不，永久间接 | 最差：装箱 + 巨型多态 dispatch | 没有推测机械就有上限 | 否 |
| H3.d comptime 单态化 | 加载/首次使用时全部实例化（立即） | 最差（完整扇出） | 零 | 最好（完全没有证据） | 最高 | 实例化就是语义 |

### H3 与其他设计轴的交互

- 与 B6：B6.a/B6.b 产生 H3.a 消费（H3.b 折叠）的证据词。B6.d 产生完全单态化的代码，对它来说 H3.d 是唯一自洽的故事。B6.a + H3.b 也自洽（唯一实例的逐实例化函数体），但要为一份函数体已经提供的经济性付启动成本。
- 与 docs/zh/design/03（E1d）：证据词 dispatch 与处理器/效应 dispatch 要复用的是同一条路径。H3.a 一次建好这套基础（research/03 §8 立场 3，docs/zh/design/03 的 B5d 行）。
- 与有保证的 TCO：尾位的证据传递泛型调用正常尾调用（证据词是普通寄存器参数，research/03 §4 的栈帧复用 TCO 不受影响）。
- 推荐：采用 H3(a)，即 tier 1 里一份泛型函数体加证据词加多态内联缓存，JIT 层用惰性的逐类型特化表（也就是 H3.b 的那张表，惰性填充，放在它划算的那一层）。这是 research/03 §8 陈述的立场，也是表 6 评分最高的那一行。

## 型变与 HKT

- 按集合包含的型变：对不可变容器，`Dog <: Animal` 推出 `List[Dog] <: List[Animal]`（作为值的集合）。协变由语义子类型直接给出，无需标注，线性性也不会破坏它（把 `Box[Dog]` 这个仿射 dog 强制转换成 `Box[Animal]` 仍然是一个值，前提是这次转换不能复制或丢弃内容，不可变数据保证了这一点，见 research/05 §7.2）。
- 正好在存在内部可变性或唯一借用逃逸的地方取不变：读角色协变，写角色逆变，同时具有两种角色的字段不变（Rust 的 `&mut T`/`Cell[T]` 规则在这里由集合代数推出，无需背诵，见 research/05 §7.2）。用户只在写唯一可变容器时才遇到型变词汇。
- HKT 延后：Kotlin 在没有 HKT 的情况下交付并繁荣（KEEP-87 自 2017 年起停滞，Arrow 放弃了模拟编码）。Swift 十年没有 HKT，用不透明类型加主关联类型（SE-0358）填上了缺口。Rust 的部分 HKT（GAT）被它自己的倡议记录为「太难学」（research/05 §8.2）。经典驱动力是在容器形状上抽象 `map`/`>>=`，它在 DreamScript 声明的风格下消失：`option.map`、`result.and_then`、`list.map` 作为具体方法，效应通过能力承载，单子塔不出现（research/05 §8.2）。
- 未来的效应行需要什么：如果 docs/zh/design/03 的 E1d 有一天升级为表层效应行（B5d 的 rows-later 那一半），效应多态签名将需要行多态或显式能力参数化。H3(a) 建起的证据 dispatch 基础，正是那些行会复用的同一条路径（research/03 §8，docs/zh/design/03 的 B5d 交互行）。这是 HKT 形状的机械唯一可能回来的地方，推迟到 E1d 升级，届时重新考虑。
- 决策：v1 交付带集合论约束的类型参数、关联类型，以及按集合包含的型变。v1 不含 HKT 与参数化关联类型（GAT）：Hylo 的借用约定设计在没有它们的情况下覆盖了「借出式迭代器」形状的需求（research/05 §8.1）。

## 线性交互（按路径引用 docs/zh/design/02，此处不重新决策）

- 泛型作用于不可复制值：`~Copyable` 式的取反约束声明一个可以不可复制的参数，于是一份泛型函数体同时服务可复制的 `T` 与仿射的 `T`，也就是 `fn push[T: ~Copyable](s: &mut Stack[T], x: T)`，即 Hylo 的模型（research/05 §7.1）。所有权风格是 C1(b) 仿射核心加可选线性层（docs/zh/design/02-linearity-ownership.md），本文件不重新决策它。
- 参数约定承载线性性，类型本身不承载：借用/消费/修改三种参数模式（C2(d) 二等参数模式，docs/zh/design/02-linearity-ownership.md，Hylo 的 `let/set/inout/sink`）让泛型签名说明它如何使用每个参数，同时把地址类型挡在值语言之外（research/05 §7.1）。
- 本文件继承的后果：`==` 取两个借用，从不消费操作数，所以比较仿射值后它们仍是仿射的，而「拥有的仿射数据上的 derive 相等」只做读取（research/05 §5.2 第 4 条断点）。仿射值上的容器操作需要借用，或者「消费并返回」的约定，Perceus 风格的复用会在 `tail` 即将死亡时把 `cons(h, tail)` 变成原地节点覆写（research/05 §7.1，research/03 概要第 8 条）。
- B6/H3 在其之上追加什么：仿射类型上的实例头是普通集合，所以不相交检查器纯粹在类型层，不受线性性影响。证据词本身可以是仿射的（一次性能力就是作为证据传递的仿射值）。H3(a) 的证据调用约定像任何其他参数一样在寄存器里传递证据，所以 TCO 与栈帧复用不变。

## derive（按 docs/zh/design/01 B9 归属本文件）

- 在 docs/zh/design/01 B9(a) 的通用结构化 `==` 之下（带上 research/05 §5.2 的五条例外：函数在静态上被拒绝，浮点按位结构比较并另具名 `is_float_eq`，可变单元/引用单元按同一性比较，`==` 取借用，哈希必须同时提供），相等由语言直接提供，derive 不参与。因此 `Eq` 只会在作者之后重新选择 B9 时才出现在内建 derive 列表里。trait 中介的相等所生成的 MoonBit `builtin/tuple_eq.mbt` 逐元数实现样板，被结构化选择整个删掉（research/05 §5.1-5.3）。
- 内建 derive（编译器合成，每个一个属性，没有宏系统，因为由编译器做所以错误信息好）：`Hash`（规范位哈希，与结构化相等一致）、`Show`、`Ord`（按字段顺序字典序，跟随 Rust/Kotlin/Swift，排序键必须显式提供 `total_compare` 式的浮点变体，遵循 MoonBit 的先例，见 research/05 §5.3）、`Clone`（深拷贝，它的意义来自仿射所有权下复制是显式的）、`ToJson`/`FromJson`（serde）、`Default`。
- 在推荐的 G1(b) 下用户 derive 如何表达：基于静态反射的 comptime 函数枚举字段/分支并拼装成员。research/05 §4.3 验证过对任意 struct 做 JSON 充分的那套 Zig `@typeInfo` + `inline for` 配方。内建 `#derive` 属性存在，是为了让热点情形只有一个词长、编译得快（最小充分集的需求 (4)）。
- MoonBit 证据（本地只读树 `C:/programs/me/DreamScript/moonbit-core/`）：`#derive(Eq, Hash, Debug, ToJson)` 在各包中密集使用（例如 `path/path.mbt`）。默认方法体加上记录在 `builtin/traits.mbt` 里的 NaN/IEEE 相等注意事项。`builtin/tuple_eq.mbt` 里手写的逐元数元组实现，展示了结构化相等删掉了什么（research/05 §5.1、§5.3）。
- 排除：derive proc 宏（G1.d）、反射驱动的运行期序列化器（G2.c），以及带外代码生成（G1.a 的逃生口）。

## 命名的组合

一个组合为每个决策点挑一个选项。下面三个覆盖了可信空间。research/05 §9 的组合 A-E 映射到这三个，加上被拒绝的动态 dispatch 方向（E）。

### 组合 1 —「Coherent-traits」：B6a + G1b + G2b + H3a

```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
// 1. polymorphic container API over affine elements (B6.a + ~Copyable bound)
type Stack[T] = { items: List[T] }
impl[T: ~Copyable] Stack[T] {
    fn push(&mut self, x: T) { self.items = cons(x, self.items) }  // consume-and-return
    fn pop(&mut self) -> Option[T] { ... }
}

// 2. trait with disjoint instance heads (B6.a)
trait Json { fn to_json(&self) -> String }
impl Json for Int  { fn to_json(&self) -> String { ... } }
impl Json for Bool { fn to_json(&self) -> String { ... } }
impl[T: Json] Json for List[T] {                     // conditional head: an intersection arrow
    fn to_json(&self) -> String { "[" + self.map(|x| x.to_json()).join(",") + "]" }
}
// impl Json for Int | Bool { ... }                  // REJECTED: head intersects both above

// 3. derive + comptime reflection (G1.b, G2.b)
@derive(Hash, Show, ToJson)                          // one word, no macro system
type Point = { x: Int, y: Int }
fn schema(comptime T: type) -> String {              // user derive-like via reflection
    var s = "{"
    for f in fields(T) { s += f.name + ": " + type_name(f.type) + ", " }
    return s + "}"
}

// 4. execution (H3.a): one body per generic fn + evidence words
let p  = max(p1, p2)            // evidence: static ref to impl Ord[Point] — constant operand
let js = p.to_json()            // dynamic site: IC keyed by tag -> the unique impl
```

- 结论：推荐基线。唯一同时满足四条硬性要求的组合（集合论类型、不重叠、线性友好、字节码原生，见 research/05 §9 打分表），而且每个组件都有独立的生产证据：MoonBit 的 derive、Swift 的合成、Rust 的证据传递 `dyn`、Julia 的内联缓存式缓存与惰性特化、Roc 的结构化相等、Scala 的受限消解作为逃生口。
- 对 docs/zh/design/01 的影响：B1c 混合方案是承重的，实例头是标签集合，不相交性在集合代数里检查。B9a 结构化 `==` 删掉 Eq 的 derive 与元组元数样板。对 docs/zh/design/02：`~Copyable` 约束加 C2(d) 参数约定承载仿射泛型。证据词是可借用的普通值。对 docs/zh/design/03：E1d 的处理器运行期与 B6.a 的证据 dispatch 共享一套基础。B5d 的错误并集经同一套标签机械降级。对 docs/zh/design/04：表层需要 trait/impl/derive/comptime 记号，不需要宏语法。不相交错误的措辞（「这些实例头在 X 处相交：用取反精化，或者合并」）是 04 应当预留预算的 UX 决策。

### 组合 2 —「Explicit-evidence」：B6b + G1b + G2b + H3a

```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
// 1. polymorphic container API — evidence is an explicit parameter
type Eq[T]   = { fn equal(&T, &T) -> Bool }
fn eq_int(): Eq[Int] = { fn equal(a, b) { a == b } }
fn eq_pair[T, U](e1: Eq[T], e2: Eq[U]): Eq[(T, U)] =
    { fn equal((a1, b1), (a2, b2)) { e1.equal(a1, a2) && e2.equal(b1, b2) } }

fn contains[T](xs: &List[T], x: &T, eq: Eq[T]) -> Bool {   // capability as parameter
    for y in xs { if eq.equal(y, x) { return true } }
    false
}
contains(xs, &needle, eq_pair(eq_int(), eq_int()))          // fully explicit
contains(xs, &needle, _)                                    // sugar: unique-instance insert

// 2. JSON serializer with derive + reflection (G1.b, G2.b)
@derive(ToJson) type Point = { x: Int, y: Int }
fn json_fields(comptime T: type) -> Int { var n = 0; for f in fields(T) { n += 1 }; n }

// 3. "disjoint heads" become coexisting evidence values — no global rule needed
let fast_eq = eq_int()
let logged_eq = { fn equal(a, b) { log("eq"); a == b } }    // two Eq[Int] alive at once
contains(xs, &needle, logged_eq)                            // caller picks, visibly
let j = any_json(box Json(p))                               // existential: value + evidence
```

- 结论：自洽，而且最「无魔法」（research/05 §3.4 的回答是肯定的，附带样板代码的注意事项）。运行期表示与组合 1 相同，所以性能相同。代价是 API 噪音：每个多态函数都长出一个证据参数，而且 `_` 语法糖悄悄把受限消解又引回来，到那时 B6.a 的不相交实例头搜索是同一便利性的更有原则的版本。只有当零隐式搜索对作者是终局价值时才选它。
- 对 docs/zh/design/01 的影响：无，完全兼容。对 docs/zh/design/02：三者中线性契合最好（能力是普通仿射值，仿射流能看到每一次使用）。对 docs/zh/design/03：E1c 的能力惯用法推广到全部多态，预先验证了 E1d 的运行期基础。对 docs/zh/design/04：没有与消解相关的语法，但调用点携带证据参数，这对表层形成朝 `_` 语法糖的压力，这是 04 的一个显式决策，需要写明规则（永远歧义即错误）。

### 组合 3 —「Comptime」：B6d + G1b + G2b + H3d

```
// illustrative; surface syntax decided in docs/en/design/04-syntax.md
// 1. polymorphic container API — generics are comptime functions
fn Stack(comptime T: type) type {
    return struct {
        items: [T], len: Int,
        fn push(&mut self, x: T) { self.items[self.len] = x; self.len += 1 }
        fn pop(&mut self) -> Option[T] { ... }
    }
}

// 2. JSON serializer — derive via type reflection (G1.b, G2.b)
fn Jsonify(comptime T: type) type {
    return struct {
        fn to_json(self: &T) -> String {
            var s = "{"
            inline for f in fields(T) { s += "\"" + f.name + "\":" + render(self, f) + "," }
            return s + "}"
        }
    }
}

// 3. "trait impl with disjoint heads" has no analogue — bodies are generated per type
impl Json for Int { ... }            // i.e., Jsonify(Int) is materialized at comptime
let s: Stack(Point) = Stack(Point)() // each instantiation is its own bytecode body
let js = Jsonify(Point).to_json(&p)  // zero runtime polymorphism; all cost at load
```

- 结论：稳态性能最好，derive 的写法最短，但整体契合最差：comptime 生成的实例逃出集合代数，所以不相交性无法强制（research/05 §9 组合 D）。线性检查器必须在生成的代码上运行，这会增加一个真实的检查趟（Zig 没有做）。comptime 与运行期的边界是要学习和调试的第二套类型系统。解释器要在启动时付物化成本。而且语义上是模板实例化，与反特化立场是混合契合。采用它的反射那一半（G1.b/G2.b），它的泛型那一半被舍弃，这正是组合 1 所做的。
- 对 docs/zh/design/01 的影响：集合代数从消解的基础降级为 comptime 可以绕过的可选检查工具，削弱 B1c 的保证。对 docs/zh/design/02：线性性必须在展开后检查，这是一个新的检查趟，成本真实。对 docs/zh/design/03：comptime 之下的效应需要显式穿线，否则失去 E1d 的基础。对 docs/zh/design/04：comptime 成为语言的重心，并把语法糖预算花在第二个执行阶段，表层易用性拿不到这笔预算。

### 跨组合解读

组合 1 与组合 2 共享整个运行期（证据词、内联缓存、comptime 反射、内建 derive），只在消解策略上不同，所以组合 1 之后可以采用组合 2 的命名受限实例，而不必重铺 VM。组合 3 与它们共享 G1b/G2b，但在 VM 处分叉（完全没有证据 ABI），所以在 tier 1 已经围绕证据词建好之后再选它就是重写。这就是 B6 与 H3 应当作为一个动作一起决策的原因。

## 本文件留给作者的开放问题

- 实例头不相交检查的确切算法，以及它点名见证的错误信息措辞（research/05 §9 开放问题 1）。
- 递归条件实例的终止规则（Modus Ponens 演算给出了形状，具体检查是开放的，见 research/05 §2.3、§9）。
- 受限命名实例（B6.b-lite）是在 v1 交付还是延后（research/05 §9 开放问题 3）。
- `==` 之下的浮点相等选择归 docs/zh/design/01 B9 负责，Roc 的结构化/IEEE 拆分对 MoonBit 的 IEEE 加有记录前置条件，输入在 research/05 §5.2。
- comptime 反射是在 v1 交付，还是内建 derive 足以支撑直到需求出现（research/05 §9 开放问题 5）。
