# 04 — 语法表层：决策目录

状态：阶段 2 选项目录，写于 2026-11-04，等待作者按 AGENTS.md 作出决策。主要证据：`docs/zh/research/04-syntax-ergonomics.md`，引用为 research/04 §N（其 §14 列出全部 URL）。历史先例：章程之前的历史 `README.md`（章程规定只读）：`const f = (a: int) -> int` 箭头赋值、`?` 作为返回标记、`-*>` 生成器箭头、上下文类型的 `{1/0}` 错误块、`.err`/`.ok` 处理器，在 F2 与 F4 中作为先例审计，从未整体重新采纳。

姊妹目录：`docs/zh/design/01-types.md` 掌管本文件只做铺陈的类型级决策（B1 并集、B3 积类型/和类型、B4c `T?` 别名、B7a 溢出即陷入、B8a UTF-8 字符串、B9a 结构化 `==`）。`docs/zh/design/03-effects-errors-async.md` 掌管错误/效应语义（B5d 混合签名、E4b 带集合差集处理器的并集错误），但明确把 `?` / `or` / `is` / `.err` 这些表层记号推给本文件（其 §2）。`docs/zh/design/02-linearity-ownership.md` 掌管可变性与借用写法（其 C3）。本文件只按路径引用该文件，未阅读其内容。`docs/zh/design/05-generics-metaprogramming.md` 掌管泛型与 where 子句的写法（自 F2 推后）、UFCS 分派所经的 trait 机制（F3）、trait 与 derive 的名字（F5、F7）以及 comptime（F5.d）。`docs/zh/design/00-decision-map.md` 索引所有决策点。

## 决策点索引

| ID | 决策点 | 选项 | 推荐（一行） |
|---|---|---|---|
| F1 | 全局形状 | (a) TS 家族花括号，(b) Gleam/Roc 花括号，(c) Koka/Python 缩进，(d) Rust 家族花括号 | (a) TS 家族花括号：作者的 TS+bun 口味与历史 README 自身的形式，语法增量最小（research/04 §12） |
| F2 | 绑定、函数、标注、返回 | (a) const/var vs val/var vs let/let-mut，(b) const 箭头 vs fn vs fun，(c) `: T` vs `-> T` vs 后缀，(d) 末表达式 vs `return` vs README 的 `?` | `const`/`var`，具名函数写作 `const name = (…) -> T { }`，`name: T` + `-> T`，末表达式返回。`?` 留给传播（docs/zh/design/03 E4）。README 的 `?` 返回标记退役 |
| F3 | UFCS 与管道 | (a) Koka 全量 UFCS，(b) UFCS + Gleam 首参 `\|>`，(c) 仅方法，(d) 无 UFCS 的管道优先 | (a) 现在就上全量 UFCS（S2，纯脱糖），(b) 的 `\|>` 作为 S1 附加项推后。能力参数被排除在点位置之外（docs/zh/design/03 E1c） |
| F4 | 模式匹配 | (a) Rust 形状，(b) Kotlin 2.1 when，(c) Gleam 最小，(d) OCaml when/as | (a) Rust 形状的分支配集合差集穷尽性诊断。`@` 绑定器推后，等 docs/zh/design/02 的线性性结论 |
| F5 | 运算符 | (a) 固定的 trait 驱动集合，(b) 没有（Go），(c) 自由重载 + 自定义，(d) 固定集合 + comptime 生成 | (a) 固定记号集合作用于固定 trait，优先级冻结，默认陷入并配 `+%` 家族回绕运算符（docs/zh/design/01 B7a）。(c) 在语法糖上限下很可能被排除 |
| F6 | 字符串与字面量 | (a) Kotlin `$x`/`${}`，(b) Swift `\(x)`，(c) format 函数 `{x}`，(d) 仅 format（Go） | (a) Kotlin 写法，在非字符串边界显式调用 `str()`，另加 Rust 井号计数的原始字符串（research/04 §7） |
| F7 | 语法糖预算账本 | 最小（约 12）vs 均衡（约 20，推荐）vs 慷慨（约 26）三档 | 均衡档：22 行 / 35 个单位，在 Kotlin 的 38 / 74-76 天花板之下还留 16 个特性与 40-42 个单位的余量（research/04 §2） |

## 共享前提（对所有选项生效）

- 语法糖天花板：Kotlin 的表层，约 38 个特性，按成本加权 74 到 76 个单位。成本刻度 S1 = 一个记号，S2 = 一个构造或一条脱糖规则，S3 = 新的类型语义或认知语义（research/04 §2.1-2.2）。推荐预算为 20 个特性或更少，约 32 个单位（research/04 §1）。F7 建立账本。
- 示例风格：F1 之外的每个代码示例都采用 F1(a) 的 TS 家族基线：`const`/`var` 绑定，`const f = (a: T) -> R { }` 具名函数，`(x) => e` lambda，`name: T` 标注，`-> R` 返回，末表达式返回，语句以换行终止、没有分号，`match e { pat => expr, }` 分支之间带逗号，以便作者公平比较各选项。风格变体只出现在 F1。
- 语义已在别处固定的记号：后缀 `?` 传播、`or` 提取、`is` 收窄、`.err`/`.ok` 差集类型处理器（docs/zh/design/03 E4b/B5d、research/04 §11）。`T?` 是 `T | none` 的零语义别名（docs/zh/design/01 B4c）。默认溢出即陷入，并配显式的 `+%` 家族回绕运算符（docs/zh/design/01 B7a、research/04 §8）。只有一个自动推导的结构化 `==`，没有 `===`（docs/zh/design/01 B9a、research/04 §9）。
- 完全表达式导向：`if` 与 `match` 在每个选项里都是表达式（research/04 §6）。提前返回的语法糖必须停留在表达式层，脱糖规则才是纯语法改写。
- 块规则：三个花括号选项（F1.a/b/d）在语法里共用「语句以换行终止、没有分号」的规则，并允许处处使用尾逗号（research/04 §6、§13 的块选项 4）。F1.c 用有效缩进取代这条规则。
- 函数在每个选项里都是值（历史 README：「每个函数都是可赋值、可解析、可修改的对象」）。F2 只决定声明写法。
- where 子句与泛型参数写法推后到 docs/zh/design/05（research/04 §6 推荐把约束写在参数本身上，不另设 where 子句）。

## F1 — 全局形状：四个风格家族

