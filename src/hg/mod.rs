use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde::Deserialize;
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::domain::{
    Bookmark, ConflictEntry, FileChange, FileStatus, HgCapabilities, RepoSnapshot, Revision, Shelf,
};

#[derive(Debug, Clone)]
pub struct CommandResult {
    pub command_preview: String,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, Copy)]
pub struct SnapshotOptions {
    pub revision_limit: usize,
    pub include_revisions: bool,
}

#[derive(Debug, Clone)]
pub enum HgAction {
    Commit { message: String, files: Vec<String> },
    Pull,
    Push,
    Incoming,
    Outgoing,
    BookmarkCreate { name: String },
    UpdateToRevision { rev: i64 },
    UpdateToBookmark { name: String },
    ShelveCreate { name: String },
    Unshelve { name: String },
    ResolveMark { path: String },
    ResolveUnmark { path: String },
    RebaseSource { source_rev: i64 },
    HisteditBase { base_rev: i64 },
}

#[derive(Debug, Clone)]
pub struct CustomInvocation {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl CustomInvocation {
    pub fn command_preview(&self) -> String {
        let mut parts = vec![self.program.clone()];
        parts.extend(self.args.clone());
        parts.join(" ")
    }
}

impl HgAction {
    pub fn command_preview(&self) -> String {
        match self {
            Self::Commit { files, .. } => {
                if files.is_empty() {
                    "hg commit -m <message>".to_string()
                } else {
                    format!("hg commit -m <message> <{} files>", files.len())
                }
            }
            Self::Pull => "hg pull -u".to_string(),
            Self::Push => "hg push".to_string(),
            Self::Incoming => "hg incoming".to_string(),
            Self::Outgoing => "hg outgoing".to_string(),
            Self::BookmarkCreate { name } => format!("hg bookmark {name}"),
            Self::UpdateToRevision { rev } => format!("hg update -r {rev}"),
            Self::UpdateToBookmark { name } => format!("hg update {name}"),
            Self::ShelveCreate { name } => format!("hg shelve --name {name}"),
            Self::Unshelve { name } => format!("hg unshelve --name {name}"),
            Self::ResolveMark { path } => format!("hg resolve -m {path}"),
            Self::ResolveUnmark { path } => format!("hg resolve -u {path}"),
            Self::RebaseSource { source_rev } => format!("hg rebase -s {source_rev} -d ."),
            Self::HisteditBase { base_rev } => format!("hg histedit {base_rev}"),
        }
    }
}

#[async_trait]
pub trait HgClient: Send + Sync {
    async fn refresh_snapshot(&self, options: SnapshotOptions) -> Result<RepoSnapshot>;
    async fn file_diff(&self, file: &str) -> Result<String>;
    async fn revision_patch(&self, rev: i64) -> Result<String>;
    async fn run_action(&self, action: &HgAction) -> Result<CommandResult>;
    async fn run_custom_command(&self, invocation: &CustomInvocation) -> Result<CommandResult>;
}

#[derive(Debug, Clone)]
pub struct CliHgClient {
    cwd: PathBuf,
    capabilities_cache: Arc<Mutex<Option<HgCapabilities>>>,
}

impl CliHgClient {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            capabilities_cache: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn run_hg<S: AsRef<str>>(&self, args: &[S]) -> Result<CommandResult> {
        let preview = format!(
            "hg {}",
            args.iter()
                .map(|part| part.as_ref().to_string())
                .collect::<Vec<_>>()
                .join(" ")
        );

        let mut command = Command::new("hg");
        command
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for arg in args {
            command.arg(arg.as_ref());
        }

