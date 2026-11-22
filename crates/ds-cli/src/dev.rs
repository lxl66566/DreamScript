//! The `ds dev` subcommand: the hot-reload dev loop [H6 item 5].
//!
//! One .ds source compiles through one query database and drives on one
//! VM; a poller watches the file, and every changed round recompiles
//! through the memoized queries (block-grain renewal keeps the untouched
//! `##` blocks' facts valid), installs the new module version into the
//! two-slot table, and re-enters the entry fiber. A parked frame keeps
//! executing its own version's code — the swap redirects its next
//! fully-qualified call through the current slot — and the install-time
//! sweep retires a version's row once no live frame or parked fiber
//! names it.
//!
//! The entry suspends to the host where the host chooses: the drive
//! arms the VM's suspension request before every entry, so a
//! loop-driving entry parks at each taken backward branch — no
//! language spelling exists — and a parked frame re-enters on EVERY
//! poll round, so the poll interval is also the parked entry's tick
//! rate — a loop-driving entry keeps running while the host watches —
//! and a changed round swaps the module before the tick, so the
//! frame's next fully-qualified call lands in the new version. A drive
//! busy past one poll interval (a compute-bound stretch, a blocking
//! world call, a hosted native loop) observes a reload at its next
//! park; a never-looping entry observes it only at completion, where
//! the next changed round restarts it. A refused round (diagnostics,
//! a lowering, staging, or install refusal) keeps the installed
//! version: the entry re-enters and the old code keeps running.
//!
//! A `ds.pkg` beside the target turns the loop over a package root: the
//! watched bytes compose every member below the root (the entry among
//! them) and the manifest, every changed round re-resolving the manifest
//! graph and recompiling the root through the same compile `ds run`
//! drives — the root's whole member set composing one namespace (G3), so
//! a sibling member's edit reloads like the entry's own — the swap
//! installing through the deps-first load walk — an
//! already-installed dep validates at its recorded identity and skips,
//! so only the root's slot moves. A registry-pinned dep's members stay
//! outside the watch — the store serves content-addressed bytes, so a
//! pinned dep's source edit reaches the session only as `ds save` plus
//! a repin (resolution serves the pinned identity whatever else the
//! store holds) — while a directory-shaped dep entry (`[deps.<name>]`
//! with `path`) joins the watch directly: the resolved path dep
//! directories watch with the package tree's own discipline (recursive,
//! dynamically grown), their members and manifests compose into the
//! watched bytes, and a dep source edit re-resolves from source and
//! hot-swaps the moved identity with no repin. Either way a moved dep
//! identity recomputes the graph
//! honestly: every resolved name the session's registry already holds
//! at another identity hot-swaps under its own name BEFORE the walk
//! loads the root — the same two-slot semantics the root's own rounds
//! ride, so the swap re-checks refuse a staged word or a live call
//! site whose named identity the incoming table moves or drops, and a
//! frame still running the retiring version keeps it until its next
//! fully-qualified call — after which the walk validates the moved
//! link against the swapped slot and only the root's slot moves. A
//! dep name the registry never held installs through the walk as
//! ever. One module's dep list linking one name at two hashes stays
//! refused (the walk's link-table rule; same-name multi-hash
//! coexistence stays refused per G3.5). `--args` carries the verbatim
//! process arguments the program reads through `args()`, staged once
//! on the session VM, so a restart reads exactly what a fresh run of
//! the same version would.
//!
//! The watch reads the watched bytes against the last loaded ones at
//! every tick (`--poll-ms`, default 100ms): on Linux an inotify edge
//! signal gates the read — a window that saw no event reads nothing —
//! while the tick itself keeps the round cadence (the parked entry's
//! drive rate and the merge window that folds a multi-file edit into
//! one round), and every other platform (or a refused watcher) keeps
//! the poll twin reading each window; changes land when the next
//! window's read sees different bytes either way. `--hot` off runs the
//! one round and exits; on, the loop ends only when the process is
//! killed (Ctrl-C during a running computation kills it; the VM has no
//! step budget to observe an interrupt at).

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use clap::Args;
use ds_diag::{Diagnostic, FileId, render};
use ds_lower::lower_with_comptime;
use ds_mod::Hash128;
use ds_query::{
    Analyzed, BlockIndex, Db, DefId, LinearityCheck, PackageEffects, PackageId, PackageLinearity,
    ParseFile, PlatformCheck, Resolve, TypeOf,
};
use ds_vm::{
    FiberId, FiberState, FrameAnswer, Value, Vm,
    native::{PLATFORM_NONE, PLATFORM_PARSE_ERR, PLATFORM_UTF8_ERR, is_io_class, is_net_class},
};

