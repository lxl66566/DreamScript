//! Timer races: spawning the racer, cutting it, and resolving the race into
//! the parked caller's answer.

use super::*;

impl Vm {
    // -- The timer race ------------------------------------------------------

    /// The timer race behind a `CALL` site — the only site shape the
    /// lowerer emits (the fallible span re-spans into the call's union
    /// after the call, so a tail transfer has no position for the
    /// re-span and the lowering refuses a bare tail like every fallible
    /// native's). A deadline already past answers the failure span at
    /// once (no racer spawns, the caller never parks); a live one opens
    /// the race's own nursery — the calling fiber its owner, the
    /// callable's fiber its one child, born ready — and the caller parks
    /// on the deadline wheel. The racer starts under the park's drive
    /// (the executor's eager start, or the sweep that meets the parked
    /// owner), never here: a racer entered from a host drive's own step
    /// would hand that drive its death.
    pub(super) fn timeout_switch(
        &mut self,
        _timer: TimerNative,
        active: &Active,
        pc: u32,
        fp: usize,
        caller: &FrameLayout,
        boundary: usize,
    ) -> Result<TimerOutcome, Trap> {
        // a hosted reentry frame cannot park: refusing here leaves no
        // racer behind the refused call
        if self.reentry_live(boundary) {
            return Err(Trap::ReentrancyRefused);
        }
        let nanos = control_i64(&self.stack, fp, caller, 0)?;
        if nanos <= 0 {
            let failure = timeout_failure_span();
            overlay_native(failure, &mut self.stack, caller, fp)?;
            return Ok(TimerOutcome::Answered);
        }
        let thunk = control_word(&self.stack, fp, caller, 1)?;
        let deadline = Instant::now() + Duration::from_nanos(nanos.cast_unsigned());
        let nursery = self.open_fresh_nursery();
        let racer = match thunk {
            // a scalar FuncRef names a non-capturing callable: its entry
            // takes no arguments
            Value::Fn(fref) => self.spawn_ref_into(Some(nursery), fref, &[]),
            // a closure word: its capture block stages into the racer's
            // frame, one acquired count per `Ref` capture claimed for the
            // racer (the spawn seam's own hand-off)
            Value::Ref(obj) => self.spawn_closure_into(Some(nursery), obj, &[]),
            staged => Err(Trap::CallLayout(SlideError::TypeMismatch {
                index: 1,
                staged: staged.slot_type(),
                expected: SlotType::Fn,
            })),
        };
        if let Err(trap) = racer {
            // nothing of the race stands: the slot closes before the
            // refusal surfaces
            self.tear_down_nursery(nursery.index());
            return Err(trap);
        }
        self.nurseries[nursery.index()].race = Some(Race {
            expired: false,
            deadline,
        });
        self.pool[self.current.index()].race = Some(nursery);
        let wait = WorldWait::Timeout { deadline };
        match self
            .park_world(active, CodePc(pc), ParkKind::World(wait))
            .map_err(|(trap, _)| trap)?
        {
            event @ DriveOutcome::WorldPark(_) => Ok(TimerOutcome::Parked(event)),
            _ => unreachable!("a park answers the park event"),
        }
    }

    /// The open race `owner` waits on, `None` when it stands in none —
    /// the fiber's link names an ended race, whose slot another may hold.
    pub(super) fn race_of(&self, owner: FiberId) -> Option<NurseryId> {
        let id = self.pool[owner.index()].race?;
        self.nurseries
            .get(id.index())
            .filter(|nursery| {
                nursery.state == NurseryState::Open
                    && nursery.race.is_some()
                    && nursery.owner == Some(owner)
            })
            .map(|_| id)
    }

    /// The scheduler token of `owner`'s standing race wait, if any.
    pub(super) fn race_wait_of(&self, owner: FiberId) -> Option<u64> {
        self.sched
            .waits
            .iter()
            .find(|(_, entry)| {
                entry.fiber == owner && matches!(entry.wait, WorldWait::Timeout { .. })
            })
            .map(|(token, _)| *token)
    }

