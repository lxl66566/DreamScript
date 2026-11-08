# 06 — Modules, tooling, FFI, and ecosystem: decision catalog

Status: Phase 2 option catalog, written 2026-11-04, awaiting author decisions per AGENTS.md. Primary evidence: `docs/en/research/06-modules-tooling-ffi.md` (cited below as research/06 with section numbers) and the author's own package-manager proposal (C:/programs/me/lxl66566.github.io/src/coding/package_manager.md, cited as UPM). Sister catalogs, referenced by path only — their contents are owned there: `docs/en/design/01-types.md` (type foundation), `docs/en/design/02-linearity-ownership.md` (linear/affine layer, points C1-C5), `docs/en/design/03-effects-errors-async.md` (effects/capabilities, points B5, E1-E5), `docs/en/design/04-syntax.md` (surface syntax), `docs/en/design/05-generics-metaprogramming.md`. All code in this file is illustrative; final surface syntax is decided in `docs/en/design/04-syntax.md`.

## Decision-point index

| ID | Decision point | Options | Recommendation (one line) |
|---|---|---|---|
| G3 | Module system and unit layering | (a) Rust crate+module tree with use-paths, (b) MoonBit-flavor layering with generated interface files and block model, (c) Unison content-addressed definitions, (d) TS/ESM files-as-modules | (b): definition < file < package < module, generated `.dsi` interfaces, `///|`-style order-independent blocks, cycles legal inside a package and banned across packages, pure compile-time module bodies — the one-rule-three-wins core (research/06 §1.7, §1.10-1.11, §6.3) |
| G4 | Visibility model | (a) pub/priv binary, (b) layered visibility with selective exports, (c) capability/capsule visibility, (d) Go exported-identifier convention | (b) with (a) as its reduced subset: `pub` / `pub(pkg)` / private plus Inko-style selective import lists, with generated interface files as the reviewable API gate |
| H5 | FFI and embedding | (a) linear C-ABI handles + handle-table embedding API, (b) platform model (host provides allocator + effect handlers), (c) plain `extern "C"` with manual safety, (d) wasm-first multi-backend | (a) as the substrate plus (b)'s packages-export-handlers variant layered on it: (a) is where docs/en/design/02's linear layer earns its keep, (b) reuses docs/en/design/03's capability substrate without Roc's monolith |
| H6 | Ecosystem and tooling stack | (a) Cargo-style manifest+lock+registry, (b) hash-store native, (c) bun-style zero-config scripts, (d) platform-model embeddable-first | (b) as the core plus (c) as a layered script mode; (a)'s registry deferred to a static JSON index over (b)'s store; (d) is an add-on axis; the mandatory-tooling subsection binds regardless |

## Shared assumptions

Charter constraints binding on every option: interpreted execution with internal bytecode IR (charter req 1), static linear/affine typing (charter req 2), ultra-high performance including guaranteed TCO (charter req 3), sugar ceiling at Kotlin's total surface (charter req 4), immutable-by-default bindings (charter req 5), algebraic effects optional and adopted only if they earn their complexity (charter req 7), decoupling-first RISC/LSP design ethic (charter philosophy).

From the sister catalogs: docs/en/design/02 recommends C1(b) affine core with an opt-in linear `~Copyable` layer and C5(c) linear RAII deinit on the resource layer — H5 option (a) below consumes exactly that substrate; docs/en/design/03 recommends E1d (handler runtime underneath, no surface effect syntax) with an E1c capability-parameter idiom and B5d (union-base error signatures) — H5 option (b) and the H6 mandatory REPL/diagnostics items consume exactly that substrate. Neither is re-decided here; this file only states what each option requires of them.

The runtime baseline comes from research/03 via the sister catalogs: typed register bytecode VM, static per-function frame layout, no import-time execution anywhere in this file's designs (it is a hard output of G3, not an option). The VM's module-format consequences are specified once in G3.5 ("The dsmod contract") and referenced by every other option.

## Author pains mapped to design levers

The charter names four author pains; each maps to specific options below, and every option is scored against this table.

| Pain (source) | Symptom | Design levers in this file |
|---|---|---|
| Nix (UPM: flake `follows`, global `flake.lock` breaking dependents, input-hash store, no FHS, undocumented builtins) | Updating one package locks or breaks the whole system; installing to standard paths needs hand-written shell files | H6(b): per-package dependency locks embedded in the manifest (Zig-style hash cascade, so `follows` cannot exist), output-hash store, optional FHS profile linker, bin-first artifacts (research/06 §2.3, §2.7, §7 combo B) |
| Python modules (research/06 §1.7: import-time execution, runtime-failing cycles, directory-granularity visibility, `__init__` re-export boilerplate) | "Module granularity insufficient, cannot cross-reference, essentially enhanced code copying" | G3(b): pure compile-time module bodies, cycles legal inside a package, files-share-namespace packages, `///|` blocks as the tooling unit (research/06 §1.9-1.11) |
| `#include` (charter taste: hatred of text-inclusion modules) | Headers paste text; no typesafe boundary; API surface not reviewable | G3(b): generated interface files (`ds info` -> `.dsi`) reviewed as diffs, `include` banned from the grammar entirely (research/06 §1.3, §1.9); G4(b): the interface file is the visibility gate |
| LSP-first (charter: IDE-centric author; research/06 §4.1) | Whole-program re-analysis per keystroke; tooling as afterthought | H6 mandatory commitments: compiler-as-query-library from day one, formatter and interface generator as compiler components, structured diagnostics (research/06 §4.1-4.4) |

## G3 — Module system and unit layering

What is decided here: the three conflateable concepts — namespace/visibility unit, compilation unit, distribution unit — get explicit layers (research/06 §1.1); the cycle rules; whether module bodies execute at import time; and what the bytecode module format must support. The author's Python pain decomposes into five checkable mechanisms (research/06 §1.7), of which import-time execution is the deepest: if bodies are pure compile-time declarations, cycles become a name-resolution detail instead of a runtime hazard. G3 also fixes the granularity ladder the author's essay asks for: "compose small modules into bigger ones" without boilerplate interface packages.

### G3.a — Rust-style crate+module tree with use-paths

Take Rust's whole-crate compilation: a crate (here: package) is the compile and distribution unit; `mod` declarations mirror files; `use crate::x::y` paths with `crate::`/`self::`/`super::` prefixes; name resolution is crate-wide and cycle-tolerant, so sibling modules cross-reference freely.

```rust
// G3.a -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
mod transform;          // reads src/transform.ds
mod io;                 // sibling modules may mutually refer: cycles inside the crate are legal
pub(crate) fn helper() {}         // visible package-wide, not to dependents
use crate::transform::normalize;  // explicit paths, re-export via pub use
```

