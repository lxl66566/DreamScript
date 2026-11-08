# 05 — 多态与元编程

DreamScript 设计的研究文件，覆盖多态的各种形式、带重载的集合论类型、反特化约束、编译期元编程、derive 与相等性的故事、字节码里的性能表示，以及与线性类型的相互影响。写于 2026-11-04。证据来自一手文档、论文，以及本地可用的两份源码树（仓库根目录下的 `moonbit-core`、`koka` 克隆，只读）。姊妹文件：01（线性性/所有权）、02（效应）、03（运行时/IR/JIT）。假定的 DreamScript 输入：解释执行（字节码 IR，可选 JIT）、静态线性/仿射类型、默认不可变、集合论类型且泛型匹配不重叠、没有 C++ 风格的特化、语法糖天花板 = Kotlin、Koka 风格 UFCS、单子的 Option/Result。

## 概要

特化是作者拒绝的东西，证据表明拒绝它的代价很低：重叠实现是特化存在的唯一理由，Rust 却花了十年都没能让重叠实现变得可靠。RFC 1210 于 2015 年被接受，跟踪 issue rust-lang/rust#31844 在 2026 年仍然开着，上面写着 "can cause undefined behavior without unsafe code"，就连 `min_specialization` 也在 2025 年被 issue #149257 再次报告为不可靠。一门泛型实例不可能重叠的语言，永远不需要挑出「最具体」的匹配。因此它既不需要特化，也不需要优先级规则，更不需要一致性律师。

每种多态机制都拆成两个正交的决策：(a) 实例如何被找到，取隐式全局消解或显式局部传递。(b) 调用如何执行，取单态化、字典/vtable 传递或带缓存的动态分派。Rust 与 Haskell 的痛点在 (a) 轴：全局隐式消解由孤儿规则强制。(b) 轴没有这种痛点。这条分解是本文件的主要结构性洞见。第 3 节按 (a) 轴评估每个决策点，第 6 节按 (b) 轴评估。

如果要求重载两两不相交（在定义时检查 `T1 & T2` 为空集），集合论类型就能承载重载，组合时没有歧义。这一点有形式化依据。"Resolution as Intersection Subtyping via Modus Ponens"（OOPSLA 2020）表明实例消解就是交集子类型。MLscript/MLstruct（布尔代数类型，OOPSLA 2022）明确把类型类列为兼容的扩展。不相交交集的合并通过 apartness 保持一致（OOPSLA 2025）。Castagna 的语义子类型这条线（CDuce、POPL 2015、Elixir 类型系统）已经在生产语言里交付了基于集合的重载消解。

derive 问题有一个符合反特化立场的廉价答案：不可变数据上的结构化相等性除了函数、浮点数、循环结构与线性资源之外是全的。证据有四条。Roc 完全没有 derive 也交付了结构化相等性，IEEE 语义另设 `is_float_eq`，已在 builtin 文档核实。MoonBit 交付 `#derive(Eq, Hash, Show, ToJson)` 属性，在 moonbit-core 里随处可见，其 `builtin/traits.mbt` 还记录了 derive 相等性上的 NaN 坑。Swift 从声明合成 `Equatable`/`Hashable`（SE-0185）。OCaml 交付通用多态结构化比较。

元编程：覆盖所有 derive 式需求的最小集合是 (1) 对类型结构的静态反射（字段/分支），(2) 对普通代码的编译期求值（Zig `comptime`、D CTFE、Mojo `comptime`），(3) 针对热点的内建 derive 属性。token 宏（Rust proc 宏、Nim、Lisp）是更重的机械，它们存在的主要原因是那些语言缺少静态反射。C++ 正在收敛到同一结论（C++26 静态反射，P2996）。基于反射的元编程还彻底删掉了卫生问题。

解释型语言的性能做法：绝不对字节码做单态化。保留一份泛型函数体，把证据（字典/vtable）作为参数传递，在调用点用内联缓存缓存消解（Self/V8 风格），并在 JIT 层按类型惰性物化特化（Julia 风格的去虚化）。这是编译器内部的特化，精确保持可观察语义。反特化立场针对源码层语义，即一次调用指向哪个实现。优化器的记账属于实现细节，两者因此兼容。

HKT 可以推到 v1 之后：Kotlin 在没有 HKT 的情况下成为主流，KEEP-87 停滞且 Arrow 放弃了模拟编码。Swift 十年没有 HKT 也照常工作（不透明类型加主关联类型），Rust 没有 trait 级 HKT（GAT 覆盖了借出迭代器这一细分需求，但被普遍认为难学）。如果效应走行/能力路线（文件 02），而 Option/Result 提供具体的 `map`/`and_then` 方法，那么经典的 HKT 驱动力就消失了。

第 9 节勾出五个一致的组合：(A) 一致的集合论 trait 加内建 derive 加带内联缓存的证据传递。(B) 完全显式的能力，零隐式消解（Effekt 风格）。(C) ML 模块/1ML。(D) Zig 风格 comptime 泛型。(E) Julia 风格动态多方法加 JIT 去虚化。与所列约束最契合的是 A，用 B 风格的受限命名实例作为扩展用的逃生口，用 D 风格的反射而非宏作为元编程基质。

## 1. 多态的各种形式：调研

每种机制都归纳成两个轴：消解规则决定编译器在哪里找到实例，执行规则决定调用如何运行。各语言的差异主要在 (a) 轴。(b) 轴的差异多半是实现策略（第 6 节）。

### 1.1 Rust trait：一致的全局类型类

Rust trait 是全局、隐式、一致的类型类：对给定的 trait 与类型对，整个程序里至多存在一个 impl。一致性由孤儿规则强制：impl 只能写在定义该 trait 的 crate 或定义 self 类型的 crate 里。这条规则附带一条已文档化的放宽，`#[fundamental]` 类型（`&T`、`Box<T>`、`Pin<P>`）不算本地类型，并且 blanket impl（`impl<T> Trait for Vec<T>`）只在参数被本地类型覆盖时才允许。消解在编译期完成：每个泛型调用点，编译器选出唯一的 impl，构造它的字典（impl 方法与父 trait 方法的函数指针记录），然后要么在单态化之后静态分派，要么把字典装进 vtable 供 `dyn Trait` 使用。

```rust
trait Draw { fn draw(&self) -> String; }
struct Button;
impl Draw for Button { fn draw(&self) -> String { "btn".into() } }

// blanket impl: coherent only because no other crate can impl Draw for Button
impl<T: Draw + ?Sized> Draw for Box<T> {
    fn draw(&self) -> String { (**self).draw() }
}

fn draw_static<D: Draw>(d: &D) { d.draw(); }   // universal: monomorphized per D
fn draw_exist(d: &dyn Draw) { d.draw(); }      // existential: vtable dispatch
fn opaque() -> impl Draw { Button }            // universal, hidden concrete type
```

返回位置的 `impl Trait` 产生不透明类型：每个返回点一个固定具体类型，静态分派。`dyn Trait` 是开放的存在类型：装箱、vtable 分派、可做动态类型检查。这与 Swift 明确区分开的 `some`/`any` 是同一回事（1.3 节）。关联类型（`type Item`）把类型族显式化，并让推断保持单值：每个 self 类型恰好一个 `Iterator` impl，于是 `iter.next()` 单凭 self 类型就能推断出 `Item`。泛型关联类型（GAT，Rust 1.65 稳定）把关联类型在生命周期与类型参数上泛化。真实用量调查显示它们主要用于借出迭代器与支持借用的集合 trait，而 GAT 计划自己的设计文档把「太难学」列为采纳顾虑，这说明即使部分 HKT 也带着真实的认知负担。

blanket impl 造成众所周知的生态死锁：一旦某个 crate 写下 `impl<T: FromStr> MyTrait<T>`，别的 crate 此后就不能写 `impl MyTrait<&str>`，因为两个 impl 重叠，而一致性禁止重叠。编译器必须保守地假设外来类型将来可能获得 impl（这个场景在 #31844 的讨论里有记录：针对 blanket 的 `impl<'a, T: FromStr> Simple<'a> for T` 特化 `Simple<'a> for &'a str` 编译不过，因为 `&str` 将来可能实现 `FromStr`）。所以一致性规则的形状是「悲观地、静态地禁止重叠」，DreamScript 的对位是「用集合代数精确地、静态地证明不重叠」。

特化的历史可以用一段叙述讲完：RFC 1210（2015）允许重叠 impl 按最具体匹配消解，被「大张旗鼓地」接受，然后停滞。卡住的可靠性问题是生命周期擦除（Matsakis 2018："code generation wants to erase regions, but the type checker doesn't"）。第二个问题是特化 impl 的谓词比它所精化的 impl 更弱，特化代码跑在违反基础 impl 调用方所作假设的值上。第三个问题是前面那条对未来 impl 的保守。2018 年的「always applicable impls」提案把可特化的 impl 限制为约束仅凭良构性即可证明的那些：每个类型参数恰好用一次、生命周期泛型、只用隐含边界。`min_specialization`（PR #68970，2020）限制得更紧。它只允许函数，关联类型与常量不允许，因为它们在类型检查与代码生成期间被求值，那里的不匹配是 UB 形状的。每个泛型参数在精化替换里至多用一次，特化 impl 的所有谓词都由基础 impl 蕴含。即便如此，issue #31844 仍说已实现的该特性 "can cause Undefined Behavior without unsafe code"，而 2025 年的 issue #149257 报告 `min_specialization` 自身就能被一个平凡为假的生命周期边界弄成不可靠。十年，两个局部设计，可靠的通用特化仍然不存在。这是作者「匹配不重叠」约束最强的一条外部证据：难点的来源是重叠加子类型加泛型，它让「哪个 impl 适用」在两个代码库的边界上实践中不可判定，实现难度本身很轻。

标准库用特化做什么很有教益：`min_specialization` 在内部用于性能精化，例如在 `T: Copy` 时为 `Vec<T>` 特化 `Clone`（实现走一次 memcpy，逐元素克隆被省掉）。也就是说，Rust 里的特化存在是为了让优化器在编译期知道一个运行期可被证伪的谓词（这个元素可复制吗？）。在字节码解释器里，同样的效果由复制操作上的内联缓存加按类型的快路径得到，不需要任何语言特性（第 6 节）。

### 1.2 Haskell 类型类：带一致性泄漏的字典传递

