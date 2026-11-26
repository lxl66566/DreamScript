# Comment rewrite plan: from audit to executable passes

Read-only planning artifact. Nothing in `/root/programs/DreamScript` was modified; no build or test was run. Source of truth for rows is `/tmp/ds-survey/comment-audit.md` (appendix A: 1,039 per-line rows for doc-path/section/item/date/ruling/narration; appendix B: 2,539 per-occurrence rows for rule IDs). Every count below was recomputed from those appendix tables, not copied from the audit prose.

## 0. Reconciled totals

| class | audit headline | recomputed from appendix rows | note |
|---|---|---|---|
| A rewrite | 2,929 | 2,919 (383 in appendix A + 2,536 in appendix B) | delta 10: the headline counts raw pattern hits, a line matching two families is one row in an appendix |
| C delete | 450 | 443 (441 + 2) | same cause |
| B keep | 225 | 216 (215 + 1) | same cause, plus one line reclassified by hand |

Rows that carry both an ID and a doc/date/history token appear in both appendixes; bucket counts below therefore count per-row edits, not distinct lines, and a line can receive two mechanical edits in the same pass. Distinct comment lines touched: 3,024 (audit) of 57,722 comment lines (5.2%).

Two classification errors were found while verifying rows; both shrink the work:

- **`round` is not always project narration.** The reload loop reuses the word: `crates/ds-cli/src/dev.rs` 17 rows ("the next round pumps the wait"), `src/repl.rs` 9, `tests/repl.rs` 4, `tests/dev_hot.rs` 9, `tests/run_cache_race.rs` 7. Same shape in `crates/ds-check/src/linearity.rs` 3 (the loop-audit round), `crates/ds-vm/tests/tls_server.rs` 4 (a test iteration), `crates/ds-lower/src/native.rs:622` ("the half rounds away from zero", arithmetic rounding). The audit's suggested rewrite for these ("replace the phase reference with the concrete fact `what the ambient cannot carry`") is nonsense, which is how they identify themselves as false positives. **54 of the 218 narration rows are class B, not class A**; true narration is 164.
- **The sweep misses phase words without a possessive.** `crates/ds-ffi/src/marshal.rs:16` ("stay queued with the ownership round") and `crates/ds-ffi/src/lib.rs:89` ("the shape a marshaling round transmutes into a typed call") were not flagged, because the patterns require `this round`, `the round` or `round's`. Per-crate undercount is small but real; the guard in section 3 is written to catch it (`\brounds?\b` is a budgeted rule, not a deny).

## 1. Buckets: five mechanical rules, counted

Every class-A row lands in exactly one bucket. Each rule is executable without judgment beyond "does the sentence still parse".

### A1 - delete a pure ID tag (1,709 occurrences, 1,694 lines, 217 files)

Rule: delete the bracketed or parenthesized run of rule IDs (`[H2]`, `[C1, C5]`, `[BL-7, H6 item 5]`, `(G4)`, `(H5.5, E1)`, `(B1, B3)`) together with its delimiter. Keep the sentence. If the tag sits mid-sentence and the deletion strands an article, delete the article too (`the uop set (BL-5), locked from M3` → `the uop set`). The tag is an appositive: in every A1 row read for the sample (section 2) the clause it trails already states the behavior.

Top files: `ds-check/src/type_of/mod.rs` 154, `ds-vm/src/vm.rs` 98, `ds-lower/src/func.rs` 84, `ds-check/src/resolve.rs` 79, `ds-parse/src/ast.rs` 77, `ds-check/src/decls.rs` 65.
Per crate: ds-check 663, ds-lower 264, ds-vm 243, ds-cli 108, ds-parse 97, ds-abi 91, ds-query 88, ds-diag 30, ds-fmt 15, ds-stdlib 15, ds-dsi 12, ds-ffi 8, remaining 75 (ds-mod 28, ds-rt 23, ds-meta 17, ds-lsp 7, fuzz 1).

### A2 - substitute the noun, or drop a milestone word (827 occurrences, 761 lines, 178 files)

Two forms, one rule: replace the token with what it names, or delete it when it names a project milestone.

