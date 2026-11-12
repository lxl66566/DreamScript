//! Per-fiber claim ledgers: the multiset of heap counts a fiber's frames
//! own at each moment — the substrate the teardown drain walk releases
//! (the cross-heap accounting channel of the counts-stay ruling), plus
//! the deinit-claim directory the teardown hook pass derives from each
//! function body's own claim accounting (E5: teardown walks frames
//! running deinits).
//!
//! Every count a frame acquires enters its fiber's ledger; every release
//! (a frame's drop, a delivery to another fiber or the host) leaves it.
//! The ledger tracks counts, not frame slots: the placement pass moves
//! words between slots without touching counts (a dup pairs its `RCINC`
//! with a later `RCDEC`, a forward rides the callee's own exit drops),
//! so no dispatch site needs to know which staged copy anchors a claim.

use std::{
    collections::{HashMap, hash_map::Entry},
    hash::{BuildHasherDefault, Hasher},
    sync::Arc,
};

use ds_abi::{Inst, Uop};

use crate::{
    fiber::{FIBER_ROOT, FiberId},
    module::ModuleVersion,
    program::Const,
    value::RefId,
};

/// The name suffix of a struct's synthesized deinit function (C5): ds-lower
/// names it `<struct>$deinit`, and `$` sits outside the identifier charset,
/// so the suffix names synthesized deinits and nothing else — the directory
/// scan's callee test. Same mangle family as the impl `<t>#<x>#<m>` names.
pub(crate) const DEINIT_SUFFIX: &str = "$deinit";

/// The claim-band tags of the teardown establishment marker, mirrored from
/// ds-lower's tag reservation (crates/ds-lower/src/shape.rs, where the mint
/// ceiling keeps every user tag id below them): a claim's dedicated band
/// word holds SET from its registration to its first death path or
/// consuming move, CLEAR after. The reserved values are what make a stale
/// word in a recycled frame region unable to spoof a set band.
pub(crate) const CLAIM_BAND_SET: u8 = u8::MAX - 8;
pub(crate) const CLAIM_BAND_CLEAR: u8 = u8::MAX - 9;

/// One deinit-claim site of a function body, derived from the body's own
/// claim accounting: the drop the placement pass emits for a deinit value
/// stages the value's whole span (a run of `MOV`s into the overlay, one
/// per word) and calls the synthesized deinit, so the staging run plus the
/// call recover the claim — the span's base body slot, its width, and the
/// hook's function-table index — with no format change: the directory is
/// decoded from the module's own words, exactly what a drop site executes.
/// A lowered drop also clears the claim's establishment band right after
/// the call, so the site carries the band's body slot too — the liveness
/// witness the doors read off the frame without the claim ledger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HookSite {
    /// The claim's base body slot (the span's first word).
    pub(crate) base: u8,
    /// The span's word count (the hook's parameter count).
    pub(crate) width: u8,
    /// The function-table index of the `$deinit` function the drop calls.
    pub(crate) hook: u32,
    /// The establishment band's body slot, recovered from the drop's own
    /// clear op; `None` for a shape without one (a hand-assembled
    /// fixture, a foreign module) — those sites keep the ledger witness.
    pub(crate) band: Option<u8>,
}

/// Derives one function's deinit-claim sites from its body: every `CALL`
/// whose callee is this module's own `$deinit`-named function, preceded
/// contiguously by the exact staging run that drop emits (`MOV overlay(i)
/// <- body(base+i)` for each `i` below the argument count). Sites come out
/// in first-occurrence stream order — the deterministic fire order of the
/// teardown pass — deduped per (base, hook): one claim has many drop sites
/// (one per exit path), all naming the same span and hook.
pub(crate) fn hook_sites(version: &ModuleVersion, func: u32) -> Arc<Vec<HookSite>> {
    let f = &version.functions()[func as usize];
    let body_len = f.layout.slots().len() - f.layout.arg_area_len();
    let mut sites = Vec::new();
    for (pc, &word) in f.code.iter().enumerate() {
        let Ok(Inst::Uop(Uop::Call { callee, nargs })) = Inst::decode(pc_as_u32(pc), word) else {
            continue;
        };
        let width = usize::from(nargs);
        // the callee must be this module's own synthesized deinit: the
        // pool FuncRef names this version, and the named function carries
        // the mangled suffix no user name can spell
        let Some(Const::FuncRef { module, func: hook }) = f.pool.get(usize::from(callee.0)) else {
            continue;
        };
        let Some(hook_fn) = usize::try_from(*hook)
            .ok()
            .and_then(|i| version.functions().get(i))
        else {
            continue;
        };
        if module != version.name() || !hook_fn.name.ends_with(DEINIT_SUFFIX) {
            continue;
        }
        // the contiguous staging run sits immediately before the call
        if pc < width {
            continue;
        }
        let Some(base) = staging_base(&f.code[pc - width..pc], body_len) else {
            continue;
        };
        let site = HookSite {
            base,
            width: nargs,
            hook: *hook,
            band: band_slot(f.code.get(pc + 1).copied(), body_len),
        };
        if !sites.contains(&site) {
            sites.push(site);
        }
    }
    Arc::new(sites)
}

