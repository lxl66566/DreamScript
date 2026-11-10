# DreamScript 代码审查报告

> 记录信息：审查日期 2026-11-11；工作区版本 `159bdf2e65150b1a9d6b062efc1e8c555e4822f9`（main 分支，工作树干净，HEAD: "plan: record the R14 closure round at CLOSURE_NEW"）；reviewer 模型 GLM-5.3-Flash（主 reviewer + 4 个并行 review subagent，同一模型）。

## 修复状态（coordinator 维护）

- 2026-11-11 R15（commit 6a9074e）：P0-1、P0-2、P0-3 已修复，另修 P1「match 臂模式走两遍」「不覆盖臂细化成空类型」。勘误：P0-1 原建议「else 只反转最左合取项」不健全（¬(a∧b) 不蕴含 ¬a），实际修法是 `&&` 链的 else 不反转任何细化（与 TypeScript 行为一致）。
- 2026-11-11 R18（commits fee4902, 6f68b97）：P1「file_count 无依赖边」「DefId 256 块截断」已修复，另修 ds-cli run 失败 tag 枚举不全。
- 2026-11-11 R19（commits e536243, b953094）：P0-6 采最小处置 (a)：计划文档 M4 行已注明 ds-rt 孤儿现状，三处注释指向改指 ds-vm/src/object.rs；最终处置（采用/冻结/删除）待作者决策。
- 2026-11-11 R16（commits f2781ac, 272cd11）：P0-4 已修复（err_value 逐成员 re-span + widen_into 写入条数防御 + 多成员联合 fixture）；P1「tag_id u8 溢出」「staged/nargs>255 expect」已修复（保留区边界 250 + FrameTooWide 五条 staging 路径）；「22 空格错误消息」已修。
- 2026-11-11 R17（commits 585c9d8, 08be3c1）：P0-5 已修复（blame self.current、driver 复验、drain 容忍缺账）；P1「call_hosted 拒绝后可复活」「256 槽位 load 期拦截」已修复；object.rs 递归 free 深度问题一并迭代化（500k 深级联通过）。
- 2026-11-11 R20（commits cef3859, 22d062a）：P1「pat_neg 静默改正号」「渲染过期 span panic」「合成 main 骗过 formatter」已修复（fmt 侧修法——parse 侧真实 span 会与 checker 的 span 键表撞键）；Ctx::line O(n²) 一并修复。
- 2026-11-11 R21（commit e60bfd4）：perf「FnSig 深克隆」「snapshot 全量克隆」「linearity 逐字段分配」「complement O(N×M)」已修（linearity 管线 -10%，extraction_family -20%）；err_ok 处理器克隆移除。
- 2026-11-11 R24（commits 5cf2899, 46688d2）：ds-abi/ds-mod 坏味道清单已清（uop 字段表以 47 例哨兵测试钉死到 encoder——审查所指 TAG_NEW/CLOSURE_NEW 行在现树已过时，实际错在 Call 行 d 域；Raw 单向放行双方向成文；header/lib 文档对齐；reader 错误链保留 + pc expect 类型化拒绝）。
- 2026-11-11 R25（commits 640c595, 0a7792e）：ds-query/ds-diag/ds-parse 坏味道清单已清（set_compute 改为失效既有 memo、memo 诊断 Arc 化、record 去重 O(1)、scan/tree block-0 span 全等、跨文件摘录带文件名行、pest 规则名用户语映射、注释挂接去重；526 测试绿）。
- 2026-11-11 R22（commit 3228ce2）：ds-stdlib 坏味道「ambient 声明名无运行时后端」大部分缓解（io_err/parse_err/net_err 分类法集中，success-typed 签名落地；open/read_exact/parse_json 待 M7 包类型；run 层 re-span 缝已记录为下轮项）。
- 2026-11-11 R26（commit d7e0696）：ds-check 剩余 P1 全清（值绑定点调用静默空类型、间接调用不查 arity、跨文件线性序依赖、顶层解构 def_types 不填——binder walker 收敛到 src/pattern.rs）；另修两个更深预存 bug（inherent 签名臂死代码致所有 inherent 点调用恒报 DS2013；跨文件 type_of 解析表错位）。

## 0. 审查方法与范围

审查覆盖 12 个 crate 的全部 src 代码（约 44,700 行），按模块边界分 4 个并行轮次完成：前端（ds-diag/ds-parse/ds-fmt/ds-query）、检查器（ds-check）、中间层（ds-abi/ds-lower/ds-mod）、运行时（ds-vm/ds-rt/ds-stdlib/ds-cli）。每轮 subagent 逐行通读分配的 src 文件，tests/ 与 conformance/ 略读用于核对意图。主 reviewer 对全部 6 个 P0 与主要 P1 逐条回读源码验证了证据链（下文标注「已验证」）；其余发现基于 subagent 的逐行阅读，未经运行时复现。另跑了 `cargo clippy --workspace --all-targets` 作为客观证据：全仓仅 18 条 pedantic 级警告，绝大多数在测试代码中。审查过程未修改任何文件、未运行测试（构建锁被审查进程占用）。

