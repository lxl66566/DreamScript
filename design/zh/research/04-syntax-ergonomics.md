# 04 — 语法表层、语法糖预算与工效学

日期：2026-11-04。范围：只谈语法表层、语法糖预算与工效学（不含类型系统理论、并发、运行期）。资料来源：尽可能使用语言文档与真实标准库源码（本地克隆读取 koka、moonbit-core 与 inko，并抓取 Roc 新编译器教程、Gleam tour、Kotlin 2.1/2.2 发布说明与 Phi 文章），再加上对稳定语言的既有了解。证据按 URL 就地标注。对比矩阵统计语法表层的数量，质量另论。

## 1. 概要

Kotlin 的语法表层就是硬性天花板：大约 38 个不同的语法特性，按下面定义的 S1/S2/S3 刻度加权约 76 单位。TypeScript 与 Swift 紧随其后（各约 28 个特性）。Rust 落在 26 左右。Koka、MoonBit、Roc、Gleam 说明一门现代语言只要有 9 到 16 个特性就能用得舒服。
推荐的 DreamScript 预算是不超过 20 个特性（加权约 32），把省下的额度投到集合论类型上：data class、sealed 层级、智能转换、安全调用全都塌缩成一个机制。
集合论流类型（TS 的类型收窄、Whiley、MLscript、Phi）是语法表层上杠杆最大的一项决策：并集类型、`is` 收窄，加一个 `or` 提取运算符，让 `null`、错误与枚举字面上成为同一个特性，替换掉 Kotlin 的 `?.`/`?:`/`!!`、Swift 的四种 `try` 拼法，以及针对每个单子的组合子。
Koka 风格的完整 UFCS 已在 tour 源码中核实：`s.encode(3)` 只是 `encode(s,3)` 的语法糖，作用于任意第一参数，并且顺带涵盖了链式调用所需的管道。如果确实想要管道，Gleam 的第一参数 `|>` 是与 UFCS 一致的变种（已核实：`a |> b(1,2)` 去糖为 `b(a,1,2)`）。
模式匹配：采用 Rust 形状的分支（`pat if guard`、`|` 或模式、`@` 绑定、穷尽性）。Kotlin 2.1 稳定下来的守卫语法是另一种拼法。Gleam 省略守卫是反面教材，代价是嵌套膨胀。
默认参数是唯一与作者单子哲学直接冲突的 Kotlin 特性：typst 风格的即时求值默认值会造成乱序求值（typst#3052）。Koka 的惰性命名参数默认值是唯一安全的通用形式。只写字面量的默认值是最便宜的安全子集。
旧的 DreamScript 块错误想法（`{1/0}` 由上下文定型，`.err((e)->...)`）目标完全正确（Option/Result/效应共用一个提取机制），但机制不具组合性：同一个块在不同使用点定型结果不同。修法是保留语法表层、修正语义：后缀 `?` 加 `or` 加带集合差集类型的 `.err` 处理器，详见第 11 节。
字符串：Kotlin 风格的 `$x` 与 `${expr}` 是单一约定里最好的（裸标识符加一种花括号形式）。原始字符串应当用一个构造同时覆盖多行与 heredoc 需求。
数值：默认 `Int`/`Float`（i64/f64）加后缀，不做隐式收窄，采用 Swift 风格的溢出即陷入（trapping）加显式 `+%` 回绕运算。这是解释器下唯一可预测的溢出策略。
相等性：不可变性之下只有一个结构化 `==`（Roc 风格）。`===` 与同一性比较属于删除候选。
花括号胜过缩进：缩进语言里只有 Koka 站得住，Python/Nim 的抱怨是结构性的。无分号、以换行结束语句的语法是成本最低的规则。

## 2. 语法糖天花板（交付物 a）

### 2.1 计入范围

语法糖特性指任何能被解析器去糖成一个更小的核心语言、且不改变表达力的语法。表中所有语言共享的基线语义（变量、调用、块、泛型）不计入。类型层 DSL 计入，因为它们消耗作者注意力，与语法完全一样。成本刻度：S1 = 一个 token 或运算符，彼此正交，一句话就能解释。S2 = 一个新构造或一条代码生成规则。S3 = 程序员必须在脑子里记住的新定型规则或认知语义。

### 2.2 Kotlin — 完整清单（天花板）

| # | 特性 | 例子 | 成本 |
|---|---------|---------|------|
| 1 | 无分号语句 | newline ends statement | S1 |
| 2 | 字符串模板 | `"n=$n max=${a+b}"` (2.2 adds `$$` multi-dollar) | S1 |
| 3 | 原始字符串 | `"""\path\no\escapes"""` | S1 |
| 4 | 安全调用 | `list?.first()?.id` | S1 |
| 5 | Elvis | `val x = y ?: default` | S1 |
| 6 | 非空断言 | `y!!` | S1 |
| 7 | 可空类型后缀 | `String?` | S2 |
| 8 | 智能转换 | `if (x is String) x.length` | S2 |
| 9 | if/when 表达式、when 主语、2.1 守卫 | `when (v) { is Int -> ...; else -> ... }` | S2 |
| 10 | 区间 | `1..10`, `0 until n`, `10 downTo 1 step 2` | S2 |
| 11 | `in` 运算符（包含、for 区间） | `for (c in 'a'..'z')` | S1 |
| 12 | 运算符约定（约 15 个：plus、get、invoke、iterator、rangeTo、compareTo 等） | `a + b` desugars to `a.plus(b)` | S2 |
| 13 | 中缀函数 | `x shr 2`, `a to b` | S1 |
| 14 | 扩展函数 | `fun String.shout() = uppercase() + "!"` | S2 |
| 15 | 扩展属性 | `val String.lastChar: Char` | S2 |
| 16 | 带访问器的属性 + `field` | `var x: Int get() = ... set(v) {...}` | S2 |
| 17 | 属性委托 | `val db by lazy { connect() }` | S2 |
| 18 | 类委托 | `class A(b: B) : Iface by b` | S2 |
| 19 | data class 合成 | `data class P(val x: Int)` gives equals/hashCode/toString/copy/componentN | S2 |
| 20 | 解构声明（含 lambda 与 for 中） | `val (a, b) = pair`, `for ((k, v) in map)` | S2 |
| 21 | 默认参数 | `fun f(x: Int = 0)` | S2 |
| 22 | 命名参数 | `f(x = 1)` | S1 |
| 23 | 尾随 lambda | `list.map { it * 2 }` | S1 |
| 24 | lambda 简写 `it` | `list.filter { it > 0 }` | S1 |
| 25 | 接收者 lambda（DSL 作用域） | `buildList { add(1) }` | S2 |
| 26 | 类型别名 | `typealias Row = Map<String,String>` | S1 |
| 27 | sealed 层级 + 穷尽 when | `sealed interface Shape` | S2 |
| 28 | 对象表达式/声明、伴生对象 | `object : Clickable {...}`, `companion object` | S2 |
| 29 | 主构造函数 | `class P(val name: String)` | S2 |
| 30 | 值类/内联类 | `@JvmInline value class Meters(val v: Int)` | S2 |
| 31 | 变型语法 | `List<out T>`, `Consumer<in T>` | S2 |
| 32 | 具体化的类型参数 | `inline fun <reified T> parse(...)` | S3 |
| 33 | 变长参数 + 展开 | `fun f(vararg xs: Int)`, `f(*arr)` | S1 |
| 34 | 标签 | `return@map`, `loop@ for {... break@loop}` | S2 |
| 35 | 反引号标识符 | `` val `data class` = 1 `` | S1 |
| 36 | 下划线占位符 | `val (_, second) = pair` | S1 |
| 37 | `tailrec` 修饰符 | `tailrec fun go(...)` | S1 |
| 38 | 上下文参数（2.2 预览） | `context(Db) fun q() = ...` | S3 |

