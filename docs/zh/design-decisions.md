# DreamScript 工作设计决策（中文镜像）

状态：本文件是全部语言决策（44 个决策点：A1-A3、B1-B9、C1-C5、D1-D3、E1-E5、F1-F9、G1-G4、H1-H6）权威工作记录的中文版，由作者于 2026-11-08 定案，提取自 `design/` 冻结档案，2026-11-10 修订（E4/F2/F5/F7/F8：`or` 运算符退役、裸 `return` 采纳、反自然语言表层守卫定案），并于 2026-11-10 再次修订（B3/F2/F6/F7/G3/G4/H6/6.1 加新增 F9，语法表层采纳：插值即字符串上下文加定案转义集、带列表字面量的声明/构造故事、fn 唯一命名声明加 `unit`、接收者拼写、双命名空间、脚本入口、`##` 分隔符、sigil 形式导入、糖账目重算）。全部 Phase 4 实现工作以英文版为依据；`design/` 目录是只读档案，仅供交叉查阅 — 永不修改，除非本文档某条规则确实不够用，否则不必阅读它。研究笔记（`design/en/research/01-06`）是原始证据，实现不需要。档案的种子语料清单不在本文件重现：它们以已定表层移植到 `conformance/`（附逐文件移植说明），其运行时编码固定在 ds-abi/ds-vm 测试中。两版内容分歧时以英文版 `docs/en/design-decisions.md` 为准。

来源映射（各节对应的档案文件）：§2 `design/en/design/08-decisions.md`；§3 `design/en/design/01-types.md`；§4 `design/en/design/02-linearity-ownership.md`；§5 `design/en/design/03-effects-errors-async.md`；§6 `design/en/design/04-syntax.md`；§7 `design/en/design/05-generics-metaprogramming.md`；§8 `design/en/design/06-modules-tooling.md`；§9 `design/en/design/07-runtime.md`；各决策点的详细理由在 `design/en/design/09-decision-rationale.md`；总工作表是 `design/en/design/00-decision-map.md`。

## 0. 作者补充（design/author_say.md）— 约束性

1. 主要方向遵循 Phase 3 决策记录（08）及其理由文档（09），已全部提取到本文档。
2. 大小写绝不能成为语法的定死特征：任何语法构造都不得用标识符大小写区分（例如类型不要求首字母大写）。类型上作者偏好小写拼写（`int`、`string`、`list`、`file`）。本文档与档案中的代码使用大写类型名，仅为目录示例；实现时统一归一为小写。大小写在语言任何位置都不承载语法含义。

## 1. 已定语言（总览）

