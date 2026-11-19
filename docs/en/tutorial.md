# DreamScript in one short tutorial

DreamScript is a general-purpose interpreted language with static set-theoretic types, affine-by-default values, guaranteed tail calls, reference-counted memory with no GC, and optional JIT tiers. This file is the entire v1 surface, the artifact of the explainability budget: the whole language fits one short tutorial.

Run examples as scripts with `ds run file.ds`. Snippets below are fragments; top-level statements are legal only in a script's entry file (see Modules and script mode).

## Values and bindings

A binding is `const` (the default, immutable) or `var` (a rebindable local); rebinding a `var` changes which value the name holds, while changing a value in place is a separate, always-visible mechanism (see Linearity).

```ts
const name = "dreamscript"   // immutable, type inferred as string
fn count_up() -> int {
  var count = 0              // a rebindable local
  count = count + 1          // rebinding, not mutation
  count
}
```

Numbers: `int` is 64-bit signed, `float` is 64-bit IEEE, both raw in slots; literals take separators and radix prefixes (`1_000_000`, `0xff`, `0b1010`, `0o777`); a width suffix (`42u32`) names a declared type, so undeclared widths refuse, and a suffixed literal itself still refuses at lowering today — the width word is built by the conversions named below. Arithmetic traps on overflow and division by zero (`1 / 0` panics; floats follow IEEE and never trap, so `1.0 / 0.0` is `+inf`); wrapping is explicit (`a +% b`, `a *% b`, `-%a`), and narrowing is an explicit call that waits on the width types (`i32(u)`, `i64(f)`), while `u32.of(x)` answers today — the ambient's trapping conversion on the width's own static tier, in range the width's word and out of range a trap naming the value and the range (the checked `u32_of` answers `u32 | none` instead; the bare `u32(x)` spelling refuses as a type called as a function under F9's namespace priority, awaiting the author's word); the shift and bitwise operators (`<<`, `>>` arithmetic, `&`, `^`, `|`) lower over ints, a shift amount at or past 64 (or below zero) trapping like overflow and division by zero, with the bitwise trio total.

Strings are immutable UTF-8 with escapes `\n \t \r \\ \" \' \0 \u{...}`; raw strings `r"..."` and nestable `r#"..."#` carry no escapes and cover multiline needs; `'x'` is a Unicode scalar char, `b'x'` a single byte (its `\u{..}` escape spelling the full 0-255 range, `b'\u{ff}'` naming any value while a raw non-ASCII char refuses), `code_of(c)` reads the char's scalar back, and the checked `char_of(code)` answers `string | none`, `none` naming a code that is no Unicode scalar. Interpolation is a string-only context: `"$x"` and `"${e}"` desugar to `str(e)` and compile when the operand is a printable primitive (`int`, `float`, `bool`, `char`, `byte`, `string`) or carries `show`, the hole staging the solved `show` dispatch at its own span (a union receiver through its covering impls); a union or optional operand with no covering `show` is an error — extract first or implement `show` over the whole union.

```ts
const who = "ada"
const tries = 3
const msg = "hi $who, next attempt ${tries + 1}"   // both hole spellings
const path = r"C:\root\tmp"                        // raw: no escapes
const first = 'x'                                  // char (b'x' is a byte)
```

Equality is one structural `==` over all data, never overloadable; no `===` exists. Floats compare bit-structurally (`nan == nan` is `true`), function comparison is rejected statically, and mutable cells and `~Copyable` resources compare by identity.

## Types are sets

Declarations are nominal — every `tag`, `struct`, and `enum` has identity — and the type language above them is set algebra: unions `a | b`, intersections `a & b`, and differences `a - b` over nominal atoms. Intersections keep the most precise member's layout; difference types erase entirely. Type aliases never declare cases; payload cases live only in enum blocks.

```ts
struct bad_port { got: int }
struct page { url: string, body: string }
struct user { name: string, age: int }
enum parse_error { bad_number(string), missing_field(string) }
type entry = map[string, string]
```

The ambient predeclares the failure taxonomy of the fallible reads — `io_err`, `parse_err`, the net error `net_err` (`timeout | refused`), and the bytes-decode error `utf8_err` — as it predeclares `none`, the `json` value family (one enum case per grammar shape, the number split kept faithful), and the `config` record the extraction example below uses, so a script's errors and parsed documents share one vocabulary; redeclaring any of them is a duplicate-definition error.

Construction is fixed per declaration: structs use brace form only, enum cases call form only, unit tags are bare, and cross-spellings are checker errors; struct braces support punning.

```ts
timeout                                    // unit tag: bare
bad_number("nan")                          // enum case: call form only
page { url: "https://ds.dev", body: "" }   // struct: brace form only
const url = "https://ds.dev"
const home = page { url, body: "" }        // punning: the field fills from the local
```

