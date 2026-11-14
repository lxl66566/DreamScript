//! Ruling-evidence pins for the stage-two per-fiber-heap tables (the
//! memory endgame arc contract). Each test
//! documents substrate behavior the arc's gates demand — the landed
//! rulings' pins, not proposals.

use core::mem::size_of;

use ds_vm::{
    Const, ConstFuncRef, FIBER_ROOT, FunctionDef, ModuleRegistry, ModuleVersion, Value, Vm, Word,
    abi::{ConstIdx, FrameLayout, Inst, RcOp, SlotType, uop::Uop},
    assemble,
};

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn def(name: &str, slots: &[SlotType], params: usize, args: usize, body: &[Inst]) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout"),
        pool: Vec::new(),
        code: assemble(body).expect("fixture body encodes"),
    }
}

/// The frame word is 16 bytes today (the i64/f64 payloads force it), so a
/// `Ref` payload can widen to (fiber id, object id) without growing any
/// frame slot — the owner-encoding candidate's zero-growth out from a
/// bit-split ceiling.
#[test]
fn the_frame_word_holds_a_widened_ref_payload() {
    assert_eq!(size_of::<Value>(), 16);
    assert_eq!(size_of::<Word>(), 16);
}

/// The intern map rides each fiber's store: two drives of the same
/// constant on the SAME fiber share one object (the second load's intern
/// hit — the per-fiber interning ruling keeps this within one heap), and
/// each host-held delivered word keeps its own count — the load acquires,
/// the RET crossing mutates nothing, and the host never releases. A
/// different fiber's drive interns its own copy (the KSTR pin below).
#[test]
fn the_intern_map_serves_every_drive_one_counted_object() {
    let main = def("main", &[SlotType::Ref, SlotType::I64], 0, 0, &[
        Uop::Funcf { frame_slots: 2 }.into(),
        Uop::Kstr { dst: 0, kidx: k(0) }.into(),
        Uop::Ret { src: 0 }.into(),
    ]);
    let mut main = main;
    main.pool = vec![Const::Str("shared".into())];
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("oe", vec![main]).expect("valid fixture module"));
    let mut vm = Vm::new(reg);
    let first = vm
        .run("oe", "main", &[])
        .expect("the first drive completes");
    let Value::Ref(id1) = first else {
        panic!("a string reference, got {first:?}");
    };
    assert_eq!(vm.objects().rc_at(id1.id()), Ok(1));
    let second = vm
        .run("oe", "main", &[])
        .expect("the second drive completes");
    let Value::Ref(id2) = second else {
        panic!("a string reference, got {second:?}");
    };
    assert_eq!(
        id2, id1,
        "one VM-global intern map: the hit path reuses the object"
    );
    assert_eq!(
        vm.objects().rc_at(id1.id()),
        Ok(2),
        "each host-held delivered word keeps its own acquired count"
    );
    assert_eq!(vm.objects().str_at(id1.id()), Ok("shared"));
}

/// A dropped KSTR constant fully drains within its drive: the dec to zero
/// frees the object and unbinds the intern entry, so the next drive re-interns
/// a fresh object instead of resurrecting the freed id. This is the
/// ephemeral-constant churn the KSTR tables cite (loop-body fragments die
/// with their frames, so steady-state loads see miss-heavy or fully-miss
/// intern traffic).
#[test]
fn a_dropped_kstr_constant_fully_drains() {
    let cycle = def("cycle", &[SlotType::Ref, SlotType::I64], 0, 0, &[
        Uop::Funcf { frame_slots: 2 }.into(),
        Uop::Kstr { dst: 0, kidx: k(0) }.into(),
        RcOp::Dec { src: 0 }.into(),
        Uop::Kimm { dst: 1, kidx: k(1) }.into(),
        Uop::Ret { src: 1 }.into(),
    ]);
    let mut cycle = cycle;
    cycle.pool = vec![Const::Str("ephemeral".into()), Const::I64(0)];
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("oe", vec![cycle]).expect("valid fixture module"));
    let mut vm = Vm::new(reg);
    let first = vm.run("oe", "cycle", &[]).expect("the drive completes");
    assert_eq!(first, Value::I64(0));
    assert!(
        vm.objects().is_empty(),
        "the dec to zero frees the object and unbinds the intern entry"
    );
    // the next drive re-interns: the freed slot's id may be reused by the
    // free list, but the load answers a freshly interned object with one
    // count for the delivered word
    let second = vm
        .run("oe", "cycle", &[])
        .expect("the second drive completes");
    assert_eq!(second, Value::I64(0));
    assert!(vm.objects().is_empty(), "the second cycle drains too");
}

