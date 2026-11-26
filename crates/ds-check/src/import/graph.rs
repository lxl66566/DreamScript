//! The import surface: the linked half of name binding. An import's
//! selected symbols bind at use sites through a caller-supplied surface —
//! one entry per import declaration, keyed by the declaration's span —
//! while an `@std` import no graph serves keeps the merged-ambient
//! reading (its selections validate against the compile unit's merged
//! namespace) and an import no linked package serves stays declarative
//! for the link walk.

use super::*;

/// One import declaration's linked surface, keyed by the declaration's
/// span in [`ImportGraph`].
#[derive(Clone, Debug)]
pub enum ImportSurface {
    /// An `@std` path: the compile unit's merged ambient namespace
    /// serves (selections validate against it, names already visible),
    /// and a module alias resolves through `member`'s declarations.
    Merged {
        /// The merged member's file id inside the consuming compile.
        member: ds_diag::FileId,
    },
    /// A linked package member's exported surface: every top-level
    /// definition the named member declares, with the link facts a
    /// binding call site lowers through.
    Linked(MemberSurface),
    /// The path names no member of the linked package: every selection
    /// refuses naming the member segment.
    NoSuchMember {
        /// The offending member segment, for the diagnostic's wording.
        member: String,
    },
}

/// One linked member's exported surface.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct MemberSurface {
    /// The member's top-level definitions, declaration order.
    pub entries: Vec<ForeignEntry>,
    /// The member's declared trait impls, one row per impl item: the
    /// owner-keyed member link facts a consuming trait call site
    /// dispatches through, each member body's index addressing this
    /// member's own module table at the surface's link position.
    pub impls: Vec<ImplFact>,
    /// The member's declared inherent impls, one row per impl item over
    /// the package's own exported types: the type-keyed receiver
    /// seam's member link facts a consuming static or value call site
    /// dispatches through, each member body's index addressing this
    /// member's own module table at the surface's link position.
    pub inherent: Vec<InherentFact>,
    /// The linked module's tag-table rows, the declaring declaration's
    /// name beside the label and the id: the shared tag mapping's
    /// identity. A union-typed word crossing the module boundary
    /// carries the producing module's tag ids as immediates,
    /// so the consuming compile's own table must reserve each row's id
    /// for the atom the linked facts mint under the same name — the
    /// seed runs at lowering, before any body mints. Rows the consuming
    /// compile mints no atom for (the producer's private atoms) ride
    /// verbatim, consuming their ids.
    pub tags: Vec<ds_mod::MemberTag>,
}

/// One linked member's top-level definitions, the import-side shape: the
/// declared tier, the written signature a call checks against, the
/// solved effect row the caller unions, and the link facts the call
/// lowering addresses.
#[derive(Clone, Debug)]
pub struct ForeignEntry {
    // PartialEq ignores the signature: equality is the linked fact set
    // (name, tier, shape marks, span, row, link facts), which one source
    // text determines; the signature rides the surface to the decls
    // table and is never compared.
    /// The declared name.
    pub name: String,
    /// The declared visibility tier.
    pub vis: Vis,
    /// What the declaration defines.
    pub kind: ForeignKind,
    /// A generic declaration: its bodies are per-instantiation facts
    /// the linked module serves, so it refuses selection.
    pub generic: bool,
    /// A defaulted parameter: its literal stages at the linked module's
    /// own call sites, so it refuses selection.
    pub defaulted: bool,
    /// The defining name occurrence, rebased into the consuming compile's
    /// file space so two-span diagnostics render.
    pub decl_span: Span,
    /// The written signature, fn declarations only.
    pub sig: Option<Arc<FnSig>>,
    /// The definition's solved effect row.
    pub row: EffectRow,
    /// The definition's world-capability bits.
    pub caps: u8,
    /// The literal fact a `const` binding carries: the value itself, so
    /// selection sites embed it with no linked-module machinery. `None`
    /// for every other kind and for a computed initializer.
    pub value: Option<ConstFact>,
    /// The structural fact a type declaration carries: the word layout
    /// the consuming compile mints its own def from. `None` for every
    /// other kind and for every shape the vocabulary cannot spell.
    pub ty: Option<Arc<TypeFact>>,
    /// The linked dep entry's position in the consuming module's
    /// recorded list. For a re-exported entry (`pub import`) this is the
    /// ORIGIN package's position — the body lives in the origin's
    /// module, so the call's FuncRef addresses it there — while a direct
    /// member's position names the linked package itself.
    pub link: u32,
    /// The origin package's link name when this entry re-exports
    /// transitively (a `pub import` row of a package that itself
    /// imported the member): the name the next consumer's dep-list walk
    /// records for the entry's `link` position, so the closure composes
    /// over package chains. `None` for a direct member of the linked
    /// package.
    pub origin: Option<String>,
    /// The function's index inside the linked module's table; `None`
    /// for a bodiless declaration (an extern), whose callee is the
    /// native the consumer's own lowering answers by name — the
    /// compiler-wired criterion — never a linked body.
    pub func: Option<u32>,
    /// The entry links the reserved `@std` package: the compiler-wired
    /// identity the @std spellings and recognitions key on — the
    /// package's first member is the ambient core itself (the migrated
    /// members riding beside it), and a declaring compile
    /// registers that same core as its ambient, so a word-shaped atom
    /// head the consuming ambient also declares is the same declaration,
    /// and the lowerer's compiler-wired recognition keeps serving its
    /// @std-linked defs. Every other linked package's entries carry
    /// `false` and their own identities alone.
    pub std: bool,
}

