/// Functions for amending existing commits.
pub mod amend;

/// Functions for creating new commits.
pub mod create;

/// Functions for discarding commits.
pub mod discard_commit;

/// JSON transport types for commit APIs.
pub mod json;

/// Functions for inserting blank commits.
pub mod insert_blank;

/// Functions for moving changes between commits.
pub mod move_changes;

/// Functions for moving commits within the graph.
pub mod move_commit;

/// Functions for rewording commits.
pub mod reword;

/// Functions for squashing commits.
pub mod squash;

/// Phase 7d: functions for moving and uncommitting sub-ranges of a
/// committed hunk.
pub mod sub_hunk;

/// Shared result and selector types for commit APIs.
pub mod types;

/// Functions for uncommitting changes from commits.
pub mod uncommit;

/// Functions for undoing commits.
pub mod undo;
