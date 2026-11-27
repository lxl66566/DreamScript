//! # The generic container closes (indices 32-37)
//!
//! The six generic container natives the ambient declares past the map
//! block, CALLG-addressed like the container reads above them: `cons`,
//! `tail`, and `collect` are producers — fresh lists over the source's
//! element words, construction copying, the copy
//! acquiring one count per `Ref` element it adopts; `first` is the
//! one checked first-element body (the retired `head` spelling's
//! historical slot at 33 stays reserved), answering
//! `list_get`'s two-word span (the hit is [`PLATFORM_IO_OK`] with the
//! element handed out under its own count, an empty list the
//! [`PLATFORM_NONE`] member with the inert unit word as payload —
//! there is no rejected index to carry). The ord-routed `max`/`min`
//! pair selects one of two same-width operand spans through the staged
//! compare callable. Their table slots stay reserved trap entries like
//! every generic builtin's.
//!
//! # The reentrant combinators (indices 38-40, 46)
//!
//! The lambda-taking container combinators — `map`, `filter`,
//! `reduce`, `partition` — are the first generic bodies that call user
//! code: the staged lambda parameter is a `Fn` word (the callable-word
//! classification) naming a planned function, and the body runs it
//! per element through the VM's reentrancy seam (a synthetic reentry
//! frame driven to its return; see `vm::Vm::call_hosted`). `map`
//! collects each answered word into a fresh list, `filter` keeps the
//! elements whose lambda answers true (a non-bool answer refuses like
//! a mismatched slot), and `reduce` left-folds with the borrowed init's
//! whole word span as the seed — an empty list answers the init under
//! its own count per `Ref` word.
//! `partition` runs `filter`'s predicate with two destinations — a
//! true answer pushes onto the first fresh list, a false one onto the
//! second — and the two list `Ref`s answer as the tag-less multi-word
//! total: no discriminant, the two words are the tuple itself. A
//! drive that ends the fiber or parks at the host instead of
//! returning refuses honestly: a Rust-side loop cannot suspend.

use std::sync::LazyLock;

use ds_abi::Staging;

use super::{
    fork::{par_map_generic_fn, par_reduce_generic_fn},
    list::{
        collect_fn, cons_fn, first_element_fn, list_concat_fn, list_get_fn, list_len_fn, tail_fn,
    },
    map::{map_get_fn, map_keys_fn, map_len_fn},
    scalar::abs_fn,
    sink::{i64_arg, ref_arg},
    *,
};
use crate::vm::Vm;

/// One generic builtin body: the ONE body a CALLG site
/// dispatches, addressed by the pool FuncRef (module, native index) the
/// same way a CALL addresses a native. `params` are the real argument
/// words (checked against the staged words like any native's), and
/// `evidence` is the count of evidence words the dispatch reads from the
/// overlay after them — the constraint list travels with the callee, so
/// the uop carries no count of its own.
pub struct GenericFunction {
    /// The call name, for diagnostics.
    pub name: &'static str,
    /// The real arguments' expected word kinds — the named words of the
    /// entry's [`Staging`] discipline. A `Leading` entry's `params` cover
    /// the trailing named words (the staged words before them are one
    /// element's whole inline span, `cons`'s head); an `Interior` entry's
    /// cover the leading named word plus the final named word (a fold's
    /// accumulator span sits between them, so the final word's position
    /// is dynamic per call).
    pub params: &'static [SlotType],
    /// How the staged words map onto `params`: fixed one-to-one, one
    /// element span leading the named words, or one span between the
    /// leading and trailing named words — the arity check relaxes to
    /// "at least" for the two span forms and the kind check covers the
    /// named words alone.
    pub staging: Staging,
    /// How many evidence words follow the staged arguments.
    pub evidence: u8,
    /// Result span width on the caller's overlay.
    pub result_words: u8,
    /// The one body: `(evidence, args)`.
    pub func: GenericFn,
}

/// The body shape of a generic builtin: the whole VM, the evidence
/// words, then the real arguments. The whole-VM signature is the
/// reentrancy seam — the lambda-taking combinators call user code back
/// through `[Vm::call_hosted]` mid-body — while the pure bodies split
/// the heap and sink off themselves (`[Vm::split]`) and never touch the
/// rest.
pub type GenericFn = fn(&mut Vm, &[Value], &[Value]) -> Result<NativeResult, Trap>;

