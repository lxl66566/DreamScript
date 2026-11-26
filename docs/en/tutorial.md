# DreamScript in one short tutorial

DreamScript is a general-purpose interpreted language with static set-theoretic types, affine-by-default values, guaranteed tail calls, reference-counted memory with no GC, and optional JIT tiers. This file is the whole v1 surface, the explainability budget in one tutorial.

Run examples with `ds run file.ds`; top-level statements are legal only in a script's entry file (see Modules and script mode).

## Values and bindings

A binding is `const` (the default, immutable) or `var` (a rebindable local); rebinding swaps the value the name holds, and in-place change is the separate mutation channel (see Linearity).

```ts
const name = "dreamscript"   // immutable, type inferred as string
fn count_up() -> int {
  var count = 0              // a rebindable local
  count = count + 1          // rebinding, not mutation
  count
}
```

`int` is 64-bit signed and `float` 64-bit IEEE, both raw in slots; literals take separators and radix prefixes (`1_000_000`, `0xff`, `0b1010`, `0o777`), and a width suffix (`42u32`) names a declared type, so an undeclared width refuses and a suffixed literal itself refuses at lowering. Overflow and division by zero trap (`1 / 0` panics), while floats never trap (`1.0 / 0.0` is `+inf`). Wrapping is explicit (`+%`, `*%`, `-%a`); narrowing is a width-type call (`i32(u)`, `i64(f)`) with `u32.of(x)` the trapping conversion and `u32_of` the checked `u32 | none` (bare `u32(x)` refuses under the F9 namespace priority). `<<`, `>>` shift arithmetically, `&`, `^`, `|` are total, and a shift amount at or past 64 or below zero traps.

Strings are immutable UTF-8 with escapes `\n \t \r \\ \" \' \$ \0 \u{...}`; raw `r"..."` and nestable `r#"..."#` take no escapes and cover multiline needs. `'x'` is a Unicode scalar char and `b'x'` a byte (`\u{..}` spelling 0-255, while a raw non-ASCII byte char refuses); `code_of(c)` reads the char's scalar, and checked `char_of(code)` answers `string | none` (one scalar, a codepoint's UTF-8 bytes not being expressible in ds source). Interpolation is string-only: `"$x"` and `"${e}"` desugar to `str(e)` for a printable primitive (`int`, `float`, `bool`, `char`, `byte`, `string`) or a `show` carrier staged at its own span, a union receiver through its covering impls; a union or optional operand without covering `show` is an error.

```ts
const who = "ada"
const tries = 3
const msg = "hi $who, next attempt ${tries + 1}"   // both hole spellings
const path = r"C:\root\tmp"                        // raw: no escapes
const first = 'x'                                  // char (b'x' is a byte)
```

Equality is one structural `==` over all data, never overloadable; no `===` exists. Floats compare bit-structurally (`nan == nan` is `true`), function comparison is rejected statically, and mutable cells and `~Copyable` resources compare by identity.

## Types are sets

Declarations are nominal: every `tag`, `struct`, and `enum` has identity. Types are sets — unions `a | b`, intersections `a & b`, differences `a - b` over nominal atoms — where an intersection keeps the most precise member's layout and a difference erases entirely. Aliases never declare cases; payload cases live only in enum blocks.

```ts
struct bad_port { got: int }
struct page { url: string, body: string }
struct user { name: string, age: int }
enum parse_error { bad_number(string), missing_field(string) }
type entry = map[string, string]
```

The ambient predeclares the fallible-read taxonomy (`io_err` and `net_err` as branchable class-tag aliases, `parse_err` carrying its position payload, `utf8_err`) beside `none`, the `json` family, and the `config` record the extraction example uses; redeclaring one is a duplicate-definition error.

Construction is fixed per declaration: structs brace form only, enum cases call form only, unit tags bare, cross-spellings checker errors, and struct braces support punning.

