# Documentation index

Every doc lives under `docs/en/` (working docs, English) or `docs/zh/` (read-only Chinese mirror). The repository's front door is `README.md`; the binding project rules are `AGENTS.md`.

## Working docs — `docs/en/`

| File | Purpose | Read it when |
|---|---|---|
| `design-decisions.md` | The authoritative record of every language decision (44 points, A1-H6, plus dated author rulings and author supplements). | You implement or design any part of the language, or need to know *why* a rule exists. |
| `tutorial.md` | The entire v1 surface in one short tutorial, the explainability-budget proof. | You need the ruled surface syntax and semantics in reading order. |
| `implementation-plan.md` | Milestones M0-M9 with scope, work items, and acceptance criteria; workspace and crate layout; dependency policy; test and performance methodology; Rust-specific notes; risks and recorded fallbacks; scope guards. | You plan a round or judge whether something is in scope. |
| `status.md` | What runs today: per-milestone state and the exact lowered subset the tier-1 pipeline executes. | You need the current boundary before promising or probing behavior. |
| `todo.md` | The ranked open-work queue, with milestone, effort tag, author rulings, and questions awaiting the author. | You pick the next round's work. |
| `pitfalls.md` | Invariants and lessons that prevent breakage, grouped by area (parallel agents and cargo cache, CI gates, goldens, lowering, native and reentry seams, runtime, fuzzing, benchmarks). | You touch any of those areas, or a gate fails in a way that smells familiar. |
| `memory-arc.md` | The memory endgame execution contract: stage order, rulings, gates, silent-breakage register, STOP conditions, measured baselines. | You work on `ds-rt`, ownership lowering, or multi-word values. |

## Archive — `docs/zh/`

Read-only Chinese documents, not updated with the working docs: `ARCHITECTURE.md` (early architecture overview), `design-decisions.md` (translation of an earlier record state), `syntax-review.md` (surface syntax review). When the two languages disagree, the English archive under `design/` and the working record `docs/en/design-decisions.md` win.

## Frozen archive — `design/`

Stage 1-3 research notes, decision catalogs, decision records, and the author's supplements. Never edit it; read it only when the working record is genuinely thin on a point, and treat it as background.

## Related indexes

- `AGENTS.md` — the project charter: expectations, design philosophy, documentation and commit conventions.
- `conformance/README.md` — how the conformance corpus is organized and how to add a regression.
