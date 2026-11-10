//! The Perceus dup/drop placement pass (C4): decides, per heap-referenced
//! word of one function frame, where `rc_inc`/`rc_dec` go — replacing the
//! M3 placeholders (nothing emitted) with placed RC ops in the symbolic
//! buffer.
//!
//! # Model
//!
//! The walk in [`super::func`] is the forward pass over the linear
//! dataflow; this module is its reference-counting state machine. A
//! `Ref`-typed body word that holds a live reference is *owned* by the
//! frame and must be accounted for exactly once, on every path. Ownership
//! moves through the buffer:
//!
//! * a production (KSTR, STRCAT, a call result copied out of the overlay, a zero-init of a dead Ref
//!   payload word) makes the word owned;
//! * a binder naming a word is an alias, not a copy — `const t = s` and the initializer's temp are
//!   the same owner;
//! * a word copy (`Mov` between body slots) is a transfer when its source dies there (the value's
//!   death use, per the checker's last-use facts), else a duplication: `RCINC` on the source, the
//!   copy owns the new count;
//! * a read (EQSTR/STRCAT operands, a discarded statement value) drops at the use when the use is
//!   the value's death, else ownership survives to a later death;
//! * staging into the overlay: a local callee's borrow parameter owns its word (the callee balances
//!   with its own exit drops), so a dying source forwards its count and a live source dups; a
//!   native is a transient borrow window (C2) — never a dup, and a dying source drops right after
//!   the call;
//! * exits (RET/RETSPAN/CALLT) transfer the returned words and drop every other still-owned word,
//!   in ascending slot order; HALT ends the program, where the heap dies whole — no drops.
//!
//! Branches fork the ownership map per arm and rejoin by intersection: a
//! word stays owned after the join exactly when every non-diverging arm
//! kept it, so each runtime path drops each count once. The deinit hook
//! (C5) rides the drop: when the dying word's concrete type carries a
//! `deinit`, the drop first stages the word and calls the static deinit
//! target, then emits the `RCDEC`.
//!
//! # Conservative corners (dup or defer rather than risk a premature free)
//!
//! * A use inside a loop of a reference produced outside it never drops at the use (the use
//!   re-executes per iteration); the count dies at the frame exit or the owning binder's scope end.
//! * A second copy of a word whose count already forwarded at this same use (`f(s, s)`) dups: the
//!   callee drops each parameter once, so every handed word needs a count of its own.
//! * A word still owned at a loop back-edge with no binder left inside the body leaks until frame
//!   exit — placement never decs on a path that did not produce the count.
//! * Native calls never dup their staged Ref arguments: a staged word is a borrow. A native that
//!   retains what it reads (list_push storing an element, list_concat copying one, list_get handing
//!   a payload out) acquires its own count inside the call, so the borrow discipline stays
//!   balanced: a dying staged word drops its count right after the call, and the retained copy
//!   holds the native's acquired one.

use std::collections::{BTreeMap, HashMap, HashSet};

use ds_diag::Span;

use super::func::Op;

/// One outstanding reference count this frame must account for.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Owned {
    /// The static `deinit` hook of the dying word's concrete type (C5):
    /// the pool index of the placeholder call target, staged before the
    /// dec. `None` for deinit-less types (everything in the lowered
    /// subset today).
    deinit: Option<u16>,
    /// The loop nesting depth at the production site.
    depth: usize,
}

/// The per-function ownership map snapshot fork/join passes around.
pub(crate) type State = BTreeMap<u8, Owned>;

/// The placement state machine over one function's walk.
pub(crate) struct Placement {
    /// The checker's final use per binder span: (use span, borrowed).
    last_use: HashMap<Span, (Span, bool)>,
    /// The checker's consuming uses (binder, at): a move forces the death
    /// test true at exactly that use. Empty in the lowerable subset
    /// (borrow parameters only); consumed so a linear surface slots in
    /// without re-deriving affine facts.
    moves: HashSet<(Span, Span)>,
    /// Owned words, ascending slot order (every emission iterates it, so
    /// output is deterministic).
    owned: State,
    /// The binder spans naming each word (alias sets).
    binders: HashMap<u8, HashSet<Span>>,
    /// The words each binder names (for use notification).
    binder_words: HashMap<Span, Vec<u8>>,
    /// The death use of each word: the latest last-use among its binders,
    /// or the forced move site. `None`-keyed absence means no binder death
    /// is known (anonymous temps die at first consumption; unused binders
    /// die at scope end).
    death: HashMap<u8, Span>,
    /// The most recent use span seen for each word.
    seen: HashMap<u8, Span>,
    /// Words whose count forwarded into a callee at the current use (the
    /// `f(s, s)` corner).
    forwarded: HashSet<u8>,
    /// Loop nesting depth of the current walk position.
    loop_depth: usize,
    /// Staged Ref words a native borrow must drop after its call.
    native_pending: Vec<u8>,
}

