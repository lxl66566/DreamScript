//! The binding/assignment widening probe: a value whose union is a
//! strict subset of the destination type (an annotated `var` initializer,
//! a re-assignment, or an if arm joining into a wider result) checks clean
//! by containment and must lower — the union write path tag-dispatches
//! per member and re-spans into the destination region instead of
//! demanding a single atom (DS4009 before). The run proves both members
//! carry their own payloads through the widened span.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

const SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "tag bool\n",
    "tag io_error\n",
    "tag parse_error\n",
    "struct config { host: string, port: int }\n",
    "extern fn println (msg: string) -> ()\n",
    "\n",
    "fn parse_step(s: string) -> config | parse_error {\n",
    "  if s == \"ok\" { config { host: \"h\", port: 1 } } else { parse_error }\n",
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
    "  // the var initializer is one member short of the declared union\n",
    "  var store: config | io_error | parse_error = parse_step(\"ok\")\n",
    "  show(store)\n",
    "  // the assignment widens the same sub-union over the old value\n",
    "  store = parse_step(\"bad\")\n",
    "  show(store)\n",
    "  store = io_error\n",
    "  show(store)\n",
    "  // a value-position if joins one sub-union arm into the wide result\n",
    "  const picked = if true { parse_step(\"ok\") } else { io_error }\n",
    "  show(picked)\n",
    "  const flipped = if false { parse_step(\"ok\") } else { io_error }\n",
    "  show(flipped)\n",
    "}\n",
);

#[test]
fn a_strict_sub_union_binds_assigns_and_joins_through_the_wide_region() {
    let mut files = Files::new();
    let id = files.add_source("widen_span.ds", SOURCE);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "containment already proves the writes: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "widen_span",
        Hash128::compute(SOURCE.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("a strict-sub-union binding lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run("widen_span", "main", &[]),
        Ok(Value::I64(0)),
        "the unit entry result"
    );
    assert_eq!(
        String::from_utf8_lossy(vm.captured_output()),
        concat!("h\n", "parse\n", "io\n", "h\n", "io\n",),
        "every widened write keeps its member and payload"
    );
}

/// Containment is the checker's proof and it stays the gate: a
/// destination union missing a member the source carries never reaches
/// lowering (DS2001, not a lowered DS4009).
#[test]
fn an_incompatible_union_binding_stays_rejected_at_the_checker() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "tag io_error\n",
        "tag parse_error\n",
        "struct config { host: string, port: int }\n",
        "fn parse_step(s: string) -> config | parse_error {\n",
        "  if s == \"ok\" { config { host: \"h\", port: 1 } } else { parse_error }\n",
        "}\n",
        "fn f() -> () {\n",
        "  var v: io_error | parse_error = parse_step(\"ok\")\n",
        "}\n",
    );
    let mut files = Files::new();
    let id = files.add_source("widen_negative.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    let rendered = analyzed
        .checked
        .diags
        .iter()
        .map(|d| format!("{} {}", d.code.as_str(), d.message))
        .collect::<Vec<_>>();
    assert_eq!(
        rendered.first().map(String::as_str),
        Some(
            "DS2001 `annotated binding` has type `parse_error | config`, not contained in \
             `io_error | parse_error` [B2]"
        )
    );
}
