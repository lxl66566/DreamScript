# Pitfall and lesson register

Invariants and lessons that prevent breakage. Each entry was paid for by a failed gate, a miscompile, or a wasted round; decision IDs are kept so an entry can be traced to the record.

### Parallel agents and cargo cache

- One git worktree and branch per agent, additive-only edits to the root `Cargo.toml`, one shared `CARGO_TARGET_DIR` across worktrees, agents never run git in the main checkout, and the coordinator is the only writer in main.
- `Cargo.lock` conflicts on rebase resolve by deleting the file and regenerating it with `cargo check`; after any hand-resolved merge run `cargo clean -p <affected>` plus a fresh check before pushing, since the shared target dir serves stale fingerprints and has masked merge breakages.
- A workspace test can link a parallel worktree's rlib; `touch`-ing the affected crate's sources and rebuilding refreshes the fingerprint without any clean.
- Deeper staleness form: `cargo build -p <downstream>` recompiles the downstream crate and leaves a touched upstream crate stale, and cargo can report fresh while a stale rlib gets linked; the reliable remedy is `cargo clean -p <touched>`, then building the touched crate directly and verifying its `Compiling` line before trusting an all-green suite.
- The shared target dir serves phantom artifacts under two triggers: feature-differentiated builds (`--features ds-vm/diag` beside plain) poisoning each other's ds-abi artifacts, and a public-API change one agent lands while others build the same package from frozen trees (identical metadata hashes share one output path, so builds overwrite the variant-bearing rmeta). Downstream compiles then fail E0004 on a `Uop` variant that exists in no checkout, which a solo `cargo check` cannot reproduce. Iterate against a private `CARGO_TARGET_DIR` during the change window; the shared dir recovers via `cargo clean -p <affected>`, or `touch` + rebuild retries until the release exe lands.
- An interrupted or parallel nextest run leaves an orphaned test exe that locks its own output file and fails relinking with LNK1104; find the exact PID by image name and kill by PID.
- `target/debug/ds` is shared across worktrees and parallel builds clobber it, so run-probes copy it to a private `/tmp` path first.
- Never run `cargo fmt` outside your own worktree; a stray fmt in the main checkout rewrites tracked files and breaks merges.

### CI and gates

- CI runs fmt on nightly only because `rustfmt.toml` uses unstable options; the nightly rustfmt stderr quirk around `single_line_if_else_max_width` is harmless and left as-is.
- Every crate must inherit the workspace package fields, because clippy's cargo lint group fails on missing readme metadata.
- Nightly clippy rejects `map(format!).collect::<String>()` and `push_str(&format!())`; build strings with plain `format!` composition.
- `cargo doc` belongs in the verification loop beside clippy and nextest: a single-token bracket reference such as `[C4]` in a doc comment parses as a broken intra-doc link, so write the parenthesized `(C4)` or a slash form, and silent instances on private items still need cleanup.
- The one true test gate is `cargo nextest run --workspace --all-features`; `--all-targets` fails at listing on `harness = false` benches (nextest executes them during list, exit 104) and `test = false` does not filter explicit target selection; clippy runs `--workspace --all-targets --all-features -- -D warnings`.
- `cargo nextest` name filters match test names, so a new test file is selected with `cargo test --test <file>`.
- Round-closing: corpus noise counts measured in a worktree go stale as parallel landings retire errors, so re-measure every count on the merged main before pushing.
- The host's `GIT_EDITOR` is nvim and interactive git (a bare `rebase --continue`, a commit prompt) hangs the shell-proxy session; kill the nvim PID and rerun with `GIT_EDITOR=true`.

### Diagnostics and goldens

- Union-member render order in diagnostics follows tag allocation order, so goldens are pinned from live output.
- Multi-statement one-line blocks are parse errors; write block statements on separate lines.
- Test assertions on OS error text pin locale-independent facts (the operation plus target prefix, `(os error N)`), never English substrings.
- Checker expression-error poison is subtree-scoped, so a poisoned initializer bound to a name still prints one follow-on at a new use site; threading the poison through `Binding` doubles the state surface.

### Language-surface spellings

