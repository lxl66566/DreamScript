//! The shared generator for the VM execution fuzz leg: fuzz bytes become a
//! structurally valid-ish dsmod module — a small deterministic uop-program
//! builder — assembled through ds-mod's public encode path, written, read
//! back through the real reader, installed into a budgeted VM, and run.
//! The property under test is the VM's own discipline: decode, validate,
//! install, and execute never panic, and every stop is structured (a halt
//! value, a trap, or a budget stop). Included by the cargo-fuzz target
//! `fuzz_vm` and the nextest mirror in `crates/ds-vm/tests/fuzz_vm.rs`.
//!
//! Termination (why every generated run stops by itself):
//! - jumps are forward-only except each loop body's one template back-edge, a Jmpif whose counter
//!   slot the template decrements exactly once per pass; the four template slots are excluded from
//!   every random write (destinations remap around them; call overlays land in the argument area),
//!   so the trip count is the pool constant, at most 5
//! - CALL/CALLS recursion consumes stack words per frame and the VM runs under `Vm::with_limit`
//!   (128 words), so depth is bounded and StackOverflow is a routinely reachable structured stop
//! - CALLT targets only strictly-lower functions or natives, so a tail chain strictly descends and
//!   cannot cycle
//! - work stays bounded: same-module call edges point strictly down the function table (self-calls
//!   only in the loop-free lowest function, one site), at most two CALL/CALLS sites and one CALLT
//!   per body, loop trip counts at most 5, and the generated natives are deterministic and
//!   non-blocking (the std references are the captured-output console pair only — file and net
//!   natives stay out of the vocabulary)

use ds_abi::{
    FrameLayout, SlotType,
    uop::{ConstIdx, Inst, RcOp, Uop},
};
use ds_mod::{Constant, Function, Hash128, Module, TargetClass, read, write};
use ds_vm::{
    ModuleRegistry, Trap, Vm,
    native::{self, BUILTIN_MODULE, STD_PLATFORM_MODULE},
};

/// The generated module's name (the run entry target).
pub const MODULE_NAME: &str = "fzm";
/// The entry function every run drives (the highest table index).
pub const ENTRY_NAME: &str = "main";
/// The tight value-stack word budget: bounds recursion depth and makes the
/// stack-limit stop routinely reachable.
const STACK_LIMIT: usize = 128;
/// The string pool entry (the Kstr source).
const STR_KIDX: u16 = 10;

/// Direct natives the generator may CALL/CALLT, as (index, params) in the
/// builtin core — builtins resolve in every VM; reserved slots answer
/// their reserved trap — plus the std console pair (captured output; the
/// file and net surfaces stay unreferenced so execution is hermetic).
const NATIVES: &[(u32, &[SlotType])] = &[
    (2, &[SlotType::I64]),
    (3, &[SlotType::F64]),
    (4, &[SlotType::Bool]),
    (11, &[SlotType::Ref, SlotType::I64, SlotType::I64]),
    (12, &[SlotType::Ref, SlotType::Ref]),
    (13, &[SlotType::Ref]),
    (14, &[SlotType::Ref]),
    (15, &[SlotType::Ref]),
    (16, &[SlotType::Ref]),
    (17, &[SlotType::Ref, SlotType::Ref]),
    (18, &[SlotType::Ref, SlotType::I64]),
    (20, &[]),
    (24, &[SlotType::I64]),
    (25, &[SlotType::I64, SlotType::I64]),
    (26, &[SlotType::I64]),
    (27, &[SlotType::Ref, SlotType::Raw]),
    (28, &[]),
    (31, &[SlotType::Ref, SlotType::Raw, SlotType::Raw]),
    (0, &[]),
    (1, &[]),
    (19, &[]),
];
const STD_CONSOLE: &[(u32, &[SlotType])] = &[(0, &[SlotType::Ref]), (1, &[SlotType::Ref])];
/// The CALLG-addressed generic bodies: (index, param count, evidence words).
const GENERICS: &[(u32, u8, u8)] = &[
    (10, 1, 1),
    (21, 1, 1),
    (22, 2, 1),
    (23, 2, 1),
    (29, 1, 2),
    (30, 2, 2),
    (32, 2, 1),
    (33, 1, 1),
    (34, 1, 1),
    (35, 1, 1),
    (36, 1, 1),
    (37, 2, 1),
    (38, 2, 2),
    (39, 2, 1),
    (40, 3, 1),
];

