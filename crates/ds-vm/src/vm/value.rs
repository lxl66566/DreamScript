//! The slot-overlay vocabulary: reading and writing a frame's words by slot,
//! the text and integer helpers, and the fused-opcode peek helpers.

use super::*;

/// The outcome of [`ret_hit`]: the frame stands popped and the caller
/// resumes, or the return takes the general path.
pub(super) enum RetHit {
    Resume {
        version: VersionIdx,
        func: u32,
        pc: CodePc,
    },
    General,
}

/// The outcome of [`call_hit`]: the callee's frame stands pushed, the
/// site's cached native hands its shared table across, or the site is no
/// cache hit and the caller takes the general path.
pub(super) enum CallHit {
    Pushed(Transfer),
    Native {
        table: Arc<Vec<NativeFunction>>,
        index: u32,
    },
    Miss,
}

/// CALL's single cache probe: on a uop hit whose site stages no `Raw`
/// word, the callee's metadata word lands in the caller's call word and
/// the chain gains the callee's entry — exactly the general path's push
/// ([`StackState::call`]) with the plan, the raw list, and the version
/// table row all read straight off the cache entry instead of re-derived.
/// A native hit answers the shared table and index the general path's
/// native seam takes, so both cached kinds pay one probe per call. Every
/// refusal the general path raises (a layout-static refusal riding the
/// entry, a past-limit frame) and every other entry shape answer
/// [`CallHit::Miss`] before any word moves, so the general path then runs
/// untouched and raises the same trap. The three key words are the
/// caller's identity, the pc, and the registry's install generation — a
/// hit is a resolution no install has undone.
#[inline]
#[allow(clippy::too_many_arguments)]
pub(super) fn call_hit(
    site: Option<&CallSite>,
    caller: usize,
    pc: u32,
    stamp: u64,
    stack: &mut Segment,
    callinfo: &mut Vec<CallInfo>,
    limit: usize,
    max_len: &mut usize,
    fp: usize,
    body_len: usize,
) -> CallHit {
    let Some(CallSite {
        caller: at,
        pc: at_pc,
        stamp: at_stamp,
        callee,
    }) = site
    else {
        return CallHit::Miss;
    };
    if *at != caller || *at_pc != pc || *at_stamp != stamp {
        return CallHit::Miss;
    }
    match callee {
        CachedCallee::Uop {
            version,
            func,
            facts:
                SiteFacts::Ready {
                    raw: None,
                    frame_size,
                    ..
                },
        } => {
            // the callee's metadata word lands in the caller's call word:
            // fp + 1 + body_len (the pinned physical mapping)
            let callee_fp = fp + 1 + body_len;
            let end = callee_fp + *frame_size as usize;
            if end > limit {
                return CallHit::Miss;
            }
            if end > stack.len() {
                stack.grow_to(end);
                *max_len = (*max_len).max(end);
            }
            stack[callee_fp] = Word::Meta(FrameMetadata {
                return_pc: CodePc(pc + 1),
                frame_size: *frame_size,
                tail: TailChain::default(),
            });
            callinfo.push(CallInfo {
                fp: callee_fp,
                module: *version,
                func: *func,
            });
            CallHit::Pushed(Transfer {
                version: *version,
                func: *func,
            })
        },
        CachedCallee::Native { table, index } => CallHit::Native {
            table: Arc::clone(table),
            index: *index,
        },
        // a bound callee and a uop entry staging `Raw` words (or carrying
        // a refusal) keep the general path's own handling
        CachedCallee::Bound { .. } | CachedCallee::Uop { .. } => CallHit::Miss,
    }
}

