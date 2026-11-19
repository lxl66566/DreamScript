//! The shared grammar-based source generator for the checker fuzz leg:
//! fuzz bytes drive a structurally valid DS program — declarations (enums
//! with bare/payload cases, structs, tags, type aliases, traits, impls,
//! extern signatures, consts over lambdas, fns), statements (annotated and
//! inferred bindings, destructuring, compound assignment, loops over
//! ranges, narrowing ifs, matches with or-patterns and guards, `return
//! e?`), and expressions biased to the well-typed surface (calls including
//! UFCS and trailing-lambda blocks, lambdas, list/map/tuple literals and
//! constructors, string interpolation, `is` narrowing, `?` propagation and
//! the extraction family), with occasional semantically wrong splices so
//! the checker's error paths stay exercised too. The scalar tags, the
//! list/map generic structs, and every callee are declared by the program
//! itself, so well-typed mode reaches inference over unions, refinement,
//! and linearity instead of drowning in unknown-name noise. The span
//! rework's landed surfaces are drawn vocabulary: record-typed list
//! elements (the per-list element stride), unions over wider members (a
//! multi-word struct member, or a heap-ref generic application's one Ref
//! word), and struct-typed fields, each counted in the draw census the
//! mirror asserts. The pattern rounds' landed vocabulary is drawn the
//! same way: value-level or-patterns (literal ladders, struct-pattern
//! alternatives), struct patterns over struct-typed scrutinees, case
//! constructor arms with payload tests, guarded arms over union and
//! enum dispatch, nested struct sub-patterns over struct-typed payloads
//! and fields, and whole-span equality over union operands, each counted
//! in the census. The CALL_GENERIC round's landed surface is drawn the
//! same way: free generic fn calls over declared bounds (the int,
//! string, and list instantiation chains, the two-level
//! list[list[int]] instantiation, the nested runtime form whose
//! callee demand rides the calling frame's own staged slot), the
//! honest doors (the lone nested demand the frame never staged,
//! the mut-over-variable planning parameter, the generic fn value,
//! the generic fn name staged where a callable word must go, the
//! local generic lambda, the multi-word argument into a variable
//! parameter, the inert uncalled declaration), and the
//! impl-completeness shapes (an impl omitting a bodiless
//! non-defaulted member or spelling it bodiless reports DS2023
//! at the impl's own door, while an omitted defaulted member
//! stays clean), each counted in the census. Every program
//! runs through parse, the effects walk over rows the generator picked,
//! the bidirectional check with the affine pass, and both diag renderers;
//! the property is that none of this panics. The json derive families'
//! attribute grammar rides the generated declarations (the census's
//! structural carrier a from_json attribute over the guaranteed
//! nested-carrier struct, occasional attributes over the other structs
//! and enums), and the import-row vocabulary names const- and type-shaped
//! members — the linked facts' import-surface spelling, parsed and
//! checked here; the link walk itself needs a package store this
//! single-file harness does not carry. Included by the cargo-fuzz
//! target `fuzz_check_gen` and the nextest mirror in
//! crates/ds-check/tests/fuzz_check_gen.rs.
//!
//! Termination and size: a shared node budget (a few hundred) plus
//! expression depth caps bound every program to a couple hundred lines,
//! so each input checks in bounded time; the byte stream cycles through
//! the fuzz input, so any input (including empty) yields a full program
//! and any mutation shifts the program deterministically.

use std::hint::black_box;

use ds_check::{DeclaredRows, Effect, EffectRow, WorldCaps, check, effects_of};
use ds_diag::{Files, render};
use ds_parse::parse;

/// The ambient-name vocabulary the generator may call without a
/// declaration: unknown to the checker (diagnostics), but declared as
/// effect rows below, so the effects walk crosses real union edges.
const AMBIENT: [&str; 12] = [
    "read_file",
    "parse_json",
    "log",
    "fetch",
    "cancel",
    "die",
    "close",
    "str",
    "head",
    "tail",
    "cons",
    "open",
];
const EFFECTS: [Effect; 5] = [
    Effect::Error,
    Effect::Trap,
    Effect::World,
    Effect::Cancel,
    Effect::Discontinue,
];

/// The per-run outcome, for the mirror's coverage assertions.
pub struct Outcome {
    pub parsed: bool,
    pub moves: usize,
    pub effects: usize,
    pub drawn: Drawn,
}

/// The span-surface draw census: the shapes the generator drew over the
/// span rework's landed surfaces, counted wherever the vocabulary picks
/// them (generation runs regardless of parse, so the census is the
/// generator's own fact).
#[derive(Default)]
pub struct Drawn {
    /// List types over record elements (a declared struct or enum at the
    /// per-list element stride).
    pub record_lists: u32,
    /// Unions carrying a wider member (a declared struct).
    pub wide_unions: u32,
    /// Struct-typed or list-typed struct fields (the field-span shape).
    pub span_fields: u32,
    /// Unions carrying a heap-ref generic application member (a
    /// `list[int]` member riding one Ref word).
    pub heap_ref_unions: u32,
    /// Value-level or-patterns: literal ladders over scalar scrutinees
    /// and struct-pattern alternative arms.
    pub or_patterns: u32,
    /// Struct-pattern arms over struct-typed scrutinees (literal field
    /// tests in place, punning binders).
    pub struct_patterns: u32,
    /// Case constructor arms with literal payload tests (a case's tuple
    /// argument or struct field tested).
    pub payload_tests: u32,
    /// Value-comparison guards over union and enum dispatch arms.
    pub union_guards: u32,
    /// Nested struct sub-patterns over struct-typed payloads and fields.
    pub nested_sub_patterns: u32,
    /// Whole-span equality conditions over union operands (a member
    /// value against the union).
    pub union_equality: u32,
    /// Free generic fn call sites over declared bounds (the
    /// instantiation chains).
    pub generic_calls: u32,
    /// Serving impl bodies the story's evidence chains resolve
    /// through.
    pub generic_impls: u32,
    /// Nested generic callee bodies riding the calling frame's own
    /// staged slot (the coinciding runtime form).
    pub generic_runtime_forms: u32,
    /// The honest door shapes drawn: the lone nested demand, the
    /// mut-over-variable planning parameter, the generic fn
    /// value, the generic fn name staged where a callable word
    /// must go, the local generic lambda, the multi-word
    /// argument into a variable parameter.
    pub generic_doors: u32,
    /// Declared-but-uncalled free generic fns (the inert ONE
    /// body).
    pub inert_generics: u32,
    /// Incomplete impls reporting DS2023 at the impl's own door
    /// (the member omitted or spelled bodiless).
    pub incomplete_impls: u32,
    /// Defaulted members omitted by an impl (the clean shape).
    pub omitted_defaults: u32,
    /// Two-level instantiations (list[list[int]]).
    pub two_level_calls: u32,
    /// Derive attributes over the json families emitted above generated
    /// struct and enum declarations (the parse-and-check carrier; the
    /// expansion itself lives behind the analyze entries the source
    /// pipeline owns).
    pub derive_attrs: u32,
    /// Import rows naming const- and type-shaped members (the linked
    /// facts' import-surface spelling; the link walk itself needs a
    /// package store this single-file harness does not carry).
    pub import_members: u32,
}

/// One generated program plus the effect rows declared around it.
pub struct Generated {
    pub source: String,
    pub rows: Vec<(String, EffectRow)>,
    pub drawn: Drawn,
}

/// Generates the program for one input and runs the whole checker
/// pipeline over it, rendering every diagnostic through both renderers.
pub fn exercise(data: &[u8]) -> Outcome {
    let program = generate(data);
    let mut files = Files::new();
    let file = files.add_source("gen.ds", &program.source);
    let ast = match parse(&files, file) {
        Ok(ast) => ast,
        Err(diags) => {
            for diag in &diags {
                black_box(render::human(diag, &files));
            }
            return Outcome {
                parsed: false,
                moves: 0,
                effects: 0,
                drawn: program.drawn,
            };
        },
    };
    let mut declared = DeclaredRows::new();
    for (name, row) in &program.rows {
        declared = declared.with(name, *row);
    }
    let effects = effects_of(&[&ast], &declared, &WorldCaps::new());
    let (checked, linearity) = check(&files, &[ast]);
    // both renderers run over real pipeline output; the per-program cap
    // keeps heavily diagnostic programs bounded in the debug profile
    for diag in checked.diags.iter().take(8) {
        black_box(render::human(diag, &files));
        black_box(render::to_json(diag, &files));
    }
    black_box((
        checked.diags.len(),
        linearity.moves.len(),
        effects.def_count(),
    ));
    Outcome {
        parsed: true,
        moves: linearity.moves.len(),
        effects: effects.def_count(),
        drawn: program.drawn,
    }
}

/// Byte stream over the fuzz input: cycles through the data, so any
/// input yields a full program and any mutation shifts the generated
/// program deterministically.
pub struct Stream<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Stream<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn byte(&mut self) -> u8 {
        if self.data.is_empty() {
            return 0;
        }
        let b = self.data[self.pos % self.data.len()];
        self.pos = self.pos.wrapping_add(1);
        b
    }

    fn below(&mut self, bound: usize) -> usize {
        usize::from(self.byte()) % bound.max(1)
    }

    fn roll(&mut self, one_in: usize) -> bool {
        self.below(one_in.max(1)) == 0
    }
}

/// The type vocabulary the generator tracks while building a program.
/// Unions are kept flat (members are atoms); a member is an enum case.
#[derive(Clone)]
enum Ty {
    Int,
    Float,
    Str,
    Bool,
    Char,
    Byte,
    None,
    Unit,
    List(Box<Ty>),
    Map(Box<Ty>, Box<Ty>),
    Tuple(Vec<Ty>),
    Struct(usize),
    Enum(usize),
    Member(usize, usize),
    Tag(usize),
    Union(Vec<Ty>),
    Fn(Vec<Ty>, Box<Ty>),
}

#[derive(Clone)]
struct Local {
    name: String,
    ty: Ty,
    mutable: bool,
}

#[derive(Clone)]
struct StructDecl {
    name: String,
    fields: Vec<(String, Ty)>,
    generic: bool,
    copyable: bool,
}

#[derive(Clone)]
enum Payload {
    Tuple(Vec<Ty>),
    Fields(Vec<(String, Ty)>),
}

#[derive(Clone)]
struct CaseDecl {
    name: String,
    payload: Option<Payload>,
}

struct EnumDecl {
    name: String,
    cases: Vec<CaseDecl>,
}

#[derive(Clone)]
struct Param {
    name: String,
    ty: Ty,
    default: Option<String>,
}

#[derive(Clone)]
struct FnDecl {
    name: String,
    params: Vec<Param>,
    ret: Option<Ty>,
}

/// One declared free generic fn call sites may name.
#[derive(Clone)]
struct GenericFn {
    name: String,
    arity: usize,
    /// True when the fn declares a bound the argument's
    /// instantiation must satisfy; a boundless fn accepts any
    /// instantiation.
    bounded: bool,
}

