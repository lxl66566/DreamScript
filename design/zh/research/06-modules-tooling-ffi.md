# 06 模块、包、构建、工具链、FFI 与分发

为 DreamScript 撰写的研究笔记（2026-11-04）。输入：作者的 PL 哲学随笔（模块即解耦，反感 `#include` 式的文本复制，反感 Python 的模块粒度）与包管理器随笔（Nix 的痛苦：输入哈希存储、flake `follows`、全局锁打断下游、没有 FHS、内置文档上手陡。哈希存储 UPM 提案：bin 优先、逐包依赖锁、带哈希前缀的单一存储、FHS 链接、P2P 分发）。章程约束：解释执行的字节码 IR，JIT 可选，线性与仿射类型，默认不可变，效应可选，语法糖天花板为 Kotlin，解耦优先（LSP/RISC 模型）。证据：本地克隆的 inko、koka、luajit、moonbit-core、wasmtime（只读），以及就地标注的网络来源。约定：每段一行，代码块与表格例外。

## 概要

模块：对 DreamScript 最强的先例是分层模型，文件 = 模块，目录 = 包，包 = 依赖与分发单元。配套有三样东西：**符号级导入列表**（Inko `import mod1.mod2 (A, B)`，MoonBit `import { "moonbitlang/core/builtin" }`）、**生成的接口文件**（MoonBit `.mbti`、OCaml `.mli`），以及**逐包的循环规则**。接口文件同时充当 API 差异的审阅对象。循环依赖方面：Rust 允许 crate 内部的模块循环（名字解析在整个 crate 范围进行），Go 禁止一切循环（换来快速的 DAG 编译、定义清楚的 `init()` 语义、以及对设计异味的强制暴露），ESM 允许带 live binding 的循环，但部分初始化会触发 TDZ 崩溃，Python 的导入即执行模型让循环在运行期才失败。对 DreamScript 的结论：允许包内模块之间的循环（像 Rust/Nim 那样在编译期解析），禁止包之间的循环（像 Go/Inko/MoonBit，MoonBit 核心库文档说明了共享 trait 为何住在 `builtin` 里以避免循环）。理由是包级循环会破坏增量编译与热重载，而模块级循环恰好能解决作者「无法互相引用」的痛苦，且不必引入样板化的接口包。

包管理：三个可行的后端已经在向作者的哈希存储思路收敛。pnpm 是内容寻址存储加硬链接再加新的全局虚拟存储。Zig 用 build.zig.zon 以 URL 加哈希描述依赖，并配全局哈希寻址缓存。Inko 的清单直接内嵌版本与提交哈希，清单本身即锁。作者的提案（逐包锁、输出哈希存储、FHS 链接、bin 优先、P2P）几乎完全能用现成机制实现：pnpm 证明了文件级 CAS 加硬链接物化，Zig 证明了没有锁文件时哈希即锁，Nix 证明了符号链接 profile 的链接方式，而它 `follows` 的痛苦恰好说明应当采用 Zig 式的依赖哈希级联，代价是放弃用锁图钉住版本。Bazel 证明了以输出为键的动作缓存。真正新的部分只有两处：一是跨语言且 bin 优先地做这件事，二是 P2P 分发。两者都可行，只是 DreamScript v1 可以先把它们推迟。语言本身只需要清单格式、MVS 解析和存储路径约定。

构建：DreamScript 应当只发布一个 `ds` 二进制，构建层用 Cargo 风格的声明式清单，并留一个同语言逃生口，直接在编译器已经内嵌的 VM 里运行。Zig 需要编译出 build runner，解释器只需直接求值。对解释型语言来说，声明式默认加代码逃生口的成本更低：90% 的场景（`ds run script.ds`、`ds test`、`ds build --target wasm`）必须像 `bun` 一样零配置，交叉编译应当借 Zig `cc` 当链接器（Inko 文档里的做法），代价是放弃自建工具链矩阵。

工具链：LSP 优先属于编译到 IR 的架构决策，实现方式是让编译器本身充当库。rust-analyzer 与 salsa 证明了基于查询的「编译器即库」设计才能支撑 IDE 级别的增量检查，DreamScript 的字节码 IR 流水线从第一天起就应当组织成记忆化查询。格式化器、接口生成器（`ds info`）、诊断渲染器（codespan-reporting/ariadne 风格）和 REPL 都应当是编译器的组件，MoonBit 的 `moon fmt`/`moon info` 工作流是当下最好的模板。热重载方面，对解释型语言来说，Erlang 的双版本代码模型（当前版与旧版共存，全限定调用切换代码，purge 语义回收旧版）是正确的运行期模块格式契约，成本远低于 Mun 用 C-ABI 装配做状态迁移。Julia 的 Revise 说明只要方法失效是增量的，REPL 驱动的重定义就完全可行。

FFI 与嵌入：C ABI 作为通用的底层（Rust `repr(C)`、Zig `export`/`@cImport`）。从 Lua 的栈加注册表 API 和 Python C API 的持有、借用引用坑里学到，互操作层要用句柄来替代指针。WIT/component-model 作为 wasm 宿主上可选的带类型边界。Roc 的平台模型与 DreamScript 的效应设计在概念上最契合：平台提供分配器、效应与宿主函数，语言把它们当作效应行消费，边界处与线性类型正好接上。跨 FFI 的线性句柄必须恰好消费一次，这是需要强制执行的特性。

分发：字节码缓存按源码哈希校验（PEP 552 风格，不用 mtime），可以 mmap 加载（Java AppCDS 证明了收益），单文件模式内嵌字节码（Bun `--compile --bytecode` 相对纯打包实测启动快 1.38x）。LuaJIT 在 `lj_bcdump.h` 里定义的 `bc` 格式是最小先例：magic、版本字节、ULEB128、strip 标志。wasm 作为一等分发目标（MoonBit 用目标后缀文件从一棵源码树同时产出 wasm/js/native）。Windows 上的 PATH 采用 shim 策略（scoop 式），用桩程序替代符号链接。

最后一节列出四种可行的生态组合：A 是 Cargo 风格的清单加注册表。B 是原生哈希存储（推荐：清单即锁、输出哈希存储、FHS 物化器，把作者的 UPM 建立在已验证的机制上）。C 是 bun 风格零配置脚本生态，模块允许循环并带热重载。D 是平台模型的可嵌入形态（Roc/Koka 混合）。B 与 C 相容，建议同时采用。若希望早期就有注册表基础设施，则退回 A。D 是附加的设计轴，可以叠加在任一组合之上。

## 1. 模块系统

### 1.1 先厘清词汇

有三个不同的概念常被混为一谈，每门被考察的语言划的线都不一样。作者的「`#include` 之痛」正是语言缺少前两者的结果：(a) **命名空间与可见性单元**，即哪些名字住在一起、谁能看见它们。(b) **编译单元**，即编译器能独立做类型检查与缓存的范围。(c) **分发与依赖单元**，即包管理器解析和定版本的对象。C 三个都没有（头文件就是文本包含）。C++20 的模块把 (a) 与 (b) 补上了，但工具链成熟度很糟。Python 在目录粒度上有 (a)，语义却是运行期执行。Rust 三者齐备，但 (b) 粒度偏粗。MoonBit 三者齐备，边界也划得清楚。DreamScript 应当明确写出每个构造服务于哪一层。

被考察系统的映射表：

| 语言 | 命名空间单元 | 编译单元 | 分发单元 | 接口 |
|---|---|---|---|---|
| C/C++（20 之前） | 文件 / 头文件 | 翻译单元（文本粘贴） | 库 / 归档文件 | 头文件（纯文本，没有类型安全边界） |
| Rust | 模块（crate 内部） | crate | 包（Cargo） | 没有显式接口，靠 `pub` 可见性加 rustdoc |
| Go | 包 | 包 | module（go.mod，含多个包） | 首字母大写即导出 |
| OCaml | 模块 / 编译单元 | `.ml` 文件 | opam 包 | `.mli` 签名文件 |
| Ada | 包（spec 加 body） | 包体 | 库 | 包 spec（语言级） |
| Nim | 模块 | 整个程序（惰性） | nimble 包 | 只有 `export` 标记 |
| Zig | 文件 | 惰性分析的文件图 | 包（zon tarball） | 没有接口，只有声明级可见性 |
| ESM/TS | 文件 | 文件（每个项目一个打包器） | npm 包 | `.d.ts` 声明（生成或手写） |
| Python | 模块文件 / 包目录 | 模块（导入时执行） | wheel/dist | 没有接口（只能运行期内省） |
| Unison | 定义 | 定义（哈希） | codebase 命名空间 | 类型在哈希里（带类型的 AST） |
| MoonBit | 文件（块结构） | 包（目录） | module（moon.mod.json，注册表） | 生成的 `.mbti` |
| Inko | 文件（模块） | 包 | 包（git 仓库） | 没有显式接口，靠 `pub` 可见性 |

### 1.2 Rust：crate、模块与路径

Rust 的 **crate** 是编译与分发单元（Cargo 负责解析与定版本）。**模块** 是 crate 内部的命名空间与可见性边界，用 `mod` 声明，与文件布局一一对应（`mod foo;` 读取 `foo.rs` 或 `foo/mod.rs`）。可见性是 `pub`、`pub(crate)`、`pub(super)`、`pub(in path::to::)`。有两点对 DreamScript 重要。第一，**名字解析在 crate 范围内进行，且容忍循环**：同级模块里的条目可以自由互相引用（包括互引用），因为 rustc 把整个 crate 当作不动点来解析，模块循环合法。只有 crate 之间的循环会被拒绝（Cargo 对循环的包依赖报错）。第二，**隐私性与文件正交**：可见性单元由模块提供，文件只负责存放，所以在文件之间搬代码属于中性重构，这就是 Rust 代码库不会出现 Python 式「模块管道」改动的理由。代价是：`mod` 声明是样板，路径语法（`crate::`、`self::`、`super::`、2018 版的 `use crate::...`）是 Rust 较陚的角落之一，Rust 2024 版甚至不得不预留带版次前缀的路径来给语法留出空间。代价是：`mod` 声明是样板，路径语法（`crate::`、`self::`、`super::`、2018 版的 `use crate::...`）是 Rust 较陡的角落之一，Rust 2024 版甚至不得不预留带版次前缀的路径来给语法留出空间。

```rust
// Rust: crate = package; modules mirror files; cycles between a.rs and b.rs are fine.
mod a;            // reads src/a.rs
mod b;            // reads src/b.rs
pub(crate) fn helper() {}   // visible crate-wide, not to dependents
// in a.rs:  use crate::b::B;      -- b may equally `use crate::a::A`
```

对 DreamScript 的启示：采用 crate 范围的解析与循环容忍，去掉 `mod` 声明（文件即模块，路径即名字），保留一套小的可见性语法（`pub`、`pub(pkg)`、私有）。

### 1.3 OCaml、Nim、Ada：层级模块与接口

OCaml 的编译单元与文件一一对应即是模块，显式接口文件（`.mli`）给每个模块一个签名。它的模块语言很深：模块近似一等值，functor 用模块参数化模块，签名的角色是抽象单元（透明与不透明）。编译单元之间不能互相递归（链接顺序是 DAG），要打断循环就得抽出一个共享的下层单元，或者用递归模块（少见且笨重）。Ada 的 spec/body 分离在业内划界最明确：包 **spec** 是客户端编译所依赖的接口，包 **body** 是实现，两者之外还有层级化的 **子包**（`Ada.Strings.Unbounded`），子包能看到父包的私有部分。可见性沿层级向下流动，这正是作者随笔里说的良好解耦「嵌套模块」。

