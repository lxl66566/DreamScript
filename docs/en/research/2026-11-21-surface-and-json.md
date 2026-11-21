# Research memo: `is`, collection member surface, JSON architecture

Scope: three author complaints about the current surface. Sources: `docs/en/design-decisions.md` (authoritative rulings), `docs/en/tutorial.md` (v1 surface), `docs/en/status.md` and `docs/en/todo.md` (implementation boundary). Nothing in the repo was modified.

## Q1: The `is` keyword

### What `is` does in DreamScript today

`is` is a pure type test and the entry point of flow refinement (B2): its right side is a type expression — unions and intersections included, so `x is a | b` and `x is a & b` are legal — and a successful test narrows the binding's type monotonically until the next assignment. It is never identity comparison (the tutorial says so explicitly, distancing it from Python's `is`). It is one of exactly two word-form operators (`in` the other, F5/F8), and it holds a line in the sugar budget at S2 (`is` narrowing, F7). Its two idiomatic uses in the corpus are the guard-style early exit (`if x is none { return missing }`, the sanctioned extraction spelling after `or` was retired) and expression-position tests composed with `&&` (`if pv is doc_num && pv.value >= 1`). Match arms narrow too, but arm narrowing and `is` narrowing are the same refinement mechanism (B2), so `is` is the expression-level face of a mechanism the language already needs for match.

### Survey: how other languages spell type tests and narrowing

- Kotlin: `is` / `!is` with smart casts, `as`/`as?` for casts. The direct model for DreamScript's spelling; smart cast is exactly flow refinement.
- Swift: `is` returns Bool and does not narrow; narrowing goes through `as?` (conditional cast to optional) or `if case let` patterns. Two mechanisms: a test keyword plus a cast operator.
- TypeScript: `instanceof`, `typeof`, `in`, and user-defined type predicates (`x is T` as a return type — note TS uses `is` only in predicate signatures, not as an expression operator); discriminated-union narrowing rides literal tag checks (`if (x.kind === "a")`).
- Rust: no test keyword. `matches!(x, pat)` (a macro), `if let`, `let else`, `match`. Trait-object downcasts go through a method literally named `Any::is::<T>()` plus `downcast_ref`. Narrowing is pattern-only.
- Python: `isinstance(x, T)` — an ordinary function, no keyword; class patterns in `match` since 3.10. Python's `is` is identity comparison, which DreamScript explicitly rejects as a meaning.
- Gleam: no test form at all; `case` (match) is the only narrowing channel. Clean but forces a branch structure for every test.
- Roc: no standalone test expression; `when x is ...` uses `is` as the match introducer, then patterns do all the work.
- MoonBit: `expr is Pattern` — an `is` test expression where the right side is a pattern, not a type; narrows inside the guarded branch.
- Zig: nothing at runtime. Tagged unions dispatch through `switch` with payload capture; type questions are comptime-only (`@typeInfo`).

The survey splits languages into three camps: keyword test plus flow narrowing (Kotlin, MoonBit, DreamScript), pattern-only narrowing (Rust, Gleam, Roc, Zig), and ordinary-function tests without narrowing (Python). Only DreamScript lets the right side be a full set-algebra type expression.

### Is `is` carrying too little weight for a reserved word?

No. Measured by what removing it would cost: every narrowing outside a match would need an arm-shaped construct, which means a scrutinee, braces, and a catchall for a pure test; expression-position tests composed with `&&`/`||` (the port-range example in the tutorial) would be unexpressible without nesting matches; and the `if x is none { return ... }` early-exit idiom — the sanctioned replacement for the retired `or` operator (F8) — would lose its spelling. The word also carries set-theoretic weight no peer gives it: `x is a - b` style right sides are the surface of B1's algebra, and there is no cheaper spelling of "is this value in this type set." Against that, the reservation cost is one word in a language that has exactly two (`in`, `is`) and a sugar budget with 40-42 units of headroom.

### Alternatives

