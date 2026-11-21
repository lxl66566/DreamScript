//! The narrowing-copy probe: a place narrowed to a single-member enum
//! whose sole case carries a multi-word tuple payload copies out of the
//! wider union span positionally — the payload's unnamed fields have no
//! names to match by, so the word gather matches them by position
//! against the member's claimed region offsets. A call argument and a
//! const re-bind of the narrowed place both ride the gather; a direct
//! match over the narrowed place reads the member's own words and stays
//! the control.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

const SOURCE: &str = concat!(
    "tag int\n",
    "tag bool\n",
    "tag string\n",
    "tag my_err\n",
    "enum pair2 {\n",
    "  just(int, int)\n",
    "}\n",
    "enum tri3 {\n",
    "  pack(int, int, int)\n",
    "}\n",
    "extern fn println (s: string) -> ()\n",
    "\n",
    "fn fallible(b: bool) -> pair2 | my_err {\n",
    "  if b { just(7, 8) } else { my_err }\n",
    "}\n",
    "\n",
    "fn fallible3(b: bool) -> tri3 | my_err {\n",
    "  if b { pack(1, 2, 3) } else { my_err }\n",
    "}\n",
    "\n",
    "fn show_pair(p: pair2) -> () {\n",
    "  match p {\n",
    "    just(a, b) => println(\"inner ${a} ${b}\"),\n",
    "  }\n",
    "}\n",
    "\n",
    "fn show_tri(t: tri3) -> () {\n",
    "  match t {\n",
    "    pack(a, b, c) => println(\"tri ${a} ${b} ${c}\"),\n",
    "  }\n",
    "}\n",
    "\n",
    "fn main() -> () {\n",
    "  const t = fallible(true)\n",
    "  if t is pair2 {\n",
    "    show_pair(t)\n",
    "    const u = t\n",
    "    show_pair(u)\n",
    "    match t {\n",
    "      just(a, b) => println(\"direct ${a} ${b}\"),\n",
    "    }\n",
    "  }\n",
    "  const t3 = fallible3(true)\n",
    "  if t3 is tri3 {\n",
    "    show_tri(t3)\n",
    "    const u3 = t3\n",
    "    show_tri(u3)\n",
    "    match t3 {\n",
    "      pack(a, b, c) => println(\"direct3 ${a} ${b} ${c}\"),\n",
    "    }\n",
    "  }\n",
    "}\n",
);

/// The narrowed place copies into a call argument and a const re-bind
/// with every payload word at its own claimed region offset: the pair
/// reads `7 8` and the triple `1 2 3` through both copies, matching the
/// direct matches over the narrowed places.
#[test]
fn a_narrowed_sole_member_place_copies_every_payload_word() {
    let mut files = Files::new();
    let id = files.add_source("narrowed_copy.ds", SOURCE);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the probe checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "narrowed_copy",
        Hash128::compute(SOURCE.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the narrowed copies lower");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run("narrowed_copy", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the unit entry result"
    );
    assert_eq!(
        String::from_utf8_lossy(vm.captured_output()),
        concat!(
            "inner 7 8\n",
            "inner 7 8\n",
            "direct 7 8\n",
            "tri 1 2 3\n",
            "tri 1 2 3\n",
            "direct3 1 2 3\n",
        ),
        "every copy reads each payload word at its own offset"
    );
}
