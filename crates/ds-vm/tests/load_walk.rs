//! The deps-first load protocol pinned: deterministic diamond order and
//! ids, content-hash validation on fetch and against installed slots,
//! provider misses, cycle refusal, self-named dep entries staying local,
//! and hot-swap composition through the current slot.

use ds_mod::{Constant, FuncRefKey, Function, Hash128, Module, TargetClass};
use ds_vm::{
    Const, FrameAnswer, FunctionDef, LoadError, ModuleRegistry, ModuleSlots, ModuleVersion, Value,
    Vm,
    abi::{ConstIdx, FrameLayout, SlotType, SlotType::I64, uop::Uop},
    assemble,
};

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn layout(slots: &[SlotType], params: usize, args: usize) -> FrameLayout {
    FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout")
}

/// A leaf module whose `answer` function returns a constant: bodies are
/// irrelevant to walk order, so every fixture module is this shape.
fn leaf(name: &str, source: &[u8], answer: i64) -> Module {
    let answer = Function::new(
        "answer",
        layout(&[I64], 0, 0),
        vec![Constant::I64(answer)],
        vec![
            Uop::Funcf { frame_slots: 1 },
            Uop::Kimm { dst: 0, kidx: k(0) },
            Uop::Ret { src: 0 },
        ],
    )
    .expect("valid function");
    Module::new(name, TargetClass::Bytecode, Hash128::compute(source)).with_function(answer)
}

/// The `lib` versions for the swap test: `inc(x) = x + addend`.
fn inc_module(source: &[u8], addend: i64) -> Module {
    let inc = Function::new(
        "inc",
        layout(&[I64, I64], 1, 0),
        vec![Constant::I64(addend)],
        vec![
            Uop::Funcf { frame_slots: 2 },
            Uop::Kimm { dst: 1, kidx: k(0) },
            Uop::AddI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            },
            Uop::Ret { src: 0 },
        ],
    )
    .expect("valid function");
    Module::new("lib", TargetClass::Bytecode, Hash128::compute(source)).with_function(inc)
}

/// The dependent: `main` stages 5 and calls the first dep entry's function
/// 0, halting on the overlaid result.
fn app_calling_first_dep(dep: &str, linked: Hash128) -> Module {
    let main = Function::new(
        "main",
        layout(&[I64], 0, 1),
        vec![Constant::I64(5), Constant::FuncRef {
            key: FuncRefKey::Dep(1),
            func: 0,
        }],
        vec![
            Uop::Funcf { frame_slots: 1 },
            Uop::Kimm { dst: 0, kidx: k(0) },
            Uop::Call {
                callee: k(1),
                nargs: 1,
            },
            Uop::Halt { slot: 0 },
        ],
    )
    .expect("valid function");
    Module::new("app", TargetClass::Bytecode, Hash128::compute(b"app"))
        .with_dep(dep, linked)
        .with_function(main)
}

/// A provider answering from a consumed list: each served module is
/// removed, so a second request for a name is a miss.
fn serving(modules: Vec<Module>) -> impl FnMut(&str, Hash128) -> Option<Module> {
    let mut modules = modules;
    move |name, _linked| {
        modules
            .iter()
            .position(|module| module.name() == name)
            .map(|at| modules.remove(at))
    }
}

/// The diamond installs depth-first in dep-list order: first-install ids
/// are the post-order completion sequence (base, left, right, app), and
/// the shared base installs exactly once.
#[test]
fn diamond_installs_depth_first_with_deterministic_ids() {
    let base = leaf("base", b"base-src", 1);
    let left = leaf("left", b"left-src", 2).with_dep("base", Hash128::compute(b"base-src"));
    let right = leaf("right", b"right-src", 3).with_dep("base", Hash128::compute(b"base-src"));
    let app = leaf("app", b"app-src", 4)
        .with_dep("left", Hash128::compute(b"left-src"))
        .with_dep("right", Hash128::compute(b"right-src"));
    let mut vm = Vm::new(ModuleRegistry::new());
    let mut fetches: Vec<String> = Vec::new();
    let mut pool = vec![left, right, base];
    let installed_row = vm
        .load(app, |name, _linked| {
            fetches.push(name.to_owned());
            pool.iter()
                .position(|module| module.name() == name)
                .map(|at| pool.remove(at))
        })
        .expect("the diamond loads");
    assert_eq!(
        vm.registry()
            .version(installed_row)
            .map(ModuleVersion::name),
        Some("app")
    );
    // left's subtree installs first (base inside it), right's link then
    // validates base against its installed slot
    assert_eq!(fetches, vec![
        "left".to_owned(),
        "base".to_owned(),
        "right".to_owned()
    ]);
    assert_eq!(vm.registry().module_id("base"), Some(0));
    assert_eq!(vm.registry().module_id("left"), Some(1));
    assert_eq!(vm.registry().module_id("right"), Some(2));
    assert_eq!(vm.registry().module_id("app"), Some(3));
}