    /// Starts the racer of the race `owner` waits on, when it still stands
    /// ready: the first entry's step, for the caller's contained pass.
    pub(super) fn start_racer(&mut self, owner: FiberId) -> Option<(Active, CodePc)> {
        let nursery = self.race_of(owner)?;
        let racer = *self.nurseries[nursery.index()].children.first()?;
        (self.pool[racer.index()].state == FiberState::Ready)
            .then(|| self.switch_in(racer, Value::Tag(PLATFORM_NONE)))
    }

    /// The wheel's deadline door: the race `owner` waits on expired. The
    /// racer's own nursery takes the raised cancel flag — the standard
    /// sentinel observation point cuts a looping racer at its next
    /// backward branch — and a racer the scheduler meets parked is cut
    /// inline here; its cancel death resolves the race and the owner
    /// takes the failure span at the parked call. The wait finishes
    /// here only when no racer stood to resolve it. The wheel fires
    /// only between the racer's yields; a racer that never parks
    /// answers the deadline through the interpreter's back-edge
    /// deadline poll, which raises the same marks at the edge.
    pub(super) fn expire_race(&mut self, token: u64, owner: FiberId) {
        if let Some(nursery) = self.race_of(owner) {
            let slot = nursery.index();
            self.nurseries[slot].cancelled = true;
            if let Some(race) = self.nurseries[slot].race.as_mut() {
                race.expired = true;
            }
            if let Some(&racer) = self.nurseries[slot].children.first() {
                self.cut_race_racer(racer);
            }
        }
        if self.sched.waits.contains_key(&token) {
            self.finish_wait_with(token, Wake::Answer(timeout_failure_span()));
        }
    }

    /// Cuts the racer where the scheduler meets it — a park, never a run.
    /// Every nursery the racer owns raises its cancel flag first, so a
    /// drive standing over the racer's own block cuts that block's
    /// children instead of running them on past the deadline. The
    /// racer's waits drop, then the cancel cut runs on its own context:
    /// the kept frames' hooks fire, the claims drain, the fiber dies
    /// cancelled. A live resumer re-parks with the sentinel and enters
    /// through the ordinary wake.
    pub(super) fn cut_race_racer(&mut self, racer: FiberId) {
        for nursery in &mut self.nurseries {
            if nursery.state == NurseryState::Open && nursery.owner == Some(racer) {
                nursery.cancelled = true;
            }
        }
        let fiber = &self.pool[racer.index()];
        let parked = matches!(
            fiber.state,
            FiberState::Ready
                | FiberState::Suspended
                | FiberState::Preempted
                | FiberState::WorldParked
        ) && fiber.resume.is_some();
        if !parked {
            return;
        }
        self.cancel_wait(racer);
        self.sched.take_ready(racer);
        self.switch_in(racer, Value::Tag(PLATFORM_NONE));
        if let Some((active, pc)) = self.cancel_current() {
            let resumer = self.current;
            let fiber = &mut self.pool[resumer.index()];
            fiber.resume = Some(ResumePoint {
                version: active.version,
                func: active.func,
                pc,
            });
            fiber.state = FiberState::Suspended;
            self.sched.deliveries.insert(
                resumer,
                Wake::Answer(NativeResult::Word(Value::Tag(PLATFORM_CANCELLED))),
            );
            self.sched.push_ready(resumer);
        }
        self.host_release();
    }

