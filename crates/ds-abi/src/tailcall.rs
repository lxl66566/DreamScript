//! The CALLT argument slide (H2): the heart of the slot-overlay ABI.

use std::ops::Range;

use crate::layout::{FrameLayout, SlotType};

/// Direction of an overlap-safe slot copy (memmove semantics).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlideDirection {
    /// Nothing moves: the ranges coincide, or are empty.
    None,
    /// Copy index 0, 1, ..., n-1. Required and safe when the destination
    /// sits below the source (a down-slide).
    Ascending,
    /// Copy index n-1, n-2, ..., 0. Required and safe when the destination
    /// sits above the source (an up-slide).
    Descending,
}

/// Why a [`SlidePlan`] cannot be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlideError {
    /// The caller's outgoing-argument area cannot stage the callee's
    /// parameters.
    StagingTooSmall { available: usize, needed: usize },
    /// Staged argument `index` has the wrong slot type.
    TypeMismatch {
        index: usize,
        staged: SlotType,
        expected: SlotType,
    },
    /// The two ranges differ in length.
    LengthMismatch { src_len: usize, dst_len: usize },
}

/// An overlap-safe plan for moving `len` slot words from `src` to `dst`
/// within one frame: ranges plus the copy direction that makes overlapping
/// moves correct (memmove semantics).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlidePlan {
    src: Range<usize>,
    dst: Range<usize>,
    direction: SlideDirection,
}

impl SlidePlan {
    /// Plans a copy between two equally long slot ranges, choosing the
    /// overlap-safe direction.
    ///
    /// [`slide_plan`] (the ABI-level constructor) yields only `None` or
    /// [`SlideDirection::Ascending`], because a CALLT destination is the
    /// frame base and the staging area sits above it; `Descending` exists
    /// so this core is a total memmove rule.
    pub fn try_new(src: Range<usize>, dst: Range<usize>) -> Result<Self, SlideError> {
        if src.len() != dst.len() {
            return Err(SlideError::LengthMismatch {
                src_len: src.len(),
                dst_len: dst.len(),
            });
        }
        let direction = plan_direction(&src, &dst);
        Ok(Self {
            src,
            dst,
            direction,
        })
    }

    /// Source slot range (the staged arguments' positions).
    #[must_use]
    pub fn src(&self) -> Range<usize> {
        self.src.start..self.src.end
    }

    /// Destination slot range (always the frame base for a CALLT plan).
    #[must_use]
    pub fn dst(&self) -> Range<usize> {
        self.dst.start..self.dst.end
    }

    /// How many words the slide moves.
    #[must_use]
    pub fn len(&self) -> usize {
        self.src.len()
    }

    /// Whether the slide moves nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.src.is_empty()
    }

    /// The overlap-safe copy direction.
    #[must_use]
    pub fn direction(&self) -> SlideDirection {
        self.direction
    }

    /// Displacement between source and destination in words.
    #[must_use]
    pub fn delta(&self) -> usize {
        self.src.start.abs_diff(self.dst.start)
    }

    /// Whether the source and destination ranges intersect.
    #[must_use]
    pub fn overlaps(&self) -> bool {
        self.src.start < self.dst.end && self.dst.start < self.src.end
    }

    /// Reference semantics of the plan: moves the words in the planned
    /// direction. `words` must cover both ranges. The tier-1 slide and the
    /// JIT stencil must behave exactly like this function. The plan and
    /// this semantics stay logical — flat frame-relative slot indices with
    /// no call-word displacement; translating them to physical stack words
    /// applies each end's owner-relative mapping (the pinned table in the
    /// `layout` module docs).
    ///
    /// # Panics
    ///
    /// Panics in debug builds if `words` does not cover both ranges.
    pub fn apply(&self, words: &mut [u64]) {
        debug_assert!(
            self.src.end <= words.len() && self.dst.end <= words.len(),
            "word array must cover both ranges"
        );
        match self.direction {
            SlideDirection::None => {},
            SlideDirection::Ascending => {
                for i in 0..self.len() {
                    words[self.dst.start + i] = words[self.src.start + i];
                }
            },
            SlideDirection::Descending => {
                for i in (0..self.len()).rev() {
                    words[self.dst.start + i] = words[self.src.start + i];
                }
            },
        }
    }
}

fn plan_direction(src: &Range<usize>, dst: &Range<usize>) -> SlideDirection {
    if src.is_empty() || src.start == dst.start {
        SlideDirection::None
    } else if dst.start < src.start {
        SlideDirection::Ascending
    } else {
        SlideDirection::Descending
    }
}

