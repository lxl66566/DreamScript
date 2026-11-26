# DreamScript 现实使用局限评估（2026-11-24）

非规范性评审文档：基于当日仓库状态（`137762b1`）的一份外部视角评估，不属于 `docs/en/` 的裁决记录。语言表面或实现边界变化后，本文结论可能失效。

## 结论

这套设计在“目标、裁决、实现”三者之间的一致性很高：集合论类型、仿射默认加可选线性层、Perceus RC 无 GC、nursery 结构化并发、错误即并集成员、comptime 取代宏，彼此咬合，没有明显互相打架的特性。实现广度也很大：19 个 crate、约 26.7 万行 Rust、M0–M7 全链路加 LSP/包管理/热重载/fuzz。

现实使用的局限不在某条语法，而在四个硬点：性能承诺的载体（JIT）不存在、并发与 IO 实际是单线程且 Linux-only、所有权模型删掉了真实程序最常用的别名与原地更新、泛型深度只到一个机器字。再叠加上薄标准库、封闭的 derive、半成品工具链和决策日志式文档，它今天更像作者自用的工具链，而不是可承接项目的通用语言。

一句话：设计值得抄，实现值得读，产品还不能用。

## 1. 性能：承诺是编译语言，现实是略逊于 CPython

仓库自己提交的基准（`benchmarks/results/2026-11-21-kernels.jsonl`，release 构建）里，ds 对 CPython 3.14：

| 内核 | ds | python | 结论 |
|---|---:|---:|---|
| fib(34) | 993ms | 431ms | 慢 2.3x |
| closures | 856ms | 563ms | 慢 1.5x |
| lists | 755ms | 394ms | 慢 1.9x |
| combinators | 472ms | 241ms | 慢 2.0x |
| maps | 67ms | 36ms | 慢 1.9x |
| strings | 54ms | 28ms | 慢 1.9x |
| records | 276ms | 246ms | 慢 1.1x |
| loops-arith | 581ms | 1875ms | 快 3.2x |
| float-loop | 433ms | 822ms | 快 1.9x |
| containers | 161ms | 267ms | 快 1.7x |

node 在所有内核上快 4–14 倍。设计目标是 tier 1 达到 C 的 3–10x，而 CPython 大约是 C 的 30–60x，所以今天的 tier 1 大概落在 C 的 15–30x，离目标一个数量级。tier 2/3 的 `ds-jit` crate 根本不存在（`crates/` 下没有），copy-and-patch、Cranelift、wasm 全是纸面设计。

现实含义：“脚本语言人体工学加编译语言性能”目前只兑现了前半句。用 ds 替换 Python/Node 写脚本，只有纯整数或浮点热循环会更快，调用密集和分配密集的代码更慢。这不是调优问题，是“没有 JIT”的结构问题。

正面数据：启动确实好——热路径 4.1ms、冷路径 18.7ms 中位数，符合脚本语言预期。

## 2. 并发与 IO：结构化并发很好，但基本不并行

