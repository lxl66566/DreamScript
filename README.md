# DreamScript

DreamScript is a general-purpose interpreted language with hybrid set-theoretic types over nominal tags, affine-by-default values, and guaranteed tail calls, running on precise reference counting with per-frame arenas — no GC. Concurrency is stackful fibers in mandatory nurseries; execution is three planned tiers over one shared lowering, from a typed-register bytecode VM to a copy-and-patch JIT and Cranelift/wasm. The compiler is a Rust Cargo workspace, and every language decision is written down first: one short tutorial rules the whole v1 surface, and implementation follows a milestone plan from locked contracts to 1.0.

## The surface today

This block runs end to end through `ds run` — every construct in it is inside the lowered subset (the final `0` is the echoed unit entry result):

```ts
fn shout_at(words: list[string], i: int) -> string {
  list_get(words, i).ok((w: string) => w.to_upper()).err((_: none) => "?")
}

trait shouty {
  fn shout(self) -> string
}
impl shouty for string {
  fn shout(self) -> string { to_upper(self) }
}

fn main() -> () {
  const line = "set types, affine values, no GC"
  const words = split(line, ' ')
  println("line: ${line}")
  println("words: ${len(words)}")
  println("first: ${shout_at(words, 0)}")
  println("tenth: ${shout_at(words, 9)}")
  println("shouted: ${line.shout()}")

  const all = list_concat(words, ["tail", "calls"])
  var scanned: int = 0
  for i in 0..len(all) {
    const w = list_get(all, i)
    if w is string {
      scanned += 1
    }
  }
  println("scanned: ${scanned} of ${len(all)}")

  const xs = [1, 2, 3]
  const doubled = xs.map((x: int) => x * 2)
  println("sum: ${reduce(doubled, 0, (a: int, b: int) => a + b)}")
}
```

```text
line: set types, affine values, no GC
words: 6
first: SET
tenth: ?
shouted: SET TYPES, AFFINE VALUES, NO GC
scanned: 8 of 8
sum: 12
0
```

The lowered subset behind that block: `const`/`var` bindings with compound assignment, `fn` declarations and lambdas (capturing closures included), set-algebra types with `T?` and `is` narrowing, enum blocks and struct construction, list literals with the `list[t]()` constructor and `list_push`, `map[k, v]()` builder calls, anonymous tuples with destructuring, `for x in a..b`/`a..=b` and `for x in xs` over lists with `in` membership over ranges and lists (map iteration and non-list membership refuse with a diagnostic), `while`/`if`/`match`, the combinator surface (`map`/`filter`/`reduce` and the container closes `cons`/`head`/`tail`/`collect`/`first`/`max`), the early exits `return`/`?`/`.ok`/`.err` with the extraction family (`unwrap_or`, `unwrap_or_else`, `ok_or`, `map_err`), string interpolation over printable primitives, char literals, script-entry top-level statements, and guaranteed self-and-mutual tail calls at constant stack. The trait surface lowers end to end: impl member bodies compile under mangled identity and statically-solved trait call sites dispatch through CALLG with an instance-id evidence word (generic impl call sites check clean and refuse at lowering naming the runtime-evidence seam). Declared `deinit` blocks on `~Copyable` structs execute at the value's unique death — scope exit, early return, `?`, overwrite, frame exit. Constructs outside the subset fail with a diagnostic naming the seam they need.

## The `ds` CLI

| Command | What it does |
|---|---|
| `ds run <file>` | Executes one module — a `.ds` source compiled in-process, or a `.dsmod` loaded through the mmap loader. `--entry <name>` (default `main`), `--args <i64>...` staged onto the entry's parameter slots, `--no-ambient` to compile without the stdlib ambient. |
| `ds build <file> -o <module>` | Compiles a `.ds` source to a dsmod module whose header carries the source's blake3 hash; `ds run` loads it back with identical output. `--no-ambient` as above. |
| `ds check <files...>` | Runs the static story alone — parse, resolve, bidirectional typing with refinement, linearity, effect rows. `--no-ambient`; `--error-format json` renders one JSON object per diagnostic. |
| `ds fmt <files...>` | Rewrites `.ds` sources in place through the zero-option formatter (no style flags exist). `--check` reports would-change files without writing and exits 1. |

The stdlib ambient registers before user sources: the string/list/map natives, the generic vocabulary (`len`, `str`, the combinators, the container closes, the extraction family), and the failure taxonomy the fallible reads answer — `read_file -> string | io_err`, `parse_int -> int | parse_err`, `fetch -> response | net_err` with `net_err` the two-case `timeout | refused` — plus the `config` struct and the `urls` const. An unhandled fallible-native failure prints `io error: <message>` (or `net error:`) and exits 1; a handled one re-spans the platform's two-word span into the call-site union (`.ok`/`.err`/`?` extraction over the success-typed ambient) and runs.

## Design highlights

- Hybrid set-theoretic types over nominal tags: union, intersection, and difference compose freely, `T?` is `T | none`, and `is` tests narrow types with flow refinement.
- Affine values by default with an opt-in `~Copyable` linear resource layer (`deinit`, phantom states); bindings are immutable unless declared `var`.
- Guaranteed tail-call optimization, self and mutual, on every tier: a tail call is an O(delta) argument slide plus a jump, so the stack never grows at any depth.
- No GC: Perceus precise reference counting with block reuse over per-frame bump arenas, and deterministic destruction for linear resources.
- Concurrency as VM fibers in mandatory nurseries — cooperative cancellation, fork-join parallelism over the linear/immutable substrate, no async function coloring.
- Hash-addressed `dsmod` binary modules (blake3-checked, mmap-loaded) and a two-slot module table designed for hot reload from day one.
- The compiler as a query library: CLI, LSP, REPL, formatter, and hot reload all read the same memoized queries with block-grain invalidation.

