//! headless text engine. text lives in a balanced piece tree, edits are
//! checked, snapshots are cheap and never change.
//!
//! ```
//! use resolvase::TextBuffer;
//!
//! let mut buffer = TextBuffer::from_text("Hello world\n");
//! buffer.replace(6..11, "Ligase")?;
//! assert_eq!(buffer.line_count(), 2);
//! buffer.undo();
//! assert_eq!(buffer.text().to_string(), "Hello world\n");
//! # Ok::<(), resolvase::Error>(())
//! ```
//!
//! offsets are UTF-8 bytes. an edit that would split a char is refused, never
//! clamped. no fonts, files or syntax in here; that belongs to the app.

mod buffer;
mod edit;
mod error;
mod history;
mod piece_tree;
mod position;
mod snapshot;

pub use buffer::{SavePoint, TextBuffer, Transaction};
pub use edit::{Anchor, Bias, Change, ChangeSet};
pub use error::Error;
pub use piece_tree::Chunks;
pub use position::{Encoding, Point};
pub use snapshot::{Revision, TextSnapshot};
