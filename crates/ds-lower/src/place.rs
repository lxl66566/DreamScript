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
//!   the call. The window opens before the call's operand evaluation and pins every word its CALL
//!   reads: a death firing inside that window (a later operand's own last-use dec) defers to the
//!   window's flush, so the object survives the CALL; nested windows flush LIFO, and a word several
//!   nested calls read dies at the outermost reading window's flush;
//! * exits (RET/RETSPAN/CALLT) transfer the returned words and drop every other still-owned word,
//!   in ascending slot order; HALT ends the program, where the heap dies whole — no drops.
//!
//! Branches fork the ownership map per arm and rejoin by intersection: a
//! word stays owned after the join exactly when every non-diverging arm
//! kept it, and an arm keeping a word the join does not keep decs it at
//! its own flow end (the balanced join: a count produced or kept on one
//! arm's path dies inside that arm), so each runtime path drops each
//! count once. The deinit hook (C5) rides the value's base word: when
//! the dying value's concrete type declares a `deinit`, the drop first
//! stages the value's whole word span, calls the synthesized deinit
//! function while the fields are still live, then emits the `RCDEC` on
//! the count words.
//!
//! # Conservative corners (dup or defer rather than risk a premature free)
//!
//! * A use inside a loop of a reference produced outside it never drops at the use (the use
//!   re-executes per iteration); the count dies at the frame exit or the owning binder's scope end.
//! * A word owned at a loop entry is loop-carried: no in-body death site kills it (the claim exists
//!   once but the body replays, so an in-body death would dec once per iteration — the head-reset
//!   word re-stamped by its own overwrite defeated the depth guard above). An overwrite inside the
//!   body rolls the slot's content over — the store's dec consumes the previous content's count
//!   while the word stays owned for the back-edge join — and the final content dies where the
//!   carried claim dies, past the loop.
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

/// One declared deinit hook (C5): the pool index of the struct's
/// synthesized deinit function, the word width of the value it stages
/// at the death, and the span's `Ref` word offsets — the claim decs
/// each at the value's death, so a multi-`Ref` span dies as one unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hook {
    /// The pool index of the `Constant::FuncRef` naming the deinit.
    pub(crate) pool: u16,
    /// How many words the dying value spans; the hook stages them all.
    pub(crate) width: u8,
    /// Offsets of the span's `Ref` words from the claim's base word.
    pub(crate) refs: std::rc::Rc<[u8]>,
}

/// One outstanding reference count this frame must account for.
#[derive(Clone, Debug)]
pub(crate) struct Owned {
    /// The deinit hook of the dying value this word anchors (C5). A
    /// hook rides the value's base word only: a scalar base word owns
    /// the hook without any `Ref` count, and the span's other `Ref`
    /// words fold into the claim (their decs fire with the hook's).
    hook: Option<Hook>,
    /// Whether the word carries a `Ref` count alongside any hook. A
    /// claimed base word keeps this clear — its decs are the hook's.
    count: bool,
    /// A deinit value's word: its uses are borrows and never kill it
    /// (the C5 discipline keeps the value live to its drop site); only
    /// a consuming move, the scope end, an assignment, or the frame
    /// exit may fire the drop.
    pinned: bool,
    /// The loop nesting depth at the production site.
    depth: usize,
}

/// The per-function ownership map snapshot fork/join passes around.
pub(crate) type State = BTreeMap<u8, Owned>;

