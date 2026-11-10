# DreamScript 架构与实现导览

> 记录信息：撰写日期 2026-11-11；工作区版本 `159bdf2`（main 分支，工作树干净）；reviewer 模型 GLM-5.3-Flash。本文面向完全不了解本项目的编程语言研究者，目标是讲清 DreamScript 这门语言是怎么被实现的：每个模块做什么、依赖关系如何、核心机制背后的原理。语言语法的权威教程在 `docs/en/tutorial.md`，每个设计决策的权威记录在 `docs/en/design-decisions.md`，本文是纯实现视角的导览。

## 1. 这是一门什么语言

DreamScript 是一门通用解释型语言，其技术画像可以浓缩为一句话：静态集合论类型（union / intersection / difference 组合在 nominal tag 之上）、仿射（affine）默认的所有权、保证自递归与互递归尾调用、精确引用计数加每帧 bump arena（无 GC、无暂停）、栈式 fiber 加强制 nursery 的结构化并发。编译器是一个 Rust Cargo workspace，生成自研的 32 位定长 uop 字节码，由 tier-1 解释器执行；copy-and-patch JIT（tier 2）与 Cranelift/wasm（tier 3）是规划中的后续 tier，共享同一套 lowering。

设计哲学上，这个项目坚持几条对实现影响深远的纪律：解耦优先（每个特性是正交可组合的部件，拒绝函数着色、特化、继承这类纠缠特性）；编译期错误优于运行期错误（没有 `any` 回退，推断失败即编译错误）；可解释性预算（整个 v1 语言面必须能装进一篇短教程，任何加不进教程的特性都不属于这门语言）；以及「先把承重契约锁死再往下盖楼」的工程顺序——slot-overlay 调用约定（`ds-abi`）和固定宽度可 mmap 的 dsmod 模块格式（`ds-mod`）这两个契约在 M0 里程碑就锁定，之后所有层都对着它们编译。

## 2. 从源码到执行：完整流水线

```text
用户 .ds 源文件 + ds-stdlib 的 ambient.ds（内嵌声明层）
        │
        ▼
   ds-parse      词法预扫描（lexscan）→ pest 语法 → span 化 AST（## 块分隔）
        │
        ▼
   ds-query      memoized 查询引擎（修订号 + 依赖边 + 块粒度失效）
        │
        ▼
   ds-check      双命名空间解析 → 集合论类型核 → 双向检查 + 流细化 → 仿射分析 → 效应行
        │
        ▼
   ds-lower      帧布局规划 → 逐函数降级（Perceus 风格 RC 放置）→ 符号指令渲染为 32 位码字
        │
        ▼
   ds-mod        固定宽度、位置无关、可 mmap 的 dsmod 二进制格式（blake3 内容寻址缓存）
        │
        ▼
   ds-vm         tier-1 解释器：typed register VM + slot-overlay 调用约定
                 + 精确 RC 对象堆 + fiber/nursery + 两槽热重载模块表 + 原生函数表
        │
        ▼
   ds-cli        ds run / ds check / ds build 三个子命令把上述环节串成用户入口
```

箭头即依赖方向。数据流与控制流是同一个方向：每一层消费上一层的产物，`ds-cli` 是唯一把这些环节串起来的地方，也是默认的平台提供者（安装 `std` 原生表，让脚本的 `println` 直接能跑）。

## 3. 模块地图与依赖关系

| crate | src 行数 | 一句话职责 |
|---|---|---|
| `ds-diag` | 约 1300 | Span 模型、稳定错误码注册表、诊断的结构化表示与人类/JSON 双渲染 |
| `ds-parse` | 约 4700 | 词法预扫描 + pest 语法 + 带 span 和 trivia 的 AST、`##` 块索引、script 模式语句折叠 |
| `ds-fmt` | 约 1700 | 零配置块感知 formatter，由源码自身行结构驱动布局决策 |
| `ds-query` | 约 1800 | 把编译器做成按需求值的查询库：memoization、失效、续期 |
| `ds-check` | 约 12500 | 集合论类型核、双向检查与流细化、仿射/DI 分析、trait 一致性、效应行 |
| `ds-abi` | 约 3700 | 零依赖契约层：帧布局、slot-overlay 调用约定、尾调用语义、uop 指令编码 |
| `ds-lower` | 约 7900 | typed AST → 帧布局 → uop 字节码，Perceus 风格 RC 放置 |
| `ds-mod` | 约 2000 | dsmod 二进制格式读写、mmap 加载、blake3 哈希、内容寻址缓存路径 |
| `ds-vm` | 约 8100 | tier-1 解释器、对象堆与 RC、fiber/nursery、两槽模块表、原生函数表、TCP net |
| `ds-rt` | 约 1200 | 未接线的内存机器：per-frame arena、headered RC 块、reuse threading（孤儿 crate，见审查报告） |
| `ds-stdlib` | 约 185 | 标准库的静态半：内嵌 `ambient.ds` 声明文本与效应行表 |
| `ds-cli` | 约 925 | `ds` 二进制：run / check / build |

