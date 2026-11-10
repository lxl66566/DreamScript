//! The types_hybrid run items, replicated at uop level: the config-parser
//! flow of `conformance/types_hybrid.ds` (`get_port`, `load`, the `.err`
//! handler, `same`) hand-assembled over the M3 uop extensions, plus its
//! structural-`==` and interpolation surface. Every fixture asserts the
//! exact golden stdout of its run.
//!
//! Layout conventions this suite pins (the contract ds-lower targets):
//!
//! - Union values are tag + payload spans [BL-2]: one tag word, then the widest member's payload,
//!   with per-member payload offsets that respect the slot types where the value flows (unit
//!   members carry no payload; dead payload slots hold whatever was last written and are never
//!   read). Spans are uniform-width per type: every value of a type occupies the same word count,
//!   so RETSPAN returns one width per function.
//! - `int | missing | bad_port` lowers to the tagged baseline (`INT` member tag + i64 payload); `T
//!   | none` niche packing [B4] is a later pass over exactly these spans.
//! - Early exits: `or return`/`?` are TAG_TEST + JMPIF + a RETSPAN that either constructs the
//!   alternative (`or return missing`) or propagates the span by re-spanning it (`?`). Bare-struct
//!   returns use RETSPAN too; single-word returns may use RET.
//! - The `.err` handler: dispatch on the error member tags, call the handler, take its result —
//!   here inline per member.
//! - Interpolation `${x}` lowers to a STRCAT chain over `str()` builtin calls; every run golden
//!   below is that chain's output.
//! - Mixed-shape overlay slots (staged probe args, then a differently typed result span, then
//!   builtin staging) declare `SlotType::Raw`.

use ds_vm::{
    Const, FunctionDef, ModuleBuildError, ModuleRegistry, ModuleVersion, Trap, Value, Vm,
    abi::{
        ConstIdx, FrameLayout, SlideError, SlotType,
        SlotType::{Bool, F64, I64, Raw, Ref, Tag},
        uop::Uop,
    },
    assemble,
    native::{
        BUILTIN_CHAR_AT, BUILTIN_FIBER_RESUME, BUILTIN_FIBER_YIELD, BUILTIN_LEN,
        BUILTIN_LIST_CONCAT, BUILTIN_LIST_GET, BUILTIN_LIST_LEN, BUILTIN_LIST_NEW,
        BUILTIN_LIST_PUSH, BUILTIN_NURSERY_JOIN, BUILTIN_STR_BOOL, BUILTIN_STR_FLOAT,
        BUILTIN_STR_INT, STD_HTTP_GET, STD_NET_CONNECT, STD_NET_RECV, STD_PLATFORM_MODULE,
        STD_PRINT, STD_PRINTLN, std_platform_module,
    },
};

/// Module-wide tag ids, as ds-lower will assign them.
mod tags {
    /// The `none` member of `T | none` option unions.
    pub const NONE: u8 = 0;
    /// `p_none`: the probe union's unit member.
    pub const P_NONE: u8 = 1;
    /// `p_num { value: int }`: i64 payload at offset 0.
    pub const P_NUM: u8 = 2;
    /// `p_str { text: string }`: ref payload at offset 1.
    pub const P_STR: u8 = 3;
    /// `missing`: cfg_err's unit member.
    pub const MISSING: u8 = 4;
    /// `bad_port { got: int }`: i64 payload at offset 0.
    pub const BAD_PORT: u8 = 5;
    /// `INT`: the tagged `int` member of `int | missing | bad_port`.
    pub const INT: u8 = 6;
    /// `CONFIG`: the config member of `config | cfg_err` — host: string at
    /// offset 0, port: int at offset 1, retries: int at offset 2 (and
    /// bad_port.got rides offset 1, both i64).
    pub const CONFIG: u8 = 7;
}