/// Byte stream over the fuzz input: cycles through the data, so any input
/// (including empty) yields a full module and any mutation shifts the
/// generated program deterministically.
pub struct Stream<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Stream<'a> {
    /// Wraps the fuzz input.
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn byte(&mut self) -> u8 {
        if self.data.is_empty() {
            return 0;
        }
        let b = self.data[self.pos % self.data.len()];
        self.pos = self.pos.wrapping_add(1);
        b
    }

    fn below(&mut self, bound: usize) -> usize {
        usize::from(self.byte()) % bound.max(1)
    }
}

/// One function's decided shape, fixed before any body is emitted. The
/// frame is `[params | usable | template(4 when looping) | arg area]`.
struct Plan {
    index: usize,
    has_loop: bool,
    slots: Vec<SlotType>,
    params: usize,
    arg_area: usize,
    /// `params + usable`: where the template block starts when looping.
    body_span: usize,
    /// The primary native's index: its parameter list types the overlay
    /// prefix and its pool FuncRef is the body's first builtin entry.
    native1_index: usize,
}

impl Plan {
    /// The destination domain: every writable slot except the template
    /// block (mapped around it into the argument area).
    fn dst_span(&self) -> usize {
        self.slots.len() - if self.has_loop { 4 } else { 0 }
    }

    /// Maps a roll over the destination domain onto a real slot index.
    fn dst(&self, roll: usize) -> u8 {
        let raw = if !self.has_loop || roll < self.body_span { roll } else { roll + 4 };
        u8::try_from(raw.min(self.slots.len() - 1)).expect("slot indices are tiny")
    }

    /// A destination whose static type accepts `want` when one exists
    /// (a `Raw` slot accepts everything), else a uniformly random one:
    /// writer ops land type-consistently often enough for straight-line
    /// prefixes to run deep.
    fn typed_dst(&self, rng: &mut Stream, want: SlotType) -> u8 {
        let mut candidates = [0u8; 16];
        let mut found = 0;
        for roll in 0..self.dst_span() {
            let slot = self.dst(roll);
            let ty = self.slots[usize::from(slot)];
            if (ty == want || ty == SlotType::Raw) && found < candidates.len() {
                candidates[found] = slot;
                found += 1;
            }
        }
        if found > 0 && rng.below(4) > 0 {
            candidates[rng.below(found)]
        } else {
            self.dst(rng.below(self.dst_span()))
        }
    }

    /// Any slot is readable (counter reads are harmless; only writes to
    /// the template block would break the loop invariant).
    fn src(&self, roll: usize) -> u8 {
        u8::try_from(roll.min(self.slots.len() - 1)).expect("slot indices are tiny")
    }
}

/// Call-site budgets per body: the work-bound guard.
#[derive(Default)]
struct Budgets {
    call_sites: u32,
    callt_sites: u32,
    self_used: bool,
}

/// Builds one function's plan: slot count, types, and the loop decision.
/// Slot weights lean on `Raw` (every write accepts; reads check the
/// concrete value) so type-consistent prefixes are common enough for
/// execution to run deep instead of trapping on the first typed write.
fn plan_function(rng: &mut Stream, index: usize, is_entry: bool) -> Plan {
    let params = if is_entry { 0 } else { rng.below(3) }; // 0..=2
    let usable = 1 + rng.below(3); // 1..=3
    let has_loop = rng.below(2) == 0;
    let reserved = if has_loop { 4 } else { 0 };
    let arg_area = if rng.below(10) < 8 {
        1 + rng.below(3) // 1..=3, wide enough to stage native args
    } else {
        rng.below(2)
    };
    // the primary native's parameter list types the overlay prefix, so its
    // staged arguments type-check by construction
    let native1_index = rng.below(NATIVES.len());
    let native1 = NATIVES[native1_index].1;
    let mut slots = Vec::with_capacity(params + usable + reserved + arg_area);
    let random_slot = |rng: &mut Stream| match rng.below(12) {
        0..=2 => SlotType::I64,
        3..=6 => SlotType::Raw,
        7 => SlotType::F64,
        8 => SlotType::Bool,
        9 | 10 => SlotType::Ref,
        _ => match rng.below(3) {
            0 => SlotType::Tag,
            1 => SlotType::Fn,
            _ => SlotType::I32,
        },
    };
    for _ in 0..params {
        slots.push(random_slot(rng));
    }
    for _ in 0..usable {
        slots.push(random_slot(rng));
    }
    let body_span = slots.len();
    for _ in 0..reserved {
        slots.push(SlotType::Raw);
    }
    for i in 0..arg_area {
        slots.push(native1.get(i).copied().unwrap_or(SlotType::Raw));
    }
    Plan {
        index,
        has_loop,
        slots,
        params,
        arg_area,
        body_span,
        native1_index,
    }
}