定级标准：[P0] 真实正确性 bug 或必须由作者决策的工程级问题；[P1] 边界条件 / 潜在 bug / 静默错误路径；[smell] 坏味道；[perf] 性能问题。所有行号对应版本 `159bdf2`。

## 1. 发现总览

| 严重度 | 数量 | 分布 |
|---|---|---|
| P0（正确性） | 5 | ds-check ×3，ds-lower ×1，ds-vm ×1 |
| P0（工程级） | 1 | ds-rt 孤儿 crate（需作者决策） |
| P1 | 21 | ds-check ×10，ds-vm ×4，ds-query/ds-cli ×3，ds-lower ×3，ds-parse ×1，ds-diag ×1，ds-fmt ×1 |
| smell | 约 30 | 全部 crate |
| perf | 约 15 | 集中在 ds-check 热路径与 ds-vm 主循环 |

总体评价：代码质量整体高于同类项目的平均水准——契约层的编码/解码对称性逐 opcode 核对无差错，验证前置且全部类型化，注释把每个锁定契约的「为什么」写清楚，clippy pedantic 几乎干净。真实风险集中在两处接线层：`ds-check` 的 type_of/decls（流细化与泛型 impl 的三个「测试没走到、语言一用就撞」型 P0）与 `ds-lower` 的 `err_value` 类型谎报链条。

## 2. P0 正确性缺陷（建议立即修复）

### P0-1 ds-check：`&&` 条件的 else 分支对流细化不健全（已验证）

位置：`crates/ds-check/src/type_of/mod.rs:3447-3463`（else 分支与 then 发散的隐式 else），配合 `harvest_into`（3635-3643）。

`harvest_into` 把 `a && b` 的两个子条件产生的 Narrow 摊平进一个列表；`if_expr` 的 else 分支对**全部** narrows 逐个 `apply_inverse`。但 else 执行的前提是 ¬(A∧B) = ¬A∨¬B——只保证至少一个子条件不成立，不能同时推出两者都不成立。反例（当前 checker 静默接受）：

```text
fn f(x: int | string, y: int | string) -> string {
  if x is int && y is string { "both" } else { x }  // else 里 x 被错误细化成 string
}
```

运行时 else 可因 `y` 不是 string 进入而 `x` 是 int，`x` 被当作 string 返回。单 Narrow 的 else 反转是对的（测试覆盖的形状）；`&&` 链是错的。then 发散时的隐式 else（3456-3462）犯同样的错。修法：else 只反转 `&&` 链的最左合取项（¬(a∧b∧…) 至多推出 ¬a），即 harvest 时标记链头。

### P0-2 ds-check：`top_level_local` 清空 `ret` 不恢复，函数体内前向引用顶层绑定后 `?`/`return` 全部误报（已验证）

位置：`crates/ds-check/src/type_of/mod.rs:449`（`self.ret = None`），配合 `snapshot`/`restore`（4225-4237，只保存 locals/refined/assumptions）。

`const_ty` 会在函数体内部按需触发 `top_level_local`（引用的顶层绑定尚未 memo 化，即声明在使用之后——G3 要求支持的前向引用），它清空 `self.ret` 后靠 `restore` 恢复现场，但 snapshot 根本不保存 `ret`。此后同一函数体内所有 `?`（3312-3323 的 `(None, None)` 分支）与 `return`（848-858）误报 "`?` outside a function has nothing to propagate into"。对照 `fn_decl`（489-563）自己手工保存恢复 `old_ret`，证明这是遗漏而非约定。修法：把 `ret`（顺带 `loop_depth`）纳入 snapshot/restore，或让 `const_ty` 走独立子 walk。

### P0-3 ds-check：impl 泛型变量用裸 `$t` 进包级宇宙，第二个同名参数的泛型 impl 必然失败（已验证）

位置：`crates/ds-check/src/decls.rs:803-808`。

