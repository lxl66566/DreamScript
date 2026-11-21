//! Criterion bench of the cache-probe shape over the provider seam: a
//! store holding many installed bundles, a root manifest pinning a
//! subset, and one probe = a fresh provider's resolution plus the
//! materialization of every resolved dep's member identity — the work
//! `ds info`'s prefix tier and the LSP's linked surfaces pay per probe.
//! The two benches differ only in the materialization: the per-dep
//! bundle loads the store answered before the seam carried identities,
//! and the seam-carried identities the probe reads today (the provider
//! folds them where it decodes each demand's pinned bundle, so the
//! probe's own work beyond resolution is the field read — and the
//! resolution under it reads one file per demand, never the installed
//! population).

use std::fmt::Write as _;

use criterion::{Criterion, criterion_group, criterion_main};
use ds_diag::FileId;
use ds_mod::Hash128;
use ds_pkg::{Manifest, Package, Store, StoreProvider, resolve};

/// The installed bundles the store holds beside the root's deps: each
/// demand's pin directs its own store read, so the unrelated bundles
/// price nothing — the population stays so the fixture keeps the shape
/// a store-wide scan used to price.
const INSTALLED: usize = 64;

/// The root's pinned deps: the per-probe per-dep work prices these.
const DEPS: usize = 16;

/// Each bundle's member count and member size: a mid-sized package.
const MEMBERS: usize = 4;
const MEMBER_BYTES: usize = 2048;

/// One fixture bundle: `MEMBERS` source members of `MEMBER_BYTES`
/// deterministic bytes over a dep-free manifest.
fn bundle(index: usize) -> Package {
    let manifest = Manifest::parse(
        &format!("name = \"@p{index}\"\nversion = \"1.0.0\"\n"),
        FileId::new(0),
    )
    .expect("fixture manifest parses");
    let sources = (0..MEMBERS).map(|member| {
        // ASCII text: the member fold refuses bytes a compile cannot
        // read, so the fixture members must be UTF-8
        let bytes: Vec<u8> = (0..MEMBER_BYTES)
            .map(|byte| b'a' + u8::try_from((index ^ member ^ byte) % 26).expect("below 26"))
            .collect();
        (format!("m{member}.ds"), bytes)
    });
    Package::new(&manifest, sources).expect("fixture bundle builds")
}

/// The probe fixture: a store with `INSTALLED` bundles and the root
/// manifest pinning the first `DEPS`.
fn fixture() -> (tempfile::TempDir, Store, Manifest) {
    let dir = tempfile::tempdir().expect("the scratch dir creates");
    let store = Store::open(dir.path());
    let mut pins = String::new();
    for index in 0..INSTALLED {
        let package = bundle(index);
        let hash = store.install(&package).expect("the fixture installs");
        if index < DEPS {
            // one write per dep line: String formatting never fails
            let _ = writeln!(
                pins,
                "@p{index} = {{ ver = \"1.0.0\", hash = \"blake3:{hash}\" }}"
            );
        }
    }
    let root = Manifest::parse(
        &format!("name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n{pins}"),
        FileId::new(0),
    )
    .expect("the root manifest parses");
    (dir, store, root)
}

/// One probe over the per-dep bundle loads, the probe shape before the
/// seam carried identities: a fresh provider resolves, then each
/// resolved dep's bundle loads and validates from the store and the
/// member identity folds over the decoded members.
fn probe_over_per_dep_loads(store: &Store, root: &Manifest) -> Vec<Hash128> {
    let mut provider = StoreProvider::cached(store);
    let resolved = resolve(root, |name, ver, pin| provider.fetch(name, ver, pin))
        .expect("the fixture resolves");
    resolved
        .deps()
        .values()
        .map(|package| {
            let bundle = store
                .load(package.content_hash)
                .expect("the fixture bundle loads")
                .expect("the fixture bundle is installed");
            ds_pkg::member_identity(
                bundle
                    .members()
                    .iter()
                    .filter(|(path, _)| path.as_str() != ds_pkg::MANIFEST_MEMBER)
                    .map(|(path, bytes)| (path.as_str(), bytes.as_slice())),
            )
            .expect("the fixture members fold")
        })
        .collect()
}

/// One probe over the seam-carried identities, the probe shape today: a
/// fresh provider resolves and each resolved dep's identity reads off
/// the seam's answer — no bundle loads, no member extraction.
fn probe_over_the_seam(store: &Store, root: &Manifest) -> Vec<Hash128> {
    let mut provider = StoreProvider::cached(store);
    let resolved = resolve(root, |name, ver, pin| provider.fetch(name, ver, pin))
        .expect("the fixture resolves");
    resolved
        .deps()
        .values()
        .map(|package| package.member_identity)
        .collect()
}

fn bench(criterion: &mut Criterion) {
    let (_dir, store, root) = fixture();
    let mut group = criterion.benchmark_group("probe");
    group.sample_size(30);
    group.throughput(criterion::Throughput::Elements(DEPS as u64));
    group.bench_function("identities_over_per_dep_loads", |bench| {
        bench.iter(|| probe_over_per_dep_loads(&store, &root));
    });
    group.bench_function("identities_over_the_seam", |bench| {
        bench.iter(|| probe_over_the_seam(&store, &root));
    });
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
