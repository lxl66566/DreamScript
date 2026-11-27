//! The nursery registry and the join/teardown drive: the interlock stack that
//! drives children to death in spawn order, delivers each answer onto its parked
//! joiner, and routes the fork-join seams to the worker pool or the sequential
//! assembly.

use super::*;

impl Vm {
    // -- Nurseries ---------------------------------------------
    //
    // Determinism: spawn order drives everything observable — the join and
    // teardown drivers enter children in spawn order, one to death at a
    // time, and outcomes report in spawn order. A host-visible yield under
    // a drive is answered with the bare PLATFORM_NONE bounce tag (the
    // run-entry convention); a child's own resumer links stay live.

    /// Opens (or returns the live) root nursery: the explicit root a
    /// bare host spawn routes into. With none live, the host itself is the
    /// supervisor and bare spawns detach.
    pub fn open_root_nursery(&mut self) -> NurseryId {
        if let Some(id) = self.live_root_nursery() {
            return id;
        }
        let id = self.open_fresh_nursery();
        // the host supervises the root: no fiber owns it, so no release
        // door ever tears it down
        self.nurseries[id.index()].owner = None;
        self.root_nursery = Some(id);
        id
    }

    /// Opens a fresh nursery: the in-language `nursery` block's own
    /// (never the root — bare host spawns route there alone), and the
    /// fork-join combinator's own over the same reuse discipline. Dead
    /// slots reuse like fiber slots. The opening fiber owns it — the link
    /// a trapped owner's release follows to the nursery its abandoned
    /// block left open.
    pub(crate) fn open_fresh_nursery(&mut self) -> NurseryId {
        let owner = Some(self.current);
        let slot = match self
            .nurseries
            .iter()
            .position(|n| n.state == NurseryState::Dead)
        {
            Some(at) => at,
            None => self.nurseries.len(),
        };
        if slot == self.nurseries.len() {
            self.nurseries.push(Nursery::open(owner));
        } else {
            self.nurseries[slot] = Nursery::open(owner);
        }
        NurseryId(u32::try_from(slot).expect("nursery pool fits u32 ids"))
    }

    /// The root nursery while it is open.
    pub(super) fn live_root_nursery(&self) -> Option<NurseryId> {
        self.root_nursery.filter(|&n| {
            self.nurseries
                .get(n.index())
                .is_some_and(|nr| nr.state == NurseryState::Open)
        })
    }

    /// Whether `nursery` names an open nursery.
    pub(super) fn nursery_open(&self, nursery: NurseryId) -> Result<(), Trap> {
        if self
            .nurseries
            .get(nursery.index())
            .is_some_and(|n| n.state == NurseryState::Open)
        {
            Ok(())
        } else {
            Err(Trap::UnknownCallee {
                module: "nursery".into(),
                func: nursery.0,
            })
        }
    }

    /// Joins a nursery: drives every child to death in spawn order —
    /// a child dying trapped raises the cancel flag for the rest —
    /// then reports the boundary result and consumes the nursery.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownCallee`] with the virtual module `nursery` when
    /// `nursery` names no open nursery (absent, or already consumed).
    pub fn join_nursery(&mut self, nursery: NurseryId) -> Result<JoinOutcome, Trap> {
        self.nursery_open(nursery)?;
        let outcomes = self.face_drive(nursery, false);
        Ok(
            // only an all-Completed record joins; a synthesized Cancelled
            // with no Trapped sibling (an interrupted drive) is a boundary
            // failure the same way a trap is
            if outcomes
                .iter()
                .all(|o| matches!(o, ChildOutcome::Completed(_)))
            {
                JoinOutcome::Joined(
                    outcomes
                        .into_iter()
                        .map(|o| match o {
                            ChildOutcome::Completed(span) => span,
                            // the all-Completed test took every other shape
                            // to the Failed branch
                            ChildOutcome::Cancelled | ChildOutcome::Trapped(_) => {
                                unreachable!("a joined child completed")
                            },
                        })
                        .collect(),
                )
            } else {
                JoinOutcome::Failed(outcomes)
            },
        )
    }

    /// Tears a nursery down: raises the cancel flag, drives every
    /// live child to death (straight-line work lands, looping work stops
    /// at its next backward branch), then consumes the nursery and reports
    /// the per-child outcomes. The owner-death panic path will call this;
    /// the per-child death walk is where the future deinit pass hooks in.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownCallee`] with the virtual module `nursery` when
    /// `nursery` names no open nursery.
    pub fn teardown_nursery(&mut self, nursery: NurseryId) -> Result<Vec<ChildOutcome>, Trap> {
        self.nursery_open(nursery)?;
        Ok(self.face_drive(nursery, true))
    }

    /// A nursery's lifecycle state, for debug and test assertions.
    #[must_use]
    pub fn nursery_state(&self, nursery: NurseryId) -> Option<NurseryState> {
        self.nurseries.get(nursery.index()).map(|n| n.state)
    }

    /// Whether a nursery's cancel flag is raised, for debug and test
    /// assertions.
    #[must_use]
    pub fn nursery_cancelled(&self, nursery: NurseryId) -> Option<bool> {
        self.nurseries.get(nursery.index()).map(|n| n.cancelled)
    }

    /// The host face's drive (`join_nursery`/`teardown_nursery`): the
    /// nursery consumed with no in-language call site to answer. A child's
    /// own cross-nursery join pushes frames the same way, so the face
    /// drives through the identical interlock stack.
    fn face_drive(&mut self, nursery: NurseryId, cancel_first: bool) -> Vec<ChildOutcome> {
        if cancel_first {
            self.nurseries[nursery.index()].cancelled = true;
        }
        let frame = self.push_drive_frame(nursery, None, DriveSeam::Face);
        match self.nursery_drive_run(frame) {
            NurseryDriveEnd::Face(outcomes) => outcomes,
            _ => unreachable!("a face frame consumes without a delivery"),
        }
    }

    /// Tears a fork combinator's nursery down for a refused park or
    /// spawn (the cancel discipline): the face drive runs the
    /// children under the raised flag — a straight-line child's first
    /// entry still lands — and the calling fiber's context, which the
    /// first child switch stashed into its pool slot, restores (the
    /// combinator body continues on its own fiber with the refusal
    /// about to surface). A completed answer the caller abandons (a
    /// landed-under-the-flag word) keeps its claim row until the
    /// receiver's own drain door.
    pub(crate) fn teardown_fork_nursery(
        &mut self,
        nursery: NurseryId,
    ) -> Result<Vec<ChildOutcome>, Trap> {
        let host = self.current;
        let outcomes = self.teardown_nursery(nursery)?;
        for outcome in &outcomes {
            if let ChildOutcome::Completed(span) = outcome {
                for word in span.as_slice() {
                    if let Value::Ref(word) = word {
                        self.claims[host.index()].acquire(*word);
                    }
                }
            }
        }
        self.restore_context(host);
        Ok(outcomes)
    }