`declare_instance` 用 `decls.universe.declare_tag(&format!("${}", param.name.name))` 注册泛型变量，失败被 `if let Ok` 静默吞掉。宇宙是包级单例：第一个 `impl[t: json] for list[t]` 永久占住 `$t`，之后任何参数也叫 `t` 的泛型 impl（最常见的形状）`declare_tag("$t")` 返回 DuplicateName，vars 为空 → `conv_node` 解析 head 失败 → 报出误导性诊断 "impl head `list[t]` does not name atoms the type core can check"，且该 instance 完全未声明、coherence 未跑。type_of 侧的 `mint_var` 用 `${name}#{counter}` 已避开此问题，正确做法已存在。当前 corpus 只有一个泛型 impl 所以测试全绿——典型潜伏 P0。修法：`declare_instance` 复用唯一计数命名。

### P0-4 ds-lower：`.err` 成功分支以结果类型谎报接收者 place 类型，越出 result span 写槽位并可对非指针对象 RcDec（已验证）

位置：`crates/ds-lower/src/func.rs:4467-4471`（err_value 成功分支）+ `5491-5512`（widen_into 同型快路）+ `1007-1019`（word_addrs 子集联合分支）；对照 ok_value 的正确写法（4557，传真实类型 `src_ty`）。

证据链三环，均已回读确认：err_value 成功分支把 `ty`（子集联合/保留集）同时当作 place 的声称类型传给 `widen_into`，而 `src.place` 物理持有全集联合 `src_ty` 的布局；widen_into 见 `src.ty == dst_ty` 走同型快路调 `word_addrs`；word_addrs 对「place shape 是 Span(src_ty) 但声称类型是子集联合」命中"子集联合保持全布局"分支，返回全集 span 的全部词。随后逐词 Mov 写入条数 = 全集宽度，而 `result = temp_span(ty)` 只按子集宽度分配 → 越界写相邻槽位；且 Ref 词会 `rc.copied` 把越界槽位登记进 RC owned map——帧出口 `Placement::exit` 会对该槽位发 `RcDec`，运行时该词是标量，等于对垃圾指针做引用计数递减（UAF 方向）。触发条件：被 `.err` 处理掉的成员贡献了保留集不需要的词形状（例如 `bad_port(port: int) | missing(name: string) | none` 上 `.err` 余集含 I64 成员而全集含 Ref 成员）；现有 fixtures 全是 `t | none` 形状（none 是单元、宽度恰好相等）所以测试全绿。修法：err_value 成功分支改走与 ok_value 一致的逐成员构造；同时给 `widen_into` 补「写入条数 ≤ dst span 宽度」的防御检查——这个不变量目前散落在每个调用点的自觉上，值得收敛成一处检查。建议补一个多成员联合 `.err` fixture 验证。

### P0-5 ds-vm：trap 归因于错误的 fiber，被归因者被误杀、真实受困者变成永不入账的 Running 僵尸，join 边界 panic（已验证关键两处）

位置：`crates/ds-vm/src/vm.rs:1399-1403`（`run_nursery_to_death` 的 Err 分支用循环变量 `child` 归因）对照 `1756`（`run` 正确地用 `self.current`）；后果链终点 `1417-1420` 的 `.expect("every child died through a ledgered path")`。

`execute` 内部经 `control_switch::Resume` 切入另一根 fiber 后，若那根 fiber trap，Err 直接穿透出整个 drive——此时 `self.current` 是真正受困的 fiber，而 `run_nursery_to_death` 把 trap 记到发起 resume 的 child 名下。后果链：无辜 child 被 `trap_child` 记 `Trapped` 并拉起 cancel；驱动循环收尾把这个只是 parked 的 child 强制 `Dead`（1408-1412）；真正 trap 的 fiber 状态留在 `Running`、永不入账；nursery 收尾 drain 时账目缺位 → `expect` panic。触发序列公开 API 可达（spawn 两根 fiber、A `fiber_resume(B)`、B 除零 trap、join）。修法：Err 分支统一 blame `self.current`；驱动循环在每次 switch 后重新校验 child；drain 的 expect 改为容忍缺账。`resume_fiber` 的 Err 分支（用请求参数 `fiber` 归因）同样需要按 `self.current` 校对——主 reviewer 已验证 `run_nursery_to_death` 与 `run` 两处的差异及 expect 存在，`resume_fiber` 一处未逐行复核。

### P0-6（工程级）ds-rt：孤儿 crate，声称服务的契约与 ds-vm/object.rs 的实际实现已漂移成两套平行实现（已验证）