`T?` is exactly `T | none` and is legal in every annotation position; `none` is an ordinary unit tag, so optionals, error unions, and enums share one mechanism — narrowing, matching, one extraction family — with no `?.` operator.

Narrowing is `is`: Kotlin-shaped type narrowing (never identity comparison) whose right side is a type expression, so `|` and `&` are legal there. Refinement narrows monotonically through `is` tests, none tests, and match arms, resetting only at assignment.

```ts
tag doc_null
tag doc_bool
struct doc_num { value: int }
struct doc_str { value: string }
struct doc_obj { fields: map[string, doc] }
type doc = doc_null | doc_bool | doc_num | doc_str | doc_obj

fn port_of(j: doc) -> int | none | bad_port {
  if j is doc_obj {
    const pv = j.fields.map_get("port")        // inferred: doc | none
    if pv is doc_num && pv.value >= 1 && pv.value <= 65535 { pv.value }
    else { bad_port { got: -1 } }
  } else {
    none
  }
}
```

Types and values resolve in two namespaces: constructors (brace forms, bare tags, generic applications like `list[page]()`) in the type namespace, functions in the value namespace. A struct `map` and a function `map` coexist; a call-form name in both is an ambiguity error, never a priority search.

Lists and tuples: `[1, 2, 3]` is a `list[int]` literal, the empty mutable collection is `list[page]()`, and map literals are deferred (maps are built by calls for now; union values ride the map value channel, so a `map[string, json]` read runs — the ambient's `json_object` case is exactly that map). Ad-hoc aggregates are anonymous tuples — `("host", 8080)` — with destructuring in bindings and patterns.

## Functions

`fn` is the only named-function declaration — at top level and as trait/impl members — and the defined name is in scope inside its own body, so recursion is direct. Parameters are `name: t`, returns are `-> t`, union returns parse as `-> a | b`, and `T?` is legal in any annotation.

The body's final expression is the return value; `return e` exits early; `break`/`continue` serve loops; `()` is the unit type; a function returning nothing may omit the annotation. Signatures are required on top-level and public definitions and anchor the set algebra; locals, lambdas, and match arms are inferred.

Anonymous functions are const-arrow lambdas, the only other function spelling: a binding inside any block names one, while top level names functions with `fn` only.

```ts
fn doubled(xs: list[int]) -> list[int] {
  const double = (x: int) => x * 2
  const out = xs.map(double)
  out
}

println("${doubled([1, 2, 3]).reduce(0, (a, b) => a + b)}")
```

Functions are values end to end: a named fn binds and passes like any value, a `fn(t) -> r` parameter or return names a fn type, and any callable (named fn, lambda, capturing closure) inhabits the slot with one call spelling. A fn-typed return builds factories: `(k: int) => (x: int) => x + k` makes add-k functions.

```ts
fn apply(f: fn(int) -> int, v: int) -> int { f(v) }
fn inc(x: int) -> int { x + 1 }

const k = 10
const g = inc                           // a named fn binds like any value
const a = apply(g, 4)                   // and fills the fn-typed parameter
const b = apply((x: int) => x * k, 4)   // so does a capturing closure
const raised = [1, 2].map(inc)          // one combinator parameter, every callable
```

UFCS is universal and free: `x.f(a)` desugars to `f(x, a)` for any first-argument function, with no candidate search — free functions first (a same-named free function owns the dot position even when its parameter types do not fit, so the member needs the hatch), then trait methods, `trait::f(x)` as the escape hatch, and true ambiguity a compile error. Capability parameters are named and excluded from the dot position: `println(io, msg)` can never be written `msg.println()` with `io` hidden.

```ts
const n = "hello".len()        // desugars to len("hello")
```

Signatures may default parameters — literals only, the omitted argument staged at the call site whatever the callee class (free fn, inherent member, or trait dispatch). Call sites may name arguments after positional ones: an enum case constructor fills its payload fields by name, while the function-call spellings parse the form and refuse at lowering. A last-argument lambda may be written after the call as a brace block — `twice { k + 11 }` stages exactly like `twice(() => k + 11)` over every callee class that takes one (named fn, dot-call, indirect call).

```ts
fn connect(host: string, port: int = 8080) -> string { "${host}:${port}" }
const c = connect("db.local", 8081)
const d = connect("db.local")     // the omitted default stages at the call site

fn twice(f: fn() -> int) -> int { f() + f() }
const k = 10
const t = twice { k + 11 }        // trailing block: the written trailing lambda, staged the same
```

## Control flow

Every block opens with a visible token; statements are newline-terminated with no semicolons; trailing commas are allowed everywhere; `if` and `match` are expressions.

```ts
const m = if a >= b { a } else { b }
```

`match` is Rust-shaped: arms `pat => expr` separated by commas or newlines, guards `pat if cond`, or-patterns `a | b`, destructuring bindings, and the `_` wildcard; a struct pattern tests its literal fields in place and binds the rest (`point { x: 1, y }`), an enum case arm tests and binds its payload the same way (`scored(1)`, `scored(n)`), and a failed payload test or guard slides to the next arm, so every can-fail member arm needs a later settling arm or a catchall; nested struct patterns test and bind fields the same way. Exhaustiveness is checked by tag identity over the set algebra, and the diagnostic names the missing members as a set computation ("missing: timeout | refused"). A tuple scrutinee carrying a union element refuses at lowering today — narrow the element into a local first.

```ts
fn parse_age(s: string) -> int | parse_error {
  match s.trim().parse_int() {
    parse_err => bad_number(s),
    n         => n,
  }
}

type fetch_err = timeout | refused | parse_error
fn retryable(e: fetch_err) -> bool {
  match e {
    timeout | refused => true,    // or-pattern over the transient members
    _                 => false,
  }
}

fn report(total: int, failed: int) -> string {
  match (total, failed) {
    (_, f) if f > 0 => "warning: ${f} row(s) failed",   // guard over a tuple pattern
    (0, _)          => "no valid users",
    (t, _)          => "total: ${t}",
  }
}
```

Loops are `for x in xs`, ranges `a..b` (exclusive) and `a..=b` (inclusive) — the `in` operator takes a bare range as its right side, and collection membership has no operator form (`xs.contains(x)`) — plus `while cond`, with `break` and `continue`. Boolean contexts (conditions, `&&`/`||`/`!` operands) are bool-typed.

```ts
fn drill() {
  for i in 0..10 { println("${i}") }
  var n = 0
  while n < 3 { n = n + 1 }
}

drill()
```

The explicit early-exit spelling is `if x is none { return missing }`, with refinement applying to everything after it. `return` is a statement, never an arm's answer: a value-consumed position — an arm inside an `if`/`match` used as an expression, a bare block initializer, an argument slot — refuses it (DS2039), because the diverged path has no answer for the consuming slot; spell the exit as its own statement line (an arm of a statement `if`/`match` may exit beside its value-bearing sibling) and let the code below it read refined.

Operator precedence is one frozen C-family table, published here once and never extended, with no user-defined operators: postfix calls and subscripts, unary `!` `-`, `*` `/` `%`, `+` `-`, shifts, `&` `^` `|`, comparisons and `in` (`x in a..b && flag` reads `(x in a..b) && flag`), `&&`, `||`, tightest first; the word-form operators are exactly `in` and `is`.

## Errors and extraction

Expected failures are union members of return types — storable data, one predicted branch in the VM, no exception machinery. `?` propagates: members outside the ok-type return early, with checked containment — the enclosing return union must contain the propagated member, and no implicit conversions exist.

```ts
fn parse_config(text: string) -> config | parse_error { config { host: text, port: none, retries: 3 } }
fn load_config(path: string) -> config | io_err | parse_error {
  const text = read_file(path)?          // non-members of config return early
  parse_config(text)?
}

write_file("app.cfg", "localhost")
```

`.err(h)` and `.ok(h)` are set-difference-typed handlers: the result type is the incoming union minus what the handler covers, so the type shows what is left.

```ts
const cfg = load_config("app.cfg").err((e) => config { host: "localhost", port: none, retries: 3 })
// result type: (config | io_err | parse_error) - (io_err | parse_error) == config
const port = cfg.port.unwrap_or(8080)                       // int? -> int, eager fallback
const p2 = cfg.port.unwrap_or_else(() => 8081)              // lazy variant
fn require_name(m: map[string, string]) -> string | parse_error {
  map_get(m, "name").ok_or(missing_field("name"))?          // early exit with a new value
}
println("${cfg.host} ${port} ${p2} ${require_name(map[string, string]()).ok((n: string) => n).err((e) => "?")}")
```

The @std extraction family — `unwrap_or`, `unwrap_or_else`, `ok_or`, `map_err` — plus `.err`/`.ok` are UFCS-callable generic functions with fully local typing. Bugs panic unhandleably: the fiber aborts, linear deinits run during the frame walk, and the trap surfaces at the nursery or supervisor boundary; let-it-crash supervision is a library pattern. Deliberate abort is `die(msg)`, a host provision rather than a language primitive: the `ds` CLI installs the spelling for its entry sessions (`run`, `dev`, `repl`) and prints `die: <msg>` exiting failed, while a compile without the provision (tests, embedders, `--no-ambient`) keeps the name unknown; a block-tail `die` against an annotated return refuses (the never-type machinery is unset), so spell it as its own statement.

## Linearity

Every value is affine: moving it into a new binding, a consume-mode call, or a return kills the old binding, and use-after-move is a compile error rendered as a two-span linear diagnostic ("value `h` was consumed here / used again here"). Checking is one intraprocedural forward pass over last uses — no lifetime annotations, no mode syntax at call sites. Copyable values satisfy the rule by copying invisibly, so everyday code never notices it.

`~Copyable` is a negated set constraint — it subtracts the default copyability constraint and composes with unions — marking real resources such as file descriptors, locks, and foreign memory. Such values cannot be copied and cannot be lost: `deinit` runs exactly once at the unique instance's death, on every path including early return, panic, and cancellation teardown. The ambient's own resources carry the pattern: `open_file` answers the deinit-carrying `file_handle` whose fallible reads (`read_raw`, `read_exact`) answer `bytes | io_err`, and `net_connect` answers the same-shaped `net_handle` served by `send`/`recv`/`close_conn`, the fallible reads answering the net error union (`string | net_err` for `recv`, `() | net_err` for `send`) over the taxonomy's two cases; the open and the connect keep the total spelling because a deinit-carrying value refuses as a union member, and each deinit closes what an explicit close already closed, idempotently. Phantom-state unions (`file[open] | file[closed]` style) typestate protocols on the same marker; explicit `drop` is a debug and FFI tool only, since users never write routine drops.

Mutation has one channel — a named call on an owned receiver with a `mut` mode — always lexically visible at the call. The three parameter modes are fixed at the definition site: default borrow (immutable), `mut` (an exclusive, re-storable window: the callee may re-seat its parameter and the caller's binding never observes the re-seat), and `move` (a consuming hand-off: the deinit claim rides the move, so the callee's scope end fires the hook once). Call sites carry zero mode syntax: `xs.list_push(1)` reads identically whatever the mode. Inside a window are zero runtime checks, alias-freeness being proven; outside it everything is shared-immutable, so shared mutation is unrepresentable by construction.

```ts
type raw_fd = int
struct file: ~Copyable {
  fd: raw_fd
  deinit { println("closing ${self.fd}") }     // exactly once, on every path
}

fn size(s: string) -> int { s.len() }          // borrow: immutable for the call
fn read_all(f: mut file) -> string { "" }      // exclusive mutable window
fn close(move h: file) -> int { h.fd }         // consume: dies and deinits here

println("${size("x")} ${close(file { fd: 3 })}")
```

## Collections and iteration

Collections are linear-mutable copy-on-write: mutating calls take an exclusive window over the receiver and reuse it in place if and only if it is uniquely owned, so in-place reuse is guaranteed and observable. Exclusivity is by spelling: a mutating call's receiver is a `var` binding or a fresh value, a `const` receiver is a compile error, and no hidden copy hides behind an aliasing call. Functional updates (`map`, `filter`, `reduce` and friends) copy only when their source is aliased, surfaced as a perf note, never an error. Persistent structures are ordinary library types on the same runtime; `bytes` and `vec` are the unboxed unique members of the same model (FFI-grade).

A record element is one unit on the same surface: the checked `list_get` read answers `t | none` whole, fields project out of the extraction, and the copy closes move whole spans.

```ts
var club = list[user]()
club.list_push(user { name: "ada", age: 45 })                  // the whole span lands in place
club.list_push(user { name: "bob", age: 15 })
const cell = list_get(club, 0)                                 // user | none, the checked read
const who = cell.ok((u: user) => u.name).err((_: none) => "?")  // fields project out
const grown = cons(user { name: "eve", age: 30 }, tail(club))   // cons and tail move whole spans
const roster = collect(grown)                                   // the whole-list copy
for u in roster { println("user ${u.name}") }
```

Iteration is combinators — `map`, `filter`, `reduce`, and friends — plus `for`, all over one hidden linear cursor that linearity lets the compiler strip bounds checks from; chains fuse into loops, and non-fusable shapes fall back to the same cursor semantics. Subscripts are single-element (`xs[0]`) with no slicing sugar; index typing waits on the stdlib collection types, and string slices are library calls — the checked `substring(s, 0, 2)` answers `string | none` on char boundaries today, an out-of-range slice answering none, and the indexed read `char_at(s, i)` answers `char | none` under the same convention; `split(s, c)`/`join(xs, c)` take exactly one `char` separator, answering a `list[string]` and a `string`.

```ts
// rows: list[entry]
var users = list[user]()
var errors = list[parse_error]()
for r in rows.map(parse_user) {
  if r is parse_error { errors.list_push(r) } else { users.list_push(r) }
}
const total = users
  .filter((u) => u.age >= 18)
  .map((u) => u.age * 2)
  .reduce(0, (a, b) => a + b)
```

Stage note: `list[record]` runs end to end — `list_push` stages each element's whole word span, the checked `list_get`/`first` read answers whole, for-in and `in` bind whole elements per step, the closes (`cons`, `tail`, `collect`, `list_concat`) move whole spans, and `map`/`filter`/`partition`/`reduce` run over struct elements and answers (`partition` through the native channel's tag-less multi-word total, a struct-folding `reduce` staging the init's span between the list and the callable). The `rows.map(parse_user)` half waits: union elements refuse at every read. A `list[(int, string)]` element stages its positional words per element, so the checked reads, for-in, `in`, the closes, and the combinators (a tuple-accumulator `reduce` included) run over tuple elements, and a tuple member of a union destructures positionally inside a member arm's body (a tuple arm resolves no member). Maps have no iteration — build a list, or use the checked `map_get` read; `m.len()` counts the pairs through the free generic's Map instance. `bytes` runs end to end — one immutable byte string per value behind one reference word: `bytes_of` constructs from `list[byte]` and `byte_list` bridges back, `len` counts the payload, `==`/`!=` answers payload content equality over bytes operands, the checked `byte_at` answers `byte | none`, and the file face's `read_exact` answers `bytes | io_err`. The representation carries the derived faces too: `show` renders the payload as a byte list (`bytes[1, 2, 255]`, empty printing `bytes[]`), `ord` orders and `hash` hashes over the payload lexicographically (a shorter prefix first), so `derive(show/ord/hash)` composes over a bytes part, and the bridges cross to text — `utf8_encode` takes the string's own bytes, `utf8_decode` validates on the way back answering `string | utf8_err`.

## Traits and generics

A trait is a set-theoretic interface; an instance is declared for a type head; receivers are spelled `self` (borrow) or `mut self` (exclusive window), typed `Self` and exempt from annotation.

```ts
trait renderable {
  fn render(self) -> string
}
impl renderable for int {
  fn render(self) -> string { str(self) }
}
impl[t: renderable] renderable for list[t] {
  fn render(self) -> string { "[" + self.map((x) => x.render()).join(',') + "]" }
}

println([1, 2, 3].render())
```

A member default lives in the declaration that owns the body: the trait declaration is the single authority for a trait member's default — every impl omits it, and both call spellings stage the declared literal when the argument is absent — while an inherent impl member keeps its default in its own body.

```ts
trait sized {
  fn pad(self, by: int = 1) -> int            // the declaration owns the default
}
impl sized for int {
  fn pad(self, by: int) -> int { self + by }  // impls omit it
}
struct counter { ticks: int }
impl counter {
  fn bump(self, by: int = 3) -> int { self.ticks + by }  // an inherent member keeps its own
}
const a = 3.pad()                      // 4: the declared literal stages when omitted
const b = sized::pad(3, 10)            // 13: a written argument, either spelling
const c = counter { ticks: 1 }.bump()  // 4
```

An impl member may also omit the receiver: `impl gauge { fn make(level: int) -> gauge }` is called through its type name (`gauge.make(4)`) or an instantiated application (`list[int].make()`, the applied type solving as the call's `Self` and binding a generic impl's head variables so the member's signature reads through them; a bare type name spells no type arguments, so it cannot reach a generic impl's members), type-keyed and static — no instance word, a bounded impl's evidence staging at the call like the value-receiver spelling — and the `default` derive answers through the same surface (`fn default() -> Self`, so `point.default()` and the primitive spelling `int.default()` run); trait members keep the receiver, dispatch being receiver-carried.

The only coherence rule is pairwise disjointness of one trait's instance heads — for every pair, prove `H1 & H2 = empty` — checked at each declaration, again over the package's closed instance set, and again at link time, so an impl landing after the generics it intersects cannot hide an overlap; generic heads count through their instantiations, so `impl shows for list[int]` and `impl [t: shows] shows for list[t]` overlap at `list[int]`. Any module may implement for any type, foreign included: no orphan rule, no newtype wrappers, no priority machinery; an overlap error names both heads and a witness value type, union heads decompose into per-branch instances, and a `& ~trait` bound in a generic clause carves exclusivity when overlap is intended. Heads never overlap, so a tag-keyed inline cache never needs tie-breaking.

Generics carry one clause — `[t: c]` on functions, `[t]` on types — and every generic function exists exactly once in bytecode: call sites pass evidence words (statically resolved instances, or runtime slots behind one-to-two-entry inline caches with no correctness role), and the JIT may specialize per type lazily. Dispatch is determined by types and the set algebra, never by declaration order or priority; no HKT and no GATs in 1.0.

```ts
struct stack[t] { items: list[t] }
fn push[t: ~Copyable](s: stack[t], x: t) -> stack[t] { stack { items: cons(x, s.items) } }
```

Stage note: the traits above run — trait sites dispatch over impl member bodies, a generic impl lowers once with the entering call site staging the evidence words, nested trait calls inside a generic body ride that evidence, and a nested generic dispatch transfers transitively through the module's evidence-chain table, one table read per hop (no frame bound on chain depth — the checker's instance solving is the practical limit). The runtime-evidence seam stays one-word: a substituted type spanning more than one word refuses at the entering site, since the one body carries the variable as a single Raw word end to end and per-instantiation frames are the JIT specialization tier's, never the interpreter's. Free generic fn calls run the same way: one body per fn, the call site staging the bound's serving words after the arguments (a generic fn used as a value and a generic lambda value still refuse their callable words — the word carries no instantiation), the running bodies keeping to trait dispatch on the variable and the builtin `list`/`map` applications — a user-declared generic application's values refuse at lowering, and a builtin generic native (`cons`, `first`) refuses a bare-variable argument, no instance covering it.

