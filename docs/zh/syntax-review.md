# DreamScript 语法面评审：与 Rust 及现代语言对照

评审对象是 v1 语法面，以 `docs/en/tutorial.md` 为准，对照实现现状（ds-parse grammar、ds-check 行为）与语例（`examples/`）。对照语言：Rust、Kotlin、Swift、TypeScript、Go，辅以 Zig、Gleam、Roc、MoonBit。目标是心智负担小、表达能力强；结论按可执行度分级。评审日期 2026-11-11。

## 1. 总评

语法面的核心赌注成立：TS 家族拼写承载 Rust 级语义，糖预算 22 特性/34 单位约为 Kotlin（38/74-76）的一半。负担最重的传统部分被系统性移走：生命周期与借用标注不存在（模式固定在定义处，调用处零语法，优于 Rust 的 `&`/`&mut` 噪音）；错误处理没有 Result 包装词汇（联合成员 + `?` + 提取族，日常拼写短于 Rust 与 Go）；无色并发（fiber + nursery）优于 Kotlin/Swift/TS 的 suspend/async 染色；TCO 是规范保证（全部对照语言都不保证）。

剩余心智负担集中在四处：集合代数的高级算子（`&`、`-`、`~Copyable` 否定）是新词汇，日常用 `|` 与 `T?` 可渐进覆盖；`.err`/`.ok` 差集 handler 的覆盖侧判定规则未写明（第 6 节）；UFCS 自由函数模型下多类型竞争的操作名面临前缀化（`list_push`/`list_get`）；字面量限定的默认参数偶有不便。

真正的风险在表面空白与漂移：每个未裁决点将来都是惊讶或 breaking，每处漂移都在违反"一篇短教程即全部表面"的自我契约。第 3、4 节逐项列出。

## 2. 分维度对照

| 维度 | DreamScript | Rust | Kotlin | Swift | TypeScript | Go |
|---|---|---|---|---|---|---|
| 绑定 | `const`/`var`，默认不可变 | `let`/`let mut` | `val`/`var` | `let`/`var` | `const`/`let`/`var` | `:=`/`var` |
| 可变性 | affine 值 + `mut` 窗口，调用处零语法 | 所有权 + `&mut` 标注 | 引用默认可变 | 引用默认可变 | 引用默认可变 | 引用默认可变 |
| 类型表达 | 集合代数 `\|` `&` `-`、`T?`、nominal 原子 | enum + trait，无并集 | sealed + 平台 nullable | enum + Optional 糖 | 结构化 union | 无并集 |
| 泛型子句 | `[t: c]` | `<T: C>` | `<T : C>` | `<T: C>` | `<T>` | `[T any]` |
| 匿名函数 | `(x) => e` 单一拼写 | Fn/FnMut/FnOnce 三分 | lambda 统一 | lambda 统一 | arrow 统一 | func 字面量 |
| 方法解析 | UFCS 自由函数优先 + trait | impl 块内方法 | class 方法 + extension | extension/protocol | class 方法 | 方法 |
| 模式匹配 | match 全量 + `is` 流细化 | match 全量 | when（无嵌套解构） | switch 模式 | 无（判别联合靠链） | 无 |
| 错误处理 | 联合成员 + `?` + `.err`/`.ok` + 提取族 | Result + `?` + 组合子 | 异常 + runCatching | throws/try | 异常 + async 染色 | error 返回值 + if err |
| 空值 | `none` 单元 tag + `T?` | `Option<t>` | 内建 `T?` | 内建 optional 糖 | undefined/null 无类型化 | nil 无类型化 |
| 并发 | fiber + nursery，无色，结构化 | async/await 染色 + Send | suspend 染色 | async/await 染色 | async/await 染色 | goroutine 非结构化 |
| 尾调用 | 规范保证，自/互 | 无保证 | 无保证 | 无保证 | 无保证 | 无保证 |
| 运算符 | 冻结表 + 固定 trait 集 | trait 重载 + 宏 | infix 函数可任意加 | 有限重载 | 无重载 | 无重载 |
| 相等 | 一个结构 `==`，不可重载 | 派生 Eq，可重载 | equals 可重载 | Equatable 可重载 | `===`/`==` 双轨 | 可重载 |
| 分号 | 无，换行终止 | 必须 | 可省 | 无 | 必须（ASI 陷阱） | 必须 |
| 元编程 | comptime 求值 + 静态反射，无宏 | 声明宏 + 过程宏 | 编译器插件 | result builder | 无 | 无 |

