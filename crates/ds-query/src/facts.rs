//! Block-grain fact buckets: the index a per-definition slice reads so its
//! cost is one lookup plus that block's facts, not a scan of the whole
//! package- or file-grain pass it slices.

use ds_diag::Span;
use rustc_hash::FxHashMap;

use crate::key::BlockId;

/// One file's fact buckets: the entry range of every block.
#[derive(Clone, Debug)]
struct FileFacts {
    /// The file the buckets belong to.
    file: u32,
    /// Entry offset of every block plus one for the end: block `i` owns
    /// `starts[i]..starts[i + 1]`, numbered in source order like
    /// [`BlockIndex`](crate::QueryKey::BlockIndex) numbers blocks.
    starts: Box<[u32]>,
}

/// Fact indices bucketed by the block that contains them, in pass order.
///
/// A fact is one span-addressed entry of a package- or file-grain pass (a
/// definition, a diagnostic's primary label, a placement) and its index
/// addresses the pass's own list. Attribution is containment: the fact
/// belongs to the last block starting at or before it, and to no block when
/// that block ends before the fact does — block spans are disjoint and in
/// source order, so this answers exactly what a containment scan over the
/// whole list answers, including the fact spanning two blocks that belongs
/// to neither.
#[derive(Clone, Debug, Default)]
pub(crate) struct BlockFacts {
    /// One entry per file the pass indexed, in ascending file-id order.
    files: Box<[FileFacts]>,
    /// Every bucket's fact indices, laid out per file and per block.
    entries: Box<[u32]>,
}

impl BlockFacts {
    /// Buckets `facts` by the block containing them. `blocks` are every
    /// indexed block's `(file, span)`, grouped by file in source order;
    /// `facts` are their `(fact index, span)` in pass order.
    pub(crate) fn build(
        blocks: impl IntoIterator<Item = (u32, Span)>,
        facts: impl IntoIterator<Item = (u32, Span)>,
    ) -> Self {
        let mut by_file: FxHashMap<u32, Vec<Span>> = FxHashMap::default();
        for (file, span) in blocks {
            by_file.entry(file).or_default().push(span);
        }
        let mut layout: Vec<(u32, Vec<Span>)> = by_file.into_iter().collect();
        layout.sort_unstable_by_key(|(file, _)| *file);
        let mut buckets: Vec<Vec<Vec<u32>>> = layout
            .iter()
            .map(|(_, spans)| vec![Vec::new(); spans.len()])
            .collect();
        for (index, span) in facts {
            let slot = layout
                .binary_search_by_key(&span.file.as_u32(), |(file, _)| *file)
                .ok()
                .and_then(|at| {
                    let spans = &layout[at].1;
                    let slot = spans.partition_point(|block| block.start <= span.start);
                    let block = slot.checked_sub(1).map(|i| (i, spans[i]))?;
                    (block.1.end >= span.end).then_some((at, block.0))
                });
            if let Some((at, block)) = slot {
                buckets[at][block].push(index);
            }
        }
        let mut entries = Vec::new();
        let mut files = Vec::with_capacity(layout.len());
        for ((file, _), buckets) in layout.into_iter().zip(buckets) {
            let mut starts = Vec::with_capacity(buckets.len() + 1);
            for bucket in buckets {
                starts.push(u32::try_from(entries.len()).expect("fact count fits u32"));
                entries.extend(bucket);
            }
            starts.push(u32::try_from(entries.len()).expect("fact count fits u32"));
            files.push(FileFacts {
                file,
                starts: starts.into_boxed_slice(),
            });
        }
        Self {
            files: files.into_boxed_slice(),
            entries: entries.into_boxed_slice(),
        }
    }

    /// The indices of the facts `block` holds, in pass order.
    pub(crate) fn slice(&self, block: BlockId) -> Vec<u32> {
        let mut out = Vec::new();
        if let Ok(at) = self
            .files
            .binary_search_by_key(&block.file.as_u32(), |file| file.file)
        {
            let starts = &self.files[at].starts;
            let index = block.index as usize;
            if let Some((&low, &high)) = starts.get(index).zip(starts.get(index + 1)) {
                out.extend_from_slice(&self.entries[low as usize..high as usize]);
            }
        }
        // A diagnostic can label its block with several primary spans.
        out.sort_unstable();
        out.dedup();
        out
    }
}

#[cfg(test)]
mod tests {
    use ds_diag::FileId;

    use super::*;

    fn span(file: u32, start: u32, end: u32) -> Span {
        Span::new(FileId::new(file), start, end)
    }

    fn block(file: u32, index: u32) -> BlockId {
        BlockId {
            file: FileId::new(file),
            index,
        }
    }

    #[test]
    fn a_slice_holds_exactly_the_facts_its_block_contains() {
        let indexed = [(0, span(0, 0, 10)), (0, span(0, 10, 20))];
        let facts = [
            (0, span(0, 0, 5)),
            (1, span(0, 5, 10)),
            // Spans both blocks, so it belongs to neither, exactly like a
            // containment scan over the whole list.
            (2, span(0, 5, 15)),
            (3, span(0, 12, 14)),
            // A diagnostic labeling its block twice answers once.
            (4, span(0, 12, 13)),
            (4, span(0, 13, 14)),
            // Facts of an unindexed file or block land nowhere.
            (5, span(0, 30, 31)),
            (6, span(1, 0, 5)),
        ];
        let keyed = BlockFacts::build(indexed, facts);
        assert_eq!(keyed.slice(block(0, 0)), vec![0, 1]);
        assert_eq!(keyed.slice(block(0, 1)), vec![3, 4]);
        assert_eq!(keyed.slice(block(0, 2)), Vec::<u32>::new());
        assert_eq!(keyed.slice(block(1, 0)), Vec::<u32>::new());
    }

    #[test]
    fn every_file_keeps_its_own_buckets() {
        let indexed = [(0, span(0, 0, 4)), (1, span(1, 0, 4))];
        let facts = [(0, span(0, 1, 2)), (1, span(1, 1, 2))];
        let keyed = BlockFacts::build(indexed, facts);
        assert_eq!(keyed.slice(block(0, 0)), vec![0]);
        assert_eq!(keyed.slice(block(1, 0)), vec![1]);
    }
}
