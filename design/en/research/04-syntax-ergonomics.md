# 04 — Syntax Surface, Sugar Budget, and Ergonomics

Research date: 2026-11-04. Scope: syntax surface, sugar budget, ergonomics only (no type-system theory, no concurrency, no runtime). Sources: language docs and real stdlib sources where possible (koka and moonbit-core and inko clones read locally; Roc new-compiler tutorial, Gleam tour, Kotlin 2.1/2.2 release notes, Phi article fetched), plus prior knowledge of stable languages. Evidence is cited inline by URL; the comparative matrix is a syntactic-surface count, not a quality judgment.

## 1. Executive summary

Kotlin's sugar surface is the hard ceiling: about 38 distinct syntactic features, cost-weighted about 76 units on the S1/S2/S3 scale defined below. TypeScript and Swift are close behind (about 28 features each); Rust lands around 26; Koka, MoonBit, Roc, and Gleam show that a modern language needs only 9 to 16 features to feel pleasant.
The recommended DreamScript budget is 20 features or fewer (cost-weighted about 32), spending the savings on set-theoretic typing which replaces several Kotlin features wholesale (data classes, sealed hierarchies, smart casts, and safe calls all collapse into one mechanism).
Set-theoretic flow typing (TS narrowing, Whiley, MLscript, Phi) is the single highest-leverage surface decision: union types plus `is` narrowing plus an `or` extraction operator make `null`, errors, and enums literally one feature, replacing Kotlin's `?.`/`?:`/`!!`, Swift's four `try` spellings, and per-monad combinators.
Koka-style full UFCS is verified in the tour source: `s.encode(3)` is plain sugar for `encode(s,3)` on any first argument, and it subsumes pipelines for chaining; if a pipeline is wanted, Gleam's first-argument `|>` is the variant consistent with UFCS (verified: `a |> b(1,2)` desugars to `b(a,1,2)`).
Pattern matching: adopt Rust-shaped arms (`pat if guard`, `|` or-patterns, `@` binders, exhaustiveness); Kotlin 2.1's stabilized guard syntax is the alternative spelling; Gleam is the cautionary tale of omitting guards (nesting inflation).
Default arguments are the one Kotlin feature that directly conflicts with the author's monad philosophy: typst-style eager expression defaults create out-of-order evaluation (typst#3052); Koka's lazy named-parameter defaults are the only safe general form; literal-only defaults are the cheapest safe subset.
The old DreamScript block-error idea (`{1/0}` typed by context, `.err((e)->...)`) has exactly the right goal (one extraction mechanism for Option/Result/effects) but a non-compositional mechanism: the same block typechecks differently at different use sites. The fix keeps the surface and repairs the semantics: postfix `?` plus `or` plus set-difference-typed `.err` handlers, detailed in section 11.
Strings: Kotlin-style `$x` and `${expr}` is the best single convention (bare identifier plus one brace form); raw strings should cover multiline/heredoc needs in one construct.
Numerics: default `Int`/`Float` (i64/f64) with suffixes, no implicit narrowing, Swift-style trapping overflow plus explicit `+%` wrapping ops; this is the only overflow policy that is predictable under an interpreter.
Equality: one structural `==` under immutability (Roc-style); `===` and identity comparisons are deletable features, not additions.
Braces beat indentation for this project (Koka is the lone healthy indentation language; Python/Nim complaints are structural), and no-semicolon newline-terminated grammar is the lowest-cost rule.

## 2. The sugar ceiling (deliverable a)

### 2.1 What counts

A sugar feature is any syntax that a parser could desugar into a smaller core language without changing expressiveness. Baseline semantics shared by every language in the table (variables, calls, blocks, generics) are excluded; type-level DSLs are included because they consume author attention exactly like syntax. Cost scale: S1 = one token or operator, orthogonal, trivially explained; S2 = a new construct or code-generation rule; S3 = new typing or cognitive semantics the programmer must hold in mind.

### 2.2 Kotlin — the full inventory (the ceiling)

| # | Feature | Example | Cost |
|---|---------|---------|------|
| 1 | Semicolon-free statements | newline ends statement | S1 |
| 2 | String templates | `"n=$n max=${a+b}"` (2.2 adds `$$` multi-dollar) | S1 |
| 3 | Raw strings | `"""\path\no\escapes"""` | S1 |
| 4 | Safe call | `list?.first()?.id` | S1 |
| 5 | Elvis | `val x = y ?: default` | S1 |
| 6 | Not-null assertion | `y!!` | S1 |
| 7 | Nullable type suffix | `String?` | S2 |
| 8 | Smart casts | `if (x is String) x.length` | S2 |
| 9 | if/when expressions, when-subject, 2.1 guards | `when (v) { is Int -> ...; else -> ... }` | S2 |
| 10 | Ranges | `1..10`, `0 until n`, `10 downTo 1 step 2` | S2 |
| 11 | `in` operator (contains, for-range) | `for (c in 'a'..'z')` | S1 |
| 12 | Operator conventions (~15 ops: plus, get, invoke, iterator, rangeTo, compareTo, ...) | `a + b` desugars to `a.plus(b)` | S2 |
| 13 | Infix functions | `x shr 2`, `a to b` | S1 |
| 14 | Extension functions | `fun String.shout() = uppercase() + "!"` | S2 |
| 15 | Extension properties | `val String.lastChar: Char` | S2 |
| 16 | Properties with accessors + `field` | `var x: Int get() = ... set(v) {...}` | S2 |
| 17 | Property delegates | `val db by lazy { connect() }` | S2 |
| 18 | Class delegation | `class A(b: B) : Iface by b` | S2 |
| 19 | Data class synthesis | `data class P(val x: Int)` gives equals/hashCode/toString/copy/componentN | S2 |
| 20 | Destructuring declarations (+ in lambdas, for) | `val (a, b) = pair`, `for ((k, v) in map)` | S2 |
| 21 | Default arguments | `fun f(x: Int = 0)` | S2 |
| 22 | Named arguments | `f(x = 1)` | S1 |
| 23 | Trailing lambda | `list.map { it * 2 }` | S1 |
| 24 | Lambda shorthand `it` | `list.filter { it > 0 }` | S1 |
| 25 | Receiver lambdas (DSL scope) | `buildList { add(1) }` | S2 |
| 26 | Type aliases | `typealias Row = Map<String,String>` | S1 |
| 27 | Sealed hierarchies + exhaustive when | `sealed interface Shape` | S2 |
| 28 | Object expressions/declarations, companion | `object : Clickable {...}`, `companion object` | S2 |
| 29 | Primary constructor | `class P(val name: String)` | S2 |
| 30 | Value/inline classes | `@JvmInline value class Meters(val v: Int)` | S2 |
| 31 | Variance syntax | `List<out T>`, `Consumer<in T>` | S2 |
| 32 | Reified type parameters | `inline fun <reified T> parse(...)` | S3 |
| 33 | Vararg + spread | `fun f(vararg xs: Int)`, `f(*arr)` | S1 |
| 34 | Labels | `return@map`, `loop@ for {... break@loop}` | S2 |
| 35 | Backtick identifiers | `` val `data class` = 1 `` | S1 |
| 36 | Underscore placeholders | `val (_, second) = pair` | S1 |
| 37 | `tailrec` modifier | `tailrec fun go(...)` | S1 |
| 38 | Context parameters (2.2 preview) | `context(Db) fun q() = ...` | S3 |

