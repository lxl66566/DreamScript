//! The dispatcher: decode-on-dispatch over the encoded uop bodies, one arm per
//! opcode byte.

use super::*;

impl Vm {
    /// The dispatcher: decode-on-dispatch (see the module docs), then a
    /// dense `match` over the uop (computed goto is not available on stable
    /// Rust). A drive ends when the current fiber completes
    /// ([`DriveOutcome::Died`]) or parks with the host as resumer
    /// ([`DriveOutcome::Yielded`]); switches between fibers happen inside.
    /// `boundary` is the call-chain depth of a nested reentrant drive's
    /// synthetic reentry frame ([`Vm::call_hosted`]): a host-level drive
    /// passes `usize::MAX` and never sees [`DriveOutcome::Frame`], and a
    /// nested drive answers it when its reentry frame returns.
    /// `FUELED` selects the budgeted instantiation ([`Vm::run_with_budget`]
    /// alone drives it): `true` charges `fuel` one step per decoded
    /// instruction and answers [`Trap::OutOfFuel`] at zero; `false` compiles
    /// the charge away entirely, keeping the unbounded drives' dispatch
    /// cost (the pin table's contract) untouched.
    // One flat match over the whole uop set is the point of a dispatch loop;
    // splitting arms into helpers would fragment it and add a call
    // per op in debug builds. The bias word re-reads u16 as i16 (the
    // format's documented flip-is-subtract), and the call arms bind the
    // caller/callee pair the site vocabulary names.
    #[allow(clippy::too_many_lines)]
    #[allow(clippy::cast_possible_wrap)]
    #[allow(clippy::similar_names)]
    pub(super) fn execute<const FUELED: bool>(
        &mut self,
        mut active: Active,
        mut pc: CodePc,
        boundary: usize,
    ) -> Result<DriveOutcome, (Trap, CodePc)> {
        // The frame context (body, code, slots, pool, fp) is derived once
        // per frame transfer, not per instruction: the executing frame and
        // its fp change only at the transfer arms (CALL/CALLT/CALLS, the
        // RET/RETSPAN/HALT resume paths, a control-native switch, the
        // cancel switch), and every other instruction runs against the
        // same immutable body — the frame-context cache. A transfer arm
        // assigns `active`/`pc` and `continue 'transfer`; the pc bounds
        // check stays per instruction.
        'transfer: loop {
            // the frame context reads the version table through the
            // address-stable pointer its row pins, so the borrows below
            // hold nothing on the VM while the arms call into it whole
            let f = &self.registry.pinned(active.version).functions()[active.func as usize];
            let code = &f.code;
            let slots = f.layout.slots();
            let frame_slots = slots.len();
            let body_len = f.layout.body_len();
            let pool = &f.pool;
            let fp = self.callinfo[self.callinfo.len() - 1].fp;
            // The frame's KSTR intern row: resolved by the frame's first
            // literal load, read directly by every later one. A row that
            // reclaim drops or shifts revalidates against its own key.
            let mut kstr_slot: Option<KstrRowSlot> = None;
            loop {
                // the budgeted drive's step charge: the constant folds
                // the whole block away on the unbounded instantiation,
                // so only `run_with_budget`'s drive pays it
                if FUELED {
                    match self.fuel {
                        0 => return Err((Trap::OutOfFuel, pc)),
                        _ => self.fuel -= 1,
                    }
                }
                let idx = pc.0 as usize;
                if idx >= code.len() {
                    let e = Trap::InvalidPc {
                        pc: pc.0,
                        code_len: code_len_u32(code.len()),
                    };
                    return Err((e, pc));
                }
                // The debugger's stop-before (the one data-path guard the
                // tier contract allows beside the stack-limit compare):
                // a pc the armed set names parks the fiber there before
                // the word executes, and the resumed drive's re-take of
                // the parked pc is the resume itself, skipped once. Never
                // under a teardown drive, whose walk must finish. The
                // span table is read at arm time only; the per-instruction
                // cost when disarmed is one branch on the Option.
                if self.breakpoints.is_some()
                    && !self.teardown_drive
                    && self
                        .breakpoints
                        .as_ref()
                        .is_some_and(|set| set.contains(&(active.version.0, active.func, pc.0)))
                {
                    if self.breakpoint_skip.take() == Some((self.current, pc)) {
                        // the resumed hit's own re-take
                    } else {
                        let fiber = &mut self.pool[self.current.index()];
                        fiber.resume = Some(ResumePoint {
                            version: active.version,
                            func: active.func,
                            pc,
                        });
                        fiber.state = FiberState::Preempted;
                        self.last_breakpoint = Some((active.version, active.func, pc.0));
                        self.breakpoint_park = true;
                        self.breakpoint_skip = Some((self.current, pc));
                        return Ok(DriveOutcome::Yielded(Value::Tag(PLATFORM_NONE)));
                    }
                }
                macro_rules! jump {
                    ($target:expr) => {{
                        let t = $target;
                        if t as usize >= code.len() {
                            let e = Trap::InvalidPc {
                                pc: t,
                                code_len: code_len_u32(code.len()),
                            };
                            return Err((e, pc));
                        }
                        // the preemption point: a taken backward branch
                        // (a loop back-edge, self or below) is where the running
                        // fiber observes its nursery's cancel flag. Never
                        // mid-instruction: a linear consume in flight is done.
                        // Inside a teardown hook drive the observation refuses
                        // instead of cutting: the cut would kill the fiber whose
                        // death walk is driving the hook (a teardown hook must
                        // not re-enter the nursery)
                        // the sampling profiler's tick: every Nth
                        // taken back edge records the running frame's
                        // position into the histogram, source resolved
                        // only at report time through the span tables
                        if self.sample_every != 0 && t <= pc.0 {
                            self.sample_tick += 1;
                            if self.sample_tick >= self.sample_every {
                                self.sample_tick = 0;
                                *self
                                    .samples
                                    .entry((active.version.0, active.func, pc.0))
                                    .or_insert(0) += 1;
                            }
                        }
                        if t <= pc.0 {
                            self.fuel_tick = self.fuel_tick.wrapping_sub(1);
                            if self.fuel_tick == 0 {
                                self.fuel_tick = FUEL_QUANTA;
                                self.poll_race_deadline();
                            }
                        }
                        if t <= pc.0 && self.cancel_requested() {
                            if self.teardown_drive {
                                return Err((Trap::ReentrancyRefused, pc));
                            }
                            match self.cancel_current() {
                                Some((a, at)) => {
                                    active = a;
                                    pc = at;
                                    continue 'transfer;
                                },
                                None => {
                                    return Ok(DriveOutcome::Died(FrameAnswer::one(Value::Tag(
                                        PLATFORM_CANCELLED,
                                    ))))
                                },
                            }
                        }
                        // the host's suspension request [the dev host's
                        // capability face]: the requested fiber parks at the
                        // same observation point a cancel uses, and the drive
                        // ends handing the host its continuation. The resume
                        // point is the branch itself: the frame is frozen in
                        // between, so the resumed drive re-takes the branch
                        // and decides the same. The parked branch's own
                        // re-take is the resume — the skip marker — so a
                        // resumed drive parks at its NEXT backward branch,
                        // one loop iteration per drive. Never inside a
                        // hosted reentry frame — a Rust-side native loop
                        // cannot suspend, so the request waits for the
                        // hosting drive to return — and never under a
                        // teardown drive, which must finish its walk.
                        if t <= pc.0
                            && self.suspend_request == Some(self.current)
                            && !self.teardown_drive
                            && !self.reentry_live(boundary)
                        {
                            if self.suspend_skip.take() == Some((self.current, pc)) {
                                // the resumed branch's own re-take
                            } else {
                                self.suspend_request = None;
                                let fiber = &mut self.pool[self.current.index()];
                                fiber.resume = Some(ResumePoint {
                                    version: active.version,
                                    func: active.func,
                                    pc: CodePc(pc.0),
                                });
                                fiber.state = FiberState::Preempted;
                                return Ok(DriveOutcome::Yielded(Value::Tag(PLATFORM_NONE)));
                            }
                        }
                        pc = CodePc(t);
                    }};
                }
                macro_rules! stack_state {
                    () => {
                        StackState {
                            stack: &mut self.stack,
                            callinfo: &mut self.callinfo,
                            versions: &self.registry,
                            limit: self.limit,
                            max_len: &mut self.pool[self.current.index()].max_len,
                        }
                    };
                }
                // the inline canonicity doors every fast arm shares: each
                // names the same error the decoder would, without the
                // cross-crate call per instruction
                macro_rules! bad_encoding {
                    ($cause:expr) => {
                        return Err((
                            Trap::BadEncoding {
                                pc: pc.0,
                                cause: $cause,
                            },
                            pc,
                        ))
                    };
                }
                macro_rules! reserved {
                    () => {
                        bad_encoding!(DecodeError::ReservedField)
                    };
                }
                // the biased jump offset of the uop format, branch-free
                let word = code[idx];
                let [byte, fa, fb, fc] = word.to_be_bytes();
                let fd = u16::from_be_bytes([fb, fc]);
                // the raw jump target: a fast arm's bias math; a target
                // outside the u32 index range is the same JumpOutOfRange
                // the decoder reports
                macro_rules! jump_target {
                    () => {
                        match u32::try_from(i64::from(pc.0) + 1 + i64::from((fd ^ BIAS_J) as i16)) {
                            Ok(target) => target,
                            Err(_) => {
                                bad_encoding!(DecodeError::JumpOutOfRange {
                                    pc: pc.0,
                                    field: fd
                                })
                            },
                        }
                    };
                }
                // the fused compare-branch tail: pc first bumped onto the
                // branch's own word, then the JMPIF_NOT arm's exact body —
                // the biased target from the peeked word's field (the same
                // math and JumpOutOfRange attribution `jump_target!` gives
                // the decoded word at that pc), the decision on the
                // computed bool (the branch reads the dst the compare just
                // wrote, so the read cannot fail and the value is in
                // hand), the taken branch through the `jump!` macro body
                // verbatim, and the not-taken fallthrough at the branch's
                // next word — the pair consumed two words
                macro_rules! fused_jmpif_not {
                    ($field:expr, $cond:expr) => {{
                        pc.0 += 1;
                        let Ok(target) = u32::try_from(
                            i64::from(pc.0) + 1 + i64::from(($field ^ BIAS_J) as i16),
                        ) else {
                            bad_encoding!(DecodeError::JumpOutOfRange {
                                pc: pc.0,
                                field: $field
                            })
                        };
                        if $cond {
                            pc.0 += 1;
                        } else {
                            jump!(target);
                        }
                    }};
                }
                // the fused tag-branch tail: the JMPIF polarity of
                // `fused_jmpif_not!` — taken on the held bool — over the
                // same pc-first bump onto the branch's own word
                macro_rules! fused_jmpif {
                    ($field:expr, $cond:expr) => {{
                        pc.0 += 1;
                        let Ok(target) = u32::try_from(
                            i64::from(pc.0) + 1 + i64::from(($field ^ BIAS_J) as i16),
                        ) else {
                            bad_encoding!(DecodeError::JumpOutOfRange {
                                pc: pc.0,
                                field: $field
                            })
                        };
                        if $cond {
                            jump!(target);
                        } else {
                            pc.0 += 1;
                        }
                    }};
                }
                // one dispatch table over the raw opcode byte: the loader
                // validated every word, and decode stays the canonicity
                // authority for whatever the fast arms refuse, so the hot
                // loop extracts each arm's operand fields inline and jumps
                // through the byte table — adding a uop adds one arm and
                // never re-lowers its neighbors. Field names follow the uop
                // format: fa/fb/fc the register bytes, fd the wide field.
                match byte {
                    OP_FUNCF => {
                        if fa != 0 {
                            reserved!();
                        }
                        if fd > MAX_FRAME_SLOTS {
                            bad_encoding!(DecodeError::FrameSlotsExceedCap { frame_slots: fd });
                        }
                        let n = fd;
                        if usize::from(n) != frame_slots {
                            let e = Trap::FrameSlotsMismatch {
                                funcef: n,
                                layout: frame_slots,
                            };
                            return Err((e, pc));
                        }
                        pc.0 += 1;
                    },
                    // the `?` error leg's propagation mark: no
                    // operands, the frame's own position is the datum.
                    // Armed, the hop joins the running fiber's private
                    // ring, innermost first, the oldest dropping past the
                    // cap; disarmed, the mark falls through — the switch
                    // is VM state, so the same body runs either way.
                    OP_ERR_MARK => {
                        if self.trace_errors {
                            let fiber = &mut self.pool[self.current.index()];
                            let ring = &mut fiber.err_ring;
                            ring.push_back(ErrHop {
                                version: active.version,
                                func: active.func,
                                pc: pc.0,
                            });
                            if ring.len() > ERR_RING_CAP {
                                ring.pop_front();
                            }
                        }
                        pc.0 += 1;
                    },
                    OP_KIMM => {
                        let dst = fa;
                        let kidx = ConstIdx(fd);
                        // install proved the operand's pool index in range and
                        // its kind I64 (module.rs's body validation); the fetch
                        // indexes directly and the kind arm is the
                        // corrupted-pool tripwire
                        let v = match &pool[usize::from(kidx.0)] {
                            Const::I64(v) => *v,
                            found => {
                                let e = Trap::PoolKind {
                                    kidx: kidx.0,
                                    expected: ConstKind::I64,
                                    found: found.kind(),
                                };
                                return Err((e, pc));
                            },
                        };
                        if let Some((op, adst, lhs, rhs)) = fused_i64_arith(code, idx, dst) {
                            // the load's own errors are spent; the fused
                            // arith's errors attribute to its own pc
                            write_slot(&mut self.stack, fp, dst, slots, body_len, Value::I64(v))
                                .map_err(|e| (e, pc))?;
                            let at = CodePc(pc.0 + 1);
                            let (x, y) = (
                                read_i64(&self.stack, fp, lhs, slots, body_len)
                                    .map_err(|e| (e, at))?,
                                read_i64(&self.stack, fp, rhs, slots, body_len)
                                    .map_err(|e| (e, at))?,
                            );
                            // the ok_or_else form keeps the overflow
                            // value off the success path; the eager ok_or
                            // form regresses the arith kernels' instruction
                            // count
                            #[allow(clippy::unnecessary_lazy_evaluations)]
                            let result = match op {
                                OP_ADD_I64 => x.checked_add(y).ok_or_else(|| Trap::IntegerOverflow),
                                OP_SUB_I64 => x.checked_sub(y).ok_or_else(|| Trap::IntegerOverflow),
                                OP_MUL_I64 => x.checked_mul(y).ok_or_else(|| Trap::IntegerOverflow),
                                // the fourth fused arith, MODI64: the same
                                // zero-divide / overflow split as DIVI64,
                                // spelled inline so `int_mod` keeps its one
                                // call site (a second site leaves the helper
                                // outlined, and the per-mod call and result
                                // move show up on the reentry kernels)
                                _ => {
                                    if y == 0 {
                                        Err(Trap::DivideByZero)
                                    } else {
                                        x.checked_rem(y).ok_or_else(|| Trap::IntegerOverflow)
                                    }
                                },
                            }
                            .map_err(|e| (e, at))?;
                            write_slot(
                                &mut self.stack,
                                fp,
                                adst,
                                slots,
                                body_len,
                                Value::I64(result),
                            )
                            .map_err(|e| (e, at))?;
                            pc.0 += 2;
                        } else {
                            write_slot(&mut self.stack, fp, dst, slots, body_len, Value::I64(v))
                                .map_err(|e| (e, pc))?;
                            pc.0 += 1;
                        }
                    },
                    OP_KCONST => {
                        let dst = fa;
                        let kidx = ConstIdx(fd);
                        // install proved the operand's pool index in range and
                        // its kind among F64/Bool/FuncRef (module.rs's body
                        // validation); the fetch indexes directly and the kind
                        // arm is the corrupted-pool tripwire
                        let v = match &pool[usize::from(kidx.0)] {
                            Const::F64(v) => Value::F64(*v),
                            Const::Bool(v) => Value::Bool(*v),
                            // A fn word: an install-resolved id mints
                            // the word directly — the dsmod install already
                            // walked position → dep name → module id, and the
                            // mint carries exactly that one vector index.
                            // A retiring frame's own-module ref carries its
                            // row's index space, so the mint translates it
                            // into the current table (below): the minted
                            // value enters current-index-valid exactly like
                            // a re-keyed staged word. A name-keyed callee
                            // resolves at issue time: a directly installed
                            // module through the registry's id space, a
                            // native surface refusing as no callable word
                            // (natives are not values). The loader validated
                            // the reference at install, so an unresolved
                            // name is a corrupted pool, never a program
                            // fact.
                            Const::FuncRef(callee) => match callee {
                                ConstFuncRef::Uop { module, func } => StackState::minted_fn_word(
                                    &self.registry,
                                    active.version,
                                    FuncRef::new(*module, *func),
                                )
                                .map_err(|e| (e, pc))?,
                                ConstFuncRef::Named { module, func } => {
                                    let module_id = self.registry.module_id(module).ok_or((
                                        Trap::UnknownCallee {
                                            module: module.clone(),
                                            func: *func,
                                        },
                                        pc,
                                    ))?;
                                    StackState::minted_fn_word(
                                        &self.registry,
                                        active.version,
                                        FuncRef::new(module_id, *func),
                                    )
                                    .map_err(|e| (e, pc))?
                                },
                                // a bound C symbol is not a callable word:
                                // the lowering routes every bound
                                // extern through its call sites, so a
                                // value-site use never lowers to this pool
                                // kind — the honest refusal, the same
                                // discipline a native surface takes
                                ConstFuncRef::Bound { module, index, .. } => {
                                    return Err((
                                        Trap::UnknownCallee {
                                            module: module.clone(),
                                            func: *index,
                                        },
                                        pc,
                                    ));
                                },
                            },
                            found => {
                                let e = Trap::PoolKind {
                                    kidx: kidx.0,
                                    expected: ConstKind::F64,
                                    found: found.kind(),
                                };
                                return Err((e, pc));
                            },
                        };
                        if let Some((op, adst, lhs, rhs)) = fused_f64_arith(code, idx, dst) {
                            // the load's own errors are spent; the fused
                            // arith's errors attribute to its own pc
                            write_slot(&mut self.stack, fp, dst, slots, body_len, v)
                                .map_err(|e| (e, pc))?;
                            let at = CodePc(pc.0 + 1);
                            let (x, y) = (
                                read_f64(&self.stack, fp, lhs, slots, body_len)
                                    .map_err(|e| (e, at))?,
                                read_f64(&self.stack, fp, rhs, slots, body_len)
                                    .map_err(|e| (e, at))?,
                            );
                            let result = if op == OP_ADD_F64 { x + y } else { x * y };
                            write_slot(
                                &mut self.stack,
                                fp,
                                adst,
                                slots,
                                body_len,
                                Value::F64(result),
                            )
                            .map_err(|e| (e, at))?;
                            pc.0 += 2;
                        } else {
                            write_slot(&mut self.stack, fp, dst, slots, body_len, v)
                                .map_err(|e| (e, pc))?;
                            pc.0 += 1;
                        }
                    },
                    OP_KSTR => {
                        let dst = fa;
                        let kidx = ConstIdx(fd);
                        let obj = self
                            .kstr_load(pool, kidx, true, &mut kstr_slot)
                            .map_err(|e| (e, pc))?;
                        let word = RefId::new(self.current, obj);
                        // the load's acquire is the loading frame's claim
                        self.claims[self.current.index()].acquire(word);
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Ref(word))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_KSTR_BORROW => {
                        let dst = fa;
                        let kidx = ConstIdx(fd);
                        // the placement pass proved this word's count never
                        // moves inside its frame, so the load borrows the
                        // intern map's pinned canonical copy: no store
                        // count, no claim ledger row, no terminal dec
                        let obj = self
                            .kstr_load(pool, kidx, false, &mut kstr_slot)
                            .map_err(|e| (e, pc))?;
                        let word = RefId::new(self.current, obj);
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Ref(word))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_ADD_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        let Some(sum) = x.checked_add(y) else {
                            return Err((Trap::IntegerOverflow, pc));
                        };
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::I64(sum))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_SUB_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        let Some(diff) = x.checked_sub(y) else {
                            return Err((Trap::IntegerOverflow, pc));
                        };
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::I64(diff))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_MUL_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        let Some(prod) = x.checked_mul(y) else {
                            return Err((Trap::IntegerOverflow, pc));
                        };
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::I64(prod))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_DIV_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        let q = int_div(x, y).map_err(|e| (e, pc))?;
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::I64(q))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_MOD_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        let r = int_mod(x, y).map_err(|e| (e, pc))?;
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::I64(r))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    // the wrapping trio never traps (the wrap set), so
                    // these are the only IntArith arms without a cold
                    // overflow edge
                    OP_WRAPADD_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::I64(x.wrapping_add(y)),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_WRAPSUB_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::I64(x.wrapping_sub(y)),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_WRAPMUL_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::I64(x.wrapping_mul(y)),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    // the shift and bitwise quintet stays out of the
                    // hot loop's match shape: one range arm hands the
                    // word to [`shift_bitwise_word`], so the five extra
                    // opcode arms never re-lower the dispatch's
                    // neighbors (the pin bake caught them costing ~1% Ir
                    // on every kernel)
                    OP_SHL_I64..=OP_BITOR_I64 => {
                        shift_bitwise_word(&mut self.stack, byte, fa, fb, fc, fp, slots, body_len)
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_ADD_F64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::F64(x + y))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_SUB_F64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::F64(x - y))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_MUL_F64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::F64(x * y))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_DIV_F64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        // IEEE division: a zero divisor produces inf, not a trap
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::F64(x / y))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_EQ_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        let v = x == y;
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Bool(v))
                            .map_err(|e| (e, pc))?;
                        if let Some(field) = fused_jmpif_not(code, idx, dst) {
                            fused_jmpif_not!(field, v);
                        } else {
                            pc.0 += 1;
                        }
                    },
                    OP_NEQ_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Bool(x != y),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_LT_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        let v = x < y;
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Bool(v))
                            .map_err(|e| (e, pc))?;
                        if let Some(field) = fused_jmpif_not(code, idx, dst) {
                            fused_jmpif_not!(field, v);
                        } else {
                            pc.0 += 1;
                        }
                    },
                    OP_GE_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Bool(x >= y),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_LE_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Bool(x <= y),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_GT_I64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Bool(x > y),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_EQ_F64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        // bit-structural equality: reflexive, total, dict-safe
                        let v = x.to_bits() == y.to_bits();
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Bool(v))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_NEQ_F64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        let v = x.to_bits() != y.to_bits();
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Bool(v))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_LT_F64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        // partial order: every NaN comparison answers false
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Bool(x < y),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_GE_F64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Bool(x >= y),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_LE_F64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Bool(x <= y),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_GT_F64 => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        let v = x > y;
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Bool(v))
                            .map_err(|e| (e, pc))?;
                        if let Some(field) = fused_jmpif_not(code, idx, dst) {
                            fused_jmpif_not!(field, v);
                        } else {
                            pc.0 += 1;
                        }
                    },
                    OP_EQ_BOOL => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_bool(&self.stack, fp, lhs, slots, body_len)
                                .map_err(|e| (e, pc))?,
                            read_bool(&self.stack, fp, rhs, slots, body_len)
                                .map_err(|e| (e, pc))?,
                        );
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Bool(x == y),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_NEQ_BOOL => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_bool(&self.stack, fp, lhs, slots, body_len)
                                .map_err(|e| (e, pc))?,
                            read_bool(&self.stack, fp, rhs, slots, body_len)
                                .map_err(|e| (e, pc))?,
                        );
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Bool(x != y),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    // the string data ops take operands of both string
                    // domains: a heap string word or an arena temporary
                    OP_EQ_STR => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let eq = str_eq_content(self, fp, lhs, rhs, slots, body_len, pc)?;
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Bool(eq))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_NEQ_STR => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let eq = str_eq_content(self, fp, lhs, rhs, slots, body_len, pc)?;
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Bool(!eq))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_STRCAT => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_typed(&self.stack, fp, lhs, slots, body_len, SlotType::Ref)
                                .map_err(|e| (e, pc))?,
                            read_typed(&self.stack, fp, rhs, slots, body_len, SlotType::Ref)
                                .map_err(|e| (e, pc))?,
                        );
                        // a heap-to-heap pair keeps the store's own
                        // fast path; any arena operand is a mixed-domain
                        // concat whose contents build the counted result
                        // directly on the live context store (the content
                        // borrows read pool and objects immutably, the
                        // alloc and the claim touch objects and claims
                        // alone, and the fields are disjoint)
                        let obj = if let (Value::Ref(xw), Value::Ref(yw)) = (x, y) {
                            self.heaps().concat(xw, yw).map_err(|e| (e, pc))?
                        } else {
                            let xb = str_content(
                                &self.pool,
                                &self.objects,
                                self.ctx_live,
                                self.current,
                                x,
                            )
                            .map_err(|e| (e, pc))?;
                            let yb = str_content(
                                &self.pool,
                                &self.objects,
                                self.ctx_live,
                                self.current,
                                y,
                            )
                            .map_err(|e| (e, pc))?;
                            let text = concat_text(xb, yb).map_err(|e| (e, pc))?;
                            let id = self.objects.alloc_str(text);
                            self.claims[self.current.index()].acquire(RefId::new(self.current, id));
                            id
                        };
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Ref(RefId::new(self.current, obj)),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    // the placement pass proved this result a designated
                    // temporary: its bytes serve from the running fiber's
                    // frame arena and its word carries no count, so no
                    // acquire and no claim row exist; the first
                    // allocation in a frame also serves that frame's
                    // entry mark (the return-side close restores it)
                    OP_STRCAT_ARENA => {
                        let dst = fa;
                        let lhs = fb;
                        let rhs = fc;
                        let (x, y) = (
                            read_typed(&self.stack, fp, lhs, slots, body_len, SlotType::Ref)
                                .map_err(|e| (e, pc))?,
                            read_typed(&self.stack, fp, rhs, slots, body_len, SlotType::Ref)
                                .map_err(|e| (e, pc))?,
                        );
                        let xb =
                            str_content(&self.pool, &self.objects, self.ctx_live, self.current, x)
                                .map_err(|e| (e, pc))?;
                        let yb =
                            str_content(&self.pool, &self.objects, self.ctx_live, self.current, y)
                                .map_err(|e| (e, pc))?;
                        let total = xb.len().checked_add(yb.len()).ok_or((
                            Trap::ArenaExhausted {
                                field: ArenaPackError::Length,
                            },
                            pc,
                        ))?;
                        if total >= ARENA_LEN_LIMIT {
                            return Err((
                                Trap::ArenaExhausted {
                                    field: ArenaPackError::Length,
                                },
                                pc,
                            ));
                        }
                        let Word::Meta(meta) = &mut self.stack[fp] else {
                            return Err((Trap::MetaWordCorrupt { fp }, pc));
                        };
                        if !meta.arena_active() {
                            meta.mark_arena_active();
                            // the mark precedes this frame's first
                            // allocation, so it is the frame's exact
                            // entry position
                            self.pool[self.current.index()].arena.push_frame_mark();
                        }
                        let at = self.pool[self.current.index()].arena.alloc_concat(xb, yb);
                        let handle = ArenaStr::pack(at.chunk(), at.offset(), total)
                            .map_err(|field| (Trap::ArenaExhausted { field }, pc))?;
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Arena(handle),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_CLOSURE_NEW => {
                        let dst = fa;
                        let func = fb;
                        let n = fc;
                        // CLOSURE_NEW: the header is the scalar FuncRef
                        // word at `func` (KCONST-minted), the capture block the
                        // n words staged at overlay positions 0..n. The object
                        // acquires one count per Ref capture as it adopts the
                        // block — the staged sources keep their own counts and
                        // die their ordinary deaths — and the fresh Ref lands
                        // in dst with birth count one. The acquire stays inside
                        // the op on purpose: the object's count must stay
                        // unreachable from frame-word dec noise (the fuzz
                        // mirror's structured-stop property pins it).
                        let funcref = match read_value(&self.stack, fp, func, slots, body_len)
                            .map_err(|e| (e, pc))?
                        {
                            Value::Fn(funcref) => funcref,
                            v => {
                                return Err((
                                    Trap::SlotTypeMismatch {
                                        slot: func,
                                        expected: SlotType::Fn,
                                        actual: v.slot_type(),
                                    },
                                    pc,
                                ))
                            },
                        };
                        let overlay_base = slots.len() - f.layout.arg_area_len();
                        let mut captures = Vec::with_capacity(usize::from(n));
                        for index in 0..usize::from(n) {
                            let slot = u8::try_from(overlay_base + index)
                                .expect("overlay words fit u8; frames cap at 256 slots");
                            captures.push(
                                read_value(&self.stack, fp, slot, slots, body_len)
                                    .map_err(|e| (e, pc))?,
                            );
                        }
                        for word in &captures {
                            if let Value::Ref(obj) = *word {
                                // the object adopts the block: one edge
                                // count per Ref capture, no frame claim
                                self.inc_edge(obj).map_err(|e| (e, pc))?;
                            }
                        }
                        let obj = self.objects.alloc_closure(funcref, captures);
                        // the fresh closure's birth count is the caller's
                        // frame claim
                        let closure_word = RefId::new(self.current, obj);
                        self.claims[self.current.index()].acquire(closure_word);
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Ref(closure_word),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_CAPTURE_STORE => {
                        let src = fa;
                        let obj_slot = fb;
                        let idx = fc;
                        // CAPTURE_STORE: the mut window over the
                        // running closure's own capture block. `obj_slot` holds
                        // the closure word CALLS wrote into this frame — the
                        // frame's borrowed copy, its count the caller still
                        // owns — and the incoming word at `src` replaces the
                        // block's word `idx`. The incoming Ref acquires one
                        // edge count for the object before the swap and the
                        // displaced word's releases after, both routed by
                        // owner, so a self-assignment over the same object
                        // cannot free the word it re-stores and the frame's
                        // own copies keep their counts.
                        let incoming = read_value(&self.stack, fp, src, slots, body_len)
                            .map_err(|e| (e, pc))?;
                        let closure = match read_value(&self.stack, fp, obj_slot, slots, body_len)
                            .map_err(|e| (e, pc))?
                        {
                            Value::Ref(word) => word,
                            v => {
                                return Err((
                                    Trap::SlotTypeMismatch {
                                        slot: obj_slot,
                                        expected: SlotType::Ref,
                                        actual: v.slot_type(),
                                    },
                                    pc,
                                ))
                            },
                        };
                        if let Value::Ref(word) = incoming {
                            self.heaps().inc_edge(word).map_err(|e| (e, pc))?;
                        }
                        let replaced = self
                            .heaps()
                            .replace_capture(closure, usize::from(idx), incoming)
                            .map_err(|e| (e, pc))?;
                        if let Value::Ref(word) = replaced {
                            self.dec_edge(word).map_err(|e| (e, pc))?;
                        }
                        pc.0 += 1;
                    },
                    OP_TAG_TEST => {
                        let dst = fa;
                        let src = fb;
                        let tag = fc;
                        let v =
                            read_tag(&self.stack, fp, src, slots, body_len).map_err(|e| (e, pc))?;
                        let held = v == tag;
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Bool(held))
                            .map_err(|e| (e, pc))?;
                        if let Some(field) = fused_jmpif(code, idx, dst) {
                            fused_jmpif!(field, held);
                        } else {
                            pc.0 += 1;
                        }
                    },
                    OP_TAG_TEST_NOT => {
                        let dst = fa;
                        let src = fb;
                        let tag = fc;
                        let v =
                            read_tag(&self.stack, fp, src, slots, body_len).map_err(|e| (e, pc))?;
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Bool(v != tag),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_TAG_NEW => {
                        if fc != 0 {
                            reserved!();
                        }
                        let dst = fa;
                        let tag = fb;
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Tag(tag))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_CVT_I2F => {
                        if fc != 0 {
                            reserved!();
                        }
                        let dst = fa;
                        let src = fb;
                        let x =
                            read_i64(&self.stack, fp, src, slots, body_len).map_err(|e| (e, pc))?;
                        // precision loss beyond 2^53 is the semantics of the
                        // i64-to-f64 conversion itself
                        #[allow(clippy::cast_precision_loss)]
                        let v = x as f64;
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::F64(v))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_CVT_F2I => {
                        if fc != 0 {
                            reserved!();
                        }
                        let dst = fa;
                        let src = fb;
                        let x =
                            read_f64(&self.stack, fp, src, slots, body_len).map_err(|e| (e, pc))?;
                        // trap on NaN and on magnitudes at or past 2^63; -2^63
                        // itself is exactly i64::MIN and converts
                        if x.is_nan() || x >= F64_I64_EDGE || x < -F64_I64_EDGE {
                            return Err((Trap::CvtOutOfRange, pc));
                        }
                        // in range by the check above, so the cast is exact
                        #[allow(clippy::cast_possible_truncation)]
                        let v = x as i64;
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::I64(v))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_MOV => {
                        if fc != 0 {
                            reserved!();
                        }
                        let dst = fa;
                        let src = fb;
                        let v = read_value(&self.stack, fp, src, slots, body_len)
                            .map_err(|e| (e, pc))?;
                        write_slot(&mut self.stack, fp, dst, slots, body_len, v)
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_JMPIF => {
                        let cond = fa;
                        let target = jump_target!();
                        if read_bool(&self.stack, fp, cond, slots, body_len).map_err(|e| (e, pc))? {
                            jump!(target);
                        } else {
                            pc.0 += 1;
                        }
                    },
                    OP_JMPIF_NOT => {
                        let cond = fa;
                        let target = jump_target!();
                        if read_bool(&self.stack, fp, cond, slots, body_len).map_err(|e| (e, pc))? {
                            pc.0 += 1;
                        } else {
                            jump!(target);
                        }
                    },
                    OP_JMP => {
                        if fa != 0 {
                            reserved!();
                        }
                        let target = jump_target!();
                        jump!(target);
                    },
                    // the loop-head mark: the paired restore at the back
                    // edge rewinds here, so each iteration re-marks before
                    // the cond and body allocate
                    OP_ARENA_MARK => {
                        if fb != 0 || fc != 0 {
                            reserved!();
                        }
                        let dst = fa;
                        let at = self.pool[self.current.index()].arena.mark();
                        let packed = ArenaStr::pack(at.chunk(), at.offset(), 0)
                            .map_err(|field| (Trap::ArenaExhausted { field }, pc))?;
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::I64(packed.raw() as i64),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_ARENA_RESTORE => {
                        if fb != 0 || fc != 0 {
                            reserved!();
                        }
                        let src = fa;
                        let cursor =
                            read_i64(&self.stack, fp, src, slots, body_len).map_err(|e| (e, pc))?;
                        let raw = u64::try_from(cursor).map_err(|_| {
                            (
                                Trap::ArenaHandle {
                                    class: ds_rt::ArenaMisuse::Range,
                                },
                                pc,
                            )
                        })?;
                        let (chunk, offset, _) = ArenaStr::unpack_raw(raw);
                        self.pool[self.current.index()]
                            .arena
                            .try_restore(ds_rt::Mark::at(chunk, offset))
                            .map_err(|class| (Trap::ArenaHandle { class }, pc))?;
                        pc.0 += 1;
                    },
                    OP_CALL => {
                        let callee = ConstIdx(fd);
                        let nargs = fa;
                        let caller = CallerAt {
                            func: std::ptr::from_ref(f) as usize,
                            row: active.version,
                        };
                        // the site's one probe: a uop hit pushes the frame
                        // straight off the entry (no plan, no raw list, no
                        // table row read), a native hit hands the shared
                        // table across, and every other shape (an unfilled
                        // slot, a bound callee, a site staging `Raw` words,
                        // a refusal riding the entry) falls to the general
                        // resolution below
                        let resolved = match call_hit(
                            self.call_cache[pc.0 as usize & (CALL_CACHE_SLOTS - 1)].as_ref(),
                            caller.func,
                            pc.0,
                            self.registry.generation(),
                            &mut self.stack,
                            &mut self.callinfo,
                            self.limit,
                            &mut self.pool[self.current.index()].max_len,
                            fp,
                            body_len,
                        ) {
                            CallHit::Pushed(t) => {
                                active = Active {
                                    version: t.version,
                                    func: t.func,
                                };
                                pc = CodePc(0);
                                continue 'transfer;
                            },
                            CallHit::Native { table, index } => Resolved::Native { table, index },
                            CallHit::Miss => self
                                .resolve_site(pool, caller, &f.layout, pc.0, callee, nargs)
                                .map_err(|e| (e, pc))?,
                        };
                        match resolved {
                            Resolved::Uop {
                                version,
                                func,
                                facts,
                            } => {
                                let mut st = stack_state!();
                                let t = st
                                    .call(version, func, &facts, &f.layout, fp, pc.0 + 1)
                                    .map_err(|e| (e, pc))?;
                                active = Active {
                                    version: t.version,
                                    func: t.func,
                                };
                                pc = CodePc(0);
                                continue 'transfer;
                            },
                            // A native call stays in the caller's frame: the
                            // staged words are read as arguments and the result
                            // overlays argument slot 0 exactly where a RET
                            // would put it (a fallible native's span overlays
                            // slots 0 and 1). The fiber-control natives are the
                            // exception: the dispatcher itself is the switch —
                            // their bodies never run from a CALL site.
                            Resolved::Native { table, index } => {
                                let nf = &table[index as usize];
                                check_native_call_site(&f.layout, nf).map_err(|e| (e, pc))?;
                                if let Some(control) = nf.control {
                                    match self
                                        .control_switch(
                                            control, &active, pc.0, fp, &f.layout, nargs,
                                        )
                                        .map_err(|e| (e, pc))?
                                    {
                                        SwitchOutcome::Entered(a, at) => {
                                            active = a;
                                            pc = at;
                                            continue 'transfer;
                                        },
                                        SwitchOutcome::Bounced => {
                                            pc.0 += 1;
                                        },
                                        SwitchOutcome::Host(event) => return Ok(event),
                                        SwitchOutcome::Interlock(req) => {
                                            return Ok(DriveOutcome::Interlock(req))
                                        },
                                    }
                                } else if let Some(channel) = nf.channel {
                                    match self
                                        .chan_switch(channel, &active, pc.0, fp, &f.layout, nargs)
                                        .map_err(|e| (e, pc))?
                                    {
                                        ChanOutcome::Answered => {
                                            pc.0 += 1;
                                        },
                                        ChanOutcome::Parked(event) => return Ok(event),
                                    }
                                } else if let Some(timer) = nf.timer {
                                    // the timer race: the staged callable
                                    // becomes the racer; a live deadline
                                    // parks the caller, a past one answers
                                    // the failure span in place
                                    match self
                                        .timeout_switch(
                                            timer, &active, pc.0, fp, &f.layout, boundary,
                                        )
                                        .map_err(|e| (e, pc))?
                                    {
                                        TimerOutcome::Answered => {
                                            pc.0 += 1;
                                        },
                                        TimerOutcome::Parked(event) => return Ok(event),
                                    }
                                } else {
                                    let mut heaps = Heaps::new(
                                        self.current,
                                        &mut self.objects,
                                        &mut self.pool,
                                        &mut self.claims,
                                        self.ctx_live,
                                    );
                                    let result = invoke_native(
                                        nf,
                                        usize::from(nargs),
                                        &mut self.stack,
                                        &mut heaps,
                                        &mut self.out,
                                        &f.layout,
                                        fp,
                                    )
                                    .map_err(|e| (e, pc))?;
                                    // a world op's would-block parks the
                                    // calling fiber instead of blocking
                                    // the drive — the resume point is
                                    // the instruction after the call
                                    let result = match result {
                                        NativeResult::Park(wait) => {
                                            return self.park_world(
                                                &active,
                                                pc,
                                                ParkKind::World(wait),
                                            )
                                        },
                                        other => other,
                                    };
                                    // the close seams' wake door: a native
                                    // that closed a handle wakes the parked
                                    // waits on it now
                                    self.wake_closed();
                                    overlay_native(result, &mut self.stack, &f.layout, fp)
                                        .map_err(|e| (e, pc))?;
                                    pc.0 += 1;
                                }
                            },
                            // A bound artifact's C symbol runs in the
                            // caller's frame like a native: the staged
                            // words marshal per their own kinds and the
                            // scalar answer overlays argument slot 0. The
                            // call is blocking and synchronous — it pins the
                            // fiber for its whole duration, and a foreign
                            // abort kills the process (panic=abort, the VM
                            // never unwinding through a foreign frame).
                            Resolved::Bound { module, index, ret } => {
                                let value = invoke_bound(
                                    &self.pool,
                                    &self.objects,
                                    self.ctx_live,
                                    self.current,
                                    &module,
                                    index,
                                    ret,
                                    usize::from(nargs),
                                    &mut self.stack,
                                    &f.layout,
                                    fp,
                                )
                                .map_err(|e| (e, pc))?;
                                overlay_native(
                                    NativeResult::Word(value),
                                    &mut self.stack,
                                    &f.layout,
                                    fp,
                                )
                                .map_err(|e| (e, pc))?;
                                pc.0 += 1;
                            },
                        }
                    },
                    OP_CALLT => {
                        let callee = ConstIdx(fd);
                        let nargs = fa;
                        let caller = CallerAt {
                            func: std::ptr::from_ref(f) as usize,
                            row: active.version,
                        };
                        let resolved = self
                            .resolve_site(pool, caller, &f.layout, pc.0, callee, nargs)
                            .map_err(|e| (e, pc))?;
                        let result = match resolved {
                            Resolved::Uop {
                                version,
                                func,
                                facts,
                            } => {
                                self.arena_frame_close(fp);
                                let mut st = stack_state!();
                                let t = st
                                    .tail_call(version, func, &facts, &f.layout, fp)
                                    .map_err(|e| (e, pc))?;
                                active = Active {
                                    version: t.version,
                                    func: t.func,
                                };
                                pc = CodePc(0);
                                continue 'transfer;
                            },
                            // Tail-calling a native runs it, then returns its
                            // result from the current frame: the value the
                            // native produced is the tail position's value.
                            // A span result rides the reused frame back to
                            // the ORIGINAL caller: this frame's slot 0
                            // already aliases the caller's overlay word 0
                            // (the RET below writes the tag there), and
                            // overlay word 1 is the physical word fp+2. At
                            // the root frame only the head word survives,
                            // the same root rule RETSPAN returns by.
                            Resolved::Native { table, index } => {
                                let nf = &table[index as usize];
                                if let Some(caller_area) = self.caller_arg_area() {
                                    check_native_overlay(caller_area, nf).map_err(|e| (e, pc))?;
                                }
                                if let Some(channel) = nf.channel {
                                    // the channel step runs in place of
                                    // the body; a park pops the frame
                                    // exactly the net tail park would
                                    match self
                                        .chan_step(channel, fp, &f.layout, nargs)
                                        .map_err(|e| (e, pc))?
                                    {
                                        ChanStep::Answer(result) => result,
                                        ChanStep::Park(wait) => {
                                            let kind = wait.map_or(ParkKind::Chan, ParkKind::World);
                                            return self.park_world_tail(&active, pc, boundary, kind)
                                        },
                                    }
                                } else {
                                    let mut heaps = Heaps::new(
                                        self.current,
                                        &mut self.objects,
                                        &mut self.pool,
                                        &mut self.claims,
                                        self.ctx_live,
                                    );
                                    invoke_native(
                                        nf,
                                        usize::from(nargs),
                                        &mut self.stack,
                                        &mut heaps,
                                        &mut self.out,
                                        &f.layout,
                                        fp,
                                    )
                                    .map_err(|e| (e, pc))?
                                }
                            },
                            // A bound artifact's C symbol tail-calls in
                            // place: the blocking call pins the fiber
                            // for its whole duration, and the one scalar
                            // word answers exactly a native word result's
                            // tail ride
                            Resolved::Bound { module, index, ret } => {
                                if let Some(caller_area) = self.caller_arg_area() {
                                    check_bound_overlay(caller_area).map_err(|e| (e, pc))?;
                                }
                                NativeResult::Word(
                                    invoke_bound(
                                        &self.pool,
                                        &self.objects,
                                        self.ctx_live,
                                        self.current,
                                        &module,
                                        index,
                                        ret,
                                        usize::from(nargs),
                                        &mut self.stack,
                                        &f.layout,
                                        fp,
                                    )
                                    .map_err(|e| (e, pc))?,
                                )
                            },
                        };
                        // a world op's would-block parks the calling
                        // fiber instead of blocking the drive — the
                        // frame pops as the ret below would, the wake's
                        // answer delivering at the pending return
                        let result = match result {
                            NativeResult::Park(wait) => {
                                return self.park_world_tail(
                                    &active,
                                    pc,
                                    boundary,
                                    ParkKind::World(wait),
                                )
                            },
                            other => other,
                        };
                        // the close seams' wake door: a native
                        // that closed a handle wakes the parked waits on
                        // it now — the tail call's own return flows on
                        self.wake_closed();
                        // the answer's whole word span rides a
                        // boundary return (the hosted seam's result
                        // channel); every other outcome keeps its
                        // prior words
                        let (value, answer) = match result {
                            NativeResult::Word(value) => (value, FrameAnswer::one(value)),
                            // a plain table native never parks (the
                            // generic fork bodies live behind the
                            // CALLG dispatch, and this arm never
                            // reaches them)
                            NativeResult::Fork { .. } => {
                                unreachable!("a plain native never parks")
                            },
                            // intercepted above: the park returned
                            // before this match ever ran
                            NativeResult::Park(_) => {
                                unreachable!("a world park returned before the match")
                            },
                            NativeResult::Tag { tag, payload } => {
                                if self.callinfo.len() > 1 {
                                    self.stack[fp + 2] = Word::Slot(payload);
                                }
                                (Value::Tag(tag), FrameAnswer::tagged(tag, &[payload]))
                            },
                            // a span channel's tail ride: the payload
                            // words land on the original caller's
                            // overlay past the head (physical words
                            // fp+2..), bounds-checked against that
                            // caller's area; at the root only the
                            // head word survives, the root rule
                            // RETSPAN returns by
                            NativeResult::Span { tag, payload } => {
                                if self.callinfo.len() > 1 {
                                    let caller = &self.callinfo[self.callinfo.len() - 2];
                                    let caller_fn =
                                        &self.registry.pinned(caller.module).functions()
                                            [caller.func as usize];
                                    let area = caller_fn.layout.arg_area_len();
                                    if 1 + payload.len() > area {
                                        return Err((
                                            Trap::RetSpanTooWide {
                                                span: u16::try_from(1 + payload.len())
                                                    .expect("span widths fit u16"),
                                                caller_area: u16::try_from(area)
                                                    .expect("argument areas fit u16"),
                                            },
                                            pc,
                                        ));
                                    }
                                    for (index, word) in payload.iter().enumerate() {
                                        self.stack[fp + 2 + index] = Word::Slot(*word);
                                    }
                                }
                                (Value::Tag(tag), FrameAnswer::tagged(tag, &payload))
                            },
                            // the tag-less total's tail ride: no
                            // discriminant, so word 0 rides the RET
                            // and the rest land on the original
                            // caller's overlay words 1.. (physical
                            // words fp+2..), bounds-checked against
                            // that caller's area; at the root only
                            // word 0 survives
                            NativeResult::Total { words } => {
                                if self.callinfo.len() > 1 {
                                    let caller = &self.callinfo[self.callinfo.len() - 2];
                                    let caller_fn =
                                        &self.registry.pinned(caller.module).functions()
                                            [caller.func as usize];
                                    let area = caller_fn.layout.arg_area_len();
                                    if words.len() > area {
                                        return Err((
                                            Trap::RetSpanTooWide {
                                                span: u16::try_from(words.len())
                                                    .expect("span widths fit u16"),
                                                caller_area: u16::try_from(area)
                                                    .expect("argument areas fit u16"),
                                            },
                                            pc,
                                        ));
                                    }
                                    for (index, word) in words.iter().enumerate().skip(1) {
                                        self.stack[fp + 1 + index] = Word::Slot(*word);
                                    }
                                }
                                let lead = words
                                    .first()
                                    .copied()
                                    .expect("a tag-less total carries a word");
                                (lead, FrameAnswer::many(words))
                            },
                        };
                        self.arena_frame_close(fp);
                        let mut st = stack_state!();
                        match st.ret(value, fp, boundary).map_err(|e| (e, pc))? {
                            RetOutcome::Root(_) => {
                                // the native's whole span answer rides the
                                // completion channel to a live resumer and
                                // the drive's death value whole
                                match self.complete_current_span(&answer) {
                                    Some((a, at)) => {
                                        active = a;
                                        pc = at;
                                        continue 'transfer;
                                    },
                                    None => return Ok(DriveOutcome::Died(answer)),
                                }
                            },
                            RetOutcome::Boundary(_) => return Ok(DriveOutcome::Frame(answer)),
                            RetOutcome::Resume {
                                version,
                                func,
                                pc: at,
                            } => {
                                active = Active { version, func };
                                pc = at;
                                continue 'transfer;
                            },
                        }
                    },
                    OP_CALLS => {
                        if fc != 0 {
                            reserved!();
                        }
                        let callee_slot = fb;
                        let nargs = fa;
                        // CALLS, the callable-word call form: the callee
                        // operand is a frame slot, the ISA's one dynamic-dispatch
                        // point — the callee word's identity is runtime data by
                        // construction, so the read dispatches on the word's
                        // kind: a scalar FuncRef names the function directly, a
                        // Ref word names a closure object whose header is the
                        // FuncRef and whose capture block rides along. The
                        // slot's static classification stays the boundary: a
                        // Fn slot is callable-defaulted (either word may
                        // arrive), a Ref slot holds a value-site closure.
                        // read_value bounds-checks the slot operand first.
                        let word = read_value(&self.stack, fp, callee_slot, slots, body_len)
                            .map_err(|e| (e, pc))?;
                        if !matches!(
                            slots[usize::from(callee_slot)],
                            SlotType::Fn | SlotType::Ref
                        ) {
                            return Err((
                                Trap::SlotTypeMismatch {
                                    slot: callee_slot,
                                    expected: SlotType::Fn,
                                    actual: slots[usize::from(callee_slot)],
                                },
                                pc,
                            ));
                        }
                        let (funcref, captures, closure_id) = match word {
                            Value::Fn(funcref) => (funcref, None, None),
                            Value::Ref(obj) => {
                                let (header, block) = stage_closure(
                                    &mut self.objects,
                                    &mut self.pool,
                                    self.current,
                                    &mut self.calls_staging,
                                    obj,
                                )
                                .map_err(|e| (e, pc))?;
                                (header, Some(block), Some(obj))
                            },
                            v => {
                                return Err((
                                    Trap::SlotTypeMismatch {
                                        slot: callee_slot,
                                        expected: SlotType::Fn,
                                        actual: v.slot_type(),
                                    },
                                    pc,
                                ))
                            },
                        };
                        let (version, func_id) = Self::resolve_word(
                            &self.registry,
                            &mut self.word_cache,
                            funcref,
                            None,
                            nargs,
                        )
                        .map_err(|e| (e, pc))?;
                        let target = &self.registry.pinned(version).functions()[func_id as usize];
                        // a closure's capture block lands in the callee's frame
                        // starting at the first local slot (after the params),
                        // so the body reads a capture where a lowered lambda
                        // declares it. Each Ref capture acquires one count for
                        // the frame at the copy-in — the callee owns its copy
                        // like a borrow parameter and balances it with its own
                        // exit drops, while the closure object keeps its own
                        // count untouched. The range check runs before the
                        // frame is pushed; each write re-checks the word
                        // against the slot's declared type.
                        let capture_body_len = target.layout.body_len();
                        if let Some(block) = captures {
                            // the range check counts the block plus the
                            // reserved closure word right past it (the
                            // mutation window's address of the object)
                            let end = target.layout.params_len() + block.len() + 1;
                            if end > capture_body_len {
                                return Err((
                                    Trap::SlotOutOfRange {
                                        slot: u8::try_from(end - 1).unwrap_or(u8::MAX),
                                        frame_slots: target.layout.slots().len(),
                                    },
                                    pc,
                                ));
                            }
                        }
                        // the frame setup mirrors CALL: slide-plan validation,
                        // the raw-staged check, the callee's metadata word in
                        // the caller's call word, the args overlaid. No tail
                        // variant exists, so this is an ordinary push. The
                        // callee is runtime data (the staged word named it), so
                        // the facts derive here, once per call. The resolved
                        // handle moves through the call into the transfer
                        // (one mint per call, CALL's own discipline), and the
                        // capture copy-in re-derives its layout borrow from
                        // the transfer's handle.
                        let facts = SiteFacts::derive(&f.layout, &target.layout);
                        let t = {
                            let mut st = stack_state!();
                            st.call(version, func_id, &facts, &f.layout, fp, pc.0 + 1)
                                .map_err(|e| (e, pc))?
                        };
                        if let Some(block) = captures {
                            // the callee's frame base: fp + 1 + the caller's
                            // body span (the pinned physical mapping)
                            let callee_fp = fp + 1 + f.layout.body_len();
                            let target =
                                &self.registry.pinned(t.version).functions()[t.func as usize];
                            let capture_owner = self.current;
                            for (index, &value) in block.iter().enumerate() {
                                if let Value::Ref(word) = value {
                                    Self::inc_ref_split(
                                        &mut self.objects,
                                        &mut self.pool,
                                        &mut self.claims,
                                        capture_owner,
                                        self.ctx_live,
                                        capture_owner,
                                        word,
                                    )
                                    .map_err(|e| (e, pc))?;
                                }
                                let slot = u8::try_from(target.layout.params_len() + index)
                                    .expect("capture slot fits u8 after the range check");
                                write_slot(
                                    &mut self.stack,
                                    callee_fp,
                                    slot,
                                    target.layout.slots(),
                                    capture_body_len,
                                    value,
                                )
                                .map_err(|e| (e, pc))?;
                            }
                            // the callee's closure word lands right past the
                            // capture block (the mutation window's one
                            // address of the running object): the lowerer
                            // reserves a Ref-classified slot there, and the
                            // word needs no claim — the caller's frame keeps
                            // the count it handed the staged callee
                            let closure_slot =
                                u8::try_from(target.layout.params_len() + block.len())
                                    .expect("the closure word slot fits u8 after the range check");
                            let closure_word =
                                closure_id.expect("a block rides only a closure callee");
                            write_slot(
                                &mut self.stack,
                                callee_fp,
                                closure_slot,
                                target.layout.slots(),
                                capture_body_len,
                                Value::Ref(closure_word),
                            )
                            .map_err(|e| (e, pc))?;
                        }
                        active = Active {
                            version: t.version,
                            func: t.func,
                        };
                        pc = CodePc(0);
                        continue 'transfer;
                    },
                    OP_CALLG => {
                        let callee = ConstIdx(fd);
                        let nargs = fa;
                        // A generic call site: the callee is a generic
                        // builtin's ONE body, and the call's evidence words sit
                        // in the overlay after the staged arguments — their
                        // count travels with the callee, so the uop carries
                        // CALL's operand layout. The dispatch reads the
                        // evidence words and selects the body's behavior on the
                        // resolved types; monomorphic instances do not exist.
                        // Install proved the operand's pool index in range and
                        // its kind FuncRef (module.rs's body validation); the
                        // fetch indexes directly and the kind arm is the
                        // corrupted-pool tripwire.
                        let found = &pool[usize::from(callee.0)];
                        let Const::FuncRef(callee) = found else {
                            return Err((
                                Trap::PoolKind {
                                    kidx: callee.0,
                                    expected: ConstKind::FuncRef,
                                    found: found.kind(),
                                },
                                pc,
                            ));
                        };
                        // A runtime-resolved trait dispatch inside a
                        // generic impl body: the sentinel FuncRef
                        // names the calling frame's evidence slot, and
                        // the word staged there by the site that entered
                        // the body names an evidence-chain object — the
                        // dispatch resolves its serving body and stages
                        // the tail objects' indices as the body's trailing
                        // evidence parameters, then transfers exactly like
                        // a CALL's uop callee.
                        if let ConstFuncRef::Named { module, func } = callee
                            && module == EVIDENCE_MODULE
                        {
                            let slot = u8::try_from(*func).map_err(|_| {
                                (
                                    Trap::UnknownCallee {
                                        module: module.clone(),
                                        func: *func,
                                    },
                                    pc,
                                )
                            })?;
                            let version = active.version;
                            let func_id = evidence_chain_dispatch(
                                self, version, slot, slots, body_len, fp, &f.layout, nargs, pc,
                            )?;
                            let target =
                                &self.registry.pinned(version).functions()[func_id as usize];
                            debug_assert_eq!(target.entry.tier1_prologue(), func_id as usize);
                            // the serving body is runtime data (the frame's
                            // evidence word named it), so the facts derive
                            // here, once per call
                            let facts = SiteFacts::derive(&f.layout, &target.layout);
                            let mut st = stack_state!();
                            let t = st
                                .call(version, func_id, &facts, &f.layout, fp, pc.0 + 1)
                                .map_err(|e| (e, pc))?;
                            active = Active {
                                version: t.version,
                                func: t.func,
                            };
                            pc = CodePc(0);
                            continue 'transfer;
                        }
                        // A user trait-method dispatch site: the
                        // FuncRef names the impl body the lowering resolved
                        // from the call's statically solved instance. The
                        // resolution runs through the per-site inline cache
                        // (the same bank and key CALL/CALLT use, full
                        // lookup as the fallback), so a hot dispatch site
                        // skips the module lookup. The
                        // staged words past `nargs` carry the runtime
                        // evidence a generic impl body claims as its
                        // trailing parameters (one per the body's nested
                        // demands, validated as I64 words), then the
                        // solved declared-instance id riding past them —
                        // validated here, carried rather than
                        // re-dispatched (the instance-keyed cache story
                        // keys off this word later). The body transfers
                        // exactly like a CALL's uop callee: the carried
                        // word sits past the callee's parameters, so the
                        // callee's frame never claims it.
                        let caller_site = std::ptr::from_ref(f) as usize;
                        let resolved = self
                            .resolve_dispatch_site(
                                callee,
                                caller_site,
                                active.version,
                                &f.layout,
                                pc.0,
                                nargs,
                            )
                            .map_err(|e| (e, pc))?;
                        if let DispatchResolved::Uop {
                            version,
                            func: func_id,
                            facts,
                        } = resolved
                        {
                            let target =
                                &self.registry.pinned(version).functions()[func_id as usize];
                            let params_len = target.layout.params_len();
                            if usize::from(nargs) > params_len {
                                return Err((
                                    Trap::ArityMismatch {
                                        expected: params_len,
                                        given: usize::from(nargs),
                                    },
                                    pc,
                                ));
                            }
                            // the staged evidence parameters the body
                            // claims, then the solved instance word
                            let ev_count = params_len - usize::from(nargs);
                            let ev_body_len = f.layout.body_len();
                            for i in 0..=ev_count {
                                let ev_slot = u8::try_from(
                                    f.layout.arg_area().start + usize::from(nargs) + i,
                                )
                                .expect("slot index fits u8");
                                let word = read_value(&self.stack, fp, ev_slot, slots, ev_body_len)
                                    .map_err(|e| (e, pc))?;
                                if word.slot_type() != SlotType::I64 {
                                    return Err((
                                        Trap::SlotTypeMismatch {
                                            slot: ev_slot,
                                            expected: SlotType::I64,
                                            actual: word.slot_type(),
                                        },
                                        pc,
                                    ));
                                }
                            }
                            let mut st = stack_state!();
                            let t = st
                                .call(version, func_id, &facts, &f.layout, fp, pc.0 + 1)
                                .map_err(|e| (e, pc))?;
                            active = Active {
                                version: t.version,
                                func: t.func,
                            };
                            pc = CodePc(0);
                            continue 'transfer;
                        }
                        // the generic builtins are native-by-name by
                        // construction: an id-spelled callee reaches
                        // NotUop only through a corrupted pool
                        let (module, func) = match callee {
                            ConstFuncRef::Named { module, func } => (module.as_str(), *func),
                            ConstFuncRef::Uop { module, func } => {
                                let name = self
                                    .registry
                                    .module_name(*module)
                                    .unwrap_or("an uninstalled dependency");
                                return Err((
                                    Trap::UnknownCallee {
                                        module: name.to_owned(),
                                        func: *func,
                                    },
                                    pc,
                                ));
                            },
                            // a bound C symbol never rides the generic
                            // dispatch
                            ConstFuncRef::Bound { module, index, .. } => {
                                return Err((
                                    Trap::UnknownCallee {
                                        module: module.clone(),
                                        func: *index,
                                    },
                                    pc,
                                ));
                            },
                        };
                        let g = crate::native::resolve_generic(module, func).ok_or((
                            Trap::UnknownCallee {
                                module: module.to_owned(),
                                func,
                            },
                            pc,
                        ))?;
                        // a fork-join combinator parks its call site; a
                        // teardown-hook drive must return to its driver,
                        // so the fork refuses before its spawn phase runs
                        // (a parked hook would strand the pass)
                        if self.teardown_drive && crate::native::is_fork_join(module, func) {
                            return Err((Trap::ReentrancyRefused, pc));
                        }
                        let needed = generic_staged_check(g, nargs).map_err(|e| (e, pc))?;
                        // the evidence words share the overlay with the staged
                        // arguments, so the area must hold both plus the result
                        let area = f.layout.arg_area_len();
                        if needed > area {
                            return Err((
                                Trap::CallLayout(SlideError::StagingTooSmall {
                                    available: area,
                                    needed,
                                }),
                                pc,
                            ));
                        }
                        if usize::from(g.result_words) > area {
                            return Err((
                                Trap::RetSpanTooWide {
                                    span: u16::from(g.result_words),
                                    caller_area: u16::try_from(area)
                                        .expect("argument areas fit u16"),
                                },
                                pc,
                            ));
                        }
                        let body_len = f.layout.body_len();
                        let start = f.layout.arg_area().start;
                        // the staged arguments and evidence words copy inline
                        // (generic call sites sit well under the inline width),
                        // with the heap buffer the oversized fallback
                        let mut inline = [Value::I64(0); INLINE_WORDS];
                        let mut heap = Vec::new();
                        let args: &[Value] = if usize::from(nargs) <= INLINE_WORDS {
                            for (i, word) in inline.iter_mut().enumerate().take(usize::from(nargs))
                            {
                                let slot = u8::try_from(start + i).expect("slot index fits u8");
                                *word = read_value(&self.stack, fp, slot, slots, body_len)
                                    .map_err(|e| (e, pc))?;
                            }
                            &inline[..usize::from(nargs)]
                        } else {
                            heap.reserve(usize::from(nargs));
                            for i in 0..usize::from(nargs) {
                                let slot = u8::try_from(start + i).expect("slot index fits u8");
                                heap.push(
                                    read_value(&self.stack, fp, slot, slots, body_len)
                                        .map_err(|e| (e, pc))?,
                                );
                            }
                            &heap
                        };
                        generic_kind_check(g, args).map_err(|e| (e, pc))?;
                        let mut ev_inline = [Value::I64(0); INLINE_WORDS];
                        let mut ev_heap = Vec::new();
                        let evidence: &[Value] = if usize::from(g.evidence) <= INLINE_WORDS {
                            for (i, word) in ev_inline
                                .iter_mut()
                                .enumerate()
                                .take(usize::from(g.evidence))
                            {
                                let slot = u8::try_from(start + usize::from(nargs) + i)
                                    .expect("slot fits u8");
                                *word = read_value(&self.stack, fp, slot, slots, body_len)
                                    .map_err(|e| (e, pc))?;
                            }
                            &ev_inline[..usize::from(g.evidence)]
                        } else {
                            ev_heap.reserve(usize::from(g.evidence));
                            for i in 0..usize::from(g.evidence) {
                                let slot = u8::try_from(start + usize::from(nargs) + i)
                                    .expect("slot fits u8");
                                ev_heap.push(
                                    read_value(&self.stack, fp, slot, slots, body_len)
                                        .map_err(|e| (e, pc))?,
                                );
                            }
                            &ev_heap
                        };
                        // the body takes the whole VM: the pure bodies
                        // split off the heap and sink themselves, the
                        // lambda-taking combinators reenter the
                        // dispatcher through `call_hosted`, and the
                        // fork-join combinators park — the spawn phase
                        // ran inside the body, and the join runs on
                        // the interlock stack, the drive delivering the
                        // answer onto this call site
                        let result = (g.func)(self, evidence, args).map_err(|e| (e, pc))?;
                        if let NativeResult::Fork { nursery, stage } = result {
                            return self.fork_park(&active, pc, area, nursery, stage);
                        }
                        overlay_native(result, &mut self.stack, &f.layout, fp)
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    OP_CALLTG => {
                        let callee = ConstIdx(fd);
                        let nargs = fa;
                        // CALLTG, the dispatch tail call: the dispatch
                        // resolution is CALLG's own
                        // — the sentinel evidence FuncRef, a trait site's
                        // impl body, or a generic builtin's ONE body — and
                        // the transfer is CALLT's: the staged words slide
                        // into the reused frame's parameters per the ABI
                        // slide plan, the metadata word rewrites in place,
                        // and the stack never grows at any depth. The
                        // evidence/instance words past the callee's
                        // parameters ride the transfer as count-free dead
                        // words the callee never claims, so the slide moves
                        // exactly the parameter count. Install proved the
                        // operand's pool index in range and its kind FuncRef
                        // (module.rs's body validation); the fetch indexes
                        // directly and the kind arm is the corrupted-pool
                        // tripwire.
                        let found = &pool[usize::from(callee.0)];
                        let Const::FuncRef(callee) = found else {
                            return Err((
                                Trap::PoolKind {
                                    kidx: callee.0,
                                    expected: ConstKind::FuncRef,
                                    found: found.kind(),
                                },
                                pc,
                            ));
                        };
                        // the sentinel names the frame's evidence slot: the
                        // target is runtime data the entering site staged,
                        // so this path reads the slot every time and never
                        // touches the cache — the chain object stages the
                        // riders exactly like the CALLG sentinel path above,
                        // before the frame-reuse slide moves them
                        if let ConstFuncRef::Named { module, func } = callee
                            && module == EVIDENCE_MODULE
                        {
                            let slot = u8::try_from(*func).map_err(|_| {
                                (
                                    Trap::UnknownCallee {
                                        module: module.clone(),
                                        func: *func,
                                    },
                                    pc,
                                )
                            })?;
                            let version = active.version;
                            let func_id = evidence_chain_dispatch(
                                self, version, slot, slots, body_len, fp, &f.layout, nargs, pc,
                            )?;
                            let target =
                                &self.registry.pinned(version).functions()[func_id as usize];
                            debug_assert_eq!(target.entry.tier1_prologue(), func_id as usize);
                            // the serving body is runtime data (the frame's
                            // evidence word named it), so the facts derive
                            // here, once per call
                            let facts = SiteFacts::derive(&f.layout, &target.layout);
                            self.arena_frame_close(fp);
                            let mut st = stack_state!();
                            let t = st
                                .tail_call(version, func_id, &facts, &f.layout, fp)
                                .map_err(|e| (e, pc))?;
                            active = Active {
                                version: t.version,
                                func: t.func,
                            };
                            pc = CodePc(0);
                            continue 'transfer;
                        }
                        // a trait site's impl body: the per-site inline
                        // cache keys the callee (full lookup as the
                        // fallback), the staged evidence words validate as
                        // I64 before the slide reads them, and the transfer
                        // reuses this frame
                        let caller_site = std::ptr::from_ref(f) as usize;
                        let resolved = self
                            .resolve_dispatch_site(
                                callee,
                                caller_site,
                                active.version,
                                &f.layout,
                                pc.0,
                                nargs,
                            )
                            .map_err(|e| (e, pc))?;
                        if let DispatchResolved::Uop {
                            version,
                            func: func_id,
                            facts,
                        } = resolved
                        {
                            let params_len = self.registry.pinned(version).functions()
                                [func_id as usize]
                                .layout
                                .params_len();
                            // the staged evidence parameters the body
                            // claims, then the solved instance word — both
                            // count-free I64 constants riding past the
                            // slide's width
                            let ev_count = params_len - usize::from(nargs);
                            let ev_body_len = f.layout.body_len();
                            for i in 0..=ev_count {
                                let ev_slot = u8::try_from(
                                    f.layout.arg_area().start + usize::from(nargs) + i,
                                )
                                .expect("slot index fits u8");
                                let word = read_value(&self.stack, fp, ev_slot, slots, ev_body_len)
                                    .map_err(|e| (e, pc))?;
                                if word.slot_type() != SlotType::I64 {
                                    return Err((
                                        Trap::SlotTypeMismatch {
                                            slot: ev_slot,
                                            expected: SlotType::I64,
                                            actual: word.slot_type(),
                                        },
                                        pc,
                                    ));
                                }
                            }
                            self.arena_frame_close(fp);
                            let mut st = stack_state!();
                            let t = st
                                .tail_call(version, func_id, &facts, &f.layout, fp)
                                .map_err(|e| (e, pc))?;
                            active = Active {
                                version: t.version,
                                func: t.func,
                            };
                            pc = CodePc(0);
                            continue 'transfer;
                        }
                        // a generic builtin's ONE body runs inside the
                        // instruction and the frame returns its result —
                        // the CALLT-to-native discipline: the span result
                        // lands on the ORIGINAL caller's overlay words, and
                        // at the root frame only the head word survives.
                        // A fork-join combinator cannot run here: the
                        // join parks its site through the drive, and the
                        // tail call would return this frame at the call —
                        // the fork needs the CALLG site
                        // the generic builtins are native-by-name by
                        // construction: an id-spelled callee reaches
                        // NotUop only through a corrupted pool
                        let (module, func) = match callee {
                            ConstFuncRef::Named { module, func } => (module.as_str(), *func),
                            ConstFuncRef::Uop { module, func } => {
                                let name = self
                                    .registry
                                    .module_name(*module)
                                    .unwrap_or("an uninstalled dependency");
                                return Err((
                                    Trap::UnknownCallee {
                                        module: name.to_owned(),
                                        func: *func,
                                    },
                                    pc,
                                ));
                            },
                            // a bound C symbol never rides the generic
                            // dispatch
                            ConstFuncRef::Bound { module, index, .. } => {
                                return Err((
                                    Trap::UnknownCallee {
                                        module: module.clone(),
                                        func: *index,
                                    },
                                    pc,
                                ));
                            },
                        };
                        let g = crate::native::resolve_generic(module, func).ok_or((
                            Trap::UnknownCallee {
                                module: module.to_owned(),
                                func,
                            },
                            pc,
                        ))?;
                        if crate::native::is_fork_join(module, func) {
                            return Err((Trap::TailForkJoin, pc));
                        }
                        let needed = generic_staged_check(g, nargs).map_err(|e| (e, pc))?;
                        let area = f.layout.arg_area_len();
                        if needed > area {
                            return Err((
                                Trap::CallLayout(SlideError::StagingTooSmall {
                                    available: area,
                                    needed,
                                }),
                                pc,
                            ));
                        }
                        if let Some(caller_area) = self.caller_arg_area()
                            && usize::from(g.result_words) > caller_area
                        {
                            return Err((
                                Trap::RetSpanTooWide {
                                    span: u16::from(g.result_words),
                                    caller_area: u16::try_from(caller_area)
                                        .expect("argument areas fit u16"),
                                },
                                pc,
                            ));
                        }
                        let start = f.layout.arg_area().start;
                        let mut inline = [Value::I64(0); INLINE_WORDS];
                        let mut heap = Vec::new();
                        let args: &[Value] = if usize::from(nargs) <= INLINE_WORDS {
                            for (i, word) in inline.iter_mut().enumerate().take(usize::from(nargs))
                            {
                                let slot = u8::try_from(start + i).expect("slot index fits u8");
                                *word = read_value(&self.stack, fp, slot, slots, body_len)
                                    .map_err(|e| (e, pc))?;
                            }
                            &inline[..usize::from(nargs)]
                        } else {
                            heap.reserve(usize::from(nargs));
                            for i in 0..usize::from(nargs) {
                                let slot = u8::try_from(start + i).expect("slot index fits u8");
                                heap.push(
                                    read_value(&self.stack, fp, slot, slots, body_len)
                                        .map_err(|e| (e, pc))?,
                                );
                            }
                            &heap
                        };
                        generic_kind_check(g, args).map_err(|e| (e, pc))?;
                        let mut ev_inline = [Value::I64(0); INLINE_WORDS];
                        let mut ev_heap = Vec::new();
                        let evidence: &[Value] = if usize::from(g.evidence) <= INLINE_WORDS {
                            for (i, word) in ev_inline
                                .iter_mut()
                                .enumerate()
                                .take(usize::from(g.evidence))
                            {
                                let slot = u8::try_from(start + usize::from(nargs) + i)
                                    .expect("slot fits u8");
                                *word = read_value(&self.stack, fp, slot, slots, body_len)
                                    .map_err(|e| (e, pc))?;
                            }
                            &ev_inline[..usize::from(g.evidence)]
                        } else {
                            ev_heap.reserve(usize::from(g.evidence));
                            for i in 0..usize::from(g.evidence) {
                                let slot = u8::try_from(start + usize::from(nargs) + i)
                                    .expect("slot fits u8");
                                ev_heap.push(
                                    read_value(&self.stack, fp, slot, slots, body_len)
                                        .map_err(|e| (e, pc))?,
                                );
                            }
                            &ev_heap
                        };
                        let result = (g.func)(self, evidence, args).map_err(|e| (e, pc))?;
                        // the answer's whole word span rides a boundary
                        // return (the hosted seam's result channel); every
                        // other outcome keeps its prior words
                        let (value, answer) = match result {
                            NativeResult::Word(value) => (value, FrameAnswer::one(value)),
                            // the tail site refuses the fork before its
                            // body runs (the is_fork_join check above)
                            NativeResult::Fork { .. } => {
                                unreachable!("the tail site refuses a fork request first")
                            },
                            // the generic builtins are pure: a world op
                            // lives behind the platform table's plain
                            // call sites, never the generic dispatch
                            NativeResult::Park(_) => {
                                unreachable!("a generic builtin never parks")
                            },
                            NativeResult::Tag { tag, payload } => {
                                if self.callinfo.len() > 1 {
                                    self.stack[fp + 2] = Word::Slot(payload);
                                }
                                (Value::Tag(tag), FrameAnswer::tagged(tag, &[payload]))
                            },
                            // the span channel's tail ride, the CALLT
                            // native shape's: the payload words land on the
                            // original caller's overlay past the head,
                            // bounds-checked against that caller's area
                            // (the root keeps only the head word)
                            NativeResult::Span { tag, payload } => {
                                if self.callinfo.len() > 1 {
                                    let caller = &self.callinfo[self.callinfo.len() - 2];
                                    let caller_fn =
                                        &self.registry.pinned(caller.module).functions()
                                            [caller.func as usize];
                                    let area = caller_fn.layout.arg_area_len();
                                    if 1 + payload.len() > area {
                                        return Err((
                                            Trap::RetSpanTooWide {
                                                span: u16::try_from(1 + payload.len())
                                                    .expect("span widths fit u16"),
                                                caller_area: u16::try_from(area)
                                                    .expect("argument areas fit u16"),
                                            },
                                            pc,
                                        ));
                                    }
                                    for (index, word) in payload.iter().enumerate() {
                                        self.stack[fp + 2 + index] = Word::Slot(*word);
                                    }
                                }
                                (Value::Tag(tag), FrameAnswer::tagged(tag, &payload))
                            },
                            // the tag-less total's tail ride, the CALLT
                            // native shape's with no discriminant: word 0
                            // rides the RET, the rest land on the original
                            // caller's overlay words 1.., bounds-checked
                            // against that caller's area
                            NativeResult::Total { words } => {
                                if self.callinfo.len() > 1 {
                                    let caller = &self.callinfo[self.callinfo.len() - 2];
                                    let caller_fn =
                                        &self.registry.pinned(caller.module).functions()
                                            [caller.func as usize];
                                    let area = caller_fn.layout.arg_area_len();
                                    if words.len() > area {
                                        return Err((
                                            Trap::RetSpanTooWide {
                                                span: u16::try_from(words.len())
                                                    .expect("span widths fit u16"),
                                                caller_area: u16::try_from(area)
                                                    .expect("argument areas fit u16"),
                                            },
                                            pc,
                                        ));
                                    }
                                    for (index, word) in words.iter().enumerate().skip(1) {
                                        self.stack[fp + 1 + index] = Word::Slot(*word);
                                    }
                                }
                                let lead = words
                                    .first()
                                    .copied()
                                    .expect("a tag-less total carries a word");
                                (lead, FrameAnswer::many(words))
                            },
                        };
                        self.arena_frame_close(fp);
                        let mut st = stack_state!();
                        match st.ret(value, fp, boundary).map_err(|e| (e, pc))? {
                            RetOutcome::Root(_) => {
                                // the entry frame answers with its whole span:
                                // it rides the completion channel (a RETSPAN
                                // at the root, or a tail native's span/total)
                                // to a live resumer, and the drive's death
                                // value carries it whole
                                match self.complete_current_span(&answer) {
                                    Some((a, at)) => {
                                        active = a;
                                        pc = at;
                                        continue 'transfer;
                                    },
                                    None => return Ok(DriveOutcome::Died(answer)),
                                }
                            },
                            RetOutcome::Boundary(_) => return Ok(DriveOutcome::Frame(answer)),
                            RetOutcome::Resume {
                                version,
                                func,
                                pc: at,
                            } => {
                                active = Active { version, func };
                                pc = at;
                                continue 'transfer;
                            },
                        }
                    },
                    OP_RET => {
                        if fd != 0 {
                            reserved!();
                        }
                        let src = fa;
                        // the root frame and the nested drive's reentry
                        // frame answer through the general path (the drive's
                        // Root/Boundary outcomes), so the fast return
                        // engages only past them; the chain position is a
                        // pure pre-filter — the sentinel test and every
                        // other gate stay with [`ret_hit`] and the general
                        // path, which re-derive them from scratch
                        let chain_len = self.callinfo.len();
                        let at_drive_edge =
                            chain_len < 2 || (boundary != usize::MAX && chain_len == boundary);
                        if !at_drive_edge
                            && let RetHit::Resume {
                                version,
                                func,
                                pc: at,
                            } = ret_hit(
                                &mut self.stack,
                                &mut self.callinfo,
                                &self.registry,
                                &mut self.pool[self.current.index()].arena,
                                fp,
                                boundary,
                                slots,
                                body_len,
                                src,
                            )
                        {
                            active = Active { version, func };
                            pc = at;
                            continue 'transfer;
                        }
                        let value = read_value(&self.stack, fp, src, slots, body_len)
                            .map_err(|e| (e, pc))?;
                        self.arena_frame_close(fp);
                        let mut st = stack_state!();
                        match st.ret(value, fp, boundary).map_err(|e| (e, pc))? {
                            RetOutcome::Root(v) => match self.complete_current(v) {
                                Some((a, at)) => {
                                    active = a;
                                    pc = at;
                                    continue 'transfer;
                                },
                                None => return Ok(DriveOutcome::Died(FrameAnswer::one(v))),
                            },
                            RetOutcome::Boundary(v) => {
                                return Ok(DriveOutcome::Frame(FrameAnswer::one(v)))
                            },
                            RetOutcome::Resume {
                                version,
                                func,
                                pc: at,
                            } => {
                                active = Active { version, func };
                                pc = at;
                                continue 'transfer;
                            },
                        }
                    },
                    OP_RET_SPAN => {
                        if fb != 0 || fc == 0 {
                            reserved!();
                        }
                        let src = fa;
                        let n = fc;
                        // the span must fit inside this frame's own slots (a
                        // load-time fact, re-checked defensively)
                        if usize::from(src) + usize::from(n) > frame_slots {
                            return Err((
                                Trap::SlotOutOfRange {
                                    slot: src,
                                    frame_slots,
                                },
                                pc,
                            ));
                        }
                        // the caller's overlay area must be wide enough to
                        // receive the whole span (cross-function, so runtime)
                        // — except at the hosted seam's reentry frame, where
                        // the span rides the drive's outcome instead of any
                        // caller's overlay (the sentinel return pc marks it)
                        let at_reentry = boundary != usize::MAX
                            && self.callinfo.len() == boundary
                            && matches!(self.stack[fp], Word::Meta(m) if m.return_pc.0 == u32::MAX);
                        if self.callinfo.len() > 1 && !at_reentry {
                            let caller = &self.callinfo[self.callinfo.len() - 2];
                            let caller_fn = &self.registry.pinned(caller.module).functions()
                                [caller.func as usize];
                            let area = caller_fn.layout.arg_area_len();
                            if usize::from(n) > area {
                                return Err((
                                    Trap::RetSpanTooWide {
                                        span: u16::from(n),
                                        caller_area: u16::try_from(area)
                                            .expect("argument areas fit u16"),
                                    },
                                    pc,
                                ));
                            }
                        }
                        // the span's answer rides the boundary return whole
                        // and lands at overlay words 0..n — the caller's
                        // outgoing-argument words, since the callee's own slots
                        // alias them. The words are read whole before any is
                        // placed, so the callee's slots and the caller's
                        // overlay words never overlap, and a narrow span
                        // (every lowered lambda's answer) allocates nothing.
                        let answer = FrameAnswer::staged(usize::from(n), |i| {
                            let slot = u8::try_from(usize::from(src) + i)
                                .expect("span stays inside the u8 slot range");
                            read_value(&self.stack, fp, slot, slots, body_len)
                        })
                        .map_err(|e| (e, pc))?;
                        for i in 0..usize::from(n) {
                            self.stack[fp + 1 + i] = Word::Slot(answer.as_slice()[i]);
                        }
                        let value = match self.stack[fp + 1] {
                            Word::Slot(v) => v,
                            Word::Meta(_) => {
                                return Err((Trap::MetaWordCorrupt { fp }, pc));
                            },
                        };
                        self.arena_frame_close(fp);
                        let mut st = stack_state!();
                        match st.ret(value, fp, boundary).map_err(|e| (e, pc))? {
                            RetOutcome::Root(_) => {
                                // the entry frame answers with its whole span:
                                // it rides the completion channel (a RETSPAN
                                // at the root, or a tail native's span/total)
                                // to a live resumer, and the drive's death
                                // value carries it whole
                                match self.complete_current_span(&answer) {
                                    Some((a, at)) => {
                                        active = a;
                                        pc = at;
                                        continue 'transfer;
                                    },
                                    None => return Ok(DriveOutcome::Died(answer)),
                                }
                            },
                            RetOutcome::Boundary(_) => return Ok(DriveOutcome::Frame(answer)),
                            RetOutcome::Resume {
                                version,
                                func,
                                pc: at,
                            } => {
                                active = Active { version, func };
                                pc = at;
                                continue 'transfer;
                            },
                        }
                    },
                    OP_HALT => {
                        if fd != 0 {
                            reserved!();
                        }
                        let slot = fa;
                        let value = read_value(&self.stack, fp, slot, slots, body_len)
                            .map_err(|e| (e, pc))?;
                        // HALT ends the fiber; under a nested reentrant drive
                        // that fiber carries the Rust-side native loop's
                        // frames — refuse instead of dying mid-native (the
                        // frames stay for backtrace)
                        if self.reentry_live(boundary) {
                            return Err((Trap::ReentrancyRefused, pc));
                        }
                        match self.complete_current(value) {
                            Some((a, at)) => {
                                active = a;
                                pc = at;
                                continue 'transfer;
                            },
                            None => return Ok(DriveOutcome::Died(FrameAnswer::one(value))),
                        }
                    },
                    // Major 0xE: the placed RC pair. Load decode-checks
                    // every word once, so a canonicity violation here is like
                    // any other; each arm moves one counted heap block's
                    // count per word.
                    OP_RC_INC => {
                        if fb != 0 || fc != 0 {
                            reserved!();
                        }
                        if let Some(word) = read_counted(&self.stack, fp, fa, slots, body_len)
                            .map_err(|e| (e, pc))?
                        {
                            self.inc_ref(word).map_err(|e| (e, pc))?;
                        }
                        pc.0 += 1;
                    },
                    OP_RC_DEC => {
                        if fb != 0 || fc != 0 {
                            reserved!();
                        }
                        if let Some(word) = read_counted(&self.stack, fp, fa, slots, body_len)
                            .map_err(|e| (e, pc))?
                        {
                            self.dec_ref(word).map_err(|e| (e, pc))?;
                        }
                        pc.0 += 1;
                    },
                    _ => {
                        // every canonical byte is served by an arm above,
                        // so anything here is a canonicity violation and the
                        // decoders remain the error authority for its exact
                        // cause
                        let cause = if byte >> 4 == RC_MAJOR {
                            match RcOp::decode(word) {
                                Err(RcDecodeError::UnknownOpcode { major, minor }) => {
                                    DecodeError::UnknownOpcode { major, minor }
                                },
                                Err(RcDecodeError::ReservedField) => DecodeError::ReservedField,
                                Ok(_) => unreachable!("every canonical RC byte is a fast arm"),
                            }
                        } else {
                            DecodeError::UnknownOpcode {
                                major: byte >> 4,
                                minor: byte & 0x0f,
                            }
                        };
                        return Err((Trap::BadEncoding { pc: pc.0, cause }, pc));
                    },
                }
            }
        }
    }
}
