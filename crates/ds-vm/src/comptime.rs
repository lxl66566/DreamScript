//! The comptime reflection native module (G1, G2): the two builtins the
//! comptime evaluator's fenced VM serves — `fields`, answering one field
//! record per declared field of a reflected type, and `type_name`,
//! answering a reflected type's name. The module is installed under the
//! reserved `comptime` name only inside the evaluator's VMs; a runtime
//! module never carries a reference to it (the checker refuses the
//! builtins outside comptime functions), so the reflection table lives
//! exactly as long as one evaluation.
//!
//! The table itself is a thread-local slot because the native calling
//! convention is a plain fn pointer with no capture: the evaluator
//! installs its table before driving the VM and the guard it gets back
//! restores the previous one when the evaluation ends. Thread scoping is
//! what lets concurrently evaluating threads keep their own tables — a
//! fenced run stays entirely on its calling thread, so no worker ever
//! reads the slot. The records are the evaluator's projection of the
//! checker's declaration tables (names and field shapes as plain data),
//! so no checker type crosses this seam.

use std::cell::RefCell;

use ds_abi::SlotType;

use crate::{
    Trap,
    native::{NativeFn, NativeFunction, NativeResult, OutputSink},
    object::Heaps,
    value::Value,
};

/// The reserved native module name the comptime builtins' pool FuncRefs
/// address; `$`-free but outside the identifier charset's keywords, and
/// a user package cannot collide with it because the checker wires the
/// builtins by name alone (H5.5) and refuses their calls outside
/// comptime functions.
pub const COMPTIME_MODULE: &str = "comptime";

/// One reflected field record, the 2026-11-14 derive-extension ruling's
/// (name, type, tag) triple over declared data. Over a struct's fields:
/// the field's name, the reflection index of its type, and the field's
/// nominal tag name (the type's own name when the field type is a bare
/// atom, empty for composite field types). Over an enum's cases (the
/// same record shape, one per case): the case's name, the reflection
/// index of its payload (the case's own minted atom, or `none` when the
/// case constructs bare), and the constructor tag its values carry at
/// runtime (the case's own name).
#[derive(Clone, Debug)]
pub struct ComptimeField {
    /// The field's (or case's) declared name.
    pub name: String,
    /// The reflection index of the field's type (or the case's payload).
    pub type_index: u8,
    /// The field's nominal tag name (empty when the field type is
    /// composite), or the case's own constructor tag name.
    pub tag: String,
}

/// One reflected type: its name and its declared fields — one record
/// per field over a struct, one record per case over a non-generic
/// enum. Empty over tags, generic declarations (atoms kept opaque until
/// substitution lands), and composite projections.
#[derive(Clone, Debug)]
pub struct ComptimeType {
    /// The type's declared name, or its rendered spelling for a
    /// composite projection.
    pub name: String,
    /// The declared fields (or cases), in declaration order.
    pub fields: Vec<ComptimeField>,
}

// The installed reflection table: the natives' whole view of the
// package's declared data, scoped to the evaluating thread.
thread_local! {
    static REFLECTION: RefCell<Vec<ComptimeType>> = const { RefCell::new(Vec::new()) };
}

/// Installs `table` as the reflection the comptime natives answer from,
/// answering the guard that restores the previous table when dropped.
/// The evaluator holds the guard across one fenced run.
#[must_use]
pub fn set_reflection(table: Vec<ComptimeType>) -> ReflectionGuard {
    let restored = REFLECTION.with(|current| std::mem::replace(&mut *current.borrow_mut(), table));
    ReflectionGuard {
        table: Some(restored),
    }
}

/// The restore handle of one [`set_reflection`]: dropping it puts its
/// table back, so evaluations nest and never leak a stale view.
pub struct ReflectionGuard {
    table: Option<Vec<ComptimeType>>,
}

impl Drop for ReflectionGuard {
    fn drop(&mut self) {
        let restored = self.table.take().unwrap_or_default();
        REFLECTION.with(|current| *current.borrow_mut() = restored);
    }
}

/// Runs `go` against the reflection table, one native's whole read.
fn with_reflection<T>(go: impl FnOnce(&[ComptimeType]) -> T) -> T {
    REFLECTION.with(|current| go(&current.borrow()))
}