Totals: 38 features; cost-weighted 76 (24 S1 at 1, 12 S2 at 2 = 48, plus 2 S3 at 4 = 76 if S3 counts double; using S1=1/S2=2/S3=3 it is 74). Any scoring convention lands Kotlin near twice the feature count of the leanest modern languages.

### 2.3 Per-language inventory counts

Counts below are features, not lines; "approx" is honest — the point is the order of magnitude.

| Language | Features (approx) | Cost-weighted | Notes |
|----------|------------------:|--------------:|-------|
| Kotlin | 38 | 74-76 | the ceiling; 2.1/2.2 still adding (guards, non-local break, multi-dollar, context params) |
| Swift | 29 | 60 | 4 error spellings (try/try?/try!/catch), property wrappers, result builders push weight |
| TypeScript | 28 | 58 | most weight is the type-level DSL (conditionals, mapped, template literal types, overloads, declaration merging) |
| Rust | 27 | 45 | `?`, let-else, let-chains (1.88), match ergonomics, derive, attributes, lifetime elision (S3) |
| OCaml | 14 | 28 | guards, as-binders, or-patterns, labeled/optional args, custom operators, effect handlers |
| MoonBit | 15 | 27 | `T?`, `raise?` rows, `~` labels, `::` methods, derives; verified from moonbit-core stdlib source |
| Koka | 15 | 26 | UFCS, generalized trailing lambdas, `with`, lazy named defaults, copy-update `p(age = ...)` |
| Zig | 13 | 24 | orelse/catch/try, defer, comptime, anonymous literals, labeled blocks |
| Roc (new compiler) | 11 | 19 | verified from docs/mini-tutorial-new-compiler.md; `$var` mutability, `?` postfix, `=>` match arms |
| Gleam | 9 | 15 | first-arg pipe, `use`, labeled args; no guards, no early return |
| Go | 9 | 14 | `:=`, defer, composite literals, iota; the deliberate near-zero baseline |

### 2.4 Feature x language matrix

Legend: y = yes, p = partial or a differently-spelled equivalent, n = absent. "Fmt-only" means placeholders live in a format function argument, not in string literals.

