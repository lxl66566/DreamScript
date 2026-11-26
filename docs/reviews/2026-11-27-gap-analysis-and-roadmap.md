# DreamScript 缺口分析与方向建议（2026-11-27）

非规范性评审文档：基于 HEAD `08f836e6`，接续 `docs/reviews/2026-11-24-usability-limitations.md`（基线提交 `137762b1`，此后 54 个提交）。本文不记录裁决。语言表面或实现边界变化后，结论可能失效。

## 结论

三天内的落地量很大。基线评审约 70 条结论（含跨方向重复）里，15 条已解决，15 条部分解决，7 条已变化。字段与下标赋值、集合变更成员、`timeout` 与 channel deadline、assert 族与 `ds test`、trap 回溯、链接分层、开放错误箱都在这三天落地。上一份评审点名的五个改进杠杆都有实质进展。原地更新与集合操作一项已经兑现，JIT 一项目前只兑现了 README 的诚实表述。

新发现 50 余条缺口，重心与上一份不同。最重的一批落在两类。第一类是文档承诺的行为在实现里被破坏：集合写入对 `const` 别名可见，失败的 `net_listen` 放进子 fiber 后进程以 0 退出，经组合子的递归在 100 层深时 abort，嵌套 case 子模式通过检查后在运行期撞上 VM 内部错误。第二类是最日常的动作写不出来：负的 float 字面量，int 转 float，闭包进集合，比较器排序，给用户自己的类型定义 `max`。这两类决定外部采用者的第一印象，修复成本大多只有一个 S。

性能没有新数据。仓库里唯一的内核对照表采样于 11 月 20 日，此后 54 个提交没有刷新。章程目标“tier 1 落在 C 的 3 到 10 倍”从未被度量，因为基准套件里没有 C。

一句话：表面在快速变宽，承诺的兑现落后于表面。下一步应当从扩表面切到兑现承诺，用一组真实程序驱动排序，特性清单排在它们之后。

## 1. 方法与核实口径

三个 subagent 分头探索：语言表面与表达力，运行时与性能，标准库与工具链。每个方向先逐条核对基线评审的结论，再找新问题，最后用探针验证。

探针运行在预构建的 `target/debug/ds`（2026-11-26 19:14 构建，比 HEAD 早约 6 小时）。源码层面的结论以 HEAD 为准。每条缺口注明核实方式：已复现表示评审者用自己的探针跑出了相同结果，代码核对表示读过 HEAD 的对应分支，未验证表示只有 subagent 的报告。

评审者核对后改判了一条：`move s: string` 之后继续用 `s` 能通过检查，这是设计行为，`crates/ds-check/src/linearity/walk.rs:430` 只对不可复制类型判定移动，问题只在 tutorial 的措辞。线性资源经点调用传给 `move` 参数后重复使用被正确拒绝（DS3001）。

## 2. 基线变化

| 基线结论 | 现状 | 证据 |
|---|---|---|
| 没有字段赋值与下标赋值 | 已解决 | `docs/en/tutorial.md:248`，`conformance/place_stores.ds` |
| list 与 map 各只有三个成员 | 已解决 | list 7 个，map 6 个，含 `entries()`（`crates/ds-stdlib/src/ambient.ds:224-252`） |
| `write_file` 等失败即停，`die`/`mkdir`/`remove`/`exit` 是 CLI provision | 已解决 | 返回 `io_err` 并集或进入 ambient（`ambient.ds:647-658`） |
| 没有 timeout，channel 没有 deadline | 已解决 | `timeout`、`.send_deadline`、`.recv_deadline` |
| 没有测试框架与 assert | 已解决 | `ds test` 与 assert 族 |
| 没有任何格式化能力 | 已解决 | `str_fixed`、`hex`、`pad_left`、`pad_right` |
| README 的性能承诺与实现不符 | 已解决 | 改为 “the JIT tiers are designed but not started” |
| 注释里是裁决日期加编号 | 已解决 | 提交 `2277100f` 清退，CI 有守卫 |
| trap 只有一行 | 已变化 | 带源码位置回溯与 `?` 的错误轨迹 |
| 一致性冲突到链接期才报 | 已变化 | 链接分层（DS2047 到 DS2049） |
| 闭包没有可变 upvalue | 已变化 | 闭包能改自己的捕获，外层与兄弟闭包不能共享 |
| 错误签名沿调用链蔓延 | 部分解决 | 导出别名惯例，`any_err`/`box_err` 落地，跨包读取仍报 DS4021 |
| 没有一等引用的替代品 | 部分解决 | `string_view` 与 `arena[t]` 落地，arena 只收一个机器字的元素 |
| 泛型值与多字实例在 entering site 被拒 | 部分解决 | 自由泛型 fn 与泛型 struct 的 application 已按 shape 实例化，泛型 fn 值、泛型 lambda、泛型 enum 的 application 仍拒 |
| FFI 限制 | 部分解决 | string/bytes 参数与 unit 返回落地，4 个 C 字的上限等未变 |
| 执行器单线程，`par_map` 对用户永远顺序 | 仍成立 | `enable_fork_pool` 只有 `crates/ds-vm/tests/fork_pool.rs` 调用 |
| reactor 只有 epoll | 仍成立 | `crates/ds-vm/src/reactor.rs:104-115` |
| 句柄构造失败即停 | 仍成立 | 七个 total 入口，见 C2 |
| 没有 JIT | 仍成立 | `crates/` 下没有 `ds-jit` |
| node 在所有内核上快 4 到 14 倍 | 当时就不准 | 按原始数据实算为 1.29 倍到 23.1 倍 |
| tier 1 约为 C 的 15 到 30 倍 | 无法验证 | 仓库没有 C 基线，这一句是推断 |