GHC 类型类通过全局实例表消解：对约束 `C t`，GHC 收集头部匹配 `C t` 的所有实例声明，并要求恰好一个匹配，同时提供三个逃生口，而它们全都是一致性泄漏。`{-# OVERLAPPING #-}` / `{-# OVERLAPPABLE #-}` / `{-# INCOHERENT #-}` pragma 让程序员主动选择多个匹配实例（OVERLAPPING 之下最具体者胜，INCOHERENT 之下任意挑选）。孤儿实例（为类 `C` 与类型 `T` 声明的实例，落在一个既没定义 `C` 也没定义 `T` 的模块里）让实例的可用性取决于导入图，同一个程序行为可以随链接了哪些模块而不同。Safe Haskell 在 `Safe` 模块里明确禁止孤儿实例与 `INCOHERENT`，因为它们破坏安全保证。HaskellWiki 的孤儿实例页面推荐一种约定，把孤儿实例隔离进专门的 `Orphan()` 模块，这样就能一起禁止它们被导入。

```haskell
class Eq a where
  (==) :: a -> a -> Bool

instance Eq a => Eq [a] where        -- compiles to: eqList :: Eq a -> Eq [a]
  xs == ys = ...                     -- a dictionary-taking function

-- orphan instance: defined in a module that owns neither Eq nor MyType
instance Eq Foreign where ...
```

执行方式是字典传递：实例编译成函数记录（可能内嵌父类字典），约束变成每个多态函数上额外的字典参数，而 GHC 只在程序员的 `INLINE`/`SPECIALIZE` pragma 指导下擦除它们。字典消除因此是用户可见的性能杂务，Haskell 的性能轶事里充满了「忘了给字典 SPECIALIZE」的故事。给 DreamScript 的教训：字典传递是自然的运行期表示（证据即普通值，可组合），而带孤儿自由的全局隐式消解才是一致性的来源。GHC 交付的这些逃生口（重叠 pragma、INCOHERENT）正是作者拒绝的、特化形状的复杂度，它们的存在说明了允许重叠、却没有集合代数去证明不相交会带来什么。

### 1.3 Swift 协议：遵循关系、存在类型与合成

Swift 协议在同一张遵循表下组合了静态泛型、存在类型与关联类型。词汇：`some P` 是不透明类型（静态分派、隐藏具体类型、每个声明点一个固定类型，即带推断的全称），`any P` 是存在类型（装箱，通过见证表做动态分派，可做动态转换）。关联类型用 `associatedtype` 声明并加约束。主关联类型（SE-0358，2022）借助轻量的同类型要求（SE-0346）让协议在多数位置可以像参数化泛型那样书写（`Sequence<Element>`、`Collection<Element>`）。隐式打开的存在类型（SE-0352，2022）让 `any P` 的值流进泛型参数，程序员不必手动解包。协议的遵循关系全局记录（追溯性遵循允许，但会产生库演进告警，这是 Swift 的软孤儿规则）。

```swift
protocol Collection { associatedtype Element; mutating func next() -> Element? }

func f(c: some Collection<Int>) { }   // static dispatch, concrete type hidden
func g(c: any Collection<Int>) { }    // existential: boxed, witness-table dispatch

func firstTwo<S: Collection>(s: S) -> [S.Element] { ... }  // universal
firstTwo(c)                             // any Collection<Int> implicitly opened (SE-0352)
```

Swift 对 DreamScript 最可移植的特性是遵循关系合成（SE-0185，2017）：声明 `struct P: Equatable {}`，只要所有存储属性都遵循，编译器就合成 `==` 与 `hash(into:)`。`Codable` 用同样的方式合成 `init(from:)`/`encode(to:)`。没有关联值的 enum 自动获得 `Equatable`。条件遵循（`extension Array: Equatable where Element: Equatable`）让合成在泛型上组合。要留意的模式：遵循关系是显式的（在类型声明处主动选择，写进 API 可见），而方法体是推导出来的（没有手写样板代码，不涉及宏系统）。这个位于 Rust derive 宏与 Roc 内建结构化相等性之间的中间点，任何已经知道类型字段布局的编译器都能实现，不需要元编程机械。

### 1.4 OCaml 模块与 functor（以及 1ML）

OCaml 的第二条抽象轴是模块系统：模块是类型与值的具名捆绑，由签名约束。functor 取模块作参数并返回模块。实例是模块应用时显式传入的值。这里没有隐式消解，也没有一致性约束。`Map.Make` 用不同比较器模块的两次应用产生两个不同的抽象 map 类型（生成式 functor），因此多个实例按构造共存而不产生歧义。一等模块（把模块打包成运行期值的 `(module S)`）给出显式的存在类型：一个值连同它的字典一起旅行，需要时解包。

```ocaml
module type ORDERED = sig
  type t
  val compare : t -> t -> int
end

module Map (O : ORDERED) : sig
  type key = O.t
  type t
  val add : key -> t -> t -> t
  val find : key -> t -> t option
end = struct ... end

module StringMap  = Map (StringOrd)    (* explicit instantiation 1 *)
module ReverseMap = Map (ReverseOrd)   (* explicit instantiation 2: coexists, no conflict *)
```

Rossberg 的 1ML（ICFP 2018）证明 ML 核心语言与模块语言可以被统一：普通函数就是 functor，一等模块就是一等类型，语义是有原则的 System F 编码（"F-ing modules"）。对 DreamScript 的示范价值：显式能力传递可以扩展到一切，包括泛型、存在类型与抽象类型，且零歧义、零一致性机械。代价是语法表层：签名、functor、应用式与生成式的区别、实例化的冗长。这正是 OCaml 生态长出 `ppx` 代码生成的原因，也正是那里 derive 的故事很弱的原因（第 5 节）。还要注意 OCaml 的第三条路：为所有数据内建通用多态结构化比较 `=`（5.1 节），derive 问题在语言层解决，而不靠任何抽象机制。

### 1.5 Scala given：受限作用域内的确定性消解

Scala 3 的 `given`/`using` 是词法作用域内的类型导向隐式消解：上下文参数 `(using ord: Ord[T])` 靠搜索词法作用域来消解，搜索顺序是内部定义、导入、继承。如果同一优先级下作用域里有两个同类型的可用 given，编译器报歧义错误，不做挑选：选取是确定的，默认不重叠，`@annotation.priority` 机制只为迁移场景存在。匿名 given 得到编译器按类型合成的名字（`given_Ord_Int`）。文档推荐公开库使用具名 given，理由是二进制稳定性。条件 given（`given [T: Ord] => Ord[List[T]]`）是消解规则，所以消解是一次递归的证明搜索，在歧义即错误的前提下它总是确定性终止。

```scala
trait Ord[T]:
  def compare(x: T, y: T): Int

given intOrd: Ord[Int]:
  def compare(x: Int, y: Int) = if x < y then -1 else if x > y then 1 else 0

def max[T](x: T, y: T)(using ord: Ord[T]): T =
  if ord.compare(x, y) < 0 then y else x

given listOrd: [T: Ord] => Ord[List[T]]:   // conditional given: resolution rule
  def compare(xs, ys) = ...

// two competing givens of Ord[Int] in one scope = compile error, not a pick
```

Scala 是最接近「受限实例」的主流样式：统一性由词法确定性与歧义即错误提供，做法上放弃全局孤儿规则。同一个类型可以在不同作用域里同时有多个实例存活。历史代价是「隐式地狱」：消解依赖微妙的作用域与优先级互动。Scala 3 通过收紧作用域规则并对歧义报错把它驯服。这条教训可以照搬：当且仅当歧义是错误、优先级来自词法而非启发式时，受限消解才是一致的。

### 1.6 Kotlin：接口加扩展函数加上下文参数，没有 HKT

Kotlin 没有类型类，也没有 HKT：KEEP-87（高阶类型）从 2017 年起停滞，从未被接受，也不在路线图上。旗舰 FP 库 Arrow 在承受多年维护负担之后放弃了它的 `Kind<F, A>` HKT 模拟编码（社区编译器插件 `highKT` 作为权宜之计存在）。Kotlin 交付的是四样东西。带默认方法的接口，走 vtable 分派。扩展函数是静态消解的语法糖，`x.f(y)` 去糖为按导入作用域消解的顶层 `f(x, y)`，没有动态分派，没有方法表命中，这正是 Koka 风格 UFCS 在编译期消解的形态。data class 从主构造函数生成 `equals`/`hashCode`/`toString`/`copy`/`componentN`。上下文参数（KEEP-367，2025 年在 Kotlin 2.2 落地）把具名的上下文声明按类型注入到声明了它的函数里。这又是隐式消解，但声明处有显式名字、查找按类型确定、也没有隐式接收者的继承游戏（更早的 KEEP-259 上下文接收者在 K2 落地时被废止）。

```kotlin
data class Point(val x: Int, val y: Int)               // equals/hashCode/toString/copy generated
fun Point.distanceTo(o: Point): Double = hypot(x - o.x, y - o.y)  // extension = UFCS sugar
val d = p1.distanceTo(p2)                              // reads like a method, resolves statically

context(Logger)                                        // KEEP-367: declared capability
fun processOrder(order: Order) { log("processing ${order.id}") }
```

Kotlin 是存在性证明：一门生产力高的主流语言可以靠接口加静态 UFCS 加编译器生成成员加偏显式的上下文传递运转，零一致性机械、零特化、最高到注解处理为止的零元编程。作者设定的语法糖天花板（Kotlin 的）由一门恰好只有这份多态菜单的语言达成，这是强信号：DreamScript 要的工效学不多于组合 A+B 所能提供的。

### 1.7 C# 具体化泛型对 Java 擦除

Java 泛型是擦除的：`List<Integer>` 在运行期就是裸 `List`，元素装箱成 `Object`，不做逐元素装箱就不可能存在原语特化的泛型代码，泛型类型信息只以编译器侧检查加反射元数据的形式残留。C# 泛型在运行期具体化：CLR 的 JIT 为每个值类型实例化生成专用机器码（不装箱、布局最优的字段访问、直接调用），并为所有引用类型共享一份实例化（引用类型是指针大小，一份函数体就够，这份共享实例化就是带平凡字典的字典传递）。.NET 的设计（Anders Hejlsberg、Don Syme，微软研究院，2004）刻意把泛型放进 VM 层，由 VM 承担泛型实例化。Matt Warren 的回顾记录了另一条路线 Java 擦除被明确考虑过，并因性能原因否决。

给 DreamScript 的可移植教训：「具体化对擦除」得按实例化形状逐项决定。在字节码 VM 里，.NET 这一分裂的对等物是：值类型的泛型参数走形状专用的快路径（拆箱布局、按标签的内联缓存），引用类型的参数共享一份函数体加证据，这正是第 6 节提议的做法。两半对用户都不可见，也都不需要源码层特化。

### 1.8 C++ 模板与 concepts：反例