/// The shared pool, copied per function (the payload granularity).
fn pool() -> Vec<Const> {
    vec![
        Const::I64(1),                               // k0: range-check bound low
        Const::I64(65535),                           // k1: range-check bound high
        Const::I64(-1),                              // k2: bad_port.got
        Const::I64(3),                               // k3: retries
        Const::I64(8080),                            // k4: default port
        Const::Str("localhost".to_owned()),          // k5: default host (the interned object)
        Const::Str("local".to_owned()),              // k6
        Const::Str("host".to_owned()),               // k7
        Const::Str("bad port ".to_owned()),          // k8: interpolation prefix
        Const::Str("config read failed".to_owned()), // k9
        Const::Str("same ".to_owned()),              // k10
        Const::Str("value: ".to_owned()),            // k11: the print (no-newline) prefix
        Const::Bool(true),                           // k12
        Const::Bool(false),                          // k13
        Const::FuncRef {
            module: "cfg".into(),
            func: 1,
        }, // k14: find_num
        Const::FuncRef {
            module: "cfg".into(),
            func: 2,
        }, // k15: get_port
        Const::FuncRef {
            module: "cfg".into(),
            func: 3,
        }, // k16: load
        Const::FuncRef {
            module: "cfg".into(),
            func: 4,
        }, // k17: default_config
        platform(STD_PRINTLN),                       // k18: println
        builtin(BUILTIN_STR_INT),                    // k19: str_int
        builtin(BUILTIN_STR_BOOL),                   // k20: str_bool
        platform(STD_PRINT),                         // k21: print
        builtin(BUILTIN_STR_FLOAT),                  // k22: str_float
    ]
}

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn builtin(index: u32) -> Const {
    Const::FuncRef {
        module: "builtin".into(),
        func: index,
    }
}

fn platform(index: u32) -> Const {
    Const::FuncRef {
        module: STD_PLATFORM_MODULE.into(),
        func: index,
    }
}

fn def(name: &str, slots: &[SlotType], params: usize, args: usize, body: &[Uop]) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout"),
        pool: pool(),
        code: assemble(body).expect("fixture body encodes"),
    }
}

fn registry(module: &str, defs: Vec<FunctionDef>) -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    // the fixtures call println/print: the std platform table is the
    // host's to install (`Vm::new` only adds the builtin core)
    reg.install_native(STD_PLATFORM_MODULE, std_platform_module());
    reg.install(ModuleVersion::new(module, defs).expect("valid fixture module"));
    reg
}

/// `const find_num = (p: probe) -> int | none`: the `int | none` probe of
/// the port field — the shape `j.fields.get("port")` takes in the source.
/// Probe spans [Tag, I64, Ref]; the result spans [Tag, I64].
fn find_num() -> FunctionDef {
    def("find_num", &[Tag, I64, Ref, Bool, Tag, I64], 3, 0, &[
        Uop::Funcf { frame_slots: 6 },
        Uop::TagTest {
            dst: 3,
            src: 0,
            tag: tags::P_NUM,
        },
        Uop::JmpifNot { cond: 3, target: 6 },
        Uop::TagNew {
            dst: 4,
            tag: tags::INT,
        },
        Uop::Mov { dst: 5, src: 1 },
        Uop::RetSpan { src: 4, n: 2 },
        Uop::TagNew {
            dst: 4,
            tag: tags::NONE,
        },
        Uop::RetSpan { src: 4, n: 2 },
    ])
}

/// `const get_port = (p: probe) -> int | missing | bad_port`, the source's
/// union flow: the `or return missing` of the `int | none` probe, the range
/// check, and the `bad_port { got: -1 }` construction. Result spans
/// [Tag, I64] (the tagged `int` baseline; B4 niche packing is later).
fn get_port() -> FunctionDef {
    def(
        "get_port",
        &[Tag, I64, Ref, Bool, Tag, I64, Tag, I64, Ref],
        3,
        3,
        &[
            Uop::Funcf { frame_slots: 9 },
            Uop::Mov { dst: 6, src: 0 },
            Uop::Mov { dst: 7, src: 1 },
            Uop::Mov { dst: 8, src: 2 },
            Uop::Call {
                callee: k(14),
                nargs: 3,
            },
            Uop::TagTest {
                dst: 3,
                src: 6,
                tag: tags::NONE,
            },
            Uop::Jmpif {
                cond: 3,
                target: 19,
            },
            Uop::Kimm { dst: 5, kidx: k(0) },
            Uop::LtI64 {
                dst: 3,
                lhs: 7,
                rhs: 5,
            },
            Uop::Jmpif {
                cond: 3,
                target: 16,
            },
            Uop::Kimm { dst: 5, kidx: k(1) },
            Uop::GtI64 {
                dst: 3,
                lhs: 7,
                rhs: 5,
            },
            Uop::Jmpif {
                cond: 3,
                target: 16,
            },
            Uop::TagNew {
                dst: 4,
                tag: tags::INT,
            },
            Uop::Mov { dst: 5, src: 7 },
            Uop::RetSpan { src: 4, n: 2 },
            Uop::TagNew {
                dst: 4,
                tag: tags::BAD_PORT,
            },
            Uop::Kimm { dst: 5, kidx: k(2) },
            Uop::RetSpan { src: 4, n: 2 },
            Uop::TagNew {
                dst: 4,
                tag: tags::MISSING,
            },
            Uop::RetSpan { src: 4, n: 2 },
        ],
    )
}

