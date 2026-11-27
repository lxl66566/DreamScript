//! Channel steps: the send and receive fast paths, rendezvous parking, and
//! the deadline and wake seams.

use ds_abi::native::CHAN_HOOK_STAMP;

use super::*;

impl Vm {
    /// One channel step: the fast
    /// paths run against the channel's own store through the router
    /// (deposit, pop, the waiter queues), a full send or empty receive
    /// parks the caller in the channel's waiter queue — the payload's
    /// whole word span staged with no count move, the parked frame
    /// keeping its own counts exactly the net send wait's discipline —
    /// and a completed hand-off wakes the parked peer through the
    /// scheduler's delivery and run queue, the peer fiber itself the
    /// resumer (no reactor interest, no deadline). The payload element
    /// spans words at the buffer's element stride (the collection span
    /// seam: a record or tuple payload's whole span crosses, the
    /// recv's answer the tag-less total of its words), the staged span
    /// read past the channel word exactly a `list_push` element's.
    /// A zero-bound channel is the rendezvous: the buffer holds
    /// nothing, so a send with no parked receiver parks with its staged
    /// span, a receive with no staged sender parks — and the first peer
    /// to arrive pairs with its opposite directly (the deposit's edge
    /// counts and the take's ledger claims the buffered hand-off's
    /// own discipline, the buffer never retaining the element).
    pub(super) fn chan_step(
        &mut self,
        channel: ChannelNative,
        fp: usize,
        caller: &FrameLayout,
        nargs: u8,
        version: VersionIdx,
        module: u32,
    ) -> Result<ChanStep, Trap> {
        match channel {
            ChannelNative::New => {
                let cap = control_i64(&self.stack, fp, caller, 0)?;
                if cap < 0 {
                    return Err(Trap::ChanCapacity { cap });
                }
                let cap = usize::try_from(cap).expect("a non-negative capacity fits usize");
                let word = self.heaps().alloc_chan(cap);
                Ok(ChanStep::Answer(NativeResult::Word(word)))
            },
            ChannelNative::Send => {
                let Value::Ref(chan) = control_word(&self.stack, fp, caller, 0)? else {
                    return Err(Trap::UnknownObject { obj: 0 });
                };
                // the one-word payload keeps the allocation-free read; a
                // wider element's whole staged span rides the vec (the
                // park path owns its copy either way)
                let payload = if usize::from(nargs) == 2 {
                    vec![control_word(&self.stack, fp, caller, 1)?]
                } else {
                    (1..usize::from(nargs))
                        .map(|index| control_word(&self.stack, fp, caller, index))
                        .collect::<Result<Vec<_>, Trap>>()?
                };
                let here = self.current;
                let (payload, hook) =
                    self.chan_hook_stamp(payload, version, module, fp, caller, nargs)?;
                // a zero-bound channel's only room is a live parked
                // receiver: the deposit takes the hand-off's edge counts
                // and the release hands the element straight over, the
                // buffer never retaining it; with no receiver the
                // sender parks, its staged span the rendezvous's other
                // door
                if !self.heaps().chan_room(chan)? && !self.chan_live_receiver(chan)? {
                    self.heaps()
                        .chan_park_sender(chan, here, payload, hook.clone())?;
                    if hook.is_some() {
                        self.chan_leases.push((chan, here));
                    }
                    return Ok(ChanStep::Park(None));
                }
                self.heaps().chan_deposit(chan, &payload, hook)?;
                self.chan_release(chan)?;
                Ok(ChanStep::Answer(NativeResult::Word(Value::I64(0))))
            },
            ChannelNative::Recv => {
                let Value::Ref(chan) = control_word(&self.stack, fp, caller, 0)? else {
                    return Err(Trap::UnknownObject { obj: 0 });
                };
                let here = self.current;
                let Some(elem) = self.heaps().chan_pop(chan)? else {
                    // a zero-bound channel's buffer holds nothing, so
                    // the empty pop pairs with a staged sender first:
                    // the staged span's deposit takes the buffer's edge
                    // count per `Ref` word (the parked sender's frame
                    // keeps its own counts), the wake answers the sender
                    // with the unit word, and the immediate pop hands
                    // the span to this frame — no refill, the next
                    // staged sender has no slot to fill
                    let Some(sender) = self.chan_take_live_sender(chan)? else {
                        self.heaps().chan_park_receiver(chan, here)?;
                        return Ok(ChanStep::Park(None));
                    };
                    self.drop_chan_lease(chan, sender.fiber);
                    self.heaps()
                        .chan_deposit(chan, &sender.payload, sender.hook)?;
                    let elem = self
                        .heaps()
                        .chan_pop(chan)?
                        .expect("the rendezvous deposit precedes its pop");
                    self.wake_chan_waiter(sender.fiber, NativeResult::Word(Value::I64(0)));
                    return Ok(ChanStep::Answer(self.chan_elem_answer(elem, here)));
                };
                let result = self.chan_elem_answer(elem, here);
                self.chan_refill(chan)?;
                Ok(ChanStep::Answer(result))
            },
            ChannelNative::SendDeadline => {
                let Value::Ref(chan) = control_word(&self.stack, fp, caller, 0)? else {
                    return Err(Trap::UnknownObject { obj: 0 });
                };
                let nanos = control_i64(&self.stack, fp, caller, 1)?;
                let payload = (2..usize::from(nargs))
                    .map(|index| control_word(&self.stack, fp, caller, index))
                    .collect::<Result<Vec<_>, Trap>>()?;
                let here = self.current;
                let (payload, hook) =
                    self.chan_hook_stamp(payload, version, module, fp, caller, nargs)?;
                if !self.heaps().chan_room(chan)? && !self.chan_live_receiver(chan)? {
                    // the deadline bounds only the wait: a full channel
                    // beside a past deadline answers the failure span at once,
                    // no park
                    if nanos <= 0 {
                        return Ok(ChanStep::Answer(timeout_failure_span()));
                    }
                    self.heaps()
                        .chan_park_sender(chan, here, payload, hook.clone())?;
                    if hook.is_some() {
                        self.chan_leases.push((chan, here));
                    }
                    return Ok(ChanStep::Park(Some(WorldWait::chan_deadline(
                        chan,
                        true,
                        Instant::now() + Duration::from_nanos(nanos.cast_unsigned()),
                    ))));
                }
                self.heaps().chan_deposit(chan, &payload, hook)?;
                self.chan_release(chan)?;
                Ok(ChanStep::Answer(ok_tagged(Value::I64(0))))
            },
            ChannelNative::RecvDeadline => {
                let Value::Ref(chan) = control_word(&self.stack, fp, caller, 0)? else {
                    return Err(Trap::UnknownObject { obj: 0 });
                };
                let nanos = control_i64(&self.stack, fp, caller, 1)?;
                let here = self.current;
                let Some(elem) = self.heaps().chan_pop(chan)? else {
                    let Some(sender) = self.chan_take_live_sender(chan)? else {
                        if nanos <= 0 {
                            return Ok(ChanStep::Answer(timeout_failure_span()));
                        }
                        self.heaps().chan_park_receiver(chan, here)?;
                        return Ok(ChanStep::Park(Some(WorldWait::chan_deadline(
                            chan,
                            false,
                            Instant::now() + Duration::from_nanos(nanos.cast_unsigned()),
                        ))));
                    };
                    self.drop_chan_lease(chan, sender.fiber);
                    self.heaps()
                        .chan_deposit(chan, &sender.payload, sender.hook)?;
                    let elem = self
                        .heaps()
                        .chan_pop(chan)?
                        .expect("the rendezvous deposit precedes its pop");
                    self.wake_chan_waiter(sender.fiber, NativeResult::Word(Value::I64(0)));
                    return Ok(ChanStep::Answer(self.chan_deadline_answer(elem, here)));
                };
                let result = self.chan_deadline_answer(elem, here);
                self.chan_refill(chan)?;
                Ok(ChanStep::Answer(result))
            },
        }
    }