Operators are fixed traits — `+` dispatches to `add`, indexing to `index`, over a closed token set under the frozen precedence table above; `==` is never overloadable — but the operator traits are surface only (user `+` refuses at the checker today), and ordering runs through the landed `ord` derive. Derives are attributes covering the mechanical cases: `hash`, `show`, `ord` (field-order lexicographic), `clone`, `default`, `from_json`, and `to_json` run today — `==` never derives; an enum's own `default()` answers its first case (a unit case, or the one `#[default]` annotates: the annotation on one enum case names the member the family synthesizes the union's default over, so the union's `default()` answers the annotated case's construction and a defaulting struct field typing the union answers the same construction, while an un-annotated union-typed field refuses as ambiguous); and the json families ride the ambient's json face (the paragraph below). Comptime is the language itself: a free fn whose parameters are all `comptime` — `comptime t: type` over the builtin `type` atom, or a scalar annotation taking a plain literal — has its calls evaluated at compile time on the same VM in a world-fenced context (a world-touching call refuses), and it may call an ordinary helper whose solved effect row is pure or trap alone — the helper's body joins the fenced evaluation, any other row refusing at the fence naming the call — the answer, a scalar or a list of them, embedding at the call site as the literal a user would have written; no macros of any level exist, so hygiene is deleted structurally. Reflection is comptime-only: `fields(t)` walks a struct's declared fields or an enum's declared cases as (name, type, tag) records — a case's `type` is its payload atom, `none` when it constructs bare, its `tag` its constructor name — and `type_name` spells a type as a string; runtime type questions are answered by the tags data already carries (`is`, match).

