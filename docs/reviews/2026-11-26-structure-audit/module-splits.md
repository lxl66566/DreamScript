# Oversized-module survey: split seams

Read-only survey of `/root/programs/DreamScript`. Nothing in the repo was modified, built, or tested.

Line counts below are **code lines** (a trailing `mod tests` block subtracted) plus the approximate item ranges from `rg`/`awk` item starts, so ranges are accurate to the item boundary and approximate within a few lines.

| file | code lines | tests | verdict |
| --- | --- | --- | --- |
| `crates/ds-vm/src/vm.rs` | 13,079 | 580 | split, staged (biggest win) |
| `crates/ds-check/src/type_of/mod.rs` | 10,630 | 0 | split, staged |
| `crates/ds-lower/src/func.rs` | 9,791 | 0 | split, staged |
| `crates/ds-vm/src/native.rs` | 5,312 | 1,199 | split (clean, family-per-file) |
| `crates/ds-lower/src/func/calls.rs` | 5,535 | 0 | split into 4 |
| `crates/ds-meta/src/derive.rs` | 4,900 | 48 | split (cleanest seam in the tree) |
| `crates/ds-check/src/resolve.rs` | 3,783 | 0 | split into 3 |
| `crates/ds-check/src/decls.rs` | 3,045 | 0 | split into 5 |
| `crates/ds-vm/src/object.rs` | 3,224 | 1,568 | split into 5-6 |
| `crates/ds-dsi/src/lib.rs` | 2,507 | 169 | split into 4 (encode/decode/serve) |
| `crates/ds-vm/src/net.rs` | 2,245 | 444 | split into 4, `cfg`-sensitive |
| `crates/ds-check/src/linearity.rs` | 2,066 | 69 | split into 3 |
| `crates/ds-check/src/import.rs` | 2,054 | 1,749 | split into 3 |
| `crates/ds-lower/src/func/pattern.rs` | 2,022 | 0 | low priority, seam exists |
| `crates/ds-lsp/src/lib.rs` | 1,846 | 2,122 | low priority, seam exists |
| `crates/ds-lower/src/native.rs` | 1,824 | 0 | skip |
| `crates/ds-lower/src/shape.rs` | 1,724 | 566 | skip |
| `crates/ds-cli/src/repl.rs` | 1,728 | 0 | skip |
| `crates/ds-parse/src/convert/mod.rs` | 1,695 | 0 | skip |
| `crates/ds-cli/src/package.rs` | 1,437 | 182 | skip |
| `crates/ds-lower/src/place.rs` | 1,344 | 683 | skip |
| `crates/ds-cli/src/source.rs` | 1,279 | 493 | skip |

## Method

For each file: `rg -n '^(pub )?(fn|struct|enum|impl|mod|const|static|trait|type)'` plus a method-name scan restricted to the big `impl` blocks, then a read of only the boundary regions and the doc comments. A seam was accepted only when three things held: distinct concerns, distinct mutable state, and few cross-calls between the two sides. Proposals below are **mechanical moves**: items keep their bodies, the parent keeps the `struct`/type definitions it owns, and the parent re-exports what outside code names (so `crate::vm::Trap`, `crate::type_of::Val`, `ds_vm::native::NativeResult` and friends keep resolving).

## Cross-cutting notes (apply to every proposal)