- An un-annotated `.err` handler under no contextual expectation consumes the whole union (an empty-use cascade), so fallback sites annotate the handler parameter (`(e: net_err) => ...`).
- `let x = e?` over a fully-contained union reports DS2002 by design: `expected = none` propagates every member so nothing binds; the honest spellings are `return e?` and the final expression.
- `u32 + int` surfaces DS2001 because no conversion callable exists yet, and the B7 message's `u32(x)` suggestion is unimplemented.
- Destructured const binders alias a `var`'s words, so a reassignment through the var is observable through the binders.
- The guard quirk's parenthesization is whole-expression: `x > (m)` before `=>` still steals `(m) =>` as a paren-parameter lambda, so only `if (x > m) =>` parses and a one-name guard (`if m =>`) has no parenthesizable form; DS1016 suggests whole-expression parens while the paren-tail shapes keep the generic error.

### Lowering invariants

- Union payloads share one region per union type: a member claims the lowest free word of its kind so every word keeps a single `SlotType`, and a constructed member zero-writes its dead payload words; declaring dead payload words `Raw` was falsified because the call slide validates staged words against the typed callee params.
- A union-typed parameter stages its whole region at the call, with the argument's checked member or sub-union re-spanned through an anonymous staging temp; a deinit-carrying member refuses there because the temp carries no claim contract.
- An operand's raw span is never returned directly; returns re-span per member into the destination union.
- A dead Fn payload word zero-writes the frame's scalar `FuncRef`, so a union carrying a dead fn member still stages its whole region truthfully.
- Scalar `var` places are `PlaceShape::Word`, `list_new`'s zero-arg call still requires a one-word overlay area at the call site, and float `%` is rejected at the lowering boundary because the locked uop set has `ModI64` only.
- Generic callees route through `GENERIC_NATIVES` and CALLG, because name-routing a monomorphic body for an un-tabled generic miscompiles receivers; tail calls to routed generic natives stay CALLTG-reserved.
- A generic inherent impl's member body claims its evidence words as trailing parameters, so the entering site stages the per-pair serving-body indexes trailing the arguments and the ordinary CALL/CALLT names the full staged count; no instance word exists, so the trait spelling's CALLG path (nargs below `params_len`, one carried word past the parameters) never applies.
- On a tail call the Kimm staging must follow the frame's exit claims, and a frame ending in a tail call stages the exit path's deinit claims before the argument Movs; claims first, arguments after, or a claim's overlay window clobbers the staged argument span (a Ref-expecting callee traps on a staged/expected mismatch, an i64-expecting callee silently receives garbage).
- A construct that re-executes its head forks the placement map before the head (`rc.save` plus `loop_enter`); a Ref cond operand born outside the loop is loop-carried with its count dying at the owning scope's end past the loop, and anonymous carried operands sweep through `drop_unnamed` once per evaluation at the exit.
- for-range keeps the pre-fork head only because its replayed test reads frame-static scalar words; any future Ref-reading head must fork first or the head's read-dec re-fires per back edge and frees the object after the first iteration.
- Every loop registers its exit and backedge labels below its body scope (`LoopCtx`), and `break`/`continue` drop the body scope's binders inwards at the jump from a read-only placement snapshot (`loop_exit_drops`) so each runtime path fires each deinit claim exactly once, while the fall-through path keeps its own scope-pop drops; an exit lexically inside a lambda or spawned body refuses at the checker's frame boundary.
- A bare identifier in a match test position resolves through the type namespace first, so a hit is a member test binding nothing while declaration positions always bind; the predicate lives in three aligned places (resolve `arm_tag`, type_of `unit_pattern_atom`, ds-lower `binding_atom`) that must stay aligned, and tuple elements in an arm walk the same resolution.
- A catchall-free match is accepted at lowering only when no arm can fail (union tag tests cover the dispatch); a value-level chain with literal tests or guards refuses without one unguarded catchall arm, because those tests and guards prove no coverage and the fall-through stays open.
- A non-union match chain runs every arm's tests and guard on the fall-through path in order, snapshots the post-guard state per arm so the bodies fork from exactly what their path executed, and the join's reconcile absorbs the heterogeneous states (a count one guard ate stays eaten on every later path, a count no reached guard ate decs at its own arm's jump).
- A guarded arm over a Ref-carrying scrutinee binds its binder a word-for-word copy inc'd at the section top before its tests, so every path reaching the section incs what its decs will eat; a guard consuming the scrutinee's own Ref word refuses.
- An `is` test over a written type other than a nominal single-segment path lowers through the checker's solved test recorded by the test expression's span (`Checked::is_tests`): a multi-atom solution joins one per-atom TAG_TEST by jumps, a covering set folds true, a disjoint one folds false, and a type variable on either side of the test refuses naming runtime reflection, since only the covered fold is instantiation-stable.
- Placement conservative lifetime: a loop-carried word's textual last use in-body holds its count until the next overwrite or exit, a deliberate extension of the loop-claims stack.
- A span claim folds its span's plain count words and decs every `Ref` word of the span behind the hook, so a folded word never dies on its own; a field-read copy of a folded word borrows (an inc) because the claim keeps its count for the hook, and a copy into the claim's own moved span rides silently.
- A deinit-carrying value stored as a struct field type refuses at the shape layer: the pinned-transfer rule assumes a copy of a claimed base word is the consuming move, but a struct-literal field write is a borrow at the checker's facts, so the transfer steals the claim while the source survives and the hook later reads a freed object (`reference names no heap object` inside the deinit call); per-span deinit claims own the seam.

### Native and platform seam

- Install natives into the registry before `loader::install`, because pool FuncRefs validate at install time; `Vm::with_limit` installs only the builtin core, so the CLI calls `provide_platform` on both run paths.
- Tag ids: module lowering allocates union ids upward from 0 while the platform reserves the u8 top downward (IO_OK 255, IO_ERR 254, NET_ERR 253, PARSE_ERR 252, NONE 251); the cross-module none member is minted MAX-4, so the countdown reservation is four deep.
- The builtin and std native modules' index spaces have diverged (builtin 0-45: split 17, list natives 20-23, map block 28-31, container closes 32-37, combinators 38-40, nursery control seams 41-44, join 45, with the monomorphic slots reserved and their bodies in the generic table, against std's net 20-23); re-pin the per-module indices when the M7 freeze happens.
- Fallible natives: over the success-typed ambient the two-word platform span re-spans into the call's failure union, a total spelling extracts the payload, and a failure prints `io|net error: <msg>` and halts; CALLT-to-fallible is Unsupported.
- The ambient `enum net_err { timeout, refused }` pre-commits two runtime case tags against the platform's single NET_ERR discriminant, so a future fetch native must answer both or the enum gets re-shaped; https stays rejected in the net natives until the M7 platform decision.
- The unnameable ambient set is `parse_json`/`open`/`read_exact`, whose signatures must name file-owned types, so they wait on M7 package-owned types.