```ada
package Stack is                  -- spec: the contract clients compile against
  type Stack is limited private;
  procedure Push(S : in out Stack; X : in Integer);
private
  type Stack is record Top : Integer := 0; Buf : array (1..256) of Integer; end record;
end Stack;
package body Stack is ... end Stack;          -- body: freely refactorable
package Stack.Bounded is ... end Stack.Bounded;  -- child package, sees parent's private part
```

Nim 在循环问题上是有用的反例：Nim 明确 **支持模块间的循环导入**（编译器编译整个程序，名字解析推后），这消掉了接口包的样板，代价是编译器必须做全程序分析。对 Nim 可接受（它本来就通过 C 编译整个程序），对已经加载全程序字节码的解释型 DreamScript 更是直接相关。Nim 还展示了 `import`/`from`/`include` 三分法：`include` 就是文本粘贴（作者最反感的），`import` 引入模块命名空间（配合 `export` 再导出），`from X import y` 是按符号选择。`include` 应当远离 DreamScript 语言的任何角落。

### 1.4 1ML 与一等模块

1ML（Rossberg，ICFP 2017）说明 ML 模块与核心语言可以统一，且实际程序不丢类型推断：每个表达式都是模块，每个模块都是值，functor 就是函数，签名就是类型，新鲜性由定型产生，每次应用都得到新的抽象类型，独立的 functor 层因此消失。对 DreamScript 的教训落在统一本身的价值上，采用 ML 模块只是其中一条路。统一换来的是一个概念而非三个（值、类型、模块三个层级塌缩），这符合章程的正交原则。也要看清代价：检查器里需要精细的依赖类型风格类型论（小宇宙类型），与「短教程即可完整解释」的预算冲突。Kotlin/TS/Rust 的务实路线走中间：模块只作命名空间，代码复用靠函数与泛型，模块参数化让位，functor 的真实用例（参数化实现、可 mock）在线性加效应的语言里由 trait 与参数化效应覆盖。

### 1.5 Go：包与循环禁令

Go 的包 = 目录加共享命名空间。同一个包里的文件零仪式地看见彼此的顶层声明（同包文件之间不用 import，这条值得抄进包内工效学），`import` 走规范路径，导出靠首字母大小写。包之间的循环是编译错误。公开给出的理由（golang/go 讨论，另见 DoltHub 2025 那篇关于避免循环策略的文章）有三条：编译沿 DAG 逐包进行并因此并行。包 `init()` 函数在循环下语义无法定义，可能递归。禁令本身是设计异味探测器，逼着做依赖倒置。反面证据是：golang/go#49251 与 DoltHub 那篇策略文记录了大量真实代码不得不靠接口间接、共享的「types」包（臭名昭著的 `pkg/types` 垃圾场）或函数值注入来伪造循环，这正是作者在 Python 里反感的样板。注意 Go 的 `init()` 是承重的约束：ESM 证明只要模块初始化是一套有定义、有顺序的协议，循环就没问题（见 1.7）。

### 1.6 Zig 与 TypeScript/ESM：文件即模块，显式导入

Zig：模块就是文件，`@import("path.zig")` 把文件当作结构体（命名空间）返回。导入是显式的，没有包级通配，文件之间可以互相递归，因为分析是惰性的（声明只在被引用时才分析），这是 ESM live binding 在编译期的类比。Zig 标准库本身就展示了这套纪律的代价：`std.Io` 重组落地时（0.15），代码被拆开，阻塞与非阻塞类型分别住在可导入的独立模块里。显式导入图可重构，但每条边都要人维护。TypeScript/ESM：静态 import/export 语法，带 **live binding**，按 MDN 的定义，被导入的名字是指向导出模块那个绑定的引用，按 jameshfisher.com 的说法可以理解为「所有模块拼接到同一个作用域」，求值是依赖图的深度优先后序。因此循环可以解析：函数声明会被提升，跨循环可调用。但在导出方求值完成之前访问 `let`/`const`/class 导出会抛 `ReferenceError`，落入 TDZ 死区，这就是 ofriperetz.dev 那篇文章说的「能通过所有工具链检查的 bug」。CommonJS 返回的是 `module.exports` 的快照副本，循环于是「能用」，但埋了陈旧值的坑。

```js
// a.mjs imports b.mjs, b.mjs imports a.mjs -- legal ESM
// a.mjs:  import { later } from "./b.mjs";  export const ready = 1;  later();  // OK: hoisted fn
// b.mjs:  import { ready } from "./a.mjs";  export function later() { return ready; }
//          console.log(ready);  // ReferenceError: TDZ -- a hasn't finished evaluating
```

Deno/bun 沿用 ESM 语义，加了 URL 与注册表说明符（`jsr:`、`npm:`）和 import map（`deno.json` 的 `"imports"` 把裸名映射到 URL）。这是零配置的模块解析层，DreamScript 可以用内置的标准库命名空间替代 URL 来模仿它。

### 1.7 Python：作者之痛的具体机制

作者的抱怨（随笔原文：「模块粒度不够，无法交叉引用，本质上是增强版代码复制」）可以拆成五个可核查的机制。(1) **导入即执行**：模块体是任意脚本，运行一次后缓存进 `sys.modules`。「模块」是运行期可变的命名空间字典，编译期在这里不设边界，于是不存在类似接口的东西（`.mli`/`.mbti`/头文件）。API 表面等于运行期存活下来的东西，只能靠执行来发现。(2) **循环在运行期而非编译期失败**：导入成环时一方会看到只初始化了一半的模块，`from x import y` 会复制绑定，若 `y` 还没定义就抛 `AttributeError`/`ImportError`。这就是「把 import 挪进函数里」这种变通文化的来源。(3) **粒度是目录或文件，可见性靠约定**：`__init__.py` 里写再导出样板，下划线前缀表示私有，没有逐符号的 `pub`，因此解耦只在社会层面被强制执行。(4) **基于路径的解析对环境敏感**：`sys.path` 遍历、`site-packages` 拍平的命名空间、editable 安装、命名空间包，这些是「在我机器上能跑」以及 venv/uv 泛滥的根因。(5) **两套并存的语义**（脚本与模块：`__main__` 与导入）意味着同一个文件按入口不同而行为不同。

```py
# a.py:  from b import g        # b executes first
# b.py:  from a import f        # a is partially initialized (in sys.modules, body not done)
#         -> ImportError: cannot import name 'f' from partially initialized module 'a'
# idiomatic fixes: move import inside function; or extract a third module -- boilerplate
```

评判 DreamScript 的模块设计，就看它是否复现了这五条中的任何一条。其中 (1) 最深：如果模块体是纯编译期声明（像 Rust/Go/MoonBit），包内循环就降级成名字解析的细节，运行期隐患随之消失。

### 1.8 Unison：内容寻址的定义

Unison 把每个定义存成其带类型语法树的哈希，哈希里包含依赖的哈希（unison-lang.org「The Big Idea」）。codebase 是只追加的 CAS，名字只是元数据，文件根本不存在，代码以文本展示但以结构化形式存储。后果是：重命名与搬移不会冲突，哈希不变。依赖的「版本化」是内生的，改动一个定义会得到新哈希，下游的哈希随之传递更新。合并是定义级的集合并。哈希无歧义地标识可运行代码，从而支持安全的远程执行（微服务即单体）。代价是：整套工具链都要自建（没有纯文本文件生态，面向文本的 git 被绕开，编辑器要通过同步协议工作）。程序文本无法按行做差异，Unison 自己的 UCM 差异是语义级的。针对文件的临时脚本（grep、patch、对文本的 LSP）都得重造。LWN 2023 年的报道与 Unison 1.0 的发布说明确认这套模型活到了 1.0，但仍是小众。

```
-- Unison sketch: hash replaces name; `ability` = effect
unique type Counter = { current : Nat }            -- stored as #abcdef...
increment : Counter -> Counter                     -- hash covers body + deps' hashes
```

对 DreamScript 来说，这是一条独立的设计轴：作者自己的 UPM 随笔要的是哈希寻址的 *包与输出*，定义本身仍以名字为身份。采用 Unison 的完整模型会排除作者日常赖以生存的纯文本文件文化（VSCode、grep）。可以低成本借用的是：字节码缓存里的编译器 IR 产物按源码哈希内容寻址（第 6 节），*包* 的身份用哈希寻址（2.7 节），定义的身份保持不变。

### 1.9 MoonBit：现代小语言的模板（证据来自 moonbit-core）

从 moonbit-core 克隆读到的具体事实：MoonBit 的组织是 **module → package → file**。仓库根目录的 `moon.mod.json` 描述一个 module（名字、版本、依赖），每个目录是一个包，`moon.pkg` 的内容通常只有一张导入列表（例如 `moonbit-core/abort/moon.pkg` 只含 `import { "moonbitlang/core/builtin" }`），包可以用别名导入（`@json`）。`moon info` 为每个包生成 **接口文件**（`pkg.generated.mbti`），`moonbit-core/abort/pkg.generated.mbti` 在 Values/Errors/Types/Traits 这些小节下只列了一条 `pub fn[T] abort(String) -> T`。仓库的 AGENTS.md 要求贡献者把 `.mbti` 差异当作 API 变更的审阅物（原文：「If nothing in .mbti changes ... it is typically a safe refactoring」）。源文件是 **以 `///|` 分隔的块结构**，文档说明块的顺序无关，于是工具可以做到块粒度，重构也容易（这直接回应作者的粒度抱怨：编译器的关注单元在文件层级之下）。条件编译用文件名后缀按后端与目标区分（`double_mod_js.mbt`、`double_mod_nonjs.mbt`、`double_ryu_wasm.mbt`），没有预处理器，靠文件名选目标。测试与代码相邻：黑盒 `*_test.mbt`（以客户端身份导入该包）与白盒 `*_wbtest.mbt`（能看到私有条目），快照测试通过 `inspect` 与 `moon test --update` 完成。mooncakes.io 注册表（2024 年 2 月公布，到 2026 年约 3.5k 个 module）托管 module，并为每个版本生成文档。这是现存生态中最贴近 DreamScript 声称价值观的一个：自足的工具链、小而正交的部件、LSP 优先、中文社区势头。

```json
// moonbit-core/abort/moon.pkg (verbatim): a package's whole config is its import list
{ "import": ["moonbitlang/core/builtin"] }

// moonbit-core/abort/pkg.generated.mbti (verbatim, abridged): generated, reviewable API
// package "moonbitlang/core/abort"
// pub fn[T] abort(String) -> T
```

### 1.10 大数据工作负载下的脚本语言需要什么模块粒度

大数据脚本（作者在自己站点上从事的领域）需要四件事。一是便宜的顶层脚本，bun 风格，不需要包脚手架。二是把脚本长成包的低摩擦路径，TS/bun 下同一个文件被别人导入就行，Go 下同一个包多写几个文件就行。三是在多 GB 数据集上做细粒度增量复查，因为改一处就要重跑整条流水线，这偏向块级相关性追踪，即 MoonBit 的 `///|` 块与 salsa 式依赖追踪。四是跨重载的数据模式稳定，这偏向默认不可变且布局显式的结构体，同时也服务热重载（见 4.5 节）。推荐的粒度：**定义（函数与类型）的可见性与身份 < 文件（模块）< 目录（包）< 仓库（module 与注册表单元）**，包是唯一的无环边界，包内文件按一个命名空间解析（Go 风格，包内不写 import）。这样既得到 Python 那种零样板的成长路径，又不带上 Python 的运行期命名空间语义，同时符合随笔里「把小模块组合成大模块」的意思，而且不要求手写接口文件（接口像 MoonBit 那样生成）。

DreamScript 的成长路径草图：

```
script:  ds run pipeline.ds                         # no manifest, deps auto-resolved
package: pipeline.ds + transform/ directory         # same file, now imported by others
module:  ds.pkg at repo root { name, deps, hash }   # registry/publishable unit
```