DreamScript 是一门独立通用解释型语言（通过平台包实现嵌入），采用混合集合论类型 — 名义标签在并、交、差之下组合，`is` 收窄，`T?` 是 `T | none` 的别名 — 以双向检查加必需的边界签名与流收窄进行检查，具备带五个特例的通用结构化相等。值默认是仿射的，带可选的 `~Copyable` 线性资源层（`deinit`、幻影状态）、二等参数传递模式（借用 / `mut` / 消费）、独占窗口可变性和线性可变 COW 集合；内存是 Perceus 引用计数，带块复用，架在每帧 arena 之上，无 GC。多态是带两两不相交实例头的连贯集合论 trait，单一泛型函数体携带证据字与内联缓存；元编程是无宏的 comptime 反射。错误是返回类型中的联合成员，由 `?`、`.err`/`.ok` 差集 handler 与 `unwrap_or`/`ok_or` 提取族处理，另有不可捕获的 trap；并发是强制 nursery 中的 VM 纤程，在线性/不可变的可发送性基底上做 fork-join 并行；效果隐藏在表层之下，运行时是证据传递的 handler 运行时加能力参数式 IO。表层是 TS 风格花括号加全量 UFCS、`fn` 声明加 const 箭头 lambda、Rust 形状的模式匹配（带差集诊断）、Kotlin 插值，语法糖预算 22 特性 / 34 单位（Kotlin 上限是 38 / 74-76）。模块是 definition < file < package < module 分层，带生成的 `.dsi` 接口且无导入期执行；包是 blake3 哈希级联清单铺在内容寻址存储之上，带零配置脚本模式；FFI 是线性 C 句柄。执行是三层流水线 — 类型化寄存器 VM、copy-and-patch JIT、Cranelift/wasm — 并带规格保证的自递归与互递归尾调用。

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
fn get_port(j: json) -> int | missing | bad_port {
  const pv = j.fields.get("port")   // 推断：json | none；下方 is 测试收窄为 int
  if pv is int && pv >= 1 && pv <= 65535 { pv } else { return bad_port{ got: -1 } }
}
```

### B3 — 名义 struct/enum 加匿名元组

命名数据用名义 struct 与 enum（声明式布局，各一个 type_id，允许 tuple-enum）；临时聚合用匿名元组加解构；支持 record punning（字段名简写）。enum 块是唯一的载荷 case 声明形式；独立 `tag x;` 声明单元 tag；类型别名永不声明 case（2026-11-10）。构造形式按声明定死 — struct = 仅花括号形式，enum case = 仅调用形式，单元 tag = 裸形式 — 交叉拼写是检查器错误（Swift 与 Rust 都在声明处固定构造形式；交叉拼写在各处都是硬错误）。列表字面量 `[a, b, c]` 采纳进 v1；map 字面量随匿名 record 字面量保留位一起延迟，过渡期用 builder 调用。匿名 record 字面量（`const point = { x: 1, y: 2 }`）是编译器合成的糖，铺在一个名义 record 之上 — 推断故事稳定后作为增量特性再加入。row 类型对数据一律否决。

### B4 — `T?` 恰是 `T | none`

`none` 是若干单元类型之一；可选、错误联合、枚举共享一套机制（`is` 收窄、match、以及按 E4 与 `unwrap_or`/`ok_or` 提取族组合的 `?`）。v1 无 `?.` 运算符（只有真实代码提出需求时才可能后加一个薄脱糖）；`t | none` 上的 `??` 是唯一记录在案的运算符阀门（F8）。`T | none` 收进一个槽的 niche 装箱是合法的后续优化器 pass（引用永不为零）。已知限制：`V | none` 中 `V` 本身可为 `none` 时有歧义 — 恰在那类位置用显式包装（`found[v] | none`）。

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

对所有数据的一套结构化 `==`，构造上全序（total），永不可被用户重载，不存在 `===`。五个特例：(1) 函数比较静态拒绝；(2) 浮点按位结构比较 — `nan == nan` 为真，字典键保持健全 — IEEE 语义走具名的 `is_float_eq` / `is_approx_eq`；(3) 可变单元与 `~Copyable` 资源按身份比较；(4) 操作数被借用，绝不被消费；(5) `hash` 必须与结构化相等一致。`Ord` / `hash` / `show` 以派生形式抵达；排序永远不能是结构化的。按类型的自定义值语义走具名方法（`equiv` 风格）。

## 4. 线性、所有权、内存、数据

### C1 — 仿射内核，可选 `~Copyable` 线性层

日常规则只有一句：对仿射值赋值即移动，被移动的绑定即死亡。普通可拷贝值从不可观察地撞上这条规则。`~Copyable` 减去默认的可拷贝性约束（一个被否定的集合约束，与 B1 联合自然组合）；它是 `deinit` 所在之处，并容纳幻影状态联合（`file[open] | file[closed]` 风格）。仿射检查是末次使用/DI 分析 — 过程内、单次前向遍历。

分阶段（后续步骤落地时语言零改动）：先发运行时 `rc==1` 唯一性检查加 Inko 式借用计数器作为诊断层；借用推断、完整复用配对与 TRMC 稍后作为纯优化抵达。

### C2 — 二等参数传递模式

三种模式固定在定义处 — 默认借用（不可变）、`mut`（独占可变）、消费（移动）— 调用点零模式语法：`xs.push(1)` 的读法与模式无关。借用永不逃出调用，所以检查是过程内的；不存在生命周期标注。一等引用永久出局。下标把同一组模式扩展到元素访问（get/mutate/set 访问器），取代 getter/setter 协议。

```ts
fn len(s: str) -> int { ... }              // 默认：借用
fn push(xs: mut list<int>, x: int) { ... } // 该调用的独占可变窗口
fn consume(f: move file) -> int { ... }    // 消费式移动
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

运行时从第一天起就是 handler 形状的：证据传递服务两个内部效果（`cancel`、`discontinue`），仅在 nursery 边界 — 尾续内部操作不分配；用户代码路径不变。触碰世界的标准库 API 遵循能力习语：脚本由 `ds` CLI 安装一个环境默认 handler，测试、嵌入与调度器选择则用显式能力参数 — 同一份代码可跑在阻塞、事件循环或自定义调度器上。用户自定义 handler（`with handler { op ask() => resume(42) }`）保留为 v2 增量逃生口，永不需要迁移。v2 激活时的常设类型义务：续体除非标注否则是仿射的（多射续体不得捕获线性值）；FFI 调用钉住纤程（H5）。