/// `const load = (p: probe) -> config | cfg_err`: the `?` propagation of
/// `missing | bad_port` into the error union (the span re-spanned by
/// RETSPAN — the payload word riding the same offset the `config` member
/// uses), and the config construction with a concatenated host. Result
/// spans [Tag, Ref, I64, I64].
fn load() -> FunctionDef {
    def(
        "load",
        &[
            Tag, I64, Ref, Bool, Tag, Ref, I64, I64, Ref, Ref, Tag, I64, Ref,
        ],
        3,
        3,
        &[
            Uop::Funcf { frame_slots: 13 },
            Uop::Mov { dst: 10, src: 0 },
            Uop::Mov { dst: 11, src: 1 },
            Uop::Mov { dst: 12, src: 2 },
            Uop::Call {
                callee: k(15),
                nargs: 3,
            },
            Uop::TagTest {
                dst: 3,
                src: 10,
                tag: tags::MISSING,
            },
            Uop::Jmpif {
                cond: 3,
                target: 19,
            },
            Uop::TagTest {
                dst: 3,
                src: 10,
                tag: tags::BAD_PORT,
            },
            Uop::Jmpif {
                cond: 3,
                target: 16,
            },
            Uop::TagNew {
                dst: 4,
                tag: tags::CONFIG,
            },
            Uop::Kstr { dst: 8, kidx: k(6) },
            Uop::Kstr { dst: 9, kidx: k(7) },
            Uop::StrCat {
                dst: 5,
                lhs: 8,
                rhs: 9,
            },
            Uop::Mov { dst: 6, src: 11 },
            Uop::Kimm { dst: 7, kidx: k(3) },
            Uop::RetSpan { src: 4, n: 4 },
            Uop::Mov { dst: 4, src: 10 },
            Uop::Mov { dst: 6, src: 11 },
            Uop::RetSpan { src: 4, n: 4 },
            Uop::Mov { dst: 4, src: 10 },
            Uop::RetSpan { src: 4, n: 4 },
        ],
    )
}

/// `const default_config = () -> config`: the bare struct — a 3-word span
/// with no union tag. Its host is the interned `localhost` object, while
/// `load` builds its host by concatenation, so the `same` comparison
/// exercises content equality across distinct string objects.
fn default_config() -> FunctionDef {
    def("default_config", &[Ref, I64, I64], 0, 0, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Kstr { dst: 0, kidx: k(5) },
        Uop::Kimm { dst: 1, kidx: k(4) },
        Uop::Kimm { dst: 2, kidx: k(3) },
        Uop::RetSpan { src: 0, n: 3 },
    ])
}

