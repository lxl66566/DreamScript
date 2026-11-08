# DreamScript 工作设计决策（中文镜像）

状态：本文件是全部语言决策（42 个决策点：A1-A3、B1-B9、C1-C5、D1-D3、E1-E5、F1-F7、G1-G4、H1-H6）的权威工作记录中文版，由作者于 2026-11-08 定案，提取自 `design/` 冻结档案。Phase 4 实现工作以本文件为依据；`design/` 目录是只读档案，仅供交叉查阅 — 永不修改，除非本文档某条规则确实不够用，否则不必阅读它。研究笔记（`design/en/research/01-06`）是原始证据，实现不需要。两版内容分歧时以英文版 `docs/en/design-decisions.md` 为准。

来源映射（各节对应的档案文件）：§2 `design/en/design/08-decisions.md`；§3 `design/en/design/01-types.md`；§4 `design/en/design/02-linearity-ownership.md`；§5 `design/en/design/03-effects-errors-async.md`；§6 `design/en/design/04-syntax.md`；§7 `design/en/design/05-generics-metaprogramming.md`；§8 `design/en/design/06-modules-tooling.md`；§9 `design/en/design/07-runtime.md`；各决策点的详细理由在 `design/en/design/09-decision-rationale.md`；总工作表是 `design/en/design/00-decision-map.md`。

## 0. 作者补充（design/author_say.md）— 约束性

1. 主要方向遵循 Phase 3 决策记录（08）及其理由文档（09），已全部提取到本文档。
2. 大小写绝不能成为语法的定死特征：任何语法构造都不得用标识符大小写区分（例如类型不要求首字母大写）。类型作者倾向于小写拼写（`int`、`string`、`list`、`file`）。本文档与档案中的代码使用大写类型名，仅为目录示例；实现时统一归一为小写。大小写在语言任何位置都不承载语法含义。

## 1. 已定语言（总览）

DreamScript 是一门独立通用解释型语言（通过平台包实现嵌入），采用混合集合论类型 — 名义标签在并、交、差之下组合，`is` 收窄，`T?` 是 `T | none` 的别名 — 以双向检查加必需的边界签名与流收窄进行检查，具备带五个特例的通用结构化相等。值默认是仿射的，带可选的 `~Copyable` 线性资源层（`deinit`、幻影状态）、二等参数传递模式（借用 / `mut` / 消费）、独占窗口可变性和线性可变 COW 集合；内存是 Perceus 引用计数，带块复用，架在每帧 arena 之上，无 GC。多态是带两两不相交实例头的连贯集合论 trait，单一泛型函数体携带证据字与内联缓存；元编程是无宏的 comptime 反射。错误是返回类型中的联合成员，由 `?` / `or` / `.err` 处理，另有不可捕获的 trap；并发是强制 nursery 中的 VM 纤程，在线性/不可变的可发送性基底上做 fork-join 并行；效果隐藏在表层之下，运行时是证据传递的 handler 运行时加能力参数式 IO。表层是 TS 风格花括号加全量 UFCS、const 箭头函数、Rust 形状的模式匹配（带差集诊断）、Kotlin 插值，语法糖预算 22 特性 / 35 单位（Kotlin 上限是 38 / 74-76）。模块是 definition < file < package < module 分层，带生成的 `.dsi` 接口且无导入期执行；包是 blake3 哈希级联清单加内容寻址存储，带零配置脚本模式；FFI 是线性 C 句柄。执行是三层流水线 — 类型化寄存器 VM、copy-and-patch JIT、Cranelift/wasm — 并带规格保证的自递归与互递归尾调用。

## 2. 身份与兼容性

- **A1 — 独立内核，嵌入经由平台包。** 先建性能宪章要求的真正独立运行时；嵌入作为导出 effect handler 的普通包抵达同一运行时（默认平台就是 `ds` CLI 本身）。不为嵌入建第二套架构。
- **A2 — 目标工作负载。** CLI/工具/脚本为主，游戏/模拟为次（由解释器优先的启动、零配置脚本模式、双槽热重载服务）。数值/AI 通过 tier-3 Cranelift 路径保持可达，不塑造 v1。
- **A3 — 兼容性。** 1.0 之前快速破坏（无兼容承诺），之后实行 semver 纪律。内容寻址被否决为源码身份模型，在包级别采用（H6：blake3 哈希级联，清单即锁）。

## 3. 类型系统

### B1 — 名义标签之上的混合集合论类型

声明是名义的（tag 与 struct 有身份）；其上的类型语言是集合论的：名义原子上的并 `|`、交 `&`、差 `-` 是普通类型表达式。穷尽性按标签身份检查（enum-cover 难度）；差类型只存在于编译期并整体擦除；名义原子的联合是未装箱 tag+payload 布局；交集保留最精确成员的布局。

```ts
tag Timeout; tag NotFound              // 单元 tag：独立身份，值即 tag 本身
struct BadPort { got: int }            // 名义，携带载荷
type read_err = Timeout | NotFound | BadPort   // 名义原子上的集合表达式
fn load(p: path) -> config | read_err { ... }  // 联合承载的错误，无包装
const ok: config - read_err = load(p).err((e) => default())  // 差集收窄 handler 的结果类型
trait animal; struct dog: animal; struct cat: animal
const a: animal = dog{}                // 按声明名义子类型；`dog | cat` 按标签身份匹配
```

约束：名义原子上的不相交性/空性检查器是承重墙（它是 B6 实例连贯性的地基）；没有 `any` 兜底 — 推断失败是编译错误，绝不退化为动态类型。记录在案的后路：若不相交性机制拖期，退回纯名义内核（B1b），错误处理随之退化为基于 match 的形式（无差集类型 `.err`）。

### B2 — 双向检查 + 边界签名，加流收窄

顶层与公开签名必需，并在那里锚定集合代数；局部绑定、lambda 与 match 分支由双向传播推断。收窄经由 `is` 测试、none 测试与 match 分支单调进行；重置只发生在赋值处（在不可变默认下罕见）。`?` 传播检查是对着写下的返回联合做集合包含。已知限制：不能穿过联合类型的可变字段收窄。

```ts
const get_port = (j: json) -> int | missing | bad_port {
  const pv = j.fields.get("port")   // 推断：int | none
  if pv is int && pv >= 1 && pv <= 65535 { pv } else { return bad_port{ got: -1 } }
}
```

