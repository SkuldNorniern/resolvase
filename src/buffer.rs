use std::ops::Range;

use crate::edit::{Change, ChangeSet};
use crate::history::History;
use crate::piece_tree::Tree;
use crate::{Error, Revision, TextSnapshot};

/// a state of a buffer's history, from [`TextBuffer::save_point`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SavePoint(u64);

/// editable text with undo. one owner; hand out [`TextSnapshot`]s to readers.
#[derive(Clone, Debug, Default)]
pub struct TextBuffer {
    text: TextSnapshot,
    history: History,
}

impl TextBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_text(text: &str) -> Self {
        Self {
            text: TextSnapshot {
                tree: Tree::from_text(text),
                revision: Revision::default(),
            },
            history: History::default(),
        }
    }

    /// current text, borrowed.
    pub fn text(&self) -> &TextSnapshot {
        &self.text
    }

    /// current text, owned. cheap, and later edits never change it.
    pub fn snapshot(&self) -> TextSnapshot {
        self.text.clone()
    }

    pub fn revision(&self) -> Revision {
        self.text.revision
    }

    pub fn len(&self) -> usize {
        self.text.len()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn line_count(&self) -> usize {
        self.text.line_count()
    }

    /// starts a group of edits that commit as one undo step.
    pub fn transaction(&mut self) -> Transaction<'_> {
        Transaction {
            buffer: self,
            edits: Vec::new(),
            join: false,
        }
    }

    /// # Errors
    ///
    /// `range` is inverted, past the end or splits a char.
    pub fn replace(&mut self, range: Range<usize>, text: &str) -> Result<ChangeSet, Error> {
        self.transaction().replace(range, text).commit()
    }

    /// # Errors
    ///
    /// `offset` is past the end or splits a char.
    pub fn insert(&mut self, offset: usize, text: &str) -> Result<ChangeSet, Error> {
        self.replace(offset..offset, text)
    }

    /// # Errors
    ///
    /// `range` is inverted, past the end or splits a char.
    pub fn delete(&mut self, range: Range<usize>) -> Result<ChangeSet, Error> {
        self.replace(range, "")
    }

    /// reverts the last step. none when there is nothing to undo.
    pub fn undo(&mut self) -> Option<ChangeSet> {
        let changes = self.history.undo()?;
        Some(self.apply(changes))
    }

    /// reapplies the last undone step. none when there is nothing to redo.
    pub fn redo(&mut self) -> Option<ChangeSet> {
        let changes = self.history.redo()?;
        Some(self.apply(changes))
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    /// stops the next joining commit from folding into the last step.
    pub fn break_join(&mut self) {
        self.history.close();
    }

    /// marks the current state as what is on disk.
    pub fn mark_saved(&mut self) {
        let point = self.save_point();
        self.mark_saved_at(point);
    }

    /// the current state, for a save that writes a snapshot on another
    /// thread. ends a typing run, so later typing is a new undo step.
    pub fn save_point(&mut self) -> SavePoint {
        self.history.close();
        SavePoint(self.history.state())
    }

    /// marks `point` as what is on disk once its write is done. edits made
    /// meanwhile keep the buffer unsaved.
    pub fn mark_saved_at(&mut self, point: SavePoint) {
        self.history.mark_saved(point.0);
    }

    /// true when undo/redo/edits brought the text back to the saved state.
    pub fn is_saved(&self) -> bool {
        self.history.is_saved()
    }

    /// applies checked changes and bumps the revision.
    fn apply(&mut self, changes: Vec<Change>) -> ChangeSet {
        let before = self.text.revision;
        for change in &changes {
            self.text.tree.delete(change.range.clone());
            self.text.tree.insert(change.range.start, &change.inserted);
        }
        self.text.revision = before.next();
        ChangeSet {
            before,
            after: self.text.revision,
            changes,
        }
    }
}

/// edits that commit together. every range is in the text before the commit;
/// ranges must not overlap. nothing changes until [`Transaction::commit`].
#[must_use = "a transaction does nothing until committed"]
pub struct Transaction<'a> {
    buffer: &'a mut TextBuffer,
    edits: Vec<(Range<usize>, String)>,
    join: bool,
}

