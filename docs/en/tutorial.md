# DreamScript in one short tutorial

DreamScript is a general-purpose interpreted language with static set-theoretic types, affine-by-default values, guaranteed tail calls, reference-counted memory with no GC, and optional JIT tiers. This file is the entire v1 surface: the explainability budget requires the whole language to fit one short tutorial, and this file is that artifact.

Run examples as scripts with `ds run file.ds`. Snippets below are fragments; top-level statements are legal only in a script's entry file (see Modules and script mode).

## Values and bindings

A binding is `const` (the default, immutable) or `var` (a rebindable local). Immutability is the default everywhere; rebinding a `var` changes which value the name holds, and changing a value in place is a separate, always-visible mechanism (see Linearity).

```ts
const name = "dreamscript"        // const: immutable, type inferred as string
fn count_up() -> int {
  var count = 0                   // var: a rebindable local
  count = count + 1               // rebinding, not mutation
  count
}
```

Numbers: `int` is 64-bit signed, `float` is 64-bit IEEE, both raw in slots; literals take separators, radix prefixes, and width suffixes (`1_000_000`, `0xff`, `0b1010`, `0o777`, `42u8`, `2.0f32`). Arithmetic traps on overflow, division by zero, and oversized shifts (`1 / 0` panics; floats follow IEEE and never trap, so `1.0 / 0.0` is `+inf`); wrapping is explicit (`a +% b`, `a *% b`, unary `-%a`); narrowing is always an explicit call (`i32(u)`, `i64(f)`).

Strings are immutable UTF-8 with escapes `\n \t \r \\ \" \' \0 \u{...}`; raw strings `r"..."` and nestable `r#"..."#` carry no escapes and serve raw and multiline needs in one construct; `'x'` is a Unicode scalar char and `b'x'` a single byte. Interpolation is a string-only context: `"$x"` and `"${e}"` desugar to `str(e)` and compile when the operand is a primitive (`int`, `float`, `bool`, `char`, `string`) or carries the `show` trait; a union or optional operand is an error — extract first.

```ts
const who = "ada"
const tries = 3
const msg = "hi $who, next attempt ${tries + 1}"   // both hole spellings
const path = r"C:\root\tmp"                        // raw: no escapes
const first = 'x'                                  // char (b'x' is a byte)
```

Equality is one structural `==` over all data, never overloadable, and no `===` exists. Floats compare bit-structurally (`nan == nan` is `true`), function comparison is rejected statically, and mutable cells and `~Copyable` resources compare by identity.

## Types are sets

Declarations are nominal — every `tag`, `struct`, and `enum` has identity — and the type language above them is set algebra: unions `a | b`, intersections `a & b`, and differences `a - b` are ordinary type expressions over nominal atoms. Intersections keep the most precise member's layout; difference types are compile-time only and erase entirely. Type aliases never declare cases; payload cases live only in enum blocks.

```ts
struct bad_port { got: int }
struct page { url: string, body: string }
struct user { name: string, age: int }
enum parse_error { bad_number(string), missing_field(string) }
type entry = map[string, string]
```

The stdlib ambient predeclares the failure taxonomy the fallible reads answer — `io_err`, `parse_err`, and the net error `net_err` (`timeout | refused`) — the way `none` is predeclared, so a script's IO errors share one vocabulary (the bare `timeout` construction example below names an ambient member; redeclaring any of them is a duplicate-definition error).

Construction is fixed per declaration, and cross-spellings are checker errors: structs use brace form only, enum cases use call form only, unit tags are bare. Struct braces support punning.

```ts
timeout                                    // unit tag: bare
bad_number("nan")                          // enum case: call form only
page { url: "https://ds.dev", body: "" }   // struct: brace form only
const url = "https://ds.dev"
page { url, body: "" }                     // punning: the field fills from the local
```

`T?` is exactly `T | none` and is legal in every annotation position; `none` is an ordinary unit tag, so optionals, error unions, and enums share one mechanism — narrowing, matching, and one extraction family — with no `?.` operator.

Narrowing is `is`: Kotlin-shaped type narrowing (never identity comparison), whose right side is a type expression, so `|` and `&` are legal there. Refinement narrows monotonically through `is` tests, none tests, and match arms, resetting only at assignment.

