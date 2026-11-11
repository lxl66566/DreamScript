//! The `ds run` subcommand: execute one module — a validated mmap-read
//! dsmod file, or a .ds source compiled in-process through the source
//! pipeline (parse, check, lower, install) — resolving one entry
//! function and reporting its result.

use std::path::{Path, PathBuf};

use anyhow::{Context, Error, Result, anyhow};
use clap::Args;
use ds_diag::{Files, human};
use ds_mod::{LoadError as ModLoadError, Module, ReadError, read_mmap};
use ds_vm::{
    LoadError as VmLoadError, ModuleBuildError, ModuleRegistry, Trap, Value, Vm,
    abi::SlotType,
    native::{
        PLATFORM_IO_ERR, PLATFORM_NET_ERR, PLATFORM_NONE, PLATFORM_PARSE_ERR, STD_PLATFORM_MODULE,
        std_platform_module,
    },
};

use crate::source;

/// The CLI is the default platform provider (H5.5): every run installs
/// the `std` platform natives (console, file, net) into its registry.
/// This must precede [`Vm::install_dsmod`] — the loader validates pool
/// FuncRefs at install time, so a world-native call site in the module
/// would fail as an unresolved callee otherwise (`Vm::new` installs the
/// builtin core only, and too late for this path regardless).
fn provide_platform(registry: &mut ModuleRegistry) {
    registry.install_native(STD_PLATFORM_MODULE, std_platform_module());
}

/// The `ds run` surface: one module or source file, one entry name, i64
/// arguments.
#[derive(Debug, Args)]
pub struct RunArgs {
    /// Path to the .dsmod module file or the .ds source file
    file: PathBuf,
    /// Entry function to execute
    #[arg(long, default_value = "main")]
    entry: String,
    /// i64 arguments staged into the entry's parameter slots
    #[arg(long, num_args = 0.., value_name = "I64")]
    args: Vec<i64>,
    /// Compile without the stdlib ambient package member (source files only)
    #[arg(long)]
    no_ambient: bool,
}

/// Runs one module: the dsmod path mmaps and installs; the source path
/// compiles in-process first. Both then resolve one entry and run it.
pub fn run(args: &RunArgs) -> Result<()> {
    let mut prepared = match source_or_module(&args.file)? {
        FileKind::Dsmod => read_dsmod(args)?,
        FileKind::Source => compile_source(args)?,
    };
    resolve_and_run(&mut prepared, &args.entry)
}

/// The extension as UTF-8, when it is.
fn ds_std_ext(ext: &std::ffi::OsStr) -> Option<&str> {
    ext.to_str()
}

/// The file kinds `ds run` executes.
enum FileKind {
    /// A compiled dsmod module.
    Dsmod,
    /// A .ds source file, compiled in-process.
    Source,
}

/// The file's kind, by extension.
fn source_or_module(path: &Path) -> Result<FileKind> {
    match path.extension().and_then(ds_std_ext).unwrap_or("") {
        "dsmod" => Ok(FileKind::Dsmod),
        "ds" => Ok(FileKind::Source),
        other => Err(anyhow!(
            "`{}` has extension `{other}`; ds run executes .ds sources and .dsmod modules",
            path.display()
        )),
    }
}

/// One run prepared for execution: the VM holding the installed module,
/// the staged entry arguments, and the module's name and function names.
struct PreparedRun {
    vm: Vm,
    staged: Vec<Value>,
    name: String,
    functions: Vec<String>,
}

/// The existing dsmod path: validated mmap read and install.
fn read_dsmod(args: &RunArgs) -> Result<PreparedRun> {
    let module = read_mmap(&args.file).map_err(|error| open_error(&args.file, &error))?;
    let name = module.module().name().to_owned();
    let mut registry = ModuleRegistry::new();
    provide_platform(&mut registry);
    let mut vm = Vm::new(registry);
    vm.install_dsmod(module.module())
        .map_err(|error| install_error(module.module(), &error))?;
    Ok(PreparedRun {
        staged: args.args.iter().map(|&value| Value::I64(value)).collect(),
        functions: module
            .module()
            .functions()
            .iter()
            .map(|function| function.name().to_owned())
            .collect(),
        name,
        vm,
    })
}

/// The source path: compile, install, and stage the entry's arguments per
/// the entry parameters' checked types.
fn compile_source(args: &RunArgs) -> Result<PreparedRun> {
    let compiled = source::compile(&args.file, &args.entry, args.no_ambient)?;
    let mut registry = ModuleRegistry::new();
    provide_platform(&mut registry);
    let mut vm = Vm::new(registry);
    vm.install_dsmod(compiled.lowered.module())
        .map_err(|error| install_error(compiled.lowered.module(), &error))?;
    Ok(PreparedRun {
        staged: stage_source_args(&compiled, &args.args)?,
        functions: compiled.lowered.functions().to_vec(),
        name: compiled.name,
        vm,
    })
}