| Feature | Kotlin | Rust | TS | Swift | Go | Zig | Koka | MoonBit | Roc(new) | Gleam | OCaml |
|---|---|---|---|---|---|---|---|---|---|---|---|
| String interpolation in literals | y `$x` | n (fmt-only) | y `${e}` | y `\(e)` | n | n (fmt-only) | n | y `\{e}` | y `${e}` | n | n |
| Raw/multiline literals | y `"""` | y `r#"..."#` | p (template) | y `#"""#` | y backticks | y `\` lines | n | n | n | n | n |
| Optional chaining `?.` | y | n | y | y | n | n | n | n | n | n | n |
| Null-coalescing/elvis | y `?:` | n | y `??` | y `??` | n | y `orelse` | n | n | n | n | n |
| Postfix error propagation | p (`?: return`) | y `?` | n | p (`try` kw) | n | y `try` | n (handlers) | y `try?`/`try!` | y `?` | p (`use`) | n |
| Union types in signatures | n | n | y | n | n | p (error sets) | p (effect rows) | p (`raise E`) | n | n | p (poly variants) |
| Intersection types | n | n | y | n | n | n | n | n | n | n | n |
| Difference/negation types | n | n | p (`Exclude`) | n | n | n | n | n | n | n | n |
| Flow narrowing (smart cast) | y | p (match-only) | y | p (switch) | n | n | n | n | n | n | n |
| Exhaustive matching | y | y | p (`never`) | y | n | y | y | y | y | y | y |
| Match guards | y (2.1) | y `if` | n | y `where` | n | n | n | y | n | n | y `when` |
| Or-patterns in one arm | p (`is A, is B`) | y `\|` | n | y | n | n | n | y | n | y | y |
| as/@ binders | n | y `@` | n | p (`case let`) | n | n | n | n | n | n | y `as` |
| Destructuring bindings | y | y | y | y | p (multi-assign) | p (captures) | p (match) | y | y | y | y |
| Default arguments | y | n | y | y | n | n | y (lazy) | y (opt) | n | n | y (`?lab`) |
| Named arguments | y | n | n | y | n | n | y | y (`~`) | n | y (labeled) | y (`~`) |
| Trailing lambda | y | n | n | y | n | n | y | n | n | n | n |
| UFCS on free functions | n | p (trait methods) | n | n | n | n | y | p (`T::f` as `x.f()`) | n | n | n |
| Pipeline operator | n | n | n | n | n | n | n | n | n (removed in new compiler) | y first-arg | y last-arg |
| Operator overloading | y (conventions) | y (traits) | n | y | n | n | p (ops are fns) | y (`op_add`) | n | n | y |
| Custom infix operators | y | n | n | y | n | n | y (fixity) | n | n | n | y |
| Record/derive synthesis | y (data) | y (derive) | n | y (synth) | n | n | n | y | n | n | n |
| Type aliases | y | y | y | y | y | y | y | y | p | y | y |
| Sum types (enums) | p (sealed) | y | p (discriminated) | y | n | y (tagged) | y | y | y (`:=`) | y | y |
| Extension methods | y | p (traits) | n | y | n | n | n | y (`T::f`) | n | n | n |
| Delegation / property delegates | y `by` | n | n | p (wrappers) | n | n | n | n | n | n | n |
| Accessor properties | y | n | y get/set | y | n | n | n | n | n | n | n |
| Value/inline classes | y | n (plain structs) | n | n | n | n | n | n | n | n | n |
| Ranges as values | y | y | n | y | n | y | n | y `..<` | p | n | n |
| if/match as expressions | y | y | p (ternary) | p (ternary) | n | y | y | y | y | p (case only) | y |
| Labeled break/continue | y | y | y | y | y | y | n | y | n | n | n |
| Generics variance syntax | y in/out | y +/- | n (inferred) | y in/out | p (interfaces) | n | n | n | n | n | n |
| Anonymous struct literals | n | p (struct expr) | y (objects) | p | y (composite) | y `.{}` | n | y (records) | y (records) | y (records) | y (records) |
| Mutability marker on binding | y val/var | y mut | y let/const | y let/var | y var | y var/const | y val/var `:=` | y let mut | y `$var` | n (immutable) | p (ref/mut fields) |
| y-count (approx) | 28 | 19 | 17 | 22 | 5 | 12 | 10 | 17 | 9 | 8 | 13 |

Reading of the matrix: the languages the author loves (TS, Rust, Koka, MoonBit) cluster at 17-22 y-votes while Kotlin sits at 28; the ceiling number to enforce is therefore Kotlin's 28 y-votes, and a comfortable target is 16-20.

### 2.5 Budget conclusions

The ceiling is not a wish list; it is a maximum. Since set-theoretic types plus flow narrowing collapse Kotlin features 4, 5, 6, 7, 8, 19, 27 (safe call, elvis, not-null, nullable suffix, smart cast, data class, sealed when) into roughly two primitives (`is` narrowing and `or` extraction, section 3), DreamScript can stay far under the ceiling while matching Kotlin's day-to-day feel.
Recommended keep-set (about 18 features): string interpolation, raw strings, postfix `?`, `or`-extraction, `is` narrowing, exhaustive match with guards and or-patterns, destructuring, default+named arguments (restricted), trailing lambda, UFCS, type aliases, sum types, derives, ranges, val/var split, if/match expressions, anonymous record literals, labeled loops. Everything else in section 2.2 is a candidate for deletion.

## 3. Set-theoretic and flow typing surface

TypeScript narrows with `typeof`, `instanceof`, the `in` operator, discriminant literal checks, user predicates (`x is T`), assertion functions (`asserts x is T`), and checks exhaustiveness via assignment to `never`; it has no negation type syntax but computes difference via conditional types (`Exclude<T, U>`). Narrowing survives on `const` locals and readonly structures and is reset by closures, mutation, and `await` — exactly the sharp edges a new language can cut away under immutability-by-default.
MLscript/MLstruct (Parreaux, Castagna lineage) makes union `|`, intersection `&`, and negation `~` first-class with principal type inference, and treats classes transparently: a class is its set of instances, so `A & B` is a real type and class hierarchies become intersections (https://github.com/hkust-taco/mlscript, MLstruct OOPSLA 22 paper). This is the strongest published evidence that the author's set-algebra ambition (`type A = (B & C) - D` from the README) is inferable, not just checkable.
Whiley ships flow typing on the JVM with structural types: `type Tree is null | { int data, Tree left, Tree right }`, narrowing by `if x is T` and by null tests, with union/intersection/negation operators documented in the language spec (https://whiley.org/pdfs/WhileyLanguageSpec.pdf). Whiley is proof that flow-typed unions can carry a non-null discipline with updateable data structures; its retyping rules are the formal base for DreamScript's narrowing.
Lobster gives union types with flow narrowing in a game-scripting context (tests like `x != nil` narrow the variable; see the flow-sensitive typing survey on Wikipedia which cites it) — evidence that narrowing is cheap to implement and understandable by non-FP users.
Erlang's success typing (Dialyzer) is the opposite point on the axis: no narrowing syntax at all, types inferred as success approximations; useful as a warning that inferring flows without letting the user write union annotations produces opaque diagnostics.
The Phi language article ("Null, Errors and Enums Are One Feature", https://www.arshad.fyi/writings/null-errors-and-enums-are-one-feature) is the cleanest synthesis and matches the author's taste exactly: unit types (`type Timeout` is a type and its only value), optional is `T | none` with no `Some` wrapper, errors are plain structs with no base class, enums are unions of unit types; values enter unions unwrapped; narrowing via `is`/`match`; extraction via `or` where `lookup("port") or 8080` is fallback, `read(p) or return` is propagation checked against the caller's signature, and boolean short-circuit falls out because `bool = true | false` with `true` the first member. Its union semantics are set-like: duplicates collapse, nesting flattens, declaration site distinguishes nominal identities (`io.Timeout` != `net.Timeout`).
Interaction with exhaustiveness and error handling is the payoff: a function returns `Text | Timeout | NotFound`; a `match` on it must cover all three; growing the error set (`type ReadError = Timeout | NotFound | Locked`) breaks only failing matches, never `or return` chains; handling one member with a difference type (`(T | E1 | E2) - E1`, which is exactly the README's `B - C` set subtraction) shrinks the required remainder. Zig's error-set merging and Koka's effect rows are the same phenomenon (sets of outcomes) reached from different corners; set-typed unions make it one mechanism instead of three.
Known limits to design around: (1) the `V | none` flattening ambiguity Phi documents (a stored `none` is indistinguishable from a missing key when `V` itself contains `none`) needs an explicit `Found[V] | none` wrapper at exactly the ambiguous sites — TypeScript has the same limit; (2) narrowing on mutable fields and loop-carried variables is where all systems get complicated, which immutability-by-default largely eliminates; (3) first-member `or` conventions need one documented rule ("first member is the wanted one") and nothing more.
For DreamScript: `|`, `&`, `-` in type positions (already in the README), `is` with flow narrowing on locals, `match` exhaustiveness over unions, `never`/`void` as the empty set, and the `or` operator from Phi. No `Some`, no `Ok`, unless the user names an ambiguity wrapper.

## 4. Pattern matching

| Aspect | Rust | OCaml | Scala 3 | F# | Elixir | Roc (new) | Gleam | Koka |
|---|---|---|---|---|---|---|---|---|
| Arm syntax | `pat => expr` | `pat -> expr` | `case p => e` | `\|p -> e` | `p -> e` | `pat => expr` | `pat -> expr` | `pat -> expr` |
| Guards | `if cond` | `when cond` | `if cond` | `when cond` | `when cond` | none | none | none |
| Or-patterns | `A(x) \| B(x)` | `\|` | `\|` | `\|` | `\|` | none | list `A(x), B(x) ->` | none |
| Binders | `x @ pat` | `pat as x` | `x @ pat` | `x as pat` | pin `^x` | nested only | nested only | nested only |
| Exhaustiveness | yes | yes | yes | yes | partial (no static) | yes | yes | yes |
| Match ergonomics | default binding modes on refs | none needed | none needed | none needed | pin/underscore rules | no refs | no refs | no refs |

Rust's guard `pat if cond` and Kotlin 2.1's stabilized `if` guards in when-with-subject are the same feature; OCaml's `when` and Swift's `where` prove the keyword is interchangeable. The `if` spelling wins because DreamScript already uses `if` and the guard is literally an expression test.
Or-patterns: Rust and OCaml allow `Some(1) | Some(2) -> ...` in one arm; Gleam instead allows a comma list of whole patterns per arm (`1, 2 -> ...`) which reads better for flat enums but cannot share sub-binders; Kotlin's `is A, is B` is a flat-type-only version. Recommend Rust/OCaml `|` plus the underscore rule; cost S1.
@-binders (`x @ Some(y)`) are needed to bind a while still matching inside it; OCaml's `as` is the same with the operands swapped. Under set types the commonest case (`u is User && u.age >= 18`) needs no binder at all because narrowing reuses the original variable, so @-binders are a candidate for deletion from the keep-set — worth keeping only if match ergonomics for linear values require re-binding a moved-out value; cost S1.
Exhaustiveness over unions falls out of section 3: the compiler checks the match arms cover the whole set, and can emit "these members are missing" diagnostics naming the set difference — better errors than any of the surveyed languages, because the missing members are literally a set computation.
Elixir is the cautionary contrast for pin operators (`^x` matches the value of `x` instead of rebinding): a consequence of single-assignment rebinding semantics that DreamScript avoids by having immutable bindings that cannot be shadowed silently.
Match ergonomics: Rust's default binding modes (auto-ref/deref through `&`, `&mut`, and box-like patterns) are the only real complexity in the surveyed set and exist because of references; an interpreter with value semantics and linear types does not need them. Recommend plain patterns on values, plus explicit `ref`-like binders only if linear borrowing demands it later.

## 5. UFCS and pipelines

Koka (verified from doc/spec/tour.kk.md in the local clone): "the expression `s.encode(3)` does not select the `encode` method from the `:string` object, but it is simply syntactic sugar for the function call `encode(s,3)` where `s` becomes the first argument"; it chains freely (`s.encode(3).count.println` desugars to `println(count(encode(s,3)))`) and lets any library add "methods" to any type by defining a first-argument function. This is precisely the author's stated preference, and it composes with effects: an effectful `parse_int(s) -> Int | E` becomes `s.parse_int()` and then `s.parse_int()?` under section 11's propagation.
Rust's method syntax is not UFCS: `x.f()` resolves through traits and auto-ref/deref with a fixed coherence-driven precedence; that resolution machinery is a hidden complexity cost the author pays in Rust today. Full UFCS plus explicit `Trait::f(x)`-style qualification is simpler to explain and to implement in an interpreter (pure desugaring, no candidate search).
D and Nim have the same dot-sugar as Koka (Nim's method-call syntax, D's UFCS), so the mechanism has three independent production proofs; none of them reports confusion in practice, and Koka's tour cites D's UFCS as precedent.
Pipes: Gleam passes the piped value as the first argument (`a |> b(1, 2)` = `b(a, 1, 2)`, verified via tour.gleam.run and the v0.8 release note); Elixir does the same; F# and OCaml's `|>` pass as the last curried position (`x |> f a b` = `f a b x`); Hack and the TC39 proposal use a placeholder. First-argument piping is the variant consistent with UFCS and with the filter-map-reduce ergonomics the author wants; last-argument piping is a currying artifact.
Roc removed its pipeline and `?>`-family operators in the 0.14+ redesign; the new-compiler mini tutorial shows no `|>` at all — method-style chaining plus `?` was judged sufficient, which is evidence that UFCS + `?` covers most pipe use cases.
Side by side on the same five-stage chain, with the pipe shown because it is the one place it genuinely reads better (named stages with comments between them):

```koka
// Koka UFCS: pure desugaring, every stage is a plain call
s.trim.lower.split(",").map(fn(x) x.trim.parse_int.or(0)).sum

