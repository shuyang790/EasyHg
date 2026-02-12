use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde::Deserialize;
use tokio::process::Command;

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

#[derive(Debug, Clone)]
pub enum HgAction {
    Commit { message: String },
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

impl HgAction {
    pub fn command_preview(&self) -> String {
        match self {
            Self::Commit { .. } => "hg commit -m <message>".to_string(),
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
    async fn refresh_snapshot(&self, log_limit: usize) -> Result<RepoSnapshot>;
    async fn file_diff(&self, file: &str) -> Result<String>;
    async fn revision_patch(&self, rev: i64) -> Result<String>;
    async fn run_action(&self, action: &HgAction) -> Result<CommandResult>;
}

#[derive(Debug, Clone)]
pub struct CliHgClient {
    cwd: PathBuf,
}

impl CliHgClient {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }

    async fn run_hg<S: AsRef<str>>(&self, args: &[S]) -> Result<CommandResult> {
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

    async fn detect_capabilities(&self) -> HgCapabilities {
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

        HgCapabilities {
            version,
            has_rebase,
            has_histedit,
            has_shelve,
            supports_json_status: true,
            supports_json_log: true,
        }
    }
}

#[async_trait]
impl HgClient for CliHgClient {
    async fn refresh_snapshot(&self, log_limit: usize) -> Result<RepoSnapshot> {
        let caps = self.detect_capabilities().await;

        let root = self.run_hg(&["root"]).await?;
        if !root.success {
            return Err(anyhow!("{}\n{}", root.stdout.trim(), root.stderr.trim()));
        }
        let repo_root = root.stdout.trim().to_string();

        let branch = self
            .run_hg(&["branch"])
            .await
            .ok()
            .map(|out| out.stdout.trim().to_string());

        let status = self.run_hg(&["status", "-Tjson"]).await?;
        let files = if status.success {
            parse_status_json(&status.stdout)?
        } else {
            let fallback = self.run_hg(&["status"]).await?;
            parse_status_plain(&fallback.stdout)
        };

        let log_limit_arg = log_limit.to_string();
        let log = self
            .run_hg(&["log", "-l", &log_limit_arg, "-Tjson"])
            .await?;
        let revisions = if log.success {
            parse_log_json(&log.stdout)?
        } else {
            Vec::new()
        };

        let bookmarks = self.run_hg(&["bookmarks", "-Tjson"]).await?;
        let bookmarks = if bookmarks.success {
            parse_bookmarks_json(&bookmarks.stdout)?
        } else {
            Vec::new()
        };

        let shelves = if caps.has_shelve {
            let shelves = self.run_hg(&["shelve", "--list"]).await?;
            parse_shelve_list(&shelves.stdout)
        } else {
            Vec::new()
        };

        let conflicts = {
            let out = self.run_hg(&["resolve", "-l"]).await?;
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
        if !out.success && !out.stderr.trim().is_empty() {
            return Err(anyhow!("{}", out.stderr.trim()));
        }
        Ok(out.stdout)
    }

    async fn revision_patch(&self, rev: i64) -> Result<String> {
        let rev_s = rev.to_string();
        let out = self.run_hg(&["log", "-r", &rev_s, "-p"]).await?;
        if !out.success {
            return Err(anyhow!("{}", out.stderr.trim()));
        }
        Ok(out.stdout)
    }

    async fn run_action(&self, action: &HgAction) -> Result<CommandResult> {
        match action {
            HgAction::Commit { message } => self.run_hg(&["commit", "-m", message]).await,
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
            if line.len() < 2 {
                return None;
            }
            let mut chars = line.chars();
            let status = chars.next()?;
            let path = line[2..].to_string();
            Some(FileChange {
                path,
                status: FileStatus::from_hg_code(&status.to_string()),
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
}