### 1.11 循环：决策分析

先例表：

| 系统 | 模块循环 | 包循环 | 机制与成本 |
|---|---|---|---|
| Rust | 允许（crate 范围解析） | 禁止 | 逐 crate 的不动点名字解析，保住按 crate 并行编译 |
| Go | 不适用（文件共享命名空间） | 禁止 | DAG 构建加并行加 `init()` 语义，逼出接口间接的变通 |
| ESM/TS | 允许 | 允许（有警告） | live binding 加深度优先求值，过早访问 `const` 触发 TDZ 坑，打包器行为分歧（webpack#10870） |
| Python | 语法上允许，实际上失败 | 允许 | 导入即执行，半初始化的模块对象，AttributeError 变通 |
| OCaml | 禁止（编译单元） | 禁止 | 显式 `.mli` 接口，DAG 链接顺序，递归模块作为逃生口 |
| Nim | 允许 | 允许（程序内） | 名字解析推后，全程序编译 |
| Inko | 允许（模块） | 禁止 | MVS 包 DAG，带符号列表的 `import` 路径语法 |
| Zig | 允许（惰性分析） | 近似禁止（build.zig.zon 描述的依赖图是 DAG） | 惰性分析声明 |
| MoonBit | 允许（块解析推后） | 禁止（见下文 builtin trait 规则） | 块级解析，注册表 DAG |
| Unison | 无意义（定义是集合） | 无意义 | 哈希在构造上就是 DAG |

MoonBit 核心仓库给出了禁止包级循环最锋利的工程理由：AGENTS.md 说明只有当 trait 住在 `builtin` 里（每个包本来就依赖它）时，提升它的方法才安全。把更高层的 trait `@json.ToJson` 提升到低层类型上「会形成循环（builtin → json 等）」，所以从不这么做。这就是要抄的模式：**把共享抽象（trait、错误类型、模式类型）放进一个通用基础包来避免循环**，编译器的循环诊断应当提示「把 `X` 移到 `core`」。

一个 DreamScript 大数据脚本的具体模块图，模块设计要让这种图直接成立：

```
pipeline.ds ──> transform/ ──> io/ ──> @std/io          # intra-package: cycles allowed
    │               │                          │
    └───────────────┴──────> schema/ ─────────┘        # shared types live low
                 (package graph stays a DAG: pipeline -> transform -> schema -> std)
```

对 DreamScript 的推荐：允许包内模块（文件）循环，采用 Rust 式的整包解析。禁止包循环，并给出指向明确的错误信息。让标准库的基础层（类型、trait）不依赖任何包，保证逃生口始终存在。这直接回答作者的 Python 交叉引用之痛，增量编译仍以包为单位逐包进行，热重载本来也需要它（4.5 节）。

导入语法汇总（值得抄的部分）：

| 特性 | 例子 | 来源 | 采用？ |
|---|---|---|---|
| 按符号选择导入 | `import mod1.mod2 (A, B as C)` | Inko syntax.md（原文照抄） | 是 |
| 带别名命名空间的导入 | `import @json`，之后 `@json.from_json` | MoonBit | 是 |
| 条件导入 | `import foo if linux and amd64` | Inko syntax.md（原文照抄） | 是 |
| 外部 C 链接 | `import extern "m"` | Inko syntax.md（原文照抄） | 是，映射到 FFI（5.1） |
| 再导出 | `pub use crate::x::*` | Rust | 以 `pub import` 形式采用 |
| 目标后缀文件 | `foo_js.ds`、`foo_nonjs.ds` | MoonBit（文件名原文照抄） | 是 |
| 文本包含 | `include "x.nim"` | Nim | 永不 |

## 2. 包管理器与版本

### 2.1 Cargo：声明式的基线

`Cargo.toml`（清单：名字、版本、带 semver 区间的依赖、features、按目标条件的依赖、workspace）加 `Cargo.lock`（精确解析出的依赖图与校验和，应用会提交它）。解析把 SemVer 兼容的版本统一成一个，但同时允许不兼容的主版本共存（像 Go/Inko）。解析器（基于 PubGrub）偏好允许范围内最高的版本。锁文件让构建在同一 workspace 内可重现，封闭范围到源码依赖为止：构建脚本与 proc-macro 会执行任意代码，缓解办法是 vendoring 与 `cargo fetch --locked`。**features 是这套设计的疣**：它们必须是可加的，开启一个 feature 只能增加功能，改变功能则超出约定。而且 features **在整个依赖图上取并**，对一个 crate 的每次构建都会开启所有请求方 features 的并集（见 cargo#10489）。这会产生「我的依赖用我根本没要求的 feature 编译了 serde」，也让感知 feature 的缓存更难做。Cargo 自己的文档说明，生成锁文件时的依赖图是按所有 workspace features 全部开启来构建的。

```toml
# Cargo.toml shape
[package]
name = "foo"
version = "0.3.1"
[dependencies]
serde = { version = "1.0", features = ["derive"] }   # range -> lockfile pins 1.0.219
json = "2"                                            # major 2 may coexist with another dep's json = "1"
```

对 DreamScript：采用清单加锁文件的形状（或它的哈希级联等价物，见 2.7），采用多主版本共存，不过可以考虑干脆放弃 features。解释型语言可以用多个包来表达变体。字节码语言里的 features 会重新引入条件编译，模块系统随后还得按配置逐一做类型检查。MoonBit 改用文件名后缀，Inko 改用 `import foo if linux and amd64`，两者都更简单。

### 2.2 npm/pnpm/bun：值得抄的存储机制

npm 的经典布局（每个项目的 `node_modules` 里是一棵嵌套树，去重不彻底）引发了「node_modules 大战」，pnpm 用 **全局内容寻址存储** 解决了它：文件按内容哈希只存一份，每个项目的 `node_modules/.pnpm/<pkg@ver>/node_modules/<pkg>` 里是对存储文件的 **硬链接**，粒度是文件而非目录，APFS/Btrfs 上用 reflink，顶层的直接依赖是指向 `.pnpm` 的 **符号链接**。于是同时得到严格隔离与 O(1) 磁盘占用：一个包只能 require 它声明过的依赖，磁盘上每个内容只占一份（pnpm.io 的动机页与符号链接结构文档）。2025 年前后的两项进展直接对应作者的 UPM 草图：pnpm 的 **全局虚拟存储** 把存储文件硬链接到一个全局位置，再把项目符号链接进去，省掉逐项目物化。bun 在保留 workspace 支持的同时，把二进制的 `bun.lockb` 换成了纯文本 `bun.lock`（1.2+）。吃过的苦给出了两条约束：硬链接要求存储在同一卷上，Windows 的 NTFS 硬链接可用，但 junction 与符号链接有权限细节，pnpm 文档里写了回退方案。另一条是 CAS 的完整性需要逐文件哈希并在抓取时校验，pnpm 锁文件 v9 按文件记录完整性。

```
node_modules/
  .pnpm/
    lodash@4.17.21/node_modules/lodash/index.js   # hardlink -> ~/.local/share/pnpm/store/v10/files/ab/cd...
    left-pad@1.3.0/node_modules/left-pad/...
  lodash -> .pnpm/lodash@4.17.21/node_modules/lodash   # symlink: only direct deps visible
```

对 DreamScript 可迁移的想法是：**按哈希存一份，用硬链接物化，用符号链接图做隔离**。这基本就是作者的后端提案，且已在主流生态里被验证。

### 2.3 Zig：哈希即锁，没有注册表，没有锁文件

Zig 的包管理器（0.11 起稳定，文档：ziglang.org/learn/build-system/dependencies）把依赖在 `build.zig.zon` 里声明为 **URL 加哈希**（一个 tarball，通常是某个 git ref）。`zig fetch` 把内容缓存进 **全局缓存**（`~/.cache/zig/p/<hash>`），哈希按声明里 `paths` 过滤后的文件集计算，内容一变构建就失败，因此 **清单就是锁文件**：没有单独的锁，没有注册表，任何静态文件服务器或 git 都能用。关键点在于，Zig 算的 **包哈希包含该包依赖的哈希**，所以依赖一变，每个下游包的哈希都级联更新。作者想要的内容级联（「更新单个包绝不能打断上游」）正是这样在没有锁文件的情况下实现的：清单在构造上就钉住了传递依赖的身份。升级要手动跑 `zig fetch --save`（用占位哈希时错误信息里会打印新哈希），相对 `cargo update`/`moon update` 这是一处工效学缺陷。中心注册表按设计不存在，发现靠 URL（社区索引是第三方做的）。

```zig
// build.zig.zon (shape)
.{
    .name = .mylib,
    .version = "0.2.0",
    .paths = .{ "build.zig", "build.zig.zon", "src" },   // hash covers exactly this
    .dependencies = .{
        .zap = .{ .url = "https://github.com/zigzap/zap/archive/refs/tags/v0.9.1.tar.gz",
                  .hash = "1220faf8e9fce1955636b6bd6f78f4d7c8b3e6b1e6e8e2a1a5f4c3d2b1a0987654321" },
    },
}
```

对 DreamScript 来说，这是作者随笔最强的先例：它用两个字段的清单格式同时展示了逐包锁、输出哈希寻址的存储、多版本共存（哈希不同即可）与不绑定 FHS 的链接。

### 2.4 Inko：MVS 加 git 包，清单即锁（证据来自 inko/docs）

Inko 的 `inko.pkg` 清单（读自 `inko/docs/guides/packages.md`）只有一条命令：`require URL VERSION CHECKSUM`，例如 `require github.com/alice/json 1.0.0 f3f378ad8ea4b617401b40ace743614995904755`。版本加提交 SHA1 内嵌在清单里，于是清单同时是声明和锁，也没有注册表，包就是 git 仓库。版本选择用 **最小版本选择**（Russ Cox 的 MVS）：每条要求是一个下界，解析取所有下界的最大值，没有求解器，也不需要锁文件，效果是「永远不会用到你没测过的版本」。多主版本可以共存，foo 用 json 1.x 时 bar 仍可用 json 2.x。0.x 版本之间视为兼容，避免依赖爆炸。安全更新需要显式上调下界，文档里承认这是权衡。依赖被 vendor 进本地 `dep/` 目录，由 `inko pkg` 管理。Inko 也带着两笔设计债：SHA1，钉提交够用，但弱于 blake3。版本写在清单里意味着每次升依赖都是一处源码差异，而这恰好就是作者逐包锁性质得以成立的原因。

### 2.5 Deno：URL 导入、import map、JSR，以及向 node_modules 的退让

Deno 1.x 用 URL 导入，配 `deps.ts` 约定与 `deno.lock`。Deno 2（2024 年 10 月）掉头，加入原生 `package.json`/`node_modules` 兼容，口号是「直接跑你现有的 Node 项目」，因为生态引力胜过 URL 纯粹性。接替的想法是 **JSR**（jsr.io，随 Deno 1.42/2024 推出）：一个以 TypeScript 源码为载体的注册表。包以源码形式发布，文档由 JSDoc 派生，没有 tarball 构建步骤，可以写成 `jsr:@scope/pkg@version`，Deno、Node、bun 以及 pip 式工具都能导入，Deno 侧的接入点是 `deno.json` 的 import map 条目。`npm:` 与 `jsr:` 说明符并存，文档里写明一处细节：`jsr:` 说明符经 import map 解析，node_modules 里出现的是 `npm:` 包（deno#33370）。

```jsonc
// deno.json: the whole resolution layer is an import map
{ "imports": { "@std/path": "jsr:@std/path@^1.0.0", "lodash": "npm:lodash@^4.17.21" } }
```

