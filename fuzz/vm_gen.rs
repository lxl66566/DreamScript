//! The shared generator for the VM execution fuzz leg: fuzz bytes become a
//! structurally valid-ish dsmod module — a small deterministic uop-program
//! builder — assembled through ds-mod's public encode path, written, read
//! back through the real reader, installed into a budgeted VM, run, and
//! then driven over the fiber/nursery host face. The property under test
//! is the VM's own discipline: decode, validate, install, execute, and
//! switch never panic, and every stop is structured (a halt value, a trap,
//! or a budget stop). Included by the cargo-fuzz target `fuzz_vm` and the
//! nextest mirrors in `crates/ds-vm/tests/fuzz_vm.rs` and
//! `crates/ds-vm/tests/fuzz_vm_fibers.rs`.
//!
//! Vocabulary: the builtin core's scalar, string, list, map, and callable
//! words plus the fiber/nursery control seams — `fiber_yield`,
//! `fiber_resume`, `nursery_open`, `nursery_join`, `nursery_boundary`,
//! `nursery_cancel` at random call sites, and `nursery_spawn` only inside
//! the entry's template (see Termination) — plus the std console pair
//! (captured output; the file and net natives stay unreferenced so
//! execution is hermetic) and the host fiber face
//! (`open_root_nursery`/`spawn_fiber_in`/`join_nursery`/`teardown_nursery`/
//! `resume_fiber`) driven with staged Ref/string words. Four fixed
//! templates cover the span/evidence/deinit/defaults landings: the entry's
//! records-list block (a two-word element span through `list_new`/
//! `list_push`, the checked `list_get` read with its tag test,
//! `cons`/`tail` at the stride, and `map`/`filter` over the span through
//! the hosted seam), the
//! sentinel evidence pair (a serving body whose `$evidence` CALLG
//! dispatches under the exact arity, entered from the entry's concrete
//! staging site), the trait-defaults pair (a concrete pad leaf whose
//! signature carries the defaulted member's word, and a generic pad
//! serving body whose nested `$evidence` site stages the trait's
//! declared default among the real arguments — the F7 discipline — the
//! entry's two spellings staging the same count, an occasional drift
//! leg answering the structured arity trap), and the deinit-claim child
//! (a claimed string word with its establishment band, a countdown loop
//! whose taken back edge a raised cancel flag cuts, an unreachable
//! `$deinit` drop shape the teardown doors' directory scan recovers,
//! and the `$deinit` hook those doors fire) — the entry's nursery
//! template carries one fixed spawn site for it, whose recycle door
//! fires over the live spawner and so exercises the doors' takeover
//! rule: a door firing mid-flight parks the running fiber's context and
//! re-adopts it after the pass.
//!
//! Termination (why every generated run stops by itself):
//! - jumps are forward-only except each loop body's one template back-edge, a Jmpif whose counter
//!   slot the template decrements exactly once per pass; the reserved template blocks (the loop's
//!   four words, the entry nursery block's three) are excluded from every random write
//!   (destinations remap around them; call overlays land in the argument area), so the trip count
//!   is the pool constant, at most 5
//! - CALL/CALLS recursion consumes stack words per frame and the VM runs under `Vm::with_limit`
//!   (128 words), so depth is bounded and StackOverflow is a routinely reachable structured stop
//! - CALLT targets only strictly-lower functions or natives, so a tail chain strictly descends and
//!   cannot cycle
//! - the spawn tree is depth one: `nursery_spawn` call sites exist only in the entry body's
//!   template (the spawned child word names a non-entry function, whose body can never emit a spawn
//!   site), the template is straight-line and each of its landing points executes at most once, and
//!   the host driver spawns at most a handful per input, so no fiber tree recurses through
//!   boundaries (each join drive would nest one `run_nursery_to_death` per level)
//! - every switch makes progress: a parked fiber re-enters at the instruction after the call that
//!   parked it, and bodies are finite with bounded loop trips, so each fiber parks finitely often
//!   per drive; join/teardown drivers enter each ledgered child until it dies (a parked yield is
//!   bounced back in with the bare PLATFORM_NONE tag), so every drive returns. The substrate can
//!   hang by construction — a child yielding inside an infinite loop never lets a join driver
//!   return — and the VM has no step budget (the run limit is stack words; a steps limit is a ds-vm
//!   change outside this leg's boundary), so the generator's bounded-back-edge discipline is the
//!   fuzzer's only hang guard: any relaxation of the loop template must keep every backward edge
//!   passing the decrement exactly once
//! - work stays bounded: same-module call edges point strictly down the function table (self-calls
//!   only in the loop-free lowest function, one site), at most two CALL/CALLS sites and one CALLT
//!   per body, loop trip counts at most 5, and the generated natives are deterministic and
//!   non-blocking
//! - the fixed templates keep every backward edge bounded: the records-list, sentinel, and defaults
//!   blocks are straight-line, the sentinel chain cannot cycle because the evidence constants stage
//!   only the trivial leaf functions (or out-of-range garbage that traps), never the serving body
//!   itself, the defaults pair cannot cycle because its serving body's nested dispatch targets only
//!   the concrete leaf (a strictly-lower straight-line seam), and the deinit child's one back edge
//!   passes its countdown's decrement exactly once (the loop template's discipline), while the
//!   dedicated seam functions (the leaves, the serving bodies, the defaults pair, the deinit child
//!   and its hook) stay out of every random call, spawn, and host-driver path — only the fixed
//!   templates reach them
//!
//! Re-audit point: the emit path's target discipline — forward targets computed against the
//! fixed-part length (a lower bound, since call staging appends words), the retargeting of
//! stray landings onto the loop's decrement, and the retargeting of pre-loop landings inside
//! the loop template onto the loop's entry (a landing at the decrement or the body would
//! enter the countdown on uninitialized frame words, and recycled fiber chunks carry stale
//! values that can keep the decrement from ever reaching zero) — is the invariant to
//! re-check whenever the uop ABI changes. The fixed templates keep it: every template length
//! is decided before emission and emitted exactly (no staging append can grow a template
//! block), and no template word sits between the loop's decrement and its back edge.

