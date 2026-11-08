# DreamScript 决策地图

本文件是 DreamScript 语言设计的主工作表。语言的全部决策点都在这里列出，每个决策点附带选项、其目录文档给出的推荐，以及完整分析所在的位置。状态：阶段 3 已完成——每行都带 Decision（记录于 2026-11-08）；带分期与已解决开放问题的完整决策记录是 `docs/en/design/08-decisions.md`（中文版 `docs/zh/design/08-decisions.md`），逐点详细依据是 `docs/en/design/09-decision-rationale.md`（中文版 `docs/zh/design/09-decision-rationale.md`）。

阅读指南：每行按目录文档给出的顺序列出选项，字母与文档保持一致。「Rec」列是目录给出的推荐，附一句理由；「Decision」列是作者的阶段 3 裁决，与 08/09 文档同步。标记为 likely-rejected 的选项只在分析中作为参照点保留。

## 文档索引

| 文件 | 领域 | 决策点 |
|---|---|---|
| docs/zh/design/01-types.md | 类型系统 | B1, B2, B3, B4, B7, B8, B9 |
| docs/zh/design/02-linearity-ownership.md | 线性性、所有权、内存、数据 | C1-C5, D1-D3 |
| docs/zh/design/03-effects-errors-async.md | 效应、错误、异步、并行 | B5, E1-E5 |
| docs/zh/design/04-syntax.md | 语法表层与语法糖预算 | F1-F7 |
| docs/zh/design/05-generics-metaprogramming.md | 多态、元编程 | B6, G1, G2, H3 |
| docs/zh/design/06-modules-tooling.md | 模块、可见性、FFI、生态 | G3, G4, H5, H6 |
| docs/zh/design/07-runtime.md | 执行管线、TCO、内存运行时 | H1, H2, H4 |
| docs/zh/design/08-decisions.md | 阶段 3 决策记录 | 全部 |
| docs/zh/design/09-decision-rationale.md | 阶段 3 决策依据 | 全部 |

每条结论背后的原始研究：docs/zh/research/01-06（按领域整理的研究笔记，附一手来源引用）。

## A. 定位（在本文件内决定，没有单独的目录）

- A1 定位：(a) 独立通用的脚本语言、(b) 嵌入优先，一切由宿主提供，即平台模型、(c) 独立内核，再加一层可嵌入的平台模式、(d) 面向 web/wasm。推荐 (c)。**裁决：(c)**——性能章程要求有真正的独立运行时；平台模式随后就是「导出效应处理器的包」（docs/zh/design/06 H5b），顺带拿到 Lua 那类嵌入定位。
- A2 主要工作负载，可多选：(a) CLI/工具/脚本、(b) web 后端、(c) 游戏/仿真，需要热重载、(d) AI/数值，Mojo 的定位。推荐 (a) 优先，(c) 其次。**裁决：(a) 为主、(c) 为次**——两者都由解释器优先的管线和热重载工具支撑；(d) 仍可经 H1a 的 Cranelift 层到达，不反向塑造 v1。
- A3 兼容策略：(a) v0 快速破坏，兼容承诺留到 1.0 之后，Roc/MoonBit 的姿态、(b) 尽早执行 semver 纪律，Cargo 的姿态、(c) 永远内容寻址，Unison 的姿态。推荐 1.0 之前用 (a)，之后用 (b)。**裁决：(a) 之后 (b)**；(c) 被否决作为源码级身份模型，在包一级采纳（H6b）——收益在那里真实，文化代价在那里不存在。

## B. 类型（docs/zh/design/01-types.md）