/// Stage two, the split's cross-heap service pin: a child pushes its own
/// freshly interned string into a list captured from the parent — the
/// corpus shape (examples/nursery.ds) in miniature. What must hold: the
/// push serves the foreign list; the list's acquired element count lands
/// on the child's heap through the routing face; the child's own exit dec
/// balances its frame claim; and the list's death cascade releases the
/// foreign-held element count on the child's heap, so both stores drain.
#[test]
fn a_child_pushes_into_the_parent_list_and_both_heaps_drain() {
    use ds_vm::native::{BUILTIN_LIST_NEW, BUILTIN_LIST_PUSH};

    fn builtin(index: u32) -> Const {
        Const::FuncRef(ConstFuncRef::Named {
            module: "builtin".into(),
            func: index,
        })
    }
    // main: a fresh list on the root heap, handed to the host (the
    // native's result overlays argument word 0)
    let main = def("main", &[SlotType::Ref, SlotType::I64], 0, 1, &[
        Uop::Funcf { frame_slots: 2 }.into(),
        Uop::Call {
            callee: ConstIdx(0),
            nargs: 0,
        }
        .into(),
        Uop::Ret { src: 1 }.into(),
    ]);
    let mut main = main;
    main.pool = vec![builtin(BUILTIN_LIST_NEW)];

    // child(list): intern "body of the child" on its OWN heap, push it
    // into the foreign (root-heap) list, drop the frame's own claim,
    // land
    let child = def(
        "child",
        &[SlotType::Ref, SlotType::Ref, SlotType::Raw, SlotType::Raw],
        1,
        2,
        &[
            Uop::Funcf { frame_slots: 4 }.into(),
            Uop::Kstr {
                dst: 1,
                kidx: ConstIdx(0),
            }
            .into(),
            Uop::Mov { dst: 2, src: 0 }.into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Call {
                callee: ConstIdx(1),
                nargs: 2,
            }
            .into(),
            RcOp::Dec { src: 1 }.into(),
            Uop::Kimm {
                dst: 2,
                kidx: ConstIdx(2),
            }
            .into(),
            Uop::Ret { src: 2 }.into(),
        ],
    );
    let mut child = child;
    child.pool = vec![
        Const::Str("body of the child".into()),
        builtin(BUILTIN_LIST_PUSH),
        Const::I64(0),
    ];

    // drop(list): the host's borrowed word's count decs, freeing the
    // list whose cascade releases the foreign element count
    let drop_it = def("drop", &[SlotType::Ref, SlotType::I64], 1, 1, &[
        Uop::Funcf { frame_slots: 2 }.into(),
        RcOp::Dec { src: 0 }.into(),
        Uop::Kimm {
            dst: 1,
            kidx: ConstIdx(0),
        }
        .into(),
        Uop::Ret { src: 1 }.into(),
    ]);
    let mut drop_it = drop_it;
    drop_it.pool = vec![Const::I64(0)];

    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("sx", vec![main, child, drop_it]).expect("valid module"));
    let mut vm = Vm::new(reg);
    let Value::Ref(list) = vm.run("sx", "main", &[]).expect("the root lands a list") else {
        panic!("a list ref");
    };
    assert_eq!(list.fiber(), FIBER_ROOT);
    let child_id = vm
        .spawn_fiber("sx", "child", &[Value::Ref(list)])
        .expect("spawn the pusher");
    vm.resume_fiber(child_id, Value::I64(0))
        .expect("the child lands");
    // the foreign push served: the root-heap list now holds the child's
    // word, and the child's heap carries the list's acquired count
    let elems = vm
        .objects_of(list)
        .list_at(list.id())
        .expect("the list lives")
        .to_vec();
    assert_eq!(elems.len(), 1, "the foreign push appended");
    let Value::Ref(element) = elems[0] else {
        panic!("a string ref");
    };
    assert_eq!(
        element.fiber(),
        child_id,
        "the element names the child heap"
    );
    assert_eq!(
        vm.objects_of(element).str_at(element.id()),
        Ok("body of the child")
    );
    assert_eq!(
        vm.objects_of(element).rc_at(element.id()),
        Ok(1),
        "the list's acquired count survives the child's exit dec"
    );
    // the list's death: the cascade decs the foreign element word on the
    // child's heap through the routing face
    vm.run("sx", "drop", &[Value::Ref(list)])
        .expect("the drop lands");
    assert!(vm.objects_at(FIBER_ROOT).is_empty(), "the root heap drains");
    assert!(
        vm.objects_at(child_id).is_empty(),
        "the cascade released the foreign-held count, so the child heap drains"
    );
}