/// A dep the root names again after an earlier sibling's subtree already
/// installed it resolves through its slot: fetched once, installed once
/// (no retiring row, one generation bump per module), ids in depth-first
/// order with the shared dep first.
#[test]
fn a_later_link_validates_against_the_earlier_subtree_s_install() {
    let c = leaf("c", b"c-src", 1);
    let a = leaf("a", b"a-src", 2).with_dep("c", Hash128::compute(b"c-src"));
    let app = leaf("app", b"app-src", 3)
        .with_dep("a", Hash128::compute(b"a-src"))
        .with_dep("c", Hash128::compute(b"c-src"));
    let mut vm = Vm::new(ModuleRegistry::new());
    let generation = vm.registry().generation();
    let mut fetches: Vec<String> = Vec::new();
    let mut pool = vec![a, c];
    vm.load(app, |name, _linked| {
        fetches.push(name.to_owned());
        pool.iter()
            .position(|module| module.name() == name)
            .map(|at| pool.remove(at))
    })
    .expect("loads");
    // a's subtree fetches c; the root's own c link never fetches
    assert_eq!(fetches, vec!["a".to_owned(), "c".to_owned()]);
    // exactly three installs (c, a, app): a second c install would bump
    // the generation again and leave a retiring row
    assert_eq!(vm.registry().generation(), generation + 3);
    assert_eq!(
        vm.registry().slots("c").and_then(ModuleSlots::retiring_idx),
        None
    );
    assert_eq!(vm.registry().module_id("c"), Some(0));
    assert_eq!(vm.registry().module_id("a"), Some(1));
    assert_eq!(vm.registry().module_id("app"), Some(2));
}

/// The same shape with the shared dep two levels down the earlier
/// sibling: one fetch, one install, depth-first ids.
#[test]
fn a_shared_dep_two_levels_down_installs_once() {
    let c = leaf("c", b"c-src", 1);
    let mid = leaf("mid", b"mid-src", 2).with_dep("c", Hash128::compute(b"c-src"));
    let side = leaf("side", b"side-src", 3).with_dep("mid", Hash128::compute(b"mid-src"));
    let app = leaf("app", b"app-src", 4)
        .with_dep("side", Hash128::compute(b"side-src"))
        .with_dep("c", Hash128::compute(b"c-src"));
    let mut vm = Vm::new(ModuleRegistry::new());
    let generation = vm.registry().generation();
    let mut fetches: Vec<String> = Vec::new();
    let mut pool = vec![side, mid, c];
    vm.load(app, |name, _linked| {
        fetches.push(name.to_owned());
        pool.iter()
            .position(|module| module.name() == name)
            .map(|at| pool.remove(at))
    })
    .expect("loads");
    assert_eq!(fetches, vec![
        "side".to_owned(),
        "mid".to_owned(),
        "c".to_owned()
    ]);
    assert_eq!(vm.registry().generation(), generation + 4);
    assert_eq!(
        vm.registry().slots("c").and_then(ModuleSlots::retiring_idx),
        None
    );
    assert_eq!(vm.registry().module_id("c"), Some(0));
    assert_eq!(vm.registry().module_id("mid"), Some(1));
    assert_eq!(vm.registry().module_id("side"), Some(2));
    assert_eq!(vm.registry().module_id("app"), Some(3));
}

/// A provider-fetched module whose source hash differs from the linked
/// content hash refuses the load before anything installs.
#[test]
fn a_fetched_dep_whose_source_hash_disagrees_with_the_link_refuses() {
    let lib = leaf("lib", b"lib-actual", 1);
    let app = app_calling_first_dep("lib", Hash128::compute(b"lib-other"));
    let mut vm = Vm::new(ModuleRegistry::new());
    assert_eq!(
        vm.load(app, serving(vec![lib])).unwrap_err(),
        LoadError::HashMismatch {
            dep: "lib".into(),
            dependent: "app".into(),
            linked: Hash128::compute(b"lib-other"),
            found: Some(Hash128::compute(b"lib-actual")),
        }
    );
    assert_eq!(vm.registry().module_id("lib"), None);
    assert_eq!(vm.registry().module_id("app"), None);
}

/// An installed dep validates by its current slot's recorded hash: a
/// mismatch refuses, a slot recording no hash refuses (it cannot confirm
/// the linked content), and a match loads without consulting the
/// provider.
#[test]
fn an_installed_dep_validates_by_its_recorded_hash() {
    let mut vm = Vm::new(ModuleRegistry::new());
    vm.install_dsmod(leaf("lib", b"lib-src", 1))
        .expect("lib installs");
    // the linked hash disagrees with the recorded one
    let app = app_calling_first_dep("lib", Hash128::compute(b"lib-other"));
    assert_eq!(
        vm.load(app, serving(vec![])).unwrap_err(),
        LoadError::HashMismatch {
            dep: "lib".into(),
            dependent: "app".into(),
            linked: Hash128::compute(b"lib-other"),
            found: Some(Hash128::compute(b"lib-src")),
        }
    );
    // a directly built host version records no hash: unverifiable refuses
    vm.install_module(
        ModuleVersion::new("hostmod", vec![FunctionDef {
            name: "answer".into(),
            layout: layout(&[I64], 0, 0),
            pool: vec![Const::I64(1)],
            code: assemble(&[
                Uop::Funcf { frame_slots: 1 },
                Uop::Kimm { dst: 0, kidx: k(0) },
                Uop::Ret { src: 0 },
            ])
            .expect("encodes"),
        }])
        .expect("valid module"),
    );
    let app = app_calling_first_dep("hostmod", Hash128::compute(b"hostmod-src"));
    assert_eq!(
        vm.load(app, serving(vec![])).unwrap_err(),
        LoadError::HashMismatch {
            dep: "hostmod".into(),
            dependent: "app".into(),
            linked: Hash128::compute(b"hostmod-src"),
            found: None,
        }
    );
    // a matching recorded hash loads with a provider that answers nothing
    let app = app_calling_first_dep("lib", Hash128::compute(b"lib-src"));
    vm.load(app, |_, _| None)
        .expect("the recorded hash validates without a fetch");
}

