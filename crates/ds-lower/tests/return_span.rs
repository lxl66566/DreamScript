//! The exit-lowering probe: a mid-block `return e` whose operand union
//! is a strict subset of the declared return (a different, narrower
//! union type) checks clean by containment and must lower — the union
//! return path tag-dispatches per member and re-spans into the return
//! union's region instead of demanding a single atom (DS4009 before).
//! The run proves the dispatch: both members survive with their own
//! payloads, not a swapped or zeroed span.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Value, Vm, loader, native};

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
    registry.install_native(native::STD_PLATFORM_MODULE, native::std_platform_module());
    loader::install(&mut registry, &module).expect("the module installs");
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
