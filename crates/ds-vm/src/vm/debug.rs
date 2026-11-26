//! The driver and introspection API: the run entries, breakpoints, sampling,
//! and the backtrace over the live frames.

use super::*;

/// Where a backtrace frame stands: a bytecode position, or parked inside
/// a hosted native's reentrant call — the synthetic reentry frame
/// `[Vm::call_hosted]` pushed carries the entry-frame sentinel return pc,
/// which names no real position, so the frame's caller renders the seam
/// instead of a bogus integer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FramePc {
    /// A real position in the frame's function: the trapping uop for the
    /// innermost frame, the callee's saved return pc for callers.
    At(CodePc),
    /// The frame called a native that reenters the VM per element (the
    /// container combinators' hosted calls): the frame parks inside the
    /// native, so no bytecode position exists.
    Hosted,
}

/// One backtrace entry: a real frame plus its tail chain's elided count.
#[derive(Clone, Debug, PartialEq)]
pub struct BacktraceFrame {
    /// The frame's module.
    pub module: String,
    /// The frame's function.
    pub func: String,
    /// Where the frame is.
    pub pc: FramePc,
    /// How many logical frames this physical frame elided via tail calls
    /// since the normal call that created it.
    pub elided: u32,
    /// The frame position resolved through the module's pc-to-span table,
    /// `None` before a region (or with the section stripped): the
    /// one source channel a renderer names a frame through.
    pub location: Option<SourceLocation>,
}

/// A source position the span table named: file name plus 1-based line
/// and column. The trap renderer, the debugger, and the sampler report
/// through this one shape; nothing else in the VM carries source text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLocation {
    /// The file name as the module's span table recorded it.
    pub file: String,
    /// 1-based line.
    pub line: u32,
    /// 1-based column.
    pub col: u32,
}

/// One hop of the boundary's recorded error chain: the frame whose
/// `?` error leg propagated, innermost first, source resolved through
/// the span table at read time.
#[derive(Clone, Debug, PartialEq)]
pub struct ErrorTraceFrame {
    /// The frame's module.
    pub module: String,
    /// The frame's function.
    pub func: String,
    /// The propagation mark's pc.
    pub pc: u32,
    /// The position the span table names, `None` before a region.
    pub location: Option<SourceLocation>,
}

/// Where the debugger's armed breakpoint stopped the running fiber.
#[derive(Clone, Debug, PartialEq)]
pub struct BreakpointHit {
    /// The module the stopped function runs in.
    pub module: String,
    /// The stopped function.
    pub func: String,
    /// The stop's instruction pc.
    pub pc: u32,
    /// The position the span table names, `None` before a region.
    pub location: Option<SourceLocation>,
}

/// A source position the host arms a breakpoint at: file name plus
/// 1-based line, resolved to instruction pcs through the span tables.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Breakpoint {
    /// The file name as the module's span table records it.
    pub file: String,
    /// 1-based line.
    pub line: u32,
}

/// One sampling histogram row: a frame position and its backward-branch
/// sample count, source resolved through the span table.
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileRow {
    /// The frame's module.
    pub module: String,
    /// The frame's function.
    pub func: String,
    /// Recorded samples.
    pub hits: u64,
    /// The position the span table names, `None` before a region.
    pub location: Option<SourceLocation>,
}

/// What a debugged run ends on: the entry's own answer, or a breakpoint
/// stop the host inspects and resumes past ([`Vm::resume_fiber`]).
#[derive(Clone, Debug, PartialEq)]
pub enum DebugStop {
    /// The entry completed: its whole answer span.
    Answer(FrameAnswer),
    /// The running fiber stopped at an armed breakpoint: it stays
    /// resumable at the stop, [`Vm::breakpoint_hit`] names it.
    Breakpoint,
}