/// RET's common return: the result word overlays slot 0, the caller's
/// frame span restores, and the caller resumes at the metadata word's
/// return pc — [`StackState::ret`]'s `Resume` outcome with the frame's
/// arena close folded in. A slot read that fails, a non-metadata word at
/// the frame base, the root frame, a nested drive's reentry frame, a
/// return pc past the caller's code, and a chain entry naming no row all
/// answer [`RetHit::General`] before any word moves, so the general path
/// then runs untouched and raises the same trap.
#[inline]
#[allow(clippy::too_many_arguments)]
pub(super) fn ret_hit(
    stack: &mut Segment,
    callinfo: &mut Vec<CallInfo>,
    versions: &ModuleRegistry,
    arena: &mut ds_rt::Arena,
    fp: usize,
    boundary: usize,
    slots: &[SlotType],
    body_len: usize,
    src: u8,
) -> RetHit {
    if usize::from(src) >= slots.len() {
        return RetHit::General;
    }
    let Word::Slot(value) = stack[word_of(fp, usize::from(src), body_len)] else {
        return RetHit::General;
    };
    let Word::Meta(meta) = stack[fp] else {
        return RetHit::General;
    };
    if callinfo.len() < 2
        || (boundary != usize::MAX && callinfo.len() == boundary && meta.return_pc.0 == u32::MAX)
    {
        return RetHit::General;
    }
    // the caller's entry answers the bounds checks (its version's code
    // length, its layout's frame span); the row is Copy, so no borrow
    // outlives the chain move
    let caller = &callinfo[callinfo.len() - 2];
    let (caller_fp, caller_module, caller_func) = (caller.fp, caller.module, caller.func);
    let Some(caller_def) = versions
        .pinned(caller_module)
        .functions()
        .get(caller_func as usize)
    else {
        return RetHit::General;
    };
    let return_pc = meta.return_pc;
    if return_pc.0 as usize >= caller_def.code.len() {
        return RetHit::General;
    }
    stack[fp + 1] = Word::Slot(value);
    if meta.arena_active() {
        arena.pop_frame_mark();
        let Word::Meta(meta) = &mut stack[fp] else {
            unreachable!("the frame base held a metadata word");
        };
        meta.clear_arena_active();
    }
    let restore = (caller_fp + caller_def.layout.frame_size() as usize).max(fp + 2);
    stack.truncate(restore);
    callinfo.pop();
    RetHit::Resume {
        version: caller_module,
        func: caller_func,
        pc: return_pc,
    }
}

pub(super) fn read_value(
    stack: &Segment,
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
) -> Result<Value, Trap> {
    let i = usize::from(slot);
    if i >= slots.len() {
        return Err(Trap::SlotOutOfRange {
            slot,
            frame_slots: slots.len(),
        });
    }
    let at = word_of(fp, i, body_len);
    debug_assert!(at < stack.len(), "slot inside the frame span");
    match stack[at] {
        Word::Slot(v) => Ok(v),
        Word::Meta(_) => Err(Trap::MetaWordCorrupt { fp }),
    }
}

/// Resolves one runtime-evidence sentinel dispatch over the
/// module's evidence-chain table: the frame's slot word names a chain
/// object whose `func` is the serving body and whose `next` tail list
/// answers that body's own evidence demands — one chain word per demand,
/// staged onto the caller's overlay past the real arguments exactly
/// where the static trait path's constant stores land. One static table
/// read per hop, so chain depth is table-bounded, never frame-bounded.
/// Answers the target's function-table index.
#[allow(clippy::too_many_arguments)]
pub(super) fn evidence_chain_dispatch(
    vm: &mut Vm,
    version: VersionIdx,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
    fp: usize,
    caller: &FrameLayout,
    nargs: u8,
    pc: CodePc,
) -> Result<u32, (Trap, CodePc)> {
    let word = read_value(&vm.stack, fp, slot, slots, body_len).map_err(|e| (e, pc))?;
    let Value::I64(chain_idx) = word else {
        return Err((
            Trap::SlotTypeMismatch {
                slot,
                expected: SlotType::I64,
                actual: word.slot_type(),
            },
            pc,
        ));
    };
    // the dead sentinel: the entering site staged no chain for a demand
    // whose variable its branch never mentions, so the code reading it
    // ran under a narrowing that branch cannot satisfy — a checker gap,
    // never a silent wrong call
    if chain_idx < 0 {
        return Err((Trap::DeadEvidenceChain { slot }, pc));
    }
    let module = vm.registry.pinned(version);
    let chains = module.chains();
    let index = usize::try_from(chain_idx)
        .ok()
        .filter(|index| *index < chains.len());
    let Some(index) = index else {
        return Err((
            Trap::UnknownCallee {
                module: EVIDENCE_MODULE.to_owned(),
                func: chain_idx.try_into().unwrap_or(u32::MAX),
            },
            pc,
        ));
    };
    let chain = &chains[index];
    let func_id = chain.func;
    let target = &module.functions()[func_id as usize];
    let params_len = target.layout.params_len();
    let riders = chain.next.len();
    // the exact sentinel-arity contract: the chain's tail list was built
    // one entry per the serving body's own evidence demands, so the
    // parameter count must be the staged real-argument count plus it — a
    // gap names a corrupted table (a one-word receiver staged against a
    // multi-word `self`), never a silent slide of a
    // rider into the parameter span
    if usize::from(nargs) + riders != params_len {
        return Err((
            Trap::ArityMismatch {
                expected: params_len,
                given: usize::from(nargs),
            },
            pc,
        ));
    }
    // the riders share the overlay with the staged arguments; the
    // lowering reserves the widest serving body's demand count, so an
    // escape names hand-assembled code
    if usize::from(nargs) + riders > caller.arg_area_len() {
        return Err((
            Trap::CallLayout(SlideError::StagingTooSmall {
                available: caller.arg_area_len(),
                needed: usize::from(nargs) + riders,
            }),
            pc,
        ));
    }
    let caller_body_len = caller.body_len();
    for (i, tail) in chain.next.iter().enumerate() {
        let slot = u8::try_from(caller.arg_area().start + usize::from(nargs) + i)
            .expect("slot index fits u8");
        let at = word_of(fp, usize::from(slot), caller_body_len);
        vm.stack[at] = Word::Slot(Value::I64(i64::from(*tail)));
    }
    Ok(func_id)
}

