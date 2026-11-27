//! The lowering plan: the planned function records, the plan's own minting
//! (free generics, impl and default bodies, lambdas, spawns, deinits), the
//! capture/cancel discovery walks, and the runtime-pair demand walk every
//! call site mints evidence through.

use super::*;

/// One planned function: its table name plus the lambda that is its
/// body — a named top-level const's init, or a value-position lambda.
/// The lambda is an owned clone preserving the node's spans, so the
/// checker's span-keyed tables resolve against it unchanged and the
/// walk's borrows never outlive one lowering.
#[derive(Clone)]
pub(crate) struct PlannedFn {
    pub(crate) name: String,
    pub(crate) lambda: Expr,
    /// The package definition the function names; `None` for a
    /// synthesized lambda value (no package def exists, and nothing
    /// resolves to it by name — its word calls) and for an impl-member
    /// function (a member declares no package def — its identity is the
    /// mangled table name).
    pub(crate) def: Option<DefId>,
    /// The deinit body's block span when this is a struct's synthesized
    /// deinit function; the checked type recorded there is the unit
    /// return, and the one `self` parameter binds the struct's atom.
    /// `None` for every other planned function.
    pub(crate) deinit: Option<Span>,
    /// The enclosing bindings a synthesized lambda captures, in
    /// first-use order: binder span plus declared type. Always empty
    /// for named functions — a named body resolves its own names.
    pub(crate) captures: Rc<Vec<(Span, Ty)>>,
    /// A declared impl-member body, when this entry is one: the walk
    /// lowers the declaration's params (the `Self` receiver among them)
    /// and block body instead of a lambda's shape. `lambda` still
    /// carries the body block, so its span names the lowering's file.
    pub(crate) decl: Option<Rc<DeclFn>>,
    /// The checker-solved return type, set for declared bodies: a member
    /// function has no def-typed entry the signature path reads.
    pub(crate) ret: Option<Ty>,
    /// The runtime-evidence pairs this frame's body demands, in body
    /// order — a generic body's own sites, every demand lexically
    /// inside its lambdas and spawned subtrees, and every generic
    /// callee's own demands its call sites' records answer in the
    /// runtime form (a lambda's or spawned child's entry carries its own
    /// body's collection when the entry is one). Empty outside generic
    /// bodies.
    pub(crate) evidence: Rc<Vec<RuntimePair>>,
    /// The declared bounds of the generic impl whose member body this
    /// entry lowers (empty elsewhere): a lambda or spawned child
    /// lexically inside the body inherits them, so its own demands
    /// collect at its construction or spawn site.
    pub(crate) bounds: Rc<HashMap<String, HashSet<TraitId>>>,
    /// Whether this entry is a `spawn` expression's child: the
    /// capture block is the entry's own argument area — the spawned
    /// fiber's entry frame writes it exactly where a run entry writes
    /// its parameters, evidence words trailing the captures.
    pub(crate) spawn_child: bool,
    /// Whether this entry is a free generic fn's ONE body: its
    /// declared parameters may sit over the fn's own type variables
    /// (one Raw word each — the runtime kind is the caller's fact), and
    /// every call site entering it stages the body's runtime-evidence
    /// demands as trailing words after the true arguments. False for
    /// impl-member declared bodies, whose parameters stay concrete.
    pub(crate) free_generic: bool,
    /// The instantiation this entry is: one body of a free generic fn,
    /// lowered over the shape an entering site solved. `None` for every
    /// other entry.
    pub(crate) instance: Option<Rc<Instance>>,
    /// The substitution this entry lowers under: its own instantiation's
    /// when it is an instance, the enclosing instance's for a lambda or a
    /// spawned child minted inside one, `None` everywhere else.
    pub(crate) subst: Option<Rc<Subst>>,
    /// The per-instance `for comptime` expansion this entry lowers
    /// under: the walked fn's definition and solved arguments. The
    /// instance entry carries its own; a lambda or spawned child minted
    /// inside one inherits it; every other entry is `None`.
    pub(crate) comptime_instance: Option<Rc<ds_check::type_of::InstanceScope>>,
}

impl PlannedFn {
    /// Whether call sites entering this body stage its runtime-evidence
    /// demands as trailing words: a free generic fn's body, the ONE form
    /// or an instance alike.
    pub(crate) fn is_generic_body(&self) -> bool {
        self.free_generic || self.instance.is_some()
    }
}

/// One declared body's lowering shape (an impl member's or a free
/// generic fn's): its parameters (binder facts live in the checked
/// tables, keyed by name span) and its body wrapped as the block
/// expression the tail walk lowers.
pub(crate) struct DeclFn {
    pub(crate) params: Rc<Vec<Param>>,
    pub(crate) body: Rc<Expr>,
}

/// One runtime-evidence demand inside a generic body (a generic impl
/// member's or a free generic fn's): the bound type variable a nested
/// trait-method call receives on, plus the member it calls. The body's
/// plan carries the pairs in body order; the call site that enters the
/// body stages one function-table word per pair — the serving member
/// body under its own substitution — and the nested site dispatches
/// through the word at its frame slot.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct RuntimePair {
    /// The declaration's generic parameter name (`t` in `[t: shows]`):
    /// the instance or fn declaration and the body's checking mint their
    /// own variable atoms, so the demand keys by the written name they
    /// share.
    pub(crate) var: String,
    /// The trait member the body calls on it.
    pub(crate) method: MethodId,
}

/// A generic body's runtime-evidence environment: the demand
/// pairs the body's frame stages slots for, plus the declared bounds
/// they collected against — the bounds ride along so a lambda lexically
/// inside the body inherits them and collects its own demands at its
/// construction.
pub(crate) struct EvidenceEnv {
    pub(crate) pairs: Rc<Vec<RuntimePair>>,
    pub(crate) bounds: Rc<HashMap<String, HashSet<TraitId>>>,
}

/// One generic body the package's demand table keys: a free generic fn
/// by its definition, a generic impl's member by the impl item's span
/// and member name, a trait's declared default body by the trait's
/// definition and member name.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) enum BodyId {
    Free(DefId),
    Member { impl_site: Span, member: String },
    Default { trait_def: DefId, member: String },
}

/// Every generic body's runtime-evidence demands after the package-wide
/// propagation: a body's walk collects its own dot-calls plus, at every
/// call entering another generic body (a free generic fn or a generic
/// inherent member), that callee's own demands whose per-call record
/// answers in the runtime form — keyed onto the calling frame's
/// variables, the same peel the staging's slot lookup runs. A callee's
/// demands may themselves carry callee demands, so the pass iterates
/// until no body's list grows; the walk always reads a callee's list
/// through this table, keyed by the callee's identity.
#[derive(Default, Clone)]
pub(crate) struct Demands {
    pub(super) pairs: HashMap<BodyId, Rc<Vec<RuntimePair>>>,
}

impl Demands {
    pub(crate) fn insert(&mut self, id: BodyId, pairs: Rc<Vec<RuntimePair>>) {
        self.pairs.insert(id, pairs);
    }

    pub(crate) fn get(&self, id: &BodyId) -> Option<&[RuntimePair]> {
        self.pairs.get(id).map(|pairs| pairs.as_slice())
    }

    /// One body's list as an `Rc` clone (the planning pushes move it
    /// into their `EvidenceEnv`).
    pub(crate) fn rc(&self, id: &BodyId) -> Option<Rc<Vec<RuntimePair>>> {
        self.pairs.get(id).cloned()
    }
}

/// One staged evidence word's source: an evidence-chain object's
/// table index (the entering site's substitution is concrete), or a
/// frame slot read (a nested site inside a generic body whose
/// substitution is that frame's own — the word rides the frame's
/// evidence area).
#[derive(Clone, Copy)]
pub(super) enum EvidenceSrc {
    /// The evidence-chain object's index in the module's chain table.
    Const(u32),
    /// This frame's evidence-area word (a `Mov` into the overlay).
    Slot(u8),
    /// The dead sentinel: a demand the entering branch's head never
    /// mentions, staged as a word naming no chain object (the nested
    /// dispatch that reads it can only run under a narrowing that branch
    /// cannot satisfy).
    Dead,
}

/// A solved trait-method call site's dispatch.
pub(crate) enum TraitDispatch {
    /// The impl body the checker's solved instance names: the body's
    /// function-table index plus the checker-recorded call (the declared
    /// instance id rides as the final staged word, the per-bound evidence
    /// words the body's runtime pairs demand stage ahead of it).
    Static { index: u32, call: TraitCall },
    /// The linked body the checker's solved instance names: the
    /// body lives in a linked package's module, so the FuncRef
    /// addresses the dep position and the linked module's own table —
    /// the same transfer a linked free fn's call takes, with the solved
    /// instance id riding as the final staged word exactly like the
    /// local body's (a linked body rides no evidence pairs: the walk
    /// refuses the generic and conditional impls, and the trait's
    /// default bodies cross only when they stage none). The body is
    /// the trait's declared default when the solving impl spells no
    /// member — the receiver then claims the `Self` variable's single
    /// Raw word, so the staging records it.
    Foreign {
        position: u32,
        func: u32,
        call: TraitCall,
        default_body: bool,
    },
    /// A bound-variable receiver inside a generic impl body: dispatch
    /// resolves at runtime from the frame's evidence slot the call site
    /// entering the body staged. The trait member the evidence pair
    /// names rides along so omitted defaulted parameters stage the
    /// trait's declared defaults (the call-site contract).
    Runtime { slot: u8, method: MethodId },
    /// A solved inherent-method call site: the impl body's
    /// function-table index plus the checker-recorded bounds when the
    /// body is a generic impl's (empty for a monomorphic impl's), the
    /// method id naming the impl item the transitive chain starts from.
    /// The receiver's own type picked the body at check time, so the call
    /// dispatches like a plain local callee — no instance word and no
    /// dispatch uop; a generic body's evidence words stage trailing the
    /// arguments and the body claims them as its trailing parameters.
    Inherent {
        index: u32,
        bounds: InherentCallBounds,
        method: MethodId,
    },
    /// A solved inherent-method call site whose body lives in a linked
    /// package's module (the type-keyed receiver seam): the FuncRef
    /// addresses the dep position and the linked module's own table,
    /// the same transfer a linked free fn's call takes — no instance
    /// word exists for an inherent impl, so the call stages exactly
    /// the written arguments and dispatches through a plain CALL. A
    /// linked member body is monomorphic by the producer's walk (the
    /// generic and conditional shapes refuse the crossing), so no
    /// evidence words stage either.
    InherentForeign {
        position: u32,
        func: u32,
        method: MethodId,
    },
}

