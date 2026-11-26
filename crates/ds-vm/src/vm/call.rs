//! The call ABI: the staged-word facts a call site reads, closure staging,
//! the native invocation seam, and the overlay and bound-call checks.

use super::*;

/// One `Raw`-staged word a call site reads per call: the parameter
/// position (the mismatch trap's index), the staged slot's u8 address, and
/// the callee parameter's static type the word's runtime kind must match.
#[derive(Clone, Copy, Debug)]
pub(super) struct RawStagedWord {
    pub(super) index: u8,
    pub(super) slot: u8,
    pub(super) expected: SlotType,
}

/// A call site's layout-static facts, derived once per (caller, callee)
/// layout pair at cache fill and cached beside the callee: the validated
/// slide plan plus the `Raw`-staged words the site reads per call, or the
/// plan's layout-static refusal, which the call seam re-raises exactly
/// where the per-call derivation raised it (never answered as a hit-side
/// trap: the refusal rides the entry under the same key discipline, so a
/// refill after any install re-derives it).
#[derive(Clone, Debug)]
pub(super) enum SiteFacts {
    /// The plan validated; `raw` is `None` when the caller's area stages
    /// no `Raw` word (then no word is read at all). `frame_size` is the
    /// callee's, so the frame push reads no version table on a hit.
    Ready {
        plan: SlidePlan,
        raw: Option<Arc<[RawStagedWord]>>,
        frame_size: u32,
    },
    /// The layout pair refuses the slide.
    Refused(SlideError),
}

impl SiteFacts {
    /// Derives the facts from a caller/callee layout pair — a pure
    /// function of the pair, [`ds_abi::slide_plan`] the authority.
    // caller/callee is this contract's domain vocabulary, not a naming slip.
    #[allow(clippy::similar_names)]
    pub(super) fn derive(caller: &FrameLayout, callee: &FrameLayout) -> Self {
        match ds_abi::slide_plan(caller, callee) {
            Ok(plan) => Self::Ready {
                plan,
                raw: raw_staged_words(caller, callee),
                frame_size: callee.frame_size(),
            },
            Err(error) => Self::Refused(error),
        }
    }

    /// The refusal, when the pair refused the slide.
    pub(super) fn refused(&self) -> SlideError {
        match self {
            Self::Refused(error) => *error,
            Self::Ready { .. } => unreachable!("a ready site carries no refusal"),
        }
    }
}

/// The `Raw`-staged words of a proven plan the call site must read per
/// call: exactly the staged-`Raw` positions whose callee parameter is not
/// itself `Raw` (a `Raw` parameter is the wildcard — the word can never
/// fail, so the site never reads it). `None` when there is nothing to
/// read.
// caller/callee is this contract's domain vocabulary, not a naming slip.
#[allow(clippy::similar_names)]
pub(super) fn raw_staged_words(
    caller: &FrameLayout,
    callee: &FrameLayout,
) -> Option<Arc<[RawStagedWord]>> {
    if !caller.arg_area_has_raw() {
        return None;
    }
    let start = caller.arg_area().start;
    let words: Vec<RawStagedWord> = caller.arg_types()[..callee.params_len()]
        .iter()
        .zip(callee.params())
        .enumerate()
        .filter(|&(_, (&staged, &expected))| staged == SlotType::Raw && expected != SlotType::Raw)
        .map(|(index, (_, &expected))| RawStagedWord {
            index: u8::try_from(index).expect("parameter index fits u8"),
            slot: u8::try_from(start + index).expect("slot index fits u8"),
            expected,
        })
        .collect();
    (!words.is_empty()).then(|| words.into())
}

/// Reads a closure word's header and stages its capture block through the
/// shared buffer, routing to the word's own heap. The disjoint-field
/// parameters keep the staged slice's borrow on the staging buffer alone,
/// so the caller may push frames and route count mutations while it lives.
pub(super) fn stage_closure<'s>(
    objects: &mut Objects,
    pool: &mut [Fiber],
    owner: FiberId,
    staging: &'s mut CaptureStaging,
    word: RefId,
) -> Result<(FuncRef, &'s [Value]), Trap> {
    let store = if word.fiber() == owner {
        objects
    } else {
        pool.get_mut(word.fiber().index())
            .map(|fiber| &mut fiber.objects)
            .ok_or_else(|| Trap::UnknownObject { obj: word.id() })?
    };
    let (header, block) = store.closure_at(word.id())?;
    Ok((header, staging.stage(block)))
}

