# DreamScript crate granularity survey

Read-only survey. No repo file was modified, nothing was built, no test was run. Evidence: every `crates/*/Cargo.toml`, every `src/lib.rs` (module list + doc), per-file line counts, `cargo tree -p <crate> -e normal` closure for `ds-query` and `ds-lsp`, and grep counts of `ds_*::` uses per crate.

Scale: ~160k lines of `src` across 19 crates. Three crates hold 62% of it: `ds-vm` 38.7k, `ds-check` 33.1k, `ds-lower` 28.4k. That is where the granularity question actually lives; the other 16 crates average 3.4k.

## 1. The crate graph

Direction is `dependent -> dependency`. "Consumers" counts normal `[dependencies]` only; dev-only consumers are listed separately because they do not constrain the product API.

| Crate | src lines | Depends on | Normal consumers | Dev consumers |
|---|---|---|---|---|
| ds-diag | 1.8k | (serde/sonic optional) | check, dsi, fmt, lower, meta, mod, parse, pkg, query, stdlib, lsp, cli (12) | pins |
| ds-parse | 7.1k | diag | check, dsi, fmt, lower, meta, query, stdlib, lsp, cli (9) | pins |
| ds-mod | 2.7k | abi, diag | check, dsi, lower, meta, pkg, stdlib, vm, cli (8) | pins |
| ds-check | 33.1k | diag, mod, parse | dsi, lower, meta, query, stdlib, lsp, cli (7) | — |
| ds-abi | 4.6k | — | mod, lower, vm (3) | pins |
| ds-meta | 7.1k | check, diag, **lower**, mod, parse, **vm** | query, stdlib, cli (3) | lower, pins |
| ds-stdlib | 2.1k | check, diag, meta, mod, parse, pkg, dsi? | query, cli, lsp (3) | lower, pins |
| ds-ffi | 1.4k | thiserror | lower, vm, cli (3) | — |
| ds-fmt | 2.8k | diag, parse | dsi, lsp, cli (3) | — |
| ds-lower | 28.4k | abi, check, diag, ffi, mod, parse | meta, cli (2) | stdlib, pins |
| ds-vm | 38.7k | abi, ffi, mod, rt (+rustls, webpki-roots, youpipe) | meta, cli (2) | stdlib, lower, pkg, pins |
| ds-dsi | 3.3k | check, diag, fmt, mod, parse | stdlib (optional), cli (2) | — |
| ds-pkg | 4.7k | diag, mod (+tempfile, blake3) | stdlib, cli (2) | — |
| ds-query | 3.3k | check, diag, meta, parse, stdlib | cli, lsp (2) | — |
| ds-rt | 1.4k | — | vm (1) | — |
| ds-lsp | 8.0k | check, diag, fmt, parse, query, stdlib (+tokio, tower-lsp) | cli (1) | — |
| ds-corpus-path | 62 | — | — | check, fmt, meta, parse, cli (5) |
| ds-pins | 9 | — | — | (nothing; leaf harness, dev-deps on 9 crates) |
| ds-cli | 11.6k | 14 crates | — (bin-only, no lib target) | — |

Observations that drive everything below:

- The graph is acyclic in normal edges. `ds-stdlib <-> ds-lower` and `ds-check -> ds-stdlib` cycles exist only through `[dev-dependencies]`; cargo permits that, but it means neither edge can ever be promoted to a normal dependency, and `ds-lower`'s own doc already leans on the stdlib seam.
- `ds-vm` and `ds-lower` each have exactly **two** normal consumers, one of which is `ds-cli`. `ds-vm`'s other consumer is `ds-meta` — a front-end pass.
- `cargo tree -p ds-lsp -e normal` reaches `ds-vm -> rustls -> ring`, `rustls-webpki`, `webpki-roots`, and `youpipe/youpipe-sys`. A language server compiles a TLS stack and a fork-join scheduler because `ds-lsp -> ds-query -> ds-meta -> ds-vm`.
- The three deliberately thin crates are documented as such and are right: `ds-corpus-path` (62 lines, run-time corpus resolution, dev-only, dependency-free so no crate's test binaries inherit a path resolver), `ds-pins` (9-line lib, its payload is `benches/` + `tests/pin_bake.rs`, kept out of `ds-vm` so the gungraun harness graph never lands in `ds-vm`'s test binaries), `ds-rt` (1.4k, the unsafe memory core, zero deps, one consumer — the plan locks unsafe code to `ds-rt`/`ds-jit` behind the diag layer). Same verdict for `ds-abi` (zero deps, "spec as code", 3 consumers) and `ds-diag` (12 consumers, the bottom of the graph). All five stay as they are.

