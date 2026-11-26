//! The checked-in ambient dsmod artifact behind `ds run`'s cold compile:
//! the ambient member's impl bodies pre-lowered to module form, embedded
//! in ds-stdlib beside the AST blob and the `.dsi` tables, so a cold
//! compile lowers only the user's bodies and splices the ambient's in
//! verbatim (`ds_lower::lower_with_served`).
//!
//! The artifact is a pure cache, never correctness-load-bearing: the
//! serving seam validates the bytes before installing anything, and every
//! miss falls back to the fresh in-process lowering with no behavior
//! change. The decode parks once per process behind one shared handle
//! ([`resident`]) the same way the `.dsi` tables park in
//! `ServedTables`: every serve clones what it consumes, so a process
//! that compiles many members over one ambient pays the validating
//! decode once. The checks mirror the script cache's identity story —
//!
//! * the 16-byte header must fold this build's identity
//!   ([`run_cache::BUILD_ID`](crate::run_cache::BUILD_ID), the same digest the cache key carries),
//!   so a lowerer or ambient edit the artifact predates never serves;
//! * the payload must pass `ds_mod::read`'s validating decode (magic, format version, section
//!   framing, payload versions, per-word body canonicity);
//! * the module must name the ambient's artifact label and carry the ambient source's hash;
//! * the module must carry no chains: the splice installs function records plus the tag rows the
//!   bodies minted (the merged lowering seeds its tag table with them), so a chain table — a module
//!   fact no splice installs — is still a miss.
//!
//! The drift gates pin the artifact like the `.dsi` round: a byte gate
//! regenerates from the current compiler and compares, an equivalence
//! gate runs one user program through the served splice and the fresh
//! lowering and byte-compares the finished modules, and the miss matrix
//! exercises every refusal.

use std::sync::{Arc, OnceLock};

use ds_mod::{Hash128, Module, read};

/// The artifact's module-name label. The label never installs — the
/// served function records move into the user module's table — it is
/// the identity the serving seam pins the embedded bytes against.
pub(crate) const AMBIENT_MODULE_NAME: &str = "@std";

/// The artifact header's size in bytes: the build identity's fold.
pub(crate) const HEADER: usize = 16;

/// The artifact's bytes for one generated module: the build identity's
/// fold ahead of the module's own bytes, the `.ast`/`.dsi` header
/// discipline over the dsmod payload (which itself carries the source
/// hash, so the header carries the one identity the payload cannot).
#[cfg(test)]
pub(crate) fn artifact_bytes(module: &Module, identity: Hash128) -> Vec<u8> {
    let mut bytes = identity.as_bytes().to_vec();
    bytes.extend_from_slice(&ds_mod::write(module));
    bytes
}

/// This build's artifact identity: the same build digest the script
/// cache keys fold, through the same Hash128 fold the cache applies to
/// its key components — one identity, two consumers, so any change that
/// re-keys the script cache invalidates the ambient artifact with it.
pub(crate) fn build_identity() -> Hash128 {
    Hash128::compute(crate::run_cache::BUILD_ID.as_bytes())
}

/// The ambient member's lowered module, generated from the current
/// compiler: parse, the fresh analyze (the `.dsi` round pins the served
/// and fresh id spaces equal, so either analyze lowers identically),
/// and the declaration-only lowering whose `main` waiver the artifact's
/// generation needs. The ambient must check clean on its own for the
/// artifact to build.
#[cfg(test)]
pub(crate) fn generate_module() -> Module {
    let mut files = ds_diag::Files::new();
    let id = files.add_source(ds_stdlib::AMBIENT_FILE, ds_stdlib::AMBIENT_SOURCE);
    let ast = ds_parse::parse(&files, id).expect("the ambient parses");
    let analyzed = ds_meta::analyze_owned(&files, vec![Arc::new(ast)], &[false]);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the ambient does not check clean: {:?}",
        analyzed.checked.diags
    );
    let lowered = ds_lower::lower_ambient(
        AMBIENT_MODULE_NAME,
        Hash128::compute(ds_stdlib::AMBIENT_SOURCE.as_bytes()),
        &analyzed.asts,
        &analyzed,
    )
    .expect("the ambient lowers");
    let module = lowered.module();
    assert!(
        module.chains().is_empty(),
        "the ambient's lowering mints a chain table: the splice installs function records and tag \
         rows only, so the serving seam needs widening first"
    );
    module.clone()
}

