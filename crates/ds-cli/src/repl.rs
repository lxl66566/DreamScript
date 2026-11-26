//! The `ds repl` subcommand: an interactive read-eval-print loop over the
//! in-process pipeline with a persistent session — the resumed frame
//! (H6 item 4).
//!
//! # Session model
//!
//! The session is one growing module. Declaration submissions (`fn`,
//! `struct`, ...) append verbatim as their own `##` blocks; statement and
//! expression submissions append verbatim as statements of one REPL-owned
//! `fn main` block — the only scope where a later line both sees an
//! earlier line's bindings and the block's tail returns the last
//! submission's value. An expression submission lowers as the initializer
//! of one synthesized answer binding (`const __repl_ans = (<it>)`)
//! followed by the binding's name as the tail: the wrapper keeps every
//! component stock, gives `main` a single exit (the answer's `RET`), and
//! forces the value-position lowering that no tail call can replace.
//!
//! Every submission re-runs the memoized queries over the whole session
//! text through one query database the session owns across submissions
//! (the check passes are package-grain: each submission re-derives them
//! once, the ambient prefix served from the process park, and the
//! per-block type and linearity stories stay one-bucket slices), then
//! lowers in the resumed-frame mode over the affine rows the gate's own
//! walk already produced — the lowerer's facts seam, so no submission
//! pays the lowerer's backstop re-walk of the package. `main`'s binders
//! pin, their words claimed to the frame's teardown, since a
//! session-live value must never die at a text-relative last use a later
//! submission may extend.
//!
//! # The resumed frame
//!
//! Execution inverts re-running: the freshly lowered module is spliced
//! (the answer staging `RET` would perform plus a `repl_park` control
//! call at the tail's exit boundary) and installed, and the VM keeps
//! `main`'s frame alive between submissions — prior statements ran once,
//! and only the new statement computes. The resume target is the previous
//! statement's value end in the CURRENT round's own lowering (the
//! lowerer's per-statement fact), so the discarded statement's reference
//! counts release at the next resume without its value ever recomputing;
//! a declaration round re-parks at the boundary, restaging the last
//! answer. A failing or trapping line never commits: the frame rolls
//! back to the parked extent — the partial run's own deinit-carrying
//! bindings fire their hooks exactly once on the rollback's drain
//! (effects that already ran stand; a value the frame still holds
//! through a parked binding survives untouched), then the partial run's
//! words zero and its acquired counts release against a pre-step ledger
//! reading. `:drop <name>`
//! fires one live binding's deinit exactly once and neuters its words
//! (the binding's line is renamed so later lines cannot resolve it, the
//! text-level death that keeps every later binding's placement stable;
//! a name other lines still reference refuses the drop — a referencing
//! line would poison every later submission's check); the session end
//! (`:quit`, end of input, `:reset`) is the scope exit: every surviving
//! deinit fires, then the claims drain.
//!
//! # Reading
//!
//! An interactive terminal reads through a line editor (in-memory
//! history: up/down recall prior submissions; Ctrl-C answers as an event,
//! so the line in progress is discarded and the prompt re-issues while
//! the session survives); a piped stdin reads plain lines — no editing
//! exists over a pipe. Ctrl-C while a submission computes still kills the
//! process: the VM has no step budget to observe an interrupt at.
//!
//! # The executor seam
//!
//! The session's VM installs the platform's reactor (the `ds run`
//! precedent), so a submission whose code parks on a world op — an
//! accept, a recv, a timed sleep — ends its drive in flight instead of
//! blocking the prompt: the session holds the submission, refuses new
//! submissions and `:drop` until it lands, and pumps the executor/wait
//! seam between its own reads, so the parked fiber wakes exactly as
//! under `ds run` while the prompt keeps serving the user. A host
//! without a reactor driver keeps the blocking natives, no submission
//! ever parks, and the pump is inert.

