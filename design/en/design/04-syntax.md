# 04 — Syntax Surface: Decision Catalog

Status: Phase 2 option catalog, written 2026-11-04, awaiting author decisions per AGENTS.md. Primary evidence: `docs/en/research/04-syntax-ergonomics.md`, cited as research/04 §N (its §14 lists all URLs). Historical prior art: the pre-charter `README.md` (read-only by charter): `const f = (a: int) -> int` arrow assignment, `?` as a return marker, `-*>` generator arrows, context-typed `{1/0}` error blocks, `.err`/`.ok` handlers — audited as prior art in F2 and F4, never re-adopted wholesale.

Sister catalogs: `docs/en/design/01-types.md` owns the type-level decisions this file only spells (B1 unions, B3 products/sums, B4c `T?` alias, B7a trapping overflow, B8a UTF-8 strings, B9a structural `==`); `docs/en/design/03-effects-errors-async.md` owns error/effect semantics (B5d hybrid signatures, E4b union errors with set-difference handlers) but explicitly defers the `?` / `or` / `is` / `.err` surface tokens to this file (its §2). `docs/en/design/02-linearity-ownership.md` owns mutation and borrow spelling (its C3); it is referenced by path only and was not read. `docs/en/design/05-generics-metaprogramming.md` owns generic/where-clause spelling (deferred from F2), the trait mechanism UFCS dispatches through (F3), trait and derive names (F5, F7), and comptime (F5.d). `docs/en/design/00-decision-map.md` indexes all points.

## Decision-point index

| ID | Decision point | Options | Recommendation (one line) |
|---|---|---|---|
| F1 | Global shape | (a) TS-family braces, (b) Gleam/Roc braces, (c) Koka/Python indentation, (d) Rust-family braces | (a) TS-family braces — the author's TS+bun taste and the historical README's own form, at the lowest grammar delta (research/04 §12) |
| F2 | Bindings, functions, annotations, returns | (a) const/var vs val/var vs let/let-mut, (b) const-arrow vs fn vs fun, (c) `: T` vs `-> T` vs postfix, (d) final-expression vs `return` vs README `?` | `const`/`var`, named fns as `const name = (…) -> T { }`, `name: T` + `-> T`, final-expression returns; `?` is reserved for propagation (docs/en/design/03 E4) — the README's `?` return marker is retired |
| F3 | UFCS and pipelines | (a) Koka full UFCS, (b) UFCS + Gleam first-arg `\|>`, (c) methods-only, (d) pipe-first without UFCS | (a) full UFCS now (S2, pure desugaring), (b) `\|>` deferred as an S1 additive; capability parameters are excluded from the dot position (docs/en/design/03 E1c) |
| F4 | Pattern matching | (a) Rust-shaped, (b) Kotlin 2.1 when, (c) Gleam-minimal, (d) OCaml when/as | (a) Rust-shaped arms with set-difference exhaustiveness diagnostics; `@` binders deferred pending docs/en/design/02 linearity |
| F5 | Operators | (a) fixed trait-driven set, (b) none (Go), (c) free + custom, (d) fixed + comptime-generated | (a) fixed token set on fixed traits, frozen precedence, trapping default with `+%`-family wrapping ops (docs/en/design/01 B7a); (c) is likely-rejected under the sugar ceiling |
| F6 | Strings and literals | (a) Kotlin `$x`/`${}`, (b) Swift `\(x)`, (c) format-function `{x}`, (d) format-only (Go) | (a) Kotlin spelling with explicit `str()` at the non-string boundary, plus Rust hash-counted raw strings (research/04 §7) |
| F7 | Sugar budget ledger | Minimal (~12) vs Balanced (~20, recommended) vs Generous (~26) tiers | Balanced: 22 rows / 35 units, leaving 16 features and 40-42 units of headroom under Kotlin's 38 / 74-76 ceiling (research/04 §2) |

## Shared assumptions (binding on every option)

- Sugar ceiling: Kotlin's surface, about 38 features and cost-weighted 74-76 units; cost scale S1 = one token, S2 = a construct or desugaring rule, S3 = new typing or cognitive semantics (research/04 §2.1-2.2). The recommended budget is 20 features or fewer, about 32 units (research/04 §1); F7 builds the ledger.
- Illustrative style: every code example outside F1 uses the F1(a) TS-family baseline — `const`/`var` bindings, `const f = (a: T) -> R { }` named functions, `(x) => e` lambdas, `name: T` annotations, `-> R` returns, final-expression returns, newline-terminated statements with no semicolons, `match e { pat => expr, }` with commas between arms — so the author compares options fairly. Style variants appear only in F1.
- Tokens with fixed semantics decided elsewhere: postfix `?` propagation, `or` extraction, `is` narrowing, `.err`/`.ok` difference-typed handlers (docs/en/design/03 E4b/B5d; research/04 §11); `T?` is a zero-semantic alias for `T | none` (docs/en/design/01 B4c); overflow traps by default with explicit `+%`-family wrapping operators (docs/en/design/01 B7a; research/04 §8); one structural `==`, auto-derived, no `===` (docs/en/design/01 B9a; research/04 §9).
- Full expression orientation: `if` and `match` are expressions in every option (research/04 §6); early-return sugar must stay expression-level to desugar cleanly.
- Block rule: the three brace options (F1.a/b/d) share newline-terminated statements with no semicolons in the grammar and trailing commas allowed everywhere (research/04 §6, §13 blocks option 4); F1.c replaces this with significant indentation.
- Functions are values in every option (the historical README: "each function is an object that could be assign, parse and modify"); F2 only decides the declaration spelling.
- Where-clauses and generic-parameter spelling are deferred to docs/en/design/05 (research/04 §6 recommends bounds on the parameter itself, no separate where-clause).

## F1 — Global shape: the four style families

