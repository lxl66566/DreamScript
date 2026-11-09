//! Stable `DSnnnn` error-code registry.

use core::{fmt, ops::RangeInclusive};

/// Coarse error class; the thousands digit of a code picks the class.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ErrorClass {
    /// Parse and syntax errors (DS1xxx).
    Parse,
    /// Type-system errors (DS2xxx).
    Type,
    /// Linearity and ownership errors (DS3xxx).
    Linearity,
    /// ABI, bytecode, and module-system errors (DS4xxx).
    Module,
}

impl ErrorClass {
    /// Short lowercase name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Parse => "parse",
            Self::Type => "type",
            Self::Linearity => "linearity",
            Self::Module => "module",
        }
    }

    /// Inclusive numeric range of the class's codes.
    #[must_use]
    pub const fn range(self) -> RangeInclusive<u16> {
        match self {
            Self::Parse => 1000..=1999,
            Self::Type => 2000..=2999,
            Self::Linearity => 3000..=3999,
            Self::Module => 4000..=4999,
        }
    }
}

/// Generates the enum, the string table, the class table, and `ALL` from one
/// list, so the three views cannot drift apart.
macro_rules! error_codes {
    ( $( #[doc = $doc:expr] $variant:ident => $code:literal , $class:ident ),* $(,)? ) => {
        /// Every diagnostic code of the language and toolchain.
        ///
        /// Codes are a stable API once shipped: adding variants is fine,
        /// renumbering is not. The thousands digit picks the error class
        /// (1xxx parse/syntax, 2xxx type system, 3xxx linearity,
        /// 4xxx ABI/bytecode/module). The whole table is pinned by the
        /// `code_table_snapshot` test.
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
        pub enum ErrorCode {
            $( #[doc = $doc] $variant, )*
        }

        impl ErrorCode {
            /// Every registered code, in ascending numeric order.
            pub const ALL: &'static [Self] = &[ $( Self::$variant, )* ];

            /// Stable string form, e.g. `"DS3001"`.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $( Self::$variant => $code, )* }
            }

            /// The class the code's thousands digit picks.
            #[must_use]
            pub const fn class(self) -> ErrorClass {
                match self { $( Self::$variant => ErrorClass::$class, )* }
            }
        }

        impl fmt::Display for ErrorCode {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

// design-decisions.md H6 shows a moved error as DS2001 in one example; the
// shipped registry pins linearity at 3xxx, so the two-span linear class is
// DS3xxx. The docs example predates the class partition.
error_codes! {
    /// Token not valid at this position.
    UnexpectedToken => "DS1001", Parse,
    /// Input ended while a construct was still open.
    UnexpectedEof => "DS1002", Parse,
    /// Literal does not fit its declared form.
    InvalidLiteral => "DS1003", Parse,
    /// String or raw string left unterminated.
    UnterminatedString => "DS1004", Parse,
    /// Invalid escape sequence in a string literal.
    InvalidEscape => "DS1005", Parse,
    /// Character not valid in any token.
    InvalidChar => "DS1006", Parse,
    /// String interpolation hole malformed: `$` not followed by a name or `{`.
    InvalidInterpolation => "DS1007", Parse,
    /// Bracketing delimiter unclosed, closed by the wrong partner, or stray.
    UnbalancedDelimiter => "DS1008", Parse,
    /// Source nests deeper than the parser's fixed limit.
    NestingTooDeep => "DS1009", Parse,
    /// An impl member is neither a `fn` nor a const-arrow lambda.
    InvalidImplMember => "DS1010", Parse,
    /// One call passes two values for the same named argument.
    DuplicateArgument => "DS1011", Parse,
    /// A default argument value is not a literal [F7].
    NonLiteralDefault => "DS1012", Parse,

    /// Value's type is not contained in the expected type.
    TypeMismatch => "DS2001", Type,
    /// Inference could not pin a type; there is no `any` fallback.
    InferenceFailed => "DS2002", Type,
    /// Match does not cover the scrutinee type; the message names the missing members as a set.
    NonExhaustiveMatch => "DS2003", Type,
    /// Name not found in scope.
    UnresolvedName => "DS2004", Type,
    /// Two definitions claim one name.
    DuplicateDefinition => "DS2005", Type,
    /// Two trait instance heads intersect; the message names both heads and a witness value type.
    InstanceOverlap => "DS2006", Type,
    /// Wrong number of arguments at a call.
    ArityMismatch => "DS2007", Type,
    /// `?` propagates a member the enclosing return union does not contain.
    PropagationNotContained => "DS2008", Type,
    /// A conditional instance's condition resolution reaches back to itself, directly or transitively.
    ConditionCycle => "DS2009", Type,
    /// Instance lookup found multiple containing heads; the message names the competing heads.
    AmbiguousInstance => "DS2010", Type,
    /// An instance head denotes the empty type.
    EmptyInstanceHead => "DS2011", Type,
    /// A tuple distribution would mint more atom combinations than the solver's cap.
    TupleDistributionCap => "DS2012", Type,
    /// A trait constraint has no instance for the type; the message names the trait and the type.
    UnsatisfiedConstraint => "DS2013", Type,
    /// Several trait instances match one dot-call; the message names the competing heads.
    AmbiguousCall => "DS2014", Type,

    /// Binding used after being moved.
    MovedThenUsed => "DS3001", Linearity,
    /// Binding read on a path where it may not be initialized yet.
    UninitializedUse => "DS3002", Linearity,
    /// Copy attempted on a `~Copyable` value.
    LinearCopy => "DS3003", Linearity,
    /// `deinit` well-formedness broken: not exactly once on all paths.
    DeinitIllFormed => "DS3004", Linearity,
    /// Call-scoped borrow escaped its call.
    BorrowEscapes => "DS3005", Linearity,

    /// Stored source hash does not match the module's source.
    ModuleHashMismatch => "DS4001", Module,
    /// File does not start with the dsmod magic.
    ModuleBadMagic => "DS4002", Module,
    /// dsmod format version not supported.
    ModuleVersionUnsupported => "DS4003", Module,
    /// Bytecode stream failed structural validation.
    BytecodeMalformed => "DS4004", Module,
    /// Cross-package dependency cycle at link time.
    CrossPackageCycle => "DS4005", Module,
    /// Declared dependency missing from the store.
    MissingDependency => "DS4006", Module,
    /// dsmod target-class byte is not a member of the target enum.
    ModuleBadTargetClass => "DS4007", Module,
    /// dsmod section directory broken: unknown kind, duplicate or missing section, or a range outside the file.
    ModuleSectionMalformed => "DS4008", Module,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the full registry: every variant's code string and class, in
    /// `ALL` order. Codes are stable API once shipped, so any change to the
    /// table — renumbering, reclassing, reordering, or adding a variant —
    /// must update this snapshot in the same commit, by design.
    #[test]
    fn code_table_snapshot() {
        let snapshot: &[(&str, &str, ErrorClass)] = &[
            ("UnexpectedToken", "DS1001", ErrorClass::Parse),
            ("UnexpectedEof", "DS1002", ErrorClass::Parse),
            ("InvalidLiteral", "DS1003", ErrorClass::Parse),
            ("UnterminatedString", "DS1004", ErrorClass::Parse),
            ("InvalidEscape", "DS1005", ErrorClass::Parse),
            ("InvalidChar", "DS1006", ErrorClass::Parse),
            ("InvalidInterpolation", "DS1007", ErrorClass::Parse),
            ("UnbalancedDelimiter", "DS1008", ErrorClass::Parse),
            ("NestingTooDeep", "DS1009", ErrorClass::Parse),
            ("InvalidImplMember", "DS1010", ErrorClass::Parse),
            ("DuplicateArgument", "DS1011", ErrorClass::Parse),
            ("NonLiteralDefault", "DS1012", ErrorClass::Parse),
            ("TypeMismatch", "DS2001", ErrorClass::Type),
            ("InferenceFailed", "DS2002", ErrorClass::Type),
            ("NonExhaustiveMatch", "DS2003", ErrorClass::Type),
            ("UnresolvedName", "DS2004", ErrorClass::Type),
            ("DuplicateDefinition", "DS2005", ErrorClass::Type),
            ("InstanceOverlap", "DS2006", ErrorClass::Type),
            ("ArityMismatch", "DS2007", ErrorClass::Type),
            ("PropagationNotContained", "DS2008", ErrorClass::Type),
            ("ConditionCycle", "DS2009", ErrorClass::Type),
            ("AmbiguousInstance", "DS2010", ErrorClass::Type),
            ("EmptyInstanceHead", "DS2011", ErrorClass::Type),
            ("TupleDistributionCap", "DS2012", ErrorClass::Type),
            ("UnsatisfiedConstraint", "DS2013", ErrorClass::Type),
            ("AmbiguousCall", "DS2014", ErrorClass::Type),
            ("MovedThenUsed", "DS3001", ErrorClass::Linearity),
            ("UninitializedUse", "DS3002", ErrorClass::Linearity),
            ("LinearCopy", "DS3003", ErrorClass::Linearity),
            ("DeinitIllFormed", "DS3004", ErrorClass::Linearity),
            ("BorrowEscapes", "DS3005", ErrorClass::Linearity),
            ("ModuleHashMismatch", "DS4001", ErrorClass::Module),
            ("ModuleBadMagic", "DS4002", ErrorClass::Module),
            ("ModuleVersionUnsupported", "DS4003", ErrorClass::Module),
            ("BytecodeMalformed", "DS4004", ErrorClass::Module),
            ("CrossPackageCycle", "DS4005", ErrorClass::Module),
            ("MissingDependency", "DS4006", ErrorClass::Module),
            ("ModuleBadTargetClass", "DS4007", ErrorClass::Module),
            ("ModuleSectionMalformed", "DS4008", ErrorClass::Module),
        ];
        assert_eq!(
            ErrorCode::ALL.len(),
            snapshot.len(),
            "the code table changed; update this snapshot in the same commit"
        );
        for (&code, &(name, code_str, class)) in ErrorCode::ALL.iter().zip(snapshot) {
            assert_eq!(format!("{code:?}"), name, "variant name at this slot");
            assert_eq!(code.as_str(), code_str, "code string of {name}");
            assert_eq!(code.class(), class, "class of {name}");
        }
    }

    /// Machine checks the snapshot cannot express alone: every code string is
    /// a well-formed `DSnnnn`, its number sits inside its class's thousands
    /// range, strings are unique, `ALL` is strictly ascending, and the table
    /// is terminal at DS4008.
    #[test]
    fn all_codes_well_formed_in_range_unique_and_ascending() {
        let mut prev = 0;
        let mut seen: Vec<&str> = Vec::new();
        for &code in ErrorCode::ALL {
            let s = code.as_str();
            assert!(
                s.starts_with("DS") && s.len() == 6,
                "malformed code string {s}"
            );
            let n: u16 = s[2..].parse().expect("four trailing digits");
            assert!(
                code.class().range().contains(&n),
                "{code} ({n}) outside its class range"
            );
            assert!(!seen.contains(&s), "{s} is registered twice");
            seen.push(s);
            assert!(n > prev, "{code} ({n}) breaks ascending order after {prev}");
            prev = n;
        }
        assert_eq!(prev, 4008, "the table's highest code is DS4008");
    }
}