/// The per-function constant pool plus the indices its body addresses.
struct Pool {
    constants: Vec<Constant>,
    /// Same-module callees strictly below this index: (kidx, table index).
    down: Vec<(u16, usize)>,
    /// A self-call entry; only the loop-free lowest function gets one.
    self_kidx: Option<u16>,
    /// A Kconst Fn word naming the lowest function; loop-free bodies only.
    fn_kidx: Option<u16>,
    /// Two direct builtin entries: (kidx, index, params).
    natives: [(u16, u32, usize); 2],
    /// The CALLG entry: (kidx, index, params, evidence words).
    generic: (u16, u32, u8, u8),
    /// The std console entry: (kidx, index, params).
    std: (u16, u32, usize),
}

fn name_of(index: usize, total: usize) -> String {
    if index + 1 == total { ENTRY_NAME.to_owned() } else { format!("f{index}") }
}

/// Pool prefix (fixed indices): 0=i64 zero, 1=i64 one, 2=loop trip count,
/// 3=generic evidence instance, 4=small i64, 5=edge i64, 6=plain f64,
/// 7=edge f64, 8=false, 9=true, 10=short string.
fn build_pool(rng: &mut Stream, plan: &Plan, garbage: bool) -> Pool {
    let mut constants: Vec<Constant> = vec![
        Constant::I64(0),
        Constant::I64(1),
        Constant::I64(rng.below(5) as i64), // loop trip count, 0..=4
        Constant::I64(rng.below(5) as i64), // generic evidence instance
        Constant::I64(rng.below(32) as i64 - 16),
        Constant::I64([i64::MIN, i64::MAX, 1_000_000][rng.below(3)]),
        Constant::F64([0.5, -2.25, 17.25, 1e10][rng.below(4)]),
        Constant::F64([f64::NAN, f64::INFINITY, f64::MAX, -0.0][rng.below(4)]),
        Constant::Bool(false),
        Constant::Bool(true),
        Constant::Str(["", "a", "abcd", "hello world"][rng.below(4)].into()),
    ];
    fn push(constants: &mut Vec<Constant>, module: &str, func: u32) -> u16 {
        constants.push(Constant::FuncRef {
            module: module.to_owned(),
            func,
        });
        u16::try_from(constants.len() - 1).expect("pools stay far below u16")
    }

    let mut down = Vec::new();
    for j in 0..plan.index {
        down.push((push(&mut constants, "", j as u32), j));
    }
    let self_kidx = (plan.index == 0 && !plan.has_loop).then(|| push(&mut constants, "", 0));
    let fn_kidx = (!plan.has_loop).then(|| push(&mut constants, "", 0));
    let n1 = NATIVES[plan.native1_index];
    let n2 = NATIVES[rng.below(NATIVES.len())];
    let natives = [
        (push(&mut constants, BUILTIN_MODULE, n1.0), n1.0, n1.1.len()),
        (push(&mut constants, BUILTIN_MODULE, n2.0), n2.0, n2.1.len()),
    ];
    let g = GENERICS[rng.below(GENERICS.len())];
    let generic = (push(&mut constants, BUILTIN_MODULE, g.0), g.0, g.1, g.2);
    let s = STD_CONSOLE[rng.below(STD_CONSOLE.len())];
    let std = (
        push(&mut constants, STD_PLATFORM_MODULE, s.0),
        s.0,
        s.1.len(),
    );
    if garbage && plan.index == 0 {
        // one unresolved foreign reference: the loader's structured refusal
        push(&mut constants, "nowhere", 7);
    }
    Pool {
        constants,
        down,
        self_kidx,
        fn_kidx,
        natives,
        generic,
        std,
    }
}

