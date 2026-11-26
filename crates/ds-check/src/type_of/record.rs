//! The checked-fact writers: every fact the lowering reads back (expression
//! types, binder facts, calls and bounds, place stores) is written here,
//! routed by `record_scope` to the body-wide table, a written `for comptime`
//! copy, or a per-instance expansion's copy.

use super::*;

impl Bodies<'_> {
    pub(super) fn record_scope(&self) -> RecordScope {
        if self.comptime_path.is_empty() {
            return RecordScope::Body;
        }
        let path = self.comptime_path.clone();
        match &self.instance {
            Some(run) => RecordScope::Instance(run.def, Rc::clone(&run.args), path),
            None => RecordScope::Written(path),
        }
    }

    pub(super) fn record_val(&mut self, span: Span, val: Val) {
        match self.record_scope() {
            RecordScope::Body => {
                self.expr_types.insert(span, val);
            },
            RecordScope::Written(path) => {
                self.comptime_records.entry((path, span)).or_default().val = Some(val);
            },
            RecordScope::Instance(def, args, path) => {
                self.comptime_instance_records
                    .entry((def, args, path, span))
                    .or_default()
                    .val = Some(val);
            },
        }
    }

    pub(super) fn record_binder(&mut self, span: Span, fact: crate::linearity::BinderFact) {
        match self.record_scope() {
            RecordScope::Body => {
                self.binders.insert(span, fact);
            },
            RecordScope::Written(path) => {
                self.comptime_records
                    .entry((path, span))
                    .or_default()
                    .binder = Some(fact);
            },
            RecordScope::Instance(def, args, path) => {
                self.comptime_instance_records
                    .entry((def, args, path, span))
                    .or_default()
                    .binder = Some(fact);
            },
        }
    }

    pub(super) fn record_generic_call(&mut self, def: DefId, span: Span, solved: Vec<Ty>) {
        // The site the expansion worklist reads: every solved call into
        // a fn whose body could hold a per-instance loop must be
        // reachable here, the walk mode never hiding it.
        self.comptime_sites.insert(span, (def, solved.clone()));
        match self.record_scope() {
            RecordScope::Body => {
                self.generic_calls.insert(span, solved);
            },
            RecordScope::Written(path) => {
                self.comptime_records
                    .entry((path, span))
                    .or_default()
                    .generic_call = Some(solved);
            },
            RecordScope::Instance(id, args, path) => {
                self.comptime_instance_records
                    .entry((id, args, path, span))
                    .or_default()
                    .generic_call = Some(solved);
            },
        }
    }

    pub(super) fn record_generic_bounds(&mut self, span: Span, bounds: GenericCallBounds) {
        match self.record_scope() {
            RecordScope::Body => {
                self.generic_call_bounds.insert(span, bounds);
            },
            RecordScope::Written(path) => {
                self.comptime_records
                    .entry((path, span))
                    .or_default()
                    .generic_bounds = Some(bounds);
            },
            RecordScope::Instance(id, args, path) => {
                self.comptime_instance_records
                    .entry((id, args, path, span))
                    .or_default()
                    .generic_bounds = Some(bounds);
            },
        }
    }

    pub(super) fn record_trait_call(&mut self, span: Span, call: TraitCall) {
        match self.record_scope() {
            RecordScope::Body => {
                self.trait_calls.insert(span, call);
            },
            RecordScope::Written(path) => {
                self.comptime_records
                    .entry((path, span))
                    .or_default()
                    .trait_call = Some(call);
            },
            RecordScope::Instance(id, args, path) => {
                self.comptime_instance_records
                    .entry((id, args, path, span))
                    .or_default()
                    .trait_call = Some(call);
            },
        }
    }

    pub(super) fn record_inherent_call(&mut self, span: Span, method: MethodId) {
        match self.record_scope() {
            RecordScope::Body => {
                self.inherent_calls.insert(span, method);
            },
            RecordScope::Written(path) => {
                self.comptime_records
                    .entry((path, span))
                    .or_default()
                    .inherent_call = Some(method);
            },
            RecordScope::Instance(id, args, path) => {
                self.comptime_instance_records
                    .entry((id, args, path, span))
                    .or_default()
                    .inherent_call = Some(method);
            },
        }
    }

    pub(super) fn record_inherent_bounds(&mut self, span: Span, bounds: InherentCallBounds) {
        match self.record_scope() {
            RecordScope::Body => {
                self.inherent_call_bounds.insert(span, bounds);
            },
            RecordScope::Written(path) => {
                self.comptime_records
                    .entry((path, span))
                    .or_default()
                    .inherent_bounds = Some(bounds);
            },
            RecordScope::Instance(id, args, path) => {
                self.comptime_instance_records
                    .entry((id, args, path, span))
                    .or_default()
                    .inherent_bounds = Some(bounds);
            },
        }
    }

    pub(super) fn record_is_test(&mut self, span: Span, ty: Ty) {
        match self.record_scope() {
            RecordScope::Body => {
                self.is_tests.insert(span, ty);
            },
            RecordScope::Written(path) => {
                self.comptime_records
                    .entry((path, span))
                    .or_default()
                    .is_test = Some(ty);
            },
            RecordScope::Instance(id, args, path) => {
                self.comptime_instance_records
                    .entry((id, args, path, span))
                    .or_default()
                    .is_test = Some(ty);
            },
        }
    }

    pub(super) fn record_directed_tag(&mut self, span: Span, atom: TypeId) {
        match self.record_scope() {
            RecordScope::Body => {
                self.directed_tags.insert(span, atom);
            },
            RecordScope::Written(path) => {
                self.comptime_records
                    .entry((path, span))
                    .or_default()
                    .directed_tag = Some(atom);
            },
            RecordScope::Instance(id, args, path) => {
                self.comptime_instance_records
                    .entry((id, args, path, span))
                    .or_default()
                    .directed_tag = Some(atom);
            },
        }
    }

    pub(super) fn record_handler_handled(&mut self, span: Span, ty: Ty) {
        match self.record_scope() {
            RecordScope::Body => {
                self.handler_handled.insert(span, ty);
            },
            RecordScope::Written(path) => {
                self.comptime_records
                    .entry((path, span))
                    .or_default()
                    .handler_handled = Some(ty);
            },
            RecordScope::Instance(id, args, path) => {
                self.comptime_instance_records
                    .entry((id, args, path, span))
                    .or_default()
                    .handler_handled = Some(ty);
            },
        }
    }

    pub(super) fn record_hole_show(&mut self, span: Span, method: MethodId) {
        match self.record_scope() {
            RecordScope::Body => {
                self.hole_show_dispatches.insert(span, method);
            },
            RecordScope::Written(path) => {
                self.comptime_records
                    .entry((path, span))
                    .or_default()
                    .hole_show = Some(method);
            },
            RecordScope::Instance(id, args, path) => {
                self.comptime_instance_records
                    .entry((id, args, path, span))
                    .or_default()
                    .hole_show = Some(method);
            },
        }
    }

    pub(super) fn record_member_params(&mut self, span: Span, params: Vec<Ty>) {
        match self.record_scope() {
            RecordScope::Body => {
                self.member_call_params.insert(span, params);
            },
            RecordScope::Written(path) => {
                self.comptime_records
                    .entry((path, span))
                    .or_default()
                    .member_params = Some(params);
            },
            RecordScope::Instance(id, args, path) => {
                self.comptime_instance_records
                    .entry((id, args, path, span))
                    .or_default()
                    .member_params = Some(params);
            },
        }
    }

    pub(super) fn record_place_store(&mut self, span: Span, store: PlaceStore) {
        match self.record_scope() {
            RecordScope::Body => {
                self.place_stores.insert(span, store);
            },
            RecordScope::Written(path) => {
                self.comptime_records
                    .entry((path, span))
                    .or_default()
                    .place_store = Some(store);
            },
            RecordScope::Instance(id, args, path) => {
                self.comptime_instance_records
                    .entry((id, args, path, span))
                    .or_default()
                    .place_store = Some(store);
            },
        }
    }

    // ---- expressions ----------------------------------------------------------
}
