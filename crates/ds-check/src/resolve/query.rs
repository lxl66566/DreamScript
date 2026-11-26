//! The read-only surface of the package tables: the lookups, iterators, and
//! linked-member facts every later pass reads them through.

use super::*;

impl Package {
    /// The linked modules' tag-table rows the link pass collected (the
    /// shared tag mapping): the lowering seeds its own table with them
    /// before any body mints, so a union-typed word crossing a module
    /// boundary carries ids both modules agree on. Each row carries the
    /// producing declaration's name beside the label.
    #[must_use]
    pub fn linked_tag_rows(&self) -> &[ds_mod::MemberTag] {
        &self.linked_tags
    }

    /// The linked members' trait-impl facts, deduplicated per linked
    /// impl: the declaration pass declares each row's instance and the
    /// lowering reads the member links a solved dispatch addresses.
    #[must_use]
    pub fn linked_impl_facts(&self) -> &[crate::import::ImplFact] {
        &self.linked_impls
    }

    /// The linked members' inherent-impl facts with the method ids the
    /// link pass registered, deduplicated per linked impl: the
    /// declaration pass installs each member's sig and member link
    /// over its recorded id, skipping a member a root registration owns.
    #[must_use]
    pub fn linked_inherent_facts(&self) -> &[(crate::import::InherentFact, Vec<Option<MethodId>>)] {
        &self.linked_inherent
    }

    /// One linked type name's def: the link table's row, which the
    /// local `type_names` walk never holds — a name resolves through
    /// both before a local declaration's own row wins.
    #[must_use]
    pub fn foreign_type_def(&self, name: &str) -> Option<DefId> {
        self.foreign_type_defs.get(name).copied()
    }

    /// The import-bound functions, keyed by their def.
    #[must_use]
    pub fn foreign(&self) -> &FxHashMap<DefId, ForeignEntry> {
        &self.foreign
    }

    /// The selections a `pub import` row bound (a re-export): the
    /// def, the bound name it exports under, and the row's span — in
    /// binding order, a def bound by two pub rows riding twice.
    #[must_use]
    pub fn reexports(&self) -> &[(DefId, String, Span)] {
        &self.reexports
    }

    /// The module reference an id names.
    #[must_use]
    pub fn module_ref(&self, id: ModuleId) -> &ModuleRef {
        &self.module_refs[id.index() as usize]
    }

    /// The base tier's definition count: the boundary of the served
    /// prefix's leading run of the def id space (a compile's own files
    /// continue past it).
    #[must_use]
    pub fn base_def_count(&self) -> usize {
        self.base.as_ref().map_or(0, |base| base.defs.len())
    }

    /// The declaration of a definition: the base answers a served prefix
    /// id, the growth tier a compiled file's own.
    #[must_use]
    pub fn def(&self, id: DefId) -> &Def {
        let slot = id.index() as usize;
        match self.base.as_deref() {
            Some(base) if slot < base.defs.len() => &base.defs[slot],
            Some(base) => &self.defs[slot - base.defs.len()],
            None => &self.defs[slot],
        }
    }

    /// Every declaration, in collection order: the base tier's run first,
    /// the growth tier's after it.
    pub fn defs(&self) -> impl Iterator<Item = &Def> + '_ {
        self.base
            .as_ref()
            .into_iter()
            .flat_map(|base| base.defs.iter())
            .chain(self.defs.iter())
    }

    /// The declaration of a registered method: the base answers a served
    /// prefix id, the growth tier a compiled file's own.
    #[must_use]
    pub fn method(&self, id: MethodId) -> &MethodDecl {
        let slot = id.index() as usize;
        match self.base.as_deref() {
            Some(base) if slot < base.methods.len() => &base.methods[slot],
            Some(base) => &self.methods[slot - base.methods.len()],
            None => &self.methods[slot],
        }
    }

    /// The registered methods, in registration order: the base tier's run
    /// first, the growth tier's after it.
    pub fn methods(&self) -> impl Iterator<Item = &MethodDecl> + '_ {
        self.base
            .as_ref()
            .into_iter()
            .flat_map(|base| base.methods.iter())
            .chain(self.methods.iter())
    }

    /// The definition of a type-namespace name, if any: the growth
    /// tier answers first, the shared base below.
    #[must_use]
    pub fn lookup_type(&self, name: &str) -> Option<DefId> {
        self.type_names.get(name).copied().or_else(|| {
            self.base
                .as_deref()
                .and_then(|base| base.type_names.get(name).copied())
        })
    }

    /// The type declaration a name carries for ONE FILE's uses:
    /// the file's own shadowing tag declaration beats the first
    /// declaration per label, whose entry owns the shared namespace.
    #[must_use]
    pub fn lookup_type_in(&self, file: ds_diag::FileId, name: &str) -> Option<DefId> {
        if let Some(shadow) = self.file_tags.get(&file).and_then(|tags| tags.get(name)) {
            return Some(*shadow);
        }
        self.lookup_type(name)
    }

    /// The definition of a value-namespace name, if any: the growth
    /// tier answers first, the shared base below.
    #[must_use]
    pub fn lookup_value(&self, name: &str) -> Option<DefId> {
        self.value_names.get(name).copied().or_else(|| {
            self.base
                .as_deref()
                .and_then(|base| base.value_names.get(name).copied())
        })
    }

    /// Whether either namespace declares the name — the sentinel checks
    /// (`none`, `self`) that must not shadow universe spellings.
    #[must_use]
    pub fn declares(&self, name: &str) -> bool {
        self.lookup_type(name).is_some() || self.lookup_value(name).is_some()
    }

    /// The namespace-agnostic lookup: the type declaration when the name
    /// has one, else the value declaration. For consumers that cannot say
    /// which namespace they mean (a name's atom, a legacy plan); call
    /// sites that know their position use [`Self::lookup_type`] or
    /// [`Self::lookup_value`].
    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<DefId> {
        self.lookup_type(name).or_else(|| self.lookup_value(name))
    }

    /// The ids of the files collected so far, in addition order: the base
    /// tier's run first, the growth tier's after it.
    pub fn files(&self) -> impl Iterator<Item = ds_diag::FileId> + '_ {
        self.base
            .as_ref()
            .into_iter()
            .flat_map(|base| base.files.iter().copied())
            .chain(self.files.iter().copied())
    }

    /// The position of one file in the addition order (the resolutions'
    /// tree alignment).
    #[must_use]
    pub fn file_position(&self, file: ds_diag::FileId) -> Option<usize> {
        self.files().position(|candidate| candidate == file)
    }

    /// The registered candidates for a method name, in declaration order:
    /// the base tier's run first, the growth tier's after it (a growth
    /// entry for a name the base carries is seeded with the base's
    /// candidates, so its list alone is the complete one).
    pub fn method_candidates<'a>(&'a self, name: &str) -> impl Iterator<Item = MethodId> + 'a {
        let growth = self.method_index.get(name).map_or(&[][..], Vec::as_slice);
        let base = if self.method_index.contains_key(name) {
            &[][..]
        } else {
            self.base
                .as_deref()
                .and_then(|base| base.method_index.get(name))
                .map_or(&[][..], Vec::as_slice)
        };
        base.iter().chain(growth).copied()
    }

    /// Number of package-level definitions across both tiers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.defs.len() + self.base.as_ref().map_or(0, |base| base.defs.len())
    }

    /// Whether the package has no definitions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