```ts
#[derive(hash, show, ord)]
struct point { x: int, y: int }

println(point { x: 1, y: 2 }.show())
```

```ts
struct page { title: string, views: int }
enum tone { quiet, loud { volume: int } }

fn schema(comptime t: type) -> string {
  var s = "{"
  for f in fields(t) { s += f.name + ": " + type_name(f.type) + ", " }
  s + "}"
}

fn cases(comptime t: type) -> string {
  var s = "{"
  for c in fields(t) { s += c.name + ": " + type_name(c.type) + "/" + c.tag + ", " }
  s + "}"
}

println(schema(page))  // {title: string, views: int, }: a constant before anything runs
println(cases(tone))   // {quiet: none/quiet, loud: loud/loud, }: payload atom or none, tag the constructor
```

JSON is the ambient's `json` face: the predeclared enum's cases cover the grammar's shapes (`json_string { string }`, `json_object { map[string, json] }` among them), `parse_json: (string) -> json | parse_err` reads one document, the plain `to_json: (json) -> string` writes the minified form back, and the typed readers walk a document whose shape is still open — `json_member(v, "port")` answers `json | none`, and each `json_as_*` reader (`json_as_int`, `json_as_string`, ...) answers its scalar or `parse_err` on a wrong shape. The scalar widths ride their ruled representations: `byte` (0-255) and `u32` as json numbers, `char` as a one-character json string, `bytes` as a json array of numbers — one element per payload byte — so an out-of-range number or wrong-shaped value answers `parse_err` like any other. When the shape is a declared type, the derive families close it: `#[derive(from_json)]` synthesizes the receiverless member `from_json(v: json) -> Self | parse_err` — the struct walk reads one member per field, a required member's absence answering `parse_err`, an optional field reading absence as `none`, a list field walking the array, a field whose own type derives `from_json` composing, the width fields reading through their readers above — and `#[derive(to_json)]` is the mirror, synthesizing `to_json(v: Self) -> json` that builds the value through the enum's own cases, an optional field's presence setting its member and its absence leaving the member out; `t.to_json(v).to_json()` answers the string. An enum target reads its unit cases from their own names and its one struct-payload case from its members. Beside the shared refusal classes (a tag, a tuple field, generic declarations, `~Copyable` parts), the encode side refuses an optional nested inside a part.

