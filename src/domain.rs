use std::fmt;

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileChange {
    pub path: String,
    pub status: FileStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum FileStatus {
    Modified,
    Added,
    Removed,
    Missing,
    Unknown,
    Ignored,
    Clean,
    Copied,
    Other(char),
}

impl FileStatus {
    pub fn from_hg_code(code: &str) -> Self {
        match code.chars().next().unwrap_or('?') {
            'M' => Self::Modified,
            'A' => Self::Added,
            'R' => Self::Removed,
            '!' => Self::Missing,
            '?' => Self::Unknown,
            'I' => Self::Ignored,
            'C' => Self::Clean,
            ' ' => Self::Copied,
            other => Self::Other(other),
        }
    }

    pub fn code(self) -> char {
        match self {
            Self::Modified => 'M',
            Self::Added => 'A',
            Self::Removed => 'R',
            Self::Missing => '!',
            Self::Unknown => '?',
            Self::Ignored => 'I',
            Self::Clean => 'C',
            Self::Copied => ' ',
            Self::Other(c) => c,
        }
    }
}

impl fmt::Display for FileStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Revision {
    pub rev: i64,
    pub node: String,
    pub desc: String,
    pub user: String,
    pub branch: String,
    pub phase: String,
    pub tags: Vec<String>,
    pub bookmarks: Vec<String>,
    pub date_unix_secs: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Bookmark {
    pub name: String,
    pub rev: i64,
    pub node: String,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConflictEntry {
    pub resolved: bool,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Shelf {
    pub name: String,
    pub age: Option<String>,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct HgCapabilities {
    pub version: String,
    pub has_rebase: bool,
    pub has_histedit: bool,
    pub has_shelve: bool,
    pub supports_json_status: bool,
    pub supports_json_log: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct RepoSnapshot {
    pub repo_root: Option<String>,
    pub branch: Option<String>,
    pub files: Vec<FileChange>,
    pub revisions: Vec<Revision>,
    pub bookmarks: Vec<Bookmark>,
    pub shelves: Vec<Shelf>,
    pub conflicts: Vec<ConflictEntry>,
    pub capabilities: HgCapabilities,
}