## 3. 缺口清单

严重度分三档。blocker 表示某一整类常见程序写不出来或不可用，major 表示高频卡手或性能悬崖，minor 表示打磨。成本标记沿用 `docs/en/todo.md` 的 S、M、L。

### 3.1 承诺被破坏

| 编号 | 缺口 | 严重度 | 证据 | 核实 |
|---|---|---|---|---|
| C1 | 集合的就地写入对所有别名可见，`const` 绑定的内容会变 | blocker | `crates/ds-vm/src/object/list.rs:43-80` 的 `push_list` 直接追加，没有引用计数是否为 1 的判断。`docs/en/tutorial.md:252` 与 `docs/en/design-decisions.md:79` 承诺唯一持有时才就地复用，别名意味着复制 | 已复现，代码核对 |
| C2 | 七个 total 入口失败即停，放进子 fiber 后进程以 0 退出 | blocker | `open_file`、`net_connect`、`http_get`、`net_listen`、`net_accept`、`net_accept_tls`、`proc_spawn` 的失败降级为 `Op::Halt`（`crates/ds-lower/src/func/calls/result.rs:509-613`） | 已复现 |
| C3 | 经组合子的递归走 Rust 栈，深度不足时 abort，deinit 不运行 | major | `crates/ds-vm/src/vm/host.rs:91-251` 的 `call_hosted_in` 逐层嵌套 `execute_host_step`，没有栈余量检查 | 已复现 |
| C4 | 嵌套 case 子模式通过检查与 lowering，运行期撞上 VM 内部类型错位 | major | F4（`docs/en/design-decisions.md:145`）规定成员子模式在检查期拒绝 | 已复现 |
| C5 | `ds run` 把入口返回值打到 stdout，每个脚本末尾多一行 `0` | blocker | `crates/ds-cli/tests/examples.rs:47-50` 把末尾的 `0` 钉成期望输出 | 已复现 |
| C6 | 语句位置丢弃 `() \| io_err` 没有任何诊断 | major | `mkdir("/nonexistent/a/b/c")` 作为语句，程序打印 `done` 并以 0 退出 | 已复现 |
| C7 | trap 回溯没有帧数上限 | major | 非尾递归触发的值栈溢出打印 174764 行 | 已复现 |
| C8 | 检查期接受的代码在 lowering 被拒，文案是内部口径 | major | DS4009 “outside the M3 runtime contract”。负 float、union 接收者上的泛型调用、`list.set` 的两种拼法都属于这一类 | 已复现（负 float） |
| C9 | 诊断里的提示指向自身被拒或过期的写法 | major | 混合 int 与 float 时建议 `float(n)`，而 `float(n)` 报 DS2002（`crates/ds-check/src/type_of/ops.rs:294`）。int 与 `u32` 混算时建议已被永久拒绝的 `u32(x)`（`ops.rs:513`）。闭包捕获 `int` 后附注写 “`hits` is `~Copyable`”。字符比较报 “ord derives arrive at M6” | 已复现（`float(n)`） |
| C10 | SIGINT 打不断纯计算循环 | major | `timeout -s INT 1 ds run spin.ds` 两分钟后仍在运行。信号在 park 点才被观察（`crates/ds-cli/src/run.rs:76-82`） | 未验证 |
| C11 | 引用环可以由别名构造并永久泄漏 | 待定 | 由 C1 推断，`x.push(y)` 之后再 `y.push(x)`。没有弱引用，H4 的 collector hook 只是承诺 | 未验证 |

C1 的探针与输出：

```ds
var a = [1, 2]
const alias = a
a.push(3)
println("alias sees len ${alias.len()}")
// alias sees len 3

var src = [1, 2, 3, 4, 5]
var twin = src
var visits = 0
for x in src {
  visits += 1
  twin.remove(0)
}
// visits=3 len=2：5 个元素只走了 3 个，无 trap 无诊断
```

第二段绕过了 DS3007 对迭代中 `mut` 窗口的拒绝，循环静默提前结束。

C2 的探针与输出：

```ds
const answers = nursery {
  spawn {
    const second = net_listen("127.0.0.1", 47321)   // 端口已被占用
    const got = local_addr(second)
    got
  }
}
println("after nursery: ${answers.len()}")
```

```text
net error: net_listen 127.0.0.1:47321: Address already in use (os error 98)
after nursery: 1
end
```

进程以 0 退出。同一个调用放在顶层时打印错误行并以 1 退出，两种位置的行为不一致。

C3 的探针与输出：

```ds
fn rec(n: int) -> int {
  if n <= 0 {
    0
  } else {
    const ys = [n].map((x: int) => rec(x - 1))
    ys.get(0).unwrap_or(0) + 1
  }
}
```

```text
thread 'main' has overflowed his stack
fatal runtime error: stack overflow, aborting
```

`rec(100)` 在 debug 构建下 abort，退出码 134。VM 自己的值栈上限给出的是干净 trap，这条路径绕过了它。

C4 的探针与输出：