### Reentry seam

- (R12) Generic builtin bodies receive `&mut Vm` and may call user code through `Vm::call_hosted`; hosted calls inc each Ref argument because the callee's placement owns its parameters, and staging them as borrowed double-decs.
- A boundary drive answers `DriveOutcome::Frame` keyed on the sentinel reentry frame (u32::MAX return pc), which stays precise across fiber switches; a yield or HALT escaping a hosted drive is the `ReentrancyRefused` trap.
- (R13) A non-root frame's native tail call lowers in place (CALL plus flush plus return), and only the root frame's native tail may CALLT with abandoned staged counts.
- Native staged-borrow invariant: every native call's emission opens one borrow window in the placement pass before its operand evaluation and pops it at the call's flush; the window pins each word its CALL reads (staged Ref arguments plus the eagerly registered operand words), so a death firing inside it (a later operand's own last-use dec, the shape that freed `b` in `b + "${b}x"` before the CALL read it) defers to that window's flush.
- Nested windows flush LIFO, the staged word's own flush-dec dedupes against its deferred last-use dec, a local callee handed a window word dups it to keep its own claim, and a root tail transfer rides the CALLT with the deferred counts.
- Every `native_done` site pairs with exactly one open window; a mispaired flush pops an enclosing window and decs early, which the `debug_assert` in `native_done` guards.

### Memory and runtime facts

- A child's pool slot can be recycled while the nursery's children list still holds the id, so drivers re-check `fiber.nursery` before driving and outcomes are ledgered before recycling.
- The list copy natives (cons/tail/collect) must touch their element words exactly once: memcpy into the one `Vec` the fresh list adopts, then one branchy walk taking `inc_edge` per `Ref` word. Fusing the check into a loop that builds a second `Vec` adds an allocation plus a full copy pass per call (containers regressed +117% that way before the single-vec shape landed).
- A child yielding inside an infinite loop under a join drive hangs the driver by construction, a substrate fact the language surface must bound.
- CALLT pc-reset is deliberately not a preemption point, so a self-tail-loop never observes cancellation.
- VM honesty residuals: staged Ref counts stay with interrupted fibers' kept frames until the deinit walk, the cross-nursery join interlock recursion is open, the CALL inline cache retains swapped-out versions until eviction (bounded, memory only), and KSTR intern rows for dead pools are never reclaimed.
- Captures copy by value at CLOSURE_NEW, so mutable-capture shapes stay out of the kernels by construction.
- The module-id space stays registry-assigned as an interim until the ds-mod link table assigns indices.
- Pinned honest refusals: var-span widening, a lambda capturing a closure word, a lambda capturing a callable-defaulted binding, callable collection elements and struct fields, and native or generic fn values minting callable words; whether native fn values should ever mint one stays open.
- Tuple and multi-word seams: value-level tuple tests and guarded multi-arm matches over non-union values lower as a linear test chain, struct-typed fields carry whole inline spans since the width flips, multi-word list elements and tuple union members wait on the native-channel slice, and `partition` refuses because its tuple result needs the span channel in the native path.
- Generic-impl lowerer seam: generic trait bodies lower once per impl under runtime evidence, and the missing halves are transitive staging (a nested dispatch whose serving body is itself a generic impl's member refuses) and per-branch evidence (a receiver unioning type variables across match branches has no single serving body).
- Evidence crosses a lambda frame through capture-block words staged from the enclosing frame's evidence slots (nested lambdas transitively); the enclosing body's demand walk must collect every demand lexically inside its lambdas because the construction site looks the words up in the frame's map by pair, and spawned children carry no evidence channel so their bound-demanding sites keep the honest refusal.

### Fuzzing

- cargo-fuzz has no automatic dictionary discovery; pass `-dict=` explicitly.
- A Rust panic aborts before libFuzzer's deadly-signal callback registers, so crash artifacts and tmin miss it; recover the input from the panic message.
- Valid dsmod modules are unguessable by mutation because the content hash gates acceptance; generate seeds deterministically (`fuzz/tools/gen_dsmod_seeds.rs`) before fuzzing the reader.
- `fuzz/corpus` is gitignored yet tracked, so seed additions need `git add -f`.
- The ds-mod reader rejects non-canonical section-directory order (found by the campaign, 70-byte reproducer kept in the corpus).
- The checker generator leg (`fuzz_check_gen` over the shared generator in `fuzz/check_gen.rs`, 240-seed nextest mirror in CI) biases declarations through the extraction family to the well-typed surface with weighted wrong splices; the linearity audit walk is linear in program size by invariant, and a nested loop's duplicate round mutates nothing and must stay skipped.
- Multi-worker sanitizer campaigns fit only on a quiet host: 8-worker ASan legs were OOM-killed by earlyoom under parallel-agent load, so serialize or cap workers when the host is loaded.
- The `fuzz_vm` run limit stays stack words, not steps (the VM has no step budget), so termination rests on the generator's invariants: every backward edge passes the loop template's decrement exactly once, the spawn tree stays depth one, and every switch re-enters a parked fiber at pc+1 so each drive makes progress; any relaxation of the loop template must keep those invariants.
- The emit path's retargeting invariants (forward targets computed against the fixed-part lower bound, stray loop-tail landings retargeted onto the decrement) are the re-audit point if the uop ABI changes.
- A gone joiner's boundary answer surfaces to the drive's host, never switching into a continuation-less fiber, pinned in `crates/ds-vm/tests/nursery_joiner_reentry.rs`.

### Benchmarks

- substring/char_at/len answer from a cached per-string char map (lazy one-time build for non-ASCII, zero storage for ASCII), so string-heavy benchmarking is unblocked.
- String lengths are tracked arithmetically in benchmarks/examples until generic `len[t]` lowers at M6.
- Rust f64 Display prints `250.0` as `250` while Python and Lua print `250.0` and Lua tostring is %.14g, so float goldens print values every printer agrees on by construction (odd fourths).
- `ds run` echoes the unit entry result as a trailing `0` line, absorbed by the `^result:` extraction rule.
- `benchmarks/ab.sh` times whole-process wall clock, so its percentages understate in-VM deltas symmetrically.