```ts
tag json_null
tag json_bool
struct json_num { value: int }
struct json_str { value: string }
struct json_obj { fields: map[string, json] }
type json = json_null | json_bool | json_num | json_str | json_obj

fn port_of(j: json) -> int | none | bad_port {
  if j is json_obj {
    const pv = j.fields.get("port")            // inferred: json | none
    if pv is json_num && pv.value >= 1 && pv.value <= 65535 { pv.value }
    else { bad_port { got: -1 } }
  } else {
    none
  }
}
```

Types and values resolve in two namespaces: constructors — brace forms, bare tags, generic applications like `list[page]()` — live in the type namespace, functions in the value namespace. A struct `map` and a function `map` coexist; a call-form name present in both namespaces is an ambiguity error, never a priority search.

Lists and tuples: `[1, 2, 3]` is a `list[int]` literal; the empty mutable collection is `list[page]()`; map literals are deferred and maps are built by calls for now (see the absent list). Ad-hoc aggregates are anonymous tuples — `("host", 8080)` — with destructuring in bindings and patterns.

## Functions

`fn` is the only named-function declaration — at top level and as trait/impl members — and the defined name is in scope inside its own body, so recursion is direct.

```ts
fn add(a: int, b: int) -> int { a + b }
```

Parameters are `name: t`, returns are `-> t`, union returns parse as `-> a | b`, and `T?` is legal in any annotation. The body's final expression is the return value; `return e` exits early; `break` and `continue` serve loops; `unit` optionally names the unit type and `()` stays legal, and a function returning nothing may omit the annotation. Signatures are required on top-level and public definitions and anchor the set algebra; locals, lambdas, and match arms are inferred. Functions are values.

Anonymous functions are const-arrow lambdas, the only other function spelling — a binding inside any block names one, while top level names functions with `fn` only:

```ts
fn doubled(xs: list[int]) -> list[int] {
  const double = (x: int) => x * 2
  xs.map(double)
}
```

UFCS is universal and free: `x.f(a)` desugars to `f(x, a)` for any first-argument function, with no candidate search — free functions first, then trait methods, `trait::f(x)` qualification as the escape hatch, and true ambiguity a compile error. Capability parameters (IO and the like) are named and excluded from the dot position: `println(io, msg)` can never be written `msg.println()` with `io` hidden.

```ts
const n = "hello".len()        // desugars to len("hello")
```

Call sites may name arguments after positional ones, and signatures may default parameters — with literals only. A lambda in the last argument position may be written outside the parentheses (trailing lambda, last argument only).

```ts
fn connect(host: string, port: int = 8080) -> conn { open_socket(host, port) }
const c = connect("db.local", port: 8081)
```

## Control flow

Every block opens with a visible token; statements are newline-terminated with no semicolons; trailing commas are allowed everywhere; `if` and `match` are expressions.

```ts
const m = if a >= b { a } else { b }
```

`match` is Rust-shaped: arms `pat => expr` separated by commas or newlines, guards `pat if cond`, or-patterns `a | b`, destructuring bindings, and the `_` wildcard. Exhaustiveness is checked by tag identity over the set algebra, and the diagnostic names the missing members as a set computation ("missing: timeout | refused").

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