```ds
enum opt { some { value: int }, nothing }
struct cell { o: opt }

const c = cell { o: some(value: 3) }
println("${match c { cell { o: some { value } } => value, _ => 0 }}")
```

```text
trap: slot 4 holds Tag where I64 is required
```

### 3.2 日常动作写不出来

| 编号 | 缺口 | 严重度 | 证据 | 核实 |
|---|---|---|---|---|
| E1 | 负的 float 写不出来 | blocker | `crates/ds-lower/src/func/op.rs:1416-1422` 的一元负号只有 `I64` 分支。`const x = -3.5`、`-f`、`pow(2.0, -2.0)` 全部报 DS4009，唯一写法是 `0.0 - 3.5` | 已复现，代码核对 |
| E2 | int 与 float 之间没有转换函数 | major | 全表只有 `parse_float`、`int_of_byte`、`int_of_u32`、`u32.of`。`n * 1.0` 报 DS2001 | 已复现 |
| E3 | 可调用值的四处拒绝 | blocker | 闭包不能进集合、元组与 struct 字段，lambda 不能捕获 fn 类型的值，`f()()` 被拒，fn 类型字段上的 `h.run(1)` 被读成 UFCS。stdlib 的 `firstof`/`interleave` 因此对每个 child 线性扫描整个 thunk 表（`crates/ds-stdlib/src/bodied.ds:38-58`、`:100-117`） | 已复现（集合元素） |
| E4 | 递归 enum 不可用，arena 只收一个机器字的元素 | major | `branch { left: tree, right: tree }` 报 “a cyclic field type has no finite span”。`arena[node]()` 插入两字 struct 被拒 | 未验证 |
| E5 | 元组含 union 元素时分布成多个成员，不能一次解构 | major | `const (a, b) = p` 对 `(step \| none, int)` 报 DS2001。stdlib 自己改用 struct（`crates/ds-stdlib/src/json.ds:29-32`） | 未验证 |
| E6 | 排序只能靠 `derive(ord)`，`reduce` 的累加器必须与元素同型，`sum` 只收 `list[int]` | major | `sort_by` 不存在。`ord` 按字段顺序比较，按某个字段排序要求把它写在 struct 的第一位 | 未验证 |
| E7 | map 计数惯用法 `m[k] += 1` 遇到缺键即停 | major | `docs/en/tutorial.md:248`。可用写法是 `m.set(k, m.get(k).unwrap_or(0) + 1)` | 未验证 |
| E8 | @std 的自由函数名占着扁平的包命名空间 | major | `fn max(a: money, b: money)` 报 DS2005，指向 `std/ambient.ds:1767`。没有重载，`take`、`sum`、`contains`、`first`、`sort` 同理 | 已复现（`max`、`take`） |
| E9 | 没有局部函数，lambda 不能递归 | major | 函数体内的 `fn` 报 DS1001，递归辅助函数必须提到顶层 | 未验证 |
| E10 | checked 算术与 float 取余缺失 | major | B7 写明 “error-union arithmetic variants are library functions”，全表没有。`7.0 % 2.0` 报 DS4009 | 未验证 |
| E11 | 泛型与 comptime 的残余 | major | 泛型 fn 值、泛型 lambda、泛型 enum 的 application 仍拒。comptime 参数只收 `type` 与四个标量。跨包泛型等模板编码（todo 140、58） | 代码核对（`docs/en/tutorial.md:331`） |
| E12 | 模式匹配缺 range 模式与 `@` binder，`return` 不能作 arm 值 | minor | `0..=9 => ...` 报 DS1001。DS2039 | 未验证 |
| E13 | 闭包捕获 `var` 后独占该绑定 | minor | 外层与第二个闭包再用报 DS3001。绕法是把状态放进堆上的 `list` | 未验证 |
| E14 | 数值与字符的杂项 | minor | `'a' < 'b'` 报 DS2001。`"${3.0}"` 打印 `3`。`u32` 不可插值。B9 点名的 `is_float_eq`/`is_approx_eq` 不存在。enum case 用花括号声明，却必须用调用形式构造 | 未验证 |
| E15 | 集合成员的小缺陷 | minor | `xs.set(0, 100)` 报 DS2014，按提示写 `list::set` 又报 “list is not a trait”。`a.clone()` 对 list 没有实例。带 deinit 的实参在 tail 位置被拒，与 H2 “no callee-class exception” 有出入 | 已复现（tail 位置） |

E1、E2、E3、E8 的探针与输出：

```text
const x = -3.5
error[DS4009]: a unary operation is outside the M3 runtime contract

println("${float(n)}")
error[DS2002]: `float` is a type; construct structs with `name { ... }` and tags by name

fns.push((x: int) => x + k)
error[DS4009]: a list_push element is outside the M3 runtime contract

fn max(a: money, b: money) -> money
error[DS2005]: `max` is already defined
```

### 3.3 失败路径：所有权与 IO

