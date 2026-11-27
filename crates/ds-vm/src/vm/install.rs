//! Module install and hot reload: the swap re-checks, the retirement sweeps,
//! and the registry accessors the loader drives.

use super::*;

impl Vm {
    /// Installs a module version into its two-slot table (hot reload swap):
    /// the swap re-checks run first (a reordering table re-keys the staged
    /// words whose identity survived while refusing a dropped one, and a
    /// live code row's direct-call site naming a moved or dropped index
    /// refuses the swap), then the swap moves the retiring slot's old row
    /// out of the registry's own reachability, and the install-time sweep
    /// retires every row no live frame or parked fiber names — the retired
    /// pools' KSTR rows reclaimed at the same moment. Answers the installed
    /// version's table row, the version's single home.
    ///
    /// # Errors
    ///
    /// [`crate::LoadError::StagedFnMisroute`] or
    /// [`crate::LoadError::PoolRefMisroute`] when a re-check refuses (the
    /// installed version keeps running).
    pub fn install_module(&mut self, version: ModuleVersion) -> Result<VersionIdx, LoadError> {
        let name = version.name().to_owned();
        let swap = self.validate_staged_swap(&name, &version)?;
        let mut dead_pools = self.retiring_pool_addrs(&name);
        let row = self.registry.install(version);
        self.finish_swap(&swap);
        self.sweep_call_cache();
        dead_pools.extend(self.sweep_versions());
        self.reclaim_retired_kstr_rows(&dead_pools);
        Ok(row)
    }

    /// Installs a dsmod module into its two-slot table: the swap re-checks
    /// run first (below), the loader maps the payload, fills the
    /// entry-descriptor placeholders, and the slots swap.
    /// The module is consumed, so every body's code words reach the
    /// installed table without a copy. Answers the installed version's
    /// table row, the version's single home.
    ///
    /// # Errors
    ///
    /// Returns [`crate::LoadError`] for unqualified or unresolved callee
    /// strings, for bodies failing the load checks, and for the swap
    /// re-checks' refusals (the installed version keeps running).
    pub fn install_dsmod(&mut self, module: ds_mod::Module) -> Result<VersionIdx, LoadError> {
        let name = module.name().to_owned();
        // build first, so the re-checks read the incoming table in its
        // installed spelling (a failed build touches no slot, exactly like
        // a refused swap)
        let version = crate::loader::build(&self.registry, module)?;
        let swap = self.validate_staged_swap(&name, &version)?;
        let mut dead_pools = self.retiring_pool_addrs(&name);
        let row = self.registry.install(version);
        self.finish_swap(&swap);
        self.sweep_call_cache();
        dead_pools.extend(self.sweep_versions());
        self.reclaim_retired_kstr_rows(&dead_pools);
        Ok(row)
    }

    /// Loads a dsmod module with its whole dependency closure: the
    /// deps-first walk installs every uop dependency the root's dep list
    /// names (fetching each uninstalled one through `provider`) before
    /// the root itself, so first-install ids assign in the deterministic
    /// dep-list depth-first order, and every link's content hash is
    /// validated — a mismatched link, a dep cycle, or a provider miss
    /// refuses the load. Resolution keeps reading each name's current
    /// slot, so a later swap of a loaded dep redirects the dependent's
    /// calls. The swap re-checks cover the root's swap (the
    /// walk's own door: already-installed deps validate and skip, so only
    /// the root can move a slot). Answers the root's table row, the
    /// version's single home.
    ///
    /// # Errors
    ///
    /// Returns [`crate::LoadError`] for every refusal the walk names and
    /// for the swap re-checks' refusals; dependencies that finished
    /// installing stay installed.
    pub fn load(
        &mut self,
        root: ds_mod::Module,
        provider: impl FnMut(&str, ds_mod::Hash128) -> Option<ds_mod::Module>,
    ) -> Result<VersionIdx, LoadError> {
        let name = root.name().to_owned();
        // the walk installs the dependencies and answers the root built;
        // only the root can swap a slot (already-installed deps validate
        // and skip), so the re-checks read the built table before the
        // install lands and the pre-walk retiring pools are its own
        let version = crate::load::load_built(&mut self.registry, root, provider)?;
        let swap = self.validate_staged_swap(&name, &version)?;
        let mut dead_pools = self.retiring_pool_addrs(&name);
        let row = self.registry.install(version);
        self.finish_swap(&swap);
        self.sweep_call_cache();
        dead_pools.extend(self.sweep_versions());
        self.reclaim_retired_kstr_rows(&dead_pools);
        Ok(row)
    }

