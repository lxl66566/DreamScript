//! The two-slot module table [H6 item 5]: versioned module slots and
//! fully-qualified function resolution.

use std::{cell::RefCell, collections::BTreeMap, sync::Arc};

use ds_abi::{EntryDescriptor, Inst, InstDecodeError, Uop};

use crate::{
    native::NativeFunction,
    platform::Platform,
    program::{Const, ConstKind, FunctionDef},
};

/// Why a module version cannot be loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModuleBuildError {
    /// Two functions in one version share a name.
    DuplicateFunction(String),
    /// A function's pool exceeds the u16 index space the encoding addresses.
    PoolTooLarge { func: String, len: usize },
    /// A function's frame exceeds the u8 slot space the ISA addresses —
    /// the same width limit the lowerer enforces, caught here so a
    /// producer bypassing ds-lower fails at load instead of panicking at
    /// a slot operand.
    FrameTooWide {
        /// The function whose layout is too wide.
        func: String,
        /// The layout's slot count.
        slots: usize,
    },
    /// A body word does not decode: the stream is not canonical.
    BodyDecode {
        /// The function whose body is malformed.
        func: String,
        /// The instruction index of the malformed word.
        pc: u32,
        /// Why the word does not decode (uops outside 0xE, RC ops inside).
        cause: InstDecodeError,
    },
    /// A `FUNCF` word disagrees with the function's frame layout slot count.
    FuncfMismatch {
        /// The function whose entry disagrees.
        func: String,
        /// The `FUNCF` operand.
        funcef: u16,
        /// The layout's slot count.
        layout: usize,
    },
    /// A pool-index operand is out of range for its function's pool.
    PoolIndex {
        /// The function whose body addresses the pool.
        func: String,
        /// The offending pool index.
        kidx: u16,
    },
    /// A pool entry is loaded by an opcode of the wrong kind.
    PoolKind {
        /// The function whose pool holds the entry.
        func: String,
        /// The offending pool index.
        kidx: u16,
        /// The kind the opcode requires.
        expected: ConstKind,
        /// The kind the entry holds.
        found: ConstKind,
    },
    /// A RETSPAN addresses slots past its function's frame: the span must
    /// fit inside the callee's own slots (`src + n <= slots`), which is a
    /// static layout fact.
    RetSpanLayout {
        /// The function whose body carries the op.
        func: String,
        /// The span's first slot.
        src: u8,
        /// The span's word count.
        span: u8,
        /// The function's slot count.
        slots: usize,
    },
    /// An evidence-chain entry indexes outside the module's own tables
    /// (the function or the chain table itself): the dsmod reader rejects
    /// this class at parse, so reaching it names a bypassed producer.
    ChainIndex {
        /// The offending chain-table index.
        chain: usize,
        /// The index that escapes its table.
        index: u32,
    },
}

/// A loaded function: definition plus its two-word entry descriptor [H2].
/// The tier-2 word is null until the JIT exists, so
/// [`EntryDescriptor::resolve`] answers with word 0: this function's index
/// in the module version's function table.
#[derive(Debug)]
pub struct Function {
    /// The function's name within its module.
    pub name: String,
    /// The function's static frame layout [BL-6].
    pub layout: ds_abi::FrameLayout,
    /// The function's constant pool (the dsmod blocks-payload granularity).
    pub pool: Vec<Const>,
    /// The encoded uop body [BL-1].
    pub code: Vec<u32>,
    /// The two-word entry descriptor; word 0 is this function's table index.
    pub entry: EntryDescriptor,
}

/// One immutable version of a module: a name, a function table, its name
/// index, and the content hash the load protocol validates dependency
/// links against when the version is installed. Every function body is
/// decode-checked once at load, its `FUNCF` cross-checked against its
/// layout, and every pool-index operand checked against its pool's
/// kinds, so a bad stream never reaches the dispatcher.
#[derive(Debug)]
pub struct ModuleVersion {
    name: String,
    functions: Vec<Function>,
    /// The evidence-chain table [H3]: one entry per chain object the
    /// staged evidence words index. Module-local — a chain's `func` and
    /// `next` indexes address this version's own tables, resolved at
    /// dispatch like the sentinel slot word, so no install-time rewrite
    /// runs over them.
    chains: Vec<ds_mod::EvidenceChain>,
    index: BTreeMap<String, u32>,
    /// The content identity the load protocol validates dependency
    /// links against (a dsmod module's content hash is its source hash
    /// this round); `None` for a directly built version, which carries
    /// no content identity.
    content_hash: Option<ds_mod::Hash128>,
}

