# 01 — 类型：决策目录

状态：阶段 2 选项目录，写于 2026-11-04，等待作者按 AGENTS.md 做出决策。输入：章程（AGENTS.md：集合论口味、单子式提取、可解释性预算）、研究笔记 01（线性性）、03（运行时/IR）、04（语法/易用性）、05（泛型/元编程），以及历史文件 README.md（集合指令）。本文件覆盖七个决策点：B1 类型基础、B2 类型推断、B3 积类型与和类型、B4 可空性、B7 数值塔、B8 字符串、B9 相等/排序/哈希。错误处理（B5）归 docs/zh/design/03-effects-errors-async.md 负责。线性性与所有权归 docs/zh/design/02 负责（正在并行撰写）。derive 机制归 docs/zh/design/05-generics-metaprogramming.md 负责。表层记号归 docs/zh/design/04-syntax.md 负责。本文件中的所有代码都是示例性的中性 TS 家族语法，表层语法由 docs/zh/design/04-syntax.md 决定。

阅读指南：每个决策点给出 3 到 4 个有名字的选项。每个选项包含一段具体的语法示例、先例及其出处（研究文件加章节，或 URL）、一行权衡，以及该选项排除的内容。每个决策点以一张对比表收尾（推断负担 / VM 表示 / 表达力 / 编译器复杂度 / 排除），接着是一段与其他设计轴的交互说明（B5 错误与 docs/zh/design/02 的线性性），以及一行的推荐。每一行 VM 表示所依据的运行时基线在下文「共享前提」中定义，来自 research/03。

## 决策点索引

| ID | 决策点 | 选项 | 推荐（一行） |
|---|---|---|---|
| B1 | 类型基础 | (a) setcore：结构化并集/交集/取反，(b) nomcore：名义 ADT，(c) hybrid：名义标签 + 集合代数，(d) setdecl：README 集合指令 + `any` | (c) hybrid：名义标签按集合论方式组合。保留 README 的集合表层，穷尽性由标签身份判定，运行时复杂度与 (b) 一样简单 |
| B2 | 类型推断 | (a) 双向推断 + 边界标注，(b) 全 HM 推断、无标注，(c) HM/流敏感 + 精化，(d) 处处标注 | 在 (a) 的双向骨架上叠加 (c) 的流敏感精化。全代数推断 (b) 仍是研究性赌注 |
| B3 | 积类型与和类型 | (a) 匿名结构化记录/变体，(b) 名义 struct/enum，(c) 可扩展行，(d) 元组 + 记录混合 | (d)：名义 struct/enum + 匿名元组，匿名记录字面量作为编译器合成的语法糖 |
| B4 | 可空性 | (a) 无 null，`T \| none` 并集，(b) 语言级 null + 严格流类型，(c) `T?` 语法糖脱糖为 (a) | (c)：语义与 (a) 的并集完全相同，`T?` 只是零语义别名。链式记号留给 docs/zh/design/04-syntax.md |
| B7 | 数值塔 | (a) Int/Float 默认，溢出即陷入（trapping），(b) 默认为任意精度，(c) 完整机器数值塔，(d) (a) + 延后 decimal | (a)，并把 (d) 的 decimal 作为后续的库类型。陷入是解释器下唯一可预测的策略 |
| B8 | 字符串 | (a) 不可变 UTF-8 字节 + 索引纪律，(b) 驻留字符串 + builder，(c) UTF-16/平台字符串，(d) (a) + rope | (a) 外加零拷贝的受检切片。rope 保持为库类型。FFI 转换放在边界 |
| B9 | 相等/排序/哈希 | (a) 通用结构化 `==`，(b) trait 中介的相等，(c) 内建结构化 + derive，(d) 引用同一性 | (a) 并带上 research/05 §5.2 的五条例外。Ord/Hash/Show 的 derive 归 docs/zh/design/05-generics-metaprogramming.md |

## 共享前提（对所有选项生效）

绑定默认不可变（章程硬性要求 5）。语法糖上限取 Kotlin 的全部表层，约 38 个特性，按成本加权 74 到 76 个单位，推荐预算为 20 个或更少（research/04 §2）。不做 C++ 式的特化，因为泛型实例不可能重叠的语言永远不需要优先级规则（research/05 §3）。在带类型的寄存器虚拟机上解释执行（章程硬性要求 1）。编译期错误优先于运行期错误（章程）。集合论类型是作者口味，尚未成为定论，本文件对结构化核心、名义核心与混合核心给予同等严谨的呈现。

来自 research/03 的运行时基线：带类型的寄存器字节码虚拟机。每个槽位的类型都是编译期常量，因此槽位直接存放未装箱的 i64/f64/bool/bytes 和裸 GC 引用，无需 NaN-boxing，也无需逐槽标签（research/03 的「Value representation」一节以及与 LuaJIT 带类型编码的对比）。ADT、并集和 Result 类值都是未装箱的标签加载荷对，铺在连续的栈帧槽位上，常见情形零堆装箱（research/03 的「Unboxed ADT/enum/Result layout」）。堆值携带固定的 8 字节头 `{ u32 type_id; u32 rc }`，这个头同时充当内联缓存与动态成员测试的键（research/03，research/05 §6.3）。栈帧布局对每个函数完全静态，正是这一点让 TCO 变成指针挪动，让 copy-and-patch JIT 无需守卫（research/03 的栈帧与 copy-and-patch 章节）。下面每个选项都按这套基线打分：任何强制装箱或逐槽标签的选项，都要支付 research/03 §b.10 所说的、静态类型本应消除的那 2 到 10 倍装箱开销。

## B1 — 类型基础

本点决定的内容：类型语言的原子是什么，以及并集、交集、差集是否为作用在这些原子之上的类型级运算符。这一个选择会确定 B5 关于错误能说什么，docs/zh/design/02 关于 `~Copyable` 的组合能说什么，README 的集合指令设想还能保留多少，以及穷尽性检查器的形状。章程把集合论口味写成作者偏好。真正的问题在于检查器和 VM 能承担多少集合代数。

### B1.a — `setcore`：以结构化集合类型为核心

所有类型都是结构化的。并集 `|`、交集 `&`、取反 `~` 在任何位置都是一等公民。两个结构相同的类型就是同一个类型。名义名字只是结构化集合的别名。

```ts
// B1.a -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
type Str      = Int | String              // union: value is a member, unwrapped, no wrapper
type Both     = Drawable & Resizable      // intersection: satisfies both structural shapes
type Plain    = User - Admin              // difference: user-shaped values that are not admins
type F        = (Int -> Str) & (Bool -> Int)  // overloaded function as intersection of arrows (research/05 §2.2)
fn area(s: Circle | Rect) -> Float {     // parameter is a set; coverage is set arithmetic
  match s { c: Circle => 3.14159 * c.r * c.r, r: Rect => r.w * r.h }
}
const j: Json = { port: 8080 }           // records are anonymous; same shape = same type
```

先例：MLscript/MLstruct 把并集、交集、取反做成一等公民，并声称可以在结构化类型的布尔代数上做主类型推断。它把类透明地视为其实例集合（research/04 §3，research/05 §2.3，https://github.com/hkust-taco/mlscript，MLstruct OOPSLA 2022，`https://lptk.github.io/files/[v6.2]%20mlstruct.pdf`）。语义子类型这条线（CDuce、Castagna POPL 2015、Elixir 的类型系统）在生产中提供基于集合的子类型判定和重载消解（research/05 §2.1-2.2）。Whiley 在其规范中提供带并集/交集/取反运算符的结构化类型（research/04 §3，https://whiley.org/pdfs/WhileyLanguageSpec.pdf）。

权衡：推断负担在四个选项里最高。MLscript 的主类型声明覆盖了这件事，但目前没有生产编译器实现该算法，而且被推断位置上的取反会强制进行补集推理。表达力最高：错误集合算术 `(T | E1 | E2) - E1` 是原生能力，穷尽的 match 就是集合覆盖，重载就是箭头类型的交集，配合可判定的不相交检查（research/05 §2.2）。编译器复杂度集中在检查器：语义子类型、空集可判定性，以及精确的「缺失成员是一次集合运算」诊断（research/04 §4）。这些机械都真实存在，规模有界。排除：名义封装（用私有字段表达身份，靠构造函数私有化来守卫类型不变式）、隐藏表示的抽象类型，以及任何 Go 式的结构化接口叙事都会变得多余。

### B1.b — `nomcore`：以名义 ADT 为核心

类型都有名字。enum 是封闭的和类型，struct 是命名的积类型。类型级只有类型别名和泛型，不提供并集/交集运算符。

```rust
// B1.b -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
enum Shape { Circle(f64), Rect { w: f64, h: f64 } }   // closed sum, nominal identity
struct User  { name: String, age: Int }               // nominal product
enum ReadError { Timeout, NotFound }                  // errors are enum cases
fn load(p: Path) -> Result[Config, ReadError] { ... } // success/error channel is a wrapper type
fn area(s: Shape) -> f64 {                            // exhaustiveness = cover the 2 enum cases
  match s { Circle(r) => 3.14159 * r * r, Rect { w, h } => w * h }
}
```

先例：Rust 的 enum/struct 配合经穷尽性检查的 match（research/04 §2.4，§12 Style D）。Gleam 刻意做小的名义核心，没有守卫，也没有并集（research/04 §2.3：9 个特性，成本 15，调研中最精简的一个）。权衡：推断负担最低，不同名义类型之间没有子类型关系，HM 式推断加上泛型就能覆盖全部情形。VM 表示最便宜，除 enum 之外每个槽位都是单一形状，enum 使用 research/03 已规定的未装箱标签加载荷布局。表达力最低，`(T | E1 | E2) - E1` 无法表达，错误集合增长会破坏针对错误 enum 的每一个 match，research/04 §3 所说的「扩容错误集合只破坏失败的 match，绝不会破坏 `or return` 链」也随之丢失。编译器复杂度最小，这正是 (b) 的真正优势，也是这个选项必须被认真对待的原因。排除：并集类型、超出 enum 匹配的跨类型流敏感收窄、Phi 的 null/错误/enum 单一机制叙事（research/04 §3），以及 research/04 §2.5 所指望的大部分语法糖节省（Kotlin 特性 4、5、6、7、8、27 只有在并集存在时才可能塌缩消失）。

