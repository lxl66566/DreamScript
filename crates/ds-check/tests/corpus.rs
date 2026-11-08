//! Focused tests over the shapes of `conformance/types_hybrid.ds`: set-typed
//! errors, json unions, `is`-style containment, difference erasure, `T?`
//! equivalence, negated markers, and witnesses [B1, B4, B6].

use ds_check::{
    Case, Error, Field, TraitId, Ty, TypeExpr, TypeId, Universe, Witness, WitnessField,
};

struct Corpus {
    universe: Universe,
    int: TypeId,
    string: TypeId,
    io: TypeId,
    missing: TypeId,
    bad_port: TypeId,
    json_null: TypeId,
    json_num: TypeId,
    config: TypeId,
    copyable: TraitId,
}

fn corpus() -> Corpus {
    let mut u = Universe::new();
    let int = u.declare_tag("int").unwrap();
    let float = u.declare_tag("float").unwrap();
    let string = u.declare_tag("string").unwrap();

    let io = u.declare_tag("io_err").unwrap();
    let missing = u.declare_tag("missing").unwrap();
    let bad_port = u.declare_struct("bad_port").unwrap();
    u.set_struct_fields(bad_port, vec![Field::named("got", TypeExpr::atom(int))])
        .unwrap();

    let json_null = u.declare_tag("json_null").unwrap();
    let json_bool = u.declare_tag("json_bool").unwrap();
    let json_num = u.declare_struct("json_num").unwrap();
    u.set_struct_fields(json_num, vec![Field::named("value", TypeExpr::atom(float))])
        .unwrap();
    let json_str = u.declare_struct("json_str").unwrap();
    u.set_struct_fields(json_str, vec![Field::named(
        "value",
        TypeExpr::atom(string),
    )])
    .unwrap();
    let json_arr = u.declare_tag("json_arr").unwrap();
    let json_obj = u.declare_tag("json_obj").unwrap();

    let config = u.declare_struct("config").unwrap();
    u.set_struct_fields(config, vec![
        Field::named("host", TypeExpr::atom(string)),
        Field::named("port", TypeExpr::atom(int)),
        Field::named("retries", TypeExpr::atom(int)),
    ])
    .unwrap();

    let copyable = u.declare_trait("Copyable").unwrap();
    for atom in [int, float, string, config] {
        u.impl_trait(copyable, atom).unwrap();
    }

    let _ = (json_bool, json_str, json_arr, json_obj); // held by the universe
    Corpus {
        universe: u,
        int,
        string,
        io,
        missing,
        bad_port,
        json_null,
        json_num,
        config,
        copyable,
    }
}

impl Corpus {
    fn ty(&mut self, id: TypeId) -> Ty {
        self.universe.atom_ty(id).unwrap()
    }

    fn by_name(&mut self, name: &str) -> Ty {
        let id = self.universe.atom_by_name(name).unwrap();
        self.ty(id)
    }

    /// `type cfg_err = io_err | missing | bad_port`
    fn cfg_err(&mut self) -> Ty {
        self.universe
            .solve(
                &TypeExpr::atom(self.io)
                    .union(self.missing.into())
                    .union(self.bad_port.into()),
            )
            .unwrap()
    }

    /// `type json = json_null | json_bool | json_num | json_str | json_arr | json_obj`
    fn json(&mut self) -> Ty {
        let names = [
            "json_null",
            "json_bool",
            "json_num",
            "json_str",
            "json_arr",
            "json_obj",
        ];
        let mut expr: Option<TypeExpr> = None;
        for name in names {
            let id = self.universe.atom_by_name(name).unwrap();
            expr = Some(match expr {
                None => TypeExpr::atom(id),
                Some(e) => e.union(id.into()),
            });
        }
        self.universe.solve(&expr.unwrap()).unwrap()
    }

    /// get_port's written return union: `int | missing | bad_port`
    fn get_port_ret(&mut self) -> Ty {
        self.universe
            .solve(
                &TypeExpr::atom(self.int)
                    .union(self.missing.into())
                    .union(self.bad_port.into()),
            )
            .unwrap()
    }
}

