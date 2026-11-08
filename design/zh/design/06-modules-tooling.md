# 06 — 模块、工具链、FFI 与生态（决策目录）

状态：阶段 2 选项目录，写于 2026-11-04，等待作者按 AGENTS.md 作出决策。主要证据：`docs/zh/research/06-modules-tooling-ffi.md`（下文引用为 research/06 加章节号），以及作者本人的包管理器提案（C:/programs/me/lxl66566.github.io/src/coding/package_manager.md，引用为 UPM）。姊妹目录仅按路径引用，内容归各自文件负责：`docs/zh/design/01-types.md`（类型基础）、`docs/zh/design/02-linearity-ownership.md`（线性/仿射层，决策点 C1-C5）、`docs/zh/design/03-effects-errors-async.md`（效应与能力，决策点 B5、E1-E5）、`docs/zh/design/04-syntax.md`（表层语法）、`docs/zh/design/05-generics-metaprogramming.md`。本文件所有代码均为示例。最终表层语法由 `docs/zh/design/04-syntax.md` 决定。

## 决策点索引

| ID | 决策点 | 选项 | 推荐（一行） |
|---|---|---|---|
| G3 | 模块系统与单元分层 | (a) Rust 式 crate+模块树配 use 路径，(b) MoonBit 风格的分层，生成接口文件与块模型，(c) Unison 的内容寻址定义，(d) TS/ESM 的文件即模块 | (b)：定义 < 文件 < 包 < 模块，生成 `.dsi` 接口，`///\|` 风格的顺序无关块，包内允许循环、跨包禁止，模块体只在编译期求值，这条规则在三处得利（research/06 §1.7、§1.10-1.11、§6.3） |
| G4 | 可见性模型 | (a) pub/priv 二分，(b) 分层可见性配选择性导出，(c) 能力/胶囊可见性，(d) Go 的导出标识符约定 | (b)，把 (a) 作为它的缩小版子集：`pub` / `pub(pkg)` / 私有，再加 Inko 风格的选择性导入列表，并用生成的接口文件充当可评审的 API 关口 |
| H5 | FFI 与嵌入 | (a) 线性类型的 C ABI 句柄 + 句柄表嵌入 API，(b) 平台模型（宿主提供分配器与效应处理器），(c) 裸 `extern "C"` 加手工安全，(d) wasm 优先的多后端 | (a) 作基底，并在其上叠加 (b) 的「包导出处理器」变体：(a) 正是 docs/zh/design/02 的线性层兑现价值的地方，(b) 复用 docs/zh/design/03 的能力基底，同时避开 Roc 式巨石 |
| H6 | 生态与工具链栈 | (a) Cargo 式清单+锁+注册表，(b) 哈希存储原生，(c) bun 式零配置脚本，(d) 平台模型、可嵌入优先 | (b) 作核心，把 (c) 作为叠加的脚本模式。(a) 的注册表延后，改成 (b) 存储之上的静态 JSON 索引。(d) 是附加轴，「必做工具链」小节在所有选项下都生效 |

## 共享前提（对所有选项生效）

章程对所有选项都生效的约束：解释执行并带内部字节码 IR（章程硬性要求 1），静态线性/仿射类型（章程硬性要求 2），超高性能，包括保证尾调用优化（章程硬性要求 3），语法糖天花板取 Kotlin 的总表层（章程硬性要求 4），绑定默认不可变（章程硬性要求 5），代数效应可选、只有值回复杂度才采纳（章程硬性要求 7），解耦优先的 RISC/LSP 设计伦理（章程设计哲学）。

姊妹目录给出的前提：docs/zh/design/02 推荐 C1(b) 仿射核心配可选的线性 `~Copyable` 层，以及 C5(c) 资源层上的线性 RAII 析构，H5 选项 (a) 正好消费这套基底。docs/zh/design/03 推荐 E1d（处理器运行期在底层，表层没有效应语法）配 E1c 的能力参数惯用法，以及 B5d（并集基底的错误签名），H5 选项 (b) 与 H6 的必做 REPL/诊断各项正好消费这套基底。这两处由各自的目录决策，本文件只说明每个选项对它们提出什么要求。

运行时基线经姊妹目录来自 research/03：带类型的寄存器字节码虚拟机，每个函数静态帧布局，本文件的每个设计都排除导入期执行，它是 G3 的硬性产出，处在选项之外。VM 的模块格式后果只在 G3.5（「dsmod 契约」）里说明一次，其余每个选项都引用它。

## 作者痛点映射到设计杠杆

章程列出四项作者痛点。每一项都映射到下文的具体选项，每个选项都按这张表打分。

| 痛点（来源） | 症状 | 本文件的设计杠杆 |
|---|---|---|
| Nix（UPM：flake `follows`、全局 `flake.lock` 拖累依赖方、输入哈希存储、没有 FHS、内建函数缺文档） | 更新一个包就锁死或拖垮整个系统。装到标准路径需要手写 shell 文件 | H6(b)：依赖锁按包嵌入清单（Zig 式哈希级联，因此 `follows` 无从存在），输出哈希存储，可选的 FHS profile 链接器，bin 优先的产物（research/06 §2.3、§2.7、§7 组合 B） |
| Python 模块（research/06 §1.7：导入期执行、循环在运行期才失败、目录粒度的可见性、`__init__` 重导出样板代码） | 「模块粒度不够，无法交叉引用，实质上只是加强版代码复制」 | G3(b)：模块体只在编译期求值，包内循环合法，同目录文件共享命名空间成包，`///\|` 块作为工具链单位（research/06 §1.9-1.11） |
| `#include`（章程口味：厌恶文本包含式模块） | 头文件粘贴文本。边界上没有类型安全保证。API 面拿不出评审产物 | G3(b)：生成接口文件（`ds info` -> `.dsi`），按 diff 评审，`include` 从文法里彻底删除（research/06 §1.3、§1.9）。G4(b)：接口文件就是可见性关口 |
| LSP 优先（章程：以 IDE 为中心的作者，出处 research/06 §4.1） | 每次按键都做全程序重分析。工具链事后才补上 | H6 必做承诺：从第一天起编译器就是查询库，格式化器与接口生成器都是编译器组件，结构化诊断（research/06 §4.1-4.4） |

## G3 — 模块系统与单元分层

本点决定的内容：三个容易混为一谈的概念（命名空间/可见性单元、编译单元、分发单元）各自得到明确的分层（research/06 §1.1）。本点同样定下循环规则、模块体是否在导入时执行、字节码模块格式必须支持什么。作者的 Python 痛点分解成五条可检查的机制（research/06 §1.7），其中导入期执行是最深的一条：模块体只作编译期的纯声明，循环就退化成名字解析的细节，运行期没有隐患。G3 还固定作者随笔要求的粒度阶梯：把「小模块组合成更大的模块」，且不需要样板式接口包。

### G3.a — Rust 式 crate+模块树配 use 路径

沿用 Rust 的整 crate 编译：crate（这里叫包）是编译与分发单元，`mod` 声明对应文件，`use crate::x::y` 路径带 `crate::`/`self::`/`super::` 前缀，名字解析是 crate 范围且容忍循环，所以同级模块可以自由互相引用。

```rust
// G3.a -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
mod transform;          // reads src/transform.ds
mod io;                 // sibling modules may mutually refer: cycles inside the crate are legal
pub(crate) fn helper() {}         // visible package-wide, not to dependents
use crate::transform::normalize;  // explicit paths, re-export via pub use
```

