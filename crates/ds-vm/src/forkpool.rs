//! The fork-join worker pool [E3, the 2026-11-15 sendability ruling]:
//! a worker thread owns whole fibers and never shares them. The host
//! drive extracts a fork child from its pool — the fiber (segment,
//! arena, heap store, call chain) with its claim ledger beside it — and
//! hands the cargo to a youpipe scoped task; the worker drives the
//! fiber to death on a worker VM over a `Send + Sync` code snapshot of
//! the version table (the same rows, ids, and slots the host resolves
//! through), and the cargo comes home before any death walk, ledger
//! write, or delivery runs — all of those fire on the host exactly as
//! the sequential interlock drive runs them. The transfer is ownership
//! only: the host pool slot stands vacated for the drive's duration, so
//! the non-atomic counts and block headers stay confined [C4] — the
//! same contract ds-rt's `Heap`/`Arena` and `Objects` carry their
//! `unsafe impl Send` under.
//!
//! What is per-worker by ruling: the CALL-site inline caches, the KSTR
//! intern rows, and the deinit-hook directories of the worker VM (each
//! derived from immutable code, correctness never depending on cache
//! state [H3]), and the nurseries (a fresh ledger stub per cargo, real
//! worker-local nurseries for a nested combinator). What is refused:
//! the world surface never reaches a worker (the snapshot strips the
//! platform tables, so a world call refuses with the standard
//! unresolved-callee trap — the pool's purity contract), and a child
//! whose entry frame holds a `Ref` word (a foreign heap the worker
//! cannot route) never pools — the host drive takes the sequential
//! interlock for it, byte-identical by construction.