use std::{
    collections::HashMap,
    io::{self, BufRead, IsTerminal, Write},
    process::ExitCode,
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use clap::Args;
use ds_check::DefKind;
use ds_diag::{Diagnostic, FileId, Files, LabelStyle, Span, render};
use ds_lower::{LowerMode, lower_in_mode};
use ds_mod::Hash128;
use ds_parse::{Ast, ItemKind, LocalDecl, Pattern, StmtKind};
use ds_query::{
    Analyzed, BlockIndex, BlockInfo, Db, DefId, EffectsOf, LinearityCheck, PackageEffects,
    PackageId, PackageLinearity, ParseFile, PlatformCheck, PooledCheck, QueryError, Resolve,
    TypeOf,
};
use ds_vm::{
    FrameAnswer, LoadError, ModuleRegistry, Platform, ReplOutcome, SpliceError, Value, Vm,
    abi::CodePc, native, splice_park,
};

use crate::{run, source};

/// The session module's name (the reserved stems `builtin`/`std` are not
/// taken).
const MODULE: &str = "repl";

/// The session file's name in the diagnostic registry.
const SESSION_FILE: &str = "repl.ds";

/// The synthesized answer binding's stem: the wrapper that keeps `main`'s
/// tail a plain path (never a tail call) and `main`'s return the last
/// expression's own type.
const ANS: &str = "__repl_ans";

/// The `ds repl` surface: no options in the first slice.
#[derive(Debug, Args)]
pub struct ReplArgs {}

/// Runs the read-eval-print loop until `:quit` or end of input.
///
/// # Errors
///
/// Query-engine failures only (the engine's self-describing errors);
/// language-level errors print as diagnostics and never end the session.
pub fn repl(_args: &ReplArgs) -> Result<ExitCode> {
    let mut session = Session::new();
    println!(
        "DreamScript repl {} (:quit, :drop <name>, :reset)",
        env!("CARGO_PKG_VERSION")
    );
    if io::stdin().is_terminal() {
        let mut reader = EditorInput::new()?;
        drive(&mut session, &mut reader)
    } else {
        let stdin = io::stdin();
        let mut locked = stdin.lock();
        let mut reader = PipedInput {
            input: &mut locked as &mut dyn BufRead,
        };
        drive(&mut session, &mut reader)
    }
}

/// The read-dispatch loop over one read layer: submissions run, `:`
/// commands answer, end of input ends the session (the scope exit runs
/// the surviving deinits), an interrupt discards the line in progress and
/// re-issues the prompt. The read is the host's blocking wait, so the
/// pass pumps the executor/wait seam first: a submission parked on a
/// world op wakes between the host's reads — what already stands ready
/// or expired, never blocking the prompt — and its landing reports
/// ahead of the line the read just answered.
fn drive(session: &mut Session, reader: &mut dyn ReadsLines) -> Result<ExitCode> {
    loop {
        match read_submission(reader) {
            ReadOutcome::Eof => {
                session.vm.repl_teardown();
                return Ok(ExitCode::SUCCESS);
            },
            // Ctrl-C at the prompt: the submission in progress is already
            // discarded; a newline closes the aborted line and the prompt
            // re-issues. The session survives.
            ReadOutcome::Interrupted => println!(),
            ReadOutcome::Text(line) => {
                // the read blocked for the host's whole wait: take the
                // wake that landed under it before the line is judged
                session.pump()?;
                if line.trim().is_empty() {
                    continue;
                }
                if let Some(command) = line.strip_prefix(':') {
                    if !session.command(command)? {
                        session.vm.repl_teardown();
                        return Ok(ExitCode::SUCCESS);
                    }
                } else if session.in_flight.is_some() {
                    println!("the previous submission still waits; :reset discards the session");
                } else {
                    session.submit(&line)?;
                }
            },
        }
    }
}

/// One live session: the query database over the ambient plus the session
/// text, the submitted lines, the persistent VM, the last committed
/// round's placement facts (the `:drop` seam's word discovery), and the
/// in-flight submission parked on a world op (the executor seam's
/// deferred landing).
struct Session {
    db: Db,
    file: FileId,
    decls: Vec<String>,
    stmts: Vec<String>,
    vm: Vm,
    /// `main`'s binder spans to body slots, from the last committed
    /// round's lowering.
    binders: HashMap<Span, Vec<u8>>,
    /// The renamed-away drop counter: one fresh unspellable-by-accident
    /// stem per `:drop`.
    drops: usize,
    /// The submission parked on a world op, its text committed ahead (a
    /// clean check ran and its effects stand): the host pumps until it
    /// lands, and a pump-time failure rolls the text back to the
    /// snapshot.
    in_flight: Option<InFlight>,
}

/// The in-flight submission's deferred facts: the snapshot a pump-time
/// failure restores the text to, the resume position its rollback
/// re-parks at, the zone the landing report reads, and the round's
/// binder registrations the landing commits.
struct InFlight {
    /// The committed-round snapshot (`decls`, `stmts`).
    before: (Vec<String>, Vec<String>),
    /// The resume position a pump-time trap's rollback re-parks at.
    at: u32,
    /// The submission's zone, the landing report's grain.
    zone: Zone,
    /// The round's registered binder spans, the landing's commit.
    binders: HashMap<Span, Vec<u8>>,
}

impl Session {
    /// The growing module text: declaration blocks, then the REPL-owned
    /// `main` carrying every statement submission in order — the last
    /// expression submission wrapped as one answer binding whose name is
    /// the tail, so `main` returns its value (a binding submission is its
    /// own tail and returns unit).
    fn text(decls: &[String], stmts: &[String]) -> String {
        let mut text = String::new();
        for (index, decl) in decls.iter().enumerate() {
            if index > 0 {
                text.push_str("\n##\n");
            }
            text.push_str(decl);
        }
        if !decls.is_empty() {
            text.push_str("\n##\n");
        }
        text.push_str("fn main() {\n");
        if let Some((last, head)) = stmts.split_last() {
            for stmt in head {
                text.push_str(stmt);
                text.push('\n');
            }
            if is_binding(last) {
                text.push_str(last);
                text.push('\n');
            } else {
                let ans = answer_name(decls, stmts);
                text.push_str("const ");
                text.push_str(&ans);
                text.push_str(" = (");
                text.push_str(last);
                text.push_str(")\n");
                text.push_str(&ans);
                text.push('\n');
            }
        }
        text.push_str("}\n");
        text
    }

    fn new() -> Self {
        let mut db = Db::with_declared_rows(source::session_rows().clone())
            .with_world_caps(ds_stdlib::world_caps().clone());
        // The ambient is a session package member (G3): its definitions
        // resolve for every submission, and its file id is never needed
        // again. The host provision rides beside it (the die split): the
        // entry session's own declaration member, rows included.
        let _ambient = db.add_file(ds_stdlib::AMBIENT_FILE, ds_stdlib::AMBIENT_SOURCE);
        let _host = db.add_file(source::HOST_FILE, source::HOST_SOURCE);
        let file = db.add_file(SESSION_FILE, &Self::text(&[], &[]));
        Self {
            db,
            file,
            decls: Vec::new(),
            stmts: Vec::new(),
            vm: fresh_vm(),
            binders: HashMap::new(),
            drops: 0,
            in_flight: None,
        }
    }

    /// The committed text, what the database should hold between
    /// submissions.
    fn committed(&self) -> String {
        Self::text(&self.decls, &self.stmts)
    }

    /// Handles one language submission: trial-append the line, run the
    /// memoized query pass over the whole session text, and on a clean
    /// pass drive the resumed frame. A submission with diagnostics, a
    /// lowering refusal, or a trap is not committed — a poisoned line
    /// would re-fail (or re-trap) every later round.
    fn submit(&mut self, line: &str) -> Result<()> {
        let before = (self.decls.clone(), self.stmts.clone());
        let zone = declaration_zone(line);
        match &zone {
            Zone::Named(_) | Zone::Nameless => self.decls.push(line.to_owned()),
            Zone::Statement => self.stmts.push(line.to_owned()),
        }
        let trial = self.committed();
        self.db.set_file_content(self.file, &trial)?;
        if !self.check_clean()? {
            return self.rollback(&before);
        }
        match self.execute(&trial, zone.clone()) {
            Ok(SubmitEnd::Landed) => Ok(()),
            // the submission parked on a world op: its text stays
            // committed and the host pumps between its reads until the
            // drive lands
            Ok(SubmitEnd::Waiting { at, binders }) => {
                self.in_flight = Some(InFlight {
                    before,
                    at,
                    zone,
                    binders,
                });
                Ok(())
            },
            Err(Failure::Engine(error)) => Err(error),
            Err(_) => self.rollback(&before),
        }
    }

    /// Pumps the executor/wait seam between the host's blocking reads: a
    /// world-parked submission wakes here and lands exactly a drive-time
    /// landing does — its answer reporting, its binders committing — and
    /// a pump-time trap rolls the frame and the text back exactly a
    /// drive-time trap would. The zero budget takes only what already
    /// stands ready or expired, never blocking the prompt.
    fn pump(&mut self) -> Result<()> {
        let Some(flight) = self.in_flight.take() else {
            return Ok(());
        };
        match self.vm.repl_continue(Some(Duration::ZERO)) {
            Ok(ReplOutcome::Parked(answer)) => {
                self.binders = flight.binders;
                let session = query(self.db.get::<ParseFile>(self.file), "parse")?;
                // the committed text checked clean: the tree exists
                let Some(session_ast) = session.ast.as_deref() else {
                    return Err(anyhow!("the session parse lost its tree"));
                };
                let analyzed =
                    query(self.db.get::<Analyzed>(PackageId::new(0)), "analyzed")?.analyzed;
                self.report(session_ast, &analyzed, &answer, flight.zone)
            },
            Ok(ReplOutcome::WorldParked) => {
                // the wait still stands: the next pass pumps again
                self.in_flight = Some(flight);
                Ok(())
            },
            Ok(ReplOutcome::Died(_)) => {
                // the spliced body has no live exit path, so a completion
                // is a should-not-happen: the frame is gone, the text the
                // snapshot restores, and the session rebuilds from the
                // next submission's fresh start
                self.vm.repl_teardown();
                self.vm = fresh_vm();
                self.rollback(&flight.before)?;
                println!("the entry frame completed instead of parking; the session rebuilt");
                Ok(())
            },
            Err(trap) => {
                // the report reads the interrupted fiber's kept frames
                print!("{}", run::trap_error(&self.vm, &trap));
                if let Err(rebuild) = self.vm.repl_rollback(MODULE, "main", CodePc(flight.at)) {
                    self.vm.repl_teardown();
                    self.vm = fresh_vm();
                    print!("{}", run::trap_error(&self.vm, &rebuild));
                }
                self.rollback(&flight.before)?;
                Ok(())
            },
        }
    }

    /// Removes the just-tried submission from the session and restores
    /// the database; the VM's own state was restored at the failure site.
    fn rollback(&mut self, before: &(Vec<String>, Vec<String>)) -> Result<()> {
        self.decls.clone_from(&before.0);
        self.stmts.clone_from(&before.1);
        let committed = self.committed();
        self.db.set_file_content(self.file, &committed)?;
        Ok(())
    }

    /// The memoized query pass over the session text (the same surface
    /// `ds check` drives): parse, resolve, the per-block typed and
    /// linearity stories, and the platform check, printing every
    /// diagnostic that addresses the session file. Clean answers true.
    fn check_clean(&mut self) -> Result<bool> {
        let mut reports = Vec::new();
        let parse = query(self.db.get::<ParseFile>(self.file), "parse")?;
        reports.extend(parse.diagnostics.iter().cloned());
        let resolved = query(self.db.get::<Resolve>(PackageId::new(0)), "resolve")?;
        reports.extend(
            resolved
                .diags
                .iter()
                .filter(|diag| touches(diag, self.file))
                .cloned(),
        );
        let index = query(self.db.get::<BlockIndex>(self.file), "block index")?;
        for block in index.blocks.iter() {
            let Ok(def) = DefId::from_block(block.id) else {
                reports.push(block_key_overflow(block));
                continue;
            };
            let typed = query(self.db.get::<TypeOf>(def), "type_of")?;
            reports.extend(typed.diags.iter().cloned());
            let linear = query(self.db.get::<LinearityCheck>(def), "linearity_check")?;
            reports.extend(linear.diags.iter().cloned());
        }
        let platform = query(
            self.db.get::<PlatformCheck>(PackageId::new(0)),
            "platform_check",
        )?;
        reports.extend(
            platform
                .diags
                .iter()
                .filter(|diag| touches(diag, self.file))
                .cloned(),
        );
        // The pooled-body refusal [E3] rides its query like every other
        // pass: combinator element bodies refuse the members a pooled
        // drive cannot serve.
        let pooled = query(
            self.db.get::<PooledCheck>(PackageId::new(0)),
            "pooled_check",
        )?;
        reports.extend(
            pooled
                .diags
                .iter()
                .filter(|diag| touches(diag, self.file))
                .cloned(),
        );
        if reports.is_empty() {
            return Ok(true);
        }
        reports.sort_by_key(focus);
        for diag in &reports {
            print!("{}", render::human(diag, self.db.files()));
        }
        Ok(false)
    }

    /// Compiles the clean session through the analyzed query (the
    /// analyzer's check half over the expansion and resolve memos,
    /// lowered over the bundle's own expanded trees, in the resumed-frame
    /// mode), splices the park into `main`, installs the module over its
    /// own retiring version, and drives the frame: prior statements never
    /// re-run, only the new statement computes, and the parked frame
    /// carries the session's live bindings to the next round. A trap
    /// rolls the frame back to the parked extent before this answers.
    fn execute(&mut self, text: &str, zone: Zone) -> std::result::Result<SubmitEnd, Failure> {
        let analyzed = query(self.db.get::<Analyzed>(PackageId::new(0)), "analyzed")
            .map_err(Failure::Engine)?
            .analyzed;
        // The gate already paid the affine walk (the check pass reads it
        // per block), so the lowering consumes its rows instead of the
        // backstop re-walking the whole package — the same facts seam the
        // run path and `ds dev`'s reload drive. The read is a memo hit
        // over the same revision; `rows()` clones the lists the lowerer
        // plans over, still far under one extra walk of the ambient.
        let linear = query(
            self.db.get::<PackageLinearity>(PackageId::new(0)),
            "package linearity",
        )
        .map_err(Failure::Engine)?
        .rows();
        let lowered = lower_in_mode(
            MODULE,
            Hash128::compute(text.as_bytes()),
            &analyzed.asts,
            &analyzed,
            Some(&linear),
            LowerMode::ResumedFrame,
        )
        .map_err(|error| Failure::lower(&error, self.db.files()))?;
        let Some(ends) = lowered.value_ends("main").map(<[u32]>::to_vec) else {
            return Err(Failure::Engine(anyhow!(
                "the session main lowered no statement value ends"
            )));
        };
        let Some(boundary) = ends.last().copied() else {
            return Err(Failure::Engine(anyhow!(
                "the session main lowered no statement value ends"
            )));
        };
        let binders = lowered.registered("main").cloned().unwrap_or_default();
        let spliced =
            splice_park(lowered.into_module(), "main", boundary).map_err(Failure::splice)?;
        self.vm
            .install_dsmod(spliced)
            .map_err(|error| Failure::install(&error))?;
        // The resume begins the new submission's code at the previous
        // statement's value end — the record's entry `stmts.len() - 1`
        // (the leading body start stands for the zeroth statement, and
        // the wrapper is not a committed statement, so the entries up to
        // the new one are exactly the committed statements' value ends) —
        // where the discarded statement's counts release at the next
        // resume; a declaration round owns no statement, so it re-parks
        // at the boundary and the last answer restages.
        let at = if let Zone::Statement = zone {
            let at = (self.stmts.len() - 1).min(ends.len() - 1);
            ends[at]
        } else {
            boundary
        };
        let driven = if self.vm.repl_parked() {
            self.vm.repl_resume(MODULE, "main", CodePc(at))
        } else {
            self.vm.repl_start(MODULE, "main")
        };
        match driven {
            Ok(ReplOutcome::Parked(answer)) => {
                self.binders = binders;
                let session =
                    query(self.db.get::<ParseFile>(self.file), "parse").map_err(Failure::Engine)?;
                // a clean check implies the tree exists
                let Some(session_ast) = session.ast.as_deref() else {
                    return Err(Failure::Engine(anyhow!("the session parse lost its tree")));
                };
                self.report(session_ast, &analyzed, &answer, zone)
                    .map_err(Failure::Engine)?;
                Ok(SubmitEnd::Landed)
            },
            // the submission parked on a world op: the drive hands the
            // host its own waits back, and the pump between the reads
            // lands it — the facts the landing and a pump-time trap
            // need ride out
            Ok(ReplOutcome::WorldParked) => Ok(SubmitEnd::Waiting { at, binders }),
            Ok(ReplOutcome::Died(_)) => {
                // the spliced body has no live exit path, so a completion
                // is a should-not-happen: the frame is gone, and the
                // session rebuilds from the next submission's fresh start
                self.vm.repl_teardown();
                self.vm = fresh_vm();
                println!("the entry frame completed instead of parking; the session rebuilt");
                Err(Failure::Frame)
            },
            Err(trap) => {
                // the report reads the interrupted fiber's kept frames
                print!("{}", run::trap_error(&self.vm, &trap));
                if let Err(rebuild) = self.vm.repl_rollback(MODULE, "main", CodePc(at)) {
                    self.vm.repl_teardown();
                    self.vm = fresh_vm();
                    print!("{}", run::trap_error(&self.vm, &rebuild));
                    return Err(Failure::Frame);
                }
                Err(Failure::Trap)
            },
        }
    }

    /// Prints the submission's story: the value of an expression tail
    /// (the unit word is inert, so a unit-typed tail prints no value),
    /// the inferred type, and the effect row — the answer expression's
    /// own row from the effects pass's per-span table (an expression
    /// submission's answer is the wrapper's initializer, a binding
    /// submission's the binding line's initializer), falling back to the
    /// touched definition's row when no span covers it.
    fn report(
        &mut self,
        ast: &Ast,
        analyzed: &ds_check::Analyzed,
        span: &FrameAnswer,
        zone: Zone,
    ) -> Result<()> {
        let universe = &analyzed.decls.universe;
        match zone {
            Zone::Named(name) => {
                let Some(def) = analyzed.package.lookup(&name) else {
                    return Ok(());
                };
                match analyzed.package.def(def).kind {
                    DefKind::Fn | DefKind::Const | DefKind::Var => {
                        if let Some(&ty) = analyzed.checked.def_types.get(&def) {
                            println!("{name}: {}", universe.render(ty));
                            self.print_row(def.index())?;
                        }
                    },
                    _ => println!("{name} declared"),
                }
                Ok(())
            },
            Zone::Nameless => {
                println!("declared");
                Ok(())
            },
            Zone::Statement => {
                let Some(answer) = main_answer(ast) else {
                    return Ok(());
                };
                match answer {
                    Answer::Expr(_) => {
                        // The wrapper's initializer is the submission's
                        // expression: `main`'s solved return is its type
                        // (the universe spells the arity-0 tuple "()",
                        // the inert unit word).
                        if let Some(ret) = main_return(analyzed, universe) {
                            if ret != "()" {
                                print_result(span);
                                run::print_error_trace(&self.vm);
                            }
                            println!(": {ret}");
                        }
                    },
                    Answer::Bindings(local) => {
                        // A binding tail prints each binder's name and
                        // type; the frame's value is the unit word.
                        let mut names = Vec::new();
                        pattern_names(&local.pat, &mut names);
                        for ident in names {
                            if let Some(fact) = analyzed.checked.binders.get(&ident.span) {
                                println!("{}: {}", ident.name, universe.render(fact.ty));
                            }
                        }
                    },
                }
                // The answer's own row (the tightest recorded span
                // covering it — earlier statements' effects stay out);
                // the definition grain is the fallback.
                // the wrapper's initializer carries the submission's own
                // expression; paren layers are placement, not the answer
                let answer_expr = match answer {
                    Answer::Expr(expr) => Some(expr),
                    Answer::Bindings(local) => Some(&local.init),
                }
                .map(unwrap_parens);
                let effects = query(
                    self.db.get::<PackageEffects>(PackageId::new(0)),
                    "package effects",
                )?;
                let row = answer_expr
                    .and_then(|expr| effects.effects.covering_row(expr.span()))
                    .or_else(|| {
                        analyzed
                            .package
                            .lookup("main")
                            .and_then(|def| effects.effects.of(def))
                    });
                if let Some(row) = row {
                    println!("~ {}", row.render());
                }
                Ok(())
            },
        }
    }

    /// The effect row of one definition, read through the `effects_of`
    /// query (E1): `pure` or the vocabulary row.
    fn print_row(&mut self, index: u32) -> Result<()> {
        let row = query(self.db.get::<EffectsOf>(DefId::new(index)), "effects_of")?;
        println!("~ {}", row.render());
        Ok(())
    }

    /// Handles one `:` command; `:quit` answers false and ends the loop
    /// (the scope exit runs the surviving deinits first).
    fn command(&mut self, command: &str) -> Result<bool> {
        let mut words = command.split_whitespace();
        match words.next() {
            Some("quit") => Ok(false),
            Some("drop") => {
                if self.in_flight.is_some() {
                    // a world-parked submission holds the frame mid-drive;
                    // the drop's hook drive needs the frame parked at the
                    // seam
                    println!("the previous submission still waits; :drop needs the frame parked");
                } else if let Some(name) = words.next() {
                    self.drop_binding(name);
                } else {
                    println!("usage: :drop <name>");
                }
                Ok(true)
            },
            Some("reset") => {
                self.in_flight = None;
                self.vm.repl_teardown();
                self.decls.clear();
                self.stmts.clear();
                self.binders.clear();
                let empty = Self::text(&[], &[]);
                self.db.set_file_content(self.file, &empty)?;
                self.vm = fresh_vm();
                println!("session reset");
                Ok(true)
            },
            Some(other) => {
                println!("unknown command :{other} (commands: :quit, :drop <name>, :reset)");
                Ok(true)
            },
            None => {
                println!("commands: :quit, :drop <name>, :reset");
                Ok(true)
            },
        }
    }

    /// `:drop <name>`: the binding's death runs now — its deinit fires
    /// exactly once (when its type carries one), its counts release, its
    /// words neuter — and its line is renamed so later submissions cannot
    /// resolve the name (the text-level death; the renamed binder's slots
    /// stay reserved, keeping every later binding's placement stable).
    /// A declaration drop removes the block: no frame words exist for
    /// it, and the next round's own lowering decides whether the session
    /// still checks.
    fn drop_binding(&mut self, name: &str) {
        let Some(pos) = self.stmts.iter().position(|s| binds_name(s, name)) else {
            if let Some(at) = self.decls.iter().position(|d| declares_name(d, name)) {
                self.decls.remove(at);
                let committed = self.committed();
                if self.db.set_file_content(self.file, &committed).is_ok() {
                    println!("dropped {name}");
                }
            } else {
                println!("no binding {name} in the session");
            }
            return;
        };
        // A name other lines still reference cannot die textually: the
        // referencing line (live or already-dropped) would stop checking,
        // refusing every later submission. The drop names them and stays.
        let referencing: Vec<&str> = self
            .stmts
            .iter()
            .enumerate()
            .filter(|(at, line)| *at != pos && references_name(line, name))
            .map(|(_, line)| first_line(line))
            .chain(
                self.decls
                    .iter()
                    .filter(|decl| references_name(decl, name))
                    .map(|decl| first_line(decl)),
            )
            .collect();
        if let Some(line) = referencing.first() {
            println!("cannot drop {name}: `{line}` still references it");
            return;
        }
        // the binder's words: the span the last committed round placed it
        // at, when the registration owns them (an alias's death is its
        // owner's)
        let owned = self
            .binder_span(pos, name)
            .and_then(|span| self.binders.get(&span).cloned());
        match owned {
            // an empty registration is a scalar alias (no words, no
            // counts): the rename is the whole death
            Some(words) if !words.is_empty() => {
                if let Err(trap) = self.vm.repl_drop_binding(MODULE, "main", &words) {
                    println!("{}", run::trap_error(&self.vm, &trap));
                }
            },
            _ => println!("{name} holds no live words of its own (a constant or an alias)"),
        }
        self.drops += 1;
        let fresh = format!("__repl_dropped_{}", self.drops);
        self.stmts[pos] = rename_binding(&self.stmts[pos], &fresh);
        let committed = self.committed();
        if self.db.set_file_content(self.file, &committed).is_ok() {
            println!("dropped {name}");
        }
    }

    /// The span of the `pos`-th statement's binder named `name`, from the
    /// committed round's own tree.
    fn binder_span(&mut self, pos: usize, name: &str) -> Option<Span> {
        let parse = self.db.get::<ParseFile>(self.file).ok()?;
        let ast = parse.ast.as_deref()?;
        let block = main_block(ast)?;
        let stmt = block.stmts.get(pos)?;
        let StmtKind::Local(local) = &stmt.kind else {
            return None;
        };
        let mut names = Vec::new();
        pattern_names(&local.pat, &mut names);
        names
            .into_iter()
            .find(|ident| ident.name == name)
            .map(|i| i.span)
    }
}

/// Which zone a submission goes to: a declaration block (its declared
/// name, when it carries one — `impl`, `import`, and `platform` blocks
/// declare none the package names), or a statement of the REPL-owned
/// `main`.
#[derive(Clone)]
enum Zone {
    /// A declaration block defining `name`.
    Named(String),
    /// A declaration block the package does not name.
    Nameless,
    /// A statement of the REPL-owned `main`.
    Statement,
}

/// How a clean-checked submission's drive ended: parked at the splice
/// (committed and reported), or parked on a world op (in flight — the
/// host pumps until it lands).
enum SubmitEnd {
    /// The frame parked at the seam: the submission committed, its
    /// story reported.
    Landed,
    /// The submission parked on a world op mid-drive: the resume
    /// position a pump-time trap's rollback re-parks at, and the
    /// round's binder registrations the landing commits.
    Waiting {
        /// The resume position.
        at: u32,
        /// The round's registered binder spans.
        binders: HashMap<Span, Vec<u8>>,
    },
}

/// Why a clean-checked submission still did not run: every arm but
/// [`Failure::Engine`] prints at its own site and leaves the session
/// usable (the caller rolls the text back).
enum Failure {
    /// A query-engine failure; session-fatal.
    Engine(anyhow::Error),
    /// A lowering refusal, rendered like `ds run`'s at construction.
    Lower,
    /// An install failure after a clean check: the swap consumed the
    /// module, so the session rebuilt its frame.
    Install,
    /// A park splice refusal, reported at construction.
    Splice,
    /// A runtime trap: reported and rolled back at its own site.
    Trap,
    /// The frame rebuilt at its own site (a completion or a rollback
    /// refusal).
    Frame,
}

impl Failure {
    /// A lowering refusal: spanned constructs render as diagnostics, the
    /// package-level errors stay one line (the `ds run` rule).
    fn lower(error: &ds_lower::LowerError, files: &Files) -> Self {
        if let Some(span) = error.span() {
            let diagnostic =
                Diagnostic::error(ds_diag::ErrorCode::LowerUnsupported, error.to_string())
                    .with_primary(span, "outside the M3 lowering contract");
            print!("{}", render::human(&diagnostic, files));
            println!("the line does not lower under the M3 contract");
        } else {
            println!("{error}");
        }
        Self::Lower
    }

    /// A park splice refusal; a clean check makes this a
    /// should-not-happen, so one self-describing line is enough.
    fn splice(error: SpliceError) -> Self {
        println!("splicing the session park failed: {error}");
        Self::Splice
    }

    /// An install failure; a clean check makes this a should-not-happen,
    /// so one self-describing line is enough.
    fn install(error: &LoadError) -> Self {
        println!("installing the session module failed: {error:?}");
        Self::Install
    }
}

/// One query failure, self-describing with the pass that failed.
pub(crate) fn query<T>(result: Result<T, QueryError>, pass: &str) -> Result<T> {
    result.with_context(|| format!("{pass} query failed"))
}

/// A fresh VM with the default platform installed (H5.5): the CLI is the
/// provider, and the install validates pool FuncRefs against it. The
/// platform's reactor installs with it (the `ds run` precedent), so a
/// session's world op parks on a would-block instead of blocking the
/// drive; a host with no driver keeps the blocking natives — the
/// embedder's platform choice, never a language fact.
pub(crate) fn fresh_vm() -> Vm {
    let mut registry = ModuleRegistry::new();
    registry.install_platform(Platform::std());
    let mut vm = Vm::new(registry);
    // the interactive sessions' boundary reports read the recorded
    // error chain (H7): the switch is the VM's debug face, armed here
    // for the session's whole life
    vm.set_trace_errors(true);
    if let Some(reactor) = ds_vm::reactor::platform_default() {
        vm.install_reactor(reactor);
    }
    vm
}

/// One read at the prompt: a completed submission, end of input, or an
/// interrupt that discards the submission in progress (the session
/// continues).
enum ReadOutcome {
    /// A submission whose bracket depth closed.
    Text(String),
    /// End of input.
    Eof,
    /// Ctrl-C: the submission in progress is discarded.
    Interrupted,
}

/// One line event from the read layer: a line, end of input, or an
/// interrupt (Ctrl-C at the prompt — an event the session survives, not a
/// kill).
enum LineEvent {
    /// One read line, its trailing newline trimmed.
    Line(String),
    /// End of input.
    Eof,
    /// An interrupt discarded the line in progress.
    Interrupted,
}

/// The prompt-level read layer: one line per call. The interactive layer
/// is a line editor (up/down recall of prior submissions, Ctrl-C as an
/// event); the piped layer is a plain buffered read — no editing exists
/// over a pipe, and there is nothing to interrupt.
trait ReadsLines {
    /// Reads one line, rendering `prompt` first.
    fn read_line(&mut self, prompt: &str) -> Result<LineEvent>;
    /// Records one completed submission for recall.
    fn record(&mut self, submission: &str);
}

/// The piped read layer over any buffered input.
struct PipedInput<'a> {
    input: &'a mut dyn BufRead,
}