#[test]
fn cfg_err_union_members_and_render() {
    let mut c = corpus();
    let cfg_err = c.cfg_err();
    assert_eq!(c.universe.members(cfg_err), &[c.io, c.missing, c.bad_port]);
    let text = c.universe.render(cfg_err);
    assert_eq!(text, "io_err | missing | bad_port");
}

#[test]
fn json_union_membership() {
    let mut c = corpus();
    let json = c.json();
    for name in [
        "json_null",
        "json_bool",
        "json_num",
        "json_str",
        "json_arr",
        "json_obj",
    ] {
        let member = c.by_name(name);
        assert!(c.universe.contains(json, member), "{name} in json");
    }
    let json_num = c.ty(c.json_num);
    let json_null = c.ty(c.json_null);
    assert!(
        c.universe.is_disjoint(json_num, json_null),
        "distinct atoms are disjoint"
    );
    let cfg_err = c.cfg_err();
    assert!(
        c.universe.is_disjoint(json, cfg_err),
        "json and cfg_err share no atom"
    );
    assert!(
        !c.universe.contains(json_num, json),
        "membership is by tag identity [B1]"
    );
}

#[test]
fn get_port_propagation_containment() {
    let mut c = corpus();
    let ret = c.get_port_ret();
    for atom in [c.int, c.missing, c.bad_port] {
        let member = c.ty(atom);
        assert!(c.universe.contains(ret, member));
    }
    let json_null = c.ty(c.json_null);
    assert!(!c.universe.contains(ret, json_null));
    // `?` on an `int | none` intermediate propagates `int` into `ret`; the
    // none member is not contained until the return union carries it.
    let int = c.ty(c.int);
    let pv = c.universe.optional(int);
    assert!(c.universe.contains(ret, int));
    assert!(
        !c.universe.contains(ret, pv),
        "none is not in the written union"
    );
    // load's return: config | cfg_err. `?` propagates only get_port's error
    // members — (int | missing | bad_port) - int — into that union; the
    // success member int is consumed by the binding, not propagated [B2, E4].
    let config = c.ty(c.config);
    let cfg_err = c.cfg_err();
    let load_ret = c.universe.union(config, cfg_err);
    let errors = c.universe.diff(ret, int);
    assert!(c.universe.contains(load_ret, errors));
    assert!(c.universe.contains(load_ret, config));
}

#[test]
fn difference_erases_when_disjoint() {
    let mut c = corpus();
    let config = c.ty(c.config);
    let cfg_err = c.cfg_err();
    // `const ok: config - read_err` where read_err == cfg_err [B1]
    let erased = c.universe.diff(config, cfg_err);
    assert_eq!(
        erased, config,
        "difference of disjoint types erases entirely"
    );
    let expr = c
        .universe
        .solve(
            &TypeExpr::atom(c.config).diff(
                TypeExpr::atom(c.io)
                    .union(c.missing.into())
                    .union(c.bad_port.into()),
            ),
        )
        .unwrap();
    assert_eq!(
        expr, config,
        "expression-level difference lowers to the solved shape"
    );
}

#[test]
fn difference_erasure_on_overlap() {
    let mut c = corpus();
    let cfg_err = c.cfg_err();
    let missing = c.ty(c.missing);
    let without_missing = c.universe.diff(cfg_err, missing);
    let io = c.ty(c.io);
    let bad_port = c.ty(c.bad_port);
    let expected = c.universe.union(io, bad_port);
    assert_eq!(without_missing, expected);
    // `.err`-style: (config | cfg_err) - cfg_err is exactly config.
    let config = c.ty(c.config);
    let widened = c.universe.union(config, cfg_err);
    let shrunk = c.universe.diff(widened, cfg_err);
    assert_eq!(shrunk, config);
}