/// One open native-call borrow window: the words its CALL reads (each
/// staged Ref argument and the body words its already-evaluated operands
/// resolved to) plus the deaths deferred to the window's flush. The
/// emitter pushes one window before a native call's operand evaluation
/// and pops it at the call's flush, so windows nest LIFO — an inner
/// call's flush runs before the outer CALL reads.
#[derive(Default)]
struct Window {
    /// Words the window's CALL reads or will read.
    readers: HashSet<u8>,
    /// Words whose death decs fire at this window's flush, in
    /// first-deferral order; `seen` dedupes a word both staged and dying
    /// at one call so the count dies exactly once.
    deferred: Vec<u8>,
    seen: HashSet<u8>,
}

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
    /// The words owned at each enclosing loop's entry: a loop-carried
    /// word's claim survives the whole body (every in-body death site is
    /// suppressed), its content rolls over at in-body overwrites, and the
    /// final content dies past the join.
    loop_claims: Vec<HashSet<u8>>,
    /// The open native-call borrow windows, outermost first.
    windows: Vec<Window>,
    /// Whether the walk's current path exited the frame since the last
    /// fork (an arm that exits never reaches the join, so its state
    /// must not constrain what the join keeps).
    diverged: bool,
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
            loop_claims: Vec::new(),
            windows: Vec::new(),
            diverged: false,
        }
    }

    // ---- productions and binders ------------------------------------------------

    /// A fresh reference landed in `slot` (KSTR, STRCAT, a call result
    /// copied off the overlay, a zero-init dead Ref payload word).
    pub(crate) fn produced(&mut self, slot: u8) {
        self.owned.insert(slot, Owned {
            hook: None,
            count: true,
            pinned: false,
            depth: self.loop_depth,
        });
    }

    /// Registers one deinit-carrying value's claim (C5): the hook
    /// anchors the value's base word and fires at the value's unique
    /// death, dec-ing every `Ref` word of the span (the hook's refs).
    /// The span's other plain-count words fold into the claim — their
    /// decs fire with the hook's, never on their own — while a word
    /// anchoring its own nested claim keeps it (the nested value's
    /// death stays its own). Every already-owned word of the value
    /// pins — its uses are borrows, never deaths.
    pub(crate) fn claimed(&mut self, base: u8, hook: Hook, words: &[u8]) {
        match self.owned.get_mut(&base) {
            Some(owned) => {
                owned.hook = Some(hook);
                owned.count = false;
                owned.pinned = true;
            },
            None => {
                self.owned.insert(base, Owned {
                    hook: Some(hook),
                    count: false,
                    pinned: true,
                    depth: self.loop_depth,
                });
            },
        }
        for &word in words {
            match self.owned.get_mut(&word) {
                // a plain count word of the span folds into the claim
                Some(owned) if owned.hook.is_none() => {
                    self.owned.remove(&word);
                },
                // a nested claim keeps its own death
                Some(owned) => {
                    owned.pinned = true;
                },
                None => {},
            }
        }
    }

    /// Whether the placement currently owns `word`'s count (a guard's
    /// consumption check reads ownership before and after its walk).
    pub(crate) fn owns(&self, word: u8) -> bool {
        self.owned.contains_key(&word)
    }

    /// Whether one word anchors a deinit value's claim.
    pub(crate) fn has_claim(&self, word: u8) -> bool {
        self.owned
            .get(&word)
            .is_some_and(|owned| owned.hook.is_some())
    }

    /// The base word of the span claim covering `word`, if one does: a
    /// folded word of a deinit value answers its claim's base, so a
    /// hand-off can fire the claim while the value's words are live.
    pub(crate) fn claim_base_of(&self, word: u8) -> Option<u8> {
        self.owned.iter().find_map(|(base, owned)| {
            let hook = owned.hook.as_ref()?;
            (word >= *base && usize::from(word - base) < usize::from(hook.width)).then_some(*base)
        })
    }

    /// Fires one word's deinit claim at a hand-off (C5): the word
    /// escapes as a smaller value (a field read of the dying struct),
    /// so the claim the word anchors runs here while the value's other
    /// words are still live. The `Ref` words of the span whose absolute
    /// word the hand-off does not carry dec right after the hook; a
    /// count whose word escapes rides the hand-off untouched — the
    /// claim is spent, the count is the returned value's.
    pub(crate) fn fire_claim(&mut self, code: &mut Vec<Op>, word: u8, escaping: &[u8]) {
        let Some(hook) = self
            .owned
            .get_mut(&word)
            .and_then(|owned| owned.hook.take())
        else {
            return;
        };
        for index in 0..hook.width {
            code.push(Op::Mov {
                dst: super::func::Slot::overlay(index),
                src: super::func::Slot::body(word + index),
            });
        }
        code.push(Op::Call {
            callee: hook.pool,
            nargs: hook.width,
        });
        for &offset in hook.refs.iter() {
            let count_word = word + offset;
            if !escaping.contains(&count_word) {
                code.push(Op::RcDec {
                    src: super::func::Slot::body(count_word),
                });
            }
        }
    }

    /// A binder starts naming `words`. Aliases add to the set; the word's
    /// death use widens to the latest last-use among all its binders.
    pub(crate) fn bound(&mut self, binder: Span, words: &[u8]) {
        self.binder_words.insert(binder, words.to_vec());
        for &word in words {
            self.binders.entry(word).or_default().insert(binder);
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
        // A pinned word's death never derives from last uses (C5): the
        // value stays live to its drop site, so only the checker's
        // forced moves (the consuming use that transfers ownership) may
        // claim it at a use.
        if self.owned.get(&word).is_some_and(|owned| owned.pinned) {
            return;
        }
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
    /// between. Anonymous words (no binder) die at first consumption. A
    /// loop-carried word never dies in-body: its content's count rolls
    /// over at the next overwrite and the carried claim dies past the
    /// loop.
    fn dies_at(&self, word: u8) -> bool {
        let Some(owned) = self.owned.get(&word) else {
            return false;
        };
        if self.loop_depth > owned.depth {
            return false;
        }
        if self.loop_carried(word) {
            return false;
        }
        match self.death.get(&word) {
            Some(death) => self.seen.get(&word).is_some_and(|seen| seen == death),
            None => self.binders.get(&word).is_none_or(HashSet::is_empty),
        }
    }

    /// Whether an enclosing loop carried `word` in from outside its body.
    fn loop_carried(&self, word: u8) -> bool {
        self.loop_claims.iter().any(|claims| claims.contains(&word))
    }

    // ---- copies, reads, and drops ---------------------------------------------------

    /// A `Mov` copied `src` into the body word `dst` (already emitted).
    /// Transfer on the source's death, duplication otherwise. A word
    /// folded into a span claim borrows instead: the claim keeps its
    /// count (the hook reads it at the claim's death), so the copy incs
    /// and owns a fresh one — unless the destination sits inside the
    /// moved claim's own span, where the bits ride the claim.
    pub(crate) fn copied(&mut self, code: &mut Vec<Op>, dst: u8, src: u8) {
        if let Some(owned) = self.owned.get(&src).cloned() {
            // A pinned word belongs to a deinit value: the copy site is
            // the checker's consuming move (a linear value never
            // alias-copies), so the claim and its count transfer with no
            // dup.
            if owned.pinned || self.dies_at(src) {
                // the count moves with the word
                self.owned.remove(&src);
                self.owned.insert(dst, owned);
            } else {
                Self::emit_inc(code, src);
                self.owned.insert(dst, owned);
            }
            return;
        }
        if self.claim_base_of(src).is_some() && self.claim_base_of(dst).is_none() {
            Self::emit_inc(code, src);
            self.owned.insert(dst, Owned {
                hook: None,
                count: true,
                pinned: false,
                depth: self.loop_depth,
            });
            self.recompute_death(dst);
        }
    }

    /// One Ref word duplicated unconditionally: a construct that keeps
    /// the source live past its checker-recorded death position (a
    /// guarded match arm's scrutinee copy) owns its dup outright — the
    /// source's count is not this use's to consume, so the death test
    /// does not apply.
    pub(crate) fn duped(&mut self, code: &mut Vec<Op>, dst: u8, src: u8) {
        if self.owned.contains_key(&src) {
            Self::emit_inc(code, src);
            let owned = Owned {
                hook: None,
                count: true,
                pinned: false,
                depth: self.loop_depth,
            };
            self.owned.insert(dst, owned);
            self.recompute_death(dst);
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
    /// A loop-carried word's dec is a rollover, not a claim death: the
    /// body re-executes, so the word keeps its claim for the back-edge
    /// join — each iteration's dec consumes the previous content's count
    /// and the final content dies past the loop.
    pub(crate) fn store_over(&mut self, code: &mut Vec<Op>, words: &[u8]) {
        for &word in words {
            if let Some(owned) = self.owned.get(&word).cloned() {
                Self::emit_drop(code, word, owned);
                if !self.loop_carried(word) {
                    self.owned.remove(&word);
                }
            }
        }
    }

    /// One Ref word staged into the overlay for a local callee (the Mov is
    /// emitted): the callee's borrow parameter owns the handed word and
    /// drops it at its own exit, so a dying source forwards its count and
    /// a live source dups. A word an enclosing native window still reads
    /// cannot forward — the callee decs its parameter inside the CALL,
    /// before the enclosing window's CALL reads the word — so the
    /// handing dups instead and the frame keeps its count for that
    /// window's flush.
    pub(crate) fn staged_local(&mut self, code: &mut Vec<Op>, src: u8) {
        if self.owned.contains_key(&src) {
            if self.dies_at(src) && self.enclosing_window_reads(src) {
                Self::emit_inc(code, src);
            } else if self.dies_at(src) {
                self.owned.remove(&src);
                self.forwarded.insert(src);
            } else {
                Self::emit_inc(code, src);
            }
        } else if self.forwarded.contains(&src) || self.claim_base_of(src).is_some() {
            // the count already left at this use, or the word belongs to
            // a span claim (a deinit value's borrow): the extra handed
            // word needs its own (the callee drops each parameter once,
            // the claim keeps its count for the hook)
            Self::emit_inc(code, src);
        }
    }

    /// Whether an open window below the innermost one reads `word`: its
    /// CALL runs after the current emission's, so the word must outlive
    /// this call.
    fn enclosing_window_reads(&self, word: u8) -> bool {
        self.windows
            .get(..self.windows.len().saturating_sub(1))
            .is_some_and(|below| below.iter().any(|w| w.readers.contains(&word)))
    }

    /// One Ref word staged for a local callee the frame tail-transfers
    /// to: the transfer ends the frame, so no post-transfer position
    /// exists for a dup or a drop — the count rides the callee (its own
    /// exit drops the handed word) and a claim the word anchors fires
    /// here, at the value's frame-exit death, with its fields alive,
    /// the span's non-escaping `Ref` words dec-ing behind the hook.
    pub(crate) fn staged_tail_local(&mut self, code: &mut Vec<Op>, src: u8) {
        if self.owned.contains_key(&src) {
            self.fire_claim(code, src, &[src]);
            self.owned.remove(&src);
            self.forwarded.insert(src);
        } else if self.forwarded.contains(&src) {
            Self::emit_inc(code, src);
        }
    }

    /// One Ref word staged for a native call: a transient borrow window —
    /// no dup, ever; a dying source drops right after the call returns
    /// (retaining natives acquire their own counts inside the call, so
    /// the drop never steals a stored copy's count). `tail` marks the
    /// root frame's tail transfer (CALLT-to-native pops the frame inside
    /// the instruction, so the dec has no legal position and a pre-call
    /// dec would free the object the still-running native reads): the
    /// staged source is abandoned there, and the count dies with the
    /// program — sound only because that transfer ends the program. Every
    /// other frame passes `false` even in tail position (the walk lowers
    /// the call in place and returns), keeping the borrow window
    /// flushable.
    pub(crate) fn staged_native(&mut self, src: u8, tail: bool) {
        if !self.owned.contains_key(&src) {
            return;
        }
        if tail {
            self.owned.remove(&src);
            return;
        }
        // the window's CALL reads the staged word, so its own death pins
        // inside it
        if let Some(window) = self.windows.last_mut() {
            window.readers.insert(src);
        }
        if self.dies_at(src) {
            self.defer_death(src);
        }
    }

    /// Opens one native call's borrow window: the walk pushes it before
    /// evaluating the call's operands (a later operand's own use can fire
    /// an earlier word's last-use dec there, and the window pins every
    /// word the CALL still reads) and pops it at the call's flush.
    pub(crate) fn native_open(&mut self) {
        self.windows.push(Window::default());
    }

    /// Registers words the innermost open window's CALL will read: each
    /// staged Ref argument and, eagerly, the body words the call's
    /// already-evaluated operands resolved to.
    pub(crate) fn native_reads(&mut self, words: &[u8]) {
        if let Some(window) = self.windows.last_mut() {
            window.readers.extend(words.iter().copied());
        }
    }

    /// The outermost open window whose CALL reads `word`: an inner
    /// window flushes before an outer CALL reads, so a word several
    /// nested calls read must die at the outermost reader's flush.
    fn pinning_window(&self, word: u8) -> Option<usize> {
        self.windows.iter().position(|w| w.readers.contains(&word))
    }

    /// Defers one death into its pinning window's flush list; false when
    /// no open window reads the word, so the caller drops it now.
    fn defer_death(&mut self, word: u8) -> bool {
        let Some(index) = self.pinning_window(word) else {
            return false;
        };
        let window = &mut self.windows[index];
        if window.seen.insert(word) {
            window.deferred.push(word);
        }
        true
    }

    /// The native call returned: pop its borrow window and flush the
    /// deaths the window deferred — the CALL has consumed the staged
    /// words, so their counts die now, in first-deferral order.
    pub(crate) fn native_done(&mut self, code: &mut Vec<Op>) {
        debug_assert!(
            !self.windows.is_empty(),
            "every native flush pairs with its window's open"
        );
        let Some(window) = self.windows.pop() else {
            return;
        };
        for word in window.deferred {
            if self.owned.contains_key(&word) {
                self.drop_word(code, word);
            }
        }
    }

    /// The frame's tail transfer flushed its borrow window: the staged
    /// words ride the CALLT with their counts (a local callee owns its
    /// borrow parameters; the root frame's native tail ends the program,
    /// where the heap dies whole), so deferred deaths abandon instead of
    /// dec'ing — no legal position exists after the transfer.
    pub(crate) fn native_tail_transfer(&mut self) {
        debug_assert!(
            !self.windows.is_empty(),
            "every native flush pairs with its window's open"
        );
        let Some(window) = self.windows.pop() else {
            return;
        };
        for word in window.deferred {
            self.owned.remove(&word);
        }
    }

    /// The central drop (C5): a deinit claim stages the value's whole
    /// word span and calls the synthesized deinit function first — while
    /// the value's fields are still alive — then the RCDEC consumes the
    /// count when the word carries one (a unique death pools the block
    /// for reuse inside the `ds-vm/src/object.rs` store — the
    /// dec-to-alloc pairing needs no uop). A death firing while an open
    /// native window's CALL still reads the word defers instead: the dec
    /// moves to that window's flush, after the CALL, and the word stays
    /// owned so the count dies exactly once, at the flush.
    fn drop_word(&mut self, code: &mut Vec<Op>, word: u8) {
        if self.defer_death(word) {
            return;
        }
        let owned = self.owned.remove(&word).unwrap_or(Owned {
            hook: None,
            count: true,
            pinned: false,
            depth: 0,
        });
        Self::emit_drop(code, word, owned);
    }

    /// Emits one drop without touching the ownership map — the exit path
    /// emits from a snapshot so sibling paths keep their counts. A claim
    /// stages its whole span, calls the hook, then decs every `Ref` word
    /// of the span (the hook's refs); an unclaimed count decs alone.
    fn emit_drop(code: &mut Vec<Op>, word: u8, owned: Owned) {
        if let Some(hook) = owned.hook {
            for index in 0..hook.width {
                code.push(Op::Mov {
                    dst: super::func::Slot::overlay(index),
                    src: super::func::Slot::body(word + index),
                });
            }
            code.push(Op::Call {
                callee: hook.pool,
                nargs: hook.width,
            });
            for &offset in hook.refs.iter() {
                code.push(Op::RcDec {
                    src: super::func::Slot::body(word + offset),
                });
            }
            return;
        }
        if owned.count {
            code.push(Op::RcDec {
                src: super::func::Slot::body(word),
            });
        }
    }

    fn emit_inc(code: &mut Vec<Op>, word: u8) {
        code.push(Op::RcInc {
            src: super::func::Slot::body(word),
        });
    }

    /// One branch arm's epilogue: every word the join does not keep dies
    /// inside its arm, deinit hook first like every drop.
    pub(crate) fn emit_drops(code: &mut Vec<Op>, dying: &[(u8, Owned)]) {
        for (word, owned) in dying {
            Self::emit_drop(code, *word, owned.clone());
        }
    }

    /// Drops one expression's consumed operand words when no binder
    /// names them: an anonymous production whose consumption sat inside
    /// a nested loop (the depth and carried guards suppressed its
    /// in-loop dec) dies here instead — once per evaluation of the
    /// enclosing expression, so a per-iteration production inside an
    /// outer loop balances. A named word keeps its carried claim and
    /// dies at its own scope end.
    pub(crate) fn drop_unnamed(&mut self, code: &mut Vec<Op>, words: &[u8]) {
        for &word in words {
            if self.owned.contains_key(&word)
                && self.binders.get(&word).is_none_or(HashSet::is_empty)
            {
                self.drop_word(code, word);
            }
        }
    }

    /// The words dying at a loop-exit jump (break/continue): every
    /// binder of the scopes the jump leaves drops its words there when
    /// no alias in a kept scope survives. Computed against the current
    /// map without consuming it — the fall-through path still owns
    /// these words and drops them at its own scope pops, so each
    /// runtime path drops each count exactly once.
    pub(crate) fn loop_exit_drops(&self, doomed: &[Span]) -> Vec<(u8, Owned)> {
        let mut owned = self.owned.clone();
        let mut dying = Vec::new();
        for binder in doomed {
            for &word in self.binder_words.get(binder).into_iter().flatten() {
                let survives = self
                    .binders
                    .get(&word)
                    .is_some_and(|names| names.iter().any(|name| !doomed.contains(name)));
                if !survives && let Some(entry) = owned.remove(&word) {
                    dying.push((word, entry));
                }
            }
        }
        dying
    }

    // ---- loops and branches ----------------------------------------------------------

    /// Enters a loop body: uses at the deeper depth never kill productions
    /// born outside, and the words owned here become the loop's carried
    /// claims — their in-body deaths stay suppressed so a loop-carried
    /// slot keeps exactly one live claim across every iteration.
    pub(crate) fn loop_enter(&mut self) {
        self.loop_claims.push(self.owned.keys().copied().collect());
        self.loop_depth += 1;
    }

    pub(crate) fn loop_exit(&mut self) {
        self.loop_claims.pop();
        self.loop_depth -= 1;
    }

    /// The ownership snapshot for a branch fork.
    pub(crate) fn save(&self) -> State {
        self.owned.clone()
    }

    /// Installs a snapshot (an arm's walk mutates from there; the arm
    /// starts on a path that has not exited).
    pub(crate) fn restore(&mut self, state: State) {
        self.owned = state;
        self.diverged = false;
    }

    /// Whether the current path exited the frame since the last fork.
    pub(crate) fn diverged(&self) -> bool {
        self.diverged
    }

    /// Takes the current snapshot (arm end).
    pub(crate) fn take(&mut self) -> State {
        std::mem::take(&mut self.owned)
    }

    /// The balanced branch join: installs the intersection of the
    /// non-diverging arms (an arm that exited the frame never reaches
    /// the join, so it constrains nothing) and returns, per arm, the
    /// words it owns beyond that intersection. A count kept on one
    /// arm's path only dies inside that arm, so the caller splices each
    /// arm's epilogue decs in before its jump to the join point; a
    /// diverging arm owns nothing (its exit path dropped from its own
    /// state).
    pub(crate) fn reconcile(&mut self, arms: &[(State, bool)]) -> Vec<Vec<(u8, Owned)>> {
        self.diverged = false;
        let live: Vec<&State> = arms
            .iter()
            .filter(|(_, diverged)| !diverged)
            .map(|(state, _)| state)
            .collect();
        let keep: State = match live.first() {
            Some(first) => first
                .iter()
                .filter(|(word, _)| live.iter().all(|arm| arm.contains_key(word)))
                .map(|(word, owned)| (*word, owned.clone()))
                .collect(),
            // every arm exited: no path reaches the join, nothing flows on
            None => State::new(),
        };
        let dying = arms
            .iter()
            .map(|(state, diverged)| {
                if *diverged {
                    Vec::new()
                } else {
                    state
                        .iter()
                        .filter(|(word, _)| !keep.contains_key(word))
                        .map(|(word, owned)| (*word, owned.clone()))
                        .collect()
                }
            })
            .collect();
        self.owned = keep;
        for word in self.owned.keys().copied().collect::<Vec<_>>() {
            self.recompute_death(word);
        }
        dying
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
            .map(|(word, owned)| (*word, owned.clone()))
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
            .map(|(word, owned)| (*word, owned.clone()))
            .collect();
        for (word, owned) in dying {
            Self::emit_drop(code, word, owned);
        }
        self.owned.clear();
        self.diverged = true;
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

    fn hook(pool: u16, width: u8, refs: &[u8]) -> Hook {
        Hook {
            pool,
            width,
            refs: std::rc::Rc::from(refs),
        }
    }

    /// The deinit hook (C5): a dying value's claim stages its whole word
    /// span, calls the synthesized deinit, then decs every `Ref` word of
    /// the span — the structure every drop site routes through.
    #[test]
    fn a_deinit_death_calls_the_hook_before_the_decs() {
        let mut rc = placement();
        let mut code = Vec::new();
        // a span with Refs at both words: the second word's independent
        // count folds into the claim, both dec behind the hook
        rc.produced(3);
        rc.produced(4);
        rc.claimed(3, hook(42, 2, &[0, 1]), &[3, 4]);
        rc.drop_word(&mut code, 3);
        assert_eq!(
            code,
            vec![
                Op::Mov {
                    dst: Slot::overlay(0),
                    src: Slot::body(3),
                },
                Op::Mov {
                    dst: Slot::overlay(1),
                    src: Slot::body(4),
                },
                Op::Call {
                    callee: 42,
                    nargs: 2,
                },
                Op::RcDec { src: Slot::body(3) },
                Op::RcDec { src: Slot::body(4) },
            ],
            "the hook stages the span, calls, then decs every Ref word"
        );
        assert!(
            !rc.owned.contains_key(&4),
            "the folded word rides the claim"
        );
        // a scalar base word: the hook fires alone, nothing decs
        let mut rc = placement();
        let mut code = Vec::new();
        rc.claimed(7, hook(42, 1, &[]), &[]);
        rc.drop_word(&mut code, 7);
        assert_eq!(
            code,
            vec![
                Op::Mov {
                    dst: Slot::overlay(0),
                    src: Slot::body(7),
                },
                Op::Call {
                    callee: 42,
                    nargs: 1,
                },
            ],
            "a scalar claim never decs"
        );
    }

    /// A hand-off fires the covering claim: the hook stages the whole
    /// span while the escaping words stay live, and the span's `Ref`
    /// words the hand-off does not carry dec behind the hook.
    #[test]
    fn a_hand_off_fires_the_covering_claim_and_decs_the_rest() {
        let mut rc = placement();
        let mut code = Vec::new();
        rc.claimed(3, hook(42, 3, &[0, 2]), &[3, 5]);
        // a folded word answers its claim's base (the anchor included)
        assert_eq!(rc.claim_base_of(5), Some(3));
        assert_eq!(rc.claim_base_of(3), Some(3));
        assert_eq!(rc.claim_base_of(9), None);
        // the word at offset 1 escapes (a scalar field read of the span)
        rc.fire_claim(&mut code, 3, &[4]);
        assert_eq!(
            code,
            vec![
                Op::Mov {
                    dst: Slot::overlay(0),
                    src: Slot::body(3),
                },
                Op::Mov {
                    dst: Slot::overlay(1),
                    src: Slot::body(4),
                },
                Op::Mov {
                    dst: Slot::overlay(2),
                    src: Slot::body(5),
                },
                Op::Call {
                    callee: 42,
                    nargs: 3,
                },
                Op::RcDec { src: Slot::body(3) },
                Op::RcDec { src: Slot::body(5) },
            ],
            "the claim decs the Ref words the hand-off leaves behind"
        );
    }

    /// A copy of a word folded into a claim borrows: the destination owns
    /// a fresh inc'd count (the claim keeps its own for the hook), and a
    /// copy into the claim's own moved span rides silently.
    #[test]
    fn a_folded_word_copies_as_a_borrow() {
        let mut rc = placement();
        rc.produced(4);
        rc.claimed(3, hook(9, 2, &[1]), &[4]);
        let mut code = Vec::new();
        rc.copied(&mut code, 8, 4);
        assert_eq!(
            code,
            vec![Op::RcInc { src: Slot::body(4) }],
            "the field-read copy incs and owns its count"
        );
        assert!(rc.owned.contains_key(&8));
        // a whole-value move lands inside the moved claim's span: silent
        let mut rc = placement();
        rc.produced(4);
        rc.claimed(3, hook(9, 2, &[1]), &[4]);
        rc.copied(&mut code, 10, 3);
        let mut code = Vec::new();
        rc.copied(&mut code, 11, 4);
        assert_eq!(code, Vec::new(), "the claim's own span carries the bits");
        // a staged folded word borrows with an inc (the callee decs it)
        let mut rc = placement();
        rc.produced(4);
        rc.claimed(3, hook(9, 2, &[1]), &[4]);
        let mut code = Vec::new();
        rc.staged_local(&mut code, 4);
        assert_eq!(
            code,
            vec![Op::RcInc { src: Slot::body(4) }],
            "the staged borrow incs; the claim keeps its count"
        );
    }

    /// A pinned deinit word never dies at a use: borrows keep it alive
    /// to the scope end, where the last alias leaving kills it once.
    #[test]
    fn a_pinned_claim_survives_its_uses() {
        let mut rc = placement();
        let b = binder_span(10);
        let use_at = binder_span(60);
        rc.last_use.insert(b, (use_at, true));
        rc.claimed(7, hook(9, 1, &[]), &[7]);
        rc.bound(b, &[7]);
        rc.use_of(b, use_at);
        let mut code = Vec::new();
        rc.read(&mut code, &[7]);
        assert!(code.is_empty(), "the borrowed use is not the death");
        // a copy is the consuming move: the claim transfers, no dup
        let mut code = Vec::new();
        rc.copied(&mut code, 8, 7);
        assert_eq!(code, Vec::new(), "a pinned copy transfers silently");
        assert!(!rc.owned.contains_key(&7) && rc.owned.contains_key(&8));
        let mut code = Vec::new();
        rc.exit(&mut code, &[]);
        assert_eq!(
            code,
            vec![
                Op::Mov {
                    dst: Slot::overlay(0),
                    src: Slot::body(8),
                },
                Op::Call {
                    callee: 9,
                    nargs: 1,
                },
            ],
            "the exit fires the transferred claim exactly once"
        );
    }

    /// A copy off the death use transfers; a copy before it dups.
    #[test]
    fn copies_transfer_at_the_death_and_dup_before_it() {
        let mut rc = placement();
        let mut code = Vec::new();
        rc.produced(1);
        // no binders: the temp dies at first consumption
        rc.copied(&mut code, 2, 1);
        assert!(matches!(code.as_slice(), []), "a transfer emits nothing");
        assert!(!rc.owned.contains_key(&1));
        assert!(rc.owned.contains_key(&2));

        // a bound word with a later death dups at the copy
        let b = binder_span(50);
        let use_at = binder_span(90);
        rc.last_use.insert(b, (use_at, true));
        rc.bound(b, &[2]);
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
        rc.produced(7);
        rc.bound(s, &[7]);
        rc.bound(t, &[7]);
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
        rc.produced(4);
        rc.bound(b, &[4]);
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

    /// A loop-carried word (owned at loop entry, overwritten in-body): the
    /// overwrite's dec rolls the slot's content over while the claim
    /// survives for the back-edge join, no other in-body death site fires,
    /// and the exit decs the final content exactly once.
    #[test]
    fn a_loop_carried_word_rolls_over_without_dying_in_the_body() {
        let mut rc = placement();
        let b = binder_span(10);
        let use_at = binder_span(50);
        rc.last_use.insert(b, (use_at, true));
        rc.produced(4);
        rc.bound(b, &[4]);
        rc.loop_enter();
        // the head overwrite: one dec for the previous content, claim kept
        let mut code = Vec::new();
        rc.store_over(&mut code, &[4]);
        assert_eq!(
            code,
            vec![Op::RcDec { src: Slot::body(4) }],
            "the overwrite still decs the content it replaces"
        );
        assert!(
            rc.owned.contains_key(&4),
            "the carried claim survives the overwrite for the join"
        );
        // the in-body re-production re-stamps the slot at the body depth
        rc.produced(4);
        // the binder's textual last use inside the body is NOT a death
        rc.use_of(b, use_at);
        let mut code = Vec::new();
        rc.read(&mut code, &[4]);
        assert!(code.is_empty(), "a carried word never dies in-body");
        // past the loop the surviving claim drops exactly once
        rc.loop_exit();
        let mut code = Vec::new();
        rc.exit(&mut code, &[]);
        assert_eq!(
            code,
            vec![Op::RcDec { src: Slot::body(4) }],
            "the final content dies at the frame exit"
        );
    }

    /// The borrow-window stack: a death firing inside an open native
    /// window whose CALL still reads the word defers to that window's
    /// flush — the read emits nothing, the staging does not re-kill the
    /// word (the deferred dec and the staged borrow's own dec are the
    /// same count, deduped), and the flush decs exactly once.
    #[test]
    fn an_open_window_defers_a_staged_word_death_to_its_flush() {
        let mut rc = placement();
        let b = binder_span(10);
        let death = binder_span(60);
        rc.last_use.insert(b, (death, true));
        rc.produced(4);
        rc.bound(b, &[4]);
        rc.native_open();
        rc.native_reads(&[4]);
        // a later operand's own use fires the binder's last-use dec
        rc.use_of(b, death);
        let mut code = Vec::new();
        rc.read(&mut code, &[4]);
        assert!(code.is_empty(), "the window pins the word past the read");
        assert!(rc.owned.contains_key(&4), "the deferred word stays owned");
        // the staging of the same word is the same death, not a second one
        rc.staged_native(4, false);
        rc.native_done(&mut code);
        assert_eq!(
            code,
            vec![Op::RcDec { src: Slot::body(4) }],
            "the flush decs the deferred count exactly once, after the CALL"
        );
        assert!(!rc.owned.contains_key(&4));
    }

    /// Nested windows flush LIFO, and a word several nested calls read
    /// dies at the outermost reading window's flush: the inner window's
    /// flush leaves it alone so the outer CALL still reads a live
    /// object.
    #[test]
    fn nested_windows_flush_the_outermost_reader_last() {
        let mut rc = placement();
        let b = binder_span(10);
        let death = binder_span(60);
        rc.last_use.insert(b, (death, true));
        rc.produced(4);
        rc.bound(b, &[4]);
        rc.native_open();
        rc.native_reads(&[4]);
        rc.native_open();
        rc.use_of(b, death);
        rc.staged_native(4, false);
        let mut code = Vec::new();
        rc.read(&mut code, &[4]);
        assert!(code.is_empty(), "the death defers past the inner window");
        rc.native_done(&mut code);
        assert!(
            code.is_empty(),
            "the inner flush leaves the outer window's word alive"
        );
        assert!(rc.owned.contains_key(&4));
        rc.native_done(&mut code);
        assert_eq!(
            code,
            vec![Op::RcDec { src: Slot::body(4) }],
            "the outermost reading window's flush decs it once"
        );
    }

    /// A local callee staged inside a native window dups instead of
    /// forwarding when an enclosing window still reads the word: the
    /// callee consumes the fresh count inside its CALL, the frame's
    /// count dies at the window's flush, and a word no window reads
    /// still forwards untouched.
    #[test]
    fn a_local_staging_dups_under_an_enclosing_window() {
        let mut rc = placement();
        let b = binder_span(10);
        let death = binder_span(60);
        rc.last_use.insert(b, (death, true));
        rc.produced(4);
        rc.bound(b, &[4]);
        rc.native_open();
        rc.native_reads(&[4]);
        rc.native_open();
        rc.use_of(b, death);
        let mut code = Vec::new();
        rc.staged_local(&mut code, 4);
        assert_eq!(
            code,
            vec![Op::RcInc { src: Slot::body(4) }],
            "the handing dups; the frame keeps its count for the flush"
        );
        assert!(rc.owned.contains_key(&4), "the window's word stays owned");
        // the inner window closes empty; the enclosing window then stages
        // the same word and its flush decs the frame's count
        rc.native_done(&mut code);
        rc.staged_native(4, false);
        rc.native_done(&mut code);
        assert_eq!(
            code,
            vec![Op::RcInc { src: Slot::body(4) }, Op::RcDec {
                src: Slot::body(4)
            },],
            "the flush decs the frame's count after both CALLs"
        );
        // outside every window the same staging forwards as always
        let mut rc = placement();
        rc.last_use.insert(b, (death, true));
        rc.produced(4);
        rc.bound(b, &[4]);
        rc.use_of(b, death);
        let mut code = Vec::new();
        rc.staged_local(&mut code, 4);
        assert!(code.is_empty(), "a forward emits nothing");
        assert!(!rc.owned.contains_key(&4));
    }

    /// The tail transfer abandons its window's deferred deaths: the
    /// staged words ride the CALLT with their counts, so no dec exists.
    #[test]
    fn a_tail_transfer_rides_the_callt_with_deferred_counts() {
        let mut rc = placement();
        let b = binder_span(10);
        let death = binder_span(60);
        rc.last_use.insert(b, (death, true));
        rc.produced(4);
        rc.bound(b, &[4]);
        rc.native_open();
        rc.native_reads(&[4]);
        rc.use_of(b, death);
        let mut code = Vec::new();
        rc.read(&mut code, &[4]);
        assert!(code.is_empty(), "the death deferred into the window");
        rc.native_tail_transfer();
        assert!(!rc.owned.contains_key(&4), "the count rode the transfer");
        rc.exit(&mut code, &[]);
        assert!(code.is_empty(), "nothing decs after the transfer");
    }

    /// Fork/join: words owned on every arm survive the join; one-arm words
    /// do not (their counts died inside the arm).
    #[test]
    fn branches_keep_what_every_arm_keeps() {
        let mut rc = placement();
        rc.produced(1);
        let base = rc.save();
        // arm A also produces 2
        rc.produced(2);
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

    /// The balanced join: an arm owning a word the join does not keep
    /// reports it, so its epilogue decs the count its path owns; an arm
    /// that exited the frame constrains nothing.
    #[test]
    fn a_branch_arm_reports_what_it_owns_beyond_the_join() {
        let mut rc = placement();
        rc.produced(1);
        let base = rc.save();
        // arm A also produces 2 and keeps both words
        rc.produced(2);
        let arm_a = rc.take();
        rc.restore(base);
        // arm B keeps only the base word
        let arm_b = rc.take();
        let dying = rc.reconcile(&[(arm_a, false), (arm_b, false)]);
        assert!(rc.owned.contains_key(&1), "the shared word survives");
        assert!(
            !rc.owned.contains_key(&2),
            "the one-arm word leaves the state"
        );
        let [a, b] = &dying[..] else {
            panic!("one dying list per arm: {dying:?}");
        };
        let [(word, owned)] = a.as_slice() else {
            panic!("arm A owns exactly the one-arm word: {a:?}");
        };
        assert_eq!(*word, 2, "arm A's epilogue decs the one-arm word");
        assert!(owned.hook.is_none() && owned.count);
        assert!(b.is_empty(), "arm B owns nothing beyond the join");
        // the epilogue carries the dec for the arm's own count
        let mut code = Vec::new();
        Placement::emit_drops(&mut code, a);
        assert_eq!(
            code,
            vec![Op::RcDec { src: Slot::body(2) }],
            "the arm's epilogue is exactly the dec it owes"
        );

        // a diverging arm (an exit cleared its state) never constrains
        // the join: what the surviving arm owns flows past unchanged
        let mut rc = placement();
        rc.produced(1);
        let base = rc.save();
        let mut code = Vec::new();
        rc.exit(&mut code, &[]);
        let exit_div = rc.diverged();
        let arm_exit = rc.take();
        rc.restore(base);
        rc.produced(2);
        let arm_live = rc.take();
        let dying = rc.reconcile(&[(arm_exit, exit_div), (arm_live, false)]);
        assert!(rc.owned.contains_key(&1) && rc.owned.contains_key(&2));
        assert!(
            dying[0].is_empty() && dying[1].is_empty(),
            "the exiting arm owes nothing and the live arm keeps all"
        );
    }
}