### B3 — 名义 struct/enum 加匿名元组

命名数据用名义 struct 与 enum（声明式布局，各一个 type_id，允许 tuple-enum）；临时聚合用匿名元组加解构；支持 record punning（字段名简写）。匿名 record 字面量（`const point = { x: 1, y: 2 }`）是编译器合成的糖，铺在一个名义 record 之上 — 推断故事稳定后作为增量特性再加入。row 类型对数据一律否决。

### B4 — `T?` 恰是 `T | none`

`none` 是若干单元类型之一；可选、错误联合、枚举共享一套机制（`is`、`or`、match）。v1 无 `?.` 运算符（只有真实代码提出需求时才可能后加一个薄脱糖）。`T | none` 的 niche 装箱（压进一个槽）是合法的后续优化器 pass（引用永不为零）。已知限制：`V | none` 中 `V` 本身可为 `none` 时有歧义 — 恰在那类位置用显式包装（`found[v] | none`）。

### B7 — 数值：i64/f64 默认，溢出陷阱

`int` = i64，`float` = f64，裸放在未装箱槽中。字面量：`1_000_000`、`0x`/`0b`/`0o`、宽度后缀（`42u8`、`2.0f32`）；无后缀整数为 int，无后缀浮点为 float。任何位置都无隐式窄化 — 转换是显式调用（`i32(u)`、`i64(f)`）。

| 表达式 | 行为 |
|---|---|
| `9223372036854775807 + 1` | 确定性陷阱（panic） |
| `a +% b`、`a -% b`、`a *% b`、`-%a` | 按 2^64 回绕（哈希/PRNG 用） |
| `1.0 / 0.0` | `+inf`（IEEE，浮点永不陷阱） |
| `1 / 0`、`int_min / -1` | 陷阱；返回错误联合的变体是库函数，类型为 `t | arith_error` |
| `a << k`、`a >> k` | `k` 超出位宽时陷阱；旋转在库中 |

`Dec`（精确十进制）与量纲单位是名义库类型加派生。

### B8 — 字符串：不可变 UTF-8，受检切片

不可变 UTF-8 字节，位于单个裸指针加字节长度之后；堆布局 `{ type_id, rc, byte_len, bytes }`。切片是不拥有所有权的借用，由父串的 RC 保活，且必须落在字符边界上 — 经由返回 `string | none` 的受检 API（`s.slice_char(0, 2)`）。`chars` 迭代 Unicode 标量；字素在标准库；相等是 O(n) 加长度快速否决；rope 是未来的库类型；编码转换发生在 FFI 边界。

### B9 — 相等：通用结构化 `==` 加五个特例

对所有数据的一套结构化 `==`，构造上全序（total），永不可被用户重载，不存在 `===`。五个特例：(1) 函数比较静态拒绝；(2) 浮点按位结构比较 — `nan == nan` 为真，字典键保持健全 — IEEE 语义走具名的 `is_float_eq` / `is_approx_eq`；(3) 可变单元与 `~Copyable` 资源按身份比较；(4) 操作数被借用，绝不被消费；(5) `hash` 必须与结构化相等一致。`ord` / `hash` / `show` 走派生；排序永远不能是结构化的。按类型的自定义值语义走具名方法（`equiv` 风格）。

## 4. 线性、所有权、内存、数据

### C1 — 仿射内核，可选 `~Copyable` 线性层

日常规则只有一句：对仿射值赋值即移动，被移动的绑定即死亡。普通可拷贝值从不可观察地撞上这条规则。`~Copyable` 减去默认的可拷贝性约束（一个被否定的集合约束，与 B1 联合自然组合）；它是 `deinit` 所在之处，并容纳幻影状态联合（`file[open] | file[closed]` 风格）。仿射检查是末次使用/DI 分析 — 过程内、单次前向遍历。

分阶段（后续步骤落地时语言零改动）：先发运行时 `rc==1` 唯一性检查加 Inko 式借用计数器作为诊断层；借用推断、完整复用配对与 TRMC 稍后作为纯优化抵达。

### C2 — 二等参数传递模式

三种模式固定在定义处 — 默认借用（不可变）、`mut`（独占可变）、消费（移动）— 调用点零模式语法：`xs.push(1)` 的读法与模式无关。借用永不逃出调用，所以检查是过程内的；不存在生命周期标注。一等引用永久出局。下标把同一组模式扩展到元素访问（get/mutate/set 访问器），取代 getter/setter 协议。

```ts
fn len(s: str) -> int { ... }              // 默认：借用
fn push(xs: mut list<int>, x: int) { ... } // 该调用的独占可变窗口
fn consume(f: file) -> int { ... }         // 消费式移动
```

### C3 — `const`/`var` 绑定，可变经由 `mut` 窗口

`var` 是可重绑定局部；被调方的可变性经由 `mut` 参数窗口进行 — 一套机制同时服务参数、窗口与圈禁（D1）。窗口内部零运行时检查（无别名是证明出来的）；窗口之外一切共享不可变。底层 FBIP 复用使函数式更新在值唯一时原地运行。没有面向用户的内部可变性 cell 类型（最多在标准库里藏一个 unsafe-cell 等价物，用于实现持久化结构）。

### C4 — Perceus 精确 RC + 块复用，加每帧 bump arena

装箱/共享值用精确非原子引用计数：每次显式 clone 处 `rc_inc`，每个唯一死亡点 `rc_dec`（在仿射检查下语法上可见）；每次 drop 串线到下一次分配作为复用候选（`kk_block_alloc_at` 模式；无候选时以 ~15-25 周期回退 mimalloc）。临时值用每帧 bump arena：分配是一次指针加法，回收是帧弹出时一次还原 — 健全，因为线性使逃逸成为编译期事实。零暂停、零写屏障、任何 tier 无 safepoint、无 GC map。环在 v0 于类型层否决；双钩子收集器接口（环候选注册、弱引用）现在就提交进头格式，作为增长路径。非原子计数在值线程封闭时健全 — E3 的 fork-join 所有权转移维持这一点。

### C5 — `deinit` 限定在 `~Copyable` 类型

`deinit` 恰好一次运行在该唯一实例的死亡处，覆盖一切路径，包括提前返回、panic 与取消销毁。deinit 派发是静态调用（具体类型已知）。定值初始化（DI）检查是所有方案共用的基础设施。显式 `drop` 仅作为调试/FFI 工具存在；用户从不写例行 drop。