impl Transaction<'_> {
    pub fn replace(mut self, range: Range<usize>, text: impl Into<String>) -> Self {
        self.edits.push((range, text.into()));
        self
    }

    pub fn insert(self, offset: usize, text: impl Into<String>) -> Self {
        self.replace(offset..offset, text)
    }

    pub fn delete(self, range: Range<usize>) -> Self {
        self.replace(range, String::new())
    }

    /// folds this commit into the last undo step when that one also joined,
    /// for typing runs. saving or [`TextBuffer::break_join`] ends a run.
    pub fn join(mut self, join: bool) -> Self {
        self.join = join;
        self
    }

    /// checks every edit, then applies all of them as one step.
    /// an empty or no-op transaction changes nothing and keeps the revision.
    ///
    /// # Errors
    ///
    /// a range is inverted, past the end, splits a char, or overlaps another.
    pub fn commit(self) -> Result<ChangeSet, Error> {
        let Transaction {
            buffer,
            mut edits,
            join,
        } = self;
        let text = &buffer.text;
        for (range, _) in &edits {
            text.check_range(range)?;
        }
        edits.retain(|(range, insert)| !range.is_empty() || !insert.is_empty());
        // stable, so inserts at one spot keep the order they were given.
        // an insert sorts before a replace starting at the same spot.
        edits.sort_by_key(|(range, _)| (range.start, range.end));
        for pair in edits.windows(2) {
            let (first, second) = (&pair[0].0, &pair[1].0);
            if first.end > second.start {
                return Err(Error::Overlap {
                    first: first.clone(),
                    second: second.clone(),
                });
            }
        }
        if edits.is_empty() {
            let at = text.revision;
            return Ok(ChangeSet {
                before: at,
                after: at,
                changes: Vec::new(),
            });
        }
        // last-first, so each range still means the text before the commit
        let changes: Vec<Change> = edits
            .into_iter()
            .rev()
            .map(|(range, inserted)| Change {
                deleted: text.tree.chunks(range.clone()).collect(),
                range,
                inserted,
            })
            .collect();
        let set = buffer.apply(changes);
        buffer.history.record(set.changes.clone(), join);
        Ok(set)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Anchor, Bias};

    #[test]
    fn replace_then_undo_redo() {
        let mut buffer = TextBuffer::from_text("Hello world\n");
        let change = buffer.replace(6..11, "Ligase").unwrap();
        assert_eq!(buffer.text().to_string(), "Hello Ligase\n");
        assert_eq!(buffer.line_count(), 2);
        assert_eq!(change.after, buffer.revision());
        buffer.undo().unwrap();
        assert_eq!(buffer.text().to_string(), "Hello world\n");
        buffer.redo().unwrap();
        assert_eq!(buffer.text().to_string(), "Hello Ligase\n");
        assert_eq!(buffer.revision().get(), 3);
    }

    #[test]
    fn bad_edits_change_nothing() {
        let mut buffer = TextBuffer::from_text("가나");
        let rev = buffer.revision();
        assert_eq!(
            buffer.delete(0..1),
            Err(Error::NotCharBoundary { offset: 1 })
        );
        assert_eq!(
            buffer.insert(9, "x"),
            Err(Error::OutOfBounds { offset: 9, len: 6 })
        );
        let overlap = buffer
            .transaction()
            .replace(0..3, "a")
            .replace(0..6, "b")
            .commit();
        assert!(matches!(overlap, Err(Error::Overlap { .. })));
        assert_eq!(buffer.revision(), rev);
        assert_eq!(buffer.text().to_string(), "가나");
        assert!(!buffer.can_undo());
    }

    #[test]
    fn noop_keeps_revision_and_history() {
        let mut buffer = TextBuffer::from_text("abc");
        let set = buffer.insert(1, "").unwrap();
        assert!(set.is_empty());
        assert_eq!(set.before, set.after);
        assert!(!buffer.can_undo());
    }

    #[test]
    fn multi_cursor_uses_pre_edit_ranges() {
        let mut buffer = TextBuffer::from_text("a b c");
        let set = buffer
            .transaction()
            .insert(0, "[")
            .insert(2, "[")
            .insert(4, "[")
            .commit()
            .unwrap();
        assert_eq!(buffer.text().to_string(), "[a [b [c");
        let mut cursor = Anchor::new(4, Bias::Right);
        set.map_anchor(&mut cursor);
        assert_eq!(cursor.offset, 7);
        buffer.undo().unwrap();
        assert_eq!(buffer.text().to_string(), "a b c");
    }

    #[test]
    fn inserts_at_one_spot_keep_given_order() {
        let mut buffer = TextBuffer::from_text("|");
        buffer
            .transaction()
            .insert(1, "a")
            .insert(1, "b")
            .commit()
            .unwrap();
        assert_eq!(buffer.text().to_string(), "|ab");
    }

    #[test]
    fn insert_beside_replace_is_not_overlap() {
        let mut buffer = TextBuffer::from_text("abcd");
        buffer
            .transaction()
            .replace(1..3, "X")
            .insert(1, "<")
            .insert(3, ">")
            .commit()
            .unwrap();
        assert_eq!(buffer.text().to_string(), "a<X>d");
    }

    #[test]
    fn typing_joins_into_one_undo() {
        let mut buffer = TextBuffer::new();
        for (i, c) in ["h", "e", "y"].into_iter().enumerate() {
            buffer
                .transaction()
                .insert(i, c)
                .join(true)
                .commit()
                .unwrap();
        }
        buffer.break_join();
        buffer
            .transaction()
            .insert(3, "!")
            .join(true)
            .commit()
            .unwrap();
        buffer.undo().unwrap();
        assert_eq!(buffer.text().to_string(), "hey");
        let set = buffer.undo().unwrap();
        assert_eq!(buffer.text().to_string(), "");
        assert_eq!(set.changes().len(), 3);
    }

    #[test]
    fn saved_state_survives_undo_redo() {
        let mut buffer = TextBuffer::from_text("a");
        assert!(buffer.is_saved());
        buffer.insert(1, "b").unwrap();
        buffer.mark_saved();
        buffer.insert(2, "c").unwrap();
        assert!(!buffer.is_saved());
        buffer.undo();
        assert!(buffer.is_saved());
        buffer.redo();
        assert!(!buffer.is_saved());
        buffer.undo();
        buffer.undo();
        assert!(!buffer.is_saved());
        buffer.redo();
        assert!(buffer.is_saved());
        // new edit after undo drops the redo; saved state can't come back
        buffer.undo();
        buffer.insert(0, "x").unwrap();
        buffer.undo();
        assert!(!buffer.is_saved());
    }

    #[test]
    fn save_point_from_before_later_edits() {
        let mut buffer = TextBuffer::new();
        buffer
            .transaction()
            .insert(0, "a")
            .join(true)
            .commit()
            .unwrap();
        let point = buffer.save_point();
        let snapshot = buffer.snapshot();
        buffer
            .transaction()
            .insert(1, "b")
            .join(true)
            .commit()
            .unwrap();
        // the write of `snapshot` finishes now
        buffer.mark_saved_at(point);
        assert_eq!(snapshot.to_string(), "a");
        assert!(!buffer.is_saved());
        buffer.undo();
        assert!(buffer.is_saved(), "typing after the point was a new step");
    }

    #[test]
    fn join_after_save_starts_new_step() {
        let mut buffer = TextBuffer::new();
        buffer
            .transaction()
            .insert(0, "a")
            .join(true)
            .commit()
            .unwrap();
        buffer.mark_saved();
        buffer
            .transaction()
            .insert(1, "b")
            .join(true)
            .commit()
            .unwrap();
        assert!(!buffer.is_saved());
        buffer.undo();
        assert!(buffer.is_saved());
        assert_eq!(buffer.text().to_string(), "a");
    }

    #[test]
    fn snapshot_keeps_old_text() {
        let mut buffer = TextBuffer::from_text("one\ntwo\n");
        let old = buffer.snapshot();
        buffer.replace(0..3, "ONE").unwrap();
        assert_eq!(old.to_string(), "one\ntwo\n");
        assert_eq!(old.revision().get(), 0);
        assert_eq!(buffer.revision().get(), 1);
    }

    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n.max(1) as u64) as usize
        }
    }

    fn boundary(text: &str, mut at: usize) -> usize {
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        at
    }

    /// random transactions, undo and redo against a plain String model,
    /// with every past state kept to check undo lands exactly on it.
    #[test]
    fn random_against_string() {
        const BITS: [&str; 6] = ["x", "\n", "가", "😀", "é\r\n", "word "];
        for seed in 1..=8u64 {
            let mut rng = Rng(seed.wrapping_mul(0x2545_F491_4F6C_DD1D));
            let mut model = "seed text\n".repeat(30);
            let mut buffer = TextBuffer::from_text(&model);
            let mut past: Vec<String> = Vec::new();
            let mut future: Vec<String> = Vec::new();
            for _ in 0..1500 {
                match rng.below(10) {
                    0 if !past.is_empty() => {
                        buffer.undo().unwrap();
                        future.push(std::mem::replace(&mut model, past.pop().unwrap()));
                    }
                    1 if !future.is_empty() => {
                        buffer.redo().unwrap();
                        past.push(std::mem::replace(&mut model, future.pop().unwrap()));
                    }
                    _ => {
                        // a few non-overlapping edits, built in pre-edit coords
                        let mut cuts: Vec<usize> = (0..2 * (1 + rng.below(3)))
                            .map(|_| boundary(&model, rng.below(model.len() + 1)))
                            .collect();
                        cuts.sort_unstable();
                        let edits: Vec<(usize, usize, String)> = cuts
                            .chunks(2)
                            .map(|pair| {
                                let text = (0..rng.below(3))
                                    .map(|_| BITS[rng.below(BITS.len())])
                                    .collect();
                                (pair[0], pair[1], text)
                            })
                            .collect();
                        let mut next = model.clone();
                        for (start, end, text) in edits.iter().rev() {
                            next.replace_range(*start..*end, text);
                        }
                        let mut tx = buffer.transaction();
                        for (start, end, text) in edits {
                            tx = tx.replace(start..end, text);
                        }
                        let mut probe = Anchor::new(cuts[0], Bias::Right);
                        let set = tx.commit().unwrap();
                        // same-text rewrites are still a step, so follow the set
                        if !set.is_empty() {
                            past.push(std::mem::replace(&mut model, next));
                            future.clear();
                        }
                        set.map_anchor(&mut probe);
                        assert!(probe.offset <= model.len());
                    }
                }
                assert_eq!(buffer.len(), model.len());
            }
            assert_eq!(buffer.text().to_string(), model);
            assert_eq!(buffer.line_count(), model.matches('\n').count() + 1);
            buffer.text().tree.check();
        }
    }
}