    /// The deadline recv's ok answer: the element's word under the ok
    /// discriminant (the fallible union's success member), a wider
    /// element's whole span under the same discriminant (the span
    /// channel's shape, the parked call site's own span staging)
    pub(super) fn chan_deadline_answer(&mut self, elem: ChanPop, fiber: FiberId) -> NativeResult {
        match self.chan_elem_answer(elem, fiber) {
            NativeResult::Word(word) => ok_tagged(word),
            NativeResult::Total { words } => ok_spanned(words),
            other => other,
        }
    }

    /// The channel switch behind a `CALL` site: the step runs, the
    /// answer overlays argument slot 0, and a park ends the drive with
    /// the world-park event (the resume point the instruction after the
    /// call).
    pub(super) fn chan_switch(
        &mut self,
        channel: ChannelNative,
        active: &Active,
        pc: u32,
        fp: usize,
        caller: &FrameLayout,
        nargs: u8,
    ) -> Result<ChanOutcome, Trap> {
        // the sending function's module: the hook fact resolves its
        // stamped `$deinit` index through it (the deinit is the payload
        // struct's own synthesized function in the sending module's plan)
        let module = self
            .registry
            .module_id(self.registry.pinned(active.version).name())
            .unwrap_or(u32::MAX);
        match self.chan_step(channel, fp, caller, nargs, active.version, module)? {
            ChanStep::Answer(result) => {
                overlay_native(result, &mut self.stack, caller, fp)?;
                Ok(ChanOutcome::Answered)
            },
            ChanStep::Park(wait) => {
                let kind = wait.map_or(ParkKind::Chan, ParkKind::World);
                match self
                    .park_world(active, CodePc(pc), kind)
                    .map_err(|(trap, _)| trap)?
                {
                    event @ DriveOutcome::WorldPark(_) => Ok(ChanOutcome::Parked(event)),
                    _ => unreachable!("a park answers the park event"),
                }
            },
        }
    }