impl ModuleVersion {
    /// Loads a version: rejects duplicate names, checks every frame against
    /// the ISA's u8 slot space, decode-checks every body word, cross-checks
    /// each `FUNCF` against its layout, validates every pool access, and
    /// assigns function-table indices (which become entry descriptor word
    /// 0). The tier-2 word stays null; it flips exactly once when the JIT
    /// compiles the function [H2].
    pub fn new(name: impl Into<String>, defs: Vec<FunctionDef>) -> Result<Self, ModuleBuildError> {
        let mut functions = Vec::with_capacity(defs.len());
        let mut index = BTreeMap::new();
        for (position, def) in defs.into_iter().enumerate() {
            let func_id = u32::try_from(position).expect("function table exceeds u32");
            if index.insert(def.name.clone(), func_id).is_some() {
                return Err(ModuleBuildError::DuplicateFunction(def.name));
            }
            validate_body(&def)?;
            functions.push(Function {
                entry: EntryDescriptor::new_interpreted(position),
                name: def.name,
                layout: def.layout,
                pool: def.pool,
                code: def.code,
            });
        }
        Ok(Self {
            name: name.into(),
            functions,
            chains: Vec::new(),
            index,
            content_hash: None,
        })
    }

    /// Attaches the module's evidence-chain table [H3], validating every
    /// entry's function index against the function table and every tail
    /// index against the chain table itself.
    ///
    /// # Errors
    ///
    /// Returns [`ModuleBuildError::ChainIndex`] when an entry escapes its
    /// table — the dsmod reader already rejects the class at parse, so a
    /// failure here names a producer that bypassed it.
    pub fn with_chains(
        mut self,
        chains: Vec<ds_mod::EvidenceChain>,
    ) -> Result<Self, ModuleBuildError> {
        let len = chains.len();
        for (chain, entry) in chains.iter().enumerate() {
            if usize::try_from(entry.func).is_ok_and(|func| func < self.functions.len())
                && entry
                    .next
                    .iter()
                    .all(|tail| usize::try_from(*tail).is_ok_and(|tail| tail < len))
            {
                continue;
            }
            let index = if usize::try_from(entry.func).is_ok_and(|func| func < self.functions.len())
            {
                entry.next.first().copied().unwrap_or(u32::MAX)
            } else {
                entry.func
            };
            return Err(ModuleBuildError::ChainIndex { chain, index });
        }
        self.chains = chains;
        Ok(self)
    }

    /// Records the version's content hash: the identity the load
    /// protocol validates a dependent's linked hash against. The dsmod
    /// install stamps it from the module's source hash; a host building
    /// a version directly records none.
    #[must_use]
    pub fn with_content_hash(mut self, hash: ds_mod::Hash128) -> Self {
        self.content_hash = Some(hash);
        self
    }

    /// The version's recorded content hash, if any: what a dependency
    /// link's hash is compared against when this version holds the
    /// linked name's current slot.
    #[must_use]
    pub const fn content_hash(&self) -> Option<ds_mod::Hash128> {
        self.content_hash
    }

    /// The module's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// All functions in table order; the position is entry word 0.
    #[must_use]
    pub fn functions(&self) -> &[Function] {
        &self.functions
    }

    /// The evidence-chain table the staged evidence words index [H3].
    #[must_use]
    pub fn chains(&self) -> &[ds_mod::EvidenceChain] {
        &self.chains
    }

    /// Looks a function up by name.
    #[must_use]
    pub fn function(&self, name: &str) -> Option<&Function> {
        let &func_id = self.index.get(name)?;
        self.functions.get(func_id as usize)
    }
}

