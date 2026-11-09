//! The two-slot module table [H6 item 5]: versioned module slots and
//! fully-qualified function resolution.

use std::{collections::BTreeMap, sync::Arc};

use ds_abi::{
    EntryDescriptor,
    uop::{DecodeError, Uop},
};

use crate::{
    native::NativeFunction,
    program::{Const, ConstKind, FunctionDef},
};

/// Why a module version cannot be loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModuleBuildError {
    /// Two functions in one version share a name.
    DuplicateFunction(String),
    /// A function's pool exceeds the u16 index space the encoding addresses.
    PoolTooLarge { func: String, len: usize },
    /// A body word does not decode: the stream is not canonical.
    BodyDecode {
        /// The function whose body is malformed.
        func: String,
        /// The instruction index of the malformed word.
        pc: u32,
        /// Why the word does not decode.
        cause: DecodeError,
    },
    /// A `FUNCF` word disagrees with the function's frame layout slot count.
    FuncfMismatch {
        /// The function whose entry disagrees.
        func: String,
        /// The `FUNCF` operand.
        funcef: u16,
        /// The layout's slot count.
        layout: usize,
    },
    /// A pool-index operand is out of range for its function's pool.
    PoolIndex {
        /// The function whose body addresses the pool.
        func: String,
        /// The offending pool index.
        kidx: u16,
    },
    /// A pool entry is loaded by an opcode of the wrong kind.
    PoolKind {
        /// The function whose pool holds the entry.
        func: String,
        /// The offending pool index.
        kidx: u16,
        /// The kind the opcode requires.
        expected: ConstKind,
        /// The kind the entry holds.
        found: ConstKind,
    },
    /// A RETSPAN addresses slots past its function's frame: the span must
    /// fit inside the callee's own slots (`src + n <= slots`), which is a
    /// static layout fact.
    RetSpanLayout {
        /// The function whose body carries the op.
        func: String,
        /// The span's first slot.
        src: u8,
        /// The span's word count.
        span: u8,
        /// The function's slot count.
        slots: usize,
    },
}

/// A loaded function: definition plus its two-word entry descriptor [H2].
/// The tier-2 word is null until the JIT exists, so
/// [`EntryDescriptor::resolve`] answers with word 0: this function's index
/// in the module version's function table.
#[derive(Debug)]
pub struct Function {
    /// The function's name within its module.
    pub name: String,
    /// The function's static frame layout [BL-6].
    pub layout: ds_abi::FrameLayout,
    /// The function's constant pool (the dsmod blocks-payload granularity).
    pub pool: Vec<Const>,
    /// The encoded uop body [BL-1].
    pub code: Vec<u32>,
    /// The two-word entry descriptor; word 0 is this function's table index.
    pub entry: EntryDescriptor,
}

/// One immutable version of a module: a name, a function table, and its
/// name index. Every function body is decode-checked once at load, its
/// `FUNCF` cross-checked against its layout, and every pool-index operand
/// checked against its pool's kinds, so a bad stream never reaches the
/// dispatcher.
#[derive(Debug)]
pub struct ModuleVersion {
    name: String,
    functions: Vec<Function>,
    index: BTreeMap<String, u32>,
}

impl ModuleVersion {
    /// Loads a version: rejects duplicate names, decode-checks every body
    /// word, cross-checks each `FUNCF` against its layout, validates every
    /// pool access, and assigns function-table indices (which become entry
    /// descriptor word 0). The tier-2 word stays null; it flips exactly once
    /// when the JIT compiles the function [H2].
    pub fn new(name: impl Into<String>, defs: Vec<FunctionDef>) -> Result<Self, ModuleBuildError> {
        let mut functions = Vec::with_capacity(defs.len());
        let mut index = BTreeMap::new();
        for (position, def) in defs.into_iter().enumerate() {
            let func_id = u32::try_from(position).expect("function table exceeds u32");
            if index.insert(def.name.clone(), func_id).is_some() {
                return Err(ModuleBuildError::DuplicateFunction(def.name));
            }
            validate_body(&def)?;
            functions.push(Function {
                entry: EntryDescriptor::new_interpreted(position),
                name: def.name,
                layout: def.layout,
                pool: def.pool,
                code: def.code,
            });
        }
        Ok(Self {
            name: name.into(),
            functions,
            index,
        })
    }