/// The module plan: lowered functions in function-table order.
pub(crate) struct Plan {
    pub(super) functions: Vec<PlannedFn>,
    pub(super) index: HashMap<DefId, u32>,
    /// Impl-member bodies keyed by (impl item span, member name): a
    /// trait-method call site reaches its body through the solved
    /// instance's impl item, never through a package def.
    pub(super) impl_index: HashMap<(Span, String), u32>,
    /// The trait's declared default bodies keyed by (trait definition,
    /// member name): a dispatch whose solving impl spells no member
    /// reaches the trait's default body through this tie.
    pub(super) default_index: HashMap<(DefId, String), u32>,
    /// The function-table indexes of every planned trait default body.
    pub(super) default_bodies: HashSet<u32>,
    /// The per-shape instances of a trait's declared default bodies:
    /// their Self-keyed demands stage through the default chain
    /// resolution, exactly the ONE body's own path.
    pub(super) default_instances: HashSet<u32>,
    /// The trait default bodies a link surface may carry, keyed by
    /// mangled name: exactly the rows staging no runtime evidence, so
    /// a consumer enters them staging the true arguments and the
    /// solved instance word alone — the demanded bodies stay out (the
    /// evidence-chain machinery names module-local bodies).
    pub(super) carried_defaults: HashMap<String, u32>,
    /// The function-table index of every planned struct deinit, keyed
    /// by the struct's definition.
    pub(super) deinits: HashMap<DefId, u32>,
    /// Every struct whose declaration carries a `deinit` member,
    /// planned or not (a generic struct's deinit declares but cannot
    /// plan — monomorphic instantiation is the CALL_GENERIC seam).
    pub(super) declared_deinits: HashSet<DefId>,
    /// Every generic body's final demand table: the planning pushes read
    /// their own body's list here, and the lambda/spawn walks at
    /// lowering time read callee bodies' lists through it.
    pub(super) demands: Demands,
    /// The module's evidence-chain objects, built during lowering
    /// as the entering sites walk their chains: entry `i` is what a
    /// staged word carrying `i` names.
    pub(super) chains: Vec<ds_mod::EvidenceChain>,
    /// The memo the chain walk keys its states by (member, instance,
    /// substitution): a repeated state shares its entry, which is also
    /// the walk's termination — a cyclic demand graph builds a cyclic
    /// table the dispatches walk lazily, one hop per call.
    pub(super) chain_memo: HashMap<(MethodId, InstanceId, Option<Vec<TypeId>>), u32>,
    /// Every minted record's site answer, keyed by the lambda's or
    /// spawn's own span inside its enclosing entry: the table index, the
    /// capture list, and the evidence pairs the record's body stages —
    /// the facts the minting site's walk reads instead of minting, so the
    /// walk and the plan hold one mint truth. The enclosing entry keys
    /// too: one lambda site mints one record per instantiation of the
    /// body holding it.
    pub(super) mints: HashMap<(Span, u32), Minted>,
    /// The defs whose initializers planned as the evaluation slice's
    /// zero-parameter bodies: a value use of one keeps the top-level
    /// refusal (the planned entry answers the fenced run, it is not a
    /// callable the runtime half mints a word for).
    pub(super) const_defs: HashSet<DefId>,
    /// Every declared free generic fn, keyed by its definition: a body
    /// that holds no table entry of its own and lowers once per shape
    /// its entering sites solve (the discovery plans the instances).
    pub(super) generics: HashMap<DefId, Rc<GenericFn>>,
    /// Every member body of a generic impl, keyed by the impl item's
    /// span and the member name: a body that holds no table entry of
    /// its own until a site enters it or a runtime-evidence demand
    /// names its member (the discovery plans the instances and the
    /// ONE Raw-word bodies).
    pub(super) generic_members: HashMap<(Span, String), Rc<GenericMember>>,
    /// A trait's declared default bodies, keyed by the trait's
    /// definition and member: the same per-shape instantiation story an
    /// impl's generic members run, over the trait's `Self` variable.
    pub(super) generic_defaults: HashMap<(DefId, String), Rc<GenericMember>>,
    /// The instance of each generic member at one shape key: the
    /// function table's index of the body lowered over that shape.
    pub(super) member_instances: HashMap<(Span, String, Rc<InstanceKey>), u32>,
    /// The same instances by their entering site's receiver type: what a
    /// lowering site looks its target up by, the handle its lens reads.
    pub(super) member_instance_args: HashMap<(Span, String, Ty), u32>,
    /// Every generic member's recorded types, in a fixed order: the
    /// declared parameters, the solved return, and every type the
    /// checker recorded inside the body's block span.
    pub(super) member_types: HashMap<(Span, String), Rc<[Ty]>>,
    /// The instance of each generic fn at one shape key: the function
    /// table's index of the body lowered over that shape.
    pub(super) instances: HashMap<(DefId, Rc<InstanceKey>), u32>,
    /// The same instances by their solved argument types: what an
    /// entering site looks its target up by.
    pub(super) instance_args: HashMap<(DefId, Vec<Ty>), u32>,
    /// Every generic fn's recorded types, in a fixed order: the signature
    /// and every type the checker recorded inside the body's span.
    pub(super) body_types: HashMap<DefId, Rc<[Ty]>>,
    /// The instances whose evidence words this module bakes statically:
    /// a dispatch site's solved instance word or an evidence-chain route
    /// naming the impl's body. The link layering reads the set as the
    /// internal-exercise fact — an artifact carrying one cannot have the
    /// impl shadowed under a later winner.
    pub(super) exercised: HashSet<u32>,
}

/// One minted record's plan row: the facts keyed by the site's span.
pub(crate) struct Minted {
    /// The record's function-table index.
    pub(super) index: u32,
    /// The record's capture list, in first-use order.
    pub(super) captures: Rc<Vec<(Span, Ty)>>,
    /// The record's runtime-evidence demands.
    pub(super) evidence: Rc<Vec<RuntimePair>>,
}

/// One mint site's plan answer: what the lowering site reads where it
/// once minted mid-walk.
pub(crate) struct MintSite {
    /// The record's function-table index.
    pub(crate) index: u32,
    /// The record's capture list, in first-use order.
    pub(crate) captures: Rc<Vec<(Span, Ty)>>,
    /// The record's runtime-evidence demands.
    pub(crate) evidence: Rc<Vec<RuntimePair>>,
}

impl Plan {
    pub(crate) fn new() -> Self {
        Self {
            functions: Vec::new(),
            index: HashMap::new(),
            impl_index: HashMap::new(),
            default_index: HashMap::new(),
            default_bodies: HashSet::new(),
            default_instances: HashSet::new(),
            carried_defaults: HashMap::new(),
            deinits: HashMap::new(),
            declared_deinits: HashSet::new(),
            demands: Demands::default(),
            chains: Vec::new(),
            chain_memo: HashMap::new(),
            mints: HashMap::new(),
            const_defs: HashSet::new(),
            generics: HashMap::new(),
            generic_members: HashMap::new(),
            generic_defaults: HashMap::new(),
            member_instances: HashMap::new(),
            member_instance_args: HashMap::new(),
            member_types: HashMap::new(),
            instances: HashMap::new(),
            instance_args: HashMap::new(),
            body_types: HashMap::new(),
            exercised: HashSet::new(),
        }
    }

    /// Records one instance's evidence word as statically baked: the
    /// internal-exercise fact a consuming link reads before shadowing.
    pub(crate) fn mark_exercised(&mut self, instance: u32) {
        self.exercised.insert(instance);
    }

    /// The statically baked instances, in no order.
    #[must_use]
    pub fn exercised(&self) -> &HashSet<u32> {
        &self.exercised
    }

    /// Installs the package's propagated demand table before any body
    /// plans.
    pub(crate) fn set_demands(&mut self, demands: Demands) {
        self.demands = demands;
    }

    /// One generic body's final demands: a body outside the table (a
    /// boundless one) demands nothing.
    pub(crate) fn demand(&self, id: &BodyId) -> Rc<Vec<RuntimePair>> {
        self.demands.rc(id).unwrap_or_else(|| Rc::new(Vec::new()))
    }

    /// Registers one dep-side evaluation site's initializer as its own
    /// zero-parameter module function: the declared-body shape the
    /// fenced run drives for the const's value, the const's checked type
    /// its solved return (no fn def type names it).
    pub(crate) fn push_const(
        &mut self,
        name: String,
        def: DefId,
        local: &LocalDecl,
        analyzed: &Analyzed,
    ) {
        self.index.insert(
            def,
            u32::try_from(self.functions.len()).expect("function index fits u32"),
        );
        self.const_defs.insert(def);
        let ret = analyzed
            .checked
            .def_types
            .get(&def)
            .copied()
            .expect("the planning gate checked a typed value");
        self.functions.push(PlannedFn {
            name,
            lambda: local.init.clone(),
            def: Some(def),
            deinit: None,
            captures: Rc::new(Vec::new()),
            decl: Some(Rc::new(DeclFn {
                params: Rc::new(Vec::new()),
                body: Rc::new(local.init.clone()),
            })),
            ret: Some(ret),
            evidence: Rc::new(Vec::new()),
            bounds: Rc::new(HashMap::new()),
            spawn_child: false,
            free_generic: false,
            instance: None,
            subst: None,
            comptime_instance: None,
        });
    }

    pub(crate) fn push(&mut self, name: String, def: DefId, local: &LocalDecl) {
        self.index.insert(
            def,
            u32::try_from(self.functions.len()).expect("function index fits u32"),
        );
        self.functions.push(PlannedFn {
            name,
            lambda: local.init.clone(),
            def: Some(def),
            deinit: None,
            captures: Rc::new(Vec::new()),
            decl: None,
            ret: None,
            evidence: Rc::new(Vec::new()),
            bounds: Rc::new(HashMap::new()),
            spawn_child: false,
            free_generic: false,
            instance: None,
            subst: None,
            comptime_instance: None,
        });
    }

    /// Registers one value-position lambda as a synthesized function
    /// inside the enclosing entry and returns its table index. The
    /// captures list was collected by the caller: the discovery and the
    /// body share the one list, so the staged block and the frame layout
    /// cannot drift. The minted name keys off the enclosing function's
    /// name and the table index — `$` is outside the identifier charset,
    /// so the name never shadows a user-declared one.
    pub(crate) fn push_lambda(
        &mut self,
        enclosing: u32,
        lambda: &Expr,
        captures: Rc<Vec<(Span, Ty)>>,
        evidence: Rc<Vec<RuntimePair>>,
        bounds: Rc<HashMap<String, HashSet<TraitId>>>,
        subst: Option<Rc<Subst>>,
    ) -> u32 {
        let index = u32::try_from(self.functions.len()).expect("function index fits u32");
        let name = format!("{}${index}", self.functions[enclosing as usize].name);
        let comptime_instance = self.functions[enclosing as usize].comptime_instance.clone();
        self.functions.push(PlannedFn {
            name,
            lambda: lambda.clone(),
            def: None,
            deinit: None,
            captures: captures.clone(),
            decl: None,
            ret: None,
            evidence: evidence.clone(),
            bounds,
            spawn_child: false,
            free_generic: false,
            instance: None,
            subst,
            comptime_instance,
        });
        let Expr::Lambda { span, .. } = lambda else {
            unreachable!("push_lambda receives a lambda");
        };
        self.mints.insert((*span, enclosing), Minted {
            index,
            captures,
            evidence,
        });
        index
    }