/// Load-time body validation: canonical decode, FUNCF-vs-layout agreement,
/// and pool-index/kind agreement [BL-5, BL-6].
fn validate_body(def: &FunctionDef) -> Result<(), ModuleBuildError> {
    if def.pool.len() > usize::from(u16::MAX) + 1 {
        return Err(ModuleBuildError::PoolTooLarge {
            func: def.name.clone(),
            len: def.pool.len(),
        });
    }
    // slot operands are u8-encoded: a wider frame could never be addressed
    if def.layout.slots().len() > usize::from(u8::MAX) {
        return Err(ModuleBuildError::FrameTooWide {
            func: def.name.clone(),
            slots: def.layout.slots().len(),
        });
    }
    for (pc, &word) in def.code.iter().enumerate() {
        let pc = u32::try_from(pc).expect("pc fits u32");
        let inst = Inst::decode(pc, word).map_err(|cause| ModuleBuildError::BodyDecode {
            func: def.name.clone(),
            pc,
            cause,
        })?;
        let op = match inst {
            Inst::Uop(op) => op,
            // the 0xE RC pair (C4): no pool, no layout fact to check —
            // the slot addressing traps at run time like every other
            // operand
            Inst::Rc(_) => continue,
        };
        if let Uop::Funcf { frame_slots } = op {
            if usize::from(frame_slots) != def.layout.slots().len() {
                return Err(ModuleBuildError::FuncfMismatch {
                    func: def.name.clone(),
                    funcef: frame_slots,
                    layout: def.layout.slots().len(),
                });
            }
        }
        // the RETSPAN span is a static frame-layout fact: it must fit
        // inside the callee's own slots
        if let Uop::RetSpan { src, n } = op {
            if usize::from(src) + usize::from(n) > def.layout.slots().len() {
                return Err(ModuleBuildError::RetSpanLayout {
                    func: def.name.clone(),
                    src,
                    span: n,
                    slots: def.layout.slots().len(),
                });
            }
        }
        // which pool kinds each loading opcode accepts, and the primary
        // kind its trap names: KIMM loads i64 entries, KCONST the non-i64
        // constant words (this VM executes f64, bool, and fn words), KSTR
        // the string entries, CALL/CALLT/CALLG/CALLTG the function
        // references
        let (kidx, accepts, expected): (_, &[_], _) = match op {
            Uop::Kimm { kidx, .. } => (kidx, &[ConstKind::I64], ConstKind::I64),
            Uop::Kconst { kidx, .. } => (
                kidx,
                &[ConstKind::F64, ConstKind::Bool, ConstKind::FuncRef],
                ConstKind::F64,
            ),
            Uop::Kstr { kidx, .. } | Uop::KstrBorrow { kidx, .. } => {
                (kidx, &[ConstKind::Str], ConstKind::Str)
            },
            Uop::Call { callee, .. }
            | Uop::Callt { callee, .. }
            | Uop::Callg { callee, .. }
            | Uop::Calltg { callee, .. } => (callee, &[ConstKind::FuncRef], ConstKind::FuncRef),
            _ => continue,
        };
        let found = def
            .pool
            .get(usize::from(kidx.0))
            .ok_or(ModuleBuildError::PoolIndex {
                func: def.name.clone(),
                kidx: kidx.0,
            })?;
        if !accepts.contains(&found.kind()) {
            return Err(ModuleBuildError::PoolKind {
                func: def.name.clone(),
                kidx: kidx.0,
                expected,
                found: found.kind(),
            });
        }
    }
    Ok(())
}

/// A module version's row in the per-VM version table: the u32 name the hot
/// seams carry — every frame's chain entry, every parked fiber's resume
/// point, every CALL-cache row — in place of a shared handle, so a call or
/// a return moves no reference count. Assigned at install, never reused:
/// the table only appends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VersionIdx(pub(crate) u32);

#[derive(Debug)]
pub struct ModuleSlots {
    /// The module id a scalar FuncRef word addresses (F2): this name's
    /// first-install position in the registry's id space.
    id: u32,
    current: VersionIdx,
    retiring: Option<VersionIdx>,
}

impl ModuleSlots {
    /// The version-table row of the version qualified calls resolve
    /// through.
    #[must_use]
    pub fn current_idx(&self) -> VersionIdx {
        self.current
    }

    /// The version-table row of the previous version, if any frames may
    /// still run it.
    #[must_use]
    pub fn retiring_idx(&self) -> Option<VersionIdx> {
        self.retiring
    }
}

/// The module registry: every loaded module's two slots, the version
/// table, and the native modules. The VM owns one and resolves every call
/// through the current slot, so a swap changes the destination of
/// subsequent calls immediately; frames already running a retiring version
/// keep their code and pool until nothing references them — every frame
/// carries a [`VersionIdx`], and the table owns each version's survival
/// until an install-time sweep proves nothing references its row [H6
/// item 5]. Native modules ([`NativeFunction`] tables, the builtin
/// mechanism) live in the same name space: a pool FuncRef resolves through
/// the uop tables first, the native tables second, so `CALL` has one
/// resolution path for both callee kinds.
#[derive(Debug, Default)]
pub struct ModuleRegistry {
    modules: BTreeMap<String, ModuleSlots>,
    natives: BTreeMap<String, Arc<Vec<NativeFunction>>>,
    /// The installed platform handlers, by name (H5.5, E1): the world
    /// surface's capability gate — a world call site's FuncRef resolves
    /// through the platform's table only when the platform serves the
    /// op's capability.
    platforms: BTreeMap<String, Arc<Platform>>,
    /// The module-id space scalar FuncRef words address (F2): one id per
    /// module name, assigned at first install and stable across swaps.
    /// Name-keyed modules are the interim until the M7 loading round's
    /// link table assigns module indices, mirroring the pool FuncRef's
    /// name-keyed interim. This vector is the id-to-name direction; the
    /// name-to-id direction rides each slot's own `id`, so a name lookup
    /// is one `modules` descent.
    id_names: Vec<String>,
    /// The per-VM version table: one row per installed version, appended
    /// at install and never reused. The row holds the one `Arc` that owns
    /// the version's survival — the version's single home, so immutable
    /// code stays `Send + Sync` for worker sharing — and every other
    /// holder of a version is a [`VersionIdx`] naming the row. A row is
    /// dropped only by the install-time sweep, which proves no live frame
    /// or parked fiber names it — the hint and cache entries never reach a
    /// sweep, since the install takes the hint and empties the cache
    /// before it runs.
    versions: Vec<Option<Arc<ModuleVersion>>>,
    /// The last run entry [`ModuleRegistry::resolve`] answered, as of
    /// `generation`: a host drive re-entering one entry skips both name
    /// lookups. A hint beside the authoritative lookup, so a miss costs one
    /// full resolution, and a module install clears it.
    entry: RefCell<Option<ResolvedEntry>>,
    /// Bumped by every install (module version or native table): the
    /// version stamp the CALL-site inline cache validates against. Any
    /// swap redirects subsequent calls through the current slot, so one
    /// monotonic counter covering both tables is the whole invalidation
    /// story — a stamped hit is a resolution no install has undone.
    generation: u64,
}