本点决定的内容：其他一切所依附的框架，即花括号还是缩进、函数头部，以及整体的观感。下面四个程序复用自 research/04 §12（交付物 b），只做了一处改动：风格 C 的 `Just`/`Nothing` 阶梯换成 `or return`，这正是 research/04 §12 为 DreamScript 版本规定的做法（Koka 缺少并集可选值，DreamScript 按 docs/zh/design/01 B4 具备）。风格 A 与 B 保留研究原文里的 `Ok(...)`/`Err(...)` 写法。在 docs/zh/design/01 B1 的并集与 docs/zh/design/03 B5d 之下，这些包装会消解为普通并集成员（见 F4），因此这次比较针对的是形状，与包装策略无关。四个程序的语义完全相同，只有表层不同。

### F1.a — TS 家族花括号（`const f = (a: int) -> int { ... }`）

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

先例：历史 README 自身的函数形式、TS 的 `const f = (x) => ...` 加 Rust 的 match，在 research/04 §12 中作为风格 A 端到端演示。

权衡：语法糖成本 S1（花括号块、换行终止）。解析器复杂度四者最低：每个块都以可见记号开始，语法面向行，TS 家族的工具直觉可以直接迁移。对这个作者来说学习成本最低（research/04 §12：「四者中最不新颖的表层」）。研究指出的风险：箭头疲劳，以及 `const` 一词身兼绑定、函数与模块项三重角色（F2 决定这一拆分）。

排除：本目录内不排除什么。它略微不利于日后改名为 `fun` 关键字（F2.b），因为混用两种声明形式得不偿失。

### F1.b — Gleam/Roc 花括号（`fn add(a: Int) -> Int { ... }`）

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

先例：花括号里的 ML 家族表层，即 Roc 新编译器的语法（`|u|` lambda、后缀 `?`，https://raw.githubusercontent.com/roc-lang/roc/main/docs/mini-tutorial-new-compiler.md）与 Gleam 的 tour，在 research/04 §12 中作为风格 B 演示。

权衡：语法糖成本 S1。解析器复杂度同样低，research/04 §12 称其「最友好于实现（每个构造都是一个简单的 AST 节点）」。对这个作者来说学习成本高于 (a)：`fn`/`let`/`|x|` 这些符号比 TS 箭头更密集，也没有任何构造能复用 TS 肌肉记忆。

排除：以 `|x|` 取代箭头 lambda 的习惯（`(x) => e`）。日后要是采纳 (a) 的箭头语法糖，语言的 lambda 写法就会分裂。

### F1.c — Koka/Python 有效缩进（`fun` / `val`，没有花括号）

```koka
type ParseError
  BadNumber(string)
  MissingField(string)

struct user
  name : string
  age  : int

fun parse_age( s : string ) : int | ParseError
  match s.trim.parse_int
    Ok(n)  -> n
    Err(_) -> BadNumber(s)

fun parse_user( row : Row ) : user | ParseError
  val name = row.get("name") or return MissingField("name")   // adapted per research/04 §12:
  val age  = row.get("age")  or return MissingField("age")    // the Just/Nothing ladder disappears
  User(name, parse_age(age)?)

fun main() : () | IoError
  val rows = load_rows("users.csv")?
  val (users, errors) = rows.map(parse_user).partition(fn(r) r is ParseError)

  val total = users
    .filter(fn(u) u.age >= 18)
    .map(fn(u) u.age * 2)
    .foldl(0, fn(a, b) a + b)

  for (u in users)
    println("user ${u.name} age ${u.age}")

  match (total, errors.first())
    (_, e) if e is ParseError -> println("warning: ${e}")
    (0, _) -> println("no valid users")
    (t, _) -> println("total score: $t")
```

先例：Koka tour 自身的布局（https://koka-lang.github.io/koka/doc/book.html），在 research/04 §12 中作为风格 C 演示。research/04 §6 的花括号与缩进调查（Python/Nim 关于结构的抱怨，Koka 是唯一健康的缩进语言，Roc 在 0.14+ 明确回到花括号）。

权衡：语法糖成本 S2，另加一项未计价的工具负担（research/04 §13 排除了选项 2）。解析器复杂度最高：有效空白要求解析器、格式化器、REPL 和错误恢复器都具备 Koka 级别的空白感知基础设施（research/04 §6）。学习成本中等，但复制粘贴与重构的敏感性是永久的（Python/Nim 的抱怨记录，research/04 §6）。

排除：便宜的面向行工具。机器编辑代码变得有风险，格式化器、补丁与生成的片段都在其列。共享前提里的无分号换行规则被布局吞并。

### F1.d — Rust 家族花括号（`fn` + `let` + 花括号，impl 块）

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

先例：日常的 Rust，在 research/04 §12 中作为风格 D 演示：「对照组：熟悉，但它的语法糖花在弥补缺失的集合类型上，并未花在易用性上」（`Result`/`Option` 的双层包装、`.ok_or(...)?` 阶梯、`matches!`/`unwrap`/`collect` 的噪声）。

权衡：语法糖成本 S1。解析器复杂度低，但 impl 块这一层多出一级声明嵌套 (a)。对这个作者来说学习成本低（Rust 是日常练习），可是这种风格与集合类型基础相抗争，research/04 §12 称风格 D「除 Rust 熟悉度外，在每一项被测轴上都不如 A/B」。

排除：并集承载的错误（docs/zh/design/03 B5d）读起来自然。它重新引入了 `Result`、`Some`、`.unwrap()` 这套包装词汇，而 docs/zh/design/01 B1 与 research/04 §3 的存在正是为了删掉它们。

### F1 对比与结论

| 风格 | grep 友好度 | diff 噪声 | 工具简单度 | 作者的 TS 口味 | 总评 |
|---|---|---|---|---|---|
| F1.a TS 家族 | 好：定义可以 grep 成 `const name =`（与其他 const 绑定冲突，按名字 grep 不受影响） | 低：最多是只有花括号的行 | 最高：面向行的解析器，没有布局状态，无需 ASI | 最高：TS+bun 肌肉记忆，README 自身的形式 | 推荐 |
| F1.b Gleam/Roc | 最好：`fn name(` 是无歧义的定位锚 | 低：只有花括号的行 | 最高：每种形式都是简单的 AST 节点（research/04 §12） | 中低：ML 符号，没有箭头 | 若实现简单度优先则最强 |
| F1.c Koka/缩进 | 中等：名字可 grep，块的起止范围不能 | 高：重排缩进与复制粘贴会泛起涟漪（research/04 §6） | 最低：处处都要感知空白 | 最低：没有 TS 迁移 | 只在承诺工具投入时 |
| F1.d Rust 家族 | 最好：`fn name(` 锚加 impl 块 | 中等：四空格花括号块增加行数 | 高，外加 impl 块这一层 | 中等：只有 Rust 熟悉度 | 对照组。在集合类型契合度上落后 |

