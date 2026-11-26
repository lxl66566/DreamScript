//! # The fork-join combinators (indices 47-48)
//!
//! The pooled combinators' engine seams, the ds-source spellings
//! retired from the user surface until the fork-join admission round
//! (the private externs in the @std bodied member name these natives).
//! `fork_map` (the parked-CALLG
//! fork-join): the combinator body is
//! spawn-only — fork is one spawned child per element applying the
//! callable under a fresh nursery, the callable's own function entered
//! with the element's whole inline span, the spawn copy-in acquiring
//! one count per `Ref` word for the child's frame — and the join parks
//! the CALLG site through the interlock: the drive seam
//! (`vm::DriveSeam::Fork` over `ForkStage`) carries the assembly
//! stage, the drive runs the children in spawn order so the answers
//! assemble in input order regardless of completion order
//! (deterministic replay by construction), and the assembled result
//! list delivers onto the parked call site. An element may park at a
//! yield mid-flight where the hosted `map` loop refuses — the worklist
//! bounces the parked child back in. The first failure cancels
//! the siblings at their next preemption point and re-raises at the
//! call site — exactly a spawn-body failure at its enclosing
//! boundary. The completion channel is whole-span (a `FrameAnswer`):
//! the callable's answer rides it one word or whole (a multi-word
//! RETSPAN or tail-native answer), its width the callable's
//! checker-proved signature, and the seam assembles each answer's
//! words onto the fresh result list, the first push fixing the list's
//! element stride (an answering width the checker proves). A closure
//! callable runs the same fork — the capture block staged per spawn
//! into the child's entry frame (the CALLS copy-in discipline), one
//! acquired count per `Ref` capture. Nesting (a par_map inside a
//! par_map element) parks the element's fiber at its own CALLG site,
//! the drive frames stacking on the interlock's heap stack — depth
//! costs heap, never the Rust stack.
//!
//! `par_reduce` (the same park over the fold): the fold's left
//! data dependency chains the children — each element's application is
//! one spawned child entered with the running accumulator's whole word
//! span and the element's span, the seam's chain state carrying the
//! fold between steps (a fresh nursery per step, the consumed slot
//! recycled) — so the reduction applies strictly in input order, the
//! deterministic replayable spelling whatever a later scheduler does
//! (an associative split would reorder float sums, so it waits on an
//! associativity proof). The fold type is the call's own fact: the
//! init's whole span stages between the list and the callable (the
//! interior-span discipline), the callable's checker-proved answer
//! width matching it, and the final accumulator delivers whole onto
//! the parked call site. The accumulator's counts follow `reduce`'s
//! owned discipline (the init borrowed until the first answer adopts,
//! each delivered answer owned by the joiner, each replaced word
//! dec'd at adoption); the callable is a scalar `Fn` word or a closure
//! object's `Ref` (the capture block staged per spawn exactly
//! par_map's) over any fold width — and the first failure re-raises at
//! the call site with the owned accumulator released.

use super::{generic::fn_arg, *};
use crate::{
    fiber::FiberId,
    nursery::NurseryId,
    value::{FuncRef, RefId},
    vm::Vm,
};

/// The shared body of the control natives: a `CALL` site never reaches it
/// (the dispatcher intercepts and switches), so the only reachable call is
/// a `CALLT`, where no switch is possible — the bounce answer.
#[allow(clippy::unnecessary_wraps)]
pub(super) fn fiber_control_bounce_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    _args: &[Value],
) -> Result<NativeResult, Trap> {
    Ok(NativeResult::Word(Value::Tag(PLATFORM_NONE)))
}

/// The fork-join callable word, forked per element: the scalar `Fn`
/// names the callable's own function, a closure object's `Ref` its body
/// (the capture block staged per spawn into the child's entry frame,
/// exactly the CALLS copy-in).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ForkCallable {
    Scalar(FuncRef),
    Closure(RefId),
}

/// The fork-join drive seam's staged state — the assembly/fold stage
/// the join runs after the spawn phase parks the CALLG site.
/// `par_map`'s answers assemble in spawn order onto the fresh result
/// list at the seam's consumption; `par_reduce`'s chain state carries
/// the fold between the per-element joins (the element snapshot, the
/// running accumulator span with its ownership flag, and the next
/// element's offset).
#[derive(Clone, Debug, PartialEq)]
pub enum ForkStage {
    /// Each completed answer's whole word span pushes onto `result` as
    /// one element, the first push fixing the list's stride.
    Map {
        /// The fresh result list (empty until the seam assembles).
        result: RefId,
    },
    /// The next element's child spawns with the running accumulator
    /// span then `elements[at..]` as its entry arguments, the adopted
    /// answer replacing `acc` (`owned` marking the adoption).
    Reduce {
        /// The input list's element snapshot, `stride` words per
        /// element.
        elements: Vec<Value>,
        /// The element word width.
        stride: usize,
        /// The callable spawned per step.
        callable: ForkCallable,
        /// The running accumulator's whole word span.
        acc: Vec<Value>,
        /// Whether `acc` is the seam's own (an adopted answer) or the
        /// caller's borrowed init.
        owned: bool,
        /// The next element's word offset.
        at: usize,
    },
}

