//! Regression pin of the span rework's first slice: the union payload
//! claim rework must leave the lowered frame layouts of union-carrying
//! corpus shapes byte-identical — the region claim, the zero-init
//! placement, and every payload read live inside these slot vectors.
//! The asserted layouts were recorded from the pre-rework allocator.

use ds_abi::SlotType;
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::{Hash128, read_checked, write};
use ds_parse::parse;

const SRC: &str = concat!(
    "tag int\n",
    "tag string\n",
    "tag io_error\n",
    "struct config { host: string, port: int }\n",
    "struct bad_port { expected: int, got: int }\n",
    "extern fn println (msg: string) -> ()\n",
    "fn port_of(r: config | io_error) -> int {\n",
    "  match r {\n",
    "    io_error => 0,\n",
    "    c => c.port,\n",
    "  }\n",
    "}\n",
    "fn show(r: config | bad_port | io_error) -> () {\n",
    "  match r {\n",
    "    io_error => println(\"io\"),\n",
    "    c => println(\"payload\"),\n",
    "  }\n",
    "}\n",
    "fn main() -> () {\n",
    "  var store: config | bad_port | io_error = config { host: \"h\", port: 1 }\n",
    "  store = bad_port { expected: 2, got: 3 }\n",
    "  show(store)\n",
    "  show(io_error)\n",
    "  var p = port_of(io_error)\n",
    "  store = config { host: \"x\", port: p }\n",
    "  show(store)\n",
    "}\n",
);

/// The lowered layout of one function, read back from the module bytes.
fn layout_of<'m>(
    module: &'m ds_mod::Module,
    lowered: &ds_lower::Lowered,
    name: &str,
) -> &'m ds_abi::FrameLayout {
    module.functions()[lowered.function_index(name).expect("the function planned") as usize]
        .layout()
}

#[test]
fn union_carrying_layouts_stay_bit_identical() {
    let mut files = Files::new();
    let id = files.add_source("union_pins.ds", SRC);
    let asts = [parse(&files, id).expect("the source parses")];
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
        "union_pins",
        Hash128::compute(SRC.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the probe lowers");
    let bytes = write(lowered.module());
    let module = read_checked(&bytes, SRC.as_bytes()).expect("the module round-trips");

    // port_of: the two-word-io_error config union param spans three
    // slots, the match result and its condition one each
    let port_of = layout_of(&module, &lowered, "port_of");
    assert_eq!(port_of.params_len(), 3);
    assert_eq!(port_of.arg_area_len(), 0);
    assert_eq!(port_of.slots(), &[
        SlotType::Tag,
        SlotType::Ref,
        SlotType::I64,
        SlotType::Bool,
        SlotType::I64
    ]);

    // show: the three-member union param spans four slots (the shared
    // region: string host, the shared port word, bad_port's grown word)
    let show = layout_of(&module, &lowered, "show");
    assert_eq!(show.params_len(), 4);
    assert_eq!(show.arg_area_len(), 1);
    assert_eq!(show.slots(), &[
        SlotType::Tag,
        SlotType::Ref,
        SlotType::I64,
        SlotType::I64,
        SlotType::Bool,
        SlotType::Ref,
        SlotType::I64,
        SlotType::Ref,
        SlotType::I64,
        SlotType::Raw,
    ]);

    // main: the widened var, both struct-literal re-spans, and both
    // staged call shapes
    let main = layout_of(&module, &lowered, "main");
    assert_eq!(main.params_len(), 0);
    assert_eq!(main.arg_area_len(), 4);
    assert_eq!(main.slots(), &[
        SlotType::Ref,
        SlotType::I64,
        SlotType::Ref,
        SlotType::I64,
        SlotType::Tag,
        SlotType::Ref,
        SlotType::I64,
        SlotType::I64,
        SlotType::I64,
        SlotType::I64,
        SlotType::I64,
        SlotType::I64,
        SlotType::I64,
        SlotType::I64,
        SlotType::Tag,
        SlotType::Tag,
        SlotType::Ref,
        SlotType::I64,
        SlotType::I64,
        SlotType::I64,
        SlotType::Tag,
        SlotType::Tag,
        SlotType::Ref,
        SlotType::I64,
        SlotType::I64,
        SlotType::I64,
        SlotType::Ref,
        SlotType::I64,
        SlotType::Ref,
        SlotType::I64,
        SlotType::Raw,
        SlotType::Ref,
        SlotType::I64,
        SlotType::I64,
    ]);
}
