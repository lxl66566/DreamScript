//! The REPL's resumed frame: the seam that keeps one entry frame alive
//! across submissions and appends into it, so prior statements run once
//! and only the new statement computes.
//!
//! # The model
//!
//! The REPL owns a growing session module whose `main` carries every
//! statement submission. Each round lowers the whole session (the check
//! stays session-length; block-grain memoization renews the untouched
//! blocks), then the seam takes over:
//!
//! 1. [`splice_park`] rewrites the lowered module's `main`: the answer staging the exit's `RET`
//!    would perform (a `MOV` run into the outgoing-argument area) is emitted at the splice point,
//!    followed by a call to the `repl_park` control native — and the frame's ordinary exit sequence
//!    becomes dead code past the park. The lowerer's per-statement value-end facts place the
//!    splice; the caller picks the position.
//! 2. [`Vm::repl_start`] pushes the entry frame and drives to the park: the fiber parks with the
//!    host as resumer, the frame alive.
//! 3. [`Vm::repl_resume`] runs after the caller installs the next round's module (the two-slot swap
//!    retires the prior version; the parked frame pins it until the re-point), re-points the parked
//!    fiber's resume into the new version at the position where the new submission's code begins,
//!    grows the frame to the new layout (append-only statements grow monotonically, and
//!    chunk-append growth keeps every live word's address), and drives to the next park. The prior
//!    statements' code is skipped by the resume point: side effects never repeat.
//! 4. [`Vm::repl_rollback`] restores a trapped submission's frame: the partial run's own deinit
//!    claims fire exactly once (the teardown pass over the kept frames, gated by the parked words'
//!    establishment bands and the pre-step ledger reading — a claim already live when the step
//!    entered, and a value moved into a dying frame through a parked binding, both survive), then
//!    the pushed frames and the partial statement's slots die unwired, so the words past the parked
//!    extent zero (no stale claim band or dangling reference survives into a later version's slot
//!    reads), the counts the partial run acquired release against a pre-step ledger reading, the
//!    line's own open nursery tears down beside the same pre-step nursery reading (never-entered
//!    children release cold, the slot recycles — the trapped-drive release's discipline, the root
//!    surviving its own rollback being the reason the dead-owner door never catches it), and the
//!    fiber re-parks at the same resume position — the ruled behavior that a trapping line never
//!    commits.
//! 5. [`Vm::repl_drop_binding`] fires one live binding's deinit exactly once at `:drop`, and
//!    [`Vm::repl_teardown`] is the session-end scope exit: every surviving deinit claim fires, then
//!    the claims drain.
//!
//! # What the model refuses
//!
//! The rollback's honesty: effects that already ran stand (a trapped
//! line's object mutations and completed assignments — a store that
//! landed stays landed), nothing fires twice (a hook that traps inside
//! the rollback pass keeps its effects and ends the pass there, the
//! remaining claims draining unfired), and a trapped line's arena
//! temporaries stay allocated until the session-end drain — the arena
//! never rewinds below live bindings' bytes, a landed store possibly
//! holding arena bytes above any pre-step mark. The frame stays
//! consistent through all of this because the resumed-frame mode pins
//! every binder (the lowerer's rule): a session-live value's count never
//! leaves at a text-relative last use, so a partial run cannot dec a
//! prior binding under the frame.

use std::time::{Duration, Instant};

use ds_abi::{CodePc, FrameLayout, Inst, SlotType, Uop};
use ds_mod::{Constant, FuncRefKey, Module};

use crate::{
    Trap,
    claims::{CLAIM_BAND_CLEAR, ClaimReading},
    fiber::{FIBER_ROOT, FiberState, ResumePoint},
    native::BUILTIN_REPL_PARK,
    nursery::NurseryState,
    value::{Value, Word},
    vm::{Active, CallInfo, DriveOutcome, ExecutorStep, FrameAnswer, Vm},
};

