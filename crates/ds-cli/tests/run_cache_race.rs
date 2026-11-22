//! The script-mode dsmod cache's cross-process safety under contention:
//! many concurrent `ds run` processes over ONE shared cache root — the
//! same entry sources compiled fresh (all racers miss, compile, and race
//! to install) and served warm (all racers hit) — where every process's
//! stdout must match the single-process golden byte for byte, and a
//! reader hammering the artifact paths throughout must never observe a
//! file that does not fully decode as the module its content-addressed
//! name claims (the install-ordering invariant: the final visibility
//! step is one atomic rename of a fully written temp file, so the only
//! observables at an artifact path are absence, an unopenable transient,
//! or the complete artifact — never a partial install). The campaign is
//! bounded: a fixed round budget across the process counts 2/4/8, a wall
//! deadline that stops early on a slow host, and a graceful skip when
//! the environment cannot spawn the binary at all.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

mod support;

/// The two entry sources the campaign races: distinct texts and stems so
/// their keys (and artifacts) differ while sharing one root — multi-line
/// outputs make any served deviation visible in full, not just at the
/// first byte.
const SOURCES: [(&str, &str); 2] = [
    (
        "racea",
        concat!(
            "fn main() -> () {\n",
            "  println(\"alpha line one\")\n",
            "  println(\"beta line two\")\n",
            "  println(\"gamma line three\")\n",
            "}\n",
        ),
    ),
    (
        "raceb",
        concat!(
            "fn helper(w: int) -> int {\n",
            "  w +% 2\n",
            "}\n",
            "fn main() -> () {\n",
            "  println(\"delta line one\")\n",
            "  println(\"epsilon ${helper(21)}\")\n",
            "}\n",
        ),
    ),
];

/// The concurrent process counts the campaign cycles through: a
/// contended pair, a small fleet, and a wide fleet over one root.
const COUNTS: [usize; 3] = [2, 4, 8];

/// Rounds per process count: 210 concurrent rounds, 980 spawned runs.
const ROUNDS_PER_COUNT: usize = 70;

/// The campaign's wall budget: a host slower than this stops early and
/// asserts over the rounds that ran (the nextest profile carries its own
/// slower bound for the kill).
const DEADLINE: Duration = Duration::from_secs(60);

/// One round's kill bound: a racer wedging past this is killed and names
/// a hang, never a loaded run.
const ROUND_BUDGET: Duration = Duration::from_secs(20);

/// One shared cache root per test process: the whole campaign runs over
/// it, never a real user cache directory.
fn campaign_root() -> PathBuf {
    support::guarded_dir("ds-cli-run-cache-race-")
}

/// One source file per tag, under a private directory.
fn source_file(tag: &str, text: &str) -> PathBuf {
    let dir = support::guarded_dir("ds-cli-run-cache-race-src-");
    let path = dir.join(format!("{tag}.ds"));
    fs::write(&path, text).expect("the source writes");
    path
}

/// Every `.dsmod` under the root, recursively.
fn artifacts(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "dsmod") {
                found.push(path);
            }
        }
    }
    found
}

/// The reader hammer's shared tallies: how many byte streams it opened,
/// how many fully decoded as the module their path claims, and the first
/// violation.
#[derive(Default)]
struct Hammer {
    stop: AtomicBool,
    samples: AtomicUsize,
    decodes: AtomicUsize,
    violation: Mutex<Option<String>>,
}

impl Hammer {
    /// One sampled byte stream at one watched path: a decode refusal or
    /// an identity disagreement is the violation the invariant forbids.
    fn sample(&self, path: &Path, bytes: &[u8], tag: &str, text: &str) {
        self.samples.fetch_add(1, Ordering::Relaxed);
        match ds_mod::read(bytes) {
            Ok(module) => {
                if module.name() == tag
                    && module.source_hash() == ds_mod::Hash128::compute(text.as_bytes())
                {
                    self.decodes.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.record(format!(
                        "module `{}` hash {} under the {} key",
                        module.name(),
                        module.source_hash(),
                        path.display()
                    ));
                }
            },
            Err(error) => self.record(format!(
                "a partial or foreign artifact at {}: {error}",
                path.display()
            )),
        }
    }