合计：38 个特性，加权 76（24 个 S1 按 1 计，12 个 S2 按 2 计 = 48，再加 2 个 S3 按 4 计 = 76，前提是 S3 双倍计。若用 S1=1/S2=2/S3=3 则为 74）。任何计分约定下，Kotlin 的特性数都接近最精简现代语言的两倍。

### 2.3 各语言的清单计数

下面的计数是特性数。“约”字表示数量级，关注点同样在数量级。

| 语言 | 特性数（约） | 加权成本 | 说明 |
|----------|------------------:|--------------:|-------|
| Kotlin | 38 | 74-76 | 天花板。2.1/2.2 还在新增（守卫、非局部 break、multi-dollar、上下文参数） |
| Swift | 29 | 60 | 4 种错误拼法（try/try?/try!/catch）、属性包装器、result builder 推高了权重 |
| TypeScript | 28 | 58 | 大头是类型层 DSL（条件类型、映射类型、模板字面量类型、重载、声明合并） |
| Rust | 27 | 45 | `?`、let-else、let-chains（1.88）、匹配工效学、derive、属性、生命周期省略（S3） |
| OCaml | 14 | 28 | 守卫、as 绑定、或模式、带标签/可选参数、自定义运算符、效应处理器 |
| MoonBit | 15 | 27 | `T?`、`raise?` 行、`~` 标签、`::` 方法、derives。已按 moonbit-core 标准库源码核实 |
| Koka | 15 | 26 | UFCS、泛化尾随 lambda、`with`、惰性命名默认值、复制更新 `p(age = ...)` |
| Zig | 13 | 24 | orelse/catch/try、defer、comptime、匿名字面量、带标签的块 |
| Roc（新编译器） | 11 | 19 | 已按 docs/mini-tutorial-new-compiler.md 核实。`$var` 可变性、后缀 `?`、`=>` 匹配分支 |
| Gleam | 9 | 15 | 第一参数管道、`use`、带标签实参。没有守卫，没有提前返回 |
| Go | 9 | 14 | `:=`、defer、复合字面量、iota。刻意作出的近零基线 |

### 2.4 特性 × 语言矩阵

图例：y = 有，p = 部分有或有另一种拼法的等价物，n = 无。“仅格式函数”表示占位符写在格式函数的实参里，不在字符串字面量里。