| ID | 问题 | 选项 | Rec | Decision |
|---|---|---|---|---|
| B1 | 类型系统基础 | (a) 集合论结构化内核、(b) 名义 ADT 内核、(c) 混合：名义标签本身按集合论组合，即在具名类型上做并集、交集、取反、(d) 旧 README 的 set 声明加 `any` | (c) | **(c)**；`any` 回退否决；(b) 记录为不相交机制失控时的可逆回退 |
| B2 | 推断 | (a) 双向推断加边界标注、(b) 完整 HM、(c) 在双向推断骨架上做精化、(d) 处处标注 | (c) | **(c) 叠加在 (a) 上** |
| B3 | 积类型与和类型 | (a) 匿名结构化类型、(b) 名义 structs/enums、(c) 可扩展行、(d) structs/enums 加元组，匿名字面量作为语法糖 | (d) | **(d)**；匿名 record 语法糖延后增量（F7 第 25 行） |
| B4 | 可空性 | (a) `T \| none` 并集纪律、(b) 语言级 null 加强制流类型、(c) (a) 的语义，表面写成零成本的 `T?` | (c) | **(c)**；v1 无 `?.` |
| B7 | 数值塔 | (a) 默认 Int(i64)/Float(f64)，带后缀，溢出即陷入并可用显式 `+%`、(b) 默认 bigint、(c) 完整的机器数值塔、(d) (a) 加作为库的 decimal | (a) | **(a)** 加 (d) 的 decimal 库路径 |
| B8 | 字符串 | (a) 不可变 UTF-8 字节，切片检查后类型为 `String \| none`，字素操作放在标准库、(b) 驻留字符串加 builder、(c) UTF-16、(d) (a) 加 rope | (a) | **(a)**；rope 作为未来库类型 |
| B9 | 相等/排序/哈希 | (a) 通用结构化 `==`，配五处全体性例外、(b) 由 trait 中介、(c) 内建加 derive、(d) 引用身份 | (a) | **(a)**；float 按位结构比较配具名 `is_float_eq`；Ord/Hash/Show 走 derive |

## C/D. 线性性、所有权、内存、数据（docs/zh/design/02-linearity-ownership.md）

| ID | 问题 | 选项 | Rec | Decision |
|---|---|---|---|---|
| C1 | 线性性的形态 | (a) 显式线性作为默认、(b) 仿射内核加可选的 `~Copyable` 线性资源层、(c) 完全隐藏线性性、(d) 单一所有权加运行时借用，再加可选静态线性 | (b) | **(b)**；(c) 的推断分期为后续纯优化，(d) 的借用计数分期为 v0 诊断层 |
| C2 | 借用 | (a) 只做移动与复制、(b) 编译器不可见的推断、(c) 运行时借用计数、(d) 二等参数传递模式（Hylo 的 `let/inout/sink`，Swift 的 `borrowing/consuming`） | (d) | **(d)** |
| C3 | 不可变性机制 | (a) val/var，var 仅限局部、(b) val/var 加静态保证的排他窗口、(c) 再加带类型的内部可变性、(d) 不可变内核加 FBIP | (b) | **(b)**，以 (d) 的 FBIP 作为底下的运行时保证 |
| C4 | 内存策略 | (a) Perceus 精确 RC 加块复用、(b) RC 加带代引用、(c) 追踪式 GC、(d) 以 arena 为主，在逃逸边界用 RC | (a)+(d) | **(a)+(d)**；v0 在类型级拒绝环 |
| C5 | 移动/丢弃/确定初始化 | (a) 隐式丢弃、(b) 显式 `drop` 加确定初始化检查、(c) RAII 析构限定在 `~Copyable` 层、(d) 效应处理器作终结器 | (c) | **(c)**；DI 作基础设施；显式 `drop` 仅作调试/FFI 工具 |
| D1 | 可变性收敛 | (a) 仅函数局部 var、(b) 排他窗口内可变、(c) 由能力门控的可变、(d) 纯表层 | (b) | **(b)** |
| D2 | 集合 | (a) 只用持久化集合、(b) 线性可变的 COW，函数式 API，唯一时原地改、(c) 双模式、(d) 务实的混合 | (b) | **(b)**；(d) 是它的标准库结果，不是独立分类规则 |
| D3 | 迭代器 | (a) 线性游标、(b) 一等生成器、(c) 只用组合子、(d) 组合子加隐式生成器，做融合 | (d) | **(d)**；1.0 无公开生成器语法，`-*>'` 退役 |