What is decided here: the frame everything else hangs on — braces vs indentation, the function headline, and the overall read. The four programs below are reused from research/04 §12 (deliverable b) with one adaptation: Style C's `Just`/`Nothing` ladders are replaced by `or return`, exactly as research/04 §12 itself prescribes for a DreamScript version (Koka lacks union optionals; DreamScript has them per docs/en/design/01 B4). Styles A and B keep their `Ok(...)`/`Err(...)` spellings as the research wrote them; under docs/en/design/01 B1 unions plus docs/en/design/03 B5d those wrappers dissolve into plain union members (see F4), so this comparison is about shape, not wrapper policy. Program semantics are identical across all four; only the surface differs.

### F1.a — TS-family braces (`const f = (a: int) -> int { ... }`)

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

Prior art: the historical README's own function form; TS `const f = (x) => ...` plus Rust's match; demonstrated end to end as Style A in research/04 §12.

Tradeoffs: sugar cost S1 (brace blocks, newline termination); parser complexity the lowest of the four — every block opens with a visible token, the grammar is line-oriented, and TS-family tooling intuitions transfer; learning cost lowest for this author (research/04 §12: "the least novel surface of the four"). Risk noted by the research: arrow fatigue, and `const` doing triple duty as binding, function, and module-item keyword (F2 decides the split).

Forecloses: nothing in this catalog; it slightly disfavors a later `fun`-keyword rebrand (F2.b) since mixing two declaration forms buys nothing.

### F1.b — Gleam/Roc braces (`fn add(a: Int) -> Int { ... }`)

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

Prior art: ML-family surface in braces — Roc's new-compiler syntax (`|u|` lambdas, `?` postfix, https://raw.githubusercontent.com/roc-lang/roc/main/docs/mini-tutorial-new-compiler.md) and Gleam's tour; demonstrated as Style B in research/04 §12.

Tradeoffs: sugar cost S1; parser complexity equally low — research/04 §12 calls it "the most implementation-friendly (every construct is a trivial AST node)"; learning cost higher than (a) for this author: `fn`/`let`/`|x|` sigils are denser than TS arrows and no construct reuses TS muscle memory.

Forecloses: the arrow-lambda habit (`(x) => e`) in favor of `|x|`; adopting (a)'s arrow sugar later would split the language's lambda spellings.

### F1.c — Koka/Python significant indentation (`fun` / `val`, no braces)

```koka
type ParseError
  BadNumber(string)
  MissingField(string)

struct user
  name : string
  age  : int

fun parse_age( s : string ) : int | ParseError
  match s.trim.parse_int
    Ok(n)  -> n
    Err(_) -> BadNumber(s)

fun parse_user( row : Row ) : user | ParseError
  val name = row.get("name") or return MissingField("name")   // adapted per research/04 §12:
  val age  = row.get("age")  or return MissingField("age")    // the Just/Nothing ladder disappears
  User(name, parse_age(age)?)

fun main() : () | IoError
  val rows = load_rows("users.csv")?
  val (users, errors) = rows.map(parse_user).partition(fn(r) r is ParseError)

  val total = users
    .filter(fn(u) u.age >= 18)
    .map(fn(u) u.age * 2)
    .foldl(0, fn(a, b) a + b)

  for (u in users)
    println("user ${u.name} age ${u.age}")

  match (total, errors.first())
    (_, e) if e is ParseError -> println("warning: ${e}")
    (0, _) -> println("no valid users")
    (t, _) -> println("total score: $t")
```

Prior art: the Koka tour's own layout (https://koka-lang.github.io/koka/doc/book.html), demonstrated as Style C in research/04 §12; the braces-vs-indentation survey in research/04 §6 (Python/Nim structural complaints, Koka the lone healthy indentation language, Roc explicitly returning to braces in 0.14+).

Tradeoffs: sugar cost S2 plus an unpriced tooling burden (research/04 §13 blocks option 2); parser complexity the highest — significant whitespace needs Koka-grade whitespace-aware infrastructure in parser, formatter, REPL, and error recoverer (research/04 §6); learning cost moderate, but copy-paste and refactoring sensitivity is permanent (Python/Nim complaint record, research/04 §6).

Forecloses: cheap line-oriented tooling; machine-editing code (formatters, patches, generated snippets) becomes risky; the no-semicolon newline rule from shared assumptions is subsumed by layout.

### F1.d — Rust-family braces (`fn` + `let` + curly, impl blocks)

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

Prior art: daily Rust, demonstrated as Style D in research/04 §12 — "the control group: familiar, but its sugar is spent compensating for missing set types rather than on ergonomics" (the `Result`/`Option` double wrapping, `.ok_or(...)?` ladders, `matches!`/`unwrap`/`collect` noise).

Tradeoffs: sugar cost S1; parser complexity low but the impl-block layer adds a declaration nesting level (a); learning cost low for this author (Rust is daily practice), yet the style fights the set-type foundation — research/04 §12: Style D "loses to A/B on every measured axis except Rust familiarity".

Forecloses: union-carried errors (docs/en/design/03 B5d) read naturally; it re-imports the wrapper vocabulary (`Result`, `Some`, `.unwrap()`) that docs/en/design/01 B1 and research/04 §3 exist to delete.

### F1 comparison and verdict

| Style | Grep-ability | Diff-noise | Tooling simplicity | Author's TS taste | Overall |
|---|---|---|---|---|---|
| F1.a TS-family | good — definitions grep as `const name =` (collides with other const bindings; name-grep unaffected) | low — brace-only lines at most | highest — line-oriented parser, no layout state, ASI-free | highest — TS+bun muscle memory, README's own form | recommended |
| F1.b Gleam/Roc | best — `fn name(` is an unambiguous definition anchor | low — brace-only lines | highest — every form a trivial AST node (research/04 §12) | low-medium — ML sigils, no arrows | strongest if implementation simplicity wins |
| F1.c Koka/indent | medium — names grep, block extents do not | high — reindent and copy-paste ripple (research/04 §6) | lowest — whitespace-aware everything | lowest — no TS transfer | only with committed tooling |
| F1.d Rust-family | best — `fn name(` anchor plus impl blocks | medium — four-space brace blocks add lines | high, plus impl-block layer | medium — Rust familiarity only | control group; loses on set-type fit |