### E2 — VM 纤程加强制 nursery

VM 池上的栈式堆纤程（纤程栈就是解释器数据；spawn 是一次分配；切换是指针交换，O(1)）。`spawn` 只存在于 `nursery` 块内；作用域退出即 join 子任务，首个失败取消其余兄弟，子任务错误在边界联合汇出。阻塞式代码，无颜色，无 `await`。抢占是派发循环每 N 条指令或回边处的一次标志检查。TCO 按纤程成立且不受影响（尾循环纤程永不增长其段）。裸 detach 需要显式根 nursery 或监督者。

```ts
nursery {
  for url in urls { spawn { pages.push(fetch_body(url)) } }
}   // 退出：已 join；首个失败已取消其余；错误已联合汇出
```

### E3 — 可发送性基底上的 fork-join 并行

无数据竞争与可发送性是线性/不可变数据的静态推论（线性值按构造即 `sending`；不可变值按构造可共享）；缺的只是调度器。`par_map` / `par_reduce` fork-join 组合子跑在不可变或线性分区的输入上 — 确定可重放、按定理无锁、静态 RC 计数使 fork 侧快照廉价。actor 与监督层可日后作为库加入（监督者就是一个带重启策略的 nursery）。

### E4 — 联合错误用 `?`、`.err`/`.ok` 与提取族处理；`or` 于 2026-11-10 退役

预期失败是联合成员，由令牌与方法处理，其语义只在联合上陈述一次：`?`（受检包含的传播）、`.err(h)` / `.ok(h)`（差集类型的 handler — 余留类型直接展示还剩什么）、以及 @std 提取族 — `unwrap_or(y)`、`unwrap_or_else(fn)`、`ok_or(e)`、`map_err(fn)` — 普通 UFCS 可调用的泛型函数，类型完全局部，与 `?` 组合实现提前退出。`or` 运算符退役：它把一个词重载在回退、提前退出、循环控制与联合合并之上，与布尔 `||` 冲突，并按上下文 wanted 成员推断定型（完整理由记录于 F8）。显式提前退出拼写是 `if x is none { return e }` 加收窄，裸 `return` 已采纳（F2）。bug 一律 `panic` 且不可捕获：纤程中止，帧遍历运行线性 deinit，trap 在 nursery 或监督者边界浮现。let-it-crash 监督是库模式。

```ts
fn load_config(path: str) -> config | io_error | parse_error {
  const text = read_file(path)?        // `?`：ok 类型之外的成员提前返回
  parse_config(text)?
}
const cfg  = load_config("app.cfg").err((_) => default_config())   // 错误联合回退，惰性 handler
const cfg2 = load_config("app.cfg").err((e) => {   // 结果类型：(config | io_error | parse_error) - (io_error | parse_error) == config
  log("config failed")                     // e 是联合类型；插值洞只接受原语或带 show 的类型（F6）
  default_config()
})
const port = cfg2.port.unwrap_or(8080)             // 可选回退：t | none -> t，右操作数急切求值
const name = row.get("name").ok_or(missing_field("name"))?   // 带新值提前退出；`?` 传播它
```

### E5 — nursery 范围的协作取消加消费即关闭

取消是 nursery 范围且协作式的，只在 await/抢占点被观察（一次标志检查），因此线性消费永不在飞行中途被打断；销毁遍历帧运行 deinit。资源靠消费关闭（一次静态移动；语言内句柄按构造无泄漏）。`defer` 作为作用域退出糖存在，非规范，且在取消下于销毁时运行（Koka 的 `finally` 语义）。超时、`firstof`、`interleave` 是计时器能力上的库组合子 — 没有任何超时形状的东西是语言原语。

## 6. 语法表层与语法糖预算

### F1 — TS 风格花括号

每个块以可见令牌开启；面向行的文法，无布局状态；换行终止语句，无分号，处处允许尾逗号；`if` 与 `match` 是表达式。记录在案的后路：若原型显示箭头赋值文法与解析器相搏，退回 Gleam/Roc 风格花括号（`fn name(` 标题行）。

### F2 — 绑定、函数、标注、返回