impl ReadsLines for PipedInput<'_> {
    fn read_line(&mut self, prompt: &str) -> Result<LineEvent> {
        print!("{prompt}");
        io::stdout().flush()?;
        let mut line = String::new();
        match self.input.read_line(&mut line) {
            Ok(0) => Ok(LineEvent::Eof),
            Ok(_) => Ok(LineEvent::Line(
                line.trim_end_matches(['\r', '\n']).to_owned(),
            )),
            // An interrupted read (a signal landing mid-read) discards its
            // partial line; the loop re-issues the prompt.
            Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(LineEvent::Interrupted),
            Err(error) => {
                println!("cannot read input: {error}");
                Ok(LineEvent::Eof)
            },
        }
    }

    fn record(&mut self, _submission: &str) {}
}

/// The interactive read layer: a line editor with one in-memory history
/// (rustyline over the terminal's raw keys on Unix, the console's key
/// events on Windows), so up/down recall prior submissions and Ctrl-C
/// answers `Interrupted` instead of killing the process.
struct EditorInput {
    editor: rustyline::DefaultEditor,
}

impl EditorInput {
    fn new() -> Result<Self> {
        Ok(Self {
            editor: rustyline::DefaultEditor::new().context("the line editor starts")?,
        })
    }
}

impl ReadsLines for EditorInput {
    fn read_line(&mut self, prompt: &str) -> Result<LineEvent> {
        match self.editor.readline(prompt) {
            Ok(line) => Ok(LineEvent::Line(line)),
            Err(rustyline::error::ReadlineError::Interrupted) => Ok(LineEvent::Interrupted),
            Err(rustyline::error::ReadlineError::Eof) => Ok(LineEvent::Eof),
            Err(error) => Err(anyhow!("the line editor failed: {error}")),
        }
    }