- **possessive (100):** `B9's structural equality` → `structural equality`; `B6's link-time check` → `the link-time check`; `E3's fork-join transfers ownership` → `fork-join transfers ownership`; `G4's transitive closure` → `the transitive re-export closure`.
- **subject (727):** `per BL-6 the parameter frames start at ...` → `the parameter frames start at ...`; `the pass fires the kept frames' hooks first (E5: teardown walks frames running deinits)` → `the pass fires the kept frames' hooks first (teardown walks frames running deinits)`; `the value use keeps the M3 contract refusal` → `keeps the contract refusal`.
- **milestones M0-M9 (179 rows: M3 66, M2 56, M4 22, M7 16, M0 5, M5 5, M1 4, M6 3, M8 1, M9 1):** delete the word, keep the concrete fact. `The ds check subcommand: the M2 static story` → `the static story`; `UFCS resolves in M2` → `UFCS resolves in the checker`; `a clean whole-file parse is the right M1 expectation` → `is the expectation`; `the M3 stdlib owns the bodies` → `the ambient stdlib owns the bodies`.

Top files: `ds-vm/src/vm.rs` 52, `ds-abi/src/uop/mod.rs` 31, `ds-check/src/type_of/mod.rs` 27, `ds-check/tests/type_of.rs` 27, `ds-vm/src/native.rs` 27, `ds-check/src/effects.rs` 22.
Per crate: ds-check 231, ds-vm 171, ds-lower 98, ds-cli 61, ds-abi 51, ds-query 49, ds-mod 48, ds-parse 37, rest 81.

The legend that supplies the noun already exists in the audit (`/tmp/ds-survey/legend.json`, 60 tokens). No pass needs the design record for this bucket.

### A3 - doc path, doc section, doc record, or "the record" pointer (64 lines, 58 files)

Rule: delete the pointer and keep the invariant; if the invariant is only in the pointer, state it in code words. In the sample (section 2) every doc-pointer row carried its content in the same sentence, so this bucket is the cheapest of the five. Concretely:

- **13 `README` rows** (`crates/ds-check/src/corpus.rs:3`, `ds-check/tests/conformance_gate.rs:6,21,595`, `ds-cli/tests/check.rs:26,72`, `ds-cli/tests/negative_goldens.rs:7`, `ds-lower/src/shape.rs:38`, `ds-parse/tests/conformance.rs:2`, `ds-stdlib/src/lib.rs:39`): drop `the corpus README's` and keep the vocabulary term in front of it.
- **21 `the record` rows** (`ds-abi/src/tailcall.rs:257`, `ds-abi/src/uop/mod.rs:400`, `ds-abi/src/uop/tests.rs:1424,1430,1451`, `ds-check/src/effects.rs:16,91`, `ds-check/src/platform.rs:7`, `ds-check/src/linearity.rs:2072`, `ds-check/src/resolve.rs:11`, `ds-check/src/type_of/mod.rs:27,4315,8319`, `ds-fmt/tests/canonical.rs:1`, `ds-lower/src/func/calls.rs:3451`, `ds-meta/src/derive.rs:1236`, `ds-stdlib/src/lib.rs:603`, `ds-vm/src/native.rs:204`, `fuzz/check_gen.rs:1040`): replace `the record's` with `the ABI's`/`the registry's`/nothing; the example or the list follows in the sentence.
- **7 `section 10` rows, 4 `record 6.1` rows** (all `ds-check`): the doc number is the only referent in 4 of them (`src/resolve.rs:62`, `src/resolve.rs:304`, `src/instance.rs:58`, `src/lib.rs:39`) - those state their rule (`the structural instance-cycle check`, `the handler surface form; no name sits behind it`).
- **4 doc-path rows** (`ds-abi/src/lib.rs:47`, `ds-mod/src/lib.rs:5`, `ds-parse/src/lib.rs:2`, `ds-rt/src/lib.rs:8`): the surviving sentence is `a change requires a written justification carried with the change` / `Parser for DreamScript: lexer, pest grammar, and spanned AST for the language surface` / `The arena module's docs carry the full contract mapping`.
- **4 `pitfalls`, 6 `per the`, 3 `the contract`, 1 `documented in`, 1 `design record` row:** the parenthetical or pointer *is* the content; drop the pointer, keep the clause (`ds-cli/tests/relocation_gate.rs:10` becomes `a silently wrong-id miscompile at the ABI; so this gate holds the seam name-addressed`).