/// Why [`splice_park`] cannot rewrite a module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpliceError {
    /// The named function is not in the module.
    UnknownFunction,
    /// The splice point is not inside the function's body.
    BadPc {
        /// The rejected position.
        at: u32,
        /// The body's word count.
        len: u32,
    },
    /// The body does not end in the single-exit `RET`/`RETSPAN` shape the
    /// answer staging mirrors (a tail call, or a hand-assembled stream).
    NoSingleExit,
    /// The widened frame exceeds the ISA's u8 slot space.
    FrameTooWide {
        /// The needed slot count.
        need: usize,
    },
    /// A word could not be re-encoded (the assembler's bias window).
    Encode,
}

impl std::fmt::Display for SpliceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownFunction => write!(f, "the module holds no such function"),
            Self::BadPc { at, len } => {
                write!(f, "the splice point {at} is outside the {len}-word body")
            },
            Self::NoSingleExit => write!(
                f,
                "the body does not end in the single-exit return shape the park mirrors"
            ),
            Self::FrameTooWide { need } => {
                write!(
                    f,
                    "the widened frame needs {need} slots, past the u8 slot space"
                )
            },
            Self::Encode => write!(f, "a spliced word could not be re-encoded"),
        }
    }
}

impl std::error::Error for SpliceError {}

/// The answer channel one spliced park carries: the body slots the exit's
/// `RET` reads, staged into the outgoing-argument area word for word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AnswerSpan {
    /// The first body slot of the answer.
    src: u8,
    /// The answer's word count.
    width: u8,
}

/// Rewrites `module`'s `func` so the frame parks instead of exiting: the
/// answer staging `RET` would perform is emitted at `at`, then the
/// `repl_park` call; the ordinary exit sequence stays past the park as
/// dead code no resume ever reaches (the next round's resume points at
/// its own spliced position). The layout's outgoing-argument area widens
/// to the answer's width when the body's own calls never needed it, and
/// `FUNCF` follows. Every other function and section rides unchanged.
///
/// # Errors
///
/// [`SpliceError`] names every refusal: the function, the point, the exit
/// shape, and the widened frame's slot space.
pub fn splice_park(mut module: Module, func: &str, at: u32) -> Result<Module, SpliceError> {
    let (name, layout, mut constants, code) = module
        .functions()
        .iter()
        .find(|f| f.name() == func)
        .ok_or(SpliceError::UnknownFunction)?
        .clone()
        .into_parts();
    let len = u32::try_from(code.len()).unwrap_or(u32::MAX);
    if at == 0 || at >= len {
        return Err(SpliceError::BadPc { at, len });
    }
    let answer = answer_span(&code).ok_or(SpliceError::NoSingleExit)?;
    let body_len = layout.body_len();
    // the park callee: the builtin table's control row, one pool entry
    let callee = Constant::FuncRef {
        key: FuncRefKey::Native(crate::native::BUILTIN_MODULE.to_owned()),
        func: BUILTIN_REPL_PARK,
    };
    let index = if let Some(at) = constants.iter().position(|c| *c == callee) {
        u16::try_from(at).expect("a pool index fits u16")
    } else {
        let index = u16::try_from(constants.len()).map_err(|_| SpliceError::FrameTooWide {
            need: constants.len(),
        })?;
        constants.push(callee);
        index
    };
    let mut spliced: Vec<Inst> = Vec::with_capacity(code.len() + 1 + usize::from(answer.width));
    for (pc, &word) in code.iter().enumerate() {
        let decoded = Inst::decode(u32::try_from(pc).expect("pc fits u32"), word)
            .map_err(|_| SpliceError::Encode)?;
        if usize::try_from(at).is_ok_and(|at| at == pc) {
            // the answer staging mirrors the exit's RET: the span's words
            // move into the outgoing-argument area, then the park call
            for i in 0..usize::from(answer.width) {
                let dst = body_len + i;
                spliced.push(Inst::Uop(Uop::Mov {
                    dst: u8::try_from(dst).map_err(|_| SpliceError::FrameTooWide { need: dst })?,
                    src: answer
                        .src
                        .checked_add(u8::try_from(i).expect("width fits u8"))
                        .expect("the span stays inside the u8 slot space"),
                }));
            }
            spliced.push(Inst::Uop(Uop::Call {
                callee: ds_abi::ConstIdx(index),
                nargs: answer.width,
            }));
        }
        spliced.push(decoded);
    }
    // the widened layout: the answer staging needs its width in the
    // outgoing-argument area, and FUNCF follows the new slot total. The
    // area's words go Raw: the answer's kinds are the answer's own (a
    // unit answer stages an I64 where a string call staged its Ref), and
    // Raw staging is the wildcard every existing call site re-checks at
    // runtime, so the body's own calls keep their behavior.
    let area = layout.arg_area_len().max(usize::from(answer.width));
    let mut slots = layout.slots().to_vec();
    slots.truncate(slots.len() - layout.arg_area_len());
    slots.extend(std::iter::repeat_n(SlotType::Raw, area));
    let total =
        u16::try_from(slots.len()).map_err(|_| SpliceError::FrameTooWide { need: slots.len() })?;
    spliced[0] = Inst::Uop(Uop::Funcf { frame_slots: total });
    let layout = FrameLayout::new(slots, layout.params_len(), area).map_err(|_| {
        SpliceError::FrameTooWide {
            need: usize::from(total),
        }
    })?;
    let function =
        ds_mod::Function::new(name, layout, constants, spliced).map_err(|_| SpliceError::Encode)?;
    let replaced = module.replace_function(function);
    debug_assert!(replaced, "the position was found above");
    Ok(module)
}