/// One body per generic row name: a name and its own body cannot drift apart, since two rows
/// can share every declarative field.
pub(super) const GENERIC_BODIES: &[(&str, GenericFn)] = &[
    ("len", len_generic_fn),
    ("list_len", list_len_generic_fn),
    ("list_get", list_get_generic_fn),
    ("list_concat", list_concat_generic_fn),
    ("map_len", map_len_generic_fn),
    ("map_get", map_get_generic_fn),
    ("map_keys", map_keys_generic_fn),
    ("cons", cons_generic_fn),
    ("tail", tail_generic_fn),
    ("collect", collect_generic_fn),
    ("first", first_generic_fn),
    ("max", max_generic_fn),
    ("min", min_generic_fn),
    ("abs", abs_generic_fn),
    ("map", map_generic_fn),
    ("filter", filter_generic_fn),
    ("reduce", reduce_generic_fn),
    ("fork_map", par_map_generic_fn),
    ("partition", partition_generic_fn),
    ("fork_reduce", par_reduce_generic_fn),
    ("sort", sort_generic_fn),
    ("sort_by", sort_generic_fn),
];

/// The generic builtin vocabulary, keyed by the (module, native index) `FuncRef` address: the abi
/// rows with this VM's one body per row attached.
pub(super) static GENERIC_FUNCTIONS: LazyLock<Vec<GenericFunction>> = LazyLock::new(|| {
    ds_abi::native::GENERIC_NATIVES
        .iter()
        .map(|row| GenericFunction {
            name: row.name,
            params: row.params,
            staging: row.staging,
            evidence: row.evidence,
            result_words: row.shape.result_words(),
            func: generic_body(row.name),
        })
        .collect()
});

/// The one body a generic row names.
pub(super) fn generic_body(name: &str) -> GenericFn {
    GENERIC_BODIES
        .iter()
        .find_map(|(row, func)| (*row == name).then_some(*func))
        .unwrap_or_else(|| panic!("the generic `{name}` row carries a body"))
}

/// Resolves the generic builtin a CALLG `FuncRef` addresses, if any.
#[must_use]
pub fn resolve_generic(module: &str, index: u32) -> Option<&'static GenericFunction> {
    if module != BUILTIN_MODULE {
        return None;
    }
    let at = ds_abi::native::GENERIC_NATIVES
        .iter()
        .position(|row| row.index == index)?;
    Some(&GENERIC_FUNCTIONS[at])
}

/// The one `len` body: dispatch on the call's evidence word, the
/// statically-resolved builtin instance of the instantiated type
/// ([`ds_abi::BuiltinInstance`]). The string instance counts unicode
/// scalar values; the list instance counts elements; the bytes
/// instance counts payload bytes; the map instance counts pairs.
pub(super) fn len_generic_fn(
    vm: &mut Vm,
    evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (mut heaps, _out) = vm.split();
    let instance = evidence.first().copied().unwrap_or(Value::I64(-1));
    match instance {
        Value::I64(id) if id == ds_abi::BuiltinInstance::Str as i64 => {
            // char count, never bytes: multi-byte text measures its scalar values
            let n = i64::try_from(heaps.char_count(ref_arg(args, 0)?)?)
                .expect("string length fits i64");
            Ok(NativeResult::Word(Value::I64(n)))
        },
        Value::I64(id) if id == ds_abi::BuiltinInstance::List as i64 => {
            let (elems, stride) = heaps.list_at(ref_arg(args, 0)?)?;
            let n = i64::try_from(elems.len() / stride.max(1)).expect("list length fits i64");
            Ok(NativeResult::Word(Value::I64(n)))
        },
        Value::I64(id) if id == ds_abi::BuiltinInstance::Bytes as i64 => {
            let n = i64::try_from(heaps.bytes_at(ref_arg(args, 0)?)?.len())
                .expect("bytes length fits i64");
            Ok(NativeResult::Word(Value::I64(n)))
        },
        Value::I64(id) if id == ds_abi::BuiltinInstance::Map as i64 => {
            let n = i64::try_from(heaps.map_len(ref_arg(args, 0)?)?).expect("map length fits i64");
            Ok(NativeResult::Word(Value::I64(n)))
        },
        Value::I64(id) if id == ds_abi::BuiltinInstance::Arena as i64 => {
            let n =
                i64::try_from(heaps.arena_len(ref_arg(args, 0)?)?).expect("arena length fits i64");
            Ok(NativeResult::Word(Value::I64(n)))
        },
        staged => Err(Trap::UnknownInstance {
            evidence: match staged {
                Value::I64(id) => id,
                _ => -1,
            },
        }),
    }
}

// The container reads and counts' one bodies:
// element-agnostic — the container word is one `Ref` whatever the
// element, so each adapter drops the evidence words (they record the
// receiver's container instance for the dispatch registry) and runs the
// same body a native's signature would.
pub(super) fn list_len_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (mut heaps, out) = vm.split();
    list_len_fn(&mut heaps, out, args)
}