/// Runs a native in the caller's frame: reads the staged overlay words
/// as arguments (type-checked against the native's parameter list, the
/// same discipline a uop callee's layout gets) and invokes the body. The
/// CALL site then places the result on the caller's overlay words
/// ([`overlay_native`]); the CALLT site returns it from the tail
/// position. No frame is pushed and no metadata word moves. A free
/// function over the disjoint state the dispatcher hands it (stack, heap,
/// sink), so the registry borrow the callee came from stays alive.
pub(super) fn invoke_native(
    nf: &NativeFunction,
    staged: usize,
    stack: &mut Segment,
    heaps: &mut Heaps,
    out: &mut OutputSink,
    caller: &FrameLayout,
    fp: usize,
) -> Result<NativeResult, Trap> {
    let body_len = caller.body_len();
    if nf.params.len() > caller.arg_area_len() || staged < nf.params.len() {
        return Err(Trap::CallLayout(SlideError::StagingTooSmall {
            available: caller.arg_area_len(),
            needed: nf.params.len(),
        }));
    }
    let start = caller.arg_area().start;
    // the argument list stages inline — every native table entry sits well
    // under the inline width — with the heap buffer the oversized fallback;
    // a variadic native reads the whole staged span (its `params` cover the
    // named prefix), every other native exactly its parameter count
    let words = if nf.variadic { staged } else { nf.params.len() };
    let mut inline = [Value::I64(0); INLINE_WORDS];
    let mut heap = Vec::new();
    let args: &[Value] = if words <= INLINE_WORDS {
        for (i, word) in inline.iter_mut().enumerate().take(words) {
            let slot = u8::try_from(start + i).expect("slot index fits u8");
            *word = read_value(stack, fp, slot, caller.slots(), body_len)?;
        }
        &inline[..words]
    } else {
        heap.reserve(words);
        for i in 0..words {
            let slot = u8::try_from(start + i).expect("slot index fits u8");
            heap.push(read_value(stack, fp, slot, caller.slots(), body_len)?);
        }
        &heap
    };
    for (index, (&value, &want)) in args.iter().zip(nf.params).enumerate() {
        // a `Raw` parameter is the wildcard: the native's argument kind
        // varies by call site (list_push's element), so the body receives
        // whatever word staged
        let kind = value.slot_type();
        if kind != want && want != SlotType::Raw {
            return Err(Trap::CallLayout(SlideError::TypeMismatch {
                index,
                staged: kind,
                expected: want,
            }));
        }
    }
    (nf.func)(heaps, out, args)
}

/// The generic dispatch's staged-span contract shared by the CALLG and
/// CALLTG paths: the two span forms take at least their named words plus
/// one span word (`cons`'s head span riding the front, a fold's
/// accumulator span between the leading and trailing named words), a
/// fixed entry stages exactly its named count, and the evidence words
/// need the overlay past the whole staged span either way. Answers the
/// needed width.
pub(super) fn generic_staged_check(
    g: &crate::native::GenericFunction,
    nargs: u8,
) -> Result<usize, Trap> {
    let given = usize::from(nargs);
    let arity_ok = match g.staging {
        Staging::Fixed => given == g.params.len(),
        Staging::Leading => given >= g.params.len(),
        Staging::Interior => given > g.params.len(),
    };
    if !arity_ok {
        return Err(Trap::ArityMismatch {
            expected: g.params.len(),
            given,
        });
    }
    Ok(given + usize::from(g.evidence))
}

/// The generic dispatch's kind check over the named words: a `Raw`
/// parameter is the wildcard (the staged value words carry the call
/// site's concrete kinds), and a `Ref` word against a `Fn` parameter is
/// the callable-defaulted slot — the combinators' staged lambda
/// may be a closure object. The span forms' dynamic words are the call
/// site's own fact and stay unchecked: a `Leading` entry checks its
/// trailing named words, an `Interior` one its leading named words plus
/// the final named word at the staged span's end. The mismatch's index
/// names the offending word's position among the named words.
pub(super) fn generic_kind_check(
    g: &crate::native::GenericFunction,
    args: &[Value],
) -> Result<(), Trap> {
    let check = |index: usize, value: &Value, want: SlotType| -> Result<(), Trap> {
        let staged = value.slot_type();
        if staged != want
            && want != SlotType::Raw
            && !(staged == SlotType::Ref && want == SlotType::Fn)
        {
            return Err(Trap::CallLayout(SlideError::TypeMismatch {
                index,
                staged,
                expected: want,
            }));
        }
        Ok(())
    };
    match g.staging {
        Staging::Fixed => args
            .iter()
            .zip(g.params)
            .enumerate()
            .try_for_each(|(index, (&value, &want))| check(index, &value, want)),
        Staging::Leading => {
            let named = g.params.len();
            args[args.len() - named..]
                .iter()
                .zip(g.params)
                .enumerate()
                .try_for_each(|(index, (&value, &want))| check(index, &value, want))
        },
        Staging::Interior => {
            let lead = g.params.len() - 1;
            (0..lead).try_for_each(|index| check(index, &args[index], g.params[index]))?;
            check(
                lead,
                args.last().expect("the arity check ran"),
                g.params[lead],
            )
        },
    }
}