/// One run entry resolution: the names it answered for, the install
/// generation it ran against, and its answer.
#[derive(Debug)]
struct ResolvedEntry {
    module: String,
    func: String,
    stamp: u64,
    version: VersionIdx,
    func_id: u32,
}

impl ModuleRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Installs a version: it becomes the module's current slot, the
    /// previous current moves to the retiring slot, and the version table
    /// appends the installed version as a fresh row (the row the retiring
    /// slot drops is the sweep's business, never this method's). Answers
    /// the installed version's table row: the row's `Arc` is the one
    /// owner of the version's survival, and no second home exists to
    /// outlive it — a host inspecting the installed code reads through
    /// [`ModuleRegistry::version`] on this row while it stays live.
    pub fn install(&mut self, version: ModuleVersion) -> VersionIdx {
        let name = version.name().to_owned();
        let installed = Arc::new(version);
        let row = VersionIdx(u32::try_from(self.versions.len()).expect("version rows fit u32"));
        self.versions.push(Some(installed));
        if let Some(slots) = self.modules.get_mut(&name) {
            slots.retiring = Some(slots.current);
            slots.current = row;
        } else {
            let id = u32::try_from(self.id_names.len()).expect("module count fits u32");
            self.id_names.push(name.clone());
            self.modules.insert(name, ModuleSlots {
                id,
                current: row,
                retiring: None,
            });
        }
        self.generation += 1;
        self.entry.borrow_mut().take();
        row
    }

    /// The install generation: the version stamp inline caches validate
    /// against. Every install (version swap or native-table replacement)
    /// bumps it, so a cache hit whose stamp matches resolved against the
    /// registry's exact current state.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The module id a scalar FuncRef word addresses (F2): assigned at
    /// first install, kept across hot-reload swaps, and answered by the
    /// module's own slot.
    #[must_use]
    pub fn module_id(&self, module: &str) -> Option<u32> {
        self.modules.get(module).map(|slots| slots.id)
    }

    /// The module id a payload FuncRef naming `module` resolves to at
    /// install: the installed name's own id, or the fresh tail id the
    /// name's first install assigns. Ids are one per name in
    /// first-install order and never move, so a pre-install resolve and
    /// the install itself agree; a failed install discards the derived
    /// id and the next attempt derives it again.
    pub(crate) fn id_for_install(&self, module: &str) -> u32 {
        self.modules.get(module).map_or_else(
            || u32::try_from(self.id_names.len()).expect("module count fits u32"),
            |slots| slots.id,
        )
    }

    /// The module name an id addresses, for call-time resolution of
    /// scalar FuncRef words.
    pub(crate) fn module_name(&self, module: u32) -> Option<&str> {
        self.id_names.get(module as usize).map(String::as_str)
    }

    /// The version a table row names, if the sweep has not retired it:
    /// the host's only read of installed code — no owning handle exists,
    /// the row is the version's single home, so the borrow ends with the
    /// row's life.
    #[must_use]
    pub fn version(&self, row: VersionIdx) -> Option<&ModuleVersion> {
        self.versions.get(row.0 as usize).and_then(Option::as_deref)
    }

    /// The strong count of the row a `VersionIdx` names (0 once the sweep
    /// retired it): the single-home invariant's witness for this crate's
    /// own tests — the row's `Arc` is the version's one owner, so a live
    /// row answers 1, and any seam regressing into minting a second home
    /// moves the count.
    #[cfg(test)]
    pub(crate) fn row_arc_count(&self, row: VersionIdx) -> usize {
        self.versions
            .get(row.0 as usize)
            .and_then(Option::as_ref)
            .map_or(0, Arc::strong_count)
    }

    /// The version a table row names, read through the address-stable
    /// pointer its row pins: the row's `Arc` keeps the version's heap
    /// allocation alive and unmoved from install until a sweep retires
    /// the row. The answer carries no borrow, so a dispatch arm may hold
    /// it across the arm's own whole-VM calls.
    ///
    /// Safety of the raw pointer: the sweep retires a row only when no
    /// frame or parked fiber names it, and a sweep runs only inside an
    /// install — a host call on the `!Send`, single-threaded VM, which
    /// also empties every cache entry before the sweep, so no dispatch
    /// is in flight while a row retires and every reference this method
    /// answers is dead before the next install. Nothing that outlives a
    /// dispatch may hold the answer.
    pub(crate) fn pinned(&self, row: VersionIdx) -> &'static ModuleVersion {
        let version = self
            .version(row)
            .expect("a named row stays pinned until a sweep retires it");
        // SAFETY: the row's Arc pins the version's allocation for the
        // method's whole contract (see above), so the pointee is valid.
        unsafe { &*std::ptr::from_ref(version) }
    }

    /// How many rows the version table holds (live and retired): the
    /// reachability bitset's width at a sweep.
    pub(crate) fn version_rows(&self) -> usize {
        self.versions.len()
    }

    /// Marks the rows the registry itself keeps alive into `live`: every
    /// module's current and retiring slot. The run-entry hint never
    /// reaches a sweep — the install that runs the sweep takes the hint
    /// first — so it is no marking source.
    pub(crate) fn mark_own_rows(&self, live: &mut [bool]) {
        for slots in self.modules.values() {
            live[slots.current.0 as usize] = true;
            if let Some(row) = slots.retiring {
                live[row.0 as usize] = true;
            }
        }
    }

    /// The install-time sweep's retire half: drops every row `live` leaves
    /// unmarked and answers the pool addresses of the retired versions'
    /// functions (the kstr rows keyed by those pools are dead hints). A
    /// dropped row releases the table's one reference, and the row is the
    /// version's single home, so the version's memory frees right there.
    pub(crate) fn retire_unmarked_rows(&mut self, live: &[bool]) -> Vec<usize> {
        let mut pools = Vec::new();
        for (at, row) in self.versions.iter_mut().enumerate() {
            if live[at] {
                continue;
            }
            if let Some(version) = row.take() {
                pools.extend(version.functions().iter().map(|f| f.pool.as_ptr() as usize));
            }
        }
        pools
    }

    /// Resolves a function by name through the module's current slot, for
    /// run entries: the version's table row plus the function's table
    /// index. The last answer is kept in the `entry` hint, so a drive
    /// re-entering one entry answers without either name lookup; the
    /// answer names a row, so resolution moves no reference count.
    #[must_use]
    pub fn resolve(&self, module: &str, func: &str) -> Option<(VersionIdx, u32)> {
        {
            let entry = self.entry.borrow();
            if let Some(entry) = entry.as_ref() {
                if entry.stamp == self.generation && entry.module == module && entry.func == func {
                    return Some((entry.version, entry.func_id));
                }
            }
        }
        let slots = self.modules.get(module)?;
        let version = slots.current;
        let &func_id = self.version(version)?.index.get(func)?;
        *self.entry.borrow_mut() = Some(ResolvedEntry {
            module: module.to_owned(),
            func: func.to_owned(),
            stamp: self.generation,
            version,
            func_id,
        });
        Some((version, func_id))
    }

    /// Resolves a pool FuncRef through the target module's current slot
    /// [H6 item 5]: `func` is the function-table index (entry word 0). A
    /// swap between a call's issue and its resolution redirects here, and
    /// the answer names a table row, so the resolution mints no handle.
    #[must_use]
    pub fn resolve_indexed(&self, module: &str, func: u32) -> Option<(VersionIdx, u32)> {
        let version = self.modules.get(module)?.current;
        let bounds = |functions: &[_]| usize::try_from(func).is_ok_and(|i| i < functions.len());
        self.version(version)
            .filter(|v| bounds(&v.functions))
            .map(|_| (version, func))
    }

    /// Whether a uop module under `module`'s current slot would resolve
    /// `func`: the bounds-only existence check, without materializing a
    /// version handle. The generic-call site's shadow check and install-time
    /// FuncRef validation run it.
    #[must_use]
    pub fn has_uop(&self, module: &str, func: u32) -> bool {
        self.modules
            .get(module)
            .and_then(|slots| self.version(slots.current))
            .is_some_and(|version| usize::try_from(func).is_ok_and(|i| i < version.functions.len()))
    }

    /// A module's two slots, if the module is loaded.
    #[must_use]
    pub fn slots(&self, module: &str) -> Option<&ModuleSlots> {
        self.modules.get(module)
    }

    /// The current version's own reference, if the module is loaded: the
    /// slot lookup for hosts inspecting code (the version table's row
    /// indirection).
    #[must_use]
    pub fn current_version(&self, module: &str) -> Option<&ModuleVersion> {
        self.modules
            .get(module)
            .and_then(|slots| self.version(slots.current))
    }

    /// The retiring version's own reference, if the module has one: the
    /// version a pending sweep may still retire.
    #[must_use]
    pub fn retiring_version(&self, module: &str) -> Option<&ModuleVersion> {
        self.modules
            .get(module)
            .and_then(|slots| self.version(slots.retiring?))
    }

    /// Installs a platform handler (H5.5, E1): the world surface `platform`
    /// serves, under its own name — the module name the source's platform
    /// declaration bakes into the lowered world calls' pool FuncRefs. The
    /// platform's op table installs as a native table (served ops' bodies,
    /// unserved ops as reserved traps), and the registry records the
    /// platform so install-time FuncRef validation can refuse a call site
    /// whose op the platform does not serve. A same-named prior platform
    /// replaces it, exactly like a native table.
    pub fn install_platform(&mut self, platform: Platform) {
        let name = platform.name().to_owned();
        let shared = Arc::new(platform);
        let table = crate::platform::platform_module(&shared);
        self.natives.insert(name.clone(), Arc::new(table));
        self.generation += 1;
        self.platforms.insert(name, shared);
    }

    /// Whether an installed platform named `module` serves the world op
    /// at `index` of the fixed table: `None` when no such platform is
    /// installed (the call site resolves like any uninstalled native
    /// module), `Some(false)` when the platform does not serve the op's
    /// capability.
    #[must_use]
    pub fn platform_serves(&self, module: &str, index: u32) -> Option<bool> {
        let platform = self.platforms.get(module)?;
        Some(platform.serves(index))
    }

    /// Installs a native module: a name plus its function table, whose
    /// indices pool FuncRefs address. A second install under the same name
    /// replaces the table (the same replacement discipline as a uop
    /// version's swap, without the retiring slot — native code holds no
    /// live frames).
    pub fn install_native(&mut self, name: &str, functions: Vec<NativeFunction>) {
        self.natives.insert(name.to_owned(), Arc::new(functions));
        self.generation += 1;
    }

    /// Resolves a pool FuncRef against the native modules: the function
    /// at `func`'s index, if the module is installed and the index is in
    /// range.
    #[must_use]
    pub fn resolve_native(&self, module: &str, func: u32) -> Option<&NativeFunction> {
        let fns = self.natives.get(module)?;
        (usize::try_from(func).is_ok_and(|i| i < fns.len())).then(|| &fns[func as usize])
    }

    /// The native module table a pool FuncRef addresses, if installed: the
    /// shared handle the CALL-site inline cache stores for a native callee.
    #[must_use]
    pub fn native_table(&self, module: &str) -> Option<&Arc<Vec<NativeFunction>>> {
        self.natives.get(module)
    }
}