交互：不可变绑定的写法与 docs/zh/design/02（C3，未阅读，仅按路径引用）交互。四种风格都展示了双关键字或标记式的拆分，且都不预设 02 的决定。`?` / `or` / `is` 记号在四者中渲染一致（docs/zh/design/03 E4）。花括号选项继承无分号的换行规则（research/04 §6），所以 F1 真正的差异是 (c) 与其他一切之间，然后是 (a) 与 (b)/(d) 在头部写法上的差异。

推荐：F1(a) TS 家族花括号。它既是历史 README 自身的形式，也是作者声明的口味，语法增量最小。把 F1(b) 保留为现成的备选，一旦原型显示箭头赋值的语法与解释器的解析器相争就切换过去。F1(c)/F1(d) 丢弃。

## F2 — 绑定、函数、标注、返回

四个子决策。老 README 具体是 (b) 与 (d) 的先例：`const f = (a: int) -> int { ... }` 以 `?` 作返回标记，以及 `-*>` 作为生成器箭头的种类标记。

### F2.a — 默认可变之下的一对绑定关键字

```ts
const port = 8080            // (1) const / var  — README + TS + Zig
var   hits = 0               //     mutation is the loud, local choice (charter req 5)

val   name = "ds"            // (2) val / var    — Kotlin, Swift, Koka
var   other = name

let   done = false           // (3) let / let mut — Rust, MoonBit
let   mut count = 0          //     `mut` as a modifier scales to graded mutability

const p2 = p(age = p.age + 1)  // (4) no reassignment at all — Koka copy-update (research/04 §13)
```

| 选项 | 关键字对 | 先例 | 成本 | 排除 |
|---|---|---|---|---|
| F2.a.1 | `const` / `var` | 老 README、TS 的 `const`、Zig（research/04 §6） | S1 | 范围内不排除什么。代价是失去 val/var 的押韵 |
| F2.a.2 | `val` / `var` | Kotlin、Swift、Koka（research/04 §6：「自我说明性最强」） | S1 | TS/README 的肌肉记忆 |
| F2.a.3 | `let` / `let mut` | Rust、MoonBit（research/04 §13 可变性选项 2） | S1 | 两词对称，换来一个修饰符。如果 docs/zh/design/02（C3）需要分级可变性标记，这修饰符能活下来 |
| F2.a.4 | 完全不做重新赋值，改用复制更新 | Koka 的 `p(age = ...)`（research/04 §13 选项 4，S0） | S0 | 命令式脚本风格。会修改累加器的循环 |

交互：`var`（或 `mut`）意味着什么，只是重新赋值、深层可变，还是线性 put，都由 docs/zh/design/02 C3 掌管（仅按路径引用）。表层关键字对必须能与 02 的标记组合，如果 02 最终采用分级可变性，这一点就让 (1)/(3) 优于 `val`/`var` 这种固定的押韵组合。推荐：F2.a.1 `const`/`var`，沿用 README 自己的词与 TS 的习惯，代价是一个五个字母的承诺。只有当 docs/zh/design/02（C3）需要不止一个可变性标记时才重新考虑。

### F2.b — 函数形式

```ts
const parse_age = (s: String) -> Int | ParseError {   // (1) const-arrow: old README, TS
  42
}

fn parse_age(s: String) -> Int | ParseError {         // (2) fn declaration: Rust, Zig, MoonBit, Gleam
  42
}

fun parse_age(s: String) -> Int | ParseError { ... }  // (3) fun/fn split: Koka, Kotlin — `fn` stays anonymous
```

| 选项 | 形式 | 先例 | 成本 | 排除 |
|---|---|---|---|---|
| F2.b.1 | `const name = (…) -> T { }`，匿名 `(x) => e` | 老 README §Function、TS（research/04 §6） | S1 | 一个专用于 grep 定义的关键字。除此之外没有实质损失 |
| F2.b.2 | `fn name(…) -> T { }` | Rust/Zig/MoonBit/Gleam（research/04 §6） | S1 | 声明与表达式的一致性（README 的函数即对象）。除非采用 Gleam 式处处 `fn`，否则有 Rust 式 fn 项与闭包分裂的风险 |
| F2.b.3 | 具名 `fun` + 匿名 `fn` | Koka/Kotlin，research/04 §6：「关键字家族里最干净的」 | S1 | 代价是两种写法，两个关键字要保持区分 |
| F2.b.4 | 同时允许 (1) 与 (2) | Swift 的 `func` 加闭包、TS 的 `function` 加 const 箭头 | S2 | 预算纪律：一个概念两种写法，风格之争 |

先例审计（老 README）：生成器箭头 `-*>`（`const f = (a: int) -*> int`）把函数的种类编码进箭头里。现代的对等做法是修饰符关键字、返回通道标记或效应行（docs/zh/design/03 B5c）。生成器在本目录里没有决策点，已推后。这里记录「箭头编码种类」这一想法，以免它无声消失。交互：docs/zh/design/05 掌管 (1) 的泛型参数怎么写（`const f[T] = ...`）。推荐：F2.b.1 const 箭头，它与绑定一致，符合 README 的函数即值立场与 F1(a) 基线。如果 F1 落在别处，这一点随之重新决定。

### F2.c — 标注位置与返回箭头

```ts
const f = (a: Int, b: String) -> Bool { ... }   // (1) `name: T` params, `-> T` return (README, TS, Rust)
const g = (a: Int, b: String) : Bool { ... }    // (2) `: T` for both (Koka, Scala)
fn h(a Int, b String) Bool { ... }              // (3) postfix `name T` (Go, Zig)

type Mapper = (Int) -> Bool                     // function-type arrow matches the return arrow under (1)
const short: (Int) -> Bool = (n) => n > 0       // locals inferred; boundary signatures required
const maybe: Int? = row.get("port")             // `T?` alias = Int | none (docs/en/design/01 B4c)
```

| 选项 | 写法 | 先例 | 成本 | 排除 |
|---|---|---|---|---|
| F2.c.1 | 参数 `name: T` + 返回 `-> T` | 老 README、Rust/TS/Koka（research/04 §6：`name: T` 占主导） | S1 | 不排除什么 |
| F2.c.2 | 参数与返回都用 `: T` | Koka、Scala（research/04 §6） | S1 | 函数类型 `(A) -> B` 里「箭头意味着产生」的读法 |
| F2.c.3 | 后缀 `name T` | Go、Zig（research/04 §6） | S1 | TS 口味。函数类型随之写成后缀形式，`(A) -> B` 的箭头消失 |
| F2.c.4 | 只在边界标注，内部全推断 | research/04 §13 标注选项 3 | S0 表层，S3 推断 | 显式的局部签名。教学叙事 |