/// Places a native's result on the caller's outgoing-argument overlay:
/// one word on argument slot 0, a fallible native's two-word
/// tag+payload span on argument slots 0 and 1, or a span
/// channel's discriminant plus multi-word payload on slots
/// `0 ..= payload.len()`. Like a RET's write, this bypasses slot typing:
/// the overlay words belong to the call ABI, and reading them as typed
/// slots is the lowering's discipline (mixed shapes declare the slots
/// `Raw`). The fixed widths are pre-checked at the call site; a span
/// channel answer's width is runtime data, so its write bounds-checks
/// against the overlay area here (the lowering sizes the area, RETSPAN's
/// discipline — a violation names hand-assembled code).
pub(super) fn overlay_native(
    result: NativeResult,
    stack: &mut Segment,
    caller: &FrameLayout,
    fp: usize,
) -> Result<(), Trap> {
    let body_len = caller.body_len();
    let start = caller.arg_area().start;
    match result {
        NativeResult::Word(value) => {
            stack[word_of(fp, start, body_len)] = Word::Slot(value);
        },
        // a park request never overlays: the site parked, the wake
        // delivering the completed answer there instead
        NativeResult::Park(_) => unreachable!("a world park never overlays"),
        NativeResult::Tag { tag, payload } => {
            debug_assert!(caller.arg_area_len() >= 2, "width pre-checked");
            stack[word_of(fp, start, body_len)] = Word::Slot(Value::Tag(tag));
            stack[word_of(fp, start + 1, body_len)] = Word::Slot(payload);
        },
        NativeResult::Span { tag, payload } => {
            let span = 1 + payload.len();
            if span > caller.arg_area_len() {
                return Err(Trap::RetSpanTooWide {
                    span: u16::try_from(span).expect("span widths fit u16"),
                    caller_area: u16::try_from(caller.arg_area_len())
                        .expect("argument areas fit u16"),
                });
            }
            stack[word_of(fp, start, body_len)] = Word::Slot(Value::Tag(tag));
            for (index, value) in payload.into_iter().enumerate() {
                let slot = start + 1 + index;
                stack[word_of(fp, slot, body_len)] = Word::Slot(value);
            }
        },
        // the tag-less total rides the same bounds-checked write with no
        // discriminant: the words are the answer, slots `0 .. n`
        NativeResult::Total { words } => {
            if words.len() > caller.arg_area_len() {
                return Err(Trap::RetSpanTooWide {
                    span: u16::try_from(words.len()).expect("span widths fit u16"),
                    caller_area: u16::try_from(caller.arg_area_len())
                        .expect("argument areas fit u16"),
                });
            }
            for (index, value) in words.into_iter().enumerate() {
                let slot = start + index;
                stack[word_of(fp, slot, body_len)] = Word::Slot(value);
            }
        },
        // a park request never reaches an overlay: the CALLG path parks
        // (or refuses and releases) before it, and the CALLTG site
        // refuses the fork before its spawn phase runs
        NativeResult::Fork { .. } => {
            unreachable!("the dispatcher parks or refuses a fork request first")
        },
    }
    Ok(())
}

/// Reads the i64 the caller staged on overlay word `index` for a control
/// native — the same staged-type discipline `invoke_native` applies to
/// ordinary natives.
pub(super) fn control_i64(
    stack: &Segment,
    fp: usize,
    caller: &FrameLayout,
    index: usize,
) -> Result<i64, Trap> {
    let body_len = caller.body_len();
    let slot = u8::try_from(caller.arg_area().start + index).expect("slot index fits u8");
    match read_value(stack, fp, slot, caller.slots(), body_len)? {
        Value::I64(v) => Ok(v),
        v => Err(Trap::CallLayout(SlideError::TypeMismatch {
            index,
            staged: v.slot_type(),
            expected: SlotType::I64,
        })),
    }
}

