//! The anonymous-tuple surface end to end (B3): construction, destructuring
//! in bindings and match arms, tuple params, returns, and tail calls — every
//! tuple is consecutive frame words (one plain word per element: strings and
//! lists ride their Ref, `()` rides the I64 unit word), and destructuring is
//! per-slot reads binding each element word in place. A destructuring `var`
//! binds each element its own re-storable slot (a fresh word the initializer
//! copies into, so an assignment through an element re-stores its slot alone
//! while the const convention aliases the tuple's words). The multi-word
//! corners reject at the lowering boundary with their own notes: a union-typed
//! element distributed the product into several tuple atoms, a struct or
//! nested-tuple element spans several words, and a tuple member of a union is
//! a multi-word payload.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Value, Vm, loader, native};

/// Builds the probe module once per test VM: parse -> check -> lower ->
/// write -> read back -> install, with both native tables in place before
/// the loader's install (pool FuncRefs validate against installed tables).
fn build_vm() -> Vm {
    let source = include_str!("fixtures/tuples_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("tuples_probe.ds", source);
    let asts = [parse(&files, id).expect("the fixture parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the fixture checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "tuples_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_native(native::STD_PLATFORM_MODULE, native::std_platform_module());
    loader::install(&mut registry, &module).expect("the module installs");
    Vm::new(registry)
}

#[test]
fn tuples_construct_destructure_and_dispatch_on_the_frame() {
    // every leg of the fixture: the annotated and inferred constructions,
    // let and match-arm destructuring, the 1-tuple, the unit element, the
    // list element, structural ==, the tail call returning a pair, and
    // the per-element var slots (assignment through an element re-stores
    // its slot while the const aliases still read the source)
    let mut vm = build_vm();
    vm.capture_output();
    assert_eq!(vm.run("tuples_probe", "main", &[]), Ok(Value::Bool(true)));
    assert_eq!(
        vm.captured_output(),
        b"host:8080\nsum 7\narm go\ntail z2\none 5\nunit 1\nlist 23\neq\nvar true\n" as &[u8]
    );
}

/// Lowers one inline source through the full pipeline (parse -> analyze
/// -> lower), asserting the check is clean so failures are the lowerer's.
fn lower_inline(source: &str) -> Result<(), LowerError> {
    // in-process checking has no ambient: the primitives ride along
    const PRELUDE: &str = "tag int\ntag string\ntag bool\ntrait Copyable { }\n";
    let combined = format!("{PRELUDE}{source}");
    let mut files = Files::new();
    let id = files.add_source("inline.ds", &combined);
    let asts = [parse(&files, id).expect("the source parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the source checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    lower(
        "inline",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .map(|_| ())
}

/// Every multi-word corner names its own seam at the boundary.
#[test]
fn multi_word_tuple_corners_reject_at_the_lowering_boundary() {
    let cases = [
        (
            // a union-typed element distributed the product [B3]
            "fn pick() -> int | none { 1 }\nfn main() -> int { const t = (\"x\", pick())\n 0 }",
            "the product distributed into several tuple atoms",
        ),
        (
            // a struct element spans several words
            "struct pt { x: int }\nfn main() -> int { const t = (pt { x: 1 }, 2)\n 0 }",
            "tuple elements lower as one plain frame word",
        ),
        (
            // a nested-tuple element spans several words
            "fn main() -> int { const t = ((1, 2), 3)\n 0 }",
            "tuple elements lower as one plain frame word",
        ),
        (
            // a tuple member of a union is a multi-word payload
            "fn pick() -> (int, int) | none { (1, 2) }\nfn main() -> int { match pick() { (a, b) \
             => a, none => 0 } }",
            "a tuple member of a union",
        ),
        (
            // a var destructuring element binds: literals test at match
            // time and a declaration cannot fail a test
            "fn main() -> int { var (a, 0) = (1, 0)\n a }",
            "a var destructuring element pattern",
        ),
        (
            // the const convention is the same: a declaration never tests
            "fn main() -> int { const (a, 0) = (1, 0)\n a }",
            "a destructuring element pattern",
        ),
    ];
    for (source, needle) in cases {
        let Err(error) = lower_inline(source) else {
            panic!("the multi-word corner does not reject: {source}");
        };
        assert!(
            error.to_string().contains(needle),
            "the rejection names its seam: {error}"
        );
    }
}