/// `const main = (p: probe)`: the `.err` dispatch over `config | cfg_err`
/// with the handler's `match e { b if b is bad_port => log(...), _ => ... }`
/// (println standing in for log, M5 owns effects), then
/// `same = cfg == default_config()` — the structural `==` chain — and the
/// interpolated report. The overlay area is Raw: it stages the probe, then
/// receives load's 4-word result, then stages builtin calls.
fn main_fn() -> FunctionDef {
    def(
        "main",
        &[
            Tag, I64, Ref, Bool, Bool, Bool, Tag, Ref, I64, I64, Raw, Raw, Raw, Raw,
        ],
        3,
        4,
        &[
            Uop::Funcf { frame_slots: 14 },
            Uop::Mov { dst: 10, src: 0 },
            Uop::Mov { dst: 11, src: 1 },
            Uop::Mov { dst: 12, src: 2 },
            Uop::Call {
                callee: k(16),
                nargs: 3,
            },
            Uop::TagTest {
                dst: 3,
                src: 10,
                tag: tags::BAD_PORT,
            },
            Uop::Jmpif {
                cond: 3,
                target: 14,
            },
            Uop::TagTest {
                dst: 3,
                src: 10,
                tag: tags::MISSING,
            },
            Uop::Jmpif {
                cond: 3,
                target: 24,
            },
            Uop::Mov { dst: 7, src: 11 },
            Uop::Mov { dst: 8, src: 12 },
            Uop::Mov { dst: 9, src: 13 },
            Uop::Call {
                callee: k(17),
                nargs: 0,
            },
            Uop::Jmp { target: 31 },
            Uop::Mov { dst: 10, src: 12 },
            Uop::Call {
                callee: k(19),
                nargs: 1,
            },
            Uop::Kstr {
                dst: 11,
                kidx: k(8),
            },
            Uop::StrCat {
                dst: 10,
                lhs: 11,
                rhs: 10,
            },
            Uop::Call {
                callee: k(18),
                nargs: 1,
            },
            Uop::Call {
                callee: k(17),
                nargs: 0,
            },
            Uop::Mov { dst: 7, src: 10 },
            Uop::Mov { dst: 8, src: 11 },
            Uop::Mov { dst: 9, src: 12 },
            Uop::Jmp { target: 31 },
            Uop::Kstr {
                dst: 10,
                kidx: k(9),
            },
            Uop::Call {
                callee: k(18),
                nargs: 1,
            },
            Uop::Call {
                callee: k(17),
                nargs: 0,
            },
            Uop::Mov { dst: 7, src: 10 },
            Uop::Mov { dst: 8, src: 11 },
            Uop::Mov { dst: 9, src: 12 },
            Uop::Jmp { target: 31 },
            Uop::EqStr {
                dst: 3,
                lhs: 7,
                rhs: 10,
            },
            Uop::JmpifNot {
                cond: 3,
                target: 39,
            },
            Uop::EqI64 {
                dst: 4,
                lhs: 8,
                rhs: 11,
            },
            Uop::JmpifNot {
                cond: 4,
                target: 39,
            },
            Uop::EqI64 {
                dst: 4,
                lhs: 9,
                rhs: 12,
            },
            Uop::JmpifNot {
                cond: 4,
                target: 39,
            },
            Uop::Kconst {
                dst: 5,
                kidx: k(12),
            },
            Uop::Jmp { target: 40 },
            Uop::Kconst {
                dst: 5,
                kidx: k(13),
            },
            Uop::Mov { dst: 10, src: 5 },
            Uop::Call {
                callee: k(20),
                nargs: 1,
            },
            Uop::Mov { dst: 11, src: 10 },
            Uop::Kstr {
                dst: 10,
                kidx: k(10),
            },
            Uop::StrCat {
                dst: 10,
                lhs: 10,
                rhs: 11,
            },
            Uop::Call {
                callee: k(18),
                nargs: 1,
            },
            Uop::Halt { slot: 5 },
        ],
    )
}

/// `const probe_same = (a: probe, b: probe) -> bool`: structural `==`
/// across the probe union itself — tag dispatch per member, then the
/// member's payload comparison (EQI64 for `p_num`, EQSTR for `p_str`,
/// true for the unit member).
fn probe_same() -> FunctionDef {
    def(
        "probe_same",
        &[Tag, I64, Ref, Tag, I64, Ref, Bool, Bool],
        6,
        0,
        &[
            Uop::Funcf { frame_slots: 8 },
            Uop::TagTest {
                dst: 6,
                src: 0,
                tag: tags::P_NUM,
            },
            Uop::JmpifNot { cond: 6, target: 7 },
            Uop::TagTest {
                dst: 6,
                src: 3,
                tag: tags::P_NUM,
            },
            Uop::JmpifNot {
                cond: 6,
                target: 19,
            },
            Uop::EqI64 {
                dst: 7,
                lhs: 1,
                rhs: 4,
            },
            Uop::Halt { slot: 7 },
            Uop::TagTest {
                dst: 6,
                src: 0,
                tag: tags::P_STR,
            },
            Uop::JmpifNot {
                cond: 6,
                target: 13,
            },
            Uop::TagTest {
                dst: 6,
                src: 3,
                tag: tags::P_STR,
            },
            Uop::JmpifNot {
                cond: 6,
                target: 19,
            },
            Uop::EqStr {
                dst: 7,
                lhs: 2,
                rhs: 5,
            },
            Uop::Halt { slot: 7 },
            Uop::TagTest {
                dst: 6,
                src: 0,
                tag: tags::P_NONE,
            },
            Uop::JmpifNot {
                cond: 6,
                target: 19,
            },
            Uop::TagTest {
                dst: 6,
                src: 3,
                tag: tags::P_NONE,
            },
            Uop::JmpifNot {
                cond: 6,
                target: 19,
            },
            Uop::Kconst {
                dst: 7,
                kidx: k(12),
            },
            Uop::Halt { slot: 7 },
            Uop::Kconst {
                dst: 7,
                kidx: k(13),
            },
            Uop::Halt { slot: 7 },
        ],
    )
}