先例：Rust 的 crate 与模块、crate 范围的不动点解析、`pub(crate)` 可见性阶梯、2018 edition 的路径文法以及 2024 edition 的保留路径补丁（research/06 §1.2）。权衡：容忍循环，以及隐私与文件相互正交，是实实在在的收益（Rust 代码库里看不到 Python 式的管道杂务）。可是 `mod` 声明是样板，作者的成长路径不想要它。路径文法属于 Rust 较陡的角落。编译单元是整个包，对带多 GB 负载的脚本语言偏粗：每次按键都按包粒度失效，除非编译器自行推导更细的单元，而 G3(b) 靠块模型白拿这一点。排除：文件即模块的零样板成长（先 `ds run script.ds`，然后就地长大）、块粒度的工具链、H6(c) 的 bun 式脚本模式。

启动与分发：字节码按包粒度缓存（`<triple>/<pkg-hash>.dsmod`），对应用够用，对脚本偏粗，只有一个导入的脚本也要付整包加载的代价。G3.5 的 dsmod 契约照旧生效。

### G3.b — MoonBit 风格的分层：定义 < 文件 < 包 < 模块

四层明确的层：定义（函数与类型，可见性与身份的单位）、文件（模块，命名空间的拼写）、目录（包，即编译单元，同一个包里的文件共享一个命名空间，包内没有导入，Go 式）、模块（`ds.pkg` 清单，即依赖与分发单元）。顶层代码组织成以 `///|` 分隔、顺序无关的块，于是编译器关心的单元低于文件层级。包按路径导入包，可选别名。接口文件由工具生成，从不手写。模块体是编译期的纯声明，导入期执行一律排除。

```text
// G3.b -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
// repo layout:  ds.pkg (manifest) | pipeline.ds | transform/ | io/ | schema/
///|                                        // block separator; blocks are order-independent
pub fn parse_row(s: String) -> Row | ParseError { ... }   // definition = visibility unit
///|
type Row = { id: Int, tags: List[String] }   // blocks: reordering, moving, deleting are safe ops
// transform/moon.pkg-equivalent (ds.pkg of a subdir): entire content is the import list
// { "import": ["acme/pipeline/schema", "acme/pipeline/io"] }
// `ds info` generates schema/pkg.dsi -- the reviewable API surface:
// package "acme/pipeline/schema"
// pub type Row;  pub fn parse_row(String) -> Row | ParseError
```

先例：MoonBit 的模块-包-文件组织，`moon.pkg` 的全部内容就是一张导入列表，生成的 `.mbti` 作为 API diff 产物来评审（「如果 .mbti 里什么都没变，通常就是一次安全的重构」，出处 moonbit-core AGENTS.md），`///|` 块模型及其有文档的顺序无关性，按目标加后缀的文件，blackbox/whitebox 测试划分（research/06 §1.9）。粒度阶梯与成长路径草图见 research/06 §1.10。循环策略：包内文件之间的循环合法（Rust 式整包解析），包与包之间的循环是编译错误，诊断建议「把 `X` 移到 core」，即 MoonBit 的 `builtin` trait 规则，共享抽象住在无依赖的基础包里（research/06 §1.9、§1.11）。编译期纯模块体是那条一条规则三处得利的规则：(1) 包内循环只是解析细节，回答了 Python 的交叉引用痛点。(2) 热重载变得安全，Erlang 模型不需要顶层副作用（research/06 §4.5）。(3) 惰性加载模块图能缩短启动时间（research/06 §6.3 杠杆 3）。三个设计，一条规则。权衡：整包不动点的名字解析是实打实的编译器工作量（按包有界）。四层的词汇表需要半页教程（在可解释性预算之内）。选择性导入的易用性归 G4 负责。排除：Unison 式哈希寻址定义（G3.c）、Python 式导入期副作用（有意放弃），以及跨越包边界的按文件独立编译（包就是失效单位，这也正是必须禁止包循环的原因：循环会废掉按包增量检查，research/06 §4.1）。

启动与分发：dsmod 头部携带依赖名列表，加载器只加载 `main` 能到达的部分。接口文件让 LSP 悬停变便宜，且不需要执行任何东西。同一张包图可以编译成单个内嵌字节码的二进制（G3.5）。

### G3.c — Unison 的内容寻址定义

哈希即代码身份：每个定义以其带类型的语法树的哈希存储，哈希里包含依赖的哈希。「代码库」是一个只追加的 CAS，名字只是元数据，文件不存在。

```text
// G3.c -- illustrative
unique type Row = { id : Int }              // stored as #blake3...; body + deps' hashes in the hash
parse_row : String -> Row | ParseError      // edit the body -> new hash -> dependents rehash transitively
```

先例：Unison 的内容寻址代码库、无冲突重命名、内建依赖版本化、定义级合并、按哈希安全远程执行（research/06 §1.8，另见 LWN 2023）。权衡：整套工具链都得自己造，没有纯文本文件意味着 grep、git、patch、基于文本的 LSP 都要经同步协议重新发明。程序文本无法按行 diff，而作者生活在 VSCode 与 git 里。排除：直接排除纯文本文件文化，以及 G3 里每一个面向文件的选项和 G4 里每一套文本 diff 评审流程。这是一条设计轴，用途限于借鉴。启动与分发：完美的缓存与可平凡验证的分发（按哈希抓取）在这一选项上故事最强，但 G3.5 的 dsmod 能以零文化成本拿到其中九成（内容寻址的 IR 缓存，带哈希校验），这正是 research/06 §1.8 推荐的便宜借鉴：给 IR 产物和包身份算哈希，不给定义算哈希。

结论：作为模块模型被排除，作为缓存与存储哲学保留（G3.5、H6.b）。

### G3.d — TS/ESM 风格的文件即模块配 import/export

每个文件都是模块，带静态 `import`/`export`。绑定是活的，图按深度优先后序求值，循环合法且语义有定义。这就是作者钟爱的 TS+bun 模型去掉坑之后的样子：检查器在整张图上静态拒绝先使用后初始化，TS 做不到这一点。

```ts
// G3.d -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
import { normalize, Row } from "./transform.ds"     // files are modules; directories need index files
export function parse_row(s: string): Row | ParseError { ... }
// cycles a<->b are legal: hoisted functions callable across the cycle; TDZ cannot happen
// because the compiler rejects use-before-init -- the ESM trap made a compile error
```

先例：ESM 的静态导入、活绑定、局部初始化时的 TDZ 作为「能通过每一条工具链检查的 bug」、CommonJS 的快照语义、Deno/bun 的注册表说明符与 import map（research/06 §1.6）。Python 的五机制痛点在同样的形状上展示了导入即执行的后果（research/06 §1.7）。权衡：词汇量最小，而且是作者日常的工效。但目录要变成包就得靠 `index` 文件约定或 import map，样板又爬回来。编译单元是文件，对脚本没问题，但跨文件类型检查需要额外焊一个项目层，正是 TS 的 `tsconfig` 处境。即使模块体为纯，求值顺序语义也必须完整规定（提升规则）。排除：包即共享可见性的命名空间被排除，每个文件都得重导出才能暴露包 API，也就是 `__init__.py` 的杂务重生。MoonBit 级接口文件同样被排除，`.d.ts` 那样的层要么手工维护，要么靠工具魔法。H6(c) 的热重载故事也变弱，因为按文件的模块是重新求值，热重载只能重跑文件而换不掉一个带版本的包槽位。

