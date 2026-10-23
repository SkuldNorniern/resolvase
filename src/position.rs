//! line and column positions. byte offsets are the truth inside; columns
//! can be counted in UTF-8 bytes, UTF-16 units or chars, which are the three
//! encodings LSP lets a server pick.

use crate::{Error, TextSnapshot};

/// unit a column is counted in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Encoding {
    /// bytes.
    Utf8,
    /// UTF-16 code units. LSP default.
    Utf16,
    /// chars (Unicode scalar values).
    Utf32,
}

impl Encoding {
    fn width(self, c: char) -> usize {
        match self {
            Encoding::Utf8 => c.len_utf8(),
            Encoding::Utf16 => c.len_utf16(),
            Encoding::Utf32 => 1,
        }
    }
}

/// line and column, both from 0. the column unit comes from an [`Encoding`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Point {
    pub line: usize,
    pub column: usize,
}

impl Point {
    pub const fn new(line: usize, column: usize) -> Self {
        Self { line, column }
    }
}

impl TextSnapshot {
    /// line and column of `offset`.
    ///
    /// # Errors
    ///
    /// `offset` is past the end or splits a char.
    pub fn point(&self, offset: usize, encoding: Encoding) -> Result<Point, Error> {
        self.check_offset(offset)?;
        let line = self.tree.line_of(offset);
        let start = self.tree.line_start(line);
        let column = match encoding {
            Encoding::Utf8 => offset - start,
            _ => self
                .tree
                .chunks(start..offset)
                .flat_map(str::chars)
                .map(|c| encoding.width(c))
                .sum(),
        };
        Ok(Point { line, column })
    }

    /// byte offset of `point`. never clamps; a column past the line end is an
    /// error, callers that want LSP's clamp do it themselves.
    ///
    /// # Errors
    ///
    /// line or column out of range, or the column lands inside a char.
    pub fn offset(&self, point: Point, encoding: Encoding) -> Result<usize, Error> {
        let range = self.line_range(point.line)?;
        let past = Error::ColumnOutOfBounds {
            line: point.line,
            column: point.column,
        };
        if encoding == Encoding::Utf8 {
            let offset = range.start.checked_add(point.column).ok_or(past.clone())?;
            if offset > range.end {
                return Err(past);
            }
            if !self.tree.is_char_boundary(offset) {
                return Err(Error::MidChar {
                    line: point.line,
                    column: point.column,
                });
            }
            return Ok(offset);
        }
        let mut units = 0;
        let mut offset = range.start;
        for c in self.tree.chunks(range).flat_map(str::chars) {
            if units == point.column {
                return Ok(offset);
            }
            units += encoding.width(c);
            offset += c.len_utf8();
            if units > point.column {
                return Err(Error::MidChar {
                    line: point.line,
                    column: point.column,
                });
            }
        }
        if units == point.column {
            Ok(offset)
        } else {
            Err(past)
        }
    }
}

/// bytes of context read on each side when looking for a grapheme edge.
/// doubles until the edge is found inside the window.
const GRAPHEME_WINDOW: usize = 64;

impl TextSnapshot {
    /// start of the next grapheme after `offset`, or none at the end.
    /// cursor moves step by these, so `e` + combining accent or a family
    /// emoji is one step, and so is `\r\n`.
    ///
    /// # Errors
    ///
    /// `offset` is past the end or splits a char.
    pub fn next_grapheme(&self, offset: usize) -> Result<Option<usize>, Error> {
        self.check_offset(offset)?;
        self.grapheme_edge(offset, true)
    }

    /// start of the grapheme before `offset`, or none at the start.
    ///
    /// # Errors
    ///
    /// `offset` is past the end or splits a char.
    pub fn prev_grapheme(&self, offset: usize) -> Result<Option<usize>, Error> {
        self.check_offset(offset)?;
        self.grapheme_edge(offset, false)
    }

