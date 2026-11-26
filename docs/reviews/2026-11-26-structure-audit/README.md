# Code structure and comment audit (2026-11-26)

Non-normative review document: a read-only survey of the tree at `13b9257e` (19 crates, ~160k `src` lines, ~300k with tests, 57,722 comment lines in 499 `.rs` files). It records no ruling; every item below is a proposal with its risk and its staging. Evidence files beside this file carry the per-file detail: `module-splits.md`, `crate-splits.md`, `docs-to-code-runtime.md`, `docs-to-code-surface.md`, `comment-rewrite-plan.md`. The per-line comment inventory is not checked in: the guard proposed in `comment-rewrite-plan.md` §3 re-derives it from the tree on every run, which is a fresher worklist than a frozen appendix.

Nothing in the tree was modified, built, or tested by the survey.

## Status

Landed with this document: the code change that single-sources the native rows into `ds-abi`, lifts the comptime evaluator into `ds-comptime`, and splits `ds-dsi` and the VM object store by phase/value family; and the comment and docs pass — every tier‑1 reference removed (3,604 audited hits to zero under the guard), the `tools/check_comment_refs.py` gate and its CI job, the 19 doc-to-code inlines, and the stale doc corrections. Still queued in §1/§2: the `ds-check` vocabulary-file splits, the staged `vm.rs` split, the `ds-lower` `func.rs` split, the `type_of/mod.rs` split, the `derive.rs` split, the `ds-cli` driver split, `ds-query`'s ambient-as-input inversion, and the four drift rulings of §4 (D1 needs an author ruling before anyone edits F8, `DS2017` or the `in` implementation).

## Conclusion

The tree is healthy in the dimension the audit expected to be worst: the runtime and ABI invariants that the docs carry are already stated at the site that enforces them, so the docs' irreducible content is process (which gates stay green, escalation policy, measured baselines, rejected alternatives) rather than behavior. Two structural facts dominate everything else:

1. **Size is intra-crate, not inter-crate.** `ds-vm` 38.7k, `ds-check` 33.1k and `ds-lower` 28.4k hold 62% of the code, and inside them single files hold 3k-13.7k lines. Splitting modules is the cheap lever (zero dependency-graph cost, no hot-path seam); splitting crates mostly buys dependency hygiene for the two cases named below.
2. **Comment debt is a vocabulary problem, not a prose problem.** 3,604 comments (5.2%) cite a document, a decision ID, a date, a work-item number or a phase name. 1,709 of those occurrences are a pure bracketed ID tag that can simply go; only ~66 rows across the tree need the invariant inlined first. The fix is five mechanical rules plus a CI gate that keeps it from regrowing.

Ranked backlog, with effort tags: guard script (S) → `ds-ffi` comment pass as the convention proof (S) → native vocabulary hoist into `ds-abi` (M) → `derive.rs` split (S) → `ds-meta` crate split (M) → per-crate comment passes (S each, 13 crates) → docs MOVE batch (S) → module splits in the order of §1 (S each, `vm.rs` M) → `ds-cli` driver split (L, trigger-gated).

## 1. Files that need a module split

Conventions that make every proposal in this section mechanical: `src/vm.rs` and `src/vm/*.rs` coexist in Rust 2018+, so no file becomes `mod.rs` and no call site changes; moved items get `pub(super)` (privacy is module-tree based and the compiler flags every miss); the `mod tests` block stays in the parent file with `use super::*`; each moved cluster takes the module-doc paragraph that describes it, the parent keeps the overall block (these doc blocks are the only statement of some locked invariants, so nothing is deleted and nothing is copied); `#[cfg]` items travel whole. Release builds use `lto = "thin"` + `codegen-units = 1` so intra-crate splits cost nothing there; dev/test profiles have no LTO, so inlining deltas must be measured in release only.