impl Vm {
    /// Runs an entry function to its `HALT` or entry-frame `RET`/`RETSPAN`
    /// and reports the resulting answer span: the root fiber's drive to
    /// completion. A multi-word answer (a tuple, a widened span)
    /// crosses whole — one word per span word, never a truncated head.
    /// The root's execution state resets per run (other pool fibers keep
    /// their state); on a `Trap` the state is kept for [`Vm::backtrace`], and
    /// the host that reports the trap must then call [`Vm::release_interrupted`]
    /// for the root: the kept frames' deinit hooks and their claim rows release
    /// only at a release door, the next run's reset, or pool-slot reuse.
    /// A root that parks with the host as resumer is re-entered with the
    /// bare `PLATFORM_NONE` tag as the park's answer — a `run` entry
    /// drives to completion; interactive stepping is
    /// [`Vm::resume_fiber`].
    pub fn run(&mut self, module: &str, func: &str, args: &[Value]) -> Result<FrameAnswer, Trap> {
        self.run_budgeted(module, func, args, None)
    }

    /// The debugger's drive: exactly [`Vm::run`], except an armed
    /// breakpoint stop ends the drive with [`DebugStop::Breakpoint`] —
    /// the fiber stays resumable at the stop, [`Vm::breakpoint_hit`]
    /// names it, and [`Vm::resume_fiber`] continues the program (to the
    /// next stop or the entry's answer, whichever comes first).
    ///
    /// # Errors
    ///
    /// [`Vm::run`]'s own.
    pub fn run_debugged(
        &mut self,
        module: &str,
        func: &str,
        args: &[Value],
    ) -> Result<DebugStop, Trap> {
        self.debug_stops = true;
        let answer = self.run_budgeted(module, func, args, None);
        self.debug_stops = false;
        match answer {
            Ok(_a) if self.breakpoint_park => {
                self.breakpoint_park = false;
                Ok(DebugStop::Breakpoint)
            },
            Ok(a) => Ok(DebugStop::Answer(a)),
            Err(e) => Err(e),
        }
    }

    /// The error-trace switch: armed, the `?` error leg's mark
    /// writes the failing frame's position into the running fiber's
    /// private ring. VM-layer state alone — no lowering input reads it,
    /// so flipping it changes no artifact.
    pub fn set_trace_errors(&mut self, on: bool) {
        self.trace_errors = on;
    }

    /// The running fiber's recorded error chain, innermost first: one
    /// hop per `?` error leg the fiber took, source resolved through the
    /// span tables at read time. The boundary prints it beside the
    /// unhandled-error message; the ring is the trap layer's diagnostic,
    /// carried by no error value.
    #[must_use]
    pub fn error_trace(&self) -> Vec<ErrorTraceFrame> {
        let fiber = &self.pool[self.current.index()];
        fiber
            .err_ring
            .iter()
            .map(|hop| {
                let (module, func, location) = self
                    .registry
                    .version(hop.version)
                    .and_then(|version| {
                        let f = version.functions().get(hop.func as usize)?;
                        let location =
                            version.location(hop.func, hop.pc).map(|(file, line, col)| {
                                SourceLocation {
                                    file: file.to_owned(),
                                    line,
                                    col,
                                }
                            });
                        Some((version.name().to_owned(), f.name.clone(), location))
                    })
                    .unwrap_or_else(|| ("(retired)".to_owned(), format!("fn {}", hop.func), None));
                ErrorTraceFrame {
                    module,
                    func,
                    pc: hop.pc,
                    location,
                }
            })
            .collect()
    }

    /// Arms the debugger's breakpoints: every span-table region whose
    /// position names one of the spots resolves to a stop pc, and the
    /// running fiber parks there before the word executes. Answers how
    /// many pcs the spots resolved to (a spot no installed module's
    /// table names resolves to nothing). An empty slice disarms.
    pub fn arm_breakpoints(&mut self, spots: &[Breakpoint]) -> usize {
        self.breakpoint_skip = None;
        self.last_breakpoint = None;
        self.breakpoint_park = false;
        if spots.is_empty() {
            self.breakpoints = None;
            return 0;
        }
        let mut set = BTreeSet::new();
        for row in 0..self.registry.version_rows() {
            let Some(version) = self.registry.version(VersionIdx(
                u32::try_from(row).expect("version rows fit u32"),
            )) else {
                continue;
            };
            let Some(spans) = version.pc_spans() else {
                continue;
            };
            for row_spans in &spans.funcs {
                for region in &row_spans.regions {
                    let named = spans.file(region.at.file).unwrap_or_default();
                    if spots
                        .iter()
                        .any(|spot| spot.file == named && spot.line == region.at.line)
                    {
                        set.insert((
                            u32::try_from(row).expect("version rows fit u32"),
                            row_spans.func,
                            region.pc,
                        ));
                    }
                }
            }
        }
        let resolved = set.len();
        self.breakpoints = Some(set);
        resolved
    }