C++ 模板是编译期的项重写，类型系统是后来焊上去的。基于模板签名的重载消解加上显式特化与偏特化，正是特化乱局的源头：主模板、完全特化与偏特化之间的优先级规则、SFINAE 重载技巧、带整页候选列表的歧义错误。C++20 concepts 把约束变成一等谓词，让受约束候选之间的重载消解有原则可循：约束蕴含另一方的候选胜出，即通过逻辑蕴含的约束包含。包含关系仍是特化规则：重叠匹配之间的优先级排序，只是可判定的那种。C++ constexpr 把模板元编程搬进了看起来普通的代码（语法上的大胜），同时保留隐式的常量上下文规则（「这个函数能否在编译期运行」取决于实参而非标注），这正是 Zig/Mojo/D 通过把编译期阶段显式化所消除的痛点。C++26 将加入静态反射（P2996），目的正是在 derive 用例上终结模板元编程。模板发布二十五年之后，这门语言收敛到「反射，而非重写」。C++ 是 DreamScript 的参考反例：最大的力量，最大的语义税，以及一条长达二十年的稳定尾巴。

### 1.9 Go 接口：带运行期窟窿的结构化存在类型

Go 接口是结构化存在类型：一个值只要具备所需方法就满足接口：声明、一致性要求与实例搜索都不存在，1.18 之前也没有 trait 对象对泛型的分裂。接口值是 `(itab, data)` 对，itab（接口表）在值被装箱时构造。分派是按动态类型做键的表查找。Go 泛型（1.18，2022）是擦除的，配 GC 形状刻印：每个 GC 形状一份实例化加运行期字典。字面上就是字典传递，字典承载该形状无法特化的操作。Go 的零魔法哲学在学习成本上很有吸引力，但它不提供静态特化，不提供重载，也不提供 trait 级多态（`impl<T> Eq for Vec<T>` 那样的泛型实例不存在，你没法表达「我的泛型容器在其元素相等时相等」，除非在每个使用点写逐实例化样板）。它还交付了两个 DreamScript 不能照抄的相等性窟窿：接口值上的 `==` 在动态类型不可比较时运行期 panic，值类型不可比较的 map 只在比较点失败。相等性上的静态全性比 Go 省下的语法更值钱。

### 1.10 Julia 多重分派：带 JIT 去虚化的动态集合匹配

Julia 没有 trait，也没有类：方法属于泛型函数，分派匹配所有实参的运行期类型（对类型模式的多重分派）。方法表按调用搜索。JIT 在首次调用时按具体实参类型元组编译一份特化（按需单态化，每种实参类型组合都有自己的编译体，缓存在方法实例表里）。类型推断去虚化热代码：当推断在某个调用点证出具体类型时，"type checks and method dispatch ... can be resolved statically"，从而可以内联。推断失败时，例如变量被推断为 `Union{Int, Float64}`，这次推断就是类型不稳定，分派保持动态，性能崩溃。分派研究（arXiv 1808.03370）与那篇经典论文（Bezanson 等，2018）记录了 LU 案例：朴素实现 12 GB 分配 / 4.15 s，去掉临时对象后 7 MB / 0.75 s，经过对推断友好的改写与边界消除后 0.4 s。十倍差距完全由推断质量驱动，与分派机制本身无关。

```julia
area(r::Circle)    = pi * r.radius^2      # clauses are type patterns: set membership tests
area(r::Triangle)  = ...
area(s::Square)    = s.width^2            # disjoint heads: no ambiguity at runtime
```

Julia 是存在性证明：动态分派加 JIT 特化能达到 C 的速度。它同样是警告：这份速度取决于推断成功与实参类型稳定。对以解释器为先的语言，解释器侧的对等物是每个调用点的方法缓存（6.3 节）。设计风险在于分派是运行期匹配，所以「找不到方法」与歧义都是运行期错误。这违反 DreamScript 偏好编译期错误的立场，除非静态检查器对静态可能的实参集合证明全性（那正是对集合类型分支做的不相交加覆盖检查，2.4 节）。

## 2. 集合论类型遇上多态

### 2.1 集合论子类型买到了什么

如果类型就是集合，并集、交集、取反都是真实的格运算，子类型就是语义上的集合包含，那么有三件对多态重要的事情随之而来。语义子类型这条线有三个出处。CDuce、Castagna、Nguyen、Xu、Frisch 的 "Polymorphic Functions with Set-Theoretic Types"（POPL 2015）。Castagna 2023 年的专著 "Programming with Union, Intersection, and Negation Types"。MLstruct 的布尔代数子类型（OOPSLA 2022）。第一，子类型是可判定且可组合的：没有变型标注要背，`T1 <: T2` 就是一次集合计算。第二，`T1 | T2` 的值真的是 `T1` 的值或 `T2` 的值：并集就是值的集合成员关系，值本身无需包装。因此收窄是经过检查的集合运算，而分支分析在各分支交集为空、并集覆盖被匹配值时是穷尽的。第三，函数类型有意义的相交：`(T1 -> U1) & (T2 -> U2)` 是同时属于两者的函数集合，那恰恰是一个重载函数。

### 2.2 重载即箭头交集（Castagna 的 ◁ 机制）

在 CDuce/Castagna 系统里，一个重载函数的类型是 `(Int -> String) & (Bool -> Int)`，把它应用到类型为 `T` 的值上时，计算方式是拿实参集合与每条箭头的定义域求交，丢弃贡献为空集的箭头。「比……更精确」（`◁`）关系形式化了哪些箭头存活。于是重载的消解规则由类型代数本身定义，有原则且可判定，临时优先级被排除：在实参类型完全已知的静态调用点，恰好匹配的箭头留下，若留下不止一条，结果类型是它们结果类型的并集，这可靠但笼统。如果语言要求唯一性，就报错。Elixir 类型系统（Castagna、Duboc、Valim 等，"The Design Principles of the Elixir Type System"，arXiv 2111.03354）把同一套机械用于模式匹配：分支的模式就是一个类型，调用按集合成员关系匹配分支，Erlang 的「首个匹配分支胜出」语义留在运行期，而静态检查器在分支不可达时告警（被更早的分支完全包含）。于是 Elixir 接受有序重叠，并把它外显为诊断信息，这是 C++ 优先级规则与 DreamScript 计划中的不相交要求之间的务实中间地带。

```text
-- DreamScript-shaped sketch of the two regimes:
f : (Int & ~Even -> "odd") & (Even -> "even")   -- disjoint by negation: unique match, no priority
g : (Int -> "int") & (Number -> "num")          -- overlapping: g(3) matches both -> reject at definition
```

对 DreamScript 而言，「泛型实例不得重叠」这条规则在集合代数里有直接写法：在声明时对同一组内每一对实例/重载签名检查 `T1 & T2 = Empty`（空集在语义子类型里可判定），或者要求程序员像上面的 `f` 那样用取反在一侧切出边界。匹配的唯一性于是从类型代数推出，优先级机械永远不需要。重叠的错误信息可以很精确："these two signatures both accept `Even Int`; refine one with a negation or merge them"。编译器侧的成本是每个实例组一次成对空集检查，随组大小的平方增长，但实践中很小且可缓存。

### 2.3 实例消解即交集子类型（形式化依据）

"Resolution as Intersection Subtyping via Modus Ponens"（Marntirosian、Schrijvers、Thiemann、Steel，OOPSLA 2020）把类型类消解形式化为交集类型规则下的子类型。实例 `I => C` 是箭头 `I -> C`，一组实例是箭头的交集。消解约束 `C t` 是一次由肯定前件（modus ponens）复合而成的子类型判定：若实例环境的交集类型在 `t` 处被栖居，则 `t` 具备所需的实例箭头。这单独一套机制涵盖了函数重载、按返回类型重载（从上下文消解 `Read s => Read (State s a)` 式约束）与可扩展记录。对 DreamScript 重要的结构性事实是，隐式消解与集合论子类型就是同一套系统：消解字面上就是在这个实例箭头交集上做的子类型判定。唯一性于是要求实例头不重叠，而那正是已声明的约束，所以形式化故事与设计同向，两者互相支撑。

MLscript/MLstruct 这条线把该基质端到端演示了一遍：MLstruct（Parreaux、Chau，OOPSLA 2022）在结构化类型的布尔代数里给出主类型推断，并集、交集、取反都是一等公民。它的未来工作一节明确点出类型类与构造器重载是兼容的扩展，代数里没有任何东西阻挡它们：布尔代数的要点恰恰是有空间说「非」。关于不相交交集类型与合并算子的后续工作（Oliveira 及其合作者，最终成果为 Xu、Huang、Oliveira，"Liberating Merges via Apartness and Guarded Subtyping"，OOPSLA 2025）通过要求 apartness，即类型可证明不重叠，让合并保持一致。它同时放宽了早期不相交交集系统的严格性。文献的走向正是「不可能产生歧义的重载」，这与 DreamScript 的既定立场一致，并且有学术支撑。

### 2.4 多方法：Cecil 与 Julia 作为动态集合匹配

Cecil（Chambers）对所有实参做动态最具体匹配来消解多方法，调用期报 "no applicable method"/"ambiguous method" 这类运行期歧义错误。CLOS 用有序方法组合（primary/around/before/after）精化它。Julia 把它精化成带 JIT 特化的类型化方法表（1.10 节）。对集合类型语言，多方法就是一个分支即类型模式的函数（Elixir 的表述），分派是运行期的集合成员测试。这在类型层与集合类型完全兼容，但它把消解挪到了运行期，代价是结构性的而非偶然的：分派成本变成每次调用的成本，可缓存，见 6.3。静态线性性推理退化：被调方的函数体在匹配之后才确定，因此跨调用的仿射流分析需要把分支体内联进检查器，实际上就是检查的单态化。编译期全性即「每次调用都能消解」，它需要对静态可能的实参集合做穷尽性检查，即对集合类型做不相交加覆盖计算，这可以做，代价是真实的机械。多重分派可以作为可选的动态层存在：值携带自己的方法表，像一等模块那样。作为主要的多态机制，它与静态仿射类型、与「编译期错误优于运行期错误」都冲突。Julia 自己的证据显示让它变快的是 JIT，分派机制本身很快。

### 2.5 结论

对不相交的集合类型实例做静态泛型消解（2.2/2.3）一次性满足全部既定约束：集合论类型、匹配不重叠、无特化、编译期错误、有形式化依据（消解即子类型）。动态多方法如果真的需要，可以作为单独定型的特性存在。在带线性性的语言里，它们不应成为普通泛型的默认消解方式。

## 3. 反特化约束

### 3.1 特化到底为什么存在

只有当两个 impl 匹配同一个值时，特化才有意义：语言必须决定哪一个运行。去掉重叠的三条路是孤儿一致性（Rust）、歧义即错误（Scala）、集合类型上的结构化不相交（2.2 节）。此时特化就无事可做：每次泛型调用恰好一个 impl，「优化它」就只是编译。这是作者立场最直接的表述：特化的需求来源只有歧义，禁掉歧义，特化就随之消失。推论也值得注意：任何重新引入重叠的特性（通配模式、开放世界的实例表、首次匹配语义）都会悄悄重建对优先级规则的需求，所以不相交不变式必须在实例可能出现的所有地方检查：声明处、条件实例，以及 comptime 生成的实例，一视同仁。