| 特性 | Kotlin | Rust | TS | Swift | Go | Zig | Koka | MoonBit | Roc(new) | Gleam | OCaml |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 字符串字面量内插值 | y `$x` | n（仅格式函数） | y `${e}` | y `\(e)` | n | n（仅格式函数） | n | y `\{e}` | y `${e}` | n | n |
| 原始/多行字面量 | y `"""` | y `r#"..."#` | p（模板） | y `#"""#` | y 反引号 | y `\` lines | n | n | n | n | n |
| 可选链 `?.` | y | n | y | y | n | n | n | n | n | n | n |
| 空值合并/Elvis | y `?:` | n | y `??` | y `??` | n | y `orelse` | n | n | n | n | n |
| 后缀错误传播 | p（`?: return`） | y `?` | n | p（`try` 关键字） | n | y `try` | n（处理器） | y `try?`/`try!` | y `?` | p（`use`） | n |
| 签名里的并集类型 | n | n | y | n | n | p（错误集合） | p（效应行） | p（`raise E`） | n | n | p（多态变体） |
| 交集类型 | n | n | y | n | n | n | n | n | n | n | n |
| 差集/取反类型 | n | n | p（`Exclude`） | n | n | n | n | n | n | n | n |
| 流收窄（智能转换） | y | p（仅匹配） | y | p（switch） | n | n | n | n | n | n | n |
| 穷尽匹配 | y | y | p（`never`） | y | n | y | y | y | y | y | y |
| 匹配守卫 | y (2.1) | y `if` | n | y `where` | n | n | n | y | n | n | y `when` |
| 单分支或模式 | p（`is A, is B`） | y `\|` | n | y | n | n | n | y | n | y | y |
| as/@ 绑定 | n | y `@` | n | p（`case let`） | n | n | n | n | n | n | y `as` |
| 解构绑定 | y | y | y | y | p（多重赋值） | p（捕获） | p（匹配） | y | y | y | y |
| 默认参数 | y | n | y | y | n | n | y（惰性） | y（可选） | n | n | y（`?lab`） |
| 命名参数 | y | n | n | y | n | n | y | y（`~`） | n | y（带标签） | y（`~`） |
| 尾随 lambda | y | n | n | y | n | n | y | n | n | n | n |
| 自由函数上的 UFCS | n | p（trait 方法） | n | n | n | n | y | p（`T::f` 作 `x.f()`） | n | n | n |
| 管道运算符 | n | n | n | n | n | n | n | n | n（新编译器已移除） | y 第一参数 | y 最后参数 |
| 运算符重载 | y（约定） | y（trait） | n | y | n | n | p（运算符即函数） | y（`op_add`） | n | n | y |
| 自定义中缀运算符 | y | n | n | y | n | n | y（fixity） | n | n | n | y |
| 记录/derive 合成 | y（data） | y（derive） | n | y（synth） | n | n | n | y | n | n | n |
| 类型别名 | y | y | y | y | y | y | y | y | p | y | y |
| 和类型（枚举） | p（sealed） | y | p（判别式） | y | n | y（带标签） | y | y | y（`:=`） | y | y |
| 扩展方法 | y | p（trait） | n | y | n | n | n | y（`T::f`） | n | n | n |
| 委托/属性委托 | y `by` | n | n | p（包装器） | n | n | n | n | n | n | n |
| 访问器属性 | y | n | y get/set | y | n | n | n | n | n | n | n |
| 值类/内联类 | y | n（普通结构体） | n | n | n | n | n | n | n | n | n |
| 区间作为值 | y | y | n | y | n | y | n | y `..<` | p | n | n |
| if/match 作为表达式 | y | y | p（三元） | p（三元） | n | y | y | y | y | p（仅 case） | y |
| 带标签的 break/continue | y | y | y | y | y | y | n | y | n | n | n |
| 泛型变型语法 | y in/out | y +/- | n（推断） | y in/out | p（接口） | n | n | n | n | n | n |
| 匿名结构体字面量 | n | p（struct expr） | y（对象） | p | y（复合） | y `.{}` | n | y（记录） | y（记录） | y（记录） | y（记录） |
| 绑定上的可变性标记 | y val/var | y mut | y let/const | y let/var | y var | y var/const | y val/var `:=` | y let mut | y `$var` | n（不可变） | p（ref/mut 字段） |
| y 计数（约） | 28 | 19 | 17 | 22 | 5 | 12 | 10 | 17 | 9 | 8 | 13 |

读矩阵：作者喜欢的语言（TS、Rust、Koka、MoonBit）聚在 17-22 个 y，Kotlin 停在 28。因此要施加的天花板数字是 Kotlin 的 28 个 y，而舒服的目标区间是 16-20。

### 2.5 预算结论

天花板是上限。集合论类型加流收窄把 Kotlin 的特性 4、5、6、7、8、19、27（安全调用、Elvis、非空断言、可空后缀、智能转换、data class、sealed when）塌缩成大约两个原语（`is` 收窄与 `or` 提取，见第 3 节），所以 DreamScript 能远远待在天花板之下，同时保持 Kotlin 日常手感。
推荐的保留集（约 18 个特性）：字符串内插值、原始字符串、后缀 `?`、`or` 提取、`is` 收窄、带守卫与或模式的穷尽匹配、解构、默认参数加命名参数（受限）、尾随 lambda、UFCS、类型别名、和类型、derives、区间、val/var 之分、if/match 表达式、匿名记录字面量、带标签的循环。2.2 节里其余的一切都是删除候选。

## 3. 集合论与流类型的语法表层

TypeScript 用 `typeof`、`instanceof`、`in` 运算符、判别式字面量检查、用户谓词（`x is T`）、断言函数（`asserts x is T`）做收窄，并通过赋值给 `never` 检查穷尽性。它没有取反类型语法，但用条件类型（`Exclude<T, U>`）算出差集。收窄在 `const` 局部变量与只读结构上成立，并会被闭包、可变操作与 `await` 重置。默认不可变的新语言正能直接切掉这些坑。
MLscript/MLstruct（Parreaux、Castagna 一脉）把并集 `|`、交集 `&`、取反 `~` 做成一等公民，并具备主类型推断。它透明地处理类：一个类就是它的实例集合，因此 `A & B` 是真实类型，类层级变成交集（https://github.com/hkust-taco/mlscript，MLstruct OOPSLA 22 论文）。这是已发表的最强证据，说明作者的集合代数野心（README 里的 `type A = (B & C) - D`）落在可推断的范围内。
Whiley 在 JVM 上以结构化类型提供流类型：`type Tree is null | { int data, Tree left, Tree right }`，通过 `if x is T` 与空值测试收窄，语言规范里记录了并集/交集/取反运算符（https://whiley.org/pdfs/WhileyLanguageSpec.pdf）。Whiley 证明带流类型的并集能在可更新的数据结构上维持非空纪律。它的重定型规则是 DreamScript 收窄的形式基础。
Lobster 在游戏脚本场景里给出带流收窄的并集类型（`x != nil` 这类测试会收窄变量，见 Wikipedia 上引用它的流敏感定型综述）。证据是收窄实现便宜，非函数式背景的用户也能理解。
Erlang 的成功类型（Dialyzer）是这条轴的另一端：完全没有收窄语法，类型作为成功近似被推断。它的用处是提醒：不让用户写并集标注而直接推断流，会产出看不懂的诊断信息。
Phi 那篇文章（“Null, Errors and Enums Are One Feature”，https://www.arshad.fyi/writings/null-errors-and-enums-are-one-feature）是最彻底的综合作法，与作者口味完全吻合：unit 类型（`type Timeout` 本身是类型，而它的唯一值就是它自己），可选值是 `T | none` 且没有 `Some` 包装，错误是不带基类的普通结构体，枚举是 unit 类型的并集。值进入并集时不加包装，收窄走 `is`/`match`，提取走 `or`，其中 `lookup("port") or 8080` 是回退，`read(p) or return` 是传播并按调用方签名检查，布尔短路则自然落下，因为 `bool = true | false` 且 `true` 是第一个成员。它的并集语义是集合式的：重复项塌缩、嵌套拉平，声明位置区分名义同一性（`io.Timeout` != `net.Timeout`）。
与穷尽性和错误处理的交互才是回报：一个函数返回 `Text | Timeout | NotFound`，对它做 `match` 必须覆盖三者。扩展错误集合（`type ReadError = Timeout | NotFound | Locked`）只会让失败路径的匹配断裂，`or return` 链照旧成立。用差集类型（`(T | E1 | E2) - E1`，正是 README 里的 `B - C` 集合减法）处理掉一个成员，就会缩小剩余需要处理的部分。Zig 的错误集合合并与 Koka 的效应行是同一现象（结果集合）从不同角落抵达。集合类型的并集让三者变成一个机制。
需要围绕设计的已知局限：（1）Phi 记录的 `V | none` 拉平歧义：当 `V` 本身包含 `none` 时，存下的 `none` 与缺失的键无法区分，歧义位置需要显式加 `Found[V] | none` 包装，TypeScript 有同样的局限。（2）在可变字段与循环携带变量上收窄，是所有系统都变复杂的地方，默认不可变基本消除了它。（3）第一成员 `or` 约定需要一条有文档的规则（“第一成员就是想要的那个”），此外无需更多。
对 DreamScript 而言：类型位置用 `|`、`&`、`-`（README 已有），局部变量上用带流收窄的 `is`，对并集做穷尽 `match`，`never`/`void` 当空集，以及 Phi 的 `or` 运算符。不要 `Some`，不要 `Ok`，除非用户为某处歧义自己命名一个包装。

## 4. 模式匹配

| 方面 | Rust | OCaml | Scala 3 | F# | Elixir | Roc（新） | Gleam | Koka |
|---|---|---|---|---|---|---|---|---|
| 分支语法 | `pat => expr` | `pat -> expr` | `case p => e` | `\|p -> e` | `p -> e` | `pat => expr` | `pat -> expr` | `pat -> expr` |
| 守卫 | `if cond` | `when cond` | `if cond` | `when cond` | `when cond` | none | none | none |
| 或模式 | `A(x) \| B(x)` | `\|` | `\|` | `\|` | `\|` | none | 列表 `A(x), B(x) ->` | none |
| 绑定 | `x @ pat` | `pat as x` | `x @ pat` | `x as pat` | pin `^x` | 仅嵌套 | 仅嵌套 | 仅嵌套 |
| 穷尽性 | 有 | 有 | 有 | 有 | 部分（无静态检查） | 有 | 有 | 有 |
| 匹配工效学 | 引用上的默认绑定模式 | 无需 | 无需 | 无需 | pin/下划线规则 | 无引用 | 无引用 | 无引用 |

Rust 的守卫 `pat if cond` 与 Kotlin 2.1 在带主语 when 中稳定下来的 `if` 守卫是同一个特性。OCaml 的 `when` 与 Swift 的 `where` 证明这个关键字可以互换。`if` 拼法胜出，因为 DreamScript 已经在用 `if`，而守卫字面上就是一个表达式测试。
或模式：Rust 与 OCaml 允许在同一个分支里写 `Some(1) | Some(2) -> ...`。Gleam 允许每个分支用逗号列出若干完整模式（`1, 2 -> ...`），对扁平枚举读起来更好，但无法共享子绑定。Kotlin 的 `is A, is B` 是仅限扁平类型的版本。推荐 Rust/OCaml 的 `|` 再加下划线规则，成本 S1。
@ 绑定（`x @ Some(y)`）用于既要绑定 `x` 又要在里面继续匹配。OCaml 的 `as` 是同一件事，操作数位置相反。集合类型下最常见的场景（`u is User && u.age >= 18`）根本不需要绑定，因为收窄复用了原变量，所以 @ 绑定是保留集里的删除候选。只有当线性值的匹配工效学要求重新绑定一个已被移动的值时，@ 绑定才值得保留，成本 S1。
对并集的穷尽性从第 3 节直接落下：编译器检查匹配分支覆盖整个集合，并能输出“缺少这些成员”的诊断信息，直接点名集合差集。这比调研过的任何语言给出的错误都好，因为缺失的成员字面上就是一次集合计算。
Elixir 是 pin 运算符（`^x` 按值匹配 `x`）的反面教材：这是单赋值重新绑定语义的后果，DreamScript 通过不可变绑定避免它，不可变绑定不会被静默遮蔽。
匹配工效学：Rust 的默认绑定模式，即穿过 `&`、`&mut` 与类 box 模式自动引用/解引用，是调研对象里唯一的真实复杂度，存在的原因是引用。值语义加线性类型的解释器不需要它。推荐在值上用朴素模式，只有在将来线性借用要求时才加显式的类 `ref` 绑定。

## 5. UFCS 与管道

Koka（已按本地克隆里的 doc/spec/tour.kk.md 核实）：“表达式 `s.encode(3)` 并不从 `:string` 对象上选出 `encode` 方法，它只是函数调用 `encode(s,3)` 的语法糖，其中 `s` 成为第一参数”。它能自由链式调用（`s.encode(3).count.println` 去糖为 `println(count(encode(s,3)))`），并允许任何库通过定义第一参数函数给任何类型加“方法”。这正是作者明确表达的偏好，而且它与效应组合良好：一个带效应的 `parse_int(s) -> Int | E` 变成 `s.parse_int()`，之后按第 11 节的传播变成 `s.parse_int()?`。
Rust 的点号语法建立在 trait 解析上：`x.f()` 通过 trait 与自动引用/解引用解析，并有一套由一致性驱动的固定优先级。这套解析机制是作者今天在 Rust 里付出的隐性复杂度成本。完整 UFCS 加上显式 `Trait::f(x)` 式限定更容易解释，也更容易在解释器里实现（纯去糖，无需候选搜索）。
D 与 Nim 有与 Koka 相同的点号语法糖（Nim 的 method-call syntax，D 的 UFCS），因此这个机制有三份独立的生产级验证。它们都没有报告实践中的困惑，Koka 的 tour 直接把 D 的 UFCS 列为先例。
管道：Gleam 把被管道的值作为第一参数传入（`a |> b(1, 2)` = `b(a, 1, 2)`，已通过 tour.gleam.run 与 v0.8 发布说明核实）。Elixir 做法相同。F# 与 OCaml 的 `|>` 把它作为最后一个柯里化位置传入（`x |> f a b` = `f a b x`）。Hack 与 TC39 提案使用占位符。第一参数管道是与 UFCS 一致的变种，也符合作者想要的 filter-map-reduce 工效学。最后参数管道是柯里化的副产物。
Roc 在 0.14+ 重设计中移除了管道以及 `?>` 一族运算符，新编译器迷你教程里完全没有 `|>`。判定是方法式链式调用加 `?` 已经够用，这说明 UFCS + `?` 覆盖了大部分管道用例。
同一条五段链并排对照，这里展示管道，是因为它确实在读起来更好的那一处（各段有命名且段间有注释）：

```koka
// Koka UFCS: pure desugaring, every stage is a plain call
s.trim.lower.split(",").map(fn(x) x.trim.parse_int.or(0)).sum

