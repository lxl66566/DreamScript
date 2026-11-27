//! The Rust-side reentry seam: the hosted calls that drive a frame to its return,
//! the word-keyed callee resolution they share, and the heap and claim accessors.

use super::*;

impl Vm {
    /// Drives one reentrant call of a planned function from native code
    /// — the lambda-taking container combinators' per-element
    /// invocation: a synthetic reentry frame (the entry-frame sentinel
    /// return pc marks it) is pushed on the CURRENT fiber's chain at
    /// `boundary` and driven to its return, which answers
    /// [`DriveOutcome::Frame`]. The callee word is callable-defaulted:
    /// a scalar FuncRef names the function directly, a `Ref` word
    /// names a closure object whose capture block lands in the callee's
    /// leading local slots exactly like a CALLS copy-in (one acquired
    /// count per `Ref` capture, balanced by the callee body's own
    /// placement). Each Ref argument carries one count into
    /// the frame (the local-call hand-off: the callee's placement owns
    /// its params), and the returned words transfer their counts to the
    /// caller — one word for a scalar body, the whole span for a
    /// multi-word return (the element span each way: the arguments are
    /// the callee's flattened parameter words, the answer its whole
    /// return span). A drive that ends any other way —
    /// the lambda HALTs under nesting, a fiber switch's resumer chain
    /// never returns, a nursery cancel kills the fiber, a yield parks
    /// at the host — refuses with [`Trap::ReentrancyRefused`]: a
    /// Rust-side loop cannot suspend, and the chain is already lost or
    /// parked. On every non-returning outcome the fiber that hosted the
    /// reentry frame is interrupted (never resumable again — a parked
    /// resume would run its lambda into the sentinel return pc). A
    /// trapped lambda propagates its trap with the frames kept for
    /// [`Vm::backtrace`].
    pub(crate) fn call_hosted(
        &mut self,
        callee: Value,
        args: &[Value],
    ) -> Result<FrameAnswer, Trap> {
        self.call_hosted_in(callee, args, None)
    }

    /// The hosted call seam's owner-explicit half: the teardown pass
    /// names the dying frame's version row itself (the hook's owning
    /// frame may sit deep in the dying chain, not at its innermost entry),
    /// every other caller resolving staged values through the current
    /// slot.
    /// Resolves a callable word for the CALLS arm and the hosted-call
    /// seam through the word-keyed inline cache: a hit whose key matches
    /// the (FuncRef, owner, nargs) triple and whose stamp matches the
    /// registry generation answers the row and index the full path
    /// resolved, otherwise the full [`StackState::resolve_func_word`]
    /// runs and its answer fills the slot. The arity rode the key, so a
    /// hit skips the full path's arity compare; traps are never cached,
    /// so every refusal keeps the full path's shape.
    pub(super) fn resolve_word(
        registry: &ModuleRegistry,
        word_cache: &mut [Option<WordCallee>],
        funcref: FuncRef,
        owner: Option<VersionIdx>,
        nargs: u8,
    ) -> Result<(VersionIdx, u32), Trap> {
        let stamp = registry.generation();
        let mut hasher = WordKeyHasher::default();
        hasher.mix(u64::from(funcref.module()));
        hasher.mix(u64::from(funcref.func()));
        hasher.mix(u64::from(nargs));
        hasher.mix(owner.map_or(0, |row| u64::from(row.0) + 1));
        // the truncated high hash bits only spread the slot choice;
        // the mask keeps the index inside the table on every pointer width
        #[allow(clippy::cast_possible_truncation)]
        let at = hasher.finish() as usize & (WORD_CACHE_SLOTS - 1);
        if let Some(entry) = word_cache[at].as_ref()
            && entry.funcref == funcref
            && entry.owner == owner
            && entry.nargs == nargs
            && entry.stamp == stamp
        {
            return Ok((entry.version, entry.func));
        }
        let resolved = StackState::resolve_func_word(registry, funcref, owner, nargs)?;
        word_cache[at] = Some(WordCallee {
            funcref,
            owner,
            nargs,
            stamp,
            version: resolved.0,
            func: resolved.1,
        });
        Ok(resolved)
    }

