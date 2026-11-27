//! Address computation: the body words one place spans, the field store,
//! the tag word of a union place, and the single-word read every scalar
//! site shares.

use super::*;

impl<'a, 'p, 's> FnLower<'a, 'p, 's> {
    /// The runtime words of `ty`'s value inside `place`. A refined
    /// single-atom use of a union span addresses only that member's
    /// payload words.
    pub(super) fn word_addrs(
        &mut self,
        place: &Place,
        ty: Ty,
    ) -> Result<Vec<WordAddr>, LowerError> {
        let use_shape = self.shapes.shape(ty)?;
        match (&*use_shape, &place.shape) {
            (Shape::Word(slot), PlaceShape::Word(classified)) => Ok(vec![WordAddr {
                name: None,
                slot: place.base,
                // the place's own classification leads when the two
                // disagree: a closure word is Ref-classified behind a
                // fn-typed use, and the staged word's kind must tell
                // the truth for the call slide and the borrow placement
                slot_ty: if *slot == SlotType::Fn && *classified == SlotType::Ref {
                    SlotType::Ref
                } else {
                    *slot
                },
            }]),
            (Shape::Word(slot), PlaceShape::Span(underlying)) => {
                let atom = self.only_atom(ty, no_span())?;
                // a single-member span holds the member's own words, so
                // a word-shaped member sits at the span base itself
                if self.members(*underlying) == [atom] {
                    return Ok(vec![WordAddr {
                        name: None,
                        slot: place.base,
                        slot_ty: *slot,
                    }]);
                }
                let layout = self.shapes.shape(*underlying)?;
                let fields = layout
                    .member_fields(atom)
                    .ok_or_else(|| mismatch("a union member read"))?;
                match fields {
                    [] => Ok(vec![WordAddr {
                        name: None,
                        slot: place.base,
                        slot_ty: *slot,
                    }]),
                    [only] => Ok(vec![WordAddr {
                        name: None,
                        slot: place.base + only.word,
                        slot_ty: *slot,
                    }]),
                    _ => Err(mismatch("a multi-word member payload")),
                }
            },
            (Shape::Struct(s), PlaceShape::Span(underlying)) => {
                let place_shape = self.shapes.shape(*underlying)?;
                if matches!(&*place_shape, Shape::Struct(_)) {
                    // the same bare struct: words in declaration order,
                    // one address per word of each field's span
                    return Ok(s
                        .fields
                        .iter()
                        .flat_map(|f| field_addrs(place.base, f))
                        .collect());
                }
                let atom = self.only_atom(ty, no_span())?;
                let fields = place_shape
                    .member_fields(atom)
                    .ok_or_else(|| mismatch("a union member field"))?;
                let mut out = Vec::new();
                for (index, field) in s.fields.iter().enumerate() {
                    // a positional field (a tuple-payload case's) matches
                    // by position — the None names all compare equal, so
                    // the name-find would return the first field for
                    // every element
                    let member = if field.name.is_some() {
                        fields
                            .iter()
                            .find(|f| f.name == field.name)
                            .ok_or_else(|| mismatch("a union member field"))?
                    } else {
                        fields
                            .get(index)
                            .ok_or_else(|| mismatch("a union member field"))?
                    };
                    out.extend(field_addrs(place.base, member));
                }
                Ok(out)
            },
            (Shape::Tuple(s), PlaceShape::Span(underlying)) => {
                let place_shape = self.shapes.shape(*underlying)?;
                if matches!(&*place_shape, Shape::Tuple(_)) {
                    return Ok(s
                        .fields
                        .iter()
                        .flat_map(|f| field_addrs(place.base, f))
                        .collect());
                }
                // a tuple member inside a union region: the elements
                // gather from the member's claimed, possibly scattered
                // offsets — positionally, the tuple's own field map has
                // no names to match by
                let atom = self.only_atom(ty, no_span())?;
                let fields = place_shape
                    .member_fields(atom)
                    .ok_or_else(|| mismatch("a tuple member read"))?;
                if fields.len() != s.fields.len() {
                    return Err(mismatch("a tuple member's arity"));
                }
                Ok(s.fields
                    .iter()
                    .zip(fields.iter())
                    .flat_map(|(_, member)| field_addrs(place.base, member))
                    .collect())
            },
            (Shape::Union(u), PlaceShape::Span(underlying)) if *underlying == ty => Ok(u
                .words
                .iter()
                .enumerate()
                .map(|(index, slot)| WordAddr {
                    name: None,
                    slot: place.base + u8::try_from(index).expect("span width fits u8"),
                    slot_ty: *slot,
                })
                .collect()),
            (Shape::Union(_), PlaceShape::Span(underlying)) => {
                // a subset union: the value keeps the span's full layout
                let words = self.shapes.words(*underlying)?;
                Ok(words
                    .iter()
                    .enumerate()
                    .map(|(index, slot)| WordAddr {
                        name: None,
                        slot: place.base + u8::try_from(index).expect("span width fits u8"),
                        slot_ty: *slot,
                    })
                    .collect())
            },
            _ => Err(mismatch("this value's representation")),
        }
    }