| 编号 | 缺口 | 严重度 | 证据 | 核实 |
|---|---|---|---|---|
| F1 | 带句柄的调用没有非终止写法，`timeout` 也包不住 | blocker | `timeout(…, () => net_connect(…))` 报 DS4009 “a deinit-carrying union member”。唯一的超时是宿主默认的 3 秒（`crates/ds-vm/src/net.rs:143`） | 已复现（DS4009） |
| F2 | 文件面只有只读顺序句柄 | major | `open_file` 只读（`crates/ds-vm/src/fsys.rs:101-122`）。没有追加、seek、`read_up_to`，`read_raw` 必须恰好读满。`fs_list` 只列一层，`mkdir` 只建单层。没有 mtime、rename、copy、递归遍历、锁。`fs_size` 与 `file_size` 实现相同，命名混用 `file_*`、`fs_*` 与裸名 | 未验证 |
| F3 | 错误标签信息量不足 | minor | `not_found` 不带路径，`parse_err` 只有位置。`file_exists` 把权限错误吞成 false。`http_get` 的 effect 行记为 `error \| world`，签名无法表达错误 | 未验证 |
| F4 | 别名在语言层没有替代 | 设计裁决 | C2 永久排除一等引用。C4b 的量化触发已经满足：同一内核经 `arena[int]` 4.5499G Ir，经手写的 id 加表 15.8106G Ir，相差 3.47 倍（todo 120） | 代码核对 |

### 3.4 并发与平台

| 编号 | 缺口 | 严重度 | 证据 | 核实 |
|---|---|---|---|---|
| P1 | `par_map`/`par_reduce` 对用户没有任何并行 | major | `enable_fork_pool` 只有测试调用。`docs/en/tutorial.md:471` 仍写 “deterministic, replayable, and lock-free by theorem” | 代码核对 |
| P2 | macOS 与 Windows 上没有 reactor，全部世界操作阻塞唯一的线程 | blocker | `crates/ds-vm/src/reactor.rs:104-115` 在非 Linux 返回 `None`。CI 矩阵只有 ubuntu 与 windows（`.github/workflows/ci.yml:47`） | 代码核对 |
| P3 | channel 载荷 | major | 带 deinit 的载荷在 send 被拒。deadline 孪生只收一个机器字（`crates/ds-stdlib/src/ambient.ds:266-267`） | 代码核对 |
| P4 | `http_get` 只覆盖 close-delimited 的 HTTP/1.0 GET | major | 不检查状态行，404 与 500 当成功。不支持 chunked。响应上限 1 MiB。签名只有 `(url) -> string`（`crates/ds-vm/src/net.rs:86-105`） | 代码核对 |
| P5 | `platform "web"` 没有宿主 | minor | run 路径只装 `std` 与 `embed` | 未验证 |
| P6 | 执行器单线程 | 设计裁决 | 多线程执行器排在 1.0 之后（E2） | 代码核对（`docs/en/status.md:14`） |

### 3.5 性能

`benchmarks/results/2026-11-21-kernels.jsonl` 是仓库里唯一的一份内核对照，采样于 2026-11-20，单次采样，lua 全部 skip。比值是 ds 的耗时除以对手的耗时。

| 内核 | ds ms | python ms | node ms | ds/python | ds/node |
|---|---:|---:|---:|---:|---:|
| fib | 993 | 431 | 70 | 2.30 | 14.19 |
| closures | 856 | 563 | 37 | 1.52 | 23.14 |
| lists | 755 | 394 | 133 | 1.92 | 5.68 |
| combinators | 472 | 241 | 61 | 1.96 | 7.74 |
| records | 276 | 246 | 32 | 1.12 | 8.62 |
| maps | 67 | 36 | 33 | 1.86 | 2.03 |
| strings | 54 | 28 | 31 | 1.93 | 1.74 |
| loops-arith | 581 | 1875 | 452 | 0.31 | 1.29 |
| float-loop | 433 | 822 | 46 | 0.53 | 9.41 |
| containers | 161 | 267 | 103 | 0.60 | 1.56 |
| nursery | 294 | 1961 | 82 | 0.15 | 3.59 |

几何平均：ds/python 0.98，ds/node 4.78。纯算术循环快，调用密集与分配密集的内核慢。启动是真实优势，floor 3.7 ms，热路径 4.1 ms，冷路径 18.7 ms，不过 `startup.sh` 只测 ds，没有对手对照。

| 编号 | 缺口 | 严重度 | 证据 | 核实 |
|---|---|---|---|---|
| X1 | 对照数据过期，且没有 C 基线 | major | 54 个提交没有刷新。`benchmarks/README.md:3` 写明只有 ds、python、lua、node | 代码核对 |
| X2 | tier 1 的槽值逐槽带标签 | major | `crates/ds-vm/src/value.rs:47-76` 是 `I64`/`F64`/`Bool`/`Tag`/`Ref`/`Fn`/`Arena` 七种带标签的枚举。BL-2 写的是 “no per-slot tags”（`docs/en/design-decisions.md:250`） | 代码核对 |
| X3 | 集合操作带分配税，标准库组合子是二次复杂度 | major | `enumerate` 每个元素一次 `concat`（`crates/ds-stdlib/src/bodied.ds:88-96`）。`zip` 是双层循环（`:173-188`），debug 构建下 500 个元素 399 ms，4000 个 25.7 s | 代码核对 |
| X4 | `char` 是堆上的单字符 string 引用 | major | `crates/ds-lower/src/shape.rs:72` 把 `char` 映射到 `Ref`。`char_at` 每次调用分配一个对象，json 解析器逐字符 `char_at` | 代码核对 |
| X5 | JIT 没有起步，中间层不存在 | major | 没有 `ds-jit` crate，没有 bytecode 到 SSA 的层 | 代码核对 |
| X6 | 特性轮次吸收了性能漂移 | major | `docs/en/design-decisions.md:125` 记录：未改动的树在门禁主机上已偏离提交的 pin 表 2.0% 到 5.2%，再定基线一并吸收 | 代码核对 |
| X7 | 其它结构成本 | minor | 每条指令逐条解码。分发是对 opcode 的 `match`，BL-3 写的是 computed goto 或尾调用线程。`CLOSURE_NEW` 两次分配。托管调用逐字拷参数并压哨兵帧。内联缓存只缓存 callee 的解析 | 未验证 |