/// Computes the CALLT argument slide from the caller's frame layout to a
/// callee's (H2). Pure: a compile-time fact between two static layouts,
/// checked again by the VM at hand-assembled call sites.
///
/// The staged arguments are the first `callee.params_len()` slots of the
/// caller's outgoing-argument area; the destination is the frame base
/// (slots `0..n`), because CALLT reuses the current frame at the same fp
/// (callee-owned body words — see the pinned physical mapping in the
/// `layout` module docs).
/// Validation: the caller's area must stage all callee parameters
/// ([`SlideError::StagingTooSmall`]) with pairwise-equal slot types
/// ([`SlideError::TypeMismatch`]).
///
/// Cost: n word copies at displacement [`SlidePlan::delta`] — the O(delta)
/// argument slide plus a jump of (H2); the stack never grows at any depth,
/// in any tier. Argument words move as raw words: a tail call moving
/// arguments transfers ownership with zero RC traffic.
// caller/callee is this contract's domain vocabulary, not a naming slip.
#[allow(clippy::similar_names)]
pub fn slide_plan(caller: &FrameLayout, callee: &FrameLayout) -> Result<SlidePlan, SlideError> {
    let needed = callee.params_len();
    let available = caller.arg_area_len();
    if needed > available {
        return Err(SlideError::StagingTooSmall { available, needed });
    }
    for (index, (&staged, &expected)) in caller.arg_types()[..needed]
        .iter()
        .zip(callee.params())
        .enumerate()
    {
        // `Raw` staging carries no static type: the value words are
        // checked at the call site instead (see SlotType::Raw).
        if staged != expected && staged != SlotType::Raw {
            return Err(SlideError::TypeMismatch {
                index,
                staged,
                expected,
            });
        }
    }
    let src = caller.arg_area().start..caller.arg_area().start + needed;
    let dst = 0..needed;
    // Both ranges span `needed` slots, so the direction rule is total.
    let direction = plan_direction(&src, &dst);
    Ok(SlidePlan {
        src,
        dst,
        direction,
    })
}

// caller/callee pairs are the point of every test here.
#[cfg(test)]
#[allow(clippy::similar_names)]
mod tests {
    use super::*;
    use crate::layout::SlotType::{Bool, F64, I64, Raw, Ref};

    fn layout(slots: Vec<SlotType>, params: usize, args: usize) -> FrameLayout {
        FrameLayout::new(slots, params, args).expect("valid test layout")
    }

    #[test]
    fn mutual_even_odd_slide_matches_the_record_encoding() {
        // even/odd, the record's H2 example: one i64 param, one bool
        // temp, one staged outgoing i64 ("slide r3 into slot0").
        let even = layout(vec![I64, Bool, I64], 1, 1);
        let odd = layout(vec![I64, Bool, I64], 1, 1);
        let plan = slide_plan(&even, &odd).expect("layouts agree");
        assert_eq!(plan.src(), 2..3);
        assert_eq!(plan.dst(), 0..1);
        assert_eq!(plan.direction(), SlideDirection::Ascending);
        assert_eq!(plan.delta(), 2);
        assert!(!plan.overlaps());
        let mut words = [7, 0, 9];
        plan.apply(&mut words);
        assert_eq!(words, [9, 0, 9]);
        // mutual: odd -> even slides identically
        slide_plan(&odd, &even).expect("layouts agree");
    }

    #[test]
    fn delta_zero_when_frame_is_pure_arg_area() {
        // A trampoline whose frame is only the overlay region: the staged
        // arguments already sit at the frame base.
        let caller = layout(vec![I64, F64], 0, 2);
        let callee = layout(vec![I64, F64, Bool], 2, 1);
        let plan = slide_plan(&caller, &callee).expect("layouts agree");
        assert_eq!(plan.direction(), SlideDirection::None);
        assert_eq!(plan.delta(), 0);
        let mut words = [4, 5, 0];
        plan.apply(&mut words);
        assert_eq!(words, [4, 5, 0]);
    }

    #[test]
    fn zero_argument_tail_call_moves_nothing() {
        let caller = layout(vec![I64, I64], 1, 1);
        let callee = layout(vec![Bool, Bool], 0, 1);
        let plan = slide_plan(&caller, &callee).expect("layouts agree");
        assert!(plan.is_empty());
        assert_eq!(plan.direction(), SlideDirection::None);
    }