use crate::{package, repl, run, source, watch::FsWatch};

/// The `ds dev` surface: one .ds source, one entry, the watch loop.
#[derive(Debug, Args)]
pub struct DevArgs {
    /// Path to the .ds source file
    file: PathBuf,
    /// Entry function to drive
    #[arg(long, default_value = "main")]
    entry: String,
    /// Watch the source and hot-swap reloaded versions; without it the
    /// entry runs once and the command exits
    #[arg(long)]
    hot: bool,
    /// Compile without the stdlib ambient package member
    #[arg(long)]
    no_ambient: bool,
    /// The watch poll interval, in milliseconds
    #[arg(long, default_value_t = 100)]
    poll_ms: u64,
    /// String arguments the dev-served program reads through `args()`
    /// (the CLI process surface), staged verbatim on the session VM
    #[arg(long, num_args = 0.., value_name = "STRING")]
    args: Vec<String>,
}

/// What the loop watches and how a changed round recompiles: a bare
/// script keeps its query database over the ambient plus the watched
/// file, a package root keeps its manifest path and recompiles through
/// the run path's package compile.
enum Watch {
    /// The query database and the watched file's id inside it.
    Script { db: Box<Db>, file: FileId },
    /// A package root: the manifest beside the watched entry member,
    /// re-resolved from the target's directory every changed round.
    Package,
}

/// The dev loop: the watch target, the persistent VM, and the entry
/// fiber's state. One session spans every reload round; the module
/// installs over its own retiring version under the same name, so the
/// two-slot swap redirects calls and the version table owns each
/// round's survival.
struct DevSession {
    watch: Watch,
    /// The watched target's path.
    path: PathBuf,
    /// The last watched bytes a round loaded: the poll gate, so an
    /// unchanged watch costs one read and a refused round reports once
    /// per distinct content — the target alone for a script, every
    /// member plus the manifest composed for a package root. `None`
    /// until the first load — an empty source is a load too, not
    /// "never loaded".
    loaded: Option<String>,
    /// A change the reconcile read found that no round has loaded yet
    /// (an edit landing inside the signal-creation gap, its inotify
    /// event spent before any watch existed): the next loop window
    /// consumes it through the ordinary round — never an extra
    /// out-of-cadence drive, which would advance a parked frame a tick
    /// the merge window owes the edit.
    pending: Option<String>,
    name: String,
    entry: String,
    no_ambient: bool,
    vm: Vm,
    fiber: Option<FiberId>,
    /// Every directory a resolved path dep read from (the transitive
    /// closure, ascending) — the watch roots the package rounds grow
    /// beside the package tree, and the watched-bytes composition folds
    /// so a dep source edit drives the same reload round an entry edit
    /// does. Empty until the first package round resolves.
    path_dirs: Vec<PathBuf>,
    /// Bumped whenever a round moved the path dep directory set: the
    /// loop's cue to rebuild the signal and re-baseline the watch gate.
    dir_epoch: u64,
    /// Whether the last round's reload installed a version.
    clean: bool,
    /// Whether the last drive ended trapped or on an unhandled world
    /// failure — the single-shot mode's exit code.
    failed: bool,
}