### 3.6 标准库、包与生态

| 编号 | 缺口 | 严重度 | 证据 | 核实 |
|---|---|---|---|---|
| S1 | 标准库缺项 | major | 文本：regex、glob、Unicode 规范化、`starts_with`、`ends_with`、`lines`、builder。集合：set、堆、deque、`flat_map`、`scan`、`compose`、map 字面量。文件见 F2。IO：HTTP 方法与请求头、HTTP server、UDP。时间：时区、时长。数学：bigint、decimal。编码：base64、哈希函数、压缩、归档、csv、toml、yaml。另有日志与命令行参数解析 | 枚举（`crates/ds-stdlib/src/*.ds`） |
| S2 | `println` 只收 `string` | minor | `println(42)` 报 DS2001 | 未验证 |
| S3 | FFI 接不上真实的 C 库 | blocker | `MAX_PARAMS = 4`（`crates/ds-ffi/src/marshal.rs:26`）。没有 string 返回、结构体、回调、deinit 句柄。字符串以 `(ptr, len)` 传入。`ds bind` 只打印导出符号，不生成声明。`[build]` deferred | 代码核对 |
| S4 | 包管理只有解析与存储 | major | 没有 `add`、`publish`、`init`。manifest 只有 name、version、deps、artifacts。依赖哈希要从 `ds save` 的输出手抄。bundle 只收 `.ds`，数据文件发不了包 | 未验证 |
| S5 | 发布与安装 | minor | 手工下载归档（`README.md:92`）。fuzz 的定时任务被注释（`.github/workflows/fuzz.yml:4`） | 代码核对 |

### 3.7 工具链与上手体验

| 编号 | 缺口 | 严重度 | 证据 | 核实 |
|---|---|---|---|---|
| T1 | 编辑器对脚本模式的文件报假错误 | major | LSP 对能被 `ds run` 跑通的两行脚本发布两条 error。带 `ds.pkg` 的入口反而不能走脚本模式，DS1014 的提示自相矛盾。没有 manifest 的 run 看不到兄弟文件 | 未验证 |
| T2 | 断言失败不报告测试文件里的行号 | major | 回溯只有 `assert_eq_string at std/ambient.ds:1959:5`，尾调用省掉了用户帧，`std/ambient.ds` 是磁盘上不存在的路径 | 未验证 |
| T3 | `ds test` 缺过滤、并行与共享辅助模块 | minor | 只有 `--list` 与 `--no-cache`。`ds test b_add` 回答 “is not a directory” 并以 0 退出 | 未验证 |
| T4 | 调试器与 profiler 只有 VM API | minor | `ds` 的 12 个子命令里没有入口。采样只数回边，不含时间 | 代码核对 |
| T5 | LSP 与 REPL 的打磨项 | minor | 缺 codeAction、signatureHelp、inlayHint、foldingRange。REPL 把 `"a" + "b"` 显示成 `<object 2>` | 未验证 |

### 3.8 文档

| 编号 | 缺口 | 严重度 | 证据 | 核实 |
|---|---|---|---|---|
| D1 | tutorial 与实现漂移 | major | 第 477 行写 channel “no deadline”。第 324 行写泛型函数在字节码里只有一份。第 331 行写泛型 struct 的值在 lowering 被拒。第 267 行写 `split`/`join` 只收 `char`。第 265 行写 place 链上的复合赋值被拒，第 248 行写它可用。第 491 行把三层执行写成现状。第 228 行写每个值都是仿射的，而可复制类型按复制处理 | 代码核对 |
| D2 | 参考没有说明文字 | major | `docs/en/reference.md` 的 221 条里 157 条只有签名。标准库 220 个 `pub` 声明的 `///` 文档注释数为 0 | 代码核对 |
| D3 | 索引与记录的缺口 | minor | `README.md` 的命令表缺 `test` 与 `doc`。`docs/README.md` 的索引缺 `reference.md` 与 `errors.md`。没有 CHANGELOG，只有一个 tag。tutorial 的 `@std/csv` 示例在 run 下会被拒绝 | 代码核对 |

## 4. 如何解决

### 4.1 兑现被破坏的承诺

C1 有三条路。(a) 变更类 native 在引用计数大于 1 时先复制再写，这正是 D2 的原文，成本 M。`concat` 在左操作数唯一持有时原地追加，同一个判断同时解决 X3 的分配税。(b) 检查器在源绑定仍存活时拒绝集合的 `var` 复制，成本 M，过程内检查看不到跨函数的别名。(c) 改文档承认集合是引用语义，等于修订 D1、D2 与 C3。推荐 (a)。它与 C1 “窗口内零运行时检查”有一点张力，代价是一次计数读加一个分支，而 C1 本来就把 rc==1 唯一性检查列为诊断层。