```ts
#[derive(from_json, to_json)]
struct user { name: string, age: int, email: string? }

fn roundtrip(text: string) -> string {
  const j = parse_json(text)
  if j is parse_err {
    "not a document"
  } else {
    const u = user.from_json(j)
    if u is parse_err { "not a user" } else { user.to_json(u).to_json() }
  }
}

println(roundtrip("{\"name\": \"ada\", \"age\": 45}"))
// {"name":"ada","age":45}: the absent optional stays absent, the write minified
```

## Modules and script mode

Four layers: definitions (the identity and visibility unit) live in files (a namespace spelling); files in one package share one namespace — zero intra-package imports, and intra-package cycles are legal — and the package is the compilation unit; the module is the `ds.pkg` manifest, the dependency and distribution unit whose hash cascade makes the manifest the lock (no lockfile exists). Cross-package cycles are a link error.

Module bodies are pure compile-time declarations — no import-time execution, period — which buys intra-package cycle tolerance, safe hot reload, and lazy loading. Script exception: in script mode only, the entry file's top-level statements are legal and compile into a synthesized `main`; imported files and package members stay pure.

A line-initial `##` at top level separates blocks (the rest of the line is free separator text); definitions are order-independent with or without separators, and a file without separators is one block. A `///` line is a doc comment attaching to the next definition — a member or field included, printed exactly where it stood — while `//` is the ordinary comment.