impl ForkStage {
    /// The answer's word width the parked call site's overlay must
    /// hold (the map's one result word, the fold's whole span).
    pub(crate) fn answer_words(&self) -> usize {
        match self {
            Self::Map { .. } => 1,
            Self::Reduce { acc, .. } => acc.len(),
        }
    }
}

/// Whether the generic builtin a dispatch site addresses is a
/// fork-join combinator (a spawn-only body that parks the site).
#[must_use]
pub fn is_fork_join(module: &str, index: u32) -> bool {
    module == BUILTIN_MODULE && (index == BUILTIN_PAR_MAP || index == BUILTIN_PAR_REDUCE)
}

/// Reads the callable word at argument `index` (the callable-defaulted
/// `Fn` slot admits both spellings).
pub(super) fn fork_callable(args: &[Value], index: usize) -> Result<ForkCallable, Trap> {
    match fn_arg(args, index)? {
        Value::Fn(fref) => Ok(ForkCallable::Scalar(fref)),
        Value::Ref(word) => Ok(ForkCallable::Closure(word)),
        _ => unreachable!("fn_arg admits only the Fn and Ref words"),
    }
}

/// Spawns one fork child applying the callable to `element` — the
/// element's whole word span as the entry arguments either way.
pub(crate) fn fork_spawn(
    vm: &mut Vm,
    nursery: NurseryId,
    callable: ForkCallable,
    element: &[Value],
) -> Result<FiberId, Trap> {
    match callable {
        ForkCallable::Scalar(fref) => vm.spawn_ref_into(Some(nursery), fref, element),
        ForkCallable::Closure(word) => vm.spawn_closure_into(Some(nursery), word, element),
    }
}

/// `par_map`'s spawn phase: fork is
/// one spawned child per element applying the callable under a fresh
/// nursery — the callable's own function entered with the element's
/// whole inline span, the spawn copy-in acquiring one count per `Ref`
/// word for the child's frame — and the join parks the CALLG site
/// through the interlock: the body answers the park request, the drive
/// seam carries the assembly stage, and the answers assemble in spawn
/// order onto the fresh result list at the seam's consumption (each
/// answer's whole word span pushing as one element, the first push
/// fixing the list's stride, the push taking the list's own edge count
/// per `Ref` word, the child's delivered count ending at the joiner's
/// single dec — one per `Ref` word, the cross-heap channel). The
/// first failure in spawn-drive order cancels the siblings at their
/// next preemption point and re-raises at the parked call site,
/// exactly a spawn-body failure at its enclosing boundary; a failed
/// spawn tears the already-spawned children down under the cancel
/// discipline before its trap surfaces. An empty input answers the
/// empty list without parking (no child stands to join). The callable
/// is a scalar `Fn` word or a closure object's `Ref` — a closure
/// callable's capture block is staged per spawn into the child's entry
/// frame past the element's argument span, one acquired count per
/// `Ref` capture claimed for the child (the CALLS copy-in discipline),
/// so a forked lambda is one shape with a called one; a `Ref` word
/// naming no closure object refuses at the spawn.
pub(super) fn par_map_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // the callable word: a scalar `Fn` names the callable's own
    // function; a closure object's `Ref` names its body, the capture
    // block staged per spawn into the child's entry frame
    let callable = fork_callable(args, 1)?;
    let list = ref_arg(args, 0)?;
    let (elems, stride) = {
        let (mut heaps, _) = vm.split();
        let (elems, stride) = heaps.list_at(list)?;
        (elems.to_vec(), stride.max(1))
    };
    let res = {
        let (mut heaps, _) = vm.split();
        heaps.new_list_id(Vec::new())
    };
    if elems.is_empty() {
        return Ok(NativeResult::Word(Value::Ref(res)));
    }
    let nursery = vm.open_fresh_nursery();
    for at in (0..elems.len()).step_by(stride) {
        if let Err(trap) = fork_spawn(vm, nursery, callable, &elems[at..at + stride]) {
            vm.teardown_fork_nursery(nursery)?;
            let (mut heaps, _) = vm.split();
            heaps.dec(res)?;
            return Err(trap);
        }
    }
    Ok(NativeResult::Fork {
        nursery,
        stage: ForkStage::Map { result: res },
    })
}

