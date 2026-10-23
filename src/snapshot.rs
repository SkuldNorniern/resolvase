use std::fmt;
use std::ops::Range;

use crate::Error;
use crate::piece_tree::{Chunks, Tree};

/// counts every change to a buffer. goes up on edit, undo and redo, never down.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Revision(u64);

impl Revision {
    pub const fn get(self) -> u64 {
        self.0
    }

    pub(crate) const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

/// read-only text at one revision. cloning is cheap, later edits to the
/// buffer never touch it, so it can go to another thread.
///
/// lines split on `\n` only. a `\r` before it is ordinary text here; line
/// ending policy belongs to whoever loads and saves the file.
#[derive(Clone, Debug, Default)]
pub struct TextSnapshot {
    pub(crate) tree: Tree,
    pub(crate) revision: Revision,
}

impl TextSnapshot {
    pub fn revision(&self) -> Revision {
        self.revision
    }

    /// length in bytes.
    pub fn len(&self) -> usize {
        self.tree.summary().bytes
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// lines, always at least one. text ending in `\n` has an empty last line.
    pub fn line_count(&self) -> usize {
        self.tree.summary().newlines + 1
    }

    pub fn is_char_boundary(&self, offset: usize) -> bool {
        offset <= self.len() && self.tree.is_char_boundary(offset)
    }

    /// text chunks over `range`, in order, without copying.
    ///
    /// # Errors
    ///
    /// `range` is inverted, past the end or splits a char.
    pub fn chunks(&self, range: Range<usize>) -> Result<Chunks<'_>, Error> {
        self.check_range(&range)?;
        Ok(self.tree.chunks(range))
    }

    /// copies `range` out.
    ///
    /// # Errors
    ///
    /// same as [`TextSnapshot::chunks`].
    pub fn slice(&self, range: Range<usize>) -> Result<String, Error> {
        let mut out = String::with_capacity(range.len());
        out.extend(self.chunks(range)?);
        Ok(out)
    }

    /// line holding `offset`, from 0.
    ///
    /// # Errors
    ///
    /// `offset` is past the end.
    pub fn line_of(&self, offset: usize) -> Result<usize, Error> {
        self.check_offset(offset)?;
        Ok(self.tree.line_of(offset))
    }

    /// byte where `line` starts.
    ///
    /// # Errors
    ///
    /// `line` is past the last line.
    pub fn line_start(&self, line: usize) -> Result<usize, Error> {
        self.check_line(line)?;
        Ok(self.tree.line_start(line))
    }

    /// bytes of `line` without its `\n`.
    ///
    /// # Errors
    ///
    /// `line` is past the last line.
    pub fn line_range(&self, line: usize) -> Result<Range<usize>, Error> {
        let start = self.line_start(line)?;
        let end = if line + 1 < self.line_count() {
            self.tree.line_start(line + 1) - 1
        } else {
            self.len()
        };
        Ok(start..end)
    }

    pub(crate) fn check_offset(&self, offset: usize) -> Result<(), Error> {
        if offset > self.len() {
            return Err(Error::OutOfBounds {
                offset,
                len: self.len(),
            });
        }
        if !self.tree.is_char_boundary(offset) {
            return Err(Error::NotCharBoundary { offset });
        }
        Ok(())
    }

    pub(crate) fn check_range(&self, range: &Range<usize>) -> Result<(), Error> {
        if range.start > range.end {
            return Err(Error::Inverted {
                start: range.start,
                end: range.end,
            });
        }
        self.check_offset(range.start)?;
        self.check_offset(range.end)
    }

    pub(crate) fn check_line(&self, line: usize) -> Result<(), Error> {
        if line >= self.line_count() {
            return Err(Error::LineOutOfBounds {
                line,
                lines: self.line_count(),
            });
        }
        Ok(())
    }
}

impl fmt::Display for TextSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.tree
            .chunks(0..self.len())
            .try_for_each(|chunk| f.write_str(chunk))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(text: &str) -> TextSnapshot {
        TextSnapshot {
            tree: Tree::from_text(text),
            revision: Revision::default(),
        }
    }

    #[test]
    fn lines_of_small_text() {
        let s = snap("ab\n\ncd");
        assert_eq!(s.line_count(), 3);
        assert_eq!(s.line_range(0), Ok(0..2));
        assert_eq!(s.line_range(1), Ok(3..3));
        assert_eq!(s.line_range(2), Ok(4..6));
        assert_eq!(s.line_of(3), Ok(1));
        assert_eq!(s.line_of(6), Ok(2));
        assert_eq!(
            s.line_range(3),
            Err(Error::LineOutOfBounds { line: 3, lines: 3 })
        );
    }

    #[test]
    fn empty_text_has_one_line() {
        let s = snap("");
        assert_eq!(s.line_count(), 1);
        assert_eq!(s.line_range(0), Ok(0..0));
        assert_eq!(s.to_string(), "");
    }

    #[test]
    fn trailing_newline_gives_empty_last_line() {
        let s = snap("Hello world\n");
        assert_eq!(s.line_count(), 2);
        assert_eq!(s.line_range(1), Ok(12..12));
    }

    #[test]
    fn slices_refuse_split_chars() {
        let s = snap("가나다");
        assert_eq!(s.slice(3..6).as_deref(), Ok("나"));
        assert_eq!(s.slice(1..6), Err(Error::NotCharBoundary { offset: 1 }));
        let inverted = Range { start: 6, end: 3 };
        assert_eq!(s.slice(inverted), Err(Error::Inverted { start: 6, end: 3 }));
        assert_eq!(
            s.slice(0..10),
            Err(Error::OutOfBounds { offset: 10, len: 9 })
        );
    }
}