### 3.2 一致性的成本：孤儿规则买到了什么，又付出了什么

一致性买到三件事。(1) trait 对象的唯一全局语义：`Box<dyn Eq>` 知道自己携带哪个 `==`，因此存在类型分派有定义。(2) 库的安全演进：下游新增 impl 不会悄悄改变上游行为（没有「孤儿实例改了他人的泛型函数」这类隐患）。(3) 证据构造的有原则基础：证据可以按 (trait, 类型) 对构造一次并全局缓存。它付出四件事。(1) 孤儿规则挡住自然扩展：你无法为外来类型定义 `Eq`，也无法为外来 trait 定义你自己类型的实例，当两者都外来时都不行，于是逼出 newtype 包装（`struct MyHashMap<K, V>(HashMap<K, V>)` 纯粹为了挂 impl）。(2) 任何 crate 里的 blanket impl 否决所有其他 crate 未来的 impl（1.1 节的生态死锁）。(3) 它把库推向设计扭曲：sealed trait、扩展 trait，例如 `IteratorExt`，方法由独立 trait 承载。、`#[fundamental]` 标记。(4) 它与宏和代码生成配合很差，后者可能意外定义出与尚未写下的东西重叠的 impl。Haskell 展示了没有规则的世界：孤儿实例让行为依赖导入，Safe Haskell 禁止它们，社区约定是把它们隔离。所以一致性是带真实摩擦的真实约束。设计问题在于摩擦能否被局部化。

### 3.3 不产生歧义的扩展机制清单

机制 1：孤儿规则（Rust）。全局一致性，摩擦局部地出现在 newtype 包装与 blanket impl 否决上。规则陈述简单、边界强硬，摩擦集中在库边界。如果实例声明在类型旁边，像 Rust 一致的 impl 块，DreamScript 可以廉价采用它，区别在于集合类型的不相交检查让规则的理由可见，报出「这两个实例头相交」，位置式的「crate 不对」被取代。
机制 2：受限实例，先例是 Scala 3 given 与 OCaml 模块实参。消解是词法的，两个实例在不同作用域共存，最近者或显式者胜。歧义是编译错误。全局不变式无需维护。代价：依赖实例的 API 变得对作用域敏感，Scala 的隐式地狱靠 given 严格的词法优先级驯服。「会找到哪个实例」成了读者必须沿着导入追溯的问题。
机制 3：具名实例加显式传递，先例是 Effekt 能力、Zig comptime 实参、Go 接口参数、Koka 处理器值。这里完全没有消解。实例是一个值，需要它的人把它当参数取走。Effekt 的 "Effects as Capabilities"（OOPSLA 2020）显式传递处理器能力，让携带能力的块成为二等，它们无法逃出处理器的作用域，从而得到完整的效应多态，既不需要行多态推断，也没有一致性疑问。Zig 的 comptime 实参是退化情形：「实例」就是任意编译期值，泛型行为就是对 `type` 值的显式代码。
机制 4：结构化接口（Go）。声明不存在。itab 在装箱时构造。同一方法集的多个接口实现平凡共存，因为每个具体类型只有一套方法集。扩展是免费的，但没有重载，没有泛型实例，也没有针对形状相关操作的静态证据。
机制 5：集合类型下的不相交重载（2.2 节）。重载存在，匹配可证明唯一，消解即子类型。不相交检查就是全部故事。

### 3.4 显式能力传递就是无魔法的答案吗？

是，但有一个附带条件。显式传递按构造消除一切一致性疑问：没有东西被消解，一切都被提供。它也消除孤儿规则的摩擦：任何代码都能定义并传递能力。它符合作者的解耦哲学：一次操作的行为是可见参数，Koka UFCS 让 `x.f(y)` 读起来自然，同时仍是对 `f(x, y)` 的普通调用。附带条件是工效学：穿过很多层的能力是样板代码，所以每条走这条路线的语言都会加缓解手段。Effekt 与 Koka 让处理器在每个作用域里成为环境值，手工参数退到其次。Scala 在显式值之上加受限隐式搜索（机制 2）作为语法糖。Zig 接受样板，因为 comptime 实参通常很短。有证据支持的 DreamScript 综合方案：把一致实例声明作为默认路径（不相交让歧义不可能），允许具名受限实例作为第二条路径（Scala 风格，确定性，歧义即错误），并且只在可证明唯一时才隐式传递能力。消解永远不看优先级顺序。只要存在选择，就报编译错误，点出两个候选及其来源。这样在要紧的地方保持「无魔法」，行为可静态解释，同时避开纯机制 3 撞上的样板之墙。

### 3.5 Eclipse 式问题：不一致性真正破坏什么

允许自由实例的具体失败模式并不假设：两个 crate 各自为 `ThirdPartyKey` 定义 `Eq`，产生的程序其 map 行为取决于链接了哪些模块，这是 Haskell 的孤儿隐患，1.2 节的源码已核实。一个 blanket 实例 `impl<T> Foo<T>` 加上后来出现的 `impl Foo<&str>`，让新 impl 出现时泛型代码悄悄改变行为，这是 Rust 对未来 impl 的保守，#31844 已核实。一个特化的 `Drop` impl 能让自动生成的丢弃胶水与特化语义不一致，后果是泄漏或双重丢弃，Rust 特化 issue 记录的正是这类 UB。这些是生态级失败，类似 Eclipse 式可扩展系统里的插件版本冲突：扩展点是全局的，于是独立扩展在链接期无声碰撞。防线有两条：一条靠不相交让碰撞不可能，一条靠歧义错误让碰撞响亮。两条都拒绝静默。

## 4. comptime 与元编程谱系

### 4.1 谱系：从轻机械到重机械

第 0 级：无，先例是 1.18 之前的 Go 与 Elm。derive 式需求靠手写或带外代码生成：`go generate` 跑一个程序写出源文件，编译器不参与，生成边界上没有类型安全，但语言复杂度也是零。Go 直到痛感无法忍受才加入泛型，而且即便那时也没加元编程，团队既定立场是用反射与生成，宏被排除。
第 1 级：运行期反射，先例是 Go `reflect` 与 Java 反射。序列化够用，代价是性能、丢失静态检查、在未导出状态上失败，并把错误从编译期推到运行期，直接违背 DreamScript 偏好编译期错误的立场。
第 2 级：编译器内建合成，先例是 Swift SE-0185 遵循关系、Kotlin data class、Java record、C# record、MoonBit `#derive(...)`、Rust 内建 derive 宏。编译器已经知道类型结构，于是自己发出成员。面向用户的元编程完全没有，扩展性限于编译器交付的内容，这是换取零机械的老实交易。
第 3 级：对普通代码的编译期求值加静态反射，先例是 Zig `comptime` 加 `@typeInfo`/`@Type`、D CTFE 加 `mixin`、Mojo 参数加 `comptime`、C++26 静态反射，以及 TypeScript 的条件/映射/模板字面量类型作为仅类型层的变体。语言自身的语义在编译期运行，没有第二门语言，没有 token 操作，泛型作为特例自然落出，因为类型就是值。Zig 的哲学陈述是最直接的表述：comptime 就是编译期求值的 Zig 代码，宏没有必要，因为代码可以生成代码。
第 4 级：项重写模板，先例是 Nim template 与基于带类型 AST 的 C 宏。卫生的 AST 替换模式，对 DSL 式语法糖与零成本惯用法很强，Nim 标准库就建立在它之上。它仍是第二语义层，带自己的失败模式：模板的组合可预测性不如函数，类型错误在展开之后才浮现。
第 5 级：带阶段的语法树宏，先例是 Lisp 与 Racket。这是基于 token 的这一族里最强也最有原则的。Racket 的阶段系统（编译期/运行期层级、语法对象、完整卫生）让宏可靠地组合，见 "macros that work together"（Flatt/Findler 2018）。但在带类型的语言里展开发生在类型检查之前，于是带类型的宏系统要么在宏里重新实现类型检查，如 Typed Racket，要么把错误推迟到展开，Rust proc 宏操作无类型 token 树正是如此。
第 6 级：依赖类型，先例是 Idris 与 Lean。通过类型系统本身做元编程，类型由程序计算。终极力量，最高学习成本，在 Kotlin 语法糖天花板之下超出范围。

```zig
// Zig: derive-like JSON serialization without any macro machinery (documented comptime pattern)
fn Jsonify(comptime T: type) type {
    return switch (@typeInfo(T)) {
        .@"struct" => |s| struct {
            pub fn toJson(self: T) []const u8 {
                var out: std.ArrayList(u8) = .empty;
                inline for (s.fields) |f| {          // compile-time iteration over fields
                    // append field name, recurse on f.type
                }
                return out.items;
            }
        },
        else => @compileError("Jsonify: not a struct"),
    };
}
```

### 4.2 卫生

不卫生的文本替换（C 预处理器）被公认为错误：`#define max(a,b)` 会把实参求值两次、捕获标识符，并在每个边界情况上坏掉。卫生是分档的。Racket 与 Common Lisp 第二代按展开点重命名引入的标识符，Racket 的语法对象让卫生成为默认，靠 `datum->syntax` 显式逃逸。Nim 模板默认卫生，附带显式逃逸 ` {.inject.}`。Rust `macro_rules!` 在 token 层卫生，宏体里的标识符不与调用点碰撞，而 proc 宏根本上是无卫生的，它们操作原始 token 树，这是 proc 宏脆弱性的主要来源：derive 宏重新解析任意 Rust，serde 那样的属性 DSL 活在 token 流里。Zig/D/Mojo 式的编译期求值完全不需要卫生概念，因为根本没有替换：生成的代码就是普通代码，反射读取结构，重写这条路不存在。这是 DreamScript 选第 3 级的强论据：它连同宏机械一起删掉了整个卫生问题。

### 4.3 最小元编程如何覆盖 derive 式需求