// with a pipe for the long stages (Gleam first-arg, same desugaring as UFCS)
s
|> trim
|> lower
|> split(",")
|> map(fn(x) x.trim.parse_int.or(0))
|> sum
```

推荐：完整 UFCS（S2，一条去糖规则，零运行期成本）作为主要链式手段。可选地把 Gleam 风格的第一参数 `|>`（S1）作为第二阶段舒适特性，用于段间带注释的多行链。`x |> f(a)` 去糖为 `f(x, a)`，与 UFCS 完全相同，因此两者正交，管道是纯语法，不引入新语义。

## 6. 绑定与声明形式

调研范围内的绑定关键字：Rust `let`/`let mut`，Kotlin `val`/`var`，Swift `let`/`var`，Zig `const`/`var`，TS `const`/`let`（外加遗留的 `var`），Koka 用 `val`/`var` 且赋值用 `:=`，MoonBit `let`/`let mut`，Roc 用 `$var` 前缀表示可重新赋值，Gleam 只有 `let`（强制不可变），OCaml 只有 `let`，Go `var`/`:=`。val/var 之分（Kotlin、Swift、Koka）最自解释。Rust 的 `mut` 后缀挂在通用的 `let` 上。Roc 的 `$` 是唯一的前缀写法，在表达式里读感很差（`$num * 10 + $num`）。
函数关键字：Rust/Zig/MoonBit `fn`，Kotlin `fun`，Swift `func`，Koka `fun`（顶层）加 `fn`（匿名），OCaml `let f`，Gleam 处处 `fn` 且绑定带名字，TS `const f = (x) => ...` 与 `function f() {}`。TS 的箭头赋值形式（`const f = (a: int) -> int {...}`）已经是 README 的选择，也符合作者对 TS 的喜爱。Koka 的拆分（`fun` 给具名、`fn` 给匿名）用两个关键字划清两类函数的边界。两者都是可行的全局风格（第 12 节展示了两种）。
标注位置：`name: T`（Rust、Kotlin、TS、Koka、MoonBit、Inko、旧 DreamScript README）占主流。后缀 `name T`（Go、Zig）是少数派。`let` 上的标注在各语言都是可选的（局部变量靠推断，公开签名在 Rust/Koka/MoonBit 风格里必写）。推荐 `name: T`，局部变量推断，顶层签名必写。
表达式取向：Koka、Roc、Gleam、Rust（块）、OCaml、MoonBit、Zig（if）与 Kotlin（if/when）都把条件与匹配当表达式，Go 与大部分 TS 则保留语句形式。对带错误并集的语言来说，表达式取向是硬要求：提前返回的语法糖必须落在表达式级别，去糖后仍能保持局部推理。推荐完整的表达式取向。
if/when 作为表达式：在相关语言里普遍存在且免费（S1），同样契合作者的 Lua/TS 习惯。
尾随 lambda：Kotlin 的在工效上占优（`list.map { it * 2 }`）。Koka 的在原理上最彻底，它把任何花括号块都泛化成被挂起的 thunk 实参（`while { i >= 0 } ...`），而尾随 lambda 最初正是 Koka 推广开的（tour，sec-anon）。代价是 `{}` 同时兼任块与 thunk 让 Koka 的语法上下文敏感，Kotlin 把它限制在最后一个实参，更可预测。推荐 Kotlin 风格的最后实参尾随 lambda（S1），代价是放弃 Koka 泛化的花括号即 thunk（S3）。
命名参数与默认参数：作者对 typst 的抱怨（即时求值默认值导致乱序求值，typst/typst#3052，以及嵌套的 `values.at(6, default: values.at(-1))` 模式）是真的：引用其他参数的表达式默认值，要么需要在定义好的顺序里即时求值（typst 的坑），要么把每个默认值惰性地重新定型成一个隐式函数（Koka 把 `len : int = xs.length` 定型为可选的 `?int`，并在被调用方作用域里求值，这有明确定义，但属于 S3 语义）。同一个例子上的三种设计：

```text
// (a) monadic extraction, no default — evaluation order is explicit and local
row.get("scale").float.or(1.0)

// (b) literal-only defaults — the default cannot reference anything, no order exists
fun scale(s: String, factor: Float = 1.0)

// (c) lazy named default (Koka) — desugars to scale(s, factor: () => xs.length.float),
//     evaluated per call inside the callee; well-defined but every default is a hidden closure
fun sublist(xs: List[A], start: Int, len: Int = xs.length)