```ts
struct file: ~Copyable {
  fd: raw_fd
  deinit { close(self.fd) }
}
fn with_file(p: str, body: fn(mut file) -> t) -> t {
  const f = open(p)     // 不能被拷贝或悄悄丢弃
  body(f)               // deinit 运行在 f 的唯一死亡处 — 包括 panic 路径
}
```

### D1 — 独占窗口是唯一可变通道

可变永远是持 `mut` 模式对拥有接收者的具名调用：静态独占、在调用处词法可见、窗口内部零运行时检查。共享可变性按构造不可表达。

### D2 — 线性可变 COW 集合

集合暴露函数式 API，且当且仅当唯一持有时原地修改 — 复用是被保证且可观察的。别名意味着拷贝，作为性能注记浮现，从不是错误。持久化 HAMT 风格结构是同一运行时上的普通库类型；`bytes`/`vec` 是同一模型中未装箱的唯一成员（FFI 级）。

### D3 — 组合子铺在隐藏线性游标上，编译器融合

用户表层是组合子（`map`/`filter`/`reduce`）加 `for`；隐藏协议是线性游标（边界检查可由线性消除）；编译器把链融合成循环；不可融合的形状回退到同一游标语义，行为统一。下标扩展原地元素访问。1.0 无公开生成器语法；隐藏下层可在日后采用纤程设施而无表层变化。

## 5. 错误、效果、异步、并行

### B5 — 错误是返回类型中的联合成员

错误是可存储的数据（nursery 收集、监督、缓存），VM 中一条良好预测的分支，与 effect row 信息完全等价。`throws(...)` 拼写保留为之后的糖，脱糖到同一联合。传播要求外层返回联合包含被传播成员（受检包含，无隐式转换 — Zig 规则，非 Rust 的 `From`）。

### E1 — v1 无效果语法，handler 形状的运行时，能力习语

运行时从第一天起就是 handler 形状的：证据传递服务两个内部效果（`cancel`、`discontinue`），仅在 nursery 边界 — 尾续内部操作不分配；用户代码路径不变。触碰世界的标准库 API 遵循能力习语：脚本由 `ds` CLI 安装一个环境默认 handler，测试、嵌入与调度器选择则用显式能力参数 — 同一份代码可跑在阻塞、事件循环或自定义调度器上。用户自定义 handler（`with handler { op ask() => resume(42) }`）保留为 v2 增量逃生口，永不需要迁移。v2 激活时的常设类型义务：续体除非标注否则是仿射的（多射续体不得捕获线性值）；FFI 调用钉住纤程（已裁决问题 1）。

### E2 — VM 纤程加强制 nursery

VM 池上的栈式堆纤程（纤程栈就是解释器数据；spawn 是一次分配；切换是指针交换，O(1)）。`spawn` 只存在于 `nursery` 块内；作用域退出即 join 子任务，首个失败取消其余兄弟，子任务错误在边界联合汇出。阻塞式代码，无颜色，无 `await`。抢占是派发循环每 N 条指令或回边处的一次标志检查。TCO 按纤程成立且不受影响（尾循环纤程永不增长其段）。裸 detach 需要显式根 nursery 或监督者。

```ts
nursery {
  for url in urls { spawn { pages.push(fetch_body(url)) } }
}   // 退出：已 join；首个失败已取消其余；错误已联合汇出
```

### E3 — 可发送性基底上的 fork-join 并行

无数据竞争与可发送性是线性/不可变数据的静态推论（线性值按构造即 `sending`；不可变值按构造可共享）；缺的只是调度器。`par_map` / `par_reduce` fork-join 组合子跑在不可变或线性分区的输入上 — 确定可重放、按定理无锁、静态 RC 计数使 fork 侧快照廉价。actor 与监督层可日后作为库加入（监督者就是一个带重启策略的 nursery）。

### E4 — 联合错误用 `?`/`or`/`.err` 处理，外加不可捕获的 trap

预期失败是联合成员，由三个令牌处理，其语义只在联合上陈述一次：`?`（受检包含的传播）、`or`（提取第一个想要的成员，右操作数惰性求值，带 `or return` / `or break` / `or continue` 提前退出形式）、`.err(h)` / `.ok(h)`（差集类型的 handler — 余留类型直接展示还剩什么）。bug 一律 `panic` 且不可捕获：纤程中止，帧遍历运行线性 deinit，trap 在 nursery 或监督者边界浮现。let-it-crash 监督是库模式。

```ts
fn load_config(path: str) -> config | io_error | parse_error {
  const text = read_file(path)?        // `?`：ok 类型之外的成员提前返回
  parse_config(text)?
}
const cfg  = load_config("app.cfg") or default_config()
const cfg2 = load_config("app.cfg").err((e) => {   // 结果类型：(config | io_error | parse_error) - (io_error | parse_error) == config
  log("config failed: ${e}")
  default_config()
})
const port = cfg2.port or 8080                     // 可选就是 t | none：同样三个令牌
```

### E5 — nursery 范围的协作取消加消费即关闭

取消是 nursery 范围且协作式的，只在 await/抢占点被观察（一次标志检查），因此线性消费永不在飞行中途被打断；销毁遍历帧运行 deinit。资源靠消费关闭（一次静态移动；语言内句柄按构造无泄漏）。`defer` 作为作用域退出糖存在，非规范，且在取消下于销毁时运行（Koka 的 `finally` 语义）。超时、`firstof`、`interleave` 是计时器能力上的库组合子 — 没有任何超时形状的东西是语言原语。

## 6. 语法表层与语法糖预算

### F1 — TS 风格花括号

每个块以可见令牌开启；面向行的文法，无布局状态；换行终止语句，无分号，处处允许尾逗号；`if` 与 `match` 是表达式。记录在案的后路：若原型显示箭头赋值文法与解析器相搏，退回 Gleam/Roc 风格花括号（`fn name(` 标题行）。

### F2 — 绑定、函数、标注、返回

`const` / `var` 绑定（var 是可重绑定局部）。具名函数用 const 箭头声明，与绑定同形；定义名在自己体内可见（递归）。匿名 lambda `(x) => e`。参数 `name: t`，返回 `-> t`；联合返回解析为 `-> a | b`，无额外语法；`T?` 在一切标注位置合法。末表达式返回；提前退出走 `or return` / `or break` / `or continue`。`?` 保留给后缀传播；旧 README 的前缀 `?` 返回记号已退役。裸 `return` 关键字日后可作为纯增量加入。函数是值。

