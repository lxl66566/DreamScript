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
//!
//! # Fibers and nurseries
//!
//! The dispatcher also owns the fiber switch mechanics over the pool in
//! `crate::fiber` [E2]: the control natives park the running fiber (record
//! its resume point), and entering a fiber swaps segment and call-chain
//! ownership between its pool slot and the VM's context — two buffer
//! swaps, O(1), no word copied. A fiber completing at its entry frame
//! marks itself dead and switches back to its resumer with its final
//! value.
//!
//! Nurseries [E2, E5] layer on top (`crate::nursery`): fibers spawn into a
//! nursery (the child-of link), the join/teardown driver drives children to
//! death in spawn order, and a first failure raises the nursery's cancel
//! flag — observed only at taken backward branches (the `jump!` macro), so
//! a linear consume in flight is never interrupted.
//!
//! # Reentrant native calls
//!
//! The lambda-taking generic builtins (map/filter/reduce) call user code
//! from Rust: `Vm::call_hosted` pushes a synthetic reentry frame (marked
//! by the entry-frame sentinel return pc) on the running fiber's chain
//! and drives it through `execute` with a `boundary` depth, nesting
//! stack-style — map-in-lambda stacks another frame on the same chain.
//! A drive that ends the fiber or parks at the host instead of returning
//! refuses with [`Trap::ReentrancyRefused`]: a Rust-side loop cannot
//! suspend. CALLG stays one instruction from the outer loop's view, so
//! the cancel discipline adds no observation point inside the
//! per-element loop; a lambda's own backward jumps observe through the
//! same `jump!` macro as any code.

use std::sync::Arc;

use ds_abi::{
    CodePc, ConstIdx, FrameLayout, FrameMetadata, RC_MAJOR, RcDecodeError, RcOp, SlideDirection,
    SlideError, SlotType, TailChain,
    uop::{DecodeError, Uop},
};