// (d) caller-scope eager default (typst) — which `xs`? evaluated when? typst#3052
values.at(6, default: values.at(-1))
```

选项，从便宜到贵：（a）不设默认值，在调用点用 `or` 提取，最纯粹，符合单子哲学。（b）只允许字面量/常量表达式默认值，成本 S2，覆盖九成用法，没有求值顺序语义。（c）Koka 风格惰性默认值，成本 S3，完全通用，每个带默认值的参数都付出一个隐藏闭包。推荐（b），计算型回退用（a）。文档里注明（c）存在，以备将来真需要。
where 子句：Rust `where T: Ord`、MoonBit `T : Compare` 约束、Roc 在类型变量上的 `where`。集合类型下约束就是类型集合约束（`T <: Comparable`），一种语法就够了。把它放在泛型参数自身上，不设单独的 where 子句（省下 S1，少一个表层构造）。
花括号 vs 缩进调研：Python（缩进）长期被抱怨复制粘贴、重构与制表符/空格混用。Nim（缩进）重复了这些抱怨，还加上宏编写痛苦。Koka（缩进）能用，且是缩进语言里实现最好的，但它的工具链需要大量能感知空白的设施。Roc 在 0.14+ 重设计里从无括号讨论明确转向花括号。Gleam、Zig、Swift、Go、Rust、MoonBit、TS 全都用花括号。结论：花括号加可选的单行体（Kotlin `if (x) return y` / Rust）是 DreamScript 的低风险默认。只有当作者愿意维护 Koka 级工具链时，缩进才站得住。
分号：三个活跃家族，完全不用、以换行结束语句（Koka、Roc、Gleam-lite），可选并自动插入（Kotlin、TS/ASI 及其著名的坑），必需（Rust、Zig、Gleam 其实必需）。对带行导向 REPL 的解释器而言，语句以换行结束、语法里没有分号是成本最低的规则（S1），并彻底消除 ASI 那类陷阱。改为到处允许尾随逗号。

## 7. 字符串

调研过的内插值约定：Kotlin 用 `"$x"` 作裸标识符、`"${expr}"` 作表达式。Swift 用 `"\(expr)"`（圆括号与元组语法冲突）。TS/JS 用 `"${expr}"`。Rust 只在 `format!`/`println!` 宏内使用格式捕获的 `"hello {name}"`，标识符从作用域裸捕获。Zig 把 `"{s}"` 占位符写在 `std.fmt` 实参里，字面量里什么都没有。Python 用 `f"{expr}"` 加格式规格迷你语言。Roc 用 `"${expr}"` 且表达式本身必须已是字符串（没有隐式 `to_str`，已在迷你教程核实）。MoonBit 用 `"\{expr}"`（按标准库用法）。Go、Gleam、OCaml 没有内插值（Go 用 `fmt.Sprintf`，Gleam 用 `<>` 拼接，OCaml 用 `Printf.sprintf`）。
Kotlin 的方案在除隐式字符串化之外的每个轴上都是赢家：常见场景一个字符，复杂场景一条花括号规则，没有一堆记号，而且正是作者 Lua/TS 肌肉记忆所期待的。需要明确决定：`${expr}` 对非字符串要求 `.to_str()` 式转换（Roc 的规则，显式、类型安全），还是自动字符串化（Kotlin 的规则，方便、隐藏分配）。推荐 Kotlin 语法加 Rust 式的边界显式：非字符串值的内插可见地调用 `str()`（`"${x.str()}"`），或者若作者后来想要自动转换，则走 `Display` 般的 trait。
原始字符串与 heredoc：Rust `r"..."`/`r#"..."#` 用井号计数关闭转义。Kotlin/Swift `"""..."""` 三引号（Kotlin 用 `.trimIndent()` 去掉缩进，Swift 按闭合定界符去缩进）。Go 反引号（完全无转义）。Zig 行首 `\\` 形式。一个构造应当同时覆盖两种需求。推荐 Rust 的井号计数原始字符串（可嵌套，能嵌入引号与代码而不会有转义歧义），或者若作者更看重 Python 的熟悉感则用三引号，两种成本都是 S1。
UTF-8 切片：Rust 的 `&str` 是 UTF-8 加字节索引，非字符边界会 panic，另有 `char_indices()`/`chars()` 迭代器。Swift 的 `String` 按字形簇索引（根本没有整数下标）。Koka 暴露字节并具备 UTF-8 感知。Zig 是 u8 数组，解码交给库。对解释器的推荐：按 UTF-8 存储，零拷贝切片按字节索引但必须落在字符边界上，通过带检查的 API（`s.slice_char(i, j) -> String | none`），逻辑迭代走 `chars`，把字形簇留在核心之外，这属于库的关切。`| none` 返回与第 3 节一致，并避免静默 panic。

## 8. 数值字面量与数值塔

溢出策略：Rust 在 debug 构建里 panic，在 release 里回绕（已记录，按构建而分歧）。Zig 把溢出当作被检查的非法行为（安全模式下 panic，ReleaseFast 下 UB），并提供显式 `+%`/`-*` 回绕运算符。Swift 永远陷入（trap），`&+` 是显式回绕的逃生口。Go 永远静默回绕，Kotlin/Java 静默回绕，C 是未定义。对解释器而言诚实的选择是 Swift 的：溢出永远陷入（可预测、可调试，在带标志的字节码循环里实现便宜），并为哈希/PRNG 用途提供 `+%` 式回绕运算，成本 S1。
字面量到类型的规则：Rust 从上下文推断整数字面量类型，i32 作为回退。Zig 的 comptime-int 在编译期是任意精度，随后强制到目标类型。Go 的无类型常量适配上下文，这是“字面量是一组值”最彻底的形式化：字面就是一组类型。Python 整数永远任意精度，Julia 做类型提升。Go 的无类型常量模型最有集合论味道，契合 DreamScript“每个类型都是集合”的叙事：字面量有字面类型集合，收窄到上下文想要的任何类型。做好要 S3，代价是常量折叠加范围检查，对用户是 S1。
推荐：推断默认用 `Int`（i64）与 `Float`（f64），例外用后缀 `1u8`、`2.0f32`（Rust/Kotlin 约定，S1），数字分隔符 `1_000_000`（S1，人人喜欢）。任何地方都不做隐式收窄转换（Rust 的规则）。任意精度通过 `BigInt` 库实现，字面量本身保持简单。十六进制/二进制/八进制前缀 `0x 0b 0o`（通用）。
计量单位：F# 的 `<Measure>` 属性以零运行期成本给物理单位定型，但它是类型系统扩展（S3），会触及每个数值。作者的集合类型能在库层面表达九成场景（不同的名义包装类型 `type Meters = Wrap[int]` 加 derives）。计量单位推迟处理，不要焊进语言。

## 9. 相等性与同一性

Java：`==` 在对象上是同一性（经典陷阱），值比较用 `equals()`。Python：`==` 比值，`is` 比同一性，而小整数上的 `is` 是大家钟爱的坑。Kotlin：`==` 调 `equals`（结构化），`===` 是引用同一性，这是务实的修法。Rust：没有 `PartialEq` 实现（derived 或手写）就根本没有 `==`，`Eq` 标记全等价，同一性只有裸指针上的 `ptr::eq`。Roc：全都有唯一一个结构化 `==`，没有同一性概念，因为值不可变、语义按值定义。Zig：原生类型上 `==`，深结构比较用 `std.meta.eql`。
在默认不可变加值语义之下，同一性几乎无意义：两个相等的不可变记录可以互换，这正是 Roc 的理由。同一性只在两处仍有意义：（a）线性资源（文件、锁），那里本来就禁止重复。（b）记忆化/驻留，那是实现细节，用户层没有对应运算符。
推荐：唯一一个 `==`，结构化，对记录与和类型自动 derive（对浮点作键之类的情况可通过 trait 覆盖），浮点用 IEEE 语义（`nan != nan`，排序/作键可用 `total_cmp` 式定序），同时去掉 `===` 与 `is`：这个词已被类型测试占用，这正好是个愉快的碰撞，`is` 给类型，`==` 给值，两者各管一摊。删掉 `===` 是相对 Kotlin 的一次负语法糖收益。

## 10. 运算符重载策略