/// Emits one function body over its plan: FUNCF, pre ops, the optional
/// countdown loop, post ops, and a final stop. Forward jump targets are
/// computed against `total` (a lower bound on the final length, since the
/// call-staging patterns may append extra words), so every target lands
/// in-body by construction.
fn emit_function(rng: &mut Stream, plans: &[Plan], plan: &Plan, garbage: bool) -> Function {
    let pool = build_pool(rng, plan, garbage);
    let mut code: Vec<Inst> = Vec::new();
    let mut budgets = Budgets::default();

    // sizes fixed first: `total` is the fixed part's length and a lower
    // bound of the final body (call-staging patterns only append words),
    // so forward targets picked against it always land in-body
    let pre = rng.below(4);
    let loop_body = if plan.has_loop { 1 + rng.below(4) } else { 0 };
    let post = rng.below(5);
    let total = 1 + pre + if plan.has_loop { 6 + loop_body } else { 0 } + post + 1;
    let emit = |code: &mut Vec<Inst>, rng: &mut Stream, budgets: &mut Budgets| {
        emit_op(
            rng,
            code,
            plan,
            plans,
            &pool,
            budgets,
            u32::try_from(total).expect("bodies stay tiny"),
        );
    };

    code.push(
        Uop::Funcf {
            frame_slots: u16::try_from(plan.slots.len()).expect("slot counts are tiny"),
        }
        .into(),
    );
    for _ in 0..pre {
        emit(&mut code, rng, &mut budgets);
    }
    if plan.has_loop {
        // the countdown: counter/one/zero/flag live in the reserved block
        let (c, one, zero, flag) = (
            plan.body_span as u8,
            (plan.body_span + 1) as u8,
            (plan.body_span + 2) as u8,
            (plan.body_span + 3) as u8,
        );
        code.push(
            Uop::Kimm {
                dst: c,
                kidx: ConstIdx(2),
            }
            .into(),
        );
        code.push(
            Uop::Kimm {
                dst: one,
                kidx: ConstIdx(1),
            }
            .into(),
        );
        code.push(
            Uop::Kimm {
                dst: zero,
                kidx: ConstIdx(0),
            }
            .into(),
        );
        let head = code.len() as u32;
        for _ in 0..loop_body {
            emit(&mut code, rng, &mut budgets);
        }
        let tail = code.len() as u32;
        code.push(
            Uop::SubI64 {
                dst: c,
                lhs: c,
                rhs: one,
            }
            .into(),
        );
        code.push(
            Uop::GtI64 {
                dst: flag,
                lhs: c,
                rhs: zero,
            }
            .into(),
        );
        code.push(
            Uop::Jmpif {
                cond: flag,
                target: head,
            }
            .into(),
        );
        // A random forward jump may land inside the three-word tail past
        // the decrement, which would give the back edge a cycle that never
        // decrements (an unbounded loop). Retarget any such landing onto
        // the SubI64 itself: every back-edge traversal then passes the
        // decrement exactly once, so the trip count stays the pool constant.
        for inst in &mut code {
            if let Inst::Uop(
                Uop::Jmp { target } | Uop::Jmpif { target, .. } | Uop::JmpifNot { target, .. },
            ) = inst
                && tail <= *target
                && *target < tail + 3
            {
                *target = tail;
            }
        }
    }
    for _ in 0..post {
        emit(&mut code, rng, &mut budgets);
    }
    code.push(
        if rng.below(2) == 0 {
            Uop::Ret {
                src: plan.src(rng.below(plan.slots.len())),
            }
        } else {
            Uop::Halt {
                slot: plan.src(rng.below(plan.slots.len())),
            }
        }
        .into(),
    );
    debug_assert!(code.len() >= total as usize);

    let layout =
        FrameLayout::new(plan.slots.clone(), plan.params, plan.arg_area).expect("consistent");
    Function::new(
        name_of(plan.index, plans.len()),
        layout,
        pool.constants,
        code,
    )
    .expect("generator produces valid functions")
}