`rg -l "ds-rt"` 全仓命中仅注释与计划文档；workspace 与任何 crate 的 `[dependencies]` 都没有 ds-rt，Cargo.lock 亦无记录。逐项漂移对照：RC 载体（headered 块 vs slab 表）、reuse threading（size-class 池化 vs 单一 LIFO free list）、deinit（静态 hook vs 无 hook 的递归 dec）、per-frame arena（完整实现 vs VM 内完全无 arena）、diag（tripwire+借用计数 vs 只搬了 tripwire）。ds-rt 代码本身质量好、测试覆盖到位，但它是 M4 的影子实现——真正落点是 `ds-vm/src/object.rs`，而 `ds-abi/src/uop/rc.rs`、`ds-lower/src/place.rs:338`、`func.rs:174` 三处注释指向一个不存在的依赖关系，`implementation-plan.md` 把 ds-rt 记为 done 且未标注未被消费。处置三选一（作者决策）：(a) 在计划文档明确记录现状并修正三处注释指向；(b) 删除或降为实验分支；(c) 若判定迁移必做，加 dev-dependency 编译守卫防契约继续漂移。

## 3. P1 缺陷

### ds-check（10 条）

- **type_of/mod.rs:3499-3513** — match 每条臂的模式被走两遍（先 `pattern_cover` 再为收集 binder 走 `pattern_walk`），模式错误全部重复上报；测试用 `.last()` 取诊断掩盖了重复。修法：`pattern_cover` 直接返回 binders，删第二次 walk。
- **type_of/mod.rs:3500-3503** — 不覆盖任何成员的臂（字面量臂）把 scrutinee binder 细化成空类型（`cover_ty(&[])`），臂体内一切使用产生级联假错（`match x { 1 => x + 1, _ => 0 }` 报 "needs numeric operands, `empty` and `int`"）。修法：arm_cover 为空时不插入 refinement。
- **type_of/mod.rs:2177-2182** — 点调用命中无函数签名的 const/var 绑定时静默返回空类型、零诊断（`call_def` 第一行 `let Some(sig) = ... else { return Val { ty: empty, ... } }`），违反 crate 自己的"推断失败即编译错误"。`const n = 5` 之后 `x.n()` 无任何诊断。
- **type_of/mod.rs:2150-2159** — 间接调用（fn 类型值调用）不检查参数个数，多余实参被静默丢弃（`zip` 截断），连表达式都不 walk。直接调用路径有 ArityMismatch，间接路径完全缺失。
- **type_of/mod.rs:4208-4215** — `mint_var` 失败时静默把类型变量变成 `none`（`unwrap_or(TypeId::NONE)`），后续一切推断建立在 none 上且无诊断。实践中 `${name}#{counter}` 不撞名，属静默错型回退的坏模式。
- **type_of/mod.rs:1300-1317（待确认）** — 期望返回为裸类型变量时 lambda 内 `ret=None`，lambda 体中的 `?` 误报 stray propagation；把 `?` 闭包传给返回类型为未绑定变量的高阶泛型函数（`fn map[t, r](..., f: fn(t) -> r)`）会假 reject。是否属 M2 刻意未支持范围待作者确认。
- **type_of/mod.rs:438-441 配合 resolve.rs:541-565** — 顶层解构绑定只支持 `Pattern::Binding`；tuple/or/struct 模式的顶层绑定 `def_types` 永不填充，使用处报 "binding `x` has no initializer"（明明有）。另 resolve.rs:551-557 对顶层 struct 模式的 pun 字段跳过不建 def，与 decls.rs 的 walker 不一致。根因见 smell-1（三个互不一致的模式 binder walker）。
- **decls.rs:404-415** — inherent impl 方法按「名字 + owner==Inherent」认领 MethodId，两个 inherent impl 定义同名方法时后处理的签名插到先注册的 id 下（HashMap 覆盖），`method_select` 用错签名检查调用。effects.rs:613-622 的同一逻辑用 span 精确匹配——正确写法就在隔壁 crate 内。修法：对齐 span 匹配。
- **linearity.rs:197-200 + 405-422** — 跨文件时顶层线性绑定的消费判定依赖文件处理顺序（文件 A 的函数消费文件 B 的顶层绑定、A 先处理时 `read` 查 info 落空静默跳过），与文档声称的 "order-independent" 矛盾。修法：两遍处理改为先对所有文件跑 top_local、再对所有文件跑函数体。
- **universe.rs:486（受控 panic 面，存档不要求修）** — `members` 直接索引 `&self.sets[t.0 as usize]`，跨宇宙/野 handle 是索引 panic；同类 resolve.rs:383-390 的 `def()/method()`。文档已声明 handle 不得跨宇宙。

### ds-vm（4 条）