1. **No path churn.** Every crate already declares `pub mod vm;`, `pub mod object;`, `pub mod native;`, `pub mod decls;` etc. Rust 2018+ allows `src/vm.rs` **and** `src/vm/*.rs` simultaneously, so each split adds `mod foo;` + `pub use foo::*;` lines to the existing file. `vm.rs` never has to become `vm/mod.rs`; `type_of/mod.rs` is already a directory. Zero call sites change.
2. **Visibility is the real cost.** Privacy is module-tree based, so a moved item's visibility shrinks to the new submodule unless bumped. Counts today: `impl Vm` has 115 private `fn`, `impl Bodies` 187 private, `impl FnLower` 170 private. The uniform mechanical rule for every moved item is `pub(super)` (= visible in the parent module and all its descendants); free helpers used by one new file stay private. Expect a large but purely syntactic diff line (`fn` → `pub(super) fn`). Sibling modules never call each other directly: the call graph is `execute/scheduler → helpers`, so most helpers only need to be visible from the parent.
3. **Inlining / hot paths.** `[profile.release]` sets `lto = "thin"` and `codegen-units = 1`, and the crate already inlines across `ds-vm` → `ds-abi`/`ds-rt`. Intra-crate module splits therefore cost nothing in release. Debug/test builds use the default `codegen-units = 256`, where splitting can change inlining, so benchmark deltas must be measured in release only. No `#[inline]` annotations exist in `vm.rs` (7) / `object.rs` (1) that would move across a boundary and lose force.
4. **Tests.** Files with a trailing `mod tests` (`vm.rs`, `native.rs`, `object.rs`, `import.rs`, `net.rs`, `linearity.rs`, `shape.rs`, `place.rs`, `dsi/lib.rs`, `lsp/lib.rs`) use `use super::*` and reach private items. Keep the test module in the parent (it can now see the submodules' `pub(super)` items) rather than moving it; moving it would force `pub(crate)` on internals.
5. **Doc comments are design records here.** Each file opens with a long `//!` block that names the invariants (call ABI, decode-on-dispatch, namespace split, ...). The parent keeps the overall block, and each new submodule takes the paragraph that describes it. Do not delete prose that is the only statement of a locked invariant; do not copy it either.
6. **`cfg` items.** `net.rs` has 62 `#[cfg(target_os = "linux")]` / `unix` items densely interleaved with the socket state machine; `vm.rs` has 8 (`wait_world`'s clock backend). Moves there must keep attributes attached to the same items — no `cfg` item may be separated from its function.

## 1. `crates/ds-vm/src/vm.rs` — 13,079 lines (top priority)

### (a) Current concerns

| range | concern |
| --- | --- |
| 1-60 | module doc: dispatch strategy, fiber/nursery scheduling, reentrant host calls |
| 62-110 | imports, `FUEL_QUANTA`, `DEFAULT_STACK_LIMIT`, `F64_I64_EDGE` |
| 111-482 | `pub enum Trap` — 372-line trap taxonomy |
| 483-560 | `CallInfo`, `Active`, `Transfer` (the running-frame handle) |
| 496-620 | call-site facts: `RawStagedWord`, `SiteFacts`, `raw_staged_words` |
| 589-720 | call-target caches: `Resolved`, `CachedCallee`, `DispatchResolved`, `CallSite`, `CALL_CACHE_SLOTS`, `WordCallee`, `WORD_CACHE_SLOTS` |
| 718-852 | kstr (interned-string) row index: `KstrRowKey`, `WordKeyHasher`, `KstrRowIndex`, `HookDirIndex`, `KstrRow`, `KstrRows` |
| 853-995 | `CaptureStaging`, `RetOutcome`, `FrameAnswer` (+ inline helpers) |
| 999-1352 | **drive protocol**: `DriveOutcome`, `InstalledReactor`, `ParkKind`, `WorldParkRequest`, `InterlockRequest`, `NurseryDrive`, `Delivery`, `NurseryDriveEnd`, `ForkEnd`, `StepFlow`, `Woken`, `ExecutorStep`, `IoDrive`, `EagerPass`, `DriveSeam`, `ChanStep`, `TimerOutcome`, `ChanOutcome`, `SwitchOutcome`, `timeout_failure_span`, `ok_tagged`, `native_head_word`, `frame_answer_of` |
| 1353-1710 | `StackState` + impl — frame geometry over the segment |
| 1711-2460 | slot/word access vocabulary: `code_len_u32`, `word_of`, `RetHit`/`CallHit`/`call_hit`/`ret_hit`, `read_*`, `slot_word`, `write_slot`, `str_content`, `str_eq_content`, `concat_text`, `int_div`, `int_mod`, `shift_bitwise_word`, `fused_*` |
| 2453-2932 | native-call ABI: `stage_closure`, `invoke_native`, `generic_staged_check`, `generic_kind_check`, `overlay_native`, `control_i64`, `control_word`, `overlay_word`, `overlay_tag`, `check_native_*`, `check_bound_*`, `invoke_bound` |
| 2932-3044 | debug types: `FramePc`, `BacktraceFrame`, `SourceLocation`, `ErrorTraceFrame`, `BreakpointHit`, `Breakpoint`, `ProfileRow`, `DebugStop` |
| 3045-3230 | `struct Vm` fields, `CallerAt`, `StagedSwap` |
| 3231-3294 | `OP_*` opcode byte table (63 words) |
| 3295-13079 | **one** `impl Vm` — 9,785 lines, 200 methods, including `fn execute` at 9534-12385 (2,851 lines, 63 opcode arms, single `'transfer: loop`) |

Inside the impl, in line order: construction/config 3298-3560 · module install + hot-reload validation + sweeps 3573-4060 · fiber spawn 4107-4380 · suspend/resume/io pump/release 4369-4800 · nursery registry + join/teardown 4831-5005 · nursery drive machinery 5005-6030 · executor step / entry checks / host release 6033-6230 · park 6233-6360 · reactor waits 6363-7240 · cut/continue/switch/die 7240-7900 · entry points + debugger 7918-8210 · host calls + heap accessors 8353-8600 · claims + teardown hooks + split 8652-9060 · call-site resolution 9067-9534 · `execute` 9534-12385 · `backtrace` 12385-12445 · chan ops 12447-12767 · race/timeout ops 12804-13080.

### (b) Proposed split — `crates/ds-vm/src/vm/`

`vm.rs` keeps: module doc, imports, consts, `Active`/`Transfer`/`CallInfo`, `struct Vm` + `CallerAt`/`StagedSwap`, the `OP_*` table, `new`/`with_limit`, `mod` decls, `pub use` re-exports, `mod tests`.

| new file | items that move | ~lines | notes |
| --- | --- | --- | --- |
| `vm/trap.rs` | `enum Trap` (111-482) | 372 | parent: `pub use trap::Trap;` — `crate::vm::Trap` and `ds_vm::Trap` unchanged |
| `vm/proto.rs` | the whole 999-1352 drive-protocol band | 354 | types are shared by dispatch + fiber/nursery/wait/chan/race; one home beats six `pub(super)` re-exports |
| `vm/stack.rs` | `StackState` + impl, `word_of`, `code_len_u32` (1353-1710, 1711-1726) | 360 | used by `execute`, `call_hit`/`ret_hit` and `resolve_word` |
| `vm/value.rs` | `read_*`, `slot_word`, `write_slot`, `str_content`, `str_eq_content`, `concat_text`, `int_div`, `int_mod`, `shift_bitwise_word`, `fused_*`, `RetHit`/`CallHit`/`call_hit`/`ret_hit` (1727-2460) | 735 | "value read/write over the slot overlay"; `execute` reaches them through `use super::*` |
| `vm/call.rs` | `SiteFacts`, `RawStagedWord`, `raw_staged_words`, `stage_closure`, `invoke_native`, `generic_*_check`, `overlay_*`, `control_i64/word`, `check_native_*`, `check_bound_*`, `invoke_bound` (496-620, 2453-2932) | 620 | the locked call-ABI move; `CaptureStaging` stays with `FrameAnswer` |
| `vm/site.rs` | call-target caches (589-720), `KstrRow*`/`WordKeyHasher`/`HookDirIndex` (718-852), `uop_site_index`, `resolve_site`, `resolve_site_full`, `resolve_dispatch_site`, `kstr_load` (9067-9534) | ~1,000 | the caches and the resolvers that own them are one concern today split across the file |
| `vm/install.rs` | `install_module`, `install_dsmod`, `load`, `installed_content_hash`, `validate_staged_swap`, `validate_pool_call_refs`, `finish_swap`, `walk_staged_fns`, `retiring_pool_addrs`, `reclaim_*`, `sweep_call_cache`, `sweep_versions`, `kstr_cache_rows`, `arena_stats`, `registry`, `install_native_module`, `install_bound` (3573-4060) | 490 | hot-reload/version-table concern; uses `StagedSwap` from the parent |
| `vm/fiber.rs` | spawn cluster (4107-4380), suspend/resume/`io_pump`/`release_*`/`fiber_*` accessors (4369-4800), `switch_in`, `die_current`, `complete_current*`, `cancel_current`, `control_switch`, `overlay_boundary`, `overlay_span_tail`, `arena_frame_close` (7472-7900) | ~1,150 | "fiber switch mechanics" per the module doc |
| `vm/nursery.rs` | open/join/teardown/`face_drive`/`restore_context`/`push_drive_frame`/`drive_step`/`try_fork_pool`/`payload_words`/`child_death_walk`/`drive_frame_*`/`is_racer`/`ready_racer`/`consume_*`/`nursery_drive_run`/`fork_heaps`/`fork_frame_end`/`drive_interlock` (4831-6030) + eager/drive/fork_park (7318-7470) + `joiner_parked`/`reenter_joiner` (7884-7918) | ~1,900 | largest coherent cluster in the file |
| `vm/wait.rs` | `register_wait`, `sched_has_work`, `cancel_wait`, `release_wait_resources`, `finish_wait*`, `wake_closed`, `rearm_step`, `wait_error_span`, `complete_wait`, `wait_world`, `shutdown_raised`, `cut_candidate`, `stage_cut`, `enter_woken`, `executor_continue` (6363-7240) | ~880 | reactor-facing; owns the 8 `cfg(target_os)` items — move attributes with their items |
| `vm/host.rs` | `call_hosted`, `call_hosted_in`, `resolve_word`, `heaps`, `heaps_walk`, `root_store`, `inc_ref`/`dec_ref`/`dec_edge`/`inc_ref_split`/`inc_edge`, `closure_parts`, `split` (8353-8600, 9030-9060) | ~330 | the Rust-side reentry seam described by the module doc |
| `vm/teardown.rs` | `drain_claims*`, `fire_teardown_hooks*`, `collect_teardown_drives`, `gate_teardown_site`, `hook_sites_for`, `host_delivery`, `trap_delivery` (8652-9030) | 380 | claims-driven drop hooks; `crate::claims` stays the table owner |
| `vm/debug.rs` | debug types (2932-3044), `run`, `run_debugged`, `set_trace_errors`, `error_trace`, `arm_breakpoints`, `parked_at_breakpoint`, `breakpoint_hit`, `arm_sampling`, `sampling_report`, `run_with_budget`, `run_budgeted`, `root_drive`, `reentry_live`, `caller_arg_area`, `backtrace` (7918-8210, 12385-12445) | ~850 | driver + introspection are one API surface |
| `vm/chan.rs` | `chan_step`, `chan_deadline_answer`, `chan_switch`, `chan_elem_answer`, `chan_live_receiver`, `chan_take_live_sender`, `chan_release`, `chan_refill`, `chan_deadline_parked`, `wake_chan_waiter` (12447-12767) | 331 | uses `ChanStep`/`ChanOutcome` from `proto.rs` |
| `vm/race.rs` | `timeout_switch`, `race_of`, `race_wait_of`, `start_racer`, `expire_race`, `cut_race_racer`, `resolve_race`, `close_race`, `deliver_race`, `end_race_of` (12804-13080) | 284 | uses `TimerOutcome`/`SwitchOutcome` from `proto.rs` |
| `vm/execute.rs` + `vm.rs` | `execute<const FUELED>` (9534-12385) | 2,851 | **not mechanically splittable** — see risk note |

Staging so no single review is 10k lines: **A** `trap`, `proto`, `stack`, `value`, `call`, `chan`, `race`, `debug`, `install` (≈5,900 lines leave `vm.rs`); **B** `site`, `fiber`, `wait`, `host`, `teardown` (≈3,300 more); **C** `nursery`. After A+B, `vm.rs` is `struct Vm` + config + `execute` + nursery.

### (c) Risks

- **`execute` is one function.** 2,851 lines with a single `'transfer: loop` and 63 opcode arms sharing mutable frame-context locals (`f`, `code`, `slots`, `fp`, `pool`). Moving it to `execute.rs` is mechanical; *shrinking* it is not — extracting the chan/race/fork arm bodies into `&mut self` helpers changes borrow shapes (the loop borrows `self.registry` and `self.callinfo` across arms) and must be reviewed as behavior work, not a split. Recommend: accept `execute.rs` at ~2.9k now, do the arm extraction as its own change with the VM bench suite.
- **`OP_*` const table placement.** `vm.rs`'s `mod tests` asserts `OP_KIMM`/`OP_ADD_I64`/... equal `ds_abi` opcodes via `use super::*`. Keep the table in `vm.rs` (it is an encoding fact, not dispatch state) and let `execute.rs` `use super::*`.
- **Private-method reach.** The dispatch loop calls roughly 80 private `Vm` methods; after the move each needs `pub(super)`. The compiler will not flag a missing bump as an API leak, only as a compile error at the call site, so a wrong split shows up immediately.
- **`Active`, `CallInfo`, `Vm` field privacy.** `Active` is passed by value through almost every moved function; keep it in the parent. `Vm`'s fields stay private — child modules may read them, so no field churn is needed.
- **`crate::vm::ParkKind`, `crate::vm::FrameAnswer`, `crate::vm::DriveSeam`** are named from `repl.rs` and `native.rs`; `proto.rs` must be re-exported from `vm.rs` with `pub(crate) use` (or `pub use` for the ones in the current `Pub` surface).

## 2. `crates/ds-check/src/type_of/mod.rs` — 10,630 lines

### (a) Current concerns

1-60 module doc (engine shape, flow refinement, E4 union errors). 62-76 imports. 78-486 the value/engine types: `Val`, `Binding`, `IterWindow`/`IterBind`, `NurseryScope`, `ScopeMark`, `Narrow`, `HoleShow`, `InherentCallBounds`, `HoleShowDispatches`, `GenericCallBounds`, `PlaceStep`, `StoreIndex`, `Projection`, `CopyRecords`, `ComptimeLoop`, `InstanceScope`, `RecordScope`, `InstanceRun`, `PlaceStore`, `Checked` (333-465), `TraitCall` (465-486). 486-490 existing submodules: `impl_sig`, `ops`, `pattern`, `scope`. 497-1034 entry: `type_check`, `analyze`, `analyze_owned`, `analyze_resolved`, `analyze_served*`, `analyze_served_over`, `check_bodies`, `MAX_INSTANCE_EXPANSIONS`. 1038-1106 small predicates + `fn_lambda`. 1106-1289 `expand_comptime_instances`. 1289-1305 `ArtifactSymbols`, `span_files_of`. 1305-1405 `Analyzed` + impl, `FilePositions`. 1397-1549 `DirectedLabel`, `struct Bodies` fields. 1555-10402 **one** `impl Bodies<'_>` (8,848 lines, 187 methods). 10407-10630 free helpers.

Inside `impl Bodies`: item/file collection 1571-1997 · blocks/stmts/locals 1997-2108 · `record_*` fact emitters 2108-2382 (27 near-identical writers) · `expr`/`expr_inner` walk 2382-2701 · control forms `while`/`tuple` 2701-2781 and `if`/`match`/`for`/`for_comptime` 8121-8589 · literal/string/hole 2781-3074 · name & def resolution 3074-3620 · lambda 3619-3851 · constructors 3851-4509 · call checking + method selection 4509-7277 (2,770 lines) · field access / assignment / places 7277-8121 · comptime loop projection 8589-8900 · refinement `harvest`/`apply_narrow` 8918-9000 · diagnostics & containment 9009-9370 · type solving 9366-9788 · trait satisfaction & instances 9788-9994 · nursery/spawn blocks 10010-10081 · scope frames 10081-10140 · projection/query helpers 10140-10395.

### (b) Proposed split — `crates/ds-check/src/type_of/`

`mod.rs` keeps: doc, imports, all engine types (78-486), `mod` decls + `pub use`, entry fns 497-1034, `Analyzed`, `struct Bodies`, `mod tests` (none today).

| new file | items | ~lines |
| --- | --- | --- |
| `type_of/item.rs` | `bind_local`, `file`, `item`, `struct_item`, `impl_item`, `trait_arg_subst`, `top_level_local`, `fn_decl`, `block*`, `stmt`, `local_stmt` (1571-2108) | 540 |
| `type_of/record.rs` | the 27 `record_*` emitters, `record_scope`, `record_place_store` (2108-2382) | 275 |
| `type_of/expr.rs` | `expr`, `invalid_expr`, `expr_inner`, `while_expr`, `tuple_expr`, `literal`, `named_ty`, `string_expr`, `hole_show_dispatch`, `printable_hole`, `ident_hole_value` (2382-3074) | 690 |
| `type_of/name.rs` | `path_expr`, `probe_label`, `head_type_of`, `members_labeled`, `qualified_label_val`, `directed_atom_val`, `def_value`, `const_ty` (3074-3619) | 545 |
| `type_of/lambda.rs` | `lambda` (3619-3851) | 232 |
| `type_of/ctor.rs` | `struct_expr`, `list_expr`, `ctor_apply_call`, `map_ctor_call`, `ctor_type_arg`, `applied_*` (3851-4509) | 660 |
| `type_of/call/mod.rs` | `call`, `case_call`, `indirect_call`, `call_def`, `settle_routed_return`, `has_hole`, `atom_has_hole`, `arg_slots`, `check_slots`, `iter_window`, `spawn_cancel_refines`, `check_mut_window_args` (4509-5794) | 1,285 |
| `type_of/call/select.rs` | `qualified_call`, `dot_call`, `type_keyed_receiver`, `applied_receiver`, `refuse_static_member_call`, `static_member_call`, `method_select*`, `free_fn_takes_receiver`, `inherent_accepts`, `record_solved_dispatch`, `self_binding`, `member_call`, `err_ok`, `handler_body`, `extraction_*`, `selection_without_dispatch`, `unknown_selection`, `named_selection_note`, `recorded_type_def` (4721-4785, 5794-7277) | 1,480 |
| `type_of/place.rs` | `field_access`, `no_field`, `try_expr`, `assign*`, `field_step_ty`, `index_*`, `gate_mut_window_root`, `place_chain`/`PlaceShape`/`ChainStep` (7529-8121, 10417-10489) | 690 |
| `type_of/flow.rs` | `if_expr`, `match_expr`, `for_expr`, `for_comptime`, `instance_fn_body`, loop projection (`project_loop_*`, `comptime_*`, `harvest*`, `apply_narrow`, `apply_inverse`, `is_expr`, `is_test_*`) (8121-8900, 8918-9000, 9206-9370) | 1,240 |
| `type_of/solve.rs` | `solve_here`/`solve_qualified`/`solve_with`/`match_param`, `trait_of_type`, `bound_traits`, `inherent_evidence`, `trait_satisfied`, `report_bound_miss`, `assumption_covers`, `instance_*`, `push/pop_generics`, `mint_var`, `fresh_var` (9167-9188, 9366-9994) | 650 |
| `type_of/expect.rs` | `error`, `expect_contains`, `expect_param_contains`, `has_free_vars`, `atom_has_free_vars`, `expect_bool`, `check_args`, `names_no_variable`, `atom_is_concrete` (9009-9127, 1038-1065) | 200 |
| `type_of/query.rs` | `nursery_block`, `spawned_block`, `enter_frame`, `exit_frame`, `snapshot`, `restore`, `local_ty`, `is_type_atom`, `names_declared_type`, `path_local`, `cover_ty`, `missing_ty`, `render_members`, `struct_shape_of`, `payload_tys`, `atom_name`, `single_apply*`, `applied_enum_head`, `decompose_fn*`, `fn_typed`, `expected_tuple_elems`, `tuple_elements`, `named_atom_ty`, `is_string_ty`, `none_ty`, `bool_ty`, `unit`, `empty`, `empty_val`, `fn_atom_of`, `span_text`, `member_sig`, `type_def_of`, `take_*_reported` (10010-10395, 8993-9010, 9177-9188) | 460 |
| `type_of/util.rs` | the tail free helpers 10407-10630 minus the ones claimed by `place.rs` and `expect.rs` | ~150 |

Existing `impl_sig.rs`, `ops.rs`, `pattern.rs`, `scope.rs` stay untouched; `type_of/pattern.rs` (727 lines, arm patterns) is the pattern for how new files start (`use super::*;` + `impl Bodies<'_>`).

### (c) Risks

- All 187 methods are private → `pub(super)` on those the parent or siblings still call (`expr`, `value`-adjacent, `error`, `expect_*`, `solve_*` are the hub methods). Keep `expr`/`expr_inner`'s callee set in mind: it is the single dispatcher, so it needs the widest bump.
- `CopyRecords` and the `record_*` writers are one state pair (`self.records`); moving the writers together is safe, splitting a writer from its field is not — do not move `record_*` individually.
- `checked`/`resolutions`/`scope` fields on `Bodies` are written by the entry fns in `mod.rs`; leave the construction path there.
- Compile-time fanout: `ds-check` is the slowest crate to compile; each new file adds a unit. Expected net-neutral (same total code), but the `-j 4` shared-lock rule from AGENTS.md applies to any `nextest`/`check` run after this split.

## 3. `crates/ds-lower/src/func.rs` — 9,791 lines

### (a) Current concerns

1-45 doc + imports. 46-51 existing submodules (`calls`, `extraction`, `instance`, `pattern`, `sort`, `store`, `tail`). 53-420 lowering vocabulary: `Slot` (+impl), `DataOp` (+impl), `Op` (187-324, the emitted instruction set), `PlaceShape`, `Place`, `LoopCtx`, `LoopSpan`, `TypedPlace`, `ValueArmKind`, `WordAddr`, `field_addrs`, `DeinitClaim`. 422-740 plan types: `PlannedFn`, `DeclFn`, `RuntimePair`, `EvidenceEnv`, `BodyId`, `Demands`, `EvidenceSrc`, `TraitDispatch`, `Plan`, `Minted`, `MintSite`. 747-1242 `impl Plan`. 1243-1664 capture/cancel discovery free fns (`collect_captures`, `stray_exit*`, `cancel_operands*`, `capture_use`, `visit_uses`). 1664-2055 discovery (`discover_item`, `Discovery`, `entry_body`, `mint_evidence`, `spawn_return`, `captures_under`, `inlines_lambda_arg`, `extraction_lambda_taker`, `discover_expr/block/stmt`). 2055-2513 `runtime_pairs`, `carries_a_var`, `DEAD_CHAIN`, `peeled_param`, `PairWalk`. 2513-2720 `RunContext`, `ComptimeRun`, `Callee`, `struct FnLower`. 2720-9482 **one** `impl FnLower` (6,762 lines, 171 methods). 9482-9791 lowering free helpers (`resolve`, `eq_op`, refusals, `arith_shape`, `single`, `ordered_field_srcs`, `int_radix`, `parse_int`, `ScalarConst`, `ScalarDst`, ...).

Inside `impl FnLower`: `lower` 2724-2940 · check-side fact readers `copy_*`/`val`/`written_*`/`binder_fact`/`trait_call`/`is_test`/`generic_call`/`member_call_params`/`handler_handled`/`place_store`/`def_type`/`members` 2940-3256 · emit primitives (`alloc`, `temp_word`, `emit`, `kstr`, `label`, `bind_label`, `splice`, `loop_back_edge`, `arm_epilogue`, `overlay_site`, `pool`, `pool_str`) 3256-3427 · ownership words (`ref_words`, `tracked_words`, `count_words`, `claim_band_word`, `discard_words`, `rc_bind*`, `deinit_claim`, `deinit_hook`) 3427-3617 · param/capture binding + fn scope (`bind_params`, `param_is_var_ty`, `bind_evidence`, `bind_capture_evidence`, `bind_deinit_param`, `bind_captures`, `scope_*`, `only_atom`) 3617-3856 · address computation (`word_addrs`, `store_field_span`, `tag_addr`, `is_span_ty`, `struct_typed`) 3856-4034 · blocks/stmts/locals (`block_value`, `stage_tail_words`, `block_inner`, `unit_value`, `canonical_unit`, `is_unit_ty`, `join_span`, `stmt`, `note_value_end`, `guard_deinit_union_member`, `local_stmt`, `scalar_var_init`, `scalar_const_into`, `var_tuple_slots`) 4034-4455 · control & loops (`assign_value`, `compound_value`, `while_value`, `for_comptime_value`, `comptime_*`, `for_value`, `for_range_value`, `for_list_value`, `break_value`, `continue_value`, `loop_exit_drops`) 4455-5291 · element fetch/gather 5291-5476 · the expression walk (`value`, `value_node`, `literal`) 5476-5795 · closures/lambdas/spawn (`lambda_value`, `child_callable`, `seam_call`, `nursery_value`, `boundary_*`, `spawn_value`, closure guards/refusals) 5795-6660 · strings & holes 6655-6885 · paths/type atoms/`is_value` 6885-7228 · data construction (`field_access`, `struct_literal`, `field_members`, `store_union_member_field`, `tuple_literal`) 7228-7545 · operators (`binary_value`, `arith*`, float refusals, `compare`, `emit_data*`, `bool_chain`, `in_range_value`, `in_list_value`, `eq_chain`, `scalar/collection/content` eq words, `payload_word_plan`, `emit_word_compares`, `union_eq`, `unary`) 7545-8973 · `finalize`, `native_key`, `funcref_const`, `zero_word`, `zero_init_member` 8973-9482.

### (b) Proposed split — `crates/ds-lower/src/func/`

`func.rs` keeps: doc, imports, `mod` decls, `Op`/`DataOp`/`Slot` instruction vocabulary, placement types (`Place`, `PlaceShape`, `TypedPlace`, `WordAddr`, `LoopCtx`, `DeinitClaim`), the plan types + `impl Plan`, `FnLower`/`Callee`/`RunContext`/`ComptimeRun`, discovery fns, free helpers.

| new file | items | ~lines |
| --- | --- | --- |
| `func/plan.rs` | plan types 422-740, `impl Plan` 747-1242, discovery 1243-2513 (`collect_captures`, cancel-operand walk, `visit_uses`, `discover_*`, `runtime_pairs`, `PairWalk`) | 2,100 |
| `func/facts.rs` | the `copy_*` readers and `val`/`written_val`/`binder_fact`/`trait_call`/`is_test`/`generic_call`/`member_call_params`/`handler_handled`/`place_store`/`def_type`/`members` (2940-3256) | 320 |
| `func/emit.rs` | `alloc`, `temp_*`, `place_of_ty`, `emit`, `kstr`, `label`, `bind_label`, `splice`, `loop_back_edge`, `arm_epilogue`, `overlay_site`, `pool`, `pool_str`, `pool_len`, `resolve`, `eq_op`, `mismatch`/`no_span`/`pattern_span` (3256-3427, 9482-9530) | 250 |
| `func/ownership.rs` | `ref_words`, `tracked_words`, `count_words`, `claim_band_word`, `discard_words`, `rc_bind*`, `deinit_claim`, `deinit_hook` (3427-3617) | 190 |
| `func/params.rs` | `bind_params`, `param_is_var_ty`, `bind_evidence`, `bind_capture_evidence`, `bind_deinit_param`, `bind_captures`, `scope_*`, `only_atom` (3617-3856) | 240 |
| `func/place.rs` | `word_addrs`, `store_field_span`, `tag_addr`, `is_span_ty`, `struct_typed`, `single`, `ordered_field_srcs` (3856-4034, 9609-9663) | 260 |
| `func/block.rs` | block/stmt/local lowering incl. scalar init and tuple slots (4034-4455) | 420 |
| `func/loop.rs` | control forms and loop exits (4455-5291) + `element_words`/`fetch_element`/`gather_element`/`list_len_word` (5291-5476) | 1,020 |
| `func/value.rs` | `value`, `value_node`, `literal`, `ScalarConst`/`scalar_const`/`ScalarDst` (5476-5795, 9732-9791) | 380 |
| `func/callable.rs` | closures, seams, spawn, nursery, boundary answers, closure guards/refusals (5795-6660) | 865 |
| `func/text.rs` | `string_expr`, `hole_word`, `hole_show_dispatch`, `show_dispatch_word`, `str_native_word` (6655-6885) | 230 |
| `func/path.rs` | `path_value`, `is_value`, `solved_is_members`, `type_atom`, `path_atom` (6885-7230) | 345 |
| `func/data.rs` | field access, struct/tuple literals, union field store (7228-7545) | 320 |
| `func/op.rs` | binary/arith/compare/bool-chain/in-range/in-list/equality word plans/unary (7545-8973) | 1,430 |
| `func/finalize.rs` | `finalize`, `native_key`, `funcref_const`, `zero_word`, `zero_init_member` (8973-9482) | 510 |

### (c) Risks

- Same `pub(super)` churn (170 private methods). `func.rs` already proves the pattern: `calls.rs`/`pattern.rs`/`tail.rs` use `use super::*;` and mix private helpers with `pub(super)` entry points.
- `Op`, `Slot`, `DataOp`, `Place`, `TypedPlace` are used by every new file; keeping them in `func.rs` avoids a web of imports (`use super::*` covers it).
- `finalize.rs` moves the only place that mutates the emitted plan's pools into a final `FinalizedFn`; that function is the owner of cross-cutting invariants (label patching, pool indices). Review it separately; do not fold other clusters into it.
- `arity`/label patching is global mutable state (`self.labels`, `self.code`, `self.pool`), which is why `emit.rs` must stay the only writer — a split that lets two files call `self.emit` differently is fine, but the split must not duplicate label allocation.

## 4. `crates/ds-vm/src/native.rs` — 5,312 code lines (+1,199 test lines)

### (a) Current concerns

1-390 doc + imports. 393-980 the `BUILTIN_*` index constants and `PLATFORM_*` tag constants — the whole ambient surface as data. 920-980 `is_io_class`/`is_net_class`. 980-1200 `ControlNative`, `ChannelNative`, `TimerNative`, `NativeResult` (+impl), `NativeFn`, `NativeFunction`. 1203-2550 free native fns grouped by value family: text (`substring_fn` … `replace_fn`), bytes (`bytes_*`, `utf8_*`), scalar conversions (`byte_of`/`int_of_u32`/...), math (`float_unary!`, `atan2_fn`, `pow_fn`), compare/hash (`total_compare_fn`, `fnv1a`, `str_hash_fn`, ...), list/map constructors and ops, `cons_fn`/`tail_fn`/`collect_fn`/`first_element_fn`/`max_fn`, `fiber_control_bounce_fn`. 2559-2810 `GenericFunction`, `GenericFn`, `GENERIC_FUNCTIONS` table, `resolve_generic`. 2814-3300 generic fns (`len`, `list_*`, `map_*`, `cons`, `tail`, `collect`, `max`, `abs`, `map`/`filter`/`reduce`, `compare_orders_first`, `sort`). 3296-3550 fork-join combinators: `ForkCallable`, `ForkStage` (+impl), `is_fork_join`, `fork_callable`, `fork_spawn`, `par_map_generic_fn`, `par_reduce_generic_fn`, `fork_abandon`, `release_owned_span`. 3546-3643 `reserved!` macro + `reserved_entry`. 3643-4537 `builtin_module()` — an 895-line registration function. 4537-4606 `chan_refuse_fn`, arena fns. 4606-4870 `OutputSink` (+impl) and its targets. 4868-5005 arg extractors (`ref_arg`/`i64_arg`/`f64_arg`), `println_fn`/`print_fn`/`eprintln_fn`/`eprint_fn`/`read_line_fn`/`trap_fn`. 5005-5313 scalar→string conversions, `io_class`, `io_err`, `read_file_fn`/`write_file_fn`/`file_exists_fn`/`fs_list_fn`, `time_fn`, `mono_fn`, `sleep_fn`, `file_size_fn`.

### (b) Proposed split — `crates/ds-vm/src/native/`

| new file | items | ~lines |
| --- | --- | --- |
| `native/mod.rs` (keeps) | `BUILTIN_*`/`PLATFORM_*` constants, `ControlNative`/`ChannelNative`/`TimerNative`, `NativeResult`/`NativeFn`/`NativeFunction`, `mod` decls + `pub use`, `mod tests` | ~1,000 |
| `native/text.rs` | substring…replace, `split_fn`, `char_at_fn`, `join_fn`, `contains_fn`, `index_of_fn`, compare/hash fns, `bytes_*`, `utf8_*` | ~900 |
| `native/scalar.rs` | `byte_of`/`int_of_byte`/`u32_of`/`int_of_u32`, `float_unary!`, `atan2_fn`, `pow_fn`, `total_compare_fn`, `float_hash_fn`, `char_hash_fn`/`char_compare_fn`, string conversions `str_int_fn`…`str_fixed_fn` | ~450 |
| `native/list.rs` | `list_*_fn`, `copy_into_fresh_list`, `cons_fn`/`tail_fn`/`collect_fn`/`first_element_fn`/`max_fn`, `arena_*_fn` | ~700 |
| `native/map.rs` | `map_*_fn`, `map_eq_fn`, `list_eq_fn`, `native_result_word`/`native_result_span` | ~350 |
| `native/generic.rs` | `GenericFunction`, `GENERIC_FUNCTIONS`, `resolve_generic`, `fn_arg`, `predicate_bool`, all `*_generic_fn` | ~700 |
| `native/fork.rs` | `ForkCallable`, `ForkStage`, `is_fork_join`, `fork_callable`, `fork_spawn`, `par_map`/`par_reduce`, `fork_abandon`, `release_owned_span`, `fiber_control_bounce_fn`, `chan_refuse_fn` | ~400 |
| `native/registry.rs` | `reserved!`, `reserved_entry`, `builtin_module()` | ~950 |
| `native/sink.rs` | `OutputSink`, `SinkTarget`, `ErrTarget`, `ScalarText`, `println_fn`/`print_fn`/`eprintln_fn`/`eprint_fn`/`read_line_fn`/`trap_fn`, arg extractors | ~450 |
| `native/io.rs` | `io_class`, `io_err`, `read_file_fn`/`write_file_fn`/`file_exists_fn`/`fs_list_fn`, `time_fn`, `mono_fn`/`MONO_ORIGIN`, `sleep_fn`, `file_size_fn` | ~250 |

`builtin_module()` is the mechanical risk: it names every function in every other submodule, so it moves last (or first with `pub(super)` stubs). The 33 external users of `ds_vm::native::…` (ds-cli, ds-ffi, ds-stdlib, ds-rt) go through `ds-vm/src/lib.rs`'s `pub use native::{…}`, which is unaffected.

### (c) Risks

- `GENERIC_FUNCTIONS` is a `&'static` table of fn pointers across modules — legal, but it fixes the visibility of every referenced generic fn at `pub(super)`.
- `NativeFunction`/`NativeResult` are part of the crate's public API (`ds_vm::native::{…}`); they must remain in `native/mod.rs` (or be `pub use`d there) so the re-export list in `ds-vm/src/lib.rs:153` keeps working.
- `mod tests` (1,199 lines) reaches `builtin_module()` and individual `*_fn`s: keeping it in `native/mod.rs` needs `pub(super)` on everything it touches — or move it to `native/tests.rs` with `use super::*;` (a plain file, tests stay `#[cfg(test)]`).

## 5. `crates/ds-lower/src/func/calls.rs` — 5,535 lines

### (a) Current concerns

1-13 doc. 15-50 `EvidenceSite`, `NativeMember`, `SuccessShape`. 51-5432 **one** `impl FnLower`; 5432-5535 `embedded_const_expr`. Clusters: argument staging guards + direct literal/compute 77-462 · constructors `ctor_call`/`map_ctor_call` 462-605 · embedded comptime values 606-842 · `case_value` 842-2297 · `stage_moves` + `calls_value` (the one staging walk over every callee class) 2297-2678 · native result plans (`native_span_result`, `fallible_native_result`, `static_success_plan`, `fallible_total_result`, `classed_native_union`, `class_members_of`, `keyed_native_result`, `overlay_fallible`, `checked_native_result`, `err_value`, `ok_side`) 2678-2748 and 4329-5432 · evidence chains (`stage_generic_evidence`, `slot_evidence`, `chain_for`, `default_pair_chain`, `serving_body`, `branch_mentions_var`, `evidence_words`, `free_evidence_words`, `chain_start`, `stage_member_evidence`, `pair_trait_id`, `resolve_callee`, `bound_callee`) 2748-3912 · defaults (`defaulted_args`, `trait_default_exprs`, `member_default_exprs`) 3912-4061 · callee plan queries 4061-4555.

### (b) Proposed split — `crates/ds-lower/src/func/calls/`

| new file | items | ~lines |
| --- | --- | --- |
| `calls/mod.rs` | doc, `mod` decls, `pub(super) use`, `stage_moves`, `calls_value` — the staging walk stays the facade | ~430 |
| `calls/ctor.rs` | `element_slot_ty`, `guard_staged_width`, `scalar_arg_slot`, `direct_literal`, `direct_compute`, `ctor_call`, `map_ctor_call`, `case_value`, `embedded_*`, `embedded_const_expr`, defaults (`defaulted_args`, `trait_default_exprs`, `member_default_exprs`) | ~2,400 |
| `calls/evidence.rs` | the evidence-chain cluster + `pair_trait_id`, `resolve_callee`, `bound_callee`, `native_member_def`, `resolve_inherent_method`, `comptime_member_refusal`, `slot_evidence`, `stage_generic_evidence` | ~1,200 |
| `calls/result.rs` | every native-result shaper (`native_span_result` … `ok_side`) + callee plan queries (`callee_param_types` … `callee_result_words`, `guard_one_word_instantiation`, `guard_race_answer`, `is_unit_member`, `tail_builds_a_closure`) | ~1,450 |

### (c) Risks

- `SuccessShape`/`NativeMember`/`EvidenceSite` are used on both sides → move to `calls/mod.rs` or duplicate nothing.
- `calls_value` is called from inside `calls.rs` only (line 1203) and, once split, from `mod.rs` → `pub(super)` on it and on the ctor/evidence/result entry points it calls.
- The result shapers are the most invariant-dense code in the crate (fallible-call overlay arithmetic); splitting column-wise (each shaper with its plan query) is safer than the row-wise grouping above if the reviewer prefers cohesion, at the cost of a bigger `result.rs`.

## 6. `crates/ds-meta/src/derive.rs` — 4,900 code lines

### (a) Current concerns

1-111 doc + imports. 112-131 `RULED_SET`, `HASH_SEED`, `HASH_MIX`, `PRINTABLE`. 132-268 `expand`/`expand_owned`/`expand_trees`/`expand_tree_in_place`/`has_expansion`/`linear_heads`. 268-346 `expand_item` (the attribute dispatcher). 346-453 diagnostics helpers. Then one section per derive family, each minting its own AST: **show** 453-840, **ord** 840-1247, **hash** 1247-1501, **clone** 1501-1579, **default** 1579-2667 (incl. `AnnotatedCase`/`AnnotatedEnum`/`UnionDefaults`/`NoDefault`/`RefusedField`), **from_json** 2667-3666, **to_json** 3666-4379. Shared AST-minting helpers 4379-4747 (`local`, `local_typed`, `assign`, `assign_add`, `block_of`, `minted_path`, `Member`, `self_param`, `name_type`, `expr_stmt`, `text`, `str_expr`, `self_expr`, `path_expr`, `trait_type`, `parse_err_expr`, `key_literal`). `impl_item` 4559-4682. `Mint` 4747-4802. `written_spans`/`type_spans` 4802-4900. Tests 4901-4948.

### (b) Proposed split — `crates/ds-meta/src/derive/`

`derive.rs` becomes the parent (allowed alongside `derive/`): doc, `RULED_SET`/`HASH_*`/`PRINTABLE`, `expand*`, `expand_item`, diagnostics, `mod` decls + `pub(crate) use`, `mod tests`.

| new file | items | ~lines |
| --- | --- | --- |
| `derive/mint.rs` | `Mint`, `impl_item`, `Member`, `self_param`, `name_type`, `expr_stmt`, `local`, `local_typed`, `assign`, `assign_add`, `block_of`, `minted_path`, `text`, `str_expr`, `self_expr`, `path_expr`, `trait_type`, `parse_err_expr`, `key_literal`, `minted_type`, `field_read`, `binder_use`, `member_call`, `mint_binder`, `fresh_binder`, `case_pattern`, `arm`, `name_body`, `part_hole`, `field_hole`, `binder_hole`, `printable`, `written_spans`, `type_spans`, `struct_compare_body`-style shared builders if any | ~1,000 |
| `derive/show.rs` | 453-840 | 390 |
| `derive/ord.rs` | 840-1247 | 410 |
| `derive/hash.rs` | 1247-1501 | 255 |
| `derive/clone.rs` | 1501-1579 | 80 |
| `derive/default.rs` | 1579-2667 | 1,090 |
| `derive/from_json.rs` | 2667-3666 | 1,000 |
| `derive/to_json.rs` | 3666-4379 | 715 |

### (c) Risks

- **Cleanest seam in the tree**: each `*_impl` consumes a declaration and returns an `Item`; families do not call each other. The only shared state is `Mint` and the AST builders, both of which move to `mint.rs` as `pub(super)`.
- `expand_item` (the dispatcher) must import every family's `*_impl`; that is the only cross-module edge, plus `linear_heads`/`PRINTABLE` staying in the parent.
- `default.rs` and `from_json.rs` are each ~1,000 lines and will need their own second-level pass later (`default/union.rs`, `from_json/decode.rs`); flag, do not do now.
- Diagnostics constructors (`unknown`, `*_refused`, `duplicate_annotation`, …) are used by several families → keep them in the parent and `use super::*`.

## 7. `crates/ds-check/src/resolve.rs` — 3,783 lines

### (a) Current concerns

1-107 doc (namespace split, method tier). 108-563 id/table vocabulary: `DefId`, `DefKind`, `Vis`, `Def`, `MethodId`, `ModuleId`, `MethodOwner`, `MethodDecl`, `Res`, `DotRes`, `Resolutions` (+impl), `span_key`, `Unspellable`, `UseIndex` (+impl). 563-708 `Package` (+`PackageBase`, `PartialEq`/`Eq`, `NamespaceTiers`). 708-935 `impl Package` name-table accessors, `FileImports`, `ModuleRef`, `ModuleSurface`. 864-935 `resolve_package`/`resolve_package_linked` + `impl ModuleRef`. 935-2143 `impl Package` (1,208 lines): construction/`add_file`/`resolve`/`link_imports` 938-1016, import linking 1016-1197, member registration/validation 1197-1668, module alias binding 1668-1731, query surface 1731-1897, `collect_item`/`declare`/`register_method` 1897-2143. 2143-3721 `FileResolver` (1,578 lines): per-file walk (`item`/`type_params`/`fn_decl`/`param`/`local`/`block`/`stmt`/`expr` 2175-2557), visibility & def-use recording 2557-2607, imports 2607-2740, name/call/path use resolution 2740-2970, module-member and typed receiver resolution 2970-3354, types & patterns 3354-3646, diagnostics/candidates 3646-3721. 3721-3783 tail helpers.

### (b) Proposed split — `crates/ds-check/src/resolve/`

| new file | items | ~lines |
| --- | --- | --- |
| `resolve/mod.rs` | doc, id/table vocabulary (108-563) + `pub use`, `mod` decls | 470 |
| `resolve/package.rs` | `Package`/`PackageBase`/`FileImports`/`ModuleRef`/`ModuleSurface`/`resolve_package*` + the collection half of `impl Package` (`new`, `add_file`, `resolve`, `link_imports`, `register_member`, `validate_member`, `bind_module_alias`, `collect_item`, `declare*`, `register_method`) | ~1,500 |
| `resolve/query.rs` | the read-only surface of `impl Package` (`def`/`defs`/`method`/`methods`/`lookup*`/`declares`/`files`/`file_position`/`method_candidates`/`len`/`is_empty`/`foreign`/`reexports`/`module_ref`/`base_def_count`) | ~330 |
| `resolve/file.rs` | `FileResolver`, `ArmTag`, `impl FileResolver` (2143-3721) + tail candidate helpers | ~1,600 |

### (c) Risks

- `Resolutions`/`Res`/`DefId` are used by every other ds-check module (`use crate::resolve::…`); they stay in `mod.rs`.
- `impl Package` split across two files means two `impl` blocks; both need `pub(crate)` on the methods the other calls (`lookup_type`, `declare`, `register_method`), and the `Default`/`PartialEq` impls must stay with the type.
- `FileResolver` holds `&mut Package` plus per-file scope maps; it is the only writer of `Resolutions`, so it must not be split along anything but its walk (`item/type/expr` vs `pattern`) if that ever continues.

## 8. `crates/ds-check/src/decls.rs` — 3,045 lines

### (a) Current concerns

1-54 doc. 55-460 declaration tables: `GenericParam`, `ParamSig`, `FnSig`, `SolvedField`, `StructShape`, `EnumShape`, `CaseDecl`, `Decls` (201-352), `ForeignLink`, `ForeignImplLink`, `ServedDrift`, `ServedResolve`, `ServedDecls`. 461-827 `build`/`build_over` (the two declaration passes). 827-1611 foreign/linked install: `install_foreign_*`, `linked_instance_diag`, `ImplLayer`, `LinkSide`, `LinkOutcome`, `impl_side_spell`, `shadowed_exercised_diag`, `origin_positions_of`, `MintCtx`, `force_*`, `foreign_field_type`. 1611-1720 coherence (`is_served`, `package_coherence`, `item_def`, `type_def_for_use`). 1715-2010 reservation (`reserve_item`, `reserve_impl_fns`, `reserve_local`, `def_kind_is_binding`, `consts_base`). 2012-2276 shapes (`shape_item`, `struct_fields_ast`, `linear_marker`, `register_supers`, `trait_of_type`, `bound_traits`, `universe_fields`, `field_expr`). 2276-2665 written-type conversion & application checks (`ApplicationMismatch`, `check_application`, `conv_expr`, `conv_args`, `conv_node`, `solve_written`, `path_expr`, `path_def_expr`, `enum_case_union`, `atom_of_path`, `declare_*_unit_name`, `record_apply_structs`). 2742-2995 instances (`declare_inherent_bounds`, `declare_instance`, `instance_diag`, `member_id`, `trait_member_sig`, `pattern_names`, `item_name`, `render_type`).

### (b) Proposed split — `crates/ds-check/src/decls/`

| new file | items | ~lines |
| --- | --- | --- |
| `decls/mod.rs` | doc, all `pub` table types (55-460), `ServedDecls`, `build`/`build_over` (461-827), `mod` decls + re-exports | ~830 |
| `decls/foreign.rs` | 827-1611 (`install_foreign_*`, `ForeignImplLink` helpers, linking diags, `MintCtx`, `force_*`, `foreign_field_type`) | 785 |
| `decls/reserve.rs` | 1687-2010 + `package_coherence`/`is_served`/`item_def`/`type_def_for_use` | 340 |
| `decls/shape.rs` | 2012-2276 (`shape_item`, `linear_marker`, bounds, `universe_fields`, `field_expr`) | 265 |
| `decls/conv.rs` | 2276-2665 (written-type conversion, application checks, apply-struct recording) | 390 |
| `decls/instance.rs` | 2742-2995 | 255 |

### (c) Risks

- `shape_item`, `conv_expr`, `declare_instance` are called from `build_over` in the parent → `pub(super)`.
- `ImplFact`/`ForeignEntry` come from `crate::import`; the foreign/link half is the only place that reads them, so `decls/foreign.rs` and `import.rs` are a natural pair.
- `Decls`'s `install_base` (used by `ds-dsi`) lives on the table types in the parent; untouched.

## 9. `crates/ds-vm/src/object.rs` — 3,224 code lines (+1,568 test lines)

### (a) Current concerns

1-73 doc + imports. 74-224 GC/string core: `NO_FREE`, `Closure`, `Str`, `CharIndex` (+impl), `char_offsets`. 224-400 the payload union: `Object`, `ChanData`, `ArenaData`, `ArenaSlot`, `ChanSender`, `ChanPop`, `MapData`, `ListData`, `ROW_NONE`. 400-660 hashing: `fnv_step`, `class_seed`, `SEED_*`, `text_hash`, `ref_hash`, `word_eq`, `scalar_hash`, `ProbeKey` (+impl), `Slot`, `ForeignOp`, `DeathScratch`, `Interned`, `InternMap`, `InternHasher`, `Objects`. 660-699 `Debug`/`Drop` for `Objects`. 699-2265 `impl Objects` (1,566 lines): allocation/RC/intern 702-1341, list ops 1342-1626, text/char/bytes 1595-1790, maps 1789-2242, chan 934-1047, arena 1048-1207, staged-fn walk 1840-1907. 2265-3193 `struct Heaps` + `impl Heaps` (912 lines): ledger/claim routing 2284-2410, allocation 2408-2480, RC 2488-2610, text 2572-2650, collections 2650-3080, content faces 3081-3193. 3193-3225 `ContentFace`, `ResolvedKey`.

### (b) Proposed split — `crates/ds-vm/src/object/`

| new file | items | ~lines |
| --- | --- | --- |
| `object/mod.rs` | doc, `Object` payload union + payload types (`Closure`, `Str`, `CharIndex`, `ChanData`, `ArenaData`, `ChanSender`, `ChanPop`, `MapData`, `ListData`, `Slot`, `ForeignOp`, `DeathScratch`, `Interned`, `InternMap`, `InternHasher`), `struct Objects` + `Debug`/`Drop`, `pub use` re-exports | ~700 |
| `object/rc.rs` | allocation/RC/intern: `alloc`, `intern`, `intern_borrow`, `release_pins`, `inc`, `dec`, `free`, `rc_at`, `header_of`, `payload_of`, `acquire_stored`/`release_stored`, `for_each_staged_fn` | ~600 |
| `object/text.rs` | `Str`/`CharIndex` impls, `char_offsets`, `alloc_str*`, `str_at`, `char_count`, `char_byte_range`, `ensure_char_*`, `str_with_chars`, `alloc_bytes`, `bytes_at`, `is_live_str_with`, `str_with_hash`, `concat` | ~450 |
| `object/list.rs` | `alloc_list*`, `push_list`, `set_list`, `remove_list`, `insert_list`, `clear_list`, `concat_lists`, `list_parts`, `list_at`, `list_stride` | ~350 |
| `object/map.rs` | hashing (`fnv_step`…`scalar_hash`), `ProbeKey`, `MapData`, `resolve_probe`, `stored_key_matches`, `map_row_at`, `alloc_map`, `set_map`, `map_at`, `map_len`, `map_keys`, `remove_map`, `contains_map`, `map_entries` | ~700 |
| `object/chan.rs` | `alloc_chan`, `chan_deposit`, `chan_pop`, `payload_chan*` | ~150 |
| `object/arena.rs` | `alloc_arena`, `arena_unpack`, `arena_take_slot`, `arena_insert`, `arena_word`, `arena_remove`, `arena_len` | ~180 |
| `object/heaps.rs` | `Heaps` + impl + `ContentFace`/`ResolvedKey` | ~950 |

### (c) Risks

- `Objects`' fields are private and used by nearly every moved method; child modules may read them, so no field churn.
- `unsafe impl Send for Objects` and `Drop` must stay with the struct in `mod.rs` (their SAFETY comment cites field layout).
- `Heaps` is the routing layer (`store`/`store_shared`/`ledger`/`settle`) that every native uses; it must keep `pub(crate)` on all of it, and it must not be split from `Objects` (they share `owner` and the pending-op queue).
- `alloc_closure`/`closure_at`/`replace_capture` are called by the CALLS dispatcher; `object/rc.rs` is the natural home but they are the highest-traffic allocations — verify with the existing VM benchmarks after the move.

## 10. `crates/ds-dsi/src/lib.rs` — 2,507 code lines (+169) — four phases

### (a) Current concerns

1-65 doc + imports. 66-612 the wire format as data: `HEADER`, `PrefixDsiError`, `ServedPrefix`, `PrefixDsi`, `PackageData`, `DefData`, … `MethodRowData` (all postcard DTOs). 612-660 kind/vis bit mapping. 662-805 `artifact_bytes`, `extract`. 805-1361 the encode walk: `Names`, `universe_log`, `decls_data`, `instance_rows_of`, `*_data` converters, `assert_stable`, `res_data`. 1361-1504 decode entry: `decode_prefix`, `decode_dsi`, `serve_prefix`, `serve_prefix_tables`, `ServedTables`, `convert_tables`. 1620-1729 `serve` + `ServedRows` + drift helpers. 1729-2109 resolution rebuild (`resolve_atom`, `resolve_trait`, `resolve_expr`, `drain_sets`, `resolve_rows`). 2109-2224 `package_base`, `serve_resolutions`. 2224-2508 inline `pub mod surface`/`pub mod gates` declarations (bodies live in `src/surface.rs`, 660 lines).

### (b) Proposed split — `crates/ds-dsi/src/`

| new file | items | ~lines |
| --- | --- | --- |
| `lib.rs` (keeps) | doc, `mod` decls, `pub use`, `pub mod surface`, `pub mod gates`, `mod tests` | ~120 |
| `dsi/data.rs` | all DTO structs/enums (66-612) + kind/vis bits | 610 |
| `dsi/encode.rs` | `artifact_bytes`, `extract`, `Names`, `universe_log`, `decls_data`, `instance_rows_of`, `generic_param*`, `fn_sig*`, `instance_row_data`, `assert_stable`, `field_data`, `case_data`, `type_expr_data`, `res_data` | 700 |
| `dsi/decode.rs` | `decode_prefix`, `decode_dsi`, `ServedTables`, `convert_tables` | 270 |
| `dsi/serve.rs` | `serve_prefix*`, `serve`, `ServedRows`, `drift`, `name_of`, `resolve_*`, `drain_sets`, `resolve_rows`, `package_base`, `serve_resolutions` | 600 |

### (c) Risks

- `gates`/`surface` import DTOs through `super::` (crate root). A `pub use data::*;` in `lib.rs` keeps every `super::AtomKind` resolving; this must be `pub use`, not `pub(crate) use`.
- The DTO set is a serialized wire format: moving types is inert, but the `#[derive(Serialize, Deserialize)]` order in `PrefixDsi` must not change (postcard positional encoding).
- `decode_prefix`'s `HEADER` chunk split belongs with its decoder (`decode.rs`), while `artifact_bytes` writes that header (`encode.rs`) — the two must keep agreeing on `HEADER`, which stays in `lib.rs` or `data.rs`, not duplicated.

## 11. `crates/ds-vm/src/net.rs` — 2,245 code lines (+444)

### (a) Current concerns

1-137 doc. 138-160 `NET_TIMEOUT`, `NET_CAP`, `HTTP_PORT`, `HTTP_CHUNK`, `ACCEPT_POLL`. 160-330 `Connections` (+`Default`), `NetAttempt`, `NetConnectStart`, `ConnectWake`, `FetchWake`, `AcceptWake`, `Fetch`. 340-490 `mod raw_sock` (the raw fd backend, `cfg(target_os = "linux")`). 497-1430 `impl Connections` (933 lines, 60+ `cfg` arms inside). 1430-1627 `Listeners` (+`Default`, impl). 1627-1700 error mapping (`unknown*`, `stream_handle`, `normalize`, `NetClass` +impl, `net_err`, `ok`). 1759-1850 URL/HTTP request shaping (`Scheme`, `scheme_of`, `split_target`, `request`, `exchange`, `finish_response`). 1850-2245 the natives (`net_connect_fn`, `net_send_fn`, `net_recv_fn`, `recv_answer`, `net_close_fn`, `net_listen_fn`, `net_accept_fn`, `net_listener_close_fn`, `net_local_addr_fn`, `http_get_fn`, `plain_fetch`, `parked_plain_fetch`).

### (b) Proposed split — `crates/ds-vm/src/net/`

| new file | items | ~lines |
| --- | --- | --- |
| `net/mod.rs` | doc, consts, the wake/attempt enums, `mod` decls + `pub(crate) use` | ~200 |
| `net/raw.rs` | `mod raw_sock`, `fd_into_stream`, `stream_handle` | ~200 |
| `net/conn.rs` | `Connections` + `impl Connections` (497-1430) | 950 |
| `net/listen.rs` | `Listeners` + impl, `AcceptWake` handling | 250 |
| `net/error.rs` | `unknown*`, `normalize`, `NetClass`, `net_err`, `ok` | 130 |
| `net/http.rs` | `Scheme`, `scheme_of`, `split_target`, `request`, `exchange`, `finish_response`, `plain_fetch`, `parked_plain_fetch` | 300 |
| `net/natives.rs` | the ten `net_*_fn` natives | 400 |

### (c) Risks

- **Highest `cfg` density in the tree** (62 items). `Connections`' methods are half per-platform: the Linux arm drives a reactor over raw fds, the non-Linux arm a blocking socket. Any move must move whole `#[cfg]`-annotated items, and a split that separates a `cfg(feature)` pair from its `not(...)` twin is an easy way to break the Windows build silently. Recommend moving `conn.rs` + `raw.rs` first (mechanical once `cfg` items travel together) and skipping the rest.
- `Connections`' private state (`conns`, `fetches`, waiter maps) is the reactor's registration table; only `conn.rs` writes it, `natives.rs` only calls methods → `pub(super)` on those methods.

## 12. `crates/ds-check/src/linearity.rs` — 2,066 code lines

### (a) Current concerns

1-107 doc. 108-174 `BinderFact`, `Move`, `DropSite`, `LastUse`, `Linearity`. 174-390 entry points (`check`, `linearity_check`, `ServedLinearity`, `DefFact`, `linearity_check_served`). 387-510 copyability (`ty_copyable`, `ty_copyable_seen`, `atom_copyable`, `field_copyable`, `expr_copyable`, `struct_linear`, `no_linear_vars`). 510-601 state vocabulary (`State`, `Place` (+impl), `Info`, `States`, `Delta`, `StateUndo`, `comptime_copy_counts`). 601-1950 `Lin` + `impl Lin` (1,300 lines) — the walk. 1950-2066 `moved_diag`, `merge_branch`, `peel`, `PlaceStep`, `place_root`, `place_steps`. Tests 2067+.

### (b) Proposed split — `crates/ds-check/src/linearity/`

| new file | items | ~lines |
| --- | --- | --- |
| `linearity/mod.rs` | doc, `BinderFact`/`Move`/`DropSite`/`LastUse`/`Linearity`/`ServedLinearity`, entry points, `mod` decls + re-export, `mod tests` | ~350 |
| `linearity/copyable.rs` | the `ty_copyable` family (387-510) | 125 |
| `linearity/state.rs` | `State`/`Place`/`Info`/`States`/`Delta`/`StateUndo`/`PlaceStep`, `merge_branch`, `place_root`, `place_steps`, `peel` | 260 |
| `linearity/walk.rs` | `Lin` + `impl Lin` (601-1950) | 1,350 |

### (c) Risks

- `impl Lin` is a single 1,300-line walk on one struct; the split above only lifts the state vocabulary out. A real second pass would cut the walk by statement class (branches/loops/moves/drops) — that is behavior-adjacent work.
- `BinderFact` is written by the checker (`type_of`) and read here; it stays in `mod.rs`.

## 13. `crates/ds-check/src/import.rs` — 2,054 code lines (+1,749 tests)

### (a) Current concerns

1-22 doc. 23-196 `ConstFact`. 196-1361 fact vocabulary: `TypeFact`, `TraitFact`, `TraitMemberFact`, `ImplFact`, `ImplMemberFact`, `TypeCase`, `CasePayload`, `CaseFact`, `TypeField`, `TypeWord`, `ScalarWord`, `PrimitiveWord` (+`PRIMITIVE_WORDS`), `HeapRefWord` (+`HEAP_REFS`), `InherentFact`, `InherentMemberFact`, `impl TypeFact`, `impl ImplFact`. 1280-1361 `RefusedDefault`, `refused_default_bodies`. **1361-1815 `WordSolver` + impl** (the encoded-word → `TypeWord` decoder). 1815-2055 surface/graph: `ImportSurface`, `MemberSurface`, `ForeignEntry`, `ForeignKind`, `ImportGraph` (+impl), `PartialEq` impls.

### (b) Proposed split — `crates/ds-check/src/import/`

| new file | items | ~lines |
| --- | --- | --- |
| `import/mod.rs` | doc, `ConstFact`, `TypeFact`, `Trait*`, `Impl*`, `Case*`, `TypeField`, `RefusedDefault`/`refused_default_bodies`, `mod` decls + re-exports, `mod tests` | ~1,000 |
| `import/word.rs` | `TypeWord`, `ScalarWord`, `PrimitiveWord`, `HeapRefWord`, tables, `render_word`, `WordSolver` | ~500 |
| `import/graph.rs` | `ImportSurface`, `MemberSurface`, `ForeignEntry`, `ForeignKind`, `ImportGraph` | ~250 |

### (c) Risks

- `import.rs` is consumed by `decls.rs`, `resolve.rs`, `ds-lower`, `ds-dsi`, `ds-cli` through `crate::import::*`; the `pub use` list in `import/mod.rs` must be complete (the `pub` surface here is wide).
- Tests (1,749 lines) exercise fact equality/rendering; keep in `mod.rs`.

## 14. Low-priority but real seams

- **`crates/ds-lower/src/func/pattern.rs`** (2,022, one impl): control forms (`if_value`, `match_arms`, `match_single`, `value_match_chain`, lines 8-594) vs pattern binding + test emission (594-2022). Split `pattern/control.rs` + `pattern/bind.rs` only if it grows; 2k with one concern (`match` lowering) is defensible.
- **`crates/ds-lsp/src/lib.rs`** (1,846 code): protocol `Request`/`Target`/`Reply` types + `Session` (state: `open`/`set`/`edit`/`file`/`uri`/`surface`, features: `hover`/`definition`/`completion`/`references`/`rename`/`symbols`/`format`/`on_type_format`/`semantic_tokens`) + worker/handle. Split `lsp/session.rs` (state + query plumbing) from `lsp/features.rs` and leave `lib.rs` as the protocol + worker. The crate already has six sibling modules (tokens/completion/walk/convert/server/autoimport), so this is consistent with its existing shape. Tests (2,122) keep `use super::*`.

## Explicitly skipped (split not justified)

- `crates/ds-lower/src/native.rs` (1,824) — one concern: the native-call description table and its evidence/return shapes.
- `crates/ds-lower/src/shape.rs` (1,724 code) / `place.rs` (1,344 code) — single concern each (type shape resolution, placement/affine facts), already below the 2k line.
- `crates/ds-parse/src/convert/mod.rs` (1,695) — one linear AST → encoded-form conversion.
- `crates/ds-cli/src/repl.rs` (1,728), `package.rs` (1,437 code), `source.rs` (1,279 code), `run.rs` (992) — CLI commands; each is one command's flow. The crate total is 11,627 with 21 files, i.e. already module-per-command; splitting further would fragment one command across files.
- `crates/ds-check/src/effects.rs` (1,725), `instance.rs` (1,051), `instance_solve.rs` (1,230), `universe.rs` (770) — under the threshold.
- `crates/ds-vm/src/proc.rs` (1,479), `module.rs` (1,369) — under the threshold.

## Recommended order (highest value per unit of risk)

1. `ds-meta/src/derive.rs` — 8 files, no shared mutable state, each family independent. Proves the `pub(super)` convention with the least blast radius.
2. `crates/ds-dsi/src/lib.rs` — encode/decode/serve are phase-disjoint, and the crate is small enough to review in one pass.
3. `crates/ds-vm/src/object.rs` — text/list/map/chan/arena are value-family disjoint; the largest single-file win after `vm.rs` with no scheduling involvement.
4. `crates/ds-vm/src/native.rs` — family-per-file is mechanical; `builtin_module()` is the only coupling point.
5. `crates/ds-check/src/{import,decls,resolve,linearity}.rs` — vocabulary-vs-algorithm splits, all `pub(super)`-only.
6. `crates/ds-vm/src/vm.rs` stages A → B → C (per §1). Do `trap`/`proto`/`value`/`call`/`chan`/`race`/`debug` first; `execute.rs` is a move, its arm extraction is separate work.
7. `crates/ds-lower/src/func.rs` + `func/calls.rs` — largest `pub(super)` churn (170 + 15 private methods); do after the VM pass has exercised the convention.
8. `crates/ds-check/src/type_of/mod.rs` — the 187-private-method engine; last, and stage it (item/record/expr/name first, then call/select, flow/solve, query).