/// Emits one random instruction (call sites may append a typed-argument
/// staging pattern first) at the body's current position.
fn emit_op(
    rng: &mut Stream,
    code: &mut Vec<Inst>,
    plan: &Plan,
    plans: &[Plan],
    pool: &Pool,
    budgets: &mut Budgets,
    total: u32,
) {
    // rare kidx drift exercises the load-time pool checks without often
    // killing the whole install
    let drift = |kidx: u16, rng: &mut Stream| -> u16 {
        if rng.below(128) == 0 { rng.below(pool.constants.len() + 4) as u16 } else { kidx }
    };
    let dst = |rng: &mut Stream| plan.dst(rng.below(plan.dst_span()));
    let src = |rng: &mut Stream| plan.src(rng.below(plan.slots.len()));
    let pc = code.len() as u32;
    let pick = rng.below(66);
    let inst = match pick {
        0..=7 => Uop::Kimm {
            dst: plan.typed_dst(rng, SlotType::I64),
            kidx: ConstIdx(drift(rng.below(6) as u16, rng)),
        },
        8..=11 => {
            let base = 6 + rng.below(4) as u16; // f64/bool entries
            let (kidx, want) = if rng.below(8) == 0 && pool.fn_kidx.is_some() {
                (pool.fn_kidx.expect("checked"), SlotType::Fn)
            } else if base < 8 {
                (base, SlotType::F64)
            } else {
                (base, SlotType::Bool)
            };
            Uop::Kconst {
                dst: plan.typed_dst(rng, want),
                kidx: ConstIdx(drift(kidx, rng)),
            }
        },
        12..=13 => Uop::Kstr {
            dst: plan.typed_dst(rng, SlotType::Ref),
            kidx: ConstIdx(drift(STR_KIDX, rng)),
        },
        14..=19 => {
            let which = rng.below(5);
            binop(rng, plan, which)
        },
        20..=22 => {
            let which = rng.below(4);
            fbinop(rng, plan, which)
        },
        23..=28 => {
            let which = rng.below(6);
            icmp(rng, plan, which)
        },
        29..=31 => {
            let which = rng.below(6);
            fcmp(rng, plan, which)
        },
        32 | 33 => {
            let d = plan.typed_dst(rng, SlotType::Bool);
            let l = src(rng);
            let r = src(rng);
            if pick == 32 {
                Uop::EqBool {
                    dst: d,
                    lhs: l,
                    rhs: r,
                }
            } else {
                Uop::NeqBool {
                    dst: d,
                    lhs: l,
                    rhs: r,
                }
            }
        },
        34 | 35 => {
            let d = plan.typed_dst(rng, SlotType::Bool);
            let l = src(rng);
            let r = src(rng);
            if pick == 34 {
                Uop::EqStr {
                    dst: d,
                    lhs: l,
                    rhs: r,
                }
            } else {
                Uop::NeqStr {
                    dst: d,
                    lhs: l,
                    rhs: r,
                }
            }
        },
        36 => {
            let d = plan.typed_dst(rng, SlotType::Ref);
            let l = src(rng);
            let r = src(rng);
            Uop::StrCat {
                dst: d,
                lhs: l,
                rhs: r,
            }
        },
        37 => Uop::ClosureNew {
            dst: plan.typed_dst(rng, SlotType::Ref),
            func: src(rng),
            n: rng.below(3) as u8,
        },
        38..=40 => {
            // small module tags mostly; the platform band 250..=255 rarely
            let tag = if rng.below(8) == 0 { 250 + rng.below(6) } else { rng.below(5) } as u8;
            match pick {
                38 => Uop::TagTest {
                    dst: plan.typed_dst(rng, SlotType::Bool),
                    src: src(rng),
                    tag,
                },
                39 => Uop::TagTestNot {
                    dst: plan.typed_dst(rng, SlotType::Bool),
                    src: src(rng),
                    tag,
                },
                _ => Uop::TagNew {
                    dst: plan.typed_dst(rng, SlotType::Tag),
                    tag,
                },
            }
        },
        41 | 42 => {
            let s = src(rng);
            if pick == 41 {
                Uop::CvtI2F {
                    dst: plan.typed_dst(rng, SlotType::F64),
                    src: s,
                }
            } else {
                Uop::CvtF2I {
                    dst: plan.typed_dst(rng, SlotType::I64),
                    src: s,
                }
            }
        },
        43 => Uop::Mov {
            dst: dst(rng),
            src: src(rng),
        },
        44 | 45 => {
            let op =
                if pick == 44 { RcOp::Inc { src: src(rng) } } else { RcOp::Dec { src: src(rng) } };
            code.push(Inst::Rc(op));
            return;
        },
        46..=49 => {
            // forward in-body targets only; the loop template owns the one
            // backward edge; saturating because staging words can push the
            // live pc past the fixed-part `total`, and then pc+1 (the final
            // stop still to come) is the only safe landing
            let span = total.saturating_sub(pc + 1).max(1) as usize;
            let target = pc + 1 + rng.below(span) as u32;
            let cond = src(rng);
            match rng.below(3) {
                0 => Uop::Jmp { target },
                1 => Uop::Jmpif { cond, target },
                _ => Uop::JmpifNot { cond, target },
            }
        },
        50..=61 => {
            emit_call_family(rng, code, plan, plans, pool, budgets);
            return;
        },
        62 => Uop::Ret { src: src(rng) },
        63 => {
            // the span must fit the callee's own frame (a load-time fact);
            // a frame too small for the widest span falls back to a Ret
            let n = 1 + rng.below(2) as u8;
            let max_src = plan.slots.len().saturating_sub(usize::from(n));
            if usize::from(n) > plan.slots.len() {
                Uop::Ret { src: src(rng) }
            } else {
                Uop::RetSpan {
                    src: rng.below(max_src + 1) as u8,
                    n,
                }
            }
        },
        _ => Uop::Halt { slot: src(rng) },
    };
    code.push(inst.into());
}

