//! Ownership words: which body words carry heap references or tracked
//! counts, the RC binds, and the deinit claim and hook one value's type
//! answers.

use super::*;

impl FnLower<'_, '_, '_> {
    /// The Ref-typed body words of one value: where a value of `ty` at
    /// `base` carries heap references (the words the RC pass tracks).
    pub(super) fn ref_words(&mut self, base: u8, ty: Ty) -> Result<Vec<u8>, LowerError> {
        let words = self.shapes.words(ty)?;
        Ok(words
            .iter()
            .enumerate()
            .filter(|(_, word)| **word == SlotType::Ref)
            .map(|(index, _)| base + u8::try_from(index).expect("span width fits u8"))
            .collect())
    }

    /// The Ref words the placement tracks for one value: the place's own
    /// word classification leads, so a closure word (a fn-typed value in
    /// a Ref-classified word) is tracked where the type-derived list
    /// would miss it.
    pub(super) fn tracked_words(&mut self, place: &Place, ty: Ty) -> Result<Vec<u8>, LowerError> {
        Ok(match place.shape {
            PlaceShape::Word(SlotType::Ref) => vec![place.base],
            _ if self.is_var_ty(ty) => vec![place.base],
            _ => self.count_words(place.base, ty)?,
        })
    }

    /// Whether a type is exactly one of the checker's minted type
    /// variables: its single value word is Raw and its runtime kind is
    /// the caller's fact.
    pub(super) fn is_var_ty(&self, ty: Ty) -> bool {
        match self.members(ty) {
            [only] => self.shapes.is_var_atom(*only),
            _ => false,
        }
    }

    /// The words the RC pass tracks for one value at `base`: its Ref
    /// words plus every Raw word — a type variable's own single word or
    /// a variable-keyed union member's region claim, whose runtime kind
    /// is the caller's fact. The VM's RC ops check the runtime kind, so
    /// a scalar instantiation decs nothing and a reference
    /// instantiation stays balanced.
    pub(super) fn count_words(&mut self, base: u8, ty: Ty) -> Result<Vec<u8>, LowerError> {
        let words = self.shapes.words(ty)?;
        Ok(words
            .iter()
            .enumerate()
            .filter(|(_, word)| **word == SlotType::Ref || **word == SlotType::Raw)
            .map(|(index, _)| base + u8::try_from(index).expect("span width fits u8"))
            .collect())
    }

    /// The establishment band word of one claim site: a fresh Tag body
    /// word at the first registration, the site's own at a
    /// re-registration (a literal's temp re-bound at its binder writes
    /// the same band once).
    pub(super) fn claim_band_word(&mut self, base: u8) -> Result<u8, LowerError> {
        match self.rc.claim_band(base) {
            Some(band) => Ok(band),
            None => self.temp_word(SlotType::Tag),
        }
    }

    /// The words a discarded value must account for: its tracked Ref
    /// words plus the deinit claim's base when the value carries one —
    /// a deinit value dying unnamed at a statement still runs its hook.
    pub(super) fn discard_words(
        &mut self,
        place: &Place,
        ty: Ty,
        span: Span,
    ) -> Result<Vec<u8>, LowerError> {
        let mut words = self.tracked_words(place, ty)?;
        if let Some(claim) = self.deinit_claim(ty, span)?
            && self.rc.anchors_claim(place.base)
        {
            self.overlay_site(&claim.staged, &[SlotType::I64]);
            let band = self.claim_band_word(place.base)?;
            self.rc
                .claimed(&mut self.code, place.base, claim.hook, band, &words);
            if !words.contains(&place.base) {
                words.insert(0, place.base);
            }
        }
        Ok(words)
    }

    /// Registers an owning binder with the RC pass: its tracked words
    /// (the place's own classification leads, so a closure word behind a
    /// fn-typed binding is owned by its binder — its dec fires at the
    /// checker-recorded last use, and a never-consumed word drops at its
    /// scope's end) plus the deinit claim its concrete type carries
    /// — the value's base word anchors the hook, and the frame's overlay
    /// grows to hold the hook's staging.
    pub(super) fn rc_bind(
        &mut self,
        binder: Span,
        place: &Place,
        ty: Ty,
    ) -> Result<(), LowerError> {
        let mut words = self.tracked_words(place, ty)?;
        if let Some(claim) = self.deinit_claim(ty, binder)?
            && self.rc.anchors_claim(place.base)
        {
            self.overlay_site(&claim.staged, &[SlotType::I64]);
            let band = self.claim_band_word(place.base)?;
            self.rc
                .claimed(&mut self.code, place.base, claim.hook, band, &words);
            if !words.contains(&place.base) {
                words.insert(0, place.base);
            }
        }
        // a binder aliasing a borrowed word (a parameter, a member
        // alias) keeps no claim here: the word never anchors one, the
        // owner's own drop site firing the hook once
        self.rc.bound(binder, &words);
        Ok(())
    }

    /// Registers a borrowing binder (a parameter, a handler's member
    /// alias, a pattern field): its Ref words only. A borrow never owns
    /// the value's deinit — the owning frame's drop site fires the
    /// hook — so the base word joins the borrowed set and a later
    /// binder aliasing it (a const alias, a match binder) never
    /// re-establishes one.
    pub(super) fn rc_bind_borrowed(
        &mut self,
        binder: Span,
        base: u8,
        ty: Ty,
    ) -> Result<(), LowerError> {
        let words = self.count_words(base, ty)?;
        self.rc.bound_borrowed(binder, base, &words);
        Ok(())
    }

    /// The deinit claim one value of `ty` carries: the synthesized
    /// deinit's pool constant, the word span it stages at the death, and
    /// the span's `Ref` word offsets (the claim's decs). `None` for
    /// types without a declared deinit.
    pub(super) fn deinit_claim(
        &mut self,
        ty: Ty,
        span: Span,
    ) -> Result<Option<DeinitClaim>, LowerError> {
        let Some(hook) = self.deinit_hook(ty, span)? else {
            return Ok(None);
        };
        let staged = self.shapes.words(ty)?;
        Ok(Some(DeinitClaim { hook, staged }))
    }

    /// The deinit hook of a value's concrete type: a single struct
    /// atom whose declaration carries a `deinit` member names its
    /// synthesized function through the pool — the struct's application
    /// resolving its constructor's own. A declared deinit with no plan
    /// (a generic struct's) refuses; unions, scalars, and deinit-less
    /// structs carry none.
    pub(super) fn deinit_hook(&mut self, ty: Ty, span: Span) -> Result<Option<Hook>, LowerError> {
        let members = self.members(ty);
        let [atom] = members else {
            return Ok(None);
        };
        let def = match self.analyzed.decls.defs_of_atoms.get(atom).copied() {
            Some(def) => def,
            None => match self
                .universe()
                .atom_decl(*atom)
                .ok()
                .map(|decl| decl.kind.clone())
            {
                Some(AtomKind::Apply(ctor, _)) => {
                    match self.analyzed.decls.defs_of_atoms.get(&ctor).copied() {
                        Some(def) => def,
                        None => return Ok(None),
                    }
                },
                _ => return Ok(None),
            },
        };
        if !self.plan.declares_deinit(def) {
            return Ok(None);
        }
        let Some(index) = self.plan.deinit_index(def) else {
            return Err(LowerError::unsupported(
                span,
                "a deinit-carrying value",
                "the struct's deinit is generic; monomorphic instantiation is the CALL_GENERIC \
                 seam",
            ));
        };
        let pool = self.pool(Constant::FuncRef {
            key: FuncRefKey::Dep(0), // the enclosing module
            func: index,
        });
        let staged = self.shapes.words(ty)?;
        let width = u8::try_from(staged.len()).expect("span width fits u8");
        // the claim decs every Ref word of the span at the value's death
        let refs: Vec<u8> = staged
            .iter()
            .enumerate()
            .filter(|(_, slot)| **slot == SlotType::Ref)
            .map(|(offset, _)| u8::try_from(offset).expect("span offset fits u8"))
            .collect();
        Ok(Some(Hook {
            pool,
            width,
            refs: refs.into(),
        }))
    }

    // ---- params and scopes ---------------------------------------------------
}
