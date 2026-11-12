# Conformance corpus

The DreamScript conformance corpus: the frozen archive's seed programs (`design/en/design/` catalogs) ported to the decided surface syntax per the M1 work items of `docs/en/implementation-plan.md`. The runtime encodings (catalog 07-runtime) are covered by the ds-abi/ds-vm hand-assembled fixtures instead. Every future bug regression lands in this directory as a new item or a new file; a regression never lives only in a unit test. Every `.ds` file sits at `ds fmt`'s canonical form (the CI ds-fmt job gates the corpus alongside examples/ and the benchmark kernels), so run the formatter after editing one; the header blocks and separator lines are preserved verbatim.

## File list

| File | Archive catalog | Run-binding milestone | Content |
|---|---|---|---|
| types_hybrid.ds | 01-types | M3 | config parser: enum-minted cases, unions, `is`-narrowing, bare `return`, `?`, `.err`, structural `==` |
| linearity_modes_rc.ds | 02-linearity-ownership | M4 | linear `file` with `deinit`, `mut` windows, affine move, COW split, fused chain |
| effects_nursery.ds | 03-effects-errors-async | M5 (check) | nursery `fetch_all` with cancel-on-failure, retry loop, `par_map`/`par_reduce`; the fetch-driven items check clean and wait on their non-nursery seams (the `fetch` extern has no builtin seam, ambient data values, unions over generic applications, the par combinators) — the nursery and retry shapes themselves run (nursery_surface.ds and the e2e probes) |
| syntax_ts_heart.ds | 04-syntax | M3 | full surface: guards, bare `return`, `?`, `.err`, interpolation, destructuring, byte literal; the row reads wait on the generic `get` callee (the M6 seam maps_surface records) |
| generics_coherent_traits.ds | 05-generics-metaprogramming | M2 (check), M6 (comptime) | `~Copyable` stack, disjoint heads, `#[derive]` + comptime schema |
| modules_hash_core.ds | 06-modules-tooling | M7 | ds.pkg manifest, selective imports, linear sqlite handle |
| deinit_surface.ds | record C5, the deinit wiring round 2026-11-11 | M4 | declared `deinit` blocks run observably at the value's unique death: scope end, early return, `?` propagation, var overwrite; borrow parameters never fire the callee's hook |
| linearity_negative.ds | C1, C5 | M2 (check) | reject items: moved-then-used, move-mode parameters, projection transfers, moves across match arms and loops, deinit outside `~Copyable` |
| range_for.ds | plan near-term queue, tutorial-gap round 2026-11-10 | M3 | `for x in a..b` / `a..=b` counted loops (empty, single-step, exclusive/inclusive bounds, non-constant ends, wildcard patterns) and `x in a..b` membership |
| nursery_surface.ds | M5, the nursery/spawn surface round, the payloaded-cancel round | M5 | the in-language nursery surface: spawn-in-scope, join at scope exit in spawn order, first-failure cancellation at safe points with payloaded cancel shapes (a struct member's flat span, a generic application's one Ref word), child error unioning out at the boundary, a completing child's deinit claim |
| nursery_deep.ds | the cross-nursery join interlock round 2026-11-12 | M5 | the recursive nursery block: a spawned child's body joining its own fresh nursery while an enclosing drive owns the child stands one interlock frame per recursion level, and the iterative interlock drains depth 2000 without the Rust stack (the recursive driver overflowed the process near depth 50) |
| while_cond_refs.ds | record C4, the while-cond placement round 2026-11-12 | M3 | a Ref operand of a while condition is loop-carried, its count dying past the loop instead of at the per-iteration head use: the dying string/list/compound shapes, nested while heads, a `~Copyable` guard whose deinit claim composes with the head reads, and uses after the loop stay live |
| pattern_dispatch.ds | tutorial verification pass 2026-11-10 | M3 | bug regressions: bare-tag match arms dispatch by value (both orders), or-patterns of unit tags, tail-`?`, mid-block `return e?`, and branch-final `?` over a contained union, a strict-sub-union `return e` and binding/assignment; load_config stages typecheck (config's optional field and the file's own extern — the tail-`?` shape is the fallible-union e2e's regression) |
| lists_surface.ds | record B3/F7/F9, the constructor round 2026-11-10, the iteration round 2026-11-11, the native-tables round 2026-11-12 | M3 | list literals, the `list[t]()` empty constructor, the empty literal against a declared return, checked reads over built lists, `for x in xs` iteration and `x in xs` membership, `join` rendering the parts back to one string (the empty list to the empty one), the mixed-element rejection |
| tuples_surface.ds | record B3, the expected-gap round closing tuple lowering | M3 | anonymous tuples: construction, destructuring in bindings and match arms, one-word frame elements, the 1-tuple and `()`, the union-destructuring rejection |
| maps_surface.ds | record B3/F7/D2, the builder-call interim round | M3 | the `map[k, v]()` empty constructor, `map_set`'s mut-window construction, checked reads over the hit and miss members, the entry count, the `get` spelling (typecheck; its generic dispatch waits for the M6 seam), the report builder's string `+` operands (run; the concat native's own call shape), the constructor-arguments rejection |
| extraction_family.ds | record E4, the tutorial-block plumbing round 2026-11-10, the native-tables round 2026-11-12 | M3 | the @std extraction family: eager `unwrap_or`, inlined-lazy `unwrap_or_else`, `ok_or` composed with `?` (tail and `return e?`), `ok_or`'s degenerate union (the err value's own type is the extracted side, so the result collapses to a plain binding) and the bound union read through match, `map_err` over the handled members, the non-optional receiver rejection |
| calls_surface.ds | tutorial verification pass (R9), call-site staging [F7, BL-6]; the R26 indirect-arity round | M3 | literal parameter defaults stage at the call site; an inline union-member argument against a multi-word parameter region refuses at lowering instead of trapping in the VM; a fn-typed value called through its slot checks the argument count (the let-stored lambda call runs, the extra argument is the reject) |
| closures_surface.ds | record F2, F7, the b07 callable-word seam audit 2026-11-10, the block-argument round 2026-11-12 | M3 | lambda values: fn-typed parameters, parenthesized arguments, let-stored and passed lambdas, a capturing closure, and the trailing block (staging exactly like the written trailing lambda) run through the callable words |
| inherent_impls.ds | F3 method tier, the R26/R31 inherent-impl checker rounds, the inherent dispatch landing, the member-tail and default-staging round | run (inherent member bodies plan under the `<target>#<member>` mangle and dot-calls dispatch as plain local calls; generic inherent impls run in the ds-lower evidence suites) | two owner types share one method name, each dot-call selects its own owner's signature, the receiver binds `Self` for field reads, a member tail-recursion transfers through CALLT at constant stack, an omitted defaulted member parameter stages the literal default at the call site, and a wrong-receiver dot-call is the reject |
| toplevel_consumption.ds | G3 order-independent top levels plus the R26 package-wide linearity round | M2 (check); execution refuses at lowering (the file's own extern callee; the lowerer also reads top levels in source order) | a top-level consumer declared before the linear value it consumes, the twice-consumed seed as the reject; the cross-file direction (an earlier file's binding consuming the corpus's own `far_seed`) rides the gate's two-file harness — the corpus runners are single-file, which is the harness gap |
| trait_defaults.ds | F7 default staging, the trait-default authority round 2026-11-12 | M3 | the trait declaration is a defaulted member parameter's single staged authority (two impls omit the default), all four call spellings (dot and `Trait::method`, omitted and written argument) answer identically, and an inherent member keeps its own body default |
| records_lists.ds | the native result span channel round 2026-11-12 | M3 | a `list[record]` built with `list_push` over whole element spans, the checked `list_get` read extracted through `.ok`/`.err` (a missing index answered on the none side), field projections summed over the explicit-index walk |
| transitive_dispatch.ds | the transitive evidence round 2026-11-12 (H3) | M3 | the two-level generic-impl dispatch chain: a generic `tells` impl for `list[t]` dispatching through the list's own generic `shows` impl, entered at int and string plus one nested-list hop, the entering site staging the serving body's whole evidence area |
| teardown_deinit.ds | E5 teardown firing, the drain-doors round 2026-11-12 | M5 | a nursery whose children hold deinit-carrying values: the completing child's hook fires at its own frame exit, and the cancel-cut child's kept frame fires its hook at the teardown drain — the prints deterministic across runs (the corpus's E5 end-to-end pin) |

## Header format

Every `.ds` file starts with a header comment block that ds-parse tests parse mechanically:

```text
// source: <record reference>
// purpose: <one line>
// items:
// <item-id>: stage: <stage-expression>
```

Rules: exactly one `source` line and one `purpose` line, one `items:` line, then one item line per top-level item, listed in source order. An item-id is `[a-z0-9_-]+`: the declared identifier when the item declares one, otherwise a descriptive slug (for example `impl-json-int`, `ds-pkg-manifest`). A stage-expression is one of the vocabulary entries below; `reject` and `unresolved` carry free text after the keyword. Items with stage `unresolved` are preserved in the body as commented-out code and are skipped by every runner; items with stage `reject` are live code that must parse.

## Stage vocabulary

| Stage | Binds at | Meaning |
|---|---|---|
| `parse` | M1 | item must parse; no further expectation yet |
| `typecheck` | M2 | item must typecheck under ds-check (syntax_ts_heart.ds's byte literal `b'x'` types through the ambient's `tag byte` with no user declaration) |
| `run` | M3-M5 | item must execute with correct output once its file's run-binding milestone lands (see file list) |
| `m6` | M6 | item needs derives, comptime evaluation, or reflection |
| `m7` | M7 | item needs modules, packages, or FFI |
| `reject <what>` | M2 unless the text names later | negative item: must parse, then produce the described error |
| `unresolved: <why>` | never | spelling genuinely undecided in the record or out of scope for v1 |

Cumulation rules: every item except `unresolved` must parse from M1. Items staged `typecheck` or `run` must typecheck from M2. Items staged `run` execute from their file's run-binding milestone. Items staged `m6` or `m7` are exempt from the typecheck and run gates until their own milestone, because their constructs (comptime parameters, extern declarations, imports) do not check earlier. Golden output files land together with the milestone that first runs each file.

## Normalization decisions vs the record examples

The archive's seed programs predate the surface decisions for catalogs 01/02/03/05/06; the record's prose decisions win over the archive's examples. The normalizations applied here:

1. Named functions: `fn name(args) -> t { ... }` is the only named-function declaration [F2, adopted 2026-11-10]; `let` becomes `const` for value bindings, and the interim const-arrow spelling (`const name = (args) -> t { ... }`) is retired at the top level (DS1013 suggests the `fn` spelling). `extern fn` keeps its own form; ds-ffi (M7) owns that spelling.
2. Type names are lowercase per record section 0 (`int`, `string`, `bad_port`). The trait self-type is `Self` in type position, and the receiver is bare — B6's `fn compare(self: &Self, other: &Self) -> int` is written `fn compare(self, other: Self) -> int` [record 6.1]: plain `self` is the borrow receiver, `mut self` the exclusive window, both exempt from annotation; `self` as a type name is retired (DS2015 names `Self`, DS2016 the annotated receiver).
3. Early exits use bare `return` [F2] with `is` narrowing, final-expression returns, `.err(h)` fallback handlers, and (inside loops) a flag binding plus `break` [E4, F8 — the `or` operator is retired]; see `with_retry` in effects_nursery.ds.
4. Interpolation holes convert under the F6 printable rule: a printable primitive (`int`, `float`, `bool`, `char`, `string`) converts through `str(e)` implicitly, so the record's `${e}`, `${b.got}`, `$t` lower as written; a union or optional operand is an error (extract first), and every other type waits for the M6 `show` derive. The bare `$x` form is the same rule over one identifier.
5. Trait and impl members are `fn` declarations, bodiless signatures in traits [F2, record 6.1]; generic functions carry a bracketed type-parameter clause after the name (`fn push[t: ~Copyable](s: stack[t], ...) -> ...`).
6. types_hybrid.ds: `missing` stays a bare case used bare, following B2's own decided rewrite of `get_port` (the catalog's `Missing { key: ... }` payload is dropped); the `j is json_obj or return ...` pseudocode becomes `if j is json_obj` narrowing per B2; the json value family and the config error family are enum blocks per B3's payload-case story (cases mint into the tag namespace, so `int | missing | bad_port` and `config | cfg_err` are set expressions over them), and case construction is call form (`bad_port(got: -1)`).
7. syntax_ts_heart.ds: the alias-declared cases `type parse_error = bad_number(string) | missing_field(string)` become an enum block (type aliases never declare cases per B3), keeping the case names and payload spellings verbatim; io failures mint no file enum — they ride the ambient's `io_err` tag, and `parse_age` matches the ambient `parse_err` member the success-typed `parse_int` answers.
8. linearity_modes_rc.ds: the single-line `while` body is expanded to newline-terminated statements [F1]; `raw_fd` is declared as `type raw_fd = int`; compound assignment `+=` is kept verbatim (used by two seed programs, contradicted by no decision).
9. effects_nursery.ds: `val pages = Mut[List[Page]]()` becomes `var pages = list[page]()` (D2 COW list; `freeze` dropped since a COW list is already a value, and the empty-construction spelling `list[t]()` is a stdlib surface choice owned by M3); the transient error arms are member tests over the ambient `net_err` cases (`timeout`/`refused`) with the leftover member a plain binder; positional `Page(url, body)` construction becomes named fields; `p.size` becomes `p.body.len()`; the file declares `type url = string` while `net_err`, `response`, `fetch`, `consume`, and the `urls` const are ambient-owned.
10. generics_coherent_traits.ds: `@derive(...)` becomes `#[derive(...)]` with lowercase derive names [F7, G1]; the anonymous-record alias `type Stack[T] = { items: List[T] }` becomes `struct stack[t] { items: list[t] }` (anonymous record types are deferred-additive [B3]); `|x|` lambdas become `(x) => ...` [F2]; stack operations are functional under D2 (collections expose a functional API and mutate in place iff unique) because `&mut self` field assignment has no decided surface; `ord` joins point's derives so `max` resolves; the execution item is staged m6 because it needs the derive engine.
11. modules_hash_core.ds: the ds.pkg manifest is preserved as a comment block (manifest syntax is parsed by ds-pkg at M7, never by ds-parse); import symbols are lowercased; the extern declarations keep the record's C-type notation near-verbatim (`*const c::char`, `c["sqlite3"]`, `c::int` — `c.char` parses identically, and `ds fmt` canonicalizes the type-path separator to `::`), and ds-ffi (M7) owns the final spelling; the `double_close` function is added as the reject item proving the linear-handle rule the record states in its comment.
12. Multi-statement blocks in match arms use braces with newline-terminated statements; `for` bodies always brace [F1].

## Ambient names

The stdlib ambient (`crates/ds-stdlib/src/ambient.ds`) declares the vocabulary the corpus assumes, registered as a package member before the user's sources; it also owns the failure taxonomy (`io_err`, `parse_err`, `enum net_err { timeout, refused }`) and the `config`/`row`/`response` types, so those names need no file-local declaration:

- types_hybrid.ds: `read_file: (string) -> string | io_err`, `parse_json: (string) -> string` (success-typed; the `json | io_err` shape waits on M7 package-owned types), `log`, `default_config: () -> config` over the ambient `config` struct; the file owns its `json`/`cfg_err` enums.
- linearity_modes_rc.ds: `to_u32_le: (bytes) -> u32`, `close: (int) -> ()`, `map`, `collect`, `len`, and the `bytes`/`u32` tags (the `words` binding builds by literal since the constructor round); `open`/`read_exact` stay absent because their signatures cannot name the caller's file-owned `file` struct — the M7 package-owned-types blocker.
- effects_nursery.ds: the ambient-owned `net_err` enum, `response`, `fetch: (string) -> response | net_err`, `consume: (response) -> string`, the `urls` const (a running use waits on ambient data values lowering), `list[t]()` empty construction, `list_push: (mut list[t], t) -> ()`, `cancel`, `par_map`, `par_reduce`, `print`, `die`, `len`; the file owns `type url = string` and `page`.
- syntax_ts_heart.ds: the `row` alias and `config` struct, `load_rows: (string) -> list[row] | io_err`, `read_config: (string) -> config | io_err`, `default_config`, `parse_int: (string) -> int | parse_err`, `println`, `trim`, `get`, `map`, `partition`, `filter`, `reduce`, `first`.
- generics_coherent_traits.ds: `cons`, `head: (list[t]) -> t?`, `tail`, `join`, `map`, `str: (t) -> string`, `max: (t, t) -> t`, and the comptime builtins `fields`/`type_name` (M6).
- modules_hash_core.ds: `os` (build target) and the `c` namespace of C types and handles.
- while_cond_refs.ds: `len` and `println`; the file owns its `guard` struct.
- pattern_dispatch.ds: `read_file: (string) -> string | io_err` and `println`, both against the ambient taxonomy; the file declares its own `parse_config` extern over the ambient `config`.
- lists_surface.ds: `list_new`/`list_len`/`list_get`/`list_concat`/`list_push`/`join` (the ambient list surface).
- tuples_surface.ds: `list_len` (the one list read the list-bearing tuple item builds against; tuples themselves need no ambient).
- maps_surface.ds: `map_new`/`map_set`/`map_get`/`map_len` (the ambient map surface; the constructor spelling is `map[k, v]()`), plus the record's `get` spelling the tutorial's field lookups use.
- inherent_impls.ds: the primitives alone; the file owns its receiver types and impls.
- toplevel_consumption.ds: the primitives plus the `Copyable` marker; the file declares its own linear `blob` and `digest_of`, and the gate's ambient adds one consumer of the file's `far_seed` (the cross-file direction).
- trait_defaults.ds: `len` and `println`; the file owns its trait, both impls, and the receiver types (the inherent impl included).
- records_lists.ds: `list_len`/`list_get`/`list_push` (the ambient list surface) and `println`; the file owns its `part` struct.
- transitive_dispatch.ds: `println` beside the primitives and the `list` type; the file owns both traits and the four impls.
- teardown_deinit.ds: the ambient-owned `net_err` taxonomy, the `cancel` seam, and `println`; the file owns its `resource`.
