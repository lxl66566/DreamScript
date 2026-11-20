# Benchmarks

Cross-language kernel suite plus a ds cold-start harness: matched programs in DreamScript and peer scripting languages (python, lua, node), one runner, no committed timings. The suite records correctness goldens and the exact commands; numbers are produced by whoever runs `bench` under an exclusive lock.

## Kernels

Each kernel lives in `kernels/` as `<name>.ds`, `.py`, `.lua`, `.mjs`, plus a committed `<name>.golden` holding the one expected final line. The kernels mirror each other operation for operation; every program prints exactly one `result:` line and that line is identical across all four languages — that identity is the correctness golden. The `.ds` kernels sit at `ds fmt`'s canonical form (all eleven verified unchanged; the CI ds-fmt job gates them alongside examples/), so run the formatter after editing one.

- `fib` — naive recursive fib(34): call overhead and integer add per call.
- `loops-arith` — 10^7 hot var-mutation iterations of a Lehmer generator (mul, mod, parity branch, accumulator): integer arithmetic and loop dispatch.
- `strings` — build 25k three-digit segments through interpolation and concat (grouped to bound copying), then rescan with char_at, substring, and parse_int (its int | parse_err answer extracted through the .err handler): string allocation and the string-op natives.
- `float-loop` — 10^7 f64 iterations of mul/add/compare with a conditional rescale: float slots.
- `lists` — split 20k three-digit pieces once, 120 checked-read passes checksummed through parse_int, and a 32-step concat ladder (~10M element-word copies) re-checksummed in full: the list object surface (checked reads, len, concat copying).
- `closures` — 800k-iteration loops of capturing closures built and called per iteration: a named int factory (one CLOSURE_NEW per pass), a const closure threading the accumulator through every call, a value-position factory capturing through two levels, and a string factory (Ref captures). Captures stay immutable — ds copies captures by value while the peers capture by reference.
- `combinators` — 250 rounds of map/filter/reduce over a 10k int list plus 40 rounds over a 1k string list, all lambdas non-capturing: the CALLG hosted-reentry surface (the VM reenters the interpreter once per element per combinator).
- `containers` — a 15000-element cons ladder (~112M element-word copies), a head/tail drain with a running max (~112M more), and 150 collect copies: the container closes cons/head/tail/collect/first/max.
- `maps` — one 5000-entry map over string keys: fill, 16 checked-read rounds over all keys, 2000 missing-key probes, a key-for-key overwrite pass observing the count per set, and a final re-read: the interim map builder surface (map_new/set/get/len).
- `records` — 20001 four-field records (two ints, a float, a string Ref) staged into a `list[record]` by `list_push` at the element stride, 45 read passes walking all elements by checked `list_get` with `.ok`/`.err` extraction and field projection, a 2000 missing-index probe over the none side, and a head pin: the native result span channel for whole record word spans.
- `nursery` — 200k nursery cycles each opening a fresh scope that spawns two children (one computing on a copied-in int capture, one touching a string Ref capture so the spawn-seam copy-in acquire rides), joined at scope exit; every 100th cycle a spawned child opens its own nursery while the enclosing drive still owns one (a cross-level join, kept at 1%), and one final walk re-reads the shared out-list by checked `list_get` with `.ok`/`.err` extraction: the spawn/join fiber path.

Mirroring rules the kernels obey: every integer intermediate stays below 2^47 so ds i64, f64-only JS, and bignum Python compute identical values; division and modulo only ever see non-negative operands, where ds truncating division equals Python/Lua floored division; the float kernel prints an odd fourth (x.25 or x.75), exact in binary and short enough in decimal that Rust, Python, JS shortest-repr and Lua `%.14g` render the same digits. The ds kernels stay inside the currently lowered M3 subset (`fn` declarations, const/var bindings, arithmetic, interpolation with implicit primitive conversion, the lowered string and list natives, if/else, while, UFCS, the generic `len` dispatched on its call-site type evidence, `.ok`/`.err` handlers, capturing closures with immutable captures, the combinators map/filter/reduce, the container closes cons/head/tail/collect/first/max, the interim map builder calls, and struct-typed list elements staged and read back whole through the native result span channel — list_push/list_get/head over records with `.ok`/`.err` extraction and field projection, no copy closes or combinators over wider elements, the `nursery` blocks with `spawn` children (int captures copied in and string Ref captures acquired at the spawn seam), a child-owned nested nursery joining across levels, and the value match over a union refutable label (net_err timeout/refused)). One shape is excluded: a mutated captured binding (ds copies captures at CLOSURE_NEW while the peers share them — immutable captures make the two agree). The combinator lambdas stay non-capturing to keep closure build traffic out of the CALLG reentry measurement.

