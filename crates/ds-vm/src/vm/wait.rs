//! The reactor-facing waits: registering a world park, the loop's work check,
//! the wakes, the deadline wheel step, and the driver's continue seam.

use super::*;

impl Vm {
    /// Registers one world park with the scheduler: the token minted,
    /// the interest registered with the reactor (a registration refusal
    /// wakes the fiber at once with the refusal as its own error
    /// class), the deadline armed on the wheel. The fiber keeps its
    /// `WorldParked` state until a wake moves it to the run queue.
    pub(crate) fn register_wait(&mut self, fiber: FiberId, wait: WorldWait) {
        let deadline = wait.deadline();
        // the wait's IO interest resolves through the table its handle
        // names: the connection table for the data ops, the listener
        // table for the accept, the pending-connect row for a parked
        // connect, the exchange machine's own transport for a fetch,
        // the process table for the proc rows (a pid names no
        // connection — the pipe's own fd for a read, the entry's
        // pidfd for the exit)
        let handle = match &wait {
            WorldWait::Accept { listener, .. } => self.out.listeners().raw_handle(*listener),
            WorldWait::Connect { conn, .. } => self.out.net().raw_connect_handle(*conn),
            WorldWait::Fetch { fetch, .. } => self.out.net().raw_fetch_handle(*fetch),
            WorldWait::TlsFetch { fetch, .. } => self.out.net().raw_tls_fetch_handle(*fetch),
            WorldWait::TlsAccept { accept, .. } => self.out.net().raw_tls_accept_handle(*accept),
            #[cfg(target_os = "linux")]
            WorldWait::ProcRead { pid, pipe, .. } => self.out.procs().raw_pipe_handle(*pid, *pipe),
            #[cfg(target_os = "linux")]
            WorldWait::ProcWait { pid, .. } => self.out.procs().raw_exit_handle(*pid),
            _ => wait.conn().and_then(|conn| self.out.net().raw_handle(conn)),
        };
        // the staged machines' resolve stage (the DNS twin): a
        // connect or fetch row may stand at its resolve step, whose
        // interest is the posted eventfd's readability rather than
        // the connect's writability — the tables answer the stage's
        // own direction
        #[cfg(target_os = "linux")]
        let direction = match &wait {
            WorldWait::Connect { conn, .. } => Some(self.out.net().connect_interest(*conn)),
            WorldWait::Fetch { fetch, .. } => Some(self.out.net().fetch_interest(*fetch)),
            WorldWait::TlsFetch { fetch, .. } => Some(self.out.net().tls_fetch_interest(*fetch)),
            _ => wait.direction(),
        };
        #[cfg(not(target_os = "linux"))]
        let direction = wait.direction();
        let token = self.sched.mint();
        if let (Some(handle), Some(direction)) = (handle, direction) {
            let registered = match self.reactor.as_mut() {
                Some(installed) => installed.driver().register(handle, direction, token),
                None => Err(crate::reactor::ReactorError(
                    "no reactor installed".to_owned(),
                )),
            };
            if let Err(err) = registered {
                // the refusal span allocates in the parked fiber's own
                // store: the context releases first, so the completion's
                // swap lands the message where its Ref word names — an
                // allocation over the live context rides the slot's
                // stale stash instead (the stale-store class)
                self.host_release();
                let result = self.wait_error_span(fiber, &wait, false, &err.0);
                self.sched.waits.insert(token, WaitEntry {
                    fiber,
                    wait,
                    handle: None,
                });
                self.finish_wait(token, result);
                return;
            }
        }
        // the unbounded wait (the quiet accept — the signal its
        // backstop) arms no wheel entry: nothing but a connection, a
        // cancel cut, or the shutdown ends it
        if let Some(deadline) = deadline {
            self.sched.arm(deadline, token);
        }
        self.sched.waits.insert(token, WaitEntry {
            fiber,
            wait,
            handle,
        });
    }

    /// Whether the scheduler holds runnable or wakeable work: the pump
    /// entries' idle guard, so a host pumping a quiet seam idles instead
    /// of answering the blocking drives' stall guard.
    pub(crate) fn sched_has_work(&self) -> bool {
        self.sched.has_ready() || self.sched.can_wake()
    }