    /// The module's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// All functions in table order; the position is entry word 0.
    #[must_use]
    pub fn functions(&self) -> &[Function] {
        &self.functions
    }

    /// Looks a function up by name.
    #[must_use]
    pub fn function(&self, name: &str) -> Option<&Function> {
        let &func_id = self.index.get(name)?;
        self.functions.get(func_id as usize)
    }
}

/// Load-time body validation: canonical decode, FUNCF-vs-layout agreement,
/// and pool-index/kind agreement [BL-5, BL-6].
fn validate_body(def: &FunctionDef) -> Result<(), ModuleBuildError> {
    if def.pool.len() > usize::from(u16::MAX) + 1 {
        return Err(ModuleBuildError::PoolTooLarge {
            func: def.name.clone(),
            len: def.pool.len(),
        });
    }
    for (pc, &word) in def.code.iter().enumerate() {
        let pc = u32::try_from(pc).expect("pc fits u32");
        let op = Uop::decode(pc, word).map_err(|cause| ModuleBuildError::BodyDecode {
            func: def.name.clone(),
            pc,
            cause,
        })?;
        if let Uop::Funcf { frame_slots } = op {
            if usize::from(frame_slots) != def.layout.slots().len() {
                return Err(ModuleBuildError::FuncfMismatch {
                    func: def.name.clone(),
                    funcef: frame_slots,
                    layout: def.layout.slots().len(),
                });
            }
        }
        // the RETSPAN span is a static frame-layout fact: it must fit
        // inside the callee's own slots
        if let Uop::RetSpan { src, n } = op {
            if usize::from(src) + usize::from(n) > def.layout.slots().len() {
                return Err(ModuleBuildError::RetSpanLayout {
                    func: def.name.clone(),
                    src,
                    span: n,
                    slots: def.layout.slots().len(),
                });
            }
        }
        // which pool kinds each loading opcode accepts, and the primary
        // kind its trap names: KIMM loads i64 entries, KCONST the non-i64
        // constant words (this VM executes f64 and bool), KSTR the string
        // entries, CALL/CALLT the function references
        let (kidx, accepts, expected): (_, &[_], _) = match op {
            Uop::Kimm { kidx, .. } => (kidx, &[ConstKind::I64], ConstKind::I64),
            Uop::Kconst { kidx, .. } => (kidx, &[ConstKind::F64, ConstKind::Bool], ConstKind::F64),
            Uop::Kstr { kidx, .. } => (kidx, &[ConstKind::Str], ConstKind::Str),
            Uop::Call { callee, .. } | Uop::Callt { callee, .. } => {
                (callee, &[ConstKind::FuncRef], ConstKind::FuncRef)
            },
            _ => continue,
        };
        let found = def
            .pool
            .get(usize::from(kidx.0))
            .ok_or(ModuleBuildError::PoolIndex {
                func: def.name.clone(),
                kidx: kidx.0,
            })?;
        if !accepts.contains(&found.kind()) {
            return Err(ModuleBuildError::PoolKind {
                func: def.name.clone(),
                kidx: kidx.0,
                expected,
                found: found.kind(),
            });
        }
    }
    Ok(())
}

#[derive(Debug)]
pub struct ModuleSlots {
    current: Arc<ModuleVersion>,
    retiring: Option<Arc<ModuleVersion>>,
}

impl ModuleSlots {
    /// The version qualified calls resolve through.
    #[must_use]
    pub fn current(&self) -> &ModuleVersion {
        &self.current
    }

    /// The previous version, if any frames may still run it.
    #[must_use]
    pub fn retiring(&self) -> Option<&ModuleVersion> {
        self.retiring.as_deref()
    }
}

/// The module registry: every loaded module's two slots, plus the native
/// modules. The VM owns one and resolves every call through the current
/// slot, so a swap changes the destination of subsequent calls immediately;
/// frames already running a retiring version keep their code and pool until
/// they return (their frames hold an `Arc` to the version they entered)
/// [H6 item 5]. Native modules ([`NativeFunction`] tables, the builtin
/// mechanism) live in the same name space: a pool FuncRef resolves through
/// the uop tables first, the native tables second, so `CALL` has one
/// resolution path for both callee kinds.
#[derive(Debug, Default)]
pub struct ModuleRegistry {
    modules: BTreeMap<String, ModuleSlots>,
    natives: BTreeMap<String, Arc<Vec<NativeFunction>>>,
}