    /// Whether the last drive ended parked at an armed breakpoint (the
    /// resume word is in the host's hand): read without consuming the
    /// stop's memory, which [`Vm::breakpoint_hit`] does.
    #[must_use]
    pub fn parked_at_breakpoint(&self) -> bool {
        self.breakpoint_park
    }

    /// The breakpoint the running fiber last stopped at, resetting the
    /// stop's memory: `None` once the program moved past it (a completed
    /// resume reads `None` exactly once).
    pub fn breakpoint_hit(&mut self) -> Option<BreakpointHit> {
        let (version, func, pc) = self.last_breakpoint.take()?;
        self.breakpoint_park = false;
        let named = self.registry.version(version).and_then(|version| {
            let f = version.functions().get(func as usize)?;
            let location = version
                .location(func, pc)
                .map(|(file, line, col)| SourceLocation {
                    file: file.to_owned(),
                    line,
                    col,
                });
            Some((version.name().to_owned(), f.name.clone(), location))
        });
        let Some((module, func_name, location)) = named else {
            return Some(BreakpointHit {
                module: "(retired)".to_owned(),
                func: format!("fn {func}"),
                pc,
                location: None,
            });
        };
        Some(BreakpointHit {
            module,
            func: func_name,
            pc,
            location,
        })
    }

    /// Arms the sampling profiler: every `every`-th taken backward
    /// branch records the running frame's position into the histogram
    /// (0 disarms and clears). The samples' source resolves only at
    /// report time, through the span tables — the shared substrate the
    /// trap renderer and the debugger read.
    pub fn arm_sampling(&mut self, every: u32) {
        self.sample_every = every;
        self.sample_tick = 0;
        if every == 0 {
            self.samples.clear();
        }
    }

    /// The sampling histogram, heaviest first: one row per sampled
    /// frame position, ties broken by module, function, and pc for a
    /// deterministic report.
    #[must_use]
    pub fn sampling_report(&self) -> Vec<ProfileRow> {
        let mut rows: Vec<((u32, u32, u32), u64)> =
            self.samples.iter().map(|(k, v)| (*k, *v)).collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        rows.into_iter()
            .map(|((version, func, pc), hits)| {
                let named = self.registry.version(VersionIdx(version)).and_then(|v| {
                    let f = v.functions().get(func as usize)?;
                    let location = v
                        .location(func, pc)
                        .map(|(file, line, col)| SourceLocation {
                            file: file.to_owned(),
                            line,
                            col,
                        });
                    Some((v.name().to_owned(), f.name.clone(), location))
                });
                let (module, func_name, location) =
                    named.unwrap_or_else(|| ("(retired)".to_owned(), format!("fn {func}"), None));
                ProfileRow {
                    module,
                    func: func_name,
                    hits,
                    location,
                }
            })
            .collect()
    }

    /// [`Vm::run`] under a total step budget (the comptime fence's
    /// runaway bound): the fueled instantiation of the dispatch loop
    /// charges one step per decoded instruction, and a drive past the
    /// budget answers [`Trap::OutOfFuel`] instead of hanging — a pure
    /// fn whose loop never terminates while its stack stays flat has
    /// no other termination witness. The budget is the calling host's
    /// policy, never a property of the code; every unbounded drive
    /// keeps [`Vm::run`].
    pub fn run_with_budget(
        &mut self,
        module: &str,
        func: &str,
        args: &[Value],
        fuel: u64,
    ) -> Result<FrameAnswer, Trap> {
        self.run_budgeted(module, func, args, Some(fuel))
    }