/// Builds one program from one input.
struct Gen<'a> {
    s: Stream<'a>,
    out: String,
    indent: usize,
    budget: u32,
    structs: Vec<StructDecl>,
    enums: Vec<EnumDecl>,
    tags: Vec<String>,
    fns: Vec<FnDecl>,
    consts: Vec<(String, Ty)>,
    /// Trait and inherent-impl methods: (name, receiver type, return).
    methods: Vec<(String, Ty, Ty)>,
    /// The declared free generic fns call sites may name.
    generics: Vec<GenericFn>,
    /// The declared fn taking a fn-typed parameter (the generic
    /// fn argument door's driver).
    applier: String,
    locals: Vec<Local>,
    loop_depth: u32,
    nest: u32,
    n: usize,
    ret: Option<Ty>,
    rows: Vec<(String, EffectRow)>,
    drawn: Drawn,
}

pub fn generate(data: &[u8]) -> Generated {
    let mut g = Gen {
        s: Stream::new(data),
        out: String::new(),
        indent: 0,
        budget: 420,
        structs: Vec::new(),
        enums: Vec::new(),
        tags: Vec::new(),
        fns: Vec::new(),
        consts: Vec::new(),
        methods: Vec::new(),
        generics: Vec::new(),
        applier: String::new(),
        locals: Vec::new(),
        loop_depth: 0,
        nest: 0,
        n: 0,
        ret: None,
        rows: Vec::new(),
        drawn: Drawn::default(),
    };
    g.module();
    Generated {
        source: g.out,
        rows: g.rows,
        drawn: g.drawn,
    }
}

