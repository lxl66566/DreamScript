//! The frame state shared by the stack operations: growth, frame push, tail
//! transfer and return over the current fiber's segment.

use super::*;

/// The mutable execution state the frame operations share, borrowed disjoint
/// from the dispatcher's current-function snapshot. The stack is the
/// current fiber's segment; `max_len` is that fiber's high-water mark, so
/// the TCO criterion holds per fiber.
pub(super) struct StackState<'a> {
    pub(super) stack: &'a mut Segment,
    pub(super) callinfo: &'a mut Vec<CallInfo>,
    pub(super) versions: &'a ModuleRegistry,
    pub(crate) limit: usize,
    pub(super) max_len: &'a mut usize,
}

impl StackState<'_> {
    /// Grows the segment to `new_len` after the per-entry limit compare.
    /// Never shrinks; growth appends chunks, so live frames' words
    /// keep their addresses.
    #[inline]
    pub(super) fn grow(&mut self, new_len: usize) -> Result<(), Trap> {
        if new_len <= self.stack.len() {
            return Ok(());
        }
        if new_len > self.limit {
            return Err(Trap::StackOverflow {
                needed: new_len,
                limit: self.limit,
            });
        }
        self.stack.grow_to(new_len);
        *self.max_len = (*self.max_len).max(new_len);
        Ok(())
    }

    /// Resolves a scalar FuncRef word (module id + function index, the
    /// `Fn` slot classification) for the CALLS dispatch and the hosted call
    /// seam. A staged fn word is a VALUE: it carries the
    /// current table's index space (the swap-time re-key keeps it there),
    /// so `None` resolves through the target module's current slot — the
    /// hot-reload redirect a staged word picks up. `Some(row)` is the
    /// frame-own rule — the teardown pass alone uses it, driving a hook
    /// the dying frame's own code declared. Callable words address the
    /// uop module tables only — natives are not values.
    pub(super) fn resolve_func_word(
        registry: &ModuleRegistry,
        funcref: FuncRef,
        owner: Option<VersionIdx>,
        nargs: u8,
    ) -> Result<(VersionIdx, u32), Trap> {
        let owner_name = owner.map(|row| registry.pinned(row).name());
        let target =
            if owner_name.is_some_and(|own| registry.module_name(funcref.module()) == Some(own)) {
                registry.resolve_indexed_in(owner.expect("checked above"), funcref.func())
            } else {
                let name =
                    registry
                        .module_name(funcref.module())
                        .ok_or_else(|| Trap::UnknownModule {
                            module: funcref.module(),
                        })?;
                registry.resolve_indexed(name, funcref.func())
            };
        let (version, func_id) = target.ok_or_else(|| Trap::UnknownCallee {
            module: registry
                .module_name(funcref.module())
                .unwrap_or("")
                .to_owned(),
            func: funcref.func(),
        })?;
        let target = &registry.pinned(version).functions()[func_id as usize];
        debug_assert_eq!(target.entry.tier1_prologue(), func_id as usize);
        if usize::from(nargs) != target.layout.params_len() {
            return Err(Trap::ArityMismatch {
                expected: target.layout.params_len(),
                given: usize::from(nargs),
            });
        }
        Ok((version, func_id))
    }

    /// Mints a scalar fn word from a pool FuncRef (the KCONST arm):
    /// a retiring frame's own-module ref carries its row's index space, so
    /// when the minting frame's row does not hold the module's current
    /// slot the mint translates the index through the identity the row
    /// records — name plus frame layout, the staged re-check's judge,
    /// extended by the content identity a moved index-embedded name
    /// judges by (body plus capture set hashing equal against exactly
    /// one current candidate) — into the current table. The minted value
    /// enters current-index-valid exactly like a re-keyed staged word,
    /// and a dropped or ambiguous identity refuses the mint instead of
    /// minting a word the current slot would silently misroute. A frame
    /// on the current row (the ordinary case) and every cross-module or
    /// native ref keep the verbatim spelling: a dependency's version the
    /// minting pool linked against is not recorded, the cross-module
    /// redirect staying the current-slot rule.
    pub(super) fn minted_fn_word(
        registry: &ModuleRegistry,
        frame_row: VersionIdx,
        fref: FuncRef,
    ) -> Result<Value, Trap> {
        let own_name = registry.pinned(frame_row).name();
        if registry.module_name(fref.module()) != Some(own_name) {
            return Ok(Value::Fn(fref));
        }
        let current = registry
            .slots(own_name)
            .expect("a frame's module stays installed")
            .current_idx();
        if current == frame_row {
            return Ok(Value::Fn(fref));
        }
        let refused = || Trap::UnknownCallee {
            module: own_name.to_owned(),
            func: fref.func(),
        };
        let Some(at) = usize::try_from(fref.func()).ok() else {
            return Err(refused());
        };
        let row = registry.pinned(frame_row);
        let Some(old) = row.functions().get(at) else {
            return Err(refused());
        };
        let current_version = registry.pinned(current);
        let moved = current_version
            .functions()
            .iter()
            .position(|f| f.name == old.name && f.layout == old.layout)
            .and_then(|at| u32::try_from(at).ok())
            .or_else(|| {
                old.embeds_table_index()
                    .then(|| current_version.content_unique(old.content_hash()))
                    .flatten()
            });
        moved.map_or_else(
            || Err(refused()),
            |at| Ok(Value::Fn(FuncRef::new(fref.module(), at))),
        )
    }

    /// The per-call-site check for `Raw`-staged arguments: they carry no
    /// static type, so the staged value words are checked against the
    /// callee's parameter types here (concrete staging was already proved
    /// by the site's plan). The site's cached word list names exactly the
    /// `Raw`-staged slots whose parameter is not itself the `Raw`
    /// wildcard, so a caller staging no `Raw` word reads nothing.
    pub(super) fn check_raw_staged(
        stack: &Segment,
        raw: &[RawStagedWord],
        caller: &FrameLayout,
        fp: usize,
    ) -> Result<(), Trap> {
        let body_len = caller.body_len();
        for word in raw {
            let value = read_value(stack, fp, word.slot, caller.slots(), body_len)?;
            let staged = value.slot_type();
            // a Ref word staged against a Fn parameter is the
            // callable-defaulted slot: a closure object's word behind
            // a fn-typed parameter, admitted exactly like the slide admits
            // the static pairing
            if staged != word.expected
                && !(staged == SlotType::Ref && word.expected == SlotType::Fn)
            {
                return Err(Trap::CallLayout(SlideError::TypeMismatch {
                    index: usize::from(word.index),
                    staged,
                    expected: word.expected,
                }));
            }
        }
        Ok(())
    }

    /// Non-tail `CALL`: the callee's metadata word lands in the caller's
    /// call word, the staged arguments overlay the caller's outgoing-argument
    /// area with no copying, and the stack grows for the callee's frame
    /// words. The plan (the ABI's own overlay fact) arrives proven by
    /// the site's cached facts; a refused pair re-raises here, exactly
    /// where the per-call derivation raised it.
    pub(super) fn call(
        &mut self,
        version: VersionIdx,
        func_id: u32,
        facts: &SiteFacts,
        caller: &FrameLayout,
        fp: usize,
        return_pc: u32,
    ) -> Result<Transfer, Trap> {
        let SiteFacts::Ready {
            raw, frame_size, ..
        } = facts
        else {
            return Err(Trap::CallLayout(facts.refused()));
        };
        Self::check_raw_staged(self.stack, raw.as_deref().unwrap_or(&[]), caller, fp)?;
        self.push_frame(
            version,
            func_id,
            *frame_size,
            caller.body_len(),
            fp,
            return_pc,
        )
    }

    /// The frame push under a proven plan: the callee's metadata word
    /// lands in the caller's call word and the chain gains the callee's
    /// entry. The callee's frame size arrives from the site's cached
    /// facts (derived at fill), so the push reads no version table row;
    /// the entry-staging check and the plan belong to [`Self::call`].
    #[inline]
    pub(super) fn push_frame(
        &mut self,
        version: VersionIdx,
        func_id: u32,
        frame_size: u32,
        caller_body_len: usize,
        fp: usize,
        return_pc: u32,
    ) -> Result<Transfer, Trap> {
        // the callee's metadata word lands in the caller's call word:
        // fp + 1 + body_len (the pinned physical mapping)
        let callee_fp = fp + 1 + caller_body_len;
        self.grow(callee_fp + frame_size as usize)?;
        self.stack[callee_fp] = Word::Meta(FrameMetadata {
            return_pc: CodePc(return_pc),
            frame_size,
            tail: TailChain::default(),
        });
        self.callinfo.push(CallInfo {
            fp: callee_fp,
            module: version,
            func: func_id,
        });
        Ok(Transfer {
            version,
            func: func_id,
        })
    }

    /// `CALLT`, the locked contract: keep the frame pointer, slide the
    /// staged arguments into slots `0..nargs` per the ABI slide plan (source
    /// derived from the caller's arg area), rewrite the metadata word in
    /// place (return pc preserved, callee frame size, tail chain grows by
    /// one elided frame), and enter the callee. The stack never grows at any
    /// depth; the only growth here is the one-time span extension when the
    /// callee's frame is physically larger, which is bounded by the largest
    /// callee frame, not by call depth. The plan arrives proven by the
    /// site's cached facts; a refused pair re-raises here, exactly where
    /// the per-call derivation raised it.
    pub(super) fn tail_call(
        &mut self,
        version: VersionIdx,
        func_id: u32,
        facts: &SiteFacts,
        caller: &FrameLayout,
        fp: usize,
    ) -> Result<Transfer, Trap> {
        let target = &self.versions.pinned(version).functions()[func_id as usize];
        let SiteFacts::Ready { plan, raw, .. } = facts else {
            return Err(Trap::CallLayout(facts.refused()));
        };
        Self::check_raw_staged(self.stack, raw.as_deref().unwrap_or(&[]), caller, fp)?;
        // The O(delta) slide, per the plan's direction. Two different slot
        // mappings meet here: the source slots are the CALLER's staged
        // arguments, which sit caller-relative (above the caller's call
        // word); the destination is the callee reusing the frame at the same
        // fp, whose param slot i sits at fp+1+i unconditionally, below the
        // CALLEE's own call word - never mapped through the caller's body,
        // or wide callees (params_len > caller body) would land one word off.
        let n = plan.len();
        let (dst0, src0) = (plan.dst().start, plan.src().start);
        let caller_body_len = caller.body_len();
        match plan.direction() {
            SlideDirection::None => {},
            SlideDirection::Ascending => {
                for i in 0..n {
                    let (d, s) = (fp + 1 + dst0 + i, word_of(fp, src0 + i, caller_body_len));
                    self.stack[d] = self.stack[s];
                }
            },
            SlideDirection::Descending => {
                for i in (0..n).rev() {
                    let (d, s) = (fp + 1 + dst0 + i, word_of(fp, src0 + i, caller_body_len));
                    self.stack[d] = self.stack[s];
                }
            },
        }
        let Word::Meta(meta) = &mut self.stack[fp] else {
            return Err(Trap::MetaWordCorrupt { fp });
        };
        meta.enter_tail_call(target.layout.frame_size());
        self.grow(fp + target.layout.frame_size() as usize)?;
        let frame = self.callinfo.last_mut().expect("a frame is always live");
        frame.module = version;
        frame.func = func_id;
        Ok(Transfer {
            version,
            func: func_id,
        })
    }

    /// `RET`: the result word overlays slot 0 (the caller reads it from its
    /// outgoing-argument slot 0); the stack restores the caller's frame
    /// span; the caller resumes at the metadata word's return pc. The
    /// restore answers the caller's own chain-entry row as the new
    /// [`Active`]: the row is a copyable table name, so the return moves no
    /// Arc count in any shape — a same-module return, a cross-module
    /// return, or a swap between the two entries' rows alike — and the
    /// entry always names the version the frame was entered under. A nested
    /// drive's synthetic reentry frame answers
    /// [`RetOutcome::Boundary`] instead: its caller is Rust-side code
    /// (`Vm::call_hosted`), not a frame. The sentinel return pc is the
    /// frame's identity marker — every CALL-pushed frame carries a real
    /// return pc, and a fiber switched in mid-drive never carries a
    /// reentry frame at all (a resumable fiber parks only at switch
    /// points, never mid-native), so the chain-position test cannot
    /// false-positive across a switch.
    pub(super) fn ret(
        &mut self,
        value: Value,
        fp: usize,
        boundary: usize,
    ) -> Result<RetOutcome, Trap> {
        let meta = match self.stack[fp] {
            Word::Meta(m) => m,
            Word::Slot(_) => return Err(Trap::MetaWordCorrupt { fp }),
        };
        if boundary != usize::MAX && self.callinfo.len() == boundary && meta.return_pc.0 == u32::MAX
        {
            // the reentry frame and everything above it leave the chain;
            // the value rides the outcome, and no caller frame reads the
            // overlay slot
            self.stack.truncate(fp);
            self.callinfo.pop();
            return Ok(RetOutcome::Boundary(value));
        }
        self.stack[fp + 1] = Word::Slot(value);
        if self.callinfo.len() == 1 {
            self.stack.truncate(fp + 2);
            return Ok(RetOutcome::Root(value));
        }
        // the caller's entry answers the bounds checks (its version's code
        // length, its layout's frame span); the row is Copy, so no borrow
        // outlives the chain move
        let caller_at = self.callinfo.len() - 2;
        let caller_row = self.callinfo[caller_at].module;
        let caller_fn =
            &self.versions.pinned(caller_row).functions()[self.callinfo[caller_at].func as usize];
        let code_len = code_len_u32(caller_fn.code.len());
        if meta.return_pc.0 >= code_len {
            return Err(Trap::InvalidPc {
                pc: meta.return_pc.0,
                code_len,
            });
        }
        let restore =
            (self.callinfo[caller_at].fp + caller_fn.layout.frame_size() as usize).max(fp + 2);
        self.stack.truncate(restore);
        self.callinfo
            .pop()
            .expect("a caller entry stands below the returning frame");
        let caller = &self.callinfo[caller_at];
        Ok(RetOutcome::Resume {
            version: caller.module,
            func: caller.func,
            pc: meta.return_pc,
        })
    }
}

pub(super) fn code_len_u32(len: usize) -> u32 {
    u32::try_from(len).expect("code length exceeds u32")
}

/// Maps a slot index to its stack-word index: parameters and locals sit at
/// `fp + 1 + i`; the call word sits between them and the outgoing-argument
/// area, whose slots live at `fp + 2 + i` (the frame diagram in
/// `ds-abi/src/layout.rs`). `body_len` is the parameter-plus-local count of
/// the frame's OWNING layout.
#[inline]
pub(super) fn word_of(fp: usize, slot: usize, body_len: usize) -> usize {
    fp + 1 + slot + usize::from(slot >= body_len)
}
