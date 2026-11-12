//! The dsmod loader: install a `ds-mod` module into the two-slot registry
//! and execute it [BL-7, H6 item 5].
//!
//! Mapping decisions:
//!
//! - Pools. The dsmod blocks payload carries one constant pool per function ("uop ConstIdx operands
//!   index it"), and this VM's [`FunctionDef`] matches that granularity. ds-abi's uop module doc
//!   says "one pool per module [BL-7]" — the two locked contracts disagree, and the executable
//!   model follows the payload that files actually carry.
//! - Callees. A payload [`ds_mod::Constant::FuncRef`] names the callee's module (empty, or the
//!   enclosing module's own name, means same-module) and its function index in that module's blocks
//!   payload. The loader validates it at install time — a same-module index against this module's
//!   own table, a foreign one against the target's currently installed version (so cross-module
//!   references require dependencies to be installed first) — and stores it as the pool
//!   [`Const::FuncRef`] the uop contract expects. Calls re-resolve through the target's current
//!   slot at issue time, which is what the two-slot retirement semantics require [H6 item 5].
//! - Strings. A `Str` entry is a genuine string constant; KSTR loads it as an interned heap string
//!   reference (the pool-kind check admits it for KSTR only).
//! - Natives. A payload FuncRef may name a reserved native module: the `builtin` core (every VM
//!   installs it, so its primitives resolve like any installed dependency) or the `std` platform
//!   module (host-installed — a run over world natives requires the platform table in place before
//!   the module's install, the dependencies-first rule).
//! - Entry descriptors. The blocks payload stores a two-word zero placeholder per function; the
//!   in-memory `ds_mod::Function` record does not carry it, and the fill happens here: install
//!   writes word 0 (the tier-1 prologue pointer, the function's table index by the M3 convention)
//!   once, exactly as the two-word contract requires [H2].

use std::sync::Arc;

use crate::{
    module::{ModuleBuildError, ModuleRegistry, ModuleVersion},
    program::{Const, FunctionDef},
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
    /// The mapped version failed this VM's load checks (decode canonicity,
    /// FUNCF-vs-layout agreement, pool kinds).
    Build(ModuleBuildError),
}

/// Validates a payload FuncRef and maps it to the pool kind: an empty module
/// or the enclosing module's own name is a same-module reference (validated
/// against the table under construction); anything else resolves through the
/// target module's currently installed version.
fn resolve_funcref(
    registry: &ModuleRegistry,
    own_module: &str,
    local_len: usize,
    module: &str,
    index: u32,
) -> Result<(String, u32), ()> {
    // The runtime-evidence sentinel names no module: its `func` field is
    // the calling frame's slot, resolved by the CALLG arm at dispatch,
    // so validation only bounds the slot index.
    if module == crate::EVIDENCE_MODULE {
        return (index < 256)
            .then_some((module.to_owned(), index))
            .ok_or(());
    }
    if module.is_empty() || module == own_module {
        if (index as usize) < local_len {
            return Ok((own_module.to_owned(), index));
        }
        return Err(());
    }
    // the registry's native tables answer too: the builtin core is
    // installed in every VM and the platform handler by its host, so
    // natives resolve like any installed dependency — except a world op
    // the installed platform does not serve, which is the capability
    // refusal, never a resolution
    // bounds only: install needs to know a callee exists, never a version
    // handle
    let resolved =
        registry.resolve_native(module, index).is_some() || registry.has_uop(module, index);
    if registry.platform_serves(module, index) == Some(false) {
        return Err(());
    }
    if resolved { Ok((module.to_owned(), index)) } else { Err(()) }
}

/// Maps one dsmod function record to a [`FunctionDef`]: its pool constants
/// (FuncRefs validated and normalized to their target module) and its body
/// words taken over from the consumed record, which already holds the
/// canonical stream.
fn map_function(
    registry: &ModuleRegistry,
    own_module: &str,
    local_len: usize,
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
            ds_mod::Constant::FuncRef { module, func } => {
                let (module, index) =
                    resolve_funcref(registry, own_module, local_len, &module, func)
                        .map_err(|()| unresolved(&module, func))?;
                Const::FuncRef {
                    module,
                    func: index,
                }
            },
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
/// install, and returns the shared handle to the installed version. A
/// re-install of the same module name swaps the slots [H6 item 5].
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
) -> Result<Arc<ModuleVersion>, LoadError> {
    let name = module.name().to_owned();
    let functions = module.into_functions();
    let local_len = functions.len();
    // the function table's length is known and a `Result` collect gets no
    // size hint from its iterator, so the table reserves its exact length
    let mut defs = Vec::with_capacity(local_len);
    for function in functions {
        defs.push(map_function(registry, &name, local_len, function)?);
    }
    let version = ModuleVersion::new(name, defs).map_err(LoadError::Build)?;
    Ok(registry.install(version))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn funcref_validation_accepts_same_module_and_installed_foreign() {
        let registry = ModuleRegistry::new();
        // same-module: empty name and own name both mean local
        assert_eq!(
            resolve_funcref(&registry, "m", 2, "", 1),
            Ok(("m".to_owned(), 1))
        );
        assert_eq!(
            resolve_funcref(&registry, "m", 2, "m", 0),
            Ok(("m".to_owned(), 0))
        );
        assert_eq!(resolve_funcref(&registry, "m", 2, "", 2), Err(()));
        // foreign: nothing is installed
        assert_eq!(resolve_funcref(&registry, "m", 2, "other", 0), Err(()));
    }
}