Per crate: ds-check 30, ds-cli 8, ds-abi 7, ds-parse 4, ds-vm 4, ds-lower 3, ds-stdlib 3, ds-mod 1, ds-rt 1.

### A4 - history and phase narration (164 lines after removing the 54 false positives)

Rule: delete the narration, keep the standing fact. Four shapes:

- **round/phase (the bulk of the 164):** `the artifact round's richer alias and union words` → `the richer alias and union words`; `until the M7 loading round's link pass` → `until the loading walk links it` (or state the current value: `is null until the JIT compiles the body`); `this round reads ELF64 alone` → `the reader parses ELF64 alone`.
- **queued/waits-on (part of the above):** `deinit-carrying handles stay queued with the ownership round` → `are outside this face`; `stays deferred` → state the refusal.
- **landed (1 A row):** the remaining 39 `landed` rows are class B (placement verb).
- **residual (12):** `residual` names a gap without naming it. Replace with the concrete case (`a struct beside the fn is still the DS2014 call-form`) or delete the clause.

Top files: `ds-check/src/import.rs` 6, `ds-cli/tests/package.rs` 6, `ds-cli/tests/std_members.rs` 5, `ds-lower/src/func/calls.rs` 5, `ds-meta/src/lib.rs` 4, `ds-pkg/src/resolve.rs` 4.
Per crate: ds-lower 37, ds-vm 32, ds-check 30, ds-cli 28, ds-pkg 7, ds-diag 5, ds-query 5, ds-meta 4, ds-ffi 4, rest 12 (ds-lsp 3, ds-pins 3, fuzz 4, ds-parse 1, ds-stdlib 1).

### A5 - dates, rulings, and dated provenance (367 lines: 362 class C, 1 class B, 4 class A)

Rule: delete `(ruled YYYY-MM-DD)` / `(the YYYY-MM-DD <noun> ruling)` / `since YYYY-MM-DD` and keep the behavior clause that follows. **Never keep the date "because it carries a trigger"** - a trigger is either current behavior (then state it without the date) or history (then it belongs to the commit message). Two shapes need a rewrite rather than a deletion:

- the parenthetical is the only thing explaining an odd rule (`an IP-literal host resolves inline (the 2026-11-23 ruling)`) → restate the oddity as a fact (`resolves inline, without parking`);
- a whole heading is the narration (`# Parking discipline (the 2026-11-21 scheduler ruling)`) → delete the parenthetical, keep the heading.

Top files: `ds-vm/src/vm.rs` 48, `ds-vm/src/net.rs` 36, `ds-meta/src/derive.rs` 31, `ds-vm/src/native.rs` 28, `ds-check/src/type_of/mod.rs` 15.
Concentrated in `ds-vm` (`vm.rs` 48, `net.rs` 36, `native.rs` 28) and `ds-meta/src/derive.rs` 31; the rest is one or two rows per file across 15 crates.

### A6 - work-item pointers (95 lines, 95 files)

Rule: delete `(item N)`, `[H6 item 5]`, `(todo item 58's ...)`, `(item 79, ruled ...)` and keep the mechanism the sentence already names (`the two-slot module table`, `the parked DNS twin`, `the close seams' wake notes`). Where the item number is the sentence's subject (`todo item 53's end-to-end pins`), the noun is in the same line (`the library-boundary pins`). Per crate: ds-vm 57, ds-check 15, ds-cli 9, ds-abi 7, ds-lower 7, ds-query 4, ds-fmt 1.

### Not in any bucket

- **443 class-C lines** (appendix A rows marked C: date 198, ruling 164, retired 32, landed 41, round 2, crossdoc 4, docpath 2) are deleted, not rewritten. They overlap A4/A5 in intent; count them once, in the bucket where their token appears.
- **216 class-B lines** are frozen. The catalogue (audit section 8, verified): placement-verb `landed` 39, `per the` 54, data-`record` 54, runtime slot/row `retired` 40, `$t#N` 5, `C11` (C Annex K) 1, the `ds doc` dumper paths 6, `N1/N2/N3`, `round-trip`, `between rounds`, `the die split`, `the contract` (crate layout).