无论哪个选项都共有同一条子规则：局部推断，顶层与公开签名必须标注（research/04 §6）。并集返回直接写作 `-> A | B`，不需要额外语法。交互：`T?` 在任何标注位置都可作为 docs/zh/design/01 B4c 的别名使用。泛型约束的写法推后到 docs/zh/design/05。推荐：F2.c.1 加上边界规则，一个冒号表示「具有类型」，一个箭头表示「产生」，两者在 README 里已经存在。

### F2.d — 返回机制，以及 `?` 的冲突（已标注）

```ts
const area = (r: Rect) -> Int {
  r.w * r.h                    // (1) final expression is the value (Rust, Koka; README's own second form)
}

const find_user = (xs: List[User], id: Int) -> User | none {
  const hit = xs.find((u) => u.id == id)
  hit or return                // early exit via `or` (research/04 §11; docs/en/design/03 E4)
  hit
}

const old_readme = (a: int) -> int {
  ? a                          // (3) the README's `?` return marker — see the flag below
}
```

**标注：记号冲突。** 老 README 用前缀 `?` 返回值，也用来结束生成器。现在 `?` 是后缀错误传播记号，语义由 docs/zh/design/03 E4 与 research/04 §11 固定：`e?` 作用于 `T | E` 时产生 `T`，或提前返回错误成员。一个记号将承载两种无关的含义，只能靠前缀与后缀的位置区分，机器能解析，人和错误信息都读不出区别。README 的 `?` 甚至身兼三职：返回、结束生成器，以及它的错误示例。可选解法：(i) 只用末表达式，`?` 留给传播。(ii) 保留 `?` 返回，把传播降级为 `try` 风格关键字，这会排除研究过的 `?` 语义，增加一个 S2 关键字，并破坏与 docs/zh/design/03 的一致性。(iii) 只在尾部位置用 `?` 返回，含义取决于位置，是三个解法里最差的一种。

| 选项 | 机制 | 先例 | 成本 | 排除 |
|---|---|---|---|---|
| F2.d.1 | 末表达式。提前退出走 `or return` / `or break` / `or continue` 与 `?` | Rust 块、Koka、README 的次要形式、research/04 §11 | S1 | 函数中段裸用的逃生口（少见，`or` 形式覆盖了并集的情形） |
| F2.d.2 | 只用显式的 `return` 关键字 | TS、Go、Python | S1 | 表达式导向收不到回报。漏写返回的 bug 会回归 |
| F2.d.3 | README 的前缀 `?` 标记 | 老 README §Function | S1 + 冲突 | 如上标注。建议彻底退役这个标记 |
| F2.d.4 | 末表达式 + 可选的 `return` 关键字 | Rust、Kotlin | S1 | 「块即值」的纯粹性。小的风格分裂 |

交互：`or return` 由 docs/zh/design/03 E4 的 `or` 运算符加一个提前退出目标组合而成（research/04 §11c）。`?` 传播要求外层返回并集包含被传播的成员（受检查，无转换）。推荐：F2.d.1 末表达式返回，`?` 留给传播，README 的 `?` 标记退役。如果真实代码有需求，F2.d.4 的 `return` 日后可作为纯 S1 附加项加入，它不排除什么。

## F3 — UFCS 与管道

```ts
// the shared five-stage chain, four ways (research/04 §5)
s.trim.lower.split(",").map(clean).sum                    // UFCS: every dot is plain sugar
s |> trim |> lower |> split(",") |> map(clean) |> sum     // first-arg pipe: same desugaring
s.trim().lower().split(",").map(clean).sum()              // methods-only: trim/lower must be declared methods
sum(map(split(lower(trim(s)), ","), clean))               // pipe-free FP spelling
s.encode(3)                                               // verified Koka semantics: encode(s, 3)
```

| 选项 | 机制 | 先例 | 成本 | 排除 |
|---|---|---|---|---|
| F3.a | 全量 UFCS：`x.f(a)` = `f(x, a)`，作用于任何首参函数 | Koka tour，在 research/04 §5 验证过、D 与 Nim 有同样的点语法糖，三份生产证据，没有混淆报告 | S2：一条脱糖规则，零运行期成本，不做候选搜索 | 不排除什么。还需要下面的优先级规则 |
| F3.b | F3.a + Gleam 首参管道 `x \|> f(a)` = `f(x, a)` | Gleam，已核实 https://tour.gleam.run/functions/pipelines 与 v0.8 说明（research/04 §5） | S1：同样的脱糖，纯语法 | 不排除什么。代价是一个代码库里有两套链式写法 |
| F3.c | 仅方法：`x.f()` 要求 `f` 被声明为 `x` 类型的成员方法 | TS、Rust、Go | S0 表层 | 给外部类型加「方法」需要 docs/zh/design/05 的 trait 机制。Rust 的自动引用/解引用候选搜索正是 research/04 §5 警告的隐藏 S3 |
| F3.d | 无 UFCS 的管道优先 | Elixir 的 `\|>` 首参、F#/OCaml 末参（「柯里化的产物」，research/04 §5） | S1 | 完全排除点链语法糖。末参变体一旦混用就会与首参 UFCS 相争 |

交互：与 trait（机制由 docs/zh/design/05 掌管）衔接时，UFCS 的消解顺序固定且简单：先自由函数，再 trait 方法，`Trait::f(x)` 限定作为逃生口。真正有歧义就是编译错误，永不做优先级搜索（research/04 §5 对 Rust 由一致性驱动的消解的批评）。与效应和能力参数（docs/zh/design/03 E1c）衔接时，固定的规则是：点调用喂给第一个非能力数据参数。能力参数是具名的，被排除在 UFCS 位置之外，需要显式传入或通过某个作用域形式传入，其写法由 docs/zh/design/03 E1 掌管（`println(io, "done")` 永远不能写成 `msg.println()` 把 `io` 吞掉）。与 `T?` 和安全调用：没有 `?.` 运算符，链式可选访问由 `x.f() or ...` 与 `is` 收窄组合而成（research/04 §11 明确把 `?.` 挡在 v1 之外）。Roc 是 F3.b 的反面证据：它删掉了自己的管道，理由是与方法链加 `?` 重复（research/04 §5），因此在 F7 账本里管道记作 defer，与 include 档区分开。

推荐：F3(a) 现在就上全量 UFCS，它是解释器只需消解一次的纯脱糖，配上面的能力参数规则。F3(b) 的 `|>` 作为 S1 附加项推后，日后加上也不破坏任何东西。

## F4 — 模式匹配

