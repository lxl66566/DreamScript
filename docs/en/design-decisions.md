# DreamScript working design decisions

Status: the authoritative working record of every language decision (44 points: A1-A3, B1-B9, C1-C5, D1-D3, E1-E5, F1-F9, G1-G4, H1-H6), authored 2026-11-08 from the frozen `design/` archive and amended through 2026-11-13. Every Phase 4 task reads this file; `design/` is a read-only archive that is never modified and is read only when a rule here is insufficient. The seed corpus is ported to the decided surface in `conformance/`, with its runtime encodings pinned in ds-abi/ds-vm tests.
Source map, under `design/en/design/`: §2 `08-decisions.md`; §3 `01-types.md`; §4 `02-linearity-ownership.md`; §5 `03-effects-errors-async.md`; §6 `04-syntax.md`; §7 `05-generics-metaprogramming.md`; §8 `06-modules-tooling.md`; §9 `07-runtime.md`; per-point rationale `09-decision-rationale.md`; worksheet `00-decision-map.md`.

## 0. Author supplements (design/author_say.md) — binding

1. The main direction follows the Phase 3 record (08) and its rationale (09), extracted here.
2. Casing carries no grammatical meaning: no construct is distinguished by identifier case. Types are spelled lowercase (`int`, `string`, `list`, `file`); capitalized type names here and in the archive are catalog illustration.

## 1. The decided language (summary)

DreamScript is a standalone, embeddable general-purpose interpreted language: set-theoretic types over nominal tags checked bidirectionally with required boundary signatures and flow refinement, affine values with an opt-in `~Copyable` linear layer, Perceus RC with block reuse over per-frame arenas and no GC, coherent set-theoretic traits over one generic body, comptime reflection without macros, errors as union members handled by `?`/`.err`/`.ok`, VM fibers in mandatory nurseries with fork-join parallelism, an evidence-passing handler runtime with capability IO, and a three-tier execution pipeline (typed register VM, copy-and-patch, Cranelift/wasm) with guaranteed self and mutual tail calls. Its surface is TS-family braces with full UFCS, Rust-shaped matching and Kotlin interpolation under a 22-feature / 34-unit sugar budget, packaged as definition < file < package < module with `.dsi` interfaces, blake3 hash-cascade manifests, a zero-config script mode and linear C-ABI FFI.

## 2. Identity and compatibility

- **A1 — standalone core, embedding via platform packages.** The standalone runtime is the only architecture; embedding arrives as ordinary packages exporting effect handlers over it, the default platform being the `ds` CLI.
- **A2 — workloads.** CLI/tools/scripts primary, games/simulation secondary (interpreter-first startup, zero-config script mode, two-slot hot reload); numeric/AI reaches the tier-3 Cranelift path without shaping v1.
- **A3 — compatibility.** Rapid-break until 1.0, semver discipline after; content addressing applies at the package level (H6), never as source identity.

## 3. Type system

### B1 — hybrid set-theoretic types over nominal tags

Nominal declarations (tags, structs) carry identity; unions `|`, intersections `&` and differences `-` of nominal atoms are ordinary type expressions over them. Exhaustiveness is checked by tag identity, difference types erase at compile time, a nominal union is the unboxed tag+payload layout, and an intersection keeps the most precise member's layout. The disjointness/emptiness checker is load-bearing because B6 coherence reads it, and inference failure is a compile error with no `any` fallback. Recorded fallback: a nominal-only core (B1b) if the disjointness machinery slips schedule, degrading error handling to match-based.

### B2 — bidirectional checking with boundary signatures, plus flow refinement

Required top-level and public signatures anchor the set algebra; locals, lambdas and match arms are inferred bidirectionally. Refinement narrows monotonically through `is` tests, none tests and match arms and resets only at assignment. The `?` check is set containment against the written return union. Known limit: no narrowing through mutable fields of union-typed values. Ruling (2026-11-12): a value-level arm subtracts nothing from the remainder set, so later arms check over the full scrutinee type, sound because a branch checked on a superset may be dead, never wrong; the arm-ordering constraint is rejected with it. Per-element value-level refinement stays an open B1 extension in the checker queue.

### B3 — nominal structs/enums plus anonymous tuples

Named data is nominal structs and enums (declared layouts, one type_id each, tuple-enums allowed, record punning supported); ad-hoc aggregates are anonymous tuples with destructuring. Enum blocks are the only payload-case declaration form, `tag x;` declares unit tags, and type aliases never declare cases (2026-11-10). Construction is fixed per declaration (struct brace form, enum case call form, unit tag bare), a cross-spelling being a checker error. v1 adopts the list literal `[a, b, c]`; the map literal and anonymous record literals (compiler-synthesized sugar over a nominal record) stay deferred additive, builder calls interim. Row types are rejected for data.

### B4 — `T?` is exactly `T | none`