```ts
timeout                                    // unit tag: bare
bad_number("nan")                          // enum case: call form only
page { url: "https://ds.dev", body: "" }   // struct: brace form only
const url = "https://ds.dev"
const home = page { url, body: "" }        // punning: the field fills from the local
```

`T?` is exactly `T | none`, legal in every annotation position; `none` is an ordinary unit tag, so optionals, error unions, and enums share one mechanism (narrowing, matching, one extraction family), and no `?.` exists.

Narrowing is `is`, Kotlin-shaped and never identity comparison, its right side a type expression, so `|` and `&` are legal there; refinement narrows monotonically through `is` tests, none tests, and match arms, resetting at assignment.

```ts
tag doc_null
struct doc_num { value: int }
struct doc_obj { fields: map[string, doc] }
type doc = doc_null | doc_num | doc_obj

fn port_of(j: doc) -> int | none | bad_port {
  if j is doc_obj {
    const pv = j.fields.get("port")            // inferred: doc | none
    if pv is doc_num && pv.value >= 1 && pv.value <= 65535 { pv.value }
    else { bad_port { got: -1 } }
  } else {
    none
  }
}
```

Constructors (brace forms, bare tags, generic applications like `list[page]()`) live in the type namespace, functions in the value namespace: a struct `map` and a function `map` coexist, and a call-form name in both is an ambiguity error.