#[cfg(test)]
mod tests {
    use ds_abi::uop::{DecodeError, Uop};

    use super::*;
    use crate::program::assemble;

    fn empty_def(name: &str) -> FunctionDef {
        FunctionDef {
            name: name.into(),
            layout: ds_abi::FrameLayout::new(Vec::new(), 0, 0).expect("valid layout"),
            pool: Vec::new(),
            code: Vec::new(),
        }
    }

    #[test]
    fn load_writes_entry_word_zero_once() {
        let version =
            ModuleVersion::new("m", vec![empty_def("a"), empty_def("b")]).expect("valid module");
        for (position, f) in version.functions().iter().enumerate() {
            // word 0 is the table index, immutable after load; word 1 is the
            // null sentinel, so resolve() answers with word 0
            assert_eq!(f.entry.tier(), ds_abi::Tier::Interpreted);
            assert_eq!(f.entry.resolve(), position);
        }
        assert_eq!(version.function("b").map(|f| f.entry.resolve()), Some(1));
    }

    #[test]
    fn resolve_answers_the_current_slot_and_follows_a_swap() {
        let mut reg = ModuleRegistry::new();
        let v1_row = reg.install(
            ModuleVersion::new("m", vec![empty_def("a"), empty_def("b")]).expect("valid module"),
        );
        let (first, func_id) = reg.resolve("m", "b").expect("installed");
        assert_eq!(func_id, 1);
        assert_eq!(first, v1_row);
        // the repeat query answers from the hint, and a name the hint does
        // not carry falls back to the full lookup
        assert_eq!(reg.resolve("m", "b").expect("installed").0, first);
        assert!(reg.resolve("m", "nowhere").is_none());
        let v2_row = reg.install(
            ModuleVersion::new("m", vec![empty_def("a"), empty_def("b")]).expect("valid module"),
        );
        // the swap moved v1's row to the retiring slot; the row's own Arc
        // is the version's single home, so its count never left 1
        assert_eq!(reg.row_arc_count(v1_row), 1);
        assert_eq!(
            reg.slots("m").and_then(ModuleSlots::retiring_idx),
            Some(v1_row)
        );
        let (second, _) = reg.resolve("m", "b").expect("installed");
        assert_eq!(second, v2_row);
    }