    /// Stages one popped channel element as the native answer to fiber
    /// `fiber`: each `Ref` word's edge count becomes the fiber's claim
    /// (the ledger transfer, no count mutation), the one-word element a
    /// word, a wider element the tag-less total of its span.
    pub(super) fn chan_elem_answer(&mut self, elem: ChanPop, fiber: FiberId) -> NativeResult {
        match elem {
            ChanPop::One(word) => {
                if let Value::Ref(payload) = word {
                    self.heaps().adopt_claim(fiber, payload);
                }
                NativeResult::Word(word)
            },
            ChanPop::Span(words) => {
                for word in &words {
                    if let Value::Ref(payload) = word {
                        self.heaps().adopt_claim(fiber, *payload);
                    }
                }
                NativeResult::Total { words }
            },
        }
    }

    /// Whether channel `chan`'s receiver queue holds a live parked
    /// receiver, dropping the stale head entries a cancel or release
    /// door left (their queue entry rode no state): the rendezvous
    /// send's pairing door reads it without consuming the live entry —
    /// the release the door's deposit precedes performs the take.
    pub(super) fn chan_live_receiver(&mut self, chan: RefId) -> Result<bool, Trap> {
        loop {
            match self.heaps().chan_peek_receiver(chan)? {
                None => return Ok(false),
                Some(fiber) => {
                    if self
                        .pool
                        .get(fiber.index())
                        .is_some_and(|f| f.state == FiberState::WorldParked)
                    {
                        return Ok(true);
                    }
                    self.heaps().chan_take_receiver(chan)?;
                },
            }
        }
    }

    /// Pops the head live staged sender off channel `chan`, dropping
    /// the stale entries a cancel or release door left (the payload's
    /// counts were the parked frame's own, released by whoever cut it):
    /// the rendezvous receive's pairing door.
    pub(super) fn chan_take_live_sender(
        &mut self,
        chan: RefId,
    ) -> Result<Option<ChanSender>, Trap> {
        loop {
            match self.heaps().chan_take_sender(chan)? {
                None => return Ok(None),
                Some(sender) => {
                    if self
                        .pool
                        .get(sender.fiber.index())
                        .is_some_and(|f| f.state == FiberState::WorldParked)
                    {
                        self.drop_chan_lease(chan, sender.fiber);
                        return Ok(Some(sender));
                    }
                    // a stale entry whose sender is gone: its loss door
                    // already took the fact — but a hand-assembled tangle
                    // can leave one queued, so the fact queues here rather
                    // than dropping with the entry
                    self.queue_sender_fire(sender);
                },
            }
        }
    }

