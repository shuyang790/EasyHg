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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graph_prefix: Option<String>,
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
    pub supports_json_bookmarks: bool,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_status_from_hg_code_maps_known_values() {
        assert_eq!(FileStatus::from_hg_code("M"), FileStatus::Modified);
        assert_eq!(FileStatus::from_hg_code("A"), FileStatus::Added);
        assert_eq!(FileStatus::from_hg_code("R"), FileStatus::Removed);
        assert_eq!(FileStatus::from_hg_code("!"), FileStatus::Missing);
        assert_eq!(FileStatus::from_hg_code("?"), FileStatus::Unknown);
        assert_eq!(FileStatus::from_hg_code("I"), FileStatus::Ignored);
        assert_eq!(FileStatus::from_hg_code("C"), FileStatus::Clean);
        assert_eq!(FileStatus::from_hg_code(" "), FileStatus::Copied);
    }

    #[test]
    fn file_status_from_hg_code_uses_first_character() {
        assert_eq!(FileStatus::from_hg_code("??"), FileStatus::Unknown);
        assert_eq!(FileStatus::from_hg_code(""), FileStatus::Unknown);
        assert_eq!(FileStatus::from_hg_code("Z"), FileStatus::Other('Z'));
    }

    #[test]
    fn repo_snapshot_serializes_expected_shape() {
        let snapshot = RepoSnapshot {
            repo_root: Some("/repo".to_string()),
            branch: Some("default".to_string()),
            files: vec![FileChange {
                path: "src/main.rs".to_string(),
                status: FileStatus::Modified,
            }],
            revisions: vec![Revision {
                rev: 1,
                node: "abc".to_string(),
                desc: "msg".to_string(),
                user: "u".to_string(),
                branch: "default".to_string(),
                phase: "draft".to_string(),
                tags: vec!["tip".to_string()],
                bookmarks: vec!["main".to_string()],
                date_unix_secs: 10,
                graph_prefix: Some("@".to_string()),
            }],
            bookmarks: vec![Bookmark {
                name: "main".to_string(),
                rev: 1,
                node: "abc".to_string(),
                active: true,
            }],
            shelves: vec![Shelf {
                name: "wip".to_string(),
                age: None,
                description: "work in progress".to_string(),
            }],
            conflicts: vec![ConflictEntry {
                resolved: false,
                path: "src/lib.rs".to_string(),
            }],
            capabilities: HgCapabilities {
                version: "hg 6.9".to_string(),
                has_rebase: true,
                has_histedit: true,
                has_shelve: true,
                supports_json_status: true,
                supports_json_log: true,
                supports_json_bookmarks: true,
            },
        };

        let json = serde_json::to_value(&snapshot).expect("serialize snapshot");
        assert_eq!(json["repo_root"], "/repo");
        assert_eq!(json["branch"], "default");
        assert_eq!(json["files"][0]["path"], "src/main.rs");
        assert_eq!(json["revisions"][0]["graph_prefix"], "@");
        assert_eq!(json["bookmarks"][0]["name"], "main");
        assert_eq!(json["capabilities"]["version"], "hg 6.9");
        assert_eq!(json["capabilities"]["supports_json_bookmarks"], true);
    }
}