    fn record(&mut self, submission: &str) {
        // Recall is best-effort: a history store refusing one more entry
        // never fails the session.
        let _ = self.editor.add_history_entry(submission);
    }
}

/// Reads one submission: lines until the bracket depth closes, `>>> ` for
/// the first line and `... ` while open. End of input mid-submission
/// answers the partial text; an interrupt discards it. A completed
/// non-blank submission is recorded for recall.
fn read_submission(reader: &mut dyn ReadsLines) -> ReadOutcome {
    let mut text = String::new();
    loop {
        let prompt = if text.is_empty() { ">>> " } else { "... " };
        match reader.read_line(prompt) {
            Ok(LineEvent::Line(line)) => {
                text.push_str(&line);
                text.push('\n');
            },
            Ok(LineEvent::Eof) => {
                return if text.is_empty() {
                    ReadOutcome::Eof
                } else {
                    ReadOutcome::Text(text.trim_end_matches(['\r', '\n']).to_owned())
                };
            },
            Ok(LineEvent::Interrupted) => return ReadOutcome::Interrupted,
            Err(error) => {
                println!("{error:#}");
                return ReadOutcome::Eof;
            },
        }
        let trimmed = text.trim_end_matches(['\r', '\n']);
        if bracket_depth(trimmed) <= 0 {
            let submission = trimmed.to_owned();
            if !submission.trim().is_empty() {
                reader.record(&submission);
            }
            return ReadOutcome::Text(submission);
        }
    }
}

