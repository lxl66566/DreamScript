//! Finalization: the op stream's labels, spans, loop pairings, and pool
//! rebasing into the emitted function record, the callee FuncRef
//! constants, and the zero loads of dead payload words.

use super::*;

impl<'a, 'p, 's> FnLower<'a, 'p, 's> {
    /// Renders the symbolic buffer: the placed sequence (uops plus the RC
    /// ops at their walk positions) and the module body carry the same
    /// words — the RC ops ride the 0xE major beside the uops, and the VM
    /// dispatches both. FUNCF occupies word 0 and every op occupies one
    /// word, so a label's stream position is its op index plus one; jump
    /// targets cannot shift.
    #[allow(clippy::too_many_lines)]
    pub(super) fn finalize(mut self) -> Result<crate::FinalizedFn, LowerError> {
        // The canonical unit word's one `Kimm 0` enters at the frame top:
        // a use inside a branch arm must not own the only write (a path
        // through another arm would read the previous frame's garbage),
        // and a loop head bound at stream position 0 must not pick the
        // store up per back edge — so every label and loop span shifts
        // past it, keeping each jump target on its own op.
        if let Some(word) = self.unit_slot {
            let kidx = self.pool(Constant::I64(0));
            self.code.insert(0, Op::Kimm {
                dst: Slot::body(word),
                kidx,
            });
            for pos in &mut self.labels {
                if *pos != usize::MAX {
                    *pos += 1;
                }
            }
            for span in &mut self.loop_spans {
                span.head += 1;
                if span.back != usize::MAX {
                    span.back += 1;
                }
            }
            for end in &mut self.value_ends {
                *end += 1;
            }
            for (pos, _) in &mut self.span_marks {
                *pos += 1;
            }
        }
        // Stage three's temporary slices: every slot the placement pass
        // still classifies count-free takes its count-free spelling — a
        // KSTR load becomes a borrow load (KSTR -> KSTR_BORROW), a
        // STRCAT result becomes an arena serving (STRCAT ->
        // STRCAT_ARENA) — and its terminal decs leave the stream. Slots
        // bump-allocate, so each dec belongs to a word the pass hosted
        // there, and the classification survived the whole walk, so no
        // path moves a deleted count. A loop body carrying a designated
        // arena word gains a mark/restore pair at its head and back
        // edge, one fresh body word per marked loop (appended after the
        // walk, so only qualifying frames grow). Deleting and inserting
        // words shift the stream, so bound labels rebase against both
        // movements: a label bound at a removed dec lands on the op
        // that followed it, and a loop's head label stays on the
        // inserted mark (a jump lands on the re-mark).
        let borrow = self.rc.take_kstr_slots();
        let mut arena = self.rc.take_strcat_slots();
        let mut insertions: Vec<(usize, Op)> = Vec::new();
        if !arena.is_empty() {
            // a marked loop's body holds a designated concatenation
            // somewhere between its head and back edge
            let designated = |index: usize, arena: &HashSet<u8>| match self.code[index] {
                Op::Data {
                    op: DataOp::StrCat,
                    dst:
                        Slot {
                            index: word,
                            overlay: false,
                        },
                    ..
                } => arena.contains(&word),
                _ => false,
            };
            let marked: Vec<usize> = self
                .loop_spans
                .iter()
                .enumerate()
                .filter(|(_, span)| (span.head..span.back).any(|index| designated(index, &arena)))
                .map(|(span_index, _)| span_index)
                .collect();
            // the mark words must fit beside the body and overlay words;
            // a frame that cannot host them keeps the counted spelling
            if u8::try_from(self.slots.len() + self.overlay_len + marked.len()).is_ok() {
                for &span_index in &marked {
                    let word = u8::try_from(self.slots.len()).expect("mark word fits u8");
                    self.slots.push(SlotType::I64);
                    let span = self.loop_spans[span_index];
                    insertions.push((span.head, Op::ArenaMark {
                        dst: Slot::body(word),
                    }));
                    insertions.push((span.back, Op::ArenaRestore {
                        src: Slot::body(word),
                    }));
                }
            } else {
                arena.clear();
            }
        }
        if !borrow.is_empty() || !arena.is_empty() {
            let mut dead = Vec::new();
            for index in 0..self.code.len() {
                match self.code[index] {
                    Op::Kstr {
                        dst:
                            Slot {
                                index: word,
                                overlay: false,
                            },
                        kidx,
                    } if borrow.contains(&word) => {
                        self.code[index] = Op::KstrBorrow {
                            dst: Slot::body(word),
                            kidx,
                        };
                    },
                    Op::Data {
                        op: DataOp::StrCat,
                        dst:
                            Slot {
                                index: word,
                                overlay: false,
                            },
                        lhs,
                        rhs,
                    } if arena.contains(&word) => {
                        self.code[index] = Op::Data {
                            op: DataOp::StrCatArena,
                            dst: Slot::body(word),
                            lhs,
                            rhs,
                        };
                    },
                    Op::RcDec {
                        src:
                            Slot {
                                index: word,
                                overlay: false,
                            },
                    } if borrow.contains(&word) || arena.contains(&word) => {
                        dead.push(index);
                    },
                    _ => {},
                }
            }
            // `dead` rises with the forward walk and `insertions` sorts
            // into code order, so one forward pass splices the survivors
            // and the mark ops and a second answers every label's shift
            insertions.sort_by_key(|&(at, _)| at);
            let mut rebuilt = Vec::with_capacity(self.code.len() + insertions.len() - dead.len());
            let mut at = 0;
            let mut dead_at = 0;
            for (index, op) in self.code.iter().enumerate() {
                while at < insertions.len() && insertions[at].0 == index {
                    rebuilt.push(insertions[at].1);
                    at += 1;
                }
                if dead_at < dead.len() && dead[dead_at] == index {
                    dead_at += 1;
                    continue;
                }
                rebuilt.push(*op);
            }
            self.code = rebuilt;
            let mut moved: Vec<usize> = (0..self.labels.len())
                .filter(|&label| self.labels[label] != usize::MAX)
                .collect();
            moved.sort_unstable_by_key(|&label| self.labels[label]);
            let (mut removed, mut added) = (0usize, 0usize);
            for label in moved {
                // both movements count against the original position
                // (a deletion before the label removes one op, an
                // insertion before it adds one; the insertions cannot
                // underflow because each removal sits strictly below)
                let base = self.labels[label];
                while removed < dead.len() && dead[removed] < base {
                    removed += 1;
                }
                while added < insertions.len() && insertions[added].0 < base {
                    added += 1;
                }
                self.labels[label] = base - removed + added;
            }
            // the value ends ride the same movements (the seam's resume
            // targets stay on the ops they named)
            for end in &mut self.value_ends {
                let base = usize::try_from(*end).expect("code length fits usize");
                let (mut removed, mut added) = (0usize, 0usize);
                while removed < dead.len() && dead[removed] < base {
                    removed += 1;
                }
                while added < insertions.len() && insertions[added].0 < base {
                    added += 1;
                }
                *end = u32::try_from(base - removed + added).expect("code length fits u32");
            }
            // the span marks ride the same movements, so each region
            // start stays on the op it named
            for (pos, _) in &mut self.span_marks {
                let base = *pos;
                let (mut removed, mut added) = (0usize, 0usize);
                while removed < dead.len() && dead[removed] < base {
                    removed += 1;
                }
                while added < insertions.len() && insertions[added].0 < base {
                    added += 1;
                }
                *pos = base - removed + added;
            }
        }
        let body_len = self.slots.len();
        // one word type per overlay slot, or Raw when shapes mix there
        let mut overlay_words = Vec::with_capacity(self.overlay_len);
        for index in 0..self.overlay_len {
            let slot = self
                .overlay
                .get(index)
                .filter(|set| set.len() == 1)
                .and_then(|set| set.iter().next().copied())
                .unwrap_or(SlotType::Raw);
            overlay_words.push(slot);
        }
        if body_len + overlay_words.len() > usize::from(MAX_FRAME_SLOTS) {
            return Err(LowerError::FrameTooWide {
                name: self.name.clone(),
                need: body_len + overlay_words.len(),
            });
        }
        let total = body_len + overlay_words.len();
        let mut slots = self.slots.clone();
        slots.extend(overlay_words);
        let layout =
            ds_abi::FrameLayout::new(slots, self.params_len, self.overlay_len).map_err(|_| {
                LowerError::FrameTooWide {
                    name: self.name.clone(),
                    need: total,
                }
            })?;
        let len = self.code.len();
        let mut placed = Vec::with_capacity(len + 1);
        let mut code = Vec::with_capacity(len + 1);
        let funcf = Uop::Funcf {
            frame_slots: u16::try_from(total).expect("frame slots fit u16"),
        };
        placed.push(PlacedOp::Uop(funcf));
        code.push(ds_abi::Inst::Uop(funcf));
        for op in &self.code {
            let body = |slot: Slot| resolve(slot, body_len);
            // FUNCF occupies stream word 0, so op i sits at word i + 1
            let target =
                |label: usize| u32::try_from(self.labels[label] + 1).expect("jump target fits u32");
            let rendered = match *op {
                Op::Kimm { dst, kidx } => PlacedOp::Uop(Uop::Kimm {
                    dst: body(dst),
                    kidx: ConstIdx(kidx),
                }),
                Op::Kconst { dst, kidx } => PlacedOp::Uop(Uop::Kconst {
                    dst: body(dst),
                    kidx: ConstIdx(kidx),
                }),
                Op::Kstr { dst, kidx } => PlacedOp::Uop(Uop::Kstr {
                    dst: body(dst),
                    kidx: ConstIdx(kidx),
                }),
                Op::KstrBorrow { dst, kidx } => PlacedOp::Uop(Uop::KstrBorrow {
                    dst: body(dst),
                    kidx: ConstIdx(kidx),
                }),
                Op::Data { op, dst, lhs, rhs } => {
                    PlacedOp::Uop(op.uop(body(dst), body(lhs), body(rhs)))
                },
                Op::TagTest { dst, src, tag } => PlacedOp::Uop(Uop::TagTest {
                    dst: body(dst),
                    src: body(src),
                    tag,
                }),
                Op::TagNew { dst, tag } => PlacedOp::Uop(Uop::TagNew {
                    dst: body(dst),
                    tag,
                }),
                Op::Mov { dst, src } => PlacedOp::Uop(Uop::Mov {
                    dst: body(dst),
                    src: body(src),
                }),
                Op::RcInc { src } => PlacedOp::Rc(RcOp::Inc { src: body(src) }),
                Op::RcDec { src } => PlacedOp::Rc(RcOp::Dec { src: body(src) }),
                Op::Jmp { target: at } => PlacedOp::Uop(Uop::Jmp { target: target(at) }),
                Op::ArenaMark { dst } => PlacedOp::Uop(Uop::ArenaMark { dst: body(dst) }),
                Op::ArenaRestore { src } => PlacedOp::Uop(Uop::ArenaRestore { src: body(src) }),
                Op::Jmpif { cond, target: at } => PlacedOp::Uop(Uop::Jmpif {
                    cond: body(cond),
                    target: target(at),
                }),
                Op::JmpifNot { cond, target: at } => PlacedOp::Uop(Uop::JmpifNot {
                    cond: body(cond),
                    target: target(at),
                }),
                Op::Call { callee, nargs } => PlacedOp::Uop(Uop::Call {
                    callee: ConstIdx(callee),
                    nargs,
                }),
                Op::Callt { callee, nargs } => PlacedOp::Uop(Uop::Callt {
                    callee: ConstIdx(callee),
                    nargs,
                }),
                Op::Callg { callee, nargs } => PlacedOp::Uop(Uop::Callg {
                    callee: ConstIdx(callee),
                    nargs,
                }),
                Op::Calltg { callee, nargs } => PlacedOp::Uop(Uop::Calltg {
                    callee: ConstIdx(callee),
                    nargs,
                }),
                Op::Calls { callee_slot, nargs } => {
                    PlacedOp::Uop(Uop::Calls { callee_slot, nargs })
                },
                Op::ClosureNew { dst, func, n } => PlacedOp::Uop(Uop::ClosureNew { dst, func, n }),
                Op::CaptureStore { src, obj, idx } => {
                    PlacedOp::Uop(Uop::CaptureStore { src, obj, idx })
                },
                Op::Ret { src } => PlacedOp::Uop(Uop::Ret { src: body(src) }),
                Op::RetSpan { src, n } => PlacedOp::Uop(Uop::RetSpan { src: body(src), n }),
                Op::Halt { slot } => PlacedOp::Uop(Uop::Halt { slot: body(slot) }),
                Op::ErrMark => PlacedOp::Uop(Uop::ErrMark),
            };
            code.push(match rendered {
                PlacedOp::Uop(uop) => ds_abi::Inst::Uop(uop),
                PlacedOp::Rc(rc) => ds_abi::Inst::Rc(rc),
            });
            placed.push(rendered);
        }
        let function = ds_mod::Function::new(self.name.clone(), layout, self.pool, code)
            .map_err(|error| LowerError::Build(error.to_string()))?;
        // FUNCF occupies stream word 0, so every recorded value end sits
        // one word above its pre-finalize index; the leading zero is the
        // body's start — the target that runs everything, standing for
        // the value end of the zeroth statement
        let facts = crate::FnFacts {
            value_ends: std::iter::once(0)
                .chain(self.value_ends.iter().map(|end| end + 1))
                .collect(),
            registered: self.rc.take_registered(),
        };
        // FUNCF occupies stream word 0, so every span mark's final pc is
        // one word above its op position; regions merge nothing here —
        // the module's table build merges adjacent equal locations.
        let marks = self
            .span_marks
            .into_iter()
            .map(|(pos, span)| (u32::try_from(pos + 1).expect("code length fits u32"), span))
            .collect();
        Ok((function, placed, facts, marks))
    }
}