依赖图（箭头读作「依赖」；`ds-abi` 与 `ds-rt` 是零依赖叶子）：

```text
                 ds-diag ────────────────►（serde/serde_json，全仓公共词汇表）
                /   |   \
        ds-parse    |    ds-fmt ─► ds-parse
        /    \      |
  (pest)     ds-check ─► ds-parse
      |        |
      ds-query ┘（消费 ds-check 的分析产物）

  ds-abi ─►（零依赖，纯契约）
      ▲          ▲
      |          |
  ds-lower ─► ds-mod ─► ds-abi
  （dev-dep: ds-vm，仅 native 表 drift 测试）
      |            |
      |            ▼
      |         ds-vm ─► ds-abi, ds-mod
      |
  ds-stdlib ─► ds-check, ds-diag, ds-parse（运行时半体在 ds-vm 的 native 表里）
      |
  ds-cli ─► ds-check, ds-query, ds-stdlib, ds-mod, ds-vm, ds-lower, ds-parse, ds-diag

  ds-rt ─►（零依赖，当前无任何 crate 指向它）
```

两个值得注意的结构事实：其一，`ds-abi` 是全仓最底层 crate 且不含任何运行时状态，它是「规格即代码」——解释器、布局分配器、未来的 JIT 三方必须逐位一致地实现它；其二，`ds-lower` 的产物以 dsmod 文件或内存 `Module` 进入 `ds-vm`，`ds-vm` 不依赖 `ds-lower`，编译与执行靠二进制格式这一层解耦。

## 4. 前端四件套：ds-diag / ds-parse / ds-fmt / ds-query

### 4.1 ds-diag —— 全仓公共的 span 与诊断词汇表

一切位置信息的基础是 `FileId`（注册序号）+ `Span`（半开字节区间 `[start, end)`，构造函数用 const assert 保证不反向）+ `Files`（源码注册表）。`Files` 为每个文件预计算 `line_starts`（每个换行符后的位置），字节偏移到行列的换算用 `partition_point` 做 O(log n)；列按 Unicode scalar 计数；CRLF 的 `\r` 在行边界剥离。核心不变量：span 是对「当前」文件内容的字节偏移，`set_source` 原地换内容并重建行索引，跨 swap 的 span 作废。

错误码由一个宏从单一列表生成 `ErrorCode` 枚举、`DSnnnn` 字符串表和错误类表（1xxx 语法 / 2xxx 类型 / 3xxx 线性 / 4xxx 模块字节码）三个视图，改表必须同 commit 改快照测试——错误码被当作稳定 API 机械地钉住。`Diagnostic` 是 severity + code + message + labels（span 加 primary/secondary 标记加文案）+ notes + suggestions 的 builder 结构，专为「moved here / used again here」的双 span 线性诊断设计。渲染层是纯函数：`render::human` 产出无色文本（gutter 自适应宽度、多行 span 每行一条 caret 线），`render::json` 输出 schema 冻结的紧凑 JSON，两个 golden 测试钉死输出形状。`--error-format=json` 的机器可读契约就落在这里。

### 4.2 ds-parse —— 两级词法、pest 语法、全函数性 AST 构建

解析分三级。第一级是 `lexscan`：在 pest 之前跑一遍迭代式状态机（栈帧：括号 / 字符串 / 插值洞 / char / 原始串 / 注释），职责有二——把 pest 递归下降在对抗性嵌套下的爆栈风险（abort 不可捕获）用 40 层深度上限变成 DS1009 诊断；为 pest furthest-failure 定位无能为力的词法类错误（未闭合字符串、插值洞、原始串 hash 计数、括号错配、孤立 `\r`）给出双 span 精确诊断。`##` 分隔符只在「栈空且行首」时识别，与语法的 `sep_unit` 规则对齐。

第二级是 `grammar.pest`：单一优先级链（assign > range > `||` > `&&` > is > `|` > `^` > `&` > cmp(+in) > shift > add > mul > unary > postfix > primary），换行敏感——语句以换行结束、二元操作符必须与左操作数同行、只有 `.` 链可折行；控制流头用平行的 `expr_nos` 链禁掉结构体字面量与尾随 lambda 块以消歧义；原始串用 pest 的 PUSH/POP 精确配对 hash 数。为防深链爆栈，平铺操作符链有 128 项上限。