对 DreamScript 的教训：**只讲 URL 导入走不通**，Deno 自己放弃了这条路。带生成文档的生态自有注册表可行，JSR 与 mooncakes 都做到了。import map 说明裸名解析可以住在一个小配置文件里，代价是放弃遍历目录的解析算法。

### 2.6 Unison、Roc、MoonBit 的注册表

Unison 根本不需要版本，哈希就是版本，共享是按哈希在 codebase 之间抓取。Roc 的包方案还在围绕 **平台** 成形：应用声明一个平台，平台提供效应。通用库包管理器被反复推迟（见 roc-lang 的 FAQ 与 zulip）。这说明平台式架构会复杂化包模型，因为兼容单元是「平台版本」，库的 semver 在此之上再叠一层。MoonBit 的 mooncakes.io 是反例：一个常规注册表，`moon add`、`moon install`、`moon publish` 齐备，并在 mooncakes.io/docs/... 为每个版本生成文档，为一门年轻语言在两年内建成。注册表如今是通用基础设施（GitHub API 加 tarball 加静态文档），护城河的位置已经让出。对 DreamScript：注册表可以晚些再加，做成静态文件服务器加 JSON 索引（Cargo 的稀疏索引模型），清单与存储的设计必须先落地。

版本模型对比：

| 模型 | 系统 | 可重现性 | 升级工效学 | 失效形态 |
|---|---|---|---|---|
| 区间加锁文件 | Cargo、npm、pnpm、deno.lock | 高（锁文件提交） | `update` 改锁文件 | 锁与清单漂移，求解器意外 |
| 清单内嵌哈希 | Zig、Inko、Go（伪版本） | 构造上即高 | 手动改钉住的版本 | 升级时差异噪声大 |
| MVS，无锁文件 | Go modules、Inko | 高（下界被钉住） | 逐依赖显式上调 | 升级不足（下界陈旧） |
| 无版本（哈希） | Unison、Nix（FOD 内容） | 绝对 | 按哈希抓取 | 没有「2.1 版」这类人类习惯的版本标签 |

### 2.7 用现有机制评估作者的哈希存储 UPM

作者的提案（package_manager.md）：前端用任意语言的构建脚本，按 PKGBUILD 式阶段执行。bin 优先，`-bin`/`-git`/带版本的变体写在同一个清单里。清单格式不限，内置模板变量（`#OUT#`、`#TMP#`、`#ARCH#`），更新与测试逻辑写在脚本里，并对**逐包依赖加锁（哈希加版本）**。后端是 **带哈希前缀的单一存储**、**多版本共存**、为兼容第三方而做的 **FHS 标准链接**、版本后缀的冲突规则。分发用 **双哈希算法的 P2P torrent** 加中心哈希权威。逐项对照先例：

| 作者的要素 | 已由谁解决 | 机制 | 剩余缺口 |
|---|---|---|---|
| 逐包依赖锁（哈希加版本） | Inko 清单的 `require URL VER SHA`、Zig 的 `url+hash`、Cargo.lock | 清单内嵌钉版本（Inko/Zig）或生成锁文件（Cargo） | 无。改用 blake3 替代 SHA1，自动升级工具 `ds update` 是工效学成本 |
| 输出哈希寻址的存储 | pnpm CAS（文件）、Zig 全局缓存（tarball）、Nix FOD（构建输出）、Bazel CAS（动作输出） | 内容哈希即存储键，抓取时校验 | Nix 的教训：**输入哈希不等于输出哈希**。DreamScript 应当用输出内容哈希作存储键（作者第 6 点），只在不可重现的构建上退回输入哈希 |
| 多版本共存 | Nix 存储路径、Zig 包缓存、Cargo（主版本）、pnpm | 用哈希或名字加版本消歧 | 无 |
| FHS 链接 | Nix 用户 profile（`/etc/profiles/per-user/.../bin` 下的符号链接）、buildEnv、scoop shim（Windows）、pnpm 全局虚拟存储 | 从存储向标准路径铺符号链接农场，冲突时加后缀 | 作者的设计可行：冲突加后缀，无后缀给最高版本。Windows 上要用 shim 替代符号链接。硬链接与符号链接的取舍：硬链接不能跨卷也不能指向目录，所以符号链接或文件级硬链接（pnpm）是已验证的选择 |
| bin 优先 | Nix 二进制缓存（作者有充分理由否掉）、AUR 的 `-bin` 包、cargo-binstall 式在清单里声明 bin URL | 每个发布目标声明预构建产物的 URL 与哈希，回退到源码构建 | 信任模型：预构建产物必须按平台签名或校验，作者「bin 与源码在同一清单」的做法把信任范围收在一个文件里，Nix 的缓存分离则要两处对齐 |
| 任意语言的分阶段构建脚本 | PKGBUILD（bash）、Homebrew formula（ruby）、Nix builder（任意语言）、Bazel genrule | 阶段脚本在沙箱里运行，工具依赖被钉住 | 封闭性缺口：逐包钉住工具的 *版本*，作者的依赖锁也覆盖脚本依赖，这样构建才诚实。Nix 的沙箱（bwrap）是作者已经认可的先例 |
| P2P torrent 分发 | IPFS/CAS 网络、nix-casync、Nix-P2P 研究 | 分块 CAS 加 magnet 加签名的清单根哈希 | 对长尾包在经济上脆弱，作者自己的镜像分析说明了中心镜像为何吃紧。作为 *可选* 加速器可行，因为 CAS 分块在构造上就适合 torrent。双哈希（blake3 加 sha256）的 multihash 式方案是合理的 |

结论：提案可实现，且大部分已在各处 *预先实现*。真正新的主张有两点。一是跨生态的单一通用管理器，有范围蔓延风险，Nix 说明通用构建描述才是难点，作者用逐语言的阶段脚本来缓解，这等于承认封闭工具链要按语言分别解决。二是端到端的输出哈希键，理由充分，Nix 的 ca-derivations 工作既确认了可行性，也确认它在别处仍处早期。对 **DreamScript 本身**，语言只需要能自己拥有的三个原语：带哈希的清单（Zig/Inko 风格）、MVS 解析（Inko/Go）、全局存储加硬链接物化器（pnpm 风格）。跨语言的 UPM 可以以后建在 DreamScript 的存储 *之上*，先拿 DreamScript 的包当第一批用户。

一份满足作者要求的具体 DreamScript 清单（组合 B 的草图）：

```
# ds.pkg (TOML/KDL; one file, no lockfile, manifest is the lock)
name    = "pipeline"
version = "0.4.0"

[deps]                          # MVS minima; each pin carries output hash
std     = { ver = "1.2", hash = "blake3:9f2c..." }
csv     = { ver = "0.7", hash = "blake3:1a88..." }   # csv 0.7 may coexist with another pkg's csv 0.6

[artifacts]                     # bin-first: prebuilt per target, verified against store
x86_64-pc-windows-msvc = { url = ".../pipeline-0.4.0-x86_64-pc-windows-msvc.dsmod", hash = "blake3:77e0..." }
aarch64-linux-gnu      = { url = ".../pipeline-0.4.0-aarch64-linux-gnu.dsmod", hash = "blake3:c0d1..." }
# no [artifacts] entry for a target -> build from source in sandbox

[build]                         # optional, only if needed
script = "build.ds"             # run by the embedded VM; sees #OUT#, #TMP#, #ARCH# like the author's vars
```

这个顺序避免了 Nix 的失效形态（通用系统但逐语言工效学很差），也符合作者的解耦伦理：存储、解析器、链接器、抓取器是彼此正交的部件。

## 3. 构建系统

### 3.1 综述：谁在构建什么，为什么赢或为什么输

| 系统 | 模型 | 胜在哪 | 败在哪 |
|---|---|---|---|
| Cargo | 声明式清单加 build.rs 逃生口，一个二进制包办 build/test/doc/install | 90% 场景零配置，锁文件，目标三元组，`cargo add/check/fmt/clippy/doc` 出自同一个工具的工效学 | feature 统一的混乱，build.rs 的沙箱说法，C 依赖交叉编译仍需手动 |
| Zig build | `build.zig` **用语言本身编写**，被编译成 build runner 可执行文件来构造步骤 DAG（官方文档，issue #20981 拆分 configure 与 execute 两个阶段） | 交叉编译轻松（`zig cc` 为每个目标自带 libc），会 Zig 就不用学 DSL，构建逻辑里可用 comptime | 社区共识（ziggit/reddit 2025-26）：构建 API 难以上手（「really difficult to grasp」那条帖子），每次 `zig build` 都要重编 runner，启动有成本，文档滞后 |
| MoonBit moon | 清单加逐包 `moon.pkg`，`moon build/test/fmt/info/check/publish` 出自单一工具链二进制 | 工作流统一，生成 `.mbti` 接口，快照测试，一棵源码树产出 wasm/js/native 后端 | 生态较新，构建不透明（清单驱动，扩展性小） |
| Meson | 声明式 DSL → 生成 Ninja | 语法接近声明式清单，默认值按构造即正确，原生与交叉的目标区分明确 | 又多一门小语言，与宿主语言不同源，基本限于 C/C++ |
| Bazel | 封闭的动作图，远程缓存与执行（动作键 = 输入、命令与环境的哈希，输出内容寻址） | 能撑住百万文件的 monorepo，构建可重现，远程缓存 | 笨重：要 Java 运行时，到处是 BUILD 文件，宏分层，错误费解，文档差（keasigmadelta 的正面对比），对任何单语言项目都过重 |
| CMake | 元构建：DSL → 生成 IDE/Make/Ninja 文件 | C/C++ 的生态引力 | DSL 名声极差（twdev.blog 的「Why CMake sucks?」），目标与变量的二重性，`find_package` 的民间传说，HN 上的评价是「我们手上最不烂的」 |

```zig
// build.zig (shape): the build graph is ordinary Zig code
pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const exe = b.addExecutable(.{ .name = "tool", .root_source_file = b.path("src/main.zig"), .target = target });
    const tests = b.addTest(.{ .root_source_file = b.path("src/main.zig") });
    const step = b.step("test", "run tests"); step.dependOn(&b.addRunArtifact(tests).step);
    b.installArtifact(exe);
}
```

决定性的模式是：**成功的单语言构建系统都是一个二进制加声明式默认**，Cargo、moon、roc build、deno task、bun build 都如此，**再在需要时给一个同语言逃生口**，如 build.rs。Zig 的纯代码路线是这个模式的纯粹形态，代价是理解 API 的成本，它自己的社区也这么说。DreamScript 是解释型的：所谓「构建」大部分就是类型检查加字节码产出，`ds` 二进制本来就要做。构建层只需要依赖抓取、IR 缓存失效、FFI 包的 C-ABI 胶水以及目标选择。

### 3.2 同语言、声明式，还是两者都要

同语言构建（Zig）提供图灵完备的编排，不引入额外语言，代价是一套要像框架一样学的专属 API 表面，这是反复出现的抱怨，并且把构建的正确性耦合到宿主语言的稳定性上。声明式清单（Cargo/Meson）让 90% 可声明、可差异、可加锁，剩下的部分最终需要逃生口，build.rs 就是构建期执行的任意 Rust，Cargo 自己的安全敏感角落。对 DreamScript：**默认清单，10% 的场景用同语言 `build.ds`**。差别在于解释型语言可以直接在编译器已内嵌的同一个 VM 里跑构建脚本，Zig 必须先编译出 runner 可执行文件，DreamScript 只是求值，成本接近零。章程的解耦原则也得到满足，因为构建脚本是一个带小型 build API 的普通程序，与魔法 DSL 无关。

决策表：