```ts
const parse_age = (s: string) -> int | parse_error {
  match s.trim().parse_int() {
    e if e is parse_error => bad_number(s),
    n                     => n,
  }
}
const parse_user = (row: row_t) -> user | parse_error {
  const name = row.get("name") or return missing_field("name")
  user { name, age: parse_age(row.get("age") or return missing_field("age"))? }
}
```

### F3 — 全量 UFCS

`x.f(a)` 对任何首参函数脱糖为 `f(x, a)`；零运行时成本，无候选搜索。解析固定：自由函数优先，其次 trait 方法，`Trait::f(x)` 限定作为逃生口；真歧义是编译错误，永不是优先级搜索。能力参数具名且被排除出点号位置 — 点调用喂给第一个非能力数据参数，因此 `println(io, msg)` 永远写不成 `msg.println()` 而藏起 `io`；能力显式传递或经作用域形式。`|>` 是延迟增量（Roc 因其与链式调用加 `?` 冗余而删除了管道）。

### F4 — Rust 形状的模式匹配

分支 `pat => expr` 以逗号分隔；守卫 `pat if cond`；或模式 `a | b`；解构绑定；`_` 通配；穷尽匹配带差集诊断 — 错误信息把缺失成员按集合计算报出（"missing: NotFound | Locked"）。`@` 绑定器 v1 排除（`is` 收窄保持 scrutinee 变量可用；延迟增量，若被需求则需设计仿射重绑定语义）。

### F5 — 固定 trait 驱动运算符集，冻结优先级

固定令牌集映射到固定 trait（Add、Sub、Mul、…、Index）— 运算符调用就是 trait 方法调用，可内联缓存。一张冻结的 C 家族优先级表在教程中发布一次，永不扩展，永无用户自定义运算符；`Deref` 式强制转换被排除出用户表层。`==` 是 B9 的结构化相等且永不可重载；排序走 `ord` 派生。

| 运算 | 陷阱（默认） | 回绕（显式） | 备注 |
|---|---|---|---|
| 加 / 减 / 乘 | `a + b`、`a - b`、`a * b` | `a +% b`、`a -% b`、`a *% b` | trap panic 带位置 |
| 取负 | `-a` | `-%a` | int-min 边界 |
| 除 / 余 | `a / b`、`a % b` | 无 | 除零与 int-min/-1 永远陷阱 |
| 移位 | `a << k`、`a >> k` | 无 | `k` 超位宽时陷阱 |
| 浮点 | IEEE 语义，永不陷阱 | 无 | inf/nan 按 IEEE |

### F6 — 字符串与字面量

插值 `$x`（裸标识符）与 `${expr}`（表达式），在非字符串边界要求显式 `str()` 调用 — 无隐藏字符串化，`T?` 值的插值要求显式处理。原始字符串 `r"..."` 与按哈希计数的 `r#"..."#`（可嵌套到任意深度，一个构造同时服务 raw/多行/heredoc）。`'x'` 是 Unicode 标量 char；`b'x'` 是单字节。

### F7 — 语法糖预算：Balanced 档（22 特性 / 35 单位）

对照 Kotlin 的 38 特性 / 74-76 成本单位上限；预留余量：16 特性 / 40-42 单位。包含行：换行终止语句（S1）；插值 `$x`/`${e}`（S1）；原始字符串 `r#"..."#`（S1）；后缀 `?`（S2）；`or` 提取 + `or return/break/continue`（S2）；`is` 收窄（S2）；`.err(h)`/`.ok(h)` 差集类型 handler（S2）；match/if 表达式带穷尽联合诊断（S2）；match 守卫（S1）；或模式（S1）；解构（S2）；具名 + 仅字面量默认参数（S2）；尾 lambda，仅限末参（S2）；UFCS（S2）；类型别名（S1）；和类型声明 `tag`/enum（S2）；派生属性 `#[derive(...)]`，`==` 自动（S2）；范围 `..`/`..=` + `in`（S1）；数值后缀 + 分隔符（S1）；`const`/`var` 之分（S1）；运算符重载 trait，固定集合与优先级（S2）；`T?` 别名（S2）。延迟增量行（按余量使用可能性排序）：`|>`（S1）、匿名 record 字面量（S2）、`@` 绑定器（S1）、vararg/spread（S1）、带标签 break/continue（S1）、lambda 简写 `it`（S1）。被集合类型与 UFCS 整体删除的 Kotlin 特性：`?.`、elvis、`!!`（由 `is`/`or` + none 类型取代）、smart cast（`is` 收窄）、中缀函数与扩展函数/属性（UFCS）、data class 合成（派生 + 结构化 `==`）、sealed 层级（封闭联合）、接收者 lambda DSL、主构造器、reified 类型参数、反引号标识符、`tailrec`（TCO 已保证）、context 参数（能力参数）。永久排除出用户表层：自由运算符重载与自定义运算符、Deref 强制转换、基于约定的私有性。

## 7. 多态与元编程

### B6 — 带两两不相交实例头的连贯集合论 trait

trait 可携带关联类型；实例为类型头声明；唯一连贯性规则是同一 trait 各头两两不相交 — 对每一对 `H1`、`H2`，证明 `H1 & H2 = Empty`（可判定的集合空性），在声明时检查，并在链接时对最终实例环境再次检查。任何模块可为任何类型定义实例，外来类型亦然 — 无孤儿规则，无 newtype 包装。优先级机制永不存在；解析是经由 modus ponens 的交集子类型。重叠错误必须指名两个头及交集中的见证值类型；满足自身所需的条件实例被解析步骤上的结构环检查拒绝（链接时检查，终止性要求已固定；算法实现自定）。联合头规范化：`impl eq for int | string` 在不相交检查前分解为逐分支实例。因为头两两不相交，按标签键控的内联缓存永不需要优先级裁决。作用域具名实例（显式字典值逃生口）保留为延迟增量，歧义永远是错误。

```ts
trait Ord { fn compare(self: &Self, other: &Self) -> int }
impl Ord for int { ... }
impl[T: Ord] Ord for list[T] { ... }        // 条件头
impl Draw for Circle { ... }
impl Draw for Circle | Square { ... }       // 拒绝：与两个头相交；见证：Circle
impl Draw for Circle & ~Shadowed { ... }    // 需要刻意重叠时，用否定雕刻排他性
```