impl DevSession {
    /// The session over one watched target: a script keeps its query
    /// database seeded with the first text (the ambient registered
    /// first), a package root keeps no database — its rounds re-resolve
    /// the manifest beside the target — and both drive on a fresh VM
    /// with the default platform.
    fn new(args: &DevArgs, name: &str, package_root: bool, initial: &str) -> Self {
        let watch = if package_root {
            Watch::Package
        } else {
            let mut db = if args.no_ambient {
                Db::new()
            } else {
                Db::with_declared_rows(source::session_rows().clone())
                    .with_world_caps(ds_stdlib::world_caps().clone())
            };
            if !args.no_ambient {
                let _ambient = db.add_file(ds_stdlib::AMBIENT_FILE, ds_stdlib::AMBIENT_SOURCE);
                // the die provision rides the dev session beside the
                // ambient (the 2026-11-17 split)
                let _host = db.add_file(source::HOST_FILE, source::HOST_SOURCE);
            }
            let file = db.add_file(args.file.to_string_lossy().into_owned(), initial);
            Watch::Script {
                db: Box::new(db),
                file,
            }
        };
        let mut vm = repl::fresh_vm();
        // the process arguments stage once: every round's installed
        // version reads the same `args()` answer
        vm.set_argv(args.args.clone());
        Self {
            watch,
            path: args.file.clone(),
            loaded: None,
            pending: None,
            name: name.to_owned(),
            entry: args.entry.clone(),
            no_ambient: args.no_ambient,
            vm,
            fiber: None,
            path_dirs: Vec::new(),
            dir_epoch: 0,
            clean: false,
            failed: false,
        }
    }

    /// One poll round: a changed watch recompiles and installs (or
    /// reports the refusal and keeps the installed version), and the
    /// entry always re-enters — a parked frame continues on its own
    /// version's code with its next fully-qualified call resolving
    /// through the swapped slot, and a completed or interrupted frame
    /// restarts fresh on the current version once a clean round exists
    /// to restart on. The poll interval is therefore also the parked
    /// entry's tick rate: the program advances one park per round
    /// whether or not the watch changed, so a loop-driving entry keeps
    /// running while the host watches. A package root's watch composes
    /// every member below the root plus the resolved path deps' members
    /// and manifests, so a sibling or dep-source edit is a change.
    fn round(&mut self, watched: Option<&str>) -> Result<()> {
        let mut installed = false;
        if let Some(watched) = watched.filter(|watched| self.loaded.as_deref() != Some(watched)) {
            self.loaded = Some(watched.to_owned());
            match self.reload(watched)? {
                Some(()) => {
                    self.clean = true;
                    installed = true;
                },
                None => self.clean = false,
            }
        }
        match self.fiber {
            Some(fiber)
                if matches!(
                    self.vm.fiber_state(fiber),
                    Some(FiberState::Ready | FiberState::Suspended | FiberState::Preempted)
                ) =>
            {
                self.drive(fiber);
            },
            // a completed or interrupted entry restarts exactly when this
            // round installed a fresh version — never on the idle ticks
            _ if installed => {
                if let Some(fiber) = self.spawn_entry() {
                    self.fiber = Some(fiber);
                    self.drive(fiber);
                }
            },
            _ => {},
        }
        Ok(())
    }

    /// One changed round's recompile over whichever target the loop
    /// watches, answering `Some(())` when it installs and `None` when it
    /// refuses. The borrows stay on disjoint fields: the watch's
    /// database on one hand, the VM on the other.
    fn reload(&mut self, watched: &str) -> Result<Option<()>> {
        match &mut self.watch {
            Watch::Script { db, file } => {
                Self::reload_script(db, *file, watched, &self.name, &mut self.vm)
            },
            Watch::Package => {
                match Self::reload_package(&self.path, self.no_ambient, &mut self.vm) {
                    Some(dirs) => {
                        if dirs != self.path_dirs {
                            self.path_dirs = dirs;
                            self.dir_epoch += 1;
                        }
                        Ok(Some(()))
                    },
                    None => Ok(None),
                }
            },
        }
    }

