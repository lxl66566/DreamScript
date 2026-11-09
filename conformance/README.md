# Conformance corpus

The DreamScript conformance corpus: the seed programs of the authoritative design record (`docs/en/design-decisions.md`, sections 13.1-13.6) ported to the decided surface syntax per the M1 work items of `docs/en/implementation-plan.md`. Section 13.7 (runtime encodings) is covered by the ds-vm hand-assembled fixtures instead. Every future bug regression lands in this directory as a new item or a new file; a regression never lives only in a unit test.

## File list

| File | Record section | Run-binding milestone | Content |
|---|---|---|---|
| types_hybrid.ds | 13.1 | M3 | config parser: tags, unions, `is`-narrowing, `or return`, `?`, `.err`, structural `==` |
| linearity_modes_rc.ds | 13.2 | M4 | linear `file` with `deinit`, `mut` windows, affine move, COW split, fused chain |
| effects_nursery.ds | 13.3 | M5 | nursery `fetch_all` with cancel-on-failure, retry loop, `par_map`/`par_reduce` |
| syntax_ts_heart.ds | 13.4 | M3 | full surface: guards, `or return`, `?`, `.err`, interpolation, destructuring |
| generics_coherent_traits.ds | 13.5 | M2 (check), M6 (comptime) | `~Copyable` stack, disjoint heads, `#[derive]` + comptime schema |
| modules_hash_core.ds | 13.6 | M7 | ds.pkg manifest, selective imports, linear sqlite handle |
| linearity_negative.ds | C1, C5 | M2 (check) | reject items: moved-then-used, move-mode parameters, projection transfers, moves across match arms and loops, deinit outside `~Copyable` |

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
| `parse` | M1 | item must parse; no further expectation yet (no current item uses this stage) |
| `typecheck` | M2 | item must typecheck under ds-check |
| `run` | M3-M5 | item must execute with correct output once its file's run-binding milestone lands (see file list) |
| `m6` | M6 | item needs derives, comptime evaluation, or reflection |
| `m7` | M7 | item needs modules, packages, or FFI |
| `reject <what>` | M2 unless the text names later | negative item: must parse, then produce the described error |
| `unresolved: <why>` | never | spelling genuinely undecided in the record or out of scope for v1 |

Cumulation rules: every item except `unresolved` must parse from M1. Items staged `typecheck` or `run` must typecheck from M2. Items staged `run` execute from their file's run-binding milestone. Items staged `m6` or `m7` are exempt from the typecheck and run gates until their own milestone, because their constructs (comptime parameters, extern declarations, imports) do not check earlier. Golden output files land together with the milestone that first runs each file.

## Normalization decisions vs the record examples

The record's seed programs predate the section 6 surface decisions for catalogs 01/02/03/05/06; the record's prose decisions win over its examples. The normalizations applied here:

