//! what an edit did, and how positions move through it.

use std::ops::Range;

use crate::Revision;

/// one replace. `range` is in the text as left by the change before it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub range: Range<usize>,
    pub deleted: String,
    pub inserted: String,
}

impl Change {
    /// bytes the inserted text covers after the change.
    pub fn new_range(&self) -> Range<usize> {
        self.range.start..self.range.start + self.inserted.len()
    }

    pub(crate) fn inverted(&self) -> Change {
        Change {
            range: self.new_range(),
            deleted: self.inserted.clone(),
            inserted: self.deleted.clone(),
        }
    }

    fn map(&self, offset: usize, bias: Bias) -> usize {
        let Range { start, end } = self.range;
        let new_end = start + self.inserted.len();
        if offset < start {
            offset
        } else if offset > end {
            offset - (end - start) + self.inserted.len()
        } else if offset == end && offset != start {
            // just past the replaced text stays just past the new text
            new_end
        } else {
            // at the start or inside: bias picks the side of the new text
            match bias {
                Bias::Left => start,
                Bias::Right => new_end,
            }
        }
    }
}

/// which side a position sticks to when text lands on it or around it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Bias {
    /// stays before text inserted at it.
    #[default]
    Left,
    /// moves after text inserted at it.
    Right,
}

/// a position that follows edits. map it through every change set in order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Anchor {
    pub offset: usize,
    pub bias: Bias,
}

impl Anchor {
    pub const fn new(offset: usize, bias: Bias) -> Self {
        Self { offset, bias }
    }
}

/// everything one commit, undo or redo did, from `before` to `after`.
///
/// changes run in order and each range is in the text the change before it
/// left, the same way LSP `contentChanges` work. edits from one transaction
/// come last-first, so their ranges also match the text before the commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeSet {
    pub before: Revision,
    pub after: Revision,
    pub(crate) changes: Vec<Change>,
}

impl ChangeSet {
    pub fn changes(&self) -> &[Change] {
        &self.changes
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// where `offset` from before the change set ends up after it.
    pub fn map(&self, offset: usize, bias: Bias) -> usize {
        self.changes
            .iter()
            .fold(offset, |offset, change| change.map(offset, bias))
    }

    pub fn map_anchor(&self, anchor: &mut Anchor) {
        anchor.offset = self.map(anchor.offset, anchor.bias);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(range: Range<usize>, deleted: &str, inserted: &str) -> Change {
        Change {
            range,
            deleted: deleted.into(),
            inserted: inserted.into(),
        }
    }

    fn set(changes: Vec<Change>) -> ChangeSet {
        ChangeSet {
            before: Revision::default(),
            after: Revision::default().next(),
            changes,
        }
    }

    #[test]
    fn insert_pushes_by_bias() {
        let s = set(vec![change(3..3, "", "xy")]);
        assert_eq!(s.map(2, Bias::Right), 2);
        assert_eq!(s.map(3, Bias::Left), 3);
        assert_eq!(s.map(3, Bias::Right), 5);
        assert_eq!(s.map(4, Bias::Left), 6);
    }

    #[test]
    fn replace_collapses_inside() {
        // "abcdef" -> "aZf"
        let s = set(vec![change(1..5, "bcde", "Z")]);
        assert_eq!(s.map(1, Bias::Left), 1);
        assert_eq!(s.map(1, Bias::Right), 2);
        assert_eq!(s.map(3, Bias::Left), 1);
        assert_eq!(s.map(3, Bias::Right), 2);
        assert_eq!(s.map(5, Bias::Left), 2);
        assert_eq!(s.map(6, Bias::Left), 3);
    }

    #[test]
    fn inverted_change_undoes() {
        let c = change(2..4, "cd", "XYZ");
        let back = c.inverted();
        assert_eq!(back.range, 2..5);
        assert_eq!(back.inserted, "cd");
    }

    #[test]
    fn anchor_follows() {
        let s = set(vec![change(10..10, "", "abc"), change(0..2, "xx", "")]);
        let mut a = Anchor::new(10, Bias::Right);
        s.map_anchor(&mut a);
        assert_eq!(a.offset, 11);
    }
}