/// Open-bracket depth of one submission: `(`/`[`/`{` open, the matching
/// closers close, strings, chars, and `//` comments never count (a
/// paren-depth scan is the slice's continuation contract).
fn bracket_depth(text: &str) -> i32 {
    let mut depth = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            },
            '"' | '\'' => {
                let quote = c;
                while let Some(c) = chars.next() {
                    if c == '\\' {
                        chars.next();
                    } else if c == quote {
                        break;
                    }
                }
            },
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {},
        }
    }
    depth
}

/// Which zone a submission belongs to: the first word decides (a
/// `const`/`var` line is a statement of `main`, where later lines see
/// it).
fn declaration_zone(line: &str) -> Zone {
    const NAMED: [&str; 6] = ["fn", "struct", "enum", "tag", "type", "trait"];
    const NAMELESS: [&str; 3] = ["impl", "import", "platform"];
    let first = first_line(line);
    let mut words = first.split_whitespace();
    let Some(mut head) = words.next() else {
        return Zone::Statement;
    };
    // A leading attribute is not the declaration's head (a `#[derive(...)]`
    // token carries no declaration of its own): skip attribute tokens to
    // the first plain word.
    while head.starts_with('#') {
        match words.next() {
            Some(next) => head = next,
            None => return Zone::Statement,
        }
    }
    let head = match head {
        "pub" | "extern" => match words.next() {
            Some(next) => next,
            None => return Zone::Statement,
        },
        _ => head,
    };
    if let Some(name) = NAMED
        .contains(&head)
        .then(|| ident_after(first, head))
        .flatten()
    {
        return Zone::Named(name);
    }
    if NAMELESS.contains(&head) {
        return Zone::Nameless;
    }
    Zone::Statement
}