    /// One changed script round: recompiles through the memoized
    /// queries and installs the lowered module over its own retiring
    /// version. Answers `None` when the round refuses — diagnostics
    /// printed, the installed version untouched.
    fn reload_script(
        db: &mut Db,
        file: FileId,
        text: &str,
        name: &str,
        vm: &mut Vm,
    ) -> Result<Option<()>> {
        db.set_file_content(file, text)?;
        let reports = Self::check(db, file)?;
        if !reports.is_empty() {
            Self::report(db, &reports);
            return Ok(None);
        }
        let analyzed = repl::query(db.get::<Analyzed>(PackageId::new(0)), "analyzed")?.analyzed;
        let effects = repl::query(
            db.get::<PackageEffects>(PackageId::new(0)),
            "package effects",
        )?
        .effects;
        // The round's gate already paid the affine walk (the check pass
        // reads it per block), so lowering consumes its rows instead of
        // the backstop re-walking the whole package — the same facts seam
        // the run path drives.
        let linear = repl::query(
            db.get::<PackageLinearity>(PackageId::new(0)),
            "package linearity",
        )?
        .rows();
        let hash = Hash128::compute(text.as_bytes());
        let comptime = match ds_meta::evaluate(name, hash, &analyzed.asts, &analyzed, &effects) {
            Ok(values) => values,
            Err(diags) => {
                Self::report(db, &diags);
                return Ok(None);
            },
        };
        let lowered = match lower_with_comptime(
            name,
            hash,
            &analyzed.asts,
            &analyzed,
            Some(&linear),
            &comptime,
        ) {
            Ok(lowered) => lowered,
            Err(error) => {
                let error = source::lower_error(db.files(), &error);
                eprintln!("dev: {error:#}");
                return Ok(None);
            },
        };
        if let Err(error) = vm.install_dsmod(lowered.into_module()) {
            eprintln!("dev: installing the reload refused: {error:?}");
            return Ok(None);
        }
        Ok(Some(()))
    }

    /// One changed package-root round: the manifest graph re-resolves
    /// and the root recompiles through the same compile `ds run`
    /// drives, the swap installing through the deps-first walk (an
    /// already-installed dep validates at its recorded identity and
    /// skips, so only the root's slot moves). A dep whose resolved
    /// identity moved since the session installed it hot-swaps under
    /// its own name before the walk — the same two-slot swap semantics
    /// the root rides, the re-checks refusing a staged word or a live
    /// call site whose named identity the incoming table moves or
    /// drops — so the walk validates the moved link against the
    /// swapped slot; a dep that finished swapping stays swapped when a
    /// later step refuses, exactly the walk's own partial-install rule.
    /// The compile is cold every round, the package run's own rule.
    /// Answers `Some` with the graph's resolved path dep directories
    /// (empty for a pin-only graph) when the round installs, and `None`
    /// when it refuses — reported, the installed version untouched.
    fn reload_package(path: &Path, no_ambient: bool, vm: &mut Vm) -> Option<Vec<PathBuf>> {
        let graph = match package::open(path, no_ambient) {
            Ok(Some(graph)) => graph,
            Ok(None) => {
                eprintln!(
                    "dev: the manifest beside {} is gone; still watching",
                    path.display()
                );
                return None;
            },
            Err(error) => {
                eprintln!("dev: {error:#}");
                return None;
            },
        };
        // the round's graph binds its artifacts into the session's VM
        // (H5) ahead of the compile and the walk: the same install the
        // run path performs, so a bound extern dev-sessions like it runs.
        // A load that refused leaves the artifact uninstalled and a
        // binding module answers the standard install refusal per round
        let (modules, _) = graph.bound_modules();
        for (name, module) in modules {
            vm.install_bound(&name, module);
        }
        let compiled = match package::compile_root(path, &graph, false, None, None) {
            Ok(compiled) => compiled,
            Err(error) => {
                eprintln!("dev: {error:#}");
                return None;
            },
        };
        // A dep whose resolved identity moved since the session installed
        // it (a manifest edit pinning new content) hot-swaps under its own
        // name before the walk loads the root: the two-slot swap runs the
        // same re-checks the root's own rounds run — a staged word or a
        // live call site whose named identity the incoming table moves or
        // drops refuses, the installed versions keep running — and a
        // frame still on the retiring version keeps it until its next
        // fully-qualified call. A name the registry never held (or holds
        // at this identity already) skips: the walk below installs or
        // validates it as ever, so only a moved identity swaps here.
        for (name, identity) in graph.identities() {
            let moved = match vm.installed_content_hash(name) {
                Some(recorded) => recorded != *identity,
                None => false,
            };
            if !moved {
                continue;
            }
            let module = match graph.serve(name, None) {
                Ok(Some(module)) => module,
                // identities hold resolved names, so a miss here names a
                // graph the walk would refuse anyway
                Ok(None) => {
                    eprintln!("dev: nothing serves the moved dep `{name}`");
                    return None;
                },
                Err(error) => {
                    eprintln!("dev: {error:#}");
                    return None;
                },
            };
            if let Err(error) = vm.install_dsmod(module) {
                eprintln!("dev: swapping the moved dep `{name}` refused: {error:?}");
                return None;
            }
        }
        // a member that fails to compile refuses through the walk's
        // provider miss, so its failure rides the stash out of the
        // provider closure
        let failed = RefCell::new(None);
        let walk = vm.load(compiled.lowered.into_module(), |dep, _| {
            match graph.serve(dep, None) {
                Ok(module) => module,
                Err(error) => {
                    *failed.borrow_mut() = Some(error);
                    None
                },
            }
        });
        if let Some(error) = failed.into_inner() {
            eprintln!("dev: {error:#}");
            return None;
        }
        if let Err(error) = walk {
            eprintln!("dev: installing the reload refused: {error:?}");
            return None;
        }
        Some(graph.path_dirs().to_vec())
    }