/// `const report = (x: float)`: `print("value: ")` (no newline), then
/// `println(str(x))` — the print/str_float builtins in one interpolation
/// chain.
fn report() -> FunctionDef {
    def("report", &[F64, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Kstr {
            dst: 1,
            kidx: k(11),
        },
        Uop::Call {
            callee: k(21),
            nargs: 1,
        },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Call {
            callee: k(22),
            nargs: 1,
        },
        Uop::Call {
            callee: k(18),
            nargs: 1,
        },
        Uop::Halt { slot: 0 },
    ])
}

fn cfg_registry() -> ModuleRegistry {
    registry("cfg", vec![
        main_fn(),
        find_num(),
        get_port(),
        load(),
        default_config(),
        probe_same(),
        report(),
    ])
}

/// A staged probe value: [Tag, I64, Ref] with the `p_num` payload at
/// offset 0 and a dead ref word.
fn probe_num(vm: &mut Vm, value: i64) -> Vec<Value> {
    vec![
        Value::Tag(tags::P_NUM),
        Value::I64(value),
        vm.intern_str("probe"),
    ]
}

fn probe_str(vm: &mut Vm, text: &str) -> Vec<Value> {
    vec![Value::Tag(tags::P_STR), Value::I64(0), vm.intern_str(text)]
}

fn probe_none(vm: &mut Vm) -> Vec<Value> {
    vec![
        Value::Tag(tags::P_NONE),
        Value::I64(0),
        vm.intern_str("probe"),
    ]
}

/// Runs `main` over a probe and asserts the exact golden stdout plus the
/// `same` result word.
fn run_main(probe: impl FnOnce(&mut Vm) -> Vec<Value>, stdout: &str, same: bool) {
    let mut vm = Vm::new(cfg_registry());
    vm.capture_output();
    let args = probe(&mut vm);
    assert_eq!(
        vm.run("cfg", "main", &args),
        Ok(Value::Bool(same)),
        "the run result is the `same` comparison"
    );
    assert_eq!(
        vm.captured_output(),
        stdout.as_bytes(),
        "the golden stdout of the types_hybrid flow"
    );
}

#[test]
fn get_port_flow_ok_path_runs_the_config_to_the_same_true_golden() {
    // `const cfg = load(p)` with a valid port: no handler line, the config
    // reaches the `same` comparison, and the concatenated host equals the
    // interned one by content (EQSTR across distinct objects)
    run_main(|vm| probe_num(vm, 8080), "same true\n", true);
}

#[test]
fn get_port_flow_bad_port_prints_the_interpolated_handler_message() {
    // port 0 fails the range check: `bad_port { got: -1 }` propagates out
    // of load, the handler prints `"bad port ${str(b.got)}"`
    run_main(|vm| probe_num(vm, 0), "bad port -1\nsame true\n", true);
    // the upper bound fails the same way
    run_main(|vm| probe_num(vm, 70000), "bad port -1\nsame true\n", true);
}

#[test]
fn get_port_flow_missing_and_none_print_the_fallback_handler_line() {
    // `p_none` is not `p_num`: find_num answers none, `or return missing`
    // fires, and the handler's fallback arm prints
    run_main(probe_none, "config read failed\nsame true\n", true);
    // `p_str` fails the same tag test — the second-member tag dispatch
    run_main(
        |vm| probe_str(vm, "oops"),
        "config read failed\nsame true\n",
        true,
    );
}