/// Whether a statement line is a simple `const`/`var` binding.
fn is_binding(line: &str) -> bool {
    matches!(
        first_line(line).split_whitespace().next(),
        Some("const" | "var")
    )
}

/// The name a statement line binds (`const x`/`var x`), when it is a
/// simple binding.
fn binds_name(line: &str, name: &str) -> bool {
    let first = first_line(line);
    let mut words = first.split_whitespace();
    let Some(head) = words.next() else {
        return false;
    };
    if head != "const" && head != "var" {
        return false;
    }
    match ident_after(first, head) {
        Some(bound) => bound == name,
        None => false,
    }
}

/// Whether one line references `name` as a whole word.
fn references_name(line: &str, name: &str) -> bool {
    let mut word = String::new();
    for c in line.chars().chain([' ']) {
        if c.is_alphanumeric() || c == '_' {
            word.push(c);
        } else {
            if word == name {
                return true;
            }
            word.clear();
        }
    }
    false
}

/// Whether a declaration block declares `name`.
fn declares_name(line: &str, name: &str) -> bool {
    matches!(declaration_zone(line), Zone::Named(declared) if declared == name)
}

/// The identifier token following `keyword` on the line, if the next
/// token is a plain identifier.
fn ident_after(line: &str, keyword: &str) -> Option<String> {
    let rest = line.split_once(keyword)?.1;
    let mut chars = rest.char_indices().skip_while(|(_, c)| c.is_whitespace());
    let (start, _) = chars.next()?;
    let mut end = rest.len();
    for (index, c) in rest[start..].char_indices() {
        if !(c.is_alphanumeric() || c == '_') {
            end = start + index;
            break;
        }
    }
    let ident = &rest[start..end];
    (!ident.is_empty() && ident.chars().next().is_some_and(char::is_alphabetic))
        .then(|| ident.to_owned())
}

/// The first non-empty, non-comment line of a submission.
fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("//"))
        .unwrap_or("")
}

/// The answer binding's name: the stem, suffixed until no session text
/// holds it (a user binding of the same name must not collide).
fn answer_name(decls: &[String], stmts: &[String]) -> String {
    let mut stem = ANS.to_owned();
    let mut n = 1;
    let holds = |name: &str| {
        decls
            .iter()
            .chain(stmts.iter())
            .any(|text| text.contains(name))
    };
    while holds(&stem) {
        n += 1;
        stem = format!("{ANS}_{n}");
    }
    stem
}

/// Rewrites one binding line's bound name to `fresh` (the `:drop`
/// neutering): only the identifier after the `const`/`var` head changes,
/// so the line's shape, placement, and every later line's offsets stay.
fn rename_binding(line: &str, fresh: &str) -> String {
    let Some((old, at)) = binding_name_at(line) else {
        return line.to_owned();
    };
    format!("{}{}{}", &line[..at], fresh, &line[at + old.len()..])
}

/// The bound identifier of a `const`/`var` binding submission and its
/// byte offset in the whole text: the first content line's head token
/// names the kind, the next identifier token after it is the bound name.
/// The offset is the token's own position — never a substring hit inside
/// the head keyword (an `n` bound is not the `n` of `const`) or an
/// earlier occurrence of the name on the line.
fn binding_name_at(line: &str) -> Option<(String, usize)> {
    let mut line_at = 0;
    for raw in line.split('\n') {
        let trimmed = raw.trim_start();
        let lead = raw.len() - trimmed.len();
        if !trimmed.is_empty() && !trimmed.starts_with("//") {
            let head = trimmed.split_whitespace().next()?;
            let after_head = line_at + lead + head.len();
            let rest = &line[after_head..];
            let mut chars = rest.char_indices().skip_while(|(_, c)| c.is_whitespace());
            let (start, _) = chars.next()?;
            let mut end = rest.len();
            for (index, c) in rest[start..].char_indices() {
                if !(c.is_alphanumeric() || c == '_') {
                    end = start + index;
                    break;
                }
            }
            let ident = &rest[start..end];
            return (!ident.is_empty() && ident.chars().next().is_some_and(char::is_alphabetic))
                .then(|| (ident.to_owned(), after_head + start));
        }
        line_at += raw.len() + 1;
    }
    None
}

/// The REPL-owned `main`'s body block.
fn main_block(ast: &Ast) -> Option<&ds_parse::BlockExpr> {
    let main = ast.blocks.iter().flat_map(|b| b.items.iter()).find(|item| {
        matches!(
            &item.kind,
            ItemKind::Local(local)
                if matches!(&local.pat, Pattern::Binding { name, .. } if name.name == "main")
        )
    });
    let ItemKind::Local(local) = &main?.kind else {
        return None;
    };
    let ds_parse::Expr::Lambda { body, .. } = &local.init else {
        return None;
    };
    let ds_parse::Expr::Block(block) = body.as_ref() else {
        return None;
    };
    Some(block)
}

/// The last statement of the REPL-owned `main`'s body, as the answer
/// surface sees it.
enum Answer<'a> {
    /// An expression submission: the synthesized wrapper binding's
    /// initializer.
    Expr(&'a ds_parse::Expr),
    /// A binding submission: the binding reports its binders.
    Bindings(&'a LocalDecl),
}

/// The last statement of `main`'s body as the answer surface sees it.
fn main_answer(ast: &Ast) -> Option<Answer<'_>> {
    let stmts = &main_block(ast)?.stmts;
    let stmt = stmts.last()?;
    match &stmt.kind {
        StmtKind::Expr(expr) => {
            if is_answer_path(expr) {
                // the wrapper binding's initializer, one statement below
                // the tail path
                match &stmts.get(stmts.len().checked_sub(2)?)?.kind {
                    StmtKind::Local(local) => Some(Answer::Expr(&local.init)),
                    StmtKind::Expr(expr) => Some(Answer::Expr(expr)),
                }
            } else {
                Some(Answer::Expr(expr))
            }
        },
        StmtKind::Local(local) => Some(Answer::Bindings(local)),
    }
}

/// Whether an expression is the session's synthesized answer path.
fn is_answer_path(expr: &ds_parse::Expr) -> bool {
    matches!(expr, ds_parse::Expr::Path { path, .. }
        if path.segments.len() == 1 && path.segments[0].name.starts_with(ANS))
}

/// The expression beneath any paren layers (the wrapper's placement, not
/// the answer itself).
fn unwrap_parens(expr: &ds_parse::Expr) -> &ds_parse::Expr {
    match expr {
        ds_parse::Expr::Paren { inner, .. } => unwrap_parens(inner),
        other => other,
    }
}