impl Placement {
    /// Builds the pass from the checker's affine facts (the lowerer never
    /// re-derives last-use or move information).
    pub(crate) fn from_linearity(lin: &ds_check::Linearity) -> Self {
        Self::new(
            lin.last_uses
                .iter()
                .map(|u| (u.binder, (u.at, u.borrowed)))
                .collect(),
            lin.moves.iter().map(|m| (m.binder, m.at)).collect(),
        )
    }

    pub(crate) fn new(last_use: HashMap<Span, (Span, bool)>, moves: HashSet<(Span, Span)>) -> Self {
        Self {
            last_use,
            moves,
            owned: BTreeMap::new(),
            binders: HashMap::new(),
            binder_words: HashMap::new(),
            death: HashMap::new(),
            seen: HashMap::new(),
            forwarded: HashSet::new(),
            loop_depth: 0,
            native_pending: Vec::new(),
        }
    }

    // ---- productions and binders ------------------------------------------------

    /// A fresh reference landed in `slot` (KSTR, STRCAT, a call result
    /// copied off the overlay, a zero-init dead Ref payload word).
    pub(crate) fn produced(&mut self, slot: u8, deinit: Option<u16>) {
        self.owned.insert(slot, Owned {
            deinit,
            depth: self.loop_depth,
        });
    }

    /// A binder starts naming `words`. Aliases add to the set; the word's
    /// death use widens to the latest last-use among all its binders.
    pub(crate) fn bound(&mut self, binder: Span, words: &[u8], deinit: Option<u16>) {
        self.binder_words.insert(binder, words.to_vec());
        for &word in words {
            self.binders.entry(word).or_default().insert(binder);
            if let Some(owned) = self.owned.get_mut(&word) {
                owned.deinit = deinit;
            }
            self.recompute_death(word);
        }
    }

    /// A binder's scope ends: drop what only it kept alive. Bind order is
    /// the deinit order, so callers pass the scope's binders in order.
    pub(crate) fn scope_end(&mut self, code: &mut Vec<Op>, binder: Span) {
        let Some(words) = self.binder_words.remove(&binder) else {
            return;
        };
        for word in words {
            if let Some(names) = self.binders.get_mut(&word) {
                names.remove(&binder);
            }
            self.recompute_death(word);
            // the last alias leaving an owned word kills it here; words
            // with surviving aliases wait for the outer scope or the exit
            if self.owned.contains_key(&word)
                && self.binders.get(&word).is_none_or(HashSet::is_empty)
            {
                self.drop_word(code, word);
            }
        }
    }

    fn recompute_death(&mut self, word: u8) {
        let death = self
            .binders
            .get(&word)
            .into_iter()
            .flatten()
            .filter_map(|b| self.last_use.get(b).map(|(at, _)| *at))
            .max_by(span_order);
        match death {
            Some(at) => {
                self.death.insert(word, at);
            },
            None => {
                self.death.remove(&word);
            },
        }
    }

    // ---- uses ---------------------------------------------------------------------

    /// One use of `binder` at `at` (a path use or a punned field read):
    /// records the position the death test compares against.
    pub(crate) fn use_of(&mut self, binder: Span, at: Span) {
        let Some(words) = self.binder_words.get(&binder) else {
            return;
        };
        let forced = self.moves.contains(&(binder, at));
        for &word in words {
            // a fresh use context clears the forward marker of the prior one
            if self.seen.get(&word).is_none_or(|seen| *seen != at) {
                self.forwarded.remove(&word);
            }
            self.seen.insert(word, at);
            if forced {
                self.death.insert(word, at);
            }
        }
    }