/// Stage two, the KSTR fiber-axis pin: each heap interns its own copy of
/// a pool constant — two fibers loading the same constant answer distinct
/// objects on distinct heaps, equal by content wherever the words land
/// (the routed read serves a foreign word), and the kstr cache's fiber
/// axis never serves one heap's id against another's.
#[test]
fn kstr_loads_serve_per_fiber_copies_across_switches() {
    // root: load the constant, hand the word to the host
    let main = def("main", &[SlotType::Ref, SlotType::I64], 0, 0, &[
        Uop::Funcf { frame_slots: 2 }.into(),
        Uop::Kstr {
            dst: 0,
            kidx: ConstIdx(0),
        }
        .into(),
        Uop::Ret { src: 0 }.into(),
    ]);
    let mut main = main;
    main.pool = vec![Const::Str("shared constant".into())];

    // eqfn(a, b): content equality across whatever heaps the words name
    let eqfn = def(
        "eqfn",
        &[SlotType::Ref, SlotType::Ref, SlotType::Bool],
        2,
        1,
        &[
            Uop::Funcf { frame_slots: 3 }.into(),
            Uop::EqStr {
                dst: 2,
                lhs: 0,
                rhs: 1,
            }
            .into(),
            Uop::Ret { src: 2 }.into(),
        ],
    );

    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("kx", vec![main, eqfn]).expect("valid module"));
    let mut vm = Vm::new(reg);
    let Value::Ref(root_word) = vm.run("kx", "main", &[]).expect("the root loads") else {
        panic!("a string ref");
    };
    let child_id = vm
        .spawn_fiber("kx", "main", &[])
        .expect("spawn the child loader");
    let Value::Ref(child_word) = vm
        .resume_fiber(child_id, Value::I64(0))
        .expect("the child loads")
    else {
        panic!("a string ref");
    };
    assert_eq!(root_word.fiber(), FIBER_ROOT);
    assert_eq!(child_word.fiber(), child_id);
    assert_ne!(
        root_word, child_word,
        "each heap interns its own copy: the words differ"
    );
    // one count per load, each on its own heap
    assert_eq!(vm.objects_of(root_word).rc_at(root_word.id()), Ok(1));
    assert_eq!(vm.objects_of(child_word).rc_at(child_word.id()), Ok(1));
    assert_eq!(
        vm.objects_of(child_word).str_at(child_word.id()),
        Ok("shared constant"),
        "the routed read serves the delivered foreign word"
    );
    // content equality holds across heaps: EQSTR routes both operands
    assert_eq!(
        vm.run("kx", "eqfn", &[
            Value::Ref(root_word),
            Value::Ref(child_word)
        ]),
        Ok(Value::Bool(true)),
        "cross-heap string comparison answers by content"
    );
    assert_eq!(
        vm.run("kx", "eqfn", &[
            Value::Ref(child_word),
            Value::Ref(root_word)
        ]),
        Ok(Value::Bool(true)),
        "in either operand order"
    );
}