`const` / `var` 绑定（var 是可重绑定局部）。具名函数是 `fn name(p) -> t { }` — 唯一的具名函数声明形式，用于顶层、trait/impl 成员、以及 trait 中的无体签名（`fn to_json(self) -> string`）；定义名在自己体内可见（递归）。const 箭头仅保留给匿名 lambda `(x) => e`（Rust：具名用 `fn`，闭包是表达式；TypeScript 的双拼写是要避免的样式战争）。`unit` 是单位类型在标注中的可选具名拼写；`()` 保持合法。参数 `name: t`，返回 `-> t`；联合返回解析为 `-> a | b`，无额外语法；`T?` 在一切标注位置合法。末表达式返回；提前退出走裸 `return`（2026-11-10 采纳 — B2 自己的示例已在用它而本节此前延迟它）与循环中的 `break` / `continue`。`?` 保留给后缀传播；旧 README 的前缀 `?` 返回记号已退役，`or` 提取运算符按 E4/F8 退役。函数是值。

```ts
fn parse_age(s: string) -> int | parse_error {
  match s.trim().parse_int() {
    e if e is parse_error => bad_number(s),
    n                     => n,
  }
}
fn parse_user(row: row_t) -> user | parse_error {
  const name = row.get("name").ok_or(missing_field("name"))?
  user { name, age: parse_age(row.get("age").ok_or(missing_field("age")))? }
}
```

### F3 — 全量 UFCS

`x.f(a)` 对任何首参函数脱糖为 `f(x, a)`；零运行时成本，无候选搜索。解析固定：自由函数优先，其次 trait 方法，`Trait::f(x)` 限定作为逃生口；真歧义是编译错误，永不是优先级搜索。能力参数具名且被排除出点号位置 — 点调用喂给第一个非能力数据参数，因此 `println(io, msg)` 永远写不成 `msg.println()` 而藏起 `io`；能力显式传递或经作用域形式。`|>` 是延迟增量（Roc 因其与链式调用加 `?` 冗余而删除了管道）。

### F4 — Rust 形状的模式匹配

分支 `pat => expr` 以逗号分隔；守卫 `pat if cond`；或模式 `a | b`；解构绑定；`_` 通配；穷尽匹配带差集诊断 — 错误信息把缺失成员按集合计算报出（"missing: NotFound | Locked"）。`@` 绑定器 v1 排除（`is` 收窄保持 scrutinee 变量可用；延迟增量，若被需求则需设计仿射重绑定语义）。

### F5 — 固定 trait 驱动运算符集，冻结优先级

固定令牌集映射到固定 trait（Add、Sub、Mul、…、Index）— 运算符调用就是 trait 方法调用，可内联缓存。一张冻结的 C 家族优先级表在教程中发布一次，永不扩展，永无用户自定义运算符；词形运算符恰是 `in` 与 `is`（Kotlin 形状），布尔上下文一律 bool 类型，任何位置都无真值强制转换（F8）；`Deref` 式强制转换被排除出用户表层。`==` 是 B9 的结构化相等且永不可重载；排序走 `Ord` 派生。

| 运算 | 陷阱（默认） | 回绕（显式） | 备注 |
|---|---|---|---|
| 加 / 减 / 乘 | `a + b`、`a - b`、`a * b` | `a +% b`、`a -% b`、`a *% b` | trap panic 带位置 |
| 取负 | `-a` | `-%a` | int-min 边界 |
| 除 / 余 | `a / b`、`a % b` | 无 | 除零与 int-min/-1 永远陷阱 |
| 移位 | `a << k`、`a >> k` | 无 | `k` 超位宽时陷阱 |
| 浮点 | IEEE 语义，永不陷阱 | 无 | inf/nan 按 IEEE |

### F6 — 字符串与字面量

插值 `$x`（裸标识符）与 `${expr}`（表达式）；插值孔是显式字符串上下文 — `$e` 与 `${e}` 脱糖为 `str(e)`，操作数类型为原始类型（int、float、bool、char、string）或携带 `show` 时合法，联合或可选操作数是错误：先提取（B4/E4）。其余一切位置 — 运算符、实参、绑定 — 保持严格显式，隐式转换会在那里改变解析（Kotlin、Swift 与 Rust 的格式化孔都是字符串上下文）。转义集（2026-11-10）：`\n \t \r \\ \" \' \0 \u{...}`；原始字符串 `r"..."` 与按哈希计数的 `r#"..."#`（可嵌套到任意深度，一个构造同时服务 raw/多行/heredoc）不携带转义。`'x'` 是 Unicode 标量 char；`b'x'` 是单字节。

### F7 — 语法糖预算：Balanced 档（22 特性 / 34 单位）