/// Reads a slot typed `want`; the value-type check doubles as the check of
/// the static layout (writes enforce layout and value agreement).
pub(super) fn read_typed(
    stack: &Segment,
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
    want: SlotType,
) -> Result<Value, Trap> {
    let v = read_value(stack, fp, slot, slots, body_len)?;
    if v.slot_type() != want {
        return Err(Trap::SlotTypeMismatch {
            slot,
            expected: want,
            actual: v.slot_type(),
        });
    }
    Ok(v)
}

/// The frame word one slot read addresses, with its static bounds: the
/// shared front of every specialized slot reader (the typed check is the
/// caller's match on the word itself).
#[inline]
pub(super) fn slot_word<'a>(
    stack: &'a Segment,
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
) -> Result<&'a Word, Trap> {
    let i = usize::from(slot);
    if i >= slots.len() {
        return Err(Trap::SlotOutOfRange {
            slot,
            frame_slots: slots.len(),
        });
    }
    let at = word_of(fp, i, body_len);
    debug_assert!(at < stack.len(), "slot inside the frame span");
    Ok(&stack[at])
}

pub(super) fn read_i64(
    stack: &Segment,
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
) -> Result<i64, Trap> {
    match slot_word(stack, fp, slot, slots, body_len)? {
        Word::Slot(Value::I64(v)) => Ok(*v),
        Word::Slot(v) => Err(Trap::SlotTypeMismatch {
            slot,
            expected: SlotType::I64,
            actual: v.slot_type(),
        }),
        Word::Meta(_) => Err(Trap::MetaWordCorrupt { fp }),
    }
}

pub(super) fn read_f64(
    stack: &Segment,
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
) -> Result<f64, Trap> {
    match slot_word(stack, fp, slot, slots, body_len)? {
        Word::Slot(Value::F64(v)) => Ok(*v),
        Word::Slot(v) => Err(Trap::SlotTypeMismatch {
            slot,
            expected: SlotType::F64,
            actual: v.slot_type(),
        }),
        Word::Meta(_) => Err(Trap::MetaWordCorrupt { fp }),
    }
}

pub(super) fn read_bool(
    stack: &Segment,
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
) -> Result<bool, Trap> {
    match slot_word(stack, fp, slot, slots, body_len)? {
        Word::Slot(Value::Bool(v)) => Ok(*v),
        Word::Slot(v) => Err(Trap::SlotTypeMismatch {
            slot,
            expected: SlotType::Bool,
            actual: v.slot_type(),
        }),
        Word::Meta(_) => Err(Trap::MetaWordCorrupt { fp }),
    }
}

pub(super) fn read_tag(
    stack: &Segment,
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
) -> Result<u8, Trap> {
    match slot_word(stack, fp, slot, slots, body_len)? {
        Word::Slot(Value::Tag(v)) => Ok(*v),
        Word::Slot(v) => Err(Trap::SlotTypeMismatch {
            slot,
            expected: SlotType::Tag,
            actual: v.slot_type(),
        }),
        Word::Meta(_) => Err(Trap::MetaWordCorrupt { fp }),
    }
}

