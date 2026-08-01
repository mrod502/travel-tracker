use std::path::PathBuf;

use chrono::{DateTime, Local};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileAttrs {
    pub(crate) name: String,
    pub(crate) full_path: PathBuf,
    pub(crate) created_at: DateTime<Local>,
}

impl PartialOrd for FileAttrs {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match self.created_at.partial_cmp(&other.created_at) {
            Some(core::cmp::Ordering::Equal) => {}
            ord => return ord,
        }
        match self.full_path.partial_cmp(&other.full_path) {
            Some(core::cmp::Ordering::Equal) => {}
            ord => return ord,
        }
        self.name.partial_cmp(&other.name)
    }
}

impl Ord for FileAttrs {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        return self.created_at.cmp(&other.created_at);
    }
}