/// What one linked entry declares.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ForeignKind {
    /// A function.
    Fn,
    /// A const or var binding.
    Binding,
    /// A type-namespace declaration.
    Type,
}

impl ForeignKind {
    /// The diagnostic word for the kind.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Fn => "function",
            Self::Binding => "binding",
            Self::Type => "type",
        }
    }
}

impl MemberSurface {
    /// The entry one selection or qualified member names, if any.
    #[must_use]
    pub fn entry(&self, name: &str) -> Option<&ForeignEntry> {
        self.entries.iter().find(|entry| entry.name == name)
    }
}

/// The import surfaces one compile binds through: one entry per import
/// declaration the caller linked, keyed by the declaration's span, plus
/// the link-name walk the surfaces' positions and the module's dep list
/// both derive from — the source's own import names in first-occurrence
/// order, then each surface's re-export origins as the walk met them
/// (`pub import`), so a transitive entry's FuncRef addresses the origin
/// package the same dep list names. The graph also carries the run's
/// bound artifacts: each `[artifacts]` name the manifest graph
/// resolved mapped to its installed library's exported C symbols in
/// enumeration order — the fold a bound extern's declaration resolves
/// through, exactly the table the run installs as the artifact's native
/// module.
#[derive(Clone, Debug, Default)]
pub struct ImportGraph {
    surfaces: FxHashMap<Span, ImportSurface>,
    link_order: Vec<String>,
    artifacts: std::collections::BTreeMap<String, Arc<Vec<String>>>,
}

impl ImportGraph {
    /// No surfaces: every import stays declarative.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one import declaration's surface.
    pub fn add(&mut self, span: Span, surface: ImportSurface) {
        self.surfaces.insert(span, surface);
    }

    /// Adds one resolved artifact's exported-symbol table, in
    /// enumeration order (the same sorted report `enumerate` answers).
    pub fn add_artifact(&mut self, name: &str, symbols: Vec<String>) {
        self.artifacts.insert(name.to_owned(), Arc::new(symbols));
    }

    /// One import declaration's surface.
    #[must_use]
    pub fn get(&self, span: Span) -> Option<&ImportSurface> {
        self.surfaces.get(&span)
    }

    /// The resolved artifacts' exported-symbol tables.
    #[must_use]
    pub fn artifacts(&self) -> &std::collections::BTreeMap<String, Arc<Vec<String>>> {
        &self.artifacts
    }

    /// Whether nothing linked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.surfaces.is_empty()
    }

    /// Records the link-name walk the surfaces' positions assigned: the
    /// ordered names the dep-list writer reuses, so the two never drift.
    pub fn set_link_order(&mut self, order: Vec<String>) {
        self.link_order = order;
    }

    /// The ordered link names the surfaces' positions assigned, the
    /// dep-list writer's walk: empty until the builder that assigned the
    /// positions records it.
    #[must_use]
    pub fn link_order(&self) -> &[String] {
        &self.link_order
    }
}

impl PartialEq for ForeignEntry {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.vis == other.vis
            && self.kind == other.kind
            && self.generic == other.generic
            && self.defaulted == other.defaulted
            && self.decl_span == other.decl_span
            && self.row == other.row
            && self.caps == other.caps
            && self.value == other.value
            && self.ty == other.ty
            && self.link == other.link
            && self.origin == other.origin
            && self.func == other.func
            && self.std == other.std
    }
}

impl Eq for ForeignEntry {}