pub(super) fn list_get_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (mut heaps, out) = vm.split();
    list_get_fn(&mut heaps, out, args)
}

pub(super) fn list_concat_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (mut heaps, out) = vm.split();
    list_concat_fn(&mut heaps, out, args)
}

pub(super) fn map_len_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (mut heaps, out) = vm.split();
    map_len_fn(&mut heaps, out, args)
}

pub(super) fn map_keys_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (mut heaps, out) = vm.split();
    map_keys_fn(&mut heaps, out, args)
}

pub(super) fn map_get_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (mut heaps, out) = vm.split();
    map_get_fn(&mut heaps, out, args)
}

pub(super) fn cons_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (mut heaps, out) = vm.split();
    cons_fn(&mut heaps, out, args)
}

pub(super) fn first_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (mut heaps, out) = vm.split();
    first_element_fn(&mut heaps, out, args)
}

pub(super) fn tail_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (mut heaps, out) = vm.split();
    tail_fn(&mut heaps, out, args)
}

pub(super) fn collect_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (mut heaps, out) = vm.split();
    collect_fn(&mut heaps, out, args)
}

pub(super) fn max_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    ord_pick_generic_fn(vm, args, true)
}

pub(super) fn min_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    ord_pick_generic_fn(vm, args, false)
}

/// Selects one of two same-width spans through the trailing comparator.
/// The answer carries one fresh count per selected reference word.
fn ord_pick_generic_fn(vm: &mut Vm, args: &[Value], max: bool) -> Result<NativeResult, Trap> {
    let Some((&cmp, operands)) = args.split_last() else {
        return Err(Trap::ArityMismatch {
            expected: 3,
            given: 0,
        });
    };
    if operands.len() < 2 || operands.len() % 2 != 0 {
        return Err(Trap::ArityMismatch {
            expected: 3,
            given: args.len(),
        });
    }
    let width = operands.len() / 2;
    let (first, second) = operands.split_at(width);
    let ordering = compare_order(vm, cmp, first, second)?;
    let select_first = if max { ordering >= 0 } else { ordering <= 0 };
    let selected = if select_first { first } else { second };
    let words = selected.to_vec();
    let (mut heaps, _) = vm.split();
    for value in &words {
        if let Value::Ref(word) = *value {
            heaps.inc(word)?;
        }
    }
    Ok(NativeResult::Total { words })
}

/// `abs`'s one body: dispatch on the call's evidence word like `max`'s —
/// the numeric instances the body answers. The value-kind match inside
/// the shared scalar body stays the defense: a corrupted stream staging
/// a non-numeric word refuses there instead of misreading it.
pub(super) fn abs_generic_fn(
    vm: &mut Vm,
    evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let instance = evidence.first().copied().unwrap_or(Value::I64(-1));
    match instance {
        Value::I64(id) if id == ds_abi::BuiltinInstance::Float as i64 => {
            let (mut heaps, out) = vm.split();
            abs_fn(&mut heaps, out, args)
        },
        Value::I64(id) if id == ds_abi::BuiltinInstance::Int as i64 => {
            let x = i64_arg(args, 0)?;
            // the magnitude has no in-range answer at the word's floor;
            // the arithmetic family's own overflow trap names it
            let magnitude = x.checked_abs().ok_or(Trap::IntegerOverflow)?;
            Ok(NativeResult::Word(Value::I64(magnitude)))
        },
        staged => Err(Trap::UnknownInstance {
            evidence: match staged {
                Value::I64(id) => id,
                _ => -1,
            },
        }),
    }
}

// The lambda-taking container combinators: the one bodies reenter the
// dispatcher per element through `Vm::call_hosted` — the staged lambda
// word is callable-defaulted (a scalar FuncRef or a closure object's
// Ref word), and its answer word arrives under the
// RET transfer count, which the body owns. The evidence words record
// the receiver's container instance like every element-agnostic
// body's; a mid-loop trap leaks the partial result's birth count, the
// same single-leak class the producer bodies' traps leave.

/// The staged lambda word: callable-defaulted — the scalar FuncRef
/// of a non-capturing lambda or named function, or a closure object's
/// `Ref` word. The hosted call resolves either kind; every other word
/// kind refuses like a mismatched slot.
pub(super) fn fn_arg(args: &[Value], index: usize) -> Result<Value, Trap> {
    match args.get(index) {
        Some(word @ (Value::Fn(_) | Value::Ref(_))) => Ok(*word),
        _ => Err(Trap::SlotTypeMismatch {
            slot: u8::try_from(index).expect("slot index fits u8"),
            expected: SlotType::Fn,
            actual: args.get(index).map_or(SlotType::I64, Value::slot_type),
        }),
    }
}

