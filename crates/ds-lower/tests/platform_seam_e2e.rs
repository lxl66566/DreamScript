//! The platform seam end to end (H5.5, E1): a package's platform
//! declaration names the platform its world calls address — the lowered
//! FuncRefs carry that name, so the host must install a platform under
//! it (the build-target selection at link time), and the default `std`
//! platform does not serve a package that named its own platform.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{
    FrameAnswer, LoadError, ModuleRegistry, Platform, Value, Vm, loader,
    native::{self, BUILTIN_MODULE},
    platform::{Capability, OP_FILE_SIZE, OP_HTTPS_GET, STD_NAME},
};

/// The probe lowered to a module: parse -> check -> lower -> write ->
/// read back.
fn lowered_module() -> ds_mod::Module {
    let source = include_str!("fixtures/platform_seam_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("platform_seam_probe.ds", source);
    let asts = [parse(&files, id).expect("the fixture parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the fixture checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "platform_seam",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers");
    let bytes = write(lowered.module());
    read(&bytes).expect("the module round-trips")
}

/// The registry every test starts from: the builtin core only.
fn bare_registry() -> ModuleRegistry {
    let mut registry = ModuleRegistry::new();
    registry.install_native(BUILTIN_MODULE, native::builtin_module());
    registry
}

/// The module's pool carries the world calls against the declared
/// platform's name: a bare registry refuses the install, the default
/// `std` platform refuses it too (the package named its own platform),
/// and a platform installed under the declared name serves it.
#[test]
fn world_calls_address_the_declared_platforms_name() {
    let error = loader::install(&mut bare_registry(), lowered_module())
        .map(|_| ())
        .expect_err("a bare VM has no world surface");
    assert!(
        matches!(
            error,
            LoadError::UnresolvedCallee { ref module, .. } if module == "host"
        ),
        "the bare install names the uninstalled platform: {error:?}"
    );

    let mut std_registry = bare_registry();
    std_registry.install_platform(Platform::std());
    let error = loader::install(&mut std_registry, lowered_module())
        .map(|_| ())
        .expect_err("the default platform does not serve `host` call sites");
    assert!(
        matches!(
            error,
            LoadError::UnresolvedCallee { ref module, .. } if module == "host"
        ),
        "the std-only install refuses the host-named call sites: {error:?}"
    );
}

/// A platform installed under the declaration's name serves the world
/// calls: the run prints through the platform's console op and the
/// fsys op answers its span.
#[test]
fn a_platform_under_the_declared_name_serves_the_world_calls() {
    let module = lowered_module();
    let mut registry = bare_registry();
    registry.install_platform(Platform::new("host", &[
        Capability::Console,
        Capability::Fsys,
    ]));
    loader::install(&mut registry, module).expect("the host platform installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    let size = i64::try_from(std::fs::metadata("Cargo.toml").expect("exists").len())
        .expect("the manifest size fits i64");
    let staged = [vm.intern_str("Cargo.toml")];
    assert_eq!(
        vm.run("platform_seam", "size", &staged),
        Ok(FrameAnswer::one(Value::I64(size))),
        "the fsys op answered through the platform's span"
    );
    assert_eq!(vm.captured_output(), b"");
}

/// A platform under the declaration's name that does not serve the
/// called op's capability refuses the install with the structured
/// capability error.
#[test]
fn an_unprovided_capability_refuses_at_install() {
    let module = lowered_module();
    let mut registry = bare_registry();
    registry.install_platform(Platform::new("host", &[Capability::Console]));
    let error = loader::install(&mut registry, module)
        .map(|_| ())
        .expect_err("the console-only platform refuses the fsys call site");
    assert!(
        matches!(
            &error,
            LoadError::UnprovidedPlatformOp {
                platform, capability, index, ..
            } if platform == "host" && capability == "fsys" && *index == OP_FILE_SIZE
        ),
        "the install error names the unprovided capability: {error:?}"
    );
}

/// The default platform name is `std`: the ambient's declaration names
/// it and packages without their own declaration lower against it.
#[test]
fn the_default_platform_name_is_std() {
    assert_eq!(STD_NAME, "std");
}

/// The tls capability's own module: the package declares the https op's
/// extern itself (no ambient spelling exists — the net face's
/// `http_get` routes https URLs onto the tier), lowered through the
/// same parse -> check -> lower -> round-trip pipeline as the probe.
fn tls_module() -> ds_mod::Module {
    let source = include_str!("fixtures/platform_seam_tls.ds");
    let mut files = Files::new();
    let id = files.add_source("platform_seam_tls.ds", source);
    let asts = [parse(&files, id).expect("the fixture parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the fixture checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "platform_seam_tls",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers");
    let bytes = write(lowered.module());
    read(&bytes).expect("the module round-trips")
}

/// The tls capability's call-site validation (H5.5's 2026-11-17
/// ruling): an https call site's FuncRef validates against the host's
/// tls membership at install — a tls-omitting platform under the
/// declaration's name refuses with the structured capability error, the
/// existing refusal diagnostic.
#[test]
fn an_omitted_tls_capability_refuses_the_https_call_site_at_install() {
    let mut registry = bare_registry();
    registry.install_platform(Platform::new("std", &[Capability::Net]));
    let error = loader::install(&mut registry, tls_module())
        .map(|_| ())
        .expect_err("the tls-omitting platform refuses the https call site");
    assert!(
        matches!(
            &error,
            LoadError::UnprovidedPlatformOp {
                platform, capability, index, ..
            } if platform == "std" && capability == "tls" && *index == OP_HTTPS_GET
        ),
        "the install error names the unprovided tls capability: {error:?}"
    );
}

/// The default platform selects the tls capability: the same https call
/// site installs clean (the run itself is the ds-vm TLS fixtures' domain).
#[test]
fn the_std_platform_serves_the_https_call_site() {
    let mut registry = bare_registry();
    registry.install_platform(Platform::std());
    loader::install(&mut registry, tls_module()).expect("the std platform serves the https op");
}