    /// Hands one buffered payload element to the head live receiver
    /// after a deposit: each `Ref` word's buffer edge count becomes the
    /// receiver's claim (the ledger transfer, no count mutation), the
    /// delivery staged — the one-word element as a word, a wider one as
    /// the tag-less total of its span — and the receiver queued ready.
    /// Stale waiters — a nursery cancel cut them, a release door took
    /// them — drop; their entry rode no count.
    pub(super) fn chan_release(&mut self, chan: RefId) -> Result<(), Trap> {
        loop {
            let Some(receiver) = self.heaps().chan_take_receiver(chan)? else {
                return Ok(());
            };
            if self
                .pool
                .get(receiver.index())
                .is_none_or(|f| f.state != FiberState::WorldParked)
            {
                continue;
            }
            let elem = self.heaps().chan_pop(chan)?;
            let Some(elem) = elem else {
                // a woken receiver parks only over an empty buffer and
                // the deposit preceded, so this names hand-assembled
                // waiter tangle; the receiver goes back empty-handed
                self.heaps().chan_park_receiver(chan, receiver)?;
                return Ok(());
            };
            let result = self.chan_elem_answer(elem, receiver);
            self.wake_chan_waiter(receiver, result);
            return Ok(());
        }
    }

    /// Moves one parked sender's staged payload span into the slot a
    /// receive freed: the deposit takes the buffer's edge count per
    /// `Ref` word on the payload's own heap (the count funnel), the sender
    /// woken with the unit word. Stale senders drop — the payload's
    /// counts were the parked frame's own, released by whoever cut it.
    pub(super) fn chan_refill(&mut self, chan: RefId) -> Result<(), Trap> {
        loop {
            let Some(sender) = self.heaps().chan_take_sender(chan)? else {
                return Ok(());
            };
            if self
                .pool
                .get(sender.fiber.index())
                .is_none_or(|f| f.state != FiberState::WorldParked)
            {
                continue;
            }
            self.drop_chan_lease(chan, sender.fiber);
            self.heaps()
                .chan_deposit(chan, &sender.payload, sender.hook)?;
            self.wake_chan_waiter(sender.fiber, NativeResult::Word(Value::I64(0)));
            return Ok(());
        }
    }

    /// Whether `fiber` stands parked on a channel deadline wait — the
    /// win doors' answer shaping reads the row before the wake's own
    /// deregistration drops it.
    pub(super) fn chan_deadline_parked(&self, fiber: FiberId) -> bool {
        self.sched.waits.iter().any(|(_, entry)| {
            entry.fiber == fiber && matches!(entry.wait, WorldWait::ChanDeadline { .. })
        })
    }

    /// Wakes one channel-parked fiber with `result`: the delivery and
    /// run-queue push the net wake performs, minus the wait table (a
    /// channel waiter registers in the channel's own data, never the
    /// scheduler's — nothing to deregister, no deadline to cancel). A
    /// deadline waiter is the one exception: its row (and its wheel
    /// entry, stale on pop) drops here, the win door's own
    /// deregistration, so the fired wheel never meets a delivered fiber
    /// — and its win answer rides the two-word ok span its parked call
    /// site overlays (the plain waiter's one-word answer would leave the
    /// span's payload word stale at the site).
    /// The send's trailing stamp read: a deinit-carrying payload stages
    /// its `$deinit` function-table index then the reserved stamp tag
    /// after the payload span, so the last staged word decides the
    /// staging's shape — the mint point of the channel hook fact (code
    /// identity beside the buffer's edge counts; no count, no ledger
    /// row). The stamp words strip off the payload span the doors read.
    fn chan_hook_stamp(
        &mut self,
        mut payload: Vec<Value>,
        version: VersionIdx,
        module: u32,
        fp: usize,
        caller: &FrameLayout,
        nargs: u8,
    ) -> Result<(Vec<Value>, Option<ChanHook>), Trap> {
        let stamp = control_word(&self.stack, fp, caller, usize::from(nargs) - 1)?;
        let hook = match stamp {
            Value::Tag(tag) if tag == CHAN_HOOK_STAMP => {
                let idx = control_i64(&self.stack, fp, caller, usize::from(nargs) - 2)?;
                payload.pop();
                let func = u32::try_from(idx).map_err(|_| Trap::UnknownObject { obj: 0 })?;
                payload.pop();
                Some(ChanHook {
                    version,
                    module,
                    func,
                    width: u8::try_from(payload.len())
                        .map_err(|_| Trap::UnknownObject { obj: 0 })?,
                })
            },
            _ => None,
        };
        Ok((payload, hook))
    }