pub(super) fn read_ref(
    stack: &Segment,
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
) -> Result<RefId, Trap> {
    match slot_word(stack, fp, slot, slots, body_len)? {
        Word::Slot(Value::Ref(word)) => Ok(*word),
        // an arena word in a counted read is a broken proof or
        // hand-assembled code: the design keeps designated temporaries
        // away from every count funnel, and this trap is the tripwire
        Word::Slot(Value::Arena(_)) => Err(Trap::ArenaHandoff { slot }),
        Word::Slot(v) => Err(Trap::SlotTypeMismatch {
            slot,
            expected: SlotType::Ref,
            actual: v.slot_type(),
        }),
        Word::Meta(_) => Err(Trap::MetaWordCorrupt { fp }),
    }
}

/// The reference one RC op moves, when the word holds one: a `Raw` slot's
/// kind is a runtime fact (a type-variable value), so its inc/dec apply
/// only when the word is a reference at runtime and no-op on scalars;
/// every other slot classification keeps the strict typed read.
pub(super) fn read_counted(
    stack: &Segment,
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
) -> Result<Option<RefId>, Trap> {
    if slots[usize::from(slot)] == SlotType::Raw {
        return match read_value(stack, fp, slot, slots, body_len)? {
            Value::Ref(word) => Ok(Some(word)),
            Value::Arena(_) => Err(Trap::ArenaHandoff { slot }),
            _ => Ok(None),
        };
    }
    read_ref(stack, fp, slot, slots, body_len).map(Some)
}

/// The string content behind one operand of the string data ops: a heap
/// string word routed to its own store, or an arena handle read from the
/// running fiber's arena. The operands arrive already slot-checked
/// (`read_typed` against `SlotType::Ref`), so the remaining work is
/// content routing. The direct field borrows (never a `&self` method)
/// keep the returned slices alive beside the arena's interior-mutable
/// allocation in the dispatcher's arms.
pub(super) fn str_content<'a>(
    pool: &'a [Fiber],
    objects: &'a Objects,
    ctx_live: bool,
    current: FiberId,
    word: Value,
) -> Result<&'a [u8], Trap> {
    match word {
        Value::Ref(id) => {
            let store = if ctx_live && id.fiber() == current {
                objects
            } else {
                &pool[id.fiber().index()].objects
            };
            Ok(store.str_at(id.id())?.as_bytes())
        },
        Value::Arena(handle) => {
            let (chunk, offset, len) = handle.unpack();
            let bytes = pool[current.index()]
                .arena
                .bytes_at(ds_rt::Mark::at(chunk, offset), len)
                .map_err(|class| Trap::ArenaHandle { class })?;
            // SAFETY: the claim ledger is the aliasing discipline for arena
            // words: the running frame granted these bytes and no restore
            // reclaims them before the instruction reading them completes.
            Ok(unsafe { &*bytes })
        },
        _ => unreachable!("slot kind agrees with the variant"),
    }
}

/// Reads and compares both operands' string contents of an EQSTR/NEQSTR:
/// structural byte equality across both string domains.
pub(super) fn str_eq_content(
    vm: &mut Vm,
    fp: usize,
    lhs: u8,
    rhs: u8,
    slots: &[SlotType],
    body_len: usize,
    pc: CodePc,
) -> Result<bool, (Trap, CodePc)> {
    let (x, y) = (
        read_typed(&vm.stack, fp, lhs, slots, body_len, SlotType::Ref).map_err(|e| (e, pc))?,
        read_typed(&vm.stack, fp, rhs, slots, body_len, SlotType::Ref).map_err(|e| (e, pc))?,
    );
    // interning makes equal string constants one object, and an arena word
    // names one slice: an equal word pair holds equal content
    if x == y {
        return Ok(true);
    }
    let xb = str_content(&vm.pool, &vm.objects, vm.ctx_live, vm.current, x).map_err(|e| (e, pc))?;
    let yb = str_content(&vm.pool, &vm.objects, vm.ctx_live, vm.current, y).map_err(|e| (e, pc))?;
    Ok(xb == yb)
}

/// Joins two operand contents into one owned UTF-8 string. Every byte
/// range reaches here from a heap or pool `str` or from an arena slice the
/// concat op wrote, so the join is one validation total: a range a
/// hand-assembled or corrupted operand names stays a named refusal, and
/// since the join copies every byte of both operands one pass over the
/// result answers for both.
pub(super) fn concat_text(xb: &[u8], yb: &[u8]) -> Result<String, Trap> {
    let mut joined = Vec::with_capacity(xb.len() + yb.len());
    joined.extend_from_slice(xb);
    joined.extend_from_slice(yb);
    String::from_utf8(joined).map_err(|_| Trap::Utf8Content)
}