光谱：Go 没有，`==` 之流固定绑到内建，没有例外。Roc（新编译器）没有。Zig 没有，运算符只作用于原生类型。Rust 在固定运算符集合上以 trait 为门槛、优先级固定（`std::ops`），永不支持自定义运算符。Kotlin 在固定 token 集合上用约定方法（`plus`、`get`、`invoke`、`rangeTo`），外加给具名运算符用的 `infix fun` 和自定义优先级提示。Swift 任意自定义运算符加优先级组。OCaml/Haskell/C++ 自由形式运算符加自定义 fixity 声明。
作者的约束把设计钉在 Rust 那一格：固定 token 集合映射到固定 trait 名字，固定优先级表，没有用户自定义 token，没有 fixity 声明。这保住了可解释性（教程里一张短表），让解释器的分派简单（运算符调用 = trait 方法调用，可内联缓存），并与集合类型正确交互：运算符为类型集合上的 trait 定义，按特化优先级定义的方式作者在原则上一概否决。
对线性类型语言，固定集合应包含：算术 `+ - * / %`、取负、位运算、通过 `Ord` 的比较（一次 derive 就覆盖），`Index` 式取值，其余不要花活。`Deref` 式强制转换留在用户表层之外：它们是 Rust 里被误用最多的 trait，UFCS 让它们变得多余。
成本：S2（一个 trait 家族加一条分派规则），与 Kotlin 的约定相同，远低于 Swift/C++ 的自由度。

## 11. 错误与可空值的传播表层

（a）调研过的表层：Rust 的 `?`（提前返回 `Err`，并应用 `From` 转换，这条转换规则是隐藏的 S3）、Zig 的 `try`（提前返回错误）与 `catch |e|`（绑定）与 `orelse`（可选回退）与 `catch`（回退）、Swift 的 `try`/`try?`/`try!`（对“我承认这会抛出”的三种拼法，外加 `do/catch`）、Kotlin 的 `?: return`/`?: throw` Elvis 链与 `runCatching`、Roc 的后缀 `?`（“若是 Ok 就求值为其中的值，否则返回 Err”，已在新编译器教程逐字核实）、Gleam 的 `use x <- result.try(...)` 单子糖、TS 的 `throw`/`catch` 加不受检查的错误、Koka 的效应处理器 `with handler { ... }` 可恢复。
（b）旧的 DreamScript 想法（来自历史 README）：任何块 `{ 1 / 0 }` 都按上下文携带错误并集（“每个错误都能在另一个维度上被抛出”），因此 `{1/0}` 在未处理处是 `double`，在带类型标注处是 `double | ArithmeticError`，提取靠 `.ok((v) -> ...)` / `.err((e) -> ...)` 处理器，`throw` 用于重新抛出。评估：目标是对的（Option/Result/效应共用一个机制，没有针对每个单子的关键字，与 Phi 是同一洞察），但机制有三个缺陷。（1）不具组合性：`{1/0}` 的类型取决于使用点的标注，于是同一个表达式在不同位置有不同类型。局部推理与推断双双失效，这直接违反章程的可解释性预算。（2）`.err` 处理器返回 `double | null` 只是把单子改了名（Rust 有 `Err` 的地方换成 `null`），问题被挪走，语义照旧。（3）花括号同时兼任块、thunk 与错误边界，是 Koka-thunk 歧义再加额外语义。可挽救的部分是块作为错误作用域（也就是 `try { }` 作为表达式）以及 `.err`/`.ok` 作为提取组合子。
（c）推荐的最小表层，因为 Option/Result/效应全都是并集（第 3 节），所以对三者统一：（a）后缀 `?`，Zig/Roc 语义，泛化到并集：`e?`，其中 `e : T | E`，若值属于 `T` 则求值为 `T`，否则提前返回该并集成员。外围函数声明的返回并集必须包含 `E`（要检查，不做 `From` 转换，这是 Zig 的规则，比 Rust 便宜），成本 S2。（b）`or`，Phi 的提取运算符：`x or fallback`，其中 `x : T | F` 得到 `T`（想要第一成员），`or return`、`or continue`、`or break` 是提前退出的形式。这一个 token 取代 Elvis、`unwrap_or`、`orelse` 与 `??`，成本 S2。（c）`.err(h)` / `.ok(h)` 作为并集上的库函数，带集合差集定型：当处理器覆盖 `E` 时，`{expr}.err(handle)` 的类型是 `(T | E) - E`，README 里的 `B - C` 减法在这里做实事，成本 S2，而这正是旧想法里的 `.err`，现在具备组合性。（d）`is` 收窄（免费，第 3 节）覆盖其余一切。
（d）整个错误/可空故事的净语法糖：三个 token（`?`、`or`、`is`）加一个组合子块，对比 Kotlin 的四个（`?.`、`?:`、`!!`、`T?`）、Swift 的五个（`?`、`?`、`!`、`try?`、`try!`）与 Zig 的四个（`try`、`catch`、`orelse`、`?T`/`!T` 对）。如果之后采纳代数效应，Koka 风格的 `with` 处理器能直接插入而无需新增提取语法，因为 `or` 与 `?` 本来就作用于并集成员，这是“表层已为效应做好准备”的最强论据。
（e）推荐的表层在一个小例子上端到端跑通，其中包含修复旧想法的差集类型 `.err` 处理器：

```text
fn load_config(path: String) -> Config | IoError | ParseError {
  const text = read_file(path)?            // IoError propagates; return union contains it
  const cfg  = parse_config(text)?         // ParseError propagates the same way
  cfg
}

const cfg = load_config("app.cfg") or default_config()   // `or`: fallback over the union

const cfg2 = load_config("app.cfg").err((e) => {         // handler covers IoError | ParseError,
  log("config failed: ${e}")                             // result type is Config - (IoError | ParseError)
  default_config()
})                                                       //  == Config, no remainder to handle

const port = cfg2.port or 8080                            // optional is T | none, same three tokens
```

注意消失了什么：`Some`、`Ok`、`unwrap`、`unwrap_or_else`、`try?`、`try!`、`??`、`?.`、`!!`，以及任何针对单个单子的关键字。剩下的是三个 token，它们的语义只需在并集上陈述一次。
明确排除的：Swift 的三种拼法（一个概念三个 S2）。Rust 在 `?` 内部做 `From` 转换（传播处出现意外的类型变化）。上下文依赖的块定型（旧想法的缺陷）。把 `??`/`?.` 当作独立运算符（已被 `or` 与 `is` 涵盖，若真实代码要求，日后可以加一个薄的 `x?.f` 去糖为 `x or none-map`，v1 里为尊重预算先不放）。

## 12. 全局风格候选（交付物 b）

四种自洽的全局风格，每个都在同一个代表性程序上演示：一个小记录定义、一个带错误并集的 parse 函数、对行做 filter-map-reduce、对结果做一次 match，以及一个错误处理器。四者的程序语义完全相同，只有表层不同。这些是手感的草图，最终语法另定。

### 风格 A — TS 家族花括号（箭头赋值、const/let、UFCS、`?`/`or`/`is`）

```ts
type ParseError = BadNumber(String) | MissingField(String)

struct User {
  name: String,
  age: Int,
}

const parse_age = (s: String) -> Int | ParseError {
  match s.trim().parse_int() {
    Ok(n)  => n,
    Err(_) => BadNumber(s),
  }
}

const parse_user = (row: Row) -> User | ParseError {
  const name = row.get("name") or return MissingField("name")
  const age  = row.get("age")  or return MissingField("age")
  User { name, age: parse_age(age)? }
}

const main = () -> () | IoError {
  const rows  = load_rows("users.csv")?
  const parts = rows.map(parse_user).partition((r) => r is ParseError)
  const (users, errors) = parts

  const total = users
    .filter((u) => u.age >= 18)
    .map((u) => u.age * 2)
    .reduce(0, (a, b) => a + b)

  for (u in users) println("user ${u.name} age ${u.age}")

  match (total, errors.first()) {
    (_, e) if e is ParseError => println("warning: ${e}"),
    (0, _) => println("no valid users"),
    (t, _) => println("total score: $t"),
  }
}
```