// -- The death-walk doors: each child-death path drains its heap ------
//
// The walk's claim ledger releases exactly what a dead fiber's kept
// frames own, routed to each word's own heap — the cancel-cut door (the
// preemption), the interrupt-marking door (the nursery walk), and the
// reuse door (the pool slot) below; the completed door holds nothing to
// release (a completed fiber's exit decs already ran).

/// The trapped-interrupted door: a child that dies trapped mid-drive
/// (frames kept) is walked dead by the nursery join, and the walk
/// releases both claim classes — the captured root-heap word's count
/// through the cross-heap channel, and the child's own kept frame claim
/// on its own heap. Both heaps drain.
#[test]
fn a_trapped_child_is_walked_dead_and_both_heaps_drain() {
    use ds_vm::native::{
        BUILTIN_NURSERY_BOUNDARY, BUILTIN_NURSERY_CANCEL, BUILTIN_NURSERY_OPEN,
        BUILTIN_NURSERY_SPAWN,
    };

    fn builtin(index: u32) -> Const {
        Const::FuncRef(ConstFuncRef::Named {
            module: "builtin".into(),
            func: index,
        })
    }

    // trap_parent: intern "shared" on the root heap, wrap the child in a
    // closure capturing it, spawn through the seam, boundary with a mask
    // that contains the cancel tag, then drop the root's own claim.
    // Slots: [0] the shared word, [1] scratch, [2] the closure; the
    // overlay (words 3, 4) stages each native call's operands.
    let parent = def(
        "trap_parent",
        &[
            SlotType::Ref,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
        ],
        0,
        2,
        &[
            Uop::Funcf { frame_slots: 5 }.into(),
            Uop::Kstr { dst: 0, kidx: k(6) }.into(),
            Uop::Kconst { dst: 1, kidx: k(3) }.into(),
            Uop::Mov { dst: 3, src: 0 }.into(),
            Uop::ClosureNew {
                dst: 2,
                func: 1,
                n: 1,
            }
            .into(),
            // open: the fresh nursery id lands on overlay word 0
            Uop::Call {
                callee: k(0),
                nargs: 0,
            }
            .into(),
            Uop::Mov { dst: 1, src: 3 }.into(),
            // spawn: (id, the closure word)
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Mov { dst: 4, src: 2 }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            // boundary: (id, the containment mask); the answer span lands
            // on the same overlay words
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kimm { dst: 4, kidx: k(5) }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 2,
            }
            .into(),
            // the root's own claim on "shared" goes with the answer
            // the parent's own claims go with the answer: the closure
            // first (its capture edge releases with it), then the word
            RcOp::Dec { src: 2 }.into(),
            RcOp::Dec { src: 0 }.into(),
            Uop::Kimm { dst: 4, kidx: k(4) }.into(),
            Uop::Ret { src: 4 }.into(),
        ],
    );
    let mut parent = parent;
    // [0] open, [1] spawn, [2] boundary, [3] the child's callable, [4]
    // the landing answer, [5] the mask, [6] the shared constant
    parent.pool = vec![
        builtin(BUILTIN_NURSERY_OPEN),
        builtin(BUILTIN_NURSERY_SPAWN),
        builtin(BUILTIN_NURSERY_BOUNDARY),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dw".into(),
            func: 1,
        }),
        Const::I64(0),
        Const::I64(1),
        Const::Str("shared".into()),
    ];

    // trap_child(shared: Ref): intern its own kept word, then cancel —
    // the call never answers, the drive ends trapped mid-frame
    let child = def(
        "trap_child",
        &[
            SlotType::Ref,
            SlotType::Ref,
            SlotType::Tag,
            SlotType::Raw,
            SlotType::Raw,
        ],
        1,
        1,
        &[
            Uop::Funcf { frame_slots: 5 }.into(),
            Uop::Kstr { dst: 1, kidx: k(0) }.into(),
            Uop::TagNew { dst: 2, tag: 0 }.into(),
            Uop::Mov { dst: 4, src: 2 }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 1,
            }
            .into(),
            Uop::Ret { src: 4 }.into(),
        ],
    );
    let mut child = child;
    // [0] the kept word, [1] cancel, [2] the unreachable answer
    child.pool = vec![
        Const::Str("kept by the trapped child".into()),
        builtin(BUILTIN_NURSERY_CANCEL),
        Const::I64(0),
    ];

    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("dw", vec![parent, child]).expect("valid module"));
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("dw", "trap_parent", &[]),
        Ok(Value::I64(0)),
        "the boundary unions the cancel out and the parent lands"
    );
    assert!(
        vm.objects_at(FIBER_ROOT).is_empty(),
        "the walk released the child's captured claim (cross-heap), the parent's dec freed the \
         object"
    );
    assert!(
        vm.objects_at(ds_vm::FiberId(1)).is_empty(),
        "the walk released the trapped child's own kept-frame claim — its heap drains"
    );
}

