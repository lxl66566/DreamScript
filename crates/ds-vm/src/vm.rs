//! The tier-1 VM: a dense-match dispatch loop [BL-3 note: stable Rust has no
//! computed goto] over encoded uop bodies, proving the locked slot-overlay
//! call ABI [BL-6, H2].
//!
//! # Decode strategy: decode on dispatch
//!
//! The `Vec<u32>` word stream is the program, and the dispatcher decodes
//! each word with [`ds_abi::uop::Uop::decode`] immediately before matching
//! it. A pre-decoded cache is deliberately rejected: the encoded stream is
//! the authoritative form (the dsmod loader mmaps it [BL-7]), tier
//! transitions are in-place J-variant rewrites of the encoded words [H1] —
//! execution must observe them — and a decode cache keyed per version would
//! be extra state to invalidate across hot-reload swaps [H6 item 5]. Load
//! decode-checks every word once, so a dispatch-time
//! [`Trap::BadEncoding`] is a canonicity violation, not a slow path. The
//! decode itself is branch-light by construction: the bias arithmetic is
//! branch-free [BL-1].

use std::sync::Arc;

use ds_abi::{
    CodePc, ConstIdx, FrameLayout, FrameMetadata, SlideDirection, SlotType, TailChain,
    uop::{DecodeError, Uop},
};

use crate::{
    module::{ModuleRegistry, ModuleVersion},
    program::{Const, ConstKind},
    value::{Value, Word},
};

/// Default value-stack limit: 1 Mi words (8 MiB on 64-bit targets).
const DEFAULT_STACK_LIMIT: usize = 1 << 20;

/// The 2^63 boundary f64: the smallest float magnitude that cannot be an
/// i64, for the CVTF2I range check [B7].
const F64_I64_EDGE: f64 = 9_223_372_036_854_775_808.0;

/// A structured stop condition raised as an enum error, never a panic [H1:
/// the per-entry stack-limit compare is the only stack exit].
#[derive(Clone, Debug, PartialEq)]
pub enum Trap {
    /// A frame entry needs more stack words than the limit allows.
    StackOverflow {
        /// Words the entering frame needed.
        needed: usize,
        /// The configured stack limit.
        limit: usize,
    },
    /// An i64 add/sub/mul/div/mod left the i64 range [B7].
    IntegerOverflow,
    /// DIVI64/MODI64 with a zero divisor [B7].
    DivideByZero,
    /// CVTF2I on a NaN or a value outside the i64 range [B7].
    CvtOutOfRange,
    /// A run entry resolved through no current module slot.
    UnknownFunction {
        /// The entry module's name.
        module: String,
        /// The entry function's name.
        func: String,
    },
    /// A pool FuncRef resolved to no function in the target module's
    /// current slot.
    UnknownCallee {
        /// The target module's name.
        module: String,
        /// The target function-table index.
        func: u32,
    },
    /// A call or run passed the wrong argument count.
    ArityMismatch {
        /// Parameter count the callee's layout declares.
        expected: usize,
        /// Argument count the call staged.
        given: usize,
    },
    /// A slot holds (or a uop produces) a value of the wrong static type.
    SlotTypeMismatch {
        /// The offending slot index.
        slot: u8,
        /// The slot's static type.
        expected: SlotType,
        /// The value's type.
        actual: SlotType,
    },
    /// A uop addressed a slot outside its function's frame.
    SlotOutOfRange {
        /// The addressed slot index.
        slot: u8,
        /// The function's slot count.
        frame_slots: usize,
    },
    /// The ABI's own call-site layout check rejected the call pair.
    CallLayout(ds_abi::SlideError),
    /// Control left the function's code stream.
    InvalidPc {
        /// The offending instruction index.
        pc: u32,
        /// The function's instruction count.
        code_len: u32,
    },
    /// `FUNCF`'s slot count disagrees with the function's layout.
    FrameSlotsMismatch {
        /// The `FUNCF` operand.
        funcef: u16,
        /// The layout's slot count.
        layout: usize,
    },
    /// The entry descriptor's resolved word disagrees with the table index.
    EntryWordMismatch {
        /// What `resolve()` answered.
        resolved: usize,
        /// The function's table index.
        expected: usize,
    },
    /// A pool index is out of range for the current module version's pool.
    PoolIndex {
        /// The offending pool index.
        kidx: u16,
    },
    /// A pool entry was loaded by an opcode of the wrong kind.
    PoolKind {
        /// The offending pool index.
        kidx: u16,
        /// The kind the opcode requires.
        expected: ConstKind,
        /// The kind the entry holds.
        found: ConstKind,
    },
    /// A body word does not decode: the stream is not canonical. Load
    /// rejects these, so this is defensive.
    BadEncoding {
        /// The instruction index of the word.
        pc: u32,
        /// Why the word does not decode.
        cause: DecodeError,
    },
    /// A frame base or slot holds a value word where the metadata word must
    /// be (or vice versa): hand-assembled code corrupted the layout.
    MetaWordCorrupt {
        /// The frame base whose metadata word is wrong.
        fp: usize,
    },
}