    /// A racer's death resolves its race: the slot closes, and the owner
    /// takes the outcome at its parked call — the racer's whole answer
    /// span under the ok discriminant (one payload word the two-word
    /// shape, a wider struct or tuple or union span the span channel's,
    /// every `Ref` word's claim crossing to the owner, the counts the
    /// racer's death left in flight), the failure span with the
    /// `timed_out` class tag once a deadline door expired the race, or
    /// the racer's own trap re-raised on the owner the way a nursery
    /// boundary re-raises a child's. An unexpired cancel ends a race its
    /// owner's death already closed: nothing is delivered, the user
    /// cancel keeping its own `NurseryCancelled` member.
    pub(super) fn resolve_race(
        &mut self,
        nursery: NurseryId,
        racer: FiberId,
        outcome: &ChildOutcome,
    ) {
        let slot = nursery.index();
        let expired = self.nurseries[slot]
            .race
            .as_ref()
            .is_some_and(|race| race.expired);
        let Some(owner) = self.nurseries[slot].owner else {
            self.close_race(slot);
            return;
        };
        let wake = match outcome {
            ChildOutcome::Completed(span) => {
                for word in span.as_slice() {
                    if let Value::Ref(word) = word {
                        self.claims[owner.index()].acquire(*word);
                    }
                }
                let wake = match span.as_slice() {
                    [word] => NativeResult::Tag {
                        tag: PLATFORM_IO_OK,
                        payload: *word,
                    },
                    words => NativeResult::Span {
                        tag: PLATFORM_IO_OK,
                        payload: words.to_vec(),
                    },
                };
                Some(Wake::Answer(wake))
            },
            ChildOutcome::Cancelled if expired => Some(Wake::Answer(timeout_failure_span())),
            ChildOutcome::Trapped(trap) => {
                // the racer's kept frames die here: the hooks fire, and a
                // cancel payload's `Ref` words cross to the owner — the
                // re-raised trap's delivery lands their counts at the
                // owner's own boundary
                let held = Self::payload_words(outcome);
                self.child_death_walk(racer, &held, Some(owner));
                Some(Wake::Fault(trap.clone()))
            },
            ChildOutcome::Cancelled => None,
        };
        self.close_race(slot);
        if let Some(wake) = wake {
            self.deliver_race(owner, wake);
        }
    }

    /// Closes a resolved race's slot: no drive ever stands over it, so
    /// the consumed shape is the release door's own.
    pub(super) fn close_race(&mut self, slot: usize) {
        let nursery = &mut self.nurseries[slot];
        nursery.driving = false;
        nursery.state = NurseryState::Dead;
        nursery.outcomes.clear();
        nursery.race = None;
    }

    /// Hands a resolved race's outcome to its owner: the parked call's
    /// wait ends with the wake. A wait already gone (the owner's own cut
    /// or release took it) drops the delivery — the words' claims then
    /// die at the owner's own drain door while it still lives, or here
    /// through the walk's no-ledger dec once it is dead.
    pub(super) fn deliver_race(&mut self, owner: FiberId, wake: Wake) {
        if let Some(token) = self.race_wait_of(owner) {
            self.finish_wait_with(token, wake);
            return;
        }
        if self.pool[owner.index()].state != FiberState::Dead {
            return;
        }
        let words: Vec<RefId> = match &wake {
            Wake::Answer(
                NativeResult::Word(Value::Ref(word))
                | NativeResult::Tag {
                    payload: Value::Ref(word),
                    ..
                },
            ) => vec![*word],
            Wake::Answer(NativeResult::Span { payload, .. }) => payload
                .iter()
                .filter_map(|value| match value {
                    Value::Ref(word) => Some(*word),
                    _ => None,
                })
                .collect(),
            Wake::Fault(Trap::NurseryCancelled { payload }) => payload
                .iter()
                .filter_map(|value| match value {
                    Value::Ref(word) => Some(*word),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        for word in words {
            self.claims[owner.index()].release(word);
            let _ = self.heaps_walk().dec_walk(word);
        }
    }

    /// What the death of `fiber` ends: the race it owned. The racer
    /// releases cold — the hooks of the frames it kept fire, no sentinel,
    /// exactly an abandoned nursery's children at a release door — and a
    /// race the racer itself owned folds in through the same door.
    pub(super) fn end_race_of(&mut self, fiber: FiberId) {
        if let Some(race) = self.race_of(fiber) {
            self.tear_down_nursery(race.index());
        }
    }
}