derive 用例需要的恰好是：(a) 对一个类型字段/分支列表的静态访问，含名字、类型、顺序。(b) 对该列表的编译期迭代与分支。(c) 对普通代码的编译期求值，用来组装成员实现。(d) 可选地，针对热点的内建 derive 属性，例如 Eq/Hash/Show，让常见情形只有一个词长并编译得快。Zig 演示了 (a)+(b)+(c) 足以完成任意 struct 的 JSON 序列化（`@typeInfo` 加 `inline for` 就是全部配方）。MoonBit 用 `#derive(Eq, Hash, Debug, ToJson)` 属性演示了 (d)，在 moonbit-core 里随处可见：每个包的类型都带 derive 属性，内建 trait 交付 `not_equal` 之类的默认方法体。Swift 说明即使没有反射，内建合成也能覆盖日常情形，SE-0185 加 Codable 合成即是例证。Rust 说明内建之外的一切都退化成一个 proc 宏子行业，derive_more 与在 token 流里带自己属性语言的 serde 都是，代价是编译期成本与错误信息成本：proc 宏要跨 crate 边界，构建更慢。推荐的 DreamScript 形态：第 2 级为 Eq/Hash/Show/ser-de 提供内建 derive，第 3 级为其余一切提供静态反射加 comptime 求值，v1 不做 token 宏。刻意不需要的东西：任意语法扩展、项重写、阶段分裂，调研到的 derive 式需求没有一条需要它们。

## 5. derive 的故事

### 5.1 调研：各语言如何避免手写相等性/哈希/显示/序列化

Rust：`#[derive(Eq, Hash, Debug, Clone, Serialize)]`，derive proc 宏展开成 impl 块。代价有四项。第一，独立的 proc 宏 crate 边界带来编译期开销。第二，token 树解析取代带类型分析，错误质量因此下降。第三，生态碎片化，serde 的属性 DSL 活在 token 流里。第四，一致性耦合，derive 出的 impl 仍须满足孤儿规则。内建（`Debug`、`Eq`、`Hash`、`PartialEq`、`Clone`、`Default`）不用 proc 宏就能覆盖核心情形。

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Point { x: i32, y: i32 }
```

Swift：声明处主动选择加编译器合成（SE-0185），当所有存储属性或所有 enum 分支的关联值都遵循时合成 `Equatable`/`Hashable`。`Codable` 合成编码/解码。没有关联值的 enum 自动获得遵循。条件遵循在泛型上组合，例如 `extension Array: Equatable where Element: Equatable`。标准情形不涉及用户宏，编译器就是 derive 引擎。

```swift
struct Point: Equatable, Hashable, Codable { let x: Int; let y: Int }  // all members synthesized
```

Kotlin：`data class`，从主构造函数生成 `equals`/`hashCode`/`toString`/`copy`/`componentN`。它限于带主构造函数的类，并受继承限制：data class 不能继承其他 data class，`copy` 只触及构造参数，在类体里声明的属性被静默排除在 `equals` 之外，这是已文档化的坑。

```kotlin
data class Point(val x: Int, val y: Int)   // equals/hashCode/toString/copy + destructuring
```

Java：record（16+），从 record 头生成 `equals`/`hashCode`/`toString`/访问器。`==` 仍是引用同一性，这避开了浮点语义问题，也让同一性比较成为逃生口。C# record（9+）做同样的生成，外加 record 类型的 `==`/`!=` 运算符。

```java
record Point(int x, int y) {}              // equals/hashCode/toString generated
```

OCaml：按语言语义为所有数据提供通用多态比较 `=`，结构化、对有限数据全、对每个类型都有效，包括 functor 的闭包，由运行期做泛型遍历。`==` 是物理同一性。已知代价：多态比较是反射式的且会装箱（慢，这是已文档化的性能瑕疵，逼得库去定义自定义比较），循环值可能发散，NaN 比较在遍历内部遵循 IEEE 语义。打印/序列化的扩展走 `ppx_deriving` 插件，即编译期 AST 重写，语言内机制不参与。OCaml 是「相等性默认就是通用的，不需要为它 derive」最强的例子。

MoonBit（仓库证据，2026）：类型声明上的 `#derive(Eq, Hash, Debug, ToJson)` 属性。trait 带默认方法体：`trait Eq { fn equal(Self, Self) -> Bool; fn not_equal(Self, Self) -> Bool = _ }`。元组的 blanket 式 impl 按元数逐个手写在 `builtin/tuple_eq.mbt` 里，先是 `impl[T0: Eq, T1: Eq] Eq for (T0, T1)`，然后三元组版本，依次下去，这展示了 trait 中介的相等性所产生的元数样板。`builtin/traits.mbt` 记录了浮点的坑：Double/Float 使用 IEEE 相等性（NaN 不等于自身，有符号零相等），要求自反相等性的算法必须排除 NaN，而为排序/搜索的键必须显式提供 `Double::total_compare`。这是 derive 相等性世界里选择 IEEE 语义所付代价的活证据。

```moonbit
#derive(Eq, Hash, Debug)                    // moonbit-core/path/path.mbt
pub(all) struct Path { components : Array[String], absolute : Bool }
```

Roc：完全没有 derive，`==` 对所有数据都是结构化的。标准库把可比较性表达成能力要求：按 builtin 文档，`Dict.is_eq` 要求 `k.is_eq`、`k.to_hash`、`v.is_eq`。它把浮点相等性拆成三个名字：`is_eq` 走结构化，`is_float_eq` 走 IEEE，`is_approx_eq` 走容差。默认十进制类型 `Dec` 让 `0.1 + 0.2 == 0.3` 按构造为真。相等性与哈希在标准库签名里处处耦合（`is_eq` 必伴 `to_hash`），从 API 形状上阻止不一致的覆写。

Go：没有 derive。`==` 是结构化的，但只对「可比较」类型有效，且在静态检查。map/slice/函数不可比较：slice/函数直接比较在编译期失败，含它们的接口值在运行期 panic，这是全性窟窿。生态填这个缺口有两条路：`reflect.DeepEqual`，运行期且慢，或 `go generate` 的带外代码生成。所调研语言里 derive 故事最弱的一个。

Haskell：`deriving (Eq, Ord, Show)` 子句。`DeriveGeneric` 加基于 `Generic` 的默认方法让用户类也能获得推导。`StandaloneDeriving` 与 `GeneralizedNewtypeDeriving` 覆盖特殊情况。紧凑且有类型，但与类耦合：推导只为它设计过的类提供。

Scala：`case class`，生成 `equals`/`hashCode`/`toString`/`unapply`。`canEqual` 模式守护继承的不对称，这是已文档化的坑，Kotlin 的 final data class 用强制规定避开了它。

汇总表：

| 语言 | 相等性 | 哈希 | 显示 | 序列化/反序列化 | 机械 |
|---|---|---|---|---|---|
| Rust | derive trait | derive trait | derive trait | 生态宏 | proc 宏 |
| Swift | 合成 | 合成 | （靠 Mirror/CustomStringConvertible） | 合成（Codable） | 编译器内建 |
| Kotlin | data class | data class | data class | 无内建 | 编译器内建 |
| Java/C# | record | record | record | 无内建 | 编译器内建 |
| OCaml | 通用 `=` | 无（靠库提供 Hash） | `Deriving_Show` | ppx_deriving | ppx 插件 |
| MoonBit | derive trait | derive trait | derive trait | derive（ToJson） | 编译器内建 |
| Roc | 通用结构化 | 耦合的 `to_hash` | `to_string`/Inspect | `ToJson` 属性 | 无 |
| Go | 可比较时结构化 | 无 | 靠反射的 fmt | 反射/生成 | 反射 |
| Haskell | deriving 子句 | deriving 子句 | deriving 子句 | 库（aeson+Generic） | deriving/Generic |

### 5.2 集合论不可变数据下的通用结构化相等性：哪里会坏

如果类型是集合，且数据不可变、没有同一性，那么结构化相等性就是唯一自洽的 `==` 概念：没有地址值得比较，同一性是实现上的偶然事件，而按构造相等的两个值必须相等，因为语言没有其他可区分的属性。对值域做全性分析，逐条看破裂点：

破裂点 1：函数。闭包的外延相等性不可判定，比较函数行为需要解任意程序的等价性。内涵（指针）相等性与 `==` 的其他部分不一致（两个语法相同的 lambda 会被判为不同）。Elm 与 Roc 静态拒绝函数比较。Lisp 比较同一性，不一致但诚实。Go 在函数值的接口比较上 panic。DreamScript 应当静态拒绝函数类型上的 `==`，函数干脆不实现相等性能力。这不会带来成本：在带类型的代码里比较函数很少见，通常是 bug，例如拿传进来的回调与预期的那个比。集合论的表述让这条有原则：函数集合没有可判定的相等性谓词，所以它们的类型里没有 `Eq` 能力，而只要 `==` 受能力约束，类型系统就已经知道这一点。

破裂点 2：浮点与 NaN。IEEE 相等性（NaN != NaN，+0.0 == -0.0）与结构自反性（NaN == NaN，位相同则相等）冲突。三个自洽的选择。(i) 让 `==` 走结构化/位语义，另设具名的 IEEE 比较 `is_float_eq`。这是 Roc 的拆分，已在 builtin 文档核实：`Num.F32.is_eq`、`Num.F32.is_float_eq`、`is_approx_eq` 是三个独立名字。自反性成立，于是哈希 map 的键与集合成员关系行为正常，IEEE 语义仍可按名字取到。(ii) 让 `==` 走 IEEE 语义，配已文档化的前置条件与一个全序辅助函数，这是 MoonBit 的选择：`builtin/traits.mbt` 声明 Double/Float 使用 IEEE 相等性且 NaN 不等于自身，derive/复合相等性把排除 NaN 作为前置条件，而为排序/搜索 API 必须显式提供 `Double::total_compare`。仓库里的注释痕迹，例如 `argparse` 里的 TODO 与 `physical_equal_nan_test.mbt`，显示这条前置条件在 derive 相等性触及浮点的每个地方都渗进用户文档。(iii) 禁止 IEEE 浮点走默认 `==`，除非包进 newtype。最大安全，最大摩擦。证据偏向 (i)：`==` 的自反性对哈希 map、集合论成员关系与去重都是承重的，而 Roc/MoonBit 这一对说明两种设计都在生产环境里跑，(i) 需要记录在案的前置条件更少。哈希一致性自动跟随：结构化相等性的哈希就是规范位的哈希（NaN 一致地哈希，+0.0 与 -0.0 在它们比较相等时哈希相同，取位相等为规则并在哈希时归一化）。

破裂点 3：循环的不可变数据。严格默认不可变的语言没有变异或惰性就无法形成循环，所以循环只经由显式的 `ref`/惰性单元出现。结构化相等性有两条路。第一条拒绝穿过可变/不可复制单元，按同一性比较它们，与第 7 节对资源的处理一致。第二条用已见集合处理循环，OCaml 的朴素遍历在循环值上发散，这是已文档化的瑕疵。让引用单元按同一性比较，彻底绕开循环问题，也符合「变异是显式的、局部可见的选择」：如果你能观察到循环，那就是你选了可变单元，它的同一性就是诚实的语义。