对照 Kotlin 的 38 特性 / 74-76 成本单位上限；预留余量：16 特性 / 40-42 单位。包含行：换行终止语句（S1）；插值 `$x`/`${e}`（S1）；原始字符串 `r#"..."#`（S1）；列表字面量 `[a, b, c]`（S1，2026-11-10 采纳）；后缀 `?`（S2）；`is` 收窄（S2）；`.err(h)`/`.ok(h)` 差集类型 handler（S2）；match/if 表达式带穷尽联合诊断（S2）；match 守卫（S1）；或模式（S1）；解构（S2）；具名 + 仅字面量默认参数（S2）；尾 lambda，仅限末参（S2）；UFCS（S2）；类型别名（S1）；和类型声明 `tag`/enum（S2）；派生属性 `#[derive(...)]`（S2）；范围 `..`/`..=` + `in`（S1）；数值后缀 + 分隔符（S1）；`const`/`var` 之分（S1）；运算符重载 trait，固定集合与优先级（S2）；`T?` 别名（S2）。延迟增量行（按余量使用可能性排序）：`|>`（S1）、匿名 record 字面量（S2；map 字面量按 B3 随此保留位）、`@` 绑定器（S1）、`??` 作用于 `t | none`（S1）、带标签 break/continue（S1）、lambda 简写 `it`（S1）。被集合类型与 UFCS 整体删除的 Kotlin 特性：`?.`、elvis、`!!`（由 `is` 收窄、none 类型与提取族取代；`t | none` 上的 `??` 是记录在案的阀门）、smart cast（`is` 收窄）、中缀函数与扩展函数/属性（UFCS）、data class 合成（派生 + 结构化 `==`）、sealed 层级（封闭联合）、接收者 lambda DSL、主构造器、reified 类型参数、反引号标识符、`tailrec`（TCO 已保证）、context 参数（能力参数）。永久排除出用户表层：自由运算符重载与自定义运算符、Deref 强制转换、基于约定的私有性、布尔上下文的真值强制转换、链式比较、推导式语法、下标切片糖、vararg/spread 打包、以及 `in`/`is` 之外的词形运算符（F8）。fn 唯一命名声明（F2）与双命名空间拆分（F9）删除拼写而非增加糖。

### F8 — 经方法与既有令牌提取；无自然语言运算符、无隐式强制转换（2026-11-10）

`or` 运算符从表层退役。遍及记录与语料它承载六种行为 — 可选回退、错误联合回退、带新值的提前退出、落到发散被调方的回退、不同类型操作数的联合合并（`done or last` 产出 `page | net_err`）、以及循环控制 — 由上下文 wanted 成员推断定型，且是与布尔 `||` 冲突的词；档案里布尔测试上的 `is ... or return` 就是已经在滑动的边界。替代物铺在既有令牌加一个很小的 @std 族上：可选回退 `x.unwrap_or(y)` 与惰性 `x.unwrap_or_else(fn)`；带新值提前退出 `x.ok_or(e)?` 与 `x.map_err(fn)?`；错误联合与发散回退 `x.err(h)`；联合合并 `match done { p => p, none => last }`；循环控制 `if x is none { break }`。裸 `return` 随退役进入表层（B2 示例已在用它而 F2 此前延迟它 — 以采纳解决）。若语料证据日后显示可选回退配得上一运算符，`??` 限定于 `t | none` 是唯一记录在案的延迟增量（S1）；任何英语单词永不再进入运算符集。

随附自然语言语法审计采纳的守则，评审时即约束：运算符保持 C 家族符号，`in` 与 `is` 是仅有的词形；布尔上下文（`if`/`while` 条件、`&&`/`||`/`!` 操作数）一律 bool 类型，任何位置对联合、可选、数字、字符串都无真值强制转换；链式比较永久排除；推导式语法永久排除（D3 组合子表层已覆盖）；下标是单元素 Index 访问，无切片糖（字符串切片按 B8 是受检库调用，其余集合同此）；vararg/spread 打包从延迟增量清单退役（显式 list 实参与 `concat` 已覆盖）。结构性已删除、记录备查的隐患：可变默认参数因默认值仅限字面量而不可能；混合数值算术是报出转换的错误（B7）。`is` 是 Kotlin 形状的类型收窄（非 Python 身份比较），`in` 是 Kotlin 形状的成员测试；两个拼写都保留。

### F9 — 双命名空间：类型与值（2026-11-10）

类型与值在两个命名空间中解析。构造器 — 花括号构造、裸 tag、泛型应用构造器如 `list[page]()` — 在类型命名空间解析；函数在值命名空间解析；同时出现在两个命名空间的调用形式名字是歧义错误，绝不是优先级搜索（F3 规则）。`map` struct 与 `map` 函数共存（Rust 在独立命名空间中解析类型路径与值路径，struct 与函数自由重名；C++ 更繁的全命名空间机制是过度工程的反例）；enum case 对函数的调用形式冲突是残余的错误类别。