| 选项 | 例子 | 优点 | 缺点 |
|---|---|---|---|
| 仅声明式 | Cargo、Meson、moon | 可差异、可加锁、工具可分析 | 最终需要逃生口 |
| 仅代码 | Zig build.zig、Buck2 starlark | 没有 DSL，表达力完整 | 框架级别的 API 学习曲线，缓存分析更难 |
| 声明式加代码逃生口 | Cargo 加 build.rs、提案中的 DreamScript | 90% 零配置，10% 也能做 | 两套心智模型，代码路径需要沙箱方案 |

具体草图：`ds.pkg`（清单，TOML/KDL）声明名字、依赖与目标。可选的 `build.ds` 暴露 `pub fn build(ctx: Build)` 来添加步骤，例如代码生成、通过 `cc`/`zig cc` 编译 C、嵌入资源。脚本模式下 `ds run x.ds` 完全忽略这两者。

### 3.3 交叉编译的做法

Go：`GOOS=linux GOARCH=arm64 go build` 是黄金标准，成立的原因是标准库是纯 Go，默认不带 cgo。Zig：交叉编译就是它的招牌特性（目标三元组加自带的 libc 与头文件）。Rust：`--target <triple>` 再加一套外部链接器方案（`cargo-zigbuild` 把 zig 包成链接器，这也正是 Inko 文档里写的：「Zig greatly simplifies cross compiling... Inko favours using Zig over gcc/clang」）。Bun：`bun build --compile --target=bun-linux-x64` 可以交叉编译出自带运行时的可执行文件，连 Windows 图标与签名都覆盖（文档）。MoonBit/Roc：目标是后端（wasm/js/native），操作系统不参与目标选择，交叉「编译」免费，因为产物是 wasm/js，原生构建借用 LLVM/cargo 的基础设施。

| 系统 | 命令 | 需要预装什么 |
|---|---|---|
| Go | `GOOS=windows GOARCH=arm64 go build` | 无（纯 Go），用 cgo 则需 mingw |
| Zig | `zig build -Dtarget=aarch64-linux-gnu` | 无（自带 libc） |
| Rust | `cargo build --target aarch64-unknown-linux-gnu` | 目标的 std 加交叉链接器（或经 cargo-zigbuild 用 zig） |
| Inko | `inko build --target aarch64-...`（存在 zig 时用它） | 建议装 zig（文档里这么写） |
| Bun | `bun build --compile --target=bun-linux-x64` | 无 |
| DreamScript（提案） | `ds build --target aarch64-linux-gnu` | 字节码场景无需预装，原生嵌入可选 zig cc |

对 DreamScript 的推荐：字节码优先的目标免费，IR 缓存按三元组区分，用来承载原生 JIT 的假设。需要原生或嵌入分发时，采用 Inko/cargo-zigbuild 的姿态，有 zig cc 就用，否则用 cc，并把 wasm 当作通用的低投入目标。

### 3.4 构建层不能变成什么

上文已经给出了反面目标：不要第二门 DSL（CMake），不要宏分层的 BUILD 图（Bazel），不要没有钉住方案的构建期代码执行（build.rs 与 Nix 的教训），也不要为分叉出一份新的类型检查表面的 feature 开关。feature 开关的位置由 MoonBit 式文件名后缀与 Inko 式显式 `import x if os == "windows"` 接管。章程的 LSP/RISC 模型在这里适用：抓取、解析、缓存、物化、编译、链接是由 `ds` 二进制组合起来的独立步骤，各自可单独缓存、可单独观察（`ds fetch --print-hash`、`ds build --explain-cache`）。

## 4. 工具链

### 4.1 LSP 优先：编译器即查询库

rust-analyzer 的架构（rust-analyzer 博客 2023 年的「Durable Incrementality」，salsa 文档）把遍流水线换成 **数据库上的记忆化查询**（解析、条目树、名字解析、逐函数体的类型推断、诊断），并用红绿依赖失效：一次编辑递增一个修订号，查询只在输入变化时重算。rustc 自己也用同样的查询系统思路做增量编译（rustc-dev-guide）。tsserver 走了另一条有启发性的路：每个项目一个全局程序，用一个文档注册表在打开的文件之间复用 AST，语言特性按需计算。历史上它不如 salsa 讲究，但足够快，而且是「LSP 作为产品」的参照（自动导入、快捷修复、跨文件重命名）。

```
// salsa-shaped query set for DreamScript (sketch)
parse(file_id)            -> Tree            // memoized on file content hash
block_index(file_id)      -> Vec<Block>      // MoonBit ///| blocks: finer invalidation units
resolve(pkg, name)        -> DefId
type_of(def_id)           -> Type            // red-green: depends on resolve()s
effects_of(def_id)        -> EffectRow
linearity_check(def_id)   -> Vec<Diag>
lower(def_id)             -> Bytecode        // depends on type_of + effects_of
diagnostics(pkg)          -> Vec<Diag>       // union of per-def diagnostics
```

对 DreamScript，这条要求直接从章程落出来（解释执行、启动快、作者以 IDE 为中心）：**编译器必须是库**（`ds-check` 模块），API 从第一天起就是查询形状：parse(files) -> tree（内容寻址）、resolve(pkg, name)、typecheck(fn)、diagnostics(pkg)、lowering(fn) -> bytecode。因为热重载（4.5）、REPL（4.3）与 LSP 都是同一个消费者，只是驱动方式不同。非目标之一：每敲一个键就重新分析整个程序。模块与包的设计（1.10-1.11）必须让逐包失效成为单位（这也是禁止包循环的另一个理由：循环会毁掉逐包的增量检查）。Koka 提供了小语言的存在性证明：同一棵编译器代码树同时承载编译器、解释器与 LSP。`koka/src/Main/` 下有 `langserver/`（编译器加 VS Code LSP）、`plain/`（在 Haskell LSP 库编译不了的平台上提供编译器加解释器）与 `playground/`，一套代码，三个前端。

### 4.2 格式化器与接口生成器作为编译器组件

证据：gofmt 随 `go` 一起发布，不可配置，这个决定终结了风格争论。同类工具还有 zig fmt（在编译器二进制里）、roc format（`roc fmt`）、moon fmt（`moon fmt`，因为 `///|` 块而具备块感知）、rustfmt（独立，位置紧邻 rustc），而 rustc 自己开发用的格式化是内部的。值得额外抄 MoonBit 的一点是 `moon info` 生成 `.mbti` 接口文件并入库，于是 **API 变更会当下落在差异里**，审阅者直接看 `.mbti` 的变化（moonbit-core 的 AGENTS.md：「check the diffs of .mbti file to see if the changes are expected」）。对 DreamScript：`ds fmt`（一种规范格式，不带选项）与 `ds info`（按包产出 `pkg.iface` 文本）应当是编译器组件，共用 AST 与块模型。接口文件同时充当增量编译的边界和 LSP 的悬停与签名来源。

```
// hypothetical ds info output for pkg "pipeline/transform"
package "acme/pipeline/transform"
// Functions
pub fn parse_row(String) -> Result[Row, ParseError]        // effects: <>
pub fn normalize(Row) -> Row                               // effects: <>
pub fn aggregate(Iterator[Row]) -> Stats                   // effects: <io>
```

格式化器配置对比（「不带选项」为什么赢）：

| 格式化器 | 选项数 | 避免的失效 |
|---|---|---|
| gofmt、zig fmt | 零 | 风格争论、团队内配置漂移 |
| prettier | 数十 | 各仓库的配置文件分叉 |
| rustfmt | 约 100 | nightly 标志频繁变动，团队得自带 rustfmt.toml |
| ocamlformat | 约 100 | 需要按 profile 预置 |
| moon fmt | 零（块感知） | 同 gofmt |

### 4.3 REPL 与类型、效应的呈现

Koka：REPL 对每个表达式打印推断出的类型 **连同它的效应行**（koka 的 docs/tour：「try out this example directly in the interactive environment」）。REPL 前端在 `koka/src/Interpreter/`，与编译器同一棵树，是编译器即库的证据）。Elixir 的 iex：每次求值 `defmodule` 就重编一个模块，同时保留进程状态，这种操作模型让热替换成为常规形态。关于 Erlang 代码加载的机制见 4.5。OCaml 的 utop：带类型的 toplevel，在当前环境上做补全。GHCi：`:r` 重编模块 DAG，状态丢失（公认的疣）。Julia：REPL 加 Revise（4.5）重新定义了 REPL 驱动的开发：改一个文件，方法就地替换而不丢会话数据，因为 Julia 在方法实例级别追踪方法失效。

```
$ koka
> println("hi")
hi
() : <console,io> ()                 # type AND effect row displayed
> map(x -> x + 1) : list<int>  ...
```

对线性类型的语言，REPL 要解决一个真正的新问题：**提示符上的线性值不能被悄悄丢掉**。REPL 要么自动消费（先展示再释放，相当于对提示符变量做隐式 `consume`），要么维护一个「会话拥有」的 arena，由用户显式丢弃。Koka 的带效应 REPL 与 Rust 的 evcxr 在所有权上的折腾是先例。推荐：REPL 就是同一套字节码 IR 上的解释模式，配一个会话堆。效应不处理，或由打印来处理。线性值由会话的线性上下文拥有，用显式 `:drop` 释放。

```
$ ds repl
> let h = File.open("data.csv")     # h : File  (linear)
h : File -- owned by session context
> let rows = Csv.read(h)            # h consumed here; typechecker accepts because REPL re-checks
rows : Iterator[Row]
> :drop rows                        # explicit; or rows auto-dropped at :reset
```

### 4.4 错误消息

源流：Elm 普及了带提示的友好多行解释。rustc 把它工业化，采用 **结构化诊断模型**，用 derive 声明 `Diagnostic`/`Subdiagnostic` 结构体，结构化建议渲染成可直接粘贴的差异，为机器提供 JSON 输出（见 rustc-dev-guide 的「Errors and lints」）。可复用的库是 **codespan-reporting**（「beautiful error diagnostics」，带标签的跨度、注解、严重级别）与 **ariadne**（跨多行、多文件的带标签跨度），两者都是 rustc 风格的直接仿制。Roc 与 Elm 的编译器消息是「名消息」的文化参照。对 DreamScript，实际的硬要求是：跨度必须穿过 IR 边界，诊断在带类型 AST 层计算，对源码渲染。建议要带结构化区间，标明插入点与替换范围，机器直接按区间应用。为 LSP 与 CI 提供 `--error-format=json`。另外，因为语言是线性的，要有一类专门的线性性违规诊断，输出「在此处被移动 / 在此处再次使用」的两个跨度（rustc 的借用检查器诊断是模板：E0382 式的移动与使用配对正是线性类型错误的样子，只是没有借用带来的复杂度）。

```
error[DS2001]: value `h` was consumed here
  --> src/pipeline.ds:41:15
   |
41 |     let rows = Csv.read(h)
   |                        ^ `h` moved (File is linear)
42 |     Log.write(h.path)      <-- error[DS2001]: used again here after move
   |
help: `File` must be consumed exactly once; clone the path before the move
   |
41 |     let path = h.path
42 |     let rows = Csv.read(h)
   |       ++++++++++++++++++++++++++++++++++ (suggestion machine-readable as JSON)
```

### 4.5 热重载：运行期模块格式必须提供什么

四类先例：