## 2. Split candidates

### 2a. `ds-meta` -> `ds-meta` (derive + analyze entries) + `ds-comptime` (evaluator)

- **Moves**: `lib.rs`'s `evaluate`, `evaluate_consts`, the slice-fence builders and the reflection/registry setup (lines ~200-700 of `lib.rs`, the parts that use `ds_lower::lower_comptime_slice`, `ds_vm::comptime`, `ds_vm::Platform`, `ds_vm::native`). `derive.rs` (4.9k) and `analyze`/`analyze_owned`/`analyze_served`/`analyze_owned_linked`/`analyze_served_linked` stay: they only touch `ds-check`/`ds-parse`/`ds-diag`.
- **Why the boundary is real and stable**: `derive.rs` has zero references to `ds_vm`/`ds_lower`; the evaluator is a leaf pass whose whole job is "lower a slice, install it into a fenced VM, drive call sites". The consumers already partition exactly along that line: `ds-query` calls only `expand_derives` (one site, `queries.rs:117`), `ds-stdlib` calls only `expand_derives` + `analyze_*`, and only `ds-cli` (`source.rs`, `dev.rs`) and `ds-meta`'s own tests call `evaluate*`. A second consumer of the derive/analyze half (fuzz, a future `ds doc` reflection path, M8's JIT front end) never wants the interpreter.
- **Payoff**: `ds-query` and `ds-lsp` drop `ds-lower`, `ds-vm`, `ds-rt`, `ds-ffi`, `rustls`, `ring`, `rustls-webpki`, `webpki-roots`, and `youpipe*` from their closure. `ds-lsp` becomes a pure front end (`cargo tree` above). Per-package build time and dependency clarity, not shipped-binary size — `ds` is one binary and `ds lsp` needs the evaluator anyway through `ds run`.
- **Cost**: M. The evaluator's error surface (`slice_refusal`, `site_refusal`) uses `ds_check` diagnostic types, so the new crate depends on `check`, `diag`, `parse`, `lower`, `vm` — no new edges, one new crate row in `workspace.dependencies`. `ds-meta`'s `analyze_*` re-export list and `docs/en/implementation-plan.md`'s crate table need one line each.
- **LTO/inlining**: none. The evaluator is a cold compile-time pass; nothing in the hot VM path calls it, so no inlining boundary is created.
- **Cheaper alternative if only build time matters**: a `comptime` feature on `ds-meta` (default on, off in `ds-query`). Rejected as the primary plan because cargo unifies features across a workspace build, so `cargo test`/`cargo build --workspace` would still compile the evaluator into `ds-query`'s graph — the hard crate boundary is the only way the layering holds. Decide by whether "`-p ds-lsp` builds without ring" is a requirement; if it is not, do nothing.

### 2b. `ds-lower` -> hoist the native vocabulary into `ds-abi` (not a split: an ownership move)

`ds-lower/src/native.rs` (1824 lines) is a hand-maintained second copy of the VM's native tables: 123 `Native` rows (name, module, index, result shape, union payload/fail payload/err prefix/classes) and 20 `GenericNative` rows (name, index, evidence arity, params, staging, shape, instances). `ds-vm/src/native.rs` holds the executable tables (84 `builtin_module` entries, `platform_module`'s fixed op table, the comptime module) plus 104 `BUILTIN_*` index constants. The two are kept aligned **only by test assertions** (`ds-lower/src/native.rs` `mod drift`, which calls `ds_vm::native::builtin_module()` / `ds_vm::platform::platform_module()` from the lowerer's test build, and `ds-lower`'s drift tests for `PLATFORM_IO_OK`/`NURSERY_BOUNDARY_*`/`NET_*` discriminants).