```ts
type ReadError = Timeout | NotFound | Locked

const serve_all = (r: Config | ReadError) -> () {
  match r {
    Timeout           => retry(),                          // unit/tag patterns
    NotFound | Locked => println("unrecoverable"),         // or-patterns in one arm (research/04 §4)
    cfg               => serve(cfg),                       // cfg : Config — set difference did the work
  }
}

const check = (u: User) -> () {
  match u {
    { age } if age >= 18 => println("adult ${age}"),       // guard: `pat if cond` (Rust, Kotlin 2.1)
    { age }              => println("minor ${age}"),
  }
}

const first_or_skip = (xs: List[User | ParseError]) -> () {
  const h = xs.first() or return                           // `or` early exit (docs/en/design/03 E4)
  const msg = if (h is ParseError) "skipped: ${h}" else "ok: ${h.name}"
  println(msg)                                             // `h` narrowed by `is` in each branch
}

match load(p) {
  n       => serve(n),
  Timeout => retry(),
}
// error: match is not exhaustive; missing members: NotFound | Locked
//        coverage is set arithmetic: (Config | Timeout | NotFound | Locked) - Config - Timeout
```

| 选项 | 装备 | 先例 | 成本 | 排除 |
|---|---|---|---|---|
| F4.a | Rust 形状：`pat if guard`、或模式 `\|`、`x @ pat` 绑定器（推后）、配集合差集诊断的穷尽性 | research/04 §4（Rust，Kotlin 2.1 的守卫是同一个特性） | 守卫 S1、或模式 S1、绑定器 S1、穷尽匹配 S2 | 不排除什么。诊断需要并集类型（docs/zh/design/01 B1） |
| F4.b | Kotlin 2.1 的 `when (v) { is Int -> ... }`，配稳定的守卫与主语绑定 | research/04 §2.2 第 9 项、https://kotlinlang.org/docs/whatsnew21.html | S2（带主语的 when） | 第二个 match 关键字，且 `->` 分支与基线的 `=>` 相左。其他方面能力等价 |
| F4.c | Gleam 最小：没有守卫，整模式逗号列表，体里嵌套 `if` | research/04 §4 的表、research/04 §6 里嵌套膨胀的警示故事 | S0 语法糖，但分支会嵌套 | 扁平的决策表。每一次条件分派都变成分支里的 `if` |
| F4.d | OCaml 风格：`pat when cond` 守卫 + `pat as x` 绑定器 | research/04 §4（OCaml 的 `when`/`as`，Swift 的 `where` 是同一个关键字槽） | 各 S1，与 F4.a 相同 | 相对 F4.a 只有用词差异。`as` 比 `@` 读起来更长 |

各选项共有的注记：在集合类型下，最常见的守卫（`u is User && u.age >= 18`）根本不需要绑定器，因为 `is` 会收窄原始变量（research/04 §4），这也是 `@`/`as` 绑定器成为删除候选的原因：只有当线性匹配的人机工程学需要重新绑定一个已被移出的值时才保留它们，而这个问题由 docs/zh/design/02 掌管（仅按路径引用）。集合差集诊断（「缺失：NotFound | Locked」）优于任何被调研的语言，因为缺失成员字面上就是一次集合运算（research/04 §4）。如果 docs/zh/design/01 最终落地的是仅名义的 B1.b，同一个检查器会平稳降级为列出缺失的 enum 分支。与错误的交互：`?` 与 `or` 是部分处理层，`match` 是同一并集上的全处理器，一套机制，没有逐 monad 的关键字（docs/zh/design/03 E4b、research/04 §11）。历史 README 审计：它的差集类型 `.err` 处理器与上下文类型的 `{1/0}` 块是非 match 处理路径的先例。块类型化机制因不具组合性被排除（research/04 §11 第 1 项缺陷），而 `.err`/`.ok` 被抢救为差集类型组合子（docs/zh/design/03 E4b、research/04 §11c）。README 的 `B - C` 集合减法想法正是以这条诊断的形式存活下来。

推荐：F4(a) Rust 形状的分支，守卫用 `if`，或模式用 `|`，穷尽并集配集合差集诊断。`@` 绑定器明确推后到 docs/zh/design/02 的结论。

## F5 — 运算符

```ts
// (a) fixed trait-driven set: a + b desugars to Add.add(a, b); trait names owned by docs/en/design/05
trait Add { fn add(self, rhs: Self) -> Self }
const v3 = a + b - c * 2          // only the fixed tokens below can be given trait impls
const m  = xs[0]                  // Index-style get is in the fixed set (research/04 §10)

// (b) none: `+` exists only on builtin numerics; BigInt/complex read x.add(y)
// (c) free overloading + definable operators with fixity (Kotlin infix fun, C++, OCaml) — likely-rejected
// (d) fixed set on primitives + comptime generates Vector3.add(a, b) you call by name (Zig flavor)
```

| 选项 | 策略 | 先例 | 成本 | 排除 |
|---|---|---|---|---|
| F5.a | 固定的记号集合映射到固定的 trait，优先级固定，其他一律不重载 | Rust 的 `std::ops`，research/04 §10：「作者的约束把选择推到 Rust 的槽位」 | S2（一个 trait 家族加一条分派规则，运算符调用即 trait 方法调用，可内联缓存） | 永远没有自定义运算符记号。`Deref` 式强制转换被排除在用户表层之外（research/04 §10） |
| F5.b | 用户不能重载，运算符固定于内建类型 | Go、Roc（新版）、Zig（research/04 §10） | S0 | BigInt、复数、向量上的 `+` 无从定义，损害 docs/zh/design/01 B7 的数值塔扩展。DSL 里用户类型只能写 `x.add(y)` |
| F5.c | 自由重载加用户自定义运算符与优先级声明 | Kotlin 约定 + `infix fun`、Swift 优先级组、C++、OCaml（research/04 §10，§13 选项 4「S3，已排除」） | S3：在语法糖上限下标记为很可能被排除 | 章程的可解释性预算。grep 友好度（优先级变成声明局部的） |
| F5.d | 固定集合，仅内建类型，由 comptime 生成按类型的算术并按名调用 | Zig 的 comptime（research/04 §2.3） | S1 表层 + comptime 机械，成本记在 docs/zh/design/05 G1 | 用户类型上的运算符语法。改用代码生成找回样板 |

溢出运算符写法表（策略由 docs/zh/design/01 B7a 固定：陷入是解释器下唯一可预测的策略，见 research/04 §8。写法沿用 Zig）：

| 运算 | 陷入（默认） | 回绕（显式） | 注记 |
|---|---|---|---|
| 加 / 减 / 乘 | `a + b`、`a - b`、`a * b` | `a +% b`、`a -% b`、`a *% b` | 陷入会带位置 panic。回绕运算符是给哈希/PRNG 用的 S1（research/04 §8） |
| 取负 | `-a` | `-%a` | Int 最小值的边界 |
| 除 / 取余 | `a / b`、`a % b` | 无 | 除零与 Int 最小值除以 -1 始终陷入。不存在回绕除法 |
| 移位 | `a << k`、`a >> k` | 无 | `k` 超出位宽时陷入。循环移位放在库里 |
| 浮点 | IEEE 语义，永不陷入 | 不适用 | inf/nan 按 IEEE 处理（docs/zh/design/01 B7a） |

