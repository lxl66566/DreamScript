# DreamScript working design decisions

Status: this file is the authoritative working record of every language decision (44 points: A1-A3, B1-B9, C1-C5, D1-D3, E1-E5, F1-F9, G1-G4, H1-H6), recorded by the author 2026-11-08, extracted from the frozen archive in `design/`, amended 2026-11-10 (E4/F2/F5/F7/F8: the `or` operator retired, bare `return` adopted, anti-prose surface guards pinned), and amended 2026-11-10 again (B3/F2/F6/F7/G3/G4/H6/6.1 plus new F9, the syntax-surface adoption: interpolation as a string context with the pinned escape set, the declaration/construction story with the list literal, fn-only declarations with `unit`, the receiver spelling, two namespaces, script entry, the `##` separator, sigil-form imports, ledger recompute). All Phase 4 implementation work reads this file; the `design/` directory is a read-only archive kept for cross-reference only — never modify it, and do not read it unless a rule here is genuinely insufficient. Research notes (`design/en/research/01-06`) are raw evidence, not needed for implementation. The archive's seed-corpus listings are not reproduced here: they are ported to the decided surface in `conformance/` (with per-file porting notes), and their runtime encodings are pinned in ds-abi/ds-vm tests.

Source map (archive files backing each section): §2 `design/en/design/08-decisions.md`; §3 `design/en/design/01-types.md`; §4 `design/en/design/02-linearity-ownership.md`; §5 `design/en/design/03-effects-errors-async.md`; §6 `design/en/design/04-syntax.md`; §7 `design/en/design/05-generics-metaprogramming.md`; §8 `design/en/design/06-modules-tooling.md`; §9 `design/en/design/07-runtime.md`; per-point rationale lives in `design/en/design/09-decision-rationale.md`; the master worksheet is `design/en/design/00-decision-map.md`.

## 0. Author supplements (design/author_say.md) — binding

1. The main direction follows the Phase 3 record (08) and its rationale (09), extracted here.
2. Casing must never be a fixed grammatical feature: no construct may be distinguished by identifier case (e.g. types are not required to start uppercase). For types the author prefers lowercase spelling (`int`, `string`, `list`, `file`). All code in this document and in the archive uses capitalized type names as catalog illustration; normalize to lowercase when implementing. Case carries no grammatical meaning anywhere in the language.

## 1. The decided language (summary)

