# DreamScript working design decisions

Status: this file is the authoritative working record of every language decision (42 points: A1-A3, B1-B9, C1-C5, D1-D3, E1-E5, F1-F7, G1-G4, H1-H6), recorded by the author 2026-11-08 and extracted from the frozen archive in `design/`. All Phase 4 implementation work reads this file; the `design/` directory is a read-only archive kept for cross-reference only — never modify it, and do not read it unless a rule here is genuinely insufficient. Research notes (`design/en/research/01-06`) are raw evidence, not needed for implementation.

Source map (archive files backing each section): §2 `design/en/design/08-decisions.md`; §3 `design/en/design/01-types.md`; §4 `design/en/design/02-linearity-ownership.md`; §5 `design/en/design/03-effects-errors-async.md`; §6 `design/en/design/04-syntax.md`; §7 `design/en/design/05-generics-metaprogramming.md`; §8 `design/en/design/06-modules-tooling.md`; §9 `design/en/design/07-runtime.md`; per-point rationale lives in `design/en/design/09-decision-rationale.md`; the master worksheet is `design/en/design/00-decision-map.md`.

## 0. Author supplements (design/author_say.md) — binding

1. The main direction follows the Phase 3 record (08) and its rationale (09), extracted here.
2. Casing must never be a fixed grammatical feature: no construct may be distinguished by identifier case (e.g. types are not required to start uppercase). For types the author prefers lowercase spelling (`int`, `string`, `list`, `file`). All code in this document and in the archive uses capitalized type names as catalog illustration; normalize to lowercase when implementing. Case carries no grammatical meaning anywhere in the language.

## 1. The decided language (summary)

