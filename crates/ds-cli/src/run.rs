//! The `ds run` subcommand: mmap-read a dsmod module file with full
//! validation, install it into a fresh VM, and execute one entry function.

use std::path::{Path, PathBuf};

use anyhow::{Error, Result, anyhow};
use clap::Args;
use ds_diag::{Files, human};
use ds_mod::{LoadError as ModLoadError, Module, ReadError, read_mmap};
use ds_vm::{LoadError as VmLoadError, ModuleBuildError, ModuleRegistry, Trap, Value, Vm};

/// The `ds run` surface: one module file, one entry name, i64 arguments.
#[derive(Debug, Args)]
pub struct RunArgs {
    /// Path to the .dsmod module file
    file: PathBuf,
    /// Entry function to execute
    #[arg(long, default_value = "main")]
    entry: String,
    /// i64 arguments staged into the entry's parameter slots
    #[arg(long, num_args = 0.., value_name = "I64")]
    args: Vec<i64>,
}

/// Runs one module: validated mmap read, install, entry execution.
pub fn run(args: &RunArgs) -> Result<()> {
    let module = read_mmap(&args.file).map_err(|error| open_error(&args.file, &error))?;
    let name = module.module().name().to_owned();
    let mut vm = Vm::new(ModuleRegistry::new());
    vm.install_dsmod(module.module())
        .map_err(|error| install_error(module.module(), &error))?;
    if vm.registry().resolve(&name, &args.entry).is_none() {
        let names: Vec<&str> = module
            .module()
            .functions()
            .iter()
            .map(ds_mod::Function::name)
            .collect();
        return Err(anyhow!(
            "module `{name}` has no function `{}` (available: {})",
            args.entry,
            names.join(", ")
        ));
    }
    let staged = args
        .args
        .iter()
        .map(|&value| Value::I64(value))
        .collect::<Vec<_>>();
    match vm.run(&name, &args.entry, &staged) {
        Ok(value) => {
            print_value(&value);
            Ok(())
        },
        Err(trap) => Err(trap_error(&vm, &trap)),
    }
}

/// Missing files get one line; format failures get the structured ds-diag
/// rendering over the offending bytes.
fn open_error(path: &Path, error: &ModLoadError) -> Error {
    match error {
        ModLoadError::Io(err) => anyhow!("cannot open module `{}`: {err}", path.display()),
        ModLoadError::Format(err) => anyhow!("{}", render_read_error(path, err)),
    }
}

/// Renders a [`ReadError`] as a human diagnostic against the file's bytes.
///
/// ds-diag sources are strings, so bytes map one-for-one to printable ASCII
/// (non-graphic bytes become `.`); the mapping preserves every byte offset,
/// keeping the diagnostic's spans exact over binary content.
fn render_read_error(path: &Path, error: &ReadError) -> String {
    let bytes = std::fs::read(path).unwrap_or_default();
    let text: String = bytes
        .iter()
        .map(|&b| if b.is_ascii_graphic() || b == b' ' { char::from(b) } else { '.' })
        .collect();
    let mut files = Files::new();
    let file = files.add_source(path.display().to_string(), &text);
    human(&error.diagnostic(file), &files).trim_end().to_owned()
}

/// Why a structurally valid module still cannot be installed.
fn install_error(module: &Module, error: &VmLoadError) -> Error {
    match error {
        VmLoadError::UnresolvedCallee {
            func,
            module: callee,
            index,
        } => {
            if callee.is_empty() || callee == module.name() {
                anyhow!(
                    "function `{func}` references function index {index}, past module `{}`'s own \
                     table of {} functions",
                    module.name(),
                    module.functions().len()
                )
            } else {
                anyhow!(
                    "module `{callee}` is not installed (function `{func}` calls its function \
                     {index}); install dependencies before their dependents"
                )
            }
        },
        VmLoadError::Build(error) => anyhow!(
            "module `{}` fails the VM load checks: {}",
            module.name(),
            describe_build_error(error)
        ),
    }
}