```ts
## state

tag disk_full

## io helpers

/// reads the whole text
fn read_all(f: string) -> string { f.to_upper() }

println(read_all("ready"))
```

Visibility is private by default; `pub(pkg)` opens package-wide (tests included) and never enters the interface; `pub` opens to dependents and enters the generated `.dsi` — `ds info` writes it, it is the reviewable API surface, and if nothing in `.dsi` changes it is typically a safe refactoring.

Imports are symbol-selective, aliasable, and conditional per target, unified on the sigil form (a `pub import` re-export refuses today, waiting on the transitive surface closure):

```ts
import @std/csv (reader)
import acme/pipeline/schema (entry, parse_row)
import acme/pipeline/io as pio
import acme/pipeline/fs if os == "windows"
```

Script mode is zero-config: `ds run script.ds` compiles and runs the entry file, and a `ds.pkg` beside the entry file turns the run into a package run — the manifest resolves over the content-addressed store (`$DS_CACHE/pkg`), `$DS_REGISTRY` names a registry — a local index root or a git remote URL (`https://...`, `file://...` for local testing) — whose verified content-addressed fetch fills the store's misses, and `ds save <pkg-root>` installs a working-tree package (the `ds.pkg` plus every `.ds` below the root) into the store.

```ts
import @std/csv

fn greet(row: entry) -> string {
  match row.get("name") {
    none => "hello, stranger",
    nm   => "hello, ${nm}",
  }
}

const rows = reader("users.csv").err((e) => [])
for row in rows {
  println(greet(row))
}
```