破裂点 4：线性/仿射值。`==` 不得消耗操作数：它取两个借用并返回布尔值，所以仿射值比较之后仍然仿射，`a == b` 不移动 `a` 或 `b`。比较资源，例如文件、通道与唯一句柄，必须意味着同一性而非内容：同一个文件上的两个打开句柄按引用相等处理，内容相等被跳过。derive 出的相等性在拥有的仿射数据上仍然有效，因为生成的代码只读取并借用字段。这让 `==` 成为借用不可变数据上的方法，正是 Rust 的 `PartialEq for &T` 与 Hylo 的 `let` 约定已经在用的形状。它也意味着线性值永远不会被比较「用完」，从而保住 Perceus 式复用分析所要求的「确切知道值何时死亡」。

破裂点 5：哈希一致性。结构化 `==` 蕴含结构化 `hash`。任何语言级相等性能力都应当同时要求哈希：Roc 在每个标准库签名里把 `is_eq` 与 `to_hash` 耦合，MoonBit 按约定把 `Hash` 与 `Eq` 一起 derive，Kotlin data class 从同一组构造参数生成两者。把它们解耦会引入经典 bug：相等的值哈希不同，或哈希 map 里出现重复项。

### 5.3 对 DreamScript 的结论

带能力边界的通用结构化 `==` 是自洽的：把 `==` 定义为数据上的结构化比较，在函数类型上拒绝，取借用，浮点用位结构语义并另设具名 IEEE 比较，可变/引用单元按同一性比较，哈希与相等性共同要求。derive 于是收缩为 Hash、Show 与序列化，这恰是 MoonBit/Swift/Roc 的证据所说应当由编译器内建合成的部分，即 `#derive(Hash, Show, ToJson)` 风格：一个属性，没有宏系统，没有用户 DSL，错误信息好是因为编译器在做。MoonBit 手写的元组元数样板，即 `Eq for (T0, T1)`、`(T0, T1, T2)` 等按元数逐个，已在 `builtin/tuple_eq.mbt` 核实，在相等性是语言级结构化时完全消失，trait 中介在其中缺席。仍然由 trait 中介的是比较顺序 `Ord`：它没法结构化，因为集合上的任何全序都是任意的。所以 `Ord` 保持显式能力，配内建 `derive(Ord)`（字段顺序即字典序，与 Rust/Kotlin/Swift 一致）与用于键的 `total_compare` 浮点变体，遵循 MoonBit 的先例。

## 6. 性能：泛型在解释器里如何执行

### 6.1 三种执行策略

单态化：为每个实例化编译一份副本。优势：直接调用、可内联、稳态速度最好、没有运行期类型机械，先例是 C++、Rust、Mojo、Zig 与 .NET 的值类型。代价：编译期爆炸、二进制体积、i-cache 压力，即经典的 C++ 模板膨胀，以及对 DreamScript 决定性的问题：它不适配解释器的执行模型。解释器需要按实例化惰性物化字节码，Julia 在 JIT 层做这件事，解释器层不做，Zig 与 Rust 提前做，付出用户可见的编译延迟。

字典/证据传递：每个泛型函数一份函数体，约束编译成证据参数：GHC 字典、Rust `dyn` vtable、Go 的 GC 形状字典、Effekt 能力。优势：代码尺寸均匀、无实例化的解释、契合字节码 VM、证据可组合：`(T0, T1)` 的字典由 `T0` 与 `T1` 的字典搭出来，MoonBit 的元组 impl 就是这件事在源码层的镜像。代价：不经过去虚化就是间接调用，证据穿线在冷路径上可见。

动态分派加按需特化，先例是 Julia。它通过方法表在运行期分派。JIT 在变热时按类型元组编译特化，推断把静态可证明的调用去虚化。优势：最大动态性，契合基于集合的方法匹配。代价：缓存暖起来之前解释器侧每次调用都有分派成本，性能取决于推断是否成功，即类型不稳定的悬崖。除非静态检查全性，「找不到方法」就是运行期错误。

### 6.2 证据怎么说

Julia：特化调用完全编译掉。未特化的分派要付一次方法表搜索。LU 案例显示单凭推断质量就有十倍以上差距，朴素实现 4.15 s，调优实现 0.4 s，出处 arXiv 1808.03370，瓶颈落在推断失败上，分派机制本身很快。.NET：值类型实例化用布局最优的专用代码消除装箱。引用类型实例化共享一份函数体，也就是说 .NET 刻意在划算的地方单态化，即小、热、拆箱的情形，其他地方一律共享，走字典传递。Java：巨态的 `invokeinterface` 分派是已知的成本中心。接收者集合小时 JIT 用内联缓存与类层级分析把它捞回来。被调方静态未知时逃逸分析与内联都被挡住，这是没有证据传递的擦除泛型要付的代价。GHC：字典传递，靠 `INLINE`/`SPECIALIZE` 手工擦除，没有跨模块的自动特化，于是字典成本在性能敏感代码里成了用户可见的杂务。四个系统的共同教训：赢的配置是「一份泛型函数体加证据加调用点缓存，再在优化层做有针对性的惰性特化」，单态化只留给可证明划算的情形（拆箱原语）。

### 6.3 DreamScript 的具体字节码设计

每个泛型函数在字节码里只表示一次，附带它的约束列表。在泛型调用点发出 `CALL_GENERIC fn, evidence_words`：每个证据词要么是静态消解好的实例引用，接收者类型已知时取一个 vtable 指针或缓存的实例闭包，要么是运行期消解的槽位，实参的集合类型只在动态时已知。解释器为每个调用点维护一份内联缓存：从观察到的运行期类型向量到已消解被调方与方法偏移的映射，首次执行后就地打补丁，这是 Self/V8 风格的多态内联缓存（Ungar/Chambers 1991），它在每个快速动态 VM 里仍是标准技术。需要值形状知识的操作，例如拆箱集合类型的值、访问变体，走一个按运行期标签的小 switch，内联缓存把常见情形变成直接跳转。在 JIT 层，被类型证明器静态消解的证据词内联成直接调用。变热的证据传递链按具体类型向量惰性物化特化（Julia 风格），缓存在函数的实例化表里，并在内存压力下降时丢弃。

字节码形状的示意，仅为示例：

```text
-- generic function, stored once
fn map[T: Eq, U](f: fn(T) -> U, xs: List[T]) -> List[U]
  ; constraint words: [Eq_T_slot]  -- resolved per call site

-- call site with statically known T (Int):
  CALL_GENERIC map, [static_ref: eq_instance_int]
  ; interpreter: no lookup at all — evidence is a constant operand

-- call site with dynamic T (set-typed value):
  CALL_GENERIC map, [dynamic: tag_of(x)]
  ; interpreter: IC keyed by tag; first call resolves, later calls jump directly
  IC_PATCH map@42: { tag_list -> callee, expected_shape }
```

集合论实例声明让证据表有良好基础：实例头两两不相交（2.2 节），所以一个运行期值的成员测试至多识别出一个实例，内联缓存因此不需要优先级决胜。缓存永远不会与语义不一致，失效也是单调的：类型只会收窄，更窄的类型命中同一个唯一实例。

### 6.4 调用点缓存算违反立场的「特化」吗？

不算，而且这条区分值得精确陈述。作者拒绝 C++ 风格特化，拒绝的是源码层、可观察的分派：程序员写下重叠 impl，语言按程序员必须学习并推理的优先级规则消解它们，于是行为取决于哪些特化写在什么地方。内联缓存与 JIT 层惰性特化是编译器内部的：它们从不改变哪段代码在语义上被选中（那个唯一的不相交实例），只改变到达它的速度，而且不可观察（设计良好的内联缓存会退回到完整查找，所以正确性不依赖缓存命中）。同一条区分也把 .NET 的值类型刻印（不可见）与 C++ 偏特化（可见）分开。DreamScript 因此可以精确地陈述这条不变式："dispatch is determined by types and the set algebra, never by declaration order or priority; the runtime may cache and specialize any dispatch that is already unique"。

## 7. 与线性/仿射类型的相互影响

### 7.1 不可复制值上的泛型

Rust：泛型代码默认在非 `Copy` 类型（仿射）上就工作，`Copy`/`Clone` 边界是主动选择复制。`Sized`/`?Sized` 在类型层区分全称与存在。标准库是 `min_specialization` 的主要用户，例如在 `T: Copy` 时为 `Vec<T>` 特化 `Clone` 并走 memcpy。这说明即便 Rust 那个小小的可靠特化子集，存在目的也是让泛型容器更快，用户的表达力从中得不到什么。同样效果对字节码 VM 就是一条内联缓存快路径（6.3 节）。Hylo：所有类型默认不可复制且不可移动，`Copyable`/`Movable`/`Deinitializable` 是主动选择遵循。不写复制边界的泛型代码对两种世界都成立。`~Copyable` 边界是取反的约束，它声明某个参数可以不可复制。参数约定 `let`/`set`/`inout`/`sink` 分别对应借用、设置、消费，取代「指针即类型」：引用是二等的，类型层不表达引用，见 Hylo 讨论 #754。于是函数签名说明了它如何使用实参，而不必把地址类型引进值语言。这是已知最直接的「线性值上的泛型」设计：承载参数线性性的是传递约定，类型本身不承担这件事。

```hylo
// Hylo-shaped: one generic body serves copyable and noncopyable T alike
square[T: Movable & ~Copyable](x: T) -> T   // no copy bound: works for owned linear T
```

Koka：Perceus 引用计数配复用分析（Reinking、Xie、de Moura、Leijen，ICFP 2021）让函数式泛型代码避免重新分配。当某个拥有值的引用计数即将降到零时，`reuse` 消费它的缓冲区，在可能时把 `Cons(h, tail)` 变成对已消费链表节点的就地覆写。仓库证据显示标准库为此而弯折：`lib/std/core/types.kk` 记录了一个 `pad` 引用类型，"sometimes used for fbip programming to match up the size of constructors for reuse"。复用分析在场时，被消费的构造器与新建构造器之间的分配形状兼容性成了真实的标准库设计关注点。对 DreamScript 的含义：仿射值上的泛型容器操作（map/filter）有两条实现路线。第一条是借用，把仿射降为共享以便读取，正如 Rust 的 `&T` 对拥有值所做的。第二条是显式的消费并返回约定，例如 `fn cons[T](h: T, tail: List[T]) -> List[T]` 复用 `tail` 的节点。`~Copyable` 式的不可知边界让两种世界共用一条代码路径，借用对消费的选择活在参数约定里，重复的泛型 API 因此不存在。

### 7.2 线性性之下的变型

