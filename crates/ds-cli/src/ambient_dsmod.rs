//! The checked-in ambient dsmod artifact behind `ds run`'s cold compile:
//! the ambient member's impl bodies pre-lowered to module form, embedded
//! in ds-stdlib beside the AST blob and the `.dsi` tables, so a cold
//! compile lowers only the user's bodies and splices the ambient's in
//! verbatim (`ds_lower::lower_with_served`).
//!
//! The artifact is a pure cache, never correctness-load-bearing: the
//! serving seam validates the bytes before installing anything, and every
//! miss falls back to the fresh in-process lowering with no behavior
//! change. The checks mirror the script cache's identity story —
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

#[cfg(test)]
use ds_mod::Module;
use ds_mod::{Hash128, read};

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
    let analyzed = ds_meta::analyze_owned(&files, vec![std::sync::Arc::new(ast)], &[false]);
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

/// The served ambient splice for this build, when the embedded artifact
/// verifies: the function records a cold compile splices in front of
/// its own table, plus the tag rows those bodies minted. `None` is the
/// miss every caller falls back from.
pub(crate) fn serve() -> Option<ds_lower::Served> {
    serve_bytes(ds_stdlib::AMBIENT_DSMOD, &build_identity())
}

/// The validating serve over artifact bytes: the miss matrix in one
/// place, so the poisoned-input gates exercise exactly what the run path
/// consults.
fn serve_bytes(bytes: &[u8], identity: &Hash128) -> Option<ds_lower::Served> {
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
            Some("min"),
            "each struct's deinit plans right after its own item, so a growing module's\ntable \
             ends on its last item's entries and stays append-stable under live-frame swaps (the \
             merged composition's tail is the @std members' ordering pair)",
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
        let tagged = module.clone().with_tag("a_probe_row", 200);
        assert!(serve_bytes(&artifact_bytes(&tagged, identity), &identity).is_some());

        // the unperturbed bytes still serve
        assert!(serve_bytes(&good, &identity).is_some());
    }
}
