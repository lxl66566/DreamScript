# DreamScript

DreamScript is a general-purpose interpreted language with static linear (affine) types, immutable-by-default bindings, and near-compiled performance: code runs on a typed-register bytecode VM whose calling convention guarantees tail calls, with a tiered JIT planned over the same lowering. The compiler is implemented in Rust as a Cargo workspace, and every language decision is written down first — implementation follows a milestone plan from locked contracts to 1.0.

## Design highlights

- Hybrid set-theoretic types over nominal tags: union, intersection, and difference compose freely, `T?` is `T | none`, and `is` tests narrow types with flow refinement.
- Affine values by default with an opt-in `~Copyable` linear resource layer (`deinit`, phantom states); bindings are immutable unless declared `var`.
- Guaranteed tail-call optimization, self and mutual, on every tier: a tail call is an O(delta) argument slide plus a jump, so the stack never grows at any depth.
- No GC: Perceus precise reference counting with block reuse over per-frame bump arenas, and deterministic destruction for linear resources.
- Concurrency as VM fibers in mandatory nurseries — cooperative cancellation, fork-join parallelism over the linear/immutable substrate, no async function coloring.
- Hash-addressed `dsmod` binary modules (blake3-checked, mmap-loaded) and a two-slot module table designed for hot reload from day one.
- The compiler as a query library: CLI, LSP, REPL, formatter, and hot reload all read the same memoized queries with block-grain invalidation.

## Status

Implementation is under way. The M0 contracts have landed — the locked slot-overlay call ABI (`ds-abi`), the dsmod format (`ds-mod`), diagnostics (`ds-diag`), and a skeleton VM executing hand-assembled fixtures, with mutual tail recursion at depth 10^7 running at constant stack — and M1 has partially landed (conformance corpus, `ds-query` engine; the parser is in flight). See the progress log and milestone status table in [docs/en/implementation-plan.md](docs/en/implementation-plan.md#progress-log).

## Repository layout

| Path | Contents |
|---|---|
| `crates/ds-diag` | Span model, error-code registry, human and JSON diagnostic rendering |
| `crates/ds-abi` | The locked slot-overlay call ABI: typed frame layouts, CALLT semantics, two-word entry descriptors |
| `crates/ds-mod` | The dsmod module format: strict reader, byte-stable writer, blake3-checked hash, mmap loader |
| `crates/ds-query` | Memoized query engine with block-grain invalidation |
| `crates/ds-vm` | Skeleton VM proving the ABI with hand-assembled uop fixtures; criterion bench |
| `conformance/` | Seed conformance corpus ported from the design record, with per-item stage expectations |
| `fuzz/` | cargo-fuzz targets (dsmod reader) |
| `docs/` | Design record and implementation plan |

## Building

Stable Rust (MSRV 1.85) builds and tests the workspace; `cargo fmt` needs the nightly toolchain (`rustfmt.toml` uses unstable options).

```sh
cargo fmt --all                              # nightly
cargo clippy --all-targets -- -D warnings
cargo nextest run
cargo bench --bench vm
```

The cargo-fuzz targets live in the separate `fuzz/` workspace, e.g. `cargo fuzz run fuzz_read_dsmod`.

## Running

`ds run` executes one module end to end: a `.ds` source compiled in-process (parse, static check, lowering), or a compiled `.dsmod` file. `ds build` compiles a source to a module that `ds run <file.dsmod>` and the mmap loader read back. The stdlib ambient registers by default; `--no-ambient` compiles the file alone.

```sh
cargo run -- ds run examples/hello.ds             # source mode: parse, check, lower, run
cargo run -- ds run examples/fib.ds               # the tail-recursive variant runs 10^6 deep
cargo run -- ds run examples/net.ds --args 8080   # stage i64 arguments onto the entry
cargo run -- ds run examples/net.ds               # without a staged port: instructions, exit 0
cargo run -- ds build examples/hello.ds -o hello.dsmod
cargo run -- ds run hello.dsmod                   # the built module, identical output
```

Entries default to `main` (`--entry <name>` overrides). The lowered subset: monomorphic `const` arrow functions, the builtin natives (console, scalar conversions, file IO, TCP), early exits (`or return`, `?`, `.err`), string interpolation, and guaranteed tail calls; constructs outside it fail with a diagnostic naming the seam they need. A fallible native's unhandled failure prints `io error: <message>` (or `net error:`) and exits 1.

## Documentation

- [docs/en/design-decisions.md](docs/en/design-decisions.md) — the authoritative record of all 42 language decisions.
- [docs/en/implementation-plan.md](docs/en/implementation-plan.md) — milestones M0-M9, progress log, and status table.
- [AGENTS.md](AGENTS.md) — the project charter: expectations, philosophy, constraints.
- [old-README.md](old-README.md) — the author's historical vision document, kept for background.

## License

Dual-licensed under MIT OR Apache-2.0 at your option; see [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).