// zero loads for the dead payload words of a constructed member: a
// union span's every word carries a value of its declared kind, so
// whole-span moves and typed callee parameters never see a foreign kind
// (the live member's words are the only ones ever read)
impl FnLower<'_, '_, '_> {
    /// The native-by-name key a native surface's pool FuncRef addresses:
    /// the builtin core's reserved name, or the platform declaration's
    /// name for the world surface.
    pub(super) fn native_key(&self, module: crate::native::NativeModule) -> FuncRefKey {
        match module {
            crate::native::NativeModule::Builtin => {
                FuncRefKey::Native(crate::native::BUILTIN_MODULE_NAME.to_owned())
            },
            crate::native::NativeModule::Platform => FuncRefKey::Native(self.platform.clone()),
            crate::native::NativeModule::Comptime => {
                FuncRefKey::Native(crate::native::COMPTIME_MODULE_NAME.to_owned())
            },
        }
    }

    /// The pool FuncRef of a callee: a builtin or platform native's
    /// reserved index under its native-by-name key, the enclosing
    /// module's dep-position key for a local function, the
    /// runtime-evidence sentinel for a staged dispatch site.
    pub(super) fn funcref_const(&self, target: Callee) -> Constant {
        match target {
            Callee::Builtin(native) => Constant::FuncRef {
                key: self.native_key(native.module),
                func: native.index,
            },
            Callee::Bound { def, index } => {
                // the pool constant carries the return's marshaling word
                // the parameters marshal per the staged words' own
                // kinds, so the return alone needs a payload — the scalar
                // word read off the signature the resolution already
                // validated, an absent return the unit word
                let sig = self
                    .analyzed
                    .decls
                    .fns
                    .get(&def)
                    .expect("a bound callee resolved against its signature");
                Constant::Bound {
                    module: sig
                        .bind
                        .clone()
                        .expect("the resolution validated the bind target"),
                    index,
                    ret: sig
                        .ret
                        .as_ref()
                        .and_then(calls::bound_ret_word)
                        .unwrap_or(BoundRet::Unit),
                }
            },
            Callee::Generic(generic) => Constant::FuncRef {
                key: self.native_key(generic.module),
                func: generic.index,
            },
            Callee::Local(index) => Constant::FuncRef {
                key: FuncRefKey::Dep(0), // the enclosing module
                func: index,
            },
            Callee::Foreign { position, func, .. } => Constant::FuncRef {
                // position 0 is the enclosing module, so the dep entry's
                // list position shifts by one.
                key: FuncRefKey::Dep(position + 1),
                func,
            },
            Callee::Evidence(slot) => Constant::FuncRef {
                key: FuncRefKey::Native(crate::native::EVIDENCE_MODULE.to_owned()),
                func: u32::from(slot),
            },
        }
    }

    pub(super) fn zero_word(&mut self, dst: u8, slot: SlotType) {
        let kidx = match slot {
            SlotType::I64 => {
                let k = self.pool(Constant::I64(0));
                self.emit(Op::Kimm {
                    dst: Slot::body(dst),
                    kidx: k,
                });
                return;
            },
            SlotType::F64 => self.pool(Constant::F64(0.0)),
            SlotType::Bool => self.pool(Constant::Bool(false)),
            SlotType::Ref | SlotType::Raw => {
                // the dead payload word owns its interned empty-string
                // reference like any production — a borrow candidate like
                // every KSTR load. A Raw dead word (a variable-keyed
                // member's claim another member's write kills) rides the
                // same discipline: its count is a runtime fact, and the
                // kind-checked dec reads a valid reference whatever the
                // instantiation
                let k = self.pool_str("");
                self.kstr(dst, k);
                return;
            },
            // Fn words need kind-true content too: the call slide
            // validates staged words against typed callee params, so
            // the zero is the frame's own scalar FuncRef — a copyable
            // word the dead member never calls
            SlotType::Fn => {
                let k = self.pool(Constant::FuncRef {
                    key: FuncRefKey::Dep(0), // the enclosing module
                    func: self.index,
                });
                self.emit(Op::Kconst {
                    dst: Slot::body(dst),
                    kidx: k,
                });
                return;
            },
            // A dead Tag word still needs kind-true content: RETSPAN
            // hands the whole span to the caller, whose layout types
            // every word of the union's span, so moving the frame's
            // inert I64 zero into a Tag-classified slot would trap the
            // caller's typed write. Tag id 0 is the module's first
            // minted user tag — a Tag region word only exists once some
            // union field's shape minted one — and a dead word is never
            // read (only the live member's words are), so the
            // discriminant's identity is inert; only its kind matters.
            SlotType::Tag => {
                self.emit(Op::TagNew {
                    dst: Slot::body(dst),
                    tag: 0,
                });
                return;
            },
            // no shape mints an I32 region word (explicit-width locals
            // only), and no uop writes one; unreachable until that
            // vocabulary exists
            SlotType::I32 => return,
        };
        self.emit(Op::Kconst {
            dst: Slot::body(dst),
            kidx,
        });
    }

    /// Writes kind-correct zeros into every payload word the constructed
    /// member does not fill: a field's whole claimed span stays live.
    pub(super) fn zero_init_member(
        &mut self,
        base: u8,
        words: &[SlotType],
        member_fields: &[MemberField],
    ) -> Result<(), LowerError> {
        for (index, slot) in words.iter().enumerate().skip(1) {
            let word = u8::try_from(index).map_err(|_| mismatch("a union span"))?;
            let live = member_fields.iter().any(|field| {
                let start = usize::from(field.word);
                (start..start + field.width()).contains(&usize::from(word))
            });
            if !live {
                self.zero_word(base + word, *slot);
            }
        }
        Ok(())
    }
}