启动与分发：按文件的字节码缓存很自然（像 `.pyc`），但加载器必须在运行期重建导入图。import map 或生成的清单替代目录遍历式解析（research/06 §2.5 的 Deno 教训：裸名解析住在一份小配置里，取代目录遍历式解析）。

### G3.5 — dsmod 字节码契约（对所有选项生效）

无论哪种模块模型胜出，运行期模块格式都必须让启动与分发免费。格式需要这些字段：魔数头与格式版本，IR 变化就升版本。一个目标类别字节（bytecode | wasm | native triple）。一个标志字节（STRIP、FFI-USED、HAS-JIT-METADATA）。模块的稳定名。依赖列表，形式为 (name, content-hash) 对，即热重载需要的重编译闭包。用于 PEP 552 式 checked-hash 校验的源码哈希，它比对哈希，mtime 在 git 检出和内容寻址存储里会撒谎。以及 ULEB 前缀、长度分隔、重定位极少的分段，包括字节码原型、常量、类型元数据、效应行、可剥离的调试区间（research/06 §6.2，LuaJIT `lj_bcdump.h` 的纪律）。

```text
dsmod (proposed):
  bytes 0..3  magic "\0DSM"      byte 4 format version      byte 5 target class
  byte 6      flags (STRIP | FFI-USED | HAS-JIT-METADATA)
  uleb        module name        uleb deps: [(name, content-hash)]...
  uleb        source hash (blake3-128)                      # PEP 552 checked-hash mode
  sections    blocks | constants | type metadata | effect rows | debug spans (strippable)
```

启动杠杆按收益排序（research/06 §6.3）：命中的缓存路径走 mmap（`$DS_CACHE/ir/<triple>/<blake3-of-source+deps>.dsmod`，Java AppCDS 证明对预链接元数据做 mmap 就是收益所在）、由头部依赖列表驱动的惰性加载、排除导入期执行（G3.b 的规则）、用 `ds build --exe` 产出内嵌字节码的单文件二进制。字节码本来就是内部 IR，所以 Bun 实测 `--compile --bytecode` 相对仅打包有 1.38x 启动收益，这只是下限，上限更高（research/06 §6.1）。分发后果：每个选项都提供 `ds run`（走缓存）、`ds build --exe`（单个二进制内嵌依赖闭包与 VM）、wasm/component 目标（H5.d）。Windows 上用 scoop 式 shim 代替符号链接（research/06 §6.4）。

G3 对比：

| 维度 | G3.a Rust 树 | G3.b MoonBit 分层 | G3.c Unison | G3.d ESM |
|---|---|---|---|---|
| 从脚本长成包所需的样板 | mod 声明 + 路径 | 没有（先同文件，再一个目录） | 不适用（没有文件） | index 文件 / import map |
| 循环故事 | 包内合法 | 包内合法，跨包禁止 | 无意义 | 合法，求值顺序隐患有规格 |
| 工具链粒度 | 包 | 块（`///\|`） | 定义 | 文件 |
| 接口产物 | 没有（rustdoc） | 生成的 `.dsi` | 哈希里的类型 | `.d.ts` 式，手工或工具 |
| 编译期循环错误 | 只跨包 | 只跨包 | 不适用 | 没有（运行期求值顺序） |
| 导入期执行 | 没有 | 没有 | 没有 | 没有（检查先使用后初始化） |
| 排除 | 脚本优先的成长 | 哈希寻址定义 | 文本文化 | 包级 API 关口 |

**推荐 — G3(b)**：MoonBit 风格的分层是唯一同时回应四类要求的选项：Python 痛点靠无需样板即可交叉引用解决，`#include` 痛点靠生成接口解决，脚本成长路径起步时只需零清单，热重载与启动契约靠纯模块体加 dsmod 头部的依赖列表满足。把 G3.a 的容忍循环作为它的解析策略，把 G3.5 的格式作为硬性产出。

## G4 — 可见性模型

本点决定的内容：谁能看见哪些定义，以及这一点怎么拼写。可见性与 G3 的循环策略相互作用：包内循环会让按文件的可见性自相矛盾，一个包里的所有文件按一个命名空间解析，所以可见性挂在定义上。可见性也与接口文件相互作用，`.dsi` 就是「依赖方看到什么」变成可评审 diff 的地方（research/06 §1.9、§4.2）。作者的 Python 痛点里包含「用 `_` 下划线按约定表达隐私」（research/06 §1.7 第 3 点），这就排除了纯约定式的答案。

### G4.a — pub/priv 二分

Rust 的模型：一切默认私有，`pub` 把它开放给所有依赖方，中间只有第三种可选标记 `pub(crate)`。

```rust
// G4.a -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
fn shuffle(buf: Buf) -> Buf { ... }        // package-private by default
pub fn parse_row(s: String) -> Row | ParseError { ... }   // visible to all dependents
pub(crate) fn bench_shim() { ... }         // visible package-wide only
```

先例：Rust 的 `pub`/`pub(crate)`/`pub(super)`/`pub(in path)` 阶梯，隐私与文件正交（research/06 §1.2）。权衡：文法最简单，要解释的东西最少。但二分的选择把「只对我那一个依赖方可见」和「对全世界可见」混为一谈：中型库要么暴露内部实现，要么用 `pub` 加文档隐藏，给某个同级包访问权的唯一路径是打开全局 API。排除：分层 API 面（在两个第一方包之间共享的内部包）、符号级的暴露控制，以及把接口文件当作按依赖方的关口。启动/分发：与运行期无关，可见性只存在于编译期。

### G4.b — 分层可见性配选择性导出

可见性搭在 G3(b) 的阶梯上：定义默认私有。`pub(pkg)` 把它开放到包内，同包文件与本包自己的测试可见。`pub` 开放到模块级，对依赖方可见，并写入生成的 `.dsi`。导入按符号选择并支持改名，因此包可以重导出一份挑选过的表层。条件导入按目标选择。

```text
// G4.b -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
fn shuffle(buf: Buf) -> Buf { ... }                 // private to this file's package internals
pub(pkg) fn bench_shim() { ... }                    // package-wide, never in .dsi
pub fn parse_row(s: String) -> Row | ParseError { ... }   // in .dsi; the only depedent-visible fn
// dependent side -- selective import with alias/rename (Inko grammar, research/06 §1.11):
import acme/pipeline/schema (Row, parse_row)
import acme/pipeline/io as pio                      // namespace alias: pio.read_file(...)
import acme/pipeline/fs if os == "windows"          // conditional import
pub import acme/pipeline/api (parse_row)            // re-export as the package's own API
```

先例：Inko 的 `import mod1.mod2 (A, B as C)` 符号选择文法与 `import foo if linux and amd64`（research/06 §1.11 的导入文法表，逐字取自 inko syntax.md）。MoonBit 的 `.mbti` 分段正好列出 `pub` 表层，以及 AGENTS.md 规定的评审流程（research/06 §1.9）。定义 < 文件 < 包 < 模块的阶梯（research/06 §1.10）。Ada 的子包可见性沿层级下传，是概念上的祖先（research/06 §1.3）。权衡：拼写从两种变成三种，外加一条文法规则（选择性导入）。换来的是在文件之间移动代码属于重构中立（可见性按定义），内部包可以在不打开公开 API 的前提下表达，`.dsi` diff 正是作者 `#include` 痛点所要的 API 评审产物。与循环的关系：包内循环不受影响，`pub(pkg)` 及以下都在同一个命名空间内解析。跨包时 `.dsi` 是唯一关口，于是包 DAG 加接口文件等于整份可见性契约。排除：没有实质内容，路径范围的 `pub(crate)` 式阶梯（`pub(in acme/pipeline`）被刻意略去，以留在语法糖天花板之内。