    /// Registers one `spawn` expression's block as a synthesized child
    /// function inside the enclosing entry and returns its table index:
    /// the block becomes a zero-parameter lambda over the capture list
    /// the caller collected. The plan entry's return is the body's answer
    /// type — the value the frame's exit carries out for the join to
    /// collect in spawn order. A child lexically inside a generic body
    /// carries its own runtime-evidence pairs (staged from the spawning
    /// frame's slots into the entry argument area) and inherits the
    /// body's bounds so its nested lambdas and spawns collect their
    /// demands the same way.
    pub(crate) fn push_spawn(
        &mut self,
        enclosing: u32,
        spawn: &Expr,
        captures: Rc<Vec<(Span, Ty)>>,
        ret: Ty,
        evidence: Rc<Vec<RuntimePair>>,
        bounds: Rc<HashMap<String, HashSet<TraitId>>>,
        subst: Option<Rc<Subst>>,
    ) -> u32 {
        let Expr::Spawn { span, body, .. } = spawn else {
            unreachable!("push_spawn receives a spawn expression");
        };
        let index = u32::try_from(self.functions.len()).expect("function index fits u32");
        let lambda = Expr::Lambda {
            span: *span,
            generics: Vec::new(),
            params: Vec::new(),
            ret: None,
            arrow: false,
            body: Box::new(Expr::Block(body.clone())),
        };
        let name = format!("{}${index}", self.functions[enclosing as usize].name);
        let comptime_instance = self.functions[enclosing as usize].comptime_instance.clone();
        self.functions.push(PlannedFn {
            name,
            lambda,
            def: None,
            deinit: None,
            captures: captures.clone(),
            decl: None,
            ret: Some(ret),
            evidence: evidence.clone(),
            bounds,
            spawn_child: true,
            free_generic: false,
            instance: None,
            subst,
            comptime_instance,
        });
        self.mints.insert((*span, enclosing), Minted {
            index,
            captures,
            evidence,
        });
        index
    }

    /// One mint site's plan answer: the record the discovery pushed for
    /// this lambda's or spawn's own span inside the entry `enclosing`,
    /// for the walk that reaches the site to stage instead of minting.
    pub(crate) fn minted(&self, enclosing: u32, span: Span) -> Option<MintSite> {
        let minted = self.mints.get(&(span, enclosing))?;
        Some(MintSite {
            index: minted.index,
            captures: minted.captures.clone(),
            evidence: minted.evidence.clone(),
        })
    }

    /// Records one declared free generic fn: its body holds no table
    /// entry until a call site enters it, the discovery planning the
    /// instance (or the ONE body) each site needs.
    pub(crate) fn register_generic(&mut self, generic: GenericFn) {
        self.generics.insert(generic.def, Rc::new(generic));
    }

    /// The declared free generic fn one definition names.
    pub(crate) fn generic_fn(&self, def: DefId) -> Option<Rc<GenericFn>> {
        self.generics.get(&def).cloned()
    }

    /// Registers one free generic fn's ONE body under its own name
    /// (a package def is unique, and the impl members' `#` mangles sit
    /// outside the identifier charset) and returns its table index: the
    /// fn lowers once over its variables' single Raw words, the form a
    /// call site still naming a variable enters. Its parameters and block
    /// become the declared body the walk lowers like an impl member's.
    /// `None` return: the checker's def type carries the fn's signature
    /// over its minted variables.
    pub(crate) fn push_free_generic(&mut self, generic: &GenericFn) -> u32 {
        let index = u32::try_from(self.functions.len()).expect("function index fits u32");
        self.index.insert(generic.def, index);
        let Expr::Lambda { params, body, .. } = &generic.lambda else {
            unreachable!("a free generic fn plans a lambda-armed local");
        };
        self.functions.push(PlannedFn {
            name: generic.name.clone(),
            lambda: generic.lambda.clone(),
            def: Some(generic.def),
            deinit: None,
            captures: Rc::new(Vec::new()),
            decl: Some(Rc::new(DeclFn {
                params: Rc::new(params.clone()),
                body: Rc::new((**body).clone()),
            })),
            ret: None,
            evidence: generic.pairs.clone(),
            bounds: generic.bounds.clone(),
            spawn_child: false,
            free_generic: true,
            instance: None,
            subst: None,
            comptime_instance: None,
        });
        index
    }

    /// Registers one impl-member function under its mangled table name
    /// and returns its index. The mangle `<trait>#<target>#<member>` is
    /// deterministic, and `#` sits outside the identifier charset, so a
    /// user-declared name can never collide; two impls of one trait over
    /// intersecting heads cannot both check clean, so the mangle is
    /// unique per body.
    pub(crate) fn push_declared(
        &mut self,
        name: String,
        impl_site: Span,
        member: String,
        params: Rc<Vec<Param>>,
        body: Rc<Expr>,
        ret: Ty,
        evidence: EvidenceEnv,
    ) -> u32 {
        let index = u32::try_from(self.functions.len()).expect("function index fits u32");
        self.impl_index.insert((impl_site, member), index);
        self.functions.push(PlannedFn {
            name,
            lambda: (*body).clone(),
            def: None,
            captures: Rc::new(Vec::new()),
            decl: Some(Rc::new(DeclFn { params, body })),
            ret: Some(ret),
            deinit: None,
            evidence: evidence.pairs,
            bounds: evidence.bounds,
            spawn_child: false,
            free_generic: false,
            instance: None,
            subst: None,
            comptime_instance: None,
        });
        index
    }

    /// Registers one trait's declared default body under its mangled
    /// table name and returns its index. The mangle `<trait>#<member>`
    /// cannot collide with an impl member's three-part mangle or an
    /// inherent impl's two-part one (a trait and a type share one
    /// namespace, and `#` sits outside the identifier charset). The
    /// body is the ONE generic shape over the trait's own variables:
    /// its declared parameters may sit over them (`self` typed `Self`
    /// among them — one Raw word each, the runtime kind the caller's
    /// fact), and the dispatch sites entering it stage the body's
    /// runtime-evidence demands as trailing words after the true
    /// arguments, the same shape a free generic fn's entering sites
    /// stage.
    pub(crate) fn push_default_body(
        &mut self,
        name: String,
        trait_def: DefId,
        member: String,
        params: Rc<Vec<Param>>,
        body: Rc<Expr>,
        ret: Option<Ty>,
        evidence: EvidenceEnv,
    ) -> u32 {
        let index = u32::try_from(self.functions.len()).expect("function index fits u32");
        self.default_index.insert((trait_def, member), index);
        self.default_bodies.insert(index);
        if evidence.pairs.is_empty() {
            self.carried_defaults.insert(name.clone(), index);
        }
        self.functions.push(PlannedFn {
            name,
            lambda: (*body).clone(),
            def: None,
            deinit: None,
            captures: Rc::new(Vec::new()),
            decl: Some(Rc::new(DeclFn { params, body })),
            ret,
            evidence: evidence.pairs,
            bounds: evidence.bounds,
            spawn_child: false,
            free_generic: true,
            instance: None,
            subst: None,
            comptime_instance: None,
        });
        index
    }

    /// The impl-member body a solved instance plus member name address.
    pub(crate) fn impl_body(&self, impl_site: Span, member: &str) -> Option<u32> {
        self.impl_index
            .get(&(impl_site, member.to_owned()))
            .copied()
    }

    /// The trait's declared default body for one member.
    pub(crate) fn default_body(&self, trait_def: DefId, member: &str) -> Option<u32> {
        self.default_index
            .get(&(trait_def, member.to_owned()))
            .copied()
    }

    /// Whether one function-table entry is a trait's declared default
    /// body.
    pub(crate) fn is_default_body(&self, index: u32) -> bool {
        self.default_bodies.contains(&index)
    }

    /// Whether one table entry is a trait default body's per-shape
    /// instance: the Self-keyed demands stage through the default chain
    /// resolution like the ONE body's.
    pub(crate) fn is_default_instance(&self, index: u32) -> bool {
        self.default_instances.contains(&index)
    }

    /// The trait default bodies a link surface may carry, keyed by
    /// mangled name: the rows staging no runtime evidence.
    pub(crate) fn carried_defaults(&self) -> &HashMap<String, u32> {
        &self.carried_defaults
    }

    /// Registers one struct's `deinit` member as a synthesized module
    /// function: one `self` parameter (borrowed, the dying instance)
    /// whose body is the declared block, named after the struct — `$`
    /// sits outside the identifier charset, so the name never shadows a
    /// user one. Every declared deinit registers here, planned or not:
    /// a struct whose deinit carries no plan (a generic struct's) makes
    /// binding its values refuse instead of silently skipping the hook.
    pub(crate) fn push_deinit(&mut self, struct_def: DefId, name: &str, lambda: Expr, body: Span) {
        let index = u32::try_from(self.functions.len()).expect("function index fits u32");
        self.declared_deinits.insert(struct_def);
        self.deinits.insert(struct_def, index);
        self.functions.push(PlannedFn {
            name: format!("{name}$deinit"),
            lambda,
            def: None,
            deinit: Some(body),
            captures: Rc::new(Vec::new()),
            decl: None,
            ret: None,
            evidence: Rc::new(Vec::new()),
            bounds: Rc::new(HashMap::new()),
            spawn_child: false,
            free_generic: false,
            instance: None,
            subst: None,
            comptime_instance: None,
        });
    }

    /// Records a struct whose declaration carries a `deinit` member but
    /// whose body has no synthesized function (a generic struct's).
    pub(crate) fn declare_deinit(&mut self, struct_def: DefId) {
        self.declared_deinits.insert(struct_def);
    }

    /// The function-table index of a struct's planned deinit.
    pub(crate) fn deinit_index(&self, def: DefId) -> Option<u32> {
        self.deinits.get(&def).copied()
    }

    /// Whether a struct's declaration carries a `deinit` member.
    pub(crate) fn declares_deinit(&self, def: DefId) -> bool {
        self.declared_deinits.contains(&def)
    }

    /// Every struct whose declaration carries a `deinit` member.
    pub(crate) fn declared_deinits(&self) -> HashSet<DefId> {
        self.declared_deinits.clone()
    }

    /// The planned function at one table index (the worklist read; the
    /// queue grows while function bodies lower).
    pub(crate) fn function(&self, index: usize) -> Option<&PlannedFn> {
        self.functions.get(index)
    }

    /// The function-table index of a planned DefId.
    pub(crate) fn index_of(&self, def: DefId) -> Option<u32> {
        self.index.get(&def).copied()
    }

    /// The instance one generic fn's concrete argument types entered, when
    /// the plan's discovery planned one: the call-site walk's target,
    /// shared with every other site solving the same shape.
    pub(crate) fn instance_target(&self, def: DefId, args: &[Ty]) -> Option<u32> {
        self.instance_args.get(&(def, args.to_vec())).copied()
    }

    pub(crate) fn functions(&self) -> &[PlannedFn] {
        &self.functions
    }

