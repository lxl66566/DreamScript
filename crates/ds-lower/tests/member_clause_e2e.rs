//! A member fn's own generic clause end to end: the clause solves at
//! the call site from the arguments behind the receiver, the solved
//! types riding the call's checker row, and the entering site plans the
//! per-shape instance over the head's arguments plus the clause's —
//! equal solved shapes sharing one body, a same-name clause parameter
//! and impl head parameter keeping their two segments apart, and a
//! site inside another generic body's instance resolving its clause
//! through the walking entry's own substitution. The doors that stay
//! closed live in `generic_impl_refusals.rs`: a clause no argument
//! solves keeps the head-only instance, and a clause the checker
//! cannot infer refuses at the call.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// Parse, check, and lower one source; panics with the rendered
/// diagnostics when an earlier stage refuses.
fn lower_clean(name: &str, source: &str) -> Lowered {
    let mut files = Files::new();
    let id = files.add_source(format!("{name}.ds"), source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the surface checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed).expect("the probe lowers")
}

/// One lowered module installed into a fresh VM, output captured.
fn run_of(lowered: &Lowered) -> Vm {
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    vm
}

/// The table names of one generic member's instances.
fn instances_of<'l>(lowered: &'l Lowered, prefix: &str) -> Vec<&'l str> {
    lowered
        .functions()
        .iter()
        .map(String::as_str)
        .filter(|name| *name == prefix || name.starts_with(&format!("{prefix}@")))
        .collect()
}

/// The member clause over the whole instance surface: the inherent
/// member and the trait impl's member both instantiate over the head's
/// arguments plus the clause's, equal solved shapes sharing one body
/// across their entering sites — the nested site inside another generic
/// body's instance included, its clause record resolved through the
/// walking entry's own substitution — and the heap drains.
#[test]
fn a_members_own_generic_clause_lowers_per_solved_shape() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "struct list[t]\n",
        "extern fn println (s: string) -> ()\n",
        "trait foldy[t] {\n",
        "  fn foldy[r] (self, init: r, f: fn(r, t) -> r) -> r\n",
        "}\n",
        "impl[t] foldy[t] for list[t] {\n",
        "  fn foldy[r] (self, init: r, f: fn(r, t) -> r) -> r {\n",
        "    var acc = init\n",
        "    for x in self {\n",
        "      acc = f(acc, x)\n",
        "    }\n",
        "    acc\n",
        "  }\n",
        "}\n",
        "impl[t] list[t] {\n",
        "  fn fold[r] (self, init: r, f: fn(r, t) -> r) -> r {\n",
        "    var acc = init\n",
        "    for x in self {\n",
        "      acc = f(acc, x)\n",
        "    }\n",
        "    acc\n",
        "  }\n",
        "}\n",
        "fn pick[r] (xs: list[int], init: r, g: fn(r, int) -> r) -> r {\n",
        "  xs.fold(init, g)\n",
        "}\n",
        "fn main() -> int {\n",
        "  println(\"${[1, 2, 3].fold(0, (acc: int, x: int) => acc + x)}\")\n",
        "  println(\"${[1, 2].fold(0, (acc: int, x: int) => acc + x)}\")\n",
        "  println([\"a\", \"b\"].fold(\"\", (acc: string, x: string) => acc + x))\n",
        "  println(\"${pick([1], \"q\", (acc: string, x: int) => acc)}\")\n",
        "  println(\"${pick([1], 7, (acc: int, x: int) => acc)}\")\n",
        "  println(\"${[1, 2].foldy(0, (acc: int, x: int) => acc + x)}\")\n",
        "  0\n",
        "}\n",
    );
    let lowered = lower_clean("member_clause", source);
    // the head's int plus the clause's int, the head's int plus the
    // clause's string, the head's string plus the clause's string:
    // three bodies, the two same-shape sites and the nested int site
    // sharing the first
    let folds = instances_of(&lowered, "list[t]#fold");
    assert_eq!(
        folds.len(),
        3,
        "one fold body per solved head-plus-clause shape (table: {:?})",
        lowered.functions()
    );
    let trait_side = instances_of(&lowered, "foldy#list[t]#foldy");
    assert_eq!(
        trait_side.len(),
        1,
        "the trait impl's member instantiates over its own site's shape (table: {:?})",
        lowered.functions()
    );
    let mut vm = run_of(&lowered);
    assert_eq!(
        vm.run("member_clause", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        concat!("6\n", "3\n", "ab\n", "q\n", "7\n", "3\n").as_bytes(),
        "every site answers through its own body's fold"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// A clause parameter sharing the impl head parameter's name keeps the
/// two segments apart: the clause's minting shadows the head's inside
/// the written signature, and the instance binds each atom to its own
/// segment — the same receiver over two clause shapes answers through
/// two bodies, neither conflating the clause's argument with the head's.
#[test]
fn a_clause_sharing_the_head_parameter_name_binds_its_own_segment() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "struct list[t]\n",
        "extern fn println (s: string) -> ()\n",
        "impl[t] list[t] {\n",
        "  fn foldish[t] (self, init: t, f: fn(t, t) -> t) -> t {\n",
        "    f(init, init)\n",
        "  }\n",
        "}\n",
        "fn main() -> int {\n",
        "  println([1, 2].foldish(\"z\", (a: string, b: string) => a + b))\n",
        "  println(\"${[1, 2].foldish(9, (a: int, b: int) => a + b)}\")\n",
        "  0\n",
        "}\n",
    );
    let lowered = lower_clean("same_name", source);
    let foldish = instances_of(&lowered, "list[t]#foldish");
    assert_eq!(
        foldish.len(),
        2,
        "the same receiver over two clause shapes takes two bodies (table: {:?})",
        lowered.functions()
    );
    let mut vm = run_of(&lowered);
    assert_eq!(
        vm.run("same_name", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        concat!("zz\n", "18\n").as_bytes(),
        "each clause shape answers through its own body"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}