## E. 错误、效应、异步、并行（docs/zh/design/03-effects-errors-async.md）

| ID | 问题 | 选项 | Rec | Decision |
|---|---|---|---|---|
| B5 | 签名如何携带错误 | (a) 推断出的标签并集（Roc）、(b) 显式 `Result<T,E>`、(c) 效应行、(d) 以并集为载体，之后可选加行 | (d) | **(d)**；`throws(...)` 留作后续增量语法糖 |
| E1 | 效应系统 | (a) 完整的行多态可恢复效应、(b) 精简能力集，只有一个 IO 能力、(c) 能力参数，不带效应语法、(d) 推迟：当前用 Result 加 panic，建立在传递证据的处理器运行时上 | (d) | **(d)** 配 (c) 作标准库惯用法；脚本用环境默认处理器，测试/嵌入用显式能力 |
| E2 | 异步模型 | (a) 异步即效应，没有 `await`（Koka）、(b) VM 纤程加强制结构化并发（nursery/Job 树）、(c) 无结构的栈式 spawn（Go）、(d) 带颜色的 async/await（TS） | (b) | **(b)** |
| E3 | 并行 | (a) 用可发送性类型让共享内存变安全、(b) 在 (a) 之上做 fork-join 数据并行、(c) actor/隔离、(d) 单线程加 FFI 并行 | (b) 建立在 (a) 之上 | **(b) 建立在 (a) 之上** |
| E4 | 错误处理 | (a) Result 与无法处理的陷入分开、(b) 并集错误加 `?`/`or`/`.err` 三套集合差集处理器、(c) 异常即效应、(d) let-it-crash 监督层 | (b) 加 (a) 的陷入 | **(b) 加 (a) 的陷入**；(d) 作为库模式 |
| E5 | 取消/资源 | (a) nursery 作用域内的协作式取消、(b) 线性消耗即关闭、(c) defer/errdefer 块、(d) 超时即处理器 | (a)+(b) | **(a)+(b)**；(c) 作语法糖，(d) 作库 |

## F. 语法（docs/zh/design/04-syntax.md）

| ID | 问题 | 选项 | Rec | Decision |
|---|---|---|---|---|
| F1 | 整体形态 | (a) TS 系花括号、(b) Gleam/Roc 花括号、(c) Koka 缩进、(d) Rust 系 | (a) | **(a)**；(b) 是已记录的解析器回退 |
| F2 | 绑定/函数/返回 | `const`/`var`。具名函数写成 const 箭头函数。`name: T` 加 `-> T`。末表达式返回，配合 `or return`。README 里的 `?` 返回标记退役，`?` 保留给值传播 | 如上述 | **如上述**；`?` 保留给传播 |
| F3 | UFCS/管道 | (a) Koka 的完整 UFCS、(b) 在 (a) 上加 Gleam 的首参 `\|>`、(c) 仅方法调用、(d) 管道优先 | (a) | **(a)**；`\|>` 延后增量；能力参数永不进入点位置 |
| F4 | 模式匹配 | (a) Rust 形态：守卫、或模式、穷尽检查并给出集合差集诊断（「missing: E2 \| E3」）、(b) Kotlin when 守卫、(c) 无守卫（Gleam）、(d) OCaml when/as | (a) | **(a)**；`@` 绑定器排除出 v1，延后增量 |
| F5 | 运算符 | (a) 固定的、由 trait 驱动的集合，C 系优先级冻结、(b) 关闭重载、(c) 自由重载与自定义运算符、(d) comptime 可扩展 | (a) | **(a)** 加 Zig 风格 `+% -% *%` 溢出写法；优先级表冻结 |
| F6 | 字符串/字面量 | (a) `$x`/`${expr}`、(b) `\(x)`、(c) `{x}` 格式化函数、(d) 都不做 | (a) | **(a)** 加 `r#"..."#` 原始字符串、`'x'`/`b'x'` 字面量、显式 `str()` 边界 |
| F7 | 语法糖预算 | Minimal 12 项/18u、Balanced 22/35u、Generous 26/40u，对照 Kotlin 的 38/74-76u 上限 | Balanced | **Balanced**（22/35u）；余量预留给 `\|>`、record 语法糖、`@` 绑定器、vararg |