对不可变数据，集合论子类型让变型从集合包含里直接落出：如果 `Dog <: Animal`，那么作为值集合 `List<Dog> <: List<Animal>`，因此不可变容器是协变的，无需任何标注，Scala 的 `val` 字段与 Kotlin 的只读接口就是这样。线性性不会破坏这一点：强制转换仿射值的类型只做重标记，`Box<Dog>` 是一块仿射的狗，转换成 `Box<Animal>` 后仍然只有一个值。前提是这次转换不能复制或丢弃内容，不可变数据按构造保证了这点。不变性恰好出现在存在内部可变性或唯一借用逃逸的地方，例如 Rust 的 `Cell<T>`/`&mut T` 不变性与 Hylo 的可变值语义：可变容器状态的集合在其元素类型上并不单调，`Cell<Animal>` 可以存 `Cat`，于是它在任何意义上都算不上 `Cell<Dog>`。集合论子类型把它表述为「读角色协变，写角色逆变，同时具备两种角色的字段不变」。给设计的经验规则：不可变数据默认协变。字段唯一可变时不变。除非用户写可变容器，否则没有变型标注要学。集合代数同时给出检查器的推理：容器的子类型就是它们值集合的子类型，那正是语义子类型已经在计算的东西。

### 7.3 资源的幻影类型

幻影/gift 包装的参数在类型层编码资源状态：`File[Open]` -> `close : File[Open] -> File[Closed]`，配上仿射强制：已关闭的文件无法再关，因为值被消费了，被遗忘的文件也不会变成垃圾，因为类型系统跟踪它。在集合论类型下，状态转换是集合变换：`File = File[Open] | File[Closed]`，`read` 把 `File[Open]` 收窄到自身，`close` 把 `File[Open]` 映射进 `File[Closed]`，并集与部分使用的 API 因此直接拼合。幻影上的 trait 式边界（`S: SocketState`）让泛型代码约束它接受哪些资源协议。这个模式只需要普通泛型，HKT 与 GAT 都不需要，类型参数是一个普通类型，类型族在这里不出现。它与 7.1 的线性借用约定组合良好：为 `read` 借用的 `File[Open]` 仍然存活，被 `close` 消费后不再存活。

```text
-- DreamScript-shaped sketch
type File[S: FileState] = ...
fn open(p: Path) -> File[Open]          -- linear: exactly one
fn read(f: &File[Open], n: Int) -> Bytes  -- borrow: file survives
fn close(f: File[Open]) -> File[Closed]   -- consume: can't close twice, can't read after
-- f : File[Open | Closed]  is allowed: read needs &File[Open] (narrow first), close needs File[Open]
```

## 8. 存在类型、全称类型、关联类型、HKT

### 8.1 存在类型对全称类型

全称类型即泛型函数，由被调方书写，在编译期消解。存在类型即携带自身证据的动态值，在生产方书写，在运行期分派。Rust 把两者拼成 `impl Trait` 对 `dyn Trait`。Swift 是 `some P` 对 `any P`，SE-0352 把存在类型隐式打开进泛型调用。OCaml 是一等模块 `(module S)`。Go 里一切都是存在类型，即接口。跨语言的教训：两种都保留，语法各用两个词，并且在性能导向的线性语言里默认用全称类型，因为存在类型会装箱并破坏仿射所有权推理。`dyn` 值擦除了自己的具体仿射类型，Rust 需要 `T: ?Sized` 那套体操，正是因为存在类型与所有权相处很差：一个无尺寸的仿射值无法按值穿过泛型参数 `T: ?Sized` 移动。存在类型对异构集合与插件边界仍不可缺，一等模块（组合 C）是同一件事显式、证据可见的拼法。

关联类型的目的是让两种推断都友好：带 `type Item` 的 `Iterator` 避免了「什么类型的迭代器？」这种歧义，参数化的 `Iterable<T>` 在元素类型由 self 类型决定时表达不了它。对借出迭代而言，即 `Item<'a>` 从 `self` 借用，关联类型也是唯一诚实的拼法，这正是催生 Rust GAT 的细分需求，见 GAT 计划的用量调查。DreamScript 应当交付参数（带集合论边界）与关联类型，但跳过参数化的关联类型（GAT），直到出现具体需求：Hylo 的借用约定设计不用它们就覆盖了多数借出迭代器形状的需求。

### 8.2 HKT 究竟什么时候需要？

HKT 指对类型构造器抽象，`Functor F => map : (a -> b) -> F a -> F b` 就是典型形状。你在写出对容器形状泛型、对内容不关心的代码时才需要它。需求调研：Kotlin 从来没有 HKT，KEEP-87 自 2017 年停滞，Arrow 弃用了模拟编码，它仍然是一门繁荣的主流语言。Swift 十年没有 HKT，用不透明类型加主关联类型填补缺口，HKT 本身不引入，而 Swift 团队的方向是带 `~Copyable` 边界的不可复制类型与上下文敏感的遵循，继续不含 HKT。Rust 没有 trait 级 HKT，靠 GAT 部分 HKT 应付，它自己的社区都说难学。Haskell 是 HKT 承重的语言，因为它的抽象风格是对抽象形状的自由类方法，例如 `fmap` 与 `>>=`。DreamScript 声明的风格指向另一边：Option/Result 是带方法的具体单子，`option.map`、`result.and_then` 不需要抽象 functor。效应如果采纳，走行/能力路线（文件 02），单子变换器塔被排除。集合暴露方法，自由的泛型组合子被排除。在这些选择之下，HKT 的驱动力消失了。推荐：v1 交付参数加关联类型加基于集合的变型。HKT 不进 v1，直到出现具体且重复的模式，而证据说这个模式在 Kotlin 与 Swift 里十年都没出现。

```text
-- what DreamScript needs vs what HKT would add:
option.map(f)      -- concrete monad method: no HKT, full inference
result.and_then(f) -- ditto
list.map(f)        -- ditto
-- an abstract `map : (F, (a->b)) -> F b` over all three at once is the HKT ask;
-- it buys abstraction over containers, costs inference complexity (projections
-- through constructor variables), and has no consumer in the declared stdlib style.
```

## 9. DreamScript 的可行组合

### 组合 A：一致的集合论 trait 加内建 derive 加证据传递 VM（推荐的基线）

带关联类型的 trait。实例头两两不相交，声明时用空集检查，取反类型可用于切出边界。Rust 意义上的孤儿规则在这里缺席，因为不相交加实例注册让歧义不可能。内建 `#derive(Eq, Hash, Show, Serde)` 编译器合成。通用结构化 `==` 配第 5 节的例外。字节码 = 一份泛型函数体加证据词加内联缓存。JIT 按类型向量做惰性特化。

```text
-- DreamScript-shaped sketch
trait Ord[T] { fn compare(&T, &T) -> Int }

impl Ord[Point] { fn compare(a, b) { ... } }        -- unique: heads are disjoint by construction
impl[T: Ord] Ord[List[T]] { ... }                    -- conditional instance: an intersection arrow

fn max[T: Ord](a: &T, b: &T) -> &T { if a.compare(b) < 0 then b else a }
max(p1, p2)          -- resolves to impl Ord[Point]; evidence is a static ref
max(xs, ys)          -- evidence composes from Ord[T]'s evidence word
```

学习成本：低到中等，涉及 trait、关联类型与不相交规则，没有特化，没有一致性律师，没有宏系统。性能：解释器里靠内联缓存接近 JIT 层的行为，未装箱的原语靠形状专用证据，带集合标签的值在常见路径上不装箱。语法糖成本：接近零，derive 属性承担大部分，derive 故事不再有 data class 式的重复，UFCS 让普通函数具备方法调用的工效学。集合论契合度：完美，实例是集合模式，消解是子类型，不相交可检查。线性契合度：好，`T: Copyable` 那样的主动选择边界或 `~Copyable` 不可知泛型都可用，`==` 取借用，参数约定承载线性性，遵循 Hylo 模型。反特化契合度：按构造完美。标准库影响：集合带 `Eq`/`Hash`/`Ord` 边界，即证据词。手写的元组元数 impl 不存在，结构化相等性把它们删掉了。map 要求 `Hash` 与 `Eq` 共同 derive。迭代走关联类型。资源类型走幻影参数（7.3 节）。风险：不相交检查器必须完备，错误信息必须有教益，例如「这两个实例头在 `Even Int` 相交，用取反精化一侧或合并它们」。条件实例需要为递归消解给出终止性论证，Modus Ponens 演算提供了它。

### 组合 B：显式能力，没有隐式消解（Effekt/Zig 方向）

完全没有实例搜索：`Eq` 的实现是值（`eqInt`、`eqPair(eqInt, eqInt)` 这类组合子），泛型函数把它们当参数取走，UFCS 在唯一时把 `x == y` 变成传入能力的语法糖。Zig 风格 comptime 反射覆盖 derive 式需求。

```text
-- DreamScript-shaped sketch
type Eq[T] = { fn equal(&T, &T) -> Bool }
fn dict_get[K, V](d: &Dict[K, V], k: &K, eq: Eq[K]) -> Option[V]

dict_get(m, key, eqPair(eqInt, eqStr))   -- fully explicit: no resolution ever
dict_get(m, key, _)                      -- sugar: compiler inserts the unique Eq[K] if provable
```

学习成本：概念上最低，没有魔法，但样板最高。Scala 的历史说明隐式搜索之所以存在，是因为手工穿线很痛，而 Effekt 的缓解办法是让处理器在每个作用域成为环境值，手工参数退到其次。性能：与组合 A 相同，证据是显式的，内联缓存照样适用，静态证据是常量操作数。语法糖成本：高，除非用唯一实例插入语法糖缓解，即上面的 `_`，那是机制 2 的轻量版受限消解，所以实践中纯组合 B 做不到完全「无魔法」。集合论契合度：好，没有交互。线性契合度：极好，能力是普通值，线性性可组合，显式证据让仿射流一目了然。反特化契合度：完美。标准库影响：每个多态 API 都长出能力参数或 comptime 反射路径。无 derive 的相等性仍需要通用结构化 `==`，与 A 相同。如果能力泛滥，Effekt 式的环境作用域是缓解手段。

### 组合 C：ML 模块（1ML 风格统一核心与模块）

一切都是模块。泛型是 functor。一等模块给出存在类型。`Map.Make(Ord)` 式显式实例化。抽象类型按构造获得。

```text
-- DreamScript-shaped sketch
module type Ordered = { type T; fn compare(&T, &T) -> Int }
module Map(O: Ordered) = { ... }        -- generative: two instantiations, two distinct types
module StringMap = Map(StringOrd)       -- explicit
```

学习成本：高，涉及签名以及生成式对应用式 functor，语法预算顶到 Kotlin 语法糖天花板。性能：极好，全静态，运行期机械不需要。语法糖成本：重，functor 即泛型对集合代码来说很啰嗦，这正是 OCaml 长出 ppx 与一等模块垫片的原因。集合论契合度：正交，没有冲突。线性契合度：没问题，模块是值，生成性给出新的抽象类型，对能力有用。反特化契合度：完美，处处显式。标准库影响：集合按比较器模块参数化，Rust 的 `BTreeMap<K: Ord>` 一行变成两步实例化。derive 故事最弱，需要 OCaml 那样的通用比较或外部代码生成。结论：一个讲原则但语法糖昂贵的整门语言方向。它有用的碎片可以借进 A：一等模块作为存在类型，用于能力的生成式抽象类型。