Interactions: the immutable-binding spelling interacts with docs/en/design/02 (C3, unread; path reference only) — all four styles show a two-keyword or marker-based split and none presumes 02's decision; the `?` / `or` / `is` tokens render identically in all four (docs/en/design/03 E4); the brace options inherit the no-semicolon newline rule (research/04 §6), so the real F1 delta is (c) versus everything else, then (a) versus (b)/(d) on headline spelling.

Recommendation: F1(a) TS-family braces — it is the historical README's own form, the author's stated taste, and the smallest grammar delta; keep F1(b) as the live fallback if prototyping shows the arrow-assignment grammar fights the interpreter's parser, and drop F1(c)/F1(d).

## F2 — Bindings, functions, annotations, returns

Four sub-decisions. The old README is prior art for (b) and (d) specifically: `const f = (a: int) -> int { ... }` with `?` as a return marker, and `-*>' as a generator-arrow kind marker.

### F2.a — The binding keyword pair under immutability-by-default

```ts
const port = 8080            // (1) const / var  — README + TS + Zig
var   hits = 0               //     mutation is the loud, local choice (charter req 5)

val   name = "ds"            // (2) val / var    — Kotlin, Swift, Koka
var   other = name

let   done = false           // (3) let / let mut — Rust, MoonBit
let   mut count = 0          //     `mut` as a modifier scales to graded mutability

const p2 = p(age = p.age + 1)  // (4) no reassignment at all — Koka copy-update (research/04 §13)
```

| Option | Pair | Prior art | Cost | Forecloses |
|---|---|---|---|---|
| F2.a.1 | `const` / `var` | old README; TS `const`; Zig (research/04 §6) | S1 | nothing in scope; loses the val/var rhyme |
| F2.a.2 | `val` / `var` | Kotlin, Swift, Koka (research/04 §6: "most self-documenting") | S1 | TS/README muscle memory |
| F2.a.3 | `let` / `let mut` | Rust, MoonBit (research/04 §13 mutability option 2) | S1 | two-word symmetry; gains a modifier that survives if docs/en/design/02 (C3) needs graded mutability markers |
| F2.a.4 | no reassignment; copy-update | Koka `p(age = ...)` (research/04 §13 option 4, S0) | S0 | imperative scripting style; loops that mutate accumulators |

Interactions: what `var` (or `mut`) means — reassignment only, deep mutation, or linear puts — is owned by docs/en/design/02 C3 (path reference only); the surface pair must compose with 02's markers, which favors (1)/(3) over a fixed rhyme like `val`/`var` if 02 lands graded. Recommendation: F2.a.1 `const`/`var` — the README's own word, TS habit, and a five-letter promise; revisit only if docs/en/design/02 (C3) needs more than one mutation marker.

### F2.b — The function form

```ts
const parse_age = (s: String) -> Int | ParseError {   // (1) const-arrow: old README, TS
  42
}

fn parse_age(s: String) -> Int | ParseError {         // (2) fn declaration: Rust, Zig, MoonBit, Gleam
  42
}

fun parse_age(s: String) -> Int | ParseError { ... }  // (3) fun/fn split: Koka, Kotlin — `fn` stays anonymous
```

| Option | Form | Prior art | Cost | Forecloses |
|---|---|---|---|---|
| F2.b.1 | `const name = (…) -> T { }`, anonymous `(x) => e` | old README §Function; TS (research/04 §6) | S1 | a dedicated definition keyword for grep; nothing material |
| F2.b.2 | `fn name(…) -> T { }` | Rust/Zig/MoonBit/Gleam (research/04 §6) | S1 | declaration-expression uniformity (README's functions-are-objects); risks a Rust-style fn-item vs closure split unless Gleam-style `fn`-everywhere |
| F2.b.3 | `fun` named + `fn` anonymous | Koka/Kotlin — research/04 §6: "the cleanest of the keyword families" | S1 | one-form-to-learn; two keywords to keep distinct |
| F2.b.4 | allow both (1) and (2) | Swift `func` + closures; TS `function` + const-arrow | S2 | budget discipline — two spellings for one concept, style wars |

Prior-art audit (old README): the generator arrow `-*>' (`const f = (a: int) -*> int`) encoded function kind in the arrow; the modern equivalents are a modifier keyword, a return-channel marker, or an effect row (docs/en/design/03 B5c). Generators have no decision point in this catalog and are deferred; the arrow-encodes-kind idea is recorded here so it is not silently lost. Interaction: docs/en/design/05 owns how the generic parameters of (1) would be spelled (`const f[T] = ...`). Recommendation: F2.b.1 const-arrow — uniform with bindings, matches the README's functions-are-values stance and the F1(a) baseline; if F1 lands elsewhere, this re-decides with it.

### F2.c — Annotation placement and the return arrow

```ts
const f = (a: Int, b: String) -> Bool { ... }   // (1) `name: T` params, `-> T` return (README, TS, Rust)
const g = (a: Int, b: String) : Bool { ... }    // (2) `: T` for both (Koka, Scala)
fn h(a Int, b String) Bool { ... }              // (3) postfix `name T` (Go, Zig)

type Mapper = (Int) -> Bool                     // function-type arrow matches the return arrow under (1)
const short: (Int) -> Bool = (n) => n > 0       // locals inferred; boundary signatures required
const maybe: Int? = row.get("port")             // `T?` alias = Int | none (docs/en/design/01 B4c)
```

| Option | Spelling | Prior art | Cost | Forecloses |
|---|---|---|---|---|
| F2.c.1 | `name: T` params + `-> T` return | old README; Rust/TS/Koka (research/04 §6: `name: T` dominates) | S1 | nothing |
| F2.c.2 | `: T` for params and return | Koka, Scala (research/04 §6) | S1 | the "arrow means produces" reading in function types `(A) -> B` |
| F2.c.3 | postfix `name T` | Go, Zig (research/04 §6) | S1 | TS taste; readable function types |
| F2.c.4 | annotations only at boundaries, full inference inside | research/04 §13 annotation option 3 | S0 surface, S3 inference | explicit local signatures; teaching story |