| Order | File | Code lines | Seam | Risk |
|---|---|---|---|---|
| 1 | `crates/ds-meta/src/derive.rs` | 4,900 | one file per derive family (`show`/`ord`/`hash`/`clone`/`default`/`from_json`/`to_json`) + shared `mint.rs`; families never call each other | lowest in the tree: only `expand_item` and `Mint` couple them |
| 2 | `crates/ds-dsi/src/lib.rs` | 2,507 | `data.rs` (wire DTOs), `encode.rs`, `decode.rs`, `serve.rs`: phase-disjoint | postcard field order must not move; `HEADER` stays single-owned |
| 3 | `crates/ds-vm/src/object.rs` | 3,224 | `rc.rs`/`text.rs`/`list.rs`/`map.rs`/`chan.rs`/`arena.rs`/`heaps.rs`: value-family disjoint | `Drop`/`unsafe impl Send` stay with the struct; `Heaps` must not split from `Objects` |
| 4 | `crates/ds-vm/src/native.rs` | 5,312 | family-per-file `text`/`scalar`/`list`/`map`/`generic`/`fork`/`registry`/`sink`/`io` | `builtin_module()` (895 lines) names everything, so it moves last; `NativeResult`/`NativeFunction` stay re-exported at the parent |
| 5 | `crates/ds-check/src/{import,decls,resolve,linearity}.rs` | 3,045 / 3,783 / 2,054 / 2,066 | vocabulary-vs-algorithm: `import/{word,graph}`, `decls/{foreign,reserve,shape,conv,instance}`, `resolve/{package,query,file}`, `linearity/{copyable,state,walk}` | each parent's `pub use` list must stay complete (`import::*` is a wide public surface) |
| 6 | `crates/ds-vm/src/vm.rs` | 13,079 | stage A `trap`/`proto`/`stack`/`value`/`call`/`chan`/`race`/`debug`/`install` (~5.9k), stage B `site`/`fiber`/`wait`/`host`/`teardown` (~3.3k), stage C `nursery` (~1.9k) | `execute` (2,851 lines) is movable to `execute.rs` but not splittable: its 63 arms share mutable frame-context locals, so arm extraction is behavior work with the VM benches, not a split |
| 7 | `crates/ds-lower/src/func.rs` (+`func/calls.rs`) | 9,791 / 5,535 | `plan.rs`, `facts.rs`, `emit.rs`, `ownership.rs`, `params.rs`, `place.rs`, `block.rs`, `loop.rs`, `value.rs`, `callable.rs`, `text.rs`, `path.rs`, `data.rs`, `op.rs`, `finalize.rs`; `calls/` into `ctor`/`evidence`/`result` with `stage_moves`/`calls_value` as the facade | largest `pub(super)` churn (170 + 15 private methods); `emit.rs` must stay the only writer of labels/pools |
| 8 | `crates/ds-check/src/type_of/mod.rs` | 10,630 | `item`/`record`/`expr`/`name`/`lambda`/`ctor`/`call(+select)`/`place`/`flow`/`solve`/`expect`/`query`, following the existing `impl_sig|ops|pattern|scope` shape | 187 private methods on one `impl Bodies`; do it last and in that order; never separate a `record_*` writer from `self.records` |

Low-priority but real seams: `ds-lower/src/func/pattern.rs` (control vs bind), `crates/ds-lsp/src/lib.rs` (protocol/worker vs `Session` state vs features). Explicitly not worth splitting at their current size and shape: `ds-lower/src/{native,shape,place}.rs`, `ds-parse/src/convert/mod.rs`, `ds-cli/src/{repl,package,source,run}.rs`, `ds-check/src/{effects,instance,instance_solve,universe}.rs`, `ds-vm/src/{proc,module}.rs`.

The three largest module docs (`vm.rs`, `type_of/mod.rs`, `func.rs`) already carry the design record for their area. After a split the parent keeps the overview and each submodule takes its paragraph; that is what keeps the reading entry point near 100 lines instead of 10k.

## 2. What is worth splitting into crates

Crate count is not the problem: five thin crates (`ds-abi`, `ds-diag`, `ds-rt`, `ds-corpus-path`, `ds-pins`) are thin for a stated reason and must stay. The normal-edge graph is acyclic; the `ds-stdlib <-> ds-lower` and `ds-check -> ds-stdlib` cycles exist only through dev-dependencies, so neither edge can ever be promoted.

Do now:

| Change | Why the boundary is real | Cost |
|---|---|---|
| Hoist the native vocabulary into `ds-abi` | `ds-lower/src/native.rs` (1.8k) is a hand-maintained second copy of `ds-vm`'s tables (123 `Native` + 20 `GenericNative` rows vs 84 `builtin_module` entries + 104 index constants), kept aligned *only* by cross-crate test assertions in `mod drift`. A locked contract belongs in the ABI crate, not in a test. Deletes ~1.2-1.5k mirror lines and drops `ds-lower`'s dev edge on `ds-vm`. | M, no runtime risk (static tables), no LTO seam |
| Split `ds-meta` into `ds-meta` (derives + analyze entries) + `ds-comptime` (evaluator) | `derive.rs` has zero `ds_vm`/`ds_lower` references; the evaluator is the entire reason `cargo tree -p ds-lsp` reaches `ds-vm`, `rustls`, `ring`, `webpki-roots`, `youpipe`. Consumers already partition on that line: `ds-query` calls only `expand_derives`, only `ds-cli` and the meta tests call `evaluate*`. A feature gate is the cheaper half-measure but cargo unifies features across a workspace build, so it would not hold. | M, no hot-path seam; the new crate must be added to `ds-cli/build.rs`'s `DS_BUILD_ID` list (`BYTECODE_CRATES`) while `ds-meta` stays listed |