第三级是 `convert/`：把 pest pair 树变成 `ast.rs` 的纯数据 AST。核心原则是全函数性——语法接受的形状转换必不 panic，意外形状记诊断加占位符。值级校验也在这层：默认参数仅字面量（DS1012）、顶层 const-lambda 退役（DS1013）、trait/impl 成员仅 fn（DS1010）。注释作为 `Trivia` 挂上 AST（行首注释挂给下一个 item 的 leading，与前项同行挂 trailing，容器内注释上浮到 item 级），保证 formatter 无损。两个语义级转换值得记住：有体的具名 `fn` 折叠成「绑定一个 lambda 的 `Local`」（下游只见统一的具名函数形状）；script 模式把入口文件顶层语句折叠进合成的 `main`。文件被切成 `Vec<Block>`（`##` 块），这是后面查询引擎失效粒度的物理基础。

### 4.3 ds-fmt —— 由源码行结构驱动的零配置打印机

formatter 的布局决策完全由「源码自己的行结构」驱动，不重排、不换行、保证 `fmt` 幂等且 fmt→reparse→fmt 是不动点。`ctx.rs` 预计算 `line_starts`，`one_line(span)`（span 是否单行）驱动一切花括号容器的内联/展开决策，`blank_between` 保留元素间至多一个空行，`newline_between` 只保留作者手折的 `.` 链（唯一合法的跨行续写）。括号策略是「平铺会重新结合才加括号」：左结合链平铺、右同级别加括号，优先级表镜像 grammar 链。字符串字面量从源 span 逐字回写、永不重转义；尾随块参数识别零参块 lambda 后省略括号对。装配层的关键设计是每个 `##` 块从自己的 items+trivia 独立渲染（`render_block` 是未来 LSP 单块格式化的接口），块间文本从源码窗口逐字恢复——块渲染与装配严格分离并被测试钉死。

### 4.4 ds-query —— 编译器即查询库

这是所有上层消费者（CLI、未来的 LSP/REPL/热重载）的唯一入口，实现了增量计算引擎的标准三件套。输入带修订号：`set_file_content` 同内容是 no-op（省掉 LSP 整条失效链），真变更则 bump 全局 revision 并沿反向边传递性标脏。查询带依赖边：每个 memo 记录 `(value, deps, verified_at, changed_at, dirty)`，读取时三态——fresh 命中 / dirty 则逐依赖深校验（`changed_at <= verified_at`）通过则续用 / 否则重算。重算后值相等则续期（`changed_at` 不动），这是块粒度失效的关键：文件编辑 → parse 必变（按源文本判等，永不续期）→ BlockIndex 若 span 集合不变则续期 → BlockSource 若块文本不变则续期 → 该块的下游查询深校验通过、不重算。

查询集合目前是：`ParseFile`（语法错误是数据不是引擎错误——ast=None 加诊断，不毒化依赖者）、`BlockIndex`（解析成功用树的块 span，失败退化为一个与 grammar 对齐的 `##` 字节扫描，保证带语法错误的文件仍有块粒度 seam）、`BlockSource`（续期 seam）、`Resolve`/`TypeOf`/`EffectsOf`（包级查询，按文件粒度失效——整包检查下块粒度续期会低估，文档写明了这个取舍）、`LinearityCheck`。引擎错误（Cycle/UnknownFile/KindMismatch）与语言级错误严格分离，后者一律是值内的 ds-diag 诊断。查询级循环用显式帧栈检测，而语言级的定义循环是合法语义（G3），由 ds-check 的整包不动点解决。

## 5. 类型检查器：ds-check

### 5.1 类型如何表示：原子、宇宙、规范形

一切建立在原子之上。`TypeId(u32)` 标识一个原子——nominal 的 tag/struct/enum、枚举铸造的 case、按需铸造的匿名结构原子（tuple / 泛型应用 / 函数签名）。`TypeId::NONE` 是每个宇宙第一个声明的 `none` 单元原子，所以 `T?` 就是普通的 `T | none`，optional、错误联合、enum 共享同一套机制。类型语言 `TypeExpr` 是声明里保留的书写形态（Union/Intersect/Difference/Optional/Not/Tuple/Fn 等）。

`Universe` 是原子注册表加 hash-cons 表：`solve` 把 `TypeExpr` 折叠成规范形——排序去重后的原子集合，经 `intern` 哈希合并为 `Ty(u32)` 句柄。基石性质由此而来：**Ty 句柄相等 = 集合相等 = 结构类型相等**，整个等价判定不需要结构比较。关键规范形决策：`Diff`/`Not` 在 solve 时直接算成员抹除；tuple 乘积对 union 逐位分配（4096 组合上限封顶防爆炸）；解出的是宇宙状态快照，声明新原子可能使旧解过期（过期职责归 instance 层的 check_all）。没有独立的「子类型算法」：`contains(sup, sub)` 就是有序切片上的子集判定；nominal 继承在声明期被摊平为 trait 成员登记，运行期判定全部是集合运算，无递归子类型，终止性由「一切过程扎根于有限原子宇宙」论证。

### 5.2 双向检查与流细化