use ds_abi::{
    FrameLayout, SlotType,
    uop::{ConstIdx, Inst, RcOp, Uop},
};
use ds_mod::{Constant, FuncRefKey, Function, Hash128, Module, TargetClass, read, write};
use ds_vm::{
    ChildOutcome, FiberId, FiberState, FuncRef, JoinOutcome, ModuleRegistry, NurseryId,
    NurseryState, Trap, Value, Vm,
    native::BUILTIN_MODULE,
    platform::{Platform, STD_NAME},
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
/// The checked-read discriminants the records template tests after a
/// `list_get` (the ok/none extraction's runtime half).
const PLATFORM_IO_OK: u8 = 255;
const PLATFORM_NONE: u8 = 251;
/// The reserved establishment-band tags, mirrored from ds-vm's claim-band
/// reservation: a deinit claim's dedicated band word holds SET from its
/// registration to its first death path, CLEAR after, and the reserved
/// values sit above every user tag id, so a stale word in a recycled
/// frame region can never spoof a live band.
const BAND_SET: u8 = u8::MAX - 8;
const BAND_CLEAR: u8 = u8::MAX - 9;

/// Direct natives the generator may CALL/CALLT, as (index, params) in the
/// builtin core — builtins resolve in every VM; reserved slots answer
/// their reserved trap — plus the std console pair (captured output; the
/// file and net surfaces stay unreferenced so execution is hermetic). The
/// control seams park and drive: a random `nursery_join`/`nursery_boundary`
/// bounces or drives a bounded nursery, a random `nursery_cancel` kills the
/// caller carrying its payload. `nursery_spawn` (42) stays out: a random
/// spawn site would break the depth-one spawn tree (see Termination), so
/// it appears only in the entry's template.
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
    (41, &[]),
    (43, &[SlotType::I64, SlotType::I64]),
    (44, &[SlotType::Tag]),
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
/// frame is `[params | usable | loop template (4 when looping) | records
/// list word (1, entry only) | nursery template (3, entry only) | arg
/// area]`. The flags are independent template decisions, so the count
/// stands.
#[allow(clippy::struct_excessive_bools)]
struct Plan {
    index: usize,
    has_loop: bool,
    /// Whether this (entry) body carries the in-language nursery template.
    nursery: bool,
    /// Whether this (entry) body carries the records-list template.
    records: bool,
    /// Whether this (entry) body carries the sentinel evidence template.
    sentinel: bool,
    /// Whether this (entry) body carries the trait-defaults template.
    defaults: bool,
    /// Whether this (entry) body carries the deinit-claim spawn site.
    deinit: bool,
    /// Whether this is a hand-emitted seam function (a sentinel leaf, the
    /// serving body, the deinit child and its hook): excluded from every
    /// random call, spawn, and host-driver path.
    seam: bool,
    slots: Vec<SlotType>,
    params: usize,
    arg_area: usize,
    /// `params + usable`: where the reserved template blocks start.
    body_span: usize,
    /// The primary native's index: its parameter list types the overlay
    /// prefix and its pool FuncRef is the body's first builtin entry.
    native1_index: usize,
}

impl Plan {
    /// The reserved template blocks' total width (loop four, records one,
    /// nursery three): excluded from every random destination.
    fn reserved(&self) -> usize {
        usize::from(self.has_loop) * 4 + usize::from(self.records) + usize::from(self.nursery) * 3
    }

    /// The destination domain: every writable slot except the reserved
    /// template blocks.
    fn dst_span(&self) -> usize {
        self.slots.len() - self.reserved()
    }

    /// Maps a roll over the destination domain onto a real slot index.
    fn dst(&self, roll: usize) -> u8 {
        let raw = if roll < self.body_span { roll } else { roll + self.reserved() };
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

    /// The entry's reserved records-list word (the list `Ref` the
    /// template's natives share).
    fn records_slot(&self) -> u8 {
        let at = self.body_span + if self.has_loop { 4 } else { 0 };
        u8::try_from(at).expect("slot indices are tiny")
    }

    /// The entry's reserved nursery block: (nursery id, fn word, closure).
    fn nursery_slots(&self) -> (u8, u8, u8) {
        let id = self.body_span + usize::from(self.has_loop) * 4 + usize::from(self.records);
        (
            u8::try_from(id).expect("slot indices are tiny"),
            u8::try_from(id + 1).expect("slot indices are tiny"),
            u8::try_from(id + 2).expect("slot indices are tiny"),
        )
    }

    /// The first overlay (argument-area) slot index.
    fn ov_slot(&self, index: usize) -> u8 {
        u8::try_from(self.slots.len() - self.arg_area + index).expect("slot indices are tiny")
    }

    /// Any slot is readable (counter reads are harmless; only writes to
    /// the template blocks would break the loop invariant).
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

/// The module-level template decisions, drawn before any plan so slot
/// layouts, function indexes, and the census account for them.
#[allow(clippy::struct_excessive_bools)]
struct Shape {
    /// The entry carries the records-list template (span elements through
    /// the list natives and the hosted combinators).
    records: bool,
    /// The module gains the sentinel evidence pair and serving body, and
    /// the entry stages the entering call.
    sentinel: bool,
    /// The sentinel pair's table indexes (leaf0, leaf1, serving).
    leaves: Option<(usize, usize, usize)>,
    /// The module gains the trait-defaults pair (the concrete pad leaf
    /// and the generic pad serving body), and the entry stages the
    /// defaulted dispatch.
    defaults: bool,
    /// The defaults pair's table indexes (leaf, serving).
    defaults_pair: Option<(usize, usize)>,
    /// The module gains the deinit child and its `$deinit` hook, and the
    /// entry's nursery template spawns the child.
    deinit: bool,
    /// The deinit pair's table indexes (child, hook).
    deinit_pair: Option<(usize, usize)>,
}

/// Builds one function's plan: slot count, types, and the loop decision.
/// Slot weights lean on `Raw` (every write accepts; reads check the
/// concrete value) so type-consistent prefixes are common enough for
/// execution to run deep instead of trapping on the first typed write.
/// The entry may carry the in-language nursery template when a non-entry
/// child function exists to name (`can_spawn`), plus the records/sentinel
/// templates when drawn.
fn plan_function(
    rng: &mut Stream,
    index: usize,
    is_entry: bool,
    can_spawn: bool,
    shape: &Shape,
) -> Plan {
    let params = if is_entry { 0 } else { rng.below(3) }; // 0..=2
    let usable = 1 + rng.below(3); // 1..=3
    let has_loop = rng.below(2) == 0;
    // the deinit shape forces the nursery template: the dedicated child
    // is the only spawnable target for its fixed site
    let nursery = is_entry && (shape.deinit || (can_spawn && rng.below(2) == 0));
    let records = is_entry && shape.records;
    let sentinel = is_entry && shape.sentinel;
    let defaults = is_entry && shape.defaults;
    let deinit = is_entry && shape.deinit;
    let reserved = usize::from(has_loop) * 4 + usize::from(records) + usize::from(nursery) * 3;
    // the entry's fixed templates need a four-word overlay (the sentinel's
    // staged argument span, the hosted combinator's two arguments plus two
    // evidence words, the checked read's three-word answer span), five
    // when the defaults block rides too (its receiver, default, and the
    // three-word evidence area)
    let wide_entry = records || sentinel;
    let arg_area = if wide_entry || nursery || defaults {
        if defaults { 5 } else { 4 }
    } else if rng.below(10) < 8 {
        1 + rng.below(3) // 1..=3, wide enough to stage native args
    } else {
        rng.below(2)
    };
    // the primary native's parameter list types the overlay prefix, so its
    // staged arguments type-check by construction; a template-bearing
    // body's overlay stays Raw (the templates round-trip ids, callable
    // words, and tag+payload spans through it)
    let overlay_raw = nursery || wide_entry || defaults;
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
        slots.push(if overlay_raw {
            SlotType::Raw
        } else {
            native1.get(i).copied().unwrap_or(SlotType::Raw)
        });
    }
    Plan {
        index,
        has_loop,
        nursery,
        records,
        sentinel,
        defaults,
        deinit,
        seam: false,
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
    /// The nursery control seams (open/spawn/boundary/join/cancel), when
    /// this body's plan carries the template.
    nursery: Option<NurserySeams>,
    /// The records-list template's seams, when this body carries it.
    records: Option<RecordsSeams>,
    /// The sentinel evidence template's seams, when this body carries it.
    sentinel: Option<SentinelSeams>,
    /// The trait-defaults template's seams, when this body carries it.
    defaults: Option<DefaultsSeams>,
}

/// Pool entries for the entry's records-list template: the two plain
/// natives (`list_new`, the variadic `list_push`) and the CALLG-addressed
/// generics (the checked `list_get`, the stride-aware `cons`/`tail`, and
/// the hosted combinator over the element span).
struct RecordsSeams {
    list_new: u16,
    push: u16,
    get: u16,
    cons: u16,
    tail: u16,
    /// The hosted combinator: (kidx, native index, evidence words) —
    /// `map` (38, two evidence words) or `filter` (39, one).
    comb: (u16, u32, u8),
}

/// Pool entries for the entry's sentinel evidence template: the serving
/// body's FuncRef and the two evidence-word constants (the concrete
/// leaf's table index, the rider-claiming leaf's table index).
struct SentinelSeams {
    serving: u16,
    leaf0: u16,
    leaf1: u16,
}

/// Pool entries for the entry's trait-defaults template: the generic pad
/// serving body's FuncRef, the concrete pad leaf's table index (the
/// staged evidence slot word), and the two i64 literals — the trait's
/// declared default (the omitted spelling stages it at the site) and a
/// written argument.
struct DefaultsSeams {
    serving: u16,
    leaf: u16,
    declared: u16,
    written: u16,
}

/// Pool FuncRef indices for the entry's nursery template.
struct NurserySeams {
    open: u16,
    spawn: u16,
    boundary: u16,
    join: u16,
    cancel: u16,
    /// The fixed deinit-child spawn site (kidx, table index), when the
    /// deinit shape is drawn.
    deinit_child: Option<(u16, usize)>,
}

fn name_of(index: usize, total: usize) -> String {
    if index + 1 == total { ENTRY_NAME.to_owned() } else { format!("f{index}") }
}

/// Pool prefix (fixed indices): 0=i64 zero, 1=i64 one, 2=loop trip count,
/// 3=generic evidence instance, 4=small i64, 5=edge i64, 6=plain f64,
/// 7=edge f64, 8=false, 9=true, 10=short string.
fn build_pool(rng: &mut Stream, plans: &[Plan], plan: &Plan, garbage: bool, shape: &Shape) -> Pool {
    fn push(constants: &mut Vec<Constant>, module: &str, func: u32) -> u16 {
        let key = if module.is_empty() {
            FuncRefKey::Dep(0) // the enclosing module
        } else {
            FuncRefKey::Native(module.to_owned())
        };
        constants.push(Constant::FuncRef { key, func });
        u16::try_from(constants.len() - 1).expect("pools stay far below u16")
    }
    fn push_i64(constants: &mut Vec<Constant>, value: i64) -> u16 {
        constants.push(Constant::I64(value));
        u16::try_from(constants.len() - 1).expect("pools stay far below u16")
    }

    let mut constants: Vec<Constant> = vec![
        Constant::I64(0),
        Constant::I64(1),
        Constant::I64(i64::try_from(rng.below(5)).expect("rng bounds are tiny")), /* loop trip count, 0..=4 */
        Constant::I64(i64::try_from(rng.below(5)).expect("rng bounds are tiny")), /* generic evidence instance */
        Constant::I64(i64::try_from(rng.below(32)).expect("rng bounds are tiny") - 16),
        Constant::I64([i64::MIN, i64::MAX, 1_000_000][rng.below(3)]),
        Constant::F64([0.5, -2.25, 17.25, 1e10][rng.below(4)]),
        Constant::F64([f64::NAN, f64::INFINITY, f64::MAX, -0.0][rng.below(4)]),
        Constant::Bool(false),
        Constant::Bool(true),
        Constant::Str(["", "a", "abcd", "hello world"][rng.below(4)].into()),
    ];
    let mut down = Vec::new();
    for (j, callee_plan) in plans.iter().enumerate().take(plan.index) {
        // the dedicated seam functions stay out of every random call and
        // spawn path (see Termination): only the fixed templates reach them
        if callee_plan.seam {
            continue;
        }
        down.push((
            push(
                &mut constants,
                "",
                u32::try_from(j).expect("function tables are tiny"),
            ),
            j,
        ));
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
    let std = (push(&mut constants, STD_NAME, s.0), s.0, s.1.len());
    let nursery = plan.nursery.then(|| NurserySeams {
        open: push(&mut constants, BUILTIN_MODULE, 41),
        spawn: push(&mut constants, BUILTIN_MODULE, 42),
        boundary: push(&mut constants, BUILTIN_MODULE, 43),
        join: push(&mut constants, BUILTIN_MODULE, 26),
        cancel: push(&mut constants, BUILTIN_MODULE, 44),
        deinit_child: (plan.deinit).then(|| {
            let (child, _) = shape
                .deinit_pair
                .expect("the deinit shape carries its function indexes");
            (
                push(
                    &mut constants,
                    "",
                    u32::try_from(child).expect("function tables are tiny"),
                ),
                child,
            )
        }),
    });
    let records = plan.records.then(|| {
        let (comb_index, comb_evidence) = if rng.below(2) == 0 { (38, 2) } else { (39, 1) };
        RecordsSeams {
            list_new: push(&mut constants, BUILTIN_MODULE, 20),
            push: push(&mut constants, BUILTIN_MODULE, 27),
            get: push(&mut constants, BUILTIN_MODULE, 22),
            cons: push(&mut constants, BUILTIN_MODULE, 32),
            tail: push(&mut constants, BUILTIN_MODULE, 34),
            comb: (
                push(&mut constants, BUILTIN_MODULE, comb_index),
                comb_index,
                comb_evidence,
            ),
        }
    });
    let defaults = plan.defaults.then(|| {
        let (leaf, serving) = shape
            .defaults_pair
            .expect("the defaults shape carries its function indexes");
        DefaultsSeams {
            serving: push(
                &mut constants,
                "",
                u32::try_from(serving).expect("function tables are tiny"),
            ),
            leaf: push_i64(
                &mut constants,
                i64::try_from(leaf).expect("tables are tiny"),
            ),
            declared: push_i64(&mut constants, 2),
            written: push_i64(&mut constants, 9),
        }
    });
    let sentinel = plan.sentinel.then(|| {
        let (leaf0, leaf1, serving) = shape
            .leaves
            .expect("the sentinel shape carries its function indexes");
        SentinelSeams {
            serving: push(
                &mut constants,
                "",
                u32::try_from(serving).expect("tables are tiny"),
            ),
            leaf0: push_i64(
                &mut constants,
                i64::try_from(leaf0).expect("tables are tiny"),
            ),
            leaf1: push_i64(
                &mut constants,
                i64::try_from(leaf1).expect("tables are tiny"),
            ),
        }
    });
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
        nursery,
        records,
        sentinel,
        defaults,
    }
}

/// The entry's in-language nursery template, decided before any body word
/// so its fixed instruction count enters `total`: one `nursery_open`, a
/// straight-line spawn run (each child names a non-entry function, so the
/// spawn tree stays depth one), and one ending — the boundary join, the
/// count join, or a cancel that kills the entry carrying the payload.
struct NurseryTemplate {
    spawns: Vec<SpawnSite>,
    end: NurseryEnd,
}

/// One template spawn site.
struct SpawnSite {
    /// A pool FuncRef naming the child function (a `down` entry).
    fn_kidx: u16,
    /// The child's function-table index (its params drive capture staging).
    child: usize,
    /// The closure path: captures (one typed word per child param,
    /// including Ref/string words whose counts cross at the copy-in)
    /// become the child's entry arguments; the scalar path stages a bare
    /// Fn word, so it only names a parameter-less child.
    closure: bool,
}

enum NurseryEnd {
    /// `nursery_boundary(id, mask)`: the boundary span (clean join or the
    /// first trapped cancel payload, contained in the staged mask).
    Boundary { mask_kidx: u16 },
    /// `nursery_join(id)`: the failed-children count.
    Join,
    /// `nursery_cancel(payload)`: the entry dies carrying the payload.
    Cancel { kstr: bool },
}

fn decide_nursery(rng: &mut Stream, plans: &[Plan], pool: &Pool) -> NurseryTemplate {
    // spawnable children only: no parameter may be typed I32 (no value
    // word inhabits the kind, so the entry check would refuse the spawn);
    // the scalar path additionally needs a parameter-less child
    let spawnable: Vec<(u16, usize)> = pool
        .down
        .iter()
        .copied()
        .filter(|entry| !plans[entry.1].slots[..plans[entry.1].params].contains(&SlotType::I32))
        .collect();
    let scalar_ok: Vec<(u16, usize)> = spawnable
        .iter()
        .copied()
        .filter(|entry| plans[entry.1].params == 0)
        .collect();
    let count = if spawnable.is_empty() {
        0
    } else {
        1 + rng.below(3) // 1..=3 children
    };
    let mut spawns: Vec<SpawnSite> = (0..count)
        .map(|_| {
            let closure = scalar_ok.is_empty() || rng.below(2) == 0;
            let (fn_kidx, child) = if closure {
                spawnable[rng.below(spawnable.len())]
            } else {
                scalar_ok[rng.below(scalar_ok.len())]
            };
            SpawnSite {
                fn_kidx,
                child,
                closure,
            }
        })
        .collect();
    // the fixed deinit-child site rides last, after any random child that
    // can cancel: driven in spawn order, the child then reaches its back
    // edge under a raised flag and dies at the cut door
    if let Some((fn_kidx, child)) = pool.nursery.as_ref().and_then(|n| n.deinit_child) {
        spawns.push(SpawnSite {
            fn_kidx,
            child,
            closure: false,
        });
    }
    let end = match rng.below(5) {
        0 => NurseryEnd::Boundary { mask_kidx: 0 }, // mask zero: every contained check fails
        1 | 2 => NurseryEnd::Boundary { mask_kidx: 1 }, // mask one: tag zero is contained
        3 => NurseryEnd::Join,
        _ => NurseryEnd::Cancel {
            kstr: rng.below(2) == 0,
        },
    };
    NurseryTemplate { spawns, end }
}

fn template_len(template: &NurseryTemplate, plans: &[Plan]) -> usize {
    let open = 2; // CALL nursery_open + Mov (the id into its reserved slot)
    let spawns: usize = template
        .spawns
        .iter()
        .map(|s| if s.closure { 5 + plans[s.child].params } else { 3 })
        .sum();
    let end = match template.end {
        NurseryEnd::Boundary { .. } => 3,
        NurseryEnd::Join | NurseryEnd::Cancel { .. } => 2,
    };
    open + spawns + end
}

/// Stages one capture word of the child's parameter type at overlay
/// position `i` — the words that cross at the spawn copy-in.
fn capture_word(rng: &mut Stream, pool: &Pool, dst: u8, ty: SlotType) -> Uop {
    match ty {
        SlotType::I64 => Uop::Kimm {
            dst,
            kidx: ConstIdx(u16::try_from(rng.below(2)).expect("rng bounds are tiny")),
        },
        SlotType::F64 => Uop::Kconst {
            dst,
            kidx: ConstIdx(6),
        },
        SlotType::Bool => Uop::Kconst {
            dst,
            kidx: ConstIdx(8),
        },
        SlotType::Tag => Uop::TagNew {
            dst,
            tag: u8::try_from(rng.below(5)).expect("rng bounds are tiny"),
        },
        // the string capture: a Ref word whose count crosses at the copy-in
        SlotType::Ref => Uop::Kstr {
            dst,
            kidx: ConstIdx(STR_KIDX),
        },
        // any callable word serves a fn-typed capture
        SlotType::Fn => Uop::Kconst {
            dst,
            kidx: ConstIdx(
                pool.down
                    .first()
                    .expect("the template names lower functions")
                    .0,
            ),
        },
        // no value word inhabits the kind; staging an i64 lets the
        // spawn's entry check refuse it
        SlotType::I32 | SlotType::Raw => Uop::Kimm {
            dst,
            kidx: ConstIdx(0),
        },
    }
}

fn emit_nursery(
    rng: &mut Stream,
    code: &mut Vec<Inst>,
    plans: &[Plan],
    plan: &Plan,
    pool: &Pool,
    template: &NurseryTemplate,
) {
    let seams = pool
        .nursery
        .as_ref()
        .expect("the template's plan built its seams");
    let (id_slot, fnw_slot, clo_slot) = plan.nursery_slots();
    // open the block's own nursery; the returned id lives in the reserved
    // slot and is re-staged before every call
    code.push(
        Uop::Call {
            callee: ConstIdx(seams.open),
            nargs: 0,
        }
        .into(),
    );
    code.push(
        Uop::Mov {
            dst: id_slot,
            src: plan.ov_slot(0),
        }
        .into(),
    );
    for site in &template.spawns {
        let params = &plans[site.child].slots[..plans[site.child].params];
        if site.closure {
            // the closure word: header fn plus one typed capture per child
            // param, staged at the overlay positions the closure adopts
            code.push(
                Uop::Kconst {
                    dst: fnw_slot,
                    kidx: ConstIdx(site.fn_kidx),
                }
                .into(),
            );
            for (i, &ty) in params.iter().enumerate() {
                code.push(capture_word(rng, pool, plan.ov_slot(i), ty).into());
            }
            code.push(
                Uop::ClosureNew {
                    dst: clo_slot,
                    func: fnw_slot,
                    n: u8::try_from(params.len()).expect("child params fit a capture count"),
                }
                .into(),
            );
            // re-stage the id (capture zero clobbered the overlay) and the
            // closure Ref word against the fn-typed parameter
            code.push(
                Uop::Mov {
                    dst: plan.ov_slot(0),
                    src: id_slot,
                }
                .into(),
            );
            code.push(
                Uop::Mov {
                    dst: plan.ov_slot(1),
                    src: clo_slot,
                }
                .into(),
            );
        } else {
            code.push(
                Uop::Mov {
                    dst: plan.ov_slot(0),
                    src: id_slot,
                }
                .into(),
            );
            code.push(
                Uop::Kconst {
                    dst: plan.ov_slot(1),
                    kidx: ConstIdx(site.fn_kidx),
                }
                .into(),
            );
        }
        code.push(
            Uop::Call {
                callee: ConstIdx(seams.spawn),
                nargs: 2,
            }
            .into(),
        );
    }
    match template.end {
        NurseryEnd::Boundary { mask_kidx } => {
            code.push(
                Uop::Mov {
                    dst: plan.ov_slot(0),
                    src: id_slot,
                }
                .into(),
            );
            code.push(
                Uop::Kimm {
                    dst: plan.ov_slot(1),
                    kidx: ConstIdx(mask_kidx),
                }
                .into(),
            );
            code.push(
                Uop::Call {
                    callee: ConstIdx(seams.boundary),
                    nargs: 2,
                }
                .into(),
            );
        },
        NurseryEnd::Join => {
            code.push(
                Uop::Mov {
                    dst: plan.ov_slot(0),
                    src: id_slot,
                }
                .into(),
            );
            code.push(
                Uop::Call {
                    callee: ConstIdx(seams.join),
                    nargs: 1,
                }
                .into(),
            );
        },
        NurseryEnd::Cancel { kstr } => {
            // the payload word: a string Ref rides the trap word; a small
            // tag exercises the containment check at a boundary
            if kstr {
                code.push(
                    Uop::Kstr {
                        dst: plan.ov_slot(0),
                        kidx: ConstIdx(STR_KIDX),
                    }
                    .into(),
                );
            } else {
                code.push(
                    Uop::TagNew {
                        dst: plan.ov_slot(0),
                        tag: u8::try_from(rng.below(5)).expect("rng bounds are tiny"),
                    }
                    .into(),
                );
            }
            code.push(
                Uop::Call {
                    callee: ConstIdx(seams.cancel),
                    nargs: 1,
                }
                .into(),
            );
        },
    }
}

/// The entry's records-list template, decided before any body word so
/// its fixed instruction count enters `total`: a two-word element span
/// staged per a two-parameter child's parameter kinds (the hosted
/// combinator hands that span to the child's parameters per element),
/// pushed onto a fresh list twice — the second append occasionally
/// disagreeing on width, the stride trap — read back through the checked
/// `list_get` with the discriminant tag test (the ok/none extraction's
/// runtime half), and carried through `cons`/`tail` at the stride plus
/// the hosted combinator when a fitting child exists.
struct RecordsPlan {
    /// The element span's word kinds (one staging op each).
    span: Vec<SlotType>,
    /// The combinator child (kidx, table index), when a two-parameter
    /// non-seam child exists.
    child: Option<(u16, usize)>,
    /// The second push stages a one-word element (the stride trap).
    stride_trap: bool,
    /// The checked read's index lands in range (else out of range, the
    /// none side of the extraction).
    in_range: bool,
}

fn decide_records(rng: &mut Stream, plans: &[Plan], pool: &Pool) -> RecordsPlan {
    let child = pool.down.iter().copied().find(|(_, j)| {
        let plan = &plans[*j];
        plan.params == 2 && !plan.slots[..2].contains(&SlotType::I32)
    });
    let span = match child {
        Some((_, j)) => plans[j].slots[..2].to_vec(),
        None => vec![SlotType::Ref, SlotType::I64],
    };
    RecordsPlan {
        span,
        child,
        stride_trap: rng.below(8) == 0,
        in_range: rng.below(2) == 0,
    }
}

/// The template's exact word count: every leg is fixed-shape (one staging
/// op per span word, one call), so the block emits its planned length.
fn records_len(records: &RecordsPlan, comb_evidence: u8) -> usize {
    // the second push: Mov plus the one-word element plus Call when
    // trapping, Mov plus two span words plus Call when repeating
    let second_push = 3 + usize::from(!records.stride_trap);
    let comb = if records.child.is_some() { 3 + usize::from(comb_evidence) } else { 0 };
    // list_new + save (2), first push (4), second push, checked read (4),
    // tag test (1), cons (5), tail (3), combinator
    2 + 4 + second_push + 4 + 1 + 5 + 3 + comb
}

fn emit_records(
    rng: &mut Stream,
    code: &mut Vec<Inst>,
    plan: &Plan,
    pool: &Pool,
    records: &RecordsPlan,
) {
    let seams = pool
        .records
        .as_ref()
        .expect("the records plan built its seams");
    let list = plan.records_slot();
    // the fresh list object; its Ref word lives in the reserved word so
    // the block's natives share one list
    code.push(
        Uop::Call {
            callee: ConstIdx(seams.list_new),
            nargs: 0,
        }
        .into(),
    );
    code.push(
        Uop::Mov {
            dst: list,
            src: plan.ov_slot(0),
        }
        .into(),
    );
    let stage_span = |rng: &mut Stream, code: &mut Vec<Inst>, at: usize| {
        for (i, &ty) in records.span.iter().enumerate() {
            code.push(capture_word(rng, pool, plan.ov_slot(at + i), ty).into());
        }
    };
    // the first push fixes the stride at the span's width
    code.push(
        Uop::Mov {
            dst: plan.ov_slot(0),
            src: list,
        }
        .into(),
    );
    stage_span(rng, code, 1);
    code.push(
        Uop::Call {
            callee: ConstIdx(seams.push),
            nargs: 3,
        }
        .into(),
    );
    // the second push repeats the stride, or disagrees on width (the trap)
    code.push(
        Uop::Mov {
            dst: plan.ov_slot(0),
            src: list,
        }
        .into(),
    );
    if records.stride_trap {
        code.push(
            Uop::Kimm {
                dst: plan.ov_slot(1),
                kidx: ConstIdx(0),
            }
            .into(),
        );
        code.push(
            Uop::Call {
                callee: ConstIdx(seams.push),
                nargs: 2,
            }
            .into(),
        );
    } else {
        stage_span(rng, code, 1);
        code.push(
            Uop::Call {
                callee: ConstIdx(seams.push),
                nargs: 3,
            }
            .into(),
        );
    }
    // the checked read: in range answers the ok span (the discriminant
    // plus one payload word per element word), out of range the none
    // discriminant with the rejected index
    code.push(
        Uop::Mov {
            dst: plan.ov_slot(0),
            src: list,
        }
        .into(),
    );
    code.push(
        Uop::Kimm {
            dst: plan.ov_slot(1),
            kidx: ConstIdx(if records.in_range { 0 } else { 5 }),
        }
        .into(),
    );
    code.push(
        Uop::Kimm {
            dst: plan.ov_slot(2),
            kidx: ConstIdx(3),
        }
        .into(),
    );
    code.push(
        Uop::Callg {
            callee: ConstIdx(seams.get),
            nargs: 2,
        }
        .into(),
    );
    let want = if records.in_range { PLATFORM_IO_OK } else { PLATFORM_NONE };
    code.push(
        Uop::TagTest {
            dst: plan.typed_dst(rng, SlotType::Bool),
            src: plan.ov_slot(0),
            tag: want,
        }
        .into(),
    );
    // cons at the stride: the head span rides the front, the list Ref is
    // the trailing named word
    stage_span(rng, code, 0);
    code.push(
        Uop::Mov {
            dst: plan.ov_slot(2),
            src: list,
        }
        .into(),
    );
    code.push(
        Uop::Kimm {
            dst: plan.ov_slot(3),
            kidx: ConstIdx(3),
        }
        .into(),
    );
    code.push(
        Uop::Callg {
            callee: ConstIdx(seams.cons),
            nargs: 3,
        }
        .into(),
    );
    // tail: a fresh list over the elements after the first
    code.push(
        Uop::Mov {
            dst: plan.ov_slot(0),
            src: list,
        }
        .into(),
    );
    code.push(
        Uop::Kimm {
            dst: plan.ov_slot(1),
            kidx: ConstIdx(3),
        }
        .into(),
    );
    code.push(
        Uop::Callg {
            callee: ConstIdx(seams.tail),
            nargs: 1,
        }
        .into(),
    );
    // the hosted combinator over the span: the staged Fn word names the
    // child whose parameters are the span's kinds
    if records.child.is_some() {
        let (kidx, _, evidence) = seams.comb;
        code.push(
            Uop::Mov {
                dst: plan.ov_slot(0),
                src: list,
            }
            .into(),
        );
        code.push(
            Uop::Kconst {
                dst: plan.ov_slot(1),
                kidx: ConstIdx(kidx),
            }
            .into(),
        );
        for i in 0..usize::from(evidence) {
            code.push(
                Uop::Kimm {
                    dst: plan.ov_slot(2 + i),
                    kidx: ConstIdx(3),
                }
                .into(),
            );
        }
        code.push(
            Uop::Callg {
                callee: ConstIdx(seams.comb.0),
                nargs: 2,
            }
            .into(),
        );
    }
}

/// The entry's sentinel evidence template: the entering site stages the
/// serving body's whole parameter span — the receiver, the evidence word
/// naming the dispatch target, two follow words — and CALLs it from the
/// concrete site; the serving body re-stages the receiver with I64 rider
/// words and dispatches through the `$evidence` sentinel under the exact
/// arity (the P0 shape).
enum SentinelEvidence {
    /// The concrete leaf: a one-parameter target, no riders.
    Leaf0,
    /// The rider-claiming leaf: a four-parameter target whose trailing
    /// three words ride as its pair area.
    Leaf1,
    /// An out-of-range index: the structured UnknownCallee refusal.
    Garbage,
}

struct SentinelPlan {
    evidence: SentinelEvidence,
    /// The entering CALL's staged count drifts (the arity trap).
    drift: bool,
}

fn decide_sentinel(rng: &mut Stream) -> SentinelPlan {
    SentinelPlan {
        evidence: match rng.below(6) {
            0..=2 => SentinelEvidence::Leaf0,
            3 | 4 => SentinelEvidence::Leaf1,
            _ => SentinelEvidence::Garbage,
        },
        drift: rng.below(6) == 0,
    }
}

fn emit_sentinel(code: &mut Vec<Inst>, plan: &Plan, pool: &Pool, sentinel: &SentinelPlan) {
    let seams = pool
        .sentinel
        .as_ref()
        .expect("the sentinel plan built its seams");
    let evidence = match sentinel.evidence {
        SentinelEvidence::Leaf0 => seams.leaf0,
        SentinelEvidence::Leaf1 => seams.leaf1,
        // the edge i64 (MIN/MAX/1e6): outside every table, so the
        // sentinel dispatch refuses structurally
        SentinelEvidence::Garbage => 5,
    };
    code.push(
        Uop::Kimm {
            dst: plan.ov_slot(0),
            kidx: ConstIdx(4),
        }
        .into(),
    );
    code.push(
        Uop::Kimm {
            dst: plan.ov_slot(1),
            kidx: ConstIdx(evidence),
        }
        .into(),
    );
    code.push(
        Uop::Kimm {
            dst: plan.ov_slot(2),
            kidx: ConstIdx(0),
        }
        .into(),
    );
    code.push(
        Uop::Kimm {
            dst: plan.ov_slot(3),
            kidx: ConstIdx(0),
        }
        .into(),
    );
    let nargs = if sentinel.drift { 2 } else { 4 };
    code.push(
        Uop::Call {
            callee: ConstIdx(seams.serving),
            nargs,
        }
        .into(),
    );
}

/// The entry's trait-defaults template, decided before any body word so
/// its fixed instruction count enters `total`: two entering legs stage
/// the receiver plus the trait's declared default (the omitted spelling
/// — the trait declaration is the defaulted member's single authority)
/// or a written argument, both spellings staging the same count, then
/// the serving body's own evidence area naming the concrete leaf; the
/// drawn drift leg stages no defaulted word, answering the structured
/// arity trap (the pre-fix shape: a defaulted member omitted at a
/// nested site staged nothing).
struct DefaultsPlan {
    drift: bool,
}

fn decide_defaults(rng: &mut Stream) -> DefaultsPlan {
    DefaultsPlan {
        drift: rng.below(6) == 0,
    }
}

/// The template's exact word count: two entering legs of five staged
/// words plus the call, the drift leg one staged word fewer.
fn defaults_len(drift: bool) -> usize {
    2 * 6 + usize::from(drift) * 5
}

fn emit_defaults(code: &mut Vec<Inst>, plan: &Plan, pool: &Pool, defaults: &DefaultsPlan) {
    let seams = pool
        .defaults
        .as_ref()
        .expect("the defaults plan built its seams");
    // the entering legs: the receiver (a small i64), the defaulted word
    // (the declared default or a written argument), and the serving
    // body's evidence area — the slot naming the concrete leaf, the
    // follow words padded with it
    for default_kidx in [seams.declared, seams.written] {
        for (i, kidx) in [4u16, default_kidx, seams.leaf, seams.leaf, seams.leaf]
            .into_iter()
            .enumerate()
        {
            code.push(
                Uop::Kimm {
                    dst: plan.ov_slot(i),
                    kidx: ConstIdx(kidx),
                }
                .into(),
            );
        }
        code.push(
            Uop::Call {
                callee: ConstIdx(seams.serving),
                nargs: 5,
            }
            .into(),
        );
    }
    if defaults.drift {
        for (i, kidx) in [4u16, seams.leaf, seams.leaf, seams.leaf]
            .into_iter()
            .enumerate()
        {
            code.push(
                Uop::Kimm {
                    dst: plan.ov_slot(i),
                    kidx: ConstIdx(kidx),
                }
                .into(),
            );
        }
        code.push(
            Uop::Call {
                callee: ConstIdx(seams.serving),
                nargs: 4,
            }
            .into(),
        );
    }
}

/// Emits the trait-defaults concrete leaf: the two-parameter pad body —
/// the receiver plus the defaulted member's word — answering their sum;
/// both spellings' staged words enter it at the exact arity.
fn emit_defaults_leaf(index: usize) -> Function {
    let code = [
        Uop::Funcf { frame_slots: 2 },
        Uop::AddI64 {
            dst: 0,
            lhs: 0,
            rhs: 1,
        },
        Uop::Ret { src: 0 },
    ];
    Function::new(
        format!("f{index}"),
        FrameLayout::new(vec![SlotType::I64, SlotType::I64], 2, 0).expect("consistent"),
        Vec::new(),
        code,
    )
    .expect("generator produces valid functions")
}

/// Emits the trait-defaults serving body: the receiver, the defaulted
/// member's word, and the pair area's three words as trailing
/// parameters; the body re-stages the receiver plus the trait's declared
/// default at its nested `$evidence` sentinel site — the omitted default
/// stages inside the real arguments (F7), the riders trailing — and the
/// evidence word it copies verbatim from its own frame names the
/// concrete leaf.
fn emit_defaults_serving(index: usize) -> Function {
    // [0..=4] the parameters (receiver, default, the pair area), [5..=9]
    // the overlay staging the nested dispatch
    let code = [
        Uop::Funcf { frame_slots: 10 },
        Uop::Mov { dst: 5, src: 0 },
        Uop::Kimm {
            dst: 6,
            kidx: ConstIdx(1),
        },
        Uop::Mov { dst: 7, src: 2 },
        Uop::Mov { dst: 8, src: 3 },
        Uop::Mov { dst: 9, src: 4 },
        Uop::Callg {
            callee: ConstIdx(0),
            nargs: 2,
        },
        Uop::Ret { src: 0 },
    ];
    let pool = vec![
        Constant::FuncRef {
            key: FuncRefKey::Native(ds_vm::EVIDENCE_MODULE.to_owned()),
            func: 1,
        },
        // the trait's declared default literal, the one the entering site
        // stages for the omitted spelling
        Constant::I64(2),
    ];
    Function::new(
        format!("f{index}"),
        FrameLayout::new(
            {
                let mut slots = vec![SlotType::I64; 5];
                slots.extend([SlotType::Raw; 5]);
                slots
            },
            5,
            5,
        )
        .expect("consistent"),
        pool,
        code,
    )
    .expect("generator produces valid functions")
}

/// Emits one function body over its plan: FUNCF, pre ops, the optional
/// countdown loop, the entry's optional records-list, sentinel, and
/// defaults templates, the optional entry nursery template, post ops,
/// and a final stop. Forward jump targets are computed against `total` (a lower
/// bound on the final length, since the call-staging patterns may append
/// extra words), so every target lands in-body by construction.
fn emit_function(
    rng: &mut Stream,
    plans: &[Plan],
    plan: &Plan,
    garbage: bool,
    shape: &Shape,
) -> Function {
    let pool = build_pool(rng, plans, plan, garbage, shape);
    let template = plan.nursery.then(|| decide_nursery(rng, plans, &pool));
    let records = plan.records.then(|| decide_records(rng, plans, &pool));
    let sentinel = plan.sentinel.then(|| decide_sentinel(rng));
    let defaults = plan.defaults.then(|| decide_defaults(rng));
    let comb_evidence = pool.records.as_ref().map_or(0, |seams| seams.comb.2);
    let mut code: Vec<Inst> = Vec::new();
    let mut budgets = Budgets::default();

    // sizes fixed first: `total` is the fixed part's length and a lower
    // bound of the final body (call-staging patterns only append words),
    // so forward targets picked against it always land in-body
    let pre = rng.below(4);
    let loop_body = if plan.has_loop { 1 + rng.below(4) } else { 0 };
    let post = rng.below(5);
    let nursery_len = template.as_ref().map_or(0, |t| template_len(t, plans));
    let records_fixed = records
        .as_ref()
        .map_or(0, |r| records_len(r, comb_evidence));
    let sentinel_len = if sentinel.is_some() { 5 } else { 0 };
    let defaults_fixed = defaults.as_ref().map_or(0, |d| defaults_len(d.drift));
    let total = 1
        + pre
        + if plan.has_loop { 6 + loop_body } else { 0 }
        + records_fixed
        + sentinel_len
        + defaults_fixed
        + nursery_len
        + post
        + 1;
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
            u8::try_from(plan.body_span).expect("slot indices are tiny"),
            u8::try_from(plan.body_span + 1).expect("slot indices are tiny"),
            u8::try_from(plan.body_span + 2).expect("slot indices are tiny"),
            u8::try_from(plan.body_span + 3).expect("slot indices are tiny"),
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
        let head = u32::try_from(code.len()).expect("generated bodies are tiny");
        for _ in 0..loop_body {
            emit(&mut code, rng, &mut budgets);
        }
        let tail = u32::try_from(code.len()).expect("generated bodies are tiny");
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
        // A jump emitted before the loop block that lands anywhere inside
        // the template enters the countdown on uninitialized frame words
        // — recycled fiber chunks carry stale values (the pool-slot reuse
        // the establishment band guards), so a stale counter or one could
        // keep the decrement from ever reaching zero and the loop would
        // run unbounded with no step budget to cut it. Retarget any such
        // pre-loop landing onto the loop's entry, so the counter/one/zero
        // Kinms run first and the trip count stays the pool constant.
        let (start, tail) = (
            usize::try_from(head).expect("bodies stay tiny") - 3,
            usize::try_from(tail).expect("bodies stay tiny"),
        );
        for (at, inst) in code.iter_mut().enumerate() {
            if let Inst::Uop(
                Uop::Jmp { target } | Uop::Jmpif { target, .. } | Uop::JmpifNot { target, .. },
            ) = inst
                && at < start
                && usize::try_from(*target).expect("targets are tiny") >= start
                && usize::try_from(*target).expect("targets are tiny") < tail + 3
            {
                *target = u32::try_from(start).expect("bodies stay tiny");
            }
        }
    }
    if let Some(records) = records {
        emit_records(rng, &mut code, plan, &pool, &records);
    }
    if let Some(sentinel) = sentinel {
        emit_sentinel(&mut code, plan, &pool, &sentinel);
    }
    if let Some(defaults) = defaults {
        emit_defaults(&mut code, plan, &pool, &defaults);
    }
    if let Some(template) = template {
        emit_nursery(rng, &mut code, plans, plan, &pool, &template);
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
    debug_assert!(code.len() >= total);

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
        if rng.below(128) == 0 {
            u16::try_from(rng.below(pool.constants.len() + 4)).expect("pools stay far below u16")
        } else {
            kidx
        }
    };
    let dst = |rng: &mut Stream| plan.dst(rng.below(plan.dst_span()));
    let src = |rng: &mut Stream| plan.src(rng.below(plan.slots.len()));
    let pc = u32::try_from(code.len()).expect("generated bodies are tiny");
    let pick = rng.below(66);
    let inst = match pick {
        0..=7 => Uop::Kimm {
            dst: plan.typed_dst(rng, SlotType::I64),
            kidx: ConstIdx(drift(
                u16::try_from(rng.below(6)).expect("rng bounds are tiny"),
                rng,
            )),
        },
        8..=11 => {
            let base = 6 + u16::try_from(rng.below(4)).expect("rng bounds are tiny"); // f64/bool entries
            let draw_fn = rng.below(8) == 0;
            let (kidx, want) = match pool.fn_kidx {
                Some(fn_kidx) if draw_fn => (fn_kidx, SlotType::Fn),
                _ if base < 8 => (base, SlotType::F64),
                _ => (base, SlotType::Bool),
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
            n: u8::try_from(rng.below(3)).expect("rng bounds are tiny"),
        },
        38..=40 => {
            // small module tags mostly; the platform band 250..=255 rarely
            let tag = if rng.below(8) == 0 {
                250 + u8::try_from(rng.below(6)).expect("rng bounds are tiny")
            } else {
                u8::try_from(rng.below(5)).expect("rng bounds are tiny")
            };
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
            let target = pc + 1 + u32::try_from(rng.below(span)).expect("rng bounds are tiny");
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
            let n = 1 + u8::try_from(rng.below(2)).expect("rng bounds are tiny");
            let max_src = plan.slots.len().saturating_sub(usize::from(n));
            if usize::from(n) > plan.slots.len() {
                Uop::Ret { src: src(rng) }
            } else {
                Uop::RetSpan {
                    src: u8::try_from(rng.below(max_src + 1)).expect("slot indices are tiny"),
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
            kidx: ConstIdx(u16::try_from(rng.below(6)).expect("rng bounds are tiny")),
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
    let (kidx, params, is_native, native_index) = if roll < 3 && !pool.down.is_empty() {
        let (kidx, j) = pool.down[rng.below(pool.down.len())];
        (kidx, plans[j].params, false, None)
    } else if roll == 3 && pool.self_kidx.is_some() && !budgets.self_used {
        budgets.self_used = true;
        (
            pool.self_kidx.expect("checked above"),
            plans[0].params,
            false,
            None,
        )
    } else if roll < 8 {
        let (kidx, index, params) = pool.natives[rng.below(2)];
        (kidx, params, true, Some(index))
    } else if roll == 8 {
        (pool.std.0, pool.std.2, true, None)
    } else {
        fallback_load(rng, code, plan);
        return;
    };
    budgets.call_sites += 1;
    if is_native {
        if matches!(native_index, Some(24 | 25)) {
            // the switch pair always stages plausible ids and payloads, so
            // a drive genuinely hops fibers (the O(1) switch path) instead
            // of trapping on stale overlay words
            stage_fiber_args(rng, code, plan, params);
        } else if rng.below(2) == 0 {
            stage_native_args(rng, code, plan, params);
        }
    }
    let nargs = if rng.below(4) > 0 {
        u8::try_from(params).expect("native arities are tiny")
    } else {
        u8::try_from(rng.below(4)).expect("rng bounds are tiny")
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
        u8::try_from(rng.below(4)).expect("rng bounds are tiny")
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
    let nargs = if rng.below(4) > 0 {
        params
    } else {
        u8::try_from(rng.below(4)).expect("rng bounds are tiny")
    };
    if rng.below(2) == 0 {
        // stage a string receiver and valid instance evidence words so the
        // generic bodies (len[str] first) genuinely execute
        let start = plan.slots.len() - plan.arg_area;
        if usize::from(nargs) >= 1 && start < plan.slots.len() {
            code.push(
                Uop::Kstr {
                    dst: u8::try_from(start).expect("slot indices are tiny"),
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
                        dst: u8::try_from(slot).expect("slot indices are tiny"),
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
                dst: u8::try_from(slot).expect("slot indices are tiny"),
                kidx: ConstIdx(fn_kidx),
            }
            .into(),
        );
    }
    let nargs = if rng.below(4) > 0 {
        u8::try_from(plans[0].params).expect("worker params are tiny")
    } else {
        u8::try_from(rng.below(3)).expect("rng bounds are tiny")
    };
    code.push(
        Uop::Calls {
            callee_slot: u8::try_from(slot).expect("slot indices are tiny"),
            nargs,
        }
        .into(),
    );
}

/// Stages plausible fiber ids and payloads for the switch pair: small
/// constants (pool ids of the joiner and the first few siblings as the
/// target, arbitrary words as payloads) into overlay slots that accept
/// an i64. Slots of other kinds stay as-is and the call traps on them
/// structurally.
fn stage_fiber_args(rng: &mut Stream, code: &mut Vec<Inst>, plan: &Plan, params: usize) {
    let start = plan.slots.len() - plan.arg_area;
    for i in 0..params.min(plan.arg_area) {
        let slot = plan.slots[start + i];
        if slot != SlotType::I64 && slot != SlotType::Raw {
            continue;
        }
        let dst = u8::try_from(start + i).expect("slot indices are tiny");
        let kidx = u16::try_from([0, 1, 4][rng.below(3)]).expect("rng bounds are tiny");
        code.push(
            Uop::Kimm {
                dst,
                kidx: ConstIdx(kidx),
            }
            .into(),
        );
    }
}

/// Stages one typed argument word per native param into the outgoing
/// argument area, so the call that follows lands in a real native body.
fn stage_native_args(rng: &mut Stream, code: &mut Vec<Inst>, plan: &Plan, params: usize) {
    let start = plan.slots.len() - plan.arg_area;
    if start + params > plan.slots.len() {
        return; // area cannot hold the staged words; the call traps on layout
    }
    for i in 0..params {
        let dst = u8::try_from(start + i).expect("slot indices are tiny");
        let inst = match rng.below(3) {
            0 => Uop::Kimm {
                dst,
                kidx: ConstIdx(u16::try_from(rng.below(2)).expect("rng bounds are tiny")),
            },
            1 => Uop::Kconst {
                dst,
                kidx: ConstIdx(6 + u16::try_from(rng.below(4)).expect("rng bounds are tiny")),
            },
            _ => Uop::Kstr {
                dst,
                kidx: ConstIdx(STR_KIDX),
            },
        };
        code.push(inst.into());
    }
}

/// One generated function's host-facing entry description: what the
/// host driver needs to spawn it (its name and parameter words).
pub struct ChildFn {
    pub name: String,
    pub params: Vec<SlotType>,
}

/// The module-level census of drawn templates, for the mirror's
/// draw-coverage assertions (plan-level facts: whether the shapes were
/// drawn, not whether a given run reached them deep).
pub struct Drawn {
    /// Entry bodies carrying the records-list template.
    pub records: u32,
    /// Entry bodies carrying the sentinel evidence template.
    pub sentinel: u32,
    /// Entry bodies carrying the trait-defaults template.
    pub defaults: u32,
    /// Entry bodies carrying the deinit-claim spawn site.
    pub deinit: u32,
}

/// A seam function's plan (the fixed layout its hand-emitted body uses):
/// the whole slot list plus its parameter count, so a hand-emitted body
/// can keep scratch words past its parameters.
fn seam_plan(index: usize, slots: Vec<SlotType>, params: usize, arg_area: usize) -> Plan {
    Plan {
        index,
        has_loop: false,
        nursery: false,
        records: false,
        sentinel: false,
        defaults: false,
        deinit: false,
        seam: true,
        body_span: params,
        params,
        arg_area,
        slots,
        native1_index: 0,
    }
}

/// Emits a sentinel leaf: a trivial body over I64 parameters (one word
/// for the concrete target, four for the rider-claiming target whose
/// trailing three parameters are its pair area).
fn emit_leaf(index: usize, wide: bool) -> Function {
    let n = if wide { 4 } else { 1 };
    let plan = seam_plan(index, vec![SlotType::I64; n], n, 0);
    let code = [
        Uop::Funcf {
            frame_slots: u16::try_from(plan.slots.len()).expect("slot counts are tiny"),
        },
        Uop::Ret { src: 0 },
    ];
    Function::new(
        format!("f{index}"),
        FrameLayout::new(plan.slots, plan.params, plan.arg_area).expect("consistent"),
        Vec::new(),
        code,
    )
    .expect("generator produces valid functions")
}

/// Emits the sentinel serving body: the receiver, the evidence word
/// naming the dispatch target, and two follow words as I64 parameters;
/// the body re-stages the receiver plus I64 rider words and dispatches
/// through the `$evidence` sentinel — CALLG mostly, CALLTG in the tail
/// variant (the leaf's answer rides the tail transfer).
fn emit_serving(index: usize, tail: bool) -> Function {
    let plan = seam_plan(
        index,
        {
            let mut slots = vec![SlotType::I64; 4];
            slots.extend([SlotType::Raw; 4]);
            slots
        },
        4,
        4,
    );
    let pool = vec![
        Constant::FuncRef {
            key: FuncRefKey::Native(ds_vm::EVIDENCE_MODULE.to_owned()),
            func: 1,
        },
        Constant::I64(0),
    ];
    let mut code = vec![
        Uop::Funcf {
            frame_slots: u16::try_from(plan.slots.len()).expect("slot counts are tiny"),
        },
        Uop::Mov {
            dst: plan.ov_slot(0),
            src: 0,
        },
        Uop::Kimm {
            dst: plan.ov_slot(1),
            kidx: ConstIdx(1),
        },
        Uop::Kimm {
            dst: plan.ov_slot(2),
            kidx: ConstIdx(1),
        },
        Uop::Kimm {
            dst: plan.ov_slot(3),
            kidx: ConstIdx(1),
        },
    ];
    if tail {
        code.push(Uop::Calltg {
            callee: ConstIdx(0),
            nargs: 1,
        });
        // unreachable past the tail transfer; a guard stop only
        code.push(Uop::Halt { slot: 0 });
    } else {
        code.push(Uop::Callg {
            callee: ConstIdx(0),
            nargs: 1,
        });
        code.push(Uop::Ret { src: 0 });
    }
    Function::new(
        format!("f{index}"),
        FrameLayout::new(plan.slots, plan.params, plan.arg_area).expect("consistent"),
        pool,
        code,
    )
    .expect("generator produces valid functions")
}

/// Emits the deinit child: a claimed string word with its establishment
/// band, a countdown loop whose taken back edge passes the decrement
/// exactly once (a raised cancel flag cuts the child there), and the
/// unreachable `$deinit` drop shape the teardown doors' directory scan
/// recovers — stage the span, call the hook, clear the band. The doors
/// fire the claim wherever the child dies: the cancel cut under a
/// raised flag, the death walk's interrupt marking, or the recycle door
/// over the dead slot.
fn emit_deinit_child(rng: &mut Stream, index: usize, (child, hook): (usize, usize)) -> Function {
    debug_assert_eq!(child, index);
    // [0] zero, [1] one, [2] the trip count, [3] the claimed word
    let pool = vec![
        Constant::I64(0),
        Constant::I64(1),
        Constant::I64(i64::try_from(rng.below(5)).expect("rng bounds are tiny")),
        Constant::Str("deinit value".into()),
    ];
    // body_len 6: [0] counter, [1] one, [2] zero, [3] the loop flag, [4]
    // the claim word, [5] its band; the overlay word at 6 stages the drop
    let code = [
        Uop::Funcf { frame_slots: 7 },
        Uop::Kimm {
            dst: 0,
            kidx: ConstIdx(2),
        },
        Uop::Kimm {
            dst: 1,
            kidx: ConstIdx(1),
        },
        Uop::Kimm {
            dst: 2,
            kidx: ConstIdx(0),
        },
        Uop::Kstr {
            dst: 4,
            kidx: ConstIdx(3),
        },
        Uop::TagNew {
            dst: 5,
            tag: BAND_SET,
        },
        Uop::SubI64 {
            dst: 0,
            lhs: 0,
            rhs: 1,
        },
        Uop::GtI64 {
            dst: 3,
            lhs: 0,
            rhs: 2,
        },
        Uop::Jmpif { cond: 3, target: 6 },
        Uop::Ret { src: 4 },
        Uop::Mov { dst: 6, src: 4 },
        Uop::Call {
            callee: ConstIdx(4),
            nargs: 1,
        },
        Uop::TagNew {
            dst: 5,
            tag: BAND_CLEAR,
        },
    ];
    let pool = {
        let mut pool = pool;
        // the drop call's FuncRef names this module: the doors' directory
        // scan matches the callee's module against the version's own name
        pool.push(Constant::FuncRef {
            key: FuncRefKey::Dep(0), // the enclosing module
            func: u32::try_from(hook).expect("function tables are tiny"),
        });
        pool
    };
    Function::new(
        format!("f{index}"),
        FrameLayout::new(
            vec![
                SlotType::I64,
                SlotType::I64,
                SlotType::I64,
                SlotType::Bool,
                SlotType::Ref,
                SlotType::Tag,
                SlotType::Raw,
            ],
            0,
            1,
        )
        .expect("generator produces valid layouts"),
        pool,
        code,
    )
    .expect("generator produces valid functions")
}

/// Emits the deinit child's `$deinit` hook: one borrowed `Ref` parameter
/// (the claim's span word), the hand-off count balanced at the entry,
/// answering a scalar word — only the teardown doors ever call it.
fn emit_deinit_hook(index: usize) -> Function {
    let code: Vec<Inst> = vec![
        Uop::Funcf { frame_slots: 2 }.into(),
        RcOp::Dec { src: 0 }.into(),
        Uop::Kconst {
            dst: 1,
            kidx: ConstIdx(0),
        }
        .into(),
        Uop::Ret { src: 1 }.into(),
    ];
    Function::new(
        format!("f{index}$deinit"),
        FrameLayout::new(vec![SlotType::Ref, SlotType::Bool], 1, 0).expect("consistent"),
        vec![Constant::Bool(false)],
        code,
    )
    .expect("generator produces valid functions")
}

/// Builds a full module plus the host driver's view of its functions and
/// the census of drawn templates. The layout is `[random non-entry
/// bodies | leaf0 leaf1 serving | entry]`: the dedicated seam functions
/// sit strictly below the entry, never in any random call, spawn, or
/// host-driver path, and the entry's fixed templates reach them through
/// their own pool entries.
pub fn build_planned(rng: &mut Stream) -> (Module, Vec<ChildFn>, Drawn) {
    let funcs = 1 + rng.below(3); // 1..=3 random bodies including the entry
    let garbage = rng.below(32) == 0;
    let mut shape = Shape {
        records: rng.below(3) == 0,
        sentinel: rng.below(3) == 0,
        defaults: rng.below(3) == 0,
        deinit: rng.below(3) == 0,
        leaves: None,
        defaults_pair: None,
        deinit_pair: None,
    };
    let mut base = funcs - 1;
    if shape.sentinel {
        shape.leaves = Some((base, base + 1, base + 2));
        base += 3;
    }
    if shape.defaults {
        shape.defaults_pair = Some((base, base + 1));
        base += 2;
    }
    if shape.deinit {
        shape.deinit_pair = Some((base, base + 1));
    }
    let serving_tail = shape.leaves.is_some() && rng.below(4) == 0;
    let mut plans: Vec<Plan> = (0..funcs - 1)
        .map(|i| plan_function(rng, i, false, funcs > 1, &shape))
        .collect();
    if let Some((l0, l1, serving)) = shape.leaves {
        plans.push(seam_plan(l0, vec![SlotType::I64], 1, 0));
        plans.push(seam_plan(l1, vec![SlotType::I64; 4], 4, 0));
        plans.push(seam_plan(
            serving,
            {
                let mut slots = vec![SlotType::I64; 4];
                slots.extend([SlotType::Raw; 4]);
                slots
            },
            4,
            4,
        ));
    }
    if let Some((leaf, serving)) = shape.defaults_pair {
        plans.push(seam_plan(leaf, vec![SlotType::I64, SlotType::I64], 2, 0));
        plans.push(seam_plan(
            serving,
            {
                let mut slots = vec![SlotType::I64; 5];
                slots.extend([SlotType::Raw; 5]);
                slots
            },
            5,
            5,
        ));
    }
    if let Some((child, hook)) = shape.deinit_pair {
        // the deinit child: [0] counter, [1] one, [2] zero, [3] the loop
        // flag, [4] the claimed string word, [5] its band, [6] the
        // overlay staging the drop shape
        plans.push(seam_plan(
            child,
            vec![
                SlotType::I64,
                SlotType::I64,
                SlotType::I64,
                SlotType::Bool,
                SlotType::Ref,
                SlotType::Tag,
                SlotType::Raw,
            ],
            0,
            1,
        ));
        // the hook: the borrowed Ref parameter plus its answer word
        plans.push(seam_plan(hook, vec![SlotType::Ref, SlotType::Bool], 1, 0));
    }
    plans.push(plan_function(rng, plans.len(), true, funcs > 1, &shape));
    let mut module = Module::new(
        MODULE_NAME,
        TargetClass::Bytecode,
        Hash128::compute(b"vm execution fuzz"),
    );
    let mut entries = Vec::with_capacity(funcs);
    let mut drawn = Drawn {
        records: 0,
        sentinel: 0,
        defaults: 0,
        deinit: 0,
    };
    for (i, plan) in plans.iter().enumerate() {
        let function = if shape.leaves.is_some_and(|(l0, ..)| l0 == i) {
            emit_leaf(i, false)
        } else if shape.leaves.is_some_and(|(_, l1, _)| l1 == i) {
            emit_leaf(i, true)
        } else if shape.leaves.is_some_and(|(_, _, s)| s == i) {
            emit_serving(i, serving_tail)
        } else if shape.defaults_pair.is_some_and(|(l, _)| l == i) {
            emit_defaults_leaf(i)
        } else if shape.defaults_pair.is_some_and(|(_, s)| s == i) {
            emit_defaults_serving(i)
        } else if shape.deinit_pair.is_some_and(|(c, _)| c == i) {
            emit_deinit_child(rng, i, shape.deinit_pair.expect("just checked"))
        } else if shape.deinit_pair.is_some_and(|(_, h)| h == i) {
            emit_deinit_hook(i)
        } else {
            emit_function(rng, &plans, plan, garbage, &shape)
        };
        module = module.with_function(function);
        if plan.records {
            drawn.records += 1;
        }
        if plan.sentinel {
            drawn.sentinel += 1;
        }
        if plan.defaults {
            drawn.defaults += 1;
        }
        if plan.deinit {
            drawn.deinit += 1;
        }
        if !plan.seam {
            entries.push(ChildFn {
                name: name_of(plan.index, plans.len()),
                params: plan.slots[..plan.params].to_vec(),
            });
        }
    }
    (module, entries, drawn)
}

/// Coverage counters over one input's fiber/nursery phases, for the
/// mirror's sweep assertions.
#[derive(Default)]
pub struct Fibers {
    /// Host-spawned children (root nursery plus one detached spawn).
    pub spawned: u32,
    /// Host `resume_fiber` calls that returned a value.
    pub resumed: u32,
    /// Resumes whose returned value was a Ref word.
    pub resumed_ref: u32,
    /// Children whose drive ended `Completed` at a boundary.
    pub completed: u32,
    /// Completions delivering a Ref word (the completion crossing).
    pub completed_ref: u32,
    /// Children cut down under the cancel flag.
    pub cancelled: u32,
    /// Children that died trapped.
    pub trapped: u32,
    /// A join boundary ran; a teardown boundary ran; the run entry itself
    /// died cancelled; a boundary re-raised an uncontained payload.
    pub joined: u32,
    pub torn_down: u32,
    pub run_cancelled: u32,
    pub run_uncontained: u32,
    /// Draw counters (plan-level facts, not runtime depth): entry bodies
    /// carrying the records-list, sentinel evidence, trait-defaults, and
    /// deinit-claim templates.
    pub records: u32,
    pub sentinel: u32,
    pub defaults: u32,
    pub deinit: u32,
}

/// Stages one host argument word of the child's parameter type: Ref and
/// Raw parameters receive interned string words (the captured Ref/string
/// crossing at the spawn copy-in), a Fn parameter receives a bogus
/// callable word (typed-correct at the entry check, trapped only if the
/// child calls it).
fn stage_host_word(vm: &mut Vm, rng: &mut Stream, ty: SlotType) -> Value {
    match ty {
        SlotType::I64 => Value::I64(i64::try_from(rng.below(5)).expect("rng bounds are tiny")),
        SlotType::F64 => Value::F64(0.5),
        SlotType::Bool => Value::Bool(rng.below(2) == 0),
        SlotType::Tag => Value::Tag(u8::try_from(rng.below(5)).expect("rng bounds are tiny")),
        SlotType::Ref => vm.intern_str("fiber spawn word"),
        SlotType::Fn => Value::Fn(FuncRef::new(u32::MAX, 0)),
        // no value word inhabits the kind; the spawn's entry check refuses
        SlotType::I32 => Value::I64(0),
        SlotType::Raw => vm.intern_str("fiber raw word"),
    }
}

fn spawn_host_child(
    vm: &mut Vm,
    rng: &mut Stream,
    entries: &[ChildFn],
    nursery: Option<NurseryId>,
) -> Option<FiberId> {
    let child = &entries[rng.below(entries.len())];
    let args: Vec<Value> = child
        .params
        .iter()
        .map(|&ty| stage_host_word(vm, rng, ty))
        .collect();
    let spawned = match nursery {
        Some(id) => vm.spawn_fiber_in(id, MODULE_NAME, &child.name, &args),
        // a bare spawn after the root nursery died detaches (the host is
        // the supervisor: no child-of link)
        None => vm.spawn_fiber(MODULE_NAME, &child.name, &args),
    };
    match spawned {
        Ok(id) => Some(id),
        // a refused entry (arity, word kinds, frame versus the limit) is a
        // structured refusal
        Err(trap) => {
            std::hint::black_box(&trap);
            None
        },
    }
}

fn record_child_outcomes(outcomes: &[ChildOutcome], shape: &mut Fibers) {
    for outcome in outcomes {
        match outcome {
            ChildOutcome::Completed(span) => {
                shape.completed += 1;
                for word in span.as_slice() {
                    if matches!(word, Value::Ref(_)) {
                        shape.completed_ref += 1;
                    }
                }
            },
            ChildOutcome::Cancelled => shape.cancelled += 1,
            ChildOutcome::Trapped(_) => shape.trapped += 1,
        }
    }
}

/// Drives the host fiber face over the installed module: spawns children
/// with staged Ref/string words into the root nursery, host-resumes one
/// child a bounded number of times (the O(1) switch face, payloads
/// crossing in and completions out), then joins or tears the nursery down
/// (teardown raises the cancel flag: looping children stop at their next
/// taken backward branch), and finishes with a detached bare spawn.
fn drive_fibers(vm: &mut Vm, rng: &mut Stream, entries: &[ChildFn], shape: &mut Fibers) {
    let root = vm.open_root_nursery();
    let count = 1 + rng.below(3); // 1..=3 children
    let mut first: Option<FiberId> = None;
    for _ in 0..count {
        if let Some(id) = spawn_host_child(vm, rng, entries, Some(root)) {
            shape.spawned += 1;
            if first.is_none() {
                first = Some(id);
            }
        }
    }
    if let Some(id) = first {
        for _ in 0..=rng.below(3) {
            let payload = stage_host_word(vm, rng, SlotType::Raw);
            match vm.resume_fiber(id, payload) {
                Ok(answer) => {
                    shape.resumed += 1;
                    if matches!(answer.as_slice(), [Value::Ref(_)]) {
                        shape.resumed_ref += 1;
                    }
                },
                Err(trap) => {
                    std::hint::black_box(&trap);
                    break;
                },
            }
            if vm.fiber_state(id) == Some(FiberState::Dead) {
                break;
            }
        }
    }
    if rng.below(2) == 0 {
        if let Ok(outcome) = vm.join_nursery(root) {
            shape.joined += 1;
            match outcome {
                JoinOutcome::Joined(spans) => {
                    for span in &spans {
                        shape.completed += 1;
                        for word in span.as_slice() {
                            if matches!(word, Value::Ref(_)) {
                                shape.completed_ref += 1;
                            }
                        }
                    }
                },
                JoinOutcome::Failed(outcomes) => record_child_outcomes(&outcomes, shape),
            }
            assert_eq!(
                vm.nursery_state(root),
                Some(NurseryState::Dead),
                "a join consumes its nursery"
            );
        }
    } else if let Ok(outcomes) = vm.teardown_nursery(root) {
        shape.torn_down += 1;
        record_child_outcomes(&outcomes, shape);
        assert_eq!(
            vm.nursery_state(root),
            Some(NurseryState::Dead),
            "a teardown consumes its nursery"
        );
        assert_eq!(
            vm.nursery_cancelled(root),
            Some(true),
            "teardown raises the cancel flag"
        );
    }
    if let Some(id) = spawn_host_child(vm, rng, entries, None) {
        shape.spawned += 1;
        let payload = stage_host_word(vm, rng, SlotType::Raw);
        if vm.resume_fiber(id, payload).is_ok() {
            shape.resumed += 1;
        }
        if vm.fiber_state(id) != Some(FiberState::Dead) {
            let payload = stage_host_word(vm, rng, SlotType::Raw);
            let _ = vm.resume_fiber(id, payload);
        }
    }
}

/// Runs the full pipeline over one fuzz input and asserts the discipline:
/// canonical write-read round trip, natives installed before the loader,
/// a structured install outcome (pool FuncRefs validate at install; entry
/// descriptors fill), a structured run stop (a value, or a trap whose
/// backtrace materializes), the fiber/nursery host phase, and the stack
/// never passing the limit. Returns 1 for a halted value, 0 for a
/// structured trap, plus the fiber-phase coverage record.
pub fn exercise_fibers(data: &[u8]) -> (u32, Fibers) {
    let mut rng = Stream::new(data);
    let (module, entries, drawn) = build_planned(&mut rng);
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
    registry.install_platform(Platform::std());
    let mut vm = Vm::with_limit(registry, STACK_LIMIT);
    vm.capture_output();
    let installed = vm.install_dsmod(back).is_ok();
    if installed {
        let version = vm
            .registry()
            .current_version(MODULE_NAME)
            .expect("just installed");
        for (position, f) in version.functions().iter().enumerate() {
            assert_eq!(f.entry.resolve(), position, "install fills entry word 0");
        }
    }
    let mut shape = Fibers::default();
    shape.records += drawn.records;
    shape.sentinel += drawn.sentinel;
    shape.defaults += drawn.defaults;
    shape.deinit += drawn.deinit;
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
            match &trap {
                Trap::NurseryCancelled { .. } => shape.run_cancelled += 1,
                Trap::NurseryUncontained { .. } => shape.run_uncontained += 1,
                _ => {},
            }
            std::hint::black_box(&trap);
            std::hint::black_box(vm.backtrace());
            0
        },
    };
    if installed {
        drive_fibers(&mut vm, &mut rng, &entries, &mut shape);
    }
    assert!(
        vm.max_stack_len() <= STACK_LIMIT,
        "the per-entry stack-limit discipline holds"
    );
    std::hint::black_box(vm.captured_output().len());
    (outcome, shape)
}

/// The pipeline over one fuzz input, outcome only: the cargo-fuzz
/// target's and the base mirror's entry; the fiber mirror drives
/// `exercise_fibers` directly, so this wrapper is dead code there.
#[allow(dead_code)]
pub fn exercise(data: &[u8]) -> u32 {
    exercise_fibers(data).0
}
