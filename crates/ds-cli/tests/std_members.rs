//! The library-boundary round's end-to-end pins (todo item 53): the
//! ordering pair left the ambient core as the @std package's first
//! bodied members (the word-shaped int signature a linked call binds)
//! and the pure string operations left it as the @std string face
//! (each declaration naming the builtin core native the lowering
//! routes by name), the entry surfaces keep the bare spellings alive
//! (the run path's script sessions composing the faces back beside
//! the core, the dev script rounds and the REPL serving the merged
//! composition — the implicit resolution while the migration runs),
//! and a package compile over a std-declaring graph keeps the
//! boundary the migration left: the ambient band is the shrunk core,
//! a bare use an unknown name, the linked import the binding face.
//! The FP combinator round (todo item 81) rides the same pins: the
//! landed list pair are bodied members of the same file, so the bare
//! spellings run through a script session while the linked import
//! keeps the generic-head refusal until its crossing round (the take,
//! drop, and flat_map shapes wait on the ambient splice's
//! lowering-time-records seam, their residuals on todo item 81). The
//! pooled pair (todo item 66) rides it too: `par_map`/`par_reduce`
//! are bodied members over the private fork-join engine seams, so the
//! bare spellings keep the sequential contract (the pooled drive's
//! byte-identical answers pinned at the VM level) while the fold's
//! init parameter reads the library spelling's one-word instantiation
//! boundary (the multi-word fold keeps its seam-level pin in the
//! lowerer's e2e).

use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn ds() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command.current_dir(env!("CARGO_MANIFEST_DIR"));
    command.env("DS_CACHE", test_cache());
    command
}

/// A private per-process script-cache root: spawned runs never read or
/// write a real user cache directory.
fn test_cache() -> PathBuf {
    static CACHE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            let dir = env::temp_dir().join(format!("ds-cli-std-cache-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            dir
        })
        .clone()
}