    #[test]
    fn the_sweep_retires_only_unnamed_rows() {
        let mut reg = ModuleRegistry::new();
        let v1_row = reg.install(
            ModuleVersion::new("m", vec![empty_def("a"), empty_def("b")]).expect("valid module"),
        );
        let v1_arc = reg.versions[v1_row.0 as usize]
            .clone()
            .expect("the fresh row holds its version");
        // two more installs: the second moves v1's row out of every slot
        // the registry itself marks
        reg.install(
            ModuleVersion::new("m", vec![empty_def("a"), empty_def("b")]).expect("valid module"),
        );
        reg.install(
            ModuleVersion::new("m", vec![empty_def("a"), empty_def("b")]).expect("valid module"),
        );
        let mut live = vec![false; reg.version_rows()];
        reg.mark_own_rows(&mut live);
        live[v1_row.0 as usize] = true; // a parked fiber's frame
        let pools = reg.retire_unmarked_rows(&live);
        assert!(pools.is_empty(), "the named row stays");
        assert!(std::ptr::eq(
            reg.version(v1_row).expect("still live"),
            v1_arc.as_ref()
        ));
        assert_eq!(
            reg.row_arc_count(v1_row),
            2,
            "the row plus this test's private probe"
        );
        // the same sweep with no live reference drops the row and answers
        // the retired version's pool addresses; the row was the version's
        // single home, so the drop frees it (this probe keeps the last
        // count here)
        let mut live = vec![false; reg.version_rows()];
        reg.mark_own_rows(&mut live);
        let pools = reg.retire_unmarked_rows(&live);
        assert_eq!(pools.len(), 2, "the two retired functions' pools");
        assert!(reg.version(v1_row).is_none());
        assert_eq!(reg.row_arc_count(v1_row), 0);
    }