    /// Drops every wait one fiber owns (a nursery's cancel cut, a
    /// release door, the REPL's rollback and teardown): the reactor
    /// interest deregisters first (the parked-IO cancellation), then the
    /// table row and any staged delivery go; stale timer
    /// entries drop lazily on pop.
    pub(crate) fn cancel_wait(&mut self, fiber: FiberId) {
        self.sched.deliveries.remove(&fiber);
        let tokens: Vec<u64> = self
            .sched
            .waits
            .iter()
            .filter(|(_, entry)| entry.fiber == fiber)
            .map(|(token, _)| *token)
            .collect();
        for token in tokens {
            if let Some(entry) = self.sched.waits.remove(&token) {
                // a channel deadline's interest is the channel's own
                // waiter entry: the cut deregisters it here, the
                // cancel-deregistration shape over the channel's queue,
                // so no stale entry lingers and no later peer pairs with
                // a cut fiber
                if let WorldWait::ChanDeadline { chan, sender, .. } = &entry.wait {
                    // the wait's loss door: the removed send entry's hook
                    // fact queues a fire (the span rode no count; its
                    // death is the sender's own flush, so the hook must
                    // fire here at the loss, not at a pairing skip)
                    for removed in self
                        .heaps()
                        .chan_remove_waiter(*chan, fiber, *sender)
                        .unwrap_or_default()
                    {
                        self.queue_sender_fire(removed);
                    }
                }
                self.release_wait_resources(&entry.wait);
                if let Some(handle) = entry.handle
                    && let Some(installed) = self.reactor.as_mut()
                {
                    let _ = installed.driver().deregister(handle);
                }
            }
        }
    }

    /// Releases the OS resource one ended wait's stage owned: a parked
    /// connect's raw socket and a parked exchange's transport — every
    /// other shape parks on a row its handle's own table already owns.
    pub(super) fn release_wait_resources(&mut self, wait: &WorldWait) {
        match wait {
            WorldWait::Connect { conn, .. } => self.out.net().cancel_connect(*conn),
            WorldWait::Fetch { fetch, .. } => self.out.net().cancel_fetch(*fetch),
            WorldWait::TlsFetch { fetch, .. } => self.out.net().cancel_tls_fetch(*fetch),
            WorldWait::TlsAccept { accept, .. } => self.out.net().cancel_tls_accept(*accept),
            _ => {},
        }
    }

    /// Resolves one wait with `result`: the table row and its reactor
    /// interest drop, the answer stages as the fiber's wake delivery,
    /// and the fiber — still `WorldParked` — moves to `Suspended` on
    /// the run queue in wake order.
    pub(super) fn finish_wait(&mut self, token: u64, result: NativeResult) {
        self.finish_wait_with(token, Wake::Answer(result));
    }

    /// [`Self::finish_wait`] over either wake: an answer, or a failure
    /// the woken fiber dies with at its resume point.
    pub(super) fn finish_wait_with(&mut self, token: u64, wake: Wake) {
        let Some(entry) = self.sched.waits.remove(&token) else {
            return;
        };
        if let Some(handle) = entry.handle
            && let Some(installed) = self.reactor.as_mut()
        {
            let _ = installed.driver().deregister(handle);
        }
        self.release_wait_resources(&entry.wait);
        let fiber = entry.fiber;
        if self.pool[fiber.index()].state == FiberState::WorldParked {
            self.pool[fiber.index()].state = FiberState::Suspended;
            self.sched.deliveries.insert(fiber, wake);
            self.sched.push_ready(fiber);
        }
    }

    /// The close seam's wake door: the close natives note the
    /// handle they closed into the sink, and the dispatcher's native
    /// call site drains the notes here — every parked wait the closed
    /// handle names deregisters its interest and enters the run queue
    /// NOW, its answer staged for the resume point, so the deadline
    /// never bounds a wait whose subject is gone (the quiet accept's
    /// unbounded park included: its listener's close ends it). The
    /// answer is the surface's own closed-handle span: the net data and
    /// accept waits complete through the wake door itself (a synthetic
    /// readiness runs the op against the closed row, the completion
    /// rendering the error span a wake on the closed handle answers —
    /// the unknown-connection or unknown-listener net floor), the proc
    /// read the pipe's orderly end (HUP's empty string — the close killed
    /// the child holding the write end), the proc wait
    /// the closed handle's io floor (the blocking twin's post-close
    /// answer). The wake never resurrects a dead fiber: a released
    /// waiter's waits died with its release door, and a fiber not
    /// world-parked keeps its row for whatever door owns it.
    pub(super) fn wake_closed(&mut self) {
        let closed = self.out.take_closed();
        for (seam, id) in closed {
            let tokens: Vec<u64> = self
                .sched
                .waits
                .iter()
                .filter(|(_, entry)| entry.wait.watches(seam, id))
                .map(|(token, _)| *token)
                .collect();
            for token in tokens {
                let Some(entry) = self.sched.waits.get(&token).cloned() else {
                    continue;
                };
                if self.pool[entry.fiber.index()].state != FiberState::WorldParked {
                    continue;
                }
                match entry.wait {
                    // the data and accept waits take the wake door: the
                    // synthetic readiness performs the op against the
                    // closed row, the completion answering the error
                    // span the surface rules for the closed handle
                    WorldWait::Recv { .. } | WorldWait::Accept { .. } => {
                        self.complete_wait(
                            token,
                            Some(crate::reactor::TokenEvent {
                                token,
                                read: true,
                                write: false,
                                hup: false,
                            }),
                        );
                    },
                    WorldWait::Send { .. } => {
                        self.complete_wait(
                            token,
                            Some(crate::reactor::TokenEvent {
                                token,
                                read: false,
                                write: true,
                                hup: false,
                            }),
                        );
                    },
                    // the child died with its handle: the read wakes as
                    // the pipe's orderly end (the empty string, the
                    // HUP wake's own answer), never the closed table's
                    // io floor; the exit wait answers that floor — the
                    // blocking twin's post-close answer
                    WorldWait::ProcRead { pid, .. } => {
                        let op = entry.wait.op_name();
                        let result = self.complete_in_store(entry.fiber, |heaps| {
                            crate::proc::read_answer(heaps, op, pid, Vec::new())
                        });
                        self.finish_wait(token, result);
                    },
                    WorldWait::ProcWait { pid, .. } => {
                        let op = entry.wait.op_name();
                        let target = pid.to_string();
                        let err = crate::proc::unknown_process(pid);
                        let result = self.complete_in_store(entry.fiber, |heaps| {
                            crate::native::io_err(heaps, op, &target, &err)
                        });
                        self.finish_wait(token, result);
                    },
                    // the staged machines name rows no close seam
                    // addresses: their own doors own them
                    _ => {},
                }
            }
        }
    }