/// The body's single-exit answer: the final word must be the `RET` (or
/// `RETSPAN`) the tail emitted — the wrapper the REPL builds around the
/// last expression guarantees exactly one exit — and its source slots are
/// the answer the park stages.
fn answer_span(code: &[u32]) -> Option<AnswerSpan> {
    let last = u32::try_from(code.len().checked_sub(1)?).expect("pc fits u32");
    match Inst::decode(last, *code.last()?) {
        Ok(Inst::Uop(Uop::Ret { src })) => Some(AnswerSpan { src, width: 1 }),
        Ok(Inst::Uop(Uop::RetSpan { src, n })) => Some(AnswerSpan { src, width: n }),
        _ => None,
    }
}

/// Which REPL entry the session state refused: each entry names the
/// root-fiber state its protocol needs, and the refusal is that state
/// itself, never a callable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplEntry {
    /// [`Vm::repl_start`]: the root still holds a parked session (a live
    /// session resumes instead).
    Start,
    /// [`Vm::repl_resume`]: no parked session stands to resume.
    Resume,
    /// [`Vm::repl_drop_binding`]: no parked session holds the binding.
    DropBinding,
}

/// How one REPL drive ended: parked at the spliced call holding the
/// submission's answer span, or the entry frame completed (a defensive
/// outcome — the spliced body has no live exit path, so a completion
/// means hand-assembled code reached the dead region).
#[derive(Clone, Debug, PartialEq)]
pub enum ReplOutcome {
    /// The fiber parked at the seam's call; the whole answer span rides
    /// here, never a truncated head word.
    Parked(FrameAnswer),
    /// The entry frame completed anyway; its whole answer span rides
    /// here, and the frame is gone so the caller must rebuild before the
    /// next submission.
    Died(FrameAnswer),
    /// The submission parked on a world op mid-drive: the wait stands
    /// registered, the frame holds its partial run, and the drive ends
    /// handing the host its own waits back — no answer rides here. The
    /// host pumps [`Vm::repl_continue`] between its own waits until the
    /// submission parks at the seam (its answer reporting then) or ends;
    /// nothing else may enter the parked frame, and the next splice
    /// resumes only once the drive lands (the caller's
    /// [`Vm::repl_resume`] is the splice's own entry, never this park's).
    WorldParked,
}