fn binop(rng: &mut Stream, plan: &Plan, which: usize) -> Uop {
    let dst = plan.typed_dst(rng, SlotType::I64);
    let lhs = plan.src(rng.below(plan.slots.len()));
    let rhs = plan.src(rng.below(plan.slots.len()));
    match which {
        0 => Uop::AddI64 { dst, lhs, rhs },
        1 => Uop::SubI64 { dst, lhs, rhs },
        2 => Uop::MulI64 { dst, lhs, rhs },
        3 => Uop::DivI64 { dst, lhs, rhs },
        _ => Uop::ModI64 { dst, lhs, rhs },
    }
}

fn fbinop(rng: &mut Stream, plan: &Plan, which: usize) -> Uop {
    let dst = plan.typed_dst(rng, SlotType::F64);
    let lhs = plan.src(rng.below(plan.slots.len()));
    let rhs = plan.src(rng.below(plan.slots.len()));
    match which {
        0 => Uop::AddF64 { dst, lhs, rhs },
        1 => Uop::SubF64 { dst, lhs, rhs },
        2 => Uop::MulF64 { dst, lhs, rhs },
        _ => Uop::DivF64 { dst, lhs, rhs },
    }
}

fn icmp(rng: &mut Stream, plan: &Plan, which: usize) -> Uop {
    let dst = plan.typed_dst(rng, SlotType::Bool);
    let lhs = plan.src(rng.below(plan.slots.len()));
    let rhs = plan.src(rng.below(plan.slots.len()));
    match which {
        0 => Uop::EqI64 { dst, lhs, rhs },
        1 => Uop::NeqI64 { dst, lhs, rhs },
        2 => Uop::LtI64 { dst, lhs, rhs },
        3 => Uop::GeI64 { dst, lhs, rhs },
        4 => Uop::LeI64 { dst, lhs, rhs },
        _ => Uop::GtI64 { dst, lhs, rhs },
    }
}

fn fcmp(rng: &mut Stream, plan: &Plan, which: usize) -> Uop {
    let dst = plan.typed_dst(rng, SlotType::Bool);
    let lhs = plan.src(rng.below(plan.slots.len()));
    let rhs = plan.src(rng.below(plan.slots.len()));
    match which {
        0 => Uop::EqF64 { dst, lhs, rhs },
        1 => Uop::NeqF64 { dst, lhs, rhs },
        2 => Uop::LtF64 { dst, lhs, rhs },
        3 => Uop::GeF64 { dst, lhs, rhs },
        4 => Uop::LeF64 { dst, lhs, rhs },
        _ => Uop::GtF64 { dst, lhs, rhs },
    }
}

/// Emits a CALL, CALLT, CALLG, or CALLS family member, respecting the
/// per-body call-site budgets that keep total work bounded. Occasionally
/// pre-stages typed arguments so native and generic bodies actually run.
fn emit_call_family(
    rng: &mut Stream,
    code: &mut Vec<Inst>,
    plan: &Plan,
    plans: &[Plan],
    pool: &Pool,
    budgets: &mut Budgets,
) {
    match rng.below(16) {
        0..=5 => emit_call(rng, code, plan, plans, pool, budgets),
        6 | 7 => emit_callt(rng, code, plan, plans, pool, budgets),
        8..=10 => emit_callg(rng, code, plan, pool),
        _ => emit_calls(rng, code, plan, plans, pool, budgets),
    }
}

