# DreamScript

DreamScript is a general-purpose interpreted language with hybrid set-theoretic types over nominal tags, affine-by-default values, and guaranteed tail calls, running on precise reference counting with per-frame arenas — no GC. Concurrency is stackful fibers in mandatory nurseries; execution is three planned tiers over one shared lowering, from a typed-register bytecode VM to a copy-and-patch JIT and Cranelift/wasm. The compiler is a Rust Cargo workspace, and every language decision is written down first: one short tutorial rules the whole v1 surface, and implementation follows a milestone plan from locked contracts to 1.0.

## The surface today

This block runs end to end through `ds run` — every construct in it is inside the lowered subset (the final `0` is the echoed unit entry result):

```ts
extern fn str_int (v: int) -> string

fn shout_at(words: list[string], i: int) -> string {
  list_get(words, i).ok((w: string) => w.to_upper()).err((_: none) => "?")
}

fn main() -> () {
  const line = "set types, affine values, no GC"
  const words = split(line, ' ')
  println("line: ${line}")
  println("words: ${str_int(list_len(words))}")
  println("first: ${shout_at(words, 0)}")
  println("tenth: ${shout_at(words, 9)}")

  const all = list_concat(words, split("tail calls", ' '))
  var scanned: int = 0
  var i: int = 0
  while i < list_len(all) {
    const w = list_get(all, i)
    if w is string {
      scanned += 1
    }
    i += 1
  }
  println("scanned: ${str_int(scanned)} of ${str_int(list_len(all))}")
}
```

```text
line: set types, affine values, no GC
words: 6
first: SET
tenth: ?
scanned: 8 of 8
0
```

## Design highlights

- Hybrid set-theoretic types over nominal tags: union, intersection, and difference compose freely, `T?` is `T | none`, and `is` tests narrow types with flow refinement.
- Affine values by default with an opt-in `~Copyable` linear resource layer (`deinit`, phantom states); bindings are immutable unless declared `var`.
- Guaranteed tail-call optimization, self and mutual, on every tier: a tail call is an O(delta) argument slide plus a jump, so the stack never grows at any depth.
- No GC: Perceus precise reference counting with block reuse over per-frame bump arenas, and deterministic destruction for linear resources.
- Concurrency as VM fibers in mandatory nurseries — cooperative cancellation, fork-join parallelism over the linear/immutable substrate, no async function coloring.
- Hash-addressed `dsmod` binary modules (blake3-checked, mmap-loaded) and a two-slot module table designed for hot reload from day one.
- The compiler as a query library: CLI, LSP, REPL, formatter, and hot reload all read the same memoized queries with block-grain invalidation.

## Status

The front end, the checker, and the tier-1 pipeline run: `ds run file.ds` compiles in-process — parse, static check, lowering — and executes on the tier-1 VM, and `ds build` freezes the same compilation to a dsmod module that `ds run` loads back through the mmap loader. The lowered subset: monomorphic `fn` declarations, `const`/`var` with compound assignment, `while` and `if`, `is` narrowing over unions, monomorphic UFCS dot-calls, string interpolation (holes are string-typed; non-strings wrap through the `str_*` natives), char literals, the early exits (`return`, `?`, `.err`, `.ok`), the list surface (`split`, `list_len`, `list_get`, `list_concat`), the console/file/net natives, and guaranteed self-and-mutual tail calls at constant stack. Constructs outside the subset fail with a diagnostic naming the seam they need.