## Running

```sh
cargo build -p ds-cli --release          # required: ds timings are release-build timings
benchmarks/run.sh check                  # correctness: all kernels, all available interpreters
benchmarks/run.sh bench                  # wall-clock table, one run per kernel per interpreter
```

`check` compares each interpreter's final `result:` line against `<name>.golden` and exits nonzero on any mismatch; a missing interpreter prints a `SKIP` line and is not a failure. The ds binary is found at `../target/release/ds` relative to `benchmarks/` (`.exe` on Windows); set `DS_BIN` to point elsewhere, e.g. at a shared `CARGO_TARGET_DIR` worktree build. Optional arguments filter kernels: `run.sh check fib strings`. Every ds invocation forces the cold path (`--no-cache`): the suite measured the full pipeline before the script-mode dsmod cache existed, the timings stay comparable, and no run inherits or writes a user cache directory.

The runner is POSIX sh and runs under git bash on Windows and on Linux. Every interpreter invocation runs under a per-invocation `timeout` (600 s, a passthrough where `timeout` is absent), so a hung interpreter fails its invocation instead of wedging the round. Timing uses `date +%s%N` wall clock; a one-time probe refuses the bench loudly when `date` answers without nanoseconds (BSD prints a literal `N`; the portable fallback would be `date +%s` whole seconds). Interpreter versions matter: record the versions you benched with (python/python3, lua or lua5.4/5.3, node) alongside any results you keep — the runner does not pin them.

## Startup

`startup.sh bench` times three fixed ds cold-start stages plus the script-cache hit stage as medians over N runs (default 30; `-n` to change, `--json` for one machine-readable line per metric). `floor` (`ds --version`) isolates the process + allocator floor under the harness; `trivial` (`startup/trivial.ds`, three arithmetic statements and one print) adds front-end + check + lower + VM entry; `small-real` (`startup/small-real.ds`, a list literal, a map through the interim constructor, one reduce) adds the first-allocations cost; `warm` runs the trivial fixture served from the script-mode dsmod cache (a private `DS_CACHE` root primed untimed), so `warm - floor` is the artifact load + install + execute cost. The cold stages force `--no-cache`, so they never inherit or write a user cache and stay comparable with the pre-cache suite. The adjacent deltas are the diagnostic: `trivial - floor` is compiler-pipeline entry cost, `small-real - trivial` is first-allocation cost, `trivial - warm` is what the cache deletes of the front end. On Windows git bash the floor includes the msys fork/exec overhead — compare deltas across stages, not the floor against other platforms.

```sh
benchmarks/startup.sh bench                 # median/min/p95 table, ms
DS_BIN=<ds> benchmarks/startup.sh bench --json -n 50
```

DS_BIN resolution and the `date +%s%N` wall clock follow run.sh; the fixtures sit at `ds fmt`'s canonical form. Every timed run's `result:` line is checked against the fixture's `.golden` and any mismatch or nonzero exit fails loudly — a wrong-result benchmark is worse than none. Numbers are never committed: performance rounds record the startup medians in the commit message alongside the other gate numbers.

## Rules

`bench` numbers are meaningful only under the repo convention of an exclusive `flock` (benchmarks exclusive, compiles and tests shared) with the machine otherwise idle, and one invocation is one sample: repeat for stability. Timing numbers are never committed — not to this directory, not to docs; the committed artifacts are the kernels, the goldens, the runner, and this method. Differences found this way (for example, `substring` costing O(string length) per call in the tier-1 VM, which dominated the first strings sizing) are recorded as implementation findings elsewhere, not as benchmark tables.
