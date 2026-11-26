//! The natives the lowering routes, and the ambient spelling their call sites carry.
//!
//! The rows themselves live in the call ABI (`ds_abi::native`): the names, reserved indices,
//! parameter word kinds, staging disciplines, and result shapes the VM installs from. This module
//! re-exports them and adds the one thing a row cannot say, because it is a fact of the source
//! surface rather than of the call: the failure classes' ambient spelling — the member name each
//! class tag lands on, and the prefix an unhandled failure prints.

pub(crate) use ds_abi::native::{
    BUILTIN_MODULE as BUILTIN_MODULE_NAME, COMPTIME_MODULE as COMPTIME_MODULE_NAME,
    EVIDENCE_MODULE, FailureClasses, GENERIC_NATIVES, GenericNative, NURSERY_BOUNDARY_ERR, Native,
    NativeModule, NativeShape, PLATFORM_IO_OK, PLATFORM_UTF8_ERR as UTF8_ERR,
    STD_PLATFORM_NAME as DEFAULT_PLATFORM_NAME, lookup, lookup_generic,
};

/// The ambient spelling of one failure family: the unhandled-failure line's prefix, and one
/// discriminant per class paired with the ambient member name it lands on.
pub(crate) struct FailureSpelling {
    /// The prefix the unhandled-failure line prints, e.g. `io error: `.
    pub err_prefix: &'static str,
    /// The classes the span answers, each with the member name a matching union carries.
    pub classes: &'static [(u8, &'static str)],
}

/// The io classes: the `io_err` alias's member set, `io_other` the floor.
const IO_CLASSES: &[(u8, &str)] = &[
    (ds_abi::native::PLATFORM_IO_NOT_FOUND, "not_found"),
    (
        ds_abi::native::PLATFORM_IO_PERMISSION_DENIED,
        "permission_denied",
    ),
    (ds_abi::native::PLATFORM_IO_ALREADY_EXISTS, "already_exists"),
    (ds_abi::native::PLATFORM_IO_TIMED_OUT, "timed_out"),
    (ds_abi::native::PLATFORM_IO_UNEXPECTED_EOF, "unexpected_eof"),
    (ds_abi::native::PLATFORM_IO_OTHER, "io_other"),
];
/// The net classes: the `net_err` alias's member set, `net_other` the floor.
const NET_CLASSES: &[(u8, &str)] = &[
    (ds_abi::native::PLATFORM_NET_TIMEOUT, "timeout"),
    (ds_abi::native::PLATFORM_NET_REFUSED, "refused"),
    (ds_abi::native::PLATFORM_NET_ADDR_IN_USE, "addr_in_use"),
    (ds_abi::native::PLATFORM_NET_RESET, "reset"),
    (ds_abi::native::PLATFORM_NET_OTHER, "net_other"),
];
/// The parse classes: `parse_err` alone, its member carrying the position payload.
const PARSE_CLASSES: &[(u8, &str)] = &[(ds_abi::native::PLATFORM_PARSE_ERR, "parse_err")];
/// The utf8 classes: the one bare tag.
const UTF8_CLASSES: &[(u8, &str)] = &[(ds_abi::native::PLATFORM_UTF8_ERR, "utf8_err")];
/// The deadline alone: the io taxonomy's `timed_out` member, which io-handling callers absorb by
/// set containment.
const TIMED_OUT_CLASSES: &[(u8, &str)] = &[(ds_abi::native::PLATFORM_IO_TIMED_OUT, "timed_out")];

/// The ambient spelling of the family a fallible row answers. A [`FailureClasses::TimedOut`] span
/// always re-spans into the call's own union, so its prefix never prints.
pub(crate) const fn failure_spelling(family: FailureClasses) -> FailureSpelling {
    match family {
        FailureClasses::Io => FailureSpelling {
            err_prefix: "io error: ",
            classes: IO_CLASSES,
        },
        FailureClasses::Net => FailureSpelling {
            err_prefix: "net error: ",
            classes: NET_CLASSES,
        },
        FailureClasses::Parse => FailureSpelling {
            err_prefix: "parse error: at position ",
            classes: PARSE_CLASSES,
        },
        FailureClasses::Utf8 => FailureSpelling {
            err_prefix: "utf8 error: ",
            classes: UTF8_CLASSES,
        },
        FailureClasses::TimedOut => FailureSpelling {
            err_prefix: "io error: ",
            classes: TIMED_OUT_CLASSES,
        },
    }
}

pub(crate) use ds_abi::native::PLATFORM_NONE as NONE_TAG;

#[cfg(test)]
mod tests {
    use super::*;

    /// The comptime reflection module's name is the evaluator's own table key: the one module name
    /// a surface spells for itself rather than reading the reservation.
    #[test]
    fn the_comptime_module_name_is_the_vm_reservation() {
        assert_eq!(COMPTIME_MODULE_NAME, ds_vm::comptime::COMPTIME_MODULE);
    }

    /// The tag ceiling sits at the reserved run's floor, and the claim band's pair sits inside the
    /// run: a module mint can never alias a span discriminant.
    #[test]
    fn the_tag_ceiling_covers_the_reserved_run() {
        assert_eq!(crate::shape::FIRST_RESERVED_TAG, UTF8_ERR);
        const {
            assert!(
                crate::shape::FIRST_RESERVED_TAG < NURSERY_BOUNDARY_ERR,
                "the mint ceiling must cover every reserved tag"
            );
            assert!(crate::shape::CLAIM_BAND_CLEAR < NURSERY_BOUNDARY_ERR);
        }
    }

    /// Every failure family spells its classes once: the tags are the abi's discriminants, the
    /// names the ambient members, and no tag repeats within a family.
    #[test]
    fn the_failure_classes_are_distinct_per_family() {
        for family in [
            FailureClasses::Io,
            FailureClasses::Net,
            FailureClasses::Parse,
            FailureClasses::Utf8,
            FailureClasses::TimedOut,
        ] {
            let spelling = failure_spelling(family);
            assert!(!spelling.classes.is_empty(), "{family:?} names its classes");
            for (at, (tag, name)) in spelling.classes.iter().enumerate() {
                assert!(!name.is_empty(), "a class carries its member name");
                assert!(
                    spelling.classes[at + 1..]
                        .iter()
                        .all(|(other, _)| other != tag),
                    "{name} names its tag once"
                );
            }
        }
    }
}