    /// The two run entries' shared setup and drive: `fuel` selects the
    /// dispatch loop's instantiation, so the unbounded run's dispatch
    /// cost stays untouched (the pin table's contract).
    pub(super) fn run_budgeted(
        &mut self,
        module: &str,
        func: &str,
        args: &[Value],
        fuel: Option<u64>,
    ) -> Result<FrameAnswer, Trap> {
        self.trap_pc = None;
        let (version, func_id) = self.resolve_entry(module, func, args)?;
        let frame_size = self.registry.pinned(version).functions()[func_id as usize]
            .layout
            .frame_size() as usize;
        self.host_release(); // stash any interrupted drive's context
        // the reset door of the death walk: a halted or trapped root
        // drive's kept frames fire their deinits here, then release
        // their claims before the reset discards them (the tail-native
        // abandon's count dies here now, at the next run, instead of
        // leaking to the VM's drop) — and a trapped root's abandoned
        // nurseries die with it here, their never-entered children
        // releasing their rows and slots the same way
        self.fire_teardown_hooks(FIBER_ROOT, &[]);
        self.drain_claims(FIBER_ROOT);
        self.release_owned_nurseries(FIBER_ROOT);
        let root = &mut self.pool[FIBER_ROOT.index()];
        root.segment.clear();
        root.err_ring.clear();
        Self::push_entry_frame(root, version, func_id, args, frame_size);
        root.resume = None;
        root.resumer = None;
        root.state = FiberState::Running;
        let root_slot = &mut self.pool[FIBER_ROOT.index()];
        std::mem::swap(&mut self.stack, &mut root_slot.segment);
        std::mem::swap(&mut self.callinfo, &mut root_slot.callinfo);
        std::mem::swap(&mut self.objects, &mut root_slot.objects);
        self.current = FIBER_ROOT;
        self.ctx_live = true;
        let active = Active {
            version,
            func: func_id,
        };
        let answer = match fuel {
            Some(fuel) => {
                self.fuel = fuel;
                self.root_drive::<true>(active)
            },
            None => self.root_drive::<false>(active),
        };
        self.fuel = 0;
        // the blocking twin's stop flag answers the same shutdown trap
        // the reactor path's wake does: record the observation so the
        // host reads one fact whatever door the event came through
        if matches!(answer, Err(Trap::Shutdown)) {
            self.shutdown = true;
        }
        answer
    }

