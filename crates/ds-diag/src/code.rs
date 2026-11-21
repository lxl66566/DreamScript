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
    /// A trait or impl member is not a `fn` declaration.
    InvalidImplMember => "DS1010", Parse,
    /// One call passes two values for the same named argument.
    DuplicateArgument => "DS1011", Parse,
    /// A default argument value is not a literal (F7).
    NonLiteralDefault => "DS1012", Parse,
    /// A top-level named const-arrow lambda; `fn` is the only named-function declaration.
    NamedConstLambda => "DS1013", Parse,
    /// A top-level statement outside the entry file's script mode.
    TopLevelStatement => "DS1014", Parse,
    /// Script statements synthesize `main`; the entry file already declares `main`.
    ScriptMainConflict => "DS1015", Parse,
    /// A match guard's bare identifier directly before `=>` is read as a bare-parameter lambda; parenthesize the guard expression.
    GuardTailLambda => "DS1016", Parse,
    /// A lambda arrow chain deeper than the parser's unbracketed recursion budget.
    ArrowChainTooDeep => "DS1017", Parse,
    /// An `else if` chain deeper than the parser's unbracketed recursion budget.
    ElseChainTooDeep => "DS1018", Parse,

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
    /// `self` used as a type name; the receiver type is spelled `Self`.
    LowercaseSelfType => "DS2015", Type,
    /// The receiver parameter carries an annotation; spell it `self` or `mut self`.
    AnnotatedReceiver => "DS2016", Type,
    /// `in` right-hand side is not a range; collection membership is not ruled yet.
    NonRangeIn => "DS2017", Type,
    /// An impl member's signature disagrees with its trait member's arity, parameter modes, parameter types, or return.
    ImplMemberSignatureMismatch => "DS2018", Type,
    /// An impl member re-declares a parameter default its trait member owns; the trait declaration is the default's single authority.
    ImplMemberDefaultRedeclared => "DS2019", Type,
    /// An impl member declares a parameter default its trait member does not have; there is no trait default to override and the literal never stages at a call site.
    ImplMemberDefaultWithoutTrait => "DS2020", Type,
    /// A trait or impl member's signature declares no receiver; the first parameter is `self` or `mut self`, typed `Self`.
    MemberWithoutReceiver => "DS2021", Type,
    /// An enum case named `none` carries a payload; `none` is the predeclared unit atom, so the case claims no atom of its own and its construction refuses.
    NonePayloadCase => "DS2022", Type,
    /// A trait impl provides no body for a trait member that declares no default body; dispatch would have no body to plan.
    ImplMemberWithoutBody => "DS2023", Type,
    /// A ruled derive family this round has not implemented yet; the message names the family and the ruled set (G1).
    DeriveUnimplemented => "DS2024", Type,
    /// A derive name outside the ruled set; the message names the ruled set (G1).
    DeriveUnknown => "DS2025", Type,
    /// A derive part refuses structural derivation (a function-typed or ~Copyable part); the message names the part (G1, B9).
    DerivePartRefused => "DS2026", Type,
    /// `==` in a derive list; equality is B9's universal structural operator and is never derived.
    DeriveEqRefused => "DS2027", Type,
    /// A derive attribute on a declaration that is not a struct, enum, or tag (G1).
    DeriveNonDataItem => "DS2028", Type,
    /// A derive on a declaration carrying its own type parameters; the synthesized member reads its parts through those variables, which the one-body-per-impl runtime seam cannot carry (per-instantiation frames are the JIT tier's).
    DeriveGenericRefused => "DS2029", Type,
    /// A parameter annotated `type` without the `comptime` mode; type values exist only inside comptime evaluation (G1, G2).
    TypeParamNotComptime => "DS2030", Type,
    /// A `comptime` parameter outside the round-one shape: every parameter comptime and `type`-annotated, on a free fn (the message names the broken clause).
    ComptimeParamShape => "DS2031", Type,
    /// A type name used as a value, or a reflection builtin called, outside a comptime evaluation context (G2).
    ComptimeValueOutside => "DS2032", Type,
    /// A world-touching call inside a comptime function: the evaluation's fence refuses it as a compile error (G1).
    ComptimeFenceRefusal => "DS2033", Type,
    /// The comptime evaluator's honest refusals: an argument shape it does not stage, a result it cannot embed, or a trap the fenced run raised.
    ComptimeEvalRefusal => "DS2034", Type,
    /// A pooled fork-join child's body reaches a sibling-addressing fiber control native.
    PooledControlNative => "DS2035", Type,
    /// A pooled fork-join child's body reaches the world surface.
    PooledWorldCall => "DS2036", Type,
    /// The comptime fence's callee admission refuses a runtime callee: an impure effect row, a generic body's evidence seam, or a member call the slice plans no body for (G1).
    ComptimeCalleeRefusal => "DS2037", Type,
    /// The `#[default]` union-member annotation's refusals: a second annotated member of one union, an annotation no default derivation reads, or one outside an enum case (G1).
    DeriveMemberAnnotation => "DS2038", Type,
    /// A `return` in a value-consumed position: an early exit is a statement, so a block whose value feeds a binding, an argument, or a join arm refuses it (F2).
    ReturnInValuePosition => "DS2039", Type,
    /// A written type application disagrees with its head declaration: a generic applied with the wrong number of type arguments, or a head that takes none (a tag, a case, a value-namespace name, a non-generic declaration).
    TypeApplicationMismatch => "DS2040", Type,
    /// A tag declared with a tuple payload; a tag's payload spells named fields (B1).
    TagPayloadTuple => "DS2041", Type,
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
    /// A `mut` window parameter receives a `const` binding; spell the binder `var`.
    MutWindowConstReceiver => "DS3006", Linearity,
    /// A for-in body opens a `mut` window over its own iterated binding; the loop's element count froze at entry, so the mutation never joins this loop's walk (D3).
    ForIterMutWindow => "DS3007", Linearity,
    /// A spawned body opens a `mut` window over a binding outliving its nursery block; the refusal is permanent and drive-independent (E2).
    SpawnOutlivedMutWindow => "DS3008", Linearity,

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
    /// A checked construct the lowering contract does not cover.
    LowerUnsupported => "DS4009", Module,
    /// A package declares two platform declarations (H5).
    PlatformDuplicate => "DS4010", Module,
    /// A platform declaration names a capability outside the world vocabulary (H5).
    PlatformUnknownCapability => "DS4011", Module,
    /// A definition's world usage needs a capability its platform does not provide (H5, E1).
    PlatformNotProvided => "DS4012", Module,
    /// World-touching code with no platform declaration (H5, E1).
    PlatformMissing => "DS4013", Module,
    /// ds.pkg manifest syntax or structure error: unknown key or section, duplicate key, malformed value, or a non-canonical spelling (H6).
    PackageManifestMalformed => "DS4014", Module,
    /// A declared dependency minimum names a version the provider does not offer of a name it knows (H6).
    UnsatisfiableVersionBound => "DS4015", Module,
    /// A package's content disagrees with a recorded or pinned blake3 hash (H6).
    PackageHashMismatch => "DS4016", Module,
    /// A private definition used from outside its defining file (G4).
    PrivateUse => "DS4017", Module,
    /// An import selects a name its target does not export to dependents (G4).
    ImportNotExported => "DS4018", Module,
    /// An import selects a name no member of the compile unit declares (G4).
    ImportNameMissing => "DS4019", Module,
    /// A visibility modifier on an item that declares no package name, or a re-export whose semantics land with cross-package linking (G4).
    VisNoTarget => "DS4020", Module,
    /// An import selects an exported name whose shape cannot bind across packages this round: a generic fn, a defaulted parameter, a computed or mutable binding, a type whose layout the surface vocabulary cannot spell (or whose name a local declaration or another linked package owns), or a linked member a module alias serves neither at a call site nor in value position (a function value, a type name) (G4).
    ImportUnsupportedShape => "DS4021", Module,
    /// Two imports of one file bind the same name (G4).
    ImportDuplicate => "DS4022", Module,
    /// A file's `##` block index exceeds the checker's DefId block-key budget (256); the block is skipped.
    DefIdBlockOverflow => "DS4023", Module,
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
            ("NamedConstLambda", "DS1013", ErrorClass::Parse),
            ("TopLevelStatement", "DS1014", ErrorClass::Parse),
            ("ScriptMainConflict", "DS1015", ErrorClass::Parse),
            ("GuardTailLambda", "DS1016", ErrorClass::Parse),
            ("ArrowChainTooDeep", "DS1017", ErrorClass::Parse),
            ("ElseChainTooDeep", "DS1018", ErrorClass::Parse),
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
            ("LowercaseSelfType", "DS2015", ErrorClass::Type),
            ("AnnotatedReceiver", "DS2016", ErrorClass::Type),
            ("NonRangeIn", "DS2017", ErrorClass::Type),
            ("ImplMemberSignatureMismatch", "DS2018", ErrorClass::Type),
            ("ImplMemberDefaultRedeclared", "DS2019", ErrorClass::Type),
            ("ImplMemberDefaultWithoutTrait", "DS2020", ErrorClass::Type),
            ("MemberWithoutReceiver", "DS2021", ErrorClass::Type),
            ("NonePayloadCase", "DS2022", ErrorClass::Type),
            ("ImplMemberWithoutBody", "DS2023", ErrorClass::Type),
            ("DeriveUnimplemented", "DS2024", ErrorClass::Type),
            ("DeriveUnknown", "DS2025", ErrorClass::Type),
            ("DerivePartRefused", "DS2026", ErrorClass::Type),
            ("DeriveEqRefused", "DS2027", ErrorClass::Type),
            ("DeriveNonDataItem", "DS2028", ErrorClass::Type),
            ("DeriveGenericRefused", "DS2029", ErrorClass::Type),
            ("TypeParamNotComptime", "DS2030", ErrorClass::Type),
            ("ComptimeParamShape", "DS2031", ErrorClass::Type),
            ("ComptimeValueOutside", "DS2032", ErrorClass::Type),
            ("ComptimeFenceRefusal", "DS2033", ErrorClass::Type),
            ("ComptimeEvalRefusal", "DS2034", ErrorClass::Type),
            ("PooledControlNative", "DS2035", ErrorClass::Type),
            ("PooledWorldCall", "DS2036", ErrorClass::Type),
            ("ComptimeCalleeRefusal", "DS2037", ErrorClass::Type),
            ("DeriveMemberAnnotation", "DS2038", ErrorClass::Type),
            ("ReturnInValuePosition", "DS2039", ErrorClass::Type),
            ("TypeApplicationMismatch", "DS2040", ErrorClass::Type),
            ("TagPayloadTuple", "DS2041", ErrorClass::Type),
            ("MovedThenUsed", "DS3001", ErrorClass::Linearity),
            ("UninitializedUse", "DS3002", ErrorClass::Linearity),
            ("LinearCopy", "DS3003", ErrorClass::Linearity),
            ("DeinitIllFormed", "DS3004", ErrorClass::Linearity),
            ("BorrowEscapes", "DS3005", ErrorClass::Linearity),
            ("MutWindowConstReceiver", "DS3006", ErrorClass::Linearity),
            ("ForIterMutWindow", "DS3007", ErrorClass::Linearity),
            ("SpawnOutlivedMutWindow", "DS3008", ErrorClass::Linearity),
            ("ModuleHashMismatch", "DS4001", ErrorClass::Module),
            ("ModuleBadMagic", "DS4002", ErrorClass::Module),
            ("ModuleVersionUnsupported", "DS4003", ErrorClass::Module),
            ("BytecodeMalformed", "DS4004", ErrorClass::Module),
            ("CrossPackageCycle", "DS4005", ErrorClass::Module),
            ("MissingDependency", "DS4006", ErrorClass::Module),
            ("ModuleBadTargetClass", "DS4007", ErrorClass::Module),
            ("ModuleSectionMalformed", "DS4008", ErrorClass::Module),
            ("LowerUnsupported", "DS4009", ErrorClass::Module),
            ("PlatformDuplicate", "DS4010", ErrorClass::Module),
            ("PlatformUnknownCapability", "DS4011", ErrorClass::Module),
            ("PlatformNotProvided", "DS4012", ErrorClass::Module),
            ("PlatformMissing", "DS4013", ErrorClass::Module),
            ("PackageManifestMalformed", "DS4014", ErrorClass::Module),
            ("UnsatisfiableVersionBound", "DS4015", ErrorClass::Module),
            ("PackageHashMismatch", "DS4016", ErrorClass::Module),
            ("PrivateUse", "DS4017", ErrorClass::Module),
            ("ImportNotExported", "DS4018", ErrorClass::Module),
            ("ImportNameMissing", "DS4019", ErrorClass::Module),
            ("VisNoTarget", "DS4020", ErrorClass::Module),
            ("ImportUnsupportedShape", "DS4021", ErrorClass::Module),
            ("ImportDuplicate", "DS4022", ErrorClass::Module),
            ("DefIdBlockOverflow", "DS4023", ErrorClass::Module),
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
    /// is terminal at DS4022.
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
        assert_eq!(prev, 4023, "the table's highest code is DS4023");
    }
}
