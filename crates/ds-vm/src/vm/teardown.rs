//! The claims-driven teardown: the drain walk over a dying fiber's frames, the
//! deinit-hook directories, and the drive the hooks run under.

use super::*;

impl Vm {
    /// The teardown drain walk: every claim the fiber's frames own,
    /// released, routed to each word's own heap — the one channel for
    /// cross-heap count mutation on a death. A bogus word (a
    /// hand-assembled release without an acquire) errors and is ignored:
    /// the walk releases what the store knows.
    pub(crate) fn drain_claims(&mut self, fiber: FiberId) {
        self.drain_claims_except(fiber, &[]);
    }

    /// [`Self::drain_claims`] holding `held` back — a word about to ride
    /// its recorded outcome to a delivery keeps its claim. The claims are
    /// taken before any dec runs, so both doors ride the walk's no-ledger
    /// dec: a fiber running under the live context through its context
    /// store, every other case (and every fiber while the drive is parked)
    /// through the pool, the scratch never receiving a walk's dec.
    pub(super) fn drain_claims_except(&mut self, fiber: FiberId, held: &[RefId]) {
        let live_here = self.ctx_live && fiber == self.current;
        for (word, n) in self.claims[fiber.index()].take_all_except(held) {
            for _ in 0..n {
                let _ = if live_here {
                    self.heaps().dec_walk(word)
                } else {
                    self.heaps_walk().dec_walk(word)
                };
            }
        }
        // the door's frames never run again, so the fiber's intern pins
        // release with the walk (interrupted, marking, reuse, and reset
        // deaths; a live-here death releases again in `die_current`,
        // idempotently)
        if live_here {
            self.objects.release_pins();
        } else {
            self.pool[fiber.index()].objects.release_pins();
        }
        // the door's frames never run again, so the fiber's arena
        // temporaries die with them: a wholesale rewind to the base, the
        // frame marks dropping with it (a recycled slot starts empty)
        self.pool[fiber.index()].arena.release_all();
    }

    /// The teardown deinit pass (teardown walks frames running deinits):
    /// every deinit claim of the dying fiber's kept frames
    /// whose liveness the claim ledger certifies fires its hook, exactly
    /// once, deterministically — frames innermost-first (an unwind's
    /// order), each frame's claims in directory (drop-site) order. The
    /// drive rides [`Vm::call_hosted`] on a live context: the dying
    /// fiber's own when it is current (the cancel-cut door), a takeover
    /// of its parked context otherwise (interrupted and dead fibers carry
    /// no resume point, so the three ownership swaps run directly). A
    /// takeover over a live third fiber parks that fiber's context first
    /// and re-adopts it after the pass: the door's caller continues on
    /// its own frames.
    ///
    /// The drive rule: a teardown hook must return to its driver. A hook
    /// that parks (a yield, a resume switch), dies (a `HALT`), traps,
    /// observes a raised cancel flag at a backward branch, or calls a
    /// nursery control seam (the natives refuse at the dispatch under the
    /// drive flag) refuses with [`Trap::ReentrancyRefused`]; its
    /// already-run effects stand, the pass ends at that hook, and the
    /// remaining hooks' claims drain unfired — teardown never re-enters
    /// the nursery and never surfaces a user-hook failure, exactly the
    /// death walk's bogus-word honesty.
    pub(crate) fn fire_teardown_hooks(&mut self, fiber: FiberId, held: &[RefId]) {
        self.fire_teardown_hooks_past(fiber, held, None, None);
    }

