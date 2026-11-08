//! Witness value types for non-empty overlaps [B6].

use crate::{
    atom::{AtomKind, Field, TypeId},
    error::Error,
    universe::{Ty, Universe},
};

/// One payload field of a witness, with its solved type.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WitnessField {
    /// Field name; `None` for positional payload slots [B3].
    pub name: Option<String>,
    /// Solved type of the field.
    pub ty: Ty,
}

/// A concrete value-type witness describing one inhabitant of a non-empty
/// type: an atom plus one level of field/case shape. Field types are shown,
/// not recursively expanded, so recursive shapes terminate; B6 overlap errors
/// name both heads plus one of these.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Witness {
    /// A unit tag (including `none`): the tag is the witness [B1].
    Unit {
        /// The witnessed atom.
        atom: TypeId,
    },
    /// A struct: the atom plus its field types.
    Struct {
        /// The witnessed atom.
        atom: TypeId,
        /// One entry per declared field, in declaration order.
        fields: Vec<WitnessField>,
    },
    /// An enum case: the atom, the case index, and the payload types.
    EnumCase {
        /// The witnessed atom.
        atom: TypeId,
        /// Index of the inhabited case, in declaration order.
        case: usize,
        /// One entry per case field, in declaration order.
        fields: Vec<WitnessField>,
    },
}

impl Universe {
    /// Produces a witness for a non-empty type [B6]: deterministically the
    /// earliest-declared member that is shallowly inhabited, with one level
    /// of field/case shape.
    ///
    /// Shallow means each field's type is checked non-empty as a solved set,
    /// not recursively inhabited; a struct or case with an empty-typed field
    /// is skipped. A reserved atom is an error (the declaration fixpoint has
    /// not finished). A type with no shallowly inhabited member returns
    /// [`Error::WitnessOfEmpty`].
    pub fn witness(&mut self, t: Ty) -> Result<Witness, Error> {
        // Copy members out: solving field types below needs &mut self.
        let members: Vec<TypeId> = self.members(t).to_vec();
        for atom in members {
            // Clone the shape out for the same reason.
            let kind = self.atom_decl(atom)?.kind.clone();
            match kind {
                AtomKind::Unit => return Ok(Witness::Unit { atom }),
                AtomKind::Reserved => return Err(Error::IncompleteAtom(atom)),
                AtomKind::Struct(fields) => {
                    if let Some(fields) = self.witness_fields(&fields)? {
                        return Ok(Witness::Struct { atom, fields });
                    }
                },
                AtomKind::Enum(cases) => {
                    for (index, case) in cases.iter().enumerate() {
                        if let Some(fields) = self.witness_fields(&case.fields)? {
                            return Ok(Witness::EnumCase {
                                atom,
                                case: index,
                                fields,
                            });
                        }
                    }
                },
            }
        }
        Err(Error::WitnessOfEmpty(t))
    }

    fn witness_fields(&mut self, fields: &[Field]) -> Result<Option<Vec<WitnessField>>, Error> {
        let mut out = Vec::with_capacity(fields.len());
        for field in fields {
            let ty = self.solve(&field.ty)?;
            if self.is_empty(ty) {
                return Ok(None);
            }
            out.push(WitnessField {
                name: field.name.clone(),
                ty,
            });
        }
        Ok(Some(out))
    }

    /// Renders a witness as `atom`, `atom{f: t, ...}`, or `atom.case{t, ...}`
    /// for diagnostics.
    #[must_use]
    pub fn render_witness(&self, witness: &Witness) -> String {
        match witness {
            Witness::Unit { atom } => self.atom_name(*atom).to_owned(),
            Witness::Struct { atom, fields } => {
                format!(
                    "{}{{{}}}",
                    self.atom_name(*atom),
                    self.render_fields(fields)
                )
            },
            Witness::EnumCase { atom, case, fields } => {
                let case_name = self
                    .atom_decl(*atom)
                    .ok()
                    .and_then(|decl| match &decl.kind {
                        AtomKind::Enum(cases) => cases.get(*case).map(|c| c.name.as_str()),
                        AtomKind::Unit | AtomKind::Reserved | AtomKind::Struct(_) => None,
                    })
                    .unwrap_or("<unknown>");
                if fields.is_empty() {
                    format!("{}.{}", self.atom_name(*atom), case_name)
                } else if fields.iter().all(|f| f.name.is_none()) {
                    // tuple case: parens [B3]
                    format!(
                        "{}.{}({})",
                        self.atom_name(*atom),
                        case_name,
                        self.render_fields(fields)
                    )
                } else {
                    format!(
                        "{}.{}{{{}}}",
                        self.atom_name(*atom),
                        case_name,
                        self.render_fields(fields)
                    )
                }
            },
        }
    }

    fn render_fields(&self, fields: &[WitnessField]) -> String {
        let parts: Vec<String> = fields
            .iter()
            .map(|field| match &field.name {
                Some(name) => format!("{}: {}", name, self.render(field.ty)),
                None => self.render(field.ty),
            })
            .collect();
        parts.join(", ")
    }

    fn atom_name(&self, id: TypeId) -> &str {
        self.atom_decl(id)
            .map_or("<unknown>", |decl| decl.name.as_str())
    }
}