#[test]
fn structural_eq_fails_on_a_differing_payload() {
    // port 9999 passes get_port's range check but differs from the default:
    // the EQI64 link in the `same` chain fails
    run_main(|vm| probe_num(vm, 9999), "same false\n", false);
}

#[test]
fn structural_eq_across_unions_covers_members_and_payloads() {
    // one VM per case: Ref arguments name objects in the running VM's own
    // heap, so probes intern and run on the same instance
    let case = |a: fn(&mut Vm) -> Vec<Value>,
                b: fn(&mut Vm) -> Vec<Value>,
                av: i64,
                bv: i64,
                expect: Result<Value, Trap>| {
        let mut vm = Vm::new(cfg_registry());
        let mut args = a(&mut vm);
        args.extend(b(&mut vm));
        assert_eq!(
            vm.run("cfg", "probe_same", &args),
            expect,
            "probes {av} and {bv}"
        );
    };
    // same member, same payload
    case(
        |v| probe_num(v, 8080),
        |v| probe_num(v, 8080),
        8080,
        8080,
        Ok(Value::Bool(true)),
    );
    // differing i64 payload
    case(
        |v| probe_num(v, 8080),
        |v| probe_num(v, 80),
        8080,
        80,
        Ok(Value::Bool(false)),
    );
    // member mismatch: p_num against p_str
    case(
        |v| probe_num(v, 8080),
        |v| probe_str(v, "8080"),
        8080,
        0,
        Ok(Value::Bool(false)),
    );
    // string payloads compare by content
    case(
        |v| probe_str(v, "ab"),
        |v| probe_str(v, "ab"),
        0,
        0,
        Ok(Value::Bool(true)),
    );
    case(
        |v| probe_str(v, "ab"),
        |v| probe_str(v, "cd"),
        0,
        0,
        Ok(Value::Bool(false)),
    );
    // unit members: both p_none
    case(probe_none, probe_none, 0, 0, Ok(Value::Bool(true)));
}

#[test]
fn interpolation_chain_uses_print_and_the_float_conversion() {
    let mut vm = Vm::new(cfg_registry());
    vm.capture_output();
    // the last staged overlay word is println's inert unit word, so the
    // report body halts on its input; the golden is the stdout
    assert_eq!(
        vm.run("cfg", "report", &[Value::F64(2.5)]),
        Ok(Value::F64(2.5))
    );
    assert_eq!(vm.captured_output(), b"value: 2.5\n");
}

/// RETSPAN mechanics under CALLT: the tail-callee's span lands on the
/// original caller's overlay words (the frame is reused, never repointed).
#[test]
// caller/callee pairs are the point of every slide test
#[allow(clippy::similar_names)]
fn tail_called_span_returns_through_the_reused_frame() {
    let callee = def("sum3", &[I64, I64, I64, I64, Tag, I64, I64], 3, 0, &[
        Uop::Funcf { frame_slots: 7 },
        Uop::AddI64 {
            dst: 3,
            lhs: 0,
            rhs: 1,
        },
        Uop::AddI64 {
            dst: 3,
            lhs: 3,
            rhs: 2,
        },
        Uop::TagNew {
            dst: 4,
            tag: tags::CONFIG,
        },
        Uop::Mov { dst: 5, src: 3 },
        Uop::Kimm { dst: 6, kidx: k(3) },
        Uop::RetSpan { src: 4, n: 3 },
    ]);
    let mut caller = def("tc", &[Raw, Raw, Raw], 0, 3, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Kimm { dst: 0, kidx: k(2) },
        Uop::Kimm { dst: 1, kidx: k(4) },
        Uop::Kimm { dst: 2, kidx: k(0) },
        Uop::Callt {
            callee: k(14),
            nargs: 3,
        },
        Uop::Halt { slot: 0 },
    ]);
    // the shared fixture pool's k14 names the "cfg" module; repoint it at
    // this module's own callee (function-table index 1)
    caller.pool[14] = Const::FuncRef {
        module: "t".into(),
        func: 1,
    };
    let mut vm = Vm::new(registry("t", vec![caller, callee]));
    vm.capture_output();
    assert_eq!(vm.run("t", "tc", &[]), Ok(Value::Tag(tags::CONFIG)));
    // the frame was reused at the same base; only the head word survives
    // the root RET, so the stack is the metadata word plus the result
    assert_eq!(vm.stack_len(), 2);
}