    /// [`Self::fire_teardown_hooks`] with the REPL rollback's partial
    /// gate: `pre` — the pre-step ledger reading — and `parked` — the
    /// parked frame's words as the step entered — together skip every
    /// claim that was already live before the partial run started (a
    /// band set in the parked words is the parked frame's own binder, a
    /// span word already counted in the reading is a value moved into a
    /// dying frame through a parked binding), so only the claims the run
    /// itself established fire and every surviving value keeps its hook
    /// for its real death.
    pub(crate) fn fire_teardown_hooks_past(
        &mut self,
        fiber: FiberId,
        held: &[RefId],
        pre: Option<&ClaimReading>,
        parked: Option<&[Word]>,
    ) {
        if self.teardown_drive {
            // a door inside a teardown drive never starts a second pass.
            // The nursery seams refuse under the drive flag, so no path
            // reaches a door in-drive; this stays as the safety net for an
            // exotic switch tangle — the outer pass remains the only walker
            return;
        }
        let drives = self.collect_teardown_drives(fiber, held, pre, parked);
        if drives.is_empty() {
            return;
        }
        let takeover = !(self.ctx_live && self.current == fiber);
        let interrupted = takeover.then_some(self.current).filter(|_| self.ctx_live);
        if takeover {
            self.host_release();
            let slot = &mut self.pool[fiber.index()];
            std::mem::swap(&mut self.stack, &mut slot.segment);
            std::mem::swap(&mut self.callinfo, &mut slot.callinfo);
            std::mem::swap(&mut self.objects, &mut slot.objects);
            self.current = fiber;
            self.ctx_live = true;
        }
        self.teardown_drive = true;
        for (row, callee, words) in drives {
            if self
                .call_hosted_in(Value::Fn(callee), &words, Some(row))
                .is_err()
            {
                break;
            }
        }
        self.teardown_drive = false;
        if takeover {
            self.host_release();
        }
        if let Some(resume) = interrupted {
            // the door fired mid-flight of a live fiber (a spawner's
            // recycle door over a dead slot): the drive's caller
            // continues on its own frames, so the parked context swaps
            // back in — otherwise it stays stranded in its slot while
            // the caller runs on the dying fiber's stale scratch buffer
            let slot = &mut self.pool[resume.index()];
            std::mem::swap(&mut self.stack, &mut slot.segment);
            std::mem::swap(&mut self.callinfo, &mut slot.callinfo);
            std::mem::swap(&mut self.objects, &mut slot.objects);
            self.current = resume;
            self.ctx_live = true;
        }
    }