### 6.1 实现归一的表层拼写（语料采纳；待 M2 锁定）

种子语料（`conformance/`）在记录未细化表层之处使用的拼写；为实现采纳，M2 检查时锁定或修订：trait 成员签名的接收者免标注且定型为 `Self` — 借用模式用裸 `self`，独占窗口用 `mut self`（按 C2），无 `&Self`/`&mut self`；其余参数可在标注中使用 `Self`；`self` 作为类型名退役（2026-11-10；Rust 先例 — 没有主流 trait 系统让接收者重述自己的类型）；派生是属性 `#[derive(hash, show, ord)]`（按 F7 派生行）；泛型值声明用 `struct stack[t]`，函数式 `push/pop` 返回更新后的值（按 D2；原地字段赋值未决）；函数上的泛型子句拼写为 `[t: c]`；空可变集合以 `list[page]()` 构造（最终标准库拼写归 M3 定）；v2 `with handler` 块按 E1 出 v1 范围，语料中保持注释。

解析器裁决（ds-parse 落地 3167597）：泛型应用规范为方括号 `list[int]`；角括号形式 `list<int>`（出现在 C2 的前表层示例文本中）作为解析器宽容被接受，但格式化器永不输出。块体成员、match 分支、struct/enum 字段规范为换行分隔；按 F1 处处接受尾逗号。参数模式位于标注内部（`f: mut file`）并带前导裸模式简写（`mut self`）（按 C2）；`move` 与 `comptime` 是上下文关键字；trait/impl 成员是 `fn` 声明，trait 中无体（const 箭头具名成员按 F2 退役）；类型层 case 引用、元组类型 `(a, t?)`、外来类型 `c["sqlite3"]` 解析为类型形式；`is` 右侧是类型表达式（它吃掉 `|`/`&`）；C 家族 `&&`/`||`/`!`/`&`/`^`/`|` 在固定运算符集中；普通字符串单行且块注释不嵌套；顶层 `fn` 是唯一的具名函数声明（const 箭头具名声明按 F2 退役），顶层表达式语句按 G3 拒绝，脚本模式下入口文件除外（G3）；`none`、`self`、`Self` 非保留字。M1 时标识符为 ASCII。

解析器裁决，审计第 3 轮：`in` 按 F5 绑定在比较级（`x in xs && flag` 读作 `(x in xs) && flag`，绝不是 `x in (xs && flag)`）且允许裸范围作右侧（`x in a..b`）；孤立的 `\r` 在字符串、插值孔与 char 字面量中恰如在代码中一样被拒绝（唯原始字符串可携带）；换行分支分隔覆盖表达式体分支，不限于块体分支；trait 与 impl 成员清单恰如 struct/enum 体一样接受逗号分隔与尾逗号；impl 成员仅是 `fn` 声明 — 裸 `const x = 1` 或 const 箭头成员是解析诊断（DS1010），绝不静默丢弃；插值孔绝不跨换行（普通字符串保持单行）；F7 的具名 + 仅字面量默认参数已落地：调用点在位置实参之后取具名实参（`f(x: 1)`；重名是 DS1011，具名后再位置被拒绝），签名取仅字面量默认值（`x: int = 0`，允许 `-lit` 负数；其余一切，包括插值字符串，是 DS1012）；扁平运算符链（包括堆叠一元与后缀链）上限 128 项（DS1009），因为丢弃更深的 `Box` 链会溢出调用方栈。

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

四层：definition（可见性与身份单位）、file（命名空间拼写）、package/目录（编译单位 — 同一包内文件共享一个命名空间，包内零 import）、module（`ds.pkg` 清单：依赖与分发单位）。包内环合法（整包不动点解析）；跨包环是链接错误，其诊断建议把共享抽象移入基础包。生成的 `.dsi` 接口文件是可评审的 API 表面（`ds info` 生成；".dsi 无变化通常即安全重构"）。模块体是纯编译期声明 — 无导入期执行，无一例外；这一条规则同时买到包内环容忍、安全热重载与惰性依赖驱动加载。脚本入口例外（2026-11-10）：仅脚本模式下，入口文件的顶层语句合法并编译进合成的 `main` — Node 与 Bun 执行入口文件顶层语句，宪章的脚本目标指向前者且仅限入口文件 — 导入的文件与包成员保持纯声明体，保证在它保护的一切地方成立。块分隔符（2026-11-10 重拼）是顶层的行首 `##`，行余下部分为分隔符文本；`///|` 退役，`///` 成为保留的文档注释前缀（设计待定；今天仅是 trivia）。无论有无分隔符，顶层定义都与顺序无关，无分隔符的文件就是一个块。

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