fn report(total: int, errors: list[parse_error]) -> string {
  match (total, errors.first()) {
    (_, e) if e is parse_error => "warning: one row failed",
    (0, _)                     => "no valid users",
    (t, _)                     => "total: ${t}",
  }
}
```

Loops are `for x in xs`, ranges `a..b` (exclusive) and `a..=b` (inclusive) — `in` keeps a bare range as its right side and is otherwise membership — plus `while cond`, with `break` and `continue`. Boolean contexts (conditions, `&&`/`||`/`!` operands) are bool-typed.

```ts
fn drill() {
  for i in 0..10 { println("${i}") }
  var n = 0
  while n < 3 { n = n + 1 }
}
```

The explicit early-exit spelling is `if x is none { return missing }`, with refinement applying to everything after it.

Operator precedence is one frozen C-family table, published here once and never extended, with no user-defined operators: postfix calls and subscripts bind tightest, then unary `!` and `-`, then `*` `/` `%`, then `+` `-`, then shifts, then `&` `^` `|`, then comparisons and `in` (`x in a..b && flag` reads `(x in a..b) && flag`), then `&&`, then `||`; the word-form operators are exactly `in` and `is`.

## Errors and extraction

Expected failures are union members of return types — storable data, one predicted branch in the VM, no exception machinery. `?` propagates: members outside the ok-type return early, with checked containment — the enclosing return union must contain the propagated member, and there are no implicit conversions.

```ts
fn load_config(path: string) -> config | io_err | parse_error {
  const text = read_file(path)?          // non-members of config return early
  parse_config(text)?
}
```

`.err(h)` and `.ok(h)` are set-difference-typed handlers: the result type is the incoming union minus what the handler covers, so the type itself shows what is left.

```ts
const cfg = load_config("app.cfg").err((e) => default_config())
// result type: (config | io_err | parse_error) - (io_err | parse_error) == config
const port = cfg.port.unwrap_or(8080)                       // int? -> int, eager fallback
const p2 = cfg.port.unwrap_or_else(() => env_port())        // lazy variant
const nm = entry.get("name").ok_or(missing_field("name"))?  // early exit with a new value
```

The @std extraction family — `unwrap_or`, `unwrap_or_else`, `ok_or`, `map_err` — plus `.err`/`.ok` are plain UFCS-callable generic functions with fully local typing; there is no operator to remember. Bugs panic unhandleably: the fiber aborts, linear deinits run during the frame walk, and the trap surfaces at the nursery or supervisor boundary; let-it-crash supervision is a library pattern.

The whole story in one function:

```ts
fn parse_user(row: entry) -> user | parse_error {
  const nm = row.get("name").ok_or(missing_field("name"))?
  user { name: nm, age: parse_age(row.get("age").ok_or(missing_field("age"))?)? }
}
```

## Linearity

Every value is affine: moving it into a new binding, a consume-mode call, or a return kills the old binding, and use-after-move is a compile error rendered as a two-span linear diagnostic ("value `h` was consumed here / used again here"). Checking is one intraprocedural forward pass over last uses — no lifetime annotations, no mode syntax at call sites. Copyable values satisfy the rule by copying invisibly, so everyday code never notices it.

`~Copyable` is a negated set constraint — it subtracts the default copyability constraint and composes with unions — marking real resources such as file descriptors, locks, and foreign memory. Such values cannot be copied and cannot be lost: `deinit` runs exactly once at the unique instance's death, on every path including early return, panic, and cancellation teardown. Phantom-state unions (`file[open] | file[closed]` style) typestate protocols on the same marker; explicit `drop` exists only as a debug and FFI tool — users never write routine drops.

Mutation has exactly one channel — a named call on an owned receiver with a `mut` mode — so it is always lexically visible at the call itself. The three parameter modes are fixed at the definition site — default borrow (immutable), `mut` (an exclusive mutable window for the call), consume — and call sites carry zero mode syntax: `xs.list_push(1)` reads identically whatever the mode. Inside a window there are zero runtime checks, because alias-freeness is proven; outside it everything is shared-immutable. Shared mutation is unrepresentable by construction: there is no hidden sharing anywhere in the language.

```ts
type raw_fd = int
struct file: ~Copyable {
  fd: raw_fd
  deinit { close_fd(self.fd) }              // exactly once, on every path
}

fn size(s: string) -> int { ... }           // borrow: immutable for the call
fn read_all(f: mut file) -> string { ... }  // exclusive mutable window
fn close(move h: file) { ... }              // consume: moved in, dies and deinits here
```

## Collections and iteration

Collections are linear-mutable copy-on-write: mutating calls take an exclusive window over the receiver and reuse it in place if and only if it is uniquely owned — in-place reuse is guaranteed and observable, not a JIT hope. Exclusivity is by spelling: a mutating call's receiver is a `var` binding or a fresh value, a `const` receiver is a compile error, and no hidden copy ever happens behind an aliasing call — mutation is visible at the call site or it does not happen. Functional updates (`map`, `filter`, `reduce` and friends returning fresh values) copy only when their source is aliased, surfaced as a perf note, never an error. Persistent structures are ordinary library types on the same runtime; `bytes` and `vec` are the unboxed unique members of the same model (FFI-grade).

```ts
var pages = list[page]()
pages.list_push(page { url: "https://ds.dev", body: "" })        // in place: the var receiver owns the window
pages.list_push(page { url: "https://example.org", body: "" })   // still in place: no copy hides behind the call
```

Iteration is combinators — `map`, `filter`, `reduce`, and friends — plus `for`, all over one hidden linear cursor that linearity lets the compiler strip bounds checks from; chains fuse into loops, and non-fusable shapes fall back to the same cursor semantics, so behavior is uniform. Subscripts are single-element (`xs[0]`); there is no slicing sugar — string slices are checked library calls (`s.slice_char(0, 2)` returns `string | none`, because UTF-8 char boundaries must be honored).

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
for u in users {
  println("user ${u.name} age ${u.age}")
}
```