fn fallback_load(rng: &mut Stream, code: &mut Vec<Inst>, plan: &Plan) {
    code.push(
        Uop::Kimm {
            dst: plan.dst(rng.below(plan.dst_span())),
            kidx: ConstIdx(rng.below(6) as u16),
        }
        .into(),
    );
}

fn emit_call(
    rng: &mut Stream,
    code: &mut Vec<Inst>,
    plan: &Plan,
    plans: &[Plan],
    pool: &Pool,
    budgets: &mut Budgets,
) {
    if budgets.call_sites >= 2 {
        fallback_load(rng, code, plan);
        return;
    }
    let roll = rng.below(10);
    let (kidx, params, is_native) = if roll < 3 && !pool.down.is_empty() {
        let (kidx, j) = pool.down[rng.below(pool.down.len())];
        (kidx, plans[j].params, false)
    } else if roll == 3 && pool.self_kidx.is_some() && !budgets.self_used {
        budgets.self_used = true;
        (
            pool.self_kidx.expect("checked above"),
            plans[0].params,
            false,
        )
    } else if roll < 8 {
        let (kidx, _, params) = pool.natives[rng.below(2)];
        (kidx, params, true)
    } else if roll == 8 {
        (pool.std.0, pool.std.2, true)
    } else {
        fallback_load(rng, code, plan);
        return;
    };
    budgets.call_sites += 1;
    if is_native && rng.below(2) == 0 {
        stage_native_args(rng, code, plan, params);
    }
    let nargs = if rng.below(4) > 0 {
        u8::try_from(params).expect("native arities are tiny")
    } else {
        rng.below(4) as u8
    };
    code.push(
        Uop::Call {
            callee: ConstIdx(kidx),
            nargs,
        }
        .into(),
    );
}

fn emit_callt(
    rng: &mut Stream,
    code: &mut Vec<Inst>,
    plan: &Plan,
    plans: &[Plan],
    pool: &Pool,
    budgets: &mut Budgets,
) {
    if budgets.callt_sites >= 1 {
        fallback_load(rng, code, plan);
        return;
    }
    // strictly-lower functions or natives only: a self tail call is an
    // infinite loop by construction
    let (kidx, params) = if !pool.down.is_empty() && rng.below(2) == 0 {
        let (kidx, j) = pool.down[rng.below(pool.down.len())];
        (kidx, plans[j].params)
    } else {
        let (kidx, _, params) = pool.natives[rng.below(2)];
        (kidx, params)
    };
    budgets.callt_sites += 1;
    let nargs = if rng.below(4) > 0 {
        u8::try_from(params).expect("native arities are tiny")
    } else {
        rng.below(4) as u8
    };
    code.push(
        Uop::Callt {
            callee: ConstIdx(kidx),
            nargs,
        }
        .into(),
    );
}

fn emit_callg(rng: &mut Stream, code: &mut Vec<Inst>, plan: &Plan, pool: &Pool) {
    let (kidx, _index, params, evidence) = pool.generic;
    let nargs = if rng.below(4) > 0 { params } else { rng.below(4) as u8 };
    if rng.below(2) == 0 {
        // stage a string receiver and valid instance evidence words so the
        // generic bodies (len[str] first) genuinely execute
        let start = plan.slots.len() - plan.arg_area;
        if usize::from(nargs) >= 1 && start < plan.slots.len() {
            code.push(
                Uop::Kstr {
                    dst: start as u8,
                    kidx: ConstIdx(STR_KIDX),
                }
                .into(),
            );
        }
        for i in 0..usize::from(evidence) {
            let slot = start + usize::from(nargs) + i;
            if slot < plan.slots.len() {
                code.push(
                    Uop::Kimm {
                        dst: slot as u8,
                        kidx: ConstIdx(3),
                    }
                    .into(),
                );
            }
        }
    }
    code.push(
        Uop::Callg {
            callee: ConstIdx(kidx),
            nargs,
        }
        .into(),
    );
}