    /// The content identity the name's current installed version records
    /// (`None`: no current version under the name): the host-side seam a
    /// dev session reads to decide whether a reload round moves a dep
    /// before the walk validates its link — the identity stays pinned at
    /// link time for the walk itself.
    #[must_use]
    pub fn installed_content_hash(&self, name: &str) -> Option<ds_mod::Hash128> {
        self.registry
            .current_version(name)
            .and_then(ModuleVersion::content_hash)
    }

    /// The slot swap's staged-word re-check:
    /// a scalar callable word keys on the function-table index and
    /// resolution reads the name's current slot, so a swap whose incoming
    /// table moves or drops the indexed function would silently misroute
    /// every staged word into whatever function now holds the index. The
    /// identity judge is what the table already records — the function's
    /// name plus its frame layout, extended by the content identity a
    /// moved index-embedded name judges by (body plus capture set hashing
    /// equal against exactly one incoming candidate, never a guess).
    /// Phase one (here, read-only, before any install): build the
    /// retiring table's re-key map against the incoming rows and refuse on
    /// the first staged word whose identity did not survive, then the
    /// direct-call half ([`Vm::validate_pool_call_refs`]) refuses on the
    /// first live code row's call site whose named identity the incoming
    /// table moves or drops. Phase two ([`Vm::finish_swap`], after the
    /// install landed): re-key every staged word through the validated
    /// map, transparent where the identity kept, moved, or content-
    /// survived its index. A first install under a fresh name needs no
    /// check — no staged word can name an id the registry has not
    /// assigned.
    ///
    /// # Errors
    ///
    /// [`LoadError::StagedFnMisroute`] naming the staged value and the
    /// dropped function, or [`LoadError::PoolRefMisroute`] naming the
    /// refused call site; the registry is untouched.
    pub(super) fn validate_staged_swap(
        &mut self,
        name: &str,
        incoming: &ModuleVersion,
    ) -> Result<StagedSwap, LoadError> {
        let Some(old) = self.registry.current_version(name) else {
            return Ok(StagedSwap::default());
        };
        let old_names: Vec<String> = old.functions().iter().map(|f| f.name.clone()).collect();
        let map: Vec<Option<u32>> = (0..u32::try_from(old.functions().len())
            .expect("function tables fit u32"))
            .map(|at| old.surviving_index(at, incoming))
            .collect();
        let module_id = self
            .registry
            .module_id(name)
            .expect("a module with a current version has its id");
        let module = name.to_owned();
        self.walk_staged_fns(&mut |site, func| {
            if func.module() != module_id {
                return Ok(());
            }
            let at = usize::try_from(func.func()).ok();
            let survived = at.and_then(|at| map.get(at).copied().flatten()).is_some();
            if survived {
                return Ok(());
            }
            Err(LoadError::StagedFnMisroute {
                module: module.clone(),
                index: func.func(),
                func: at.and_then(|at| old_names.get(at).cloned()),
                site,
            })
        })?;
        self.validate_pool_call_refs(name, incoming)?;
        Ok(StagedSwap { module_id, map })
    }