    /// A root drive's completion loop, shared by the two run entries:
    /// the constant parameter selects the dispatch loop's instantiation,
    /// so the unbounded drive compiles exactly the dispatch it always
    /// had and the budgeted drive adds its per-instruction step charge
    /// inside the fueled instantiation alone.
    pub(super) fn root_drive<const FUELED: bool>(
        &mut self,
        active: Active,
    ) -> Result<FrameAnswer, Trap> {
        let mut active = active;
        let mut pc = CodePc(0);
        loop {
            match self.execute_host_step::<FUELED>(active, pc, usize::MAX) {
                Ok(DriveOutcome::Died(answer)) => {
                    self.host_release();
                    // the completed root's whole answer span crosses to
                    // the host, one delivered word per span word
                    for value in answer.as_slice() {
                        self.host_delivery(value);
                    }
                    return Ok(answer);
                },
                Ok(DriveOutcome::Yielded(_)) => {
                    // a debugged root's breakpoint stop hands the host
                    // the parked fiber: the drive ends, the host inspects
                    // (the backtrace now carries positions) and resumes
                    // through [`Vm::resume_fiber`]. An un-debugged run
                    // never parks this way, so the switch_in path is the
                    // ordinary one.
                    if self.breakpoint_park && self.debug_stops {
                        self.host_release();
                        return Ok(FrameAnswer::one(Value::Tag(PLATFORM_NONE)));
                    }
                    let (a, p) = self.switch_in(FIBER_ROOT, Value::Tag(PLATFORM_NONE));
                    active = a;
                    pc = p;
                },
                // the drive went quiet on a world park: register the
                // wait, then the single-thread executor's continuation —
                // the next ready fiber (FIFO), the reactor's wait
                // bridging the gap when nothing is runnable
                Ok(DriveOutcome::WorldPark(req)) => {
                    if let ParkKind::World(wait) = req.kind {
                        self.register_wait(req.fiber, wait);
                    }
                    match self.executor_continue(FIBER_ROOT, None) {
                        Ok(ExecutorStep::Continue((a, p))) => {
                            active = a;
                            pc = p;
                        },
                        Ok(ExecutorStep::HostAnswer(answer)) => {
                            self.host_release();
                            for value in answer.as_slice() {
                                self.host_delivery(value);
                            }
                            return Ok(answer);
                        },
                        Ok(ExecutorStep::Shutdown) => {
                            self.host_release();
                            return Err(Trap::Shutdown);
                        },
                        // the blocking drive never idles: its wait loops
                        // until a wake
                        Ok(ExecutorStep::Idle) => {
                            unreachable!("the blocking executor wait never idles")
                        },
                        Err(trap) => {
                            self.trap_pc = Some(pc);
                            self.host_release();
                            self.trap_child(self.current, &trap);
                            self.trap_delivery(&trap);
                            return Err(trap);
                        },
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
                    // the drive may have ended on a nursery child entered
                    // in-language: ledger it and raise its nursery's flag
                    self.trap_child(self.current, &trap);
                    // the trap's payload word crosses to the host
                    self.trap_delivery(&trap);
                    return Err(trap);
                },
            }
        }
    }

    /// Whether the live call chain still carries this drive's reentry
    /// frame (the sentinel-metadata frame [`Vm::call_hosted`] pushed at
    /// `boundary - 1`): the honesty condition for effects that cannot
    /// suspend the Rust-side native loop. A fiber switched in mid-drive
    /// never carries one — a resumable fiber parks only at switch
    /// points, never mid-native — so the sentinel test cannot
    /// false-positive across a switch.
    pub(super) fn reentry_live(&self, boundary: usize) -> bool {
        if boundary == usize::MAX || self.callinfo.len() < boundary {
            return false;
        }
        matches!(
            self.stack[self.callinfo[boundary - 1].fp],
            Word::Meta(m) if m.return_pc.0 == u32::MAX
        )
    }

    /// The outgoing-argument area of the frame below the executing one,
    /// when the chain has one: a tail site's answer overlays that caller's
    /// area, never the reusing frame's own.
    pub(super) fn caller_arg_area(&self) -> Option<usize> {
        let at = self.callinfo.len().checked_sub(2)?;
        let caller = &self.callinfo[at];
        Some(
            self.registry.pinned(caller.module).functions()[caller.func as usize]
                .layout
                .arg_area_len(),
        )
    }

    /// Renders the live frames of the fiber the last drive ended on
    /// (innermost first): each real frame with its position plus the
    /// tail-chain elided count. The counter is always available;
    /// per-elision `ElisionRecord`s for lazy materialization come with
    /// per-fiber debug recording, so the count reports how many logical frames
    /// a debugger would materialize later.
    #[must_use]
    pub fn backtrace(&self) -> Vec<BacktraceFrame> {
        let fiber = &self.pool[self.current.index()];
        let mut frames = Vec::with_capacity(fiber.callinfo.len());
        for depth in (0..fiber.callinfo.len()).rev() {
            let ci = &fiber.callinfo[depth];
            let f = &self.registry.pinned(ci.module).functions()[ci.func as usize];
            let pc = if depth + 1 == fiber.callinfo.len() {
                FramePc::At(self.trap_pc.unwrap_or(CodePc(0)))
            } else {
                match fiber.segment[fiber.callinfo[depth + 1].fp] {
                    Word::Meta(m) if m.return_pc.0 == u32::MAX => FramePc::Hosted,
                    Word::Meta(m) => FramePc::At(m.return_pc),
                    Word::Slot(_) => FramePc::At(CodePc(0)),
                }
            };
            let elided = match fiber.segment[ci.fp] {
                Word::Meta(m) => m.tail.elided,
                Word::Slot(_) => 0,
            };
            let version = self.registry.pinned(ci.module);
            let location = match pc {
                FramePc::At(at) => {
                    version
                        .location(ci.func, at.0)
                        .map(|(file, line, col)| SourceLocation {
                            file: file.to_owned(),
                            line,
                            col,
                        })
                },
                FramePc::Hosted => None,
            };
            frames.push(BacktraceFrame {
                module: version.name().to_owned(),
                func: f.name.clone(),
                pc,
                elided,
                location,
            });
        }
        frames
    }
}