/// A CALLT to a native: the builtin runs, then its unit word returns from
/// the tail position.
#[test]
fn tail_call_to_a_native_prints_and_returns_the_unit_word() {
    let body = def("printer", &[Ref, Raw], 1, 1, &[
        Uop::Funcf { frame_slots: 2 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Callt {
            callee: k(18),
            nargs: 1,
        },
        Uop::Halt { slot: 0 },
    ]);
    let mut vm = Vm::new(registry("t", vec![body]));
    vm.capture_output();
    let msg = vm.intern_str("done");
    assert_eq!(vm.run("t", "printer", &[msg]), Ok(Value::I64(0)));
    assert_eq!(vm.captured_output(), b"done\n");
}

/// Hand-assembly edge coverage: the typed guards the new ops add.
#[test]
fn retspan_edges_trap_and_load_checks_reject() {
    // a callee returning a span wider than the caller's overlay area
    let wide = def("wide_ret", &[Tag, I64, I64, I64], 0, 0, &[
        Uop::Funcf { frame_slots: 4 },
        Uop::TagNew {
            dst: 0,
            tag: tags::CONFIG,
        },
        Uop::Kimm { dst: 1, kidx: k(0) },
        Uop::Kimm { dst: 2, kidx: k(0) },
        Uop::Kimm { dst: 3, kidx: k(0) },
        Uop::RetSpan { src: 0, n: 4 },
    ]);
    let mut narrow = def("narrow", &[Raw, Raw], 0, 2, &[
        Uop::Funcf { frame_slots: 2 },
        Uop::Call {
            callee: k(0),
            nargs: 0,
        },
        Uop::Halt { slot: 0 },
    ]);
    // repoint the shared pool's k0 (an i64) at this module's wide callee
    narrow.pool[0] = Const::FuncRef {
        module: "e".into(),
        func: 1,
    };
    let mut vm = Vm::new(registry("e", vec![narrow, wide]));
    assert_eq!(
        vm.run("e", "narrow", &[]),
        Err(Trap::RetSpanTooWide {
            span: 4,
            caller_area: 2
        })
    );

    // a bogus Ref word reaches println: the object store names it dead
    let printer = def("printer", &[Ref, Raw], 1, 1, &[
        Uop::Funcf { frame_slots: 2 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Call {
            callee: k(18),
            nargs: 1,
        },
        Uop::Halt { slot: 0 },
    ]);
    let mut vm = Vm::new(registry("e", vec![printer]));
    assert_eq!(
        vm.run("e", "printer", &[Value::Ref(9999)]),
        Err(Trap::UnknownObject { obj: 9999 })
    );

    // a Bool staged for the I64 str_int parameter: the native's call-site
    // value check names the slot
    let badstage = def("badstage", &[Raw, Raw], 0, 2, &[
        Uop::Funcf { frame_slots: 2 },
        Uop::Kconst {
            dst: 0,
            kidx: k(12),
        },
        Uop::Call {
            callee: k(19),
            nargs: 1,
        },
        Uop::Halt { slot: 0 },
    ]);
    let mut vm = Vm::new(registry("e", vec![badstage]));
    assert_eq!(
        vm.run("e", "badstage", &[]),
        Err(Trap::CallLayout(SlideError::TypeMismatch {
            index: 0,
            staged: Bool,
            expected: I64,
        }))
    );

    // a native call whose staging area is empty: StagingTooSmall at the
    // call site, the same SlideError a uop callee's layout yields
    let nostage = def("nostage", &[I64], 0, 0, &[
        Uop::Funcf { frame_slots: 1 },
        Uop::Call {
            callee: k(18),
            nargs: 1,
        },
        Uop::Halt { slot: 0 },
    ]);
    let mut vm = Vm::new(registry("e", vec![nostage]));
    assert_eq!(
        vm.run("e", "nostage", &[]),
        Err(Trap::CallLayout(SlideError::StagingTooSmall {
            available: 0,
            needed: 1
        }))
    );

    // load-time: RETSPAN past the function's own slots is a static fact
    let over = def("over", &[Tag, I64, I64], 0, 0, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::RetSpan { src: 2, n: 4 },
    ]);
    assert_eq!(
        ModuleVersion::new("e", vec![over]).unwrap_err(),
        ModuleBuildError::RetSpanLayout {
            func: "over".into(),
            src: 2,
            span: 4,
            slots: 3,
        }
    );

    // load-time: KSTR on a non-string pool entry is a pool-kind violation
    let mut badkind = def("badkind", &[Ref], 0, 0, &[
        Uop::Funcf { frame_slots: 1 },
        Uop::Kstr { dst: 0, kidx: k(0) },
        Uop::Ret { src: 0 },
    ]);
    badkind.pool = vec![Const::I64(7)];
    assert_eq!(
        ModuleVersion::new("e", vec![badkind]).unwrap_err(),
        ModuleBuildError::PoolKind {
            func: "badkind".into(),
            kidx: 0,
            expected: ds_vm::ConstKind::Str,
            found: ds_vm::ConstKind::I64,
        }
    );
}

/// Native module resolution: both tables are contract-checked and the
/// registry routes FuncRefs to them through the same path as uop callees.
#[test]
fn both_native_modules_are_installed_and_resolvable() {
    let vm = Vm::new(cfg_registry());
    let reg = vm.registry();
    // the builtin core: the str conversions and string ops, the gaps as
    // reserved entries, and the appended list block (20-23)
    assert!(reg.resolve_native("builtin", BUILTIN_STR_INT).is_some());
    assert!(reg.resolve_native("builtin", BUILTIN_STR_FLOAT).is_some());
    assert!(reg.resolve_native("builtin", BUILTIN_STR_BOOL).is_some());
    assert!(reg.resolve_native("builtin", BUILTIN_LEN).is_some());
    assert!(reg.resolve_native("builtin", BUILTIN_CHAR_AT).is_some());
    assert!(reg.resolve_native("builtin", 0).is_some(), "a gap entry");
    assert!(
        reg.resolve_native("builtin", 19).is_some(),
        "the spare slot"
    );
    assert!(reg.resolve_native("builtin", BUILTIN_LIST_NEW).is_some());
    assert!(reg.resolve_native("builtin", BUILTIN_LIST_LEN).is_some());
    assert!(reg.resolve_native("builtin", BUILTIN_LIST_GET).is_some());
    assert!(reg.resolve_native("builtin", BUILTIN_LIST_CONCAT).is_some());
    // the fiber substrate's control pair (24-25) and the nursery join (26)
    assert!(reg.resolve_native("builtin", BUILTIN_FIBER_YIELD).is_some());
    assert!(
        reg.resolve_native("builtin", BUILTIN_FIBER_RESUME)
            .is_some()
    );
    assert!(
        reg.resolve_native("builtin", BUILTIN_NURSERY_JOIN)
            .is_some()
    );
    assert!(reg.resolve_native("builtin", BUILTIN_LIST_PUSH).is_some());
    assert!(
        reg.resolve_native("builtin", 28).is_none(),
        "28 table entries: the core's, the list block, the fiber pair, the join, the push"
    );
    // the std platform module: console, file, net — its own gaps reserved
    assert!(
        reg.resolve_native(STD_PLATFORM_MODULE, STD_PRINTLN)
            .is_some()
    );
    assert!(reg.resolve_native(STD_PLATFORM_MODULE, STD_PRINT).is_some());
    assert!(
        reg.resolve_native(STD_PLATFORM_MODULE, STD_NET_CONNECT)
            .is_some()
    );
    assert!(
        reg.resolve_native(STD_PLATFORM_MODULE, STD_NET_RECV)
            .is_some()
    );
    assert!(
        reg.resolve_native(STD_PLATFORM_MODULE, STD_HTTP_GET)
            .is_some()
    );
    assert!(
        reg.resolve_native(STD_PLATFORM_MODULE, 19).is_some(),
        "a gap entry"
    );
    assert!(reg.resolve_native(STD_PLATFORM_MODULE, 25).is_none());
    // a bare VM's registry carries no other native module
    let bare = Vm::new(ModuleRegistry::new());
    assert!(
        bare.registry()
            .resolve_native(STD_PLATFORM_MODULE, 0)
            .is_none()
    );
    assert!(reg.resolve_native("other", 0).is_none());
}