## Status

The front end, the checker, and the tier-1 pipeline run: `ds run` compiles in-process — parse, static check, Perceus dup/drop placement, lowering — and executes on the tier-1 VM — trait call sites dispatch through CALLG over impl member bodies, and declared `deinit` blocks run at the value's unique death; the M5 substrate (fiber pool, nursery join with first-failure cancellation, boundary error unioning) is landed, with the in-language `nursery`/`spawn` surface waiting on its round. Hardening rounds have pinned the locked contracts to their encoders — the uop field map (a sentinel assertion per variant), the dsmod reader, DefId packing, trap attribution, `.err` member re-spanning — and the ambient failure taxonomy gives the fallible reads real success unions. Designed and recorded, not yet running: M6 derives and comptime (the open trait seam is the generic-impl lowerer half: impl bodies in the plan plus runtime-resolved evidence through the FuncRef instance registry), M7 modules, packages, FFI, and tooling, and the M8 JIT tiers. Gates: the conformance corpus with golden diagnostics, fuzz targets (parse, check, checker generator, dsmod reader, query engine, VM program generator) with nextest mirrors in CI, and the cross-language benchmark suite. The ruled surface is [docs/en/tutorial.md](docs/en/tutorial.md); the authoritative decisions are [docs/en/design-decisions.md](docs/en/design-decisions.md); milestone state lives in [docs/en/implementation-plan.md](docs/en/implementation-plan.md#milestone-status).

## Repository layout

| Path | Contents |
|---|---|
| `crates/ds-parse` | Lexer, pest grammar, spanned AST for the decided surface |
| `crates/ds-query` | Memoized query engine with block-grain invalidation |
| `crates/ds-fmt` | Block-aware zero-option formatter over the parse |
| `crates/ds-check` | Set-theoretic type algebra, bidirectional typing, linearity, effect rows |
| `crates/ds-lower` | Checked programs to frame layouts, uop bodies, dsmod modules, plus the Perceus dup/drop placement pass |
| `crates/ds-vm` | Tier-1 VM: uop decoding, unboxed slots, frame-reuse TCO, CALLS/CALLG dispatch, inline caches, the RC object runtime, fibers and nurseries, builtins |
| `crates/ds-rt` | Reference unsafe core: per-frame bump arenas, precise RC, reuse threading (the consumed runtime lives in ds-vm's object module) |
| `crates/ds-stdlib` | The stdlib ambient: checked declarations and the failure taxonomy, registered before user sources |
| `crates/ds-cli` | The `ds` binary: check, run, build, fmt |
| `crates/ds-abi` | Locked slot-overlay call ABI: typed frame layouts, CALLT slide, entry descriptors |
| `crates/ds-mod` | dsmod module format: byte-stable writer, validating reader, blake3 hash, mmap loader |
| `crates/ds-diag` | Span model, error-code registry, human and JSON diagnostic rendering |
| `conformance/` | Seed corpus ported to the decided surface, with per-item stage expectations |
| `benchmarks/` | Cross-language kernel suite (ds, python, lua, node) with correctness goldens |
| `fuzz/` | cargo-fuzz targets: parse, check, checker generator, dsmod reader, query engine, VM program generator |
| `docs/` | Tutorial, decision record, implementation plan |

## Building and testing

Stable Rust (MSRV 1.85) builds and tests the workspace; `cargo fmt` needs the nightly toolchain (`rustfmt.toml` uses unstable options).

```sh
cargo fmt --all                              # nightly
cargo clippy --all-targets -- -D warnings
cargo nextest run
cargo bench -p ds-vm --bench vm              # tier-1 VM (criterion)
cargo bench -p ds-check --bench check_perf   # checker pipeline
```

The cargo-fuzz targets live in the separate `fuzz/` workspace, e.g. `cargo fuzz run fuzz_parse`. The cross-language kernels in `benchmarks/` time a release build and run under POSIX sh (git bash on Windows, Linux):

```sh
cargo build -p ds-cli --release
benchmarks/run.sh check                      # correctness against the goldens, all interpreters
benchmarks/run.sh bench                      # wall-clock table; DS_BIN overrides the ds path
```

## Running

```sh
cargo run -- ds run examples/hello.ds             # parse, check, lower, run
cargo run -- ds run examples/fib.ds               # the tail-recursive variant runs 10^6 deep
cargo run -- ds run examples/combinators.ds       # map/filter/reduce driving lambdas
cargo run -- ds run examples/script_entry.ds      # entry-file top-level statements
cargo run -- ds run examples/net.ds               # without a staged port: instructions, exit 0
cargo run -- ds run examples/net.ds --args 8080   # stage i64 arguments onto the entry
cargo run -- ds build examples/hello.ds -o hello.dsmod
cargo run -- ds run hello.dsmod                   # the built module, identical output
```

Entries default to `main` (`--entry <name>` overrides). Every example runs green, including the fallible reads in `files.ds` and `mutation.ds`.

## Documentation

- [docs/en/tutorial.md](docs/en/tutorial.md) — the entire v1 surface in one short tutorial (the explainability-budget artifact).
- [docs/en/design-decisions.md](docs/en/design-decisions.md) — the authoritative record of all 44 language decisions.
- [docs/en/implementation-plan.md](docs/en/implementation-plan.md) — milestones M0-M9, the status table, standing pitfalls, and the ranked next-round queue.
- [AGENTS.md](AGENTS.md) — the project charter: expectations, philosophy, constraints.
- [old-README.md](old-README.md) — the author's historical vision document, kept for background.

## License

Dual-licensed under MIT OR Apache-2.0 at your option; see [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).