- 执行器单线程，多线程 fiber 执行器被明确裁决为 post-1.0（`design-decisions.md` E2）。`nursery`/`spawn` 提供的是 IO 并发，不是 CPU 并行。
- `par_map`/`par_reduce` 的 worker pool 只有 `Vm::enable_fork_pool()`（`crates/ds-vm/src/vm.rs:2925`）能打开，而 `ds-cli` 从不调用它，只在测试和 bench 里出现。普通用户跑的 `par_map` 永远是顺序的。
- 即使打开 pool，门禁是“子任务的 entry span 里必须全是标量”。`list[string]` 的元素是一个 `Ref` 字、`list[struct]` 是多字 span，全部回退顺序；`par_reduce` 按裁决本身就是顺序 fold。README/tutorial 里的 `pages.par_map(...)` 示例正好是 struct 元素，双重顺序。
- 共享状态被设计性禁止：没有 lock/mutex/atomic，fiber 之间只有一个有界 channel，而且 payload 只能是一个字，多字或带 deinit 的 payload 在 send 时直接拒绝（`todo.md` 80）。做缓存、连接池、共享索引这类真实服务端结构只能靠消息传递重新发明。
- reactor 只有 Linux（epoll）。kqueue/IOCP 还在队列里（`todo.md` 79），所以 macOS/Windows 上 net/file 操作退回阻塞 twin，fiber 会被真正卡住。`platform "web"` 没有 wasm 宿主。跨平台在这里是“能编译运行”，不是“行为等价”。
- `timeout` 组合子不存在（`todo.md` 74/97），只有 `firstof`/`interleave`；channel 的 send/recv 也没有 deadline。没有超时的网络代码在工程上不可用。
- total 拼写会直接终止进程：`write_file`、`fs_list`、`file_size`、`net_connect`、`net_listen`、`accept` 失败时按 H5.5 打印错误行并 halt。服务端最需要处理的“端口被占用”“握手失败”恰恰没有非终止拼写（`todo.md` 76 明确仍开放）。写文件失败也一样崩。
- 更麻烦的是 `die`、`mkdir`、`remove`、`fs_size`、`exit` 是 CLI host 的 provision，未 provisioning 的编译里这些名字是 unknown，所以库作者写不出创建目录、删除文件、主动 panic 的代码，只有入口脚本能用。这不是能力边界，是生态边界。

## 3. 所有权模型：换掉了真实程序最常用的表达方式

设计上这是最有野心的一块，也是现实摩擦最大的地方。

- 没有一等引用（C2 明确永久排除），借用不能逃逸调用，检查是过程内的。于是无法返回指向参数的视图、无法写自引用结构、无法表达环状图、无法持有跨调用的游标或迭代器、无法做零拷贝解析（`substring` 返回新 `string`）。真实代码里“解析器加游标”“GUI 节点树”“ECS 里互相引用”全都要退化成 id 索引加查表。
- 没有字段赋值，也没有下标赋值。扫描全部 `.ds` 源（examples/conformance/stdlib/测试）后，`self.x = ...`、`p.x = ...`、`xs[i] = v` 一次都没出现；D1 规定变更只能是对 owned receiver 的具名调用。而 list 的成员只有 `get`/`push`/`concat`，map 只有 `get`/`set`/`keys`：没有 `set` 元素、remove、insert、clear、entries 迭代，连 `m.keys()` 是不是最终接口都还没裁决（`todo.md` 102）。所谓就地变更实际是“重建值再 re-store，唯一持有时运行时复用分配”。
  
  现实后果：更新 `list[record]` 里某个字段要 `map` 重建整张表；嵌套 map 更新要手工拼回。这不是“少点语法糖”，是把最常见的数据操作变成 O(n) 重写。
- 闭包没有可变 upvalue（F2 裁决）。`map` 里累加计数器、注册事件回调、写解析器状态机，都得靠 channel 或把状态穿进参数。`CLOSURE_NEW` 还在创建时复制捕获（`benchmarks/README.md` 明确写了这一点），捕获大对象有额外代价。
- 泛型函数的值、泛型 lambda、多字实例的 entering site 全部拒绝（tutorial、`todo.md` 52）。所以“仿射加泛型”的组合在真实抽象里很快撞墙。
- FFI 只支持不超过 4 个 C 字、`int/bool/float/string/bytes` 参数、无字符串返回、无结构体、无回调。绑 sqlite、图形、加解密库，必须手写 Rust artifact、手工算 content hash 写进 `[artifacts]`，而且没有 `[build]` 源码构建（deferred）。用不了现成库是生态冷启动最硬的墙。

## 4. 类型系统与泛型：漂亮，但深度不够写库