启动/分发：运行期没有影响，`.dsi` 同时充当 LSP 的悬停/签名来源和增量编译的边界（research/06 §4.2）。

### G4.c — 能力/胶囊式可见性

类型状态口味的可见性：模块导出胶囊类型，其成员只有在能力令牌（打开胶囊时铸出的线性值）位于作用域内时才可访问。可见性于是变成一种近乎运行期检查、由静态跟踪的资源。

```text
// G4.c -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
capsule Db { fn query(self, q: String) -> Rows }    // members gated by Cap[Db]
fn with_db[T](cfg: Cfg, body: (cap: Cap[Db]) -> T) -> T   // minting scoped to the callback
```

先例：docs/zh/design/03 的能力参数惯用法（E1c）。经由 Roc 平台反转的对象能力文献（research/06 §5.4）。权衡：可撤销、限定作用域的访问对嵌入（H5）确实有力，但作为可见性机制，它把每次导入都变成要穿线传递的值，让整套名字解析背上一个线性层，并且过不了语法糖天花板（章程硬性要求 4）与可解释性预算：「为什么我不能调用这个函数」变成一个类型合一问题。排除：便宜的静态名字解析、简单的 LSP 悬停（可见性查询变成类型查询），而且它以高得多的成本重复了 E1c 已经为效应提供的东西。

结论：在语法糖天花板下很可能被排除，留作记录，用途限于库惯用法（能力参数）。

### G4.d — Go 式按约定导出标识符

大小写就是可见性标记：`Parse_row` 导出，`parse_row` 只在本包内可见。文法里没有可见性关键字。

```go
// G4.d -- illustrative
func Parse_row(s String) Row | ParseError { ... }   // capital = exported
func shuffle(buf Buf) Buf { ... }                   // lower = package-internal
```

先例：Go 用大小写表达导出、包内文件共享命名空间、把禁止循环当作设计异味的探测器（research/06 §1.5）。权衡：零文法、零仪式。代价是 API 被拼进了标识符，为了风格改名就改动了 API。「包内可见」与「公开可见」之间缺少中间层，工具链过滤可见性只能靠大小写约定。作者明确的口味拒绝约定式隐私（Python 的 `_` 下划线，research/06 §1.7 第 3 点）。排除：按依赖方的暴露、用大小写以外的任何东西过滤接口文件，可评审 diff 的性质也被削弱（每次改名都是一次 API diff）。启动/分发：没有影响。

G4 对比：

| 维度 | G4.a 二分 | G4.b 分层 | G4.c 胶囊 | G4.d 约定 |
|---|---|---|---|---|
| 文法成本 | 1 个关键字 | 2 种拼写 + 选择性导入 | 胶囊 + 令牌类型 | 没有 |
| 按依赖方暴露 | 不行 | 通过 .dsi + 挑选过的重导出 | 按令牌作用域 | 不行 |
| 与包内循环的交互 | 没问题 | 没问题（按定义的可见性） | 破坏（令牌必须穿线） | 没问题 |
| 接口文件故事 | 弱（只有 pub 过滤） | 强（关口本身） | 不适用 | 弱（大小写过滤） |
| 语法糖天花板风险 | 没有 | 低 | 高 | 没有 |
| 作者痛点匹配（Python `_` 隐私） | 好 | 最好 | 过度 | 被口味排除 |

**推荐 — G4(b)**：分层可见性配选择性导出，以 G4(a) 的默认私有作为基本情况。接口文件既是可见性关口，也是评审产物，这正是 `#include` 痛点要求的机制，也是仍能表达内部包的最便宜选项。

## H5 — FFI 与嵌入

本点决定的内容：DreamScript 如何调用 C，宿主如何嵌入 DreamScript，以及效应与线性性在边界上如何表现。research/06 §5 已经立好基底：C ABI 作为通用的低层，基于句柄的 GC 互操作取代裸指针，这一条来自 Lua 的栈加注册表 API，以及 Python C API 的 owned/borrowed/stolen 引用坑，WIT/component model 作为带类型的 wasm 边界，Roc 的平台反转作为与效应设计在概念上契合的做法。两处承重的交互：选项 (a) 需要 docs/zh/design/02 的线性层（C1(b) 仿射核心配 `~Copyable`，C5(c) 线性析构），句柄只消费一次，双重释放就是类型错误。选项 (b) 需要 docs/zh/design/03 的 E1d 处理器运行期与 E1c 能力惯用法，平台的效应行就是契约。

### H5.a — 线性类型化的 C ABI 句柄 + 句柄表嵌入 API

外部资源是不透明的线性句柄，C 层遵循有文档的 `repr(c)` 布局纪律。嵌入 API 只经下标暴露对象：`Value` 是 VM 对象表里的下标（Lua 注册表模式），借用是调用范围的 `&Value`，所有权转移用线性 `Value`，上下文是显式的 `ctx: &mut Vm`，两个嵌入方就是两个 VM，GIL 问题在设计上自然消失。对 FFI 可见的内存做 pinning，使 GC 永不搬移它（research/06 §5.1-5.2）。

```text
// H5.a -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
type c["sqlite3"]                                        // opaque, layout unknown, ~Copyable => linear
extern fn sqlite3_open(path: *const c.Char) -> (c["sqlite3"] | c.Int)   // out-param flattened
extern fn sqlite3_close(db: c["sqlite3"]) -> c.Int       // consumes db: exactly-once, checked
// let db = sqlite3_open(p); sqlite3_close(db); sqlite3_close(db)  -- compile error (2nd use of moved db)
// host embedding side (C API):
//   ds_push_int(L, 42); ds_call(L, "process", 1, 1);      // explicit stack, Lua-shaped
//   int r = ds_ref(L, -1);                                // registry ref = GC-scanned handle
```

先例：Lua 的值栈加注册表引用加 `luaL_ref` 句柄，回调只在 VM 管理的栈上。Python C API 的 owned/borrowed/stolen 三元组以及成串的引用计数 bug 作为警示故事。Rust 的 `repr(C)`/`extern "C"` 与 Zig 的 `export fn`/`@cImport` 用于 ABI 层。`ds bind header.h` 以 Zig `translate-c`/bindgen 的工效产出带类型的外声明（research/06 §5.1-5.2 及其中各表）。催生这套设计的一行对比：Lua 证明最小化的句柄已经够用，Python 证明指针加约定正是扩展 bug 的所在，而 DreamScript 有一个类型系统，可以把这条约定变成类型错误。权衡：每个返回拥有型指针的 C 函数都必须包一层，好让句柄类型诚实（这是真实但机械的绑定工作，`ds bind` 摊薄它）。回调必须在 VM 管理的栈上执行。在提示符上维持线性纪律意味着用户必须消费或显式丢弃句柄（H6 必做小节里的 REPL 设计处理这点）。它排除什么：随意把指针传给 C，这是设计意图。对 H5(b)/(d) 它没有任何限制，那两个选项都是叠加项。要求：docs/zh/design/02 的 C1(b) 仿射 + `~Copyable` 与 C5(c) 析构钩子，线性层缺席时这个选项会退化成 H5(c)。