C2 有两条路。(a) todo 76 的 optional-over-linear union，作者已经排在所有权轮首位，一个形状同时解决 listen、accept、connect、`open_file`、`proc_spawn` 与 F1。(b) 先止血，把 total 失败从 `Op::Halt` 改成 `Die` trap，子 fiber 内同样终止、以 1 退出、deinit 照常运行，成本 S。推荐 (b) 立即做，(a) 进所有权轮。

C3 有三条路。(a) 托管调用前探测 Rust 栈余量，不足时给出 `Trap::StackOverflow`，成本 S。(b) 把元素级组合子改进解释器循环内驱动，去掉 Rust 重入，与 todo 129 的 CALLG 重入成本是同一个方向，成本 L。(c) CLI 给主线程加大栈。推荐 (a) 与 (c) 立即做，(b) 并入 tier 1 降本。

C4、C8、C9 共用一条原则：检查期接受的程序，lowering 必须能降。做不到的，把拒绝前移到检查期并给出可操作的提示。逐项是：负 float 补 F64 分支并把负号折进字面量，嵌套 case 子模式在检查期按 F4 拒绝，`list.set` 的成员解析按接收者类型消歧，三项各 S。文案方面，DS4009 去掉 “M3 contract”，`$t#N` 映射成可读类型，人类渲染去掉 `[B2]` 一类的决策号，修正三条指向被拒写法的提示。

C5 到 C7 各是一处小改动。`ds run` 对 unit 答案不再打印，其余类型只在 `--print-result` 下打印，回显交给 REPL，同时更新钉死的 golden。返回 `io_err` 或 `net_err` 并集的 world extern 在语句位置被丢弃时给出警告，先只对这一类生效。回溯加帧数上限与 `--backtrace` 档位。

C10 先在 HEAD 复验。成立的话，在回边或时间片预算处检查 shutdown 标志，这与只在安全点取消的裁决相容。

### 4.2 让日常动作一步可达

数值：加 `float.of(n)` 与 `int.of(f)`，与 `u32.of`、`box_err.of` 用同一种拼法。加 `checked_add` 一族，返回 `int | none`。加 float 的 `rem`。`<` 与 `>` 对带 `ord` 实例的类型路由到 compare。每项 S。

可调用值：(a) computed callee 先降成临时绑定再调用，成本 S，与章程无冲突。(b) fn 类型的字段、元组元素、集合元素接受闭包的引用字，成本 M，需要修订 F2 在 2026-11-10 的裁决。(c) 闭包捕获 fn 类型的参数，依赖 (b)。stdlib 的 `firstof` 与 `interleave` 随之改写成线性扫描。

数据处理：`sort_by(xs, cmp)` 走 todo 134 已经点名的 compare-callable 路线。`fold[a, t](xs, init, f)` 的签名是一阶的，落在 B2 在 2026-11-25 的裁决内。map 加 `get_or`，`sum` 扩到 float。每项 S。

命名：把 F3 的接收者回退扩到自由函数之间，两个同名自由函数的首参类型集合不交就共存，用户声明优先于 ambient。成本 M，与链接分层的 root 优先同向。另一条路是让 @std 的 bodied 成员进具名模块，它与合并 ambient 的设计相反。

递归 enum 与 arena：`box[t]` 或自动装箱需要在 B1 的 unboxed 布局下裁决，建议并入所有权轮（todo 120）。arena 的多字元素跟 todo 140。

元组含 union 元素：让解构与 match 把分布后的元组仍然当作一个乘积来匹配，表示不变。

局部函数：允许块内 `fn`，名字在自己的 body 内可见，需要修订 F2 的措辞。

模式匹配：range 模式是 F4 没有排除的。`@` binder、`?.`、`??`、`|>` 留给 M9 分诊，由语料数据决定。

泛型残余：todo 140 的后续切片，包括泛型 enum、泛型 impl 成员与 trait default、跨包模板编码。它们是 stdlib 泛型体、comptime walker、第三方字段泛型代码、多字 channel 载荷的共同前提，解锁度最高。

### 4.3 所有权与失败路径

所有权轮有两项。第一项是 optional-over-linear union，见 4.1 的 C2。第二项是 C4b，量化触发已经满足。形状是只读别名：不拥有，可存字段，可逃逸调用，解引用时一次 O(1) 的世代检查，悬空是确定性 trap。写入仍然走 D1，E3 的无竞态定理与 C4 的非原子 RC 都成立。章程对所有权的要求是学习成本低、错误率低、保留性能收益，这个形状符合。

文件面补写句柄与追加、`read_up_to`、`stat`、rename、递归遍历，都是 native 与库的加法。顺手统一命名，删掉重复的 `fs_size`。

错误标签加可选的路径字段，趁 1.0 前做。`file_exists` 改为 `bool | io_err`。

### 4.4 并发与平台

`par_map` 加 `ds run --par`（todo 45），或者在此之前改名。两者取其一，同时改掉 tutorial 第 471 行的表述。

非 Linux 平台先用 todo 79 记录的 threadpool-bridge reactor 买到行为对等，kqueue 随后。CI 矩阵加 macOS 是一行改动。

HTTP 加 `http_request(method, url, headers, body)`，返回带 status、headers、body 的记录，支持 chunked 与可配超时。在此之前先让 `http_get` 返回状态并解除 1 MiB 上限。

多线程执行器已经裁决排在 1.0 之后。代价是 macOS 上开发、Linux 上部署的程序在并发 IO 上行为不同，threadpool-bridge 就是为了缩小这个差距。