    /// The memoized query pass over the watched file (the same surface
    /// `ds check` drives), reporting only what addresses it: parse,
    /// resolve, the per-block typed and linearity stories, the
    /// platform check, and the pooled-body refusal.
    fn check(db: &mut Db, file: FileId) -> Result<Vec<Diagnostic>> {
        let mut reports = Vec::new();
        let parse = repl::query(db.get::<ParseFile>(file), "parse")?;
        reports.extend(parse.diagnostics.iter().cloned());
        let resolved = repl::query(db.get::<Resolve>(PackageId::new(0)), "resolve")?;
        reports.extend(
            resolved
                .diags
                .iter()
                .filter(|diag| repl::touches(diag, file))
                .cloned(),
        );
        let index = repl::query(db.get::<BlockIndex>(file), "block index")?;
        for block in index.blocks.iter() {
            let Ok(def) = DefId::from_block(block.id) else {
                reports.push(repl::block_key_overflow(block));
                continue;
            };
            let typed = repl::query(db.get::<TypeOf>(def), "type_of")?;
            reports.extend(
                typed
                    .diags
                    .iter()
                    .filter(|diag| repl::touches(diag, file))
                    .cloned(),
            );
            let linear = repl::query(db.get::<LinearityCheck>(def), "linearity_check")?;
            reports.extend(
                linear
                    .diags
                    .iter()
                    .filter(|diag| repl::touches(diag, file))
                    .cloned(),
            );
        }
        let platform = repl::query(db.get::<PlatformCheck>(PackageId::new(0)), "platform_check")?;
        reports.extend(
            platform
                .diags
                .iter()
                .filter(|diag| repl::touches(diag, file))
                .cloned(),
        );
        let analyzed = repl::query(db.get::<Analyzed>(PackageId::new(0)), "analyzed")?;
        reports.extend(
            ds_check::check_pooled_bodies(&analyzed.analyzed, db.declared_rows())
                .into_iter()
                .filter(|diag| repl::touches(diag, file)),
        );
        reports.sort_by_key(repl::focus);
        Ok(reports)
    }

    fn report(db: &Db, reports: &[Diagnostic]) {
        for diag in reports {
            eprintln!("{}", render::human(diag, db.files()));
        }
    }

    /// Spawns the entry fiber: resolves through the current slot, so a
    /// restart runs the version this round installed. The entry binds no
    /// parameters — the program reads `args()` off the process surface.
    fn spawn_entry(&mut self) -> Option<FiberId> {
        match self.vm.spawn_fiber(&self.name, &self.entry, &[]) {
            Ok(fiber) => Some(fiber),
            Err(trap) => {
                eprintln!("dev: {}", run::trap_error(&self.vm, &trap));
                self.failed = true;
                None
            },
        }
    }

    /// Drives the fiber to its next host-observable suspension: the drive
    /// arms the VM's suspension request first, so a loop-driving entry
    /// parks at its next taken backward branch and the host-resumed
    /// continuation behaves exactly as a plain run's would. A completed
    /// entry reports its answer; a trapped drive reports and then releases
    /// the fiber — the report's backtrace reads the kept frames first,
    /// and the release fires their deinit hooks once, drains the claims,
    /// and returns the pool slot, so nothing of the dead drive pins its
    /// version's row past the two-slot bound.
    fn drive(&mut self, fiber: FiberId) {
        self.vm.request_suspend(fiber);
        match self.vm.resume_fiber(fiber, Value::Tag(PLATFORM_NONE)) {
            Ok(answer) => {
                if self.vm.fiber_state(fiber) == Some(FiberState::Dead) {
                    self.report_answer(&answer);
                }
            },
            Err(trap) => {
                eprintln!("dev: {}", run::trap_error(&self.vm, &trap));
                self.failed = true;
                // the trapped entry releases here: the release refuses a
                // fiber still resumable (a child trapped while this entry
                // parked — the next poll keeps driving it), so only the
                // entry the drive ended on trapped goes
                if self.vm.release_interrupted(fiber) {
                    self.fiber = None;
                }
            },
        }
    }