优先级策略选项：(1) 冻结的常规 C 家族表，在教程里公布一次，永不新增运算符，永不重新安排优先级。(2) 原样采用 Rust 当前的表，包括它的怪癖（后缀 `?` 的链式、区间记号），同样冻结，对作者来说最熟悉。(3) Swift 式优先级组或 OCaml 优先级声明，随 F5.c 一起排除（S3，research/04 §13）。交互：`==` 是 docs/zh/design/01 B9a 的结构化相等（用户不能重载，排序走 `Ord` derive，由 docs/zh/design/05 掌管）。回绕的 `+%` 家族是 docs/zh/design/01 B7a 决定的表层写法，与其他任何记号都不共用。推荐：F5(a) 固定的 trait 驱动集合，配策略 (1) 的冻结优先级，外加上面的溢出表。F5(d) 的 comptime 路线属于 docs/zh/design/05 G1，日后可以叠加而不破坏 F5(a)。

## F6 — 字符串与字面量

```ts
const name = "dream"
const n = 3
const a = "hello $name, ${n} apples"       // (a) Kotlin: bare id + one brace form (research/04 §7)
const b = "hello \(name)"                  // (b) Swift: paren form
const c = println("{name} has {n}", name, n)  // (c) placeholders live in the format call, not the literal
const d = "hello " + name                  // (d) concat only (Go fmt.Sprintf, Gleam <>)

const loud = "${name.str().upper()}"       // (a) explicitness rule: non-strings call str() visibly
```

| 选项 | 机制 | 先例 | 成本 | 排除 |
|---|---|---|---|---|
| F6.a | `$x` 裸标识符，`${expr}` 表示表达式，非字符串边界显式调用 `str()` | Kotlin（「除隐式字符串化外每项轴上都胜出」，research/04 §7。在边界处采纳 Roc 的显式规则） | S1 | 会隐藏分配的自动字符串化（日后可以通过类似 Display 的 trait 加入，docs/zh/design/05） |
| F6.b | `\(x)` 始终带括号 | Swift（research/04 §7：括号与元组语法冲突） | S1 | 不排除什么，但每次插值多两个字符，而且与元组相邻时记号泛滥 |
| F6.c | `{x}` 占位符在 format 函数里捕获 | Rust 的 `format!` 捕获、Zig 的 `std.fmt`、Python f-string 规格（research/04 §7） | S1 | 普通字符串字面量里的插值。会招来一套 format 规格迷你语言 |
| F6.d | 没有插值，只用 format 或拼接 | Go 的 `fmt.Sprintf`、Gleam 的 `<>`、OCaml 的 `Printf`（research/04 §13 字符串选项 4） | S0 语法，S2 啰嗦度 | 字面量里没法直接插值。format 字符串除非受检，否则就是字符串类型，与章程的编译期错误口味相左 |

原始字符串、多行、heredoc：一个构造应当同时覆盖三者（research/04 §7）：

```ts
const re  = r"\d+\.\d+"                     // raw: no escape processing
const sql = r#"SELECT * FROM "users" WHERE k = 'v'"#   // hash-counted: any quotes, nests to any depth
const doc = r#"
line one
line two
"#                                          // the same construct serves as heredoc/multiline
```

Rust 的井号计数 `r#"..."#` 在嵌套与嵌入代码、引号方面胜出，没有转义歧义。三引号（`"""..."""`，Kotlin/Python/Swift）是熟悉的替代方案。Go 的反引号字符串里嵌不进反引号。Zig 的 `\\` 行形式最弱（research/04 §7）。两种做法的成本都是 S1。字符与字节字面量：`'x'` 是一个 Unicode 标量值（`Char`，按 docs/zh/design/01 B8a 的 UTF-8 存储），`b'x'` 是用于传输格式的单个 `u8` 字节，成本 S1，与 Rust 的那一对一致。交互：字符串存储与受检切片（`s.slice_char(i, j) -> String | none`）由 docs/zh/design/01 B8a 掌管。`str()` 边界函数是 `Show` 家族 trait 的投影，名字在 docs/zh/design/05 里给。`T?` 值的插值仍要求显式处理，可选值的静默打印被排除。推荐：F6(a) Kotlin 写法配显式的 `str()` 边界规则，加上 Rust 井号计数的原始字符串和 `'x'` / `b'x'` 字面量对，全是 S1，全部符合 research/04 §7 的结论。

## F7 — 语法糖预算账本

账本按 S1/S2/S3 刻度给每一项候选特性计价（research/04 §2.1），按档位标记 include / exclude / defer，并对 Kotlin 的 38 个特性 / 74-76 个单位天花板求和（research/04 §2.2）。「Defer」表示不在首发预算里，但可以附加而不破坏。