Prior art: Rust crates/modules, crate-wide fixed-point resolution, `pub(crate)` visibility ladder, 2018-edition path grammar and the 2024 edition's reserved-paths patch (research/06 §1.2). Tradeoffs: cycle tolerance and privacy-orthogonal-to-files are real wins (Rust codebases show no Python-style plumbing churn), but `mod` declarations are boilerplate the author's growth path does not want, the path grammar is one of Rust's steeper corners, and the compile unit (whole package) is coarse for a scripting language with multi-GB workloads — every keystroke invalidates at package grain unless the compiler re-derives finer units, which is exactly what G3(b) gets for free from its block model. Forecloses: file-as-module zero-boilerplate growth (`ds run script.ds` then grow in place), block-granular tooling, bun-style script mode in H6(c).

Startup and distribution: bytecode caches naturally at package grain (`<triple>/<pkg-hash>.dsmod`), which is fine for apps and coarse for scripts — a script with one import pays a full-package load; the dsmod contract of G3.5 still applies unchanged.

### G3.b — MoonBit-flavor layering: definition < file < package < module

Four explicit layers: definitions (functions/types, the visibility and identity unit), files (modules: namespace spelling), directories (packages: the compilation unit — files in one package share a namespace with zero intra-package imports, Go-style), and the module (the `ds.pkg` manifest: dependency and distribution unit). Top-level code is organized as order-independent `///|`-separated blocks, so the compiler's unit of relevance is below file level. Packages import packages by path with optional alias; interface files are generated, never hand-written; module bodies are pure compile-time declarations — no import-time execution, period.

```text
// G3.b -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
// repo layout:  ds.pkg (manifest) | pipeline.ds | transform/ | io/ | schema/
///|                                        // block separator; blocks are order-independent
pub fn parse_row(s: String) -> Row | ParseError { ... }   // definition = visibility unit
///|
type Row = { id: Int, tags: List[String] }   // blocks: reordering, moving, deleting are safe ops
// transform/moon.pkg-equivalent (ds.pkg of a subdir): entire content is the import list
// { "import": ["acme/pipeline/schema", "acme/pipeline/io"] }
// `ds info` generates schema/pkg.dsi -- the reviewable API surface:
// package "acme/pipeline/schema"
// pub type Row;  pub fn parse_row(String) -> Row | ParseError
```

Prior art: MoonBit module-package-file organization, `moon.pkg` whose entire content is an import list, generated `.mbti` reviewed as the API-diff artifact ("if nothing in .mbti changes it is typically a safe refactoring" — moonbit-core AGENTS.md), `///|` block model with documented order-independence, target-suffixed files, blackbox/whitebox test split (research/06 §1.9); the granularity ladder and growth path sketch (research/06 §1.10); cycle policy: cycles between files in a package are legal (Rust-style whole-package resolution), cycles between packages are a compile error whose diagnostic suggests "move `X` to core" — the MoonBit `builtin`-trait rule that shared abstractions live in a dependency-free base package (research/06 §1.9, §1.11). Pure compile-time bodies are the one-rule-three-wins rule: (1) intra-package cycles are a resolution detail, answering the Python cross-reference pain; (2) hot reload becomes safe (Erlang model needs no top-level side effects, research/06 §4.5); (3) lazy module-graph loading cuts startup (research/06 §6.3 lever 3) — three designs, one rule. Tradeoffs: whole-package fixpoint name resolution is real compiler work (bounded per package); the four-layer vocabulary needs a half-page of tutorial (within the explainability budget); selective-import ergonomics are G4's job. Forecloses: Unison-style hash-addressed definitions (G3.c), Python-style import-time side effects (intended loss), and per-file independent compilation across package edges (packages are the invalidation unit — which is also why package cycles must be banned: cycles defeat incremental per-package checking, research/06 §4.1).

Startup and distribution: dsmod headers carry the dependency-name list so the loader loads only what `main` reaches; interface files make LSP hover cheap without executing anything; the same package graph compiles to a single embedded-bytecode binary (G3.5).

### G3.c — Unison content-addressed definitions

Hash = code identity: every definition is stored as the hash of its typed syntax tree including dependency hashes; the "codebase" is an append-only CAS; names are metadata; files do not exist.

```text
// G3.c -- illustrative
unique type Row = { id : Int }              // stored as #blake3...; body + deps' hashes in the hash
parse_row : String -> Row | ParseError      // edit the body -> new hash -> dependents rehash transitively
```

Prior art: Unison's content-addressed codebase, conflict-free renames, intrinsic dependency versioning, definition-level merges, safe remote execution by hash (research/06 §1.8; LWN 2023). Tradeoffs: the entire tooling stack becomes bespoke — no plain-text files means grep, git, patch, and text-based LSP are reinvented through a sync protocol; program text cannot be diffed line-wise; the author lives in VSCode and git. Forecloses: the plain-text-file culture outright, plus every file-oriented option in G3 and every text-diff review flow in G4; this is a design axis to borrow from, not an option to adopt. Startup and distribution: perfect caching and trivially verifiable distribution (fetch by hash), the strongest possible story — but G3.5's dsmod achieves 90% of it (content-addressed IR cache, hash-validated) at zero cultural cost, which is the cheap borrow research/06 §1.8 recommends: hash the IR artifacts and package identity, not the definitions.

Verdict — rejected as the module model; retained as the caching and store philosophy (G3.5, H6.b).

### G3.d — TS/ESM-flavor files-as-modules with import/export

Every file is a module with static `import`/`export`; live bindings; depth-first post-order evaluation of the graph; cycles allowed with defined semantics. This is the author's beloved TS+bun model, minus its traps: the checker statically rejects use-before-init across the whole graph, which TS cannot do.

```ts
// G3.d -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
import { normalize, Row } from "./transform.ds"     // files are modules; directories need index files
export function parse_row(s: string): Row | ParseError { ... }
// cycles a<->b are legal: hoisted functions callable across the cycle; TDZ cannot happen
// because the compiler rejects use-before-init -- the ESM trap made a compile error
```

Prior art: ESM static imports, live bindings, TDZ-on-partial-initialization as "the bug that passes every toolchain check", CommonJS snapshot semantics, Deno/bun registry specifiers and import maps (research/06 §1.6); the Python five-mechanism pain shows what execute-on-import does to the same shape (research/06 §1.7). Tradeoffs: minimal vocabulary and the author's daily ergonomics; but directories need `index`-file convention or import maps to be packages (boilerplate creeping back), the compilation unit is the file (fine for scripts, means cross-file type checking needs a project layer bolted on — exactly the TS `tsconfig` situation), and evaluation-order semantics must be fully specified even with pure bodies (hoisting rules). Forecloses: package-as-namespace-with-shared-visibility (each file must re-export to expose a package API — the `__init__.py` chore reborn), MoonBit-grade interface files (a `.d.ts`-style layer would be hand-maintained or tool-magic), and the H6(c) hot-reload story weakens because per-file modules re-evaluate rather than swap versioned package slots.