definition 默认私有；`pub(pkg)` 对整包开放（含包自身的测试）且永不进入 `.dsi`；`pub` 对依赖方开放并进入 `.dsi`。导入按符号选择、可别名、可按目标条件化，并可用 `pub import` 再导出；拼写统一为 sigil 形式 — `import @std/csv (reader)`，路径加可选选择/别名 — 引号形式退役（2026-11-10）：

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

**H5.5 — std/builtin/平台三层（约束性）。** 三层，各带成员判据。(a) VM 内核原生（保留的 `builtin` 模块）：ds 源码无法表达的不可再简原语 — 针对 VM 内部的操作（驻留字符串、对象存储），lower 契约本身需要它们；纯函数或错误联合全函数，绝不触碰世界（当前成员：str_* 转换与字符串操作）。(b) `@std`：普通 ds 源码包 — DreamScript 自身可表达的一切都落在这里（D3 组合子在泛型/M6 允许后抵达），脚本模式的裸限定符导入在这里解析。(c) 平台：世界表面（console、fsys、net、time）作为按 H5/E1 的 effect handler，按构建目标选择；`ds` CLI 是默认平台提供者。触碰世界的原生是 M3 过渡形态的平台 handler：M3 的单 builtin 模块（原生 0-24）混合了各层，如今原生注册表携带两个原生模块 — `builtin`（纯内核）与 `std`（默认平台的 console/fsys/net 表面）— 因此构造 VM 的库嵌入者除非安装平台，否则拿不到世界表面；原生在 M7 handler 运行时存在时（E1）迁移为 effect handler 形态，原生索引在那之前冻结。

### H6 — 哈希存储内核加零配置脚本模式；五项强制工具链承诺

**包栈。** `ds.pkg` 清单以 `name = { ver, hash }` 声明依赖，hash 是对该包内容（含其依赖哈希）的 blake3 — 身份传递式流动，不存在 lockfile（清单即锁）。MVS 解析（取声明最小值的最大者；多版本共存按构造成立）。按输出哈希键控的全局内容寻址存储，兼作字节码缓存（工件即 dsmod 文件）。按 target triple 的 bin-first 工件，对照存储哈希校验。可选 FHS profile 链接器（`ds link`；Windows 用 scoop shim，不用符号链接）。registry 裁决：registry 是仅持有包元数据的 git 仓库，可配置为本地路径或远程 URL；依赖解析读索引元数据（name -> versions -> 清单哈希与工件 URL），从存储按内容寻址获取工件，并对照 blake3 哈希级联校验（清单仍是锁；MVS 不变）。依赖绝不通过克隆或追踪依赖方自身的 VCS 工作树解析 — 无分支头，无可变伪版本；git 仓库是元数据传输，不是身份来源，哈希钉定才是。本地 git 索引使离线与私有 registry 成为一等公民。

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

```text
index/<a>/<ab>/<pkg>.json   # 每包一个元数据文件：versions -> 清单哈希 + 工件 URL
git history                 # 元数据审计日志 — 仅元数据，绝非依赖来源
```

**脚本模式。** `ds run script.ds` 零配置；裸限定符标准库导入用 sigil 形式（`import @std/csv (reader)`；引号拼写退役，G4）；`ds save` 把脚本用过的东西冻结进生成的 `ds.pkg`。长成包的脚本获得清单而不丢失文件。

**五项强制工具链承诺，自第一天约束。** (1) 编译器即查询库：编译器是记忆化查询的库（`ds-check`）— parse、block_index、resolve、type_of、effects_of、linearity_check、lower — 由 LSP、REPL 与热重载共同消费；失效粒度为 `##` 分隔块加包粒（跨包环被禁的第二个理由）。(2) `ds fmt` 零选项，实现为编译器组件；块感知格式化器独立格式化各块，兼作 LSP 的输入时格式化路径。(3) 结构化诊断加双跨度线性错误 — 机器可读建议、`--error-format=json`、跨度在类型化 AST 层计算；线性诊断类别渲染"此处被移动 / 此处再次使用"成对输出：

```text
error[DS3001]: value `h` was consumed here
  --> src/pipeline.ds:41:15
   |  41 |  let rows = Csv.read(h)     ^ `h` moved (file is ~Copyable)
   |  42 |  Log.write(h.path)          used again here after move
help: clone the path before the move            (suggestion emitted as JSON too)
```