### B1.c — `hybrid`：名义标签按集合论方式组合

声明是名义的（tag/struct 具备身份），但其上的类型语言是集合论的：名义原子的并集、交集、差集都是普通的类型表达式。类或 tag 就是它的实例集合。

```ts
// B1.c -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
tag Timeout; tag NotFound                                  // nominal atoms: distinct identity
struct BadPort { got: Int }                                // nominal, carries payload
type ReadError = Timeout | NotFound | BadPort              // set expression over nominal atoms
struct Config { host: String, port: Int }
fn load(p: Path) -> Config | ReadError { ... }             // union-carried errors, unwrapped
const ok: Config - ReadError = load(p).err((e) => default())  // difference shrinks the handler result
trait Animal; struct Dog: Animal; struct Cat: Animal       // Dog | Cat matches by tag identity
const a: Animal = Dog{}                                    // nominal subtyping by declaration
match a { d: Dog => ..., c: Cat => ... }                   // exhaustive via tag identity
```

先例：MLstruct 正是这个模型，看起来是名义的类声明，其类型在布尔代数中组合，并声称可以做主类型推断（research/04 §3，research/05 §2.3）。TypeScript 的可辨识并集是它的退化生产版：名义标签、并集、收窄，但没有类的交集（research/04 §3）。按标签身份做穷尽匹配，是所有被调研的穷尽性检查器的实际工作方式（research/04 §4）。权衡：推断负担中等，原子是名义的，所以大部分推断是普通的一元化。集合代数主要做检查而不做推断，这一点让 MLstruct 式的主类型保持可信（research/05 §2.3）。VM 表示与 (b) 一样简单，名义原子的并集使用未装箱标签加载荷布局，交集保留最精确成员的布局，并在堆头里用 `type_id` 支持成员内联缓存，差集类型在编译期完全擦除。表达力在值得的地方等同 (a)，即错误集合算术、收窄、穷尽并集，它放弃的是完整的结构化能力（匿名记录交集），调研中没有谁把它用好。编译器复杂度是名义原子上的不相交/空集检查器，比 (a) 的结构化空集检查严格更容易。排除：两个独立书写的同形状结构化类型自动统一（即 TS 的「形状相同即相等」性质），以及 `type A = { x: Int } & { y: Int }` 这类纯匿名集合定义，除非把匿名记录接受为语法糖（B3 的推荐正是这么做）。

### B1.d — `setdecl`：README 的集合指令作为声明，外加 `any`

历史 README（README.md 的「Typing」一节）提出把 `type A = B | C`、`type A = B & C`、`type A = B - C`、`type A = (B & C) - D` 作为原始声明形式。它还提出一个所有变量都归属的通用类型 `any`，以及规则「如果编译器无法推断某个变量的类型，就把它当作 `any`」。按要求如实评估如下：

```
// B1.d -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
type A = B | C;        // README verbatim: set instructions as declarations
type A = B & C;
type A = B - C;        // A is B but not C
type A = (B & C) - D;  // arbitrary set instructions
any                    // every variable is any; uninferred => any (README)
```

先例：Whiley 的规范把并集/交集/取反记录为类型运算符（research/04 §3）。TypeScript 只能通过 `Exclude<T,U>` 条件类型惯用法表达差集（research/04 §2.4 的差集/取反一行）。Go 的无类型常量是关于「字面量是一组类型」的唯一有原则的设计（research/04 §8，成本 S3）。权衡：集合指令这层表层本身是合理的，正是 (a) 和 (c) 以表达式形式提供的东西，区别在于它们把这层表层并入普通类型表达式，声明系统保持单一。把 `type A = ...` 声明为别名，是另外三个选项的共同做法。经不起推敲的是两处。第一处是静默回退「无法推断 => any」，它把每一次推断失败都转成动态类型，直接违反章程的编译期错误规则，正好产生 research/04 §3 用来警告的那种晦涩诊断，其代表是 Erlang 无标注的成功类型。第二处是在被减集合超出标签的有限并集时使用 `B - C`。此时穷尽性检查和 match 编译需要针对任意结构化操作数的语义子类型空集判定（可判定，research/05 §2.1，但是实打实的机械）。收窄必须跟踪谓词级事实，标签身份无法完成判定（Whiley 的重类型规则，research/04 §3）。运行期的值还要在检查器证明它位于差集之外的同时保留自己的具体表示。差集类型本身会被擦除，但每一次针对不可枚举差集的 match 都会编译成谓词链，取代标签跳转。在带类型的寄存器虚拟机上，集合代数在这个位置开始收费。排除：这个选项本身不排除什么，它就是披着 README 语法外衣的 (a) 或 (c)。但它的 `any` 回退排除了一个保证，即栈帧槽位布局在无关改动下保持稳定，因为槽位的表示会随推断成功或失败而改变。

### B1 对比表

| 轴 | a setcore | b nomcore | c hybrid | d setdecl |
|---|---|---|---|---|
| 推断负担 | 最高。主类型由 MLscript 声称，尚无生产编译器（research/04 §3，research/05 §2.3） | 最低。HM 加泛型，无子类型（research/04 §2.3 Gleam） | 中等。名义原子做一元化，代数主要做检查（research/05 §2.3） | 同其基础选项。`any` 回退会静默降级所有内容 |
| VM 表示（research/03） | 并集用标签加载荷对。交集与已擦除的取反。部分值因 type_id 内联缓存而装箱 | 除 enum 外每个槽位单一形状。静态栈帧最简单 | 标签并集用标签加载荷。交集取最精确成员的布局。差集擦除 | 基础成本加上带 type_id 标签的装箱 `any` 槽位 |
| 表达力 | 最大：并集、交集、取反、错误集合算术、重载（research/05 §2.2） | 只有封闭和类型。差集不可用，超出 enum 的收窄不可用 | 在名义原子之上支持错误集合算术与收窄，排除匿名结构化代数 | 同基础选项，README 表层原样保留 |
| 编译器复杂度 | 语义子类型 + 空集检查器 + 推断算法 | 对 enum 分支做覆盖检查 | 名义原子上的不相交/空集判定，按标签身份做覆盖检查 | 基础检查器加上谓词级差集跟踪（Whiley 式，research/04 §3） |
| 排除 | 名义封装、抽象类型 | 单一机制的 null/错误/enum（research/04 §3）、research/04 §2.5 的语法糖节省 | 完全匿名的结构化代数 | `any` 回退排除稳定的推断错误 |

### B1 与其他设计轴的交互

B5 错误（docs/zh/design/03-effects-errors-async.md）：只有在 (a) 和 (c) 中，研究推荐的、带差集类型处理器的并集承载错误才能工作（research/04 §11：`.err(h)` 的类型为 `(T | E) - E`）。(b) 强制使用 `Result[T, E]` 包装，强制手工嵌套，并带来 Style D 的补偿阶梯（research/04 §12）。(d) 是 (a) 或 (c) 加上一个 `any` 漏洞，未检查的错误可以从中流过，这是与 B5 的受检模型最差的组合。线性性（docs/zh/design/02，仅按路径引用）：`~Copyable` 的取反约束惯用法本身就是集合论的构造，即一个默认的正面 `Copyable` 约束被取反减去，并与并集/交集类型原生组合（research/01 的 Hylo 章节与横切注记 1）。(a)、(c)、(d) 直接接受它。(b) 必须把资源状态建模成 enum，代价是失去 `File[Open] | File[Closed]` 这种并集组合（research/05 §7.3），但仍能表达为 enum 分支。

**推荐（B1）：(c) hybrid**。名义标签按集合论方式组合，保留 README 的集合指令表层（以类型表达式形式保留，吸收 (d) 的语法并丢掉它的 `any` 回退），让穷尽性可按标签身份判定，把 VM 成本保持在 (b) 的未装箱标签加载荷水平，并且有最接近生产环境的证据（MLstruct、TS 的可辨识并集）。需要如实写出的反面：(b) 是风险最低、最容易改对的核心，它放弃的只有错误集合算术和 TS 的手感。如果检查器的不相交机械拖了进度，(b) 是可以零成本退回的备选。

## B2 — 类型推断

本点决定的内容：类型由谁书写，是程序员、一元化器，还是控制流，以及 B1 的集合代数是被推断出来的，还是只被检查。章程的可解释性预算和脚本语言的甜点区指向推断。反特化立场和编译期错误规则指向检查。

### B2.a — 双向检查，并在模块边界标注

顶层与公开签名必须标注。局部绑定、lambda 和 match 分支由标注出发做双向传播推断。

```ts
// B2.a -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
const parse_age = (s: String) -> Int | ParseError { ... }  // boundary: annotated, checked
const parse_user = (row: Row) -> User | ParseError {       // boundary: annotated
  const age_text = row.get("age") or return Missing("age") // local: inferred
  const age      = parse_age(age_text)?                    // local: inferred from parse_age
  User { name: row.get("name") or "", age }                // local: inferred
}
```

先例：Rust 要求条目签名，推断局部。Koka 与 MoonBit 遵循同样的纪律（research/04 §6 的推荐：`name: T`，局部推断，顶层签名必须写）。TypeScript 使用带上下文类型的双向检查（research/04 §2.3）。权衡：推断负担有界且局部，检查器从不需要为未标注的条目凭空造一个类型，这正是 B1 的代数可被检查而无需被推断的原因。VM 成本为零。表达力不变，标注同时充当 API 文档。编译器复杂度低，走的是标准双向流水线。排除：模块顶层的脚本式零标注代码，以及「编译器会把一切搞明白」这类说法。

### B2.b — 完整 Hindley-Milner，无标注

一切由一元化推断。标注在任何位置都是可选的。

```ocaml
(* B2.b -- illustrative; surface syntax decided in docs/en/design/04-syntax.md *)
let f x = match x with Some n -> n + 1 | None -> 0
let g = List.map f
(* g : int option list -> int list, inferred; nothing was written *)
```