启动与分发：dsmod 的 FFI-USED 标志在加载时声明 dlopen 需求，被 pin 的外部对象需要一条 GC 设计注记（docs/zh/design/02 已经按 pinned-Foreign 假设），原生目标需要 zig-cc 充当链接器的姿态（research/06 §3.3）。

### H5.b — 平台模型：宿主提供分配器与效应处理器

Roc 的反转，并按 RISC 伦理拆细：应用是纯的，平台提供带效应的表层（I/O、分配、宿主函数），检查器验证用户代码的效应行是平台声明行的子集。DreamScript 版本避开 Roc 的巨石：平台就是普通包，导出效应处理器加可选的宿主粘合层，可按构建目标选择。默认平台就是 `ds` CLI 本身，提供 std 效应行（research/06 §5.4）。

```text
// H5.b -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
platform "std"   provides <console, fsys, net, time>     // the ds CLI provides these handlers
platform "web"   provides <console, fetch>               // wasm host satisfies via WIT imports
platform "embed" provides <>                              // host calls in; provides nothing
fn main() -> { console.print("hi") }   // checker: <console> is a subset of the platform's row
// effect rows render to WIT for wasm components (research/06 §5.3):
//   world pipeline { import host-io: interface { log: func(msg: string); ... }
//                   export run: func(input: string) -> result<string, string>; }
```

先例：Roc 的应用/平台拆分、平台提供分配器、粘合层加外科手术式链接器、单一平台限制、平台版本化给包管理带来麻烦（research/06 §5.4、§2.6）。WIT world 与 wasmtime `bindgen!`（research/06 §5.3）。Koka 的按后端内联外声明作为多目标粘合层的拼写方式（research/06 §5.5）。权衡：效应审计清楚、可嵌入，而且「包即平台」变体保住了脚本故事（默认平台零仪式）。代价是效应行到 WIT 的降级与粘合生成器属于实打实的编译器工作，平台版本化是包模型上的复杂化（兼容单位变成平台版本，research/06 §2.6），拥有效应的富固定标准库被排除（标准库变成多个平台之一，这是一项文化承诺）。要求：docs/zh/design/03 的 E1d 处理器运行期（即使表层效应急需延后，底层效应也是真实存在的）以及用于审计的 E1c 能力惯用法。排除：Haskell 式的全纯保证（效应总是相对平台而言），以及单一固定效应表层。启动与分发：「embed」平台让 libds 大小的嵌入成为常态（`libds.so` 与 Lua 同级）。wasm 组件是一等分发目标。平台本身经 H6(b) 的存储按 bin 优先分发。

### H5.c — 裸 extern "C" 函数配手工安全

Zig/Rust 2015 的姿态：带类型的外声明，C 布局规则有文档，资源纪律靠约定与代码评审。

```c
// H5.c -- illustrative
extern fn sqlite3_open(path: *const c.Char, out: **c["sqlite3"]) -> c.Int
extern fn sqlite3_close(db: *mut c["sqlite3"]) -> c.Int
// double-close is a runtime bug, exactly as in C; leak detection is a lint, not a type fact
```

先例：Rust 的 `extern` 块（把 unsafe 边界显式化）、Zig 的 `@cImport`/`extern "c"`、有文档的 `repr(c)` 布局规则（research/06 §5.1）。权衡：几乎零编译器工作，而且是 C 程序员最熟悉的表层。代价是丢掉章程的线性层唯一独家买到的东西：「必须恰好释放一次」退化为 lint 层面的检查，嵌入 API 继承 Python 式的约定沼泽（research/06 §5.2 的表）。排除：边界上的静态资源保证。它把 docs/zh/design/02 的 FFI 故事降级成装饰，C1 的线性层于是只为内部资源而存在，削弱 research/06 §5.1 声称的收益：「FFI 层正是线性性兑现价值的地方」。启动/分发：与 H5.a 相同，两者都是原生 C ABI，只有安全纪律不同。

### H5.d — wasm 优先，JS/原生作为次要后端

把 wasm（以及 wasm 组件）当作主要编译目标，写一次，跑在浏览器、CI 和嵌入方。JS 与原生作为后缀文件的次要后端，MoonBit 式。

```text
// H5.d -- illustrative
// foo_wasm.ds / foo_js.ds / foo.ds          -- backend selection by filename suffix, no preprocessor
// ds build --target wasm-gc                  -- one tree, three backends (MoonBit template)
```

先例：MoonBit 从一份源码树产出 wasm/js/native，用按目标加后缀的文件和 `moon build --target wasm|wasm-gc|js|native`（research/06 §1.9、§5.3、§6.4）。Component Model/WIT 的带类型边界（research/06 §5.3）。权衡：白拿通用分发与沙箱，跨「编译」变得平凡（产物就是 wasm）。代价是原生性能，也就是章程的全部要点，变成次要的一层。wasm GC 的可用性约束了值的表示（带类型寄存器与未装箱 ADT 必须映射到 wasm GC 类型或装箱回退，于是 research/03 警告的装箱开销项有风险），每条 FFI 路径也需要自己粘合层的 wasm 变体。排除：把原生字节码 VM 当作参考语义，它会漂向 wasm 那一层。如果 wasm 真的优先，docs/zh/design/02/03 的零成本布局也受约束。启动/分发：四者里 web 故事最快（wasm 流式实例化）。原生故事与 H5.a/H5.c 相同。

H5 对比：

| 维度 | H5.a 线性句柄 | H5.b 平台 | H5.c 手工外声明 | H5.d wasm 优先 |
|---|---|---|---|---|
| 编译器工作 | 绑定 + 句柄表 | 粘合生成 + WIT 降级 | 最少 | 后端 + 每目标粘合 |
| 双重释放 | 类型错误 | 不适用（谈效应，不谈资源） | 运行期 bug | 取决于每后端粘合质量 |
| 要求 | 02 的 C1(b)+C5(c) | 03 的 E1d+E1c | 没有 | 值表示上的妥协 |
| 可嵌入性 | 句柄表 C API | 最好（embed 平台） | C API，有约定沼泽风险 | 仅 wasm 宿主 |
| 边界性能 | pinned + 未装箱 | 底层与 (a) 相同 | 相同 | wasm GC 约束 |

**推荐 — H5(a) + (b)**：把 (a) 作为基底，它是唯一让 docs/zh/design/02 的线性层在边界上收回成本的选项，再把 (b) 的「包导出处理器」平台模型叠在上面，用于效应与嵌入。这就是 research/06 §7 的组合 D 去掉 Roc 的巨石。(c) 作为 (a) 内部有文档记录的逃生口，`ds bind` 的输出在包句柄之前就长这样。(d) 是分发目标，FFI 架构仍由 (a) 决定。

## H6 — 生态与工具链栈

本点决定的内容：包/构建/分发栈（research/06 §7 的四个组合），外加一组在每个选项下都生效的必做工具链承诺。research/06 §3 给出的约束：构建层必须是单个二进制，带声明式默认值加一个同语言逃生口（`build.ds` 跑在编译器已经嵌入的 VM 里，Zig 必须编译出一个 runner，解释器只要直接求值）。构建层同时要避开三种形态：第二门 DSL、一个 Bazel、一套让带类型检查的表层分叉的特性开关系统。

### H6.a — Cargo 式清单 + 锁 + 注册表