Deferred with a trigger, not rejected:

- `ds-cli` -> `ds-driver` lib + thin bin (audience split: `source`/`package`/`*_cache`/`build`/`run`/`shutdown` vs the presentation modules). Trigger: a second embedder, or a decision to move the 53 subprocess tests off `CARGO_BIN_EXE_ds`; the compile recipe is already re-implemented in `fuzz/` and the lower tests. Effort L.
- `ds-check::corpus` -> `ds-cli` (the only production consumer of the `// items:` stage parser is `ds check`). Effort S, trigger: next touch of the exemption logic.
- `ds-query`'s ambient-as-input inversion (the honest fix for the `ds-query -> ds-stdlib` edge). Trigger: M7's package-config `InputKey`; doing it earlier means inventing that key early.

Rejected: `ds-vm` core/host split (the world ops are inline legs of the dispatch loop, referenced ~50 times in the park/wake state machine, and `Platform::new(name, &[])` is already the product-level capability boundary; if build time ever hurts, gate `net`/`tls` behind a feature with trap-fallback rows instead of cutting the dispatcher), `ds-check` algebra/passes split (one mutable `Universe`; every consumer needs both halves), `ds-ffi` marshal/loader split (1.4k total).

Hot inlining boundaries no future split may cross: `ds-vm <-> ds-abi`, `ds-vm <-> ds-rt`, `ds-lower -> ds-check`, `ds-check -> ds-diag`. Dev-profile test runs have no LTO, so a boundary inside those seams costs real test time even though release is unaffected.

## 3. Comments that violate the no-external-document rule

Measured over 57,722 comment lines (code and string literals excluded): 3,604 hits on 3,024 distinct lines. Class A (rewrite into self-contained behavior wording) 2,919, class C (git-history or phase narration, delete) 443, class B (harmless, keep) 216.

Per crate, density is what matters: `ds-check` 1,031 (11.3 per 100 comment lines), `ds-vm` 839 (5.4), `ds-lower` 504 (4.4), `ds-cli` 335 (5.0), `ds-abi` 164 (14.0), `ds-parse` 151 (9.7), `ds-query` 149 (9.7); the remaining 12 crates plus `fuzz/` hold 431. Worst files: `ds-vm/src/vm.rs` 236, `ds-check/src/type_of/mod.rs` 214, `ds-check/src/resolve.rs` 114, `ds-vm/src/native.rs` 109, `ds-lower/src/func.rs` 107, `ds-parse/src/ast.rs` 85, `ds-check/src/decls.rs` 78, `ds-check/tests/type_of.rs` 78, `ds-lower/src/func/calls.rs` 76, `ds-abi/src/uop/mod.rs` 72.

Five mechanical buckets cover every class-A row (`comment-rewrite-plan.md` §1 gives the per-bucket rule, counts and top files):

| Bucket | Occurrences | Rule |
|---|---|---|
| A1 pure ID tag | 1,709 | delete the bracketed/parenthesized ID run and its delimiter, keep the sentence; drop a stranded article |
| A2 subject/possessive noun | 827 | substitute the noun the ID names, or drop a milestone word (`M0`-`M9` = 179 rows: `the M2 static story` -> `the static story`) |
| A3 doc path/section/record pointer | 64 lines | drop the pointer, keep the invariant; `section 10 item 7`, `record 6.1`, `the record's`, `the corpus README's`, four literal `docs/en/...` paths |
| A4 history/phase narration | 164 lines | delete the phase word, keep the standing fact (`this round reads ELF64 alone` -> `the reader parses ELF64 alone`) |
| A5 dates and rulings | 367 lines | delete `(ruled 2026-11-25)` / `since ...`, keep the behavior clause; restate the oddity as a fact when the date was the only explanation |
| A6 work-item pointers | 95 lines | delete `(item N)` / `(todo item 58's ...)`, keep the mechanism the sentence names |

Class B must not be swept: 39 placement-verb `landed`, 54 ordinary-English `per the`, 54 data-record `the record`, 40 runtime slot/row `retired`, `$t#N` type-variable tags, `C11` (C Annex K), and six `docs/en/reference.md`/`errors.md` occurrences that are `ds doc`'s own output paths. Two audit corrections found while verifying: 54 `round` rows in `ds-cli`'s reload path (`dev.rs`, `repl.rs`, `dev_hot.rs`, `run_cache_race.rs`) plus `linearity.rs`'s loop-audit round are reload/audit/test rounds, not project narration; and the sweep misses phase words without a possessive (`the ownership round`).

