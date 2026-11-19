# DreamScript

DreamScript is a general-purpose interpreted language with static set-theoretic types over nominal tags, affine values with an opt-in linear resource layer, guaranteed self-and-mutual tail calls, and precise reference counting with no GC. Concurrency is stackful fibers in mandatory nurseries — no async function coloring. Execution is planned as tiers over one shared lowering: the tier-1 bytecode VM runs today, the JIT tiers are designed and not started. The compiler is a Rust Cargo workspace; every language decision is written down before it is implemented, and one short tutorial rules the entire v1 surface.

## What runs today

`ds run file.ds` parses, checks, lowers, and executes in-process on the tier-1 VM; `ds build` compiles to a dsmod module (blake3-checked, mmap-loaded) that `ds run` loads back with identical output. The pipeline runs end to end through M7 — front end and checker, Perceus memory, fiber nurseries, derives and comptime reflection, packages with the content-addressed store, script mode, REPL, LSP, hot reload; the M7 loading remainder, FFI, and the M8 JIT tiers stay open. The exact lowered-subset boundary and the diagnostic each refused construct answers live in [docs/en/status.md](docs/en/status.md); the whole v1 surface is [docs/en/tutorial.md](docs/en/tutorial.md).

Everything in this block runs end to end through `ds run`:

```ts
fn fetch_body(u: string) -> string | net_err {
  if u == "down.example" {
    refused
  } else {
    "body of ${u}"
  }
}

fn main() -> () {
  match fetch_body("ds.dev") {
    timeout => println("timeout"),
    refused => println("refused"),
    body => println(body),
  }
}
```

## The `ds` CLI

`run`, `build`, `check` (the static story alone, with `--error-format json`), `fmt` (zero style options), `save` (install a package into the content-addressed store), `info` (render a `.dsi` API surface), `dev` (hot reload), `repl`, `lsp`. Entries default to `main`.

## What the language refuses

No macros — comptime evaluation of ordinary code and static reflection instead. No inheritance — structs compose, traits carry behavior. No `any` fallback — inference failure is a compile error. The rulings behind every refusal are recorded in [docs/en/design-decisions.md](docs/en/design-decisions.md).

## Repository layout

One Cargo workspace; consumption runs ds-parse -> ds-query -> ds-check -> ds-lower -> ds-vm over ds-abi, ds-mod, and ds-diag, with ds-fmt, ds-rt, ds-stdlib, ds-meta, ds-pkg, ds-dsi, ds-lsp, ds-pins, and ds-corpus-path beside them and the `ds` binary in ds-cli. Supporting directories: `conformance/` (the seed corpus, every item with stage expectations and golden outputs), `fuzz/`, `benchmarks/` (cross-language kernels), `examples/`. The crate-to-milestone map is section 2 of [docs/en/implementation-plan.md](docs/en/implementation-plan.md).

## Building and testing

Stable Rust (MSRV 1.95) builds and tests the workspace; `cargo fmt` needs the nightly toolchain (unstable rustfmt options).

```sh
cargo fmt --all                                                    # nightly
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace --all-features
```

The cargo-fuzz targets live in the separate `fuzz/` workspace (e.g. `cargo fuzz run fuzz_parse`); `benchmarks/run.sh` checks and times a release build against the other interpreters under POSIX sh. Parallel worktrees each keep a private target dir — a shared one overwrites same-package artifacts across lanes.

## Documentation

- [docs/README.md](docs/README.md) — the documentation index: which doc to read for what.
- [docs/en/tutorial.md](docs/en/tutorial.md) — the entire v1 surface in one short tutorial (the explainability-budget artifact).
- [docs/en/design-decisions.md](docs/en/design-decisions.md) — the authoritative record of the 44 language decisions.
- [docs/en/implementation-plan.md](docs/en/implementation-plan.md) — milestones M0-M9 with acceptance criteria, workspace layout, methodology.
- [docs/en/status.md](docs/en/status.md) — what runs today and the boundary each milestone left.
- [AGENTS.md](AGENTS.md) — the project charter: expectations, philosophy, constraints.

## License

Dual-licensed under MIT OR Apache-2.0 at your option; see [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).