    /// Reports a completed entry's answer on stdout exactly as `ds run`
    /// would (the failure tags and the checked read's `none` member ride
    /// the span's head word, the rest prints per word); an unhandled
    /// world failure is a dev note on stderr (the loop keeps watching).
    fn report_answer(&mut self, answer: &FrameAnswer) {
        match answer.as_slice() {
            [Value::Tag(tag), ..] if is_io_class(*tag) => {
                eprintln!("dev: the entry halted on an unhandled io failure");
                self.failed = true;
            },
            [Value::Tag(tag), ..] if is_net_class(*tag) => {
                eprintln!("dev: the entry halted on an unhandled net failure");
                self.failed = true;
            },
            [Value::Tag(tag), ..] if *tag == PLATFORM_PARSE_ERR => {
                eprintln!("dev: the entry halted on an unhandled parse failure");
                self.failed = true;
            },
            [Value::Tag(tag), ..] if *tag == PLATFORM_UTF8_ERR => {
                eprintln!("dev: the entry halted on an unhandled utf8 failure");
                self.failed = true;
            },
            [Value::Tag(tag), ..] if *tag == PLATFORM_NONE => println!("none"),
            words => {
                for value in words {
                    run::print_value(value);
                }
            },
        }
    }
}

/// The watched bytes' first read: the target alone for a script, every
/// member below the root plus the manifest for a package root — the
/// poll gate compares the composed string, so any member edit or a
/// manifest edit is one change.
fn read_watched(path: &Path, manifest: Option<&Path>) -> Result<String> {
    let Some(manifest) = manifest else {
        return std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()));
    };
    let mut parts = Vec::new();
    for member in package::walk_sources(&watch_root(manifest))? {
        parts.push(
            std::fs::read_to_string(&member)
                .with_context(|| format!("reading {}", member.display()))?,
        );
    }
    parts.push(
        std::fs::read_to_string(manifest)
            .with_context(|| format!("reading {}", manifest.display()))?,
    );
    Ok(parts.join("\0"))
}

/// One poll's read of the watched bytes: the package root's members and
/// manifest plus, per resolved path dep directory, its members and
/// manifest — the whole set the gate compares, so a dep source edit is
/// one change exactly like an entry edit. An editor's atomic replace
/// can leave a transient miss, so a read failure reports and answers
/// `None` — the previous version keeps running and the next poll
/// retries.
fn poll_watched(path: &Path, manifest: Option<&Path>, dirs: &[PathBuf]) -> Option<String> {
    let Some(manifest) = manifest else {
        return std::fs::read_to_string(path).ok();
    };
    let mut parts = Vec::new();
    let members = match package::walk_sources(&watch_root(manifest)) {
        Ok(members) => members,
        Err(error) => {
            eprintln!(
                "dev: reading {} failed ({error}); still watching",
                watch_root(manifest).display()
            );
            return None;
        },
    };
    for member in members {
        match std::fs::read_to_string(&member) {
            Ok(text) => parts.push(text),
            Err(error) => {
                eprintln!(
                    "dev: reading {} failed ({error}); still watching",
                    member.display()
                );
                return None;
            },
        }
    }
    match std::fs::read_to_string(manifest) {
        Ok(text) => parts.push(text),
        Err(error) => {
            eprintln!(
                "dev: reading {} failed ({error}); still watching",
                manifest.display()
            );
            return None;
        },
    }
    for dir in dirs {
        let members = match package::walk_sources(dir) {
            Ok(members) => members,
            Err(error) => {
                eprintln!(
                    "dev: reading {} failed ({error}); still watching",
                    dir.display()
                );
                return None;
            },
        };
        for member in members {
            match std::fs::read_to_string(&member) {
                Ok(text) => parts.push(text),
                Err(error) => {
                    eprintln!(
                        "dev: reading {} failed ({error}); still watching",
                        member.display()
                    );
                    return None;
                },
            }
        }
        match std::fs::read_to_string(dir.join(package::MANIFEST_FILE)) {
            Ok(text) => parts.push(text),
            Err(error) => {
                eprintln!(
                    "dev: reading {} failed ({error}); still watching",
                    dir.display()
                );
                return None;
            },
        }
    }
    Some(parts.join("\0"))
}

