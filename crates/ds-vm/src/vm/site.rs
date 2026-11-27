//! The call-target caches and the resolvers that own them: the CALL-site and
//! word-keyed inline caches, the KSTR intern rows, and the pool-FuncRef site
//! index the install redirects with.

use super::*;

/// An owned CALL-site resolution handed to a dispatch arm: the uop shape
/// carries the callee's version-table row plus the site's cached layout
/// facts, the native shape the shared table handle plus index — borrows
/// of the arm's own locals, so no loan on the VM outlives the arm's
/// whole-self calls (the control-native switch).
#[derive(Debug)]
pub(super) enum Resolved {
    /// Enter the callee's encoded body.
    Uop {
        /// The callee's module version's table row.
        version: VersionIdx,
        /// The callee's function-table index.
        func: u32,
        /// The site's cached layout facts (the plan to transfer under).
        facts: SiteFacts,
    },
    /// Invoke the native in the caller's frame (no frame push).
    Native {
        /// The native module's shared function table.
        table: Arc<Vec<NativeFunction>>,
        /// The callee's index in the table.
        index: u32,
    },
    /// Invoke the bound artifact's C symbol in the caller's frame, with no
    /// frame push: the loaded module's shared symbol table, the
    /// export's enumeration index, and the return's marshaling word —
    /// the absent word or a scalar word.
    Bound {
        /// The bound module's shared symbol table.
        module: Arc<crate::bound::BoundModule>,
        /// The symbol's index in the export enumeration.
        index: u32,
        /// The return's marshaling word: the absent word or a scalar
        /// word.
        ret: BoundRet,
    },
}

/// The ownable form of a call-site resolution the inline cache stores: the
/// uop shape holds the resolved version's table row plus the site's layout
/// facts, the native shape the shared table plus index (natives are not
/// values — the table is the stable handle, the index the callee).
#[derive(Clone, Debug)]
pub(super) enum CachedCallee {
    Uop {
        version: VersionIdx,
        func: u32,
        facts: SiteFacts,
    },
    Native {
        table: Arc<Vec<NativeFunction>>,
        index: u32,
    },
    /// The bound-artifact shape: the loaded module's shared symbol
    /// table, the export's index, and the return's marshaling word (a
    /// bound symbol is not a value — the table is the stable handle, the
    /// index the callee).
    Bound {
        module: Arc<crate::bound::BoundModule>,
        index: u32,
        ret: BoundRet,
    },
}

/// The resolution of a dispatch site's (CALLG/CALLTG) pool FuncRef against
/// the uop module tables: the impl body a trait site's FuncRef names, or
/// the not-a-uop-callee signal that hands the site to the generic builtin
/// tables (the sentinel evidence FuncRef never reaches the resolver).
#[derive(Debug)]
pub(super) enum DispatchResolved {
    /// Enter (or tail-transfer into) the named uop function's body.
    Uop {
        version: VersionIdx,
        func: u32,
        /// The site's cached layout facts (the plan to transfer under).
        facts: SiteFacts,
    },
    /// The FuncRef names no uop function: the generic builtin tables
    /// answer next.
    NotUop,
}

/// One CALL-site inline-cache slot: the resolution of (caller function,
/// pc) as of `stamp`. A hit requires all three to agree — the caller
/// identity is the executing function's address (stable for the version's
/// lifetime; a freed-and-reused address requires an install, which bumps
/// the stamp), the pc discriminates call sites within one body, and the
/// stamp is the registry generation the resolution ran against, so any
/// install (a two-slot swap, a native-table replacement) invalidates every
/// entry at once. A miss falls back to the full lookup, so dispatch
/// correctness never depends on the cache. A uop entry also carries the
/// site's layout facts (the slide plan and its `Raw`-staged reads), a pure
/// function of the caller/callee layout pair the key already pins — the
/// caller address names the caller's layout, the cached callee the
/// callee's — so the plan is re-derived exactly when the callee is, never
/// on a hit.
#[derive(Clone, Debug)]
pub(super) struct CallSite {
    pub(super) caller: usize,
    pub(super) pc: u32,
    pub(super) stamp: u64,
    pub(super) callee: CachedCallee,
}