先例：OCaml 和 Haskell 已经在用。Koka 大量依赖推断（research/04 §2.3，OCaml 14 个特性）。权衡：推断负担是经典 HM 代价，主类型要求没有子类型，所以纯形式的 (b) 与 B1 的并集冲突。把集合论子类型加进 HM 推断，正是 MLscript/MLstruct 的研究纲领，其主类型结论（research/05 §2.3，OOPSLA 2022）背后没有生产编译器。若不含集合代数，(b) 只需 HM 一元化加泛型，B1 也随之塌缩为 (b) nomcore。VM 成本为零。表达力在纸面上最高。含代数时编译器复杂度高，而且在并集/取反参与一元化时，错误信息是一个尚未解决的 UX 问题。排除：必须写出的 API 签名（文档会受损），以及关于推断何时成功的任何可预测说法。README 的「无法推断 => any」回退正是这个选项制造出来的诱惑。

### B2.c — 流敏感精化（叠加在 a 或 b 之上）

局部变量的类型通过 `is` 测试、null/none 测试和 match 分支收窄，并集单调精化。只有赋值会重置，而默认不可变让赋值变得罕见（research/04 §3：TS 收窄的尖锐边缘是闭包、可变性与 await，前两者在 `val` 之下消失）。

```ts
// B2.c -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
const j = json.get("port")          // j : Json | none
if j is Int && j >= 1 && j <= 65535 {
  serve(j)                          // j : Int here; no cast, no wrapper
}
match v {
  n: Int    => n + 1,               // arm narrows v to Int
  s: String => s.len(),             // arm narrows v to String
}                                   // exhaustiveness checked as set cover (research/04 §4)
```

先例：Whiley 在结构化类型和可更新数据上提供流类型，用 `is` 和 null 测试收窄，并在规范中给出重类型规则（research/04 §3，https://whiley.org/pdfs/WhileyLanguageSpec.pdf）。TypeScript 通过 `typeof`/`is`/可辨识字段收窄，并记录了自己的重置时机（research/04 §3）。Lobster 表明收窄对一个小型游戏脚本实现来说足够便宜（research/04 §3 的流敏感类型调研）。Kotlin 的智能转换属于 S2 的表层成本（research/04 §2.2 第 8 项）。权衡：推断负担为零，精化只缩小已知类型，在一元化过程中从不构造并集或交集，这是它能与交集/取反组合而不指数爆炸的结构性原因（Whiley 和 TS 都在没有求解器机械的情况下提供该能力。集合类型系统里的爆炸风险集中在推断函数类型的交集，也就是 Castagna 的 ◁ 关系，那里的实证成本是两两空集检查，随重载组大小平方增长且可缓存，research/05 §2.2）。VM 成本为零，收窄发生在编译期。表达力方面，research/04 §1 称这是杠杆最高的单项表层决策。编译器复杂度是一个作用于局部变量的流敏感环境，中等，而默认不可变消除了两个难点（可变性、闭包捕获）。排除：穿过可变字段与跨循环回边的收窄，这是每个流类型系统都有记录的难点（research/04 §3 的已知限制 2），在不可变之下可以接受，但若日后要求并集类型字段具备 `var` 可变性，这就是一堵真墙。

### B2.d — 处处必须标注

Zig/Go 的纪律：每个绑定、每个参数都标注。检查器从不推断。

```zig
// B2.d -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
const port: Int = cfg.port or 8080;
const name: String = row.get("name") or "";
fn area(s: Shape): Float = match s { ... }
```

先例：Go 和 Zig（research/04 §2.3：9 个和 13 个特性，刻意做到接近零的基线）。权衡：推断负担为零，编译器最简单，诊断最好（每个表达式都有写出来的类型）。表达力不变，但对脚本甜点区而言易用性崩掉，Go 和 Zig 正是作者不喜欢的两门语言，冗长就是原因。VM 成本为零。编译器复杂度最小。排除：TS/bun 手感的局部代码、REPL 驱动的脚本，以及章程使命里提到的解释型语言的一半意义。

### B2 对比表

| 轴 | a 双向 + 边界 | b 完整 HM | c 流敏感精化 | d 处处标注 |
|---|---|---|---|---|
| 推断负担 | 有界且局部。边界为代数提供锚点 | 主类型。与子类型冲突，要加入 B1 代数需要 MLscript 级别的研究（research/05 §2.3） | 负担为零，只缩小已知类型 | 零 |
| 与交集/取反组合而不爆炸 | 是，代数在已标注边界处做检查 | 在生产规模上未知，只有研究层面的主张（research/05 §2.3） | 是，单调缩小。唯一的集合类型成本是重载空集检查，随组大小平方增长且可缓存（research/05 §2.2） | 是，显然成立，因为一切都写出来了 |
| VM 表示成本 | 无 | 无 | 无 | 无 |
| 表达力 | 完整，并且 API 自带文档 | 完整 | 在局部变量上完整，是杠杆最高的表层特性（research/04 §1） | 完整但嘈杂 |
| 编译器复杂度 | 中低（标准流水线） | 含代数时高，不含时中等 | 中等（流环境，属标准做法） | 最小 |
| 排除 | 顶层零标注 | 必须写出的签名、可预测的推断 | 并集类型字段可变情况下的收窄（research/04 §3） | 脚本语言的易用性 |

### B2 与其他设计轴的交互

B5 错误：(a)/(c) 让 `?` 的传播检查变得平凡，外层返回并集写在边界处，包含关系就是一次集合运算（research/04 §11）。(b) 必须推断返回并集，而这里正是一元化与并集相撞的地方。(d) 到处写并集，安全但吵闹。线性性（docs/zh/design/02）：双向检查是仿射流分析已知可行的底料，检查器本来就要跟踪最后一次使用，写出的边界签名给线性检查器提供固定事实。线性性之下的完整 HM 在实践中尚未解决（research/05 §7.1 的 Hylo/Koka 证据都使用受检签名）。

**推荐（B2）：在 (a) 的双向骨架上叠加 (c) 的流敏感精化**。必须写出的边界签名把 B1 的集合代数锚定在检查而非推断之上，精化是单调的，因此能与交集/取反组合而不爆炸（research/05 §2.2），而且每个部件都有生产环境的证据（Rust、TS、Whiley）。需要如实写出的反面：不含集合代数时，(b) 实现起来确实更简单。「完整 HM + 完整集合代数」属于研究性赌注，这个项目不应在 v1 押上。

## B3 — 积类型与和类型

本点决定的内容：记录和变体如何声明、命名和匹配，包括标签的易用性、变体的开放与封闭，以及穷尽性的代价。

### B3.a — 匿名结构化记录 + 变体

记录就是它的字段集合。两个同形状的类型就是同一个类型。变体是带标签记录形状的并集。

```ts
// B3.a -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
type Point = { x: Int, y: Int }                        // anonymous; {x:1,y:2} has this type by shape
type Shape = { kind: "circle", r: Float }
           | { kind: "rect",  w: Float, h: Float }     // variant = union of tagged shapes
fn area(s: Shape) -> Float {
  match s { c: { kind: "circle" } => 3.14159 * c.r * c.r, r: { kind: "rect" } => r.w * r.h }
}
const p: { x: Int, y: Int } = { x: 1, y: 2 }           // any same-shape record assigns
```

先例：TypeScript 的对象类型与可辨识并集（research/04 §2.4）。OCaml 的多态变体是开放变体版本（属于稳定表层的常识，research/04 §2.4 的「Sum types p (discriminated)」一行）。权衡：标签易用性方面，标签是唯一身份，所以错别字会变成类型错误（这是好事），但两个库中同名而形状不同的记录会按设计意图冲突（TS 的胶水世界性质）。开放与封闭方面，变体按书写即为封闭，开放变体需要行机械（也就是选项 c）。穷尽性方面，它是对并集的集合覆盖，配合 research/04 §4 的差集诊断。VM 成本方面，匿名形状无法共享已声明的布局，每个记录字面量的布局都在使用点由其形状推导（未装箱本身没问题，基于 type_id 的内联缓存在此需要形状哈希，而名义类型只需一个 id）。编译器复杂度方面，形状归一与去重是 TS 真实付出的工作量。排除：名义封装，以及在没有形状锁定纪律的情况下跨版本稳定的库布局。

### B3.b — 名义 struct/enum 配合模式

记录和变体都经声明、命名，并按构造器匹配。这是 Rust 的形状。

```rust
// B3.b -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
struct Point { x: Int, y: Int }
enum Shape {
  Circle { r: Float },
  Rect { w: Float, h: Float },
}
fn area(s: Shape) -> Float {
  match s { Circle { r } => 3.14159 * r * r, Rect { w, h } => w * h }
}
```

先例：Rust 和 Gleam（research/04 §2.4，§12 Style D）。针对已声明分支的穷尽性检查是调研中实现得最多的检查器（research/04 §4 的表格，全部为是）。权衡：标签易用性方面，每个一次性聚合体都需要声明（Gleam 表明实际成本很低，research/04 §12 Style B）。变体按构造即封闭，穷尽性平凡。VM 成本最优，布局已声明，每个类型一个 type_id，正是 research/03 的未装箱标签加载荷叙事。编译器复杂度最小。排除：临时性的结构化胶水（`{x: p.x, y: p.y}` 无需类型即可传入），这部分由 B3.d 的元组和匿名记录语法糖找回。

### B3.c — 可扩展行（带行变量的记录）

记录携带一个行变量表示「其余字段」。函数接受任何至少包含它所需字段的记录。Koka 的效应行是同一套机制，应用对象是效应。

```ts
// B3.c -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
fn loud(r: { msg: String | rho }) -> { msg: String | rho } {   // accepts any record with msg: String
  { r with msg: r.msg.upper() }                                 // extends/preserves the rest of the row
}
loud({ msg: "hi", at: now() })    // at flows through untouched; rho = { at: Time }
```

