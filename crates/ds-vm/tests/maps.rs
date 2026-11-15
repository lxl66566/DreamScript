//! Map natives: hand-assembled fixtures over the builtin core native
//! module's map surface — `map_new`, `map_set` (indices 28 and 31) and
//! the generic reads `map_len`, `map_get` (indices 29-30, CALLG-addressed
//! with two evidence words naming the map instance). The round trips run
//! through CALLG for the generic reads and CALL for the monomorphic
//! constructors and the mutator: construction, the builder-call fill (an
//! overwrite under a content-equal key), the entry count, and the checked
//! read's both members.
//!
//! Layout conventions (the contract ds-lower targets): `map_get` is the
//! checked shape — a two-word tag+payload span [BL-2] on the caller's
//! argument slots 0 and 1, word 0 the discriminant (`PLATFORM_IO_OK` with
//! the value, `PLATFORM_NONE` with the probe key) — so a caller sizes its
//! overlay area to two; `map_set` overlays one word (the inert unit
//! word). The overlay slots declare `Raw` because the same words stage
//! arguments first; `map_set`'s key and value parameters are the `Raw`
//! wildcards whose kinds vary by call site.

use ds_vm::{
    Const, ConstFuncRef, FunctionDef, ModuleRegistry, ModuleVersion, Value, Vm,
    abi::{
        BuiltinInstance, ConstIdx, FrameLayout, SlotType,
        SlotType::{Bool, I64, Raw, Ref},
        uop::Uop,
    },
    assemble,
    native::{
        BUILTIN_CONCAT, BUILTIN_MAP_GET, BUILTIN_MAP_LEN, BUILTIN_MAP_NEW, BUILTIN_MAP_SET,
        PLATFORM_NONE,
    },
};

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn builtin(index: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: "builtin".into(),
        func: index,
    })
}

/// The shared pool, copied per function (the payload granularity): the
/// probe words and the map-facing natives plus the string concat the
/// content-key probe composes with.
fn pool() -> Vec<Const> {
    vec![
        Const::Str("host".into()),               // k0
        Const::Str("one".into()),                // k1
        Const::Str("two".into()),                // k2
        Const::Str("example".into()),            // k3
        builtin(BUILTIN_MAP_NEW),                // k4
        builtin(BUILTIN_MAP_SET),                // k5
        builtin(BUILTIN_MAP_LEN),                // k6
        builtin(BUILTIN_MAP_GET),                // k7
        Const::Str("hos".into()),                // k8
        Const::Str("t".into()),                  // k9
        builtin(BUILTIN_CONCAT),                 // k10
        Const::I64(BuiltinInstance::Map as i64), // k11: the evidence word
    ]
}

fn def(name: &str, slots: &[SlotType], params: usize, args: usize, body: &[Uop]) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout"),
        pool: pool(),
        code: assemble(body).expect("fixture body encodes"),
    }
}

fn map_registry() -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    reg.install(
        ModuleVersion::new("mp", vec![
            fresh(),
            fill(),
            count(),
            at(),
            fresh_key(),
            bogus(),
        ])
        .expect("valid fixture module"),
    );
    reg
}

/// `() -> map`: `map_new` with nothing staged; the result overlays the
/// one-word argument area.
fn fresh() -> FunctionDef {
    def("fresh", &[Ref, Raw], 0, 1, &[
        Uop::Funcf { frame_slots: 2 },
        Uop::Call {
            callee: k(4),
            nargs: 0,
        },
        Uop::Ret { src: 1 },
    ])
}