/// CALL-site cache slot count (direct-mapped, one uop per slot).
pub(super) const CALL_CACHE_SLOTS: usize = 512;

/// One word-keyed inline-cache slot: the resolution of a callable word
/// (the CALLS arm and the hosted-call seam) as of `stamp`. The key is
/// the whole input triple the full resolution takes — the scalar
/// FuncRef or closure header's bits, the caller's owner row, and the
/// staged argument count — so a hit answers exactly what the full path
/// answers, the arity check included (`nargs` rides the key). The stamp
/// is the registry generation the resolution ran against, so any
/// install invalidates every entry at once; a miss falls back to the
/// full lookup, so dispatch correctness never depends on the cache.
/// Traps are never cached: every failure comes from the full path.
#[derive(Clone, Debug)]
pub(super) struct WordCallee {
    pub(super) funcref: FuncRef,
    pub(super) owner: Option<VersionIdx>,
    pub(super) nargs: u8,
    pub(super) stamp: u64,
    pub(super) version: VersionIdx,
    pub(super) func: u32,
}

/// Word-keyed cache slot count (direct-mapped; the monomorphic call
/// sites the bank serves touch a handful of distinct keys per run).
pub(super) const WORD_CACHE_SLOTS: usize = 64;

/// The hosted-call seam's persistent reentry memo: the resolution of
/// the last scalar-callee hosted call, one entry beside the bank the
/// seam shares with CALLS. The key is the same input triple the word
/// cache keys on, and the fill happens only for a scalar callee word
/// (a closure stages captures, so its calls stay on the full path),
/// validated against the registry generation at every use — a hit
/// answers exactly what the full path would, any install invalidates
/// it, and traps never fill it. Consecutive reentries of one callee —
/// the container combinators' per-element loop — read it instead of
/// re-hashing the bank; the reentry frame itself needs no reuse state,
/// since RET truncates the segment's len and never its storage, so the
/// frame's chunks stay allocated between the calls of one loop and the
/// rebuild writes straight into them.
#[derive(Clone, Debug)]
pub(super) struct HostedReentry {
    pub(super) funcref: FuncRef,
    pub(super) owner: Option<VersionIdx>,
    pub(super) nargs: u8,
    pub(super) stamp: u64,
    pub(super) version: VersionIdx,
    pub(super) func: u32,
}

/// A KSTR intern-row key: the pool's address (stable for its version's
/// lifetime) and the loading fiber — each heap interns its own copy of a
/// constant, so a cached id is only ever served against the fiber whose
/// store minted it.
pub(super) type KstrRowKey = (usize, u32);

/// The VM's own table key hasher: the multiply-rotate round the claim
/// ledger and the intern map use, one per key word, so a per-frame table
/// probe never pays the default SipHash. The seed is fixed, never
/// per-process random, so a fuzz run replays exactly.
#[derive(Default)]
pub(super) struct WordKeyHasher(u64);

impl WordKeyHasher {
    pub(super) fn mix(&mut self, word: u64) {
        self.0 = self.0.rotate_left(13) ^ word;
        self.0 = self.0.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}

impl Hasher for WordKeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.mix(u64::from(byte));
        }
    }

    fn write_u64(&mut self, n: u64) {
        self.mix(n);
    }

    fn write_usize(&mut self, n: usize) {
        self.mix(n as u64);
    }

    fn write_u32(&mut self, n: u32) {
        self.mix(u64::from(n));
    }
}

pub(super) type KstrRowIndex = HashMap<KstrRowKey, u32, BuildHasherDefault<WordKeyHasher>>;

/// A derived deinit-hook directory's index: (module version address,
/// function) keys, hashed by the VM's own round.
pub(super) type HookDirIndex =
    HashMap<(usize, u32), Arc<Vec<HookSite>>, BuildHasherDefault<WordKeyHasher>>;