The danger class is smaller than it looks: of 64 rows read in source, 52% are pure deletes, 41% noun substitutions, and 8% (five verified rows: the `R20` clamp, the `R25` per-label naming, the `R13` abandoned corner, the `R11` CALLS word shape, the `F9.5` tag re-key) need the invariant inlined because the token is its only statement. Five of the 13 `R`-family tokens (`R6`, `R11`, `R13`, `R15`, `R20`, `R25`) are defined nowhere in the repo. The decision rule for a fixer needs no doc access: resolve test (does the noun name a type/fn/opcode/table in this crate?), gloss test (milestone or phase word -> delete the word), why test (a clamp/corner/refusal named only by ID -> read the code and state the one-clause fact). Authority rule: the code is the only source; a pass never opens `docs/en/*`, or the rewrite imports a second doc dependency that rots at the next ruling.

Couplings where a comment is behavior, to check before editing: `crates/ds-diag/src/code.rs`'s doc comments are the error table `ds doc --errors` emits into `docs/en/errors.md` (drift-pinned; a rewrite needs the regen in the same commit); `ds-vm/src/lib.rs:105-106` states the citation convention itself and is deleted with the last `ds-vm` tag, never earlier; `#[error("...")]`/`expect("...")` narration in `ds-ffi` is golden-pinned behavior and out of scope; `.ds` sources are published verbatim into `docs/en/reference.md` and were not audited.