    /// The module's finished evidence-chain table, drained by the module
    /// build after every body lowered.
    pub(crate) fn take_chains(&mut self) -> Vec<ds_mod::EvidenceChain> {
        std::mem::take(&mut self.chains)
    }

    /// The widest evidence-pair count any planned body claims: a nested
    /// runtime dispatch site cannot know its runtime-resolved serving
    /// body's demand count, so its overlay reserves this bound (the
    /// dispatch itself stages exactly what the chain object carries).
    pub(crate) fn max_evidence_pairs(&self) -> usize {
        self.functions
            .iter()
            .map(|planned| planned.evidence.len())
            .max()
            .unwrap_or(0)
    }

    pub(crate) fn names(&self) -> Vec<String> {
        self.functions.iter().map(|f| f.name.clone()).collect()
    }

    pub(crate) fn has_main(&self) -> bool {
        self.functions.iter().any(|f| f.name == "main")
    }
}

/// The enclosing bindings one lambda body captures, in first-use order
/// every single-segment path or interpolation name inside the
/// subtree that resolves to a binder whose bind span starts strictly
/// before the lambda — the resolver binds in flow order, so every binder
/// of the subtree itself (the lambda's params, its body's locals, a
/// nested lambda's) binds inside it, and a nested lambda's outer use is
/// this one's capture transitively. The walk covers every expression
/// variant; a missed shape degrades to the missing-binder refusal at its
/// use, never a miscompile.
pub(super) fn collect_captures(
    lambda: &Expr,
    res: &Resolutions,
    binders: &HashMap<Span, BinderFact>,
) -> Vec<(Span, Ty)> {
    let start = lambda.span().start;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    visit_uses(lambda, &mut |span| {
        capture_use(span, start, res, binders, &mut seen, &mut out);
    });
    out
}

/// The earliest `return` or `?` sitting directly inside a block's
/// statements: never one nested inside a lambda or a spawned block —
/// their bodies are separate frames with their own exits. The nursery
/// emission refuses strays (the block's scope exit must join), and the
/// spawn emission refuses them (a spawned body completes with its own
/// value, never the enclosing function's).
pub(super) fn stray_exit(block: &BlockExpr) -> Option<Span> {
    block.stmts.iter().find_map(|stmt| match &stmt.kind {
        StmtKind::Local(local) => stray_exit_expr(&local.init),
        // a block-local `fn` body exits its own frame, never this block
        StmtKind::Fn(_) => None,
        StmtKind::Expr(expr) => stray_exit_expr(expr),
    })
}

/// Every `cancel` call's operand span inside one block's statements:
/// the payload sites a spawned body hands the boundary, collected for the
/// spawn site's containment proof. Lambdas and nested spawned blocks are
/// separate frames and stay skipped (their own sites answer their own
/// boundaries).
pub(super) fn cancel_operands(block: &BlockExpr, out: &mut Vec<Span>) {
    cancel_operands_walk(block, out, false);
}

/// The spawn-site containment proof's walk: a spawned block's own cancel
/// sites, recursing through nested nursery blocks (an over-conservative
/// proof is legal — a false site only adds a redundant check) but not
/// through nested spawns (their cancels answer their own boundary).
pub(super) fn cancel_operands_walk(block: &BlockExpr, out: &mut Vec<Span>, through_spawns: bool) {
    for stmt in &block.stmts {
        match &stmt.kind {
            StmtKind::Local(local) => {
                cancel_operands_expr(&local.init, out, through_spawns);
            },
            // a block-local `fn` body's cancels answer the fn's own frame
            StmtKind::Fn(_) => {},
            StmtKind::Expr(expr) => cancel_operands_expr(expr, out, through_spawns),
        }
    }
}

/// The boundary join's walk: the cancel sites whose payloads can answer
/// this block's own boundary — recursing through the spawned bodies (a
/// spawned child cancels into this nursery) but not into nested nursery
/// blocks (their cancels answer their own boundary, and their failures
/// re-raise as the child's completion value, never as a payload).
pub(super) fn spawned_cancel_operands(block: &BlockExpr, out: &mut Vec<Span>) {
    cancel_operands_walk(block, out, true);
}

pub(super) fn cancel_operands_expr(e: &Expr, out: &mut Vec<Span>, through_spawns: bool) {
    if let Expr::Call { callee, args, .. } = e
        && let Expr::Path { path, .. } = &**callee
        && path.segments.len() == 1
        && path.segments[0].name.as_str() == "cancel"
        && let Some(arg) = args.first()
    {
        // the ambient spelling is generic with exactly one parameter, and
        // a same-named user function plans (so the def check in the
        // emission refuses it); the collection is name-keyed on purpose —
        // a false site only adds a redundant containment proof
        out.push(arg.value.span());
    }
    match e {
        Expr::Paren { inner, .. }
        | Expr::Unary { expr: inner, .. }
        | Expr::Is { expr: inner, .. }
        | Expr::Try { expr: inner, .. } => cancel_operands_expr(inner, out, through_spawns),
        Expr::Binary { left, right, .. }
        | Expr::Range {
            start: left,
            end: right,
            ..
        }
        | Expr::Assign { left, right, .. } => {
            cancel_operands_expr(left, out, through_spawns);
            cancel_operands_expr(right, out, through_spawns);
        },
        Expr::Str { parts, .. } => {
            for part in parts {
                if let StrPart::Expr { expr, .. } = part {
                    cancel_operands_expr(expr, out, through_spawns);
                }
            }
        },
        Expr::Call { callee, args, .. } => {
            cancel_operands_expr(callee, out, through_spawns);
            for arg in args {
                cancel_operands_expr(&arg.value, out, through_spawns);
            }
        },
        Expr::Field { recv, .. } | Expr::FieldKey { recv, .. } | Expr::Index { recv, .. } => {
            cancel_operands_expr(recv, out, through_spawns);
        },
        Expr::MapApply { key, value, .. } => {
            cancel_operands_expr(key, out, through_spawns);
            cancel_operands_expr(value, out, through_spawns);
        },
        Expr::Struct { fields, .. } => {
            for (_, value) in fields {
                if let Some(value) = value {
                    cancel_operands_expr(value, out, through_spawns);
                }
            }
        },
        Expr::Tuple { items, .. } | Expr::List { items, .. } => {
            for item in items {
                cancel_operands_expr(item, out, through_spawns);
            }
        },
        Expr::If {
            cond, then, else_, ..
        } => {
            cancel_operands_expr(cond, out, through_spawns);
            cancel_operands_walk(then, out, through_spawns);
            if let Some(else_) = else_ {
                cancel_operands_expr(else_, out, through_spawns);
            }
        },
        Expr::Match {
            scrutinee, arms, ..
        } => {
            cancel_operands_expr(scrutinee, out, through_spawns);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    cancel_operands_expr(guard, out, through_spawns);
                }
                cancel_operands_expr(&arm.body, out, through_spawns);
            }
        },
        Expr::While { cond, body, .. } => {
            cancel_operands_expr(cond, out, through_spawns);
            cancel_operands_walk(body, out, through_spawns);
        },
        Expr::For { iter, body, .. } => {
            cancel_operands_expr(iter, out, through_spawns);
            cancel_operands_expr(body, out, through_spawns);
        },
        // the two traversal modes split exactly here: the proof walk
        // skips nested spawns and recurses into nurseries, the boundary
        // walk recurses into spawns and skips nurseries
        Expr::Nursery { body, .. } => {
            if !through_spawns {
                cancel_operands_walk(body, out, through_spawns);
            }
        },
        Expr::Spawn { body, .. } => {
            if through_spawns {
                cancel_operands_walk(body, out, through_spawns);
            }
        },
        Expr::Block(block) => cancel_operands_walk(block, out, through_spawns),
        Expr::Literal { .. }
        | Expr::RawStr { .. }
        | Expr::Path { .. }
        | Expr::FnType { .. }
        | Expr::Return { .. }
        | Expr::Break(_)
        | Expr::Continue(_)
        | Expr::Lambda { .. }
        | Expr::Defer { .. }
        // a conversion placeholder cancels no operand
        | Expr::Invalid(_) => {},
    }
}

/// One expression subtree's earliest direct exit, walking every container
/// kind; lambdas and spawned blocks are separate frames and stay skipped.
pub(super) fn stray_exit_expr(e: &Expr) -> Option<Span> {
    match e {
        Expr::Return { span, .. } | Expr::Try { span, .. } => Some(*span),
        Expr::Paren { inner, .. }
        | Expr::Unary { expr: inner, .. }
        | Expr::Is { expr: inner, .. } => stray_exit_expr(inner),
        Expr::Binary { left, right, .. } => {
            stray_exit_expr(left).or_else(|| stray_exit_expr(right))
        },
        Expr::Range { start, end, .. } => stray_exit_expr(start).or_else(|| stray_exit_expr(end)),
        Expr::Assign { left, right, .. } => {
            stray_exit_expr(left).or_else(|| stray_exit_expr(right))
        },
        Expr::Str { parts, .. } => parts.iter().find_map(|part| match part {
            StrPart::Expr { expr, .. } => stray_exit_expr(expr),
            StrPart::Text { .. } | StrPart::Ident { .. } => None,
        }),
        Expr::Call { callee, args, .. } => stray_exit_expr(callee)
            .or_else(|| args.iter().find_map(|arg| stray_exit_expr(&arg.value))),
        Expr::Field { recv, .. } | Expr::FieldKey { recv, .. } | Expr::Index { recv, .. } => {
            stray_exit_expr(recv)
        },
        Expr::MapApply { key, value, .. } => {
            stray_exit_expr(key).or_else(|| stray_exit_expr(value))
        },
        Expr::Struct { fields, .. } => fields
            .iter()
            .find_map(|(_, value)| value.as_ref().and_then(stray_exit_expr)),
        Expr::Tuple { items, .. } | Expr::List { items, .. } => {
            items.iter().find_map(stray_exit_expr)
        },
        Expr::If {
            cond, then, else_, ..
        } => stray_exit_expr(cond)
            .or_else(|| stray_exit(then))
            .or_else(|| else_.as_deref().and_then(stray_exit_expr)),
        Expr::Match {
            scrutinee, arms, ..
        } => stray_exit_expr(scrutinee).or_else(|| {
            arms.iter().find_map(|arm| {
                arm.guard
                    .as_ref()
                    .and_then(stray_exit_expr)
                    .or_else(|| stray_exit_expr(&arm.body))
            })
        }),
        Expr::While { cond, body, .. } => stray_exit_expr(cond).or_else(|| stray_exit(body)),
        Expr::For { iter, body, .. } => stray_exit_expr(iter).or_else(|| stray_exit_expr(body)),
        Expr::Nursery { body, .. } => stray_exit(body),
        Expr::Block(block) => stray_exit(block),
        Expr::Literal { .. }
        | Expr::RawStr { .. }
        | Expr::Path { .. }
        | Expr::FnType { .. }
        | Expr::Lambda { .. }
        | Expr::Spawn { .. }
        | Expr::Defer { .. }
        | Expr::Break(_)
        | Expr::Continue(_)
        // a conversion placeholder is no exit
        | Expr::Invalid(_) => None,
    }
}

