//! Timing harness for the memo renewal compares: the keystroke shape
//! where bytes move but the package's facts hold (a comment-only edit),
//! re-driving parse, resolve, checked package, and analyzed over the
//! stdlib ambient plus one small member document. Every round is a real
//! content change, so the whole chain recomputes and each package-grain
//! memo's renewal compare runs — the whole-package deep compares and the
//! per-definition type render this fixture isolates. Not a libtest bench:
//! run with `cargo bench -p ds-query --bench renewal_loop`; the round
//! count defaults to 200 and is overridable via
//! `DS_QUERY_RENEWAL_ROUNDS`.

use std::time::Instant;

use ds_diag::FileId;
use ds_query::{Analyzed, CheckedPackage, Db, PackageId, ParseFile, Resolve};

/// One variant of the edited member: real declarations so the resolve,
/// check, and render facts of the renewal compares are real, and a typed
/// const so top-level def types ride in the compared facts.
const DOC: &str = concat!(
    "pub fn work(p: string) -> () {\n",
    "  const h = open_file(p)\n",
    "  close_file(h)\n",
    "}\n",
    "const limit = 42\n",
);

fn main() {
    let rounds: usize = std::env::var("DS_QUERY_RENEWAL_ROUNDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(200);
    let mut db = Db::new();
    let _ambient = db.add_file(ds_stdlib::AMBIENT_FILE, ds_stdlib::AMBIENT_SOURCE);
    let doc = db.add_file("doc.ds", DOC);
    let mut walls = Vec::with_capacity(rounds);
    for round in 0..rounds {
        // A comment-only edit: the bytes move every round (a real change
        // the parse sees), the package's facts never do, so every round
        // recomputes the chain and every renewal compare answers equal.
        let variant = format!("{DOC}\n// edit {round}\n");
        db.set_file_content(doc, &variant).expect("the edit lands");
        let start = Instant::now();
        renewal_round(&mut db, doc);
        walls.push(start.elapsed());
    }
    walls.sort_unstable();
    println!("renewal-loop rounds: {rounds}");
    println!("wall median: {:?}", walls[rounds / 2]);
    println!(
        "wall p10/p90: {:?}/{:?}",
        walls[rounds / 10],
        walls[rounds * 9 / 10]
    );
}

/// One same-facts edit round: the query set whose renewal compares walk
/// the whole package (resolve, checked package, analyzed).
fn renewal_round(db: &mut Db, doc: FileId) {
    let _ = db.get::<ParseFile>(doc).expect("the parse answers");
    let _ = db
        .get::<Resolve>(PackageId::new(0))
        .expect("the resolve answers");
    let _ = db
        .get::<CheckedPackage>(PackageId::new(0))
        .expect("the check answers");
    let _ = db
        .get::<Analyzed>(PackageId::new(0))
        .expect("the analysis answers");
}