/// One entry of the call-info stack: the frame base plus the module version
/// and function the frame runs. Frames live inline in the value stack; this
/// records which version's code and pool a frame executes, so retiring
/// versions stay runnable until their frames return [H6 item 5].
#[derive(Clone, Debug)]
struct CallInfo {
    fp: usize,
    module: Arc<ModuleVersion>,
    func: u32,
}

/// The function the dispatcher is executing.
struct Active {
    version: Arc<ModuleVersion>,
    func: u32,
}

/// Where a call transfers control next.
struct Transfer {
    version: Arc<ModuleVersion>,
    func: u32,
}

/// The outcome of a `RET`.
enum RetOutcome {
    /// The run's entry frame returned; `Value` is the run result.
    Root(Value),
    /// Resume the caller's function at the metadata word's return pc.
    Resume {
        version: Arc<ModuleVersion>,
        func: u32,
        pc: CodePc,
    },
}

/// The mutable execution state the frame operations share, borrowed disjoint
/// from the dispatcher's current-function snapshot.
struct StackState<'a> {
    stack: &'a mut Vec<Word>,
    callinfo: &'a mut Vec<CallInfo>,
    limit: usize,
    max_len: &'a mut usize,
}

impl StackState<'_> {
    /// Grows the stack to `new_len` after the per-entry limit compare [H1].
    /// Never shrinks; resizes never move live frames' bases.
    fn grow(&mut self, new_len: usize) -> Result<(), Trap> {
        if new_len <= self.stack.len() {
            return Ok(());
        }
        if new_len > self.limit {
            return Err(Trap::StackOverflow {
                needed: new_len,
                limit: self.limit,
            });
        }
        self.stack.resize(new_len, Word::Slot(Value::I64(0)));
        *self.max_len = (*self.max_len).max(new_len);
        Ok(())
    }

    /// Resolves a pool FuncRef through the current module slot, then through
    /// its entry descriptor's `resolve()` (word 1 when compiled, word 0
    /// otherwise; word 0 is immutable after load [H2]). The pool belongs to
    /// the CALLER's module version: instruction stream and pool are
    /// version-stable, callee resolution follows the target's current slot
    /// [H6 item 5].
    fn resolve_callee(
        pool: &[Const],
        registry: &ModuleRegistry,
        kidx: ConstIdx,
        nargs: u8,
    ) -> Result<(Arc<ModuleVersion>, u32), Trap> {
        let found = pool
            .get(usize::from(kidx.0))
            .ok_or(Trap::PoolIndex { kidx: kidx.0 })?;
        let Const::FuncRef { module, func } = found else {
            return Err(Trap::PoolKind {
                kidx: kidx.0,
                expected: ConstKind::FuncRef,
                found: found.kind(),
            });
        };
        let (version, func_id) =
            registry
                .resolve_indexed(module, *func)
                .ok_or(Trap::UnknownCallee {
                    module: module.clone(),
                    func: *func,
                })?;
        let target = &version.functions()[func_id as usize];
        let entry = target.entry.resolve();
        if entry != func_id as usize {
            return Err(Trap::EntryWordMismatch {
                resolved: entry,
                expected: func_id as usize,
            });
        }
        if usize::from(nargs) != target.layout.params_len() {
            return Err(Trap::ArityMismatch {
                expected: target.layout.params_len(),
                given: usize::from(nargs),
            });
        }
        Ok((version, func_id))
    }

    /// Non-tail `CALL`: the callee's metadata word lands in the caller's
    /// call word, the staged arguments overlay the caller's outgoing-argument
    /// area with no copying, and the stack grows for the callee's frame
    /// [BL-6, H2].
    fn call(
        &mut self,
        registry: &ModuleRegistry,
        pool: &[Const],
        kidx: ConstIdx,
        nargs: u8,
        caller: &FrameLayout,
        fp: usize,
        return_pc: u32,
    ) -> Result<Transfer, Trap> {
        let (version, func_id) = Self::resolve_callee(pool, registry, kidx, nargs)?;
        let target = &version.functions()[func_id as usize];
        // The ABI's own overlay fact, re-checked at this call site.
        ds_abi::slide_plan(caller, &target.layout).map_err(Trap::CallLayout)?;
        let callee_fp = fp + 1 + caller.arg_area().start;
        self.grow(callee_fp + target.layout.frame_size() as usize)?;
        self.stack[callee_fp] = Word::Meta(FrameMetadata {
            return_pc: CodePc(return_pc),
            frame_size: target.layout.frame_size(),
            tail: TailChain::default(),
        });
        self.callinfo.push(CallInfo {
            fp: callee_fp,
            module: Arc::clone(&version),
            func: func_id,
        });
        Ok(Transfer {
            version,
            func: func_id,
        })
    }

    /// `CALLT`, the locked contract [H2]: keep the frame pointer, slide the
    /// staged arguments into slots `0..nargs` per the ABI slide plan (source
    /// derived from the caller's arg area), rewrite the metadata word in
    /// place (return pc preserved, callee frame size, tail chain grows by
    /// one elided frame), and enter the callee. The stack never grows at any
    /// depth; the only growth here is the one-time span extension when the
    /// callee's frame is physically larger, which is bounded by the largest
    /// callee frame, not by call depth.
    fn tail_call(
        &mut self,
        registry: &ModuleRegistry,
        pool: &[Const],
        kidx: ConstIdx,
        nargs: u8,
        caller: &FrameLayout,
        fp: usize,
    ) -> Result<Transfer, Trap> {
        let (version, func_id) = Self::resolve_callee(pool, registry, kidx, nargs)?;
        let target = &version.functions()[func_id as usize];
        let plan = ds_abi::slide_plan(caller, &target.layout).map_err(Trap::CallLayout)?;
        // The O(delta) slide, per the plan's direction. Two different slot
        // mappings meet here: the source slots are the CALLER's staged
        // arguments, which sit caller-relative (above the caller's call
        // word); the destination is the callee reusing the frame at the same
        // fp, whose param slot i sits at fp+1+i unconditionally, below the
        // CALLEE's own call word - never mapped through the caller's body,
        // or wide callees (params_len > caller body) would land one word off.
        let caller_body_len = caller.slots().len() - caller.arg_area_len();
        let n = plan.len();
        let (dst0, src0) = (plan.dst().start, plan.src().start);
        match plan.direction() {
            SlideDirection::None => {},
            SlideDirection::Ascending => {
                for i in 0..n {
                    let (d, s) = (fp + 1 + dst0 + i, word_of(fp, src0 + i, caller_body_len));
                    self.stack[d] = self.stack[s];
                }
            },
            SlideDirection::Descending => {
                for i in (0..n).rev() {
                    let (d, s) = (fp + 1 + dst0 + i, word_of(fp, src0 + i, caller_body_len));
                    self.stack[d] = self.stack[s];
                }
            },
        }
        let Word::Meta(meta) = &mut self.stack[fp] else {
            return Err(Trap::MetaWordCorrupt { fp });
        };
        meta.enter_tail_call(target.layout.frame_size());
        self.grow(fp + target.layout.frame_size() as usize)?;
        let frame = self.callinfo.last_mut().expect("a frame is always live");
        frame.module = Arc::clone(&version);
        frame.func = func_id;
        Ok(Transfer {
            version,
            func: func_id,
        })
    }

    /// `RET`: the result word overlays slot 0 (the caller reads it from its
    /// outgoing-argument slot 0 [H2]); the stack restores the caller's frame
    /// span; the caller resumes at the metadata word's return pc.
    fn ret(&mut self, value: Value, fp: usize) -> Result<RetOutcome, Trap> {
        let meta = match self.stack[fp] {
            Word::Meta(m) => m,
            Word::Slot(_) => return Err(Trap::MetaWordCorrupt { fp }),
        };
        self.stack[fp + 1] = Word::Slot(value);
        if self.callinfo.len() == 1 {
            self.stack.truncate(fp + 2);
            return Ok(RetOutcome::Root(value));
        }
        let caller = self.callinfo[self.callinfo.len() - 2].clone();
        let caller_fn = &caller.module.functions()[caller.func as usize];
        let code_len = code_len_u32(caller_fn.code.len());
        if meta.return_pc.0 >= code_len {
            return Err(Trap::InvalidPc {
                pc: meta.return_pc.0,
                code_len,
            });
        }
        let restore = (caller.fp + caller_fn.layout.frame_size() as usize).max(fp + 2);
        self.stack.truncate(restore);
        self.callinfo.pop();
        Ok(RetOutcome::Resume {
            version: caller.module,
            func: caller.func,
            pc: meta.return_pc,
        })
    }
}