### 4.5 性能

先让目标可度量。刷新内核对照表，加入 C 与 Rust 的基线内核，启动测试加对手对照。之后每个特性轮次结束跑一次并提交结果。

tier 1 结构降本排在 JIT 之前。按静态 `SlotType` 去掉读路径的标签检查与边界检查，BL-2 本来就这么要求。闭包单次分配。CALLG 批量重入。`concat` 的唯一持有复用。`zip` 与 `enumerate` 改成 native。`char` 先用常用码点的驻留表止血，未装箱的标量按章程第 7 条作为终局。

tier 2 之前先搭跨 tier 的差分测试骨架。bytecode 到 SSA 的层还不存在，`docs/en/implementation-plan.md` 第 5 节已经要求 M8 起每个语料文件在每个 tier 上运行。

流程上，pin 表的绝对数值在工具链滚动后会整体漂移，再定基线只应在纯优化提交里做。特性提交带来的指令数变化要逐条归因，否则性能回归门禁在高速加特性时失去分辨力。

### 4.6 标准库、包与生态

标准库按语料补缺。立即可做的 S 项：`println` 接受带 `show` 的值，`starts_with`、`ends_with`、`lines`、`repeat`，`get_or`、`sort_by`、`fold`、`flat_map`，`read_up_to` 与追加写。

FFI 先做 todo 63 已排的 libffi 式调用与 C 字符串返回，随后是结构体与回调。`ds bind` 生成声明。`[build]` 跟上。在此之前可以约定一层对 ds 友好的 C shim。这是生态冷启动最硬的墙。

包管理加 `ds add`，由它解析并写入 pin，维持 manifest 即 lock 的裁决。随后是元数据键、发布命令、数据文件入包、vendoring。

发布补校验和与一行安装脚本，恢复 fuzz 的定时任务。

### 4.7 工具链

LSP 把打开的脚本文档当作入口，或者先把这一条降为 hint。带 manifest 的入口允许脚本模式。

`ds test` 失败时渲染被省略的尾帧，加 `--filter` 与带 job 上限的并行，约定辅助模块的命名。

`ds debug` 与 `ds run --profile` 接上已有的 VM API。REPL 对 `Ref` 值走 `show`。

### 4.8 文档

tutorial 的七处对齐实现。给叙述句加漂移测试成本高，更便宜的办法是把会漂移的行为细节留在 `status.md`，tutorial 只保留稳定的语义。

标准库每个 `pub` 声明补 `///`，CI 加一条门禁，`ds doc` 渲染说明。LSP 的 hover 随之有内容。

README 的命令表与 docs 索引补齐。建立 CHANGELOG 与破坏性迁移说明。`@std/csv` 示例换成能跑的。

## 5. 如何决定发展方向

### 5.1 总判断

仓库 18 天积累了约 30 万行 Rust，最近三天 54 个提交分布在每一个子系统。速度是优势，也带来了这批缺口的共同模式：每一个落地的表面，都有一处相邻的组合没有被语料走到。别名加 `push`，total 拼写加 spawn，组合子加递归，嵌套的 case 模式，都是这样漏过去的。conformance 语料约 40 个单文件，跨特性的组合只靠 crate 内的 e2e 覆盖。

所以方向的答案是收敛，而收敛需要一个度量。特性清单回答不了“哪一处最卡手”，真实程序可以。

### 5.2 决策标准

每个缺口与每个提案依次过五个问题，顺序就是优先级。

1. 它是否违反已经写下的承诺，包括章程、tutorial、design-decisions 与 status。是则无条件优先。这类修复通常便宜，而违反会同时摧毁文档与编译器的可信度。
2. 它挡住哪一类负载。对照 A2，CLI、工具与脚本是主负载，游戏与模拟是次负载，服务端是 fiber 与 nursery 设计的隐含对象。主负载挡路的最优先。
3. 它解锁多少下游工作。optional-over-linear union、per-shape 实例化的后续切片、引用计数检查各自一次解决十条以上的库级待办。解锁度高的排前。
4. 修复成本与不可逆性。便宜且不可逆的表示级决定先做，章程第 7 条要求只看长远影响。昂贵且可延后的工作，例如 JIT 与多线程执行器，排在表面收敛之后，否则每加一个特性都要为它多付一次回归风险。
5. 它是否需要修宪。需要的单列为作者裁决，不混进执行队列，也不由实现顺手决定。

### 5.3 用真实程序语料度量

建议建立一组 10 个左右、每个 100 到 500 行的真实程序，覆盖主负载的典型形态：JSON 与 CSV 的清洗汇总，带子命令的 CLI 工具，调 REST API 并带超时与重试的抓取器，目录同步与清理工具，日志分析，一个小语言的解释器，文件格式转换，构建脚本，一个带测试的库包，一个原生库的绑定。

每个程序记录五项：首次编译通过率，被拒次数与拒绝码的分布，为了绕开缺口多写的行数，运行时间与内存，三个平台上的输出是否一致。

这把 `docs/en/todo.md` 里已有的量化触发纪律推广到整个 backlog。C4b 与 B2 的推断错误成员已经采用这种写法，每个延期项写明触发它的语料证据，每个新特性提案写明它服务语料里的哪个程序。方向的定义就是这五项数字单调改善。