/// A provider miss names the dependency and its dependent, and installs
/// nothing.
#[test]
fn a_provider_miss_names_the_dep_and_dependent_in_list_order() {
    let app = leaf("app", b"app", 1)
        .with_dep("gone", Hash128::ZERO)
        .with_dep("also-gone", Hash128::ZERO);
    let mut vm = Vm::new(ModuleRegistry::new());
    // links resolve in dep-list order, so the first missing dep reports
    assert_eq!(
        vm.load(app, |_, _| None).unwrap_err(),
        LoadError::ProviderMiss {
            dep: "gone".into(),
            dependent: "app".into(),
        }
    );
    assert_eq!(vm.registry().module_id("gone"), None);
    assert_eq!(vm.registry().module_id("app"), None);
}

/// A dep entry naming a module still under construction refuses with the
/// cycle's path, before the provider is consulted for it.
#[test]
fn a_dep_cycle_refuses_with_the_cycle_path() {
    let b = leaf("b", b"b-src", 2).with_dep("a", Hash128::compute(b"a-src"));
    let a = leaf("a", b"a-src", 1).with_dep("b", Hash128::compute(b"b-src"));
    let mut vm = Vm::new(ModuleRegistry::new());
    assert_eq!(
        vm.load(a, serving(vec![b])).unwrap_err(),
        LoadError::DepCycle {
            cycle: vec!["a".into(), "b".into(), "a".into()],
        }
    );
    assert_eq!(vm.registry().module_id("a"), None);
    assert_eq!(vm.registry().module_id("b"), None);
}

/// A dep entry naming the enclosing module resolves against the module's
/// own table (the install's own-table path) and is never fetched: the
/// provider that answers nothing still loads, and the call runs.
#[test]
fn a_self_named_dep_entry_resolves_locally_and_is_never_fetched() {
    let target = Function::new(
        "target",
        layout(&[I64, I64], 1, 0),
        vec![Constant::I64(7)],
        vec![
            Uop::Funcf { frame_slots: 2 },
            Uop::Kimm { dst: 1, kidx: k(0) },
            Uop::Ret { src: 1 },
        ],
    )
    .expect("valid function");
    let go = Function::new(
        "go",
        layout(&[I64], 0, 1),
        vec![Constant::I64(0), Constant::FuncRef {
            key: FuncRefKey::Dep(1), // the first dep entry names "m" itself
            func: 0,
        }],
        vec![
            Uop::Funcf { frame_slots: 1 },
            Uop::Kimm { dst: 0, kidx: k(0) },
            Uop::Call {
                callee: k(1),
                nargs: 1,
            },
            Uop::Halt { slot: 0 },
        ],
    )
    .expect("valid function");
    let module = Module::new("m", TargetClass::Bytecode, Hash128::compute(b"m"))
        .with_dep("m", Hash128::ZERO)
        .with_function(target)
        .with_function(go);
    let mut vm = Vm::new(ModuleRegistry::new());
    vm.load(module, |_, _| None)
        .expect("the self-named entry never reaches the provider");
    assert_eq!(vm.run("m", "go", &[]), Ok(FrameAnswer::one(Value::I64(7))));
}

/// A loaded dependent runs across its dep edge, and a later swap of the
/// dep under the same name redirects the dependent's calls while the id
/// stays put.
#[test]
fn a_dep_swap_redirects_a_loaded_dependent_with_the_id_stable() {
    let v1 = inc_module(b"lib-v1", 10);
    let app = app_calling_first_dep("lib", Hash128::compute(b"lib-v1"));
    let mut vm = Vm::new(ModuleRegistry::new());
    vm.load(app, serving(vec![v1])).expect("loads over v1");
    let id = vm.registry().module_id("lib").expect("lib installed");
    assert_eq!(
        vm.run("app", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(15)))
    );
    // the same FuncRef keeps resolving through the name's current slot
    vm.install_dsmod(inc_module(b"lib-v2", 100))
        .expect("v2 swaps in");
    assert_eq!(
        vm.run("app", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(105)))
    );
    assert_eq!(vm.registry().module_id("lib"), Some(id));
}