use crate::{
    fiber::{FIBER_ROOT, Fiber, FiberId, FiberState, ResumePoint, Segment},
    module::{ModuleRegistry, ModuleVersion},
    native::{
        ControlNative, NativeFunction, NativeResult, OutputSink, PLATFORM_CANCELLED, PLATFORM_NONE,
    },
    nursery::{ChildOutcome, JoinOutcome, Nursery, NurseryId, NurseryState},
    object::Objects,
    program::{Const, ConstKind},
    value::{FuncRef, Value, Word},
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
    /// current slot. Also the host-side fiber-routing failure
    /// ([`Vm::resume_fiber`]), with the virtual module `fiber` naming the
    /// rejected pool index.
    UnknownCallee {
        /// The target module's name.
        module: String,
        /// The target function-table index.
        func: u32,
    },
    /// A scalar FuncRef word named a module id the registry never
    /// assigned: hand-assembled code or a host argument carried a bogus
    /// callable word.
    UnknownModule {
        /// The word's module id.
        module: u32,
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
    CallLayout(SlideError),
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
    /// A CALLG evidence word names no builtin instance. Lowering only
    /// mints known instance ids, so this names a corrupted stream.
    UnknownInstance {
        /// The staged evidence word.
        evidence: i64,
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
    /// A Ref word named no object (out of range, or the object is not the
    /// required kind — a non-string read as a string, a non-list read as
    /// a list): hand-assembled code or a host argument carried a bogus
    /// reference.
    UnknownObject {
        /// The bogus object-table index.
        obj: u32,
    },
    /// A result span (RETSPAN's, or a fallible native's tag+payload
    /// span) is wider than the caller's outgoing-argument area, so the
    /// caller could never receive it whole.
    RetSpanTooWide {
        /// The span's word count.
        span: u16,
        /// The caller's outgoing-argument slot count.
        caller_area: u16,
    },
    /// A print/println write failed at the output sink.
    OutputWrite {
        /// The class of the underlying io failure.
        kind: std::io::ErrorKind,
    },
    /// A reentrant native drive (a generic container body invoking its
    /// user lambda) could not return: the lambda's execution ended the
    /// fiber — HALT under nesting, a fiber switch whose resumer chain
    /// never came back, a nursery cancel — or parked at the host (a
    /// yield with no in-language resumer). A Rust-side loop cannot
    /// suspend, so the drive refuses instead of corrupting the frame
    /// chain; the frames stay for [`Vm::backtrace`].
    ReentrancyRefused,
}

/// One entry of the call-info stack: the frame base plus the module version
/// and function the frame runs. Frames live inline in the fiber's value
/// stack; this records which version's code and pool a frame executes, so
/// retiring versions stay runnable until their frames return [H6 item 5].
/// The chain belongs to the fiber whose segment holds the frames.
#[derive(Clone, Debug)]
pub(crate) struct CallInfo {
    pub(crate) fp: usize,
    pub(crate) module: Arc<ModuleVersion>,
    pub(crate) func: u32,
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

/// A resolved pool FuncRef: a uop function (resolved through its module's
/// current slot) or a native one (the VM-resident native tables).
#[derive(Debug)]
enum Callee<'a> {
    /// Enter the callee's encoded body.
    Uop {
        /// The callee's module version.
        version: Arc<ModuleVersion>,
        /// The callee's function-table index.
        func: u32,
    },
    /// Invoke the native in the caller's frame (no frame push).
    Native(&'a NativeFunction),
}

/// The outcome of a `RET`.
enum RetOutcome {
    /// The fiber's entry frame returned; `Value` is its final value.
    Root(Value),
    /// The reentry frame of a nested drive returned (the synthetic
    /// entry frame [`Vm::call_hosted`] pushed); `Value` is the hosted
    /// call's answer, and the Rust-side native loop resumes.
    Boundary(Value),
    /// Resume the caller's function at the metadata word's return pc.
    Resume {
        version: Arc<ModuleVersion>,
        func: u32,
        pc: CodePc,
    },
}

/// How a drive of the dispatcher ended for its host: the current fiber
/// completed, it parked at a yield whose resumer is the host, or a
/// nested drive's reentry frame returned.
enum DriveOutcome {
    /// The current fiber's entry frame returned or `HALT`ed.
    Died(Value),
    /// The current fiber yielded with no in-language resumer.
    Yielded(Value),
    /// A nested reentrant call's synthetic frame returned.
    Frame(Value),
}

/// The result of a control-native switch at a `CALL` site.
enum SwitchOutcome {
    /// A fiber was entered: the dispatcher continues here.
    Entered(Active, CodePc),
    /// No switch happened (an unresumable `fiber_resume` target): the
    /// bare `PLATFORM_NONE` bounce tag is on the caller's overlay; the
    /// dispatcher advances past the call.
    Bounced,
    /// The switch reached the host: this drive ends with the event.
    Host(DriveOutcome),
}

/// The mutable execution state the frame operations share, borrowed disjoint
/// from the dispatcher's current-function snapshot. The stack is the
/// current fiber's segment; `max_len` is that fiber's high-water mark, so
/// the TCO criterion holds per fiber [H2].
struct StackState<'a> {
    stack: &'a mut Segment,
    callinfo: &'a mut Vec<CallInfo>,
    limit: usize,
    max_len: &'a mut usize,
}

impl StackState<'_> {
    /// Grows the segment to `new_len` after the per-entry limit compare
    /// [H1]. Never shrinks; growth appends chunks, so live frames' words
    /// keep their addresses [H2].
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
        self.stack.grow_to(new_len);
        *self.max_len = (*self.max_len).max(new_len);
        Ok(())
    }

    /// Resolves a pool FuncRef: uop modules first (the hot path — one
    /// registry lookup), then the native tables (the builtin mechanism —
    /// arity against the native's parameter list); a uop module installed
    /// under a native table's name shadows the natives. Uop resolution
    /// goes through the current module slot and the entry descriptor's
    /// `resolve()` (word 1 when compiled, word 0 otherwise; word 0 is
    /// immutable after load [H2]). The pool belongs to the CALLER's module
    /// version: instruction stream and pool are version-stable, callee
    /// resolution follows the target's current slot [H6 item 5].
    fn resolve_callee<'r>(
        pool: &[Const],
        registry: &'r ModuleRegistry,
        kidx: ConstIdx,
        nargs: u8,
    ) -> Result<Callee<'r>, Trap> {
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
        if let Some((version, func_id)) = registry.resolve_indexed(module, *func) {
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
            return Ok(Callee::Uop {
                version,
                func: func_id,
            });
        }
        let native = registry
            .resolve_native(module, *func)
            .ok_or(Trap::UnknownCallee {
                module: module.clone(),
                func: *func,
            })?;
        if usize::from(nargs) != native.params.len() {
            return Err(Trap::ArityMismatch {
                expected: native.params.len(),
                given: usize::from(nargs),
            });
        }
        Ok(Callee::Native(native))
    }

    /// Resolves a scalar FuncRef word (module id + function index, the
    /// `Fn` slot classification, F2) through the two-slot module table at
    /// call time [H6 item 5]: the id names a module, the index names a
    /// function in its current slot's table. Resolution never caches a
    /// code pointer, so a hot-reload swap redirects a staged fn word at
    /// its next call exactly like every cross-module call. Callable words
    /// address the uop module tables only — natives are not values.
    fn resolve_func_word(
        registry: &ModuleRegistry,
        funcref: FuncRef,
        nargs: u8,
    ) -> Result<(Arc<ModuleVersion>, u32), Trap> {
        let name = registry
            .module_name(funcref.module())
            .ok_or(Trap::UnknownModule {
                module: funcref.module(),
            })?;
        let (version, func_id) =
            registry
                .resolve_indexed(name, funcref.func())
                .ok_or(Trap::UnknownCallee {
                    module: name.to_owned(),
                    func: funcref.func(),
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

    /// The per-call-site check for `Raw`-staged arguments: they carry no
    /// static type, so the staged value words are checked against the
    /// callee's parameter types here (concrete staging was already proved
    /// by [`ds_abi::slide_plan`]).
    fn check_raw_staged(
        stack: &Segment,
        target_params: &[SlotType],
        caller: &FrameLayout,
        fp: usize,
    ) -> Result<(), Trap> {
        let body_len = caller.slots().len() - caller.arg_area_len();
        let start = caller.arg_area().start;
        for (index, (param, &staged_ty)) in target_params.iter().zip(caller.arg_types()).enumerate()
        {
            if staged_ty != SlotType::Raw {
                continue;
            }
            let slot = u8::try_from(start + index).expect("slot index fits u8");
            let value = read_value(stack, fp, slot, caller.slots(), body_len)?;
            let staged = value.slot_type();
            // a Ref word staged against a Fn parameter is the
            // callable-defaulted slot (F2): a closure object's word behind
            // a fn-typed parameter, admitted exactly like the slide admits
            // the static pairing
            if staged != *param && !(staged == SlotType::Ref && *param == SlotType::Fn) {
                return Err(Trap::CallLayout(SlideError::TypeMismatch {
                    index,
                    staged,
                    expected: *param,
                }));
            }
        }
        Ok(())
    }

    /// Non-tail `CALL`: the callee's metadata word lands in the caller's
    /// call word, the staged arguments overlay the caller's outgoing-argument
    /// area with no copying, and the stack grows for the callee's frame
    /// [BL-6, H2].
    fn call(
        &mut self,
        version: Arc<ModuleVersion>,
        func_id: u32,
        caller: &FrameLayout,
        fp: usize,
        return_pc: u32,
    ) -> Result<Transfer, Trap> {
        let target = &version.functions()[func_id as usize];
        // The ABI's own overlay fact, re-checked at this call site.
        ds_abi::slide_plan(caller, &target.layout).map_err(Trap::CallLayout)?;
        Self::check_raw_staged(self.stack, target.layout.params(), caller, fp)?;
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
        version: Arc<ModuleVersion>,
        func_id: u32,
        caller: &FrameLayout,
        fp: usize,
    ) -> Result<Transfer, Trap> {
        let target = &version.functions()[func_id as usize];
        let plan = ds_abi::slide_plan(caller, &target.layout).map_err(Trap::CallLayout)?;
        Self::check_raw_staged(self.stack, target.layout.params(), caller, fp)?;
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
    /// span; the caller resumes at the metadata word's return pc. A nested
    /// drive's synthetic reentry frame answers [`RetOutcome::Boundary`]
    /// instead: its caller is Rust-side code (`Vm::call_hosted`), not a
    /// frame. The sentinel return pc is the frame's identity marker — every
    /// CALL-pushed frame carries a real return pc, and a fiber switched in
    /// mid-drive never carries a reentry frame at all (a resumable fiber
    /// parks only at switch points, never mid-native), so the
    /// chain-position test cannot false-positive across a switch.
    fn ret(&mut self, value: Value, fp: usize, boundary: usize) -> Result<RetOutcome, Trap> {
        let meta = match self.stack[fp] {
            Word::Meta(m) => m,
            Word::Slot(_) => return Err(Trap::MetaWordCorrupt { fp }),
        };
        self.stack[fp + 1] = Word::Slot(value);
        if self.callinfo.len() == 1 {
            self.stack.truncate(fp + 2);
            return Ok(RetOutcome::Root(value));
        }
        if boundary != usize::MAX && self.callinfo.len() == boundary && meta.return_pc.0 == u32::MAX
        {
            // the reentry frame and everything above it leave the chain;
            // the value rides the outcome
            self.stack.truncate(fp);
            self.callinfo.pop();
            return Ok(RetOutcome::Boundary(value));
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
    stack: &Segment,
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
    stack: &Segment,
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
    stack: &Segment,
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
    stack: &Segment,
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
    stack: &Segment,
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
    stack: &Segment,
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

fn read_ref(
    stack: &Segment,
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
) -> Result<u32, Trap> {
    match read_typed(stack, fp, slot, slots, body_len, SlotType::Ref)? {
        Value::Ref(v) => Ok(v),
        _ => unreachable!("slot_type agrees with the variant"),
    }
}

/// Writes a value into a slot, checking it against the slot's static type:
/// hand-assembled code that mis-stages a value traps here.
fn write_slot(
    stack: &mut Segment,
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
    if slots[i] != value.slot_type() && slots[i] != SlotType::Raw {
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

/// Runs a native in the caller's frame: reads the staged overlay words
/// as arguments (type-checked against the native's parameter list, the
/// same discipline a uop callee's layout gets) and invokes the body. The
/// CALL site then places the result on the caller's overlay words
/// ([`overlay_native`]); the CALLT site returns it from the tail
/// position. No frame is pushed and no metadata word moves. A free
/// function over the disjoint state the dispatcher hands it (stack, heap,
/// sink), so the registry borrow the callee came from stays alive.
fn invoke_native(
    nf: &NativeFunction,
    stack: &mut Segment,
    objects: &mut Objects,
    out: &mut OutputSink,
    caller: &FrameLayout,
    fp: usize,
) -> Result<NativeResult, Trap> {
    let body_len = caller.slots().len() - caller.arg_area_len();
    if nf.params.len() > caller.arg_area_len() {
        return Err(Trap::CallLayout(SlideError::StagingTooSmall {
            available: caller.arg_area_len(),
            needed: nf.params.len(),
        }));
    }
    let start = caller.arg_area().start;
    let mut args = Vec::with_capacity(nf.params.len());
    for i in 0..nf.params.len() {
        let slot = u8::try_from(start + i).expect("slot index fits u8");
        args.push(read_value(stack, fp, slot, caller.slots(), body_len)?);
    }
    for (index, (&value, &want)) in args.iter().zip(nf.params).enumerate() {
        // a `Raw` parameter is the wildcard: the native's argument kind
        // varies by call site (list_push's element), so the body receives
        // whatever word staged
        let staged = value.slot_type();
        if staged != want && want != SlotType::Raw {
            return Err(Trap::CallLayout(SlideError::TypeMismatch {
                index,
                staged,
                expected: want,
            }));
        }
    }
    (nf.func)(objects, out, &args)
}

/// Places a native's result on the caller's outgoing-argument overlay:
/// one word on argument slot 0, or a fallible native's two-word
/// tag+payload span [BL-2] on argument slots 0 and 1. Like a RET's write,
/// this bypasses slot typing: the overlay words belong to the call ABI,
/// and reading them as typed slots is the lowering's discipline (mixed
/// shapes declare the slots `Raw`). The call site pre-checks the width,
/// so both writes stay in range by construction.
fn overlay_native(result: NativeResult, stack: &mut Segment, caller: &FrameLayout, fp: usize) {
    let body_len = caller.slots().len() - caller.arg_area_len();
    let start = caller.arg_area().start;
    match result {
        NativeResult::Word(value) => {
            stack[word_of(fp, start, body_len)] = Word::Slot(value);
        },
        NativeResult::Tag { tag, payload } => {
            debug_assert!(caller.arg_area_len() >= 2, "width pre-checked");
            stack[word_of(fp, start, body_len)] = Word::Slot(Value::Tag(tag));
            stack[word_of(fp, start + 1, body_len)] = Word::Slot(payload);
        },
    }
}

/// Reads the i64 the caller staged on overlay word `index` for a control
/// native — the same staged-type discipline `invoke_native` applies to
/// ordinary natives.
fn control_i64(
    stack: &Segment,
    fp: usize,
    caller: &FrameLayout,
    index: usize,
) -> Result<i64, Trap> {
    let body_len = caller.slots().len() - caller.arg_area_len();
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

/// The pre-invocation width check for a native call: the callee's result
/// must fit the overlay area that receives it. For a CALL that is the
/// caller's staging area; for a CALLT the span lands on the ORIGINAL
/// caller's overlay words (the tail frame's slots alias them), so the
/// frame below supplies the area. Checked before the native runs — a
/// world-effect side effect must not happen only for the call to trap
/// on the result width.
fn check_native_overlay(area: usize, nf: &NativeFunction) -> Result<(), Trap> {
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
fn check_native_call_site(caller: &FrameLayout, nf: &NativeFunction) -> Result<(), Trap> {
    let area = caller.arg_area_len();
    if nf.params.len() > area {
        return Err(Trap::CallLayout(SlideError::StagingTooSmall {
            available: area,
            needed: nf.params.len(),
        }));
    }
    check_native_overlay(area, nf)
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
/// entry functions. Every VM owns its heap object store, output sink, and
/// the single-threaded fiber pool [E2] (slot 0 is the root a `run` entry
/// drives), and installs the builtin core native module
/// ([`crate::native::BUILTIN_MODULE`]) into the registry at construction;
/// the std platform native module ([`crate::native::STD_PLATFORM_MODULE`])
/// is the host's to install — a bare VM carries no world surface.
#[derive(Debug)]
pub struct Vm {
    registry: ModuleRegistry,
    objects: Objects,
    out: OutputSink,
    /// The running fiber's segment and call chain, swapped in from its
    /// pool slot; `ctx_live` tracks whether they are here (true) or
    /// stashed in the slot (false).
    stack: Segment,
    callinfo: Vec<CallInfo>,
    ctx_live: bool,
    pool: Vec<Fiber>,
    current: FiberId,
    /// The nursery pool [E2]: one slot per open-or-consumed nursery, Dead
    /// slots reuse like fiber slots.
    nurseries: Vec<Nursery>,
    /// The live root nursery bare host spawns route into [E2]; `None` or a
    /// consumed id = the host itself is the supervisor.
    root_nursery: Option<NurseryId>,
    limit: usize,
    trap_pc: Option<CodePc>,
}

impl Vm {
    /// A VM with the default stack limit.
    #[must_use]
    pub fn new(registry: ModuleRegistry) -> Self {
        Self::with_limit(registry, DEFAULT_STACK_LIMIT)
    }

    /// A VM with an explicit value-stack word limit. Installs the builtin
    /// core native module only (a same-named prior native table is
    /// replaced); the std platform natives are the host's explicit choice
    /// (a library embedder gets no world surface by default).
    #[must_use]
    pub fn with_limit(mut registry: ModuleRegistry, limit: usize) -> Self {
        registry.install_native(
            crate::native::BUILTIN_MODULE,
            crate::native::builtin_module(),
        );
        Self {
            registry,
            objects: Objects::new(),
            out: OutputSink::default(),
            stack: Segment::new(),
            callinfo: Vec::new(),
            ctx_live: false,
            pool: vec![Fiber::empty()],
            current: FIBER_ROOT,
            nurseries: Vec::new(),
            root_nursery: None,
            limit,
            trap_pc: None,
        }
    }

    /// Interns a string into the heap and hands back its reference word:
    /// the host-facing way to stage string arguments into `run` (and the
    /// discipline KSTR follows for pool constants). The claim is one
    /// reference count; a host that abandons a staged word never decs
    /// it, so host-held references keep their objects alive.
    #[must_use = "the reference is the handle; the count is the host's to drop"]
    pub fn intern_str(&mut self, text: &str) -> Value {
        Value::Ref(self.objects.intern(text))
    }

    /// Stages a closure value for the host (the F2 capturing-lambda
    /// object): one heap object behind one `Ref` word, the header FuncRef
    /// word plus the moved-in capture block. The host-held reference owns
    /// the object's birth count; the module-code construction path (and
    /// with it the value's own RC traffic) waits on lambda-value lowering.
    #[must_use = "the reference is the handle; the count is the host's to drop"]
    pub fn alloc_closure(&mut self, func: FuncRef, captures: Vec<Value>) -> Value {
        Value::Ref(self.objects.alloc_closure(func, captures))
    }

    /// Switches the output sink to an in-memory buffer: print/println
    /// write there until the next [`Vm::run`] or forever. Callers read the
    /// bytes with [`Vm::captured_output`].
    pub fn capture_output(&mut self) {
        self.out = OutputSink::Captured(Vec::new());
    }

    /// The bytes captured so far ([`Vm::capture_output`]).
    #[must_use]
    pub fn captured_output(&self) -> &[u8] {
        match &self.out {
            OutputSink::Captured(buf) => buf,
            OutputSink::Stdout => &[],
        }
    }

    /// The heap object store: hosts and tests inspect run results (string
    /// content, list elements) by the `Ref` word a run returned.
    #[must_use]
    pub fn objects(&self) -> &Objects {
        &self.objects
    }

    /// Bounds every blocking net native's wait (connect, read, write):
    /// the default is 3 seconds; a hostile host yields `net_err`, never a
    /// hung interpreter.
    pub fn set_net_timeout(&mut self, timeout: std::time::Duration) {
        self.objects.set_net_timeout(timeout);
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

    /// Current value-stack word count: the current fiber's segment length.
    #[must_use]
    pub fn stack_len(&self) -> usize {
        self.pool[self.current.index()].segment.len()
    }

    /// High-water mark of the ROOT fiber's segment over the last run.
    /// Constant across unbounded tail recursion is the tail-call
    /// acceptance criterion [H2, per fiber].
    #[must_use]
    pub fn max_stack_len(&self) -> usize {
        self.pool[FIBER_ROOT.index()].max_len
    }

    // -- The fiber substrate [E2] --------------------------------------
    //
    // Pool ownership invariant: the running fiber's segment and call chain
    // live in the VM's context fields while `ctx_live` holds; every other
    // time (and for every other fiber, always) they live in the pool slot.
    // A switch is two ownership swaps — no word is ever copied.

    /// Spawns a fiber bare [E2]: with a live root nursery
    /// ([`Vm::open_root_nursery`]) the fiber is born its child; without one
    /// the host itself is the supervisor and the fiber detaches (no
    /// child-of link — the substrate regime the fiber fixtures pin). The
    /// in-language surface rejects bare spawn outside any nursery block;
    /// that check lands with the checker/lowerer rounds.
    ///
    /// # Errors
    ///
    /// The entry checks of [`Vm::run`] verbatim: unknown function, arity,
    /// argument types, entry frame versus the stack limit.
    pub fn spawn_fiber(
        &mut self,
        module: &str,
        func: &str,
        args: &[Value],
    ) -> Result<FiberId, Trap> {
        let nursery = self.live_root_nursery();
        self.spawn_into(nursery, module, func, args)
    }

    /// Spawns a fiber into an open nursery [E2]: the child-of link is set
    /// and the nursery records the child in spawn order.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownCallee`] with the virtual module `nursery` when
    /// `nursery` names no open nursery; the entry checks of [`Vm::run`]
    /// verbatim otherwise.
    pub fn spawn_fiber_in(
        &mut self,
        nursery: NurseryId,
        module: &str,
        func: &str,
        args: &[Value],
    ) -> Result<FiberId, Trap> {
        self.nursery_open(nursery)?;
        self.spawn_into(Some(nursery), module, func, args)
    }

    /// The spawn core: one pool entry with a fresh entry frame, born ready
    /// and never started, plus the child-of bookkeeping. The first switch
    /// into it (a `fiber_resume` call or [`Vm::resume_fiber`]) runs its
    /// entry function; the pool slot of a dead fiber is reused.
    fn spawn_into(
        &mut self,
        nursery: Option<NurseryId>,
        module: &str,
        func: &str,
        args: &[Value],
    ) -> Result<FiberId, Trap> {
        let (version, func_id) = self.resolve_entry(module, func, args)?;
        let frame_size = version.functions()[func_id as usize].layout.frame_size() as usize;
        // born into the pool: reuse a dead slot above the reserved root,
        // or grow the pool by one
        let slot = match self
            .pool
            .iter()
            .skip(1)
            .position(|f| f.state == FiberState::Dead)
        {
            Some(at) => at + 1,
            None => self.pool.len(),
        };
        if slot == self.pool.len() {
            self.pool.push(Fiber::empty());
        }
        let fiber = &mut self.pool[slot];
        fiber.segment.truncate(0); // reuse keeps the chunks: spawn stays cheap
        Self::push_entry_frame(fiber, &version, func_id, args, frame_size);
        fiber.resume = Some(ResumePoint {
            version,
            func: func_id,
            pc: CodePc(0),
        });
        fiber.state = FiberState::Ready;
        fiber.resumer = None;
        fiber.nursery = nursery;
        let id = FiberId(u32::try_from(slot).expect("pool size fits u32 ids"));
        if let Some(n) = nursery {
            self.nurseries[n.index()].push_child(id);
        }
        Ok(id)
    }

    /// Drives one fiber slice from the host: enters `fiber` (Ready or
    /// parked), delivering `payload` as the answer of the call it parked at
    /// (a Ready fiber drops the payload), and runs until that fiber next
    /// parks with the host as resumer (the payload it yielded, or the value
    /// of a fiber it resumed completing) or completes (its final value). A
    /// trapped drive leaves the fiber interrupted (not resumable) with its
    /// frames kept for [`Vm::backtrace`].
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownCallee`] with module `fiber` when `fiber` is absent
    /// or not parked at a switch point; the execution traps otherwise.
    pub fn resume_fiber(&mut self, fiber: FiberId, payload: Value) -> Result<Value, Trap> {
        if !self.pool.get(fiber.index()).is_some_and(Fiber::resumable) {
            return Err(Trap::UnknownCallee {
                module: "fiber".into(),
                func: fiber.0,
            });
        }
        self.pool[fiber.index()].resumer = None; // the host resumes
        let (active, pc) = self.switch_in(fiber, payload);
        let outcome = self.execute(active, pc, usize::MAX);
        self.host_release();
        match outcome {
            Ok(DriveOutcome::Died(v) | DriveOutcome::Yielded(v)) => Ok(v),
            // a host-level drive passes no boundary, so no reentry frame
            // exists to return through
            Ok(DriveOutcome::Frame(_)) => unreachable!("a host drive answers no reentry frame"),
            Err((trap, at)) => {
                self.trap_pc = Some(at);
                // a nursery child dying trapped under a host drive: ledger
                // it and raise the cancel flag [E2 first failure]; the
                // fiber itself stays interrupted (frames kept). The blame
                // follows the fiber the drive ended on — the drive may have
                // switched away from `fiber` (it parked at its own resume
                // call) before the trap struck
                self.trap_child(self.current, &trap);
                Err(trap)
            },
        }
    }

    /// A fiber's lifecycle state, for debug and test assertions.
    #[must_use]
    pub fn fiber_state(&self, fiber: FiberId) -> Option<FiberState> {
        self.pool.get(fiber.index()).map(|f| f.state)
    }

    /// High-water mark of a fiber's segment: the per-fiber TCO criterion —
    /// a tail-looping fiber's mark never rises past its frame size [H2].
    #[must_use]
    pub fn fiber_max_len(&self, fiber: FiberId) -> Option<usize> {
        self.pool.get(fiber.index()).map(|f| f.max_len)
    }

    /// How many chunks a fiber's segment owns — the chunk-append growth
    /// witness.
    #[must_use]
    pub fn fiber_chunk_count(&self, fiber: FiberId) -> Option<usize> {
        self.pool
            .get(fiber.index())
            .map(|f| f.segment.chunk_count())
    }

    /// The address of word `index` in a fiber's segment — any allocated
    /// word, live or truncated-over: stable across later growth (chunks
    /// never move) and across switch round-trips (a switch copies no
    /// words) [H2, E2].
    #[must_use]
    pub fn fiber_word_addr(&self, fiber: FiberId, index: usize) -> Option<usize> {
        self.pool
            .get(fiber.index())
            .filter(|f| index < f.segment.capacity())
            .map(|f| f.segment.word_addr(index))
    }

    /// The frame base of a fiber's innermost live frame.
    #[must_use]
    pub fn fiber_top_fp(&self, fiber: FiberId) -> Option<usize> {
        self.pool
            .get(fiber.index())
            .and_then(|f| f.callinfo.last().map(|ci| ci.fp))
    }

    /// The nursery a fiber was spawned into (the child-of link [E2]);
    /// `None` = the root fiber or a detached spawn.
    #[must_use]
    pub fn fiber_nursery(&self, fiber: FiberId) -> Option<NurseryId> {
        self.pool.get(fiber.index()).and_then(|f| f.nursery)
    }

    // -- Nurseries [E2, E5] ---------------------------------------------
    //
    // Determinism: spawn order drives everything observable — the join and
    // teardown drivers enter children in spawn order, one to death at a
    // time, and outcomes report in spawn order. A host-visible yield under
    // a drive is answered with the bare PLATFORM_NONE bounce tag (the
    // run-entry convention); a child's own resumer links stay live.

    /// Opens (or returns the live) root nursery [E2]: the explicit root a
    /// bare host spawn routes into. With none live, the host itself is the
    /// supervisor and bare spawns detach.
    pub fn open_root_nursery(&mut self) -> NurseryId {
        if let Some(id) = self.live_root_nursery() {
            return id;
        }
        let slot = match self
            .nurseries
            .iter()
            .position(|n| n.state == NurseryState::Dead)
        {
            Some(at) => at,
            None => self.nurseries.len(),
        };
        if slot == self.nurseries.len() {
            self.nurseries.push(Nursery::open());
        } else {
            self.nurseries[slot] = Nursery::open();
        }
        let id = NurseryId(u32::try_from(slot).expect("nursery pool fits u32 ids"));
        self.root_nursery = Some(id);
        id
    }

    /// The root nursery while it is open.
    fn live_root_nursery(&self) -> Option<NurseryId> {
        self.root_nursery.filter(|&n| {
            self.nurseries
                .get(n.index())
                .is_some_and(|nr| nr.state == NurseryState::Open)
        })
    }

    /// Whether `nursery` names an open nursery.
    fn nursery_open(&self, nursery: NurseryId) -> Result<(), Trap> {
        if self
            .nurseries
            .get(nursery.index())
            .is_some_and(|n| n.state == NurseryState::Open)
        {
            Ok(())
        } else {
            Err(Trap::UnknownCallee {
                module: "nursery".into(),
                func: nursery.0,
            })
        }
    }

    /// Joins a nursery [E2]: drives every child to death in spawn order —
    /// a child dying trapped raises the cancel flag for the rest [E5] —
    /// then reports the boundary result and consumes the nursery.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownCallee`] with the virtual module `nursery` when
    /// `nursery` names no open nursery (absent, or already consumed).
    pub fn join_nursery(&mut self, nursery: NurseryId) -> Result<JoinOutcome, Trap> {
        self.nursery_open(nursery)?;
        let outcomes = self.run_nursery_to_death(nursery, false);
        Ok(
            // only an all-Completed record joins; a synthesized Cancelled
            // with no Trapped sibling (an interrupted drive) is a boundary
            // failure the same way a trap is
            if outcomes
                .iter()
                .all(|o| matches!(o, ChildOutcome::Completed(_)))
            {
                JoinOutcome::Joined(
                    outcomes
                        .into_iter()
                        .map(|o| match o {
                            ChildOutcome::Completed(v) => v,
                            // the all-Completed test took every other shape
                            // to the Failed branch
                            ChildOutcome::Cancelled | ChildOutcome::Trapped(_) => {
                                unreachable!("a joined child completed")
                            },
                        })
                        .collect(),
                )
            } else {
                JoinOutcome::Failed(outcomes)
            },
        )
    }

    /// Tears a nursery down [E5]: raises the cancel flag, drives every
    /// live child to death (straight-line work lands, looping work stops
    /// at its next backward branch), then consumes the nursery and reports
    /// the per-child outcomes. The owner-death panic path will call this;
    /// the per-child death walk is where the future deinit pass hooks in.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownCallee`] with the virtual module `nursery` when
    /// `nursery` names no open nursery.
    pub fn teardown_nursery(&mut self, nursery: NurseryId) -> Result<Vec<ChildOutcome>, Trap> {
        self.nursery_open(nursery)?;
        Ok(self.run_nursery_to_death(nursery, true))
    }

    /// A nursery's lifecycle state, for debug and test assertions.
    #[must_use]
    pub fn nursery_state(&self, nursery: NurseryId) -> Option<NurseryState> {
        self.nurseries.get(nursery.index()).map(|n| n.state)
    }

    /// Whether a nursery's cancel flag is raised, for debug and test
    /// assertions.
    #[must_use]
    pub fn nursery_cancelled(&self, nursery: NurseryId) -> Option<bool> {
        self.nurseries.get(nursery.index()).map(|n| n.cancelled)
    }

    /// The join/teardown driver: enters each child in spawn order and
    /// drives it to death before the next (sequential — the deterministic
    /// choice). Every death path ledgers its outcome into the nursery
    /// whoever ran it, so this loop only drives and collects. A trap
    /// escaping a drive is attributed to the fiber the drive ended on (a
    /// child parked at its own resume call while its resumee traps is
    /// innocent: it is bounced back in to land); a trapped child ledgers
    /// `Trapped` and raises the flag, and the walk that marks such an
    /// interrupted child dead below is the future deinit pass's hook
    /// point [E5: teardown walks frames].
    fn run_nursery_to_death(
        &mut self,
        nursery: NurseryId,
        cancel_first: bool,
    ) -> Vec<ChildOutcome> {
        if cancel_first {
            self.nurseries[nursery.index()].cancelled = true;
        }
        let children = self.nurseries[nursery.index()].children.clone();
        for child in children {
            // a reused slot belongs to another nursery: the child died and
            // was ledgered before its slot was recycled
            if self.pool[child.index()].nursery != Some(nursery) {
                continue;
            }
            while self.pool[child.index()].state != FiberState::Dead {
                if !self.pool[child.index()].resumable() {
                    break; // interrupted mid-trap (already ledgered)
                }
                let (active, pc) = self.switch_in(child, Value::Tag(PLATFORM_NONE));
                match self.execute(active, pc, usize::MAX) {
                    Ok(DriveOutcome::Died(_) | DriveOutcome::Yielded(_)) => {},
                    Ok(DriveOutcome::Frame(_)) => {
                        unreachable!("a host drive answers no reentry frame")
                    },
                    Err((trap, _at)) => {
                        // the trap escaped the drive on whichever fiber was
                        // running when it struck — a child that resumed
                        // another fiber parks at its resume call while the
                        // resumee traps, and that parked child is innocent.
                        // Blame the fiber the drive ended on (like `run`),
                        // then let the loop re-validate this child: itself
                        // trapped → interrupted (already ledgered, the guard
                        // breaks); parked → bounced back in to land
                        self.host_release();
                        self.trap_child(self.current, &trap);
                    },
                }
            }
            // the child's death walk completes here: a trapped child keeps
            // its frames until now, then its slot is released
            let fiber = &mut self.pool[child.index()];
            if fiber.state != FiberState::Dead {
                fiber.state = FiberState::Dead;
                fiber.resume = None;
            }
        }
        self.host_release();
        let slot = &mut self.nurseries[nursery.index()];
        slot.state = NurseryState::Dead;
        slot.outcomes
            .drain(..)
            // a child cut down before any ledgered death (an interrupted
            // drive, an exotic switch tangle) reports as a boundary
            // cancellation — a structured outcome, never a panic
            .map(|o| o.unwrap_or(ChildOutcome::Cancelled))
            .collect()
    }

    /// Ledgers a fiber's death into its nursery, if it has one.
    fn record_child_outcome(&mut self, fiber: FiberId, outcome: ChildOutcome) {
        if let Some(n) = self.pool[fiber.index()].nursery {
            self.nurseries[n.index()].record(fiber, outcome);
        }
    }

    /// A fiber dying trapped under any driver: ledger `Trapped` and raise
    /// its nursery's cancel flag [E2 first failure]. The fiber's own state
    /// is the caller's business (interrupted under interactive drives,
    /// marked dead by the nursery walk).
    fn trap_child(&mut self, fiber: FiberId, trap: &Trap) {
        if let Some(n) = self.pool[fiber.index()].nursery {
            let nursery = &mut self.nurseries[n.index()];
            nursery.record(fiber, ChildOutcome::Trapped(trap.clone()));
            nursery.cancelled = true;
        }
    }

    /// Whether the running fiber's nursery has its cancel flag raised
    /// [E5]: the preemption check. Only nurseries with live children are
    /// ever cancelled, so a stale id cannot fire here.
    fn cancel_requested(&self) -> bool {
        self.pool[self.current.index()]
            .nursery
            .is_some_and(|n| self.nurseries[n.index()].cancelled)
    }

    /// Resolves and checks a run/spawn entry: unknown function, arity,
    /// argument types, entry frame versus the limit.
    fn resolve_entry(
        &self,
        module: &str,
        func: &str,
        args: &[Value],
    ) -> Result<(Arc<ModuleVersion>, u32), Trap> {
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
            // a Ref word against a Fn parameter is the callable-defaulted
            // slot (F2): the host stages a closure word into a fn-typed
            // entry parameter exactly like a module call
            if arg.slot_type() != ty
                && ty != SlotType::Raw
                && !(arg.slot_type() == SlotType::Ref && ty == SlotType::Fn)
            {
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
        Ok((version, func_id))
    }

    /// Builds a fiber's entry frame in its own segment: metadata word,
    /// arguments, zero-filled to the frame span. The caller has already
    /// reset the segment (`clear` for a fresh root run, `truncate(0)` to
    /// reuse a dead fiber's chunks).
    fn push_entry_frame(
        fiber: &mut Fiber,
        version: &Arc<ModuleVersion>,
        func_id: u32,
        args: &[Value],
        frame_size: usize,
    ) {
        // Root frame: the return-pc sentinel is never read, because a RET
        // from the entry frame completes the fiber instead of returning.
        fiber.segment.push(Word::Meta(FrameMetadata {
            return_pc: CodePc(u32::MAX),
            frame_size: u32::try_from(frame_size).expect("frame span fits u32"),
            tail: TailChain::default(),
        }));
        for &arg in args {
            fiber.segment.push(Word::Slot(arg));
        }
        fiber.segment.grow_to(frame_size);
        fiber.callinfo.clear();
        fiber.callinfo.push(CallInfo {
            fp: 0,
            module: Arc::clone(version),
            func: func_id,
        });
        fiber.max_len = frame_size;
    }

    /// Stashes the running fiber's context into its pool slot, ending a
    /// drive; the pool is then the single home of every fiber's state.
    fn host_release(&mut self) {
        if !self.ctx_live {
            return;
        }
        let fiber = &mut self.pool[self.current.index()];
        std::mem::swap(&mut self.stack, &mut fiber.segment);
        std::mem::swap(&mut self.callinfo, &mut fiber.callinfo);
        self.ctx_live = false;
    }

    /// Parks the running fiber at `resume_pc` (the instruction after the
    /// call that is switching): only the resume point is recorded; segment
    /// and call chain are untouched.
    fn park_current(&mut self, active: &Active, resume_pc: CodePc) {
        let fiber = &mut self.pool[self.current.index()];
        fiber.resume = Some(ResumePoint {
            version: Arc::clone(&active.version),
            func: active.func,
            pc: resume_pc,
        });
        fiber.state = FiberState::Suspended;
    }

    /// Enters `target` with `payload` [E2: switch is O(1)]: stash the
    /// outgoing fiber's context, swap the target's in (buffer ownership
    /// moves, no word is copied), and deliver `payload` onto the call the
    /// target parked at (a Ready fiber drops it — first entry runs the
    /// spawn arguments). The caller has validated the target. Entering
    /// the fiber the last drive ended on is the host's O(1) round-trip:
    /// its context was stashed at host release and swaps back in.
    fn switch_in(&mut self, target: FiberId, payload: Value) -> (Active, CodePc) {
        if target != self.current {
            self.host_release(); // stash the outgoing fiber's context if live
        }
        if target != self.current || !self.ctx_live {
            let fiber = &mut self.pool[target.index()];
            std::mem::swap(&mut self.stack, &mut fiber.segment);
            std::mem::swap(&mut self.callinfo, &mut fiber.callinfo);
            self.current = target;
            self.ctx_live = true;
        }
        let fiber = &mut self.pool[target.index()];
        let resume = fiber
            .resume
            .take()
            .expect("a validated switch target carries its continuation");
        let parked = fiber.state == FiberState::Suspended;
        fiber.state = FiberState::Running;
        if parked {
            let f = &resume.version.functions()[resume.func as usize];
            let fp = self.callinfo[self.callinfo.len() - 1].fp;
            overlay_native(NativeResult::Word(payload), &mut self.stack, &f.layout, fp);
        }
        (
            Active {
                version: resume.version,
                func: resume.func,
            },
            resume.pc,
        )
    }

    /// Ends the running fiber [E2]: ledgers `outcome` into its nursery,
    /// marks it dead, and switches to its resumer delivering `value` to the
    /// call the resumer parked at. `None` = the host takes the delivery —
    /// it was the resumer, or the resumer died while this fiber was parked
    /// (the leftover pattern; the join/teardown drivers must survive it).
    fn die_current(&mut self, value: Value, outcome: ChildOutcome) -> Option<(Active, CodePc)> {
        self.record_child_outcome(self.current, outcome);
        let resumer = self.pool[self.current.index()].resumer;
        let fiber = &mut self.pool[self.current.index()];
        fiber.state = FiberState::Dead;
        fiber.resume = None;
        resumer
            .filter(|&r| self.pool[r.index()].resumable())
            .map(|target| self.switch_in(target, value))
    }

    /// The running fiber completed at its entry frame with `value`.
    fn complete_current(&mut self, value: Value) -> Option<(Active, CodePc)> {
        self.die_current(value, ChildOutcome::Completed(value))
    }

    /// The running fiber observed its nursery's cancel flag at a taken
    /// backward branch [E5]: it stops here — no instruction of the loop
    /// body it was re-entering runs — and its resumer receives the
    /// `PLATFORM_CANCELLED` sentinel.
    fn cancel_current(&mut self) -> Option<(Active, CodePc)> {
        self.die_current(Value::Tag(PLATFORM_CANCELLED), ChildOutcome::Cancelled)
    }

    /// The switch behind a control native's `CALL` site (the fiber pair,
    /// and the nursery join's park-drive-redeliver). The staged words are
    /// already width- and type-checked by the caller of this helper.
    fn control_switch(
        &mut self,
        control: ControlNative,
        active: &Active,
        pc: u32,
        fp: usize,
        caller: &FrameLayout,
    ) -> Result<SwitchOutcome, Trap> {
        match control {
            ControlNative::Yield => {
                let payload = control_i64(&self.stack, fp, caller, 0)?;
                self.park_current(active, CodePc(pc + 1));
                let resumer = self.pool[self.current.index()].resumer;
                // the resumer is the host, a fiber parked at its own resume
                // call, or dead (the leftover pattern: it died while this
                // fiber was parked) — a dead resumer hands the yield to the
                // host, never a switch into a corpse
                match resumer.filter(|&r| self.pool[r.index()].resumable()) {
                    None => Ok(SwitchOutcome::Host(DriveOutcome::Yielded(Value::I64(
                        payload,
                    )))),
                    Some(target) => {
                        let (a, at) = self.switch_in(target, Value::I64(payload));
                        Ok(SwitchOutcome::Entered(a, at))
                    },
                }
            },
            ControlNative::Resume => {
                let id = control_i64(&self.stack, fp, caller, 0)?;
                let payload = control_i64(&self.stack, fp, caller, 1)?;
                let target = u32::try_from(id).ok().map(FiberId).filter(|t| {
                    *t != self.current && self.pool.get(t.index()).is_some_and(Fiber::resumable)
                });
                let Some(target) = target else {
                    // no switch: the checked-read bounce answer [E2 debug surface]
                    overlay_native(
                        NativeResult::Word(Value::Tag(PLATFORM_NONE)),
                        &mut self.stack,
                        caller,
                        fp,
                    );
                    return Ok(SwitchOutcome::Bounced);
                };
                self.park_current(active, CodePc(pc + 1));
                self.pool[target.index()].resumer = Some(self.current);
                let (a, at) = self.switch_in(target, Value::I64(payload));
                Ok(SwitchOutcome::Entered(a, at))
            },
            ControlNative::NurseryJoin => {
                let id = control_i64(&self.stack, fp, caller, 0)?;
                let nursery = u32::try_from(id).ok().map(NurseryId).filter(|&n| {
                    self.nurseries
                        .get(n.index())
                        .is_some_and(|nr| nr.state == NurseryState::Open)
                });
                let Some(nursery) = nursery else {
                    // absent or consumed: the checked-read bounce answer
                    overlay_native(
                        NativeResult::Word(Value::Tag(PLATFORM_NONE)),
                        &mut self.stack,
                        caller,
                        fp,
                    );
                    return Ok(SwitchOutcome::Bounced);
                };
                if self.pool[self.current.index()].nursery == Some(nursery) {
                    // a child joining its own nursery would deadlock the
                    // single-threaded driver on itself [E2]
                    return Err(Trap::UnknownCallee {
                        module: "nursery".into(),
                        func: nursery.0,
                    });
                }
                self.park_current(active, CodePc(pc + 1));
                let joiner = self.current;
                let outcomes = self.run_nursery_to_death(nursery, false);
                let failed = outcomes
                    .iter()
                    .filter(|o| matches!(o, ChildOutcome::Trapped(_)))
                    .count();
                let count = i64::try_from(failed).expect("child count fits i64");
                let (a, at) = self.switch_in(joiner, Value::I64(count));
                Ok(SwitchOutcome::Entered(a, at))
            },
        }
    }

    /// Runs an entry function to its `HALT` or entry-frame `RET` and reports
    /// the resulting value: the root fiber's drive to completion [E2]. The
    /// root's execution state resets per run (other pool fibers keep their
    /// state); on a `Trap` the state is kept for [`Vm::backtrace`]. A root
    /// that yields to the host (it has no in-language resumer) is re-entered
    /// with the bare `PLATFORM_NONE` tag as the yield's answer — a `run`
    /// entry drives to completion; interactive stepping is
    /// [`Vm::resume_fiber`].
    pub fn run(&mut self, module: &str, func: &str, args: &[Value]) -> Result<Value, Trap> {
        self.trap_pc = None;
        let (version, func_id) = self.resolve_entry(module, func, args)?;
        let frame_size = version.functions()[func_id as usize].layout.frame_size() as usize;
        self.host_release(); // stash any interrupted drive's context
        let root = &mut self.pool[FIBER_ROOT.index()];
        root.segment.clear();
        Self::push_entry_frame(root, &version, func_id, args, frame_size);
        root.resume = None;
        root.resumer = None;
        root.state = FiberState::Running;
        std::mem::swap(&mut self.stack, &mut self.pool[FIBER_ROOT.index()].segment);
        std::mem::swap(
            &mut self.callinfo,
            &mut self.pool[FIBER_ROOT.index()].callinfo,
        );
        self.current = FIBER_ROOT;
        self.ctx_live = true;
        let mut active = Active {
            version,
            func: func_id,
        };
        let mut pc = CodePc(0);
        loop {
            match self.execute(active, pc, usize::MAX) {
                Ok(DriveOutcome::Died(value)) => {
                    self.host_release();
                    return Ok(value);
                },
                Ok(DriveOutcome::Yielded(_)) => {
                    let (a, p) = self.switch_in(FIBER_ROOT, Value::Tag(PLATFORM_NONE));
                    active = a;
                    pc = p;
                },
                Ok(DriveOutcome::Frame(_)) => {
                    unreachable!("a host drive answers no reentry frame")
                },
                Err((trap, at)) => {
                    self.trap_pc = Some(at);
                    self.host_release();
                    // the drive may have ended on a nursery child entered
                    // in-language: ledger it and raise its nursery's flag
                    self.trap_child(self.current, &trap);
                    return Err(trap);
                },
            }
        }
    }

    /// Whether the live call chain still carries this drive's reentry
    /// frame (the sentinel-metadata frame [`Vm::call_hosted`] pushed at
    /// `boundary - 1`): the honesty condition for effects that cannot
    /// suspend the Rust-side native loop. A fiber switched in mid-drive
    /// never carries one — a resumable fiber parks only at switch
    /// points, never mid-native — so the sentinel test cannot
    /// false-positive across a switch.
    fn reentry_live(&self, boundary: usize) -> bool {
        if boundary == usize::MAX || self.callinfo.len() < boundary {
            return false;
        }
        matches!(
            self.stack[self.callinfo[boundary - 1].fp],
            Word::Meta(m) if m.return_pc.0 == u32::MAX
        )
    }

    /// Drives one reentrant call of a planned function from native code
    /// — the lambda-taking container combinators' per-element
    /// invocation: a synthetic reentry frame (the entry-frame sentinel
    /// return pc marks it) is pushed on the CURRENT fiber's chain at
    /// `boundary` and driven to its return, which answers
    /// [`DriveOutcome::Frame`]. The callee word is callable-defaulted
    /// (F2): a scalar FuncRef names the function directly, a `Ref` word
    /// names a closure object whose capture block lands in the callee's
    /// leading local slots exactly like a CALLS copy-in (one acquired
    /// count per `Ref` capture, balanced by the callee body's own
    /// placement). Each Ref argument carries one count into
    /// the frame (the local-call hand-off: the callee's placement owns
    /// its params), and the returned word transfers its count to the
    /// caller. A drive that ends any other way —
    /// the lambda HALTs under nesting, a fiber switch's resumer chain
    /// never returns, a nursery cancel kills the fiber, a yield parks
    /// at the host — refuses with [`Trap::ReentrancyRefused`]: a
    /// Rust-side loop cannot suspend, and the chain is already lost or
    /// parked. On every non-returning outcome the fiber that hosted the
    /// reentry frame is interrupted (never resumable again — a parked
    /// resume would run its lambda into the sentinel return pc). A
    /// trapped lambda propagates its trap with the frames kept for
    /// [`Vm::backtrace`].
    pub(crate) fn call_hosted(&mut self, callee: Value, args: &[Value]) -> Result<Value, Trap> {
        let nargs = u8::try_from(args.len()).expect("hosted-call arities are tiny");
        let (funcref, captures) = match callee {
            Value::Fn(funcref) => (funcref, None),
            Value::Ref(obj) => {
                let (header, block) = self.objects.closure_at(obj)?;
                (header, Some(block.to_vec()))
            },
            other => {
                return Err(Trap::SlotTypeMismatch {
                    slot: 0,
                    expected: SlotType::Fn,
                    actual: other.slot_type(),
                })
            },
        };
        let (version, func_id) = StackState::resolve_func_word(&self.registry, funcref, nargs)?;
        let target = &version.functions()[func_id as usize];
        let frame_size = target.layout.frame_size() as usize;
        let base = self.stack.len();
        let needed = base + frame_size;
        if needed > self.limit {
            return Err(Trap::StackOverflow {
                needed,
                limit: self.limit,
            });
        }
        self.stack.push(Word::Meta(FrameMetadata {
            return_pc: CodePc(u32::MAX),
            frame_size: u32::try_from(frame_size).expect("frame span fits u32"),
            tail: TailChain::default(),
        }));
        // the local-call hand-off: each Ref argument carries one count
        // into the frame (the callee's placement treats its params as
        // owned — returning one transfers that count with the answer,
        // and a dropped one balances at the body's exit drops)
        for &arg in args {
            if let Value::Ref(id) = arg {
                self.objects.inc(id)?;
            }
        }
        for &arg in args {
            self.stack.push(Word::Slot(arg));
        }
        self.stack.grow_to(needed);
        // the closure's capture block lands in the callee's leading local
        // slots — the CALLS copy-in discipline: one acquired count per
        // Ref capture, the body's own placement balancing it, while the
        // closure object keeps its own count untouched
        let capture_body_len = target.layout.slots().len() - target.layout.arg_area_len();
        if let Some(block) = &captures {
            let end = target.layout.params_len() + block.len();
            if end > capture_body_len {
                return Err(Trap::SlotOutOfRange {
                    slot: u8::try_from(end - 1).unwrap_or(u8::MAX),
                    frame_slots: target.layout.slots().len(),
                });
            }
            for (index, &word) in block.iter().enumerate() {
                if let Value::Ref(id) = word {
                    self.objects.inc(id)?;
                }
                let slot = u8::try_from(target.layout.params_len() + index)
                    .expect("capture slot fits u8 after the range check");
                write_slot(
                    &mut self.stack,
                    base,
                    slot,
                    target.layout.slots(),
                    capture_body_len,
                    word,
                )?;
            }
        }
        self.callinfo.push(CallInfo {
            fp: base,
            module: Arc::clone(&version),
            func: func_id,
        });
        let fiber = &mut self.pool[self.current.index()];
        fiber.max_len = fiber.max_len.max(needed);
        let boundary = self.callinfo.len();
        let host = self.current;
        let active = Active {
            version: Arc::clone(&version),
            func: func_id,
        };
        let driven = match self.execute(active, CodePc(0), boundary) {
            Ok(DriveOutcome::Frame(value)) => Ok(value),
            Ok(DriveOutcome::Died(_) | DriveOutcome::Yielded(_)) => Err(Trap::ReentrancyRefused),
            Err((trap, _at)) => Err(trap),
        };
        if driven.is_err() {
            // the Rust-side loop is gone, so the sentinel reentry frame on
            // the hosting fiber's chain can never be answered — the drive
            // answered anything but its Frame. A parked host left resumable
            // would re-enter on a later resume and run its lambda straight
            // into the sentinel return pc, so interrupt it like a trapped
            // drive: frames kept (the staged-argument counts stay with
            // them, the same accounting as every trap path), re-entry
            // refused with the structured fiber refusal
            let fiber = &mut self.pool[host.index()];
            if fiber.state != FiberState::Dead {
                fiber.resume = None;
                fiber.state = FiberState::Running;
            }
        }
        driven
    }

    /// Splits the VM for a generic builtin body's pure-object work: the
    /// heap and the output sink, disjoint from every other field. The
    /// reentrant bodies (the lambda-taking combinators) keep `vm` whole
    /// and call back through [`Vm::call_hosted`].
    pub(crate) fn split(&mut self) -> (&mut Objects, &mut OutputSink) {
        (&mut self.objects, &mut self.out)
    }

    /// The dispatcher: decode-on-dispatch (see the module docs), then a
    /// dense `match` over the uop [BL-3 note: computed goto is not available
    /// on stable Rust]. A drive ends when the current fiber completes
    /// ([`DriveOutcome::Died`]) or yields to the host
    /// ([`DriveOutcome::Yielded`]); switches between fibers happen inside.
    /// `boundary` is the call-chain depth of a nested reentrant drive's
    /// synthetic reentry frame ([`Vm::call_hosted`]): a host-level drive
    /// passes `usize::MAX` and never sees [`DriveOutcome::Frame`], and a
    /// nested drive answers it when its reentry frame returns.
    // One flat match over the whole uop set is the point of a dispatch loop
    // [BL-3]; splitting arms into helpers would fragment it and add a call
    // per op in debug builds.
    #[allow(clippy::too_many_lines)]
    fn execute(
        &mut self,
        mut active: Active,
        mut pc: CodePc,
        boundary: usize,
    ) -> Result<DriveOutcome, (Trap, CodePc)> {
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
                    // the preemption point [E2, E5]: a taken backward branch
                    // (a loop back-edge, self or below) is where the running
                    // fiber observes its nursery's cancel flag. Never
                    // mid-instruction: a linear consume in flight is done.
                    if t <= pc.0 && self.cancel_requested() {
                        match self.cancel_current() {
                            Some((a, at)) => {
                                active = a;
                                pc = at;
                                continue;
                            },
                            None => return Ok(DriveOutcome::Died(Value::Tag(PLATFORM_CANCELLED))),
                        }
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
                        max_len: &mut self.pool[self.current.index()].max_len,
                    }
                };
            }
            let op = match Uop::decode(pc.0, code[idx]) {
                Ok(op) => op,
                // Major 0xE: the placed RC pair (C4). Load decode-checks
                // every word once, so a decode failure here is a
                // canonicity violation like any other; the arm moves one
                // counted heap block's count per word.
                Err(DecodeError::UnknownOpcode {
                    major: RC_MAJOR, ..
                }) => match RcOp::decode(code[idx]) {
                    Ok(RcOp::Inc { src }) => {
                        let id =
                            read_ref(&self.stack, fp, src, slots, body_len).map_err(|e| (e, pc))?;
                        self.objects.inc(id).map_err(|e| (e, pc))?;
                        pc.0 += 1;
                        continue;
                    },
                    Ok(RcOp::Dec { src }) => {
                        let id =
                            read_ref(&self.stack, fp, src, slots, body_len).map_err(|e| (e, pc))?;
                        self.objects.dec(id).map_err(|e| (e, pc))?;
                        pc.0 += 1;
                        continue;
                    },
                    Err(cause) => {
                        let cause = match cause {
                            RcDecodeError::UnknownOpcode { major, minor } => {
                                DecodeError::UnknownOpcode { major, minor }
                            },
                            RcDecodeError::ReservedField => DecodeError::ReservedField,
                        };
                        return Err((Trap::BadEncoding { pc: pc.0, cause }, pc));
                    },
                },
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
                        // A fn word (F2): the FuncRef's module name resolves
                        // through the registry id space at issue time — ids
                        // are stable across swaps, and CALLS re-resolves the
                        // word through the target's current slot at call
                        // time, so a minted word picks up a hot-reload swap
                        // like a staged one. The loader validated the
                        // reference at install, so an unresolved name is a
                        // corrupted pool, never a program fact.
                        Some(Const::FuncRef { module, func }) => {
                            let module_id = self.registry.module_id(module).ok_or((
                                Trap::UnknownCallee {
                                    module: module.clone(),
                                    func: *func,
                                },
                                pc,
                            ))?;
                            Value::Fn(FuncRef::new(module_id, *func))
                        },
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
                Uop::Kstr { dst, kidx } => {
                    let text = match pool.get(usize::from(kidx.0)) {
                        Some(Const::Str(text)) => text.as_str(),
                        Some(found) => {
                            let e = Trap::PoolKind {
                                kidx: kidx.0,
                                expected: ConstKind::Str,
                                found: found.kind(),
                            };
                            return Err((e, pc));
                        },
                        None => return Err((Trap::PoolIndex { kidx: kidx.0 }, pc)),
                    };
                    let obj = self.objects.intern(text);
                    write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Ref(obj))
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
                    // [B9]
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
                Uop::EqStr { dst, lhs, rhs } => {
                    let (x, y) = (
                        read_ref(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                        read_ref(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                    );
                    let eq = self.objects.str_at(x).map_err(|e| (e, pc))?
                        == self.objects.str_at(y).map_err(|e| (e, pc))?;
                    write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Bool(eq))
                        .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::NeqStr { dst, lhs, rhs } => {
                    let (x, y) = (
                        read_ref(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                        read_ref(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                    );
                    let neq = self.objects.str_at(x).map_err(|e| (e, pc))?
                        != self.objects.str_at(y).map_err(|e| (e, pc))?;
                    write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Bool(neq))
                        .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::StrCat { dst, lhs, rhs } => {
                    let (x, y) = (
                        read_ref(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                        read_ref(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                    );
                    let obj = self.objects.concat(x, y).map_err(|e| (e, pc))?;
                    write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Ref(obj))
                        .map_err(|e| (e, pc))?;
                    pc.0 += 1;
                },
                Uop::ClosureNew { dst, func, n } => {
                    // CLOSURE_NEW (F2): the header is the scalar FuncRef
                    // word at `func` (KCONST-minted), the capture block the
                    // n words staged at overlay positions 0..n. The object
                    // acquires one count per Ref capture as it adopts the
                    // block — the staged sources keep their own counts — and
                    // the fresh Ref lands in dst with birth count one.
                    let funcref = match read_value(&self.stack, fp, func, slots, body_len)
                        .map_err(|e| (e, pc))?
                    {
                        Value::Fn(funcref) => funcref,
                        v => {
                            return Err((
                                Trap::SlotTypeMismatch {
                                    slot: func,
                                    expected: SlotType::Fn,
                                    actual: v.slot_type(),
                                },
                                pc,
                            ))
                        },
                    };
                    let overlay_base = slots.len() - f.layout.arg_area_len();
                    let mut captures = Vec::with_capacity(usize::from(n));
                    for index in 0..usize::from(n) {
                        let slot = u8::try_from(overlay_base + index)
                            .expect("overlay words fit u8; frames cap at 256 slots");
                        captures.push(
                            read_value(&self.stack, fp, slot, slots, body_len)
                                .map_err(|e| (e, pc))?,
                        );
                    }
                    for word in &captures {
                        if let Value::Ref(id) = *word {
                            self.objects.inc(id).map_err(|e| (e, pc))?;
                        }
                    }
                    let obj = self.objects.alloc_closure(funcref, captures);
                    write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Ref(obj))
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
                    let resolved = StackState::resolve_callee(pool, &self.registry, callee, nargs)
                        .map_err(|e| (e, pc))?;
                    match resolved {
                        Callee::Uop { version, func } => {
                            let mut st = stack_state!();
                            let t = st
                                .call(version, func, &f.layout, fp, pc.0 + 1)
                                .map_err(|e| (e, pc))?;
                            active = Active {
                                version: t.version,
                                func: t.func,
                            };
                            pc = CodePc(0);
                        },
                        // A native call stays in the caller's frame: the
                        // staged words are read as arguments and the result
                        // overlays argument slot 0 exactly where a RET
                        // would put it (a fallible native's span overlays
                        // slots 0 and 1). The fiber-control natives are the
                        // exception: the dispatcher itself is the switch
                        // [E2] — their bodies never run from a CALL site.
                        Callee::Native(nf) => {
                            check_native_call_site(&f.layout, nf).map_err(|e| (e, pc))?;
                            if let Some(control) = nf.control {
                                match self
                                    .control_switch(control, &active, pc.0, fp, &f.layout)
                                    .map_err(|e| (e, pc))?
                                {
                                    SwitchOutcome::Entered(a, at) => {
                                        active = a;
                                        pc = at;
                                    },
                                    SwitchOutcome::Bounced => {
                                        pc.0 += 1;
                                    },
                                    SwitchOutcome::Host(event) => return Ok(event),
                                }
                            } else {
                                let result = invoke_native(
                                    nf,
                                    &mut self.stack,
                                    &mut self.objects,
                                    &mut self.out,
                                    &f.layout,
                                    fp,
                                )
                                .map_err(|e| (e, pc))?;
                                overlay_native(result, &mut self.stack, &f.layout, fp);
                                pc.0 += 1;
                            }
                        },
                    }
                },
                Uop::Callt { callee, nargs } => {
                    let resolved = StackState::resolve_callee(pool, &self.registry, callee, nargs)
                        .map_err(|e| (e, pc))?;
                    match resolved {
                        Callee::Uop { version, func } => {
                            let mut st = stack_state!();
                            let t = st
                                .tail_call(version, func, &f.layout, fp)
                                .map_err(|e| (e, pc))?;
                            active = Active {
                                version: t.version,
                                func: t.func,
                            };
                            pc = CodePc(0);
                        },
                        // Tail-calling a native runs it, then returns its
                        // result from the current frame: the value the
                        // native produced is the tail position's value.
                        // A span result rides the reused frame back to
                        // the ORIGINAL caller: this frame's slot 0
                        // already aliases the caller's overlay word 0
                        // (the RET below writes the tag there), and
                        // overlay word 1 is the physical word fp+2. At
                        // the root frame only the head word survives,
                        // the same root rule RETSPAN returns by.
                        Callee::Native(nf) => {
                            if self.callinfo.len() > 1 {
                                let caller = self.callinfo[self.callinfo.len() - 2].clone();
                                let caller_fn = &caller.module.functions()[caller.func as usize];
                                check_native_overlay(caller_fn.layout.arg_area_len(), nf)
                                    .map_err(|e| (e, pc))?;
                            }
                            let result = invoke_native(
                                nf,
                                &mut self.stack,
                                &mut self.objects,
                                &mut self.out,
                                &f.layout,
                                fp,
                            )
                            .map_err(|e| (e, pc))?;
                            let value = match result {
                                NativeResult::Word(value) => value,
                                NativeResult::Tag { tag, payload } => {
                                    if self.callinfo.len() > 1 {
                                        self.stack[fp + 2] = Word::Slot(payload);
                                    }
                                    Value::Tag(tag)
                                },
                            };
                            let mut st = stack_state!();
                            match st.ret(value, fp, boundary).map_err(|e| (e, pc))? {
                                RetOutcome::Root(v) => match self.complete_current(v) {
                                    Some((a, at)) => {
                                        active = a;
                                        pc = at;
                                    },
                                    None => return Ok(DriveOutcome::Died(v)),
                                },
                                RetOutcome::Boundary(v) => return Ok(DriveOutcome::Frame(v)),
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
                    }
                },
                Uop::Calls { callee_slot, nargs } => {
                    // CALLS, the callable-word call form (F2): the callee
                    // operand is a frame slot, the ISA's one dynamic-dispatch
                    // point — the callee word's identity is runtime data by
                    // construction, so the read dispatches on the word's
                    // kind: a scalar FuncRef names the function directly, a
                    // Ref word names a closure object whose header is the
                    // FuncRef and whose capture block rides along. The
                    // slot's static classification stays the boundary: a
                    // Fn slot is callable-defaulted (either word may
                    // arrive), a Ref slot holds a value-site closure.
                    // read_value bounds-checks the slot operand first.
                    let word = read_value(&self.stack, fp, callee_slot, slots, body_len)
                        .map_err(|e| (e, pc))?;
                    if !matches!(
                        slots[usize::from(callee_slot)],
                        SlotType::Fn | SlotType::Ref
                    ) {
                        return Err((
                            Trap::SlotTypeMismatch {
                                slot: callee_slot,
                                expected: SlotType::Fn,
                                actual: slots[usize::from(callee_slot)],
                            },
                            pc,
                        ));
                    }
                    let (funcref, captures) = match word {
                        Value::Fn(funcref) => (funcref, None),
                        Value::Ref(obj) => {
                            let (header, block) =
                                self.objects.closure_at(obj).map_err(|e| (e, pc))?;
                            (header, Some(block.to_vec()))
                        },
                        v => {
                            return Err((
                                Trap::SlotTypeMismatch {
                                    slot: callee_slot,
                                    expected: SlotType::Fn,
                                    actual: v.slot_type(),
                                },
                                pc,
                            ))
                        },
                    };
                    let (version, func_id) =
                        StackState::resolve_func_word(&self.registry, funcref, nargs)
                            .map_err(|e| (e, pc))?;
                    let target = &version.functions()[func_id as usize];
                    // a closure's capture block lands in the callee's frame
                    // starting at the first local slot (after the params),
                    // so the body reads a capture where a lowered lambda
                    // declares it. Each Ref capture acquires one count for
                    // the frame at the copy-in — the callee owns its copy
                    // like a borrow parameter and balances it with its own
                    // exit drops, while the closure object keeps its own
                    // count untouched. The range check runs before the
                    // frame is pushed; each write re-checks the word
                    // against the slot's declared type.
                    let capture_body_len =
                        target.layout.slots().len() - target.layout.arg_area_len();
                    if let Some(block) = &captures {
                        let end = target.layout.params_len() + block.len();
                        if end > capture_body_len {
                            return Err((
                                Trap::SlotOutOfRange {
                                    slot: u8::try_from(end - 1).unwrap_or(u8::MAX),
                                    frame_slots: target.layout.slots().len(),
                                },
                                pc,
                            ));
                        }
                    }
                    // the frame setup mirrors CALL: slide-plan validation,
                    // the raw-staged check, the callee's metadata word in
                    // the caller's call word, the args overlaid. No tail
                    // variant exists, so this is an ordinary push.
                    {
                        let mut st = stack_state!();
                        st.call(Arc::clone(&version), func_id, &f.layout, fp, pc.0 + 1)
                            .map_err(|e| (e, pc))?;
                    }
                    if let Some(block) = &captures {
                        let callee_fp = fp + 1 + f.layout.arg_area().start;
                        for (index, &word) in block.iter().enumerate() {
                            if let Value::Ref(id) = word {
                                self.objects.inc(id).map_err(|e| (e, pc))?;
                            }
                            let slot = u8::try_from(target.layout.params_len() + index)
                                .expect("capture slot fits u8 after the range check");
                            write_slot(
                                &mut self.stack,
                                callee_fp,
                                slot,
                                target.layout.slots(),
                                capture_body_len,
                                word,
                            )
                            .map_err(|e| (e, pc))?;
                        }
                    }
                    active = Active {
                        version,
                        func: func_id,
                    };
                    pc = CodePc(0);
                },
                Uop::Callg { callee, nargs } => {
                    // A generic call site [H3]: the callee is a generic
                    // builtin's ONE body, and the call's evidence words sit
                    // in the overlay after the staged arguments — their
                    // count travels with the callee, so the uop carries
                    // CALL's operand layout. The dispatch reads the
                    // evidence words and selects the body's behavior on the
                    // resolved types; monomorphic instances do not exist.
                    let found = pool
                        .get(usize::from(callee.0))
                        .ok_or((Trap::PoolIndex { kidx: callee.0 }, pc))?;
                    let Const::FuncRef { module, func } = found else {
                        return Err((
                            Trap::PoolKind {
                                kidx: callee.0,
                                expected: ConstKind::FuncRef,
                                found: found.kind(),
                            },
                            pc,
                        ));
                    };
                    if self.registry.resolve_indexed(module, *func).is_some() {
                        // a lowered generic body has no constraint list yet
                        return Err((
                            Trap::UnknownCallee {
                                module: module.clone(),
                                func: *func,
                            },
                            pc,
                        ));
                    }
                    let g = crate::native::resolve_generic(module, *func).ok_or((
                        Trap::UnknownCallee {
                            module: module.clone(),
                            func: *func,
                        },
                        pc,
                    ))?;
                    if usize::from(nargs) != g.params.len() {
                        return Err((
                            Trap::ArityMismatch {
                                expected: g.params.len(),
                                given: usize::from(nargs),
                            },
                            pc,
                        ));
                    }
                    // the evidence words share the overlay with the staged
                    // arguments, so the area must hold both plus the result
                    let area = f.layout.arg_area_len();
                    let needed = g.params.len() + usize::from(g.evidence);
                    if needed > area {
                        return Err((
                            Trap::CallLayout(SlideError::StagingTooSmall {
                                available: area,
                                needed,
                            }),
                            pc,
                        ));
                    }
                    if usize::from(g.result_words) > area {
                        return Err((
                            Trap::RetSpanTooWide {
                                span: u16::from(g.result_words),
                                caller_area: u16::try_from(area).expect("argument areas fit u16"),
                            },
                            pc,
                        ));
                    }
                    let body_len = f.layout.slots().len() - f.layout.arg_area_len();
                    let start = f.layout.arg_area().start;
                    let mut args = Vec::with_capacity(usize::from(nargs));
                    for i in 0..usize::from(nargs) {
                        let slot = u8::try_from(start + i).expect("slot index fits u8");
                        args.push(
                            read_value(&self.stack, fp, slot, slots, body_len)
                                .map_err(|e| (e, pc))?,
                        );
                    }
                    for (index, (&value, &want)) in args.iter().zip(g.params).enumerate() {
                        // a `Raw` parameter is the wildcard: the staged
                        // value words carry the call site's concrete kinds;
                        // a `Ref` word against a `Fn` parameter is the
                        // callable-defaulted slot (F2) — the combinators'
                        // staged lambda may be a closure object
                        let staged = value.slot_type();
                        if staged != want
                            && want != SlotType::Raw
                            && !(staged == SlotType::Ref && want == SlotType::Fn)
                        {
                            return Err((
                                Trap::CallLayout(SlideError::TypeMismatch {
                                    index,
                                    staged,
                                    expected: want,
                                }),
                                pc,
                            ));
                        }
                    }
                    let mut evidence = Vec::with_capacity(usize::from(g.evidence));
                    for i in 0..usize::from(g.evidence) {
                        let slot =
                            u8::try_from(start + usize::from(nargs) + i).expect("slot fits u8");
                        evidence.push(
                            read_value(&self.stack, fp, slot, slots, body_len)
                                .map_err(|e| (e, pc))?,
                        );
                    }
                    // the body takes the whole VM: the pure bodies split
                    // off the heap and sink themselves, and the
                    // lambda-taking combinators reenter the dispatcher
                    // through `call_hosted`
                    let result = (g.func)(self, &evidence, &args).map_err(|e| (e, pc))?;
                    overlay_native(result, &mut self.stack, &f.layout, fp);
                    pc.0 += 1;
                },
                Uop::Ret { src } => {
                    let value =
                        read_value(&self.stack, fp, src, slots, body_len).map_err(|e| (e, pc))?;
                    let mut st = stack_state!();
                    match st.ret(value, fp, boundary).map_err(|e| (e, pc))? {
                        RetOutcome::Root(v) => match self.complete_current(v) {
                            Some((a, at)) => {
                                active = a;
                                pc = at;
                            },
                            None => return Ok(DriveOutcome::Died(v)),
                        },
                        RetOutcome::Boundary(v) => return Ok(DriveOutcome::Frame(v)),
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
                Uop::RetSpan { src, n } => {
                    // the span must fit inside this frame's own slots (a
                    // load-time fact, re-checked defensively)
                    if usize::from(src) + usize::from(n) > frame_slots {
                        return Err((
                            Trap::SlotOutOfRange {
                                slot: src,
                                frame_slots,
                            },
                            pc,
                        ));
                    }
                    // the caller's overlay area must be wide enough to
                    // receive the whole span (cross-function, so runtime)
                    if self.callinfo.len() > 1 {
                        let caller = &self.callinfo[self.callinfo.len() - 2];
                        let caller_fn = &caller.module.functions()[caller.func as usize];
                        let area = caller_fn.layout.arg_area_len();
                        if usize::from(n) > area {
                            return Err((
                                Trap::RetSpanTooWide {
                                    span: u16::from(n),
                                    caller_area: u16::try_from(area)
                                        .expect("argument areas fit u16"),
                                },
                                pc,
                            ));
                        }
                    }
                    // place the span at overlay words 0..n. The
                    // destination `fp+1+i` is the caller's
                    // outgoing-argument word i for every i below the
                    // caller's area (the callee's own slots alias those
                    // words); words at or past the callee's call word are
                    // dead at this point, so writing over them is safe.
                    // Ascending is overlap-safe: the destination sits
                    // below the source for src > 0, and src == 0 is an
                    // identity copy.
                    for i in 0..usize::from(n) {
                        let slot = u8::try_from(usize::from(src) + i)
                            .expect("span stays inside the u8 slot range");
                        let value = read_value(&self.stack, fp, slot, slots, body_len)
                            .map_err(|e| (e, pc))?;
                        self.stack[fp + 1 + i] = Word::Slot(value);
                    }
                    let value = match self.stack[fp + 1] {
                        Word::Slot(v) => v,
                        Word::Meta(_) => {
                            return Err((Trap::MetaWordCorrupt { fp }, pc));
                        },
                    };
                    let mut st = stack_state!();
                    match st.ret(value, fp, boundary).map_err(|e| (e, pc))? {
                        RetOutcome::Root(v) => match self.complete_current(v) {
                            Some((a, at)) => {
                                active = a;
                                pc = at;
                            },
                            None => return Ok(DriveOutcome::Died(v)),
                        },
                        RetOutcome::Boundary(v) => return Ok(DriveOutcome::Frame(v)),
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
                    let value =
                        read_value(&self.stack, fp, slot, slots, body_len).map_err(|e| (e, pc))?;
                    // HALT ends the fiber; under a nested reentrant drive
                    // that fiber carries the Rust-side native loop's
                    // frames — refuse instead of dying mid-native (the
                    // frames stay for backtrace)
                    if self.reentry_live(boundary) {
                        return Err((Trap::ReentrancyRefused, pc));
                    }
                    match self.complete_current(value) {
                        Some((a, at)) => {
                            active = a;
                            pc = at;
                        },
                        None => return Ok(DriveOutcome::Died(value)),
                    }
                },
            }
        }
    }

    /// Renders the live frames of the fiber the last drive ended on
    /// (innermost first): each real frame with its position plus the
    /// tail-chain elided count [H2]. The counter is always available;
    /// per-elision `ElisionRecord`s for lazy materialization come with
    /// per-fiber debug recording, so at M3 the count reports how many
    /// logical frames a debugger would materialize later.
    #[must_use]
    pub fn backtrace(&self) -> Vec<BacktraceFrame> {
        let fiber = &self.pool[self.current.index()];
        let mut frames = Vec::with_capacity(fiber.callinfo.len());
        for depth in (0..fiber.callinfo.len()).rev() {
            let ci = &fiber.callinfo[depth];
            let f = &ci.module.functions()[ci.func as usize];
            let pc = if depth + 1 == fiber.callinfo.len() {
                self.trap_pc.unwrap_or(CodePc(0))
            } else {
                match fiber.segment[fiber.callinfo[depth + 1].fp] {
                    Word::Meta(m) => m.return_pc,
                    Word::Slot(_) => CodePc(0),
                }
            };
            let elided = match fiber.segment[ci.fp] {
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