/// `map`'s one body: each element — its whole inline span, one slice at
/// the list's stride — runs through the staged lambda, and the answered
/// span — transfer counts and all — pushes onto the fresh result list
/// (the push acquires the list's own count per `Ref` word, fixing the
/// result's stride at the answer's width); the body then drops the
/// transferred counts it still holds.
pub(super) fn map_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let f = fn_arg(args, 1)?;
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
    for at in (0..elems.len()).step_by(stride) {
        let mapped = vm.call_hosted(f, &elems[at..at + stride])?;
        let (mut heaps, _) = vm.split();
        heaps.push_list(res, mapped.as_slice())?;
        for word in mapped.as_slice() {
            if let Value::Ref(word) = *word {
                heaps.dec(word)?;
            }
        }
    }
    Ok(NativeResult::Word(Value::Ref(res)))
}

/// A predicate's one-word bool answer — `filter`'s and `partition`'s
/// shared contract. A non-bool answer refuses like a mismatched slot
/// (the declared predicate's word is the contract); a wider answer
/// names hand-assembled code and refuses at its first word.
pub(super) fn predicate_bool(answer: &crate::vm::FrameAnswer) -> Result<bool, Trap> {
    let word = match answer.as_slice() {
        [only] => *only,
        wider => {
            return Err(Trap::SlotTypeMismatch {
                slot: 0,
                expected: SlotType::Bool,
                actual: wider[0].slot_type(),
            })
        },
    };
    match word {
        Value::Bool(keep) => Ok(keep),
        other => Err(Trap::SlotTypeMismatch {
            slot: 0,
            expected: SlotType::Bool,
            actual: other.slot_type(),
        }),
    }
}

/// `filter`'s one body: each element's whole span runs through the
/// predicate, whose one-word answer keeps or drops it. A non-bool
/// answer refuses like a mismatched slot — the declared predicate's
/// word is the contract. Keeping pushes the span onto the fresh result
/// list (the push acquires the list's own count per `Ref` word; the
/// source list keeps its own), so no element-side count moves at all.
pub(super) fn filter_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let f = fn_arg(args, 1)?;
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
    for at in (0..elems.len()).step_by(stride) {
        if predicate_bool(&vm.call_hosted(f, &elems[at..at + stride])?)? {
            let (mut heaps, _) = vm.split();
            heaps.push_list(res, &elems[at..at + stride])?;
        }
    }
    Ok(NativeResult::Word(Value::Ref(res)))
}

/// `partition`'s one body: each element's whole span runs through the
/// predicate exactly as `filter`'s does, a true answer pushing onto the
/// first fresh list and a false one onto the second (each push taking
/// the destination list's own edge count per `Ref` word, so no
/// element-side count moves), and the two list `Ref`s answer as the
/// tag-less multi-word total — no discriminant, the two words are the
/// tuple itself, staged on the caller's overlay words 0 and 1.
pub(super) fn partition_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let f = fn_arg(args, 1)?;
    let list = ref_arg(args, 0)?;
    let (elems, stride) = {
        let (mut heaps, _) = vm.split();
        let (elems, stride) = heaps.list_at(list)?;
        (elems.to_vec(), stride.max(1))
    };
    let yes = {
        let (mut heaps, _) = vm.split();
        heaps.new_list_id(Vec::new())
    };
    let no = {
        let (mut heaps, _) = vm.split();
        heaps.new_list_id(Vec::new())
    };
    for at in (0..elems.len()).step_by(stride) {
        let dst =
            if predicate_bool(&vm.call_hosted(f, &elems[at..at + stride])?)? { yes } else { no };
        let (mut heaps, _) = vm.split();
        heaps.push_list(dst, &elems[at..at + stride])?;
    }
    Ok(NativeResult::Total {
        words: vec![Value::Ref(yes), Value::Ref(no)],
    })
}