先例：Koka 的效应行是行多态可用的生产证据（research/04 §2.4 的「effect rows」，§3 的「Zig 的错误集合合并与 Koka 的效应行是同一现象」）。OOPSLA 2020 的消解演算把可扩展记录纳入交集子类型消解之下（research/05 §2.3）。PureScript/Elm 的行属于稳定表层的常识。权衡：对「接受任何带这些字段的对象」这类 API，标签易用性极好。开放与封闭方面，行按定义就是开放的，所以对行类型被检查值的穷尽性只能是部分的（行类型的完整分支集合在函数处未知）。推断负担在这里最高，行一元化是产生难以阅读的推断类型的经典来源，Koka 为效应支付了这套机械，因为效应行没有替代做法，而 Gleam/Roc/MoonBit 都在数据上拒绝了它。VM 成本方面，行类型参数的布局在静态上未知，迫使在行边界装箱，或使用按形状的内联缓存（research/05 §6.3 的证据词）。编译器复杂度方面，本文件里每单位语法糖的复杂度最高。排除：行类型值上的简单穷尽匹配语义，以及错误中推断签名的可预测性（行变量会泄漏进诊断信息）。

### B3.d — 元组 + 记录混合（Gleam/MoonBit 形状）

命名数据用名义 struct 和 enum。一次性聚合体用匿名位置元组。行变量全部省去。

```rust
// B3.d -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
struct User { name: String, age: Int }            // nominal record, punning supported (moonbit-core evidence)
enum Shape { Circle(Float), Rect(Float, Float) }  // tuple-enums: Gleam/MoonBit shape
let kv = ("host", 8080)                            // anonymous tuple, positional
match kv { (k, v) => println("${k}=${v}") }        // destructuring (research/04 §4)
const point = { x: 1, y: 2 }                       // anonymous record literal: sugar over a synthesized nominal record
```

先例：MoonBit 的 struct、元组与 enum，支持记录字段缩写（punning），已在 moonbit-core 源码中核实（research/04 §14，research/05 §5.1）。Gleam 的记录加首参数元组（research/04 §2.3，9 个特性）。research/04 §2.5 保留集中的匿名记录字面量一项，正好对应语法糖那一半。权衡：标签易用性在需要的地方足够好（已声明字段），在不需要的地方改为位置（元组）。变体封闭，穷尽性平凡。推断负担低，名义原子做一元化，元组是结构化积类型，行变量不参与。VM 成本最优，布局已声明，元组是固定大小的未装箱连续区段。编译器复杂度低。排除：「接受任何带这些字段的记录」这类 API（可以用最小 trait 上的泛型或声明一个视图类型找回）以及开放变体。

### B3 对比表

| 轴 | a 匿名结构化 | b 名义 struct/enum | c 可扩展行 | d 元组 + 记录 |
|---|---|---|---|---|
| 标签易用性 | 标签即身份，错别字会变成类型错误。两个库中同名而形状不同的记录按设计冲突 | 标签已声明，一次性聚合体也要声明 | 最擅长「至少包含这些字段」式的易用性 | 已声明加位置，支持字段缩写 |
| 开放与封闭 | 按书写即封闭，开放则需要行 | 按构造即封闭 | 天生开放 | 封闭 |
| 穷尽性 | 对并集形状做集合覆盖 | 对已声明分支做覆盖 | 部分（行是开放的） | 对已声明分支做覆盖 |
| 推断负担 | 形状归一 | 最低 | 最高（行一元化） | 低 |
| VM 表示（research/03） | 按形状推导布局。内联缓存需要形状哈希 | 已声明布局，每个类型一个 type_id，最优 | 行边界处布局未知：装箱或按形状的内联缓存 | 已声明布局加上固定大小的未装箱元组连续区段 |
| 编译器复杂度 | 中等（形状去重） | 最小 | 每单位语法糖最高 | 低 |
| 排除 | 名义封装 | 临时结构化胶水（由元组 + 语法糖找回） | 行上的穷尽匹配 | 「至少包含这些字段」式 API |

### B3 与其他设计轴的交互

B5 错误：错误载荷是积类型。(b)/(d) 已声明的错误类型为未装箱的错误返回提供稳定布局（research/03 的 Result-in-registers）。(a) 的匿名错误形状也能工作，代价是把它们哈希归一进内联缓存机械。线性性（docs/zh/design/02）：(b)/(d) 的已声明布局让丢弃顺序和作用于作用域结束的丢弃在每个栈帧上都静态可知（research/03 的丢弃纪律注记）。匿名形状会迫使检查器做布局推导，从而让逐字段的仿射跟踪变复杂。

**推荐（B3）：(d) 元组 + 记录混合**。名义 struct/enum 加匿名元组，并把 research/04 §2.5 保留集里的匿名记录字面量作为编译器合成名义记录的语法糖，从而在 (b) 的运行时成本上找回选项 (a) 的临时便利。行 (c) 是本目录里每单位语法糖最贵的机械，它唯一无法回避的用途（效应行，如果采纳）属于 docs/zh/design/03，与数据无关。

## B4 — 可空性

本点决定的内容：「缺失」是一种值族（`none` 式的单位类型并集）、一个特殊的 `null`，还是两者同时存在（对并集的语法糖）。Phi 的论点是 null、错误和 enum 属于同一个特性：并集中的单位类型，`is` 收窄，`or` 提取（research/04 §3，https://www.arshad.fyi/writings/null-errors-and-enums-are-one-feature）。三个选项都围绕这个论点展开。

### B4.a — 没有 null：`T | none` 并集纪律

不存在 null 字面量。缺失就是单位类型 `none`（唯一值就是它本身的类型）。可选值就是普通并集。值以未包装的形式进入。

```ts
// B4.a -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
type Port = Int | none
const p: Port = cfg.port              // no Some wrapper; the Int is the Int
const q: Int = p or 8080              // `or` extraction: first member wanted (research/04 §3)
if p is Int { serve(p) }              // narrowing; no cast
fn find(m: Map[String, V], k: String) -> V | none { ... }
```

先例：Phi 的单位类型机制，`type Timeout` 既是一个类型也是它唯一的值。可选值就是 `T | none`，其中没有 `Some`。enum 是单位类型的并集。一个特性顶替三个（research/04 §3）。权衡：推断负担为零（并集按 B1 已经存在）。VM 成本是未装箱的标签加载荷对，而当 `T` 存在未使用的位模式时（引用永不为零，f64 有 NaN 位模式而 research/03 拒绝使用），一个 niche 优化可以在后续优化器阶段把 `T | none` 压进一个槽位，且不改变语义。表达力方面是单一机制叙事，`none`、错误成员和 enum 分支共用 `is`/`or`/match。编译器复杂度除了 B1 之外为零。`none` 作为若干单位类型之一会破坏什么吗？不会，单位类型是普通值，与并集、收窄和相等都能像任何成员一样组合（research/04 §3）。唯一有记录的破坏是扁平化歧义：当 `V` 本身可能是 `none` 时，`V | none` 让已存储的 `none` 与缺失的键无法区分。TypeScript 共有这个限制，修法正是在有歧义的位置显式包装成 `Found[V] | none`（research/04 §3 的已知限制 1）。排除：除了一个能流经所有泛型的通用 `null` 之外，什么都不排除，而这正是重点。

### B4.b — 语言级 null + 严格流类型

`null` 是一个特殊的通用单例。`T?` 式可空性作为类型系统的一个谓词，与并集成员分开。流类型分析负责证明非空性（Kotlin、TS strictNullChecks、Whiley）。

```kotlin
// B4.b -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
val p: Int? = cfg.port                // null is the special missing value
val q: Int = p ?: 8080                // elvis
if (p != null) { serve(p) }           // smart cast on null test (Whiley proves this with updateable data)
```

先例：Kotlin 的 `?`、elvis、`!!` 和智能转换（research/04 §2.2 第 4 到第 8 项）。TS 的 strictNullChecks（research/04 §3）。Whiley 在可更新结构上证明了流类型能够承载非空纪律（research/04 §3）。权衡：推断负担为零。VM 成本方面，`T?` 同样是标签加载荷对，但 `null` 的通用性使针对 `T?` 的泛型代码无法像按实例化的封闭并集那样直接做 niche 优化。表达力方面，`null` 是处理缺失的一种机制，却与错误并集和 enum 并存成第二种机制（Phi 的观点：一处足够的地方用了三个特性，research/04 §3）。此外 elvis、`!!`、`?.` 这些写法会不断增殖（Kotlin 在其 38 个语法糖特性里有 4 个花在这里，research/04 §2.2）。编译器复杂度方面，null 流跟踪就是选项 (c) 的收窄限于一种类型，难度相同。排除：把 `none` 式的单位成员当作可与 `none` 互换的泛型哨兵（影响不大），以及 B5 的统一，即 `?`/`or` 在 `T | none` 和 `T | E` 上行为一致，这是反对 (b) 最强的论据（research/04 §11）。

### B4.c — 可空后缀语法糖 `T?` 脱糖为 (a)

取 Kotlin 的表层，底层是并集语义：`T?` 恰好等于 `T | none`。类型集合与运算符语义都保持原样。

```ts
// B4.c -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
const p: Int? = cfg.port              // sugar: T? == T | none, nothing else
const q: Int = p or 8080              // same `or` as errors and enums (research/04 §11)
p?.address?.city                      // optional chain: a thin desugar, token choice deferred to docs/en/design/04-syntax.md
if p is Int { serve(p) }              // same narrowing as B4.a
```

先例：Kotlin 的 `T?` 表层（research/04 §2.2 第 7 项，成本 S2）叠加在并集语义之上。research/04 §11 明确把 `?.` 排除在 v1 之外（「把 `x?.f` 脱糖为 `x or none-map` 这种轻薄做法，可以在真实代码有需求时后加。为了尊重预算，v1 先不做」），由 `or`/`is` 承担负载。权衡：推断、VM 与编译器成本与 (a) 完全相同，后缀是纯粹的别名。表达力与 (a) 相同，另外多了一个熟悉的写法。唯一的新决策是链式记号，本文件按章程把它留给 docs/zh/design/04-syntax.md（研究笔记已经把它计为 S1 到 S2 并标出风险）。排除：反过来的做法，即让 `T?` 脱离并集而具有独立语义（也就是 (b)），会被这个选项的脱糖选择排除。

### B4 对比表

