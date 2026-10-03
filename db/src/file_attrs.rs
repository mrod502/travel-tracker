use std::cmp::Ordering;
use std::path::PathBuf;

use chrono::{DateTime, Local};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileAttrs {
    pub(crate) name: String,
    pub(crate) full_path: PathBuf,
    pub(crate) created_at: DateTime<Local>,
}

/// Migration order: timestamp, then path and name to break ties.
///
/// Sorting on the timestamp alone leaves two migrations that share an mtime in
/// whatever order the directory walk returned them, and `select_pending`
/// truncates the sorted list for `--number` — so the filesystem would decide
/// which of the two runs and which gets deferred.
impl Ord for FileAttrs {
    fn cmp(&self, other: &Self) -> Ordering {
        self.created_at
            .cmp(&other.created_at)
            .then_with(|| self.full_path.cmp(&other.full_path))
            .then_with(|| self.name.cmp(&other.name))
    }
}

impl PartialOrd for FileAttrs {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