`Bodies` 是逐文件的 body checker，核心是表达式 walk `expr(e, expected) -> Val`。边界锚定：顶层与公开签名必须书写（anchor），函数体以携带期望类型的块检查（尾部表达式持有 expected，这是 `?` 传播分裂的锚），出口做包含检查；局部、lambda、match 臂推断，推断不出即空类型加诊断，绝不回退到 `any`。泛型实例化是 M2 的 one-body 半成品：非 lambda 实参按结构匹配绑定变量，lambda 实参二遍检查并用其结果类型绑定返回变量；bound 以假设栈传递。

流细化是 per-binder 的 overlay：`harvest` 从条件收割 `is` 测试和 `x == none` 比较（`&&` 链递归、`||` 保守不动），`apply_narrow` 单调求交/求差；赋值与循环入口重置；match 臂按序消费 scrutinee 成员，穷尽性 = 差集非空则报，报错时把缺失成员算成集合差（"missing: timeout | refused"）——这是 set-difference 诊断的直接实现。

### 5.3 仿射分析（linearity）

`Lin` 是每个包一次的前向 walk。每个绑定携带三态格 `Uninit/Live/Dead{at}`；核心的 `read` 按 `owned && !copyable && (consuming || captured)` 决定 move，Dead 即「迁移后使用」的双 span 诊断。copyability 是包级推导：带 `~Copyable` 标记的声明、含线性字段的结构、线性容器都非拷贝。lambda/spawn 体对外层绑定的任何使用都是捕获即 move（epoch 机制）。循环做一轮 audit 重放（只查不改）捕获「第二次迭代又用」，join 只在所有不发散路径上都 Live 才保持 Live。产出 moves/drops/last_uses 三张表给 lowerer——这是 `ds-lower` RC 放置的输入。

### 5.4 解析、trait 一致性与效应

`Package` 持有双命名空间（type_names / value_names，F9 决策）：构造形式在类型命名空间解析、函数在值命名空间，调用形名字双命名空间同名即歧义错误，永不做优先级搜索。点调用是三层：free 函数优先、否则方法候选集、`Trait::f` 限定作为逃生舱。产物是 Span→Res 的侧表，checker 全靠 span 回查。

`InstanceEnv` 存按分支拆开的实例：并头 `impl t for a | b` 先分解成 per-branch。coherence 只有一条规则——同 trait 的实例头两两不相交（`H1 & H2 = empty`，可判定的集合空性），声明期和链接期各查一次；相交则用 `witness` 产出见证值类型，错误同时命名双方头。没有优先级机制：查询只有 containment 一条路径，因为头两两不相交，非空查询答案天然唯一——这正是「tag 键控 inline cache 永不需要 tie-breaking」的编译期根基。条件实例的结构环用 DFS 检测（「实例的满足依赖它自身」即拒绝）。

效应行是五词位集（error/trap/world/cancel/discontinue）：v1 没有效应语法，但运行时是 handler 形状的——`effects_of` 把每个定义建成图节点，walk 收集内在贡献（整数运算给 trap、`?` 给 error、nursery 给 cancel），迭代到最小不动点。刻意过近似且失败封闭（M7 平台子集检查宁可错拒）。

### 5.5 extraction：单子式提取的类型规则

`unwrap_or`/`unwrap_or_else`/`ok_or`/`map_err` 与 `.err(h)`/`.ok(h)` 是 E4 的提取家族。checker 按名路由进专门的类型规则，绝不走泛型推断（否则推断出的 t 会绑住裸接收者）：unwrap 系检查接收者含 `none`、提取 `diff(total, none)`、fallback 做包含性检查；`.err`/`.ok`/`map_err` 是差集语义——handled 集合 = 注解的 handler 参数，结果类型 = 余集，所以「类型本身显示还剩什么」。corpus.rs 是 conformance 语料的阶段期望头解析器：语料文件头部把每个 item 映射到里程碑阶段（typecheck / run / reject / 未落地），`ds check` 门禁据此区分「语料自有噪声」与「真失败」。

## 6. 中间层：ds-abi / ds-lower / ds-mod

### 6.1 ds-abi —— 被三方共享的「规格即代码」契约

**slot-overlay 调用约定。** 每个函数的帧是一段连续的、静态类型化的槽位加两个字，物理布局（`fp` 为帧指针，`B = params + locals`）：metadata word 在 `fp`（返回 pc、帧大小、尾链计数）；body slot i 在 `fp+1+i`（参数在前局部在后）；call word 在 `fp+1+B`（调用期间寄存被调者的 metadata）；overlay slot j 在 `fp+2+B+j`（出参区，恒为帧尾）。关键在「每个字由它的所有者帧定义映射」：普通 CALL 时被调帧的 `callee_fp = caller 的 call word`，于是被调者的参数槽 j 与调用者的 overlay 槽 j 物理重合——传参零拷贝。每个槽的类型在编译期固定（`SlotType`：I64/F64/Bool/Ref/Tag/Fn/Raw），唯一例外 `Raw` 是 overlay 词先后承载不同类型时的无类型词。

