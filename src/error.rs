use std::fmt;
use std::ops::Range;

/// why an edit or lookup was refused. nothing changes when one comes back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// offset past the end of the text.
    OutOfBounds { offset: usize, len: usize },
    /// range end before its start.
    Inverted { start: usize, end: usize },
    /// offset inside a UTF-8 sequence.
    NotCharBoundary { offset: usize },
    /// two edits in one transaction touch the same text.
    Overlap {
        first: Range<usize>,
        second: Range<usize>,
    },
    /// line past the last line.
    LineOutOfBounds { line: usize, lines: usize },
    /// column past the end of its line.
    ColumnOutOfBounds { line: usize, column: usize },
    /// column inside a char, like the middle of a UTF-16 surrogate pair.
    MidChar { line: usize, column: usize },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::OutOfBounds { offset, len } => {
                write!(f, "offset {offset} is past the end ({len})")
            }
            Error::Inverted { start, end } => write!(f, "range {start}..{end} is inverted"),
            Error::NotCharBoundary { offset } => {
                write!(f, "offset {offset} is inside a UTF-8 sequence")
            }
            Error::Overlap { first, second } => {
                write!(f, "edits {first:?} and {second:?} overlap")
            }
            Error::LineOutOfBounds { line, lines } => {
                write!(f, "line {line} is past the last line ({lines} lines)")
            }
            Error::ColumnOutOfBounds { line, column } => {
                write!(f, "column {column} is past the end of line {line}")
            }
            Error::MidChar { line, column } => {
                write!(f, "column {column} on line {line} is inside a char")
            }
        }
    }
}

impl std::error::Error for Error {}