    pub(crate) fn call_hosted_in(
        &mut self,
        callee: Value,
        args: &[Value],
        owner: Option<VersionIdx>,
    ) -> Result<FrameAnswer, Trap> {
        let nargs = u8::try_from(args.len()).expect("hosted-call arities are tiny");
        let (funcref, captures, closure_id) = match callee {
            Value::Fn(funcref) => (funcref, None, None),
            Value::Ref(obj) => {
                let (header, block) = stage_closure(
                    &mut self.objects,
                    &mut self.pool,
                    self.current,
                    &mut self.calls_staging,
                    obj,
                )?;
                (header, Some(block), Some(obj))
            },
            other => {
                return Err(Trap::SlotTypeMismatch {
                    slot: 0,
                    expected: SlotType::Fn,
                    actual: other.slot_type(),
                })
            },
        };
        // the persistent reentry frame's identity half: a scalar callee
        // consecutive to the last one under the same install stamp reads
        // the recorded resolution instead of probing the bank — a hit
        // answers exactly what the full path would, so only the frame
        // build below differs from a first call
        let stamp = self.registry.generation();
        let (version, func_id) = if captures.is_none()
            && let Some(memo) = self.reentry.as_ref()
            && memo.funcref == funcref
            && memo.owner == owner
            && memo.nargs == nargs
            && memo.stamp == stamp
        {
            (memo.version, memo.func)
        } else {
            let resolved =
                Self::resolve_word(&self.registry, &mut self.word_cache, funcref, owner, nargs)?;
            if captures.is_none() {
                self.reentry = Some(HostedReentry {
                    funcref,
                    owner,
                    nargs,
                    stamp,
                    version: resolved.0,
                    func: resolved.1,
                });
            }
            resolved
        };
        let target = &self.registry.pinned(version).functions()[func_id as usize];
        let frame_size = target.layout.frame_size() as usize;
        let base = self.stack.len();
        let needed = base + frame_size;
        if needed > self.limit {
            return Err(Trap::StackOverflow {
                needed,
                limit: self.limit,
            });
        }
        // the frame build grows the segment once and writes the metadata
        // word and the staged arguments in place: the region's chunks
        // persist across consecutive reentries (RET truncates the len,
        // never the storage), so a word the callee never writes holds the
        // region's previous occupant, the same recycled frame a CALL
        // pushes.
        self.stack.grow_to(needed);
        self.stack[base] = Word::Meta(FrameMetadata {
            return_pc: CodePc(u32::MAX),
            frame_size: u32::try_from(frame_size).expect("frame span fits u32"),
            tail: TailChain::default(),
        });
        // the local-call hand-off: each Ref argument carries one count
        // into the frame (the callee's placement treats its params as
        // owned — returning one transfers that count with the answer,
        // and a dropped one balances at the body's exit drops). The
        // word may name a foreign heap, so the inc routes.
        let handoff_owner = self.current;
        for &arg in args {
            if let Value::Ref(word) = arg {
                Self::inc_ref_split(
                    &mut self.objects,
                    &mut self.pool,
                    &mut self.claims,
                    handoff_owner,
                    self.ctx_live,
                    handoff_owner,
                    word,
                )?;
            }
        }
        for (at, &arg) in args.iter().enumerate() {
            self.stack[base + 1 + at] = Word::Slot(arg);
        }
        // the closure's capture block lands in the callee's leading local
        // slots — the CALLS copy-in discipline: one acquired count per
        // Ref capture, the body's own placement balancing it, while the
        // closure object keeps its own count untouched
        let capture_body_len = target.layout.body_len();
        if let Some(block) = &captures {
            // the range check counts the block plus the reserved closure
            // word right past it (the mutation window's address of the
            // object), mirroring the CALLS copy-in
            let end = target.layout.params_len() + block.len() + 1;
            if end > capture_body_len {
                return Err(Trap::SlotOutOfRange {
                    slot: u8::try_from(end - 1).unwrap_or(u8::MAX),
                    frame_slots: target.layout.slots().len(),
                });
            }
            let owner = self.current;
            for (index, &value) in block.iter().enumerate() {
                if let Value::Ref(word) = value {
                    Self::inc_ref_split(
                        &mut self.objects,
                        &mut self.pool,
                        &mut self.claims,
                        owner,
                        self.ctx_live,
                        owner,
                        word,
                    )?;
                }
                let slot = u8::try_from(target.layout.params_len() + index)
                    .expect("capture slot fits u8 after the range check");
                write_slot(
                    &mut self.stack,
                    base,
                    slot,
                    target.layout.slots(),
                    capture_body_len,
                    value,
                )?;
            }
            let closure_slot = u8::try_from(target.layout.params_len() + block.len())
                .expect("the closure word slot fits u8 after the range check");
            let closure_word = closure_id.expect("a block rides only a closure callee");
            write_slot(
                &mut self.stack,
                base,
                closure_slot,
                target.layout.slots(),
                capture_body_len,
                Value::Ref(closure_word),
            )?;
        }
        self.callinfo.push(CallInfo {
            fp: base,
            module: version,
            func: func_id,
        });
        let fiber = &mut self.pool[self.current.index()];
        fiber.max_len = fiber.max_len.max(needed);
        let boundary = self.callinfo.len();
        let host = self.current;
        // the entry and the reentry drive's Active name one table row:
        // a hosted call mints no version handle
        let active = Active {
            version,
            func: func_id,
        };
        let driven = match self.execute_host_step::<false>(active, CodePc(0), boundary) {
            Ok(DriveOutcome::Frame(value)) => Ok(value),
            Ok(DriveOutcome::Died(_) | DriveOutcome::Yielded(_) | DriveOutcome::WorldPark(_)) => {
                Err(Trap::ReentrancyRefused)
            },
            Ok(DriveOutcome::Interlock(_)) => {
                unreachable!("the step drains every interlock before returning")
            },
            Err((trap, _at)) => Err(trap),
        };
        if driven.is_err() {
            // the Rust-side loop is gone, so the sentinel reentry frame on
            // the hosting fiber's chain can never be answered — the drive
            // answered anything but its Frame. A parked host left resumable
            // would re-enter on a later resume and run its lambda straight
            // into the sentinel return pc, so interrupt it like a trapped
            // drive: frames kept (the staged-argument counts stay with
            // them, the same accounting as every trap path), re-entry
            // refused with the structured fiber refusal
            let fiber = &mut self.pool[host.index()];
            if fiber.state != FiberState::Dead {
                fiber.resume = None;
                fiber.state = FiberState::Running;
            }
        }
        driven
    }