fn code_len_u32(len: usize) -> u32 {
    u32::try_from(len).expect("code length exceeds u32")
}

/// Maps a slot index to its stack-word index: parameters and locals sit at
/// `fp + 1 + i`; the call word sits between them and the outgoing-argument
/// area, whose slots live at `fp + 2 + i` (the frame diagram in
/// `ds-abi/src/layout.rs`). `body_len` is the parameter-plus-local count of
/// the frame's OWNING layout.
#[inline]
fn word_of(fp: usize, slot: usize, body_len: usize) -> usize {
    fp + 1 + slot + usize::from(slot >= body_len)
}

fn read_value(
    stack: &[Word],
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
) -> Result<Value, Trap> {
    let i = usize::from(slot);
    if i >= slots.len() {
        return Err(Trap::SlotOutOfRange {
            slot,
            frame_slots: slots.len(),
        });
    }
    let at = word_of(fp, i, body_len);
    debug_assert!(at < stack.len(), "slot inside the frame span");
    match stack[at] {
        Word::Slot(v) => Ok(v),
        Word::Meta(_) => Err(Trap::MetaWordCorrupt { fp }),
    }
}

/// Reads a slot typed `want`; the value-type check doubles as the check of
/// the static layout (writes enforce layout and value agreement).
fn read_typed(
    stack: &[Word],
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
    want: SlotType,
) -> Result<Value, Trap> {
    let v = read_value(stack, fp, slot, slots, body_len)?;
    if v.slot_type() != want {
        return Err(Trap::SlotTypeMismatch {
            slot,
            expected: want,
            actual: v.slot_type(),
        });
    }
    Ok(v)
}