/// The pre-step state a trapped submission's rollback restores to: where
/// the parked frame's committed words ended, the parked words themselves
/// (the partial gate's band witness), and the ledger reading the partial
/// run diffed against.
#[derive(Debug)]
pub(crate) struct ReplPre {
    /// The parked version's body length: words past `1 + body` are the
    /// partial run's to zero.
    body: usize,
    /// The parked frame's whole extent: the segment truncation target.
    extent: usize,
    /// The parked frame's words as the step entered: a claim whose
    /// establishment band was already set here is the parked frame's own,
    /// surviving the rollback.
    parked: Vec<Word>,
    /// The claim reading before the step.
    claims: ClaimReading,
    /// The nursery pool's states as the step entered: the rollback's
    /// nursery gate — a root-owned open nursery at a slot the reading
    /// held Dead or never knew is the line's own, torn down with it.
    nurseries: Vec<NurseryState>,
}

impl Vm {
    /// The first submission's drive: a fresh root entry frame for
    /// `module.func`, run from its first word to the spliced park. The
    /// root must not be parked (a live session resumes instead).
    ///
    /// # Errors
    ///
    /// [`Trap::ReplState`] when the root still holds a parked session (a
    /// live session resumes instead); [`Trap::UnknownFunction`] when the
    /// entry is absent; the execution traps otherwise.
    pub fn repl_start(&mut self, module: &str, func: &str) -> Result<ReplOutcome, Trap> {
        let (version, func_id) = self.resolve_repl_entry(module, func)?;
        if self.pool[FIBER_ROOT.index()].resumable() {
            return Err(Trap::ReplState {
                entry: ReplEntry::Start,
            });
        }
        self.repl_pre = Some(ReplPre {
            body: 0,
            extent: 0,
            parked: Vec::new(),
            claims: self.claims[FIBER_ROOT.index()].reading(),
            nurseries: self.nursery_states(),
        });
        let frame_size = self.frame_size_of(version, func_id);
        let root = &mut self.pool[FIBER_ROOT.index()];
        root.segment.clear();
        Self::push_entry_frame(root, version, func_id, &[], frame_size);
        root.resume = Some(ResumePoint {
            version,
            func: func_id,
            pc: CodePc(0),
        });
        root.state = FiberState::Ready;
        root.resumer = None;
        self.repl_park = None;
        let (active, pc) = self.switch_in(FIBER_ROOT, Value::I64(0));
        self.repl_drive(active, pc)
    }

    /// The next submission's drive: re-point the parked root fiber into
    /// the freshly installed version at `pc` (where the new submission's
    /// code begins — the caller reads it from the lowerer's per-statement
    /// facts), grow the entry frame to the new layout, and drive to the
    /// next park.
    ///
    /// # Errors
    ///
    /// [`Trap::ReplState`] when no parked session stands (the root is
    /// empty; `repl_start` is that state's entry); [`Trap::UnknownFunction`]
    /// when the entry is absent; the execution traps otherwise (the
    /// caller rolls back).
    pub fn repl_resume(
        &mut self,
        module: &str,
        func: &str,
        pc: CodePc,
    ) -> Result<ReplOutcome, Trap> {
        let (version, func_id) = self.resolve_repl_entry(module, func)?;
        let root = &mut self.pool[FIBER_ROOT.index()];
        if !root.resumable() {
            return Err(Trap::ReplState {
                entry: ReplEntry::Resume,
            });
        }
        // the pre-step state, read off the parked fiber before any move
        let parked = root.resume.as_ref().expect("resumable carries a point");
        let parked_body = self.registry.pinned(parked.version).functions()[parked.func as usize]
            .layout
            .body_len();
        let pre = ReplPre {
            body: parked_body,
            extent: root.segment.len(),
            parked: (0..root.segment.len()).map(|at| root.segment[at]).collect(),
            claims: self.claims[FIBER_ROOT.index()].reading(),
            nurseries: self.nursery_states(),
        };
        let frame_size = self.frame_size_of(version, func_id);
        if frame_size > self.limit {
            return Err(Trap::StackOverflow {
                needed: frame_size,
                limit: self.limit,
            });
        }
        let root = &mut self.pool[FIBER_ROOT.index()];
        root.segment.grow_to(frame_size);
        // the metadata's frame size follows the new layout; the arena
        // flag rides the word's top bit and survives the write
        if let Word::Meta(meta) = &mut root.segment[0] {
            let arena = meta.arena_active();
            meta.frame_size = u32::try_from(frame_size).expect("frame size fits u32");
            if arena {
                meta.mark_arena_active();
            }
        }
        root.max_len = root.max_len.max(frame_size);
        root.callinfo.truncate(1);
        root.callinfo[0] = CallInfo {
            fp: 0,
            module: version,
            func: func_id,
        };
        root.resume = Some(ResumePoint {
            version,
            func: func_id,
            pc,
        });
        root.resumer = None;
        self.repl_pre = Some(pre);
        self.repl_park = None;
        let (active, at) = self.switch_in(FIBER_ROOT, Value::I64(0));
        self.repl_drive(active, at)
    }