    /// Drops the fact lease one fiber's entry held on `chan`: a delivery
    /// took the entry, and the fact dies with it — the receiver's own
    /// registration owns the hook from here.
    fn drop_chan_lease(&mut self, chan: RefId, fiber: FiberId) {
        self.chan_leases
            .retain(|&(lease, owner)| !(lease == chan && owner == fiber));
    }

    /// Queues one entry's fact as a fire with no held counts (the
    /// staged span rode none): the stale-entry skip's and the deadline
    /// wake's loss shape.
    pub(crate) fn queue_sender_fire(&mut self, sender: ChanSender) {
        if let Some(hook) = sender.hook {
            self.chan_fires.push(ChanFire {
                hook,
                words: sender.payload,
                held: Vec::new(),
            });
        }
    }

    /// The cut door's sweep: every entry the dying fiber parked on some
    /// channel, taken with its fact. The sweep runs before the frame
    /// hooks (the queued span is the innermost value's death) and before
    /// the drain releases the frame's counts, so the hook's objects
    /// still live.
    pub(crate) fn sweep_chan_leases(&mut self, fiber: FiberId) -> Vec<ChanFire> {
        let mut fires = Vec::new();
        let leases = std::mem::take(&mut self.chan_leases);
        for (chan, owner) in leases {
            if owner != fiber {
                self.chan_leases.push((chan, owner));
                continue;
            }
            let Ok(taken) = self.heaps().chan_take_sender_of(chan, fiber) else {
                continue; // a dead channel: the lease row drops with it
            };
            for sender in taken {
                if let Some(hook) = sender.hook {
                    fires.push(ChanFire {
                        hook,
                        words: sender.payload,
                        held: Vec::new(),
                    });
                }
            }
        }
        fires
    }

    /// Whether any channel fire waits: the dispatch boundaries' one
    /// check — the VM's loss-door queue plus the current store's
    /// cascade-observed deaths.
    pub(crate) fn chan_fires_pending(&self) -> bool {
        !self.chan_fires.is_empty() || self.objects.fires_pending()
    }

    /// The dying fiber's store-cascade fires, taken for a door.
    pub(crate) fn take_store_fires(&mut self, fiber: FiberId) -> Vec<ChanFire> {
        if fiber == self.current && self.ctx_live {
            self.objects.take_fires()
        } else {
            self.pool[fiber.index()].objects.take_fires()
        }
    }

    /// Fires every waiting channel hook: each hook drives under the fire
    /// flag — a hook must return to its fire door (parking, dying,
    /// trapping, or a raised cancel flag refuses with
    /// `Trap::ReentrancyRefused`, its effects standing, the fire
    /// consumed) — then releases the fire's held edge counts through the
    /// funnels, the fire-before-release door order. A stale queue fire
    /// (dead span words, no held counts) skips silently.
    pub(crate) fn drain_chan_fires(&mut self) -> Result<(), Trap> {
        let mut fires = std::mem::take(&mut self.chan_fires);
        fires.extend(self.objects.take_fires());
        for fire in fires {
            if (fire.held.is_empty() && !self.fire_words_live(&fire.words))
                || fire.words.len() != usize::from(fire.hook.width)
            {
                // a stale queue fire (dead span words) or a corrupted
                // fact: fires nothing
                continue;
            }
            self.teardown_drive = true;
            let hook_result = self.call_hosted_in(
                Value::Fn(FuncRef::new(fire.hook.module, fire.hook.func)),
                &fire.words,
                Some(fire.hook.version),
            );
            self.teardown_drive = false;
            hook_result?;
            for word in &fire.held {
                if let Value::Ref(id) = word {
                    self.heaps().dec_edge(*id)?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn wake_chan_waiter(&mut self, fiber: FiberId, result: NativeResult) {
        if self
            .pool
            .get(fiber.index())
            .is_some_and(|f| f.state == FiberState::WorldParked)
        {
            let result = if self.chan_deadline_parked(fiber) {
                match result {
                    NativeResult::Word(word) => ok_tagged(word),
                    // a wider element's span rides the ok discriminant
                    // the deadline call site overlays
                    NativeResult::Total { words } => ok_spanned(words),
                    other => other,
                }
            } else {
                result
            };
            self.cancel_wait(fiber);
            self.pool[fiber.index()].state = FiberState::Suspended;
            self.sched.deliveries.insert(fiber, Wake::Answer(result));
            self.sched.push_ready(fiber);
        }
    }
}
