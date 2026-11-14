//! The `ds repl` subcommand: an interactive read-eval-print loop over the
//! in-process pipeline with a persistent session (H6 item 4's first
//! slice).
//!
//! # Session model
//!
//! The session is one growing module. Declaration submissions (`fn`,
//! `struct`, ...) append verbatim as their own `##` blocks; statement and
//! expression submissions append verbatim as statements of one REPL-owned
//! `fn main` block — the only scope where a later line both sees an
//! earlier line's bindings and the block's tail expression returns the
//! last submission's value (the script-mode fold fixes its synthesized
//! `main` at `-> ()`, which would strap the value print; owning `main`
//! keeps every component stock). Every submission re-runs the memoized
//! queries over the whole session text through one query database —
//! block-grain invalidation renews the untouched declaration blocks, so
//! only the new block and `main` recompute — then re-executes `main` on
//! one persistent VM: the freshly lowered module installs under the same
//! name each time, so the two-slot swap retires the previous version.
//!
//! Re-execution is the model: side effects repeat, and every live
//! binding dies through its ordinary scope-exit deinit at each run's end
//! — a value is never silently dropped between runs because none is ever
//! alive between runs. `:drop <name>` removes the binding's line so later
//! runs never recreate it; `:reset` clears the text and rebuilds the VM.
//!
//! # Reading
//!
//! An interactive terminal reads through a line editor (in-memory
//! history: up/down recall prior submissions; Ctrl-C answers as an event,
//! so the line in progress is discarded and the prompt re-issues while
//! the session survives); a piped stdin reads plain lines — no editing
//! exists over a pipe. Ctrl-C while a submission computes still kills the
//! process: the VM has no step budget to observe an interrupt at.

use std::{
    io::{self, BufRead, IsTerminal, Write},
    process::ExitCode,
};

use anyhow::{Context, Result, anyhow};
use clap::Args;
use ds_check::DefKind;
use ds_diag::{Diagnostic, FileId, Files, LabelStyle, render};
use ds_lower::lower;
use ds_mod::Hash128;
use ds_parse::{Ast, ItemKind, Pattern, Stmt, StmtKind};
use ds_query::{
    Analyzed, BlockIndex, Db, DefId, EffectsOf, LinearityCheck, PackageEffects, PackageId,
    ParseFile, PlatformCheck, QueryError, Resolve, TypeOf,
};
use ds_vm::{LoadError, ModuleRegistry, Platform, Value, Vm, native};

use crate::run;

/// The session module's name (the reserved stems `builtin`/`std` are not
/// taken).
const MODULE: &str = "repl";

/// The session file's name in the diagnostic registry.
const SESSION_FILE: &str = "repl.ds";

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
/// commands answer, end of input ends the session, an interrupt discards
/// the line in progress and re-issues the prompt.
fn drive(session: &mut Session, reader: &mut dyn ReadsLines) -> Result<ExitCode> {
    loop {
        match read_submission(reader) {
            ReadOutcome::Eof => return Ok(ExitCode::SUCCESS),
            // Ctrl-C at the prompt: the submission in progress is already
            // discarded; a newline closes the aborted line and the prompt
            // re-issues. The session survives.
            ReadOutcome::Interrupted => println!(),
            ReadOutcome::Text(line) => {
                if line.trim().is_empty() {
                    continue;
                }
                if let Some(command) = line.strip_prefix(':') {
                    if !session.command(command)? {
                        return Ok(ExitCode::SUCCESS);
                    }
                } else {
                    session.submit(&line)?;
                }
            },
        }
    }
}

/// One live session: the query database over the ambient plus the session
/// text, the submitted lines, and the persistent VM.
struct Session {
    db: Db,
    file: FileId,
    decls: Vec<String>,
    stmts: Vec<String>,
    vm: Vm,
}