    fn record(&self, text: String) {
        let mut slot = self.violation.lock().expect("the violation slot locks");
        if slot.is_none() {
            *slot = Some(text);
        }
    }
}

/// One `ds run` racer with piped output and the decision observable on.
fn spawn_racer(cache: &Path, source: &Path) -> std::io::Result<Child> {
    Command::new(env!("CARGO_BIN_EXE_ds"))
        .env("DS_CACHE", cache)
        .env("DS_RUN_CACHE_DEBUG", "1")
        .arg("run")
        .arg(source)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

/// Waits for every racer in the round inside the round budget, then
/// collects (success, stdout, stderr) per racer. A racer wedging past
/// the budget is killed and the round fails naming the hang.
fn finish_round(children: &mut [Child]) -> Vec<(bool, String, String)> {
    let deadline = Instant::now() + ROUND_BUDGET;
    let mut drained: Vec<Option<(bool, String, String)>> = children.iter().map(|_| None).collect();
    while drained.iter().any(Option::is_none) {
        if Instant::now() > deadline {
            for child in children.iter_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
            panic!("a racer wedged past the {ROUND_BUDGET:?} round budget");
        }
        for (child, slot) in children.iter_mut().zip(drained.iter_mut()) {
            if slot.is_some() {
                continue;
            }
            if let Ok(Some(status)) = child.try_wait() {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stdout.take() {
                    let _ = pipe.read_to_string(&mut stdout);
                }
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = pipe.read_to_string(&mut stderr);
                }
                *slot = Some((status.success(), stdout, stderr));
            }
        }
        thread::sleep(Duration::from_millis(2));
    }
    drained
        .into_iter()
        .map(|slot| slot.expect("every racer drained"))
        .collect()
}