    /// Swaps `target`'s context back in after a face drive left no
    /// context live: the stash half of [`Vm::switch_in`] without the
    /// resume-point hand-shake (the fiber never parked — it hosts the
    /// native body that drove).
    fn restore_context(&mut self, target: FiberId) {
        self.host_release();
        let fiber = &mut self.pool[target.index()];
        std::mem::swap(&mut self.stack, &mut fiber.segment);
        std::mem::swap(&mut self.callinfo, &mut fiber.callinfo);
        std::mem::swap(&mut self.objects, &mut fiber.objects);
        self.current = target;
        self.ctx_live = true;
    }

    /// Snapshots a nursery into a drive frame and marks it in flight: the
    /// spawn-order children, the cursor of the child being driven, and who
    /// to answer at consumption. The mark refuses a second joiner while the
    /// drive stands.
    fn push_drive_frame(
        &mut self,
        nursery: NurseryId,
        joiner: Option<(FiberId, CodePc)>,
        seam: DriveSeam,
    ) -> NurseryDrive {
        debug_assert!(
            !self.nurseries[nursery.index()].driving,
            "one drive of a nursery stands at a time"
        );
        self.nurseries[nursery.index()].driving = true;
        NurseryDrive {
            children: self.nurseries[nursery.index()].children.clone(),
            nursery,
            cursor: 0,
            joiner,
            seam,
        }
    }

    /// Runs one drive segment of the join/teardown driver until it goes
    /// quiet (the fiber dies, parks at the host, or traps) or parks at a
    /// cross-nursery join — the request bubbles to the interlock stack. A
    /// trap escaping the drive is attributed to the fiber the drive ended
    /// on (a child that resumed another fiber parks at its resume call
    /// while the resumee traps, and that parked child is innocent); the
    /// caller's loop then re-validates the driven child: itself trapped →
    /// interrupted (already ledgered, the guard breaks); parked → bounced
    /// back in, or cut at that resume when the flag is up.
    fn drive_step(&mut self, active: Active, pc: CodePc) -> StepFlow {
        match self.execute::<false>(active, pc, usize::MAX) {
            Ok(DriveOutcome::Died(_) | DriveOutcome::Yielded(_)) => StepFlow::Quiet,
            Ok(DriveOutcome::Frame(_)) => unreachable!("a host drive answers no reentry frame"),
            Ok(DriveOutcome::Interlock(req)) => StepFlow::Interlock(req),
            // a child parked on a world op: the drive registers the
            // wait and moves on — the next child, or the reactor's
            // wait when every child stands parked (the pass loop in
            // `drive_frame_children`)
            Ok(DriveOutcome::WorldPark(req)) => {
                if let ParkKind::World(wait) = req.kind {
                    self.register_wait(req.fiber, wait);
                }
                StepFlow::Quiet
            },
            Err((trap, _at)) => {
                self.host_release();
                self.trap_child(self.current, &trap);
                StepFlow::Quiet
            },
        }
    }

    /// The worker-pool half of the fork drive: eligible frames are exactly
    /// a `par_map` seam (`par_reduce`'s fold chains one child
    /// per step — the left data dependency — and every other seam keeps
    /// the sequential drive) whose children all still belong to the
    /// nursery and all pass the scalar sendability gate — a `Ref` word in
    /// an entry frame names a heap a worker cannot route, so one such
    /// child sends the whole frame back to the sequential interlock,
    /// byte-identical by construction. The eligible children's fibers and
    /// ledgers leave the pool (the slots vacated) for a youpipe scoped
    /// drive over the code snapshot, and the homecoming runs the
    /// sequential drive's own tails in spawn order: the death walk for a
    /// fiber the worker left non-Dead (a trapped child keeps its frames),
    /// the nursery's ledger write, and the first-failure flag. The seam's
    /// consumption after the frame pops is untouched — the same ledger
    /// feeds it, so the assembly, deliveries, and count crossings are
    /// the sequential drive's own.
    fn try_fork_pool(&mut self, frame: &mut NurseryDrive) -> bool {
        if !matches!(
            frame.seam,
            DriveSeam::Fork(crate::native::ForkStage::Map { .. })
        ) || frame.children.len() < 2
            || !frame.children.iter().all(|&child| {
                self.pool[child.index()].nursery == Some(frame.nursery)
                    && crate::forkpool::poolable(&self.pool[child.index()])
            })
        {
            return false;
        }
        let nursery = frame.nursery;
        self.fork_pool_drives += 1;
        let mut cargos = Vec::with_capacity(frame.children.len());
        for &child in &frame.children {
            let fiber = std::mem::replace(&mut self.pool[child.index()], Fiber::empty(child));
            let claims = std::mem::replace(&mut self.claims[child.index()], ClaimTable::new(child));
            cargos.push(crate::forkpool::ForkCargo {
                fiber,
                claims,
                child,
            });
        }
        let code = self.registry.code_snapshot();
        let drive = crate::forkpool::next_drive_id();
        let limit = self.limit;
        let returns = youpipe::scope(|s| {
            s.pipe(cargos)
                .map(|cargo| crate::forkpool::exec_cargo(cargo, drive, &code, limit))
                .collect()
        });
        // the homecoming's first half, in spawn order (the scoped collect
        // is index-addressed): every fiber back in its own slot with its
        // ledger and its nursery link restored, before any count routes
        // through the pool
        let mut ledger = Vec::with_capacity(returns.len());
        for ret in returns {
            let child = ret.child;
            let mut fiber = ret.fiber;
            fiber.nursery = Some(nursery);
            self.pool[child.index()] = fiber;
            self.claims[child.index()] = ret.claims;
            ledger.push((child, ret.outcome));
        }
        // the sequential drive's own tails, in spawn order: the death
        // walk for a fiber the worker left non-Dead (a trapped child
        // keeps its frames — hooks first, then the claim drain around
        // the held payload, its claims crossing to the joiner), the
        // ledger write, and the first-failure flag
        let joiner = frame.joiner.map(|(fiber, _)| fiber);
        for (child, outcome) in ledger {
            if self.pool[child.index()].state != FiberState::Dead {
                let held = outcome
                    .as_ref()
                    .map(Self::payload_words)
                    .unwrap_or_default();
                self.child_death_walk(child, &held, joiner);
            }
            let outcome = outcome.unwrap_or(ChildOutcome::Cancelled);
            if matches!(outcome, ChildOutcome::Trapped(_)) {
                self.nurseries[nursery.index()].cancelled = true;
            }
            self.nurseries[nursery.index()].record(child, outcome);
        }
        frame.cursor = frame.children.len();
        true
    }