The guard (S, land first, budgets at today's counts so the job is green immediately): `tools/check_comment_refs.py` + self-test following the existing `tools/gc_targets.py` precedent, run by a seconds-long CI job. Tier 1 (deny, must reach zero): `doc-path`, `doc-number`, `rule-id` (2,266 lines, 69 tokens, the 10 undefined ones are the point), `date`, `ruling`, `narration`, `work-item`. Tier 2 (budget, fails only when a count rises): `landed`, `retired`, `per-the`, `the-record`, `the-contract`, `round` — the polysemous words whose false-positive classes are catalogued. Allow-list: the `ds doc` dumper paths and `C11`.

Per-crate passes, ratcheting the budget in each commit: `tools` + CI, `ds-ffi` (the 14-line sample patch is in `comment-rewrite-plan.md` §4), then `ds-check` 968, `ds-vm` 515, `ds-lower` 411, `ds-cli` 263, `ds-abi` 157, `ds-parse` 141, `ds-query` 146, `ds-meta` 35 (50 date rows in `derive.rs`, the biggest single delete), `ds-mod`+`ds-rt` 123, `ds-diag` 42 + `errors.md` regen, then the tail (`ds-fmt`, `ds-stdlib`, `ds-dsi`, `ds-lsp`, `ds-pins`, `ds-pkg`, fuzz).

## 4. Doc content that belongs in code comments

Headline: the behavior is already in code. Of memory-arc's 17 binding-register entries, 16 are already stated at the enforcing site (bands in `ds-lower/src/place.rs`, the ledger in `ds-vm/src/claims.rs`, the `settle` funnels in `object.rs`, the packed handle in `value.rs`, `slide_plan` in `tailcall.rs`, the tag rows in `shape.rs`); the pitfall boards read as transcriptions of those comments, not as their source. So the docs' irreducible content is process — which suites must stay green, STOP/escalation policy, measured baselines, rejected alternatives and their triggers — and moving that into code would either rot (numbers) or duplicate (gate lists).

Genuine MOVEs, surface axis (13 rows in `docs-to-code-surface.md`): the closed-surface statement and the frozen precedence table in `ds-parse` (`lib.rs:1-3`, `grammar.pest:197-200`; the precedence comment also still names a retired `or-extraction` level); the one-operand index rule (`grammar.pest:265-269`, where the absence of slicing is the only record); the `in`-over-a-list element rule (`type_of/ops.rs:394-421`); the chained-comparison refusal (`type_of/ops.rs:369-385`, currently incidental — nothing pins it); `DS2017`'s registry line (`ds-diag/src/code.rs:161`); the "locked, a change needs written justification" sentences in `ds-abi/src/lib.rs:44-47` and `ds-mod/src/lib.rs:3-7`, whose only content today is a doc path; the two `section 10 item 7` decision-procedure citations in `ds-check` (`lib.rs:39`, `instance.rs:58`); the no-GC/no-write-barrier/no-safepoint invariant and the arena mark/restore contract in `ds-rt/src/lib.rs:1-8` (stated nowhere in code); three facts currently sourced to `conformance/README.md` (`ds-lower/src/shape.rs:36-39`, `ds-stdlib/src/lib.rs:39-42`, `ds-check/src/corpus.rs:3`).

Genuine MOVEs, runtime axis (6 rows in `docs-to-code-runtime.md`): the host's trapped-drive release duty (`vm.rs:7908`), `Heaps{live:false}` still allocating into the scratch (`object.rs:2265`), a world-parked fiber being the scheduler's alone (`vm.rs:4373`), why the foreign-key refusal sits at the store and not the router (`object.rs:2079`), the CLI's native/platform-before-module install order (`ds-cli/src/run.rs`), and `word_shaped_ambient_atom`'s closed scalar set (`resolve.rs:1659`). Each of the six names a failure a future change would otherwise reintroduce.

Do not inline, with the reason: rejected alternatives (a rejection is not a behavior), recorded fallbacks and their speculative triggers (a queued condition expires), measurements and their provenance, budget accounting, product policy, the exhaustive "deliberately absent" surface list as prose (no enforcing site, only the grammar's silence), and the citation vocabulary itself.

Rule of thumb for the cleanup: a refusal belongs in the `ds-diag` registry doc line (enforced, rendered into `errors.md`, snapshot-pinned); a layout/encoding/protocol fact belongs in the owning crate's module doc (`ds-abi`, `ds-mod`); a dispatch or coherence rule belongs in the module doc of the deciding pass (`ds-check::instance|resolve|type_of`); a guarantee with no single deciding pass (TCO, no GC, no safepoints) is stated once at the crate root that would be tempted to break it; everything else stays in `design-decisions.md`. On the doc side that also means a DEDUPE list: `design-decisions.md` can drop the B7 enumeration, the BL-1/2/6/7 restatement, G3.5's format facts, H2's slide/descriptor story, B6's coherence rule and F2's callable classification, because those are normative in code already.

Reverse direction, more actionable than the MOVEs — the docs and one code comment encode stale facts:

- `memory-arc.md`'s measured baselines are stale against the committed `PINS` table: string_temp 8.20G/29.20G claimed vs `PINS` 4.80G/16.91G, strcat_temp 11.79G/24.45G vs 9.00G/15.85G, "~4,200 Ir per iteration, 3.6x" vs 2,422 Ir and 3.52x.
- `ds-pins/tests/pin_bake.rs:34-39` narrates a number (7,295,037,7xx) that contradicts the pin table four lines below it (6,485,912,913) — and the narration is git history the standard forbids.
- `memory-arc.md`'s `NativeResult::Span` "overlay words `0..=n`" wording invites an off-by-one: the check is `1 + payload.len()` against the caller's arg area, tag word included.
- `memory-arc.md` calls the claim ledger the teardown witness and the establishment band the witness in the next clause; `ref_arg` is a native-args helper, not a decode funnel.
- `pitfalls/language-surface.md`'s `.err` re-added-member entry describes an open lowering bug that is landed and pinned (`func/calls.rs:5085`, `collections_face.rs:108`).
- `pitfalls/lowering.md`'s `is`-test entry names a direct `Checked::is_tests` read that the lens drift pin forbids.

Four drifts between record, code and registry need a ruling more than a doc edit: (D1) `in` over a list is implemented and tested while F8 says collection membership is permanently a `@std` method and `DS2017` says "not ruled yet" — a three-way disagreement; (D2) the stale pitfall above; (D3) the grammar comment's retired `or-extraction` level; (D4) chained comparisons parse and are refused only incidentally, with no diagnostic and no test — either pin the refusal or refuse the chain in the conversion.

## 5. Suggested sequence

1. Land the comment guard (tier 1 + tier 2 with today's counts) and the `ds-ffi` pass: proves the bucket rules on 14 lines with zero behavior risk.
2. Native vocabulary into `ds-abi`: deletes a locked contract's mirror copy and a dev edge; touches no hot path.
3. `ds-meta` -> `ds-meta` + `ds-comptime`: the front end (`ds-query`, `ds-lsp`) stops building the interpreter and a TLS stack. Remember `BYTECODE_CRATES`.
4. Comment passes per crate in the order of §3, ratcheting the budget; `ds-diag` regenerates `errors.md` in the same commit.
5. Docs MOVE batch (§4): 19 code comments plus the reverse-direction corrections, one docs commit.
6. Module splits in the order of §1, one crate per commit; `ds-vm/src/vm.rs` staged A/B/C.
7. Trigger-gated, when the trigger fires: `ds-cli` driver split, `ds-check::corpus` move, `ds-query` input inversion.