Designed and recorded, not yet running: the M4 memory-pass tail (Perceus dup/drop placement in ds-lower — the ds-rt arenas/RC core has landed), M6 generics, traits, and comptime, M7 modules, packages, FFI, and tooling, and the M8 JIT tiers, with fiber groundwork in flight for M5. Gates: the conformance corpus with golden diagnostics, fuzz targets (parse, check, dsmod reader, query engine) with nextest mirrors in CI, and the cross-language benchmark suite. The ruled surface is [docs/en/tutorial.md](docs/en/tutorial.md); the authoritative decisions are [docs/en/design-decisions.md](docs/en/design-decisions.md); milestone state lives in [docs/en/implementation-plan.md](docs/en/implementation-plan.md#progress-log).

## Repository layout

| Path | Contents |
|---|---|
| `crates/ds-parse` | Lexer, pest grammar, spanned AST for the decided surface |
| `crates/ds-query` | Memoized query engine with block-grain invalidation |
| `crates/ds-fmt` | Block-aware zero-option formatter over the parse |
| `crates/ds-check` | Set-theoretic type algebra, bidirectional typing, linearity, effect rows |
| `crates/ds-lower` | Checked programs to frame layouts, uop bodies, dsmod modules |
| `crates/ds-vm` | Tier-1 VM: uop decoding, unboxed slots, frame-reuse TCO, inline caches, builtins |
| `crates/ds-rt` | Runtime unsafe core: per-frame bump arenas, precise RC, reuse threading |
| `crates/ds-stdlib` | The stdlib ambient: checked declarations registered before user sources |
| `crates/ds-cli` | The `ds` binary: check, run, build |
| `crates/ds-abi` | Locked slot-overlay call ABI: typed frame layouts, CALLT slide, entry descriptors |
| `crates/ds-mod` | dsmod module format: byte-stable writer, validating reader, blake3 hash, mmap loader |
| `crates/ds-diag` | Span model, error-code registry, human and JSON diagnostic rendering |
| `conformance/` | Seed corpus ported to the decided surface, with per-item stage expectations |
| `benchmarks/` | Cross-language kernel suite (ds, python, lua, node) with correctness goldens |
| `fuzz/` | cargo-fuzz targets: parse, check, dsmod reader, query engine |
| `docs/` | Tutorial, decision record, implementation plan |

## Building

Stable Rust (MSRV 1.85) builds and tests the workspace; `cargo fmt` needs the nightly toolchain (`rustfmt.toml` uses unstable options).

```sh
cargo fmt --all                              # nightly
cargo clippy --all-targets -- -D warnings
cargo nextest run
cargo bench --bench vm
```

The cargo-fuzz targets live in the separate `fuzz/` workspace, e.g. `cargo fuzz run fuzz_parse`. The cross-language kernels in `benchmarks/` time a release build and run under POSIX sh (git bash on Windows, Linux):

```sh
cargo build -p ds-cli --release
benchmarks/run.sh check                      # correctness against the goldens, all interpreters
benchmarks/run.sh bench                      # wall-clock table; DS_BIN overrides the ds path
```

## Running

`ds run` executes one module end to end: a `.ds` source compiled in-process, or a compiled `.dsmod` file; `ds check` runs the static story alone. The stdlib ambient registers by default; `--no-ambient` compiles the file alone.

```sh
cargo run -- ds run examples/hello.ds             # parse, check, lower, run
cargo run -- ds run examples/fib.ds               # the tail-recursive variant runs 10^6 deep
cargo run -- ds run examples/net.ds --args 8080   # stage i64 arguments onto the entry
cargo run -- ds run examples/net.ds               # without a staged port: instructions, exit 0
cargo run -- ds build examples/hello.ds -o hello.dsmod
cargo run -- ds run hello.dsmod                   # the built module, identical output
```

Entries default to `main` (`--entry <name>` overrides). A fallible native's unhandled failure prints `io error: <message>` (or `net error:`) and exits 1.

## Documentation

- [docs/en/tutorial.md](docs/en/tutorial.md) — the entire v1 surface in one short tutorial (the explainability-budget artifact).
- [docs/en/design-decisions.md](docs/en/design-decisions.md) — the authoritative record of all 44 language decisions.
- [docs/en/implementation-plan.md](docs/en/implementation-plan.md) — milestones M0-M9, progress log, and status table.
- [AGENTS.md](AGENTS.md) — the project charter: expectations, philosophy, constraints.
- [old-README.md](old-README.md) — the author's historical vision document, kept for background.

## License

Dual-licensed under MIT OR Apache-2.0 at your option; see [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).