| 轴 | a `T \| none` 并集 | b 语言级 null + 流类型 | c `T?` 语法糖叠加在 (a) 上 |
|---|---|---|---|
| 推断负担 | 除 B1 外没有 | 除 B1 外没有 | 除 B1 外没有 |
| VM 表示 | 未装箱标签加载荷，后续可做 niche 压缩 | 标签加载荷。`null` 通用，niche 优化更弱 | 与 (a) 相同 |
| 表达力 | null/错误/enum 单一机制（research/04 §3） | 只对 null 是单一机制，对错误/enum 是第二种机制 | 单一机制，外加熟悉的写法 |
| 编译器复杂度 | 只有 B1 的检查器 | null 流跟踪（B2.c 的子集） | 只有 B1 的检查器 |
| 语法糖成本 | 额外为 0（`is`/`or` 共享） | elvis + `!!` + `?.` + `T?`（4 个 Kotlin 特性，research/04 §2.2） | 1 个别名 |
| `none` 是否破坏什么 | 只有 V\|none 扁平化。在有歧义处显式包装，与 TS 的限制相同（research/04 §3） | 共享的 null 简化了该歧义，代价是多出 `!!` 形状的逃生口 | 同 (a) |
| 排除 | 通用 null | B5 关于并集上提取的统一 | 除 (a) 之外没有 |

### B4 与其他设计轴的交互

B5 错误：(a)/(c) 让 `T | none` 与 `T | E` 在字面上是同一个构造，因此 `?`、`or`、`is` 只需在并集上陈述一次，整个可选值与错误叙事一共三个记号，对比 Kotlin 的四个和 Swift 的五个（research/04 §11）。(b) 迫使 B5 维持一套并行词汇。线性性（docs/zh/design/02）：并集成员测试对仿射值的消耗或收窄恰好发生一次。在 `x` 为 `~Copyable` 时，`x or default` 必须把被选中的分支视为已移动。这与 research/05 §5.2 第 4 条断点要求的「借用后比较」形状相同，无论哪个选项胜出，docs/zh/design/02 都要面对它。

**推荐（B4）：(c)**。语义完全采用 (a) 的并集，`T?` 作为 `T | none` 的零语义别名，理由是作者的 Kotlin/TS 肌肉记忆期待这个写法，而它不花任何成本。`none` 作为若干单位类型之一不会破坏什么，唯一例外是有记录的 `V | none` 扁平化情形，TypeScript 同样如此，在有歧义处显式包装即可。`?.` 式的链式记号按 research/04 §11 的倾向留给 docs/zh/design/04-syntax.md：先不进 v1，只有当真实代码有需求时才作为脱糖加入。

## B7 — 数值塔

本点决定的内容：默认的整数与浮点类型、字面量规则，以及溢出策略。解释器这一背景具有决定性：research/04 §8 推荐 Swift 式的总是陷入，配合显式的环绕运算符，称其为「解释器下唯一可预测的溢出策略」。research/03 推荐把溢出定义为 panic，取代任意精度回退，从而让算术的常见路径零检查（LuaJIT 的 ADDOV 注记）。

### B7.a — 默认 `Int`（i64）/ `Float`（f64），溢出即陷入

字面量默认为 Int/Float。后缀选择宽度。宽度转换必须显式书写。溢出即陷入。环绕运算必须显式书写。

```ts
// B7.a -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
const n = 1_000_000          // Int (i64)
const x = 2.5               // Float (f64)
const b = 0xFF              // 255, still Int
const s = 42u8              // explicit width via suffix
const f = 2.0f32            // f32
const w = a +% b            // explicit wrapping add (Swift/Zig spelling family)
const t = i32(u)            // explicit conversion; no implicit narrowing (research/04 §8)
```

字面量语法：`1_000_000`、`0xFF`、`0b1010`、`0o777`，后缀 `u8/i8/.../i64`、`f32/f64`。无后缀整数默认为 Int，除非带标注的上下文强制要求某个带后缀宽度。无后缀浮点默认为 Float。溢出行为表：

| 表达式 | 行为 |
|---|---|
| `9223372036854775807 + 1` | 确定性陷入（panic）。可复现、可调试，在字节码循环里开销低（research/04 §8） |
| `a +% b`, `a -% b`, `a *% b` | 按 2^64 取模环绕，用于哈希/PRNG（research/04 §8） |
| `1.0 / 0.0` | `+inf`（IEEE 754，各处皆然） |
| `1 / 0` | 默认陷入。返回错误并集的除法变体属于 B5（docs/zh/design/03-effects-errors-async.md） |
| `f32(i)` / `i64(f)` | 只允许显式转换，隐式窄化的写法在任何位置都由检查器报错（research/04 §8） |

先例：Swift 总是陷入，`&+` 是显式逃生口。Zig 在安全模式下 panic，另有 `+%` 环绕运算。Rust 在 debug 下 panic、在 release 下环绕（已否决：不同构建行为不同）。Go/Kotlin 静默环绕（已否决：不可预测）。以上来自 research/04 §8 的推荐。权衡：推断负担为零。VM 成本最优，i64/f64 直接放在未装箱槽位里，`add_i64` 就是一条机器加法，没有标签（research/03 的带类型编码示例），而陷入是一次标志位检查，copy-and-patch 层可以把它折叠掉。表达力方面有 64 位天花板，可以用库 BigInt 作为逃生口。编译器复杂度最小。排除：偏爱静默环绕的用户（哈希代码需要显式改用一次 `+%`）以及隐式宽度强制转换。

### B7.b — 默认任意精度 Int

默认整数永不溢出。机器字需要标注才会使用。

```py
# B7.b -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
const n = 2 ** 100 + 1        # exact, no overflow, ever
const m = n * n               # grows; allocates
const fast: I64 = 42          # machine word is opt-in
```

先例：Python 的整数和 Julia 的类型提升（research/04 §8，成本 S3 的「慢解释器路径」）。权衡：推断负担为零。VM 成本是致命的一环。要么每个 Int 都是装箱的 bignum（这等于整体废除 research/03 的未装箱槽位设计），要么采用小整数与 bignum 的双重表示加溢出切换，后者正是 research/03 标出的、会在静态类型下存活下来的那类守卫检查（LuaJIT 的 ADDOV），并明确建议改用 panic。JIT 层下的类型稳定性会像 Julia 那样在值跨越小/大边界时退化。表达力方面，溢出错误无法再出现，代价是原本假设 O(1) 的地方会冒出不可预测的 O(n) 数位算术。编译器复杂度高（每条算术操作码都要 bignum 快路径）。排除：在标量代码上达到 C 的 3 到 10 倍区间的解释器目标（research/03 §b.10 假设未装箱标量），以及 Int 可预测的栈帧槽位大小。

### B7.c — 完整的机器数值塔 i8..i128 / f16..f128，宽度多态的标准库

所有宽度都是一等公民。标准库对宽度做泛型。字面量通过后缀或默认值选择宽度。

```rust
// B7.c -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
let a: i8  = 127;
let x: f16 = 1.5f16;
fn sum[T: Num](xs: List[T]) -> T { ... }   // width-polymorphic stdlib: evidence per width
sum([1i8, 2i8])                            // resolves T = i8
```

先例：Rust 和 MoonBit 都通过 trait 提供完整数值塔和宽度多态的标准库（research/05 §1.1，moonbit-core 的 numeric 包）。research/03 §b.8 给出了运行时的答案：由运行时提供按表示形式的代码（「与 copy-and-patch 层使用的同一套按表示形式的模板」），从而消除数值字典传递带来的经典装箱开销。权衡：推断负担方面，无约束的整数字面量需要默认值（Rust 的 i32 回退规则），并带来宽度歧义错误。VM 成本按宽度来看很好（每个槽位就是它的原始宽度），但多态标准库意味着每次泛型数值调用都要付出证据词，直到内联缓存预热（research/05 §6.3），而 f16/f128 在常见硬件上走软件路径。表达力对系统编程最大，在脚本甜点区基本闲置。编译器复杂度方面需要一个数值 trait 家族、按宽度的代码路径和歧义规则。排除：这个选项不排除其他任何选项（它是超集），但它会花掉检查器和标准库预算，而章程的可解释性规则更愿意把这些预算花在别处。

### B7.d — (a) 外加延后的 decimal/定点单位

完全采用 (a)，再加一份计划：decimal 和定点数以库类型（名义包装）形式出现，留在库层。计量单位留在库层（research/04 §8：F# 的计量单位是 S3，会触及每一个数值。九成情形只需一个名义包装加 derive）。

```ts
// B7.d -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
const price = Dec("19.99")           // library type: exact decimal (Roc Dec precedent, research/05 §5.1)
price == Dec("19.99")                // 0.1 + 0.2 == 0.3 holds by construction in Roc's Dec
type Meters = Wrap[Int]              // library-level units via nominal wrappers + derives (research/04 §8)
```

先例：Roc 的 `Dec` 默认十进制让 `0.1 + 0.2 == 0.3` 按构造成立（research/05 §5.1 的内建文档）。F# 的计量单位作为被否决的内建路线（research/04 §8）。权衡：承担 (a) 的全部成本，另外在货币与精确性场景引入一个库依赖。内建成本为零。排除项为空，它就是 (a) 加上一条写明的成长路径。

### B7 对比表

| 轴 | a 默认陷入 i64/f64 | b 默认 bignum | c 完整数值塔 | d (a) + decimal 计划 |
|---|---|---|---|---|
| 推断负担 | 无 | 无 | 字面量宽度默认值 + 歧义错误 | 无 |
| VM 表示 | i64/f64 直接放在未装箱槽位。陷入即标志位检查（research/03） | 装箱 bignum，或带溢出守卫的双重表示，即 research/03 否决的那类守卫 | 按宽度的原始槽位。多态标准库运算要付出证据词 | 同 (a)。Dec 是装箱的库类型 |
| 表达力 | 64 位天花板，BigInt 库作为逃逸 | 永不溢出，代价是出现 O(n) 意外 | 最大宽度范围 | (a) + 精确货币路径 |
| 解释器可预测性 | 最好：确定性陷入，常见路径零检查（research/04 §8，research/03） | 最差：计时依赖分配，JIT 类型稳定性悬崖 | 按宽度良好 | 最好 |
| 编译器复杂度 | 最小 | 高（每条算术操作码里都有 bignum） | 中高（trait、按宽度路径、软件 f16） | 内建部分最小 |
| 排除 | 静默环绕 | 未装箱 Int 槽位（research/03） | 没有（它是超集） | 没有 |