/// A frame's resolved KSTR intern row: its index in the row list. Reclaim
/// drops and shifts rows, so the slot is a hint revalidated against the
/// row's own key on every load — a mismatch costs the frame one key
/// lookup, never a wrong serve.
#[derive(Clone, Copy, Debug)]
pub(super) struct KstrRowSlot(pub(super) u32);

/// One (pool, fiber) intern row: the object ids the pool's string
/// constants interned on that fiber's heap, by const index.
#[derive(Debug)]
pub(super) struct KstrRow {
    pub(super) pool: usize,
    pub(super) fiber: u32,
    pub(super) ids: Vec<Option<u32>>,
}

impl KstrRow {
    pub(super) fn key(&self) -> KstrRowKey {
        (self.pool, self.fiber)
    }
}

/// The KSTR intern rows: a dense row list plus the key index into it. A
/// frame resolves its row once and its later loads in that frame read the
/// slot directly instead of probing the key index per load, which is the
/// literal-load cost that matters.
#[derive(Debug, Default)]
pub(super) struct KstrRows {
    pub(super) rows: Vec<KstrRow>,
    pub(super) index: KstrRowIndex,
}

impl KstrRows {
    /// The row `slot` names, when it is still `key`'s row.
    pub(super) fn row(&self, slot: KstrRowSlot, key: KstrRowKey) -> Option<&KstrRow> {
        self.rows
            .get(slot.0 as usize)
            .filter(|row| row.key() == key)
    }

    /// The key's row, opened when it has none. A reused pool address can
    /// meet a row of a dead pool's width, so the ids widen before they are
    /// indexed.
    pub(super) fn open(&mut self, key: KstrRowKey, width: usize) -> (KstrRowSlot, &mut KstrRow) {
        let slot = if let Some(&slot) = self.index.get(&key) {
            slot
        } else {
            let slot = u32::try_from(self.rows.len()).expect("row slots fit u32");
            self.rows.push(KstrRow {
                pool: key.0,
                fiber: key.1,
                ids: Vec::new(),
            });
            self.index.insert(key, slot);
            slot
        };
        let row = &mut self.rows[slot as usize];
        if row.ids.len() < width {
            row.ids.resize(width, None);
        }
        (KstrRowSlot(slot), row)
    }

    /// Drops the rows `keep` refuses and rebuilds the index over the
    /// survivors: the row list stays dense, so nothing outlives its
    /// reclaim as a tombstone, and a surviving row keeps its ids.
    pub(super) fn reclaim(&mut self, keep: impl Fn(&KstrRow) -> bool) {
        if self.rows.is_empty() {
            return;
        }
        self.rows.retain(keep);
        self.index.clear();
        for (slot, row) in self.rows.iter().enumerate() {
            let slot = u32::try_from(slot).expect("row slots fit u32");
            self.index.insert(row.key(), slot);
        }
    }

    /// How many (pool, fiber) rows the table holds — the bounded-memory
    /// reclamation witness for test assertions.
    pub(super) fn len(&self) -> usize {
        self.rows.len()
    }
}