/// One visited use span: an enclosing local's bind becomes one capture
/// entry, once, with its checked type.
pub(super) fn capture_use(
    span: Span,
    start: u32,
    res: &Resolutions,
    binders: &HashMap<Span, BinderFact>,
    seen: &mut HashSet<Span>,
    out: &mut Vec<(Span, Ty)>,
) {
    let Some(Res::Local { bind }) = res.get(span) else {
        return;
    };
    if bind.start >= start || !seen.insert(*bind) {
        return;
    }
    if let Some(fact) = binders.get(bind) {
        out.push((*bind, fact.ty));
    }
}

/// Visits every local-name use span inside one expression subtree: path
/// segments, interpolation idents, and punned struct-field names are the
/// use sites; every other variant recurses into its subexpressions.
pub(super) fn visit_uses(e: &Expr, visit: &mut dyn FnMut(Span)) {
    let block = |block: &BlockExpr, visit: &mut dyn FnMut(Span)| {
        for stmt in &block.stmts {
            match &stmt.kind {
                StmtKind::Local(local) => visit_uses(&local.init, visit),
                // a block-local `fn` body's uses are its own mint's
                // captures; the enclosing walk collects none of them
                StmtKind::Fn(_) => {},
                StmtKind::Expr(expr) => visit_uses(expr, visit),
            }
        }
    };
    match e {
        // a fn type in the constructor brackets is a type spelling; it
        // names no value, and a conversion placeholder names none either
        Expr::Literal { .. }
        | Expr::RawStr { .. }
        | Expr::Break(_)
        | Expr::Continue(_)
        | Expr::FnType { .. }
        | Expr::Invalid(_) => {},
        Expr::Str { parts, .. } => {
            for part in parts {
                match part {
                    StrPart::Text { .. } => {},
                    StrPart::Expr { expr, .. } => visit_uses(expr, visit),
                    // `$x` is a name use like a path segment
                    StrPart::Ident { name, .. } => visit(name.span),
                }
            }
        },
        Expr::Path { path, .. } => {
            if let [only] = path.segments.as_slice() {
                visit(only.span);
            }
        },
        // a nested lambda's body is walked here: its own outer uses are
        // captures of this lambda when they bind before it
        Expr::Lambda { body, .. } => visit_uses(body, visit),
        Expr::Paren { inner, .. }
        | Expr::Try { expr: inner, .. }
        | Expr::Unary { expr: inner, .. } => visit_uses(inner, visit),
        Expr::Struct { fields, .. } => {
            for (name, value) in fields {
                // a punned field names its local; an explicit value is
                // walked below either way
                visit(name.span);
                if let Some(value) = value {
                    visit_uses(value, visit);
                }
            }
        },
        Expr::Tuple { items, .. } | Expr::List { items, .. } => {
            for item in items {
                visit_uses(item, visit);
            }
        },
        Expr::Call {
            callee,
            args,
            trailing,
            ..
        } => {
            visit_uses(callee, visit);
            for arg in args {
                visit_uses(&arg.value, visit);
            }
            if let Some(block_arg) = trailing {
                visit_uses(block_arg, visit);
            }
        },
        Expr::Field { recv, name, .. } => {
            visit_uses(recv, visit);
            visit(name.span);
        },
        Expr::FieldKey { recv, name, .. } => {
            visit_uses(recv, visit);
            visit_uses(name, visit);
        },
        Expr::Index { recv, index, .. } => {
            visit_uses(recv, visit);
            visit_uses(index, visit);
        },
        Expr::MapApply { key, value, .. } => {
            visit_uses(key, visit);
            visit_uses(value, visit);
        },
        Expr::Is { expr, .. } => visit_uses(expr, visit),
        Expr::Binary { left, right, .. } | Expr::Assign { left, right, .. } => {
            visit_uses(left, visit);
            visit_uses(right, visit);
        },
        Expr::Range { start, end, .. } => {
            visit_uses(start, visit);
            visit_uses(end, visit);
        },
        Expr::If {
            cond, then, else_, ..
        } => {
            visit_uses(cond, visit);
            block(then, visit);
            if let Some(else_expr) = else_ {
                visit_uses(else_expr, visit);
            }
        },
        Expr::Match {
            scrutinee, arms, ..
        } => {
            visit_uses(scrutinee, visit);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    visit_uses(guard, visit);
                }
                visit_uses(&arm.body, visit);
            }
        },
        Expr::While { cond, body, .. } => {
            visit_uses(cond, visit);
            block(body, visit);
        },
        Expr::For { iter, body, .. } => {
            visit_uses(iter, visit);
            visit_uses(body, visit);
        },
        Expr::Return { expr, .. } => {
            if let Some(expr) = expr {
                visit_uses(expr, visit);
            }
        },
        Expr::Nursery { body, .. } | Expr::Defer { body, .. } | Expr::Spawn { body, .. } => {
            block(body, visit);
        },
        Expr::Block(inner) => block(inner, visit),
    }
}

/// Collects one frame's runtime-evidence demands, in body order: every
/// trait-method call whose receiver's checked type is the body's own
/// bound type variable (the checker satisfied it through the body's
/// in-scope bound, so no solved instance records there), plus every
/// generic callee's own demands the site's record answers in the
/// runtime form (below). Value-site lambda and spawned subtrees stay
/// in — their demands ride the child's own entry words (a lambda's
/// capture block, a spawned child's entry argument area), staged from
/// this frame's evidence slots at the construction or spawn site.
/// The plan-time discovery: one item's just-planned entries walked in
/// order before any body lowers. Every lambda-value and spawn site mints
/// its record into the table, and every call entering a free generic fn
/// plans the body it enters — an instance over the site's solved shape,
/// or the ONE body where the site still names a variable. Each record
/// lands after the entry holding its site, so one item's block stays
/// contiguous and append-stable — and the ambient's prefix (items,
/// deinits, their records, and instances alike) is a pure function of the
/// ambient's own trees, the property the served splice pins its bytes
/// against. The walk that reaches a site reads the plan's answer instead
/// of planning, so discovery and lowering hold one truth; a site the walk
/// reaches with no plan answer is the drift the discovery and the walk's
/// routing disagree on, refused loud at the site.
pub(crate) fn discover_item(
    plan: &mut Plan,
    found: &mut Discovery<'_, '_>,
    first: usize,
) -> Result<(), LowerError> {
    let last = plan.functions.len();
    for at in first..last {
        instance::walk_entry(
            plan,
            found,
            u32::try_from(at).expect("function index fits u32"),
        )?;
    }
    Ok(())
}

/// The discovery's shared inputs beyond the plan: the analyzed package,
/// the type lens the entering sites substitute under, and the structs
/// whose declaration carries a deinit (a deinit-carrying substitution
/// keys an instance by the struct itself).
pub(crate) struct Discovery<'a, 't> {
    pub(crate) analyzed: &'a Analyzed,
    pub(crate) types: &'t mut crate::types::Types<'a>,
    pub(crate) deinits: &'a HashSet<DefId>,
    /// The active `for comptime` copy path while the walk descends one
    /// copy's body, outer-to-inner copy indices; empty outside one.
    /// Every span-keyed record read routes through it: inside a copy
    /// the checker's facts live in the per-copy tables.
    pub(crate) comptime_path: Vec<u32>,
    /// The per-instance expansion the walk descends (the walked fn's
    /// definition and solved arguments), set from the planned entry
    /// `walk_entry` lowers; `None` outside one.
    pub(crate) comptime_instance: Option<Rc<ds_check::type_of::InstanceScope>>,
}

/// One planned entry's body expression: a declared body lowers its
/// block directly, every other entry is the lambda shape holding the
/// body.
pub(super) fn entry_body(entry: &PlannedFn) -> &Expr {
    if let Some(decl) = &entry.decl {
        return &decl.body;
    }
    let Expr::Lambda { body, .. } = &entry.lambda else {
        unreachable!("a non-declared entry plans a lambda shape");
    };
    body
}

/// The discovery walk's threaded context: the enclosing entry's table
/// index and name, its declared bounds, and the instantiation it lowers
/// under — the facts a nested record's own sites inherit, exactly as the
/// walking frame's state once fed the mint.
pub(super) struct MintCtx<'a> {
    pub(super) enclosing: u32,
    pub(super) enclosing_name: String,
    pub(super) bounds: Rc<HashMap<String, HashSet<TraitId>>>,
    pub(super) subst: Option<Rc<Subst>>,
    pub(super) res: &'a Resolutions,
}

/// The runtime-evidence demands one minted record's body stages: empty
/// outside a bounds-carrying body, the propagated pairs inside one — the
/// same rule the minting walk applied at the site.
pub(super) fn mint_evidence(
    plan: &Plan,
    analyzed: &Analyzed,
    ctx: &MintCtx,
    body: &Expr,
) -> Rc<Vec<RuntimePair>> {
    if ctx.bounds.is_empty() {
        return Rc::new(Vec::new());
    }
    Rc::new(runtime_pairs(
        analyzed,
        ctx.res,
        &ctx.bounds,
        body,
        &plan.demands,
    ))
}

/// The spawned child's answer type: the body's checked value, the
/// spawn's own type where the body diverges or answers nothing — the
/// same derivation the spawning walk applied at the site.
pub(super) fn spawn_return(analyzed: &Analyzed, span: Span, body: &BlockExpr) -> Option<Ty> {
    let answer = analyzed.checked.expr_types.get(&body.span).copied();
    let usable = answer
        .is_some_and(|val| !val.diverges && !analyzed.decls.universe.members(val.ty).is_empty());
    if usable {
        return answer.map(|val| val.ty);
    }
    analyzed.checked.expr_types.get(&span).map(|val| val.ty)
}

/// The enclosing bindings one lambda or spawned block captures, as the
/// body lowering them reads the types: under an instantiation each
/// capture's type is the substituted one.
pub(super) fn captures_under(
    found: &mut Discovery<'_, '_>,
    subst: Option<&Rc<Subst>>,
    lambda: &Expr,
    res: &Resolutions,
) -> Result<Vec<(Span, Ty)>, LowerError> {
    let captures = collect_captures(lambda, res, &found.analyzed.checked.binders);
    let Some(subst) = subst else {
        return Ok(captures);
    };
    captures
        .into_iter()
        .map(|(span, ty)| Ok((span, found.types.substitute(subst, ty)?)))
        .collect()
}