    /// The pass's drive list: for each kept frame (innermost-first) and
    /// each directory site (drop-site order), the hook target plus the
    /// span words read from the frame, gated on the site's establishment
    /// band when it carries one — the frame word the claim's
    /// registration set and every death path cleared, so the doors fire
    /// exactly the claims that live at their slots: Ref-free spans
    /// included, dead claims whose counts survive on outstanding borrows
    /// excluded. A bandless site (a hand-assembled shape) falls back to
    /// the claim ledger: every `Ref` word of the span must still hold
    /// its kind (the frame word is a live reference, not zero init or a
    /// clobbered slot) and a count (the claim's own drop releases its
    /// counts, so a dead claim answers zero), so a span with no `Ref`
    /// word stays unfired there. The held-back cancel payload skips, a
    /// word consumed by an earlier drive never fires twice, and each
    /// fired band clears in its frame word so a later door on the same
    /// kept frames cannot fire the claim again. Each drive names its
    /// owning frame's version row too: the hook declares in that row's
    /// module, so the hosted drive resolves it through the row (the
    /// frame-own rule), never the module's current slot.
    pub(super) fn collect_teardown_drives(
        &mut self,
        fiber: FiberId,
        held: &[RefId],
        pre: Option<&ClaimReading>,
        parked: Option<&[Word]>,
    ) -> Vec<(VersionIdx, FuncRef, Vec<Value>)> {
        let live = self.ctx_live && self.current == fiber;
        // the frames are read by index, innermost first: each frame's
        // version row is Copy, so the walk holds no handle at all
        let depth =
            if live { self.callinfo.len() } else { self.pool[fiber.index()].callinfo.len() };
        let mut drives = Vec::new();
        let mut driven_words: Vec<RefId> = Vec::new();
        let mut driven_bases: Vec<u8> = Vec::new();
        for index in (0..depth).rev() {
            let (fp, version, func) = if live {
                let frame = &self.callinfo[index];
                (frame.fp, frame.module, frame.func)
            } else {
                let frame = &self.pool[fiber.index()].callinfo[index];
                (frame.fp, frame.module, frame.func)
            };
            let Some(module_id) = self
                .registry
                .module_id(self.registry.pinned(version).name())
            else {
                continue;
            };
            let sites = self.hook_sites_for(version, func, module_id);
            if sites.is_empty() {
                continue;
            }
            driven_bases.clear();
            for site in sites.iter().copied() {
                // the partial gate's band witness: a site of the frame
                // the parked words cover whose establishment band was
                // already set before the partial run started belongs to
                // the parked frame's own binder — its claim survives the
                // rollback for the value's real death, so only a band
                // the run itself established fires
                if let (Some(words), Some(band)) = (parked, site.band) {
                    let at = fp + 1 + usize::from(band);
                    if matches!(words.get(at), Some(Word::Slot(Value::Tag(CLAIM_BAND_SET)))) {
                        continue;
                    }
                }
                if driven_bases.contains(&site.base) {
                    continue; // one value lives in one slot per frame
                }
                let Some(drive) = self.gate_teardown_site(
                    fiber,
                    fp,
                    version,
                    site,
                    module_id,
                    held,
                    &driven_words,
                    pre,
                ) else {
                    continue;
                };
                driven_bases.push(site.base);
                driven_words.extend(drive.1.iter().filter_map(|w| match w {
                    Value::Ref(word) => Some(*word),
                    _ => None,
                }));
                if let Some(band) = site.band {
                    // the fired claim's band clears in the frame word
                    // itself: the drive runs the hook, not the body's own
                    // clear op, and a later door over the same kept frames
                    // (the recycle safety net) must never fire it twice
                    let at = fp + 1 + usize::from(band);
                    let cleared = Word::Slot(Value::Tag(CLAIM_BAND_CLEAR));
                    if live {
                        self.stack[at] = cleared;
                    } else {
                        self.pool[fiber.index()].segment[at] = cleared;
                    }
                }
                // the hook addresses this frame's own module, so its row
                // (not the chain's innermost entry) drives the resolution
                drives.push((version, drive.0, drive.1));
            }
        }
        drives
    }

