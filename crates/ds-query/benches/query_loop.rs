//! Timing harness for the query host's edit loop: the LSP's
//! diagnostics query set (parse, resolve, checked package, package
//! linearity, platform, pooled) over the stdlib ambient plus one small
//! member document, re-driven across real text edits under
//! std::time::Instant, so the wall answers what one didChange round
//! pays — the affine pass's share is the ambient walk this fixture
//! isolates. Not a libtest bench: run with `cargo bench -p ds-query
//! --bench query_loop`; the round count defaults to 200 and is
//! overridable via `DS_QUERY_BENCH_ROUNDS`.

use std::time::Instant;

use ds_diag::FileId;
use ds_query::{
    CheckedPackage, Db, PackageId, PackageLinearity, ParseFile, PlatformCheck, PooledCheck, Resolve,
};

/// One variant of the edited member: the ambient's linear file surface
/// keeps the affine pass's rows real, and the two variants differ by a
/// second handle so every round is a genuine content change.
const DOC_A: &str = concat!(
    "pub fn work(p: string) -> () {\n",
    "  const h = open_file(p)\n",
    "  close_file(h)\n",
    "}\n",
);

const DOC_B: &str = concat!(
    "pub fn work(p: string) -> () {\n",
    "  const h = open_file(p)\n",
    "  const h2 = open_file(p)\n",
    "  close_file(h)\n",
    "  close_file(h2)\n",
    "}\n",
);

fn main() {
    let rounds: usize = std::env::var("DS_QUERY_BENCH_ROUNDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(200);
    let mut db = Db::new();
    let _ambient = db.add_file(ds_stdlib::AMBIENT_FILE, ds_stdlib::AMBIENT_SOURCE);
    let doc = db.add_file("doc.ds", DOC_A);
    let mut walls = Vec::with_capacity(rounds);
    for round in 0..rounds {
        db.set_file_content(doc, if round % 2 == 0 { DOC_B } else { DOC_A })
            .expect("the edit lands");
        let start = Instant::now();
        diagnostics_round(&mut db, doc);
        walls.push(start.elapsed());
    }
    walls.sort_unstable();
    println!("query-loop rounds: {rounds}");
    println!("wall median: {:?}", walls[rounds / 2]);
    println!(
        "wall p10/p90: {:?}/{:?}",
        walls[rounds / 10],
        walls[rounds * 9 / 10]
    );
}

/// One diagnostics round: the LSP's diagnostics query set, unchanged.
fn diagnostics_round(db: &mut Db, doc: FileId) {
    let _ = db.get::<ParseFile>(doc).expect("the parse answers");
    let _ = db
        .get::<Resolve>(PackageId::new(0))
        .expect("the resolve answers");
    let _ = db
        .get::<CheckedPackage>(PackageId::new(0))
        .expect("the check answers");
    let _ = db
        .get::<PackageLinearity>(PackageId::new(0))
        .expect("the linearity pass answers");
    let _ = db
        .get::<PlatformCheck>(PackageId::new(0))
        .expect("the platform check answers");
    let _ = db
        .get::<PooledCheck>(PackageId::new(0))
        .expect("the pooled check answers");
}