/// Writes a value into a slot, checking it against the slot's static type:
/// hand-assembled code that mis-stages a value traps here.
pub(super) fn write_slot(
    stack: &mut Segment,
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
    value: Value,
) -> Result<(), Trap> {
    let i = usize::from(slot);
    if i >= slots.len() {
        return Err(Trap::SlotOutOfRange {
            slot,
            frame_slots: slots.len(),
        });
    }
    // a Fn slot is callable-defaulted: a closure object's Ref word
    // may inhabit it at every read (the entry check, the generic
    // dispatch, the callable-word call), so the write admits the same
    // pairing — a bodied wrapper's pass-through stages its callable
    // parameter through a Fn-typed temp, and a capturing argument
    // arrives as a Ref. The pairing stays one-directional: a Ref slot
    // (a value site) never takes a scalar Fn word.
    let actual = value.slot_type();
    if slots[i] != actual
        && slots[i] != SlotType::Raw
        && !(slots[i] == SlotType::Fn && actual == SlotType::Ref)
    {
        return Err(Trap::SlotTypeMismatch {
            slot,
            expected: slots[i],
            actual,
        });
    }
    let at = word_of(fp, i, body_len);
    debug_assert!(at < stack.len(), "slot inside the frame span");
    stack[at] = Word::Slot(value);
    Ok(())
}

/// DIVI64: zero divide and `MIN / -1` are distinct traps.
// the ok_or_else form keeps the overflow value off the success path;
// the eager ok_or form regresses the arith kernels' instruction count
#[allow(clippy::unnecessary_lazy_evaluations)]
pub(super) fn int_div(x: i64, y: i64) -> Result<i64, Trap> {
    if y == 0 {
        return Err(Trap::DivideByZero);
    }
    x.checked_div(y).ok_or_else(|| Trap::IntegerOverflow)
}

/// MODI64: same trap split as [`int_div`].
#[allow(clippy::unnecessary_lazy_evaluations)]
pub(super) fn int_mod(x: i64, y: i64) -> Result<i64, Trap> {
    if y == 0 {
        return Err(Trap::DivideByZero);
    }
    x.checked_rem(y).ok_or_else(|| Trap::IntegerOverflow)
}

/// SHLI64/SHRI64/BITAND/BITXOR/BITORI64: the shift amounts trap
/// outside the 0..64 window (no wrap form exists, so both carry a trap
/// edge like the trapping arithmetic), `>>` is the arithmetic
/// (sign-propagating) shift, and the bitwise trio is total. The dispatch
/// reaches this through its one range arm over the five opcodes.
pub(super) fn shift_bitwise_word(
    stack: &mut Segment,
    op: u8,
    dst: u8,
    lhs: u8,
    rhs: u8,
    fp: usize,
    slots: &[SlotType],
    body_len: usize,
) -> Result<(), Trap> {
    let (x, y) = (
        read_i64(stack, fp, lhs, slots, body_len)?,
        read_i64(stack, fp, rhs, slots, body_len)?,
    );
    let value = match op {
        OP_SHL_I64 => {
            let Some(shift) = u32::try_from(y).ok().filter(|&y| y < 64) else {
                return Err(Trap::OversizedShift);
            };
            Value::I64(x << shift)
        },
        OP_SHR_I64 => {
            let Some(shift) = u32::try_from(y).ok().filter(|&y| y < 64) else {
                return Err(Trap::OversizedShift);
            };
            // Rust's i64 `>>` is arithmetic: the sign bit propagates
            Value::I64(x >> shift)
        },
        OP_BITAND_I64 => Value::I64(x & y),
        OP_BITXOR_I64 => Value::I64(x ^ y),
        OP_BITOR_I64 => Value::I64(x | y),
        _ => unreachable!("the range arm partitions the quintet"),
    };
    write_slot(stack, fp, dst, slots, body_len, value)
}

// The peek-next load-op fusion:
// a constant load whose immediately following word is one of the measured
// census arith pairs consuming the loaded slot runs both uops inside the
// load's dispatch arm — one dispatch for two uops, pc += 2. The uop
// stream and the module format stay byte-identical (no new uops; the peek
// only reads a word the loader already validated). Three invariants hold
// by construction: the fused arith's traps and slot errors attribute to
// its own pc (pc+1), so a trap is indistinguishable from the unfused run;
// an intervening RC word fails the opcode-byte match (the census adjacency
// rule), so a counted word between the pair never fuses; and a load at
// the code end peeks nothing (`get` on the out-of-bounds index). DIVI64
// stays unfused: the census never measured the pair.