/// Whether this call site inlines its last lambda argument instead of
/// staging it as a hosted callable: the `.err`/`.ok` handlers and the
/// extraction family's lambda-taking members (`unwrap_or_else`,
/// `map_err`) lower the lambda's body at the call site, so the lambda
/// node itself never mints. The routing reads the same resolution rows
/// and declaration tables the call walk reads.
pub(super) fn inlines_lambda_arg(
    found: &Discovery<'_, '_>,
    plan: &Plan,
    res: &Resolutions,
    callee: &Expr,
    call_span: Span,
) -> bool {
    if let Expr::Field { name, .. } = callee
        && matches!(res.get(name.span), Some(Res::Handler))
    {
        return true;
    }
    let def = match callee {
        Expr::Path { span, .. } => match res.get(*span) {
            Some(Res::Def(def)) => Some(*def),
            _ => None,
        },
        Expr::Field { name, .. } => match res.get(name.span) {
            Some(Res::Dot(DotRes::Free { func, fed_param: 0 }))
                if !member_routed(found, call_span) =>
            {
                Some(*func)
            },
            Some(Res::Dot(DotRes::Module { func })) => Some(*func),
            _ => None,
        },
        _ => None,
    };
    extraction_lambda_taker(found.analyzed, plan, def)
}

/// Whether `def` is the extraction family's lambda-taking shape: the
/// compiler-wired @std identity, the two-parameter generic signature,
/// and the member whose handler runs on the handled side.
pub(super) fn extraction_lambda_taker(
    analyzed: &Analyzed,
    plan: &Plan,
    def: Option<DefId>,
) -> bool {
    let Some(def) = def else {
        return false;
    };
    let std_shape = match analyzed.package.foreign().get(&def) {
        Some(entry) => entry.std,
        None => plan.index_of(def).is_none(),
    };
    if !std_shape {
        return false;
    }
    let Some(sig) = analyzed.decls.fns.get(&def) else {
        return false;
    };
    if sig.generics.is_empty() || sig.params.len() != 2 {
        return false;
    }
    matches!(
        Extraction::of_name(&analyzed.package.def(def).name),
        Some(Extraction::UnwrapOrElse | Extraction::MapErr)
    )
}

/// The copy record at one span for the plan-side walks: inside an
/// active `for comptime` copy the checker's facts live in the
/// per-copy tables — the active expansion's instance records first,
/// the once-check's copies second. The lowering's own lens reads the
/// same precedence, so the plan and the walk hold one truth.
pub(super) fn copy_record_at<'a>(
    checked: &'a ds_check::type_of::Checked,
    instance: Option<&ds_check::type_of::InstanceScope>,
    path: &[u32],
    span: Span,
) -> Option<&'a ds_check::type_of::CopyRecords> {
    if path.is_empty() {
        return None;
    }
    if let Some((def, args)) = instance
        && let Some(rec) =
            checked
                .comptime_instance_records
                .get(&(*def, Rc::clone(args), path.to_vec(), span))
    {
        return Some(rec);
    }
    checked.comptime_records.get(&(path.to_vec(), span))
}

/// The copy count of the `for comptime` loop at `span`, the same
/// precedence the lowering's `comptime_loop_info` reads: the active
/// expansion's instance record first, the once-check's table second.
/// `None` walks the body once with no copy path — a per-instance loop
/// at a body lowering over its own variables, whose facts no plan
/// record carries (the lowering refuses the loop loudly there).
pub(super) fn loop_copy_count(
    checked: &ds_check::type_of::Checked,
    instance: Option<&ds_check::type_of::InstanceScope>,
    span: Span,
) -> Option<usize> {
    if let Some((def, args)) = instance
        && let Some(info) = checked
            .comptime_instance_loops
            .get(&(*def, Rc::clone(args), span))
    {
        return Some(info.fields.len());
    }
    let info = checked.comptime_loops.get(&span)?;
    if info.per_instance.is_some() {
        return None;
    }
    Some(info.fields.len())
}

/// The plan-side dispatch reads over the active copy view: a
/// copy-local site's records are per-copy facts the body-wide tables
/// never hold.
fn copy_trait_call<'a>(found: &Discovery<'a, '_>, span: Span) -> Option<TraitCall> {
    copy_record_at(
        &found.analyzed.checked,
        found.comptime_instance.as_deref(),
        &found.comptime_path,
        span,
    )
    .and_then(|rec| rec.trait_call.clone())
    .or_else(|| found.analyzed.checked.trait_calls.get(&span).cloned())
}

fn copy_inherent_call(found: &Discovery<'_, '_>, span: Span) -> Option<MethodId> {
    copy_record_at(
        &found.analyzed.checked,
        found.comptime_instance.as_deref(),
        &found.comptime_path,
        span,
    )
    .and_then(|rec| rec.inherent_call)
    .or_else(|| found.analyzed.checked.inherent_calls.get(&span).copied())
}

fn copy_hole_show(found: &Discovery<'_, '_>, span: Span) -> Option<MethodId> {
    copy_record_at(
        &found.analyzed.checked,
        found.comptime_instance.as_deref(),
        &found.comptime_path,
        span,
    )
    .and_then(|rec| rec.hole_show)
    .or_else(|| {
        found
            .analyzed
            .checked
            .hole_show_dispatches
            .get(&span)
            .copied()
    })
}

/// Whether the checker routed one dot-call onto the member tier: a
/// trait or inherent dispatch record answers at the call span (the
/// copy's own record inside a copy). The F3 free-tier fallback reads
/// the same test, so a routed dot-call never plans a free fn body.
pub(super) fn member_routed(found: &Discovery<'_, '_>, call_span: Span) -> bool {
    copy_trait_call(found, call_span).is_some() || copy_inherent_call(found, call_span).is_some()
}

/// The pre-order discovery walk itself.
pub(super) fn discover_expr(
    plan: &mut Plan,
    found: &mut Discovery<'_, '_>,
    ctx: &MintCtx,
    e: &Expr,
) -> Result<(), LowerError> {
    match e {
        Expr::Lambda { generics, body, .. } => {
            if !generics.is_empty() {
                return Ok(()); // the walk refuses a generic lambda value; nothing inside it lowers
            }
            let evidence = mint_evidence(plan, found.analyzed, ctx, body);
            let captures = captures_under(found, ctx.subst.as_ref(), e, ctx.res)?;
            let index = plan.push_lambda(
                ctx.enclosing,
                e,
                Rc::new(captures),
                evidence,
                ctx.bounds.clone(),
                ctx.subst.clone(),
            );
            let nested = MintCtx {
                enclosing: index,
                enclosing_name: format!("{}${index}", ctx.enclosing_name),
                bounds: ctx.bounds.clone(),
                subst: ctx.subst.clone(),
                res: ctx.res,
            };
            discover_expr(plan, found, &nested, body)?;
        },
        Expr::Spawn { span, body } => {
            let Some(ret) = spawn_return(found.analyzed, *span, body) else {
                return Ok(()); // no checked answer: the walk's own refusal owns the site
            };
            let ret = match &ctx.subst {
                Some(subst) => found.types.substitute(subst, ret)?,
                None => ret,
            };
            let evidence = mint_evidence(plan, found.analyzed, ctx, &Expr::Block(body.clone()));
            let captures = captures_under(
                found,
                ctx.subst.as_ref(),
                &Expr::Block(body.clone()),
                ctx.res,
            )?;
            let index = plan.push_spawn(
                ctx.enclosing,
                e,
                Rc::new(captures),
                ret,
                evidence,
                ctx.bounds.clone(),
                ctx.subst.clone(),
            );
            let nested = MintCtx {
                enclosing: index,
                enclosing_name: format!("{}${index}", ctx.enclosing_name),
                bounds: ctx.bounds.clone(),
                subst: ctx.subst.clone(),
                res: ctx.res,
            };
            discover_block(plan, found, &nested, body)?;
        },
        Expr::Call {
            span,
            callee,
            args,
            trailing,
            ..
        } => {
            let inlines = inlines_lambda_arg(found, plan, ctx.res, callee, *span);
            // A call entering a free generic fn plans the body it enters
            // before anything lowers: the instance over the site's solved
            // shape, or the ONE body of a site still naming a variable.
            instance::enter_site(plan, found, ctx, callee, *span)?;
            // A dispatch the checker solved onto a generic impl's member
            // body plans that body the same way: an instance over the
            // receiver's solved shape, the ONE body where the site still
            // names a variable. An interpolation hole's implicit `show`
            // dispatch carries its records at the hole's own span, so the
            // text arm below feeds those sites here too.
            let recv_span = match &**callee {
                Expr::Field { recv, .. } => Some(recv.span()),
                _ => None,
            };
            member::enter_member_site(plan, found, *span, recv_span)?;
            // The callee first, in the receiver-first order the walk
            // stages its arguments: a dot-call's receiver holds whole
            // call shapes (the fluent spelling `xs.map(f).len()`), and
            // the lambdas and spawns inside them mint here exactly
            // where the walk lowers them — callee shapes the walk
            // refuses (a computed callee) lower no module, so their
            // records never reach a table.
            discover_expr(plan, found, ctx, callee)?;
            let mut effective: Vec<&Expr> = args.iter().map(|arg| &arg.value).collect();
            if let Some(block) = trailing {
                effective.push(block);
            }
            let last_inlined = inlines
                && effective
                    .last()
                    .is_some_and(|arg| matches!(arg, Expr::Lambda { .. }));
            for (at, arg) in effective.iter().enumerate() {
                if last_inlined && at + 1 == effective.len() {
                    if let Expr::Lambda { body, .. } = arg {
                        discover_expr(plan, found, ctx, body)?;
                    }
                } else {
                    discover_expr(plan, found, ctx, arg)?;
                }
            }
        },
        Expr::Paren { inner, .. }
        | Expr::Unary { expr: inner, .. }
        | Expr::Is { expr: inner, .. }
        | Expr::Try { expr: inner, .. } => discover_expr(plan, found, ctx, inner)?,
        Expr::Binary { left, right, .. }
        | Expr::Range {
            start: left,
            end: right,
            ..
        }
        | Expr::Assign { left, right, .. } => {
            discover_expr(plan, found, ctx, left)?;
            discover_expr(plan, found, ctx, right)?;
        },
        Expr::Str { parts, .. } => {
            for part in parts {
                if let StrPart::Expr { expr, .. } = part {
                    discover_expr(plan, found, ctx, expr)?;
                }
                // An interpolation hole's implicit `show` dispatch is a
                // member site the checker recorded at the hole's own span:
                // plan the body it enters like any call site.
                let hole_span = match part {
                    StrPart::Expr { span, .. } | StrPart::Ident { span, .. } => Some(*span),
                    StrPart::Text { .. } => None,
                };
                if let Some(hole_span) = hole_span
                    && copy_hole_show(found, hole_span).is_some()
                {
                    let recv_span = match part {
                        StrPart::Expr { expr, .. } => Some(expr.span()),
                        StrPart::Ident { name, .. } => Some(name.span),
                        StrPart::Text { .. } => None,
                    };
                    member::enter_member_site(plan, found, hole_span, recv_span)?;
                }
            }
        },
        Expr::Struct { fields, .. } => {
            for (_, value) in fields {
                if let Some(value) = value {
                    discover_expr(plan, found, ctx, value)?;
                }
            }
        },
        Expr::Tuple { items, .. } | Expr::List { items, .. } => {
            for item in items {
                discover_expr(plan, found, ctx, item)?;
            }
        },
        Expr::Field { recv, .. } => discover_expr(plan, found, ctx, recv)?,
        Expr::FieldKey { recv, name, .. } => {
            discover_expr(plan, found, ctx, recv)?;
            discover_expr(plan, found, ctx, name)?;
        },
        Expr::Index { recv, index, .. } => {
            discover_expr(plan, found, ctx, recv)?;
            discover_expr(plan, found, ctx, index)?;
        },
        Expr::MapApply { key, value, .. } => {
            discover_expr(plan, found, ctx, key)?;
            discover_expr(plan, found, ctx, value)?;
        },
        Expr::If {
            cond, then, else_, ..
        } => {
            discover_expr(plan, found, ctx, cond)?;
            discover_block(plan, found, ctx, then)?;
            if let Some(else_) = else_ {
                discover_expr(plan, found, ctx, else_)?;
            }
        },
        Expr::Match {
            scrutinee, arms, ..
        } => {
            discover_expr(plan, found, ctx, scrutinee)?;
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    discover_expr(plan, found, ctx, guard)?;
                }
                discover_expr(plan, found, ctx, &arm.body)?;
            }
        },
        Expr::While { cond, body, .. } => {
            discover_expr(plan, found, ctx, cond)?;
            discover_block(plan, found, ctx, body)?;
        },
        Expr::For {
            span, iter, body, comptime, ..
        } => {
            discover_expr(plan, found, ctx, iter)?;
            // The comptime expansion loop descends one copy per
            // projected field, the copy path keying every record read
            // inside — the same expansion the lowering runs, so a
            // copy-local site plans the body it enters from the copy's
            // own records. A per-instance loop at a body lowering over
            // its own variables walks once: its facts are a
            // per-instance record no plan carries there.
            if *comptime
                && let Some(copies) =
                    loop_copy_count(&found.analyzed.checked, found.comptime_instance.as_deref(), *span)
            {
                for index in 0..copies {
                    found
                        .comptime_path
                        .push(u32::try_from(index).expect("copy index fits u32"));
                    discover_expr(plan, found, ctx, body)?;
                    found.comptime_path.pop();
                }
                return Ok(());
            }
            discover_expr(plan, found, ctx, body)?;
        },
        Expr::Return { expr, .. } => {
            if let Some(expr) = expr {
                discover_expr(plan, found, ctx, expr)?;
            }
        },
        Expr::Nursery { body, .. } | Expr::Defer { body, .. } => {
            discover_block(plan, found, ctx, body)?;
        },
        Expr::Block(block) => discover_block(plan, found, ctx, block)?,
        Expr::Literal { .. }
        | Expr::RawStr { .. }
        | Expr::Path { .. }
        | Expr::FnType { .. }
        | Expr::Break(_)
        | Expr::Continue(_)
        // a conversion placeholder mints nothing
        | Expr::Invalid(_) => {},
    }
    Ok(())
}