### B7 与其他设计轴的交互

B5 错误：除零和溢出是算术错误并集的成员（README 中 `ArithmeticError` 的直觉）。在 (a) 之下它们默认陷入，任何返回错误的变体都是类型为 `T | ArithError` 的库函数，正好符合 docs/zh/design/03 的并集承载模型。线性性（docs/zh/design/02）：数值是显然 `Copyable` 的情形，未装箱的 i64/f64 槽位正是让别处线性记账变便宜的原因。(b) 的装箱 bignum 会把 Int 也变成受 RC 管理的值，让每一次仿射流都掺进分配点。

**推荐（B7）：(a)，并把 (d) 的 decimal 作为写明的库成长路径**。溢出即陷入是解释器下唯一可预测的策略（research/04 §8），让算术的常见路径保持零检查（research/03 明确建议不要 bignum 回退守卫），保住未装箱的 Int 槽位，后缀与成本叙事也和 research/04 §8 已选定的保留集一致。如果系统编程的需求出现，完整的数值塔 (c) 可以日后沿着数值 trait 这个扩展点生长出来。

## B8 — 字符串

本点决定的内容：编码、索引纪律、驻留，以及 VM 在槽位里放什么、在堆上放什么。在 research/03 的基线之下，字符串永远是堆对象，槽位里只放一个裸 GC 指针，所以真正的问题是字节格式、相等是 O(n) 还是 O(1)，以及切片是否复制。

### B8.a — 不可变 UTF-8 字节字符串，索引安全纪律

堆上是 UTF-8 字节。O(1) 字符索引由此排除。切片边界受检。迭代按字符进行。字形簇放在标准库。

```rust
// B8.a -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
const s = "héllo"            // 6 UTF-8 bytes on the heap; slot holds the pointer
s.byte_len()                 // 6
s.slice_char(0, 2)           // "hé" | none  -- checked boundary, union return (research/04 §7)
s.chars()                    // iterator over Unicode scalar values
s.graphemes()                // stdlib, not core (research/04 §7)
s.utf8_bytes()               // escape hatch: raw bytes
```

先例：Rust 的 `&str`，UTF-8，字节索引必须落在字符边界上，提供 `char_indices`/`chars` 迭代器，字符索引为 O(n)（属于稳定表层，与 research/04 §7 一致）。research/04 §7 自己对解释器的推荐是：存 UTF-8，用返回 `| none` 的受检 API 做零拷贝字节切片、并强制落在字符边界上，按 `chars` 迭代，把字形簇留在核心之外。权衡：推断负担为零。VM 成本方面，堆对象是 `{ type_id, rc, byte_len, bytes }`（Inko 的最小 16 字节单次分配字符串是布局先例，research/03），切片可以是指向父缓冲区的视图，由 RC 保证父对象存活（零拷贝），也可以在更简单的 RC 记账下退化为复制，这是实现选择，与语义无关。相等与哈希为 O(n)，先用长度快速拒绝。表达力对文本处理足够，字形簇作为额外一层留在标准库（Swift 禁止整数下标的立场是另一种做法，对解释器更糟，research/04 §7）。编译器复杂度最小。排除：O(1) 码点索引（没有哪个正确的实现具备它），代价是放弃免费的驻留（选项 b 的优势）。

### B8.b — 驻留的不可变字符串 + builder

所有字符串在创建时驻留。相等就是指针比较。构造用 builder 类型。切片重新驻留。

```lua
-- B8.b -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
const a = "config"           -- interned on first use
const b = "con" .. "fig"     -- same pointer after interning: a == b is O(1)
const h = header_of(a)       -- dict keys need no hashing after first intern
const pre = s.slice(0, 3)    -- slice re-interns: O(k) hash + lookup, or an unmarked view
```

先例：Lua 驻留的 8 位字符串（属于稳定表层，research/04 的作者口味一节把 Lua 列为已知）。MoonBit 带 builder 的字符串模型（research/04 §14 的 moonbit-core 源码）。权衡：推断负担为零。VM 成本方面，首次驻留之后相等判断和字典查找是 O(1)，但创建要付出一次全局哈希表加同步的查找，切片要么重新驻留（O(k)，而且大量短命切片的代码会让驻留表分配得很糟），要么放弃驻留（于是出现两套相等规则）。内存会被驻留表长期占用（Lua 在处理动态键字符串时的已知行为）。表达力对符号类数据足够，对文本处理别扭。编译器复杂度方面需要驻留表、同步和两套切片规则。排除：便宜的字节级文本处理写法（每个临时字符串都要付出驻留成本），以及在没有分片表的情况下让字符串创建扩展到多核。

### B8.c — UTF-16 或平台字符串

码元即索引空间。码元索引为 O(1)。与 JS 引擎、Windows 和 JVM 原生互通。

```ts
// B8.c -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
const s = "héllo"            // UTF-16: 6 code units; s[1] == '\u00E9' in O(1)
s[5]                          // code-unit index: the surrogate-pair hazard lives here
win32_call(s)                 // wide-char APIs: zero-conversion
JSON.parse(src)               // JS-engine-shaped data flows in directly
```

先例：JavaScript/Java/C#/Windows 的宽字符 API（属于稳定表层）。对 FFI 来说互通论据是真实的（research/06 的模块/FFI 笔记负责那条边界）。权衡：推断负担为零。VM 成本方面，ASCII 为主的文本（配置与日志的常见情形）要占两倍内存，而且每个标量值运算都要带上代理对逻辑，这正是 JS/Java 长期共存的缺陷类别。相等为 O(n)，与 (a) 相同。表达力方面，O(1) 索引只是名义上的，正确的字符索引仍需要成对感知的逻辑，因此这是一个 O(1) 的坑。编译器复杂度最小。排除：便宜的 ASCII 密度，代价是字节字符串的逃生口会变成第二种字符串类型。

### B8.d — (a) 外加面向编辑器/大文本工作负载的 rope

(a) 的 String 加上标准库中的 `Rope`（块树），为大文本提供 O(log n) 的插入、删除和切片。

```ts
// B8.d -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
const doc = Rope.from(text)          // chunk tree over immutable UTF-8 chunks
doc.insert(1_000_000, "x")           // O(log n), no full copy
const line = doc.line(42)            // materializes a String on demand
```

先例：rope 数据结构是编辑器的标准技术（属于稳定表层）。research/04 §7 没有调研它，这个选项就是 (a) 加上一个库级别的数据结构，核心不变。权衡：承担 (a) 的全部成本。VM 成本方面，rope 节点就是普通的堆树，装着块引用，不需要 VM 原语。表达力上增加了编辑器这一细分场景。编译器复杂度为零（只动标准库）。排除：没有。

### B8 对比表

| 轴 | a UTF-8 字节 | b 驻留 | c UTF-16 | d (a) + rope |
|---|---|---|---|---|
| VM 槽位内容 | 一个裸 GC 指针，指向 `{type_id, rc, byte_len, bytes}`（research/03） | 指向驻留表的指针 | 指向 UTF-16 缓冲区的指针 | 指针，同 (a) |
| 相等/哈希 | O(n)，先用长度快速拒绝 | 驻留后 O(1) 指针比较 | O(n) | 每个块操作 O(n) |
| 切片 | 零拷贝视图，边界受检，`| none`（research/04 §7） | 重新驻留（O(k)）或未标记视图 | 以码元为单位的视图 | O(log n) 的块视图 |
| 内存 | 1 倍，ASCII 密集 | 1 倍 + 驻留表占用 | ASCII 为主时要 2 倍 | 1 倍 + 树开销 |
| 推断负担 | 无 | 无 | 无 | 无 |
| 编译器复杂度 | 最小 | 驻留表 + 同步 + 两套规则 | 最小 | 只动标准库 |
| 排除 | O(1) 字符索引（正确的做法） | 临时字符串密集的工作负载 | ASCII 密度、字节逃生口 | 没有 |

### B8 与其他设计轴的交互

B5 错误：编码与之无关。`slice_char` 返回 `String | none`，复用 B4 的并集纪律，取代 panic（这是 research/04 §7 的明确设计）。线性性（docs/zh/design/02）：指向父缓冲区的字符串视图是非拥有借用，在仿射规则下，切片不能比父对象活得更久，除非父对象的 RC 保证它存活，这正是 research/05 §5.2 第 4 条断点规定的借用操作数形状。驻留 (b) 回避这个问题的方式只是把所有东西都留在表里存活。

**推荐（B8）：(a)**。不可变 UTF-8 加字节长度头，零拷贝的受检切片类型为 `String | none`，按 `chars` 迭代，字形簇放在标准库，这就是 research/04 §7 的推荐原话。它保住了未装箱槽位的 VM 叙事（字符串就是堆引用），相等判断的成本按 O(n) 计入，当编辑器类工作负载出现时，(d) 的 rope 可以作为库附加。驻留 (b) 和 UTF-16 (c) 都为了狭窄场景而对常见情形加税，而 FFI 边界（research/06，docs/zh/design 中模块/FFI 的目录）在边缘处做转换。

## B9 — 相等、排序、哈希

本点决定的内容：`==` 的含义，它是否具有全性，以及哪些部分继续由 trait 或 derive 中介。derive 机制本身（Hash/Show/serde 属性、合成规则）归 docs/zh/design/05-generics-metaprogramming.md 负责，本文件按要求只做指向。research/05 §5.2 已经给出任何结构化选择都必须满足的全性条件。这些条件已内建进下面的选项 (a)。

### B9.a — 通用结构化 `==`（Roc 模型，带五条例外）

`==` 对所有数据都是结构化的，采用借用，并且按构造即为全。函数比较在静态上被拒绝。浮点按位结构比较，旁边另有一个具名的 IEEE 比较。可变单元和引用单元以及 `~Copyable` 资源按同一性比较。哈希与相等必须同时提供。

