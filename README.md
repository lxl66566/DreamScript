# DreamScript

DreamScript is a general-purpose interpreted language. The goal is script-language ergonomics with compiled-language performance: run a file directly, catch whole error classes at compile time, and run without a garbage collector.

The core ideas:

- **Set-theoretic types.** Types are sets over nominal tags: `a | b`, `a & b`, `a - b`. Optionals are just `T | none`, so errors, optionals, and enums share one mechanism. Inference failure is a compile error; there is no `any`.
- **Affine by default, linear opt-in.** Bindings are immutable unless declared `var`. Values move at compile time; an opt-in linear layer (`~Copyable`) pins resources like file handles.
- **Perceus memory, no GC.** Precise reference counting with compile-time dup/drop placement and drop-to-zero reuse, plus guaranteed self- and mutual tail calls.
- **Fibers, not async.** Stackful fibers run in mandatory nurseries. Any function can block; there is no async/await coloring.
- **No macros.** Metaprogramming is comptime evaluation of ordinary code, static reflection, and derives.
- **One lowering, tiered execution.** Source lowers once; tier 1 is the bytecode VM running today, the JIT tiers are designed but not started.

Errors are ordinary values — union members of return types, narrowed with `is`, extracted with one handler family (`unwrap_or`, `.ok`, `.err`). No exceptions, no `?.`:

```ts
#[derive(show)]
struct row { name: string, hits: int }

enum parse_fail { bad_number(string) }

fn parse_row(line: string) -> row | parse_fail {
  const parts = line.split(',')
  const name = parts.get(0).unwrap_or("?")              // checked read: t | none
  const parsed = parts.get(1).unwrap_or("x").parse_int()
  if parsed is int {                                      // is narrows the union
    row { name, hits: parsed }                            // if is an expression; { name } puns
  } else {
    bad_number(line)                                      // the error is constructed data
  }
}

fn load(text: string) -> list[row] {
  var rows = list[row]()
  for line in text.split('\n') {
    const r = parse_row(line)
    if r is row {                                         // keep the good members only
      rows.push(r)
    }
  }
  rows
}

fn main() -> () {
  const rows = load("ada,3\ngrace,5\nada,1\nlinus,oops")
  var total = 0
  for r in rows {
    total += r.hits
  }
  println("rows ${rows.len()}, hits ${total}")
  println("first ${first(rows).unwrap_or(row { name: "?", hits: 0 })}")
}
// rows 3, hits 9
// first row { name: ada, hits: 3 }
```

Concurrency is stackful fibers in mandatory nurseries: `spawn` exists only inside a `nursery` block, scope exit joins the children, and any function can block — no async/await, no function coloring:

```ts
fn weight(path: string) -> int {
  const c = path.char_at(5)
  if c is char && c == 'b' {
    len(path) * 37
  } else {
    len(path) * 11
  }
}

fn main() -> () {
  const paths = ["/blog", "/docs", "/faq"]
  var sizes: list[int] = list[int]()
  nursery {
    for p in paths {
      spawn {                                // children run as fibers
        sizes.push(weight(p))
      }
    }
  }                                          // scope exit joins every child
  var total = 0
  for s in sizes {
    total += s
  }
  println("${sizes.len()} pages, ${total} bytes")
}
// 3 pages, 154 bytes
```

See [docs/en/tutorial.md](docs/en/tutorial.md) for more.

## Installation

Download the archive for your platform from [GitHub Releases](https://github.com/lxl66566/DreamScript/releases), unpack it, and put `ds` on your `PATH`.

## Status

The pipeline runs end to end through M7: front end and checker, Perceus memory, fiber nurseries, derives and comptime reflection, packages with a content-addressed store, script mode, REPL, LSP, hot reload. Open: the M7 loading remainder, the wider FFI surface, and the M8 JIT tiers. The exact lowered subset and each milestone's boundary are in [docs/en/status.md](docs/en/status.md).

## The `ds` CLI

| Command          | Purpose                                                                                               |
| ---------------- | ----------------------------------------------------------------------------------------------------- |
| `ds run file.ds` | parse, check, lower, and execute on the tier-1 VM                                                     |
| `ds build`       | compile to a dsmod module (blake3-checked, mmap-loaded); `ds run` loads it back with identical output |
| `ds check`       | static checks alone, `--error-format json` for tools                                                  |
| `ds fmt`         | formatter with zero style options                                                                     |
| `ds save`        | install a package into the content-addressed store                                                    |
| `ds bind`        | bind an installed artifact's native library and report its exported C symbols                         |
| `ds info`        | render a `.dsi` API surface                                                                           |
| `ds dev`         | run with hot reload                                                                                   |
| `ds repl`        | interactive session                                                                                   |
| `ds lsp`         | language server                                                                                       |

Entry points default to `main`.

### Editor integration

`ds lsp` serves the language-server protocol over stdio (hover, rename, auto-import completion); any editor that can drive a stdio LSP server works. Neovim (0.11+):

```lua
vim.lsp.config('ds', { cmd = { 'ds', 'lsp' }, filetypes = { 'ds' } })
vim.lsp.enable('ds')
```

## Repository layout

One Cargo workspace. The pipeline runs ds-parse -> ds-query -> ds-check -> ds-lower -> ds-vm over ds-abi, ds-mod, and ds-diag, with ds-fmt, ds-rt, ds-stdlib, ds-meta, ds-pkg, ds-dsi, ds-lsp, ds-pins, and ds-corpus-path beside them and the `ds` binary in ds-cli. The crate-to-milestone map is in [docs/en/implementation-plan.md](docs/en/implementation-plan.md).

Supporting directories: `conformance/` (the seed corpus, every item with stage expectations and golden outputs; every bug regression lands there), `benchmarks/` (cross-language kernels timed against other interpreters), `examples/`, `fuzz/`.

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
- [docs/en/tutorial.md](docs/en/tutorial.md) — the entire v1 surface in one short tutorial.
- [docs/en/design-decisions.md](docs/en/design-decisions.md) — the authoritative record of all 44 language decisions.
- [docs/en/implementation-plan.md](docs/en/implementation-plan.md) — milestones M0-M9 with acceptance criteria, workspace layout, methodology.
- [docs/en/status.md](docs/en/status.md) — what runs today and the boundary each milestone left.
- [conformance/README.md](conformance/README.md) — how the conformance corpus is organized.
- [archive/](archive/) — frozen stage 1-3 research notes and decision records; read only when the working record is thin on a point.
- [AGENTS.md](AGENTS.md) — the project charter: expectations, philosophy, constraints.

## License

MIT OR Apache-2.0