    /// Whether the word's current use is its death: the checker-recorded
    /// latest use of every alias, reached, without a loop re-execution in
    /// between. Anonymous words (no binder) die at first consumption.
    fn dies_at(&self, word: u8) -> bool {
        let Some(owned) = self.owned.get(&word) else {
            return false;
        };
        if self.loop_depth > owned.depth {
            return false;
        }
        match self.death.get(&word) {
            Some(death) => self.seen.get(&word).is_some_and(|seen| seen == death),
            None => self.binders.get(&word).is_none_or(HashSet::is_empty),
        }
    }

    // ---- copies, reads, and drops ---------------------------------------------------

    /// A `Mov` copied `src` into the body word `dst` (already emitted).
    /// Transfer on the source's death, duplication otherwise.
    pub(crate) fn copied(&mut self, code: &mut Vec<Op>, dst: u8, src: u8) {
        let Some(owned) = self.owned.get(&src).copied() else {
            return;
        };
        if self.dies_at(src) {
            // the count moves with the word
            self.owned.remove(&src);
            self.owned.insert(dst, owned);
        } else {
            Self::emit_inc(code, src);
            self.owned.insert(dst, owned);
        }
    }

    /// Ref operands of an emitted EQSTR/NEQSTR/STRCAT: drop at the use
    /// when it is the value's death.
    pub(crate) fn read(&mut self, code: &mut Vec<Op>, words: &[u8]) {
        for &word in words {
            if self.owned.contains_key(&word) && self.dies_at(word) {
                self.drop_word(code, word);
            }
        }
    }

    /// A statement-position value is discarded: its words die now when
    /// nothing else keeps them.
    pub(crate) fn discard(&mut self, code: &mut Vec<Op>, words: &[u8]) {
        self.read(code, words);
    }

    /// A `var` re-assignment overwrites `words`: the replaced references
    /// die before the store threads (the store site the M3 comment pins).
    pub(crate) fn store_over(&mut self, code: &mut Vec<Op>, words: &[u8]) {
        for &word in words {
            if self.owned.contains_key(&word) {
                self.drop_word(code, word);
            }
        }
    }

    /// One Ref word staged into the overlay for a local callee (the Mov is
    /// emitted): the callee's borrow parameter owns the handed word and
    /// drops it at its own exit, so a dying source forwards its count and
    /// a live source dups.
    pub(crate) fn staged_local(&mut self, code: &mut Vec<Op>, src: u8) {
        if self.owned.contains_key(&src) {
            if self.dies_at(src) {
                self.owned.remove(&src);
                self.forwarded.insert(src);
            } else {
                Self::emit_inc(code, src);
            }
        } else if self.forwarded.contains(&src) {
            // the count already left at this use; the extra handed word
            // needs its own (the callee drops each parameter once)
            Self::emit_inc(code, src);
        }
    }

    /// One Ref word staged for a native call: a transient borrow window —
    /// no dup, ever; a dying source drops right after the call returns
    /// (retaining natives acquire their own counts inside the call, so
    /// the drop never steals a stored copy's count). A tail-position
    /// native never returns to this frame, so a staged source there is
    /// abandoned, not dropped (named corner: the dec has no legal
    /// position — the frame exits next, and a dec would free the object
    /// the still-running native reads; the count dies with the program).
    pub(crate) fn staged_native(&mut self, src: u8, tail: bool) {
        if !self.owned.contains_key(&src) {
            return;
        }
        if tail {
            self.owned.remove(&src);
        } else if self.dies_at(src) {
            self.native_pending.push(src);
        }
    }

    /// The native call returned: flush the drops its borrow windows owe.
    pub(crate) fn native_done(&mut self, code: &mut Vec<Op>) {
        for word in std::mem::take(&mut self.native_pending) {
            if self.owned.contains_key(&word) {
                self.drop_word(code, word);
            }
        }
    }

    /// The central drop: the deinit hook (C5) stages the word and calls
    /// the static target first when the concrete type carries one, then
    /// the RCDEC consumes the count (a unique death pools the block for
    /// reuse inside `ds_rt::Heap` — the dec-to-alloc pairing needs no uop).
    fn drop_word(&mut self, code: &mut Vec<Op>, word: u8) {
        let owned = self.owned.remove(&word).unwrap_or(Owned {
            deinit: None,
            depth: 0,
        });
        Self::emit_drop(code, word, owned.deinit);
    }