Shared sub-rule regardless of option: locals inferred, top-level and public signatures required (research/04 §6); union returns parse as `-> A | B` with no extra syntax. Interactions: `T?` is available in every annotation position as the docs/en/design/01 B4c alias; the generic-bounds spelling is deferred to docs/en/design/05. Recommendation: F2.c.1 plus the boundary rule — one colon meaning "has type", one arrow meaning "produces", both already in the README.

### F2.d — Return mechanics, and the `?` collision (flagged)

```ts
const area = (r: Rect) -> Int {
  r.w * r.h                    // (1) final expression is the value (Rust, Koka; README's own second form)
}

const find_user = (xs: List[User], id: Int) -> User | none {
  const hit = xs.find((u) => u.id == id)
  hit or return                // early exit via `or` (research/04 §11; docs/en/design/03 E4)
  hit
}

const old_readme = (a: int) -> int {
  ? a                          // (3) the README's `?` return marker — see the flag below
}
```

**Flag — token collision:** the old README used prefix `?` to return a value (and to finish a generator), but `?` is now the postfix error-propagation token with fixed semantics owned by docs/en/design/03 E4 and research/04 §11 (`e?` on `T | E` yields `T` or returns the error member early). One token would carry two unrelated meanings, disambiguated only by prefix-vs-postfix position — machines can parse that, humans and error messages cannot; the README `?` even did triple duty (return, generator finish, and its error examples). Resolutions: (i) final-expression only, `?` reserved for propagation; (ii) keep `?`-returns and demote propagation to a `try`-style keyword — forecloses the researched `?` semantics, adds an S2 keyword, and breaks consistency with docs/en/design/03; (iii) `?`-returns in tail position only — the worst of both, position-dependent meaning.

| Option | Mechanism | Prior art | Cost | Forecloses |
|---|---|---|---|---|
| F2.d.1 | final expression; early exits via `or return` / `or break` / `or continue` and `?` | Rust blocks, Koka; README's secondary form; research/04 §11 | S1 | a bare mid-function escape hatch (rare; `or` forms cover the union cases) |
| F2.d.2 | explicit `return` keyword only | TS, Go, Python | S1 | expression orientation pays rent; missing-return bugs return |
| F2.d.3 | README prefix `?` marker | old README §Function | S1 + collision | as flagged above; recommends retiring the marker entirely |
| F2.d.4 | final expression + optional `return` keyword | Rust, Kotlin | S1 | purity of "blocks are values"; small style schism |

Interactions: `or return` is not a new keyword — it is the docs/en/design/03 E4 `or` operator with an early-exit target (research/04 §11c); `?` propagation requires the enclosing return union to contain the propagated member (checked, no conversions). Recommendation: F2.d.1 final-expression returns with `?` reserved for propagation and the README `?` marker retired; F2.d.4's `return` can be added later as a pure S1 additive if real code demands it — it forecloses nothing.

## F3 — UFCS and pipelines

```ts
// the shared five-stage chain, four ways (research/04 §5)
s.trim.lower.split(",").map(clean).sum                    // UFCS: every dot is plain sugar
s |> trim |> lower |> split(",") |> map(clean) |> sum     // first-arg pipe: same desugaring
s.trim().lower().split(",").map(clean).sum()              // methods-only: trim/lower must be declared methods
sum(map(split(lower(trim(s)), ","), clean))               // pipe-free FP spelling
s.encode(3)                                               // verified Koka semantics: encode(s, 3)
```

| Option | Mechanism | Prior art | Cost | Forecloses |
|---|---|---|---|---|
| F3.a | full UFCS: `x.f(a)` = `f(x, a)` on any first-argument function | Koka tour, verified in research/04 §5; same dot-sugar in D and Nim — three production proofs, no confusion reports | S2 — one desugaring rule, zero runtime cost, no candidate search | nothing; requires the precedence rule below |
| F3.b | F3.a + Gleam first-arg pipe `x \|> f(a)` = `f(x, a)` | Gleam, verified https://tour.gleam.run/functions/pipelines and the v0.8 note (research/04 §5) | S1 — identical desugaring, pure syntax | nothing; risks two chaining idioms in one codebase |
| F3.c | methods only: `x.f()` requires `f` declared as a method of `x`'s type | TS, Rust, Go | S0 surface | adding "methods" to foreign types needs the docs/en/design/05 trait machinery; Rust's auto-ref/deref candidate search is the hidden S3 research/04 §5 warns about |
| F3.d | pipe-first without UFCS | Elixir `\|>` first-arg; F#/OCaml last-arg — "a currying artifact" (research/04 §5) | S1 | dot-chaining sugar entirely; last-arg variant fights first-arg UFCS if ever mixed |

Interactions: with traits (mechanism owned by docs/en/design/05) — UFCS resolution order is fixed and simple: free functions first, then trait methods, with `Trait::f(x)` qualification as the escape hatch; true ambiguity is a compile error, never a priority search (research/04 §5's critique of Rust's coherence-driven resolution). With effects and capability parameters (docs/en/design/03 E1c) — the rule fixed here: a dot-call feeds the first non-capability data parameter; capability parameters are named, excluded from the UFCS position, and passed explicitly or via a scope form whose spelling docs/en/design/03 E1 owns (`println(io, "done")` can never be written `msg.println()` swallowing `io`). With `T?` and safe-call — no `?.` operator; chained optional access composes as `x.f() or ...` and `is` narrowing (research/04 §11 explicitly keeps `?.` out of v1). Roc is the counter-evidence on F3.b: it deleted its pipeline as redundant with method chaining plus `?` (research/04 §5), which is why the pipe is `defer`, not `include`, in the F7 ledger.