**CALLT 与尾调用保证。** 尾调用复用当前帧、fp 不变，做三件事：把 staging 在 overlay 里的实参 slide 到槽位 `0..n`（`slide_plan` 在编译期算出重叠安全的 memmove 方向）；metadata 原地改写（保留 return_pc、替换 frame_size、尾链计数 +1）；经入口描述符跳入。实参按 raw word 搬运、零 RC 流量——搬动即所有权转移，栈在任何深度、任何 tier 都不增长。**两字入口描述符** `{tier1_prologue, tier2_code}`：word 0 由 loader 写一次终身不变（Relaxed 读），word 1 由 JIT 线程 `compare_exchange(0 → code_ptr, Release)` 单向翻转，调用点 Acquire 读——要么见 0 走 tier-1，要么见完整初始化的 tier-2 代码，撕裂不可观测。**尾链 backtrace 元数据**：TCO 吞帧后栈回溯要诚实，每帧的 `TailChain.elided` 饱和计数记录吞了多少逻辑帧，debug 录制开启时旁路追加 elision 记录，`materialize()` 重放成逻辑帧序列。

**uop 指令集。** 全语言唯一的指令契约（tier-1 分发、stencil 源、字节码→SSA 降级三方共用），约 40 个变体、32 位定长编码：`[31:24]` 是 opcode byte（major<<4|minor），`[23:16]` 主槽位 a，`[15:0]` 是 b/c 两槽位或一个 16 位 d。要点：立即数一律走常量池（拒绝内联拆域）；跳转偏移 biased（`field = offset + 0x8000`，解码 `(field ^ BIAS) as i16` 免符号逻辑）；比较对 XOR 相邻（`opcode ^ 1` 即取反，支撑分支融合）；调用族 CALL/CALLT/CALLG/CALLS 共用 overlay staging 约定，CALLS 是 ISA 唯一的动态分派点（callee 是槽位操作数）。RC 指令（major 0xE，RCINC/RCDEC）刻意不进 `Uop` 枚举而进 `Inst` 并集，让 VM 的分发 match 对 `Uop` 保持穷尽。effects 表提供 reads/writes/pure 注解：分配类与 RC op 刻意标记为不纯，因为重排它们会破坏 dup/drop 平衡。

### 6.2 ds-lower —— 从 typed AST 到字节码的完整路径

四步流水线。第一步 planning：收集单态顶层 lambda、必须有 `main`，lowering 过程中 lambda 可递归注册（worklist）。第二步 shapes：把 checker 的类型映射到槽位表示——标量一个词；裸 struct/tuple 是按声明序的字段 span；联合是 **tag word + 共享 payload region**，region 按「词类型匹配即复用」分配，所以 region 宽度是成员所需的不同词形数而非成员数；tag id 模块级首用序分配。第三步逐函数 lowering（`func.rs`，主体）：槽位 bump 分配（参数→捕获→局部/临时，上限 256）；核心抽象是 place——一个值是 body 槽位上的一段连续 span，`word_addrs` 把流细化后的使用类型映射回具体词（子集联合保持全集布局）；`const` 绑定是 place 别名不拷贝，`var` 拥有专属 span、赋值先 drop 旧引用再重存。所有指令先进符号 buffer（槽位是符号、跳转是 label id），`finalize` 渲染：FUNCF 占 word 0、每个 overlay 词一种 SlotType 否则 Raw，最后经 `FrameLayout::new` + `ds_mod::Function::new` 双重验证。

第四步是 RC 放置（`place.rs`，Perceus 风格）：`Placement` 是随 lowering walk 同步前进的所有权状态机——production（KSTR/结果出 overlay）即 owned；词拷贝在死亡点转移、否则 RCINC；读在死亡点 RCDEC；分支 fork 快照、汇合取交集，单臂独有的计数由臂尾 splice 的 DEC 序列在臂内销毁（平衡汇合）；循环用保守规则（更深层的使用绝不 drop，防每迭代重复释放，换可能泄漏）。native 调用统一「先求值实参到 place，再 Mov 进 overlay」：本地被调的 borrow 参数拥有递来的词；Builtin/Generic 是 transient borrow 窗口（死亡源在 call 后 flush DEC）。fallible native 降级为两词 platform span + TAG_TEST(0xFF)，失败路径打印消息后 HALT。泛型调用走 CALLG：receiver 的实例作为 i64 常量 evidence words 追加在实参之后，运行时不存在单态实例、只有 one body。

### 6.3 ds-mod —— 固定宽度、可 mmap 的模块格式

