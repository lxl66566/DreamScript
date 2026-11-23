//! The package-surface conversion: one served prefix's own members
//! rendered as the `.dsi` document `ds info` prints — every `pub`
//! definition of the package's own files as its signature line, spelled
//! through [`ds_fmt::signature`] over items synthesized from the
//! artifact's tables, so the artifact and the source tree answer one
//! rendering (no second renderer forked).
//!
//! The honest boundary is what the artifact carries: signatures and
//! shapes ride its tables, so functions (a bodied fn's const-arrow
//! binding included), structs, enums, tags, and traits render; a
//! binding's initializer, an alias's target, a struct or enum's supers,
//! a case's `#[default]` marker, an associated type, and an extern's
//! `extern` keyword stay fresh by G3.5 or outside the format, so those
//! declarations render no line here (the source rendering keeps spelling
//! them). A generic function renders its generic parameters — the
//! bodiless spelling — where a bodied generic fn's source line drops
//! them; the artifact's single `Fn` kind cannot tell the two apart.

use ds_check::{DefId, DefKind, MethodId, MethodOwner, Vis};
use ds_diag::Span;
use ds_parse::{
    BindName, EnumCase, EnumDecl, EnumPayload, FnDecl, Ident, Item, ItemKind, Param, StructDecl,
    StructMember, TagDecl, TraitDecl, TraitMember, TypeParam,
};

use crate::ServedPrefix;

/// The own-members band of one served prefix: how many registered files
/// sit before the package's own members (the ambient band) and how many
/// follow them (the entry session's host provision). A definition's file
/// decides membership, so the band is the caller's knowledge about the
/// registration the artifact pins.
#[derive(Clone, Copy, Debug, Default)]
pub struct SurfaceBand {
    /// Registered files before the package's own members.
    pub leading: usize,
    /// Registered files after the package's own members.
    pub trailing: usize,
}

/// One served prefix's own-member `pub` surface: each exported
/// definition's name with its `.dsi` signature line, in definition
/// order — the conversion every consumer of a written package surface
/// shares (`ds info`'s document, the LSP hover's one line).
#[must_use]
pub fn surface_lines(prefix: &ServedPrefix, band: SurfaceBand) -> Vec<(String, String)> {
    let files: Vec<_> = prefix.package.files().collect();
    let Some(own) = files.get(band.leading..files.len().saturating_sub(band.trailing)) else {
        return Vec::new();
    };
    let methods: Vec<_> = prefix.package.methods().enumerate().collect();
    prefix
        .package
        .defs()
        .enumerate()
        .filter(|(_, def)| own.contains(&def.span.file) && def.vis == Vis::Pub)
        .filter_map(|(index, def)| {
            let id = DefId::from_raw(u32::try_from(index).expect("def count fits u32"));
            line_of(prefix, id, def, &methods).map(|line| (def.name.clone(), line))
        })
        .collect()
}

