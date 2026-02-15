use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde::Deserialize;
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::domain::{
    Bookmark, ConflictEntry, FileChange, FileStatus, HgCapabilities, RebaseState, RepoSnapshot,
    Revision, Shelf,
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

const LOG_TEMPLATE_FIELD_SEP: char = '\u{1f}';
const LOG_PLAIN_TEMPLATE: &str = "{rev}\u{1f}{node}\u{1f}{desc|firstline}\u{1f}{author}\u{1f}{branch}\u{1f}{phase}\u{1f}{tags}\u{1f}{bookmarks}\u{1f}{date|hgdate}\n";

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
    RebaseSourceDest { source_rev: i64, dest_rev: i64 },
    RebaseContinue,
    RebaseAbort,
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
            Self::RebaseSourceDest {
                source_rev,
                dest_rev,
            } => format!("hg rebase -s {source_rev} -d {dest_rev}"),
            Self::RebaseContinue => "hg rebase --continue".to_string(),
            Self::RebaseAbort => "hg rebase --abort".to_string(),
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

    async fn probe_hg_success<S: AsRef<str>>(&self, args: &[S]) -> bool {
        self.run_hg(args)
            .await
            .map(|out| out.success)
            .unwrap_or(false)
    }

    async fn run_log_template(&self, limit: usize) -> Result<CommandResult> {
        let limit_arg = limit.to_string();
        self.run_hg(&["log", "-l", limit_arg.as_str(), "-T", LOG_PLAIN_TEMPLATE])
            .await
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

        let has_rebase = self.probe_hg_success(&["rebase", "-h"]).await;
        let has_histedit = self.probe_hg_success(&["histedit", "-h"]).await;
        let has_shelve = self.probe_hg_success(&["shelve", "-h"]).await;
        let supports_json_status = self.probe_hg_success(&["status", "-Tjson"]).await;
        let supports_json_log = self.probe_hg_success(&["log", "-l", "1", "-Tjson"]).await;
        let supports_json_bookmarks = self.probe_hg_success(&["bookmarks", "-Tjson"]).await;

        let detected = HgCapabilities {
            version,
            has_rebase,
            has_histedit,
            has_shelve,
            supports_json_status,
            supports_json_log,
            supports_json_bookmarks,
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

        let rebase_state_path = PathBuf::from(&repo_root).join(".hg").join("rebasestate");
        let (branch, status, bookmarks, conflicts, shelves, revisions, rebase_in_progress) = tokio::join!(
            self.run_hg(&["branch"]),
            async {
                if caps.supports_json_status {
                    self.run_hg(&["status", "-Tjson"])
                        .await
                        .map(|out| (out, true))
                } else {
                    self.run_hg(&["status"]).await.map(|out| (out, false))
                }
            },
            async {
                if caps.supports_json_bookmarks {
                    self.run_hg(&["bookmarks", "-Tjson"])
                        .await
                        .map(|out| (out, true))
                } else {
                    self.run_hg(&["bookmarks"]).await.map(|out| (out, false))
                }
            },
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
                    let graph_args = ["log", "-G", "-l", log_limit_arg.as_str(), "-T", "{rev}\n"];
                    if caps.supports_json_log {
                        let json_args = ["log", "-l", log_limit_arg.as_str(), "-Tjson"];
                        let (log, graph_log) =
                            tokio::join!(self.run_hg(&json_args), self.run_hg(&graph_args));
                        Some((log, true, graph_log))
                    } else {
                        let (log, graph_log) = tokio::join!(
                            self.run_log_template(options.revision_limit),
                            self.run_hg(&graph_args)
                        );
                        Some((log, false, graph_log))
                    }
                } else {
                    None
                }
            },
            async { std::fs::metadata(&rebase_state_path).is_ok() }
        );

        let branch = branch.ok().map(|out| out.stdout.trim().to_string());

        let (status, status_used_json) = status?;
        let files = if status_used_json {
            if status.success {
                match parse_status_json(&status.stdout) {
                    Ok(parsed) => parsed,
                    Err(_) => {
                        let fallback = self.run_hg(&["status"]).await?;
                        if !fallback.success {
                            return Err(command_failed(&fallback));
                        }
                        parse_status_plain(&fallback.stdout)
                    }
                }
            } else {
                let fallback = self.run_hg(&["status"]).await?;
                if !fallback.success {
                    return Err(command_failed(&fallback));
                }
                parse_status_plain(&fallback.stdout)
            }
        } else {
            if !status.success {
                return Err(command_failed(&status));
            }
            parse_status_plain(&status.stdout)
        };

        let revisions = if options.include_revisions {
            let (log, log_used_json, graph_log) = revisions
                .ok_or_else(|| anyhow!("missing log command result for revision refresh"))?;
            let log = log?;
            let mut revisions = if log_used_json {
                if log.success {
                    match parse_log_json(&log.stdout) {
                        Ok(parsed) => parsed,
                        Err(_) => {
                            let fallback = self.run_log_template(options.revision_limit).await?;
                            if !fallback.success {
                                return Err(command_failed(&fallback));
                            }
                            parse_log_plain_template(&fallback.stdout)?
                        }
                    }
                } else {
                    let fallback = self.run_log_template(options.revision_limit).await?;
                    if !fallback.success {
                        return Err(command_failed(&fallback));
                    }
                    parse_log_plain_template(&fallback.stdout)?
                }
            } else {
                if !log.success {
                    return Err(command_failed(&log));
                }
                parse_log_plain_template(&log.stdout)?
            };
            if let Ok(graph_log) = graph_log {
                if graph_log.success {
                    let graph_rows = parse_log_graph(&graph_log.stdout);
                    if !graph_rows.is_empty() {
                        revisions = merge_log_graph(revisions, &graph_rows);
                    }
                }
            }
            revisions
        } else {
            Vec::new()
        };

        let (bookmarks, bookmarks_used_json) = bookmarks?;
        let bookmarks = if bookmarks_used_json {
            if bookmarks.success {
                match parse_bookmarks_json(&bookmarks.stdout) {
                    Ok(parsed) => parsed,
                    Err(_) => {
                        let fallback = self.run_hg(&["bookmarks"]).await?;
                        if !fallback.success {
                            return Err(command_failed(&fallback));
                        }
                        parse_bookmarks_plain(&fallback.stdout)
                    }
                }
            } else {
                let fallback = self.run_hg(&["bookmarks"]).await?;
                if !fallback.success {
                    return Err(command_failed(&fallback));
                }
                parse_bookmarks_plain(&fallback.stdout)
            }
        } else {
            if !bookmarks.success {
                return Err(command_failed(&bookmarks));
            }
            parse_bookmarks_plain(&bookmarks.stdout)
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
        let rebase = build_rebase_state(rebase_in_progress, &conflicts);

        Ok(RepoSnapshot {
            repo_root: Some(repo_root),
            branch,
            files,
            revisions,
            bookmarks,
            shelves,
            conflicts,
            rebase,
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
            HgAction::RebaseSourceDest {
                source_rev,
                dest_rev,
            } => {
                let source = source_rev.to_string();
                let dest = dest_rev.to_string();
                self.run_hg(&["rebase", "-s", &source, "-d", &dest]).await
            }
            HgAction::RebaseContinue => self.run_hg(&["rebase", "--continue"]).await,
            HgAction::RebaseAbort => self.run_hg(&["rebase", "--abort"]).await,
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
            graph_prefix: None,
        })
        .collect())
}