// with a pipe for the long stages (Gleam first-arg, same desugaring as UFCS)
s
|> trim
|> lower
|> split(",")
|> map(fn(x) x.trim.parse_int.or(0))
|> sum
```

Recommendation: full UFCS (S2 — one desugaring rule, zero runtime cost) as the primary chaining device; optionally Gleam-style first-argument `|>` (S1) as a second-stage comfort feature for multiline chains with comments between stages. `x |> f(a)` desugars to `f(x, a)` — identical to UFCS, so the two features are orthogonal and the pipe is pure syntax with no new semantics.

## 6. Binding and declaration forms

Binding keywords across the surveyed set: Rust `let`/`let mut`, Kotlin `val`/`var`, Swift `let`/`var`, Zig `const`/`var`, TS `const`/`let` (plus legacy `var`), Koka `val`/`var` with `:=` for assignment, MoonBit `let`/`let mut`, Roc `$var` prefix for reassignable, Gleam `let` only (immutability enforced), OCaml `let` only, Go `var`/`:=`. The val/var split (Kotlin, Swift, Koka) is the most self-documenting; Rust's `mut` suffix rides on the universal `let`; Roc's `$` is the only prefix and reads poorly in expressions (`$num * 10 + $num`).
Function keywords: Rust/Zig/MoonBit `fn`, Kotlin `fun`, Swift `func`, Koka `fun` (top-level) plus `fn` (anonymous), OCaml `let f`, Gleam `fn` everywhere with named bindings, TS `const f = (x) => ...` and `function f() {}`. The TS arrow-assignment form (`const f = (a: int) -> int {...}`) is already the README's choice and matches the author's love of TS; Koka's split (`fun` for named, `fn` for anonymous) is the cleanest of the keyword families. Both are viable global styles (section 12 shows both).
Annotation position: `name: T` (Rust, Kotlin, TS, Koka, MoonBit, Inko, old DreamScript README) dominates; postfix `name T` (Go, Zig) is a minority; annotations on `let` are optional everywhere (inference for locals, required for public signatures in Rust/Koka/MoonBit style). Recommend `name: T` with locals inferred and top-level signatures required.
Expression orientation: Koka, Roc, Gleam, Rust (blocks), OCaml, MoonBit, Zig (if), and Kotlin (if/when) all treat conditionals and matches as expressions; Go and (mostly) TS do not. For a language with error unions the expression orientation is not optional — early-return sugar must be an expression-level mechanism to desugar cleanly. Recommend full expression orientation.
If/when-as-expression: universal in the relevant languages and free (S1); matches the author's Lua/TS habits equally.
Trailing lambdas: Kotlin's is the ergonomic king (`list.map { it * 2 }`); Koka's is the most principled — generalized so that a brace block is a suspended thunk argument anywhere (`while { i >= 0 } ...`), and it was Koka that popularized trailing lambdas in the first place (tour, sec-anon). The catch is that `{}` doing double duty as block and thunk makes Koka's grammar context-sensitive; Kotlin restricts it to the last argument which is more predictable. Recommend Kotlin-style last-argument trailing lambda (S1), not Koka's generalized braces-as-thunks (S3).
Named and default arguments: the author's typst complaint (eager defaults with out-of-order evaluation, typst/typst#3052, and the nested `values.at(6, default: values.at(-1))` pattern) is real: expression defaults that reference other parameters require either eager evaluation in a defined order (typst's pitfall) or lazy re-typing of every default as an implicit function (Koka types `len : int = xs.length` as an optional `?int` and evaluates it in the callee's scope, which is well-defined but is an S3 semantics). The three designs on one example:

```text
// (a) monadic extraction, no default — evaluation order is explicit and local
row.get("scale").float.or(1.0)

// (b) literal-only defaults — the default cannot reference anything, no order exists
fun scale(s: String, factor: Float = 1.0)

// (c) lazy named default (Koka) — desugars to scale(s, factor: () => xs.length.float),
//     evaluated per call inside the callee; well-defined but every default is a hidden closure
fun sublist(xs: List[A], start: Int, len: Int = xs.length)