/// `reduce`'s one body: the accumulator starts as the borrowed init's
/// whole word span (native arguments are a transient borrow window; the
/// span stages between the list and the callable, its width the call
/// site's fact), each step hands the accumulator words plus the
/// element's whole span to the staged lambda under the hosted call's
/// one-count-per-Ref hand-off (the lambda's exit drops or returns
/// them), and the answer's whole span replaces the owned one — the old
/// words' counts drop per `Ref` word, and the first step's old span is
/// the borrowed init, never dropped. An empty list answers the init
/// handed out under its own count per `Ref` word (the `list_get`
/// discipline); otherwise the final owned counts transfer to the
/// caller with the words through the tag-less total. The callable's
/// own signature governs the answer's width (the checker proves it the
/// folded type), so no body-side width check exists.
pub(super) fn reduce_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let f = fn_arg(args, args.len() - 1)?;
    let list = ref_arg(args, 0)?;
    let (elems, stride) = {
        let (mut heaps, _) = vm.split();
        let (elems, stride) = heaps.list_at(list)?;
        (elems.to_vec(), stride.max(1))
    };
    let mut acc: Vec<Value> = args[1..args.len() - 1].to_vec();
    let mut owned = false;
    let mut call_args = Vec::with_capacity(acc.len() + stride);
    for at in (0..elems.len()).step_by(stride) {
        call_args.clear();
        call_args.extend_from_slice(&acc);
        call_args.extend_from_slice(&elems[at..at + stride]);
        let answer = vm.call_hosted(f, &call_args)?;
        if owned {
            for word in &acc {
                if let Value::Ref(id) = *word {
                    let (mut heaps, _) = vm.split();
                    heaps.dec(id)?;
                }
            }
        }
        acc.clear();
        acc.extend_from_slice(answer.as_slice());
        owned = true;
    }
    if !owned {
        for word in &acc {
            if let Value::Ref(id) = *word {
                let (mut heaps, _) = vm.split();
                heaps.inc(id)?;
            }
        }
    }
    Ok(NativeResult::Total { words: acc })
}

/// One comparison's ordering answer: the callable's one-word `int`
/// result names the first span the earlier element when negative, and
/// an equal answer keeps the left run's element (stability). A wider
/// or non-int answer refuses like a mismatched slot — the declared
/// comparison's word is the contract, the predicate-bool discipline.
pub(super) fn compare_orders_first(
    vm: &mut Vm,
    cmp: Value,
    first: &[Value],
    second: &[Value],
) -> Result<bool, Trap> {
    Ok(compare_order(vm, cmp, first, second)? <= 0)
}

/// Runs one staged comparator and returns its signed ordering word.
fn compare_order(vm: &mut Vm, cmp: Value, first: &[Value], second: &[Value]) -> Result<i64, Trap> {
    let mut args = Vec::with_capacity(first.len() + second.len());
    args.extend_from_slice(first);
    args.extend_from_slice(second);
    let answer = vm.call_hosted(cmp, &args)?;
    match answer.as_slice() {
        [Value::I64(ordering)] => Ok(*ordering),
        wider => Err(Trap::SlotTypeMismatch {
            slot: 0,
            expected: SlotType::I64,
            actual: wider.first().map_or(SlotType::I64, Value::slot_type),
        }),
    }
}

/// `sort`'s one body: the stable merge sort over the staged compare
/// callable. The walk is bottom-up over element indices, so no element
/// span moves before the final pushes and no Rust recursion rides the
/// depth. Each kept element's whole span pushes onto the fresh result
/// list (the push acquiring the list's own count per `Ref` word, the
/// source list keeping its own — filter's discipline), so no
/// element-side count moves at all; a mid-sort trap leaks the fresh
/// empty list alone, the same single-leak class the other combinators'
/// traps leave.
pub(super) fn sort_generic_fn(
    vm: &mut Vm,
    _evidence: &[Value],
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let cmp = fn_arg(args, 1)?;
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
    let count = elems.len() / stride;
    let mut order: Vec<usize> = (0..count).collect();
    let mut scratch: Vec<usize> = vec![0; count];
    let mut width = 1;
    while width < count {
        let mut at = 0;
        while at < count {
            let mid = (at + width).min(count);
            let end = (at + 2 * width).min(count);
            let (mut left, mut right, mut out) = (at, mid, at);
            while left < mid && right < end {
                let first = order[left] * stride;
                let second = order[right] * stride;
                if compare_orders_first(
                    vm,
                    cmp,
                    &elems[first..first + stride],
                    &elems[second..second + stride],
                )? {
                    scratch[out] = order[left];
                    left += 1;
                } else {
                    scratch[out] = order[right];
                    right += 1;
                }
                out += 1;
            }
            while left < mid {
                scratch[out] = order[left];
                left += 1;
                out += 1;
            }
            while right < end {
                scratch[out] = order[right];
                right += 1;
                out += 1;
            }
            at = end;
        }
        std::mem::swap(&mut order, &mut scratch);
        width *= 2;
    }
    for &element in &order {
        let start = element * stride;
        let (mut heaps, _) = vm.split();
        heaps.push_list(res, &elems[start..start + stride])?;
    }
    Ok(NativeResult::Word(Value::Ref(res)))
}