dsmod 是 M0 锁死的第二承重契约。布局：32 字节固定前缀（magic `\0DSM`、version、target class、flags、collector hook 槽——为未来两钩子收集器预留——max_call_depth、ir_checksum）→ 变长头（模块名、依赖表[名字+blake3-128 内容哈希]、源哈希，PEP 552 式 checked-hash、永不 mtime）→ section directory（uleb 数量，每项 kind+offset+len）→ 紧密平铺的 payload。五种 section：blocks/constants/type-metadata/effect-rows 必填、debug-spans 可 strip。reader 强制 canonical 顺序 + 平铺无间隙 + 无尾随字节，因此每个被接受的文件与其再序列化字节恒等。每个码字必须在其 pc 解码成功（保留 opcode、非 canonical 位、坏 bias 都是 4xxx 诊断）、每个跳转严格落在函数体内，writer/reader 双向共享同一条验证路径（`Function::new`）。mmap 加载的 SAFETY 契约显式：只允许映射 content-addressed 且写后不可变的缓存工件（并发截断映射文件是 UB）。缓存路径是纯函数：`$DS_CACHE/ir/<triple>/<blake3>.dsmod`，内容寻址天然不可变。

## 7. 运行时：ds-vm / ds-rt / ds-stdlib

### 7.1 ds-vm —— tier-1 解释器

**词模型与帧。** 栈是 `Vec<Word>`（Value 或 FrameMetadata）；`Value` 是无装箱标量枚举 I64/F64/Bool/Tag/Ref/Fn，Ref 是堆对象表索引而非裸指针。帧几何由 `FrameLayout` 描述，`word_of` 把槽位号映射到物理字（越过 body 要跳过 call word 那 1 个字）。调用即覆盖：callee 的 metadata 词写进 caller 的 call 词，callee 参数槽与 caller 出参区物理重叠，参数零拷贝。

**分发与调用。** `execute` 是单个扁平 match 的 dense dispatch，每条指令先解码（biased 跳转在解码内无分支完成）；major 0xE 的词走 RC 分支执行 RCINC/RCDEC。唯一的抢占点在跳转宏：向后跳时检查所在 nursery 的 cancel 标志，保证线性消费中的指令不被打断。四种调用路径各司其职：CALL 经两槽模块表解析 FuncRef（native 表兜底），native 调用不压帧（invoke_native 按参数槽类型检查 staged 词，结果写回出参槽）；CALLT 走 slide 计划 + metadata 原地改写，是 H2 承重契约的运行时实现；CALLS 是可调用词调用（Fn 槽持标量 FuncRef 零 RC 流量，Ref 槽持闭包对象——header 是 FuncRef、captures 内联），闭包调用把 capture 块拷入 callee 首个局部槽区并按 Ref 计数移交；CALLG 是泛型调用，evidence 词跟随实参 staged 在 overlay，分发到唯一 body。

**对象堆与 RC。** `Objects` 是 slab 表：`Vec<Object>`（Str/List/Map/Closure/Free 五态）+ 平行 `Vec<u32>` 计数 + 单链 free list。alloc 先取 free 头（出生计数 1）；dec 到 0 走 free：解除 intern 绑定、按容器递归 dec 持有的 Ref、槽位入 free list（last-freed-first-reused）。字符串按内容 intern 共享对象。retain 契约：存入 list/map 的 Ref 各获一计数，读出也获一计数——精确 RC 的两端在 native 边界对齐。

**fiber 与 nursery。** Fiber 由 segment（256 词 chunk 的 append-only 列表，保地址稳定）+ callinfo 链 + resume 点组成；运行 fiber 的 segment 与 VM 上下文字段 O(1) 互换（无词拷贝）。控制原语 `fiber_yield`/`fiber_resume`/`nursery_join` 是被 dispatcher 在 CALL 点拦截的 control native。nursery 维护 children 按 spawn 序、逐位 outcome 账本、cancel 标志；join 驱动按 spawn 序逐根 drive 到死，首失败拉旗。root fiber 由 `Vm::run` 驱动，yield-to-host 以 bounce 回答。

**模块两槽表与原生函数。** `ModuleRegistry`：name → {current, retiring}；install 把旧 current 挪到 retiring（更老的被 drop，live 帧靠 `CallInfo` 里的 Arc 存活）——这是 Erlang 式两版本热重载的运行时半体，call 一律经 current 槽解析，换槽即刻生效。native 表分两张：`builtin`（41 项，每 VM 必装，纯核心：字符串/容器原语）与 `std`（25 项，由 host 即 `ds` CLI 安装的世界面：console/fsys/net/time）。索引是跨模块契约（native 表与 ds-lower 的镜像表由 drift 测试逐项钉住）。fallible native 恒答两词 tag+payload span（word0 共享成功标记 / 失败 TAG 从 u8::MAX 向下倒数），平台 tag id 约定在 native.rs 钉死。net.rs 的五个 TCP native：连接是 VM 状态（单调 handle 永不复用、VM drop 关闭全部）、阻塞等待有 3 秒上界、`http_get` 有 1 MiB 上限。

### 7.2 ds-rt —— 未接线的内存机器