Recommendation: F3(a) full UFCS now — pure desugaring the interpreter resolves once, with the capability-parameter rule above; F3(b) `|>` deferred as an S1 additive that breaks nothing.

## F4 — Pattern matching

```ts
type ReadError = Timeout | NotFound | Locked

const serve_all = (r: Config | ReadError) -> () {
  match r {
    Timeout           => retry(),                          // unit/tag patterns
    NotFound | Locked => println("unrecoverable"),         // or-patterns in one arm (research/04 §4)
    cfg               => serve(cfg),                       // cfg : Config — set difference did the work
  }
}

const check = (u: User) -> () {
  match u {
    { age } if age >= 18 => println("adult ${age}"),       // guard: `pat if cond` (Rust, Kotlin 2.1)
    { age }              => println("minor ${age}"),
  }
}

const first_or_skip = (xs: List[User | ParseError]) -> () {
  const h = xs.first() or return                           // `or` early exit (docs/en/design/03 E4)
  const msg = if (h is ParseError) "skipped: ${h}" else "ok: ${h.name}"
  println(msg)                                             // `h` narrowed by `is` in each branch
}

match load(p) {
  n       => serve(n),
  Timeout => retry(),
}
// error: match is not exhaustive; missing members: NotFound | Locked
//        coverage is set arithmetic: (Config | Timeout | NotFound | Locked) - Config - Timeout
```