    /// The re-check's direct-call half: a live code row's CALL/CALLT/
    /// CALLG/CALLTG site names the swapped module through its constant
    /// pool by function-table index, the site's code is immutable, and
    /// resolution follows the module's current slot — so a swap that
    /// re-keys the index onto a different function NAME would silently
    /// route every live frame's next direct call into whatever function
    /// now holds the index. The staged-word half covers the callable
    /// VALUES (re-key or refuse at the swap); this half covers the
    /// executable CODE. The site's identity is the name alone: a kept
    /// name with a changed frame layout is the ordinary body edit (the
    /// inline cache re-derives the site's layout facts at its stamp
    /// miss). A moved name refuses — with one carve-out: the swapped
    /// module's OWN retiring rows carry sites whose index reads that
    /// row's index space, and the dispatch translates those through the
    /// identity the row records, so a moved index-embedded name (a
    /// synthesized lambda, renamed by the reorder itself) passes exactly
    /// when its content identity survives the move — body plus capture
    /// set hashing equal against exactly one incoming candidate. A
    /// dependent module's sites keep the strict name judge: their rows
    /// record no linked version of the swapped module, so the
    /// cross-module redirect stays the verbatim current-slot rule. The
    /// walk reads every version-table row a live frame or parked fiber
    /// pins (the registry's own slots excluded — a row no runnable
    /// frame names never executes again, so its pool refs take the
    /// ordinary redirect the inline-cache tests pin), so a dependent
    /// module's direct calls into the swapped module are covered exactly
    /// like the swapped module's own.
    ///
    /// # Errors
    ///
    /// [`LoadError::PoolRefMisroute`] naming the pool that carries the
    /// site, the index, and the identity the installed table records;
    /// the registry is untouched.
    pub(super) fn validate_pool_call_refs(
        &self,
        name: &str,
        incoming: &ModuleVersion,
    ) -> Result<(), LoadError> {
        let current = self
            .registry
            .current_version(name)
            .expect("the staged phase proved a current version exists");
        let module_id = self
            .registry
            .module_id(name)
            .expect("a module with a current version has its id");
        let identity = |index: u32| -> Option<&str> {
            usize::try_from(index)
                .ok()
                .and_then(|at| current.functions().get(at))
                .map(|f| f.name.as_str())
        };
        // the rows live frames name — the sweep's marking walk minus the
        // registry's own slots: a row no frame or parked fiber names will
        // never execute again (a fresh run resolves through the swapped-in
        // current table), so its pool refs take the ordinary redirect, the
        // completed-frame semantics the inline-cache tests pin
        let mut live = vec![false; self.registry.version_rows()];
        for frame in &self.callinfo {
            live[frame.module.0 as usize] = true;
        }
        for fiber in &self.pool {
            // a dead fiber's kept frames never execute again (the recycle
            // door walks them for deinit claims alone, resolving hooks
            // through the row the sweep pins), so they take no part in
            // the valve: only a fiber that can run its code names a row
            if fiber.state == FiberState::Dead {
                continue;
            }
            for frame in &fiber.callinfo {
                live[frame.module.0 as usize] = true;
            }
            if let Some(resume) = &fiber.resume {
                live[resume.version.0 as usize] = true;
            }
        }
        for (row, &named) in live.iter().enumerate() {
            if !named {
                continue;
            }
            let Some(version) = self.registry.version(VersionIdx(
                u32::try_from(row).expect("version rows fit u32"),
            )) else {
                continue;
            };
            for function in version.functions() {
                for (pc, &word) in function.code.iter().enumerate() {
                    let pc = u32::try_from(pc).expect("pc fits u32");
                    let Ok(ds_abi::Inst::Uop(op)) = ds_abi::Inst::decode(pc, word) else {
                        continue;
                    };
                    let (ds_abi::uop::Uop::Call { callee, .. }
                    | ds_abi::uop::Uop::Callt { callee, .. }
                    | ds_abi::uop::Uop::Callg { callee, .. }
                    | ds_abi::uop::Uop::Calltg { callee, .. }) = op
                    else {
                        continue;
                    };
                    let Some(Const::FuncRef(entry)) = function.pool.get(usize::from(callee.0))
                    else {
                        continue;
                    };
                    let index = match entry {
                        ConstFuncRef::Uop { module, func } if *module == module_id => *func,
                        ConstFuncRef::Named { module, func } if module == name => *func,
                        _ => continue,
                    };
                    let Some(func_name) = identity(index) else {
                        return Err(LoadError::PoolRefMisroute {
                            module: name.to_owned(),
                            pool: version.name().to_owned(),
                            index,
                            func: None,
                        });
                    };
                    // a call site's identity is the NAME the index holds —
                    // the index re-keying a different name is the misroute.
                    // A kept name with a changed frame layout is the
                    // ordinary body edit: the inline cache re-derives the
                    // site's layout facts at its stamp miss, and an edited
                    // signature traps at the call exactly like a fresh
                    // run's would, so layout never gates the swap here
                    // (unlike a staged callable WORD, whose re-check judges
                    // name plus layout). The one moved name that passes is
                    // the swapped module's own retiring row's index-
                    // embedded callee whose content identity survived —
                    // the dispatch translates the site through the row's
                    // recorded identity, so the install keeps its
                    // byte-identical redirect (never a guess: zero or
                    // several content matches refuse)
                    let kept = usize::try_from(index)
                        .ok()
                        .and_then(|at| incoming.functions().get(at))
                        .is_some_and(|f| f.name == func_name);
                    let translated = !kept
                        && version.name() == name
                        && usize::try_from(index)
                            .ok()
                            .and_then(|at| current.functions().get(at))
                            .filter(|f| f.embeds_table_index())
                            .and_then(|f| incoming.content_unique(f.content_hash()))
                            .is_some();
                    if !kept && !translated {
                        return Err(LoadError::PoolRefMisroute {
                            module: name.to_owned(),
                            pool: version.name().to_owned(),
                            index,
                            func: Some(func_name.to_owned()),
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// The re-check's phase two: rewrites every staged word keyed to the
    /// swapped module's id through the validated map. Runs only after the
    /// install landed, so a refused or failed install never leaves a
    /// re-keyed word behind (a re-keyed index is wrong against the table
    /// still installed).
    pub(super) fn finish_swap(&mut self, swap: &StagedSwap) {
        let mut rekey =
            |_: StagedSite, func: &mut FuncRef| -> Result<(), std::convert::Infallible> {
                if let Some(to) = usize::try_from(func.func())
                    .ok()
                    .and_then(|at| swap.map.get(at).copied().flatten())
                {
                    *func = FuncRef::new(swap.module_id, to);
                }
                Ok(())
            };
        let () = self
            .walk_staged_fns(&mut rekey)
            .unwrap_or_else(|never| match never {});
    }

    /// Walks every staged callable word the VM owns, applying `visit`,
    /// which may rewrite the word in place; the first `Err` short-circuits.
    /// The sites: every fiber's heap objects (closure headers and captures,
    /// list elements, map keys and values) and every fiber's live segment
    /// words — the live drive context's store and segment walk through the
    /// context fields while its pool slot (scratch while the context is
    /// live) is skipped, every other fiber's through its slot. Dead
    /// fibers' stores and segments walk too: their objects survive
    /// foreign-held words and their frames fire deinit claims at slot
    /// reuse, the sweep's own reachability discipline.
    pub(super) fn walk_staged_fns<E>(
        &mut self,
        visit: &mut impl FnMut(StagedSite, &mut FuncRef) -> Result<(), E>,
    ) -> Result<(), E> {
        if self.ctx_live {
            self.objects.for_each_staged_fn(visit)?;
            let fiber = self.current.0;
            for at in 0..self.stack.len() {
                if let Word::Slot(Value::Fn(func)) = &mut self.stack[at] {
                    visit(StagedSite::FrameSlot { fiber }, func)?;
                }
            }
        }
        for (index, slot) in self.pool.iter_mut().enumerate() {
            if self.ctx_live && index == self.current.index() {
                continue;
            }
            slot.objects.for_each_staged_fn(visit)?;
            let fiber = u32::try_from(index).expect("fiber ids fit u32");
            for at in 0..slot.segment.len() {
                if let Word::Slot(Value::Fn(func)) = &mut slot.segment[at] {
                    visit(StagedSite::FrameSlot { fiber }, func)?;
                }
            }
        }
        Ok(())
    }

    // -- Bounded-memory reclamation over the two hint caches -------------
    //
    // Both caches sit beside an authoritative path (the full call-site
    // lookup, the full intern), so every reclamation below drops hints
    // only: a dropped row or entry costs its next use one full path and
    // nothing else. No count and no object moves at any door.

    /// The pool addresses of `module`'s retiring version's functions: the
    /// version the NEXT install under this name drops out of the registry
    /// (in-flight frames may hold it longer; its pools' rows are hints, so
    /// reclaiming them early only costs those frames a re-intern).
    pub(super) fn retiring_pool_addrs(&self, module: &str) -> Vec<usize> {
        self.registry
            .retiring_version(module)
            .map(|version| {
                version
                    .functions()
                    .iter()
                    .map(|f| f.pool.as_ptr() as usize)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Drops the KSTR intern rows keyed by pools a swap retired. The
    /// reclaim is unconditional even when kept frames still hold the
    /// retiring version — the version leaving the two-slot table never
    /// serves another host entry, but those frames keep loading through
    /// its pools, and each dropped row costs their next load one full
    /// intern (a reused address re-keys its row at the new pool's first
    /// load). Rows of the version moving into the retiring slot stay —
    /// its in-flight frames still load through them.
    pub(super) fn reclaim_retired_kstr_rows(&mut self, pools: &[usize]) {
        self.kstr_cache.reclaim(|row| !pools.contains(&row.pool));
    }

    /// Drops the KSTR intern rows a dead pool slot leaves behind at its
    /// reuse door: the slot's store persists (a foreign-held object
    /// outlives its dead owner), so the rows stay valid hints for the
    /// next spawn — but the store's own intern map already holds the
    /// content, so a dropped row costs its next load one intern hit, and
    /// the VM-global table tracks live loads, not every fiber that ever
    /// loaded.
    pub(super) fn reclaim_fiber_kstr_rows(&mut self, fiber: FiberId) {
        self.kstr_cache.reclaim(|row| row.fiber != fiber.0);
    }

    /// Empties the CALL inline-cache entries a swap unstamped: a stale
    /// entry can never hit again (the stamp check fails), so by the time
    /// the install's sweep counts reachability no cache entry is left to
    /// name a row. The full lookup stays the fallback and the refill, so
    /// dispatch correctness never depends on the cache.
    pub(super) fn sweep_call_cache(&mut self) {
        let stamp = self.registry.generation();
        for entry in &mut self.call_cache {
            if entry.as_ref().is_some_and(|e| e.stamp != stamp) {
                *entry = None;
            }
        }
    }

    /// The install-time reachability sweep over the version table: a row no
    /// module slot names sits on the registry's detached
    /// list, and the sweep retires every detached row no fiber names —
    /// the context's live call chain while a drive runs, every pool slot's
    /// kept frames otherwise (interrupted fibers keep their frames, and a
    /// Dead fiber's included: the recycle door walks a dead slot's
    /// leftover frames firing whatever banded deinit claims survived the
    /// death, so their rows must outlive the slot's reuse) — or any parked
    /// or ready fiber's resume point. The run-entry hint and the CALL cache
    /// never reach a sweep as namers: the install takes the hint and the
    /// stamp check empties every cache entry before the sweep runs, and no
    /// dispatch happens between them. Every retired row's memory frees
    /// right there (the row is the version's single home) and its index
    /// joins the reuse freelist, so the table's width stays bounded under
    /// install churn; the sweep answers the retired versions' pool
    /// addresses so their KSTR rows die with them. The judge list is the
    /// detached rows alone, so the sweep costs O(detached), never the
    /// table's width.
    pub(super) fn sweep_versions(&mut self) -> Vec<usize> {
        let named = |row: VersionIdx| {
            self.callinfo.iter().any(|frame| frame.module == row)
                || self.chan_fires.iter().any(|fire| fire.hook.version == row)
                || self.pool.iter().any(|fiber| {
                    fiber.callinfo.iter().any(|frame| frame.module == row)
                        || fiber
                            .resume
                            .as_ref()
                            .is_some_and(|resume| resume.version == row)
                        || fiber.objects.names_version(row)
                })
        };
        self.registry.retire_detached_rows(named)
    }

    /// How many (pool, fiber) intern-row keys the KSTR cache holds — the
    /// bounded-memory reclamation witness for test assertions.
    #[must_use]
    pub fn kstr_cache_rows(&self) -> usize {
        self.kstr_cache.len()
    }

    /// The running fiber's arena snapshot - the bounded-memory reclamation
    /// witness for the loop-back-edge restore in test assertions.
    #[must_use]
    pub fn arena_stats(&self) -> ds_rt::ArenaStats {
        self.pool[self.current.index()].arena.stats()
    }

    /// The registry, for slot inspection.
    #[must_use]
    pub fn registry(&self) -> &ModuleRegistry {
        &self.registry
    }

    /// Replaces one installed native module's table: the comptime
    /// fence's builtin row (a world-touching builtin dropped so the
    /// evaluation slice's own load refuses the call, the same compile
    /// error an unserved platform op takes). A same-named prior install
    /// is replaced, the registry's own discipline; the generation bump
    /// re-keys the CALL-site caches.
    pub fn install_native_module(&mut self, name: &str, functions: Vec<NativeFunction>) {
        self.registry.install_native(name, functions);
    }

    /// Installs a bound-artifact module under its manifest name:
    /// the same replacement discipline a native table takes, so a host
    /// or a dev session installs ahead of the module installs whose
    /// pool validation resolves bound constants against the table.
    pub fn install_bound(&mut self, name: &str, module: crate::bound::BoundModule) {
        self.registry.install_bound(name, module);
    }
}
