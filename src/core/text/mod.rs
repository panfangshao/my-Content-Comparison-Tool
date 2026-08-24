//! Document storage, editing, and the text transformations the toolbar offers.

pub mod buffer;
pub mod cleanup;
pub mod encoding;
pub mod history;
pub mod search;

pub use buffer::TextBuffer;
pub use cleanup::CleanupOp;
pub use encoding::{DecodedFile, FileEncoding, Newline};
pub use history::{Edit, Selection};
pub use search::{Match, SearchMode, SearchQuery};