/// `par_reduce`'s spawn phase: the
/// fold's left data dependency chains the children — each element's
/// application is one spawned child entered with the running
/// accumulator's whole word span and the element's span (the
/// callable's own function, the spawn copy-in acquiring one count per
/// `Ref` word for the child's frame), the seam's chain state carrying
/// the fold between the per-step joins — so the reduction applies
/// strictly in input order, the replayable spelling whatever a later
/// scheduler does. The fold type is the call's own fact: the init's
/// whole word span stages between the list and the callable (the
/// interior-span discipline), the callable's checker-proved answer
/// width matching it, and the final accumulator delivers whole onto
/// the parked call site. The accumulator's counts follow `reduce`'s
/// owned discipline: the init stays borrowed until the first delivered
/// answer adopts (the child's completion count landing on the joiner —
/// the cross-heap channel), each replaced owned word dec'd at
/// adoption; an empty input answers the init, inc'd to own it. The
/// callable is a scalar `Fn` word or a closure object's `Ref` (the
/// capture block staged per spawn exactly par_map's).
pub(super) fn par_reduce_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // the callable word rides at the staged init span's end: a scalar
    // `Fn` or a closure object's `Ref`, the capture block staged per
    // spawn exactly par_map's
    let callable = fork_callable(args, args.len() - 1)?;
    let list = ref_arg(args, 0)?;
    let (elems, stride) = {
        let (mut heaps, _) = vm.split();
        let (elems, stride) = heaps.list_at(list)?;
        (elems.to_vec(), stride.max(1))
    };
    // the fold's whole span: the init's words stage between the list
    // and the callable, its width the call site's fact
    let acc: Vec<Value> = args[1..args.len() - 1].to_vec();
    if elems.is_empty() {
        // an empty input answers the init, inc'd to own it — no child
        // stands to join, so no park
        for word in &acc {
            if let Value::Ref(id) = *word {
                let (mut heaps, _) = vm.split();
                heaps.inc(id)?;
            }
        }
        return Ok(NativeResult::Total { words: acc });
    }
    let mut child_args = Vec::with_capacity(acc.len() + stride);
    child_args.extend_from_slice(&acc);
    child_args.extend_from_slice(&elems[..stride]);
    let nursery = vm.open_fresh_nursery();
    if let Err(trap) = fork_spawn(vm, nursery, callable, &child_args) {
        // a refused spawn's child may be half-built: tear the step's
        // nursery down under the cancel discipline; the init stays
        // borrowed (nothing owned to release)
        vm.teardown_fork_nursery(nursery)?;
        return Err(trap);
    }
    Ok(NativeResult::Fork {
        nursery,
        stage: ForkStage::Reduce {
            elements: elems,
            stride,
            callable,
            acc,
            owned: false,
            at: 0,
        },
    })
}

/// Releases a park request the dispatcher refused after the spawn
/// phase ran (a whole-span answer wider than the parked site's
/// overlay): the spawned children torn down under the cancel
/// discipline, then the staged state's own words released (the result
/// list's count, the owned accumulator's span).
pub(crate) fn fork_abandon(vm: &mut Vm, nursery: NurseryId, stage: ForkStage) -> Result<(), Trap> {
    vm.teardown_fork_nursery(nursery)?;
    let (mut heaps, _) = vm.split();
    match stage {
        ForkStage::Map { result } => heaps.dec(result),
        ForkStage::Reduce { acc, owned, .. } => release_owned_span(&mut heaps, &acc, owned),
    }
}

/// Decs each `Ref` word of an owned span (a fold's replaced
/// accumulator) through the given router; a borrowed span (`owned`
/// false, the init before the first adoption) skips every word — its
/// counts belong to the caller.
pub(crate) fn release_owned_span(
    heaps: &mut Heaps<'_>,
    words: &[Value],
    owned: bool,
) -> Result<(), Trap> {
    if !owned {
        return Ok(());
    }
    for word in words {
        if let Value::Ref(id) = *word {
            heaps.dec(id)?;
        }
    }
    Ok(())
}

/// The intercepted natives' placeholder body (the channel steps and the
/// timer race): the dispatcher intercepts every `CALL` and `CALLT` site
/// ahead of the body, so a body that runs names hand-assembled code that
/// bypassed the interception — the same refusal the control natives'
/// bounce carries, as a trap.
pub(super) fn chan_refuse_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    _args: &[Value],
) -> Result<NativeResult, Trap> {
    Err(Trap::ReentrancyRefused)
}