/// The cancel-cut door: a child cut down at its first taken backward
/// branch (the preemption point) never runs its exit decs — the walk at
/// the cut releases its captured count and its own kept word, so both
/// the cut child's heap and the capturer's drain.
#[test]
fn a_cancel_cut_child_releases_its_kept_claims_at_the_cut() {
    use ds_vm::native::{
        BUILTIN_NURSERY_BOUNDARY, BUILTIN_NURSERY_CANCEL, BUILTIN_NURSERY_OPEN,
        BUILTIN_NURSERY_SPAWN,
    };

    fn builtin(index: u32) -> Const {
        Const::FuncRef(ConstFuncRef::Named {
            module: "builtin".into(),
            func: index,
        })
    }

    // cut_parent: spawn the flag raiser first and the cut child second
    // (spawn order decides the drive order, so the flag is up before
    // the cut child runs), then boundary with a containing mask
    let parent = def(
        "cut_parent",
        &[
            SlotType::Ref,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
        ],
        0,
        2,
        &[
            Uop::Funcf { frame_slots: 5 }.into(),
            Uop::Kstr { dst: 0, kidx: k(8) }.into(),
            Uop::Kconst { dst: 1, kidx: k(3) }.into(),
            Uop::Mov { dst: 3, src: 0 }.into(),
            Uop::ClosureNew {
                dst: 2,
                func: 1,
                n: 1,
            }
            .into(),
            // open: the fresh nursery id lands on overlay word 0
            Uop::Call {
                callee: k(0),
                nargs: 0,
            }
            .into(),
            Uop::Mov { dst: 1, src: 3 }.into(),
            // the flag raiser spawns first (drive order follows spawn
            // order, so its trapped death raises the flag before the cut
            // child runs)
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kconst { dst: 4, kidx: k(4) }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            // the cut child spawns second, carrying the capture
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Mov { dst: 4, src: 2 }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            // boundary: (id, the containment mask)
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Kimm { dst: 4, kidx: k(7) }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 2,
            }
            .into(),
            RcOp::Dec { src: 2 }.into(),
            RcOp::Dec { src: 0 }.into(),
            Uop::Kimm { dst: 4, kidx: k(6) }.into(),
            Uop::Ret { src: 4 }.into(),
        ],
    );
    let mut parent = parent;
    // [0] open, [1] spawn, [2] boundary, [3] the cut child's callable
    // (the closure's target), [4] the flag raiser's callable, [5] the
    // raiser's answer, [6] the landing answer, [7] the mask, [8] the
    // shared constant
    parent.pool = vec![
        builtin(BUILTIN_NURSERY_OPEN),
        builtin(BUILTIN_NURSERY_SPAWN),
        builtin(BUILTIN_NURSERY_BOUNDARY),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dw2".into(),
            func: 2,
        }),
        Const::FuncRef(ConstFuncRef::Named {
            module: "dw2".into(),
            func: 1,
        }),
        Const::I64(0),
        Const::I64(0),
        Const::I64(1),
        Const::Str("shared".into()),
    ];

    // flag_child: cancel immediately — the first trapped death raises
    // the flag the cut child observes
    let flag_child = def(
        "flag_child",
        &[SlotType::Tag, SlotType::Raw, SlotType::Raw],
        0,
        1,
        &[
            Uop::Funcf { frame_slots: 3 }.into(),
            Uop::TagNew { dst: 0, tag: 0 }.into(),
            Uop::Mov { dst: 2, src: 0 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 1,
            }
            .into(),
            Uop::Ret { src: 2 }.into(),
        ],
    );
    let mut flag_child = flag_child;
    flag_child.pool = vec![builtin(BUILTIN_NURSERY_CANCEL), Const::I64(0)];

    // cut_child(shared: Ref): intern its own kept word, then loop on a
    // taken backward branch — the flag is up, so the very first back
    // edge is the cut
    let cut_child = def(
        "cut_child",
        &[SlotType::Ref, SlotType::Ref, SlotType::Bool, SlotType::I64],
        1,
        1,
        &[
            Uop::Funcf { frame_slots: 4 }.into(),
            Uop::Kstr { dst: 1, kidx: k(0) }.into(),
            Uop::Kconst { dst: 2, kidx: k(1) }.into(),
            Uop::Jmpif { cond: 2, target: 3 }.into(),
            Uop::Ret { src: 3 }.into(),
        ],
    );
    let mut cut_child = cut_child;
    // [0] the kept word, [1] the loop condition, [2] the unreachable
    // answer
    cut_child.pool = vec![
        Const::Str("kept by the cut child".into()),
        Const::Bool(true),
        Const::I64(0),
    ];

    let mut reg = ModuleRegistry::new();
    reg.install(
        ModuleVersion::new("dw2", vec![parent, flag_child, cut_child]).expect("valid module"),
    );
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("dw2", "cut_parent", &[]),
        Ok(Value::I64(0)),
        "the boundary unions the flag raiser's cancel out and the parent lands"
    );
    assert!(
        vm.objects_at(FIBER_ROOT).is_empty(),
        "the cut released the captured claim cross-heap, the parent's dec freed the object"
    );
    assert!(
        vm.objects_at(ds_vm::FiberId(1)).is_empty(),
        "the flag raiser's kept frames walked dead by the nursery join"
    );
    assert!(
        vm.objects_at(ds_vm::FiberId(2)).is_empty(),
        "the cut child's own kept word released at the cut — its heap drains"
    );
}