- **vm.rs:456, 803, 854, 1476, 2816, 2840 等** — 布局槽位数无 256 上限的运行期拦截：256 上限只存在于 ds-lower（FrameTooWide），ds-mod 读回槽位表无上限、`ModuleVersion::new` 的 validate_body 也不查，而 VM 的槽位寻址全是 `u8::try_from(...).expect(...)`。任何绕过 ds-lower 的生产者喂入 >256 槽位的函数会在运行时 panic，违背 vm.rs:76-77 自己声明的 "structured stop condition, never a panic" 纪律。修法：在 load 期（validate_body）把槽位上限列为新的 ModuleBuildError 变体，一处拦截覆盖全部 expect。
- **vm.rs:1840-1844** — `call_hosted` 拒绝（ReentrancyRefused）后 lambda 所在 fiber 仍是 `Suspended` + resumable，链上挂着 sentinel return-pc 的 reentry frame；host 再次 `resume_fiber` 必以 `InvalidPc { pc: u32::MAX }` 告终——一条撒谎的 trap，且参数 RC hand-off 计数在 trap 路径泄漏。修法：拒绝分支把该 fiber 的 resume 清空，使其不可再进入。
- **object.rs:341-372** — `free` 的递归 dec 无深度上限：深嵌套 list/map/closure 的唯一死亡是原生 Rust 递归（dec→free→dec），完全不经过 fiber 栈的 1 Mi word 限，几十万层嵌套在 drop 路径 SIGABRT、不可捕获。修法：迭代化（显式工作栈）或 deferred 析构队列。
- **vm.rs:1667-1702（待确认）** — `nursery_join` 可经跨 nursery 互锁循环造成原生栈无限递归（自 join 有防死锁检查，互锁没有）。当前 in-language 无 spawn/join 面、nursery id 只能由 host 喂入，可达性依赖 fixture；in-language surface 落地后是真实风险。修法：`run_nursery_to_death` 加嵌套深度上限。

### ds-query / ds-cli（3 条）

- **db.rs:284-287 + queries.rs:58-67（已验证）** — `Ctx::file_count()` 直接返回 `changed_at.len()`，不记录任何依赖边；`package_asts` 用它拿文件数再循环读取。「文件集合本身」没有输入键，`add_file` 不 bump revision、不触发反向边——先查询 Resolve/TypeOf 再 `add_file`，旧 memo 保持 FRESH，新文件的声明对解析/类型/效应完全不可见且无任何诊断。effects_of 的文档承认了这一点（归为 M7 seam），但 resolve/type_of 共享同一漏洞且未声明。修法：把 file_count 做成可记录的输入（add_file 时 bump），改动很小，比等 M7 更早堵洞。
- **key.rs:5-9 + queries.rs:99-103 + ds-cli/src/check.rs:113（已验证）** — `DefId` 占位打包 `(file << 8) | block`，块数 ≥256 时静默串块：第 256 块的 key 与下一文件第 0 块相同，TypeOf/LinearityCheck 返回另一个定义的数据或 UnknownBlock，无告警。CLI 是唯一消费点且无防御。修法：打包处对 index > 0xff 显式报错，或提供真正的 DefId 分配器。
- **ds-cli/src/run.rs:148-154** — 运行结果的失败 tag 只识别 `PLATFORM_IO_ERR`/`PLATFORM_NET_ERR`；`PLATFORM_PARSE_ERR`/`PLATFORM_NONE` 若成为 main 返回 tag，打印原始 u8 数字。低危，建议按 `ds_vm::native` 常量补全。

### ds-lower / ds-mod（3 条）

- **func.rs:3654、3743、3758、3763、5802-5804** — staged/overlay 词数或 nargs 超过 255 时 `u8::try_from(...).expect(...)` 让编译器 panic 而非报类型化错误。可达路径：`f(s, s, ..., s)` 256 个实参在 256 个 Mov 后 panic；`FrameTooWide` 检查只看 body+overlay 槽位，拦不住 staged 词数。修法：staging 循环前统一检查 ≤255 并报 LowerError。
- **shape.rs:143-151（已验证）** — `tag_id` 的 `next_tag: u8` 无溢出检查：第 256 个不同原子时 debug panic / release 回绕为 0，与第一个原子撞号 → TAG_TEST/match 分派静默串号。对照 `word_of` 对 span 宽度做了 typed 拒绝，这里漏了。修法：usize 存储超界报 typed error。
- **read.rs:417 + 438-439** — 前者：pc 的 `u32::try_from(...).expect` 与同文件 "the reader never panics on any input" 的承诺矛盾（需 >16 GiB 输入触发，纯理论但文档立了 flag）；后者：`Function::new` 二次校验失败时 `map_err(|_| ...)` 把 `JumpEscapesBody { pc, target, len }` 等具体诊断信息吞成一句通用文案。建议保留错误链。

### ds-parse / ds-diag / ds-fmt（3 条）