    /// A trapped submission's rollback: fire the partial run's own deinit
    /// claims exactly once (the teardown pass over the kept frames, gated
    /// by the parked words' establishment bands and the pre-step ledger
    /// reading — a claim already live when the step entered, and a value
    /// moved into a dying frame through a parked binding, both survive),
    /// zero the partial run's words, release
    /// the counts it acquired, tear down the line's own open nursery under
    /// the pre-step nursery reading (never-entered children release cold,
    /// the slot recycles), restore the parked resume at the position
    /// the failed step entered. The ruled behavior — a trapping line never
    /// commits, the frame does not advance, and its own deinit-carrying
    /// bindings die their one death here: effects that already ran stand,
    /// nothing fires twice.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownCallee`] when the entry is absent (the re-park
    /// resolves it through the current slot).
    pub fn repl_rollback(&mut self, module: &str, func: &str, pc: CodePc) -> Result<(), Trap> {
        let (version, func_id) = self.resolve_repl_entry(module, func)?;
        // a world-parked continuation's wait dies with the rollback: the
        // interest deregisters and the row drops, so no later wake
        // delivers an answer to a frame the rollback re-parked elsewhere
        self.cancel_wait(FIBER_ROOT);
        let pre = self.repl_pre.take().unwrap_or(ReplPre {
            body: 0,
            extent: 0,
            parked: Vec::new(),
            claims: self.claims[FIBER_ROOT.index()].reading(),
            nurseries: self.nursery_states(),
        });
        self.fire_teardown_hooks_past(FIBER_ROOT, &[], Some(&pre.claims), Some(&pre.parked));
        self.claims[FIBER_ROOT.index()].release_since(&pre.claims);
        self.release_line_nurseries(&pre.nurseries);
        let root = &mut self.pool[FIBER_ROOT.index()];
        // the partial run's region: everything past the parked body —
        // the failed statement's slots (wherever the new layout put
        // them, over the retired overlay or past the old extent) plus
        // every frame it pushed — zeroes before any truncation, so no
        // stale claim band or dangling reference survives a later
        // version's slot reads
        for at in (1 + pre.body)..root.segment.len() {
            root.segment[at] = Word::Slot(Value::I64(0));
        }
        root.segment.truncate(pre.extent);
        if pre.extent == 0 {
            root.callinfo.clear();
            root.resume = None;
            root.state = FiberState::Ready;
            root.resumer = None;
            return Ok(());
        }
        root.callinfo.truncate(1);
        root.callinfo[0] = CallInfo {
            fp: 0,
            module: version,
            func: func_id,
        };
        root.resume = Some(ResumePoint {
            version,
            func: func_id,
            pc,
        });
        root.state = FiberState::Suspended;
        root.resumer = None;
        Ok(())
    }

