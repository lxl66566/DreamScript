# 06 — Modules, packages, build, tooling, FFI, distribution

Research for DreamScript (2026-11-04). Inputs: the author's PL-philosophy essay (modules as decoupling; hatred of `#include`-style text copying and of Python's module granularity) and the package-manager essay (Nix pain: input-hash store, flake `follows`, global locks breaking dependents, no FHS, steep builtin docs; the hash-store UPM proposal: bin-first, per-package dependency locks, hash-prefixed single store, FHS linking, P2P distribution). Charter constraints: interpreted bytecode IR with optional JIT, linear/affine types, immutable-by-default, effects optional, sugar ceiling Kotlin, decoupling-first (LSP/RISC model). Evidence: local clones of inko, koka, luajit, moonbit-core, wasmtime (read-only) plus web sources cited inline. Conventions: every paragraph is one line; code blocks and tables are exempt.

## Executive summary

Modules: the strongest prior art for DreamScript is a layered model — file = module, directory = package, package = dependency/distribution unit — with **symbol-level import lists** (Inko `import mod1.mod2 (A, B)`, MoonBit `import { "moonbitlang/core/builtin" }`), **generated interface files** (MoonBit `.mbti`, OCaml `.mli`) that double as reviewable API diffs, and **per-package cycle rules**. Cycles: Rust allows module cycles inside a crate (name resolution is crate-wide), Go bans all cycles (fast DAG compilation, `init()` semantics, design-smell enforcement), ESM allows cycles with live bindings but partial initialization causes TDZ crashes, Python's execute-on-import model makes cycles fail at runtime. Verdict for DreamScript: allow cycles between modules inside a package (compile-time resolution like Rust/Nim), ban cycles between packages (like Go/Inko/MoonBit — MoonBit's core library documents how shared traits live in `builtin` to avoid cycles), because package cycles are what break incremental compilation and hot reload, while module cycles are what fix the author's "cannot cross-reference" pain without boilerplate interface packages.

Package management: three viable backends converge on the author's hash-store idea already: pnpm's content-addressable store + hardlinks + (new) global virtual store, Zig's build.zig.zon URL+hash manifests with a global hash-addressed package cache, and Inko's manifest that embeds version+commit-hash so the manifest itself is the lock. The author's proposal (per-package lock, output-hash store, FHS linking, bin-first, P2P) is implementable almost entirely out of existing mechanics: pnpm proves file-level CAS + hardlink materialization; Zig proves hash-as-lock with no lockfile; Nix proves symlink-profile linking (and its `follows` pain argues for Zig-style dependency-hash-cascading instead of lock-graph pinning); Bazel proves output-keyed action caches. The two genuinely new parts are (1) doing it cross-language and bin-first, and (2) P2P distribution — both feasible but non-core for DreamScript v1; the language only needs manifest format, MVS resolution, and a store path convention.