/// Writes one manifest-free script under a fresh temp dir and answers
/// the file's path.
fn script(tag: &str, text: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("ds-cli-std-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("the temp dir creates");
    let file = dir.join("app.ds");
    fs::write(&file, text).expect("the script writes");
    file
}

/// Writes one package root (a `ds.pkg` manifest beside the members)
/// under a fresh temp dir and answers the root's path.
fn write_root(tag: &str, manifest: &str, members: &[(&str, &str)]) -> PathBuf {
    let root = env::temp_dir().join(format!("ds-cli-std-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("the root dir creates");
    fs::write(root.join("ds.pkg"), manifest).expect("the manifest writes");
    for (path, text) in members {
        fs::write(root.join(path), text).expect("the member writes");
    }
    root
}

fn run(args: &[&str]) -> Output {
    ds().args(args).output().expect("the binary runs")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn entry_of(root: &Path) -> String {
    root.join("app.ds")
        .to_str()
        .expect("temp path is UTF-8")
        .to_owned()
}

/// The ordering pair's program text: bare calls over the ints, the
/// negative edges included.
const PAIR_MAIN: &str = concat!(
    "fn main() -> int {\n",
    "  println(\"${max(3, 9)} ${min(3, 9)} ${max(-4, -2)} ${min(-4, -2)}\")\n",
    "  0\n",
    "}\n",
);
const PAIR_OUT: &str = "9 3 -2 -4\n0\n";

/// A bare script's session composes the @std bodied members back
/// beside the shrunk core (the implicit resolution over the implicit
/// `@std` graph), so the bare ordering pair runs cold and serves warm
/// under the same answer.
#[test]
fn the_bare_spellings_run_through_a_bare_script() {
    let file = script("run", PAIR_MAIN);
    let entry = file.to_str().expect("temp path is UTF-8");
    let cold = run(&["run", entry]);
    assert_eq!(
        cold.status.code(),
        Some(0),
        "the cold run exits 0: {}",
        text(&cold.stderr)
    );
    assert_eq!(
        text(&cold.stdout),
        PAIR_OUT,
        "the cold run answers the pair"
    );
    let warm = run(&["run", entry]);
    assert_eq!(warm.status.code(), Some(0), "the warm run exits 0");
    assert_eq!(
        text(&warm.stdout),
        PAIR_OUT,
        "the warm run answers identically"
    );
    fs::remove_dir_all(file.parent().expect("the temp dir")).expect("the temp tree removes");
}

/// The same session shape builds: `ds build` of the bare script lowers
/// the session composition's pair into the artifact.
#[test]
fn the_bare_spellings_build_through_a_bare_script() {
    let file = script("build", PAIR_MAIN);
    let out = file.parent().expect("the temp dir").join("pair.dsmod");
    let built = run(&[
        "build",
        file.to_str().expect("temp path is UTF-8"),
        "--out",
        out.to_str().expect("temp path is UTF-8"),
    ]);
    assert_eq!(
        built.status.code(),
        Some(0),
        "the build exits 0: {}",
        text(&built.stderr)
    );
    fs::remove_dir_all(file.parent().expect("the temp dir")).expect("the temp tree removes");
}

/// The REPL session registers the merged ambient: the bare spellings
/// answer per line and the session exits clean.
#[test]
fn the_repl_answers_the_bare_spellings() {
    let mut child = ds()
        .arg("repl")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("the binary spawns");
    child
        .stdin
        .as_mut()
        .expect("stdin pipes")
        .write_all(b"max(3, 9)\nmin(3, 9)\n")
        .expect("the input writes");
    let out = child.wait_with_output().expect("the session drains");
    assert!(out.status.success(), "the session exits cleanly");
    let stdout = text(&out.stdout);
    assert!(
        stdout.contains("9\n: int") && stdout.contains("3\n: int"),
        "the pair answers per line: {stdout}"
    );
}

/// The dev loop's script round registers the merged ambient: the one
/// round compiles over the composed members and answers the pair.
#[test]
fn the_dev_script_round_answers_the_bare_spellings() {
    let file = script("dev", PAIR_MAIN);
    let out = run(&["dev", file.to_str().expect("temp path is UTF-8")]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "the dev round exits 0: {}",
        text(&out.stderr)
    );
    assert_eq!(text(&out.stdout), PAIR_OUT);
    fs::remove_dir_all(file.parent().expect("the temp dir")).expect("the temp tree removes");
}

/// A root manifest declaring the @std dep at the default bundle's
/// content hash and toolchain version.
fn std_manifest() -> String {
    let package = ds_stdlib::std_package();
    let identity = package.content_hash();
    let version = package
        .manifest(ds_diag::FileId::new(0))
        .expect("the default bundle's manifest parses")
        .version
        .to_string();
    format!(
        "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@std = {{ ver = \"{version}\", hash = \
         \"blake3:{identity}\" }}\n"
    )
}

/// The manifest-root boundary the migration keeps: a package compile
/// over a std-declaring graph registers the shrunk core (the ordering
/// pair gone from its namespace — a bare use an unknown name), while
/// the linked import binds the bodied members through the @std surface,
/// the package's exported call face answering over the link.
#[test]
fn a_std_declared_root_refuses_bare_and_binds_the_link() {
    // the bare spelling refuses over the core
    let bare = write_root("core", &std_manifest(), &[(
        "app.ds",
        "fn main() -> int {\n  max(3, 9)\n}\n",
    )]);
    let out = run(&["run", &entry_of(&bare)]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "the bare use over the core refuses"
    );
    assert!(
        text(&out.stderr).contains("unknown name `max`"),
        "the refusal names the missing member: {}",
        text(&out.stderr)
    );
    fs::remove_dir_all(&bare).expect("the temp tree removes");

    // the linked import binds the bodied pair: the first bodied @std
    // members answering through the linked call surface
    let linked = write_root("link", &std_manifest(), &[(
        "app.ds",
        concat!(
            "import @std (max, min)\n",
            "\n",
            "fn main() -> int {\n",
            "  println(\"${max(3, 9)} ${min(3, 9)}\")\n",
            "  0\n",
            "}\n",
        ),
    )]);
    let out = run(&["run", &entry_of(&linked)]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "the linked run exits 0: {}",
        text(&out.stderr)
    );
    assert_eq!(
        text(&out.stdout),
        "9 3\n0\n",
        "the bodied pair answers over the link"
    );
    fs::remove_dir_all(&linked).expect("the temp tree removes");
}

/// The string face's program text: the bare and UFCS spellings over the
/// migrated surface — the trim and case pair, the separator pair, the
/// checked slice and char read, the search family, and the parses with
/// their `parse_err` unions extracted through the err handler.
const STRINGS_MAIN: &str = concat!(
    "fn main() -> int {\n",
    "  const t = \"  Hello World  \".trim()\n",
    "  println(\"${t}\")\n",
    "  println(\"${to_upper(t)} ${to_lower(t)}\")\n",
    "  println(join(t.split(' '), '-'))\n",
    "  println(substring(t, 0, 5).unwrap_or(\"none\"))\n",
    "  println(\"${char_at(t, 1).unwrap_or('?')}\")\n",
    "  println(\"${contains(t, \"World\")} ${index_of(t, \"World\").err((_: none) => -1)}\")\n",
    "  println(replace(t, \"World\", \"There\"))\n",
    "  println(\"${parse_int(\"42\").err((_: parse_err) => 0)} ",
    "${parse_float(\"3.5\").err((_: parse_err) => 0.0)}\")\n",
    "  0\n",
    "}\n",
);
const STRINGS_OUT: &str = concat!(
    "Hello World\n",
    "HELLO WORLD hello world\n",
    "Hello-World\n",
    "Hello\n",
    "e\n",
    "true 6\n",
    "Hello There\n",
    "42 3.5\n",
    "0\n",
);

/// The bare spellings of the @std string face run through a bare
/// script's session (the face composed back beside the core), cold and
/// warm under the same answer.
#[test]
fn the_bare_string_face_runs_through_a_bare_script() {
    let file = script("strings", STRINGS_MAIN);
    let entry = file.to_str().expect("temp path is UTF-8");
    let cold = run(&["run", entry]);
    assert_eq!(
        cold.status.code(),
        Some(0),
        "the cold run exits 0: {}",
        text(&cold.stderr)
    );
    assert_eq!(
        text(&cold.stdout),
        STRINGS_OUT,
        "the cold run answers the face"
    );
    let warm = run(&["run", entry]);
    assert_eq!(warm.status.code(), Some(0), "the warm run exits 0");
    assert_eq!(
        text(&warm.stdout),
        STRINGS_OUT,
        "the warm run answers identically"
    );
    fs::remove_dir_all(file.parent().expect("the temp dir")).expect("the temp tree removes");
}

/// The std-declared root keeps the boundary over the string face too: a
/// bare use over the shrunk core refuses as an unknown name, while the
/// linked import binds the face's extern entries and the calls route to
/// the builtin natives by name.
#[test]
fn a_std_declared_root_refuses_bare_strings_and_binds_the_link() {
    // the bare spelling refuses over the core
    let bare = write_root("strings-core", &std_manifest(), &[(
        "app.ds",
        "fn main() -> int {\n  trim(\"  hi  \")\n}\n",
    )]);
    let out = run(&["run", &entry_of(&bare)]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "the bare use over the core refuses"
    );
    assert!(
        text(&out.stderr).contains("unknown name `trim`"),
        "the refusal names the missing member: {}",
        text(&out.stderr)
    );
    fs::remove_dir_all(&bare).expect("the temp tree removes");

    // the linked import binds the string face's scalar-word extern
    // entries: the package owns the spellings while the builtin
    // natives keep the seams, the calls routing by name exactly as the
    // ambient's own declarations did. The union-returning spellings
    // (`substring`, `char_at`, `index_of`, the parses) keep the
    // linked-surface's tag-seam refusal and the list-returning pair
    // (`split`/`join`) the generic-head refusal, both riding the
    // entry faces alone until their crossing rounds
    let linked = write_root("strings-link", &std_manifest(), &[(
        "app.ds",
        concat!(
            "import @std (trim, to_upper, to_lower, contains, replace)\n",
            "\n",
            "fn main() -> int {\n",
            "  const t = trim(\"  hi there  \")\n",
            "  println(\"${to_upper(t)} ${to_lower(t)} ${contains(t, \"there\")}\")\n",
            "  println(replace(t, \"hi\", \"yo\"))\n",
            "  0\n",
            "}\n",
        ),
    )]);
    let out = run(&["run", &entry_of(&linked)]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "the linked run exits 0: {}",
        text(&out.stderr)
    );
    assert_eq!(
        text(&out.stdout),
        concat!("HI THERE hi there true\n", "yo there\n", "0\n",),
        "the string face answers over the link"
    );
    fs::remove_dir_all(&linked).expect("the temp tree removes");
}

/// The FP combinator round's program text: the landed list pair
/// (`enumerate`, `zip`) through the bare spellings and the UFCS dot
/// forms, the empty and shorter-side edges included.
const COMBINATORS_MAIN: &str = concat!(
    "fn main() -> int {\n",
    "  const xs = [1, 2, 3, 4, 5]\n",
    "  const pairs = zip(xs, [\"a\", \"b\"])\n",
    "  const shorter = zip([7, 8], [\"x\", \"y\", \"z\"])\n",
    "  const words = [\"alpha\", \"beta\", \"gamma\"]\n",
    "  const enumd = enumerate(words)\n",
    "  println(\"zip ${pairs.len()} ${shorter.len()} enum ${enumd.len()} \
     ${xs.enumerate().len()}\")\n",
    "  println(\"edge ${zip(xs, list[int]()).len()} ${enumerate(list[int]()).len()}\")\n",
    "  0\n",
    "}\n",
);
const COMBINATORS_OUT: &str = concat!("zip 2 2 enum 3 5\n", "edge 0 0\n", "0\n",);

/// The landed pair runs cold and serves warm under the same answer:
/// the script session composes the bodied file back beside the shrunk
/// core (the implicit resolution over the implicit `@std` graph), the
/// generic bodies lowering as ambient bodies through the seams their
/// shapes cover.
#[test]
fn the_bare_list_pair_runs_through_a_bare_script() {
    let file = script("combinators", COMBINATORS_MAIN);
    let entry = file.to_str().expect("temp path is UTF-8");
    let cold = run(&["run", entry]);
    assert_eq!(
        cold.status.code(),
        Some(0),
        "the cold run exits 0: {}",
        text(&cold.stderr)
    );
    assert_eq!(text(&cold.stdout), COMBINATORS_OUT, "the cold run answers");
    let warm = run(&["run", entry]);
    assert_eq!(warm.status.code(), Some(0), "the warm run exits 0");
    assert_eq!(
        text(&warm.stdout),
        COMBINATORS_OUT,
        "the warm run answers identically"
    );
    fs::remove_dir_all(file.parent().expect("the temp dir")).expect("the temp tree removes");
}

/// The landed pair's linked-surface boundary over a std-declaring graph:
/// the bare use is an unknown name over the shrunk core, and the linked
/// import keeps the generic-head refusal (a type-argumented signature
/// rides the entry faces alone until its crossing round) — the same
/// seam the string face's list-returning pair keeps.
/// The pooled pair's program text through the @std library spelling:
/// the fork-join combinators as bodied members over the engine seams —
/// the sequential contract the pooled drive answers byte-identically.
const POOLED_MAIN: &str = concat!(
    "fn at(xs: list[int], i: int) -> int {\n",
    "  const cell = xs.get(i)\n",
    "  cell.ok((x: int) => x).err((_: none) => -1)\n",
    "}\n",
    "\n",
    "fn main() -> int {\n",
    "  const xs = [1, 2, 3, 4, 5]\n",
    "  const bias = 10\n",
    "  const doubled = xs.par_map((x: int) => x * 2)\n",
    "  const shifted = par_map(xs, (x: int) => x + bias)\n",
    "  println(\"map ${at(doubled, 0)} ${at(doubled, 4)} ${len(shifted)}\")\n",
    "  const sum = xs.par_reduce(0, (a: int, b: int) => a + b)\n",
    "  const words = [\"alpha\", \"beta\", \"gamma\"]\n",
    "  const joined = words.par_reduce(\"\", (a: string, b: string) => a + b)\n",
    "  const empty = xs.par_reduce(100, (a: int, b: int) => a * b)\n",
    "  println(\"fold ${sum} ${joined} ${empty}\")\n",
    "  println(\"edge ${len(list[int]().par_map((x: int) => x))} ${list[int]().par_reduce(7, (a: \
     int, b: int) => a + b)}\")\n",
    "  0\n",
    "}\n",
);
const POOLED_OUT: &str = concat!(
    "map 2 10 5\n",
    "fold 15 alphabetagamma 12000\n",
    "edge 0 7\n",
    "0\n",
);

/// The pooled pair runs cold and serves warm under the same answer
/// through the @std library spelling: the script session composes the
/// bodied file back beside the shrunk core, the generic body's seam
/// call lowering to the parked-CALLG drive the sequential interlock
/// answers (the pool's byte-identical other half pinned at the VM
/// level).
#[test]
fn the_pooled_pair_runs_through_the_bare_script() {
    let file = script("pooled", POOLED_MAIN);
    let entry = file.to_str().expect("temp path is UTF-8");
    let cold = run(&["run", entry]);
    assert_eq!(
        cold.status.code(),
        Some(0),
        "the cold run exits 0: {}",
        text(&cold.stderr)
    );
    assert_eq!(text(&cold.stdout), POOLED_OUT, "the cold run answers");
    let warm = run(&["run", entry]);
    assert_eq!(warm.status.code(), Some(0), "the warm run exits 0");
    assert_eq!(
        text(&warm.stdout),
        POOLED_OUT,
        "the warm run answers identically"
    );
    fs::remove_dir_all(file.parent().expect("the temp dir")).expect("the temp tree removes");
}

/// A diverging element body refuses through the @std library spelling
/// exactly as through the retired extern spelling: the refusal rides
/// the resolved member's identity, never the call's spelling.
#[test]
fn the_pooled_pair_refuses_diverging_element_bodies() {
    let file = script(
        "pooled-refusal",
        "fn main() -> list[int] {\n  const xs = [1, 2]\n  xs.par_map((x: int) => {\n    \
         println(\"elem\")\n    x\n  })\n}\n",
    );
    let entry = file.to_str().expect("temp path is UTF-8");
    let out = run(&["run", entry]);
    assert_eq!(out.status.code(), Some(1), "the world call inside refuses");
    assert!(
        text(&out.stderr).contains("DS2036"),
        "the refusal is the pooled world call: {}",
        text(&out.stderr)
    );
    fs::remove_dir_all(file.parent().expect("the temp dir")).expect("the temp tree removes");
}

#[test]
fn a_std_declared_root_refuses_the_combinator_link() {
    let bare = write_root("comb-core", &std_manifest(), &[(
        "app.ds",
        "fn main() -> int {\n  zip([1, 2], [\"a\"])\n}\n",
    )]);
    let out = run(&["run", &entry_of(&bare)]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "the bare use over the core refuses"
    );
    assert!(
        text(&out.stderr).contains("unknown name `zip`"),
        "the refusal names the missing member: {}",
        text(&out.stderr)
    );
    fs::remove_dir_all(&bare).expect("the temp tree removes");

    let linked = write_root("comb-link", &std_manifest(), &[(
        "app.ds",
        "import @std (zip)\n\nfn main() -> int {\n  zip([1, 2], [\"a\"]).len()\n}\n",
    )]);
    let out = run(&["run", &entry_of(&linked)]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "the linked import refuses the generic head"
    );
    assert!(
        text(&out.stderr).contains("DS4021"),
        "the refusal is the linked-surface's head seam: {}",
        text(&out.stderr)
    );
    fs::remove_dir_all(&linked).expect("the temp tree removes");
}