### G1 — comptime 求值 + 静态反射，无宏

元语言就是语言本身：普通代码的 comptime 求值加静态类型反射（类型的字段/case 清单，编译期迭代与分支）；一切层级的宏都被否决 — 不存在替换，卫生问题按构造消失。分阶段交付：内建派生先行 — `hash`、`show`、`ord`（按字段序字典序；排序键需要显式提供 `total_compare` 浮点变体）、`clone`（深拷贝）、`to_json`/`from_json`、`default`；`==` 是通用的且永不派生。comptime 求值器与反射 API 在 1.0 前落地。

### G2 — 仅编译期反射，铺在 tag 即数据之上

反射只存在于 comptime；运行时类型问题由数据已携带的 tag 回答（`is`、match — tag 测试就是类型测试）。任何位置都不存在类型对象与运行时元数据表。存在类型仍以证据携带盒的形式可用（`box Json` — 值加字典），服务异构集合与插件边界，这是唯一正当的装箱点；签名默认用全称类型，因为存在类型装箱并破坏仿射所有权推理。

### H3 — 单一泛型函数体、证据字、内联缓存、惰性 JIT 特化

每个泛型函数在字节码中恰好表示一次，附带约束清单。泛型调用点发射 `CALL_GENERIC fn, evidence_words`；每个证据字要么是静态解析的实例引用（常量操作数 — 常见情形），要么是按运行时 tag 键控的运行时解析槽，首次执行后补丁为 1-2 项内联缓存；缓存未命中回退完整查找，正确性从不依赖缓存状态。JIT tier 惰性物化按具体类型向量键控的逐类型特化，缓存且无正确性角色。裁决（反特化不变量，原文）："dispatch is determined by types and the set algebra, never by declaration order or priority; the runtime may cache and specialize any dispatch that is already unique." 证据字是普通寄存器实参，故尾位置的泛型调用正常尾调用；证据字本身可以是仿射值。方差由集合包含得出：不可变容器协变；读角色协变、写角色逆变、兼具两种角色的字段不变。1.0 无 HKT、无 GAT（具体方法覆盖经典驱动场景；仅当表层 effect row 日后毕业时重议）。

## 8. 模块、可见性、FFI、生态

### G3 — definition < file < package < module；无导入期执行

四层：definition（可见性与身份单位）、file（命名空间拼写）、package/目录（编译单位 — 同一包内文件共享一个命名空间，包内零 import）、module（`ds.pkg` 清单：依赖与分发单位）。包内环合法（整包不动点解析）；跨包环是链接错误，其诊断建议把共享抽象移入基础包。生成的 `.dsi` 接口文件是可评审的 API 表面（`ds info` 生成；".dsi 无变化通常即安全重构"）。模块体是纯编译期声明 — 无导入期执行，无一例外；这一条规则同时买到包内环容忍、安全热重载与惰性依赖驱动加载。`///|` 是可选的显式块分隔符，给工具亚文件粒度；无论有无分隔符，顶层定义都与顺序无关，无分隔符的文件就是一个块。

**G3.5 — dsmod 字节码契约（约束性）。** 定宽、位置无关、可 mmap 的模块格式：

```text
bytes 0..3  magic "\0DSM"       byte 4 format version     byte 5 target class (bytecode | wasm | triple)
byte 6      flags (STRIP | FFI-USED | HAS-JIT-METADATA)
uleb        module name         uleb deps: [(name, content-hash)]...
uleb        source hash (blake3-128)   # PEP 552 checked-hash 校验，永不用 mtime
sections    blocks | constants | type metadata | effect rows | debug spans (可剥离)
```

缓存在 `$DS_CACHE/ir/<triple>/<blake3-of-source+deps>.dsmod`；命中路径 mmap；由头部依赖表驱动惰性加载；`ds build --exe` 产出嵌依赖闭包 + VM 的单体二进制（Bun 实测 `--compile --bytecode` 带来 1.38x 启动增益，此为下限）。启动目标：经 mmap 存储的冷启动 sub-10ms。

### G4 — private / `pub(pkg)` / `pub` 加选择性导入

definition 默认私有；`pub(pkg)` 对整包开放（含包自身的测试）且永不进入 `.dsi`；`pub` 对依赖方开放并进入 `.dsi`。导入按符号选择、可别名、可按目标条件化，并可用 `pub import` 再导出：

```text
import acme/pipeline/schema (Row, parse_row)
import acme/pipeline/io as pio
import acme/pipeline/fs if os == "windows"
pub import acme/pipeline/api (parse_row)
```

`.dsi` 是可见性闸门、API 评审工件、LSP hover 来源与增量编译边界。路径作用域的 `pub(in ...)` 阶梯被刻意排除以守住语法糖上限。

### H5 — 线性 C-ABI 句柄为基底，平台包叠加

外来资源是不透明线性句柄（`~Copyable`）：双重释放是编译错误，extern 借用是类型层禁止逃出调用的调用作用域裸指针，回调运行在 VM 托管栈上，`ds bind header.h` 生成带类型的 extern。裸 `extern` 声明保留为句柄纪律内文档化的逃生口（它们正是 `ds bind` 在句柄包装之前的输出形态）。嵌入 API 是 Lua 形状的句柄表（`ds_push` / `ds_ref`）；`Value` 是 VM 对象表的索引；上下文是显式 `ctx: &mut Vm`；两个嵌入者即两个独立 VM（按设计无 GIL）。FFI 钉住策略（已裁决）：RC/arena 堆从不移动对象，钉住只是生命周期问题 — 拥有的外来资源是 `~Copyable` 句柄，其 deinit 恰好释放一次；外来回调执行在 VM 托管栈；阻塞式外来调用钉住其纤程，阻塞 C 调用必须与调度器重叠时以 carrier 线程执行为逃生口；取消只在 VM 重入处被观察，因此飞行中的线性消费永不被打断。

叠加于上：平台是导出 effect handler 的普通包，按构建目标选择 — `platform "std" provides <console, fsys, net, time>`（`ds` CLI 即默认平台）、`platform "web"`、`platform "embed"`（宿主调入，不提供任何东西）；检查器验证用户代码的效果用法是平台声明 row 的子集。wasm/WIT 是 tier-3 部署目标，不是 FFI 架构。dsmod 的 FFI-USED 标志声明加载时需要 dlopen。