/// The completed door: a child driven to completion by the host releases
/// its own claims through its exit decs (the walk holds nothing), and
/// its delivered word keeps exactly one transferred count over its dead
/// heap — dropping the parent-side count empties it.
#[test]
fn a_completed_child_drains_via_its_own_exit_decs() {
    // main: hand the host a list to capture (the same shape as the
    // push pin, over the host face instead)
    let main = def("main", &[SlotType::Ref, SlotType::I64], 0, 1, &[
        Uop::Funcf { frame_slots: 2 }.into(),
        Uop::Call {
            callee: ConstIdx(0),
            nargs: 0,
        }
        .into(),
        Uop::Ret { src: 1 }.into(),
    ]);
    let mut main = main;
    main.pool = vec![Const::FuncRef(ConstFuncRef::Named {
        module: "builtin".into(),
        func: 20,
    })];

    // child(list): a kept string balanced by its own exit dec — the
    // completed door's honesty witness
    let child = def(
        "child",
        &[SlotType::Ref, SlotType::Ref, SlotType::Raw, SlotType::I64],
        1,
        1,
        &[
            Uop::Funcf { frame_slots: 4 }.into(),
            Uop::Kstr { dst: 1, kidx: k(0) }.into(),
            RcOp::Dec { src: 1 }.into(),
            Uop::Kimm { dst: 2, kidx: k(1) }.into(),
            Uop::Ret { src: 2 }.into(),
        ],
    );
    let mut child = child;
    child.pool = vec![Const::Str("balanced".into()), Const::I64(0)];

    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("dw3", vec![main, child]).expect("valid module"));
    let mut vm = Vm::new(reg);
    let Value::Ref(list) = vm.run("dw3", "main", &[]).expect("the root lands a list") else {
        panic!("a list ref");
    };
    let child_id = vm
        .spawn_fiber("dw3", "child", &[Value::Ref(list)])
        .expect("spawn the completing child");
    vm.resume_fiber(child_id, Value::I64(0))
        .expect("the child completes");
    assert!(
        vm.objects_at(child_id).is_empty(),
        "the completed child's exit decs drained its own heap — the walk held nothing"
    );
    // the borrowed capture never claimed: the list survives for the host
    assert_eq!(
        vm.objects_of(list).list_at(list.id()),
        Ok(&[][..]),
        "the child's borrow never entered the parent's counts"
    );
}