fn read_i64(
    stack: &[Word],
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
) -> Result<i64, Trap> {
    match read_typed(stack, fp, slot, slots, body_len, SlotType::I64)? {
        Value::I64(v) => Ok(v),
        _ => unreachable!("slot_type agrees with the variant"),
    }
}

fn read_f64(
    stack: &[Word],
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
) -> Result<f64, Trap> {
    match read_typed(stack, fp, slot, slots, body_len, SlotType::F64)? {
        Value::F64(v) => Ok(v),
        _ => unreachable!("slot_type agrees with the variant"),
    }
}

fn read_bool(
    stack: &[Word],
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
) -> Result<bool, Trap> {
    match read_typed(stack, fp, slot, slots, body_len, SlotType::Bool)? {
        Value::Bool(v) => Ok(v),
        _ => unreachable!("slot_type agrees with the variant"),
    }
}

fn read_tag(
    stack: &[Word],
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
) -> Result<u8, Trap> {
    match read_typed(stack, fp, slot, slots, body_len, SlotType::Tag)? {
        Value::Tag(v) => Ok(v),
        _ => unreachable!("slot_type agrees with the variant"),
    }
}

/// Writes a value into a slot, checking it against the slot's static type:
/// hand-assembled code that mis-stages a value traps here.
fn write_slot(
    stack: &mut [Word],
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
    value: Value,
) -> Result<(), Trap> {
    let i = usize::from(slot);
    if i >= slots.len() {
        return Err(Trap::SlotOutOfRange {
            slot,
            frame_slots: slots.len(),
        });
    }
    if slots[i] != value.slot_type() {
        return Err(Trap::SlotTypeMismatch {
            slot,
            expected: slots[i],
            actual: value.slot_type(),
        });
    }
    let at = word_of(fp, i, body_len);
    debug_assert!(at < stack.len(), "slot inside the frame span");
    stack[at] = Word::Slot(value);
    Ok(())
}

/// DIVI64 [B7]: zero divide and `MIN / -1` are distinct traps.
fn int_div(x: i64, y: i64) -> Result<i64, Trap> {
    if y == 0 {
        return Err(Trap::DivideByZero);
    }
    x.checked_div(y).ok_or(Trap::IntegerOverflow)
}

/// MODI64 [B7]: same trap split as [`int_div`].
fn int_mod(x: i64, y: i64) -> Result<i64, Trap> {
    if y == 0 {
        return Err(Trap::DivideByZero);
    }
    x.checked_rem(y).ok_or(Trap::IntegerOverflow)
}

/// One backtrace entry: a real frame plus its tail chain's elided count [H2].
#[derive(Clone, Debug, PartialEq)]
pub struct BacktraceFrame {
    /// The frame's module.
    pub module: String,
    /// The frame's function.
    pub func: String,
    /// Where the frame is: the trapping uop for the innermost frame, the
    /// callee's saved return pc for callers.
    pub pc: CodePc,
    /// How many logical frames this physical frame elided via tail calls
    /// since the normal call that created it.
    pub elided: u32,
}

/// The tier-1 VM. Construct with a [`ModuleRegistry`], then run encoded
/// entry functions.
#[derive(Debug)]
pub struct Vm {
    registry: ModuleRegistry,
    stack: Vec<Word>,
    callinfo: Vec<CallInfo>,
    limit: usize,
    max_stack_len: usize,
    trap_pc: Option<CodePc>,
}

impl Vm {
    /// A VM with the default stack limit.
    #[must_use]
    pub fn new(registry: ModuleRegistry) -> Self {
        Self::with_limit(registry, DEFAULT_STACK_LIMIT)
    }