/// The artifact's resident module, decoded and validated once per
/// process behind one shared handle: every input the validation reads
/// is process-constant (the embedded bytes, the compile-time build
/// identity, the embedded ambient source), so the checks answer once
/// with the same verdict each serve would reach, and a miss stays
/// cached with it — a drifted artifact answers every caller with the
/// same miss it answers today, only without re-decoding the bytes per
/// call. Both consumers clone their own copy (the splice its function
/// records and tag rows, the load walk the whole module), so the
/// clone — plain `Vec` copies, no varint decode, no body re-validation
/// — is all a later serve pays, and no caller holds the resident
/// itself: no two compiles share a mutable module.
pub(crate) fn resident() -> Option<Arc<Module>> {
    RESIDENT
        .get_or_init(|| decode(ds_stdlib::AMBIENT_DSMOD, &build_identity()).map(Arc::new))
        .clone()
}

/// The artifact's decoded module, parked once per process.
static RESIDENT: OnceLock<Option<Arc<Module>>> = OnceLock::new();

/// The served ambient splice for this build, when the embedded artifact
/// verifies: the function records a cold compile splices in front of
/// its own table, plus the tag rows those bodies minted. `None` is the
/// miss every caller falls back from.
pub(crate) fn serve() -> Option<ds_lower::Served> {
    let module = resident()?;
    Some(ds_lower::Served {
        tags: module.tags().to_vec(),
        functions: module.functions().to_vec(),
    })
}

/// The validating decode over artifact bytes: the miss matrix in one
/// place, so the poisoned-input gates exercise exactly the checks the
/// resident park consults.
fn decode(bytes: &[u8], identity: &Hash128) -> Option<Module> {
    let (header, payload) = bytes.split_first_chunk::<HEADER>()?;
    if header != identity.as_bytes() {
        return None;
    }
    let module = read(payload).ok()?;
    if module.name() != AMBIENT_MODULE_NAME {
        return None;
    }
    if module.source_hash() != Hash128::compute(ds_stdlib::AMBIENT_SOURCE.as_bytes()) {
        return None;
    }
    if !module.chains().is_empty() {
        return None;
    }
    Some(module)
}

