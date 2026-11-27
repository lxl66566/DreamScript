# Pitfall and lesson register

Invariants and lessons that prevent breakage, split into boards by area. Each entry was paid for by a failed gate, a miscompile, or a wasted round; decision IDs are kept so an entry can be traced to the record. Read the board for the area you touch before changing it, and add new entries to the board whose area owns the failure.

| Board | Covers | Read it when |
|---|---|---|
| `pitfalls/coordination.md` | Parallel agent lanes, CI gate mechanics, corpus header discipline and goldens. | You run parallel agents, touch the workspace `Cargo.toml`, or a gate fails in a way that smells like staleness. |
| `pitfalls/front-end.md` | Scanner, grammar, diagnostics, packages and modules. | You change `ds-parse`, `ds-diag`, `ds-pkg`. |
| `pitfalls/language-surface.md` | Ruled spellings that are easy to drift from. | You add or rename surface syntax. |
| `pitfalls/lowering.md` | Lowering and typechecking invariants that a green unit test does not catch. | You change `ds-check`, `ds-lower`, or a uop shape. |
| `pitfalls/runtime-memory.md` | The native/platform seam, the reentry seam, and runtime memory facts. | You work on `ds-rt`, `ds-vm`, ownership lowering, or multi-word values. |
| `pitfalls/testing-and-perf.md` | Fuzzing, benchmarks, and measurement traps. | You add a test, a fuzz target, or read a performance number. |

A pitfall that a gate now enforces with nothing left to remember is deleted rather than kept; the gate is the record.
