//! The dsmod loader: install a `ds-mod` module into the two-slot registry
//! and execute it.
//!
//! Mapping decisions:
//!
//! - Pools. The dsmod blocks payload carries one constant pool per function ("uop ConstIdx operands
//!   index it"), and this VM's [`FunctionDef`] matches that granularity. ds-abi's uop module doc
//!   says "one pool per module" — the two locked contracts disagree, and the executable model
//!   follows the payload that files actually carry.
//! - Callees. A payload [`ds_mod::Constant::FuncRef`] carries a selector-led key (the format-v4
//!   spelling): a uop dependency's position in the header's dep list (position 0 the enclosing
//!   module, position `n` the `n-1`-th entry) or a native surface's reserved name, plus the
//!   callee's function index in that module's blocks payload. The loader resolves and validates at
//!   install time — the enclosing module's index against this module's own table, a dependency's
//!   against the target's currently installed version (so cross-module references require
//!   dependencies to be installed first) — and stores the pool entry the uop contract expects: a
//!   dep-position key becomes the target's registry module id (position → dep name → id, one vector
//!   index per minted fn word, never serialized), a native key keeps its name for issue-time
//!   resolution through the native tables. Calls re-resolve through the target's current slot at
//!   issue time, which is what the two-slot retirement semantics require.
//! - Strings. A `Str` entry is a genuine string constant; KSTR loads it as an interned heap string
//!   reference (the pool-kind check admits it for KSTR only).
//! - Natives. A payload FuncRef may name a reserved native module: the `builtin` core (every VM
//!   installs it, so its primitives resolve like any installed dependency) or the `std` platform
//!   module (host-installed — a run over world natives requires the platform table in place before
//!   the module's install, the dependencies-first rule).
//!
//! The deps-first load protocol over this install — a recursive walk
//! installing a root's whole dependency closure through a caller-supplied
//! provider, validating every link's content hash — lives in
//! [`crate::load`].
//!
//! - Entry descriptors. The blocks payload stores a two-word zero placeholder per function; the
//!   in-memory `ds_mod::Function` record does not carry it. Word 0 (the tier-1 prologue pointer,
//!   the function's table index) is minted once by `ModuleVersion::new` when it assigns table
//!   indices, and the tier-2 word stays null until the JIT compiles the body.

use ds_mod::Hash128;

use crate::{
    module::{ModuleBuildError, ModuleRegistry, ModuleVersion, VersionIdx},
    program::{Const, ConstFuncRef, FunctionDef},
};

/// Why a dsmod module cannot be installed.
#[derive(Clone, Debug, PartialEq)]
pub enum LoadError {
    /// A FuncRef names no function: a same-module index past the module's
    /// own table, or a foreign module that is not installed (dependencies
    /// install first) or whose current version has no such index.
    UnresolvedCallee {
        /// The function whose pool holds the reference.
        func: String,
        /// The callee's module name.
        module: String,
        /// The callee's function index.
        index: u32,
    },
    /// A world call site addresses an op the installed platform does not
    /// serve: the platform declaration the call sites were lowered
    /// against names a platform whose provides list does not cover the
    /// op's capability.
    UnprovidedPlatformOp {
        /// The function whose pool holds the reference.
        func: String,
        /// The installed platform's name.
        platform: String,
        /// The op's world capability.
        capability: String,
        /// The op's index in the platform's fixed table.
        index: u32,
    },
    /// The deps-first load walk met a dependency cycle: `cycle` names the
    /// modules from the repeated dependency around to the module whose
    /// dep list closes it, the repeated name first and last.
    DepCycle {
        /// The cycle's module names, the repeated name first and last.
        cycle: Vec<String>,
    },
    /// The dep provider has no module for a dependency link (or answered
    /// with a module under another name, which is the same miss).
    ProviderMiss {
        /// The dependency's name.
        dep: String,
        /// The module whose dep list names it.
        dependent: String,
    },
    /// A dependency link's content hash disagrees with the content the
    /// name resolved to. `found` is the content the name actually holds:
    /// the provider-fetched module's source hash, the hash another entry
    /// of the same dep list already linked, or the installed slot's
    /// recorded hash — `None` when the installed version records no hash
    /// at all, which cannot validate a link and refuses the load.
    HashMismatch {
        /// The dependency's name.
        dep: String,
        /// The module whose dep list names it.
        dependent: String,
        /// The hash the dependent linked the dependency at.
        linked: Hash128,
        /// The hash the name actually holds, or no recorded hash.
        found: Option<Hash128>,
    },
    /// The swap's staged-word re-check refused: a staged callable word
    /// keys on a function-table index whose function identity (name plus
    /// frame layout, extended by the content identity — body plus capture
    /// set — a moved index-embedded name judges by) the incoming table
    /// drops, so installing would silently misroute the word into
    /// whatever function now holds the index. The refusal keeps the
    /// installed version running — a refused swap over a silent
    /// misroute.
    StagedFnMisroute {
        /// The swapped module's name.
        module: String,
        /// The function-table index the staged word carries.
        index: u32,
        /// The function the index named in the retiring table, or `None`
        /// when the index already escaped that table (the word's identity
        /// is unknowable, so no re-key can be proven).
        func: Option<String>,
        /// Where the staged word waited.
        site: StagedSite,
    },
    /// The swap's direct-call re-check refused: a live code row's
    /// CALL/CALLT/CALLG/CALLTG site names the swapped module through
    /// its constant pool by an index whose named identity the incoming
    /// table moves or drops — a moved index-embedded name passing only
    /// when its content identity survives against exactly one incoming
    /// candidate, since the site's code is immutable and a retiring
    /// row's site translates at dispatch — so installing would silently
    /// route every live frame's next direct call into whatever function
    /// now holds the index. The refusal keeps the installed version
    /// running, the same contract the staged-word refusal answers.
    PoolRefMisroute {
        /// The swapped module's name.
        module: String,
        /// The module whose constant pool carries the refused call site.
        pool: String,
        /// The function-table index the site carries.
        index: u32,
        /// The identity the index held in the installed table, or `None`
        /// when the index escapes it.
        func: Option<String>,
    },
    /// The mapped version failed this VM's load checks (decode canonicity,
    /// FUNCF-vs-layout agreement, pool kinds).
    Build(ModuleBuildError),
}