### 5.4 建议的四个阶段

| 阶段 | 目标 | 内容 | 出口判据 |
|---|---|---|---|
| 0 兑现承诺 | 文档说的等于程序做的 | C1 到 C9，D1，CI 加 macOS，bench 刷新并加 C 基线 | 语料里没有正确性反例。tutorial 与实现一致。有 C 基线的对照表 |
| 1 主负载成立 | CLI 工具与脚本写得出、跑得对 | 可调用值，数值转换，数据处理三件套与组合子，命名，局部函数，文件面，HTTP，`--par` 或改名，LSP 脚本模式，断言行号，标准库 `///`。同时建立语料 | 语料全部跑通。管道输出干净。LSP 没有假错误 |
| 2 性能与所有权 | 兑现编译语言性能与别名 | 表面冻结一个切片。tier 1 降本。SSA 与 tier 2，带跨 tier 差分。optional-over-linear union。C4b。threadpool-bridge reactor | tier 1 与 C 的比值进入章程目标。跨 tier 差分为零。非 Linux 上并发 IO 行为对等 |
| 3 生态与 1.0 | 别人能发包、能绑库 | FFI 扩面与 `[build]`，registry 与发布，HTTP server 包，wasm runner，调试器前端，semver | 一个第三方包从发布到被依赖走通 |

排序的理由是每一阶段的产出是下一阶段的输入。语料给出性能与表达力的真实优先级。表面冻结让 JIT 有稳定的目标。性能到位之后，生态投入才有回报。

### 5.5 需要作者裁决的问题

1. F2 “闭包字不得进入集合元素或字段”是否修订，这是 E3 的根治。
2. 引用计数检查落地后与 C1 “窗口内零运行时检查”的张力。接受一次计数读，还是改文档承认集合是引用语义。
3. 命名占位。自由函数之间按首参类型不交共存，还是让 @std 进具名模块。
4. 局部函数需要修订 F2 的措辞。
5. `ds run` 是否回显入口返回值。
6. C4b 的触发条件已经满足，是否现在拉入所有权轮。
7. 递归 enum 的装箱，这是 B1 的表示问题。
8. `char` 的表示，未装箱的标量还是驻留表。
9. 多线程执行器是否仍然排在 1.0 之后。单线程加阻塞孪生意味着 macOS 与 Linux 上行为不同。
10. `platform "web"` 与热重载这类已有基础设施而没有消费者的特性，是否冻结到主负载成立之后。
11. pin 再定基线的纪律。
12. `par_map` 的名字，加 `--par` 还是改名。

### 5.6 不建议做的事

在 JIT 之前继续扩表面。每加一个特性，tier 2 的目标就多一个变量。

给单个 API 打补丁绕开 C5 与 C4b 这两个 seam。作者在 todo 76 已经写明没有 accept 专属的补丁。

用改文档的方式消化 bug。别名可见、`ds run` 的 `0`、负 float 被拒都应当修实现。

补齐 Kotlin 级的语法糖。语料没有要的糖不加，`?.` 与 `??` 等分诊的数据。

在 optional-over-linear union 落地之前投资服务端库，例如 HTTP server 与 supervision。

## 6. 应当保护的部分

尾调用保证实测成立，一千万次互调栈不增长。值栈上限是干净 trap。确定性释放与取消时的 deinit 由门禁与 fuzz 镜像覆盖。

启动时间，floor 3.7 ms，热路径 4.1 ms。

诊断的基本盘：双 span，did-you-mean，JSON 里带 suggestion，缺分支时点名缺失成员。

`.err`/`.ok` 的差集类型，UFCS 与接收者回退，嵌套位置的赋值语法。

nursery，channel 的 rendezvous 与 deadline，`timeout` 复用 io 分类。

`ds fmt` 的零配置与幂等。`ds doc` 的生成加漂移测试。

501 行装下整个表面的可解释性预算。

## 证据来源

- `docs/en/design-decisions.md`、`docs/en/status.md`、`docs/en/todo.md`、`docs/en/tutorial.md`、`docs/en/implementation-plan.md`
- `docs/reviews/2026-11-24-usability-limitations.md`（基线）
- `benchmarks/results/2026-11-21-kernels.jsonl`、`benchmarks/results/2026-11-21-startup.jsonl`、`benchmarks/README.md`
- `crates/ds-vm/src/object/list.rs`、`crates/ds-vm/src/native/list.rs`、`crates/ds-vm/src/vm/host.rs`、`crates/ds-vm/src/vm/execute.rs`、`crates/ds-vm/src/value.rs`、`crates/ds-vm/src/reactor.rs`、`crates/ds-vm/src/net.rs`、`crates/ds-vm/src/fsys.rs`
- `crates/ds-lower/src/func/op.rs`、`crates/ds-lower/src/shape.rs`、`crates/ds-lower/src/func/calls/result.rs`
- `crates/ds-check/src/type_of/ops.rs`、`crates/ds-check/src/linearity/walk.rs`
- `crates/ds-stdlib/src/*.ds`、`crates/ds-ffi/src/marshal.rs`、`crates/ds-cli/src/run.rs`、`crates/ds-cli/tests/examples.rs`、`crates/ds-lsp/src/lib.rs`
- `.github/workflows/ci.yml`、`.github/workflows/fuzz.yml`、`README.md`
- 评审者在 `target/debug/ds` 上运行的 17 个探针程序