/// The dead-pool KSTR row reclamation: a hot-reload swap retires the
/// oldest version out of the two-slot table and its pools' intern rows go
/// with it, while a recycled pool slot's rows go at the reuse door — so
/// swap-and-churn cycles keep the VM-global row table bounded by the two
/// live versions' pools times the loading fibers instead of growing with
/// every version ever installed. Every load still serves: a reclaimed row
/// costs its next load one intern hit on the store's own content map (the
/// per-fiber ruling's duplicate cost, paid once per recycle).
#[test]
fn dead_pool_kstr_rows_reclaim_across_swap_and_slot_cycles() {
    fn loader_module() -> ModuleVersion {
        let mut load = def("load", &[SlotType::Ref, SlotType::I64], 0, 0, &[
            Uop::Funcf { frame_slots: 2 }.into(),
            Uop::Kstr { dst: 0, kidx: k(0) }.into(),
            Uop::Ret { src: 0 }.into(),
        ]);
        load.pool = vec![Const::Str("recycled constant".into())];
        ModuleVersion::new("rk", vec![load]).expect("valid fixture module")
    }

    let mut vm = Vm::new(ModuleRegistry::new());
    for _ in 0..24 {
        vm.install_module(loader_module());
        let Value::Ref(root_word) = vm.run("rk", "load", &[]).expect("the root load serves") else {
            panic!("a string ref");
        };
        let child_id = vm.spawn_fiber("rk", "load", &[]).expect("spawn the loader");
        let Value::Ref(child_word) = vm
            .resume_fiber(child_id, Value::I64(0))
            .expect("the child load serves")
        else {
            panic!("a string ref");
        };
        assert_eq!(root_word.fiber(), FIBER_ROOT);
        assert_eq!(
            child_word.fiber(),
            child_id,
            "each heap interns its own copy"
        );
        assert_eq!(
            vm.objects_of(child_word).str_at(child_word.id()),
            Ok("recycled constant")
        );
        // each install answers the row; the table's row is the version's
        // single home, so nothing below holds a version
    }
    // two live versions' pools times the loading fibers, minus what the
    // slot reuse dropped: without the reclamation doors this count climbs
    // with every distinct dead-pool address (measured 16 at this cycle
    // count before the doors landed)
    assert!(
        vm.kstr_cache_rows() <= 6,
        "rows stay bounded across swap and slot cycles, got {}",
        vm.kstr_cache_rows()
    );
}
