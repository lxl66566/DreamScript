//! Singleton-set fields over lone unit words: a struct field (and a
//! tuple field's element, and a payloaded case's field) typed as a bare
//! unit tag or case carries the case's own Tag word — the same word the
//! value vocabulary rules the bare value — so the construction embeds,
//! the read hands off, and the tag test dispatches with no layout
//! disagreement. The plain singleton-annotated binding runs the same
//! word it always did.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// Builds one probe module into a fresh VM, asserting the fixture
/// checks clean so failures are the lowerer's or the VM's.
fn probe_vm(name: &str, source: &str) -> Vm {
    let combined = format!(
        "tag int\ntag string\ntag bool\ntrait Copyable {{ }}\nextern fn println (s: string) -> \
         ()\n{source}"
    );
    let mut files = Files::new();
    let id = files.add_source(name, &combined);
    let asts = [parse(&files, id).expect("the source parses")];
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
        name,
        Hash128::compute(combined.as_bytes()),
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

/// One acceptance: the run result and the golden stdout.
fn acceptance(name: &str, source: &str, stdout: &str) {
    let mut vm = probe_vm(name, source);
    vm.capture_output();
    assert_eq!(
        vm.run(name, "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns unit"
    );
    assert_eq!(vm.captured_output(), stdout.as_bytes());
}

#[test]
fn singleton_fields_carry_the_tag_word() {
    acceptance(
        "singleton_fields",
        r#"
enum tone {
  quiet
  loud { volume: int }
}
tag armed
struct holder {
  pick: quiet,
  flag: armed,
  n: int,
}
fn use(q: quiet) -> int {
  1
}
fn main() -> () {
  const plain: quiet = quiet
  const again: quiet = plain
  const h = holder { pick: quiet, flag: armed, n: 2 }
  var k = 0
  match h.pick {
    quiet => k = 5
  }
  const m = if h.flag is armed { 3 } else { 4 }
  println("k=${k} m=${m} n=${h.n} u=${use(h.pick)} a=${again is quiet}")
}
"#,
        "k=5 m=3 n=2 u=1 a=true\n",
    );
}

#[test]
fn tuple_elements_and_case_payloads_carry_the_tag_word() {
    acceptance(
        "singleton_span_members",
        r#"
enum tone {
  quiet
  loud { volume: int }
}
enum wrapped {
  big { pick: quiet },
  blank
}
struct pair {
  t: (quiet, int),
}
fn main() -> () {
  const p = pair { t: (quiet, 3) }
  const w: wrapped = big(pick: quiet)
  var a = 0
  var b = 0
  match p.t {
    (quiet, n) => a = n
  }
  match w {
    big { pick } => b = 6
    blank => b = 7
  }
  println("a=${a} b=${b}")
}
"#,
        "a=3 b=6\n",
    );
}