impl Vm {
    /// The current-table index a pool FuncRef's uop target resolves to for
    /// a caller running `caller_row`: the verbatim index unless the caller
    /// runs one of the target module's retiring rows and names its own
    /// module — then the site's index reads the caller row's own index
    /// space, so a kept name keeps the index (the ordinary redirect the
    /// re-check's name judge pins) and a moved index-embedded name (a
    /// synthesized lambda, renamed by the reorder itself) translates
    /// through the content judge: body plus capture set hashing equal
    /// against exactly one current candidate. `None` names an identity
    /// that did not survive the swap — the call refuses rather than
    /// routing the verbatim index into whatever function now holds it.
    /// Sites crossing modules keep the verbatim rule outright: the
    /// caller's row records no linked version of the target, so the
    /// cross-module redirect stays the current-slot read.
    pub(super) fn uop_site_index(
        registry: &ModuleRegistry,
        caller_row: VersionIdx,
        module: &str,
        index: u32,
    ) -> Option<u32> {
        let row = registry.version(caller_row)?;
        if row.name() != module {
            return Some(index);
        }
        let current = registry.slots(module)?.current_idx();
        if current == caller_row {
            return Some(index);
        }
        let at = usize::try_from(index).ok()?;
        let old = row.functions().get(at)?;
        let current = registry.version(current)?;
        // the kept-name fast path: the current table's function at the
        // same index carries the site's name — verbatim
        if current
            .functions()
            .get(at)
            .is_some_and(|f| f.name == old.name)
        {
            return Some(index);
        }
        // the moved index-embedded name: the content judge, exactly one
        // match — zero or several answer None (the never-guessing refusal)
        if !old.embeds_table_index() {
            return None;
        }
        current.content_unique(old.content_hash())
    }

    /// Resolves a pool FuncRef for the CALL/CALLT dispatch: the per-site
    /// inline cache first — the entry keyed on the caller function's
    /// identity and pc, validated against the registry's install
    /// generation — then the full lookup as the fallback and the refill.
    /// The full path is the same resolution discipline as ever: uop
    /// modules first (through the target's current slot, a retiring
    /// caller's own-module index translating through the identity its row
    /// records), then the native tables (a uop module installed under a
    /// native table's name shadows the natives); a stamp mismatch after a
    /// swap lands here, so callee resolution always follows the current
    /// slot. Traps are
    /// never cached — every failure comes from the
    /// full path (a refused layout pair rides the entry as its cached
    /// refusal, re-raised at the call seam, never answered as a resolution
    /// failure). A uop fill also derives the site's layout facts against
    /// `caller_layout`, so the plan is re-derived exactly when the callee
    /// is.
    #[inline]
    pub(super) fn resolve_site(
        &mut self,
        pool: &[Const],
        caller: CallerAt,
        caller_layout: &FrameLayout,
        pc: u32,
        kidx: ConstIdx,
        nargs: u8,
    ) -> Result<Resolved, Trap> {
        let at = pc as usize & (CALL_CACHE_SLOTS - 1);
        let stamp = self.registry.generation();
        let hit = self.call_cache[at]
            .as_ref()
            .is_some_and(|e| e.caller == caller.func && e.pc == pc && e.stamp == stamp);
        if !hit {
            let entry =
                self.resolve_site_full(pool, caller, caller_layout, pc, kidx, nargs, stamp)?;
            self.call_cache[at] = Some(entry);
        }
        Ok(
            match &self.call_cache[at]
                .as_ref()
                .expect("the slot was just filled")
                .callee
            {
                CachedCallee::Uop {
                    version,
                    func,
                    facts,
                } => Resolved::Uop {
                    version: *version,
                    func: *func,
                    facts: facts.clone(),
                },
                CachedCallee::Native { table, index } => Resolved::Native {
                    table: Arc::clone(table),
                    index: *index,
                },
                CachedCallee::Bound { module, index, ret } => Resolved::Bound {
                    module: Arc::clone(module),
                    index: *index,
                    ret: *ret,
                },
            },
        )
    }