### H6 — 哈希存储内核加零配置脚本模式；五项强制工具链承诺

**包栈。** `ds.pkg` 清单以 `name = { ver, hash }` 声明依赖，hash 是对该包内容（含其依赖哈希）的 blake3 — 身份传递式流动，不存在 lockfile（清单即锁）。MVS 解析（取声明最小值的最大者；多版本共存按构造成立）。按输出哈希键控的全局内容寻址存储，兼作字节码缓存（工件即 dsmod 文件）。按 target triple 的 bin-first 工件，对照存储哈希校验。可选 FHS profile 链接器（`ds link`；Windows 用 scoop shim，不用符号链接）。registry 延迟到存储上的静态 JSON 索引，直至生态规模需要。

```text
name    = "pipeline"
version = "0.4.0"
[deps]
std     = { ver = "1.2", hash = "blake3:9f2c..." }
csv     = { ver = "0.7", hash = "blake3:1a88..." }
[artifacts]
x86_64-pc-windows-msvc = { url = ".../pipeline-0.4.0-x86_64-pc-windows-msvc.dsmod", hash = "blake3:77e0..." }
[build]                     # 可选逃生口，由内嵌 VM 运行
script = "build.ds"         # 可见 #OUT#、#TMP#、#ARCH#
```

**脚本模式。** `ds run script.ds` 零配置；裸限定符标准库导入（`import "@std/csv"`）；`ds save` 把脚本用过的东西冻结进生成的 `ds.pkg`。长成包的脚本获得清单而不丢失文件。

**五项强制工具链承诺，自第一天约束。** (1) 编译器即查询库：编译器是记忆化查询的库（`ds-check`）— parse、block_index、resolve、type_of、effects_of、linearity_check、lower — 由 LSP、REPL 与热重载共同消费；失效粒度为 `///|` 块加包粒（包内环被禁的第二个理由）。(2) `ds fmt` 零选项，实现为编译器组件；块感知格式化器独立格式化各块，兼作 LSP 的输入时格式化路径。(3) 结构化诊断加双跨度线性错误 — 机器可读建议、`--error-format=json`、跨度在类型化 AST 层计算；线性诊断类别渲染"此处被移动 / 此处再次使用"成对输出：

```text
error[DS2001]: value `h` was consumed here
  --> src/pipeline.ds:41:15
   |  41 |  let rows = Csv.read(h)     ^ `h` moved (file is ~Copyable)
   |  42 |  Log.write(h.path)          used again here after move
help: clone the path before the move            (suggestion emitted as JSON too)
```

(4) 线性感知 REPL：打印推断类型与 effect row；跑在同一字节码 IR 上；会话持有的线性上下文配显式 `:drop` 与 `:reset` 清除 — 提示符处的线性值永不被悄悄丢弃。(5) Erlang 双版本代码槽热重载：运行时持有逐模块版本槽（current + old，至多两个存活）；`ds dev --hot` 重编译变更块并换槽；旧版本帧在其下一次跨模块调用处退役（DreamScript 一切跨模块调用都全限定，Erlang 的要求被平凡满足）；堆值对新定义做类型检查；effect handler 捕获其安装时的代码版本。无导入期执行正是其健全性来源。双槽运行时必须从第一天存在 — 后补是被点名的错误。

## 9. 运行时

### 固定基线（BL-1..BL-7，对每个 tier 约束）

- **BL-1 类型化寄存器字节码。** 32 位定宽指令、寄存器索引操作数、按类型特化的 opcode（ADDI64/ADDF64、EQI64/EQF64、KIMM/KCONST）；无动态 "V" 变体，因为每个操作数的类型都是编译期事实。照抄 LuaJIT 的偏置跳转偏移（BCBIAS_J）与 XOR 取反比较对（ISEQV^1 == ISNEV）。
- **BL-2 未装箱槽。** 无 NaN-boxing、无逐槽 tag、无装箱标量：i32/i64/f64/bool 裸放帧槽，引用为裸指针，ADT/联合/Result 是横跨连续槽的未装箱 tag+payload 对。装箱正是静态类型要删除的 2-10x 标量开销项。
- **BL-3 派发。** Computed goto 或 C 尾调用穿线（`musttail` + `preserve_none`）— 这是构建可移植性选择，不是设计点。
- **BL-4 按画像数据组装超指令。** 汇编器从实测 opcode 频率组装融合（比较-分支、load-op、稠密 tag match 跳转表），从不手工挑选。
- **BL-5 自第一天起按 uop 分解。** 指令集按微操作定义，带显式槽效应与 `pure` 注记；copy-and-patch stencil 集按构造与这些 uop 一一对应。
- **BL-6 帧内联于类型化值栈。** 每帧是静态类型槽的连续跨度加一个元数据字（返回地址、帧大小、TCO 链标志）。静态布局正是使 TCO 成为指针洗牌、stencil ABI 平凡的原因。
- **BL-7 位置无关、可 mmap 的 dsmod。** 定宽、基于索引、免重定位的字节码，带模块头（版本、类型/常量表、逐函数帧槽类型、帧大小、最大调用深度）与 IR 版本校验和；契约详见 G3.5。

### H1 — 共享单一 lower 的三层流水线

Tier 1：类型化寄存器 VM，computed-goto uop 循环，未装箱帧 — 目标 3-10x of C。Tier 2：copy-and-patch stencil — 每个 uop 一段 x86-64 片段，构建期由 LLVM 从含 VM 头的主模板发射，洞为帧槽偏移与常量，按 u16/u64 补丁；比 LLVM -O0 快约 14% 且编译速度约 100 倍；目标 1.5-3x；每单位微秒级。Tier 3：字节码 → 类型化 SSA → Cranelift CLIF，或 wasm 3.0（`return_call` 携带 TCO）；目标 1-2x 原生 / 1.5-2.5x wasm。一套 uop 与一条字节码到 SSA 的 lower 被所有 tier 共享，尽早建成。tier 迁移是原地 J 变体 opcode 重写（一次派发）；OSR 化归为编译循环体 — 无帧变换机制。任何数据路径无守卫；仅有的出口是 (i) 调试器断点（从 JIT 元数据物化帧）、(ii) 逐函数入口的栈上限比较、(iii) effect handler 进入（一次控制转移）、(iv) B7 的冷溢出边 — 因此没有 deopt 悬崖。短脚本（<0.2s）从不支付分层代价且仍达 3-10x。无验证 stencil 构建的平台只发 tier1（CPython 的 opt-in 姿态）。记录在案的后路：若逐架构 stencil 工具链被证不可接受，退回单一 method-JIT tier（H1b）。

