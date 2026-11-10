# Benchmarks

Cross-language kernel suite: matched programs in DreamScript and peer scripting languages (python, lua, node), one runner, no committed timings. The suite records correctness goldens and the exact commands; numbers are produced by whoever runs `bench` under an exclusive lock.

## Kernels

Each kernel lives in `kernels/` as `<name>.ds`, `.py`, `.lua`, `.mjs`, plus a committed `<name>.golden` holding the one expected final line. The kernels mirror each other operation for operation; every program prints exactly one `result:` line and that line is identical across all four languages — that identity is the correctness golden.

- `fib` — naive recursive fib(34): call overhead and integer add per call.
- `loops-arith` — 10^7 hot var-mutation iterations of a Lehmer generator (mul, mod, parity branch, accumulator): integer arithmetic and loop dispatch.
- `strings` — build 25k three-digit segments through interpolation and concat (grouped to bound copying), then rescan with char_at, substring, and a char digit fold (the parse native's success-typed union answers no runtime extraction yet, so all four languages fold 100/10/1 positional weights — identical values to parsing): string allocation and the string-op natives.
- `float-loop` — 10^7 f64 iterations of mul/add/compare with a conditional rescale: float slots.
- `lists` — split 20k three-digit pieces once, 120 checked-read passes checksummed through the strings digit fold, and a 32-step concat ladder (~10M element-word copies) re-checksummed in full: the list object surface (checked reads, len, concat copying).
- `closures` — 800k-iteration loops of capturing closures built and called per iteration: a named int factory (one CLOSURE_NEW per pass), a const closure threading the accumulator through every call, a value-position factory capturing through two levels, and a string factory (Ref captures). Captures stay immutable — ds copies captures by value while the peers capture by reference.
- `combinators` — 250 rounds of map/filter/reduce over a 10k int list plus 40 rounds over a 1k string list, all lambdas non-capturing: the CALLG hosted-reentry surface (the VM reenters the interpreter once per element per combinator).
- `containers` — a 15000-element cons ladder (~112M element-word copies), a head/tail drain with a running max (~112M more), and 150 collect copies: the container closes cons/head/tail/collect/first/max.
- `maps` — one 5000-entry map over string keys: fill, 16 checked-read rounds over all keys, 2000 missing-key probes, a key-for-key overwrite pass observing the count per set, and a final re-read: the interim map builder surface (map_new/set/get/len).

Mirroring rules the kernels obey: every integer intermediate stays below 2^47 so ds i64, f64-only JS, and bignum Python compute identical values; division and modulo only ever see non-negative operands, where ds truncating division equals Python/Lua floored division; the float kernel prints an odd fourth (x.25 or x.75), exact in binary and short enough in decimal that Rust, Python, JS shortest-repr and Lua `%.14g` render the same digits. The ds kernels stay inside the currently lowered M3 subset (`fn` declarations, const/var bindings, arithmetic, interpolation with implicit primitive conversion, the lowered string and list natives, if/else, while, UFCS, the generic `len` dispatched on its call-site type evidence, `.ok`/`.err` handlers, capturing closures with immutable captures, the non-capturing-lambda combinators map/filter/reduce, the container closes cons/head/tail/collect/first/max, and the interim map builder calls). Two shapes are excluded by current refusals: a closure into a fn-typed slot (so every combinator lambda is non-capturing) and a mutated captured binding (ds copies captures at CLOSURE_NEW while the peers share them — immutable captures make the two agree).

## Running

```sh
cargo build -p ds-cli --release          # required: ds timings are release-build timings
benchmarks/run.sh check                  # correctness: all kernels, all available interpreters
benchmarks/run.sh bench                  # wall-clock table, one run per kernel per interpreter
```

`check` compares each interpreter's final `result:` line against `<name>.golden` and exits nonzero on any mismatch; a missing interpreter prints a `SKIP` line and is not a failure. The ds binary is found at `../target/release/ds` relative to `benchmarks/` (`.exe` on Windows); set `DS_BIN` to point elsewhere, e.g. at a shared `CARGO_TARGET_DIR` worktree build. Optional arguments filter kernels: `run.sh check fib strings`.

The runner is POSIX sh and runs under git bash on Windows and on Linux. Timing uses `date +%s%N` wall clock, so platforms without GNU date cannot time. Interpreter versions matter: record the versions you benched with (python/python3, lua or lua5.4/5.3, node) alongside any results you keep — the runner does not pin them.

## Rules

`bench` numbers are meaningful only under the repo convention of an exclusive `flock` (benchmarks exclusive, compiles and tests shared) with the machine otherwise idle, and one invocation is one sample: repeat for stability. Timing numbers are never committed — not to this directory, not to docs; the committed artifacts are the kernels, the goldens, the runner, and this method. Differences found this way (for example, `substring` costing O(string length) per call in the tier-1 VM, which dominated the first strings sizing) are recorded as implementation findings elsewhere, not as benchmark tables.