1. Named functions: catalog `fn name(args) -> t` becomes const-arrow declarations `const name = (args) -> t { ... }` [F2]; `let` becomes `const`. The `fn` keyword survives only in `extern fn`, for which no decided alternative exists; ds-ffi (M7) owns that spelling.
2. Type names are lowercase per record section 0 (`int`, `string`, `bad_port`), including the trait self-type: record `&Self` becomes `self` in type position, so B6's `fn compare(self: &Self, other: &Self) -> int` is written `const compare = (self: self, other: self) -> int`. The `&`/`&mut` reference spellings are pre-surface; the C2 modes make borrow the default and spell exclusive windows `mut` on parameters.
3. Bare `return` is deferred-additive [F2, section 12]: early exits are written as final-expression returns, `or return`, or (inside loops) a flag binding plus `break`; see `with_retry` in effects_nursery.ds.
4. Interpolation of non-string expressions wraps the hole in `str()` per F6 ("explicit `str()` call at the non-string boundary"): the record's `${e}`, `${b.got}`, `$t` become `${str(e)}`, `${str(b.got)}`, `${str(t)}`. The bare `$x` form has no remaining seed usage and is covered by parser unit tests instead.
5. Trait members are const-arrow signatures without bodies; generic functions carry a bracketed type-parameter clause between `=` and the lambda (`const push = [t: ~Copyable] (s: stack[t], ...) -> ...`), by analogy with the record's `impl[T: Ord]` heads.
6. types_hybrid.ds (13.1): `missing` stays the unit tag used bare, following B2's own decided rewrite of `get_port` (the catalog's `Missing { key: ... }` payload is dropped); the `j is json_obj or return ...` pseudocode becomes `if j is json_obj` narrowing per B2; the json value atoms are declared in-file because the record references `Json` without declaring it.
7. linearity_modes_rc.ds (13.2): the single-line `while` body is expanded to newline-terminated statements [F1]; `raw_fd` is declared as `type raw_fd = int`; compound assignment `+=` is kept verbatim (used by two seed programs, contradicted by no decision).
8. effects_nursery.ds (13.3): `val pages = Mut[List[Page]]()` becomes `var pages = list[page]()` (D2 COW list; `freeze` dropped since a COW list is already a value, and the empty-construction spelling `list[t]()` is a stdlib surface choice owned by M3); `error e` match patterns become plain binding patterns (the arm binds the leftover member by set difference); positional `Page(url, body)` construction becomes named fields; `p.size` becomes `p.body.len()`; `url` is declared as `type url = string` and `urls` stays ambient as in the record.
9. generics_coherent_traits.ds (13.5): `@derive(...)` becomes `#[derive(...)]` with lowercase derive names [F7, G1]; the anonymous-record alias `type Stack[T] = { items: List[T] }` becomes `struct stack[t] { items: list[t] }` (anonymous record types are deferred-additive [B3]); `|x|` lambdas become `(x) => ...` [F2]; stack operations are functional under D2 (collections expose a functional API and mutate in place iff unique) because `&mut self` field assignment has no decided surface; `ord` joins point's derives so `max` resolves; the execution item is staged m6 because it needs the derive engine.
10. modules_hash_core.ds (13.6): the ds.pkg manifest is preserved as a comment block (manifest syntax is parsed by ds-pkg at M7, never by ds-parse); import symbols are lowercased; the extern declarations keep the record's C-type notation (`*const c.char`, `c["sqlite3"]`, `c.int`) near-verbatim, and ds-ffi (M7) owns the final spelling; the `double_close` function is added as the reject item proving the linear-handle rule the record states in its comment.
11. Multi-statement blocks in match arms use braces with newline-terminated statements; `for` bodies always brace [F1].

## Ambient names

Stdlib and environment names the corpus assumes; the M3+ minimal stdlib grows to cover them:

- types_hybrid.ds: `read_file: (string) -> string | io_err`, `parse_json: (string) -> json | io_err`, `log`, `default_config: () -> config`, `map`/`list` types.
- linearity_modes_rc.ds: `open: (string) -> file`, `close: (raw_fd) -> ()`, `read_exact: (mut file, int) -> bytes`, `to_u32_le: (bytes) -> u32`, `split`, `map`, `collect`, `len`.
- effects_nursery.ds: `urls: list[url]`, `fetch: (url) -> response | net_err`, `consume: (response) -> string`, `list[t]()` empty construction, `push: (mut list[t], t) -> ()`, `cancel`, `par_map`, `par_reduce`, `print`, `die`, `len`.
- syntax_ts_heart.ds: the `row` and `config` types, `load_rows: (string) -> list[row] | io_error`, `read_config: (string) -> config | io_error`, `default_config`, `println`, `trim`, `parse_int`, `get`, `map`, `partition`, `filter`, `reduce`, `first`.
- generics_coherent_traits.ds: `cons`, `head: (list[t]) -> t?`, `tail`, `join`, `map`, `str: (t) -> string`, `max: [t: ord] (t, t) -> t`, and the comptime builtins `fields`/`type_name` (M6).
- modules_hash_core.ds: `os` (build target) and the `c` namespace of C types and handles.