        let output = command
            .output()
            .await
            .with_context(|| format!("failed to spawn mercurial command: {preview}"))?;
        Ok(CommandResult {
            command_preview: preview,
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    pub async fn detect_capabilities(&self) -> HgCapabilities {
        if let Some(cached) = self.capabilities_cache.lock().await.clone() {
            return cached;
        }

        let version = self
            .run_hg(&["--version"])
            .await
            .ok()
            .and_then(|out| {
                out.stdout
                    .lines()
                    .find(|line| line.contains("version"))
                    .map(|line| line.trim().to_string())
            })
            .unwrap_or_else(|| "unknown".to_string());

        let has_rebase = self
            .run_hg(&["rebase", "-h"])
            .await
            .map(|out| out.success)
            .unwrap_or(false);
        let has_histedit = self
            .run_hg(&["histedit", "-h"])
            .await
            .map(|out| out.success)
            .unwrap_or(false);
        let has_shelve = self
            .run_hg(&["shelve", "-h"])
            .await
            .map(|out| out.success)
            .unwrap_or(false);

        let detected = HgCapabilities {
            version,
            has_rebase,
            has_histedit,
            has_shelve,
            supports_json_status: true,
            supports_json_log: true,
        };
        *self.capabilities_cache.lock().await = Some(detected.clone());
        detected
    }
}

#[async_trait]
impl HgClient for CliHgClient {
    async fn refresh_snapshot(&self, options: SnapshotOptions) -> Result<RepoSnapshot> {
        let caps = self.detect_capabilities().await;

        let root = self.run_hg(&["root"]).await?;
        if !root.success {
            return Err(command_failed(&root));
        }
        let repo_root = root.stdout.trim().to_string();

        let (branch, status, bookmarks, conflicts, shelves, revisions) = tokio::join!(
            self.run_hg(&["branch"]),
            self.run_hg(&["status", "-Tjson"]),
            self.run_hg(&["bookmarks", "-Tjson"]),
            self.run_hg(&["resolve", "-l"]),
            async {
                if caps.has_shelve {
                    Some(self.run_hg(&["shelve", "--list"]).await)
                } else {
                    None
                }
            },
            async {
                if options.include_revisions {
                    let log_limit_arg = options.revision_limit.to_string();
                    let args = ["log", "-l", log_limit_arg.as_str(), "-Tjson"];
                    Some(self.run_hg(&args).await)
                } else {
                    None
                }
            }
        );

        let branch = branch.ok().map(|out| out.stdout.trim().to_string());

        let status = status?;
        let files = if status.success {
            parse_status_json(&status.stdout)?
        } else {
            let fallback = self.run_hg(&["status"]).await?;
            if !fallback.success {
                return Err(command_failed(&fallback));
            }
            parse_status_plain(&fallback.stdout)
        };

        let revisions = if options.include_revisions {
            let log = revisions
                .ok_or_else(|| anyhow!("missing log command result for revision refresh"))??;
            if log.success {
                parse_log_json(&log.stdout)?
            } else {
                return Err(command_failed(&log));
            }
        } else {
            Vec::new()
        };

        let bookmarks = bookmarks?;
        let bookmarks = if bookmarks.success {
            parse_bookmarks_json(&bookmarks.stdout)?
        } else {
            return Err(command_failed(&bookmarks));
        };

        let shelves = if caps.has_shelve {
            let shelves =
                shelves.ok_or_else(|| anyhow!("missing shelve list command result"))??;
            if !shelves.success {
                return Err(command_failed(&shelves));
            }
            parse_shelve_list(&shelves.stdout)
        } else {
            Vec::new()
        };

        let conflicts = {
            let out = conflicts?;
            if !out.success {
                return Err(command_failed(&out));
            }
            parse_resolve_list(&out.stdout)
        };

        Ok(RepoSnapshot {
            repo_root: Some(repo_root),
            branch,
            files,
            revisions,
            bookmarks,
            shelves,
            conflicts,
            capabilities: caps,
        })
    }

    async fn file_diff(&self, file: &str) -> Result<String> {
        let out = self.run_hg(&["diff", file]).await?;
        if !out.success {
            return Err(command_failed(&out));
        }
        Ok(out.stdout)
    }

    async fn revision_patch(&self, rev: i64) -> Result<String> {
        let rev_s = rev.to_string();
        let out = self.run_hg(&["log", "-r", &rev_s, "-p"]).await?;
        if !out.success {
            return Err(command_failed(&out));
        }
        Ok(out.stdout)
    }

    async fn run_action(&self, action: &HgAction) -> Result<CommandResult> {
        match action {
            HgAction::Commit { message, files } => {
                let mut args = vec!["commit".to_string(), "-m".to_string(), message.to_string()];
                args.extend(files.iter().cloned());
                self.run_hg(&args).await
            }
            HgAction::Pull => self.run_hg(&["pull", "-u"]).await,
            HgAction::Push => self.run_hg(&["push"]).await,
            HgAction::Incoming => self.run_hg(&["incoming"]).await,
            HgAction::Outgoing => self.run_hg(&["outgoing"]).await,
            HgAction::BookmarkCreate { name } => self.run_hg(&["bookmark", name]).await,
            HgAction::UpdateToRevision { rev } => {
                let rev = rev.to_string();
                self.run_hg(&["update", "-r", &rev]).await
            }
            HgAction::UpdateToBookmark { name } => self.run_hg(&["update", name]).await,
            HgAction::ShelveCreate { name } => self.run_hg(&["shelve", "--name", name]).await,
            HgAction::Unshelve { name } => self.run_hg(&["unshelve", "--name", name]).await,
            HgAction::ResolveMark { path } => self.run_hg(&["resolve", "-m", path]).await,
            HgAction::ResolveUnmark { path } => self.run_hg(&["resolve", "-u", path]).await,
            HgAction::RebaseSource { source_rev } => {
                let rev = source_rev.to_string();
                self.run_hg(&["rebase", "-s", &rev, "-d", "."]).await
            }
            HgAction::HisteditBase { base_rev } => {
                let rev = base_rev.to_string();
                self.run_hg(&["histedit", &rev]).await
            }
        }
    }

    async fn run_custom_command(&self, invocation: &CustomInvocation) -> Result<CommandResult> {
        let preview = invocation.command_preview();
        let mut command = Command::new(&invocation.program);
        command
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .args(&invocation.args);
        for (key, value) in &invocation.env {
            command.env(key, value);
        }
        let output = command
            .output()
            .await
            .with_context(|| format!("failed to spawn custom command: {preview}"))?;
        Ok(CommandResult {
            command_preview: preview,
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

fn command_failed(out: &CommandResult) -> anyhow::Error {
    let stderr = compact_output(&out.stderr);
    let stdout = compact_output(&out.stdout);
    let mut details = Vec::new();
    if !stderr.is_empty() {
        details.push(format!("stderr: {stderr}"));
    }
    if !stdout.is_empty() {
        details.push(format!("stdout: {stdout}"));
    }
    if details.is_empty() {
        anyhow!("command failed: {}", out.command_preview)
    } else {
        anyhow!(
            "command failed: {} ({})",
            out.command_preview,
            details.join(" | ")
        )
    }
}

fn compact_output(text: &str) -> String {
    const LIMIT: usize = 240;
    let trimmed = text.trim();
    if trimmed.len() <= LIMIT {
        return trimmed.to_string();
    }
    let mut shortened = trimmed.chars().take(LIMIT).collect::<String>();
    shortened.push_str("…");
    shortened
}

#[derive(Debug, Deserialize)]
struct StatusJsonItem {
    path: String,
    status: String,
}

fn parse_status_json(raw: &str) -> Result<Vec<FileChange>> {
    let parsed = serde_json::from_str::<Vec<StatusJsonItem>>(raw)
        .with_context(|| "failed parsing hg status json")?;
    Ok(parsed
        .into_iter()
        .map(|item| FileChange {
            path: item.path,
            status: FileStatus::from_hg_code(&item.status),
        })
        .collect())
}

fn parse_status_plain(raw: &str) -> Vec<FileChange> {
    raw.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            let mut parts = trimmed.splitn(2, char::is_whitespace);
            let status_token = parts.next()?;
            let path = parts.next()?.trim_start();
            if path.is_empty() {
                return None;
            }
            Some(FileChange {
                path: path.to_string(),
                status: FileStatus::from_hg_code(status_token),
            })
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct LogJsonItem {
    rev: i64,
    node: String,
    desc: String,
    user: String,
    branch: String,
    phase: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    bookmarks: Vec<String>,
    date: (i64, i64),
}

fn parse_log_json(raw: &str) -> Result<Vec<Revision>> {
    let parsed = serde_json::from_str::<Vec<LogJsonItem>>(raw)
        .with_context(|| "failed parsing hg log json")?;
    Ok(parsed
        .into_iter()
        .map(|item| Revision {
            rev: item.rev,
            node: item.node,
            desc: item.desc,
            user: item.user,
            branch: item.branch,
            phase: item.phase,
            tags: item.tags,
            bookmarks: item.bookmarks,
            date_unix_secs: item.date.0,
        })
        .collect())
}

#[derive(Debug, Deserialize)]
struct BookmarkJsonItem {
    bookmark: String,
    rev: i64,
    node: String,
    #[serde(default)]
    active: bool,
}

fn parse_bookmarks_json(raw: &str) -> Result<Vec<Bookmark>> {
    let parsed = serde_json::from_str::<Vec<BookmarkJsonItem>>(raw)
        .with_context(|| "failed parsing hg bookmarks json")?;
    Ok(parsed
        .into_iter()
        .map(|item| Bookmark {
            name: item.bookmark,
            rev: item.rev,
            node: item.node,
            active: item.active,
        })
        .collect())
}

fn parse_shelve_list(raw: &str) -> Vec<Shelf> {
    raw.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            let mut parts = trimmed.split_whitespace();
            let name = parts.next()?.to_string();
            let rest = trimmed[name.len()..].trim().to_string();
            Some(Shelf {
                name,
                age: None,
                description: rest,
            })
        })
        .collect()
}

fn parse_resolve_list(raw: &str) -> Vec<ConflictEntry> {
    raw.lines()
        .filter_map(|line| {
            if line.len() < 2 {
                return None;
            }
            let mut chars = line.chars();
            let status = chars.next()?;
            let path = line[2..].trim().to_string();
            if path.is_empty() {
                return None;
            }
            Some(ConflictEntry {
                resolved: status == 'R',
                path,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_json_parser() {
        let raw = r#"[{"path":"src/main.rs","status":"M"},{"path":"README.md","status":"A"}]"#;
        let parsed = parse_status_json(raw).expect("parse status");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].status, FileStatus::Modified);
        assert_eq!(parsed[1].status, FileStatus::Added);
    }

    #[test]
    fn log_json_parser() {
        let raw = r#"[{"rev":4,"node":"abcd","desc":"msg","user":"u","branch":"default","phase":"draft","tags":["tip"],"bookmarks":["main"],"date":[10,0]}]"#;
        let parsed = parse_log_json(raw).expect("parse log");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].rev, 4);
        assert_eq!(parsed[0].bookmarks, vec!["main"]);
    }

    #[test]
    fn parse_resolve_entries() {
        let raw = "U src/main.rs\nR README.md\n";
        let parsed = parse_resolve_list(raw);
        assert_eq!(parsed.len(), 2);
        assert!(!parsed[0].resolved);
        assert!(parsed[1].resolved);
    }

    #[test]
    fn status_plain_parser_trims_and_handles_multi_char_status_tokens() {
        let raw = "M src/main.rs\nA  docs/guide.md\n?? README.md\n";
        let parsed = parse_status_plain(raw);
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].path, "src/main.rs");
        assert_eq!(parsed[0].status, FileStatus::Modified);
        assert_eq!(parsed[1].path, "docs/guide.md");
        assert_eq!(parsed[1].status, FileStatus::Added);
        assert_eq!(parsed[2].path, "README.md");
        assert_eq!(parsed[2].status, FileStatus::Unknown);
    }

    #[test]
    fn bookmarks_json_parser_maps_active_and_node() {
        let raw = r#"[{"bookmark":"main","rev":7,"node":"abc","active":true},{"bookmark":"dev","rev":5,"node":"def"}]"#;
        let parsed = parse_bookmarks_json(raw).expect("parse bookmarks");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "main");
        assert!(parsed[0].active);
        assert_eq!(parsed[1].name, "dev");
        assert!(!parsed[1].active);
    }

    #[test]
    fn shelve_list_parser_splits_name_and_description() {
        let raw = "feature-wip 2 hours ago\nhotfix\n";
        let parsed = parse_shelve_list(raw);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "feature-wip");
        assert_eq!(parsed[0].description, "2 hours ago");
        assert_eq!(parsed[1].name, "hotfix");
        assert_eq!(parsed[1].description, "");
    }

    #[test]
    fn commit_preview_includes_selected_file_count() {
        let action = HgAction::Commit {
            message: "msg".to_string(),
            files: vec!["a.txt".to_string(), "b.txt".to_string()],
        };
        assert_eq!(action.command_preview(), "hg commit -m <message> <2 files>");
    }

    #[test]
    fn custom_invocation_preview_joins_program_and_args() {
        let invocation = CustomInvocation {
            program: "hg".to_string(),
            args: vec!["log".to_string(), "-l".to_string(), "1".to_string()],
            env: vec![("X".to_string(), "1".to_string())],
        };
        assert_eq!(invocation.command_preview(), "hg log -l 1");
    }
}