/// The per-bytes splice over freshly decoded bytes: the poisoned-input
/// gates' probe shape, exercising the same validating decode the
/// resident consults.
#[cfg(test)]
fn serve_bytes(bytes: &[u8], identity: &Hash128) -> Option<ds_lower::Served> {
    let module = decode(bytes, identity)?;
    Some(ds_lower::Served {
        tags: module.tags().to_vec(),
        functions: module.into_functions(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The embedded artifact must byte-match a fresh generation over the
    /// current source and compiler: any grammar, checker, lowerer, or
    /// format drift fails here until `cargo run -p ds-cli --example
    /// gen-ambient-dsmod` regenerates it.
    #[test]
    fn the_artifact_matches_a_fresh_generation() {
        let fresh = artifact_bytes(&generate_module(), build_identity());
        assert_eq!(
            ds_stdlib::AMBIENT_DSMOD,
            fresh.as_slice(),
            "the ambient dsmod artifact drifted; regenerate with `cargo run -p ds-cli --example \
             gen-ambient-dsmod`"
        );
    }

    /// The embedded artifact serves in this build: the run path's cold
    /// compile takes the splice, not the fresh ambient lowering.
    #[test]
    fn the_embedded_artifact_serves() {
        let served = serve().expect("the embedded artifact verifies");
        assert!(
            served.functions.len() > 20,
            "the ambient's impl bodies are many; a shrunk serve is a drifted artifact"
        );
        assert_eq!(
            served.functions.last().map(ds_mod::Function::name),
            Some("datetime_str"),
            "each struct's deinit plans right after its own item, so a growing module's\ntable \
             ends on its last item's entries and stays append-stable under live-frame swaps (the \
             merged composition's tail is the @std datetime face, its last record the \
             datetime_str body)",
        );
        assert!(
            served
                .functions
                .iter()
                .any(|f| f.name() == "net_handle$deinit"),
            "the ambient's deinit-carrying structs plan their hooks"
        );
        assert_eq!(
            served.tags.len(),
            generate_module().tags().len(),
            "the served tag rows match the fresh generation's"
        );
    }

    /// Every refusal in the miss matrix, each over bytes the run path
    /// could plausibly hold: a wrong identity (a stale artifact), a
    /// truncated file, a corrupt payload, a foreign module name, a stale
    /// source hash, and a module carrying a chain table the splice does
    /// not install. A module carrying tag rows is the healthy path, not
    /// a miss: the splice seeds the merged lowering's tag table with
    /// them, and a poisoned row set surfaces through the lowering's
    /// [`ds_lower::LowerError::ServedDrift`] (the ids' density and the
    /// duplicate names; the row set as a whole is pinned by the
    /// artifact's identity and the equivalence gates).
    #[test]
    fn every_miss_refuses_the_serve() {
        let identity = build_identity();
        let good = artifact_bytes(&generate_module(), identity);

        // a stale build identity
        assert!(serve_bytes(&good, &Hash128::new([9; 16])).is_none());

        // a truncated artifact
        assert!(serve_bytes(&good[..good.len() - 1], &identity).is_none());

        // a corrupt payload: the header passes, the dsmod decode refuses
        let mut corrupt = good.clone();
        let last = corrupt.len() - 1;
        corrupt[last] ^= 0xff;
        assert!(
            serve_bytes(&corrupt, &identity).is_none(),
            "a flipped payload byte must fail the validating read"
        );

        // a foreign module name
        let module = generate_module();
        let foreign = Module::new(
            "other",
            ds_mod::TargetClass::Bytecode,
            Hash128::compute(ds_stdlib::AMBIENT_SOURCE.as_bytes()),
        )
        .with_function(module.functions()[0].clone());
        assert!(serve_bytes(&artifact_bytes(&foreign, identity), &identity).is_none());

        // a stale source hash
        let stale = Module::new(
            AMBIENT_MODULE_NAME,
            ds_mod::TargetClass::Bytecode,
            Hash128::new([1; 16]),
        )
        .with_function(module.functions()[0].clone());
        assert!(serve_bytes(&artifact_bytes(&stale, identity), &identity).is_none());

        // a module whose chain table the splice does not install
        let chained = module.clone().with_chain(ds_mod::EvidenceChain {
            func: 0,
            next: Vec::new(),
        });
        assert!(serve_bytes(&artifact_bytes(&chained, identity), &identity).is_none());

        // a module whose tag rows ride the splice: the serve hands them to
        // the lowering, whose seeding validates them against the plan
        let tagged = module.clone().with_tag("probe", "a_probe_row", 200);
        assert!(serve_bytes(&artifact_bytes(&tagged, identity), &identity).is_some());

        // the unperturbed bytes still serve
        assert!(serve_bytes(&good, &identity).is_some());
    }

    /// The decode parks once per process: two resident fetches answer
    /// one shared handle (a per-serve decode would mint two), so a
    /// process that compiles many members over one ambient pays the
    /// validating decode once. The park is process state, so one test
    /// process must do both fetches itself.
    #[test]
    fn the_resident_module_parks_one_decode_per_process() {
        let first = resident().expect("the embedded artifact verifies");
        let second = resident().expect("the second fetch serves");
        assert!(
            Arc::ptr_eq(&first, &second),
            "the resident module is one shared handle across fetches"
        );
    }

    /// Every serve answers the resident module's own records: the splice
    /// clones its function records and tag rows off the park, so two
    /// serves answer equal content — the once-per-process decode's
    /// discipline the `.dsi` tables' every-serve gate pins the same way.
    #[test]
    fn every_serve_answers_the_resident_module() {
        let resident = resident().expect("the embedded artifact verifies");
        let first = serve().expect("the first serve clones the resident");
        let second = serve().expect("the second serve clones the resident");
        assert_eq!(first.tags, second.tags, "the tag rows answer equal");
        assert_eq!(
            first.functions.len(),
            second.functions.len(),
            "both serves install the same record count"
        );
        assert_eq!(
            first.functions.len(),
            resident.functions().len(),
            "each serve carries the resident module's whole record set"
        );
        for (served, resident_row) in first.functions.iter().zip(resident.functions()) {
            assert_eq!(served.name(), resident_row.name());
        }
    }
}
