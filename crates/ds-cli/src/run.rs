//! The `ds run` subcommand: execute one module — a validated dsmod file
//! read from bytes, or a .ds source compiled in-process through the source
//! pipeline (parse, check, lower, install), with the script-mode dsmod
//! cache consulted before that pipeline — resolving one entry function
//! and reporting its result. A .ds source with a `ds.pkg` beside it runs
//! as a package: the manifest resolves over the store, the deps-first
//! walk installs the linked graph before the root, and the same
//! script-mode caches — the dsmod artifact, the checked-prefix `.dsi`,
//! and the dep-member dsmod artifacts, each linked package's own
//! compiled module keyed on its content identity — serve the root and
//! the walk's dep demands across runs under keys that fold the root's
//! own member identity and the graph's dep identities.

use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{Error, Result, anyhow};
use clap::Args;
use ds_diag::{Files, human};
use ds_mod::{Module, ReadError, read};
use ds_vm::{
    ArenaMisuse, ArenaPackError, FIBER_ROOT, FramePc, LoadError as VmLoadError, ModuleBuildError,
    ModuleRegistry, ReplEntry, StagedSite, Trap, Value, Vm,
    native::{PLATFORM_NONE, PLATFORM_PARSE_ERR, PLATFORM_UTF8_ERR, is_io_class, is_net_class},
    platform::Platform,
};

use crate::{package, run_cache::RunCache, shutdown::ShutdownWatch, source};

/// The CLI is the platform provider (H5, E1): every run installs the
/// default `std` platform handler (console, fsys, net, time) into its
/// registry, and the console-only `embed` handler beside it under its
/// own name, so a source declaring `platform "embed"` runs against
/// the bare face — every world op beyond print/println refusing at
/// install as an unprovided op. This must precede [`Vm::install_dsmod`]
/// — the loader validates pool FuncRefs against the installed
/// platforms' served ops at install time, so a world call site in the
/// module would fail as an unprovided or unresolved op otherwise
/// (`Vm::new` installs the builtin core only, and too late for this
/// path regardless).
fn provide_platform(registry: &mut ModuleRegistry) {
    registry.install_platform(Platform::std());
    registry.install_platform(Platform::embed());
}

/// The `ds run` surface: one module or source file, one entry name, and
/// the string arguments the program reads through `args()`.
#[derive(Debug, Args)]
pub struct RunArgs {
    /// Path to the .dsmod module file or the .ds source file
    file: PathBuf,
    /// Entry function to execute
    #[arg(long, default_value = "main")]
    entry: String,
    /// String arguments the running program reads through `args()` (the
    /// CLI process surface)
    #[arg(long, num_args = 0.., value_name = "STRING")]
    args: Vec<String>,
    /// Compile without the stdlib ambient package member (source files only)
    #[arg(long)]
    no_ambient: bool,
    /// Run .ds sources without consulting the script-mode caches (the
    /// dsmod artifact and the checked-prefix `.dsi`)
    #[arg(long)]
    no_cache: bool,
}

/// Runs one module: the dsmod path mmaps and installs; the source path
/// compiles in-process first. Both then resolve one entry and run it.
/// The host owns the exit code (the CLI process surface): a clean
/// completion exits 0, a trap or an unhandled
/// failure 1, and an `exit(code)` call answers its deliberate status.
pub fn run(args: &RunArgs) -> Result<ExitCode> {
    // The host's shutdown door, armed
    // before any other thread can spawn: the signals block on this
    // thread first — mask inheritance carries the block to every
    // thread the run later spawns (the fork-join workers, the
    // resolver helper), so no thread can receive the default
    // disposition — and the drive observes the event through the
    // door the platform's shape arms below.
    let signals = ShutdownWatch::install()?;
    let mut prepared = match source_or_module(&args.file)? {
        FileKind::Dsmod => read_dsmod(args)?,
        FileKind::Source => run_source(args)?,
    };
    // the process arguments stage on the VM once, the `args()` op
    // reading them verbatim whatever the entry
    prepared.vm.set_argv(args.args.clone());
    // The scheduler's CLI half: `ds run` installs the
    // platform's reactor, so the std platform's net face parks on a
    // would-block instead of blocking the drive — the single-thread
    // executor runs the next ready fiber and the reactor bridges the
    // gap (the epoll driver on Linux, the tier-1 host; a host with no
    // driver keeps the blocking natives, blocking semantics staying the
    // embedder's choice). The REPL and dev hosts install the reactor
    // over the same seam and pump the executor between their own
    // waits (the prompt's reads, the watch windows).
    if let Some(reactor) = ds_vm::reactor::platform_default() {
        prepared.vm.install_reactor(reactor);
    }
    // the platform's own shutdown door: Linux's signalfd registers as
    // the reactor's own interest, every other host's helper flips the
    // blocking drive's stop flag
    signals.arm(&mut prepared.vm)?;
    resolve_and_run(&mut prepared, &args.entry, &signals)
}