Lists and tuples: `[1, 2, 3]` is a `list[int]` literal and `list[page]()` the empty mutable collection; map literals are deferred, maps building by calls, and union values ride the map value channel (the ambient's `json_object` is exactly one). Maps carry no iteration of their own; `m.keys()` answers the entry-order key list. Ad-hoc aggregates are anonymous tuples (`("host", 8080)`), destructuring in bindings and patterns.

## Functions

`fn` is the only named-function declaration, at top level and as trait/impl members, the name in scope inside its own body, so recursion is direct. Parameters are `name: t`, returns `-> t`, and union returns parse as `-> a | b`. The body's final expression is the return value, `return e` exits early, `()` is the unit type, and a function returning nothing may omit the annotation. Signatures are required on top-level and public definitions; locals, lambdas, and match arms are inferred. Anonymous functions are const-arrow lambdas, the only other function spelling, named by a binding inside any block; top level names functions with `fn` only.

Functions are values end to end: a named fn binds and passes like any value, `fn(t) -> r` names a fn type in either position, and any callable (named fn, lambda, capturing closure) inhabits the slot with one call spelling. A fn-typed return builds factories: `(k: int) => (x: int) => x + k`.

```ts
fn apply(f: fn(int) -> int, v: int) -> int { f(v) }
fn inc(x: int) -> int { x + 1 }

const k = 10
const g = inc                            // a named fn binds like any value
const double = (x: int) => x * 2         // a lambda named by a block binding
const a = apply(g, 4)
const b = apply(double, 4)
const c = apply((x: int) => x * k, 4)    // a capturing closure fills the same slot
const raised = [1, 2].map(inc)           // one combinator parameter, every callable
```

UFCS is universal: `x.f(a)` desugars to `f(x, a)` for any first-argument function — free functions first, then trait methods, `trait::f(x)` the escape hatch, true ambiguity a compile error. A free function whose first parameter cannot take the receiver's type never takes the call, so a same-named free function and a member coexist over disjoint receivers. Capability parameters are named and excluded from the dot position: `println(io, msg)` is never `msg.println()`.

```ts
const n = "hello".len()        // desugars to len("hello")
```

Signatures may default parameters, literals only, the omitted argument staged at the call site whatever the callee class. Call sites may name arguments after positional ones: an enum case constructor fills its payload fields by name, while the function-call spellings parse the form and refuse at lowering. A last-argument lambda may be written after the call as a brace block, so `twice { k + 11 }` stages like `twice(() => k + 11)`.

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

`match` is Rust-shaped: arms `pat => expr` separated by commas or newlines, guards `pat if cond`, or-patterns `a | b`, destructuring bindings, and `_`. A struct pattern tests its literal fields in place and binds the rest (`point { x: 1, y }`), an enum case arm tests and binds its payload the same way, and a failed test or guard slides to the next arm, so a can-fail arm needs a settling arm or a catchall. Exhaustiveness is checked by tag identity over the set algebra ("missing: timeout | refused"); a tuple scrutinee carrying a union element refuses at lowering.

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

Loops are `for x in xs`, ranges `a..b` (exclusive) and `a..=b` (inclusive), and `while cond`, with `break` and `continue`. `in` takes a bare range on its right and serves list membership (`x in xs`, checked against the element type; map membership refuses, so maps read through the checked `get`). A `for` over a list freezes the element count at loop entry.

```ts
fn drill() {
  for i in 0..10 { println("${i}") }
  var n = 0
  while n < 3 { n = n + 1 }
}

drill()
```

The early-exit spelling is `if x is none { return missing }`, with refinement applying after it. `return` is a statement, never an arm's answer: a value-consumed position (an `if`/`match` arm used as an expression, a bare block initializer, an argument slot) refuses it (DS2039).

Precedence is one frozen C-family table, never extended, with no user-defined operators: postfix calls and subscripts, unary `!` `-`, `*` `/` `%`, `+` `-`, shifts, `&` `^` `|`, comparisons and `in`, `&&`, `||`, tightest first, so `x in a..b && flag` reads `(x in a..b) && flag`. The word-form operators are exactly `in` and `is`.

## Errors and extraction

Expected failures are union members of return types: storable data, one predicted branch in the VM, no exception machinery. `?` propagates, returning early on members outside the ok-type with checked containment (the enclosing return union must contain the propagated member).

```ts
fn parse_config(text: string) -> config | parse_error { config { host: text, port: none, retries: 3 } }
fn load_config(path: string) -> config | io_err | parse_error {
  const text = read_file(path)?          // non-members of config return early
  parse_config(text)?
}

write_file("app.cfg", "localhost")
```

`.err(h)` and `.ok(h)` are set-difference-typed handlers: the result type is the incoming union minus what the handler covers.

```ts
const cfg = load_config("app.cfg").err((e) => config { host: "localhost", port: none, retries: 3 })
// result type: (config | io_err | parse_error) - (io_err | parse_error) == config
const port = cfg.port.unwrap_or(8080)                       // int? -> int, eager fallback
const p2 = cfg.port.unwrap_or_else(() => 8081)              // lazy variant
fn require_name(m: map[string, string]) -> string | parse_error {
  m.get("name").ok_or(missing_field("name"))?               // early exit with a new value
}
println("${cfg.host} ${port} ${p2} ${require_name(map[string, string]()).err((e) => "?")}")
```

The @std extraction family (`unwrap_or`, `unwrap_or_else`, `ok_or`, `map_err`) plus `.err`/`.ok` are UFCS-callable generic functions with fully local typing. Bugs panic unhandleably: the fiber aborts, linear deinits run during the frame walk, and the trap surfaces at the nursery or supervisor boundary. Deliberate abort is `die(msg)`, a host provision the `ds` CLI installs for `run`/`dev`/`repl` and withholds from tests, embedders, and `--no-ambient`; a block-tail `die` against an annotated return refuses. The process face rides the `proc` capability of `platform "std"`: `args()` answers the process arguments `ds run --args` stages, `env(name)` reads one variable into the checked none member, `read_line()` answers stdin's next line, `eprint`/`eprintln` write stderr, `proc_run(cmd, args)` answers a spawned child's exit status, and piped `proc_spawn(cmd, args)` answers the linear `proc_handle` — `write` feeds the child's stdin, `read_out`/`read_err` take the captured output and diagnostics (the empty string a pipe's orderly end), `close_in` is the idempotent end-of-input while the handle stays live, `wait` answers the exit status idempotently, `try_wait` the non-blocking `int | none` poll, and the closing deinit kills and reaps a still-running child. The host member `exit(code)` ends the process with a deliberate status, the CLI owning the code: 0 clean, 1 trapped; a negative code traps — the run answers 1 with the standard trap note, a deliberate failure never reading as clean success — and a code above 255 wraps to its low byte, POSIX process semantics.

