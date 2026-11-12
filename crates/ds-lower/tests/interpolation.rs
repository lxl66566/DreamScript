//! The interpolation boundary end to end [F6]: printable primitives
//! convert through the str natives inside the STRCAT chain, strings and
//! chars pass their Ref word through untouched, both hole spellings
//! work, the pinned escape set cooks the literal parts, and the empty
//! string literal lowers as a plain constant.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::{Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

/// The declaration prelude every case shares.
const PRELUDE: &str = concat!(
    "tag int\n",
    "tag float\n",
    "tag bool\n",
    "tag char\n",
    "tag string\n",
    "extern fn println (s: string) -> ()\n",
);

/// Parses, checks, lowers, and installs one inline source, then runs
/// `main` and returns the captured stdout.
fn run_stdout(body: &str) -> String {
    let source = format!("{PRELUDE}{body}");
    let mut files = Files::new();
    let id = files.add_source("inline.ds", &source);
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
    let lowered = lower(
        "inline",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the source lowers");
    let bytes = write(lowered.module());
    let module = read_checked(&bytes, source.as_bytes()).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(vm.run("inline", "main", &[]), Ok(Value::I64(0)));
    String::from_utf8(vm.captured_output().to_vec()).expect("stdout is UTF-8")
}

#[test]
fn primitives_convert_and_strings_pass_through_raw() {
    let stdout = run_stdout(concat!(
        "fn main() -> () {\n",
        "  const n = 42\n",
        "  const f = 2.5\n",
        "  const ok = true\n",
        "  const c = 'x'\n",
        "  const s = \"wor\\\"ld\\n\"\n",
        // the int, float, bool holes ride str natives inside the STRCAT
        // chain; the char and string holes pass their Ref word through
        // with no re-escaping of the embedded value
        "  println(\"n=${n} f=${f} ok=${ok} c=${c} s=${s}end\")\n",
        "}\n",
    ));
    assert_eq!(stdout, "n=42 f=2.5 ok=true c=x s=wor\"ld\nend\n");
}

#[test]
fn bare_ident_holes_and_the_empty_literal() {
    let stdout = run_stdout(concat!(
        "fn main() -> () {\n",
        "  const n = 7\n",
        "  println(\"n is $n\")\n",
        // the empty literal lowers as one constant, not a chain
        "  println(\"\")\n",
        "  println(\"done\")\n",
        "}\n",
    ));
    assert_eq!(stdout, "n is 7\n\ndone\n");
}

/// An explicit `str(e)` call routes through the same checked-type
/// conversion the interpolation holes use [F6]: scalars convert through
/// the str natives, strings pass their Ref word through, and a
/// tail-position call returns the conversion like any value.
#[test]
fn explicit_str_calls_run_the_hole_conversion() {
    let stdout = run_stdout(concat!(
        "extern fn str[t] (v: t) -> string
",
        "fn text(n: int) -> string {
",
        "  str(n)
",
        "}
",
        "fn main() -> () {
",
        "  println(str(42))
",
        "  println(str(\"word\"))
",
        "  println(text(7))
",
        "}
",
    ));
    assert_eq!(
        stdout,
        "42
word
7
"
    );
}

#[test]
fn the_pinned_escape_set_cooks_the_literal_parts() {
    let stdout = run_stdout(concat!(
        "fn main() -> () {\n",
        "  println(\"a\\tb\\nc\\rd\\\\e\\\"f\\'g\\0h\\u{41}\")\n",
        "}\n",
    ));
    assert_eq!(stdout, "a\tb\nc\rd\\e\"f'g\0hA\n");
}