#[test]
fn optional_sugar_is_union_none() {
    let mut c = corpus();
    let int = c.ty(c.int);
    let missing = c.ty(c.missing);
    let none = c.ty(TypeId::NONE);
    // (int | missing)? == int | missing | none [B4]
    let inner = c.universe.union(int, missing);
    let opt = c.universe.optional(inner);
    let written = c
        .universe
        .solve(
            &TypeExpr::atom(c.int)
                .union(c.missing.into())
                .union(TypeId::NONE.into()),
        )
        .unwrap();
    assert_eq!(opt, written);
    let none_opt = c.universe.optional(none);
    assert_eq!(none_opt, none);
    // niche packing is a later optimizer pass; here the sets are identical
    let int_opt = c.universe.optional(int);
    let int_or_none = c.universe.union(int, none);
    assert_eq!(int_opt, int_or_none);
}

#[test]
fn negated_marker_complement() {
    let mut c = corpus();
    let not_copyable = c.universe.solve(&TypeExpr::not(c.copyable)).unwrap();
    // none plus every atom not implementing Copyable
    let mut expected = c.universe.empty_ty();
    for name in [
        "none",
        "io_err",
        "missing",
        "bad_port",
        "json_null",
        "json_bool",
        "json_num",
        "json_str",
        "json_arr",
        "json_obj",
    ] {
        let member = c.by_name(name);
        expected = c.universe.union(expected, member);
    }
    assert_eq!(not_copyable, expected);
    // json is entirely non-Copyable: json & ~Copyable == json [C1 composition]
    let json = c.json();
    let json_noncopyable = c.universe.inter(json, not_copyable);
    assert_eq!(json_noncopyable, json);
    // copyable scalars vanish under ~Copyable
    let int = c.ty(c.int);
    let string = c.ty(c.string);
    let scalars = c.universe.union(int, string);
    let residue = c.universe.inter(scalars, not_copyable);
    assert!(c.universe.is_empty(residue));
}

#[test]
fn trait_as_type_and_nominal_subtyping() {
    let mut c = corpus();
    let copyable = c.universe.solve(&TypeExpr::trait_ref(c.copyable)).unwrap();
    let int = c.ty(c.int);
    let config = c.ty(c.config);
    let io = c.ty(c.io);
    assert!(c.universe.contains(copyable, int));
    assert!(c.universe.contains(copyable, config));
    assert!(c.universe.is_disjoint(copyable, io));
    // `const a: animal = dog{}` shape: dog's singleton is contained in animal
    let animal = c.universe.declare_trait("animal").unwrap();
    let dog = c.universe.declare_struct("dog").unwrap();
    c.universe.set_struct_fields(dog, vec![]).unwrap();
    c.universe.impl_trait(animal, dog).unwrap();
    let animal_ty = c.universe.solve(&TypeExpr::trait_ref(animal)).unwrap();
    let dog_ty = c.ty(dog);
    assert!(c.universe.contains(animal_ty, dog_ty));
    assert!(c.universe.is_disjoint(animal_ty, io));
}

#[test]
fn b6_overlap_witnesses_unit_head() {
    let mut c = corpus();
    // impl Draw for Circle; impl Draw for Circle | Square  =>  overlap
    let draw = c.universe.declare_trait("Draw").unwrap();
    let circle = c.universe.declare_tag("circle").unwrap();
    let square = c.universe.declare_tag("square").unwrap();
    c.universe.impl_trait(draw, circle).unwrap();
    let head1 = c.ty(circle);
    let head2 = c
        .universe
        .solve(&TypeExpr::atom(circle).union(square.into()))
        .unwrap();
    assert!(!c.universe.is_disjoint(head1, head2), "heads overlap");
    let overlap = c.universe.inter(head1, head2);
    let witness = c.universe.witness(overlap).unwrap();
    assert_eq!(witness, Witness::Unit { atom: circle });
    let text = c.universe.render_witness(&witness);
    assert_eq!(text, "circle");
}