`ds.toml` 写名字/版本/semver 范围依赖，生成 `ds.lock`，`src/` 布局，`ds add/build/test/fmt/info/publish` 子命令，稀疏索引注册表（Cargo 的模型），可选特性开关。

```toml
# H6.a -- illustrative
[package]
name = "pipeline"
version = "0.4.0"
[dependencies]
csv = { version = "0.7", features = ["simd"] }   # range + lockfile; features unified across graph
```

先例：Cargo 的清单/锁/解析器、依赖图上的特性统一（cargo#10489：「我的依赖用上了我从未要求的 serde 特性」）、PubGrub 解析器、多主版本共存（research/06 §2.1）。mooncakes.io 证明一门年轻语言能在约两年内建起注册表（约 3.5k 模块，research/06 §1.9、§2.6）。权衡：最熟悉的外形（作者懂 Cargo），第一天就有常规注册表。代价是注册表属于基础设施里的长杆，锁文件与清单的漂移是一项永久的税。特性开关重新引入按配置的类型检查，模块系统必须应对这一点（research/06 §2.1 建议字节码语言不要特性开关，MoonBit 用文件名后缀，Inko 用条件导入）。每个生态克隆一份 Cargo 在设计上就排除了作者的 UPM 抱负。排除：哈希即锁的简洁、作为用户可见概念的输出哈希存储（被降级成内部缓存细节）、H6(b) 的整套设计。启动/分发：与 G3.5 相同，这条轴与启动正交，单文件二进制与缓存的行为一致。

### H6.b — 哈希存储原生（作者的 UPM，落地版）

依赖声明为 `name = { ver, hash }`，其中 hash 是包内容的 blake3，内容包含它的依赖哈希，这就是 Zig 的级联设计：身份沿依赖传递，锁文件无从出现，清单就是锁。MVS 解析（Inko/Go：在各方声明的最低版本里取最大，不做求解，「绝不会用到你没测过的版本」）。全局内容寻址存储，键是输出哈希，按项目通过硬链接/符号链接物化（pnpm 的机制）。按目标三元组提供 bin 优先的产物，并用存储哈希校验。可选的 FHS profile 链接器（`ds link`），Windows 上用 scoop shim，NixOS 上用 profile 符号链接。等生态规模需要时，注册表延后成存储之上的静态 JSON 索引（Cargo 稀疏索引的形状）。

```text
# H6.b -- illustrative (ds.pkg; one file, no lockfile, manifest is the lock)
name    = "pipeline"
version = "0.4.0"
[deps]                          # MVS minima; each pin carries a content hash
std     = { ver = "1.2", hash = "blake3:9f2c..." }
csv     = { ver = "0.7", hash = "blake3:1a88..." }   # csv 0.7 coexists with another pkg's 0.6
[artifacts]                     # bin-first: prebuilt per triple, hash-verified
x86_64-pc-windows-msvc = { url = ".../pipeline-0.4.0-x86_64-pc-windows-msvc.dsmod", hash = "blake3:77e0..." }
[build]                         # optional escape hatch, run by the embedded VM
script = "build.ds"             # sees #OUT#, #TMP#, #ARCH# (UPM template vars)
```

先例：Zig 的 `build.zig.zon` URL+哈希配依赖哈希级联（「清单即锁文件」，research/06 §2.3）。Inko 的 `require URL VERSION CHECKSUM` 配 MVS 与多主版本共存（research/06 §2.4）。pnpm 的 CAS + 硬链接物化 + 全局虚拟存储（research/06 §2.2）。Nix profile 与 scoop shim 用于 FHS 链接（research/06 §2.7、§6.4）。Bazel 按输出键的 action 缓存（research/06 §2.7）。把作者的 UPM 与既有的东西逐项对照（research/06 §2.7 的表，精简版）：

| UPM 元素 | 已被谁证明 | 剩余缺口 |
|---|---|---|
| 按包的依赖锁（哈希+版本），更新永不破坏上游 | Inko 的清单即锁、Zig 的依赖哈希级联、Cargo.lock | 没有缺口。哈希用 blake3，Inko 用 SHA1。提供 `ds update` 缓和手动改版本的体验坑 |
| 输出哈希寻址的存储 | pnpm（文件）、Zig（tarball）、Nix FOD、Bazel（action 输出） | 输入哈希与输出哈希是两回事，这是作者 UPM 第 6 点与 Nix ca-derivations 的教训：键取输出内容哈希，不可复现的构建回退到输入哈希。端到端输出哈希在别处也还处在早期 |
| 多版本共存 | Nix 存储、Zig 缓存、Cargo 主版本、pnpm | 没有 |
| FHS profile 链接 | Nix 用户 profile、buildEnv、scoop shim（Windows）、pnpm 虚拟存储 | Windows 上 shim 取代符号链接。硬链接跨不了卷，也链不了目录，因此用文件级硬链接（pnpm）或符号链接。冲突加后缀的规则可以直接用 |
| 一份清单里的 bin 优先 | AUR `-bin`、cargo-binstall 式声明产物 | 信任模型：按平台校验和/签名。作者「bin 与 unbin 同在一份清单」把 Nix 分裂的两份缓存合并为一份 |
| 分阶段的任意语言构建脚本 | PKGBUILD、Homebrew、Nix builders、Bazel genrules | 封闭性：把脚本的工具依赖也钉住（由依赖锁覆盖）。DreamScript 自己在嵌入 VM 里跑 `build.ds` 是默认做法 |
| 双哈希的 P2P 种子分发 | IPFS/CAS、nix-casync、Nix-P2P 研究 | 长尾在经济上脆弱。CAS 分块对种子友好，可作为可选加速器。对 v1 明确非核心 |

权衡：存储加物化器（pnpm-lite）以及哈希级联方案就是实现成本。范围求解器的易用性没有了，退到 MVS 取最大，这一取舍与 Go/Inko 相同。安全升级变成显式改清单，Inko 有文档并接受这一点。UPM 的跨语言普适抱负留在语言的关键路径之外，Nix 的教训是通用构建描述才难，语言自己拥有清单、MVS、存储路径约定，跨语言的 UPM 以后可以在同一个存储上生长（research/06 §2.7 的结论）。排除：自动升级文化，以及锁文件那种「如果」式解析查询，可复现性由构造保证。启动/分发：四者里最强，存储同时充当字节码缓存（产物就是按内容哈希键控的 dsmod 文件），bin 优先让 `ds build --exe` 产物成为存储里的一等公民，PEP-552 式哈希校验是设计自带的（research/06 §6.2-6.3）。

### H6.c — bun 式零配置即时脚本

`ds run script.ds` 不需要清单，依赖在首次导入时从默认索引自动解析，并在 `ds save` 时记录进生成的清单。隐式包内部允许模块循环，热重载内建，存储托底一切，因此没有 `node_modules`。

```text
# H6.c -- illustrative
$ ds run pipeline.ds            # zero config; imports auto-resolve; cache is warm via the store
$ ds save                       # freeze what the script used into a generated ds.pkg
import "@std/csv"               # std = bare specifier, no URLs (Deno's import-map lesson, no retreat)
```

