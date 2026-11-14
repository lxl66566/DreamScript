//! The `par_reduce` lowering seam end to end [E3]: the fold call site
//! lowers to CALLG — one site staging (list, init, callable) plus the
//! one List evidence word — and the module round-trips through the
//! writer and loader and folds on the VM. A capturing closure callable
//! folds through the same site (the capture block staged per spawn past
//! the accumulator and element), and the fold's width is the call's
//! own fact: the init's whole word span stages between the list and
//! the callable, so a struct-typed fold lowers and chains its whole
//! span through the spawn seam.

use ds_abi::Inst;
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::{FuncRefKey, Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

/// The record's body decoded to instructions, one per pc.
fn insts(function: &ds_mod::Function) -> Vec<Inst> {
    function
        .code()
        .iter()
        .enumerate()
        .map(|(pc, word)| {
            Inst::decode(u32::try_from(pc).expect("pc fits u32"), *word)
                .expect("ds-mod validated every word")
        })
        .collect()
}

/// Builds the VM once per test: parse -> check -> lower -> write -> read
/// back -> install. Both native tables must precede the loader's install
/// (pool FuncRefs validate against installed tables).
fn build_vm() -> Vm {
    let source = include_str!("fixtures/par_reduce_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("par_reduce_probe.ds", source);
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
        "par_reduce_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    Vm::new(registry)
}

#[test]
fn the_fold_site_lowers_round_trips_and_folds() {
    let mut vm = build_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("par_reduce_probe", "main", &[]),
        Ok(Value::I64(0)),
        "main returns unit"
    );
    assert_eq!(vm.captured_output(), b"fold 12345\n");
}

/// The site shape is pinned: exactly one CALLG staging (list, init,
/// callable) — the callable a scalar `Fn` word — with the List evidence
/// word riding the overlay past the staged span.
#[test]
fn the_callg_stages_the_list_init_callable_and_one_evidence_word() {
    let source = include_str!("fixtures/par_reduce_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("par_reduce_probe.ds", source);
    let asts = [parse(&files, id).expect("the fixture parses")];
    let analyzed = analyze(&files, &asts);
    let lowered = lower(
        "par_reduce_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers");
    let mut sites = 0;
    for f in lowered.module().functions() {
        let code = insts(f);
        for (i, uop) in code.iter().enumerate() {
            let Inst::Uop(ds_abi::Uop::Callg { callee, nargs }) = uop else {
                continue;
            };
            sites += 1;
            assert_eq!(*nargs, 3, "the fold stages list, init, callable");
            assert_eq!(
                f.constants()[usize::from(callee.0)],
                ds_mod::Constant::FuncRef {
                    key: FuncRefKey::Native("builtin".into()),
                    func: native::BUILTIN_PAR_REDUCE,
                },
                "the callee addresses the generic par_reduce slot"
            );
            // the one List evidence word rides the staged span's end
            let Some(Inst::Uop(ds_abi::Uop::Kimm { kidx, .. })) =
                i.checked_sub(1).and_then(|p| code.get(p))
            else {
                panic!("no evidence word staged before the Callg");
            };
            assert!(matches!(
                &f.constants()[usize::from(kidx.0)],
                ds_mod::Constant::I64(_)
            ));
            // the callable stages as the scalar Fn word: the span's last
            // argument slot moves a body slot an earlier Kconst loaded
            // from a FuncRef constant
            let Some(Inst::Uop(ds_abi::Uop::Mov {
                src: callable_slot, ..
            })) = i.checked_sub(2).and_then(|p| code.get(p))
            else {
                panic!("no callable word staged before the Callg");
            };
            let loaded = code.iter().take(i).find_map(|inst| match inst {
                Inst::Uop(ds_abi::Uop::Kconst { dst, kidx }) if *dst == *callable_slot => {
                    Some((kidx, *dst))
                },
                _ => None,
            });
            let Some((kidx, _)) = loaded else {
                panic!("the callable slot has no FuncRef load");
            };
            assert!(
                matches!(
                    &f.constants()[usize::from(kidx.0)],
                    ds_mod::Constant::FuncRef { .. }
                ),
                "the callable is the scalar Fn word"
            );
        }
    }
    assert_eq!(sites, 1, "exactly one par_reduce site");
}

#[test]
fn a_capturing_closure_callable_folds_end_to_end() {
    // the closure callable: the staged Ref word is the caller's borrow,
    // each spawned child entered with the running accumulator and the
    // element, the capture block staged past them — the fold applies
    // strictly in input order: 0 + 1 + 10 = 11, 11 + 2 + 10 = 23,
    // 23 + 3 + 10 = 36
    let source = r#"
tag int
tag string
struct list[t]

extern fn println (s: string) -> ()
extern fn par_reduce[t] (xs: list[t], init: t, f: fn(t, t) -> t) -> t

fn main() -> () {
  const xs = [1, 2, 3]
  const bias = 10
  const sum = xs.par_reduce(0, (a: int, b: int) => a + b + bias)
  println("sum ${sum}")
}
"#;
    let mut files = Files::new();
    let id = files.add_source("closure_fold.ds", source);
    let asts = [parse(&files, id).expect("the source parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the closure fold checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "closure_fold",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the closure fold lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run("closure_fold", "main", &[]),
        Ok(Value::I64(0)),
        "main returns unit"
    );
    assert_eq!(vm.captured_output(), b"sum 36\n");
}

#[test]
fn a_struct_typed_fold_lowers_round_trips_and_folds() {
    // the callable's answer is the pair span, the init staging its two
    // words between the list and the callable — the interior-span fold
    // end to end, each spawned child answering the next accumulator
    // whole: left 0 -> 1 -> 12 -> 123, right 1 -> 3 -> 6
    let source = r#"
tag int
tag string
struct pair { left: int, right: int }
struct list[t]

extern fn println (s: string) -> ()
extern fn par_reduce[t] (xs: list[t], init: t, f: fn(t, t) -> t) -> t

fn step(a: pair, b: pair) -> pair {
  pair { left: a.left * 10 + b.left, right: a.right + b.right }
}

fn main() -> () {
  const xs: list[pair] = [
    pair { left: 1, right: 1 },
    pair { left: 2, right: 2 },
    pair { left: 3, right: 3 },
  ]
  const folded = xs.par_reduce(pair { left: 0, right: 0 }, step)
  println("folded ${folded.left} ${folded.right}")
}
"#;
    let mut files = Files::new();
    let id = files.add_source("struct_fold.ds", source);
    let asts = [parse(&files, id).expect("the source parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the struct fold checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "struct_fold",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the struct fold lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run("struct_fold", "main", &[]),
        Ok(Value::I64(0)),
        "main returns unit"
    );
    assert_eq!(vm.captured_output(), b"folded 123 6\n");
}