| 路线 | 系统 | 机制 | 带来的约束 |
|---|---|---|---|
| 原生 re-dlopen 加 ABI 元数据 | Mun | assembly 就是 C-ABI 动态库，运行期对比新旧类型元数据并递归迁移结构体状态（docs.mun-lang.org ch04-04，另见 mun_runtime 文档） | 稳定的 ABI、完整的类型反射、状态迁移代码，代价高昂。Mun 之所以存在，就是因为编译型语言需要这一整套 |
| 双版本代码槽位 | Erlang/OTP | 最多同时存在当前版与旧版。进程在下一个 **全限定** 调用（`mod:func()`）处切换，本地调用仍留在旧代码上。第三次加载强制 purge，会杀掉滞留者，`soft_purge` 与 `purge` 的区分见 erlang.org 的代码加载文档 | 生成代码要遵守外部调用纪律（编译器里很容易做到）、旧版本寿命有界、状态由进程承载，全局变量持有不到它 |
| 增量方法重定义 | Julia Revise | 监听文件，降级变更的代码，在不重启的情况下失效或替换 **方法实例**，保留会话数据 | 运行中的任务需要 world-age 语义，运行期要有方法级粒度 |
| VM 级模块重载 | Bun `--hot` 与打包器 HMR | 重新求值变更的模块，替换导出，通过模块注册表间接层保住模块状态 | 受 ESM live binding 图约束，状态丢失常见，脚本场景够用 |

对 **解释型字节码语言**（DreamScript 的情形）来说，Erlang 模型几乎免费，成本也低于 Mun：运行期按模块持有 **带版本的代码槽位**。`ds reload` 把变更的文件编译成新 IR，切换槽位，把旧版本的栈帧标记为在下一个调用边界退役，在 DreamScript 里全限定调用等于所有跨模块调用，这条纪律由调用约定直接成立。线性与 GC 管理的状态住在堆里，代码里只有指令，于是「状态迁移」退化为对保留下来的堆值做类型兼容检查，即 Mun 的结构体差异逻辑作用在 GC 堆上，省掉 ABI 这层后简单得多。Julia 式的方法失效是更细的变体，可以以后长进去（有了 JIT 层它才有意义）。

```
Hot reload protocol (DreamScript sketch):
  watcher: file changed -> reparse/recheck changed blocks (salsa queries, 4.1)
  compiler: lower changed defs -> new dsmod v(n+1)
  runtime:  install into slot[1] (slot[0] = old); frames referencing slot[0] age out
            at next cross-module call; heap values type-checked against new defs
            (compatible: keep; changed layout: migrate via Mun-style diff or quarantine)
  effects:  open effect handlers capture a code version; on reload, resumption uses
            the version the handler was installed with (Erlang's two-version rule
            generalized to handlers)
```

这对模块格式提出的要求是：模块能用稳定名字寻址。每个模块的字节码带依赖列表，用于重编闭包。带版本槽位遵守最多两份存活。**加载期没有顶层副作用**，初始化必须是显式的 `main`/`init`，同一条规则让 Python 的循环问题不可能出现（见 1.7）。Zig 是编译侧的警示故事：通过 `zig build-lib -dynamic` 做热代码加载会不断撞上稳定 ABI 与结构体布局的墙，社区讨论与 thenumb.at 的「Hot Reloading in Exile」盘点了各种技术及其代价。解释型运行期直接绕过这一切，这是 DreamScript 应当主动宣传的真实架构优势。

## 5. FFI 与嵌入

### 5.1 C ABI 层

通用基底：C-ABI 函数就是遵循公开调用约定的调用，跨边界的数据必须有 **确定的布局**。Rust 的 `#[repr(C)]` 给出与 C 兼容的结构体布局（字段按声明顺序、按对齐补白，正是 C 编译器输出的那样。Rust 的 `extern "C" fn` 与 `#[no_mangle] pub extern "C" fn` 分别是导入与导出形式。Zig 的 `export fn` 产出 C-ABI 符号，`@cImport`/`translate-c` 把头文件自动转成声明，`extern "c" fn` 用来导入。DreamScript 具体需要三样东西。(a) `repr(c)` 结构体属性，并写出 C 布局规则（自然对齐、补白、顺序）。(b) 带调用约定标记的 `extern` 函数类型。(c) 所有权边界类型：进出的指针都是不透明的。线性类型把「必须恰好释放一次」从约定变成类型系统的一部分：`Foreign<T>` 值是线性的，`free(f)` 消费它，重复释放是类型错误。FFI 层正是线性性证明自己价值的场合。

C 结构体布局示例（`repr(c)` 必须精确复现的东西）：

```
struct sample {          // 64-bit C ABI:
  char  a;              // offset 0
  double d;             // offset 8  (padded: alignof(double)=8)
  char  b;              // offset 16
  int   i;              // offset 20
};                      // sizeof = 24 (tail-padded to alignof 8)
```

DreamScript 的表面示例：

```
type c["sqlite3"]                 // opaque handle, layout unknown, linear
extern fn sqlite3_open(path: *const c.Char, out: **c["sqlite3"]) -> c.Int
extern fn sqlite3_close(db: *mut c["sqlite3"]) -> c.Int   // consumes: db is linear
// typechecker enforces: every open path reaches exactly one close or leak-diagnostic
```

头文件摄取：MoonBit/Inko/Zig 都靠声明或翻译来绑定 C，绑定发生在编译期，运行期不做解析。Zig 的 `translate-c` 与 bindgen 式的从头文件生成是应当抄的工效学：一个 `ds bind header.h` 命令产出带类型的 extern，运行期 cimport 的做法就此消失。

### 5.2 与 GC 运行时双向调用

两个方向的风险不同。**语言调用 C**（DreamScript 作为消费者）：GC 不能搬移或释放 C 侧仍持有的内存，标准解法是对 FFI 可见的对象做 pinning 分配，或者采用句柄表模式。**C 调用语言**（回调）：被调方跑在外部栈上，搬移式或有栈的 GC 需要知道这件事。要么回调只在 VM 管理的栈上运行（Lua 的模型），要么运行期支持任意栈进入（代价高）。Lua 的 C API 是典范性的最小设计：所有数据交换都走一个显式的 **值栈**，即压参数、`lua_call`、按索引读结果。跨调用保留的 C 值放进 **注册表**，靠伪索引 `LUA_REGISTRYINDEX` 与 `luaL_ref` 管理，后者是指向 GC 会扫描的表的整数句柄。C 闭包用 `lua_upvalueindex`。指向 Lua 值的裸指针从不外流（lua.org PIL 27.3.1 与 Lua 5.3/5.4 手册里合法与可接受索引的规则）。mlua 展示了在此之上能做到的工效学包装层。

```c
/* Lua C API canonical sequence */
lua_State *L = luaL_newstate();                    /* VM + stack created */
lua_getglobal(L, "process");                       /* push fn */
lua_pushinteger(L, 42);                            /* push arg */
lua_call(L, 1, 1);                                 /* pop fn+arg, push result */
int r = (int)lua_tointeger(L, -1); lua_pop(L, 1);  /* read top, pop */
lua_pushlightuserdata(L, my_obj);                  /* opaque ptr, GC-invisible */
int ref = luaL_ref(L, LUA_REGISTRYINDEX);          /* GC-visible handle (int) */
```

Python 的 C API 是警示故事：每个函数返回 **持有引用或借用引用**（docs.python.org c-api/intro 的「ownership of references」），部分参数会被 *窃取*，引用计数纪律全靠人维护（学术研究发现引用计数 bug 在 CPython 扩展里普遍存在，见 PSU refcount.pdf 与 ASE-2023。GIL 让嵌入串行化。子解释器隔离把 C 扩展坑了几十年，逐解释器 GIL 直到 3.12+ 才稳定，pybind11 至今仍记录着脆弱之处。

嵌入 API 设计对比：

| 属性 | Lua C API | Python C API | DreamScript 提案 |
|---|---|---|---|
| 取值方式 | 显式栈索引 | 裸 `PyObject*` 指针 | 句柄：`Value = VM 对象表里的索引` |
| 所有权约定 | 栈保持平衡，另有注册表引用 | 持有、借用、窃取三元组，手动引用计数 | 线性：借用 = `&Value`（限于调用范围），拥有 = `Value`（转移） |
| GC 安全 | GC 扫描注册表 | 引用计数，循环需要 `gc` 模块 | GC 扫描句柄表，裸指针不会外流 |
| 线程 | 每线程一个 `lua_State`，或用 `xmove` | GIL，子解释器脆弱 | VM 就是一个 `&mut Vm` 上下文，多个 VM 各自持有独立上下文并行运行 |
| 错误模型 | 基于 `longjmp` 的 panic | 异常加 `NULL` 加最后错误 | 通过边界返回 `Result`，栈不向 C 展开 |
| 回调入口 | 仅 VM 管理的栈 | 持有 GIL 的任意线程 | 回调调度到 VM 栈上（或专用入口蹦床） |

从两者提炼出的 DreamScript 嵌入 API 设计规则如下。**用句柄承载值**：`Value` 是宿主 VM 对象表里的索引，即 Lua 注册表思路，GC 安全由构造保证。**取消窃取引用**：每个 API 要么借用且生命周期限于本次调用，要么返回新的持有句柄。线性类型可以编码这一点，借用型 API 收 `&Value`，消费型 API 收 `Value`。**显式上下文**：用 `ctx: &mut Vm` 穿参，全局状态不参与，于是「GIL 造成的问题」在设计上不再出现，两个 VM 就是两个上下文。**pin 住外部对象**，让 GC 永不搬移 FFI 可见的内存。

### 5.3 wasm 边界与 JS 互操作

Wasm 核心只提供小类型集（i32/i64/f32/f64 加引用）的导入与导出。**WASI** 预览提供系统调用。**Component Model** 配合 **WIT**（WebAssembly Interface Types）加上带类型、语言无关的接口：`world` 声明导入与导出，wasmtime 的 `bindgen!` 与 wit-bindgen 为各种语言生成宿主侧与访客侧绑定（component-model.bytecodealliance.org 的 WIT 参考与 WIT.md 规范）。MoonBit 是「小语言可以把 wasm/js/native 当作平等后端，从一棵源码树产出」的最好证据，moonbit-core 里用 `*_js.mbt`、`*_nonjs.mbt`、`*_wasm.mbt` 后缀文件区分，`moon build --target wasm|wasm-gc|js|native`。

```wit
// WIT world sketch: the typed contract a DreamScript wasm component would export
package acme:pipeline@0.4.0;
world pipeline {
  import host-io: interface {                 // platform effects arrive as imports
    log: func(msg: string);
    read-file: func(path: string) -> result<list<u8>, string>;
  }
  export run: func(input: string) -> result<string, string>;
}
```

对 DreamScript：把字节码 IR 编译到 wasm 用于浏览器与嵌入式分发，编译到 wasm *组件*（WIT 导出）用于宿主嵌入。于是组件边界对 wasm 宿主来说 *就是* 嵌入 API，同一套效应行类型可以直接渲染进 WIT（例如 `console.print : (String) -> ()` 效应浮现为宿主要满足的 WIT 导入，这就是「效应编译成能力导入」的具体机制）。wasm 目标上的 JS 互操作：沿用 MoonBit 的后缀文件模式，不要预处理器。`foo.js.ds` 提供只在 js 后端编译的 JS 胶水 extern。

### 5.4 Roc 的平台模型：「宿主提供，语言消费」

Roc 把通常的标准库安排反过来：应用是纯的，**平台**单独编写，提供整个带效应的表面（I/O、分配、宿主函数），并提供应用链接进去的宿主可执行文件。编译器生成 **胶水**，一个「外科链接器」把应用代码补进宿主二进制（roc-lang.org 的 FAQ、deepwiki roc-lang/roc，以及 basic-webserver 与 roc-ray 两个参考平台）。应用声明 `app [main] { pf: platform "..." }`，然后直接调用平台提供的函数。效应的归属是平台，语言本身不带效应。这套做法与 DreamScript 的设计组合得比与 Roc 自己更好，因为 DreamScript 能给平台边界定型：平台声明它处理的 **效应行**（`platform "web" provides <fsys, net, console>`），检查器校验用户代码只用到了已提供的效应，效应行即契约，线性分配器也与线性类型设计相配，Roc 明确由平台提供分配器。