- **ds-parse/src/convert/type.rs:237-265（已验证）** — `pat_neg` 的负号只被 int/float 字面量消费；`-true`、`-'a'`、`-"s"` 静默产出正字面量，零诊断。grammar 的 `pat_neg?` 在语法上可修饰任意字面量，转换层把语法接受的形状静默改义，违反「编译期错误优于运行期错误」。修法：neg + 非数字字面量记 InvalidLiteral 类诊断，或收紧 grammar。
- **ds-diag/src/files.rs:190-198 + render.rs/json.rs** — 渲染路径对「过期 span」会 panic（`location` 对越界 offset assert），render/json 是纯 pub API 无防御。LSP 场景「先 set_source、再异步渲染旧 Diagnostic」是现实时序。契约已文档化故定 P1，但 panic 发生在最下游展示层。建议渲染层 clamp 或提供 try_human。
- **ds-fmt/src/expr.rs:127-133 配合 ds-parse/src/convert/mod.rs:311-341** — script 模式合成 `main` 的合成节点用零宽 span（避开下游 span 键表），骗过 ds-fmt 的 `one_line` 探针 → 多语句 entry 文件被格式化成 `fn main() -> () { const a = 1 const b = 2 }`——不可再解析。当前 `ds fmt` 命令尚未接线所以不发作，但 `format_source` 的契约就是吃 parse 结果、entry 文件是最先被格式化的对象，一旦接线即 P0。修法（二选一）：parse 侧给合成块一个覆盖语句真实范围的非空 span（更符合「span 不撒谎」）；或 fmt 侧 `write_block_inline` 在多语句时强制 multiline。

## 4. 坏味道（smell）与性能（perf）清单

### 4.1 跨 crate 的结构性坏味道

- **三个模式 binder walker 漂移**（ds-check）：resolve.rs:541-565 不收 struct pun、decls.rs:901-925 收 pun 不收 Pattern::Call、linearity.rs:575-622 最全。P1 的顶层解构假错正是漂移的现行后果。应收敛为一个共享 walker。
- **ds-vm 两个超长文件**：vm.rs（3004 行）与 native.rs（2220 行）。execute 的算术/比较臂是同一 read-read-compute-write 样板 ×20+（约 250 行），文件内注释以 debug 构建的 call 开销为由拒绝拆 helper——宏展开不产生 call，两全；native.rs 的 41 个表项可用 reserved 占位已有的宏法收敛。
- **ds-check type_of/mod.rs 的 4538 行单体**：calls（约 900 行）与 refinement/solve 是明显可拆的内聚边界，符合仓库「明确边界才拆」的规范。

### 4.2 逐 crate 清单

**ds-lower**：`func.rs:3929/3934` 错误消息含 22 个连续空格（字符串续行事故，会原样出现在用户诊断里，已验证）；`func.rs:5875` "Set difference over member lists." 文档长在 `fn_sig_of` 头上、主人 `set_diff` 无文档；[perf] `func.rs:762-769` 常量池去重线性扫描 O(n²)（建议 HashMap 旁路索引）；[perf] `func.rs:1025-1038` `member_fields` 每次调用 clone 整个 Vec（可 Rc 化）。

**ds-abi**：`tailcall.rs:185` `SlotType::Raw` 放行是单向的（caller Raw 可打任意 callee，callee Raw 不可被具体类型匹配），文档只写了 caller 方向；`uop/mod.rs:38-59` 字段映射表与实际编码小有出入（TAG_NEW 行、CLOSURE_NEW 未列 c）——这是锁契约的门面，应对齐一次。**正面确认**：encode/decode 对称性已逐 opcode 核对无差错（含保留位、RETSPAN/CALLS/FUNCF/HALT 的零域检查、±32768 跳转边界、slide 方向规则）。

**ds-mod**：`header.rs:52` flags 文档字节偏移错一位（写 byte 5 实为 byte 6）；`lib.rs:77-78` blocks payload 的 slot 类型字节只列到 Tag=5，module.rs 已扩展 Raw=6/Fn=7（免版本升级，但 crate 级锁定文档应同步）；[perf] read.rs:414-428 + module.rs:110-121 reader 对每个码字做 decode+encode 两遍校验（刻意设计，加载大模块多一趟，记录备查）。**正面确认**：writer/reader 布局完全对称、三重定界检查、canonical ULEB、每个被接受文件与再序列化字节恒等。

**ds-diag**：`render.rs:77/132` `last_off` 表达式逐字重复两份；`render.rs:73-107` 跨文件诊断的 excerpt 无文件名前缀（M7 多包后是 UX 债）。