/// The base slot of one staging run, or `None` when the words are not the
/// drop shape: `MOV overlay(i) <- body(base+i)` for every `i` in order,
/// the sources consecutive body words.
fn staging_base(run: &[u32], body_len: usize) -> Option<u8> {
    let mut base = None;
    for (i, &word) in run.iter().enumerate() {
        let Ok(Inst::Uop(Uop::Mov { dst, src })) = Inst::decode(0, word) else {
            return None;
        };
        if usize::from(dst) != body_len + i || usize::from(src) >= body_len {
            return None;
        }
        match base {
            None => base = Some(src),
            Some(first) if usize::from(src) == usize::from(first) + i => {},
            Some(_) => return None,
        }
    }
    base
}

/// The establishment band's body slot, decoded from the clear op that
/// immediately follows a lowered drop's `CALL` (`TagNew` writing the
/// reserved CLEAR tag into a body word), or `None` when the word is not
/// that op — a hand-assembled drop shape — whose site keeps the claim
/// ledger's count witness instead.
fn band_slot(word: Option<u32>, body_len: usize) -> Option<u8> {
    let Ok(Inst::Uop(Uop::TagNew { dst, tag })) = Inst::decode(0, word?) else {
        return None;
    };
    (tag == CLAIM_BAND_CLEAR && usize::from(dst) < body_len).then_some(dst)
}

fn pc_as_u32(pc: usize) -> u32 {
    u32::try_from(pc).expect("pc fits u32")
}

/// A one-multiply hasher: claim keys are single 64-bit word identities,
/// so ledger traffic stays a handful of naniseconds on the count
/// funnels.
#[derive(Default)]
struct WordHasher(u64);

impl Hasher for WordHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.write_u64(u64::from(byte));
        }
    }

    fn write_u64(&mut self, n: u64) {
        self.0 = self.0.rotate_left(13) ^ n;
        self.0 = self.0.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}

type WordBuild = BuildHasherDefault<WordHasher>;

/// One fiber's claim ledger: each word with the multiplicity of counts
/// the fiber's frames own on it. Releasing a never-ledgered word (a
/// host-borrowed word a drive decs, a hand-assembled release with no
/// matching acquire) is a no-op: the ledger never goes negative, and the
/// store count is the mutation sites' business either way.
///
/// Every count funnel hits this ledger, so the rows are dense: a word the
/// ledger's own fiber owns indexes `counts` by its object id (the vector
/// grows with that fiber's id table as claims arrive, and a freed id's
/// slot stays zero until an unconditional take returns the id space), while
/// a word naming another fiber keeps its row in `foreign`, keyed by the
/// whole word so a foreign id never collides with a local one of the same
/// number.
#[derive(Debug)]
pub(crate) struct ClaimTable {
    /// The fiber this ledger belongs to: the owner axis a word must agree
    /// with to be local.
    owner: FiberId,
    /// The local words' multiplicities, indexed by object id.
    counts: Vec<u32>,
    /// The words naming another fiber's store.
    foreign: HashMap<RefId, u32, WordBuild>,
}

impl Default for ClaimTable {
    /// The root fiber's ledger: pool slot 0, where a fresh pool's claim
    /// vector starts and what fixtures over root-owned words build.
    fn default() -> Self {
        Self::new(FIBER_ROOT)
    }
}

impl ClaimTable {
    /// An empty ledger for `owner`'s frames.
    #[must_use]
    pub(crate) fn new(owner: FiberId) -> Self {
        Self {
            owner,
            counts: Vec::new(),
            foreign: HashMap::default(),
        }
    }

    /// One count the fiber's frames now own on `word`.
    pub(crate) fn acquire(&mut self, word: RefId) {
        if word.fiber() == self.owner {
            let at = word.id() as usize;
            if at >= self.counts.len() {
                self.counts.resize(at + 1, 0);
            }
            self.counts[at] += 1;
        } else {
            *self.foreign.entry(word).or_insert(0) += 1;
        }
    }