- 泛型 evidence 是“一个字”，所以多于一个字的实例在 entering site 拒绝；容器抽象（`t` 是用户 struct 或 tuple）基本不可用。这正是 stdlib 自己写不出 `flat_map`、`scan`、`compose` 的原因（`todo.md` 81），而这些 bodies 要等 per-instantiation frames（`todo.md` 6）。
- derive 词汇表永久封闭给内建实现（H5.5），第三方 derive 被永久拒绝；宏也永久拒绝。结果是：没有 serde 式生态、没有测试框架的自定义属性、没有 ORM 或序列化格式扩展。toml/yaml 只能作为“实现 driver trait 的普通包”存在，而 driver 的 ambient trait 跨包绑定还在 `todo.md` 77。
- 运行时没有类型元数据（G2），反射只在 comptime。于是动态插件、按名字构造值、通用（反）序列化都没有；异构集合只能 `box Json`。代价是可解释、可优化，收益是不可扩展。
- 一致性规则是“同一 trait 的实例头两两不交”，且无 orphan rule、link 时复查。这意味着任意包都能给任意类型加 impl，两个互不认识的依赖可以各自给同一头加 impl，冲突在链接期才炸——对一个包生态来说这是版本兼容的定时炸弹。同时“无特化”意味着不能给 `list[int]` 写快速路径，性能优化只能进运行时。
- 错误是返回并集的成员：`.err`/`.ok` 的差集类型确实优雅，但每个上游签名都要把成员写全，新增一类错误会波及整条调用链；没有 `Error` trait 或 `Result<T, E>` 统一别名，也没有 `any`。库 API 演进成本比 Rust 更高。
- panic 不可捕获、错误值不带 backtrace（“traces belong to the trap layer”，M9 才排期），trap 只有一行 message。生产排障基本靠猜。

## 5. 标准库、生态、工具链

- 整个标准库源代码约 1.4k 行（`crates/ds-stdlib/src/`，含 616 行 JSON），API 面大致是 list 三个成员、map 三个成员、三十来个字符串/文件/网络/进程函数、八个浮点函数。没有 map 字面量、没有 map 迭代、没有正则或 glob（`todo.md` 101）、没有 Dec 或定点数、没有 bigint、没有日志裁决（`todo.md` 102）、没有测试框架或 `assert`（`trap` 还是 ambient 私有）、没有 HTTP server。
- 数值面只有 `i64/f64`，`byte/u32` 要过 checked 桥；`abs` 只有 float，float 的 `max/min` 还在队列里（`todo.md` 53），没有 `round/trunc/log/exp/tan`。整数溢出默认 trap，哈希或 PRNG 必须满屏 `+%`。数值和 AI 明确不在 v1 形状内。
- 字符串是 char 索引 O(n)、`substring` 分配、无 builder；没有任何格式化能力（插值只是 `str(e)`），做 CLI 输出时连“保留两位小数、对齐、十六进制”都写不出来。
- 包管理：git-over-https registry、一个名字一个版本、manifest 即 lock、无源码构建、原生库手工发布。可用的第三方生态基本为零。
- 工具链：没有 debugger、没有 profiler、没有 `ds test`、没有文档生成；LSP 缺跨包 rename（`todo.md` 54）；REPL 每次提交重新 lower 整个模块（`todo.md` 49）。
- 语言还在 A3“1.0 前快速破坏”阶段，且 44 个决策点几乎每天在修订（文档日期从 11-08 到 11-24）。现在投入生产等于押注一个仍在高速变形的表面。

## 6. 一个被低估的问题：文档与可维护性

`tutorial.md` 写得很好，477 行覆盖全部 v1 表面，这是真本事。但它是唯一的用户文档，而标准库没有参考手册，只能读 `ambient.ds`——那里的注释是“2026-11-15 ruling”“item 74”“the seam”这类内部决策语言。更矛盾的是，仓库自己的 `AGENTS.md` 禁止代码注释依赖外部文档或章节编号，而实现里大量注释正是“裁决日期加 todo 编号加 seam 名”。

