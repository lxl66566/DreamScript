//! The platform seam's checker half (H5, E1): the `platform "..."`
//! declaration a package is checked against.
//!
//! A platform declaration names a platform and the world capabilities it
//! provides — `platform "std" provides <console, fsys, net, time>`, an
//! absent provides list providing nothing (the embed target). The check
//! is the record's subset rule: the package's effect usage must be a
//! subset of the platform's declared row. The row vocabulary carries one
//! world effect, so the row check reduces to its finer grain — a
//! definition's world usage names the capabilities whose functions it
//! calls ([`crate::effects`] attributes them beside the row), and every
//! attributed capability must be provided. An unattributed world touch
//! (an extern whose declared row carries `world` without a capability
//! entry — the FFI seam) licenses on any world-bearing platform: the
//! check may over-reject, never silently accept.
//!
//! World usage with no platform declaration at all refuses: the
//! declaration is what a package is checked against, and the stdlib
//! ambient carries one (`std`, every capability), so a package including
//! the ambient is checked against the default platform the `ds` CLI
//! provides. A package is checked alone (`--no-ambient`) without one.

use std::borrow::Borrow;

use ds_diag::{Diagnostic, ErrorCode};
use ds_parse::{Ast, ItemKind, PlatformDecl};

use crate::effects::{Effect, Effects};

/// The world capability vocabulary (H5's provides list), in bit order.
pub const CAPABILITIES: [&str; 4] = ["console", "fsys", "net", "time"];

/// One capability's bit in a [`Caps`] set.
#[must_use]
pub fn capability_bit(name: &str) -> Option<u8> {
    CAPABILITIES
        .iter()
        .position(|cap| *cap == name)
        .map(|index| 1 << index)
}

/// The names in a capability set, in vocabulary order.
#[must_use]
pub fn capability_names(caps: u8) -> Vec<&'static str> {
    CAPABILITIES
        .iter()
        .enumerate()
        .filter(|(index, _)| caps & (1 << index) != 0)
        .map(|(_, name)| *name)
        .collect()
}

/// The ambient world functions' capability attribution: the name-keyed
/// table the effects pass reads beside [`crate::DeclaredRows`] — each
/// entry names the capability whose surface the function belongs to. The
/// stdlib ambient owns the real table; this type is its carrier.
#[derive(Clone, Debug, Default)]
pub struct WorldCaps {
    caps: std::collections::HashMap<String, u8>,
}

impl WorldCaps {
    /// No attributions: every world-touching function is unattributed.
    #[must_use]
    pub fn new() -> Self {
        Self {
            caps: std::collections::HashMap::new(),
        }
    }

    /// Builder: attributes `name` to the capability `cap` names.
    #[must_use]
    pub fn with(mut self, name: &str, cap: &str) -> Self {
        if let Some(bit) = capability_bit(cap) {
            self.caps.insert(name.to_owned(), bit);
        }
        self
    }

    /// `name`'s capability bits, zero when unattributed.
    #[must_use]
    pub fn bit(&self, name: &str) -> u8 {
        self.caps.get(name).copied().unwrap_or(0)
    }
}

/// Verifies the package's effect usage against its platform declaration
/// (H5): every capability a definition's world usage needs must be
/// provided, world usage with no declaration refuses, a second
/// declaration refuses, and an unknown capability name refuses. Pure
/// over the computed rows — no diagnostics are consumed, only emitted.
#[must_use]
pub fn check_platform<T>(asts: &[T], effects: &Effects) -> Vec<Diagnostic>
where
    T: Borrow<Ast>,
{
    let mut diags = Vec::new();
    let mut declared: Option<&PlatformDecl> = None;
    let mut caps: Option<(u8, &PlatformDecl)> = None;
    for ast in asts {
        for block in &ast.borrow().blocks {
            for item in &block.items {
                let ItemKind::Platform(platform) = &item.kind else {
                    continue;
                };
                if let Some(first) = declared {
                    diags.push(
                        Diagnostic::error(
                            ErrorCode::PlatformDuplicate,
                            format!(
                                "a second platform declaration `{}`: the package is checked \
                                 against one platform",
                                platform.name.name,
                            ),
                        )
                        .with_primary(platform.name.span, "declared here")
                        .with_secondary(first.name.span, "the first declaration"),
                    );
                    continue;
                }
                declared = Some(platform);
                let mut mask = 0u8;
                for cap in &platform.provides {
                    match capability_bit(&cap.name) {
                        Some(bit) => mask |= bit,
                        None => diags.push(
                            Diagnostic::error(
                                ErrorCode::PlatformUnknownCapability,
                                format!(
                                    "`{}` is not a world capability (one of {})",
                                    cap.name,
                                    CAPABILITIES.join(", ")
                                ),
                            )
                            .with_primary(cap.span, "not a capability"),
                        ),
                    }
                }
                caps = Some((mask, platform));
            }
        }
    }
    for def in effects.defs() {
        let unattributed = def.row.contains(Effect::World) && def.caps == 0;
        let Some((mask, platform)) = caps else {
            if def.row.contains(Effect::World) {
                diags.push(
                    Diagnostic::error(
                        ErrorCode::PlatformMissing,
                        format!(
                            "`{}` touches the world but the package declares no platform",
                            def.name
                        ),
                    )
                    .with_primary(def.span, "world usage with no platform declaration"),
                );
            }
            continue;
        };
        let unmet = def.caps & !mask;
        if unmet != 0 {
            let needed = capability_names(unmet);
            diags.push(
                Diagnostic::error(
                    ErrorCode::PlatformNotProvided,
                    format!(
                        "`{}` needs the {} the platform `{}` does not provide",
                        def.name,
                        needed.join(", "),
                        platform.name.name,
                    ),
                )
                .with_primary(def.span, "uses an unprovided capability")
                .with_secondary(platform.name.span, "the platform declaration"),
            );
        } else if unattributed && mask == 0 {
            diags.push(
                Diagnostic::error(
                    ErrorCode::PlatformNotProvided,
                    format!(
                        "`{}` touches the world but the platform `{}` provides no world surface",
                        def.name, platform.name.name,
                    ),
                )
                .with_primary(def.span, "unattributed world usage")
                .with_secondary(platform.name.span, "the platform declaration"),
            );
        }
    }
    diags
}