impl ModuleRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Installs a version: it becomes the module's current slot, the previous
    /// current moves to the retiring slot, and an older retiring version is
    /// retired (dropped). Returns the shared handle to the installed version.
    pub fn install(&mut self, version: ModuleVersion) -> Arc<ModuleVersion> {
        let installed = Arc::new(version);
        match self.modules.get_mut(&installed.name) {
            Some(slots) => {
                slots.retiring = Some(std::mem::replace(
                    &mut slots.current,
                    Arc::clone(&installed),
                ));
            },
            None => {
                self.modules.insert(installed.name.clone(), ModuleSlots {
                    current: Arc::clone(&installed),
                    retiring: None,
                });
            },
        }
        installed
    }

    /// Resolves a function by name through the module's current slot, for
    /// run entries: the version plus the function's table index.
    #[must_use]
    pub fn resolve(&self, module: &str, func: &str) -> Option<(Arc<ModuleVersion>, u32)> {
        let version = &self.modules.get(module)?.current;
        let &func_id = version.index.get(func)?;
        Some((Arc::clone(version), func_id))
    }

    /// Resolves a pool FuncRef through the target module's current slot
    /// [H6 item 5]: `func` is the function-table index (entry word 0). A
    /// swap between a call's issue and its resolution redirects here.
    #[must_use]
    pub fn resolve_indexed(&self, module: &str, func: u32) -> Option<(Arc<ModuleVersion>, u32)> {
        let version = &self.modules.get(module)?.current;
        if usize::try_from(func).is_ok_and(|i| i < version.functions.len()) {
            Some((Arc::clone(version), func))
        } else {
            None
        }
    }

    /// A module's two slots, if the module is loaded.
    #[must_use]
    pub fn slots(&self, module: &str) -> Option<&ModuleSlots> {
        self.modules.get(module)
    }

    /// Installs a native module: a name plus its function table, whose
    /// indices pool FuncRefs address. A second install under the same name
    /// replaces the table (the same replacement discipline as a uop
    /// version's swap, without the retiring slot — native code holds no
    /// live frames).
    pub fn install_native(&mut self, name: &str, functions: Vec<NativeFunction>) {
        self.natives.insert(name.to_owned(), Arc::new(functions));
    }

    /// Resolves a pool FuncRef against the native modules: the function
    /// at `func`'s index, if the module is installed and the index is in
    /// range.
    #[must_use]
    pub fn resolve_native(&self, module: &str, func: u32) -> Option<&NativeFunction> {
        let fns = self.natives.get(module)?;
        (usize::try_from(func).is_ok_and(|i| i < fns.len())).then(|| &fns[func as usize])
    }
}

#[cfg(test)]
mod tests {
    use ds_abi::uop::Uop;

    use super::*;
    use crate::program::assemble;

    fn empty_def(name: &str) -> FunctionDef {
        FunctionDef {
            name: name.into(),
            layout: ds_abi::FrameLayout::new(Vec::new(), 0, 0).expect("valid layout"),
            pool: Vec::new(),
            code: Vec::new(),
        }
    }

    #[test]
    fn load_writes_entry_word_zero_once() {
        let version =
            ModuleVersion::new("m", vec![empty_def("a"), empty_def("b")]).expect("valid module");
        for (position, f) in version.functions().iter().enumerate() {
            // word 0 is the table index, immutable after load; word 1 is the
            // null sentinel, so resolve() answers with word 0
            assert_eq!(f.entry.tier(), ds_abi::Tier::Interpreted);
            assert_eq!(f.entry.resolve(), position);
        }
        assert_eq!(version.function("b").map(|f| f.entry.resolve()), Some(1));
    }

    #[test]
    fn duplicate_names_are_rejected() {
        assert_eq!(
            ModuleVersion::new("m", vec![empty_def("a"), empty_def("a")]).unwrap_err(),
            ModuleBuildError::DuplicateFunction("a".into())
        );
    }