#[test]
fn concurrent_runs_over_one_shared_cache_match_the_golden() {
    let root = campaign_root();
    let sources: Vec<PathBuf> = SOURCES
        .iter()
        .map(|(tag, text)| source_file(tag, text))
        .collect();

    // the single-process goldens: one cold solo run per source over the
    // shared root captures the byte reference every racer must print and
    // lands each source's artifact once
    let mut goldens = Vec::new();
    for source in &sources {
        let output = match Command::new(env!("CARGO_BIN_EXE_ds"))
            .env("DS_CACHE", &root)
            .env("DS_RUN_CACHE_DEBUG", "1")
            .arg("run")
            .arg(source)
            .output()
        {
            Ok(output) => output,
            // an environment that cannot spawn the binary proves nothing
            // about process contention — skip rather than fail
            Err(error) => {
                eprintln!("skipping: the ds binary cannot spawn here: {error}");
                return;
            },
        };
        assert!(output.status.success(), "the golden run succeeds");
        goldens.push(String::from_utf8_lossy(&output.stdout).into_owned());
    }

    // map each source to its artifact by the module the artifact itself
    // names (directory order carries no such mapping)
    let mut artifact_for_source: Vec<PathBuf> = SOURCES.iter().map(|_| PathBuf::new()).collect();
    for path in artifacts(&root) {
        let module =
            ds_mod::read(&fs::read(&path).expect("the golden artifact reads")).expect("it decodes");
        let index = SOURCES
            .iter()
            .position(|(tag, _)| module.name() == *tag)
            .expect("the golden artifact names its source");
        artifact_for_source[index] = path;
    }
    assert!(
        artifact_for_source
            .iter()
            .all(|path| !path.as_os_str().is_empty()),
        "one artifact per source"
    );

    // the reader hammer: the whole campaign long, a thread samples both
    // artifact paths and asserts the install-ordering invariant on every
    // byte stream it can open — a partial install would decode-fail or
    // disagree with the identity the path's name claims
    let hammer = Arc::new(Hammer::default());
    let hammer_thread = {
        let hammer = Arc::clone(&hammer);
        let watched = artifact_for_source.clone();
        let texts = SOURCES.map(|(tag, text)| (tag.to_owned(), text.to_owned()));
        thread::spawn(move || {
            while !hammer.stop.load(Ordering::Relaxed) {
                for (path, (tag, text)) in watched.iter().zip(texts.iter()) {
                    // absence or an unopenable transient (mid-install
                    // sharing states) observes no content at all
                    if let Ok(bytes) = fs::read(path) {
                        hammer.sample(path, &bytes, tag, text);
                    }
                }
            }
        })
    };

    // the campaign: process counts cycle 2/4/8, the source alternates
    // every three rounds, and cold rounds (deleting the round source's
    // artifact so every racer compiles fresh and races to install)
    // alternate with warm rounds every six — every racer in a round runs
    // concurrently over the one shared root
    let start = Instant::now();
    let total_rounds = COUNTS.len() * ROUNDS_PER_COUNT;
    let mut rounds_done = 0usize;
    let mut hits = 0usize;
    let mut misses = 0usize;
    for round in 0..total_rounds {
        if start.elapsed() > DEADLINE {
            eprintln!(
                "the campaign stopped early at round {round}/{total_rounds} past the {DEADLINE:?} \
                 deadline"
            );
            break;
        }
        let count = COUNTS[round % COUNTS.len()];
        let source = round / COUNTS.len() % SOURCES.len();
        if round / (COUNTS.len() * SOURCES.len()) % 2 == 0 {
            let _ = fs::remove_file(&artifact_for_source[source]);
        }
        let mut children = Vec::with_capacity(count);
        for _ in 0..count {
            match spawn_racer(&root, &sources[source]) {
                Ok(child) => children.push(child),
                Err(error) => panic!("round {round} cannot spawn its racer: {error}"),
            }
        }
        for (success, stdout, stderr) in finish_round(&mut children) {
            assert!(
                success,
                "round {round} racer exits clean (stderr: {stderr})"
            );
            assert_eq!(
                stdout, goldens[source],
                "round {round} racer prints its golden byte for byte"
            );
            let decisions = stderr
                .lines()
                .filter(|line| line.contains("ds run cache: "))
                .count();
            assert_eq!(
                decisions, 1,
                "one lookup decision per racer (stderr: {stderr})"
            );
            if stderr.contains("cache: hit") {
                hits += 1;
            } else {
                assert!(
                    stderr.contains("cache: miss"),
                    "the only other decision is a miss (stderr: {stderr})"
                );
                misses += 1;
            }
        }
        rounds_done += 1;
    }
    assert!(rounds_done >= 1, "at least one round ran");
    assert!(hits > 0, "the campaign exercised the hit path");
    assert!(misses > 0, "the campaign exercised the miss path");

    hammer.stop.store(true, Ordering::Relaxed);
    hammer_thread.join().expect("the hammer thread joins");
    assert!(
        hammer.decodes.load(Ordering::Relaxed) >= 100,
        "the hammer sampled enough readable artifacts ({}/{})",
        hammer.decodes.load(Ordering::Relaxed),
        hammer.samples.load(Ordering::Relaxed),
    );
    assert!(
        hammer
            .violation
            .lock()
            .expect("the violation slot locks")
            .is_none(),
        "no reader observed a partial or foreign install"
    );

    // the root at rest: every dsmod under it is one of the two known
    // artifacts and still validates as the module its key claims
    for path in artifacts(&root) {
        assert!(
            artifact_for_source.contains(&path),
            "no stray dsmod lands under the root: {}",
            path.display()
        );
        let bytes = fs::read(&path).expect("the rested artifact reads");
        ds_mod::read(&bytes).expect("the rested artifact decodes");
    }

    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(sources[0].parent().expect("the source dir"));
}