(4) 线性感知 REPL：打印推断类型与 effect row；跑在同一字节码 IR 上；会话持有的线性上下文配显式 `:drop` 与 `:reset` 清除 — 提示符处的线性值永不被悄悄丢弃。(5) Erlang 双版本代码槽热重载：运行时持有逐模块版本槽（current + old，至多两个存活）；`ds dev --hot` 重编译变更块并换槽；旧版本帧在其下一次跨模块调用处退役（一切跨模块调用都全限定，Erlang 的要求被平凡满足）；堆值对新定义做类型检查；effect handler 捕获其安装时的代码版本。无导入期执行正是其健全性来源。双槽运行时必须从第一天存在 — 后补是被点名的错误。

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

Tier 1：类型化寄存器 VM，computed-goto uop 循环，未装箱帧 — 目标 3-10x of C。Tier 2：copy-and-patch stencil — 每个 uop 一段 x86-64 片段，构建期由 LLVM 从含 VM 头的主模板发射，洞为帧槽偏移与常量，按 u16/u64 补丁；比 LLVM -O0 快约 14% 且编译速度约 100 倍；目标 1.5-3x；每单位微秒级。Tier 3：字节码 → 类型化 SSA → Cranelift CLIF，或 wasm 3.0（`return_call` 携带 TCO）；目标 1-2x 原生 / 1.5-2.5x wasm。一套 uop 与一条字节码到 SSA 的 lower 被所有 tier 共享，尽早建成。tier 迁移是原地 J 变体 opcode 重写（一次派发）；OSR 化归为编译循环体 — 无帧变换机制。任何数据路径无守卫；仅有的出口是 (i) 调试器断点（从 JIT 元数据物化帧）、(ii) 逐入口的栈上限比较、(iii) effect handler 进入（一次控制转移）、(iv) B7 的冷溢出边 — 因此没有 deopt 悬崖。短脚本（<0.2s）从不支付分层代价且仍达 3-10x。无验证 stencil 构建的平台只发 tier1（CPython 的 opt-in 姿态）。记录在案的后路：若逐架构 stencil 工具链被证不可接受，退回单一 method-JIT tier（H1b）。

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

## 10. 分阶段（v0 → 1.0）

有序；每步落在前一步上，无语言改动的重写。

1. 先锁定两个承重契约：槽位重叠调用 ABI（H2）与定宽可 mmap 的 dsmod 格式（BL-7、G3.5）。下游一切编译都以它们为前提。
2. 检查器内核：B1(c) 名义 tag 上的集合代数、B2 双向加收窄、B3(d) 积/和、B4(c)、B7(a)、B8(a)、B9(a)；仿射检查器加 DI（C1b、C5c）。
3. Tier-1 解释器：computed-goto 或尾调用穿线的 uop 循环、未装箱槽、tag+payload ADT、CALLT 帧复用 TCO。
4. 内存：Perceus dup/drop 放置、复用串线、每帧 arena；运行时 `rc==1` 检查与借用计数器作为诊断层。
5. 并发：纤程、nursery、内部 `cancel`/`discontinue` 证据派发、fork-join 组合子。
6. 派生；然后 comptime 求值器与反射 API（1.0 前）。
7. 自第一天起兑现工具链承诺，与它们消费的查询编译器一同排程：`ds fmt`、结构化诊断、REPL、双槽热重载。
8. Tier 2 copy-and-patch（x86-64，然后 aarch64；其余平台仅 tier1）、尽早建字节码到 SSA lower、tier 3 按需。
9. 核心稳定后的延迟增量清单（见 §11）；借用推断、TRMC、融合升级作为零语言改动的纯优化落地。

## 11. 延迟增量清单与记录在案的后路

延迟增量（按余量使用可能性排序）：`|>`、匿名 record 字面量（map 字面量按 B3 随此保留位）、`@` 绑定器、`t | none` 上的 `??`；随后 `?.` 脱糖、`throws(...)` 糖、用户自定义 handler（`with handler`）、作用域具名实例、带标签 break/continue、`it` 简写。已从本清单退役：vararg/spread（F8）；裸 `return` 已于 2026-11-10 采纳进表层（F2）。纯优化项（零语言改动）：借用推断、完整复用配对、TRMC、融合升级、`T | none` niche 装箱、rope。记录在案的后路（命名风险成真时）：B1(b) 纯名义内核（若不相交机制拖期；错误处理退化为基于 match）；F1(b) Gleam/Roc 花括号标题行（若箭头赋值文法与解析器相搏）；H1(b) 单一 method-JIT tier（若逐架构 stencil 工具链被证不可接受）；C4(b) 分代引用（若仿射摩擦比预期更糟）。
