//! The deps-first load protocol:
//! one walk installs a root module and every uop dependency its dep list
//! names, through a caller-supplied provider.
//!
//! - Order. Dependencies install before their dependent, and each dep list is walked depth-first in
//!   stored order, so first-install module ids assign in that deterministic order (ids never
//!   serialize and stay stable across hot-reload swaps). Links resolve lazily, one per dep entry in
//!   list order: by the time a later entry's link runs, every earlier sibling's subtree has fully
//!   installed, so a name that subtree already installed validates against its slot — fetched once,
//!   installed once — and resolution keeps reading the current slot, so a later swap of an
//!   installed dep composes with loaded dependents.
//! - Validation. A provider-fetched module whose source hash differs from the linked content hash
//!   refuses the load; one dep list linking one name at two hashes refuses (same-name multi-hash
//!   coexistence is refused). A dep entry naming the enclosing module resolves against the table
//!   under construction (the install's own-table path) and is never fetched.
//! - Failure. A provider miss, a dep cycle, a hash disagreement, or any install refusal fails the
//!   load. Every dependency that finished installing stays installed — its id stays assigned, the
//!   deterministic prefix of the walk order — and the module whose install failed is absent from
//!   the registry. The walk itself never uninstalls, so a retried load revalidates the installed
//!   deps by their recorded hashes instead of fetching them again.

use ds_mod::{Hash128, Module};

use crate::{
    loader::{LoadError, build},
    module::{ModuleRegistry, ModuleVersion, VersionIdx},
};

/// One pending step of the walk's explicit stack (no Rust-stack
/// recursion: a long dep chain grows this heap vector, not the call
/// stack).
enum Step {
    /// Queue the module's own install and its dep list's links: every dep
    /// installs before this module's own install.
    Enter(Module),
    /// Resolve one dep entry of `dependent`. The cycle check, the
    /// installed-slot validation, and the provider fetch all run when
    /// this step pops — after every earlier sibling's subtree has fully
    /// installed, so `path` is exactly the ancestor chain and a name
    /// reached twice resolves through its installed slot the second
    /// time.
    Link { dep: ds_mod::Dep, dependent: String },
    /// Install the module; `root` marks the walk's seed, whose built
    /// version [`load_built`] answers without installing (the host's swap
    /// re-checks read the built table before the install lands).
    Install { module: Module, root: bool },
}

