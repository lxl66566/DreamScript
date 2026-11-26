# Doc-carried language rulings vs. the code that enforces them

Read-only survey of `/root/programs/DreamScript`. Nothing modified, nothing built. Question: for the language rulings carried in `docs/en/design-decisions.md` + `docs/en/tutorial.md`, which should be restated as a comment where the compiler enforces them, and which are already stated by code (so a comment would duplicate)?

Scope sampled, not swept: `crates/ds-abi` (`lib.rs`, `uop/mod.rs`, `uop/inst.rs`, `layout.rs`, `descriptor.rs`, `tailcall.rs`, `meta.rs`), `crates/ds-mod` (`lib.rs`, `write.rs`, `read.rs`), `crates/ds-parse` (`lib.rs`, `grammar.pest`, `ast.rs`, `guardtail.rs`, `arrowbudget.rs`, `tests/parse_suite.rs`), `crates/ds-check` (`lib.rs`, `instance.rs`, `resolve.rs`, `pattern.rs`, `pooled.rs`, `extraction.rs`, `type_of/mod.rs`, `type_of/ops.rs`, `corpus.rs`), `crates/ds-diag/src/code.rs`, `crates/ds-vm` (`vm.rs`, `object.rs`, `lib.rs`, `native.rs`), `crates/ds-ffi` (`word.rs`, `marshal.rs`), `crates/ds-rt/src/lib.rs`, `crates/ds-lower` (`func.rs`, `func/calls.rs`, `shape.rs`), `crates/ds-stdlib` (`lib.rs`, `ambient.ds`), plus the two pitfalls files named in the task.

## Headline

The heavy crates are already spec-as-code. `ds-abi` calls itself "spec-as-code: the portable contract", `ds-mod` writes the whole byte layout into its crate docs, `ds-check::instance` writes the coherence rule and its probes into the module docs, `ds-check::lib` writes the set-core normal form and the decision procedures into the module docs, and `ds-diag::code` puts every refusal's ruling on the refusal's own registry entry — from which `docs/en/errors.md` regenerates. So for the majority of the numbered rulings the answer is **IN-CODE-ALREADY**, and the corresponding doc paragraph is a **DEDUPE**: a new comment at the enforcement site would duplicate text that is already there, and `design-decisions.md` mostly restates what the crate docs already say.

What is genuinely missing is (a) the few rulings whose only code trace is an *absence* or an *idiom*, (b) the process-level lock facts that currently name a document path as their whole content, and (c) three live drifts between record and code (section 6).

Counts across the sampled surface: 20 IN-CODE-ALREADY rows, 13 MOVE rows, 7 do-not-inline categories, 4 drift rows.

## 1. MOVE: state the ruling where the compiler enforces it

Proposed comments are <=3 lines, factual, self-contained, no doc path, no decision ID. Each names the failure prevented.