    /// Re-arms one staged machine's pending step (a fetch exchange,
    /// plain or tls, the tls accept machine, or a connect's resolve
    /// step): the bound resets to the step-grained deadline the
    /// blocking twin's socket timeouts keep, and the standing interest
    /// re-aims at the direction the machine named. A machine whose
    /// registration key moved under the step (the tls accept's
    /// listener fd becoming the accepted stream's, or a resolve
    /// stage's eventfd becoming the connect stage's raw fd) swaps
    /// the stored handle and deregisters the stale key — a left
    /// level-triggered interest would wake every poll with a dead
    /// token and busy-loop the executor.
    pub(super) fn rearm_step(
        &mut self,
        token: u64,
        handle: Option<crate::reactor::IoHandle>,
        watch: crate::reactor::Direction,
    ) {
        let deadline = Instant::now() + self.out.net().timeout();
        if let Some(live) = self.sched.waits.get_mut(&token)
            && let WorldWait::Fetch {
                deadline: armed, ..
            }
            | WorldWait::TlsFetch {
                deadline: armed, ..
            }
            | WorldWait::TlsAccept {
                deadline: armed, ..
            }
            | WorldWait::Connect {
                deadline: armed, ..
            } = &mut live.wait
        {
            *armed = deadline;
        }
        if let Some(live) = self.sched.waits.get_mut(&token)
            && live.handle != handle
        {
            let stale = live.handle;
            live.handle = handle;
            if let (Some(stale), Some(installed)) = (stale, self.reactor.as_mut()) {
                let _ = installed.driver().deregister(stale);
            }
        }
        self.sched.arm(deadline, token);
        if let (Some(installed), Some(handle)) = (self.reactor.as_mut(), handle) {
            let _ = installed.driver().register(handle, watch, token);
        }
    }

    /// One wait's deadline-or-refusal span rendered in its own face's
    /// member shape, allocated in the parked fiber's own store: the
    /// net face answers the net span, the process face the io span —
    /// the deadline naming its bound either way, the refusal carrying
    /// the caller's detail.
    pub(super) fn wait_error_span(
        &mut self,
        fiber: FiberId,
        wait: &WorldWait,
        timed_out: bool,
        refusal: &str,
    ) -> NativeResult {
        let op = wait.op_name();
        let target = wait.target();
        if wait.is_proc() {
            if timed_out {
                let bound = self.out.procs().timeout();
                return self.complete_in_store(fiber, |heaps| {
                    crate::proc::timeout_err(heaps, op, &target, bound)
                });
            }
            return self.complete_in_store(fiber, |heaps| {
                crate::native::io_err(heaps, op, &target, &std::io::Error::other(refusal))
            });
        }
        let (class, detail) = if timed_out {
            let bound = self.out.net().timeout();
            (
                crate::net::NetClass::Timeout,
                format!("timed out after {bound:?}"),
            )
        } else {
            (crate::net::NetClass::Refused, refusal.to_owned())
        };
        self.complete_in_store(fiber, |heaps| {
            crate::net::net_err(heaps, class, op, &target, &detail)
        })
    }