/// Loads a dsmod module with its whole dependency closure into the
/// registry: a deps-first walk over the root's dep list, fetching each
/// uninstalled dependency from `provider` (answered per `(name, linked
/// content hash)`, `None` for a miss) and installing dependencies before
/// their dependent, so first-install ids assign in the deterministic
/// dep-list depth-first order. The walk builds the root last and answers
/// its built version uninstalled; a root name already installed swaps its
/// slot (hot reload), ids unchanged.
///
/// The walk never fetches a name twice: each dep entry's link resolves
/// lazily in list order (so every earlier sibling's subtree has fully
/// installed by then), a dep already installed validates through its current slot's
/// recorded content hash, and a dep entry naming the enclosing module
/// stays local to that module's own install. A provider-fetched module
/// must carry the linked name and source hash. An installed version
/// recording no content hash cannot validate a link and refuses the load
/// — a link names content, and a slot with no recorded identity cannot
/// confirm it serves those bytes; directly built host versions record
/// none, so a host module joined to hash-validated links installs
/// through the dsmod path (or records its hash) to participate.
///
/// # Errors
///
/// Returns [`LoadError::DepCycle`] when a dep entry names a module still
/// under construction, [`LoadError::ProviderMiss`] when the provider has
/// no module for a link, [`LoadError::HashMismatch`] when a link's hash
/// disagrees with the content the name resolved to, and the build
/// refusals of [`crate::loader::build`] verbatim.
pub fn load_built(
    registry: &mut ModuleRegistry,
    root: Module,
    mut provider: impl FnMut(&str, Hash128) -> Option<Module>,
) -> Result<ModuleVersion, LoadError> {
    let mut root_version = None;
    // names of modules under construction (entered, not yet installed):
    // both the cycle detector and the root marker read it
    let mut path: Vec<String> = Vec::new();
    let mut stack = vec![Step::Enter(root)];
    while let Some(step) = stack.pop() {
        match step {
            Step::Enter(module) => {
                let name = module.name().to_owned();
                // the walk's seed is the only module entered at depth 1
                let root = path.is_empty();
                let deps = module.deps().to_vec();
                path.push(name.clone());
                stack.push(Step::Install { module, root });
                queue_links(&mut stack, &name, &deps)?;
            },
            Step::Link { dep, dependent } => {
                // every earlier sibling's subtree has fully installed by
                // now, so `path` is exactly the ancestor chain: a name on
                // it closes a cycle the walk would never exit
                if let Some(at) = path.iter().position(|ancestor| ancestor == &dep.name) {
                    let mut cycle: Vec<String> = path[at..].to_vec();
                    cycle.push(dep.name.clone());
                    return Err(LoadError::DepCycle { cycle });
                }
                // a name an earlier subtree installed (or a host install
                // from before the walk) validates through its current
                // slot's recorded hash and is never re-fetched; no
                // recorded hash cannot validate a link, and refuses
                if let Some(version) = registry.current_version(&dep.name) {
                    match version.content_hash() {
                        Some(recorded) if recorded == dep.content_hash => continue,
                        found => {
                            return Err(LoadError::HashMismatch {
                                dep: dep.name.clone(),
                                dependent,
                                linked: dep.content_hash,
                                found,
                            })
                        },
                    }
                }
                // the provider answers under the linked name with the
                // linked content: a module under another name is the same
                // miss, and a source-hash disagreement refuses the load
                match provider(&dep.name, dep.content_hash) {
                    Some(fetched) if fetched.name() == dep.name => {
                        if fetched.source_hash() != dep.content_hash {
                            return Err(LoadError::HashMismatch {
                                dep: dep.name.clone(),
                                dependent,
                                linked: dep.content_hash,
                                found: Some(fetched.source_hash()),
                            });
                        }
                        stack.push(Step::Enter(fetched));
                    },
                    _ => {
                        return Err(LoadError::ProviderMiss {
                            dep: dep.name.clone(),
                            dependent,
                        })
                    },
                }
            },
            Step::Install { module, root } => {
                path.pop();
                // the walk's seed is only built: the host installs it after
                // the swap re-checks read the built table; every dependency
                // is already current and swaps no slot
                let version = build(registry, module)?;
                if root {
                    root_version = Some(version);
                } else {
                    registry.install(version);
                }
            },
        }
    }
    Ok(root_version.expect("the walk always builds its seed last"))
}

/// Loads a dsmod module with its whole dependency closure and installs
/// the root's built version: the walk of [`load_built`] plus the root's
/// slot swap, answering the installed row. The host that needs the swap
/// re-checks against the incoming table calls [`load_built`] and installs
/// the answer itself.
///
/// # Errors
///
/// The refusals of [`load_built`] verbatim.
pub fn load(
    registry: &mut ModuleRegistry,
    root: Module,
    provider: impl FnMut(&str, Hash128) -> Option<Module>,
) -> Result<VersionIdx, LoadError> {
    let version = load_built(registry, root, provider)?;
    Ok(registry.install(version))
}

/// Queues one link step per dep entry, in reverse so the stack pops them
/// in dep-list order. The eager half is only the list's own consistency —
/// self-named entries stay local (the enclosing module's own bytes are
/// the link target, so there is nothing to fetch or compare), a name
/// twice at one hash links once, and one name at two hashes refuses —
/// which touches neither the registry nor the provider; every check that
/// does runs lazily when the link pops.
fn queue_links(stack: &mut Vec<Step>, name: &str, deps: &[ds_mod::Dep]) -> Result<(), LoadError> {
    let mut queued: Vec<&ds_mod::Dep> = Vec::new();
    for dep in deps {
        if dep.name == name {
            continue;
        }
        if let Some(at) = queued.iter().position(|other| other.name == dep.name) {
            if queued[at].content_hash != dep.content_hash {
                return Err(LoadError::HashMismatch {
                    dep: dep.name.clone(),
                    dependent: name.to_owned(),
                    linked: dep.content_hash,
                    found: Some(queued[at].content_hash),
                });
            }
            continue;
        }
        queued.push(dep);
    }
    for dep in queued.iter().rev() {
        stack.push(Step::Link {
            dep: (*dep).clone(),
            dependent: name.to_owned(),
        });
    }
    Ok(())
}