## Linearity

Every value is affine: moving it into a new binding, a consume-mode call, or a return kills the old binding, and use-after-move is a compile error rendered as a two-span linear diagnostic. Checking is one intraprocedural forward pass over last uses, with no lifetime annotations and no mode syntax at call sites.

`~Copyable` is a negated set constraint marking real resources (file descriptors, locks, foreign memory): such values cannot be copied or lost, `deinit` running exactly once at the unique instance's death, on every path including early return, panic, and cancellation teardown. The ambient's resources carry the pattern: `open_file` answers the deinit-carrying `file_handle` (fallible reads `read_raw`/`read_exact` answering `bytes | io_err`) and `net_connect` a `net_handle` served by `send`/`recv`/`close_conn` (fallible reads answering `() | net_err` and `string | net_err`), and `net_listen` a `net_listener` served by `accept` (answering the accepted connection's own `net_handle`) and `close_listener`. A deinit-carrying value refuses as a union member, and a deinit closes what an explicit close already closed. Phantom-state unions (`file[open] | file[closed]`) typestate on the same marker; explicit `drop` is a debug and FFI tool.

Mutation has one channel, a named call on an owned receiver with a `mut` mode, lexically visible at the call. Parameter modes are fixed at the definition site: default borrow (immutable), `mut` (an exclusive re-storable window, where the callee may re-seat its parameter and the caller never observes it), and `move` (a consuming hand-off, the deinit claim riding the move). Call sites carry zero mode syntax; inside a window are zero runtime checks, alias-freeness being proven, and outside it everything is shared-immutable.

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

Collections are linear-mutable copy-on-write: mutating calls take an exclusive window over the receiver and reuse it in place if and only if it is uniquely owned. Exclusivity is by spelling: a mutating call's receiver is a `var` binding or a fresh value, and a `const` receiver is a compile error. Functional updates (`map`, `filter`, `reduce` and friends) copy only when their source is aliased, a perf note and never an error; persistent structures are ordinary library types, `bytes`/`vec` the unboxed unique members. A record element is one unit: the checked `get` answers `t | none` whole, fields project out, and the copy closes move whole spans.

```ts
var club = list[user]()
club.push(user { name: "ada", age: 45 })                       // the whole span lands in place
club.push(user { name: "bob", age: 15 })
const cell = club.get(0)                                      // user | none, the checked read
const who = cell.ok((u: user) => u.name).err((_: none) => "?")  // fields project out
const grown = cons(user { name: "eve", age: 30 }, tail(club))   // cons and tail move whole spans
const roster = collect(grown)                                   // the whole-list copy
for u in roster { println("user ${u.name}") }
```

Iteration is combinators plus `for`, over one hidden linear cursor that linearity lets the compiler strip bounds checks from; chains fuse into loops. Subscripts are single-element (`xs[0]`) with no slicing sugar. String slices are library calls: checked `substring(s, 0, 2)` answers `string | none` on char boundaries, `char_at(s, i)` answers `char | none`, and `split`/`join` take one `char` separator. `xs.len()` and `m.len()` answer through the free generic's List and Map instances.

```ts
// rows: list[entry]
var users = list[user]()
var errors = list[parse_error]()
for r in rows.map(parse_user) {
  if r is parse_error { errors.push(r) } else { users.push(r) }
}
const total = users.filter((u) => u.age >= 18).map((u) => u.age * 2).reduce(0, (a, b) => a + b)
```

`list[record]` and `list[(int, string)]` carry whole word spans across each read, close (`cons`, `tail`, `collect`, `xs.concat(ys)`), and combinator; union elements refuse at every read, and a tuple arm resolves no member. `bytes` is one immutable byte string per value: `bytes_of`/`byte_list` bridge to `list[byte]`, `len`, the checked `byte_at` (`byte | none`), payload `==`, `show` as a byte list, lexicographic `ord`/`hash`, and the `utf8_encode`/`utf8_decode` bridges.

## Traits and generics

A trait is a set-theoretic interface; an instance is declared for a type head; receivers are `self` (borrow) or `mut self` (exclusive window), typed `Self`.

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

A trait member's default lives in the trait declaration: impls omit it and both call spellings stage the declared literal, while an inherent impl member keeps its own.

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

An impl member may omit the receiver: `impl gauge { fn make(level: int) -> gauge }` is called through its type name (`gauge.make(4)`) or an instantiated application (`list[int].make()`, the applied type solving as the call's `Self` and binding a generic impl's head variables, while a bare type name cannot reach a generic impl's members). Such a member is type-keyed and static, and the `default` derive answers through the same surface (`point.default()`); trait members keep the receiver.

Coherence is pairwise disjointness of one trait's instance heads — every pair proves `H1 & H2 = empty` — checked at each declaration, over the package's closed instance set, and at link time; generic heads count through their instantiations, so `impl shows for list[int]` and `impl [t: shows] shows for list[t]` overlap at `list[int]`. Any module may implement for any type, foreign included: no orphan rule, no newtype, no priority machinery. An overlap error names both heads and a witness value type, union heads decompose per branch, and a `& ~trait` bound carves exclusivity where overlap is intended.

Generics carry one clause — `[t: c]` on functions, `[t]` on types — and every generic function exists once in bytecode: call sites pass evidence words (statically resolved instances or runtime slots behind inline caches), and the JIT may specialize lazily. Dispatch comes from types and the set algebra, never declaration order or priority; no HKT, no GATs.

```ts
struct stack[t] { items: list[t] }
fn push[t: ~Copyable](s: stack[t], x: t) -> stack[t] { stack { items: cons(x, s.items) } }
```

Trait dispatch, generic impls, and transitively chained nested dispatch are legal, while a substituted type spanning more than one word refuses at the entering site, the runtime-evidence seam being one word; a generic fn used as a value, a generic lambda value, a user-declared generic application's values, and a builtin generic native (`cons`, `first`) over a bare variable refuse at lowering.

Operators are fixed traits — `+` to `add`, indexing to `index` — over a closed token set under the frozen precedence table, `==` never overloadable, and the operator traits are surface only (user `+` refuses at the checker). Derives cover the mechanical cases (`hash`, `show`, `ord` as field-order lexicographic, `clone`, `default`, `from_json`, `to_json`), and `==` never derives. An enum's `default()` answers its first case, or the one `#[default]` annotates, which also settles a defaulting struct field typing the union; an un-annotated union-typed field refuses as ambiguous.

Comptime is the language itself: a free fn whose parameters are all `comptime` (`comptime t: type` over the builtin `type` atom, or a scalar annotation taking a plain literal) evaluates its calls at compile time on the same VM in a world-fenced context, and may call an ordinary helper whose solved effect row is pure or trap alone. The answer, a scalar or a list, embeds at the call site as the literal a user would have written. No macros of any level exist, so hygiene is deleted structurally. Reflection is comptime-only: `fields(t)` walks a struct's fields or an enum's cases as (name, type, tag) records (a case's `type` its payload atom, `none` when it constructs bare, its `tag` its constructor name), `type_name` spells a type, and runtime type questions use the tags the data carries.

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

JSON is the ambient's `json` face: the predeclared enum's cases cover the grammar's shapes (`json_string { string }`, `json_object { map[string, json] }` among them), `parse_json: (string) -> json | parse_err` reads one document, `to_json: (json) -> string` writes the minified form back, and the typed readers walk a still-open shape (`json_member` answers `json | none`, each `json_as_*` reader its scalar or `parse_err`). `byte` (0-255) and `u32` ride json numbers, `char` a one-character string, `bytes` an array of one element per payload byte, an out-of-range or wrong-shaped value answering `parse_err`. Derive closes a declared shape: `from_json(v: json) -> Self | parse_err` reads one member per field — a missing required member answering `parse_err`, an optional field reading absence as `none`, a list field walking the array (a `t?` element reading each present element, a json `null` refusing like any wrong shape), a deriving field composing — and `to_json(v: Self) -> json` mirrors it, an absent optional leaving its member out (an optional nested inside a part included, a `t?` list element skipping its absent elements, an optional payload field leaving its member out). An enum target reads its unit cases from their names, its sole payloaded case from the payload's bare shape (an object for a struct payload, an array for a tuple), and several payloaded cases from the one-member object each case's own name keys. The shared refusal classes stay: a tag, a tuple field, generic declarations, `~Copyable` parts, an empty enum.

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

Definitions (the identity and visibility unit) live in files (a namespace spelling); files in one package share one namespace — no intra-package imports, intra-package cycles legal — and the package is the compilation unit; the module is the `ds.pkg` manifest, whose hash cascade makes it the lock. Cross-package cycles are a link error.

Module bodies are pure compile-time declarations; in script mode only, the entry file's top-level statements compile into a synthesized `main`, and imported files stay pure.

A line-initial `##` at top level separates blocks (the rest of the line is free text); definitions are order-independent, and a file without separators is one block. `///` is a doc comment attaching to the next definition, member or field included; `//` is the ordinary comment.

```ts
## state

tag disk_full

## io helpers

/// reads the whole text
fn read_all(f: string) -> string { f.to_upper() }
```

Visibility is private by default; `pub(pkg)` opens package-wide (tests included) and never enters the interface; `pub` opens to dependents and enters the generated `.dsi`, the reviewable API surface `ds info` writes.

Imports are symbol-selective, aliasable, and conditional per target, unified on the sigil form, and a `pub import` row is a re-export:

```ts
import @std/csv (reader)
import acme/pipeline/schema (entry, parse_row)
import acme/pipeline/io as pio
import acme/pipeline/fs if os == "windows"
pub import @deep (base, base as origin, step, unit)
```

A pub row's bound selections join the package's export surface under the names the row bound (alias included), a dependent binding them like the package's own exports, while a private row's binding stays internal. Refusals: a selectionless `pub import` re-exports nothing, `pub(pkg)` names nothing on an import, and `pub import @std` refuses — the merged-ambient reading binds the compile's own names, never import-bound ones.

Script mode is zero-config: `ds run script.ds` compiles and runs the entry file, and a `ds.pkg` beside it makes the run a package run over the content-addressed store (`$DS_CACHE/pkg`). `$DS_REGISTRY` names a registry, a local index root or a git remote URL (`https://...`, `file://...`), whose verified fetch fills the store's misses; `ds save <pkg-root>` installs a working-tree package into the store.

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

`ds run` folds an entry file's top-level statements into a synthesized `main`; `ds build` records the declared imports into the link table; an `@std` import's selections validate against the exported surface. A linked import binds callables, plain-literal consts, flat struct types (scalar, `bytes`, list-of-one-scalar, map-of-two-scalars, optional-scalar fields), non-generic enums and aliases, and bodiless structs, seeding the consumer's tag table; a computed or mutable const, a deeper foreign type, a trait member, an impl body, and a generic or defaulted function refuse as the unbindable shape (DS4021). `@std` is itself a package split between the shrunk core and the members moved into its own ds source (the string and json faces, the `max`/`min` pair, the `enumerate`/`zip`/`take`/`drop`/`flatten` combinators among them): a graph declaring the dep compiles over the core and binds the linked extern face (`to_upper`, `concat`), while a graph without the dep keeps the merged-ambient reading that `ds check`, the REPL, `ds dev`, and the LSP ride. A manifest-free script takes the declaring-graph discipline implicitly over the run path: `ds run`/`ds build` resolve the default bundle ahead of the compile, so a bare `config` — the extraction example above included — refuses as unknown over the core, and the bare stem form `import @std (...)` is the spelling that runs; the csv row above parses under the merged sessions with its `reader` use unresolved, while a run refuses at the import itself, naming the member the bundle does not carry.

`ds repl` appends declarations as their own blocks and folds statements into a REPL-owned `main`, each submission printing the value, its inferred type, and its effect row. The frame parks between submissions and resumes at the new statement, so prior statements run once; a trapping line rolls back without committing, its deinit-carrying bindings firing once on the drain. `:drop <name>` fires one live binding's deinit, `:reset` clears the session, `:quit` leaves.

`ds dev file.ds --hot` injects the suspension itself: a loop-driving entry parks at its back edge every watch round, so no language spelling exists; every saved edit recompiles and swaps the module in place, the parked frame's next call running the new code. A `ds.pkg` beside the file turns the watch over the package root it names, and `--args` stages the entry's parameters as `ds run` does.

`ds lsp` answers, over stdio, the hovering position's checked type with its effect row, preceded by the declaration's `.dsi` signature line when the position names an exported one; a word an import selection names but the open documents alone leave unknown answers its line from the linked dependency's cached `.dsi` artifact, the resolution re-run mid-session when the package's consumed inputs change. Completion offers the visible names (ambient first, package next, local scopes innermost-first), a dot receiver's members and UFCS candidates, the in-scope type variables in a type position, fixed declaration skeletons behind a typed keyword prefix, and auto-import edits binding an unresolved prefix through the file's `@std` import row; definitions, references, rename, document symbols, whole-document and on-type formatting, and semantic tokens answer beside them.

World-touching APIs (console, files, net, time) follow the capability idiom: the `ds` CLI installs an ambient platform handler for scripts behind the source's declaration (`std` the full face, `embed` console-only with every other world op refusing at install), so `println` above just runs, while tests and embedders pass the capability explicitly (`println(io, msg)`); v1 has no effect syntax. Foreign code enters as opaque linear C handles behind `extern fn` (double-free is a compile error); native libraries are package artifacts, the manifest pinning each under `[artifacts]` as `name = { ver, target, hash }`, and `ds bind <entry-or-root>` resolves the graph as a run does and reports each installed artifact's identity and exported C symbols.

A bound extern calls the installed library: `extern "math" fn ds_add(left: int, right: int) -> int` names the `[artifacts]` entry between `extern` and `fn`, binding the declaration to the library owning the `ds_add` C symbol, so `ds_add(20, 22)` answers `42`. The serving slice takes `int`, `bool`, and `float` words, a `string` or `bytes` parameter riding a call-scoped (pointer, length) pair, a unit return riding the absent word, the cap four C words with a pair occupying two. Outside the slice the refusal names the boundary: a string or bytes return, linear handles and richer aggregates, a generic, a fifth word, a bound extern used as a callable value, and a symbol the artifact does not export all refuse at lowering. A declared artifact no source binds never loads, while a binding run loads it at start; a bound extern outside a package run refuses. The binding is panic=abort, and a blocking foreign call pins its fiber, so preemption and cancellation observe only after it returns.

## Concurrency

Concurrency is stackful VM fibers in mandatory nurseries: `spawn` exists only inside a `nursery` block, whose scope exit joins children, has the first failure cancel siblings, and unions child errors out at the boundary, a payloaded member riding its whole value out and extracting like any union value. The block's value is the children's answers collected in spawn order — one list element per `spawn`, the body's own answer the element — and a spawned body may not hold a `mut` window over a binding that outlives the block. Code is blocking-style with no colors and no `await`, tail calls stay guaranteed per fiber, and bare detach requires an explicit root nursery or supervisor.

```ts
fn fetch_all(urls: list[string]) -> list[page] | net_err {
  nursery {
    for url in urls {
      spawn {
        match fetch_body(url) {
          timeout => cancel(timeout),
          refused => cancel(refused),
          p       => p,
        }
      }
    }
  }
}
```

Parallelism is fork-join over the sendability substrate: a linear value is sendable by construction and an immutable value shareable, so race-freedom is static; `par_map` and `par_reduce` over immutable or linear-partitioned inputs are deterministic, replayable, and lock-free by theorem.

```ts
const total = pages.par_map((p) => p.body.len()).par_reduce(0, (a, b) => a + b)
```

Shared state between fibers is one shape, the bounded channel: no lock, mutex, or atomic exists, the window discipline already making shared mutation unrepresentable. `chan[t](cap)` is the one type application taking a value argument — the bound, a channel being one opaque word — with `send(x)` answering unit and `recv()` the payload; a send onto a full channel and a receive from an empty one park in the channel's own waiter queue, the waking peer the resumer, no reactor interest and no deadline. The payload rides one word (a multi-word or deinit-carrying payload refusing at the send's staging), a negative bound traps, and a zero bound is the rendezvous: a send parks its staged payload until a receive takes it whole, a receive parks until a send pairs directly, the buffer never holding an element. A program whose every fiber waits on a channel no live fiber serves is the honest deadlock the stall guard reports.

```ts
const ch = chan[int](1)
const _ = nursery {
  spawn { println("woke ${ch.recv()}") }   // parks empty; the sibling's send resumes it
  spawn { ch.send(42) }
}
```

Cancellation is nursery-scoped and cooperative: an explicit cancel terminates a yielding child at its next resume, so a linear consume is never interrupted mid-flight, and teardown walks the kept frames with each deinit firing exactly once, `defer` (scope-exit sugar, non-normative) in teardown. The timer capability: `time()` the wall clock, `mono()` monotonic nanoseconds against a process-fixed origin (differences measure elapsed), `sleep(nanos)` parking the calling fiber; `firstof(thunks)` answers the first arrival of a raced thunk list and `interleave(thunks)` collects in arrival order, both @std combinators over the timer capability and the bounded channel; `timeout` stays queued on the value-reflection seam; supervision is a library pattern.

## The runtime

Execution is three tiers over one shared lowering: a typed register VM with unboxed slots, a copy-and-patch JIT, and Cranelift/wasm on demand. Tail calls are a spec-level guarantee, self and mutual, in every tier: the stack never grows at any call depth. Memory is precise reference counting with block reuse plus per-frame bump arenas — zero pauses, zero write barriers, no GC — and static typing deletes hidden classes, inline-cache guards, and deoptimization frames from the data path.

## What is deliberately absent

Deliberately absent, each a recorded refusal: truthiness (boolean contexts are bool-typed, nothing is coerced), the `or` operator (one word overloaded across fallback, early exit, loop control, and union coalescing; the extraction family replaces it), chained comparisons (`a < b < c` is not an expression), comprehensions (the combinator surface covers them), inheritance (structs compose and traits carry behavior), expression-evaluating defaults (defaults are literal-only), `any` (inference failure is a compile error), and macros (comptime plus static reflection, so hygiene is structural).

Also permanently excluded: user-defined operators, `Deref`-style coercions, subscript slicing sugar, vararg/spread packing, and word-form operators beyond `in` and `is`.

Deferred, recorded, not promised — additive valves that enter only after the core is stable and must re-fit this file: `|>`, anonymous record literals (the map literal rides this reservation; builder calls interim), `@` binders, `??` on `t | none`, `?.` desugar, `throws(...)` sugar, user-defined handlers, scoped named instances, labeled break/continue, and `it` shorthand.

That is the whole language. The explainability budget is the contract: any proposal that cannot re-fit this tutorial does not belong in DreamScript.