```text
; stencil ADDI64_rr，洞以 <H> 标记；无类型检查、无 RC 调用、无错误胶水
mov  rax, [r14 + <H off_r1>]
add  rax, [r14 + <H off_r0>]
mov  [r14 + <H off_r1>], rax
```

### H2 — 自第一天起保证 TCO，自递归与互递归皆然

规格级保证：tier 1 帧复用加实参滑动（Lua 的 `luaD_pretailcall`/`CIST_TAIL` 纪律），JIT tier 用标签循环（Koka `kk__tailcall`）与槽位重叠 `jmp`，wasm 用 `return_call`。槽位重叠 ABI 在帧布局时锁定：被调方实参槽覆盖调用方出参区，尾调用至多是一次 O(delta) 实参滑动加一条 jmp；任何深度、任何 tier、确定性地下栈永不增长。回溯经尾链元数据保持诚实，仅当调试器索取时惰性物化被省略的帧。保证限定于同一 effect 上下文内的尾调用；handler 进入物化一帧。跨 tier 尾调用经双字被调入口描述子 `{tier1_prologue_ptr, tier2_code_ptr}` — 被调方日后编译时翻转一个描述子字（与热重载同一失效故事）。按纤程保证成立：尾循环的已 spawn 纤程永不增长其段；纤程段增长（深非尾递归）是 chunk-list 追加，不是 realloc，被捕获的帧地址因此保持稳定。移动实参的尾调用以零 RC 流量转移所有权。此 ABI 与 BL-7 是要先于一切锁定的两个决策。

```text
; fn even(n: i64) -> Bool { if n == 0 { true } else { odd(n - 1) } }   ; 互递归
even:
  0: EQI64   r1, r0, 0
  1: JMPIF   r1 -> 3
  2: SUBI64  r3, r0, 1
  3: CALLT   odd, nargs=1, src=r3    ; r3 滑入 slot0，复用帧，重载前奏
  4: RET     true
```

### H4 — Perceus RC + 复用 + 每帧 arena，收集器接口作为增长路径

非逃逸临时值用每帧 bump arena（入口 mark/save，bump 分配，每条弹出路径 restore；段增长按 chunk）。装箱/共享值用 Perceus 精确非原子 RC 带复用串线（`rc_dec(tmp)` 唯一死亡 → 复用候选 → `alloc_at(reuse, ...)`）。零暂停、零屏障、任何 tier 无 safepoint 或 GC map；确定性释放喂给确定性 deinit。arena 标记按纤程持有，纤程切换无需 arena 动作。诚实的账目：100% 的非移动临时值驻留寄存器/arena；装箱/共享残余持续支付 RC；复用 pass 按优化器工作做预算，带 mimalloc 回退。H4(d) 双钩子收集器接口（环候选注册、弱引用）现在就提交进头格式，作为共享环形工作负载的文档化增长路径（线程外试验删除；永不适用于线性数据，线性数据不成环）。

### 静态类型从 JIT 中删除了什么

无 hidden class/map 及其迁移（字段偏移是字节码立即数，`FIELD_LOAD %dst, %obj, #12`）；无字段访问内联缓存或反馈向量；数据路径无类型守卫与 deopt 帧；无投机记录；无侧 trace 与 trace 链接；分层预算化归为普通热度计数器。这将约六个投机子系统折叠为两个工件 — 一个 stencil 补丁器与一条 lower pass — 释放动态 VM 分层风险约 70%。以内联缓存形式残存者，均为纯性能提示、无正确性角色：动态/trait 调用点的 1-2 项按 type-id 键控 IC、按模块版本计数器键控的热重载模块全局缓存、逐帧 effect handler 操作表。

## 10. 已裁决的开放问题

1. **FFI 钉住** — 见 H5。
2. **nursery 形状与令牌集** — 块形式 `nursery { ... }` 内含 `spawn { ... }` 子表达式；令牌定案：后缀 `?`（受检包含，无隐式转换）、`or`（首个被wanted成员提取，右操作数惰性，`or return`/`or break`/`or continue`）、`is`（收窄）、`.err(h)`/`.ok(h)`（差集类型）。v1 无 `?.`。
3. **`.err` 差集类型后路** — 失效：B1(c) 已采纳，差集类型按设计成立。
4. **纤程与隐藏生成器** — v0 隐藏迭代协议是不涉纤程的线性游标；纤程存在后隐藏下层可采用同一设施。允许并分阶段安排共享的 VM 机制；两种情况下用户表层完全相同。
5. **生成器箭头 `-*>'`（历史 README）** — 退役；1.0 无公开生成器语法。
6. **作用域具名实例** — 延迟增量，1.0 之后；除非不相交头解析被证明确实妨碍库作者才提前；任何未来糖中歧义仍是错误。
7. **不相交算法与终止性** — 实现自定义，要求固定：链接时对最终实例环境检查；重叠错误指名两个头与见证值类型；满足自身的条件实例被结构环检查拒绝。
8. **浮点相等** — 按位结构（自反、全序、字典安全）；IEEE 语义走具名 `is_float_eq` / `is_approx_eq`。
9. **comptime 在 v1** — 分阶段：内建派生先行，comptime 求值器加反射在 1.0 前。
10. **match 的 `@` 绑定器** — v1 排除；延迟增量，若被需求则需设计仿射重绑定语义。

## 11. 分阶段（v0 → 1.0）

有序；每步落在前一步上，无语言改动的重写。