fn parse_log_plain_template(raw: &str) -> Result<Vec<Revision>> {
    let mut revisions = Vec::new();
    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        let fields = line
            .split(LOG_TEMPLATE_FIELD_SEP)
            .map(str::to_string)
            .collect::<Vec<_>>();
        if fields.len() != 9 {
            return Err(anyhow!("failed parsing hg log template row: {line}"));
        }
        let rev = fields[0]
            .parse::<i64>()
            .with_context(|| format!("invalid revision number in log row: {line}"))?;
        let date_unix_secs = fields[8]
            .split_whitespace()
            .next()
            .and_then(|token| token.parse::<i64>().ok())
            .unwrap_or(0);
        revisions.push(Revision {
            rev,
            node: fields[1].clone(),
            desc: fields[2].clone(),
            user: fields[3].clone(),
            branch: fields[4].clone(),
            phase: fields[5].clone(),
            tags: split_whitespace_list(&fields[6]),
            bookmarks: split_whitespace_list(&fields[7]),
            date_unix_secs,
            graph_prefix: None,
        });
    }
    Ok(revisions)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedGraphRow {
    rev: i64,
    graph_prefix: String,
}

fn parse_log_graph(raw: &str) -> Vec<ParsedGraphRow> {
    raw.lines()
        .filter_map(|line| {
            let trimmed = line.trim_end();
            if trimmed.is_empty() || !trimmed.chars().last()?.is_ascii_digit() {
                return None;
            }
            let rev_start = trimmed
                .rfind(|c: char| !c.is_ascii_digit())
                .map(|idx| idx + 1)
                .unwrap_or(0);
            let rev = trimmed[rev_start..].parse::<i64>().ok()?;
            Some(ParsedGraphRow {
                rev,
                graph_prefix: trimmed[..rev_start].trim_end().to_string(),
            })
        })
        .collect()
}