Build: DreamScript should ship a single `ds` binary whose build layer is a declarative manifest (Cargo-style) with a same-language escape hatch run in the VM the compiler already embeds (Zig compiles a build runner; an interpreter does not have to). Declarative default + code escape is strictly better for an interpreted language: the 90% case (`ds run script.ds`, `ds test`, `ds build --target wasm`) must be zero-config like `bun`, and cross-compilation should lean on Zig `cc` as a linker (Inko's documented approach) rather than a bespoke toolchain matrix.

Tooling: LSP-first is a compile-to-IR architecture decision, not a tool: rust-analyzer/salsa prove that a query-based ("compiler as library") design is what makes IDE-grade incremental checking possible, and DreamScript's bytecode IR pipeline should be structured as memoized queries from day one. Formatter, interface generator (`ds info`), diagnostics renderer (codespan-reporting/ariadne style), and REPL should all be compiler components: MoonBit's `moon fmt`/`moon info` workflow is the best modern template. Hot reload: for an interpreted language, Erlang's two-version code model (current + old, fully-qualified calls switch code, purge semantics) is the right runtime-module-format contract and is far cheaper than Mun's C-ABI-assembly state migration; Julia Revise shows REPL-driven redefinition is practical if method invalidation is incremental.

FFI/embedding: C ABI as the universal low level (Rust `repr(C)`, Zig `export`/`@cImport`), a handle-based (not pointer-based) GC interop layer learned from Lua's stack+registry API and the Python C API's owned/borrowed-reference trap, WIT/component-model as the optional typed boundary for wasm hosts, and Roc's platform model as the conceptual fit with DreamScript's effects design: the platform provides allocators/effects/host functions, the language consumes them as effect rows, which composes cleanly with linear types at the boundary (linear handles crossing the FFI must be consumed exactly once — this is a feature, enforce it).

Distribution: bytecode cache validated by source hash (PEP 552-style, not mtime), loadable via mmap (Java AppCDS proves the win), single-binary mode with embedded bytecode (Bun `--compile --bytecode`: measured 1.38x startup gain over bundle-only; LuaJIT's `bc` format from `lj_bcdump.h` is the minimal precedent: magic, version byte, ULEB128, strip flag), and wasm as a first-class distribution target (MoonBit ships wasm/js/native from one source tree via target-suffixed files). Windows needs a shim-based PATH strategy (scoop-style), not symlinks.

Four viable ecosystem combos are enumerated in the final section: (A) Cargo-style manifest+registry, (B) hash-store native (recommended: manifest-as-lock + output-hash store + FHS materializer, the author's UPM grounded in proven mechanics), (C) bun-style zero-config script ecosystem with cycles-allowed modules and hot reload, (D) platform-model embeddable (Roc/Koka hybrid). B and C are compatible and jointly recommended; A is the fallback if registry infrastructure is wanted early; D is an add-on axis, not an alternative.

## 1. Module systems

### 1.1 The vocabulary first

Three distinct concepts are usually conflated; every surveyed language draws the lines differently, and the author's "#include pain" is precisely about a language lacking the first two: (a) **namespace/visibility unit** (what names live together and who can see them), (b) **compilation unit** (what the compiler can typecheck/cache independently), (c) **distribution/dependency unit** (what a package manager resolves and versions). C has none (headers are text inclusion); C++20 modules bolt (a)+(b) on with painful toolchain maturity; Python has (a) at directory granularity with runtime-executed semantics; Rust has all three but with coarse (b); MoonBit has all three cleanly. DreamScript should state explicitly which layer each construct serves.

Mapping table of the surveyed systems:

| Language | Namespace unit | Compile unit | Distribution unit | Interfaces |
|---|---|---|---|---|
| C/C++ (pre-20) | file / header | translation unit (text-pasted) | library/archive | headers (text, no typesafe boundary) |
| Rust | module (crate-internal) | crate | package (Cargo) | none explicit; `pub` visibility + rustdoc |
| Go | package | package | module (go.mod, many packages) | exportedness by capitalization |
| OCaml | module / compilation unit | `.ml` file | opam package | `.mli` signature files |
| Ada | package (spec+body) | package body | library | package spec (language-level) |
| Nim | module | whole program (lazy) | nimble package | `export` markers only |
| Zig | file | lazily analyzed file graph | package (zon tarball) | none; decl-level visibility |
| ESM/TS | file | file (bundler per project) | npm package | `.d.ts` declarations (generated or hand-written) |
| Python | module file / package dir | module (executed at import) | wheel/dist | none (runtime introspection only) |
| Unison | definition | definition (hash) | codebase namespace | types are in the hash (typed AST) |
| MoonBit | file (block-structured) | package (directory) | module (moon.mod.json, registry) | generated `.mbti` |
| Inko | file (module) | package | package (git repo) | none explicit; `pub` visibility |

### 1.2 Rust: crates, modules, and paths

A Rust **crate** is the compilation and distribution unit (Cargo resolves and versions crates); a **module** is a namespace and visibility boundary inside a crate, declared with `mod` and mirrored by file layout (`mod foo;` reads `foo.rs` or `foo/mod.rs`); visibility is `pub`, `pub(crate)`, `pub(super)`, `pub(in path::to::)`. Two properties matter for DreamScript. First, **name resolution is crate-wide and cycle-tolerant**: items in sibling modules can refer to each other freely (including mutual references), because rustc resolves the whole crate as a fixed-point — module cycles are legal; only cycles between crates are rejected (Cargo errors on cyclic package dependencies). Second, **privacy is orthogonal to files**: a file is not a visibility unit, so moving code between files is refactor-neutral; this is why Rust codebases do not exhibit Python-style "module plumbing" churn. The cost: `mod` declarations are boilerplate, and path grammar (`crate::`, `self::`, `super::`, 2018-edition `use crate::...`) is one of Rust's steeper corners — the Rust 2024 edition even had to reserve edition-prefixed paths to keep the grammar open.

```rust
// Rust: crate = package; modules mirror files; cycles between a.rs and b.rs are fine.
mod a;            // reads src/a.rs
mod b;            // reads src/b.rs
pub(crate) fn helper() {}   // visible crate-wide, not to dependents
// in a.rs:  use crate::b::B;      -- b may equally `use crate::a::A`
```

For DreamScript: take crate-wide resolution and cycle tolerance, drop `mod` declarations (file = module, path = name), keep a small visibility grammar (`pub`, `pub(pkg)`, private).

### 1.3 OCaml, Nim, Ada: hierarchic modules and interfaces

OCaml compilation units are modules one-to-one with files, with explicit interface files (`.mli`) giving each module a signature; the module language is deep: modules are first-class-ish values, functors parameterize modules over modules, and the signature is the unit of abstraction (transparency/opacity). Compilation units cannot be mutually recursive (link order is a DAG); within a program you break cycles by extracting a shared lower unit or using recursive modules (rare, heavyweight). Ada draws the cleanest spec/body split in the industry: package **spec** (the interface, what clients compile against) vs package **body** (the implementation), plus hierarchical **child packages** (`Ada.Strings.Unbounded`) that see the parent's private part — visibility flows down the hierarchy, which is exactly the "nested module" the author's essay describes as good decoupling.

```ada
package Stack is                  -- spec: the contract clients compile against
  type Stack is limited private;
  procedure Push(S : in out Stack; X : in Integer);
private
  type Stack is record Top : Integer := 0; Buf : array (1..256) of Integer; end record;
end Stack;
package body Stack is ... end Stack;          -- body: freely refactorable
package Stack.Bounded is ... end Stack.Bounded;  -- child package, sees parent's private part
```

Nim is the useful counter-example for cycles: Nim explicitly **supports circular imports** between modules (the compiler compiles the whole program and resolves names late), which removes the interface-package boilerplate but pushes the compiler to whole-program analysis — acceptable for Nim (it compiles via C, whole-program anyway), and directly relevant to an interpreted DreamScript that already loads whole-program bytecode. Nim also shows the `import`/`from`/`include` trichotomy: `include` is literal text pasting (what the author hates), `import` brings a module's namespace (with `export` re-export), `from X import y` is symbol-selective — DreamScript should have `include` nowhere near the language.

### 1.4 1ML and first-class modules

1ML (Rossberg, ICFP 2017) shows that ML modules and the core language can be unified without losing type inference for practical programs: every expression is a module, every module is a value, functors are functions, signatures are types, and generativity comes from typing (fresh abstract types per application) rather than from a separate functor layer. The lesson for DreamScript is not to adopt ML modules but to note what unification buys: one concept instead of three (value/type/module levels collapse), which serves the charter's orthogonality principle; and what it costs: elaborate dependent-style type theory in the checker (small universe types), which fights the "fully explainable in a short tutorial" budget. A middle path used by Kotlin/TS/Rust pragmatics: modules are not values; code is shared by functions/generics, not by module parameterization; functors' real use cases (parameterized implementations, mockability) are covered by traits/parametric effects in a linear effects language.

### 1.5 Go: packages and the cycle ban

Go's package = directory with a shared namespace, files within a package see each other's top-level declarations with zero ceremony (no import between files of one package — a property worth copying for intra-package ergonomics), `import` is by canonical path, exportedness is capitalization. Cycles between packages are a compile error. The stated reasons (golang/go discussions; see also the DoltHub 2025 writeup on cycle-avoidance strategies): compilation proceeds package-by-package over a DAG and parallelizes; package `init()` functions would have ill-defined (potentially recursive) semantics under cycles; and the ban is a design-smell detector that pushes dependency inversion. The counter-evidence: issue golang/go#49251 and the DoltHub strategies article document how often real code must fake cycles via interface indirection, shared "types" packages (the notorious `pkg/types` dumping ground), or function-value injection — exactly the boilerplate the author hates in Python. Note Go's `init()` is the load-bearing constraint: ESM proves cycles are fine when module initialization is a defined, sequential protocol (see 1.7).

### 1.6 Zig and TypeScript/ESM: file-as-module, explicit imports

Zig: a module is a file, `@import("path.zig")` returns the file as a struct (namespace); imports are explicit, there is no package-level glob, and files can be mutually recursive because analysis is lazy (declarations are only analyzed when referenced) — a compile-time analogue of ESM live bindings. Zig's std itself demonstrates the discipline's cost: when the `std.Io` reorganization landed (0.15), code was split so blocking-vs-nonblocking types live in separate importable modules — explicit-import graphs are refactorable but every edge is hand-maintained. TypeScript/ESM: static import/export syntax, **live bindings** (imported names are references to the exporting module's binding, per MDN; mentally "all modules concatenated into one scope" per jameshfisher.com), evaluation is depth-first post-order of the dependency graph. Cycles therefore resolve: hoisted function declarations survive (callable across the cycle), but `let`/`const`/class exports accessed before their exporter finished evaluating throw `ReferenceError` (TDZ) — the "bug that passes every toolchain check" (ofriperetz.dev article). CommonJS instead returns a snapshot copy of `module.exports`, which makes cycles "work" but with stale-value traps.

```js
// a.mjs imports b.mjs, b.mjs imports a.mjs -- legal ESM
// a.mjs:  import { later } from "./b.mjs";  export const ready = 1;  later();  // OK: hoisted fn
// b.mjs:  import { ready } from "./a.mjs";  export function later() { return ready; }
//          console.log(ready);  // ReferenceError: TDZ -- a hasn't finished evaluating
```

Deno/bun keep ESM semantics, add URL/registry specifiers (`jsr:`, `npm:`) and import maps (`deno.json` `"imports"` mapping bare names to URLs) — a zero-config module resolution layer DreamScript can imitate with a built-in standard-library namespace instead of URLs.

### 1.7 Python: precise mechanics of the author's pain

The author's complaints (essay: "module granularity insufficient, cannot cross-reference, essentially enhanced code copying") decompose into five checkable mechanisms. (1) **Import-time execution**: a module body is an arbitrary script run once and cached in `sys.modules`; a "module" is a runtime mutable namespace dict, not a compile-time boundary — so there is nothing resembling an interface (`.mli`/`.mbti`/header); API surface is whatever survives at runtime, discoverable only by executing. (2) **Cycles fail at runtime, not at compile time**: under import cycles, one side sees a partially-initialized module; `from x import y` copies the binding and raises `AttributeError`/`ImportError` if `y` is not yet defined — the classic "move the import into the function" workaround culture. (3) **Granularity is directory-or-file, visibility is convention**: `__init__.py` re-export boilerplate, `_`-underscore privacy by convention, no per-symbol `pub`, so decoupling is enforced only socially. (4) **Path-based resolution is environment-sensitive**: `sys.path` walking, `site-packages` flat namespace, editable installs, namespace packages — the root cause of "works on my machine" and of the venv/uv sprawl. (5) **Two competing semantics** (script vs module: `__main__` vs import) mean the same file behaves differently by entry mode.

```py
# a.py:  from b import g        # b executes first
# b.py:  from a import f        # a is partially initialized (in sys.modules, body not done)
#         -> ImportError: cannot import name 'f' from partially initialized module 'a'
# idiomatic fixes: move import inside function; or extract a third module -- boilerplate
```

DreamScript's module design should be judged by whether it reproduces any of these five; notably (1) is the deepest: if module bodies are pure compile-time declarations (as in Rust/Go/MoonBit), cycles inside a package become a name-resolution detail instead of a runtime hazard.

### 1.8 Unison: content-addressed definitions

Unison stores every definition as a hash of its typed syntax tree including dependency hashes (unison-lang.org "The Big Idea"); the codebase is an append-only CAS, names are merely metadata, and files don't exist — code is displayed as text but stored structurally. Consequences: renames and moves are conflict-free (hashes unchanged), dependency "versioning" is intrinsic (changing a definition yields a new hash, dependents get new hashes transitively), merges are definition-level set unions, and a hash unambiguously identifies runnable code enabling safe remote execution (the microservices-as-monolith property). Costs: the entire tooling stack is bespoke (no plain-text file ecosystem, git-for-text is bypassed, editors work through a sync protocol), program text cannot be diffed line-wise (Unison's own UCM diff is semantic), and ad-hoc scripting against files (grep, patch, LSP-on-text) is reinvented. LWN's 2023 coverage and the Unison 1.0 announcement confirm the model survived to 1.0 but remains niche.

```
-- Unison sketch: hash replaces name; `ability` = effect
unique type Counter = { current : Nat }            -- stored as #abcdef...
increment : Counter -> Counter                     -- hash covers body + deps' hashes
```

For DreamScript this is a design axis, not a feature: the author's own UPM essay wants hash-addressed *packages/outputs*, not hash-addressed definitions; adopting Unison's full model would foreclose the plain-text-file culture the author lives in (VSCode, grep). A cheap borrow: make compiler-IR artifacts content-addressed by source hash for the bytecode cache (section 6), and make *package* identity hash-addressed (section 2.7) without touching definition identity.

### 1.9 MoonBit: the modern small-language template (evidence from moonbit-core)

Concrete facts from the moonbit-core clone: MoonBit is organized **module → package → file**, where a module has `moon.mod.json` (name, version, deps) at the repo root, each directory is a package with a `moon.pkg` whose entire content is typically an import list (e.g. `moonbit-core/abort/moon.pkg` contains only `import { "moonbitlang/core/builtin" }`), and packages can be imported under aliases (`@json`). `moon info` generates per-package **interface files** (`pkg.generated.mbti`) — `moonbit-core/abort/pkg.generated.mbti` lists exactly `pub fn[T] abort(String) -> T` under sections Values/Errors/Types/Traits — and the repo's AGENTS.md instructs contributors to treat `.mbti` diffs as the API-change review artifact ("If nothing in .mbti changes ... it is typically a safe refactoring"). Source files are **block-structured with `///|` separators**, and the docs state block order is irrelevant, enabling block-granular tooling and refactorability (a direct fix for the author's granularity complaint: the compiler's unit of relevance is below file level). Conditional compilation is filename-suffixed per backend/target (`double_mod_js.mbt`, `double_mod_nonjs.mbt`, `double_ryu_wasm.mbt`) — no preprocessor, target selection by file name. Tests live beside code: blackbox `*_test.mbt` (imports the package as a client) and whitebox `*_wbtest.mbt` (sees private items), with snapshot testing via `inspect` and `moon test --update`. The mooncakes.io registry (announced Feb 2024; ~3.5k modules by 2026) hosts modules with per-version generated docs. This is the closest existing ecosystem to DreamScript's stated values (self-contained tooling, small orthogonal pieces, LSP-first, Chinese-community momentum).

```json
// moonbit-core/abort/moon.pkg (verbatim): a package's whole config is its import list
{ "import": ["moonbitlang/core/builtin"] }

// moonbit-core/abort/pkg.generated.mbti (verbatim, abridged): generated, reviewable API
// package "moonbitlang/core/abort"
// pub fn[T] abort(String) -> T
```

### 1.10 Module granularity for a scripting language with big-data workloads

Big-data scripts (the author's domain per the site) need: cheap top-level scripts (bun-style, no package scaffold), a low-friction way to grow a script into a package (TS/bun: same file just gets imported; Go: same package just gets more files), fine-grained incremental rechecking for multi-GB datasets where a tweak re-runs a pipeline (favors block-level relevance tracking like MoonBit's `///|` blocks and salsa-style dependency tracking), and data-schema stability across reloads (favors immutable-by-default structs with explicit layout, which also serves hot reload, section 4.5). Recommended granularity: **definition (function/type) visibility and identity < file (module) < directory (package) < repo (module/registry unit)**, with packages as the only acyclicity boundary and files within a package resolving as one namespace (Go-style, no intra-package imports). This gives Python's zero-boilerplate growth path without Python's runtime namespace semantics, and matches the essay's "compose small modules into bigger ones" without an interface-file requirement (interfaces are generated, as MoonBit does, not hand-written).

Growth path sketch for DreamScript:

```
script:  ds run pipeline.ds                         # no manifest, deps auto-resolved
package: pipeline.ds + transform/ directory         # same file, now imported by others
module:  ds.pkg at repo root { name, deps, hash }   # registry/publishable unit
```

### 1.11 Cycles: decision analysis

Prior art table:

| System | Module cycles | Package cycles | Mechanism / cost |
|---|---|---|---|
| Rust | allowed (crate-wide resolution) | banned | fixed-point name resolution per crate; parallel per-crate compilation preserved |
| Go | n/a (files share namespace) | banned | DAG build + parallelism + `init()` semantics; forces interface indirection workarounds |
| ESM/TS | allowed | allowed (with warnings) | live bindings + depth-first eval; TDZ traps on early `const` access; bundler divergence (webpack#10870) |
| Python | allowed syntactically, fails in practice | allowed | execute-on-import; partial module objects; AttributeError workarounds |
| OCaml | banned (units) | banned | explicit `.mli` interfaces; DAG link order; recursive modules as escape hatch |
| Nim | allowed | allowed (within program) | late name resolution, whole-program compilation |
| Inko | allowed (modules) | banned | MVS package DAG; `import` path grammar with symbol lists |
| Zig | allowed (lazy analysis) | banned-ish (dependency graph via build.zig.zon is a DAG) | lazily analyzed declarations |
| MoonBit | allowed (blocks resolve late) | banned (see builtin-trait rule below) | block-level resolution; registry DAG |
| Unison | moot (definitions are a set) | moot | hashes are a DAG by construction |

The MoonBit core repo documents the sharpest engineering rationale for banning package cycles: its AGENTS.md explains that promoting a trait's methods is safe only when the trait lives in `builtin` (every package already depends on it), while promoting traits from higher packages (`@json.ToJson`) on lower-level types "would form a cycle (builtin → json etc.)" and is therefore never done. That is the pattern to copy: **cycles are prevented by placing shared abstractions (traits, error types, schema types) in a universal base package**, and the compiler's cycle diagnostic should suggest "move `X` to `core`".

A concrete module graph for a DreamScript big-data script (what the design should make natural):

```
pipeline.ds ──> transform/ ──> io/ ──> @std/io          # intra-package: cycles allowed
    │               │                          │
    └───────────────┴──────> schema/ ─────────┘        # shared types live low
                 (package graph stays a DAG: pipeline -> transform -> schema -> std)
```

DreamScript recommendation: allow module (file) cycles inside a package with Rust-style whole-package resolution; forbid package cycles with a good error; make the standard library's base layer (types, traits) dependency-free so the escape hatch always exists. This directly answers the author's Python cross-reference pain at zero cost to incremental compilation, and it is what hot reload needs anyway (section 4.5).

Import grammar summary (what to steal):

| Feature | Example | Source | Adopt? |
|---|---|---|---|
| Symbol-selective import | `import mod1.mod2 (A, B as C)` | Inko syntax.md (verbatim) | yes |
| Import with alias namespace | `import @json` then `@json.from_json` | MoonBit | yes |
| Conditional import | `import foo if linux and amd64` | Inko syntax.md (verbatim) | yes |
| External C link | `import extern "m"` | Inko syntax.md (verbatim) | yes, mapped to FFI (5.1) |
| Re-export | `pub use crate::x::*` | Rust | as `pub import` |
| Target-suffixed files | `foo_js.ds`, `foo_nonjs.ds` | MoonBit (verbatim filenames) | yes |
| Text inclusion | `include "x.nim"` | Nim | never |

## 2. Package managers and versioning

### 2.1 Cargo: the declarative baseline

`Cargo.toml` (manifest: name, version, deps with semver ranges, features, target-conditional deps, workspace) + `Cargo.lock` (exact resolved graph with checksums; committed for apps). Resolution unifies SemVer-compatible versions to one and allows incompatible majors to coexist (like Go/Inko); the resolver (PubGrub-based) prefers the highest allowed version. Lockfile makes builds reproducible per workspace but **not hermetic** (build scripts and proc-macros run arbitrary code; vendoring and `cargo fetch --locked` are the mitigation). **Features are the design's wart**: they must be additive (enabling a feature may only add functionality, never change it) and are **unified across the dependency graph** (the union of all requesters' features is enabled for every build of that crate — cargo#10489), which produces "my dependency compiled serde with features I never asked for" and makes feature-aware caching harder; Cargo's own docs note lockfile generation builds the graph as if all workspace features were enabled.

```toml
# Cargo.toml shape
[package]
name = "foo"
version = "0.3.1"
[dependencies]
serde = { version = "1.0", features = ["derive"] }   # range -> lockfile pins 1.0.219
json = "2"                                            # major 2 may coexist with another dep's json = "1"
```

For DreamScript: take the manifest+lockfile shape (or its hash-cascade equivalent, 2.7), take multi-major coexistence, but consider *not* having features at all (an interpreted language can afford multiple packages for variants; features in a bytecode language reintroduce conditional compilation the module system then must typecheck per configuration — MoonBit instead uses filename suffixes, Inko uses `import foo if linux and amd64`, both simpler).

### 2.2 npm/pnpm/bun: store mechanics worth stealing

npm's classic layout (each project's `node_modules` containing a nested tree, deduped imperfectly) produced the "node_modules wars" that pnpm resolved with a **global content-addressable store**: files are stored once keyed by content hash, each project's `node_modules/.pnpm/<pkg@ver>/node_modules/<pkg>` contains **hardlinks** to store files (file-level, not directory-level; reflinks on APFS/Btrfs), and top-level direct deps are **symlinks** into `.pnpm` — giving strict isolation (a package can only require what it declared) plus O(1)-disk installs (pnpm.io motivation + symlinked-structure docs). Two 2025-era developments map directly onto the author's UPM sketch: pnpm's **global virtual store** hardlinks store files into one global location and symlinks projects into it (cutting per-project materialization), and bun replaced the binary `bun.lockb` with a readable text `bun.lock` (1.2+) while keeping workspace support. Constraints learned the hard way: hardlinks require same-volume stores (Windows: NTFS hardlinks work, but junctions/symlinks have permission nuances — pnpm documents fallbacks), and CAS integrity needs per-file hashes verified at fetch (pnpm lockfile v9 stores integrity per file).

```
node_modules/
  .pnpm/
    lodash@4.17.21/node_modules/lodash/index.js   # hardlink -> ~/.local/share/pnpm/store/v10/files/ab/cd...
    left-pad@1.3.0/node_modules/left-pad/...
  lodash -> .pnpm/lodash@4.17.21/node_modules/lodash   # symlink: only direct deps visible
```

For DreamScript the transferable idea is: **store once by hash, materialize by hardlink, isolate by symlink graph** — the author's backend proposal essentially, already proven for a mainstream ecosystem.

### 2.3 Zig: hash-as-lock, no registry, no lockfile

Zig's package manager (stable since 0.11, docs: ziglang.org/learn/build-system/dependencies): dependencies are declared in `build.zig.zon` as **URL + hash** (a tarball, usually a git ref), `zig fetch` caches into the **global cache** (`~/.cache/zig/p/<hash>`), and the hash is computed over the file set filtered by the declared `paths` — any content change fails the build, so **the manifest is the lockfile** (no separate lock, no registry, works with any static file host or git). Crucially, Zig computes a **package hash that includes the hashes of the package's dependencies**, so a dependency change cascades a new hash for every dependent package — this is exactly the content-cascade the author wants ("updating a single package must never break upstream") achieved without lockfiles: the manifest pins transitive identity by construction. Updating is a manual `zig fetch --save` (the hash is printed in the error message when a placeholder is used) — the UX wart versus `cargo update`/`moon update`. No central registry exists by design; discovery is via URLs (community indexes are third-party).

```zig
// build.zig.zon (shape)
.{
    .name = .mylib,
    .version = "0.2.0",
    .paths = .{ "build.zig", "build.zig.zon", "src" },   // hash covers exactly this
    .dependencies = .{
        .zap = .{ .url = "https://github.com/zigzap/zap/archive/refs/tags/v0.9.1.tar.gz",
                  .hash = "1220faf8e9fce1955636b6bd6f78f4d7c8b3e6b1e6e8e2a1a5f4c3d2b1a0987654321" },
    },
}
```

For DreamScript, this is the strongest prior art for the author's essay: it demonstrates per-package locks, output-hash-addressed storage, multi-version coexistence (hashes differ), and FHS-agnostic linking, all with a two-field manifest format.

### 2.4 Inko: MVS + git packages, manifest-as-lock (evidence from inko/docs)

Inko's `inko.pkg` manifest (read from `inko/docs/guides/packages.md`) has one command: `require URL VERSION CHECKSUM`, e.g. `require github.com/alice/json 1.0.0 f3f378ad8ea4b617401b40ace743614995904755` — version plus commit SHA1 embedded in the manifest, so the manifest is simultaneously declaration and lock, and there is no registry (packages are git repos). Version selection is **minimal version selection** (Russ Cox's MVS): each requirement is a minimum, resolution picks the max of minima, no solver, no lockfile needed, and "you never end up using a version you never tested against". Multi-major coexistence is supported (foo can use json 1.x while bar uses json 2.x); 0.x versions are treated as compatible to avoid dependency explosion. Security updates are opt-in by bumping the minimum (documented tradeoff). Deps are vendored into a local `dep/` dir managed by `inko pkg`. Note the design debt Inko carries: SHA1 (fine for commit-pinning but weaker than blake3), and version-in-manifest means every dependency bump is a source diff — which is exactly what makes the author's per-package-lock property hold.

### 2.5 Deno: URL imports, import maps, JSR, and the node_modules retreat

Deno 1.x shipped URL imports + `deps.ts` convention + `deno.lock`; Deno 2 (Oct 2024) reversed course and added native `package.json`/`node_modules` compatibility ("run your existing Node projects"), because ecosystem gravity beat URL purity. The successor idea is **JSR** (jsr.io, launched with Deno 1.42/2024): a TypeScript-source registry — packages are published as source with docs derived from JSDoc, no tarball build step, resolvable as `jsr:@scope/pkg@version`, importable from Deno (via import map entries in `deno.json`), Node, bun, and pip-like tooling; npm: and jsr: specifiers coexist, with the documented subtlety that jsr: specifiers resolve through the import map rather than installing into node_modules (deno#33370).

```jsonc
// deno.json: the whole resolution layer is an import map
{ "imports": { "@std/path": "jsr:@std/path@^1.0.0", "lodash": "npm:lodash@^4.17.21" } }
```

Lesson for DreamScript: **URL-based imports as the only story is a dead end** (Deno itself abandoned it); but registry-per-ecosystem with generated docs (JSR + mooncakes) is viable, and import maps show that bare-name resolution can live in one small config file rather than a resolution algorithm walking directories.

### 2.6 Unison, Roc, MoonBit registries

Unison needs no versioning at all (hashes are the versions; sharing is codebase-to-codebase fetch by hash). Roc's package story is still forming around **platforms**: an app declares a platform, the platform provides effects; the general library package manager has been repeatedly deferred (per roc-lang FAQ/zulip) — evidence that platform-style architectures complicate the package model because the unit of compatibility is "platform version" not "library semver". MoonBit's mooncakes.io is the counter-example: a conventional registry (`moon add`, `moon install`, `moon publish`, per-version generated docs at mooncakes.io/docs/...) built in under two years for a young language — a registry is now commodity infrastructure (GitHub API + tarballs + static docs), not a moat. For DreamScript: registry can be a static file server + JSON index (Cargo's sparse index model) added late; the manifest and store design must come first.

Versioning-model comparison:

| Model | Systems | Reproducibility | Upgrade UX | Failure mode |
|---|---|---|---|---|
| Range + lockfile | Cargo, npm, pnpm, deno.lock | high (lock committed) | `update` bumps lock | lock/manifest drift; solver surprises |
| Hash-in-manifest | Zig, Inko, Go (pseudo-versions) | high by construction | manual pin bump | noisy diffs on update |
| MVS, no lock | Go modules, Inko | high (min pinned) | explicit bump per dep | under-upgrade (stale mins) |
| None (hashes) | Unison, Nix (FOD contents) | absolute | fetch by hash | no human-readable "version 2.1" |

### 2.7 Evaluating the author's hash-store UPM against existing mechanics

The author's proposal (package_manager.md): frontend with any-language build scripts in PKGBUILD-style phases, bin-first with `-bin`/`-git`/versioned variants in one manifest, arbitrary manifest formats, builtin template vars (`#OUT#`, `#TMP#`, `#ARCH#`), in-script update/test logic, **per-package dependency locks (hash+version)**; backend with a **hash-prefixed single store**, **multi-version coexistence**, **FHS-standard linking** for third-party compatibility, version-suffix collision rules; distribution via **P2P torrent with two hash algorithms** and a central hash authority. Mapping against prior art, piece by piece:

| Author's element | Already solved by | Mechanism | Gap remaining |
|---|---|---|---|
| Per-package dependency lock (hash+version) | Inko manifest `require URL VER SHA`; Zig `url+hash`; Cargo.lock | manifest-embedded pins (Inko/Zig) or generated lock (Cargo) | none — embed blake3 instead of SHA1; auto-bump tooling (`ds update`) is the UX cost |
| Output-hash-addressed store | pnpm CAS (files), Zig global cache (tarballs), Nix FOD (build outputs), Bazel CAS (action outputs) | content hash = store key; verify at fetch | Nix's lesson: **input-hash ≠ output-hash**; DreamScript should key store entries by output content hash (author's point 6), falling back to input-hash only for non-reproducible builds |
| Multi-version coexistence | Nix store paths, Zig package cache, Cargo (majors), pnpm | hash/name+version disambiguation | none |
| FHS linking | Nix user profiles (`/etc/profiles/per-user/.../bin` symlinks), buildEnv, scoop shims (Windows), pnpm global virtual store | symlink farm from store into standard paths, collision suffixing | the author's design (suffix-on-conflict, highest-version-unnamed) is workable; on Windows use shims not symlinks; hardlink-vs-symlink: hardlinks can't cross volumes and can't link directories — symlinks or file-level hardlinks (pnpm) are the proven choice |
| Bin-first | Nix binary cache (rejected by author for good reasons), AUR `-bin` packages, cargo-binstall-style manifest-declared bin URLs | declare a prebuilt artifact URL + hash per release target; fall back to source build | trust model: bin must be signed/checksummed per-platform; the author's "bin and unbin in one manifest" is cleaner than Nix's split cache |
| Any-language build scripts in phases | PKGBUILD (bash), Homebrew formulae (ruby), Nix builders (any), Bazel genrules | phase scripts run in sandbox with pinned tool deps | hermeticity gap: pinned tool *versions* per package (the author's dependency lock covers script deps too) keeps it honest; Nix sandbox (bwrap) is the precedent the author already endorses |
| P2P torrent distribution | IPFS/CAS networks; nix-casync; Nix-P2P research | chunk-CAS + magnet + signed manifest root hash | economically fragile for long-tail packages (the author's own mirror analysis shows why central mirrors strain); viable as *optional* accelerator since CAS chunks are torrent-friendly by construction; two-hash (blake3+sha256) multihash-style scheme is sound |

Verdict: the proposal is implementable and mostly *pre-implemented* in pieces; the genuinely novel claims are (1) one universal manager across ecosystems (scope creep risk — Nix shows universal build description is the hard part, and the author mitigates it with per-language phase scripts, which concedes hermetic toolchains per language), and (2) output-hash keys end-to-end (sound; Nix's ca-derivations work confirms feasibility and confirms it is early-stage elsewhere). For **DreamScript itself** the language only needs the three primitives it can own: manifest-with-hash (Zig/Inko style), MVS resolution (Inko/Go), and a global store + hardlink materializer (pnpm style) — the cross-language UPM can be built *on top of* DreamScript's store later, using DreamScript packages as first customers.

A concrete DreamScript manifest that realizes the author's requirements (sketch for Combo B):

```
# ds.pkg (TOML/KDL; one file, no lockfile, manifest is the lock)
name    = "pipeline"
version = "0.4.0"

[deps]                          # MVS minima; each pin carries output hash
std     = { ver = "1.2", hash = "blake3:9f2c..." }
csv     = { ver = "0.7", hash = "blake3:1a88..." }   # csv 0.7 may coexist with another pkg's csv 0.6

[artifacts]                     # bin-first: prebuilt per target, verified against store
x86_64-pc-windows-msvc = { url = ".../pipeline-0.4.0-x86_64-pc-windows-msvc.dsmod", hash = "blake3:77e0..." }
aarch64-linux-gnu      = { url = ".../pipeline-0.4.0-aarch64-linux-gnu.dsmod", hash = "blake3:c0d1..." }
# no [artifacts] entry for a target -> build from source in sandbox

[build]                         # optional, only if needed
script = "build.ds"             # run by the embedded VM; sees #OUT#, #TMP#, #ARCH# like the author's vars
```

This ordering avoids Nix's failure mode (a universal system with weak per-language ergonomics) and matches the author's decoupling ethic: store, resolver, linker, and fetcher are orthogonal pieces.

## 3. Build systems

### 3.1 Survey: who builds what, and why they win or lose

| System | Model | Why it wins | Why it loses |
|---|---|---|---|
| Cargo | declarative manifest + build.rs escape hatch + one binary doing build/test/doc/install | zero-config for the 90% case; lockfile; target triples; `cargo add/check/fmt/clippy/doc` single-brain UX | features unification mess; build.rs sandboxing story; cross-compiling C deps still manual |
| Zig build | `build.zig` **in the language itself**, compiled to a "build runner" executable that constructs a DAG of steps (official docs; issue #20981 splits configure vs execute phases) | cross-compilation is trivial (`zig cc` bundles libc for every target); no DSL to learn if you know Zig; comptime in build logic | community consensus (ziggit/reddit 2025-26): the build API is hard to grasp ("really difficult to grasp" thread), recompiling the runner on every `zig build` costs startup, docs lag |
| MoonBit moon | manifest + per-package `moon.pkg`; `moon build/test/fmt/info/check/publish`; one toolchain binary | uniform workflow, generated `.mbti` interfaces, snapshot tests, wasm/js/native backends from one tree | newer ecosystem; build is opaque (manifest-driven, little extensibility) |
| Meson | declarative DSL → generates Ninja | readable, correct-by-construction defaults, native vs cross clean | another mini-language; not same-language; mostly C/C++-scoped |
| Bazel | hermetic action graph, remote cache/execution (action key = hash of inputs+cmd+env, outputs content-addressed) | scales to monorepos of millions of files; reproducible; remote caching | heavy: Java runtime, BUILD files everywhere, macro layers, cryptic errors, poor docs (keasigmadelta head-to-head) — overkill for any single-language project |
| CMake | meta-build: DSL → generates IDE/Make/Ninja files | ecosystem gravity for C/C++ | the DSL is infamously bad (twdev.blog "Why CMake sucks?"), target-vs-variable duality, `find_package` folklore; "least bad we have" (HN) |

```zig
// build.zig (shape): the build graph is ordinary Zig code
pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const exe = b.addExecutable(.{ .name = "tool", .root_source_file = b.path("src/main.zig"), .target = target });
    const tests = b.addTest(.{ .root_source_file = b.path("src/main.zig") });
    const step = b.step("test", "run tests"); step.dependOn(&b.addRunArtifact(tests).step);
    b.installArtifact(exe);
}
```

The decisive pattern: **successful single-language build systems are one binary with a declarative default** (Cargo, moon, roc build, deno task, bun build) **plus a same-language escape hatch when needed** (build.rs; Zig's pure-code approach is the pure form and pays for it in API-comprehension cost — its own community says so). DreamScript is interpreted: most "build" is typecheck+bytecode-emit, which the `ds` binary does anyway; the build layer only needs dependency fetching, IR cache invalidation, C-ABI glue for FFI packages, and target selection.

### 3.2 Same-language vs declarative vs both

Same-language build (Zig) gives Turing-complete orchestration with zero extra languages, at the cost of a bespoke API surface that must be learned like a framework (the recurring complaint), and it couples build correctness to the host language's stability. Declarative manifest (Cargo/Meson) makes the 90% declarable, diffable, and lockable, but needs an escape hatch (build.rs is arbitrary Rust run at build time — Cargo's own security-sensitive corner). For DreamScript: **manifest by default, same-language `build.ds` for the 10%**, with the twist that an interpreted language can run the build script in the same VM the compiler already embeds (Zig must compile a runner executable; DreamScript just evaluates it — near-zero cost, and the charter's decoupling principle is served by the build script being an ordinary program with a small build-API, not a magic DSL).

Decision table:

| Option | Example | Pros | Cons |
|---|---|---|---|
| Declarative only | Cargo, Meson, moon | diffable, lockable, tool-analyzable | needs escape hatch eventually |
| Code only | Zig build.zig, Buck2 starlark | no DSL, full expressiveness | framework-scale API learning curve; cache analysis harder |
| Declarative + code escape | Cargo + build.rs, proposed DreamScript | 90% zero-config, 10% possible | two mental models; code path needs sandbox story |

Concrete sketch: `ds.pkg` (manifest, TOML/KDL) declares name/deps/targets; optional `build.ds` exposes `pub fn build(ctx: Build)` adding steps (codegen, C compile via `cc`/`zig cc`, asset embedding); `ds run x.ds` ignores both entirely for scripts.

### 3.3 Cross-compilation stories

Go: `GOOS=linux GOARCH=arm64 go build` — the gold standard, works because the stdlib is pure Go plus cgo-free defaults. Zig: cross is the headline feature (target triple + bundled libc + headers). Rust: `--target <triple>` plus an external linker story (`cargo-zigbuild` wraps zig as the linker, exactly what Inko documents: "Zig greatly simplifies cross compiling... Inko favours using Zig over gcc/clang"). Bun: `bun build --compile --target=bun-linux-x64` cross-compiles self-contained executables including Windows icons/signing (docs). MoonBit/Roc: target = backend (wasm/js/native), not OS — cross-"compilation" is free because the artifact is wasm/js and native builds ride on LLVM/cargo infra.

| System | Command | What it needs installed |
|---|---|---|
| Go | `GOOS=windows GOARCH=arm64 go build` | nothing (pure Go); mingw if cgo |
| Zig | `zig build -Dtarget=aarch64-linux-gnu` | nothing (libc bundled) |
| Rust | `cargo build --target aarch64-unknown-linux-gnu` | target std + cross linker (or zig via cargo-zigbuild) |
| Inko | `inko build --target aarch64-...` (uses zig when present) | zig recommended (its docs say so) |
| Bun | `bun build --compile --target=bun-linux-x64` | nothing |
| DreamScript (proposed) | `ds build --target aarch64-linux-gnu` | nothing for bytecode; zig cc optional for native embedding |

DreamScript recommendation: bytecode-first targets are free (IR cache per triple for native JIT assumptions); for native/embedded distribution, adopt the Inko/cargo-zigbuild posture (use zig cc when present, cc otherwise) and treat wasm as the universal low-effort target.

### 3.4 What the build layer must not become

Anti-goals evidenced above: no second DSL (CMake), no macro-layered BUILD graph (Bazel), no build-time code execution without a pinning story (build.rs/Nix lesson), no feature flags that fork the type-checked surface (Cargo features; prefer MoonBit-style filename suffixes + explicit `import x if os == "windows"` like Inko). The charter's LSP/RISC model applies: fetch, resolve, cache, materialize, compile, link are separate steps the `ds` binary composes, each independently cacheable and inspectable (`ds fetch --print-hash`, `ds build --explain-cache`).

## 4. Tooling

### 4.1 LSP-first: compiler as a query library

rust-analyzer's architecture (rust-analyzer blog "Durable Incrementality" 2023; salsa docs) replaces the pass pipeline with **memoized queries over a database** (parse, item tree, name resolution, type inference per-body, diagnostics), with red-green dependency invalidation: an edit bumps a revision; queries recompute only if an input changed. rustc itself uses the same query-system idea (rustc-dev-guide) for incremental compilation. tsserver took a different, instructive path: one global program per project, a document registry reusing ASTs across open files, and language features computed on demand — historically less principled than salsa but fast enough and *the* reference for "LSP as product" (auto-imports, quick fixes, cross-file renames).

```
// salsa-shaped query set for DreamScript (sketch)
parse(file_id)            -> Tree            // memoized on file content hash
block_index(file_id)      -> Vec<Block>      // MoonBit ///| blocks: finer invalidation units
resolve(pkg, name)        -> DefId
type_of(def_id)           -> Type            // red-green: depends on resolve()s
effects_of(def_id)        -> EffectRow
linearity_check(def_id)   -> Vec<Diag>
lower(def_id)             -> Bytecode        // depends on type_of + effects_of
diagnostics(pkg)          -> Vec<Diag>       // union of per-def diagnostics
```

For DreamScript the requirement falls out of the charter (interpreted, fast startup, IDE-centric author): **the compiler must be a library** (`ds-check` module) whose API is query-shaped from day one — parse(files) -> tree (content-addressed), resolve(pkg, name), typecheck(fn), diagnostics(pkg), lowering(fn) -> bytecode — because hot reload (4.5), REPL (4.3), and LSP are all the same consumer with different drivers. A non-goal: whole-program re-analysis per keystroke; the module/package design (1.10-1.11) must make per-package invalidation the unit (which is another reason to ban package cycles: cycles defeat incremental per-package checking). Koka is a small-language existence proof that the same compiler tree hosts compiler, interpreter and LSP: `koka/src/Main/` contains `langserver/` (compiler + VS Code LSP), `plain/` (compiler + interpreter for platforms where the Haskell LSP library does not build) and `playground/` — one codebase, three front-ends.

### 4.2 Formatter and interface generator as compiler components

Evidence: gofmt (shipped in `go`, unconfigurable — the decision that ended style debates), zig fmt (in the compiler binary), roc format (`roc fmt`), moon fmt (`moon fmt`, block-aware thanks to `///|` blocks), rustfmt (separate but rustc-adjacent; note rustc's own dev-formatting is internal). The MoonBit extra worth copying is `moon info` generating `.mbti` interface files, checked into the repo, so **API changes are reviewable diffs** (moonbit-core AGENTS.md: "check the diffs of .mbti file to see if the changes are expected"). For DreamScript: `ds fmt` (one canonical format, no options) and `ds info` (emit `pkg.iface` text per package) should be compiler components sharing the AST and the block model; the interface file doubles as the incremental-compilation boundary and as the LSP's hover/signature source.

```
// hypothetical ds info output for pkg "pipeline/transform"
package "acme/pipeline/transform"
// Functions
pub fn parse_row(String) -> Result[Row, ParseError]        // effects: <>
pub fn normalize(Row) -> Row                               // effects: <>
pub fn aggregate(Iterator[Row]) -> Stats                   // effects: <io>
```

Formatter-config comparison (why "no options" wins):

| Formatter | Options | Failure avoided |
|---|---|---|
| gofmt, zig fmt | zero | style debates, config drift in teams |
| prettier | ~dozens | config files diverge across repos |
| rustfmt | ~100 | nightly-flag churn; teams carry rustfmt.toml |
| ocamlformat | ~100 | per-profile presets needed |
| moon fmt | zero (block-aware) | same as gofmt |

### 4.3 REPLs and how types/effects show up

Koka: the REPL prints the inferred type **with its effect row** for every expression (koka docs/tour "try out this example directly in the interactive environment"; the REPL front-end lives in `koka/src/Interpreter/` — same tree as the compiler, evidence of compiler-as-library). Elixir iex: recompiles a module on each `defmodule` evaluation, keeping process state — the operational model that makes hot-swap feel natural (and exactly what Erlang code loading implements, 4.5). OCaml utop: typed toplevel with completion over the current environment; GHCi: `:r` recompiles the module DAG, state lost (the known wart). Julia: the REPL + Revise (4.5) redefined REPL-driven development: edit a file, methods are replaced in-place without losing session data, because Julia tracks method invalidation at the method-instance level.

```
$ koka
> println("hi")
hi
() : <console,io> ()                 # type AND effect row displayed
> map(x -> x + 1) : list<int>  ...
```

For a linear-typed language, the REPL has one real novelty to solve: **linear values at the prompt cannot be silently dropped** — the REPL must either auto-consume (display then free, like an implicit `consume` on the prompt variable), or keep an arena of "session-owned" values that the user explicitly drops; Koka's effectful REPL and Rust's evcxr ownership dance are the prior art. Recommendation: REPL = interpreted mode over the same bytecode IR with a session heap, effects unhandled (or handled by printing), and linear values owned by a session linear context with an explicit `:drop`.

```
$ ds repl
> let h = File.open("data.csv")     # h : File  (linear)
h : File -- owned by session context
> let rows = Csv.read(h)            # h consumed here; typechecker accepts because REPL re-checks
rows : Iterator[Row]
> :drop rows                        # explicit; or rows auto-dropped at :reset
```

### 4.4 Error messages

The lineage: Elm popularized friendly multi-line explanations with hints; rustc industrialized it with a **structured diagnostic model** (derive-based `Diagnostic`/`Subdiagnostic` structs, structured suggestions rendering as copy-pasteable diffs, JSON output for machines — rustc-dev-guide "Errors and lints"); the reusable libraries are **codespan-reporting** ("beautiful error diagnostics", labeled spans, notes, severities) and **ariadne** (multi-line/multi-file labeled spans), both direct rustc-style emulations. Roc's compiler messages and Elm's are the cultural reference for "famous messages". For DreamScript the practical requirements: spans must survive the IR boundary (diagnoses computed at typed-AST level, rendered against source), suggestions must be structured (insert/replace ranges) not stringly, `--error-format=json` for the LSP/CI, and — because the language is linear — a dedicated diagnostic class for linearity violations with "moved here / used again here" two-span output (rustc's borrow-checker diagnostics are the model: the E0382-style move/use pair is precisely what linear-type errors look like, minus borrow complexity).

```
error[DS2001]: value `h` was consumed here
  --> src/pipeline.ds:41:15
   |
41 |     let rows = Csv.read(h)
   |                        ^ `h` moved (File is linear)
42 |     Log.write(h.path)      <-- error[DS2001]: used again here after move
   |
help: `File` must be consumed exactly once; clone the path before the move
   |
41 |     let path = h.path
42 |     let rows = Csv.read(h)
   |       ++++++++++++++++++++++++++++++++++ (suggestion machine-readable as JSON)
```

### 4.5 Hot reload: what the runtime module format must provide

Four families of prior art:

| Approach | System | Mechanism | Constraint it imposes |
|---|---|---|---|
| Native re-dlopen + ABI metadata | Mun | assemblies are C-ABI shared libraries; runtime diffs type metadata old vs new and migrates struct state recursively (docs.mun-lang.org ch04-04; mun_runtime docs) | stable ABI, full type-reflection, state migration code — expensive; Mun exists because compiled languages need all this |
| Two-version code slots | Erlang/OTP | at most current+old module versions live; a process switches on the next **fully-qualified** call (`mod:func()`), local calls stay on old code; third load forces purge which kills stragglers; `soft_purge` vs `purge` (erlang.org code-loading docs) | external-call discipline in generated code (easy in a compiler), bounded old-version lifetime, state carried by processes not globals |
| Incremental method redefinition | Julia Revise | watches files, lowers changed code, invalidates/replaces **method instances** without restart, preserving session data | world-age semantics for running tasks; needs method-level granularity in the runtime |
| VM-level module reload | Bun `--hot` / bundler HMR | re-evaluate changed modules, swap exports, keep module state via module-registry indirection | ESM live-binding graph; state loss is common; fine for scripts |

For an **interpreted bytecode language** (DreamScript's case) the Erlang model is nearly free and strictly better than Mun's: the runtime holds per-module **versioned code slots**; `ds reload` compiles changed files to new IR, swaps the slot, marks old-version frames to retire at their next call boundary (fully-qualified call = every cross-module call in DreamScript, trivially satisfied); linear/GC'd state lives in the heap, not in code, so "state migration" reduces to type-compatibility checking of retained heap values (Mun's struct-diff logic, applied to a GC heap — much simpler without ABI). Julia-style method invalidation is the finer-grained variant worth growing into later (the JIT tier makes it relevant).

```
Hot reload protocol (DreamScript sketch):
  watcher: file changed -> reparse/recheck changed blocks (salsa queries, 4.1)
  compiler: lower changed defs -> new dsmod v(n+1)
  runtime:  install into slot[1] (slot[0] = old); frames referencing slot[0] age out
            at next cross-module call; heap values type-checked against new defs
            (compatible: keep; changed layout: migrate via Mun-style diff or quarantine)
  effects:  open effect handlers capture a code version; on reload, resumption uses
            the version the handler was installed with (Erlang's two-version rule
            generalized to handlers)
```

Requirements this places on the module format: modules addressable by stable name, per-module bytecode with dependency lists (for recompile closure), versioned slots with at-most-two-live rule, and **no top-level side effects** at load time (initialization must be explicit `main`/`init` — same rule that makes Python's cycle problem impossible, 1.7). Zig is the cautionary tale from the compiled side: hot-code-loading via `zig build-lib -dynamic` keeps hitting the stable-ABI/struct-layout wall (community threads; thenumb.at's "Hot Reloading in Exile" catalogs the techniques and their costs) — an interpreted runtime sidesteps all of it, which is a genuine architectural advantage DreamScript should advertise.

## 5. FFI and embedding

### 5.1 The C ABI layer

The universal substrate: a C-ABI function is a call with a documented calling convention; data crossing it must have a **defined layout**. Rust `#[repr(C)]` gives C-compatible struct layout (fields in declaration order, alignment-padded, exactly what C compilers emit); Rust `extern "C" fn` / `#[no_mangle] pub extern "C" fn` are the import/export forms; Zig `export fn` emits C-ABI symbols, `@cImport`/`translate-c` auto-converts headers into declarations, and `extern "c" fn` imports them. DreamScript needs, concretely: (a) a `repr(c)` struct attribute with C layout rules documented (natural alignment, padding, ordering), (b) `extern` function types with convention markers, (c) ownership boundary types — pointers in/out are opaque; linear types turn "must free exactly once" from convention into the type system: a `Foreign<T>` value is linear, `free(f)` consumes it, double-free is a type error, and the FFI layer is where linearity earns its keep.

C struct layout example (what `repr(c)` must reproduce exactly):

```
struct sample {          // 64-bit C ABI:
  char  a;              // offset 0
  double d;             // offset 8  (padded: alignof(double)=8)
  char  b;              // offset 16
  int   i;              // offset 20
};                      // sizeof = 24 (tail-padded to alignof 8)
```

Example surface for DreamScript:

```
type c["sqlite3"]                 // opaque handle, layout unknown, linear
extern fn sqlite3_open(path: *const c.Char, out: **c["sqlite3"]) -> c.Int
extern fn sqlite3_close(db: *mut c["sqlite3"]) -> c.Int   // consumes: db is linear
// typechecker enforces: every open path reaches exactly one close or leak-diagnostic
```

Header ingestion: MoonBit/Inko/Zig all bind C by declaring or translating, not by parsing at runtime; Zig's `translate-c` and bindgen-style generation from headers is the UX to copy (a `ds bind header.h` command that emits typed externs, not a runtime cimport).

### 5.2 Calling into/from GC'd runtimes

Two directions with different hazards. **Language calls C** (DreamScript as consumer): the GC must not move or free memory the C side retains; standard solutions are pinned allocation for FFI-visible objects, or the handle-table pattern. **C calls language** (callbacks): the callee runs on a foreign stack; a moving/stackful GC needs to know — either callbacks run on VM-managed stacks only (Lua's model), or the runtime supports arbitrary-stack entry (expensive). Lua's C API is the canonical minimal design: all data exchange through an explicit **value stack** (push args, `lua_call`, read results by index), C values persisted across calls go into the **registry** via pseudo-index `LUA_REGISTRYINDEX` and `luaL_ref` (integer handles into a table the GC scans), with `lua_upvalueindex` for C closures — no raw pointers to Lua values ever escape (lua.org PIL 27.3.1; Lua 5.3/5.4 manual's valid/acceptable-index rules). mlua shows the ergonomic wrapper layer achievable over it.

```c
/* Lua C API canonical sequence */
lua_State *L = luaL_newstate();                    /* VM + stack created */
lua_getglobal(L, "process");                       /* push fn */
lua_pushinteger(L, 42);                            /* push arg */
lua_call(L, 1, 1);                                 /* pop fn+arg, push result */
int r = (int)lua_tointeger(L, -1); lua_pop(L, 1);  /* read top, pop */
lua_pushlightuserdata(L, my_obj);                  /* opaque ptr, GC-invisible */
int ref = luaL_ref(L, LUA_REGISTRYINDEX);          /* GC-visible handle (int) */
```

The Python C API is the cautionary tale: every function returns **owned or borrowed references** (docs.python.org c-api/intro "ownership of references"), some arguments are *stolen*, refcount discipline is manual (academic studies find refcount bugs endemic in CPython extensions: PSU refcount.pdf, ASE-2023), the GIL serializes embedding, and subinterpreter isolation broke C extensions for decades (per-interpreter GIL only stabilized in 3.12+, pybind11 still documents fragility).

Embedding API design comparison:

| Property | Lua C API | Python C API | DreamScript proposal |
|---|---|---|---|
| Value access | explicit stack indices | raw `PyObject*` pointers | handles: `Value = index into VM object table` |
| Ownership convention | stack balanced; registry refs | owned/borrowed/stolen triad, manual refcounts | linear: borrow = `&Value` (call-scoped), own = `Value` (transfer) |
| GC safety | GC scans registry | refcounts; cycles need `gc` module | GC scans handle table; no raw pointers escape |
| Threading | one `lua_State` per thread or `xmove` | GIL; subinterpreters fragile | VM is a `&mut Vm` context; multiple VMs trivially parallel |
| Error model | `longjmp`-based panics | exceptions + `NULL` + last-error | `Result` returned through the boundary; no unwinding into C |
| Callback entry | VM-managed stacks only | any thread with GIL held | callbacks scheduled on VM stacks (or dedicated entry trampoline) |

Design rules for DreamScript's embedding API, derived from both: **handles not pointers** (a `Value` is an index into the host VM's object table — the Lua registry idea, GC-safe by construction), **no stolen references** (every API either borrows with lifetime bound to the call, or returns a new owned handle — linear types can encode this: borrowing APIs take `&Value`, consuming APIs take `Value`), **explicit context** (`ctx: &mut Vm` threading instead of globals — kills the GIL-by-design problem since two VMs are just two contexts), and **pinned foreign objects** so the GC never moves FFI-visible memory.

### 5.3 Wasm boundaries and JS interop

Wasm core gives imports/exports over a small type set (i32/i64/f32/f64 + refs); **WASI** previews give syscalls; the **Component Model** with **WIT** (WebAssembly Interface Types) adds typed, language-agnostic interfaces: a `world` declares imports/exports, and tooling (wasmtime's `bindgen!`, wit-bindgen) generates host and guest bindings in each language (component-model.bytecodealliance.org WIT reference; WIT.md spec). MoonBit is the best evidence a small language can treat wasm/js/native as equal backends from one source tree (moonbit-core's `*_js.mbt` / `*_nonjs.mbt` / `*_wasm.mbt` suffix files; `moon build --target wasm|wasm-gc|js|native`).

```wit
// WIT world sketch: the typed contract a DreamScript wasm component would export
package acme:pipeline@0.4.0;
world pipeline {
  import host-io: interface {                 // platform effects arrive as imports
    log: func(msg: string);
    read-file: func(path: string) -> result<list<u8>, string>;
  }
  export run: func(input: string) -> result<string, string>;
}
```

For DreamScript: compile the bytecode IR to wasm for browser/embedded distribution, and to a wasm *component* (WIT export) for host embedding — the component boundary then *is* the embedding API for wasm hosts, and the same effect-row types can render into WIT (a `console.print : (String) -> ()` effect surfaces as a WIT import the host satisfies — this is the mechanism that makes "effects compile to capability imports" concrete). JS interop from wasm target: follow MoonBit's suffix-file pattern rather than a preprocessor — `foo.js.ds` provides JS-glue externs compiled only for the js backend.

### 5.4 Roc's platform model: "the host provides, the language consumes"

Roc inverts the usual stdlib arrangement: applications are pure, and a **platform** (authored separately) supplies the entire effectful surface — I/O, allocations, host functions — plus the host executable the app is linked into; the compiler generates **glue** and a "surgical linker" patches app code into the host binary (roc-lang.org FAQ; deepwiki roc-lang/roc; basic-webserver and roc-ray as reference platforms). The app declares `app [main] { pf: platform "..." }` and simply calls platform-provided functions; effects are not a language feature but a platform property. This composes with DreamScript's design better than with Roc's own, because DreamScript can type the platform boundary: the platform declares an **effect row** it handles (`platform "web" provides <fsys, net, console>`), the checker verifies user code only uses provided effects (effect rows as the contract), and linear allocators (platform-provides-allocators is explicit in Roc) match the linear-types design.

Costs to acknowledge: Roc's FAQ documents the single-platform limitation (an app targets exactly one platform; multi-platform = multiple targets), platform versioning complicates packages (2.6), and the glue/linker machinery is significant compiler work. Recommendation: adopt the *pattern* (effects come from an injectable capability set; a minimal default platform = the `ds` binary itself providing a std effect row) without Roc's app/platform linkage complexity: DreamScript "platforms" are just packages that export effect handlers plus optional host glue, selectable per build target — the LSP/RISC decomposition of Roc's monolith.

```
// DreamScript platform sketch
platform "std"   provides <console, fsys, net, time>     // the ds CLI itself
platform "web"   provides <console, fetch>               // wasm host satisfies via WIT imports
platform "embed" provides <>                              // host provides nothing; host calls in
// app code: fn main() -> {console.print("hi")}  -- checker: <console> ⊆ provided row
```

### 5.5 Koka's multi-backend inline FFI (evidence)

koka/lib/std/core.kk contains `extern import c file "core/inline/core"` and `cs file "core/inline/core.cs"`; koka/lib/std/async/legacy/readline.kk declares `extern import { cs file "readline-inline.cs"; js file "readline-inline.js" }` — i.e., Koka lets library authors ship **inline snippets per backend** (C, C#, JS) alongside typed extern declarations, and the async stdlib provides backend-specific event-loop implementations (`api/web/evloop.kk`, `api/uv/file.kk`). This is a clean pattern for an interpreted multi-target language: typed extern signatures live in DreamScript; each target gets an implementation file (native: C ABI; wasm: imports; js: JS glue), selected by the same filename-suffix mechanism MoonBit uses. It also shows effects at the boundary: Koka's std declares FFI'd functions with precise effect types (`<asyncx,console>`), which is exactly the DreamScript shape.

```koka
// koka (verbatim shape): typed declaration + per-backend inline impl
extern import { cs file "readline-inline.cs"; js file "readline-inline.js" }
pub fun readline() : <asyncx,console> string { ... }
```

### 5.6 Cangjie and other interop claims

Cangjie (Huawei, open-sourced 2025) ships C FFI (call and be-called), ArkTS interop, claims Python-library interop, and multi-backend compilation including JVM bytecode (cangjie-lang.cn docs; Medium overview). Useful as market validation that (a) C ABI + one dynamic-runtime interop (Python) + one typed-VM interop is the expected surface for a new Chinese-ecosystem language, and (b) "seamless interop" claims are marketing — the mechanics are always handle tables + marshaling layers. No design novelty to import beyond confirming 5.1-5.3.

## 6. Distribution and startup

### 6.1 Single-binary distribution

Zig: `zig build-exe` produces self-contained binaries (statically linked libc via its bundled musl/glibc stubs) — the cross-compilation baseline. Go: single static binary plus `embed.FS` for assets. **Bun `--compile`** is the interpreted-language reference: bundles JS/TS into one executable with the runtime attached, and `--bytecode` precompiles to bytecode at build time so runtime startup skips parsing — measured ~1.38x faster startup vs bundle-only and ~2.01x vs `bun run` (peterbe.com benchmark; bun docs/compiler). The cost is size (the whole runtime ships per app — bun#19498). For DreamScript: `ds build --exe` should embed the bytecode of the whole dependency closure plus the VM; since bytecode is already the internal IR, this is natural (unlike Bun, which had to add a bytecode pipeline). On Windows, embed an icon/manifest via RC like Bun does; code-signing notes per platform (Bun docs cover macOS signing).

### 6.2 Bytecode caching and format

CPython: `__pycache__/*.pyc` with a header — magic number, bit field, source **mtime+size** (or, since PEP 552, a source **hash** in checked/unchecked modes); stale header = recompile; distros precompile at install time because read-only FS can't (Fedora change page). LuaJIT ships `luajit -b` producing bytecode files whose format is specified in `luajit/src/lj_bcdump.h`: header `0x1b 'L' 'J'`, version byte (2), ULEB128 flags (`STRIP`, `FFI`, `FR2`, `BITOP`...), then a stream of length-prefixed prototypes (`phead = flagsB numparamsB framesizeB numuvB numkgcU numknU numbcU ...`), children referenced as `BCDUMP_KGC_CHILD` — i.e., **modules link by embedded child prototypes, and the format is small enough to fully specify in a comment block**. Java: `.class`/jar plus **AppCDS** (JEP 310/350, JEP 483 AOT class loading) — pre-parsed class metadata dumped to an archive and **mmap'd** at startup, cutting class-loading work; Red Hat reports gains scaling with class count.

```
dsmod (proposed DreamScript module file):
  bytes  0..3   magic "\0DSM"                       # LuaJIT-style magic discipline
  byte   4      format version                      # bump on IR change (private forks: >=0x80)
  u8     5      target class (bytecode | wasm | native-triple-id)
  u8     6      flags (STRIP, FFI-USED, HAS-JIT-METADATA)
  uleb   ...    module name length + name
  uleb   ...    dep count; deps as (name, content-hash) pairs   # recompile closure for hot reload
  uleb   ...    source hash (blake3-128)            # PEP 552 checked-hash validation
  sections      blocks (bytecode protos): uleb-prefixed, length-delimited, relocation-light
  sections      string/int constants, type metadata, effect rows, debug spans (strippable)
```

Design for DreamScript: define a `dsmod` binary format with (a) magic + format version + target/IR-version (LuaJIT's header discipline; private-modification rule: bump version if IR changes), (b) per-module records with dependency-name list and content hash of source (PEP 552 checked-hash validation — hash, not mtime, because the author's store is content-addressed and mtime lies in git checkouts and stores), (c) ULEB-style varints and length-prefixed sections (LuaJIT), (d) strip flag for shipped artifacts. Cache layout mirrors the package store: `$DS_CACHE/ir/<triple>/<blake3-of-source+deps>.dsmod`, mmap-loaded (AppCDS proves mmap of prelinked metadata is the startup win; avoids parse+resolve).

### 6.3 Startup budget

Interpreted-language startup = process spawn + runtime init + module-graph load + main. Levers, in order of leverage: (1) precompiled bytecode cache hit path with mmap (no text parse — the entire reason bun's --bytecode wins 1.38x), (2) lazy module loading with the dependency graph in the module header (load only what `main` reaches — Go-style), (3) no top-level side effects / no import-time execution (also the hot-reload and cycle requirements — three designs, one rule), (4) single-binary mode for distribution (6.1). The mmap story: map the `dsmod` file read-only, use relative offsets/relocation-free layout (LuaJIT-style prototypes are relocation-light by design; Java's CDS archive is the large-scale proof), optionally share one mapped stdlib image across processes (dyld shared cache pattern).

| System | Startup mechanism | Measured/claimed |
|---|---|---|
| bun --compile --bytecode | embedded precompiled bytecode | 1.38x faster than bundle-only; 2.01x vs `bun run` (peterbe.com) |
| CPython | `.pyc` validated by mtime/hash (PEP 552) | skips tokenize/compile; import machinery still dominates |
| LuaJIT | `-b` bytecode, minimal loader | bytecode files are the shipped artifact for many Lua apps |
| JVM + AppCDS | mmap of pre-linked class metadata | gains scale with loaded-class count (Oracle/Red Hat) |
| DreamScript (target) | mmap dsmod cache + lazy graph + no import-time exec | budget: ~10ms cold for scripts, sub-ms warm |

### 6.4 Naming, targets, wasm, Windows

Binary naming: adopt target-triple suffixes for artifacts (`name-x86_64-pc-windows-msvc.exe`, `name-aarch64-linux-gnu`) as cargo/zig/bun all effectively standardize on triples; for scripts (`run` mode) there is nothing to name. Wasm as distribution: a DreamScript package builds to (a) native bytecode for `ds run`, (b) wasm/wasm-gc for web/embedding, (c) a wasm component (WIT) for typed host embedding (5.3) — MoonBit's one-tree-three-backends via suffix files is the template. Windows UX (author runs Windows + NixOS): PATH management via **shims** (scoop model: a shim dir on PATH, per-tool stubs) rather than symlinks; installer as a single `ds.exe` (self-updating or winget/scoop-listed); avoid requiring developer-mode symlinks in the package store materializer (hardlinks where same-volume, copy otherwise — pnpm's documented Windows fallbacks); keep the cache under `%LOCALAPPDATA%` with `DS_CACHE` override. NixOS: the author dislikes flake ceremony — DreamScript tooling should be a static binary that works without any Nix packaging, while shipping a trivial flake for those who want it; the hash-store design (2.7) makes a Nix package of DreamScript itself nearly automatic (output-hash-friendly, no FOD tricks needed).

## 7. Viable ecosystem combos for DreamScript

### Combo A — "Cargo-style": manifest + lock + src-layout + builtin registry

Shape: `ds.toml` (name, version, deps with semver), generated `ds.lock`, `src/` layout, `ds add/build/test/fmt/info/publish`, a sparse-index registry (Cargo's model), features or target-conditional deps for variants.

Costs to implement: registry hosting + publishing UI is the long pole (mooncakes took ~2 years on a young language, so it is doable); resolver (PubGrub/MVS) and lockfile are well-trodden. Forecloses: content-addressed store mechanics (hash-addressing is demoted to internal cache detail), the author's UPM ambitions (a Cargo clone is per-ecosystem by construction), hash-as-lock simplicity (lockfile + manifest divergence questions). Pain fit: good for familiarity (author knows Cargo), medium on Nix pain (a registry is another thing to mirror), good on Python pain (proper manifest), neutral on hot reload/FFI. This is the fallback combo if the author wants conventional behavior and a registry from day one.

### Combo B — "Hash-store native" (recommended core)

Shape: Zig/Inko hybrid package layer — deps declared as `name = { ver, hash }` (blake3 of content, dependency-hashes included so identity cascades, Zig-style), MVS resolution (Inko/Go), **no separate lockfile** (manifest is the lock), global output-hash store (`$DS_STORE/<blake3>/`), per-project materialization via hardlinks/symlinks into `build/` or a virtual-store dir (pnpm mechanics), bin-first: each release declares prebuilt `dsmod`/native artifacts per triple (verified against store hash) with source-build fallback; FHS materializer as an optional `ds link` command (scoop shims on Windows, profile symlinks on NixOS) for interop with non-DreamScript tooling.

```
$ ds fetch                      # resolve MVS, download, verify hashes -> global store
$ ds build                      # materialize into .ds/deps/ (hardlinks), typecheck+emit IR cache
$ ds link --profile bin         # optional FHS materialization: shims (win) / symlinks (nix) into PATH
```

Costs: store + materializer (~pnpm-lite), hash-cascade scheme (Zig's design is documented), MVS resolver (simple), registry can be deferred to static JSON indexes; the UPM wrapper (any-language scripts, torrent distribution) is explicitly out of the language's critical path and can grow later on the same store. Forecloses: naive semver-range ergonomics (ranges still fine at declaration, but resolution is MVS-max — same as Go/Inko, accepted), auto-upgrade culture (security bumps are explicit manifest edits — Inko documents and accepts this). Pain fit: direct hit on the author's essay (per-package locks, output-hash store, FHS linking, bin-first all present); no flake-follows analog because dependency-hashes cascade automatically; store doubles as build cache (6.2), making `ds run` startup fast; P2P and cross-ecosystem phases remain possible without redesign. This combo also matches the charter's decoupling ethic — fetcher/resolver/store/linker are orthogonal inspectable pieces.

### Combo C — "bun-style script ecosystem": zero-config, cycles allowed, hot reload

Shape: `ds run script.ds` with no manifest (deps resolved from a default index on first import, recorded into a generated manifest on `ds save` — bun/deno pattern), module design per 1.10 (file cycles allowed inside a package, package DAG enforced), ESM-like live bindings but compile-time-checked (no TDZ runtime trap: the checker rejects use-before-init across the graph, which TS cannot do), standard library importable as bare `@std/...` specifiers (no URLs), hot reload per 4.5 (two-slot code model, `ds dev --hot` watching files), REPL per 4.3.

Costs: the two-slot runtime module format must be in the VM from the start (cheap if planned; retrofit is the mistake to avoid), cycle-tolerant whole-package name resolution (Rust-style fixpoint) is real compiler work but bounded per package. Forecloses: per-package parallel typechecking across cycle edges (packages stay acyclic, so cross-package parallelism survives), Python-style import-time side effects (a feature, not a loss), Unison-style hash-addressed definitions. Pain fit: direct hit on Python cross-reference pain and on scripting ergonomics the author loves in TS+bun; hot reload serves big-data iteration loops; interprets-only mode keeps startup sub-10ms.

### Combo D — "Platform-model embeddable": host provides effects, language embeds everywhere

Shape: Roc-pattern capability layer — the VM is a C library (`libds.so`: ~Lua-sized embeddable), platforms are packages exporting effect handlers + host glue (default platform = the `ds` CLI providing std effects), typed boundaries: C ABI handles (5.1/5.2) for native hosts, WIT components for wasm hosts (5.3), effect rows as the platform contract (5.4); backend-specific inline implementations per Koka's `extern import c/cs/js file` pattern (5.5).

Costs: the handle-table embedding API + glue generator is the main compiler work; effect-row-to-WIT lowering adds one mapping table; avoiding Roc's app/platform monolith (per 5.4) keeps this incremental. Forecloses: a fixed rich stdlib owning effects (stdlib becomes one platform among several — a real cultural commitment), zero-effect-surface purity guarantees like Haskell's IO (effects are always platform-relative). Pain fit: makes DreamScript embeddable in Rust/JS/C hosts (scripting target the author cares about), composes with linear types at the boundary (linear handles = exactly-once resource discipline for FFI), and the default-platform choice keeps the scripting story of Combo C intact. D is orthogonal to A/B/C: it can be layered onto whichever package/module combo is chosen, and B's store conveniently distributes platform packages bin-first.

### Combo comparison

| Axis | A Cargo-style | B Hash-store | C bun-style | D Platform-model |
|---|---|---|---|---|
| Implement cost | medium (registry heavy) | medium (store+materializer) | low-medium (runtime slots) | medium (embedding API+WIT) |
| Registry needed day 1 | yes | no (deferred, static index) | no | no |
| Author UPM fit | weak | strong | neutral | neutral |
| Nix pain addressed | partial | strong (hash cascade, bin-first) | neutral | neutral |
| Python pain addressed | strong | strong | strong | neutral |
| Hot reload | neutral | strong (cache) | strong (built in) | strong (handler model) |
| Embeddable scripting | neutral | strong (store = artifacts) | neutral | strong |
| Forecloses | hash-store UPM | range-solver ergonomics | import-time effects | fixed-effects stdlib |

Recommended synthesis: **B + C + D**: hash-store native packages, bun-style script mode with cycle-tolerant modules and a two-slot hot-reload runtime, and a platform-model embeddable VM; Combo A's registry is added later as a thin static index over B's hash-addressed artifacts. This selection fixes each named pain (Nix: hash-cascade locks without lockfiles; Python: cross-references + real interfaces + no import-time execution; `#include`: interfaces are generated and checked; LSP-first: compiler-as-query-library shared by LSP/REPL/hot-reload; embeddable scripting: handle-table C API + wasm components) while keeping every piece independently replaceable, which is the author's stated RISC/LSP design ethic.

## Source notes

Primary local evidence (read-only clones in repo root): `inko/docs/source/guides/packages.md` (manifest format, MVS, checksum-in-manifest), `inko/docs/source/references/structure.md` (src/test layout, multi-binary convention), `inko/docs/source/references/syntax.md` (import grammar, conditional imports, `import extern`), `inko/docs/source/guides/cross-compilation.md` (Zig-as-linker posture), `moonbit-core/AGENTS.md` (block model, `moon fmt/info`, `.mbti` review flow, wbtest/blackbox tests, trait-promotion cycle rule), `moonbit-core/abort/moon.pkg` and `moonbit-core/abort/pkg.generated.mbti` (manifest and generated interface), `moonbit-core/builtin/` (target-suffix files), `luajit/src/lj_bcdump.h` (bytecode dump format), `koka/lib/std/core.kk` and `koka/lib/std/async/legacy/readline.kk` (multi-backend inline FFI), `koka/src/Main/README.md` (compiler/REPL/LSP front-end structure), `koka/src/Interpreter/` (REPL as compiler component).

Web sources: [Zig build system docs](https://ziglang.org/learn/build-system) and [dependencies docs](https://www.ziglang.in/learn/build-system/dependencies), [ziggit build-system discussion](https://ziggit.dev/t/the-zig-build-system-is-under-apreciated/14743), [zig build API difficulty thread](https://www.reddit.com/r/Zig/comments/1jfiwm9/zig_build_system_is_really_difficult_to_grasp), [Zig runner split issue #20981](https://github.com/ziglang/zig/issues/20981), [Unison The Big Idea](https://www.unison-lang.org/docs/the-big-idea), [LWN on Unison](https://lwn.net/Articles/978955), [Unison 1.0](https://www.unison-lang.org/unison-1-0), [Cargo resolver docs](https://doc.rust-lang.org/cargo/reference/resolver.html), [cargo features unification issue #10489](https://github.com/rust-lang/cargo/issues/10489), [pnpm motivation](https://pnpm.io/motivation), [pnpm symlinked structure](https://pnpm.io/symlinked-node-modules-structure), [pnpm global virtual store](https://pnpm.io/global-virtual-store), [Deno 2 announcement](https://deno.com/blog/v2.0), [JSR with Deno](https://jsr.io/docs/with/deno), [deno#33370 import-map subtlety](https://github.com/denoland/deno/issues/33370), [mooncakes.io](https://mooncakes.io), [MoonBit package manager intro](https://www.moonbitlang.com/blog/intro-to-mooncakes), [moon package tour](https://docs.moonbitlang.com/en/latest/toolchain/moon/package-manage-tour.html), [Roc FAQ (platform design)](https://www.roc-lang.org/faq), [rust-analyzer durable incrementality](https://rust-analyzer.github.io/blog/2023/07/24/durable-incrementality.html), [salsa overview](https://salsa-rs.github.io/salsa/overview.html), [rustc-dev-guide queries](https://rustc-dev-guide.rust-lang.org/queries/salsa.html), [rustc-dev-guide diagnostics](https://rustc-dev-guide.rust-lang.org/diagnostics.html), [codespan-reporting](https://docs.rs/codespan-reporting), [Mun hot reloading](https://docs.mun-lang.org/ch04-04-hot-reloading), [mun_runtime](https://docs.rs/mun_runtime), [Hot Reloading in Exile](https://thenumb.at/Hot-Reloading-in-Exile), [Erlang code loading](https://www.erlang.org/doc/system/code_loading.html), [Bun compiler docs](https://bun.com/docs/compiler), [bun --bytecode benchmark](https://www.peterbe.com/plog/trying-bun-compile-to-bytecode), [bun#19498 runtime size](https://github.com/oven-sh/bun/issues/19498), [PEP 3147](https://peps.python.org/pep-3147), [PEP 552](https://peps.python.org/pep-0552), [Oracle class data sharing](https://docs.oracle.com/en/java/javase/17/vm/class-data-sharing.html), [Red Hat AppCDS](https://developers.redhat.com/articles/2024/01/23/speed-java-application-startup-time-appcds), [WIT reference](https://component-model.bytecodealliance.org/design/wit.html), [Component Model WIT.md](https://github.com/WebAssembly/component-model/blob/main/design/mvp/WIT.md), [Python C API intro](https://docs.python.org/3/c-api/intro.html), [Python subinterpreters](https://docs.python.org/3/c-api/subinterpreters.html), [refcount-bug study](https://www.cse.psu.edu/~gxt29/papers/refcount.pdf), [PIL registry](https://www.lua.org/pil/27.3.1.html), [Lua 5.3 manual C API](https://www.lua.org/manual/5.3/manual.html), [Go import-cycle strategies (DoltHub)](https://www.dolthub.com/blog/2025-03-14-go-import-cycle-strategies), [golang#49251](https://github.com/golang/go/issues/49251), [MDN import statement](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Statements/import), [live bindings as concatenation](https://jameshfisher.com/2020/09/28/javascript-live-bindings-are-just-concatenation), [circular dependency deep dive](https://ofriperetz.dev/articles/circular-dependencies-in-javascript-explained), [CMake critique](https://twdev.blog/2021/08/cmake), [CMake vs Bazel in practise](https://keasigmadelta.com/blog/cmake-vs-bazel-in-practise-real-code-real-results/), [Meson comparisons](https://mesonbuild.com/Comparisons.html), [Cangjie docs](https://cangjie-lang.cn/en/docs?url=%2F0.53.13%2Fuser_manual%2Fsource_en%2Ffirst_understanding%2Fbasic.html), [Cangjie overview](https://medium.com/snapp-mobile/cangjie-a-programming-language-from-huawei-fb0f7776be06).