## Traits and generics

A trait is a set-theoretic interface; an instance is declared for a type head; receivers are spelled `self` (borrow) or `mut self` (exclusive window), typed `Self` and exempt from annotation.

```ts
trait writable {
  fn to_json(self) -> string
}
impl writable for int {
  fn to_json(self) -> string { str(self) }
}
impl[t: writable] writable for list[t] {
  fn to_json(self) -> string { "[" + self.map((x) => x.to_json()).join(",") + "]" }
}
```

The only coherence rule is pairwise disjointness of one trait's instance heads — for every pair, prove `H1 & H2 = empty` — checked at declaration and again at link time. Any module may implement for any type, foreign included: no orphan rule, no newtype wrappers, no priority machinery; an overlap error names both heads and a witness value type, union heads decompose into per-branch instances, and `& ~trait` carves exclusivity when overlap is intended. Because heads never overlap, a tag-keyed inline cache never needs tie-breaking.

Generics carry one clause — `[t: c]` on functions, `[t]` on types — and every generic function exists exactly once in bytecode: call sites pass evidence words (statically resolved instances, or runtime slots behind one-to-two-entry inline caches with no correctness role), and the JIT may specialize per type lazily. Dispatch is determined by types and the set algebra, never by declaration order or priority — there is no specialization semantics to learn; no HKT and no GATs in 1.0.

```ts
struct stack[t] { items: list[t] }
fn push[t: ~Copyable](s: stack[t], x: t) -> stack[t] { stack { items: cons(x, s.items) } }
```

Operators are fixed traits: `+` dispatches to `add`, indexing to `index`, and so on over a closed token set under the frozen precedence table above; `==` is never overloadable, and ordering goes through the `ord` derive. Derives are attributes covering the mechanical cases — `hash`, `show` (which licenses interpolation holes), `ord` (field-order lexicographic), `clone`, `to_json`/`from_json`, `default` — and comptime is the language itself: ordinary code evaluated at compile time plus static reflection over fields and cases (`fn schema(comptime t: type) -> string`), with no macros of any level — no substitution exists, so hygiene is deleted structurally. Reflection is comptime-only; runtime type questions are answered by the tags data already carries (`is`, match).

```ts
#[derive(hash, show, ord)]
struct point { x: int, y: int }
```

## Modules and script mode

Four layers: definitions (the identity and visibility unit) live in files (a namespace spelling); files in one package share one namespace — zero intra-package imports, and intra-package cycles are legal — and the package is the compilation unit; the module is the `ds.pkg` manifest, the dependency and distribution unit whose hash cascade makes the manifest the lock (no lockfile exists). Cross-package cycles are a link error.

Module bodies are pure compile-time declarations — no import-time execution, period — and that one rule buys intra-package cycle tolerance, safe hot reload, and lazy loading. Script exception: in script mode only, the entry file's top-level statements are legal and compile into a synthesized `main`; imported files and package members stay pure, so the guarantee holds everywhere it protects.

A line-initial `##` at top level separates blocks (the rest of the line is free separator text); definitions are order-independent with or without separators, and a file without separators is one block.

```ts
## state

tag disk_full

## io helpers

fn read_all(f: mut file) -> string { ... }
```