fn merge_log_graph(revisions: Vec<Revision>, graph_rows: &[ParsedGraphRow]) -> Vec<Revision> {
    if graph_rows.is_empty() {
        return revisions;
    }

    let mut original_order = Vec::with_capacity(revisions.len());
    let mut revisions_by_rev = HashMap::with_capacity(revisions.len());
    for rev in revisions {
        original_order.push(rev.rev);
        revisions_by_rev.insert(rev.rev, rev);
    }

    let mut merged = Vec::with_capacity(revisions_by_rev.len());
    let mut seen_graph_revs = HashSet::new();
    for row in graph_rows {
        if !seen_graph_revs.insert(row.rev) {
            continue;
        }
        if let Some(mut revision) = revisions_by_rev.remove(&row.rev) {
            revision.graph_prefix = Some(row.graph_prefix.clone());
            merged.push(revision);
        }
    }

    for rev_num in original_order {
        if let Some(revision) = revisions_by_rev.remove(&rev_num) {
            merged.push(revision);
        }
    }
    merged
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

fn parse_bookmarks_plain(raw: &str) -> Vec<Bookmark> {
    raw.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }

            let mut parts = trimmed.split_whitespace();
            let mut active = false;
            let first = parts.next()?;
            let name = if first == "*" {
                active = true;
                parts.next()?.to_string()
            } else {
                first.to_string()
            };
            let rev_node = parts.find(|token| token.contains(':'))?;
            let mut rev_node_parts = rev_node.splitn(2, ':');
            let rev = rev_node_parts.next()?.parse::<i64>().ok()?;
            let node = rev_node_parts.next()?.to_string();

            Some(Bookmark {
                name,
                rev,
                node,
                active,
            })
        })
        .collect()
}