ds-rt 实现了设计记录 C4 的另一套答案：per-fiber bump arena（chunk 几何翻倍、Mark/restore LIFO、`RestoreOnDrop` 保 panic 安全）+ headered RC 块（`repr(C)` 头内联 rc/size/align/deinit 指针，块 = 头 + payload 单次分配）+ 按 2 的幂 size-class 池化的 reuse threading（dec-to-alloc 原地复活）+ Inko 式借用计数诊断层。代码质量好、测试覆盖 alloc/restore/panic/深递归内存界，但**当前工作区没有任何 crate 依赖它**——ds-vm 的实际执行路径走的是 object.rs 的 slab 方案。它是 M4 的影子实现，三处其它 crate 的注释指向一个不存在的依赖关系；处置方向（文档纠偏 / 删除 / 真迁移）是作者决策，详见审查报告。

### 7.3 ds-stdlib —— 两层 stdlib 的静态半

标准库分两层（H5.5）：VM 核心原生（`builtin` 模块，ds 源码表达不了的不可约原语）与 `@std` 平台面（`std` 模块，世界副作用）。`ds-stdlib` crate 只承载静态半：`AMBIENT_SOURCE`（内嵌的 `ambient.ds` 声明文本，52 个 extern fn + tag/struct/trait 声明）由 CLI 作为包的第一个成员注册在用户源之前——G3 的单包命名空间配合 F9 双命名空间，所以 `struct map` 与 `fn map` 共存。运行半体在 ds-vm 的两张 native 表里，对接机制是 ds-lower 的镜像表：名字、索引、result 宽度三轴被 drift 测试钉住；效应行（`declared_rows()`）经 ds-query 的 seam 进入 checker。checker 放行的名字由 lower 镜像表翻成 pool FuncRef（泛型名走 CALLG + evidence），暂无运行时后端的名字成为 lowering 诊断（staged surface）。

## 8. 语言语法设计（面向 PL 读者的浓缩教程）

### 8.1 值与绑定

绑定 `const`（默认不可变）或 `var`（可重绑定局部）——重绑定与原地变异是两个独立机制，后者走 8.4 的 mut 窗口。`int` 是 i64、`float` 是 f64，裸放在无装箱槽位里；算术默认 trap（溢出、除零、超宽移位），wrap 是显式的 `+%`/`*%`，收窄永远是显式调用。字符串是不可变 UTF-8，插值 `$x`/`${e}` 是字符串上下文的 `str(e)` 糖（操作数必须是原始类型或带 `show` trait——联合或 optional 是错误，先提取）；原始串 `r#"..."#` 无转义可嵌套。相等是一个结构化 `==`，不可重载，五个刻意的例外（函数比较静态拒绝、浮点按位比较、可变单元按身份、操作数只借不耗、hash 与结构相等一致）。

### 8.2 类型是集合

声明是 nominal 的（tag/struct/enum 有身份），类型语言其上是集合代数：`a | b`、`a & b`、`a - b` 是普通类型表达式。`T?` 就是 `T | none`；`none` 是普通单元 tag，所以 optional、错误联合、enum 共享一套窄化/匹配/提取机制，没有独立的 Option 类型也没有 `?.`。窄化是 `is`（Kotlin 式类型窄化，右侧是类型表达式所以 `|`/`&` 合法），refinement 单调收窄、只在赋值处重置。构造形式按声明固定：struct 只能花括号、enum case 只能调用形式、单元 tag 裸用，跨拼写是 checker 错误。类型与值双命名空间：`struct map` 和 `fn map` 共存，调用形撞名是歧义错误、永不做优先级搜索。

### 8.3 函数、UFCS、模式匹配

`fn` 是唯一的具名函数声明；匿名函数只有 const-arrow lambda `(x) => e`。UFCS 普遍且免费：`x.f(a)` 脱糖为 `f(x, a)`，无候选搜索——free 函数优先、trait 方法其次、`Trait::f` 限定是逃生舱、真歧义是编译错误。能力参数（IO 等）从点位置排除：`println(io, msg)` 永远不能写成 `msg.println()`。默认参数仅字面量（默认值不可能捕获可变状态）；尾随 lambda 只限最后一位。match 是 Rust 形（臂、guard、or-pattern、解构、穷尽性检查），穷尽性错误把缺失成员算成集合差。算子是封闭集合映射到固定 trait（`+` 派发 `add`），优先级表冻结一次永不扩展，无用户自定义算子；`==` 永不可重载。

### 8.4 线性与所有权

每值仿射：移入新绑定、consume 调用、返回都会杀死旧绑定，use-after-move 是编译期双 span 诊断；检查是一次过程内前向 last-use 分析，无生命周期标注、无调用点模式语法。普通可拷贝值隐形拷贝、日常代码无感；`~Copyable` 是取反集合约束（从默认可拷性约束中减掉），标记真实资源（fd、锁），`deinit` 在唯一实例死亡时恰跑一次（每条路径，包括 panic 与取消拆除）。变异只有一条通道——对 owned 接收者的具名调用加 `mut` 模式——三个参数模式（borrow/`mut`/consume）在定义点固定、调用点零模式语法。集合是 linear-mutable COW：变异调用拿独占窗口，当且仅当唯一拥有时原地复用（保证可观察，不是 JIT 的希望）；独占性靠拼写（`var` 接收者），`const` 接收者的变异是编译错误——共享变异在构造上不可表达。