    /// A VM with an explicit value-stack word limit.
    #[must_use]
    pub fn with_limit(registry: ModuleRegistry, limit: usize) -> Self {
        Self {
            registry,
            stack: Vec::new(),
            callinfo: Vec::new(),
            limit,
            max_stack_len: 0,
            trap_pc: None,
        }
    }

    /// Installs a module version into its two-slot table (hot reload swap).
    pub fn install_module(&mut self, version: ModuleVersion) -> Arc<ModuleVersion> {
        self.registry.install(version)
    }

    /// Installs a dsmod module into its two-slot table: the loader maps the
    /// payload, fills the entry-descriptor placeholders, and the slots swap
    /// [BL-7, H6 item 5].
    ///
    /// # Errors
    ///
    /// Returns [`crate::LoadError`] for unqualified or unresolved callee
    /// strings and for bodies failing the load checks.
    pub fn install_dsmod(
        &mut self,
        module: &ds_mod::Module,
    ) -> Result<Arc<ModuleVersion>, crate::LoadError> {
        crate::loader::install(&mut self.registry, module)
    }

    /// The registry, for slot inspection.
    #[must_use]
    pub fn registry(&self) -> &ModuleRegistry {
        &self.registry
    }

    /// Current value-stack word count.
    #[must_use]
    pub fn stack_len(&self) -> usize {
        self.stack.len()
    }

    /// High-water mark of the value stack over the last run. Constant across
    /// unbounded tail recursion is the tail-call acceptance criterion.
    #[must_use]
    pub fn max_stack_len(&self) -> usize {
        self.max_stack_len
    }

    /// Runs an entry function to its `HALT` or entry-frame `RET` and reports
    /// the resulting value. Execution state resets per run; on a `Trap` the
    /// state is kept for [`Vm::backtrace`].
    pub fn run(&mut self, module: &str, func: &str, args: &[Value]) -> Result<Value, Trap> {
        self.trap_pc = None;
        let (version, func_id) =
            self.registry
                .resolve(module, func)
                .ok_or_else(|| Trap::UnknownFunction {
                    module: module.into(),
                    func: func.into(),
                })?;
        let target = &version.functions()[func_id as usize];
        if args.len() != target.layout.params_len() {
            return Err(Trap::ArityMismatch {
                expected: target.layout.params_len(),
                given: args.len(),
            });
        }
        for (i, (&arg, &ty)) in args.iter().zip(target.layout.params()).enumerate() {
            if arg.slot_type() != ty {
                return Err(Trap::SlotTypeMismatch {
                    slot: u8::try_from(i).expect("argument index exceeds u8"),
                    expected: ty,
                    actual: arg.slot_type(),
                });
            }
        }
        let frame_size = target.layout.frame_size() as usize;
        if frame_size > self.limit {
            return Err(Trap::StackOverflow {
                needed: frame_size,
                limit: self.limit,
            });
        }
        // Root frame: the return-pc sentinel is never read, because a RET
        // from the entry frame ends the run instead of returning.
        self.stack.clear();
        self.stack.push(Word::Meta(FrameMetadata {
            return_pc: CodePc(u32::MAX),
            frame_size: target.layout.frame_size(),
            tail: TailChain::default(),
        }));
        self.stack.extend(args.iter().map(|&a| Word::Slot(a)));
        self.stack.resize(frame_size, Word::Slot(Value::I64(0)));
        self.max_stack_len = frame_size;
        self.callinfo.clear();
        self.callinfo.push(CallInfo {
            fp: 0,
            module: Arc::clone(&version),
            func: func_id,
        });
        let active = Active {
            version,
            func: func_id,
        };
        match self.execute(active, CodePc(0)) {
            Ok(v) => Ok(v),
            Err((trap, at)) => {
                self.trap_pc = Some(at);
                Err(trap)
            },
        }
    }