fn split_whitespace_list(raw: &str) -> Vec<String> {
    raw.split_whitespace()
        .map(|entry| entry.to_string())
        .collect()
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

fn build_rebase_state(in_progress: bool, conflicts: &[ConflictEntry]) -> RebaseState {
    let unresolved_conflicts = conflicts.iter().filter(|entry| !entry.resolved).count();
    let resolved_conflicts = conflicts.iter().filter(|entry| entry.resolved).count();
    RebaseState {
        in_progress,
        unresolved_conflicts,
        resolved_conflicts,
        total_conflicts: conflicts.len(),
    }
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
        assert_eq!(parsed[0].graph_prefix, None);
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
    fn build_rebase_state_counts_resolved_and_unresolved_conflicts() {
        let conflicts = vec![
            ConflictEntry {
                resolved: false,
                path: "a".to_string(),
            },
            ConflictEntry {
                resolved: true,
                path: "b".to_string(),
            },
            ConflictEntry {
                resolved: false,
                path: "c".to_string(),
            },
        ];
        let state = build_rebase_state(true, &conflicts);
        assert!(state.in_progress);
        assert_eq!(state.total_conflicts, 3);
        assert_eq!(state.unresolved_conflicts, 2);
        assert_eq!(state.resolved_conflicts, 1);
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
    fn bookmarks_plain_parser_maps_active_and_revision() {
        let raw = " * main                     7:abc123\n   dev                      5:def456\n";
        let parsed = parse_bookmarks_plain(raw);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "main");
        assert_eq!(parsed[0].rev, 7);
        assert!(parsed[0].active);
        assert_eq!(parsed[1].name, "dev");
        assert_eq!(parsed[1].rev, 5);
        assert!(!parsed[1].active);
    }

    #[test]
    fn log_plain_template_parser_maps_all_fields() {
        let raw = "9\u{1f}abcdef\u{1f}msg\u{1f}u\u{1f}default\u{1f}draft\u{1f}tip\u{1f}main\u{1f}1700000000 0\n";
        let parsed = parse_log_plain_template(raw).expect("parse plain template");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].rev, 9);
        assert_eq!(parsed[0].node, "abcdef");
        assert_eq!(parsed[0].desc, "msg");
        assert_eq!(parsed[0].tags, vec!["tip"]);
        assert_eq!(parsed[0].bookmarks, vec!["main"]);
        assert_eq!(parsed[0].date_unix_secs, 1_700_000_000);
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

    #[test]
    fn log_graph_parser_extracts_graph_prefix_and_revision() {
        let raw = "@  9\n|\no  8\n|\\\n| o  7\n";
        let parsed = parse_log_graph(raw);
        assert_eq!(
            parsed,
            vec![
                ParsedGraphRow {
                    rev: 9,
                    graph_prefix: "@".to_string(),
                },
                ParsedGraphRow {
                    rev: 8,
                    graph_prefix: "o".to_string(),
                },
                ParsedGraphRow {
                    rev: 7,
                    graph_prefix: "| o".to_string(),
                },
            ]
        );
    }

    #[test]
    fn merge_log_graph_applies_order_and_prefixes() {
        let revisions = vec![
            Revision {
                rev: 7,
                node: "n7".to_string(),
                desc: "seven".to_string(),
                user: "u".to_string(),
                branch: "default".to_string(),
                phase: "draft".to_string(),
                tags: Vec::new(),
                bookmarks: Vec::new(),
                date_unix_secs: 7,
                graph_prefix: None,
            },
            Revision {
                rev: 8,
                node: "n8".to_string(),
                desc: "eight".to_string(),
                user: "u".to_string(),
                branch: "default".to_string(),
                phase: "draft".to_string(),
                tags: Vec::new(),
                bookmarks: Vec::new(),
                date_unix_secs: 8,
                graph_prefix: None,
            },
            Revision {
                rev: 9,
                node: "n9".to_string(),
                desc: "nine".to_string(),
                user: "u".to_string(),
                branch: "default".to_string(),
                phase: "draft".to_string(),
                tags: Vec::new(),
                bookmarks: Vec::new(),
                date_unix_secs: 9,
                graph_prefix: None,
            },
        ];
        let graph = vec![
            ParsedGraphRow {
                rev: 9,
                graph_prefix: "@".to_string(),
            },
            ParsedGraphRow {
                rev: 8,
                graph_prefix: "o".to_string(),
            },
        ];
        let merged = merge_log_graph(revisions, &graph);
        assert_eq!(
            merged.iter().map(|r| r.rev).collect::<Vec<_>>(),
            vec![9, 8, 7]
        );
        assert_eq!(merged[0].graph_prefix.as_deref(), Some("@"));
        assert_eq!(merged[1].graph_prefix.as_deref(), Some("o"));
        assert_eq!(merged[2].graph_prefix, None);
    }

    #[test]
    fn rebase_preview_includes_source_and_destination() {
        let action = HgAction::RebaseSourceDest {
            source_rev: 5,
            dest_rev: 2,
        };
        assert_eq!(action.command_preview(), "hg rebase -s 5 -d 2");
        assert_eq!(
            HgAction::RebaseContinue.command_preview(),
            "hg rebase --continue"
        );
        assert_eq!(HgAction::RebaseAbort.command_preview(), "hg rebase --abort");
    }
}