## G. 多态、元编程、模块（docs/zh/design/05 + 06）

| ID | 问题 | 选项 | Rec | Decision |
|---|---|---|---|---|
| B6 | 多态机制 | (a) 一致的集合论 trait，实例头互不重叠，用 `T1 & T2 = Empty` 检查替代孤儿规则、(b) 显式字典传递、(c) ML 模块、(d) comptime 泛型 | (a) | **(a)**；外来类型实例合法，不相交在声明与链接期检查；(b) 的带作用域具名实例延后增量 |
| G1 | 元编程 | (a) 除 derive 之外不做、(b) comptime 求值加静态反射，不做宏、(c) 卫生宏、(d) proc 宏 | (b) | **(b)**；derive 先行，comptime 求值器加反射在 1.0 前补齐 |
| G2 | 一等类型 | (a) 类型作为运行时值、(b) 只在 comptime 反射、(c) 运行时元数据表、(d) 标签即数据 | (b) 加 (d) | **(b) 加 (d)** |
| H3 | 泛型执行 | (a) 一份函数体加证据词，加多态内联缓存，加惰性的 JIT 特化、(b) 字节码单态化缓存、(c) 擦除加装箱、(d) comptime 单态化 | (a) | **(a)**；1.0 无 HKT/GAT，变异性按集合包含 |
| G3 | 模块系统 | (a) Rust crate 树、(b) MoonBit 风格的分层，层级为定义 < 文件 < 包 < 模块，生成 `.dsi` 接口，`///\|` 顺序无关块，包内循环导入合法，跨包禁止，函数体纯编译期、(c) Unison 内容寻址、(d) TS/ESM | (b) | **(b)**；`///\|` 为可选分隔符，G3.5 dsmod 契约生效 |
| G4 | 可见性 | (a) pub/priv、(b) 分层加选择性导出、(c) 胶囊、(d) Go 约定 | (b) | **(b)**：private / `pub(pkg)` / `pub` 加选择性导入 |
| H5 | FFI/嵌入 | (a) 线性类型的 C 句柄加句柄表嵌入，双重释放是类型错误、(b) 平台即导出处理器的包、(c) 手写 `extern`、(d) wasm 优先 | (a) 加 (b) | **(a) 作基底叠加 (b)**；(c) 作为有文档记录的逃生口；pinning 策略已在 08 解决 |
| H6 | 生态栈 | (a) Cargo 风格、(b) 原生哈希存储，blake3、输出哈希作键、按包锁级联、FHS profile 链接、注册表作为静态 JSON 索引、(c) bun 风格零配置脚本、(d) 平台优先 | (b) 加 (c) | **(b) 为核心加 (c) 脚本模式**；五项必须兑现的工具承诺从第一天起生效 |

## H. 运行时（docs/zh/design/07-runtime.md）

| ID | 问题 | 选项 | Rec | Decision |
|---|---|---|---|---|
| H1 | 执行管线 | (a) tier1 带类型寄存器 VM 加 tier2 copy-and-patch 加 tier3 Cranelift/wasm，共享一套 SSA 降级、(b) 寄存器 VM 加单一方法 JIT、(c) AST 解释器加元追踪、(d) wasm-AOT 为主 | (a) | **(a)**；(b) 记录为模板工具链不可接受时的回退 |
| H2 | 尾调用 | (a) 从第一天起把 TCO 写进规范并保证生效，覆盖自身与互递归，栈帧复用加槽位叠加 ABI、(b) 尽力而为、(c) 用户层蹦床、(d) 只做效应侧的尾式恢复 | (a) | **(a)**；限定于同一效应上下文内 |
| H4 | 内存运行时 | (a) Perceus RC 加块复用加逐帧 arena、(b) 带代搬移式 GC、(c) malloc/free，不做 GC、(d) 混合并带延迟收集器 | (a) | **(a)**，以 (d) 的双钩子收集器接口作为已承诺的增长路径 |