- Keep `is` (status quo). Cost: one reserved word. Benefit: expression-position tests, the early-exit idiom, set-algebra right sides.
- Match-only narrowing (Rust/Gleam/Roc camp). Retire `is`; all refinement through arms. Aligns with "one mechanism," but B2 already treats arms and `is` as one mechanism — this removes the expression face and keeps the engine. It also reintroduces arm-order and catchall noise for simple tests and makes the retired `or`'s replacement idioms heavier.
- `x as? T` (Swift camp). A cast producing `T | none`, extracted by the existing family. This is a second narrowing channel with a new operator, overlapping `T?`, and it suggests runtime casting semantics DreamScript does not have (values already carry their tags; a test is a tag read, not a conversion). It would spend sugar budget to duplicate what `is` plus refinement already does.
- Pattern on the right of `is` (MoonBit). `x is some_case(v)` — merges binding and testing. This is real sugar beyond the test and collides with F4's arm machinery; deferred-backlog material at best, not a replacement.

### Recommendation

Keep `is`. It is the expression-level face of B2 refinement, it carries the set algebra onto values, and the language's word-form budget is already minimal at two words. If the author wants to reduce its surface, the honest lever is restricting the right side from full type expressions to nominal atoms and their unions (dropping `&`/`-`), which would simplify the checker without touching the idiom set — but even that is a simplification of convenience, not a fix for a real problem. Do not add `as?`; do not go pattern-only.

## Q2: `list_*` / `map_*` global natives vs member spellings

### (a) The end-state syntax is already ruled

H5.5 collection face (2026-11-17, binding): inherent members plus UFCS spellings — `xs.len()`, `m.get(k)`, `list[t]()` — never `list_*`/`map_*` prefixes. So the end state is `club.push(u)`, `club.get(0)`, `m.get(k)`, `m.set(k, v)`, with `list_get`/`map_get`/`list_push` retired from the user surface. The complaint is not a design question; the design is settled and the complaint is that the implementation has not arrived there. `xs.len()` and `m.len()` already answer through the free generic's List and Map instances, and `m.keys()` already answers through an ambient inherent member — the shape of the end state exists and runs today.

### (b) What blocks it today, and interim routes

Todo item 69 states the blocker precisely: the element-carrying members (`xs.get(i)`, `m.get(k)`, `xs.push(x)`, `m.set(k, v)`) check clean as declared generic bodies but refuse at lowering, because a member body over the impl's own type variable carries no static word kind — the bytecode frame cannot be laid out without knowing whether an element occupies one word or a multi-word span. Lifting the refusal needs per-instantiation member bodies, which is todo item 6, and item 6 is scoped to the JIT specialization tier (per-instantiation receiver frames). Taken literally, that couples a surface cleanup to M8-scale tier work.

Item 6 is the only path for ds-source generic member bodies, but it is not the only path to the ruled surface. The key observation: the refusal is about compiling a ds bytecode body over an unknown element word kind, and the `list_*`/`map_*` operations are natives today — natives whose runtime representations already carry the information a ds body would lack. Element-stride spans for `list[record]` and `list[tuple]` exist and run (status.md: "element-stride spans for list[record]/list[tuple]", "the stride-aware cons/tail/collect/list_concat"), so the runtime list knows its element stride; a native does not need a static word kind. The interim route is therefore: declare the ruled inherent member signatures on `list[t]` and `map[k, v]` in the ambient, and back each one with the native seam that already exists — exactly the precedent `m.keys()` set (an ambient inherent member) and that `len` set (a member spelling over a native-backed generic instance). No ds generic body is compiled, so no per-instantiation frame is needed, and item 6 stays where it belongs (derives over generic declarations, item 5/52 residuals), unblocking on the JIT tier's own schedule. When item 6 lands, the member bodies can migrate to ds source in @std without any user-visible change, because the spelling was already the end state.

Two rejected interim routes for the record: renaming the natives to nicer free functions still occupies the global value namespace (F9 puts functions and types in separate namespaces, but `list_get` as a free function is still an ambient global) and keeps UFCS resolution order (free functions own the dot position, F3) as a hazard; and shadowing via @std free fns has the same namespace problem. The prefix form has no defense worth preserving — it is a module prefix flattened into the global value namespace, which no surveyed language does.

### (c) Survey: how peers expose collection operations