/// One block's statements, in source order.
pub(super) fn discover_block(
    plan: &mut Plan,
    found: &mut Discovery<'_, '_>,
    ctx: &MintCtx,
    block: &BlockExpr,
) -> Result<(), LowerError> {
    for stmt in &block.stmts {
        discover_stmt(plan, found, ctx, stmt)?;
    }
    Ok(())
}

/// One statement: a binding's initializer or an expression.
pub(super) fn discover_stmt(
    plan: &mut Plan,
    found: &mut Discovery<'_, '_>,
    ctx: &MintCtx,
    stmt: &Stmt,
) -> Result<(), LowerError> {
    match &stmt.kind {
        StmtKind::Local(local) => discover_expr(plan, found, ctx, &local.init),
        // A block-local `fn` mints at its declaration's span inside the
        // entry holding the statement, the lambda-value site's own
        // discipline; its captures skip the declaration's own name (that
        // one is the fn's frame identity, never a capture of itself).
        StmtKind::Fn(f) => {
            if !f.generics.is_empty() {
                return Ok(()); // the statement's walk refuses the generic fn value; nothing inside it lowers
            }
            let Some(synth) = Expr::fn_decl_lambda(f) else {
                return Ok(()); // the grammar keeps a statement's body present
            };
            let evidence = mint_evidence(plan, found.analyzed, ctx, &synth);
            let mut captures = captures_under(found, ctx.subst.as_ref(), &synth, ctx.res)?;
            captures.retain(|(bind, _)| !found.analyzed.checked.self_fns.contains(bind));
            let index = plan.push_lambda(
                ctx.enclosing,
                &synth,
                Rc::new(captures),
                evidence,
                ctx.bounds.clone(),
                ctx.subst.clone(),
            );
            let nested = MintCtx {
                enclosing: index,
                enclosing_name: format!("{}${index}", ctx.enclosing_name),
                bounds: ctx.bounds.clone(),
                subst: ctx.subst.clone(),
                res: ctx.res,
            };
            // the descent enters the synthesized body directly: routing
            // it through the lambda arm would mint the record twice
            let Expr::Lambda { body, .. } = &synth else {
                unreachable!("the fn statement's synth is a lambda shape");
            };
            discover_expr(plan, found, &nested, body)?;
            Ok(())
        },
        StmtKind::Expr(expr) => discover_expr(plan, found, ctx, expr),
    }
}

pub(crate) fn runtime_pairs(
    analyzed: &Analyzed,
    res: &Resolutions,
    bounds: &HashMap<String, HashSet<TraitId>>,
    body: &Expr,
    demands: &Demands,
) -> Vec<RuntimePair> {
    let mut walk = PairWalk {
        analyzed,
        res,
        bounds,
        demands,
        seen: HashSet::new(),
        out: Vec::new(),
        comptime_path: Vec::new(),
    };
    walk.expr(body);
    walk.out
}

/// Whether an atom names a minted type variable anywhere in its shape —
/// any variable (`None`), or one of the given written base name: the
/// variable itself, or one nested in an application argument. A
/// trait-reference argument carrying one names a compound substitution
/// this seam resolves per variable alone; a branch head carrying the
/// named one keeps its per-branch condition on it.
pub(super) fn carries_a_var(
    universe: &ds_check::Universe,
    atom: TypeId,
    want: Option<&str>,
) -> bool {
    if let Some(name) = crate::shape::var_param_name(universe, atom) {
        return want.is_none_or(|want| name == want);
    }
    let Ok(decl) = universe.atom_decl(atom) else {
        return false;
    };
    match &decl.kind {
        AtomKind::Apply(_, args) => args.iter().any(|arg| {
            universe
                .members(*arg)
                .iter()
                .any(|&a| carries_a_var(universe, a, want))
        }),
        _ => false,
    }
}

/// The staged chain word for a demand whose variable the entering
/// branch's head never mentions: no chain object, a marker a nested
/// dispatch can read only under a narrowing that branch cannot satisfy,
/// trapping loudly there (a checker gap) rather than calling a wrong
/// body.
pub(crate) const DEAD_CHAIN: i64 = -1;

/// The generic parameter one single-atom receiver covers: the atom itself
/// when it is a minted variable, else the variable at the deepest
/// single-argument constructor chain inside it (`list[t]` and
/// `list[list[t]]` both demand on `t`). Union receivers and variable-free
/// atoms cover nothing — the demand walk and the nested-site lookup key
/// by this one name.
pub(super) fn peeled_param(universe: &ds_check::Universe, atom: TypeId) -> Option<String> {
    if let Some(param) = crate::shape::var_param_name(universe, atom) {
        return Some(param);
    }
    let decl = universe.atom_decl(atom).ok()?;
    let AtomKind::Apply(_, args) = &decl.kind else {
        return None;
    };
    let [only] = args.as_ref() else {
        return None;
    };
    let [member] = universe.members(*only) else {
        return None;
    };
    peeled_param(universe, *member)
}

/// The pair-collecting walk over one member body.
pub(super) struct PairWalk<'a> {
    pub(super) analyzed: &'a Analyzed,
    pub(super) res: &'a Resolutions,
    pub(super) bounds: &'a HashMap<String, HashSet<TraitId>>,
    pub(super) demands: &'a Demands,
    pub(super) seen: HashSet<(String, MethodId)>,
    pub(super) out: Vec<RuntimePair>,
    /// The active written `for comptime` copy path while the walk
    /// descends one copy's body: the demand reads route through the
    /// per-copy tables, or a copy-local bound receiver's fact is
    /// invisible and the frame never stages its evidence slot.
    pub(super) comptime_path: Vec<u32>,
}