    /// The full, uncached call-site resolution the cache falls back to:
    /// pool kind check, the target module's current slot, the entry-word
    /// and arity checks. The pool belongs to the CALLER's module version:
    /// instruction stream and pool are version-stable, callee resolution
    /// follows the target's current slot — the redirect a
    /// parked old frame's next direct call picks up, with a retiring
    /// caller's own-module index translated through the identity its row
    /// records ([`Vm::uop_site_index`]). The pool operand's
    /// index and FuncRef kind were proved at install (module.rs's body
    /// validation), so the fetch indexes directly and the kind arm is the
    /// corrupted-pool tripwire.
    pub(super) fn resolve_site_full(
        &self,
        pool: &[Const],
        caller: CallerAt,
        caller_layout: &FrameLayout,
        pc: u32,
        kidx: ConstIdx,
        nargs: u8,
        stamp: u64,
    ) -> Result<CallSite, Trap> {
        let found = &pool[usize::from(kidx.0)];
        let Const::FuncRef(entry) = found else {
            return Err(Trap::PoolKind {
                kidx: kidx.0,
                expected: ConstKind::FuncRef,
                found: found.kind(),
            });
        };
        // a bound callee resolves through the registry's bound tier alone
        // — never a uop module, never the native tables — the
        // artifact's symbol table under its manifest name
        if let ConstFuncRef::Bound { module, index, ret } = entry {
            let bound = self
                .registry
                .bound_table(module)
                .filter(|table| usize::try_from(*index).is_ok_and(|at| at < table.len()))
                .ok_or(Trap::UnknownCallee {
                    module: module.clone(),
                    func: *index,
                })?;
            return Ok(CallSite {
                caller: caller.func,
                pc,
                stamp,
                callee: CachedCallee::Bound {
                    module: Arc::clone(bound),
                    index: *index,
                    ret: *ret,
                },
            });
        }
        // uop first: an install-resolved id addresses the dep's module
        // directly (its name sits in the registry's id vector), a name
        // through the module table — so a uop module installed under a
        // native table's name still shadows the natives. A retiring
        // caller's own-module index translates first; a translation that
        // cannot be proven traps rather than routing the verbatim index.
        let uop_target =
            match entry {
                ConstFuncRef::Uop { module, func } => {
                    let name = self
                        .registry
                        .module_name(*module)
                        .ok_or(Trap::UnknownModule { module: *module })?;
                    let func = Self::uop_site_index(&self.registry, caller.row, name, *func)
                        .ok_or(Trap::UnknownCallee {
                            module: name.to_owned(),
                            func: *func,
                        })?;
                    self.registry.resolve_indexed(name, func)
                },
                ConstFuncRef::Named { module, func } => {
                    let func = Self::uop_site_index(&self.registry, caller.row, module, *func)
                        .ok_or(Trap::UnknownCallee {
                            module: module.clone(),
                            func: *func,
                        })?;
                    self.registry.resolve_indexed(module, func)
                },
                // the bound tier answered ahead of this match
                ConstFuncRef::Bound { .. } => None,
            };
        if let Some((version, func_id)) = uop_target {
            let target = &self.registry.pinned(version).functions()[func_id as usize];
            debug_assert_eq!(target.entry.tier1_prologue(), func_id as usize);
            if usize::from(nargs) != target.layout.params_len() {
                return Err(Trap::ArityMismatch {
                    expected: target.layout.params_len(),
                    given: usize::from(nargs),
                });
            }
            let facts = SiteFacts::derive(caller_layout, &target.layout);
            return Ok(CallSite {
                caller: caller.func,
                pc,
                stamp,
                callee: CachedCallee::Uop {
                    version,
                    func: func_id,
                    facts,
                },
            });
        }
        // the native fallback belongs to name-keyed callees only: a
        // dep-position callee addressed a uop module at install, so its
        // miss here is a corrupted pool, never a native
        let (module, func) = match entry {
            ConstFuncRef::Uop { module, func } => {
                let name = self
                    .registry
                    .module_name(*module)
                    .ok_or(Trap::UnknownModule { module: *module })?;
                return Err(Trap::UnknownCallee {
                    module: name.to_owned(),
                    func: *func,
                });
            },
            ConstFuncRef::Named { module, func } => (module.as_str(), *func),
            // the bound tier answered ahead of this match
            ConstFuncRef::Bound { .. } => {
                unreachable!("the bound tier resolved ahead of the native fallback")
            },
        };
        let table = self
            .registry
            .native_table(module)
            .filter(|t| usize::try_from(func).is_ok_and(|i| i < t.len()))
            .ok_or(Trap::UnknownCallee {
                module: module.to_owned(),
                func,
            })?;
        let native = &table[func as usize];
        // a variadic native takes at least its named parameters (the
        // staged tail is one element's whole word span, its width the
        // call site's static fact); every other native takes exactly them
        let arity_ok = if native.variadic {
            usize::from(nargs) >= native.params.len()
        } else {
            usize::from(nargs) == native.params.len()
        };
        if !arity_ok {
            return Err(Trap::ArityMismatch {
                expected: native.params.len(),
                given: usize::from(nargs),
            });
        }
        Ok(CallSite {
            caller: caller.func,
            pc,
            stamp,
            callee: CachedCallee::Native {
                table: Arc::clone(table),
                index: func,
            },
        })
    }