- Rust: inherent methods on the concrete types (`vec.push`, `map.get`, `map.insert`) plus traits for the shared axes (`Iterator`, `IntoIterator`, `Extend`). Same conceptual operation, per-type implementation, member spelling. No free-function prefixes.
- Kotlin: member functions on the collection interfaces (`list.add`, `map.get`, `map[key]`) plus extension functions in the `kotlin.collections` package — package-namespaced, resolved as extensions, never global. The standard library's own prefix-era naming (`listOf`, `mapOf`) applies to constructors, not operations.
- Gleam: no methods; per-type modules (`list.map`, `dict.get`, `list.reverse`) — free functions namespaced by module, first argument the collection. The module is the namespace, so no stringly prefix exists.
- Roc: identical structure — `List.map`, `Dict.get`, `Set.insert`: module-namespaced free functions. Roc keeps the prefix idea but the prefix is a real module with import control, not a naming convention in one global namespace.
- MoonBit: methods on types (`xs.push(x)`, `xs[i]`) alongside per-package free functions.

The pattern is uniform: languages with members or UFCS use member spelling (Rust, Kotlin, MoonBit, and DreamScript's ruled end state); languages without methods use module namespaces (Gleam, Roc). Nobody keeps `list_push`-style prefixes in a single global function namespace, because that form has the discoverability cost of a prefix without the namespacing benefit of a module.

### Recommended path

1. Now (S effort, no language ruling needed — H5.5 already rules it): wire `xs.push(x)`, `xs.get(i)`, `m.get(k)`, `m.set(k, v)` as ambient inherent members backed by the existing native seams, retiring the `list_*`/`map_*` spellings from the user surface. The natives remain as VM-internal names, not user spellings. Item 69's element-carrying clause closes on this route; its ds-body framing is superseded by the native-backed framing, which should be recorded on the item.
2. Later (rides item 6, JIT tier): migrate the member bodies to ds source in @std as per-instantiation frames become available, per the H5.5 library-boundary direction. The user surface does not change.
3. Keep `cons`/`tail`/`collect`/`list_concat` on the same trajectory: they are the remaining span-moving free functions and deserve the same member spelling review, but they move whole spans (no element-carrying variable in the same sense) and can migrate with the @std collections round.

## Q3: JSON architecture

### The complaints, restated against the machinery

Three complaints: `derive from_json` is a builtin closed derive, not user-implementable machinery; a type that needs both json and toml has no path to a second synthesized decoder; and the json face predeclares ambient names (`json` enum, `parse_json`, `to_json`, `json_member`, `json_as_*`) into every compile. The relevant rulings: G1 (2026-11-14) — the derive vocabulary is closed to third parties, builtin families are checker-synthesized, unknown names refuse; the one sanctioned comptime seam is `fields(t)`, and libraries write comptime functions "without code synthesis or identifier minting." G2 — reflection is comptime-only; runtime type questions are answered by tags, and no type objects or metadata tables exist. B6 — coherent traits with no orphan rule: any module may instance any trait for any type. H5.5 json (2026-11-17..18) — json is @std-owned source, `from_json`/`to_json` a derive family over the typed readers, never a comptime extension.

### Option (a): open derive registration via comptime reflection — analysis

The proposal: let a library register a derive, implemented as comptime functions over `fields(t)`. This collides with the architecture at four load-bearing points, and the collision is structural rather than engineering difficulty.

First, there is nothing to synthesize with. Derive expansion today is checker-synthesized (the seven families expand after parse into ordinary impl items, M6); a comptime function's answer embeds at the call site as a literal a user would have written — a scalar or a list — never as code, and comptime functions never reach a runtime module. An open derive needs to produce an impl body, which is code synthesis and identifier minting: exactly the channel G1 deleted to make hygiene structural. Admitting it reintroduces macros through the comptime door.

Second, the trait-default-body route fails on G2. The remaining macro-free shape for "user-defined derive" would be a trait whose default body computes the decode from `fields(Self)` — but trait member bodies run at runtime, and reflection is comptime-only: `fields(t)` does not exist at runtime, deliberately (G2: runtime type questions are answered by tags, no metadata tables). Evaluating the body at comptime per instance is precisely what a derive does, which brings us back to synthesis by the checker. Splitting the difference — a comptime function that returns a decode plan as data, interpreted by a runtime walker — is a runtime-reflection interpreter in disguise: it needs per-field word access at runtime, which the one-word runtime-evidence seam and the no-metadata ruling both refuse, and it would carry item 6's per-instantiation problem for every generic target.

Third, H3's one-generic-body model: a synthesized body over a generic declaration needs per-instantiation frames (item 6 already blocks this for the builtin derives). An open derive system would multiply that demand by every library.

Fourth, explainability and error quality: checker-synthesized derives produce checkable, documented refusal classes (the from_json/to_json refusal taxonomy is pinned in status). User comptime functions producing code reintroduce the debuggability and hygiene questions the language deleted.

Conclusion: option (a) requires amending G1 (closed vocabulary, no synthesis), G2 (comptime-only reflection), and H3 (one body) simultaneously. That is three of the language's most load-bearing rulings for a feature whose value the serde survey below shows can be captured without it. Not recommended.

### Option (b): closed derives over an open serializer substrate

The productive split is serde's, minus macros: separate the per-type question (how do this type's fields encode/decode — mechanical, derivable) from the per-format question (what is a string, a number, an object, an array in this format — library code). Concretely:

- @std defines a small driver trait pair — an `encoder` face and a `decoder` face — describing the format-neutral shapes (scalar kinds, sequence begin/end, keyed-member walk), mirroring serde's Serializer/Deserializer and Swift's Encoder/Decoder at the type level.
- The closed derive vocabulary gains nothing per format. Either the existing `from_json`/`to_json` are reinterpreted as the json instance of a format-agnostic closed derive pair (one new derive name, e.g. `encode`/`decode`, checker-synthesized over the driver traits), or the json pair stays as-is and the generalized pair lands beside it. The synthesis algorithm is the one the checker already runs for json, parameterized over the driver face instead of the concrete `json` enum.
- json, toml, yaml are ordinary packages supplying a driver (an impl of the decoder/encoder faces over their document model). B6's no-orphan-rule is exactly what lets a third-party package do this without cooperation from the type's author.
- Users hand-implement the per-type traits whenever the derived shape is wrong for them — coherent traits, no orphan rule, so this works for foreign types too. This answers "user-implementable": today the json members are synthesized as receiverless members per target, and nothing in the surface lets a user supply the toml analog with synthesis; under this option, hand-implementation is universal and synthesis is format-agnostic.

The both-formats question dissolves: `user.decode(json_doc)` and `user.decode(toml_doc)` dispatch on the driver's type, or the two packages expose `json.decode(user)` / `toml.decode(user)` free fns over the same traits. Note the generic-over-driver call is an ordinary bounded generic, which H3 already serves with evidence words.