impl PairWalk<'_> {
    /// The copy record at one span over the walk's active written-copy
    /// path (the demand pass runs at declaration and mint sites, never
    /// inside a per-instance expansion, so the instance tables take no
    /// part here).
    fn copy_record(&self, span: Span) -> Option<&ds_check::type_of::CopyRecords> {
        copy_record_at(&self.analyzed.checked, None, &self.comptime_path, span)
    }

    /// The checked receiver type at one span, the copy-routed read.
    fn recv_val(&self, span: Span) -> Option<Val> {
        self.copy_record(span)
            .and_then(|rec| rec.val)
            .or_else(|| self.analyzed.checked.expr_types.get(&span).copied())
    }

    /// One dot-call whose receiver covers a bounded type variable demands
    /// one evidence word: the member it calls, recorded once. The receiver
    /// is the variable itself (a nested runtime site) or a constructor
    /// over it (a transitive site whose serving body is itself a generic
    /// impl member — its rider words ride this frame's area for the same
    /// pair, so the walk collects the demand here too).
    fn call(&mut self, callee: &Expr, recv: &Expr) {
        let Expr::Field { name, .. } = callee else {
            return;
        };
        // A type-keyed member call carries no receiver value, so no frame
        // variable can demand an evidence word through it.
        if matches!(
            self.res.get(name.span),
            Some(Res::Dot(DotRes::Static { .. }))
        ) {
            return;
        }
        let Some(Res::Dot(DotRes::Methods { candidates })) = self.res.get(name.span) else {
            return;
        };
        let Some(val) = self.recv_val(recv.span()) else {
            return;
        };
        let [only] = self.analyzed.decls.universe.members(val.ty) else {
            return;
        };
        // the instance declaration and the body's checking mint their own
        // variable atoms; the written parameter name bridges them
        let Some(param) = peeled_param(&self.analyzed.decls.universe, *only) else {
            return;
        };
        let Some(traits) = self.bounds.get(&param) else {
            return;
        };
        for &method in candidates {
            let MethodOwner::Trait(def) = &self.analyzed.package.method(method).owner else {
                continue;
            };
            let Some(&trait_id) = self.analyzed.decls.traits.get(def) else {
                continue;
            };
            if traits.contains(&trait_id) && self.seen.insert((param.clone(), method)) {
                self.out.push(RuntimePair {
                    var: param.clone(),
                    method,
                });
            }
        }
    }

    /// One interpolation hole whose implicit `show` dispatch resolves
    /// through this frame's runtime evidence: the checker's
    /// hole record names the trait member and the operand's checked
    /// type peels the frame variable the dispatch's slot lookup reads —
    /// the same demand a written bound dot-call collects. A solved
    /// instance or an inherent owner records beside the hole record and
    /// stages its own words, so those demand nothing.
    fn hole_demand(&mut self, span: Span, recv: Span) {
        let Some(method) = self
            .copy_record(span)
            .and_then(|rec| rec.hole_show)
            .or_else(|| {
                self.analyzed
                    .checked
                    .hole_show_dispatches
                    .get(&span)
                    .copied()
            })
        else {
            return;
        };
        let routed = self
            .copy_record(span)
            .is_some_and(|rec| rec.trait_call.is_some() || rec.inherent_call.is_some())
            || self.analyzed.checked.trait_calls.contains_key(&span)
            || self.analyzed.checked.inherent_calls.contains_key(&span);
        if routed {
            return;
        }
        let Some(val) = self.recv_val(recv) else {
            return;
        };
        let [only] = self.analyzed.decls.universe.members(val.ty) else {
            return;
        };
        let Some(param) = peeled_param(&self.analyzed.decls.universe, *only) else {
            return;
        };
        let MethodOwner::Trait(def) = &self.analyzed.package.method(method).owner else {
            return;
        };
        let Some(&trait_id) = self.analyzed.decls.traits.get(def) else {
            return;
        };
        if self
            .bounds
            .get(&param)
            .is_some_and(|traits| traits.contains(&trait_id))
            && self.seen.insert((param.clone(), method))
        {
            self.out.push(RuntimePair { var: param, method });
        }
    }

    /// A call site entering a generic body — a free generic fn (named,
    /// or a UFCS dot-call feeding its first parameter) or a generic
    /// inherent impl's member — stages the callee's own runtime-evidence
    /// demands as the callee's trailing words. The checker's per-call
    /// record answers each demanded bound under this site's
    /// substitution, keyed by the callee's own variable name: a solved
    /// answer stages constants at the site, while a `None` answer (only
    /// an in-scope assumption covers it, so the substitution is this
    /// frame's own variable) reads this frame's evidence slots — the
    /// walk collects that demand here, keyed by the frame variable the
    /// staging's slot lookup peels. A trait dispatch site collects
    /// nothing here: the follow chain its entering site carries serves
    /// the transitive level.
    fn callee_demands(&mut self, callee: &Expr, call: Span, recv: Option<&Expr>) {
        let id = match callee {
            Expr::Field { name, .. } => match self.res.get(name.span) {
                Some(Res::Dot(DotRes::Free { func, .. })) => BodyId::Free(*func),
                // the checker-selected inherent member, when one won — the
                // type-keyed static call selects through the same record;
                // its receiver is no value, so a runtime-demand answer
                // (only a generic impl body has one) finds no variable and
                // contributes nothing, matching the checker's refusal
                Some(Res::Dot(DotRes::Methods { .. } | DotRes::Static { .. })) => {
                    let Some(method) = self
                        .copy_record(call)
                        .and_then(|rec| rec.inherent_call)
                        .or_else(|| self.analyzed.checked.inherent_calls.get(&call).copied())
                    else {
                        return;
                    };
                    let Some(&impl_site) = self.analyzed.decls.inherent_impls.get(&method) else {
                        return;
                    };
                    BodyId::Member {
                        impl_site,
                        member: self.analyzed.package.method(method).name.clone(),
                    }
                },
                _ => return,
            },
            Expr::Path { span, .. } => match self.res.get(*span) {
                Some(Res::Def(def)) => BodyId::Free(*def),
                _ => return,
            },
            _ => return,
        };
        let Some(pairs) = self.demands.get(&id) else {
            return;
        };
        for pair in pairs {
            let MethodOwner::Trait(def) = self.analyzed.package.method(pair.method).owner else {
                continue;
            };
            let Some(&trait_id) = self.analyzed.decls.traits.get(&def) else {
                continue;
            };
            let universe = &self.analyzed.decls.universe;
            // the runtime form's substituted type: the free fn's record
            // carries the solved type itself; the inherent record carries
            // none, and the receiver's checked type peels the same frame
            // variable the staging's slot lookup reads
            // The per-call bounds record, the copy-routed read: a
            // copy-local site's answers live in the per-copy tables.
            let free_record = || {
                self.copy_record(call)
                    .and_then(|rec| rec.generic_bounds.clone())
                    .or_else(|| {
                        self.analyzed
                            .checked
                            .generic_call_bounds
                            .get(&call)
                            .cloned()
                    })
            };
            let member_record = || {
                self.copy_record(call)
                    .and_then(|rec| rec.inherent_bounds.clone())
                    .or_else(|| {
                        self.analyzed
                            .checked
                            .inherent_call_bounds
                            .get(&call)
                            .cloned()
                    })
            };
            let solved = match &id {
                BodyId::Free(_) => {
                    let Some((_, _, served, solved)) = free_record().and_then(|record| {
                        record
                            .iter()
                            .find(|(var, bound, ..)| {
                                crate::shape::var_param_name(universe, *var).as_deref()
                                    == Some(pair.var.as_str())
                                    && *bound == trait_id
                            })
                            .copied()
                    }) else {
                        continue;
                    };
                    if served.is_some() {
                        continue;
                    }
                    solved
                },
                BodyId::Member { .. } => {
                    let Some((_, _, served)) = member_record().and_then(|record| {
                        record
                            .iter()
                            .find(|(var, bound, _)| {
                                crate::shape::var_param_name(universe, *var).as_deref()
                                    == Some(pair.var.as_str())
                                    && *bound == trait_id
                            })
                            .copied()
                    }) else {
                        continue;
                    };
                    if served.is_some() {
                        continue;
                    }
                    let Some(recv) = recv else {
                        continue;
                    };
                    let Some(val) = self.recv_val(recv.span()) else {
                        continue;
                    };
                    val.ty
                },
                // a dispatch target reaches its default body through the
                // trait path, never a name
                BodyId::Default { .. } => return,
            };
            let [only] = universe.members(solved) else {
                continue;
            };
            let Some(param) = peeled_param(universe, *only) else {
                continue;
            };
            // the frame's declared bounds carry the trait (the checker
            // satisfied the callee's bound through one), mirroring the
            // walk's own dot-call collection
            if !self
                .bounds
                .get(&param)
                .is_some_and(|traits| traits.contains(&trait_id))
            {
                continue;
            }
            if self.seen.insert((param.clone(), pair.method)) {
                self.out.push(RuntimePair {
                    var: param,
                    method: pair.method,
                });
            }
        }
    }

    /// One block's statements: a block-local `fn` body's demands stage
    /// into the closure block from this frame's slots at the
    /// construction, so this frame carries their slots transitively.
    fn block(&mut self, block: &BlockExpr) {
        for stmt in &block.stmts {
            match &stmt.kind {
                StmtKind::Local(local) => self.expr(&local.init),
                StmtKind::Fn(f) => {
                    if let Some(body) = &f.body {
                        self.block(body);
                    }
                },
                StmtKind::Expr(expr) => self.expr(expr),
            }
        }
    }

    fn expr(&mut self, e: &Expr) {
        match e {
            Expr::Literal { .. }
            | Expr::RawStr { .. }
            | Expr::Break(_)
            | Expr::Continue(_)
            | Expr::FnType { .. }
            | Expr::Path { .. }
            // a conversion placeholder demands no slot
            | Expr::Invalid(_) => {},
            // a spawned block's demands ride its entry argument area,
            // staged from this frame's evidence slots at the spawn site,
            // so this frame must carry a slot for every demand lexically
            // inside it, transitively
            Expr::Spawn { body, .. } => self.block(body),
            // a value-site lambda's body sites demand evidence too: the
            // construction stages the words from this frame's slots into
            // the capture block, so this frame must carry a slot for
            // every demand lexically inside the lambda, transitively
            Expr::Lambda { body, .. } => self.expr(body),
            Expr::Str { parts, .. } => {
                for part in parts {
                    match part {
                        StrPart::Text { .. } => {},
                        StrPart::Expr { span, expr } => {
                            self.hole_demand(*span, expr.span());
                            self.expr(expr);
                        },
                        StrPart::Ident { span, name } => self.hole_demand(*span, name.span),
                    }
                }
            },
            Expr::Paren { inner, .. }
            | Expr::Try { expr: inner, .. }
            | Expr::Unary { expr: inner, .. } => self.expr(inner),
            Expr::Block(inner) => self.block(inner),
            Expr::Struct { fields, .. } => {
                for (_, value) in fields {
                    if let Some(value) = value {
                        self.expr(value);
                    }
                }
            },
            Expr::Tuple { items, .. } | Expr::List { items, .. } => {
                for item in items {
                    self.expr(item);
                }
            },
            Expr::Call {
                span,
                callee,
                args,
                trailing,
                ..
            } => {
                if let Expr::Field { recv, .. } = &**callee {
                    self.call(callee, recv);
                    self.callee_demands(callee, *span, Some(recv));
                    self.expr(recv);
                } else {
                    self.callee_demands(callee, *span, None);
                    self.expr(callee);
                }
                for arg in args {
                    self.expr(&arg.value);
                }
                if let Some(block_arg) = trailing {
                    self.expr(block_arg);
                }
            },
            Expr::Field { recv, .. } => self.expr(recv),
            Expr::FieldKey { recv, name, .. } => {
                self.expr(recv);
                self.expr(name);
            },
            Expr::Index { recv, index, .. } => {
                self.expr(recv);
                self.expr(index);
            },
            Expr::MapApply { key, value, .. } => {
                self.expr(key);
                self.expr(value);
            },
            Expr::Is { expr, .. } => self.expr(expr),
            Expr::Binary { left, right, .. } | Expr::Assign { left, right, .. } => {
                self.expr(left);
                self.expr(right);
            },
            Expr::Range { start, end, .. } => {
                self.expr(start);
                self.expr(end);
            },
            Expr::If {
                cond, then, else_, ..
            } => {
                self.expr(cond);
                self.block(then);
                if let Some(else_expr) = else_ {
                    self.expr(else_expr);
                }
            },
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.expr(scrutinee);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.expr(guard);
                    }
                    self.expr(&arm.body);
                }
            },
            Expr::While { cond, body, .. } => {
                self.expr(cond);
                self.block(body);
            },
            Expr::For {
                span, iter, body, comptime, ..
            } => {
                self.expr(iter);
                // A written copy's bound-receiver site demands its
                // evidence word per copy: the walk descends each copy
                // so the reads route through the per-copy tables. A
                // per-instance loop (the once-check's marker, no
                // fields) walks once — its sites solve per concrete
                // instance, never a declared-body demand.
                if *comptime
                    && let Some(copies) = loop_copy_count(&self.analyzed.checked, None, *span)
                {
                    for index in 0..copies {
                        self.comptime_path
                            .push(u32::try_from(index).expect("copy index fits u32"));
                        self.expr(body);
                        self.comptime_path.pop();
                    }
                    return;
                }
                self.expr(body);
            },
            Expr::Return { expr, .. } => {
                if let Some(expr) = expr {
                    self.expr(expr);
                }
            },
            Expr::Nursery { body, .. } | Expr::Defer { body, .. } => {
                self.block(body);
            },
        }
    }
}