    /// One site's gate: reads the span words out of the frame (the live
    /// stack under the dying fiber's context, its pool segment otherwise)
    /// and checks every span word against the hook's parameter kinds. A
    /// site carrying an establishment band (the marker the body's own
    /// drop clears right after the hook call) reads the band word: set
    /// means the claim lives at this slot — the exact once-per-value
    /// witness, firing Ref-free spans and refusing dead claims whose
    /// counts survive on outstanding borrows. A bandless site (a
    /// hand-assembled shape) falls back to the claim ledger: every `Ref`
    /// word of the span must still hold a count. `pre` — the pre-step
    /// ledger reading of a partial run's rollback — skips the site when
    /// any `Ref` word already carried a count before the run started:
    /// that value survives the rollback through its parked claim.
    /// `None` = skip.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn gate_teardown_site(
        &self,
        fiber: FiberId,
        fp: usize,
        version: VersionIdx,
        site: HookSite,
        module_id: u32,
        held: &[RefId],
        driven_words: &[RefId],
        pre: Option<&ClaimReading>,
    ) -> Option<(FuncRef, Vec<Value>)> {
        let hook = &self.registry.pinned(version).functions()[site.hook as usize];
        let params = hook.layout.params();
        let width = usize::from(site.width);
        if params.len() != width {
            return None; // not this body's drop shape (a stale directory)
        }
        let read = |at: usize| -> Option<Value> {
            let word = if self.ctx_live && self.current == fiber {
                self.stack.get(fp + 1 + at)
            } else {
                self.pool[fiber.index()].segment.get(fp + 1 + at)
            }?;
            match word {
                Word::Slot(v) => Some(*v),
                Word::Meta(_) => None,
            }
        };
        let mut words = Vec::with_capacity(width);
        for at in usize::from(site.base)..usize::from(site.base) + width {
            words.push(read(at)?);
        }
        if !held.is_empty() {
            // the outcome's payload rides to a delivery: its values are
            // alive
            if words
                .iter()
                .any(|w| matches!(w, Value::Ref(word) if held.contains(word)))
            {
                return None;
            }
        }
        // the establishment band is the witness; span kinds still check
        // (a clobbered slot refuses) and a word an earlier drive consumed
        // never fires twice, but the ledger's counts gate nothing
        if site.band.is_some_and(|band| {
            !matches!(read(usize::from(band)), Some(Value::Tag(CLAIM_BAND_SET)))
        }) {
            return None;
        }
        let ledger_witness = site.band.is_none();
        let mut witness = false;
        // one match classifies each span word against the hook's parameter
        // kind: the layout's own Ref arm names its object (an arena word
        // refuses there — it classifies Ref but names no object), a Raw
        // parameter stages any word, every other kind checks once
        for (word, ty) in words.iter().zip(params.iter()) {
            match (word, ty) {
                (Value::Ref(id), SlotType::Ref) => {
                    if driven_words.contains(id)
                        || (ledger_witness && self.claims[fiber.index()].count_of(*id) == 0)
                        || pre.is_some_and(|reading| reading.count_of(*id) > 0)
                    {
                        return None;
                    }
                    witness = true;
                },
                (_, SlotType::Raw) => {},
                (_, SlotType::Ref) => return None,
                (word, ty) if word.slot_type() == *ty => {},
                _ => return None,
            }
        }
        // a band carries its own witness; the ledger path needs a Ref word
        (witness || site.band.is_some()).then(|| (FuncRef::new(module_id, site.hook), words))
    }

    /// The derived deinit-claim directory of one function (crates/ds-vm/
    /// src/claims.rs), cached per (version, function) and swept wholesale
    /// when the registry generation moves — the kstr cache's discipline.
    pub(crate) fn hook_sites_for(
        &mut self,
        version: VersionIdx,
        func: u32,
        own_id: u32,
    ) -> Arc<Vec<HookSite>> {
        let generation = self.registry.generation();
        if self.hook_dirs_stamp != generation {
            self.hook_dirs.clear();
            self.hook_dirs_stamp = generation;
        }
        // the key is the version's pinned address: stable for the row's
        // whole life, and the directory clears at the same install that
        // sweeps the row, so a reused address never meets a stale row
        let pinned = self.registry.pinned(version);
        let key = (std::ptr::from_ref(pinned) as usize, func);
        if let Some(sites) = self.hook_dirs.get(&key) {
            return Arc::clone(sites);
        }
        let sites = hook_sites(pinned, func, own_id);
        self.hook_dirs.insert(key, Arc::clone(&sites));
        sites
    }

    /// A value crossing to the host from the drive-ended fiber: its
    /// claim leaves the ledger. The host never releases, so the count
    /// lives until the stores die — exactly the pre-split behavior.
    pub(crate) fn host_delivery(&mut self, value: &Value) {
        if let Value::Ref(word) = value {
            self.claims[self.current.index()].release(*word);
        }
    }

    /// The payload span a nursery trap carries: delivered to the host
    /// like any drive-ending value, one `Ref` word at a time.
    pub(crate) fn trap_delivery(&mut self, trap: &Trap) {
        match trap {
            Trap::NurseryCancelled { payload } | Trap::NurseryUncontained { payload } => {
                for value in payload {
                    self.host_delivery(value);
                }
            },
            _ => {},
        }
    }
}