- **Moves into `ds-abi`**: the declarative row — `name`, `module`, `index`, `params`, `staging`, result width, evidence arity, generic instances. `ds-abi` already declares itself "spec-as-code ... every item is a data model or a pure function the three consumers must agree on exactly"; the native index assignment is exactly that. `ds-lower` keeps what is genuinely lowering-side (the call-site union spelling: `err_prefix`, `classes` -> ambient member names).
- **Why real and stable**: this is a contract with three prospective implementers (lowerer, tier-1 VM, tier-2/3 JIT at M8), currently enforced by a test that runs one crate's binary against another crate's tables. A test is the wrong place for a locked contract; `ds-abi` is where BL-5 already lives.
- **Payoff**: one source of truth, no drift class, and roughly 1.2-1.5k of the 1.8k mirror lines disappear. `ds-lower` stops needing `ds_vm` in its test build for the table pins (a dev-edge removed).
- **Cost**: M. The VM's `NativeFunction` gains a constructor from an abi row plus its fn pointer; `platform_module`'s fixed dense table must stay index-aligned (it already is; the rows carry their index). Churn: `ds-vm/src/native.rs` table literals, `ds-lower/src/native.rs`, the two drift tests.
- **LTO/inlining**: none. The tables are static data and the entries are already called through function pointers; a data move across crates changes nothing at run time.

### 2c. `ds-cli` -> `ds-driver` (lib) + `ds-cli` (bin) — deferred, not rejected

`ds-cli` is a bin-only crate: 20 modules, 11.6k lines, no `lib.rs`, no `[lib]` target. The compiler pipeline lives inside it (`source.rs` 1.8k = parse -> check -> platform check -> lower -> dsmod; `package.rs` 1.6k = manifest/store/registry package runs; `run_cache.rs` 709 + `dsi_cache.rs` 836 = the CLI-owned cache key folds and atomic writes), interleaved with clap wiring (`main.rs`, the `*Args` structs), interactive surfaces (`repl.rs` 1.7k, `dev.rs` 859, `watch.rs` 466), and pure tooling (`doc.rs`, `info.rs`, `bind.rs`, `test.rs`, `save.rs`, `fmt.rs`).

Audience split by module is clean: **pipeline** (`source`, `package`, `dsi_cache`, `run_cache`, `build`, `run`, `ambient_dsmod`, `shutdown`) vs **presentation** (everything else). The real cost of keeping it merged is visible today: no crate can reuse the compile recipe, so it is re-implemented three times — `fuzz/exec_gen.rs` and `fuzz/vm_gen.rs` (fuzz deps: abi, check, diag, lower, meta, mod, parse, query, vm), plus `crates/ds-lower/tests/*` helpers — and all 53 files in `crates/ds-cli/tests/` drive the binary as a subprocess (`CARGO_BIN_EXE_ds`), which is the slowest possible way to test an in-process compiler.

**Why deferred**: the only consumer of the split today is the bin itself plus fuzz/test reuse. Nothing in the *product* needs it: the embedding story the plan reserves is `ds-ffi`, and `ds run` already exercises the pipeline. Splitting means designing a pub surface over 5k lines of `pub(crate)` code, moving ~30 integration tests off subprocesses, and accepting a new crate row — real churn for reuse that is not yet approved. Do it when a second embedder appears (a `ds-check`-only API for CI, an `ds-bench` driver, or fuzz adopting the crate instead of its own copy). Not before.

### 2d. `ds-vm` -> core + host faces (`net`, `tls`, `proc`, `fsys`, `tz`) — rejected