同时规模是：2026-11-09 到 11-24，16 天，164 个提交，26.7 万行 Rust。这不可能是逐行人工验证的产物，实际质量保证是 conformance 语料（约 40 个文件）加 fuzz 加内部 gate。也就是说，广度是断言出来的，深度是抽样验证的：真实大程序、长会话、错误路径、跨包组合下的行为还没有被外部使用检验过。Bus factor 是 1，且知识以决策日志形式存在。

## 7. 日常语法摩擦

`?.`、`??`、`|>`、`@` binder、labeled break/continue、`it` 简写全在 backlog；`return` 不能作 arm 值（DS2039）；guard 不能 own scrutinee；嵌套 tuple/member 子模式拒绝；`V | none` 有歧义；`none` 单独作字段类型拒绝（DS2042）；`()` 作字段类型拒绝；`u32(x)` 这样的直觉拼写被永久拒绝，必须 `u32.of(x)`；数值字面量后缀在 lowering 拒绝。单条都不致命，合起来是“写两百行就要查一次文档”的税率。

## 适用场景判断

- 适合：单文件 CLI 或工具脚本、构建脚本、文本与 JSON 处理、作者自己的工具链、需要热重载的开发循环、演示仿射加结构化并发的样例。启动快、错误信息是两 span 线性诊断且带 JSON 格式、TCO 有保证、无 GC 停顿，这些在脚本场景是真实优势。
- 不适合：网络服务（无 timeout、accept 会崩、单线程、Linux-only reactor、无共享状态）、GUI 或游戏（无引用、别名、环，无并行，无字段更新，无图形库绑定）、数值或 AI（无 JIT、无数组原语、无 bigint 或 Dec）、需要第三方库的项目（FFI 与包生态）、多人长期维护的项目（快速破坏加决策日志式文档加 bus factor 1）。

## 关键改进杠杆

1. 把 tier 2 copy-and-patch 做出来并公布对 C/Python 的对比，或者把“编译语言性能”从 README 降级为路线图。当前数据会被任何一个评估者第一眼证伪。
2. 让 `par_map` 默认可用且覆盖 `Ref` 元素，同时给 channel 加多字 payload 和一个 `timeout` 或 deadline。否则 fiber 加 nursery 这套最贵的设计在真实 IO 代码里用不上。
3. 补上原地更新与集合基本操作：字段或下标赋值（或 `with` 式 lens）、`list.set/remove/insert`、map 的 `remove/entries/contains`、map 字面量。这是能不能写业务代码的分界线，比再加一个类型特性重要。
4. 把 `die`/`mkdir`/`remove`/`fs_size` 从 CLI provision 移到 `@std`，并给 `write_file`、`accept` 一类提供非终止拼写。库作者不能用这些 API，等于生态从第一天就残废。
5. 写用户文档并向内收口内部代号：一份从 `.dsi` 生成的 stdlib 参考、一份错误码手册、把注释里的“裁决日期加 item 编号”换成行为描述。外部采用者不会读 271 行决策日志。

## 证据来源

- `README.md`、`docs/en/tutorial.md`（表面与承诺）
- `docs/en/design-decisions.md`（A1–H6 裁决、E2/E3、C1–C5、D1/D2、G2、H5.5）
- `docs/en/status.md`（lowered subset 的精确边界与永久拒绝类）
- `docs/en/todo.md`（开放项 6、39、45、49、52、53、54、58、63、74、76、79、80、81、101、102）
- `docs/en/implementation-plan.md`（M8 未开始、风险与 scope guard）
- `benchmarks/results/2026-11-21-kernels.jsonl`、`2026-11-21-startup.jsonl`、`benchmarks/README.md`
- `crates/ds-stdlib/src/*.ds`（ambient 744 行、json 616 行、bodied 207 行等）
- `crates/ds-vm/src/vm.rs`（`enable_fork_pool` 的唯一入口）
- `git log`（首次提交 2026-11-09，164 个提交，26.7 万行 Rust）