**ds-parse**：`convert/expr.rs:806-866` 注释挂接逻辑逐字复制两份；`expr.rs:735` arm() 的 block_expr 分支是死代码；`expr.rs:350` `pending_op.take().unwrap_or(BinaryOp::Eq)` 把理论不可达的「缺操作符」静默映射为 `==`（建议 debug_assert）；`lib.rs:163-177` 解析错误消息直接暴露 pest 规则内部名（`expr_nos`、`kw_const` 等）；[perf] `convert/mod.rs:442-448` `Ctx::line` 每次从头数换行，注释密集大文件 O(n²)——ds-fmt 的 Ctx 已有 line_starts+partition_point 的正确写法可搬，是最值得顺手修的性能点；[perf 备注] `lib.rs:126-139` 每次 parse 新建 8 MiB 栈线程（防栈溢出的正确取舍，LSP 路径可考虑线程池/stacker）。

**ds-fmt**：`decl.rs:190-198` 等四处 struct/enum/trait/impl 多行成员间空行被删光，与 lib.rs:36-37 的文档矛盾（要么文档收窄要么补保留逻辑）；`expr.rs:50-60` `stmt_span_end/start` 的 min/max 是恒等操作；[perf] `lib.rs:175-188` `find_separator` 每个分隔符 O(全文)（Ctx 已有 line_starts 可 O(log n)）。

**ds-query**：`db.rs:218-220` `set_compute` 不失效该 kind 既有 memo（约束未文档化）；`value.rs:37-43` ParseOutput::eq 只比 source，正确性悬在「parse 只依赖 FileContent」的跨文件隐式契约上（建议互引注释或改恒不等）；[perf] `db.rs:309-317` record 线性去重 O(deps²)；[perf] `db.rs:332-339` memo 命中深克隆 Box<[Diagnostic]>（建议 Arc，ParseOutput 已是 Arc，风格不一致）；[perf] `queries.rs:359-391` 退化扫描与树 span 在「第 0 块前置注释」上不一致，文件从 scan 愈合为 tree 时第 0 块无谓失效一次。

**ds-check**：`type_of/mod.rs:3768` 诊断文案硬编码 `` `argument` ``；`type_of/mod.rs:765-782（待确认）` 表达式位置 `is` 缺恒假测试检查（`x is io_err` 对 x: string 静默恒假，若刻意应只对原子测试收紧）；`decls.rs:226` 与 `decls.rs:614-627` 死参数（`let _ = resolutions;`、`let _ = vars;`）；`decls.rs:486-487` 载荷字段全不可转换时 case 原子停在 Reserved（应空 fields 也 set_struct_fields 定形）；`linearity.rs:469` 不可达分支；`linearity.rs:1077-1112` 循环 audit 的正确性依赖 merge_states 丢弃新键的隐式行为——两处靠耦合而非注释互指，「修好」merge 会引入大量假错，值得加交叉注释；`effects.rs:386` 哨兵节点魔法值（FileId::new(0), 0, 0）；`corpus.rs:36-59` `// items:` 头解析过脆，头不完整时整个文件按未分阶段全诊断致命（conformance 门禁静默行为翻转，建议至少给诊断）；`lib.rs:83-88` Scope 章节陈旧（声称 bidirectional checking "later work"，type_of 已完整实现）；`pattern.rs:398-406`/`mod.rs:4288` 死参数；`ops.rs:358-373` In 臂注释说 refuse、代码返回 bool_ty；[perf] `type_of/mod.rs:1101/2177/2581/2698/2755 + linearity.rs:1136/1187/1226` 每个调用点深克隆 FnSig——ParamSig.default 是字面量表达式 AST 子树，method_select 对每个 candidate 都克隆，type_of 最热路径，建议 `Arc<FnSig>`；[perf] `mod.rs:2869` err_ok 克隆整个 handler lambda AST（无需）；[perf] `mod.rs:4225-4237` snapshot 每个块/if/match/lambda 边界全量克隆两个 HashMap（可用代际标记替代）；[perf] `universe.rs:403-409` complement O(N×M)（仅测试使用，改二分即可）；[perf] `linearity.rs:947-987` 每次字段投影新建空 HashMap + 字段名比较克隆 String（提为空引用 + as_deref）。