## 2. The danger class and the decision rule

### Definition

A row is dangerous when the ID (or doc pointer) is the **only** statement of a load-bearing invariant in that comment, so a mechanical delete silently drops the rule instead of a decoration. The test is not "is the ID a subject" (the audit's 532-row estimate) but "does anything left in the comment still name the mechanism".

### Measured

64 distinct rows read in the source: a stratified sample across 16 crates, plus 8 rows of the undefined `.5` family and the whole `R` family. Result:

| verdict | rows | share | example |
|---|---|---|---|
| pure delete (tag is an appositive, or the gloss follows in the same comment) | 33 | 52% | `// C3 has exactly two re-assignable carriers: var ... and mut ...` |
| noun substitution only (token supplies the meaning, legend supplies the noun, no code reading beyond the sentence) | 26 | 41% | `The BL-4 candidate family` → `The superinstruction candidate family` |
| **inline required** (the clause names the invariant by ID only) | **5** | **8%** | `stay the R13 abandoned corner` |
| also flagged separately: milestone word to drop | (counted inside the 26) | | `the M2 static story` → `the static story` |

Applied to the 827 A2 rows: ~66 inline-required rows (range 25-140 at this sample size), not 532. The 761-row set is therefore a 10-crate mechanical pass with a ~66-row reading tail, not a 532-row reading pass.

Systematic subclasses that came out cheap in the same reading:

- the **`.5` family** (`F9.5` 77, `G3.5` 32, `H5.5` 19, `H3.5` 3 = 131 occurrences, defined nowhere in the repo) is *not* the danger peak it looks like: 7 of 8 sampled rows state their behavior and only need the phrase `the F9.5 phase-1 re-key` shortened to `the tag re-key`. Undefined-ness predicts a reword, not an inline.
- the **`R` family** (14 occurrences on 13 lines) is the true danger: 4 of the 13 lines need content that only the source reveals (`R20` clamp, `R25` naming, `R13` corner, `R11` shape), the rest only lose the token.
- `the record's` + `per the` + `the pitfalls entry` are pointer noise; every one sampled loses nothing when the pointer goes.

### The five rows that must inline (verified, with the replacement)

| location | fragment | inline to |
|---|---|---|
| `crates/ds-diag/tests/render_fuzz.rs:4` | `stale offsets past the end of their file (the R20 clamp)` | `(clamped to the file's span)` |
| `crates/ds-diag/tests/render_fuzz.rs:5` | `labels crossing files in any order (the R25 per-label --> naming)` | `(each label naming its own file)` |
| `crates/ds-lower/tests/deinit_tail_exit.rs:9` | `a root native's staged borrows stay the R13 abandoned corner` | `stay unflushed` |
| `crates/ds-vm/tests/calls.rs:575` | `word in the same classification is the R11 CALLS shape` | `is the CALLS word shape` (the opcode name is code) |
| `crates/ds-check/tests/conformance_gate.rs:1392` | `tag_resolution.ds: the F9.5 phase-1 re-key round - reworked at the timer race's landing` | `tag_resolution.ds: the tag re-key beside the ambient's own timeout fn:` (the four clauses after it already state the pins) |

Two audit-flagged candidates turned out *not* to need inlining because the gloss is in the next line: `crates/ds-lower/src/func/calls.rs:1749` (`the R13 native-tail shape` - followed by `stack-identical and count-exact`) and `ds-lower/tests/callable_e2e.rs:230` (same phrase, followed by the dec/window clause). `crates/ds-cli/tests/fmt.rs:101` (`the R20 synthetic path`) needs a one-word substitution (`the synthesized entry path`), not an inline.

### Decision rule (no doc access)

A fixer decides with three tests, in order, reading only the comment and the code in front of it:

1. **Resolve test** - after deleting the token, does the noun phrase name something in this crate (a type, fn, field, opcode, table, enum variant) or something the adjacent clause of the same comment defines? Yes → delete (A1/A2-lite).
2. **Gloss test** - is the token a milestone (`M0`-`M9`) or a phase word (`round`, `this round`, `ownership round`)? Yes → delete the word, keep the concrete fact (A4).
3. **Why test** - does the clause exist only to name an asymmetry, clamp, refusal or corner (`the R13 abandoned corner`, `the R20 clamp`)? Then write the one-clause fact: read the code (the arithmetic that clamps, the flag that is not flushed) and state it. Never read `docs/en/*` to fill the clause. If the code doesn't determine the fact either, delete the clause - the argument belongs to the commit message.

Authority rule: **the code is the only source.** A pass must not open the design record, the todo queue or `pitfalls.md` while rewriting. This is what makes "inline vs delete" decidable per row without the doc, and it prevents the rewrite from importing a second doc dependency (a paraphrased ruling is still a doc paraphrase that rots at the next ruling).

Non-obvious couplings a fixer must check before editing (these are the only places where a comment is *behavior*):

- `crates/ds-diag/src/code.rs` doc comments are the error table text: `code.doc()` feeds `ds doc --errors` → `docs/en/errors.md`, drift-pinned by `ds-cli/tests/doc.rs:the_error_table_is_the_registrys_own_bytes`. Rewriting these 43 rows requires `target/debug/ds doc --errors > docs/en/errors.md` in the same commit.
- `crates/ds-vm/src/lib.rs:105-106` states the citation convention. Delete it in the same commit as the last ID tag of `ds-vm`, never earlier.
- Rust `#[error("...")]` and `expect("...")` strings carry the same narration but are not comments (`crates/ds-ffi/src/exports.rs:22,25,227`, pinned by `crates/ds-ffi/tests/exports_malformed.rs:214`). Out of scope for every pass here; a separate behavior-changing pass needs the operator's sign-off.
- `.ds` sources are published verbatim by `ds doc` into `docs/en/reference.md`; the audit did not cover them. Out of scope, candidate for a follow-up sweep.

## 3. The guard: `tools/check_comment_refs.py`

### Placement

`tools/` already holds exactly this kind of analyzer: `tools/gc_targets.py`, a stdlib-only Python 3.9+ script with a `--self-test` flag and a sibling `tools/test_gc_targets.py`. Follow it, and add the first Python step to CI (`ci.yml` has no Python job today):

```
comment-refs:                 # new job in .github/workflows/ci.yml
  runs-on: ubuntu-latest
  timeout-minutes: 5
  steps:
    - uses: actions/checkout@v7
    - run: python3 tools/check_comment_refs.py --self-test
    - run: python3 tools/check_comment_refs.py
```

No cargo, no toolchain: the job is seconds and runs beside `fmt`. The scanner is the same comment-only state machine the audit used (full-line `//`, `///`, `//!`, and `/* */` bodies; string literals never enter the text), over `crates/**/*.rs`, `fuzz/**/*.rs`, `benchmarks/**/*.rs`, `target/` excluded. A trailing comment after code is out of reach of this scanner by design (missed violations are caught on the next full-line edit); do not grow a parser for it.

### Rules: two tiers

**Tier 1 - deny, must stay at zero.** No legitimate occurrence anywhere in the tree; a hit fails the job with `file:line: rule: text`.

| id | regex | today |
|---|---|---|
| `doc-path` | `docs/en/\|design-decisions\|implementation-plan\|status\.md\|todo\.md\|pitfalls\|memory-arc\|tutorial\.md\|(?:conformance/)?README\|\bdesign record\b` | 23 lines |
| `doc-number` | `\bsections? \d\|\bitems? \d+\b\|§\|\brecord \d+\.\d+` | 101 |
| `rule-id` | `(?<![\w.$])(?:BL-[1-9]\|[BCDEFGH][0-9](?:\.[0-9])?\|M[0-9]\|R[0-9]{1,2})(?![\w])` | 2,266 |
| `date` | `\b20\d\d-\d\d-\d\d\b` | 199 |
| `ruling` | `\bruled\b\|\brulings?\b` | 268 |
| `work-item` | `\btodo \d+\b\|\bslice \d+\b\|\bissue #?\d+\b\|\bround \d+\b\|(?<![$\w])#[0-9]+` | 3 |
| `narration` | `\bno longer\b\|\bpreviously\b\|\bnow that\b\|\bthis round\b\|\bthe (?:[a-z-]+ )?round'?s\b\|\bownership round\b\|\bsuperseded\b\|\bresiduals?\b` | 135 |

**Tier 2 - budget.** Polysemous words that also carry correct code meaning. Frozen in `tools/comment_refs_budget.txt` (`rule count` per line), failing only when a count rises; each crate pass lowers its own line. This is what stops the regrowth without corrupting correct comments.

| id | regex | today |
|---|---|---|
| `landed` | `\blanded\b` | 80 |
| `retired` | `\bretire[sd]\b\|\bretiring\b` | 162 |
| `per-the` | `\bper the\b` | 63 |
| `the-record` | `\bthe record\b` | 53 |
| `the-contract` | `\bthe contract\b` | 25 |
| `round` | `\brounds?\b` minus `round-trip\|round trip\|rounding` | 411 (512 raw) |

### Frozen allow-list (each needs a reason in the script)

| path | rule | reason |
|---|---|---|
| `crates/ds-cli/src/doc.rs`, `crates/ds-cli/tests/doc.rs` | `doc-path` | `docs/en/reference.md` and `docs/en/errors.md` are the dumper's own output paths; the drift tests read them from disk (`crates/ds-cli/tests/doc.rs:30,31,34,45,48`) |
| `crates/ds-vm/src/tz.rs:157` | `rule-id` `C11` | C standard Annex K argument order, an external spec |
| all files | `work-item` `$t#N` | the `(?<![$\w])` lookbehind already excludes type-variable tag syntax (`ds-check/src/instance_solve.rs:42,43,362,808`); keep the fixture that proves it |

### Known false-positive classes (catalogued, from audit section 8 plus this re-run)

The audit's own raw-count trap: `land(ed|ing|s)` matches 593 comment lines, 512 ordinary English; `per the` 54/149 correct; `the record` 54/78 a data record; `retired` 43/76 runtime slot/row retirement in `ds-vm/src/module.rs` and `object.rs`. Each is why that word is tier 2. Tier 1 was re-run over the tree and is clean of these:

- `rule-id` finds 2,266 lines and **69 distinct tokens, all 69 in the audit legend except 10**: `F9.5` (77), `G3.5` (32), `H5.5` (19), `R13` (6), `R20` (3), `H3.5` (3), `R25` (2), `R15` (1), `R6` (1), `R11` (1). Those 10 are the undefined tokens - flagging them is the point (they are exactly the inline set of section 2). The single digit form deliberately does not match `F64`/`I64`/`U32` width tokens and `B_caller`; `C11` is the one allow.
- `narration`'s `until the [A-Z]` shape was **dropped** from the deny list after probing: 3 of its 7 hits are correct (`until the OS answers would-block`, `until the VM drops`, `until the JIT exists`). Milestone forms (`until the M7 loading round's`) are caught by `rule-id` plus the `round` budget instead.
- `doc-number`'s `§` branch has 0 hits today; keep it because prose drafts reintroduce it.
- `\brounds?\b` at 411 lines is the most FP-prone rule in the set (`the half rounds away from zero`, `between rounds`, `executor poll rounds`, `defends its round`); it stays a budget rule, never a deny.
- The audit's suggested rewrites are not always safe to apply verbatim: its `ROT` template ("replace the phase reference with the concrete fact `what the ambient cannot carry`") is wrong for the 54 reload-round rows in section 0. Fixers must rewrite, not copy the template.

### Self-test

`tools/test_check_comment_refs.py` carries a table of (comment line, expected rule ids) covering: every tier-1 rule positive; `C11`, `$t#1`, `N1/N2/N3`, `F64`, `B_caller`, `round-trip`, `the half rounds away from zero`, `the record` (data), `install landed` as negatives; the dumper-path allow; and the budget parser. `--self-test` runs it and fails the CI job. A rule edit that starts flagging `until the OS` fails there, not in a review.

### Rollout

1. Land the script + self-test + CI job with budgets set to today's counts (the job passes immediately, so it can land first).
2. Each crate pass lowers its own budget lines in the same commit (`docs`-style ratchet); the review reads the diff against the budget file.
3. When the tail pass closes, tier 1 counts are zero and every tier-2 count sits at the frozen class-B catalogue; only then delete the tier-1 section of the budget file.

## 4. Sample patch: `crates/ds-ffi` (14 comment lines, 5 files)

Slice chosen because it is a leaf crate (453 comment lines, no dependents on its internals), 12 audited comment lines (13 rows), and 0 class-B/C rows - the whole crate can be verified in one read. Two of the 14 lines below are **not in the audit** (`lib.rs:89`, `marshal.rs:16`) and are included because the same convention removes them. Every line below was read in the source; the replacements state what `Param`, `Ret`, `ArgWord` and `ExportError` actually do in this crate.

```diff
--- a/crates/ds-ffi/src/lib.rs
+++ b/crates/ds-ffi/src/lib.rs
@@ line 1
-//! DreamScript's FFI substrate (H5): binding a native library's C
+//! DreamScript's FFI substrate: binding a native library's C
@@ line 14 (line 15 continues "contract clauses, holding from the first bound library: a bound")
-//! word per its own kind onto it. Its H5
+//! word per its own kind onto it. Its ABI
@@ line 20 (line 19 ends "... string/bytes marshaling are")
-//! the ownership round's own refusals until it lands.
+//! outside this face.
@@ line 89 (line 90 continues "transmutes into a typed call.")
-/// One bound symbol: its address alone, the shape a marshaling round
+/// One bound symbol: its address alone, the shape the marshaling face

--- a/crates/ds-ffi/src/marshal.rs
+++ b/crates/ds-ffi/src/marshal.rs
@@ line 1
-//! The marshaling round's widened C-ABI encoding (H5): the signature
+//! The widened C-ABI encoding: the signature
@@ line 16 (line 15 ends "... and")
-//! deinit-carrying handles stay queued with the ownership round.
+//! deinit-carrying handles are outside this face.
@@ line 28
-/// One extern parameter's marshaling encoding (H5): the scalar words
+/// One extern parameter's marshaling encoding: the scalar words
@@ line 57
-/// One extern return's marshaling encoding (H5): a scalar word or the
+/// One extern return's marshaling encoding: a scalar word or the
@@ line 147
-    /// Calls the bound symbol over one marshaled signature (H5): the
+    /// Calls the bound symbol over one marshaled signature: the

--- a/crates/ds-ffi/src/sys.rs
+++ b/crates/ds-ffi/src/sys.rs
@@ line 1
-//! The OS loader primitives, raw and cfg-split (H5): dlopen/dlsym/dlclose
+//! The OS loader primitives, raw and cfg-split: dlopen/dlsym/dlclose

--- a/crates/ds-ffi/src/word.rs
+++ b/crates/ds-ffi/src/word.rs
@@ line 1
-//! The enumerated per-register-class transmute (H5), shared by every
+//! The enumerated per-register-class transmute, shared by every
@@ line 90
-/// The per-placement call (H5): the macro takes the staged words, the
+/// The per-placement call: the macro takes the staged words, the

--- a/crates/ds-ffi/src/exports.rs
+++ b/crates/ds-ffi/src/exports.rs
@@ line 20
-    /// A Mach-O library, whose export table this round does not read
+    /// A Mach-O library, whose export table the reader does not parse
     /// (the matrix carries the PE and ELF legs).
@@ line 24
-    /// A 32-bit ELF library: this round reads ELF64 alone.
+    /// A 32-bit ELF library: the reader parses ELF64 alone.
```

Counts: bucket A1 8 (the `(H5)` tags), bucket A2 2 (`Its H5` → `Its ABI`, `the shape a marshaling round` → `the shape the marshaling face`), bucket A4 3 (two `round` phrases, one `until it lands`), class C 0, class B 0. 14 lines changed: all 12 distinct audited lines of the crate, plus the 2 the sweep missed (`lib.rs:89`, `marshal.rs:16`). The crate has no other audit rows; see the excluded items below.

Verification (comment-only, so zero behavior risk; `ds_pins` instruction counts and the doc-drift tests are unaffected because no string literal and no `.ds` source moves):

```
cargo fmt -p ds-ffi && git diff --stat          # wrap_comments = true reflows; expect the same
                                                # paragraph with line breaks re-chosen, <= 100 cols
flock -s /tmp/myflock.lock cargo check -p ds-ffi
cargo fmt --all --check
flock -s /tmp/myflock.lock cargo test -p ds-ffi  # belt and braces: exports_malformed still pins the
                                                 # #[error] strings, which this slice does not touch
```

Risk: `low`. Changes are comment text only. The one caveat is `wrap_comments = true` in `rustfmt.toml`: rustfmt reflows doc paragraphs, so the diff after `cargo fmt` may join or split lines around the edited words. That is expected; the content above is the contract, line breaks are rustfmt's.

Not in the slice (flagged, needs a decision before anyone touches them):

- `crates/ds-ffi/src/exports.rs:22,25,227` - the same "this round" narration inside `#[error("...")]` strings. Changing it changes diagnostic text and the golden at `crates/ds-ffi/tests/exports_malformed.rs:214`. Not comment work.
- `crates/ds-ffi/src/exports.rs:54,220` - `says which round is missing`, `refuses its round`. "round" here reads as the parser's pass, not the project phase; the meaning is unclear and the sample leaves it (the `round` budget rule will count it, which is the correct outcome for an unresolved phrase).

## 5. Execution order

One commit per crate, in the audit's volume order, with two changes: `ds-ffi` first (proves the convention on 14 lines), and the guard script in the first commit's PR so each later commit visibly ratchets the budget.

| # | crate | A rows | bucket mix | note |
|---|---|---|---|---|
| 0 | `ds-ffi` | 13 | A1 8, A2 2, A4 3 | section 4 |
| 1 | `tools/check_comment_refs.py` + CI job | - | - | budgets at today's counts, warn only |
| 2 | `ds-check` | 968 | A1 663, A2 231, A3 30, A4 34, A6 15 | 11 `section 10 item N` rows; `B1/B6` decision-procedure prose |
| 3 | `ds-vm` | 515 | A1 243, A2 171, A3 4, A4 37, A6 57 | the date block in `net.rs`, the `lib.rs` convention comment last |
| 4 | `ds-lower` | 411 | A1 264, A2 98, A3 3, A4 37, A6 7, rest 2 | `R13` rows here need the inline | 
| 5 | `ds-cli` | 263 | A1 108, A2 61, A3 8, A4 28 (46 reload-round rows reclassified B), A6 9 | reload-round rows stay |
| 6 | `ds-abi` | 157 | A1 91, A2 51, A3 7, A6 7 | the LOCKED/item narration in `lib.rs:37,45` |
| 7 | `ds-parse` | 141 | A1 97, A2 37, A3 4, A6 0 | `ast.rs` 77 tags, one doc path in `lib.rs` |
| 8 | `ds-query` | 146 | A1 88, A2 49, A6 4 | `H6 item 1` |
| 9 | `ds-meta` | 35 | A1 17, A2 13 | 50 class-C date rows in `derive.rs`: the biggest single delete |
| 10 | `ds-mod`, `ds-rt` | 123 | A1 51, A2 67 | justification/record pointers in both `lib.rs` |
| 11 | `ds-diag` + regenerate `errors.md` | 42 | A1 30, A2 7 | `code.rs` doc comments are the emitted table |
| 12 | tail (`ds-fmt` 32, `ds-stdlib` 27, `ds-dsi` 14, `ds-lsp` 14, `ds-ffi` 0, `ds-pins` 7, `ds-pkg` 7, fuzz 21) | 122 | mostly A1 | |
| 13 | flip tier 1 to zero, freeze budgets | - | - | section 3 rollout step 3 |

## 6. Residual risks

- **The scan is comment-line based**, so narration inside string literals (`#[error]`, `expect`, `to_owned`) survives every pass and will be caught by no guard here. That is deliberate: those strings are behavior with goldens.
- **`wrap_comments = true`** means every pass produces a larger diff than the word count suggests; reviewers must read content, not line counts.
- **The bucket counts are per-row**, so a line carrying an ID and a date receives two edits; a per-line acceptance check ("0 tier-1 hits in this crate") is the objective gate, not the row count.
- **`docs/en/errors.md` regeneration** is the only artifact write any pass needs; nothing else in `docs/` is touched, and `docs/en/reference.md` moves only if `.ds` comments move (out of scope).
- The audit's 10-row reconciliation gap is not chased: the appendixes are the working set and the guard measures the real state at each commit.
