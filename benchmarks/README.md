# Benchmarks

Cross-language kernel suite plus a ds cold-start harness: matched programs in ds, python, lua, and node under one POSIX-sh runner, with committed correctness goldens. Committed timing tables are dated single-sample snapshots (see Results); the raw rows live in `results/`.

## Kernels

Each kernel sits in `kernels/` as `<name>.ds`, `.py`, `.lua`, `.mjs`, plus a `<name>.golden` holding the one expected final `result:` line, identical across every language. The `.ds` files sit at `ds fmt`'s canonical form (the CI ds-fmt job gates them), so run the formatter after editing one.

| Kernel | Measures |
|---|---|
| `fib` | naive recursive fib(34): call overhead and integer add per call |
| `loops-arith` | 10^7 hot iterations of a Lehmer generator: integer arithmetic and loop dispatch |
| `float-loop` | 10^7 f64 iterations of mul/add/compare with a conditional rescale: float slots |
| `strings` | 25k interpolated segments built, then a char_at scan, substring cuts, parse_int: string allocation and the string natives |
| `lists` | one split, 120 checked-read passes, a 32-step concat ladder (~10M word copies): the list object surface |
| `closures` | 800k iterations building and calling capturing closures: CLOSURE_NEW and Ref captures |
| `combinators` | 250 rounds of map/filter/reduce over a 10k list: CALLG hosted reentry per element |
| `containers` | a 15000-element cons ladder (~112M word copies) and a first/tail drain (~112M more): cons/first/tail/collect |
| `maps` | one 5000-entry string-keyed map: fill, 16 read rounds, 2000 missing probes, an overwrite pass |
| `records` | 20001 four-field records staged at the element stride, 45 checked-read passes with field projection: the record span channel |
| `nursery` | 200k spawn/join cycles of two children each, 1% cross-level joins: the fiber path |

The kernels mirror each other operation for operation. Every integer intermediate stays below 2^47 so ds i64, f64-only JS, and bignum Python agree; division and modulo see only non-negative operands (truncating equals floored there); the float kernel prints an odd fourth so every language's shortest representation renders the same digits; captures stay immutable (ds copies at CLOSURE_NEW while the peers share); the combinator lambdas stay non-capturing so closure traffic stays out of the CALLG measurement.

## Running

```sh
cargo build -p ds-cli --release          # ds timings are release timings
benchmarks/run.sh check                  # all kernels, all available interpreters, vs goldens
benchmarks/run.sh bench                  # wall-clock table, one run per kernel per interpreter
benchmarks/run.sh bench --json           # one JSON object per row, meta line first
benchmarks/startup.sh bench              # cold-start medians; --json and -n runs likewise
```

Arguments after the mode filter kernels (`run.sh bench fib strings`). The ds binary is found at `../target/release/ds` or through `DS_BIN`; a missing ds fails the run, missing peers print `SKIP`. Every interpreter invocation runs under a 600 s timeout, and the bench needs GNU `date` (`%s%N` nanoseconds). Bench numbers are meaningful only under the repo's exclusive `flock` with the machine otherwise idle; one invocation is one sample, repeat for stability. The JSON meta line carries the interpreter versions, so a saved file needs no side notes.

## Startup

`startup.sh bench` times four stages as medians over N runs (default 30): `floor` (`ds --version`), `trivial` (arithmetic and one print), `small-real` (a list, a map, one reduce), `warm` (trivial served from the script-mode dsmod cache; the cold stages force `--no-cache`). The adjacent deltas are the diagnostic: `trivial - floor` is pipeline entry cost, `warm` vs `trivial` is what the cache deletes. On Windows git bash the floor carries msys fork/exec overhead — compare deltas, not floors.

## Results

Committed tables are dated single-sample snapshots; refresh one with `bench --json > results/<date>-kernels.jsonl`, `startup.sh bench --json > results/<date>-startup.jsonl`, and rewrite the tables here from that run.

2026-11-21, Linux x64 (32 cores), exclusive flock, ds 0.1.0, Python 3.14.8, node v24.21.0, lua absent. Milliseconds, one run per cell.

| Kernel | ds | python | node |
|---|---:|---:|---:|
| fib | 993 | 431 | 70 |
| closures | 856 | 563 | 37 |
| lists | 755 | 394 | 133 |
| loops-arith | 581 | 1875 | 452 |
| float-loop | 433 | 822 | 46 |
| combinators | 472 | 247 | 61 |
| nursery | 294 | 1961 | 82 |
| records | 276 | 246 | 32 |
| containers | 161 | 267 | 103 |
| maps | 67 | 36 | 33 |
| strings | 54 | 28 | 31 |

Startup medians over 30 runs: floor 3.7, trivial 18.7, small-real 19.0, warm 4.1 (ms). Raw rows: `results/2026-11-21-kernels.jsonl`, `results/2026-11-21-startup.jsonl`.