| # | 特性 | S | 最小 | 均衡 | 慷慨 | 理由（一行） |
|---|---|---|---|---|---|---|
| 1 | 语句以换行终止，没有分号 | S1 | 收 | 收 | 收 | research/04 §6：最便宜的规则，删掉整套 ASI 隐患（F1.c 把这一行换成缩进 S2） |
| 2 | 字符串插值 `$x` / `${e}` | S1 | 收 | 收 | 收 | research/04 §7 的胜出者，配显式 `str()` 边界 |
| 3 | 原始/多行字符串 `r#"..."#` | S1 | 收 | 收 | 收 | research/04 §7：一个构造覆盖原始与 heredoc，嵌套无歧义 |
| 4 | 后缀 `?` 传播 | S2 | 收 | 收 | 收 | docs/zh/design/03 E4、research/04 §11：做包含检查，没有隐藏转换 |
| 5 | `or` 提取 + `or return/break/continue` | S2 | 收 | 收 | 收 | research/04 §11（Phi）：取代 elvis、`??`、`unwrap_or`、`orelse` |
| 6 | 局部变量上的 `is` 收窄 | S2 | 收 | 收 | 收 | research/04 §3：杠杆最高的项，需要默认不可变（章程硬性要求 5） |
| 7 | `.err(h)` / `.ok(h)` 差集类型处理器 | S2 | 不收 | 收 | 收 | docs/zh/design/03 E4b。把老 README 的 `.err` 变得可组合（research/04 §11）。最小档用 `or` + match |
| 8 | match/if 表达式、穷尽并集、缺失集合诊断 | S2 | 收 | 收 | 收 | research/04 §3-§4：覆盖度就是集合算术，诊断就是一次集合运算 |
| 9 | 匹配守卫 `pat if cond` | S1 | 收 | 收 | 收 | research/04 §4：没有守卫的 Gleam 会嵌套（§6 的警示故事） |
| 10 | 或模式 `a \| b` | S1 | 不收 | 收 | 收 | research/04 §13：重复分支是最小档的 S0 替代 |
| 11 | 解构绑定 | S2 | 不收 | 收 | 收 | research/04 §2.5 保留集。最小档直接读字段 |
| 12 | 具名 + 仅字面量的默认参数 | S2 | 不收 | 收 | 收 | research/04 §6：覆盖约 90%，避开 typst#3052。Koka 的惰性默认值（S3）已排除 |
| 13 | 尾随 lambda（仅最后一个参数） | S2 | 不收 | 收 | 收 | research/04 §6：Kotlin 受限形式，代价是放弃 Koka 的上下文敏感花括号 thunk |
| 14 | UFCS `x.f(a)` = `f(x, a)` | S2 | 收 | 收 | 收 | research/04 §5：纯脱糖，三份生产证据，F3(a) |
| 15 | 类型别名 | S1 | 收 | 收 | 收 | research/04 §2.2 第 26 项：并集不给名字就没法读 |
| 16 | 和类型声明（`tag`/enum） | S2 | 收 | 收 | 收 | 由 docs/zh/design/01 B3 掌管。这里计入以保持全局预算诚实 |
| 17 | derive 属性（`#[derive(Show, Ord)]`，`==` 自动） | S2 | 不收 | 收 | 收 | 一个记号的表层。语义由 docs/zh/design/05 掌管，`==` 按 docs/zh/design/01 B9a 自动 |
| 18 | 区间 `..` `..=` + `in`（for/contains） | S1 | 收 | 收 | 收 | research/04 §13 区间选项 1，`in` 搭同一族记号 |
| 19 | 数值字面量默认类型 + 后缀 `1u8` + 分隔符 `1_000_000` | S1 | 不收 | 收 | 收 | docs/zh/design/01 B7a、research/04 §8 |
| 20 | 可变性划分 `const`/`var` | S1 | 收 | 收 | 收 | F2.a。`var` 的语义由 docs/zh/design/02 C3 掌管 |
| 21 | 运算符重载 trait（固定集合、固定优先级） | S2 | 不收 | 收 | 收 | research/04 §10：Rust 的槽位，F5(a) |
| 22 | 带标签的 break/continue（仅循环） | S1 | 不收 | 不收 | 收 | research/04 §2.5。适用范围仅限循环，Kotlin 那种通用的 `return@map` 标签被排除 |
| 23 | lambda 简写 `it` | S1 | 不收 | 不收 | 收 | research/04 §2.2 第 24 项，filter/map 的舒适度 |
| 24 | 首参管道 `\|>` | S1 | 不收 | 推后 | 收 | research/04 §5：与 UFCS 同样的脱糖。Roc 认为管道冗余而删除，见 F3(b) |
| 25 | 匿名记录字面量 | S2 | 不收 | 推后 | 收 | docs/zh/design/01 B3d 语法糖。等推断方案定下来再定 |
| 26 | `T?` 别名 | S2 | 不收 | 收 | 收 | docs/zh/design/01 B4c：`T \| none` 的零语义别名，随时可删 |

替代规则：把第 2 行换成 format 函数占位符风格（F6.c/d，research/04 §13 字符串选项 3）在单位上是中性的：S1 换 S1。

| 档位 | 特性数 | 单位数 | 相对 38 个特性的余量 | 相对 74-76 个单位的余量 |
|---|---|---|---|---|
| 最小 | 12 | 18 | 26 | 56-58 |
| 均衡（推荐） | 22 | 35 | 16 | 40-42 |
| 慷慨 | 26 | 40 | 12 | 34-36 |
| Kotlin 天花板（research/04 §2.2） | 38 | 74-76 | — | — |

均衡档落在 35 个单位，对照 research/04 §1 的「约 32」，比 20 个特性多出两个，因为守卫、或模式与 `.err`/`.ok` 按 research/04 §13 的计价方式各自单列一行。把任意两行合并（它们各自只占一段教程）就能拿回预算的字面数字。再对照一下被集合类型和其他目录整体删除的 Kotlin 特性：

| Kotlin 编号（research/04 §2.2） | 特性 | 命运 | 由什么取代 |
|---|---|---|---|
| 4 | 安全调用 `?.` | 排除出 v1 | `is` 收窄 + `or`（research/04 §11：只有真实代码有需求时才补一个薄的 `x?.f` 脱糖） |
| 5 | elvis `?:` | 删除 | `or`（research/04 §11） |
| 6 | 非空断言 `!!` | 删除 | `none` 类型 + `or`（docs/zh/design/01 B4） |
| 7 | 可空后缀 `T?` | 仅作别名（账本第 26 行） | 零语义别名（docs/zh/design/01 B4c） |
| 8 | 智能转换 | 作为独立特性删除 | `is` 流收窄（research/04 §3） |
| 12 | 运算符约定 | 取代 | 固定的 trait 集合，F5(a)（research/04 §10） |
| 13 | infix 函数 | 排除 | UFCS + 具名函数（research/04 §5） |
| 14、15 | 扩展函数 / 扩展属性 | 删除 | UFCS，F3(a) |
| 16、17、18 | 访问器属性、属性委托、类委托 | 排除 | 普通字段 + 函数 + derive，另加章程的可解释性预算 |
| 19 | data class 合成 | 取代 | derive（docs/zh/design/05）+ 结构化 `==`（docs/zh/design/01 B9a） |
| 21、22 | 默认参数 / 具名参数 | 受限 | 仅字面量的默认值 + 标签（账本第 12 行） |
| 25 | 接收者 lambda（DSL 作用域） | 排除 | Koka 的泛化花括号 thunk 是 S3，已排除（research/04 §6） |
| 27 | 密封层级 + 穷尽 when | 取代 | 封闭并集（docs/zh/design/01 B1/B3）。账本第 8 行 |
| 28 | object 表达式、伴生对象 | 排除 | 模块级绑定（模块由模块目录掌管，research/06） |
| 29 | 主构造函数 | 排除 | 普通 struct 字面量 + init 函数（docs/zh/design/01 B3d） |
| 30 | value/inline class | 推后 | docs/zh/design/01 掌管包装类型 |
| 31 | 型变语法 | 推后 | docs/zh/design/05 掌管泛型 |
| 32 | reified 类型参数 | 排除 | S3 且没有集合类型的回报（research/04 §2.2） |
| 33 | vararg + 展开 | 推后 | 附加 S1。标准库可以先不带它上线 |
| 34 | 标签 | 受限 | 仅循环，慷慨档（账本第 22 行） |
| 35 | 反引号标识符 | 排除 | 命名纪律比逃生口便宜 |
| 36 | 下划线占位符 | 免费 | 属于模式的一部分，账本里并进模式那一项（research/04 §13） |
| 37 | `tailrec` 修饰符 | 不必要 | TCO 是章程硬性要求（AGENTS.md 第 3 条） |
| 38 | 上下文参数 | 取代 | 能力参数，docs/zh/design/03 E1c |