    /// One count left the fiber's frames: a frame's drop, or a delivery
    /// to another fiber or the host.
    pub(crate) fn release(&mut self, word: RefId) {
        if word.fiber() == self.owner {
            if let Some(n) = self.counts.get_mut(word.id() as usize)
                && *n > 0
            {
                *n -= 1;
            }
        } else {
            // a vacant row inserts nothing: the entry API holds the probe,
            // so a live row's last release removes it without a second one
            if let Entry::Occupied(mut row) = self.foreign.entry(word) {
                *row.get_mut() -= 1;
                if *row.get() == 0 {
                    row.remove();
                }
            }
        }
    }

    /// The fiber's outstanding count on `word` (0 when none): the
    /// teardown hook pass's liveness witness — a deinit claim whose span
    /// words no longer carry counts on this ledger has already died its
    /// ordinary death (its drop's RCDECs released them).
    pub(crate) fn count_of(&self, word: RefId) -> u32 {
        if word.fiber() == self.owner {
            self.counts.get(word.id() as usize).copied().unwrap_or(0)
        } else {
            self.foreign.get(&word).copied().unwrap_or(0)
        }
    }

    /// The drain walk's input: every claim with its multiplicity, taken
    /// out of the ledger — the local words in id order, then the foreign
    /// rows. `held` — the words about to ride their recorded outcome to a
    /// delivery — keep their claims and stay. An unconditional take also
    /// returns the id space: a pooled fiber slot respawns onto the same
    /// ledger, so the next life's drain never walks the earlier lives' ids.
    pub(crate) fn take_all_except(&mut self, held: &[RefId]) -> Vec<(RefId, u32)> {
        let mut out = Vec::new();
        let owner = self.owner;
        for (id, n) in self.counts.iter_mut().enumerate() {
            if *n == 0 {
                continue;
            }
            let word = RefId::new(owner, u32::try_from(id).expect("the id space fits u32"));
            if held.contains(&word) {
                continue;
            }
            out.push((word, *n));
            *n = 0;
        }
        for (word, n) in std::mem::take(&mut self.foreign) {
            if held.contains(&word) {
                self.foreign.insert(word, n);
            } else {
                out.push((word, n));
            }
        }
        if held.is_empty() {
            self.counts.clear();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use ds_abi::{ConstIdx, FrameLayout, Inst, RcOp, SlotType, uop::Uop};

    use super::{ClaimTable, HookSite, hook_sites};
    use crate::{
        fiber::FiberId,
        module::ModuleVersion,
        program::{Const, FunctionDef, assemble},
        value::RefId,
    };

    fn word(id: u32) -> RefId {
        RefId::new(FiberId(0), id)
    }

    #[test]
    fn acquire_release_balances_to_empty() {
        let mut table = ClaimTable::default();
        let w = word(3);
        table.acquire(w);
        table.acquire(w);
        table.acquire(word(7));
        table.release(w);
        let drained = table.take_all_except(&[]);
        assert_eq!(drained, vec![(w, 1), (word(7), 1)]);
        assert!(
            table.take_all_except(&[]).is_empty(),
            "a taken ledger drains to nothing"
        );
    }

    #[test]
    fn release_of_a_never_ledgered_word_is_a_no_op() {
        let mut table = ClaimTable::default();
        table.release(word(9));
        table.release(RefId::new(FiberId(2), 9));
        assert!(
            table.take_all_except(&[]).is_empty(),
            "a never-ledgered release leaves nothing"
        );
    }

    /// A foreign word keeps its row in the overflow map, keyed by the whole
    /// word: it neither collides with a local id of the same number nor
    /// grows the local vector.
    #[test]
    fn a_foreign_word_keeps_its_own_row() {
        let mut table = ClaimTable::default();
        let local = word(5);
        let foreign = RefId::new(FiberId(2), 5);
        table.acquire(local);
        table.acquire(foreign);
        table.acquire(foreign);
        assert_eq!(table.count_of(local), 1);
        assert_eq!(table.count_of(foreign), 2, "the whole word keys the row");
        table.release(foreign);
        assert_eq!(table.count_of(foreign), 1);
        assert_eq!(
            table.take_all_except(&[]),
            vec![(local, 1), (foreign, 1)],
            "the local ids drain first, then the foreign rows"
        );
        assert!(
            table.take_all_except(&[]).is_empty(),
            "a taken ledger drains to nothing"
        );
    }

    #[test]
    fn take_all_keeps_the_held_word() {
        let mut table = ClaimTable::default();
        let held = word(1);
        let gone = word(2);
        table.acquire(held);
        table.acquire(gone);
        let drained = table.take_all_except(&[held]);
        assert_eq!(drained, vec![(gone, 1)]);
        // the held claim survives a take
        table.release(held);
        assert!(
            table.take_all_except(&[]).is_empty(),
            "the held claim left with its release"
        );
    }

    /// An unconditional take returns the id space: a pooled fiber slot
    /// respawns onto the same ledger, and its next drain walks only the
    /// ids its own life claimed.
    #[test]
    fn an_unconditional_take_returns_the_id_space() {
        let mut table = ClaimTable::default();
        table.acquire(word(4096));
        assert_eq!(table.take_all_except(&[]), vec![(word(4096), 1)]);
        assert_eq!(table.counts.len(), 0, "the emptied ledger keeps no ids");
        table.acquire(word(2));
        assert_eq!(table.count_of(word(4096)), 0, "the earlier life is gone");
        assert_eq!(table.take_all_except(&[]), vec![(word(2), 1)]);
    }

    /// A take that holds a word back keeps the id space: the surviving row
    /// still indexes it.
    #[test]
    fn a_held_take_keeps_the_id_space() {
        let mut table = ClaimTable::default();
        let held = word(4096);
        table.acquire(held);
        assert_eq!(table.take_all_except(&[held]), []);
        assert_eq!(table.count_of(held), 1, "the held claim survives");
        assert!(table.counts.len() > held.id() as usize);
        table.release(held);
        assert_eq!(table.take_all_except(&[]), []);
    }

    #[test]
    fn count_of_answers_the_outstanding_multiplicity() {
        let mut table = ClaimTable::default();
        let w = word(4);
        assert_eq!(table.count_of(w), 0, "a never-ledgered word counts zero");
        table.acquire(w);
        table.acquire(w);
        assert_eq!(table.count_of(w), 2);
        table.release(w);
        assert_eq!(table.count_of(w), 1);
        table.release(w);
        assert_eq!(table.count_of(w), 0, "a drained word counts zero");
    }

    /// One fixture module: an owner body carrying the drop shape (a
    /// staging run into the overlay plus the `$deinit` call), a plain call
    /// of the same staging shape to a non-deinit callee, and a broken run,
    /// beside the hook function itself.
    fn scan_module() -> ModuleVersion {
        let body_len = 2_u8;
        let owner = FunctionDef {
            name: "owner".into(),
            layout: FrameLayout::new(vec![SlotType::I64, SlotType::Ref, SlotType::Raw], 1, 1)
                .expect("valid layout"),
            pool: vec![
                Const::FuncRef {
                    module: "m".into(),
                    func: 1,
                },
                Const::FuncRef {
                    module: "m".into(),
                    func: 2,
                },
                Const::I64(0),
            ],
            code: assemble(&[
                // the drop shape: stage body(1) then call the hook — one
                // word span, base 1
                Inst::Uop(Uop::Mov {
                    dst: body_len,
                    src: 1,
                }),
                Inst::Uop(Uop::Call {
                    callee: ConstIdx(0),
                    nargs: 1,
                }),
                // an ordinary call of the same staging shape: the callee is
                // not a `$deinit` function, so no site
                Inst::Uop(Uop::Mov {
                    dst: body_len,
                    src: 1,
                }),
                Inst::Uop(Uop::Call {
                    callee: ConstIdx(1),
                    nargs: 1,
                }),
            ])
            .expect("encodes"),
        };
        let deinit = FunctionDef {
            name: "res$deinit".into(),
            layout: FrameLayout::new(vec![SlotType::Ref], 1, 0).expect("valid layout"),
            pool: Vec::new(),
            code: assemble(&[Inst::Uop(Uop::Funcf { frame_slots: 1 })]).expect("encodes"),
        };
        let plain = FunctionDef {
            name: "plain".into(),
            layout: FrameLayout::new(vec![SlotType::Ref], 1, 0).expect("valid layout"),
            pool: Vec::new(),
            code: assemble(&[Inst::Uop(Uop::Funcf { frame_slots: 1 })]).expect("encodes"),
        };
        ModuleVersion::new("m", vec![owner, deinit, plain]).expect("valid module")
    }

    #[test]
    fn the_scan_finds_exactly_the_deinit_drop_shapes() {
        let version = scan_module();
        assert_eq!(
            hook_sites(&version, 0).as_slice(),
            &[HookSite {
                base: 1,
                width: 1,
                hook: 1,
                band: None,
            }],
            "the staging run plus the $deinit call names the claim; the plain call does not"
        );
        assert!(
            hook_sites(&version, 1).is_empty(),
            "the hook body itself carries no claim"
        );
    }

    /// A lowered drop clears the claim's establishment band right after
    /// the call: the scan recovers the band's slot from that op, so the
    /// site names the liveness witness the doors read off the frame.
    #[test]
    fn a_drop_clearing_a_band_after_the_call_names_it_in_the_site() {
        let body_len = 2_u8;
        let owner = FunctionDef {
            name: "owner".into(),
            layout: FrameLayout::new(vec![SlotType::I64, SlotType::Ref, SlotType::Raw], 1, 1)
                .expect("valid layout"),
            pool: vec![Const::FuncRef {
                module: "m".into(),
                func: 1,
            }],
            code: assemble(&[
                // the lowered drop shape: stage body(1), call the hook,
                // clear the band word at body slot 0, dec the count
                Inst::Uop(Uop::Mov {
                    dst: body_len,
                    src: 1,
                }),
                Inst::Uop(Uop::Call {
                    callee: ConstIdx(0),
                    nargs: 1,
                }),
                Inst::Uop(Uop::TagNew {
                    dst: 0,
                    tag: super::CLAIM_BAND_CLEAR,
                }),
                Inst::Rc(RcOp::Dec { src: 1 }),
            ])
            .expect("encodes"),
        };
        let deinit = FunctionDef {
            name: "res$deinit".into(),
            layout: FrameLayout::new(vec![SlotType::Ref], 1, 0).expect("valid layout"),
            pool: Vec::new(),
            code: assemble(&[Inst::Uop(Uop::Funcf { frame_slots: 1 })]).expect("encodes"),
        };
        let version = ModuleVersion::new("m", vec![owner, deinit.clone()]).expect("valid module");
        assert_eq!(
            hook_sites(&version, 0).as_slice(),
            &[HookSite {
                base: 1,
                width: 1,
                hook: 1,
                band: Some(0),
            }],
            "the clear op after the call names the site's band word"
        );
        // a clear into an overlay word (dst past the body) is not a band
        // site, and neither is any other tag value
        let owner = FunctionDef {
            name: "owner".into(),
            layout: FrameLayout::new(vec![SlotType::I64, SlotType::Ref, SlotType::Raw], 1, 1)
                .expect("valid layout"),
            pool: vec![Const::FuncRef {
                module: "m".into(),
                func: 1,
            }],
            code: assemble(&[
                Inst::Uop(Uop::Mov {
                    dst: body_len,
                    src: 1,
                }),
                Inst::Uop(Uop::Call {
                    callee: ConstIdx(0),
                    nargs: 1,
                }),
                Inst::Uop(Uop::TagNew {
                    dst: 2,
                    tag: super::CLAIM_BAND_CLEAR,
                }),
            ])
            .expect("encodes"),
        };
        let version = ModuleVersion::new("m", vec![owner, deinit]).expect("valid module");
        assert_eq!(
            hook_sites(&version, 0).as_slice(),
            &[HookSite {
                base: 1,
                width: 1,
                hook: 1,
                band: None,
            }],
            "an overlay dst is no band; the site keeps the ledger witness"
        );
    }

    #[test]
    fn a_foreign_or_misshapen_run_yields_no_site() {
        // the staging run broken: the MOV's source is an overlay word, not
        // a body word, so the run is not a drop shape
        let owner = FunctionDef {
            name: "owner".into(),
            layout: FrameLayout::new(vec![SlotType::I64, SlotType::Ref, SlotType::Raw], 1, 1)
                .expect("valid layout"),
            pool: vec![Const::FuncRef {
                module: "m".into(),
                func: 1,
            }],
            code: assemble(&[
                Inst::Uop(Uop::Mov { dst: 2, src: 2 }),
                Inst::Uop(Uop::Call {
                    callee: ConstIdx(0),
                    nargs: 1,
                }),
            ])
            .expect("encodes"),
        };
        let deinit = FunctionDef {
            name: "res$deinit".into(),
            layout: FrameLayout::new(vec![SlotType::Ref], 1, 0).expect("valid layout"),
            pool: Vec::new(),
            code: assemble(&[Inst::Uop(Uop::Funcf { frame_slots: 1 })]).expect("encodes"),
        };
        let version = ModuleVersion::new("m", vec![owner, deinit]).expect("valid module");
        assert!(
            hook_sites(&version, 0).is_empty(),
            "a run whose source is not a body word is not a drop shape"
        );
    }
}