impl Gen<'_> {
    /// One fresh name per call; the shared counter keeps every prefix
    /// (locals, fields, cases, fns) pairwise distinct.
    fn fresh(&mut self, prefix: &str) -> String {
        self.n += 1;
        format!("{prefix}{}", self.n)
    }

    fn line(&mut self, text: &str) {
        self.out.push_str(&"  ".repeat(self.indent));
        self.out.push_str(text);
        self.out.push('\n');
    }

    /// The rendered type spelling used in annotations and type positions.
    fn render_ty(&self, ty: &Ty) -> String {
        match ty {
            Ty::Int => "int".into(),
            Ty::Float => "float".into(),
            Ty::Str => "string".into(),
            Ty::Bool => "bool".into(),
            Ty::Char => "char".into(),
            Ty::Byte => "byte".into(),
            Ty::None => "none".into(),
            Ty::Unit => "()".into(),
            Ty::List(el) => format!("list[{}]", self.render_ty(el)),
            Ty::Map(k, v) => format!("map[{}, {}]", self.render_ty(k), self.render_ty(v)),
            Ty::Tuple(ts) => format!(
                "({})",
                ts.iter()
                    .map(|t| self.render_ty(t))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Ty::Struct(i) => self.structs[*i].name.clone(),
            Ty::Enum(i) => self.enums[*i].name.clone(),
            Ty::Member(e, c) => self.enums[*e].cases[*c].name.clone(),
            Ty::Tag(i) => self.tags[*i].clone(),
            Ty::Union(ms) => ms
                .iter()
                .map(|m| self.render_ty(m))
                .collect::<Vec<_>>()
                .join(" | "),
            Ty::Fn(ps, r) => format!(
                "fn({}) -> {}",
                ps.iter()
                    .map(|p| self.render_ty(p))
                    .collect::<Vec<_>>()
                    .join(", "),
                self.render_ty(r)
            ),
        }
    }

    /// The atom set a type denotes: unions flatten; every other type is
    /// one atom keyed by its rendering. Atom renderings are unique, so
    /// string sets are a faithful subset witness here.
    fn atom_keys(&self, ty: &Ty) -> Vec<String> {
        match ty {
            Ty::Union(ms) => ms.iter().flat_map(|m| self.atom_keys(m)).collect(),
            other => vec![self.render_ty(other)],
        }
    }

    /// True when `sub`'s atom set is non-empty and contained in `sup`'s.
    fn contained(&self, sub: &Ty, sup: &Ty) -> bool {
        let sub_keys = self.atom_keys(sub);
        !sub_keys.is_empty() && sub_keys.iter().all(|k| self.atom_keys(sup).contains(k))
    }

    /// A union of 2-3 distinct atoms drawn from scalars, members, wider
    /// members (a declared struct — the span rework's multi-word member),
    /// and tags; falls back to the optional shape when picks collide.
    fn pick_union(&mut self) -> Ty {
        let mut members: Vec<Ty> = Vec::new();
        let mut heap_ref = false;
        for _ in 0..2 + self.s.below(2) {
            let pick = match self.s.below(9) {
                0 => Ty::Float,
                1 => Ty::Str,
                2 => Ty::Bool,
                3 => Ty::None,
                // two member slots: unions carrying enum cases are the
                // dispatch-arm vocabulary's carrier shape
                4 | 8 if !self.enums.is_empty() => {
                    let e = self.s.below(self.enums.len());
                    let c = self.s.below(self.enums[e].cases.len());
                    Ty::Member(e, c)
                },
                5 => self.pickable_struct().map_or(Ty::Int, Ty::Struct),
                6 if !self.tags.is_empty() => Ty::Tag(self.s.below(self.tags.len())),
                // the heap-ref generic application member: one Ref word
                7 => {
                    heap_ref = true;
                    Ty::List(Box::new(Ty::Int))
                },
                _ => Ty::Int,
            };
            let key = self.render_ty(&pick);
            if !members.iter().any(|m: &Ty| self.render_ty(m) == key) {
                members.push(pick);
            }
        }
        if members.len() < 2 {
            members = vec![Ty::Int, Ty::None];
        }
        if members.iter().any(|m| matches!(m, Ty::Struct(_))) {
            self.drawn.wide_unions += 1;
        }
        if heap_ref && members.iter().any(|m| matches!(m, Ty::List(_))) {
            self.drawn.heap_ref_unions += 1;
        }
        Ty::Union(members)
    }

    /// A random already-declared non-generic struct index, when one
    /// exists.
    fn pickable_struct(&mut self) -> Option<usize> {
        let pickable: Vec<usize> = self
            .structs
            .iter()
            .enumerate()
            .filter(|(_, d)| !d.generic)
            .map(|(i, _)| i)
            .collect();
        if pickable.is_empty() { None } else { Some(pickable[self.s.below(pickable.len())]) }
    }

    /// A list element type: scalars mostly, records (a declared struct
    /// or enum — the record-typed element stride) sometimes.
    fn pick_element(&mut self) -> Ty {
        match self.s.below(6) {
            3 => Ty::Str,
            4 | 5 => match self.pickable_struct() {
                Some(i) => Ty::Struct(i),
                None if !self.enums.is_empty() => Ty::Enum(self.s.below(self.enums.len())),
                None => Ty::Int,
            },
            _ => Ty::Int,
        }
    }

    /// A scalar-ish type for fields, payloads, and small elements.
    fn pick_base(&mut self) -> Ty {
        match self.s.below(12) {
            0..=3 => Ty::Int,
            4 | 5 => Ty::Str,
            6 => Ty::Float,
            7 => Ty::Bool,
            8 => Ty::Char,
            9 => Ty::Byte,
            10 if !self.enums.is_empty() => {
                let e = self.s.below(self.enums.len());
                let c = self.s.below(self.enums[e].cases.len());
                Ty::Member(e, c)
            },
            _ => Ty::None,
        }
    }

    /// A struct field's type: scalars mostly, a struct-typed or
    /// list-typed field sometimes (the span rework's field-span shape).
    /// A field names an already-declared struct only, so a struct
    /// literal's field recursion strictly descends and stays finite.
    fn pick_field_ty(&mut self) -> Ty {
        match self.s.below(12) {
            0..=8 => self.pick_base(),
            9 | 10 => match self.pickable_struct() {
                Some(i) => {
                    self.drawn.span_fields += 1;
                    Ty::Struct(i)
                },
                None => self.pick_base(),
            },
            _ => {
                let el = self.pick_element();
                self.drawn.span_fields += 1;
                Ty::List(Box::new(el))
            },
        }
    }

    /// A full type for parameters, bindings, and returns.
    fn pick_ty(&mut self) -> Ty {
        match self.s.below(24) {
            0..=7 => Ty::Int,
            8..=10 => Ty::Str,
            11 => Ty::Float,
            12 => Ty::Bool,
            13 => Ty::Char,
            14 => Ty::Byte,
            15 | 16 => self.pick_union(),
            17 => {
                let el = self.pick_element();
                if matches!(el, Ty::Struct(_) | Ty::Enum(_)) {
                    self.drawn.record_lists += 1;
                }
                Ty::List(Box::new(el))
            },
            18 => Ty::Map(Box::new(Ty::Str), Box::new(Ty::Int)),
            19 => {
                let pickable: Vec<usize> = self
                    .structs
                    .iter()
                    .enumerate()
                    .filter(|(_, d)| !d.generic)
                    .map(|(i, _)| i)
                    .collect();
                if pickable.is_empty() {
                    Ty::Int
                } else {
                    Ty::Struct(pickable[self.s.below(pickable.len())])
                }
            },
            20 | 21 => {
                if self.enums.is_empty() {
                    Ty::Int
                } else {
                    Ty::Enum(self.s.below(self.enums.len()))
                }
            },
            22 => Ty::Tuple(vec![Ty::Int, self.pick_base()]),
            _ => Ty::Fn(vec![Ty::Int], Box::new(Ty::Int)),
        }
    }

    /// The whole program: prelude, declarations, then fn bodies.
    fn module(&mut self) {
        // the import-row vocabulary: the plain callable member beside the
        // linked-facts spellings — const members riding the surface as
        // plain-literal names, struct types as member names (the link
        // walk's registration vocabulary, parsed and checked here)
        match self.s.below(18) {
            0 => self.line("import @std/io (get)"),
            1 => {
                self.line("import @facts (limit, step, greeting, ratio, ready, initial, code)");
                self.drawn.import_members += 1;
            },
            2 => {
                self.line("import @geom (cell, pair)");
                self.drawn.import_members += 1;
            },
            _ => {},
        }
        self.prelude();
        self.gen_structs();
        self.gen_enums();
        self.gen_tags();
        self.gen_aliases();
        self.gen_traits();
        self.gen_generic_calls();
        if self.s.roll(3) {
            self.gen_extern();
        }
        self.gen_consts();
        if self.s.roll(6) {
            self.line("## generated section");
        }
        self.gen_fns();
        self.pick_rows();
    }

    /// Scalars are declared data in the checker: declaring them keeps
    /// literals and arithmetic checking clean instead of reporting
    /// unknown types on every line. `list`/`map` are the generic record
    /// spellings the corpus uses.
    fn prelude(&mut self) {
        for name in [
            "int", "float", "string", "bool", "char", "byte", "i64", "u32", "i8",
        ] {
            self.line(&format!("tag {name}"));
        }
        self.line("struct list[t]");
        self.line("struct map[k, v]");
    }

    fn gen_structs(&mut self) {
        let count = 1 + self.s.below(2);
        for _ in 0..count {
            let name = self.fresh("s");
            let generic = self.s.roll(4);
            let copyable = self.s.roll(2);
            let mut fields = Vec::new();
            if generic {
                // the polymorphic container shape: one field over the
                // type parameter; values are never generated for it
                fields.push(("items".to_owned(), Ty::List(Box::new(Ty::Int))));
            } else {
                for _ in 0..=self.s.below(3) {
                    fields.push((self.fresh("f"), self.pick_field_ty()));
                }
            }
            let supers = if copyable { ": ~Copyable" } else { "" };
            let generics = if generic { "[t]" } else { "" };
            // the json derive families' attribute grammar over generated
            // declarations (the families rule structs and enums; the
            // generic spelling stays attribute-free)
            if !generic && self.s.roll(3) {
                let attr = match self.s.below(3) {
                    0 => "from_json",
                    1 => "to_json",
                    _ => "from_json, to_json",
                };
                self.line(&format!("#[derive({attr})]"));
                self.drawn.derive_attrs += 1;
            }
            self.line(&format!("struct {name}{generics}{supers} {{"));
            self.indent += 1;
            for (fname, ty) in &fields {
                let spelling = if generic { "list[t]".to_owned() } else { self.render_ty(ty) };
                self.line(&format!("{fname}: {spelling}"));
            }
            if copyable && self.s.roll(3) && !fields.is_empty() {
                let field = fields[0].0.clone();
                self.line("deinit {");
                self.indent += 1;
                self.line(&format!("close(self.{field})"));
                self.indent -= 1;
                self.line("}");
            }
            self.indent -= 1;
            self.line("}");
            self.structs.push(StructDecl {
                name,
                fields,
                generic,
                copyable,
            });
        }
        // the nested-carrier struct: one field over the first declared
        // plain struct, so the nested sub-pattern vocabulary has a
        // guaranteed carrier shape in every program
        if let Some(i) = self.structs.iter().position(|d| !d.generic) {
            let name = self.fresh("s");
            let target = self.structs[i].name.clone();
            let mut fields = vec![(self.fresh("f"), Ty::Struct(i))];
            for _ in 0..self.s.below(2) {
                fields.push((self.fresh("f"), self.pick_base()));
            }
            // the census's structural carrier: this struct exists in
            // every program, so its attribute always draws
            self.line("#[derive(from_json)]");
            self.drawn.derive_attrs += 1;
            self.line(&format!("struct {name} {{"));
            self.indent += 1;
            for (fname, ty) in &fields {
                self.line(&format!("{fname}: {}", self.render_ty(ty)));
            }
            self.indent -= 1;
            self.line("}");
            self.structs.push(StructDecl {
                name,
                fields,
                generic: false,
                copyable: false,
            });
            let _ = target;
        }
    }

    fn gen_enums(&mut self) {
        let count = self.s.below(3);
        for _ in 0..count {
            let name = self.fresh("e");
            let mut cases = Vec::new();
            for _ in 0..=self.s.below(2) {
                cases.push(CaseDecl {
                    name: self.fresh("c"),
                    payload: None,
                });
            }
            for k in 0..=self.s.below(2) {
                let payload = if k == 0 {
                    // the guaranteed Int tuple payload: the dispatch
                    // arm vocabulary's primary carrier
                    Payload::Tuple(vec![Ty::Int])
                } else {
                    match self.s.below(4) {
                        0 => Payload::Tuple(vec![self.pick_base()]),
                        2 if self.structs.iter().any(|d| !d.generic) => {
                            let pickable: Vec<usize> = self
                                .structs
                                .iter()
                                .enumerate()
                                .filter(|(_, d)| !d.generic)
                                .map(|(k, _)| k)
                                .collect();
                            let s = pickable[self.s.below(pickable.len())];
                            Payload::Tuple(vec![Ty::Struct(s)])
                        },
                        _ => Payload::Fields(vec![(self.fresh("f"), self.pick_base())]),
                    }
                };
                cases.push(CaseDecl {
                    name: self.fresh("c"),
                    payload: Some(payload),
                });
            }
            if self.s.roll(3) {
                let attr = match self.s.below(2) {
                    0 => "to_json",
                    _ => "from_json",
                };
                self.line(&format!("#[derive({attr})]"));
                self.drawn.derive_attrs += 1;
            }
            self.line(&format!("enum {name} {{"));
            self.indent += 1;
            for case in &cases {
                match &case.payload {
                    None => self.line(&case.name),
                    Some(Payload::Tuple(ts)) => self.line(&format!(
                        "{}({})",
                        case.name,
                        ts.iter()
                            .map(|t| self.render_ty(t))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                    Some(Payload::Fields(fs)) => self.line(&format!(
                        "{} {{ {} }}",
                        case.name,
                        fs.iter()
                            .map(|(f, t)| format!("{f}: {}", self.render_ty(t)))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                }
            }
            self.indent -= 1;
            self.line("}");
            self.enums.push(EnumDecl { name, cases });
        }
    }

    fn gen_tags(&mut self) {
        for _ in 0..self.s.below(4) {
            let name = self.fresh("t");
            self.line(&format!("tag {name}"));
            self.tags.push(name);
        }
    }

    fn gen_aliases(&mut self) {
        for _ in 0..self.s.below(3) {
            let name = self.fresh("a");
            let union = self.pick_union();
            let spelling = self.render_ty(&union);
            self.line(&format!("type {name} = {spelling}"));
        }
    }

    /// A trait with one method plus impls over scalar heads — including
    /// an overlapping `int | bool` head (the instance-overlap path) and a
    /// generic list head — and one inherent impl over a declared struct.
    /// The impl'd methods register for dot-calls in fn bodies.
    fn gen_traits(&mut self) {
        let pickable: Vec<usize> = self
            .structs
            .iter()
            .enumerate()
            .filter(|(_, d)| !d.generic)
            .map(|(i, _)| i)
            .collect();
        if !pickable.is_empty() && self.s.roll(2) {
            let i = pickable[self.s.below(pickable.len())];
            let name = self.fresh("mt");
            let owner = Ty::Struct(i);
            self.line(&format!("impl {} {{", self.structs[i].name));
            self.line(&format!("  fn {name}(self) -> int {{ 1 }}"));
            self.line("}");
            self.methods.push((name, owner, Ty::Int));
        }
        if self.s.roll(2) {
            let tr = self.fresh("tr");
            let m = self.fresh("mf");
            self.line(&format!("trait {tr} {{"));
            self.line(&format!("  fn {m}(self) -> int"));
            self.line("}");
            let bodies = ["{ 1 }", "{ 2 }"];
            for (head, body) in [
                ("int", bodies[self.s.below(2)]),
                ("bool", bodies[self.s.below(2)]),
                ("int | bool", bodies[self.s.below(2)]),
                ("list[int]", bodies[self.s.below(2)]),
            ] {
                if self.s.roll(3) {
                    continue;
                }
                let generics =
                    if head == "list[int]" { format!("[t: {tr}] ") } else { String::new() };
                if head == "list[int]" {
                    self.line(&format!("impl{generics}{tr} for list[t] {{"));
                } else {
                    self.line(&format!("impl {tr} for {head} {{"));
                }
                self.line(&format!("  fn {m}(self) -> int {body}"));
                self.line("}");
                if head == "int" {
                    self.methods.push((m.clone(), Ty::Int, Ty::Int));
                }
            }
        }
    }

    /// The free-generic story: a bound trait with full impls over int,
    /// string, and the generic list head; the free generic fns over it
    /// (the bounded call, the coinciding nested runtime form whose
    /// callee demand rides the calling frame's own staged slot, the lone
    /// nested demand the frame never staged, the boundless echo, the
    /// mut-over-variable planning door, the inert uncalled
    /// declaration); and the impl-completeness shapes (an impl omitting
    /// a bodiless non-defaulted member, or spelling it bodiless,
    /// reports DS2023 at the impl's own door, while an impl omitting a
    /// defaulted member stays clean). Call sites over the story's fns
    /// live in generic_call_stmt.
    fn gen_generic_calls(&mut self) {
        let tr = self.fresh("tr");
        let m = self.fresh("mf");
        self.line(&format!("trait {tr} {{"));
        self.line(&format!("  fn {m}(self) -> int"));
        self.line("}");
        self.line(&format!("impl {tr} for int {{"));
        self.line(&format!("  fn {m}(self) -> int {{ 1 }}"));
        self.line("}");
        self.line(&format!("impl {tr} for string {{"));
        self.line(&format!("  fn {m}(self) -> int {{ 2 }}"));
        self.line("}");
        self.line(&format!("impl [t: {tr}] {tr} for list[t] {{"));
        self.line(&format!("  fn {m}(self) -> int {{ 3 }}"));
        self.line("}");
        self.drawn.generic_impls += 3;
        let call = self.fresh("gf");
        self.line(&format!("fn {call}[t: {tr}] (x: t) -> int {{ x.{m}() }}"));
        // the coinciding runtime form: the nested callee's demand rides
        // the calling frame's own staged slot (the frame's own dot-call)
        let nested = self.fresh("gf");
        self.line(&format!(
            "fn {nested}[t: {tr}] (a: t, b: t) -> int {{ a.{m}() + {call}(b) }}"
        ));
        self.drawn.generic_runtime_forms += 1;
        // the lone nested demand: the body stages no slot of its own
        let lone = self.fresh("gf");
        self.line(&format!("fn {lone}[t: {tr}] (x: t) -> int {{ {call}(x) }}"));
        self.drawn.generic_doors += 1;
        let echo = self.fresh("gf");
        self.line(&format!("fn {echo}[t] (x: t) -> t {{ x }}"));
        // the mut-over-variable planning door
        let mutable = self.fresh("gf");
        self.line(&format!("fn {mutable}[t: {tr}] (mut x: t) -> int {{ 1 }}"));
        self.drawn.generic_doors += 1;
        // the inert uncalled declaration
        let inert = self.fresh("gf");
        self.line(&format!("fn {inert}[t: {tr}] (x: t) -> int {{ x.{m}() }}"));
        self.drawn.inert_generics += 1;
        for (name, arity, bounded) in [
            (call, 1, true),
            (nested, 2, true),
            (lone, 1, true),
            (echo, 1, false),
        ] {
            self.generics.push(GenericFn {
                name,
                arity,
                bounded,
            });
        }
        // the fn-typed-parameter door's driver: a generic fn name staged
        // where a callable word must go refuses at checking
        let applier = self.fresh("g");
        self.line(&format!(
            "fn {applier}(f: fn(int) -> int) -> int {{ f(1) }}"
        ));
        self.applier.clone_from(&applier);
        self.fns.push(FnDecl {
            name: applier,
            params: vec![Param {
                name: "f".to_owned(),
                ty: Ty::Fn(vec![Ty::Int], Box::new(Ty::Int)),
                default: None,
            }],
            ret: Some(Ty::Int),
        });
        // the impl-completeness shapes: a defaulted member stays
        // omittable, a bodiless non-defaulted one reports DS2023 (the
        // member omitted entirely or spelled bodiless)
        let t2 = self.fresh("tr");
        let m2 = self.fresh("mf");
        let defaulted = !self.s.roll(2);
        self.line(&format!("trait {t2} {{"));
        if defaulted {
            self.line(&format!("  fn {m2}(self) -> int {{ 7 }}"));
        } else {
            self.line(&format!("  fn {m2}(self) -> int"));
        }
        self.line("}");
        if defaulted {
            self.line(&format!("impl {t2} for int {{\n}}"));
            self.drawn.omitted_defaults += 1;
        } else if self.s.roll(2) {
            self.line(&format!("impl {t2} for int {{\n}}"));
            self.drawn.incomplete_impls += 1;
        } else {
            self.line(&format!("impl {t2} for int {{\n  fn {m2}(self) -> int\n}}"));
            self.drawn.incomplete_impls += 1;
        }
    }

    /// One extern signature with no body (the record's parser shape).
    fn gen_extern(&mut self) {
        let name = self.fresh("x");
        let pickable: Vec<usize> = self
            .structs
            .iter()
            .enumerate()
            .filter(|(_, d)| !d.generic)
            .map(|(i, _)| i)
            .collect();
        let ret = if pickable.is_empty() {
            let union = self.pick_union();
            self.render_ty(&union)
        } else {
            let i = pickable[self.s.below(pickable.len())];
            let extra = if self.tags.is_empty() {
                self.render_ty(&Ty::None)
            } else {
                let tag = Ty::Tag(self.s.below(self.tags.len()));
                self.render_ty(&tag)
            };
            format!("{} | {}", self.structs[i].name, extra)
        };
        self.line(&format!("extern fn {name}(s: string) -> {ret}"));
    }

    fn gen_consts(&mut self) {
        for _ in 0..self.s.below(3) {
            let name = self.fresh("k");
            if self.s.below(3) == 0 {
                self.line(&format!("const {name} = [1, 2, 3]"));
                self.consts.push((name, Ty::List(Box::new(Ty::Int))));
            } else {
                // a top-level const cannot name a lambda (a parse
                // refusal), so fn-typed values stay out of this arm
                let mut ty = self.pick_ty();
                if matches!(ty, Ty::Fn(..)) {
                    ty = Ty::Int;
                }
                let value = self.typed(&ty, 3);
                self.line(&format!("const {name}: {} = {value}", self.render_ty(&ty)));
                self.consts.push((name, ty));
            }
        }
    }

    /// The declared effect rows: 2-6 ambient names with stream-chosen
    /// bits, so the effects walk crosses the union edges it is given.
    fn pick_rows(&mut self) {
        for _ in 0..2 + self.s.below(5) {
            let name = AMBIENT[self.s.below(AMBIENT.len())];
            if self.rows.iter().any(|(n, _)| n == name) {
                continue;
            }
            let mut row = EffectRow::PURE;
            for effect in EFFECTS {
                if self.s.roll(2) {
                    row = row.union(EffectRow::of(effect));
                }
            }
            self.rows.push((name.to_owned(), row));
        }
    }

    fn gen_fns(&mut self) {
        let count = 2 + self.s.below(3);
        for _ in 0..count {
            let name = self.fresh("g");
            let mut params = Vec::new();
            for _ in 0..self.s.below(4) {
                let ty = self.pick_ty();
                let default = match &ty {
                    Ty::Int if self.s.roll(4) => Some("7".to_owned()),
                    Ty::Str if self.s.roll(6) => Some("\"s\"".to_owned()),
                    Ty::Bool if self.s.roll(6) => Some("true".to_owned()),
                    _ => None,
                };
                params.push(Param {
                    name: self.fresh("p"),
                    ty,
                    default,
                });
            }
            // the return: unit rarely, an error-widened union often (the
            // `?`-anchoring shape callers narrow against), else a picked type
            let ret = if self.s.roll(6) {
                Ty::Unit
            } else if self.s.roll(3) {
                let mut members = vec![Ty::Int];
                members.extend(self.pick_union_members());
                Ty::Union(members)
            } else {
                self.pick_ty()
            };
            let spelling = self.render_ty(&ret);
            let header = format!("fn {name}({}) -> {spelling}", self.render_params(&params));
            self.line(&format!("{header} {{"));
            self.fns.push(FnDecl {
                name,
                params: params.clone(),
                ret: Some(ret.clone()),
            });
            self.body(&params, &ret);
        }
        self.gen_move_fn();
    }

    /// The affine-pass shape: when a copyable-excluded struct exists, one
    /// fn takes it by value and its body passes a fresh binding twice —
    /// the first call records the move, the second the use-after-move, so
    /// the linearity pass's core rules run in most programs.
    fn gen_move_fn(&mut self) {
        let Some(i) = self
            .structs
            .iter()
            .enumerate()
            .find(|(_, d)| d.copyable && !d.generic)
            .map(|(i, _)| i)
        else {
            return;
        };
        let name = self.fresh("g");
        let h = self.fresh("h");
        let struct_name = self.structs[i].name.clone();
        self.fns.push(FnDecl {
            name: name.clone(),
            params: vec![Param {
                name: h.clone(),
                ty: Ty::Struct(i),
                default: None,
            }],
            ret: Some(Ty::Int),
        });
        self.line(&format!("fn {name}(move {h}: {struct_name}) -> int {{"));
        self.indent += 1;
        let k = self.fresh("k");
        let literal = self.struct_lit(i, 2);
        self.line(&format!("const {k}: {struct_name} = {literal}"));
        self.line(&format!("{name}({k})"));
        self.line(&format!("{name}({k})"));
        self.line("0");
        self.indent -= 1;
        self.line("}");
    }

    /// One fn's statements over its parameter scope, then the final
    /// expression typed against (or `?`-anchored to) the declared return.
    fn body(&mut self, params: &[Param], ret: &Ty) {
        self.indent += 1;
        let base = self.locals.len();
        for p in params {
            self.locals.push(Local {
                name: p.name.clone(),
                ty: p.ty.clone(),
                mutable: false,
            });
        }
        let saved_ret = self.ret.replace(ret.clone());
        for _ in 0..2 + self.s.below(5) {
            self.stmt();
        }
        match ret {
            Ty::Unit => {
                if self.s.roll(3) {
                    self.line("()");
                }
            },
            _ => {
                if self.s.roll(5)
                    && let Some(tail) = self.anchored_try(ret)
                {
                    self.line(&tail);
                } else {
                    let final_expr = self.typed(ret, 4);
                    self.line(&final_expr);
                }
            },
        }
        self.ret = saved_ret;
        self.locals.truncate(base);
        self.indent -= 1;
        self.line("}");
    }

    /// Distinct atoms for widening a return union (members and tags).
    fn pick_union_members(&mut self) -> Vec<Ty> {
        let mut out = Vec::new();
        for _ in 0..=self.s.below(2) {
            let pick = match self.s.below(3) {
                0 if !self.enums.is_empty() => {
                    let e = self.s.below(self.enums.len());
                    let c = self.s.below(self.enums[e].cases.len());
                    Ty::Member(e, c)
                },
                1 if !self.tags.is_empty() => Ty::Tag(self.s.below(self.tags.len())),
                _ => Ty::None,
            };
            let key = self.render_ty(&pick);
            if !out.iter().any(|t: &Ty| self.render_ty(t) == key) {
                out.push(pick);
            }
        }
        out
    }

    fn render_params(&self, params: &[Param]) -> String {
        params
            .iter()
            .map(|p| {
                let spelling = self.render_ty(&p.ty);
                match &p.default {
                    Some(d) => format!("{}: {spelling} = {d}", p.name),
                    None => format!("{}: {spelling}", p.name),
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// One statement, budget-guarded and weighted toward the shapes that
    /// drive deep checker state (bindings, narrowing, matches). Past the
    /// nesting cap only flat statements are emitted, so the parse's own
    /// recursion over nested blocks stays bounded.
    fn stmt(&mut self) {
        if self.budget < 12 {
            return;
        }
        self.budget -= 2;
        if self.nest > 8 {
            match self.s.below(4) {
                0 => self.binding_stmt(),
                1 => self.assign_stmt(),
                2 => self.return_stmt(),
                _ => self.call_stmt(),
            }
            return;
        }
        match self.s.below(29) {
            0..=7 => self.binding_stmt(),
            8..=10 => self.if_stmt(),
            11 => self.while_stmt(),
            12 | 13 => self.for_stmt(),
            14..=16 => self.match_stmt(),
            17 | 18 => self.return_stmt(),
            19 | 20 => self.assign_stmt(),
            21 | 22 => self.extraction_stmt(),
            23 => self.chaos_stmt(),
            26..=28 => self.generic_call_stmt(),
            _ => self.call_stmt(),
        }
    }

    fn binding_stmt(&mut self) {
        if self.s.roll(12) {
            self.destructure_stmt();
            return;
        }
        let var = self.s.roll(3);
        let ty = if self.s.roll(3)
            && let Some(l) = self.locals.last()
        {
            l.ty.clone()
        } else {
            self.pick_ty()
        };
        let name = self.fresh("x");
        let init = if self.s.roll(28) {
            let wrong = self.pick_base();
            self.typed(&wrong, 3)
        } else {
            self.typed(&ty, 4)
        };
        let spelling = self.render_ty(&ty);
        if var {
            self.line(&format!("var {name}: {spelling} = {init}"));
        } else if self.s.roll(6) {
            self.line(&format!("const {name} = {init}"));
        } else {
            self.line(&format!("const {name}: {spelling} = {init}"));
        }
        self.locals.push(Local {
            name,
            ty,
            mutable: var,
        });
    }

    /// Tuple destructuring binds each element as its own local.
    fn destructure_stmt(&mut self) {
        let t0 = self.pick_base();
        let t1 = self.pick_base();
        let v0 = self.typed(&t0, 3);
        let v1 = self.typed(&t1, 3);
        let a = self.fresh("d");
        let b = self.fresh("d");
        self.line(&format!("const ({a}, {b}) = ({v0}, {v1})"));
        self.locals.push(Local {
            name: a,
            ty: t0,
            mutable: false,
        });
        self.locals.push(Local {
            name: b,
            ty: t1,
            mutable: false,
        });
    }

    /// A boolean condition for control headers (never a struct literal:
    /// headers ban that form). The condition and the expression sides
    /// share one depth budget: the logical arms recurse into shallower
    /// conditions, and the comparison sides are shallower expressions,
    /// so no cycle can grow the condition unboundedly.
    fn cond(&mut self) -> String {
        self.cond_at(4)
    }

    fn cond_at(&mut self, depth: u32) -> String {
        if depth == 0 {
            return "true".to_owned();
        }
        match self.s.below(6) {
            0 => {
                let a = self.typed(&Ty::Int, depth - 1);
                let b = self.typed(&Ty::Int, depth - 1);
                format!("{a} < {b}")
            },
            1 => format!("!{}", self.cond_at(depth - 1)),
            2 => format!("{} && {}", self.cond_at(depth - 1), self.cond_at(depth - 1)),
            3 => format!("{} || {}", self.cond_at(depth - 1), self.cond_at(depth - 1)),
            4 => {
                let x = self.typed(&Ty::Int, depth - 1);
                let n = self.int_lit();
                format!("{x} in {n}..{n}")
            },
            _ => {
                if let Some(l) = self
                    .locals
                    .iter()
                    .rev()
                    .find(|l| matches!(l.ty, Ty::Union(_)))
                    .cloned()
                {
                    // whole-span equality over a union operand: a member
                    // value against the union (the multi-atom vocabulary)
                    let rhs = self.leaf(&l.ty);
                    self.drawn.union_equality += 1;
                    format!("{} == {}", l.name, rhs)
                } else {
                    "true".to_owned()
                }
            },
        }
    }

    /// The narrowing if: over a union local, `is` refines the binder in
    /// the then-branch; a bare boolean condition otherwise.
    fn if_stmt(&mut self) {
        let union_local = self
            .locals
            .iter()
            .rev()
            .find(|l| matches!(l.ty, Ty::Union(_)))
            .map(|l| (l.name.clone(), l.ty.clone()));
        let Some((name, ty)) = union_local else {
            let header = self.cond();
            self.line(&format!("if {header} {{"));
            let count = 1 + self.s.below(2);
            self.block(count);
            if self.s.roll(3) {
                self.line("} else {");
                let count = 1 + self.s.below(2);
                self.block(count);
            }
            self.line("}");
            return;
        };
        let members = match &ty {
            Ty::Union(ms) => ms.clone(),
            _ => vec![ty],
        };
        let member = members[self.s.below(members.len())].clone();
        let mut header = format!("{name} is {}", self.render_ty(&member));
        // the &&-chain narrowing of the corpus: numeric member tested
        // further in the same condition
        if matches!(member, Ty::Int) && self.s.roll(2) {
            let n = self.int_lit();
            header = format!("{header} && {name} >= {n}");
        }
        self.line(&format!("if {header} {{"));
        let mark = self.locals.len();
        self.locals.push(Local {
            name: name.clone(),
            ty: member,
            mutable: false,
        });
        let count = 1 + self.s.below(3);
        self.block(count);
        self.locals.truncate(mark);
        if self.s.roll(3) {
            self.line("} else {");
            let count = 1 + self.s.below(2);
            self.block(count);
        }
        self.line("}");
    }

    fn block(&mut self, count: usize) {
        self.indent += 1;
        self.nest += 1;
        for _ in 0..count {
            self.stmt();
        }
        self.nest -= 1;
        self.indent -= 1;
    }

    /// A while over an int var with a guaranteed mutation of the tested
    /// var (the loop-carried binding the affine pass tracks).
    fn while_stmt(&mut self) {
        let found = self
            .locals
            .iter()
            .rev()
            .find(|l| l.mutable && matches!(l.ty, Ty::Int))
            .map(|l| l.name.clone());
        let counter = if let Some(name) = found {
            name
        } else {
            let name = self.fresh("w");
            let n = self.int_lit();
            self.line(&format!("var {name}: int = {n}"));
            self.locals.push(Local {
                name: name.clone(),
                ty: Ty::Int,
                mutable: true,
            });
            name
        };
        let bound = self.int_lit();
        self.line(&format!("while {counter} < {bound} {{"));
        self.indent += 1;
        self.nest += 1;
        self.loop_depth += 1;
        let count = 1 + self.s.below(2);
        for _ in 0..count {
            self.stmt();
        }
        self.line(&format!("{counter} += 1"));
        match self.s.below(64) {
            0 => self.line("break"),
            1 => self.line("continue"),
            _ => {},
        }
        self.loop_depth -= 1;
        self.nest -= 1;
        self.indent -= 1;
        self.line("}");
    }

    /// A for-in over an int range (the lowered form), plus a nested
    /// collection iteration when a list local exists (the checker's
    /// for-in over collections).
    fn for_stmt(&mut self) {
        let binder = self.fresh("i");
        let a = self.int_lit();
        let b = self.int_lit();
        let inclusive = if self.s.roll(2) { "..=" } else { ".." };
        let pattern = if self.s.roll(6) { "_".to_owned() } else { binder.clone() };
        self.line(&format!("for {pattern} in {a}{inclusive}{b} {{"));
        let mark = self.locals.len();
        self.locals.push(Local {
            name: binder,
            ty: Ty::Int,
            mutable: false,
        });
        self.indent += 1;
        self.nest += 1;
        self.loop_depth += 1;
        let count = 1 + self.s.below(3);
        for _ in 0..count {
            self.stmt();
        }
        self.loop_depth -= 1;
        self.nest -= 1;
        self.indent -= 1;
        self.locals.truncate(mark);
        self.line("}");
        if let Some(list) = self
            .locals
            .iter()
            .rev()
            .find(|l| matches!(l.ty, Ty::List(_)))
            .map(|l| l.name.clone())
            && self.s.roll(4)
        {
            let elem = self.fresh("y");
            self.line(&format!("for {elem} in {list} {{"));
            self.indent += 1;
            self.nest += 1;
            let inner = match &self
                .locals
                .iter()
                .rev()
                .find(|l| l.name == list)
                .unwrap()
                .ty
            {
                Ty::List(el) => (**el).clone(),
                _ => Ty::Int,
            };
            let mark = self.locals.len();
            self.locals.push(Local {
                name: elem,
                ty: inner,
                mutable: false,
            });
            self.stmt();
            self.locals.truncate(mark);
            self.nest -= 1;
            self.indent -= 1;
            self.line("}");
        }
    }

    fn return_stmt(&mut self) {
        let Some(ret) = self.ret.clone() else {
            self.call_stmt();
            return;
        };
        if matches!(ret, Ty::Unit) {
            if self.s.roll(3) {
                self.line("return");
            } else {
                self.line("return ()");
            }
            return;
        }
        if self.s.roll(4)
            && let Some(tail) = self.anchored_try(&ret)
        {
            self.line(&format!("return {tail}"));
        } else if self.s.roll(3) {
            let value = self.typed(&ret, 3);
            self.line(&format!("return {value}"));
        } else {
            self.line("return");
        }
    }

    fn assign_stmt(&mut self) {
        let Some(target) = self
            .locals
            .iter()
            .rev()
            .find(|l| l.mutable)
            .map(|l| (l.name.clone(), l.ty.clone()))
        else {
            self.binding_stmt();
            return;
        };
        let (name, ty) = target;
        match ty {
            Ty::Int => {
                let op = ["+=", "-=", "*=", "/=", "%=", "<<=", ">>="][self.s.below(7)];
                let value = if op == "<<=" || op == ">>=" {
                    self.s.below(9).to_string()
                } else {
                    self.int_lit()
                };
                self.line(&format!("{name} {op} {value}"));
            },
            other => {
                let value = self.typed(&other, 3);
                self.line(&format!("{name} = {value}"));
            },
        }
    }

    /// One call into the story's free generic fns: one drawn
    /// instantiation for the whole site (every parameter over the fn's
    /// type variable takes the same type — int, string, list[int], or
    /// the two-level list[list[int]]), with the value doors drawn
    /// occasionally — the multi-word struct argument into a boundless
    /// variable parameter, the generic fn value, the generic fn name
    /// staged where a callable word must go (a checking refusal), and
    /// the local generic lambda.
    fn generic_call_stmt(&mut self) {
        let callee = self.generics[self.s.below(self.generics.len())].clone();
        let (args, multiword) = if !callee.bounded && self.s.roll(5) {
            match self.multiword_struct() {
                Some(lit) => (vec![lit], true),
                None => (self.args_of_kind(callee.arity), false),
            }
        } else {
            (self.args_of_kind(callee.arity), false)
        };
        let call = format!("{}({})", callee.name, args.join(", "));
        if self.s.roll(3) {
            let name = self.fresh("q");
            self.line(&format!("const {name} = {call}"));
        } else {
            self.line(&call);
        }
        self.drawn.generic_calls += 1;
        if multiword {
            self.drawn.generic_doors += 1;
        }
        if self.s.roll(6) {
            // the generic fn value: the bare name checks clean and
            // refuses at lowering (the word carries no instantiation)
            let name = self.fresh("q");
            self.line(&format!("const {name} = {}", callee.name));
            self.drawn.generic_doors += 1;
        }
        if self.s.roll(6) {
            // the local generic lambda: a generic lambda value refuses
            // at its binding (at lowering)
            let name = self.fresh("q");
            self.line(&format!("const {name} = [t] (x: mut t) => {{ x = x }}"));
            self.drawn.generic_doors += 1;
        }
        if self.s.roll(6) {
            // a generic fn name staged where a callable word must go
            // refuses at checking (the fn type escapes containment)
            let applier = self.applier.clone();
            self.line(&format!("{applier}({})", callee.name));
            self.drawn.generic_doors += 1;
        }
    }

    /// The site's arguments over one drawn instantiation kind: each
    /// parameter gets its own value of the same kind.
    fn args_of_kind(&mut self, arity: usize) -> Vec<String> {
        let kind = self.s.below(7);
        if kind == 4 {
            self.drawn.two_level_calls += 1;
        }
        (0..arity).map(|_| self.arg_of_kind(kind)).collect()
    }

    /// One argument of a drawn kind: the int, string, list[int], and
    /// list[list[int]] instantiation chains.
    fn arg_of_kind(&mut self, kind: usize) -> String {
        match kind {
            2 => self.str_lit(),
            3 => format!("[{}]", self.int_lit()),
            4 => {
                let e = self.int_lit();
                format!("[[{e}]]")
            },
            _ => self.int_lit(),
        }
    }

    /// A struct literal over a declared multi-field struct, for the
    /// multi-word argument door.
    fn multiword_struct(&mut self) -> Option<String> {
        let pickable: Vec<usize> = self
            .structs
            .iter()
            .enumerate()
            .filter(|(_, d)| !d.generic && d.fields.len() >= 2)
            .map(|(i, _)| i)
            .collect();
        if pickable.is_empty() {
            return None;
        }
        let i = pickable[self.s.below(pickable.len())];
        Some(self.struct_lit(i, 2))
    }

    /// A bare expression statement: a call, a UFCS dot-call, a lambda
    /// call through a fn-typed local, or an ambient call.
    fn call_stmt(&mut self) {
        let expr = if self.s.roll(3) {
            let want = self.pick_ty();
            match self.call_for(&want, 3) {
                Some(call) => call,
                None => self.typed(&Ty::Unit, 2),
            }
        } else if self.s.roll(4) {
            match self
                .locals
                .iter()
                .rev()
                .find(|l| matches!(l.ty, Ty::Fn(..)))
                .map(|l| l.name.clone())
            {
                Some(g) => format!("{g}({})", self.typed(&Ty::Int, 2)),
                None => self.typed(&Ty::Unit, 2),
            }
        } else {
            let name = AMBIENT[self.s.below(AMBIENT.len())];
            let arg = self.typed(&Ty::Str, 2);
            format!("{name}({arg})")
        };
        self.line(&expr);
    }

    /// A match over a union local (member arms with or-patterns,
    /// destructuring, guards, binders, and the optional catchall), an
    /// int local, or an optional.
    fn match_stmt(&mut self) {
        // the struct-pattern leg: a struct-typed scrutinee draws the
        // struct pattern vocabulary (literal field tests in place,
        // punning binders, the or-alternative arm, nested levels)
        let struct_found = self.s.roll(3).then(|| {
            self.locals
                .iter()
                .rev()
                .find(|l| matches!(l.ty, Ty::Struct(_)))
                .map(|l| match &l.ty {
                    Ty::Struct(i) => (l.name.clone(), *i),
                    _ => unreachable!("the scrutinee filter admits structs only"),
                })
        });
        if let Some(Some((name, i))) = struct_found {
            self.line(&format!("match {name} {{"));
            self.indent += 1;
            self.drawn.struct_patterns += 1;
            let pat = self.struct_sub_pattern(i);
            let body = self.typed(&Ty::Int, 2);
            if self.s.roll(2) {
                // the or-vocabulary: two struct alternatives, one arm
                let alt = self.struct_sub_pattern(i);
                self.drawn.or_patterns += 1;
                self.line(&format!("{pat} | {alt} => {body}"));
            } else {
                self.line(&format!("{pat} => {body}"));
            }
            if self.s.roll(2) {
                let n = self.fresh("m");
                let body = self.typed(&Ty::Int, 2);
                self.line(&format!("{n} => {body}"));
            }
            self.indent -= 1;
            self.line("}");
            return;
        }
        // dispatch scrutinees (unions, enums) carry the arm vocabulary,
        // so they win the pick over the scalar fallback half the time
        let dispatch = self
            .locals
            .iter()
            .rev()
            .find(|l| matches!(l.ty, Ty::Union(_) | Ty::Enum(_)))
            .map(|l| (l.name.clone(), l.ty.clone()));
        let scalar = self
            .locals
            .iter()
            .rev()
            .find(|l| matches!(l.ty, Ty::Int))
            .map(|l| (l.name.clone(), l.ty.clone()));
        let found = if dispatch.is_some() && self.s.roll(2) { dispatch } else { scalar };
        let Some((name, ty)) = found else {
            self.binding_stmt();
            return;
        };
        self.line(&format!("match {name} {{"));
        self.indent += 1;
        match &ty {
            Ty::Union(members) => {
                let mut catchall = true;
                for m in members {
                    self.arm_over_member(&name, m, &mut catchall);
                }
                if self.s.roll(3) && members.len() > 1 {
                    // the or-pattern arm: two members, one arm
                    let a = members[0].clone();
                    let b = members[1].clone();
                    let body = self.typed(&Ty::Int, 2);
                    catchall = false;
                    self.line(&format!(
                        "{} | {} => {body}",
                        self.render_ty(&a),
                        self.render_ty(&b)
                    ));
                }
                if self.s.roll(3) {
                    let n = self.fresh("n");
                    let body = self.typed(&Ty::Int, 2);
                    self.line(&format!("{n} if {n} is int => {body}"));
                }
                if catchall || self.s.roll(2) {
                    let n = self.fresh("m");
                    let body = self.typed(&Ty::Int, 2);
                    self.line(&format!("{n} => {body}"));
                    self.line("_ => ()");
                }
            },
            Ty::Int => {
                if self.s.roll(3) {
                    // the value-level or-pattern: a literal ladder
                    let a = self.int_lit();
                    let b = self.int_lit();
                    let body = self.typed(&Ty::Int, 2);
                    self.line(&format!("{a} | {b} => {body}"));
                    self.drawn.or_patterns += 1;
                }
                let n = self.int_lit();
                let body = self.typed(&Ty::Int, 2);
                self.line(&format!("{n} => {body}"));
                let body = self.typed(&Ty::Int, 2);
                self.line(&format!("7 => {body}"));
                if self.s.roll(2) {
                    let k = self.fresh("k");
                    let body = self.typed(&Ty::Int, 2);
                    self.line(&format!("{k} if {k} > 0 => {body}"));
                }
                let k = self.fresh("k");
                let body = self.typed(&Ty::Int, 2);
                self.line(&format!("{k} => {body}"));
            },
            _ => {
                let e = match &ty {
                    Ty::Enum(i) => *i,
                    _ => unreachable!("the scrutinee filter admits unions, ints, and enums"),
                };
                let cases = self.enums[e].cases.clone();
                let mut catchall = true;
                for case in &cases {
                    let body = self.typed(&Ty::Int, 2);
                    self.line(&format!("{} => {body}", case.name));
                    catchall = false;
                }
                // the dispatch vocabulary over enum scrutinees: payload
                // tests, guarded payload binders, and nested struct
                // sub-patterns, as dead-but-typed duplicate arms over
                // the already-covered cases
                for case in &cases {
                    self.dispatch_extra_arm(&case.name, case.payload.as_ref());
                }
                if catchall || self.s.roll(2) {
                    self.line("_ => ()");
                }
            },
        }
        self.indent -= 1;
        self.line("}");
    }

    /// One dead-but-typed duplicate arm over an already-covered case or
    /// member: the dispatch vocabulary's tested shapes — a literal
    /// payload test, a value-comparison guard over the payload binder,
    /// a struct field test, or a nested struct sub-pattern — drawn for
    /// their checker walks (the arm is dead, never wrong).
    fn dispatch_extra_arm(&mut self, case: &str, payload: Option<&Payload>) {
        let Some(payload) = payload else { return };
        match payload {
            Payload::Tuple(ts) if ts.len() == 1 => match ts[0].clone() {
                Ty::Int if self.s.roll(2) => {
                    let lit = self.int_lit();
                    let body = self.typed(&Ty::Int, 2);
                    self.line(&format!("{case}({lit}) => {body}"));
                    self.drawn.payload_tests += 1;
                },
                Ty::Int if self.s.roll(2) => {
                    let n = self.fresh("n");
                    let lit = self.int_lit();
                    let body = self.typed(&Ty::Int, 2);
                    self.line(&format!("{case}({n}) if {n} > {lit} => {body}"));
                    self.drawn.union_guards += 1;
                },
                Ty::Str if self.s.roll(2) => {
                    let lit = self.str_lit();
                    let body = self.typed(&Ty::Int, 2);
                    self.line(&format!("{case}({lit}) => {body}"));
                    self.drawn.payload_tests += 1;
                },
                Ty::Struct(j) if !self.structs[j].generic && self.s.roll(2) => {
                    let sub = self.struct_sub_pattern(j);
                    let body = self.typed(&Ty::Int, 2);
                    self.line(&format!("{case}({sub}) => {body}"));
                },
                _ => {},
            },
            Payload::Fields(fs) if self.s.roll(2) => {
                let Some((f, _)) = fs.iter().find(|(_, t)| matches!(t, Ty::Int)).cloned() else {
                    return;
                };
                let lit = self.int_lit();
                let body = self.typed(&Ty::Int, 2);
                self.line(&format!("{case} {{ {f}: {lit} }} => {body}"));
                self.drawn.payload_tests += 1;
            },
            _ => {},
        }
    }

    /// A struct sub-pattern over struct `i`: the int-typed fields test
    /// literals, one struct-typed field carries a nested sub-pattern
    /// one level deeper (counted), and the remaining fields pun (bind
    /// their own names).
    fn struct_sub_pattern(&mut self, i: usize) -> String {
        let decl = self.structs[i].clone();
        let mut parts: Vec<String> = Vec::new();
        let mut nested = false;
        for (f, t) in &decl.fields {
            if !nested
                && let Ty::Struct(j) = t
                && !self.structs[*j].generic
                && !self.s.roll(3)
            {
                nested = true;
                let inner = self.structs[*j].clone();
                let mut inner_parts: Vec<String> = Vec::new();
                for (g, gt) in &inner.fields {
                    if matches!(gt, Ty::Int) && self.s.roll(2) {
                        inner_parts.push(format!("{g}: {}", self.int_lit()));
                    } else {
                        inner_parts.push(g.clone());
                    }
                }
                if inner_parts.is_empty() {
                    parts.push(f.clone());
                    nested = false;
                } else {
                    parts.push(format!(
                        "{f}: {} {{ {} }}",
                        inner.name,
                        inner_parts.join(", ")
                    ));
                }
            } else if matches!(t, Ty::Int) && self.s.roll(3) {
                parts.push(format!("{f}: {}", self.int_lit()));
            } else {
                parts.push(f.clone());
            }
        }
        if nested {
            self.drawn.nested_sub_patterns += 1;
        }
        format!("{} {{ {} }}", decl.name, parts.join(", "))
    }

    /// One member arm: bare member test, payload destructuring, or a
    /// guarded binder over the refined member.
    fn arm_over_member(&mut self, name: &str, member: &Ty, catchall: &mut bool) {
        let pattern = self.render_ty(member);
        if let Ty::Member(e, c) = member
            && let Some(payload) = self.enums[*e].cases[*c].payload.clone()
        {
            match payload {
                Payload::Tuple(ts) if self.s.roll(2) => {
                    let binds: Vec<String> = ts.iter().map(|_| self.fresh("b")).collect();
                    let body = self.typed(&Ty::Int, 2);
                    self.line(&format!("{pattern}({}) => {body}", binds.join(", ")));
                },
                Payload::Fields(fs) if self.s.roll(2) => {
                    let fields: Vec<String> = fs.iter().map(|(f, _)| f.clone()).collect();
                    let body = self.typed(&Ty::Int, 2);
                    self.line(&format!("{pattern} {{ {} }} => {body}", fields.join(", ")));
                },
                _ => {
                    let body = self.typed(&Ty::Int, 2);
                    self.line(&format!("{pattern} => {body}"));
                },
            }
        } else if self.s.roll(5) {
            let n = self.fresh("n");
            let body = self.typed(&Ty::Int, 2);
            self.line(&format!("{n} if {n} is {pattern} => {body}"));
        } else if self.s.roll(8) && matches!(member, Ty::Int) {
            let body = self.typed(&Ty::Int, 2);
            self.line(&format!("{name} => {body}"));
        } else {
            let body = self.typed(&Ty::Int, 2);
            self.line(&format!("{pattern} => {body}"));
        }
        // the dispatch vocabulary's dead-but-typed duplicate over this
        // member: a payload test, a value guard, or a nested
        // sub-pattern, walked by the checker beside the covering arm
        if let Ty::Member(e, c) = member {
            let payload = self.enums[*e].cases[*c].payload.clone();
            self.dispatch_extra_arm(&pattern, payload.as_ref());
        }
        *catchall = false;
    }

    /// The extraction family over a union or optional receiver: the
    /// total `.ok`/`.err` pair, `unwrap_or`, `unwrap_or_else`, `ok_or`,
    /// `map_err`, match-based extraction, and the mid-block `e?` binding
    /// whose propagated members escape the return union (plus the
    /// fully-contained spelling that reports by design, so the checker's
    /// boundary stays exercised).
    fn extraction_stmt(&mut self) {
        let found = self
            .locals
            .iter()
            .rev()
            .find(|l| matches!(l.ty, Ty::Union(_)))
            .map(|l| (l.name.clone(), l.ty.clone()));
        let Some((name, ty)) = found else {
            self.binding_stmt();
            return;
        };
        let members = match &ty {
            Ty::Union(ms) => ms.clone(),
            other => vec![other.clone()],
        };
        let target = if self.s.roll(3) { Ty::Int } else { Ty::Str };
        let spelling = self.render_ty(&target);
        let y = self.fresh("x");
        let bind = |g: &mut Self, ty: Ty| {
            g.locals.push(Local {
                name: y.clone(),
                ty,
                mutable: false,
            });
        };
        match self.s.below(9) {
            0 => {
                // the total pair: ok handles one member, err the rest
                let handled = members[0].clone();
                let rest: Vec<Ty> = members.iter().skip(1).cloned().collect();
                let h = self.fresh("h");
                let ok_body = self.typed(&target, 2);
                let err_body = self.typed(&target, 2);
                if rest.is_empty() {
                    self.line(&format!(
                        "const {y}: {spelling} = {name}.ok(({h}: {}) => {ok_body})",
                        self.render_ty(&handled)
                    ));
                } else {
                    let rest_spelling = self.render_ty(&Ty::Union(rest));
                    self.line(&format!(
                        "const {y}: {spelling} = {name}.ok(({h}: {}) => {ok_body}).err((_: \
                         {rest_spelling}) => {err_body})",
                        self.render_ty(&handled)
                    ));
                }
                bind(self, target);
            },
            1 => {
                let body = self.typed(&target, 2);
                if self.s.roll(2) {
                    self.line(&format!("const {y}: {spelling} = {name}.unwrap_or({body})"));
                } else {
                    self.line(&format!(
                        "const {y}: {spelling} = {name}.unwrap_or_else(() => {body})"
                    ));
                }
                bind(self, target);
            },
            2 => {
                // ok_or widens by one atom; either kept or handled total
                let extra = self
                    .pick_union_members()
                    .into_iter()
                    .next()
                    .unwrap_or(Ty::None);
                let extra_value = self.member_value(&extra);
                let extra_spelling = self.render_ty(&extra);
                if self.s.roll(2) {
                    self.line(&format!(
                        "const {y}: {spelling} | {extra_spelling} = {name}.ok_or({extra_value})"
                    ));
                    bind(self, Ty::Union(vec![target.clone(), extra]));
                } else {
                    let body = self.typed(&target, 2);
                    self.line(&format!(
                        "const {y}: {spelling} = {name}.ok_or({extra_value}).err((_: \
                         {extra_spelling}) => {body})"
                    ));
                    bind(self, target);
                }
            },
            3 => {
                // map_err mirrors the handler over one handled member
                let handled = members[self.s.below(members.len())].clone();
                let rest: Vec<Ty> = members
                    .iter()
                    .filter(|m| self.render_ty(m) != self.render_ty(&handled))
                    .cloned()
                    .collect();
                let mapped = self
                    .pick_union_members()
                    .into_iter()
                    .next()
                    .unwrap_or(Ty::None);
                let mapped_value = self.member_value(&mapped);
                let mapped_spelling = self.render_ty(&mapped);
                let rest_spelling = if rest.is_empty() {
                    String::new()
                } else {
                    format!("{} | ", self.render_ty(&Ty::Union(rest.clone())))
                };
                self.line(&format!(
                    "const {y}: {rest_spelling}{mapped_spelling} = {name}.map_err((e: {}) => \
                     {mapped_value})",
                    self.render_ty(&handled)
                ));
                let mut widened = rest;
                widened.push(mapped);
                bind(self, Ty::Union(widened));
            },
            4 => {
                // match extraction: the none arm plus a binder over the rest
                let none_body = self.typed(&target, 2);
                let rest: Vec<Ty> = members
                    .iter()
                    .filter(|m| !matches!(m, Ty::None))
                    .cloned()
                    .collect();
                let v = self.fresh("v");
                let value_arm = if rest.len() == 1 && self.render_ty(&rest[0]) == spelling {
                    v.clone()
                } else {
                    self.typed(&target, 2)
                };
                self.line(&format!(
                    "const {y}: {spelling} = match {name} {{\n  none => {none_body}\n  {v} => \
                     {value_arm}\n}}"
                ));
                bind(self, target);
            },
            5 | 6 => {
                // the mid-block `e?`: members inside the return union
                // bind, the rest propagate; the fully-contained spelling
                // (nothing binds) is kept as the deliberate boundary
                let Some(ret) = self.ret.clone() else {
                    self.extraction_fallback(&y, &spelling, &target);
                    return;
                };
                let ret_keys = self.atom_keys(&ret);
                let inside: Vec<Ty> = members
                    .iter()
                    .filter(|m| ret_keys.contains(&self.render_ty(m)))
                    .cloned()
                    .collect();
                if inside.is_empty() || self.s.roll(4) {
                    self.line(&format!("const {y} = {name}?"));
                    bind(self, Ty::Union(members.clone()));
                } else {
                    self.line(&format!("const {y} = {name}?"));
                    bind(self, Ty::Union(inside));
                }
            },
            _ => self.extraction_fallback(&y, &spelling, &target),
        }
    }

    fn extraction_fallback(&mut self, y: &str, spelling: &str, target: &Ty) {
        let value = self.typed(target, 3);
        self.line(&format!("const {y}: {spelling} = {value}"));
        self.locals.push(Local {
            name: y.to_owned(),
            ty: target.clone(),
            mutable: false,
        });
    }

    /// A deliberately wrong but parse-valid statement, so the checker's
    /// error paths (empty intersections, unknown names, un-annotated
    /// handlers, never-cover arms) stay exercised alongside clean ones.
    fn chaos_stmt(&mut self) {
        let name = self.fresh("q");
        match self.s.below(7) {
            0 => self.line(&format!("const {name}: int & string = 0")),
            1 => self.line(&format!("const {name} = z_{name}(1)")),
            2 => {
                let weird = self.chaos_ty();
                self.line(&format!("const {name}: {weird} = 0"));
            },
            3 => {
                let recv = self
                    .locals
                    .last()
                    .map_or_else(|| "x0".to_owned(), |l| l.name.clone());
                self.line(&format!("const {name} = {recv}.zz_{name}()"));
            },
            4 => {
                let recv = self
                    .locals
                    .iter()
                    .rev()
                    .find(|l| matches!(l.ty, Ty::Union(_)))
                    .map(|l| l.name.clone());
                if let Some(recv) = recv {
                    self.line(&format!("const {name} = {recv}.err((e) => 0)"));
                } else {
                    self.line(&format!("const {name} = 0"));
                }
            },
            5 => {
                let recv = self
                    .locals
                    .last()
                    .map_or_else(|| "x0".to_owned(), |l| l.name.clone());
                self.line(&format!(
                    "match {recv} {{\n  true => ()\n  'q' => ()\n  b'z' => ()\n  _ => ()\n}}"
                ));
            },
            _ => self.line(&format!("const {name} = {name}")),
        }
    }

    /// A rare type spelling from the full grammar: intersections,
    /// differences, complements, optionals, fn types, pointer types, and
    /// the legacy angle-bracket application.
    fn chaos_ty(&mut self) -> String {
        let picked = self.pick_base();
        let base = self.render_ty(&picked);
        match self.s.below(7) {
            0 => format!("int & {base}"),
            1 => format!("{base} - int"),
            2 => format!("~{base}"),
            3 => format!("{base}?"),
            4 => "fn(int) -> int".to_owned(),
            5 => "list<int>".to_owned(),
            _ => "* const int".to_owned(),
        }
    }

    /// A well-typed expression for one type, depth-capped and budget-fed.
    fn typed(&mut self, ty: &Ty, depth: u32) -> String {
        if self.budget <= 4 || depth == 0 {
            return self.leaf(ty);
        }
        self.budget -= 2;
        if self.s.roll(20) {
            // the ambient splice: a row-declared name, unknown to the
            // checker, exercised by the effects walk
            let name = AMBIENT[self.s.below(AMBIENT.len())];
            let arg = self.leaf(&Ty::Str);
            return format!("{name}({arg})");
        }
        if self.s.roll(7)
            && let Some(call) = self.call_for(ty, depth)
        {
            return call;
        }
        if self.s.roll(8)
            && let Some(local) = self.local_for(ty)
        {
            return local;
        }
        if self.s.roll(24) {
            // one deliberately wrong operand: the checker's mismatch path
            let wrong = self.pick_base();
            return self.typed(&wrong, depth - 1);
        }
        match ty.clone() {
            Ty::Int => match self.s.below(8) {
                0 => format!(
                    "{} + {}",
                    self.typed(&Ty::Int, depth - 1),
                    self.typed(&Ty::Int, depth - 1)
                ),
                1 => format!("-{}", self.typed(&Ty::Int, depth - 1)),
                2 => format!("{} % 7", self.typed(&Ty::Int, depth - 1)),
                3 => format!("{} << 3", self.typed(&Ty::Int, depth - 1)),
                4 | 5 => format!(
                    "if {} {{ {} }} else {{ {} }}",
                    self.cond_at(depth - 1),
                    self.typed(&Ty::Int, depth - 1),
                    self.typed(&Ty::Int, depth - 1)
                ),
                _ => self.int_lit(),
            },
            Ty::Float => match self.s.below(4) {
                0 => format!("{} + 1.5", self.typed(&Ty::Float, depth - 1)),
                _ => self.float_lit(),
            },
            Ty::Str => match self.s.below(8) {
                0 => self.interp_str(),
                1 => format!(
                    "{} + {}",
                    self.typed(&Ty::Str, depth - 1),
                    self.typed(&Ty::Str, depth - 1)
                ),
                3 if self
                    .consts
                    .iter()
                    .any(|(_, t)| self.atom_keys(t) == self.atom_keys(&Ty::Str)) =>
                {
                    let fitting: Vec<usize> = self
                        .consts
                        .iter()
                        .enumerate()
                        .filter(|(_, (_, t))| self.atom_keys(t) == self.atom_keys(&Ty::Str))
                        .map(|(i, _)| i)
                        .collect();
                    let (name, _) = self.consts[fitting[self.s.below(fitting.len())]].clone();
                    name
                },
                _ => self.str_lit(),
            },
            Ty::Bool => match self.s.below(8) {
                0 | 1 => self.cond(),
                2 => format!(
                    "{} == {}",
                    self.typed(&Ty::Str, depth - 1),
                    self.typed(&Ty::Str, depth - 1)
                ),
                3 => format!(
                    "{} < {}",
                    self.typed(&Ty::Int, depth - 1),
                    self.typed(&Ty::Int, depth - 1)
                ),
                _ => "true".to_owned(),
            },
            Ty::Char => "'q'".to_owned(),
            Ty::Byte => "b'z'".to_owned(),
            Ty::None => "none".to_owned(),
            Ty::Unit => "()".to_owned(),
            Ty::List(el) => match self.s.below(4) {
                0 => format!("list[{}]()", self.render_ty(&el)),
                1 => format!(
                    "[{}, {}]",
                    self.typed(&el, depth - 1),
                    self.typed(&el, depth - 1)
                ),
                2 => format!("[{}]", self.typed(&el, depth - 1)),
                _ => "[]".to_owned(),
            },
            Ty::Map(k, v) => format!("map[{}, {}]()", self.render_ty(&k), self.render_ty(&v)),
            Ty::Tuple(ts) => format!(
                "({})",
                ts.iter()
                    .map(|t| self.typed(t, depth - 1))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Ty::Struct(i) => self.struct_lit(i, depth),
            Ty::Enum(i) => {
                let case = self.s.below(self.enums[i].cases.len());
                self.member_value(&Ty::Member(i, case))
            },
            Ty::Member(e, c) => self.member_value(&Ty::Member(e, c)),
            Ty::Tag(i) => self.tags[i].clone(),
            Ty::Union(ms) => {
                // set theory: any member's expression inhabits the union
                let pick = ms[self.s.below(ms.len())].clone();
                self.typed(&pick, depth)
            },
            Ty::Fn(ps, ret) => {
                // the wrong splice: a loop exit inside a lambda body
                // inside a loop is the checker's frame-boundary refusal
                if self.loop_depth > 0 && self.s.roll(32) {
                    return "() => { break }".to_owned();
                }
                match self.s.below(4) {
                    0 => {
                        let names: Vec<String> = ps.iter().map(|_| self.fresh("z")).collect();
                        let saved = self.locals.len();
                        for (n, p) in names.iter().zip(&ps) {
                            self.locals.push(Local {
                                name: n.clone(),
                                ty: p.clone(),
                                mutable: false,
                            });
                        }
                        let body = self.typed(&ret, depth - 1);
                        self.locals.truncate(saved);
                        let header = names
                            .iter()
                            .zip(&ps)
                            .map(|(n, p)| format!("{n}: {}", self.render_ty(p)))
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!("({header}) => {body}")
                    },
                    1 => {
                        let p = self.fresh("z");
                        format!("({p}) => {p}")
                    },
                    _ => {
                        let body = self.typed(&ret, depth - 1);
                        format!("() => {body}")
                    },
                }
            },
        }
    }

    /// The smallest well-typed value for a type.
    fn leaf(&mut self, ty: &Ty) -> String {
        if self.s.roll(3)
            && let Some(local) = self.local_for(ty)
        {
            return local;
        }
        match ty {
            Ty::Int => self.int_lit(),
            Ty::Float => self.float_lit(),
            Ty::Str => self.str_lit(),
            Ty::Bool => "true".to_owned(),
            Ty::Char => "'q'".to_owned(),
            Ty::Byte => "b'z'".to_owned(),
            Ty::None => "none".to_owned(),
            Ty::Unit => "()".to_owned(),
            Ty::List(el) => format!("[{}]", self.typed(el, 1)),
            Ty::Map(..) => "map[string, int]()".to_owned(),
            Ty::Tuple(ts) => format!(
                "({})",
                ts.iter()
                    .map(|t| self.typed(t, 1))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Ty::Struct(i) => self.struct_lit(*i, 1),
            Ty::Enum(i) => {
                let case = self.s.below(self.enums[*i].cases.len());
                self.member_value(&Ty::Member(*i, case))
            },
            Ty::Member(e, c) => self.member_value(&Ty::Member(*e, *c)),
            Ty::Tag(i) => self.tags[*i].clone(),
            Ty::Union(ms) => {
                let pick = ms[self.s.below(ms.len())].clone();
                self.leaf(&pick)
            },
            Ty::Fn(..) => "() => 0".to_owned(),
        }
    }

    fn int_lit(&mut self) -> String {
        let n = self.s.below(200);
        match self.s.below(10) {
            0 => format!("{n}i64"),
            1 => format!("{n}u32"),
            2 => format!("0x{n:x}"),
            3 => format!("0b{:b}", n & 0xf),
            4 => format!("1_0{n}"),
            _ => n.to_string(),
        }
    }

    fn float_lit(&mut self) -> String {
        match self.s.below(4) {
            0 => "2.5".to_owned(),
            1 => "1e3".to_owned(),
            2 => "0.25f64".to_owned(),
            _ => "-1.5".to_owned(),
        }
    }

    /// A single-line string literal over a small text vocabulary.
    fn str_lit(&mut self) -> String {
        let body = match self.s.below(4) {
            0 => "plain text".to_owned(),
            1 => "with a 'quote'".to_owned(),
            2 => String::new(),
            _ => "alpha".to_owned(),
        };
        format!("\"{body}\"")
    }

    /// An interpolated string whose holes draw printable primitives from
    /// scope (holes never carry nested strings, which would break the
    /// single-line spelling).
    fn interp_str(&mut self) -> String {
        let printable: Vec<&Local> = self
            .locals
            .iter()
            .filter(|l| matches!(l.ty, Ty::Int | Ty::Float | Ty::Str | Ty::Bool | Ty::Char))
            .collect();
        let (a, b) = if printable.is_empty() {
            ("${1 + 2}".to_owned(), "3".to_owned())
        } else {
            let i = self.s.below(printable.len());
            let first = printable[i].name.clone();
            let second = printable[(i + 1) % printable.len()].name.clone();
            let hole =
                if self.s.roll(2) { format!("${first}") } else { format!("${{{first} + 1}}") };
            (hole, format!("${second}"))
        };
        format!("\"v {a} w {b}\"")
    }

    /// The value expression constructing one atom: payload members take
    /// their constructor call, structs their literal.
    fn member_value(&mut self, ty: &Ty) -> String {
        match ty {
            Ty::Member(e, c) => {
                let case = self.enums[*e].cases[*c].clone();
                match &case.payload {
                    None => case.name.clone(),
                    Some(Payload::Tuple(ts)) => format!(
                        "{}({})",
                        case.name,
                        ts.iter()
                            .map(|t| self.leaf(t))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    Some(Payload::Fields(fs)) => format!(
                        "{}({})",
                        case.name,
                        fs.iter()
                            .map(|(f, t)| format!("{f}: {}", self.leaf(t)))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                }
            },
            Ty::Struct(i) => self.struct_lit(*i, 2),
            other => self.leaf(other),
        }
    }

    /// A struct literal with every field, in declaration order; a field
    /// whose name is in scope with the right type may use the shorthand.
    fn struct_lit(&mut self, i: usize, depth: u32) -> String {
        let decl = self.structs[i].clone();
        let parts: Vec<String> = decl
            .fields
            .iter()
            .map(|(f, t)| {
                let shorthand = self
                    .locals
                    .iter()
                    .rev()
                    .any(|l| l.name == *f && self.render_ty(&l.ty) == self.render_ty(t));
                if shorthand && self.s.roll(2) {
                    f.clone()
                } else {
                    format!("{f}: {}", self.typed(t, depth.saturating_sub(1)))
                }
            })
            .collect();
        format!("{} {{ {} }}", decl.name, parts.join(", "))
    }

    /// An in-scope binding whose atoms fit the wanted type (equal, or a
    /// member of the wanted union). Latest binding wins.
    fn local_for(&mut self, ty: &Ty) -> Option<String> {
        self.locals
            .iter()
            .rev()
            .find(|l| {
                let l_keys = self.atom_keys(&l.ty);
                let want = self.atom_keys(ty);
                !l_keys.is_empty() && l_keys.iter().all(|k| want.contains(k))
            })
            .map(|l| l.name.clone())
    }

    /// A well-typed call to a declared fn returning a fitting type:
    /// direct, UFCS over a receiver local, a trailing-lambda block over
    /// a fn-typed last parameter, or a method dot-call from an impl.
    fn call_for(&mut self, want: &Ty, depth: u32) -> Option<String> {
        let candidates: Vec<usize> = self
            .fns
            .iter()
            .enumerate()
            .filter(|(_, f)| {
                f.ret
                    .as_ref()
                    .is_some_and(|r| self.contained(r, want) || self.contained(want, r))
            })
            .map(|(i, _)| i)
            .collect();
        if !candidates.is_empty() && self.s.roll(3) {
            let i = candidates[self.s.below(candidates.len())];
            let callee = self.fns[i].clone();
            if callee.params.is_empty() {
                return Some(format!("{}()", callee.name));
            }
            // UFCS: a local matching the first parameter takes its place
            if self.s.roll(3)
                && let Some(recv) = self.local_for(&callee.params[0].ty)
            {
                let rest = self.args_for(&callee.params[1..], depth);
                return Some(format!("{recv}.{}({rest})", callee.name));
            }
            // the trailing-lambda block over a fn-typed last parameter
            // (zero-parameter fn types only: the block is a zero-param
            // lambda literal, and the literal's arity must match the
            // expected fn type's exactly)
            if let Some(last) = callee.params.last()
                && let Ty::Fn(ps, fret) = &last.ty
                && ps.is_empty()
                && self.s.roll(4)
            {
                let head = self.args_for(&callee.params[..callee.params.len() - 1], depth);
                let body = self.typed(fret, depth - 1);
                return if head.is_empty() {
                    Some(format!("{} {{ {body} }}", callee.name))
                } else {
                    Some(format!("{}({head}) {{ {body} }}", callee.name))
                };
            }
            let args = self.args_for(&callee.params, depth);
            return Some(format!("{}({args})", callee.name));
        }
        // a dot-call through a registered impl method
        if self.s.roll(4) && !self.methods.is_empty() {
            let (m, recv_ty, ret) = self.methods[self.s.below(self.methods.len())].clone();
            if self.contained(&ret, want)
                && let Some(recv) = self.local_for(&recv_ty)
            {
                return Some(format!("{recv}.{m}()"));
            }
        }
        None
    }

    /// Arguments for a parameter list: matching expressions, with
    /// defaulted trailing parameters occasionally omitted and one wrong
    /// argument occasionally injected (the mismatch path).
    fn args_for(&mut self, params: &[Param], depth: u32) -> String {
        let mut parts = Vec::new();
        for (i, p) in params.iter().enumerate() {
            let last = i + 1 == params.len();
            if last && p.default.is_some() && self.s.roll(3) {
                break;
            }
            if self.s.roll(24) {
                let wrong = self.pick_base();
                parts.push(self.typed(&wrong, depth - 1));
            } else {
                parts.push(self.typed(&p.ty, depth - 1));
            }
        }
        parts.join(", ")
    }

    /// A `?`-anchored tail whose operand union is contained in the fn's
    /// return: a call to a declared fn or a receiver local.
    fn anchored_try(&mut self, ret: &Ty) -> Option<String> {
        for _ in 0..2 {
            if !self.fns.is_empty() && self.s.roll(2) {
                let i = self.s.below(self.fns.len());
                let callee = self.fns[i].clone();
                let Some(callee_ret) = &callee.ret else {
                    continue;
                };
                if !self.contained(callee_ret, ret) {
                    continue;
                }
                let args = self.args_for(&callee.params, 3);
                return Some(format!("{}({args})?", callee.name));
            }
            if let Some(l) = self
                .locals
                .iter()
                .rev()
                .find(|l| matches!(l.ty, Ty::Union(_)))
                .cloned()
                && self.contained(&l.ty, ret)
            {
                return Some(format!("{}?", l.name));
            }
        }
        None
    }
}
