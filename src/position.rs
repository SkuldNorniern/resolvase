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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::piece_tree::Tree;

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
