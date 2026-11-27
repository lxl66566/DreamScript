# Documentation index

Every working doc lives under `docs/en/`. The repository's front door is `README.md`; the binding project rules are `AGENTS.md`.

## Working docs — `docs/en/`

| File | Purpose | Read it when |
|---|---|---|
| `design-decisions.md` | The authoritative record of every language decision (44 points, A1-H6, plus dated author rulings and author supplements). | You implement or design any part of the language, or need to know *why* a rule exists. |
| `tutorial.md` | The entire v1 surface in one short tutorial, the explainability-budget proof. | You need the ruled surface syntax and semantics in reading order. |
| `implementation-plan.md` | Milestones M0-M9 with scope and acceptance criteria; workspace and crate layout; dependency policy; test and performance methodology; risks and recorded fallbacks; scope guards. | You plan a round or judge whether something is in scope. |
| `status.md` | What runs today: per-milestone state and the exact lowered subset the tier-1 pipeline executes. | You need the current boundary before promising or probing behavior. |
| `pitfalls.md` | Index over `pitfalls/`: invariants and lessons that prevent breakage, one board per area (coordination and gates, front end, language surface, lowering, runtime and memory, testing and perf). | You touch any of those areas, or a gate fails in a way that smells familiar. |
| `memory-arc.md` | The memory endgame execution contract: binding rulings, gates that stay green, silent-breakage register, STOP conditions, measured baselines. | You work on `ds-rt`, ownership lowering, or multi-word values. |

## Work queue — `.agents/todograph/`

The ranked queue lives in the todograph graph, one Markdown file per item, with priority (0 highest, the head item is the next round's work), milestone, effort, section and parked tags, and `deps` edges between items. Drive it with the todograph CLI or MCP tools; done items stay on disk as records, residual clauses keep queuing.

## Frozen archive — `archive/`

Stage 1-3 research notes (`archive/research/`), decision catalogs and records (`archive/design/`), and the author's supplements (`archive/author_say.md`). Never edit it; read it only when the working record is genuinely thin on a point, and treat it as background.

## Related indexes

- `AGENTS.md` — the project charter: expectations, design philosophy, documentation and commit conventions.
- `conformance/README.md` — how the conformance corpus is organized and how to add a regression.