Stage note: script mode's run half is live — `ds run script.ds` folds an entry file's top-level statements into a synthesized `main`, `ds build` records the declared imports into the artifact's link table (an `@std/...` path linking its package stem, so the deps-first load walk resolves them), and an `@std` import's selections validate against the exported surface (a `pub(pkg)`, private, or missing selection refusing) — while a linked import binds callables, plain-literal consts (the value crossing as its own fact), flat struct types (scalar and list-of-scalar fields), and non-generic enums (case names plus payload shape over the same word vocabulary) over which the consumer constructs, reads fields, matches, `is`-narrows, and binds signatures — a linked enum's tag rows seeding the consumer's table, so union-typed values cross both directions over agreed ids and a linked signature unioning over its own type runs — a computed or mutable const, a deeper foreign type, a trait member, an impl body, or a generic or defaulted function refusing as the unbindable shape (DS4021) — and `@std` is itself a package in the store: a manifest declaring the dep binds its imports through this same linked surface, while a graph that does not keeps the merged-ambient reading — so the csv script above parses with its `reader` use unresolved, the merged `@std` ambient exporting no csv; `ds info` renders a file's or package's `.dsi`, and `ds bind` waits with the loading round.

`ds repl` is the interactive half of script mode: one persistent session where declarations append as their own blocks and statements fold into a REPL-owned `main`, each submission printing the value, its inferred type, and its effect row; the frame parks between submissions and resumes at the new statement, so prior statements run once and a trapping line rolls back without committing — its own deinit-carrying bindings firing their hooks once on the drain and the line's abandoned nursery torn down the same way, never-entered children released cold and the slot recycled; `:drop <name>` fires one live binding's deinit exactly once, `:reset` clears the session, `:quit` leaves.

`ds dev file.ds --hot` is the running-program half: the dev host injects the suspension itself — a loop-driving entry parks at its back edge every watch round, so no language spelling exists — and every saved edit recompiles, swaps the module in place, and the parked frame's next call runs the new code; a broken edit keeps the old version running until you fix it. A `ds.pkg` beside the file turns the watch over the package root it names, and `--args` stages the entry's parameters the way `ds run` does, re-derived on every reload.