/// Resolves the entry, runs it, and reports the result: a platform
/// failure tag (io, net, parse) is a failed run — the lowered
/// unhandled-failure handler already printed the message — the `none`
/// member of a checked read prints by name, and everything else prints
/// like the dsmod path always did.
fn resolve_and_run(prepared: &mut PreparedRun, entry: &str) -> Result<()> {
    if prepared
        .vm
        .registry()
        .resolve(&prepared.name, entry)
        .is_none()
    {
        return Err(anyhow!(
            "module `{}` has no function `{entry}` (available: {})",
            prepared.name,
            prepared.functions.join(", ")
        ));
    }
    match prepared.vm.run(&prepared.name, entry, &prepared.staged) {
        Ok(value) => match value {
            Value::Tag(tag) if tag == PLATFORM_IO_ERR => Err(anyhow!(
                "the run halted on an unhandled io failure (the message is on stdout)"
            )),
            Value::Tag(tag) if tag == PLATFORM_NET_ERR => Err(anyhow!(
                "the run halted on an unhandled net failure (the message is on stdout)"
            )),
            Value::Tag(tag) if tag == PLATFORM_PARSE_ERR => Err(anyhow!(
                "the run halted on an unhandled parse failure (the message is on stdout)"
            )),
            // The `none` member of a checked read is a plain result: print
            // its name, not the platform reservation's raw id.
            Value::Tag(tag) if tag == PLATFORM_NONE => {
                println!("none");
                Ok(())
            },
            value => {
                print_value(&value);
                Ok(())
            },
        },
        Err(trap) => Err(trap_error(&prepared.vm, &trap)),
    }
}

/// Stages `--args` onto the entry's parameter words per the parameters'
/// checked types (the source pipeline's plan): the scalar atoms stage
/// their word, an `int | none` parameter stages its two-word union span —
/// a staged value presenting the int member, an absent value presenting
/// none. Without a plan (the entry is not a source function) every value
/// stages as its own i64 word, the dsmod rule.
// the i64->f64 staging of --args is a lossy host conversion by design
#[allow(clippy::cast_precision_loss)]
fn stage_source_args(compiled: &source::CompiledSource, raw: &[i64]) -> Result<Vec<Value>> {
    let Some(plans) = &compiled.entry_args else {
        return Ok(raw.iter().map(|&value| Value::I64(value)).collect());
    };
    if raw.len() > plans.len() {
        return Err(anyhow!(
            "--args stages {} value(s) but entry `{}` takes {} parameter(s)",
            raw.len(),
            compiled.name,
            plans.len()
        ));
    }
    let mut staged = Vec::new();
    for (index, plan) in plans.iter().enumerate() {
        let value = raw.get(index).copied();
        match plan {
            source::ArgPlan::Word(slot) => {
                let Some(value) = value else {
                    return Err(anyhow!(
                        "entry `{}` parameter {} expects an --args value",
                        compiled.name,
                        index + 1
                    ));
                };
                staged.push(match *slot {
                    SlotType::I64 => Value::I64(value),
                    SlotType::F64 => Value::F64(value as f64),
                    SlotType::Bool => Value::Bool(value != 0),
                    other => return Err(anyhow!("--args stages no {other:?} word")),
                });
            },
            source::ArgPlan::OptionalInt => {
                let member = |name: &str| {
                    compiled.lowered.tag(name).with_context(|| {
                        format!(
                            "the `{name}` member has no module tag id; construct an \\
                                 int|none somewhere so lowering assigns one"
                        )
                    })
                };
                if let Some(value) = value {
                    staged.push(Value::Tag(member("int")?));
                    staged.push(Value::I64(value));
                } else {
                    staged.push(Value::Tag(member("none")?));
                    staged.push(Value::I64(0));
                }
            },
            source::ArgPlan::Foreign => {
                return Err(anyhow!(
                    "--args cannot stage entry `{}`'s parameter {} (int, float, bool, and \\
                     int|none parameters stage)",
                    compiled.name,
                    index + 1
                ));
            },
        }
    }
    Ok(staged)
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
        ModuleBuildError::FrameTooWide { func, slots } => {
            format!("function `{func}`'s frame ({slots} slots) exceeds the u8 slot space")
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
        ModuleBuildError::RetSpanLayout {
            func,
            src,
            span,
            slots,
        } => {
            format!("RETSPAN at slot {src} spans {span} words, past `{func}`'s {slots} slots")
        },
    }
}

/// Prints the run result to stdout. Unit-result entries halt on the inert
/// unit word (`I64(0)`), so nothing distinguishes them; a heap reference
/// prints as its object-table index (the object kind is a VM concern).
fn print_value(value: &Value) {
    match value {
        Value::I64(v) => println!("{v}"),
        Value::F64(v) => println!("{v:?}"),
        Value::Bool(v) => println!("{v}"),
        Value::Tag(v) => println!("{v}"),
        Value::Ref(v) => println!("<object {v}>"),
        Value::Fn(f) => println!("<fn {}:{}>", f.module(), f.func()),
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
        Trap::UnknownModule { module } => {
            format!("callable word names module id {module}, which is not installed")
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
        Trap::UnknownInstance { evidence } => {
            format!("generic evidence word {evidence} names no builtin instance")
        },
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
        Trap::UnknownObject { obj } => {
            format!("reference {obj} names no heap object")
        },
        Trap::RetSpanTooWide { span, caller_area } => format!(
            "span return of {span} words exceeds the caller's {caller_area}-slot argument area"
        ),
        Trap::OutputWrite { kind } => format!("output write failed: {kind:?}"),
        Trap::MetaWordCorrupt { fp } => {
            format!("frame metadata word corrupt at stack index {fp}")
        },
        Trap::ReentrancyRefused => "a native container call's lambda ended its fiber (HALT, fiber \
                                    switch, or yield under nesting) instead of returning"
            .to_owned(),
    }
}