/// Where a staged callable word waited across a slot swap: the staged
/// value a [`LoadError::StagedFnMisroute`] refusal names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StagedSite {
    /// A closure object's header word, on the owning fiber's heap.
    ClosureHeader {
        /// The fiber owning the closure's heap.
        fiber: u32,
    },
    /// A closure object's captured binding word.
    ClosureCapture {
        /// The fiber owning the closure's heap.
        fiber: u32,
    },
    /// A list object's element word.
    ListElement {
        /// The fiber owning the list's heap.
        fiber: u32,
    },
    /// An arena object's stored element word.
    ArenaElement {
        /// The fiber owning the arena's heap.
        fiber: u32,
    },
    /// A map object's key or value word.
    MapWord {
        /// The fiber owning the map's heap.
        fiber: u32,
    },
    /// A parked frame's slot word, in the fiber's value-stack segment.
    FrameSlot {
        /// The fiber whose segment holds the frame.
        fiber: u32,
    },
}

/// Validates a payload FuncRef and maps it to the pool entry: a
/// dep-position key resolves position → dep name → registry module id and
/// validates the index — the enclosing module (position 0) against the
/// table under construction, a dependency against the target's currently
/// installed version (bounds only: install needs to know a callee exists,
/// never a version handle) — and a native key keeps its name, validating
/// through the native tables exactly as a directly installed name would.
fn resolve_funcref(
    registry: &ModuleRegistry,
    own_module: &str,
    own_id: u32,
    local_len: usize,
    deps: &[ds_mod::Dep],
    key: &ds_mod::FuncRefKey,
    index: u32,
) -> Result<ConstFuncRef, ()> {
    match key {
        // position 0 is the enclosing module: the table under construction
        // bounds the index, and the id is the one this install will hold
        ds_mod::FuncRefKey::Dep(0) => (usize::try_from(index).is_ok_and(|index| index < local_len))
            .then_some(ConstFuncRef::Uop {
                module: own_id,
                func: index,
            })
            .ok_or(()),
        ds_mod::FuncRefKey::Dep(position) => {
            let at = usize::try_from(*position).expect("the reader bounds the position") - 1;
            let name = deps.get(at).map(|dep| dep.name.as_str()).ok_or(())?;
            // a dep entry naming the enclosing module resolves against the
            // table under construction, exactly like position 0
            if name == own_module {
                return (usize::try_from(index).is_ok_and(|index| index < local_len))
                    .then_some(ConstFuncRef::Uop {
                        module: own_id,
                        func: index,
                    })
                    .ok_or(());
            }
            // a uop dependency must be installed first; its id came with
            // the install
            if !registry.has_uop(name, index) {
                return Err(());
            }
            let module = registry.module_id(name).ok_or(())?;
            Ok(ConstFuncRef::Uop {
                module,
                func: index,
            })
        },
        // The runtime-evidence sentinel names no module: its `func` field is
        // the calling frame's slot, resolved by the CALLG arm at dispatch,
        // so validation only bounds the slot index.
        ds_mod::FuncRefKey::Native(name) if name == crate::EVIDENCE_MODULE => (index < 256)
            .then_some(ConstFuncRef::Named {
                module: name.clone(),
                func: index,
            })
            .ok_or(()),
        // the registry's native tables answer too: the builtin core is
        // installed in every VM and the platform handler by its host, so
        // natives resolve like any installed dependency — except a world op
        // the installed platform does not serve, which is the capability
        // refusal, never a resolution
        ds_mod::FuncRefKey::Native(name) => {
            if registry.platform_serves(name, index) == Some(false) {
                return Err(());
            }
            let resolved =
                registry.resolve_native(name, index).is_some() || registry.has_uop(name, index);
            if resolved {
                Ok(ConstFuncRef::Named {
                    module: name.clone(),
                    func: index,
                })
            } else {
                Err(())
            }
        },
    }
}