    /// Attempts one wait's completion: the deadline answers the timeout
    /// span, an fd readiness runs the op's non-blocking completion
    /// against the parked fiber's own store, a spurious wake (wrong
    /// direction, or the readiness already consumed) leaves the wait
    /// standing, and a partial send keeps its remainder waiting. No
    /// trap path: every failure is the op's own error span.
    pub(super) fn complete_wait(&mut self, token: u64, ready: Option<crate::reactor::TokenEvent>) {
        let Some(entry) = self.sched.waits.get(&token).cloned() else {
            return;
        };
        let fiber = entry.fiber;
        // the timer-only wait: the wheel's firing IS the op's completion
        // — the unit word delivered at the parked site, no readiness to
        // consume and no error span the op could answer
        if matches!(entry.wait, WorldWait::Sleep { .. }) {
            self.finish_wait(token, NativeResult::Word(Value::I64(0)));
            return;
        }
        // the timer race's deadline door: the wheel's firing is the
        // deadline itself — no readiness to consume, the racer's cut and
        // the owner's answer resolve the wait
        if let WorldWait::Timeout { .. } = entry.wait {
            self.expire_race(token, fiber);
            return;
        }
        // the channel deadline's door: the wheel fires the loser's cut —
        // the waiter entry deregisters from the channel's own queue (the
        // sender's staged span dropping with it, its counts the parked
        // frame's own), and the parked call answers the io taxonomy's
        // `timed_out` span, the re-span the ambient's `t | timed_out`
        // union carries. The entry is present whenever the row stands:
        // the win doors wake through `wake_chan_waiter`, which drops the
        // row before this arm could ever see a woken fiber.
        if let WorldWait::ChanDeadline { chan, sender, .. } = &entry.wait {
            // the wheel's loss door: the removed send entry's hook fact
            // queues a fire — the losing span rode no count, so its hook
            // must fire at the wait's loss, not drop with the entry
            for removed in self
                .heaps()
                .chan_remove_waiter(*chan, fiber, *sender)
                .unwrap_or_default()
            {
                self.queue_sender_fire(removed);
            }
            self.finish_wait_with(token, Wake::Answer(timeout_failure_span()));
            return;
        }
        let target = entry.wait.target();
        let op = entry.wait.op_name();
        if entry.wait.deadline().is_some_and(|at| at <= Instant::now()) {
            let result = self.wait_error_span(fiber, &entry.wait, true, "");
            self.finish_wait(token, result);
            return;
        }
        let result =
            match entry.wait {
                // the process face's read: the pipe's readable wake
                // performs the one read against the parked fiber's own
                // store; the HUP wake answers too — a pipe whose write
                // end closed reads as the orderly empty string, and
                // the kernel reports the drained pipe as HUP alone,
                // no IN bit; a spurious wake (the readiness consumed
                // elsewhere) leaves the wait standing for the next poll
                #[cfg(target_os = "linux")]
                WorldWait::ProcRead {
                    pid: id, pipe, max, ..
                } => {
                    let Some(event) = ready.filter(|event| event.read || event.hup) else {
                        return;
                    };
                    let _ = event;
                    match self.out.procs().try_read(id, pipe, max) {
                        crate::net::NetAttempt::Done(Ok(bytes)) => self
                            .complete_in_store(fiber, |heaps| {
                                crate::proc::read_answer(heaps, op, id, bytes)
                            }),
                        crate::net::NetAttempt::Done(Err(err)) => self
                            .complete_in_store(fiber, |heaps| {
                                crate::native::io_err(heaps, op, &target, &err)
                            }),
                        // spurious: the readiness was consumed elsewhere,
                        // the wait stands for the next poll
                        crate::net::NetAttempt::Park => return,
                    }
                },
                // the exit interest: the pidfd's readable wake reaps
                // the status non-blockingly (the idempotent word a
                // later wait answers again), the HUP wake the same
                // completion (a gone peer answers its word now); a
                // spurious wake (the child still runs) leaves the wait
                // standing
                #[cfg(target_os = "linux")]
                WorldWait::ProcWait { pid: id, .. } => {
                    let Some(event) = ready.filter(|event| event.read || event.hup) else {
                        return;
                    };
                    let _ = event;
                    match self.out.procs().try_wait(id) {
                        Some(status) => crate::net::ok(Value::I64(status)),
                        None => return,
                    }
                },
                // the proc parked twins are the linux driver's own: the
                // parking flag never flips anywhere else, so no wait of
                // these shapes can stand
                #[cfg(not(target_os = "linux"))]
                WorldWait::ProcRead { .. } | WorldWait::ProcWait { .. } => {
                    unreachable!("the proc parked twins are the linux driver's own")
                },
                // the timer-only waits completed at their own arms above;
                // the arm keeps this match total
                WorldWait::Sleep { .. }
                | WorldWait::Timeout { .. }
                | WorldWait::ChanDeadline { .. } => {
                    unreachable!("the timer-only waits completed at their own arms")
                },
                WorldWait::Recv { conn: id, max, .. } => {
                    let Some(event) = ready.filter(|event| event.read) else {
                        return;
                    };
                    let _ = event;
                    match self.out.net().try_recv(id, max) {
                        crate::net::NetAttempt::Done(Ok(bytes)) => self
                            .complete_in_store(fiber, |heaps| {
                                crate::net::recv_answer(heaps, id, bytes)
                            }),
                        crate::net::NetAttempt::Done(Err(err)) => {
                            let class = crate::net::NetClass::of(&err);
                            let detail = err.to_string();
                            self.complete_in_store(fiber, |heaps| {
                                crate::net::net_err(heaps, class, op, &target, &detail)
                            })
                        },
                        // spurious: the readiness was consumed elsewhere,
                        // the wait stands for the next poll
                        crate::net::NetAttempt::Park => return,
                    }
                },
                WorldWait::Send {
                    conn: id, mut data, ..
                } => {
                    if !ready.is_some_and(|event| event.write) {
                        return;
                    }
                    match self.out.net().try_send(id, &mut data) {
                        crate::net::NetAttempt::Done(Ok(())) => crate::net::ok(Value::I64(0)),
                        crate::net::NetAttempt::Done(Err(err)) => {
                            let class = crate::net::NetClass::of(&err);
                            let detail = err.to_string();
                            self.complete_in_store(fiber, |heaps| {
                                crate::net::net_err(heaps, class, op, &target, &detail)
                            })
                        },
                        // partial drain: the remainder keeps the wait, its
                        // level-triggered readiness reporting again once
                        // the buffer takes more
                        crate::net::NetAttempt::Park => {
                            if let Some(live) = self.sched.waits.get_mut(&token)
                                && let WorldWait::Send { data: pending, .. } = &mut live.wait
                            {
                                *pending = data;
                            }
                            return;
                        },
                    }
                },
                WorldWait::Accept { listener, .. } => {
                    let Some(event) = ready.filter(|event| event.read) else {
                        return;
                    };
                    let _ = event;
                    match self.out.listeners().try_accept(listener) {
                        // the accepted stream registers like a connecting
                        // one, so the parked wake answers the connection
                        // handle the caller's net_handle pairs with
                        crate::net::NetAttempt::Done(Ok(stream)) => {
                            let conn = self.out.net().register(stream);
                            crate::net::ok(Value::I64(conn))
                        },
                        crate::net::NetAttempt::Done(Err(err)) => {
                            let class = crate::net::NetClass::of(&err);
                            let detail = err.to_string();
                            self.complete_in_store(fiber, |heaps| {
                                crate::net::net_err(heaps, class, op, &target, &detail)
                            })
                        },
                        // spurious: the pending connection was consumed
                        // elsewhere, the wait stands for the next poll
                        crate::net::NetAttempt::Park => return,
                    }
                },
                // the staged connect machine's wake: the readable eventfd takes the resolver
                // helper's answer and starts the connect over the
                // resolved addresses, the writable (or errored) wake
                // probing the attempt — connected registering the
                // stream under the pending row's id, the errno it
                // holds the failure; a stage left standing re-aims the
                // interest at its own direction and registration key
                WorldWait::Connect { conn: id, .. } => {
                    let Some(event) = ready.filter(|event| event.write || event.read) else {
                        return;
                    };
                    let _ = event;
                    match self.out.net().connect_wake(id) {
                        crate::net::ConnectWake::Done(Ok(conn)) => crate::net::ok(Value::I64(conn)),
                        crate::net::ConnectWake::Done(Err(err)) => {
                            let class = crate::net::NetClass::of(&err);
                            let detail = err.to_string();
                            self.complete_in_store(fiber, |heaps| {
                                crate::net::net_err(heaps, class, op, &target, &detail)
                            })
                        },
                        crate::net::ConnectWake::Pending { watch } => {
                            let handle = self.out.net().raw_connect_handle(id);
                            self.rearm_step(token, handle, watch);
                            return;
                        },
                    }
                },
                // the plain exchange's machine: the OS probes gate the
                // stages (the event advisory), a pending step re-aiming
                // the standing interest at its next direction and re-arming
                // the bound — the per-op bound the blocking twin's socket
                // timeouts keep, step-grained
                WorldWait::Fetch { fetch, .. } => {
                    let _ = ready;
                    match self.out.net().fetch_wake(fetch) {
                        crate::net::FetchWake::Done(Ok(body)) => self
                            .complete_in_store(fiber, |heaps| crate::net::ok(heaps.new_str(body))),
                        crate::net::FetchWake::Done(Err(err)) => {
                            let class = crate::net::NetClass::of(&err);
                            let detail = err.to_string();
                            self.complete_in_store(fiber, |heaps| {
                                crate::net::net_err(heaps, class, op, &target, &detail)
                            })
                        },
                        crate::net::FetchWake::Pending { watch } => {
                            // the machine's registration key moves with
                            // its stage (the resolve stage's eventfd
                            // becoming the connect stage's raw fd)
                            let handle = self.out.net().raw_fetch_handle(fetch);
                            self.rearm_step(token, handle, watch);
                            return;
                        },
                    }
                },
                // the TLS exchange's machine: the same staged discipline,
                // the session's own wants naming the direction each step
                // watches next (the wake runs the session against the
                // non-blocking socket until it would block, so the event's
                // own directions carry no extra truth the machine lacks)
                WorldWait::TlsFetch { fetch, .. } => {
                    let _ = ready;
                    match self.out.net().tls_fetch_wake(fetch) {
                        crate::net::FetchWake::Done(Ok(body)) => self
                            .complete_in_store(fiber, |heaps| crate::net::ok(heaps.new_str(body))),
                        crate::net::FetchWake::Done(Err(err)) => {
                            let class = crate::net::NetClass::of(&err);
                            let detail = err.to_string();
                            self.complete_in_store(fiber, |heaps| {
                                crate::net::net_err(heaps, class, op, &target, &detail)
                            })
                        },
                        crate::net::FetchWake::Pending { watch } => {
                            let handle = self.out.net().raw_tls_fetch_handle(fetch);
                            self.rearm_step(token, handle, watch);
                            return;
                        },
                    }
                },
                // the tls accept's machine: the accept on the listener's
                // readable door, the staged handshake over the accepted
                // stream after — the completion registering the stream
                // with its session under the pre-minted id (the machine's
                // registration key moving under the step, so the re-arm
                // re-resolves it); every failure the error span answers
                // and the row's drop closes the stream
                WorldWait::TlsAccept { accept, .. } => {
                    let _ = ready;
                    match self.out.tls_accept_wake(accept) {
                        crate::net::AcceptWake::Done(Ok(conn)) => crate::net::ok(Value::I64(conn)),
                        crate::net::AcceptWake::Done(Err(err)) => {
                            let class = crate::net::NetClass::of(&err);
                            let detail = err.to_string();
                            self.complete_in_store(fiber, |heaps| {
                                crate::net::net_err(heaps, class, op, &target, &detail)
                            })
                        },
                        crate::net::AcceptWake::Pending { watch } => {
                            let handle = self.out.net().raw_tls_accept_handle(accept);
                            self.rearm_step(token, handle, watch);
                            return;
                        },
                    }
                },
            };
        self.finish_wait(token, result);
    }