/// Reads one staged word of any kind on overlay word `index` for a
/// control native (the callable and cancel seams carry words whose kinds
/// the call site proves, not the table).
pub(super) fn control_word(
    stack: &Segment,
    fp: usize,
    caller: &FrameLayout,
    index: usize,
) -> Result<Value, Trap> {
    let body_len = caller.body_len();
    let slot = u8::try_from(caller.arg_area().start + index).expect("slot index fits u8");
    read_value(stack, fp, slot, caller.slots(), body_len)
}

/// The control paths' infallible one-word overlay: a `Word` write has no
/// width check to fail (only the span channel's dynamic width can), so
/// the switch and delivery sites keep the unchecked shape.
pub(super) fn overlay_word(value: Value, stack: &mut Segment, caller: &FrameLayout, fp: usize) {
    let body_len = caller.body_len();
    let start = caller.arg_area().start;
    stack[word_of(fp, start, body_len)] = Word::Slot(value);
}

/// The boundary seam's infallible tag-plus-flat-span overlay: the area
/// is checked against `1 + payload.len()` by the caller
/// ([`overlay_boundary`]), so like [`overlay_word`] the write cannot
/// fail.
pub(super) fn overlay_tag(
    tag: u8,
    payload: &[Value],
    stack: &mut Segment,
    caller: &FrameLayout,
    fp: usize,
) {
    debug_assert!(
        caller.arg_area_len() >= 2,
        "the static floor is pre-checked"
    );
    let body_len = caller.body_len();
    let start = caller.arg_area().start;
    stack[word_of(fp, start, body_len)] = Word::Slot(Value::Tag(tag));
    for (index, value) in payload.iter().enumerate() {
        let slot = start + 1 + index;
        debug_assert!(
            slot < caller.arg_area().start + caller.arg_area_len(),
            "width pre-checked"
        );
        stack[word_of(fp, slot, body_len)] = Word::Slot(*value);
    }
}

/// The pre-invocation width check for a native call: the callee's result
/// must fit the overlay area that receives it. For a CALL that is the
/// caller's staging area; for a CALLT the span lands on the ORIGINAL
/// caller's overlay words (the tail frame's slots alias them), so the
/// frame below supplies the area. Checked before the native runs — a
/// world-effect side effect must not happen only for the call to trap
/// on the result width. The checked width is the static `result_words`
/// floor; a span channel answer's extra words bounds-check at the
/// overlay write.
pub(super) fn check_native_overlay(area: usize, nf: &NativeFunction) -> Result<(), Trap> {
    if usize::from(nf.result_words) > area {
        return Err(Trap::RetSpanTooWide {
            span: u16::from(nf.result_words),
            caller_area: u16::try_from(area).expect("argument areas fit u16"),
        });
    }
    Ok(())
}

/// The pre-invocation checks for a native CALL, in trap order: the staged
/// arguments must fit the overlay area (the same StagingTooSmall a uop
/// callee's layout yields), then the result width must fit it too.
pub(super) fn check_native_call_site(
    caller: &FrameLayout,
    nf: &NativeFunction,
) -> Result<(), Trap> {
    let area = caller.arg_area_len();
    if nf.params.len() > area {
        return Err(Trap::CallLayout(SlideError::StagingTooSmall {
            available: area,
            needed: nf.params.len(),
        }));
    }
    check_native_overlay(area, nf)
}

/// The pre-invocation width check for a bound extern's CALL: the
/// staged words and the one overlay word the scalar answer takes must
/// fit the caller's staging area — the same discipline
/// [`check_native_call_site`] applies, checked before the foreign call
/// runs so a world-effect side effect never happens only for the trap.
pub(super) fn check_bound_call_site(caller: &FrameLayout, staged: usize) -> Result<(), Trap> {
    let area = caller.arg_area_len();
    if staged.max(1) > area {
        return Err(Trap::CallLayout(SlideError::StagingTooSmall {
            available: area,
            needed: staged.max(1),
        }));
    }
    Ok(())
}

/// The pre-invocation width check for a bound extern's CALLT: the one
/// overlay word lands on the ORIGINAL caller's area (the tail frame's
/// slot 0 aliases it), so the frame below supplies the area — the same
/// rule [`check_native_overlay`] applies to a native's `result_words`.
pub(super) fn check_bound_overlay(area: usize) -> Result<(), Trap> {
    if area < 1 {
        return Err(Trap::RetSpanTooWide {
            span: 1,
            caller_area: u16::try_from(area).expect("argument areas fit u16"),
        });
    }
    Ok(())
}