    /// Resolves a dispatch site's (CALLG/CALLTG) pool FuncRef — fetched and
    /// matched by the calling arm — against the
    /// uop module tables: the same CALL-site inline cache bank CALL/CALLT
    /// key — the caller function's identity and pc, validated against the
    /// registry's install generation — so a dispatch tail site keys its
    /// callee exactly like every other call site, with the full lookup as
    /// the fallback and the refill. The dispatch arity rule differs from
    /// a plain call: the callee's parameters include the trailing
    /// evidence parameters the body claims, so `nargs` names only the
    /// real arguments and the full path proves `nargs <= params_len`
    /// (the staged evidence words' kinds are the arm's own check, on
    /// both paths). The sentinel evidence FuncRef never reaches here
    /// (its target is runtime data, never cacheable), and a generic
    /// builtin's FuncRef answers [`DispatchResolved::NotUop`] uncached —
    /// the static generic table needs no cache. Traps are never cached:
    /// every failure comes from the full path. The caller hands in the
    /// pool entry it already fetched and matched (the arm's own refusal
    /// path raised the pool traps), so the resolver never re-classifies
    /// the word, and a uop fill derives the site's layout facts against
    /// `caller_layout` — the plan is re-derived exactly when the callee
    /// is.
    pub(super) fn resolve_dispatch_site(
        &mut self,
        entry: &ConstFuncRef,
        caller: usize,
        caller_row: VersionIdx,
        caller_layout: &FrameLayout,
        pc: u32,
        nargs: u8,
    ) -> Result<DispatchResolved, Trap> {
        let at = pc as usize & (CALL_CACHE_SLOTS - 1);
        let stamp = self.registry.generation();
        let hit = self.call_cache[at].as_ref().is_some_and(|e| {
            e.caller == caller
                && e.pc == pc
                && e.stamp == stamp
                && matches!(e.callee, CachedCallee::Uop { .. })
        });
        if !hit {
            // an install-resolved id addresses its dep's module directly
            // (the name sits in the registry's id vector), a name through
            // the module table; a retiring caller's own-module index
            // translates through the identity its row records
            let target = match entry {
                ConstFuncRef::Uop { module, func } => {
                    let name = self
                        .registry
                        .module_name(*module)
                        .ok_or(Trap::UnknownModule { module: *module })?;
                    let func = Self::uop_site_index(&self.registry, caller_row, name, *func)
                        .ok_or(Trap::UnknownCallee {
                            module: name.to_owned(),
                            func: *func,
                        })?;
                    self.registry.resolve_indexed(name, func)
                },
                ConstFuncRef::Named { module, func } => {
                    let func = Self::uop_site_index(&self.registry, caller_row, module, *func)
                        .ok_or(Trap::UnknownCallee {
                            module: module.clone(),
                            func: *func,
                        })?;
                    self.registry.resolve_indexed(module, func)
                },
                // a bound C symbol never rides the generic dispatch (the
                // lowering refuses generic bound externs), so the honest
                // refusal names the callee rather than routing a foreign
                // address into the uop tables
                ConstFuncRef::Bound { module, index, .. } => {
                    return Err(Trap::UnknownCallee {
                        module: module.clone(),
                        func: *index,
                    });
                },
            };
            let Some((version, func_id)) = target else {
                return Ok(DispatchResolved::NotUop);
            };
            let target = &self.registry.pinned(version).functions()[func_id as usize];
            debug_assert_eq!(target.entry.tier1_prologue(), func_id as usize);
            if usize::from(nargs) > target.layout.params_len() {
                return Err(Trap::ArityMismatch {
                    expected: target.layout.params_len(),
                    given: usize::from(nargs),
                });
            }
            let facts = SiteFacts::derive(caller_layout, &target.layout);
            self.call_cache[at] = Some(CallSite {
                caller,
                pc,
                stamp,
                callee: CachedCallee::Uop {
                    version,
                    func: func_id,
                    facts: facts.clone(),
                },
            });
            return Ok(DispatchResolved::Uop {
                version,
                func: func_id,
                facts,
            });
        }
        match &self.call_cache[at]
            .as_ref()
            .expect("the slot was just validated")
            .callee
        {
            CachedCallee::Uop {
                version,
                func,
                facts,
            } => Ok(DispatchResolved::Uop {
                version: *version,
                func: *func,
                facts: facts.clone(),
            }),
            // unreachable by the hit predicate; answered defensively so a
            // corrupted bank degrades to the generic path's honest trap
            CachedCallee::Native { .. } | CachedCallee::Bound { .. } => {
                Ok(DispatchResolved::NotUop)
            },
        }
    }