/// `(map) -> map`: two fresh inserts, an overwrite under the same
/// interned key, and a second entry — the map word hands back unchanged
/// (the unit word lands on the overlay, read by nobody).
fn fill() -> FunctionDef {
    def("fill", &[Ref, Raw, Raw, Raw], 1, 3, &[
        Uop::Funcf { frame_slots: 4 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Kstr { dst: 2, kidx: k(0) },
        Uop::Kstr { dst: 3, kidx: k(1) },
        Uop::Call {
            callee: k(5),
            nargs: 3,
        },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Kstr { dst: 2, kidx: k(0) },
        Uop::Kstr { dst: 3, kidx: k(2) },
        Uop::Call {
            callee: k(5),
            nargs: 3,
        },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Kstr { dst: 2, kidx: k(0) },
        Uop::Kstr { dst: 3, kidx: k(3) },
        Uop::Call {
            callee: k(5),
            nargs: 3,
        },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Kstr { dst: 2, kidx: k(3) },
        Uop::Kstr { dst: 3, kidx: k(2) },
        Uop::Call {
            callee: k(5),
            nargs: 3,
        },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Ret { src: 1 },
    ])
}

/// `(map) -> int`: `map_len` through CALLG, the two evidence words (one
/// per constraint-list entry) staged after the receiver.
fn count() -> FunctionDef {
    def("count", &[Ref, I64, Raw, Raw, Raw], 1, 3, &[
        Uop::Funcf { frame_slots: 5 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Kimm {
            dst: 3,
            kidx: k(11),
        },
        Uop::Kimm {
            dst: 4,
            kidx: k(11),
        },
        Uop::Callg {
            callee: k(6),
            nargs: 1,
        },
        Uop::Ret { src: 2 },
    ])
}

/// `(map, key: value) -> value | none`: stage the map and key, the two
/// evidence words behind them, call through CALLG, then dispatch on the
/// span's tag word; success returns the payload value, a miss halts on
/// the tag word so the run result itself is the `none` shape.
///
/// Slots: [map, key, value, hit, overlay words: args, evidence, span].
fn at() -> FunctionDef {
    def("at", &[Ref, Raw, Raw, Bool, Raw, Raw, Raw, Raw], 2, 4, &[
        Uop::Funcf { frame_slots: 8 },
        Uop::Mov { dst: 4, src: 0 },
        Uop::Mov { dst: 5, src: 1 },
        Uop::Kimm {
            dst: 6,
            kidx: k(11),
        },
        Uop::Kimm {
            dst: 7,
            kidx: k(11),
        },
        Uop::Callg {
            callee: k(7),
            nargs: 2,
        },
        Uop::TagTest {
            dst: 3,
            src: 4,
            tag: PLATFORM_NONE,
        },
        Uop::Jmpif {
            cond: 3,
            target: 10,
        },
        Uop::Mov { dst: 2, src: 5 },
        Uop::Ret { src: 2 },
        Uop::Halt { slot: 4 },
    ])
}

/// `() -> string | none`: builds a one-pair map under the interned
/// `"host"` key, then probes it with a fresh string object of the same
/// content (concatenation never interns) — the content-key match, and
/// the checked span's payload handed out to the caller.
///
/// Slots: [map, value, key, result, overlay words 0-4].
fn fresh_key() -> FunctionDef {
    def(
        "fresh_key",
        &[Ref, Ref, Ref, Ref, Raw, Raw, Raw, Raw, Raw],
        0,
        5,
        &[
            Uop::Funcf { frame_slots: 9 },
            Uop::Call {
                callee: k(4),
                nargs: 0,
            },
            Uop::Mov { dst: 0, src: 4 },
            // the value word first: the staged key words follow it
            Uop::Kstr { dst: 1, kidx: k(1) },
            Uop::Kstr { dst: 2, kidx: k(8) },
            Uop::Kstr { dst: 3, kidx: k(9) },
            Uop::Mov { dst: 4, src: 2 },
            Uop::Mov { dst: 5, src: 3 },
            Uop::Call {
                callee: k(10),
                nargs: 2,
            },
            Uop::Mov { dst: 2, src: 4 },
            Uop::Mov { dst: 4, src: 0 },
            Uop::Mov { dst: 5, src: 2 },
            Uop::Mov { dst: 6, src: 1 },
            Uop::Call {
                callee: k(5),
                nargs: 3,
            },
            Uop::Mov { dst: 4, src: 0 },
            Uop::Mov { dst: 5, src: 2 },
            Uop::Kimm {
                dst: 6,
                kidx: k(11),
            },
            Uop::Kimm {
                dst: 7,
                kidx: k(11),
            },
            Uop::Callg {
                callee: k(7),
                nargs: 2,
            },
            Uop::Mov { dst: 1, src: 5 },
            Uop::Ret { src: 1 },
        ],
    )
}

/// `(s: string) -> unit`: a non-map first argument traps — the staged
/// word names a live string object of the wrong kind.
fn bogus() -> FunctionDef {
    def("bogus", &[Ref, Raw, Raw, Raw], 1, 3, &[
        Uop::Funcf { frame_slots: 4 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Call {
            callee: k(5),
            nargs: 3,
        },
        Uop::Ret { src: 1 },
    ])
}

fn run_value(vm: &mut Vm, func: &str, args: &[Value]) -> Value {
    let answer = vm.run("mp", func, args).expect("the fixture runs");
    let [word] = answer.as_slice() else {
        panic!("the fixture answers one word")
    };
    *word
}

fn count_of(vm: &mut Vm, map: Value) -> i64 {
    match run_value(vm, "count", &[map]) {
        Value::I64(n) => n,
        other => panic!("an int, got {other:?}"),
    }
}

#[test]
fn map_new_answers_the_empty_map() {
    let mut vm = Vm::new(map_registry());
    let map = run_value(&mut vm, "fresh", &[]);
    let Value::Ref(id) = map else {
        panic!("a map ref, got {map:?}");
    };
    assert_eq!(count_of(&mut vm, map), 0);
    assert_eq!(
        vm.objects().map_pairs(id.id()),
        Ok(&[] as &[(Value, Value)]),
        "the constructor answers an empty map"
    );
    // the empty map is a fresh object per call, never shared
    assert_ne!(map, run_value(&mut vm, "fresh", &[]));
}

#[test]
fn map_set_fills_overwrites_and_counts() {
    let mut vm = Vm::new(map_registry());
    let map = run_value(&mut vm, "fresh", &[]);
    let filled = run_value(&mut vm, "fill", &[map]);
    assert_eq!(
        count_of(&mut vm, filled),
        2,
        "the overwrite kept two entries"
    );
    let Value::Ref(id) = filled else {
        panic!("a map ref, got {filled:?}");
    };
    assert_eq!(
        vm.objects().map_pairs(id.id()).map(<[_]>::len),
        Ok(2),
        "host was overwritten, not appended"
    );
    // a non-map receiver traps: the staged word names a string object
    let text = vm.intern_str("words");
    assert!(matches!(
        vm.run("mp", "bogus", &[text]),
        Err(ds_vm::Trap::UnknownObject { .. })
    ));
}

#[test]
fn map_get_answers_hit_and_miss_with_the_checked_span() {
    let mut vm = Vm::new(map_registry());
    let empty = run_value(&mut vm, "fresh", &[]);
    let map = run_value(&mut vm, "fill", &[empty]);
    let Value::Ref(key) = vm.intern_str("host") else {
        panic!("the interned probe word is a ref");
    };
    // hit: the overwritten value, handed out as a read copy
    match run_value(&mut vm, "at", &[map, Value::Ref(key)]) {
        Value::Ref(hit) => {
            assert_eq!(
                vm.objects().str_at(hit.id()),
                Ok("example"),
                "the overwrite won"
            );
        },
        other => panic!("the hit's value ref, got {other:?}"),
    }
    // miss by either side: the none member, never a trap
    let Value::Ref(absent) = vm.intern_str("user") else {
        panic!("the interned probe word is a ref");
    };
    assert_eq!(
        run_value(&mut vm, "at", &[map, Value::Ref(absent)]),
        Value::Tag(PLATFORM_NONE)
    );
    assert_eq!(
        run_value(&mut vm, "at", &[map, Value::I64(1)]),
        Value::Tag(PLATFORM_NONE),
        "a scalar key misses the string entry"
    );
    // a fresh string object of equal content names the interned key's
    // entry: content matching, not identity
    let built = run_value(&mut vm, "fresh_key", &[]);
    match built {
        Value::Ref(hit) => {
            assert_eq!(
                vm.objects().str_at(hit.id()),
                Ok("one"),
                "content keys match"
            );
        },
        other => panic!("the content-key hit's value ref, got {other:?}"),
    }
}