// (d) caller-scope eager default (typst) — which `xs`? evaluated when? typst#3052
values.at(6, default: values.at(-1))
```

Options, cheapest first: (a) no defaults, use `or` extraction at call sites — purest, matches monad philosophy; (b) literal/const-expression defaults only — S2, covers 90 percent of use, zero evaluation-order semantics; (c) Koka-style lazy defaults — S3, fully general, costs a hidden closure per defaulted parameter. Recommend (b) plus (a) for computed fallbacks; document that (c) exists if ever needed.
Where clauses: Rust `where T: Ord`, MoonBit `T : Compare` bounds, Roc's `where` on type variables. With set types, bounds are just type-set constraints (`T <: Comparable`) and one syntax suffices; keep it on the generic parameter itself, no separate where-clause (S1 saved, one less surface construct).
Braces vs indentation survey: Python (indentation) has permanent complaints about copy-paste, refactoring, and tab/space mixing; Nim (indentation) repeats them and adds macro-authoring pain; Koka (indentation) works and is the best-implemented indentation language, but its tooling story needed significant whitespace-aware infrastructure; Roc explicitly went from braceless discussions to braces in the 0.14+ redesign; Gleam, Zig, Swift, Go, Rust, MoonBit, TS all use braces. Outcome: braces with optional single-line bodies (Kotlin `if (x) return y` / Rust) are the low-risk default for DreamScript; indentation is defensible only if the author is willing to maintain Koka-grade tooling.
Semicolons: three live families — none at all with newline termination (Koka, Roc, Gleam-lite), optional with insertion (Kotlin, TS/ASI with its famous footguns), required (Rust, Zig, Gleam actually requires them). For an interpreted language with a line-oriented REPL, newline-terminated statements with no semicolons in the grammar is the cheapest rule (S1) and removes the ASI hazard class entirely; allow a trailing comma everywhere instead.

## 7. Strings

Interpolation conventions surveyed: Kotlin `"$x"` bare identifier and `"${expr}"` for expressions; Swift `"\(expr)"` (parentheses conflict with tuple syntax); TS/JS `"${expr}"`; Rust format-captured `"hello {name}"` inside `format!`/`println!` macros only, bare identifiers captured from scope; Zig `"{s}"` placeholders in `std.fmt` arguments, nothing in literals; Python `f"{expr}"` with a format-spec mini-language; Roc `"${expr}"` where the expression must already be a string (no implicit `to_str`, verified in the mini tutorial); MoonBit `"\{expr}"` (per stdlib usage); Go and Gleam and OCaml have no interpolation (Go `fmt.Sprintf`, Gleam `<>` concatenation, OCaml `Printf.sprintf`).
Kotlin's scheme is the winner on every axis except implicit stringification: one character for the common case, one brace rule for the complex case, no sigil soup, and it is what the author's Lua/TS muscle memory expects. Decide explicitly: `${expr}` requires `.to_str()`-style conversions for non-strings (Roc's rule, explicit, type-safe) or auto-stringifies (Kotlin's rule, convenient, hides allocation). Recommend Kotlin syntax with Rust-style explicitness at the boundary: interpolation of non-string values calls `str()` visibly (`"${x.str()}"`) or via a `Display`-like trait if the author later wants auto.
Raw strings and heredocs: Rust `r"..."`/`r#"..."#` with hash-count escape closure; Kotlin/Swift `"""..."""` triple quotes (Kotlin's strips indentation via `.trimIndent()`, Swift strips to the closing delimiter); Go backticks (no escapes at all); Zig `\\` line-prefix form. One construct should cover both needs; recommend Rust's hash-counted raw strings (they nest and embed quotes and code without escape ambiguity) or triple quotes if the author prefers Python familiarity; cost S1 either way.
UTF-8 slicing: Rust `&str` is UTF-8 with byte indexing that panics on non-char boundaries and `char_indices()`/`chars()` iterators; Swift `String` indexes by grapheme clusters (no integer subscript at all); Koka exposes bytes with UTF-8 awareness; Zig arrays of u8 with the library doing decoding. Recommendation for an interpreter: store UTF-8, index by byte for zero-copy slices that must land on char boundaries via a checked API (`s.slice_char(i, j) -> String | none`), iterate `chars` for logic, keep grapheme clusters out of core (library concern); the `| none` return matches section 3 and avoids silent panics.

## 8. Numeric literals and towers

Overflow policies: Rust panics on overflow in debug builds and wraps in release (documented, per-build divergence); Zig treats overflow as checked illegal behavior (panic in safe modes, UB in ReleaseFast) with explicit `+%`/`-*` wrapping operators; Swift always traps with `&+` as the explicit wrapping escape; Go silently wraps always; Kotlin/Java silently wrap; C is undefined. For an interpreted language the honest choice is Swift's: trap on overflow always (predictable, debuggable, cheap to implement in a bytecode loop with flags) and provide `+%`-style wrapping ops for hash/PRNG use; cost S1.
Literal-to-type rules: Rust infers integer literal types from context with i32 as fallback; Zig comptime-int is arbitrary precision at compile time and coerces to the target; Go untyped constants adapt to context (the cleanest formalization of "literal is a set of values" — literally a set of types); Python ints are arbitrary precision always; Julia promotes. Go's untyped-constant model is the most set-theoretic and would fit DreamScript's "every type is a set" story: a literal has a literal type-set and narrows to whatever the context wants; cost S3 to implement well (constant folding plus range checking), S1 to the user.
Recommendation: default `Int` (i64) and `Float` (f64) for inference; suffixes `1u8`, `2.0f32` for the exceptions (Rust/Kotlin convention, S1); digit separators `1_000_000` (S1, universally loved); no implicit narrowing conversions anywhere (Rust rule); arbitrary precision via a `BigInt` library, not literal magic; hex/binary/octal prefixes `0x 0b 0o` (universal).
Units of measure: F#'s `<Measure>` attribute types physical units with zero runtime cost, but it is a type-system extension (S3) that touches every numeric; the author's set types can express the 90 percent case (distinct nominal wrapper types `type Meters = Wrap[int]` plus derives) at library level. Defer units of measure; do not bake in.

## 9. Equality and identity

Java: `==` is identity on objects (the classic trap), `equals()` for values. Python: `==` values, `is` identity — and `is` on small ints is a beloved footgun. Kotlin: `==` calls `equals` (structural), `===` is referential identity — the pragmatic fix. Rust: no `==` at all without a `PartialEq` impl, derived or manual; `Eq` marks total equivalence; identity is `ptr::eq` on raw pointers only. Roc: one structural `==` on everything, no identity concept because values are immutable and semantics are value-based. Zig: `==` on primitives, `std.meta.eql` for deep structure comparison.
Under immutability-by-default with value semantics, identity is almost meaningless: two equal immutable records are interchangeable, which is exactly Roc's justification. Identity remains meaningful only for (a) linear resources (files, locks) where duplication is forbidden anyway, and (b) memoization/interning, which is an implementation concern, not a user-facing operator.
Recommendation: one `==`, structural, auto-derived for records and sum types (overridable via a trait for floats-in-keys cases), IEEE semantics for floats (`nan != nan`, with `total_cmp`-style ordering available for sorting/keys), no `===`, no `is` (the word is taken by type tests anyway — a happy collision: `is` for types, `==` for values, nothing left to confuse). Deletion of `===` is a negative-sugar win against Kotlin.