    /// The session-end scope exit: every surviving deinit claim of the
    /// parked frame fires (innermost frame, drop-site order — one frame
    /// here), then the claims drain and the root resets. Idempotent.
    pub fn repl_teardown(&mut self) {
        // an in-flight submission's wait dies with the session: no wake
        // may deliver past the scope exit
        self.cancel_wait(FIBER_ROOT);
        self.fire_teardown_hooks(FIBER_ROOT, &[]);
        self.drain_claims(FIBER_ROOT);
        let root = &mut self.pool[FIBER_ROOT.index()];
        root.segment.clear();
        root.callinfo.clear();
        root.resume = None;
        root.state = FiberState::Ready;
        root.resumer = None;
        self.repl_pre = None;
        self.repl_park = None;
    }

    /// `:drop <name>`: one live binding's death, on demand. The words are
    /// the binding's body slots (the lowerer's placement facts answer
    /// them); when the binding's type carries a deinit claim the hook
    /// fires exactly once — the same gate the teardown pass reads — then
    /// the claims release and the words neuter, so no later door fires
    /// the claim again. A hook that traps keeps its effects (the teardown
    /// rule) and still releases: the error rides home for the report.
    ///
    /// # Errors
    ///
    /// [`Trap::ReplState`] when no parked session stands to hold the
    /// binding; a hook's own trap otherwise.
    pub fn repl_drop_binding(
        &mut self,
        module: &str,
        func: &str,
        words: &[u8],
    ) -> Result<(), Trap> {
        let (version, func_id) = self.resolve_repl_entry(module, func)?;
        let root = &mut self.pool[FIBER_ROOT.index()];
        if !root.resumable() {
            return Err(Trap::ReplState {
                entry: ReplEntry::DropBinding,
            });
        }
        let extent = root.segment.len();
        let before = self.claims[FIBER_ROOT.index()].reading();
        let module_id = self
            .registry
            .module_id(self.registry.pinned(version).name())
            .expect("the entry resolved through a registered module name");
        let sites = self.hook_sites_for(version, func_id, module_id);
        let site = sites
            .iter()
            .copied()
            .find(|s| words.first() == Some(&s.base) && words.len() == usize::from(s.width));
        // the parked fiber's context swaps in for the hook drive (the
        // takeover dance the teardown pass performs)
        let takeover = !(self.ctx_live && self.current == FIBER_ROOT);
        if takeover {
            self.host_release();
            let slot = &mut self.pool[FIBER_ROOT.index()];
            std::mem::swap(&mut self.stack, &mut slot.segment);
            std::mem::swap(&mut self.callinfo, &mut slot.callinfo);
            std::mem::swap(&mut self.objects, &mut slot.objects);
            self.current = FIBER_ROOT;
            self.ctx_live = true;
        }
        let mut hook_trap = None;
        if let Some(site) = site
            && let Some((callee, values)) =
                self.gate_teardown_site(FIBER_ROOT, 0, version, site, module_id, &[], &[], None)
        {
            // the fired claim's band clears in the frame word itself, the
            // collect pass's discipline: no later door fires it twice
            if let Some(band) = site.band {
                self.stack[1 + usize::from(band)] = Word::Slot(Value::Tag(CLAIM_BAND_CLEAR));
            }
            if let Err(trap) = self.call_hosted(Value::Fn(callee), &values) {
                hook_trap = Some(trap);
            }
        }
        for &word in words {
            let at = 1 + usize::from(word);
            if let Some(Word::Slot(Value::Ref(id))) = self.stack.get(at) {
                self.claims[FIBER_ROOT.index()].release(*id);
            }
            if at < self.stack.len() {
                self.stack[at] = Word::Slot(Value::I64(0));
            }
        }
        // a trapping hook leaves its reentry frame pushed: the parked
        // extent restores, and the hook's own acquisitions release with
        // the reading diff
        if hook_trap.is_some() {
            self.stack.truncate(extent);
            self.callinfo.truncate(1);
            self.claims[FIBER_ROOT.index()].release_since(&before);
        }
        if takeover {
            self.host_release();
        }
        match hook_trap {
            Some(trap) => Err(trap),
            None => Ok(()),
        }
    }

