//! Data construction: field access, struct and tuple literals, and the
//! union member field store.

use super::*;

impl FnLower<'_, '_, '_> {
    /// `x.f`: the member payload word, addressed through the receiver's
    /// checked (possibly refined) type.
    pub(super) fn field_access(&mut self, e: &Expr, static_ty: Ty) -> Result<Place, LowerError> {
        // A written field and the comptime-keyed spelling share one
        // resolution: the keyed form resolves the copy's field name from
        // the active `for comptime` loop first, then lowers as that
        // written access (the fixed offset the seam guarantees).
        let (recv, field_name) = match e {
            Expr::Field { recv, name, .. } => {
                // A module alias's qualified const: the checker bound the
                // linked binding's def at the member's span, so the value embeds
                // through the same literal fact the selection spelling lowers
                // — no receiver word exists.
                if let Some(&Res::Def(def)) = self.res.get(name.span)
                    && let Some(fact) = self.analyzed.decls.foreign_consts.get(&def).cloned()
                {
                    return self.embed_const_fact(fact, static_ty, name.span);
                }
                (&**recv, name.name.as_str().to_owned())
            },
            Expr::FieldKey { recv, name, .. } => {
                let Some(field_name) = self.comptime_key_name(name) else {
                    return Err(LowerError::unsupported(
                        name.span(),
                        "a keyed field access",
                        "the key names no active `for comptime` record's `name` member",
                    ));
                };
                (&**recv, field_name)
            },
            _ => return Err(mismatch("a field access")),
        };
        // The comptime record binder's `name` and `tag` members are
        // string constants: the copy's own field name, and the field
        // type's nominal name (empty when composite), embed from the
        // pool, no receiver word exists.
        if let Expr::Field { name: member, .. } = e
            && let Some((field_name, tag)) = self.comptime_record_field(recv)
        {
            let record_string = if member.name == "tag" { tag } else { field_name };
            let dst = self.temp_word(SlotType::Ref)?;
            let kidx = self.pool_str(&record_string);
            self.kstr(dst, kidx);
            return Ok(Place {
                base: dst,
                shape: PlaceShape::Word(SlotType::Ref),
            });
        }
        let recv_val = self.value(recv)?;
        // the receiver's recorded type carries the refinement: a member
        // name inside an `is`/match arm
        let recv_ty = self.val(recv.span())?.ty;
        let PlaceShape::Span(underlying) = recv_val.place.shape else {
            return Err(mismatch("a field access on a scalar"));
        };
        let place_shape = self.shapes.shape(underlying)?;
        let field: &MemberField = match &*place_shape {
            Shape::Union(_) => {
                let atom = self.only_atom(recv_ty, e.span())?;
                let fields = place_shape
                    .member_fields(atom)
                    .ok_or_else(|| mismatch("a union member field"))?;
                fields
                    .iter()
                    .find(|f| f.name.as_deref() == Some(field_name.as_str()))
                    .ok_or_else(|| mismatch("a union member field"))?
            },
            Shape::Struct(s) => s
                .fields
                .iter()
                .find(|f| f.name.as_deref() == Some(field_name.as_str()))
                .ok_or_else(|| mismatch("a struct field"))?,
            Shape::Word(_) | Shape::Tuple(_) => return Err(mismatch("a field access")),
        };
        // A word-shaped field reads as its word; every wider-shaped
        // field (a struct, a tuple, a union) reads as the span the
        // expression's own checked type names, nested access resolving
        // through it. The choice keys on the field's checked shape,
        // never its raw width: a one-word struct field still spans, so
        // receiver staging and a nested access address its words.
        let use_ty = self.val(e.span())?.ty;
        let shape = if matches!(&*self.shapes.shape(use_ty)?, Shape::Word(_)) {
            PlaceShape::Word(field.slot())
        } else {
            PlaceShape::Span(use_ty)
        };
        Ok(Place {
            base: recv_val.place.base + field.word,
            shape,
        })
    }

    /// A struct literal: a fresh span, fields written by name.
    pub(super) fn struct_literal(&mut self, e: &Expr, static_ty: Ty) -> Result<Place, LowerError> {
        let Expr::Struct { fields, .. } = e else {
            return Err(mismatch("a struct literal"));
        };
        // an annotated union destination names its member here (the
        // checker propagated the annotation into the literal): a
        // deinit-carrying member refuses at the literal's own span
        if self.members(static_ty).len() > 1 {
            for atom in self.members(static_ty) {
                if let Some(def) = self.analyzed.decls.defs_of_atoms.get(atom).copied()
                    && self.plan.declares_deinit(def)
                {
                    return Err(LowerError::unsupported(
                        e.span(),
                        "a deinit-carrying union member",
                        "the union's death decs the member's payload words and frees nested \
                         objects, but dispatches no user deinit; a deinit value's death is its \
                         own binding's",
                    ));
                }
            }
        }
        let shape = self.shapes.shape(static_ty)?;
        let Shape::Struct(s) = &*shape else {
            return Err(mismatch("a struct literal"));
        };
        let target = self.temp_span(static_ty)?;
        for (name, value) in fields {
            let index = s
                .fields
                .iter()
                .position(|f| f.name.as_deref() == Some(name.name.as_str()))
                .ok_or_else(|| {
                    LowerError::unsupported(
                        name.span,
                        "a struct literal field",
                        "the field's type stayed outside the core's span vocabulary (a callable- \
                         or foreign-typed field), so the planned shape carries no such field",
                    )
                })?;
            let field = &s.fields[index];
            let (v, value_span) = if let Some(expr) = value {
                let v = self.value(expr)?;
                (v, expr.span())
            } else {
                // punning: the field name is the value's name
                let bind = self.scope_lookup(&name.name).ok_or_else(|| {
                    LowerError::unsupported(
                        name.span,
                        "a punned struct field",
                        "the name does not name an in-scope binding",
                    )
                })?;
                self.rc.use_of(bind, name.span);
                let place = self
                    .binders
                    .get(&bind)
                    .cloned()
                    .ok_or_else(|| mismatch("a punned field's binding"))?;
                let ty = self.binder_fact(bind)?.map_or(static_ty, |f| f.ty);
                (TypedPlace { place, ty }, name.span)
            };
            let addrs = self
                .word_addrs(&v.place, v.ty)
                .map_err(|err| err.respanned(value_span))?;
            if addrs.len() != field.width() {
                // A member value into a union-typed field re-spans
                // through the field's own region; anything else is the
                // honest mismatch.
                let members = self.field_members(static_ty, index);
                self.store_union_member_field(
                    target.base + field.word,
                    members.as_deref(),
                    &v,
                    name.span,
                )?;
                continue;
            }
            // a fn-typed field holds the closure Ref word: a proven
            // scalar fn value wraps into its static closure object, an
            // unproven or Ref-classified word stages as itself — the
            // runtime kind the field copy's own registration checks
            let addrs = if field.slot() == SlotType::Ref && addrs[0].slot_ty == SlotType::Fn {
                vec![self.stage_heap_fn(&v, name.span)?]
            } else {
                addrs
            };
            self.store_field_span(target.base, field, &addrs);
        }
        // the constructed value's deinit claim anchors its base word
        // the value dies unnamed unless a binder aliases it
        if let Some(claim) = self.deinit_claim(static_ty, e.span())? {
            self.overlay_site(&claim.staged, &[SlotType::I64]);
            let words = self.ref_words(target.base, static_ty)?;
            let band = self.claim_band_word(target.base)?;
            self.rc
                .claimed(&mut self.code, target.base, claim.hook, band, &words);
        }
        Ok(target)
    }

    /// The declared union sides of one field of a struct atom, by
    /// position: the shape's fields pair with the declaration's, so the
    /// union store reads the field's own union sides — a nominal
    /// struct's written field type, an application value's solved field
    /// type.
    pub(super) fn field_members(&self, ty: Ty, index: usize) -> Option<Vec<TypeId>> {
        let atom = self.only_atom(ty, no_span()).ok()?;
        let decl = self.universe().atom_decl(atom).ok()?;
        match &decl.kind {
            AtomKind::Struct(fields) => fields.get(index).and_then(|f| expr_members(&f.ty)),
            AtomKind::Apply(..) => {
                let fields = self.shapes.apply_fields_of(atom)?;
                let field = fields.get(index)?;
                Some(self.universe().members(field.ty).to_vec())
            },
            _ => None,
        }
    }

    /// Stores one member value into a union-typed literal field's
    /// region: the field's words are the union's tag-plus-payload span,
    /// and a bare member value re-spans through the same
    /// tag/zero/write construction the annotated-binding widen uses, so
    /// the field physically carries its union's tag and zeroed dead
    /// payload words. A wider union value refuses — narrow it first.
    pub(super) fn store_union_member_field(
        &mut self,
        region_base: u8,
        members: Option<&[TypeId]>,
        v: &TypedPlace,
        span: Span,
    ) -> Result<(), LowerError> {
        let mismatched = || mismatch("a struct literal field's span");
        let members = members.ok_or_else(mismatched)?;
        let shape = self.shapes.members_shape(members, 1)?;
        let Shape::Union(u) = &shape else {
            return Err(mismatched());
        };
        if self.members(v.ty).len() > 1 {
            return Err(LowerError::unsupported(
                span,
                "a union-typed struct field",
                "the field stores one member value; narrow a union value through a match first",
            ));
        }
        let atom = self.only_atom(v.ty, span)?;
        let member = u.member(atom).ok_or_else(mismatched)?;
        // a callable word entering the union's fn-typed payload slot
        // keeps the var slot's provenance rule: a closure's Ref word
        // would trap there the same way it traps a re-storable var slot
        self.refuse_union_fn_payload(v, span)?;
        let tag = self.shapes.tag_id(atom)?;
        self.emit(Op::TagNew {
            dst: Slot::body(region_base),
            tag,
        });
        self.zero_init_member(region_base, &u.words, &member.fields)?;
        let addrs = self.word_addrs(&v.place, v.ty)?;
        let srcs = ordered_field_srcs(&addrs, &member.fields)?;
        for (field, srcs) in member.fields.iter().zip(srcs) {
            self.store_field_span(region_base, field, srcs);
        }
        Ok(())
    }

    /// One anonymous tuple `(e0, e1, ...)`: a fresh frame span with each
    /// element's whole word span moved in evaluation order. A union-typed
    /// element distributed the product into several tuple atoms, which the
    /// boundary rejects — narrow the element first.
    pub(super) fn tuple_literal(
        &mut self,
        items: &[Expr],
        ty: Ty,
        span: Span,
    ) -> Result<Place, LowerError> {
        if self.members(ty).len() != 1 {
            return Err(LowerError::unsupported(
                span,
                "a tuple with a union-typed element",
                "the product distributed into several tuple atoms; narrow the element through a \
                 match before the tuple",
            ));
        }
        let shape = self.shapes.shape(ty)?;
        let Shape::Tuple(s) = &*shape else {
            return Err(mismatch("a tuple construction"));
        };
        if s.fields.len() != items.len() {
            return Err(mismatch("a tuple construction's arity"));
        }
        let target = self.temp_span(ty)?;
        for (index, item) in items.iter().enumerate() {
            let field = &s.fields[index];
            let v = self.value(item)?;
            let mut addrs = self.word_addrs(&v.place, v.ty)?;
            // A fn-typed element holds the closure Ref word: a proven
            // scalar fn value wraps into its static closure object, an
            // unproven or Ref-classified word stages as itself.
            if field.slot() == SlotType::Ref
                && let [addr] = addrs.as_mut_slice()
                && addr.slot_ty == SlotType::Fn
            {
                *addr = self.stage_heap_fn(&v, item.span())?;
            }
            if addrs.len() != field.width() {
                return Err(mismatch("a tuple element's span"));
            }
            self.store_field_span(target.base, field, &addrs);
        }
        Ok(target)
    }
}