    /// The worker-sharing contract the fork-join tier queues on: a
    /// version's immutable code is `Send + Sync`, so the table's rows can
    /// back workers reading code concurrently.
    #[test]
    fn module_version_code_is_send_and_sync() {
        fn assert_shared<T: Send + Sync>() {}
        assert_shared::<ModuleVersion>();
        assert_shared::<Arc<ModuleVersion>>();
    }

    #[test]
    fn duplicate_names_are_rejected() {
        assert_eq!(
            ModuleVersion::new("m", vec![empty_def("a"), empty_def("a")]).unwrap_err(),
            ModuleBuildError::DuplicateFunction("a".into())
        );
    }

    #[test]
    fn frames_past_the_u8_slot_space_are_rejected_at_load() {
        // slot operands are u8-encoded: a producer bypassing ds-lower's
        // width check fails here, never at a runtime slot conversion
        let mut def = empty_def("wide");
        def.layout =
            ds_abi::FrameLayout::new(vec![ds_abi::SlotType::I64; 256], 0, 0).expect("valid layout");
        assert_eq!(
            ModuleVersion::new("m", vec![def]).unwrap_err(),
            ModuleBuildError::FrameTooWide {
                func: "wide".into(),
                slots: 256,
            }
        );
        // u8::MAX slots is the widest addressable frame and loads
        let mut def = empty_def("edge");
        def.layout =
            ds_abi::FrameLayout::new(vec![ds_abi::SlotType::I64; 255], 0, 0).expect("valid layout");
        assert!(ModuleVersion::new("m", vec![def]).is_ok());
    }

    #[test]
    fn non_canonical_body_words_are_rejected_at_load() {
        // reserved major 0xF: decode fails, so the loader refuses the stream
        let mut def = empty_def("bad");
        def.code = vec![0xf0_00_00_00];
        assert_eq!(
            ModuleVersion::new("m", vec![def]).unwrap_err(),
            ModuleBuildError::BodyDecode {
                func: "bad".into(),
                pc: 0,
                cause: InstDecodeError::Uop(DecodeError::UnknownOpcode {
                    major: 0xf,
                    minor: 0
                }),
            }
        );
        // inside the RC major, a noncanonical field bit is the RC error
        let mut def = empty_def("bad");
        def.code = vec![0xe0_00_01_00];
        assert_eq!(
            ModuleVersion::new("m", vec![def]).unwrap_err(),
            ModuleBuildError::BodyDecode {
                func: "bad".into(),
                pc: 0,
                cause: InstDecodeError::Rc(ds_abi::RcDecodeError::ReservedField),
            }
        );
    }