| Option | Kit | Prior art | Cost | Forecloses |
|---|---|---|---|---|
| F4.a | Rust-shaped: `pat if guard`, or-patterns `\|`, `x @ pat` binders (deferred), exhaustiveness with set-difference diagnostics | research/04 §4 (Rust; Kotlin 2.1's guards are the same feature) | guards S1, or-patterns S1, binders S1, exhaustive-match S2 | nothing; diagnostics require union types (docs/en/design/01 B1) |
| F4.b | Kotlin 2.1 `when (v) { is Int -> ... }` with stabilized guards and subject binding | research/04 §2.2 item 9; https://kotlinlang.org/docs/whatsnew21.html | S2 (when-with-subject) | a second match keyword and `->` arms against the baseline's `=>`; otherwise equivalent power |
| F4.c | Gleam-minimal: no guards, whole-pattern comma lists, nested `if` in bodies | research/04 §4 table; the nesting-inflation cautionary tale in research/04 §6 | S0 sugar, but arms nest | flat decision tables; every conditional dispatch becomes an `if` inside an arm |
| F4.d | OCaml-style: `pat when cond` guards + `pat as x` binders | research/04 §4 (OCaml `when`/`as`; Swift's `where` is the same keyword slot) | S1 each, same as F4.a | nothing over F4.a except word choice; `as` reads longer than `@` |

Notes shared by the options: under set types the commonest guard (`u is User && u.age >= 18`) needs no binder at all because `is` narrows the original variable (research/04 §4), which is why `@`/`as` binders are a deletion candidate — keep them only if linear match ergonomics require re-binding a moved-out value, a question owned by docs/en/design/02 (path reference only). The set-difference diagnostic ("missing: NotFound | Locked") is better than any surveyed language because the missing members are literally a set computation (research/04 §4); if docs/en/design/01 lands the nominal-only B1.b instead, the same checker degrades gracefully to naming missing enum cases. Interactions with errors: `?` and `or` are the partial-handling layer and `match` is the total handler over the same union — one mechanism, no per-monad keywords (docs/en/design/03 E4b; research/04 §11). Historical README audit: its difference-typed `.err` handlers and context-typed `{1/0}` blocks are prior art for the non-match handling path — the block-typing mechanism was rejected as non-compositional (research/04 §11, flaw 1), while `.err`/`.ok` were salvaged as difference-typed combinators (docs/en/design/03 E4b; research/04 §11c); the README's `B - C` set-subtraction idea survives as exactly this diagnostic.

Recommendation: F4(a) Rust-shaped arms — guards `if`, or-patterns `|`, exhaustive unions with set-difference diagnostics — with `@` binders explicitly deferred to the docs/en/design/02 outcome.

## F5 — Operators

```ts
// (a) fixed trait-driven set: a + b desugars to Add.add(a, b); trait names owned by docs/en/design/05
trait Add { fn add(self, rhs: Self) -> Self }
const v3 = a + b - c * 2          // only the fixed tokens below can be given trait impls
const m  = xs[0]                  // Index-style get is in the fixed set (research/04 §10)

// (b) none: `+` exists only on builtin numerics; BigInt/complex read x.add(y)
// (c) free overloading + definable operators with fixity (Kotlin infix fun, C++, OCaml) — likely-rejected
// (d) fixed set on primitives + comptime generates Vector3.add(a, b) you call by name (Zig flavor)
```

| Option | Policy | Prior art | Cost | Forecloses |
|---|---|---|---|---|
| F5.a | fixed token set mapped to fixed traits, fixed precedence, nothing else overloads | Rust `std::ops` — research/04 §10: "the author's constraints pick Rust's slot" | S2 (one trait family + dispatch rule; operator call = trait-method call, inline-cacheable) | custom operator tokens forever; `Deref`-style coercions are excluded from user surface (research/04 §10) |
| F5.b | no user overloading; operators fixed to builtins | Go, Roc (new), Zig (research/04 §10) | S0 | `+` on BigInt, complex, vectors — hurts the docs/en/design/01 B7 tower extensions; DSL readability |
| F5.c | free overloading plus user-defined operators and fixity declarations | Kotlin conventions + `infix fun`; Swift precedence groups; C++, OCaml (research/04 §10, §13 option 4 "S3, rejected") | S3 — marked likely-rejected under the sugar ceiling | the charter's explainability budget; grep-ability (precedence becomes declaration-local) |
| F5.d | fixed set, primitives only, comptime generates per-type arithmetic called by name | Zig's comptime (research/04 §2.3) | S1 surface + comptime machinery priced in docs/en/design/05 G1 | operator syntax on user types; recovers boilerplate via codegen instead |

Overflow-operator spelling table (policy fixed by docs/en/design/01 B7a: trapping is the only predictable interpreter policy, research/04 §8; spellings follow Zig):

| Operation | Trapping (default) | Wrapping (explicit) | Notes |
|---|---|---|---|
| add / sub / mul | `a + b`, `a - b`, `a * b` | `a +% b`, `a -% b`, `a *% b` | trap panics with location; wrapping ops are S1 for hash/PRNG use (research/04 §8) |
| negate | `-a` | `-%a` | Int-min edge |
| div / rem | `a / b`, `a % b` | none | division by zero and Int-min/-1 always trap; no wrapping division exists |
| shifts | `a << k`, `a >> k` | none | trap when `k` exceeds bit width; rotations live in the library |
| float | IEEE semantics, never traps | n/a | inf/nan per IEEE (docs/en/design/01 B7a) |

Precedence policy options: (1) frozen conventional C-family table published once in the tutorial — no new operators, no re-precedencing, ever; (2) adopt Rust's current table verbatim including its quirks (postfix `?` chaining, range tokens) — equally frozen, maximally familiar to the author; (3) Swift-style precedence groups or OCaml fixity declarations — rejected with F5.c (S3, research/04 §13). Interactions: `==` is the structural equality of docs/en/design/01 B9a (not user-overloadable; ordering goes through the `Ord` derive, owned by docs/en/design/05); the wrapping `+%` family is the surface spelling of the docs/en/design/01 B7a decision and shares tokens with nothing else. Recommendation: F5(a) fixed trait-driven set with policy (1) frozen precedence, plus the overflow table above; F5(d)'s comptime route belongs to docs/en/design/05 G1 and can layer on later without breaking F5(a).

## F6 — Strings and literals

```ts
const name = "dream"
const n = 3
const a = "hello $name, ${n} apples"       // (a) Kotlin: bare id + one brace form (research/04 §7)
const b = "hello \(name)"                  // (b) Swift: paren form
const c = println("{name} has {n}", name, n)  // (c) placeholders live in the format call, not the literal
const d = "hello " + name                  // (d) concat only (Go fmt.Sprintf, Gleam <>)

const loud = "${name.str().upper()}"       // (a) explicitness rule: non-strings call str() visibly
```

| Option | Mechanism | Prior art | Cost | Forecloses |
|---|---|---|---|---|
| F6.a | `$x` bare identifier, `${expr}` for expressions, explicit `str()` at the non-string boundary | Kotlin — "the winner on every axis except implicit stringification" (research/04 §7; Roc's explicit rule adopted at the boundary) | S1 | hidden-allocation auto-stringify (can be added later via a Display-like trait, docs/en/design/05) |
| F6.b | `\(x)` always parenthesized | Swift (research/04 §7: paren collides with tuple syntax) | S1 | nothing, but two characters per interpolation and sigil soup next to tuples |
| F6.c | `{x}` placeholders captured in a format function | Rust `format!` capture, Zig `std.fmt`, Python f-string specs (research/04 §7) | S1 | interpolation inside plain string literals; invites a format-spec mini-language |
| F6.d | no interpolation; format-only or concat | Go `fmt.Sprintf`, Gleam `<>`, OCaml `Printf` (research/04 §13 string option 4) | S0 syntax, S2 verboseness | literal readability; format strings are stringly-typed unless checked — against the charter's compile-time-errors taste |

Raw strings, multiline, heredocs — one construct should cover all three (research/04 §7):

```ts
const re  = r"\d+\.\d+"                     // raw: no escape processing
const sql = r#"SELECT * FROM "users" WHERE k = 'v'"#   // hash-counted: any quotes, nests to any depth
const doc = r#"
line one
line two
"#                                          // the same construct serves as heredoc/multiline
```

Rust's hash-counted `r#"..."#` wins on nesting and embedding code/quotes without escape ambiguity; triple quotes (`"""..."""`, Kotlin/Python/Swift) are the familiar alternative; Go backticks cannot embed backticks; Zig's `\\` line form is weakest (research/04 §7). Cost S1 either way. Character and byte literals: `'x'` is one Unicode scalar value (`Char`, stored per docs/en/design/01 B8a UTF-8), `b'x'` is a single `u8` byte for wire formats — S1, matching Rust's pair. Interactions: string storage and checked slicing (`s.slice_char(i, j) -> String | none`) are owned by docs/en/design/01 B8a; the `str()` boundary function is the `Show`-family trait's projection, named in docs/en/design/05; `T?`-valued interpolations still require explicit handling — no silent "optional" printing. Recommendation: F6(a) Kotlin spelling with the explicit `str()` boundary rule, plus Rust hash-counted raw strings and the `'x'` / `b'x'` literal pair — all S1, all matching research/04 §7's verdicts.

## F7 — The sugar budget ledger

The ledger prices every candidate affordance on the S1/S2/S3 scale (research/04 §2.1), marks it include / exclude / defer per tier, and sums against Kotlin's 38-feature / 74-76-unit ceiling (research/04 §2.2). "Defer" means not in the launch budget but additive without breakage.

| # | Affordance | S | Minimal | Balanced | Generous | Rationale (one line) |
|---|---|---|---|---|---|---|
| 1 | Newline-terminated statements, no semicolons | S1 | in | in | in | research/04 §6: cheapest rule, deletes the ASI hazard class (F1.c swaps this row for indentation S2) |
| 2 | String interpolation `$x` / `${e}` | S1 | in | in | in | research/04 §7 winner; explicit `str()` boundary |
| 3 | Raw/multiline strings `r#"..."#` | S1 | in | in | in | research/04 §7: one construct for raw + heredoc, nests unambiguously |
| 4 | Postfix `?` propagation | S2 | in | in | in | docs/en/design/03 E4; research/04 §11: containment-checked, no hidden conversions |
| 5 | `or` extraction + `or return/break/continue` | S2 | in | in | in | research/04 §11 (Phi): replaces elvis, `??`, `unwrap_or`, `orelse` |
| 6 | `is` narrowing on locals | S2 | in | in | in | research/04 §3: highest-leverage item; needs immutable default (charter req 5) |
| 7 | `.err(h)` / `.ok(h)` difference-typed handlers | S2 | out | in | in | docs/en/design/03 E4b; the old README's `.err` made compositional (research/04 §11); Minimal uses `or` + match |
| 8 | match/if expressions, exhaustive unions, missing-set diagnostics | S2 | in | in | in | research/04 §3-§4: coverage is set arithmetic; the diagnostic is a set computation |
| 9 | Match guards `pat if cond` | S1 | in | in | in | research/04 §4: guardless Gleam nests (the §6 cautionary tale) |
| 10 | Or-patterns `a \| b` | S1 | out | in | in | research/04 §13: duplicate arms are the Minimal S0 substitute |
| 11 | Destructuring bindings | S2 | out | in | in | research/04 §2.5 keep-set; Minimal reads fields directly |
| 12 | Named + literal-only default arguments | S2 | out | in | in | research/04 §6: covers ~90 percent, dodges typst#3052; Koka lazy defaults (S3) rejected |
| 13 | Trailing lambda (last argument only) | S2 | out | in | in | research/04 §6: Kotlin-restricted form, not Koka's context-sensitive braces-thunks |
| 14 | UFCS `x.f(a)` = `f(x, a)` | S2 | in | in | in | research/04 §5: pure desugaring, three production proofs; F3(a) |
| 15 | Type aliases | S1 | in | in | in | research/04 §2.2 item 26: unions are unreadable unnamed |
| 16 | Sum-type declaration (`tag`/enum) | S2 | in | in | in | owned by docs/en/design/01 B3; counted here for an honest global budget |
| 17 | Derive attribute (`#[derive(Show, Ord)]`; `==` auto) | S2 | out | in | in | one token of surface; semantics owned by docs/en/design/05; `==` auto per docs/en/design/01 B9a |
| 18 | Ranges `..` `..=` + `in` (for/contains) | S1 | in | in | in | research/04 §13 ranges option 1; `in` rides the same token family |
| 19 | Numeric literal defaults + suffixes `1u8` + separators `1_000_000` | S1 | out | in | in | docs/en/design/01 B7a; research/04 §8 |
| 20 | Mutability split `const`/`var` | S1 | in | in | in | F2.a; `var` semantics owned by docs/en/design/02 C3 |
| 21 | Operator-overload traits (fixed set, fixed precedence) | S2 | out | in | in | research/04 §10: the Rust slot; F5(a) |
| 22 | Labeled break/continue (loops only) | S1 | out | out | in | research/04 §2.5; not Kotlin's general `return@map` labels |
| 23 | Lambda shorthand `it` | S1 | out | out | in | research/04 §2.2 item 24; filter/map comfort |
| 24 | First-arg pipeline `\|>` | S1 | out | defer | in | research/04 §5: same desugaring as UFCS; Roc deleted its pipe as redundant — F3(b) |
| 25 | Anonymous record literals | S2 | out | defer | in | docs/en/design/01 B3d sugar; defer until the inference story settles |
| 26 | `T?` alias | S2 | out | in | in | docs/en/design/01 B4c: zero-semantic alias for `T \| none`, deletable at will |

Substitution rule: dropping row 2 for the format-function placeholder style (F6.c/d, research/04 §13 string option 3) is unit-neutral — S1 for S1.

| Tier | Features | Units | Headroom vs 38 features | Headroom vs 74-76 units |
|---|---|---|---|---|
| Minimal | 12 | 18 | 26 | 56-58 |
| Balanced (recommended) | 22 | 35 | 16 | 40-42 |
| Generous | 26 | 40 | 12 | 34-36 |
| Kotlin ceiling (research/04 §2.2) | 38 | 74-76 | — | — |

Balanced lands at 35 units against research/04 §1's "about 32", two features over 20 because guards, or-patterns, and `.err`/`.ok` are priced as their own rows exactly as research/04 §13 prices them; merging any two (they share one tutorial paragraph each) recovers the letter of the budget. Reconciling the Kotlin features deleted wholesale by set typing and by other catalogs:

| Kotlin # (research/04 §2.2) | Feature | Fate | Replaced by |
|---|---|---|---|
| 4 | safe call `?.` | excluded from v1 | `is` narrowing + `or` (research/04 §11: add a thin `x?.f` desugar later only if real code demands) |
| 5 | elvis `?:` | deleted | `or` (research/04 §11) |
| 6 | not-null assertion `!!` | deleted | none-typing + `or` (docs/en/design/01 B4) |
| 7 | nullable suffix `T?` | alias only (ledger row 26) | zero-semantic alias (docs/en/design/01 B4c) |
| 8 | smart casts | deleted as a separate feature | `is` flow narrowing (research/04 §3) |
| 12 | operator conventions | replaced | fixed trait set, F5(a) (research/04 §10) |
| 13 | infix functions | excluded | UFCS + named functions (research/04 §5) |
| 14, 15 | extension functions / properties | deleted | UFCS, F3(a) |
| 16, 17, 18 | accessor properties, property delegates, class delegation | excluded | plain fields + functions + derives; charter explainability budget |
| 19 | data class synthesis | replaced | derives (docs/en/design/05) + structural `==` (docs/en/design/01 B9a) |
| 21, 22 | default / named arguments | restricted | literal-only defaults + labels (ledger row 12) |
| 25 | receiver lambdas (DSL scope) | excluded | Koka's generalized braces-as-thunks is S3, rejected (research/04 §6) |
| 27 | sealed hierarchies + exhaustive when | replaced | closed unions (docs/en/design/01 B1/B3); ledger row 8 |
| 28 | object expressions, companion | excluded | module-level bindings (modules owned by the modules catalog, research/06) |
| 29 | primary constructor | excluded | plain struct literals + init functions (docs/en/design/01 B3d) |
| 30 | value/inline classes | defer | docs/en/design/01 owns wrapper types |
| 31 | variance syntax | defer | docs/en/design/05 owns generics |
| 32 | reified type parameters | rejected | S3 with no set-type payoff (research/04 §2.2) |
| 33 | vararg + spread | defer | additive S1; stdlib can launch without it |
| 34 | labels | restricted | loops only, Generous (ledger row 22) |
| 35 | backtick identifiers | excluded | naming discipline is cheaper than an escape hatch |
| 36 | underscore placeholders | free | part of patterns, not a separate feature (research/04 §13) |
| 37 | `tailrec` modifier | unnecessary | TCO is a charter hard requirement (AGENTS.md req 3) |
| 38 | context parameters | replaced | capability parameters, docs/en/design/03 E1c |

Interactions: rows 4-8 are the docs/en/design/03 E4b token set and docs/en/design/01 B1 unions — delete unions and rows 5-8 lose their subject; row 20's meaning is docs/en/design/02 C3's to define; row 21's trait names and row 17's derive list are docs/en/design/05's to name; row 19 is the surface of docs/en/design/01 B7a. Recommendation: adopt the Balanced tier — it keeps DreamScript at roughly half of Kotlin's feature count and 43 percent of its cost-weighted surface while covering every construct the F1-F6 recommendations use.

## Coherent combos

Three named surface packages, each internally consistent and each within the ceiling. Budget totals count ledger rows and units under that package's F1 choice (F1.c swaps ledger row 1 for indentation S2).

### Combo 1 — "TS-heart" (recommended): F1a + const/var + const-arrow fns + UFCS + Rust-shaped patterns + fixed-trait operators + Kotlin interpolation + Balanced ledger

```ts
// TS-heart: F1a + F2(const/var, const-arrow, : T + -> T, final-expression)
//           + F3a UFCS + F4a Rust-patterns + F5a fixed traits + F6a interpolation + Balanced (22 rows / 35 units)

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

Budget: Balanced ledger unchanged — 22 rows, 35 units; headroom 16 features / 40-42 units.

### Combo 2 — "Gleam-simple": F1b + fn/let declarations + UFCS + first-arg pipe + format-only strings + Minimal ledger

```roc
// Gleam-simple: F1b + F2(fn, let/let mut, : T + -> T, final-expression)
//           + F3b UFCS + pipe + F4a patterns + F5b no overloads + F6d format-only
//           + Minimal (12 rows / 18 units; interpolation row swapped for the format-function row, S1 for S1)

type ParseError {
  BadNumber(String)
  MissingField(String)
}

fn parse_age(s: String) -> Int | ParseError {
  match s.trim().parse_int() {
    e if e is ParseError -> BadNumber(s)
    n -> n
  }
}

fn parse_user(row: Row) -> User | ParseError {
  let name = row.get("name") or return MissingField("name")
  let age = row.get("age") or return MissingField("age")
  User { name, age: parse_age(age)? }
}

fn main() -> () | IoError {
  let rows = load_rows("users.csv")?
  let (users, errors) = rows
    |> map(parse_user)
    |> partition(fn(r) r is ParseError)

  let total = users
    |> filter(fn(u) u.age >= 18)
    |> map(fn(u) u.age * 2)
    |> reduce(0, fn(a, b) a + b)

  for u in users {
    println(format("user {} age {}", [u.name, u.age.str()]))
  }

  match (total, errors.first()) {
    (_, e) if e is ParseError -> println(format("warning: {}", [e]))
    (0, _) -> println("no valid users")
    (t, _) -> println(format("total score: {}", [t]))
  }
}
```

Budget: Minimal ledger with row 2 swapped for format-function placeholders — 12 rows, 18 units; headroom 26 features / 56-58 units; the cheapest package to implement and explain.

### Combo 3 — "Indent-Koka": F1c + fun/val + full UFCS + Rust-shaped patterns + Balanced ledger

```koka
// Indent-Koka: F1c + F2(fun, val/var, : T both, final-expression)
//           + F3a full UFCS + F4a patterns + F5b no overloads + F6a interpolation
//           + Balanced with row 1 swapped for indentation S2 (22 rows / 35 units + tooling burden)

type ParseError
  BadNumber(string)
  MissingField(string)

struct user
  name : string
  age  : int

fun parse_age( s : string ) : int | ParseError
  match s.trim.parse_int
    e if e is ParseError -> BadNumber(s)
    n                    -> n

fun parse_user( row : Row ) : user | ParseError
  val name = row.get("name") or return MissingField("name")
  val age  = row.get("age")  or return MissingField("age")
  User(name, parse_age(age)?)

fun main() : () | IoError
  val rows  = load_rows("users.csv")?
  val parts = rows.map(parse_user).partition(fn(r) r is ParseError)
  val (users, errors) = parts

  val total = users
    .filter(fn(u) u.age >= 18)
    .map(fn(u) u.age * 2)
    .foldl(0, fn(a, b) a + b)

  for (u in users)
    println("user ${u.name} age ${u.age}")

  match (total, errors.first())
    (_, e) if e is ParseError -> println("warning: ${e}")
    (0, _)                    -> println("no valid users")
    (t, _)                    -> println("total score: $t")
```

Budget: Balanced with the indentation swap — 22 rows, 35 units, plus the unpriced whitespace-aware tooling burden (research/04 §6); choose only if the author commits to Koka-grade editor and formatter infrastructure.

## Sources

All evidence, URLs, and the local-clone citations for this file are in research/04 §14; claims above cite research/04 by section. Sister-catalog constraints come from the recommendation tables of docs/en/design/01-types.md (B1/B3/B4c/B7a/B8a/B9a) and docs/en/design/03-effects-errors-async.md (B5d/E1c/E4b). The historical README.md is prior art only and intentionally untouched.
