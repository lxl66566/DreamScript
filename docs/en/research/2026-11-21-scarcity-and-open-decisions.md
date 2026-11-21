# Q6 — Feature scarcity across domains; Q7 — Remaining open decisions

Research memo, 2026-11-21 basis. Sources: `docs/en/design-decisions.md`, `docs/en/todo.md`, `docs/en/status.md`, `docs/en/tutorial.md` ("What is deliberately absent"), `crates/ds-stdlib/src/ambient.ds` (+ `std_members.ds`, `bodied.ds`), `crates/ds-vm/src/native.rs` (builtin table), `crates/ds-cli/src/run.rs` (`--args` staging).

## Q6 — Feature scarcity across domains

### What exists today (grounding)

The entire world surface is the `platform "std" provides <console, fsys, net, time>` face in `ambient.ds`:

- **console**: `println`, `print` (stdout only; no stdin, no stderr, no terminal/TTY surface).
- **fsys**: `read_file`, `write_file`, `file_exists`, `fs_list`, `file_size`, plus the linear `file_handle` face (`open_file`/`read_exact`/`close_file`).
- **net**: `net_connect`/`send`/`recv`/`close_conn` (client TCP only) and `http_get` (HTTPS client over rustls). No listener/accept, no server side of anything; TLS ruled client-only (2026-11-17).
- **time**: `time()` epoch nanoseconds only.
- **pure builtins**: string face (`trim`, `split`, `join`, `substring`, `concat`, `to_upper`/`to_lower`, `char_at`, `char_of`/`code_of`, `parse_int`/`parse_float`), collection face (list/map/bytes complete), width bridges, json face, hashing/compare, combinators (`map`/`filter`/`reduce`/`partition`), `par_map`/`par_reduce`, extraction family.
- **CLI entry**: `ds run --args` stages raw **i64 words only** into entry parameters — there is no string argv, no environment access, no exit-code channel, no process spawn anywhere in the tree (verified against the native table and ds-cli).
- **Concurrency**: nursery/spawn/cancel, payloaded cancel, fork-join pool; timers/sleep do not exist although E5 promises "timeouts, `firstof` and `interleave` are library combinators over the timer capability".
- **Blocked-at-lowering expressiveness**: element-carrying generic collection members (`xs.get(i)`, `m.get(k)`, `xs.push(x)`, `m.set(k, v)`) check clean but refuse at lowering (todo item 69, blocked on item 6's per-instantiation member bodies); union-typed list elements refuse at every read; derive on generic declarations refuses (item 6).

### Per-domain assessment

**CLI / tools / scripts (primary target, A2).** This domain is the most underserved relative to its priority. A script cannot read its own command line as strings, read an environment variable, read stdin, write stderr, exit with a status code, or spawn a subprocess — the four defining acts of a CLI tool are all absent. File globbing (only flat `fs_list`), path manipulation (no join/dirname/extension helpers), datetime (only epoch ns), random, and regex are all missing too. The json/string surface and the error/extraction story are genuinely good; the platform seam is the hole.

**Servers.** The surface is zero beyond an HTTPS *client*: no TCP listener/accept, no HTTP server, no server TLS (ruled client-only), no timers (E5's promised timeout combinators are unbuildable), no signal handling (SIGTERM graceful shutdown), no logging convention. The concurrency substrate (nurseries, fibers, cancellation) is ahead of the I/O surface it would serve — a server is the natural nursery showcase and cannot be written.

**Games / simulation (secondary, A2).** The language-side story is decent (hot reload landed, FFI landed, unboxed f64, guaranteed TCO, deterministic RC). What is missing: float math functions — `sqrt`/`sin`/`cos`/`atan2`/`floor`/`pow` do not exist in any form, so even vector math is unwritable; PRNG (the `+%`/`*%` wrapping ops exist for exactly this, and `time()` can seed, so this is a small @std module once someone writes it); vec/matrix types are writable in ds source only after item 69's element-carrying members lower (a `vec3` struct with scalar fields works today; a generic `vec[t]` does not). Frame-loop host integration is by design the embedder's job (A1 platform packages).

**Numeric / AI (later, A2).** Explicitly not shaping v1. Gaps when it arrives: float transcendentals (shared with games), bigint (never ruled on; B7 rules `Dec` as a nominal library type but bigint arithmetic over `bytes` in ds source would be far too slow — it wants a builtin or an artifact-backed extern), PRNG, and the Cranelift tier for throughput. Not blocking now by design.

### Top 10 most-scarce features (ranked)

| # | Feature | Why | Kind |
|---|---|---|---|
| 1 | String argv + environment access | Every real CLI script needs both; today impossible at any layer (`--args` is i64-only) | Platform capability (console/host face) |
| 2 | stdin read + stderr write | Pipelines and diagnostics; console capability is stdout-only | Platform capability |
| 3 | Process spawn + exit status | Scripts orchestrating tools is the primary workload; wholly absent | Platform capability (new `proc` face) |
| 4 | Exit-code channel (`exit(code)` / main-result mapping) | CLI tools cannot report failure status; the Die ruling explicitly leaves exit codes undecided | Platform + one small ruling |
| 5 | Float math builtins (`sqrt`, `sin`, `cos`, `atan2`, `floor`, `ceil`, `pow`, `abs`) | Games/sim and numeric both dead without them; pure builtins per H5.5, trivial to add | Pure builtin |
| 6 | TCP listener / HTTP server | The server domain has literally no surface; nurseries have no showcase workload | Platform capability (net face) |
| 7 | Timer/sleep op | E5 already promises timeout/firstof/interleave combinators "over the timer capability" that does not exist | Platform capability (time face) |
| 8 | PRNG (seeded) | Games, simulation, test data; wrapping ops + `time()` make it a small pure/@std module | Library (@std) or pure builtin |
| 9 | String search/replace/contains | Text processing beyond split/trim is impossible (no `contains`, `index_of`, `replace`); F8 already reserved the `xs.contains(x)` spelling | Pure builtin + @std |
| 10 | Datetime/calendar formatting | Logs and CLI output need civil time; `time()`'s own comment defers "a monotonic or zone-aware clock" with no follow-up | Library over `time()` + platform tz op |

Just below the cut: sort combinator over the `ord` trait (surprisingly absent given `ord` derives landed); map iteration beyond `keys()`; item 69's element-carrying generic members (the single biggest *language-implementation* unlock — it gates `xs.get`, good vec types, and ergonomic collections everywhere); regex/glob (library-scale, defer); signal handling (servers, defer with the server round); file metadata/mkdir/remove (fsys face completion).

**Language features vs library vs platform**: only one item is a language-*implementation* blocker (item 69/item 6 generic member bodies) — notably, zero of the ten require new language *features*; the syntax/type system is not the scarce resource. The scarce layers are platform capabilities (items 1,2,3,4,6,7), pure builtins (5, 9), and @std library code (8, 10).

## Q7 — Complete inventory of open decision points

Scan method: every occurrence of "open question", "stays open", "stays unmeasured/unranked", "waits on / awaiting the author", "keeps honest lowering refusals", "stays deferred", "reopenable", "valve", "trigger" in `design-decisions.md` and `todo.md`, cross-checked against `status.md`. Items are split into (A) genuinely undecided rulings, (B) recorded deferred valves with a defined trigger (decided *when*, open *whether*), and (C) unrecorded gaps.

### A. Genuinely open rulings

1. **The bare `u32(x)` spelling vs F9 namespace priority** — design-decisions B7 ("Open question: whether the bare spelling warrants a checker round amending F9's priority") and todo item 75, the only item explicitly "awaiting the author's word". Options: (i) amend F9 call-form priority so a width-type name resolves a call to the trapping conversion; (ii) keep `u32.of(x)` permanently; (iii) ambient value named `u32` (already ruled out, DS2014 ambiguity). Priority: **low** — cosmetic, one spelling; but it is an author-decision blocker on an otherwise-finished item. [M6, S]

2. **Whether pooled foreign calls should run** — todo item 63: a foreign call inside a fork-join pooled child refuses like a world op; "whether pooled foreign calls should run stays open". Options: keep the refusal (determinism contract, E3's pooled-legality ruling argues this way); or admit with fiber-pinning semantics on workers (H5's pinning rule extends naturally). Priority: **low**, behind 0.1 with the fork-join rounds. [M-tier, S]

3. **`[build]` source-build round for native artifacts** — design-decisions H6 and todo item 63: deferred on "a pinned-toolchain reproducibility story, never permanently rejected". Options: prebuilt-only artifacts (status quo) vs source builds with a pinned toolchain story. Priority: **medium** for ecosystem credibility, safely post-1.0. [M7+, M]

4. **Built package artifacts through the install walk + multi-version coexistence per name** — todo item 63 ("stay open"), beside G3.5's standing refusal of same-name multi-hash coexistence. Options: keep one-version-per-name (MVS already picks one) vs version-keyed module slots (the two-version machinery exists for hot reload). Priority: **medium** — diamond-dependency ergonomics eventually force it. [M7+, M]

5. **Directory-shaped dep declarations (manifest grammar ruling)** — todo item 67: hot reload cannot watch a linked dep's source because content-addressed pins leave the manifest the only lever; "a directory-shaped dep declaration needs a manifest grammar ruling". Options: path deps in the manifest (Cargo-style) vs pin-only with a dev-mode override flag. Priority: **low-medium**, dev-loop UX. [M7, S]

6. **`platform "web"` op set** — todo item 29: waits on the wasm tier; "its op set ruled with the tier". Priority: **low**, M8-locked by construction. [M8, S]

7. **The M9 pin-bake gate** — status.md M9: "the pin-bake gate stays open" (acceptance tooling, not a language ruling). Priority: **medium**, it is on the 1.0 path. [M9, M]

### B. Recorded deferred valves (decided triggers, open outcomes)

These are not undecided — each has a recorded trigger — but each will require a ruling when its trigger fires:

8. **`??` on `t | none`** — B4/F8: reopenable only through M9 backlog triage on corpus evidence that `t | none` fallback dominates. [M9 triage]
9. **User-defined effect handlers** — E1: v2 additive, trigger is "a real use case a capability parameter cannot cover cleanly"; multi-shot valve closed, resumptions affine. [v2]
10. **The deferred-additive backlog** — §11: `|>`, anonymous record literals (map literal rides it), `@` binders, `?.` desugar, `throws(...)` sugar, scoped named instances, labeled break/continue, `it` shorthand. Each re-enters only through the M9 triage and must re-fit the tutorial. [M9, per item S]
11. **C4b generational-references fallback** — C4: dormant behind a quantified moved-after-use-rate trigger confirmed ownership-specific by tutorial feedback. [post-1.0 telemetry]
12. **H1b single method-JIT fallback** — retires when the x86-64 stencil build lands (M8). [M8]
13. **`par_reduce` parallelism** — todo item 45: waits on the associativity story (the `associative` marker trait was rejected; the members migrate to @std). [behind 0.1]
14. **Effects-prefix park ranking** — todo item 49: "the dropped Effects-prefix park stays unranked" (performance micro-decision). [M7, S]
15. **Comptime fence residuals admission** — todo item 57 (under "Questions awaiting the author" but engineering-shaped): member calls and generic callees inside comptime-admitted bodies refuse, "both waiting on the runtime module" — the admission widening will need a ruling when the runtime module lands. [M6/M7, S]

Engineering residuals that are **not** open decisions (refusals already ruled, work queued): todo items 5, 6, 14, 30, 39, 45, 49, 52, 53, 54, 56, 58, 66, 67(watcher), 69, 88, 93. Item 14's refusals are explicitly permanent.

### C. Design gaps not recorded anywhere

16. **Error taxonomy granularity** (memo 1): `io_err` is one bare tag with no classes and no message; the runtime knows the error class and the message and drops both from the typed value. No ruling on granularity, payloads, or a reporting convention exists.
17. **The whole CLI process surface**: string argv, environment variables, stdin/stderr, exit codes, process spawn — none is mentioned in any doc. The `--args` i64 staging looks like a placeholder nobody ruled on. This is the largest unrecorded gap relative to the A2 primary workload.
18. **Exit-code semantics**: the Die split (G3.5, 2026-11-17) rules the engine "decides no exit code" — and nothing rules who does (the CLI host? a fixed 1 on trap? a `main` returning int?).
19. **The timer capability**: E5's timeouts/firstof/interleave are ruled as "library combinators over the timer capability", but no timer/sleep op, capability face, or ruling exists anywhere.
20. **Signal handling** (SIGINT/SIGTERM): never raised; required for any server or long-running CLI.
21. **Server-side net**: no listener/accept surface; TLS ruled client-only with the server side never raised.
22. **Logging**: no convention (levels, structured, sink capability) is recorded; presumably library, but nobody said so.
23. **Bigint**: B7 rules `Dec` a library type; bigint is never mentioned, and a ds-source bigint would be too slow — it needs a builtin or extern ruling.
24. **Float math builtins**: `sqrt`/`sin`/`cos`/etc. are never raised; they are pure builtins per H5.5 but the absence is unrecorded.
25. **String search** (`contains`/`index_of`/`replace`): F8 rules collection membership is "permanently a @std method (`xs.contains(x)`)" — yet no `contains` exists for strings or lists, and no string-search ruling exists.
26. **`parse_err` position payload**: the ambient comments "the offending position is not carried" — a known deficiency with no ruling on whether/when errors carry payloads (see memo 1).
27. **Trap diagnostics**: `Die` carries a message; no ruling on trap traces/backtraces for the CLI report (Rust/Zig both treat this as essential debuggability).
28. **Map iteration**: tutorial states "Maps have no iteration" except `keys()`; no ruling on whether that is the v1 endpoint or a seam (an `entries()` face seems inevitable).

## Recommended next steps

Ranked; milestone/effort tags in todo.md style.

1. [M6, S] **Rule the error taxonomy (Q5, memo 1)**: split `io_err`/`net_err` into branchable unit tags under aliases, rule per-member payload policy (`parse_err` position), decline a standard `err` trait. Cheap now, breaking later — rule before more natives harden the bare tags.
2. [M7, S] **Rule and land the CLI process surface (Q6 items 1–4)**: string argv, env vars, stdin/stderr, exit-code channel, as console/host capability extensions. This is the primary workload's minimum viability.
3. [M3, S] **Fix the ds-lower narrowing-copy miscompile (todo item 93)**: multi-word tuple payloads read from offset 0; blocks item 5's json roundtrips and any single-member-enum code. Highest-leverage correctness item in the queue.
4. [M7, M] **Cross-package linking residuals (todo item 58)**: the ecosystem backbone; unblocks @std growth (item 66's `par_*` migration, item 53's phasing, real third-party packages).
5. [M6, S] **Author ruling on the bare `u32(x)` spelling (todo item 75, Q7 item A1)**: the only explicitly awaiting-author item; close it either way.
6. [M7, S] **Pure-builtin round (Q6 items 5, 9)**: float math family (`sqrt`/`sin`/`cos`/`atan2`/`floor`/`pow`) + string search (`contains`/`index_of`/`replace`) — small, unblocks games/sim smoke tests and real text processing.
7. [M6, M] **Per-instantiation member bodies (todo item 6 → item 69)**: unblocks `xs.get`/`m.get`/`xs.push` as members, derive on generic types, and generic `vec[t]`-shaped libraries — the largest single expressiveness unlock short of the JIT tier.
8. [M7, M] **Timer capability ruling + landing, then the server round**: land `sleep`/timer in the time face so E5's promised timeout/firstof/interleave combinators become buildable; follow with `net_listen`/accept and the exit-code + signal rulings (Q7 items C17–C21) as the server-surface round.
