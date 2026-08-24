//! The comparison core: line alignment, intra-line highlighting, and export.
//!
//! Everything here is pure - no UI types, no I/O - so it is fast to test and
//! easy to reason about in isolation.

pub mod engine;
pub mod inline;
pub mod options;
pub mod similarity;
pub mod unified;

pub use engine::{
    Budget, DiffResult, DiffRow, DiffStats, Hunk, HunkSummary, RowKind, Side, diff_lines,
    single_document,
};
pub use inline::{InlineDiff, Span, inline_diff};
pub use options::{AlignmentKey, DiffAlgorithm, DiffOptions, Granularity, WhitespaceMode};
pub use unified::{UnifiedOptions, to_unified};