    /// The `Ref` words a trapped outcome's cancel payload carries — the
    /// words that ride the trap's delivery past a death walk, their counts
    /// held back from the drain and their claims routed to the joiner.
    pub(super) fn payload_words(outcome: &ChildOutcome) -> Vec<RefId> {
        match outcome {
            ChildOutcome::Trapped(Trap::NurseryCancelled { payload }) => payload
                .iter()
                .filter_map(|value| match value {
                    Value::Ref(word) => Some(*word),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The trapped-child death walk every drive's tail shares: the
    /// kept frames' deinit pass with the payload words held back, the
    /// claim drain around them, each held word's claim crossing to the
    /// joiner (the trap's delivery lands its counts there), and the Dead
    /// marking.
    pub(super) fn child_death_walk(
        &mut self,
        child: FiberId,
        held: &[RefId],
        joiner: Option<FiberId>,
    ) {
        self.teardown_fiber(child, held);
        for &word in held {
            self.claims[child.index()].release(word);
            if let Some(target) = joiner {
                self.claims[target.index()].acquire(word);
            }
        }
        let fiber = &mut self.pool[child.index()];
        fiber.state = FiberState::Dead;
        fiber.resume = None;
    }

    /// Drives one frame's remaining children in spawn order, each to death
    /// before the next (sequential — the deterministic choice). Every
    /// death path ledgers its outcome into the nursery whoever ran it, so
    /// this loop only drives and collects. A parked child of a cancelled
    /// (or discontinuing) nursery is cut at its next resume instead of
    /// being bounced back in; a trapped child ledgers `Trapped`
    /// and raises the flag, and the walk that marks such an interrupted
    /// child dead below is the deinit pass's hook point (teardown walks
    /// frames). A child parking at a cross-nursery join
    /// parks and returns the request — the frame pauses on that child
    /// until the pushed frame's answer re-enters it.
    fn drive_frame_children(&mut self, frame: &mut NurseryDrive) -> Option<InterlockRequest> {
        if self.fork_pool && self.try_fork_pool(frame) {
            return None;
        }
        // The pass loop: one sweep drives every child it can (in spawn
        // order, each to death or a park), and a sweep that ends with
        // live children left — all standing world-parked — waits on the reactor before the next
        // sweep. Once a sibling parks on a world op the relative order
        // of sibling observables is unspecified, so re-sweeping
        // in spawn order per wake is one fair interleaving of many.
        loop {
            while frame.cursor < frame.children.len() {
                let child = frame.children[frame.cursor];
                // a reused slot belongs to another nursery: the child died and
                // was ledgered before its slot was recycled
                if self.pool[child.index()].nursery != Some(frame.nursery) {
                    frame.cursor += 1;
                    continue;
                }
                // a dead child's sweep slot (its death walk already
                // ran on the sweep that drove it)
                if self.pool[child.index()].state == FiberState::Dead {
                    frame.cursor += 1;
                    continue;
                }
                while self.pool[child.index()].state != FiberState::Dead {
                    let state = self.pool[child.index()].state;
                    let cancelled = self.nurseries[frame.nursery.index()].cancelled;
                    // A woken child's entry delivers the completed op's
                    // staged answer at its parked site (the scheduler's
                    // wake), exactly the join answer delivery's shape.
                    //
                    // Otherwise the resume is preemption-shaped:
                    // a child parked at a suspension it chose (a control
                    // call) or one the host's request injected sits at a
                    // park, so the bounce under the nursery's raised
                    // cancel flag delivers the cut instead of resuming —
                    // the driver-side twin of the backward-branch
                    // preemption point, and the only bound on a child that
                    // parks with no taken backward jump left between parks
                    // (a park inside a tail loop would otherwise bounce
                    // forever; the CALLT pc-reset is deliberately not a
                    // preemption point). A world-parked child — waiting or
                    // woken — under the flag is cut the same way, its
                    // reactor interest deregistered first (the parked-IO
                    // cancellation: the sentinel at the
                    // resume-shaped point, the reactor never reporting
                    // into a cut fiber). A Ready child is a first entry,
                    // not a resume: its straight-line work still lands.
                    let step = if cancelled
                        && matches!(
                            state,
                            FiberState::Suspended | FiberState::Preempted | FiberState::WorldParked
                        ) {
                        // enter, then die at the resume point: cancel_current
                        // fires the kept-frame deinits (the cancel-cut door)
                        // and delivers the sentinel to the child's resumer,
                        // whose continuation the drive then runs
                        self.cancel_wait(child);
                        self.switch_in(child, Value::Tag(PLATFORM_NONE));
                        self.cancel_current()
                    } else if state == FiberState::WorldParked {
                        // a parked timeout's racer runs under the
                        // contained pass before the sweep waits on the
                        // world: the owner's answer is the racer's
                        match self.start_racer(child) {
                            Some(step) => {
                                self.drive_swept(step);
                                continue;
                            },
                            None => break, // waiting on the world, a later sweep owns it
                        }
                    } else if !self.pool[child.index()].resumable() {
                        break; // interrupted mid-trap (already ledgered)
                    } else if self.sched.deliveries.contains_key(&child) {
                        // the sweep enters the woken child itself: its
                        // run-queue entry goes with it (a stale entry
                        // would answer the executor's wait forever)
                        self.sched.take_ready(child);
                        match self.enter_woken(child) {
                            Ok(Woken::Step(step)) => Some(step),
                            // a completed-or-cut woken child: the ledger
                            // or the cut already wrote its outcome, the
                            // sweep's death check below finishes it
                            Ok(Woken::Completed(_) | Woken::Quiet) => None,
                            Err(trap) => {
                                self.host_release();
                                self.trap_child(child, &trap);
                                None
                            },
                        }
                    } else {
                        Some(self.switch_in(child, Value::Tag(PLATFORM_NONE)))
                    };
                    let Some((active, pc)) = step else {
                        continue;
                    };
                    if let StepFlow::Interlock(req) = self.drive_step(active, pc) {
                        return Some(req);
                    }
                }
                // the child's death walk completes here: a trapped child keeps
                // its frames until now, then its slot is released. The deinit
                // pass fires the kept frames' hooks first (teardown walks frames
                // running deinits), holding the delivery's payload
                // back; then every other claim the kept frames own is
                // released, routed to each word's own heap (the one
                // cross-heap count channel of a death). A
                // world-parked child skips the walk — its frames still
                // stand for the wake.
                match self.pool[child.index()].state {
                    FiberState::Dead | FiberState::WorldParked => {},
                    _ => {
                        let held = self.nurseries[frame.nursery.index()]
                            .outcome(child)
                            .as_ref()
                            .map(Self::payload_words)
                            .unwrap_or_default();
                        self.child_death_walk(child, &held, frame.joiner.map(|(fiber, _)| fiber));
                    },
                }
                frame.cursor += 1;
            }
            // the racers the sweep owes the executor run here, before the
            // wait: a racer that landed answers its owner, and a woken
            // owner keeps the frame open without a reactor wait
            self.drive_frame_racers();
            // the sweep is done: waiting children keep the frame open.
            // Under the raised cancel flag the next sweep cuts them —
            // no reactor wait stands between a first failure and its
            // siblings' cut; otherwise the reactor owns the gap (a stall
            // guard failure raises the flag, the honest degenerate). A
            // woken sibling counts too: a channel send mid-pass wakes a
            // child the cursor already passed, so a staged delivery
            // keeps the frame open for the next sweep's entry instead
            // of consuming the nursery out from under a live child
            let waiting = frame.children.iter().any(|&child| {
                self.pool[child.index()].nursery == Some(frame.nursery)
                    && (self.pool[child.index()].state == FiberState::WorldParked
                        || self.sched.deliveries.contains_key(&child))
            });
            if !waiting {
                return None;
            }
            if !self.nurseries[frame.nursery.index()].cancelled && self.wait_world(None).is_err() {
                self.nurseries[frame.nursery.index()].cancelled = true;
            }
            frame.cursor = 0;
        }
    }

    /// The racers a block's sweep drives in the executor's stead: no
    /// executor loop runs while a join drive stands, so a racer that
    /// woke (its own sleep, a channel hand-off) and a ready racer whose
    /// owner parked inside this drive run here — contained exactly like
    /// the executor's own owned children, their owners being the frame's
    /// parked children or racers parked under them.
    fn drive_frame_racers(&mut self) {
        loop {
            let woken = self
                .sched
                .run_queue
                .iter()
                .copied()
                .find(|&fiber| self.is_racer(fiber) && self.owned_parked_child(fiber));
            let step = match woken {
                Some(racer) => {
                    self.sched.take_ready(racer);
                    match self.enter_woken(racer) {
                        Ok(Woken::Step(step)) => Some(step),
                        Ok(Woken::Completed(_) | Woken::Quiet) => None,
                        Err(trap) => {
                            self.host_release();
                            self.trap_child(racer, &trap);
                            None
                        },
                    }
                },
                None => self
                    .ready_racer()
                    .map(|racer| self.switch_in(racer, Value::Tag(PLATFORM_NONE))),
            };
            if let Some(step) = step {
                self.drive_swept(step);
            } else {
                return;
            }
        }
    }

    /// The contained pass over a fiber a sweep entered itself (a racer):
    /// no host drive stands over a sweep, so the pass keeps every
    /// outcome — the unreachable `Err` ledgers like a contained child's.
    fn drive_swept(&mut self, step: (Active, CodePc)) {
        if let Err(trap) = self.drive_owned_child(step, None) {
            self.host_release();
            self.trap_child(self.current, &trap);
        }
    }

    /// Whether `fiber` runs as the racer of an open race.
    fn is_racer(&self, fiber: FiberId) -> bool {
        self.pool[fiber.index()]
            .nursery
            .is_some_and(|id| self.nurseries[id.index()].race.is_some())
    }

    /// The next racer still waiting for its first entry, its owner a live
    /// parked fiber — the eager start's scan over the races alone.
    fn ready_racer(&self) -> Option<FiberId> {
        self.nurseries
            .iter()
            .enumerate()
            .filter(|(_, nursery)| nursery.state == NurseryState::Open && nursery.race.is_some())
            .filter(|(_, nursery)| self.owner_parked(nursery))
            .find_map(|(slot, nursery)| {
                let id = NurseryId(u32::try_from(slot).expect("nursery pool fits u32 ids"));
                nursery
                    .children
                    .first()
                    .filter(|&&racer| {
                        self.pool[racer.index()].nursery == Some(id)
                            && self.pool[racer.index()].state == FiberState::Ready
                    })
                    .copied()
            })
    }

    /// Consumes a driven nursery — the in-flight mark down, the slot Dead,
    /// the ledger drained — and computes the delivery its seam owes the
    /// parked joiner. A child cut down before any ledgered death (an
    /// interrupted drive, an exotic switch tangle) reports as a boundary
    /// cancellation — a structured outcome, never a panic.
    /// The consumption prologue every seam shares: the drive's live
    /// context stashed, the in-flight mark down, the slot Dead, the
    /// ledger drained.
    fn consume_nursery(&mut self, nursery: NurseryId) -> Vec<ChildOutcome> {
        self.host_release();
        let slot = &mut self.nurseries[nursery.index()];
        slot.driving = false;
        slot.state = NurseryState::Dead;
        slot.outcomes
            .drain(..)
            .map(|o| o.unwrap_or(ChildOutcome::Cancelled))
            .collect()
    }

    fn consume_frame(&mut self, frame: &NurseryDrive) -> (Vec<ChildOutcome>, Option<Delivery>) {
        let outcomes = self.consume_nursery(frame.nursery);
        // A boundary seam about to deliver its collected answers keeps
        // every completed span for the channel: the join answers list
        // below adopts the words instead of the walk. Every other
        // no-answer case — the count seam, a failed boundary (first
        // failure, interrupted drive), or a joiner that moved on before
        // consumption — decs each discarded span's `Ref` words once
        // through the death walk's no-ledger door (the child's claim row
        // released at its death, the count left in flight for a receiver
        // that never comes), so the word's unique death still runs — its
        // free cascade, deinit claims included. The face seam's outcomes
        // cross to the host driver and the fork seam's stage adopts its
        // own words, so neither walks here.
        let boundary_collects = match (
            &frame.seam,
            outcomes
                .iter()
                .find(|o| matches!(o, ChildOutcome::Trapped(_) | ChildOutcome::Cancelled)),
        ) {
            // a clean join with a parked joiner answers the collected
            // list; a cancelled outcome with no trapped one cannot stand
            // (the flag only rises at a trapped death), but the scan
            // stays honest about it
            (DriveSeam::Boundary { .. }, None) => frame
                .joiner
                .is_some_and(|(joiner, resume_pc)| self.joiner_parked(joiner, resume_pc)),
            _ => false,
        };
        if matches!(frame.seam, DriveSeam::Count | DriveSeam::Boundary { .. }) && !boundary_collects
        {
            for outcome in &outcomes {
                if let ChildOutcome::Completed(span) = outcome {
                    for word in span.as_slice() {
                        if let Value::Ref(word) = word {
                            let _ = self.heaps_walk().dec_walk(*word);
                        }
                    }
                }
            }
        }
        // the first failure in spawn-drive order decides: a cancelled child
        // with no trapped one before it cannot happen (the flag only rises
        // at a trapped death)
        let delivery = match &frame.seam {
            // the fork seam ends through [`Vm::fork_frame_end`] in the
            // drive loop, which moves the stage out of the frame
            DriveSeam::Fork(_) => unreachable!("the fork seam ends through fork_frame_end"),
            DriveSeam::Face => None,
            DriveSeam::Count => {
                let failed = outcomes
                    .iter()
                    .filter(|o| matches!(o, ChildOutcome::Trapped(_)))
                    .count();
                let count = Value::I64(i64::try_from(failed).expect("child count fits i64"));
                Some(Delivery::Answer {
                    call: count,
                    span: None,
                    gone: count,
                })
            },
            DriveSeam::Boundary { mask } => {
                match outcomes
                    .iter()
                    .find(|o| matches!(o, ChildOutcome::Trapped(_)))
                {
                    None => {
                        if boundary_collects {
                            // The join channel: the children's answer spans
                            // collect into one list in spawn order
                            // — the joiner's own heap owns the list, the
                            // delivered words' claim rows acquire on the
                            // joiner (the fork stage's crossing), each
                            // push adopts the span as one element, and
                            // the in-flight count dies behind it. The
                            // whole-span element discipline is the fork
                            // seam's: the first push fixes the stride, a
                            // mismatching width traps before any count
                            // moves.
                            let (joiner_id, _) =
                                frame.joiner.expect("a collecting boundary parks a joiner");
                            // The joiner's store owns the collected list:
                            // swap it live for the allocation (the
                            // consumption prologue stashed every live
                            // context back into the pool), the fresh
                            // word's claim row acquiring on the joiner's
                            // ledger exactly where the fork stage's
                            // pre-park allocation did on its caller's.
                            let result = {
                                std::mem::swap(
                                    &mut self.objects,
                                    &mut self.pool[joiner_id.index()].objects,
                                );
                                let mut heaps = Heaps::new(
                                    joiner_id,
                                    &mut self.objects,
                                    &mut self.pool,
                                    &mut self.claims,
                                    true,
                                );
                                let word = heaps.new_list_id(Vec::new());
                                std::mem::swap(
                                    &mut self.objects,
                                    &mut self.pool[joiner_id.index()].objects,
                                );
                                word
                            };
                            for outcome in &outcomes {
                                let ChildOutcome::Completed(answer) = outcome else {
                                    continue;
                                };
                                for word in answer.as_slice() {
                                    if let Value::Ref(word) = word {
                                        self.claims[joiner_id.index()].acquire(*word);
                                    }
                                }
                                let mut heaps = self.fork_heaps(joiner_id);
                                if let Err(trap) = heaps.push_list(result, answer.as_slice()) {
                                    let _ = heaps.dec(result);
                                    return (Vec::new(), Some(Delivery::Raise(trap)));
                                }
                                for word in answer.as_slice() {
                                    if let Value::Ref(word) = word
                                        && let Err(trap) = heaps.dec(*word)
                                    {
                                        let _ = heaps.dec(result);
                                        return (Vec::new(), Some(Delivery::Raise(trap)));
                                    }
                                }
                            }
                            Some(Delivery::Answer {
                                call: Value::I64(0),
                                span: Some((NURSERY_BOUNDARY_OK, vec![Value::Ref(result)])),
                                gone: Value::Tag(NURSERY_BOUNDARY_OK),
                            })
                        } else {
                            Some(Delivery::Answer {
                                call: Value::I64(0),
                                span: Some((NURSERY_BOUNDARY_OK, vec![Value::I64(0)])),
                                gone: Value::Tag(NURSERY_BOUNDARY_OK),
                            })
                        }
                    },
                    Some(ChildOutcome::Trapped(Trap::NurseryCancelled { payload })) => {
                        // the staged mask is the caller's return-union
                        // member containment set: an error the boundary
                        // cannot union out refuses instead of corrupting
                        // the union's tag space. The member's identity is
                        // the span's word 0 — the tag the cancel site
                        // staged first
                        let contained = matches!(
                            payload.first(),
                            Some(Value::Tag(tag)) if *tag < 64 && (mask & (1i64 << *tag)) != 0
                        );
                        if contained {
                            // the gone surfacing is the member's tag word:
                            // a moved-on joiner's host drive sees the
                            // identity; the payload span's `Ref` words
                            // ride the trap delivery's host handoff
                            let gone = payload.first().copied().unwrap_or(Value::I64(0));
                            Some(Delivery::Answer {
                                call: Value::I64(0),
                                span: Some((NURSERY_BOUNDARY_ERR, payload.clone())),
                                gone,
                            })
                        } else {
                            Some(Delivery::Raise(Trap::NurseryUncontained {
                                payload: payload.clone(),
                            }))
                        }
                    },
                    Some(ChildOutcome::Trapped(trap)) => {
                        // a real trap surfaces at the boundary
                        Some(Delivery::Raise(trap.clone()))
                    },
                    Some(_) => unreachable!("the scan yields only trapped failures"),
                }
            },
        };
        (outcomes, delivery)
    }

    /// The interlock stack: drives the initial frame's children (and every
    /// frame a driven child pushes — its own cross-nursery join parks it
    /// and pushes the joined nursery, LIFO, the nesting order the
    /// recursive driver used to carry on the Rust stack), consumes each
    /// exhausted frame, and delivers its answer onto the parked joiner,
    /// whose continuation runs inside the same drain. The initial frame's
    /// delivery returns to its host instead.
    fn nursery_drive_run(&mut self, initial: NurseryDrive) -> NurseryDriveEnd {
        let mut stack = vec![initial];
        loop {
            while let Some(req) = self.drive_frame_children(
                stack
                    .last_mut()
                    .expect("the initial frame stands until its nursery consumes"),
            ) {
                stack.push(self.push_drive_frame(
                    req.nursery,
                    Some((req.joiner, req.resume_pc)),
                    req.seam,
                ));
            }
            let frame = stack
                .pop()
                .expect("the initial frame stands until its nursery consumes");
            let (outcomes, delivery) = match frame.seam {
                DriveSeam::Fork(stage) => {
                    let joiner = frame.joiner.expect("the fork seam parks a call site");
                    match self.fork_frame_end(frame.nursery, joiner, stage) {
                        ForkEnd::Chain(next) => {
                            stack.push(next);
                            continue;
                        },
                        ForkEnd::End(outcomes, delivery) => (outcomes, delivery),
                    }
                },
                _ => self.consume_frame(&frame),
            };
            let Some((joiner, resume_pc)) = frame.joiner else {
                return NurseryDriveEnd::Face(outcomes);
            };
            let Some(delivery) = delivery else {
                unreachable!("a join seam frames a delivery");
            };
            if stack.is_empty() {
                // the initial frame: the delivery belongs to the
                // interrupted host drive's fiber
                return match delivery {
                    Delivery::Answer { call, span, gone } => {
                        match self.reenter_joiner(joiner, resume_pc, call) {
                            Some((a, at)) => {
                                if let Some((tag, payload)) = span {
                                    self.overlay_boundary(&a, tag, &payload);
                                }
                                NurseryDriveEnd::Entered(a, at)
                            },
                            // no call site stands: the answer surfaces to
                            // the host
                            None => NurseryDriveEnd::Gone(gone),
                        }
                    },
                    Delivery::Words { head, tail, gone } => {
                        match self.reenter_joiner(joiner, resume_pc, head) {
                            Some((a, at)) => {
                                self.overlay_span_tail(&a, &tail);
                                NurseryDriveEnd::Entered(a, at)
                            },
                            None => NurseryDriveEnd::Gone(gone),
                        }
                    },
                    Delivery::Raise(trap) => {
                        // re-enter the joiner so the drive ends on it and
                        // the trap attributes at the boundary; the call
                        // site never reads the answer
                        let _ = self.reenter_joiner(joiner, resume_pc, Value::I64(0));
                        NurseryDriveEnd::Raised(trap, CodePc(resume_pc.0 - 1))
                    },
                };
            }
            // a nested frame: deliver onto the parked joiner and run its
            // continuation here (the recursive driver ran it inside the
            // same drive step); a further join pushes the next frame, a
            // gone joiner's answer surfaces to no one (its drive already
            // ended elsewhere)
            match delivery {
                Delivery::Answer { call, span, .. } => {
                    if let Some((a, at)) = self.reenter_joiner(joiner, resume_pc, call) {
                        if let Some((tag, payload)) = span {
                            self.overlay_boundary(&a, tag, &payload);
                        }
                        if let StepFlow::Interlock(req) = self.drive_step(a, at) {
                            stack.push(self.push_drive_frame(
                                req.nursery,
                                Some((req.joiner, req.resume_pc)),
                                req.seam,
                            ));
                        }
                    }
                },
                Delivery::Words { head, tail, .. } => {
                    if let Some((a, at)) = self.reenter_joiner(joiner, resume_pc, head) {
                        self.overlay_span_tail(&a, &tail);
                        if let StepFlow::Interlock(req) = self.drive_step(a, at) {
                            stack.push(self.push_drive_frame(
                                req.nursery,
                                Some((req.joiner, req.resume_pc)),
                                req.seam,
                            ));
                        }
                    }
                },
                Delivery::Raise(trap) => {
                    // re-enter the joiner so the drive ends on it, then
                    // the trap attributes there — the joiner's own nursery
                    // ledgers it and raises its flag
                    let _ = self.reenter_joiner(joiner, resume_pc, Value::I64(0));
                    self.host_release();
                    self.trap_child(self.current, &trap);
                },
            }
        }
    }

    /// The fork seam's router at consumption: the joiner's store and
    /// ledger reached through the pool — the drive's live context
    /// belongs to a dead child (the consumption prologue stashed it),
    /// while the parked joiner's real store sits in its own pool slot.
    fn fork_heaps(&mut self, joiner: FiberId) -> Heaps<'_> {
        Heaps::new(
            joiner,
            &mut self.objects,
            &mut self.pool,
            &mut self.claims,
            false,
        )
    }

    /// The fork seam's frame end: the consumed nursery's outcomes run
    /// through the assembly or fold stage exactly the synchronous
    /// body's post-join work did — the claims crossing first (the
    /// joiner's rows, one per `Ref` word of each completed answer: the
    /// release-side exact-ownership gate's row), the heap operations
    /// routed through the joiner's own store and ledger.
    /// `par_reduce`'s chain continues while elements remain — the next
    /// step's child spawns into its fresh nursery and the frame rides
    /// [`ForkEnd::Chain`] — the fold's left data dependency, one drive
    /// frame per step, nesting depth still heap. Any trap the stage
    /// raises (a stride refusal, a bogus word, a delivered width the
    /// checker never proved) releases the staged state and surfaces at
    /// the parked call site like a child failure.
    fn fork_frame_end(
        &mut self,
        nursery: NurseryId,
        joiner: (FiberId, CodePc),
        stage: crate::native::ForkStage,
    ) -> ForkEnd {
        let outcomes = self.consume_nursery(nursery);
        let (joiner_id, resume_pc) = joiner;
        match stage {
            crate::native::ForkStage::Map { result } => {
                for outcome in &outcomes {
                    if let ChildOutcome::Completed(answer) = outcome {
                        for word in answer.as_slice() {
                            if let Value::Ref(word) = word {
                                self.claims[joiner_id.index()].acquire(*word);
                            }
                        }
                    }
                }
                for outcome in outcomes {
                    match outcome {
                        ChildOutcome::Completed(answer) => {
                            let mut heaps = self.fork_heaps(joiner_id);
                            // the answer's whole span lands as one
                            // element: the first push fixes the result
                            // list's stride (a width mismatch traps at
                            // the stride check before any count moves,
                            // so the releases balance)
                            if let Err(trap) = heaps.push_list(result, answer.as_slice()) {
                                let _ = heaps.dec(result);
                                return ForkEnd::End(Vec::new(), Some(Delivery::Raise(trap)));
                            }
                            for word in answer.as_slice() {
                                if let Value::Ref(word) = word
                                    && let Err(trap) = heaps.dec(*word)
                                {
                                    let _ = heaps.dec(result);
                                    return ForkEnd::End(Vec::new(), Some(Delivery::Raise(trap)));
                                }
                            }
                        },
                        ChildOutcome::Trapped(trap) => {
                            let mut heaps = self.fork_heaps(joiner_id);
                            let _ = heaps.dec(result);
                            return ForkEnd::End(Vec::new(), Some(Delivery::Raise(trap)));
                        },
                        // the cancel flag only rises at a trapped death,
                        // so a cancelled outcome with no trapped one is
                        // an interrupted drive — the cut sentinel is its
                        // honest cause
                        ChildOutcome::Cancelled => {
                            let mut heaps = self.fork_heaps(joiner_id);
                            let _ = heaps.dec(result);
                            return ForkEnd::End(
                                Vec::new(),
                                Some(Delivery::Raise(Trap::NurseryCancelled {
                                    payload: vec![Value::Tag(PLATFORM_CANCELLED)],
                                })),
                            );
                        },
                    }
                }
                ForkEnd::End(
                    Vec::new(),
                    Some(Delivery::Answer {
                        call: Value::Ref(result),
                        span: None,
                        gone: Value::Ref(result),
                    }),
                )
            },
            crate::native::ForkStage::Reduce {
                elements,
                stride,
                callable,
                mut acc,
                mut owned,
                mut at,
            } => match outcomes.as_slice() {
                [ChildOutcome::Completed(answer)] => {
                    // the crossing's claim rows: the joiner owns the
                    // delivered words
                    for word in answer.as_slice() {
                        if let Value::Ref(word) = word {
                            self.claims[joiner_id.index()].acquire(*word);
                        }
                    }
                    let words = answer.as_slice();
                    if words.len() != acc.len() {
                        // the delivered span disagrees with the staged
                        // init's width — the callable the checker never
                        // proved: release the owned accumulator and the
                        // delivered words' own counts, then refuse
                        let mut heaps = self.fork_heaps(joiner_id);
                        if let Err(trap) =
                            crate::native::release_owned_span(&mut heaps, &acc, owned)
                        {
                            return ForkEnd::End(Vec::new(), Some(Delivery::Raise(trap)));
                        }
                        for word in words {
                            if let Value::Ref(id) = *word
                                && let Err(trap) = heaps.dec(id)
                            {
                                return ForkEnd::End(Vec::new(), Some(Delivery::Raise(trap)));
                            }
                        }
                        return ForkEnd::End(
                            Vec::new(),
                            Some(Delivery::Raise(Trap::ArityMismatch {
                                expected: acc.len(),
                                given: words.len(),
                            })),
                        );
                    }
                    let mut heaps = self.fork_heaps(joiner_id);
                    if let Err(trap) = crate::native::release_owned_span(&mut heaps, &acc, owned) {
                        return ForkEnd::End(Vec::new(), Some(Delivery::Raise(trap)));
                    }
                    acc.clear();
                    acc.extend_from_slice(words);
                    owned = true;
                    at += stride;
                    if at >= elements.len() {
                        // the chain's last answer: the whole span
                        // delivers onto the parked call site, the head
                        // through the switch payload, the tail past it
                        let head = acc[0];
                        let tail = acc[1..].to_vec();
                        return ForkEnd::End(
                            Vec::new(),
                            Some(Delivery::Words {
                                head,
                                tail,
                                gone: head,
                            }),
                        );
                    }
                    // the fold chain continues: the next element's child
                    // spawns with the running accumulator and rides the
                    // next frame — the left data dependency, one drive
                    // frame per step
                    let mut child_args = Vec::with_capacity(acc.len() + stride);
                    child_args.extend_from_slice(&acc);
                    child_args.extend_from_slice(&elements[at..at + stride]);
                    let nursery = self.open_fresh_nursery();
                    if let Err(trap) =
                        crate::native::fork_spawn(self, nursery, callable, &child_args)
                    {
                        // a refused spawn's child may be half-built: tear
                        // the step's nursery down under the cancel
                        // discipline, then release the owned accumulator
                        // before the trap surfaces
                        let _ = self.teardown_fork_nursery(nursery);
                        let mut heaps = self.fork_heaps(joiner_id);
                        if let Err(release) =
                            crate::native::release_owned_span(&mut heaps, &acc, true)
                        {
                            return ForkEnd::End(Vec::new(), Some(Delivery::Raise(release)));
                        }
                        return ForkEnd::End(Vec::new(), Some(Delivery::Raise(trap)));
                    }
                    ForkEnd::Chain(self.push_drive_frame(
                        nursery,
                        Some((joiner_id, resume_pc)),
                        DriveSeam::Fork(crate::native::ForkStage::Reduce {
                            elements,
                            stride,
                            callable,
                            acc,
                            owned,
                            at,
                        }),
                    ))
                },
                [ChildOutcome::Trapped(trap)] => {
                    // the chain holds no live sibling at the failure —
                    // the join consumed the step's nursery — so the
                    // trap re-raises with the owned accumulator released
                    let mut heaps = self.fork_heaps(joiner_id);
                    let _ = crate::native::release_owned_span(&mut heaps, &acc, owned);
                    ForkEnd::End(Vec::new(), Some(Delivery::Raise(trap.clone())))
                },
                [ChildOutcome::Cancelled] => {
                    // the cancel flag only rises at a trapped death, so
                    // a cancelled outcome with no trap is an interrupted
                    // drive — the cut sentinel is its honest cause
                    let mut heaps = self.fork_heaps(joiner_id);
                    let _ = crate::native::release_owned_span(&mut heaps, &acc, owned);
                    ForkEnd::End(
                        Vec::new(),
                        Some(Delivery::Raise(Trap::NurseryCancelled {
                            payload: vec![Value::Tag(PLATFORM_CANCELLED)],
                        })),
                    )
                },
                _ => unreachable!("the chain joins exactly the one spawned child"),
            },
        }
    }

    /// Drives a parked joiner's request to its answer and continues the
    /// joiner's drive under `boundary` (the interrupted host drive's
    /// reentry boundary): the nursery's frame plus every frame a driven
    /// child pushes, then the answer delivered onto the parked call site
    /// and the joiner re-entered — program nesting depth costs heap, not
    /// Rust stack. A gone joiner's answer surfaces to the host drive as
    /// its death value; a boundary raise propagates at the join call's pc.
    pub(super) fn drive_interlock<const FUELED: bool>(
        &mut self,
        req: InterlockRequest,
        boundary: usize,
    ) -> Result<DriveOutcome, (Trap, CodePc)> {
        let frame = self.push_drive_frame(req.nursery, Some((req.joiner, req.resume_pc)), req.seam);
        let (mut active, mut pc) = match self.nursery_drive_run(frame) {
            NurseryDriveEnd::Face(_) => unreachable!("a join seam frames a delivery"),
            NurseryDriveEnd::Gone(value) => return Ok(DriveOutcome::Died(FrameAnswer::one(value))),
            NurseryDriveEnd::Raised(trap, at) => return Err((trap, at)),
            NurseryDriveEnd::Entered(a, at) => (a, at),
        };
        loop {
            match self.execute::<FUELED>(active, pc, boundary) {
                Ok(DriveOutcome::Interlock(req)) => {
                    let frame = self.push_drive_frame(
                        req.nursery,
                        Some((req.joiner, req.resume_pc)),
                        req.seam,
                    );
                    match self.nursery_drive_run(frame) {
                        NurseryDriveEnd::Face(_) => unreachable!("a join seam frames a delivery"),
                        NurseryDriveEnd::Gone(value) => {
                            return Ok(DriveOutcome::Died(FrameAnswer::one(value)))
                        },
                        NurseryDriveEnd::Raised(trap, at) => return Err((trap, at)),
                        NurseryDriveEnd::Entered(a, at) => {
                            active = a;
                            pc = at;
                        },
                    }
                },
                outcome => return outcome,
            }
        }
    }

    /// Whether the open nursery `nursery`'s owner is a live parked
    /// fiber: the in-language block's joiner itself waits (a world op, a
    /// channel op, an in-language resume, a host suspension), so its
    /// children are the executor's to start. A dead owner's children
    /// release cold at the release doors without ever running.
    fn owner_parked(&self, nursery: &Nursery) -> bool {
        nursery.owner.is_some_and(|owner| {
            self.pool
                .get(owner.index())
                .is_some_and(|slot| !matches!(slot.state, FiberState::Running | FiberState::Dead))
        })
    }

    /// Whether `fiber` is a nursery child the executor owns while its
    /// joiner parks: nobody resumed it in-language, and the open nursery
    /// it belongs to has a live parked owner. The host-supervised root
    /// nursery (no owner) keeps its children for the host's explicit
    /// join drive.
    pub(super) fn owned_parked_child(&self, fiber: FiberId) -> bool {
        self.pool.get(fiber.index()).is_some_and(|slot| {
            slot.resumer.is_none()
                && slot.nursery.is_some_and(|id| {
                    self.nurseries.get(id.index()).is_some_and(|nursery| {
                        nursery.state == NurseryState::Open && self.owner_parked(nursery)
                    })
                })
        })
    }

    /// The next Ready child of an open parked-owner nursery, in nursery
    /// slot order then spawn order — the eager start's scan.
    fn eager_ready_child(&self) -> Option<FiberId> {
        for (slot, nursery) in self.nurseries.iter().enumerate() {
            if nursery.state != NurseryState::Open || !self.owner_parked(nursery) {
                continue;
            }
            let id = NurseryId(u32::try_from(slot).expect("nursery pool fits u32 ids"));
            for &child in &nursery.children {
                if self.pool[child.index()].nursery == Some(id)
                    && self.pool[child.index()].state == FiberState::Ready
                {
                    return Some(child);
                }
            }
        }
        None
    }

    /// Starts one Ready child — the first entry runs the spawn arguments,
    /// the dropped payload the switch convention — and drives it under
    /// the contained pass; the pass answers whether anything started.
    pub(super) fn eager_start(&mut self, host_fiber: FiberId) -> Result<EagerPass, Trap> {
        let Some(child) = self.eager_ready_child() else {
            return Ok(EagerPass::Idle);
        };
        let step = self.switch_in(child, Value::Tag(PLATFORM_NONE));
        match self.drive_owned_child(step, Some(host_fiber))? {
            Some(answer) => Ok(EagerPass::Host(answer)),
            None => Ok(EagerPass::Drove),
        }
    }

    /// One owned child's drive, sweep-shaped: the child's world parks
    /// register their waits, its traps ledger into its nursery and raise
    /// the flag exactly like a sweep's drive (the boundary reports them,
    /// the host drive never sees one), a cross-nursery join drains
    /// through the interlock machinery, and a death discards the drive's
    /// answer — the nursery ledger holds the span the boundary collects.
    /// The host drive answers its own fiber alone, so a child's step
    /// never leaves this pass for the host loop; `Some` answers the host
    /// fiber's death value, `Err` its own trap. `host_fiber` is `None`
    /// under a sweep that starts a racer itself: no host drive stands, so
    /// every outcome stays contained.
    pub(super) fn drive_owned_child(
        &mut self,
        step: (Active, CodePc),
        host_fiber: Option<FiberId>,
    ) -> Result<Option<FrameAnswer>, Trap> {
        let (active, pc) = step;
        match self.drive_step(active, pc) {
            StepFlow::Quiet => Ok(None),
            StepFlow::Interlock(req) => match self.drive_interlock::<false>(req, usize::MAX) {
                Ok(DriveOutcome::WorldPark(park)) => {
                    if let ParkKind::World(wait) = park.kind {
                        self.register_wait(park.fiber, wait);
                    }
                    Ok(None)
                },
                Ok(DriveOutcome::Died(answer)) => {
                    if host_fiber == Some(self.current) {
                        return Ok(Some(answer));
                    }
                    Ok(None)
                },
                Ok(DriveOutcome::Yielded(_)) => Ok(None),
                Ok(DriveOutcome::Interlock(_) | DriveOutcome::Frame(_)) => {
                    unreachable!("the interlock loop drains its own frames")
                },
                Err((trap, _at)) => {
                    if host_fiber == Some(self.current) {
                        return Err(trap);
                    }
                    self.host_release();
                    self.trap_child(self.current, &trap);
                    Ok(None)
                },
            },
        }
    }

    /// Parks the running fiber at a fork-join CALLG site: the
    /// combinator's spawn phase ran (the children stand in `nursery`),
    /// and the join runs on the interlock stack — the request's frame
    /// drives the children, runs the assembly/fold stage, and delivers
    /// the answer onto this call site, nesting depth costing the
    /// interlock's heap frames, never the Rust stack. The whole-span
    /// answer's width check runs here (the total overlay's write
    /// check): the parked overlay must hold the answer (the lowerer
    /// pre-widens; hand-assembled code refuses instead of losing the
    /// tail words at the delivery).
    pub(super) fn fork_park(
        &mut self,
        active: &Active,
        pc: CodePc,
        area: usize,
        nursery: NurseryId,
        stage: crate::native::ForkStage,
    ) -> Result<DriveOutcome, (Trap, CodePc)> {
        let resume_pc = CodePc(pc.0 + 1);
        let answer_words = stage.answer_words();
        if answer_words > area {
            // the staged children and state must not outlive the
            // refused park: tear the nursery down, release the staged
            // words, then refuse
            crate::native::fork_abandon(self, nursery, stage).map_err(|e| (e, pc))?;
            return Err((
                Trap::RetSpanTooWide {
                    span: u16::try_from(answer_words).expect("answer widths fit u16"),
                    caller_area: u16::try_from(area).expect("argument areas fit u16"),
                },
                pc,
            ));
        }
        self.park_current(active, resume_pc);
        Ok(DriveOutcome::Interlock(InterlockRequest {
            nursery,
            joiner: self.current,
            resume_pc,
            seam: DriveSeam::Fork(stage),
        }))
    }

    /// Whether the joiner a drive parked at `resume_pc` still stands
    /// parked there — the same predicate [`Self::reenter_joiner`]
    /// applies, read at consumption time so a channel that hands the
    /// joiner words (the boundary's collected answers list) builds them
    /// only for a delivery that can land.
    fn joiner_parked(&self, joiner: FiberId, resume_pc: CodePc) -> bool {
        self.pool.get(joiner.index()).is_some_and(|f| {
            f.state == FiberState::Suspended && f.resume.as_ref().is_some_and(|r| r.pc == resume_pc)
        })
    }

    /// Re-enters the joiner a join/boundary drive parked at `resume_pc`,
    /// unless a child resumed it mid-drive and it moved on — died, was
    /// cancel-cut, trapped, or re-parked at another call — in which case
    /// the boundary's answer has no call site, and the drive surfaces
    /// `value` to its own host instead of switching into a fiber with no
    /// continuation (the shrunk `nursery_joiner_reentry` repro).
    fn reenter_joiner(
        &mut self,
        joiner: FiberId,
        resume_pc: CodePc,
        value: Value,
    ) -> Option<(Active, CodePc)> {
        let parked_here = self.pool.get(joiner.index()).is_some_and(|f| {
            f.state == FiberState::Suspended && f.resume.as_ref().is_some_and(|r| r.pc == resume_pc)
        });
        if parked_here { Some(self.switch_in(joiner, value)) } else { None }
    }
}