| Ruling (record / tutorial) | Verdict | Enforcing site | Proposed comment |
|---|---|---|---|
| The accepted surface is closed; the excluded spellings have no grammar | MOVE | `crates/ds-parse/src/lib.rs:1-3` (cites `docs/en/design-decisions.md` + nine IDs and never says what the surface excludes) | `// The surface is closed: bindings, fn declarations, traits/impls, one fixed operator set. Custom operators, subscript slicing, vararg/spread, `or`, `??` and `?.` have no spelling — they do not parse.` |
| One frozen C-family precedence table, published once (F5) | MOVE | `crates/ds-parse/src/grammar.pest:197-200` (describes the chain, never says the order is frozen; the comment also still lists an "or-extraction" level that has no rule in the grammar) | `// The order below is the frozen precedence table: each level binds tighter than the one above it. Reordering a level reparses every expression silently; adding one is a surface change.` |
| No subscript slicing (F7/F8, tutorial "permanently excluded") | MOVE | `crates/ds-parse/src/grammar.pest:265-269` (`postfix_index` accepts one `ctor_arg`, so the absence is the only record) | `// An index takes exactly one operand: `xs[a..b]` is a range operand, not a slicing form, and the checker refuses it as a non-integer index.` |
| `in` types against the element of a list application; other non-range right sides refuse (F7/F8 — see drift D1) | MOVE | `crates/ds-check/src/type_of/ops.rs:394-421` (comment cites `[F7]` for a rule the record states as permanently refused) | `// A list-application right side types against the element type; every other non-range right side refuses as DS2017. A range right side is routed before this arm.` |
| `in` right side must be a range or a list | MOVE | `crates/ds-diag/src/code.rs:161-162` (registry doc line — the source `docs/en/errors.md` regenerates from — still says "collection membership is not ruled yet") | `/// `in` right-hand side is neither a range nor a list; membership over other collections refuses.` |
| ABI items 1-6 are locked; a change needs written justification | MOVE | `crates/ds-abi/src/lib.rs:44-47` (the justification's *location* is the whole content: "recorded in `docs/en/design-decisions.md` before the change lands") | `// Locked: three tiers and the bytecode format consume these words, so a change to any item needs written justification before it lands.` |
| The dsmod byte layout is locked (BL-7, G3.5) | MOVE | `crates/ds-mod/src/lib.rs:3-7` (same shape: path + "before the code changes") | `// Locked layout: readers, writers and installed artifacts share these bytes, so a header, framing or constants change needs written justification and a format-version decision.` |
| Decision procedures decide in linear time over the finite atom universe (B1/B6, record section 10 item 7) | MOVE | `crates/ds-check/src/lib.rs:39` (heading "Decision procedures (B1, B6, section 10 item 7)" — the section/item number is the only referent) | `// Decision procedures: emptiness, disjointness, containment, witness. Each is a linear scan over the solved finite atom set, so the link can re-run all four over the closed instance environment.` |
| Conditional instances are rejected on a structural cycle; the link re-probes (B6) | MOVE | `crates/ds-check/src/instance.rs:58` ("this is the structural cycle check of design-decisions section 10 item 7") | `// A conditional instance whose condition edges reach back to itself is a structural cycle and is rejected; the link re-runs the same probe over the closed instance environment.` |
| No GC, no write barriers, no safepoints or stack maps in any tier (C4/H4) | MOVE | `crates/ds-rt/src/lib.rs:1-4` (the invariant exists nowhere in code; a "let us add a safepoint" change would not contradict a single comment) | `// No collector exists: counts are precise, non-atomic and thread-confined, so no stack scan, safepoint, write barrier or GC map is ever needed, and frees stay deterministic.` |
| Arena contract: mark/restore on every pop path, chunk-append growth keeps captured frame addresses stable, marks are per-fiber (C4/H4) | MOVE | `crates/ds-rt/src/lib.rs:6-8` (the sentence's payload is "the arena module's docs carry the full contract mapping from the design record ([C4], [H4] in docs/…)" — a pointer in place of the facts) | `// Arenas: mark/save at entry, bump, restore on every pop path; chunk-append growth keeps captured frame addresses stable; marks are per-fiber, so a fiber switch needs no arena action.` |
| Primitive atoms: `int/float/string/bool/char` ambient tags mapping onto slot types, `byte` an I64 0-255 outside the int conversion family (F6) | MOVE | `crates/ds-lower/src/shape.rs:36-39` (the fact is stated, but sourced to `conformance/README.md`, so the paragraph rots when the README moves) | `// The ambient declares int, float, string, bool, char as tags; the lowerer maps them onto the same-named slot types (char rides the string Ref). byte rides I64 as its 0-255 value, outside the int conversion family.` |
| The ambient is the corpus's assumed stdlib; the `os`/`c` surfaces are out of scope (H5.5) | MOVE | `crates/ds-stdlib/src/lib.rs:39-42` (sourced to `conformance/README.md`, "Ambient names") | `// Scope: the compiler-required face plus comptime reflection (the field record, fields/type_name). In scope: everything @std resolves. Out of scope: the os build target and the c FFI namespace.` |
| A chained comparison `a < b < c` "is not an expression" (tutorial, F8) | MOVE | `crates/ds-check/src/type_of/ops.rs:369-385` (the grammar's `cmp_lvl = shift_lvl ~ cmp_tail*` *does* parse the chain; it refuses here only because ordering demands one numeric type on both sides. Nothing pins that, no test covers it) | `// Ordering needs one numeric type on both sides, which is also what refuses a chained comparison: `(a < b) < c` types a bool operand here.` |

## 2. IN-CODE-ALREADY: a comment at the enforcement site would duplicate code that is already there

The record paragraph is then a DEDUPE, not a MOVE target.

| Ruling | Where the code states it |
|---|---|
| Integer overflow, `1/0`, `int_min / -1` trap; `+% -% *%` wrap; shifts outside `0..64` trap, `>>` sign-propagating, `& ^ \|` never trap (B7) | `crates/ds-abi/src/uop/mod.rs:205-247` (per-uop doc lines), `crates/ds-vm/src/vm.rs:2295` (DIVI64's two distinct traps) |
| Floats follow IEEE and never trap; `nan == nan` bit-structurally true (B7, B9) | `crates/ds-abi/src/uop/mod.rs:250-254,286-288`, `crates/ds-vm/src/vm.rs:10235` |
| Typed opcodes, no dynamic "V" variants, 32-bit fixed format, biased jumps, XOR-negated pairs (BL-1, BL-5) | `crates/ds-abi/src/uop/mod.rs:1-40` (encoding diagram + field map) |
| Frames inline in a typed stack; slot-overlay call; CALLT slide with no stack growth; metadata word (BL-6, H2) | `crates/ds-abi/src/lib.rs:9-40`, `crates/ds-abi/src/layout.rs:1-90` (owner-relative word mapping, pinned by a test) |
| Unboxed slots; tag+payload union layout; callable word classification and the one dynamic-dispatch point (BL-2, F2) | `crates/ds-abi/src/layout.rs:96-110` and the `SlotType` doc lines |
| Two-word entry descriptor, one-way tier flip, release/acquire publication, hot-reload slot (H1, H6) | `crates/ds-abi/src/descriptor.rs:1-50` |
| Tail-call guarantee incl. mutual/self and the O(delta) slide (H2) | `crates/ds-abi/src/tailcall.rs:1-60`, `crates/ds-abi/src/lib.rs:20-30` |
| Fixed-width, position-independent, mmap-able dsmod; dep list as link table; FuncRef addressed by dep position; source hash validated (BL-7, G3.5) | `crates/ds-mod/src/lib.rs:1-47` (full byte layout), `crates/ds-mod/src/read.rs:390-410` |
| Decode on dispatch; load decode-checks once; the decoder stays the canonicity authority (H1) | `crates/ds-vm/src/vm.rs:5-17` |
| Non-atomic precise RC, one store per fiber on one thread, cross-heap requests through the pending-foreign queue (C4) | `crates/ds-vm/src/object.rs:43-54` |
| Coherence = pairwise disjointness of one trait's heads; no orphan rule; any module may instance any type (B6) | `crates/ds-check/src/instance.rs:1-32` |
| Ambiguity is a refusal, never a priority search; `Trait::f` escape hatch; capability parameters excluded from the dot position (F3, F9) | `crates/ds-check/src/resolve.rs:20-40,2707-2740` |
| Within one module a label names at most one tag declaration; `type::label` is the qualified escape (F9.5) | `crates/ds-check/src/resolve.rs:1999-2020`, `crates/ds-parse/src/grammar.pest:188-195` |
| Differences erase at compile time; `T?` is exactly `T | none`; tuple products distribute (B1, B3, B4) | `crates/ds-check/src/lib.rs:12-40`, `crates/ds-parse/src/ast.rs:508-571` |
| Refinement narrows monotonically; no narrowing through mutable fields of union-typed values (B2) | `crates/ds-check/src/type_of/mod.rs:23-28` |
| A value-level arm subtracts nothing, so later arms check the full scrutinee (F4/B2) | `crates/ds-check/src/type_of/mod.rs:8245`, `crates/ds-check/src/type_of/pattern.rs:643` |
| Frozen loop element count + DS3007 (D3) | `crates/ds-lower/src/func.rs:5067-5140`, `crates/ds-check/src/type_of/mod.rs:5690-5705` |
| `spawn` only inside a nursery; spawned-body mut-window refusal DS3008 (E2) | `crates/ds-lower/src/func.rs:6295-6305`, `crates/ds-check/src/type_of/mod.rs:5715-5745` |
| Pooled child: no sibling fiber control, no world surface (E3, permanent) | `crates/ds-check/src/pooled.rs:80-118` (the diagnostics carry the rule) |
| One seed per part, `-0.0` normalized, every NaN payload normalized (B9) | `crates/ds-vm/src/native.rs:632` |
| Every refusal's ruling, one per registry entry, and `errors.md` regenerates from it | `crates/ds-diag/src/code.rs:96-250` (append-only, `code_table_snapshot` pins the table) |
| The u32 width construction: `u32.of(x)` is the spelling, `u32` carries no literal or constructor (B7) | `crates/ds-stdlib/src/ambient.ds:564-572,599-600`, `crates/ds-stdlib/src/lib.rs:73-76` |
| FP-placement mask bit i = word i, capped before dispatch (H5) | `crates/ds-ffi/src/marshal.rs:163-193`, `crates/ds-ffi/src/word.rs:1-12` |
| Guard-tail collision quirk and its repair; the two unbracketed recursion budgets (F8, F1) | `crates/ds-parse/src/guardtail.rs:1-16`, `crates/ds-parse/src/arrowbudget.rs:1-20` |
| Extraction family types locally by name, never through generic inference (E4) | `crates/ds-check/src/extraction.rs:1-30` |

## 3. Do not inline (pure rationale — stays in `design-decisions.md`)

Categories, not individual rows. Inlining any of these puts an argument where the code needs a behavior, and the argument rots the moment the code changes.

1. **Rejected alternatives, with their reasons.** Serve-and-patch, symbolic dsmod, canonical name-sorted minting, a sidecar order manifest, process-global interning, content-hash ids, `getsockopt(SO_ERROR)`, a third-party-default link rule, `assoc` as a marker trait, a GC/lifetime escape hatch, socket2. Reason: a rejection is not a behavior; no site can enforce "we did not do this other way".
2. **Recorded fallbacks and their quantified triggers.** B1b nominal-only core, F1b brace headlines, H1b single method-JIT tier, C4b generational references, user-defined handlers, inferred error members on non-`pub` fns, `[sealed]`, multi-version coexistence. Reason: a trigger is a queued condition that expires; a comment describing a future or dormant behavior violates the comment rule directly.
3. **Measurements and their provenance.** The fuel quanta choice (+0.06 %..+0.21 % whole-process Ir, the nine pins, the re-baseline), the ~114 KiB conversion recursion cost, the 16384-edge latency bound, "~14 % faster than LLVM -O0 at ~100x its compile speed". Reason: the numbers live with the harness that measures them; a doc comment that quotes a measurement drifts on the next landing.
4. **Budget accounting.** F7's 22 features / 34 units against Kotlin's 38 / 74-76, the "explainability budget", the sugar-ceiling reasoning for excluding `pub(in ...)`. Reason: an accounting exercise, not an enforced rule.
5. **Policy and product priorities.** A2 (CLI/tools first, games second), A3 (rapid-break until 1.0), workload ordering, "no `any` fallback" as a philosophy, the "one word one meaning" principle. Reason: policy that the code only *instances*, never *states*.
6. **The exhaustive negative surface list as prose** ("deliberately absent" in the tutorial). Reason: an enumeration of what does not parse has no enforcing site to sit on — only the grammar's silence — and an inline copy rots silently. Concede the shape only where a *class* has a live site (the parity rules in section 1's first two rows); the exhaustive list stays in `tutorial.md`, which is the public surface.
7. **The citation vocabulary itself** (bracketed `[B7]`, `[F9.5]`, "(ruled 2026-11-25)", "landed", "retired"). Reason: it is the audit's other work item, not a ruling; deleting the token must not delete the sentence that carries it.

## 4. Drift: decision without implementation, or implementation without decision

Ordered by how silently each would mislead a reader.

| # | Drift | Evidence | Fix direction |
|---|---|---|---|
| D1 | `in` over a list is implemented and tested, but F8 says `in` is "range-wired in v1 with a non-range right side refused, collection membership permanently a @std method (`xs.contains(x)`)". Three-way disagreement: the record (permanently out), the registry (`DS2017` = "not ruled yet"), the code + tests (list membership ruled). | `crates/ds-check/src/type_of/ops.rs:394-421`; `crates/ds-check/tests/byte_in.rs:150-175`; `crates/ds-lower/tests/list_for_in.rs:197-220`; `crates/ds-diag/src/code.rs:161` | Amend F8 to the implemented rule and move the `contains`-only stance to `.contains` for other collections. The registry line and the checker message change with it (errors.md regenerates). |
| D2 | A `.err` handler whose answer re-adds a handled member is recorded in `docs/en/pitfalls/language-surface.md` as a live lowering bug ("the fix is a checker-recorded handled set … the queued todo item owns it"). The fix is landed and pinned end to end. | `crates/ds-lower/src/func/calls.rs:5085-5097` (reads `handler_handled`, difference only as fallback), `crates/ds-cli/tests/collections_face.rs:108-110`, fixture `crates/ds-cli/tests/fixtures/handler_seam_faces.ds:2` | Delete the pitfall row: the doc carries an open bug the code closes. |
| D3 | The frozen precedence comment in the grammar names an "or-extraction" level (`assign > or-extraction > range > …`) that no rule implements; the `or` operator was retired, so the comment describes a level that cannot be parsed. | `crates/ds-parse/src/grammar.pest:198` vs `assign_lvl = { range_lvl ~ … }` at `:206` | Rewrite the line (section 1, row 2). |
| D4 | The tutorial states chained comparisons are "not an expression", but the grammar parses `a < b < c`; only the ordering rule refuses it, incidentally, with no dedicated diagnostic and no test. A future bool ordering would silently legalize the chain. | `crates/ds-parse/src/grammar.pest:226-228` (`cmp_lvl = { shift_lvl ~ cmp_tail* }`), `crates/ds-check/src/type_of/ops.rs:369-385`, no test hits `a < b < c` | Either pin a `ds-check` test asserting the DS2001 refusal, or make the conversion refuse a comparison chain outright. |

Nothing else in the sample is a decision without implementation: `.ok` minus-`none` defaulting (`type_of/mod.rs:6913`), `u32.of` (`ambient.ds:599`), the F9.5 qualified spelling, named instances, the link layering, the duration/fuel quanta, and the comptime keyed field all have their enforcing site and their site-level comment.

## 5. DEDUPE: where `design-decisions.md` can shrink

These doc paragraphs restate what the crate docs already carry verbatim in code, so a comment would duplicate and the doc's copy is the maintenance liability:

- **B7's trap/wrap set** — every clause is a per-uop doc line in `ds-abi/src/uop/mod.rs`; the doc keeps only the ruling's *why* (trapping beats wrapping for a scripting default) and drops the enumeration.
- **BL-1, BL-2, BL-6, BL-7 as prose** — `ds-abi/src/lib.rs` items 1-6, `layout.rs`, `uop/mod.rs` and `ds-mod/src/lib.rs` byte tables are the normative statements; the record's baseline section can point at "the ABI contract" instead of restating the encodings.
- **G3.5's format facts** (header fields, dep list, source hash, section directory, constants tags, FuncRef addressing) — all in `ds-mod/src/lib.rs:9-47`; keep only the decisions that are *not* in the format (content addressing, the pin as the only lever, the refused alternatives).
- **H2's slide, descriptor and tail-chain story** — `ds-abi/src/lib.rs:9-40`, `layout.rs:41-70`, `descriptor.rs:28-50`, `meta.rs`; the record keeps the guarantee and the tier independence claim.
- **B6's coherence rule, no-orphan rule and empty-head canonicalization** — `ds-check/src/instance.rs:1-40`; keep the pairwise-disjointness *rationale* (why no priority machinery) in the record.
- **F2's callable-word classification** — `ds-abi/src/layout.rs:96-120` (scalar `FuncRef` vs closure Ref, callable-defaulted parameter, the one dynamic-dispatch point); the record keeps the capture/escape boundary rules that no slot type can express.
- **F5's subscript routing and F6's join/split** — the ambient (`std/…`) and the lowerer's CALL seam state the current spelling; the record keeps the *ruling date* and the alternatives.
- **Every `DSnnnn` description** — `errors.md` is generated, so a doc paragraph that merely paraphrases a refusal code is a third copy.

## 6. Rule of thumb for the cleanup

1. If the ruling class is a refusal, its home is the `ds-diag` registry doc line (`crates/ds-diag/src/code.rs`): it is enforced, rendered into `errors.md`, and pinned by `code_table_snapshot`. Nothing else needs to carry it.
2. If the ruling is a layout, encoding or protocol fact, its home is the owning crate's module doc (`ds-abi`, `ds-mod`): those docs already are the spec, and they are the only place where a diff is forced to touch them.
3. If the ruling is a dispatch/coherence rule, its home is the module doc of the pass that decides it (`ds-check::instance`, `ds-check::resolve`, `ds-check::type_of`).
4. If the ruling is a guarantee with no single deciding pass (TCO, no GC, no safepoints, no shared mutation), state it once at the crate root of the crate that would be tempted to break it (`ds-rt`, `ds-abi`).
5. Everything else — why-not-what, rejected alternatives, measurements, triggers, budgets — stays in `design-decisions.md` and must not be inlined.