### 组合 D：Zig 风格 comptime 泛型（类型是值）

泛型是返回类型的编译期函数。反射走类型信息。分派完全消失，实现走完全单态化。

```text
-- DreamScript-shaped sketch (Zig flavor)
fn List(comptime T: type) type {
    return struct { items: [64]T, len: Int, ... }
}
var xs: List(Point) = ...
```

学习成本：中等，只有一个概念「在编译期运行代码」，但这是用户必须学着调试的第二执行阶段。Zig 在 comptime 上的错误信息是它已知的弱点。性能：稳态最好，没有证据，没有分派，直接调用。但在解释器里这意味着按实例化惰性物化字节码，编译成本在运行期可见，走 Julia 风格，或提前实例化热路径，于是有解释器启动成本风险。语法糖成本：语法低，语义开销高。comptime 失败是编译期求值失败，编译期值与运行期值的边界是第二套类型系统。集合论契合度：差，comptime 绕过静态类型系统。在 comptime 构造的类型逃出集合代数，不重叠不变式在 comptime 生成的实例上无法强制，而编译期单态化与默认不可变的字节码故事冲突，每个实例化都是新函数体，一种语义形式的特化。线性契合度：弱，comptime 生成的类型需要手工的线性性纪律，线性检查器必须在生成的代码上运行，而 Zig 不做这件事。反特化契合度：混合，没有源码层的重载优先级，但语义上它就是模板实例化，它对类型代数的不透明正是集合论可解释性的反面。标准库影响：容器靠 comptime 构造，derive 靠反射满足，实现直接，但每个 API 在 comptime 边界上都变得部分动态定型。结论：采纳反射那一半，即静态类型反射加 comptime 求值用于 derive，第 4 节的第 3 级。泛型那一半不采纳。

### 组合 E：Julia 风格动态多重分派加 JIT 去虚化

没有 trait。方法挂在泛型函数上并匹配所有实参集合。消解是动态的，配内联缓存。JIT 特化热的类型元组。

```text
-- DreamScript-shaped sketch
fn area(r: Circle)    = pi * r.radius^2     -- clauses are set patterns
fn area(t: Triangle)  = ...
fn area(s: Square)    = s.width^2           -- disjoint heads checked statically
```

学习成本：脚本用途最低，完全没有类型类概念。性能：经过 JIT 且推断成功时极好，内联缓存暖起来之前有解释器分派成本，类型不稳定的悬崖是真实的，Julia 证据显示单凭推断质量就有十倍差距。语法糖成本：语法低，但运行期分派意味着运行期的「找不到方法」与歧义错误，这违反「编译期错误优于运行期错误」，除非静态检查器对静态可能的实参集合证明全性，即对集合类型做不相交加覆盖，2.4 节。集合论契合度：类型层好，分支是集合模式，穷尽性可静态检查不相交。线性契合度：差，动态分派隐藏被调方，挡住仿射流分析与跨分派的 TCO 保证，每次调用都变成潜在的效应点。反特化契合度：源码层好，分支不相交，但设计依赖运行期匹配，那是一种延迟消解。标准库影响：数值/数组风格 API 丰富，但仿射值上的集合需要内部无分派，而 derive 故事不变，结构化相等性在两种情况下都成立。结论：作为数值/脚本层有吸引力，对静态类型的线性语言是错误的默认。日后可作为按模块开启的「动态模式」，不进 v1。

### 对比表

| 组合 | 学习成本 | 性能（解释层） | 性能（JIT 层） | 语法糖成本 | 集合类型契合 | 线性契合 | 反特化契合 | 标准库 derive |
|---|---|---|---|---|---|---|---|---|
| A 一致 trait 加 derive | 低到中 | 好（内联缓存） | 很好 | 低 | 极好 | 好 | 完美 | 内建合成 |
| B 显式能力 | 低（样板多） | 好 | 很好 | 中到高 | 好 | 极好 | 完美 | 基于反射 |
| C ML 模块 | 高 | 极好 | 极好 | 高 | 没问题 | 没问题 | 完美 | 弱 |
| D comptime 泛型 | 中 | 启动风险 | 最好 | 语法低，语义高 | 差 | 弱 | 混合 | 反射 |
| E 动态多方法 | 低 | 分派加内联缓存 | 很好（不稳定悬崖） | 低 | 好 | 差 | 好 | 结构化 == |

### 证据的读法

组合 A 是唯一同时满足全部四条硬性约束的选项，四条是集合论、不重叠、线性友好、字节码原生，它同时把语法糖预算留给多态之外的其他特性。它也是每个组件都有独立生产证据的组合：MoonBit derive、Swift 合成、Rust 的证据传递 `dyn`、Julia 式内联缓存、Roc 结构化相等性、Scala 受限消解作为逃生口。B 是 A 自然的扩展口，并共享它的运行期表示，Effekt 的能力模型是显式路线可扩展的证明。D 贡献元编程基质，即反射加 comptime 求值，宏不在其中。C 在一等模块需要抽象类型时贡献存在类型。E 不进 v1。没有任何组合需要 token 宏系统。一旦采纳通用结构化相等性，五个组合都有自洽的 derive 故事：A/B/E 直接成立，C 靠通用比较，D 靠反射，而这正是 derive 调研（第 5 节）强烈倾向的。本文件留给决策目录的开放设计问题有五条。(1) 不相交检查的准确算法及其错误信息。(2) 条件实例是否允许递归，终止规则是什么。(3) 受限具名实例，即轻量版 B，进 v1 还是延后。(4) `==` 的浮点相等性选择，Roc 的拆分对 MoonBit 的 IEEE 加前置条件。(5) comptime 反射进 v1，还是在内建 derive 够用之前只交付它们。

## 10. 参考资料

Rust：RFC 1210（impl 特化）。rust-lang/rust#31844 是跟踪 issue，写着 "unsound ... undefined behavior without unsafe code"，同时记录对未来 impl 的保守与生命周期分派窟窿。N. Matsakis，"Maximally minimal specialization: always applicable impls"，2018，讲生命周期擦除与 always-applicable 条件。PR #68970 记录 min_specialization 的限制：常量/关联类型、参数使用、谓词蕴含。issue #149257 于 2025 年报告 min_specialization 被平凡为假的生命周期边界弄成不可靠。GAT 计划的设计文档记录了用量调查与「太难学」顾虑。2024 年 Rust 现状调查显示 GAT 使用率低于 async trait。

Haskell：GHC 用户指南覆盖实例 pragma 与 Safe Haskell 的孤儿限制。HaskellWiki 孤儿实例页面给出导入约定，把孤儿实例放在一起。osa1，"Coherence and orphan instance rules"，2026。Freckle，"A Home for Orphan Instances"，2018。michaelpj，"Your orphan instances are probably fine"，2020，是反面观点。Serokell，"Learn From Errors: Overlapping Instances"。

集合论：Castagna、Nguyen、Xu、Frisch，"Polymorphic Functions with Set-Theoretic Types"（POPL 2015），提出重载即箭头交集与 `◁` 关系。Castagna，"Programming with Union, Intersection, and Negation Types"，2023 专著。Castagna、Duboc、Valim、Sussman、Lespiau、Ferreira-Corso，"The Design Principles of the Elixir Type System"（arXiv 2111.03354），按集合成员关系匹配分支。Parreaux、Chau，"MLstruct: Principal Type Inference in a Boolean Algebra of Structural Types"（OOPSLA 2022），把类型类列为兼容扩展。Marntirosian、Schrijvers、Thiemann、Steel，"Resolution as Intersection Subtyping via Modus Ponens"（OOPSLA 2020）。Xu、Huang、Oliveira，"Liberating Merges via Apartness and Guarded Subtyping"（OOPSLA 2025）。Parreaux，"Coherent Mergers" 研究笔记。

效应/能力：Brachthäuser、Schuster、Ostermann，"Effects as Capabilities: Effect Handlers and Lightweight Effect Polymorphism"（OOPSLA 2020）。"Effekt: capability-passing style for type- and effect-safe, extensible effect handlers in Scala"（JFP 2020）。Lee 等，"Effects, Capabilities, and Boxes"，讨论二等块。

性能：Bezanson、Karpinski、Shah、Edelman，"Julia: Dynamism and Performance Reconciled by Design"，2018。"Fast Flexible Function Dispatch in Julia"（arXiv 1808.03370），讲推断驱动的去虚化与 LU 案例数字。Matt Warren，"How generics were added to .NET"，2018，讲 CLR 按值类型的 JIT 实例化。Ungar、Chambers、Patterson，"Optimizing Dynamically-Typed Object-Oriented Languages With Polymorphic Inline Caches"（ECOOP 1991）。

线性/所有权：Hylo 规范给出参数约定 `let/set/inout/sink`、`~Copyable`，以及引用不作为类型的立场，见讨论 #754。Reinking、Xie、de Moura、Leijen，"Perceus: Garbage Free Reference Counting with Reuse"（ICFP 2021）。本地仓库证据：`koka/lib/std/core/types.kk` 里的 `pad` 类型，用于复用之下对齐构造器大小。

derive：Swift SE-0185（合成）、SE-0346（轻量同类型语法）、SE-0358（主关联类型）、SE-0352（隐式打开的存在类型）。moonbit-core 里的 MoonBit `builtin/traits.mbt` 记录 IEEE 相等性文档与默认方法，`builtin/tuple_eq.mbt` 记录元组元数 impl。Roc builtin 文档给出 `is_eq`、`is_float_eq`、`is_approx_eq` 三个名字与 `Dec` 的精确性。Kotlin data class 与 KEEP-367 上下文参数在 2025 年随 2.2 落地，KEEP-87 的 HKT 未被接受，Arrow 已弃用。

元编程：Zig 语言参考与 zig.guide 的 comptime 页面覆盖 0.15 的 comptime 参数、`@typeInfo`、以及每个 comptime 实参组合独立实例化。Mojo parameters 手册（mojolang.org）覆盖 `[]` 参数、只推断参数与 `comptime` 控制流。C++26 将加入静态反射（P2996）。Flatt、Findler，"Macros that Work Together"，2018，讲 Racket 阶段。Nim 手册覆盖 template/macro 与卫生。

本地源码证据（只读，仓库根目录下）：`C:/programs/me/DreamScript/moonbit-core/` 提供跨包的 derive 语法、trait 默认方法、NaN 文档与元组 impl。`C:/programs/me/DreamScript/koka/` 提供 Perceus 复用基础设施与基于处理器的效应。