Startup and distribution: per-file bytecode cache is natural (like `.pyc`), but the loader must reconstruct the import graph at runtime; import maps or a generated manifest substitute for path-walking resolution (research/06 §2.5's Deno lesson: bare-name resolution lives in one small config, not directory walking).

### G3.5 — The dsmod bytecode contract (binding on every option)

Whatever module model wins, the runtime module format must make startup and distribution free. The format needs: a magic header and format version (bump on IR change); a target class byte (bytecode | wasm | native triple); a flags byte (STRIP, FFI-USED, HAS-JIT-METADATA); the module's stable name; a dependency list as (name, content-hash) pairs — the recompile closure hot reload needs; a source hash for PEP 552-style checked-hash validation (hash, not mtime — mtime lies in git checkouts and content-addressed stores); and ULEB-prefixed, length-delimited, relocation-light sections (bytecode prototypes, constants, type metadata, effect rows, strippable debug spans) (research/06 §6.2, LuaJIT `lj_bcdump.h` discipline).

```text
dsmod (proposed):
  bytes 0..3  magic "\0DSM"      byte 4 format version      byte 5 target class
  byte 6      flags (STRIP | FFI-USED | HAS-JIT-METADATA)
  uleb        module name        uleb deps: [(name, content-hash)]...
  uleb        source hash (blake3-128)                      # PEP 552 checked-hash mode
  sections    blocks | constants | type metadata | effect rows | debug spans (strippable)
```

Startup levers in leverage order (research/06 §6.3): mmap the cache hit path (`$DS_CACHE/ir/<triple>/<blake3-of-source+deps>.dsmod`; Java AppCDS proves mmap of prelinked metadata is the win); lazy loading driven by the header's dep list; no import-time execution (the G3.b rule); embedded-bytecode single binaries via `ds build --exe` — bytecode is already the internal IR, so Bun's measured 1.38x startup gain from `--compile --bytecode` over bundle-only is the floor, not the ceiling (research/06 §6.1). Distribution consequence: every option ships `ds run` (cache), `ds build --exe` (single binary embedding the dependency closure + VM), and wasm/component targets (H5.d); Windows gets scoop-style shims, not symlinks (research/06 §6.4).

G3 comparison:

| Dimension | G3.a Rust tree | G3.b MoonBit layers | G3.c Unison | G3.d ESM |
|---|---|---|---|---|
| Boilerplate to grow script -> package | mod decls + paths | none (same file, then a dir) | n/a (no files) | index files / import maps |
| Cycle story | legal in package | legal in package, banned across | moot | legal, eval-order hazards spec'd |
| Tooling granularity | package | block (`///|`) | definition | file |
| Interface artifacts | none (rustdoc) | generated `.dsi` | types in hash | `.d.ts`-style, hand or tool |
| Compile-time cycle errors | cross-package only | cross-package only | n/a | none (runtime eval order) |
| Import-time execution | none | none | none | none (checked use-before-init) |
| Forecloses | script-first growth | hash-addressed defs | text culture | package-level API gate |

**Recommendation — G3(b)**: the MoonBit-flavor ladder is the only option that simultaneously answers the Python pain (cross-references without boilerplate), the `#include` pain (generated interfaces), the scripting growth path (zero manifest to start), and the hot-reload/startup contract (pure bodies + dep lists in dsmod headers); take G3.a's cycle tolerance as its resolution strategy and G3.5's format as binding output.

## G4 — Visibility model

What is decided here: who can see which definitions, and how that is spelled. Visibility interacts with G3's cycle policy (intra-package cycles make file-scoped visibility incoherent — all files in a package resolve as one namespace, so visibility attaches to definitions, not files) and with interface files (the `.dsi` is where "what dependents see" becomes a reviewable diff — research/06 §1.9, §4.2). The author's Python pain includes "`_`-underscore privacy by convention" (research/06 §1.7 point 3), which rules out pure-convention answers.

### G4.a — pub/priv binary

Rust's model: everything private by default; `pub` opens it to all dependents; nothing in between (optionally `pub(crate)` as a third mark).

```rust
// G4.a -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
fn shuffle(buf: Buf) -> Buf { ... }        // package-private by default
pub fn parse_row(s: String) -> Row | ParseError { ... }   // visible to all dependents
pub(crate) fn bench_shim() { ... }         // visible package-wide only
```

Prior art: Rust `pub`/`pub(crate)`/`pub(super)`/`pub(in path)` ladder, privacy orthogonal to files (research/06 §1.2). Tradeoffs: the simplest grammar and the least to explain; but the binary choice conflates "visible to my one dependent" with "visible to the world" — mid-sized libraries either expose internals or force `pub` + doc-hiding, and there is no way to give one sibling package access without opening the API globally. Forecloses: layered API surfaces (internal packages shared between two first-party packages), symbol-level exposure control, and using the interface file as a per-dependent gate. Startup/distribution: irrelevant to runtime; visibility is compile-time only.

### G4.b — Layered visibility with selective exports

Visibility rides the G3(b) ladder: definitions are private by default; `pub(pkg)` opens them package-wide (visible to files in the same package and to the package's own tests); `pub` opens them module-wide (to dependents) and puts them in the generated `.dsi`; imports are symbol-selective with renaming, so a package can re-export a curated surface; conditional imports select per target.

```text
// G4.b -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
fn shuffle(buf: Buf) -> Buf { ... }                 // private to this file's package internals
pub(pkg) fn bench_shim() { ... }                    // package-wide, never in .dsi
pub fn parse_row(s: String) -> Row | ParseError { ... }   // in .dsi; the only depedent-visible fn
// dependent side -- selective import with alias/rename (Inko grammar, research/06 §1.11):
import acme/pipeline/schema (Row, parse_row)
import acme/pipeline/io as pio                      // namespace alias: pio.read_file(...)
import acme/pipeline/fs if os == "windows"          // conditional import
pub import acme/pipeline/api (parse_row)            // re-export as the package's own API
```

Prior art: Inko's `import mod1.mod2 (A, B as C)` symbol-selective grammar with `import foo if linux and amd64` (research/06 §1.11 import-grammar table, verbatim from inko syntax.md); MoonBit's `.mbti` sections listing exactly the `pub` surface and the AGENTS.md review flow (research/06 §1.9); the definition < file < package < module ladder (research/06 §1.10); Ada's child-package visibility flowing down a hierarchy as the conceptual ancestor (research/06 §1.3). Tradeoffs: three spellings instead of two and one more grammar rule (selective imports); in exchange, moving code between files is refactor-neutral (visibility is per-definition), internal packages are expressible without opening the public API, and the `.dsi` diff is exactly the API-review artifact the author's `#include` pain asks for. Interaction with cycles: intra-package cycles are unaffected (everything `pub(pkg)` or below resolves within one namespace); across packages the `.dsi` is the only gate, so package DAG + interface files = the entire visibility contract. Forecloses: nothing material; the `pub(crate)`-style path-scoped ladder (`pub(in acme/pipeline`) is left out deliberately to stay under the sugar ceiling.

Startup/distribution: none at runtime; the `.dsi` doubles as the LSP's hover/signature source and the incremental-compilation boundary (research/06 §4.2).

### G4.c — Capability/capsule-based visibility

Typestate-flavor visibility: a module exports capsule types whose members are accessible only while a capability token (a linear value minted on opening the capsule) is in scope; visibility becomes a runtime-checked-ish, statically-tracked resource.

```text
// G4.c -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
capsule Db { fn query(self, q: String) -> Rows }    // members gated by Cap[Db]
fn with_db[T](cfg: Cfg, body: (cap: Cap[Db]) -> T) -> T   // minting scoped to the callback
```

Prior art: capability-parameter idiom in docs/en/design/03 (E1c); object-capability literature via the Roc platform inversion (research/06 §5.4). Tradeoffs: revocable, scoped access is genuinely powerful for embedding (H5) — but as a *visibility* mechanism it makes every import a value to thread, costs a linear layer over the whole name-resolution system, and fails the sugar ceiling (charter req 4) and the explainability budget: "why can I not call this function" becomes a type-unification question. Forecloses: cheap static name resolution, simple LSP hover (visibility queries become type queries), and it duplicates what E1c already provides for effects at far lower cost.

Verdict — likely-rejected under the sugar ceiling; kept for the record and as a library idiom (capability parameters), not a visibility system.

### G4.d — Go convention-based exported-identifier heuristic

Capitalization is the visibility marker: `Parse_row` is exported, `parse_row` is not; no keywords exist.

```go
// G4.d -- illustrative
func Parse_row(s String) Row | ParseError { ... }   // capital = exported
func shuffle(buf Buf) Buf { ... }                   // lower = package-internal
```

Prior art: Go's exportedness by capitalization, files-in-a-package sharing a namespace, cycle ban as design-smell detector (research/06 §1.5). Tradeoffs: zero grammar and zero ceremony; but the API is spelled into identifiers (renaming for style changes the API), there is no package-internal-vs-public intermediate, tooling cannot filter visibility without case conventions, and the author's stated taste rejects convention-enforced privacy (Python `_`-underscore, research/06 §1.7 point 3). Forecloses: per-dependent exposure, interface-file filtering by anything other than case, and the reviewable-diff property is weakened (every rename is an API diff). Startup/distribution: none.

G4 comparison:

| Dimension | G4.a binary | G4.b layered | G4.c capsule | G4.d convention |
|---|---|---|---|---|
| Grammar cost | 1 keyword | 2 spellings + selective import | capsule + token types | none |
| Per-dependent exposure | no | via .dsi + curated re-exports | per-token scope | no |
| Interacts with intra-pkg cycles | fine | fine (per-definition visibility) | breaks (tokens must thread) | fine |
| Interface-file story | weak (pub filter only) | strong (the gate itself) | n/a | weak (case filter) |
| Sugar-ceiling risk | none | low | high | none |
| Author-pain fit (Python `_` privacy) | good | best | overkill | rejected by taste |

**Recommendation — G4(b)**: layered visibility with selective exports, taking G4(a)'s private-by-default as the base case; the interface file is the visibility gate and the review artifact, which is the exact mechanism the `#include` pain demands and the cheapest option that still expresses internal packages.

## H5 — FFI and embedding

What is decided here: how DreamScript calls C, how hosts embed DreamScript, and how effects and linearity behave at the boundary. Research/06 §5 establishes the substrate: C ABI as the universal low level, handle-based (not pointer-based) GC interop learned from Lua's stack+registry API and the Python C API's owned/borrowed/stolen reference trap, WIT/component-model as the typed wasm boundary, and Roc's platform inversion as the conceptual fit with the effects design. The two load-bearing interactions: option (a) requires docs/en/design/02's linear layer (C1(b) affine core with `~Copyable`, C5(c) linear deinit) — a linear handle consumed exactly once makes double-free a type error; option (b) requires docs/en/design/03's E1d handler runtime and E1c capability idiom — the platform's effect row is the contract.

### H5.a — Linear-typed C ABI handles + handle-table embedding API

Foreign resources are opaque linear handles; the C layer uses a documented `repr(c)` layout discipline; the embedding API exposes no raw pointers — a `Value` is an index into the VM's object table (Lua-registry pattern), borrows are call-scoped `&Value`, ownership transfers are linear `Value`, the context is an explicit `ctx: &mut Vm` (two embedders = two VMs, killing the GIL-by-design problem), and FFI-visible memory is pinned so the GC never moves it (research/06 §5.1-5.2).

```text
// H5.a -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
type c["sqlite3"]                                        // opaque, layout unknown, ~Copyable => linear
extern fn sqlite3_open(path: *const c.Char) -> (c["sqlite3"] | c.Int)   // out-param flattened
extern fn sqlite3_close(db: c["sqlite3"]) -> c.Int       // consumes db: exactly-once, checked
// let db = sqlite3_open(p); sqlite3_close(db); sqlite3_close(db)  -- compile error (2nd use of moved db)
// host embedding side (C API):
//   ds_push_int(L, 42); ds_call(L, "process", 1, 1);      // explicit stack, Lua-shaped
//   int r = ds_ref(L, -1);                                // registry ref = GC-scanned handle
```

Prior art: Lua's value stack + registry refs + `luaL_ref` handles, callbacks on VM-managed stacks only; the Python C API's owned/borrowed/stolen triad and endemic refcount bugs as the cautionary tale; Rust `repr(C)`/`extern "C"` and Zig `export fn`/`@cImport` for the ABI layer; `ds bind header.h` emitting typed externs in the Zig `translate-c`/bindgen UX (research/06 §5.1-5.2, tables therein). The one-line comparison that motivates the design: Lua proves minimal handles are enough, Python proves pointer-plus-convention is where extension bugs live — and DreamScript has a type system that can make the convention a type error. Tradeoffs: every C function that returns an owning pointer must be wrapped so the handle type is honest (real but mechanical binding work — `ds bind` amortizes it); callbacks must run on VM-managed stacks; linear discipline at the prompt means users must consume or explicitly drop handles (the REPL design in H6-mandatory handles this). What it forecloses: casual pointer-passing to C (by design — that is the point); also forecloses nothing about H5(b)/(d), which layer on top. Requires: docs/en/design/02 C1(b) affine + `~Copyable` and C5(c) deinit hooks — without the linear layer this option collapses into H5(c).

Startup and distribution: the dsmod FFI-USED flag declares dlopen needs at load time; pinned foreign objects require a GC design note (already assumed pinned-Foreign in docs/en/design/02); native targets need zig-cc-as-linker posture (research/06 §3.3).

### H5.b — Platform model: host provides allocator + effect handlers

Roc's inversion, decomposed per the RISC ethic: applications are pure; a platform supplies the effectful surface (I/O, allocation, host functions); the checker verifies user code's effect row is a subset of the platform's declared row. The DreamScript variant avoids Roc's monolith: platforms are ordinary packages exporting effect handlers plus optional host glue, selectable per build target — the default platform is the `ds` CLI itself providing the std effect row (research/06 §5.4).

```text
// H5.b -- illustrative; surface syntax decided in docs/en/design/04-syntax.md
platform "std"   provides <console, fsys, net, time>     // the ds CLI provides these handlers
platform "web"   provides <console, fetch>               // wasm host satisfies via WIT imports
platform "embed" provides <>                              // host calls in; provides nothing
fn main() -> { console.print("hi") }   // checker: <console> is a subset of the platform's row
// effect rows render to WIT for wasm components (research/06 §5.3):
//   world pipeline { import host-io: interface { log: func(msg: string); ... }
//                   export run: func(input: string) -> result<string, string>; }
```

Prior art: Roc's app/platform split, platform-provides-allocators, glue + surgical linker, single-platform limitation, platform-versioning complicating packages (research/06 §5.4, §2.6); WIT worlds and wasmtime `bindgen!` (research/06 §5.3); Koka's per-backend inline externs as the multi-target glue spelling (research/06 §5.5). Tradeoffs: clean effect audit and embeddability, and the packages-as-platforms variant keeps the scripting story intact (default platform needs zero ceremony) — but the effect-row-to-WIT lowering and glue generator are real compiler work, platform versioning is a package-model complication (the unit of compatibility becomes platform version, research/06 §2.6), and a rich fixed stdlib owning effects is foreclosed (the stdlib becomes one platform among several — a cultural commitment). Requires: docs/en/design/03 E1d handler runtime (effects are real under the hood even with deferred surface syntax) and E1c capability idiom for the audit. Forecloses: Haskell-style total-purity guarantees (effects are always platform-relative), and a single fixed effect surface. Startup/distribution: "embed" platforms make libds-sized embedding the norm (`libds.so` Lua-sized); wasm components are a first-class distribution target; platforms themselves distribute bin-first through H6(b)'s store.

### H5.c — Plain extern "C" functions with manual safety

The Zig/Rust 2015 posture: typed extern declarations, C layout rules documented, resource discipline by convention and code review.

```c
// H5.c -- illustrative
extern fn sqlite3_open(path: *const c.Char, out: **c["sqlite3"]) -> c.Int
extern fn sqlite3_close(db: *mut c["sqlite3"]) -> c.Int
// double-close is a runtime bug, exactly as in C; leak detection is a lint, not a type fact
```

Prior art: Rust `extern` blocks (unsafe boundary made explicit), Zig `@cImport`/`extern "c"`, the documented `repr(c)` layout rules (research/06 §5.1). Tradeoffs: near-zero compiler work and the most familiar surface for C programmers; but it throws away the one thing the charter's linear layer uniquely buys — "must free exactly once" becomes a lint instead of a type error, and the embedding API inherits the Python-style convention swamp (research/06 §5.2 table). Forecloses: static resource guarantees at the boundary, and it demotes docs/en/design/02's FFI story to decoration (C1's linear layer would then exist only for internal resources, weakening the payoff research/06 §5.1 claims: "the FFI layer is where linearity earns its keep"). Startup/distribution: identical to H5.a (both are native C ABI; only the safety discipline differs).

### H5.d — wasm-first with JS/native as secondary backends

Treat wasm (and wasm components) as the primary compilation target — write once, run in browsers, CI, and embedders — with JS and native as suffix-file secondary backends, MoonBit-style.

```text
// H5.d -- illustrative
// foo_wasm.ds / foo_js.ds / foo.ds          -- backend selection by filename suffix, no preprocessor
// ds build --target wasm-gc                  -- one tree, three backends (MoonBit template)
```

Prior art: MoonBit shipping wasm/js/native from one source tree with target-suffixed files and `moon build --target wasm|wasm-gc|js|native` (research/06 §1.9, §5.3, §6.4); Component Model/WIT typed boundaries (research/06 §5.3). Tradeoffs: universal distribution and sandboxing for free, cross-"compilation" is trivial (the artifact is wasm); but native performance — the charter's whole point — becomes the secondary tier, wasm GC availability constrains the value representation (typed registers and unboxed ADTs must map onto wasm GC types or boxed fallbacks, risking the boxing overhead term research/03 warns about), and every FFI path needs a wasm variant of its glue. Forecloses: treating the native bytecode VM as the reference semantics (it would drift to the wasm tier); also constrains docs/en/design/02/03's zero-cost layouts if wasm is truly first. Startup/distribution: fastest web story of the four (wasm streaming instantiation); native story identical to H5.a/H5.c.

H5 comparison:

| Dimension | H5.a linear handles | H5.b platform | H5.c manual extern | H5.d wasm-first |
|---|---|---|---|---|
| Compiler work | binding + handle table | glue gen + WIT lowering | minimal | backend + glue per target |
| Double-free | type error | n/a (effects, not resources) | runtime bug | per-backend glue quality |
| Requires | 02 C1(b)+C5(c) | 03 E1d+E1c | nothing | value-repr compromises |
| Embeddability | handle-table C API | best (embed platform) | C API, convention-swamp risk | wasm hosts only |
| Perf at boundary | pinned + unboxed | same as (a) underneath | same | wasm GC constraints |

**Recommendation — H5(a) + (b)**: adopt (a) as the substrate (it is the only option where docs/en/design/02's linear layer pays rent at the boundary) and layer (b)'s packages-export-handlers platform model on top for effects and embedding — this is research/06 §7 combo D minus Roc's monolith; (c) remains the documented escape hatch inside (a) (extern decls are how `ds bind` output looks before handle wrapping); (d) is a distribution target, not an FFI architecture.

## H6 — Ecosystem and tooling stack

What is decided here: the package/build/distribution stack (the four combos from research/06 §7), plus a set of mandatory tooling commitments that bind under every option. Constraint from research/06 §3: the build layer must be one binary with a declarative default plus a same-language escape hatch (`build.ds` run in the VM the compiler already embeds — Zig must compile a runner; an interpreter just evaluates), and must not become a second DSL, a Bazel, or a feature-flag system that forks the typechecked surface.

### H6.a — Cargo-style manifest + lock + registry

`ds.toml` with name/version/semver-range deps, a generated `ds.lock`, `src/` layout, `ds add/build/test/fmt/info/publish`, a sparse-index registry (Cargo's model), optional features.

```toml
# H6.a -- illustrative
[package]
name = "pipeline"
version = "0.4.0"
[dependencies]
csv = { version = "0.7", features = ["simd"] }   # range + lockfile; features unified across graph
```

Prior art: Cargo manifest/lock/resolver, feature unification across the dependency graph (cargo#10489: "my dependency compiled serde with features I never asked for"), PubGrub resolver, multi-major coexistence (research/06 §2.1); mooncakes.io as the proof a young language can build a registry in ~2 years (~3.5k modules, research/06 §1.9, §2.6). Tradeoffs: the most familiar shape (author knows Cargo) and a conventional registry from day one; but the registry is the long pole of infrastructure, lockfile/manifest drift is a permanent tax, features reintroduce per-configuration typechecking that the module system must handle (research/06 §2.1 recommends against features in a bytecode language — MoonBit uses filename suffixes, Inko conditional imports), and a per-ecosystem Cargo clone forecloses the author's UPM ambitions by construction. Forecloses: hash-as-lock simplicity, the output-hash store as a user-visible concept (demoted to internal cache detail), H6(b)'s whole design. Startup/distribution: unchanged from G3.5 (this axis is orthogonal to startup); single binaries and cache work identically.

### H6.b — Hash-store native (the author's UPM, grounded)

Deps declared as `name = { ver, hash }` where hash is blake3 over the package content *including its dependency hashes* (Zig's cascade design, so identity flows transitively and no lockfile exists — the manifest is the lock); MVS resolution (Inko/Go: pick the max of declared minima, no solver, "never a version you never tested against"); a global content-addressed store keyed by **output** hash; per-project materialization by hardlinks/symlinks (pnpm mechanics); bin-first artifacts per target triple verified against store hashes; an optional FHS profile linker (`ds link`) using scoop shims on Windows and profile symlinks on NixOS; the registry deferred to a static JSON index over the store (Cargo sparse-index shape) when ecosystem size demands it.

```text
# H6.b -- illustrative (ds.pkg; one file, no lockfile, manifest is the lock)
name    = "pipeline"
version = "0.4.0"
[deps]                          # MVS minima; each pin carries a content hash
std     = { ver = "1.2", hash = "blake3:9f2c..." }
csv     = { ver = "0.7", hash = "blake3:1a88..." }   # csv 0.7 coexists with another pkg's 0.6
[artifacts]                     # bin-first: prebuilt per triple, hash-verified
x86_64-pc-windows-msvc = { url = ".../pipeline-0.4.0-x86_64-pc-windows-msvc.dsmod", hash = "blake3:77e0..." }
[build]                         # optional escape hatch, run by the embedded VM
script = "build.ds"             # sees #OUT#, #TMP#, #ARCH# (UPM template vars)
```

Prior art: Zig `build.zig.zon` URL+hash with dependency-hash cascade ("manifest is the lockfile", research/06 §2.3); Inko `require URL VERSION CHECKSUM` with MVS and multi-major coexistence (research/06 §2.4); pnpm CAS + hardlink materialization + global virtual store (research/06 §2.2); Nix profiles and scoop shims for FHS linking (research/06 §2.7, §6.4); Bazel's output-keyed action caches (research/06 §2.7). Evaluating the author's UPM concretely against what already exists (research/06 §2.7 table, condensed):

| UPM element | Already proven by | Remaining gap |
|---|---|---|
| Per-package dep lock (hash+version), update-never-breaks-upstream | Inko manifest-as-lock; Zig dep-hash cascade; Cargo.lock | none; use blake3 not Inko's SHA1; ship `ds update` to soften the manual-bump UX wart |
| Output-hash-addressed store | pnpm (files), Zig (tarballs), Nix FOD, Bazel (action outputs) | input-hash != output-hash (the author's UPM point 6 and Nix's ca-derivations lesson): key by output content hash, fall back to input hash for non-reproducible builds; end-to-end output-hashing is still early-stage elsewhere |
| Multi-version coexistence | Nix store, Zig cache, Cargo majors, pnpm | none |
| FHS profile linking | Nix user profiles, buildEnv; scoop shims (Windows); pnpm virtual store | Windows: shims not symlinks; hardlinks cannot cross volumes nor link dirs — file-level hardlinks (pnpm) or symlinks; the suffix-on-conflict rule is workable as-is |
| Bin-first in one manifest | AUR `-bin`, cargo-binstall-style declared artifacts | trust model: per-platform checksums/signatures; the author's "bin and unbin in one manifest" is cleaner than Nix's split cache |
| Any-language build scripts in phases | PKGBUILD, Homebrew, Nix builders, Bazel genrules | hermeticity: pin script tool deps too (covered by the dep lock); DreamScript's own `build.ds` in the embedded VM is the default |
| P2P torrent distribution with two hashes | IPFS/CAS, nix-casync, Nix-P2P research | economically fragile long-tail; viable as an optional accelerator since CAS chunks are torrent-friendly; explicitly non-core for v1 |

Tradeoffs: store + materializer (~pnpm-lite) and the hash-cascade scheme are the implementation cost; range-solver ergonomics are gone (MVS-max resolution, accepted — same as Go/Inko); security bumps are explicit manifest edits (Inko documents and accepts this); and the UPM's universal cross-language ambitions stay out of the language's critical path (Nix's lesson: universal build description is the hard part — the language owns manifest, MVS, store-path convention, and the cross-language UPM can grow on the same store later, research/06 §2.7 verdict). Forecloses: auto-upgrade culture, and a lockfile's "what-if" resolution queries (reproducibility is by construction instead). Startup/distribution: strongest of the four — the store doubles as the bytecode cache (the artifacts *are* dsmod files keyed by content hash), bin-first makes `ds build --exe` artifacts first-class store citizens, and PEP-552-style hash validation is native to the design (research/06 §6.2-6.3).

### H6.c — bun-style zero-config instant scripts

`ds run script.ds` with no manifest; dependencies auto-resolve from a default index on first import and are recorded into a generated manifest on `ds save`; module cycles allowed inside the implicit package; hot reload built in; the store backs everything so there is no `node_modules`.

```text
# H6.c -- illustrative
$ ds run pipeline.ds            # zero config; imports auto-resolve; cache is warm via the store
$ ds save                       # freeze what the script used into a generated ds.pkg
import "@std/csv"               # std = bare specifier, no URLs (Deno's import-map lesson, no retreat)
```

Prior art: bun/deno zero-config run + save-to-manifest pattern, bare specifiers and import maps (research/06 §1.6, §2.5, §7 combo C); the growth path sketch script -> package -> module (research/06 §1.10). Tradeoffs: the best scripting ergonomics of the four and a direct hit on the TS+bun ergonomics the author loves; but the two-slot runtime module format (hot reload) must be in the VM from day one (retrofitting is the mistake to avoid, research/06 §7 combo C), and cycle-tolerant whole-package resolution is real compiler work (bounded per package). Forecloses: import-time side effects (intended), and nothing else — this mode layers on H6(b)'s store and G3(b)'s modules rather than competing with them. Startup/distribution: sub-10ms cold target via mmap'd dsmod cache (research/06 §6.3 budget); scripts that outgrow themselves gain a manifest without losing their file.

### H6.d — Platform-model embeddable-first

The package/build story organizes around embedding: the VM is a C library (`libds.so`), apps are hosted inside other programs, platforms (H5.b) are the distribution unit of the effectful surface, and the general-purpose registry is deferred indefinitely (Roc's actual trajectory, research/06 §2.6).

```text
# H6.d -- illustrative
app [main] { pf: platform "acme/web-runtime" }   # Roc-style app manifest (simplified)
// the host embeds libds.so; scripts are store-fetched dsmod blobs; no registry needed
```

Prior art: Roc's platform-centric packaging and its repeatedly-deferred general package manager (research/06 §2.6, §7 combo D); Lua's ubiquity as the embeddable-scripting existence proof. Tradeoffs: makes "scripting target the author cares about" (embedding in Rust/JS/C hosts) the organizing principle and composes with linear handles (H5.a) for exactly-once host resources; but the general library ecosystem starves while the platform story matures, and "platform version" replaces "library semver" as the compatibility unit, which is a poor fit for the big-data scripting domain. Forecloses: a registry-first community growth path; as a *standalone* answer it is too narrow, which is why research/06 §7 scores D as an add-on axis. Startup/distribution: embedding is the product (host binary carries the VM); the store is still needed for platform/script distribution — D does not remove H6.b, it reprioritizes it.

H6 comparison:

| Axis | H6.a Cargo-style | H6.b hash-store | H6.c bun-scripts | H6.d platform-first |
|---|---|---|---|---|
| Implement cost | medium (registry heavy) | medium (store+materializer) | low-medium (runtime slots) | medium (embedding API) |
| Registry needed day 1 | yes | no (deferred static index) | no | no |
| Author UPM fit | weak | strong | neutral | neutral |
| Nix pain addressed | partial | strong (hash cascade, bin-first, FHS) | neutral | neutral |
| Python pain addressed | strong | strong | strong | neutral |
| Hot reload | neutral | strong (cache doubles) | strong (built-in) | strong (handler model) |
| Forecloses | hash-store UPM | range-solver ergonomics | import-time effects | registry-first growth |

**Recommendation — H6(b) + (c)**: hash-store native as the core with bun-style script mode layered on (B+C of research/06 §7's recommended synthesis); (a)'s registry returns later as a static JSON index over (b)'s content-addressed artifacts; (d) is delivered anyway by H5(b)'s platform-as-package design and is not a separate ecosystem decision.

### H6-mandatory — Tooling commitments binding under every option

These five are commitments, not options: research/06 §4 shows each is load-bearing for the charter's LSP-first, IDE-centric author, and each has exactly two credible implementation strategies.

**1. Compiler-as-query-library from day one (salsa-style).** The compiler must be a library (`ds-check`) whose API is memoized queries — parse, block_index, resolve, type_of, effects_of, linearity_check, lower — so LSP, REPL, and hot reload are the same consumer with different drivers; Koka is the existence proof of one tree hosting compiler, interpreter, and LSP (research/06 §4.1). Implementation options: (i) red-green incremental query database (rust-analyzer/salsa lineage; durable incrementality; invalidation at `///|` block grain) — principled and proven, cost is building the query infra early; (ii) tsserver-style one-program-per-project with a document registry reusing ASTs across open files, features computed on demand — faster to ship, historically less principled, and degrades on large graphs. Both require package-granularity invalidation, which is the second reason package cycles are banned (G3.b).

**2. Formatter as compiler component.** `ds fmt` with zero options (gofmt/zig fmt/moon fmt all prove zero-config ends style debates; prettier/rustfmt/ocamlformat prove options drift, research/06 §4.2 table). Implementation options: (i) single canonical pretty-printer over the typed AST sharing the compiler's parser (rustc-adjacent but in-tree); (ii) block-aware formatter exploiting the `///|` model (moon fmt): format blocks independently, preserve order-independence, enable partial-file formatting that is also the LSP's on-type-formatting path. Option (ii) is only available if G3(b)'s block model is chosen — a direct dependency of this commitment on G3.

**3. rustc-grade diagnostics with two-span linear errors.** Structured diagnostics (structured suggestions as machine-readable insert/replace ranges, `--error-format=json` for LSP/CI), and because the language is linear, a dedicated diagnostic class rendering "moved here / used again here" two-span output — rustc's E0382 move/use pair minus borrow complexity (research/06 §4.4). Implementation options: (i) adopt `codespan-reporting`/`ariadne` for rendering with in-house structured diagnostic types — fast to excellent output, rendering lagging rustc features; (ii) in-house derive-based `Diagnostic`/`Subdiagnostic` model (rustc-dev-guide lineage) — full control (structured suggestions, JSON, two-span grouping), more compiler-team work. Either way spans must survive the IR boundary (computed at typed-AST level, rendered against source).

```text
error[DS2001]: value `h` was consumed here
  --> src/pipeline.ds:41:15
   |  41 |  let rows = Csv.read(h)     ^ `h` moved (File is ~Copyable)
   |  42 |  Log.write(h.path)          used again here after move
help: clone the path before the move            (suggestion emitted as JSON too)
```

**4. REPL with linear session context.** The REPL prints inferred type *and effect row* (Koka's tour), runs interpreted over the same bytecode IR with a session heap, and solves the one real novelty: linear values at the prompt cannot be silently dropped (research/06 §4.3). Implementation options: (i) session-owned linear context with explicit `:drop` and `:reset` purging — the research recommendation; values persist across prompts and are typechecked when reused; (ii) implicit-consume prompt (display then free, like an auto-`consume` on the prompt variable) with `:hold` to retain — friendlier, but loses multi-line resource pipelines. Option (i) subsumes (ii) for display purposes by auto-consuming only the displayed value; both need docs/en/design/03's E1d handler runtime so effects at the prompt are handled by printing.

```text
$ ds repl
> let h = File.open("data.csv")     # h : File -- owned by session context
> let rows = Csv.read(h)            # h consumed exactly once; recheck per prompt
> :drop rows                        # explicit; or auto-dropped at :reset
```

**5. Erlang two-version code slots for hot reload.** The runtime holds per-module versioned slots (current + old, at most two live); `ds dev --hot` recompiles changed blocks, swaps the slot, and old-version frames retire at their next cross-module call (fully-qualified calls are every cross-module call in DreamScript, so Erlang's requirement is trivially met); heap values are type-checked against new defs (Mun-style struct diff, but on a GC heap — no ABI, hence nearly free in this VM); effect handlers capture the code version they were installed with (research/06 §4.5). No import-time execution (G3.b) is what makes this sound. Implementation options: (i) whole-module two-slot swap with frame aging — the near-free baseline, sufficient for script-scale reload; (ii) Julia-Revise-style method-instance invalidation — finer-grained, worth growing into when the JIT tier lands (world-age semantics for running tasks). Requirements on dsmod: stable module names, dependency lists in headers (already in G3.5), versioned slots.

**Startup/distribution note for the commitments:** the query-based compiler is also what makes the G3.5 cache honest (incremental lowering keyed by block/content hashes), the formatter and `.dsi` generator share one AST (no second parser), and the two-slot runtime is the same module loader the startup path uses — none of the five adds a second implementation of anything.

## Coherent combos

Three named packages of choices, each with a manifest, an import block, and an FFI binding, as the author would actually write them. The recommended one is Hash-core.

### Combo 1 — "Hash-core" (G3b + G4b + H5a + H5b + H6b + H6c layered)

```text
// ds.pkg -- manifest is the lock; no lockfile anywhere
name = "pipeline";  version = "0.4.0"
[deps]  std = { ver = "1.2", hash = "blake3:9f2c..." }  csv = { ver = "0.7", hash = "blake3:1a88..." }
[artifacts]  x86_64-pc-windows-msvc = { url = ".../pipeline.dsmod", hash = "blake3:77e0..." }

// pipeline.ds -- imports (selective, aliased, conditional) over a package-local cycle
import @std/csv (Reader)                       // namespace alias, MoonBit-style
import acme/pipeline/schema (Row, parse_row)   // selective, from the generated .dsi surface
import acme/pipeline/fs if os == "windows"

///|  // FFI: linear handle, consumed exactly once (H5.a on 02's C1.b layer)
extern fn sqlite3_open(path: *const c.Char) -> (c["sqlite3"] | c.Int)
extern fn sqlite3_close(db: c["sqlite3"]) -> c.Int      // double-close: compile error
```

Verdict: recommended — every named pain is hit directly (Nix: hash-cascade locks without lockfiles, bin-first, FHS linker; Python: intra-package cycles, pure bodies, block-granular tooling; `#include`: generated `.dsi` reviewed as diffs; LSP-first: compiler-as-query-library shared by LSP/REPL/hot reload), startup is the mmap'd-store fast path, and the bun-script mode is a zero-cost layer on the same store (research/06 §7 synthesis B+C+D).

Implications for docs/en/design/02 and 03: C1(b) affine + `~Copyable` and C5(c) linear deinit become mandatory (H5.a's handles are the payoff), C2's parameter modes carry FFI borrows; E1d's handler runtime plus E1c capability idiom become the effect substrate (platform rows, REPL effect printing, B5d error unions flow through `.dsi` files). None of these are new requirements — they are the catalogs' existing recommendations, now load-bearing.

### Combo 2 — "Cargo-classic" (G3a + G4a + H5c + H6a)

```toml
# ds.toml + generated ds.lock; src/ layout; sparse-index registry
[package]  name = "pipeline"  version = "0.4.0"
[dependencies]  csv = { version = "0.7", features = ["simd"] }
```

```rust
// src/lib.ds -- mod tree, pub/priv, manual FFI discipline
mod transform;  mod io;
pub use crate::transform::parse_row;
extern fn sqlite3_open(path: *const c.Char, out: **c["sqlite3"]) -> c.Int   // close twice = bug
```

Verdict: the familiar fallback — best conventional-tooling familiarity (author knows Cargo) and a registry from day one, but it forecloses the UPM, carries feature-fork and lockfile-drift taxes, leaves the Nix pain unaddressed, and demotes the linear layer's FFI payoff (H5.c makes "must free exactly once" a lint).

Implications for docs/en/design/02 and 03: 02's FFI-motivated parts of C1/C5 lose their boundary payoff (linearity remains for internal resources only); 03 is nearly unaffected — E1d/E1c are only needed by the mandatory REPL/diagnostics commitments, not by the combo.

### Combo 3 — "Platform-embeddable" (G3b + G4b + H5b + H6d, with H6b's store underneath)

```text
// ds.pkg (app): platform replaces a dependency list as the organizing entry
app [main] { pf: platform "acme/web-runtime@0.9" }      // platform version = compatibility unit

// main.ds -- effects arrive from the platform's declared row; checker audits the subset
fn main() -> { let rows = fetch_csv("in.csv"); console.print(sum(rows)) }
// binding story: same linear handles for native hosts; WIT world for wasm hosts
extern fn host_read_file(path: *const c.Char) -> (c.Bytes | c.Int)   // native glue (H5.a handles)
// world pipeline { import host-io: interface { read-file: func(path: string) -> ...; } }  -- wasm
```

Verdict: the right *add-on axis*, not a standalone answer — embedding in Rust/JS/C hosts becomes the product and effects get a typed contract, but a registry-first library ecosystem starves (Roc's documented trajectory) and the big-data scripting domain still needs H6.b's store, so D without B is incomplete (research/06 §7 combo D).

Implications for docs/en/design/02 and 03: E1d's handler runtime upgrades from recommended to mandatory (platform rows are the contract), E5's resource/cancellation points interact with linear host handles (C5.c deinit runs under handler scope), and the stdlib-owns-no-effects cultural commitment lands here.

## Source notes

Primary evidence: `docs/en/research/06-modules-tooling-ffi.md` sections 1-7 (module vocabulary and cycle analysis §1.1-1.11; package managers and the UPM evaluation §2.1-2.7; build systems §3.1-3.4; tooling §4.1-4.5; FFI/embedding §5.1-5.6; distribution/startup §6.1-6.4; combos §7). Author inputs: charter `AGENTS.md` (pains: Nix, Python modules, `#include`; taste: TS+bun ergonomics loved, Go/Lua disliked) and the package-manager essay at C:/programs/me/lxl66566.github.io/src/coding/package_manager.md (per-package locks, output-hash store, FHS linking, bin-first, P2P, `#OUT#`/`#TMP#`/`#ARCH#`). Sister catalogs consulted for index tables only: `docs/en/design/01-types.md`, `docs/en/design/02-linearity-ownership.md` (C1-C5 recommendations), `docs/en/design/03-effects-errors-async.md` (B5, E1 recommendations). research/06's inline web sources (Zig, Inko, pnpm, MoonBit/mooncakes, rust-analyzer/salsa, Erlang code loading, Mun, Julia Revise, Bun --bytecode benchmark, PEP 552, AppCDS, WIT/Component Model, Lua C API, Python C API, cargo#10489) are cited there and not duplicated here.