    /// The single-thread executor's wait: fire every expired deadline,
    /// then — while nothing is runnable — poll the reactor with the
    /// distance to the nearest deadline as the timeout, completing
    /// whatever the events answer; waits with no registered IO
    /// interest (the sleep's timer-only shape) wait the nearest
    /// deadline out on the thread instead, the wheel their whole wake.
    /// The context releases first (the pool slots hold every fiber's
    /// true store while the scheduler runs). A reactor refusal fails
    /// every standing IO wait with its own refusal span, so the program
    /// surfaces the failure through its error channel; the stall guard answers
    /// [`Trap::SchedulerDeadlock`] when nothing is runnable and
    /// nothing can wake. A `Some` budget bounds the wait: one poll (or
    /// nap) fires whatever it armed, and a still-quiet seam answers
    /// `Ok(false)` — the pumping host's idle signal — while `None`
    /// loops until a wake exactly as ever.
    pub(super) fn wait_world(&mut self, budget: Option<Duration>) -> Result<bool, Trap> {
        let mut events = Vec::new();
        // one bounded wait per call: the loop top fires what it armed
        // and answers ready, the second quiet pass answering idle
        let mut waited = false;
        loop {
            // the shutdown stands: nothing here may block again — the
            // callers' next pass cuts every parked fiber and answers
            // the host
            if self.shutdown {
                return Ok(true);
            }
            self.host_release();
            let now = Instant::now();
            for token in self.sched.pop_expired(now) {
                if self.sched.waits.contains_key(&token) {
                    self.complete_wait(token, None);
                }
            }
            if self.sched.has_ready() {
                return Ok(true);
            }
            if !self.sched.can_wake() {
                // a pumping executor loop that drained the scheduler
                // answers spent, not the blocking stall trap — a
                // budgeted host idles by its own pump's contract, the
                // unbudgeted drives keep the honest deadlock
                if budget.is_some() {
                    return Ok(false);
                }
                return Err(Trap::SchedulerDeadlock);
            }
            if waited && budget.is_some() {
                return Ok(false);
            }
            waited = true;
            // no registered IO interest stands: the wheel alone wakes
            // the waits that are left (a sleep parks with none, and a
            // wait the reactor refused never gained one), so the
            // thread waits the nearest deadline out — a broken or
            // absent reactor cannot strand a timer
            if !self
                .sched
                .waits
                .values()
                .any(|entry| entry.handle.is_some())
            {
                match self.sched.next_deadline() {
                    Some(at) => {
                        let left = at.saturating_duration_since(Instant::now());
                        let nap = budget.map_or(left, |budget| budget.min(left));
                        std::thread::sleep(nap);
                    },
                    None => return Err(Trap::SchedulerDeadlock),
                }
                continue;
            }
            let Some(mut installed) = self.reactor.take() else {
                return Err(Trap::SchedulerDeadlock);
            };
            // the budget bounds the poll even with no deadline standing
            // (the unbounded accept arms no wheel entry): a pumping host
            // never blocks past its window
            let timeout = match (self.sched.next_deadline(), budget) {
                (Some(at), Some(budget)) => Some(budget.min(at.saturating_duration_since(now))),
                (Some(at), None) => Some(at.saturating_duration_since(now)),
                (None, Some(budget)) => Some(budget),
                (None, None) => None,
            };
            let polled = installed.driver().poll(timeout, &mut events);
            self.reactor = Some(installed);
            match polled {
                Ok(()) => {
                    let drained = std::mem::take(&mut events);
                    for event in drained {
                        // the host's shutdown door: its wake is the
                        // shutdown event, never a wait — the drive
                        // raises every flag and the callers' pass cuts
                        // the parked fibers with the sentinel
                        if event.token == crate::reactor::SHUTDOWN_TOKEN {
                            self.shutdown_raised();
                            continue;
                        }
                        self.complete_wait(event.token, Some(event));
                    }
                },
                Err(err) => {
                    let tokens: Vec<u64> = self.sched.waits.keys().copied().collect();
                    for token in tokens {
                        let Some(entry) = self.sched.waits.get(&token).cloned() else {
                            continue;
                        };
                        // a sleep never touched the reactor: the wheel
                        // still owes it, and the branch above answers a
                        // reactor-less wait without one (the timer race's
                        // wait rides the same wheel)
                        if matches!(
                            entry.wait,
                            WorldWait::Sleep { .. }
                                | WorldWait::Timeout { .. }
                                | WorldWait::ChanDeadline { .. }
                        ) {
                            continue;
                        }
                        let result = self.wait_error_span(
                            entry.fiber,
                            &entry.wait,
                            false,
                            &format!("reactor: {}", err.0),
                        );
                        self.finish_wait(token, result);
                    }
                },
            }
        }
    }