```ts
// B9.a -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
p == q                  // structural, borrows both operands (Break 4: comparison never consumes)
f == g                  // compile error: functions carry no decidable equality (Break 1)
nan == nan              // true: bit-structural; reflexivity holds for dict keys (Break 2, choice i)
ieee_eq(nan, nan)       // false: the named IEEE comparison, Roc's is_float_eq split (research/05 §5.2)
cell == cell2           // identity: mutable/reference cells compare by address (Break 3)
file == file2           // identity for ~Copyable resources (Break 4; docs/en/design/02 owns the linear layer)
```

先例：Roc 提供结构化 `==`，无需 derive，把 `is_eq` / `is_float_eq` / `is_approx_eq` 分开，并在标准库签名中让 `is_eq` 与 `to_hash` 绑定（research/05 §5.1-5.2）。OCaml 的通用多态比较是同一思路，成本有记录：反射式遍历慢，遇到环会发散，而「单元按同一性比较」这条例外正好避开后者（research/05 §5.1 的 OCaml 一行，§5.2 第 3 条断点）。Elm 在静态上拒绝函数比较（research/05 §5.2 第 1 条断点）。权衡：推断负担为零。VM 成本方面，基本类型槽位一条操作码即可比较，聚合体做深比较，走按 type_id 的内联缓存快路径（research/05 §6.3），而且不可变性意味着对驻留值或归一值而言比较结果可以缓存。表达力方面只损失函数相等（通常本身就是缺陷，research/05 §5.2），以及把 IEEE 的 `==` 当作默认写法（仍可按名字调用）。编译器复杂度方面，检查器需要「函数类型没有 Eq 能力」和「单元按同一性比较」两条规则，都很简单。一致性问题不会出现，因为没有可供冲突覆写的按类型实现。排除：为 `==` 定义按类型的自定义值语义（例如一个 Money 类型，其 `==` 忽略货币，这种需求必须改用 `equiv` 式的具名方法），以及在字典中让 IEEE 浮点键按 IEEE 语义工作（它们按位语义工作，`is_float_eq` 依然可用）。

### B9.b — trait 中介的相等（Rust `PartialEq` 模型）

`==` 只在存在 `Eq` 实现的地方可用。每次比较都由用户或 derive 定义。

```rust
// B9.b -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
impl Eq for Point {                     // or #derive(Eq): nothing free, everything explicit
  fn equal(a: &Point, b: &Point) -> Bool { a.x == b.x && a.y == b.y }
}
// MoonBit evidence: builtin/tuple_eq.mbt hand-writes Eq for (T0,T1), then (T0,T1,T2), ... per arity
```

先例：Rust 的 `PartialEq`/`Eq`（research/05 §5.1）。MoonBit 的 `Eq` trait，按元数在 `builtin/tuple_eq.mbt` 中手写元组实现，这就是被引用的样板代码证据，说明即使在一门 derive 丰富的语言里，trait 中介的相等也会产生按形状的工作量（research/05 §5.1）。Rust 为此付出的一致性摩擦记录在 research/05 §3.2。权衡：推断负担方面，检查器需要判定适用哪个 Eq，按 research/05 §2.2，实例头不相交时这很平凡。VM 成本在判定之后与 (a) 相同（derive 出来的函数体就是同一套结构化比较）。表达力方面，可以按类型最大程度地控制，包括按类型的 IEEE 语义（这是 MoonBit 的选择，其有记录的 NaN 前置条件会泄漏进每个包含浮点的 derive 类型，research/05 §5.1-5.2）。编译器复杂度方面需要 trait 消解，以及章程回避的孤儿规则与一致性难题。排除：比较那些没人写过 Eq 的类型（泛型代码必须到处携带 `T: Eq` 约束和证据词），以及免样板代码这一性质。

### B9.c — 内建结构化 `==` + 基于 derive 的排序/哈希（MoonBit 模型）

`==` 对所有数据类型无条件由编译器生成结构化实现（即 Kotlin data class 的语义，去掉 data class 的仪式）。`Ord`、`Hash`、`Show` 是需要显式写出的 derive。

```moonbit
// B9.c -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
#derive(Hash, Show, Ord)
struct Point { x: Int, y: Int }   // == is builtin structural; Ord/Hash/Show come from the derive
p1 == p2                          // structural, always available
p1 < p2                           // uses derived Ord (field order = lexicographic)
```

先例：MoonBit 为 Eq/Hash/Show/ToJson 提供 derive，IEEE 浮点注意事项记录在 `builtin/traits.mbt` 中（research/05 §5.1）。Kotlin 的 data class 无条件根据构造参数生成 equals/hashCode/toString（research/05 §5.1）。Swift 的 SE-0185 合成（research/05 §5.1）。权衡：除浮点之外与 (a) 相同。若内建 `==` 跟随 IEEE（这是 MoonBit 有记录的选择），自反性会破，前置条件会泄漏。若跟随位结构，这个选项就塌缩为 (a) 加 derive 的外表。排序无法结构化（集合上的全序本来就是任意的，research/05 §5.3），所以 Ord 保持 derive 中介在每个选项里都正确。排除：与 (a) 相同的按类型 `==` 覆写。

### B9.d — 只用引用同一性

`==` 比较引用。结构化比较沿用 `equals` 方法的传统。

```java
// B9.d -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
a == b            // identity
a.equals(b)       // structural if someone remembered to override; default is identity (the Java trap)
```

先例：Go 与 Java（research/05 §1.9 与 §5.1）。Go 存在不可比较类型在运行期 panic 的漏洞，对接口值使用 `==` 时若动态类型不可比较就会 panic。这是警示性的证据，说明把同一性放在首位只会把失败推得更晚（research/05 §1.9 的原话：「相等上的静态全性比 Go 省下的那点语法更有价值」）。权衡：在默认不可变加值语义之下，同一性几乎没有意义，两个相等的不可变记录可以互换，这正是 Roc 的理由（research/04 §9）。同一性只在线性资源和记忆化中保持意义，两者都由 (a) 的单元/资源例外覆盖。推断负担为零。VM 成本方面，指针比较便宜，但每次结构化需求都退化成方法调用。表达力最差。编译器复杂度最小。排除：把值语义编程作为默认惯用法。这个选项与章程自己的不可变立场相抵触，列出来只为完整。

### B9 对比表

| 轴 | a 通用结构化 | b trait 中介 | c 内建结构化 + derive | d 只用同一性 |
|---|---|---|---|---|
| 全性 | 按构造即全：research/05 §5.2 的五条例外 | 默认为偏：缺少 Eq 就是编译错误（编译期可查，代价是样板） | 在数据上为全。浮点语义沿袭 MoonBit 的决定（IEEE + 前置条件） | 有漏洞：Go 式的运行期 panic，遇到不可比较的值就崩（research/05 §1.9） |
| 推断负担 | 无 | Eq 判定。`T: Eq` 约束蔓延到所有泛型签名 | 无 | 无 |
| VM 表示 | 槽位基本类型 + 内联缓存深比较。只用借用（第 4 条断点） | 判定后函数体相同。泛型代码里有证据词 | 同 (a) | 指针比较。深比较是方法调用 |
| 表达力 | 失去按类型的 `==` 覆写，IEEE 语义可按名字调用 | 按类型完全可控。NaN 前置条件到处都有记录（research/05 §5.1） | 同 (a)，浮点选择除外 | 放弃值语义 |
| 样板代码 | 无（MoonBit 按元数写的元组实现消失，research/05 §5.3） | 按类型实现，按元数写元组（moonbit-core 的 `tuple_eq.mbt`） | `==` 无样板，Ord/Hash/Show 靠 derive | 无 |
| 排除 | 按类型的自定义 `==` 语义 | 免样板代码的相等 | 按类型的 `==` 覆写 | 不可变值语义惯用法 |

### B9 与其他设计轴的交互

B5 错误：错误载荷在测试中做结构化比较（`e == Timeout`），不需要任何 Eq 实现的仪式。(b) 会让连测试断言都携带 trait 约束。线性性（docs/zh/design/02）：每个选项都必须采用借用，从而让比较永不消耗仿射值（research/05 §5.2 第 4 条断点），资源按同一性比较。这就是要求本文件从 research/01 带上来的 `~Copyable` 交互点：可复制性可以表达为一个取反的集合约束（research/01 的 Hylo 章节与横切注记 1）。因此 `==` 对 `~Copyable` 值的同一性例外也落在同一套集合代数之内，无需额外的临时规则。

**推荐（B9）：(a) 通用结构化 `==`，带 research/05 §5.2 的五条例外**（函数在静态上被拒绝，浮点按位结构比较并另具名 `is_float_eq`，单元与 `~Copyable` 资源按同一性，操作数只用借用，哈希必须同时提供），外加 derive 中介的 `Ord`/`Hash`/`Show`。它删掉了 MoonBit 按元数写的元组样板（research/05 §5.3），保住了自反性（字典与去重所依赖的关键性质，research/05 §5.2 第 2 条断点）。derive 机制本身由 docs/zh/design/05-generics-metaprogramming.md 决定。

## 横切交互汇总

B5（docs/zh/design/03-effects-errors-async.md，以并集承载错误为推荐基础）预设 B1 取 {a, c, d} 之一。带差集类型的 `.err` 处理器（`(T | E) - E`，research/04 §11）和扩容不破坏的错误集合（research/04 §3）只在并集是类型语言一等公民的地方存在。B1(b) 强制使用 Result 包装，应当记为一种降级备选，它会降低 docs/zh/design/03 推荐表层的等级，两者处于同等地位的说法不成立。docs/zh/design/02（线性性，正在并行撰写，仅按路径引用，此处不假设其内容）：`~Copyable` 的取反约束惯用法按构造就是集合论的（research/01），所以它能与 B1 的 {a, c, d} 直接组合，与 (b) 则通过 enum 建模的资源状态组合。B2(a+c) 的受检边界是仿射流分析偏好的底料。B9(a) 的只用借用 `==` 和资源按同一性这条例外，是 docs/zh/design/02 可以依赖的相等侧义务。

## 一致的组合

三个命名的端到端类型系统，都在同一个程序上演示：一个配置解析器，读取类似 JSON 的输入，校验端口，并处理错误，覆盖积类型、和类型、并集与收窄、错误集合以及相等。所有代码都是示例性的，表层语法由 docs/zh/design/04-syntax.md 决定。