比全部对照语言都轻的部分：调用处模式语法（Rust 需要标注，其余对照语言隐藏共享可变）；错误词汇量（Go 的 if err 链、Rust 的 Result 组合子、TS 的 try/Promise 链都长于 `?` + 提取族）；闭包词汇（Rust 的 Fn/FnMut/FnOnce 三分消失）；相等（一个 `==`，无 `===`、无重载）；Zig 式 comptime 但多了模式匹配与 trait。

与 Kotlin 大体持平的部分：插值、命名实参、尾随 lambda、`is` 细化、`in`、默认参数（更严格）。糖预算减半的前提下保住了 Kotlin 的日常手感。

更重于对照语言的部分：差集 handler 的侧判定是新推理点；集合差 `a - b` 与否定 `~Copyable` 超出 TS 的 `|`（其余对照语言无集合语义）；无重载下多类型 API 有前缀压力（Rust/Kotlin 的方法按类型命名空间隔离，天然无此问题）。

## 3. 表面空白：未裁决，建议立即 pin（零糖成本）

每条给现状、建议裁决、教程重配行。这类项的成本为零，拖延的代价是将来二选一：breaking 或永久惊讶。

1. 同作用域重绑定（shadowing）。现状：嵌套作用域可遮蔽，同作用域重定义报错（ds-check resolve 的 "redefined here" 诊断）；教程与决策记录均未写。建议允许同作用域同名重绑定：Rust `let x = x.trim()` 先例，是默认不可变语言里转换链的人体工学关键（`const text = text.trim()` 免去 `text2` 这类中间名）；affine move 语义下旧绑定死亡点明确，无额外检查成本。教程一行："同名 `const` 重绑定消耗旧值"。
2. lambda 内控制流。`return` 在 lambda 里从 lambda 自身退出还是外围 fn；循环体内 lambda 里的 `break`/`continue` 能否穿越。建议：一律最内层（lambda 是值，不触碰外层控制流，Rust 闭包同语义），循环控制穿越 lambda 边界为编译错误。教程一行。
3. nursery 块的值语义。错误在边界并出已有裁决 [E2]，但 nursery 块本身作为表达式的类型（体内尾表达式类型 ∪ 子错误联合）未写明；M5 前必须 pin，复杂可先裁"语句位置专用"。
4. 空集合字面量的上下文推断。`const xs: list[int] = []` 建议合法（B2 双向检查的自然推论，空字符串字面量已走同路径）；`list[int]()` 保留给无标注场景。教程一行。
5. 负数字面量进模式。默认参数已接纳 `-lit` [F7]，match 模式的 `-1 =>` 建议同样接纳。
6. 元组整体再赋值。`(a, b) = f()` 建议拒绝（重绑定一次一个名字），解构仅存在于声明处；写明即可。
7. Unicode 标识符。M1 ASCII 维持，但后续策略（XID_Start/XID_Continue + NFC）值得现在记入 backlog，脚本受众里非 ASCII 用户不少。
8. `else if` 链。语法已支持（else 后接 if 表达式），教程未提；补一句。
9. `///` 文档注释。design pending [G3]；它要喂 `.dsi` 与 LSP hover [H6]，建议 M7 前裁决。

## 4. 漂移：教程契约 vs 实现与语例