    /// Moves one field's whole word span from `srcs` (the source value's
    /// word addresses, in span order) into the words at `base +
    /// field.word`: a Mov per word, an RC copy per counted word (Ref, or
    /// Raw — a variable-keyed member's claim, whose runtime kind the RC
    /// ops check).
    pub(super) fn store_field_span(&mut self, base: u8, field: &MemberField, srcs: &[WordAddr]) {
        for (offset, addr) in srcs.iter().enumerate() {
            let word = base + field.word + u8::try_from(offset).expect("field span offset fits u8");
            self.emit(Op::Mov {
                dst: Slot::body(word),
                src: Slot::body(addr.slot),
            });
            if matches!(addr.slot_ty, SlotType::Ref | SlotType::Raw) {
                self.rc.copied(&mut self.code, word, addr.slot);
            }
        }
    }

    /// The tag word of a union place.
    pub(super) fn tag_addr(&self, place: &Place) -> Result<u8, LowerError> {
        match &place.shape {
            PlaceShape::Span(ty) if self.is_span_ty(*ty) => Ok(place.base),
            PlaceShape::Word(SlotType::Tag) => Ok(place.base),
            _ => Err(mismatch("a tag word read")),
        }
    }

    /// Whether one type's values carry the tag-plus-region span: a
    /// multi-member union, or a user-declared generic enum's
    /// application — one atom whose own shape is the union over its
    /// case atoms, so its values test and dispatch like any union's.
    pub(super) fn is_span_ty(&self, ty: Ty) -> bool {
        match self.members(ty) {
            [only] => self.shapes.is_enum_application(*only),
            members => !members.is_empty(),
        }
    }

    /// Whether a checked binder's type is a bare struct span: its
    /// runtime representation is its own field words, however many — a
    /// one-word struct still binds and reads as a span, never as a
    /// scalar word.
    pub(super) fn struct_typed(&mut self, ty: Ty) -> bool {
        match self.shapes.shape(ty) {
            Ok(shape) => matches!(&*shape, Shape::Struct(_)),
            Err(_) => false,
        }
    }

    /// Whether a checked binder's type carries a whole span — a bare
    /// struct's field words, a union's tag-plus-region words, a generic
    /// enum's application — so a field the binder claims binds every
    /// word of the field's span, never its one leading word.
    pub(super) fn span_typed(&mut self, ty: Ty) -> bool {
        self.struct_typed(ty) || self.is_span_ty(ty)
    }

    // ---- statements ------------------------------------------------------------
}

pub(super) fn single(addrs: &[WordAddr], span: Span) -> Result<WordAddr, LowerError> {
    let [only] = addrs else {
        return Err(LowerError::unsupported(
            span,
            "a value",
            "this lowering point needs a single word; the value spans several",
        ));
    };
    Ok(only.clone())
}

/// The source words of one field inside a flattened address list: the
/// addresses group by field in span order, each group's base word
/// carrying the field's name.
/// The per-field source slices of one member's ordered fields over a
/// value's whole word addresses: a named field name-matches anywhere in
/// the list (a bare struct's addresses carry their field names), a
/// positional field consumes the next words in order (a tuple's
/// addresses carry no names — the walk is positional exactly where the
/// fields are). The fields arrive in declaration order and the
/// addresses in span order, so both walks stay monotone.
pub(super) fn ordered_field_srcs<'a>(
    addrs: &'a [WordAddr],
    fields: &[MemberField],
) -> Result<Vec<&'a [WordAddr]>, LowerError> {
    let mut out = Vec::with_capacity(fields.len());
    let mut pos = 0usize;
    for field in fields {
        let width = field.width();
        if field.name.is_some() {
            let start = addrs
                .iter()
                .position(|a| a.name.as_deref() == field.name.as_deref())
                .ok_or_else(|| mismatch("a value field"))?;
            out.push(
                addrs
                    .get(start..start + width)
                    .ok_or_else(|| mismatch("a value field span"))?,
            );
        } else {
            out.push(
                addrs
                    .get(pos..pos + width)
                    .ok_or_else(|| mismatch("a value field span"))?,
            );
            pos += width;
        }
    }
    Ok(out)
}