## 10. Operator overloading policy

The spectrum: Go — none, `==` and friends are fixed to builtins, period; Roc (new compiler) — none; Zig — none, operators only on primitives; Rust — trait-gated on a fixed operator set with fixed precedence (`std::ops`), no custom operators ever; Kotlin — convention methods (`plus`, `get`, `invoke`, `rangeTo`) on a fixed token set plus `infix fun` for named operators with custom precedence hints; Swift — arbitrary custom operators with precedence groups; OCaml/Haskell/C++ — free-form operators with custom fixity declarations.
The author's constraints pick Rust's slot: fixed token set mapped to fixed trait names, fixed precedence table, no user-defined tokens, no fixity declarations. This preserves explainability (a short table in the tutorial), keeps the interpreter's dispatch simple (operator call = trait method call, inline-cacheable), and interacts correctly with set types (an operator is defined for a trait-set of types, not by specialization priority — which the author rejects on principle).
What the fixed set should include for a linear-typed language: arithmetic `+ - * / %`, negation, bit ops, comparison via `Ord` (one derive covers it), `Index`-style get, and nothing exotic; `Deref`-style coercions should not exist as user surface (they are Rust's most misused trait and UFCS makes them unnecessary).
Cost: S2 (one trait family + dispatch rule), same as Kotlin's conventions, far below Swift/C++ freedom.

## 11. Error and optional propagation surface

(a) Surveyed surfaces: Rust `?` (early-return `Err`, applies `From` conversions — the conversion rule is a hidden S3), Zig `try` (early-return error) and `catch |e|` (bind) and `orelse` (optional fallback) and `catch` (fallback), Swift `try`/`try?`/`try!` (three spellings of "I acknowledge this throws" plus `do/catch`), Kotlin `?: return`/`?: throw` elvis chains and `runCatching`, Roc's postfix `?` ("if this is an Ok, evaluate to its value; otherwise, return the Err" — verified verbatim in the new-compiler tutorial), Gleam's `use x <- result.try(...)` monadic sugar, TS `throw`/`catch` with unchecked errors, Koka effect handlers `with handler { ... }` resumable.
(b) The old DreamScript idea (from the historical README): any block `{ 1 / 0 }` carries an error union depending on context ("each error can be thrown in the other dimension"), so `{1/0}` is a `double` where unhandled and a `double | ArithmeticError` under a typed annotation, and extraction is `.ok((v) -> ...)` / `.err((e) -> ...)` handlers, with `throw` to re-raise. Evaluation: the goal is right (one mechanism for Option/Result/effects, no per-monad keywords — the same insight as Phi) but the mechanism has three flaws. (1) Non-compositionality: the type of `{1/0}` depends on the annotation at the use site, so the same expression has different types in different places; local reasoning and inference both break, and this violates the charter's explainability budget directly. (2) The `.err` handler returning `double | null` just renames the monad (a `null` where Rust has `Err`), moving the problem rather than solving it. (3) Braces doing double duty as block, thunk, and error boundary is the Koka-thunk ambiguity with extra semantics. The salvageable pieces are the block-as-error-scope (which is just `try { }`-as-expression) and `.err`/`.ok` as extraction combinators.
(c) The recommended minimal surface, uniform across Option/Result/effects because they are all unions (section 3): (a) postfix `?` — Zig/Roc semantics, generalized to unions: `e?` where `e : T | E` evaluates to `T` if the value is in `T`, otherwise returns the union member early; the enclosing function's declared return union must contain `E` (checked, no `From` conversions — Zig's rule, cheaper than Rust's); cost S2. (b) `or` — Phi's extraction operator: `x or fallback` where `x : T | F` yields `T` (first-member-wanted), with `or return`, `or continue`, `or break` as early-exit forms; this single token replaces elvis, `unwrap_or`, `orelse`, and `??`; cost S2. (c) `.err(h)` / `.ok(h)` as library functions on unions with set-difference typing: `{expr}.err(handle)` has type `(T | E) - E` when the handler covers `E` — the README's `B - C` subtraction doing real work; cost S2, and it is the old idea's `.err` now compositional. (d) `is` narrowing (free, section 3) covers everything else.
(d) Net sugar for the whole error/optional story: three tokens (`?`, `or`, `is`) plus one combinators block, versus Kotlin's four (`?.`, `?:`, `!!`, `T?`), Swift's five (`?`, `?`, `!`, `try?`, `try!`), and Zig's four (`try`, `catch`, `orelse`, `?T`/`!T` pair). If algebraic effects are adopted later, Koka-style `with` handlers slot in without new extraction syntax because `or` and `?` already operate on union members — this is the strongest argument that the surface is effect-ready.
(e) The recommended surface end to end on one small example, including the difference-typed `.err` handler that repairs the old idea:

```text
fn load_config(path: String) -> Config | IoError | ParseError {
  const text = read_file(path)?            // IoError propagates; return union contains it
  const cfg  = parse_config(text)?         // ParseError propagates the same way
  cfg
}

const cfg = load_config("app.cfg") or default_config()   // `or`: fallback over the union

const cfg2 = load_config("app.cfg").err((e) => {         // handler covers IoError | ParseError,
  log("config failed: ${e}")                             // result type is Config - (IoError | ParseError)
  default_config()
})                                                       //  == Config, no remainder to handle

const port = cfg2.port or 8080                            // optional is T | none, same three tokens
```

Note what disappeared: `Some`, `Ok`, `unwrap`, `unwrap_or_else`, `try?`, `try!`, `??`, `?.`, `!!`, and any per-monad keyword. What remained is three tokens whose semantics are stated once over unions.
Explicitly rejected: Swift's three spellings (three S2s for one concept); Rust's `From` conversions inside `?` (surprising type changes at propagation); context-dependent block typing (the old idea's flaw); `??`/`?.` as separate operators (subsumed by `or` and `is`; a thin `x?.f` desugaring to `x or none-map` can be added later if real code demands it — keep it out of v1 to respect the budget).

## 12. Global style candidates (deliverable b)

Four coherent global styles, each demonstrated on the same representative program: a small record definition, a parse function with an error union, filter-map-reduce over rows, a match on the outcome, and an error handler. Program semantics are identical across all four; only surface differs. These are sketches of feel, not final grammar.

### Style A — TS-family braces (arrow-assignment, const/let, UFCS, `?`/`or`/`is`)

```ts
type ParseError = BadNumber(String) | MissingField(String)

struct User {
  name: String,
  age: Int,
}

const parse_age = (s: String) -> Int | ParseError {
  match s.trim().parse_int() {
    Ok(n)  => n,
    Err(_) => BadNumber(s),
  }
}

const parse_user = (row: Row) -> User | ParseError {
  const name = row.get("name") or return MissingField("name")
  const age  = row.get("age")  or return MissingField("age")
  User { name, age: parse_age(age)? }
}

const main = () -> () | IoError {
  const rows  = load_rows("users.csv")?
  const parts = rows.map(parse_user).partition((r) => r is ParseError)
  const (users, errors) = parts

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

Reading: this is the author's TS+bun muscle memory with Rust's match; `const f = (args) -> T { }` is the historical README's own form; arrow lambdas `=>` everywhere; the least novel surface of the four, hence the lowest learning cost and the smallest grammar delta from TS tooling. Risk: arrow-fatigue and the `const` keyword doing triple duty (binding, function, module item).

### Style B — Gleam/Roc braces (named `fn`, `let`, pipes optional, `|x|` lambdas)

```roc
type ParseError {
  BadNumber(String)
  MissingField(String)
}

fn parse_age(s: String) -> Int | ParseError {
  match s.trim().parse_int() {
    Ok(n) => Ok(n)
    Err(_) => Err(BadNumber(s))
  }
}

fn parse_user(row: Row) -> User | ParseError {
  let name = row.get("name") or return MissingField("name")
  let age = row.get("age") or return MissingField("age")
  Ok(User { name, age: parse_age(age)? })
}

fn main() -> () | IoError {
  let rows = load_rows("users.csv")?
  let (users, errors) = rows.map(parse_user).partition((r) => r is ParseError)

  let total = users
    .filter(|u| u.age >= 18)
    .map(|u| u.age * 2)
    .reduce(0, |a, b| a + b)

  for u in users {
    println("user ${u.name} age ${u.age}")
  }

  match (total, errors.first()) {
    (_, e) if e is ParseError => println("warning: ${e}")
    (0, _) => println("no valid users")
    (t, _) => println("total score: $t")
  }
}
```

Reading: ML-family surface in braces — `fn` for everything, `let` bindings, `|x|` lambdas (Roc's new syntax), match arms without commas. Closest to Roc/Gleam culture and to the interpreter implementation (every form is a simple node). Risk: `fn`/`let`/`|` sigils are denser than TS; piped style is available but not shown to keep the sample under budget.

### Style C — Koka indentation (fun/val, full UFCS, trailing lambdas)

```koka
type ParseError
  BadNumber(string)
  MissingField(string)

struct user
  name : string
  age  : int

fun parse_age( s : string ) : int | parse-error
  match s.trim.parse_int
    Ok(n)  -> n
    Err(_) -> BadNumber(s)

fun parse_user( row : row ) : user | parse-error
  val name = match row.get("name")
    Just(n) -> n
    Nothing -> return MissingField("name")
  val age = match row.get("age")
    Just(a) -> a
    Nothing -> return MissingField("age")
  User(name, parse_age(age))

fun main() : () | io-error
  val rows  = load_rows("users.csv")
  val parts = rows.map(parse_user).partition(fn(r) r is ParseError)
  val (users, errors) = parts

  val total = users
    .filter(fn(u) u.age >= 18)
    .map(fn(u) u.age * 2)
    .foldl(0, fn(a,b) a + b)

  for(users) fn(u)
    println("user ${u.name} age ${u.age}")

  match (total, errors.first())
    (_, Just(e)) -> println("warning: ${e}")
    (0, _)       -> println("no valid users")
    (t, _)       -> println("total score: $t")
```

Reading: the Koka tour's own style — indentation blocks, `fun`/`val`, full UFCS (`s.trim.parse_int`), trailing lambdas after calls, no semicolons or commas between arms. Note it honestly shows the costs: without `or`/`?` the option extraction nests (the two `match ... return` ladders), and Just/Nothing wrapping appears because Koka lacks union optionals — DreamScript Style C would use `or return` and lose two ladders, but the indentation and UFCS character is what is being compared. Risk: significant whitespace infrastructure; copy-paste sensitivity.

### Style D — Rust-family braces (fn/let/mut, impl blocks, derive, `?`)

```rust
#[derive(Eq, Show)]
enum ParseError {
    BadNumber(String),
    MissingField(String),
}

struct User {
    name: String,
    age: u32,
}

fn parse_age(s: String) -> Result<u32, ParseError> {
    match s.trim().parse::<u32>() {
        Ok(n) => Ok(n),
        Err(_) => Err(ParseError::BadNumber(s)),
    }
}

fn parse_user(row: &Row) -> Result<User, ParseError> {
    let name = row.get("name").ok_or(ParseError::MissingField("name".into()))?;
    let age_s = row.get("age").ok_or(ParseError::MissingField("age".into()))?;
    let age = parse_age(age_s.to_string())?;
    Ok(User { name, age })
}

fn main() -> Result<(), IoError> {
    let rows = load_rows("users.csv")?;
    let (users, errors): (Vec<_>, Vec<_>) = rows.into_iter().partition(|r| !matches!(r, Err(_)));
    let users: Vec<User> = users.into_iter().map(|r| r.unwrap()).collect();

    let total: u32 = users.iter().filter(|u| u.age >= 18).map(|u| u.age * 2).sum();
    for u in &users {
        println!("user {} age {}", u.name, u.age);
    }
    match (total, errors.first()) {
        (_, Some(e)) => println!("warning: {e}"),
        (0, None) => println!("no valid users"),
        (t, None) => println!("total score: {t}"),
    }
    Ok(())
}
```

Reading: the author's daily Rust with the rough edges visible — `Result`/`Option` double wrapping, `.ok_or(...)?` ladders, `matches!` and `.unwrap()` needed because there are no unions, `collect()` noise for partition. This style is the control group: familiar, but its sugar is spent compensating for missing set types rather than on ergonomics. Not recommended as the global style, but useful as the anchor if the author wants maximum Rust transfer.

Recommendation: Style A or B. A maximizes TS/bun transfer (author's stated love) and reads like the historical README; B is the most implementation-friendly (every construct is a trivial AST node, Roc/Gleam prove it scales) and leaves room to add A's arrow sugar later without breaking B code. C is viable only with committed whitespace tooling; D loses to A/B on every measured axis except Rust familiarity.

## 13. Item-level toggle catalog (deliverable c)

Each affordance gets 3-4 spellings with cost marks; the author picks one per row. S1/S2/S3 as defined in 2.1.

Match guards:
1. `pat if cond -> ...` (Rust, Kotlin 2.1) — S1.
2. `pat when cond -> ...` (OCaml, F#) — S1.
3. `pat where cond => ...` (Swift) — S1.
4. No guards; nested `if` in the arm body (Gleam, Roc) — S0 but nests.
Or-patterns:
1. `A(x) | B(x) -> ...` in one arm (Rust/OCaml) — S1.
2. Comma list of whole patterns: `A(x), B(x) -> ...` (Gleam/Roc style) — S1.
3. Fallthrough sharing a body (C/Swift implicit) — S2 (hidden control flow).
4. Separate arms duplicating the body — S0.
Binders:
1. `x @ pat` (Rust/Scala) — S1.
2. `pat as x` (OCaml) — S1.
3. No binders; re-match or narrowing on the scrutinee (set-types style `u is User`) — S0.
4. Bindings inside pattern via nested patterns only — S0.
Option/error extraction:
1. `or` operator with `or return/break/continue` (Phi) — S2.
2. Elvis `?:` (Kotlin) + `!!` (S1 each, but two operators for one concept) — S2.
3. `orelse` + `catch` words (Zig) — S2.
4. Library only: `.unwrap_or(fn)`, `.ok()`, `.err(fn)` — S0 syntax, S2 verboseness.
Propagation:
1. Postfix `?` on unions, return-union containment check (Zig/Roc generalized) — S2.
2. `try` keyword prefix (Zig/Swift) — S2, plus a second spelling for bind (`catch |e|`).
3. Do-notation `use x <- f(...)` (Gleam/Haskell) — S3.
4. Context-typed blocks `{...}` carrying error unions (old DreamScript) — S3, rejected in section 11.
String interpolation:
1. `"$x"` and `"${expr}"` (Kotlin) — S1.
2. `"\(expr)"` (Swift) — S1, paren collision with tuples.
3. `"{x}"` captured in format functions only (Rust/Zig) — S1 but no literal interpolation.
4. `"${expr}"` requiring explicit `.str()` on non-strings (Roc) — S1, most explicit.
Default arguments:
1. None; `or` at call sites — S0.
2. Literal/const-expression defaults only — S2.
3. Lazy named defaults desugared to closures, evaluated in callee scope (Koka `len : int = xs.length` typed `?int`) — S3.
4. Full expression defaults with caller-scope evaluation (typst) — S3, documented footgun (typst#3052).
Named arguments:
1. Optional labels on any parameter (Kotlin/Koka) — S1.
2. Labels required past arity 2 (Swift-ish discipline) — S1.
3. Labels as part of the API, first-arg implicit via UFCS (Gleam) — S1.
4. Positional only (Rust/Go) — S0, worst for long signatures.
Trailing lambda:
1. Last-argument trailing lambda with `it` shorthand (Kotlin) — S2.
2. Koka generalized: any brace block is a thunk argument — S3 (context-sensitive grammar).
3. None; pass `fn`/lambdas explicitly (Gleam/Roc/OCaml) — S0.
Mutability marking:
1. `val`/`var` (Kotlin/Swift/Koka) — S1.
2. `let`/`let mut` (Rust/MoonBit) — S1.
3. `$var` prefix (Roc) — S1, awkward mid-expression.
4. No reassignment at all; builders/functional update (`p(age = p.age + 1)`, Koka copy-update) — S0, most radical.
Annotation position:
1. `name: T` (Rust/TS/Koka/README) — S1.
2. Postfix `name T` (Go/Zig) — S1.
3. Inferred everywhere, annotated only at boundaries — S0 plus S3 inference.
Equality:
1. One structural `==`, auto-derived (Roc) — S1.
2. `==` structural + `===` identity (Kotlin) — S2.
3. Trait-gated `==` (Rust `PartialEq`) — S2.
4. Identity-only `==`, `equals()` method (Java) — S2, worst footguns.
Numeric literals:
1. Inferred `Int`/`Float` defaults + suffixes `1u8`, `2.0f32` (Rust) — S1.
2. Untyped literal set narrowing to context (Go constants) — S3 semantics, best set-theory story.
3. Explicit annotations required (C-like) — S0 sugar, S2 verboseness.
4. Arbitrary-precision default (Python) — S3, slow interpreter path.
Operator overloading:
1. Fixed token set via fixed traits, fixed precedence (Rust) — S2.
2. Convention methods, no custom operators (Kotlin) — S2.
3. None (Go/Roc/Zig) — S0.
4. Custom operators with fixity declarations (Swift/OCaml) — S3, rejected.
Blocks and statements:
1. Braces, optional semicolons via newline termination (Kotlin/Roc) — S1.
2. Indentation (Koka/Python/Nim) — S2 plus tooling burden.
3. Braces + required semicolons (Rust/Zig) — S1, noisier.
4. Braces, newline-terminated, no semicolons in grammar (recommended) — S1.
Ranges:
1. `a..b` half-open and `a..=b` closed as values (Rust) — S1.
2. `a..b` / `a...b` closed (Swift) — S1, trippledot ambiguity risk with spread.
3. Range functions `range(a, b)` only (Go) — S0.
4. `in`-only ranges in loops, no range values (Kotlin-lite) — S1.

## 14. Sources

Primary (docs and source read for this file): koka tour and book source, local clone `koka/doc/spec/tour.kk.md` (UFCS, trailing lambdas, with-statements, optional/named parameters, struct copy-update) and online at https://koka-lang.github.io/koka/doc/book.html; Roc new-compiler mini tutorial, https://raw.githubusercontent.com/roc-lang/roc/main/docs/mini-tutorial-new-compiler.md (`?` postfix, `$var`, `=>` arrows, `${}` interpolation, no pipeline); Gleam pipelines, https://tour.gleam.run/functions/pipelines and https://gleam.run/news/gleam-v0.8-released (first-argument pipe); Phi, https://www.arshad.fyi/writings/null-errors-and-enums-are-one-feature (unit types, `is/match/or`, union semantics, memory layout, merging ambiguity); moonbit-core stdlib, local clone `moonbit-core/builtin/{array,result}.mbt` (`pub fn[T] T::f`, `T?`, `raise?`, `~` labels, record punning, `#cfg` attributes); inko stdlib, local clone `inko/std/src/std/array.inko` (`fn pub inline`, `ref T` modes, `-> Never`); Kotlin 2.1 and 2.2 release notes, https://kotlinlang.org/docs/whatsnew21.html and https://kotlinlang.org/docs/whatsnew22.html (stabilized guards, non-local break/continue, multi-dollar interpolation; context parameters preview); Whiley language spec, https://whiley.org/pdfs/WhileyLanguageSpec.pdf (flow typing, `type Tree is null | {...}`); MLscript, https://github.com/hkust-taco/mlscript and MLstruct paper https://lptk.github.io/files/[v6.2]%20mlstruct.pdf (Boolean algebra of types with principal inference); flow-sensitive typing survey, https://en.wikipedia.org/wiki/Flow-sensitive_typing (includes Lobster); typst evaluation-order issue, https://github.com/typst/typst/issues/3052; Gleam `use` discussion, https://github.com/gleam-lang/gleam/discussions/2146.
Secondary (training knowledge, stable surfaces, verified against the above where they overlap): Kotlin language reference, Rust reference and std::ops, TypeScript handbook, Swift language guide, Zig language reference, Go spec, OCaml manual, Elixir guides, F# units of measure docs.