Tension to record: the driver face must be designable within v1's no-HKT bound (B6/H3), since serde's own abstractions lean on higher-kinded shapes in places (lifetimes aside, serde's `Deserialize<'de>` is the hard part, and DreamScript's borrow-never-escapes C2 rule deletes the borrowed-data problem by construction — decoded values are owned, so the driver face needs no lifetime analog). A depth-budgeted reader (already ruled for json) generalizes to the driver contract.

### Option (c): status quo

Keep json as the one blessed format: ambient predeclared face, closed `from_json`/`to_json` derives, and any toml/yaml package exposes hand-written trait impls with no synthesis. Cost: the two named complaints stand (no second synthesized format, ambient names). Benefit: zero rulings amended, zero machinery added. The ambient-name half is already slated to shrink under the H5.5 library boundary (item 53's phasing) regardless.

### Survey: builtin simplicity vs extensibility in peers

- Rust serde: proc-macro derives generating `Serialize`/`Deserialize` impls; formats implement `Serializer`/`Deserializer`. Maximal extensibility and zero runtime reflection, at the cost of a macro subsystem, its compile times, and its error quality. DreamScript deletes the macro half by ruling; the trait half is the part worth importing.
- Swift Codable: compiler-synthesized `Encodable`/`Decodable` conformance; formats (JSONEncoder, PropertyListEncoder, third parties) implement `Encoder`/`Decoder`; users hand-override `init(from:)`. This is option (b) with a closed synthesis — the closest existing proof that the split works without macros and without runtime reflection.
- Zig std.json: comptime reflection (`@typeInfo`) feeding comptime code generation through generic instantiation — Zig's comptime is a codegen facility, so std.json walks any struct with no macros and no derive registry. This route is structurally closed to DreamScript: G1's comptime deliberately answers with literals, not code.
- Gleam decode: decoders are runtime values composed by functions (`decode.field("name", decode.string, ...)`). Fully library-level, zero compiler involvement, errors at runtime, per-type boilerplate. Explainable and decoupled, but it spends the compile-time-error advantage DreamScript's philosophy prices highest.
- Roc: `Encoding`/`Decoding` abilities with builtin derivation; formats implement the ability's low-level `Encoder`/`Decoder` interface. Same shape as Codable: closed derivation, open formats.
- Go encoding/json: runtime reflection plus struct tags. The cautionary tale: maximal convenience, errors at runtime, weak typing, silent mismatches — the opposite of every stated DreamScript priority.

The survey's verdict is consistent: the winning shape is closed, compiler-side per-type synthesis over an open, trait-defined format substrate (Swift, Roc, and serde's trait half). The losing shapes are runtime reflection (Go) and macro-dependent synthesis (Rust's cost side). Gleam's pure-library shape is the only macro-free alternative in production, and it pays with runtime errors and boilerplate.

### Recommended end state and required amendments

End state: option (b) — a Codable/Roc-shaped substrate. Keep G1's closure (no third-party derives, no synthesis seam, hygiene stays structural). @std owns the format-neutral `encoder`/`decoder` driver traits. The derive vocabulary stays closed; the json-specific `from_json`/`to_json` derives either generalize to one closed format-agnostic pair or keep their names as the json instance of the generalized synthesis. json's face leaves the ambient for `@std/json`, imported selectively (G4) — this answers the ambient-name complaint and continues item 53's boundary direction. toml/yaml become external packages implementing the driver face, getting full synthesis for free on any deriving type and hand-written impls everywhere else. All decode failures stay `parse_err`-shaped union members — compile-time-checked containment, no runtime type machinery.

Amendments required in design-decisions.md: H5.5's json ruling (2026-11-17..18) needs a one-paragraph amendment re-scoping the derive from "a derive family over the typed readers" of the concrete `json` enum to "the closed synthesis over the @std driver face, json its first driver," and moving the ambient predeclaration to `@std/json`; the 2026-11-21 discriminator ruling survives unchanged as json-driver-internal detail. G1 (2026-11-14) needs no amendment under option (b) — the vocabulary stays closed — unless the author picks the generalized derive name, in which case G1's text gains one sentence naming the format-agnostic pair as belonging to the closed vocabulary. G2 and H3 are untouched. Option (a) would require amending G1, G2, and H3 and is not recommended.

## Recommended rulings

### Q1 candidate rulings

- Keep `is` unchanged: expression-position type test, right side a full type expression, refinement as ruled in B2. (Recommended.)
- Keep `is` but restrict the right side to nominal atoms and unions of atoms; intersections and differences in test position refuse until corpus evidence demands them.
- Retire `is` and route all narrowing through match arms (pattern-only camp). Recorded for completeness; not recommended — it removes the expression face of B2 while keeping its engine and breaks the F8 early-exit idioms.
- Rejected without candidacy: an `as?` cast channel; a MoonBit-style pattern right side (deferred-backlog material at most).

### Q2 candidate rulings

- Adopt the ruled H5.5 surface now via native-backed inherent members (`xs.push`, `xs.get`, `m.get`, `m.set`) over the existing native seams, retiring the `list_*`/`map_*` user spellings; ds-source generic member bodies migrate when item 6 lands on the JIT tier. (Recommended; no design-decisions amendment needed, re-scopes todo item 69.)
- Hold the prefix surface until item 6 lands and migrate directly to ds-source members. Cleaner internally, couples a surface ruling to M8-tier scheduling.
- Interim half-step: retire only the read-side prefixes (`list_get`/`map_get` to `.get`) and keep `list_push` until the mut-window members are wired. Lower risk per round, two transitions instead of one.

### Q3 candidate rulings

- Adopt the serializer substrate: @std `encoder`/`decoder` driver traits, the closed derive synthesis parameterized over the driver face, json as the first driver package at `@std/json`; amend H5.5's json ruling, keep G1 closed. (Recommended end state.)
- Interim: keep `from_json`/`to_json` as-is but move the json face out of the ambient into `@std/json` now, answering the ambient-name complaint while the driver face is designed. Composes with the recommended end state.
- Status quo as policy: json is the one format with synthesis; toml/yaml packages ship hand-written trait impls forever. Zero amendments; records the two complaints as accepted costs.
- Rejected without candidacy: open derive registration through comptime `fields(t)` — requires amending G1, G2, and H3 to reintroduce the synthesis channel G1 deleted.