#[test]
fn b6_overlap_witnesses_struct_head() {
    let mut c = corpus();
    let bad_port = c.ty(c.bad_port);
    let head2 = c
        .universe
        .solve(&TypeExpr::atom(c.bad_port).union(c.missing.into()))
        .unwrap();
    let overlap = c.universe.inter(bad_port, head2);
    let witness = c.universe.witness(overlap).unwrap();
    let int = c.ty(c.int);
    assert_eq!(witness, Witness::Struct {
        atom: c.bad_port,
        fields: vec![WitnessField {
            name: Some("got".to_owned()),
            ty: int
        }],
    });
    let text = c.universe.render_witness(&witness);
    assert_eq!(text, "bad_port{got: int}");
    // negation carving exclusivity [B6]: circle & ~Shadowed removes the overlap
    let shadowed = c.universe.declare_trait("Shadowed").unwrap();
    let circle = c.universe.declare_tag("circle").unwrap();
    c.universe.impl_trait(shadowed, circle).unwrap();
    let carved = c
        .universe
        .solve(&TypeExpr::atom(circle).inter(TypeExpr::not(shadowed)))
        .unwrap();
    assert!(
        c.universe.is_empty(carved),
        "circle is Shadowed, so the carve is empty"
    );
}

#[test]
fn enum_witness_names_first_inhabited_case() {
    let mut c = corpus();
    let option = c.universe.declare_enum("option").unwrap();
    c.universe
        .set_enum_cases(option, vec![
            Case::new("some", vec![Field::positional(TypeExpr::atom(c.int))]),
            Case::new("nothing", vec![]),
        ])
        .unwrap();
    let option_ty = c.ty(option);
    let witness = c.universe.witness(option_ty).unwrap();
    let int = c.ty(c.int);
    assert_eq!(witness, Witness::EnumCase {
        atom: option,
        case: 0,
        fields: vec![WitnessField {
            name: None,
            ty: int
        }],
    });
    let text = c.universe.render_witness(&witness);
    assert_eq!(text, "option.some(int)");
    // a case whose payload solves empty is skipped; `bare` still inhabits
    let opt2 = c.universe.declare_enum("opt2").unwrap();
    c.universe
        .set_enum_cases(opt2, vec![
            Case::new("full", vec![Field::positional(
                TypeExpr::atom(c.io).diff(TypeExpr::atom(c.io)),
            )]),
            Case::new("bare", vec![]),
        ])
        .unwrap();
    let opt2_ty = c.ty(opt2);
    let w2 = c.universe.witness(opt2_ty).unwrap();
    let text2 = c.universe.render_witness(&w2);
    assert_eq!(text2, "opt2.bare");
}

#[test]
fn witness_edge_cases() {
    let mut c = corpus();
    let empty = c.universe.empty_ty();
    assert_eq!(c.universe.witness(empty), Err(Error::WitnessOfEmpty(empty)));
    // a struct with an empty-typed field is shallowly uninhabited and skipped
    let broken = c.universe.declare_struct("broken").unwrap();
    c.universe
        .set_struct_fields(broken, vec![Field::named(
            "f",
            TypeExpr::atom(c.io).diff(TypeExpr::atom(c.io)),
        )])
        .unwrap();
    let broken_ty = c.ty(broken);
    assert_eq!(
        c.universe.witness(broken_ty),
        Err(Error::WitnessOfEmpty(broken_ty))
    );
    let io = c.ty(c.io);
    let with_escape = c.universe.union(broken_ty, io);
    assert_eq!(
        c.universe.witness(with_escape),
        Ok(Witness::Unit { atom: c.io })
    );
    // an empty enum is uninhabited
    let void = c.universe.declare_enum("void").unwrap();
    c.universe.set_enum_cases(void, vec![]).unwrap();
    let void_ty = c.ty(void);
    assert_eq!(
        c.universe.witness(void_ty),
        Err(Error::WitnessOfEmpty(void_ty))
    );
    // determinism: earliest declared member of cfg_err is io_err
    let cfg_err = c.cfg_err();
    let w = c.universe.witness(cfg_err).unwrap();
    assert_eq!(w, Witness::Unit { atom: c.io });
}