/// The module name a payload key addresses, for the unresolved-callee
/// diagnostic: the enclosing module's own name for position 0, the dep
/// entry's name for a position (the entry is unreachable only when the
/// reader's bound was bypassed), the native name verbatim.
fn key_name(key: &ds_mod::FuncRefKey, own_module: &str, deps: &[ds_mod::Dep]) -> String {
    match key {
        ds_mod::FuncRefKey::Dep(0) => own_module.to_owned(),
        ds_mod::FuncRefKey::Dep(position) => {
            let at = usize::try_from(*position).expect("the reader bounds the position") - 1;
            deps.get(at)
                .map_or_else(|| own_module.to_owned(), |dep| dep.name.clone())
        },
        ds_mod::FuncRefKey::Native(name) => name.clone(),
    }
}

/// Resolves one bound extern's pool constant: the artifact's bound
/// module must be installed under its manifest name and the symbol's
/// index inside the export enumeration — the return's marshaling word
/// is the format's own closed vocabulary (the absent word or a scalar
/// word), so nothing validates it further here; the refusals the
/// lowerer answers for source programs replay against foreign or
/// hand-assembled modules.
fn resolve_bound(
    registry: &ModuleRegistry,
    module: &str,
    index: u32,
    ret: ds_abi::BoundRet,
) -> Result<ConstFuncRef, ()> {
    let bound = registry.bound_table(module).ok_or(())?;
    let in_range = usize::try_from(index).is_ok_and(|at| at < bound.len());
    if !in_range {
        return Err(());
    }
    Ok(ConstFuncRef::Bound {
        module: module.to_owned(),
        index,
        ret,
    })
}

/// Maps one dsmod function record to a [`FunctionDef`]: its pool constants
/// (FuncRefs validated and normalized to their target module) and its body
/// words taken over from the consumed record, which already holds the
/// canonical stream.
fn map_function(
    registry: &ModuleRegistry,
    own_module: &str,
    own_id: u32,
    local_len: usize,
    deps: &[ds_mod::Dep],
    function: ds_mod::Function,
) -> Result<FunctionDef, LoadError> {
    let (name, layout, constants, code) = function.into_parts();
    let unresolved = |module: &str, index: u32| match registry.platform_serves(module, index) {
        // an installed platform that does not serve the op's capability
        // is the capability refusal, not an unresolved module
        Some(false) => LoadError::UnprovidedPlatformOp {
            func: name.clone(),
            platform: module.to_owned(),
            capability: crate::platform::Platform::op_capability(index)
                .map_or_else(|| "unknown".to_owned(), |cap| cap.name().to_owned()),
            index,
        },
        _ => LoadError::UnresolvedCallee {
            func: name.clone(),
            module: module.to_owned(),
            index,
        },
    };
    let mut pool = Vec::with_capacity(constants.len());
    for entry in constants {
        let mapped = match entry {
            ds_mod::Constant::I64(v) => Const::I64(v),
            ds_mod::Constant::F64(v) => Const::F64(v),
            ds_mod::Constant::Bool(v) => Const::Bool(v),
            // inert in this VM until strings become values (pool-kind
            // checked at load if any uop tries to load it)
            ds_mod::Constant::Str(raw) => Const::Str(raw),
            ds_mod::Constant::FuncRef { key, func } => Const::FuncRef(
                resolve_funcref(registry, own_module, own_id, local_len, deps, &key, func)
                    .map_err(|()| unresolved(&key_name(&key, own_module, deps), func))?,
            ),
            ds_mod::Constant::Bound { module, index, ret } => Const::FuncRef(
                resolve_bound(registry, &module, index, ret)
                    .map_err(|()| unresolved(&module, index))?,
            ),
        };
        pool.push(mapped);
    }
    Ok(FunctionDef {
        name,
        layout,
        pool,
        code,
    })
}