    /// The shutdown observation's engine half: the flag stands, and every open nursery's cancel
    /// flag rises with it so the drives' cuts observe one discipline — the
    /// sentinel at the resume-shaped point, the deinit hooks firing,
    /// the boundary reporting. Nothing here decides anything: the
    /// host that registered the interest owns the outcome.
    pub(super) fn shutdown_raised(&mut self) {
        self.shutdown = true;
        for nursery in &mut self.nurseries {
            if nursery.state == NurseryState::Open {
                nursery.cancelled = true;
            }
        }
    }

    /// One fiber the executor's pass must cut: parked at a
    /// resume-shaped point while either the host's shutdown stands or
    /// its own nursery's cancel flag is raised — the flag discipline
    /// the join/teardown sweep performs for its own frame's children,
    /// extended to the fibers no drive stands over. An unbounded accept park dies here
    /// exactly a bounded one does.
    pub(super) fn cut_candidate(&self) -> Option<FiberId> {
        self.pool.iter().enumerate().find_map(|(slot, fiber)| {
            let cut = self.shutdown
                || fiber.nursery.is_some_and(|n| {
                    self.nurseries
                        .get(n.index())
                        .is_some_and(|nr| nr.state == NurseryState::Open && nr.cancelled)
                });
            let parked = matches!(
                fiber.state,
                FiberState::Suspended | FiberState::Preempted | FiberState::WorldParked
            ) && fiber.resume.is_some();
            (cut && parked).then(|| FiberId(u32::try_from(slot).expect("pool slots fit u32 ids")))
        })
    }