    #[test]
    fn non_canonical_body_words_are_rejected_at_load() {
        // reserved major 0xE: decode fails, so the loader refuses the stream
        let mut def = empty_def("bad");
        def.code = vec![0xe0_00_00_00];
        assert_eq!(
            ModuleVersion::new("m", vec![def]).unwrap_err(),
            ModuleBuildError::BodyDecode {
                func: "bad".into(),
                pc: 0,
                cause: DecodeError::UnknownOpcode {
                    major: 0xe,
                    minor: 0
                },
            }
        );
    }

    #[test]
    fn funcf_disagreement_with_the_layout_is_rejected_at_load() {
        let mut def = empty_def("bad");
        def.layout =
            ds_abi::FrameLayout::new(vec![ds_abi::SlotType::I64], 0, 0).expect("valid layout");
        def.code = assemble(&[Uop::Funcf { frame_slots: 2 }]).expect("encodes");
        assert_eq!(
            ModuleVersion::new("m", vec![def]).unwrap_err(),
            ModuleBuildError::FuncfMismatch {
                func: "bad".into(),
                funcef: 2,
                layout: 1,
            }
        );
    }

    #[test]
    fn pool_kind_and_index_disagreements_are_rejected_at_load() {
        // KIMM addressing a bool entry
        let mut def = empty_def("k");
        def.layout =
            ds_abi::FrameLayout::new(vec![ds_abi::SlotType::Bool], 0, 0).expect("valid layout");
        def.pool = vec![Const::Bool(true)];
        def.code = assemble(&[Uop::Funcf { frame_slots: 1 }, Uop::Kimm {
            dst: 0,
            kidx: ds_abi::ConstIdx(0),
        }])
        .expect("encodes");
        assert_eq!(
            ModuleVersion::new("m", vec![def]).unwrap_err(),
            ModuleBuildError::PoolKind {
                func: "k".into(),
                kidx: 0,
                expected: ConstKind::I64,
                found: ConstKind::Bool,
            }
        );
        // a pool index past the end
        let mut def = empty_def("p");
        def.code = assemble(&[Uop::Kimm {
            dst: 0,
            kidx: ds_abi::ConstIdx(3),
        }])
        .expect("encodes");
        assert_eq!(
            ModuleVersion::new("m", vec![def]).unwrap_err(),
            ModuleBuildError::PoolIndex {
                func: "p".into(),
                kidx: 3,
            }
        );
    }

    #[test]
    fn assemble_and_load_round_trip_the_record_sum_to_body() {
        // the record's 13.7 sum_to body, with the executable corrections of
        // the fixture suite: JMPIF exit, separate constant scratch slots
        let body = [
            Uop::Funcf { frame_slots: 5 },
            Uop::Kimm {
                dst: 1,
                kidx: ds_abi::ConstIdx(0),
            },
            Uop::Kimm {
                dst: 3,
                kidx: ds_abi::ConstIdx(0),
            },
            Uop::LeI64 {
                dst: 2,
                lhs: 0,
                rhs: 3,
            },
            Uop::Jmpif { cond: 2, target: 9 },
            Uop::AddI64 {
                dst: 1,
                lhs: 1,
                rhs: 0,
            },
            Uop::Kimm {
                dst: 4,
                kidx: ds_abi::ConstIdx(1),
            },
            Uop::SubI64 {
                dst: 0,
                lhs: 0,
                rhs: 4,
            },
            Uop::Jmp { target: 3 },
            Uop::Ret { src: 1 },
        ];
        let words = assemble(&body).expect("canonical body assembles");
        let version = ModuleVersion::new("mut", vec![FunctionDef {
            name: "sum_to".into(),
            layout: ds_abi::FrameLayout::new(
                vec![
                    ds_abi::SlotType::I64,
                    ds_abi::SlotType::I64,
                    ds_abi::SlotType::Bool,
                    ds_abi::SlotType::I64,
                    ds_abi::SlotType::I64,
                ],
                1,
                0,
            )
            .expect("valid layout"),
            pool: vec![Const::I64(0), Const::I64(1)],
            code: words,
        }])
        .expect("valid module");
        assert_eq!(version.function("sum_to").map(|f| f.pool.len()), Some(2));
    }
}