    /// Emits one drop without touching the ownership map — the exit path
    /// emits from a snapshot so sibling paths keep their counts.
    fn emit_drop(code: &mut Vec<Op>, word: u8, deinit: Option<u16>) {
        if let Some(deinit) = deinit {
            code.push(Op::Mov {
                dst: super::func::Slot::overlay(0),
                src: super::func::Slot::body(word),
            });
            code.push(Op::Call {
                callee: deinit,
                nargs: 1,
            });
        }
        code.push(Op::RcDec {
            src: super::func::Slot::body(word),
        });
    }

    fn emit_inc(code: &mut Vec<Op>, word: u8) {
        code.push(Op::RcInc {
            src: super::func::Slot::body(word),
        });
    }

    // ---- loops and branches ----------------------------------------------------------

    /// Enters a loop body: uses at the deeper depth never kill productions
    /// born outside.
    pub(crate) fn loop_enter(&mut self) {
        self.loop_depth += 1;
    }

    pub(crate) fn loop_exit(&mut self) {
        self.loop_depth -= 1;
    }

    /// The ownership snapshot for a branch fork.
    pub(crate) fn save(&self) -> State {
        self.owned.clone()
    }

    /// Installs a snapshot (an arm's walk mutates from there).
    pub(crate) fn restore(&mut self, state: State) {
        self.owned = state;
    }

    /// Takes the current snapshot (arm end).
    pub(crate) fn take(&mut self) -> State {
        std::mem::take(&mut self.owned)
    }

    /// Rejoins branch arms: a word stays owned exactly when every arm kept
    /// it, so each runtime path holds one count and drops it once.
    pub(crate) fn join(&mut self, arms: &[State]) {
        let Some(first) = arms.first() else {
            return;
        };
        self.owned = first
            .iter()
            .filter(|(word, _)| arms.iter().all(|arm| arm.contains_key(word)))
            .map(|(word, owned)| (*word, *owned))
            .collect();
        for word in self.owned.keys().copied().collect::<Vec<_>>() {
            self.recompute_death(word);
        }
    }

    // ---- exits ------------------------------------------------------------------------

    /// A frame exit (RET/RETSPAN/CALLT): the transferred words leave with
    /// their counts; every other owned word drops, ascending slot order,
    /// before the exit op. The frame ends on this path — the map clears,
    /// so walk positions after the exit (unreachable code, or sibling arms
    /// that forked their own state) never re-drop the same counts.
    pub(crate) fn exit(&mut self, code: &mut Vec<Op>, transferred: &[u8]) {
        let dying: Vec<(u8, Owned)> = self
            .owned
            .iter()
            .filter(|(word, _)| !transferred.contains(word))
            .map(|(word, owned)| (*word, *owned))
            .collect();
        for (word, owned) in dying {
            Self::emit_drop(code, word, owned.deinit);
        }
        self.owned.clear();
    }
}