解读：这是作者的 TS+bun 肌肉记忆配上 Rust 的 match。`const f = (args) -> T { }` 正是历史 README 自己的形式。箭头 lambda `=>` 到处适用。四种风格里最新颖度最低，因此学习成本最低，相对 TS 工具链的语法增量最小。风险：箭头符号疲劳，以及 `const` 关键字身兼三职（绑定、函数、模块条目）。

### 风格 B — Gleam/Roc 花括号（具名 `fn`、`let`、管道可选、`|x|` lambda）

```roc
type ParseError {
  BadNumber(String)
  MissingField(String)
}

fn parse_age(s: String) -> Int | ParseError {
  match s.trim().parse_int() {
    Ok(n) => Ok(n)
    Err(_) => Err(BadNumber(s))
  }
}

fn parse_user(row: Row) -> User | ParseError {
  let name = row.get("name") or return MissingField("name")
  let age = row.get("age") or return MissingField("age")
  Ok(User { name, age: parse_age(age)? })
}

fn main() -> () | IoError {
  let rows = load_rows("users.csv")?
  let (users, errors) = rows.map(parse_user).partition((r) => r is ParseError)

  let total = users
    .filter(|u| u.age >= 18)
    .map(|u| u.age * 2)
    .reduce(0, |a, b| a + b)

  for u in users {
    println("user ${u.name} age ${u.age}")
  }

  match (total, errors.first()) {
    (_, e) if e is ParseError => println("warning: ${e}")
    (0, _) => println("no valid users")
    (t, _) => println("total score: $t")
  }
}
```

解读：花括号里的 ML 家族表层，一切都用 `fn`，绑定用 `let`，lambda 用 `|x|`（Roc 的新语法），匹配分支之间不加逗号。最接近 Roc/Gleam 文化与解释器实现，每个形式都是一个简单节点。风险：`fn`/`let`/`|` 这些记号比 TS 更密。管道风格可用，但为控制样例篇幅没有展示。

### 风格 C — Koka 缩进（fun/val、完整 UFCS、尾随 lambda）

```koka
type ParseError
  BadNumber(string)
  MissingField(string)

struct user
  name : string
  age  : int

fun parse_age( s : string ) : int | parse-error
  match s.trim.parse_int
    Ok(n)  -> n
    Err(_) -> BadNumber(s)

fun parse_user( row : row ) : user | parse-error
  val name = match row.get("name")
    Just(n) -> n
    Nothing -> return MissingField("name")
  val age = match row.get("age")
    Just(a) -> a
    Nothing -> return MissingField("age")
  User(name, parse_age(age))

fun main() : () | io-error
  val rows  = load_rows("users.csv")
  val parts = rows.map(parse_user).partition(fn(r) r is ParseError)
  val (users, errors) = parts

  val total = users
    .filter(fn(u) u.age >= 18)
    .map(fn(u) u.age * 2)
    .foldl(0, fn(a,b) a + b)

  for(users) fn(u)
    println("user ${u.name} age ${u.age}")

  match (total, errors.first())
    (_, Just(e)) -> println("warning: ${e}")
    (0, _)       -> println("no valid users")
    (t, _)       -> println("total score: $t")
```

解读：这是 Koka tour 自己的风格，缩进块、`fun`/`val`、完整 UFCS（`s.trim.parse_int`）、调用之后的尾随 lambda、分支之间没有分号或逗号。它如实暴露了成本：没有 `or`/`?` 时可选值提取会嵌套（两条 `match ... return` 阶梯），而 Just/Nothing 包装之所以出现，是因为 Koka 没有并集式可选值。DreamScript 的风格 C 会用 `or return` 并省掉两条阶梯，但这里比较的正是缩进与 UFCS 的性格。风险：能感知空白的工具链负担，对复制粘贴敏感。

### 风格 D — Rust 家族花括号（fn/let/mut、impl 块、derive、`?`）

```rust
#[derive(Eq, Show)]
enum ParseError {
    BadNumber(String),
    MissingField(String),
}

struct User {
    name: String,
    age: u32,
}

fn parse_age(s: String) -> Result<u32, ParseError> {
    match s.trim().parse::<u32>() {
        Ok(n) => Ok(n),
        Err(_) => Err(ParseError::BadNumber(s)),
    }
}

fn parse_user(row: &Row) -> Result<User, ParseError> {
    let name = row.get("name").ok_or(ParseError::MissingField("name".into()))?;
    let age_s = row.get("age").ok_or(ParseError::MissingField("age".into()))?;
    let age = parse_age(age_s.to_string())?;
    Ok(User { name, age })
}

fn main() -> Result<(), IoError> {
    let rows = load_rows("users.csv")?;
    let (users, errors): (Vec<_>, Vec<_>) = rows.into_iter().partition(|r| !matches!(r, Err(_)));
    let users: Vec<User> = users.into_iter().map(|r| r.unwrap()).collect();

    let total: u32 = users.iter().filter(|u| u.age >= 18).map(|u| u.age * 2).sum();
    for u in &users {
        println!("user {} age {}", u.name, u.age);
    }
    match (total, errors.first()) {
        (_, Some(e)) => println!("warning: {e}"),
        (0, None) => println!("no valid users"),
        (t, None) => println!("total score: {t}"),
    }
    Ok(())
}
```

解读：作者日常的 Rust，粗糙边缘都露在外面：`Result`/`Option` 双重包装、`.ok_or(...)?` 阶梯、因为缺少并集而必需的 `matches!` 与 `.unwrap()`、partition 时的 `collect()` 噪音。这种风格是对照组：熟悉，但它的语法糖都花在补偿缺失的集合类型上，工效学上的收益很少。它不作为全局风格推荐，但如果作者想要最大的 Rust 迁移度，它是有用的锚点。

推荐：风格 A 或 B。A 最大化 TS/bun 迁移（作者明确表达的喜爱），读起来像历史 README。B 对实现最友好（每个构造都是平凡的 AST 节点，Roc/Gleam 证明它能扩展），并留出空间在不破坏 B 代码的情况下后加 A 的箭头语法糖。C 只有在承诺维护空白工具链时才可行。D 除 Rust 熟悉度之外，在每个被测量的轴上都输给 A/B。

## 13. 条目级开关目录（交付物 c）

每个可选形式给出 3-4 种拼法并标出成本，作者每行选一个。S1/S2/S3 如 2.1 所定义。