### 8.5 错误与提取

预期失败是返回联合的成员——可存储的数据，VM 里一个预测分支，无异常机制。`?` 传播且检查包含性（外层返回联合必须包含被传播成员，无隐式转换）；`.err(h)`/`.ok(h)` 是差集类型化的 handler（结果类型 = 进入联合减去 handler 覆盖集）；`unwrap_or`/`unwrap_or_else`/`ok_or`/`map_err` 是普通 UFCS 可调用的泛型函数。`or` 运算符被退役（一个词承载六种行为、与 `||` 撞车），换成这些既有 token 上的方法。Bug panic 不可捕获：fiber 中止、帧游走跑 deinit、trap 在 nursery 边界浮现——let-it-crash 是库模式。

### 8.6 trait、泛型与元编程

trait 是集合论接口，instance 为类型头声明，coherence 只有一条规则：头两两不相交（可判定的集合空性，声明期 + 链接期双查）。没有孤儿规则——任何模块可为任何类型实现，重叠意图用 `& ~trait` 雕出独占。泛型只有一个子句 `[t: c]`，每个泛型函数在字节码里恰好存在一次：调用点传 evidence words（静态解析的实例，或 1-2 项 inline cache 后面的运行时槽——cache 对正确性无作用），JIT 可按类型惰性特化。无 HKT、无 GATs。元编程是 comptime 求值 + 静态反射，无任何宏——不存在替换，所以卫生问题被结构性删除。

### 8.7 模块与并发

四层结构：definition < file（命名空间拼写）< package（编译单元，包内共享命名空间、零包内 import、包内循环合法）< module（`ds.pkg` 清单，blake3 哈希级联使清单即锁，无 lockfile）。模块体是纯编译期声明——无 import 期执行，这一条规则同时买到包内循环容忍、安全热重载与惰性加载。唯一例外是 script 模式的入口文件顶层语句（折进合成 `main`）。`##` 行首分隔符切块，定义与顺序无关。可见性私有默认，`pub(pkg)` 包内开、`pub` 进 `.dsi` 接口文件。

并发是栈式 VM fiber 加强制 nursery：spawn 只在 nursery 块内存在，作用域退出 join 全部孩子、首失败取消兄弟、孩子错误在边界 union 出来；代码是阻塞风格、无着色、无 await。取消是 nursery 范围的协作式（await/抢占点的标志检查），线性消费永不被打断在半途。并行是 fork-join：线性值天然可发送、不可变值天然可共享，`par_map`/`par_reduce` 确定性且无锁。

### 8.8 刻意缺失的清单

truthiness（布尔上下文强制 bool 类型）、`or` 运算符（已退役）、链式比较、推导式、继承、表达式默认参数、`any`、宏、用户自定义算子、Deref 强制、切片糖、可变长参数——每一项都是有记录理由的拒绝而非疏漏。延期增量的清单（`|>`、匿名记录字面量、`@` binder、`??` 等）在核心稳定前不进。

## 9. 测试与基础设施

- **conformance/**：种子语料移植到定案语法面，文件头部的 `// items:` 阶段期望块让 `ds check` 门禁能区分「语料自有噪声」与「真失败」。
- **fuzz/**：cargo-fuzz 目标加语料字典，覆盖 parser 与 dsmod reader 两处对抗面。
- **查询不变量测试**：ds-query 的 engine/invariants 测试钉住失效语义（脏标记传递、值相等续期、循环检测）。
- **镜像 drift 测试**：ds-lower 的 native 表与 ds-vm 的真实表逐项对照（名字/索引/result 宽度三轴），ds-check 的错误码表与快照互钉。
- **benchmarks/**：criterion 基准（VM 主循环等），release profile `codegen-units = 1`。
- **跨平台纪律**：所有代码与测试要求跨平台运行；clippy workspace 级 pedantic 全开（当前全仓仅 18 条 pedantic 警告，绝大多数在测试代码）。

## 10. 当前状态与缺口

语言面：tutorial 的核心子集（类型、联合、提取、线性、容器、闭包、泛型调用）已端到端跑通；效应语法、comptime、包管理器（`ds.pkg`/MVS/registry）、LSP、REPL、热重载 UX、tier-2/3 均在里程碑计划中未落地。运行时面：RC 已在 VM 内运行但 ds-rt 的 arena/reuse threading 未接线；native 表的参数槽位类型还没有镜像交叉验证。这两类缺口的完整清单与优先级见同日的《DreamScript 代码审查报告》。