DreamScript is a standalone general-purpose interpreted language (embeddable via platform packages) with hybrid set-theoretic types — nominal tags composing under union, intersection, and difference, `is` narrowing, `T?` as an alias for `T | none` — checked bidirectionally with required boundary signatures and flow refinement, and universal structural equality with five carve-outs. Values are affine by default with an opt-in `~Copyable` linear resource layer (`deinit`, phantom states), second-class parameter modes (borrow / `mut` / consume), exclusion-window mutation, and linear-mutable COW collections; memory is Perceus reference counting with block reuse over per-frame arenas, no GC. Polymorphism is coherent set-theoretic traits with pairwise-disjoint instance heads, one generic body carrying evidence words and inline caches; metaprogramming is comptime reflection without macros. Errors are union members handled by `?`, the `.err`/`.ok` difference handlers, and the `unwrap_or`/`ok_or` extraction family beside an unhandleable trap; concurrency is VM fibers in mandatory nurseries with fork-join parallelism on the linear/immutable sendability substrate; effects stay beneath the surface over an evidence-passing handler runtime with capability-parameter IO. The surface is TS-family braces with full UFCS, `fn` declarations with const-arrow lambdas, Rust-shaped matching with set-difference diagnostics, Kotlin interpolation, under a 22-feature / 34-unit sugar budget (Kotlin's ceiling is 38 / 74-76). Modules are definition < file < package < module with generated `.dsi` interfaces and no import-time execution; packages are blake3 hash-cascade manifests over a content-addressed store with a zero-config script mode; FFI is linear C handles. Execution is a three-tier pipeline — typed register VM, copy-and-patch JIT, Cranelift/wasm — with spec-guaranteed self-and-mutual tail calls.

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
fn get_port(j: json) -> int | missing | bad_port {
  const pv = j.fields.get("port")   // inferred: json | none; the is-test below narrows to int
  if pv is int && pv >= 1 && pv <= 65535 { pv } else { return bad_port{ got: -1 } }
}
```

### B3 — nominal structs/enums plus anonymous tuples

Named data is nominal structs and enums (declared layouts, one type_id each, tuple-enums allowed); ad-hoc aggregates are anonymous tuples with destructuring; record punning supported. Enum blocks are the only payload-case declaration form; standalone `tag x;` declares unit tags; type aliases never declare cases (2026-11-10). Construction is fixed per declaration — struct = brace form only, enum case = call form only, unit tag = bare — and cross-spellings are checker errors (Swift and Rust fix the construction form at the declaration; cross-spelling is a hard error in both). The list literal `[a, b, c]` is adopted for v1; the map literal stays deferred with the anonymous-record-literal reservation, builder calls interim. Anonymous record literals (`const point = { x: 1, y: 2 }`) are compiler-synthesized sugar over a nominal record — deferred additive until the inference story settles. Row types are rejected for data.

### B4 — `T?` is exactly `T | none`

`none` is a unit type among several; optionals, error unions, and enums share one mechanism (`is` narrowing, match, and `?` composed with the `unwrap_or`/`ok_or` extraction family per E4). No `?.` operator in v1 (a thin desugar may be added later only if real code demands); `??` on `t | none` is the single recorded operator valve (F8). Niche packing of `T | none` into one slot is a legal later optimizer pass (references are never zero). Known limit: `V | none` where `V` can itself be `none` is ambiguous — use an explicit wrapper (`found[v] | none`) at exactly those sites.

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
fn consume(f: move file) -> int { ... }    // consuming move
```

### C3 — `const`/`var` bindings, mutation through `mut` windows

`var` is a rebindable local; callee-side mutation goes through `mut` parameter windows — one mechanism serving parameters, windows, and confinement (D1). Inside a window there are zero runtime checks (alias-freeness is proven); outside it everything is shared-immutable. FBIP reuse underneath makes functional updates run in place when the value is unique. No user-facing interior-mutability cell types (at most one hidden unsafe-cell equivalent in the stdlib for implementing persistent structures).

### C4 — Perceus precise RC + block reuse, plus per-frame bump arenas

Precise non-atomic reference counting for boxed/shared values: `rc_inc` at each explicit clone, `rc_dec` at each unique death (syntactically visible under affine checking); each drop threads to the next allocation as a reuse candidate (`kk_block_alloc_at` pattern; falls back to mimalloc at ~15-25 cycles when no candidate exists). Per-frame bump arenas for temporaries: allocation is a pointer add, reclamation a restore on frame pop — sound because linearity makes escape a compile-time fact. Zero pauses, zero write barriers, no safepoints, no GC maps in any tier. Cycles are rejected at the type level for v0; the two-hook collector interface (cycle-candidate registration, weak refs) is committed in the header format now as the growth path. Non-atomic counts are sound while values are thread-confined — E3's fork-join ownership transfer maintains this. Capturing-lambda closure objects are ordinary blocks in this story (F2): the value is one Ref word and the capture block lives inside the object, so a lambda copy is a single count inc and a unique death a single dec — no per-capture counts — and the reuse threading treats a freed closure like any other candidate.

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

Mut-window exclusivity for copyable receivers (2026-11-10): a `mut` window parameter takes a `var` binding at the call site whenever its argument copies — lists, maps, and any copyable value that can reach shared-heap state; a `const`-binding receiver is a compile error (DS3006) with the binder as the secondary span. Copyable Ref words alias with no checker-visible trace, so exclusivity is by spelling; `~Copyable` receivers keep their linearity-proven exclusivity, and a fresh rvalue is uniquely owned by construction. COW-on-alias is rejected as the alternative: a hidden copy behind an aliasing call keeps the mutation invisible at the call site (the D1/C3 locally-visible-mutation rule) and silently withdraws the guaranteed in-place reuse. Where mutation is intended, spell the binder `var`.

### D3 — combinators over a hidden linear cursor, compiler fusion

The user surface is combinators (`map`/`filter`/`reduce`) plus `for`; the hidden protocol is a linear cursor (bounds-check-eliminable by linearity); the compiler fuses chains into loops; non-fusable shapes fall back to the same cursor semantics, so behavior is uniform. Subscripts extend in-place element access. No public generator syntax in 1.0; the hidden lowering may adopt the fiber facility later with no surface change.

## 5. Errors, effects, async, parallelism

### B5 — errors are union members of return types

Errors are storable data (nursery collection, supervision, caching), one predicted branch in the VM, identical information to an effect row. A `throws(...)` spelling remains later sugar desugaring to the same union. Propagation requires the enclosing return union to contain the propagated member (checked containment, no implicit conversions — Zig's rule, not Rust's `From`).

### E1 — no effect syntax in v1, handler-shaped runtime, capability idiom

The runtime is handler-shaped from day one: evidence passing serves the two internal effects (`cancel`, `discontinue`) at nursery boundaries — tail-resumptive internal ops allocate nothing; user-facing code paths are unchanged. World-touching stdlib APIs follow the capability idiom: an ambient default handler installed by the `ds` CLI for scripts, and an explicit capability parameter for tests, embedding, and scheduler selection — the same code runs over blocking, event-loop, or custom schedulers. User-defined handlers (`with handler { op ask() => resume(42) }`) stay a v2 additive escape hatch, never a migration. Standing typing obligations for whenever v2 activates: resumptions are affine unless marked (no linear value captured in a multi-shot continuation); FFI calls pin the fiber (H5).

### E2 — VM fibers with a mandatory nursery

Stackful heap fibers on the VM pool (a fiber stack is interpreter data; spawn is one allocation; switch is a pointer swap, O(1)). `spawn` exists only inside `nursery` blocks; scope exit joins children, first failure cancels siblings, child errors union out at the boundary. Blocking-style code, no colors, no `await`. Preemption is a flag check in the dispatch loop every N instructions or on backward jumps. TCO is per-fiber and intact (tail-looping fibers never grow their segment). Bare detach requires an explicit root nursery or supervisor.

```ts
nursery {
  for url in urls { spawn { pages.push(fetch_body(url)) } }
}   // exit: joined; first failure cancelled the rest; errors unioned out
```

Landed in-language (the M5 surface round). Four dispatcher-intercepted control natives in the builtin module carry the whole surface (indices 41-44: nursery open, spawn, boundary, cancel — no new uop majors; the boundary answers a two-word span on the caller's overlay, ok or err plus the payload). A spawn body lowers to a synthesized child function riding the lambda capture machinery: captured Refs bind as entry-frame argument words with copy-in count incs. The block's containment mask is the enclosing return union's bare member tag bits; a cancelled child's payload checks against it and an uncontained payload traps in the VM rather than corrupting the caller's union tag space. First failure raises the nursery's cancel flag; join drives children in spawn order and siblings observe the flag at their next taken backward branch, so a straight-line consume finishes in flight (E5's never-cut-mid-flight holds; an instruction-count preemption check is the later tier's seam). Refusals at lowering: bare `spawn` outside a nursery, a payloaded `cancel` value (bare enum members only this round; reboxing is the open alternative), a member outside the enclosing return union, and `return`/`?` inside a nursery or spawn body. Residuals: a cancel-cut child's frame keeps its captured counts (the VM substrate's future deinit pass is the seam), and unions over generic applications plus list elements wider than one word still refuse at lowering, so the fetched-page `fetch_all` shapes collect into a shared list and answer the count.

### E3 — fork-join parallelism on the sendability substrate

Race-freedom and sendability are static consequences of linear/immutable data (a linear value is `sending` by construction; an immutable value is shareable); the missing piece is only a scheduler. `par_map` / `par_reduce` fork-join combinators over immutable or linear-partitioned inputs — deterministic, replayable, lock-free by theorem, cheap fork-side snapshots via static RC counts. Actor and supervision layers can be added later as libraries (a supervisor is a nursery with a restart policy).

### E4 — union errors with `?`, `.err`/`.ok`, and the extraction family; `or` retired 2026-11-10

Expected failures are union members handled by tokens and methods whose semantics are stated once over unions: `?` (propagation with checked containment), `.err(h)` / `.ok(h)` (set-difference-typed handlers — the remainder type shows what is left), and the @std extraction family — `unwrap_or(y)`, `unwrap_or_else(fn)`, `ok_or(e)`, `map_err(fn)` — plain UFCS-callable generic functions with fully local typing, composed with `?` for early exits. The `or` operator is retired: it overloaded one word across fallback, early exit, loop control, and union coalescing, collided with boolean `||`, and typed by contextual wanted-member inference (F8 records the full rationale). The explicit early-exit spelling is `if x is none { return e }` with refinement, and bare `return` is adopted (F2). Bugs `panic` unhandleably: the fiber aborts, linear deinits run during the frame walk, and the trap surfaces at the nursery or supervisor boundary. Let-it-crash supervision is a library pattern.

```ts
fn load_config(path: str) -> config | io_error | parse_error {
  const text = read_file(path)?        // `?`: members outside the ok-type return early
  parse_config(text)?
}
const cfg  = load_config("app.cfg").err((_) => default_config())   // error-union fallback, lazy handler
const cfg2 = load_config("app.cfg").err((e) => {   // result type: (config | io_error | parse_error) - (io_error | parse_error) == config
  log("config failed")                     // e is a union; interpolation holes take primitives or show (F6)
  default_config()
})
const port = cfg2.port.unwrap_or(8080)             // option fallback: t | none -> t, eager right operand
const name = row.get("name").ok_or(missing_field("name"))?   // early exit with a new value; `?` propagates it
```

### E5 — nursery-scoped cooperative cancellation plus consume-to-close

Cancellation is nursery-scoped and cooperative, observed only at await/preemption points (a flag check), so a linear consume is never interrupted mid-flight; teardown walks frames running deinits. Resources close by consumption (a static move; leak-freedom by construction for in-language handles). `defer` exists as scope-exit sugar, non-normative, and runs at teardown under cancellation (Koka's `finally` semantics). Timeouts, `firstof`, and `interleave` are library combinators over the timer capability — nothing timeout-shaped is a language primitive.

## 6. Syntax surface and sugar budget

### F1 — TS-family braces

Every block opens with a visible token; line-oriented grammar, no layout state; newline-terminated statements, no semicolons, trailing commas allowed everywhere; `if` and `match` are expressions. Recorded fallback: Gleam/Roc-style braces (`fn name(` headlines) if prototyping shows the arrow-assignment grammar fighting the parser.

### F2 — bindings, functions, annotations, returns

`const` / `var` bindings (var is the rebindable local). Named functions are `fn name(p) -> t { }` — the only named-function declaration, at top level, as trait/impl members, and bodiless in trait signatures (`fn to_json(self) -> string`); the defined name is in scope inside its own body (recursion). Const-arrow remains only for anonymous lambdas `(x) => e` (Rust: `fn` for named, closures as expressions; TypeScript's dual spelling is the avoided style war). `unit` is an optional named spelling for the unit type in annotations; `()` stays legal. Parameters `name: t`, returns `-> t`; union returns parse as `-> a | b` with no extra syntax; `T?` is legal in every annotation position. Final-expression returns; early exits via bare `return` (adopted 2026-11-10 — B2's own example already used it while this section deferred it) and via `break` / `continue` in loops. `?` is reserved for postfix propagation; the old README's prefix-`?` return marker is retired, and the `or` extraction operator is retired per E4/F8. Functions are values.

```ts
fn parse_age(s: string) -> int | parse_error {
  match s.trim().parse_int() {
    e if e is parse_error => bad_number(s),
    n                     => n,
  }
}
fn parse_user(row: row_t) -> user | parse_error {
  const name = row.get("name").ok_or(missing_field("name"))?
  user { name, age: parse_age(row.get("age").ok_or(missing_field("age")))? }
}
```

Callable-word runtime ruling (2026-11-10, the b07 seam closed): Functions-as-values now has its runtime story, and every piece of it is a compile-time fact. A fn-typed value is a slot classification (`SlotType::Fn` in the call ABI): one word, never boxed, so a lambda passes to combinators exactly like an int. A non-capturing function value is a copyable scalar FuncRef — module id plus function index packed into the one word, copied by MOV, zero RC traffic — because a callee identity is two small integers, and wrapping it in a heap object would tax every tutorial combinator for nothing. A capturing lambda is a heap closure object: the capture block lives inside the object and the value in the slot is one Ref word under the ordinary C4 story; the flat alternative (capture words carried in the frame beside the fn word) is rejected because it makes the lambda value multi-word — no MOV copy, no single-word `fn | none` niche, per-capture slot typing, per-capture RC — while the object keeps one word, one count, and reuse threading like any other block. Captures follow the ordinary binding rules: copyable words copy into the block, affine words move (the outer binding dies); no mutable-upvalue channel exists in v1, so mutation visibility stays D1's named `mut` windows. The call is CALLS (call-major minor 0xA.6, reserved in ds-abi with the encoding locked): a call whose callee operand is a frame slot — the ISA's one dynamic-dispatch point — and not one of BL-5's banned dynamic V variants, which are polymorphic encodings of one opcode whose operand types must be tested at runtime; CALLS is a new fixed opcode with a register operand, the same shape as JMPIF's condition slot, and the word's type is the compile-time fact the slot classification already carries (a Fn slot reads the scalar FuncRef directly, a Ref slot reads it behind the closure object's header word, keyed on the static classification). Hot reload needs nothing new: a FuncRef word carries module id and index, never a code pointer, so resolution goes through the two-slot module table at call time — a staged fn word picks up a swapped module at its next call and old-version callers retire exactly like every other cross-module call (H6 item 5), a closure object's header FuncRef resolving identically. Encoding revision (2026-11-11, the closure-object round landed): construction is CLOSURE_NEW, Ref-major minor 0xD.3 — `a` the destination Ref slot, `b` the Fn slot holding the KCONST-minted FuncRef, `c` the capture count, the capture words staged at overlay positions `0..n` — allocating and impure like KSTR, with the FuncRef riding a slot because the fixed 32-bit word cannot carry a destination, a capture span, and a pool index at once. The object acquires one count per Ref capture as it adopts the block (the store-acquires rule): the staged sources keep their own counts and die their ordinary deaths — copyable bindings copy in, an affine capture's outer binding is dead to the program by the checker's move — and the object's death releases one count per Ref capture. At a CALLS on the Ref slot each Ref capture acquires one further count for the callee frame at the copy-in, the body owning its copy exactly like a borrow parameter (its exit drops balance it) while the object keeps its own count. The classification rule this round: every fn-typed slot stays the scalar Fn word, so a closure's Ref word flows only through value-site places — its construction word, const aliases of it, and a caller's result word when the callee's provenance is known (a named or lambda-value callee whose tail is a capturing lambda classifies Ref); staging a closure into any fn-typed parameter slot (user functions and the combinators' declared `Fn` parameters — the hosted per-element call's Ref branch is the R12 seam), widening one into a var span, and a lambda capturing a closure word all refuse at lowering, while a fn-typed result through any other callee keeps the scalar classification and a closure arriving there traps at the call — the callable-defaulting round owns the closure-capable callable surface. Callable-defaulting amendment (2026-11-11, the closure-capable surface landed): a fn-typed parameter slot is callable-defaulted — its classification stays `Fn` while the arriving word may be either the scalar FuncRef or a closure object's Ref word; the slide admits a staged `Ref` against a `Fn` parameter, and the call readers (CALLS on a `Fn`-classified slot, the hosted per-element call's staged word) resolve the callee word by its runtime kind — the scalar directly, the `Ref` behind the closure object's header — which is dispatch on the callee word's identity at the one dynamic-dispatch point CALLS already is, never a banned polymorphic-encoding value test. The staging discipline is a borrow window (the C2 native rule): the callee reads the word and never owns its count — the constructing caller keeps it, a dying source dropping after the call — while each CALLS or hosted copy-in acquires one count per `Ref` capture that the callee body's own placement balances; a closure-staging tail call therefore lowers in place (CALL plus flush plus return, the native-tail shape), keeping the frame-reuse guarantee for every scalar-staging tail site. The boundary that stays: a closure word never widens into a var span, is never captured by a lambda, and never enters a collection element or a struct field without proven scalar provenance (a named-fn or non-capturing-lambda word is provably scalar, a callable parameter's word is not), and a fn-typed result through any other callee keeps the scalar classification, a closure arriving there still trapping at the call; a native or generic callee's name mints no callable word (natives run inside their CALL, a generic's dispatch stages evidence per call site).

### F3 — full UFCS

`x.f(a)` desugars to `f(x, a)` for any first-argument function; zero runtime cost, no candidate search. Resolution is fixed: free functions first, then trait methods, with `Trait::f(x)` qualification as the escape hatch; true ambiguity is a compile error, never a priority search. Capability parameters are named and excluded from the dot position — a dot-call feeds the first non-capability data parameter, so `println(io, msg)` can never be written `msg.println()` with `io` hidden; capabilities are passed explicitly or via a scope form. `|>` is a deferred additive (Roc deleted its pipe as redundant with chaining plus `?`).

### F4 — Rust-shaped pattern matching

Arms `pat => expr` separated by commas; guards `pat if cond`; or-patterns `a | b`; destructuring bindings; `_` wildcard; exhaustive matching with set-difference diagnostics — the error names the missing members as a set computation ("missing: NotFound | Locked"). `@` binders are excluded from v1 (`is` narrowing keeps the scrutinee variable usable; deferred additive with affine re-binding semantics to be designed if demanded).

### F5 — fixed trait-driven operator set, frozen precedence

A fixed token set maps to fixed traits (Add, Sub, Mul, ..., Index) — an operator call is a trait-method call, inline-cacheable. One frozen C-family precedence table published once in the tutorial, never extended, no user-defined operators ever; word-form operators are exactly `in` and `is` (Kotlin-shaped) and boolean contexts are bool-typed with no truthiness coercion anywhere (F8); `Deref`-style coercions are excluded from the user surface. `==` is B9's structural equality and never overloadable; ordering goes through the `Ord` derive.

| Operation | Trapping (default) | Wrapping (explicit) | Notes |
|---|---|---|---|
| add / sub / mul | `a + b`, `a - b`, `a * b` | `a +% b`, `a -% b`, `a *% b` | trap panics with location |
| negate | `-a` | `-%a` | int-min edge |
| div / rem | `a / b`, `a % b` | none | division by zero and int-min/-1 always trap |
| shifts | `a << k`, `a >> k` | none | trap when `k` exceeds bit width |
| float | IEEE semantics, never traps | n/a | inf/nan per IEEE |

### F6 — strings and literals

Interpolation `$x` (bare identifier) and `${expr}` (expressions); the hole is an explicit string context — `$e` and `${e}` desugar to `str(e)`, legal when the operand type is a primitive (int, float, bool, char, string) or carries `show`, while a union or optional operand is an error: extract first (B4/E4). Every other position — operators, arguments, bindings — stays strictly explicit, where an implicit conversion would change resolution (Kotlin, Swift, and Rust format holes are all string contexts). Escape set (2026-11-10): `\n \t \r \\ \" \' \0 \u{...}`; raw strings `r"..."` and hash-counted `r#"..."#` (nest to any depth, serve raw/multiline/heredoc needs in one construct) carry no escapes. `'x'` is a Unicode scalar char; `b'x'` is a single byte. Byte typing ruled (2026-11-10): `byte` is its own atom in the u8 width class, outside the int conversion family — no implicit widening anywhere, explicit conversions arrive later as natives — the stdlib ambient declares `tag byte` so `b'x'` literals type with no user declaration, and lowering maps a byte word through the existing integer slot encoding (value 0-255) as a follow-up seam.

### F7 — sugar budget: Balanced tier (22 features / 34 units)

Against Kotlin's 38 features / 74-76 cost-unit ceiling; headroom reserved: 16 features / 40-42 units. Included rows: newline-terminated statements (S1); interpolation `$x`/`${e}` (S1); raw strings `r#"..."#` (S1); list literals `[a, b, c]` (S1, adopted 2026-11-10); postfix `?` (S2); `is` narrowing (S2); `.err(h)`/`.ok(h)` difference-typed handlers (S2); match/if expressions with exhaustive-union diagnostics (S2); match guards (S1); or-patterns (S1); destructuring (S2); named + literal-only default arguments (S2); trailing lambda, last argument only (S2); UFCS (S2); type aliases (S1); sum-type declaration `tag`/enum (S2); derive attribute `#[derive(...)]` (S2); ranges `..`/`..=` + `in` (S1); numeric suffixes + separators (S1); `const`/`var` split (S1); operator-overload traits, fixed set and precedence (S2); `T?` alias (S2). Deferred-additive rows (in headroom order of likelihood): `|>` (S1), anonymous record literals (S2; the map literal rides this reservation per B3), `@` binders (S1), `??` fallback on `t | none` (S1), labeled break/continue (S1), lambda shorthand `it` (S1). Kotlin features deleted wholesale by set typing and UFCS: `?.`, elvis, `!!` (replaced by `is` narrowing, none-typing, and the extraction family; `??` on `t | none` is the recorded valve), smart casts (`is` narrowing), infix functions and extension functions/properties (UFCS), data-class synthesis (derives + structural `==`), sealed hierarchies (closed unions), receiver-lambda DSLs, primary constructors, reified type parameters, backtick identifiers, `tailrec` (TCO is guaranteed), context parameters (capability parameters). Excluded from user surface permanently: free operator overloading and custom operators, Deref coercions, convention-based privacy, truthiness coercion in boolean contexts, chained comparisons, comprehension syntax, subscript slicing sugar, vararg/spread packing, and word-form operators beyond `in`/`is` (F8). Fn-only named declarations (F2) and the two-namespace split (F9) delete spellings rather than add sugar.

### F8 — extraction through methods and existing tokens; no prose operators or implicit coercions (2026-11-10)

The `or` operator is retired from the surface. Across the record and corpus it carried six behaviors — option fallback, error-union fallback, early exit with a new value, fallback onto a diverging callee, union coalescing of differently-typed operands (`done or last` producing `page | net_err`), and loop control — typed by contextual wanted-member inference, under a word colliding with boolean `||`; the archive's `is ... or return` over a boolean test was the boundary already sliding. The replacements live on tokens that already exist plus one small @std family: option fallback `x.unwrap_or(y)` with the lazy `x.unwrap_or_else(fn)`; early exit with a new value `x.ok_or(e)?` and `x.map_err(fn)?`; error-union and diverging fallback `x.err(h)`; union coalescing `match done { p => p, none => last }`; loop control `if x is none { break }`. Bare `return` enters the surface with the retirement (the B2 example already used it while F2 deferred it — resolved by adoption). If corpus evidence later shows the option fallback deserves an operator, `??` restricted to `t | none` is the single recorded deferred additive (S1); no English word ever re-enters the operator set.

Guards adopted with the accompanying prose-syntax audit, binding at review: operators stay C-family symbols with `in` and `is` as the only word forms; boolean contexts (`if`/`while` conditions, `&&`/`||`/`!` operands) are bool-typed, with no truthiness coercion of unions, options, numbers, or strings anywhere; chained comparisons are excluded permanently; comprehension syntax is excluded permanently (the D3 combinator surface covers it); subscripts are single-element Index access with no slicing sugar (string slices are checked library calls per B8, and other collections follow); vararg/spread packing is retired from the deferred-additive backlog (explicit list arguments and `concat` cover it). Hazards already deleted structurally, recorded for completeness: mutable default arguments are impossible because defaults are literal-only, and mixed numeric arithmetic is an error naming the conversion (B7). `is` is Kotlin-shaped type narrowing (not Python identity comparison) and `in` is Kotlin-shaped membership; both spellings stand. `in` is range-wired in v1 (ruled 2026-11-10): the right side is a range expression (`x in a..b`); collection membership belongs to the list surface and is not ruled, so the checker refuses a non-range right side instead of typing it bool ahead of a ruling, with lowering keeping its own refusal as defense.

### F9 — two namespaces: types and values (2026-11-10)

Types and values resolve in two namespaces. Constructors — brace construction, bare tags, generic-application constructors such as `list[page]()` — resolve in the type namespace; functions resolve in the value namespace; a call-form name present in both namespaces is an ambiguity error, never a priority search (F3's rule). The `map` struct and the `map` function coexist (Rust resolves type paths and value paths in separate namespaces, so a struct and a function share a name freely; C++'s fuller namespace machinery is the over-engineered counterexample); a call-form collision between an enum case and a function is the residual error class.

### 6.1 Implementation-normalized surface spellings (corpus-adopted; to be locked at M2)

Spellings the seed corpus (`conformance/`) uses where the record under-specifies the surface; adopted for implementation, locked or revised at M2 checking: trait member signatures spell the receiver exempt from annotation and typed `Self` — plain `self` for the borrow mode, `mut self` for the exclusive window per C2, no `&Self`/`&mut self`; other parameters may use `Self` in their annotations; `self` as a type name is retired (2026-11-10; Rust precedent — no mainstream trait system makes the receiver restate its own type); derives are attributes `#[derive(hash, show, ord)]` per F7's derive row; generic value declarations use `struct stack[t]` with functional `push/pop` returning updated values per D2 (in-place field assignment is undecided); generic clauses on functions are spelled `[t: c]`; empty mutable collections construct as `list[page]()` (final stdlib spelling owned at M3); a v2 `with handler` block is out of v1 scope per E1 and stays commented in the corpus.

Parser rulings (ds-parse landed 3167597): generic application is canonically square brackets `list[int]`; the angle form `list<int>` (which appears in C2's pre-surface example text) is accepted as parser laxity but never emitted by the formatter. Block-bodied members, match arms, and struct/enum fields are canonically newline-separated; trailing commas are accepted everywhere per F1. Parameter modes live inside the annotation (`f: mut file`) with a leading bare-mode shorthand (`mut self`) per C2; `move` and `comptime` are contextual keywords; trait/impl members are `fn` declarations, bodiless in traits (const-arrow named members retire per F2); type-level case references, tuple types `(a, t?)`, and foreign types `c["sqlite3"]` parse as type forms; the `is` right-hand side is a type expression (it consumes `|`/`&`); C-family `&&`/`||`/`!`/`&`/`^`/`|` are in the fixed operator set; ordinary strings are single-line and block comments do not nest; top-level `fn` is the only named-function declaration (const-arrow named declarations retire per F2) while top-level expression statements are rejected per G3 except in the entry file under script mode (G3); `none`, `self`, `Self` are not reserved. Identifiers are ASCII at M1.

Parser rulings, audit round 3: `in` binds at comparison level per F5 (`x in xs && flag` reads `(x in xs) && flag`, never `x in (xs && flag)`) and keeps a bare range as its right side (`x in a..b`); a lone `\r` is rejected inside strings, interpolation holes, and char literals exactly as in code (raw strings alone may carry it); newline arm separation covers expression-bodied arms, not just block-bodied ones; trait and impl member lists accept comma separators and trailing commas exactly like struct/enum bodies; impl members are `fn` declarations only — a plain `const x = 1` or const-arrow member is a parse diagnostic (DS1010), never a silent drop; an interpolation hole never crosses a newline (ordinary strings stay single-line); F7's named + literal-only default arguments landed: call sites take named arguments after positional ones (`f(x: 1)`; a duplicate name is DS1011 and positional-after-named is rejected) and signatures take literal-only defaults (`x: int = 0`, with `-lit` negatives admitted; anything else, including an interpolating string, is DS1012); flat operator chains (including stacked unary and postfix chains) are capped at 128 terms (DS1009) because dropping a deeper `Box` chain overflows the caller's stack.

## 7. Polymorphism and metaprogramming

### B6 — coherent set-theoretic traits with pairwise-disjoint instance heads

Traits may carry associated types; instances are declared for type heads; the only coherence rule is pairwise disjointness of heads of one trait — for every pair `H1`, `H2`, prove `H1 & H2 = Empty` (decidable set emptiness), checked at declaration and again over the final instance environment at link time. Any module may define an instance for any type, foreign included — no orphan rule, no newtype wrappers. No priority machinery ever exists; resolution is intersection subtyping via modus ponens. An overlap error must name both heads and a witness value type in their intersection; heads the set algebra reads disjoint because a generic head is one opaque atom get the same rule through instantiation (landed 2026-11-12): a declaration probes whether some grounding of either head is served by both, so a concrete head and a generic head that both cover it — and two generic heads whose bounds an existing atom satisfies — are rejected at the later declaration with the shared instantiation as the witness, conservatively (a pair no existing atom can prove declares and still surfaces as call-site ambiguity if a later impl makes the bounds intersect); a conditional instance whose satisfaction requires itself is rejected by a structural cycle check over resolution steps (link-time check, termination requirement fixed; algorithm implementation-defined). Union heads normalize: `impl eq for int | string` decomposes into per-branch instances before the disjointness check. Because heads are pairwise disjoint, a tag-keyed inline cache never needs priority tie-breaking. Scoped named instances (explicit dictionary values as an escape hatch) stay deferred additive with ambiguity always an error. Call-site resolution (landed 2026-11-11, checker half): a query unifies against a generic head — `impl [t: shows] shows for list[t]` serves a `list[int]` receiver — with every receiver member covered under its own substitution and each declared bound proven recursively (depth-capped; the declaration-time cycle rejection bounds the recursion); the solved instance is the declared one, no per-substitution instance is ever minted, and the call site's recorded receiver type carries the substitution for the dispatch seam; several serving impls are true ambiguity (an error naming them, never a priority order), a head that matches while a bound fails is diagnosed as the unsatisfied bound over the substituted type through both the dot and the qualified spelling, and a site only an in-scope bound assumption covers keeps recording nothing (the runtime-resolved evidence form). An impl member must agree with its trait member's registered signature — arity, receiver spelling, passing modes, parameter types, and return — because that registered signature is the call-site contract; a divergence refuses at check time (DS2018) at both sites, with the trait's generic parameters substituted positionally through the impl's trait reference; an unannotated impl return is agreed through its body's checked tail type against the same substituted declaration. A trait's own members see `Self` as satisfying their trait — a scoped assumption active only inside the trait's member bodies — so default bodies call sibling members on `self` with no instance.

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

Every generic function is represented exactly once in bytecode with its constraint list attached. At a generic call site, emit `CALL_GENERIC fn, evidence_words`; each evidence word is either a statically-resolved instance reference (a constant operand — the common case) or a runtime-resolved slot keyed by the runtime tag, patched as a 1-2 entry inline cache after first execution; a cache miss falls back to full lookup, so correctness never depends on cache state. Encoding amendment (2026-11-10, phase 1 landed): the locked ds-abi call major (0xA) reserves minor 4 for CALLG (the CALL_GENERIC emission form, operand-identical to CALL; the evidence arity travels with the callee's constraint list, so the uop keeps the CALL operand shape) and minor 5 for CALLTG (the generic tail-call form, reserved, no tier-1 emission yet); evidence words are ordinary register arguments staged in the caller's outgoing-arg area after the true arguments, phase-1 words being i64 constants naming builtin instances. The JIT tier lazily materializes per-type specializations keyed by the concrete type vector, cached with no correctness role. Ruling (anti-specialization invariant, verbatim): "dispatch is determined by types and the set algebra, never by declaration order or priority; the runtime may cache and specialize any dispatch that is already unique." Evidence words are ordinary register arguments, so generic calls in tail position tail-call normally; evidence words may themselves be affine values. Variance falls out of set inclusion: immutable containers are covariant; read roles covariant, write roles contravariant, fields with both roles invariant. No HKT and no GATs in 1.0 (concrete methods cover the classic drivers; revisit only if surface effect rows ever graduate).

## 8. Modules, visibility, FFI, ecosystem

### G3 — definition < file < package < module; no import-time execution

Four layers: definitions (visibility and identity unit), files (namespace spelling), packages/directories (the compilation unit — files in one package share one namespace with zero intra-package imports), and the module (`ds.pkg` manifest: dependency and distribution unit). Intra-package cycles are legal (whole-package fixpoint resolution); cross-package cycles are a link error whose diagnostic suggests moving the shared abstraction to a base package. Generated `.dsi` interface files are the reviewable API surface (`ds info` generates them; "if nothing in .dsi changes it is typically a safe refactoring"). Module bodies are pure compile-time declarations — no import-time execution, period; this one rule buys intra-package cycle tolerance, safe hot reload, and lazy dependency-driven loading. Script-entry exception (2026-11-10): in script mode only, top-level statements of the entry file are legal and compile into a synthesized `main` — Node and Bun execute entry-file top-level statements, and the charter's scripting goal points there for entry files only — while imported files and package members keep pure-declaration bodies, so the guarantee holds everywhere it protects. The block separator (re-spelled 2026-11-10) is a line-initial `##` at top level, the rest of the line being separator text; `///|` is retired and `///` becomes the reserved doc-comment prefix (design pending; trivia-only today). Top-level definitions are order-independent with or without separators, and a file without separators is one block.

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

Definitions are private by default; `pub(pkg)` opens them package-wide (including the package's own tests) and never enters the `.dsi`; `pub` opens them to dependents and enters the `.dsi`. Imports are symbol-selective with aliasing, conditional per target, and re-exportable via `pub import`; the spelling is unified on the sigil form — `import @std/csv (reader)`, a path with optional select/alias — and the quoted form is retired (2026-11-10):

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

**H5.5 — the std/builtin/platform layering (binding).** Three layers, each with a membership criterion. (a) VM core natives (the reserved `builtin` module): the irreducible primitives ds source cannot express — operations against VM internals (interned strings, the object store) that the lowering contract itself needs; pure or error-union-total, never world-touching (current members: the str_* conversions and the string ops). (b) `@std`: an ordinary ds-source package — everything expressible in DreamScript itself lands here (the D3 combinators arrive once generics/M6 allow), and script mode's bare-specifier imports resolve here. (c) Platforms: the world surface (console, fsys, net, time) as effect handlers per H5/E1, selected per build target; the `ds` CLI is the default platform provider. World-touching natives are the M3 stopgap form of platform handlers: the M3 single builtin module (natives 0-24) mixed the layers, and the native registry now carries two native modules — `builtin` (the pure core) and `std` (the default platform's console/fsys/net surface) — so a library embedder constructing a VM gets no world surface unless it installs a platform; the natives migrate to effect-handler form at M7 when the handler runtime exists (E1), with native indices frozen until then.

### H6 — hash-store core plus zero-config script mode; five mandatory tooling commitments

**Package stack.** `ds.pkg` manifests declare deps as `name = { ver, hash }` where hash is blake3 over the package content including its dependency hashes — identity flows transitively, no lockfile exists (the manifest is the lock). MVS resolution (pick the max of declared minima; multi-version coexistence by construction). A global content-addressed store keyed by output hash, doubling as the bytecode cache (artifacts are dsmod files). Bin-first per-triple artifacts verified against store hashes. Optional FHS profile linker (`ds link`; scoop shims on Windows, not symlinks). Registry ruling: the registry is a git repository holding package metadata only, configurable as a local path or a remote URL; dependency resolution reads the index metadata (name -> versions -> manifest hashes and artifact URLs), fetches artifacts content-addressed from the store, and verifies them against the blake3 hash cascade (the manifest stays the lock; MVS unchanged). Dependencies are never resolved by cloning or tracking a VCS working tree of the dependency itself — no branch heads, no mutable pseudo-versions; a git repository is the metadata transport, not the source of identity, hash pinning is. A local git index makes offline and private registries first-class.

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

```text
index/<a>/<ab>/<pkg>.json   # one metadata file per package: versions -> manifest hash + artifact URLs
git history                 # the metadata audit log — metadata only, never a dependency source
```

**Script mode.** `ds run script.ds` with zero config; bare-specifier std imports in the sigil form (`import @std/csv (reader)`; the quoted spelling is retired, G4); `ds save` freezes what the script used into a generated `ds.pkg`. Scripts that outgrow themselves gain a manifest without losing their file.

**Five mandatory tooling commitments, binding from day one.** (1) Compiler-as-query-library: the compiler is a library (`ds-check`) of memoized queries — parse, block_index, resolve, type_of, effects_of, linearity_check, lower — consumed by LSP, REPL, and hot reload alike; invalidation at `##` separator-block grain plus package grain (the second reason cross-package cycles are banned). (2) `ds fmt` with zero options, implemented as a compiler component; the block-aware formatter formats blocks independently and is also the LSP's on-type-formatting path. (3) Structured diagnostics with two-span linear errors — machine-readable suggestions, `--error-format=json`, spans computed at typed-AST level; the linear diagnostic class renders "moved here / used again here" pairs:

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

## 10. Staging (v0 → 1.0)

Ordered; each step lands on the previous one with no language-change rewrites.

1. Lock the two load-bearing contracts first: the slot-overlay call ABI (H2) and the fixed-width mmap-able dsmod format (BL-7, G3.5). Everything downstream compiles against them.
2. Checker core: B1(c) set algebra over nominal tags, B2 bidirectional plus refinement, B3(d) products/sums, B4(c), B7(a), B8(a), B9(a); affine checker plus DI (C1b, C5c).
3. Tier-1 interpreter: computed-goto or tail-call-threaded uop loop, unboxed slots, tag+payload ADTs, CALLT frame-reuse TCO.
4. Memory: Perceus dup/drop placement, reuse threading, per-frame arenas; runtime `rc==1` checks and the borrow counter as the diagnostic layer.
5. Concurrency: fibers, nursery, internal `cancel`/`discontinue` evidence dispatch, fork-join combinators.
6. Derives; then the comptime evaluator and reflection API (before 1.0).
7. Tooling commitments from day one, sequenced with the query compiler they consume: `ds fmt`, structured diagnostics, REPL, two-slot hot reload.
8. Tier 2 copy-and-patch (x86-64, then aarch64; tier-1-only elsewhere), bytecode-to-SSA lowering built early, tier 3 on demand.
9. Deferred-additive backlog after the core is stable (see §11); borrow inference, TRMC, and fusion upgrades land as pure optimization with zero language change.

## 11. Deferred-additive backlog and recorded fallbacks

Deferred additive (in headroom order of likelihood): `|>`, anonymous record literals (the map literal rides this reservation per B3), `@` binders, `??` on `t | none`; then `?.` desugar, `throws(...)` sugar, user-defined handlers (`with handler`), scoped named instances, labeled break/continue, `it` shorthand. Retired from this backlog: vararg/spread (F8); bare `return` was adopted into the surface 2026-11-10 (F2). Pure-optimization items (no language change): borrow inference, full reuse pairing, TRMC, fusion upgrades, `T | none` niche packing, rope. Recorded fallbacks if named risks materialize: B1(b) nominal-only core (if disjointness machinery slips schedule; error handling degrades to match-based); F1(b) Gleam/Roc brace headlines (if the arrow-assignment grammar fights the parser); H1(b) single method-JIT tier (if the per-arch stencil toolchain proves unacceptable); C4(b) generational references (if affine friction proves worse than expected).