/// `fields(t: type)`: one three-word record per declared field — name
/// `Ref`, type `Tag`, tag `Ref` — flattened into one list object at
/// stride three, matching the lowering's field-record layout exactly.
fn fields_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let Value::Tag(index) = args.first().copied().unwrap_or(Value::I64(0)) else {
        return Err(Trap::SlotTypeMismatch {
            slot: 0,
            expected: SlotType::Tag,
            actual: args.first().map_or(SlotType::I64, Value::slot_type),
        });
    };
    let fields = with_reflection(|table| {
        table
            .get(usize::from(index))
            .map(|entry| entry.fields.clone())
    });
    let Some(fields) = fields else {
        return Err(Trap::UnknownInstance {
            evidence: i64::from(index),
        });
    };
    let mut elems = Vec::with_capacity(fields.len() * 3);
    for field in &fields {
        elems.push(heaps.new_str_part(&field.name));
        elems.push(Value::Tag(field.type_index));
        elems.push(heaps.new_str_part(&field.tag));
    }
    // each fresh name/tag word's birth count becomes the list's edge, so
    // the frame claim its alloc entered leaves with the adoption
    for elem in &elems {
        if let Value::Ref(word) = *elem {
            heaps.release_claim(word);
        }
    }
    let stride = if elems.is_empty() { 0 } else { 3 };
    Ok(NativeResult::Word(heaps.new_list_strided(elems, stride)))
}

/// `type_name(t: type)`: the reflected type's name.
fn type_name_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let Value::Tag(index) = args.first().copied().unwrap_or(Value::I64(0)) else {
        return Err(Trap::SlotTypeMismatch {
            slot: 0,
            expected: SlotType::Tag,
            actual: args.first().map_or(SlotType::I64, Value::slot_type),
        });
    };
    let name = with_reflection(|table| {
        table
            .get(usize::from(index))
            .map(|entry| entry.name.clone())
            .unwrap_or_default()
    });
    Ok(NativeResult::Word(heaps.new_str_part(&name)))
}

/// The comptime native module's table: index 0 `fields`, index 1
/// `type_name` — the indices the lowerer's native table pins.
pub fn comptime_module() -> Vec<NativeFunction> {
    let native = |name: &'static str, func: NativeFn| NativeFunction {
        name,
        params: &[SlotType::Tag],
        variadic: false,
        result_words: 1,
        control: None,
        channel: None,
        timer: None,
        func,
    };
    vec![
        native("fields", fields_fn),
        native("type_name", type_name_fn),
    ]
}

/// The builtin core's table as the comptime fence serves it: the
/// runtime table with the world-touching builtin rows dropped, so the
/// slice's own load refuses a call to one exactly the platform
/// natives' unserved ops — the fence's world refusal, as a compile
/// error. The timer race's `timeout` is the fenced row: it parks the
/// calling fiber against the clock, a world touch no fenced run may
/// make. The dropped rows sit at the table's tail, so the remaining
/// indices keep their pinned ids.
#[must_use]
pub fn fenced_builtin_module() -> Vec<NativeFunction> {
    let mut table = crate::native::builtin_module();
    table.truncate(crate::native::BUILTIN_TIMEOUT as usize);
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_guard_restores_the_previous_table() {
        let first = vec![ComptimeType {
            name: "one".to_owned(),
            fields: Vec::new(),
        }];
        let _guard = set_reflection(first);
        with_reflection(|table| {
            assert_eq!(table.len(), 1);
            assert_eq!(table[0].name, "one");
        });
        let second = vec![
            ComptimeType {
                name: "a".to_owned(),
                fields: vec![ComptimeField {
                    name: "x".to_owned(),
                    type_index: 1,
                    tag: "int".to_owned(),
                }],
            },
            ComptimeType {
                name: "int".to_owned(),
                fields: Vec::new(),
            },
        ];
        let guard = set_reflection(second);
        with_reflection(|table| {
            assert_eq!(table.len(), 2);
            assert_eq!(table[0].fields[0].name, "x");
        });
        drop(guard);
        with_reflection(|table| {
            assert_eq!(table.len(), 1);
            assert_eq!(table[0].name, "one");
        });
    }
}