/// The fused pair shape for a KIMM at `idx`: the next word's opcode byte
/// and register fields when it is an i64 arith (ADD/SUB/MUL/MOD — the
/// measured census pairs) whose lhs or rhs operand reads the load's dst.
/// Returns `(opcode, arith dst, lhs, rhs)`.
pub(super) fn fused_i64_arith(code: &[u32], idx: usize, dst: u8) -> Option<(u8, u8, u8, u8)> {
    let [op, adst, lhs, rhs] = code.get(idx + 1)?.to_be_bytes();
    if lhs != dst && rhs != dst {
        return None;
    }
    matches!(op, OP_ADD_I64 | OP_SUB_I64 | OP_MUL_I64 | OP_MOD_I64).then_some((op, adst, lhs, rhs))
}

/// The fused pair shape for a KCONST at `idx`: the float counterpart of
/// [`fused_i64_arith`] over the measured census pairs (ADD/MUL F64).
pub(super) fn fused_f64_arith(code: &[u32], idx: usize, dst: u8) -> Option<(u8, u8, u8, u8)> {
    let [op, adst, lhs, rhs] = code.get(idx + 1)?.to_be_bytes();
    if lhs != dst && rhs != dst {
        return None;
    }
    matches!(op, OP_ADD_F64 | OP_MUL_F64).then_some((op, adst, lhs, rhs))
}

// The peek-next compare-branch fusion: a comparison whose immediately following word is the
// measured census branch spelling — JMPIF_NOT, the universal loop-head
// exit — testing the compare's own dst takes its branch decision inside
// the compare's dispatch: one dispatch for two uops, pc += 2, the uop
// stream and the module format staying byte-identical. The bool write
// stays (the dst may be read on either path), and the branch runs through
// the exact JMPIF_NOT arm body, so the `jump!` macro's bounds check, trap
// attribution, and taken-backward cancel observation read the branch's
// own pc exactly as the unfused spelling does — a fused back-edge
// preempts a parked fiber where the two-dispatch run does, jump-to-self
// included. An intervening word fails the opcode-byte match, and a
// compare at the code end peeks nothing (`code.get` on the
// out-of-bounds index). JMPIF consuming the compare's dst never fuses
// (the census measured no such pair), and neither do the unmeasured
// compares.

/// The fused pair shape for a comparison at `idx`: the next word's biased
/// jump field when it is a JMPIF_NOT whose tested cond is the compare's
/// own dst.
pub(super) fn fused_jmpif_not(code: &[u32], idx: usize, dst: u8) -> Option<u16> {
    let [op, cond, hi, lo] = code.get(idx + 1)?.to_be_bytes();
    if cond != dst {
        return None;
    }
    (op == OP_JMPIF_NOT).then_some(u16::from_be_bytes([hi, lo]))
}

// The peek-next tag-branch fusion:
// a TAGTEST whose immediately following word is the measured census branch
// spelling — JMPIF, the match dispatch's arm-guard exit — testing the tag
// test's own dst takes its branch decision inside the tag test's dispatch,
// the compare-branch slice's discipline exactly: one dispatch for two
// uops, the uop stream and the module format staying byte-identical. The
// bool write stays (the dst may be read on either path), and the branch
// runs through the exact JMPIF arm body, so the `jump!` macro's bounds
// check, trap attribution, and taken-backward cancel observation read the
// branch's own pc exactly as the unfused spelling does — a fused back-edge
// preempts a parked fiber where the two-dispatch run does, jump-to-self
// included. An intervening word fails the opcode-byte match, and a tag
// test at the code end peeks nothing (`code.get` on the out-of-bounds
// index). JMPIF_NOT after a TAGTEST never fuses (the census measured no
// such pair — the guard loops the lowering emits around tag tests spell
// JMPIF), and TAGTEST_NOT stays outside the family entirely: no kernel
// stream carries the uop at all, so it has no measured pair.

/// The fused pair shape for a tag test at `idx`: the next word's biased
/// jump field when it is a JMPIF whose tested cond is the tag test's own
/// dst.
pub(super) fn fused_jmpif(code: &[u32], idx: usize, dst: u8) -> Option<u16> {
    let [op, cond, hi, lo] = code.get(idx + 1)?.to_be_bytes();
    if cond != dst {
        return None;
    }
    (op == OP_JMPIF).then_some(u16::from_be_bytes([hi, lo]))
}