The size (38.7k; `vm.rs` 13.6k, `native.rs` 6.5k, `object.rs` 4.8k, `net.rs` 2.7k, `proc.rs` 1.5k, `tls.rs` 0.9k) and the dependency weight (rustls + webpki-roots + youpipe, 3 of the crates' 6 deps) make this the most tempting split. It is not a thin boundary:

- The world ops are not a library behind an interface, they are **legs of the dispatch loop**. `vm.rs` references `crate::net::`/`crate::proc::` ~50 times in the park/wake state machine (`NetAttempt::Park`, `ConnectWake`, `FetchWake`, `AcceptWake`, `proc::read_answer`, `net::net_err`), and `native.rs` owns the world state tables (`listeners`, `procs`, `files`, the TLS client/server pins) as fields of the heap state that every native shares.
- `platform.rs` is already the capability seam: `Platform::std` vs `Platform::new(name, &[])` (an empty provides list) is the entire product-level boundary, and the install-time capability validation already forbids an unserved op. A test embedder or the comptime fence gets a core-only VM today by installing nothing. The split would buy **build time only**, and would pay for it with a vtable/trampoline on the park path — a hot path the instruction pins cover (`io_sched`, `proc_park`, `net_io`).
- If build time is the goal, gate `net`/`tls` (and their rustls/webpki deps) behind a `ds-vm` feature with the op table entries falling back to reserved traps, rather than moving modules. Same effect, no seam in the dispatcher.

### 2e. `ds-check` -> algebra + passes — rejected

The tempting cut is "set algebra" (`atom`, `expr`, `universe`, `pooled`, `id_table`, `decls`, `witness` = the type model) vs "pipeline passes" (`resolve` 3.8k, `import` 3.8k, `type_of` 10.6k, `linearity` 2.1k, `effects` 1.7k, `instance_solve`). It fails on two counts: (a) the whole crate is built around **one mutable `Universe`** — `ds-lower`'s own docs state that passing the `Analyzed` bundle as one unit is load-bearing because "a second pass would mint an unrelated universe", so the two halves cannot be used independently; (b) every consumer uses both halves (`ds-lower` reads `AtomKind`/`Universe` *and* `import`/`decls`/`instance_solve`; `ds-dsi` reads `Decls`/`Ty` *and* `effects`; `ds-query` reads `DeclaredRows`/`Linearity` *and* `analyze_resolved`). A split would produce two crates that every dependent must list, with a `pub` boundary where `Ty` handles cross. Reject.

### 2f. `ds-ffi` -> marshal + loader — rejected at this size

Two faces really are distinct: `marshal.rs` + `word.rs` (~550 lines, pure types `Sig`/`Param`/`Arg`/`Ret`/`Answer`/`MAX_PARAMS`, consumed by `ds-lower` and `ds-vm`) vs `sys.rs` + `exports.rs` (~665 lines, dlopen/LoadLibraryW + PE/ELF64 export reading, consumed only by `ds bind`). But the crate is 1.4k lines total; two manifests, two lints blocks and a `workspace.dependencies` row cost more than the boundary buys. Revisit only if the loader face grows a per-platform backend of its own.

## 3. Merge / thin-crate candidates

| Candidate | Verdict |
|---|---|
| `ds-corpus-path` -> merge anywhere | **Keep.** 62 lines, zero deps, dev-only, 5 test-binary consumers; merging would hand every crate's test binaries a path resolver and the corpus rules. |
| `ds-pins` -> merge into `ds-vm` | **Keep.** The 9-line lib is the point: gungraun + `tests/pin_bake.rs` stay out of `ds-vm`'s test graph. |
| `ds-rt` -> merge into `ds-vm` | **Keep.** Locked unsafe boundary, zero deps, and the plan puts all unsafe code behind `ds-rt`'s diag layer. |
| `ds-ffi` -> merge into `ds-vm`/`ds-lower` | **Keep.** Dependency-free loader is what lets `ds bind` avoid a loader crate, and the marshal face is shared by two crates. |
| `ds-abi` -> merge into `ds-mod`/`ds-vm` | **Keep.** Zero-dep spec crate with 3 consumers; also the natural future home of the native vocabulary (2b). |
| `ds-dsi` + `ds-stdlib`'s `ambient_dsi` | **Keep.** Format crate vs data crate; the stdlib embeds an artifact, it does not own the format. |
| `ds-check::corpus` (137 lines) -> `ds-cli` | Minor real leak: the only production consumer of the `// items:` stage parser and the `ds check` exemption policy is `ds-cli/src/check.rs`; it sits in the checker. Cost to move is small but the value is one small module; **defer**. |
| `ds-stdlib` -> `ds-pkg` | **Keep**, but record why: the ambient **is** the `@std` package (it builds `ds_pkg::Manifest`/`Package`/`Version` for the bundle identity), so the edge is layering, not leakage. It does put `tempfile` + `blake3` in the LSP closure; acceptable at this size. |
| `ds-query` -> `ds-stdlib` (hard edge) | **Deferred with a trigger.** `ds-query` embeds "the ambient member's precompiled parse tree" so the parse query serves the blob. The query engine already has an ambient seam (`DeclaredRows`); when M7 makes package config an `InputKey`, the ambient sources should become inputs (callers register them, as the CLI already registers files) and this edge inverts. Doing it now means inventing that input key early. |
| `ds-fmt::signature` vs `ds-cli/info` surface rendering | Already one `pub mod` in `ds-fmt`, shared by `ds-dsi`. **Keep.** |

## 4. Ranked shortlist

Ranked by (real boundary x consumer clarity) / churn. Items 1-2 are worth doing; 3-6 are explicitly deferred with a trigger.

1. **Hoist the native vocabulary into `ds-abi`** (2b). Removes a locked BL-5 contract from a cross-crate test assertion to a data model, deletes ~1.2-1.5k mirror lines, and drops `ds-lower`'s dev edge on `ds-vm`. Effort M, no runtime risk, no LTO note. **Do now.**
2. **Split `ds-meta` into `ds-meta` + `ds-comptime`** (2a). Makes the front end (`ds-query`, `ds-lsp`) independent of the interpreter and TLS stack; `cargo tree -p ds-lsp` currently proves the leak. Effort M, no hot-path seam. **Do now** if "front end builds without ring" is a goal; otherwise a `comptime` feature is the cheap half-measure and the crate split waits.
3. **`ds-cli` -> `ds-driver` lib + thin bin** (2c). Real audience split (pipeline vs presentation) and the compile recipe is already duplicated in fuzz and lower tests, but the only current consumer is the bin. Trigger: a second embedder, or a decision to stop spawning `CARGO_BIN_EXE_ds` in 53 test files. Effort L.
4. **`ds-check::corpus` -> `ds-cli`** (3). Small leak, small value. Effort S. Trigger: next time `ds check`'s exemption logic is touched anyway.
5. **`ds-vm` host-face split** (2d). Rejected as a crate split; if the build cost ever hurts, use a `tls`/`net` feature with trap-fallback table rows. Trigger: a core-only embedder that is not already served by installing an empty `Platform`, or a wasm/browser target.
6. **`ds-query` ambient-as-input inversion** (3). Deferred to M7's package-config input key; it is the honest fix for the `ds-query -> ds-stdlib` edge and it must not be improvised earlier.

Not acted on, for the record: `ds-check` split (one universe; every consumer needs both halves), `ds-ffi` split (1.4k, manifest overhead > payload), and any change to `ds-abi`/`ds-diag`/`ds-parse`/`ds-mod`/`ds-fmt`/`ds-dsi`/`ds-pkg`/`ds-rt`/`ds-query`/`ds-lsp`/`ds-corpus-path`/`ds-pins` granularity.

## 5. Cross-cutting notes

- **Hot inlining boundaries that a split must not cross**: `ds-vm <-> ds-abi` (uop decode/dispatch, `FrameLayout`/`CallPlan`, `slide_plan`), `ds-vm <-> ds-rt` (`rc_inc`/`rc_dec`, `alloc_store`, arena marks), `ds-lower -> ds-check` (`Ty` handles read in the body walk), `ds-check -> ds-diag` (`Span`/`Diagnostic` construction on the error paths). The workspace release profile (`lto = "thin"`, `codegen-units = 1`) preserves cross-crate inlining for ships and for `benches`/`ds-pins`, but **dev-profile `cargo test` runs have no LTO**, so a new crate boundary inside any of these seams costs real time in the test suite. None of the recommended items (1, 2) touches these seams.
- **Intra-crate module size is the cheaper lever** and is a separate problem from crate count: `ds-check/src/type_of/mod.rs` 10.6k lines, `ds-vm/src/vm.rs` 13.6k, `ds-lower/src/func.rs` 9.8k, `ds-lower/src/func/calls.rs` 5.5k, `ds-dsi/src/lib.rs` 2.7k (with an inline `pub mod gates` test block), `ds-lsp/src/lib.rs` 3.9k. Extracting sibling modules inside `type_of/` and `func/` gives most of the readability payoff at zero graph cost; it should be preferred over any crate split where the audience is not actually different.
- **Dev-cycle discipline**: `ds-stdlib <-> ds-lower`, `ds-check -> ds-stdlib`, and `ds-stdlib`'s self dev-dep are legal today only because they are dev edges. Any of them promoted to a normal dependency deadlocks the graph, so new seams should not lean on them.
- `ds-cli/build.rs` folds the source trees of 9 crates into `DS_BUILD_ID` (`build_inputs::BYTECODE_CRATES`: abi, check, meta, diag, lower, mod, parse, query, stdlib) and `RUN_PATH_FILES` names the 7 `ds-cli` sources on the run path. Any crate that shapes module bytes must be added to that list, or the script cache will serve stale artifacts across builds. `ds-vm` is deliberately absent (it executes the artifact, never shapes it), which is also why item 2's new `ds-comptime` crate must be added there while `ds-meta` stays listed.