/// The package root a manifest sits in: the manifest's own directory
/// (`.` when it sits in the working directory), the tree the member
/// watch walks.
fn watch_root(manifest: &Path) -> PathBuf {
    manifest
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// Runs the dev loop: the first round compiles and drives, then `--hot`
/// polls the watched bytes and reloads every changed round; without it
/// the one round answers the exit code. A `ds.pkg` beside the target
/// turns the loop over the package root it names.
pub fn dev(args: &DevArgs) -> Result<ExitCode> {
    let path = &args.file;
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("ds") => {},
        _ => {
            return Err(anyhow!(
                "`ds dev` drives .ds sources; `{}` is not one",
                path.display()
            ));
        },
    }
    let manifest = package::manifest_beside(path);
    let name = source::script_module_name(path)?;
    let initial = read_watched(path, manifest.as_deref())?;
    let poll = Duration::from_millis(args.poll_ms);
    let mut session = DevSession::new(args, &name, manifest.is_some(), &initial);
    session.round(Some(&initial))?;
    if !args.hot {
        let code =
            if session.clean && !session.failed { ExitCode::SUCCESS } else { ExitCode::FAILURE };
        return Ok(code);
    }
    // the signal over the whole watch set — the package tree plus every
    // resolved path dep directory (the first round's resolution grew the
    // set), or the script file alone — then one reconcile read: the read
    // lands after the watch exists, and a difference from the loaded
    // bytes rides the pending slot into the next window's ordinary round
    // (an edit landing inside the gap would else hide inside a blindly
    // re-baselined gate, its event already spent and its bytes never
    // compiled), an agreement costing nothing
    let mut signal = make_signal(&session, manifest.as_deref(), path);
    reconcile(&mut session, manifest.as_deref(), path);
    loop {
        // the signal gates only the read: the tick keeps driving the
        // parked entry whether or not anything changed, and a pending
        // reconcile change loads through the same round cadence
        let watched = session.pending.take().or_else(|| {
            if signal.wait(poll) {
                poll_watched(path, manifest.as_deref(), &session.path_dirs)
            } else {
                None
            }
        });
        let epoch = session.dir_epoch;
        session.round(watched.as_deref())?;
        // a round that moved the path dep set (a manifest edit declaring
        // or undeclaring a path dep) rebuilds the signal over the new
        // roots and re-baselines the gate, the same ordering the startup
        // read keeps
        if session.dir_epoch != epoch {
            signal = make_signal(&session, manifest.as_deref(), path);
            reconcile(&mut session, manifest.as_deref(), path);
        }
    }
}

/// The signal over the session's whole watch set: the script file's
/// parent directory (the file's name the filter) for a script target,
/// the package tree plus every resolved path dep directory for a
/// package root — one watch set, the tree growth discipline inside
/// each root.
fn make_signal(session: &DevSession, manifest: Option<&Path>, path: &Path) -> FsWatch {
    match manifest {
        Some(manifest) => {
            let mut roots = vec![watch_root(manifest)];
            roots.extend(session.path_dirs.iter().cloned());
            FsWatch::trees(&roots)
        },
        None => FsWatch::script(path),
    }
}

/// Reconciles the watch gate over the current watch set: one fresh read
/// whose difference from the loaded bytes rides the pending slot into
/// the next window's ordinary round. A blindly re-set baseline could
/// swallow an edit that landed after the round the gate last served
/// (its inotify event spent inside the signal-creation gap), and a
/// round run directly here would drive a parked frame a tick the merge
/// window owes the edit — the pending slot spends neither. A failed
/// read keeps the old gate — the next window retries.
fn reconcile(session: &mut DevSession, manifest: Option<&Path>, path: &Path) {
    if let Some(fresh) = poll_watched(path, manifest, &session.path_dirs)
        && session.loaded.as_deref() != Some(&fresh)
    {
        session.pending = Some(fresh);
    }
}