所有 H1 选项共享的固定运行时基线（docs/zh/design/07 的「Fixed baseline」）：带类型的 32 位寄存器字节码、拆箱槽位，NaN-boxing 弃用、computed-goto/musttail 分派、由 profile 驱动的超级指令、从第一天起就做 uop 分解、可 mmap 的 dsmod 字节码格式。按阶段 3 裁决作为固定输入生效。

## 定稿的语言（阶段 3，全部行已解决）

一门独立的脚本语言，可通过平台包嵌入。类型用混合的集合论方案：具名标签，并集、交集、取反，`is` 精化，`T?` 作为并集之上的语法糖。所有权用仿射移动加可选的 `~Copyable` 线性资源层，配二等参数模式与排他窗口内的可变。相等性是通用结构化比较，配五处例外。内存用 Perceus 引用计数，加块复用与逐帧 arena。trait 保持一致，实例头互不重叠，泛型是一份函数体配证据词与内联缓存。元编程只做 comptime 反射，宏整层省去。错误由并集携带，用 `?`/`or`/`.err` 处理，无法处理的陷入走单独一条路径。并发在强制 nursery 的 VM 纤程上，配可发送性基底上的 fork-join。效应留在表层之下，垫在传递证据的处理器运行时与能力参数 IO 上。语法是 TS 系花括号加 Koka 的 UFCS，语法糖预算为 22 项特性、35 单位。模块是 MoonBit 风格的分层，接口自动生成，导入时不执行。包管理器用 blake3 哈希存储，带零配置脚本模式。FFI 用线性类型的 C-ABI 句柄。运行时分三层：带类型寄存器 VM、copy-and-patch JIT、Cranelift/wasm，尾调用由规范保证，覆盖自身与互递归。完整记录、分期计划与已解决的开放问题：docs/zh/design/08-decisions.md。

## 开放问题（已在阶段 3 解决）

全部在 docs/zh/design/08-decisions.md「已解决的开放问题」中解决：FFI pinning 策略（调用作用域借用、线性句柄、阻塞调用钉住纤程、载体线程逃生）；nursery 块形态（`nursery { }` 加 `spawn { }`）与定稿的 `?`/`or`/`is`/`.err` token 集；`.err` 差集类型（已无必要，采纳 B1c）；纤程与隐式生成器（v0 用游标，日后允许共享一套纤程设施，表层零变更）；生成器箭头 `-*>'`（退役）。另解决 docs/zh/design/05 遗留的：带作用域具名实例（延后增量）、不相交/终止需求（链接期检查、错误点名见证类型、拒绝自满足条件实例）、float `==`（按位结构，具名 IEEE 比较）、v1 的 comptime（derive 先行、求值器 1.0 前补齐）、`@` 绑定器（排除出 v1）。

## 决策流程

1. 从上到下读完本地图。凡提供命名的组合处，按文档采用（docs/zh/design/01 的「Hybrid」、02 的「Modes and RC」、03 的「Capability-core」/「Effect-ready」、04 的「TS-heart」、05 的「Coherent-traits」、06 的「Hash-core」、07 的 Combo A），或逐点挑选选项。
2. 通过编辑本文件记录决策：在对应行标出所选字母，未解决的行保持原样。
3. 每行都有决策之后，AGENTS.md 的状态进入阶段 4（实现）。所选组合的示例程序成为语言的第一批测试语料。

流程已完成（2026-11-08）：全部行已解决，全部命名组合按其文档形态采纳（03 取「Effect-ready」），决策与依据记录于 docs/zh/design/08-decisions.md 与 09-decision-rationale.md，首批测试语料列于其中。AGENTS.md 状态已进入阶段 4。