DreamScript is a standalone general-purpose interpreted language (embeddable via platform packages) with hybrid set-theoretic types — nominal tags composing under union, intersection, and difference, `is` narrowing, `T?` as an alias for `T | none` — checked bidirectionally with required boundary signatures and flow refinement, and universal structural equality with five carve-outs. Values are affine by default with an opt-in `~Copyable` linear resource layer (`deinit`, phantom states), second-class parameter modes (borrow / `mut` / consume), exclusion-window mutation, and linear-mutable COW collections; memory is Perceus reference counting with block reuse over per-frame arenas, no GC. Polymorphism is coherent set-theoretic traits with pairwise-disjoint instance heads, one generic body carrying evidence words and inline caches; metaprogramming is comptime reflection without macros. Errors are union members handled by `?` / `or` / `.err` beside an unhandleable trap; concurrency is VM fibers in mandatory nurseries with fork-join parallelism on the linear/immutable sendability substrate; effects stay beneath the surface over an evidence-passing handler runtime with capability-parameter IO. The surface is TS-family braces with full UFCS, const-arrow functions, Rust-shaped matching with set-difference diagnostics, Kotlin interpolation, under a 22-feature / 35-unit sugar budget (Kotlin's ceiling is 38 / 74-76). Modules are definition < file < package < module with generated `.dsi` interfaces and no import-time execution; packages are blake3 hash-cascade manifests over a content-addressed store with a zero-config script mode; FFI is linear C handles. Execution is a three-tier pipeline — typed register VM, copy-and-patch JIT, Cranelift/wasm — with spec-guaranteed self-and-mutual tail calls.

## 2. Identity and compatibility

- **A1 — standalone core, embedding via platform packages.** Build the real standalone runtime the perf charter demands; embedding arrives as ordinary packages exporting effect handlers over the same runtime (the default platform is the `ds` CLI itself). No second architecture for embedding.
- **A2 — workloads.** CLI/tools/scripts primary, games/simulation secondary (served by interpreter-first startup, zero-config script mode, two-slot hot reload). Numeric/AI stays reachable through the tier-3 Cranelift path without shaping v1.
- **A3 — compatibility.** Rapid-break until 1.0 (no compat promise), semver discipline after. Content addressing is rejected as the source-identity model and adopted at the package level (H6: blake3 hash cascade, the manifest is the lock).

## 3. Type system

### B1 — hybrid set-theoretic types over nominal tags

Declarations are nominal (tags and structs have identity); the type language above them is set-theoretic: unions `|`, intersections `&`, differences `-` of nominal atoms are ordinary type expressions. Exhaustiveness is checked by tag identity (enum-cover difficulty); difference types are compile-time only and erase entirely; a union of nominal atoms is the unboxed tag+payload layout; intersections keep the most precise member's layout.

```ts
tag Timeout; tag NotFound              // unit tags: distinct identity, value == the tag
struct BadPort { got: int }            // nominal, carries payload
type read_err = Timeout | NotFound | BadPort   // set expression over nominal atoms
fn load(p: path) -> config | read_err { ... }  // union-carried errors, unwrapped
const ok: config - read_err = load(p).err((e) => default())  // difference shrinks the handler result
trait animal; struct dog: animal; struct cat: animal
const a: animal = dog{}                // nominal subtyping by declaration; `dog | cat` matches by tag identity
```

Constraints: the disjointness/emptiness checker over nominal atoms is load-bearing (it underwrites B6 instance coherence); there is no `any` fallback — inference failure is a compile error, never dynamic typing. Recorded fallback: if the disjointness machinery slips schedule, retreat to a nominal-only core (B1b), which degrades error handling to match-based (no difference-typed `.err`).

### B2 — bidirectional checking with boundary signatures, plus flow refinement

Top-level and public signatures are required and anchor the set algebra; locals, lambdas, and match arms are inferred bidirectionally. Refinement narrows monotonically through `is` tests, none tests, and match arms; resets happen only at assignment (rare under immutability). The `?` propagation check is set containment against the written return union. Known limit: no narrowing through mutable fields of union-typed values.

```ts
const get_port = (j: json) -> int | missing | bad_port {
  const pv = j.fields.get("port")   // inferred: int | none
  if pv is int && pv >= 1 && pv <= 65535 { pv } else { return bad_port{ got: -1 } }
}
```

### B3 — nominal structs/enums plus anonymous tuples

Named data is nominal structs and enums (declared layouts, one type_id each, tuple-enums allowed); ad-hoc aggregates are anonymous tuples with destructuring; record punning supported. Anonymous record literals (`const point = { x: 1, y: 2 }`) are compiler-synthesized sugar over a nominal record — deferred additive until the inference story settles. Row types are rejected for data.

### B4 — `T?` is exactly `T | none`

`none` is a unit type among several; optionals, error unions, and enums share one mechanism (`is`, `or`, match). No `?.` operator in v1 (a thin desugar may be added later only if real code demands). Niche packing of `T | none` into one slot is a legal later optimizer pass (references are never zero). Known limit: `V | none` where `V` can itself be `none` is ambiguous — use an explicit wrapper (`found[v] | none`) at exactly those sites.

### B7 — numerics: i64/f64 defaults, trapping overflow

`int` = i64, `float` = f64, raw in unboxed slots. Literals: `1_000_000`, `0x`/`0b`/`0o`, width suffixes (`42u8`, `2.0f32`); unsuffixed integers are int, unsuffixed floats are float. No implicit narrowing anywhere — conversions are explicit calls (`i32(u)`, `i64(f)`).

| Expression | Behavior |
|---|---|
| `9223372036854775807 + 1` | deterministic trap (panic) |
| `a +% b`, `a -% b`, `a *% b`, `-%a` | wrap modulo 2^64 (hash/PRNG use) |
| `1.0 / 0.0` | `+inf` (IEEE, floats never trap) |
| `1 / 0`, `int_min / -1` | trap; error-union-returning variants are library functions typed `t | arith_error` |
| `a << k`, `a >> k` | trap when `k` exceeds bit width; rotations in library |

`Dec` (exact decimal) and units-of-measure are nominal library types plus derives.

### B8 — strings: immutable UTF-8, checked slices

Immutable UTF-8 bytes behind one raw pointer plus byte length; heap layout `{ type_id, rc, byte_len, bytes }`. Slices are non-owning borrows kept alive by the parent's RC, and must land on char boundaries via a checked API returning `string | none` (`s.slice_char(0, 2)`). `chars` iterates Unicode scalars; graphemes are stdlib; equality is O(n) with length fast-reject; rope is a future library type; encoding conversion happens at FFI boundaries.

### B9 — equality: universal structural `==` with five carve-outs

One structural `==` over all data, total by construction, never user-overloadable, no `===` exists. The five carve-outs: (1) function comparison rejected statically; (2) floats compare bit-structurally — `nan == nan` is true, dict keys stay sound — with named `is_float_eq` / `is_approx_eq` for IEEE semantics; (3) mutable cells and `~Copyable` resources compare by identity; (4) operands are borrowed, never consumed; (5) `hash` must be consistent with structural equality. `Ord` / `hash` / `show` arrive as derives; ordering can never be structural. Per-type custom value semantics go through named methods (`equiv`-style).

## 4. Linearity, ownership, memory, data

### C1 — affine core, opt-in `~Copyable` linear layer

The everyday rule is one sentence: assigning an affine value moves it, and a moved binding is dead. Ordinary copyable values never observably hit the rule. `~Copyable` subtracts the default copyability constraint (a negated set constraint, composing with B1 unions); it is where `deinit` lives, and it admits phantom-state unions (`file[open] | file[closed]` style). Affine checking is a last-use/DI analysis — intraprocedural, single forward pass.

Staging (no language change when later steps land): runtime `rc==1` uniqueness checks plus an Inko-style borrow counter ship first as the diagnostic layer; borrow inference, full reuse pairing, and TRMC arrive later as pure optimization.

### C2 — second-class parameter-passing modes

Three modes fixed at the definition site — default borrow (immutable), `mut` (exclusive mutable), consume (move) — and call sites carry zero mode syntax: `xs.push(1)` reads identically regardless of mode. Borrows never escape a call, so checking is intraprocedural; no lifetime annotations exist. First-class references are permanently out. Subscripts extend the same modes to element access (get/mutate/set accessors), replacing getter/setter protocols.

```ts
fn len(s: str) -> int { ... }              // default: borrow
fn push(xs: mut list<int>, x: int) { ... } // exclusive mutable window for the call
fn consume(f: file) -> int { ... }         // consuming move
```

### C3 — `const`/`var` bindings, mutation through `mut` windows

`var` is a rebindable local; callee-side mutation goes through `mut` parameter windows — one mechanism serving parameters, windows, and confinement (D1). Inside a window there are zero runtime checks (alias-freeness is proven); outside it everything is shared-immutable. FBIP reuse underneath makes functional updates run in place when the value is unique. No user-facing interior-mutability cell types (at most one hidden unsafe-cell equivalent in the stdlib for implementing persistent structures).

### C4 — Perceus precise RC + block reuse, plus per-frame bump arenas

Precise non-atomic reference counting for boxed/shared values: `rc_inc` at each explicit clone, `rc_dec` at each unique death (syntactically visible under affine checking); each drop threads to the next allocation as a reuse candidate (`kk_block_alloc_at` pattern; falls back to mimalloc at ~15-25 cycles when no candidate exists). Per-frame bump arenas for temporaries: allocation is a pointer add, reclamation a restore on frame pop — sound because linearity makes escape a compile-time fact. Zero pauses, zero write barriers, no safepoints, no GC maps in any tier. Cycles are rejected at the type level for v0; the two-hook collector interface (cycle-candidate registration, weak refs) is committed in the header format now as the growth path. Non-atomic counts are sound while values are thread-confined — E3's fork-join ownership transfer maintains this.

### C5 — `deinit` restricted to `~Copyable` types

`deinit` runs exactly once at the unique instance's death, on every path including early return, panic, and cancellation teardown. Deinit dispatch is a static call (concrete type known). Definite-initialization checking is infrastructure under everything. Explicit `drop` exists as a debug/FFI tool; users never write routine drops.

```ts
struct file: ~Copyable {
  fd: raw_fd
  deinit { close(self.fd) }
}
fn with_file(p: str, body: fn(mut file) -> t) -> t {
  const f = open(p)     // cannot be copied or silently dropped
  body(f)               // deinit runs at f's unique death — including on panic paths
}
```

### D1 — exclusion-window mutation is the only channel

Mutation is always a named call on an owned receiver with a `mut` mode: statically exclusive, lexically visible as the call itself, zero runtime checks inside the window. Shared mutation is unrepresentable by construction.

### D2 — linear-mutable COW collections

Collections expose a functional API and mutate in place iff uniquely owned — guaranteed and observable reuse. Aliasing means copying, surfaced as a perf note, never an error. Persistent HAMT-style structures are ordinary library types on the same runtime; `bytes`/`vec` are the unboxed unique members of the same model (FFI-grade).

### D3 — combinators over a hidden linear cursor, compiler fusion

The user surface is combinators (`map`/`filter`/`reduce`) plus `for`; the hidden protocol is a linear cursor (bounds-check-eliminable by linearity); the compiler fuses chains into loops; non-fusable shapes fall back to the same cursor semantics, so behavior is uniform. Subscripts extend in-place element access. No public generator syntax in 1.0; the hidden lowering may adopt the fiber facility later with no surface change.

## 5. Errors, effects, async, parallelism

### B5 — errors are union members of return types

Errors are storable data (nursery collection, supervision, caching), one predicted branch in the VM, identical information to an effect row. A `throws(...)` spelling remains later sugar desugaring to the same union. Propagation requires the enclosing return union to contain the propagated member (checked containment, no implicit conversions — Zig's rule, not Rust's `From`).

### E1 — no effect syntax in v1, handler-shaped runtime, capability idiom

The runtime is handler-shaped from day one: evidence passing serves the two internal effects (`cancel`, `discontinue`) at nursery boundaries — tail-resumptive internal ops allocate nothing; user-facing code paths are unchanged. World-touching stdlib APIs follow the capability idiom: an ambient default handler installed by the `ds` CLI for scripts, and an explicit capability parameter for tests, embedding, and scheduler selection — the same code runs over blocking, event-loop, or custom schedulers. User-defined handlers (`with handler { op ask() => resume(42) }`) stay a v2 additive escape hatch, never a migration. Standing typing obligations for whenever v2 activates: resumptions are affine unless marked (no linear value captured in a multi-shot continuation); FFI calls pin the fiber (resolved question 1).

### E2 — VM fibers with a mandatory nursery

Stackful heap fibers on the VM pool (a fiber stack is interpreter data; spawn is one allocation; switch is a pointer swap, O(1)). `spawn` exists only inside `nursery` blocks; scope exit joins children, first failure cancels siblings, child errors union out at the boundary. Blocking-style code, no colors, no `await`. Preemption is a flag check in the dispatch loop every N instructions or on backward jumps. TCO is per-fiber and intact (tail-looping fibers never grow their segment). Bare detach requires an explicit root nursery or supervisor.

```ts
nursery {
  for url in urls { spawn { pages.push(fetch_body(url)) } }
}   // exit: joined; first failure cancelled the rest; errors unioned out
```

### E3 — fork-join parallelism on the sendability substrate

Race-freedom and sendability are static consequences of linear/immutable data (a linear value is `sending` by construction; an immutable value is shareable); the missing piece is only a scheduler. `par_map` / `par_reduce` fork-join combinators over immutable or linear-partitioned inputs — deterministic, replayable, lock-free by theorem, cheap fork-side snapshots via static RC counts. Actor and supervision layers can be added later as libraries (a supervisor is a nursery with a restart policy).

### E4 — union errors with `?`/`or`/`.err`, plus an unhandleable trap

Expected failures are union members handled by three tokens whose semantics are stated once over unions: `?` (propagation with checked containment), `or` (extract the first wanted member, right operand evaluated lazily, with `or return` / `or break` / `or continue` early-exit forms), `.err(h)` / `.ok(h)` (set-difference-typed handlers — the remainder type shows what is left). Bugs `panic` unhandleably: the fiber aborts, linear deinits run during the frame walk, and the trap surfaces at the nursery or supervisor boundary. Let-it-crash supervision is a library pattern.

```ts
fn load_config(path: str) -> config | io_error | parse_error {
  const text = read_file(path)?        // `?`: members outside the ok-type return early
  parse_config(text)?
}
const cfg  = load_config("app.cfg") or default_config()
const cfg2 = load_config("app.cfg").err((e) => {   // result type: (config | io_error | parse_error) - (io_error | parse_error) == config
  log("config failed: ${e}")
  default_config()
})
const port = cfg2.port or 8080                     // option is t | none: the same three tokens
```

### E5 — nursery-scoped cooperative cancellation plus consume-to-close

Cancellation is nursery-scoped and cooperative, observed only at await/preemption points (a flag check), so a linear consume is never interrupted mid-flight; teardown walks frames running deinits. Resources close by consumption (a static move; leak-freedom by construction for in-language handles). `defer` exists as scope-exit sugar, non-normative, and runs at teardown under cancellation (Koka's `finally` semantics). Timeouts, `firstof`, and `interleave` are library combinators over the timer capability — nothing timeout-shaped is a language primitive.

## 6. Syntax surface and sugar budget

### F1 — TS-family braces

Every block opens with a visible token; line-oriented grammar, no layout state; newline-terminated statements, no semicolons, trailing commas allowed everywhere; `if` and `match` are expressions. Recorded fallback: Gleam/Roc-style braces (`fn name(` headlines) if prototyping shows the arrow-assignment grammar fighting the parser.

### F2 — bindings, functions, annotations, returns

`const` / `var` bindings (var is the rebindable local). Named functions as const-arrow declarations, uniform with bindings; the defined name is in scope inside its own body (recursion). Anonymous lambdas `(x) => e`. Parameters `name: t`, returns `-> t`; union returns parse as `-> a | b` with no extra syntax; `T?` is legal in every annotation position. Final-expression returns; early exits via `or return` / `or break` / `or continue`. `?` is reserved for postfix propagation; the old README's prefix-`?` return marker is retired. A bare `return` keyword may be added later as a pure additive. Functions are values.

```ts
const parse_age = (s: string) -> int | parse_error {
  match s.trim().parse_int() {
    e if e is parse_error => bad_number(s),
    n                     => n,
  }
}
const parse_user = (row: row_t) -> user | parse_error {
  const name = row.get("name") or return missing_field("name")
  user { name, age: parse_age(row.get("age") or return missing_field("age"))? }
}
```

### F3 — full UFCS

`x.f(a)` desugars to `f(x, a)` for any first-argument function; zero runtime cost, no candidate search. Resolution is fixed: free functions first, then trait methods, with `Trait::f(x)` qualification as the escape hatch; true ambiguity is a compile error, never a priority search. Capability parameters are named and excluded from the dot position — a dot-call feeds the first non-capability data parameter, so `println(io, msg)` can never be written `msg.println()` with `io` hidden; capabilities are passed explicitly or via a scope form. `|>` is a deferred additive (Roc deleted its pipe as redundant with chaining plus `?`).

### F4 — Rust-shaped pattern matching

Arms `pat => expr` separated by commas; guards `pat if cond`; or-patterns `a | b`; destructuring bindings; `_` wildcard; exhaustive matching with set-difference diagnostics — the error names the missing members as a set computation ("missing: NotFound | Locked"). `@` binders are excluded from v1 (`is` narrowing keeps the scrutinee variable usable; deferred additive with affine re-binding semantics to be designed if demanded).

### F5 — fixed trait-driven operator set, frozen precedence

A fixed token set maps to fixed traits (Add, Sub, Mul, ..., Index) — an operator call is a trait-method call, inline-cacheable. One frozen C-family precedence table published once in the tutorial, never extended, no user-defined operators ever; `Deref`-style coercions are excluded from the user surface. `==` is B9's structural equality and never overloadable; ordering goes through the `Ord` derive.

| Operation | Trapping (default) | Wrapping (explicit) | Notes |
|---|---|---|---|
| add / sub / mul | `a + b`, `a - b`, `a * b` | `a +% b`, `a -% b`, `a *% b` | trap panics with location |
| negate | `-a` | `-%a` | int-min edge |
| div / rem | `a / b`, `a % b` | none | division by zero and int-min/-1 always trap |
| shifts | `a << k`, `a >> k` | none | trap when `k` exceeds bit width |
| float | IEEE semantics, never traps | n/a | inf/nan per IEEE |

### F6 — strings and literals

Interpolation `$x` (bare identifier) and `${expr}` (expressions), with an explicit `str()` call at the non-string boundary — no hidden stringification, `T?`-valued interpolations require explicit handling. Raw strings `r"..."` and hash-counted `r#"..."#` (nest to any depth, serve raw/multiline/heredoc needs in one construct). `'x'` is a Unicode scalar char; `b'x'` is a single byte.

### F7 — sugar budget: Balanced tier (22 features / 35 units)

Against Kotlin's 38 features / 74-76 cost-unit ceiling; headroom reserved: 16 features / 40-42 units. Included rows: newline-terminated statements (S1); interpolation `$x`/`${e}` (S1); raw strings `r#"..."#` (S1); postfix `?` (S2); `or` extraction + `or return/break/continue` (S2); `is` narrowing (S2); `.err(h)`/`.ok(h)` difference-typed handlers (S2); match/if expressions with exhaustive-union diagnostics (S2); match guards (S1); or-patterns (S1); destructuring (S2); named + literal-only default arguments (S2); trailing lambda, last argument only (S2); UFCS (S2); type aliases (S1); sum-type declaration `tag`/enum (S2); derive attribute `#[derive(...)]` with auto `==` (S2); ranges `..`/`..=` + `in` (S1); numeric suffixes + separators (S1); `const`/`var` split (S1); operator-overload traits, fixed set and precedence (S2); `T?` alias (S2). Deferred-additive rows (in headroom order of likelihood): `|>` (S1), anonymous record literals (S2), `@` binders (S1), vararg/spread (S1), labeled break/continue (S1), lambda shorthand `it` (S1). Kotlin features deleted wholesale by set typing and UFCS: `?.`, elvis, `!!` (replaced by `is`/`or` + none-typing), smart casts (`is` narrowing), infix functions and extension functions/properties (UFCS), data-class synthesis (derives + structural `==`), sealed hierarchies (closed unions), receiver-lambda DSLs, primary constructors, reified type parameters, backtick identifiers, `tailrec` (TCO is guaranteed), context parameters (capability parameters). Excluded from user surface permanently: free operator overloading and custom operators, Deref coercions, convention-based privacy.

### 6.1 Implementation-normalized surface spellings (corpus-adopted; to be locked at M2)

Spellings the seed corpus (`conformance/`) uses where the record under-specifies the surface; adopted for implementation, locked or revised at M2 checking: trait member signatures spell the receiver as an ordinary leading parameter `(self: self, ...)` per C2 modes (no `&Self`/`&mut self`); derives are attributes `#[derive(hash, show, ord)]` per F7's derive row; generic value declarations use `struct stack[t]` with functional `push/pop` returning updated values per D2 (in-place field assignment is undecided); generic clauses on functions are spelled `[t: c]`; empty mutable collections construct as `list[page]()` (final stdlib spelling owned at M3); a v2 `with handler` block is out of v1 scope per E1 and stays commented in the corpus.

## 7. Polymorphism and metaprogramming

### B6 — coherent set-theoretic traits with pairwise-disjoint instance heads

Traits may carry associated types; instances are declared for type heads; the only coherence rule is pairwise disjointness of heads of one trait — for every pair `H1`, `H2`, prove `H1 & H2 = Empty` (decidable set emptiness), checked at declaration and again over the final instance environment at link time. Any module may define an instance for any type, foreign included — no orphan rule, no newtype wrappers. No priority machinery ever exists; resolution is intersection subtyping via modus ponens. An overlap error must name both heads and a witness value type in their intersection; a conditional instance whose satisfaction requires itself is rejected by a structural cycle check over resolution steps (link-time check, termination requirement fixed; algorithm implementation-defined). Union heads normalize: `impl eq for int | string` decomposes into per-branch instances before the disjointness check. Because heads are pairwise disjoint, a tag-keyed inline cache never needs priority tie-breaking. Scoped named instances (explicit dictionary values as an escape hatch) stay deferred additive with ambiguity always an error.

```ts
trait Ord { fn compare(self: &Self, other: &Self) -> int }
impl Ord for int { ... }
impl[T: Ord] Ord for list[T] { ... }        // conditional head
impl Draw for Circle { ... }
impl Draw for Circle | Square { ... }       // REJECTED: intersects both heads; witness: Circle
impl Draw for Circle & ~Shadowed { ... }    // negation carves exclusivity when overlap is intended
```

### G1 — comptime evaluation + static reflection, no macros

The meta-language is the language: comptime evaluation of ordinary code plus static type reflection (fields/cases of a type, comptime iteration and branching); macros of every level are rejected — no substitution exists, so hygiene is deleted structurally. Staged delivery: built-in derives ship first — `hash`, `show`, `ord` (field-order lexicographic; a `total_compare` float variant must be supplied explicitly for sort keys), `clone` (deep copy), `to_json`/`from_json`, `default`; `==` is universal and never derived. The comptime evaluator and reflection API land before 1.0.

### G2 — comptime-only reflection over tags-as-data

Reflection exists only at comptime; runtime type questions are answered by the tags data already carries (`is`, match — the tag test IS the type test). No type objects and no runtime metadata tables exist anywhere. Existentials remain available as evidence-carrying boxes (`box Json` — a value plus its dictionary) for heterogeneous collections and plugin boundaries, the one justified boxing site; default to universals in signatures because existentials box and break affine ownership reasoning.

### H3 — one generic body, evidence words, inline caches, lazy JIT specialization

Every generic function is represented exactly once in bytecode with its constraint list attached. At a generic call site, emit `CALL_GENERIC fn, evidence_words`; each evidence word is either a statically-resolved instance reference (a constant operand — the common case) or a runtime-resolved slot keyed by the runtime tag, patched as a 1-2 entry inline cache after first execution; a cache miss falls back to full lookup, so correctness never depends on cache state. The JIT tier lazily materializes per-type specializations keyed by the concrete type vector, cached with no correctness role. Ruling (anti-specialization invariant, verbatim): "dispatch is determined by types and the set algebra, never by declaration order or priority; the runtime may cache and specialize any dispatch that is already unique." Evidence words are ordinary register arguments, so generic calls in tail position tail-call normally; evidence words may themselves be affine values. Variance falls out of set inclusion: immutable containers are covariant; read roles covariant, write roles contravariant, fields with both roles invariant. No HKT and no GATs in 1.0 (concrete methods cover the classic drivers; revisit only if surface effect rows ever graduate).

## 8. Modules, visibility, FFI, ecosystem

### G3 — definition < file < package < module; no import-time execution

Four layers: definitions (visibility and identity unit), files (namespace spelling), packages/directories (the compilation unit — files in one package share one namespace with zero intra-package imports), and the module (`ds.pkg` manifest: dependency and distribution unit). Intra-package cycles are legal (whole-package fixpoint resolution); cross-package cycles are a link error whose diagnostic suggests moving the shared abstraction to a base package. Generated `.dsi` interface files are the reviewable API surface (`ds info` generates them; "if nothing in .dsi changes it is typically a safe refactoring"). Module bodies are pure compile-time declarations — no import-time execution, period; this one rule buys intra-package cycle tolerance, safe hot reload, and lazy dependency-driven loading. `///|` is an optional explicit block separator giving tooling sub-file granularity; top-level definitions are order-independent with or without it, and a file without separators is one block.

**G3.5 — the dsmod bytecode contract (binding).** Fixed-width, position-independent, mmap-able module format:

```text
bytes 0..3  magic "\0DSM"       byte 4 format version     byte 5 target class (bytecode | wasm | triple)
byte 6      flags (STRIP | FFI-USED | HAS-JIT-METADATA)
uleb        module name         uleb deps: [(name, content-hash)]...
uleb        source hash (blake3-128)   # PEP 552 checked-hash validation, never mtime
sections    blocks | constants | type metadata | effect rows | debug spans (strippable)
```

Cache at `$DS_CACHE/ir/<triple>/<blake3-of-source+deps>.dsmod`; mmap the hit path; lazy loading driven by the header's dep list; `ds build --exe` produces a single binary embedding the dependency closure + VM (Bun's measured 1.38x `--compile --bytecode` startup gain is the floor). Startup target: sub-10ms cold via the mmap'd store.

### G4 — private / `pub(pkg)` / `pub` plus selective imports

Definitions are private by default; `pub(pkg)` opens them package-wide (including the package's own tests) and never enters the `.dsi`; `pub` opens them to dependents and enters the `.dsi`. Imports are symbol-selective with aliasing, conditional per target, and re-exportable via `pub import`:

```text
import acme/pipeline/schema (Row, parse_row)
import acme/pipeline/io as pio
import acme/pipeline/fs if os == "windows"
pub import acme/pipeline/api (parse_row)
```

The `.dsi` is the visibility gate, the API-review artifact, the LSP hover source, and the incremental-compilation boundary. The path-scoped `pub(in ...)` ladder is deliberately left out to stay under the sugar ceiling.

### H5 — linear C-ABI handles as substrate, platform packages layered on

Foreign resources are opaque linear handles (`~Copyable`): double-free is a compile error, extern borrows are call-scoped raw pointers the type layer forbids from escaping the call, callbacks run on VM-managed stacks, and `ds bind header.h` generates typed externs. Plain `extern` declarations remain the documented escape hatch inside the handle discipline (they are what `ds bind` emits before handle wrapping). The embedding API is a handle table in the Lua shape (`ds_push` / `ds_ref`); a `Value` is an index into the VM's object table; the context is an explicit `ctx: &mut Vm`; two embedders are two independent VMs (no GIL by design). FFI pinning policy (resolved): the RC/arena heap never moves objects, so pinning is a lifetime question — owning foreign resources are `~Copyable` handles whose deinit releases exactly once; foreign callbacks execute on VM-managed stacks; a blocking foreign call pins its fiber, and carrier-thread execution is the escape when a blocking C call must overlap the scheduler; cancellation is observed only at VM re-entry, so a linear consume in flight is never interrupted.

Layered on top: platforms are ordinary packages exporting effect handlers, selectable per build target — `platform "std" provides <console, fsys, net, time>` (the `ds` CLI is the default platform), `platform "web"`, `platform "embed"` (host calls in, provides nothing); the checker verifies user code's effect usage is a subset of the platform's declared row. wasm/WIT is a tier-3 deployment target, not an FFI architecture. The dsmod FFI-USED flag declares dlopen needs at load time.

### H6 — hash-store core plus zero-config script mode; five mandatory tooling commitments

**Package stack.** `ds.pkg` manifests declare deps as `name = { ver, hash }` where hash is blake3 over the package content including its dependency hashes — identity flows transitively, no lockfile exists (the manifest is the lock). MVS resolution (pick the max of declared minima; multi-version coexistence by construction). A global content-addressed store keyed by output hash, doubling as the bytecode cache (artifacts are dsmod files). Bin-first per-triple artifacts verified against store hashes. Optional FHS profile linker (`ds link`; scoop shims on Windows, not symlinks). Registry deferred to a static JSON index over the store until ecosystem size demands it.

```text
name    = "pipeline"
version = "0.4.0"
[deps]
std     = { ver = "1.2", hash = "blake3:9f2c..." }
csv     = { ver = "0.7", hash = "blake3:1a88..." }
[artifacts]
x86_64-pc-windows-msvc = { url = ".../pipeline-0.4.0-x86_64-pc-windows-msvc.dsmod", hash = "blake3:77e0..." }
[build]                     # optional escape hatch, run by the embedded VM
script = "build.ds"         # sees #OUT#, #TMP#, #ARCH#
```

**Script mode.** `ds run script.ds` with zero config; bare-specifier std imports (`import "@std/csv"`); `ds save` freezes what the script used into a generated `ds.pkg`. Scripts that outgrow themselves gain a manifest without losing their file.

**Five mandatory tooling commitments, binding from day one.** (1) Compiler-as-query-library: the compiler is a library (`ds-check`) of memoized queries — parse, block_index, resolve, type_of, effects_of, linearity_check, lower — consumed by LSP, REPL, and hot reload alike; invalidation at `///|` block grain plus package grain (the second reason package cycles are banned). (2) `ds fmt` with zero options, implemented as a compiler component; the block-aware formatter formats blocks independently and is also the LSP's on-type-formatting path. (3) Structured diagnostics with two-span linear errors — machine-readable suggestions, `--error-format=json`, spans computed at typed-AST level; the linear diagnostic class renders "moved here / used again here" pairs:

```text
error[DS3001]: value `h` was consumed here
  --> src/pipeline.ds:41:15
   |  41 |  let rows = Csv.read(h)     ^ `h` moved (file is ~Copyable)
   |  42 |  Log.write(h.path)          used again here after move
help: clone the path before the move            (suggestion emitted as JSON too)
```

(4) Linear-aware REPL: prints inferred type and effect row; runs over the same bytecode IR; session-owned linear context with explicit `:drop` and `:reset` purging — linear values at the prompt are never silently dropped. (5) Erlang two-version code slots for hot reload: the runtime holds per-module versioned slots (current + old, at most two live); `ds dev --hot` recompiles changed blocks and swaps the slot; old-version frames retire at their next cross-module call (every cross-module call is fully qualified, so Erlang's requirement is trivially met); heap values are type-checked against new defs; effect handlers capture the code version they were installed with. No import-time execution is what makes this sound. The two-slot runtime must exist from day one — retrofitting is the named mistake to avoid.

## 9. Runtime

### Fixed baseline (BL-1..BL-7, binding on every tier)

- **BL-1 Typed register bytecode.** 32-bit fixed-format instructions, register-index operands, typed opcodes (ADDI64/ADDF64, EQI64/EQF64, KIMM/KCONST); no dynamic "V" variants because every operand's type is a compile-time fact. Copy LuaJIT's biased jump offsets (BCBIAS_J) and XOR-negated comparison pairs (ISEQV^1 == ISNEV).
- **BL-2 Unboxed slots.** No NaN-boxing, no per-slot tags, no boxed scalars: i32/i64/f64/bool raw in frame slots, references raw pointers, ADTs/unions/Result as unboxed tag+payload pairs across consecutive slots. Boxing is the 2-10x scalar overhead term static typing exists to delete.
- **BL-3 Dispatch.** Computed goto or C tail-call threading (`musttail` + `preserve_none`) — a build-portability choice, not a design point.
- **BL-4 Superinstructions on profile data.** The assembler composes fusions (compare-and-branch, load-op, dense-tag match jump tables) from measured opcode frequencies, never hand-picked.
- **BL-5 Uop decomposition from day one.** The instruction set is defined as micro-ops with explicit slot effects and `pure` annotations; the copy-and-patch stencil set is these uops one-to-one by construction.
- **BL-6 Frames inline in a typed value stack.** Each frame is a contiguous span of statically-typed slots plus a metadata word (return address, frame size, TCO-chain flags). Static layout is what makes TCO a pointer shuffle and the stencil ABI trivial.
- **BL-7 Position-independent, mmap-able dsmod.** Fixed-width, index-based, relocation-free bytecode with module header (version, type/constant tables, per-function frame-slot types, frame sizes, max call depth) and an IR-version checksum; contract detailed in G3.5.

### H1 — three tiers over one shared lowering

Tier 1: typed register VM, computed-goto uop loop, unboxed frames — target 3-10x of C. Tier 2: copy-and-patch stencils — one x86-64 fragment per uop emitted at build time by LLVM from a master template including the VM headers, with holes for frame-slot offsets and constants patched as u16/u64; ~14% faster than LLVM -O0 at ~100x its compile speed; target 1.5-3x; microseconds per unit. Tier 3: bytecode → typed SSA → Cranelift CLIF, or wasm 3.0 (`return_call` carries TCO); target 1-2x native / 1.5-2.5x wasm. One uop set and one bytecode-to-SSA lowering shared by all tiers, built early. Tier transitions are in-place J-variant opcode rewrites (one dispatch); OSR reduces to compiling the loop body — no frame transformation machinery. No guards on any data path; the only exits are (i) debugger breakpoints (frame materialization from JIT metadata), (ii) the per-entry stack-limit compare, (iii) effect-handler entry (a control transfer), (iv) B7's cold overflow edge — hence no deopt cliffs. Short scripts (<0.2s) never pay tiering and still hit 3-10x. Platforms without verified stencil builds ship tier1-only (CPython's opt-in posture). Recorded fallback: a single method-JIT tier (H1b) if the per-arch stencil toolchain proves unacceptable.

```text
; stencil ADDI64_rr, holes marked <H>; no type checks, no RC calls, no error glue
mov  rax, [r14 + <H off_r1>]
add  rax, [r14 + <H off_r0>]
mov  [r14 + <H off_r1>], rax
```

### H2 — guaranteed TCO from day one, self and mutual

Spec-level guarantee: frame reuse plus argument slide in tier 1 (Lua's `luaD_pretailcall`/`CIST_TAIL` discipline), label loops (Koka `kk__tailcall`) and slot-overlay `jmp` in JIT tiers, `return_call` on wasm. The slot-overlay ABI is locked at frame-layout time: callee argument slots overlay the caller's outgoing-arg area, so a tail call is at most an O(delta) arg slide plus a jmp; the stack never grows at any depth, in any tier, deterministically. Backtraces stay honest via tail-chain metadata with lazy materialization of elided frames on debugger request. The guarantee is scoped to tail calls within the same effect context; handler entry materializes a frame. Cross-tier tail calls go through a two-word callee entry descriptor `{tier1_prologue_ptr, tier2_code_ptr}` — when a callee later compiles, one descriptor word flips (same invalidation story as hot reload). Per-fiber guarantee holds: tail-looping spawned fibers never grow their segment; fiber segment growth (deep non-tail recursion) is a chunk-list append, not a realloc, so captured frame addresses stay stable. A tail call moving arguments transfers ownership with zero RC traffic. This ABI and BL-7 are the two decisions to lock before anything else.

```text
; fn even(n: i64) -> Bool { if n == 0 { true } else { odd(n - 1) } }   ; mutual
even:
  0: EQI64   r1, r0, 0
  1: JMPIF   r1 -> 3
  2: SUBI64  r3, r0, 1
  3: CALLT   odd, nargs=1, src=r3    ; slide r3 into slot0, reuse frame, reload prologue
  4: RET     true
```

### H4 — Perceus RC + reuse + per-frame arenas, collector interface as growth path

Per-frame bump arenas for non-escaping temporaries (mark/save at entry, bump alloc, restore on every pop path; segment growth by chunk). Perceus precise non-atomic RC with reuse threading for boxed/shared values (`rc_dec(tmp)` dies uniquely → reuse candidate → `alloc_at(reuse, ...)`). Zero pauses, zero barriers, no safepoints or GC maps in any tier; deterministic frees feed deterministic deinits. Arena marks are per-fiber, so fiber switching needs no arena action. Honest accounting: 100% of non-moved temporaries are register/arena-resident; the boxed/shared residue keeps paying RC; the reuse pass is budgeted as optimizer work with the mimalloc fallback. The H4(d) two-hook collector interface (cycle-candidate registration, weak refs) is committed in the header format now as the documented growth path for shared cyclic workloads (trial deletion off-thread; never applicable to linear data, which cannot cycle).

### What static typing deletes from the JIT

No hidden classes/maps or transitions (field offsets are bytecode immediates, `FIELD_LOAD %dst, %obj, #12`); no field-access inline caches or feedback vectors; no type guards or deopt frames on the data path; no speculation recording; no side traces or trace linking; tiering budgets reduce to plain hotness counters. This collapses roughly six speculation subsystems into two artifacts — a stencil patcher and a lowering pass — freeing ~70% of dynamic-VM tiering risk. What survives in IC form, perf-hint-only with no correctness role: 1-2 entry type-id-keyed ICs at dynamic/trait call sites, hot-reload module-global caches keyed on a module version counter, per-frame effect-handler operation tables.

## 10. Resolved open questions

1. **FFI pinning** — see H5.
2. **Nursery shape and token set** — block form `nursery { ... }` containing `spawn { ... }` child expressions; tokens finalized: postfix `?` (checked containment, no implicit conversions), `or` (first-wanted-member extraction, lazy right operand, `or return`/`or break`/`or continue`), `is` (narrowing), `.err(h)`/`.ok(h)` (set-difference-typed). No `?.` in v1.
3. **`.err` difference-typing fallback** — moot: B1(c) is adopted, difference typing stands as designed.
4. **Fibers vs hidden generators** — the v0 hidden iteration protocol is a linear cursor with no fiber involvement; once fibers exist the hidden lowering may adopt the same facility. One shared VM mechanism is permitted and staged; the user surface is identical either way.
5. **Generator arrow `-*>'` (historical README)** — retired; no public generator syntax in 1.0.
6. **Scoped named instances** — deferred additive, post-1.0 unless disjoint-head resolution demonstrably fails library authors earlier; ambiguity remains an error in any future sugar.
7. **Disjointness algorithm and termination** — implementation-defined with fixed requirements: link-time check over the final instance environment; overlap errors name both heads and a witness value type; self-satisfying conditional instances rejected by a structural cycle check.
8. **Float equality** — bit-structural (reflexive, total, dict-safe); IEEE semantics via named `is_float_eq` / `is_approx_eq`.
9. **Comptime in v1** — staged: built-in derives first, comptime evaluator plus reflection before 1.0.
10. **`@` binders in match** — excluded from v1; deferred additive with affine re-binding semantics to be designed if demanded.

## 11. Staging (v0 → 1.0)

Ordered; each step lands on the previous one with no language-change rewrites.

1. Lock the two load-bearing contracts first: the slot-overlay call ABI (H2) and the fixed-width mmap-able dsmod format (BL-7, G3.5). Everything downstream compiles against them.
2. Checker core: B1(c) set algebra over nominal tags, B2 bidirectional plus refinement, B3(d) products/sums, B4(c), B7(a), B8(a), B9(a); affine checker plus DI (C1b, C5c).
3. Tier-1 interpreter: computed-goto or tail-call-threaded uop loop, unboxed slots, tag+payload ADTs, CALLT frame-reuse TCO.
4. Memory: Perceus dup/drop placement, reuse threading, per-frame arenas; runtime `rc==1` checks and the borrow counter as the diagnostic layer.
5. Concurrency: fibers, nursery, internal `cancel`/`discontinue` evidence dispatch, fork-join combinators.
6. Derives; then the comptime evaluator and reflection API (before 1.0).
7. Tooling commitments from day one, sequenced with the query compiler they consume: `ds fmt`, structured diagnostics, REPL, two-slot hot reload.
8. Tier 2 copy-and-patch (x86-64, then aarch64; tier-1-only elsewhere), bytecode-to-SSA lowering built early, tier 3 on demand.
9. Deferred-additive backlog after the core is stable (see §12); borrow inference, TRMC, and fusion upgrades land as pure optimization with zero language change.

## 12. Deferred-additive backlog and recorded fallbacks

Deferred additive (in headroom order of likelihood): `|>`, anonymous record literals, `@` binders, vararg/spread; then `?.` desugar, `throws(...)` sugar, user-defined handlers (`with handler`), scoped named instances, bare `return` keyword, labeled break/continue, `it` shorthand. Pure-optimization items (no language change): borrow inference, full reuse pairing, TRMC, fusion upgrades, `T | none` niche packing, rope. Recorded fallbacks if named risks materialize: B1(b) nominal-only core (if disjointness machinery slips schedule; error handling degrades to match-based); F1(b) Gleam/Roc brace headlines (if the arrow-assignment grammar fights the parser); H1(b) single method-JIT tier (if the per-arch stencil toolchain proves unacceptable); C4(b) generational references (if affine friction proves worse than expected).

## 13. Seed test corpus

Per the decision protocol, the chosen combos' example programs are the language's first test corpus, in catalog order. Porting notes: the programs below are quoted verbatim from the archive; catalogs 01/02/03/05/06 predate the 04 surface decisions, so port them to the decided surface (const-arrow functions, `=>` arms, decided tokens) when turning them into tests, and normalize type names to lowercase per §0.

### 13.1 Types — "Hybrid" (config parser: tags, unions, narrowing, error sets, structural ==)

```ts
tag IoErr; tag Missing; struct BadPort { got: Int }         // nominal atoms
type CfgErr = IoErr | Missing | BadPort                     // set instruction over tags

struct JsonObj { fields: Map[String, Json] }
struct Config  { host: String, port: Int, retries: Int }

const get_port = (j: Json) -> Int | Missing | BadPort {
  const obj  = j is JsonObj or return Missing { key: "port" }
  const pv   = obj.fields.get("port") or return Missing { key: "port" }   // T | none union
  const p    = pv is Int or return BadPort { got: -1 }                    // narrows pv: Int
  if p >= 1 && p <= 65535 { p } else { return BadPort { got: p } }
}

const load = (path: String) -> Config | CfgErr {
  const text = read_file(path)?                              // CfgErr member propagates
  const port = get_port(parse_json(text)?)?
  Config { host: "localhost", port, retries: 3 }
}

const cfg = load("app.cfg").err((e) => {                     // exhaustive over the tag union:
  match e {                                                  // result: Config - CfgErr == Config
    b: BadPort => log("bad port ${b.got}"),
    _          => log("config read failed"),
  }
  default_config()
})

const kv   = ("host", 8080)                                  // anonymous tuple (B3.d)
const same = cfg == default_config()                         // universal structural == (B9.a)
```

### 13.2 Linearity — "Modes and RC" (linear File with deinit, mut windows, COW, fused chains)

```ts
struct File: ~Copyable {                 // C1.b linear resource layer (set-theoretic negation)
  fd: RawFd
  deinit { close(self.fd) }              // C5.c: runs exactly once, at unique death
}
fn read_u32(f: mut File) -> u32 | IoError {   // C2.d/C3.b: exclusive borrow for the call
  const b = f.read_exact(4)?            // window on f; ? propagates IoError; b is a fresh linear buffer
  b.to_u32_le()                         // b consumed here (affine move)
}
fn sum_file(path: Str) -> u32 | IoError {
  const f = open(path)                  // unique owner; cannot be copied
  var total: u32 = 0                    // C3.b: local var — the only rebindable binding
  var n: u32 = 0
  while n < 1000 { total += read_u32(f)?; n += 1 }   // ? moves the error out; f's deinit still runs
  total                                 // f dies here: deinit closes fd (also on early return)
}
const words = "alpha beta gamma".split(' ')          // D2.b COW: "new" list, reuses when unique
const lens  = words.map(w => w.len()).collect()      // D3.d: fused into one loop by the compiler
const total = (sum_file("metrics.bin") or 0) + lens.len()  // D1.b: mutation only inside windows/locals
```

### 13.3 Effects — "Effect-ready" (nursery fetchAll with cancel-on-failure, union errors, par combinators)

```ts
fn fetchBody(url: Url): Page | NetErr {         // no io parameter: the stdlib default handler is ambient
  const resp = fetch(url)?                      // performed through the default io handler (E1d runtime)
  return Page(url, resp.consume())              // linear consume closes (E5b)
}

fn fetchAll(urls: List[Url]): List[Page] | NetErr {
  val pages = Mut[List[Page]]()
  nursery {                                     // internally: a handler for `cancel`/`discontinue`
    for url in urls {
      spawn {
        match fetchBody(url) {
          page    => pages.push(page)
          error e => cancel(e)                  // evidence-dispatched internal effect, zero surface syntax
        }
      }
    }
  }
  return pages.freeze()
}

fn withRetry(n: Int, url: Url): Page | NetErr { // plain function: loop + unions, no effects needed
  var last = error NetErr.Timeout
  for i in 0..n {
    match fetchBody(url) { page => return page, error e => last = e }
  }
  return last
}

fn main(): void | NetErr {
  const pages = fetchAll(URLS) or die("all fetches failed")
  print("total: ${pages.par_map(fn(p) { p.size }).par_reduce(0, fn(a, b) { a + b })}")
}

// v2, additive with no migration — user handlers for final/tail-resumptive effects only:
with handler { op ask() => resume(42) }
```

### 13.4 Syntax — "TS-heart" (the full surface program: guards, `or return`, `?`, `.err`, interpolation, destructuring)

```ts
type ParseError = BadNumber(String) | MissingField(String)
type IoError = NotFound | PermissionDenied

struct User {
  name: String,
  age: Int,
}

const parse_age = (s: String) -> Int | ParseError {
  match s.trim().parse_int() {
    e if e is ParseError => BadNumber(s),   // guard + is: the union story, no wrappers
    n                    => n,              // n : Int by set difference
  }
}

const parse_user = (row: Row) -> User | ParseError {
  const name = row.get("name") or return MissingField("name")
  const age  = row.get("age")  or return MissingField("age")
  User { name, age: parse_age(age)? }
}

const main = () -> () | IoError {
  const rows = load_rows("users.csv")?
  const cfg  = read_config("app.cfg").err((e) => {
    println("config failed: ${e}")
    default_config()
  })
  const (users, errors) = rows.map(parse_user).partition((r) => r is ParseError)

  const total = users
    .filter((u) => u.age >= 18)
    .map((u) => u.age * 2)
    .reduce(0, (a, b) => a + b)

  for (u in users) println("user ${u.name} age ${u.age}")

  match (total, errors.first()) {
    (_, e) if e is ParseError => println("warning: ${e}"),
    (0, _) => println("no valid users"),
    (t, _) => println("total score: $t"),
  }
}
```

### 13.5 Generics — "Coherent-traits" (`~Copyable` generic stack, disjoint-head rejection, derive + comptime schema)

```ts
// 1. polymorphic container API over affine elements
type Stack[T] = { items: List[T] }
impl[T: ~Copyable] Stack[T] {
    fn push(&mut self, x: T) { self.items = cons(x, self.items) }  // consume-and-return
    fn pop(&mut self) -> Option[T] { ... }
}

// 2. trait with disjoint instance heads
trait Json { fn to_json(&self) -> String }
impl Json for Int  { fn to_json(&self) -> String { ... } }
impl Json for Bool { fn to_json(&self) -> String { ... } }
impl[T: Json] Json for List[T] {                     // conditional head: an intersection arrow
    fn to_json(&self) -> String { "[" + self.map(|x| x.to_json()).join(",") + "]" }
}
// impl Json for Int | Bool { ... }                  // REJECTED: head intersects both above

// 3. derive + comptime reflection
@derive(Hash, Show, ToJson)                          // one word, no macro system
type Point = { x: Int, y: Int }
fn schema(comptime T: type) -> String {              // user derive-like via reflection
    var s = "{"
    for f in fields(T) { s += f.name + ": " + type_name(f.type) + ", " }
    return s + "}"
}

// 4. execution: one body per generic fn + evidence words
let p  = max(p1, p2)            // evidence: static ref to impl Ord[Point] — constant operand
let js = p.to_json()            // dynamic site: IC keyed by tag -> the unique impl
```

### 13.6 Modules — "Hash-core" (ds.pkg manifest, selective imports, linear sqlite handle binding)

```text
// ds.pkg -- manifest is the lock; no lockfile anywhere
name = "pipeline";  version = "0.4.0"
[deps]  std = { ver = "1.2", hash = "blake3:9f2c..." }  csv = { ver = "0.7", hash = "blake3:1a88..." }
[artifacts]  x86_64-pc-windows-msvc = { url = ".../pipeline.dsmod", hash = "blake3:77e0..." }

// pipeline.ds -- imports (selective, aliased, conditional) over a package-local cycle
import @std/csv (Reader)                       // namespace alias, MoonBit-style
import acme/pipeline/schema (Row, parse_row)   // selective, from the generated .dsi surface
import acme/pipeline/fs if os == "windows"

///|  // FFI: linear handle, consumed exactly once
extern fn sqlite3_open(path: *const c.Char) -> (c["sqlite3"] | c.Int)
extern fn sqlite3_close(db: c["sqlite3"]) -> c.Int      // double-close: compile error
```

### 13.7 Runtime — `sum_to` uop encoding and the mutual `even`/`odd` tail-call encoding

```text
fn sum_to(n: i64) -> i64 { let mut acc = 0i64; while n > 0 { acc += n; n -= 1 }; acc }
  0: FUNCF  frame=2            ; slots: r0=n:i64, r1=acc:i64 (static layout)
  1: LDIMM  r1, 0:i64
  2: LEI64  r2, r0, 0          ; unboxed bool
  3: JMPIF_NOT r2 -> 6
  4: ADDI64 r1, r1, r0         ; pure machine add, no tag work
  5: SUBI64 r0, r0, 1 ; JMP 2
  6: RET    r1

; mutual tail calls (H2.a)
even:
  0: EQI64   r1, r0, 0
  1: JMPIF   r1 -> 3
  2: SUBI64  r3, r0, 1
  3: CALLT   odd, nargs=1, src=r3    ; slide r3 into slot0, reuse frame
  4: RET     true
```