1. 复合赋值。`+= -= *= /= %= <<= >>=` 全家族已在 grammar（ds-parse grammar.pest 的 assign_op）且 `examples/` 高频使用，tutorial.md 与 design-decisions.md 零记录——这是"短教程即全部表面"契约的第一个实质漏洞。建议补录：`x += e` 是 var 重绑定的算术糖（右侧求值一次），`<<=`/`>>=` 与 shift 的 trap 语义同表；糖预算记 S1。删除亦可，但脚本人群肌肉记忆强、语义无争议，补录收益大。
2. 函数类型拼写缺席教程。`fn(t) -> r` 在 grammar（fn_type）与决策示例 [C5] 中存在，教程从未介绍；高阶函数标注必需。备选 `(t) -> r`（Kotlin 型拼写统一）；建议维持 `fn` 前缀——类型位置与声明关键字呼应，读一眼可分类型与值。教程 Functions 段补一行加一例。
3. `-> ()` 噪音。教程允许省略 unit 返回标注 [F2]，examples 全部手写 `fn main() -> ()`。实现支持省略后统一清理语例，避免教坏用户。
4. `.ok(id).err(0)` 恒等链。这是提取族未落地前的过渡拼写（`.ok` handler 已落地、`unwrap_or` 未落地）。family 落地后刷新全部语例为 `unwrap_or`，并建议 conformance 加一条 lint：拒绝 `.ok` 后紧跟 `.err` 的恒等 handler 链。
5. `parse_int` 的联合返回 vs 语例。`examples/mutation.ds` 里 `"8080".parse_int()` 的结果直接进 `port + total` 算术，与 B5 的联合返回矛盾；multi-atom use 是 M3 的 open 项。语例应改为 `is`/match 分支或 `?`，或尽快实装"多原子使用拒绝"的裁决。
6. `in` 的口径。F8 裁定 `in` 右侧仅 range（非 range 拒绝），M3 状态记录写 "in-membership landed"；若指 range 成员则一致，否则两处口径要对齐。

## 5. 表达力增强候选（按建议优先级排序）

预算核算：P1、P2 各记 S1（约 2-3 单位），22/34 升至至多 24/37，余量仍充足；每项附教程重配行，过"一篇短教程"测试。

- **P1 类型测试模式（建议采纳，S1）**。现状：匹配"enum 的任意 case"要写守卫 `e if e is parse_error => ...`，或枚举全部 case 的或模式 `bad_number(_) | missing_field(_) => ...`。提案：裸名模式在类型命名空间解析为类型时即类型测试，`match e { parse_error => ..., n => n }`。零新 token：unit tag 的裸名模式已按类型命名空间解析 [F9]，扩展到 enum/struct 名是自然延伸；scrutinee 变量经 B2 细化携带收窄值，无需 `@` binder。对照 Kotlin 的 `is` in when、Scala 的 `case x: T`。教程重配一行："模式里的裸名按双命名空间解析：值命名空间是绑定，类型命名空间是类型测试"。
- **P2 struct 更新语法（建议评估排期，S1）**。`page { url: u, ..old }`：函数式更新糖，Rust `..base` 先例，TS/Swift spread 的同型。与 D2 COW 协同是卖点：base 唯一拥有时原地复用（FBIP），`..old` 按 affine 规则消耗（copyable 则拷）。填补"默认不可变 + 转换链"生态里高频的记录更新场景，减少 `var` 中间名。风险：与匿名记录字面量（deferred）的设计交互，先 pin 语义再排期。教程重配一行。
- **P3 `in` 成员资格终裁（M3 收口项）**。方案 A：`in` 保持 range 专属，`xs.contains(x)` 是集合成员的规范拼写，拒绝错误的建议信息直接给出 `contains`；方案 B：对 cursor trait 全面成员资格（Kotlin 式 `x in xs`）。当前"checker 拒绝非 range 右侧 + 未裁决"是最差状态。建议 M3 list surface 收口时以语料频率定；无论哪个方案，拒绝错误必须可操作。
- **P4 lambda 参数推断（实现里程碑，非糖）**。语例全部 `(x: int) => x * 2`，对照 TS/Kotlin/Rust 均从期望类型推断。B2 已裁决 lambda 推断，这是排期问题；对"TS+bun 人体工学"目标影响最大的单项。落地后语例全部去注解。无括号单参 lambda（`x => e`）建议不做，等 `it` 的语料证据。
- **P5 `debug(x)` 插值调试形态（建议采纳，零语法成本）**。union/optional 操作数进插值是错误（原则正确），printf 调试是真实需求。@std 提供 `debug(x)`：任意类型给出诊断形态（union 展开 tag 名），插值错误信息附建议 "extract first or use debug(x)"。Rust `{x:?}` 的库级对应物。
- **P6 `??` 维持 deferred，写明触发条件**。F8 退役 `or` 的论证干净（一词六义 + 上下文推断 + 与 `||` 冲突）。`??` 限定 `t | none` 是记录在案的阀门 [B4/F8]；拒绝了 `?.` 生态后它单独存在的必要性下降。触发条件：`unwrap_or` 落地、语例刷新后仍有 fallback 痛点的语料证据。
- **P7 comptime-pure 默认参数（记录为 valve，不进 v1）**。字面量限定 [F7] 根治了 Python 可变默认陷阱与隐藏求值；痛点是无重载下的表达式默认（`level: string = env("LOG")` 只能 wrapper）。备选：comptime-pure 表达式默认（编译期可求值、无副作用），比字面量宽、比任意表达式窄。记入 backlog 即可。