fn describe_build_error(error: &ModuleBuildError) -> String {
    match error {
        ModuleBuildError::DuplicateFunction(name) => format!("duplicate function `{name}`"),
        ModuleBuildError::PoolTooLarge { func, len } => {
            format!("function `{func}`'s pool ({len} entries) exceeds the u16 index space")
        },
        ModuleBuildError::BodyDecode { func, pc, cause } => {
            format!("word at pc {pc} in `{func}` does not decode ({cause:?})")
        },
        ModuleBuildError::FuncfMismatch {
            func,
            funcef,
            layout,
        } => {
            format!("FUNCF declares {funcef} slots in `{func}`, its layout declares {layout}")
        },
        ModuleBuildError::PoolIndex { func, kidx } => {
            format!("pool index {kidx} out of range in `{func}`")
        },
        ModuleBuildError::PoolKind {
            func,
            kidx,
            expected,
            found,
        } => {
            format!("pool entry {kidx} in `{func}` is {found:?}, the uop requires {expected:?}")
        },
    }
}

/// Prints the run result to stdout. The VM has no unit value yet; when one
/// exists it will print nothing.
fn print_value(value: &Value) {
    match value {
        Value::I64(v) => println!("{v}"),
        Value::F64(v) => println!("{v:?}"),
        Value::Bool(v) => println!("{v}"),
        Value::Tag(v) => println!("{v}"),
    }
}

/// Formats a trap plus the backtrace summary the VM kept for it.
fn trap_error(vm: &Vm, trap: &Trap) -> Error {
    use std::fmt::Write;
    // writing into a String cannot fail
    let mut message = format!("trap: {}", describe_trap(trap));
    let frames = vm.backtrace();
    if !frames.is_empty() {
        message.push_str("\nbacktrace (innermost first):");
        for frame in frames {
            let _ = write!(
                message,
                "\n  {}::{} at pc {}",
                frame.module, frame.func, frame.pc.0
            );
            if frame.elided > 0 {
                let _ = write!(message, " (+{} tail frames elided)", frame.elided);
            }
        }
    }
    anyhow!(message)
}

fn describe_trap(trap: &Trap) -> String {
    match trap {
        Trap::StackOverflow { needed, limit } => {
            format!("stack overflow: entering needs {needed} words, the limit is {limit}")
        },
        Trap::IntegerOverflow => "integer overflow".to_owned(),
        Trap::DivideByZero => "division by zero".to_owned(),
        Trap::CvtOutOfRange => "float-to-int conversion out of range".to_owned(),
        Trap::UnknownFunction { module, func } => {
            format!("module `{module}` has no function `{func}`")
        },
        Trap::UnknownCallee { module, func } => {
            format!("call into module `{module}` function index {func}, which is not installed")
        },
        Trap::ArityMismatch { expected, given } => {
            format!("callee expects {expected} argument(s), {given} staged")
        },
        Trap::SlotTypeMismatch {
            slot,
            expected,
            actual,
        } => {
            format!("slot {slot} holds {actual:?} where {expected:?} is required")
        },
        Trap::SlotOutOfRange { slot, frame_slots } => {
            format!("uop addresses slot {slot} outside the {frame_slots}-slot frame")
        },
        Trap::CallLayout(error) => format!("call-site layout rejected by the ABI: {error:?}"),
        Trap::InvalidPc { pc, code_len } => {
            format!("control left the code stream: pc {pc} of {code_len}")
        },
        Trap::FrameSlotsMismatch { funcef, layout } => {
            format!("FUNCF declares {funcef} slots, the layout declares {layout}")
        },
        Trap::EntryWordMismatch { resolved, expected } => {
            format!("entry descriptor resolved {resolved}, expected {expected}")
        },
        Trap::PoolIndex { kidx } => format!("constant-pool index {kidx} out of range"),
        Trap::PoolKind {
            kidx,
            expected,
            found,
        } => format!("constant-pool entry {kidx} is {found:?}, the uop requires {expected:?}"),
        Trap::BadEncoding { pc, cause } => {
            format!("word at pc {pc} does not decode ({cause:?})")
        },
        Trap::MetaWordCorrupt { fp } => {
            format!("frame metadata word corrupt at stack index {fp}")
        },
    }
}
