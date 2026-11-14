//! The exit-lowering probe: a mid-block `return e` whose operand union
//! is a strict subset of the declared return (a different, narrower
//! union type) checks clean by containment and must lower — the union
//! return path tag-dispatches per member and re-spans into the return
//! union's region instead of demanding a single atom (DS4009 before).
//! The run proves the dispatch: both members survive with their own
//! payloads, not a swapped or zeroed span.
//!
//! The optional-field probe: a returned union whose struct member
//! carries a `t?` field puts a Tag word inside the payload region, and
//! every non-struct member's construction leaves that word dead — the
//! zero-init must keep it kind-true, or the caller's typed move of the
//! returned span traps (the frame's inert I64 zero read as a Tag).

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

const SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "tag io_error\n",
    "tag parse_error\n",
    "struct config { host: string, port: int }\n",
    "extern fn println (msg: string) -> ()\n",
    "\n",
    "fn parse_step(s: string) -> config | parse_error {\n",
    "  if s == \"ok\" { config { host: \"h\", port: 1 } } else { parse_error }\n",
    "}\n",
    "\n",
    "// the operand union is one member short of the declared return\n",
    "fn wide(s: string) -> config | io_error | parse_error {\n",
    "  if s == \"ok\" {\n",
    "    return parse_step(s)\n",
    "  }\n",
    "  parse_step(s)\n",
    "}\n",
    "\n",
    "fn show(r: config | io_error | parse_error) -> () {\n",
    "  match r {\n",
    "    io_error => println(\"io\"),\n",
    "    parse_error => println(\"parse\"),\n",
    "    c => println(c.host),\n",
    "  }\n",
    "}\n",
    "\n",
    "fn main() -> () {\n",
    "  show(wide(\"ok\"))\n",
    "  show(wide(\"bad\"))\n",
    "}\n",
);

#[test]
fn a_strict_subset_union_return_dispatches_members_into_the_return_span() {
    let mut files = Files::new();
    let id = files.add_source("return_span.ds", SOURCE);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "containment already proves the return: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "return_span",
        Hash128::compute(SOURCE.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("a strict-subset union return lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run("return_span", "main", &[]),
        Ok(Value::I64(0)),
        "the unit entry result"
    );
    assert_eq!(
        String::from_utf8_lossy(vm.captured_output()),
        "h\nparse\n",
        "both members keep their own payloads through the re-span"
    );
}

const OPTIONAL_SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "extern fn println (msg: string) -> ()\n",
    "extern fn unwrap_or[t] (x: t?, y: t) -> t\n",
    "struct b2 { port: int? }\n",
    "tag my_err\n",
    "enum mixed { unit_err, full(int) }\n",
    "\n",
    "// the unit-tag member returns against a struct member whose\n",
    "// optional field claims a Tag word in the payload region\n",
    "fn miss() -> b2 | my_err { my_err }\n",
    "// the payloaded case's construction leaves the same Tag word dead\n",
    "fn hit() -> b2 | mixed { full(7) }\n",
    "// the ? relay re-spans the propagated error member into the union\n",
    "fn step() -> int | my_err { my_err }\n",
    "fn relay() -> b2 | my_err {\n",
    "  const p = step()?\n",
    "  b2 { port: p }\n",
    "}\n",
    "fn step_ok() -> int | my_err { 4 }\n",
    "fn relay_ok() -> b2 | my_err {\n",
    "  const p = step_ok()?\n",
    "  b2 { port: p }\n",
    "}\n",
    "// the struct member itself still fills its live optional words\n",
    "fn made() -> b2 | my_err { b2 { port: 9 } }\n",
    "\n",
    "fn main() -> () {\n",
    "  match miss() {\n",
    "    my_err => println(\"miss-err\"),\n",
    "    c => println(\"miss-port ${c.port.unwrap_or(-1)}\"),\n",
    "  }\n",
    "  match hit() {\n",
    "    full(n) => println(\"hit-full ${n}\"),\n",
    "    unit_err => println(\"hit-unit\"),\n",
    "    c => println(\"hit-port ${c.port.unwrap_or(-2)}\"),\n",
    "  }\n",
    "  match relay() {\n",
    "    my_err => println(\"relay-err\"),\n",
    "    c => println(\"relay-port ${c.port.unwrap_or(-3)}\"),\n",
    "  }\n",
    "  match relay_ok() {\n",
    "    my_err => println(\"rok-err\"),\n",
    "    c => println(\"rok-port ${c.port.unwrap_or(-4)}\"),\n",
    "  }\n",
    "  match made() {\n",
    "    my_err => println(\"made-err\"),\n",
    "    c => println(\"made-port ${c.port.unwrap_or(-5)}\"),\n",
    "  }\n",
    "}\n",
);

#[test]
fn a_unit_tag_return_over_an_optional_carrying_struct_member_stays_kind_true() {
    let mut files = Files::new();
    let id = files.add_source("optional_return_span.ds", OPTIONAL_SOURCE);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "{}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "optional_return_span",
        Hash128::compute(OPTIONAL_SOURCE.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the optional-field probe lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run("optional_return_span", "main", &[]),
        Ok(Value::I64(0)),
        "the unit entry result"
    );
    assert_eq!(
        String::from_utf8_lossy(vm.captured_output()),
        "miss-err\nhit-full 7\nrelay-err\nrok-port 4\nmade-port 9\n",
        "every member answers its own arm: the dead Tag word never traps the caller's typed move, \
         and the struct member's live optional extracts"
    );
}