需要承认的代价：Roc 的 FAQ 记录了单平台限制，一个应用只针对一个平台，多平台就意味着多个构建目标。平台版本化会让包管理复杂（见 2.6）。胶水与链接器这部分是实打实的编译器工作量。推荐：采纳这个 *模式*，即效应来自一组可注入的能力，最小的默认平台就是 `ds` 二进制，提供一条标准效应行，同时不收 Roc 那套应用与平台连杆的复杂度。DreamScript 的「平台」就是导出效应处理器、可选地附带宿主胶水的普通包，可按构建目标选择。这是把 Roc 的单体做 LSP/RISC 式拆解。

```
// DreamScript platform sketch
platform "std"   provides <console, fsys, net, time>     // the ds CLI itself
platform "web"   provides <console, fetch>               // wasm host satisfies via WIT imports
platform "embed" provides <>                              // host provides nothing; host calls in
// app code: fn main() -> {console.print("hi")}  -- checker: <console> ⊆ provided row
```

### 5.5 Koka 的多后端内联 FFI（证据）

koka/lib/std/core.kk 里有 `extern import c file "core/inline/core"` 与 `cs file "core/inline/core.cs"`。koka/lib/std/async/legacy/readline.kk 声明 `extern import { cs file "readline-inline.cs"; js file "readline-inline.js" }`。也就是说 Koka 允许库作者在带类型的 extern 声明旁边，按后端附带 **内联片段**（C、C#、JS），异步标准库还按后端提供事件循环实现（`api/web/evloop.kk`、`api/uv/file.kk`）。对解释型的多目标语言这是一个可直接照搬的模式：带类型的 extern 签名住在 DreamScript 里，每个目标配一个实现文件，原生用 C ABI，wasm 用导入，js 用 JS 胶水，由 MoonBit 那套文件名后缀机制选择。它还展示了边界上的效应：Koka 标准库给 FFI 函数写出精确的效应类型（`<asyncx,console>`），这正是 DreamScript 想要的形状。

```koka
// koka (verbatim shape): typed declaration + per-backend inline impl
extern import { cs file "readline-inline.cs"; js file "readline-inline.js" }
pub fun readline() : <asyncx,console> string { ... }
```

### 5.6 Cangjie 与其他互操作主张

Cangjie（华为，2025 年开源）带 C FFI（既能调用也能被调用）、ArkTS 互操作，声称可与 Python 库互操作，并支持包括 JVM 字节码在内的多后端编译（cangjie-lang.cn 文档，另见 Medium 上的概览）。它的价值是市场验证。一是「C ABI 加一个动态运行时互操作（Python）加一个带类型 VM 互操作」是中文生态新语言的预期表面。二是「无缝互操作」这类说法属于营销，机制永远是句柄表加编组层。除确认 5.1-5.3 之外，没有可引入的设计新意。

## 6. 分发与启动

### 6.1 单文件分发

Zig：`zig build-exe` 产出自带一切的可执行文件（经自带的 musl/glibc 桩做静态链接 libc），这是交叉编译的基线。Go：单个静态二进制，资源用 `embed.FS` 嵌入。**Bun `--compile`** 是解释型语言的参照：把 JS/TS 连同运行时打包进一个可执行文件，`--bytecode` 在构建期预编译成字节码，让运行期启动跳过解析。实测相对纯打包启动快约 1.38x，相对 `bun run` 快约 2.01x（peterbe.com 的基准，另见 bun 的 docs/compiler）。代价是体积（每个应用都要附带整个运行时，见 bun#19498）。对 DreamScript：`ds build --exe` 应当内嵌整个依赖闭包的字节码加 VM。字节码本来就是内部 IR，这条路径复用已有产物。Bun 则必须额外加一条字节码流水线。Windows 上像 Bun 那样通过 RC 嵌入图标与清单。各平台的代码签名要点参见 Bun 文档，其中覆盖了 macOS 签名。

### 6.2 字节码缓存与格式

CPython：`__pycache__/*.pyc` 带一个头部，含 magic number、位域、源码的 **mtime 加大小**（PEP 552 之后，可在 checked/unchecked 模式下改用源码 **哈希**）。头部陈旧就重编。发行版在安装期预编译，因为只读文件系统自己写不了（Fedora 的变更页）。LuaJIT 的 `luajit -b` 产出字节码文件，格式写在 `luajit/src/lj_bcdump.h` 里：头部 `0x1b 'L' 'J'`、版本字节（2）、ULEB128 标志（`STRIP`、`FFI`、`FR2`、`BITOP` 等），随后是一串带长度前缀的原型（`phead = flagsB numparamsB framesizeB numuvB numkgcU numknU numbcU ...`），子模块以 `BCDUMP_KGC_CHILD` 引用。也就是说 **模块靠内嵌的子原型链接，而格式小到能在一个注释块里写全**。Java：`.class`/jar 加 **AppCDS**（JEP 310/350、JEP 483 的 AOT 类加载）：预解析的类元数据导出成归档，启动时 **mmap**，减少类加载工作，Red Hat 报告收益随类数量增长。

```
dsmod (proposed DreamScript module file):
  bytes  0..3   magic "\0DSM"                       # LuaJIT-style magic discipline
  byte   4      format version                      # bump on IR change (private forks: >=0x80)
  u8     5      target class (bytecode | wasm | native-triple-id)
  u8     6      flags (STRIP, FFI-USED, HAS-JIT-METADATA)
  uleb   ...    module name length + name
  uleb   ...    dep count; deps as (name, content-hash) pairs   # recompile closure for hot reload
  uleb   ...    source hash (blake3-128)            # PEP 552 checked-hash validation
  sections      blocks (bytecode protos): uleb-prefixed, length-delimited, relocation-light
  sections      string/int constants, type metadata, effect rows, debug spans (strippable)
```

对 DreamScript 的设计：定义一个 `dsmod` 二进制格式，含四部分。(a) magic、格式版本、目标与 IR 版本，遵守 LuaJIT 的头部纪律：IR 一变就升版本号，私有改动同样适用。(b) 逐模块记录，带依赖名列表与源码的内容哈希（PEP 552 的 checked-hash 校验。用哈希而不用 mtime，因为作者的存储是内容寻址的，而 mtime 在 git 检出和存储里会说谎）。(c) ULEB 式变长整数与带长度前缀的段（LuaJIT）。(d) 为发布产物准备 strip 标志。缓存布局与包存储同构：`$DS_CACHE/ir/<triple>/<blake3-of-source+deps>.dsmod`，以 mmap 加载（AppCDS 证明了 mmap 预链接元数据是启动收益的来源，可避免解析与名字解析）。

### 6.3 启动预算

解释型语言的启动 = 进程创建 + 运行期初始化 + 模块图加载 + main。按杠杆大小排序的手段有四条。(1) 命中预编译字节码缓存并用 mmap（不做文本解析，这正是 bun 的 `--bytecode` 拿到 1.38x 的全部原因）。(2) 惰性加载模块，依赖图写在模块头里（只加载 `main` 达得到的东西，Go 风格）。(3) 没有顶层副作用，导入期不执行（这也是热重载与循环的两条要求，三个设计共用一条规则）。(4) 用单文件模式分发（6.1）。mmap 这一块的做法：以只读方式映射 `dsmod` 文件，使用相对偏移与无需重定位的布局（LuaJIT 式原型在设计上就重定位很少，Java 的 CDS 归档是大规模证明），可选地让多个进程共享同一份映射的标准库镜像（dyld shared cache 的模式）。

| 系统 | 启动机制 | 实测或声称 |
|---|---|---|
| bun --compile --bytecode | 内嵌预编译字节码 | 相对纯打包快 1.38x，相对 `bun run` 快 2.01x（peterbe.com） |
| CPython | `.pyc` 按 mtime 或哈希校验（PEP 552） | 跳过词法与编译，导入机制仍是主要开销 |
| LuaJIT | `-b` 字节码，极简加载器 | 许多 Lua 应用直接发布字节码文件 |
| JVM + AppCDS | mmap 预链接的类元数据 | 收益随已加载类数量增长（Oracle/Red Hat） |
| DreamScript（目标） | mmap 的 dsmod 缓存加惰性图加无导入期执行 | 预算：脚本冷启动约 10ms，热启动亚毫秒 |

### 6.4 命名、目标、wasm、Windows

二进制命名：产物采用目标三元组后缀（`name-x86_64-pc-windows-msvc.exe`、`name-aarch64-linux-gnu`），cargo/zig/bun 实际上都以三元组为标准。脚本（`run` 模式）没有东西需要命名。wasm 作为分发目标：一个 DreamScript 包构建出 (a) 供 `ds run` 使用的原生字节码，(b) 供 web 与嵌入使用的 wasm/wasm-gc，(c) 供带类型宿主嵌入的 wasm 组件（WIT，见 5.3）。MoonBit 用后缀文件实现一树三后端，这就是模板。Windows 工效学（作者同时用 Windows 与 NixOS）：PATH 管理用 **shim**，即 scoop 模式：PATH 上放一个 shim 目录，每个工具一个桩，符号链接的位置由桩程序取代。安装器就是一个 `ds.exe`，自更新或上架 winget/scoop。包存储的物化器要避开开发者模式的符号链接，同卷用硬链接，否则复制，这是 pnpm 文档里的 Windows 回退方案。缓存放在 `%LOCALAPPDATA%` 下，用 `DS_CACHE` 覆盖。NixOS：作者反感 flake 的仪式感，所以 DreamScript 工具应当是一个不依赖任何 Nix 打包就能用的静态二进制，同时给想要的人附一个简单的 flake。哈希存储设计（2.7）让 DreamScript 自身的 Nix 包几乎自动成立，对输出哈希友好，不需要 FOD 花招。

## 7. DreamScript 的可行生态组合

### 组合 A「Cargo 式」：清单加锁文件加 src 布局加内置注册表

形态：`ds.toml`（名字、版本、带 semver 的依赖）、生成的 `ds.lock`、`src/` 布局、`ds add/build/test/fmt/info/publish`、稀疏索引注册表（Cargo 的模型），变体用 features 或按目标条件的依赖表达。

实现成本：注册表托管与发布界面是最长的一根杆，mooncakes 在一门年轻语言上花了约两年，所以可行。解析器（PubGrub/MVS）与锁文件都是走熟的路。排除：内容寻址的存储机制（哈希寻址被降级成内部缓存细节）、作者的 UPM 野心（Cargo 克隆在构造上就是生态内的）、哈希即锁的简洁性（要多回答锁文件与清单漂移的问题）。痛点贴合度：熟悉度高（作者熟悉 Cargo），对 Nix 之痛的解决是中等的（注册表又多了一个要镜像的东西），对 Python 之痛解决良好（有正经的清单），对热重载与 FFI 中性。如果作者想要常规行为并且第一天就有注册表，这是兜底组合。

### 组合 B「原生哈希存储」（推荐的核心）

形态：Zig 与 Inko 混合的包层。依赖声明为 `name = { ver, hash }`，内容哈希用 blake3，哈希里含依赖哈希，于是身份级联，Zig 风格。用 MVS 解析（Inko/Go）。**没有单独的锁文件**，清单即锁。全局输出哈希存储（`$DS_STORE/<blake3>/`）。逐项目物化用硬链接或符号链接放进 `build/` 或一个虚拟存储目录，机制来自 pnpm。bin 优先，每个发布版本按三元组声明预构建的 `dsmod` 或原生产物，用存储哈希校验，并保留源码构建的回退。FHS 物化器做成可选的 `ds link` 命令，Windows 上铺 scoop shim，NixOS 上铺 profile 符号链接，用于与非 DreamScript 工具互通。