    /// The routing face over the running fiber's store and the pool, for
    /// the dispatcher's own routed count mutations and reads.
    pub(super) fn heaps(&mut self) -> Heaps<'_> {
        Heaps::new(
            self.current,
            &mut self.objects,
            &mut self.pool,
            &mut self.claims,
            self.ctx_live,
        )
    }

    /// The death walk's router: a parked drive's context store is the
    /// scratch, so every store is reached through the pool.
    pub(super) fn heaps_walk(&mut self) -> Heaps<'_> {
        Heaps::new(
            self.current,
            &mut self.objects,
            &mut self.pool,
            &mut self.claims,
            false,
        )
    }

    /// The root fiber's store, for the host faces: the running context's
    /// when the root runs, its pool slot's otherwise.
    pub(super) fn root_store(&mut self) -> &mut Objects {
        if self.ctx_live && self.current == FIBER_ROOT {
            &mut self.objects
        } else {
            &mut self.pool[FIBER_ROOT.index()].objects
        }
    }

    /// Routed `rc_inc` by the word's owner axis, queues drained.
    pub(super) fn inc_ref(&mut self, word: RefId) -> Result<(), Trap> {
        self.heaps().inc(word)
    }

    /// Routed `rc_dec` by the word's owner axis, queues drained.
    pub(super) fn dec_ref(&mut self, word: RefId) -> Result<(), Trap> {
        self.heaps().dec(word)
    }

    /// The replaced capture word's edge release (CAPTURE_STORE's displaced
    /// half): routed by owner, no frame claim — the count belonged to the
    /// closure object's block.
    pub(super) fn dec_edge(&mut self, word: RefId) -> Result<(), Trap> {
        self.heaps().dec_edge(word)
    }

    /// [`Vm::inc_ref`] over disjoint fields, for sites holding a live
    /// staging-buffer borrow (the capture copy-ins). The count is claimed
    /// for `claimant`'s frames — the running fiber's own copy-ins, or the
    /// spawned child's entry frame at the spawn seam.
    pub(super) fn inc_ref_split(
        objects: &mut Objects,
        pool: &mut [Fiber],
        claims: &mut [ClaimTable],
        running: FiberId,
        live: bool,
        claimant: FiberId,
        word: RefId,
    ) -> Result<(), Trap> {
        Heaps::new(running, objects, pool, claims, live).inc_for(claimant, word)
    }

    /// An object-graph edge's count (a closure's capture adoption): routed
    /// by the word's owner, owned by the object — its free cascade
    /// releases it, no frame claim.
    pub(super) fn inc_edge(&mut self, word: RefId) -> Result<(), Trap> {
        self.heaps().inc_edge(word)
    }

    /// A closure word's header and capture block copied out of its own
    /// heap ([`Heaps::closure_parts`]).
    pub(super) fn closure_parts(&mut self, word: RefId) -> Result<(FuncRef, Vec<Value>), Trap> {
        self.heaps().closure_parts(word)
    }

    /// Splits the VM for a generic builtin body's pure-object work: the
    /// store-side routing face and the output sink, disjoint from every
    /// other field. The running fiber's store is the allocation default;
    /// every id-taking service routes. The reentrant bodies (the
    /// lambda-taking combinators) keep `vm` whole and call back through
    /// [`Vm::call_hosted`].
    pub(crate) fn split(&mut self) -> (Heaps<'_>, &mut OutputSink) {
        (
            Heaps::new(
                self.current,
                &mut self.objects,
                &mut self.pool,
                &mut self.claims,
                self.ctx_live,
            ),
            &mut self.out,
        )
    }
}