1. 先锁定两个承重契约：槽位重叠调用 ABI（H2）与定宽可 mmap 的 dsmod 格式（BL-7、G3.5）。下游一切编译都以它们为前提。
2. 检查器内核：B1(c) 名义 tag 上的集合代数、B2 双向加收窄、B3(d) 积/和、B4(c)、B7(a)、B8(a)、B9(a)；仿射检查器加 DI（C1b、C5c）。
3. Tier-1 解释器：computed-goto 或尾调用穿线的 uop 循环、未装箱槽、tag+payload ADT、CALLT 帧复用 TCO。
4. 内存：Perceus dup/drop 放置、复用串线、每帧 arena；运行时 `rc==1` 检查与借用计数器作为诊断层。
5. 并发：纤程、nursery、内部 `cancel`/`discontinue` 证据派发、fork-join 组合子。
6. 派生；然后 comptime 求值器与反射 API（1.0 前）。
7. 自第一天起兑现工具链承诺，与它们消费的查询编译器一同排程：`ds fmt`、结构化诊断、REPL、双槽热重载。
8. Tier 2 copy-and-patch（x86-64，然后 aarch64；其余平台仅 tier1）、尽早建字节码到 SSA lower、tier 3 按需。
9. 核心稳定后的延迟增量清单（见 §12）；借用推断、TRMC、融合升级作为零语言改动的纯优化落地。

## 12. 延迟增量清单与记录在案的后路

延迟增量（按余量使用可能性排序）：`|>`、匿名 record 字面量、`@` 绑定器、vararg/spread；随后是 `?.` 脱糖、`throws(...)` 糖、用户自定义 handler（`with handler`）、作用域具名实例、裸 `return` 关键字、带标签 break/continue、`it` 简写。纯优化项（零语言改动）：借用推断、完整复用配对、TRMC、融合升级、`T | none` niche 装箱、rope。记录在案的后路（命名风险成真时）：B1(b) 纯名义内核（若不相交机制拖期；错误处理退化为基于 match）；F1(b) Gleam/Roc 花括号标题行（若箭头赋值文法与解析器相搏）；H1(b) 单一 method-JIT tier（若逐架构 stencil 工具链被证不可接受）；C4(b) 分代引用（若仿射摩擦比预期更糟）。

## 13. 种子测试语料

按决策协议，所选 combo 的示例程序是语言的第一批测试语料，按目录顺序排列。移植说明：以下程序逐字引自档案；目录 01/02/03/05/06 早于 04 的表层决策，把它们变成测试时应移植到已定表层（const 箭头函数、`=>` 分支、已定令牌），并按 §0 将类型名归一为小写。

### 13.1 类型 — "Hybrid"（配置解析器：tag、联合、收窄、错误集、结构化 ==）

```ts
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

### 13.2 线性 — "Modes and RC"（带 deinit 的线性 File、mut 窗口、COW、融合链）

```ts
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

### 13.3 效果 — "Effect-ready"（cancel-on-failure 的 nursery fetchAll、联合错误、par 组合子）

```ts
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

// v2, additive with no migration — user handlers for final/tail-resumptive effects only:
with handler { op ask() => resume(42) }
```

### 13.4 语法 — "TS-heart"（完整表层程序：守卫、`or return`、`?`、`.err`、插值、解构）

```ts
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

### 13.5 泛型 — "Coherent-traits"（`~Copyable` 泛型栈、不相交头拒绝、派生 + comptime schema）

```ts
// 1. polymorphic container API over affine elements
type Stack[T] = { items: List[T] }
impl[T: ~Copyable] Stack[T] {
    fn push(&mut self, x: T) { self.items = cons(x, self.items) }  // consume-and-return
    fn pop(&mut self) -> Option[T] { ... }
}

// 2. trait with disjoint instance heads
trait Json { fn to_json(&self) -> String }
impl Json for Int  { fn to_json(&self) -> String { ... } }
impl Json for Bool { fn to_json(&self) -> String { ... } }
impl[T: Json] Json for List[T] {                     // conditional head: an intersection arrow
    fn to_json(&self) -> String { "[" + self.map(|x| x.to_json()).join(",") + "]" }
}
// impl Json for Int | Bool { ... }                  // REJECTED: head intersects both above

// 3. derive + comptime reflection
@derive(Hash, Show, ToJson)                          // one word, no macro system
type Point = { x: Int, y: Int }
fn schema(comptime T: type) -> String {              // user derive-like via reflection
    var s = "{"
    for f in fields(T) { s += f.name + ": " + type_name(f.type) + ", " }
    return s + "}"
}

// 4. execution: one body per generic fn + evidence words
let p  = max(p1, p2)            // evidence: static ref to impl Ord[Point] — constant operand
let js = p.to_json()            // dynamic site: IC keyed by tag -> the unique impl
```

### 13.6 模块 — "Hash-core"（ds.pkg 清单、选择性导入、线性 sqlite 句柄绑定）

```text
// ds.pkg -- manifest is the lock; no lockfile anywhere
name = "pipeline";  version = "0.4.0"
[deps]  std = { ver = "1.2", hash = "blake3:9f2c..." }  csv = { ver = "0.7", hash = "blake3:1a88..." }
[artifacts]  x86_64-pc-windows-msvc = { url = ".../pipeline.dsmod", hash = "blake3:77e0..." }

// pipeline.ds -- imports (selective, aliased, conditional) over a package-local cycle
import @std/csv (Reader)                       // namespace alias, MoonBit-style
import acme/pipeline/schema (Row, parse_row)   // selective, from the generated .dsi surface
import acme/pipeline/fs if os == "windows"

///|  // FFI: linear handle, consumed exactly once
extern fn sqlite3_open(path: *const c.Char) -> (c["sqlite3"] | c.Int)
extern fn sqlite3_close(db: c["sqlite3"]) -> c.Int      // double-close: compile error
```

### 13.7 运行时 — `sum_to` uop 编码与互递归 `even`/`odd` 尾调用编码

```text
fn sum_to(n: i64) -> i64 { let mut acc = 0i64; while n > 0 { acc += n; n -= 1 }; acc }
  0: FUNCF  frame=2            ; slots: r0=n:i64, r1=acc:i64 (static layout)
  1: LDIMM  r1, 0:i64
  2: LEI64  r2, r0, 0          ; unboxed bool
  3: JMPIF_NOT r2 -> 6
  4: ADDI64 r1, r1, r0         ; pure machine add, no tag work
  5: SUBI64 r0, r0, 1 ; JMP 2
  6: RET    r1

; mutual tail calls (H2.a)
even:
  0: EQI64   r1, r0, 0
  1: JMPIF   r1 -> 3
  2: SUBI64  r3, r0, 1
  3: CALLT   odd, nargs=1, src=r3    ; slide r3 into slot0, reuse frame
  4: RET     true
```