    fn grapheme_edge(&self, offset: usize, forward: bool) -> Result<Option<usize>, Error> {
        use unicode_segmentation::GraphemeCursor;

        let mut reach = GRAPHEME_WINDOW;
        loop {
            let mut start = offset.saturating_sub(reach);
            while !self.tree.is_char_boundary(start) {
                start -= 1;
            }
            let mut end = (offset + reach).min(self.len());
            while !self.tree.is_char_boundary(end) {
                end += 1;
            }
            let window = self.slice(start..end)?;
            let at_start = start == 0;
            let at_end = end == self.len();
            let mut cursor = GraphemeCursor::new(offset - start, window.len(), true);
            let found = if forward {
                cursor.next_boundary(&window, 0)
            } else {
                cursor.prev_boundary(&window, 0)
            };
            // an edge on the window rim can move with more text, unless the
            // rim is also the end of the text
            let trusted = |at: usize| (at > 0 || at_start) && (at < window.len() || at_end);
            match found {
                Ok(Some(at)) if trusted(at) => return Ok(Some(start + at)),
                Ok(None) if (forward && at_end) || (!forward && at_start) => return Ok(None),
                // the whole text gave no answer; can't happen, but never loop
                _ if at_start && at_end => return Ok(None),
                _ => reach *= 2,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::piece_tree::Tree;

    #[test]
    fn graphemes_step_whole_clusters() {
        // e + combining acute, family emoji (zwj), crlf, korean jamo-free syllable
        let text = "e\u{301}👨‍👩‍👧\r\n가x";
        let s = snap(text);
        let mut at = 0;
        let mut edges = vec![0];
        while let Some(next) = s.next_grapheme(at).unwrap() {
            edges.push(next);
            at = next;
        }
        let e = 3;
        let family = e + "👨‍👩‍👧".len();
        let crlf = family + 2;
        assert_eq!(edges, [0, e, family, crlf, crlf + 3, crlf + 4]);
        let mut back = vec![text.len()];
        let mut at = text.len();
        while let Some(prev) = s.prev_grapheme(at).unwrap() {
            back.push(prev);
            at = prev;
        }
        back.reverse();
        assert_eq!(back, edges);
    }

    #[test]
    fn long_cluster_grows_the_window() {
        let text = format!("a{}b", "\u{301}".repeat(200));
        let s = snap(&text);
        assert_eq!(s.next_grapheme(0), Ok(Some(text.len() - 1)));
        assert_eq!(s.prev_grapheme(text.len() - 1), Ok(Some(0)));
        assert_eq!(s.next_grapheme(text.len()), Ok(None));
        assert_eq!(s.prev_grapheme(0), Ok(None));
    }

    fn snap(text: &str) -> TextSnapshot {
        TextSnapshot {
            tree: Tree::from_text(text),
            revision: Default::default(),
        }
    }

    #[test]
    fn columns_in_each_encoding() {
        // "a😀가b": a=1 byte, 😀=4 bytes / 2 units, 가=3 bytes / 1 unit
        let s = snap("x\na😀가b");
        let b = 2 + 1 + 4 + 3;
        assert_eq!(s.point(b, Encoding::Utf8), Ok(Point::new(1, 8)));
        assert_eq!(s.point(b, Encoding::Utf16), Ok(Point::new(1, 4)));
        assert_eq!(s.point(b, Encoding::Utf32), Ok(Point::new(1, 3)));
        for enc in [Encoding::Utf8, Encoding::Utf16, Encoding::Utf32] {
            let p = s.point(b, enc).unwrap();
            assert_eq!(s.offset(p, enc), Ok(b));
        }
    }

    #[test]
    fn surrogate_middle_is_refused() {
        let s = snap("😀");
        assert_eq!(
            s.offset(Point::new(0, 1), Encoding::Utf16),
            Err(Error::MidChar { line: 0, column: 1 })
        );
        assert_eq!(s.offset(Point::new(0, 2), Encoding::Utf16), Ok(4));
        assert_eq!(
            s.offset(Point::new(0, 2), Encoding::Utf8),
            Err(Error::MidChar { line: 0, column: 2 })
        );
    }

    #[test]
    fn column_past_line_end_is_refused() {
        let s = snap("ab\ncd");
        assert_eq!(
            s.offset(Point::new(0, 3), Encoding::Utf16),
            Err(Error::ColumnOutOfBounds { line: 0, column: 3 })
        );
        assert_eq!(
            s.offset(Point::new(0, 3), Encoding::Utf8),
            Err(Error::ColumnOutOfBounds { line: 0, column: 3 })
        );
        assert_eq!(s.offset(Point::new(1, 2), Encoding::Utf8), Ok(5));
    }

    #[test]
    fn split_offset_is_refused() {
        let s = snap("가");
        assert_eq!(
            s.point(1, Encoding::Utf16),
            Err(Error::NotCharBoundary { offset: 1 })
        );
    }
}