    /// Stages one fiber's cancel cut: the wait (its interest, its
    /// delivery) drops exactly the sweep's cut path, the staged cut
    /// and run-queue entry carrying the sentinel to the next entry.
    pub(super) fn stage_cut(&mut self, fiber: FiberId) {
        self.cancel_wait(fiber);
        self.sched.stage_cancel(fiber);
        self.sched.push_ready(fiber);
    }

    /// Enters one woken fiber: the staged answer's head word rides the
    /// switch payload, the whole answer overlays the parked call site,
    /// and the fiber continues. The sentinel resume point (an entry
    /// frame's pending tail park) instead completes the fiber with the
    /// delivered answer — its nursery ledgered, its resumer (or the
    /// host) taking the delivery. `Quiet` answers a fiber that cannot
    /// be entered (already cut, or its delivery gone with the cut).
    pub(super) fn enter_woken(&mut self, fiber: FiberId) -> Result<Woken, Trap> {
        // the staged cancel cut (a raised flag, the shutdown): the
        // sentinel at the resume-shaped point instead of any answer —
        // the sweep's cancelled-parked-child cut, delivered where no
        // drive stands over the fiber
        if self.sched.take_cancel(fiber) {
            if !matches!(
                self.pool[fiber.index()].state,
                FiberState::Suspended | FiberState::Preempted | FiberState::WorldParked
            ) || self.pool[fiber.index()].resume.is_none()
            {
                return Ok(Woken::Quiet);
            }
            self.switch_in(fiber, Value::Tag(PLATFORM_NONE));
            return Ok(match self.cancel_current() {
                Some(step) => Woken::Step(step),
                None => Woken::Quiet,
            });
        }
        let Some(wake) = self.sched.deliveries.remove(&fiber) else {
            return Ok(Woken::Quiet);
        };
        if !self.pool[fiber.index()].resumable() {
            return Ok(Woken::Quiet);
        }
        // a staged failure: the fiber is entered so the trap attributes
        // at its parked call, and nothing answers
        let result = match wake {
            Wake::Answer(result) => result,
            Wake::Fault(trap) => {
                self.switch_in(fiber, Value::Tag(PLATFORM_NONE));
                return Err(trap);
            },
        };
        let final_delivery = self.pool[fiber.index()]
            .resume
            .as_ref()
            .is_some_and(|resume| resume.pc.0 == u32::MAX);
        let head = native_head_word(&result);
        let (active, pc) = self.switch_in(fiber, head);
        if final_delivery {
            let answer = frame_answer_of(result);
            return Ok(match self.complete_current_span(&answer) {
                Some((a, at)) => Woken::Step((a, at)),
                None => Woken::Completed(answer),
            });
        }
        let f = &self.registry.pinned(active.version).functions()[active.func as usize];
        let fp = self.callinfo[self.callinfo.len() - 1].fp;
        overlay_native(result, &mut self.stack, &f.layout, fp)?;
        Ok(Woken::Step((active, pc)))
    }