**ds-vm**：`native.rs:69` 模块文档的 40 项索引契约表被一个空行截断，rustdoc 渲染中表格失去表头不可读；`native.rs:1700-1709`（ref_arg）与 net.rs:232/286/400 类型不匹配诊断报错参数的类型取 `args.first()` 而非越界参数本身（同文件 `i64_arg`/`fn_arg` 用 `args.get(index)` 是正确写法，复制走样）；`vm.rs:1052-1054` `Vm::stack_len` 在 drive 进行中读 pool slot 的陈旧 segment（当前 generic 体未调用此 API，API 卫生问题）；41/25 的魔法表长度靠测试 assert_eq 维持（建议 install 时加 debug_assert）；[perf] `vm.rs:1874-1889` 每条指令重复派生帧上下文（重读 functions/code/slots/pool 与越界检查——只在 CALL/RET/CALLT/CALLS 后变化，线性段可缓存，帧转移点刷新）；[perf] `vm.rs:2503-2556` 每次 CALL 的字符串查表 + Arc 克隆×3（可加 (pc → 解析结果+版本戳) 的 inline cache——正是章程「适用处的内联缓存」的主循环落点）；[perf] `vm.rs:785-819` invoke_native 每次 Vec::with_capacity（参数通常 ≤3，可固定缓冲）；`vm.rs:2657` CALLS 每次 `block.to_vec()` 复制 capture 块；`object.rs:230-238` KSTR 每次执行 re-intern（pool 常量→对象 id 的一次性缓存即可消除）。**正面确认**：net.rs 无泄漏（单调 handle 永不复用、VM drop 关闭全部、阻塞等待有界）。

**ds-rt**：`heap.rs:200-222` 非 diag 构建下 rc==0 是 wrapping 静默减（diag 有 assert，release 的 u32 wrap 使块既不释放也不报错；孤儿状态下无实际风险，若落地消费建议对齐 ds-vm 的错误返回形态）。

**ds-stdlib**：ambient.ds 有约 8 个声明名无运行时后端（get/join/log/close/parse_json/load_rows/fetch/par_map/par_reduce 等），`ds run` 对它们的失败形态是 lowering 错误而非「未实现」诊断（lib.rs:26-32 定位为 staged noise 属文档化决定；建议 ambient 注释分组或专用错误码）；另 [备注] ds-lower 镜像测试钉住了名字/索引/result 宽度三轴，但 native 的**参数槽位类型**没有镜像交叉验证，目前靠人肉对齐，可加粗检。

**ds-cli**：run.rs:148 失败 tag 枚举不全（见 P1）；正面确认：build.rs 写盘前 blake3 自校验是好实践，source.rs:31 阻止用户文件影子 builtin/std 模块，跨平台合规。

## 5. 正面观察（审查中确认的质量亮点)

1. **ds-abi 与 ds-mod 的契约纪律**：编码/解码、writer/reader、正反跳转算术全部对称（逐 opcode 核对），验证前置且全部类型化，注释把每个锁定契约的「为什么」写清楚。
2. **ds-query 引擎核心**：ensure/verify/反向边重同步/值相等续期经逐路径推演无 P0；错误路径不落 memo 且帧平衡；嵌套 ensure 的 cycle 检测正确；fuzz 不变量测试钉住了大部分失效语义。
3. **ds-check 类型核**：atom/universe/witness/instance 的算法有文档化终止性论证、上限防护（MAX_TUPLE_COMBINATIONS、checked_mul）与小宇宙穷举对拍测试；instance 无缓存、查询单路径，结构上排除了「缓存与直查不一致」。
4. **lint 卫生**：workspace 级 clippy pedantic 全开，全仓仅 18 条警告且绝大多数在测试代码。
5. **镜像与快照测试**：native 表 drift 测试三轴钉住、错误码表快照互钉、ds-fmt 往返测试，把「稳定 API」机械地钉在测试里。

## 6. 修复优先级建议

1. **ds-check 三个 P0**（§2 P0-1/2/3）：`&&` else 细化、top_level_local 的 ret、impl `$t` 撞名——都是小改动、高回报，且 `&&` 细化是会产出错误运行结果的健全性问题。
2. **ds-lower err_value 类型谎报**（P0-4）：修 widen_into 防御检查 + 补多成员联合 `.err` fixture。
3. **ds-vm trap 归因**（P0-5）：统一 blame `self.current` + drain 容忍缺账。
4. **ds-rt 处置决策**（P0-6）：至少先做文档纠偏（修正三处注释指向），处置方向是作者决策。
5. **ds-query 两个 P1**（file_count 依赖、DefId 截断防御）：改动都很小，脏缓存路径当前真实存在。
6. **一次性 panic 面收敛**：tag_id u8 溢出、staged>255 的 expect、load 期 256 槽位上限、ds-mod reader 的 expect——统一换成 typed error。
7. **性能顺手修**：ds-parse Ctx::line（搬 ds-fmt 的 line_starts 写法）、ds-check FnSig Arc 化、ds-vm 主循环帧上下文缓存与 CALL inline cache、ds-lower 常量池 HashMap 索引。
8. **结构性重构（随里程碑做）**：三个模式 binder walker 收敛、vm.rs 算术臂 binop! 宏化、type_of/mod.rs 按 calls/refinement 边界拆分。