#[test]
fn structural_equality_of_normal_forms() {
    let mut c = corpus();
    let io = c.ty(c.io);
    let missing = c.ty(c.missing);
    let ab = c.universe.union(io, missing);
    let ba = c.universe.union(missing, io);
    assert_eq!(ab, ba);
    let expr_ab = c
        .universe
        .solve(&TypeExpr::atom(c.io).union(c.missing.into()))
        .unwrap();
    let expr_ba = c
        .universe
        .solve(&TypeExpr::atom(c.missing).union(c.io.into()))
        .unwrap();
    assert_eq!(expr_ab, expr_ba);
    assert_ne!(io, missing);
}

#[test]
fn registry_validation() {
    let mut c = corpus();
    assert_eq!(
        c.universe.declare_tag("int"),
        Err(Error::DuplicateName("int".to_owned()))
    );
    assert_eq!(
        c.universe.declare_trait("int"),
        Err(Error::DuplicateName("int".to_owned()))
    );
    assert_eq!(
        c.universe.declare_trait("none"),
        Err(Error::DuplicateName("none".to_owned()))
    );
    let bad = TypeId::from_raw(9999);
    assert_eq!(
        c.universe.solve(&TypeExpr::atom(bad)),
        Err(Error::UnknownAtom(bad))
    );
    assert_eq!(
        c.universe
            .solve(&TypeExpr::trait_ref(TraitId::from_raw(9999))),
        Err(Error::UnknownTrait(TraitId::from_raw(9999)))
    );
    assert_eq!(c.universe.atom_ty(bad), Err(Error::UnknownAtom(bad)));
    // shape state machine
    assert_eq!(
        c.universe.set_struct_fields(c.io, vec![]),
        Err(Error::WrongAtomKind(c.io))
    );
    assert_eq!(
        c.universe.set_struct_fields(c.bad_port, vec![]),
        Err(Error::ShapeAlreadySet(c.bad_port))
    );
    let reserved = c.universe.declare_struct("reserved").unwrap();
    let reserved_ty = c.ty(reserved);
    assert_eq!(
        c.universe.witness(reserved_ty),
        Err(Error::IncompleteAtom(reserved))
    );
    // impl_trait validates both ids; re-inserting a member is a no-op
    assert_eq!(
        c.universe.impl_trait(c.copyable, bad),
        Err(Error::UnknownAtom(bad))
    );
    assert!(c.universe.impl_trait(c.copyable, c.io).is_ok());
    let once = c.universe.solve(&TypeExpr::trait_ref(c.copyable)).unwrap();
    assert!(c.universe.impl_trait(c.copyable, c.io).is_ok());
    let twice = c.universe.solve(&TypeExpr::trait_ref(c.copyable)).unwrap();
    assert_eq!(once, twice);
}

#[test]
fn error_codes_pin_to_ds_diag_registry() {
    let mut c = corpus();
    assert_eq!(Error::UnknownAtom(TypeId::NONE).code().as_str(), "DS2004");
    assert_eq!(
        Error::DuplicateName(String::new()).code().as_str(),
        "DS2005"
    );
    let empty = c.universe.empty_ty();
    assert_eq!(Error::WitnessOfEmpty(empty).code().as_str(), "DS2006");
    assert_eq!(
        Error::IncompleteAtom(TypeId::NONE).code().as_str(),
        "DS2004"
    );
    assert_eq!(
        Error::ShapeAlreadySet(TypeId::NONE).code().as_str(),
        "DS2005"
    );
    assert_eq!(Error::WrongAtomKind(TypeId::NONE).code().as_str(), "DS2005");
    assert_eq!(
        Error::UnknownTrait(TraitId::from_raw(0)).code().as_str(),
        "DS2004"
    );
}

#[test]
fn render_empty_and_none() {
    let mut c = corpus();
    let empty = c.universe.empty_ty();
    let text_empty = c.universe.render(empty);
    assert_eq!(text_empty, "empty");
    let none = c.ty(TypeId::NONE);
    let text_none = c.universe.render(none);
    assert_eq!(text_none, "none");
    let int = c.ty(c.int);
    let opt = c.universe.optional(int);
    let text_opt = c.universe.render(opt);
    assert_eq!(text_opt, "none | int");
}