    /// The single-thread executor's continuation after a drive went
    /// quiet on a world park: enter the next woken fiber, and when
    /// nothing is ready, eagerly start the parked joiner's own Ready
    /// nursery children (a spawned-but-unentered child otherwise runs
    /// only under its join drive), and when nothing is runnable, wait on
    /// the reactor. `HostAnswer` surfaces `host_fiber`'s own completion
    /// (the root drive's program answer, a resumed fiber's answer); any
    /// other completed orphan crosses to the host here and the executor
    /// keeps going. `budget` bounds the reactor wait (`None` keeps the
    /// blocking wait the synchronous drives hold): a spent budget with
    /// the waits still standing answers [`ExecutorStep::Idle`], the
    /// pumping host's idle signal.
    pub(crate) fn executor_continue(
        &mut self,
        host_fiber: FiberId,
        budget: Option<Duration>,
    ) -> Result<ExecutorStep, Trap> {
        loop {
            while let Some(fiber) = self.sched.pop_ready() {
                // the executor's own children drive contained: a child's
                // trap is its nursery's first failure, reported at the
                // boundary like a sweep's drive reports it
                if fiber != host_fiber && self.owned_parked_child(fiber) {
                    match self.enter_woken(fiber) {
                        Ok(Woken::Step(step)) => {
                            if let Some(answer) = self.drive_owned_child(step, Some(host_fiber))? {
                                return Ok(ExecutorStep::HostAnswer(answer));
                            }
                        },
                        // the completed span rides the nursery ledger,
                        // and a quiet entry was cut elsewhere — the
                        // sweep's own woken-child discard
                        Ok(Woken::Completed(_) | Woken::Quiet) => {},
                        Err(trap) => {
                            self.host_release();
                            self.trap_child(fiber, &trap);
                        },
                    }
                    continue;
                }
                match self.enter_woken(fiber)? {
                    Woken::Step(step) => return Ok(ExecutorStep::Continue(step)),
                    Woken::Completed(answer) => {
                        if fiber == host_fiber {
                            return Ok(ExecutorStep::HostAnswer(answer));
                        }
                        // an orphan's completion: the answer crosses to
                        // the host, the executor keeps its fiber
                        self.host_release();
                        for value in answer.as_slice() {
                            self.host_delivery(value);
                        }
                    },
                    Woken::Quiet => {},
                }
            }
            match self.eager_start(host_fiber)? {
                EagerPass::Idle => {},
                // a child drove to a park or a death: rescan before the
                // wait — its wake may have queued a fiber, and later
                // Ready siblings still stand
                EagerPass::Drove => continue,
                EagerPass::Host(answer) => {
                    return Ok(ExecutorStep::HostAnswer(answer));
                },
            }
            // the cut pass: a parked fiber under the host's shutdown or
            // its own raised flag dies with the sentinel — the staged
            // cut rides the ready queue into the loop's entry above,
            // so its resumer's continuation drives exactly a woken
            // fiber's. With the shutdown standing and nothing left to
            // cut, the drive is over: the host owns the outcome.
            if let Some(fiber) = self.cut_candidate() {
                self.stage_cut(fiber);
                continue;
            }
            if self.shutdown {
                return Ok(ExecutorStep::Shutdown);
            }
            if !self.wait_world(budget)? {
                return Ok(ExecutorStep::Idle);
            }
        }
    }
}