    /// Whether the root fiber holds a parked frame the next submission
    /// resumes.
    #[must_use]
    pub fn repl_parked(&self) -> bool {
        self.pool[FIBER_ROOT.index()].resumable()
    }

    /// The nursery pool's states, the rollback's pre-step reading.
    fn nursery_states(&self) -> Vec<NurseryState> {
        self.nurseries.iter().map(|n| n.state).collect()
    }

    /// Resolves and checks the session entry (zero arguments: the REPL
    /// owns `main`'s shape).
    fn resolve_repl_entry(
        &self,
        module: &str,
        func: &str,
    ) -> Result<(crate::module::VersionIdx, u32), Trap> {
        let (version, func_id) =
            self.registry
                .resolve(module, func)
                .ok_or_else(|| Trap::UnknownFunction {
                    module: module.into(),
                    func: func.into(),
                })?;
        self.entry_checks(version, func_id, &[])?;
        Ok((version, func_id))
    }

    /// The entry frame's word span.
    fn frame_size_of(&self, version: crate::module::VersionIdx, func_id: u32) -> usize {
        self.registry.pinned(version).functions()[func_id as usize]
            .layout
            .frame_size() as usize
    }

    /// The shared drive: to the next park (the whole answer span rides)
    /// or the defensive completion. A stop the park seam did not mark is
    /// the root fiber handing the host its continuation — the drive
    /// re-enters the root and keeps going, exactly `Vm::run`'s park arm,
    /// so the submission's code after the stop still runs.
    fn repl_drive(&mut self, mut active: Active, mut pc: CodePc) -> Result<ReplOutcome, Trap> {
        loop {
            // one host step drains every interlock inside itself and
            // answers a final outcome
            match self.execute_host_step::<false>(active, pc, usize::MAX) {
                Ok(DriveOutcome::Yielded(_)) => {
                    if let Some(answer) = self.repl_park.take() {
                        self.host_release();
                        return Ok(ReplOutcome::Parked(answer));
                    }
                    // an unmarked park left the fiber with the host as
                    // resumer (a hand-assembled module naming the splice's
                    // park seam without the marker): the continuation
                    // stays the submission's own, so the root re-enters at
                    // the parked call's answer overlay and the drive
                    // continues, the payload the scalar word the answer
                    // overlay's typed rows read
                    let (next, at) = self.switch_in(FIBER_ROOT, Value::I64(0));
                    active = next;
                    pc = at;
                },
                Ok(DriveOutcome::Died(answer)) => {
                    self.host_release();
                    for value in answer.as_slice() {
                        self.host_delivery(value);
                    }
                    return Ok(ReplOutcome::Died(answer));
                },
                // a world park under the REPL drive: the wait registers,
                // the drive's context releases, and the submission stays
                // in flight — the host keeps its own waits (the prompt
                // read) and pumps `repl_continue` between them until the
                // submission parks at the seam or ends
                Ok(DriveOutcome::WorldPark(req)) => {
                    if let crate::vm::ParkKind::World(wait) = req.kind {
                        self.register_wait(req.fiber, wait);
                    }
                    self.host_release();
                    return Ok(ReplOutcome::WorldParked);
                },
                // a host-level drive passes no boundary and answers no
                // reentry frame
                Ok(DriveOutcome::Frame(_) | DriveOutcome::Interlock(_)) => {
                    unreachable!("a REPL drive answers no reentry frame or interlock")
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
    }

    /// The in-flight submission's continuation: one bounded pump of the
    /// executor/wait seam. The root's wait stands — no entry switch, a
    /// woken fiber is the executor's to enter — and `budget` bounds the
    /// reactor wait: [`ReplOutcome::Parked`] answers the submission's
    /// landing at the seam (the answer reporting and the binders'
    /// commit are the host's, exactly a drive-time landing's),
    /// [`ReplOutcome::WorldParked`] a budget spent with the wait still
    /// standing, [`ReplOutcome::Died`] the frame's defensive completion,
    /// and a trap rolls back exactly a drive-time trap would (the host
    /// calls [`Vm::repl_rollback`], which also drops the parked wait).
    /// An idle seam (nothing runnable, nothing able to wake) answers
    /// `WorldParked` at once — the blocking drives' stall guard is the
    /// honest deadlock, a pumping host idles instead.
    ///
    /// # Errors
    ///
    /// [`Trap::Shutdown`] when the host's shutdown door cut the parked
    /// submission; the execution traps otherwise (the caller rolls
    /// back).
    pub fn repl_continue(&mut self, budget: Option<Duration>) -> Result<ReplOutcome, Trap> {
        if !self.sched_has_work() {
            return Ok(ReplOutcome::WorldParked);
        }
        let until = budget.map(|budget| Instant::now() + budget);
        let mut step: Option<(Active, CodePc)> = None;
        let mut last_pc: Option<CodePc> = None;
        loop {
            if let Some((active, pc)) = step.take() {
                last_pc = Some(pc);
                match self.execute_host_step::<false>(active, pc, usize::MAX) {
                    Ok(DriveOutcome::Yielded(_)) => {
                        // the session fiber's own park serves the prompt
                        // (the splice's marker, or the host as its
                        // resumer); a foreign fiber's park belongs to its
                        // own resumer, and the pump keeps going
                        if self.current != FIBER_ROOT {
                            continue;
                        }
                        if let Some(answer) = self.repl_park.take() {
                            self.host_release();
                            return Ok(ReplOutcome::Parked(answer));
                        }
                        // an unmarked park left the fiber with the host
                        // as resumer: the root re-enters at the parked
                        // call's answer overlay and the drive continues,
                        // the payload the scalar word the overlay's typed
                        // rows read
                        let (next, at) = self.switch_in(FIBER_ROOT, Value::I64(0));
                        step = Some((next, at));
                        continue;
                    },
                    Ok(DriveOutcome::Died(answer)) => {
                        self.host_release();
                        for value in answer.as_slice() {
                            self.host_delivery(value);
                        }
                        // the session fiber's completion ends the drive;
                        // a foreign fiber the executor handed this pump
                        // crossed its answer above, and the pump keeps
                        // going — the drive answers its own fiber alone
                        if self.current == FIBER_ROOT {
                            return Ok(ReplOutcome::Died(answer));
                        }
                    },
                    Ok(DriveOutcome::WorldPark(req)) => {
                        if let crate::vm::ParkKind::World(wait) = req.kind {
                            self.register_wait(req.fiber, wait);
                        }
                        // the executor keeps going inside the budget:
                        // ready siblings may run while the wait stands
                    },
                    Ok(DriveOutcome::Frame(_) | DriveOutcome::Interlock(_)) => {
                        unreachable!("a REPL drive answers no reentry frame or interlock")
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
            match self.executor_continue(FIBER_ROOT, remaining) {
                Ok(ExecutorStep::Continue((active, pc))) => {
                    step = Some((active, pc));
                },
                Ok(ExecutorStep::HostAnswer(answer)) => {
                    self.host_release();
                    for value in answer.as_slice() {
                        self.host_delivery(value);
                    }
                    return Ok(ReplOutcome::Died(answer));
                },
                // the host's shutdown cut the parked submission: the
                // trap carries no decision — the REPL host answers its
                // own policy
                Ok(ExecutorStep::Shutdown) => {
                    self.host_release();
                    return Err(Trap::Shutdown);
                },
                Ok(ExecutorStep::Idle) => return Ok(ReplOutcome::WorldParked),
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
}
