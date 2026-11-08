# DreamScript — design charter

This file is the global charter for the DreamScript language design. The working record of all decisions is `docs/en/design-decisions.md`. `design/` is a frozen archive (Phase 1-3 research and decision catalogs, plus `design/author_say.md`). Do not read `design/*` frequently unless necessary, because the articles are too long.

## Mission

A general-purpose, interpreted programming language with static linear typing, immutable-by-default bindings, and performance approaching compiled languages. The compiler produces an internal IR (bytecode at minimum, optional JIT tiers). The sweet spot being targeted: scripting-language ergonomics with systems-language performance guarantees.

## Hard requirements (author's fixed inputs)

1. Interpreted execution. Internal IR/bytecode is allowed and expected. JIT is allowed where it pays for itself.
2. Static linear (or affine) typing, enforced at compile time.
3. Ultra-high performance. Everything a compiled language does beyond raw codegen must also happen here: guaranteed tail-call optimization, unboxing, escape analysis, inline caching where applicable, legal specialization. JIT optional per tier.
4. Syntax sugar budget: at most Kotlin's total sugar surface. Orthogonality beats surface features.
5. Bindings immutable by default; mutation is an explicit, locally-visible choice.
6. Ownership is an open question. Prefer designs with low learning cost and low error rate that keep the performance benefits.
7. Algebraic effects are optional. Evaluate them seriously; adopt only if they earn their complexity.

## Design philosophy (author's essays, distilled)

- Decoupling first. Every feature is an orthogonal, composable piece, following the LSP/RISC model. Reject entangled features: function-coloring entangles every function with its scheduling; C++ specialization entangles generics with ad-hoc priority rules; inheritance entangles data, behavior and identity.
- Set-theoretic types. Types are sets; union and intersection are natural; generic instances must not overlap, which removes the need for specialization matching.
- Monadic extraction over default parameters. Optional/erroneous values are wrappers with flexible, composable extraction (`unwrap_or_else` style), never evaluated-out-of-order default arguments.
- Composition over inheritance; trait-like relationships over interface hierarchies.
- Compile-time errors over runtime errors. Invalid states should fail to typecheck; prefer enums and types over stringly-typed runtime checks.
- Explainability budget. Every feature must pay for itself in power; the language must remain fully explainable in a short tutorial.

## Author taste calibration

- Primary background: Rust, TS, Python, Lua, Java, C++; TS+bun ergonomics are loved; Go and Lua are disliked.
- Appreciates: Koka's UFCS and effect rows, Vale's memory-design writing, Roc's pragmatism, MoonBit's momentum, monadic `Option`/`Result`.
- Dislikes: specialization, `#include`-style modules, Python's module granularity, steep builtin-function learning curves with poor docs.

## Documentation conventions

- Language: English, no emoji. Every markdown paragraph is a single line (no hard wrapping, no width limit).
- `docs/en/design-decisions.md` is the authoritative working record of every language decision (all 42 decision points, author supplements included). Implementation and further design work read this file; it is sufficient without the archive. `docs/zh/design-decisions.md` is its Chinese mirror; the English file is authoritative when the two diverge.
- `design/` is a read-only archive, kept for reference only: never modify files under `design/`, and do not read it unless the working record is genuinely insufficient on a point (then treat it as background, still never edit it). It contains the Phase 1-3 research notes (`design/en/research/`), decision catalogs and decision record (`design/en/design/`), Chinese mirrors (`design/zh/`), and `design/author_say.md` (author supplements, already incorporated into the working record). The English archive files are authoritative when the English and Chinese mirrors diverge.
- Files are strictly disjoint per contributor; no git commits unless the author asks.

## Status

- Phase 1 (research): done. Archived under `design/en/research/`.
- Phase 2 (option catalogs): done. Archived under `design/en/design/`; `design/en/design/00-decision-map.md` is the master worksheet.
- Phase 3 (author decisions): done 2026-11-08. Every decision point resolved; extracted in full into `docs/en/design-decisions.md` (decisions, staging, resolved open questions, seed test corpus).
- Phase 4 (implementation): current. Work from `docs/en/design-decisions.md` (staging section); the seed test corpus is listed there.