先例：bun/deno 的零配置运行加存成清单的模式、裸说明符与 import map（research/06 §1.6、§2.5、§7 组合 C）。脚本 -> 包 -> 模块的成长路径草图（research/06 §1.10）。权衡：四者里脚本工效最好，直接命中作者钟爱的 TS+bun 工效。代价是双槽运行期模块格式（热重载）必须从第一天就在 VM 里，事后加装是要避免的错误（research/06 §7 组合 C），容忍循环的整包解析是实打实的编译器工作（按包有界）。排除：导入期副作用（有意），此外没有别的条目，这个模式叠加在 H6(b) 的存储与 G3(b) 的模块之上，复用二者的机制。启动/分发：经 mmap 的 dsmod 缓存朝 10ms 以内冷启动目标努力（research/06 §6.3 预算）。长大了的脚本获得一份清单，同时保留原来的文件。

### H6.d — 平台模型、可嵌入优先

包与构建的故事围绕嵌入组织：VM 是一个 C 库（`libds.so`），应用被托管在其他程序里，平台（H5.b）是带效应表层的分发单位，通用注册表无限延后（Roc 的实际轨迹，research/06 §2.6）。

```text
# H6.d -- illustrative
app [main] { pf: platform "acme/web-runtime" }   # Roc-style app manifest (simplified)
// the host embeds libds.so; scripts are store-fetched dsmod blobs; no registry needed
```

先例：Roc 以平台为中心的打包，以及它反复延后的通用包管理器（research/06 §2.6、§7 组合 D）。Lua 作为可嵌入脚本的普遍存在，是这条路的现实证明。权衡：把「作者关心的脚本目标」（嵌进 Rust/JS/C 宿主）当作组织原则，并与线性句柄（H5.a）组合出宿主持有资源恰好一次。代价是通用库生态会在平台故事成熟期间饿死，「平台版本」取代「库 semver」成为兼容单位，这对大数据脚本领域是不合适的匹配。排除：注册表优先的社区成长路径。作为单独答案它太窄，这也是 research/06 §7 把 D 评为附加轴的原因。启动/分发：嵌入就是产品（宿主二进制携带 VM）。平台与脚本的分发仍然需要存储，D 保留 H6(b)，只是改变它的优先级。

H6 对比：

| 轴 | H6.a Cargo 式 | H6.b 哈希存储 | H6.c bun 式脚本 | H6.d 平台优先 |
|---|---|---|---|---|
| 实现成本 | 中（注册表重） | 中（存储+物化器） | 低到中（运行期槽位） | 中（嵌入 API） |
| 第一天就需要注册表 | 是 | 否（延后成静态索引） | 否 | 否 |
| 与作者 UPM 的匹配 | 弱 | 强 | 中性 | 中性 |
| 回应的 Nix 痛点 | 部分 | 强（哈希级联、bin 优先、FHS） | 中性 | 中性 |
| 回应的 Python 痛点 | 强 | 强 | 强 | 中性 |
| 热重载 | 中性 | 强（缓存兼任） | 强（内建） | 强（处理器模型） |
| 排除 | 哈希存储式 UPM | 范围求解器易用性 | 导入期副作用 | 注册表优先的成长 |

**推荐 — H6(b) + (c)**：以哈希存储原生为核心，叠加 bun 式脚本模式（research/06 §7 推荐综合方案的 B+C）。(a) 的注册表日后以静态 JSON 索引的形式回到 (b) 的内容寻址产物之上。(d) 由 H5(b) 的「平台即包」设计顺带交付，不构成单独的生态决策。

### H6 必做 — 在每个选项下都生效的工具链承诺

下面五条是承诺，全部选项共同承担：research/06 §4 表明每一条对章程里 LSP 优先、以 IDE 为中心的作者都是承重的，而且每一条都恰好有两种可信的实现策略。

**1. 从第一天起编译器就是查询库（salsa 式）。** 编译器必须是一个库（`ds-check`），其 API 是记忆化的查询：parse、block_index、resolve、type_of、effects_of、linearity_check、lower。于是 LSP、REPL 和热重载是同一个消费者配不同驱动。Koka 是一份源码树同时承载编译器、解释器和 LSP 的现实证明（research/06 §4.1）。实现选项：(i) 红绿增量查询数据库（rust-analyzer/salsa 血统，持久增量，按 `///|` 块粒度失效），有原则且有验证，成本是早早建起查询基础设施。(ii) tsserver 式每项目一程序，配一份文档注册表在打开的文件之间复用 AST，特性按需计算，交付更快，历史上原则性较弱，在大图上会退化。两者都要求按包粒度失效，这也是禁止包循环的第二个理由（G3.b）。

**2. 格式化器是编译器组件。** `ds fmt` 零选项：gofmt/zig fmt/moon fmt 都证明零配置能终结风格争论，prettier/rustfmt/ocamlformat 证明选项会漂移（research/06 §4.2 的表）。实现选项：(i) 在带类型 AST 上做唯一的规范美化打印器，复用编译器的解析器（接近 rustc 的做法，但在树内）。(ii) 块感知的格式化器，利用 `///|` 模型（moon fmt）：独立格式化每个块，保持顺序无关，从而支持部分文件格式化，这条路同时也是 LSP 的输入即格式化路径。只有在选择 G3(b) 的块模型时才能用 (ii)，这条承诺直接依赖 G3。

**3. rustc 级诊断，用双区间渲染线性错误。** 结构化诊断：结构化建议以机器可读的插入/替换区间给出，`--error-format=json` 供 LSP/CI 使用。由于语言是线性的，还需要一个专门的诊断类，渲染「在此处移动 / 在此处再次使用」的双区间输出，也就是 rustc 的 E0382 移动/使用对，去掉借用相关的复杂度（research/06 §4.4）。实现选项：(i) 采用 `codespan-reporting`/`ariadne` 做渲染，诊断类型自研，很快就能得到好输出，渲染落后于 rustc 的新特性。(ii) 自研基于 derive 的 `Diagnostic`/`Subdiagnostic` 模型（rustc-dev-guide 血统），完全可控（结构化建议、JSON、双区间分组），编译器团队的工作更多。无论哪条，区间都必须穿过 IR 边界，在带类型 AST 层计算，对着源码渲染。

```text
error[DS2001]: value `h` was consumed here
  --> src/pipeline.ds:41:15
   |  41 |  let rows = Csv.read(h)     ^ `h` moved (File is ~Copyable)
   |  42 |  Log.write(h.path)          used again here after move
help: clone the path before the move            (suggestion emitted as JSON too)
```

**4. REPL 带线性会话上下文。** REPL 同时打印推断出的类型与效应行（Koka 的 tour），在同一个字节码 IR 上以会话堆解释执行，并解决唯一真正的新问题：提示符上的线性值不能被静默丢弃（research/06 §4.3）。实现选项：(i) 会话拥有的线性上下文，配显式 `:drop` 与 `:reset` 清理，这是研究的推荐。值跨提示符存活，并在复用时接受类型检查。(ii) 隐式消费的提示符，先显示再释放，相当于对提示符变量自动 `consume`，配 `:hold` 保留，更友好，代价是丢掉多行资源流水线。为了显示，选项 (i) 只自动消费被显示的那个值，从而覆盖 (ii)。两者都需要 docs/zh/design/03 的 E1d 处理器运行期，好让提示符上的效应由打印来处理。

```text
$ ds repl
> let h = File.open("data.csv")     # h : File -- owned by session context
> let rows = Csv.read(h)            # h consumed exactly once; recheck per prompt
> :drop rows                        # explicit; or auto-dropped at :reset
```

