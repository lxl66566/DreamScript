# 05 — Polymorphism and Metaprogramming

Research file for DreamScript design, covering polymorphism forms, set-theoretic types with overloading, the anti-specialization constraint, compile-time metaprogramming, the derive/equality story, performance representation in bytecode, and interplay with linear types. Written 2026-11-04. Evidence comes from primary docs, papers, and two source trees available locally (`moonbit-core`, `koka` clones in the repo root, read-only). Companion files: 01 (linearity/ownership), 02 (effects), 03 (runtime/IR/JIT). DreamScript inputs assumed: interpreted (bytecode IR, optional JIT), static linear/affine types, immutable by default, set-theoretic types with no overlapping generic matches, no C++-style specialization, sugar ceiling = Kotlin, Koka-style UFCS, monadic Option/Result.

## Executive summary

Specialization is the thing the author rejects, and the evidence says rejecting it is cheap: overlapping impls are the only reason specialization exists, and Rust has failed to make overlapping impls sound for ten years (RFC 1210 accepted 2015; tracking issue rust-lang/rust#31844 still open in 2026 with the words "can cause undefined behavior without unsafe code"; even `min_specialization` was reported unsound again in 2025 by issue #149257). A language where generic instances cannot overlap never needs to pick a "most specific" match, so it needs neither specialization nor priority rules nor the coherence lawyer.

Every polymorphism mechanism decomposes into two orthogonal decisions: (a) how instances are found (implicit global resolution vs explicit local passing) and (b) how calls execute (monomorphization, dictionary/vtable passing, or dynamic dispatch with caching). The pain points of Rust and Haskell come from decision (a) — global implicit resolution forced coherent by an orphan rule — not from decision (b). This decomposition is the main structural insight of this file; section 3 evaluates each point on axis (a) and section 6 each point on axis (b).

Set-theoretic types compose cleanly with overloading if overloads are required to be pairwise disjoint (emptiness of `T1 & T2` checked at definition time). This is formally grounded: "Resolution as Intersection Subtyping via Modus Ponens" (OOPSLA 2020) shows instance resolution is just intersection subtyping; MLscript/MLstruct (Boolean-algebra types, OOPSLA 2022) explicitly list type classes as a compatible extension; disjoint-intersection merges stay coherent via apartness (OOPSLA 2025). Castagna's semantic subtyping line (CDuce, POPL 2015, the Elixir type system) already ships set-based overload resolution in production languages.

The derive problem has a cheap answer fitting the anti-specialization stance: structural equality on immutable data is total except for functions, floats, cyclic structures, and linear resources. Evidence: Roc ships structural equality with no derive at all (with a separate `is_float_eq` for IEEE semantics — verified in the builtin docs); MoonBit ships `#derive(Eq, Hash, Show, ToJson)` attributes (verified pervasively in moonbit-core, which also documents the NaN caveat on derived equality in `builtin/traits.mbt`); Swift synthesizes `Equatable`/`Hashable` from declarations (SE-0185); OCaml ships a universal polymorphic structural compare.

Metaprogramming: the minimal set that covers all derive-like needs is (1) static reflection over type structure (fields/cases), (2) compile-time evaluation of ordinary code (Zig `comptime`, D CTFE, Mojo `comptime`), and (3) built-in derive attributes for the hot cases. Token macros (Rust proc macros, Nim, Lisp) are strictly more machinery and exist mainly because those languages lack static reflection; C++ is converging to the same conclusion (C++26 static reflection, P2996). Reflection-based metaprogramming also deletes the hygiene question entirely.

Performance for an interpreted language: never monomorphize bytecode; keep one generic body, pass evidence (dictionaries/vtables) as arguments, cache resolution at call sites with inline caches (Self/V8 style), and lazily materialize per-type specializations in the JIT tier (Julia-style devirtualization). This is compiler-internal specialization that preserves observable semantics exactly; the anti-specialization stance is about source-level semantics (which impl a call means), not optimizer bookkeeping, so it is compatible.

HKT can be deferred for v1: Kotlin shipped mainstream without HKT (KEEP-87 stalled; Arrow abandoned the simulation encoding), Swift worked without HKT for a decade (opaque types + primary associated types), and Rust survives without trait-level HKT (GATs cover the lending-iterator niche but are widely called hard to learn). If effects are row/capability-based (file 02) and Option/Result get concrete `map`/`and_then` methods, the classic HKT drivers disappear.

Five coherent combos are sketched in section 9: (A) coherent set-theoretic traits + built-in derives + evidence passing with inline caches; (B) fully explicit capabilities with zero implicit resolution (Effekt-style); (C) ML modules/1ML; (D) Zig-style comptime generics; (E) Julia-style dynamic multiple dispatch with JIT devirtualization. The strongest fit for the stated constraints is A, with B-style scoped named instances as the extension hatch and D-style reflection as the metaprogramming substrate instead of macros.

## 1. Polymorphism forms: a survey

Each mechanism is distilled to two axes: its resolution rule (where does the compiler find the instance?) and its execution rule (how does the call run?). Languages differ mainly on axis (a); axis (b) variations are mostly implementation strategy (section 6).

### 1.1 Rust traits: coherent global type classes

Rust traits are global, implicit, coherent type classes: for a given trait and type pair, at most one impl may exist in the whole program. Coherence is enforced by the orphan rule — an impl may be written only in the crate that defines the trait or the crate that defines the self type — with the documented relaxation that `#[fundamental]` types (`&T`, `Box<T>`, `Pin<P>`) do not count as local, and blanket impls (`impl<T> Trait for Vec<T>`) are allowed only when the parameters are covered by local types. Resolution is compile-time: at every generic call site the compiler selects the unique impl, constructs its dictionary (a record of function pointers for the impl's methods and supertrait methods), and either statically dispatches after monomorphization or packs the dictionary into a vtable for `dyn Trait`.

```rust
trait Draw { fn draw(&self) -> String; }
struct Button;
impl Draw for Button { fn draw(&self) -> String { "btn".into() } }

// blanket impl: coherent only because no other crate can impl Draw for Button
impl<T: Draw + ?Sized> Draw for Box<T> {
    fn draw(&self) -> String { (**self).draw() }
}

fn draw_static<D: Draw>(d: &D) { d.draw(); }   // universal: monomorphized per D
fn draw_exist(d: &dyn Draw) { d.draw(); }      // existential: vtable dispatch
fn opaque() -> impl Draw { Button }            // universal, hidden concrete type
```

`impl Trait` in return position produces an opaque type (one fixed concrete type per return site, statically dispatched); `dyn Trait` is an open existential (boxed, vtable dispatch, dynamic type checks possible). This is the same `some`/`any` distinction Swift made explicit (section 1.3). Associated types (`type Item`) make type families explicit and keep inference single-valued: exactly one `Iterator` impl per self type, so `iter.next()` infers `Item` from the self type alone. Generic associated types (GATs, stabilized in Rust 1.65) generalize associated types over lifetimes and type parameters; the real-world usage survey shows they are used mainly for lending iterators and borrowing-capable collection traits, and the GAT initiative's own design docs flag "too hard to learn" as an adoption concern — evidence that even partial HKT carries a real comprehension tax.

Blanket impls create the well-known ecosystem deadlock: once any crate writes `impl<T: FromStr> MyTrait<T>`, no crate can later write `impl MyTrait<&str>`, because the two impls overlap and coherence forbids overlap — the compiler must conservatively assume foreign types may gain impls in the future (this exact scenario is documented in the #31844 discussion: specializing `Simple<'a> for &'a str` against a blanket `impl<'a, T: FromStr> Simple<'a> for T` fails to compile because `&str` might implement `FromStr` someday). So coherence's rule shape is "forbid overlap statically, pessimistically" — DreamScript's opposite number is "prove non-overlap statically, precisely, using the set algebra".

Specialization history in one narrative: RFC 1210 (2015) allowed overlapping impls resolved by most-specific-match and was accepted "to much fanfare", then stalled. The blocking soundness problem is lifetime erasure — Matsakis 2018: "code generation wants to erase regions, but the type checker doesn't" — plus impls whose predicates are weaker than the impl they refine (the specialized code runs on values that violate assumptions the base impl's callers rely on), plus the future-impls conservatism above. The 2018 "always applicable impls" proposal restricted specializable impls to ones whose constraints are provable from well-formedness alone (each type parameter used exactly once, lifetime-generic, only implied bounds). `min_specialization` (PR #68970, 2020) restricted further: functions only (no assoc types/constants — they are evaluated during type checking and codegen, and mismatches there are UB-shaped), each generic parameter used at most once in the refined substitutions, all predicates of the specialized impl implied by the base impl. Even so, issue #31844 states the feature as implemented "can cause Undefined Behavior without unsafe code", and in 2025 issue #149257 reported `min_specialization` itself unsound via a trivially-false lifetime bound. Ten years, two partial designs, still no sound general specialization. This is the strongest single piece of external evidence for the author's "no overlapping matches" constraint: the problem is not implementation difficulty, it is that overlap plus subtyping plus generics makes "which impl applies" undecidable-in-practice at the boundary of two codebases.

What the standard library uses specialization for is instructive: `min_specialization` is used internally for performance refinements like specializing `Clone` for `Vec<T>` where `T: Copy` (memcpy instead of element-wise clone) — i.e., specialization in Rust exists to let the optimizer know a runtime-falsifiable predicate (is this element copyable?) at compile time. In a bytecode interpreter, the same effect is achieved by an inline cache on the copy operation plus a per-type fast path — no language feature needed (section 6).

### 1.2 Haskell type classes: dictionary passing with coherence leaks

GHC type classes resolve through a global instance table: for a constraint `C t`, GHC collects all instance declarations whose head matches `C t` and requires exactly one match, with three escape hatches that are all coherence leaks. `{-# OVERLAPPING #-}` / `{-# OVERLAPPABLE #-}` / `{-# INCOHERENT #-}` pragmas let the programmer opt into multiple matching instances (most-specific wins under OVERLAPPING; INCOHERENT picks arbitrarily); orphan instances (an instance for class `C` and type `T` declared in a module defining neither) make instance availability depend on the import graph — the same program can behave differently depending on which modules are linked — and Safe Haskell explicitly forbids orphans and `INCOHERENT` in `Safe` modules because they break the safety guarantee. The HaskellWiki orphan page recommends the convention of isolating orphans in dedicated `Orphan()` modules just so they can be banned from import together.

```haskell
class Eq a where
  (==) :: a -> a -> Bool

instance Eq a => Eq [a] where        -- compiles to: eqList :: Eq a -> Eq [a]
  xs == ys = ...                     -- a dictionary-taking function

-- orphan instance: defined in a module that owns neither Eq nor MyType
instance Eq Foreign where ...
```

Execution is dictionary passing: instances compile to records of functions (possibly with supertrait dictionaries embedded), constraints become extra dictionary parameters on every polymorphic function, and GHC erases them only via `INLINE`/`SPECIALIZE` pragmas guided by the programmer — dictionary elimination is a user-visible performance chore, and Haskell performance folklore is full of "forgot to SPECIALIZE the dictionary" stories. Lesson for DreamScript: dictionary passing is the natural runtime representation (evidence as ordinary values, composable), but global implicit resolution with orphan freedom is the incoherence source; the escape hatches GHC ships (overlapping pragmas, INCOHERENT) are exactly the specialization-shaped complexity the author rejects, and their existence shows what happens when you allow overlap without a set algebra to prove disjointness.

### 1.3 Swift protocols: conformances, existentials, and synthesis

Swift protocols combine static generics, existentials, and associated types under one conformance table. The vocabulary: `some P` is an opaque type (static dispatch, hidden concrete type, one fixed type per declaration site — a universal with inference), `any P` is an existential (boxed, dynamic dispatch through a witness table, dynamic casting possible); associated types are declared with `associatedtype` and constrained; primary associated types (SE-0358, 2022) let protocols be written like parameterized generics (`Sequence<Element>`, `Collection<Element>`) in most positions via lightweight same-type requirements (SE-0346); implicitly opened existentials (SE-0352, 2022) let an `any P` value flow into a generic parameter without the programmer unpacking it. Protocol conformances are recorded globally (retroactive conformances are allowed but produce library-evolution warnings — Swift's soft orphan rule).

```swift
protocol Collection { associatedtype Element; mutating func next() -> Element? }

func f(c: some Collection<Int>) { }   // static dispatch, concrete type hidden
func g(c: any Collection<Int>) { }    // existential: boxed, witness-table dispatch

func firstTwo<S: Collection>(s: S) -> [S.Element] { ... }  // universal
firstTwo(c)                             // any Collection<Int> implicitly opened (SE-0352)
```

Swift's most transferable feature for DreamScript is conformance synthesis (SE-0185, 2017): declaring `struct P: Equatable {}` makes the compiler synthesize `==` and `hash(into:)` when all stored properties conform; `Codable` synthesizes `init(from:)`/`encode(to:)` the same way; enums without associated values get `Equatable` automatically; conditional conformances (`extension Array: Equatable where Element: Equatable`) make synthesis compose over generics. The pattern to note: conformance is explicit (opt-in at the type declaration, visible in the API), but method bodies are derived (no user-written boilerplate, no macro system involved). This middle point between Rust's derive macros and Roc's built-in structural equality is implementable by any compiler that already knows the type's field layout — no metaprogramming machinery required.

### 1.4 OCaml modules and functors (and 1ML)

OCaml's second axis of abstraction is the module system: a module is a named bundle of types and values, constrained by a signature; a functor takes modules as arguments and returns modules. Instances are explicit values passed at module application; there is no implicit resolution and no coherence constraint — two applications of `Map.Make` with different comparator modules yield two distinct abstract map types (generative functors), so multiple instances coexist without ambiguity by construction. First-class modules (the `(module S)` packing of a module into a runtime value) give explicit existentials: a value plus its dictionary traveling together, unpacked when needed.

```ocaml
module type ORDERED = sig
  type t
  val compare : t -> t -> int
end

module Map (O : ORDERED) : sig
  type key = O.t
  type t
  val add : key -> t -> t -> t
  val find : key -> t -> t option
end = struct ... end

module StringMap  = Map (StringOrd)    (* explicit instantiation 1 *)
module ReverseMap = Map (ReverseOrd)   (* explicit instantiation 2: coexists, no conflict *)
```

Rossberg's 1ML (ICFP 2018) proves the ML core language and the module language can be unified: ordinary functions are functors, first-class modules are just first-class types, and the semantics is a principled system-F encoding ("F-ing modules"). The demonstration value for DreamScript: explicit capability passing scales to everything — generics, existentials, abstract types — with zero ambiguity and zero coherence machinery. The cost is language surface: signatures, functors, applicative vs generative distinction, and instantiation verbosity; this is why OCaml's ecosystem grew `ppx` code generation and why the derive story there is weak (section 5). Note also OCaml's third path: a universal polymorphic structural compare `=` built into the language for all data (section 5.1) — the derive problem solved at the language level rather than by any abstraction mechanism.

### 1.5 Scala givens: scoped deterministic resolution

Scala 3's `given`/`using` is type-directed implicit resolution with lexical scoping: a context parameter `(using ord: Ord[T])` is resolved by searching the lexical scope (inner definitions first, then imports, then inherited); if two eligible givens of the same type are in scope at the same priority, the compiler reports an ambiguity error rather than choosing — deterministic selection, no overlap by default, and the `@annotation.priority` mechanism exists only for migration cases. Anonymous givens get compiler-synthesized names derived from their types (`given_Ord_Int`); the docs recommend named givens for public libraries (binary stability); conditional givens (`given [T: Ord] => Ord[List[T]]`) are resolution rules, so resolution is a recursive proof search that always terminates deterministically (given the ambiguity-as-error rule).

```scala
trait Ord[T]:
  def compare(x: T, y: T): Int

given intOrd: Ord[Int]:
  def compare(x: Int, y: Int) = if x < y then -1 else if x > y then 1 else 0

def max[T](x: T, y: T)(using ord: Ord[T]): T =
  if ord.compare(x, y) < 0 then y else x

given listOrd: [T: Ord] => Ord[List[T]]:   // conditional given: resolution rule
  def compare(xs, ys) = ...

// two competing givens of Ord[Int] in one scope = compile error, not a pick
```

Scala is the closest mainstream analogue to "scoped instances": coherence is preserved by lexical determinism and ambiguity-as-error, not by a global orphan rule; the same type can have multiple instances alive in different scopes simultaneously. The historical cost: "implicits hell" — resolution depending on subtle scope/priority interactions — which Scala 3 tamed exactly by tightening scoping rules and erroring on ambiguity. The lesson transfers: scoped resolution is coherent if and only if ambiguity is an error and priority is lexical, never heuristic.

### 1.6 Kotlin: interfaces + extension functions + context parameters, no HKT

Kotlin has no type classes and no HKT: KEEP-87 (higher-kinded types) has been stalled since 2017 — never accepted, not on the roadmap — and Arrow, the flagship FP library, abandoned its `Kind<F, A>` HKT simulation encoding after years of maintenance burden (a community compiler plugin `highKT` exists as a workaround). What Kotlin ships instead: interfaces with default methods (vtable dispatch), extension functions (statically resolved sugar — `x.f(y)` desugars to the top-level `f(x, y)` resolved by import scope; no dynamic dispatch, no method-table hit, which is exactly Koka-style UFCS resolved at compile time), data classes (compiler-generated `equals`/`hashCode`/`toString`/`copy`/`componentN` from the primary constructor), and context parameters (KEEP-367, shipped in Kotlin 2.2, 2025): named contextual declarations injected by type into functions that declare them — implicit resolution again, but with explicit names at declaration, deterministic type-based lookup, and no implicit-receiver inheritance games (context receivers, the earlier KEEP-259 design, were abolished when K2 landed).

```kotlin
data class Point(val x: Int, val y: Int)               // equals/hashCode/toString/copy generated
fun Point.distanceTo(o: Point): Double = hypot(x - o.x, y - o.y)  // extension = UFCS sugar
val d = p1.distanceTo(p2)                              // reads like a method, resolves statically

context(Logger)                                        // KEEP-367: declared capability
fun processOrder(order: Order) { log("processing ${order.id}") }
```

Kotlin is the existence proof that a productive mainstream language can run on interfaces + static UFCS + compiler-generated members + explicit-ish context passing, with zero coherence machinery, zero specialization, and zero metaprogramming above annotation processing. The sugar ceiling the author set (Kotlin's) is achieved by a language with exactly this polymorphism menu — a strong signal that DreamScript needs nothing more exotic than combos A+B for its ergonomics.

### 1.7 C# reified generics vs Java erasure

Java generics are erased: `List<Integer>` is raw `List` at runtime, elements are boxed as `Object`, primitive-specialized generic code cannot exist without boxing every element, and generic type information survives only as compiler-side checks plus reflection metadata. C# generics are reified in the runtime: the CLR JIT creates specialized machine code per value-type instantiation (no boxing, layout-optimal field access, direct calls) and shares one instantiation for all reference types (which are pointer-sized, so one body suffices — the shared instantiation is dictionary passing with a trivial dictionary). The .NET design (Anders Hejlsberg, Don Syme, Microsoft Research, 2004) deliberately put generics in the VM rather than the compiler; Matt Warren's retrospective documents that the alternative — the Java erasure path — was explicitly considered and rejected for performance reasons.

The transferable lesson for DreamScript: "reified vs erased" is not a yes/no but a per-instantiation-shape decision. In a bytecode VM, the equivalent of .NET's split is: value-typed generic parameters get shape-specialized fast paths (unboxed layouts, per-tag ICs) while reference-typed parameters share one body with evidence — exactly what section 6 proposes. Both halves are invisible to the user; neither requires source-level specialization.

### 1.8 C++ templates and concepts: the counter-example

C++ templates are compile-time term rewriting with a type system bolted on. Overload resolution over template signatures plus explicit and partial specializations is the original specialization mess: priority rules between the primary template, full specializations, and partial specializations; SFINAE overload tricks; ambiguity errors with page-long candidate lists. C++20 concepts turned constraints into first-class predicates and made overload resolution among constrained candidates principled — the candidate whose constraints subsume the other's wins (constraint subsumption via logical implication) — but subsumption is still a specialization rule: a priority ordering among overlapping matches, just a decidable one. C++ constexpr moved template metaprogramming into ordinary-looking code (a large win in syntax) while keeping implicit constant-context rules ("can this function run at compile time?" depends on arguments, not annotations), which is the specific pain Zig/Mojo/D eliminate by making the compile-time phase explicit. C++26 will add static reflection (P2996) precisely to end template metaprogramming for the derive use case — twenty-five years after templates shipped, the language is converging on "reflection, not rewriting". C++ is the reference counter-example for DreamScript: maximal power, maximal semantic tax, and a twenty-year-long stabilization tail.

### 1.9 Go interfaces: structural existentials with runtime holes

Go interfaces are structural existentials: a value satisfies an interface if it has the required methods — no declarations, no coherence, no instance search, no trait objects vs generics split before 1.18. Interface values are `(itab, data)` pairs where the itab (interface table) is constructed when the value is boxed; dispatch is a table lookup keyed by the dynamic type. Go generics (1.18, 2022) are erased with GC-shape stenciling: one instantiation per GC shape plus runtime dictionaries — literally dictionary passing where the dictionary carries the operations the shape cannot specialize. Go's zero-magic philosophy is attractive for learning cost, but it gives no static specialization, no overloading, no trait-level polymorphism (`impl<T> Eq for Vec<T>`-style generic instances do not exist — you cannot express "my generic container is equal if its elements are" without per-instantiation boilerplate at every use site), and it ships two equality holes DreamScript must not copy: `==` on interface values panics at runtime when the dynamic type is incomparable, and maps with incomparable value types fail only at the comparison site. Static totality over equality is worth more than the syntax Go saves.

### 1.10 Julia multiple dispatch: dynamic set matching with JIT devirtualization

Julia has no traits or classes: methods belong to generic functions, and dispatch matches the runtime types of all arguments (multiple dispatch over type patterns). Method tables are searched per call; the JIT compiles a specialization per concrete argument-type tuple on first call (on-demand monomorphization — every combination of argument types gets its own compiled body, cached in the method instance table); type inference devirtualizes hot code — when inference proves concrete types at a call site, "type checks and method dispatch ... can be resolved statically", enabling inlining; when it fails (a "type instability", e.g. a variable inferred as `Union{Int, Float64}`), dispatch stays dynamic and performance collapses. The dispatch study (arXiv 1808.03370) and the canonical paper (Bezanson et al. 2018) document the LU case study: 12 GB allocated / 4.15 s naive, 7 MB / 0.75 s after removing temporaries, 0.4 s after inference-friendly rewrites and bounds elision — a 10x spread driven entirely by inference quality, not by dispatch per se.

```julia
area(r::Circle)    = pi * r.radius^2      # clauses are type patterns: set membership tests
area(r::Triangle)  = ...
area(s::Square)    = s.width^2            # disjoint heads: no ambiguity at runtime
```

Julia is the existence proof that dynamic dispatch + JIT specialization reaches C speed, and equally the warning that the speed is contingent on inference success and on argument-type stability. For an interpreter-first language, the interpreter-side analogue is a method cache per call site (section 6.3); the design risk is that dispatch is runtime matching, so "method not found" and ambiguity are runtime errors — violating DreamScript's compile-time-errors preference unless the static checker proves totality over the statically-possible argument sets (which is exactly a disjointness/exhaustiveness check over set-typed clauses, section 2.4).

## 2. Set-theoretic types meet polymorphism

### 2.1 What set-theoretic subtyping buys

If types are sets — union, intersection, and negation are actual lattice operations and subtyping is semantic set inclusion (the semantic-subtyping line: CDuce; Castagna, Nguyen, Xu, Frisch, "Polymorphic Functions with Set-Theoretic Types", POPL 2015; Castagna's 2023 monograph "Programming with Union, Intersection, and Negation Types"; MLstruct's Boolean-algebra subtyping, OOPSLA 2022) — then three things follow that matter for polymorphism. First, subtyping is decidable and compositional: no variance annotations to memorize, `T1 <: T2` is a set computation. Second, a value of type `T1 | T2` is genuinely a value of `T1` or of `T2` (the union is the value's set membership, not a wrapper), so narrowing is checked set arithmetic and case analysis is exhaustive if the cases' intersection is empty and their union covers the scrutinee. Third, function types intersect meaningfully: `(T1 -> U1) & (T2 -> U2)` is the set of functions that are both — which is precisely an overloaded function.

### 2.2 Overloading as intersection of arrows (Castagna's ◁ mechanism)

In the CDuce/Castagna system an overloaded function has type `(Int -> String) & (Bool -> Int)`, and applying it to a value of type `T` computes the result type by intersecting the argument set with each arrow's domain and discarding arrows whose contribution is empty — the "more precise than" (`◁`) relation formalizes which arrows survive. Overloading thus has a principled, decidable resolution rule defined by the type algebra itself, not by ad-hoc priority: at a static call site with a fully known argument type, exactly the matching arrows remain, and if more than one remains, the result type is the union of their result types (sound but vague) — or an error, if the language wants uniqueness. The Elixir type system (Castagna, Duboc, Valim et al., "The Design Principles of the Elixir Type System", arXiv 2111.03354) applies the same machinery to pattern matching: a clause's pattern is a type, a call matches clauses by set membership, and Erlang's first-matching-clause-wins semantics stays at runtime while the static checker warns when a clause is unreachable (fully subsumed by earlier clauses). Elixir thus accepts ordered overlap and surfaces it as diagnostics — the pragmatic middle ground between C++'s priority rules and DreamScript's planned disjointness requirement.

```text
-- DreamScript-shaped sketch of the two regimes:
f : (Int & ~Even -> "odd") & (Even -> "even")   -- disjoint by negation: unique match, no priority
g : (Int -> "int") & (Number -> "num")          -- overlapping: g(3) matches both -> reject at definition
```

For DreamScript the rule "generic instances must not overlap" is directly expressible in the set algebra: at declaration time, check `T1 & T2 = Empty` (emptiness is decidable in semantic subtyping) for every pair of instance/overload signatures in a group, or require the programmer to carve one side with negation as in `f` above. Uniqueness of match then follows from the type algebra; no priority machinery is ever needed; and the error message for an overlap is precise ("these two signatures both accept `Even Int`; refine one with a negation or merge them"). The compiler-side cost is a pairwise emptiness check per instance group, which is quadratic in group size but small in practice and cacheable.

### 2.3 Instance resolution as intersection subtyping (the formal backing)

"Resolution as Intersection Subtyping via Modus Ponens" (Marntirosian, Schrijvers, Thiemann, Steel, OOPSLA 2020) formalizes type-class resolution as subtyping in an intersection-type discipline: an instance `I => C` is an arrow `I -> C`, a set of instances is an intersection of arrows, and resolving a constraint `C t` is a subtyping judgment composed by modus ponens (`t` has the needed instance arrow if the instance environment's intersection type is inhabited at `t`). This single mechanism subsumes function overloading, return-type overloading (resolving `Read s => Read (State s a)`-style constraints from context), and extensible records. The structural fact that matters for DreamScript: implicit resolution and set-theoretic subtyping are not two systems to be glued — resolution can literally be defined as subtyping over an intersection of instance arrows. Uniqueness then requires non-overlapping instance heads, which is precisely the declared constraint, so the formal story fits the design instead of fighting it.

The MLscript/MLstruct line demonstrates the substrate end-to-end: MLstruct (Parreaux, Chau; OOPSLA 2022) gives principal type inference in a Boolean algebra of structural types with first-class unions, intersections, and negation, and its future-work section explicitly names type classes and constructor overloading as compatible extensions (nothing in the algebra blocks them — the point of the Boolean algebra is that there is room to say "not"). Follow-up work on disjoint intersection types and the merge operator (Oliveira and collaborators; culminating in "Liberating Merges via Apartness and Guarded Subtyping", Xu, Huang, Oliveira, OOPSLA 2025) keeps merges coherent by requiring apartness — types provably non-overlapping — while relaxing the strictness of earlier disjoint-intersection systems. The direction of travel in the literature is exactly toward "overloading that cannot be ambiguous", which is DreamScript's stated position with academic cover.

### 2.4 Multimethods: Cecil and Julia as dynamic set matching

Cecil (Chambers) resolved multimethods by dynamic most-specific-match over all arguments, with ambiguity errors at call time (the "no applicable method"/"ambiguous method" runtime errors); CLOS refines this with ordered method combination (primary/around/before/after); Julia refines it into typed method tables with JIT specialization (section 1.10). For a set-typed language, a multimethod is just a function whose clauses are type patterns — the Elixir formulation — and dispatch is runtime set membership testing. That is fully compatible with set types at the type level, but it moves resolution to runtime, and the costs are structural, not incidental: dispatch cost becomes a per-call cost (cachable, see 6.3); static linearity reasoning degrades (the callee's body is only known after the match, so affine flow analysis across the call needs the clause bodies inlined into the checker — effectively monomorphization of checking); and compile-time totality ("every call resolves") requires an exhaustiveness check over the statically-possible argument sets — a disjointness-plus-coverage computation over set types, which is doable but is real machinery. Multiple dispatch is viable as an opt-in dynamic layer (values carrying their method tables, like first-class modules); as the primary polymorphism mechanism it conflicts with static affine typing and with compile-time-errors-over-runtime-errors, and Julia's own evidence shows the JIT, not the dispatch, is what makes it fast.

### 2.5 Verdict

Static generic resolution over disjoint set-typed instances (2.2/2.3) satisfies all stated constraints at once: set-theoretic types, no overlapping matches, no specialization, compile-time errors, formal grounding (resolution = subtyping). Dynamic multimethods can exist as a separately-typed feature if ever wanted; they should not be the default resolution for ordinary generics in a linear language.

## 3. The anti-specialization constraint

### 3.1 Why specialization exists at all

Specialization is only meaningful when two impls match the same value: the language must decide which one runs. Remove overlap — via orphan coherence (Rust), ambiguity-as-error (Scala), or structural disjointness over set types (section 2.2) — and specialization has nothing to do: every generic call has exactly one impl, and "optimizing it" is just compilation. This is the cleanest framing of the author's position: specialization is not a feature you fail to add; it is a complexity you decline by forbidding the ambiguity that requires it. Note the corollary: any feature that reintroduces overlap (wildcard patterns, open world instance tables, first-match semantics) quietly re-creates the need for priority rules, so the disjointness invariant must be checked everywhere instances can appear — declarations, conditional instances, and comptime-generated ones alike.

### 3.2 The cost of coherence: what the orphan rule buys and what it costs

Coherence buys three things. (1) Unique global semantics for trait objects: a `Box<dyn Eq>` knows which `==` it carries, so existential dispatch is well-defined. (2) Safe library evolution: adding impls downstream cannot silently change upstream behavior (no "orphan instance changed someone else's generic function" hazard). (3) A principled basis for evidence construction: evidence can be built once per (trait, type) pair and cached globally. It costs four things. (1) The orphan rule blocks natural extensions — you cannot define `Eq` for a foreign type or a foreign trait for your type when both are foreign — forcing newtype wrappers (`struct MyHashMap<K, V>(HashMap<K, V>)` purely to attach impls). (2) Blanket impls in any crate veto future impls by every other crate (the ecosystem deadlock from 1.1). (3) It pushes libraries into design contortions: sealed traits, extension traits (`IteratorExt` instead of inherent methods), `#[fundamental]` markers. (4) It interacts badly with macros and code generation, which can accidentally define impls that overlap something not yet written. Haskell shows the no-rule world: orphans make behavior import-dependent, Safe Haskell bans them, and the community convention is to quarantine them. So coherence is a real constraint with real friction; the design question is whether the friction can be localized.

### 3.3 The menu of extension-without-ambiguity mechanisms

Mechanism 1 — orphan rule (Rust): global coherence, local friction (newtype wrappers, blanket-impl vetoes). The rule is simple to state and hard-edged; the friction concentrates at library boundaries. DreamScript could adopt it cheaply if instances are declared adjacent to types (like Rust's coherent impl blocks), with the difference that set-typed disjointness checking makes the rule's reason visible ("these two heads intersect") rather than positional ("wrong crate").
Mechanism 2 — scoped instances (Scala 3 givens; OCaml module arguments): resolution is lexical, so two instances coexist in different scopes and the nearest/explicit one wins; ambiguity is a compile error; no global invariant to maintain. Cost: APIs that rely on instances become scope-sensitive (Scala's implicits hell, tamed by givens' strict lexical priority), and "which instance will be found" is a question the reader must trace through imports.
Mechanism 3 — named instances + explicit passing (Effekt capabilities; Zig comptime args; Go interface parameters; Koka handler values): there is no resolution at all. The instance is a value; whoever needs it takes it as a parameter. Effekt's "Effects as Capabilities" (OOPSLA 2020) passes handler capabilities explicitly, makes capability-carrying blocks second-class (they cannot escape their handler's scope), and gets full effect polymorphism with no row-polymorphism inference and no coherence questions. Zig's comptime arguments are the degenerate case: the "instance" is any compile-time value, and generic behavior is explicit code over `type` values.
Mechanism 4 — structural interfaces (Go): no declaration needed; the itab is constructed at boxing time; multiple interface implementations of the same method set coexist trivially because there is only one method set per concrete type. Extension is free; but there is no overloading, no generic instances, and no static evidence for shape-dependent operations.
Mechanism 5 — disjoint overloading under set types (section 2.2): overloads exist, matches are provably unique, resolution is subtyping; the disjointness check is the whole story.

### 3.4 Is explicit capability passing the no-magic answer?

Yes, with one caveat. Explicit passing removes every coherence question by construction (nothing is resolved; everything is supplied), removes the orphan rule's friction (any code can define and pass a capability), and matches the author's decoupling philosophy (an operation's behavior is a visible parameter; Koka UFCS makes `x.f(y)` read naturally while remaining a plain call to `f(x, y)`). The caveat is ergonomics: capabilities threaded through many layers are boilerplate, which is why every language that goes this route adds a mitigation — Effekt and Koka make handlers ambient per scope rather than manual parameters; Scala adds scoped implicit search (mechanism 2) as sugar over explicit values; Zig accepts the boilerplate because comptime args are usually short. The evidence-supported synthesis for DreamScript: make coherent instance declarations the default path (ambiguity impossible by disjointness), allow named scoped instances as a second path (Scala-style, deterministic, ambiguity-as-error), and keep every capability implicitly-passable only when it is provably unique. Nothing is ever resolved by priority order; where a choice exists, it is a compile error naming the two candidates and their sources. This keeps the "no magic" property where it matters (behavior is statically explainable) while avoiding the boilerplate wall that pure mechanism 3 hits.

### 3.5 Eclipse-style problems: what incoherence actually breaks

The concrete failure mode of allowing free instances is not hypothetical: two crates each defining `Eq` for `ThirdPartyKey` produce a program whose map behavior depends on which modules are linked (the Haskell orphan hazard, verified in the sources in section 1.2); a blanket instance `impl<T> Foo<T>` plus a later `impl Foo<&str>` makes generic code silently change behavior when a new impl appears (the Rust future-impls conservatism, verified in #31844); a specialized `Drop` impl can make auto-generated drop glue disagree with the specialized semantics (leaks or double drops — the class of UB the Rust specialization issues document). These are ecosystem-level failures analogous to plugin-version conflicts in Eclipse-style extensible systems: the extension point is global, so independent extensions collide invisibly at link time. The defense is to make collisions either impossible (disjointness) or loud (ambiguity errors), never silent.

## 4. Comptime and metaprogramming spectrum

### 4.1 The spectrum, low machinery to high machinery

Level 0 — none (Go before 1.18, Elm): derive-like needs are hand-written or out-of-band codegen (`go generate` runs a program that writes source files — no compiler involvement, no type safety across the generation boundary, but also zero language complexity). Go added generics only when the pain became unbearable, and added no metaprogramming even then (the team's stated position: reflection and generate, never macros).
Level 1 — runtime reflection (Go `reflect`, Java reflection): works for serialization; costs performance, loses static checking, fails on unexported state, and pushes errors from compile time to runtime — directly against DreamScript's compile-time-errors preference.
Level 2 — compiler built-in synthesis (Swift SE-0185 conformances; Kotlin data classes; Java records; C# records; MoonBit `#derive(...)`; Rust's built-in derive macros): the compiler knows the type structure already, so the compiler emits the members. No user-facing metaprogramming at all; extensibility limited to what the compiler ships, which is the honest trade for zero machinery.
Level 3 — compile-time evaluation of ordinary code with static reflection (Zig `comptime` + `@typeInfo`/`@Type`; D CTFE + `mixin`; Mojo parameters + `comptime`; C++26 static reflection; TypeScript's conditional/mapped/template-literal types as the type-level-only variant): the language's own semantics run at compile time; no second language, no token manipulation; generics fall out as a special case (types are values). Zig's philosophy statement is the cleanest articulation: comptime is just Zig code evaluated at compile time, and macros are unnecessary because code can generate code.
Level 4 — term-rewriting templates (Nim templates, C macros over a typed AST): hygienic AST substitution patterns; powerful for DSL-ish sugar and zero-cost idioms (Nim's stdlib is built on them); still a second semantic layer with its own failure modes (templates compose less predictably than functions; type errors surface after expansion).
Level 5 — syntax-tree macros with phases (Lisp, Racket): the most powerful and most principled of the token-based family; Racket's phase system (compile-time/runtime levels, syntax objects, full hygiene) makes macros compose soundly ("macros that work together", Flatt/Findler 2018), but in a typed language expansion happens before type checking, so typed macro systems re-implement typing inside macros (Typed Racket) or defer errors to expansion (Rust proc macros operating on untyped token trees).
Level 6 — dependent types (Idris, Lean): metaprogramming via the type system itself (types computed by programs); ultimate power, highest learning cost; out of scope for a Kotlin sugar ceiling.

```zig
// Zig: derive-like JSON serialization without any macro machinery (documented comptime pattern)
fn Jsonify(comptime T: type) type {
    return switch (@typeInfo(T)) {
        .@"struct" => |s| struct {
            pub fn toJson(self: T) []const u8 {
                var out: std.ArrayList(u8) = .empty;
                inline for (s.fields) |f| {          // compile-time iteration over fields
                    // append field name, recurse on f.type
                }
                return out.items;
            }
        },
        else => @compileError("Jsonify: not a struct"),
    };
}
```

### 4.2 Hygiene

Unhygienic textual substitution (C preprocessor) is universally recognized as a mistake: `#define max(a,b)` evaluates arguments twice, captures identifiers, and breaks in every edge case. Hygiene comes in grades. Racket and Common Lisp's second generation rename introduced identifiers per expansion site (Racket's syntax objects make hygiene default and escape explicit via `datum->syntax`); Nim templates are hygienic by default with an explicit escape (` {.inject.}`); Rust `macro_rules!` is hygienic at token level (identifiers from the macro body do not collide with the call site), while proc macros are fundamentally unhygienic — they manipulate raw token trees, which is a leading source of proc-macro fragility (derive macros re-parsing arbitrary Rust, attribute DSLs living inside token streams like serde's). Zig/D/Mojo-style compile-time evaluation needs no hygiene concept at all because there is no substitution: generated code is ordinary code, and reflection reads structure rather than rewriting it. This is a strong argument for level 3 in DreamScript: it deletes the entire hygiene question along with the macro machinery.

### 4.3 What minimal metaprogramming covers derive-like needs

The derive use case needs exactly: (a) static access to a type's field/case list (names, types, order); (b) compile-time iteration and branching over that list; (c) compile-time evaluation of ordinary code to assemble the member implementations; (d) optionally, built-in derive attributes for the hot cases (Eq/Hash/Show) so the common cases are one word long and compile fast. Zig demonstrates (a)+(b)+(c) suffice for JSON serialization of arbitrary structs (`@typeInfo` + `inline for` is the whole recipe); MoonBit demonstrates (d) with `#derive(Eq, Hash, Debug, ToJson)` attributes, used pervasively in moonbit-core (verified: every package's types carry derive attributes; the builtin traits ship default method bodies like `not_equal`); Swift demonstrates that even without reflection, built-in synthesis covers the daily cases (SE-0185 + Codable synthesis); Rust demonstrates that everything beyond built-ins degenerates into a proc-macro sub-industry (derive_more, serde with its own attribute language inside token streams) with compile-time costs (proc-macro crate boundary, slower builds) and error-message costs. Recommended shape for DreamScript: built-in derives for Eq/Hash/Show/ser-de at level 2, plus static reflection + comptime evaluation at level 3 for everything else, and no token macros in v1. What you deliberately do not need: arbitrary syntax extension, term rewriting, and phase splitting — none of the surveyed derive-like needs requires them.

## 5. The derive story

### 5.1 Survey: how each language avoids hand-writing equality/hash/display/serialization

Rust: `#[derive(Eq, Hash, Debug, Clone, Serialize)]` — derive proc macros expand to impl blocks. Costs: a separate proc-macro crate boundary (compile time), token-tree parsing instead of typed analysis (error quality), ecosystem fragmentation (serde's attribute DSL lives inside token streams), and coherence coupling (a derived impl must still satisfy the orphan rule). Built-ins (`Debug`, `Eq`, `Hash`, `PartialEq`, `Clone`, `Default`) cover the core cases without proc macros.

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Point { x: i32, y: i32 }
```

Swift: declaration opt-in + compiler synthesis (SE-0185) for `Equatable`/`Hashable` when all stored properties (or all enum cases' associated values) conform; `Codable` synthesis for encoding/decoding; enums without associated values get conformance automatically; conditional conformances compose over generics (`extension Array: Equatable where Element: Equatable`). No user macro involved for the standard cases — the compiler is the derive engine.

```swift
struct Point: Equatable, Hashable, Codable { let x: Int; let y: Int }  // all members synthesized
```

Kotlin: `data class` — `equals`/`hashCode`/`toString`/`copy`/`componentN` generated from the primary constructor; restricted to classes with primary constructors and subject to inheritance restrictions (data classes cannot inherit from other data classes, `copy` only touches constructor parameters — properties declared in the body are silently excluded from `equals`, a documented footgun).

```kotlin
data class Point(val x: Int, val y: Int)   // equals/hashCode/toString/copy + destructuring
```

Java: records (16+) — `equals`/`hashCode`/`toString`/accessors generated from the record header; no `==` operator (reference identity remains for `==`, which avoids the float semantics question but leaves identity comparison as the escape hatch). C# records (9+) — same generation plus `==`/`!=` operators for record types.

```java
record Point(int x, int y) {}              // equals/hashCode/toString generated
```

OCaml: universal polymorphic compare `=` over all data by language semantics — structural, total over finite data, works on every type including closures of functors (generic traversal by the runtime); `==` is physical identity. Known costs: polymorphic compare is reflective and boxed (slow — a documented performance wart that pushes libraries to define custom compares), cyclic values can diverge, and NaN comparisons follow IEEE semantics inside the traversal. Extension for printing/serialization happens via `ppx_deriving` plugins (compile-time AST rewriting) rather than any in-language mechanism. OCaml is the strongest example of "equality is universal by default, no derive needed for it".

MoonBit (repo evidence, 2026): `#derive(Eq, Hash, Debug, ToJson)` attributes on type declarations; traits carry default method bodies (`trait Eq { fn equal(Self, Self) -> Bool; fn not_equal(Self, Self) -> Bool = _ }`); blanket-style impls for tuples are hand-written per arity in `builtin/tuple_eq.mbt` (`impl[T0: Eq, T1: Eq] Eq for (T0, T1)`, then the 3-tuple version, ...) — showing the arity boilerplate trait-mediated equality generates; and `builtin/traits.mbt` documents the floating-point caveat: Double/Float use IEEE equality (NaN unequal to itself, signed zeros equal), algorithms requiring reflexive equality must exclude NaNs, and `Double::total_compare` must be supplied explicitly for sorting/searching keys. This is live evidence of the costs of the IEEE-semantics choice inside a derived-equality world.

```moonbit
#derive(Eq, Hash, Debug)                    // moonbit-core/path/path.mbt
pub(all) struct Path { components : Array[String], absolute : Bool }
```

Roc: no derive at all — `==` is structural for all data; the stdlib expresses comparability as capability requirements (`Dict.is_eq` requires `k.is_eq`, `k.to_hash`, `v.is_eq` per the builtin docs) and splits float equality into `is_eq` (structural) vs `is_float_eq` (IEEE) vs `is_approx_eq` (tolerance); the default decimal type `Dec` makes `0.1 + 0.2 == 0.3` true by construction. Equality and hashing are coupled throughout the stdlib signatures (`is_eq` accompanied by `to_hash`), preventing incoherent overrides by API shape.

Go: no derive; `==` is structural but only for "comparable" types, checked statically; maps/slices/functions are incomparable (slices/functions fail at compile time when compared directly, but interface values containing them panic at runtime — the totality hole); the ecosystem fills the gap with `reflect.DeepEqual` (runtime, slow) or `go generate` (out-of-band codegen). The weakest derive story of the surveyed languages.

Haskell: `deriving (Eq, Ord, Show)` clauses; `DeriveGeneric` + `Generic`-based default methods let user classes get derivation; `StandaloneDeriving` and `GeneralizedNewtypeDeriving` cover special cases. Compact and typed, but class-coupled (derivation only for classes designed for it).

Scala: `case class` — `equals`/`hashCode`/`toString`/`unapply` generated; the `canEqual` pattern guards inheritance asymmetry (a documented wart that Kotlin's final data classes avoid by fiat).

Summary table:

| Language | Equality | Hash | Display | Ser/de | Machinery |
|---|---|---|---|---|---|
| Rust | derived trait | derived trait | derived trait | ecosystem macros | proc macros |
| Swift | synthesized | synthesized | (via Mirror/CustomStringConvertible) | synthesized (Codable) | compiler built-ins |
| Kotlin | data class | data class | data class | none built-in | compiler built-ins |
| Java/C# | record | record | record | none built-in | compiler built-ins |
| OCaml | universal `=` | none (Hash via libs) | `Deriving_Show` | ppx_deriving | ppx plugins |
| MoonBit | derived trait | derived trait | derived trait | derived (ToJson) | compiler built-ins |
| Roc | universal structural | coupled `to_hash` | `to_string`/Inspect | `ToJson` attrs | none |
| Go | structural-if-comparable | none | fmt via reflection | reflection/generate | reflection |
| Haskell | deriving clause | deriving clause | deriving clause | libs (aeson+Generic) | deriving/Generic |

### 5.2 Universal structural equality under set-theoretic immutable data: what breaks

If types are sets and data is immutable with no identity, structural equality is the only coherent notion of `==`: there is no address worth comparing, identity is an implementation accident, and two equal-by-construction values must be equal because the language has no other distinguishable property. Totality analysis over the value domain, break by break:

Break 1 — functions. Extensional equality of closures is undecidable (comparing function behavior requires solving equivalence of arbitrary programs); intensional (pointer) equality is inconsistent with the rest of `==` (two syntactically identical lambdas differ). Elm and Roc reject function comparison statically; Lisp compares identities (incoherent but honest); Go panics on interface comparison of function values. DreamScript should reject `==` on function types statically (functions simply do not implement the equality capability). This costs nothing: in typed code, comparing functions is rare and usually a bug (comparing a passed callback with the expected one). The set-theoretic framing makes this principled: function sets have no decidable equality predicate, so `Eq` is not a capability they carry — the type system already knows this if `==` is capability-bounded.

Break 2 — floats and NaN. IEEE equality (NaN != NaN, +0.0 == -0.0) conflicts with structural reflexivity (NaN == NaN, bits-equal-implies-equal). Three coherent choices. (i) Structural/bits semantics for `==` with a separate named IEEE comparison (`is_float_eq`): Roc's split, verified in the builtin docs (`Num.F32.is_eq` vs `Num.F32.is_float_eq` vs `is_approx_eq`); reflexivity holds, so hash-map keys and set membership behave, and IEEE semantics stays reachable by name. (ii) IEEE semantics for `==` with documented preconditions and a total-ordering helper: MoonBit's choice — `builtin/traits.mbt` states Double/Float use IEEE equality with NaN unequal to itself, derived/composite equality excludes NaNs as a precondition, and `Double::total_compare` must be supplied explicitly to sorting/searching APIs; the repo comment trail (`argparse` TODOs, `physical_equal_nan_test.mbt`) shows the precondition leaks into user documentation everywhere derived equality touches floats. (iii) Ban IEEE floats from default `==` unless wrapped in a newtype — maximal safety, maximal friction. Evidence favors (i): reflexivity of `==` is load-bearing for hash maps, set-theoretic membership, and deduplication, and the Roc/MoonBit pair shows both designs in production with (i) requiring fewer documented preconditions. Hash consistency follows automatically: hash of structural equality is hash of canonical bits (NaN hashes consistently, +0.0 and -0.0 hash identically iff they compare equal — pick bits-equal as the rule and normalize at hashing).

Break 3 — cyclic immutable data. Strict immutable-first languages cannot form cycles without mutation or laziness, so cycles only appear through explicit `ref`/lazy cells; structural equality must either refuse to traverse through mutable/non-copyable cells (compare them by identity — consistent with section 7's treatment of resources) or handle cycles with a seen-set (OCaml traverses naively and diverges on cyclic values — a documented wart). Making reference cells compare by identity sidesteps the cycle problem entirely and matches "mutation is an explicit, locally-visible choice": if you can observe a cycle, you chose the mutable cell, and its identity is the honest semantic.

Break 4 — linear/affine values. `==` must not consume its operands: it takes two borrows and returns a boolean, so affine values stay affine after comparison (`a == b` does not move `a` or `b`). Comparing resources (files, channels, unique handles) must mean identity, not content (two open handles to the same file are equal-as-references, not equal-as-values). Derived equality remains valid on owned affine data because the derived code only reads (borrows) fields. This makes `==` a method on borrowed immutable data — exactly the shape Rust's `PartialEq for &T` and Hylo's `let` convention already use — and it means linear values are never "used up" by comparison, preserving the Perceus-style reuse analysis that wants to know exactly when a value dies.

Break 5 — hash consistency. Structural `==` implies structural `hash`; any language-level equality capability should co-require hashing (Roc couples `is_eq` with `to_hash` in every stdlib signature; MoonBit derives `Hash` alongside `Eq` by convention; Kotlin data classes generate both from the same constructor parameters). Decoupling them invites the classic bug: equal values hashing differently, or hash maps containing duplicates.

### 5.3 Verdict for DreamScript

Universal structural `==` with capability bounds is coherent: define `==` as structural over data, reject on function types, take borrows, use bit-structural float semantics with named IEEE comparison, compare mutable/reference cells by identity, co-require hashing. Derives then reduce to Hash, Show, and serialization — exactly what MoonBit/Swift/Roc evidence says should be compiler-built-in synthesis (`#derive(Hash, Show, ToJson)` style: one attribute, no macro system, no user DSL, good error messages because the compiler is doing it). The tuple-arity boilerplate MoonBit hand-writes (`Eq for (T0, T1)`, `(T0, T1, T2)`, ... per arity, verified in `builtin/tuple_eq.mbt`) disappears entirely when equality is structural at the language level rather than trait-mediated. What remains trait-mediated is comparison ordering (`Ord`) — which cannot be structural (any total order over sets is arbitrary), so `Ord` stays an explicit capability with a `derive(Ord)` built-in (field order = lexicographic, as in Rust/Kotlin/Swift) and a `total_compare` float variant for keys, following the MoonBit precedent.

## 6. Performance: how generics execute in an interpreter

### 6.1 The three execution strategies

Monomorphization: compile a copy per instantiation. Pros: direct calls, inlining, best steady-state speed, no runtime type machinery (C++, Rust, Mojo, Zig, .NET for value types). Cons: compile-time blowup, binary size, i-cache pressure (the classic C++ template bloat), and — decisive for DreamScript — it does not fit an interpreter's execution model: the interpreter would need to materialize bytecode per instantiation lazily (Julia does this at the JIT level, not the interpreter level; Zig and Rust do it ahead of time, paying user-visible compile latency).

Dictionary/evidence passing: one body per generic function; constraints compiled to evidence arguments (GHC dictionaries; Rust `dyn` vtables; Go GC-shape dictionaries; Effekt capabilities). Pros: uniform code size, instantiation-free interpretation, natural fit for a bytecode VM, evidence composes (a dictionary for `(T0, T1)` is built from the dictionaries for `T0` and `T1` — the MoonBit tuple impls are the source-level image of this). Cons: indirect calls unless devirtualized; evidence plumbing visible in cold paths.

Dynamic dispatch + specialization-on-demand (Julia): dispatch at runtime through method tables; the JIT compiles per-type-tuple specializations when hot; inference devirtualizes statically-provable calls. Pros: maximum dynamism, matches set-based method matching. Cons: interpreter-side dispatch cost per call until cached; performance contingent on inference success (the type-instability cliff); "method not found" is a runtime error unless totality is checked statically.

### 6.2 What the evidence says

Julia: specialized calls compile away entirely; unspecialized dispatch costs a method-table search; the LU case study shows a 10x+ spread from inference quality alone (4.15 s naive vs 0.4 s tuned, arXiv 1808.03370) — the dispatch mechanism is not the bottleneck, the inference failure is. .NET: value-type instantiation eliminates boxing with layout-optimal specialized code; reference-type instantiation shares one body — i.e., .NET deliberately monomorphizes exactly where it pays (small, hot, unboxed) and shares (dictionary-passes) everywhere else. Java: megamorphic `invokeinterface` dispatch is a known cost center; the JIT recovers it via inline caches and class-hierarchy analysis when the receiver set is small; escape analysis and inlining are blocked when the callee is unknown statically — the cost of erased generics without evidence passing. GHC: dictionary passing with `INLINE`/`SPECIALIZE` as manual erasure — no automatic cross-module specialization, making dictionary costs a user-visible chore in performance-sensitive code. The common lesson across all four systems: the winning configuration is "one generic body + evidence + call-site caching + targeted lazy specialization in the optimizing tier", with monomorphization reserved for the cases where it provably pays (unboxed primitives).

### 6.3 Concrete bytecode design for DreamScript

Represent every generic function once in bytecode, with its constraint list attached. At a generic call site, emit `CALL_GENERIC fn, evidence_words`: each evidence word is either a statically-resolved instance reference (when the receiver's type is known — a vtable pointer or a cached instance closure) or a runtime-resolved slot (when the argument's set type is only known dynamically). The interpreter maintains an inline cache per call site: a mapping from the observed runtime type vector to the resolved callee and method offset, patched in place after the first execution — Self/V8-style polymorphic inline caches (Ungar/Chambers 1991), which remain the standard technique in every fast dynamic VM. Operations that need value-shape knowledge (unboxing a set-typed value, accessing a variant) go through a small switch on the runtime tag, which the IC turns into a direct jump for the common case. In the JIT tier, evidence words that the type prover resolved statically inline to direct calls; hot evidence-passing chains get lazily materialized specializations keyed by the concrete type vector (Julia-style), cached in the function's instantiation table and dropped under memory pressure.

Sketch of the bytecode shapes (illustrative, not prescriptive):

```text
-- generic function, stored once
fn map[T: Eq, U](f: fn(T) -> U, xs: List[T]) -> List[U]
  ; constraint words: [Eq_T_slot]  -- resolved per call site

-- call site with statically known T (Int):
  CALL_GENERIC map, [static_ref: eq_instance_int]
  ; interpreter: no lookup at all — evidence is a constant operand

-- call site with dynamic T (set-typed value):
  CALL_GENERIC map, [dynamic: tag_of(x)]
  ; interpreter: IC keyed by tag; first call resolves, later calls jump directly
  IC_PATCH map@42: { tag_list -> callee, expected_shape }
```

Set-theoretic instance declarations make the evidence table well-founded: since instance heads are pairwise disjoint (section 2.2), a runtime value's membership test identifies at most one instance, so ICs need no priority tie-breaking — a cache can never disagree with the semantics, and cache invalidation is monotone (a type can only narrow, and a narrower type hits the same unique instance).

### 6.4 Is call-site caching "specialization" that violates the stance?

No, and the distinction is worth stating precisely. The author's rejection of C++-style specialization is a rejection of source-level, observable dispatch: the programmer writes overlapping impls and the language resolves them by priority rules the programmer must learn and reason about — behavior depends on which specializations are declared where. Inline caching and JIT-tier lazy specialization are compiler-internal: they never change which code is semantically selected (the unique disjoint instance), only how fast it is reached, and they are unobservable (a well-designed IC falls back to the full lookup, so correctness does not depend on cache hits). The same distinction separates .NET's value-type stenciling (invisible) from C++ partial specialization (visible). DreamScript can therefore state the invariant cleanly: "dispatch is determined by types and the set algebra, never by declaration order or priority; the runtime may cache and specialize any dispatch that is already unique".

## 7. Interplay with linear/affine types

### 7.1 Generics over noncopyable values

Rust: generic code works over non-`Copy` types by default (affine), with `Copy`/`Clone` bounds opting into copying; `Sized`/`?Sized` distinguishes universals from existentials at the type level; the standard library is the main user of `min_specialization` (specialized `Clone` for `Vec<T>` where `T: Copy` becomes memcpy) — evidence that even Rust's tiny sound specialization subset exists to make generic containers faster, not for user expressiveness; the same effect is available to a bytecode VM as an IC fast path (section 6.3). Hylo: all types are non-copyable and immovable by default; `Copyable`/`Movable`/`Deinitializable` are opt-in conformances; generic code written without a copy bound works for both worlds; the `~Copyable` bound ("negated constraint") declares a parameter may be noncopyable; parameter conventions `let`/`set`/`inout`/`sink` (borrow/set/consume) replace pointers-as-types — references are second-class, not types (Hylo discussion #754), so a function's signature says how it uses its arguments without introducing address types into the value language. This is the cleanest known design for "generics over linear values": the passing convention, not the type, carries the linearity of the parameter.

```hylo
// Hylo-shaped: one generic body serves copyable and noncopyable T alike
square[T: Movable & ~Copyable](x: T) -> T   // no copy bound: works for owned linear T
```

Koka: Perceus reference counting with reuse analysis (Reinking, Xie, de Moura, Leijen, ICFP 2021) lets functional-generic code avoid reallocation — `reuse` consumes an owned value's buffer when its reference count is about to drop to zero, turning `Cons(h, tail)` into an in-place overwrite of the consumed list node when possible. Repo evidence shows the stdlib bends around it: `lib/std/core/types.kk` documents a `pad` reference type "sometimes used for fbip programming to match up the size of constructors for reuse" — allocation-shape compatibility between the consumed and the fresh constructor is a real stdlib design concern when reuse analysis is in play. Implication for DreamScript: generic container operations (map/filter) over affine values need either borrows (coerce affine to shared for reading, as Rust's `&T` does for owned values) or an explicit consume-and-return convention (`fn cons[T](h: T, tail: List[T]) -> List[T]` reusing `tail`'s node); a `~Copyable`-style agnostic bound keeps one code path for both, and the borrow-vs-consume choice lives in parameter conventions, not in a duplicated generic API.

### 7.2 Variance under linearity

For immutable data, set-theoretic subtyping makes variance fall out of set inclusion: if `Dog <: Animal` then `List<Dog> <: List<Animal>` as sets of values, so immutable containers are covariant with no annotations (the way Scala's `val` fields and Kotlin's read-only interfaces are). Linearity does not break this: coercing an affine value's type is not duplication — `Box<Dog>` (one affine dog) coerces to `Box<Animal>` (still one value) — provided the coercion cannot copy or drop the contents, which immutable data guarantees by construction. Invariance is needed exactly where interior mutation or unique-borrow escape exists (Rust's `Cell<T>`/`&mut T` invariance; Hylo's mutable-value semantics): the set of states of a mutable container is not monotone in its element type (a `Cell<Animal>` can store a `Cat`, making it not a `Cell<Dog>` in any sense), which set-based subtyping expresses as "read roles are covariant, write roles are contravariant; a field with both roles is invariant". Rule of thumb for the design: covariant by default for immutable data; invariant when a field is uniquely mutable; no variance annotations to learn unless the user writes a mutable container — and the set algebra gives the checker's reasoning for free (subtyping of containers is just subtyping of their value sets, which is what semantic subtyping already computes).

### 7.3 Phantom types for resources

Phantom/gift-wrapped parameters encode resource states at the type level: `File[Open]` -> `close : File[Open] -> File[Closed]` with affine enforcement (a closed file cannot be closed twice because the value was consumed; a forgotten file cannot be garbage because the type system tracks it). Under set-theoretic types, state transitions are set transformations: `File = File[Open] | File[Closed]`, and `read` narrows `File[Open]` to itself while `close` maps `File[Open]` into `File[Closed]` — unions compose with partial-usage APIs naturally. Trait-like bounds on phantoms (`S: SocketState`) let generic code constrain which resource protocols it accepts. This pattern needs only ordinary generics (no HKT, no GATs) — the type parameter is a type, not a family — and it composes with the linear borrow conventions from 7.1 (a `File[Open]` borrowed for `read` stays alive; consumed by `close` it does not).

```text
-- DreamScript-shaped sketch
type File[S: FileState] = ...
fn open(p: Path) -> File[Open]          -- linear: exactly one
fn read(f: &File[Open], n: Int) -> Bytes  -- borrow: file survives
fn close(f: File[Open]) -> File[Closed]   -- consume: can't close twice, can't read after
-- f : File[Open | Closed]  is allowed: read needs &File[Open] (narrow first), close needs File[Open]
```

## 8. Existentials, universals, associated types, HKT

### 8.1 Existentials vs universals

Universals (generic functions) are written by the callee and resolved at compile time; existentials (dynamic values carrying their evidence) are written at the producer site and dispatched at runtime. Rust spells them `impl Trait` vs `dyn Trait`; Swift `some P` vs `any P` (with SE-0352 implicitly opening existentials into generic calls); OCaml first-class modules `(module S)`; Go everything is existential (interfaces). The lesson across languages: keep both, make the syntax two words, and default to universals in a performance-focused linear language, since existentials box and break affine ownership reasoning (a `dyn` value erases its concrete affine type; Rust requires `T: ?Sized` gymnastics precisely because existentials and ownership interact badly — an unsized affine value cannot be moved by value through a generic `T: ?Sized`). Existentials remain essential for heterogeneous collections and plugin boundaries — first-class modules (C) are the explicit, evidence-visible spelling of the same thing.

Associated types exist to keep both inference-friendly: `Iterator` with `type Item` avoids the "iterator of what?" ambiguity a parameterized `Iterable<T>` cannot express when the element type is determined by the self type (and is the only honest spelling for lending iteration, where `Item<'a>` borrows from `self` — the niche that motivated Rust GATs, per the GAT initiative usage survey). DreamScript should ship parameters (with set-theoretic bounds) and associated types, but skip parameterized associated types (GATs) until a concrete need appears — the Hylo borrowing-convention design covers most lending-iterator-shaped needs without them.

### 8.2 When is HKT actually needed?

HKT — abstracting over type constructors, `Functor F => map : (a -> b) -> F a -> F b` — is needed when you write code generic over the shape of a container, not its contents. Survey of need: Kotlin has never had HKT (KEEP-87 stalled since 2017; Arrow deprecated the simulated encoding) and is a thriving mainstream language; Swift lacked HKT for a decade and filled the gap with opaque types + primary associated types rather than adding HKT (and the Swift team's direction — noncopyable types with `~Copyable` bounds, context-sensitive conformances — continues without HKT); Rust lacks trait-level HKT and got by with GATs (partial HKT) that its own community calls hard to learn; Haskell is the language where HKT is load-bearing — because its abstraction style is free-standing class methods (`fmap`, `>>=`) over abstract shapes. DreamScript's declared style points the other way: Option/Result are concrete monads with methods (`option.map`, `result.and_then` — no abstract functor needed); effects, if adopted, use rows/capabilities (file 02), not monad-transformer towers; collections expose methods, not free-standing generic combinators. Under those choices the HKT drivers vanish. Recommendation: v1 ships parameters + associated types + set-based variance; HKT stays out until a concrete repeated pattern demands it — and the evidence says that pattern has not demanded it in Kotlin or Swift for a decade.

```text
-- what DreamScript needs vs what HKT would add:
option.map(f)      -- concrete monad method: no HKT, full inference
result.and_then(f) -- ditto
list.map(f)        -- ditto
-- an abstract `map : (F, (a->b)) -> F b` over all three at once is the HKT ask;
-- it buys abstraction over containers, costs inference complexity (projections
-- through constructor variables), and has no consumer in the declared stdlib style.
```

## 9. Viable combos for DreamScript

### Combo A — Coherent set-theoretic traits + built-in derives + evidence-passing VM (recommended baseline)

Traits with associated types; instance heads pairwise disjoint (checked by set emptiness at declaration, negation types available to carve); no orphan rule needed in the Rust sense because disjointness plus instance registration makes ambiguity impossible; built-in `#derive(Eq, Hash, Show, Serde)` compiler synthesis; universal structural `==` with the section 5 carve-outs; bytecode = one generic body + evidence words + inline caches; the JIT does lazy specialization keyed by type vector.

```text
-- DreamScript-shaped sketch
trait Ord[T] { fn compare(&T, &T) -> Int }

impl Ord[Point] { fn compare(a, b) { ... } }        -- unique: heads are disjoint by construction
impl[T: Ord] Ord[List[T]] { ... }                    -- conditional instance: an intersection arrow

fn max[T: Ord](a: &T, b: &T) -> &T { if a.compare(b) < 0 then b else a }
max(p1, p2)          -- resolves to impl Ord[Point]; evidence is a static ref
max(xs, ys)          -- evidence composes from Ord[T]'s evidence word
```

Learning cost: low-medium (traits + associated types + the disjointness rule; no specialization, no coherence lawyer, no macro system). Perf: near-JIT-tier behavior in the interpreter via ICs; unboxed primitives via shape-specialized evidence; no boxing for set-tagged values on the common paths. Sugar cost: near zero (derive attributes; no data-class duplication of the derive story; UFCS gives method-call ergonomics on plain functions). Set-theoretic fit: perfect — instances are set patterns, resolution is subtyping, disjointness is checkable. Linear fit: good — bounds like `T: Copyable` opt-in or `~Copyable`-agnostic generics; `==` takes borrows; parameter conventions carry linearity (Hylo model). Anti-specialization fit: perfect by construction. Stdlib implications: collections take `Eq`/`Hash`/`Ord` bounds (evidence words); no hand-written tuple-arity impls (structural equality removes them); maps require `Hash` co-derivation with `Eq`; iteration via associated types; resource types via phantom parameters (7.3). Risks: the disjointness checker must be complete and its errors must be instructive ("these two heads intersect at `Even Int`; refine with a negation or merge them"); conditional instances need a termination argument for recursive resolution (the Modus Ponens calculus provides it).

### Combo B — Explicit capabilities, no implicit resolution (Effekt/Zig direction)

No instance search at all: `Eq` implementations are values (`eqInt`, `eqPair(eqInt, eqInt)` combinators), generic functions take them as parameters, and UFCS makes `x == y` sugar for a passed capability when unique. Zig-style comptime reflection covers derive-like needs.

```text
-- DreamScript-shaped sketch
type Eq[T] = { fn equal(&T, &T) -> Bool }
fn dict_get[K, V](d: &Dict[K, V], k: &K, eq: Eq[K]) -> Option[V]

dict_get(m, key, eqPair(eqInt, eqStr))   -- fully explicit: no resolution ever
dict_get(m, key, _)                      -- sugar: compiler inserts the unique Eq[K] if provable
```

Learning cost: lowest conceptually (no magic) but highest boilerplate; Scala's history says implicit search exists because manual threading hurts, and Effekt mitigates by making handlers ambient per scope rather than manual parameters. Perf: identical to A (evidence is explicit; ICs still apply; static evidence is a constant operand). Sugar cost: high unless mitigated by unique-instance insertion sugar (the `_` above — which is mechanism 2 scoped resolution lite, so B alone is not fully "no magic" in practice). Set-theoretic fit: good (no interaction). Linear fit: excellent (capabilities are ordinary values; linearity composes; explicit evidence makes affine flow obvious). Anti-specialization fit: perfect. Stdlib implications: every polymorphic API grows a capability parameter or a comptime reflection path; derive-free equality still requires universal structural `==` (same as A); handler-style ambient scopes (Effekt) are the mitigation if capabilities proliferate.

### Combo C — ML modules (1ML-style unified core/modules)

Everything is a module; generics are functors; first-class modules give existentials; `Map.Make(Ord)`-style explicit instantiation; abstract types by construction.

```text
-- DreamScript-shaped sketch
module type Ordered = { type T; fn compare(&T, &T) -> Int }
module Map(O: Ordered) = { ... }        -- generative: two instantiations, two distinct types
module StringMap = Map(StringOrd)       -- explicit
```

Learning cost: high (signatures, generative vs applicative functors; the syntax budget strains against the Kotlin sugar ceiling). Perf: excellent, fully static, no runtime machinery needed. Sugar cost: heavy — functors-as-generics is verbose for collection code (the reason OCaml grew ppx and first-class-module shims). Set-theoretic fit: orthogonal, fine. Linear fit: fine (modules are values; generativity gives fresh abstract types, useful for capabilities). Anti-specialization fit: perfect (explicit everywhere). Stdlib implications: collections parameterized by comparator modules — the Rust `BTreeMap<K: Ord>` one-liner becomes two-step instantiation; the derive story is weakest (needs a universal compare like OCaml's or external codegen). Verdict: a principled but sugar-expensive whole-language direction; its useful fragments (first-class modules as existentials; generative abstract types for capabilities) can be borrowed into A.

### Combo D — Zig-style comptime generics (types are values)

Generics are compile-time functions returning types; reflection via type-info; no dispatch at all (full monomorphization).

```text
-- DreamScript-shaped sketch (Zig flavor)
fn List(comptime T: type) type {
    return struct { items: [64]T, len: Int, ... }
}
var xs: List(Point) = ...
```

Learning cost: medium (one concept — "run code at compile time" — but it is a second execution phase users must learn to debug; Zig's error messages at comptime are its known weak spot). Perf: best possible steady state (no evidence, no dispatch, direct calls) — but in an interpreter it means lazy bytecode materialization per instantiation (compile time visible at runtime, Julia-style) or ahead-of-time instantiation of hot paths; interpreter startup cost risk. Sugar cost: low syntax, high semantic overhead (comptime failures are compile-time-eval failures; the boundary between compile-time and runtime values is a second type system). Set-theoretic fit: poor — comptime bypasses the static type system (types constructed at comptime escape the set algebra; the non-overlap invariant is not enforceable over comptime-generated instances), and compile-time monomorphization conflicts with the immutable-by-default bytecode story (each instantiation is a new body — a semantic form of specialization). Linear fit: weak (comptime-generated types need manual linearity discipline; the linear checker must run on generated code, which Zig does not do). Anti-specialization fit: mixed — no source-level overload priority, but semantically it is template instantiation, and its opacity to the type algebra is the anti-thesis of set-theoretic explainability. Stdlib implications: containers by comptime construction; derives met by reflection (elegant); but every API becomes partly dynamically typed at comptime boundaries. Verdict: adopt the reflection half (static type reflection + comptime evaluation for derives, level 3 of section 4), not the generics half.

### Combo E — Julia-style dynamic multiple dispatch + JIT devirtualization

No traits; methods attach to generic functions and match on all argument sets; resolution dynamic with ICs; the JIT specializes hot type tuples.

```text
-- DreamScript-shaped sketch
fn area(r: Circle)    = pi * r.radius^2     -- clauses are set patterns
fn area(t: Triangle)  = ...
fn area(s: Square)    = s.width^2           -- disjoint heads checked statically
```

Learning cost: lowest for scripting use (no type-class concepts at all). Perf: excellent after JIT for inferred code; interpreter dispatch costs before IC warm-up; the type-instability cliff is real (Julia evidence: 10x spreads from inference quality). Sugar cost: low syntax, but runtime dispatch means runtime "method not found"/ambiguity errors — violating compile-time-errors-over-runtime-errors unless the static checker proves totality over the statically-possible argument sets (disjointness + coverage over set types, section 2.4). Set-theoretic fit: good at the type level (clauses are set patterns; disjointness checkable statically for exhaustiveness). Linear fit: poor — dynamic dispatch hides the callee, blocking affine flow analysis and guaranteed TCO across dispatch; every call becomes a potential effect site. Anti-specialization fit: good at the source level (disjoint clauses), but the design leans on runtime matching — a form of deferred resolution. Stdlib implications: rich numeric/array-style APIs; but collections over affine values need dispatch-free internals, and the derive story is unchanged (structural equality works either way). Verdict: attractive as a numeric/scripting tier; wrong default for a statically-typed linear language; a per-module opt-in "dynamic mode" later, not v1.

### Comparison table

| Combo | Learning cost | Perf (interp tier) | Perf (JIT tier) | Sugar cost | Set-type fit | Linear fit | Anti-spec fit | Stdlib derive |
|---|---|---|---|---|---|---|---|---|
| A coherent traits + derives | low-med | good (ICs) | very good | low | excellent | good | perfect | built-in synthesis |
| B explicit capabilities | low (boilerplate) | good | very good | medium-high | good | excellent | perfect | reflection-based |
| C ML modules | high | excellent | excellent | high | fine | fine | perfect | weak |
| D comptime generics | medium | startup risk | best | low syntax, high semantics | poor | weak | mixed | reflection (elegant) |
| E dynamic multimethods | low | dispatch + ICs | very good (instability cliff) | low | good | poor | good | structural == |

### Reading of the evidence

Combo A is the only one that simultaneously satisfies all four hard constraints (set-theoretic, no overlap, linear-friendly, bytecode-native) while keeping the sugar budget for features other than polymorphism; it is also the combo whose every component has independent production evidence (MoonBit derives, Swift synthesis, Rust evidence-passing `dyn`, Julia IC-style caching, Roc structural equality, Scala scoped-resolution-as-escape-hatch). B is A's natural extension hatch and shares its runtime representation; the Effekt capability model is the proof that the explicit path scales. D contributes the metaprogramming substrate (reflection + comptime evaluation, no macros); C contributes existentials via first-class modules if abstract types are wanted; E stays out of v1. None of the combos requires a token-macro system; all five have coherent derive stories once universal structural equality is adopted (A/B/E directly; C via a universal compare; D via reflection), which the derive survey (section 5) strongly favors. The open design questions this file leaves for the decision catalog: (1) the exact disjointness-checking algorithm and its error messages; (2) whether conditional instances allow recursion and with what termination rule; (3) whether scoped named instances (B-lite) are in v1 or deferred; (4) the float-equality choice for `==` (Roc's split vs MoonBit's IEEE-with-preconditions); (5) whether comptime reflection ships in v1 or the built-in derives alone suffice until demand appears.

## 10. References

Rust: RFC 1210 (impl specialization); rust-lang/rust#31844 (tracking issue: "unsound ... undefined behavior without unsafe code"; future-impls conservatism; lifetime dispatch hole); N. Matsakis, "Maximally minimal specialization: always applicable impls" (2018 — lifetime erasure, always-applicable conditions); PR #68970 (min_specialization: restrictions on constants/assoc types, parameter use, predicate implication); issue #149257 (2025: min_specialization unsound via trivially-false lifetime bound); GAT initiative design docs (usage survey, "too hard to learn" concern); 2024 State of Rust Survey (GAT usage below async traits).

Haskell: GHC Users Guide (instance pragmas, Safe Haskell orphan restrictions); HaskellWiki orphan instance page (import-together convention); osa1, "Coherence and orphan instance rules" (2026); Freckle, "A Home for Orphan Instances" (2018); michaelpj, "Your orphan instances are probably fine" (2020, counterpoint); Serokell, "Learn From Errors: Overlapping Instances".

Set-theoretic: Castagna, Nguyen, Xu, Frisch, "Polymorphic Functions with Set-Theoretic Types" (POPL 2015 — overloading as arrow intersection, the ◁ relation); Castagna, "Programming with Union, Intersection, and Negation Types" (2023 monograph); Castagna, Duboc, Valim, Sussman, Lespiau, Ferreira-Corso, "The Design Principles of the Elixir Type System" (arXiv 2111.03354 — clause matching by set membership); Parreaux, Chau, "MLstruct: Principal Type Inference in a Boolean Algebra of Structural Types" (OOPSLA 2022 — type classes as compatible extension); Marntirosian, Schrijvers, Thiemann, Steel, "Resolution as Intersection Subtyping via Modus Ponens" (OOPSLA 2020); Xu, Huang, Oliveira, "Liberating Merges via Apartness and Guarded Subtyping" (OOPSLA 2025); Parreaux, "Coherent Mergers" research notes.

Effects/capabilities: Brachthäuser, Schuster, Ostermann, "Effects as Capabilities: Effect Handlers and Lightweight Effect Polymorphism" (OOPSLA 2020); "Effekt: capability-passing style for type- and effect-safe, extensible effect handlers in Scala" (JFP 2020); Lee et al., "Effects, Capabilities, and Boxes" (second-class blocks).

Performance: Bezanson, Karpinski, Shah, Edelman, "Julia: Dynamism and Performance Reconciled by Design" (2018); "Fast Flexible Function Dispatch in Julia" (arXiv 1808.03370 — inference-driven devirtualization, LU case study numbers); Matt Warren, "How generics were added to .NET" (2018 — CLR per-value-type JIT instantiation); Ungar, Chambers, Patterson, "Optimizing Dynamically-Typed Object-Oriented Languages With Polymorphic Inline Caches" (ECOOP 1991).

Linear/ownership: Hylo specification (parameter conventions `let/set/inout/sink`; `~Copyable`; references-not-types, discussion #754); Reinking, Xie, de Moura, Leijen, "Perceus: Garbage Free Reference Counting with Reuse" (ICFP 2021); local repo evidence: `koka/lib/std/core/types.kk` (`pad` type for constructor-size matching under reuse).

Derives: Swift SE-0185 (synthesis), SE-0346 (lightweight same-type syntax), SE-0358 (primary associated types), SE-0352 (implicitly opened existentials); MoonBit `builtin/traits.mbt` (IEEE-equality documentation, default methods) and `builtin/tuple_eq.mbt` (tuple-arity impls) in moonbit-core; Roc builtin docs (`is_eq` vs `is_float_eq` vs `is_approx_eq`; `Dec` exactness); Kotlin data classes and KEEP-367 context parameters (shipped in 2.2, 2025), KEEP-87 (HKT, not accepted; Arrow deprecation).

Metaprogramming: Zig language reference and zig.guide comptime pages (0.15 — comptime parameters, `@typeInfo`, distinct instantiation per comptime argument combination); Mojo parameters manual (mojolang.org — `[]` parameters, infer-only parameters, `comptime` control flow); C++26 static reflection (P2996); Flatt, Findler, "Macros that Work Together" (2018, Racket phases); Nim manual (templates/macros, hygiene).

Local source evidence (read-only, in repo root): `C:/programs/me/DreamScript/moonbit-core/` (derive syntax across packages, trait defaults, NaN documentation, tuple impls); `C:/programs/me/DreamScript/koka/` (Perceus reuse infrastructure, handler-based effects).