匹配守卫：
1. `pat if cond -> ...`（Rust、Kotlin 2.1），成本 S1。
2. `pat when cond -> ...`（OCaml、F#），成本 S1。
3. `pat where cond => ...`（Swift），成本 S1。
4. 不要守卫，在分支体里嵌套 `if`（Gleam、Roc），成本 S0 但会嵌套。
或模式：
1. 一个分支里写 `A(x) | B(x) -> ...`（Rust/OCaml），成本 S1。
2. 逗号列表列出若干完整模式：`A(x), B(x) -> ...`（Gleam/Roc 风格），成本 S1。
3. 贯穿共享同一个体（C/Swift 隐式），成本 S2，代价是隐藏控制流。
4. 分开写分支并复制体，成本 S0。
绑定：
1. `x @ pat`（Rust/Scala），成本 S1。
2. `pat as x`（OCaml），成本 S1。
3. 不要绑定，对受检值重新匹配或收窄（集合类型风格 `u is User`），成本 S0。
4. 只在嵌套模式内部隐式产生绑定，成本 S0。
可选值/错误提取：
1. `or` 运算符配 `or return/break/continue`（Phi），成本 S2。
2. Elvis `?:`（Kotlin）加 `!!`（各 S1，但一个概念两个运算符），成本 S2。
3. `orelse` 加 `catch` 两个词（Zig），成本 S2。
4. 只用库：`.unwrap_or(fn)`、`.ok()`、`.err(fn)`，成本 S0 语法，S2 冗长。
传播：
1. 并集上的后缀 `?`，加返回并集包含性检查（Zig/Roc 泛化），成本 S2。
2. `try` 关键字前缀（Zig/Swift），成本 S2，外加绑定用的第二种拼法（`catch |e|`）。
3. do 记法 `use x <- f(...)`（Gleam/Haskell），成本 S3。
4. 上下文定型的块 `{...}` 携带错误并集（旧 DreamScript），成本 S3，第 11 节已排除。
字符串内插值：
1. `"$x"` 与 `"${expr}"`（Kotlin），成本 S1。
2. `"\(expr)"`（Swift），成本 S1，圆括号与元组冲突。
3. `"{x}"` 只在格式函数里捕获（Rust/Zig），成本 S1，但字面量里没有内插值。
4. `"${expr}"` 要求非字符串显式 `.str()`（Roc），成本 S1，最显式。
默认参数：
1. 不要默认值，在调用点用 `or`，成本 S0。
2. 只允许字面量/常量表达式默认值，成本 S2。
3. 惰性命名默认值去糖成闭包，在被调用方作用域求值（Koka 的 `len : int = xs.length` 定名为 `?int`），成本 S3。
4. 完整表达式默认值，在调用方作用域求值（typst），成本 S3，有文档记录的坑（typst#3052）。
命名参数：
1. 任意参数都可选加标签（Kotlin/Koka），成本 S1。
2. 参数超过 2 个后标签必需（Swift 式的纪律），成本 S1。
3. 标签属于 API 的一部分，第一参数通过 UFCS 隐式（Gleam），成本 S1。
4. 只能按位置（Rust/Go），成本 S0，长签名下最差。
尾随 lambda：
1. 最后实参尾随 lambda 加 `it` 简写（Kotlin），成本 S2。
2. Koka 泛化：任何花括号块都是 thunk 实参，成本 S3，代价是上下文敏感语法。
3. 不要尾随 lambda，显式传 `fn`/lambda（Gleam/Roc/OCaml），成本 S0。
可变性标记：
1. `val`/`var`（Kotlin/Swift/Koka），成本 S1。
2. `let`/`let mut`（Rust/MoonBit），成本 S1。
3. `$var` 前缀（Roc），成本 S1，表达式中很别扭。
4. 完全不做重新赋值，改用构造器/函数式更新（`p(age = p.age + 1)`，Koka 复制更新），成本 S0，最激进。
标注位置：
1. `name: T`（Rust/TS/Koka/README），成本 S1。
2. 后缀 `name T`（Go/Zig），成本 S1。
3. 处处推断，只在边界标注，成本 S0 加 S3 推断成本。
相等性：
1. 唯一一个结构化 `==`，自动 derive（Roc），成本 S1。
2. `==` 结构化加 `===` 同一性（Kotlin），成本 S2。
3. 以 trait 为门槛的 `==`（Rust `PartialEq`），成本 S2。
4. `==` 只比同一性，值比较用 `equals()` 方法（Java），成本 S2，坑最多。
数值字面量：
1. 推断 `Int`/`Float` 默认类型加后缀 `1u8`、`2.0f32`（Rust），成本 S1。
2. 无类型字面量集合按上下文收窄（Go 常量），成本 S3 语义，集合论叙事最好。
3. 必须显式标注（类 C），成本 S0 语法糖，S2 冗长。
4. 默认任意精度（Python），成本 S3，解释器路径慢。
运算符重载：
1. 固定 token 集合通过固定 trait 实现，优先级固定（Rust），成本 S2。
2. 约定方法，不允许自定义运算符（Kotlin），成本 S2。
3. 不要（Go/Roc/Zig），成本 S0。
4. 自定义运算符加 fixity 声明（Swift/OCaml），成本 S3，已排除。
块与语句：
1. 花括号，通过换行结束语句来让分号可选（Kotlin/Roc），成本 S1。
2. 缩进（Koka/Python/Nim），成本 S2 加工具链负担。
3. 花括号加必需分号（Rust/Zig），成本 S1，更吵。
4. 花括号，换行结束语句，语法里没有分号（推荐），成本 S1。
区间：
1. `a..b` 半开加 `a..=b` 闭区间作为值（Rust），成本 S1。
2. `a..b` / `a...b` 闭区间（Swift），成本 S1，与展开运算符有三点歧义风险。
3. 只有区间函数 `range(a, b)`（Go），成本 S0。
4. 只在循环里用 `in` 区间，不提供区间值（Kotlin-lite），成本 S1。

## 14. 出处

一手资料（本文直接阅读的文档与源码）：koka tour 与 book 源码，本地克隆 `koka/doc/spec/tour.kk.md`（UFCS、尾随 lambda、with 语句、可选/命名参数、结构体复制更新），线上版本见 https://koka-lang.github.io/koka/doc/book.html。Roc 新编译器迷你教程，https://raw.githubusercontent.com/roc-lang/roc/main/docs/mini-tutorial-new-compiler.md（后缀 `?`、`$var`、`=>` 箭头、`${}` 内插值、无管道）。Gleam 管道，https://tour.gleam.run/functions/pipelines 与 https://gleam.run/news/gleam-v0.8-released（第一参数管道）。Phi，https://www.arshad.fyi/writings/null-errors-and-enums-are-one-feature（unit 类型、`is/match/or`、并集语义、内存布局、合并歧义）。moonbit-core 标准库，本地克隆 `moonbit-core/builtin/{array,result}.mbt`（`pub fn[T] T::f`、`T?`、`raise?`、`~` 标签、记录 punning、`#cfg` 属性）。inko 标准库，本地克隆 `inko/std/src/std/array.inko`（`fn pub inline`、`ref T` 参数模式、`-> Never`）。Kotlin 2.1 与 2.2 发布说明，https://kotlinlang.org/docs/whatsnew21.html 与 https://kotlinlang.org/docs/whatsnew22.html（稳定守卫、非局部 break/continue、multi-dollar 内插值，以及上下文参数预览）。Whiley 语言规范，https://whiley.org/pdfs/WhileyLanguageSpec.pdf（流类型、`type Tree is null | {...}`）。MLscript，https://github.com/hkust-taco/mlscript 与 MLstruct 论文 https://lptk.github.io/files/[v6.2]%20mlstruct.pdf（带主推断的类型布尔代数）。流敏感定型综述，https://en.wikipedia.org/wiki/Flow-sensitive_typing（包含 Lobster）。typst 求值顺序 issue，https://github.com/typst/typst/issues/3052。Gleam `use` 讨论，https://github.com/gleam-lang/gleam/discussions/2146。
二手资料（训练知识、稳定的表层，与上面重叠处已核对）：Kotlin 语言参考、Rust 参考与 std::ops、TypeScript 手册、Swift 语言指南、Zig 语言参考、Go 规范、OCaml 手册、Elixir 指南、F# 计量单位文档。
