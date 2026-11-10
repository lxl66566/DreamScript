# Conformance corpus

The DreamScript conformance corpus: the frozen archive's seed programs (`design/en/design/` catalogs) ported to the decided surface syntax per the M1 work items of `docs/en/implementation-plan.md`. The runtime encodings (catalog 07-runtime) are covered by the ds-abi/ds-vm hand-assembled fixtures instead. Every future bug regression lands in this directory as a new item or a new file; a regression never lives only in a unit test.

## File list

| File | Archive catalog | Run-binding milestone | Content |
|---|---|---|---|
| types_hybrid.ds | 01-types | M3 | config parser: enum-minted cases, unions, `is`-narrowing, bare `return`, `?`, `.err`, structural `==` |
| linearity_modes_rc.ds | 02-linearity-ownership | M4 | linear `file` with `deinit`, `mut` windows, affine move, COW split, fused chain |
| effects_nursery.ds | 03-effects-errors-async | M5 | nursery `fetch_all` with cancel-on-failure, retry loop, `par_map`/`par_reduce` |
| syntax_ts_heart.ds | 04-syntax | M3 | full surface: guards, bare `return`, `?`, `.err`, interpolation, destructuring, byte literal (staged `parse`) |
| generics_coherent_traits.ds | 05-generics-metaprogramming | M2 (check), M6 (comptime) | `~Copyable` stack, disjoint heads, `#[derive]` + comptime schema |
| modules_hash_core.ds | 06-modules-tooling | M7 | ds.pkg manifest, selective imports, linear sqlite handle |
| linearity_negative.ds | C1, C5 | M2 (check) | reject items: moved-then-used, move-mode parameters, projection transfers, moves across match arms and loops, deinit outside `~Copyable` |
| range_for.ds | plan near-term queue, tutorial-gap round 2026-11-10 | M3 | `for x in a..b` / `a..=b` counted loops (empty, single-step, exclusive/inclusive bounds, non-constant ends, wildcard patterns) and `x in a..b` membership |
| pattern_dispatch.ds | tutorial verification pass 2026-11-10 | M3 | bug regressions: bare-tag match arms dispatch by value (both orders), or-patterns of unit tags, tail-`?`, mid-block `return e?`, and branch-final `?` over a contained union, a strict-sub-union `return e` and binding/assignment |
| lists_surface.ds | record B3/F7/F9, the constructor round 2026-11-10 | M3 | list literals, the `list[t]()` empty constructor, the empty literal against a declared return, checked reads over built lists, the mixed-element rejection |
| tuples_surface.ds | record B3, the expected-gap round closing tuple lowering | M3 | anonymous tuples: construction, destructuring in bindings and match arms, one-word frame elements, the 1-tuple and `()`, the union-destructuring rejection |

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
| `parse` | M1 | item must parse; no further expectation yet (syntax_ts_heart.ds's byte-literal item: the record pins `b'x'` as a single byte but names no byte type, so nothing past parse is checkable) |
| `typecheck` | M2 | item must typecheck under ds-check |
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
7. syntax_ts_heart.ds: the alias-declared cases `type parse_error = bad_number(string) | missing_field(string)` and `type io_error = not_found | permission_denied` become enum blocks (type aliases never declare cases per B3), keeping the case names and payload spellings verbatim.
8. linearity_modes_rc.ds: the single-line `while` body is expanded to newline-terminated statements [F1]; `raw_fd` is declared as `type raw_fd = int`; compound assignment `+=` is kept verbatim (used by two seed programs, contradicted by no decision).
9. effects_nursery.ds: `val pages = Mut[List[Page]]()` becomes `var pages = list[page]()` (D2 COW list; `freeze` dropped since a COW list is already a value, and the empty-construction spelling `list[t]()` is a stdlib surface choice owned by M3); `error e` match patterns become plain binding patterns (the arm binds the leftover member by set difference); positional `Page(url, body)` construction becomes named fields; `p.size` becomes `p.body.len()`; `url` is declared as `type url = string` and `urls` stays ambient as in the record.
10. generics_coherent_traits.ds: `@derive(...)` becomes `#[derive(...)]` with lowercase derive names [F7, G1]; the anonymous-record alias `type Stack[T] = { items: List[T] }` becomes `struct stack[t] { items: list[t] }` (anonymous record types are deferred-additive [B3]); `|x|` lambdas become `(x) => ...` [F2]; stack operations are functional under D2 (collections expose a functional API and mutate in place iff unique) because `&mut self` field assignment has no decided surface; `ord` joins point's derives so `max` resolves; the execution item is staged m6 because it needs the derive engine.
11. modules_hash_core.ds: the ds.pkg manifest is preserved as a comment block (manifest syntax is parsed by ds-pkg at M7, never by ds-parse); import symbols are lowercased; the extern declarations keep the record's C-type notation (`*const c.char`, `c["sqlite3"]`, `c.int`) near-verbatim, and ds-ffi (M7) owns the final spelling; the `double_close` function is added as the reject item proving the linear-handle rule the record states in its comment.
12. Multi-statement blocks in match arms use braces with newline-terminated statements; `for` bodies always brace [F1].

## Ambient names

Stdlib and environment names the corpus assumes; the M3+ minimal stdlib grows to cover them:

- types_hybrid.ds: `read_file: (string) -> string | io_err`, `parse_json: (string) -> json | io_err`, `log`, `default_config: () -> config`, `map`/`list` types.
- linearity_modes_rc.ds: `open: (string) -> file`, `close: (raw_fd) -> ()`, `read_exact: (mut file, int) -> bytes`, `to_u32_le: (bytes) -> u32`, `map`, `collect`, `len` (the `words` binding builds by literal since the constructor round).
- effects_nursery.ds: `urls: list[url]`, `fetch: (url) -> response | net_err`, `consume: (response) -> string`, `list[t]()` empty construction, `push: (mut list[t], t) -> ()`, `cancel`, `par_map`, `par_reduce`, `print`, `die`, `len`.
- syntax_ts_heart.ds: the `row` and `config` types, `load_rows: (string) -> list[row] | io_error`, `read_config: (string) -> config | io_error`, `default_config`, `println`, `trim`, `parse_int`, `get`, `map`, `partition`, `filter`, `reduce`, `first`.
- generics_coherent_traits.ds: `cons`, `head: (list[t]) -> t?`, `tail`, `join`, `map`, `str: (t) -> string`, `max[t: ord](t, t) -> t`, and the comptime builtins `fields`/`type_name` (M6).
- modules_hash_core.ds: `os` (build target) and the `c` namespace of C types and handles.
- pattern_dispatch.ds: `read_file: (string) -> string | io_error` and `println` (both against the file's own tags; the file declares its own `parse_config` extern).
- lists_surface.ds: `list_new`/`list_len`/`list_get`/`list_concat` (the ambient list surface; `list_push` is the compiler-internal construction append behind literals).
- tuples_surface.ds: `list_len` (the one list read the list-bearing tuple item builds against; tuples themselves need no ambient).