/// The rendered return type of the REPL-owned `main`, when its solved
/// type is the zero-arg function it is built to be.
fn main_return(analyzed: &ds_check::Analyzed, universe: &ds_check::Universe) -> Option<String> {
    let def = analyzed.package.lookup("main")?;
    let ty = *analyzed.checked.def_types.get(&def)?;
    let [atom] = universe.members(ty) else {
        return None;
    };
    let (_, ret) = universe.fn_sig(*atom)?;
    Some(universe.render(ret))
}

/// Every binding name of a pattern, in source order.
fn pattern_names(pattern: &Pattern, out: &mut Vec<ds_parse::Ident>) {
    match pattern {
        Pattern::Binding { name, .. } => out.push(name.clone()),
        Pattern::Call { args, .. } => {
            for arg in args {
                pattern_names(arg, out);
            }
        },
        Pattern::Struct { fields, .. } => {
            for (field, pat) in fields {
                match pat {
                    Some(pat) => pattern_names(pat, out),
                    // punning: the field name binds itself
                    None => out.push(field.clone()),
                }
            }
        },
        Pattern::Tuple { pats, .. } | Pattern::Or { pats, .. } => {
            for pat in pats {
                pattern_names(pat, out);
            }
        },
        _ => {},
    }
}

/// Whether any label of the diagnostic addresses the session file.
pub(crate) fn touches(diag: &Diagnostic, file: FileId) -> bool {
    diag.labels.iter().any(|label| label.span.file == file)
}

/// The spanned diagnostic for a block past the DefId block-key budget: a
/// block whose index does not fit the 8-bit packing cannot be keyed for
/// its typed and linearity slices, so the block skips with its own span
/// and the pass continues — never a bare engine escape that would swallow
/// every diagnostic already collected.
pub(crate) fn block_key_overflow(block: &BlockInfo) -> Diagnostic {
    Diagnostic::error(
        ds_diag::ErrorCode::DefIdBlockOverflow,
        format!(
            "block {} of file {} does not fit the DefId packing",
            block.id.index,
            block.id.file.as_u32()
        ),
    )
    .with_primary(
        block.span,
        "this block is past the 256-block per-file budget; its checks are skipped",
    )
}

/// Sort key of one diagnostic: the focus span (primary label, else
/// first), source position order like `ds check`.
pub(crate) fn focus(diag: &Diagnostic) -> (u32, u32, u32) {
    diag.labels
        .iter()
        .find(|label| label.style == LabelStyle::Primary)
        .or_else(|| diag.labels.first())
        .map_or((u32::MAX, u32::MAX, u32::MAX), |label| {
            (label.span.file.as_u32(), label.span.start, label.span.end)
        })
}

/// Prints one run result's whole answer span: the platform failure tags
/// are halted runs (the lowered unhandled-failure handler already printed
/// the message) and the `none` member of a checked read prints by name,
/// both riding the span's head word; everything else prints one line per
/// word exactly like `ds run`, so a multi-word answer never truncates to
/// its head word.
fn print_result(span: &FrameAnswer) {
    match span.as_slice() {
        [Value::Tag(tag), ..] if native::is_io_class(*tag) => {
            println!("halted on an unhandled io failure");
        },
        [Value::Tag(tag), ..] if native::is_net_class(*tag) => {
            println!("halted on an unhandled net failure");
        },
        [Value::Tag(tag), ..] if *tag == native::PLATFORM_PARSE_ERR => {
            println!("halted on an unhandled parse failure");
        },
        [Value::Tag(tag), ..] if *tag == native::PLATFORM_UTF8_ERR => {
            println!("halted on an unhandled utf8 failure");
        },
        [Value::Tag(tag), ..] if *tag == native::PLATFORM_NONE => println!("none"),
        words => {
            for value in words {
                run::print_value(value);
            }
        },
    }
}

#[cfg(test)]
mod read_tests {
    use anyhow::Result;
    use rustyline::history::History;

    use super::{EditorInput, LineEvent, ReadOutcome, ReadsLines, Session, drive, read_submission};

    /// A scripted read layer: line events played in order, recording the
    /// completed submissions it is asked to recall. The injected
    /// `Interrupted` event is the test stand-in for Ctrl-C at the prompt
    /// (a live console event is not portable to spawn from a test).
    struct Scripted {
        events: Vec<LineEvent>,
        recorded: Vec<String>,
    }

    impl ReadsLines for Scripted {
        fn read_line(&mut self, _prompt: &str) -> Result<LineEvent> {
            Ok(self.events.remove(0))
        }

        fn record(&mut self, submission: &str) {
            self.recorded.push(submission.to_owned());
        }
    }

    fn scripted(events: Vec<LineEvent>) -> Scripted {
        Scripted {
            events,
            recorded: Vec::new(),
        }
    }

    /// Multi-line submissions join until the depth closes and record once;
    /// blanks record nothing; an interrupt discards the submission in
    /// progress without recording it.
    #[test]
    fn submissions_join_record_once_and_interrupt_discards() {
        let mut reader = scripted(vec![
            LineEvent::Line("const x = (".into()),
            LineEvent::Line("1,".into()),
            LineEvent::Line("2)".into()),
        ]);
        let read = read_submission(&mut reader);
        assert!(matches!(read, ReadOutcome::Text(text) if text == "const x = (\n1,\n2)"));
        assert_eq!(reader.recorded, ["const x = (\n1,\n2)"]);

        let mut reader = scripted(vec![LineEvent::Line(String::new())]);
        assert!(matches!(read_submission(&mut reader), ReadOutcome::Text(t) if t.is_empty()));
        assert!(reader.recorded.is_empty(), "blank lines never recall");

        let mut reader = scripted(vec![
            LineEvent::Line("fn f() {".into()),
            LineEvent::Interrupted,
        ]);
        assert!(matches!(
            read_submission(&mut reader),
            ReadOutcome::Interrupted
        ));
        assert!(
            reader.recorded.is_empty(),
            "the interrupted partial never recalls"
        );
    }

    /// Ctrl-C at the prompt keeps the session: the injected interrupt
    /// discards the line in progress, the loop re-issues the prompt, and
    /// the next submission commits and runs.
    #[test]
    fn ctrl_c_at_the_prompt_keeps_the_session() {
        let mut reader = scripted(vec![
            LineEvent::Interrupted,
            LineEvent::Line("1 + 1".into()),
            LineEvent::Eof,
        ]);
        let mut session = Session::new();
        let code = drive(&mut session, &mut reader).expect("the session ends cleanly");
        assert_eq!(code, std::process::ExitCode::SUCCESS);
        assert_eq!(
            session.stmts,
            ["1 + 1"],
            "the post-interrupt line committed"
        );
        assert_eq!(reader.recorded, ["1 + 1"]);
    }

    /// The editor layer records what it is asked to recall into its own
    /// history store (the up/down recall itself is the editor's).
    #[test]
    fn the_editor_records_history_entries() {
        let mut editor = EditorInput::new().expect("the editor starts without a terminal");
        editor.record("1 + 1");
        assert_eq!(editor.editor.history_mut().len(), 1);
    }
}

#[cfg(test)]
mod rename_tests {
    use super::{binding_name_at, rename_binding};

    /// The rename lands on the binding-name token's own position: a name
    /// that is a substring of the `const`/`var` head, of a leading
    /// comment, or of the initializer never captures the rewrite, and a
    /// multi-line binding renames at its first content line.
    #[test]
    fn the_rename_lands_on_the_bound_name_token() {
        assert_eq!(binding_name_at("const n = 5"), Some(("n".to_owned(), 6)));
        assert_eq!(rename_binding("const n = 5", "__x"), "const __x = 5");
        assert_eq!(rename_binding("var a = a + 1", "__x"), "var __x = a + 1");
        assert_eq!(
            rename_binding("// n note\nconst n = 5", "__x"),
            "// n note\nconst __x = 5"
        );
        assert_eq!(
            rename_binding("const m = (\n1,\n2)", "__x"),
            "const __x = (\n1,\n2)"
        );
    }
}

