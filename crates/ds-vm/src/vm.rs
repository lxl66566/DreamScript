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

use std::{collections::HashMap, sync::Arc};

use ds_abi::{
    CodePc, ConstIdx, FrameLayout, FrameMetadata, RC_MAJOR, RcDecodeError, RcOp, SlideDirection,
    SlideError, SlotType, TailChain,
    uop::{DecodeError, Uop},
};

use crate::{
    EVIDENCE_MODULE,
    claims::ClaimTable,
    fiber::{FIBER_ROOT, Fiber, FiberId, FiberState, ResumePoint, Segment},
    module::{ModuleRegistry, ModuleVersion},
    native::{
        ControlNative, NURSERY_BOUNDARY_ERR, NURSERY_BOUNDARY_OK, NativeFunction, NativeResult,
        OutputSink, PLATFORM_CANCELLED, PLATFORM_NONE,
    },
    nursery::{ChildOutcome, JoinOutcome, Nursery, NurseryId, NurseryState},
    object::{Heaps, Objects},
    program::{Const, ConstKind},
    value::{FuncRef, RefId, Value, Word},
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
    /// `cancel(payload)` [E1, E2]: the fiber's own deliberate failure. The
    /// payload is the error value it handed `cancel`; the trap never
    /// resumes — the trap path ledgers the outcome into the fiber's
    /// nursery and raises the cancel flag, and the boundary seam unions
    /// the payload out at the nursery's join.
    NurseryCancelled {
        /// The failing child's error value.
        payload: Value,
    },
    /// A nursery boundary's first-failure payload sits outside the
    /// caller's return-union containment set, so the join refuses to
    /// union it out; the payload rides for the trap report.
    NurseryUncontained {
        /// The payload the boundary refused.
        payload: Value,
    },
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

/// An owned CALL-site resolution handed to a dispatch arm: the uop shape
/// carries the callee's version handle (the frame push owns one), the
/// native shape the shared table handle plus index — borrows of the arm's
/// own locals, so no loan on the VM outlives the arm's whole-self calls
/// (the control-native switch).
#[derive(Debug)]
enum Resolved {
    /// Enter the callee's encoded body.
    Uop {
        /// The callee's module version.
        version: Arc<ModuleVersion>,
        /// The callee's function-table index.
        func: u32,
    },
    /// Invoke the native in the caller's frame (no frame push).
    Native {
        /// The native module's shared function table.
        table: Arc<Vec<NativeFunction>>,
        /// The callee's index in the table.
        index: u32,
    },
}

/// The ownable form of a call-site resolution the inline cache stores: the
/// uop shape holds the resolved version handle, the native shape the
/// shared table plus index (natives are not values — the table is the
/// stable handle, the index the callee).
#[derive(Clone, Debug)]
enum CachedCallee {
    Uop {
        version: Arc<ModuleVersion>,
        func: u32,
    },
    Native {
        table: Arc<Vec<NativeFunction>>,
        index: u32,
    },
}

/// The resolution of a dispatch site's (CALLG/CALLTG) pool FuncRef against
/// the uop module tables: the impl body a trait site's FuncRef names, or
/// the not-a-uop-callee signal that hands the site to the generic builtin
/// tables (the sentinel evidence FuncRef never reaches the resolver).
#[derive(Debug)]
enum DispatchResolved {
    /// Enter (or tail-transfer into) the named uop function's body.
    Uop {
        version: Arc<ModuleVersion>,
        func: u32,
    },
    /// The FuncRef names no uop function: the generic builtin tables
    /// answer next.
    NotUop,
}

/// One CALL-site inline-cache slot: the resolution of (caller function,
/// pc) as of `stamp`. A hit requires all three to agree — the caller
/// identity is the executing function's address (stable for the version's
/// lifetime; a freed-and-reused address requires an install, which bumps
/// the stamp), the pc discriminates call sites within one body, and the
/// stamp is the registry generation the resolution ran against, so any
/// install (a two-slot swap, a native-table replacement) invalidates every
/// entry at once. A miss falls back to the full lookup, so dispatch
/// correctness never depends on the cache.
#[derive(Clone, Debug)]
struct CallSite {
    caller: usize,
    pc: u32,
    stamp: u64,
    callee: CachedCallee,
}

/// CALL-site cache slot count (direct-mapped, one uop per slot).
const CALL_CACHE_SLOTS: usize = 512;

/// Words a native argument list or closure capture block holds before the
/// staging falls back to the heap: every native table entry and every
/// lowered capture shape sits well under this, so the per-call heap
/// allocations disappear from the common cases.
const INLINE_WORDS: usize = 8;

/// Stages a closure's capture block for a CALLS copy-in without a heap
/// allocation per call: real capture blocks are a handful of words, so
/// they copy into the inline array and only an oversized block falls back
/// to the reused heap buffer (cleared per stage, never reallocated).
#[derive(Debug)]
struct CaptureStaging {
    inline: [Value; INLINE_WORDS],
    heap: Vec<Value>,
}

impl CaptureStaging {
    fn new() -> Self {
        Self {
            inline: [Value::I64(0); INLINE_WORDS],
            heap: Vec::new(),
        }
    }

    /// Copies `block` in and answers the staged slice, valid until the
    /// next `stage`.
    fn stage<'a>(&'a mut self, block: &[Value]) -> &'a [Value] {
        if block.len() <= INLINE_WORDS {
            self.inline[..block.len()].copy_from_slice(block);
            &self.inline[..block.len()]
        } else {
            self.heap.clear();
            self.heap.extend_from_slice(block);
            &self.heap
        }
    }
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
) -> Result<RefId, Trap> {
    match read_typed(stack, fp, slot, slots, body_len, SlotType::Ref)? {
        Value::Ref(word) => Ok(word),
        _ => unreachable!("slot_type agrees with the variant"),
    }
}