use std::{
    cell::RefCell,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{
    claims::ClaimTable,
    fiber::{Fiber, FiberId, FiberState},
    module::CodeSnapshot,
    nursery::{ChildOutcome, Nursery, NurseryId},
    value::{Value, Word},
    vm::{DriveOutcome, Vm},
};

/// One pool drive's identity: worker VMs are cached per thread and
/// re-keyed whenever the drive moves, so a cached context never serves a
/// second drive against a stale snapshot.
static DRIVE_ID: AtomicU64 = AtomicU64::new(0);

/// The next drive's identity.
pub(crate) fn next_drive_id() -> u64 {
    DRIVE_ID.fetch_add(1, Ordering::Relaxed) + 1
}

thread_local! {
    /// The per-thread worker VM cache: one execution context per youpipe
    /// worker thread, rebuilt when the drive identity moves (the snapshot
    /// is per-drive). The host thread's own slot serves the tasks youpipe
    /// steals back to the joining thread — a distinct context from the
    /// host VM being driven, so no reentrancy touches it.
    static WORKER_VM: RefCell<Option<(u64, Vm)>> = const { RefCell::new(None) };
}

/// The ownership-transfer unit: one whole fork child — its fiber, its
/// claim ledger, its pool identity, and the host nursery it reports its
/// death into. `Send` is compiler-proved over the substrate (`Objects`,
/// `Heap`, `Arena`); nothing in the cargo aliases host state, the pool
/// slot having been vacated.
pub(crate) struct ForkCargo {
    pub(crate) fiber: Fiber,
    pub(crate) claims: ClaimTable,
    pub(crate) child: FiberId,
}

/// The cargo's homecoming: the fiber as the worker left it (Dead for a
/// completed run, frames kept for a trapped one — the host runs the
/// death walk), the ledger, and the outcome the worker's stub recorded.
pub(crate) struct ForkReturn {
    pub(crate) fiber: Fiber,
    pub(crate) claims: ClaimTable,
    pub(crate) child: FiberId,
    pub(crate) outcome: Option<ChildOutcome>,
}

/// Whether a fresh fork child may pool: its entry frame's whole segment
/// must hold exclusively scalar words — a `Ref` word would name a heap
/// the worker cannot route (the spawn-side count crossings stay
/// host-side, but any in-flight op on the word is a foreign dec), and an
/// arena word cannot exist at spawn. A fresh child's segment words are
/// exactly its parameters and captures (the rest zero-initialized), so
/// the scan is the sendability gate.
pub(crate) fn poolable(fiber: &Fiber) -> bool {
    (0..fiber.segment.len()).all(|at| {
        !matches!(
            fiber.segment.get(at),
            Some(Word::Slot(Value::Ref(_) | Value::Arena(_)))
        )
    })
}

/// Runs one cargo on this thread's worker VM (building or re-keying the
/// context against the drive's snapshot first): place the fiber at its
/// own pool index with a fresh ledger stub, drive it to death — the
/// sequential drive's inner loop minus the cancel flag (a pooled child
/// has no siblings on the worker; a nested combinator's failure raises
/// its own worker-local nursery's flag, never the stub's) — then stash
/// and extract the cargo for the ride home.
pub(crate) fn exec_cargo(
    cargo: ForkCargo,
    drive: u64,
    code: &CodeSnapshot,
    limit: usize,
) -> ForkReturn {
    WORKER_VM.with_borrow_mut(|slot| {
        if slot.as_ref().is_none_or(|(id, _)| *id != drive) {
            *slot = Some((drive, Vm::with_limit(code.to_registry(), limit)));
        }
        let vm = &mut slot.as_mut().expect("the slot was just keyed").1;
        run_on_worker(vm, cargo)
    })
}

/// The worker-side drive of one cargo. The placement pads the worker
/// pool to the child's index (empty fibers, never dead, so nothing
/// reuses the slot) and the nurseries to a fresh stub that takes the
/// child's record; the fiber's nursery link is pointed at the stub — the
/// host restores its own at reinsertion.
fn run_on_worker(vm: &mut Vm, cargo: ForkCargo) -> ForkReturn {
    let child = cargo.child;
    while vm.pool.len() <= child.index() {
        let id = FiberId(u32::try_from(vm.pool.len()).expect("pool index fits u32"));
        vm.pool.push(Fiber::empty(id));
    }
    while vm.claims.len() < vm.pool.len() {
        let id = FiberId(u32::try_from(vm.claims.len()).expect("pool index fits u32"));
        vm.claims.push(ClaimTable::new(id));
    }
    let stub = NurseryId(u32::try_from(vm.nurseries.len()).expect("nursery count fits u32"));
    // the stub owns nothing: it exists to take the child's records, and a
    // worker never runs a release door over its pool
    vm.nurseries.push(Nursery::open(None));
    vm.nurseries[stub.index()].push_child(child);
    let mut fiber = cargo.fiber;
    fiber.nursery = Some(stub);
    vm.pool[child.index()] = fiber;
    vm.claims[child.index()] = cargo.claims;
    loop {
        if vm.pool[child.index()].state == FiberState::Dead || !vm.pool[child.index()].resumable() {
            break;
        }
        let (active, pc) = vm.switch_in(child, Value::Tag(crate::native::PLATFORM_NONE));
        match vm.execute_host_step(active, pc, usize::MAX) {
            // a quiet end: the fiber died (the loop's head sees Dead) or
            // parked at the host (the bounce back in, exactly the
            // sequential drive's re-entry)
            Ok(DriveOutcome::Died(_) | DriveOutcome::Yielded(_)) => {},
            Ok(DriveOutcome::Frame(_)) => {
                unreachable!("a pooled child hosts no reentry frame")
            },
            Ok(DriveOutcome::Interlock(_)) => {
                unreachable!("execute_host_step drains interlocks before returning")
            },
            Err((trap, _at)) => {
                // the trapped child keeps its frames: the host's death
                // walk fires the hooks and drains the ledger, exactly
                // the sequential trapped-child tail
                vm.host_release();
                vm.trap_child(child, &trap);
                break;
            },
        }
    }
    let outcome = vm.nurseries[stub.index()].outcome(child);
    vm.host_release();
    let fiber = std::mem::replace(&mut vm.pool[child.index()], Fiber::empty(child));
    let claims = std::mem::replace(&mut vm.claims[child.index()], ClaimTable::new(child));
    ForkReturn {
        fiber,
        claims,
        child,
        outcome,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ruling's mechanical face: everything that moves is `Send`
    /// where the compiler can prove it — the cargo over the `Objects`
    /// `Send` substrate, and the homecoming with the outcome.
    #[test]
    fn the_cargo_moves_are_send() {
        fn assert_send<T: Send>() {}
        assert_send::<ForkCargo>();
        assert_send::<ForkReturn>();
        assert_send::<Fiber>();
        assert_send::<ClaimTable>();
    }
}