    #[test]
    fn growing_callee_arguments_slide_overlapping_ascending() {
        // The callee takes three params where the caller has one: dst 0..3
        // overlaps src 1..4, and ascending preserves every staged value
        // (each write lands below the word it reads next).
        let caller = layout(vec![I64, I64, F64, Ref], 1, 3);
        let callee = layout(vec![I64, F64, Ref, Bool], 3, 1);
        let plan = slide_plan(&caller, &callee).expect("layouts agree");
        assert_eq!(plan.direction(), SlideDirection::Ascending);
        assert!(plan.overlaps());
        assert_eq!(plan.delta(), 1);
        let mut words = [10, 20, 30, 40];
        plan.apply(&mut words);
        assert_eq!(words, [20, 30, 40, 40]);
    }

    #[test]
    fn shrinking_callee_arguments_slide_disjoint() {
        let caller = layout(vec![I64, F64, Ref, I64], 3, 1);
        let callee = layout(vec![I64, Bool, I64], 1, 1);
        let plan = slide_plan(&caller, &callee).expect("layouts agree");
        assert_eq!(plan.src(), 3..4);
        assert_eq!(plan.dst(), 0..1);
        assert_eq!(plan.direction(), SlideDirection::Ascending);
        assert!(!plan.overlaps());
        let mut words = [0, 0, 0, 8];
        plan.apply(&mut words);
        assert_eq!(words, [8, 0, 0, 8]);
    }

    #[test]
    fn raw_staging_matches_any_callee_shape() {
        // one frame calls a [I64, F64, Ref] callee and a [Bool] callee:
        // only Raw overlay slots admit both shapes
        let caller = layout(vec![Raw, Raw, Raw], 0, 3);
        for callee in [
            layout(vec![I64, F64, Ref, Bool], 3, 1),
            layout(vec![Bool, I64], 1, 1),
        ] {
            slide_plan(&caller, &callee).expect("Raw stages any shape");
        }
    }

    #[test]
    fn staging_area_too_small_is_rejected() {
        let caller = layout(vec![I64, I64], 0, 1);
        let callee = layout(vec![I64, I64, Bool], 2, 1);
        assert_eq!(
            slide_plan(&caller, &callee),
            Err(SlideError::StagingTooSmall {
                available: 1,
                needed: 2
            })
        );
    }

    #[test]
    fn argument_type_mismatch_names_the_slot() {
        let caller = layout(vec![I64, F64], 0, 1);
        let callee = layout(vec![I64, I64], 1, 1);
        assert_eq!(
            slide_plan(&caller, &callee),
            Err(SlideError::TypeMismatch {
                index: 0,
                staged: F64,
                expected: I64
            })
        );
        // a match at index 0 does not save a mismatch at index 1
        let caller2 = layout(vec![I64, I64, Bool], 0, 2);
        let callee2 = layout(vec![I64, I64, Bool], 2, 0);
        assert_eq!(
            slide_plan(&caller2, &callee2),
            Err(SlideError::TypeMismatch {
                index: 1,
                staged: Bool,
                expected: I64
            })
        );
    }

    #[test]
    fn descending_direction_is_overlap_safe_for_up_slides() {
        // dst 2..5 overlaps src 0..3 with the destination above the source:
        // only descending preserves the staged values (ascending would read
        // slot 2 after overwriting it, yielding [1, 2, 1, 2, 1]).
        let plan = SlidePlan::try_new(0..3, 2..5).expect("equal lengths");
        assert_eq!(plan.direction(), SlideDirection::Descending);
        assert!(plan.overlaps());
        assert_eq!(plan.delta(), 2);
        let mut words = [1, 2, 3, 0, 0];
        plan.apply(&mut words);
        assert_eq!(words, [1, 2, 1, 2, 3]);
    }

    #[test]
    fn ascending_direction_covers_down_slides() {
        let plan = SlidePlan::try_new(5..8, 1..4).expect("equal lengths");
        assert_eq!(plan.direction(), SlideDirection::Ascending);
        let mut words = [0, 0, 0, 0, 0, 6, 7, 8];
        plan.apply(&mut words);
        assert_eq!(words, [0, 6, 7, 8, 0, 6, 7, 8]);
    }

    #[test]
    fn unequal_ranges_are_rejected() {
        assert_eq!(
            SlidePlan::try_new(0..2, 0..3),
            Err(SlideError::LengthMismatch {
                src_len: 2,
                dst_len: 3
            })
        );
    }
}
