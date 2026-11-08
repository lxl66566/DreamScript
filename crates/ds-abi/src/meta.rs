//! The frame metadata word [BL-6] and the tail-chain backtrace model [H2].

/// A code position: instruction index within a function's body. Multiply by
/// 4 for the byte offset into the fixed-width 32-bit code stream [BL-1].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CodePc(pub u32);

/// Index into a module's function table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FuncId(pub u32);

/// Model of the metadata word contents [BL-6]: return address, frame size,
/// TCO-chain state. One word group at the frame base (see the layout
/// diagram in `layout.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameMetadata {
    /// Where the caller resumes when this frame returns.
    pub return_pc: CodePc,
    /// Total physical words of this frame ([`crate::FrameLayout::frame_size`]).
    pub frame_size: u32,
    /// Tail-chain state for honest backtraces under TCO [H2].
    pub tail: TailChain,
}

impl FrameMetadata {
    /// CALLT metadata update: the frame is reused, so the return address is
    /// preserved (the tail call still returns to this frame's caller), the
    /// frame size becomes the callee's, and the tail chain grows by one
    /// elided logical frame [H2].
    pub fn enter_tail_call(&mut self, callee_frame_size: u32) {
        self.frame_size = callee_frame_size;
        self.tail.record_tail_call();
    }
}

/// Per-frame TCO-chain counter [H2]: how many logical frames this physical
/// frame has elided via tail calls since the normal call that created it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TailChain {
    /// Elided-frame count. Saturates at `u32::MAX`; a saturated counter
    /// reads as "at least `u32::MAX` elided frames" — an honest lower
    /// bound, since a long-running tail loop can outgrow any fixed counter.
    pub elided: u32,
}

impl TailChain {
    /// Records one elided frame. O(1); allocates nothing: the CALLT fast
    /// path touches only this counter.
    pub fn record_tail_call(&mut self) {
        self.elided = self.elided.saturating_add(1);
    }

    /// Lazily materializes elided logical frames from the debug side chain,
    /// oldest first: each record yields one logical frame (the calling
    /// function at its call site), ending at the live frame.
    ///
    /// Recording is per-fiber and off by default; when enabled, the VM
    /// appends one [`ElisionRecord`] per elision to the frame's side chain.
    /// Frames elided while recording was off are not recoverable — a
    /// debugger renders `elided - records.len()` additional unnamed elided
    /// frames, and the count alone (no records at all) is always available.
    pub fn materialize<'a>(
        &self,
        records: &'a [ElisionRecord],
    ) -> impl Iterator<Item = (FuncId, CodePc)> + 'a {
        debug_assert!(u64::from(self.elided) >= records.len() as u64);
        records.iter().map(|r| (r.caller, r.call_pc))
    }
}

/// One elided logical frame, recorded on the debug side chain when
/// per-fiber debug recording is enabled [H2]. Self-contained, so records
/// can be read or truncated independently of each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ElisionRecord {
    /// The function that performed the tail call — the elided frame.
    pub caller: FuncId,
    /// The CALLT instruction's code position.
    pub call_pc: CodePc,
    /// The tail-call target — the next frame in the chain.
    pub callee: FuncId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_chain_counts_and_saturates() {
        let mut t = TailChain::default();
        assert_eq!(t.elided, 0);
        t.record_tail_call();
        t.record_tail_call();
        assert_eq!(t.elided, 2);
        let mut full = TailChain { elided: u32::MAX };
        full.record_tail_call();
        assert_eq!(full.elided, u32::MAX);
    }

    #[test]
    fn enter_tail_call_preserves_return_pc() {
        let mut m = FrameMetadata {
            return_pc: CodePc(7),
            frame_size: 5,
            tail: TailChain::default(),
        };
        m.enter_tail_call(9);
        assert_eq!(m.return_pc, CodePc(7));
        assert_eq!(m.frame_size, 9);
        assert_eq!(m.tail.elided, 1);
    }

    #[test]
    fn materialize_replays_recorded_frames_oldest_first() {
        let (f0, f1, f2, f3) = (FuncId(0), FuncId(1), FuncId(2), FuncId(3));
        let records = [
            ElisionRecord {
                caller: f0,
                call_pc: CodePc(3),
                callee: f1,
            },
            ElisionRecord {
                caller: f1,
                call_pc: CodePc(4),
                callee: f2,
            },
            ElisionRecord {
                caller: f2,
                call_pc: CodePc(5),
                callee: f3,
            },
        ];
        let mut tail = TailChain::default();
        for _ in &records {
            tail.record_tail_call();
        }
        let frames: Vec<_> = tail.materialize(&records).collect();
        assert_eq!(frames, vec![
            (f0, CodePc(3)),
            (f1, CodePc(4)),
            (f2, CodePc(5))
        ]);
        // frames elided before recording was enabled stay unrecorded
        let older = TailChain { elided: 5 };
        assert_eq!(older.materialize(&records).count(), 3);
    }
}