`none` is a unit type among several, so optionals, error unions and enums share one mechanism (`is`, match, and `?` with E4's extraction family). No `?.` in v1; `??` on `t | none` is the single recorded operator valve (F8). Niche packing is a legal later optimizer pass. Known limit: `V | none` where `V` can itself be `none` is ambiguous, so write an explicit wrapper at those sites.

### B7 — numerics: i64/f64 defaults, trapping overflow

`int` is i64 and `float` is f64, raw in unboxed slots; literals take `_` separators, `0x`/`0b`/`0o` forms and width suffixes (`42u8`, `2.0f32`); conversions are always explicit calls (`i32(u)`) with no implicit narrowing. Integer overflow, `1 / 0` and `int_min / -1` trap; `+%`/`-%`/`*%`/`-%a` wrap modulo 2^64 for hash and PRNG use; shifts trap past the bit width with rotations in the library; floats follow IEEE and never trap (`1.0 / 0.0` is `+inf`), while error-union arithmetic variants are library functions typed `t | arith_error`. `Dec` and units-of-measure are nominal library types plus derives.

### B8 — strings: immutable UTF-8, checked slices

Immutable UTF-8 bytes behind one raw pointer plus byte length. Slices are non-owning borrows kept alive by the parent's RC and land on char boundaries through a checked API returning `string | none`; `chars` iterates Unicode scalars, graphemes are stdlib, equality is O(n) with length fast-reject, and encoding conversion happens at FFI boundaries. String-operand `+` is the concat native's spelling (2026-11-12): the lowerer desugars `a + b` and `a += b` to the concat call shape, one operation with two spellings, mixed operands refusing at both layers. Known seam: a word staged as a native borrow dies at a later operand's last use before the CALL reads it (`b + "${b}x"` traps), pinned red in the ds-lower string_add suite until the placement pass grows a borrow-window stack.

### B9 — equality: universal structural `==` with five carve-outs

One structural `==` over all data, total by construction, never user-overloadable, no `===`. Carve-outs: function comparison rejected statically; floats compare bit-structurally (`nan == nan` true) with named `is_float_eq`/`is_approx_eq`; mutable cells and `~Copyable` resources compare by identity; operands are borrowed; `hash` stays consistent with it. `Ord`/`hash`/`show` are derives, ordering is never structural, and custom value semantics go through named methods.

## 4. Linearity, ownership, memory, data

### C1 — affine core, opt-in `~Copyable` linear layer

The everyday rule is one sentence: assigning an affine value moves it, and a moved binding is dead. `~Copyable` subtracts the default copyability constraint (a negated set constraint composing with B1 unions), carries `deinit` and admits phantom-state unions. Affine checking is an intraprocedural last-use/DI pass. Staging with no language change: runtime `rc==1` uniqueness checks plus an Inko-style borrow counter ship as the diagnostic layer; borrow inference, full reuse pairing and TRMC follow as pure optimization.

### C2 — second-class parameter-passing modes

Three modes are fixed at the definition site — default borrow, `mut` (exclusive mutable), consume — and call sites carry zero mode syntax. Borrows never escape a call, so checking is intraprocedural and no lifetime annotations exist; first-class references are permanently out. Subscripts extend the same modes to element access, replacing getter/setter protocols.

### C3 — `const`/`var` bindings, mutation through `mut` windows

`var` is a rebindable local; callee-side mutation goes through `mut` parameter windows — one mechanism serving parameters, windows and confinement (D1). Inside a window there are zero runtime checks (alias-freeness is proven); outside it everything is shared-immutable. FBIP reuse makes functional updates run in place when the value is unique. No user-facing interior-mutability cells exist.

### C4 — Perceus precise RC + block reuse, plus per-frame bump arenas

Precise non-atomic RC: `rc_inc` at each explicit clone, `rc_dec` at each unique death, each drop threading to the next allocation as a reuse candidate (`kk_block_alloc_at`, mimalloc fallback at ~15-25 cycles). Per-frame bump arenas hold temporaries — allocation is a pointer add, reclamation a restore on frame pop, sound because linearity makes escape a compile-time fact. Zero pauses, zero write barriers, no safepoints or GC maps in any tier. Cycles are rejected at the type level for v0, with the two-hook collector interface (cycle-candidate registration, weak refs) committed in the header format as the growth path. Non-atomic counts are sound while values are thread-confined, which E3's fork-join transfer maintains. Executor principle (2026-11-12): every count mutation runs through the owning heap's own count funnels, a foreign heap only posting requests drained to quiescence; a completed fiber probes the claim ledger at exit and walks the drain only when it is non-empty.

### C5 — `deinit` restricted to `~Copyable` types

`deinit` runs exactly once at the unique instance's death on every path, including early return, panic and cancellation teardown, and dispatches statically. Definite-initialization checking is infrastructure under everything. Explicit `drop` exists as a debug/FFI tool; users never write routine drops.

### D1 — exclusion-window mutation is the only channel

Mutation is always a named call on an owned receiver with a `mut` mode: statically exclusive, lexically visible as the call itself, zero runtime checks. Shared mutation is unrepresentable by construction.

### D2 — linear-mutable COW collections

Collections expose a functional API and mutate in place iff uniquely owned (guaranteed, observable reuse); aliasing means copying, surfaced as a perf note and never an error. Persistent HAMT-style structures are ordinary library types, and `bytes`/`vec` are the unboxed unique members of the same model. Mut-window exclusivity for copyable receivers (2026-11-10): a `mut` window parameter takes a `var` binding at the call site whenever its argument copies, and a `const`-binding receiver is DS3006 with the binder as the secondary span; `~Copyable` receivers keep their linearity-proven exclusivity. COW-on-alias is rejected because a hidden copy keeps the mutation invisible at the call site and withdraws guaranteed in-place reuse, so spell the binder `var` where mutation is intended.

### D3 — combinators over a hidden linear cursor, compiler fusion

The user surface is combinators (`map`/`filter`/`reduce`) plus `for`; the hidden protocol is a linear cursor, and the compiler fuses chains into loops while non-fusable shapes fall back to the same cursor semantics. Subscripts extend in-place element access. No public generator syntax in 1.0.

## 5. Errors, effects, async, parallelism

### B5 — errors are union members of return types

Errors are storable data, one predicted branch in the VM, carrying the information of an effect row. A `throws(...)` spelling stays later sugar over the same union. Propagation requires the enclosing return union to contain the member (checked containment, no implicit conversions).

### E1 — no effect syntax in v1, handler-shaped runtime, capability idiom

The runtime is handler-shaped from day one: evidence passing serves `cancel` and `discontinue` at nursery boundaries, tail-resumptive with zero allocation, and user-facing paths are unchanged. World-touching stdlib APIs follow the capability idiom — an ambient default handler installed by the `ds` CLI, an explicit capability parameter for tests, embedding and scheduler choice. User-defined handlers stay a v2 additive escape hatch, with standing obligations: resumptions affine unless marked, and FFI calls pin the fiber (H5).

### E2 — VM fibers with a mandatory nursery

Stackful heap fibers on the VM pool; spawn is one allocation, switch a pointer swap. `spawn` exists only inside `nursery` blocks; scope exit joins children, first failure cancels siblings, and child errors union out at the boundary. Blocking-style code, no colors, no `await`; preemption is a flag check every N instructions or on backward jumps; TCO is per-fiber; bare detach requires an explicit root nursery or supervisor. Builtin indices 41-44 carry the surface (nursery open, spawn, boundary, cancel), a spawn body lowering to a synthesized child function riding the lambda capture machinery, and the containment mask is the enclosing return union's member tag bits, an uncontained cancelled payload trapping in the VM. Amendments (2026-11-12): re-entry of a parked child by the join/teardown driver is preemption-shaped, so a parked child of a cancelled nursery dies with the cancel sentinel to its resumer instead of resuming (no yield budget; a first entry is not a resume); a payloaded cancel rides the native channel's flat span (tag word plus payload words in declaration order), widening the containment mask and the boundary's answer span. Lowering refuses bare `spawn` outside a nursery, a member outside the enclosing return union, a payloaded member whose payload is a union or non-heap application, and `return`/`?` inside a nursery or spawn body.

### E3 — fork-join parallelism on the sendability substrate

Race-freedom and sendability are static consequences of linear/immutable data (a linear value is `sending`, an immutable value shareable), so only the scheduler was missing. `par_map`/`par_reduce` over immutable or linear-partitioned inputs are deterministic, replayable and lock-free by theorem, with RC counts thread-confined through the fork-side ownership transfer. Amendment (2026-11-13): the join parks the CALLG site through the interlock drive — bodies spawn only, the drive seam carrying the assembly/fold stage, the answer delivering onto the parked site — so nested combinator calls run correct at any depth (drive frames pinned at 2048 levels), while a fork at a tail dispatch site (CALLTG) refuses with a structured trap. The backend is decided by measurement (2026-11-13, `crates/ds-vm/benches/fork_join.rs`): the sequential interlock drive stays M5's backend at ~252 ns per child, against ~147 ns per element for youpipe and ~19 us for std scoped threads, so a parallel tier pays its dispatch only for heavy element bodies. Actor and supervision layers are later libraries.

### E4 — union errors with `?`, `.err`/`.ok`, extraction family; `or` retired 2026-11-10

Expected failures are union members handled by tokens and methods stated once over unions: `?` (propagation with checked containment), `.err(h)`/`.ok(h)` (difference-typed handlers whose remainder type shows what is left), and the @std extraction family `unwrap_or(y)`, `unwrap_or_else(fn)`, `ok_or(e)`, `map_err(fn)` — plain UFCS-callable generics with fully local typing. `or` is retired (2026-11-10) for overloading one word across fallback, early exit, loop control and coalescing under wanted-member inference (F8), and `ok_or` collapses to `unwrap_or`'s shape when the err type sits inside the extracted side. Early exit is `if x is none { return e }` with refinement plus bare `return`. Bugs `panic` unhandleably: the fiber aborts, linear deinits run in the frame walk, the trap surfaces at the nursery or supervisor boundary; let-it-crash supervision is a library pattern.

### E5 — nursery-scoped cooperative cancellation plus consume-to-close

Cancellation is nursery-scoped and cooperative, observed only at await/preemption points (taken backward branches, plus the parked-child re-entry since the 2026-11-12 amendment), so a linear consume is never interrupted mid-flight; teardown walks frames running deinits. Resources close by consumption (a static move). `defer` is non-normative scope-exit sugar running at teardown under cancellation. Timeouts, `firstof` and `interleave` are library combinators over the timer capability.

## 6. Syntax surface and sugar budget

### F1 — TS-family braces

Every block opens with a visible token; line-oriented grammar with no layout state; newline-terminated statements without semicolons; trailing commas everywhere; `if` and `match` are expressions. Recorded fallback: Gleam/Roc-style brace headlines if the arrow-assignment grammar fights the parser.

### F2 — bindings, functions, annotations, returns

`const`/`var` bindings, `var` the rebindable local. `fn name(p) -> t { }` is the only named-function declaration — top level, trait/impl members, bodiless in trait signatures — and the name is in scope in its own body; const-arrow remains only for anonymous lambdas `(x) => e`. `unit` optionally names the unit type. Parameters `name: t`, returns `-> t`, union returns `-> a | b`, `T?` in every annotation position. Final-expression returns; early exits via bare `return` (2026-11-10) and `break`/`continue`; `?` is postfix propagation only, the prefix-`?` marker and the `or` operator being retired (E4/F8). Functions are values.

Callable-word ruling (2026-11-10): a fn-typed value is a slot classification (`SlotType::Fn`), one word, never boxed. A non-capturing function value is a copyable scalar FuncRef (module id plus index, copied by MOV, zero RC); a capturing lambda is a heap closure object holding the capture block inside, so the slot holds one Ref word under C4. Captures follow the ordinary binding rules, no mutable upvalue channel existing in v1. The call is CALLS (call-major minor 0xA.6), a call whose callee operand is a frame slot — the ISA's one dynamic-dispatch point — typed by that classification; hot reload needs nothing new since a FuncRef carries module id and index and resolves through the two-slot module table. Construction is CLOSURE_NEW (Ref-major minor 0xD.3): destination Ref slot, the KCONST-minted FuncRef, the capture count and capture words staged at overlay positions, with one count per Ref capture acquired by the object and one more per CALLS copy-in.

Callable-defaulting amendment (2026-11-11): a fn-typed parameter keeps the `Fn` classification while the arriving word may be a scalar FuncRef or a closure object's Ref word, the call readers resolving it by runtime kind. Staging is a borrow window (C2), so the callee never owns the count while each copy-in acquires one per `Ref` capture that the callee's placement balances. Boundary: a closure word never widens into a var span, is never captured by a lambda, and never enters a collection element or struct field without proven scalar provenance; a native or generic callee's name mints no callable word.

### F3 — full UFCS

`x.f(a)` desugars to `f(x, a)` for any first-argument function, at zero runtime cost and with no candidate search. Resolution is free functions first, then trait methods, with `Trait::f(x)` as the escape hatch and true ambiguity a compile error. Capability parameters are excluded from the dot position, so `println(io, msg)` cannot hide `io`. `|>` is deferred additive.

### F4 — Rust-shaped pattern matching

Arms `pat => expr`, guards `pat if cond`, or-patterns `a | b`, destructuring, `_`, and exhaustive matching with set-difference diagnostics naming the missing members; `@` binders are excluded from v1 (deferred additive). Struct patterns (2026-11-12) test each literal field in place over a non-union scrutinee and bind fields at consecutive words, composing into the or vocabulary and under guards (a Ref-carrying scrutinee's guarded arm binding over a copy), while a union member's struct payload binds at the region's claimed offsets. Tag-dispatch tests (2026-11-12): a member arm may carry value-level tests over its own payload words, a failed test sliding to the next arm's dispatch entry and re-running its tag tests over the full scrutinee (the B2 remainder rule's tag-dispatch form), so a tested arm needs a later no-test arm or catchall and tested or-alternatives must name one member. Nested struct sub-patterns (2026-11-12) compose at every match seam at any depth, tests running at the field's own offsets with binders as borrowed aliases there, while nested tuple and member sub-patterns stay refused. Guards (2026-11-12) run after the section's member tests and their failure edge composes with the slide (transients dropped at a cleanup label, then a jump to the next arm's dispatch entry); a guard must not take a count the base state holds, a can-fail arm needs a later settling arm or catchall, and a guarded catchall refuses. The first catchall wins, and the checker's remainder walk implements the B2 ruling's letter.

### F5 — fixed trait-driven operator set, frozen precedence

A fixed token set maps to fixed traits (Add, Sub, Mul, ..., Index), so an operator call is an inline-cacheable trait-method call. One frozen C-family precedence table is published once in the tutorial and never extended; no user-defined operators exist; the word forms are exactly `in` and `is`; boolean contexts are bool-typed with no truthiness coercion; `Deref`-style coercions are excluded. `==` is B9's and never overloadable, ordering comes from the `Ord` derive, and trap/wrap behavior is B7's.

### F6 — strings and literals

Interpolation `$x` (bare identifier) and `${expr}`; the hole is an explicit string context, legal when the operand is a primitive (int, float, bool, char, string) or carries `show`, while a union or optional operand is an error (extract first per B4/E4). Every other position stays strictly explicit. Escape set (2026-11-10): `\n \t \r \\ \" \' \0 \u{...}`; raw strings `r"..."` and hash-counted `r#"..."#` nest to any depth and carry no escapes. `'x'` is a Unicode scalar char, `b'x'` a single byte; `byte` is its own atom in the u8 width class outside the int conversion family, declared by the stdlib ambient (`tag byte`) and lowered through the integer slot encoding (2026-11-10).

### F7 — sugar budget: Balanced tier (22 features / 34 units)

Against Kotlin's 38 features / 74-76 cost-unit ceiling, with 16 features / 40-42 units reserved as headroom. Included rows: newline-terminated statements S1; interpolation S1; raw strings S1; list literals S1 (adopted 2026-11-10); postfix `?` S2; `is` narrowing S2; `.err(h)`/`.ok(h)` S2; match/if expressions with exhaustive-union diagnostics S2; match guards S1; or-patterns S1; destructuring S2; named + literal-only default arguments S2; trailing lambda S2; UFCS S2; type aliases S1; sum-type declaration `tag`/enum S2; derive attribute S2; ranges `..`/`..=` + `in` S1; numeric suffixes + separators S1; `const`/`var` split S1; operator-overload traits with fixed set and precedence S2; `T?` alias S2. Deferred-additive rows in headroom order: `|>` S1, anonymous record literals S2 (the map literal rides this reservation per B3), `@` binders S1, `??` on `t | none` S1, labeled break/continue S1, lambda shorthand `it` S1. Kotlin features deleted wholesale by set typing and UFCS: `?.`, elvis, `!!`, smart casts, infix and extension functions/properties, data-class synthesis, sealed hierarchies, receiver-lambda DSLs, primary constructors, reified type parameters, backtick identifiers, `tailrec`, context parameters. Also excluded permanently: custom operators, Deref coercions, convention-based privacy, truthiness coercion, chained comparisons, comprehension syntax, subscript slicing sugar and vararg/spread packing (F8).

### F8 — extraction methods, no prose operators or implicit coercions (2026-11-10)

The `or` operator is retired (2026-11-10): it carried six behaviors — option fallback, error-union fallback, early exit with a new value, fallback onto a diverging callee, union coalescing, loop control — under contextual wanted-member inference, and collided with boolean `||`. Replacements use existing tokens plus one @std family: `x.unwrap_or(y)` and `x.unwrap_or_else(fn)`; `x.ok_or(e)?` and `x.map_err(fn)?`; `x.err(h)`; `match done { p => p, none => last }`; `if x is none { break }`. Bare `return` enters with the retirement, and `??` restricted to `t | none` is the single recorded deferred additive (S1) if corpus evidence demands an operator; no English word ever re-enters the operator set.

Prose-syntax audit, binding at review: operators stay C-family symbols with `in` and `is` the only word forms; boolean contexts (`if`/`while`, `&&`/`||`/`!`) are bool-typed with no truthiness coercion anywhere; chained comparisons and comprehension syntax are excluded permanently (D3 covers comprehension); subscripts are single-element Index access with no slicing sugar (B8 owns string slices); vararg/spread packing is retired from the backlog. Mutable defaults are impossible because defaults are literal-only, and mixed numeric arithmetic is an error naming the conversion (B7). `in` is range-wired in v1 (2026-11-10): with collection membership unruled, the checker refuses a non-range right side instead of typing it bool.

Guard-tail quirk (2026-11-12): a guard whose comparison's right side is a bare identifier directly before `=>` reads it as a bare-parameter lambda, so the guard needs parentheses; a targeted parser diagnostic names the collision and the parenthesization.

### F9 — two namespaces: types and values (2026-11-10)

Types and values resolve in two namespaces: constructors (brace construction, bare tags, generic application like `list[page]()`) in the type namespace and functions in the value namespace, a call-form name present in both being an ambiguity error. The `map` struct and `map` function coexist; an enum case colliding with a function is the residual error class.

### 6.1 Implementation-normalized surface spellings (corpus-adopted; to be locked at M2)

Spellings the corpus uses where the record under-specifies the surface, adopted for implementation and locked or revised at M2: trait members spell the receiver annotation-free as `self` (borrow) or `mut self` (exclusive window, C2) while other parameters may use `Self`, `self` as a type name being retired (2026-11-10); derives are attributes `#[derive(hash, show, ord)]` (F7); generic values are `struct stack[t]` with functional `push`/`pop` (D2); function generic clauses are `[t: c]`; empty mutable collections construct as `list[page]()`; `with handler` stays out of v1 (E1).

Parser rulings: generic application is canonically `list[int]`, with block-bodied members, match arms and struct/enum fields newline-separated and trailing commas accepted (F1); parameter modes live inside the annotation with the `mut self` shorthand, `move` and `comptime` being contextual keywords; trait/impl members are `fn` declarations, bodiless in traits; tuple types `(a, t?)`, foreign types `c["sqlite3"]` and the `is` right side parse as type forms; ordinary strings are single-line and block comments do not nest; top-level `fn` is the only named declaration, top-level expression statements being rejected per G3 outside script-mode entry files; `none`, `self` and `Self` are not reserved; identifiers are ASCII at M1. Audit round 3: `in` binds at comparison level (F5) with a bare range on its right; a lone `\r` is rejected in strings, holes and char literals; impl members are `fn` declarations only (DS1010); named arguments follow positional ones (DS1011) and signatures take literal-only defaults (DS1012); flat operator chains cap at 128 terms (DS1009).

## 7. Polymorphism and metaprogramming

Trait-member default authority (2026-11-12): the trait declaration is the single authority for a defaulted member parameter, so an impl re-declaring one is a checker error and a dispatch call stages the trait's declared default; an impl-only default refuses (DS2020) because the registered signature carries none. Inherent members keep their own body defaults.

### B6 — coherent set-theoretic traits with pairwise-disjoint instance heads

Traits may carry associated types; instances are declared for type heads; the only coherence rule is pairwise disjointness of heads of one trait — for every pair prove `H1 & H2 = Empty` (decidable set emptiness), checked at declaration and again over the final instance environment at link time. Any module may define an instance for any type, foreign included: no orphan rule, no newtype wrappers. Resolution is intersection subtyping via modus ponens with no priority machinery, and an overlap error names both heads and a witness type in their intersection. Instantiation (landed 2026-11-12): a declaration probes whether some grounding of either head is served by both, so two heads covering one atom are rejected with that instantiation as witness, while a pair no probe could prove is caught by the closure pass over the final instance environment (once per package, first-error-only). A conditional instance requiring itself is rejected by a structural cycle check. Union heads decompose into per-branch instances, and pairwise disjoint heads need no cache tie-breaking; scoped named instances stay deferred additive, ambiguity always being an error.

Call-site resolution (landed 2026-11-11): a query unifies against a generic head with each receiver member covered under its own substitution and each declared bound proven recursively (depth-capped). Several serving impls are true ambiguity, a matched head with a failed bound is diagnosed as the unsatisfied bound, and a site covered only by an in-scope assumption records nothing. An impl member must agree with its trait member's registered signature (DS2018, both sites, parameters substituted positionally), because that signature is the call-site contract; a trait member with no default body must arrive with a bodied impl member (DS2023), both incomplete shapes refusing at the impl's door. A trait's own members see `Self` as satisfying their trait.

### G1 — comptime evaluation + static reflection, no macros

The meta-language is the language: comptime evaluation of ordinary code plus static type reflection; macros of every level are rejected, so hygiene is deleted structurally. Derives ship first (`hash`, `show`, `ord` with an explicit `total_compare` for float sort keys, `clone`, `to_json`/`from_json`, `default`), and `==` is universal and never derived. The comptime evaluator and reflection API land before 1.0.

### G2 — comptime-only reflection over tags-as-data

Reflection exists only at comptime; runtime type questions are answered by the tags data already carries (`is`, match). No type objects or runtime metadata tables exist. Existentials remain the one justified boxing site (`box Json`, an evidence-carrying value plus its dictionary) for heterogeneous collections and plugin boundaries, and signatures default to universals because existentials break affine reasoning.

### H3 — one generic body, evidence words, inline caches, lazy JIT specialization

Every generic function exists once in bytecode with its constraint list. A generic call site emits `CALL_GENERIC fn, evidence_words`, each word a statically-resolved instance constant or a runtime-resolved tag-keyed slot patched as a 1-2 entry inline cache, a miss falling back to full lookup so correctness never depends on cache state. The call major 0xA reserves minor 4 for CALLG (operand-identical to CALL) and minor 5 for CALLTG (H2's 2026-11-12 ruling); evidence words are ordinary register arguments staged after the true arguments, so tail-position generic calls tail-call normally. The JIT tier lazily materializes per-type specializations keyed by the type vector, with no correctness role. Ruling (anti-specialization invariant, verbatim): "dispatch is determined by types and the set algebra, never by declaration order or priority; the runtime may cache and specialize any dispatch that is already unique." Variance follows set inclusion: immutable containers and read roles covariant, write roles contravariant, both-role fields invariant. No HKT and no GATs in 1.0. Free-generic-fn amendment (2026-11-13): a free generic fn's body binds one Raw word per own type variable and each call site stages demands for its declared bounds after the true arguments, while a statically-resolved local callee emits the ordinary CALL or CALLT.

## 8. Modules, visibility, FFI, ecosystem

### G3 — definition < file < package < module; no import-time execution

Four layers: definitions (visibility and identity), files (namespace spelling), packages/directories (the compilation unit, one namespace per package with zero intra-package imports), and the module (`ds.pkg`: dependency and distribution unit). Intra-package cycles are legal (whole-package fixpoint resolution) while cross-package cycles are a link error suggesting a base package; generated `.dsi` files are the reviewable API surface. Module bodies are pure compile-time declarations with no import-time execution, which buys cycle tolerance, safe hot reload and lazy loading, and script mode alone admits top-level statements in the entry file, compiled into a synthesized `main` (2026-11-10). The block separator is a line-initial `##` and `///` is the doc-comment prefix.

Doc attachment (2026-11-12): consecutive `///` lines immediately preceding a definition attach to it as parse-layer data beside the leading trivia, while a run separated by a blank line or anything else stays loose trivia without diagnostic; `////` is an ordinary comment. A run preceding a trait/impl member `fn`, a struct field or an enum-case payload field fills that member's own doc slot, taken by the formatter as its member-grain placement input; associated types, `deinit` members and enum case names carry no slot.

**G3.5 — the dsmod bytecode contract (binding).** Fixed-width, position-independent, mmap-able module format:

```text
bytes 0..3  magic "\0DSM"       byte 4 format version     byte 5 target class (bytecode | wasm | triple)
byte 6      flags (STRIP | FFI-USED | HAS-JIT-METADATA)
uleb        module name         uleb deps: [(name, content-hash)]...
uleb        source hash (blake3-128)   # PEP 552 checked-hash validation, never mtime
sections    blocks | constants | type metadata | effect rows | debug spans (strippable)
```

Cache at `$DS_CACHE/ir/<triple>/<blake3-of-source+deps>.dsmod`, mmap'd on the hit path with lazy loading driven by the header's dep list; `ds build --exe` produces a single binary embedding the dependency closure plus VM, and startup targets sub-10ms cold via the store.

### G4 — private / `pub(pkg)` / `pub` plus selective imports

Definitions are private by default; `pub(pkg)` opens them package-wide and stays out of the `.dsi`, `pub` opens them to dependents and enters it. Imports are symbol-selective with aliasing, conditional per target and re-exportable via `pub import`, spelled in sigil form (`import @std/csv (reader)`, `import acme/pipeline/fs if os == "windows"`), the quoted form retired (2026-11-10). The `.dsi` is the visibility gate, the API-review artifact, the LSP hover source and the incremental-compilation boundary. The `pub(in ...)` ladder is left out to stay under the sugar ceiling.

### H5 — linear C-ABI handles as substrate, platform packages layered on

Foreign resources are opaque linear handles (`~Copyable`): double-free is a compile error, extern borrows are call-scoped raw pointers the type layer forbids from escaping, callbacks run on VM-managed stacks, and `ds bind header.h` generates typed externs. The embedding API is a Lua-shaped handle table where a `Value` indexes the VM's object table, two embedders being two independent VMs. Objects never move, so pinning is a lifetime question — a blocking foreign call pins its fiber (carrier-thread execution the escape) and cancellation is observed only at VM re-entry. Platforms are ordinary packages exporting effect handlers, selected per build target (`platform "std" provides <console, fsys, net, time>` default, `"web"`, `"embed"`), and the checker verifies effect usage is a subset of the platform's declared row.

**H5.5 — the std/builtin/platform layering (binding).** Three layers with membership criteria: (a) VM core natives in the reserved `builtin` module, the irreducible primitives ds source cannot express — pure or error-union-total and never world-touching; (b) `@std`, an ordinary ds-source package holding everything expressible in DreamScript and resolving script mode's bare specifiers; (c) platforms, the world surface as effect handlers per H5/E1. Platform-seam amendment (2026-11-13): the world natives migrated out of the M3 stopgap registry, retiring the `builtin`/`std` two-native-module split so the pure `builtin` core is the only native module of record while world ops live in the platform ABI's dense table (0-11). A world call's FuncRef addresses the source's `platform "..."` declaration (absent declarations lowering against `std`), and the host installs a handler there whose capability subset serves the table, install-time validation refusing a call site whose capability is unprovided. Per definition the checker attributes the capabilities its ambient call edges reach and requires each to be provided; an extern with a declared world row licenses on any world-bearing platform, and world usage without a platform declaration refuses. The endgame handler's encoding waits on M7, and `https` stays rejected until the TLS tier is ruled.

### H6 — hash-store core plus zero-config script mode; five mandatory tooling commitments

**Package stack.** `ds.pkg` manifests declare deps as `name = { ver, hash }`, hash being blake3 over content including dependency hashes, so identity flows transitively and no lockfile exists. MVS picks the max of declared minima, multi-version coexistence coming by construction. A global content-addressed store keyed by output hash doubles as the bytecode cache (dsmod artifacts), with bin-first per-triple artifacts verified against store hashes. The registry is a git repository of metadata only, local path or remote URL: resolution reads index metadata, fetches artifacts content-addressed and verifies them against the hash cascade, and never clones a VCS working tree of the dependency itself, since hash pinning is the identity and git only the transport. Manifests carry `[artifacts]` URLs and an optional `[build]` script run by the embedded VM with `#OUT#`, `#TMP#`, `#ARCH#`.

**Script mode.** `ds run script.ds` with zero config, bare-specifier imports in sigil form (G4), and `ds save` freezing what the script used into a generated `ds.pkg`.

**Five tooling commitments, binding from day one.** (1) Compiler-as-query-library: memoized `ds-check` queries (parse, block_index, resolve, type_of, effects_of, linearity_check, lower) shared by LSP, REPL and hot reload, invalidated at `##` grain plus package grain. (2) `ds fmt` with zero options, a compiler component whose block-aware formatter also serves the LSP's on-type formatting. (3) Structured two-span linear diagnostics (DS3001) with machine-readable suggestions and `--error-format=json`. (4) Linear-aware REPL reporting inferred type and effect row with `:drop`/`:reset` purging so prompt values are never silently dropped. (5) Erlang two-version code slots from day one: per-module current+old slots, `ds --hot` recompiling and swapping, old frames retiring at their next fully qualified cross-module call, and handlers capturing their install-time version.

## 9. Runtime

### Fixed baseline (BL-1..BL-7, binding on every tier)

- **BL-1 Typed register bytecode.** 32-bit fixed-format instructions with register-index operands and typed opcodes (ADDI64/ADDF64, EQI64/EQF64, KIMM/KCONST), no dynamic "V" variants since every operand's type is a compile-time fact; biased jump offsets (BCBIAS_J) and XOR-negated comparison pairs (ISEQV^1 == ISNEV).
- **BL-2 Unboxed slots.** No NaN-boxing, no per-slot tags, no boxed scalars: i32/i64/f64/bool raw in frame slots, references raw pointers, ADTs/unions/Result as unboxed tag+payload pairs across consecutive slots.
- **BL-3 Dispatch.** Computed goto or C tail-call threading (`musttail` + `preserve_none`), a build-portability choice.
- **BL-4 Superinstructions on profile data.** The assembler composes fusions (compare-and-branch, load-op, dense-tag match jump tables) from measured opcode frequencies.
- **BL-5 Uop decomposition from day one.** The instruction set is micro-ops with explicit slot effects and `pure` annotations, so the copy-and-patch stencils are these uops one-to-one.
- **BL-6 Frames inline in a typed value stack.** Each frame is a contiguous span of statically-typed slots plus a metadata word (return address, frame size, TCO-chain flags); static layout makes TCO a pointer shuffle.
- **BL-7 Position-independent, mmap-able dsmod.** Fixed-width, index-based, relocation-free bytecode with module header (version, type/constant tables, per-function frame-slot types, frame sizes, max call depth) and an IR-version checksum, contracted in G3.5.

### H1 — three tiers over one shared lowering

Tier 1: typed register VM, computed-goto uop loop, unboxed frames, target 3-10x of C. Tier 2: copy-and-patch stencils — one x86-64 fragment per uop emitted at build time from a master template, holes for frame-slot offsets and constants patched as u16/u64, ~14% faster than LLVM -O0 at ~100x its compile speed, target 1.5-3x. Tier 3: bytecode -> typed SSA -> Cranelift CLIF, or wasm 3.0 (`return_call` carries TCO), target 1-2x native / 1.5-2.5x wasm. One uop set and one bytecode-to-SSA lowering is shared by all tiers and built early. Tier transitions are in-place J-variant opcode rewrites, and OSR reduces to compiling the loop body. No guards exist on any data path: the only exits are debugger breakpoints, the per-entry stack-limit compare, effect-handler entry and B7's cold overflow edge, hence no deopt cliffs. Short scripts (<0.2s) never pay tiering, and platforms without verified stencil builds ship tier1-only. Static typing deletes the dynamic-tiering machinery from JIT code: no hidden classes, field-access ICs, type guards, deopt frames, speculation recording or side traces, leaving plain hotness counters plus type-id-keyed ICs at dynamic call sites as perf hints only. Recorded fallback: a single method-JIT tier (H1b) if the per-arch stencil toolchain proves unacceptable.

### H2 — guaranteed TCO from day one, self and mutual

Spec-level guarantee: frame reuse plus argument slide in tier 1, label loops and slot-overlay `jmp` in JIT tiers, `return_call` on wasm. The slot-overlay ABI is locked at frame-layout time: callee argument slots overlay the caller's outgoing-arg area, so a tail call is at most an O(delta) arg slide plus a jmp and the stack never grows at any depth in any tier. Backtraces stay honest via tail-chain metadata with lazy materialization on debugger request, and the guarantee covers tail calls within the same effect context. Cross-tier tail calls go through a two-word callee entry descriptor, one word flipping when a callee later compiles (the hot-reload invalidation story). Tail-looping spawned fibers never grow their segment, fiber growth being a chunk-list append, and a tail call moving arguments transfers ownership with zero RC traffic. This ABI and BL-7 are the two decisions to lock before anything else. Guarantee closure ruling (2026-11-12): the guarantee admits no callee-class exception — CALLTG (call major 0xA minor 5) is committed, so dispatch tails transfer through the same frame-reuse slide with instance/evidence words staged after the exit claims and the tag-keyed inline cache keying CALLTG identically; a count-free closure callee word transfers through CALLT by maybe-Ref tracking. Landing precedes M8's differential tiers.

Queue-closure rulings (2026-11-12): destructured `const` binders alias the source `var`'s words, so reassignment through the var is observable through the binders. Native fn values never mint a callable word in 1.0. The future fetch native answers both ambient `net_err` cases, mapping the platform's single NET_ERR discriminant onto timeout/refused by error class.

### H4 — Perceus RC + reuse + per-frame arenas, collector interface as growth path

Per-frame bump arenas for non-escaping temporaries (mark/save at entry, bump alloc, restore on every pop path). Perceus precise non-atomic RC with reuse threading for boxed/shared values. Zero pauses, zero barriers, no safepoints or GC maps in any tier; deterministic frees feed deterministic deinits, and per-fiber arena marks make fiber switching free. Honest accounting: 100% of non-moved temporaries are register/arena-resident, the boxed/shared residue keeps paying RC, and the reuse pass is budgeted as optimizer work with the mimalloc fallback. The two-hook collector interface (cycle-candidate registration, weak refs) is committed in the header format now as the documented growth path for shared cyclic workloads (trial deletion off-thread; never applicable to linear data, which cannot cycle).

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