**5. Erlang 式双版本代码槽位用于热重载。** 运行期按模块持有带版本的槽位，当前与旧版，最多两个同时存活。`ds dev --hot` 重编译改动的块，替换槽位，旧版本的帧在下一次跨模块调用时退休，因为 DreamScript 的每次跨模块调用都是全限定调用，Erlang 的那条要求由此平凡满足。堆上的值对着新定义做类型检查，即 Mun 式 struct diff，但在 GC 堆上，没有 ABI 的负担，在这个 VM 里几乎免费。效应处理器捕获它被安装时的代码版本（research/06 §4.5）。没有导入期执行（G3.b）正是这一切成立的原因。实现选项：(i) 整模块双槽替换配帧老化，近乎免费的基线，对脚本规模的刷新够用。(ii) Julia Revise 式的方法实例失效，更细粒度，等 JIT 层落地后值得长成的方向（为运行中的任务提供 world-age 语义）。对 dsmod 的要求：稳定的模块名、头部的依赖列表（G3.5 已定）、带版本的槽位。

**关于这些承诺的启动/分发说明：** 查询式编译器同时让 G3.5 的缓存诚实（按块/内容哈希键控的增量降级），格式化器与 `.dsi` 生成器共享同一棵 AST（没有第二个解析器），双槽运行期就是启动路径用的那个模块加载器。这五条共用同一套实现。

## 一致的组合

三个命名的选项组合，每个都带一份清单、一段导入和一个 FFI 绑定，按作者实际的写法书写。推荐的是 Hash-core。

### 组合 1 —「Hash-core」（G3b + G4b + H5a + H5b + H6b + H6c 分层叠加）

```text
// ds.pkg -- manifest is the lock; no lockfile anywhere
name = "pipeline";  version = "0.4.0"
[deps]  std = { ver = "1.2", hash = "blake3:9f2c..." }  csv = { ver = "0.7", hash = "blake3:1a88..." }
[artifacts]  x86_64-pc-windows-msvc = { url = ".../pipeline.dsmod", hash = "blake3:77e0..." }

// pipeline.ds -- imports (selective, aliased, conditional) over a package-local cycle
import @std/csv (Reader)                       // namespace alias, MoonBit-style
import acme/pipeline/schema (Row, parse_row)   // selective, from the generated .dsi surface
import acme/pipeline/fs if os == "windows"

///|  // FFI: linear handle, consumed exactly once (H5.a on 02's C1.b layer)
extern fn sqlite3_open(path: *const c.Char) -> (c["sqlite3"] | c.Int)
extern fn sqlite3_close(db: c["sqlite3"]) -> c.Int      // double-close: compile error
```

结论：推荐，每一个具名痛点都被直接命中。Nix 得到哈希级联锁，无需锁文件，bin 优先，FHS 链接器。Python 得到包内循环、纯模块体、块粒度工具链。`#include` 由生成的 `.dsi` 按 diff 评审。LSP 优先由编译器即查询库满足，LSP/REPL/热重载共享同一个库。启动走 mmap 存储的快路径，bun 脚本模式是同一存储上的零成本叠加层（research/06 §7 综合 B+C+D）。

对 docs/zh/design/02 与 03 的影响：C1(b) 仿射 + `~Copyable` 与 C5(c) 线性析构变成硬性要求（H5.a 的句柄就是收益），C2 的参数模式承载 FFI 借用。E1d 的处理器运行期加上 E1c 的能力惯用法变成效应的基底，平台行、REPL 效应打印、B5d 错误并集都经 `.dsi` 文件流动。这些条目在两份目录里已经是推荐，现在只是变成承重项。

### 组合 2 —「Cargo-classic」（G3a + G4a + H5c + H6a）

```toml
# ds.toml + generated ds.lock; src/ layout; sparse-index registry
[package]  name = "pipeline"  version = "0.4.0"
[dependencies]  csv = { version = "0.7", features = ["simd"] }
```

```rust
// src/lib.ds -- mod tree, pub/priv, manual FFI discipline
mod transform;  mod io;
pub use crate::transform::parse_row;
extern fn sqlite3_open(path: *const c.Char, out: **c["sqlite3"]) -> c.Int   // close twice = bug
```

结论：熟悉的退路。常规工具链熟悉度最好，作者懂 Cargo，第一天就有注册表。代价是排除 UPM，背上特性分叉与锁文件漂移的税，Nix 痛点无人处理，线性层在 FFI 上的收益被降级，H5.c 让「必须恰好释放一次」退化为 lint。

对 docs/zh/design/02 与 03 的影响：02 里为 FFI 而设的 C1/C5 部分失去边界收益，线性性只对内部资源保留。03 几乎不受影响，E1d/E1c 只被必做的 REPL/诊断承诺需要，这个组合用不到它们。

### 组合 3 —「Platform-embeddable」（G3b + G4b + H5b + H6d，底层是 H6b 的存储）

```text
// ds.pkg (app): platform replaces a dependency list as the organizing entry
app [main] { pf: platform "acme/web-runtime@0.9" }      // platform version = compatibility unit

// main.ds -- effects arrive from the platform's declared row; checker audits the subset
fn main() -> { let rows = fetch_csv("in.csv"); console.print(sum(rows)) }
// binding story: same linear handles for native hosts; WIT world for wasm hosts
extern fn host_read_file(path: *const c.Char) -> (c.Bytes | c.Int)   // native glue (H5.a handles)
// world pipeline { import host-io: interface { read-file: func(path: string) -> ...; } }  -- wasm
```

结论：它是正确的附加轴，单独采用则不够。嵌入 Rust/JS/C 宿主成为产品，效应获得带类型的契约。代价是注册表优先的库生态会饿死（Roc 有记录的轨迹），而大数据脚本领域仍然需要 H6.b 的存储，所以 D 必须带上 B 才完整（research/06 §7 组合 D）。

对 docs/zh/design/02 与 03 的影响：E1d 的处理器运行期从「推荐」升级为硬性要求（平台行就是契约），E5 的资源/取消决策点与线性宿主句柄相互作用（C5.c 的析构在处理器作用域下运行），「标准库不拥有效应」这项文化承诺落在这里。

## 出处

主要证据：`docs/zh/research/06-modules-tooling-ffi.md` 第 1-7 节，涵盖模块词汇与循环分析 §1.1-1.11、包管理器与 UPM 评估 §2.1-2.7、构建系统 §3.1-3.4、工具链 §4.1-4.5、FFI/嵌入 §5.1-5.6、分发/启动 §6.1-6.4、组合 §7。作者输入：章程 `AGENTS.md`（痛点：Nix、Python 模块、`#include`，口味：钟爱 TS+bun 工效，不喜欢 Go/Lua）以及位于 C:/programs/me/lxl66566.github.io/src/coding/package_manager.md 的包管理器随笔（按包的锁、输出哈希存储、FHS 链接、bin 优先、P2P、`#OUT#`/`#TMP#`/`#ARCH#`）。为索引表而查阅的姊妹目录：`docs/zh/design/01-types.md`、`docs/zh/design/02-linearity-ownership.md`（C1-C5 的推荐）、`docs/zh/design/03-effects-errors-async.md`（B5、E1 的推荐）。research/06 里内联的网络来源（Zig、Inko、pnpm、MoonBit/mooncakes、rust-analyzer/salsa、Erlang 代码加载、Mun、Julia Revise、Bun --bytecode 基准、PEP 552、AppCDS、WIT/Component Model、Lua C API、Python C API、cargo#10489）在那里引用，此处不重复。