World-touching APIs — console, files, net, time — follow the capability idiom: the `ds` CLI installs an ambient platform handler for scripts behind the source's declaration (`std` the full face; `embed` console-only, its print face running while every world op beyond it refuses at install as an unprovided op), so `println` above just runs, while tests and embedders pass the capability explicitly (`println(io, msg)`) and swap schedulers with no code change; v1 has no effect syntax. Foreign code enters as opaque linear C handles behind `extern fn` (double-free is a compile error), with `ds bind header.h` generating typed externs.

## Concurrency

Concurrency is stackful VM fibers in mandatory nurseries: a fiber stack is interpreter data, spawn is one allocation, a switch is a pointer swap, and `spawn` exists only inside a `nursery` block. Scope exit joins children, first failure cancels siblings, and child errors union out at the boundary — a payloaded member rides its whole value out (struct fields, a captured list) and extracts like any union value; code is blocking-style with no colors and no `await`, tail calls stay guaranteed per fiber, and bare detach requires an explicit root nursery or supervisor.

```ts
fn fetch_all(urls: list[string]) -> list[page] | net_err {
  var pages = list[page]()
  nursery {
    for url in urls {
      spawn {
        match fetch_body(url) {
          timeout => cancel(timeout),
          refused => cancel(refused),
          p       => pages.list_push(p),
        }
      }
    }
  }
  pages
}
```

Parallelism is fork-join over the sendability substrate: a linear value is sendable by construction and an immutable value shareable, so race-freedom is static and the missing piece was only a scheduler — `par_map` and `par_reduce` over immutable or linear-partitioned inputs are deterministic, replayable, and lock-free by theorem.

```ts
const total = pages.par_map((p) => p.body.len()).par_reduce(0, (a, b) => a + b)
```

Stage note: `par_map` and `par_reduce` run today as the deterministic single-thread slices — `par_map` one spawned child per element with the answers assembling in input order (the callable's answer riding the child's completion whole, one word or a whole span alike, the callable a plain fn or a capturing lambda) and the first failure canceling the siblings, `par_reduce` the fold chaining one spawned child per element (the running accumulator's whole word span and the element, each joined before the next spawns, so any fold width chains through the seam) so the reduction applies strictly in input order and the first failure re-raises at the call — while actual parallel scheduling and nesting wait on the scheduler round.

Cancellation is nursery-scoped and cooperative — an explicit cancel terminates a yielding child at its next resume, so a linear consume is never interrupted mid-flight; teardown walks the kept frames and each deinit fires exactly once at the drain, with `defer` (scope-exit sugar, non-normative; it parses with no lowering yet) in teardown. Timeouts, `firstof`, and interleave are library combinators over the timer capability; supervision is a library pattern — a nursery with a restart policy.

## The runtime

Execution is three tiers over one shared lowering — a typed register VM with unboxed slots (integers and floats raw in frame slots; boxing is the scalar overhead static typing exists to delete), a copy-and-patch JIT whose stencils compile in microseconds, and Cranelift/wasm on demand — and short scripts never pay for tiering; the register VM is the running tier today, the JIT tiers designed but not started. Tail calls are a spec-level guarantee, self and mutual, in every tier, deterministically: the stack never grows at any call depth. Memory is precise reference counting with block reuse plus per-frame bump arenas — zero pauses, zero write barriers, no GC — and static typing deletes hidden classes, inline-cache guards, and deoptimization frames from the data path; JIT exits are breakpoints, the stack-limit check, effect-handler entry, and the cold overflow edge.

## What is deliberately absent

Every item below is a refusal with a recorded reason.

- **Truthiness** — boolean contexts are bool-typed; unions, options, numbers, and strings are never coerced.
- **The `or` operator** — retired: it overloaded one word across fallback, early exit, loop control, and union coalescing, typed by contextual inference, colliding with `||`; the extraction family replaces its behaviors on existing tokens.
- **Chained comparisons** — `a < b < c` is not an expression.
- **Comprehensions** — the combinator surface over one hidden cursor covers them.
- **Inheritance** — structs compose and traits carry behavior, so data, behavior, and identity never entangle.
- **Default parameters that evaluate expressions** — defaults are literal-only, so a default can never capture mutable state or run hidden code at a call site.
- **`any`** — inference failure is a compile error, never dynamic typing.
- **Macros** — comptime evaluation plus static reflection is the language itself; no substitution exists, so hygiene is deleted structurally.

Also permanently excluded by the same audits: user-defined operators, `Deref`-style coercions, subscript slicing sugar, vararg/spread packing, and word-form operators beyond `in` and `is`.

Deferred, recorded, not promised — additive valves that enter only after the core is stable and must re-fit this file: `|>`, anonymous record literals (the map literal rides this reservation; builder calls interim), `@` binders, `??` on `t | none`, `?.` desugar, `throws(...)` sugar, user-defined handlers, scoped named instances, labeled break/continue, and `it` shorthand.

That is the whole language. The explainability budget is the contract: any proposal that cannot re-fit this tutorial does not belong in DreamScript.