交互：第 4-8 行是 docs/zh/design/03 E4b 的记号集合与 docs/zh/design/01 B1 的并集，删掉并集，第 5-8 行就失去主题。第 20 行的含义由 docs/zh/design/02 C3 定义。第 21 行的 trait 名字与第 17 行的 derive 列表由 docs/zh/design/05 命名。第 19 行是 docs/zh/design/01 B7a 的表层。推荐：采纳均衡档，它让 DreamScript 的特性数约为 Kotlin 的一半，成本加权表层约为 Kotlin 的 43%，同时覆盖 F1 到 F6 推荐所用的每一个构造。

## 一致的组合

三个命名的表层套餐，各自内部一致，且都在天花板之内。预算总数按该套餐的 F1 选择统计账本行数与单位数（F1.c 把账本第 1 行换成缩进 S2）。

### 组合 1 —「TS-heart」（推荐）：F1a + const/var + const 箭头函数 + UFCS + Rust 形状模式 + 固定 trait 运算符 + Kotlin 插值 + 均衡账本

```ts
// TS-heart: F1a + F2(const/var, const-arrow, : T + -> T, final-expression)
//           + F3a UFCS + F4a Rust-patterns + F5a fixed traits + F6a interpolation + Balanced (22 rows / 35 units)

type ParseError = BadNumber(String) | MissingField(String)
type IoError = NotFound | PermissionDenied

struct User {
  name: String,
  age: Int,
}

const parse_age = (s: String) -> Int | ParseError {
  match s.trim().parse_int() {
    e if e is ParseError => BadNumber(s),   // guard + is: the union story, no wrappers
    n                    => n,              // n : Int by set difference
  }
}

const parse_user = (row: Row) -> User | ParseError {
  const name = row.get("name") or return MissingField("name")
  const age  = row.get("age")  or return MissingField("age")
  User { name, age: parse_age(age)? }
}

const main = () -> () | IoError {
  const rows = load_rows("users.csv")?
  const cfg  = read_config("app.cfg").err((e) => {
    println("config failed: ${e}")
    default_config()
  })
  const (users, errors) = rows.map(parse_user).partition((r) => r is ParseError)

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

预算：均衡账本不变，22 行、35 个单位，余量 16 个特性 / 40-42 个单位。

### 组合 2 —「Gleam-simple」：F1b + fn/let 声明 + UFCS + 首参管道 + 仅 format 字符串 + 最小账本

```roc
// Gleam-simple: F1b + F2(fn, let/let mut, : T + -> T, final-expression)
//           + F3b UFCS + pipe + F4a patterns + F5b no overloads + F6d format-only
//           + Minimal (12 rows / 18 units; interpolation row swapped for the format-function row, S1 for S1)

type ParseError {
  BadNumber(String)
  MissingField(String)
}

fn parse_age(s: String) -> Int | ParseError {
  match s.trim().parse_int() {
    e if e is ParseError -> BadNumber(s)
    n -> n
  }
}

fn parse_user(row: Row) -> User | ParseError {
  let name = row.get("name") or return MissingField("name")
  let age = row.get("age") or return MissingField("age")
  User { name, age: parse_age(age)? }
}

fn main() -> () | IoError {
  let rows = load_rows("users.csv")?
  let (users, errors) = rows
    |> map(parse_user)
    |> partition(fn(r) r is ParseError)

  let total = users
    |> filter(fn(u) u.age >= 18)
    |> map(fn(u) u.age * 2)
    |> reduce(0, fn(a, b) a + b)

  for u in users {
    println(format("user {} age {}", [u.name, u.age.str()]))
  }

  match (total, errors.first()) {
    (_, e) if e is ParseError -> println(format("warning: {}", [e]))
    (0, _) -> println("no valid users")
    (t, _) -> println(format("total score: {}", [t]))
  }
}
```

预算：最小账本，第 2 行换成 format 函数占位符，12 行、18 个单位，余量 26 个特性 / 56-58 个单位，实现与讲解都最便宜的套餐。

### 组合 3 —「Indent-Koka」：F1c + fun/val + 全量 UFCS + Rust 形状模式 + 均衡账本

```koka
// Indent-Koka: F1c + F2(fun, val/var, : T both, final-expression)
//           + F3a full UFCS + F4a patterns + F5b no overloads + F6a interpolation
//           + Balanced with row 1 swapped for indentation S2 (22 rows / 35 units + tooling burden)

type ParseError
  BadNumber(string)
  MissingField(string)

struct user
  name : string
  age  : int

fun parse_age( s : string ) : int | ParseError
  match s.trim.parse_int
    e if e is ParseError -> BadNumber(s)
    n                    -> n

fun parse_user( row : Row ) : user | ParseError
  val name = row.get("name") or return MissingField("name")
  val age  = row.get("age")  or return MissingField("age")
  User(name, parse_age(age)?)

fun main() : () | IoError
  val rows  = load_rows("users.csv")?
  val parts = rows.map(parse_user).partition(fn(r) r is ParseError)
  val (users, errors) = parts

  val total = users
    .filter(fn(u) u.age >= 18)
    .map(fn(u) u.age * 2)
    .foldl(0, fn(a, b) a + b)

  for (u in users)
    println("user ${u.name} age ${u.age}")

  match (total, errors.first())
    (_, e) if e is ParseError -> println("warning: ${e}")
    (0, _)                    -> println("no valid users")
    (t, _)                    -> println("total score: $t")
```

预算：均衡档加缩进替换，22 行、35 个单位，另加未计价的有效空白感知工具负担（research/04 §6）。只有作者承诺投入 Koka 级别的编辑器与格式化器基础设施时才选它。

## 出处

本文件的所有证据、URL 与本地克隆引用都在 research/04 §14。上面的主张按章节引用 research/04。姊妹目录的约束来自 docs/zh/design/01-types.md（B1/B3/B4c/B7a/B8a/B9a）与 docs/zh/design/03-effects-errors-async.md（B5d/E1c/E4b）的推荐表。历史 README.md 仅作为先例，刻意保持不变。