/// Installs a dsmod module into the registry's two-slot table: maps every
/// function record, fills the entry-descriptor word-0 placeholders at
/// install, and answers the installed version's table row — the row's
/// `Arc` is the version's single home. A re-install of the same module
/// name swaps the slots.
///
/// The module is consumed: each record's body words reach the installed
/// table without a copy, and a failed install drops the record (rebuild
/// the module before retrying).
///
/// # Errors
///
/// Returns [`LoadError`] for FuncRefs naming no function and for bodies
/// failing this VM's load checks.
pub fn install(
    registry: &mut ModuleRegistry,
    module: ds_mod::Module,
) -> Result<VersionIdx, LoadError> {
    let version = build(registry, module)?;
    Ok(registry.install(version))
}

/// Builds a dsmod module into a version without touching the registry's
/// slots: the mapping and validation [`install`] performs, answering the
/// built version so a host can run a swap's re-checks against the incoming
/// table before the install lands (the re-check reads the built records'
/// identities, the same spellings the installed table would carry).
///
/// The registry is read, never written: a failed build leaves every slot
/// untouched, exactly like a refused install.
///
/// # Errors
///
/// Returns [`LoadError`] for FuncRefs naming no function and for bodies
/// failing this VM's load checks.
pub fn build(
    registry: &ModuleRegistry,
    module: ds_mod::Module,
) -> Result<ModuleVersion, LoadError> {
    let name = module.name().to_owned();
    let deps = module.deps().to_vec();
    // the version's content identity: a module's content hash is its
    // source hash, stamped so the load protocol can validate
    // later links against the installed slot
    let content_hash = module.source_hash();
    // the enclosing module's own id: an installed name's own, or the fresh
    // tail id this first install assigns (ids never move, so a
    // pre-install resolve and the install itself agree)
    let own_id = registry.id_for_install(&name);
    let chains = module.chains().to_vec();
    let spans = module.pc_spans().cloned();
    let functions = module.into_functions();
    let local_len = functions.len();
    // the function table's length is known and a `Result` collect gets no
    // size hint from its iterator, so the table reserves its exact length
    let mut function_defs = Vec::with_capacity(local_len);
    for function in functions {
        function_defs.push(map_function(
            registry, &name, own_id, local_len, &deps, function,
        )?);
    }
    Ok(ModuleVersion::new(name, function_defs)
        .map_err(LoadError::Build)?
        .with_chains(chains)
        .map_err(LoadError::Build)?
        .with_content_hash(content_hash)
        .with_spans(spans))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The enclosing module (position 0) validates against the local
    /// table and takes the id this install will hold; a dep entry and a
    /// native name resolve through the registry.
    #[test]
    fn funcref_validation_resolves_keys_at_install() {
        let registry = ModuleRegistry::new();
        let no_deps: &[ds_mod::Dep] = &[];
        assert_eq!(
            resolve_funcref(
                &registry,
                "m",
                7,
                2,
                no_deps,
                &ds_mod::FuncRefKey::Dep(0),
                1
            ),
            Ok(ConstFuncRef::Uop { module: 7, func: 1 })
        );
        assert_eq!(
            resolve_funcref(
                &registry,
                "m",
                7,
                2,
                no_deps,
                &ds_mod::FuncRefKey::Dep(0),
                2
            ),
            Err(())
        );
        // a dep entry resolves to its name; nothing is installed
        let deps = [ds_mod::Dep {
            name: "other".to_owned(),
            content_hash: Hash128::ZERO,
        }];
        assert_eq!(
            resolve_funcref(&registry, "m", 7, 2, &deps, &ds_mod::FuncRefKey::Dep(1), 0),
            Err(())
        );
        // the sentinel bounds its slot index only
        assert_eq!(
            resolve_funcref(
                &registry,
                "m",
                7,
                2,
                &deps,
                &ds_mod::FuncRefKey::Native(crate::EVIDENCE_MODULE.to_owned()),
                9
            ),
            Ok(ConstFuncRef::Named {
                module: "$evidence".to_owned(),
                func: 9
            })
        );
        assert_eq!(
            resolve_funcref(
                &registry,
                "m",
                7,
                2,
                &deps,
                &ds_mod::FuncRefKey::Native(crate::EVIDENCE_MODULE.to_owned()),
                256
            ),
            Err(())
        );
        // a native name with no table installed resolves to nothing
        assert_eq!(
            resolve_funcref(
                &registry,
                "m",
                7,
                2,
                &deps,
                &ds_mod::FuncRefKey::Native("other".to_owned()),
                0
            ),
            Err(())
        );
    }
}