```
$ ds fetch                      # resolve MVS, download, verify hashes -> global store
$ ds build                      # materialize into .ds/deps/ (hardlinks), typecheck+emit IR cache
$ ds link --profile bin         # optional FHS materialization: shims (win) / symlinks (nix) into PATH
```

成本：存储加物化器，近似 pnpm-lite，加上哈希级联方案（Zig 的设计有文档）与 MVS 解析器，后者很简单。注册表可以推迟到静态 JSON 索引。UPM 外壳（任意语言脚本、torrent 分发）被明确排除在语言的关键路径之外，以后可以在同一个存储上生长。排除：朴素的 semver 区间工效学，声明处写区间仍然可以，但解析取 MVS 的最大值，与 Go/Inko 一致并被接受。自动升级文化同样是排除项，安全修复是一处显式的清单编辑，Inko 记录了这一点并接受它。痛点贴合度：直接命中作者的随笔，逐包锁、输出哈希存储、FHS 链接、bin 优先全都在。flake `follows` 在这里没有类比物，因为依赖哈希自动级联。存储同时充当构建缓存（6.2），让 `ds run` 启动很快。P2P 与跨生态阶段无需重设计即可后加。这个组合也符合章程的解耦伦理：抓取器、解析器、存储、链接器是彼此正交、可单独观察的部件。

### 组合 C「bun 式脚本生态」：零配置、允许循环、热重载

形态：`ds run script.ds` 不需要清单，首次导入时依赖从默认索引解析，`ds save` 时写进生成的清单，这是 bun/deno 的模式。模块设计按 1.10：包内容许文件循环，包之间强制 DAG。ESM 式 live binding，检查放在编译期，运行期没有 TDZ 坑，检查器拒绝跨图的先用后初始化，TS 做不到这一点。标准库可用裸 `@std/...` 说明符导入，不需要 URL。热重载按 4.5 的双槽代码模型，`ds dev --hot` 监听文件。REPL 按 4.3。

成本：双槽的运行期模块格式必须从第一天就在 VM 里，事先规划则便宜，事后再补是要避免的错误。容忍循环的整包名字解析是 Rust 式不动点，实打实的编译器工作，但按包有界。排除：沿循环边的逐包并行类型检查，包本身保持无环，所以跨包并行仍然成立。Python 式导入期副作用同样被排除，这一点是特性，代价可以接受。Unison 式哈希寻址的定义也在排除之列。痛点贴合度：直接命中 Python 交叉引用之痛，以及作者在 TS+bun 里喜欢的脚本工效学。热重载服务大数据迭代回路，纯解释模式让启动保持在 10ms 以下。

### 组合 D「平台模型可嵌入」：宿主提供效应，语言嵌入各处

形态：Roc 模式的能力层。VM 是一个 C 库 `libds.so`，与 Lua 同量级的可嵌入体。平台是导出效应处理器与宿主胶水的包，默认平台就是 `ds` CLI，提供标准效应。带类型的边界：原生宿主走 C ABI 句柄（5.1/5.2），wasm 宿主走 WIT 组件（5.3）。效应行是平台契约（5.4）。按后端的内联实现沿用 Koka 的 `extern import c/cs/js file` 模式（5.5）。

成本：句柄表嵌入 API 加胶水生成器是主要的编译器工作量。效应行到 WIT 的降级要多一张映射表。避开 Roc 那套应用与平台单体（按 5.4）可以让这件事保持增量。排除：由固定且功能丰富的标准库拥有效应，标准库变成多个平台中的一个，这是实打实的文化承诺。像 Haskell 的 IO 那样的零效应表面纯净性保证同样被排除，效应总是相对平台而言的。痛点贴合度：让 DreamScript 可以嵌入 Rust/JS/C 宿主，这是作者关心的脚本目标。在边界上与线性类型组合得好，线性句柄就是 FFI 的恰好一次资源纪律。默认平台的选择让组合 C 的脚本故事保持完整。D 与 A/B/C 正交：它可以叠在任选中的包与模块组合之上，而 B 的存储顺便还能 bin 优先地分发平台包。

### 组合对比

| 维度 | A Cargo 风格 | B 哈希存储 | C bun 风格 | D 平台模型 |
|---|---|---|---|---|
| 实现成本 | 中（注册表重） | 中（存储加物化器） | 中低（运行期槽位） | 中（嵌入 API 加 WIT） |
| 第一天就要注册表 | 是 | 否（推迟，用静态索引） | 否 | 否 |
| 与作者 UPM 的贴合 | 弱 | 强 | 中性 | 中性 |
| 解决 Nix 之痛 | 部分 | 强（哈希级联、bin 优先） | 中性 | 中性 |
| 解决 Python 之痛 | 强 | 强 | 强 | 中性 |
| 热重载 | 中性 | 强（缓存） | 强（内置） | 强（处理器模型） |
| 可嵌入脚本 | 中性 | 强（存储即产物） | 中性 | 强 |
| 排除 | 哈希存储 UPM | 区间求解器的工效学 | 导入期效应 | 固定效应的标准库 |

推荐的合成方案是 **B + C + D**：原生哈希存储的包，bun 风格并容忍循环模块的脚本模式，双槽热重载运行期，加上平台模型的可嵌入 VM。组合 A 的注册表以后再加，做成 B 的哈希寻址产物之上的一层薄静态索引。这个选择逐一解决了点名的痛点。Nix 之痛靠无需锁文件的哈希级联锁。Python 之痛靠交叉引用、真实接口与无导入期执行。`#include` 之痛靠生成并检查接口。LSP 优先靠 LSP、REPL 与热重载共用的「编译器即查询库」。可嵌入脚本靠句柄表 C API 与 wasm 组件。同时每个部件都保持可独立替换，这正是作者声明的 RISC/LSP 设计伦理。

## 来源说明

主要本地证据（位于仓库根目录的只读克隆）：`inko/docs/source/guides/packages.md`（清单格式、MVS、校验和写在清单里）、`inko/docs/source/references/structure.md`（src/test 布局、多二进制约定）、`inko/docs/source/references/syntax.md`（导入语法、条件导入、`import extern`）、`inko/docs/source/guides/cross-compilation.md`（用 Zig 当链接器的姿态）、`moonbit-core/AGENTS.md`（块模型、`moon fmt/info`、`.mbti` 审阅流程、wbtest 与黑盒测试、trait 提升的循环规则）、`moonbit-core/abort/moon.pkg` 与 `moonbit-core/abort/pkg.generated.mbti`（清单与生成的接口）、`moonbit-core/builtin/`（目标后缀文件）、`luajit/src/lj_bcdump.h`（字节码 dump 格式）、`koka/lib/std/core.kk` 与 `koka/lib/std/async/legacy/readline.kk`（多后端内联 FFI）、`koka/src/Main/README.md`（编译器、REPL 与 LSP 的前端结构）、`koka/src/Interpreter/`（REPL 作为编译器组件）。

网络来源：[Zig build system docs](https://ziglang.org/learn/build-system) 与 [dependencies docs](https://www.ziglang.in/learn/build-system/dependencies)、[ziggit build-system discussion](https://ziggit.dev/t/the-zig-build-system-is-under-apreciated/14743)、[zig build API difficulty thread](https://www.reddit.com/r/Zig/comments/1jfiwm9/zig_build_system_is_really_difficult_to_grasp)、[Zig runner split issue #20981](https://github.com/ziglang/zig/issues/20981)、[Unison The Big Idea](https://www.unison-lang.org/docs/the-big-idea)、[LWN on Unison](https://lwn.net/Articles/978955)、[Unison 1.0](https://www.unison-lang.org/unison-1-0)、[Cargo resolver docs](https://doc.rust-lang.org/cargo/reference/resolver.html)、[cargo features unification issue #10489](https://github.com/rust-lang/cargo/issues/10489)、[pnpm motivation](https://pnpm.io/motivation)、[pnpm symlinked structure](https://pnpm.io/symlinked-node-modules-structure)、[pnpm global virtual store](https://pnpm.io/global-virtual-store)、[Deno 2 announcement](https://deno.com/blog/v2.0)、[JSR with Deno](https://jsr.io/docs/with/deno)、[deno#33370 import-map subtlety](https://github.com/denoland/deno/issues/33370)、[mooncakes.io](https://mooncakes.io)、[MoonBit package manager intro](https://www.moonbitlang.com/blog/intro-to-mooncakes)、[moon package tour](https://docs.moonbitlang.com/en/latest/toolchain/moon/package-manage-tour.html)、[Roc FAQ (platform design)](https://www.roc-lang.org/faq)、[rust-analyzer durable incrementality](https://rust-analyzer.github.io/blog/2023/07/24/durable-incrementality.html)、[salsa overview](https://salsa-rs.github.io/salsa/overview.html)、[rustc-dev-guide queries](https://rustc-dev-guide.rust-lang.org/queries/salsa.html)、[rustc-dev-guide diagnostics](https://rustc-dev-guide.rust-lang.org/diagnostics.html)、[codespan-reporting](https://docs.rs/codespan-reporting)、[Mun hot reloading](https://docs.mun-lang.org/ch04-04-hot-reloading)、[mun_runtime](https://docs.rs/mun_runtime)、[Hot Reloading in Exile](https://thenumb.at/Hot-Reloading-in-Exile)、[Erlang code loading](https://www.erlang.org/doc/system/code_loading.html)、[Bun compiler docs](https://bun.com/docs/compiler)、[bun --bytecode benchmark](https://www.peterbe.com/plog/trying-bun-compile-to-bytecode)、[bun#19498 runtime size](https://github.com/oven-sh/bun/issues/19498)、[PEP 3147](https://peps.python.org/pep-3147)、[PEP 552](https://peps.python.org/pep-0552)、[Oracle class data sharing](https://docs.oracle.com/en/java/javase/17/vm/class-data-sharing.html)、[Red Hat AppCDS](https://developers.redhat.com/articles/2024/01/23/speed-java-application-startup-time-appcds)、[WIT reference](https://component-model.bytecodealliance.org/design/wit.html)、[Component Model WIT.md](https://github.com/WebAssembly/component-model/blob/main/design/mvp/WIT.md)、[Python C API intro](https://docs.python.org/3/c-api/intro.html)、[Python subinterpreters](https://docs.python.org/3/c-api/subinterpreters.html)、[refcount-bug study](https://www.cse.psu.edu/~gxt29/papers/refcount.pdf)、[PIL registry](https://www.lua.org/pil/27.3.1.html)、[Lua 5.3 manual C API](https://www.lua.org/manual/5.3/manual.html)、[Go import-cycle strategies (DoltHub)](https://www.dolthub.com/blog/2025-03-14-go-import-cycle-strategies)、[golang#49251](https://github.com/golang/go/issues/49251)、[MDN import statement](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Statements/import)、[live bindings as concatenation](https://jameshfisher.com/2020/09/28/javascript-live-bindings-are-just-concatenation)、[circular dependency deep dive](https://ofriperetz.dev/articles/circular-dependencies-in-javascript-explained)、[CMake critique](https://twdev.blog/2021/08/cmake)、[CMake vs Bazel in practise](https://keasigmadelta.com/blog/cmake-vs-bazel-in-practise-real-code-real-results/)、[Meson comparisons](https://mesonbuild.com/Comparisons.html)、[Cangjie docs](https://cangjie-lang.cn/en/docs?url=%2F0.53.13%2Fuser_manual%2Fsource_en%2Ffirst_understanding%2Fbasic.html)、[Cangjie overview](https://medium.com/snapp-mobile/cangjie-a-programming-language-from-huawei-fb0f7776be06)。