    #[test]
    fn canonical_rc_words_load_beside_the_uops() {
        // the 0xE pair loads as part of the body (C4): no pool operand,
        // no layout fact; the word stream carries both majors
        let mut def = empty_def("rc");
        def.layout =
            ds_abi::FrameLayout::new(vec![ds_abi::SlotType::Ref], 0, 0).expect("valid layout");
        def.code = assemble(&[
            Inst::Uop(Uop::Funcf { frame_slots: 1 }),
            Inst::Rc(ds_abi::RcOp::Inc { src: 0 }),
            Inst::Rc(ds_abi::RcOp::Dec { src: 0 }),
        ])
        .expect("the words encode");
        let version = ModuleVersion::new("m", vec![def]).expect("the RC words load");
        assert_eq!(version.functions().len(), 1);
    }

    #[test]
    fn funcf_disagreement_with_the_layout_is_rejected_at_load() {
        let mut def = empty_def("bad");
        def.layout =
            ds_abi::FrameLayout::new(vec![ds_abi::SlotType::I64], 0, 0).expect("valid layout");
        def.code = assemble(&[Uop::Funcf { frame_slots: 2 }]).expect("encodes");
        assert_eq!(
            ModuleVersion::new("m", vec![def]).unwrap_err(),
            ModuleBuildError::FuncfMismatch {
                func: "bad".into(),
                funcef: 2,
                layout: 1,
            }
        );
    }

    #[test]
    fn pool_kind_and_index_disagreements_are_rejected_at_load() {
        // KIMM addressing a bool entry
        let mut def = empty_def("k");
        def.layout =
            ds_abi::FrameLayout::new(vec![ds_abi::SlotType::Bool], 0, 0).expect("valid layout");
        def.pool = vec![Const::Bool(true)];
        def.code = assemble(&[Uop::Funcf { frame_slots: 1 }, Uop::Kimm {
            dst: 0,
            kidx: ds_abi::ConstIdx(0),
        }])
        .expect("encodes");
        assert_eq!(
            ModuleVersion::new("m", vec![def]).unwrap_err(),
            ModuleBuildError::PoolKind {
                func: "k".into(),
                kidx: 0,
                expected: ConstKind::I64,
                found: ConstKind::Bool,
            }
        );
        // a pool index past the end
        let mut def = empty_def("p");
        def.code = assemble(&[Uop::Kimm {
            dst: 0,
            kidx: ds_abi::ConstIdx(3),
        }])
        .expect("encodes");
        assert_eq!(
            ModuleVersion::new("m", vec![def]).unwrap_err(),
            ModuleBuildError::PoolIndex {
                func: "p".into(),
                kidx: 3,
            }
        );
    }

    #[test]
    fn assemble_and_load_round_trip_the_record_sum_to_body() {
        // the canonical sum_to body, with the executable corrections of
        // the fixture suite: JMPIF exit, separate constant scratch slots
        let body = [
            Uop::Funcf { frame_slots: 5 },
            Uop::Kimm {
                dst: 1,
                kidx: ds_abi::ConstIdx(0),
            },
            Uop::Kimm {
                dst: 3,
                kidx: ds_abi::ConstIdx(0),
            },
            Uop::LeI64 {
                dst: 2,
                lhs: 0,
                rhs: 3,
            },
            Uop::Jmpif { cond: 2, target: 9 },
            Uop::AddI64 {
                dst: 1,
                lhs: 1,
                rhs: 0,
            },
            Uop::Kimm {
                dst: 4,
                kidx: ds_abi::ConstIdx(1),
            },
            Uop::SubI64 {
                dst: 0,
                lhs: 0,
                rhs: 4,
            },
            Uop::Jmp { target: 3 },
            Uop::Ret { src: 1 },
        ];
        let words = assemble(&body).expect("canonical body assembles");
        let version = ModuleVersion::new("mut", vec![FunctionDef {
            name: "sum_to".into(),
            layout: ds_abi::FrameLayout::new(
                vec![
                    ds_abi::SlotType::I64,
                    ds_abi::SlotType::I64,
                    ds_abi::SlotType::Bool,
                    ds_abi::SlotType::I64,
                    ds_abi::SlotType::I64,
                ],
                1,
                0,
            )
            .expect("valid layout"),
            pool: vec![Const::I64(0), Const::I64(1)],
            code: words,
        }])
        .expect("valid module");
        assert_eq!(version.function("sum_to").map(|f| f.pool.len()), Some(2));
    }
}