#[cfg(test)]
mod query_riding_tests {
    use anyhow::Result;
    use ds_diag::FileId;
    use ds_lower::{LowerMode, lower_in_mode};
    use ds_mod::Hash128;
    use ds_query::{Analyzed, Db, PackageId, PackageLinearity, QueryKey};

    use super::{LineEvent, MODULE, ReadsLines, SESSION_FILE, Session, drive};

    /// The session-shaped database over the ambient, the host provision,
    /// and one session file: the exact registration [`Session::new`]
    /// performs, so the differential below lowers the same package the
    /// live REPL lowers.
    fn session_db(text: &str) -> (Db, FileId) {
        let mut db = Db::with_declared_rows(crate::source::session_rows().clone())
            .with_world_caps(ds_stdlib::world_caps().clone());
        let _ambient = db.add_file(ds_stdlib::AMBIENT_FILE, ds_stdlib::AMBIENT_SOURCE);
        let _host = db.add_file(crate::source::HOST_FILE, crate::source::HOST_SOURCE);
        let file = db.add_file(SESSION_FILE, text);
        (db, file)
    }

    /// The query-ridden lowering answers the backstop's bytes exactly:
    /// for every round of a growing session the module lowered over the
    /// `package_linearity` query's rows (the served walk's) equals the
    /// module the lowerer's own plain re-walk produces — module bytes,
    /// per-statement value ends, and the parked binder registrations —
    /// so riding the queries moves no observable answer.
    #[test]
    fn the_query_rows_lower_the_backstops_bytes_exactly() {
        let res =
            "struct res: ~Copyable {\n  id: int\n  deinit { println(\"dying ${self.id}\") }\n}";
        let point = "#[derive(show)]\nstruct point {\n  x: int\n}";
        let rounds: [(Vec<&str>, Vec<&str>); 4] = [
            (vec![res], vec!["const r = res { id: 7 }"]),
            (vec![res], vec![
                "const r = res { id: 7 }",
                "println(\"id ${r.id}\")",
            ]),
            (vec![res, point], vec![
                "const r = res { id: 7 }",
                "println(\"id ${r.id}\")",
                "const p = point { x: 1 }",
                "p",
            ]),
            (vec![res, point, "fn f(x: res) -> int { x.id * 2 }"], vec![
                "const r = res { id: 7 }",
                "println(\"id ${r.id}\")",
                "const p = point { x: 1 }",
                "p",
                "var v = res { id: 2 }",
                "v = res { id: 3 }",
                "f(r)",
            ]),
        ];
        let mut text = Session::text(&[], &[]);
        let (mut db, file) = session_db(&text);
        for (decls, stmts) in rounds {
            text = Session::text(
                &decls
                    .iter()
                    .map(|decl| (*decl).to_owned())
                    .collect::<Vec<_>>(),
                &stmts
                    .iter()
                    .map(|stmt| (*stmt).to_owned())
                    .collect::<Vec<_>>(),
            );
            db.set_file_content(file, &text).unwrap();
            let parse = db.get::<ds_query::ParseFile>(file).unwrap();
            assert!(
                parse.diagnostics.is_empty(),
                "round {decls:?}/{stmts:?} must parse: {:?}",
                parse.diagnostics
            );
            let analyzed = db.get::<Analyzed>(PackageId::new(0)).unwrap().analyzed;
            assert!(
                analyzed.checked.diags.is_empty(),
                "round {decls:?}/{stmts:?} must check: {:?}",
                analyzed.checked.diags
            );
            let rows = db
                .get::<PackageLinearity>(PackageId::new(0))
                .unwrap()
                .rows();
            let hash = Hash128::compute(text.as_bytes());
            let backstop = lower_in_mode(
                MODULE,
                hash,
                &analyzed.asts,
                &analyzed,
                None,
                LowerMode::ResumedFrame,
            )
            .unwrap();
            let queries = lower_in_mode(
                MODULE,
                hash,
                &analyzed.asts,
                &analyzed,
                Some(&rows),
                LowerMode::ResumedFrame,
            )
            .unwrap();
            assert_eq!(
                ds_mod::write(backstop.module()),
                ds_mod::write(queries.module()),
                "round {decls:?}/{stmts:?}: the served rows must lower the backstop's bytes"
            );
            assert_eq!(
                backstop.value_ends("main"),
                queries.value_ends("main"),
                "the per-statement value ends must agree"
            );
            assert_eq!(
                backstop.registered("main"),
                queries.registered("main"),
                "the parked binder registrations must agree"
            );
        }
    }

    /// A scripted read layer over fixed lines.
    struct Lines(Vec<LineEvent>);

    impl ReadsLines for Lines {
        fn read_line(&mut self, _prompt: &str) -> Result<LineEvent> {
            Ok(self.0.remove(0))
        }

        fn record(&mut self, _submission: &str) {}
    }

    /// The per-submission work the engine sees, counted: five clean
    /// submissions re-derive each package-grain pass exactly once (the
    /// necessary work — any edit can move any resolution, so the passes
    /// are package-grain by design) and parse the ambient and host
    /// exactly once for the whole session (the ambient's parse serves
    /// from the embedded blob and no submission touches it again). The
    /// one affine walk per submission serves both the gate's per-block
    /// stories and the lowering's facts seam: before the seam rode here,
    /// every submission's lowering re-walked the whole package — the
    /// ambient included — through the lowerer's plain backstop, a second
    /// full walk these counters could never see because it bypassed the
    /// engine entirely.
    #[test]
    fn five_submissions_recheck_each_pass_once() {
        let submissions = [
            "struct res: ~Copyable {\n  id: int\n  deinit { println(\"dying ${self.id}\") }\n}",
            "const r = res { id: 7 }",
            "r.id + 1",
            "fn f(x: int) -> int { x * 2 }",
            "f(3)",
        ];
        let mut events: Vec<LineEvent> = submissions
            .iter()
            .map(|line| LineEvent::Line((*line).to_owned()))
            .collect();
        events.push(LineEvent::Eof);
        let mut reader = Lines(events);
        let mut session = Session::new();
        let code = drive(&mut session, &mut reader).expect("the session ends cleanly");
        assert_eq!(code, std::process::ExitCode::SUCCESS);
        let db = &session.db;
        // file ids: the ambient (0), the host provision (1), the session (2)
        let session_file = FileId::new(2);
        let pkg = PackageId::new(0);
        let k = submissions.len();
        for (name, key) in [
            ("session parse", QueryKey::ParseFile(session_file)),
            ("expansion", QueryKey::Expanded(pkg)),
            ("resolve", QueryKey::Resolve(pkg)),
            ("checked package", QueryKey::CheckedPackage(pkg)),
            ("analyzed", QueryKey::Analyzed(pkg)),
            ("package linearity", QueryKey::PackageLinearity(pkg)),
            ("package effects", QueryKey::PackageEffects(pkg)),
            ("platform check", QueryKey::PlatformCheck(pkg)),
            ("pooled check", QueryKey::PooledCheck(pkg)),
        ] {
            assert_eq!(
                db.compute_count(&key),
                k,
                "{name}: one re-derivation per submission, never two"
            );
        }
        assert_eq!(
            db.compute_count(&QueryKey::ParseFile(FileId::new(0))),
            1,
            "the ambient parses once for the whole session"
        );
        assert_eq!(
            db.compute_count(&QueryKey::ParseFile(FileId::new(1))),
            1,
            "the host provision parses once for the whole session"
        );
    }
}
