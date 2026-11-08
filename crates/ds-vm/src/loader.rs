//! The dsmod loader: install a `ds-mod` module into the two-slot registry
//! and execute it [BL-7, H6 item 5].
//!
//! Mapping decisions:
//!
//! - Pools. The dsmod blocks payload carries one constant pool per function ("uop ConstIdx operands
//!   index it"), and this VM's [`FunctionDef`] matches that granularity. ds-abi's uop module doc
//!   says "one pool per module [BL-7]" — the two locked contracts disagree, and the executable
//!   model follows the payload that files actually carry.
//! - Callees. ds-mod's `Constant` has no function-reference kind (i64/f64/bool/str only), so a
//!   qualified callee rides a `Str` entry spelled `"module.function"`. The loader resolves the name
//!   at install time into the pool [`Const::FuncRef`] the uop contract expects: a same-module name
//!   resolves against this module's own table order, a foreign name against the target's currently
//!   installed version (so cross-module references require dependencies to be installed first). The
//!   resolved FuncRef carries the target module's name, and calls re-resolve through the target's
//!   current slot at issue time, which is what the two-slot retirement semantics require [H6 item
//!   5]. A dedicated FuncRef payload kind would remove the string convention; reported as ds-mod
//!   friction.
//! - Entry descriptors. The blocks payload stores a two-word zero placeholder per function; the
//!   in-memory `ds_mod::Function` record does not carry it, and the fill happens here: install
//!   writes word 0 (the tier-1 prologue pointer, the function's table index by the M3 convention)
//!   once, exactly as the two-word contract requires [H2].

use std::sync::Arc;

use crate::{
    module::{ModuleBuildError, ModuleRegistry, ModuleVersion},
    program::{Const, FunctionDef, assemble},
};

/// Why a dsmod module cannot be installed.
#[derive(Clone, Debug, PartialEq)]
pub enum LoadError {
    /// A `Str` pool entry used as a callee is not a qualified
    /// `"module.function"` path.
    BadFuncRef {
        /// The function whose pool holds the entry.
        func: String,
        /// The raw string.
        raw: String,
    },
    /// A qualified callee resolved to no function: not in this module, and
    /// the target module is not installed (dependencies install first).
    Unresolved {
        /// The function whose pool holds the reference.
        func: String,
        /// The qualified path.
        path: String,
    },
    /// The mapped version failed this VM's load checks (decode canonicity,
    /// FUNCF-vs-layout agreement, pool kinds).
    Build(ModuleBuildError),
}

/// Resolves a qualified `"module.function"` path to a (target module,
/// table index) pair: same-module names against `local_names` (the module's
/// table order), foreign names against the registry's current slot for that
/// module.
fn resolve_path(
    registry: &ModuleRegistry,
    own_module: &str,
    local_names: &[&str],
    path: &str,
) -> Option<(String, u32)> {
    let (module, func) = path.split_once('.')?;
    if module == own_module {
        let index = local_names.iter().position(|&n| n == func)?;
        return Some((
            own_module.to_owned(),
            u32::try_from(index).expect("function table exceeds u32"),
        ));
    }
    registry
        .resolve(module, func)
        .map(|(_, func_id)| (module.to_owned(), func_id))
}

/// Maps one dsmod function record to a [`FunctionDef`]: its pool constants
/// (with `Str` callees resolved into [`Const::FuncRef`]) and its body
/// re-encoded to the canonical word stream.
fn map_function(
    registry: &ModuleRegistry,
    own_module: &str,
    local_names: &[&str],
    function: &ds_mod::Function,
) -> Result<FunctionDef, LoadError> {
    let mut pool = Vec::with_capacity(function.constants().len());
    for entry in function.constants() {
        let mapped = match entry {
            ds_mod::Constant::I64(v) => Const::I64(*v),
            ds_mod::Constant::F64(v) => Const::F64(*v),
            ds_mod::Constant::Bool(v) => Const::Bool(*v),
            ds_mod::Constant::Str(raw) => {
                let (module, func) = resolve_path(registry, own_module, local_names, raw)
                    .ok_or_else(|| {
                        if raw.contains('.') {
                            LoadError::Unresolved {
                                func: function.name().to_owned(),
                                path: raw.clone(),
                            }
                        } else {
                            LoadError::BadFuncRef {
                                func: function.name().to_owned(),
                                raw: raw.clone(),
                            }
                        }
                    })?;
                Const::FuncRef { module, func }
            },
        };
        pool.push(mapped);
    }
    Ok(FunctionDef {
        name: function.name().to_owned(),
        layout: function.layout().clone(),
        pool,
        // ds-mod validated every uop's encodability when the record was
        // built, so re-encoding cannot fail
        code: assemble(function.code())
            .expect("ds-mod validated every word at build and parse time"),
    })
}

/// Installs a dsmod module into the registry's two-slot table: maps every
/// function record, fills the entry-descriptor word-0 placeholders at
/// install, and returns the shared handle to the installed version. A
/// re-install of the same module name swaps the slots [H6 item 5].
///
/// # Errors
///
/// Returns [`LoadError`] for unqualified or unresolved callee strings and
/// for bodies failing this VM's load checks.
pub fn install(
    registry: &mut ModuleRegistry,
    module: &ds_mod::Module,
) -> Result<Arc<ModuleVersion>, LoadError> {
    let local_names: Vec<&str> = module
        .functions()
        .iter()
        .map(ds_mod::Function::name)
        .collect();
    let defs = module
        .functions()
        .iter()
        .map(|f| map_function(registry, module.name(), &local_names, f))
        .collect::<Result<Vec<_>, _>>()?;
    let version = ModuleVersion::new(module.name(), defs).map_err(LoadError::Build)?;
    Ok(registry.install(version))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_resolution_splits_on_the_first_dot() {
        let registry = ModuleRegistry::new();
        assert_eq!(
            resolve_path(&registry, "m", &["a", "b"], "m.b"),
            Some(("m".to_owned(), 1))
        );
        assert_eq!(resolve_path(&registry, "m", &["a"], "m.zz"), None);
        assert_eq!(resolve_path(&registry, "m", &["a"], "n.a"), None);
        assert_eq!(resolve_path(&registry, "m", &["a"], "no_dots"), None);
    }
}