/// [`surface_lines`] as the document `ds info` prints: one line per
/// exported definition, in order.
#[must_use]
pub fn member_surface(prefix: &ServedPrefix, band: SurfaceBand) -> String {
    let mut out = String::new();
    for (_, line) in surface_lines(prefix, band) {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// One exported definition's signature line — the hover tier: the first
/// `pub` definition of the name among the package's own members.
#[must_use]
pub fn signature_line(prefix: &ServedPrefix, band: SurfaceBand, name: &str) -> Option<String> {
    surface_lines(prefix, band)
        .into_iter()
        .find(|(def, _)| def == name)
        .map(|(_, line)| line)
}

/// One exported definition's line over the artifact's tables, or `None`
/// for a kind whose declaration the tables do not carry.
fn line_of(
    prefix: &ServedPrefix,
    id: DefId,
    def: &ds_check::Def,
    methods: &[(usize, &ds_check::MethodDecl)],
) -> Option<String> {
    let decls = &prefix.decls.decls;
    let item = match def.kind {
        // a bodied fn converts onto a const-arrow binding (F2) whose
        // signature the fns table carries beside the bodiless fn's; a
        // plain binding (no stored signature) renders no line below
        DefKind::Fn | DefKind::Const => {
            ItemKind::Fn(fn_decl(&def.name, def.span, decls.fns.get(&id)?))
        },
        DefKind::Struct => {
            let shape = decls.structs.get(&id)?;
            ItemKind::Struct(StructDecl {
                span: def.span,
                name: ident(&def.name, def.span),
                generics: type_params(&shape.params),
                // the artifact carries no supers; the source rendering
                // spells them (the boundary in the module doc)
                supers: Vec::new(),
                members: shape
                    .fields
                    .iter()
                    .cloned()
                    .map(StructMember::Field)
                    .collect(),
                has_body: true,
            })
        },
        DefKind::Enum => {
            let shape = decls.enums.get(&id)?;
            ItemKind::Enum(EnumDecl {
                span: def.span,
                name: ident(&def.name, def.span),
                generics: type_params(&shape.params),
                supers: Vec::new(),
                cases: shape
                    .cases
                    .iter()
                    .map(|(name, span, payload)| EnumCase {
                        span: *span,
                        name: ident(name, *span),
                        payload: payload.clone(),
                        default: None,
                    })
                    .collect(),
            })
        },
        DefKind::Tag => {
            // a payloaded tag's shape rides the structs table (the
            // minted payload case's own home), so the surface line
            // spells it back
            let payload = match decls.structs.get(&id) {
                Some(shape) => EnumPayload::Struct(shape.fields.clone()),
                None => EnumPayload::None,
            };
            ItemKind::Tag(TagDecl {
                span: def.span,
                name: ident(&def.name, def.span),
                payload,
            })
        },
        DefKind::Trait => ItemKind::Trait(trait_decl(prefix, &def.name, def.span, id, methods)?),
        // a binding's initializer, an alias's target, and an enum case's
        // own row stay outside the artifact's tables: no line here, the
        // source rendering keeps spelling them
        DefKind::Var | DefKind::Case | DefKind::TypeAlias => return None,
    };
    let mut line = String::new();
    ds_fmt::signature::item(&mut line, "", &Item {
        span: def.span,
        attrs: Vec::new(),
        // the surface gate already held the def to `pub`; the renderer
        // spells the prefix itself
        vis: None,
        kind: item,
        synthesized: false,
        leading_trivia: Vec::new(),
        doc: Vec::new(),
        trailing_trivia: Vec::new(),
    });
    let trimmed = line.trim_end();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// One stored signature as a bodiless declaration.
fn fn_decl(name: &str, span: Span, sig: &ds_check::FnSig) -> FnDecl {
    FnDecl {
        span,
        name: ident(name, span),
        generics: type_params(&sig.generics),
        params: sig
            .params
            .iter()
            .map(|param| Param {
                span: param.span,
                mode: param.mode,
                name: ident(&param.name, param.span),
                ty: param.ty.clone(),
                default: param.default.clone(),
            })
            .collect(),
        ret: sig.ret.clone(),
        bind: sig.bind.clone().map(|name| BindName { span, name }),
        body: None,
        doc: Vec::new(),
    }
}

/// One trait's declaration over the artifact's trait tables: the member
/// names in declaration order, each spelled through its stored
/// signature; an associated type stays outside the artifact's tables.
fn trait_decl(
    prefix: &ServedPrefix,
    name: &str,
    span: Span,
    id: DefId,
    methods: &[(usize, &ds_check::MethodDecl)],
) -> Option<TraitDecl> {
    let decls = &prefix.decls.decls;
    let names = decls.trait_members.get(&id)?;
    let mut members = Vec::new();
    for member in names {
        let sig = methods
            .iter()
            .find(|(_, method)| {
                method.name == *member
                    && matches!(method.owner, MethodOwner::Trait(owner) if owner == id)
            })
            .and_then(|(index, _)| {
                decls.members.get(&MethodId::from_raw(
                    u32::try_from(*index).expect("method count fits u32"),
                ))
            });
        let Some(sig) = sig else {
            continue;
        };
        members.push(TraitMember::Fn(Box::new(fn_decl(member, span, sig))));
    }
    Some(TraitDecl {
        span,
        name: ident(name, span),
        // the trait table carries the parameter names alone; a declared
        // bound stays outside the artifact (the boundary in the module
        // doc)
        generics: decls
            .trait_generics
            .get(&id)?
            .iter()
            .map(|param| TypeParam {
                span,
                name: ident(param, span),
                bound: None,
            })
            .collect(),
        supers: Vec::new(),
        members,
    })
}

fn type_params(params: &[ds_check::GenericParam]) -> Vec<TypeParam> {
    params
        .iter()
        .map(|param| TypeParam {
            span: param.span,
            name: ident(&param.name, param.span),
            bound: param.bound.clone(),
        })
        .collect()
}

fn ident(name: &str, span: Span) -> Ident {
    Ident {
        span,
        name: name.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ds_check::{DeclaredRows, Package, WorldCaps};
    use ds_diag::{FileId, Span};
    use ds_parse::{EnumPayload, FieldDecl, Ident};

    use super::{SurfaceBand, member_surface, signature_line, surface_lines};
    use crate::{
        DeclsData, DefData, EnumCaseData, EnumShapeData, FnSigData, GenericParamData,
        MethodDeclData, MethodOwnerData, PackageData, ParamSigData, PrefixDsi, StructShapeData,
        UniverseLog, artifact_bytes, decode_dsi, extract,
    };

    /// One clean compile's artifact over its sources, the same pipeline
    /// the ambient generation runs (no derives): parse, collect and
    /// resolve, the declaration-phase snapshot, the full analyze for the
    /// clean gate, the effects pass, and the method rows. The sources
    /// spell no ambient name (no `int`, no `string`): the conversion's
    /// real rows need no ambient vocabulary.
    fn served(sources: &[(&str, &str)]) -> super::ServedPrefix {
        let mut files = ds_diag::Files::new();
        let mut asts = Vec::new();
        let mut package = Package::new();
        let mut diags = Vec::new();
        let mut tables = Vec::new();
        let mut ids = Vec::new();
        for (name, text) in sources {
            let file = files.add_source(*name, text);
            let ast = ds_parse::parse(&files, file).expect("the source parses");
            diags.extend(package.add_file(&ast));
            let (uses, file_diags) = package.resolve(&ast);
            diags.extend(file_diags);
            tables.push(uses);
            ids.push(file);
            asts.push(ast);
        }
        assert!(diags.is_empty(), "the sources resolve clean: {diags:?}");
        let trees: Vec<Arc<ds_parse::Ast>> = asts.iter().cloned().map(Arc::new).collect();
        let refs: Vec<&ds_parse::Ast> = asts.iter().collect();
        let (decls, decl_diags) = ds_check::decls::build(&package, &refs, &tables);
        assert!(
            decl_diags.is_empty(),
            "the sources declare clean: {decl_diags:?}"
        );
        let analyzed = ds_check::analyze_resolved(
            &files,
            Arc::from(trees),
            Arc::new(package),
            tables.into(),
            diags,
            Arc::default(),
        );
        assert!(
            analyzed.checked.diags.is_empty(),
            "the sources check clean: {:?}",
            analyzed.checked.diags
        );
        let rows = DeclaredRows::new();
        let caps = WorldCaps::new();
        let effects = ds_check::effects::effects_of_resolved(
            &analyzed.asts[..],
            &analyzed.package,
            &analyzed.resolutions[..],
            &analyzed.checked.hole_show_dispatches,
            &rows,
            &caps,
        );
        let method_rows = ds_check::effects::solved_method_rows(
            &analyzed.asts[..],
            &analyzed.package,
            &analyzed.resolutions[..],
            &rows,
            &caps,
        );
        let payload = extract(&decls, &analyzed, &effects, &method_rows);
        // a single-member package's identity is its source hash, the
        // same key the ambient and the cache tiers fold
        let hash = ds_mod::Hash128::compute(sources[0].1.as_bytes());
        decode_dsi(&artifact_bytes(hash, &payload), hash, &ids).expect("the artifact serves")
    }

    /// The source rendering over the same sources, the document the
    /// conversion must answer for the kinds the artifact carries.
    fn rendered(sources: &[(&str, &str)]) -> String {
        let mut out = String::new();
        for (name, text) in sources {
            let mut files = ds_diag::Files::new();
            let file = files.add_source(*name, text);
            let ast = ds_parse::parse(&files, file).expect("the source parses");
            ds_fmt::signature::member(&mut out, text, &ast);
        }
        out
    }

    /// The conversion answers the source rendering for the vocabulary
    /// that carries through extraction without the ambient's names — a
    /// locally declared type stands in for the ambient atoms — the proof
    /// no second rendering was forked.
    #[test]
    fn the_surface_answers_the_source_rendering() {
        const SRC: &str = concat!(
            "struct carrier { }\n",
            "fn hidden(c: carrier) -> carrier {\nc\n}\n",
            "pub(pkg) fn internal(c: carrier) -> carrier {\nc\n}\n",
            "pub fn id(c: carrier) -> carrier {\nc\n}\n",
            "pub enum tone {\nquiet\nloud\n}\n",
            "pub tag unit\n",
        );
        let prefix = served(&[("lib.ds", SRC)]);
        let surface = member_surface(&prefix, SurfaceBand::default());
        assert_eq!(surface, rendered(&[("lib.ds", SRC)]), "one rendering");
        assert_eq!(
            surface,
            concat!(
                "pub fn id(c: carrier) -> carrier\n",
                "pub enum tone { quiet, loud }\n",
                "pub tag unit\n",
            ),
            "the carried vocabulary's lines: {surface}"
        );
    }

    /// The band holds the surface to the package's own members: a prefix
    /// serving a leading ambient-band file renders only the own files'
    /// exports, the tiers (`pub(pkg)` and private) stay out everywhere,
    /// and the hover tier answers one name.
    #[test]
    fn the_band_and_tiers_filter() {
        const AMBIENT_BAND: &str = concat!(
            "pub struct carrier { }\n",
            "pub fn ambient_face(c: carrier) -> carrier {\nc\n}\n",
        );
        const OWN: &str = concat!(
            "pub fn pick(c: carrier) -> carrier {\nc\n}\n",
            "pub(pkg) fn package_only(c: carrier) -> carrier {\nc\n}\n",
            "fn private_use(c: carrier) -> carrier {\nc\n}\n",
        );
        let prefix = served(&[("ambient.ds", AMBIENT_BAND), ("lib.ds", OWN)]);
        let band = SurfaceBand {
            leading: 1,
            trailing: 0,
        };
        assert_eq!(
            surface_lines(&prefix, band),
            vec![(
                "pick".to_owned(),
                "pub fn pick(c: carrier) -> carrier".to_owned()
            )],
            "the own band's pub surface alone"
        );
        assert_eq!(
            surface_lines(&prefix, SurfaceBand::default()).len(),
            3,
            "without the band the ambient file's exports render too"
        );
        assert_eq!(
            signature_line(&prefix, band, "pick").as_deref(),
            Some("pub fn pick(c: carrier) -> carrier"),
            "the hover tier answers the name"
        );
        for outside in ["ambient_face", "carrier", "package_only", "private_use"] {
            assert!(
                signature_line(&prefix, band, outside).is_none(),
                "`{outside}` answers nothing"
            );
        }
    }

    /// A nominal type tree for the crafted rows: the conversion renders
    /// written types as trees, so the rows need no resolution.
    fn ty(name: &str) -> ds_parse::Type {
        ds_parse::Type::Path {
            span: Span::new(FileId::new(0), 0, 0),
            path: ds_parse::Path {
                span: Span::new(FileId::new(0), 0, 0),
                segments: vec![Ident {
                    span: Span::new(FileId::new(0), 0, 0),
                    name: name.to_owned(),
                }],
            },
            args: Vec::new(),
        }
    }

    fn param(name: &str, ty: Option<ds_parse::Type>) -> ParamSigData {
        ParamSigData {
            name: name.to_owned(),
            span: Span::new(FileId::new(0), 0, 0),
            mode: ds_parse::ParamMode::Borrow,
            ty,
            default: None,
        }
    }

    fn generic(name: &str) -> GenericParamData {
        GenericParamData {
            name: name.to_owned(),
            span: Span::new(FileId::new(0), 0, 0),
            bound: None,
        }
    }

    fn empty_decls() -> DeclsData {
        DeclsData {
            atoms: Vec::new(),
            defs_of_atoms: Vec::new(),
            traits: Vec::new(),
            structs: Vec::new(),
            enums: Vec::new(),
            cases: Vec::new(),
            fns: Vec::new(),
            members: Vec::new(),
            inherent: Vec::new(),
            trait_members: Vec::new(),
            trait_member_defaults: Vec::new(),
            trait_generics: Vec::new(),
            instance_impls: Vec::new(),
            inherent_impls: Vec::new(),
        }
    }

    /// One crafted artifact over the typed vocabulary the real rows
    /// cannot spell without the ambient: a typed generic fn with a `mut`
    /// parameter, a generic struct, a four-case enum, a trait, and the
    /// kinds that render no line.
    fn crafted() -> PrefixDsi {
        let span = Span::new(FileId::new(0), 0, 0);
        let mut decls = empty_decls();
        decls.fns = vec![(0, FnSigData {
            generics: vec![generic("t")],
            params: vec![
                ParamSigData {
                    mode: ds_parse::ParamMode::Mut(span),
                    ..param("xs", Some(ty("list")))
                },
                param("seed", None),
            ],
            ret: Some(ty("int")),
            bind: None,
        })];
        decls.structs = vec![(2, StructShapeData {
            params: vec![generic("t")],
            fields: vec![
                FieldDecl {
                    span,
                    name: Ident {
                        span,
                        name: "n".to_owned(),
                    },
                    ty: ty("t"),
                    doc: Vec::new(),
                },
                FieldDecl {
                    span,
                    name: Ident {
                        span,
                        name: "items".to_owned(),
                    },
                    ty: ty("list"),
                    doc: Vec::new(),
                },
            ],
            linear: false,
        })];
        decls.enums = vec![(3, EnumShapeData {
            params: Vec::new(),
            cases: vec![
                EnumCaseData {
                    name: "quiet".to_owned(),
                    span,
                    payload: EnumPayload::None,
                },
                EnumCaseData {
                    name: "loud".to_owned(),
                    span,
                    payload: EnumPayload::None,
                },
                EnumCaseData {
                    name: "payload".to_owned(),
                    span,
                    payload: EnumPayload::Tuple(vec![ty("int"), ty("string")]),
                },
                EnumCaseData {
                    name: "shaped".to_owned(),
                    span,
                    payload: EnumPayload::Struct(vec![FieldDecl {
                        span,
                        name: Ident {
                            span,
                            name: "code".to_owned(),
                        },
                        ty: ty("int"),
                        doc: Vec::new(),
                    }]),
                },
            ],
            linear: false,
        })];
        decls.trait_members = vec![(6, vec!["speak".to_owned()])];
        decls.trait_generics = vec![(6, Vec::new())];
        decls.members = vec![(0, FnSigData {
            generics: Vec::new(),
            params: vec![param("self", None)],
            ret: Some(ty("string")),
            bind: None,
        })];
        let def = |name: &str, kind: u8, vis: u8| DefData {
            name: name.to_owned(),
            kind,
            vis,
            span,
        };
        PrefixDsi {
            names: Vec::new(),
            package: PackageData {
                files: vec![FileId::new(0)],
                // fn=0, const=1, struct=3, enum=4, tag=5, case=6,
                // trait=7, alias=8; private=0, pkg=1, pub=2
                defs: vec![
                    def("open_api", 0, 2),
                    def("answer", 1, 2),
                    def("rec", 3, 2),
                    def("tone", 4, 2),
                    def("unit", 5, 2),
                    def("quiet", 6, 2),
                    def("speak", 7, 2),
                    def("id", 8, 2),
                    def("internal", 0, 1),
                ],
                type_names: Vec::new(),
                value_names: Vec::new(),
                methods: vec![MethodDeclData {
                    name: "speak".to_owned(),
                    owner: MethodOwnerData::Trait(6),
                    span,
                }],
                method_index: vec![("speak".to_owned(), vec![0])],
            },
            uses: vec![Vec::new()],
            universe: UniverseLog {
                atoms: Vec::new(),
                traits: Vec::new(),
                memberships: Vec::new(),
                mints: Vec::new(),
                sets: Vec::new(),
                impl_args: Vec::new(),
                inherent_bounds: Vec::new(),
                var_counter: 0,
            },
            decls,
            instances: Vec::new(),
            effects: Vec::new(),
            method_rows: Vec::new(),
        }
    }

    /// The typed vocabulary renders exactly as the source rendering
    /// spells it, and the kinds outside the artifact's tables (a
    /// binding, an alias, an enum case's own row) answer no line.
    #[test]
    fn the_typed_vocabulary_renders_through_the_shared_renderer() {
        let artifact = crafted();
        let hash = ds_mod::Hash128::compute(b"crafted");
        let prefix = decode_dsi(&artifact_bytes(hash, &artifact), hash, &[FileId::new(0)])
            .expect("the crafted artifact serves");
        assert_eq!(
            member_surface(&prefix, SurfaceBand::default()),
            concat!(
                "pub fn open_api[t](mut xs: list, seed) -> int\n",
                "pub struct rec[t] { n: t, items: list }\n",
                "pub enum tone {\n",
                "  quiet\n",
                "  loud\n",
                "  payload(int, string)\n",
                "  shaped { code: int }\n",
                "}\n",
                "pub tag unit\n",
                "pub trait speak { fn speak(self) -> string }\n",
            ),
            "the carried vocabulary's lines"
        );
        for absent in ["answer", "id", "quiet", "internal"] {
            assert!(
                signature_line(&prefix, SurfaceBand::default(), absent).is_none(),
                "`{absent}` renders no line"
            );
        }
    }
}
