//! The fiber switch mechanics: spawn, suspend and resume, the context swap, and
//! the death, complete and cancel exits the dispatcher takes.

use super::*;

impl Vm {
    // -- The fiber substrate --------------------------------------
    //
    // Pool ownership invariant: the running fiber's segment, call chain,
    // and heap store live in the VM's context fields while `ctx_live`
    // holds; every other time (and for every other fiber, always) they
    // live in the pool slot. A switch is three ownership swaps — no word
    // or object is ever copied.

    /// Spawns a fiber bare: with a live root nursery
    /// ([`Vm::open_root_nursery`]) the fiber is born its child; without one
    /// the host itself is the supervisor and the fiber detaches (no
    /// child-of link — the substrate regime the fiber fixtures pin). The
    /// in-language surface rejects bare spawn outside any nursery block;
    /// that check lands with the checker/lowerer rounds.
    ///
    /// # Errors
    ///
    /// The entry checks of [`Vm::run`] verbatim: unknown function, arity,
    /// argument types, entry frame versus the stack limit.
    pub fn spawn_fiber(
        &mut self,
        module: &str,
        func: &str,
        args: &[Value],
    ) -> Result<FiberId, Trap> {
        let nursery = self.live_root_nursery();
        self.spawn_into(nursery, module, func, args)
    }

    /// Spawns a fiber into an open nursery: the child-of link is set
    /// and the nursery records the child in spawn order.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownCallee`] with the virtual module `nursery` when
    /// `nursery` names no open nursery; the entry checks of [`Vm::run`]
    /// verbatim otherwise.
    pub fn spawn_fiber_in(
        &mut self,
        nursery: NurseryId,
        module: &str,
        func: &str,
        args: &[Value],
    ) -> Result<FiberId, Trap> {
        self.nursery_open(nursery)?;
        self.spawn_into(Some(nursery), module, func, args)
    }

    /// The spawn core: one pool entry with a fresh entry frame, born ready
    /// and never started, plus the child-of bookkeeping. The first switch
    /// into it (a `fiber_resume` call or [`Vm::resume_fiber`]) runs its
    /// entry function; the pool slot of a dead fiber is reused.
    pub(super) fn spawn_into(
        &mut self,
        nursery: Option<NurseryId>,
        module: &str,
        func: &str,
        args: &[Value],
    ) -> Result<FiberId, Trap> {
        let (version, func_id) = self.resolve_entry(module, func, args)?;
        Ok(self.push_spawned(nursery, version, func_id, args))
    }

    /// Spawns a fiber from a callable word's own function: the
    /// in-language `spawn` seam, and the fork-join combinator's per-element
    /// fork (the element's whole word span as the entry arguments). The
    /// FuncRef resolves through the target module's current slot; the
    /// callable's captures become the entry arguments, so the entry checks
    /// of [`Vm::run`] apply verbatim.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownModule`] when the FuncRef's module id names no
    /// installed module; the entry checks of [`Vm::run`] otherwise.
    pub(crate) fn spawn_ref_into(
        &mut self,
        nursery: Option<NurseryId>,
        fref: FuncRef,
        args: &[Value],
    ) -> Result<FiberId, Trap> {
        let name = self
            .registry
            .module_name(fref.module())
            .ok_or(Trap::UnknownModule {
                module: fref.module(),
            })?;
        let (version, func_id) =
            self.registry
                .resolve_indexed(name, fref.func())
                .ok_or(Trap::UnknownCallee {
                    module: name.to_owned(),
                    func: fref.func(),
                })?;
        self.entry_checks(version, func_id, args)?;
        self.spawn_checked(nursery, version, func_id, args)
    }

    /// Spawns a fiber from a closure word's own body: the fork-join
    /// combinator's closure callable. The child is entered with `args`
    /// (the element's whole word span) against the closure's own body
    /// function, and the closure's capture block is staged into the
    /// entry frame's leading local slots past the parameters — exactly
    /// the CALLS copy-in discipline, so a forked lambda is one shape
    /// with a called one. Each `Ref` capture acquires one count at the
    /// copy-in, claimed for the child (the count lands on the word's
    /// own heap); the body's own exit drops balance it, and a child
    /// that never runs them releases at its drain door. The closure
    /// object keeps its own counts untouched — the callable word is the
    /// caller's borrow.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when `word` names no closure object (the
    /// callable-defaulted `Ref` spelling must hold a closure); the
    /// resolve and entry checks of [`Vm::spawn_ref_into`] verbatim;
    /// [`Trap::SlotOutOfRange`] when the capture block does not fit the
    /// frame's local slots past the parameters.
    pub(crate) fn spawn_closure_into(
        &mut self,
        nursery: Option<NurseryId>,
        word: RefId,
        args: &[Value],
    ) -> Result<FiberId, Trap> {
        let (header, captures) = self.closure_parts(word)?;
        let name = self
            .registry
            .module_name(header.module())
            .ok_or(Trap::UnknownModule {
                module: header.module(),
            })?;
        let (version, func_id) =
            self.registry
                .resolve_indexed(name, header.func())
                .ok_or(Trap::UnknownCallee {
                    module: name.to_owned(),
                    func: header.func(),
                })?;
        self.entry_checks(version, func_id, args)?;
        // the capture block lands in the frame's local slots past the
        // parameters — the CALLS range check, before the child exists
        let layout = &self.registry.pinned(version).functions()[func_id as usize].layout;
        let capture_body_len = layout.body_len();
        let end = layout.params_len() + captures.len();
        if end > capture_body_len {
            return Err(Trap::SlotOutOfRange {
                slot: u8::try_from(end - 1).unwrap_or(u8::MAX),
                frame_slots: layout.slots().len(),
            });
        }
        let child = self.spawn_checked(nursery, version, func_id, args)?;
        let handoff_owner = self.current;
        for (index, &value) in captures.iter().enumerate() {
            if let Value::Ref(word) = value {
                Self::inc_ref_split(
                    &mut self.objects,
                    &mut self.pool,
                    &mut self.claims,
                    handoff_owner,
                    self.ctx_live,
                    child,
                    word,
                )?;
            }
            let slot = u8::try_from(layout.params_len() + index)
                .expect("capture slot fits u8 after the range check");
            // the entry frame's fp is 0 and the capture slots sit below
            // the body boundary, so each write re-checks the word against
            // the slot's declared type exactly like the CALLS copy-in
            let fiber = &mut self.pool[child.index()];
            write_slot(
                &mut fiber.segment,
                0,
                slot,
                layout.slots(),
                capture_body_len,
                value,
            )?;
        }
        Ok(child)
    }