Visibility is private by default; `pub(pkg)` opens package-wide (the package's tests included) and never enters the interface; `pub` opens to dependents and enters the generated `.dsi` — `ds info` writes it, it is the reviewable API surface, and if nothing in `.dsi` changes it is typically a safe refactoring.

Imports are symbol-selective, aliasable, conditional per target, and re-exportable, unified on the sigil form:

```ts
import @std/csv (reader)
import acme/pipeline/schema (entry, parse_row)
import acme/pipeline/io as pio
import acme/pipeline/fs if os == "windows"
pub import acme/pipeline/api (parse_row)
```

Script mode is zero-config: `ds run script.ds` compiles and runs the entry file, bare `@std/...` imports resolve, and `ds save` freezes what the script used into a generated `ds.pkg` — a script that outgrows itself gains a manifest without losing its file.

```ts
import @std/csv (reader)

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

World-touching APIs — console, files, net, time — follow the capability idiom: the `ds` CLI installs an ambient default handler for scripts, so `println` above just runs, while tests and embedders pass the capability explicitly (`println(io, msg)`) and swap schedulers with no code change; there is no effect syntax in v1. Foreign code enters as opaque linear C handles behind `extern fn` (double-free is a compile error), with `ds bind header.h` generating typed externs.

## Concurrency

Concurrency is stackful VM fibers in mandatory nurseries: a fiber stack is interpreter data, spawn is one allocation, a switch is a pointer swap, and `spawn` exists only inside a `nursery` block. Scope exit joins children, first failure cancels siblings, and child errors union out at the boundary; code is blocking-style with no colors and no `await`, tail calls stay guaranteed per fiber, and bare detach requires an explicit root nursery or supervisor.

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

Parallelism is fork-join over the sendability substrate: a linear value is sendable by construction and an immutable value is shareable, so race-freedom is static and the missing piece was only a scheduler — `par_map` and `par_reduce` over immutable or linear-partitioned inputs are deterministic, replayable, and lock-free by theorem.

```ts
const total = pages.par_map((p) => p.body.len()).par_reduce(0, (a, b) => a + b)
```

Cancellation is nursery-scoped and cooperative — a flag check at await and preemption points — so a linear consume is never interrupted mid-flight; teardown walks frames running deinits, and `defer` (scope-exit sugar, non-normative) runs in teardown. Timeouts, `firstof`, and interleave are library combinators over the timer capability; supervision is a library pattern — a supervisor is a nursery with a restart policy.

## The runtime

Execution is three tiers over one shared lowering — a typed register VM with unboxed slots (integers and floats raw in frame slots; boxing is the scalar overhead static typing exists to delete), a copy-and-patch JIT whose stencils compile in microseconds, and Cranelift/wasm on demand — and short scripts never pay for tiering. Tail calls are a spec-level guarantee, self and mutual, in every tier, deterministically: the stack never grows at any call depth. Memory is precise reference counting with block reuse plus per-frame bump arenas — zero pauses, zero write barriers, no GC — and static typing deletes hidden classes, inline-cache guards, and deoptimization frames from the data path; the only exits from JIT code are breakpoints, the stack-limit check, effect-handler entry, and the cold overflow edge.

## What is deliberately absent

Every item below is a refusal with a recorded reason; nothing here is missing for lack of imagination.

- **Truthiness** — boolean contexts are bool-typed; unions, options, numbers, and strings are never coerced, so `if x` cannot silently mean three things.
- **The `or` operator** — retired: it overloaded one word across fallback, early exit, loop control, and union coalescing, typed by contextual inference, colliding with `||`; the extraction family replaces every behavior on tokens that already exist.
- **Chained comparisons** — excluded permanently; `a < b < c` is not an expression.
- **Comprehensions** — excluded permanently; the combinator surface over one hidden cursor covers them.
- **Inheritance** — no class hierarchy exists: structs compose and traits carry behavior, so data, behavior, and identity never entangle.
- **Default parameters that evaluate expressions** — defaults are literal-only, so an argument default can never capture mutable state or run hidden code at a call site.
- **`any`** — there is no fallback type; inference failure is a compile error, never dynamic typing.
- **Macros** — comptime evaluation plus static reflection is the language itself; no substitution exists, so hygiene is deleted structurally.

Also permanently excluded by the same audits: user-defined operators, `Deref`-style coercions, subscript slicing sugar, vararg/spread packing, and word-form operators beyond `in` and `is`.

Deferred, recorded, not promised — additive valves that enter only after the core is stable and must re-fit this file: `|>`, anonymous record literals (the map literal rides this reservation; builder calls interim), `@` binders, `??` on `t | none`, `?.` desugar, `throws(...)` sugar, user-defined handlers, scoped named instances, labeled break/continue, and `it` shorthand.

That is the whole language. The explainability budget is the contract: any proposal that cannot re-fit this tutorial does not belong in DreamScript.