/// Runs one bound extern's C-ABI call in the caller's frame: the
/// staged words read exactly like a native's, each marshaling per its
/// own kind — a scalar word singly, a string or bytes word as its
/// call-scoped (pointer, length) pair over the borrowed content, a heap
/// word routed to its owning store and an arena handle read from the
/// running fiber's arena (the lowering proved the written signature
/// inside the marshaling slice ahead of the run) — and the foreign
/// answer boxes by the pool constant's return word, a unit return
/// answering the canonical unit word. A blocking call pins the calling
/// fiber for its whole duration — no preemption point exists inside the
/// C call — and a foreign abort kills the process (panic=abort, the VM
/// never unwinding through a foreign frame).
#[allow(clippy::too_many_arguments)]
pub(super) fn invoke_bound(
    pool: &[Fiber],
    objects: &Objects,
    ctx_live: bool,
    current: FiberId,
    module: &crate::bound::BoundModule,
    index: u32,
    ret: BoundRet,
    staged: usize,
    stack: &mut Segment,
    caller: &FrameLayout,
    fp: usize,
) -> Result<Value, Trap> {
    let body_len = caller.body_len();
    check_bound_call_site(caller, staged)?;
    let start = caller.arg_area().start;
    let mut inline = [Value::I64(0); INLINE_WORDS];
    let mut heap = Vec::new();
    let args: &[Value] = if staged <= INLINE_WORDS {
        for (i, word) in inline.iter_mut().enumerate().take(staged) {
            let slot = u8::try_from(start + i).expect("slot index fits u8");
            *word = read_value(stack, fp, slot, caller.slots(), body_len)?;
        }
        &inline[..staged]
    } else {
        heap.reserve(staged);
        for i in 0..staged {
            let slot = u8::try_from(start + i).expect("slot index fits u8");
            heap.push(read_value(stack, fp, slot, caller.slots(), body_len)?);
        }
        &heap
    };
    let outside = |word: &'static str| Trap::BoundMarshaling {
        symbol: module.name(index).to_owned(),
        reason: word,
    };
    let mut params = Vec::with_capacity(args.len());
    let mut marshaled = Vec::with_capacity(args.len());
    for &value in args {
        match value {
            Value::I64(word) => {
                params.push(ds_ffi::Param::Int);
                marshaled.push(ds_ffi::Arg::Int(word));
            },
            Value::Bool(flag) => {
                params.push(ds_ffi::Param::Bool);
                marshaled.push(ds_ffi::Arg::Bool(flag));
            },
            Value::F64(number) => {
                params.push(ds_ffi::Param::Float);
                marshaled.push(ds_ffi::Arg::Float(number));
            },
            // a string or bytes word marshals as its call-scoped
            // (pointer, length) pair: a heap word's own payload decides
            // the encoding (the store holds exactly one of the two
            // kinds), an arena handle is a concat temporary over the
            // running fiber's arena
            Value::Ref(id) => {
                let store = if ctx_live && id.fiber() == current {
                    objects
                } else {
                    &pool[id.fiber().index()].objects
                };
                if let Ok(text) = store.str_at(id.id()) {
                    params.push(ds_ffi::Param::Str);
                    marshaled.push(ds_ffi::Arg::Str(text));
                } else if let Ok(data) = store.bytes_at(id.id()) {
                    params.push(ds_ffi::Param::Bytes);
                    marshaled.push(ds_ffi::Arg::Bytes(data));
                } else {
                    return Err(outside(
                        "a staged word outside the int, bool, float, string, and bytes slice",
                    ));
                }
            },
            Value::Arena(_) => {
                let bytes = str_content(pool, objects, ctx_live, current, value)
                    .map_err(|_| outside("a staged arena word outside the running fiber"))?;
                let text = core::str::from_utf8(bytes)
                    .map_err(|_| outside("a staged arena word outside UTF-8 text"))?;
                params.push(ds_ffi::Param::Str);
                marshaled.push(ds_ffi::Arg::Str(text));
            },
            _ => {
                return Err(outside(
                    "a staged word outside the int, bool, float, string, and bytes slice",
                ));
            },
        }
    }
    let symbol = module.symbol(index).ok_or_else(|| Trap::BoundMarshaling {
        symbol: module.name(index).to_owned(),
        reason: "an export index past the artifact's enumeration",
    })?;
    let sig = ds_ffi::Sig {
        params,
        ret: crate::bound::marshal_ret(ret),
    };
    let answer = symbol
        .call(&sig, &marshaled)
        .map_err(|_| Trap::BoundMarshaling {
            symbol: module.name(index).to_owned(),
            reason: "the enumerated transmute serves at most the marshaling arity cap",
        })?;
    Ok(crate::bound::answer_value(answer, ret))
}