impl Session {
    /// The growing module text: declaration blocks, then the REPL-owned
    /// `main` carrying every statement submission in order — the last
    /// expression submission as its tail, so `main` returns its value.
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
        for stmt in stmts {
            text.push_str(stmt);
            text.push('\n');
        }
        text.push_str("}\n");
        text
    }

    fn new() -> Self {
        let mut db = Db::with_declared_rows(ds_stdlib::declared_rows().clone())
            .with_world_caps(ds_stdlib::world_caps().clone());
        // The ambient is a session package member (G3): its definitions
        // resolve for every submission, and its file id is never needed
        // again.
        let _ambient = db.add_file(ds_stdlib::AMBIENT_FILE, ds_stdlib::AMBIENT_SOURCE);
        let file = db.add_file(SESSION_FILE, &Self::text(&[], &[]));
        Self {
            db,
            file,
            decls: Vec::new(),
            stmts: Vec::new(),
            vm: fresh_vm(),
        }
    }

    /// The committed text, what the database should hold between
    /// submissions.
    fn committed(&self) -> String {
        Self::text(&self.decls, &self.stmts)
    }

    /// Handles one language submission: trial-append the line, run the
    /// memoized query pass over the whole session text, and on a clean
    /// pass execute `main` on the persistent VM. A submission with
    /// diagnostics, a lowering refusal, or a trap is not committed — a
    /// poisoned line would re-fail (or re-trap) every later run.
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
            return self.rollback(&before, false);
        }
        match self.execute(&trial, zone) {
            Ok(()) => Ok(()),
            Err(Failure::Engine(error)) => Err(error),
            Err(failure) => {
                println!("{}", failure.message());
                self.rollback(&before, failure.rebuilds_vm())
            },
        }
    }

    /// Removes the just-tried submission from the session and restores
    /// the database; a trap additionally rebuilds the VM (the fiber a
    /// trap unwound is not one to keep running on).
    fn rollback(&mut self, before: &(Vec<String>, Vec<String>), trapped: bool) -> Result<()> {
        self.decls.clone_from(&before.0);
        self.stmts.clone_from(&before.1);
        if trapped {
            self.vm = fresh_vm();
        }
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
            let def = DefId::from_block(block.id)?;
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
        if reports.is_empty() {
            return Ok(true);
        }
        reports.sort_by_key(focus);
        for diag in &reports {
            print!("{}", render::human(diag, self.db.files()));
        }
        Ok(false)
    }

    /// Compiles the clean session through the analyzed query — the
    /// analyzer's check half over the expansion and resolve memos,
    /// memoized like every other pass, lowered over the bundle's own
    /// expanded trees —
    /// installs the module over its own retiring version, runs `main`,
    /// and prints the value, the inferred type, and the effect row. A
    /// lowering refusal, an install failure, or a trap is a [`Failure`]
    /// the caller rolls back.
    fn execute(&mut self, text: &str, zone: Zone) -> std::result::Result<(), Failure> {
        let analyzed = query(self.db.get::<Analyzed>(PackageId::new(0)), "analyzed")
            .map_err(Failure::Engine)?
            .analyzed;
        let lowered = lower(
            MODULE,
            Hash128::compute(text.as_bytes()),
            &analyzed.asts,
            &analyzed,
        )
        .map_err(|error| Failure::lower(&error, self.db.files()))?;
        let (decls, stmts) = (self.decls.len(), self.stmts.len());
        self.vm
            .install_dsmod(lowered.into_module())
            .map_err(|error| Failure::install(&error, decls, stmts))?;
        match self.vm.run(MODULE, "main", &[]) {
            Ok(value) => {
                let session =
                    query(self.db.get::<ParseFile>(self.file), "parse").map_err(Failure::Engine)?;
                // a clean check implies the tree exists
                let Some(session_ast) = session.ast.as_deref() else {
                    return Err(Failure::Engine(anyhow!("the session parse lost its tree")));
                };
                self.report(session_ast, &analyzed, &value, zone)
                    .map_err(Failure::Engine)
            },
            Err(trap) => Err(Failure::Trap(run::trap_error(&self.vm, &trap))),
        }
    }

    /// Prints the submission's story: the value of an expression tail
    /// (the unit word is inert, so a unit-typed tail prints no value),
    /// the inferred type, and the effect row — the answer expression's
    /// own row from the effects pass's per-span table (a statement
    /// submission's answer is the tail expression, or the binding line's
    /// initializer), falling back to the touched definition's row when
    /// no span covers it.
    fn report(
        &mut self,
        ast: &Ast,
        analyzed: &ds_check::Analyzed,
        value: &Value,
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
                let Some(stmt) = main_tail(ast) else {
                    return Ok(());
                };
                // The submission's answer expression: the tail
                // expression, or the binding line's initializer.
                let answer = match &stmt.kind {
                    StmtKind::Local(local) => Some(&local.init),
                    StmtKind::Expr(expr) => Some(expr),
                };
                match &stmt.kind {
                    StmtKind::Local(local) => {
                        let mut names = Vec::new();
                        pattern_names(&local.pat, &mut names);
                        for ident in names {
                            if let Some(fact) = analyzed.checked.binders.get(&ident.span) {
                                println!("{}: {}", ident.name, universe.render(fact.ty));
                            }
                        }
                    },
                    StmtKind::Expr(_) => {
                        // `main` is the REPL's own zero-arg wrapper, so
                        // its solved return is the tail expression's
                        // type; the universe spells the arity-0 tuple
                        // "()", the inert unit word.
                        if let Some(ret) = main_return(analyzed, universe) {
                            if ret != "()" {
                                print_result(value);
                            }
                            println!(": {ret}");
                        }
                    },
                }
                // The answer's own row (the tightest recorded span
                // covering it — earlier statements' effects stay out);
                // the definition grain is the fallback.
                let effects = query(
                    self.db.get::<PackageEffects>(PackageId::new(0)),
                    "package effects",
                )?;
                let row = answer
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

    /// Handles one `:` command; `:quit` answers false and ends the loop.
    fn command(&mut self, command: &str) -> Result<bool> {
        let mut words = command.split_whitespace();
        match words.next() {
            Some("quit") => Ok(false),
            Some("drop") => {
                if let Some(name) = words.next() {
                    self.drop_binding(name);
                } else {
                    println!("usage: :drop <name>");
                }
                Ok(true)
            },
            Some("reset") => {
                self.decls.clear();
                self.stmts.clear();
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

    /// Removes one binding's line from the session, so later runs never
    /// recreate it. Between submissions nothing is alive — each run
    /// already ended `main`'s scope, firing the binding's deinit through
    /// the ordinary path — so removal is the whole death.
    fn drop_binding(&mut self, name: &str) {
        if let Some(pos) = self.stmts.iter().position(|s| binds_name(s, name)) {
            self.stmts.remove(pos);
        } else if let Some(pos) = self.decls.iter().position(|d| declares_name(d, name)) {
            self.decls.remove(pos);
        } else {
            println!("no binding {name} in the session");
            return;
        }
        let committed = self.committed();
        if self.db.set_file_content(self.file, &committed).is_ok() {
            println!("dropped {name}");
        }
    }
}

/// Which zone a submission goes to: a declaration block (its declared
/// name, when it carries one — `impl`, `import`, and `platform` blocks
/// declare none the package names), or a statement of the REPL-owned
/// `main`.
enum Zone {
    /// A declaration block defining `name`.
    Named(String),
    /// A declaration block the package does not name.
    Nameless,
    /// A statement of the REPL-owned `main`.
    Statement,
}

/// Why a clean-checked submission still did not run: the caller prints
/// the message and rolls the submission back (a trapped line would
/// re-trap every later run). A trap additionally rebuilds the VM.
enum Failure {
    /// A query-engine failure; session-fatal.
    Engine(anyhow::Error),
    /// A lowering refusal, rendered like `ds run`'s at construction.
    Lower(anyhow::Error),
    /// An install failure after a clean check.
    Install(anyhow::Error),
    /// A runtime trap; the VM is rebuilt after printing.
    Trap(anyhow::Error),
}

impl Failure {
    /// A lowering refusal: spanned constructs render as diagnostics, the
    /// package-level errors stay one line (the `ds run` rule).
    fn lower(error: &ds_lower::LowerError, files: &Files) -> Self {
        match error.span() {
            Some(span) => {
                let diagnostic =
                    Diagnostic::error(ds_diag::ErrorCode::LowerUnsupported, error.to_string())
                        .with_primary(span, "outside the M3 lowering contract");
                print!("{}", render::human(&diagnostic, files));
                Self::Lower(anyhow!("the line does not lower under the M3 contract"))
            },
            None => Self::Lower(anyhow!("{error}")),
        }
    }

    /// An install failure; a clean check makes this a should-not-happen,
    /// so one self-describing line is enough.
    fn install(error: &LoadError, decls: usize, stmts: usize) -> Self {
        Self::Install(anyhow!(
            "installing the session module failed: {error:?} ({decls} declaration(s), {stmts} \
             statement(s))"
        ))
    }

    /// The failure's message.
    fn message(&self) -> String {
        match self {
            Self::Engine(error) | Self::Lower(error) | Self::Install(error) | Self::Trap(error) => {
                format!("{error:#}")
            },
        }
    }

    /// Whether the VM needs a rebuild: a trap unwound a fiber, and a
    /// failed install may have consumed the module mid-swap.
    fn rebuilds_vm(&self) -> bool {
        matches!(self, Self::Trap(_) | Self::Install(_))
    }
}

/// One query failure, self-describing with the pass that failed.
fn query<T>(result: Result<T, QueryError>, pass: &str) -> Result<T> {
    result.with_context(|| format!("{pass} query failed"))
}

/// A fresh VM with the default platform installed (H5.5): the CLI is the
/// provider, and the install validates pool FuncRefs against it.
fn fresh_vm() -> Vm {
    let mut registry = ModuleRegistry::new();
    registry.install_platform(Platform::std());
    Vm::new(registry)
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

/// The last statement of the REPL-owned `main`'s body.
fn main_tail(ast: &Ast) -> Option<&Stmt> {
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
    block.stmts.last()
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
fn touches(diag: &Diagnostic, file: FileId) -> bool {
    diag.labels.iter().any(|label| label.span.file == file)
}

/// Sort key of one diagnostic: the focus span (primary label, else
/// first), source position order like `ds check`.
fn focus(diag: &Diagnostic) -> (u32, u32, u32) {
    diag.labels
        .iter()
        .find(|label| label.style == LabelStyle::Primary)
        .or_else(|| diag.labels.first())
        .map_or((u32::MAX, u32::MAX, u32::MAX), |label| {
            (label.span.file.as_u32(), label.span.start, label.span.end)
        })
}

/// Prints one run result: the platform failure tags are halted runs (the
/// lowered unhandled-failure handler already printed the message), the
/// `none` member of a checked read prints by name, everything else like
/// `ds run`.
fn print_result(value: &Value) {
    match value {
        Value::Tag(tag) if *tag == native::PLATFORM_IO_ERR => {
            println!("halted on an unhandled io failure");
        },
        Value::Tag(tag) if *tag == native::PLATFORM_NET_ERR => {
            println!("halted on an unhandled net failure");
        },
        Value::Tag(tag) if *tag == native::PLATFORM_PARSE_ERR => {
            println!("halted on an unhandled parse failure");
        },
        Value::Tag(tag) if *tag == native::PLATFORM_NONE => println!("none"),
        value => run::print_value(value),
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