    /// KSTR over the pool-const table: the cache remembers the (pool,
    /// fiber, index) → object-id mapping from the first load — each heap
    /// interns its own copy of a constant, so a row is only ever served
    /// against the fiber whose store minted it — and a hit serves the id
    /// only after validating that it still names a live string
    /// with the pool constant's exact content — no claim is held, so a
    /// program dropping its last reference still frees the object at the
    /// same moment as without the cache (a dead or recycled id simply
    /// falls back to the full intern). Every claiming load, first or
    /// cached, acquires one count for the destination slot, exactly the
    /// discipline a fresh intern follows; a borrow load (KSTR_BORROW,
    /// the placement pass's proof that the word's count never moves)
    /// claims nothing and the intern map pins the canonical copy. The
    /// row key is a hint, never an authority: a dead pool's row can meet
    /// a DIFFERENT pool at the reused address (wrong length, wrong kinds),
    /// so the hit path validates kind as well as content and any mismatch
    /// degrades to the miss path — the kind trap stays exactly where an
    /// uncached load raises it. `slot` is the executing frame's
    /// resolved row: the first load of a frame resolves it, every later
    /// load in that frame reads it, and a row reclaimed or shifted under
    /// the frame re-resolves through the key index once.
    pub(super) fn kstr_load(
        &mut self,
        pool: &[Const],
        kidx: ConstIdx,
        claim: bool,
        slot: &mut Option<KstrRowSlot>,
    ) -> Result<u32, Trap> {
        let idx = usize::from(kidx.0);
        // install proved the operand's pool index in range and its kind Str
        // (module.rs's body validation); the fetch indexes directly and the
        // kind arm is the corrupted-pool tripwire
        let text = match &pool[idx] {
            Const::Str(text) => text.as_str(),
            found => {
                return Err(Trap::PoolKind {
                    kidx: kidx.0,
                    expected: ConstKind::Str,
                    found: found.kind(),
                });
            },
        };
        let key: KstrRowKey = (pool.as_ptr() as usize, self.current.0);
        let cached = (*slot)
            .and_then(|at| self.kstr_cache.row(at, key))
            .and_then(|row| row.ids.get(idx))
            .copied()
            .flatten();
        if let Some(id) = cached
            && self.objects.is_live_str_with(id, text)
        {
            if claim {
                self.objects.inc(id)?;
            }
            return Ok(id);
        }
        let id = if claim { self.objects.intern(text) } else { self.objects.intern_borrow(text) };
        let (at, row) = self.kstr_cache.open(key, pool.len());
        *slot = Some(at);
        row.ids[idx] = Some(id);
        Ok(id)
    }
}