    /// The dispatcher: decode-on-dispatch (see the module docs), then a
    /// dense `match` over the uop [BL-3 note: computed goto is not available
    /// on stable Rust].
    // One flat match over the whole uop set is the point of a dispatch loop
    // [BL-3]; splitting arms into helpers would fragment it and add a call
    // per op in debug builds.
    #[allow(clippy::too_many_lines)]
    fn execute(&mut self, mut active: Active, mut pc: CodePc) -> Result<Value, (Trap, CodePc)> {
        loop {
            let f = &active.version.functions()[active.func as usize];
            let code = &f.code;
            let slots = f.layout.slots();
            let frame_slots = slots.len();
            let body_len = frame_slots - f.layout.arg_area_len();
            let pool = &f.pool;
            let idx = pc.0 as usize;
            if idx >= code.len() {
                let e = Trap::InvalidPc {
                    pc: pc.0,
                    code_len: code_len_u32(code.len()),
                };
                return Err((e, pc));
            }
            let fp = self.callinfo[self.callinfo.len() - 1].fp;
            macro_rules! jump {
                ($target:expr) => {{
                    let t = $target;
                    if t as usize >= code.len() {
                        let e = Trap::InvalidPc {
                            pc: t,
                            code_len: code_len_u32(code.len()),
                        };
                        return Err((e, pc));
                    }
                    pc = CodePc(t);
                }};
            }
            macro_rules! stack_state {
                () => {
                    StackState {
                        stack: &mut self.stack,
                        callinfo: &mut self.callinfo,
                        limit: self.limit,
                        max_len: &mut self.max_stack_len,
                    }
                };
            }
            let op = match Uop::decode(pc.0, code[idx]) {
                Ok(op) => op,
                Err(cause) => return Err((Trap::BadEncoding { pc: pc.0, cause }, pc)),
            };
            match op {
                Uop::Funcf { frame_slots: n } => {
                    if usize::from(n) != frame_slots {
                        let e = Trap::FrameSlotsMismatch {
                            funcef: n,
                            layout: frame_slots,
                        };
                        return Err((e, pc));
                    }
                    pc.0 += 1;
                },
                Uop::Kimm { dst, kidx } => {
                    let v = match pool.get(usize::from(kidx.0)) {
                        Some(Const::I64(v)) => *v,
                        Some(found) => {
                            let e = Trap::PoolKind {
                                kidx: kidx.0,
                                expected: ConstKind::I64,
                                found: found.kind(),
                            };
                            return Err((e, pc));
                        },
                        None => return Err((Trap::PoolIndex { kidx: kidx.0 }, pc)),
                    };
                    write_slot(&mut self.stack, fp, dst, slots, body_len, Value::I64(v))
                        .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::Kconst { dst, kidx } => {
                    let v = match pool.get(usize::from(kidx.0)) {
                        Some(Const::F64(v)) => Value::F64(*v),
                        Some(Const::Bool(v)) => Value::Bool(*v),
                        Some(found) => {
                            let e = Trap::PoolKind {
                                kidx: kidx.0,
                                expected: ConstKind::F64,
                                found: found.kind(),
                            };
                            return Err((e, pc));
                        },
                        None => return Err((Trap::PoolIndex { kidx: kidx.0 }, pc)),
                    };
                    write_slot(&mut self.stack, fp, dst, slots, body_len, v)
                        .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::AddI64 { dst, lhs, rhs } => {
                    let (x, y) = (
                        read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                        read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                    );
                    let Some(sum) = x.checked_add(y) else {
                        return Err((Trap::IntegerOverflow, pc));
                    };
                    write_slot(&mut self.stack, fp, dst, slots, body_len, Value::I64(sum))
                        .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::SubI64 { dst, lhs, rhs } => {
                    let (x, y) = (
                        read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                        read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                    );
                    let Some(diff) = x.checked_sub(y) else {
                        return Err((Trap::IntegerOverflow, pc));
                    };
                    write_slot(&mut self.stack, fp, dst, slots, body_len, Value::I64(diff))
                        .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::MulI64 { dst, lhs, rhs } => {
                    let (x, y) = (
                        read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                        read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                    );
                    let Some(prod) = x.checked_mul(y) else {
                        return Err((Trap::IntegerOverflow, pc));
                    };
                    write_slot(&mut self.stack, fp, dst, slots, body_len, Value::I64(prod))
                        .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::DivI64 { dst, lhs, rhs } => {
                    let (x, y) = (
                        read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                        read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                    );
                    let q = int_div(x, y).map_err(|e| (e, pc))?;
                    write_slot(&mut self.stack, fp, dst, slots, body_len, Value::I64(q))
                        .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::ModI64 { dst, lhs, rhs } => {
                    let (x, y) = (
                        read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                        read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                    );
                    let r = int_mod(x, y).map_err(|e| (e, pc))?;
                    write_slot(&mut self.stack, fp, dst, slots, body_len, Value::I64(r))
                        .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::AddF64 { dst, lhs, rhs } => {
                    let (x, y) = (
                        read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                        read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                    );
                    write_slot(&mut self.stack, fp, dst, slots, body_len, Value::F64(x + y))
                        .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::SubF64 { dst, lhs, rhs } => {
                    let (x, y) = (
                        read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                        read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                    );
                    write_slot(&mut self.stack, fp, dst, slots, body_len, Value::F64(x - y))
                        .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::MulF64 { dst, lhs, rhs } => {
                    let (x, y) = (
                        read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                        read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                    );
                    write_slot(&mut self.stack, fp, dst, slots, body_len, Value::F64(x * y))
                        .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::DivF64 { dst, lhs, rhs } => {
                    let (x, y) = (
                        read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                        read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                    );
                    // IEEE division: a zero divisor produces inf, not a trap
                    write_slot(&mut self.stack, fp, dst, slots, body_len, Value::F64(x / y))
                        .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::EqI64 { dst, lhs, rhs } => {
                    let (x, y) = (
                        read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                        read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                    );
                    write_slot(
                        &mut self.stack,
                        fp,
                        dst,
                        slots,
                        body_len,
                        Value::Bool(x == y),
                    )
                    .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::NeqI64 { dst, lhs, rhs } => {
                    let (x, y) = (
                        read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                        read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                    );
                    write_slot(
                        &mut self.stack,
                        fp,
                        dst,
                        slots,
                        body_len,
                        Value::Bool(x != y),
                    )
                    .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::LtI64 { dst, lhs, rhs } => {
                    let (x, y) = (
                        read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                        read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                    );
                    write_slot(
                        &mut self.stack,
                        fp,
                        dst,
                        slots,
                        body_len,
                        Value::Bool(x < y),
                    )
                    .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::GeI64 { dst, lhs, rhs } => {
                    let (x, y) = (
                        read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                        read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                    );
                    write_slot(
                        &mut self.stack,
                        fp,
                        dst,
                        slots,
                        body_len,
                        Value::Bool(x >= y),
                    )
                    .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::LeI64 { dst, lhs, rhs } => {
                    let (x, y) = (
                        read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                        read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                    );
                    write_slot(
                        &mut self.stack,
                        fp,
                        dst,
                        slots,
                        body_len,
                        Value::Bool(x <= y),
                    )
                    .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::GtI64 { dst, lhs, rhs } => {
                    let (x, y) = (
                        read_i64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                        read_i64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                    );
                    write_slot(
                        &mut self.stack,
                        fp,
                        dst,
                        slots,
                        body_len,
                        Value::Bool(x > y),
                    )
                    .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::EqF64 { dst, lhs, rhs } => {
                    let (x, y) = (
                        read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                        read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                    );
                    // bit-structural equality: reflexive, total, dict-safe
                    // [resolved question 8]
                    let v = x.to_bits() == y.to_bits();
                    write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Bool(v))
                        .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::NeqF64 { dst, lhs, rhs } => {
                    let (x, y) = (
                        read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                        read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                    );
                    let v = x.to_bits() != y.to_bits();
                    write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Bool(v))
                        .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::LtF64 { dst, lhs, rhs } => {
                    let (x, y) = (
                        read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                        read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                    );
                    // partial order: every NaN comparison answers false
                    write_slot(
                        &mut self.stack,
                        fp,
                        dst,
                        slots,
                        body_len,
                        Value::Bool(x < y),
                    )
                    .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::GeF64 { dst, lhs, rhs } => {
                    let (x, y) = (
                        read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                        read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                    );
                    write_slot(
                        &mut self.stack,
                        fp,
                        dst,
                        slots,
                        body_len,
                        Value::Bool(x >= y),
                    )
                    .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::LeF64 { dst, lhs, rhs } => {
                    let (x, y) = (
                        read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                        read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                    );
                    write_slot(
                        &mut self.stack,
                        fp,
                        dst,
                        slots,
                        body_len,
                        Value::Bool(x <= y),
                    )
                    .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::GtF64 { dst, lhs, rhs } => {
                    let (x, y) = (
                        read_f64(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                        read_f64(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                    );
                    write_slot(
                        &mut self.stack,
                        fp,
                        dst,
                        slots,
                        body_len,
                        Value::Bool(x > y),
                    )
                    .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::EqBool { dst, lhs, rhs } => {
                    let (x, y) = (
                        read_bool(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                        read_bool(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                    );
                    write_slot(
                        &mut self.stack,
                        fp,
                        dst,
                        slots,
                        body_len,
                        Value::Bool(x == y),
                    )
                    .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::NeqBool { dst, lhs, rhs } => {
                    let (x, y) = (
                        read_bool(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                        read_bool(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                    );
                    write_slot(
                        &mut self.stack,
                        fp,
                        dst,
                        slots,
                        body_len,
                        Value::Bool(x != y),
                    )
                    .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::TagTest { dst, src, tag } => {
                    let v = read_tag(&self.stack, fp, src, slots, body_len).map_err(|e| (e, pc))?;
                    write_slot(
                        &mut self.stack,
                        fp,
                        dst,
                        slots,
                        body_len,
                        Value::Bool(v == tag),
                    )
                    .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::TagTestNot { dst, src, tag } => {
                    let v = read_tag(&self.stack, fp, src, slots, body_len).map_err(|e| (e, pc))?;
                    write_slot(
                        &mut self.stack,
                        fp,
                        dst,
                        slots,
                        body_len,
                        Value::Bool(v != tag),
                    )
                    .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::TagNew { dst, tag } => {
                    write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Tag(tag))
                        .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::CvtI2F { dst, src } => {
                    let x = read_i64(&self.stack, fp, src, slots, body_len).map_err(|e| (e, pc))?;
                    // precision loss beyond 2^53 is the semantics of the
                    // i64-to-f64 conversion itself
                    #[allow(clippy::cast_precision_loss)]
                    let v = x as f64;
                    write_slot(&mut self.stack, fp, dst, slots, body_len, Value::F64(v))
                        .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::CvtF2I { dst, src } => {
                    let x = read_f64(&self.stack, fp, src, slots, body_len).map_err(|e| (e, pc))?;
                    // trap on NaN and on magnitudes at or past 2^63; -2^63
                    // itself is exactly i64::MIN and converts [B7]
                    if x.is_nan() || x >= F64_I64_EDGE || x < -F64_I64_EDGE {
                        return Err((Trap::CvtOutOfRange, pc));
                    }
                    // in range by the check above, so the cast is exact
                    #[allow(clippy::cast_possible_truncation)]
                    let v = x as i64;
                    write_slot(&mut self.stack, fp, dst, slots, body_len, Value::I64(v))
                        .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::Mov { dst, src } => {
                    let v =
                        read_value(&self.stack, fp, src, slots, body_len).map_err(|e| (e, pc))?;
                    write_slot(&mut self.stack, fp, dst, slots, body_len, v)
                        .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::Jmpif { cond, target } => {
                    if read_bool(&self.stack, fp, cond, slots, body_len).map_err(|e| (e, pc))? {
                        jump!(target);
                    } else {
                        pc.0 += 1;
                    }
                },
                Uop::JmpifNot { cond, target } => {
                    if read_bool(&self.stack, fp, cond, slots, body_len).map_err(|e| (e, pc))? {
                        pc.0 += 1;
                    } else {
                        jump!(target);
                    }
                },
                Uop::Jmp { target } => jump!(target),
                Uop::Call { callee, nargs } => {
                    let mut st = stack_state!();
                    let t = st
                        .call(&self.registry, pool, callee, nargs, &f.layout, fp, pc.0 + 1)
                        .map_err(|e| (e, pc))?;
                    active = Active {
                        version: t.version,
                        func: t.func,
                    };
                    pc = CodePc(0);
                },
                Uop::Callt { callee, nargs } => {
                    let mut st = stack_state!();
                    let t = st
                        .tail_call(&self.registry, pool, callee, nargs, &f.layout, fp)
                        .map_err(|e| (e, pc))?;
                    active = Active {
                        version: t.version,
                        func: t.func,
                    };
                    pc = CodePc(0);
                },
                Uop::Ret { src } => {
                    let value =
                        read_value(&self.stack, fp, src, slots, body_len).map_err(|e| (e, pc))?;
                    let mut st = stack_state!();
                    match st.ret(value, fp).map_err(|e| (e, pc))? {
                        RetOutcome::Root(v) => return Ok(v),
                        RetOutcome::Resume {
                            version,
                            func,
                            pc: at,
                        } => {
                            active = Active { version, func };
                            pc = at;
                        },
                    }
                },
                Uop::Halt { slot } => {
                    return read_value(&self.stack, fp, slot, slots, body_len).map_err(|e| (e, pc));
                },
            }
        }
    }

    /// Renders the live frames, innermost first: each real frame with its
    /// position plus the tail-chain elided count [H2]. The counter is always
    /// available; per-elision `ElisionRecord`s for lazy materialization come
    /// with per-fiber debug recording, so at M3 the count reports how many
    /// logical frames a debugger would materialize later.
    #[must_use]
    pub fn backtrace(&self) -> Vec<BacktraceFrame> {
        let mut frames = Vec::with_capacity(self.callinfo.len());
        for depth in (0..self.callinfo.len()).rev() {
            let ci = &self.callinfo[depth];
            let f = &ci.module.functions()[ci.func as usize];
            let pc = if depth + 1 == self.callinfo.len() {
                self.trap_pc.unwrap_or(CodePc(0))
            } else {
                match self.stack[self.callinfo[depth + 1].fp] {
                    Word::Meta(m) => m.return_pc,
                    Word::Slot(_) => CodePc(0),
                }
            };
            let elided = match self.stack[ci.fp] {
                Word::Meta(m) => m.tail.elided,
                Word::Slot(_) => 0,
            };
            frames.push(BacktraceFrame {
                module: ci.module.name().to_owned(),
                func: f.name.clone(),
                pc,
                elided,
            });
        }
        frames
    }
}