/// The `ds test` re-entry's drive: one test module — the package's
/// members composed beside the test file, served from the script cache
/// or freshly compiled — installs through the same deps-first walk
/// as a package run over the caller's cache handle (the walk's
/// dep-member artifacts serve warm and store what compiles), then the
/// run tail as ever: empty argv, the platform reactor, the shutdown
/// door, the `main` entry.
///
/// # Errors
///
/// Every refusal the install walk and the drive report.
pub(crate) fn run_test_entry(
    module: Module,
    graph: &package::Graph,
    cache: &RunCache,
) -> Result<ExitCode> {
    let signals = ShutdownWatch::install()?;
    let name = module.name().to_owned();
    let functions: Vec<String> = module
        .functions()
        .iter()
        .map(|function| function.name().to_owned())
        .collect();
    // the walk persists no dep `.dsi`: the test fold keys over the
    // session shape, so the prefix tier's readers (whose probes fold
    // the package shape) would never serve those files
    let vm = install_package_root(module, graph, cache, None, &name, functions.len())?;
    let mut prepared = PreparedRun {
        vm,
        name,
        functions,
    };
    prepared.vm.set_argv(Vec::new());
    if let Some(reactor) = ds_vm::reactor::platform_default() {
        prepared.vm.install_reactor(reactor);
    }
    signals.arm(&mut prepared.vm)?;
    resolve_and_run(&mut prepared, "main", &signals)
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

/// One run prepared for execution: the VM holding the installed module
/// and the module's name and function names.
struct PreparedRun {
    vm: Vm,
    name: String,
    functions: Vec<String>,
}

/// The existing dsmod path: validated read and install. The file is the
/// user's own, outside the cache artifacts' write-once discipline, so the
/// run reads its bytes and validates from those instead of mapping (a
/// mapping's stability contract cannot name a file another process may
/// rewrite under the run).
fn read_dsmod(args: &RunArgs) -> Result<PreparedRun> {
    let bytes = std::fs::read(&args.file)
        .map_err(|error| anyhow!("cannot open module `{}`: {error}", args.file.display()))?;
    let module =
        read(&bytes).map_err(|error| anyhow!("{}", render_read_error(&args.file, &error)))?;
    install_module(module)
}

/// Installs one finished module into a fresh VM with the default std
/// platform — the dsmod path's rule, and the cache hit path's.
fn install_module(module: Module) -> Result<PreparedRun> {
    let name = module.name().to_owned();
    let functions: Vec<String> = module
        .functions()
        .iter()
        .map(|function| function.name().to_owned())
        .collect();
    let mut registry = ModuleRegistry::new();
    provide_platform(&mut registry);
    let mut vm = Vm::new(registry);
    vm.install_dsmod(module)
        .map_err(|error| install_error(&name, functions.len(), &error))?;
    Ok(PreparedRun {
        vm,
        name,
        functions,
    })
}

/// The source path: a package-shaped root (a `ds.pkg` beside the source)
/// takes the package path first; a manifest-free source with the ambient
/// on takes the same path over the implicit `@std` graph (the
/// bare-specifier switch: the default bundle resolves ahead of the
/// compile, so the script session composes the @std bodied members
/// back beside the core — the bare load-bearing migrated names keep
/// resolving — while its `@std` imports bind through the linked
/// surface); everything else
/// is the script-mode dsmod cache first — a servable cached artifact
/// installs without the front end — then the in-process compile exactly
/// as before, caching what a later run of the same inputs can serve.
fn run_source(args: &RunArgs) -> Result<PreparedRun> {
    if let Some(graph) = package::open(&args.file, args.no_ambient)? {
        return run_package(args, &graph, false);
    }
    if !args.no_ambient {
        return run_package(args, &package::implicit_std_graph(), true);
    }
    let cache = RunCache::discover(!args.no_cache, args.no_ambient);
    let prefix = crate::dsi_cache::PrefixCache::discover(!args.no_cache, args.no_ambient);
    if let Some(module) = cache.lookup(&args.file) {
        return run_cached(module);
    }
    let compiled = source::compile(
        &args.file,
        args.no_ambient,
        Some(&prefix),
        source::BundleAfter::Keep,
    )?;
    cache.store(compiled.lowered.module());
    compile_source(compiled)
}

/// The package path — a manifest-resolved graph or the bare script's
/// implicit `@std` graph — with `script` parsing the root's first member
/// as a script (top-level statements folding into the synthesized
/// `main`): the script-mode dsmod cache first, keyed on the root's
/// composed member identity beside the resolved graph's dep identities
/// and native-artifact pins — a servable cached artifact installs
/// without the front end — then the compile against the graph and the
/// install through the deps-first walk, caching what a later run of
/// the same member set over the same resolution can serve: the dsmod
/// artifact and the checked prefix alike, both keyed on the root's
/// member fold, the graph's dep identities, and the artifact pins. The
/// artifact's link table records the graph's identities and both keys
/// fold exactly those plus the root's members, so a dep whose resolved
/// content changed, a repinned library, or a sibling member's edit is
/// another key: an artifact or prefix linked against old content never
/// serves a new resolution.
fn run_package(args: &RunArgs, graph: &package::Graph, script: bool) -> Result<PreparedRun> {
    let pins = graph.artifact_pins();
    let cache = RunCache::discover_linked(
        !args.no_cache,
        args.no_ambient,
        graph.identities(),
        &pins,
        script,
    );
    // the root's members enter the cache key the way the graph's dep
    // identities do: a manifest root composes every member of its
    // namespace (G3), and the composed fold re-keys on a sibling edit
    let members = package::root_members(&args.file)?;
    if let Some(module) = cache.lookup_root(&args.file, &members) {
        return run_cached_package(module, graph, &cache);
    }
    // the same dep identities, artifact pins, and session shape key the
    // prefix tier: a prefix linked against a dep's old content never
    // serves a new resolution
    let prefix = crate::dsi_cache::PrefixCache::discover_linked(
        !args.no_cache,
        args.no_ambient,
        graph.identities(),
        &pins,
        script,
    );
    let compiled = package::compile_root(&args.file, graph, script, Some(&prefix), Some(&cache))?;
    let name = compiled.name.clone();
    let functions = compiled.lowered.functions().to_vec();
    cache.store(compiled.lowered.module());
    let vm = install_package_root(
        compiled.lowered.into_module(),
        graph,
        &cache,
        Some(&cache),
        &name,
        functions.len(),
    )?;
    Ok(PreparedRun {
        vm,
        name,
        functions,
    })
}

/// Installs one package root — compiled or served from the script cache
/// — through the deps-first walk over the resolved graph: `@std` and
/// every linked package serving through the graph, dependencies before
/// their dependent, each recorded link validated against the identity
/// it pins. The walk's dep demands consult the dep-member cache first,
/// so a warm run installs the linked members' own artifacts without
/// compiling them; `dep_dsi` is the checked-prefix tier a cold dep
/// compile persists beside its dsmod (`Some` from the run paths, whose
/// probes fold the same shape; `None` from a caller whose fold keys no
/// prefix reader would serve).
fn install_package_root(
    module: Module,
    graph: &package::Graph,
    cache: &RunCache,
    dep_dsi: Option<&RunCache>,
    name: &str,
    function_count: usize,
) -> Result<Vm> {
    let mut registry = ModuleRegistry::new();
    provide_platform(&mut registry);
    // the graph's bindable artifacts install before the modules (H5): the
    // deps-first walk's pool validation resolves a bound extern's
    // constant against the bound tables, exactly like a platform's
    // served ops — a load that refused leaves the artifact uninstalled
    // and a module binding it answers the recorded reason below
    let refused_binds = graph.install_bound(&mut registry);
    let mut vm = Vm::new(registry);
    // a member that fails to compile refuses through the walk's provider
    // miss, so its failure rides the stash out of the provider closure
    let failed = std::cell::RefCell::new(None);
    let walk = vm.load(module, |dep, linked| {
        if let Some(served) = cache.lookup_dep(dep, linked) {
            return Some(served);
        }
        match graph.serve(dep, dep_dsi) {
            Ok(Some(served)) => {
                cache.store_dep(dep, linked, &served);
                Some(served)
            },
            Ok(None) => None,
            Err(error) => {
                *failed.borrow_mut() = Some(error);
                None
            },
        }
    });
    if let Some(error) = failed.into_inner() {
        return Err(error);
    }
    walk.map_err(|error| install_error_bound(name, function_count, &error, &refused_binds))?;
    Ok(vm)
}

/// The walk's install refusal with the refused binds beside it: an
/// unresolved callee naming an artifact whose load refused answers the
/// recorded bind reason (the artifact installed nothing, so its bound
/// extern's pool validation is the first refusal to name it); every
/// other error renders as ever.
fn install_error_bound(
    name: &str,
    function_count: usize,
    error: &VmLoadError,
    refused_binds: &[(String, String)],
) -> Error {
    if let VmLoadError::UnresolvedCallee { module, .. } = error
        && let Some((_, reason)) = refused_binds
            .iter()
            .find(|(artifact, _)| artifact == module)
    {
        return anyhow!(
            "the artifact `{module}` does not bind, so its bound externs refuse the run: {reason}"
        );
    }
    install_error(name, function_count, error)
}

/// The package hit path: install the served artifact through the same
/// deps-first walk as the compile path — its link table pins the
/// identities the key folded.
fn run_cached_package(
    module: Module,
    graph: &package::Graph,
    cache: &RunCache,
) -> Result<PreparedRun> {
    let name = module.name().to_owned();
    let functions: Vec<String> = module
        .functions()
        .iter()
        .map(|function| function.name().to_owned())
        .collect();
    let vm = install_package_root(module, graph, cache, Some(cache), &name, functions.len())?;
    Ok(PreparedRun {
        vm,
        name,
        functions,
    })
}

/// The hit path: install the cached module.
fn run_cached(module: Module) -> Result<PreparedRun> {
    install_module(module)
}

/// Installs one compiled source.
fn compile_source(compiled: source::CompiledSource) -> Result<PreparedRun> {
    let name = compiled.name.clone();
    let functions = compiled.lowered.functions().to_vec();
    let mut registry = ModuleRegistry::new();
    provide_platform(&mut registry);
    let mut vm = Vm::new(registry);
    vm.install_dsmod(compiled.lowered.into_module())
        .map_err(|error| install_error(&name, functions.len(), &error))?;
    Ok(PreparedRun {
        vm,
        name,
        functions,
    })
}

/// Resolves the entry, runs it, and answers the run's outcome — with
/// the host's shutdown event observed, the exit code the host's own
/// per the die split: Unix answers
/// 128+signum, whatever the drive's last outcome was, the event
/// having cut it or raced it.
fn resolve_and_run(
    prepared: &mut PreparedRun,
    entry: &str,
    signals: &ShutdownWatch,
) -> Result<ExitCode> {
    let outcome = resolve_and_drive(prepared, entry);
    // the host's shutdown event: the
    // exit code is the host's own per the die split — Unix answers
    // 128+signum — whatever the drive's last outcome was, the event
    // having cut it or raced it
    if let Some(code) = signals.exit_code() {
        return Ok(code);
    }
    outcome
}

/// Resolves the entry, runs it, and reports the result: a platform
/// failure tag (io, net, parse) is a failed run — the lowered
/// unhandled-failure handler already printed the message — the `none`
/// member of a checked read prints by name, and everything else prints
/// like the dsmod path always did. The run answers the entry's whole
/// word span, so a multi-word answer (a tuple, a widened span) prints
/// one word per line instead of a truncated head. A trapped run reports
/// first — the backtrace reads the kept frames — and then releases the
/// interrupted root, so the kept frames' deinit hooks fire exactly once
/// and their claims drain before the run exits nonzero. The host owns
/// the exit code (the CLI process surface): a clean completion exits 0,
/// a trap or an unhandled failure 1, an `exit(code)` call ends the run
/// with its deliberate status, no report printed, and the shutdown
/// trap carries no report at all — the host's door observed the event
/// and owns the status.
fn resolve_and_drive(prepared: &mut PreparedRun, entry: &str) -> Result<ExitCode> {
    // the error-trace switch (H7) is a VM debug switch the boundary
    // report reads: armed, every `?` error leg records the failing
    // frame's position into the fiber's private ring, and an
    // unhandled-failure run prints the chain beside the report
    prepared.vm.set_trace_errors(true);
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
    match prepared.vm.run(&prepared.name, entry, &[]) {
        Ok(answer) => match answer.as_slice() {
            // the failure tags and the checked read's `none` member ride
            // the answer's head word
            [Value::Tag(tag), ..] if is_io_class(*tag) => {
                print_error_trace(&prepared.vm);
                Err(anyhow!(
                    "the run halted on an unhandled io failure (the message is on stdout)"
                ))
            },
            [Value::Tag(tag), ..] if is_net_class(*tag) => {
                print_error_trace(&prepared.vm);
                Err(anyhow!(
                    "the run halted on an unhandled net failure (the message is on stdout)"
                ))
            },
            [Value::Tag(tag), ..] if *tag == PLATFORM_PARSE_ERR => {
                print_error_trace(&prepared.vm);
                Err(anyhow!(
                    "the run halted on an unhandled parse failure (the message is on stdout)"
                ))
            },
            [Value::Tag(tag), ..] if *tag == PLATFORM_UTF8_ERR => {
                print_error_trace(&prepared.vm);
                Err(anyhow!(
                    "the run halted on an unhandled utf8 failure (the message is on stdout)"
                ))
            },
            // The `none` member of a checked read is a plain result: print
            // its name, not the platform reservation's raw id.
            [Value::Tag(tag), ..] if *tag == PLATFORM_NONE => {
                println!("none");
                Ok(ExitCode::SUCCESS)
            },
            _ => {
                for value in answer.as_slice() {
                    print_value(value);
                }
                Ok(ExitCode::SUCCESS)
            },
        },
        Err(Trap::Exit { code }) => {
            // the deliberate status: no report, the exit member's own
            // contract — the interrupted root still releases so the
            // kept frames' deinit hooks fire and their claims drain
            prepared.vm.release_interrupted(FIBER_ROOT);
            Ok(ExitCode::from(u8::try_from(code).unwrap_or(u8::MAX)))
        },
        Err(Trap::Shutdown) => {
            // the host's shutdown cut the drive: every parked fiber died
            // with the sentinel and its deinit hooks already ran, so no
            // report stands and the interrupted-root release is the
            // idempotent no-op — the status is the host's own (the
            // resolve_and_run override answers it), never the engine's
            prepared.vm.release_interrupted(FIBER_ROOT);
            Ok(ExitCode::FAILURE)
        },
        Err(trap) => {
            let report = trap_error(&prepared.vm, &trap);
            // the trapped root keeps its frames for the report; the
            // release fires their deinit hooks once and drains their
            // claims before the run exits nonzero
            prepared.vm.release_interrupted(FIBER_ROOT);
            Err(report)
        },
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

/// Why a structurally valid module still cannot be installed: the module
/// itself is consumed by the install, so its name and function count are
/// read beforehand and passed in.
fn install_error(name: &str, function_count: usize, error: &VmLoadError) -> Error {
    match error {
        VmLoadError::UnresolvedCallee {
            func,
            module: callee,
            index,
        } => {
            if callee.is_empty() || callee == name {
                anyhow!(
                    "function `{func}` references function index {index}, past module `{name}`'s \
                     own table of {function_count} functions"
                )
            } else {
                anyhow!(
                    "module `{callee}` is not installed (function `{func}` calls its function \
                     {index}); install dependencies before their dependents"
                )
            }
        },
        VmLoadError::UnprovidedPlatformOp {
            func,
            platform,
            capability,
            index,
        } => anyhow!(
            "function `{func}` calls world op {index} (`{capability}`), which the platform \
             `{platform}` does not provide; install a platform whose provides list covers it"
        ),
        VmLoadError::DepCycle { cycle } => anyhow!("dependency cycle: {}", cycle.join(" -> ")),
        VmLoadError::ProviderMiss { dep, dependent } => {
            anyhow!("no module for dependency `{dep}` of `{dependent}`: the provider has no answer")
        },
        VmLoadError::HashMismatch {
            dep,
            dependent,
            linked,
            found,
        } => match found {
            Some(found) => anyhow!(
                "dependency `{dep}` of `{dependent}` was linked at content hash {linked} but \
                 resolves to content {found}"
            ),
            None => anyhow!(
                "dependency `{dep}` of `{dependent}` was linked at content hash {linked}, but the \
                 installed version records no content hash to validate the link against"
            ),
        },
        VmLoadError::Build(error) => anyhow!(
            "module `{name}` fails the VM load checks: {}",
            describe_build_error(error)
        ),
        VmLoadError::StagedFnMisroute {
            module,
            index,
            func,
            site,
        } => match func {
            Some(func) => anyhow!(
                "swapping module `{module}` would misroute a staged callable word ({}) keyed to \
                 its function index {index}, which named `{func}` — the incoming table drops that \
                 function; the installed version keeps running",
                describe_staged_site(site)
            ),
            None => anyhow!(
                "swapping module `{module}` would misroute a staged callable word ({}) keyed to \
                 function index {index}, which no longer names any function in the retiring \
                 table; the installed version keeps running",
                describe_staged_site(site)
            ),
        },
        VmLoadError::PoolRefMisroute {
            module,
            pool,
            index,
            func,
        } => match func {
            Some(func) => anyhow!(
                "swapping module `{module}` would misroute a direct call in `{pool}` keyed to its \
                 function index {index}, which named `{func}` — the incoming table moves or drops \
                 that function; the installed version keeps running"
            ),
            None => anyhow!(
                "swapping module `{module}` would misroute a direct call in `{pool}` keyed to \
                 function index {index}, which no longer names any function in the installed \
                 table; the installed version keeps running"
            ),
        },
    }
}

/// One phrase naming where a refused swap's staged callable word waited.
fn describe_staged_site(site: &StagedSite) -> String {
    match site {
        StagedSite::ClosureHeader { fiber } => {
            format!("a closure header on fiber {fiber}'s heap")
        },
        StagedSite::ClosureCapture { fiber } => {
            format!("a closure capture on fiber {fiber}'s heap")
        },
        StagedSite::ListElement { fiber } => format!("a list element on fiber {fiber}'s heap"),
        StagedSite::ArenaElement { fiber } => {
            format!("an arena element on fiber {fiber}'s heap")
        },
        StagedSite::MapWord { fiber } => {
            format!("a map key or value on fiber {fiber}'s heap")
        },
        StagedSite::FrameSlot { fiber } => format!("a frame slot of fiber {fiber}"),
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
        ModuleBuildError::ChainIndex { chain, index } => {
            format!("evidence chain {chain} indexes {index} outside its module's tables")
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

/// Prints the recorded error chain (H7) to stderr beside the boundary's
/// unhandled-error report: one line per `?` error leg the entry fiber
/// took, innermost first, each named through the module's span table.
/// A chain the ring never recorded (no `?` on the path, or a stripped
/// module's unresolvable positions) prints nothing.
pub(crate) fn print_error_trace(vm: &Vm) {
    let frames = vm.error_trace();
    if frames.is_empty() {
        return;
    }
    eprintln!("error trace (innermost first):");
    for frame in frames {
        match &frame.location {
            Some(loc) => {
                eprintln!(
                    "  at {}:{}:{} in {}::{}",
                    loc.file, loc.line, loc.col, frame.module, frame.func
                );
            },
            None => {
                eprintln!("  at pc {} in {}::{}", frame.pc, frame.module, frame.func);
            },
        }
    }
}

/// Prints the run result to stdout. Unit-result entries halt on the inert
/// unit word (`I64(0)`), so nothing distinguishes them; a heap reference
/// prints as its object-table index (the object kind is a VM concern).
pub(crate) fn print_value(value: &Value) {
    match value {
        Value::I64(v) => println!("{v}"),
        Value::F64(v) => println!("{v:?}"),
        Value::Bool(v) => println!("{v}"),
        Value::Tag(v) => println!("{v}"),
        Value::Ref(v) => println!("<object {}>", v.id()),
        // the arena word never reaches a print: the placement proof
        // keeps it inside its own expression; keep the arm honest
        Value::Arena(_) => println!("<arena>"),
        Value::Fn(f) => println!("<fn {}:{}>", f.module(), f.func()),
    }
}

/// Formats a trap plus the backtrace summary the VM kept for it.
pub(crate) fn trap_error(vm: &Vm, trap: &Trap) -> Error {
    use std::fmt::Write;
    // writing into a String cannot fail
    let mut message = format!("trap: {}", describe_trap(trap));
    let frames = vm.backtrace();
    if !frames.is_empty() {
        message.push_str("\nbacktrace (innermost first):");
        for frame in &frames {
            let _ = write!(message, "\n  {}::{} ", frame.module, frame.func);
            match frame.pc {
                // the position the module's pc-to-span table names: file,
                // line, column — the one source channel the renderer
                // reads (a stripped module renders the raw pc)
                FramePc::At(pc) => match &frame.location {
                    Some(loc) => {
                        let _ = write!(message, "at {}:{}:{}", loc.file, loc.line, loc.col);
                    },
                    None => {
                        let _ = write!(message, "at pc {}", pc.0);
                    },
                },
                // a caller parked inside a hosted native call names no
                // bytecode position: the seam, not a sentinel integer
                FramePc::Hosted => {
                    let _ = write!(message, "at hosted");
                },
            }
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
        Trap::OversizedShift => "oversized shift".to_owned(),
        Trap::CvtOutOfRange => "float-to-int conversion out of range".to_owned(),
        // the die row of the CLI's halt policy:
        // the message prints through the trap report and the run exits
        // failed, exactly the io|net|utf8 halt rows' contract
        Trap::Die { message } => format!("die: {message}"),
        // the deliberate status (the CLI process surface): the run path
        // intercepts this trap ahead of the report; a host that does
        // not (the dev loop, the REPL) renders the note here
        Trap::Exit { code } => format!("exit: {code}"),
        Trap::UnknownFunction { module, func } => {
            format!("module `{module}` has no function `{func}`")
        },
        Trap::UnknownCallee { module, func } => {
            format!("call into module `{module}` function index {func}, which is not installed")
        },
        Trap::ReplState { entry } => match entry {
            ReplEntry::Start => "a REPL session is already live: a parked session resumes, it \
                                 never starts"
                .to_owned(),
            ReplEntry::Resume => "no live REPL session stands to resume".to_owned(),
            ReplEntry::DropBinding => "no live REPL session holds the binding".to_owned(),
        },
        Trap::UnservedWorldOp { op } => {
            format!("world op {op} is not served by the installed platform")
        },
        Trap::BoundMarshaling { symbol, reason } => {
            format!("the bound extern `{symbol}` refuses at the C-ABI marshaling seam: {reason}")
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
        Trap::ElementStride { obj, stride, given } => {
            format!("collection object {obj} holds {stride}-word elements, the call staged {given}")
        },
        Trap::CaptureIndex { obj, idx } => {
            format!("capture store addresses index {idx} past closure object {obj}'s block")
        },
        Trap::ForeignMapKey { obj, key } => format!(
            "map object {obj} refuses a fresh row keyed by foreign reference {key:?}: the key \
             must live on the map's own heap for any content probe to find it"
        ),
        Trap::ListIndex { obj, index, len } => format!(
            "list object {obj} holds {len} elements; index {index} names none — a subscript read, \
             store, or removal must resolve its place, it carries no miss answer"
        ),
        Trap::ArenaExhausted { field } => match field {
            ArenaPackError::Length => {
                "a string concatenation or arena slice hit the 16 MiB length field limit".to_owned()
            },
            ArenaPackError::Offset => {
                "an arena slice's chunk offset hit the 16 MiB field limit".to_owned()
            },
            ArenaPackError::Chunk => "an arena's chunk index hit the 64 Ki field limit".to_owned(),
        },
        Trap::ArenaHandoff { slot } => {
            format!("an arena temporary reached a count-bearing site at slot {slot}")
        },
        Trap::DeadEvidenceChain { slot } => format!(
            "the dead evidence sentinel at slot {slot}: the entering branch staged no chain for a \
             demand its narrowing can never reach"
        ),
        Trap::ArenaHandle { class } => match class {
            ArenaMisuse::Chunk => {
                "an arena handle names a chunk its arena never grew to".to_owned()
            },
            ArenaMisuse::Range => {
                "an arena handle or rewind target names memory a restore already freed".to_owned()
            },
        },
        Trap::Utf8Content => "string content reached a text operation as invalid UTF-8".to_owned(),
        Trap::OutputWrite { kind } => format!("output write failed: {kind:?}"),
        Trap::MetaWordCorrupt { fp } => {
            format!("frame metadata word corrupt at stack index {fp}")
        },
        Trap::ReentrancyRefused => "a native container call's lambda ended its fiber (HALT, fiber \
                                    switch, or yield under nesting) instead of returning"
            .to_owned(),
        Trap::NurseryCancelled { .. } => "a fiber cancelled its nursery and never resumed; the \
                                          payload unions out at the boundary, or the trap escaped \
                                          one"
        .to_owned(),
        Trap::NurseryUncontained { .. } => {
            "a nursery child failed with an error outside the boundary's return union".to_owned()
        },
        Trap::TailForkJoin => "a fork-join combinator (par_map, par_reduce) at a tail dispatch \
                               site; the join parks through the drive, which needs the CALLG site"
            .to_owned(),
        Trap::OutOfFuel => "a budgeted drive ran past its step limit (the comptime fence's cap; a \
                            run drive carries none)"
            .to_owned(),
        Trap::Shutdown => "the host's shutdown event cut the drive (the engine decides nothing; \
                           the host owns the status)"
            .to_owned(),
        Trap::ChanCapacity { cap } => {
            format!("chan_new refuses a negative bound: {cap}")
        },
        Trap::DanglingArenaHandle { index, generation } => {
            format!(
                "an arena handle names slot {index} at generation {generation}, which the arena \
                 never held or already retired (a handle copy taken before the removal)"
            )
        },
        Trap::SchedulerDeadlock => "the IO scheduler found nothing runnable and nothing able to \
                                    wake (every fiber parked on a wait no reactor interest, no \
                                    deadline, and no live fiber answers — a channel every fiber \
                                    waits on is the honest deadlock)"
            .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use ds_vm::{ReplEntry, Trap};

    use super::describe_trap;

    /// The REPL session-state refusals render the state they name, never
    /// an unknown-callee trap over a virtual module.
    #[test]
    fn repl_state_refusals_render_their_own_state() {
        assert_eq!(
            describe_trap(&Trap::ReplState {
                entry: ReplEntry::Start
            }),
            "a REPL session is already live: a parked session resumes, it never starts"
        );
        assert_eq!(
            describe_trap(&Trap::ReplState {
                entry: ReplEntry::Resume
            }),
            "no live REPL session stands to resume"
        );
        assert_eq!(
            describe_trap(&Trap::ReplState {
                entry: ReplEntry::DropBinding
            }),
            "no live REPL session holds the binding"
        );
    }
}