/// The reference one RC op moves, when the word holds one: a `Raw` slot's
/// kind is a runtime fact (a type-variable value), so its inc/dec apply
/// only when the word is a reference at runtime and no-op on scalars;
/// every other slot classification keeps the strict typed read.
fn read_counted(
    stack: &Segment,
    fp: usize,
    slot: u8,
    slots: &[SlotType],
    body_len: usize,
) -> Result<Option<RefId>, Trap> {
    if slots[usize::from(slot)] == SlotType::Raw {
        return Ok(match read_value(stack, fp, slot, slots, body_len)? {
            Value::Ref(word) => Some(word),
            _ => None,
        });
    }
    read_ref(stack, fp, slot, slots, body_len).map(Some)
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

/// Reads a closure word's header and stages its capture block through the
/// shared buffer, routing to the word's own heap. The disjoint-field
/// parameters keep the staged slice's borrow on the staging buffer alone,
/// so the caller may push frames and route count mutations while it lives.
fn stage_closure<'s>(
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
            .ok_or(Trap::UnknownObject { obj: word.id() })?
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
fn invoke_native(
    nf: &NativeFunction,
    stack: &mut Segment,
    heaps: &mut Heaps,
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
    // the argument list stages inline — every native table entry sits well
    // under the inline width — with the heap buffer the oversized fallback
    let mut inline = [Value::I64(0); INLINE_WORDS];
    let mut heap = Vec::new();
    let args: &[Value] = if nf.params.len() <= INLINE_WORDS {
        for (i, word) in inline.iter_mut().enumerate().take(nf.params.len()) {
            let slot = u8::try_from(start + i).expect("slot index fits u8");
            *word = read_value(stack, fp, slot, caller.slots(), body_len)?;
        }
        &inline[..nf.params.len()]
    } else {
        heap.reserve(nf.params.len());
        for i in 0..nf.params.len() {
            let slot = u8::try_from(start + i).expect("slot index fits u8");
            heap.push(read_value(stack, fp, slot, caller.slots(), body_len)?);
        }
        &heap
    };
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
    (nf.func)(heaps, out, args)
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

/// Reads one staged word of any kind on overlay word `index` for a
/// control native (the callable and cancel seams carry words whose kinds
/// the call site proves, not the table).
fn control_word(
    stack: &Segment,
    fp: usize,
    caller: &FrameLayout,
    index: usize,
) -> Result<Value, Trap> {
    let body_len = caller.slots().len() - caller.arg_area_len();
    let slot = u8::try_from(caller.arg_area().start + index).expect("slot index fits u8");
    read_value(stack, fp, slot, caller.slots(), body_len)
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
    /// The running fiber's heap object store, swapped in from its pool
    /// slot beside the segment; the slot holds a scratch store while
    /// the fiber runs. Host-facing store access routes by the current
    /// fiber ([`Vm::objects`]).
    objects: Objects,
    out: OutputSink,
    /// The running fiber's segment and call chain, swapped in from its
    /// pool slot beside the store; `ctx_live` tracks whether they are
    /// here (true) or stashed in the slot (false).
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
    /// Per-fiber claim ledgers, parallel to the pool: the multiset of
    /// counts each fiber's frames own (crates/ds-vm/src/claims.rs) —
    /// what the teardown drain walk releases. Index-parallel to the
    /// pool, never swapped: the running fiber's ledger is reached
    /// through `current` directly.
    claims: Vec<ClaimTable>,
    /// The CALL-site inline cache: one direct-mapped slot bank keyed on
    /// (caller function identity, pc), entries stamped with the registry
    /// generation they resolved against. Fibers share the bank safely —
    /// the key is per-function code, never per-fiber stack state, and
    /// validity rides the global install generation.
    call_cache: Vec<Option<CallSite>>,
    /// The KSTR pool-const intern cache: per-(pool, fiber) object-id
    /// tables, keyed by the pool's address (stable for its version's
    /// lifetime) and the loading fiber — each heap interns its own copy
    /// of a constant, so the cached id is only ever served against the
    /// fiber whose store minted it. No claim is held — a hit
    /// re-validates the id against the constant's content and falls
    /// back to the full intern when the object died, so object
    /// lifetimes read exactly as without the cache.
    kstr_cache: HashMap<(usize, u32), Vec<Option<u32>>>,
    /// The reused capture staging buffer for CALLS copy-ins (per-call
    /// `to_vec` copies are gone; a borrow of this buffer lives only
    /// within one dispatch arm or hosted call, never across a reentry).
    calls_staging: CaptureStaging,
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
            objects: Objects::new(FIBER_ROOT),
            out: OutputSink::default(),
            stack: Segment::new(),
            callinfo: Vec::new(),
            ctx_live: false,
            pool: vec![Fiber::empty(FIBER_ROOT)],
            current: FIBER_ROOT,
            nurseries: Vec::new(),
            root_nursery: None,
            limit,
            trap_pc: None,
            claims: vec![ClaimTable::default()],
            call_cache: vec![None; CALL_CACHE_SLOTS],
            kstr_cache: HashMap::new(),
            calls_staging: CaptureStaging::new(),
        }
    }

    /// Interns a string into the root fiber's heap and hands back its
    /// reference word: the host-facing way to stage string arguments
    /// into `run` and bare spawns (and the discipline KSTR follows for
    /// pool constants). The claim is one reference count; a host that
    /// abandons a staged word never decs it, so host-held references
    /// keep their objects alive. Host words always name the root heap —
    /// a child's frame holds them as foreign words, served by routing.
    #[must_use = "the reference is the handle; the count is the host's to drop"]
    pub fn intern_str(&mut self, text: &str) -> Value {
        let id = self.root_store().intern(text);
        Value::Ref(RefId::new(FIBER_ROOT, id))
    }

    /// Stages a closure value for the host (the F2 capturing-lambda
    /// object): one heap object behind one `Ref` word, the header FuncRef
    /// word plus the moved-in capture block, born on the root heap like
    /// every host-staged word. The host-held reference owns the object's
    /// birth count; the module-code construction path (and with it the
    /// value's own RC traffic) waits on lambda-value lowering.
    #[must_use = "the reference is the handle; the count is the host's to drop"]
    pub fn alloc_closure(&mut self, func: FuncRef, captures: Vec<Value>) -> Value {
        let id = self.root_store().alloc_closure(func, captures);
        Value::Ref(RefId::new(FIBER_ROOT, id))
    }

    /// Switches the output sink to an in-memory buffer: print/println
    /// write there until the next [`Vm::run`] or forever. Callers read the
    /// bytes with [`Vm::captured_output`].
    pub fn capture_output(&mut self) {
        self.out.capture();
    }

    /// The bytes captured so far ([`Vm::capture_output`]).
    #[must_use]
    pub fn captured_output(&self) -> &[u8] {
        self.out.captured_bytes()
    }

    /// The current fiber's heap object store: hosts and tests inspect run
    /// results (string content, list elements) by the `Ref` word a run
    /// returned. The drive ends on this fiber, so its store is parked in
    /// its slot; a word naming another fiber routes through
    /// [`Vm::objects_at`].
    #[must_use]
    pub fn objects(&self) -> &Objects {
        self.objects_of_fiber(self.current)
    }

    /// The store owning `fiber`'s heap: the running fiber's context store
    /// when it is `fiber`, its pool slot's otherwise.
    #[must_use]
    pub fn objects_at(&self, fiber: FiberId) -> &Objects {
        self.objects_of_fiber(fiber)
    }

    /// The store owning `word`'s heap, by the word's owner axis.
    #[must_use]
    pub fn objects_of(&self, word: RefId) -> &Objects {
        self.objects_of_fiber(word.fiber())
    }

    fn objects_of_fiber(&self, fiber: FiberId) -> &Objects {
        if self.ctx_live && fiber == self.current {
            &self.objects
        } else {
            &self.pool[fiber.index()].objects
        }
    }

    /// Bounds every blocking net native's wait (connect, read, write):
    /// the default is 3 seconds; a hostile host yields `net_err`, never a
    /// hung interpreter.
    pub fn set_net_timeout(&mut self, timeout: std::time::Duration) {
        self.out.set_net_timeout(timeout);
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
    // Pool ownership invariant: the running fiber's segment, call chain,
    // and heap store live in the VM's context fields while `ctx_live`
    // holds; every other time (and for every other fiber, always) they
    // live in the pool slot. A switch is three ownership swaps — no word
    // or object is ever copied.

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
        Ok(self.push_spawned(nursery, version, func_id, args))
    }

    /// Spawns a fiber from a callable word's own function [E2]: the
    /// in-language `spawn` seam. The FuncRef resolves through the target
    /// module's current slot; the callable's captures become the entry
    /// arguments, so the entry checks of [`Vm::run`] apply verbatim.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownModule`] when the FuncRef's module id names no
    /// installed module; the entry checks of [`Vm::run`] otherwise.
    fn spawn_ref_into(
        &mut self,
        nursery: Option<NurseryId>,
        fref: FuncRef,
        args: &[Value],
    ) -> Result<FiberId, Trap> {
        let name = self
            .registry
            .module_name(fref.module())
            .ok_or(Trap::UnknownModule {
                module: fref.module(),
            })?;
        let (version, func_id) =
            self.registry
                .resolve_indexed(name, fref.func())
                .ok_or(Trap::UnknownCallee {
                    module: name.to_owned(),
                    func: fref.func(),
                })?;
        self.entry_checks(&version, func_id, args)?;
        let child = self.push_spawned(nursery, version, func_id, args);
        // each Ref argument acquires one count for the child's frame at
        // the copy-in — the child's own exit drops balance it, precisely
        // a closure call's capture copy-in. The count lands on the word's
        // own heap (which the spawner may not be running on) and the claim
        // on the child's ledger.
        let handoff_owner = self.current;
        for &arg in args {
            if let Value::Ref(word) = arg {
                Self::inc_ref_split(
                    &mut self.objects,
                    &mut self.pool,
                    &mut self.claims,
                    handoff_owner,
                    child,
                    word,
                )?;
            }
        }
        Ok(child)
    }

    /// The pool-side half of every spawn: one pool entry with a fresh
    /// entry frame, born ready and never started, plus the child-of
    /// bookkeeping. The first switch into it runs its entry function; the
    /// pool slot of a dead fiber is reused.
    fn push_spawned(
        &mut self,
        nursery: Option<NurseryId>,
        version: Arc<ModuleVersion>,
        func_id: u32,
        args: &[Value],
    ) -> FiberId {
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
            let id = FiberId(u32::try_from(slot).expect("pool size fits u32 ids"));
            self.pool.push(Fiber::empty(id));
            self.claims.push(ClaimTable::default());
        } else {
            // the reuse door of the death walk: every death path drains
            // its fiber, so this releases what an un-walked marking or an
            // exotic death left behind — and nothing on the honest paths
            self.drain_claims(FiberId(u32::try_from(slot).expect("slot fits u32")));
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
        id
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
            Ok(DriveOutcome::Died(v) | DriveOutcome::Yielded(v)) => {
                // the drive-ended fiber's answer crosses to the host; the
                // fiber itself keeps its remaining claims (interrupted
                // drives keep their frames, and their ledgers, whole)
                self.host_delivery(&v);
                Ok(v)
            },
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
                // the trap's payload word crosses to the host
                self.trap_delivery(&trap);
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
        let id = self.open_fresh_nursery();
        self.root_nursery = Some(id);
        id
    }

    /// Opens a fresh nursery [E2]: the in-language `nursery` block's own
    /// (never the root — bare host spawns route there alone). Dead slots
    /// reuse like fiber slots.
    fn open_fresh_nursery(&mut self) -> NurseryId {
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
        NurseryId(u32::try_from(slot).expect("nursery pool fits u32 ids"))
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
        let outcomes = self.run_nursery_to_death(nursery, false, None);
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
        Ok(self.run_nursery_to_death(nursery, true, None))
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
        claim_target: Option<FiberId>,
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
            // its frames until now, then its slot is released. A cancel
            // payload riding the recorded outcome keeps its claim — the
            // delivery takes it below — while every other claim the kept
            // frames own is released here, routed to each word's own heap
            // (the one cross-heap count channel of a death [E5])
            if self.pool[child.index()].state != FiberState::Dead {
                let held = match self.nurseries[nursery.index()].outcome(child) {
                    Some(ChildOutcome::Trapped(Trap::NurseryCancelled {
                        payload: Value::Ref(word),
                    })) => Some(word),
                    _ => None,
                };
                self.drain_claims_except(child, held);
                if let Some(word) = held {
                    self.claims[child.index()].release(word);
                    if let Some(target) = claim_target {
                        self.claims[target.index()].acquire(word);
                    }
                }
                let fiber = &mut self.pool[child.index()];
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
        self.entry_checks(&version, func_id, args)?;
        Ok((version, func_id))
    }

    /// The argument and frame checks every entry shares [E2]: arity,
    /// argument word kinds (a Ref word against a Fn parameter is the
    /// callable-defaulted slot), and the entry frame versus the stack
    /// limit.
    fn entry_checks(
        &self,
        version: &Arc<ModuleVersion>,
        func_id: u32,
        args: &[Value],
    ) -> Result<(), Trap> {
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
        Ok(())
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
    /// drive; the pool is then the single home of every fiber's state —
    /// three buffer swaps, no word or object ever copied.
    fn host_release(&mut self) {
        if !self.ctx_live {
            return;
        }
        let fiber = &mut self.pool[self.current.index()];
        std::mem::swap(&mut self.stack, &mut fiber.segment);
        std::mem::swap(&mut self.callinfo, &mut fiber.callinfo);
        std::mem::swap(&mut self.objects, &mut fiber.objects);
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
            std::mem::swap(&mut self.objects, &mut fiber.objects);
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
        // the delivered value's claim crosses with it — the receiver is
        // resolved below. A completed fiber's exit decs have released its
        // other claims already, so no walk runs here: a raised leftover is
        // observable state (the rc_diag witness), released only by the
        // cancel-cut and interrupt doors
        if let Value::Ref(word) = value {
            self.claims[self.current.index()].release(word);
        }
        let fiber = &mut self.pool[self.current.index()];
        fiber.state = FiberState::Dead;
        fiber.resume = None;
        let live = resumer.filter(|&r| self.pool[r.index()].resumable());
        if let (Some(&target), Value::Ref(word)) = (live.as_ref(), value) {
            self.claims[target.index()].acquire(word);
        }
        live.map(|target| self.switch_in(target, value))
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
        // the cut fiber's exit decs never run: the death walk releases
        // its kept frames' claims here — the cancel-cut door
        self.drain_claims(self.current);
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
                let outcomes = self.run_nursery_to_death(nursery, false, Some(joiner));
                let failed = outcomes
                    .iter()
                    .filter(|o| matches!(o, ChildOutcome::Trapped(_)))
                    .count();
                let count = i64::try_from(failed).expect("child count fits i64");
                match self.reenter_joiner(joiner, CodePc(pc + 1), Value::I64(count)) {
                    Some((a, at)) => Ok(SwitchOutcome::Entered(a, at)),
                    // no call site stands: the count surfaces to the host
                    None => Ok(SwitchOutcome::Host(DriveOutcome::Died(Value::I64(count)))),
                }
            },
            // the in-language seams [E2]: open, spawn, the boundary join,
            // and cancel. The lowerer synthesizes every call site, so a
            // staged id always names this frame's own open nursery; the
            // guards answer the checked-read bounce tag (the native-table
            // convention) for hand-assembled corruption.
            ControlNative::NurseryOpen => {
                let id = self.open_fresh_nursery();
                overlay_native(
                    NativeResult::Word(Value::I64(i64::from(id.0))),
                    &mut self.stack,
                    caller,
                    fp,
                );
                Ok(SwitchOutcome::Bounced)
            },
            ControlNative::NurserySpawn => {
                let id = control_i64(&self.stack, fp, caller, 0)?;
                let word = control_word(&self.stack, fp, caller, 1)?;
                let nursery = u32::try_from(id).ok().map(NurseryId).filter(|&n| {
                    self.nurseries
                        .get(n.index())
                        .is_some_and(|nr| nr.state == NurseryState::Open)
                });
                let Some(nursery) = nursery else {
                    overlay_native(
                        NativeResult::Word(Value::Tag(PLATFORM_NONE)),
                        &mut self.stack,
                        caller,
                        fp,
                    );
                    return Ok(SwitchOutcome::Bounced);
                };
                // the callable word: a scalar `Fn` names the child with no
                // captures; a closure object's `Ref` carries them as the
                // entry arguments
                let (fref, args) = match word {
                    Value::Fn(fref) => (fref, Vec::new()),
                    Value::Ref(obj) => match self.closure_parts(obj) {
                        Ok((fref, captures)) => (fref, captures),
                        Err(e) => return Err(e),
                    },
                    staged => {
                        return Err(Trap::CallLayout(SlideError::TypeMismatch {
                            index: 1,
                            staged: staged.slot_type(),
                            expected: SlotType::Fn,
                        }));
                    },
                };
                self.spawn_ref_into(Some(nursery), fref, &args)?;
                overlay_native(
                    NativeResult::Word(Value::I64(0)),
                    &mut self.stack,
                    caller,
                    fp,
                );
                Ok(SwitchOutcome::Bounced)
            },
            ControlNative::NurseryBoundary => {
                let id = control_i64(&self.stack, fp, caller, 0)?;
                let mask = control_i64(&self.stack, fp, caller, 1)?;
                let nursery = u32::try_from(id).ok().map(NurseryId).filter(|&n| {
                    self.nurseries
                        .get(n.index())
                        .is_some_and(|nr| nr.state == NurseryState::Open)
                });
                let Some(nursery) = nursery else {
                    overlay_native(
                        NativeResult::Word(Value::Tag(PLATFORM_NONE)),
                        &mut self.stack,
                        caller,
                        fp,
                    );
                    return Ok(SwitchOutcome::Bounced);
                };
                if self.pool[self.current.index()].nursery == Some(nursery) {
                    return Err(Trap::UnknownCallee {
                        module: "nursery".into(),
                        func: nursery.0,
                    });
                }
                self.park_current(active, CodePc(pc + 1));
                let joiner = self.current;
                let outcomes = self.run_nursery_to_death(nursery, false, Some(joiner));
                // the first failure in spawn-drive order decides: a
                // cancelled child with no trapped one before it cannot
                // happen (the flag only rises at a trapped death)
                match outcomes
                    .iter()
                    .find(|o| matches!(o, ChildOutcome::Trapped(_)))
                {
                    None => match self.reenter_joiner(joiner, CodePc(pc + 1), Value::I64(0)) {
                        Some((a, at)) => {
                            self.overlay_boundary(&a, NURSERY_BOUNDARY_OK, Value::I64(0));
                            Ok(SwitchOutcome::Entered(a, at))
                        },
                        // no call site stands: the discriminant surfaces to the host
                        None => Ok(SwitchOutcome::Host(DriveOutcome::Died(Value::Tag(
                            NURSERY_BOUNDARY_OK,
                        )))),
                    },
                    Some(ChildOutcome::Trapped(Trap::NurseryCancelled { payload })) => {
                        // the staged mask is the caller's return-union
                        // member containment set: an error the boundary
                        // cannot union out refuses instead of corrupting
                        // the union's tag space
                        let contained = match payload {
                            Value::Tag(tag) => *tag < 64 && (mask & (1i64 << *tag)) != 0,
                            _ => false,
                        };
                        if !contained {
                            // re-enter the joiner so the drive ends on it
                            // and the trap attributes at the boundary; the
                            // call site never reads the answer
                            let _ = self.reenter_joiner(joiner, CodePc(pc + 1), Value::I64(0));
                            return Err(Trap::NurseryUncontained { payload: *payload });
                        }
                        match self.reenter_joiner(joiner, CodePc(pc + 1), Value::I64(0)) {
                            Some((a, at)) => {
                                self.overlay_boundary(&a, NURSERY_BOUNDARY_ERR, *payload);
                                Ok(SwitchOutcome::Entered(a, at))
                            },
                            // no call site stands: the payload surfaces to the host
                            None => Ok(SwitchOutcome::Host(DriveOutcome::Died(*payload))),
                        }
                    },
                    Some(ChildOutcome::Trapped(trap)) => {
                        // a real trap surfaces at the boundary: re-enter
                        // the joiner first, so the drive ends on it and
                        // the trap attributes there
                        let _ = self.reenter_joiner(joiner, CodePc(pc + 1), Value::I64(0));
                        Err(trap.clone())
                    },
                    Some(_) => unreachable!("the scan yields only trapped failures"),
                }
            },
            ControlNative::NurseryCancel => {
                // the fiber's deliberate failure: the trap path ledgers
                // the outcome and raises the cancel flag; the call never
                // answers
                let payload = control_word(&self.stack, fp, caller, 0)?;
                Err(Trap::NurseryCancelled { payload })
            },
        }
    }

    /// Writes the boundary seam's two-word answer span onto the joiner's
    /// call site: word 0 the discriminant, word 1 the payload. Runs
    /// after the joiner's re-entry, so the current context is its frame.
    fn overlay_boundary(&mut self, active: &Active, tag: u8, payload: Value) {
        let fp = self.callinfo[self.callinfo.len() - 1].fp;
        let f = &active.version.functions()[active.func as usize];
        overlay_native(
            NativeResult::Tag { tag, payload },
            &mut self.stack,
            &f.layout,
            fp,
        );
    }

    /// Re-enters the joiner a join/boundary drive parked at `resume_pc`,
    /// unless a child resumed it mid-drive and it moved on — died, was
    /// cancel-cut, trapped, or re-parked at another call — in which case
    /// the boundary's answer has no call site, and the drive surfaces
    /// `value` to its own host instead of switching into a fiber with no
    /// continuation (the shrunk `nursery_joiner_reentry` repro).
    fn reenter_joiner(
        &mut self,
        joiner: FiberId,
        resume_pc: CodePc,
        value: Value,
    ) -> Option<(Active, CodePc)> {
        let parked_here = self.pool.get(joiner.index()).is_some_and(|f| {
            f.state == FiberState::Suspended && f.resume.as_ref().is_some_and(|r| r.pc == resume_pc)
        });
        if parked_here { Some(self.switch_in(joiner, value)) } else { None }
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
        // the reset door of the death walk: a halted or trapped root
        // drive's kept frames release their claims before the reset
        // discards them (the tail-native abandon's count dies here now,
        // at the next run, instead of leaking to the VM's drop)
        self.drain_claims(FIBER_ROOT);
        let root = &mut self.pool[FIBER_ROOT.index()];
        root.segment.clear();
        Self::push_entry_frame(root, &version, func_id, args, frame_size);
        root.resume = None;
        root.resumer = None;
        root.state = FiberState::Running;
        let root_slot = &mut self.pool[FIBER_ROOT.index()];
        std::mem::swap(&mut self.stack, &mut root_slot.segment);
        std::mem::swap(&mut self.callinfo, &mut root_slot.callinfo);
        std::mem::swap(&mut self.objects, &mut root_slot.objects);
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
                    // the completed root's answer crosses to the host
                    self.host_delivery(&value);
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
                    // the trap's payload word crosses to the host
                    self.trap_delivery(&trap);
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
                let (header, block) = stage_closure(
                    &mut self.objects,
                    &mut self.pool,
                    self.current,
                    &mut self.calls_staging,
                    obj,
                )?;
                (header, Some(block))
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
        // and a dropped one balances at the body's exit drops). The
        // word may name a foreign heap, so the inc routes.
        let handoff_owner = self.current;
        for &arg in args {
            if let Value::Ref(word) = arg {
                Self::inc_ref_split(
                    &mut self.objects,
                    &mut self.pool,
                    &mut self.claims,
                    handoff_owner,
                    handoff_owner,
                    word,
                )?;
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
            let owner = self.current;
            for (index, &value) in block.iter().enumerate() {
                if let Value::Ref(word) = value {
                    Self::inc_ref_split(
                        &mut self.objects,
                        &mut self.pool,
                        &mut self.claims,
                        owner,
                        owner,
                        word,
                    )?;
                }
                let slot = u8::try_from(target.layout.params_len() + index)
                    .expect("capture slot fits u8 after the range check");
                write_slot(
                    &mut self.stack,
                    base,
                    slot,
                    target.layout.slots(),
                    capture_body_len,
                    value,
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

    /// The routing face over the running fiber's store and the pool, for
    /// the dispatcher's own routed count mutations and reads.
    fn heaps(&mut self) -> Heaps<'_> {
        Heaps::new(
            self.current,
            &mut self.objects,
            &mut self.pool,
            &mut self.claims,
            self.ctx_live,
        )
    }

    /// The death walk's router: a parked drive's context store is the
    /// scratch, so every store is reached through the pool.
    fn heaps_walk(&mut self) -> Heaps<'_> {
        Heaps::new(
            self.current,
            &mut self.objects,
            &mut self.pool,
            &mut self.claims,
            false,
        )
    }

    /// The root fiber's store, for the host faces: the running context's
    /// when the root runs, its pool slot's otherwise.
    fn root_store(&mut self) -> &mut Objects {
        if self.ctx_live && self.current == FIBER_ROOT {
            &mut self.objects
        } else {
            &mut self.pool[FIBER_ROOT.index()].objects
        }
    }

    /// Routed `rc_inc` by the word's owner axis, queues drained.
    fn inc_ref(&mut self, word: RefId) -> Result<(), Trap> {
        self.heaps().inc(word)
    }

    /// Routed `rc_dec` by the word's owner axis, queues drained.
    fn dec_ref(&mut self, word: RefId) -> Result<(), Trap> {
        self.heaps().dec(word)
    }

    /// [`Vm::inc_ref`] over disjoint fields, for sites holding a live
    /// staging-buffer borrow (the capture copy-ins). The count is claimed
    /// for `claimant`'s frames — the running fiber's own copy-ins, or the
    /// spawned child's entry frame at the spawn seam.
    fn inc_ref_split(
        objects: &mut Objects,
        pool: &mut [Fiber],
        claims: &mut [ClaimTable],
        running: FiberId,
        claimant: FiberId,
        word: RefId,
    ) -> Result<(), Trap> {
        Heaps::new(running, objects, pool, claims, true).inc_for(claimant, word)
    }

    /// An object-graph edge's count (a closure's capture adoption): routed
    /// by the word's owner, owned by the object — its free cascade
    /// releases it, no frame claim.
    fn inc_edge(&mut self, word: RefId) -> Result<(), Trap> {
        self.heaps().inc_edge(word)
    }

    /// The teardown drain walk: every claim the fiber's frames own,
    /// released, routed to each word's own heap — the one channel for
    /// cross-heap count mutation on a death. A bogus word (a
    /// hand-assembled release without an acquire) errors and is ignored:
    /// the walk releases what the store knows.
    fn drain_claims(&mut self, fiber: FiberId) {
        self.drain_claims_except(fiber, None);
    }

    /// [`Self::drain_claims`] holding `held` back — a word about to ride
    /// its recorded outcome to a delivery keeps its claim. A fiber running
    /// under the live context decs through `objects`; every other case
    /// (and every fiber while the drive is parked) routes through the
    /// pool, the scratch never receiving a walk's dec.
    fn drain_claims_except(&mut self, fiber: FiberId, held: Option<RefId>) {
        let live_here = self.ctx_live && fiber == self.current;
        for (word, n) in self.claims[fiber.index()].take_all_except(held) {
            for _ in 0..n {
                let _ =
                    if live_here { self.dec_ref(word) } else { self.heaps_walk().dec_walk(word) };
            }
        }
    }

    /// A value crossing to the host from the drive-ended fiber: its
    /// claim leaves the ledger. The host never releases, so the count
    /// lives until the stores die — exactly the pre-split behavior.
    fn host_delivery(&mut self, value: &Value) {
        if let Value::Ref(word) = value {
            self.claims[self.current.index()].release(*word);
        }
    }

    /// The payload word a nursery trap carries: delivered to the host
    /// like any drive-ending value.
    fn trap_delivery(&mut self, trap: &Trap) {
        match trap {
            Trap::NurseryCancelled { payload } | Trap::NurseryUncontained { payload } => {
                self.host_delivery(payload);
            },
            _ => {},
        }
    }

    /// Whether two reference words name content-equal strings, routed
    /// (string equality is content equality whatever heap each side
    /// lives on).
    fn str_eq(&mut self, x: RefId, y: RefId) -> Result<bool, Trap> {
        self.heaps().str_eq(x, y)
    }

    /// A closure word's header and capture block copied out of its own
    /// heap ([`Heaps::closure_parts`]).
    fn closure_parts(&mut self, word: RefId) -> Result<(FuncRef, Vec<Value>), Trap> {
        self.heaps().closure_parts(word)
    }

    /// Splits the VM for a generic builtin body's pure-object work: the
    /// store-side routing face and the output sink, disjoint from every
    /// other field. The running fiber's store is the allocation default;
    /// every id-taking service routes. The reentrant bodies (the
    /// lambda-taking combinators) keep `vm` whole and call back through
    /// [`Vm::call_hosted`].
    pub(crate) fn split(&mut self) -> (Heaps<'_>, &mut OutputSink) {
        (
            Heaps::new(
                self.current,
                &mut self.objects,
                &mut self.pool,
                &mut self.claims,
                self.ctx_live,
            ),
            &mut self.out,
        )
    }

    /// Resolves a pool FuncRef for the CALL/CALLT dispatch: the per-site
    /// inline cache first — the entry keyed on the caller function's
    /// identity and pc, validated against the registry's install
    /// generation — then the full lookup as the fallback and the refill.
    /// The full path is the same resolution discipline as ever: uop
    /// modules first (through the target's current slot), then the native
    /// tables (a uop module installed under a native table's name shadows
    /// the natives); a stamp mismatch after a swap lands here, so callee
    /// resolution always follows the current slot [H6 item 5]. Traps are
    /// never cached — every failure comes from the full path.
    fn resolve_site(
        &mut self,
        pool: &[Const],
        caller: usize,
        pc: u32,
        kidx: ConstIdx,
        nargs: u8,
    ) -> Result<Resolved, Trap> {
        let at = pc as usize & (CALL_CACHE_SLOTS - 1);
        let stamp = self.registry.generation();
        let hit = self.call_cache[at]
            .as_ref()
            .is_some_and(|e| e.caller == caller && e.pc == pc && e.stamp == stamp);
        if !hit {
            let entry = self.resolve_site_full(pool, caller, pc, kidx, nargs, stamp)?;
            self.call_cache[at] = Some(entry);
        }
        Ok(
            match &self.call_cache[at]
                .as_ref()
                .expect("the slot was just filled")
                .callee
            {
                CachedCallee::Uop { version, func } => Resolved::Uop {
                    version: Arc::clone(version),
                    func: *func,
                },
                CachedCallee::Native { table, index } => Resolved::Native {
                    table: Arc::clone(table),
                    index: *index,
                },
            },
        )
    }

    /// The full, uncached call-site resolution the cache falls back to:
    /// pool kind check, the target module's current slot, the entry-word
    /// and arity checks. The pool belongs to the CALLER's module version:
    /// instruction stream and pool are version-stable, callee resolution
    /// follows the target's current slot [H6 item 5].
    fn resolve_site_full(
        &self,
        pool: &[Const],
        caller: usize,
        pc: u32,
        kidx: ConstIdx,
        nargs: u8,
        stamp: u64,
    ) -> Result<CallSite, Trap> {
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
        if let Some((version, func_id)) = self.registry.resolve_indexed(module, *func) {
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
            return Ok(CallSite {
                caller,
                pc,
                stamp,
                callee: CachedCallee::Uop {
                    version,
                    func: func_id,
                },
            });
        }
        let table = self
            .registry
            .native_table(module)
            .filter(|t| usize::try_from(*func).is_ok_and(|i| i < t.len()))
            .ok_or(Trap::UnknownCallee {
                module: module.clone(),
                func: *func,
            })?;
        let native = &table[*func as usize];
        if usize::from(nargs) != native.params.len() {
            return Err(Trap::ArityMismatch {
                expected: native.params.len(),
                given: usize::from(nargs),
            });
        }
        Ok(CallSite {
            caller,
            pc,
            stamp,
            callee: CachedCallee::Native {
                table: Arc::clone(table),
                index: *func,
            },
        })
    }

    /// Resolves a dispatch site's (CALLG/CALLTG) pool FuncRef against the
    /// uop module tables: the same CALL-site inline cache bank CALL/CALLT
    /// key — the caller function's identity and pc, validated against the
    /// registry's install generation — so a dispatch tail site keys its
    /// callee exactly like every other call site, with the full lookup as
    /// the fallback and the refill. The dispatch arity rule differs from
    /// a plain call: the callee's parameters include the trailing
    /// evidence parameters the body claims, so `nargs` names only the
    /// real arguments and the full path proves `nargs <= params_len`
    /// (the staged evidence words' kinds are the arm's own check, on
    /// both paths). The sentinel evidence FuncRef never reaches here
    /// (its target is runtime data, never cacheable), and a generic
    /// builtin's FuncRef answers [`DispatchResolved::NotUop`] uncached —
    /// the static generic table needs no cache. Traps are never cached:
    /// every failure comes from the full path.
    fn resolve_dispatch_site(
        &mut self,
        pool: &[Const],
        caller: usize,
        pc: u32,
        kidx: ConstIdx,
        nargs: u8,
    ) -> Result<DispatchResolved, Trap> {
        let at = pc as usize & (CALL_CACHE_SLOTS - 1);
        let stamp = self.registry.generation();
        let hit = self.call_cache[at].as_ref().is_some_and(|e| {
            e.caller == caller
                && e.pc == pc
                && e.stamp == stamp
                && matches!(e.callee, CachedCallee::Uop { .. })
        });
        if !hit {
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
            let Some((version, func_id)) = self.registry.resolve_indexed(module, *func) else {
                return Ok(DispatchResolved::NotUop);
            };
            let target = &version.functions()[func_id as usize];
            let entry = target.entry.resolve();
            if entry != func_id as usize {
                return Err(Trap::EntryWordMismatch {
                    resolved: entry,
                    expected: func_id as usize,
                });
            }
            if usize::from(nargs) > target.layout.params_len() {
                return Err(Trap::ArityMismatch {
                    expected: target.layout.params_len(),
                    given: usize::from(nargs),
                });
            }
            self.call_cache[at] = Some(CallSite {
                caller,
                pc,
                stamp,
                callee: CachedCallee::Uop {
                    version: Arc::clone(&version),
                    func: func_id,
                },
            });
            return Ok(DispatchResolved::Uop {
                version,
                func: func_id,
            });
        }
        match &self.call_cache[at]
            .as_ref()
            .expect("the slot was just validated")
            .callee
        {
            CachedCallee::Uop { version, func } => Ok(DispatchResolved::Uop {
                version: Arc::clone(version),
                func: *func,
            }),
            // unreachable by the hit predicate; answered defensively so a
            // corrupted bank degrades to the generic path's honest trap
            CachedCallee::Native { .. } => Ok(DispatchResolved::NotUop),
        }
    }

    /// KSTR over the pool-const table: the cache remembers the (pool,
    /// fiber, index) → object-id mapping from the first load — each heap
    /// interns its own copy of a constant, so a row is only ever served
    /// against the fiber whose store minted it — and a hit serves the id
    /// only after validating that it still names a live string
    /// with the pool constant's exact content — no claim is held, so a
    /// program dropping its last reference still frees the object at the
    /// same moment as without the cache (a dead or recycled id simply
    /// falls back to the full intern). Every load, first or cached,
    /// claims one count for the destination slot, exactly the discipline
    /// a fresh intern follows. The pool-address key is a hint, never an
    /// authority: a dead pool's row can meet a DIFFERENT pool at the
    /// reused address (wrong length, wrong kinds), so the hit path
    /// validates kind as well as content and any mismatch degrades to
    /// the miss path — the kind and index traps stay exactly where an
    /// uncached load raises them.
    fn kstr_load(&mut self, pool: &[Const], kidx: ConstIdx) -> Result<u32, Trap> {
        let pool_id = (pool.as_ptr() as usize, self.current.0);
        if let Some(&Some(id)) = self
            .kstr_cache
            .get(&pool_id)
            .and_then(|ids| ids.get(usize::from(kidx.0)))
        {
            if let Some(Const::Str(text)) = pool.get(usize::from(kidx.0)) {
                if self.objects.is_live_str_with(id, text.as_str()) {
                    self.objects.inc(id)?;
                    return Ok(id);
                }
            }
        }
        let text = match pool.get(usize::from(kidx.0)) {
            Some(Const::Str(text)) => text.as_str(),
            Some(found) => {
                return Err(Trap::PoolKind {
                    kidx: kidx.0,
                    expected: ConstKind::Str,
                    found: found.kind(),
                })
            },
            None => return Err(Trap::PoolIndex { kidx: kidx.0 }),
        };
        let id = self.objects.intern(text);
        let row = self
            .kstr_cache
            .entry(pool_id)
            .or_insert_with(|| vec![None; pool.len()]);
        // a reused address can leave a shorter row from a dead pool:
        // widen before the index (the old row names nothing live)
        if row.len() < pool.len() {
            row.resize(pool.len(), None);
        }
        row[usize::from(kidx.0)] = Some(id);
        Ok(id)
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
        // The frame context (body, code, slots, pool, fp) is derived once
        // per frame transfer, not per instruction: the executing frame and
        // its fp change only at the transfer arms (CALL/CALLT/CALLS, the
        // RET/RETSPAN/HALT resume paths, a control-native switch, the
        // cancel switch), and every other instruction runs against the
        // same immutable body — the frame-context cache. A transfer arm
        // assigns `active`/`pc` and `continue 'transfer`; the pc bounds
        // check stays per instruction.
        'transfer: loop {
            let f = &active.version.functions()[active.func as usize];
            let code = &f.code;
            let slots = f.layout.slots();
            let frame_slots = slots.len();
            let body_len = frame_slots - f.layout.arg_area_len();
            let pool = &f.pool;
            let fp = self.callinfo[self.callinfo.len() - 1].fp;
            loop {
                let idx = pc.0 as usize;
                if idx >= code.len() {
                    let e = Trap::InvalidPc {
                        pc: pc.0,
                        code_len: code_len_u32(code.len()),
                    };
                    return Err((e, pc));
                }
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
                                    continue 'transfer;
                                },
                                None => {
                                    return Ok(DriveOutcome::Died(Value::Tag(PLATFORM_CANCELLED)))
                                },
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
                            if let Some(word) = read_counted(&self.stack, fp, src, slots, body_len)
                                .map_err(|e| (e, pc))?
                            {
                                self.inc_ref(word).map_err(|e| (e, pc))?;
                            }
                            pc.0 += 1;
                            continue;
                        },
                        Ok(RcOp::Dec { src }) => {
                            if let Some(word) = read_counted(&self.stack, fp, src, slots, body_len)
                                .map_err(|e| (e, pc))?
                            {
                                self.dec_ref(word).map_err(|e| (e, pc))?;
                            }
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
                        let obj = self.kstr_load(pool, kidx).map_err(|e| (e, pc))?;
                        let word = RefId::new(self.current, obj);
                        // the load's acquire is the loading frame's claim
                        self.claims[self.current.index()].acquire(word);
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Ref(word))
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
                            read_bool(&self.stack, fp, lhs, slots, body_len)
                                .map_err(|e| (e, pc))?,
                            read_bool(&self.stack, fp, rhs, slots, body_len)
                                .map_err(|e| (e, pc))?,
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
                            read_bool(&self.stack, fp, lhs, slots, body_len)
                                .map_err(|e| (e, pc))?,
                            read_bool(&self.stack, fp, rhs, slots, body_len)
                                .map_err(|e| (e, pc))?,
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
                        let eq = self.str_eq(x, y).map_err(|e| (e, pc))?;
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Bool(eq))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    Uop::NeqStr { dst, lhs, rhs } => {
                        let (x, y) = (
                            read_ref(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_ref(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        let neq = self.str_eq(x, y).map_err(|e| (e, pc))?;
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::Bool(!neq))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    Uop::StrCat { dst, lhs, rhs } => {
                        let (x, y) = (
                            read_ref(&self.stack, fp, lhs, slots, body_len).map_err(|e| (e, pc))?,
                            read_ref(&self.stack, fp, rhs, slots, body_len).map_err(|e| (e, pc))?,
                        );
                        let obj = self.heaps().concat(x, y).map_err(|e| (e, pc))?;
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Ref(RefId::new(self.current, obj)),
                        )
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
                            if let Value::Ref(obj) = *word {
                                // the object adopts the block: one edge
                                // count per Ref capture, no frame claim
                                self.inc_edge(obj).map_err(|e| (e, pc))?;
                            }
                        }
                        let obj = self.objects.alloc_closure(funcref, captures);
                        // the fresh closure's birth count is the caller's
                        // frame claim
                        let closure_word = RefId::new(self.current, obj);
                        self.claims[self.current.index()].acquire(closure_word);
                        write_slot(
                            &mut self.stack,
                            fp,
                            dst,
                            slots,
                            body_len,
                            Value::Ref(closure_word),
                        )
                        .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    Uop::TagTest { dst, src, tag } => {
                        let v =
                            read_tag(&self.stack, fp, src, slots, body_len).map_err(|e| (e, pc))?;
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
                        let v =
                            read_tag(&self.stack, fp, src, slots, body_len).map_err(|e| (e, pc))?;
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
                        let x =
                            read_i64(&self.stack, fp, src, slots, body_len).map_err(|e| (e, pc))?;
                        // precision loss beyond 2^53 is the semantics of the
                        // i64-to-f64 conversion itself
                        #[allow(clippy::cast_precision_loss)]
                        let v = x as f64;
                        write_slot(&mut self.stack, fp, dst, slots, body_len, Value::F64(v))
                            .map_err(|e| (e, pc))?;
                        pc.0 += 1;
                    },
                    Uop::CvtF2I { dst, src } => {
                        let x =
                            read_f64(&self.stack, fp, src, slots, body_len).map_err(|e| (e, pc))?;
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
                        let v = read_value(&self.stack, fp, src, slots, body_len)
                            .map_err(|e| (e, pc))?;
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
                        let caller = std::ptr::from_ref(f) as usize;
                        let resolved = self
                            .resolve_site(pool, caller, pc.0, callee, nargs)
                            .map_err(|e| (e, pc))?;
                        match resolved {
                            Resolved::Uop { version, func } => {
                                let mut st = stack_state!();
                                let t = st
                                    .call(version, func, &f.layout, fp, pc.0 + 1)
                                    .map_err(|e| (e, pc))?;
                                active = Active {
                                    version: t.version,
                                    func: t.func,
                                };
                                pc = CodePc(0);
                                continue 'transfer;
                            },
                            // A native call stays in the caller's frame: the
                            // staged words are read as arguments and the result
                            // overlays argument slot 0 exactly where a RET
                            // would put it (a fallible native's span overlays
                            // slots 0 and 1). The fiber-control natives are the
                            // exception: the dispatcher itself is the switch
                            // [E2] — their bodies never run from a CALL site.
                            Resolved::Native { table, index } => {
                                let nf = &table[index as usize];
                                check_native_call_site(&f.layout, nf).map_err(|e| (e, pc))?;
                                if let Some(control) = nf.control {
                                    match self
                                        .control_switch(control, &active, pc.0, fp, &f.layout)
                                        .map_err(|e| (e, pc))?
                                    {
                                        SwitchOutcome::Entered(a, at) => {
                                            active = a;
                                            pc = at;
                                            continue 'transfer;
                                        },
                                        SwitchOutcome::Bounced => {
                                            pc.0 += 1;
                                        },
                                        SwitchOutcome::Host(event) => return Ok(event),
                                    }
                                } else {
                                    let mut heaps = Heaps::new(
                                        self.current,
                                        &mut self.objects,
                                        &mut self.pool,
                                        &mut self.claims,
                                        self.ctx_live,
                                    );
                                    let result = invoke_native(
                                        nf,
                                        &mut self.stack,
                                        &mut heaps,
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
                        let caller = std::ptr::from_ref(f) as usize;
                        let resolved = self
                            .resolve_site(pool, caller, pc.0, callee, nargs)
                            .map_err(|e| (e, pc))?;
                        match resolved {
                            Resolved::Uop { version, func } => {
                                let mut st = stack_state!();
                                let t = st
                                    .tail_call(version, func, &f.layout, fp)
                                    .map_err(|e| (e, pc))?;
                                active = Active {
                                    version: t.version,
                                    func: t.func,
                                };
                                pc = CodePc(0);
                                continue 'transfer;
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
                            Resolved::Native { table, index } => {
                                let nf = &table[index as usize];
                                if self.callinfo.len() > 1 {
                                    let caller = self.callinfo[self.callinfo.len() - 2].clone();
                                    let caller_fn =
                                        &caller.module.functions()[caller.func as usize];
                                    check_native_overlay(caller_fn.layout.arg_area_len(), nf)
                                        .map_err(|e| (e, pc))?;
                                }
                                let mut heaps = Heaps::new(
                                    self.current,
                                    &mut self.objects,
                                    &mut self.pool,
                                    &mut self.claims,
                                    self.ctx_live,
                                );
                                let result = invoke_native(
                                    nf,
                                    &mut self.stack,
                                    &mut heaps,
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
                                            continue 'transfer;
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
                                        continue 'transfer;
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
                                let (header, block) = stage_closure(
                                    &mut self.objects,
                                    &mut self.pool,
                                    self.current,
                                    &mut self.calls_staging,
                                    obj,
                                )
                                .map_err(|e| (e, pc))?;
                                (header, Some(block))
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
                        if let Some(block) = captures {
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
                        if let Some(block) = captures {
                            let callee_fp = fp + 1 + f.layout.arg_area().start;
                            let capture_owner = self.current;
                            for (index, &value) in block.iter().enumerate() {
                                if let Value::Ref(word) = value {
                                    Self::inc_ref_split(
                                        &mut self.objects,
                                        &mut self.pool,
                                        &mut self.claims,
                                        capture_owner,
                                        capture_owner,
                                        word,
                                    )
                                    .map_err(|e| (e, pc))?;
                                }
                                let slot = u8::try_from(target.layout.params_len() + index)
                                    .expect("capture slot fits u8 after the range check");
                                write_slot(
                                    &mut self.stack,
                                    callee_fp,
                                    slot,
                                    target.layout.slots(),
                                    capture_body_len,
                                    value,
                                )
                                .map_err(|e| (e, pc))?;
                            }
                        }
                        active = Active {
                            version,
                            func: func_id,
                        };
                        pc = CodePc(0);
                        continue 'transfer;
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
                        // A runtime-resolved trait dispatch inside a
                        // generic impl body [H3]: the sentinel FuncRef
                        // names the calling frame's evidence slot, and
                        // the word staged there by the site that entered
                        // the body names the serving member body in the
                        // enclosing module — the dispatch reads it and
                        // transfers exactly like a CALL's uop callee.
                        if module == EVIDENCE_MODULE {
                            let slot = u8::try_from(*func).map_err(|_| {
                                (
                                    Trap::UnknownCallee {
                                        module: module.clone(),
                                        func: *func,
                                    },
                                    pc,
                                )
                            })?;
                            let word = read_value(&self.stack, fp, slot, slots, body_len)
                                .map_err(|e| (e, pc))?;
                            let Value::I64(target_idx) = word else {
                                return Err((
                                    Trap::SlotTypeMismatch {
                                        slot,
                                        expected: SlotType::I64,
                                        actual: word.slot_type(),
                                    },
                                    pc,
                                ));
                            };
                            let version = Arc::clone(&active.version);
                            let index = usize::try_from(target_idx)
                                .ok()
                                .filter(|index| *index < version.functions().len());
                            let Some(index) = index else {
                                return Err((
                                    Trap::UnknownCallee {
                                        module: module.clone(),
                                        func: target_idx.try_into().unwrap_or(u32::MAX),
                                    },
                                    pc,
                                ));
                            };
                            let func_id = u32::try_from(index).expect("guarded above");
                            let target = &version.functions()[func_id as usize];
                            let entry = target.entry.resolve();
                            if entry != func_id as usize {
                                return Err((
                                    Trap::EntryWordMismatch {
                                        resolved: entry,
                                        expected: func_id as usize,
                                    },
                                    pc,
                                ));
                            }
                            if usize::from(nargs) != target.layout.params_len() {
                                return Err((
                                    Trap::ArityMismatch {
                                        expected: target.layout.params_len(),
                                        given: usize::from(nargs),
                                    },
                                    pc,
                                ));
                            }
                            let mut st = stack_state!();
                            let t = st
                                .call(version, func_id, &f.layout, fp, pc.0 + 1)
                                .map_err(|e| (e, pc))?;
                            active = Active {
                                version: t.version,
                                func: t.func,
                            };
                            pc = CodePc(0);
                            continue 'transfer;
                        }
                        // A user trait-method dispatch site [H3]: the
                        // FuncRef names the impl body the lowering resolved
                        // from the call's statically solved instance. The
                        // resolution runs through the per-site inline cache
                        // (the same bank and key CALL/CALLT use, full
                        // lookup as the fallback), so a hot dispatch site
                        // skips the pool fetch and module lookup. The
                        // staged words past `nargs` carry the runtime
                        // evidence a generic impl body claims as its
                        // trailing parameters (one per the body's nested
                        // demands, validated as I64 words), then the
                        // solved declared-instance id riding past them —
                        // validated here, carried rather than
                        // re-dispatched (the instance-keyed cache story
                        // keys off this word later). The body transfers
                        // exactly like a CALL's uop callee: the carried
                        // word sits past the callee's parameters, so the
                        // callee's frame never claims it.
                        let caller_site = std::ptr::from_ref(f) as usize;
                        let resolved = self
                            .resolve_dispatch_site(pool, caller_site, pc.0, callee, nargs)
                            .map_err(|e| (e, pc))?;
                        if let DispatchResolved::Uop {
                            version,
                            func: func_id,
                        } = resolved
                        {
                            let target = &version.functions()[func_id as usize];
                            let params_len = target.layout.params_len();
                            if usize::from(nargs) > params_len {
                                return Err((
                                    Trap::ArityMismatch {
                                        expected: params_len,
                                        given: usize::from(nargs),
                                    },
                                    pc,
                                ));
                            }
                            // the staged evidence parameters the body
                            // claims, then the solved instance word
                            let ev_count = params_len - usize::from(nargs);
                            let ev_body_len = f.layout.slots().len() - f.layout.arg_area_len();
                            for i in 0..=ev_count {
                                let ev_slot = u8::try_from(
                                    f.layout.arg_area().start + usize::from(nargs) + i,
                                )
                                .expect("slot index fits u8");
                                let word = read_value(&self.stack, fp, ev_slot, slots, ev_body_len)
                                    .map_err(|e| (e, pc))?;
                                if word.slot_type() != SlotType::I64 {
                                    return Err((
                                        Trap::SlotTypeMismatch {
                                            slot: ev_slot,
                                            expected: SlotType::I64,
                                            actual: word.slot_type(),
                                        },
                                        pc,
                                    ));
                                }
                            }
                            let mut st = stack_state!();
                            let t = st
                                .call(version, func_id, &f.layout, fp, pc.0 + 1)
                                .map_err(|e| (e, pc))?;
                            active = Active {
                                version: t.version,
                                func: t.func,
                            };
                            pc = CodePc(0);
                            continue 'transfer;
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
                                    caller_area: u16::try_from(area)
                                        .expect("argument areas fit u16"),
                                },
                                pc,
                            ));
                        }
                        let body_len = f.layout.slots().len() - f.layout.arg_area_len();
                        let start = f.layout.arg_area().start;
                        // the staged arguments and evidence words copy inline
                        // (generic call sites sit well under the inline width),
                        // with the heap buffer the oversized fallback
                        let mut inline = [Value::I64(0); INLINE_WORDS];
                        let mut heap = Vec::new();
                        let args: &[Value] = if usize::from(nargs) <= INLINE_WORDS {
                            for (i, word) in inline.iter_mut().enumerate().take(usize::from(nargs))
                            {
                                let slot = u8::try_from(start + i).expect("slot index fits u8");
                                *word = read_value(&self.stack, fp, slot, slots, body_len)
                                    .map_err(|e| (e, pc))?;
                            }
                            &inline[..usize::from(nargs)]
                        } else {
                            heap.reserve(usize::from(nargs));
                            for i in 0..usize::from(nargs) {
                                let slot = u8::try_from(start + i).expect("slot index fits u8");
                                heap.push(
                                    read_value(&self.stack, fp, slot, slots, body_len)
                                        .map_err(|e| (e, pc))?,
                                );
                            }
                            &heap
                        };
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
                        let mut ev_inline = [Value::I64(0); INLINE_WORDS];
                        let mut ev_heap = Vec::new();
                        let evidence: &[Value] = if usize::from(g.evidence) <= INLINE_WORDS {
                            for (i, word) in ev_inline
                                .iter_mut()
                                .enumerate()
                                .take(usize::from(g.evidence))
                            {
                                let slot = u8::try_from(start + usize::from(nargs) + i)
                                    .expect("slot fits u8");
                                *word = read_value(&self.stack, fp, slot, slots, body_len)
                                    .map_err(|e| (e, pc))?;
                            }
                            &ev_inline[..usize::from(g.evidence)]
                        } else {
                            ev_heap.reserve(usize::from(g.evidence));
                            for i in 0..usize::from(g.evidence) {
                                let slot = u8::try_from(start + usize::from(nargs) + i)
                                    .expect("slot fits u8");
                                ev_heap.push(
                                    read_value(&self.stack, fp, slot, slots, body_len)
                                        .map_err(|e| (e, pc))?,
                                );
                            }
                            &ev_heap
                        };
                        // the body takes the whole VM: the pure bodies split
                        // off the heap and sink themselves, and the
                        // lambda-taking combinators reenter the dispatcher
                        // through `call_hosted`
                        let result = (g.func)(self, evidence, args).map_err(|e| (e, pc))?;
                        overlay_native(result, &mut self.stack, &f.layout, fp);
                        pc.0 += 1;
                    },
                    Uop::Calltg { callee, nargs } => {
                        // CALLTG, the dispatch tail call [H2's guarantee
                        // closure]: the dispatch resolution is CALLG's own
                        // — the sentinel evidence FuncRef, a trait site's
                        // impl body, or a generic builtin's ONE body — and
                        // the transfer is CALLT's: the staged words slide
                        // into the reused frame's parameters per the ABI
                        // slide plan, the metadata word rewrites in place,
                        // and the stack never grows at any depth. The
                        // evidence/instance words past the callee's
                        // parameters ride the transfer as count-free dead
                        // words the callee never claims, so the slide moves
                        // exactly the parameter count.
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
                        // the sentinel names the frame's evidence slot: the
                        // target is runtime data the entering site staged,
                        // so this path reads the slot every time and never
                        // touches the cache
                        if module == EVIDENCE_MODULE {
                            let slot = u8::try_from(*func).map_err(|_| {
                                (
                                    Trap::UnknownCallee {
                                        module: module.clone(),
                                        func: *func,
                                    },
                                    pc,
                                )
                            })?;
                            let word = read_value(&self.stack, fp, slot, slots, body_len)
                                .map_err(|e| (e, pc))?;
                            let Value::I64(target_idx) = word else {
                                return Err((
                                    Trap::SlotTypeMismatch {
                                        slot,
                                        expected: SlotType::I64,
                                        actual: word.slot_type(),
                                    },
                                    pc,
                                ));
                            };
                            let version = Arc::clone(&active.version);
                            let index = usize::try_from(target_idx)
                                .ok()
                                .filter(|index| *index < version.functions().len());
                            let Some(index) = index else {
                                return Err((
                                    Trap::UnknownCallee {
                                        module: module.clone(),
                                        func: target_idx.try_into().unwrap_or(u32::MAX),
                                    },
                                    pc,
                                ));
                            };
                            let func_id = u32::try_from(index).expect("guarded above");
                            let target = &version.functions()[func_id as usize];
                            let entry = target.entry.resolve();
                            if entry != func_id as usize {
                                return Err((
                                    Trap::EntryWordMismatch {
                                        resolved: entry,
                                        expected: func_id as usize,
                                    },
                                    pc,
                                ));
                            }
                            if usize::from(nargs) != target.layout.params_len() {
                                return Err((
                                    Trap::ArityMismatch {
                                        expected: target.layout.params_len(),
                                        given: usize::from(nargs),
                                    },
                                    pc,
                                ));
                            }
                            let mut st = stack_state!();
                            let t = st
                                .tail_call(version, func_id, &f.layout, fp)
                                .map_err(|e| (e, pc))?;
                            active = Active {
                                version: t.version,
                                func: t.func,
                            };
                            pc = CodePc(0);
                            continue 'transfer;
                        }
                        // a trait site's impl body: the per-site inline
                        // cache keys the callee (full lookup as the
                        // fallback), the staged evidence words validate as
                        // I64 before the slide reads them, and the transfer
                        // reuses this frame
                        let caller_site = std::ptr::from_ref(f) as usize;
                        let resolved = self
                            .resolve_dispatch_site(pool, caller_site, pc.0, callee, nargs)
                            .map_err(|e| (e, pc))?;
                        if let DispatchResolved::Uop {
                            version,
                            func: func_id,
                        } = resolved
                        {
                            let params_len =
                                version.functions()[func_id as usize].layout.params_len();
                            // the staged evidence parameters the body
                            // claims, then the solved instance word — both
                            // count-free I64 constants riding past the
                            // slide's width
                            let ev_count = params_len - usize::from(nargs);
                            let ev_body_len = f.layout.slots().len() - f.layout.arg_area_len();
                            for i in 0..=ev_count {
                                let ev_slot = u8::try_from(
                                    f.layout.arg_area().start + usize::from(nargs) + i,
                                )
                                .expect("slot index fits u8");
                                let word = read_value(&self.stack, fp, ev_slot, slots, ev_body_len)
                                    .map_err(|e| (e, pc))?;
                                if word.slot_type() != SlotType::I64 {
                                    return Err((
                                        Trap::SlotTypeMismatch {
                                            slot: ev_slot,
                                            expected: SlotType::I64,
                                            actual: word.slot_type(),
                                        },
                                        pc,
                                    ));
                                }
                            }
                            let mut st = stack_state!();
                            let t = st
                                .tail_call(version, func_id, &f.layout, fp)
                                .map_err(|e| (e, pc))?;
                            active = Active {
                                version: t.version,
                                func: t.func,
                            };
                            pc = CodePc(0);
                            continue 'transfer;
                        }
                        // a generic builtin's ONE body runs inside the
                        // instruction and the frame returns its result —
                        // the CALLT-to-native discipline: the span result
                        // lands on the ORIGINAL caller's overlay words, and
                        // at the root frame only the head word survives
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
                        if self.callinfo.len() > 1 {
                            let caller = self.callinfo[self.callinfo.len() - 2].clone();
                            let caller_fn = &caller.module.functions()[caller.func as usize];
                            let caller_area = caller_fn.layout.arg_area_len();
                            if usize::from(g.result_words) > caller_area {
                                return Err((
                                    Trap::RetSpanTooWide {
                                        span: u16::from(g.result_words),
                                        caller_area: u16::try_from(caller_area)
                                            .expect("argument areas fit u16"),
                                    },
                                    pc,
                                ));
                            }
                        }
                        let start = f.layout.arg_area().start;
                        let mut inline = [Value::I64(0); INLINE_WORDS];
                        let mut heap = Vec::new();
                        let args: &[Value] = if usize::from(nargs) <= INLINE_WORDS {
                            for (i, word) in inline.iter_mut().enumerate().take(usize::from(nargs))
                            {
                                let slot = u8::try_from(start + i).expect("slot index fits u8");
                                *word = read_value(&self.stack, fp, slot, slots, body_len)
                                    .map_err(|e| (e, pc))?;
                            }
                            &inline[..usize::from(nargs)]
                        } else {
                            heap.reserve(usize::from(nargs));
                            for i in 0..usize::from(nargs) {
                                let slot = u8::try_from(start + i).expect("slot index fits u8");
                                heap.push(
                                    read_value(&self.stack, fp, slot, slots, body_len)
                                        .map_err(|e| (e, pc))?,
                                );
                            }
                            &heap
                        };
                        for (index, (&value, &want)) in args.iter().zip(g.params).enumerate() {
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
                        let mut ev_inline = [Value::I64(0); INLINE_WORDS];
                        let mut ev_heap = Vec::new();
                        let evidence: &[Value] = if usize::from(g.evidence) <= INLINE_WORDS {
                            for (i, word) in ev_inline
                                .iter_mut()
                                .enumerate()
                                .take(usize::from(g.evidence))
                            {
                                let slot = u8::try_from(start + usize::from(nargs) + i)
                                    .expect("slot fits u8");
                                *word = read_value(&self.stack, fp, slot, slots, body_len)
                                    .map_err(|e| (e, pc))?;
                            }
                            &ev_inline[..usize::from(g.evidence)]
                        } else {
                            ev_heap.reserve(usize::from(g.evidence));
                            for i in 0..usize::from(g.evidence) {
                                let slot = u8::try_from(start + usize::from(nargs) + i)
                                    .expect("slot fits u8");
                                ev_heap.push(
                                    read_value(&self.stack, fp, slot, slots, body_len)
                                        .map_err(|e| (e, pc))?,
                                );
                            }
                            &ev_heap
                        };
                        let result = (g.func)(self, evidence, args).map_err(|e| (e, pc))?;
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
                                    continue 'transfer;
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
                                continue 'transfer;
                            },
                        }
                    },
                    Uop::Ret { src } => {
                        let value = read_value(&self.stack, fp, src, slots, body_len)
                            .map_err(|e| (e, pc))?;
                        let mut st = stack_state!();
                        match st.ret(value, fp, boundary).map_err(|e| (e, pc))? {
                            RetOutcome::Root(v) => match self.complete_current(v) {
                                Some((a, at)) => {
                                    active = a;
                                    pc = at;
                                    continue 'transfer;
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
                                continue 'transfer;
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
                                    continue 'transfer;
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
                                continue 'transfer;
                            },
                        }
                    },
                    Uop::Halt { slot } => {
                        let value = read_value(&self.stack, fp, slot, slots, body_len)
                            .map_err(|e| (e, pc))?;
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
                                continue 'transfer;
                            },
                            None => return Ok(DriveOutcome::Died(value)),
                        }
                    },
                }
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