/// Source order over spans: (file, start, end). The walk visits in source
/// order, so the latest last-use is the maximal span.
fn span_order(a: &Span, b: &Span) -> std::cmp::Ordering {
    (a.file.as_u32(), a.start, a.end).cmp(&(b.file.as_u32(), b.start, b.end))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::func::Slot;

    fn binder_span(start: u32) -> Span {
        Span::new(ds_diag::FileId::new(0), start, start + 1)
    }

    fn placement() -> Placement {
        Placement::new(HashMap::new(), HashSet::new())
    }

    /// The deinit hook (C5): a dying word whose type carries a deinit
    /// stages itself, calls the static placeholder target, then decs — the
    /// structure every drop site routes through.
    #[test]
    fn a_deinit_death_calls_the_static_target_before_the_dec() {
        let mut rc = placement();
        let mut code = Vec::new();
        rc.produced(3, Some(42));
        rc.drop_word(&mut code, 3);
        assert_eq!(
            code,
            vec![
                Op::Mov {
                    dst: Slot::overlay(0),
                    src: Slot::body(3),
                },
                Op::Call {
                    callee: 42,
                    nargs: 1,
                },
                Op::RcDec { src: Slot::body(3) },
            ],
            "the hook stages the word, calls, then decs"
        );
    }

    /// A copy off the death use transfers; a copy before it dups.
    #[test]
    fn copies_transfer_at_the_death_and_dup_before_it() {
        let mut rc = placement();
        let mut code = Vec::new();
        rc.produced(1, None);
        // no binders: the temp dies at first consumption
        rc.copied(&mut code, 2, 1);
        assert!(matches!(code.as_slice(), []), "a transfer emits nothing");
        assert!(!rc.owned.contains_key(&1));
        assert!(rc.owned.contains_key(&2));

        // a bound word with a later death dups at the copy
        let b = binder_span(50);
        let use_at = binder_span(90);
        rc.last_use.insert(b, (use_at, true));
        rc.bound(b, &[2], None);
        rc.use_of(b, binder_span(60));
        let mut code = Vec::new();
        rc.copied(&mut code, 5, 2);
        assert_eq!(
            code,
            vec![Op::RcInc { src: Slot::body(2) }],
            "a live source dups"
        );
        assert!(rc.owned.contains_key(&2) && rc.owned.contains_key(&5));

        // reaching the death use turns the next read into the drop
        rc.use_of(b, use_at);
        let mut code = Vec::new();
        rc.read(&mut code, &[2]);
        assert_eq!(
            code,
            vec![Op::RcDec { src: Slot::body(2) }],
            "the last use drops"
        );
    }

    /// Alias sets: the death widens to the latest alias's last use.
    #[test]
    fn an_alias_widens_the_death_span() {
        let mut rc = placement();
        let s = binder_span(10);
        let t = binder_span(30);
        let s_use = binder_span(20);
        let t_use = binder_span(40);
        rc.last_use.insert(s, (s_use, true));
        rc.last_use.insert(t, (t_use, true));
        rc.produced(7, None);
        rc.bound(s, &[7], None);
        rc.bound(t, &[7], None);
        rc.use_of(s, s_use);
        let mut code = Vec::new();
        rc.read(&mut code, &[7]);
        assert!(code.is_empty(), "s's use is not the death while t lives");
        rc.use_of(t, t_use);
        rc.read(&mut code, &[7]);
        assert_eq!(code.len(), 1, "the latest alias's use drops once");
    }

    /// Loop rule: a use one loop deeper than the production never drops.
    #[test]
    fn a_loop_use_of_an_outer_production_never_drops() {
        let mut rc = placement();
        let b = binder_span(10);
        let in_loop = binder_span(60);
        rc.last_use.insert(b, (in_loop, true));
        rc.produced(4, None);
        rc.bound(b, &[4], None);
        rc.loop_enter();
        rc.use_of(b, in_loop);
        let mut code = Vec::new();
        rc.read(&mut code, &[4]);
        assert!(
            code.is_empty(),
            "the use re-executes per iteration; the exit drops instead"
        );
        rc.loop_exit();
        rc.use_of(b, in_loop);
        let mut code = Vec::new();
        rc.read(&mut code, &[4]);
        assert_eq!(code.len(), 1, "outside the loop the same use is the death");
    }

    /// Fork/join: words owned on every arm survive the join; one-arm words
    /// do not (their counts died inside the arm).
    #[test]
    fn branches_keep_what_every_arm_keeps() {
        let mut rc = placement();
        rc.produced(1, None);
        let base = rc.save();
        // arm A also produces 2
        rc.produced(2, None);
        let arm_a = rc.take();
        rc.restore(base.clone());
        // arm B produces nothing
        let arm_b = rc.take();
        rc.join(&[arm_a, arm_b]);
        assert!(rc.owned.contains_key(&1), "kept by both arms");
        assert!(!rc.owned.contains_key(&2), "arm-A-only word left the state");

        // exits drop the survivors once, in ascending slot order, and end
        // the path: the map clears so nothing re-drops
        let mut code = Vec::new();
        rc.exit(&mut code, &[]);
        assert_eq!(
            code,
            vec![Op::RcDec { src: Slot::body(1) }],
            "exactly one dec for the surviving count"
        );
        assert!(rc.owned.is_empty(), "the frame ended on this path");
    }
}