    /// The shared spawn tail: the entry frame pushed with `args`, each
    /// `Ref` argument acquiring one count for the child's frame at the
    /// copy-in — the child's own exit drops balance it, precisely a
    /// closure call's capture copy-in. The count lands on the word's own
    /// heap (which the spawner may not be running on) and the claim on
    /// the child's ledger.
    pub(super) fn spawn_checked(
        &mut self,
        nursery: Option<NurseryId>,
        version: VersionIdx,
        func_id: u32,
        args: &[Value],
    ) -> Result<FiberId, Trap> {
        let child = self.push_spawned(nursery, version, func_id, args);
        let handoff_owner = self.current;
        for &arg in args {
            if let Value::Ref(word) = arg {
                Self::inc_ref_split(
                    &mut self.objects,
                    &mut self.pool,
                    &mut self.claims,
                    handoff_owner,
                    self.ctx_live,
                    child,
                    word,
                )?;
            }
        }
        Ok(child)
    }

    /// The pool-side half of every spawn: one pool entry with a fresh
    /// entry frame, born ready and never started, plus the child-of
    /// bookkeeping. The first switch into it runs its entry function; the
    /// pool slot of a dead fiber is reused.
    pub(super) fn push_spawned(
        &mut self,
        nursery: Option<NurseryId>,
        version: VersionIdx,
        func_id: u32,
        args: &[Value],
    ) -> FiberId {
        let frame_size = self.registry.pinned(version).functions()[func_id as usize]
            .layout
            .frame_size() as usize;
        // born into the pool: reuse a dead slot above the reserved root,
        // or grow the pool by one
        let slot = match self
            .pool
            .iter()
            .skip(1)
            .position(|f| f.state == FiberState::Dead)
        {
            Some(at) => at + 1,
            None => self.pool.len(),
        };
        if slot == self.pool.len() {
            let id = FiberId(u32::try_from(slot).expect("pool size fits u32 ids"));
            self.pool.push(Fiber::empty(id));
            self.claims.push(ClaimTable::new(id));
        } else {
            // the reuse door of the death walk: every death path drains
            // its fiber, so this releases what an un-walked marking or an
            // exotic death left behind — and nothing on the honest paths.
            // The recycled fiber's kept frames (a resume-interrupted
            // detached child's) fire their deinits here, at their last
            // door
            let recycled = FiberId(u32::try_from(slot).expect("slot fits u32"));
            self.teardown_fiber(recycled, &[]);
            self.reclaim_fiber_kstr_rows(recycled);
        }
        let fiber = &mut self.pool[slot];
        fiber.segment.truncate(0); // reuse keeps the chunks: spawn stays cheap
        Self::push_entry_frame(fiber, version, func_id, args, frame_size);
        fiber.resume = Some(ResumePoint {
            version,
            func: func_id,
            pc: CodePc(0),
        });
        fiber.state = FiberState::Ready;
        fiber.resumer = None;
        fiber.nursery = nursery;
        let id = FiberId(u32::try_from(slot).expect("pool size fits u32 ids"));
        if let Some(n) = nursery {
            self.nurseries[n.index()].push_child(id);
        }
        id
    }

    /// Requests that `fiber` park at its next taken backward branch under
    /// its next host drive ([`Vm::resume_fiber`]), the dev host's
    /// suspension-injection face: the drive ends handing the host the
    /// fiber's continuation as the suspension outcome, exactly the drive
    /// a parked-at-a-switch entry answers, and the fiber parks `Preempted`
    /// — no call site stands, so the later resume drops its payload. The
    /// request never bites inside a hosted reentry frame (a Rust-side
    /// native loop cannot suspend) or a teardown drive, and it waits
    /// past those instead. One request stands at a time; a later request
    /// overwrites it, and the drive consumes it when it parks.
    pub fn request_suspend(&mut self, fiber: FiberId) {
        self.suspend_request = Some(fiber);
    }