fn emit_calls(
    rng: &mut Stream,
    code: &mut Vec<Inst>,
    plan: &Plan,
    plans: &[Plan],
    pool: &Pool,
    budgets: &mut Budgets,
) {
    let Some(fn_kidx) = pool.fn_kidx else {
        fallback_load(rng, code, plan);
        return;
    };
    if plan.index == 0 {
        // CALLS on the f0-naming word is a self-call: same one-site rule
        if budgets.self_used || budgets.call_sites >= 2 {
            fallback_load(rng, code, plan);
            return;
        }
        budgets.self_used = true;
    } else if budgets.call_sites >= 2 {
        fallback_load(rng, code, plan);
        return;
    }
    budgets.call_sites += 1;
    let slot = plan
        .slots
        .iter()
        .position(|t| *t == SlotType::Fn)
        .unwrap_or(usize::from(plan.dst(rng.below(plan.dst_span()))));
    if rng.below(4) > 0 {
        code.push(
            Uop::Kconst {
                dst: slot as u8,
                kidx: ConstIdx(fn_kidx),
            }
            .into(),
        );
    }
    let nargs = if rng.below(4) > 0 {
        u8::try_from(plans[0].params).expect("worker params are tiny")
    } else {
        rng.below(3) as u8
    };
    code.push(
        Uop::Calls {
            callee_slot: slot as u8,
            nargs,
        }
        .into(),
    );
}

/// Stages one typed argument word per native param into the outgoing
/// argument area, so the call that follows lands in a real native body.
fn stage_native_args(rng: &mut Stream, code: &mut Vec<Inst>, plan: &Plan, params: usize) {
    let start = plan.slots.len() - plan.arg_area;
    if start + params > plan.slots.len() {
        return; // area cannot hold the staged words; the call traps on layout
    }
    for i in 0..params {
        let dst = (start + i) as u8;
        let inst = match rng.below(3) {
            0 => Uop::Kimm {
                dst,
                kidx: ConstIdx(rng.below(2) as u16),
            },
            1 => Uop::Kconst {
                dst,
                kidx: ConstIdx(6 + rng.below(4) as u16),
            },
            _ => Uop::Kstr {
                dst,
                kidx: ConstIdx(STR_KIDX),
            },
        };
        code.push(inst.into());
    }
}

/// Builds a full module from the fuzz input.
pub fn build_module(rng: &mut Stream) -> Module {
    let funcs = 1 + rng.below(3); // 1..=3
    let garbage = rng.below(32) == 0;
    let plans: Vec<Plan> = (0..funcs)
        .map(|i| plan_function(rng, i, i + 1 == funcs))
        .collect();
    let mut module = Module::new(
        MODULE_NAME,
        TargetClass::Bytecode,
        Hash128::compute(b"vm execution fuzz"),
    );
    for plan in &plans {
        module = module.with_function(emit_function(rng, &plans, plan, garbage));
    }
    module
}

/// Runs the full pipeline over one fuzz input and asserts the discipline:
/// canonical write-read round trip, natives installed before the loader,
/// a structured install outcome (pool FuncRefs validate at install; entry
/// descriptors fill), a structured run stop (a value, or a trap whose
/// backtrace materializes), and the stack never passing the limit.
/// Returns 1 for a halted value, 0 for a structured trap.
pub fn exercise(data: &[u8]) -> u32 {
    let module = build_module(&mut Stream::new(data));
    let bytes = write(&module);
    let back = read(&bytes).expect("the writer's output is canonical");
    assert_eq!(
        write(&back),
        bytes,
        "the reader round trip stays byte-stable"
    );

    let mut registry = ModuleRegistry::new();
    // the install contract: the std platform natives enter the registry
    // before loader::install (the builtin core follows inside with_limit)
    registry.install_native(STD_PLATFORM_MODULE, native::std_platform_module());
    let mut vm = Vm::with_limit(registry, STACK_LIMIT);
    vm.capture_output();
    let installed = vm.install_dsmod(&back).is_ok();
    if installed {
        let version = vm.registry().slots(MODULE_NAME).expect("just installed");
        for (position, f) in version.current().functions().iter().enumerate() {
            assert_eq!(f.entry.resolve(), position, "install fills entry word 0");
        }
    }
    let result = vm.run(MODULE_NAME, ENTRY_NAME, &[]);
    if !installed {
        assert!(
            matches!(result, Err(Trap::UnknownFunction { .. })),
            "a refused install leaves no entry to run"
        );
    }
    let outcome = match result {
        Ok(value) => {
            std::hint::black_box(&value);
            1
        },
        Err(trap) => {
            std::hint::black_box(&trap);
            std::hint::black_box(vm.backtrace());
            0
        },
    };
    assert!(
        vm.max_stack_len() <= STACK_LIMIT,
        "the per-entry stack-limit discipline holds"
    );
    std::hint::black_box(vm.captured_output().len());
    outcome
}