### 组合 1 —「Set-core」：MLstruct 风格的并集 + 流敏感精化 + 通用 ==

B1(a) + B2(取 c，叠加在双向骨架上) + B3(a) + B4(a) + B7(a) + B8(a) + B9(a)。

```ts
// Combo 1 "Set-core" -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
type Json = null | Bool | Int | Float | String | Json[] | { [String]: Json }

type CfgErr = IoErr | MissingKey(String) | BadPort(Int)     // error set: a plain union
type Port    = Int & { }                                    // (no-op here; see intersection use below)

const get = (j: Json, k: String) -> Json | MissingKey {
  match j {
    m: { [String]: Json } => m[k] or none or return MissingKey(k),  // missing key: none member
    _                     => return MissingKey(k),
  }
}

const load = (path: String) -> Config | CfgErr {
  const text = read_file(path)?                 // IoErr propagates; return union contains it
  const raw  = get(parse_json(text)?, "port")   // Json parse errors propagate likewise
  if raw is Int && raw >= 1 && raw <= 65535 {   // flow refinement: raw: Int in the branch
    Config { host: "localhost", port: raw, retries: 3 }
  } else {
    return BadPort(raw is Int ? raw : -1)
  }
}

const cfg = load("app.cfg").err((e) => {        // handler covers the whole error set:
  match e {                                     // result type is Config - CfgErr == Config
    io: IoErr             => log("io: ${io}"),
    MissingKey(k)         => log("missing: ${k}"),
    BadPort(p)            => log("bad port: ${p}"),
  }
  default_config()
})

const total = [1, 2, 3].map((x) => x * 2).reduce(0, (a, b) => a + b)
const same = cfg == default_config()            // universal structural ==, borrows
```

结论：表达力最大，也最贴合章程的集合论口味与 README 的集合指令。错误处理完全不需要包装，match 靠集合算术就能诊断（research/04 §3-4）。成本集中处正是研究指出的地方：检查器（语义子类型、空集）和推断赌注（MLscript 级别的主类型没有生产编译器，research/05 §2.3）。匿名结构化记录 (B3.a) 还要在 VM 的内联缓存机械里支付形状归一。影响：docs/zh/design/02 免费获得 `~Copyable` 组合能力（取反约束是原生的集合项，research/01）。docs/zh/design/03 可以完全按研究结论采纳并集承载错误和带差集类型的 `.err`（research/04 §11）。docs/zh/design/04 必须在类型位置和表达式位置保留 `|`、`&`、`-`、`is`、`or`，并砍掉 Kotlin 的第 4 到第 8 项和第 27 项（research/04 §2.5）。docs/zh/design/05 的不相交检查器对重载变得关键（research/05 §2.2）。

### 组合 2 —「Nominal-core」：Rust/Gleam 风格 + Result/Option

B1(b) + B2(a) + B3(b) + B4 用 Option/Result 包装 + B7(a) + B8(a) + B9(b)。

```rust
// Combo 2 "Nominal-core" -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
enum Json { Null, Bool(Bool), Num(Float), Str(String), Arr(List[Json]), Obj(Map[String, Json]) }

enum CfgErr { Io(IoErr), MissingKey(String), BadPort(Int) }

struct Config { host: String, port: Int, retries: Int }

fn get_port(j: &Json) -> Result[Int, CfgErr] {
  match j {
    Json::Obj(m) => match m.get("port") {
      Some(Json::Num(f)) if f >= 1.0 && f <= 65535.0 => Ok(f as Int),
      Some(_) => Err(CfgErr::BadPort(-1)),
      None => Err(CfgErr::MissingKey("port".into())),
    },
    _ => Err(CfgErr::MissingKey("port".into())),
  }
}

fn load(path: String) -> Result[Config, CfgErr] {
  let text = read_file(path)?;                  // Err propagates through the wrapper
  let port = get_port(&parse_json(text)?)?;
  Ok(Config { host: "localhost", port, retries: 3 })
}

fn main() -> Result[(), IoErr] {
  let cfg = load("app.cfg").unwrap_or_else(|e| { log("${e}"); default_config() });
  let total = [1, 2, 3].map(|x| x * 2).reduce(0, |a, b| a + b);
  let same = cfg == default_config();           // requires derived PartialEq on Config
  Ok(())
}
```

结论：本文件里最容易写对的编译器，子类型、集合代数与流环境全部省去。对 enum 分支的穷尽性检查是理解得最透彻的检查器，而且它直接把作者的 Rust 熟练度搬过来。成本是 research/04 §12 Style D 记录的补偿阶梯：到处是 `Some`/`Ok`/`Err` 包装，出现 `.ok_or(...)?` 这类形状，用 `unwrap_or_else` 取代 `or`。此外它失去错误集合算术：新增一个 `CfgErr` 分支会破坏针对它的每一个 match，`(T | E1 | E2) - E1` 无法表达。影响：docs/zh/design/02 必须把资源状态建模为 enum（`File[Open]` 变成 `enum FileState`），代价是失去幻影状态并集（research/05 §7.3），但一切仍可表达。docs/zh/design/03 必须以 `Result[T, E]` 加传播作为 B5 的基础，其并集承载的推荐在此不可用（带差集类型的处理器由本选项排除）。docs/zh/design/04 保住了预算，花销在与包装相邻的写法上。docs/zh/design/05 的 trait/实例叙事原样可用，其 derive 机械成为相等判断的必需项。

### 组合 3 —「Hybrid」：名义标签 + 集合算术 + 收窄（推荐）

B1(c) + B2(a + c) + B3(d，含匿名记录语法糖) + B4(c) + B7(a/d) + B8(a) + B9(a)。

```ts
// Combo 3 "Hybrid" -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
tag IoErr; tag Missing; struct BadPort { got: Int }         // nominal atoms
type CfgErr = IoErr | Missing | BadPort                     // set instruction over tags

struct JsonObj { fields: Map[String, Json] }
struct Config  { host: String, port: Int, retries: Int }

const get_port = (j: Json) -> Int | Missing | BadPort {
  const obj  = j is JsonObj or return Missing { key: "port" }
  const pv   = obj.fields.get("port") or return Missing { key: "port" }   // T | none union
  const p    = pv is Int or return BadPort { got: -1 }                    // narrows pv: Int
  if p >= 1 && p <= 65535 { p } else { return BadPort { got: p } }
}

const load = (path: String) -> Config | CfgErr {
  const text = read_file(path)?                              // CfgErr member propagates
  const port = get_port(parse_json(text)?)?
  Config { host: "localhost", port, retries: 3 }
}

const cfg = load("app.cfg").err((e) => {                     // exhaustive over the tag union:
  match e {                                                  // result: Config - CfgErr == Config
    b: BadPort => log("bad port ${b.got}"),
    _          => log("config read failed"),
  }
  default_config()
})

const kv   = ("host", 8080)                                  // anonymous tuple (B3.d)
const same = cfg == default_config()                         // universal structural == (B9.a)
```

结论：保留 README 的集合指令表层（`type CfgErr = IoErr | Missing | BadPort` 就是字面上的 `A = B | C`），把穷尽性建立在标签身份上，让检查器停留在 enum 覆盖的难度。每个槽位都保持 research/03 的未装箱标签加载荷成本，单一机制提取叙事也保住了（`?`、`or`、`is` 在 `T | none` 和 `T | CfgErr` 上行为一致）。相对组合 1，它放弃的只有完全匿名的结构化代数，而 B3.d 的匿名记录语法糖找回了其中的常见情形。影响：docs/zh/design/02 可以在同一套标签代数上表达 `~Copyable` 约束和幻影状态并集（`File[Open] | File[Closed]`，research/05 §7.3）。docs/zh/design/03 的并集承载错误与带差集类型的 `.err` 以标签差集语义工作。docs/zh/design/04 掌管这些记号（`|`、`&`、`-`、`is`、`or`、`?`、`T?`），而 research/04 §11 的表层正是为这套语义设计的。docs/zh/design/05 只在重载/实例组上需要不相交检查器，而标签身份这条限制让它比完全结构化版本严格更容易（research/05 §2.2）。

## 出处

研究笔记（以上所有引用的主要证据）：docs/zh/research/04-syntax-ergonomics.md §2（语法糖上限）、§3（集合论/流类型：Phi https://www.arshad.fyi/writings/null-errors-and-enums-are-one-feature、MLscript https://github.com/hkust-taco/mlscript、MLstruct OOPSLA 2022 `https://lptk.github.io/files/[v6.2]%20mlstruct.pdf`、Whiley https://whiley.org/pdfs/WhileyLanguageSpec.pdf）、§4（模式匹配/穷尽性）、§6（标注纪律）、§7（字符串）、§8（数值）、§9（相等）、§11（错误表层）、§12（Style A 到 D）、§14（来源）。docs/zh/research/05-generics-metaprogramming.md §2.1-2.3（集合论子类型、重载即箭头交集、消解即交集子类型 OOPSLA 2020、不相交合并 OOPSLA 2025）、§3.2（一致性成本）、§5.1（derive 调研，含 moonbit-core 的 `builtin/tuple_eq.mbt` 与 `builtin/traits.mbt` 证据、Roc 的 `is_eq`/`is_float_eq`/`Dec`、Swift SE-0185、OCaml 通用比较）、§5.2（五条全性断点）、§5.3（结论）、§6.3（证据词与内联缓存）、§7.1-7.3（Hylo 的 `~Copyable` 泛型、型变、幻影状态）。docs/zh/research/03-runtime-ir-jit.md：值表示（无 NaN-boxing、未装箱槽位）、未装箱 ADT/标签加载荷布局、`{u32 type_id; u32 rc}` 头、溢出即 panic 的推荐、解释器与 C 的差距分解。docs/zh/research/01-linearity-ownership.md：Hylo/Swift 的 `~Copyable` 作为取反的默认约束（SE-0390 的引用）、横切注记 1（可复制性作为集合论约束）、`Result<~Copyable>` 载荷注意事项。README.md：历史集合指令（`A = B | C`、`B & C`、`B - C`、`(B & C) - D`、`any`），在 B1.d 内被评估，并未无条件保留。