    /// Drives one fiber slice from the host: enters `fiber` (Ready or
    /// parked), delivering `payload` as the answer of the call it parked at
    /// (a Ready fiber drops the payload; a fiber parked by the host's
    /// suspension request drops it too — no call site stands), and runs
    /// until that fiber next parks with the host as resumer (a control
    /// park, or the requested suspension at a taken backward branch) or
    /// completes (its final value). A trapped drive leaves the fiber
    /// interrupted (not resumable) with its frames kept for
    /// [`Vm::backtrace`].
    ///
    /// A fiber parked on a world wait is the scheduler's, not this door's: it
    /// is not resumable here, because an answered payload would abandon its
    /// registered wait and leave the later wake landing on a moved-on frame.
    /// World-parked drives go through [`Vm::resume_fiber_io`]/[`Vm::pump_io`].
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownCallee`] with module `fiber` when `fiber` is absent
    /// or not parked at a switch point; the execution traps otherwise.
    pub fn resume_fiber(&mut self, fiber: FiberId, payload: Value) -> Result<FrameAnswer, Trap> {
        if !self.pool.get(fiber.index()).is_some_and(Fiber::resumable) {
            return Err(Trap::UnknownCallee {
                module: "fiber".into(),
                func: fiber.0,
            });
        }
        self.pool[fiber.index()].resumer = None; // the host resumes
        let (mut active, mut pc) = self.switch_in(fiber, payload);
        let outcome = loop {
            match self.execute_host_step::<false>(active, pc, usize::MAX) {
                // a world park under a host resume: the wait registers
                // and the executor's continuation answers it — the host
                // called a synchronous face, so the drive blocks on the
                // reactor like any blocking native would, the fiber
                // never blocking the world out
                Ok(DriveOutcome::WorldPark(req)) => {
                    if let ParkKind::World(wait) = req.kind {
                        self.register_wait(req.fiber, wait);
                    }
                    match self.executor_continue(fiber, None) {
                        Ok(ExecutorStep::Continue((a, p))) => {
                            active = a;
                            pc = p;
                        },
                        Ok(ExecutorStep::HostAnswer(answer)) => {
                            self.host_release();
                            for value in answer.as_slice() {
                                self.host_delivery(value);
                            }
                            break Ok(DriveOutcome::Died(answer));
                        },
                        Ok(ExecutorStep::Shutdown) => {
                            self.host_release();
                            break Err((Trap::Shutdown, pc));
                        },
                        // the blocking drive never idles: its wait loops
                        // until a wake
                        Ok(ExecutorStep::Idle) => {
                            unreachable!("the blocking executor wait never idles")
                        },
                        Err(trap) => break Err((trap, pc)),
                    }
                },
                other => break other,
            }
        };
        self.host_release();
        match outcome {
            Ok(DriveOutcome::Died(answer)) => {
                // the drive-ended fiber's whole answer span crosses to the
                // host; the fiber itself keeps its remaining claims
                // (interrupted drives keep their frames, and their ledgers,
                // whole)
                for value in answer.as_slice() {
                    self.host_delivery(value);
                }
                Ok(answer)
            },
            Ok(DriveOutcome::Yielded(v)) => {
                // a park with the host as resumer answers one word (the
                // suspension's bare bounce tag); the fiber itself keeps its
                // remaining claims (interrupted drives keep their frames,
                // and their ledgers, whole)
                self.host_delivery(&v);
                Ok(FrameAnswer::one(v))
            },
            // a host-level drive passes no boundary, so no reentry frame
            // exists to return through
            Ok(DriveOutcome::Frame(_)) => unreachable!("a host drive answers no reentry frame"),
            Ok(DriveOutcome::Interlock(_)) => {
                unreachable!("the step drains every interlock before returning")
            },
            // the drive loop above drained every world park into the
            // executor's continuation before breaking its outcome out
            Ok(DriveOutcome::WorldPark(_)) => {
                unreachable!("the drive loop drained every world park")
            },
            Err((trap, at)) => {
                self.trap_pc = Some(at);
                // a nursery child dying trapped under a host drive: ledger
                // it and raise the cancel flag (the first failure); the
                // fiber itself stays interrupted (frames kept). The blame
                // follows the fiber the drive ended on — the drive may have
                // switched away from `fiber` (it parked at its own resume
                // call) before the trap struck
                self.trap_child(self.current, &trap);
                // the trap's payload word crosses to the host
                self.trap_delivery(&trap);
                Err(trap)
            },
        }
    }

    /// [`Vm::resume_fiber`] under the executor seam: identical while no
    /// world op parks, but a park registers its wait and — when the
    /// wait does not resolve inside `budget` — answers
    /// [`IoDrive::Parked`] instead of blocking the host on the reactor,
    /// so a host with its own waits (a watch window, a prompt read)
    /// keeps them. `None` keeps the blocking wait exactly. The parked
    /// drive's continuation is [`Vm::pump_io`]; nothing else may enter
    /// the parked fiber (the wait owns its wake).
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownCallee`] with module `fiber` when `fiber` is absent
    /// or not parked at a switch point; the execution traps otherwise.
    pub fn resume_fiber_io(
        &mut self,
        fiber: FiberId,
        payload: Value,
        budget: Option<Duration>,
    ) -> Result<IoDrive, Trap> {
        if !self.pool.get(fiber.index()).is_some_and(Fiber::resumable) {
            return Err(Trap::UnknownCallee {
                module: "fiber".into(),
                func: fiber.0,
            });
        }
        self.pool[fiber.index()].resumer = None; // the host resumes
        // the entry switch enters the fiber directly, so its wake's
        // run-queue entry — a woken fiber is queued AND delivered — goes
        // with it, or the executor's wait would answer the stale entry
        // ready forever
        self.sched.take_ready(fiber);
        let (active, pc) = self.switch_in(fiber, payload);
        self.io_pump(fiber, Some((active, pc)), budget)
    }

    /// One bounded pump of the executor/wait seam for a world-parked host
    /// drive: no entry switch (the parked fiber's wait stands — a
    /// WorldParked fiber is the scheduler's alone), the executor drives
    /// every woken fiber, the host's included, and `budget` bounds the
    /// reactor wait. [`IoDrive::Answer`] lands when the host fiber ends
    /// its drive the way [`Vm::resume_fiber`] ends one; [`IoDrive::Parked`]
    /// answers a spent budget with the wait still standing — and, for an
    /// idle seam (nothing runnable, nothing able to wake, no shutdown
    /// standing), at once: the blocking drives' stall guard is the honest
    /// deadlock, a pumping host idles instead.
    ///
    /// # Errors
    ///
    /// [`Trap::Shutdown`] when the host's shutdown door cut the parked
    /// drive; the execution traps otherwise.
    pub fn pump_io(&mut self, host: FiberId, budget: Option<Duration>) -> Result<IoDrive, Trap> {
        // a standing shutdown never answers idle: the pump below runs
        // the cut pass and reports the shutdown trap
        if !self.sched.has_ready() && !self.sched.can_wake() && !self.shutdown {
            return Ok(IoDrive::Parked);
        }
        self.io_pump(host, None, budget)
    }

    /// The drive loop behind the two IO host entries: the staged step
    /// (an entry switch's own) runs first, then the executor until one
    /// of them answers for the host — the host fiber's completion, its
    /// park with the host as resumer, a spent budget, or a trap.
    pub(super) fn io_pump(
        &mut self,
        host: FiberId,
        entered: Option<(Active, CodePc)>,
        budget: Option<Duration>,
    ) -> Result<IoDrive, Trap> {
        let until = budget.map(|budget| Instant::now() + budget);
        let mut step = entered;
        let mut last_pc = None;
        loop {
            if let Some((active, pc)) = step.take() {
                last_pc = Some(pc);
                match self.execute_host_step::<false>(active, pc, usize::MAX) {
                    // a world park registers and the executor keeps
                    // going inside the budget: ready siblings may run
                    // while the wait stands
                    Ok(DriveOutcome::WorldPark(req)) => {
                        if let ParkKind::World(wait) = req.kind {
                            self.register_wait(req.fiber, wait);
                        }
                    },
                    Ok(DriveOutcome::Died(answer)) => {
                        self.host_release();
                        for value in answer.as_slice() {
                            self.host_delivery(value);
                        }
                        if self.current == host {
                            return Ok(IoDrive::Answer(answer));
                        }
                        // another fiber's completion crossed to the
                        // host ledger; the seam keeps pumping
                    },
                    Ok(DriveOutcome::Yielded(word)) => {
                        // the host fiber parked with the host as its
                        // resumer: the drive's own end, its park word
                        // crossing; any other park belongs to its own
                        // resumer and the seam keeps pumping
                        if self.current == host && self.pool[host.index()].resumer.is_none() {
                            self.host_release();
                            self.host_delivery(&word);
                            return Ok(IoDrive::Answer(FrameAnswer::one(word)));
                        }
                    },
                    Ok(DriveOutcome::Frame(_)) => {
                        unreachable!("a host drive answers no reentry frame")
                    },
                    Ok(DriveOutcome::Interlock(_)) => {
                        unreachable!("the step drains every interlock before returning")
                    },
                    Err((trap, at)) => {
                        self.trap_pc = Some(at);
                        self.host_release();
                        self.trap_child(self.current, &trap);
                        self.trap_delivery(&trap);
                        return Err(trap);
                    },
                }
            }
            let remaining = until.map(|at| at.saturating_duration_since(Instant::now()));
            match self.executor_continue(host, remaining) {
                Ok(ExecutorStep::Continue((active, pc))) => {
                    step = Some((active, pc));
                },
                Ok(ExecutorStep::HostAnswer(answer)) => {
                    self.host_release();
                    for value in answer.as_slice() {
                        self.host_delivery(value);
                    }
                    return Ok(IoDrive::Answer(answer));
                },
                Ok(ExecutorStep::Shutdown) => {
                    self.host_release();
                    return Err(Trap::Shutdown);
                },
                Ok(ExecutorStep::Idle) => return Ok(IoDrive::Parked),
                Err(trap) => {
                    // an executor-sourced trap (a scheduler stall, a
                    // woken drive's own trap) reports at the last step
                    // this pump drove
                    self.trap_pc = last_pc;
                    self.host_release();
                    self.trap_child(self.current, &trap);
                    self.trap_delivery(&trap);
                    return Err(trap);
                },
            }
        }
    }

    /// Releases a fiber a trapped host drive left interrupted: the kept
    /// frames' deinit hooks fire exactly once (the teardown pass every
    /// drain door shares, each drive resolving through its owning frame's
    /// version row), the claims drain, the slot's intern rows die, and
    /// the frames clear — the pool slot returns to the spawn-reuse set
    /// and the executing version's row unpins, so the next install's
    /// sweep retires it (a dead drive pins nothing). The owner's open
    /// nurseries die with it: every never-consumed nursery the fiber (or
    /// an already-dead child under it) left open tears down, its live
    /// children releasing their pins under this same discipline. The
    /// caller reports first: the trap's backtrace reads the kept frames,
    /// so the release belongs after it. Answers whether this call
    /// released the fiber; a resumable one (a parked entry keeps its
    /// drive), a dead slot, or the live running context answers false
    /// untouched.
    pub fn release_interrupted(&mut self, fiber: FiberId) -> bool {
        // the interrupted shape: a drive ended in a Trap on this fiber,
        // so it keeps Running with no resume point and its context sits
        // parked in its slot — never the context a drive still holds
        let interrupted = self
            .pool
            .get(fiber.index())
            .is_some_and(|f| f.state == FiberState::Running && f.resume.is_none());
        if !interrupted || (self.ctx_live && self.current == fiber) {
            return false;
        }
        self.release_pooled_fiber(fiber);
        self.release_owned_nurseries(fiber);
        true
    }

    /// The per-fiber release pass the interrupted door and the nursery
    /// teardown share: the kept frames' deinit hooks fire (through their
    /// owning frames' version rows, a trapping hook keeping its effects
    /// and ending the pass), the claims drain, the slot's intern rows
    /// die, and the cleared chain plus the truncated segment drop every
    /// pin the frames held — version rows through the call chain and the
    /// resume point, staged callable words through the swap re-check —
    /// before the Dead marking returns the slot to the spawn-reuse set.
    pub(super) fn release_pooled_fiber(&mut self, fiber: FiberId) {
        // a fiber parked on the world releases its reactor interest
        // first (the parked-IO cancellation discipline): the reactor
        // never reports into a released slot
        self.cancel_wait(fiber);
        self.teardown_fiber(fiber, &[]);
        self.reclaim_fiber_kstr_rows(fiber);
        let slot = &mut self.pool[fiber.index()];
        slot.callinfo.clear();
        slot.segment.truncate(0);
        slot.resume = None;
        slot.state = FiberState::Dead;
        self.end_race_of(fiber);
    }

    /// Tears down every open nursery whose owner's death released it:
    /// `released` (this door's fiber — already Dead by its own pass) and
    /// every already-dead owner (a child a drive walked dead inside its
    /// own nursery block left it open with no drive left to consume it).
    /// Each torn nursery's live children release in spawn order under the
    /// per-fiber release pass — no child is entered or re-driven at this
    /// door: a never-entered child never ran (nothing of it stands but
    /// its staged words' counts and pins, released cold), a parked child
    /// keeps the effects that ran and fires its kept frames' hooks
    /// without the cut's sentinel (its resumer is dead or releases in the
    /// same pass), and a Dead child was already walked — skipped. A
    /// released child's own open nurseries fold into the same fixpoint,
    /// so an abandoned nest dies whole; nursery slots close (Dead,
    /// spawn-reusable) lowest-slot-first, deterministic.
    pub(super) fn release_owned_nurseries(&mut self, released: FiberId) {
        while let Some(slot) = self.nurseries.iter().position(|n| {
            n.state == NurseryState::Open
                && n.owner.is_some_and(|owner| {
                    owner == released || self.pool[owner.index()].state == FiberState::Dead
                })
        }) {
            self.tear_down_nursery(slot);
        }
    }

    /// Closes one abandoned nursery: its live children release in spawn
    /// order under the per-fiber release pass — no child is entered (a
    /// reused slot belongs to another nursery: the child died and was
    /// ledgered before its slot recycled; a child a drive already walked
    /// is Dead, its hooks fired at its walk, never double-released here)
    /// — then the slot closes (Dead, spawn-reusable). The shared body of
    /// every release door.
    pub(super) fn tear_down_nursery(&mut self, slot: usize) {
        let id = NurseryId(u32::try_from(slot).expect("nursery pool fits u32 ids"));
        let children = self.nurseries[slot].children.clone();
        for child in children {
            if self.pool[child.index()].nursery != Some(id) {
                continue;
            }
            if self.pool[child.index()].state == FiberState::Dead {
                continue;
            }
            self.release_pooled_fiber(child);
        }
        let nursery = &mut self.nurseries[slot];
        nursery.driving = false;
        nursery.state = NurseryState::Dead;
        nursery.outcomes.clear();
    }

    /// The REPL rollback's line-scoped nursery door: the trapping
    /// line's root survives its own rollback, so no dead-owner door
    /// ever catches the line's open nursery. The pre-step nursery
    /// reading — the pool's states as the step entered, beside the
    /// ledger reading the rollback's claim gate already matches —
    /// floors the teardown: every root-owned open nursery sitting at
    /// a slot the reading held Dead (recycled into by the line) or
    /// past the reading's extent is the line's own and tears down
    /// with its never-entered children releasing cold, while the
    /// host-supervised root nursery and anything the reading held
    /// Open stay untouched. A child the line's join drive walked dead
    /// inside its own block left its nursery open with no drive left
    /// to consume it — the dead-owner pass below folds such a nest
    /// in, exactly the drive release's fixpoint.
    pub(crate) fn release_line_nurseries(&mut self, floor: &[NurseryState]) {
        loop {
            let slot = (0..self.nurseries.len()).find(|&slot| {
                self.nurseries[slot].state == NurseryState::Open
                    && self.nurseries[slot].owner == Some(FIBER_ROOT)
                    && !matches!(floor.get(slot), Some(NurseryState::Open))
            });
            let Some(slot) = slot else { break };
            self.tear_down_nursery(slot);
        }
        while let Some(slot) = self.nurseries.iter().position(|n| {
            n.state == NurseryState::Open
                && n.owner
                    .is_some_and(|owner| self.pool[owner.index()].state == FiberState::Dead)
        }) {
            self.tear_down_nursery(slot);
        }
    }

    /// A fiber's lifecycle state, for debug and test assertions.
    #[must_use]
    pub fn fiber_state(&self, fiber: FiberId) -> Option<FiberState> {
        self.pool.get(fiber.index()).map(|f| f.state)
    }

    /// High-water mark of a fiber's segment: the per-fiber TCO criterion —
    /// a tail-looping fiber's mark never rises past its frame size.
    #[must_use]
    pub fn fiber_max_len(&self, fiber: FiberId) -> Option<usize> {
        self.pool.get(fiber.index()).map(|f| f.max_len)
    }

    /// How many chunks a fiber's segment owns — the chunk-append growth
    /// witness.
    #[must_use]
    pub fn fiber_chunk_count(&self, fiber: FiberId) -> Option<usize> {
        self.pool
            .get(fiber.index())
            .map(|f| f.segment.chunk_count())
    }

    /// The address of word `index` in a fiber's segment — any allocated
    /// word, live or truncated-over: stable across later growth (chunks
    /// never move) and across switch round-trips (a switch copies no
    /// words).
    #[must_use]
    pub fn fiber_word_addr(&self, fiber: FiberId, index: usize) -> Option<usize> {
        self.pool
            .get(fiber.index())
            .filter(|f| index < f.segment.capacity())
            .map(|f| f.segment.word_addr(index))
    }

    /// The frame base of a fiber's innermost live frame.
    #[must_use]
    pub fn fiber_top_fp(&self, fiber: FiberId) -> Option<usize> {
        self.pool
            .get(fiber.index())
            .and_then(|f| f.callinfo.last().map(|ci| ci.fp))
    }

    /// The nursery a fiber was spawned into (the child-of link);
    /// `None` = the root fiber or a detached spawn.
    #[must_use]
    pub fn fiber_nursery(&self, fiber: FiberId) -> Option<NurseryId> {
        self.pool.get(fiber.index()).and_then(|f| f.nursery)
    }

    /// Enters `target` with `payload`: stash the outgoing fiber's context,
    /// swap the target's in (buffer ownership moves, no word is copied), and deliver `payload` onto
    /// the call the target parked at (a Ready fiber drops it — first entry runs the
    /// spawn arguments). The caller has validated the target. Entering
    /// the fiber the last drive ended on is the host's O(1) round-trip:
    /// its context was stashed at host release and swaps back in.
    pub(crate) fn switch_in(&mut self, target: FiberId, payload: Value) -> (Active, CodePc) {
        if target != self.current {
            self.host_release(); // stash the outgoing fiber's context if live
        }
        if target != self.current || !self.ctx_live {
            let fiber = &mut self.pool[target.index()];
            std::mem::swap(&mut self.stack, &mut fiber.segment);
            std::mem::swap(&mut self.callinfo, &mut fiber.callinfo);
            std::mem::swap(&mut self.objects, &mut fiber.objects);
            self.current = target;
            self.ctx_live = true;
        }
        let fiber = &mut self.pool[target.index()];
        let resume = fiber
            .resume
            .take()
            .expect("a validated switch target carries its continuation");
        let parked = fiber.state == FiberState::Suspended;
        let preempted = fiber.state == FiberState::Preempted;
        fiber.state = FiberState::Running;
        if parked {
            let f = &self.registry.pinned(resume.version).functions()[resume.func as usize];
            let fp = self.callinfo[self.callinfo.len() - 1].fp;
            overlay_word(payload, &mut self.stack, &f.layout, fp);
        }
        // a suspension-resumed fiber re-enters at its parked branch: that
        // re-take is the resume itself, so the request check skips it once
        self.suspend_skip = preempted.then_some((target, resume.pc));
        (
            Active {
                version: resume.version,
                func: resume.func,
            },
            resume.pc,
        )
    }

    /// Ends the running fiber: ledgers `outcome` into its nursery,
    /// marks it dead, and switches to its resumer delivering `value` to the
    /// call the resumer parked at. `None` = the host takes the delivery —
    /// it was the resumer, or the resumer died while this fiber was parked
    /// (the leftover pattern; the join/teardown drivers must survive it).
    /// Closes the frame at `fp` for the arena: a frame that allocated
    /// arena temporaries pops its entry mark (the frame-pop restore
    /// reclaims its bytes), and the flag clears for the frame a tail
    /// transfer reuses.
    pub(super) fn arena_frame_close(&mut self, fp: usize) {
        if let Word::Meta(meta) = &mut self.stack[fp]
            && meta.arena_active()
        {
            self.pool[self.current.index()].arena.pop_frame_mark();
            meta.clear_arena_active();
        }
    }

    pub(super) fn die_current(
        &mut self,
        value: Value,
        outcome: ChildOutcome,
    ) -> Option<(Active, CodePc)> {
        // the delivered words' claims cross with them — one release per
        // `Ref` word of the completed span (the channel is whole-span), a
        // single word for every other death. The receiver of the head word
        // is resolved below; each delivered count stays exact through the
        // joiner's own single dec. A completed fiber's exit decs have
        // released its other claims already, so no walk runs here: a
        // raised leftover is observable state (the rc_diag witness),
        // released only by the cancel-cut and interrupt doors
        match &outcome {
            ChildOutcome::Completed(span) => {
                for word in span.as_slice().iter().filter_map(|v| match v {
                    Value::Ref(word) => Some(*word),
                    _ => None,
                }) {
                    self.claims[self.current.index()].release(word);
                }
            },
            _ => {
                if let Value::Ref(word) = value {
                    self.claims[self.current.index()].release(word);
                }
            },
        }
        self.record_child_outcome(self.current, outcome);
        // the dying fiber's frames never run again, so its store's intern
        // pins release with the death: every completed, cancelled, and
        // halted exit funnels here (a cancelled fiber's claims already
        // drained below, and the release is idempotent). Interrupted and
        // marking deaths release at their own drain doors.
        self.objects.release_pins();
        self.pool[self.current.index()].arena.release_all();
        let resumer = self.pool[self.current.index()].resumer;
        let fiber = &mut self.pool[self.current.index()];
        fiber.state = FiberState::Dead;
        fiber.resume = None;
        self.end_race_of(self.current);
        let live = resumer.filter(|&r| self.pool[r.index()].resumable());
        if let (Some(&target), Value::Ref(word)) = (live.as_ref(), value) {
            self.claims[target.index()].acquire(word);
        }
        live.map(|target| self.switch_in(target, value))
    }

    /// The running fiber completed at its entry frame with `value`.
    pub(super) fn complete_current(&mut self, value: Value) -> Option<(Active, CodePc)> {
        self.die_current(value, ChildOutcome::Completed(FrameAnswer::one(value)))
    }

    /// The running fiber completed at its entry frame answering the
    /// whole span `answer` (a RETSPAN or a tail native's span/total):
    /// the span rides the nursery ledger whole, the head word alone
    /// reaching a live resumer (the resume channel stays one word). The
    /// caller keeps the span for the drive's death value — the host
    /// seam's whole answer.
    pub(super) fn complete_current_span(
        &mut self,
        answer: &FrameAnswer,
    ) -> Option<(Active, CodePc)> {
        let head = answer.as_slice().first().copied().unwrap_or(Value::I64(0));
        // the record clones the inline span so the caller keeps the words
        // for the drive's death value; the clone runs once per entry-frame
        // completion, never per call, so the hot seams stay untouched
        self.die_current(head, ChildOutcome::Completed(answer.clone()))
    }

    /// The running fiber observed its nursery's cancel flag at a taken
    /// backward branch: it stops here — no instruction of the loop
    /// body it was re-entering runs — and its resumer receives the
    /// `PLATFORM_CANCELLED` sentinel.
    pub(super) fn cancel_current(&mut self) -> Option<(Active, CodePc)> {
        // the cut fiber's exit decs never run: the deinit pass fires its
        // kept frames' hooks first (teardown walks frames running deinits),
        // then the death walk releases the claims — the
        // cancel-cut door
        self.teardown_fiber(self.current, &[]);
        let cut = self.current;
        let end = self.die_current(Value::Tag(PLATFORM_CANCELLED), ChildOutcome::Cancelled);
        // the cut scope dies with the cut: every open nursery the cut fiber
        // owned tears down here, its live children releasing cold, so a
        // cancelled block's parked children never hold a wait past the cut —
        // the same fixpoint the release doors run, moved to the death that
        // closes the scope (the race nursery's prompt close, generalized);
        // the release doors keep it for owners that die interrupted
        self.release_owned_nurseries(cut);
        end
    }

    /// The switch behind a control native's `CALL` site (the fiber pair,
    /// and the nursery join's park-drive-redeliver). The staged words are
    /// already width- and type-checked by the caller of this helper.
    pub(super) fn control_switch(
        &mut self,
        control: ControlNative,
        active: &Active,
        pc: u32,
        fp: usize,
        caller: &FrameLayout,
        nargs: u8,
    ) -> Result<SwitchOutcome, Trap> {
        // Teardown never re-enters the nursery: under the teardown-drive
        // flag every nursery control seam refuses like the drive rule's own
        // refusals — a join or boundary would drive another nursery's
        // children to death inside the pass (their death-walk doors meeting
        // the pass's own walk from below), and a spawn would strand its
        // child outside every standing drive's child snapshot. The refusal
        // precedes every read and park, so the dying fiber's takeover is
        // never parked mid-pass; the own-nursery and standing-drive refusals
        // below keep their scope outside drives.
        if self.teardown_drive
            && matches!(
                control,
                ControlNative::NurseryJoin
                    | ControlNative::NurseryOpen
                    | ControlNative::NurserySpawn
                    | ControlNative::NurseryBoundary
                    | ControlNative::NurseryCancel
            )
        {
            return Err(Trap::ReentrancyRefused);
        }
        match control {
            ControlNative::Resume => {
                let id = control_i64(&self.stack, fp, caller, 0)?;
                let payload = control_i64(&self.stack, fp, caller, 1)?;
                let target = u32::try_from(id).ok().map(FiberId).filter(|t| {
                    *t != self.current && self.pool.get(t.index()).is_some_and(Fiber::resumable)
                });
                let Some(target) = target else {
                    // no switch: the checked-read bounce answer
                    overlay_word(Value::Tag(PLATFORM_NONE), &mut self.stack, caller, fp);
                    return Ok(SwitchOutcome::Bounced);
                };
                self.park_current(active, CodePc(pc + 1));
                self.pool[target.index()].resumer = Some(self.current);
                let (a, at) = self.switch_in(target, Value::I64(payload));
                Ok(SwitchOutcome::Entered(a, at))
            },
            ControlNative::NurseryJoin => {
                let id = control_i64(&self.stack, fp, caller, 0)?;
                let nursery = u32::try_from(id).ok().map(NurseryId).filter(|&n| {
                    self.nurseries
                        .get(n.index())
                        .is_some_and(|nr| nr.state == NurseryState::Open)
                });
                let Some(nursery) = nursery else {
                    // absent or consumed: the checked-read bounce answer
                    overlay_word(Value::Tag(PLATFORM_NONE), &mut self.stack, caller, fp);
                    return Ok(SwitchOutcome::Bounced);
                };
                if self.pool[self.current.index()].nursery == Some(nursery) {
                    // a child joining its own nursery would deadlock the
                    // single-threaded driver on itself
                    return Err(Trap::UnknownCallee {
                        module: "nursery".into(),
                        func: nursery.0,
                    });
                }
                if self.nurseries[nursery.index()].driving {
                    // a drive of this nursery already stands: the second
                    // joiner would interlock the two drives on each other
                    // (its answer needs the standing drive's completion,
                    // the standing drive needs the joiner's death) — the
                    // cycle the own-nursery rule refuses for one level
                    return Err(Trap::UnknownCallee {
                        module: "nursery".into(),
                        func: nursery.0,
                    });
                }
                self.park_current(active, CodePc(pc + 1));
                Ok(SwitchOutcome::Interlock(InterlockRequest {
                    nursery,
                    joiner: self.current,
                    resume_pc: CodePc(pc + 1),
                    seam: DriveSeam::Count,
                }))
            },
            // the in-language seams: open, spawn, the boundary join,
            // and cancel. The lowerer synthesizes every call site, so a
            // staged id always names this frame's own open nursery; the
            // guards answer the checked-read bounce tag (the native-table
            // convention) for hand-assembled corruption.
            ControlNative::NurseryOpen => {
                let id = self.open_fresh_nursery();
                overlay_word(Value::I64(i64::from(id.0)), &mut self.stack, caller, fp);
                Ok(SwitchOutcome::Bounced)
            },
            ControlNative::NurserySpawn => {
                let id = control_i64(&self.stack, fp, caller, 0)?;
                let word = control_word(&self.stack, fp, caller, 1)?;
                let nursery = u32::try_from(id).ok().map(NurseryId).filter(|&n| {
                    self.nurseries
                        .get(n.index())
                        .is_some_and(|nr| nr.state == NurseryState::Open)
                });
                let Some(nursery) = nursery else {
                    overlay_word(Value::Tag(PLATFORM_NONE), &mut self.stack, caller, fp);
                    return Ok(SwitchOutcome::Bounced);
                };
                // the callable word: a scalar `Fn` names the child with no
                // captures; a closure object's `Ref` carries them as the
                // entry arguments
                let (fref, args) = match word {
                    Value::Fn(fref) => (fref, Vec::new()),
                    Value::Ref(obj) => match self.closure_parts(obj) {
                        Ok((fref, captures)) => (fref, captures),
                        Err(e) => return Err(e),
                    },
                    staged => {
                        return Err(Trap::CallLayout(SlideError::TypeMismatch {
                            index: 1,
                            staged: staged.slot_type(),
                            expected: SlotType::Fn,
                        }));
                    },
                };
                self.spawn_ref_into(Some(nursery), fref, &args)?;
                overlay_word(Value::I64(0), &mut self.stack, caller, fp);
                Ok(SwitchOutcome::Bounced)
            },
            ControlNative::NurseryBoundary => {
                let id = control_i64(&self.stack, fp, caller, 0)?;
                let mask = control_i64(&self.stack, fp, caller, 1)?;
                let nursery = u32::try_from(id).ok().map(NurseryId).filter(|&n| {
                    self.nurseries
                        .get(n.index())
                        .is_some_and(|nr| nr.state == NurseryState::Open)
                });
                let Some(nursery) = nursery else {
                    overlay_word(Value::Tag(PLATFORM_NONE), &mut self.stack, caller, fp);
                    return Ok(SwitchOutcome::Bounced);
                };
                if self.pool[self.current.index()].nursery == Some(nursery) {
                    return Err(Trap::UnknownCallee {
                        module: "nursery".into(),
                        func: nursery.0,
                    });
                }
                if self.nurseries[nursery.index()].driving {
                    // the interlock cycle of the join seam, one level up
                    return Err(Trap::UnknownCallee {
                        module: "nursery".into(),
                        func: nursery.0,
                    });
                }
                self.park_current(active, CodePc(pc + 1));
                Ok(SwitchOutcome::Interlock(InterlockRequest {
                    nursery,
                    joiner: self.current,
                    resume_pc: CodePc(pc + 1),
                    seam: DriveSeam::Boundary { mask },
                }))
            },
            ControlNative::NurseryCancel => {
                // the fiber's deliberate failure: the trap path ledgers
                // the outcome and raises the cancel flag; the call never
                // answers. The staged words are the failing value's flat
                // span — word 0 the member's tag, the rest its payload
                // words (the native channel's staging shape); the staged
                // count is the call site's fact, bounded by the frame's
                // own area
                if usize::from(nargs) > caller.arg_area_len() {
                    return Err(Trap::CallLayout(SlideError::StagingTooSmall {
                        available: caller.arg_area_len(),
                        needed: usize::from(nargs),
                    }));
                }
                let mut payload = Vec::with_capacity(usize::from(nargs));
                for index in 0..usize::from(nargs) {
                    payload.push(control_word(&self.stack, fp, caller, index)?);
                }
                Err(Trap::NurseryCancelled { payload })
            },
            ControlNative::ReplPark => {
                // the REPL's resumed frame parks here: the staged span is
                // the submission's whole answer (the splice mirrored the
                // exit's RET staging), word 0 riding the event to the host
                // while the whole span rides the seam's marker for the
                // REPL drive to read. Only a host drive ever meets the
                // seam — the call sites live in the entry function the
                // splice owns — so the resumer switch of the yield arm
                // cannot occur; a stray in-language resumer still gets the
                // honest switch.
                if usize::from(nargs) > caller.arg_area_len() {
                    return Err(Trap::CallLayout(SlideError::StagingTooSmall {
                        available: caller.arg_area_len(),
                        needed: usize::from(nargs),
                    }));
                }
                let answer = FrameAnswer::staged(usize::from(nargs), |index| {
                    control_word(&self.stack, fp, caller, index)
                })?;
                let head = answer.as_slice().first().copied().unwrap_or(Value::I64(0));
                self.park_current(active, CodePc(pc + 1));
                let resumer = self.pool[self.current.index()].resumer;
                match resumer.filter(|&r| self.pool[r.index()].resumable()) {
                    None => {
                        self.repl_park = Some(answer);
                        Ok(SwitchOutcome::Host(DriveOutcome::Yielded(head)))
                    },
                    Some(target) => {
                        let (a, at) = self.switch_in(target, head);
                        Ok(SwitchOutcome::Entered(a, at))
                    },
                }
            },
        }
    }

    /// Writes the boundary seam's answer span onto the joiner's call
    /// site: word 0 the discriminant, the tail the failing member's flat
    /// word span. Runs after the joiner's re-entry, so the current
    /// context is its frame.
    pub(super) fn overlay_boundary(&mut self, active: &Active, tag: u8, payload: &[Value]) {
        let fp = self.callinfo[self.callinfo.len() - 1].fp;
        let f = &self.registry.pinned(active.version).functions()[active.func as usize];
        let area = f.layout.arg_area_len();
        // the span channel's bounds check: the boundary's static floor is
        // the two-word answer, and the payload words check at the write
        // exactly like a span answer's (a too-narrow caller is a
        // hand-assembled module — the lowerer widens the area to the
        // widest contained member)
        if 1 + payload.len() > area {
            // a hand-assembled module only: the lowerer widens the area to
            // the widest contained member. The write cannot happen, and
            // the payload's rows are already this fiber's from the death
            // handoff — its own drain settles them, so no ledger action
            return;
        }
        overlay_tag(tag, payload, &mut self.stack, &f.layout, fp);
    }

    /// Writes the fork seam's whole-span answer tail onto the re-entered
    /// joiner's call site: the head word already landed through the
    /// switch payload, the tail words following on the overlay slots the
    /// CALLG site's answer occupies — the tag-less total's delivery
    /// twin. The park-time width check refused an overlay too narrow
    /// for the whole span, so a missed write is hand-assembled-only
    /// (the boundary seam's same defensive skip; the delivered words'
    /// rows settle at the joiner's drain door).
    pub(super) fn overlay_span_tail(&mut self, active: &Active, tail: &[Value]) {
        let fp = self.callinfo[self.callinfo.len() - 1].fp;
        let f = &self.registry.pinned(active.version).functions()[active.func as usize];
        let area = f.layout.arg_area_len();
        if 1 + tail.len() > area {
            return;
        }
        let body_len = f.layout.body_len();
        let start = f.layout.arg_area().start;
        for (index, value) in tail.iter().enumerate() {
            self.stack[word_of(fp, start + 1 + index, body_len)] = Word::Slot(*value);
        }
    }
}