## 6. 词汇表与命名审计

1. **`.err`/`.ok` 的侧判定规则**。教程只写"结果类型 = 输入联合减 handler 覆盖的成员"，覆盖集如何确定未写明：若由 handler 返回类型反推（返回 config 则覆盖 io|parse），这是 F8 退役 `or` 时点名的 wanted-member 上下文推断的近亲；若由 handler 参数类型驱动则干净。建议 pin 判定规则并写进教程，这是当前表面里唯一一处"读代码不能确定语义发生在哪侧"的推理点。
2. **`.err(h)` 与 `unwrap_or_else` 同构**（h 都拿到错误值并给出 fallback），词汇冗余。建议二选一：保留 `.err(h)` 为主（差集类型是独特价值），`unwrap_or_else` 删除或作别名；或反之。教程给提取族一张五行对照表（`?` / `unwrap_or` / `ok_or` / `map_err` / `.err`），`.ok(h)` 的命名对 Rust 读者有"转 option"的错误预期，若改名（如按"处理 ok 侧"直命名）一并定。
3. **`list_` 前缀是过渡还是终态**。UFCS + 无重载下，多类型竞争的操作名要么泛型（combinator 走 cursor trait）要么前缀。终态建议：trait 方法承载各容器操作（`push` 经各自 trait 解析），前缀仅留给无处安放的；否则 stdlib 长成 C 风格前缀 API（list_push/list_get/map_get/...），直接伤害 TS+bun 人体工学目标。M3/M6 裁决项。
4. **字符串化三入口**。`str(e)`（插值 desugar）、`show` derive（授权插值）、`debug(x)`（P5）一句话分工写清。

## 7. 维持的拒绝（背书，防回潮）

- 无 truthiness：Kotlin/TS 的 `if x` 三义是对照卖点。
- 无 `?.`/elvis/`!!`：`is` 细化 + `none` 类型化 + 提取族覆盖全部场景。
- 无三元：`if` 表达式。
- 无链式比较、无推导式：组合子面覆盖。
- 无用户运算符、词形运算符仅 `in`/`is`、冻结优先级表：可预测性收益大于表达损失。
- `or` 退役 [F8]：一词六义的论证干净。
- 无重载：双命名空间 + 泛型覆盖。
- `|>` 维持 deferred：UFCS 链已覆盖，Roc 删除先例。
- 无分片糖：UTF-8 边界理由成立，检查库调用可接受；这是对脚本人群最痛的拒绝之一，教程 absent-list 的现有解释够用。
- `it` 简写、匿名记录字面量、`@` binder、标签 break/continue：维持 deferred 排序。

## 8. 行动清单

1. 立即（零成本）：tutorial.md 补录复合赋值、`fn` 类型拼写、`else if`、遮蔽规则；design-decisions.md 同步记档（复合赋值进 F7 表，其余进对应节）。
2. M3 收口：`in` 成员资格终裁（P3）；`list_` 前缀终态裁决（6.3）；`.err`/`.ok` 侧判定 pin（6.1）；`parse_int` 语例修正（4.5）。
3. backlog 增补（design-decisions 第 11 节）：类型测试模式（P1）、struct 更新语法（P2）、comptime-pure 默认参数（P7）、lambda 控制流语义、nursery 值语义、空集合字面量推断、负数模式、元组再赋值拒绝、Unicode 标识符策略。
4. 语例刷新（提取族落地后）：`unwrap_or` 替换 `.ok(id).err(0)` 链、去掉 `-> ()`、lambda 去参数注解。
